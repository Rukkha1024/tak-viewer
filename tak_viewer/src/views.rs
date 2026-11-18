//! The 3D views: up to `MAX_VIEWS` files side by side (like VS Code editor groups). A `View`
//! holds one file's camera, frame, selection and its load. Repeated local-file views share
//! marker data, the editable document and Comments; each keeps its own frame and camera.
//! A view may carry a second file laid over its 3D (`Overlay`, plan step 7): not a view of its
//! own, it follows that view's frame and camera and is drawn as white rings only.
//! `Spot` = where a file was being viewed (frame, camera, selected marker names, chosen force
//! plate). The app keeps the latest `MAX_SPOTS` in the settings and puts a file back there when
//! it is opened again.
//! Layout of the views (which slots are on screen, where) is in `ui/dock.rs`.

use crate::camera::{Camera, Preset};
use crate::ui::panels::{self, BlockCache, DevicesState, HeatCache, LabelsState, LaneCache, PlateCache};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tak_core::comments::{self, Comments};
use tak_core::geom::{v3, Vec3};
use tak_core::playback::Playback;
use tak_core::tak::MarkerKind;
use tak_core::view_data::{AnalogPart, ViewData};
use tak_core::edit::{DocumentId, DocumentSnapshot, EditDocument, EditError, EditPatch, EditSelection, LabelKey, Revision};

mod follow;

/// One editable document per local file, shared by its views and layout history.
pub type EditSession = Arc<Mutex<EditDocument>>;
/// Captured by the initial marker reader before it opens the file.
pub type SourceStamp = Arc<Mutex<Option<(PathBuf, u64, Option<SystemTime>)>>>;

/// A closed document may keep its undo identity, but never supplies fresh-open display data.
/// Copy the old stamp before starting the new reader, which captures an independent stamp.
pub struct RetainedDocument {
    document: EditSession,
    stamp: Option<(PathBuf, u64, Option<SystemTime>)>,
    source_stamp: Option<SourceStamp>,
}

impl RetainedDocument {
    pub fn new(document: EditSession, source_stamp: Option<SourceStamp>) -> Self {
        let stamp = source_stamp.as_ref().and_then(|stamp| stamp.lock().ok().and_then(|stamp| stamp.clone()));
        Self { document, stamp, source_stamp }
    }
}

/// Live file state shared by repeated views. Cloning the handle never copies marker samples.
/// A hidden tab consumes its latest generation only when activated or explicitly notified.
#[derive(Clone)]
pub struct SharedFile(Arc<Mutex<SharedFileState>>);

struct SharedFileState {
    filters: crate::device_filter::SharedFilters,
    generation: u64,
    data: Arc<ViewData>,
    document: EditSession,
    source_stamp: Option<SourceStamp>,
    remote: Option<crate::ssh::RemoteLocation>,
    comments: SharedComments,
    emg_demand: bool,
    emg_wake: Option<(DocumentId, Sender<DocumentId>)>,
    editor_wake: Option<(DocumentId, Sender<DocumentId>)>,
}

/// File contents and write status only. Drawing caches, picked rows and reserved ID pools
/// belong to each view; copying a reserved pool would issue the same Comment ID twice.
#[derive(Clone, PartialEq)]
struct SharedComments {
    list: Comments,
    target: Option<Result<PathBuf, String>>,
    skipped: usize,
    label: Option<String>,
    pending: usize,
    pending_since: Option<Instant>,
    failed: Option<String>,
    saved_at: Option<f64>,
}

impl SharedComments {
    fn capture(state: &CommentState) -> Self {
        Self {
            list: state.list.clone(), target: state.target.clone(), skipped: state.skipped,
            label: state.label.clone(), pending: state.pending, pending_since: state.pending_since,
            failed: state.failed.clone(), saved_at: state.saved_at,
        }
    }

    fn matches(&self, state: &CommentState) -> bool {
        self.list == state.list && self.target == state.target && self.skipped == state.skipped
            && self.label == state.label && self.pending == state.pending
            && self.pending_since == state.pending_since && self.failed == state.failed
            && self.saved_at == state.saved_at
    }

    fn install(self, state: &mut CommentState) {
        if state.target != self.target {
            state.id_pool = None;
            drop_later(state.id_rx.take());
            state.id_error = None;
        }
        state.list = self.list;
        state.target = self.target;
        state.skipped = self.skipped;
        state.label = self.label;
        state.pending = self.pending;
        state.pending_since = self.pending_since;
        state.failed = self.failed;
        state.saved_at = self.saved_at;
        state.blocks = Default::default();
        state.spans = Default::default();
    }
}

pub const MAX_VIEWS: usize = 4;
/// Remembered files (`Spot`), most recent first.
pub const MAX_SPOTS: usize = 50;

/// Messages from a background open. Markers come first (`tak::fast_read`); force plate
/// forces / EMG follow when no file's markers are still loading.
pub enum LoadMsg {
    /// View data (analog still missing when `analog_pending`) and step times in ms.
    Markers(Result<(ViewData, Vec<(&'static str, f64)>), String>),
    /// Force plates / EMG, already prepared for drawing on the loading thread, and the time
    /// it took in ms.
    Analog(Result<AnalogPart, String>, f64),
}

pub const TAK_NO_COMMENTS: &str = "TAK에는 Comment를 달 수 없어요. .takm으로 저장하면 달 수 있어요.";

/// The Comments of a file being opened, read on the loading thread right after its markers
/// were sent (so it never delays "markers on screen"): a `.takm`'s Comment tail. A TAK has none
/// (its `<stem>.comments.csv` is neither read nor written; nor is one next to a `.takm`).
pub struct CommentsLoad {
    /// Where comments of this file are saved (the `.takm` itself), or why the app may not write
    /// there (plain Korean: a TAK, `input/`, the tail could not be read).
    pub target: Result<PathBuf, String>,
    /// What the tail held (None: nothing read).
    pub parsed: Option<comments::Parsed>,
    /// `file_a` of the lines written for this file: `<name>.takm|<fixed length>` (stays the
    /// same while the tail grows). None: `name|size` as before.
    pub label: Option<String>,
}

impl CommentsLoad {
    /// Read the Comments of `path` (disk work: loading thread only).
    pub fn read(path: &Path) -> CommentsLoad {
        if !tak_core::takm::is_takm_path(path) {
            return CommentsLoad { target: Err(TAK_NO_COMMENTS.to_owned()), parsed: None, label: None };
        }
        let read = tak_core::takm::read_index_path(path).and_then(|index| Ok((index.fixed_len, tak_core::takm::read_comments(path)?)));
        match read {
            Ok((fixed_len, parsed)) => CommentsLoad {
                target: tak_core::takm::check_tail(path).map(|()| path.to_path_buf()),
                parsed: Some(parsed),
                label: Some(comments::file_label(path, fixed_len)),
            },
            Err(e) => CommentsLoad { target: Err(format!("Comment를 읽지 못해서 새로 쓰지 않아요: {e}")), parsed: None, label: None },
        }
    }
}

/// One change of the saved comments, as kept for Ctrl+Z / Ctrl+Y (v0.15.0): the comment as it
/// was added or as it was before it was deleted, or before and after its note was edited
/// (v0.17.0).
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Added(comments::Comment),
    Deleted(comments::Comment),
    DeletedMany(Vec<comments::Comment>),
    Edited { before: comments::Comment, after: comments::Comment },
}

/// The selection of a view: the markers, the primary one (picked last) and the selected comment
/// (`View::picked`, v0.17.1). One undo part: a click on a comment changes them together.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub selected: BTreeSet<usize>,
    pub order: Vec<usize>,
    pub primary: Option<usize>,
    pub comment: Option<u64>,
    pub comments: crate::comment_selection::CommentSelection,
    pub list_anchors: [panels::ListSelectionState; 2],
    pub armed_label: Option<LabelKey>,
    pub linked_pick: panels::LinkedLabelPick,
}

/// A frame range of a view's band strip (`View::range`).
pub type Range = Option<(i64, i64)>;

/// Status bar words of a range: `Range 120–300`, `Range 120` (one frame), `Range cleared`.
pub fn range_words(range: Range) -> String {
    match range {
        Some((a, b)) if a != b => format!("Range {a}–{b}"),
        Some((a, _)) => format!("Range {a}"),
        None => "Range cleared".into(),
    }
}

/// Comments (주석) of the file on screen (`tak_core::comments`) and how saving them goes.
#[derive(Default)]
pub struct CommentState {
    /// The live comments; empty until the comment file is read.
    pub list: Comments,
    /// The comment file, or why nothing is written (plain Korean). None while the file is being
    /// read: no new comment then (its id could clash with one in the file).
    pub target: Option<Result<PathBuf, String>>,
    /// Lines of the file that could not be read (status bar tip).
    pub skipped: usize,
    /// `file_a` of the lines written (`CommentsLoad::label`).
    pub label: Option<String>,
    /// Writes sent and not confirmed yet.
    pub pending: usize,
    pub pending_since: Option<Instant>,
    /// A write failed (message); stays while this file is open.
    pub failed: Option<String>,
    pub id_pool: Option<std::ops::RangeInclusive<u64>>,
    pub id_rx: Option<Receiver<Result<std::ops::RangeInclusive<u64>, String>>>,
    pub id_error: Option<String>,
    /// When the last write was confirmed (`egui` input time): "saved" for a moment.
    pub saved_at: Option<f64>,
    /// Where the comment blocks sit in this view's band strip.
    pub blocks: BlockCache,
    /// The Comment bands of this view's marker graph (floating or GRAPH pane; step 6).
    pub spans: panels::SpanCache,
}

impl CommentState {
    /// Put in what the loading thread read.
    pub fn take_load(&mut self, load: CommentsLoad) {
        let parsed = load.parsed.unwrap_or_default();
        self.list = parsed.comments();
        self.skipped = parsed.skipped + usize::from(parsed.cut);
        self.label = load.label;
        self.target = Some(load.target);
        self.id_pool = None;
        self.id_rx = None;
        self.id_error = None;
    }
}

/// Marker loads running now (all views). A file's analog part waits until this is 0, so the
/// markers of every file being opened come first.
static MARKER_LOADS: AtomicUsize = AtomicUsize::new(0);

pub fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// Where a file was being viewed. Saved with the settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spot {
    pub path: String,
    pub frame: i64,
    pub pivot: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
    /// Live layout history only. Recent-file opens and persisted settings reset the mode.
    #[serde(skip)]
    pub camera_mode: (Preset, bool),
    /// Selected marker names in selection order; `primary` = the one picked last.
    /// Older settings store Labels order; restoring them puts their primary last.
    pub selected: Vec<String>,
    pub primary: Option<String>,
    /// Name of the chosen force plate (its graph floats over the 3D). Not in settings saved
    /// before v0.11.0.
    #[serde(default)]
    pub plate: Option<String>,
}

/// Keep `spot` as the newest entry of its file, at most `MAX_SPOTS` files.
pub fn remember(list: &mut Vec<Spot>, mut spot: Spot) {
    spot.camera_mode = Default::default();
    list.retain(|s| s.path != spot.path);
    list.insert(0, spot);
    list.truncate(MAX_SPOTS);
}

pub fn recall<'a>(list: &'a [Spot], path: &Path) -> Option<&'a Spot> {
    let key = path.to_string_lossy();
    list.iter().find(|s| s.path == key)
}

/// Free a file's view data on another thread (big files take a moment).
pub fn drop_later<T: Send + 'static>(old: Option<T>) {
        let _span=crate::bench_timing::span("retire.enqueue");
    if let Some(old) = old {
        crate::work_queue::retire(old);
    }
}

