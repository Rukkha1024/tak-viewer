//! TAKM marker saves. Run all functions in this module on a worker thread.
use super::{DocumentSnapshot, EditError, PreparedInput, Revision};
use crate::{comments, tak, takm};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);
fn error(e: impl std::fmt::Display) -> EditError { EditError::Other(e.to_string()) }
fn conflict() -> EditError { EditError::FileChangedOnDisk }

/// Fixed-part fingerprint captured when a document is loaded or a destination is chosen.
/// Comment-only appends do not change a TAKM version; other changes do. A missing target is
/// a version too. Capture before loading, and compare again after loading, on a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileVersion { path: PathBuf, digest: Option<[u8; 32]> }
impl FileVersion {
    pub fn capture(path: &Path) -> Result<Self, EditError> {
        let path = takm::canonical_target(path).map_err(error)?;
        let digest = match File::open(&path) {
            Ok(mut f) => {
                let len = if takm::is_takm_path(&path) {
                    takm::verify_fixed(&path).map_err(error)?;
                    takm::read_index(&f, f.metadata().map_err(error)?.len()).map_err(error)?.tail_start()
                } else { f.metadata().map_err(error)?.len() };
                // Windows positional reads advance the cursor; hash the complete fixed part.
                f.seek(SeekFrom::Start(0)).map_err(error)?;
                let mut h = Sha256::new();
                let n = std::io::copy(&mut Read::by_ref(&mut f).take(len), &mut HashWriter(&mut h)).map_err(error)?;
                if n != len { return Err(conflict()); }
                Some(h.finalize().into())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(error(e)),
        };
        Ok(Self { path, digest })
    }
    pub fn exists(&self) -> bool { self.digest.is_some() }
    pub fn path(&self) -> &Path { &self.path }
}

/// A previously captured file that can no longer be read or verified is also a conflict.
fn check_version(path: &Path, expected: &FileVersion) -> Result<(), EditError> {
    match FileVersion::capture(path) {
        Ok(current) if current == *expected => Ok(()),
        _ => Err(conflict()),
    }
}
struct HashWriter<'a>(&'a mut Sha256);
impl Write for HashWriter<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> { self.0.update(b); Ok(b.len()) }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

/// Choose a first default destination. Existing siblings and open documents are never
/// overwritten. A protected source requires the caller to ask for a permitted Save As path.
pub fn default_destination(source: &Path, open_documents: &[PathBuf]) -> Result<PathBuf, EditError> {
    if source.to_string_lossy().contains("://") { return Err(error("원격 파일은 저장할 수 없어요")); }
    if takm::is_takm_path(source) {
        if takm::in_input(source) { return Err(error("input/ 보호: 다른 이름으로 저장해야 해요")); }
        return takm::canonical_target(source).map_err(error);
    }
    if !tak::is_take_path(source) { return Err(error("TAK 또는 TAKM 파일이 아니에요")); }
    let initial = takm::path_for(source, None);
    if takm::in_input(&initial) { return Err(error("input/ 보호: 다른 이름으로 저장해야 해요")); }
    let occupied = |p: &Path| -> Result<bool, EditError> {
        let canonical = takm::canonical_target(p).map_err(error)?;
        Ok(p.exists() || open_documents.iter().any(|o| takm::canonical_target(o).ok().as_ref() == Some(&canonical)))
    };
    if !occupied(&initial)? { return Ok(initial); }
    let stem = source.file_stem().ok_or_else(|| error("파일 이름이 없어요"))?.to_string_lossy();
    for n in 0u32.. {
        let suffix = if n == 0 { "_edited".to_owned() } else { format!("_edited_{n}") };
        let candidate = source.with_file_name(format!("{stem}{suffix}.takm"));
        if !occupied(&candidate)? { return Ok(candidate); }
    }
    Err(error("저장 이름을 만들 수 없어요"))
}

