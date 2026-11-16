use crate::tak::{Take, Uid};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_DOCUMENT: AtomicU64 = AtomicU64::new(1);
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

/// Identity of one opened document. A reopen gets a new identity unless its session is restored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocumentId(u64);
/// Identity of an immutable state. Branches and different documents never reuse an ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Revision(u64);
fn next_revision() -> Revision { Revision(NEXT_REVISION.fetch_add(1, Ordering::Relaxed)) }

/// Label identity: a parent asset UID and its signed slot, not a display name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LabelKey { pub parent: Uid, pub slot: i32 }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredLabel { pub key: LabelKey, pub name: String }

/// Exact on-disk little-endian frame and XYZ bytes. Float bit patterns are never normalized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawSample(pub [u8; 16]);
impl RawSample {
    pub fn frame(self) -> u32 { u32::from_le_bytes(self.0[..4].try_into().expect("four-byte slice")) }
    pub fn xyz(self) -> [f32; 3] {
        std::array::from_fn(|i| f32::from_bits(u32::from_le_bytes(self.0[4 + i * 4..8 + i * 4].try_into().expect("four-byte slice"))))
    }
    pub fn new(frame: u32, xyz: [f32; 3]) -> Self {
        let mut raw = [0; 16];
        raw[..4].copy_from_slice(&frame.to_le_bytes());
        for (i, value) in xyz.iter().enumerate() { raw[4 + i * 4..8 + i * 4].copy_from_slice(&value.to_bits().to_le_bytes()); }
        Self(raw)
    }
}

/// Raw node and Translation header, including opaque bytes. Kept byte-for-byte for writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordBytes {
    pub node: Arc<[u8]>,
    pub translation_header: Arc<[u8]>,
    /// Set only by a strict layout verifier. Lenient-only and ambiguous records stay false.
    pub serialization_verified: bool,
}
/// One trajectory in original stream order. Changes replace only touched tracks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawTrack {
    pub uid: Uid,
    pub name: String,
    pub label: Option<LabelKey>,
    pub records: RecordBytes,
    pub samples: Arc<[RawSample]>,
}
/// Original stream reference, including duplicates, in file order. Contents remain in source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamRef { pub index: usize, pub name: String }
/// Strictly parsed source information supplied by a background loader. Opaque stream prefixes,
/// declarations and unknown bytes remain available to a writer via the original source/records.
#[derive(Clone, Debug)]
pub struct DocumentInput {
    pub streams: Arc<[StreamRef]>,
    pub nodes_prefix: Arc<[u8]>,
    pub channels_prefix: Arc<[u8]>,
    pub tracks: Vec<RawTrack>,
    pub labels: Vec<DeclaredLabel>,
    pub first_frame: u32,
    pub last_frame: u32,
}

/// Validated once off-thread, then installed by swapping small owned handles on the UI thread.
pub struct PreparedInput {
    source_path: Option<PathBuf>,
    input: Arc<DocumentInput>,
    tracks: HashMap<Uid, Arc<RawTrack>>,
    labels: HashMap<LabelKey, DeclaredLabel>,
    label_tracks: HashMap<LabelKey, Vec<Uid>>,
    order: Arc<[Uid]>,
}
impl PreparedInput {
    pub fn new(input: DocumentInput) -> Result<Self, EditError> {
        if input.first_frame > input.last_frame { return Err(EditError::InvalidRange); }
        let mut tracks = HashMap::new();
        for t in &input.tracks {
            validate_track(t, input.first_frame, input.last_frame)?;
            if tracks.insert(t.uid, Arc::new(t.clone())).is_some() { return Err(EditError::DuplicateUid); }
        }
        let mut labels = HashMap::new();
        for label in &input.labels {
            if labels.insert(label.key, label.clone()).is_some() { return Err(EditError::DuplicateLabel); }
        }
        let label_tracks = index_label_tracks(&tracks);
        let order = input.tracks.iter().map(|t| t.uid).collect::<Vec<_>>().into();
        Ok(Self { source_path: None, input: Arc::new(input), tracks, labels, label_tracks, order })
    }
    pub(crate) fn for_source(mut self, path: PathBuf) -> Self { self.source_path = Some(path); self }