fn same_file_path(a: &Path, b: &Path) -> bool {
    match (tak_core::takm::canonical_target(a), tak_core::takm::canonical_target(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The second file laid over a view's 3D (plan step 7, Explorer "이 화면에 얹기"): its markers
/// are drawn as white rings at the view's frame with the view's camera. Display only: no
/// selection, Labels, graphs or comments of its own. Accepted only when its frame count and
/// frame rate are the view's (`overlay_mismatch`).
pub struct Overlay {
    pub path: PathBuf,
    pub data: Arc<ViewData>,
    /// A non-drawing view keeps the canonical document and the one set of file workers.
    /// It can hand those workers to a primary view or another overlay before closing.
    owner: Box<View>,
    shown_document: Option<DocumentId>,
}

impl Overlay {
    /// Synthetic/already prepared marker data. Normal disk loads use `from_reader` so their
    /// source stamp, Comment read and deferred device jobs survive an overlay-to-view open.
    pub fn from_data(path: PathBuf, data: ViewData) -> Self {
        let (_tx, rx) = channel();
        Self::from_reader(path, data, rx, Box::new(View::default()))
    }

    fn from_reader(path: PathBuf, data: ViewData, rx: Receiver<LoadMsg>, mut owner: Box<View>) -> Self {
        drop_later(owner.show(data, rx, 0, 1.0, 0.0));
        owner.shared_file();
        let shown_document = owner.document_id();
        Self { path, data: owner.data.as_ref().expect("shown overlay data").clone(), owner, shown_document }
    }

    fn from_shared(_path: PathBuf, shared: SharedFile) -> Result<Self, String> {
        let mut owner = Box::new(View::default());
        drop_later(owner.show_shared_file(shared, 0, 1.0, 0.0));
        let data = owner.data.as_ref().cloned().ok_or_else(|| "공유 중인 파일을 열 수 없어요".to_owned())?;
        let shown_document = owner.document_id();
        Ok(Self { path: data.take.path.clone(), data, owner, shown_document })
    }

    pub fn document_id(&self) -> Option<DocumentId> { self.owner.document_id() }

    pub fn marker_dirty(&self) -> bool { self.owner.marker_dirty() }

    pub fn shared_file(&mut self) -> Option<SharedFile> { self.owner.shared_file() }

    pub fn owner(&self) -> &View { &self.owner }

    pub fn owner_mut(&mut self) -> &mut View { &mut self.owner }

    pub fn transfer_work_to(&mut self, target: &mut View) -> bool {
        if self.owner.shares_document(target) {
            return !self.owner.owns_shared_file_work() || self.owner.transfer_shared_loads(target);
        }
        if let Some(other) = target.overlay.as_mut() {
            if self.owner.shares_document(&other.owner) {
                return !self.owner.owns_shared_file_work() || self.owner.transfer_shared_loads(&mut other.owner);
            }
        }
        false
    }

    /// Save/rename and marker publications change the same Arc used by the overlay scene.
    /// An unchanged generation does no marker walk and does not rebuild drawing caches.
    fn sync_shared(&mut self) -> bool {
        self.owner.sync_shared();
        let Some(data) = self.owner.data.as_ref() else { return false };
        let document = self.owner.document_id();
        let document_changed = self.shown_document != document;
        if Arc::ptr_eq(data, &self.data) && !document_changed { return false; }
        let markers_changed = document_changed || self.data.edit_revision != data.edit_revision || self.path != data.take.path;
        self.shown_document = document;
        self.path = data.take.path.clone();
        drop_later(Some(std::mem::replace(&mut self.data, data.clone())));
        markers_changed
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // A transferred worker has a fresh cancellation token in this retired owner.
        self.owner.cancel_load();
    }
}

/// An overlay load, an existing shared file, or a wait for the app's already running reader.
pub struct OverlayLoad {
    pub path: PathBuf,
    /// Canonical identity captured when this pending overlay was admitted.
    pub identity: Option<PathBuf>,
    pub rx: Receiver<LoadMsg>,
    /// Put back by undo / redo: its arrival is no new action (`app_undo.rs`).
    pub quiet: bool,
    pub started: Instant,
    source: Option<Box<View>>,
    shared: Option<SharedFile>,
    waiting_shared: bool,
    /// Tests: the sending end of a load started with `hold_overlay` (no reading thread).
    #[cfg(test)]
    pub held: Option<std::sync::mpsc::Sender<LoadMsg>>,
}

/// Why file `b` cannot be laid over file `a` (plain Korean, both numbers), or None: the two must
/// have the same frame count and frame rate (plan step 7).
pub fn overlay_mismatch(a: &ViewData, b: &ViewData) -> Option<String> {
    let name = |d: &ViewData| d.take.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let count = |d: &ViewData| d.end_frame - d.start_frame + 1;
    let mut why = Vec::new();
    if count(a) != count(b) {
        why.push(format!("프레임 수가 달라요 ({} {}개, {} {}개)", name(a), count(a), name(b), count(b)));
    }
    if (a.take.frame_rate - b.take.frame_rate).abs() > 1e-6 {
        why.push(format!("프레임 속도가 달라요 ({} {} Hz, {} {} Hz)", name(a), a.take.frame_rate, name(b), b.take.frame_rate));
    }
    (!why.is_empty()).then(|| format!("얹을 수 없어요: {}", why.join(", ")))
}

/// A finished marker load, handed to the app (`TakViewerApp::finish_load`).
pub struct Loaded {
    pub path: PathBuf,
    pub result: Result<(ViewData, Vec<(&'static str, f64)>), String>,
    pub rx: Receiver<LoadMsg>,
}

pub struct View {
    pub editor_revision: u64,
    #[cfg(test)]
    pub background_work_checks: std::cell::Cell<usize>,
    pub data: Option<Arc<ViewData>>,
    shared_file: Option<SharedFile>,
    shared_generation: u64,
    /// Lazy, per-file marker document; preparing raw records never delays markers-first open.
    pub edit_document: Option<EditSession>,
    pub restore_edit_document: Option<EditSession>,
    pub restore_edit_selection: Option<EditSelection>,
    /// Selected Comment restored with a reversible close/open, after its markers arrive.
    pub restore_picked: Option<Option<u64>>,
    pub restore_comment_selection: Option<crate::comment_selection::CommentSelection>,
    pub restore_list_anchors: Option<[panels::ListSelectionState; 2]>,
    pub restore_linked_pick: Option<panels::LinkedLabelPick>,
    /// Latest desired marker revision; older worker results must not be published.
    pub edit_refresh: Option<Revision>,
    pub edit_selection: Option<EditSelection>,
    pub armed_label: Option<LabelKey>,
    pub source_stamp: Option<SourceStamp>,
    /// Canonical identity of the data still displayed while another file is loading.
    shown_identity: Option<PathBuf>,
    /// Identity bound to an admitted primary read or shared-reader wait.
    pending_identity: Option<PathBuf>,
    fresh_open_candidate: Option<RetainedDocument>,
    /// Typed remote address. The path inside ViewData is an opaque memory identity only.
    pub remote: Option<crate::ssh::RemoteLocation>,
    pub remote_warning: Option<String>,
    pub remote_loading: Option<(u64, crate::ssh::RemoteLocation, PathBuf)>,
    /// Unique for each file shown in this app (egui ids, Labels reset, graph cache).
    pub file_id: usize,
    pub cam: Camera,
    pub follow_target: Option<Vec3>,
    pub pb: Playback,
    pub selected: BTreeSet<usize>,
    /// Selection order, oldest first. Kept separate from the set used for drawing.
    pub selection_order: Vec<usize>,
    pub primary: Option<usize>,
    /// The selected comment (v0.17.1, one per view or none; not saved): a click on its row in
    /// the COMMENTS pane or on its band block, or the comment just made. While it is selected, a
    /// change of the marker selection by hand becomes its markers (`app_undo.rs` `observe`), and
    /// its row has boxes for its frames and note. Cleared when it is deleted, with Esc, when the
    /// marker selection becomes empty, or when another file is shown here.
    pub picked: Option<u64>,
    pub comment_selection: crate::comment_selection::CommentSelection,
    pub comment_marker_draft: bool,
    /// The chosen force plate (at most one per view): its graph (Fx … Mz, the lanes chosen for
    /// this view slot) floats over this 3D. Chosen by a click on the plate in the 3D or
    /// "3D 위로" in the devices pane; markers stay selected.
    pub plate: Option<usize>,
    /// Frame range picked in the band strip of this view's playback bar (or with `[` / `]`):
    /// (start, end), start <= end, inside the file's frames. Cleared when another file is shown
    /// here; not saved in the settings (v0.13.0).
    pub range: Option<(i64, i64)>,
    /// Comments of this file (blocks in the band strip, `M` / `Comment` button). Emptied when
    /// another file is shown here; filled when its comment file has been read.
    pub comments: CommentState,
    /// Just shown (its spot, framing and Labels set by the app, not by a person): the undo
    /// history (`app_undo.rs`) takes it as it is once the framing is done (v0.16.0).
    pub fresh: bool,
    /// The range to put back when the file being read is shown (undo of an open, v0.16.0).
    pub restore_range: Option<Range>,
    /// The comment file of the file being opened, still coming (read after the markers).
    pub comments_rx: Option<Receiver<CommentsLoad>>,
    pub labels: LabelsState,
    pub devices: DevicesState,
    pub filters: crate::device_filter::SharedFilters,
    /// Lanes of the selected marker's graph and of the chosen plate's graph.
    pub lanes: LaneCache,
    pub(crate) tracks: crate::ui::graph::tracks::State,
    pub plate_lanes: PlateCache,
    /// Red marks of this view's timeline (playback bar).
    pub heat: HeatCache,
    /// File being read; its markers are not here yet.
    pub loading: Option<(PathBuf, Receiver<LoadMsg>)>,
    /// Another view owns the first read of this target. The coordinator attaches its live
    /// shared handle when markers arrive; no reader or poll receiver is allocated here.
    waiting_shared: Option<PathBuf>,
    /// Force plates / EMG of the shown file still coming: (file_id, messages).
    pub analog_rx: Option<(usize, Receiver<LoadMsg>)>,
    /// Deferred local job: owning it performs no file read.
    lazy_job_rx: Option<Receiver<(Option<tak_core::tak::fast_read::AnalogJob>, Option<String>)>>,
    lazy_job: Option<tak_core::tak::fast_read::AnalogJob>,
    pub emg_rx: Option<(usize, Receiver<LoadMsg>)>,
    pub emg_requested: bool,
    pub emg_failure: Option<String>,
    pub analog_fallback: Option<String>,
    analog_queue: Vec<AnalogPart>,
    analog_merge: Option<Receiver<(Arc<ViewData>, ViewData, AnalogPart, [f64;2])>>,
    /// Set when the file being read is no longer wanted.
    load_cancel: Arc<AtomicBool>,
    /// Put the file being read here once its markers arrive (remembered `Spot`).
    pub restore: Option<Spot>,
    /// Latest open: start, step times in ms, ms until markers / force plates (`--bench`).
    pub open_started: Option<Instant>,
    pub load_steps: Vec<(&'static str, f64)>,
    pub markers_ms: Option<f64>,
    pub analog_ms: Option<f64>,
    /// Frame the whole take once the 3D size is known (new file without a remembered spot).
    pub fit_pending: bool,
    /// 3D rectangle of the last frame (points), its playback bar not included: crop of
    /// captures, drop target.
    pub scene_rect: egui::Rect,
    /// The open graph floating over this view's 3D in the last frame (`ui/graph_float.rs`);
    /// None while it is folded, in the GRAPH pane, or no marker is selected.
    pub graph_rect: Option<egui::Rect>,
    /// The chosen plate's open graph over the 3D in the last frame; None while it is
    /// folded, no plate is chosen or the forces are still loading.
    pub plate_rect: Option<egui::Rect>,
    /// The second file laid over this 3D (plan step 7), and one being read for it (it takes
    /// the place of `overlay` once it arrives and fits). Both go when this view shows another
    /// file or closes. Not saved in the settings.
    pub overlay: Option<Overlay>,
    pub overlay_load: Option<OverlayLoad>,
    /// Whose markers are drawn while an overlay is on: [this view's file, the overlay] (name
    /// tags `1` / `2`, keys 1 / 2). Both true without an overlay.
    pub shown: [bool; 2],
    /// `--bench`: ms from asking for the overlay until it was on screen.
    pub overlay_ms: Option<f64>,
    /// Tests: the sending end of a load started with `hold_load` (no reading thread).
    #[cfg(test)]
    pub held: Option<std::sync::mpsc::Sender<LoadMsg>>,
}

impl Default for View {
    fn default() -> Self {
        View {
            editor_revision: 0,
            #[cfg(test)]
            background_work_checks: std::cell::Cell::new(0),
            data: None,
            shared_file: None,
            shared_generation: 0,
            edit_document: None,
            restore_edit_document: None,
            restore_edit_selection: None,
            restore_picked: None,
            restore_comment_selection: None,
            restore_list_anchors: None,
            restore_linked_pick: None,
            edit_refresh: None,
            edit_selection: None,
            armed_label: None,
            source_stamp: None,
            shown_identity: None,
            pending_identity: None,
            fresh_open_candidate: None,
            remote: None,
            remote_warning: None,
            remote_loading: None,
            file_id: 0,
            cam: Camera::default(),
            follow_target: None,
            pb: Playback::new(0, 0, 100.0),
            selected: BTreeSet::new(),
            selection_order: Vec::new(),
            primary: None,
            picked: None,
            comment_selection: Default::default(),
            comment_marker_draft: false,
            plate: None,
            range: None,
            comments: CommentState::default(),
            fresh: false,
            restore_range: None,
            comments_rx: None,
            labels: LabelsState::default(),
            tracks: Default::default(),
            devices: DevicesState::default(),
            filters: Arc::default(),
            lanes: LaneCache::default(),
            plate_lanes: PlateCache::default(),
            heat: HeatCache::default(),
            loading: None,
            waiting_shared: None,
            analog_rx: None,
            lazy_job_rx: None,
            lazy_job: None,
            emg_rx: None,
            emg_requested: false,
            emg_failure: None,
            analog_fallback: None,
            analog_queue: Vec::new(),
            analog_merge: None,
            load_cancel: Arc::new(AtomicBool::new(false)),
            restore: None,
            open_started: None,
            load_steps: Vec::new(),
            markers_ms: None,
            analog_ms: None,
            fit_pending: false,
            scene_rect: egui::Rect::NOTHING,
            graph_rect: None,
            plate_rect: None,
            overlay: None,
            overlay_load: None,
            shown: [true; 2],
            overlay_ms: None,
            #[cfg(test)]
            held: None,
        }
    }
}

impl View {
    fn stamp_identity(stamp: Option<&SourceStamp>) -> Option<PathBuf> {
        stamp?.lock().ok()?.as_ref().map(|(path, _, _)| path.clone())
    }

    /// The identity of this view's already admitted source. This lookup never touches disk.
    /// A replacement keeps the old displayed identity separate from the incoming one.
    pub fn bound_identity(&self, path: &Path) -> Option<PathBuf> {
        if self.loading.as_ref().is_some_and(|(target, _)| target == path)
            || self.waiting_shared.as_deref() == Some(path) {
            return self.pending_identity.clone().or_else(|| Self::stamp_identity(self.source_stamp.as_ref()));
        }
        if self.path() == Some(path) {
            return if self.loading.is_some() || self.waiting_shared.is_some() {
                self.shown_identity.clone()
            } else {
                Self::stamp_identity(self.source_stamp.as_ref()).or_else(|| self.shown_identity.clone())
            };
        }
        if let Some(shared) = &self.shared_file {
            if let Ok(state) = shared.0.lock() {
                if state.data.take.path == path {
                    return Self::stamp_identity(state.source_stamp.as_ref());
                }
            }
        }
        None
    }

    pub fn overlay_bound_identity(&self, path: &Path) -> Option<PathBuf> {
        if let Some(load) = &self.overlay_load {
            if load.path == path {
                return load.identity.clone().or_else(|| load.source.as_ref().and_then(|view| view.bound_identity(path)));
            }
        }
        self.overlay.as_ref().filter(|overlay| overlay.path == path)
            .and_then(|overlay| overlay.owner.bound_identity(path))
    }

    pub fn set_pending_identity(&mut self, identity: PathBuf) { self.pending_identity = Some(identity); }

    pub fn set_overlay_pending_identity(&mut self, identity: PathBuf) {
        if let Some(load) = &mut self.overlay_load { load.identity = Some(identity); }
    }

    /// Candidate for an ordinary disk open after its final live reference was closed.
    /// The controller separately excludes app-owned pending edits/saves.
    pub fn retained_document(&self) -> Option<RetainedDocument> {
        if self.remote.is_some() || self.remote_loading.is_some() || self.loading.is_some()
            || self.waiting_shared.is_some() || self.edit_refresh.is_some() || self.marker_dirty() {
            return None;
        }
        Some(RetainedDocument::new(self.edit_document.clone()?, self.source_stamp.clone()))
    }

    pub fn fresh_open_document(&self) -> Option<EditSession> {
        self.fresh_open_candidate.as_ref().map(|candidate| candidate.document.clone())
    }

    pub fn fresh_open_source_stamp(&self) -> Option<SourceStamp> {
        self.fresh_open_candidate.as_ref().and_then(|candidate| candidate.source_stamp.clone())
    }

    /// Set only after starting the new load: every new load/cancellation clears an old candidate.
    pub fn set_fresh_open_candidate(&mut self, candidate: Option<RetainedDocument>) {
        drop_later(std::mem::replace(&mut self.fresh_open_candidate, candidate));
    }

    pub fn set_overlay_fresh_open_candidate(&mut self, candidate: Option<RetainedDocument>) {
        if let Some(load) = self.overlay_load.as_mut() {
            let source = load.source.get_or_insert_with(|| Box::new(View::default()));
            source.set_fresh_open_candidate(candidate);
        }
    }

    /// Return the canonical file handle when another view needs this file. Ordinary one-view
    /// opens keep the zero-copy analog path and allocate no shared-state wrapper.
    pub fn shared_file(&mut self) -> Option<SharedFile> {
        if self.loading.is_some() || self.remote_loading.is_some()
            || self.waiting_shared.is_some() { return None; }
        if self.shared_file.is_none() {
            self.shared_file = Some(SharedFile(Arc::new(Mutex::new(SharedFileState {
                generation: 0,
                filters: self.filters.clone(),
                data: self.data.clone()?,
                document: self.edit_document.clone()?,
                source_stamp: self.source_stamp.clone(),
                remote: self.remote.clone(),
                comments: SharedComments::capture(&self.comments),
                emg_demand: false,
                emg_wake: None,
                editor_wake: None,
            }))));
            self.shared_generation = 0;
        }
        self.sync_shared();
        self.shared_file.clone()
    }

    pub fn shares_document(&self, other: &View) -> bool {
        self.edit_document.as_ref().zip(other.edit_document.as_ref())
            .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
    }

    pub fn document_id(&self) -> Option<DocumentId> {
        self.edit_document.as_ref()?.lock().ok().map(|doc| doc.id())
    }

    /// Current canonical local target, including a save/rename not yet consumed by a hidden
    /// view. Loading and remote addresses retain the same precedence as `target()`.
    pub fn shared_target(&self) -> Option<PathBuf> {
        if self.loading.is_some() || self.remote_loading.is_some() || self.remote.is_some()
            || self.waiting_shared.is_some() {
            return self.target().map(Path::to_path_buf);
        }
        self.shared_file.as_ref().and_then(|shared| shared.0.lock().ok()
            .map(|state| state.data.take.path.clone()))
            .or_else(|| self.target().map(Path::to_path_buf))
    }

    /// Open an already loaded file without disk work. Playback, camera, selection, ID pools
    /// and controls stay independent. Pending reads remain owned by the original view.
    pub fn show_shared_file(&mut self, shared: SharedFile, file_id: usize, speed: f64, now: f64) -> Option<Arc<ViewData>> {
        let (data, document, stamp, comments, remote, generation) = {
            let state = shared.0.lock().ok()?;
            (state.data.clone(), state.document.clone(), state.source_stamp.clone(), state.comments.clone(), state.remote.clone(), state.generation)
        };
        let restore = self.restore.take();
        let restore_range = self.restore_range.take();
        self.cancel_load();
        self.restore = restore;
        self.restore_range = restore_range;
        self.restore_edit_document = Some(document);
        self.source_stamp = stamp;
        self.shown_identity = Self::stamp_identity(self.source_stamp.as_ref());
        let (_tx, rx) = channel();
        let old = self.show_shared(data, rx, file_id, speed, now);
        self.filters = shared.0.lock().unwrap().filters.clone();
        // A follower never invents an analog receiver or reloads the Comment tail.
        self.analog_rx = None;
        self.comments_rx = None;
        comments.install(&mut self.comments);
        if remote.is_some() {self.remote=remote;}
        self.shared_file = Some(shared);
        self.shared_generation = generation;
        // The canonical data already represents this document's current revision.
        if self.data.as_ref().is_some_and(|data| data.edit_revision == self.edit_refresh) {
            self.edit_refresh = None;
        }
        old
    }

    /// Reopening a retained view must join the file already open now, while keeping its own
    /// camera, frame, selected UIDs, range, picked Comment, overlays and read-only metadata.
    /// False leaves the view untouched: an independent unsaved document or unfinished open
    /// cannot be discarded by a layout restore. App-owned marker/save jobs are guarded by
    /// the controller before calling this method.
    /// The same readiness test used by adoption, without changing this retained view.
    pub fn can_adopt_shared_preserving_state(&self, shared: &SharedFile) -> bool {
        let (document, path) = {
            let Ok(state) = shared.0.lock() else { return false };
            (state.document.clone(), state.data.take.path.clone())
        };
        self.can_adopt_document(&document, &path)
    }

    /// Read-only counterpart of `source.shared_file()` followed by the adoption check.
    /// A single-view source need not allocate or install a shared wrapper for preflight.
    pub fn can_adopt_from_view(&self, source: &View) -> Option<bool> {
        if source.loading.is_some() || source.remote_loading.is_some() || source.waiting_shared.is_some() { return None; }
        if let Some(shared) = &source.shared_file { return Some(self.can_adopt_shared_preserving_state(shared)); }
        Some(self.can_adopt_document(source.edit_document.as_ref()?, &source.data.as_ref()?.take.path))
    }

    fn can_adopt_document(&self, document: &EditSession, path: &Path) -> bool {
        if self.edit_document.as_ref().is_some_and(|own| Arc::ptr_eq(own, document)) { return true; }
        self.loading.is_none() && self.remote_loading.is_none() && self.waiting_shared.is_none()
            && !self.marker_dirty() && self.comments.pending == 0 && self.comments.failed.is_none()
            && self.path().is_some_and(|own| same_file_path(own, path))
    }

    pub fn adopt_shared_preserving_state(&mut self, shared: SharedFile, now: f64) -> bool {
        let (data, document, stamp, comments, remote, generation) = {
            let Ok(state) = shared.0.lock() else { return false };
            (state.data.clone(), state.document.clone(), state.source_stamp.clone(), state.comments.clone(), state.remote.clone(), state.generation)
        };
        if !self.can_adopt_document(&document, &data.take.path) { return false; }
        if self.edit_document.as_ref().is_some_and(|own| Arc::ptr_eq(own, &document)) {
            self.sync_shared_file(&shared);
            return true;
        }
        let selection = self.uid_selection();
        let frame = self.pb.frame;
        let plate_name = self.plate.and_then(|index| self.data.as_ref()?.plates.get(index).map(|plate| plate.name.clone()));
        self.cancel_load();
        let filters=shared.0.lock().unwrap().filters.clone();
        drop_later(Some(std::mem::replace(&mut self.filters,filters)));
        drop_later(self.edit_document.replace(document));
        self.edit_refresh = None;
        self.edit_selection = Some(selection);
        self.install_shared_data(data);
        self.source_stamp = stamp;
        self.shown_identity = Self::stamp_identity(self.source_stamp.as_ref());
        comments.install(&mut self.comments);
        if remote.is_some() {self.remote=remote;}
        if let Some(data) = self.data.as_ref() {
            self.pb.first = data.start_frame;
            self.pb.last = data.end_frame.max(data.start_frame);
            self.pb.rate = data.take.frame_rate;
            self.plate = plate_name.and_then(|name| data.plates.iter().position(|plate| plate.name == name));
        }
        self.pb.set_frame(frame, now);
        if let Some((a, b)) = self.range {
            self.range = Some(panels::ordered_range(a, b, self.pb.first, self.pb.last));
        }
        drop_later(self.shared_file.replace(shared));
        self.shared_generation = generation;
        true
    }

    /// Publish after a file mutation (Comment change/ack, analog arrival, marker rebuild or
    /// save). A stale view cannot replace a newer publication: it consumes that publication
    /// instead and returns false. The app must sync before accepting input on an old tab.
    pub fn publish_shared_file(&mut self) -> bool {
        let _span=crate::bench_timing::span("shared.publish");
        self.editor_revision = self.editor_revision.wrapping_add(1);
        let Some(shared) = self.shared_file.clone() else { return true };
        let Some(data) = self.data.as_ref() else { return false };
        let Some(document) = self.edit_document.as_ref() else { return false };
        let Ok(mut state) = shared.0.lock() else { return false };
        if !Arc::ptr_eq(document, &state.document) { return false; }
        if self.shared_generation != state.generation {
            drop(state);
            self.sync_shared_file(&shared);
            return false;
        }
        let data_changed = !Arc::ptr_eq(data, &state.data);
        let comments_changed = !state.comments.matches(&self.comments);
        if data_changed || comments_changed || state.remote!=self.remote {
            if data_changed { drop_later(Some(std::mem::replace(&mut state.data, data.clone()))); }
            if !data.emg_pending { state.emg_demand = false; }
            if comments_changed { state.comments = SharedComments::capture(&self.comments); }
            state.source_stamp = self.source_stamp.clone();
            state.remote=self.remote.clone();
            state.generation = state.generation.wrapping_add(1);
        }
        if let Some((document, wake)) = &state.editor_wake { let _ = wake.send(*document); }
        self.shared_generation = state.generation;
        true
    }

    /// Consume the latest shared publication, preserving this view's frame/camera and marker
    /// selection by UID. The handle always reads live state, so an old handle cannot roll back
    /// a save, rename, Comment write, or marker edit.
    pub fn sync_shared_file(&mut self, shared: &SharedFile) -> bool {
        let _span=crate::bench_timing::span("shared.sync");
        let (data, stamp, comments, remote, generation) = {
            let Ok(state) = shared.0.lock() else { return false };
            if !self.edit_document.as_ref().is_some_and(|doc| Arc::ptr_eq(doc, &state.document)) { return false; }
            if !Arc::ptr_eq(&self.filters,&state.filters) {drop_later(Some(std::mem::replace(&mut self.filters,state.filters.clone())));}
            if self.shared_file.as_ref().is_some_and(|own| Arc::ptr_eq(&own.0, &shared.0))
                && self.shared_generation == state.generation { return false; }
            (state.data.clone(), state.source_stamp.clone(), state.comments.clone(), state.remote.clone(), state.generation)
        };
        if !self.data.as_ref().is_some_and(|current| Arc::ptr_eq(current, &data)) {
            self.install_shared_data(data);
        }
        comments.install(&mut self.comments);
        if remote.is_some() {self.remote=remote;}
        let ids: Vec<_> = self.comments.list.list().iter().map(|comment| comment.id).collect();
        self.comment_selection.prune(&ids);
        if self.picked.is_some_and(|id| !ids.contains(&id)) {
            let selection = self.comment_selection.clone();
            self.picked = selection.representative;
            if let Some(id) = self.picked { self.select_comment(id); }
            self.comment_selection = selection;
        }
        self.source_stamp = stamp;
        self.shown_identity = Self::stamp_identity(self.source_stamp.as_ref());
        drop_later(self.shared_file.replace(shared.clone()));
        self.shared_generation = generation;
        true
    }

    pub fn sync_shared(&mut self) -> bool {
        let primary = self.shared_file.clone().is_some_and(|shared| self.sync_shared_file(&shared));
        let overlay = self.overlay.as_mut().is_some_and(Overlay::sync_shared);
        if overlay { drop_later(Some(std::mem::take(&mut self.lanes))); }
        primary || overlay
    }

    /// File workers only, independent of whether this view is drawn. An idle hidden view needs
    /// no polling; a lazy EMG owner wakes when any sibling explicitly requests its channels.
    pub fn file_work_pending(&self) -> bool {
        #[cfg(test)] self.background_work_checks.set(self.background_work_checks.get() + 1);
        self.remote_loading.is_some() || self.loading.is_some() || self.analog_work_pending()
            || self.comments_rx.is_some() || self.comments.id_rx.is_some() || self.lazy_job_rx.is_some()
            || self.overlay.as_ref().is_some_and(|overlay| overlay.owner.file_work_pending())
            || (self.lazy_job.is_some() && self.shared_file.as_ref()
                .is_some_and(|shared| shared.0.lock().ok().is_some_and(|state| state.emg_demand)))
    }

    /// Finish an ID reservation that began while visible without launching another one.
    pub fn poll_started_comment_ids(&mut self) {
        let Some(rx) = self.comments.id_rx.as_ref() else { return };
        match rx.try_recv() {
            Ok(Ok(ids)) => { self.comments.id_pool = Some(ids); self.comments.id_error = None; self.comments.id_rx = None; }
            Ok(Err(reason)) => { self.comments.id_error = Some(reason); self.comments.id_rx = None; }
            Err(TryRecvError::Disconnected) => {
                self.comments.id_error = Some("Comment ID preparation stopped.".into()); self.comments.id_rx = None;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Hidden OPEN EDITORS rows subscribe only after this file is actually shared.
    pub fn subscribe_editor_updates(&self, wake: &Sender<DocumentId>) {
        let Some(document) = self.document_id() else { return };
        let Some(shared) = self.shared_file.as_ref() else { return };
        if let Ok(mut state) = shared.0.lock() { state.editor_wake = Some((document, wake.clone())); };
    }

    pub fn notify_editor_updates(&mut self) {
        self.editor_revision = self.editor_revision.wrapping_add(1);
        if let Some(shared) = &self.shared_file {
            if let Ok(state) = shared.0.lock() {
                if let Some((document, wake)) = &state.editor_wake { let _ = wake.send(*document); }
            }
        }
    }

    /// Subscribe the owning app while this view has a deferred reader. Setting demand and
    /// sending its wake happen under the same shared lock. A late subscription replays an
    /// existing demand, so hiding or handing off the owner cannot lose a channel request.
    pub fn subscribe_emg_work(&mut self, wake: &Sender<DocumentId>) {
        if self.lazy_job.is_none() && self.lazy_job_rx.is_none() { return; }
        let Some(document) = self.document_id() else { return };
        let Some(shared) = self.shared_file() else { return };
        if let Ok(mut state) = shared.0.lock() {
            state.emg_wake = Some((document, wake.clone()));
            if state.emg_demand { let _ = wake.send(document); }
        };
    }

    pub fn owns_shared_file_work(&self) -> bool {
        self.analog_rx.is_some() || self.lazy_job_rx.is_some() || self.lazy_job.is_some()
            || self.emg_rx.is_some() || self.analog_merge.is_some() || !self.analog_queue.is_empty()
            || self.comments_rx.is_some()
    }

    /// Before closing a load owner that has another view of this file, move its outstanding
    /// reads and deferred EMG job there. No file is read again and clearing the old view cannot
    /// cancel the transferred worker. Marker/save jobs live in MarkerUi and move with it.
    pub fn transfer_shared_loads(&mut self, target: &mut View) -> bool {
        if !self.shares_document(target) { return false; }
        self.publish_shared_file();
        if let Some(shared) = self.shared_file.clone() { target.sync_shared_file(&shared); }
        if self.analog_rx.is_none() && self.lazy_job_rx.is_none() && self.lazy_job.is_none()
            && self.emg_rx.is_none() && self.analog_merge.is_none() && self.analog_queue.is_empty()
            && self.comments_rx.is_none() { return false; }
        // The app gives reads one owner; refuse merging independently running workers.
        if target.analog_rx.is_some() || target.lazy_job_rx.is_some() || target.lazy_job.is_some()
            || target.emg_rx.is_some() || target.analog_merge.is_some() || !target.analog_queue.is_empty()
            || target.comments_rx.is_some() { return false; }
        target.analog_rx = self.analog_rx.take().map(|(_, rx)| (target.file_id, rx));
        target.emg_rx = self.emg_rx.take().map(|(_, rx)| (target.file_id, rx));
        target.lazy_job_rx = self.lazy_job_rx.take();
        target.lazy_job = self.lazy_job.take();
        target.analog_merge = self.analog_merge.take();
        target.analog_queue = std::mem::take(&mut self.analog_queue);
        target.comments_rx = self.comments_rx.take();
        target.load_cancel = std::mem::replace(&mut self.load_cancel, Arc::new(AtomicBool::new(false)));
        target.emg_requested = self.emg_requested;
        target.emg_failure = self.emg_failure.clone();
        target.analog_fallback = self.analog_fallback.clone();
        true
    }

    pub fn marker_dirty(&self) -> bool {
        self.edit_document.as_ref().is_some_and(|session| session.lock().map_or(true, |doc| doc.is_dirty()))
    }

    /// The file on screen.
    pub fn path(&self) -> Option<&Path> {
        self.data.as_ref().map(|d| d.take.path.as_path())
    }

    /// The file being read, else the file on screen.
    pub fn target(&self) -> Option<&Path> {
        self.remote_loading.as_ref().map(|(_, _, p)| p.as_path()).or(self.waiting_shared.as_deref())
            .or_else(|| self.loading.as_ref().map(|(p, _)| p.as_path())).or_else(|| self.path())
    }

    /// Wait for the app's existing reader of `path`; the currently displayed file remains
    /// intact until show_shared_file succeeds. The app resolves or clears waiters by event.
    pub fn wait_for_shared(&mut self, path: PathBuf, restore: Option<Spot>) {
        if self.data.is_some() && self.loading.is_none() && self.waiting_shared.is_none() {
            self.shown_identity = Self::stamp_identity(self.source_stamp.as_ref()).or(self.shown_identity.take());
        }
        self.cancel_load();
        self.waiting_shared = Some(path);
        self.restore = restore;
        self.open_started = Some(Instant::now());
        self.markers_ms = None;
        self.analog_ms = None;
    }

    pub fn waiting_shared_path(&self) -> Option<&Path> { self.waiting_shared.as_deref() }

    /// The owner failed or closed before publishing. Keep the previous visible file.
    pub fn clear_shared_wait(&mut self) -> Option<PathBuf> {
        let path = self.waiting_shared.take()?;
        self.pending_identity = None;
        self.restore = None;
        self.restore_range = None;
        Some(path)
    }

    /// Markers, the comment file, force plates or the overlay's markers still coming.
    pub fn busy(&self) -> bool {
        self.remote_loading.is_some() || self.loading.is_some() || self.waiting_shared.is_some() || self.analog_rx.is_some() || self.emg_rx.is_some() || self.analog_merge.is_some() || !self.analog_queue.is_empty() || self.comments_rx.is_some() || self.overlay_load.is_some() || self.devices.graphs_pending()
            || self.overlay.as_ref().is_some_and(|overlay| overlay.owner.busy())
    }

    /// The laid-over file on screen.
    pub fn overlay_path(&self) -> Option<&Path> {
        self.overlay.as_ref().map(|o| o.path.as_path())
    }

    pub fn overlay_shared_target(&self) -> Option<PathBuf> {
        self.overlay.as_ref().and_then(|overlay| overlay.owner.shared_target())
    }

    pub fn overlay_owner(&self) -> Option<&View> { self.overlay.as_ref().map(Overlay::owner) }

    pub fn overlay_owner_mut(&mut self) -> Option<&mut View> { self.overlay.as_mut().map(Overlay::owner_mut) }

    /// The laid-over file being read, else the one on screen (what counts for "at most 4 files").
    pub fn overlay_target(&self) -> Option<&Path> {
        self.overlay_load.as_ref().map(|l| l.path.as_path()).or_else(|| self.overlay_path())
    }

    /// Both laid-over files of this view (on screen and being read): no file twice.
    pub fn overlay_paths(&self) -> impl Iterator<Item = &Path> {
        self.overlay_path().into_iter().chain(self.overlay_load.as_ref().map(|l| l.path.as_path()))
    }

    /// A ready overlay also supplies the canonical file identity for ordinary view opens.
    pub fn shared_overlay_file(&mut self, path: &Path) -> Option<SharedFile> {
        let overlay = self.overlay.as_mut()?;
        overlay.sync_shared();
        if overlay.path != path && !same_file_path(&overlay.path, path) { return None; }
        overlay.shared_file()
    }

    /// The actual reader, excluding a follower which is waiting for this file elsewhere.
    pub fn overlay_reading_path(&self) -> Option<&Path> {
        self.overlay_load.as_ref().filter(|load| !load.waiting_shared && load.shared.is_none()).map(|load| load.path.as_path())
    }

    pub fn overlay_waiting_shared_path(&self) -> Option<&Path> {
        self.overlay_load.as_ref().filter(|load| load.waiting_shared).map(|load| load.path.as_path())
    }

    pub fn attach_overlay_shared(&mut self, path: &Path, shared: SharedFile) -> bool {
        let Some(load) = self.overlay_load.as_mut().filter(|load| load.waiting_shared && (load.path == path || same_file_path(&load.path, path))) else { return false };
        load.shared = Some(shared);
        load.waiting_shared = false;
        true
    }

    pub fn start_shared_overlay(&mut self, path: PathBuf, shared: SharedFile, quiet: bool) {
        self.wait_overlay_shared(path, quiet);
        let load = self.overlay_load.as_mut().expect("created shared overlay wait");
        load.shared = Some(shared);
        load.waiting_shared = false;
    }

    pub fn wait_overlay_shared(&mut self, path: PathBuf, quiet: bool) {
        self.cancel_overlay_load();
        let (_tx, rx) = channel();
        self.overlay_load = Some(OverlayLoad {
            path, identity: None, rx, quiet, started: Instant::now(), source: None, shared: None, waiting_shared: true,
            #[cfg(test)] held: None,
        });
    }

    pub fn cancel_overlay_load(&mut self) {
        if let Some(mut load) = self.overlay_load.take() {
            if let Some(source) = &mut load.source { source.cancel_load(); }
            drop_later(Some(load));
        }
    }

    /// Markers appear first; preserve the one reader's forceplate, Comment and lazy EMG jobs
    /// so opening this same file in a primary view never requires another full marker read.
    pub fn start_overlay(&mut self, ctx: &egui::Context, path: PathBuf, quiet: bool) {
        self.cancel_overlay_load();
        let mut source = Box::new(View::default());
        source.start_load(ctx, path.clone(), None);
        let (_, rx) = source.loading.take().expect("overlay reader started");
        self.overlay_load = Some(OverlayLoad {
            path,
            identity: None,
            rx,
            quiet,
            started: Instant::now(),
            source: Some(source), shared: None, waiting_shared: false,
            #[cfg(test)]
            held: None,
        });
    }

    /// Tests: like `start_overlay`, but the test sends the markers through `held`.
    #[cfg(test)]
    pub fn hold_overlay(&mut self, path: PathBuf, quiet: bool) {
        self.cancel_overlay_load();
        let (tx, rx) = channel();
        self.overlay_load = Some(OverlayLoad { path, identity: None, rx, quiet, started: Instant::now(), held: Some(tx), source: None, shared: None, waiting_shared: false });
    }

    /// The overlay's markers, once they are in and this view shows its own file (not while
    /// that is being read: the two are compared on arrival). (file, data or why not, quiet).
    pub fn poll_overlay(&mut self) -> Option<(PathBuf, Result<Overlay, String>, bool)> {
        if self.loading.is_some() || self.waiting_shared.is_some() || self.data.is_none() {
            return None;
        }
        let pending = self.overlay_load.as_ref()?;
        if pending.waiting_shared { return None; }
        if let Some(shared) = pending.shared.clone() {
            let load = self.overlay_load.take()?;
            let result = Overlay::from_shared(load.path.clone(), shared);
            if result.is_ok() { self.overlay_ms = Some(ms_since(load.started)); }
            return Some((load.path, result, load.quiet));
        }
        let msg = match pending.rx.try_recv() {
            Ok(LoadMsg::Markers(r)) => r.map(|(d, _)| d),
            Ok(LoadMsg::Analog(..)) | Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("읽기가 중간에 끝났어요".to_owned()),
        };
        let mut load = self.overlay_load.take()?;
        if msg.is_ok() {
            self.overlay_ms = Some(ms_since(load.started));
        }
        let path = load.path;
        let mut source = load.source.take().unwrap_or_else(|| Box::new(View::default()));
        if let Some(identity) = load.identity.take() { source.set_pending_identity(identity); }
        let result = match msg {
            Ok(data) => Ok(Overlay::from_reader(path.clone(), data, load.rx, source)),
            Err(error) => {
                source.cancel_load();
                drop_later(Some((source, load.rx)));
                Err(error)
            }
        };
        Some((path, result, load.quiet))
    }

    /// Take the overlay off (and stop reading one): both files shown again. Returns its owner,
    /// to be freed off the drawing thread.
    pub fn take_overlay(&mut self) -> Option<Overlay> {
        self.cancel_overlay_load();
        self.shown = [true; 2];
        self.overlay.take()
    }

    /// Handoff before removing this overlay. A primary view or overlay sharing its document
    /// receives the only running file workers. Marker edit/save jobs remain app-owned.
    pub fn transfer_overlay_work_to(&mut self, target: &mut View) -> bool {
        self.overlay.as_mut().is_some_and(|overlay| overlay.transfer_work_to(target))
    }

    /// A surviving overlay can keep file readers alive when its final primary view closes.
    /// The caller still moves MarkerUi jobs separately or guards that close until they end.
    pub fn transfer_primary_work_to_overlay(&mut self, target: &mut View) -> bool {
        let Some(overlay) = target.overlay.as_mut() else { return false };
        if !self.shares_document(&overlay.owner) { return false; }
        !self.owns_shared_file_work() || self.transfer_shared_loads(&mut overlay.owner)
    }

    pub fn overlay_document_id(&self) -> Option<DocumentId> { self.overlay.as_ref()?.document_id() }

    pub fn overlay_marker_dirty(&self) -> bool { self.overlay.as_ref().is_some_and(Overlay::marker_dirty) }

    pub fn overlay_analog_work_pending(&self) -> bool {
        self.overlay.as_ref().is_some_and(|overlay| overlay.owner.analog_work_pending()
            || overlay.owner.data.as_ref().is_some_and(|data| data.analog_pending))
    }

    /// Poll already-running jobs even during a save, but only start EMG with the
    /// controller's document-wide permission. Save workers wait for those existing jobs.
    pub fn poll_overlay_file_work(&mut self, ctx: &egui::Context) {
        self.poll_overlay_file_work_gated(ctx, false);
    }

    pub fn poll_overlay_file_work_gated(&mut self, ctx: &egui::Context, allow_emg: bool) {
        let changed = if let Some(overlay) = self.overlay.as_mut() {
            if overlay.owner.file_work_pending() {
                overlay.owner.poll_started_comment_ids();
                overlay.owner.poll_comments();
                overlay.owner.poll_analog();
                if allow_emg { overlay.owner.request_emg(ctx); }
            }
            overlay.sync_shared()
        } else { false };
        if changed { drop_later(Some(std::mem::take(&mut self.lanes))); }
    }

    /// The overlay's frame shown with this view's frame: the same place from the first frame
    /// (the same number when both start alike, as two versions of one recording do).
    pub fn overlay_frame(&self) -> Option<i64> {
        let (d, o) = (self.data.as_ref()?, self.overlay.as_ref()?);
        Some(self.pb.frame - d.start_frame + o.data.start_frame)
    }

    /// Read a file on a background thread: markers first, then its comment file (small), then
    /// force plates / EMG once no file's markers are loading any more. A load this view started
    /// before is cancelled.
    pub fn start_load(&mut self, ctx: &egui::Context, path: PathBuf, restore: Option<Spot>) {
        if self.data.is_some() && self.loading.is_none() && self.waiting_shared.is_none() {
            self.shown_identity = Self::stamp_identity(self.source_stamp.as_ref()).or(self.shown_identity.take());
        }
        self.waiting_shared = None;
        self.set_fresh_open_candidate(None);
        drop_later(self.restore_edit_document.take());
        self.restore_edit_selection = None;
        self.restore_picked = None;
        self.restore_comment_selection = None;
        self.restore_list_anchors = None;
        self.restore_linked_pick = None;
        let (tx, rx) = channel();
        let (comments_tx, comments_rx) = channel();
        let (lazy_tx, lazy_rx) = channel();
        drop_later(self.lazy_job.take());
        drop_later(self.lazy_job_rx.replace(lazy_rx));
        drop_later(self.emg_rx.take());
        self.emg_requested = false;
        self.emg_failure = None;
        self.analog_fallback = None;
        drop_later(Some(std::mem::take(&mut self.analog_queue)));
        drop_later(self.analog_merge.take());
        let ctx2 = ctx.clone();
        let p = path.clone();
        let stamp: SourceStamp = Arc::new(Mutex::new(None));
        self.source_stamp = Some(stamp.clone());
        self.load_cancel.store(true, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        self.load_cancel = cancel.clone();
        MARKER_LOADS.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            let panic_msg = || "읽는 중 내부 오류(panic)가 났어요.".to_owned();
            let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Ok(identity) = tak_core::takm::canonical_target(&p) {
                    if let Ok(meta) = std::fs::metadata(&identity) {
                        if let Ok(mut initial) = stamp.lock() {
                            *initial = Some((identity, meta.len(), meta.modified().ok()));
                        }
                    }
                }
                let fast = tak_core::tak::fast_read::read_take_fast(&p, false).map_err(|e| e.to_string())?;
                let t = Instant::now();
                let mapping = fast.analog.as_ref().and_then(|j| j.plate_devices()).map(<[usize]>::to_vec);
                let fallback = fast.analog.as_ref().and_then(|j| j.fallback_reason()).map(str::to_owned)
                    .or_else(|| fast.times.iter().any(|(stage, _)| *stage == "full read (fallback)")
                        .then(|| "파일 구조를 안전하게 나누어 읽을 수 없어요".to_owned()));
                let (initial, lazy) = fast.analog.map(|job| { let (first, lazy) = job.split(); (Some(first), lazy) }).unwrap_or((None, None));
                let mut data = ViewData::build_with_plate_devices(fast.take, mapping);
                data.analog_pending = initial.is_some();
                data.emg_pending = lazy.is_some();
                if let Some(reason) = &fallback { data.take.warnings.push(format!("장치를 확실히 나누지 못해 아날로그를 모두 읽어요: {reason}")); }
                let _ = lazy_tx.send((lazy, fallback));
                let mut steps = fast.times;
                steps.push(("view", ms_since(t)));
                Ok((data, steps, initial))
            }))
            .unwrap_or_else(|_| Err(panic_msg()));
            MARKER_LOADS.fetch_sub(1, Ordering::SeqCst);
            let (msg, job) = match read {
                Ok((data, steps, job)) => {
                    let job = job.map(|j| (j, data.analog_target()));
                    (LoadMsg::Markers(Ok((data, steps))), job)
                }
                Err(e) => (LoadMsg::Markers(Err(e)), None),
            };
            let markers_ok = matches!(msg, LoadMsg::Markers(Ok(_)));
            let sent = tx.send(msg).is_ok();
            ctx2.request_repaint();
            if !sent || !markers_ok {
                return;
            }
            // The markers are on their way to the screen; now the comment file (none = nothing
            // to read). It waits for nothing else, so `--screenshot` shows the blocks too.
            let load = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| CommentsLoad::read(&p)))
                .unwrap_or_else(|_| CommentsLoad { target: Err(panic_msg()), parsed: None, label: None });
            let _ = comments_tx.send(load);
            ctx2.request_repaint();
            let Some((job, target)) = job else { return };
            while MARKER_LOADS.load(Ordering::SeqCst) > 0 && !cancel.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            let t = Instant::now();
            let analog = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let indices = job.device_indices().to_vec();
                let selective = job.plate_devices().is_some();
                job.run().map(|(analog, warning)| if selective { AnalogPart::prepare_selected(analog, warning, &target, indices, true) } else { AnalogPart::prepare(analog, warning, &target) }).map_err(|e| e.to_string())
            }))
            .unwrap_or_else(|_| Err(panic_msg()));
            let _ = tx.send(LoadMsg::Analog(analog, ms_since(t)));
            ctx2.request_repaint();
        });
        self.loading = Some((path, rx));
        self.comments_rx = Some(comments_rx);
        self.restore = restore;
        self.restore_range = None;
        self.open_started = Some(Instant::now());
        self.markers_ms = None;
        self.analog_ms = None;
    }

    /// Tests: like `start_load`, but no thread reads the file: the test sends the markers
    /// through `held` when it wants them to arrive.
    #[cfg(test)]
    pub fn hold_load(&mut self, path: PathBuf, restore: Option<Spot>) {
        self.cancel_load();
        let (tx, rx) = channel();
        self.held = Some(tx);
        self.loading = Some((path, rx));
        self.restore = restore;
    }

    /// Stop reading (the view closes or shows another file).
    pub fn cancel_load(&mut self) {
        self.waiting_shared = None;
        self.pending_identity = None;
        self.set_fresh_open_candidate(None);
        self.load_cancel.store(true, Ordering::SeqCst);
        drop_later(self.restore_edit_document.take());
        self.restore_edit_selection = None;
        self.restore_picked = None;
        self.restore_comment_selection = None;
        self.restore_list_anchors = None;
        self.restore_linked_pick = None;
        self.remote_loading = None;
        drop_later(self.loading.take());
        drop_later(self.analog_rx.take());
        drop_later(self.lazy_job_rx.take());
        drop_later(self.lazy_job.take());
        drop_later(self.emg_rx.take());
        drop_later(self.analog_merge.take());
        drop_later(Some(std::mem::take(&mut self.analog_queue)));
        self.emg_requested = false;
        self.emg_failure = None;
        self.analog_fallback = None;
        self.comments_rx = None;
        self.restore = None;
        self.restore_range = None;
    }

    /// The markers of the file being read, once.
    pub fn poll_markers(&mut self) -> Option<Loaded> {
        let msg = match self.loading.as_ref()?.1.try_recv() {
            Ok(LoadMsg::Markers(r)) => r,
            Ok(LoadMsg::Analog(..)) | Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("읽기가 중간에 끝났어요".to_owned()),
        };
        let (path, rx) = self.loading.take()?;
        if msg.is_err() { self.set_fresh_open_candidate(None); }
        self.markers_ms = self.open_started.map(ms_since);
        Some(Loaded { path, result: msg, rx })
    }

    /// Show a freshly read file: new playback, no selection, the remembered spot if any (else
    /// framed). Returns the previous data, to be freed off the drawing thread.
    pub fn show(&mut self, data: ViewData, rx: Receiver<LoadMsg>, file_id: usize, speed: f64, now: f64) -> Option<Arc<ViewData>> {
        self.show_shared(Arc::new(data), rx, file_id, speed, now)
    }

    pub fn show_shared(&mut self, data: Arc<ViewData>, rx: Receiver<LoadMsg>, file_id: usize, speed: f64, now: f64) -> Option<Arc<ViewData>> {
        self.shown_identity = Self::stamp_identity(self.source_stamp.as_ref()).or(self.pending_identity.take());
        self.pending_identity = None;
        let mut verified_restore = false;
        if let Some(candidate) = self.fresh_open_candidate.take() {
            let current = self.source_stamp.as_ref().and_then(|stamp| stamp.lock().ok().and_then(|stamp| stamp.clone()));
            // Unknown timestamps cannot prove freshness. Comparing captured values performs
            // no filesystem work on the drawing thread, including for hidden/overlay loads.
            let unchanged = candidate.stamp.as_ref().zip(current.as_ref()).is_some_and(|(old, new)| {
                old.2.is_some() && new.2.is_some() && old == new
            });
            if unchanged {
                verified_restore = true;
                self.restore_edit_document = Some(candidate.document);
                // Existing undo snapshots keep this handle. Later own saves/Comment appends
                // must advance the same stamp rather than strand those snapshots on an old one.
                self.source_stamp = candidate.source_stamp;
            }
            else { drop_later(Some(candidate)); }
        }
        self.waiting_shared = None;
        self.shared_file = None;
        self.shared_generation = 0;
        self.remote = None;
        self.remote_warning = None;
        self.remote_loading = None;
        drop_later(self.emg_rx.take());
        drop_later(self.analog_merge.take());
        drop_later(Some(std::mem::take(&mut self.analog_queue)));
        self.emg_requested = false;
        self.emg_failure = None;
        let mut pb = Playback::new(data.start_frame, data.end_frame, data.take.frame_rate);
        pb.speed = speed;
        self.pb = pb;
        self.cam.preset = Preset::Perspective;
        self.cam.follow = false;
        self.follow_target = None;
        self.selected.clear();
        self.selection_order.clear();
        self.primary = None;
        self.picked = self.restore_picked.take().flatten();
        self.comment_selection = self.restore_comment_selection.take().unwrap_or_default();
        if self.comment_selection.len() == 0 { if let Some(id) = self.picked { self.comment_selection.single(id); } }
        self.comment_marker_draft = false;
        self.plate = None;
        self.range = None;
        // Its own comments come from the loading thread (`poll_comments`).
        self.comments = CommentState::default();
        self.fresh = true;
        self.file_id = file_id;
        let path = &data.take.path;
        let restored = self.restore_edit_document.take().filter(|session| verified_restore || session.lock().ok().is_some_and(|doc| doc.source_path() == path || doc.save_path() == Some(path.as_path())));
        let restored_selection = if restored.is_some() { self.restore_edit_selection.take() } else { self.restore_edit_selection = None; None };
        let session = restored.unwrap_or_else(|| Arc::new(Mutex::new(EditDocument::unloaded(path.clone()))));
        self.edit_refresh = session.lock().ok().and_then(|doc| {
            (doc.snapshot().is_loaded() && (doc.is_dirty() || doc.save_path().is_some_and(|saved| saved != path))).then(|| doc.revision())
        });
        drop_later(self.edit_document.replace(session));
        self.edit_selection = restored_selection;
        self.armed_label = None;
        drop_later(self.analog_rx.take());
        self.analog_rx = data.analog_pending.then_some((file_id, rx));
        if !data.analog_pending { self.analog_ms = self.markers_ms; }
        self.devices.reset_graphs();
        drop_later(Some(std::mem::replace(&mut self.filters, Arc::default())));
        drop_later(Some(std::mem::take(&mut self.plate_lanes)));
        self.labels.reset(&data, file_id);
        if let Some(anchors) = self.restore_list_anchors.take() { self.labels.list_selection = anchors; }
        if let Some(pick) = self.restore_linked_pick.take() { self.labels.linked_pick = pick; }
        let old = self.data.replace(data);
        match self.restore.take() {
            Some(spot) => self.apply_spot(&spot, now),
            None => self.fit_pending = true,
        }
        if let Some(selection) = self.edit_selection.clone() {
            self.apply_uid_selection(&selection);
            if self.edit_refresh.is_none() { self.edit_selection = None; }
        }
        if let Some(range) = self.restore_range.take() {
            self.range = range.map(|(a, b)| panels::ordered_range(a, b, self.pb.first, self.pb.last));
        }
        old
    }

    /// The comment file of the shown file arrived (it comes right after the markers).
    pub fn poll_comments(&mut self) {
        self.sync_shared();
        if self.loading.is_some() || self.data.is_none() {
            return;
        }
        let Some(rx) = &self.comments_rx else { return };
        let load = match rx.try_recv() {
            Ok(load) => load,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => CommentsLoad { target: Err("Comment 읽기가 중간에 끝났어요".to_owned()), parsed: None, label: None },
        };
        self.comments_rx = None;
        self.comments.take_load(load);
        self.publish_shared_file();
    }

    /// Apply a prepared chunk without copying samples or dropping large buffers on the UI.
    /// A shared view is copied on a worker; it is never silently skipped by Arc::get_mut.
    pub fn queue_analog(&mut self, part: AnalogPart) {
        self.sync_shared();
        self.analog_queue.push(part);
        self.attach_queued_analog();
        self.publish_shared_file();
    }

    fn attach_queued_analog(&mut self) {
        let _span=crate::bench_timing::span("analog.attach");
        if self.analog_merge.is_some() { return; }
        let Some(part) = self.analog_queue.pop() else { return };
        let Some(data) = self.data.as_mut() else { drop_later(Some(part)); return };
        if let Some(data) = Arc::get_mut(data) {
            drop_later(Some(data.swap_prepared(part)));
            // There are at most the two stages, never one job per frame or channel.
            self.attach_queued_analog();
        } else {
            let source = data.clone();
            let (tx, rx) = channel();
            self.analog_merge = Some(rx);
            let trace=crate::headless::BENCH_TRACE.load(std::sync::atomic::Ordering::Relaxed);
            std::thread::spawn(move || {
                let start=trace.then(Instant::now);
                let mut updated = source.as_ref().clone();
                let cloned=start.map_or(0.0,|s|s.elapsed().as_secs_f64()*1000.0);
                let merge=trace.then(Instant::now);
                drop(updated.swap_prepared(part.clone()));
                let merged=merge.map_or(0.0,|s|s.elapsed().as_secs_f64()*1000.0);
                let _ = tx.send((source, updated, part, [cloned,merged]));
            });
        }
    }

    pub fn analog_work_pending(&self) -> bool {
        self.analog_rx.is_some() || self.emg_rx.is_some() || self.analog_merge.is_some() || !self.analog_queue.is_empty()
    }

    /// A save may move Channels.dat ranges. Retire only the old deferred reader and its
    /// delivery channel; loaded samples, running analog jobs and user demand stay intact.
    pub fn clear_deferred_emg_reader(&mut self) {
        let job = self.lazy_job.take();
        let receiver = self.lazy_job_rx.take();
        if job.is_some() || receiver.is_some() { drop_later(Some((job, receiver))); }
    }

    /// Own the freshly indexed post-save job without reading samples. The controller clears
    /// sibling readers and drains active jobs before installing one owner on the saver.
    /// Already loaded EMG needs no reader and is never rearmed or erased here.
    pub fn install_deferred_emg_reader(&mut self, job: tak_core::tak::fast_read::AnalogJob) {
        self.clear_deferred_emg_reader();
        if !self.data.as_ref().is_some_and(|data| data.emg_pending) {
            drop_later(Some(job));
            return;
        }
        self.lazy_job = Some(job);
        // A still-active receiver keeps its ownership; never launch a second EMG worker.
        self.emg_requested = self.emg_rx.is_some();
        self.emg_failure = None;
        // A restored clean view may have retired an old reader via cancel_load().
        if self.load_cancel.load(Ordering::SeqCst) {
            self.load_cancel = Arc::new(AtomicBool::new(false));
        }
    }

    /// A visible non-forceplate header/channel/graph requests this once per shown file.
    pub fn request_emg(&mut self, ctx: &egui::Context) {
        self.sync_shared();
        if self.remote.is_some() || self.loading.is_some() { return; }
        if self.devices.request_emg && self.data.as_ref().is_some_and(|data| data.emg_pending) {
            if let Some(shared) = &self.shared_file {
                if let Ok(mut state) = shared.0.lock() {
                    if !state.emg_demand {
                        state.emg_demand = true;
                        if let Some((document, wake)) = &state.emg_wake { let _ = wake.send(*document); }
                    }
                }
            }
        }
        if let Some(rx) = &self.lazy_job_rx {
            match rx.try_recv() {
                Ok((job, fallback)) => {
                    self.lazy_job = job;
                    self.analog_fallback = fallback;
                    self.lazy_job_rx = None;
                }
                Err(TryRecvError::Disconnected) => self.lazy_job_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if !self.data.as_ref().is_some_and(|d| d.emg_pending) {
            self.devices.request_emg = false;
            return;
        }
        let requested = self.devices.request_emg || self.shared_file.as_ref()
            .is_some_and(|shared| shared.0.lock().ok().is_some_and(|state| state.emg_demand));
        if !requested || self.emg_requested { return; }
        let Some(job) = self.lazy_job.take() else { return };
        if let Some(shared) = &self.shared_file {
            if let Ok(mut state) = shared.0.lock() { state.emg_demand = false; }
        }
        self.devices.request_emg = false;
        self.emg_requested = true;
        let target = self.data.as_ref().unwrap().analog_target();
        let indices = job.device_indices().to_vec();
        let (tx, rx) = channel();
        self.emg_rx = Some((self.file_id, rx));
        let wake = ctx.clone();
        let cancel = self.load_cancel.clone();
        std::thread::spawn(move || {
            while MARKER_LOADS.load(Ordering::SeqCst) > 0 && !cancel.load(Ordering::SeqCst) { std::thread::sleep(Duration::from_millis(5)); }
            if cancel.load(Ordering::SeqCst) { return; }
            let t = Instant::now();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                job.run().map(|(analog, warning)| AnalogPart::prepare_selected(analog, warning, &target, indices, false)).map_err(|e| e.to_string())
            })).unwrap_or_else(|_| Err("EMG 읽는 중 내부 오류가 났어요.".into()));
            let _ = tx.send(LoadMsg::Analog(result, ms_since(t)));
            wake.request_repaint();
        });
    }

    /// Forceplate startup and explicitly requested EMG arrive independently.
    pub fn poll_analog(&mut self) {
        let _span=crate::bench_timing::span("analog.poll");
        self.sync_shared();
        let previous_data = self.data.as_ref().map(Arc::as_ptr);
        for initial in [true, false] {
            let receiver = if initial { &mut self.analog_rx } else { &mut self.emg_rx };
            let Some((id, rx)) = receiver else { continue };
            let msg = match rx.try_recv() {
                Ok(msg) => Some(msg),
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => None,
            };
            let id = *id;
            drop_later(receiver.take());
            let (result, took) = match msg {
                Some(LoadMsg::Analog(result, took)) => (result, took),
                _ => (Err("읽기가 중간에 끝났어요".into()), 0.0),
            };
            if id != self.file_id { drop_later(Some(result)); continue; }
            self.load_steps.push((if initial { "analog" } else { "emg" }, took));
            if initial { self.analog_ms = self.open_started.map(ms_since); }
            match result {
                Ok(part) => self.queue_analog(part),
                Err(reason) => {
                    if !initial { self.emg_failure = Some(format!("EMG를 읽지 못했어요: {reason}")); }
                    if let Some(data) = &self.data {
                        self.queue_analog(AnalogPart::prepare_selected(Vec::new(), Some(format!("아날로그를 읽지 못했어요: {reason}")), &data.analog_target(), Vec::new(), initial));
                    }
                }
            }
        }
        if let Some(rx) = &self.analog_merge {
            if let Ok((source, updated, retry, timing)) = rx.try_recv() {
                crate::bench_timing::background("analog.clone_worker",timing[0]);
                crate::bench_timing::background("analog.merge_worker",timing[1]);
                self.analog_merge = None;
                if self.data.as_ref().is_some_and(|current| Arc::ptr_eq(current, &source)) {
                    drop_later(self.data.replace(Arc::new(updated)));
                    drop_later(Some(retry));
                } else {
                    // Marker edits/save can finish during a lazy read; merge into the latest data.
                    self.analog_queue.push(retry);
                    drop_later(Some(updated));
                }
                drop_later(Some(source));
            }
        }
        self.attach_queued_analog();
        if previous_data != self.data.as_ref().map(Arc::as_ptr) {
            self.plate = self.plate.filter(|&plate| self.data.as_ref()
                .is_some_and(|data| data.analog_pending || plate < data.plates.len()));
            self.publish_shared_file();
        }
    }

    /// Where this view is looking now (None without a file).
    pub fn spot(&self) -> Option<Spot> {
        let d = self.data.as_ref()?;
        let name = |m: usize| d.markers.get(m).map(|m| m.name.clone());
        let pivot = self.follow_target.filter(|_| self.cam.follow).unwrap_or(self.cam.pivot);
        Some(Spot {
            path: d.take.path.to_string_lossy().into_owned(),
            frame: self.pb.frame,
            pivot: [pivot.x, pivot.y, pivot.z],
            yaw: self.cam.yaw,
            pitch: self.cam.pitch,
            distance: self.cam.distance,
            fov_y: self.cam.fov_y,
            camera_mode: (self.cam.preset, self.cam.follow),
            selected: self.ordered_selection().into_iter().filter_map(name).collect(),
            primary: self.primary.and_then(name),
            plate: self.plate.and_then(|p| d.plates.get(p)).map(|p| p.name.clone()),
        })
    }

    /// Go back to a remembered spot. Markers and the plate are found by name; missing ones are
    /// skipped.
    /// A camera that does not make sense (old or damaged settings) is replaced by framing.
    pub fn apply_spot(&mut self, spot: &Spot, now: f64) {
        self.pb.set_frame(spot.frame, now);
        self.follow_target = None;
        let (preset, follow) = spot.camera_mode;
        let cam = Camera {
            preset,
            follow,
            pivot: Vec3::from_arr(spot.pivot),
            yaw: spot.yaw,
            pitch: spot.pitch,
            distance: spot.distance,
            fov_y: spot.fov_y,
        };
        let finite = spot.pivot.iter().chain([&spot.yaw, &spot.pitch, &spot.distance, &spot.fov_y]).all(|v| v.is_finite());
        if finite && cam.distance > 0.0 && cam.fov_y > 0.0 {
            self.cam = cam;
        } else {
            self.fit_pending = true;
        }
        let Some(d) = &self.data else { return };
        let find = |name: &str| d.markers.iter().position(|m| m.name == name);
        self.selected = spot.selected.iter().filter_map(|n| find(n)).collect();
        self.selection_order = spot.selected.iter().filter_map(|n| find(n)).collect();
        self.primary = spot.primary.as_deref().and_then(find).filter(|m| self.selected.contains(m));
        // Plates are known with the markers (their forces come later).
        self.plate = spot.plate.as_deref().and_then(|n| d.plates.iter().position(|p| p.name == n));
        self.normalize_selection();
        self.labels.scroll_to = self.primary;
    }

    /// Set the frame range from two ends in any order (kept inside the file's frames).
    pub fn set_range(&mut self, a: i64, b: i64) {
        self.range = Some(panels::ordered_range(a, b, self.pb.first, self.pb.last));
    }

    /// `[`: the range starts at the current frame (no range yet: the current frame only).
    /// Start <= end is kept by swapping.
    pub fn range_start_here(&mut self) {
        let f = self.pb.frame;
        let end = self.range.map_or(f, |r| r.1);
        self.set_range(f, end);
    }

    /// `]`: the range ends at the current frame (no range yet: the current frame only).
    pub fn range_end_here(&mut self) {
        let f = self.pb.frame;
        let start = self.range.map_or(f, |r| r.0);
        self.set_range(start, f);
    }

    /// A click on comment `id`'s block on the band, or a double click on the `#` of its row in
    /// the COMMENTS pane (v0.27.3): `select_comment`, then go to its first frame (playback
    /// stops). The range stays.
    pub fn goto_comment(&mut self, id: u64, now: f64) {
        let Some(start) = self.select_comment(id) else { return };
        self.pb.playing = false;
        self.pb.set_frame(start, now);
    }

    /// A single click on comment `id`'s row in the COMMENTS pane (v0.27.3; also the first half of
    /// `goto_comment`): it becomes the selected comment (v0.17.1) and its markers are selected,
    /// found by name (names no longer in the file are skipped; the first one found is the
    /// primary). The frame, playback and the range stay. A comment without markers (v0.15.2)
    /// keeps the current marker selection. Returns its first frame (None: no such comment or no
    /// data, nothing done).
    pub fn select_comment(&mut self, id: u64) -> Option<i64> {
        let (Some(d), Some(c)) = (&self.data, self.comments.list.get(id)) else { return None };
        let found: Vec<usize> = c.markers.iter().filter_map(|n| d.markers.iter().position(|m| &m.name == n)).collect();
        let (start, markerless) = (c.start, c.markers.is_empty());
        self.picked = Some(id);
        self.comment_selection.single(id);
        if !markerless {
            self.edit_selection = None;
            self.selected = found.iter().copied().collect();
            self.selection_order = found.clone();
            self.primary = found.first().copied();
            self.normalize_selection();
            self.labels.scroll_to = self.primary;
        }
        Some(start)
    }

    /// Choose force plate `p` for this view, or un-choose it when it is the chosen one.
    pub fn toggle_plate(&mut self, p: usize) {
        self.plate = if self.plate == Some(p) { None } else { Some(p) };
    }

    /// Back to an empty view. Returns the data, to be freed off the drawing thread.
    pub fn clear(&mut self) -> Option<Arc<ViewData>> {
        self.cancel_load();
        // The laid-over file goes with the view (plan step 7).
        drop_later(self.take_overlay());
        let mut old = std::mem::take(self);
        drop_later(old.edit_document.take());
        drop_later(old.restore_edit_document.take());
        let data = old.data.take();
        drop_later(Some(old));
        data
    }

    /// Commit or undo a prepared patch; the expensive view rebuild is requested separately.
    pub fn apply_marker_patch(&mut self, patch: &EditPatch, redo: bool) -> Result<(), EditError> {
        self.sync_shared();
        let session = self.edit_document.as_ref().ok_or(EditError::NotLoaded)?;
        let mut document = session.lock().map_err(|_| EditError::Other("편집 문서를 열 수 없어요".into()))?;
        document.apply(patch, redo)?;
        let revision = document.revision();
        drop(document);
        let selection = if redo { patch.selection_after.clone() } else { patch.selection_before.clone() };
        self.apply_uid_selection(&selection);
        self.edit_refresh = Some(revision);
        self.edit_selection = Some(selection);
        self.notify_editor_updates();
        Ok(())
    }

    fn uid_selection(&self) -> EditSelection {
        let data = self.data.as_ref();
        EditSelection {
            selected: self.selected.iter().filter_map(|&i| data?.markers.get(i).map(|m| m.uid)).collect(),
            order: self.ordered_selection().into_iter().filter_map(|i| data?.markers.get(i).map(|m| m.uid)).collect(),
            primary: self.primary.and_then(|i| data?.markers.get(i).map(|m| m.uid)),
            armed_label: self.armed_label,
        }
    }

    pub fn edit_selection_state(&self) -> EditSelection {
        self.edit_selection.clone().unwrap_or_else(|| self.uid_selection())
    }

    pub fn apply_uid_selection(&mut self, selection: &EditSelection) {
        self.armed_label = selection.armed_label;
        let Some(data) = &self.data else { return };
        self.selected = data.markers.iter().enumerate().filter_map(|(i, m)| selection.selected.contains(&m.uid).then_some(i)).collect();
        self.selection_order = selection.order.iter().filter_map(|id| data.markers.iter().position(|m| m.uid == *id)).collect();
        self.primary = selection.primary.and_then(|id| data.markers.iter().position(|m| m.uid == id)).filter(|i| self.selected.contains(i));
        self.normalize_selection();
        self.labels.scroll_to = self.primary;
    }

    /// Snapshot for an off-thread rebuild. Wait for analog before sharing or replacing its view.
    pub fn edit_refresh_snapshot(&self) -> Option<DocumentSnapshot> {
        if self.analog_work_pending() || self.data.as_ref().is_some_and(|d| d.analog_pending) { return None; }
        let desired = self.edit_refresh?;
        let document = self.edit_document.as_ref()?.lock().ok()?;
        (document.revision() == desired).then(|| document.snapshot())
    }

    /// Publish a worker-built view only for the latest document revision. Reset all marker
    /// caches together and retire the old buffers off-thread; the view keeps its file identity.
    pub fn publish_edited_view(&mut self, data: ViewData) -> Result<(), EditError> {
        self.sync_shared();
        if self.analog_work_pending() || self.data.as_ref().is_some_and(|d| d.analog_pending) { drop_later(Some(data)); return Err(EditError::Other("아날로그를 읽는 중이에요".into())); }
        let validate = || -> Result<(), EditError> {
            if self.data.as_ref().is_some_and(|current| current.analog_generation() != data.analog_generation()) {
                return Err(EditError::StaleRevision);
            }
            let expected = self.edit_refresh.ok_or(EditError::StaleRevision)?;
            let document = self.edit_document.as_ref().ok_or(EditError::NotLoaded)?.lock().map_err(|_| EditError::Other("편집 문서를 열 수 없어요".into()))?;
            if document.revision() != expected || data.edit_revision != Some(expected) { return Err(EditError::StaleRevision); }
            // A worker-verified reopen may bind this document to another spelling of the
            // same source. Rebuilds retain the currently shown path; unrelated paths still fail.
            if data.take.path != document.source_path() && document.save_path() != Some(data.take.path.as_path())
                && self.data.as_ref().map(|shown| shown.take.path.as_path()) != Some(data.take.path.as_path()) {
                return Err(EditError::WrongDocument);
            }
            Ok(())
        };
        if let Err(error) = validate() { drop_later(Some(data)); return Err(error); }
        self.install_shared_data(Arc::new(data));
        self.edit_refresh = None;
        self.publish_shared_file();
        Ok(())
    }

    /// A server-verified save is built on the SSH worker. Share that one allocation with
    /// every occurrence; never clone all samples on the drawing thread.
    pub fn install_remote_saved(&mut self, data:Arc<ViewData>) {
        self.install_shared_data(data);
        self.edit_refresh=None;
        self.publish_shared_file();
    }

    fn install_shared_data(&mut self, data: Arc<ViewData>) {
        let _span=crate::bench_timing::span("shared.install");
        // Analog publication preserves all marker buffers and the edit revision. Keep the
        // already prepared graph/labels instead of rebuilding every view at EMG readiness.
        if self.data.as_ref().is_some_and(|old| old.take.path == data.take.path
            && old.edit_revision == data.edit_revision
            && old.take.trajectories.len() == data.take.trajectories.len()
            && old.take.trajectories.iter().zip(&data.take.trajectories).all(|(a,b)|
                a.frames.shares_storage(&b.frames) && a.xyz.shares_storage(&b.xyz))) {
            self.plate = self.plate.filter(|&plate| data.analog_pending || plate < data.plates.len());
            drop_later(self.data.replace(data));
            return;
        }
        let filter = self.labels.filter.clone();
        let linked_pick = self.labels.linked_pick;
        let folds = self.data.as_ref().map(|old| panels::fold_keys(old, &self.labels.collapsed)).unwrap_or_default();
        let old_groups: std::collections::HashSet<_> = self.data.as_ref().map(|old| (0..old.groups.len())
            .map(|i| panels::FoldKey::group(old, i)).collect()).unwrap_or_default();
        let mut anchors = self.labels.list_selection.clone();
        let eligible = data.markers.iter().filter(|marker| marker.n_samples > 0).map(|marker| marker.uid).collect();
        for state in &mut anchors { state.retain(&eligible); }
        self.labels.reset(&data, self.file_id);
        self.labels.list_selection = anchors;
        self.labels.filter = filter;
        self.labels.linked_pick = linked_pick;
        let new_defaults: Vec<_> = self.labels.collapsed.iter().copied()
            .filter(|&i| !old_groups.contains(&panels::FoldKey::group(&data, i))).collect();
        self.labels.collapsed = panels::fold_indices(&data, &folds);
        self.labels.collapsed.extend(new_defaults);
        // Preserve a later user selection if it replaced the patch's pending selection.
        let selection = self.edit_selection.take().unwrap_or_else(|| self.uid_selection());
        drop_later(Some(std::mem::take(&mut self.lanes)));
        drop_later(Some(std::mem::take(&mut self.heat)));
        drop_later(self.data.replace(data));
        let comments = self.comment_selection.clone();
        let picked = self.picked;
        self.apply_uid_selection(&selection);
        // Data publication remaps marker UIDs; it is not a user's new marker pick.
        self.comment_selection = comments;
        self.picked = picked;
        self.plate = self.plate.filter(|&plate| self.data.as_ref()
            .is_some_and(|data| data.analog_pending || plate < data.plates.len()));
        if self.data.as_ref().is_some_and(|data| data.edit_revision == self.edit_refresh) {
            self.edit_refresh = None;
        }
    }

    /// Current selection in click order. Public set/primary assignments and older snapshots
    /// are completed deterministically, with a valid primary last.
    pub fn ordered_selection(&self) -> Vec<usize> {
        let mut remaining = self.selected.clone();
        let mut order: Vec<_> = self.selection_order.iter().chain(self.selected.iter()).copied().filter(|m| remaining.remove(m)).collect();
        if let Some(primary) = self.primary.filter(|m| self.selected.contains(m)) {
            order.retain(|m| *m != primary);
            order.push(primary);
        }
        order
    }

    fn normalize_selection(&mut self) {
        self.selection_order = self.ordered_selection();
        self.primary = self.selection_order.last().copied();
    }

    pub fn select(&mut self, marker: usize, additive: bool) {
        for selection in &mut self.labels.list_selection { selection.end_range(); }
        self.clear_comment_selection();
        self.edit_selection = None;
        self.normalize_selection();
        if additive {
            if !self.selected.remove(&marker) {
                self.selected.insert(marker);
                self.selection_order.push(marker);
            } else {
                self.selection_order.retain(|m| *m != marker);
            }
        } else {
            self.selected.clear();
            self.selected.insert(marker);
            self.selection_order = vec![marker];
        }
        self.primary = self.selection_order.last().copied();
        self.labels.scroll_to = self.primary;
    }

    /// Box selection: replace the selection, or add to it with Ctrl. An empty box without
    /// Ctrl clears the selection (like a click on empty space).
    pub fn select_many(&mut self, mut markers: Vec<usize>, primary: Option<usize>, additive: bool) {
        for selection in &mut self.labels.list_selection { selection.end_range(); }
        self.clear_comment_selection();
        self.edit_selection = None;
        if !additive {
            self.clear_selection();
        } else {
            self.normalize_selection();
        }
        markers.sort_unstable();
        markers.dedup();
        for marker in markers {
            if self.selected.insert(marker) { self.selection_order.push(marker); }
        }
        if primary.is_some_and(|m| self.selected.contains(&m)) {
            self.primary = primary;
        } else {
            self.primary = self.selection_order.last().copied();
        }
        self.normalize_selection();
        self.labels.scroll_to = self.primary;
    }

    pub fn clear_comment_selection(&mut self) {
        if !self.comment_marker_draft {
            self.picked = None;
            self.comment_selection.clear();
        }
    }

    pub(crate) fn link_labeling_to_scene(&mut self, enabled: bool) {
        if !enabled { return; }
        self.labels.linked_pick = panels::LinkedLabelPick::default();
        let Some(index) = self.primary else { return };
        let Some(data) = self.data.as_ref() else { return };
        let Some(marker) = data.markers.get(index) else { return };
        let asset = marker.parent.filter(|uid| {
            !matches!(marker.kind, MarkerKind::Unlabeled | MarkerKind::Unknown)
                && (data.take.assets.iter().any(|a| a.uid == *uid)
                    || data.take.label_groups.iter().any(|a| a.uid == *uid))
        });
        self.labels.linked_pick = panels::LinkedLabelPick { asset, track: Some(marker.uid) };
        self.labels.collapsed.remove(&marker.group);
        self.labels.scroll_to = Some(index);
    }

    pub fn clear_selection(&mut self) {
        for selection in &mut self.labels.list_selection { selection.end_range(); }
        self.clear_comment_selection();
        self.edit_selection = None;
        self.selected.clear();
        self.selection_order.clear();
        self.primary = None;
    }

    /// The selection now (markers and the selected comment).
    pub fn selection(&self) -> Selection {
        Selection { selected: self.selected.clone(), order: self.ordered_selection(), primary: self.primary, comment: self.picked, comments: self.comment_selection.clone(), list_anchors: self.labels.list_selection.clone(), armed_label: self.armed_label, linked_pick: self.labels.linked_pick }
    }

    /// Put a selection back (undo / redo) the way a click leaves it: Labels scrolls to the
    /// primary marker; the 3D, panes and graphs read `selected` / `primary` every frame.
    pub fn set_selection(&mut self, s: Selection) {
        self.edit_selection = None;
        self.selected = s.selected;
        self.selection_order = s.order;
        self.primary = s.primary;
        self.picked = s.comment;
        self.comment_selection = s.comments;
        self.labels.list_selection = s.list_anchors;
        self.armed_label = s.armed_label;
        self.labels.linked_pick = s.linked_pick;
        self.normalize_selection();
        self.labels.scroll_to = self.primary;
    }

    /// Names of the selected markers in Labels order (the order a new comment stores them in).
    pub fn selected_names(&self) -> Vec<String> {
        let Some(d) = &self.data else { return Vec::new() };
        self.selected.iter().filter_map(|&m| d.markers.get(m).map(|m| m.name.clone())).collect()
    }

    /// The markers comment `c` gets when the marker selection is changed by hand while it is
    /// selected (v0.17.1): the selected names in Labels order, then its names this file does
    /// not have (they cannot be chosen here, so they stay, in their order). None when that is
    /// the same set of names it has (its stored order is kept then).
    pub fn markers_for(&self, c: &comments::Comment) -> Option<Vec<String>> {
        let d = self.data.as_ref()?;
        let mut names = self.selected_names();
        names.extend(c.markers.iter().filter(|n| !d.markers.iter().any(|m| &m.name == *n)).cloned());
        let set = |v: &[String]| v.iter().cloned().collect::<BTreeSet<String>>();
        (set(&names) != set(&c.markers)).then_some(names)
    }

    /// Status bar words of the marker selection now: `Selection LKNE`, `Selection LKNE +2` (the
    /// primary marker's name first), `Selection cleared`.
    pub fn selection_words(&self) -> String {
        let Some(d) = &self.data else { return "Selection".into() };
        let first = self.primary.or_else(|| self.selected.iter().next().copied());
        match first.and_then(|m| d.markers.get(m)) {
            None => "Selection cleared".into(),
            Some(m) if self.selected.len() > 1 => format!("Selection {} +{}", m.name, self.selected.len() - 1),
            Some(m) => format!("Selection {}", m.name),
        }
    }

    /// Status bar words of the selected comment now (v0.17.1): `Comment 3 selected`,
    /// `Comment deselected`.
    pub fn picked_words(&self) -> String {
        match self.picked {
            Some(id) => format!("Comment {id} selected"),
            None => "Comment deselected".into(),
        }
    }

    /// Find the closest gap boundary across the entire selected marker set.
    pub fn next_selected_gap(&self, forward: bool) -> Option<i64> {
        let data = self.data.as_ref()?;
        let frame = self.pb.frame;
        self.selected.iter().filter_map(|&index| {
            let gaps = &data.markers.get(index)?.gaps;
            if forward {
                gaps.get(gaps.partition_point(|&(start, _)| start <= frame)).map(|gap| gap.0)
            } else {
                gaps.partition_point(|&(_, end)| end < frame).checked_sub(1)
                    .and_then(|at| gaps.get(at)).map(|gap| gap.1)
            }
        }).reduce(|a, b| if forward { a.min(b) } else { a.max(b) })
    }

    /// Fit all selected points visible at the current frame.
    pub fn focus_selected(&mut self) {
        if self.overlay.is_some() && !self.shown[0] { return; }
        let Some(data) = &self.data else { return };
        let mut positions = self.selected.iter().filter_map(|&index| {
            data.markers.get(index)?.position(self.pb.frame)
        });
        let Some(first) = positions.next() else { return };
        let (mut lo, mut hi) = (first, first);
        for point in positions {
            lo = v3(lo.x.min(point.x), lo.y.min(point.y), lo.z.min(point.z));
            hi = v3(hi.x.max(point.x), hi.y.max(point.y), hi.z.max(point.z));
        }
        let aspect = if self.scene_rect.is_positive() {
            self.scene_rect.width() / self.scene_rect.height().max(1.0)
        } else { 1.0 };
        self.cam.fit_box(lo, hi, aspect);
    }
}

