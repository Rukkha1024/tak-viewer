//! The 3D views: up to `MAX_VIEWS` files side by side (like VS Code editor groups). A `View`
//! holds everything about one open file: its data, camera, frame, selection and its load.
//! Views are not linked: each has its own frame and camera (user: no frame matching).
//! A view may carry a second file laid over its 3D (`Overlay`, plan step 7): not a view of its
//! own, it follows that view's frame and camera and is drawn as white rings only.
//! `Spot` = where a file was being viewed (frame, camera, selected marker names, chosen force
//! plate). The app keeps the latest `MAX_SPOTS` in the settings and puts a file back there when
//! it is opened again.
//! Layout of the views (which slots are on screen, where) is in `ui/dock.rs`.

use crate::camera::Camera;
use crate::ui::panels::{self, BlockCache, DevicesState, HeatCache, LabelsState, LaneCache, PlateCache};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tak_core::comments::{self, Comments};
use tak_core::geom::{v3, Vec3};
use tak_core::playback::Playback;
use tak_core::view_data::{AnalogPart, ViewData};
use tak_core::edit::{DocumentSnapshot, EditDocument, EditError, EditPatch, EditSelection, LabelKey, Revision};

/// Shared only between the view and layout history, never with the `Arc<ViewData>` used by analog.
pub type EditSession = Arc<Mutex<EditDocument>>;
/// Captured by the initial marker reader before it opens the file.
pub type SourceStamp = Arc<Mutex<Option<(PathBuf, u64, Option<SystemTime>)>>>;

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
    pub armed_label: Option<LabelKey>,
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
pub fn remember(list: &mut Vec<Spot>, spot: Spot) {
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
    if let Some(old) = old {
        std::thread::spawn(move || drop(old));
    }
}

/// The second file laid over a view's 3D (plan step 7, Explorer "이 화면에 얹기"): its markers
/// are drawn as white rings at the view's frame with the view's camera. Display only: no
/// selection, Labels, graphs or comments of its own. Accepted only when its frame count and
/// frame rate are the view's (`overlay_mismatch`).
pub struct Overlay {
    pub path: PathBuf,
    pub data: ViewData,
}

/// A second file being read for a view's overlay (markers only; no force plates, no comments).
pub struct OverlayLoad {
    pub path: PathBuf,
    pub rx: Receiver<LoadMsg>,
    /// Put back by undo / redo: its arrival is no new action (`app_undo.rs`).
    pub quiet: bool,
    pub started: Instant,
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

/// Read a file's markers on a background thread for an overlay (no force plates, no comment
/// file). Counts as a marker load, so force plates of other files wait for it.
fn read_markers(ctx: &egui::Context, path: PathBuf) -> Receiver<LoadMsg> {
    let (tx, rx) = channel();
    let ctx2 = ctx.clone();
    MARKER_LOADS.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(move || {
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let fast = tak_core::tak::fast_read::read_take_fast(&path, false).map_err(|e| e.to_string())?;
            let t = Instant::now();
            let data = ViewData::build(fast.take);
            let mut steps = fast.times;
            steps.push(("view", ms_since(t)));
            Ok((data, steps))
        }))
        .unwrap_or_else(|_| Err("읽는 중 내부 오류(panic)가 났어요.".to_owned()));
        MARKER_LOADS.fetch_sub(1, Ordering::SeqCst);
        let _ = tx.send(LoadMsg::Markers(read));
        ctx2.request_repaint();
    });
    rx
}