    /// Run on a worker after analog attachment, before installing records for an already shown
    /// file. Detect changes in displayed marker data; this is not a whole-file identity check.
    pub fn matches_shown_take(&self, shown: &Take) -> bool {
        if self.source_path.as_ref().is_some_and(|p| p != &shown.path)
            || shown.start_frame != self.input.first_frame as i64
            || shown.end_frame != self.input.last_frame as i64
            || shown.trajectories.len() != self.tracks.len() { return false; }
        let mut seen = HashSet::new();
        shown.trajectories.iter().all(|track| {
            if !seen.insert(track.uid) { return false; }
            let Some(raw) = self.tracks.get(&track.uid) else { return false };
            if raw.name != track.name || raw.samples.len() != track.frames.len() || track.xyz.len() != track.frames.len() { return false; }
            let shown_label = match (track.parent_uid, track.slot) {
                (Some(parent), Some(slot)) if parent != [0; 16] => Some(LabelKey { parent, slot }),
                _ => None,
            };
            if raw.records.serialization_verified && shown_label != raw.label { return false; }
            raw.samples.iter().zip(track.frames.iter().zip(&track.xyz)).all(|(sample, (&frame, xyz))| {
                sample.frame() == frame && sample.xyz().map(f32::to_bits) == xyz.map(f32::to_bits)
            })
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditSelection {
    pub selected: BTreeSet<Uid>,
    /// Selected UIDs in selection order, oldest first; the primary UID is last.
    pub order: Vec<Uid>,
    pub primary: Option<Uid>,
    pub armed_label: Option<LabelKey>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AffectedFrames { pub first: u32, pub last: u32 }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditResult { pub tracks: usize, pub samples: usize, pub gaps: usize, pub skipped: usize, pub message: String }
/// `None` before/after means creating/removing a trajectory and its records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackChange { pub uid: Uid, pub before: Option<Arc<RawTrack>>, pub after: Option<Arc<RawTrack>> }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    NotLoaded,
    AlreadyLoaded,
    StaleRevision,
    WrongDocument,
    DuplicateUid,
    DuplicateLabel,
    MissingDeclaration,
    AmbiguousLabel,
    InvalidFrameOrder,
    InvalidRange,
    BeforeMismatch,
    UnsupportedLayout,
    NoChange,
    FileChangedOnDisk,
    Other(String),
}
impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{self:?}") }
}
impl std::error::Error for EditError {}

#[derive(Debug)]
struct State {
    revision: Revision,
    input: Option<Arc<DocumentInput>>,
    tracks: HashMap<Uid, Arc<RawTrack>>,
    labels: HashMap<LabelKey, DeclaredLabel>,
    label_tracks: HashMap<LabelKey, Vec<Uid>>,
    order: Arc<[Uid]>,
}
/// Cheap immutable input for an operation; shares unchanged records and sample arrays.
#[derive(Clone, Debug)]
pub struct DocumentSnapshot { id: DocumentId, state: Arc<State> }
impl DocumentSnapshot {
    pub fn document_id(&self) -> DocumentId { self.id }
    pub fn revision(&self) -> Revision { self.state.revision }
    pub fn is_loaded(&self) -> bool { self.state.input.is_some() }
    pub fn input(&self) -> Option<&DocumentInput> { self.state.input.as_deref() }
    pub fn track(&self, uid: &Uid) -> Option<&Arc<RawTrack>> { self.state.tracks.get(uid) }
    pub fn tracks(&self) -> impl Iterator<Item = &Arc<RawTrack>> { self.state.tracks.values() }
    pub fn track_order(&self) -> &[Uid] { &self.state.order }
    pub fn label(&self, key: &LabelKey) -> Option<&DeclaredLabel> { self.state.labels.get(key) }
    pub fn labels(&self) -> impl Iterator<Item = &DeclaredLabel> { self.state.labels.values() }
    /// More than one UID means the label is ambiguous and an edit must reject it.
    pub fn track_uids_for_label(&self, key: &LabelKey) -> &[Uid] { self.state.label_tracks.get(key).map_or(&[], Vec::as_slice) }
}

/// An open document, independent of `ViewData` and its analog attachment. Cloning is O(1).
#[derive(Clone, Debug)]
pub struct EditDocument {
    id: DocumentId,
    source_path: PathBuf,
    save_path: Option<PathBuf>,
    current: Arc<State>,
    saved: Arc<State>,
}
impl EditDocument {
    /// Does not read or copy a file. Install verified records later on a worker.
    pub fn unloaded(source_path: PathBuf) -> Self {
        let state = Arc::new(State { revision: next_revision(), input: None, tracks: HashMap::new(), labels: HashMap::new(), label_tracks: HashMap::new(), order: Arc::from([]) });
        Self { id: DocumentId(NEXT_DOCUMENT.fetch_add(1, Ordering::Relaxed)), source_path, save_path: None, current: state.clone(), saved: state }
    }
    pub fn id(&self) -> DocumentId { self.id }
    pub fn source_path(&self) -> &Path { &self.source_path }
    pub fn save_path(&self) -> Option<&Path> { self.save_path.as_deref() }
    pub fn revision(&self) -> Revision { self.current.revision }
    pub fn saved_revision(&self) -> Revision { self.saved.revision }
    pub fn is_dirty(&self) -> bool { self.current.revision != self.saved.revision }
    pub fn snapshot(&self) -> DocumentSnapshot { DocumentSnapshot { id: self.id, state: self.current.clone() } }
    pub fn saved_snapshot(&self) -> DocumentSnapshot { DocumentSnapshot { id: self.id, state: self.saved.clone() } }

    /// Convenience for worker-side setup. Do not scan input on the drawing thread.
    pub fn install_input(&mut self, expected: Revision, input: DocumentInput) -> Result<(), EditError> {
        self.install_prepared(expected, PreparedInput::new(input)?)
    }
    /// Install a worker-validated input in O(1), provided the document has not changed.
    pub fn install_prepared(&mut self, expected: Revision, prepared: PreparedInput) -> Result<(), EditError> {
        if self.revision() != expected { return Err(EditError::StaleRevision); }
        if self.current.input.is_some() { return Err(EditError::AlreadyLoaded); }
        if prepared.source_path.as_ref().is_some_and(|path| path != &self.source_path) { return Err(EditError::WrongDocument); }
        let state = Arc::new(State { revision: expected, input: Some(prepared.input), tracks: prepared.tracks, labels: prepared.labels, label_tracks: prepared.label_tracks, order: prepared.order });
        self.current = state.clone();
        self.saved = state;
        Ok(())
    }
    /// Publish one already prepared edit or its inverse in O(1). A stale result changes nothing.
    pub fn apply(&mut self, patch: &EditPatch, redo: bool) -> Result<(), EditError> {
        if self.id != patch.id { return Err(EditError::WrongDocument); }
        let (from, to) = if redo { (&patch.before, &patch.after) } else { (&patch.after, &patch.before) };
        if self.revision() != from.revision { return Err(EditError::StaleRevision); }
        self.current = to.clone();
        Ok(())
    }
    /// Record the snapshot actually written, even if a newer edit exists. The newer edit remains dirty.
    pub fn mark_saved(&mut self, written: &DocumentSnapshot, path: PathBuf) -> Result<(), EditError> {
        if written.id != self.id { return Err(EditError::WrongDocument); }
        if !written.is_loaded() { return Err(EditError::NotLoaded); }
        self.saved = written.state.clone();
        self.save_path = Some(path);
        Ok(())
    }
    /// Discard unsaved changes. The caller must also remove this document's discarded history.
    pub fn discard_unsaved(&mut self) { self.current = self.saved.clone(); }
}

fn validate_track(track: &RawTrack, first: u32, last: u32) -> Result<(), EditError> {
    let mut previous = None;
    for sample in track.samples.iter() {
        let frame = sample.frame();
        if frame < first || frame > last || previous.is_some_and(|p| frame <= p) { return Err(EditError::InvalidFrameOrder); }
        previous = Some(frame);
    }
    Ok(())
}

fn index_label_tracks(tracks: &HashMap<Uid, Arc<RawTrack>>) -> HashMap<LabelKey, Vec<Uid>> {
    let mut result: HashMap<LabelKey, Vec<Uid>> = HashMap::new();
    for track in tracks.values() {
        if let Some(key) = track.label { result.entry(key).or_default().push(track.uid); }
    }
    for ids in result.values_mut() { ids.sort_unstable(); }
    result
}

fn changes_fit_range(before: Option<&RawTrack>, after: Option<&RawTrack>, range: AffectedFrames) -> bool {
    let left = before.map_or(&[][..], |t| &t.samples);
    let right = after.map_or(&[][..], |t| &t.samples);
    let (mut i, mut j) = (0, 0);
    while i < left.len() || j < right.len() {
        let a = left.get(i).map(|s| s.frame());
        let b = right.get(j).map(|s| s.frame());
        let frame = match (a, b) {
            (Some(x), Some(y)) if x == y => { let changed = left[i] != right[j]; i += 1; j += 1; if !changed { continue; } x }
            (Some(x), Some(y)) if x < y => { i += 1; x }
            (Some(_), Some(y)) => { j += 1; y }
            (Some(x), None) => { i += 1; x }
            (None, Some(y)) => { j += 1; y }
            (None, None) => break,
        };
        if frame < range.first || frame > range.last { return false; }
    }
    true
}

/// An atomic transaction, prepared off the UI thread. Both immutable states share unchanged tracks.
#[derive(Clone, Debug)]
pub struct EditPatch {
    id: DocumentId,
    before: Arc<State>,
    after: Arc<State>,
    pub changes: Arc<[TrackChange]>,
    pub selection_before: EditSelection,
    pub selection_after: EditSelection,
    pub affected: AffectedFrames,
    pub result: EditResult,
}
impl EditPatch {
    pub fn prepare(snapshot: &DocumentSnapshot, changes: Vec<TrackChange>, selection_before: EditSelection, selection_after: EditSelection, affected: AffectedFrames, result: EditResult) -> Result<Self, EditError> {
        let input = snapshot.input().ok_or(EditError::NotLoaded)?;
        if affected.first > affected.last || affected.first < input.first_frame || affected.last > input.last_frame { return Err(EditError::InvalidRange); }
        for name in ["Nodes.dat", "Channels.dat"] {
            if input.streams.iter().filter(|s| s.name == name).count() > 1 { return Err(EditError::UnsupportedLayout); }
        }
        let mut tracks = snapshot.state.tracks.clone();
        let mut order = snapshot.state.order.to_vec();
        let mut touched = HashSet::new();
        let mut changed = false;
        for change in &changes {
            if !touched.insert(change.uid) { return Err(EditError::DuplicateUid); }
            if tracks.get(&change.uid) != change.before.as_ref() { return Err(EditError::BeforeMismatch); }
            for track in [&change.before, &change.after].into_iter().flatten() {
                if track.uid != change.uid { return Err(EditError::BeforeMismatch); }
                if !track.records.serialization_verified { return Err(EditError::UnsupportedLayout); }
                validate_track(track, input.first_frame, input.last_frame)?;
            }
            if let (Some(before), Some(after)) = (&change.before, &change.after) {
                if before.name != after.name || before.label != after.label || before.records != after.records {
                    return Err(EditError::UnsupportedLayout);
                }
            }
            if let (None, Some(after)) = (&change.before, &change.after) {
                if let Some(key) = after.label {
                    if !snapshot.state.labels.contains_key(&key) { return Err(EditError::MissingDeclaration); }
                    if tracks.values().any(|track| track.label == Some(key)) { return Err(EditError::AmbiguousLabel); }
                }
            }
            if change.before.is_some() && change.after.is_none() && input.tracks.iter().any(|t| t.uid == change.uid) {
                return Err(EditError::UnsupportedLayout);
            }
            if !changes_fit_range(change.before.as_deref(), change.after.as_deref(), affected) { return Err(EditError::InvalidRange); }
            if change.before != change.after { changed = true; }
            match &change.after {
                Some(track) => { if change.before.is_none() { order.push(change.uid); } tracks.insert(change.uid, track.clone()); },
                None => { tracks.remove(&change.uid); order.retain(|uid| *uid != change.uid); }
            }
        }
        if !changed { return Err(EditError::NoChange); }
        let label_tracks = index_label_tracks(&tracks);
        let after = Arc::new(State { revision: next_revision(), input: snapshot.state.input.clone(), tracks, labels: snapshot.state.labels.clone(), label_tracks, order: order.into() });
        Ok(Self { id: snapshot.id, before: snapshot.state.clone(), after, changes: changes.into(), selection_before, selection_after, affected, result })
    }
    pub fn document_id(&self) -> DocumentId { self.id }
    pub fn before_revision(&self) -> Revision { self.before.revision }
    pub fn after_revision(&self) -> Revision { self.after.revision }
    pub fn before_snapshot(&self) -> DocumentSnapshot { DocumentSnapshot { id: self.id, state: self.before.clone() } }
    pub fn after_snapshot(&self) -> DocumentSnapshot { DocumentSnapshot { id: self.id, state: self.after.clone() } }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn uid(n: u8) -> Uid { [n; 16] }
    fn track(n: u8, frames: &[u32]) -> RawTrack {
        RawTrack {
            uid: uid(n), name: format!("M{n}"), label: None,
            records: RecordBytes { node: Arc::from([n, 1]), translation_header: Arc::from([n, 2]), serialization_verified: true },
            samples: frames.iter().map(|&f| RawSample::new(f, [f32::from_bits(0x7fc0_1234), -0.0, f as f32])).collect::<Vec<_>>().into(),
        }
    }
    fn input() -> DocumentInput {
        DocumentInput { streams: vec![StreamRef { index: 4, name: "Unknown.dat".into() }, StreamRef { index: 7, name: "Unknown.dat".into() }].into(), nodes_prefix: Arc::from([1, 2]), channels_prefix: Arc::from([3, 4]), tracks: vec![track(1, &[1, 3]), track(2, &[2])], labels: vec![DeclaredLabel { key: LabelKey { parent: uid(7), slot: -2 }, name: "same".into() }, DeclaredLabel { key: LabelKey { parent: uid(8), slot: -2 }, name: "same".into() }], first_frame: 0, last_frame: 10 }
    }
    fn document() -> EditDocument {
        let mut d = EditDocument::unloaded("a.tak".into());
        d.install_input(d.revision(), input()).unwrap();
        d
    }
    fn patch(s: &DocumentSnapshot, uid: Uid, frames: &[u32]) -> EditPatch {
        let before = s.track(&uid).unwrap().clone();
        let mut after = (*before).clone();
        after.samples = frames.iter().map(|&f| RawSample::new(f, [1.0, 2.0, 3.0])).collect::<Vec<_>>().into();
        EditPatch::prepare(s, vec![TrackChange { uid, before: Some(before), after: Some(Arc::new(after)) }], EditSelection::default(), EditSelection::default(), AffectedFrames { first: 0, last: 10 }, EditResult::default()).unwrap()
    }
    #[test]
    fn raw_samples_identity_and_input_are_preserved() {
        let d = document();
        let s = d.snapshot();
        assert_eq!(s.track(&uid(1)).unwrap().samples[0].xyz()[0].to_bits(), 0x7fc0_1234);
        assert_eq!(s.track(&uid(1)).unwrap().samples[0].xyz()[1].to_bits(), (-0.0f32).to_bits());
        assert_eq!(s.input().unwrap().streams.iter().map(|s| s.index).collect::<Vec<_>>(), vec![4, 7]);
        assert_eq!(s.labels().count(), 2);
        assert_eq!(s.label(&LabelKey { parent: uid(8), slot: -2 }).unwrap().name, "same");
        assert!(!d.is_dirty());
    }
    #[test]
    fn patches_are_atomic_stale_safe_and_share_untouched_tracks() {
        let mut d = document();
        let initial = d.snapshot();
        let p = patch(&initial, uid(1), &[1, 2, 3]);
        let competing = patch(&initial, uid(2), &[2, 4]);
        assert!(Arc::ptr_eq(initial.track(&uid(2)).unwrap(), p.after_snapshot().track(&uid(2)).unwrap()));
        d.apply(&p, true).unwrap();
        assert!(d.is_dirty());
        assert_eq!(d.apply(&competing, true), Err(EditError::StaleRevision));
        assert_eq!(d.snapshot().track(&uid(2)).unwrap().samples.len(), 1);
        d.apply(&p, false).unwrap();
        assert!(!d.is_dirty());
        assert_eq!(d.snapshot().track(&uid(1)).unwrap().samples, initial.track(&uid(1)).unwrap().samples);
        d.apply(&p, true).unwrap();
        assert_eq!(d.revision(), p.after_revision());
        let mut another = document();
        assert_eq!(another.apply(&p, true), Err(EditError::WrongDocument));
    }
    #[test]
    fn savepoint_tracks_written_revision_across_later_edits_and_discard() {
        let mut d = document();
        let p = patch(&d.snapshot(), uid(1), &[1, 2, 3]);
        d.apply(&p, true).unwrap();
        let written = d.snapshot();
        let p2 = patch(&written, uid(2), &[2, 4]);
        d.apply(&p2, true).unwrap();
        d.mark_saved(&written, "a.takm".into()).unwrap();
        assert!(d.is_dirty());
        assert_eq!(d.save_path(), Some(Path::new("a.takm")));
        d.apply(&p2, false).unwrap();
        assert!(!d.is_dirty());
        d.apply(&p, false).unwrap();
        assert!(d.is_dirty());
        d.discard_unsaved();
        assert_eq!(d.revision(), written.revision());
        assert!(!d.is_dirty());
    }
    #[test]
    fn rejects_bad_input_and_bad_patches_before_publication() {
        let mut d = EditDocument::unloaded("a.tak".into());
        let rev = d.revision();
        let mut invalid = input();
        invalid.tracks[0] = track(1, &[3, 3]);
        assert_eq!(d.install_input(rev, invalid), Err(EditError::InvalidFrameOrder));
        assert!(!d.snapshot().is_loaded());
        let mut duplicate = input();
        duplicate.tracks.push(track(1, &[]));
        assert_eq!(d.install_input(rev, duplicate), Err(EditError::DuplicateUid));
        let mut duplicate = input();
        duplicate.labels.push(duplicate.labels[0].clone());
        assert_eq!(d.install_input(rev, duplicate), Err(EditError::DuplicateLabel));
        d.install_input(rev, input()).unwrap();
        let s = d.snapshot();
        let old = s.track(&uid(1)).unwrap().clone();
        let mut unsupported = (*old).clone();
        unsupported.records.serialization_verified = false;
        let change = TrackChange { uid: uid(1), before: Some(old.clone()), after: Some(Arc::new(unsupported)) };
        let prepare = |c| EditPatch::prepare(&s, c, EditSelection::default(), EditSelection::default(), AffectedFrames { first: 0, last: 10 }, EditResult::default());
        assert!(matches!(prepare(vec![change]), Err(EditError::UnsupportedLayout)));
        assert!(matches!(prepare(vec![TrackChange { uid: uid(1), before: Some(old.clone()), after: Some(old) }]), Err(EditError::NoChange)));
        let before = s.track(&uid(1)).unwrap().clone();
        let mut outside = (*before).clone();
        outside.samples = vec![RawSample::new(1, [0.0; 3]), RawSample::new(3, [0.0; 3])].into();
        assert!(matches!(EditPatch::prepare(&s, vec![TrackChange { uid: uid(1), before: Some(before), after: Some(Arc::new(outside)) }], EditSelection::default(), EditSelection::default(), AffectedFrames { first: 1, last: 1 }, EditResult::default()), Err(EditError::InvalidRange)));
        assert_eq!(d.revision(), rev);
    }
    #[test]
    fn new_tracks_require_declared_identity_and_keep_creation_order() {
        let mut d = document();
        let initial = d.snapshot();
        let mut created = track(3, &[2]);
        created.label = Some(LabelKey { parent: uid(9), slot: 1 });
        let prepare = |snapshot: &DocumentSnapshot, t: RawTrack| EditPatch::prepare(snapshot, vec![TrackChange { uid: t.uid, before: None, after: Some(Arc::new(t)) }], EditSelection::default(), EditSelection::default(), AffectedFrames { first: 0, last: 10 }, EditResult::default());
        assert!(matches!(prepare(&initial, created.clone()), Err(EditError::MissingDeclaration)));
        created.label = Some(LabelKey { parent: uid(7), slot: -2 });
        let p = prepare(&initial, created).unwrap();
        d.apply(&p, true).unwrap();
        assert_eq!(d.snapshot().track_order(), &[uid(1), uid(2), uid(3)]);
        assert_eq!(d.snapshot().track_uids_for_label(&LabelKey { parent: uid(7), slot: -2 }), &[uid(3)]);
        let mut duplicate = track(4, &[4]);
        duplicate.label = Some(LabelKey { parent: uid(7), slot: -2 });
        assert!(matches!(prepare(&d.snapshot(), duplicate), Err(EditError::AmbiguousLabel)));
        d.apply(&p, false).unwrap();
        assert_eq!(d.snapshot().track_order(), &[uid(1), uid(2)]);
    }
}