/// A small file's view data for tests: one unlabeled marker per name, frames 0..=9.
#[cfg(test)]
pub(crate) fn sample_data(path: &str, names: &[&str]) -> ViewData {
    ViewData::build(sample_take(path, names))
}

/// `sample_data` with two force plates on the floor, "P1" (x -0.6..-0.1) and "P2" (x 0.1..0.6),
/// z -0.25..0.25, forces read. P1's device has six channels (Fx … Mz): channel 2 (Fz) =
/// 100 + 10 × frame, none at frames 4 and 5; channel k ≠ 2 = 1000 × k + frame, none at frame 8
/// for k = 3 (Mx). P2's device has three channels (Fx, Fy, Fz), all 0.
#[cfg(test)]
pub(crate) fn sample_with_plates(path: &str, names: &[&str]) -> ViewData {
    use tak_core::tak::{AnalogChannel, AnalogDevice, ForcePlate};
    let plate = |name: &str, x0: f32, x1: f32| ForcePlate { name: name.into(), corners: [[x0, 0.0, -0.25], [x1, 0.0, -0.25], [x1, 0.0, 0.25], [x0, 0.0, 0.25]] };
    let device = |n: u32, value: &dyn Fn(u32, u32) -> f32| AnalogDevice {
        channels: (0..n)
            .map(|k| AnalogChannel {
                name: format!("AnalogChannel{k}"),
                kind: "Analog".into(),
                dim: 1,
                frames: (0..10).collect(),
                values: (0..10).map(|f| value(k, f)).collect(),
            })
            .collect(),
    };
    let p1 = |k: u32, f: u32| match k {
        2 if f == 4 || f == 5 => f32::NAN,
        2 => 100.0 + 10.0 * f as f32,
        3 if f == 8 => f32::NAN,
        _ => 1000.0 * k as f32 + f as f32,
    };
    let mut take = sample_take(path, names);
    take.plates = vec![plate("P1", -0.6, -0.1), plate("P2", 0.1, 0.6)];
    take.analog = vec![device(6, &p1), device(3, &|_, _| 0.0)];
    ViewData::build(take)
}