/// A finished marker load, handed to the app (`TakViewerApp::finish_load`).
pub struct Loaded {
    pub path: PathBuf,
    pub result: Result<(ViewData, Vec<(&'static str, f64)>), String>,
    pub rx: Receiver<LoadMsg>,
}

pub struct View {
    pub data: Option<Arc<ViewData>>,
    /// Lazy, per-file marker document; preparing raw records never delays markers-first open.
    pub edit_document: Option<EditSession>,
    pub restore_edit_document: Option<EditSession>,
    pub restore_edit_selection: Option<EditSelection>,
    /// Selected Comment restored with a reversible close/open, after its markers arrive.
    pub restore_picked: Option<Option<u64>>,
    /// Latest desired marker revision; older worker results must not be published.
    pub edit_refresh: Option<Revision>,
    pub edit_selection: Option<EditSelection>,
    pub armed_label: Option<LabelKey>,
    pub source_stamp: Option<SourceStamp>,
    /// Typed remote address. The path inside ViewData is an opaque memory identity only.
    pub remote: Option<crate::ssh::RemoteLocation>,
    pub remote_warning: Option<String>,
    pub remote_loading: Option<(u64, crate::ssh::RemoteLocation, PathBuf)>,
    /// Unique for each file shown in this app (egui ids, Labels reset, graph cache).
    pub file_id: usize,
    pub cam: Camera,
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
    /// Lanes of the selected marker's graph and of the chosen plate's graph.
    pub lanes: LaneCache,
    pub plate_lanes: PlateCache,
    /// Red marks of this view's timeline (playback bar).
    pub heat: HeatCache,
    /// File being read; its markers are not here yet.
    pub loading: Option<(PathBuf, Receiver<LoadMsg>)>,
    /// Force plates / EMG of the shown file still coming: (file_id, messages).
    pub analog_rx: Option<(usize, Receiver<LoadMsg>)>,
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
            data: None,
            edit_document: None,
            restore_edit_document: None,
            restore_edit_selection: None,
            restore_picked: None,
            edit_refresh: None,
            edit_selection: None,
            armed_label: None,
            source_stamp: None,
            remote: None,
            remote_warning: None,
            remote_loading: None,
            file_id: 0,
            cam: Camera::default(),
            pb: Playback::new(0, 0, 100.0),
            selected: BTreeSet::new(),
            selection_order: Vec::new(),
            primary: None,
            picked: None,
            plate: None,
            range: None,
            comments: CommentState::default(),
            fresh: false,
            restore_range: None,
            comments_rx: None,
            labels: LabelsState::default(),
            devices: DevicesState::default(),
            lanes: LaneCache::default(),
            plate_lanes: PlateCache::default(),
            heat: HeatCache::default(),
            loading: None,
            analog_rx: None,
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
    pub fn marker_dirty(&self) -> bool {
        self.edit_document.as_ref().is_some_and(|session| session.lock().map_or(true, |doc| doc.is_dirty()))
    }

    /// The file on screen.
    pub fn path(&self) -> Option<&Path> {
        self.data.as_ref().map(|d| d.take.path.as_path())
    }

    /// The file being read, else the file on screen.
    pub fn target(&self) -> Option<&Path> {
        self.remote_loading.as_ref().map(|(_, _, p)| p.as_path()).or_else(|| self.loading.as_ref().map(|(p, _)| p.as_path())).or_else(|| self.path())
    }

    /// Markers, the comment file, force plates or the overlay's markers still coming.
    pub fn busy(&self) -> bool {
        self.remote_loading.is_some() || self.loading.is_some() || self.analog_rx.is_some() || self.comments_rx.is_some() || self.overlay_load.is_some()
    }

    /// The laid-over file on screen.
    pub fn overlay_path(&self) -> Option<&Path> {
        self.overlay.as_ref().map(|o| o.path.as_path())
    }

    /// The laid-over file being read, else the one on screen (what counts for "at most 4 files").
    pub fn overlay_target(&self) -> Option<&Path> {
        self.overlay_load.as_ref().map(|l| l.path.as_path()).or_else(|| self.overlay_path())
    }

    /// Both laid-over files of this view (on screen and being read): no file twice.
    pub fn overlay_paths(&self) -> impl Iterator<Item = &Path> {
        self.overlay_path().into_iter().chain(self.overlay_load.as_ref().map(|l| l.path.as_path()))
    }

    /// Read `path` for this view's overlay (markers only, background thread). The overlay on
    /// screen stays until the new one arrives and fits.
    pub fn start_overlay(&mut self, ctx: &egui::Context, path: PathBuf, quiet: bool) {
        let rx = read_markers(ctx, path.clone());
        self.overlay_load = Some(OverlayLoad {
            path,
            rx,
            quiet,
            started: Instant::now(),
            #[cfg(test)]
            held: None,
        });
    }

    /// Tests: like `start_overlay`, but the test sends the markers through `held`.
    #[cfg(test)]
    pub fn hold_overlay(&mut self, path: PathBuf, quiet: bool) {
        let (tx, rx) = channel();
        self.overlay_load = Some(OverlayLoad { path, rx, quiet, started: Instant::now(), held: Some(tx) });
    }

    /// The overlay's markers, once they are in and this view shows its own file (not while
    /// that is being read: the two are compared on arrival). (file, data or why not, quiet).
    pub fn poll_overlay(&mut self) -> Option<(PathBuf, Result<ViewData, String>, bool)> {
        if self.loading.is_some() || self.data.is_none() {
            return None;
        }
        let msg = match self.overlay_load.as_ref()?.rx.try_recv() {
            Ok(LoadMsg::Markers(r)) => r.map(|(d, _)| d),
            Ok(LoadMsg::Analog(..)) | Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("읽기가 중간에 끝났어요".to_owned()),
        };
        let load = self.overlay_load.take()?;
        if msg.is_ok() {
            self.overlay_ms = Some(ms_since(load.started));
        }
        Some((load.path, msg, load.quiet))
    }

    /// Take the overlay off (and stop reading one): both files shown again. Returns its data,
    /// to be freed off the drawing thread.
    pub fn take_overlay(&mut self) -> Option<ViewData> {
        self.overlay_load = None;
        self.shown = [true; 2];
        self.overlay.take().map(|o| o.data)
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
        drop_later(self.restore_edit_document.take());
        self.restore_edit_selection = None;
        self.restore_picked = None;
        let (tx, rx) = channel();
        let (comments_tx, comments_rx) = channel();
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
                if let Ok(meta) = std::fs::metadata(&p) {
                    if let Ok(mut initial) = stamp.lock() {
                        *initial = Some((p.clone(), meta.len(), meta.modified().ok()));
                    }
                }
                let fast = tak_core::tak::fast_read::read_take_fast(&p, false).map_err(|e| e.to_string())?;
                let t = Instant::now();
                let mut data = ViewData::build(fast.take);
                data.analog_pending = fast.analog.is_some();
                let mut steps = fast.times;
                steps.push(("view", ms_since(t)));
                Ok((data, steps, fast.analog))
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
                job.run().map(|(analog, warning)| AnalogPart::prepare(analog, warning, &target)).map_err(|e| e.to_string())
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
        self.load_cancel.store(true, Ordering::SeqCst);
        drop_later(self.restore_edit_document.take());
        self.restore_edit_selection = None;
        self.restore_picked = None;
        self.remote_loading = None;
        self.loading = None;
        self.analog_rx = None;
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
        self.markers_ms = self.open_started.map(ms_since);
        Some(Loaded { path, result: msg, rx })
    }

    /// Show a freshly read file: new playback, no selection, the remembered spot if any (else
    /// framed). Returns the previous data, to be freed off the drawing thread.
    pub fn show(&mut self, data: ViewData, rx: Receiver<LoadMsg>, file_id: usize, speed: f64, now: f64) -> Option<Arc<ViewData>> {
        self.show_shared(Arc::new(data), rx, file_id, speed, now)
    }

    pub fn show_shared(&mut self, data: Arc<ViewData>, rx: Receiver<LoadMsg>, file_id: usize, speed: f64, now: f64) -> Option<Arc<ViewData>> {
        self.remote = None;
        self.remote_warning = None;
        self.remote_loading = None;
        let mut pb = Playback::new(data.start_frame, data.end_frame, data.take.frame_rate);
        pb.speed = speed;
        self.pb = pb;
        self.selected.clear();
        self.selection_order.clear();
        self.primary = None;
        self.picked = self.restore_picked.take().flatten();
        self.plate = None;
        self.range = None;
        // Its own comments come from the loading thread (`poll_comments`).
        self.comments = CommentState::default();
        self.fresh = true;
        self.file_id = file_id;
        let path = &data.take.path;
        let restored = self.restore_edit_document.take().filter(|session| session.lock().ok().is_some_and(|doc| doc.source_path() == path || doc.save_path() == Some(path.as_path())));
        let restored_selection = if restored.is_some() { self.restore_edit_selection.take() } else { self.restore_edit_selection = None; None };
        let session = restored.unwrap_or_else(|| Arc::new(Mutex::new(EditDocument::unloaded(path.clone()))));
        self.edit_refresh = session.lock().ok().and_then(|doc| {
            (doc.snapshot().is_loaded() && (doc.is_dirty() || doc.save_path().is_some_and(|saved| saved != path))).then(|| doc.revision())
        });
        drop_later(self.edit_document.replace(session));
        self.edit_selection = restored_selection;
        self.armed_label = None;
        self.analog_rx = data.analog_pending.then_some((file_id, rx));
        self.labels.reset(&data, file_id);
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
    }

    /// Force plates / EMG arrived (or failed: then a warning, like `read_take`).
    pub fn poll_analog(&mut self) {
        let Some((id, rx)) = &self.analog_rx else { return };
        let msg = match rx.try_recv() {
            Ok(m) => Some(m),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => None,
        };
        let id = *id;
        self.analog_rx = None;
        let (result, took) = match msg {
            Some(LoadMsg::Analog(r, took)) => (r, took),
            _ => (Err("읽기가 중간에 끝났어요".to_owned()), 0.0),
        };
        self.load_steps.push(("analog", took));
        self.analog_ms = self.open_started.map(ms_since);
        if let Some(d) = self.data.as_mut().filter(|_| id == self.file_id).and_then(Arc::get_mut) {
            match result {
                Ok(part) => d.attach_prepared(part),
                Err(e) => d.attach_analog(Vec::new(), Some(format!("analog channels not loaded: {e}"))),
            }
        }
    }

    /// Where this view is looking now (None without a file).
    pub fn spot(&self) -> Option<Spot> {
        let d = self.data.as_ref()?;
        let name = |m: usize| d.markers.get(m).map(|m| m.name.clone());
        Some(Spot {
            path: d.take.path.to_string_lossy().into_owned(),
            frame: self.pb.frame,
            pivot: [self.cam.pivot.x, self.cam.pivot.y, self.cam.pivot.z],
            yaw: self.cam.yaw,
            pitch: self.cam.pitch,
            distance: self.cam.distance,
            fov_y: self.cam.fov_y,
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
        let cam = Camera {
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
        old.data
    }

    /// Commit or undo a prepared patch; the expensive view rebuild is requested separately.
    pub fn apply_marker_patch(&mut self, patch: &EditPatch, redo: bool) -> Result<(), EditError> {
        let session = self.edit_document.as_ref().ok_or(EditError::NotLoaded)?;
        let mut document = session.lock().map_err(|_| EditError::Other("편집 문서를 열 수 없어요".into()))?;
        document.apply(patch, redo)?;
        let revision = document.revision();
        drop(document);
        let selection = if redo { patch.selection_after.clone() } else { patch.selection_before.clone() };
        self.apply_uid_selection(&selection);
        self.edit_refresh = Some(revision);
        self.edit_selection = Some(selection);
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

    fn apply_uid_selection(&mut self, selection: &EditSelection) {
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
        if self.analog_rx.is_some() || self.data.as_ref().is_some_and(|d| d.analog_pending) { return None; }
        let desired = self.edit_refresh?;
        let document = self.edit_document.as_ref()?.lock().ok()?;
        (document.revision() == desired).then(|| document.snapshot())
    }

    /// Publish a worker-built view only for the latest document revision. Reset all marker
    /// caches together and retire the old buffers off-thread; the view keeps its file identity.
    pub fn publish_edited_view(&mut self, data: ViewData) -> Result<(), EditError> {
        if self.analog_rx.is_some() || self.data.as_ref().is_some_and(|d| d.analog_pending) { return Err(EditError::Other("아날로그를 읽는 중이에요".into())); }
        let expected = self.edit_refresh.ok_or(EditError::StaleRevision)?;
        let document = self.edit_document.as_ref().ok_or(EditError::NotLoaded)?.lock().map_err(|_| EditError::Other("편집 문서를 열 수 없어요".into()))?;
        if document.revision() != expected || data.edit_revision != Some(expected) { return Err(EditError::StaleRevision); }
        if data.take.path != document.source_path() && document.save_path() != Some(data.take.path.as_path()) { return Err(EditError::WrongDocument); }
        drop(document);
        let filter = self.labels.filter.clone();
        let folds = self.data.as_ref().map(|old| panels::fold_keys(old, &self.labels.collapsed)).unwrap_or_default();
        let old_groups: std::collections::HashSet<_> = self.data.as_ref().map(|old| (0..old.groups.len())
            .map(|i| panels::FoldKey::group(old, i)).collect()).unwrap_or_default();
        self.labels.reset(&data, self.file_id);
        self.labels.filter = filter;
        let new_defaults: Vec<_> = self.labels.collapsed.iter().copied()
            .filter(|&i| !old_groups.contains(&panels::FoldKey::group(&data, i))).collect();
        self.labels.collapsed = panels::fold_indices(&data, &folds);
        self.labels.collapsed.extend(new_defaults);
        // Preserve a later user selection if it replaced the patch's pending selection.
        let selection = self.edit_selection.take().unwrap_or_else(|| self.uid_selection());
        drop_later(Some(std::mem::take(&mut self.lanes)));
        drop_later(Some(std::mem::take(&mut self.heat)));
        drop_later(self.data.replace(Arc::new(data)));
        self.apply_uid_selection(&selection);
        self.edit_refresh = None;
        Ok(())
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

    pub fn clear_selection(&mut self) {
        self.edit_selection = None;
        self.selected.clear();
        self.selection_order.clear();
        self.primary = None;
    }

    /// The selection now (markers and the selected comment).
    pub fn selection(&self) -> Selection {
        Selection { selected: self.selected.clone(), order: self.ordered_selection(), primary: self.primary, comment: self.picked, armed_label: self.armed_label }
    }

    /// Put a selection back (undo / redo) the way a click leaves it: Labels scrolls to the
    /// primary marker; the 3D, panes and graphs read `selected` / `primary` every frame.
    pub fn set_selection(&mut self, s: Selection) {
        self.edit_selection = None;
        self.selected = s.selected;
        self.selection_order = s.order;
        self.primary = s.primary;
        self.picked = s.comment;
        self.armed_label = s.armed_label;
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
    use super::*;

    fn selection_view() -> View {
        let mut view = View::default();
        let (_tx, rx) = channel();
        view.show(sample_data("selection.tak", &["A", "B", "C", "D"]), rx, 1, 1.0, 0.0);
        view
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
        let analog = vec![AnalogDevice { channels: vec![AnalogChannel { name: "AnalogChannel0".into(), kind: "Analog".into(), dim: 1, frames: vec![0], values: vec![42.0] }] }];
        tx.send(LoadMsg::Analog(Ok(AnalogPart::prepare(analog, None, &target)), 1.0)).unwrap();
        view.poll_analog();
        assert!(!view.data.as_ref().unwrap().analog_pending);
        assert_eq!(view.data.as_ref().unwrap().take.analog[0].channels[0].values, vec![42.0]);
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
        let dir = std::env::temp_dir().join(format!("tak_viewer_comment_load_{}", std::process::id()));
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
