//! Relay TAKM v2 archive members to the same decoder used by local files.
use super::{
    progressive, source_identity, transport::Frame, CancelToken, ReadMoment, RemoteSession,
    RemoteSource, RemoteStage, RemoteTake, Result,
};
use crate::tak::{self, channels::CHANNELS_HEADER, TakError};
use crate::takm::v2::{restore_opaque, AnalogKind, Decoder, Limits};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::Arc,
};

const MAX_LOGICAL: usize = 1 << 30;
const MANIFEST: &str = "manifest.bin.zst";
const PRESENCE: &str = "presence.bin.zst";
const FIRST: &str = "first.pco.parts";
const PREVIEW: &str = "preview.pco.parts";
const EXACT: &str = "exact.pco.parts";
const PLATES: &str = "plates.xor.zst";
const ANALOG: &str = "analog.pco.parts";
const RECOVERY: &str = "recovery.bin.zst";

#[derive(Debug)]
pub(super) struct Base {
    catalog: Vec<u8>,
    manifest: Vec<u8>,
    presence: Vec<u8>,
    packed_limit: usize,
}

impl Base {
    fn member(&self, name: &str) -> tak::Result<Vec<u8>> {
        match name {
            MANIFEST => Ok(self.manifest.clone()),
            PRESENCE => Ok(self.presence.clone()),
            _ => Err(TakError(format!("Missing SSH TAKM v2 part: {name}"))),
        }
    }

    fn decoder(&self) -> Result<Decoder> {
        Decoder::read(
            &self.catalog,
            Limits {
                packed: self.packed_limit,
                expanded: MAX_LOGICAL,
            },
            |name| self.member(name),
        )
        .map_err(|e| e.to_string())
    }
}

struct Stamp {
    identity: Value,
    size: usize,
    fixed_len: u64,
    packed_limit: usize,
}

impl Stamp {
    fn read(header: &Value, config_limit: usize) -> Result<Self> {
        let identity = source_identity(header)?;
        let size = header["file_size"]
            .as_u64()
            .filter(|n| identity[2].as_u64() == Some(*n))
            .and_then(|n| usize::try_from(n).ok())
            .ok_or("Invalid TAKM v2 SSH file size")?;
        let fixed_len = header["fixed_len"]
            .as_u64()
            .filter(|&n| n >= 512 && n % 512 == 0 && n <= (size as u64).saturating_sub(1024))
            .ok_or("Invalid TAKM v2 SSH fixed length")?;
        Ok(Self {
            identity,
            size,
            fixed_len,
            packed_limit: config_limit.min(size),
        })
    }

    fn check(&self, header: &Value, blocks: &[Vec<u8>], stage: &str, count: usize) -> Result<()> {
        if source_identity(header)? != self.identity
            || header["file_size"].as_u64() != Some(self.size as u64)
            || header["fixed_len"].as_u64() != Some(self.fixed_len)
        {
            return Err("source_changed: TAKM v2 stage belongs to another open".into());
        }
        if header["archive_stage"] != stage
            || blocks.len() != count
            || blocks
                .iter()
                .try_fold(0usize, |n, b| n.checked_add(b.len()))
                .is_none_or(|n| n > self.packed_limit)
        {
            return Err("Invalid TAKM v2 SSH stage payload".into());
        }
        Ok(())
    }
}

fn checked(cancel: &CancelToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err("SSH request cancelled".into())
    } else {
        Ok(())
    }
}

struct Layout {
    device_headers: Vec<u8>,
    order: Vec<bool>,
    devices: Vec<u8>,
    plates: Vec<usize>,
    runs: HashMap<tak::Uid, VecDeque<Vec<(i64, i64)>>>,
}