#[cfg(test)]
pub(crate) fn sample_take(path: &str, names: &[&str]) -> tak_core::tak::Take {
    use tak_core::tak::{MarkerKind, Take, Trajectory};
    let trajectories = names
        .iter()
        .enumerate()
        .map(|(k, name)| Trajectory {
            uid: [k as u8 + 1; 16],
            name: (*name).into(),
            parent_uid: None,
            slot_raw: None,
            slot: None,
            kind: MarkerKind::Unlabeled,
            asset_uid: None,
            frames: (0..10).collect(),
            xyz: (0..10).map(|f| [k as f32 * 0.1, 1.0, f as f32 * 0.01]).collect(),
            raw16_sha256: String::new(),
            lenient_membership: false,
        })
        .collect();
    Take {
        device_sampling: Vec::new(),
        device_names: Vec::new(),
        path: path.into(),
        file_size: 0,
        metadata: vec![],
        frame_rate: 100.0,
        start_frame: 0,
        end_frame: 9,
        trajectories,
        assets: vec![],
        label_groups: vec![],
        plates: vec![],
        analog: vec![],
        warnings: vec![],
    }
}

#[cfg(test)]
mod tests {
    include!("views_merge_tests.rs");
    use super::*;

    fn selection_view() -> View {
        let mut view = View::default();
        let (_tx, rx) = channel();
        view.show(sample_data("selection.tak", &["A", "B", "C", "D"]), rx, 1, 1.0, 0.0);
        view
    }