#[derive(Clone, Debug)]
pub struct SaveRequest {
    pub snapshot: DocumentSnapshot,
    pub source: PathBuf,
    pub destination: PathBuf,
    /// Version captured before/after loading, or returned by the previous successful save.
    pub expected_source: FileVersion,
    /// Version at destination selection; use an absent version for a new path.
    pub expected_destination: FileVersion,
}
#[derive(Clone, Debug)]
pub struct SaveOutcome {
    pub destination: PathBuf,
    pub written_revision: Revision,
    pub fixed_len: u64,
    pub version: FileVersion,
}
/// Only mark the written snapshot saved after success, even if newer edits exist.
pub trait SaveOperation {
    fn save(&self, request: SaveRequest) -> Result<SaveOutcome, EditError>;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct TakmSave;
impl SaveOperation for TakmSave {
    fn save(&self, request: SaveRequest) -> Result<SaveOutcome, EditError> {
        save_inner(request, |_| Ok(()))
    }
}

impl TakmSave {
    /// Abort before replacement when a viewer discards a pending save.
    pub fn save_cancellable(&self, request: SaveRequest, cancelled: &std::sync::atomic::AtomicBool) -> Result<SaveOutcome, EditError> {
        save_inner(request, |_| {
            if cancelled.load(Ordering::Acquire) { Err(error("Save cancelled")) } else { Ok(()) }
        })
    }
}

struct SourceStream { name: String, index: usize, bytes: Vec<u8> }
struct SourceData { streams: Vec<SourceStream>, table: Value, tail: Vec<u8> }
fn source_data(path: &Path) -> Result<SourceData, EditError> {
    let raw = std::fs::read(path).map_err(error)?;
    if takm::is_takm_path(path) {
        let index = takm::read_index_path(path).map_err(error)?;
        let table_offset = u64::from_le_bytes(raw[16..24].try_into().map_err(error)?) as usize;
        let fixed = usize::try_from(index.fixed_len).map_err(error)?;
        let table: Value = serde_json::from_slice(raw.get(table_offset..fixed).ok_or(EditError::UnsupportedLayout)?).map_err(error)?;
        let mut streams = Vec::new();
        for s in index.streams {
            let a = usize::try_from(s.offset).map_err(error)?;
            let b = a.checked_add(usize::try_from(s.size).map_err(error)?).ok_or(EditError::UnsupportedLayout)?;
            streams.push(SourceStream { name: s.name, index: s.index, bytes: raw.get(a..b).ok_or(EditError::UnsupportedLayout)?.to_vec() });
        }
        let tail_start = fixed.checked_add(takm::SEAL_LEN as usize).ok_or(EditError::UnsupportedLayout)?;
        Ok(SourceData { streams, table, tail: raw.get(tail_start..).ok_or(EditError::UnsupportedLayout)?.to_vec() })
    } else if tak::is_take_path(path) {
        let c = tak::container::Container::parse(&raw).map_err(error)?;
        let entries = c.streams().map_err(error)?;
        let mut streams = Vec::new();
        let mut dropped = Vec::new();
        for (index, (name, entry)) in entries.iter().enumerate() {
            let bytes = c.read_stream(*entry).map_err(error)?;
            if name == takm::CAMERA_STREAM {
                dropped.push(json!({"name": name, "index": index, "size": bytes.len(), "sha256": format!("{:x}", Sha256::digest(&bytes))}));
            } else { streams.push(SourceStream { name: name.clone(), index, bytes }); }
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let made_at = comments::iso_time(comments::civil_from_unix(secs), 0);
        let table = json!({"format":"takm", "layout":takm::LAYOUT,
            "made_by":format!("tak_viewer {}", env!("CARGO_PKG_VERSION")), "made_at":made_at,
            "source":{"name":name, "size":raw.len(), "sha256":format!("{:x}", Sha256::digest(&raw))},
            "streams":[], "dropped":dropped});
        let mut tail = comments::header_line().into_bytes();
        if let Some(parsed) = comments::read(&comments::path_for(path)).map_err(error)? {
            for e in parsed.events { tail.extend_from_slice(comments::encode(&e).as_bytes()); }
        }
        Ok(SourceData { streams, table, tail })
    } else { Err(error("원본이 TAK 또는 TAKM이 아니에요")) }
}

/// Obtain the exact original streams needed by `VerifiedCreation`. Capture and retain the
/// source version before/after loading the editable document; do not substitute a later save.
pub fn read_creation_streams(path: &Path, expected: &FileVersion) -> Result<(Vec<u8>, Vec<u8>), EditError> {
    let path = takm::canonical_target(path).map_err(error)?;
    let _guard = if takm::is_takm_path(&path) && !takm::in_input(&path) {
        Some(takm::lock_path(&path).map_err(error)?)
    } else { None };
    if FileVersion::capture(&path)? != *expected { return Err(conflict()); }
    let mut data = source_data(&path)?;
    let ni = unique_stream(&data.streams, "Nodes.dat")?;
    let ci = unique_stream(&data.streams, "Channels.dat")?;
    let nodes = std::mem::take(&mut data.streams[ni].bytes);
    let channels = std::mem::take(&mut data.streams[ci].bytes);
    if FileVersion::capture(&path)? != *expected { return Err(conflict()); }
    Ok((nodes, channels))
}

fn unique_stream(streams: &[SourceStream], name: &str) -> Result<usize, EditError> {
    let mut found = streams.iter().enumerate().filter(|(_, s)| s.name == name);
    let i = found.next().ok_or(EditError::UnsupportedLayout)?.0;
    if found.next().is_some() { return Err(EditError::UnsupportedLayout); }
    Ok(i)
}
fn change_count(bytes: &mut [u8], at: usize, delta: i64) -> Result<(), EditError> {
    let count = bytes.get_mut(at..at + 4).ok_or(EditError::UnsupportedLayout)?;
    let original = u32::from_le_bytes(count.try_into().map_err(error)?) as i64;
    let value = u32::try_from(original + delta).map_err(error)?;
    count.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Rewrite only existing marker Translation records; insert or remove only validated new nodes.
fn serialize_markers(snapshot: &DocumentSnapshot, nodes: &[u8], channels: &[u8]) -> Result<(Vec<u8>, Vec<u8>), EditError> {
    let input = snapshot.input().ok_or(EditError::NotLoaded)?;
    let assets = tak::nodes::skeleton_asset_set(nodes).map_err(error)?;
    let parsed = tak::nodes::parse_marker_nodes(nodes, &assets).map_err(error)?;
    let props = tak::nodes::property_records(nodes, &tak::nodes::MARKER_SIGNATURES).map_err(error)?;
    if parsed.len() != props.len() || props.is_empty() { return Err(EditError::UnsupportedLayout); }
    let original: HashMap<_, _> = input.tracks.iter().map(|t| (t.uid, t)).collect();
    let mut present = HashSet::new();
    let mut removals = Vec::new();
    let mut last_end = 0;
    for (node, prop) in parsed.iter().zip(&props) {
        if !present.insert(node.uid) { return Err(EditError::DuplicateUid); }
        let end = prop.end.checked_add(68).ok_or(EditError::UnsupportedLayout)?;
        if end > nodes.len() { return Err(EditError::UnsupportedLayout); }
        last_end = end;
        if let Some(base) = original.get(&node.uid) {
            if base.records.serialization_verified && &nodes[prop.start..end] != base.records.node.as_ref() { return Err(conflict()); }
            if snapshot.track(&node.uid).is_none() { return Err(EditError::UnsupportedLayout); }
        } else if snapshot.track(&node.uid).is_none() {
            // This node was written during this editing session and has since been undone or
            // emptied. The source version is checked before serialization and replacement.
            removals.push((prop.start, end));
        }
        if let Some(t) = snapshot.track(&node.uid) {
            if t.records.serialization_verified && &nodes[prop.start..end] != t.records.node.as_ref() { return Err(conflict()); }
        }
    }
    if original.keys().any(|uid| !present.contains(uid)) { return Err(conflict()); }
    let new_tracks: Vec<_> = snapshot.track_order().iter().filter_map(|uid| (!present.contains(uid)).then(|| snapshot.track(uid)).flatten()).collect();
    for t in &new_tracks {
        if !t.records.serialization_verified || t.uid == [0; 16] || t.records.node.is_empty()
            || nodes.windows(16).any(|w| w == t.uid) || channels.windows(16).any(|w| w == t.uid) {
            return Err(EditError::UnsupportedLayout);
        }
    }
    let mut new_nodes = nodes.to_vec();
    let appended: Vec<u8> = new_tracks.iter().flat_map(|t| t.records.node.iter().copied()).collect();
    new_nodes.splice(last_end..last_end, appended);
    for (start, end) in removals.iter().rev() { new_nodes.drain(*start..*end); }
    change_count(&mut new_nodes, 1, new_tracks.len() as i64 - removals.len() as i64)?;

    let inventory = tak::channels::iter_channel_inventory(channels).map_err(error)?;
    let mut rebuilt = channels[..10].to_vec();
    let mut seen = HashSet::new();
    for rec in &inventory {
        if present.contains(&rec.uid) && rec.kind == "Vector3fChannel" && rec.name == "Translation" {
            if !seen.insert(rec.uid) { return Err(EditError::UnsupportedLayout); }
            if let Some(t) = snapshot.track(&rec.uid) {
                let header = &channels[rec.start..rec.data_start];
                if header.len() < 4 || t.records.translation_header.len() != header.len()
                    || t.records.translation_header[..header.len()-4] != header[..header.len()-4] { return Err(conflict()); }
                rebuilt.extend_from_slice(&header[..header.len()-4]);
                rebuilt.extend_from_slice(&u32::try_from(t.samples.len()).map_err(error)?.to_le_bytes());
                for s in t.samples.iter() { rebuilt.extend_from_slice(&s.0); }
            }
        } else {
            // Analog and all other non-marker records remain opaque, including records the
            // viewer has never opened. Saving must not depend on decoded display samples.
            rebuilt.extend_from_slice(&channels[rec.start..rec.end]);
        }
    }
    if seen != present { return Err(EditError::UnsupportedLayout); }
    for t in &new_tracks {
        let h = &t.records.translation_header;
        if h.len() < 4 { return Err(EditError::UnsupportedLayout); }
        rebuilt.extend_from_slice(&h[..h.len()-4]);
        rebuilt.extend_from_slice(&u32::try_from(t.samples.len()).map_err(error)?.to_le_bytes());
        for s in t.samples.iter() { rebuilt.extend_from_slice(&s.0); }
    }
    change_count(&mut rebuilt, 6, new_tracks.len() as i64 - removals.len() as i64)?;
    Ok((new_nodes, rebuilt))
}

fn write_takm(file: &mut File, source: &mut SourceData, inject: &mut impl FnMut(&str) -> Result<(), EditError>) -> Result<u64, EditError> {
    let mut offset = takm::HEADER_LEN;
    let mut entries = Vec::new();
    for s in &source.streams {
        entries.push(json!({"name":s.name, "index":s.index, "offset":offset, "size":s.bytes.len()}));
        offset = offset.checked_add(s.bytes.len() as u64).ok_or(EditError::UnsupportedLayout)?;
    }
    source.table.as_object_mut().ok_or(EditError::UnsupportedLayout)?.insert("streams".into(), entries.into());
    let table = serde_json::to_vec(&source.table).map_err(error)?;
    let fixed = offset.checked_add(table.len() as u64).ok_or(EditError::UnsupportedLayout)?;
    let mut header = [0u8; takm::HEADER_LEN as usize];
    header[..8].copy_from_slice(&takm::MAGIC);
    header[8..12].copy_from_slice(&takm::LAYOUT.to_le_bytes());
    header[16..24].copy_from_slice(&offset.to_le_bytes());
    header[24..32].copy_from_slice(&(table.len() as u64).to_le_bytes());
    header[32..40].copy_from_slice(&fixed.to_le_bytes());
    let mut h = Sha256::new();
    for (i, bytes) in std::iter::once(header.as_slice()).chain(source.streams.iter().map(|s| s.bytes.as_slice())).chain(std::iter::once(table.as_slice())).enumerate() {
        file.write_all(bytes).map_err(error)?;
        h.update(bytes);
        if i == 0 { inject("during_write")?; }
    }
    file.write_all(&h.finalize()).map_err(error)?;
    file.write_all(&source.tail).map_err(error)?;
    file.flush().map_err(error)?;
    file.sync_all().map_err(error)?;
    Ok(fixed)
}
fn validate_temp(path: &Path, snapshot: &DocumentSnapshot, source: &SourceData) -> Result<(), EditError> {
    takm::verify_fixed(path).map_err(error)?;
    let mut file = File::open(path).map_err(error)?;
    let idx = takm::read_index(&file, file.metadata().map_err(error)?.len()).map_err(error)?;
    if idx.streams.len() != source.streams.len() { return Err(EditError::UnsupportedLayout); }
    for (actual, expected) in idx.streams.iter().zip(&source.streams) {
        if actual.name != expected.name || actual.index != expected.index || actual.size != expected.bytes.len() as u64 { return Err(EditError::UnsupportedLayout); }
        file.seek(SeekFrom::Start(actual.offset)).map_err(error)?;
        let mut bytes = vec![0; expected.bytes.len()];
        file.read_exact(&mut bytes).map_err(error)?;
        if bytes != expected.bytes { return Err(EditError::UnsupportedLayout); }
    }
    file.seek(SeekFrom::Start(idx.tail_start())).map_err(error)?;
    let mut tail = Vec::new(); file.read_to_end(&mut tail).map_err(error)?;
    if tail != source.tail { return Err(EditError::UnsupportedLayout); }
    let (loaded, _) = PreparedInput::read_from_path(path)?;
    let mut doc = super::EditDocument::unloaded(path.to_path_buf());
    doc.install_prepared(doc.revision(), loaded)?;
    let reopened = doc.snapshot();
    if snapshot.tracks().count() != reopened.tracks().count() { return Err(EditError::UnsupportedLayout); }
    for track in snapshot.tracks() {
        let other = reopened.track(&track.uid).ok_or(EditError::UnsupportedLayout)?;
        if track.name != other.name || track.label != other.label || track.samples != other.samples || track.records.node != other.records.node { return Err(EditError::UnsupportedLayout); }
    }
    Ok(())
}
fn complete_digest(path: &Path) -> Result<Option<[u8;32]>, EditError> {
    match File::open(path) {
        Ok(mut file) => {
            let mut hash = Sha256::new();
            std::io::copy(&mut file, &mut HashWriter(&mut hash)).map_err(error)?;
            Ok(Some(hash.finalize().into()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(error(e)),
    }
}
struct Temp(PathBuf);
impl Drop for Temp { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }
fn backup_destination(source: &Path, path: PathBuf) -> Result<Temp, EditError> {
    match std::fs::hard_link(source, &path) {
        Ok(()) => Ok(Temp(path)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(error(e)),
        Err(_) => {
            // Some filesystems cannot make hard links. A synced independent copy still lets
            // a failed post-replace directory sync restore the complete old destination.
            let mut source = File::open(source).map_err(error)?;
            let mut target = OpenOptions::new().write(true).create_new(true).open(&path).map_err(error)?;
            let guard = Temp(path);
            let copied = std::io::copy(&mut source, &mut target).and_then(|_| target.sync_all());
            drop(target);
            copied.map_err(error)?;
            Ok(guard)
        }
    }
}
fn save_inner(request: SaveRequest, mut inject: impl FnMut(&str) -> Result<(), EditError>) -> Result<SaveOutcome, EditError> {
    if !request.snapshot.is_loaded() { return Err(EditError::NotLoaded); }
    if request.source.to_string_lossy().contains("://") || request.destination.to_string_lossy().contains("://") { return Err(error("원격 파일은 저장할 수 없어요")); }
    if !takm::is_takm_path(&request.destination) || takm::in_input(&request.destination) { return Err(error("input/ 밖의 TAKM으로 저장해야 해요")); }
    let source_path = takm::canonical_target(&request.source).map_err(error)?;
    let destination = takm::canonical_target(&request.destination).map_err(error)?;
    if !takm::is_takm_path(&destination) || takm::in_input(&destination) { return Err(error("안전하지 않은 저장 경로예요")); }
    if source_path == destination && tak::is_take_path(&source_path) { return Err(error("원본 TAK를 덮어쓸 수 없어요")); }
    // Wait before taking path locks: registered writer threads may need those locks in order
    // to finish events queued before this save. No UI thread may call this function.
    if tak::is_take_path(&source_path) {
        let csv = comments::path_for(&source_path);
        comments::drain_shared_writers(&[&source_path, &destination, &csv]).map_err(error)?;
    } else {
        comments::drain_shared_writers(&[&source_path, &destination]).map_err(error)?;
    }
    inject("after_drain")?;
    let mut paths = vec![destination.clone()];
    if takm::is_takm_path(&source_path) && !takm::in_input(&source_path) && source_path != destination { paths.push(source_path.clone()); }
    if tak::is_take_path(&source_path) {
        let csv = comments::path_for(&source_path);
        if !takm::in_input(&csv) { paths.push(takm::canonical_target(&csv).map_err(error)?); }
    }
    paths.sort(); paths.dedup();
    let _locks: Vec<_> = paths.iter().map(|p| takm::lock_path(p).map_err(error)).collect::<Result<_, _>>()?;
    inject("after_locks")?;
    check_version(&source_path, &request.expected_source)?;
    check_version(&destination, &request.expected_destination)?;
    let dest_before = complete_digest(&destination)?;
    let source_before = complete_digest(&source_path)?;
    let mut data = source_data(&source_path)?;
    let ni = unique_stream(&data.streams, "Nodes.dat")?;
    let ci = unique_stream(&data.streams, "Channels.dat")?;
    unique_stream(&data.streams, "MetaData.dat")?;
    let (nodes, channels) = serialize_markers(&request.snapshot, &data.streams[ni].bytes, &data.streams[ci].bytes)?;
    data.streams[ni].bytes = nodes;
    data.streams[ci].bytes = channels;
    inject("before_write")?;
    let name = destination.file_name().unwrap_or_default().to_string_lossy();
    let (seq, tmp_path, mut file) = loop {
        let seq = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = destination.with_file_name(format!(".{name}.{}.{}.tmp.takm", std::process::id(), seq));
        match OpenOptions::new().write(true).read(true).create_new(true).open(&path) {
            Ok(file) => break (seq, path, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(error(e)),
        }
    };
    let tmp = Temp(tmp_path);
    let written = write_takm(&mut file, &mut data, &mut inject);
    drop(file);
    let fixed_len = written?;
    inject("after_write")?;
    validate_temp(&tmp.0, &request.snapshot, &data)?;
    inject("after_validate")?;
    if complete_digest(&source_path)? != source_before || complete_digest(&destination)? != dest_before { return Err(conflict()); }
    check_version(&source_path, &request.expected_source)?;
    check_version(&destination, &request.expected_destination)?;
    let temp_version = FileVersion::capture(&tmp.0)?;
    let version = FileVersion { path: destination.clone(), digest: temp_version.digest };
    #[cfg(unix)]
    let parent_dir = File::open(destination.parent().ok_or(EditError::UnsupportedLayout)?).map_err(error)?;
    // Keep an independently named link to the old inode until directory sync succeeds, so an
    // injected or actual sync failure can restore the old destination under the same lock.
    let backup = if request.expected_destination.exists() {
        let p = destination.with_file_name(format!(".{name}.{}.{}.backup", std::process::id(), seq));
        Some(backup_destination(&destination, p)?)
    } else { None };
    inject("before_replace")?;
    if let Some(old) = &backup {
        if complete_digest(&old.0)? != dest_before || complete_digest(&destination)? != dest_before { return Err(conflict()); }
        std::fs::rename(&tmp.0, &destination).map_err(error)?;
    } else {
        // Atomic create-if-absent; never use a replacing rename for a new destination.
        takm::place_new(&tmp.0, &destination).map_err(error)?;
    }
    let synced = inject("after_replace").and_then(|()| {
        #[cfg(unix)]
        parent_dir.sync_all().map_err(error)?;
        Ok(())
    });
    if let Err(sync_error) = synced {
        let restored = if let Some(old) = &backup { std::fs::rename(&old.0, &destination) }
                       else { std::fs::remove_file(&destination) };
        if let Err(restore_error) = restored {
            // Keep the old inode under its backup name if rollback itself failed.
            let location = backup.as_ref().map(|old| old.0.display().to_string());
            if let Some(old) = backup { std::mem::forget(old); }
            return Err(error(format!("저장 후 복구 실패: {sync_error}; {restore_error}; 이전 파일 보관 위치: {}", location.as_deref().unwrap_or("없음"))));
        }
        #[cfg(unix)]
        let _ = parent_dir.sync_all();
        return Err(sync_error);
    }
    Ok(SaveOutcome { destination, written_revision: request.snapshot.revision(), fixed_len, version })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{AffectedFrames, EditDocument, EditPatch, EditResult, EditSelection, RawSample, TrackChange};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    struct Fixture { dir: PathBuf, tak: PathBuf, doc: EditDocument }
    impl Fixture {
        fn new(name: &str) -> Self {
            Self::from_bytes(name, &crate::sample_tak::sample_tak())
        }
        fn from_bytes(name: &str, bytes: &[u8]) -> Self {
            let dir = std::env::temp_dir().join(format!("tak_2c_{name}_{}_{}", std::process::id(), TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).unwrap();
            let tak = dir.join("test.tak");
            std::fs::write(&tak, bytes).unwrap();
            let (input, _) = PreparedInput::read_from_path(&tak).unwrap();
            let mut doc = EditDocument::unloaded(tak.clone());
            doc.install_prepared(doc.revision(), input).unwrap();
            Self { dir, tak, doc }
        }
        fn request(&self, source: &Path, dest: &Path) -> SaveRequest {
            SaveRequest { snapshot: self.doc.snapshot(), source: source.into(), destination: dest.into(),
                expected_source: FileVersion::capture(source).unwrap(), expected_destination: FileVersion::capture(dest).unwrap() }
        }
        fn edit(&mut self, add: f32) -> EditPatch {
            let snap = self.doc.snapshot();
            let before = snap.track(&snap.track_order()[0]).unwrap().clone();
            let mut after = (*before).clone();
            let mut samples = after.samples.to_vec();
            let mut point = samples[0].xyz(); point[0] += add;
            samples[0] = RawSample::new(samples[0].frame(), point);
            after.samples = samples.into();
            let patch = EditPatch::prepare(&snap, vec![TrackChange { uid: before.uid, before: Some(before), after: Some(Arc::new(after)) }],
                EditSelection::default(), EditSelection::default(), AffectedFrames { first: 0, last: 299 }, EditResult::default()).unwrap();
            self.doc.apply(&patch, true).unwrap();
            patch
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); } }
    fn event(id: u64, file: String) -> comments::Event {
        comments::Event { time: "2026-01-01T00:00:00+00:00".into(), kind: comments::Kind::Add, id,
            markers: vec!["M".into()], start: 1, end: 2, comment_type: String::new(), text: format!("event {id}"),
            shown_a: "1".into(), shown_b: String::new(), file_a: file, file_b: String::new(), operation: None, expected: None }
    }
    fn saved(f: &mut Fixture) -> (PathBuf, SaveOutcome) {
        f.edit(1.0);
        let out = f.dir.join("saved.takm");
        let outcome = TakmSave.save(f.request(&f.tak, &out)).unwrap();
        (out, outcome)
    }

    fn raw_analog_records(data: &SourceData) -> Vec<Vec<u8>> {
        let channels = &data.streams[unique_stream(&data.streams, "Channels.dat").unwrap()].bytes;
        tak::channels::iter_channel_inventory(channels).unwrap().into_iter()
            .filter(|r| r.name.starts_with("AnalogChannel"))
            .map(|r| channels[r.start..r.end].to_vec()).collect()
    }

    fn assert_reopened_tracks(path: &Path, expected: &DocumentSnapshot) {
        let (input, take) = PreparedInput::read_from_path(path).unwrap();
        assert!(take.analog.is_empty(), "save validation or reopening decoded analog samples");
        let mut doc = EditDocument::unloaded(path.to_path_buf());
        doc.install_prepared(doc.revision(), input).unwrap();
        let reopened = doc.snapshot();
        assert_eq!(reopened.track_order(), expected.track_order());
        for track in expected.tracks() {
            let actual = reopened.track(&track.uid).unwrap();
            assert_eq!(actual.samples, track.samples);
            assert_eq!(actual.label, track.label);
        }
    }

    #[test]
    fn unopened_plate_and_emg_bytes_survive_marker_edits_save_and_save_as() {
        let mut f = Fixture::from_bytes("opaque_analog", &crate::sample_tak::sample_tak_with_devices());
        let original = std::fs::read(&f.tak).unwrap();
        let analog = raw_analog_records(&source_data(&f.tak).unwrap());
        assert_eq!(analog.len(), 8); // Six force channels and two EMG channels.
        let (_, take) = PreparedInput::read_from_path(&f.tak).unwrap();
        assert!(take.analog.is_empty());
        assert_eq!(tak::read_take(&f.tak).unwrap().analog.len(), 2);
        let output = f.dir.join("edited.takm");
        f.edit(1.25);
        TakmSave.save(f.request(&f.tak, &output)).unwrap();
        assert_reopened_tracks(&output, &f.doc.snapshot());
        assert_eq!(raw_analog_records(&source_data(&output).unwrap()), analog);
        f.doc.mark_saved(&f.doc.snapshot(), output.clone()).unwrap();
        f.edit(-0.5);
        TakmSave.save(f.request(&output, &output)).unwrap();
        assert_reopened_tracks(&output, &f.doc.snapshot());
        assert_eq!(raw_analog_records(&source_data(&output).unwrap()), analog);
        let save_as = f.dir.join("edited_as.takm");
        TakmSave.save(f.request(&output, &save_as)).unwrap();
        assert_reopened_tracks(&save_as, &f.doc.snapshot());
        assert_eq!(raw_analog_records(&source_data(&save_as).unwrap()), analog);
        assert_eq!(std::fs::read(&f.tak).unwrap(), original);
    }

    #[test]
    fn interleaved_undecodable_emg_stays_opaque_during_edit_and_save() {
        let mut f = Fixture::from_bytes("interleaved_analog", &crate::sample_tak::sample_tak_with_devices());
        let mut source = source_data(&f.tak).unwrap();
        let ci = unique_stream(&source.streams, "Channels.dat").unwrap();
        let channels = &source.streams[ci].bytes;
        let inventory = tak::channels::iter_channel_inventory(channels).unwrap();
        assert_eq!(inventory.len(), 11);
        let mut records: Vec<_> = inventory.iter().map(|r| channels[r.start..r.end].to_vec()).collect();
        let emg = &inventory[6];
        let data_start = emg.data_start - emg.start;
        // Invalid EMG frame order is accepted as opaque bytes by marker editing. An analog
        // decode would reject it and add a warning. Odd float payloads must also stay exact.
        records[6][data_start + 8..data_start + 12].copy_from_slice(&0u32.to_le_bytes());
        for (sample, bits) in [(0, 0x7fc1_2345u32), (1, 0x8000_0000), (2, 0x7f80_0000)] {
            let at = data_start + sample * 8 + 4;
            records[6][at..at + 4].copy_from_slice(&bits.to_le_bytes());
        }
        let mut mixed = channels[..10].to_vec();
        for i in [0, 1, 8, 2, 3, 9, 4, 5, 6, 7, 10] { mixed.extend_from_slice(&records[i]); }
        let (decoded, warning) = tak::decode_analog(&mixed);
        assert!(decoded.is_empty());
        assert!(warning.unwrap().contains("Non-increasing channel frames"));
        source.streams[ci].bytes = mixed;
        let analog = raw_analog_records(&source);
        let path = f.dir.join("interleaved.takm");
        let mut file = File::create(&path).unwrap();
        write_takm(&mut file, &mut source, &mut |_| Ok(())).unwrap();
        drop(file);
        let original = std::fs::read(&path).unwrap();
        let (input, take) = PreparedInput::read_from_path(&path).unwrap();
        assert!(take.analog.is_empty());
        assert!(!take.warnings.iter().any(|w| w.contains("analog channels not loaded")));
        f.doc = EditDocument::unloaded(path.clone());
        f.doc.install_prepared(f.doc.revision(), input).unwrap();
        f.edit(0.75);
        let output = f.dir.join("interleaved_edited.takm");
        TakmSave.save(f.request(&path, &output)).unwrap();
        assert_reopened_tracks(&output, &f.doc.snapshot());
        assert_eq!(raw_analog_records(&source_data(&output).unwrap()), analog);
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn file_version_ignores_comment_appends_but_rejects_damaged_fixed_data() {
        let f = Fixture::new("file_version");
        let out = takm::convert(&f.tak, None).unwrap().out;
        let original = FileVersion::capture(&out).unwrap();
        takm::append_events(&out, &[event(1, "fixture".into())]).unwrap();
        assert_eq!(FileVersion::capture(&out).unwrap(), original);
        let mut bytes = std::fs::read(&out).unwrap();
        bytes[takm::HEADER_LEN as usize] ^= 1;
        std::fs::write(&out, bytes).unwrap();
        assert_eq!(check_version(&out, &original).unwrap_err(), EditError::FileChangedOnDisk);
    }

    #[test]
    fn save_reopens_exact_samples_preserves_other_streams_and_savepoint() {
        let mut f = Fixture::new("reopen");
        let original = std::fs::read(&f.tak).unwrap();
        let patch = f.edit(1.0);
        let out = f.dir.join("saved.takm");
        let written = f.doc.snapshot();
        let outcome = TakmSave.save(f.request(&f.tak, &out)).unwrap();
        assert_eq!(outcome.written_revision, written.revision());
        assert_eq!(std::fs::read(&f.tak).unwrap(), original);
        let before = source_data(&f.tak).unwrap();
        let after = source_data(&out).unwrap();
        assert_eq!(before.streams.len(), after.streams.len());
        for (a, b) in before.streams.iter().zip(&after.streams) {
            assert_eq!((&a.name, a.index), (&b.name, b.index));
            if a.name != "Channels.dat" { assert_eq!(a.bytes, b.bytes, "{}", a.name); }
        }
        assert_eq!(before.tail, after.tail);
        f.edit(2.0);
        f.doc.mark_saved(&written, out).unwrap();
        assert!(f.doc.is_dirty());
        f.doc.discard_unsaved();
        assert!(!f.doc.is_dirty());
        f.doc.apply(&patch, false).unwrap();
        assert!(f.doc.is_dirty());
        f.doc.apply(&patch, true).unwrap();
        assert!(!f.doc.is_dirty());
    }

    #[test]
    fn added_nodes_and_channels_survive_then_undo_removes_them() {
        let mut f = Fixture::new("new_node");
        let snap = f.doc.snapshot();
        let template = snap.track(&snap.track_order()[0]).unwrap();
        let mut new = (**template).clone();
        new.uid = [0x42; 16];
        let mut node = new.records.node.to_vec(); let len = node.len();
        node[len-49..len-33].copy_from_slice(&new.uid);
        let mut header = new.records.translation_header.to_vec(); let len = header.len();
        header[len-32..len-16].copy_from_slice(&new.uid);
        new.records.node = node.into(); new.records.translation_header = header.into();
        new.samples = vec![template.samples[0]].into();
        let patch = EditPatch::prepare(&snap, vec![TrackChange { uid: new.uid, before: None, after: Some(Arc::new(new)) }],
            EditSelection::default(), EditSelection::default(), AffectedFrames { first: 0, last: 299 }, EditResult::default()).unwrap();
        f.doc.apply(&patch, true).unwrap();
        let out = f.dir.join("added.takm");
        let first = TakmSave.save(f.request(&f.tak, &out)).unwrap();
        assert_eq!(takm::read_take(&out).unwrap().trajectories.len(), 4);
        f.doc.mark_saved(&f.doc.snapshot(), out.clone()).unwrap();
        f.doc.apply(&patch, false).unwrap();
        assert!(f.doc.is_dirty());
        let req = SaveRequest { snapshot: f.doc.snapshot(), source: out.clone(), destination: out.clone(),
            expected_source: first.version.clone(), expected_destination: first.version };
        TakmSave.save(req).unwrap();
        assert_eq!(takm::read_take(&out).unwrap().trajectories.len(), 3);
    }

    #[test]
    fn failures_restore_destination_and_remove_only_our_temporary_files() {
        let mut f = Fixture::new("failure");
        let (out, _) = saved(&mut f);
        let prior = std::fs::read(&out).unwrap();
        for stage in ["before_write", "during_write", "after_write", "after_validate", "after_replace"] {
            let req = f.request(&out, &out);
            assert!(save_inner(req, |here| if here == stage { Err(error("injected")) } else { Ok(()) }).is_err());
            assert_eq!(std::fs::read(&out).unwrap(), prior, "{stage}");
            assert!(!std::fs::read_dir(&f.dir).unwrap().any(|x| x.unwrap().file_name().to_string_lossy().contains(".tmp.takm")));
        }
    }

    #[test]
    fn failed_new_save_never_leaves_a_partial_destination() {
        let mut f = Fixture::new("failure_new");
        f.edit(1.0);
        let out = f.dir.join("new.takm");
        for stage in ["before_write", "during_write", "after_write", "after_validate", "after_replace"] {
            assert!(save_inner(f.request(&f.tak, &out), |here| if here == stage { Err(error("injected")) } else { Ok(()) }).is_err());
            assert!(!out.exists(), "{stage}");
            assert!(!std::fs::read_dir(&f.dir).unwrap().any(|x| x.unwrap().file_name().to_string_lossy().contains(".tmp.takm")));
        }
    }

    #[test]
    fn changed_source_prevents_creating_a_destination() {
        let mut f = Fixture::new("source_conflict");
        f.edit(1.0);
        let out = f.dir.join("new.takm");
        let req = f.request(&f.tak, &out);
        OpenOptions::new().append(true).open(&f.tak).unwrap().write_all(b"external").unwrap();
        assert!(TakmSave.save(req).is_err());
        assert!(!out.exists());
        assert!(f.doc.is_dirty());
    }

    #[test]
    fn external_change_is_not_overwritten() {
        let mut f = Fixture::new("conflict");
        let (out, _) = saved(&mut f);
        let req = f.request(&out, &out);
        let mut expected = std::fs::read(&out).unwrap();
        expected.extend_from_slice(b"external cut fragment");
        let result = save_inner(req, |stage| {
            if stage == "after_validate" { OpenOptions::new().append(true).open(&out).unwrap().write_all(b"external cut fragment").unwrap(); }
            Ok(())
        });
        assert_eq!(result.unwrap_err(), EditError::FileChangedOnDisk);
        assert!(f.doc.is_dirty());
        assert_eq!(std::fs::read(&out).unwrap(), expected);
    }

    #[test]
    fn invalidated_destination_reports_conflict_and_keeps_edits() {
        let mut f = Fixture::new("invalidated_destination");
        let (out, _) = saved(&mut f);
        let req = f.request(&out, &out);
        let mut changed = std::fs::read(&out).unwrap();
        changed[0] ^= 1;
        std::fs::write(&out, &changed).unwrap();
        assert_eq!(TakmSave.save(req).unwrap_err(), EditError::FileChangedOnDisk);
        assert!(f.doc.is_dirty());
        assert_eq!(std::fs::read(&out).unwrap(), changed);
    }

    #[test]
    fn comment_append_waits_for_save_and_old_and_new_events_replay() {
        let mut f = Fixture::new("comments");
        let (out, first) = saved(&mut f);
        takm::append_events(&out, &[event(1, "old.tak|123".into())]).unwrap();
        let req = f.request(&out, &out);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let save = std::thread::spawn(move || save_inner(req, |stage| {
            if stage == "after_write" { started_tx.send(()).unwrap(); release_rx.recv().unwrap(); }
            Ok(())
        }));
        started_rx.recv().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let append_path = out.clone();
        let label = comments::file_label(&out, first.fixed_len);
        let append = std::thread::spawn(move || {
            takm::append_events(&append_path, &[event(2, label)]).unwrap();
            done_tx.send(()).unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(60)).is_err());
        release_tx.send(()).unwrap();
        save.join().unwrap().unwrap(); append.join().unwrap();
        assert_eq!(takm::read_comments(&out).unwrap().events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn cancelled_marker_save_waiting_for_lock_never_replaces_file() {
        let mut f = Fixture::new("cancel_locked_save");
        let (out, _) = saved(&mut f);
        f.edit(2.0);
        let before = std::fs::read(&out).unwrap();
        let request = f.request(&out, &out);
        let lock = takm::lock_path(&out).unwrap();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = tx.send(TakmSave.save_cancellable(request, &worker_cancelled));
        });
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        cancelled.store(true, Ordering::Release);
        drop(lock);
        assert!(rx.recv_timeout(Duration::from_secs(10)).unwrap().is_err());
        worker.join().unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), before);
    }

    #[test]
    fn historical_comment_labels_and_undo_events_survive_fixed_length_changes() {
        let mut f = Fixture::new("history_labels");
        let (out, first) = saved(&mut f);
        let saved = f.doc.snapshot();
        f.doc.mark_saved(&saved, out.clone()).unwrap();
        let old_label = comments::file_label(&out, first.fixed_len);
        takm::append_events(&out, &[event(1, old_label.clone())]).unwrap();
        let before = saved.track(&saved.track_order()[0]).unwrap().clone();
        let mut after = (*before).clone();
        after.samples = after.samples[1..].to_vec().into();
        let patch = EditPatch::prepare(&saved, vec![TrackChange { uid: before.uid, before: Some(before), after: Some(Arc::new(after)) }],
            EditSelection::default(), EditSelection::default(), AffectedFrames { first: 0, last: 299 }, EditResult::default()).unwrap();
        f.doc.apply(&patch, true).unwrap();
        let second = TakmSave.save(SaveRequest { snapshot: f.doc.snapshot(), source: out.clone(), destination: out.clone(),
            expected_source: first.version.clone(), expected_destination: first.version }).unwrap();
        assert_ne!(first.fixed_len, second.fixed_len);
        let next_label = comments::file_label(&out, second.fixed_len);
        takm::append_events(&out, &[event(2, next_label.clone())]).unwrap();
        f.doc.apply(&patch, false).unwrap();
        let third = TakmSave.save(SaveRequest { snapshot: f.doc.snapshot(), source: out.clone(), destination: out.clone(),
            expected_source: second.version.clone(), expected_destination: second.version }).unwrap();
        takm::append_events(&out, &[event(3, comments::file_label(&out, third.fixed_len))]).unwrap();
        let parsed = takm::read_comments(&out).unwrap();
        assert_eq!(parsed.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(parsed.events[0].file_a, old_label);
        assert_eq!(parsed.events[1].file_a, next_label);
        assert_eq!(parsed.events[2].file_a, comments::file_label(&out, third.fixed_len));
        assert_eq!(third.fixed_len, first.fixed_len);
    }

    #[test]
    fn writer_barrier_drains_previously_queued_comments() {
        let mut f = Fixture::new("barrier");
        let (out, _) = saved(&mut f);
        let writer = comments::Writer::spawn(Box::new(|| {}));
        for id in 1..=8 { assert!(writer.send(id as usize, out.clone(), event(id, "before".into()))); }
        writer.drain().unwrap();
        let done = writer.poll();
        assert_eq!(done.len(), 8);
        assert!(done.iter().all(|d| d.result.is_ok()));
        TakmSave.save(f.request(&out, &out)).unwrap();
        assert_eq!(takm::read_comments(&out).unwrap().events.len(), 8);
        assert!(writer.send(9, f.tak.clone(), event(9, "rejected".into())));
        let barrier = writer.barrier().unwrap();
        assert!(barrier.recv().unwrap().is_err());
        assert!(writer.poll().iter().any(|d| d.tag == 9 && d.result.is_err()));
        assert!(writer.barrier().unwrap().recv().unwrap().is_err());
        assert!(writer.retry(f.tak.clone()));
        assert!(writer.barrier().unwrap().recv().unwrap().is_ok());
    }

    #[test]
    fn lock_covers_another_process() {
        if let Some(p) = std::env::var_os("TAK_2C_CHILD_APPEND") {
            takm::append_events(Path::new(&p), &[event(22, "child".into())]).unwrap();
            return;
        }
        let mut f = Fixture::new("process_lock");
        let (out, _) = saved(&mut f);
        let guard = takm::lock_path(&out).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg("edit::save::tests::lock_covers_another_process")
            .env("TAK_2C_CHILD_APPEND", &out).stdout(std::process::Stdio::null())
            .spawn().unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(child.try_wait().unwrap().is_none(), "child did not wait for the lock");
        drop(guard);
        assert!(child.wait().unwrap().success());
        assert_eq!(takm::read_comments(&out).unwrap().events[0].id, 22);
    }

    #[test]
    fn first_conversion_imports_complete_sibling_csv_events_without_modifying_csv() {
        let mut f = Fixture::new("csv_import");
        f.edit(1.0);
        let csv = comments::path_for(&f.tak);
        let prior = event(44, "old.tak|42".into());
        comments::append(&csv, &[prior.clone()]).unwrap();
        OpenOptions::new().append(true).open(&csv).unwrap().write_all(b"partial").unwrap();
        let csv_before = std::fs::read(&csv).unwrap();
        let output = f.dir.join("first.takm");
        TakmSave.save(f.request(&f.tak, &output)).unwrap();
        assert_eq!(std::fs::read(csv).unwrap(), csv_before);
        assert_eq!(takm::read_comments(&output).unwrap().events, vec![prior]);
        assert!(!takm::read_comments(&output).unwrap().cut);
    }

    #[test]
    fn first_conversion_rejects_a_failed_sibling_csv_event() {
        let mut f = Fixture::new("csv_failure");
        f.edit(1.0);
        let csv = comments::path_for(&f.tak);
        std::fs::create_dir(&csv).unwrap();
        let writer = comments::Writer::spawn(Box::new(|| {}));
        writer.barrier().unwrap().recv().unwrap().unwrap();
        assert!(writer.send(52, csv.clone(), event(52, "csv".into())));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let done = writer.poll();
            if !done.is_empty() { assert!(done[0].result.is_err()); break; }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::remove_dir(&csv).unwrap();
        let output = f.dir.join("first.takm");
        assert!(TakmSave.save(f.request(&f.tak, &output)).is_err());
        assert!(!output.exists());
        assert!(writer.barrier().unwrap().recv().unwrap().is_err());
        assert!(writer.retry(csv.clone()));
        assert!(writer.send(52, csv.clone(), event(52, "csv".into())));
        writer.drain().unwrap();
        assert!(TakmSave.save(f.request(&f.tak, &output)).is_ok());
        assert_eq!(takm::read_comments(&output).unwrap().events.len(), 1);
    }

    #[test]
    fn shared_barrier_rejects_a_failed_earlier_comment_write() {
        let mut f = Fixture::new("shared_failure");
        let (source, _) = saved(&mut f);
        let original = std::fs::read(&source).unwrap();
        let writer = comments::Writer::spawn(Box::new(|| {}));
        writer.barrier().unwrap().recv().unwrap().unwrap();
        std::fs::remove_file(&source).unwrap();
        assert!(writer.send(51, source.clone(), event(51, "failed".into())));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let done = writer.poll();
            if !done.is_empty() { assert!(done[0].result.is_err()); break; }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::write(&source, &original).unwrap();
        let output = f.dir.join("failed_as.takm");
        assert!(TakmSave.save(f.request(&source, &output)).is_err());
        assert!(!output.exists());
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert!(writer.barrier().unwrap().recv().unwrap().is_err());
        assert!(writer.retry(source.clone()));
        assert!(writer.send(51, source.clone(), event(51, "failed".into())));
        writer.drain().unwrap();
        assert!(TakmSave.save(f.request(&source, &output)).is_ok());
        assert_eq!(takm::read_comments(&output).unwrap().events.len(), 1);
    }

    #[test]
    fn save_as_drains_another_process_comment_queued_before_its_file_lock() {
        if let Some(path) = std::env::var_os("TAK_2C_CHILD_QUEUED") {
            let signal = PathBuf::from(std::env::var_os("TAK_2C_CHILD_SIGNAL").unwrap());
            let writer = comments::Writer::spawn(Box::new(|| {}));
            writer.barrier().unwrap().recv().unwrap().unwrap(); // registration ready
            assert!(writer.send(1, PathBuf::from(path), event(71, "child-old".into())));
            std::fs::write(signal, b"queued").unwrap();
            writer.drain().unwrap();
            assert!(writer.poll().iter().all(|d| d.result.is_ok()));
            return;
        }
        let mut f = Fixture::new("shared_barrier");
        let (source, _) = saved(&mut f);
        let output = f.dir.join("save_as.takm");
        let signal = f.dir.join("queued");
        let lock = takm::lock_path(&source).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg("edit::save::tests::save_as_drains_another_process_comment_queued_before_its_file_lock")
            .env("TAK_2C_CHILD_QUEUED", &source).env("TAK_2C_CHILD_SIGNAL", &signal)
            .stdout(std::process::Stdio::null()).spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while !signal.exists() { assert!(std::time::Instant::now() < deadline); std::thread::sleep(Duration::from_millis(10)); }
        let req = f.request(&source, &output);
        let save = std::thread::spawn(move || TakmSave.save(req));
        while !comments::test_shared_request_for_process(child.id()) {
            assert!(std::time::Instant::now() < deadline, "shared barrier did not reach the child");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(lock);
        assert!(child.wait().unwrap().success());
        save.join().unwrap().unwrap();
        assert_eq!(takm::read_comments(&output).unwrap().events[0].id, 71);
    }

    #[test]
    fn shared_barrier_drains_events_queued_during_an_idle_poll() {
        if let Some(path) = std::env::var_os("TAK_2C_IDLE_CHILD") {
            let paused = PathBuf::from(std::env::var_os("TAK_2C_TIMEOUT_PAUSED").unwrap());
            let queued = PathBuf::from(std::env::var_os("TAK_2C_CHILD_SIGNAL").unwrap());
            let writer = comments::Writer::spawn(Box::new(|| {}));
            writer.barrier().unwrap().recv().unwrap().unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(8);
            while !paused.exists() { assert!(std::time::Instant::now() < deadline); std::thread::sleep(Duration::from_millis(5)); }
            assert!(writer.send(1, PathBuf::from(path), event(72, "idle".into())));
            std::fs::write(queued, b"queued").unwrap();
            writer.drain().unwrap();
            return;
        }
        let mut f = Fixture::new("idle_barrier");
        let (source, _) = saved(&mut f);
        let output = f.dir.join("save_as.takm");
        let paused = f.dir.join("paused"); let release = f.dir.join("release"); let queued = f.dir.join("queued");
        let lock = takm::lock_path(&source).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg("edit::save::tests::shared_barrier_drains_events_queued_during_an_idle_poll")
            .env("TAK_2C_IDLE_CHILD", &source).env("TAK_2C_TIMEOUT_PAUSED", &paused)
            .env("TAK_2C_TIMEOUT_RELEASE", &release).env("TAK_2C_CHILD_SIGNAL", &queued)
            .stdout(std::process::Stdio::null()).spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while !queued.exists() { assert!(std::time::Instant::now() < deadline); std::thread::sleep(Duration::from_millis(5)); }
        let req = f.request(&source, &output);
        let save = std::thread::spawn(move || TakmSave.save(req));
        while !comments::test_shared_request_for_process(child.id()) {
            assert!(std::time::Instant::now() < deadline); std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::write(&release, b"go").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(comments::test_shared_request_for_process(child.id()), "barrier acknowledged a queued event");
        drop(lock);
        assert!(child.wait().unwrap().success());
        save.join().unwrap().unwrap();
        assert_eq!(takm::read_comments(&output).unwrap().events[0].id, 72);
    }

    #[test]
    fn shared_barrier_aborts_if_a_writer_exits_before_flushing() {
        // The intentionally killed process is isolated from other parallel save tests.
        if std::env::var_os("TAK_2C_TEST_CRASH").is_none() { return; }
        let mut f = Fixture::new("shared_exit");
        let (source, _) = saved(&mut f);
        let original = std::fs::read(&source).unwrap();
        let output = f.dir.join("save_as.takm");
        let signal = f.dir.join("queued");
        let lock = takm::lock_path(&source).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg("edit::save::tests::save_as_drains_another_process_comment_queued_before_its_file_lock")
            .env("TAK_2C_CHILD_QUEUED", &source).env("TAK_2C_CHILD_SIGNAL", &signal)
            .stdout(std::process::Stdio::null()).spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while !signal.exists() { assert!(std::time::Instant::now() < deadline); std::thread::sleep(Duration::from_millis(10)); }
        let req = f.request(&source, &output);
        let (done_tx, done_rx) = mpsc::channel();
        let save = std::thread::spawn(move || { done_tx.send(TakmSave.save(req)).unwrap(); });
        while !comments::test_shared_request_for_process(child.id()) {
            assert!(std::time::Instant::now() < deadline, "shared barrier did not reach the child");
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap(); child.wait().unwrap();
        assert!(done_rx.recv_timeout(Duration::from_secs(5)).unwrap().is_err());
        drop(lock);
        save.join().unwrap();
        assert!(!output.exists());
        assert_eq!(std::fs::read(source).unwrap(), original);
    }

    #[test]
    fn incomplete_tail_is_kept_byte_exact_across_save_and_save_as() {
        let mut f = Fixture::new("tail");
        let (out, _) = saved(&mut f);
        takm::append_events(&out, &[event(1, "old.tak|123".into())]).unwrap();
        OpenOptions::new().append(true).open(&out).unwrap().write_all(b"\xfftruncated").unwrap();
        let tail = source_data(&out).unwrap().tail;
        let as_path = f.dir.join("as.takm");
        let next = TakmSave.save(f.request(&out, &as_path)).unwrap();
        assert_eq!(source_data(&as_path).unwrap().tail, tail);
        takm::append_events(&as_path, &[event(2, comments::file_label(&as_path, next.fixed_len))]).unwrap();
        let events = takm::read_comments(&as_path).unwrap().events;
        assert_eq!(events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(events[0].file_a, "old.tak|123");
    }

    #[test]
    fn stale_second_document_cannot_replace_first_document_save() {
        let mut f = Fixture::new("two_docs");
        let (out, _) = saved(&mut f);
        let stale = f.request(&out, &out);
        f.edit(2.0);
        TakmSave.save(f.request(&out, &out)).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert!(TakmSave.save(stale).is_err());
        assert_eq!(std::fs::read(out).unwrap(), bytes);
    }

    #[test]
    fn duplicate_opaque_stream_names_keep_both_values_and_order() {
        let f = Fixture::new("duplicate_opaque");
        let mut data = source_data(&f.tak).unwrap();
        data.streams.push(SourceStream { name: "Other.dat".into(), index: 101, bytes: b"first".to_vec() });
        data.streams.push(SourceStream { name: "Other.dat".into(), index: 102, bytes: b"second".to_vec() });
        let source = f.dir.join("duplicate.takm");
        let mut file = File::create(&source).unwrap();
        write_takm(&mut file, &mut data, &mut |_| Ok(())).unwrap(); drop(file);
        let (input, _) = PreparedInput::read_from_path(&source).unwrap();
        let mut doc = EditDocument::unloaded(source.clone()); doc.install_prepared(doc.revision(), input).unwrap();
        let output = f.dir.join("duplicate_out.takm");
        TakmSave.save(SaveRequest { snapshot: doc.snapshot(), source: source.clone(), destination: output.clone(),
            expected_source: FileVersion::capture(&source).unwrap(), expected_destination: FileVersion::capture(&output).unwrap() }).unwrap();
        let output_data = source_data(&output).unwrap();
        let duplicates: Vec<_> = output_data.streams.iter().filter(|s| s.name == "Other.dat")
            .map(|s| (s.index, s.bytes.clone())).collect();
        assert_eq!(duplicates, vec![(101, b"first".to_vec()), (102, b"second".to_vec())]);
    }

    #[test]
    fn destination_choice_and_duplicate_stream_refusals() {
        let f = Fixture::new("name");
        assert_eq!(default_destination(&f.tak, &[]).unwrap(), f.tak.with_extension("takm"));
        let sibling = f.tak.with_extension("takm");
        std::fs::write(&sibling, b"unrelated").unwrap();
        assert_eq!(default_destination(&f.tak, &[]).unwrap(), f.dir.join("test_edited.takm"));
        assert_eq!(default_destination(&f.tak, &[f.dir.join("test_edited.takm")]).unwrap(), f.dir.join("test_edited_1.takm"));
        let streams = vec![SourceStream { name: "Nodes.dat".into(), index: 0, bytes: vec![] }, SourceStream { name: "Nodes.dat".into(), index: 3, bytes: vec![] }];
        assert!(unique_stream(&streams, "Nodes.dat").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn input_alias_cannot_be_used_for_a_save_or_comment_lock() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new("input_alias");
        let repo = f.dir.join("repo");
        std::fs::create_dir_all(repo.join("apps")).unwrap();
        std::fs::create_dir_all(repo.join("input")).unwrap();
        std::fs::write(repo.join("AGENTS.md"), "").unwrap();
        std::fs::write(repo.join("apps/AGENTS.md"), "").unwrap();
        let alias = f.dir.join("alias"); symlink(repo.join("input"), &alias).unwrap();
        let target = alias.join("save.takm");
        assert!(takm::in_input(&target));
        assert!(takm::lock_path(&target).is_err());
        assert!(TakmSave.save(f.request(&f.tak, &target)).is_err());
        let csv_alias = f.dir.join("alias.comments.csv");
        symlink(&f.tak, &csv_alias).unwrap();
        let original = std::fs::read(&f.tak).unwrap();
        assert!(comments::append(&csv_alias, &[event(1, "unsafe".into())]).is_err());
        assert_eq!(std::fs::read(&f.tak).unwrap(), original);
        let nested = alias.join("missing").join("deeper").join("save.takm");
        assert!(takm::in_input(&nested));
        assert!(takm::convert(&f.tak, Some(nested.parent().unwrap())).is_err());
        assert!(!alias.join("missing").exists());
        assert_eq!(std::fs::read_dir(repo.join("input")).unwrap().count(), 0);
    }


}