impl Layout {
    fn read(decoder: &Decoder) -> Result<Self> {
        let typed = tak::nodes::analog_device_nodes(decoder.nodes()).map_err(|e| e.to_string())?;
        let mut device_headers = CHANNELS_HEADER.to_vec();
        device_headers.extend_from_slice(&[0; 4]);
        let mut spatial_headers = device_headers.clone();
        let mut order = Vec::new();
        let mut roles = Vec::new();
        let mut spatial_count = 0u32;
        for (head, kind) in decoder.record_heads() {
            let dst = match kind {
                None => {
                    spatial_count += 1;
                    &mut spatial_headers
                }
                Some(AnalogKind::Plates) => {
                    roles.push(2);
                    &mut device_headers
                }
                Some(AnalogKind::Other) => {
                    roles.push(1);
                    &mut device_headers
                }
                Some(AnalogKind::All) => return Err("Invalid TAKM v2 device role".into()),
            };
            dst.extend_from_slice(&head[..head.len() - 4]);
            dst.extend_from_slice(&[0; 4]);
            order.push(kind.is_some());
        }
        device_headers[6..10].copy_from_slice(&(roles.len() as u32).to_le_bytes());
        spatial_headers[6..10].copy_from_slice(&spatial_count.to_le_bytes());
        let mut runs = HashMap::<_, VecDeque<_>>::new();
        for (row, (_, presence)) in progressive::inventory(&spatial_headers)?
            .into_iter()
            .zip(decoder.spatial_presence())
        {
            if row.is_translation() {
                let ranges = presence
                    .iter()
                    .map(|&(start, count)| {
                        let start = i64::from(start);
                        (start, start + i64::from(count) - 1)
                    })
                    .collect();
                runs.entry(row.uid).or_default().push_back(ranges);
            }
        }

        let mut devices = Vec::new();
        let mut plates = vec![usize::MAX; typed.iter().filter(|d| d.forceplate).count()];
        let mut current = None;
        let mut used = std::collections::HashSet::new();
        for (r, role) in progressive::inventory(&device_headers)?
            .into_iter()
            .zip(roles)
        {
            let channel = r
                .name
                .strip_prefix("AnalogChannel")
                .and_then(|i| i.parse::<usize>().ok())
                .ok_or("Invalid TAKM v2 device channel")?;
            if channel == 0 {
                let device = typed
                    .iter()
                    .find(|d| d.uid == r.uid)
                    .ok_or("Unknown TAKM v2 device UID")?;
                if !used.insert(r.uid) || device.forceplate != (role == 2) {
                    return Err("Invalid TAKM v2 device role".into());
                }
                if let Some(i) = device.plate {
                    plates[i] = devices.len();
                }
                devices.push(role);
                current = Some((r.uid, role, 0));
            }
            if current != Some((r.uid, role, channel)) {
                return Err("Inconsistent TAKM v2 device group".into());
            }
            current = Some((r.uid, role, channel + 1));
        }
        if plates.contains(&usize::MAX) {
            return Err("TAKM v2 forceplate has no device group".into());
        }
        Ok(Self {
            device_headers,
            order,
            devices,
            plates,
            runs,
        })
    }