    fn overlay_host() -> View {
        let mut view = View::default();
        let (_tx, rx) = channel();
        view.show(sample_data("host.tak", &["A", "B", "C", "D"]), rx, 7, 1.0, 0.0);
        view
    }

    fn editable_selection_view() -> View {
        use tak_core::edit::{DocumentInput, RawSample, RawTrack, RecordBytes};
        let view = selection_view();
        let tracks = view.data.as_ref().unwrap().take.trajectories.iter().map(|track| RawTrack {
            uid: track.uid, name: track.name.clone(), label: None,
            records: RecordBytes { node: Arc::from([1]), translation_header: Arc::from([2]), serialization_verified: true },
            samples: track.frames.iter().zip(&track.xyz).map(|(&frame, &xyz)| RawSample::new(frame, xyz)).collect::<Vec<_>>().into(),
        }).collect();
        {
            let mut document = view.edit_document.as_ref().unwrap().lock().unwrap();
            let revision = document.revision();
            document.install_input(revision, DocumentInput {
                streams: Vec::new().into(), nodes_prefix: Arc::from([]), channels_prefix: Arc::from([]),
                tracks, labels: Vec::new(), first_frame: 0, last_frame: 9,
            }).unwrap();
        }
        view
    }

    #[test]
    fn shared_overlay_consumes_marker_edit_publications_and_keeps_the_dirty_document_alive() {
        let mut owner = editable_selection_view();
        let mut host = overlay_host();
        host.start_shared_overlay("selection.tak".into(), owner.shared_file().unwrap(), false);
        let (_, overlay, quiet) = host.poll_overlay().unwrap();
        assert!(!quiet);
        host.overlay = Some(overlay.unwrap());
        assert!(Arc::ptr_eq(owner.data.as_ref().unwrap(), &host.overlay.as_ref().unwrap().data));
        assert_eq!(owner.document_id(), host.overlay_document_id());
        owner.select(0, false);
        let snapshot = owner.edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let patch = tak_core::edit::delete::delete_samples(&snapshot, &tak_core::edit::delete::DeleteRequest {
            selection: owner.edit_selection_state(), range: None, current_frame: 0,
        }).unwrap().patch.unwrap();
        owner.apply_marker_patch(&patch, true).unwrap();
        assert!(host.overlay_marker_dirty(), "dirty status reads the same document before a marker rebuild");
        let updated = owner.data.as_ref().unwrap().rebuild_markers(&patch.after_snapshot()).unwrap();
        owner.publish_edited_view(updated).unwrap();
        host.sync_shared();
        assert!(!host.overlay.as_ref().unwrap().data.markers[0].present(0));
        let document = host.overlay_document_id();
        drop(owner);
        assert!(host.overlay_marker_dirty(), "the final overlay reference still needs Save/Discard/Cancel");
        let overlay_owner = host.overlay_owner_mut().unwrap();
        assert_eq!(overlay_owner.document_id(), document);
        let snapshot = overlay_owner.edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let destination = PathBuf::from("overlay_saved.takm");
        overlay_owner.edit_document.as_ref().unwrap().lock().unwrap().mark_saved(&snapshot, destination.clone()).unwrap();
        let mut saved = overlay_owner.data.as_ref().unwrap().rebuild_markers(&snapshot).unwrap();
        saved.take.path = destination.clone();
        overlay_owner.edit_refresh = Some(snapshot.revision());
        overlay_owner.publish_edited_view(saved).unwrap();
        host.sync_shared();
        assert!(!host.overlay_marker_dirty());
        assert_eq!(host.overlay_shared_target(), Some(destination));
        assert_eq!(host.overlay_document_id(), document);
    }

