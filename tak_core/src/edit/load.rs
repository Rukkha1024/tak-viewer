//! On-demand raw record loader. Never called by the initial markers-first open.
use super::{DeclaredLabel, DocumentInput, EditError, LabelKey, PreparedInput, RawSample, RawTrack, RecordBytes, StreamRef};
use crate::tak::{self, channels, nodes, Take};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

fn error(e: impl std::fmt::Display) -> EditError { EditError::Other(e.to_string()) }

impl PreparedInput {
    /// Read and validate source records on a worker, returning an owned Take for rebuilding
    /// the display. TAKM seals are checked. Strictly unsupported or ambiguous data is refused;
    /// lenient-only node records can be displayed but are marked ineligible for modification.
    /// A full TAK is read on demand here, never during the initial markers-first path.
    pub fn read_from_path(path: &Path) -> Result<(PreparedInput, Take), EditError> {
        let before = std::fs::metadata(path).map_err(error)?;
        let before_stamp = (before.len(), before.modified().ok());
        let (streams, nodes_bytes, channel_bytes, meta_bytes) = if crate::takm::is_takm_path(path) {
            crate::takm::verify_fixed(path).map_err(error)?;
            let mut file = File::open(path).map_err(error)?;
            let index = crate::takm::read_index(&file, before.len()).map_err(error)?;
            let get = |file: &mut File, name: &str| -> Result<Vec<u8>, EditError> {
                let s = index.unique(name).ok_or(EditError::UnsupportedLayout)?;
                let len = usize::try_from(s.size).map_err(|_| EditError::UnsupportedLayout)?;
                let mut data = vec![0; len];
                file.seek(SeekFrom::Start(s.offset)).map_err(error)?;
                file.read_exact(&mut data).map_err(error)?;
                Ok(data)
            };
            let ns = get(&mut file, "Nodes.dat")?;
            let cs = get(&mut file, "Channels.dat")?;
            let ms = get(&mut file, "MetaData.dat")?;
            (index.streams.iter().map(|s| StreamRef { index: s.index, name: s.name.clone() }).collect::<Vec<_>>(), ns, cs, ms)
        } else {
            let raw = std::fs::read(path).map_err(error)?;
            let container = tak::container::Container::parse(&raw).map_err(error)?;
            let entries = container.streams().map_err(error)?;
            let get = |name: &str| -> Result<Vec<u8>, EditError> {
                let mut hits = entries.iter().filter(|(n, _)| n == name);
                let entry = hits.next().ok_or(EditError::UnsupportedLayout)?;
                if hits.next().is_some() { return Err(EditError::UnsupportedLayout); }
                container.read_stream(entry.1).map_err(error)
            };
            let ns = get("Nodes.dat")?;
            let cs = get("Channels.dat")?;
            let ms = get("MetaData.dat")?;
            let refs = entries.iter().enumerate().map(|(index, (name, _))| StreamRef { index, name: name.clone() }).collect();
            (refs, ns, cs, ms)
        };
        let after = std::fs::metadata(path).map_err(error)?;
        if (after.len(), after.modified().ok()) != before_stamp { return Err(EditError::Other("원본 파일이 읽는 중 바뀌었어요".into())); }
        let mut take = tak::decode_markers(path, before.len() as usize, &nodes_bytes, &channel_bytes, &meta_bytes, false).map_err(error)?;
        let (analog, warning) = tak::decode_analog(&channel_bytes);
        take.analog = analog;
        take.warnings.extend(warning);
        let assets = nodes::skeleton_asset_set(&nodes_bytes).map_err(error)?;
        let marker_nodes = nodes::parse_marker_nodes(&nodes_bytes, &assets).map_err(error)?;
        let properties = nodes::property_records(&nodes_bytes, &nodes::MARKER_SIGNATURES).map_err(error)?;
        let inventory = channels::iter_channel_inventory(&channel_bytes).map_err(error)?;
        let rows = channels::read_marker_channels(&channel_bytes, &marker_nodes, take.start_frame, take.end_frame, &assets).map_err(error)?;
        let mut node_by_uid = HashMap::new();
        for (i, (node, rec)) in marker_nodes.iter().zip(&properties).enumerate() {
            let end = if node.parent.is_some() { rec.end + 68 } else { properties.get(i + 1).map_or(nodes_bytes.len(), |r| r.start) };
            if end > nodes_bytes.len() || node_by_uid.insert(node.uid, (node, rec.start, end)).is_some() { return Err(EditError::DuplicateUid); }
        }
        let mut used = HashSet::new();
        let mut tracks = Vec::with_capacity(rows.len());
        let mut first_channel = channel_bytes.len();
        for row in rows {
            if !used.insert(row.node.uid) { return Err(EditError::DuplicateUid); }
            let (node, start, end) = node_by_uid.get(&row.node.uid).copied().ok_or(EditError::UnsupportedLayout)?;
            let mut matches = inventory.iter().filter(|c| c.uid == node.uid && c.kind == "Vector3fChannel" && c.name == "Translation");
            let channel = matches.next().ok_or(EditError::UnsupportedLayout)?;
            if matches.next().is_some() || channel.data_start != row.data_start || channel.data_end != row.data_end { return Err(EditError::UnsupportedLayout); }
            first_channel = first_channel.min(channel.start);
            let samples = channel_bytes[row.data_start..row.data_end].chunks_exact(16).map(|b| {
                let mut raw = [0; 16]; raw.copy_from_slice(b); RawSample(raw)
            }).collect::<Vec<_>>();
            let label = match (node.parent, node.slot) {
                (Some(parent), Some(slot)) if parent != [0; 16] => Some(LabelKey { parent, slot }),
                _ => None,
            };
            tracks.push(RawTrack { uid: node.uid, name: node.name.clone(), label, records: RecordBytes {
                node: Arc::from(&nodes_bytes[start..end]),
                translation_header: Arc::from(&channel_bytes[channel.start..channel.data_start]),
                serialization_verified: node.parent.is_some(),
            }, samples: samples.into() });
        }
        let mut labels = Vec::new();
        for asset in &take.assets {
            if let Some(model) = &asset.model {
                labels.extend(model.constraints.iter().filter(|c| c.primary).map(|c| DeclaredLabel { key: LabelKey { parent: asset.uid, slot: c.slot }, name: c.name.clone() }));
            }
        }
        for group in &take.label_groups {
            labels.extend(group.labels.iter().map(|(name, slot)| DeclaredLabel { key: LabelKey { parent: group.uid, slot: *slot as i32 }, name: name.clone() }));
        }
        let first_frame = u32::try_from(take.start_frame).map_err(|_| EditError::InvalidRange)?;
        let last_frame = u32::try_from(take.end_frame).map_err(|_| EditError::InvalidRange)?;
        let first_node = properties.first().map_or(nodes_bytes.len(), |r| r.start);
        let input = DocumentInput { streams: streams.into(), nodes_prefix: Arc::from(&nodes_bytes[..first_node]), channels_prefix: Arc::from(&channel_bytes[..first_channel]), tracks, labels, first_frame, last_frame };
        Ok((PreparedInput::new(input)?.for_source(path.to_path_buf()), take))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_loader_keeps_raw_samples_and_installs_without_sharing_view_data() {
        let path = std::env::temp_dir().join(format!("tak_phase01_loader_{}.tak", std::process::id()));
        std::fs::write(&path, crate::sample_tak::sample_tak()).unwrap();
        let (wrong_input, _) = PreparedInput::read_from_path(&path).unwrap();
        let mut wrong_document = super::super::EditDocument::unloaded(path.with_extension("other.tak"));
        assert_eq!(wrong_document.install_prepared(wrong_document.revision(), wrong_input), Err(EditError::WrongDocument));
        let (input, take) = PreparedInput::read_from_path(&path).unwrap();
        assert!(input.matches_shown_take(&take));
        let mut changed_take = take.clone();
        changed_take.trajectories[0].xyz[0][0] += 1.0;
        assert!(!input.matches_shown_take(&changed_take));
        let mut document = super::super::EditDocument::unloaded(path.clone());
        document.install_prepared(document.revision(), input).unwrap();
        let snapshot = document.snapshot();
        assert_eq!(snapshot.tracks().count(), take.trajectories.len());
        for t in &take.trajectories {
            let raw = snapshot.track(&t.uid).unwrap();
            assert_eq!(raw.samples.iter().map(|s| s.frame()).collect::<Vec<_>>(), t.frames);
            for (sample, xyz) in raw.samples.iter().zip(&t.xyz) {
                assert_eq!(sample.xyz().map(f32::to_bits), xyz.map(f32::to_bits));
            }
        }
        assert!(!document.is_dirty());
        std::fs::remove_file(path).unwrap();
    }
}