    fn motion(&self, take: &tak::Take, requested: i64) -> super::PreviewMotion {
        let mut remaining: HashMap<_, VecDeque<_>> = self
            .runs
            .iter()
            .map(|(uid, runs)| (*uid, runs.iter().collect()))
            .collect();
        let runs = take
            .trajectories
            .iter()
            .map(|t| {
                // A native marker without a channel is an empty trajectory.
                remaining
                    .get_mut(&t.uid)
                    .and_then(VecDeque::pop_front)
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        super::PreviewMotion { requested, runs }
    }
}

fn next_comment_id(tail: &[u8]) -> u64 {
    crate::comments::parse(&String::from_utf8_lossy(tail))
        .comments()
        .next_id()
}

impl RemoteSession {
    pub(super) fn finish_archive_open(
        &mut self,
        path: &str,
        header: Value,
        blocks: Vec<Vec<u8>>,
        cancel: &CancelToken,
        mut publish: Option<&mut dyn FnMut(RemoteStage)>,
    ) -> Result<RemoteTake> {
        let stamp = Stamp::read(&header, self.config.maximum_payload_bytes)?;
        let stage = header["archive_stage"]
            .as_str()
            .ok_or("Missing TAKM v2 SSH stage")?;
        if stage == "opaque" {
            stamp.check(&header, &blocks, "opaque", 3)?;
            if publish.is_some() {
                self.record_packet(ReadMoment::FirstPacket, &header, &blocks);
            }
            return self.open_opaque_archive(path, stamp, blocks, cancel);
        }
        let staged = stage == "first" && publish.is_some();
        stamp.check(
            &header,
            &blocks,
            if staged { "first" } else { "markers" },
            if staged { 5 } else { 7 },
        )?;
        if publish.is_some() {
            self.record_packet(
                if staged {
                    ReadMoment::FirstPose
                } else {
                    ReadMoment::FirstPacket
                },
                &header,
                &blocks,
            );
        }
        checked(cancel)?;
        let mut blocks = blocks.into_iter();
        let base = Base {
            catalog: blocks.next().unwrap(),
            manifest: blocks.next().unwrap(),
            presence: blocks.next().unwrap(),
            packed_limit: stamp.packed_limit,
        };
        let tail = blocks.next().unwrap();
        let mut members: HashMap<_, _> = [FIRST, PREVIEW, EXACT].into_iter().zip(blocks).collect();
        let mut decoder = base.decoder()?;
        let layout = Layout::read(&decoder)?;
        let open_header = json!({"identity":stamp.identity,"file_size":stamp.size,"fixed_len":stamp.fixed_len,
            "next_comment_id":next_comment_id(&tail), "format":"takm", "has_analog":!layout.devices.is_empty(),
            "channel_order":layout.order,"device_types":layout.devices,"plate_devices":layout.plates});
        let decode = |session: &Self, decoder: &Decoder, channels, exact| {
            session.decode_open(
                path,
                &open_header,
                vec![
                    decoder.nodes().to_vec(),
                    decoder.metadata().to_vec(),
                    channels,
                    tail.clone(),
                    layout.device_headers.clone(),
                ],
                exact,
                None,
                cancel,
            )
        };
        let mut provide = |name: &str| {
            members
                .remove(name)
                .ok_or_else(|| TakError(format!("Missing SSH TAKM v2 part: {name}")))
        };
        if let Some(publish) = publish.as_mut().filter(|_| staged) {
            let first = decoder
                .first_channels(&mut provide)
                .map_err(|e| e.to_string())?;
            let pose = decode(self, &decoder, first, false)?;
            let at = i64::from(decoder.first_frame());
            validate_range(&pose.take, Some(at))?;
            let motion = layout.motion(&pose.take, at);
            publish(RemoteStage::ArchiveFirstPose(pose, motion));
            checked(cancel)?;

            let (header, mut blocks) = self.transport.followup(stamp.size as u64, cancel)?;
            stamp.check(&header, &blocks, "preview", 1)?;
            self.record_packet(ReadMoment::FirstPacket, &header, &blocks);
            members.insert(PREVIEW, blocks.pop().unwrap());
            let preview = decoder
                .preview_channels(|name| {
                    members
                        .remove(name)
                        .ok_or_else(|| TakError(format!("Missing SSH TAKM v2 part: {name}")))
                })
                .map_err(|e| e.to_string())?;
            let pose = decode(self, &decoder, preview, false)?;
            let motion = layout.motion(&pose.take, at);
            publish(RemoteStage::Preview(pose, motion));
            checked(cancel)?;

            let (header, mut blocks) = self.transport.followup(stamp.size as u64, cancel)?;
            stamp.check(&header, &blocks, "exact", 1)?;
            self.record_packet(ReadMoment::Remainder, &header, &blocks);
            members.insert(EXACT, blocks.pop().unwrap());
        }
        let channels = decoder
            .exact_channels(|name| {
                members
                    .remove(name)
                    .ok_or_else(|| TakError(format!("Missing SSH TAKM v2 part: {name}")))
            })
            .map_err(|e| e.to_string())?;
        let mut loaded = decode(self, &decoder, channels, true)?;
        validate_range(&loaded.take, None)?;
        if loaded.analog_pending {
            if loaded.device_types.is_none() {
                return Err("TAKM v2 device manifest and geometry disagree".into());
            }
            loaded.archive = Some(Arc::new(base));
        }
        loaded.marker_channels = Vec::new();
        loaded.channel_order = Vec::new();
        Ok(loaded)
    }

    fn open_opaque_archive(
        &self,
        path: &str,
        stamp: Stamp,
        blocks: Vec<Vec<u8>>,
        cancel: &CancelToken,
    ) -> Result<RemoteTake> {
        let [catalog, packed, comments]: [Vec<u8>; 3] = blocks.try_into().unwrap();
        let mut packed = Some(packed);
        let raw = restore_opaque(
            &catalog,
            Limits {
                packed: stamp.packed_limit,
                expanded: MAX_LOGICAL,
            },
            |name| {
                if name == RECOVERY {
                    packed
                        .take()
                        .ok_or_else(|| TakError("Missing SSH TAKM v2 recovery".into()))
                } else {
                    Err(TakError("Unexpected SSH TAKM v2 recovery member".into()))
                }
            },
        )
        .map_err(|e| e.to_string())?;
        let index = crate::takm::read_index_at(raw.len() as u64, 16 << 20, |dst, at| {
            dst.copy_from_slice(
                raw.get(at as usize..at as usize + dst.len())
                    .ok_or_else(|| TakError("TAKM v2 recovery range outside body".into()))?,
            );
            Ok(())
        })
        .map_err(|e| e.to_string())?;
        let get = |name: &str| {
            let row = index
                .streams
                .iter()
                .rev()
                .find(|row| row.name == name)
                .ok_or_else(|| format!("Missing TAKM v2 recovered stream: {name}"))?;
            Ok::<_, String>(&raw[row.offset as usize..(row.offset + row.size) as usize])
        };
        let (nodes, meta, channels) = (
            get("Nodes.dat")?,
            get("MetaData.dat")?,
            get("Channels.dat")?,
        );
        checked(cancel)?;
        let take = tak::decode_streams(Path::new(path), stamp.size, nodes, channels, meta)
            .map_err(|e| e.to_string())?;
        validate_range(&take, None)?;
        let edit_streams = if let Ok(rows) = progressive::inventory(channels) {
            let mut marker = CHANNELS_HEADER.to_vec();
            marker.extend_from_slice(
                &((rows
                    .iter()
                    .filter(|row| !row.name.starts_with("AnalogChannel"))
                    .count()) as u32)
                    .to_le_bytes(),
            );
            for row in rows
                .iter()
                .filter(|row| !row.name.starts_with("AnalogChannel"))
            {
                marker.extend_from_slice(&channels[row.start..row.end]);
            }
            super::RemoteEditStreams {
                nodes: nodes.to_vec().into(),
                channels: marker.into(),
                metadata: meta.to_vec().into(),
            }
        } else {
            super::RemoteEditStreams {
                nodes: Vec::new().into(),
                channels: Vec::new().into(),
                metadata: Vec::new().into(),
            }
        };
        let marker_proof = if self.read_observer.is_some() {
            progressive::inventory(channels)
                .ok()
                .map(|rows| super::ChannelProof::decoded(channels, &rows, None))
        } else {
            None
        };
        checked(cancel)?;
        Ok(RemoteTake {
            take,
            source: RemoteSource {
                host: self.host.clone(),
                path: path.into(),
                identity: stamp.identity,
            },
            next_comment_id: next_comment_id(&comments),
            comments,
            edit_streams,
            fixed_len: stamp.fixed_len,
            marker_channels: Vec::new(),
            channel_order: Vec::new(),
            plate_devices: None,
            device_types: None,
            fallback_reason: Some("TAKM v2 opaque archive".into()),
            initial_read: true,
            analog_pending: false,
            channel_manifest: Vec::new(),
            archive: None,
            marker_proof,
            plate_proof: None,
        })
    }

    pub(super) fn read_archive_analog(
        &mut self,
        loaded: &mut RemoteTake,
        selection: &str,
        cancel: &CancelToken,
    ) -> Result<()> {
        let base = loaded
            .archive
            .as_deref()
            .ok_or("Missing SSH TAKM v2 analog context")?;
        let (kind, names, role): (_, &[_], _) = match selection {
            "forceplates" => (AnalogKind::Plates, &[PLATES], 2),
            "non_forceplates" => (AnalogKind::Other, &[ANALOG], 1),
            "all" => (AnalogKind::All, &[PLATES, ANALOG], 0),
            _ => return Err("Invalid TAKM v2 SSH analog selection".into()),
        };
        let mut request = json!({"op":"analog","archive_ranges":2,"selection":selection,"path":loaded.source.path,
            "expected_identity":loaded.source.identity,"max_bytes":base.packed_limit,"probe_bytes":self.config.probe_bytes});
        if self.read_observer.is_some() {
            request["telemetry"] = json!(true);
        }
        self.record_request(ReadMoment::AnalogRequested);
        let frame: Frame = self.transport.request(request, cancel)?;
        let (header, blocks) = frame;
        let stamp = Stamp {
            identity: loaded.source.identity.clone(),
            size: loaded.source.file_size() as usize,
            fixed_len: loaded.fixed_len,
            packed_limit: base.packed_limit,
        };
        stamp.check(&header, &blocks, selection, names.len())?;
        self.record_packet(ReadMoment::AnalogPacket, &header, &blocks);
        checked(cancel)?;
        let mut parts: HashMap<_, _> = names.iter().copied().zip(blocks).collect();
        let decoder = base.decoder()?;
        let channels = decoder
            .analog_channels(kind, |name| {
                parts
                    .remove(name)
                    .ok_or_else(|| TakError(format!("Missing SSH TAKM v2 analog part: {name}")))
            })
            .map_err(|e| e.to_string())?;
        let kinds = loaded
            .device_types
            .as_deref()
            .ok_or("Missing TAKM v2 device roles")?;
        let selected: Vec<_> = kinds
            .iter()
            .enumerate()
            .filter_map(|(index, &kind)| (role == 0 || role == kind).then_some(index))
            .collect();
        let (analog, warning) = tak::decode_analog(&channels);
        if let Some(error) = warning {
            return Err(error);
        }
        if analog.len() != selected.len()
            || loaded.channel_manifest.len() != loaded.take.analog.len()
            || analog.iter().zip(&selected).any(|(device, &index)| {
                let expected = &loaded.channel_manifest[index];
                device.channels.len() != expected.len()
                    || device
                        .channels
                        .iter()
                        .zip(expected)
                        .any(|(c, (name, kind, dim))| {
                            c.name != *name || c.kind != *kind || c.dim != *dim
                        })
            })
        {
            return Err("TAKM v2 analog response disagrees with its manifest".into());
        }
        checked(cancel)?;
        let proof = if self.read_observer.is_some() && role != 1 && !loaded.take.plates.is_empty() {
            Some(super::ChannelProof::decoded(
                &channels,
                &progressive::inventory(&channels)?,
                None,
            ))
        } else {
            None
        };
        for (device, index) in analog.into_iter().zip(selected) {
            loaded.take.analog[index] = device;
        }
        if proof.is_some() {
            loaded.plate_proof = proof;
        }
        loaded.initial_read = true;
        loaded.analog_pending = role == 2 && kinds.contains(&1);
        Ok(())
    }
}

fn validate_range(take: &tak::Take, first: Option<i64>) -> Result<()> {
    let start = take.start_frame;
    let end = take.end_frame;
    if start < 0
        || end > i64::from(u32::MAX)
        || end < start
        || end - start + 1 > (MAX_LOGICAL / 16) as i64
        || first.is_some_and(|at| {
            !(start..=end).contains(&at)
                || take.trajectories.iter().any(|t| {
                    t.frames.len() > 1 || t.frames.first().is_some_and(|&f| i64::from(f) != at)
                })
        })
    {
        return Err("Invalid TAKM v2 SSH frame range".into());
    }
    Ok(())
}