    #[test]
    fn overlay_only_file_opens_as_primary_without_reading_or_copying_markers() {
        let mut host = overlay_host();
        host.overlay = Some(Overlay::from_data("selection.tak".into(), sample_data("selection.tak", &["A"])));
        let (tx, rx) = channel();
        let cancel = host.overlay_owner_mut().unwrap().load_cancel.clone();
        host.overlay_owner_mut().unwrap().analog_rx = Some((0, rx));
        let shared = host.shared_overlay_file(Path::new("selection.tak")).unwrap();
        let mut primary = View::default();
        primary.show_shared_file(shared, 12, 1.0, 0.0);
        assert!(Arc::ptr_eq(&host.overlay.as_ref().unwrap().data, primary.data.as_ref().unwrap()));
        assert!(primary.loading.is_none() && primary.comments_rx.is_none());
        assert!(host.transfer_overlay_work_to(&mut primary));
        assert!(host.overlay_owner().unwrap().analog_rx.is_none());
        assert!(Arc::ptr_eq(&cancel, &primary.load_cancel));
        let (id, moved_rx) = primary.analog_rx.as_ref().unwrap();
        assert_eq!(*id, 12);
        tx.send(LoadMsg::Analog(Err("sent through original reader".into()), 0.0)).unwrap();
        assert!(matches!(moved_rx.try_recv(), Ok(LoadMsg::Analog(Err(reason), _)) if reason == "sent through original reader"));
        host.take_overlay();
        assert!(!primary.load_cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn primary_reader_can_survive_in_an_overlay_and_transfer_back() {
        let mut owner = selection_view();
        let mut host = overlay_host();
        let shared = owner.shared_file().unwrap();
        host.start_shared_overlay("selection.tak".into(), shared.clone(), false);
        host.overlay = Some(host.poll_overlay().unwrap().1.unwrap());
        let (_tx, rx) = channel();
        owner.analog_rx = Some((owner.file_id, rx));
        let cancel = owner.load_cancel.clone();
        assert!(owner.transfer_primary_work_to_overlay(&mut host));
        assert!(owner.analog_rx.is_none());
        assert!(host.overlay_owner().unwrap().analog_rx.is_some());
        let mut next = View::default();
        next.show_shared_file(shared, 44, 1.0, 0.0);
        assert!(host.transfer_overlay_work_to(&mut next));
        assert!(next.analog_rx.is_some());
        assert!(Arc::ptr_eq(&cancel, &next.load_cancel));
    }

    #[test]
    fn overlay_waiter_has_no_reader_and_keeps_previous_overlay_until_shared_attach() {
        let mut host = overlay_host();
        host.overlay = Some(Overlay::from_data("old.tak".into(), sample_data("old.tak", &["M"])));
        host.wait_overlay_shared("selection.tak".into(), true);
        assert_eq!(host.overlay_reading_path(), None);
        assert_eq!(host.overlay_waiting_shared_path(), Some(Path::new("selection.tak")));
        assert!(host.poll_overlay().is_none());
        assert_eq!(host.overlay_path(), Some(Path::new("old.tak")));
        let mut owner = selection_view();
        assert!(host.attach_overlay_shared(Path::new("selection.tak"), owner.shared_file().unwrap()));
        let (path, overlay, quiet) = host.poll_overlay().unwrap();
        assert!(quiet);
        assert_eq!(path, Path::new("selection.tak"));
        assert!(Arc::ptr_eq(owner.data.as_ref().unwrap(), &overlay.unwrap().data));
    }

    #[test]
    fn retained_closed_view_adopts_fresh_document_preserving_view_state_and_overlay() {
        let mut retained = selection_view();
        retained.file_id = 78;
        retained.pb.set_frame(6, 0.0);
        retained.cam.yaw = 1.25;
        retained.range = Some((2, 8));
        retained.select(2, false);
        retained.select(0, true);
        let comment = comments::Comment { id: 91, markers: vec![], start: 2, end: 8, comment_type: String::new(), text: "retained".into() };
        let event = comments::Event::of(comments::Kind::Add, &comment, String::new(), String::new());
        retained.comments.list.apply(&event);
        retained.select_comment(91);
        let selected = retained.edit_selection_state();
        retained.remote = Some(crate::ssh::RemoteLocation { host: "readonly-server".into(), path: "selection.tak".into() });
        retained.remote_warning = Some("read only".into());
        retained.overlay = Some(Overlay::from_data("second.tak".into(), sample_data("second.tak", &["M"])));
        retained.shown = [false, true];
        let overlay = retained.overlay.as_ref().unwrap().data.clone();
        let previous_document = retained.document_id();
        let mut fresh = selection_view();
        fresh.comments.list.apply(&event);
        let mut fresh_take = sample_take("selection.tak", &["A", "B", "C", "D"]);
        fresh_take.trajectories.rotate_left(1);
        fresh.data = Some(Arc::new(ViewData::build(fresh_take)));
        let shared = fresh.shared_file().unwrap();
        assert!(retained.adopt_shared_preserving_state(shared, 4.0));
        assert_ne!(retained.document_id(), previous_document);
        assert!(retained.shares_document(&fresh));
        assert!(Arc::ptr_eq(retained.data.as_ref().unwrap(), fresh.data.as_ref().unwrap()));
        assert_eq!(retained.file_id, 78);
        assert_eq!(retained.pb.frame, 6);
        assert_eq!(retained.cam.yaw, 1.25);
        assert_eq!(retained.range, Some((2, 8)));
        assert_eq!(retained.picked, Some(91));
        assert_eq!(retained.edit_selection_state(), selected);
        assert_eq!(retained.remote.as_ref().unwrap().host, "readonly-server");
        assert_eq!(retained.remote_warning.as_deref(), Some("read only"));
        assert_eq!(retained.shown, [false, true]);
        assert!(Arc::ptr_eq(&retained.overlay.as_ref().unwrap().data, &overlay));
    }

    #[test]
    fn retained_overlay_owner_can_adopt_but_independent_dirty_documents_cannot_be_erased() {
        let mut host = overlay_host();
        host.overlay = Some(Overlay::from_data("selection.tak".into(), sample_data("selection.tak", &["A"])));
        let mut fresh = selection_view();
        assert!(host.overlay_owner_mut().unwrap().adopt_shared_preserving_state(fresh.shared_file().unwrap(), 1.0));
        host.sync_shared();
        assert_eq!(host.overlay_document_id(), fresh.document_id());
        assert!(Arc::ptr_eq(&host.overlay.as_ref().unwrap().data, fresh.data.as_ref().unwrap()));
        let mut dirty = editable_selection_view();
        dirty.select(0, false);
        let snapshot = dirty.edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let patch = tak_core::edit::delete::delete_samples(&snapshot, &tak_core::edit::delete::DeleteRequest {
            selection: dirty.edit_selection_state(), range: None, current_frame: 0,
        }).unwrap().patch.unwrap();
        dirty.apply_marker_patch(&patch, true).unwrap();
        let old_document = dirty.document_id();
        let old_data = dirty.data.as_ref().unwrap().clone();
        assert!(!dirty.adopt_shared_preserving_state(fresh.shared_file().unwrap(), 1.0));
        assert_eq!(dirty.document_id(), old_document);
        assert!(dirty.marker_dirty());
        assert!(Arc::ptr_eq(&old_data, dirty.data.as_ref().unwrap()));
    }

    #[test]
    fn retained_discarded_document_adopts_even_with_a_clean_deferred_marker_refresh() {
        let mut retained = editable_selection_view();
        retained.select(0, false);
        retained.pb.set_frame(5, 0.0);
        let snapshot = retained.edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let patch = tak_core::edit::delete::delete_samples(&snapshot, &tak_core::edit::delete::DeleteRequest {
            selection: retained.edit_selection_state(), range: None, current_frame: 0,
        }).unwrap().patch.unwrap();
        retained.apply_marker_patch(&patch, true).unwrap();
        let edited = retained.data.as_ref().unwrap().rebuild_markers(&patch.after_snapshot()).unwrap();
        retained.publish_edited_view(edited).unwrap();
        assert!(!retained.data.as_ref().unwrap().markers[0].present(0));
        {
            let mut document = retained.edit_document.as_ref().unwrap().lock().unwrap();
            document.discard_unsaved();
            retained.edit_refresh = Some(document.revision());
        }
        assert!(!retained.marker_dirty());
        assert!(retained.edit_refresh.is_some(), "closed views do not run their deferred rebuild");
        let mut fresh = selection_view();
        assert!(retained.adopt_shared_preserving_state(fresh.shared_file().unwrap(), 1.0));
        assert!(retained.shares_document(&fresh));
        assert_eq!(retained.pb.frame, 5);
        assert!(retained.edit_refresh.is_none());
        assert!(retained.data.as_ref().unwrap().markers[0].present(0));
    }

    #[test]
    fn overlay_emg_demand_is_not_started_while_controller_gates_a_save() {
        let ctx = egui::Context::default();
        let mut host = overlay_host();
        let mut data = sample_data("lazy_overlay.tak", &["M"]);
        data.emg_pending = true;
        host.overlay = Some(Overlay::from_data("lazy_overlay.tak".into(), data));
        let (_tx, rx) = channel();
        let owner = host.overlay_owner_mut().unwrap();
        owner.lazy_job_rx = Some(rx);
        owner.devices.request_emg = true;
        let shared = owner.shared_file().unwrap();
        host.poll_overlay_file_work_gated(&ctx, false);
        assert!(!shared.0.lock().unwrap().emg_demand);
        assert!(!host.overlay_owner().unwrap().emg_requested);
        host.poll_overlay_file_work_gated(&ctx, true);
        assert!(shared.0.lock().unwrap().emg_demand);
    }

    #[test]
    fn clearing_deferred_emg_preserves_loaded_data_active_jobs_and_demand() {
        let mut view = selection_view();
        let data = view.data.as_ref().unwrap().clone();
        let (_tx, lazy_rx) = channel();
        let (_analog_tx, analog_rx) = channel();
        let (_emg_tx, emg_rx) = channel();
        view.lazy_job_rx = Some(lazy_rx);
        view.analog_rx = Some((view.file_id, analog_rx));
        view.emg_rx = Some((view.file_id, emg_rx));
        view.devices.request_emg = true;
        view.emg_requested = true;
        view.emg_failure = Some("old read error".into());
        let cancel = view.load_cancel.clone();
        view.clear_deferred_emg_reader();
        assert!(view.lazy_job.is_none() && view.lazy_job_rx.is_none());
        assert!(view.analog_rx.is_some() && view.emg_rx.is_some());
        assert!(view.devices.request_emg && view.emg_requested);
        assert_eq!(view.emg_failure.as_deref(), Some("old read error"));
        assert!(Arc::ptr_eq(&data, view.data.as_ref().unwrap()));
        assert!(Arc::ptr_eq(&cancel, &view.load_cancel));
        assert!(!cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn installing_post_save_deferred_emg_reader_is_lazy_and_only_rearms_pending_data() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../input/static/000000_S01_static.tak");
        if !path.exists() { return; }
        let read = tak_core::tak::fast_read::read_take_fast(&path, false).unwrap();
        let (_, job) = read.analog.expect("fixture has device metadata").split();
        let job = job.expect("fixture has deferred EMG");
        let mut pending = sample_data("saved.takm", &["M"]);
        pending.emg_pending = true;
        let (_tx, rx) = channel();
        let mut view = View::default();
        view.show(pending, rx, 91, 1.0, 0.0);
        let data = view.data.as_ref().unwrap().clone();
        let (_lazy_tx, lazy_rx) = channel();
        view.lazy_job_rx = Some(lazy_rx);
        view.emg_requested = true;
        view.emg_failure = Some("stale offsets".into());
        view.load_cancel.store(true, Ordering::SeqCst);
        view.install_deferred_emg_reader(job);
        assert!(view.lazy_job.is_some() && view.lazy_job_rx.is_none());
        assert!(!view.emg_requested && view.emg_failure.is_none() && view.emg_rx.is_none());
        assert!(!view.load_cancel.load(Ordering::SeqCst));
        assert!(Arc::ptr_eq(&data, view.data.as_ref().unwrap()));
        let job = view.lazy_job.take().unwrap();
        let (_emg_tx, emg_rx) = channel();
        view.emg_rx = Some((view.file_id, emg_rx));
        view.install_deferred_emg_reader(job);
        assert!(view.emg_requested && view.emg_rx.is_some(), "an active read cannot be duplicated");
        let job = view.lazy_job.take().unwrap();
        let mut loaded = selection_view();
        let loaded_data = loaded.data.as_ref().unwrap().clone();
        loaded.emg_requested = true;
        loaded.emg_failure = Some("kept status".into());
        loaded.install_deferred_emg_reader(job);
        assert!(loaded.lazy_job.is_none() && loaded.emg_rx.is_none());
        assert!(loaded.emg_requested);
        assert_eq!(loaded.emg_failure.as_deref(), Some("kept status"));
        assert!(Arc::ptr_eq(&loaded_data, loaded.data.as_ref().unwrap()));
    }

    #[test]
    fn repeated_views_share_data_document_and_source_but_keep_view_state_and_id_pools() {
        let mut owner = selection_view();
        owner.pb.set_frame(6, 0.0);
        owner.select(2, false);
        owner.comments.id_pool = Some(100..=200);
        owner.source_stamp = Some(Arc::new(Mutex::new(None)));
        let shared = owner.shared_file().unwrap();
        let mut other = View::default();
        other.show_shared_file(shared, 2, 0.5, 0.0);
        assert!(Arc::ptr_eq(owner.data.as_ref().unwrap(), other.data.as_ref().unwrap()));
        assert!(owner.shares_document(&other));
        assert!(Arc::ptr_eq(owner.source_stamp.as_ref().unwrap(), other.source_stamp.as_ref().unwrap()));
        assert_eq!(owner.pb.frame, 6);
        assert_eq!(other.pb.frame, 0);
        assert!(other.selected.is_empty());
        assert!(other.comments.id_pool.is_none(), "reserved IDs must not be issued by both views");
        assert_eq!(owner.comments.id_pool.as_mut().unwrap().next(), Some(100));
        other.pb.set_frame(3, 0.0);
        other.select(0, false);
        assert_eq!(owner.pb.frame, 6);
        assert_eq!(owner.primary, Some(2));
        assert_ne!(owner.file_id, other.file_id, "UI and writer tags remain per view");
    }

    #[test]
    fn shared_waiter_keeps_previous_view_until_attach_without_a_second_reader() {
        let mut previous = selection_view();
        previous.pb.set_frame(4, 0.0);
        previous.select(1, false);
        let visible = previous.data.clone().unwrap();
        previous.wait_for_shared("new.tak".into(), None);
        assert_eq!(previous.target(), Some(Path::new("new.tak")));
        assert_eq!(previous.shared_target(), Some(PathBuf::from("new.tak")));
        assert_eq!(previous.waiting_shared_path(), Some(Path::new("new.tak")));
        assert_eq!(previous.path(), Some(Path::new("selection.tak")));
        assert!(Arc::ptr_eq(previous.data.as_ref().unwrap(), &visible));
        assert!(previous.loading.is_none() && previous.comments_rx.is_none());
        assert!(previous.busy());
        assert!(!previous.file_work_pending(), "a waiter owns no polling work");
        assert!(previous.shared_file().is_none(), "old visible data cannot satisfy the new target");
        assert_eq!(previous.clear_shared_wait(), Some(PathBuf::from("new.tak")));
        assert_eq!(previous.target(), previous.path());
        assert_eq!((previous.pb.frame, previous.primary), (4, Some(1)));

        let mut owner = View::default();
        let (_tx, rx) = channel();
        owner.show(sample_data("new.tak", &["M"]), rx, 3, 1.0, 0.0);
        owner.pb.set_frame(6, 0.0);
        let restore = owner.spot();
        previous.wait_for_shared("new.tak".into(), restore);
        previous.show_shared_file(owner.shared_file().unwrap(), 4, 1.0, 0.0);
        assert!(previous.waiting_shared_path().is_none());
        assert_eq!(previous.path(), Some(Path::new("new.tak")));
        assert_eq!(previous.pb.frame, 6);
        assert!(previous.shares_document(&owner));
    }

    #[test]
    fn shared_marker_edit_undo_and_save_change_one_document_in_every_view() {
        use tak_core::edit::{DocumentInput, RawSample, RawTrack, RecordBytes};
        let mut owner = selection_view();
        let tracks = owner.data.as_ref().unwrap().take.trajectories.iter().map(|track| RawTrack {
            uid: track.uid, name: track.name.clone(), label: None,
            records: RecordBytes { node: Arc::from([1]), translation_header: Arc::from([2]), serialization_verified: true },
            samples: track.frames.iter().zip(&track.xyz).map(|(&frame, &xyz)| RawSample::new(frame, xyz)).collect::<Vec<_>>().into(),
        }).collect();
        {
            let mut doc = owner.edit_document.as_ref().unwrap().lock().unwrap();
            let revision = doc.revision();
            doc.install_input(revision, DocumentInput {
                streams: Vec::new().into(), nodes_prefix: Arc::from([]), channels_prefix: Arc::from([]),
                tracks, labels: Vec::new(), first_frame: 0, last_frame: 9,
            }).unwrap();
        }
        let shared = owner.shared_file().unwrap();
        let mut other = View::default();
        other.show_shared_file(shared, 2, 1.0, 0.0);
        owner.select(0, false);
        other.select(2, false);
        let other_selection = other.edit_selection_state();
        let snapshot = owner.edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let patch = tak_core::edit::delete::delete_samples(&snapshot, &tak_core::edit::delete::DeleteRequest {
            selection: owner.edit_selection_state(), range: None, current_frame: 0,
        }).unwrap().patch.unwrap();
        owner.apply_marker_patch(&patch, true).unwrap();
        assert!(owner.marker_dirty() && other.marker_dirty());
        let updated = owner.data.as_ref().unwrap().rebuild_markers(&patch.after_snapshot()).unwrap();
        owner.publish_edited_view(updated).unwrap();
        other.sync_shared();
        assert!(!other.data.as_ref().unwrap().markers[0].present(0));
        assert_eq!(other.edit_selection_state(), other_selection);

        owner.apply_marker_patch(&patch, false).unwrap();
        let snapshot = owner.edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let restored = owner.data.as_ref().unwrap().rebuild_markers(&snapshot).unwrap();
        owner.publish_edited_view(restored).unwrap();
        other.sync_shared();
        assert!(!owner.marker_dirty() && !other.marker_dirty());
        assert!(other.data.as_ref().unwrap().markers[0].present(0));

        owner.apply_marker_patch(&patch, true).unwrap();
        let snapshot = owner.edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let destination = PathBuf::from("saved.takm");
        owner.edit_document.as_ref().unwrap().lock().unwrap().mark_saved(&snapshot, destination.clone()).unwrap();
        let mut saved = owner.data.as_ref().unwrap().rebuild_markers(&snapshot).unwrap();
        saved.take.path = destination.clone();
        owner.publish_edited_view(saved).unwrap();
        other.sync_shared();
        assert_eq!(other.path(), Some(destination.as_path()));
        assert!(!owner.marker_dirty() && !other.marker_dirty());
        assert!(Arc::ptr_eq(owner.data.as_ref().unwrap(), other.data.as_ref().unwrap()));
    }

    #[test]
    fn shared_publications_keep_latest_comments_write_status_and_remap_selection_by_uid() {
        let mut owner = selection_view();
        let shared = owner.shared_file().unwrap();
        let mut other = View::default();
        other.show_shared_file(shared.clone(), 2, 1.0, 0.0);
        other.select(2, false);
        other.select(0, true);
        let selection = other.edit_selection_state();
        other.pb.set_frame(7, 0.0);
        let comment = comments::Comment { id: 9, markers: vec![], start: 2, end: 3, comment_type: String::new(), text: "latest".into() };
        owner.comments.list.apply(&comments::Event::of(comments::Kind::Add, &comment, String::new(), String::new()));
        owner.comments.pending = 1;
        owner.comments.pending_since = Some(Instant::now());
        assert!(owner.publish_shared_file());
        other.comments.pending = 88;
        assert!(!other.publish_shared_file(), "stale local metadata cannot erase a newer Comment");
        assert_eq!(other.comments.list.get(9), Some(&comment));
        assert_eq!(other.comments.pending, 1);

        let revision = owner.edit_document.as_ref().unwrap().lock().unwrap().revision();
        let mut take = sample_take("selection.tak", &["A", "B", "C", "D"]);
        take.trajectories.rotate_left(1);
        let mut updated = ViewData::build(take);
        updated.edit_revision = Some(revision);
        owner.edit_refresh = Some(revision);
        owner.publish_edited_view(updated).unwrap();
        assert!(other.sync_shared_file(&shared), "the old handle reads its latest publication");
        assert!(Arc::ptr_eq(owner.data.as_ref().unwrap(), other.data.as_ref().unwrap()));
        assert_eq!(other.edit_selection_state(), selection);
        assert_eq!(other.pb.frame, 7);
        assert_eq!(other.data.as_ref().unwrap().markers[other.primary.unwrap()].uid, [1; 16]);
        assert!(!other.sync_shared(), "an unchanged generation needs no cache or selection work");

        owner.comments.pending = 0;
        owner.comments.pending_since = None;
        owner.comments.saved_at = Some(2.0);
        owner.comments.target = Some(Ok("saved.takm".into()));
        assert!(owner.publish_shared_file());
        other.comments.id_pool = Some(400..=500);
        other.sync_shared();
        assert_eq!(other.comments.pending, 0);
        assert_eq!(other.comments.saved_at, Some(2.0));
        assert_eq!(other.comments.target, Some(Ok("saved.takm".into())));
        assert!(other.comments.id_pool.is_none(), "a save to another path invalidates old reservations");
    }

    #[test]
    fn closing_shared_load_owner_hands_off_receivers_and_does_not_cancel_analog() {
        use tak_core::tak::{AnalogChannel, AnalogDevice};
        let mut data = sample_data("shared.tak", &["M"]);
        data.analog_pending = true;
        let target = data.analog_target();
        let (tx, rx) = channel();
        let mut owner = View::default();
        owner.show(data, rx, 1, 1.0, 0.0);
        let cancellation = owner.load_cancel.clone();
        let (lazy_tx, lazy_rx) = channel();
        owner.lazy_job_rx = Some(lazy_rx);
        let shared = owner.shared_file().unwrap();
        let mut other = View::default();
        other.show_shared_file(shared, 2, 1.0, 0.0);
        assert!(other.analog_rx.is_none());
        assert!(owner.transfer_shared_loads(&mut other));
        assert!(owner.analog_rx.is_none() && other.analog_rx.is_some());
        drop_later(owner.clear());
        assert!(!cancellation.load(Ordering::SeqCst));
        lazy_tx.send((None, None)).unwrap();
        let analog = vec![AnalogDevice { channels: vec![AnalogChannel { name: "AnalogChannel0".into(), kind: "Analog".into(), dim: 1, frames: vec![0].into(), values: vec![42.0].into() }] }];
        tx.send(LoadMsg::Analog(Ok(AnalogPart::prepare(analog, None, &target)), 1.0)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while other.analog_work_pending() {
            other.poll_analog();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!other.data.as_ref().unwrap().analog_pending);
        assert_eq!(other.data.as_ref().unwrap().take.analog[0].channels[0].values, [42.0]);
        assert!(other.lazy_job_rx.as_ref().unwrap().try_recv().is_ok(), "deferred EMG ownership survived");
    }

    #[test]
    fn selection_order_survives_ctrl_deselection_reselection_and_undo_snapshots() {
        let mut view = selection_view();
        view.select(2, false);
        view.select(0, true);
        view.select(1, true);
        let before = view.selection();
        assert_eq!(before.order, vec![2, 0, 1]);
        view.select(1, true);
        assert_eq!((view.ordered_selection(), view.primary), (vec![2, 0], Some(0)));
        view.select(1, true);
        view.select(0, true);
        assert_eq!((view.ordered_selection(), view.primary), (vec![2, 1], Some(1)));
        view.set_selection(before.clone());
        assert_eq!(view.selection(), before);
        view.select(1, true);
        assert_eq!(view.primary, Some(0), "undo restores the preceding click, not the greatest index");
        view.clear_selection();
        assert!(view.ordered_selection().is_empty());
        assert!(view.selection_order.is_empty());
    }

    #[test]
    fn box_selection_orders_new_markers_deterministically_and_its_primary_last() {
        let mut view = selection_view();
        view.select(2, false);
        view.select(0, true);
        view.select_many(vec![3, 1, 3], Some(3), true);
        assert_eq!(view.ordered_selection(), vec![2, 0, 1, 3]);
        view.select_many(vec![1, 0], Some(0), true);
        assert_eq!(view.ordered_selection(), vec![2, 1, 3, 0]);
        view.select(0, true);
        assert_eq!(view.primary, Some(3));
        view.select_many(vec![2, 0, 1], Some(0), false);
        assert_eq!(view.ordered_selection(), vec![1, 2, 0]);
        view.select_many(vec![], None, false);
        assert!(view.ordered_selection().is_empty());
        assert_eq!(view.primary, None);
    }

    #[test]
    fn spots_keep_selection_chronology_and_old_settings_restore_primary_last() {
        let mut view = selection_view();
        view.select(2, false);
        view.select(0, true);
        view.select(1, true);
        let saved = view.spot().unwrap();
        assert_eq!(saved.selected, vec!["C", "A", "B"]);
        let decoded: Spot = ron::from_str(&ron::to_string(&saved).unwrap()).unwrap();
        let mut restored = selection_view();
        restored.apply_spot(&decoded, 0.0);
        assert_eq!(restored.ordered_selection(), vec![2, 0, 1]);
        restored.select(1, true);
        assert_eq!(restored.primary, Some(0));

        let old = Spot { selected: vec!["A".into(), "B".into(), "C".into()], primary: Some("B".into()), ..saved };
        restored.apply_spot(&old, 0.0);
        assert_eq!(restored.ordered_selection(), vec![0, 2, 1]);
        restored.select(1, true);
        assert_eq!(restored.primary, Some(2));
    }

    #[test]
    fn uid_selection_keeps_chronology_when_an_edited_view_reorders_markers() {
        let mut view = selection_view();
        view.select(2, false);
        view.select(0, true);
        view.select(1, true);
        let selection = view.edit_selection_state();
        assert_eq!(selection.order, vec![[3; 16], [1; 16], [2; 16]]);
        let revision = view.edit_document.as_ref().unwrap().lock().unwrap().revision();
        view.edit_refresh = Some(revision);
        view.edit_selection = Some(selection.clone());
        let mut take = sample_take("selection.tak", &["A", "B", "C", "D"]);
        take.trajectories.rotate_left(1);
        let mut rebuilt = ViewData::build(take);
        rebuilt.edit_revision = Some(revision);
        view.publish_edited_view(rebuilt).unwrap();
        assert_eq!(view.edit_selection_state(), selection);
        let primary = view.primary.unwrap();
        view.select(primary, true);
        assert_eq!(view.edit_selection_state().primary, Some([1; 16]));
    }

    #[test]
    fn later_user_selection_keeps_its_order_after_a_pending_view_rebuild() {
        let mut view = selection_view();
        view.select(2, false);
        let revision = view.edit_document.as_ref().unwrap().lock().unwrap().revision();
        view.edit_refresh = Some(revision);
        view.edit_selection = Some(view.edit_selection_state());
        view.select(1, false);
        view.select(0, true);
        let expected = view.edit_selection_state();
        let mut rebuilt = sample_data("selection.tak", &["A", "B", "C", "D"]);
        rebuilt.edit_revision = Some(revision);
        view.publish_edited_view(rebuilt).unwrap();
        assert_eq!(view.edit_selection_state(), expected);
        assert_eq!(view.ordered_selection(), vec![1, 0]);
    }

    #[test]
    fn comment_selection_restores_an_order_and_markerless_comments_keep_click_order() {
        let mut view = selection_view();
        let comment = comments::Comment { id: 1, markers: vec!["C".into(), "A".into(), "B".into()], start: 2, end: 3, comment_type: String::new(), text: String::new() };
        let markerless = comments::Comment { id: 2, markers: vec![], ..comment.clone() };
        view.comments.list = Comments::from_events(&[
            comments::Event::of(comments::Kind::Add, &comment, String::new(), String::new()),
            comments::Event::of(comments::Kind::Add, &markerless, String::new(), String::new()),
        ]);
        view.select_comment(1);
        assert_eq!(view.primary, Some(2), "comment selection retains its established primary");
        assert_eq!(view.ordered_selection(), vec![0, 1, 2]);
        view.select(2, true);
        assert_eq!(view.primary, Some(1));
        view.select(0, false);
        view.select(2, true);
        view.select(1, true);
        view.select_comment(2);
        assert_eq!(view.ordered_selection(), vec![0, 2, 1]);
    }

    #[test]
    fn editable_document_does_not_prevent_late_analog_attachment() {
        use tak_core::tak::{AnalogChannel, AnalogDevice};
        let mut data = sample_data("a.tak", &["M"]);
        let target = data.analog_target();
        data.analog_pending = true;
        let (tx, rx) = channel();
        let mut view = View::default();
        view.show(data, rx, 7, 1.0, 0.0);
        assert_eq!(Arc::strong_count(view.data.as_ref().unwrap()), 1);
        assert!(view.edit_document.is_some());
        let analog = vec![AnalogDevice { channels: vec![AnalogChannel { name: "AnalogChannel0".into(), kind: "Analog".into(), dim: 1, frames: vec![0].into(), values: vec![42.0].into() }] }];
        tx.send(LoadMsg::Analog(Ok(AnalogPart::prepare(analog, None, &target)), 1.0)).unwrap();
        view.poll_analog();
        assert!(!view.data.as_ref().unwrap().analog_pending);
        assert_eq!(view.data.as_ref().unwrap().take.analog[0].channels[0].values, vec![42.0]);
    }

    #[test]
    fn shared_display_does_not_drop_a_late_analog_result_or_overwrite_newer_markers() {
        use tak_core::tak::{AnalogChannel, AnalogDevice};
        let mut data = sample_data("shared.tak", &["M"]);
        data.analog_pending = true;
        let target = data.analog_target();
        let (_tx, rx) = channel();
        let mut view = View::default();
        view.show(data, rx, 77, 1.0, 0.0);
        let shared = view.data.as_ref().unwrap().clone();
        view.analog_rx = None;
        let analog = vec![AnalogDevice { channels: vec![AnalogChannel { name: "AnalogChannel0".into(), kind: "Analog".into(), dim: 1, frames: vec![0].into(), values: vec![42.0].into() }] }];
        view.queue_analog(AnalogPart::prepare(analog, None, &target));
        assert!(view.analog_merge.is_some(), "shared data must be merged on a worker");
        // Simulate a marker view finishing while that worker owns the previous Arc.
        let mut newer = sample_data("shared.tak", &["NEWER"]);
        newer.analog_pending = true;
        drop_later(view.data.replace(Arc::new(newer)));
        let deadline = Instant::now() + Duration::from_secs(5);
        while view.analog_work_pending() {
            view.poll_analog();
            assert!(Instant::now() < deadline, "late result must be retried against the new view");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(view.data.as_ref().unwrap().markers[0].name, "NEWER");
        assert!(!view.data.as_ref().unwrap().analog_pending);
        assert_eq!(view.data.as_ref().unwrap().take.analog[0].channels[0].values, vec![42.0]);
        assert!(shared.analog_pending && shared.take.analog.is_empty(), "other readers retain their immutable snapshot");
    }

    #[test]
    fn marker_rebuild_started_before_emg_cannot_revert_the_completed_load() {
        use tak_core::tak::{AnalogChannel, AnalogDevice};
        let mut data = sample_data("edited.tak", &["M"]);
        data.emg_pending = true;
        let mut stale = data.clone();
        let (_tx, rx) = channel();
        let mut view = View::default();
        view.show(data, rx, 3, 1.0, 0.0);
        let revision = view.edit_document.as_ref().unwrap().lock().unwrap().revision();
        view.edit_refresh = Some(revision);
        stale.edit_revision = Some(revision);
        let analog = vec![AnalogDevice { channels: vec![AnalogChannel { name: "AnalogChannel0".into(), kind: "Analog".into(), dim: 1, frames: vec![0].into(), values: vec![42.0].into() }] }];
        let part = AnalogPart::prepare(analog, None, &view.data.as_ref().unwrap().analog_target());
        view.queue_analog(part);
        assert!(matches!(view.publish_edited_view(stale), Err(EditError::StaleRevision)));
        assert_eq!(view.edit_refresh, Some(revision), "normal refresh must retry from the completed analog stage");
        assert!(!view.data.as_ref().unwrap().emg_pending);
        assert_eq!(view.data.as_ref().unwrap().take.analog[0].channels[0].values, vec![42.0]);
        let mut fresh = view.data.as_ref().unwrap().as_ref().clone();
        fresh.edit_revision = Some(revision);
        view.publish_edited_view(fresh).unwrap();
        assert!(view.edit_refresh.is_none());
        assert_eq!(view.data.as_ref().unwrap().take.analog[0].channels[0].values, vec![42.0]);
    }

    #[test]
    fn actual_local_emg_is_requested_once_and_cancellation_reopen_rebuilds_deferred_job() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../input/static/000000_S01_static.tak");
        if !path.exists() { return; }
        let ctx = egui::Context::default();
        let mut view = View::default();
        let load = |view: &mut View, id| {
            view.start_load(&ctx, path.clone(), None);
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(loaded) = view.poll_markers() {
                    let (data, _) = loaded.result.unwrap();
                    drop_later(view.show(data, loaded.rx, id, 1.0, 0.0));
                    break;
                }
                assert!(Instant::now() < deadline); std::thread::sleep(Duration::from_millis(1));
            }
            while view.analog_work_pending() {
                view.poll_analog();
                assert!(Instant::now() < deadline); std::thread::sleep(Duration::from_millis(1));
            }
            view.request_emg(&ctx); // collect the owned deferred job without requesting samples
        };
        load(&mut view, 1);
        let nonplate = view.data.as_ref().unwrap().devices.iter().position(|d| d.plate.is_none()).unwrap();
        assert!(view.data.as_ref().unwrap().emg_pending);
        assert!(view.data.as_ref().unwrap().take.analog[nonplate].channels.iter().all(|c| c.values.is_empty()));
        assert!(!view.emg_requested && view.lazy_job.is_some());
        // Closing before access discards the old job. A reopen creates a new owned lazy job.
        drop_later(view.clear());
        load(&mut view, 2);
        assert!(view.lazy_job.is_some() && !view.emg_requested);
        view.devices.request_emg = true;
        view.request_emg(&ctx);
        assert!(view.emg_requested && view.emg_rx.is_some() && view.lazy_job.is_none());
        for _ in 0..20 { view.devices.request_emg = true; view.request_emg(&ctx); }
        let deadline = Instant::now() + Duration::from_secs(10);
        while view.analog_work_pending() {
            view.poll_analog();
            assert!(Instant::now() < deadline); std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!view.data.as_ref().unwrap().emg_pending);
        assert!(view.data.as_ref().unwrap().take.analog[nonplate].channels.iter().any(|c| !c.values.is_empty()));
        assert_eq!(view.load_steps.iter().filter(|(name, _)| *name == "emg").count(), 1);
        for _ in 0..10 { view.devices.request_emg = true; view.request_emg(&ctx); view.poll_analog(); }
        assert_eq!(view.load_steps.iter().filter(|(name, _)| *name == "emg").count(), 1);
        // Closing during a fresh request cannot let its late result change the next file.
        drop_later(view.clear()); load(&mut view, 3);
        view.devices.request_emg = true; view.request_emg(&ctx);
        assert!(view.emg_rx.is_some());
        drop_later(view.clear()); load(&mut view, 4);
        assert!(view.data.as_ref().unwrap().emg_pending && !view.emg_requested && view.lazy_job.is_some());
    }

    #[test]
    fn cancelling_a_reopen_does_not_apply_its_edit_selection_to_another_file() {
        let mut view = View::default();
        let (_tx, rx) = channel();
        view.show(sample_data("a.tak", &["M"]), rx, 1, 1.0, 0.0);
        view.select(0, false);
        view.restore_edit_document = view.edit_document.clone();
        view.restore_edit_selection = Some(view.edit_selection_state());
        view.hold_load("b.tak".into(), None);
        assert!(view.restore_edit_document.is_none());
        assert!(view.restore_edit_selection.is_none());
        let (_tx, rx) = channel();
        view.show(sample_data("b.tak", &["M"]), rx, 2, 1.0, 0.0);
        assert!(view.selected.is_empty());
    }

    #[test]
    fn the_range_is_ordered_kept_inside_the_file_and_cleared_by_a_new_file() {
        let mut v = View::default();
        let (_tx, rx) = channel();
        v.show(sample_data("a.tak", &["M"]), rx, 7, 1.0, 0.0);
        v.set_range(8, 3);
        assert_eq!(v.range, Some((3, 8)), "start <= end");
        v.set_range(-4, 20);
        assert_eq!(v.range, Some((0, 9)), "inside frames 0–9");
        v.pb.set_frame(5, 0.0);
        v.range = None;
        v.range_end_here();
        assert_eq!(v.range, Some((5, 5)));
        // Another file in this view: no range (the new file has its own frames).
        let (_tx, rx) = channel();
        v.show(sample_data("b.tak", &["M"]), rx, 8, 1.0, 0.0);
        assert_eq!(v.range, None);
        // The same file read again (a remembered spot) also starts without a range.
        v.set_range(1, 2);
        let (_tx, rx) = channel();
        v.restore = Some(spot("b.tak", 6));
        v.show(sample_data("b.tak", &["M"]), rx, 9, 1.0, 0.0);
        assert_eq!(v.range, None);
        assert_eq!(v.pb.frame, 6);
    }

    #[test]
    fn the_comment_file_arrives_after_the_markers_and_belongs_to_the_shown_file() {
        use tak_core::comments::{Event, Kind};
        let ev = |id| Event {
            time: "t".into(),
            kind: Kind::Add,
            id,
            markers: vec!["M".into()],
            start: 1,
            end: 2,
            comment_type: String::new(),
            text: String::new(),
            shown_a: "1".into(),
            shown_b: String::new(),
            file_a: String::new(),
            file_b: String::new(),
            operation: None,
            expected: None,
        };
        let mut v = View::default();
        let (tx, rx) = channel();
        let (_mtx, mrx) = channel();
        // The file is still being read: the comment file waits for its markers.
        v.loading = Some((PathBuf::from("a.tak"), mrx));
        v.comments_rx = Some(rx);
        let parsed = tak_core::comments::Parsed { events: vec![ev(1), ev(2)], skipped: 1, cut: true };
        tx.send(CommentsLoad { target: Ok(PathBuf::from("a.comments.csv")), parsed: Some(parsed), label: None }).unwrap();
        v.poll_comments();
        assert!(v.comments.target.is_none() && v.busy(), "not before the markers are shown");
        v.loading = None;
        let (_tx, arx) = channel();
        v.show(sample_data("a.tak", &["M"]), arx, 7, 1.0, 0.0);
        assert!(v.comments.target.is_none(), "a new file starts without comments until its file is read");
        v.poll_comments();
        assert_eq!(v.comments.list.len(), 2);
        assert_eq!((v.comments.skipped, v.comments.target.clone()), (2, Some(Ok(PathBuf::from("a.comments.csv")))), "the cut-off line counts as not read");
        assert!(v.comments_rx.is_none() && !v.busy());
        // Another file in this view: its comments go.
        let (_tx, arx) = channel();
        v.show(sample_data("b.tak", &["M"]), arx, 8, 1.0, 0.0);
        assert!(v.comments.list.is_empty() && v.comments.target.is_none());
        // A TAK: no Comments, its CSV is not read; a `.takm`: its tail, the place comments are
        // written to, a CSV next to it ignored.
        let _temp = tak_core::test_support::TempDir::new("tak_viewer_comment_load");
        let dir = _temp.join(format!("tak_viewer_comment_load_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.tak"), tak_core::sample_tak::sample_tak()).unwrap();
        let made = tak_core::takm::convert(&dir.join("x.tak"), None).unwrap();
        std::fs::write(dir.join("x.comments.csv"), "time,kind\r\n2026-11-14T10:00:00+09:00,add,1,M,1,2,,,1,,x.tak|1,\r\n").unwrap();
        let load = CommentsLoad::read(&dir.join("x.tak"));
        assert_eq!((load.target, load.parsed, load.label), (Err(TAK_NO_COMMENTS.to_owned()), None, None));
        let load = CommentsLoad::read(&made.out);
        let fixed = tak_core::takm::read_index_path(&made.out).unwrap().fixed_len;
        assert_eq!((load.target, load.label), (Ok(made.out.clone()), Some(format!("x.takm|{fixed}"))));
        assert!(load.parsed.unwrap().events.is_empty(), "the CSV next to it is not read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_read_again_comes_back_to_its_spot() {
        let mut v = View::default();
        let mut s = spot("a.tak", 6);
        s.selected = vec!["LKNE".into(), "GONE".into()];
        s.primary = Some("LKNE".into());
        v.restore = Some(s);
        let (_tx, rx) = channel();
        let old = v.show(sample_data("a.tak", &["RKNE", "LKNE"]), rx, 7, 1.0, 0.0);
        assert!(old.is_none());
        assert_eq!(v.pb.frame, 6);
        assert_eq!(v.cam.distance, 4.0);
        assert_eq!(v.selected, BTreeSet::from([1]), "found by name, missing names skipped");
        assert_eq!(v.primary, Some(1));
        assert_eq!(v.labels.scroll_to, Some(1), "Labels scrolls to it");
        assert!(!v.fit_pending);
        assert!(v.restore.is_none());
        // Without a remembered spot a new file is framed.
        let (_tx, rx) = channel();
        let old = v.show(sample_data("b.tak", &["X"]), rx, 8, 1.0, 0.0);
        assert_eq!(old.map(|d| d.file_name()), Some("a.tak".into()));
        assert!(v.fit_pending && v.selected.is_empty() && v.pb.frame == 0);
        assert_eq!(v.spot().map(|s| s.path), Some("b.tak".into()));
    }

    fn spot(path: &str, frame: i64) -> Spot {
        Spot {
            path: path.into(),
            frame,
            pivot: [0.1, 0.9, -0.2],
            yaw: 0.5,
            pitch: 0.3,
            distance: 4.0,
            fov_y: 0.8,
            camera_mode: Default::default(),
            selected: vec!["LKNE".into(), "RKNE".into()],
            primary: Some("RKNE".into()),
            plate: None,
        }
    }

    #[test]
    fn the_chosen_plate_is_remembered_by_name_and_old_spots_without_it_still_read() {
        let mut v = View::default();
        let (_tx, rx) = channel();
        v.show(sample_with_plates("a.tak", &["LKNE"]), rx, 7, 1.0, 0.0);
        v.select(0, false);
        v.toggle_plate(1);
        let s = v.spot().unwrap();
        assert_eq!(s.plate.as_deref(), Some("P2"));
        // The file again: the plate comes back with the markers.
        v.restore = Some(s.clone());
        let (_tx, rx) = channel();
        v.show(sample_with_plates("a.tak", &["LKNE"]), rx, 8, 1.0, 0.0);
        assert_eq!((v.plate, v.primary), (Some(1), Some(0)));
        v.toggle_plate(1);
        assert_eq!(v.plate, None, "choosing the chosen plate again un-chooses it");
        // A file without that plate: nothing is chosen.
        v.restore = Some(s.clone());
        let (_tx, rx) = channel();
        v.show(sample_data("a.tak", &["LKNE"]), rx, 9, 1.0, 0.0);
        assert_eq!(v.plate, None);
        // Settings text from before v0.11.0 (no plate) still reads.
        let text = ron::to_string(&s).unwrap();
        assert!(text.contains(",plate:Some(\"P2\")"), "{text}");
        let old = text.replace(",plate:Some(\"P2\")", "");
        let back: Spot = ron::from_str(&old).expect("a spot without the plate still reads");
        assert_eq!(back, Spot { plate: None, ..s });
    }

    #[test]
    fn remembers_newest_first_one_per_file_at_most_fifty() {
        let mut list = Vec::new();
        for k in 0..60 {
            remember(&mut list, spot(&format!("f{k}.tak"), k));
        }
        assert_eq!(list.len(), MAX_SPOTS);
        assert_eq!(list[0].path, "f59.tak");
        assert!(recall(&list, Path::new("f5.tak")).is_none(), "oldest ones are dropped");
        remember(&mut list, spot("f30.tak", 7));
        assert_eq!(list.len(), MAX_SPOTS);
        assert_eq!(list[0], spot("f30.tak", 7));
        assert_eq!(list.iter().filter(|s| s.path == "f30.tak").count(), 1);
        assert_eq!(recall(&list, Path::new("f30.tak")).map(|s| s.frame), Some(7));
    }

    #[test]
    fn spot_survives_settings_text() {
        let s = spot(r"C:\data\a.tak", 120);
        let back: Spot = ron::from_str(&ron::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn bad_camera_in_a_spot_is_replaced_by_framing() {
        let mut v = View::default();
        let mut s = spot("a.tak", 0);
        s.distance = f32::NAN;
        v.apply_spot(&s, 0.0);
        assert!(v.fit_pending);
        assert_eq!(v.cam, Camera::default());
        let mut v = View::default();
        v.apply_spot(&spot("a.tak", 0), 0.0);
        assert!(!v.fit_pending);
        assert_eq!(v.cam.distance, 4.0);
    }
}
