//! Application shell: menu bar with the side toggles, keyboard, pane layout (VS Code
//! arrangement; every pane can still be dragged anywhere, `ui/dock.rs`) and status bar. Up to
//! `MAX_VIEWS` 3D views, one file each (`views.rs`), each with its own playback bar under it;
//! the side panes, status bar, title, keys and captures follow the active view.

use crate::camera::Camera;
use crate::capture::{self, CapturePlan, CaptureRun, Fit, FrameSpec, ViewPreset};
use crate::instances::Registration;
use crate::shot::{self, AutoShot};
use crate::ui::comment_list::{self, Field, ListAction};
use crate::ui::dock::{self, Pane, Side};
use crate::ui::explorer::{self, ExplorerState, WsAction};
use crate::ui::graph;
use crate::ui::graph_float::{self, GraphBox, Kind, LaneSet};
use crate::ui::panels::{self, Action, CommentBar, DeckAction};
use crate::ui::scene::{self, SceneOptions};
use crate::ui::theme;
use crate::views::{self, Change, Loaded, Spot, View, MAX_VIEWS};

#[path = "app_key_focus.rs"]
pub(crate) mod key_focus;

#[path = "app_undo.rs"]
mod undo;
use undo::History;
#[path = "app_overlay.rs"]
mod overlay;
#[path = "app_overlay_dirty.rs"]
mod overlay_dirty;
#[path = "app_takm.rs"]
mod takm_save;
#[path = "app_editors.rs"]
mod editors;
#[path = "app_comment_multi.rs"]
mod comment_multi;
#[path = "app_editor_batch.rs"]
mod editor_batch;
#[path = "app_ssh.rs"]
mod remote_ui;
#[path = "app_marker_edit.rs"]
mod marker_edit;
#[path = "app_tabs.rs"]
mod layout_tabs;
#[path = "app_layout_tabs.rs"]
mod layout_controller;
#[path = "app_hidden_work.rs"]
mod hidden_work;
use crate::workspace::{self, AddRoot, Workspace, MAX_ROOTS};
use eframe::egui::{self, Key, Modifiers, RichText};
use egui_dock::{DockArea, DockState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tak_core::comments::{self, Comment, Event, Kind as EventKind};
use tak_core::geom::v3;

const MAX_RECENT: usize = 10;
/// Periodic settings save while this is the only open window (eframe's default).
const AUTO_SAVE: Duration = Duration::from_secs(30);
/// How often a window looks for other open windows (`instances.rs`).
const OTHERS_CHECK: Duration = Duration::from_secs(5);
/// Settings-file key that changes on every save, so every save writes the file (eframe writes
/// only changed files). With several windows the last one to close then always wins.
const SAVED_KEY: &str = "tak_viewer_saved";
/// A window opened while another is open moves this far (points) from the saved place,
/// so it does not sit exactly on top of the first one.
const NEW_WINDOW_SHIFT: f32 = 40.0;
/// Filters of the "파일 열기" dialog: the first (shown at start) holds both kinds the viewer
/// opens, then each kind alone, then everything.
const OPEN_FILTERS: [(&str, &[&str]); 4] =
    [("TAK · TAKM (.tak, .takm)", &["tak", "takm"]), ("TAK (.tak)", &["tak"]), ("TAKM (.takm)", &["takm"]), ("모든 파일", &["*"])];

/// The persistent, small part of a layout tab. Remote file paths are never saved.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct SavedLayoutTab {
    name: String,
    layout: dock::EditorLayout,
    views: Vec<SavedLayoutView>,
    active: u8,
    graph_boxes: Vec<GraphBox>,
    plate_boxes: Vec<GraphBox>,
    plate_channels: Vec<LaneSet>,
}

impl Default for SavedLayoutTab {
    fn default() -> Self {
        Self {
            name: String::new(), layout: dock::EditorLayout::Empty, views: Vec::new(), active: 0,
            graph_boxes: vec![GraphBox::default(); MAX_VIEWS],
            plate_boxes: vec![GraphBox::default(); MAX_VIEWS],
            plate_channels: vec![LaneSet::default(); MAX_VIEWS],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SavedLayoutView {
    slot: u8,
    path: String,
    spot: Option<Spot>,
}

/// Hidden tabs own live views, not copies of the underlying file data.
struct LayoutRuntime {
    views: [View; MAX_VIEWS],
    marker_ui: [marker_edit::MarkerUi; MAX_VIEWS],
    layout: dock::EditorLayout,
    active: usize,
    graph_boxes: Vec<GraphBox>,
    plate_boxes: Vec<GraphBox>,
    plate_channels: Vec<LaneSet>,
}

/// A recently closed view retains its controls and any unfinished worker. Shared files still
/// point at one document and one underlying data buffer.
struct ClosedLayoutView {
    view: View,
    marker_ui: marker_edit::MarkerUi,
    slot: usize,
    layout: dock::EditorLayout,
}

impl Default for LayoutRuntime {
    fn default() -> Self {
        Self {
            views: std::array::from_fn(|_| View::default()),
            marker_ui: std::array::from_fn(|_| marker_edit::MarkerUi::default()),
            layout: dock::EditorLayout::Empty,
            active: 0,
            graph_boxes: vec![GraphBox::default(); MAX_VIEWS],
            plate_boxes: vec![GraphBox::default(); MAX_VIEWS],
            plate_channels: vec![LaneSet::default(); MAX_VIEWS],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub scene: SceneOptions,
    pub recent: Vec<String>,
    /// SSH platform and Python command only; never remote paths, contents or connection state.
    pub ssh_platforms: BTreeMap<String, String>,
    pub ssh_python: BTreeMap<String, crate::ssh::PythonSetting>,
    /// Old single workspace folder (before 2026-11-10). Moved into `workspace_roots` once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Saved workspace folders (at most `MAX_ROOTS`), in screen order.
    pub workspace_roots: Vec<String>,
    /// Saved folders that were expanded (only these are read at start).
    pub workspace_open: Vec<String>,
    /// Display names the user gave saved folders (folder path -> name).
    pub workspace_names: BTreeMap<String, String>,
    pub recent_workspaces: Vec<String>,
    /// Pane layout (dragged tabs). None = default layout.
    pub layout: Option<DockState<Pane>>,
    /// `dock::LAYOUT_VERSION` the layout was saved with (0 = before v0.7.0).
    pub layout_version: u32,
    /// Panes hidden by the side toggles (they come back where they were).
    pub hidden: dock::Hidden,
    /// Files in the 3D views at the last save: (view slot, path). Opened again at start.
    pub views: Vec<(u8, String)>,
    /// The view that was active (side panes, keys, status bar).
    pub active_view: u8,
    /// `None` is the old single-layout schema. `Some([])` means every tab was closed.
    layout_tabs: Option<Vec<SavedLayoutTab>>,
    active_layout_tab: usize,
    /// Where each file was being viewed, newest first (at most `views::MAX_SPOTS`).
    pub file_memory: Vec<Spot>,
    pub speed: f64,
    /// The selected marker's graph floats over its 3D view (`graph_float.rs`); false = it is
    /// in the GRAPH pane (or nowhere, when that pane is closed).
    pub graph_float: bool,
    /// How dark the floating graph's background is, in percent: 0 = lines only, 100 = nothing
    /// shows through (menu "보기 → 설정…").
    pub graph_opacity: u8,
    /// Place, size and folded state of each view's floating graph, by view slot.
    pub graph_boxes: Vec<GraphBox>,
    /// The same for each view's floating force plate graph (the chosen plate). Not in
    /// settings saved before v0.11.0.
    pub plate_boxes: Vec<GraphBox>,
    /// Lanes (Fx … Mz) each view's force plate graph shows, by view slot (its "줄" list). Not
    /// in settings saved before v0.12.0 (then Fx · Fy · Fz).
    pub plate_channels: Vec<LaneSet>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            scene: SceneOptions::default(),
            recent: vec![],
            ssh_platforms: BTreeMap::new(),
            ssh_python: BTreeMap::new(),
            workspace: None,
            workspace_roots: vec![],
            workspace_open: vec![],
            workspace_names: BTreeMap::new(),
            recent_workspaces: vec![],
            layout: None,
            layout_version: 0,
            hidden: dock::Hidden::default(),
            views: vec![],
            active_view: 0,
            layout_tabs: None,
            active_layout_tab: 0,
            file_memory: vec![],
            speed: 1.0,
            graph_float: true,
            graph_opacity: graph_float::DEFAULT_OPACITY,
            graph_boxes: vec![GraphBox::default(); MAX_VIEWS],
            plate_boxes: vec![GraphBox::default(); MAX_VIEWS],
            plate_channels: vec![LaneSet::default(); MAX_VIEWS],
        }
    }
}

impl Settings {
    /// The pane layout and hidden sides to start with. Older valid layouts migrate in place:
    /// rename the marker list, merge its duplicates, and add Edit Tools before version 3.
    /// While the GRAPH pane is open the graph does not float.
    pub fn take_layout(&mut self) -> (DockState<Pane>, dock::Hidden) {
        let old = self.layout_version < 2;
        let needs_labeling = self.layout_version < 3;
        self.layout_version = dock::LAYOUT_VERSION;
        let mut hidden = std::mem::take(&mut self.hidden);
        let saved = self.layout.take().map(|mut d| {
            dock::merge_label_panels(&mut d, &mut hidden);
            dock::sanitize(d)
        });
        let mut dock = match saved {
            Some(mut d) => {
                if needs_labeling { dock::migrate_labeling(&mut d, &mut hidden); }
                d
            },
            None => dock::default_layout(),
        };
        dock::merge_label_panels(&mut dock, &mut hidden);
        hidden.clean(&dock);
        if dock::has(&dock, Pane::Graph) {
            self.graph_float = false;
        } else if old {
            self.graph_float = true;
        }
        (dock, hidden)
    }

    /// Settings saved before multi-folder workspaces: the one folder becomes the first
    /// saved folder (expanded, as it was). Every view slot has a place for each floating graph.
    pub fn migrate(&mut self) {
        if let Some(old) = self.workspace.take() {
            if self.workspace_roots.is_empty() {
                self.workspace_roots = vec![old.clone()];
                self.workspace_open = vec![old];
            }
        }
        self.graph_boxes.resize(MAX_VIEWS, GraphBox::default());
        self.plate_boxes.resize(MAX_VIEWS, GraphBox::default());
        self.plate_channels.resize(MAX_VIEWS, LaneSet::default());
        self.graph_opacity = self.graph_opacity.min(100);
    }
}

/// Height of the status bar at the bottom of the window.
const STATUS_H: f32 = 22.0;
/// The status bar says "saved" this long after a comment was written (seconds).
const SAVED_NOTE_S: f64 = 3.0;
/// The note after a delete, undo or redo stays this long in the status bar (seconds).
const CHANGE_NOTE_S: f64 = 4.0;

/// A comment being made by holding `M` (v0.15.0): which view and file, the frame and the
/// selected markers when `M` went down, and the view's `Playback::wraps` then (a wrap past the
/// last frame closes the range there).
#[derive(Clone, Debug, PartialEq)]
struct Hold {
    slot: usize,
    file_id: usize,
    start: i64,
    markers: Vec<String>,
    wraps: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct TypeStrip {
    slot: usize,
    file_id: usize,
    id: u64,
    /// Where it was drawn last (presses inside it do not close it).
    rect: egui::Rect,
    /// Its buttons as drawn last (tests click them).
    buttons: [egui::Rect; 3],
    /// Shown for a picture only (`--type-strip`, a command run that writes nothing).
    shot: bool,
}

/// The side toggles in screen order, with their menu text.
const SIDE_TOGGLES: [(Side, &str); 3] =
    [(Side::Left, "왼쪽 패널 전체  (Ctrl+B)"), (Side::Bottom, "아래 패널 전체  (Ctrl+J)"), (Side::Right, "오른쪽 패널 전체  (Ctrl+Alt+B)")];

/// Toggle button of one side (VS Code layout control): a small window outline whose left /
/// bottom / right part is filled while panes are docked on that side.
fn side_toggle(ui: &mut egui::Ui, side: Side, on: bool) -> egui::Response {
    use egui::{pos2, Rect, Stroke};
    let (r, resp) = ui.allocate_exact_size(egui::vec2(26.0, 20.0), egui::Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, 3.0, egui::Color32::from_white_alpha(28));
    }
    let b = Rect::from_center_size(r.center(), egui::vec2(15.0, 12.0));
    let color = if on { theme::TAB_TEXT_ACTIVE } else { theme::TAB_TEXT_DIM };
    let part = match side {
        Side::Left => Rect::from_min_max(b.min, pos2(b.left() + 5.0, b.bottom())),
        Side::Right => Rect::from_min_max(pos2(b.right() - 5.0, b.top()), b.max),
        Side::Bottom => Rect::from_min_max(pos2(b.left(), b.bottom() - 4.5), b.max),
    };
    if on {
        p.rect_filled(part, 1.0, color);
    } else {
        let edge = match side {
            Side::Left => [part.right_top(), part.right_bottom()],
            Side::Right => [part.left_top(), part.left_bottom()],
            Side::Bottom => [part.left_top(), part.right_top()],
        };
        p.line_segment(edge, Stroke::new(1.0_f32, color));
    }
    p.rect_stroke(b, 1.5, Stroke::new(1.0_f32, color));
    resp
}

/// Use up presses of Ctrl + `key` with no Shift or Alt (exact, unlike egui's `consume_key`,
/// which also takes Ctrl+Shift + `key`). True when there was one.
fn ctrl_key(ctx: &egui::Context, key: Key) -> bool {
    ctx.input_mut(|i| {
        let mut found = false;
        i.events.retain(|e| match e {
            egui::Event::Key { key: k, pressed: true, modifiers: m, .. } if *k == key && m.command && !m.shift && !m.alt => {
                found = true;
                false
            }
            _ => true,
        });
        found
    })
}

/// Consume a pressed key only when all shortcut modifiers match exactly.
fn exact_key(ctx: &egui::Context, key: Key, control: bool, shift: bool, alt: bool) -> bool {
    ctx.input_mut(|input| {
        let mut found = false;
        input.events.retain(|event| match event {
            egui::Event::Key { key: actual, pressed: true, modifiers, .. }
                if *actual == key
                    && (modifiers.command || modifiers.ctrl) == control
                    && modifiers.shift == shift
                    && modifiers.alt == alt => {
                found = true;
                false
            }
            _ => true,
        });
        found
    })
}

/// Ctrl+W uses physical Ctrl alone; egui's platform `command` also accepts Cmd/Meta.
fn close_view_key(ctx: &egui::Context) -> bool {
    ctx.input_mut(|input| {
        let mut found = false;
        input.events.retain(|event| match event {
            egui::Event::Key { key: Key::W, pressed: true, modifiers, .. }
                if modifiers.ctrl && !modifiers.shift && !modifiers.alt && !modifiers.mac_cmd => {
                found = true;
                false
            }
            _ => true,
        });
        found
    })
}

fn push_recent(list: &mut Vec<String>, s: String) {
    list.retain(|r| r != &s);
    list.insert(0, s);
    list.truncate(MAX_RECENT);
}

use views::drop_later;

pub struct TakViewerApp {
    /// The 3D views by slot; the ones on screen are the `Pane::View` tabs in `dock`.
    views: [View; MAX_VIEWS],
    marker_ui: [marker_edit::MarkerUi; MAX_VIEWS],
    /// The active tab's runtime is in `views` / `marker_ui`; its model payload is None.
    /// Hidden tabs own their payload and are never drawn or ticked.
    layout_tabs: layout_tabs::TabModel<Option<LayoutRuntime>, ClosedLayoutView>,
    hidden_work: hidden_work::HiddenWork,
    editor_cache: std::cell::RefCell<editors::EditorCache>,
    layout_strip: layout_tabs::TabStrip,
    layout_time: f64,
    pending_shared_promotions: Vec<PathBuf>,
    /// Only the newest admitted request. Existing views own their identities independently.
    identity_candidate: std::cell::RefCell<Option<(PathBuf, PathBuf)>>,
    marker_sessions: std::collections::HashMap<tak_core::edit::DocumentId, marker_edit::PreparedSession>,
    marker_click: Option<(usize, tak_core::tak::Uid)>,
    marker_delete: Option<usize>,
    dirty_prompt: Option<marker_edit::DirtyPrompt>,
    overlay_dirty: Option<overlay_dirty::OverlayDirty>,
    overlay_marker_ui: std::collections::HashMap<tak_core::edit::DocumentId, marker_edit::MarkerUi>,
    overlay_owner_scope: bool,
    layout_restore_files: Vec<overlay_dirty::LayoutRestoreFile>,
    tab_close_origin: Option<(u64, Option<u64>)>,
    exit_approved: bool,
    restoring_layout: bool,
    /// Slot of the active view (blue edge; side panes, keys, status bar, captures).
    active: usize,
    /// Counter for `View::file_id` (unique across views).
    next_file_id: usize,
    /// Tab that had the dock focus last frame: a click on a view's tab makes it active.
    focused_tab: Option<Pane>,
    /// An Explorer file let go over a 3D view this frame: (view, where in it, file).
    file_drop: Option<(usize, dock::DropZone, PathBuf)>,
    /// Window title last sent.
    title_shown: String,
    /// Pane area of the last frame (where new views are placed).
    dock_area: egui::Rect,
    settings: Settings,
    error: Option<String>,
    show_help: bool,
    show_warnings: bool,
    /// "설정" window (menu "보기 → 설정…").
    show_settings: bool,
    /// A view's floating graph asked for the GRAPH pane this frame (applied after drawing).
    graph_to_panel: bool,
    auto: Option<AutoShot>,
    workspace: Workspace,
    ssh: crate::ssh::SshManager,
    ssh_sources: std::collections::HashMap<PathBuf, crate::ssh::RemoteLocation>,
    ssh_cache: std::collections::VecDeque<remote_ui::Cached>,
    ssh_reads: std::collections::HashMap<u64, remote_ui::RemoteRead>,
    ssh_note: Option<(String, f64)>,
    ssh_demo_applied: bool,
    explorer: ExplorerState,
    /// Pane layout (saved into `settings.layout` on save).
    dock: DockState<Pane>,
    /// Panes taken off the screen by the side toggles (saved into `settings.hidden`).
    hidden: dock::Hidden,
    /// Short message window ("알림"), e.g. the saved-folder list is full.
    notice: Option<String>,
    /// "경로 입력" dialog text; Some = dialog open.
    path_input: Option<String>,
    /// "경로 입력" dialog: optional display name for a folder, and focus-once flag.
    path_name: String,
    path_focus: bool,
    /// Running capture (`--capture` or menu "화면 캡처 저장").
    capture: Option<CaptureRun>,
    /// Finished menu capture: (folder, report lines) shown in a small window.
    capture_done: Option<(Option<PathBuf>, Vec<String>)>,
    /// Command line runs (`--screenshot`, `--capture`, `--bench`) never save settings and never
    /// write comment files (they still read them).
    no_persist: bool,
    /// Appends comment events on its own thread (started with the first comment).
    comment_writer: Option<comments::Writer>,
    comment_registration: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    comment_registration_error: Option<String>,
    comment_waiting: Vec<(usize, PathBuf, Vec<Event>)>,
    comment_failed: Vec<(usize, PathBuf, Event)>,
    comment_batches: Vec<comment_multi::PendingBatch>,
    editor_selection: crate::ui::open_editors::EditorSelection,
    editor_batch: editor_batch::EditorBatchState,
    comment_outstanding: usize,
    exit_waiting_comments: bool,
    /// A playback bar's comment request this frame: (view, what), applied after drawing.
    deck_action: Option<(usize, DeckAction)>,
    /// `M` is being held: a comment from its start frame to the frame where it goes up.
    hold: Option<Hold>,
    /// Ctrl+Z / Ctrl+Y for every action, one history for the whole app (`app_undo.rs`, v0.16.0).
    history: History,
    /// COMMENTS pane (v0.17.0): how it is sorted (one for the app, this session only; an undo
    /// action), its row order (made again only on change), the cell being edited (a double
    /// click, v0.17.1; Enter saves, Esc cancels), the comment to scroll to ((file, id): just
    /// made or its band block clicked), and what a click in it asked this frame (applied after
    /// drawing).
    comment_sort: comment_list::Sort,
    comment_order: comment_list::Order,
    comment_edit: Option<comment_list::Edit>,
    comment_scroll: Option<(usize, u64)>,
    comment_out: comment_list::ListOut,
    /// The last single click on a COMMENTS row (v0.27.3): (view slot, file id, comment id, the
    /// number its selection action gets, `History::pushed`). A double click on `#` right after
    /// it makes that selection and the go-to one action (`reopen_row_select`).
    row_select: Option<(usize, usize, u64, u64)>,
    /// The type strip after a new comment (step 5): three type buttons by the `Comment`
    /// button until the next action elsewhere; and the type clicked on it this frame (applied
    /// after drawing).
    type_strip: Option<TypeStrip>,
    strip_pick: Option<&'static str>,
    /// Why a file could not be laid over a view (plan step 7, plain Korean) and when: shown in
    /// the status bar for a few seconds (`app_overlay.rs`).
    overlay_note: Option<(String, f64)>,
    /// A click on a view's overlay name tag this frame: (view, which file, ✕), applied after
    /// drawing.
    overlay_click: Option<(usize, usize, bool)>,
    /// "`.takm`으로 저장" running in the background (`app_takm.rs`), one at a time.
    takm_job: Option<takm_save::Job>,
    /// Its result, or why it or a Comment on a TAK cannot be done (plain Korean) and when:
    /// shown in the status bar for a few seconds.
    takm_note: Option<(String, f64)>,
    /// Tests: the next conversion thread waits until this is sent to (or dropped).
    #[cfg(test)]
    takm_gate: Option<std::sync::mpsc::Receiver<()>>,
    /// Tests: the COMMENTS rows drawn in the last frame (comment id, row).
    #[cfg(test)]
    comment_rows: Vec<(u64, egui::Rect)>,
    /// Tests: the items of the open type list drawn in the last frame (value, item).
    #[cfg(test)]
    type_items: Vec<(String, egui::Rect)>,
    /// Tests: files are not read by a thread; the test hands each one in (`tests::arrive`), so
    /// nothing arrives at a time the test does not choose.
    #[cfg(test)]
    hold_loads: bool,
    /// A command line run failed (load, capture or PNG save); `--headless` exits with 1.
    pub failed: bool,
    /// v0.27.0 speed fix: graph work was put off to later frames in the frame before
    /// (`graph::start_frame`), so a graph on screen is not complete yet; `--screenshot` waits.
    pub graph_put_off: bool,
    /// This window in the open-window list (`instances.rs`). None for command line runs.
    registration: Option<Registration>,
    /// Another viewer window is open: no periodic save; the last window to close saves.
    others_open: bool,
    /// When `others_open` was last checked.
    others_checked: Option<Instant>,
    /// Move the window a little once it exists (another window was open at start).
    shift_window: bool,
    /// Counts saves; part of the value under `SAVED_KEY`.
    saves: u64,
}

impl TakViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Vec<PathBuf>, auto: Option<AutoShot>, capture_plan: Option<CapturePlan>) -> Self {
        Self::build(&cc.egui_ctx, cc.storage, initial, auto, capture_plan)
    }

    /// A window the user works in (not a command line run): remember its open-window entry.
    /// `others_at_start` = another window was already open (this one moves aside once).
    pub fn set_registration(&mut self, registration: Option<Registration>, others_at_start: bool) {
        self.registration = registration;
        self.others_open = others_at_start;
        self.others_checked = Some(Instant::now());
        self.shift_window = others_at_start;
    }

    /// The window the user works in: WORKSPACE follows the disk (folder watcher thread,
    /// `workspace.rs`). Command runs never call this.
    pub fn start_folder_watch(&mut self) {
        self.workspace.start_watch();
        self.ssh.discover_hosts();
    }

    #[cfg(test)]
    pub fn workspace_watching(&self) -> bool {
        self.workspace.is_watching()
    }

    /// A run that only reads (`--bench`): no settings, no comment files written.
    pub fn set_command_run(&mut self) {
        self.no_persist = true;
    }

    /// Look for other open windows every few seconds; move a new window off the first one.
    fn watch_other_windows(&mut self, ctx: &egui::Context) {
        if let Some(r) = &self.registration {
            let now = Instant::now();
            if self.others_checked.map_or(true, |t| now.duration_since(t) >= OTHERS_CHECK) {
                self.others_open = r.others_alive();
                self.others_checked = Some(now);
            }
        }
        if self.shift_window {
            if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
                let to = rect.min + egui::vec2(NEW_WINDOW_SHIFT, NEW_WINDOW_SHIFT);
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(to));
                self.shift_window = false;
            }
        }
    }

    /// Shared by the window (`new`) and `--headless` (`headless.rs`, no storage).
    pub fn build(
        ctx: &egui::Context,
        storage: Option<&dyn eframe::Storage>,
        initial: Vec<PathBuf>,
        auto: Option<AutoShot>,
        capture_plan: Option<CapturePlan>,
    ) -> Self {
        theme::install_fonts(ctx);
        theme::apply(ctx);
        // Screenshot / capture runs use default settings so the picture is reproducible.
        let no_persist = auto.is_some() || capture_plan.is_some();
        let mut settings: Settings = if no_persist {
            Settings::default()
        } else {
            storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default()
        };
        settings.migrate();
        let (dock, hidden) = settings.take_layout();
        let settings_python = settings.ssh_python.iter().map(|(k,v)| (k.clone(),v.clone())).collect();
        let settings_ssh = settings.ssh_platforms.iter().map(|(k,v)| (k.clone(),v.clone())).collect();
        let mut app = TakViewerApp {
            views: std::array::from_fn(|_| View::default()),
            marker_ui: std::array::from_fn(|_| marker_edit::MarkerUi::default()),
            layout_tabs: layout_tabs::TabModel::new(None),
            hidden_work: hidden_work::HiddenWork::default(),
            editor_cache: Default::default(),
            layout_strip: layout_tabs::TabStrip::default(),
            layout_time: 0.0,
            pending_shared_promotions: Vec::new(),
            identity_candidate: Default::default(),
            marker_sessions: Default::default(),
            marker_click: None,
            marker_delete: None,
            dirty_prompt: None,
            overlay_dirty: None,
            overlay_marker_ui: Default::default(),
            overlay_owner_scope: false,
            layout_restore_files: Vec::new(),
            tab_close_origin: None,
            exit_approved: false,
            restoring_layout: false,
            active: 0,
            next_file_id: 0,
            focused_tab: None,
            file_drop: None,
            title_shown: String::new(),
            dock_area: dock::NOMINAL_AREA,
            settings,
            error: None,
            show_help: false,
            show_warnings: false,
            show_settings: false,
            graph_to_panel: false,
            auto,
            workspace: Workspace::new(ctx.clone()),
            ssh: { let mut ssh = crate::ssh::SshManager::new(ctx, settings_ssh); ssh.python_settings = settings_python; ssh },
            ssh_sources: Default::default(),
            ssh_cache: Default::default(),
            ssh_reads: Default::default(),
            ssh_note: None,
            ssh_demo_applied: false,
            explorer: ExplorerState::default(),
            dock,
            hidden,
            notice: None,
            path_input: None,
            path_name: String::new(),
            path_focus: false,
            capture: capture_plan.map(CaptureRun::new),
            capture_done: None,
            no_persist,
            comment_writer: None,
            comment_registration: None,
            comment_registration_error: None,
            comment_waiting: Vec::new(),
            comment_failed: Vec::new(),
            comment_batches: Vec::new(),
            editor_selection: Default::default(),
            editor_batch: Default::default(),
            comment_outstanding: 0,
            exit_waiting_comments: false,
            deck_action: None,
            hold: None,
            history: History::default(),
            comment_sort: comment_list::Sort::default(),
            comment_order: comment_list::Order::default(),
            comment_edit: None,
            comment_scroll: None,
            comment_out: comment_list::ListOut::default(),
            row_select: None,
            type_strip: None,
            strip_pick: None,
            overlay_note: None,
            overlay_click: None,
            takm_job: None,
            takm_note: None,
            #[cfg(test)]
            takm_gate: None,
            #[cfg(test)]
            comment_rows: Vec::new(),
            #[cfg(test)]
            type_items: Vec::new(),
            #[cfg(test)]
            hold_loads: false,
            failed: false,
            graph_put_off: false,
            registration: None,
            others_open: false,
            others_checked: None,
            shift_window: false,
            saves: 0,
        };
        // Saved folders come back; only the ones that were expanded are read (one level).
        for r in app.settings.workspace_roots.clone() {
            app.workspace.add_root(PathBuf::from(r));
        }
        for (r, name) in app.settings.workspace_names.clone() {
            app.workspace.set_name(Path::new(&r), &name);
        }
        for r in app.settings.workspace_open.clone() {
            let r = PathBuf::from(r);
            if app.workspace.roots().contains(&r) {
                app.explorer.expand(&mut app.workspace, &r);
            }
        }
        app.restore_layout_tabs(ctx);
        if app.capture.is_none() { dock::ensure_layout_strip(&mut app.dock, app.active as u8); }
        // Command line: a folder is added to the saved folders; files open directly, the first
        // in the active view, the others next to it (WSL paths OK).
        let args: Vec<PathBuf> = initial.iter().map(|p| workspace::resolve_path(&p.to_string_lossy())).collect();
        if let Some(d) = args.iter().find(|p| p.is_dir()).cloned() {
            app.open_workspace(d);
        }
        for (k, f) in args.into_iter().filter(|p| !p.is_dir()).enumerate() {
            if k == 0 {
                app.open(ctx, f);
            } else {
                app.open_side(ctx, f);
            }
        }
        // `--screenshot … --overlay <b.tak>`: the n-th one laid over the n-th view.
        if let Some(list) = app.auto.as_ref().map(|a| a.overlays.clone()).filter(|l| !l.is_empty()) {
            app.overlay_args(ctx, &list);
        }
        app
    }

    /// Views of the last session: each saved file opens again in its view (at its remembered
    /// spot). Views whose file is gone are taken out; an explicitly empty editor stays empty.
    fn restore_views(&mut self, ctx: &egui::Context) {
        let had_saved_files = !self.settings.views.is_empty();
        let saved: Vec<(u8, PathBuf)> = std::mem::take(&mut self.settings.views)
            .into_iter()
            .map(|(k, p)| (k, PathBuf::from(p)))
            .filter(|(k, p)| !remote_ui::is_remote_key(p) && dock::views(&self.dock).contains(k) && p.is_file())
            .collect();
        for k in dock::views(&self.dock) {
            if !saved.iter().any(|(s, _)| *s == k) && (had_saved_files || dock::views(&self.dock).len() > 1) {
                dock::remove_view(&mut self.dock, k);
            }
        }
        let open = dock::views(&self.dock);
        let want = self.settings.active_view;
        self.active = usize::from(if open.contains(&want) { want } else { open.first().copied().unwrap_or(0) });
        for (k, p) in saved {
            self.open_in(ctx, usize::from(k), p);
        }
        if !open.is_empty() { self.activate(self.active); }
    }

    /// Slots of the views on screen.
    fn open_views(&self) -> Vec<usize> {
        dock::views(&self.dock).into_iter().map(usize::from).collect()
    }

    fn view(&self) -> &View {
        &self.views[self.active]
    }

    fn view_mut(&mut self) -> &mut View {
        &mut self.views[self.active]
    }

    /// Make a view the active one (and its tab the focused one, so both agree).
    fn activate(&mut self, slot: usize) {
        if self.active != slot {
            self.active = slot;
            self.explorer.reveal = self.views[slot].target().map(Path::to_path_buf);
        }
        dock::focus_view(&mut self.dock, slot as u8);
        self.focused_tab = Some(Pane::View(slot as u8));
    }

    fn view_label(&self, slot: usize, open: &[usize]) -> Option<String> {
        let name = |p: &Path| p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some((_, location, _)) = &self.views[slot].remote_loading {
            return Some(format!("{} · {}", remote_ui::remote_name(location), location.host));
        }
        if let Some(location) = &self.views[slot].remote {
            return Some(format!("{} · {}", remote_ui::remote_name(location), location.host));
        }
        let path = self.views[slot].target()?;
        let same = open.iter().any(|&k| k != slot && self.views[k].target().is_some_and(|q| q != path && name(q) == name(path)));
        let folder = path.parent().and_then(Path::file_name).map(|s| s.to_string_lossy().into_owned());
        let mut label = match folder {
            Some(f) if same => format!("{} · {f}", name(path)),
            _ => name(path),
        };
        if self.views[slot].marker_dirty() { label.push('*'); }
        Some(label)
    }

    /// The view showing or reading this file, as its own file or laid over it (plan step 7: no
    /// file twice, overlays included).
    fn view_of(&self, path: &Path) -> Option<usize> {
        self.open_views().into_iter().find(|&k| self.views[k].target() == Some(path) || self.views[k].overlay_paths().any(|p| p == path))
    }

    /// Add a folder to the saved folders (at most `MAX_ROOTS`) and expand it.
    /// Returns false when the list is full (nothing changed, a notice is shown).
    fn open_workspace(&mut self, root: PathBuf) -> bool {
        if self.dirty_prompt.is_some() { return false; }
        if self.workspace.add_root(root.clone()) == AddRoot::Full {
            self.notice = Some(format!(
                "저장 폴더는 {MAX_ROOTS}개까지예요.\n왼쪽 Explorer에서 폴더 줄에 마우스를 올리고 ✕로 하나 뺀 뒤 다시 추가하세요.\n\n추가하려던 폴더:\n{}",
                root.display()
            ));
            return false;
        }
        push_recent(&mut self.settings.recent_workspaces, root.to_string_lossy().into_owned());
        if !dock::has(&self.dock, Pane::Explorer) {
            self.show_pane(Pane::Explorer);
        }
        self.explorer.filter.clear();
        // Lists that folder only; sub-folders are read when expanded.
        self.explorer.expand(&mut self.workspace, &root);
        self.explorer.reveal = self.current_path().map(Path::to_path_buf);
        true
    }

    /// Show a pane (menu "보기", a folder was added): at its first place unless it is open.
    fn show_pane(&mut self, pane: Pane) {
        self.hidden.forget(pane);
        dock::show(&mut self.dock, pane);
        self.activate(self.active);
    }

    /// Close a pane (menu "보기").
    fn hide_pane(&mut self, pane: Pane) {
        self.hidden.forget(pane);
        dock::hide(&mut self.dock, pane);
    }

    /// Where the selected marker's graph is: floating over the 3D views (the GRAPH pane
    /// closes) or back in the GRAPH pane below them.
    fn set_graph_float(&mut self, float: bool) {
        self.settings.graph_float = float;
        if float {
            self.hide_pane(Pane::Graph);
        } else {
            self.show_pane(Pane::Graph);
        }
    }

    /// Side toggle (menu bar buttons, Ctrl+B / Ctrl+J / Ctrl+Alt+B): hide everything docked on
    /// that side of the 3D views, or bring it back. The active view keeps the keyboard.
    fn toggle_side(&mut self, side: Side) {
        dock::toggle_side(&mut self.dock, &mut self.hidden, self.dock_area, side);
        self.activate(self.active);
    }

    /// Open whatever a path points to: folder -> saved folders, file -> take.
    fn open_any(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.dirty_prompt.is_some() { return; }
        if path.is_dir() {
            self.open_workspace(path);
        } else {
            self.open(ctx, path);
        }
    }

    /// "경로로 열기" window (Ctrl+L): empty path and name boxes, path box focused.
    fn open_path_dialog(&mut self) {
        self.path_input = Some(String::new());
        self.path_name.clear();
        self.path_focus = true;
    }

    fn pick_folder(&mut self) {
        let mut dialog = rfd::FileDialog::new();
        if let Some(last) = self.workspace.roots().last() {
            dialog = dialog.set_directory(last);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.open_workspace(dir);
        }
    }

    /// File of the active view.
    fn current_path(&self) -> Option<&Path> {
        self.view().path()
    }

    /// PageDown / PageUp: next / previous .tak or .takm in the same folder as the open file
    /// (the Explorer's name order: `a.tak`, `a.takm`, `b.tak`).
    fn step_file(&mut self, ctx: &egui::Context, delta: i64) {
        if self.view().remote.is_some() {
            self.ssh_note = Some(("원격 파일은 SSH 목록에서 골라 여세요.".into(), ctx.input(|i| i.time)));
            return;
        }
        let Some(cur) = self.current_path() else { return };
        let Some(files) = self.workspace.siblings(cur) else { return };
        let Some(k) = files.iter().position(|f| f.path == cur) else { return };
        let next = (k as i64 + delta).clamp(0, files.len() as i64 - 1) as usize;
        if next != k {
            let p = files[next].path.clone();
            self.open(ctx, p);
        }
    }

    /// Open a file in the active view. Another view may already show the same shared document.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.dirty_prompt.is_some() { return; }
        self.open_in(ctx, self.active, path);
    }

    /// Open a file in a new view next to the others (Ctrl+click, "옆 화면에 열기").
    fn open_side(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.dirty_prompt.is_some() { return; }
        if !self.allow_open_file(&path, None) { return; }
        // The first file goes into a default view, including after every view was closed.
        if self.open_views().is_empty() || (self.open_views().len() == 1 && self.view().target().is_none()) {
            self.open_in(ctx, self.active, path);
            return;
        }
        let Some(slot) = dock::free_slot(&self.dock) else {
            self.full_notice(&path);
            self.discard_identity_candidate();
            return;
        };
        dock::add_view(&mut self.dock, slot, self.dock_area);
        self.activate(usize::from(slot));
        self.open_in(ctx, usize::from(slot), path);
    }

    fn full_notice(&mut self, path: &Path) {
        self.notice = Some(format!(
            "한 탭에는 3D 화면을 {MAX_VIEWS}개까지 열 수 있어요.\n화면 이름표의 ✕로 하나 닫거나 화면 가운데에 놓아서 파일을 바꾸세요.\n\n열려던 파일:\n{}",
            path.display()
        ));
    }

    /// An Explorer file dropped on 3D view `slot`: its middle = open the file there, an edge =
    /// a new view on that side (VS Code editor drop). A file already open in a view is not
    /// opened twice: that view is chosen.
    fn drop_file(&mut self, ctx: &egui::Context, slot: usize, zone: dock::DropZone, path: PathBuf) {
        if self.dirty_prompt.is_some() { return; }
        let replacing = (zone == dock::DropZone::Center).then_some(slot);
        if !self.allow_open_file(&path, replacing) { return; }
        if self.open_views().is_empty() {
            self.open_in(ctx, slot, path);
            return;
        }
        let empty_start = self.open_views().len() == 1 && self.views[slot].target().is_none();
        if zone == dock::DropZone::Center || empty_start {
            self.activate(slot);
            self.open_in(ctx, slot, path);
            return;
        }
        let Some(new) = dock::free_slot(&self.dock) else {
            self.full_notice(&path);
            self.discard_identity_candidate();
            return;
        };
        if dock::add_view_at(&mut self.dock, new, slot as u8, zone) {
            self.activate(usize::from(new));
            self.open_in(ctx, usize::from(new), path);
        } else {
            self.discard_identity_candidate();
        }
    }

    /// Read a file into a view on a background thread (markers first, `views.rs`). The file
    /// the view showed is remembered where it was; the new one comes back to its own spot.
    fn open_in(&mut self, ctx: &egui::Context, slot: usize, path: PathBuf) {
        if self.dirty_prompt.is_some() { return; }
        let creating_tab = self.layout_tabs.active_id().is_none();
        if self.views[slot].target() == Some(path.as_path()) { self.discard_identity_candidate(); return; }
        if !self.allow_open_file(&path, Some(slot)) { return; }
        if self.save_destination_reserved(&path) {
            self.discard_identity_candidate();
            self.takm_note = Some(("Save in progress for this file.".into(), ctx.input(|i| i.time)));
            return;
        }
        let shared_elsewhere = self.views[slot].target().is_some_and(|old| self.file_has_other_view(slot, old));
        if !self.restoring_layout && self.guard_view_removal(slot, marker_edit::PendingTransition::Open { slot, path: path.clone() }) {
            self.discard_identity_candidate();
            return;
        }
        if shared_elsewhere {
            if self.comments_unsaved(slot) {
                self.discard_identity_candidate();
                self.notice = Some("Comment 저장을 마친 뒤 다시 열어 주세요.".into());
                return;
            }
            let needs_handoff = self.views[slot].owns_shared_file_work() || self.marker_ui[slot].has_pending_work();
            if !self.handoff_shared_owner(slot, false) && needs_handoff {
                self.discard_identity_candidate();
                self.notice = Some("읽기나 편집을 마친 뒤 다시 열어 주세요.".into());
                return;
            }
        }
        if self.views[slot].owns_shared_file_work() && self.has_other_overlay_for_primary(slot, false)
            && !self.handoff_primary_to_overlay(slot, false) {
            self.discard_identity_candidate();
            self.notice = Some("겹쳐 본 파일의 읽기나 저장을 마친 뒤 다시 열어 주세요.".into());
            return;
        }
        if self.views[slot].overlay_owner().is_some_and(View::owns_shared_file_work)
            && self.has_other_reference_for_overlay(slot, false) && !self.handoff_overlay_file_work(slot) {
            self.discard_identity_candidate();
            self.notice = Some("겹쳐 본 파일의 읽기를 마친 뒤 다시 열어 주세요.".into());
            return;
        }
        let previous_load = self.views[slot].loading.as_ref().map(|(path, _)| path.clone());
        let shared = self.find_shared_file(&path, slot);
        let wait_for_shared = shared.is_none() && self.file_is_loading_elsewhere(&path, slot);
        let retained = if shared.is_none() && !wait_for_shared { self.find_retained_document(&path) } else { None };
        self.cancel_pending_marker_job(slot);
        self.prepare_close_remote_view(slot);
        if let Some(location) = self.ssh_sources.get(&path).cloned() {
            self.discard_identity_candidate();
            self.open_remote_in(ctx, slot, location, true);
            return;
        }
        if remote_ui::is_remote_key(&path) {
            self.discard_identity_candidate();
            self.ssh_note = Some(("이 원격 파일은 SSH 목록에서 다시 여세요.".into(), ctx.input(|i| i.time)));
            return;
        }
        let pending_identity = self.known_identity(&path);
        if creating_tab { self.observe(true); }
        self.ensure_open_view(slot);
        self.remember(slot);
        // A file laid over this view goes with the file it was laid over (plan step 7).
        drop_later(self.views[slot].take_overlay());
        let restore = if self.no_persist { None } else { views::recall(&self.settings.file_memory, &path).cloned() };
        if let Some(shared) = shared {
            self.next_file_id += 1;
            self.views[slot].restore = restore;
            drop_later(self.views[slot].show_shared_file(shared, self.next_file_id, self.settings.speed, ctx.input(|i| i.time)));
            self.handoff_closed_file_work(slot);
            if let Some(old) = previous_load { self.promote_shared_waiter(ctx, &old); }
            self.error = None;
            self.record_implicit_file_open(creating_tab);
            self.discard_identity_candidate_for(&path);
            return;
        }
        if wait_for_shared {
            self.views[slot].wait_for_shared(path, restore);
            self.views[slot].set_pending_identity(pending_identity);
            if let Some(old) = previous_load { self.promote_shared_waiter(ctx, &old); }
            self.error = None;
            self.record_implicit_file_open(creating_tab);
            return;
        }
        #[cfg(test)]
        if self.hold_loads {
            self.views[slot].hold_load(path, restore);
            self.views[slot].set_pending_identity(pending_identity);
            self.views[slot].set_fresh_open_candidate(retained);
            self.error = None;
            self.record_implicit_file_open(creating_tab);
            return;
        }
        self.views[slot].start_load(ctx, path, restore);
        self.views[slot].set_pending_identity(pending_identity);
        self.views[slot].set_fresh_open_candidate(retained);
        if let Some(old) = previous_load { self.promote_shared_waiter(ctx, &old); }
        self.error = None;
        self.record_implicit_file_open(creating_tab);
    }

    /// A first file after closing every view starts with the default panel arrangement.
    /// Undo restoring an old layout already supplied its own views and never resets it here.
    fn ensure_open_view(&mut self, slot: usize) {
        if self.layout_tabs.active_id().is_none() { self.layout_tabs.add(None); }
        if self.open_views().is_empty() {
            dock::set_editor_layout(&mut self.dock, &dock::EditorLayout::Leaf { slots: vec![slot as u8], active: slot as u8 });
            self.activate(slot);
        }
    }

    /// Keep where a view is looking (files opened from the command line are not remembered).
    fn remember(&mut self, slot: usize) {
        self.cache_remote(slot);
        if !self.no_persist && self.views[slot].remote.is_none() {
            if let Some(spot) = self.views[slot].spot() {
                views::remember(&mut self.settings.file_memory, spot);
            }
        }
    }

    /// Close a view (tab ✕): remember its spot, stop its load, free its file.
    fn close_view(&mut self, slot: usize) {
        if self.dirty_prompt.is_some() { return; }
        let open_before = self.open_views();
        if !open_before.contains(&slot) { return; }
        if !self.restoring_layout && open_before.len() == 1 {
            if let Some(id) = self.layout_tabs.active_id() { self.close_layout_tab(id); }
            return;
        }
        let shared_elsewhere = self.views[slot].target().is_some_and(|path| self.file_has_other_view(slot, path));
        if !self.restoring_layout && self.guard_view_removal(slot, marker_edit::PendingTransition::CloseView { slot }) { return; }
        if !self.restoring_layout && shared_elsewhere {
            if self.comments_unsaved(slot) {
                self.notice = Some("Comment 저장을 마친 뒤 다시 닫아 주세요.".into());
                return;
            }
            let needs_handoff = self.views[slot].owns_shared_file_work() || self.marker_ui[slot].has_pending_work();
            if !self.handoff_shared_owner(slot, false) && needs_handoff {
                self.notice = Some("읽기나 편집을 마친 뒤 다시 닫아 주세요.".into());
                return;
            }
        }
        if !self.restoring_layout && self.views[slot].owns_shared_file_work()
            && self.has_other_overlay_for_primary(slot, false) && !self.handoff_primary_to_overlay(slot, false) {
            self.notice = Some("겹쳐 본 파일의 읽기나 저장을 마친 뒤 다시 닫아 주세요.".into());
            return;
        }
        if !self.restoring_layout && self.views[slot].overlay_owner().is_some_and(View::owns_shared_file_work)
            && self.has_other_reference_for_overlay(slot, false) && !self.handoff_overlay_file_work(slot) {
            self.notice = Some("겹쳐 본 파일의 읽기를 마친 뒤 다시 닫아 주세요.".into());
            return;
        }
        let previous_layout = dock::editor_layout(&self.dock);
        let previous_load = self.views[slot].loading.as_ref().map(|(path, _)| path.clone());
        self.cancel_pending_marker_job(slot);
        self.remember_layout_spot(slot);
        self.remember(slot);
        let previous_remote = self.views[slot].remote_loading.as_ref().map(|(_, _, path)| path.clone());
        self.prepare_close_remote_view(slot);
        if let Some(path) = previous_remote { self.pending_shared_promotions.push(path); }
        let mut view = std::mem::take(&mut self.views[slot]);
        if let Some(path) = &previous_load {
            let restore = view.restore.clone();
            view.wait_for_shared(path.clone(), restore);
        }
        let marker_ui = std::mem::take(&mut self.marker_ui[slot]);
        if !self.restoring_layout {
            if let Some(id) = self.layout_tabs.active_id() {
                let closed = ClosedLayoutView { view, marker_ui, slot, layout: previous_layout };
                let evicted = self.layout_tabs.record_closed_view(id, slot, closed);
                drop_later(evicted);
            } else {
                drop_later(Some((view, marker_ui)));
            }
        } else {
            drop_later(Some((view, marker_ui)));
        }
        if let Some(path) = previous_load { self.pending_shared_promotions.push(path); }
        dock::remove_view(&mut self.dock, slot as u8);
        let open = self.open_views();
        if open.is_empty() {
            // Keep a valid empty slot for panel code; it is not an open 3D view.
            self.active = 0;
            self.focused_tab = self.dock.find_active_focused().map(|(_, pane)| *pane);
            self.explorer.reveal = None;
        } else if self.active == slot || !open.contains(&self.active) {
            let next = open[0];
            self.active = usize::MAX;
            self.activate(next);
        }
    }

    /// Markers, comment files or force plates arrived in any view; comment writes finished.
    fn poll_loads(&mut self, ctx: &egui::Context) {
        for slot in self.open_views() {
            if let Some(loaded) = self.views[slot].poll_markers() {
                self.finish_load(ctx, slot, loaded);
            }
            self.views[slot].poll_comments();
            self.views[slot].poll_analog();
            if !self.marker_saving(slot) { self.views[slot].request_emg(ctx); }
        }
        self.poll_comment_ids(ctx);
        self.poll_overlays(ctx);
        self.poll_comment_writes(ctx);
        self.poll_takm(ctx);
        self.poll_ssh(ctx);
        self.poll_remote_emg(ctx);
    }

    fn finish_load(&mut self, ctx: &egui::Context, slot: usize, loaded: Loaded) {
        let Loaded { path, result, rx } = loaded;
        let (data, steps) = match result {
            Ok(r) => r,
            Err(e) => {
                // The view keeps what it showed, with that file's comments.
                self.views[slot].set_fresh_open_candidate(None);
                self.views[slot].comments_rx = None;
                self.error = Some(format!("{}\n\n{}", path.display(), e));
                self.fail_shared_waiters(&path);
                // A new view whose file cannot be read closes again. (Not an action: the undo
                // history takes the layout as it is.)
                if self.views[slot].data.is_none() && self.open_views().len() > 1 {
                    self.close_view(slot);
                }
                self.history.quiet_layout = true;
                return;
            }
        };
        self.next_file_id += 1;
        let now = ctx.input(|i| i.time);
        let speed = self.settings.speed;
        let v = &mut self.views[slot];
        v.load_steps = steps;
        drop_later(v.show(data, rx, self.next_file_id, speed, now));
        push_recent(&mut self.settings.recent, path.to_string_lossy().into_owned());
        if slot == self.active {
            self.explorer.reveal = Some(path.clone());
        }
        self.attach_shared_waiters(ctx, &path);
    }

    /// `--screenshot`: go to frame, select marker, wait a few frames, capture, save, close.
    fn drive_auto_shot(&mut self, ctx: &egui::Context, now: f64) {
        let Some(auto) = self.auto.clone() else { return };
        if let Some(e) = &self.error {
            eprintln!("screenshot: load failed: {e}");
            self.failed = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let shot_image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let (Some(img), true) = (shot_image, auto.stage >= 7) {
            let [w, h] = img.size;
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
            match shot::save_png(&auto.out, w, h, &rgba) {
                Ok(()) => println!("screenshot saved: {} ({w}x{h})", auto.out.display()),
                Err(e) => {
                    eprintln!("screenshot: save failed: {e}");
                    self.failed = true;
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // Pictures are taken with every view's file and force plates loaded, and every graph
        // complete (v0.27.0 speed fix: markers past a frame's work come in the next frames).
        let open = self.open_views();
        let waiting = open.iter().any(|&k| self.views[k].busy()) || self.view().data.is_none() || self.graph_put_off;
        if waiting || self.workspace.is_loading() {
            ctx.request_repaint();
            return;
        }
        let mut next = auto.clone();
        match auto.stage {
            0 => {
                // `--frame` / `--select` / `--plate` / `--range` apply to every view (several
                // files compared side by side).
                for &k in &open {
                    let v = &mut self.views[k];
                    // `--pick-comment` (v0.17.1): as a click on its band block (= since v0.27.3 a
                    // double click on the `#` of its row: selected, first frame, its markers);
                    // `--frame` still wins.
                    if let Some(id) = auto.pick_comment {
                        v.goto_comment(id, now);
                    }
                    if let Some(f) = auto.frame {
                        v.pb.set_frame(f, now);
                    }
                    if let Some((a, b)) = auto.range {
                        v.set_range(a, b);
                    }
                    // `--select A,B,C` (v0.27.0, agent pictures): several markers, the last one
                    // found the primary (as Ctrl+click one after another).
                    if let Some(names) = &auto.select {
                        let mut first = true;
                        for name in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                            let found = v.data.as_ref().and_then(|d| d.markers.iter().position(|m| m.name == name));
                            if let Some(i) = found {
                                v.select(i, !first);
                                first = false;
                            }
                        }
                    }
                    if let Some(want) = &auto.plate {
                        if let Some(p) = v.data.as_ref().and_then(|d| shot::find_plate(&d.plates, want)) {
                            v.plate = Some(p);
                        }
                    }
                    // `--hover NAME` (v0.29.0, agent pictures): the marker graph drawn as if the
                    // mouse were on that graphed marker's curve.
                    if let (Some(want), Some(d), Some(p)) = (&auto.hover, v.data.as_ref(), v.primary) {
                        let (graphed, _) = panels::graphed(d, &v.selected, p);
                        v.lanes.hover_at = graphed.iter().position(|&m| d.markers[m].name == *want);
                        if v.lanes.hover_at.is_none() {
                            eprintln!("--hover: '{want}' is not among the graphed markers");
                        }
                    }
                    // `--hide 1|2` (plan step 7): that file's markers hidden where a file is laid over.
                    if v.overlay.is_some() {
                        for &h in &auto.hide {
                            v.shown[h] = false;
                        }
                    }
                }
                if auto.comments {
                    self.show_pane(Pane::Comments);
                }
                if auto.edit_tools { self.show_pane(Pane::EditTools); }
                if auto.edit_pattern { self.select_pattern_for_capture(self.active); }
                if auto.edit_layout != 0 {
                    dock::hide(&mut self.dock, Pane::Selected);
                    dock::hide(&mut self.dock, Pane::Devices);
                    if let Some(loc) = self.dock.find_tab(&Pane::EditTools) {
                        self.dock.remove_tab(loc);
                    }
                    if auto.edit_layout == 2 {
                        self.set_graph_float(false);
                        if let Some((node, _)) = self.dock.main_surface().find_tab(&Pane::Labeling) {
                            self.dock.main_surface_mut().split_below(node, 0.55, vec![Pane::EditTools]);
                        }
                    } else if auto.edit_layout == 3 {
                        self.settings.graph_float = false;
                        dock::hide(&mut self.dock, Pane::Graph);
                        if let Some((node, _)) = self.dock.main_surface().find_tab(&Pane::View(self.active as u8)) {
                            let [_, graph] = self.dock.main_surface_mut().split_below(node, 0.58, vec![Pane::Graph]);
                            self.dock.main_surface_mut().split_above(graph, 0.32, vec![Pane::EditTools]);
                        }
                    }
                }
                // `--edit-comment` (agent pictures): the edit of that cell shown in the active
                // view. A command run writes nothing, so it is only looked at.
                if let Some((id, field)) = auto.edit_comment {
                    let v = &self.views[self.active];
                    if let Some(c) = v.comments.list.get(id) {
                        self.comment_edit = Some(comment_list::Edit::new(self.active, v.file_id, c, field, v.selection()));
                    }
                }
                // `--type-strip` (agent pictures, step 5): the strip of that comment shown as
                // right after it was made; only looked at.
                if let Some(id) = auto.type_strip {
                    let slot = self.active;
                    let file_id = self.views[slot].file_id;
                    if self.views[slot].comments.list.get(id).is_some() {
                        self.type_strip = Some(TypeStrip { slot, file_id, id, rect: egui::Rect::NOTHING, buttons: [egui::Rect::NOTHING; 3], shot: true });
                    }
                }
                if auto.menu_capture {
                    self.start_menu_capture();
                }
                // Text drawn while several files load depends on timing and decides where later
                // glyphs sit in the font atlas, which moves edge pixels of text by 1/255. Now that
                // every file is in, the atlas is made again (plain fonts for one frame, then ours),
                // so the same command always gives the same picture.
                ctx.set_fonts(egui::FontDefinitions::default());
                next.stage = 1;
            }
            1 => {
                theme::install_fonts(ctx);
                next.stage = 2;
            }
            2..=5 => next.stage += 1,
            // Verification of the menu capture: wait until it has finished.
            6 if self.capture.is_some() => {}
            6 => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                next.stage = 7;
            }
            _ => {}
        }
        self.auto = Some(next);
        ctx.request_repaint();
    }

    /// Menu "화면 캡처 저장": current frame from the current angle + top, side, front.
    fn start_menu_capture(&mut self) {
        if self.view().remote.is_some() {
            self.notice = Some("원격 파일의 캡처 저장은 아직 지원하지 않아요.".into());
            return;
        }
        let Some(d) = &self.view().data else { return };
        if self.capture.is_some() {
            return;
        }
        self.capture = Some(CaptureRun::new(CapturePlan {
            tak: d.take.path.clone(),
            out: None,
            frames: FrameSpec::List(vec![self.view().pb.frame]),
            views: vec![ViewPreset::Current, ViewPreset::Top, ViewPreset::Side, ViewPreset::Front],
            select: vec![],
            fit: Fit::Frame,
            names: self.settings.scene.names,
            trail: None,
            full_ui: false,
            sheet: true,
            size: [0.0, 0.0],
            exit_when_done: false,
        }));
    }

    fn fail_capture(&mut self, ctx: &egui::Context, run: &CaptureRun, msg: String) {
        if run.plan.exit_when_done {
            eprintln!("capture: {msg}");
            self.failed = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            self.error = Some(format!("캡처하지 못했어요.\n\n{msg}"));
        }
    }

    /// `--capture` / menu capture, one shot per (frame, view); see `capture.rs`.
    fn drive_capture(&mut self, ctx: &egui::Context, now: f64) {
        let Some(mut run) = self.capture.take() else { return };
        if run.plan.exit_when_done {
            if let Some(e) = self.error.clone() {
                self.fail_capture(ctx, &run, format!("load failed: {e}"));
                return;
            }
        }
        if self.view().data.is_none() || self.view().loading.is_some() {
            if run.plan.exit_when_done {
                self.capture = Some(run);
                ctx.request_repaint();
            }
            return;
        }
        if self.view().analog_rx.is_some() {
            // Wait for force plates / EMG so captures show them.
            self.capture = Some(run);
            ctx.request_repaint();
            return;
        }
        let shot_image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let (Some(img), 5) = (shot_image, run.stage) {
            self.store_capture_shot(ctx, &mut run, &img);
            run.index += 1;
            run.stage = 0;
        }
        if !run.started {
            if let Err(msg) = self.start_capture(&mut run) {
                self.fail_capture(ctx, &run, msg);
                return;
            }
            self.capture = Some(run);
            ctx.request_repaint();
            return;
        }
        if run.index >= run.shots.len() {
            self.finish_capture(ctx, run, now);
            return;
        }
        match run.stage {
            0 => {
                self.setup_capture_shot(&run, now);
                run.stage = 1;
            }
            1..=3 => run.stage += 1,
            4 => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                run.stage = 5;
            }
            _ => {}
        }
        self.capture = Some(run);
        ctx.request_repaint();
    }

    fn start_capture(&mut self, run: &mut CaptureRun) -> Result<(), String> {
        let slot = self.active;
        let d = self.views[slot].data.as_ref().ok_or("파일이 없어요")?;
        run.started = true;
        run.tak_before = capture::stat_key(&d.take.path);
        let tak = capture::absolute(&d.take.path);
        let out = match &run.plan.out {
            Some(o) => {
                // A folder the caller chose is used as given (may already exist).
                let out = capture::absolute(&workspace::resolve_path(&o.to_string_lossy()));
                capture::check_out_dir(&out, &tak)?;
                std::fs::create_dir_all(&out).map_err(|e| format!("폴더를 만들 수 없어요: {} ({e})", out.display()))?;
                out
            }
            None => {
                // Default folder: always a new one, even when another viewer captures the
                // same file in the same second.
                let base = capture::default_out_dir(&tak, &capture::local_stamp())?;
                capture::check_out_dir(&base, &tak)?;
                capture::new_dir(&base)?
            }
        };
        run.out_dir = Some(out);
        if run.plan.exit_when_done {
            let mut found = BTreeSet::new();
            let mut last = None;
            for name in &run.plan.select {
                match d.markers.iter().position(|m| &m.name == name) {
                    Some(i) => {
                        found.insert(i);
                        last = Some(i);
                    }
                    None => run.notes.push(format!("선택할 마커 이름을 찾지 못했어요: {name}")),
                }
            }
            let v = &mut self.views[slot];
            v.selected = found;
            v.primary = last;
            let s = &mut self.settings;
            s.scene.names = run.plan.names;
            if let Some(n) = run.plan.trail {
                s.scene.history = true;
                s.scene.trail_frames = n;
                s.scene.trail_all = true;
            }
            if run.plan.full_ui {
                dock::hide(&mut self.dock, Pane::Explorer);
            } else {
                self.dock = dock::scene_only(slot as u8);
            }
        }
        // The 3D view must be on screen to be captured.
        dock::focus_view(&mut self.dock, slot as u8);
        let v = &self.views[slot];
        let Some(d) = &v.data else { return Err("파일이 없어요".into()) };
        run.selected = v.selected.iter().copied().collect();
        run.restore_cam = Some(v.cam);
        run.restore_frame = Some(v.pb.frame);
        let (first, last) = (d.start_frame, d.end_frame);
        run.plan_shots(first, last);
        if run.shots.is_empty() {
            return Err("찍을 프레임이 없어요(--frames 범위 확인)".into());
        }
        Ok(())
    }

    fn setup_capture_shot(&mut self, run: &CaptureRun, now: f64) {
        let (frame, view) = run.shots[run.index];
        let v = &mut self.views[self.active];
        v.pb.playing = false;
        v.pb.set_frame(frame, now);
        let Some(d) = &v.data else { return };
        let aspect = v.scene_rect.width() / v.scene_rect.height().max(1.0);
        let bounds = capture::fit_bounds(d, frame, run.plan.fit, &run.selected);
        let cam = &mut v.cam;
        match view.angles(capture::walk_along_x(d)) {
            Some((yaw, pitch, fov)) => {
                cam.yaw = yaw;
                cam.pitch = pitch;
                cam.fov_y = fov;
                if let Some((lo, hi)) = bounds {
                    cam.fit_box(lo, hi, aspect);
                }
            }
            None if view == ViewPreset::Persp => {
                cam.fov_y = Camera::default().fov_y;
                if let Some((lo, hi)) = bounds {
                    cam.frame_box(lo, hi, aspect);
                }
            }
            None => {
                if let Some(c) = run.restore_cam {
                    *cam = c;
                }
            }
        }
    }

    fn store_capture_shot(&mut self, ctx: &egui::Context, run: &mut CaptureRun, img: &egui::ColorImage) {
        let [w, h] = img.size;
        let mut image = capture::Image { w, h, rgba: img.pixels.iter().flat_map(|p| p.to_array()).collect() };
        if !run.plan.full_ui {
            // Keep the 3D view only.
            let ppp = ctx.pixels_per_point();
            let r = self.view().scene_rect;
            let px = |v: f32| (v * ppp).round().max(0.0) as usize;
            image = image.crop(px(r.min.x), px(r.min.y), px(r.width()), px(r.height()));
        }
        let (Some((frame, view)), Some(out)) = (run.shots.get(run.index).copied(), run.out_dir.clone()) else { return };
        let name = run.file_name(frame, view);
        match capture::save_png(&out.join(&name), &image) {
            Ok(()) => {
                let v = self.view();
                if let Some(d) = &v.data {
                    run.records.push(capture::shot_record(d, frame, view, &name, &v.cam, &run.selected));
                }
                run.cells.push(image.shrink(capture::SHEET_CELL_W));
            }
            Err(e) => run.notes.push(format!("{name} 저장 실패: {e}")),
        }
    }

    fn finish_capture(&mut self, ctx: &egui::Context, mut run: CaptureRun, now: f64) {
        // Frames and camera come back to where they were: no action.
        self.history.quiet_all = true;
        let lines = match &self.view().data {
            Some(d) => run.finish(d, workspace::to_wsl_path(&capture::absolute(&d.take.path))),
            None => vec![],
        };
        if run.plan.exit_when_done {
            for l in &lines {
                println!("{l}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let v = self.view_mut();
        if let Some(c) = run.restore_cam {
            v.cam = c;
        }
        if let Some(f) = run.restore_frame {
            v.pb.set_frame(f, now);
        }
        self.capture_done = Some((run.out_dir.clone(), lines));
    }

    /// Whether view `slot` can take a new comment now: Ok(hover text of the `Comment` button)
    /// or Err(why not, plain Korean).
    fn comment_ready(&self, slot: usize) -> Result<String, String> {
        let path = self.comment_writable(slot)?;
        if self.views[slot].comments.id_pool.as_ref().is_none_or(|ids| ids.is_empty()) {
            return Err(self.views[slot].comments.id_error.clone().unwrap_or_else(|| "Preparing Comment IDs. Try again shortly.".into()));
        }
        if let Some(e) = &self.comment_edit {
            if e.field == Field::Type {
                return Err(format!("Comment {}의 유형 목록이 열려 있어요. 고르거나 Esc로 닫은 뒤에 새로 만들 수 있어요.", e.id));
            }
            return Err(format!("Comment {}을 고치는 중이에요. Enter = 저장, Esc = 취소 뒤에 새로 만들 수 있어요.", e.id));
        }
        let v = &self.views[slot];
        let frames = match v.range {
            Some((a, b)) if a != b => format!("구간 {a} – {b}"),
            Some((a, _)) => format!("프레임 {a}"),
            None => format!("지금 프레임 {}", v.pb.frame),
        };
        let markers = match v.selected.len() {
            0 => "마커 없이".to_owned(),
            n => format!("고른 마커 {n}개"),
        };
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(format!(
            "Comment = {frames} + {markers} (M 짧게)\nM을 누른 채 재생하다 떼면 누른 동안의 구간으로 만들어요.\n마커는 골라도 되고 안 골라도 돼요 (3D나 Labeling에서 클릭, Ctrl+클릭 = 여러 개).\n{name} 안에 바로 저장해요. Ctrl+Z = 되돌리기."
        ))
    }

    /// Whether view `slot`'s comments can be written now (new ones, notes, deletes): Ok(the
    /// comment file) or Err(why not, plain Korean; the `Comment` button and the COMMENTS pane
    /// show it).
    fn comment_writable(&self, slot: usize) -> Result<&Path, String> {
        let v = &self.views[slot];
        if self.marker_saving(slot) || self.exit_waiting_comments { return Err("저장이 끝날 때까지 기다려 주세요.".into()); }
        if v.data.is_none() {
            return Err("파일을 열면 Comment를 남길 수 있어요.".into());
        }
        if v.remote.is_some() { return Err(remote_ui::REMOTE_NO_COMMENTS.into()); }
        // A TAK takes no Comments (`.takm` step ③), also in a command run.
        if self.shows_tak(slot) {
            return Err(views::TAK_NO_COMMENTS.into());
        }
        if self.no_persist {
            return Err("명령 실행(--screenshot · --capture · --headless · --bench)은 Comment 파일을 쓰지 않아요.".into());
        }
        match &v.comments.target {
            None => Err("Comment 파일을 읽는 중이에요. 잠깐 뒤에 다시 누르세요.".into()),
            Some(Err(why)) => Err(why.clone()),
            Some(Ok(p)) => Ok(p),
        }
    }

    /// Names of view `slot`'s selected markers, in Labels order.
    fn selected_names(&self, slot: usize) -> Vec<String> {
        self.views[slot].selected_names()
    }

    /// Where changes of view `slot`'s comments are written: (comment file, `file_a` label).
    /// None when nothing may be written (command run, no file, its comment file still being read
    /// or refused).
    fn comment_target(&self, slot: usize) -> Option<(PathBuf, String)> {
        if self.no_persist || self.views[slot].remote.is_some() || self.marker_saving(slot) || self.exit_waiting_comments {
            return None;
        }
        let v = &self.views[slot];
        match (&v.data, &v.comments.target) {
            (Some(d), Some(Ok(p))) => Some((p.clone(), v.comments.label.clone().unwrap_or_else(|| comments::file_label(&d.take.path, d.take.file_size)))),
            _ => None,
        }
    }

    /// View `slot` shows a file that is not a `.takm` (a TAK: no Comments).
    fn shows_tak(&self, slot: usize) -> bool {
        self.views[slot].remote.is_none() && self.views[slot].path().is_some_and(|p| !tak_core::takm::is_takm_path(p))
    }

    /// `M`, the `Comment` button or `T` on a TAK: why nothing happens, in the status bar.
    fn say_no_comments(&mut self, ctx: &egui::Context, slot: usize) {
        if self.views[slot].remote.is_some() {
            self.ssh_note = Some((remote_ui::REMOTE_NO_COMMENTS.into(), ctx.input(|i| i.time)));
            return;
        }
        if self.shows_tak(slot) {
            self.takm_note = Some((views::TAK_NO_COMMENTS.to_owned(), ctx.input(|i| i.time)));
        }
    }

    /// Append one line about comment `c` (`add` = new or brought back, `delete`) for view
    /// `slot`: the list changes at once, the writer thread writes. False when nothing may be
    /// written there (then nothing changes).
    fn write_change(&mut self, ctx: &egui::Context, slot: usize, kind: EventKind, c: &Comment) -> bool {
        if self.dirty_prompt.is_some() { return false; }
        self.views[slot].sync_shared();
        if kind != EventKind::Add && self.views[slot].comments.list.id_is_ambiguous(c.id) {
            self.notice = Some("This older file has duplicate Comment IDs; this comment cannot be edited safely.".into());
            return false;
        }
        let Some((path, file_a)) = self.comment_target(slot) else { return false };
        let mut event = Event::of(kind, c, capture::local_iso_time(), file_a);
        if kind != EventKind::Add {
            event.expected = self.views[slot].comments.list.get(c.id).cloned();
        }
        // With a file laid over this view (plan step 7): which of the two were shown, and the
        // second file (same `name|size` form). Without one the line is as before.
        let v = &self.views[slot];
        if let Some(o) = &v.overlay {
            let flag = |on: bool| if on { "1" } else { "0" }.to_owned();
            (event.shown_a, event.shown_b) = (flag(v.shown[0]), flag(v.shown[1]));
            event.file_b = comments::file_label(&o.path, o.data.take.file_size);
        }
        self.views[slot].comments.list.apply(&event);
        self.send_comment(ctx, slot, path, event);
        self.views[slot].publish_shared_file();
        true
    }

    /// A short press of `M` / `Comment` button / ⋯ item: a comment of view `slot`'s selected
    /// markers and its range (no range = the current frame). Nothing happens when
    /// `comment_ready` says no.
    fn make_comment(&mut self, ctx: &egui::Context, slot: usize) {
        if self.comment_ready(slot).is_err() {
            self.retry_comment_ids(ctx, slot);
            self.say_no_comments(ctx, slot);
            return;
        }
        let v = &self.views[slot];
        let (start, end) = v.range.unwrap_or((v.pb.frame, v.pb.frame));
        let markers = self.selected_names(slot);
        self.add_comment(ctx, slot, markers, start, end);
    }

    /// A new comment in view `slot`: shown at once, appended by the writer thread, kept for
    /// Ctrl+Z (the redo part goes). The range is cleared, the selection stays; the cleared range
    /// is part of the same undo action (v0.15.1).
    fn add_comment(&mut self, ctx: &egui::Context, slot: usize, markers: Vec<String>, start: i64, end: i64) {
        let Some(id) = self.views[slot].comments.id_pool.as_mut().and_then(Iterator::next) else {
            self.notice = Some("Preparing Comment IDs. Try again shortly.".into());
            return;
        };
        let c = Comment { id, markers, start, end, comment_type: String::new(), text: String::new() };
        // What changed before it (e.g. frames moved while `M` was held) comes first.
        self.observe(true);
        if !self.write_change(ctx, slot, EventKind::Add, &c) {
            return;
        }
        let cleared = self.views[slot].range.take();
        // The new comment is the selected one (v0.17.1), in the same action.
        self.views[slot].picked = Some(c.id);
        self.views[slot].comment_selection.single(c.id);
        self.comment_scroll = Some((self.views[slot].file_id, c.id));
        // The type strip for it (step 5; only here, where it was written).
        let file_id = self.views[slot].file_id;
        self.type_strip = Some(TypeStrip { slot, file_id, id: c.id, rect: egui::Rect::NOTHING, buttons: [egui::Rect::NOTHING; 3], shot: false });
        self.push_comment(slot, Change::Added(c), cleared);
    }

    /// View `slot`'s comment `id` gets type `t` (the strip, `T`): one `edit` line and one undo
    /// action (consecutive type changes of one comment are one action, `app_undo.rs`); the same
    /// type = nothing. A short note in the status bar says what it is now.
    fn set_comment_type(&mut self, ctx: &egui::Context, slot: usize, id: u64, t: &str) -> bool {
        let Some(c) = self.views[slot].comments.list.get(id).cloned() else { return false };
        self.observe(true);
        if !self.edit_comment(ctx, slot, Comment { comment_type: t.to_owned(), ..c }) {
            return false;
        }
        let shown = if t.is_empty() { "no type" } else { t };
        self.history.note = Some((format!("Comment {id} · {shown}"), ctx.input(|i| i.time)));
        true
    }

    /// `T` (step 5): the selected comment of the active view goes to its next type (none →
    /// Jitter → Marker swap → Needs visual check → none, type-combo). Nothing with no comment
    /// selected, during a cell edit or with the type list open, while `M` is held, or where
    /// comments cannot be written (the key
    /// is used up either way; while a text field has the keys it never gets here). The strip
    /// stays open when it is this comment's.
    fn cycle_type(&mut self, ctx: &egui::Context) {
        let slot = self.active;
        let v = &self.views[slot];
        let target = v.picked.filter(|_| self.hold.is_none() && self.comment_edit.is_none() && self.comment_writable(slot).is_ok());
        let Some((id, now)) = target.and_then(|id| Some((id, v.comments.list.get(id)?.comment_type.clone()))) else {
            self.type_strip = None;
            self.say_no_comments(ctx, slot);
            return;
        };
        let file_id = v.file_id;
        let next = comments::step_type(&now);
        self.set_comment_type(ctx, slot, id, next);
        if !self.type_strip.as_ref().is_some_and(|s| (s.slot, s.file_id, s.id) == (slot, file_id, id)) {
            self.type_strip = None;
        }
    }

    /// Start of every frame (before any key or click is used): the type strip closes when its
    /// comment, file or the right to write is gone, a capture runs, or on the next action
    /// elsewhere: a key press (not a plain `T`, which `cycle_type` handles), a mouse button
    /// pressed outside it, the wheel. Nothing is used up: that key or click still does what it
    /// always did.
    fn type_strip_input(&mut self, ctx: &egui::Context) {
        let Some(s) = &self.type_strip else { return };
        let v = &self.views[s.slot];
        let gone = v.file_id != s.file_id || v.data.is_none() || v.comments.list.get(s.id).is_none() || self.capture.is_some();
        let unwritable = !s.shot && self.comment_target(s.slot).is_none();
        let typing = self.typing(ctx);
        let rect = s.rect;
        let elsewhere = ctx.input(|i| {
            i.events.iter().any(|e| match e {
                egui::Event::Key { key, pressed: true, modifiers, .. } => !(*key == Key::T && modifiers.is_none() && !typing),
                egui::Event::PointerButton { pos, pressed: true, .. } => !rect.contains(*pos),
                egui::Event::MouseWheel { .. } => true,
                _ => false,
            })
        });
        if gone || unwritable || elsewhere {
            self.type_strip = None;
        }
    }

    /// View `slot`'s comment becomes `after` (an edit of the COMMENTS pane): an `edit` line and
    /// one undo action, with the selection change it brings (`push_comment`). Call
    /// `observe(true)` first. False (nothing written) when it is the same, gone, or nothing may
    /// be written.
    fn edit_comment(&mut self, ctx: &egui::Context, slot: usize, after: Comment) -> bool {
        let Some(before) = self.views[slot].comments.list.get(after.id).cloned() else { return false };
        if after == before || !self.write_change(ctx, slot, EventKind::Edit, &after) {
            return false;
        }
        self.push_comment(slot, Change::Edited { before, after }, None);
        true
    }

    /// The view and file of a marker edit going on (its selection is the draft: the undo
    /// history does not compare it meanwhile).
    pub(super) fn marker_edit(&self) -> Option<(usize, usize)> {
        self.comment_edit.as_ref().filter(|e| e.field == Field::Markers).map(|e| (e.slot, e.file_id))
    }

    /// A double click on a cell of the active view's comment `id` (v0.17.1): edit it. Only
    /// where comments can be written; another edit going on is cancelled first.
    fn start_comment_edit(&mut self, id: u64, field: Field) {
        self.cancel_comment_edit();
        let slot = self.active;
        if self.comment_writable(slot).is_err() {
            return;
        }
        let v = &self.views[slot];
        let Some(c) = v.comments.list.get(id) else { return };
        // What changed so far is recorded first (the first click of the double click).
        let e = comment_list::Edit::new(slot, v.file_id, c, field, v.selection());
        self.observe(true);
        self.views[slot].comment_selection.single(id);
        self.views[slot].picked = Some(id);
        self.views[slot].comment_marker_draft = field == Field::Markers;
        self.comment_edit = Some(e);
    }

    /// Esc (or the edit cannot go on: another row clicked, its list not drawn, a delete): the
    /// edit ends with nothing written. A marker edit puts the selection from its start back, so
    /// nothing of it is left in the undo history.
    fn cancel_comment_edit(&mut self) {
        let Some(e) = self.comment_edit.take() else { return };
        let v = &mut self.views[e.slot];
        v.comment_marker_draft = false;
        if e.field == Field::Markers && v.file_id == e.file_id && v.data.is_some() {
            v.set_selection(e.before);
        }
        // A Frames edit whose boxes were dragged: the frame goes back to where it was before the
        // first drag (v0.17.3), so nothing of it is left to undo.
        if let (Some(f), true) = (e.frame_before, v.file_id == e.file_id && v.data.is_some()) {
            v.pb.playing = false;
            v.pb.set_frame(f, 0.0);
        }
    }

    /// The view and file of a Frames edit whose boxes have moved the view's frame (v0.17.3):
    /// that frame is the edit's draft until Enter (one action with the comment) or Esc (put
    /// back), so the undo history does not compare it meanwhile.
    pub(super) fn frame_edit(&self) -> Option<(usize, usize)> {
        self.comment_edit.as_ref().filter(|e| e.field == Field::Frames && e.frame_before.is_some()).map(|e| (e.slot, e.file_id))
    }

    /// The comment of a Frames edit in view `slot` and the frames in its boxes now, for the band
    /// (v0.17.3: its block follows the typing and the dragging; saved as `typed_frames`).
    fn frames_draft(&self, slot: usize) -> Option<(u64, i64, i64)> {
        let e = self.comment_edit.as_ref().filter(|e| e.slot == slot && e.field == Field::Frames)?;
        let v = &self.views[slot];
        let c = v.comments.list.get(e.id).filter(|_| v.file_id == e.file_id)?;
        let (start, end) = comment_list::typed_frames(&e.start, &e.end, c, v.pb.first, v.pb.last);
        Some((e.id, start, end))
    }

    /// A frame box of the Frames edit is dragged to `frame` (v0.17.3): the view goes there
    /// (paused), so the 3D shows that frame. The first drag of the edit records what changed
    /// before it and keeps the frame it started from (Esc goes back there).
    fn scrub_comment_frame(&mut self, frame: i64, now: f64) {
        let Some(e) = self.comment_edit.as_ref().filter(|e| e.field == Field::Frames) else { return };
        let slot = e.slot;
        if self.views[slot].file_id != e.file_id {
            return;
        }
        if e.frame_before.is_none() {
            self.observe(true);
            let before = self.views[slot].pb.frame;
            if let Some(e) = self.comment_edit.as_mut() {
                e.frame_before = Some(before);
            }
        }
        let v = &mut self.views[slot];
        v.pb.playing = false;
        v.pb.set_frame(frame, now);
    }

    /// Enter: the cell being edited is saved (one `edit` line, one undo action). Markers = the
    /// selection now (Labels order, names this file does not have stay; empty = a comment
    /// without markers), Frames = the typed boxes (`typed_frames`), Comment = the note
    /// (`clean_text`). No change = nothing written and no action (a marker edit then puts the
    /// selection from its start back, like Esc).
    fn save_comment_edit(&mut self, ctx: &egui::Context) {
        let Some(e) = self.comment_edit.clone() else { return };
        // Frame moves etc. before Enter are their own actions; the draft selection (and the
        // frame a dragged frame box moved, v0.17.3) is not compared while the edit is still on:
        // `push_comment` takes it into the edit's action.
        self.observe(true);
        self.comment_edit = None;
        self.views[e.slot].comment_marker_draft = false;
        let v = &self.views[e.slot];
        let Some(c) = v.comments.list.get(e.id).cloned().filter(|_| v.file_id == e.file_id) else { return };
        if c != e.base {
            let message = format!("Comment {} changed in another window · reloaded", e.id);
            self.history.note = Some((message.clone(), ctx.input(|i| i.time)));
            self.notice = Some(message);
            return;
        }
        // A type picked in the list (type-combo): the same path as the strip and `T` (one
        // `edit` line, one action that merges with type changes of this comment in a row, the
        // short note `Comment 2 · Jitter`; the same type = nothing).
        if e.field == Field::Type {
            self.set_comment_type(ctx, e.slot, e.id, &e.choice);
            return;
        }
        let after = match e.field {
            Field::Markers => match v.markers_for(&c) {
                Some(markers) => Comment { markers, ..c },
                None => c,
            },
            Field::Frames => {
                let (start, end) = comment_list::typed_frames(&e.start, &e.end, &c, v.pb.first, v.pb.last);
                Comment { start, end, ..c }
            }
            Field::Text => Comment { text: comments::clean_text(&e.text), ..c },
            Field::Type => Comment { comment_type: e.choice.clone(), ..c },
        };
        if !self.edit_comment(ctx, e.slot, after) && e.field == Field::Markers {
            self.views[e.slot].set_selection(e.before);
        }
    }

    /// Every frame, before the other keys: an edit whose comment or file went away is
    /// cancelled; Enter saves it, Esc cancels it (also while its box has the keyboard: egui
    /// lets go of a box on Esc before the app sees the key).
    fn comment_edit_keys(&mut self, ctx: &egui::Context) {
        let Some(e) = &self.comment_edit else { return };
        let v = &self.views[e.slot];
        if v.file_id != e.file_id || v.data.is_none() || v.comments.list.get(e.id).is_none() {
            self.cancel_comment_edit();
            return;
        }
        // The type list (type-combo): a mouse button pressed outside the list and its box, or
        // the wheel, closes it with nothing changed. Nothing is used up: that click or wheel
        // still does what it always does (like the type strip). A press on its own box is left
        // to the box (a click there closes it).
        if e.field == Field::Type {
            let (list, anchor) = (e.list, e.anchor);
            let elsewhere = ctx.input(|i| {
                i.events.iter().any(|ev| match ev {
                    egui::Event::PointerButton { pos, pressed: true, .. } => !list.contains(*pos) && !anchor.contains(*pos),
                    egui::Event::MouseWheel { .. } => true,
                    _ => false,
                })
            });
            if elsewhere {
                self.cancel_comment_edit();
                return;
            }
        }
        // Another text field with the keyboard (e.g. the Labels search) keeps its keys.
        let own = |f: egui::Id| (0..3).any(|w| f == comment_list::box_id(e.file_id, e.id, w));
        if ctx.memory(|m| m.focused()).is_some_and(|f| !own(f)) {
            return;
        }
        // The type list: ↑ ↓ move its highlight (then they do not step frames).
        if e.field == Field::Type {
            let (up, down) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowUp), i.consume_key(Modifiers::NONE, Key::ArrowDown)));
            if let Some(e) = self.comment_edit.as_mut().filter(|_| up != down) {
                e.choice = comment_list::step_item(&e.choice, up).to_owned();
            }
        }
        let (enter, esc) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape)));
        if enter {
            self.save_comment_edit(ctx);
        } else if esc {
            self.cancel_comment_edit();
        }
        if enter || esc {
            // Its boxes are gone: they let go of the keyboard.
            ctx.memory_mut(|m| {
                if let Some(f) = m.focused() {
                    m.surrender_focus(f);
                }
            });
        }
    }

    /// Keys belong to a text field now (egui's keyboard focus): the app's keys do nothing.
    fn typing(&self, ctx: &egui::Context) -> bool {
        self.layout_strip.is_renaming() || ctx.wants_keyboard_input()
    }

    /// Right click on a block: a delete line (with the comment's fields, for people reading the
    /// file) is appended, the block goes, Ctrl+Z can bring it back; a note in the status bar.
    fn delete_comment(&mut self, ctx: &egui::Context, slot: usize, id: u64) {
        self.delete_comments(ctx, slot, &[id], true);
    }

    /// `M` held (v0.15.0), run every frame before the other keys. Cancels first (the window lost
    /// focus, so its fake key-up saves nothing; a capture runs; the active view or its file
    /// changed; Esc, which is then used up), closes at the last frame when playback wrapped,
    /// then takes the `M` key events in order: a real press (not an auto-repeat, no text field
    /// focused, a comment possible) starts, a release ends and saves (`end_hold`).
    fn handle_hold(&mut self, ctx: &egui::Context) {
        let lost = ctx.input(|i| !i.focused || i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(false))));
        if let Some(h) = &self.hold {
            let v = &self.views[h.slot];
            if lost || self.capture.is_some() || self.active != h.slot || v.file_id != h.file_id || v.data.is_none() {
                self.hold = None;
            }
        }
        self.close_hold_on_wrap(ctx);
        let typing = self.typing(ctx);
        if self.hold.is_some() && !typing && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.hold = None;
        }
        // (pressed, repeat) of each M event. Presses with Ctrl / Alt or into a text field stay
        // for others; releases are always taken (they matter only to a hold).
        let keys: Vec<(bool, bool)> = ctx.input_mut(|i| {
            let mut out = Vec::new();
            i.events.retain(|e| match e {
                egui::Event::Key { key: Key::M, pressed, repeat, modifiers, .. } => {
                    let plain = !(modifiers.ctrl || modifiers.command || modifiers.alt || modifiers.mac_cmd);
                    if *pressed && (typing || !plain) {
                        return true;
                    }
                    out.push((*pressed, *repeat));
                    false
                }
                _ => true,
            });
            out
        });
        for (pressed, repeat) in keys {
            match (pressed, repeat) {
                (true, false) if self.hold.is_none() && !lost && self.capture.is_none() => self.start_hold(ctx),
                (true, _) => {}
                (false, _) => self.end_hold(ctx, false),
            }
        }
    }

    /// `M` went down in the active view: remember the frame and the selected markers (none is
    /// fine since v0.15.2; nothing when `comment_ready` says no, e.g. a command run).
    fn start_hold(&mut self, ctx: &egui::Context) {
        let slot = self.active;
        if self.comment_ready(slot).is_err() {
            self.retry_comment_ids(ctx, slot);
            self.say_no_comments(ctx, slot);
            return;
        }
        let markers = self.selected_names(slot);
        let v = &self.views[slot];
        self.hold = Some(Hold { slot, file_id: v.file_id, start: v.pb.frame, markers, wraps: v.pb.wraps });
    }

    /// The hold ends: `M` went up (`wrapped` false) at the current frame, or playback wrapped
    /// (`wrapped` true) and the range closes at the last frame. A short press (same frame up as
    /// down) is the v0.14.0 comment: the picked range, else that frame. Otherwise the held range
    /// (in order) with the markers selected at key down, and a picked range is dropped.
    fn end_hold(&mut self, ctx: &egui::Context, wrapped: bool) {
        let Some(h) = self.hold.take() else { return };
        let v = &self.views[h.slot];
        if v.file_id != h.file_id || v.data.is_none() {
            return;
        }
        let end = if wrapped { v.pb.last } else { v.pb.frame };
        let (start, end) = if !wrapped && end == h.start { v.range.unwrap_or((end, end)) } else { panels::ordered_range(h.start, end, v.pb.first, v.pb.last) };
        self.add_comment(ctx, h.slot, h.markers, start, end);
    }

    /// Playback went from the last frame back to the first while `M` is held: save start..last
    /// now; holding on does nothing more (a new press is needed).
    fn close_hold_on_wrap(&mut self, ctx: &egui::Context) {
        if self.hold.as_ref().is_some_and(|h| self.views[h.slot].pb.wraps != h.wraps) {
            self.end_hold(ctx, true);
        }
    }

    /// Hand an event to the writer thread (started on the first comment).
    fn send_comment(&mut self, ctx: &egui::Context, slot: usize, path: PathBuf, event: Event) {
        self.ensure_comment_writer(ctx);
        let tag = self.views[slot].file_id;
        if let Some(reason) = self.comment_registration_error.clone() {
            self.views[slot].comments.failed = Some(reason);
            self.comment_failed.push((tag, path, event));
        } else if self.comment_registration.is_some() {
            self.comment_waiting.push((tag, path, vec![event]));
            self.views[slot].comments.pending_since.get_or_insert_with(Instant::now);
            self.views[slot].comments.pending += 1;
            self.comment_outstanding += 1;
            ctx.request_repaint_after(Duration::from_millis(30));
        } else if self.comment_writer.as_ref().is_some_and(|writer| writer.send(tag, path.clone(), event.clone())) {
            self.views[slot].comments.pending_since.get_or_insert_with(Instant::now);
            self.views[slot].comments.pending += 1;
            self.comment_outstanding += 1;
        } else {
            self.views[slot].comments.failed = Some("저장하는 스레드가 멈췄어요".into());
            self.comment_failed.push((tag, path, event));
        }
    }

    fn ensure_comment_writer(&mut self, ctx: &egui::Context) {
        if self.comment_writer.is_some() || self.comment_registration_error.is_some() { return; }
        let wake = ctx.clone();
        let writer = comments::Writer::spawn(Box::new(move || wake.request_repaint()));
        match writer.barrier() {
            Ok(rx) => self.comment_registration = Some(rx),
            Err(error) => self.comment_registration_error = Some(error.to_string()),
        }
        self.comment_writer = Some(writer);
    }

    /// Reserve ids off the drawing thread; the on-disk reservation also covers other windows.
    fn poll_comment_ids(&mut self, ctx: &egui::Context) {
        if self.no_persist { return; }
        for slot in self.open_views() {
            let state = &mut self.views[slot].comments;
            if let Some(rx) = &state.id_rx {
                match rx.try_recv() {
                    Ok(Ok(ids)) => { state.id_pool = Some(ids); state.id_error = None; state.id_rx = None; }
                    Ok(Err(reason)) => { state.id_error = Some(reason); state.id_rx = None; }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        state.id_error = Some("Comment ID preparation stopped.".into()); state.id_rx = None;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(30)),
                }
            }
            if state.id_rx.is_some() || state.id_error.is_some() || state.id_pool.as_ref().is_some_and(|ids| !ids.is_empty()) { continue; }
            let Some(Ok(path)) = state.target.clone() else { continue };
            if !tak_core::takm::is_takm_path(&path) { continue; }
            let (tx, rx) = std::sync::mpsc::channel();
            let wake = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(comments::reserve_takm_ids(&path, 1024).map_err(|e| e.to_string()));
                wake.request_repaint();
            });
            state.id_rx = Some(rx);
        }
    }

    fn retry_comment_ids(&mut self, ctx: &egui::Context, slot: usize) {
        if self.views.get(slot).is_some_and(|v| v.comments.id_error.is_some()) {
            self.views[slot].comments.id_error = None;
            self.poll_comment_ids(ctx);
        }
    }

    /// Writes the writer thread finished: "saved" or the failure, in the view still showing
    /// that file (its `file_id` is the tag).
    fn poll_comment_writes(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.comment_registration {
            match rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(30)),
                result => {
                    self.comment_registration = None;
                    let status = result.unwrap_or_else(|_| Err("Comment 저장 준비가 중단됐어요.".into()));
                    if let Err(reason) = status { self.comment_registration_error = Some(reason); }
                    for (tag, path, events) in std::mem::take(&mut self.comment_waiting) {
                        let submitted = self.comment_registration_error.is_none() && self.comment_writer.as_ref().is_some_and(|w| w.send_batch(tag, path.clone(), events.clone()));
                        if !submitted {
                            self.comment_failed.extend(events.iter().cloned().map(|event| (tag, path.clone(), event)));
                            self.comment_outstanding = self.comment_outstanding.saturating_sub(events.len());
                            let hidden = self.layout_tabs.tabs_mut().iter_mut().filter_map(|tab| tab.layout.as_mut()).flat_map(|runtime| runtime.views.iter_mut());
                            for host in self.views.iter_mut().chain(hidden) {
                                let view = if host.file_id == tag { host } else if let Some(owner) = host.overlay_owner_mut().filter(|owner| owner.file_id == tag) { owner } else { continue };
                                view.sync_shared();
                                view.comments.pending = view.comments.pending.saturating_sub(events.len());
                                if view.comments.pending == 0 { view.comments.pending_since = None; }
                                view.comments.failed = Some(self.comment_registration_error.clone().unwrap_or_else(|| "Comment 저장이 중단됐어요.".into()));
                                view.publish_shared_file();
                            }
                        }
                    }
                }
            }
        }
        let Some(w) = &self.comment_writer else { return };
        let done = w.poll();
        let now = ctx.input(|i| i.time);
        for d in done {
            self.finish_comment_batch_event(&d);
            self.comment_outstanding = self.comment_outstanding.saturating_sub(1);
            if d.result.is_err() && d.conflict.is_none() && !d.cancelled {
                self.comment_failed.push((d.tag, d.path.clone(), d.event.clone()));
            }
            if let Some(conflict) = &d.conflict {
                self.history.forget_comment(&d.path, conflict.id);
            }
            let hidden = self.layout_tabs.tabs_mut().iter_mut().filter_map(|tab| tab.layout.as_mut()).flat_map(|runtime| runtime.views.iter_mut());
            for host in self.views.iter_mut().chain(hidden) {
                let v = if host.file_id == d.tag && host.data.is_some() { host } else if let Some(owner) = host.overlay_owner_mut().filter(|owner| owner.file_id == d.tag && owner.data.is_some()) { owner } else { continue };
                v.sync_shared();
                v.comments.pending = v.comments.pending.saturating_sub(1);
                if v.comments.pending == 0 { v.comments.pending_since = None; }
                if d.cancelled { v.publish_shared_file(); continue; }
                if let Some(conflict) = &d.conflict {
                    v.comments.list.reload_one(conflict.id, conflict.latest.clone());
                    if conflict.latest.is_none() {
                        let ids: Vec<_> = v.comments.list.list().iter().map(|c| c.id).collect();
                        v.comment_selection.remove(conflict.id, &ids);
                        v.picked = v.comment_selection.representative;
                    }
                    let message = d.result.as_ref().err().filter(|reason| reason.contains("저장된 Comment를 다시 읽지 못")).cloned().unwrap_or_else(|| format!("Comment {} changed in another window · reloaded", conflict.id));
                    self.history.note = Some((message.clone(), now));
                    self.notice = Some(message);
                    v.publish_shared_file();
                    continue;
                }
                if d.result.is_ok() {
                    if let (Some(change), Some(stamp)) = (&d.source_stamp, &v.source_stamp) {
                        if let Ok(mut known) = stamp.lock() {
                            if known.as_ref() == Some(&change.before) { *known = Some(change.after.clone()); }
                        }
                    }
                }
                match d.result {
                    Ok(()) => v.comments.saved_at = Some(now),
                    Err(ref e) => v.comments.failed = Some(e.clone()),
                }
                v.publish_shared_file();
            }
        }
        self.poll_comment_retry_transition(ctx);
    }

    /// Status bar item of view `slot`'s comments: text, hover text, and whether it is a warning.
    /// None while the comment file is still being read.
    fn comment_status(&self, slot: usize, now: f64) -> Option<(String, String, bool)> {
        let c = &self.views[slot].comments;
        let target = c.target.as_ref()?;
        let mut text = format!("Comments {}", c.list.len());
        let mut tip = match target {
            Ok(p) => format!("Comment 파일: {}", p.display()),
            Err(why) => format!("Comment는 저장하지 않아요: {why}"),
        };
        if self.no_persist {
            tip = "명령 실행은 Comment 파일을 읽기만 해요.".into();
        }
        if c.skipped > 0 {
            tip += &format!("\n읽지 못한 줄 {}개는 건너뛰었어요 (끝이 잘린 줄 포함).", c.skipped);
        }
        let warn = c.failed.is_some();
        if let Some(e) = &c.failed {
            text += " · 저장 못 함";
            tip += &format!("\n\n새 Comment를 파일에 저장하지 못했어요. 화면에는 있지만 뷰어를 끄면 사라져요.\n이유: {e}");
        } else if c.pending > 0 {
            text += " · saving…";
        } else if c.saved_at.is_some_and(|t| now - t < SAVED_NOTE_S) {
            text += " · saved";
        }
        Some((text, tip, warn))
    }

    /// The COMMENTS pane (v0.17.0): the active view's comments (`ui/comment_list.rs`); during an
    /// edit, the edited view's (v0.17.1). What a click asks is kept in `comment_out` and done
    /// after drawing (`after_comments_tab`).
    fn comments_tab(&mut self, ui: &mut egui::Ui) {
        let slot = self.comment_edit.as_ref().map_or(self.active, |e| e.slot);
        let write = self.comment_writable(slot).map(|_| ());
        if self.views[slot].remote.is_some() {
            ui.add_space(6.0);
            ui.label(RichText::new(remote_ui::REMOTE_NO_COMMENTS).size(12.0).color(theme::WARN));
            ui.add(egui::Label::new(RichText::new(format!("있던 Comment {}개는 아래에서 볼 수 있어요. 달려면 서버에서 .takm을 만들어 주세요.", self.views[slot].comments.list.list().len())).size(11.0).color(theme::TEXT_DIM)).wrap());
            if self.views[slot].comments.list.list().is_empty() { return; }
        }
        if self.shows_tak(slot) {
            self.tak_comments_tab(ui, slot);
            return;
        }
        if let Some(reason) = self.views[slot].comments.id_error.clone() {
            ui.label(RichText::new(format!("Comment ID reservation failed: {reason}")).color(theme::WARN));
            if ui.button("Retry Comment IDs").clicked() {
                let ctx = ui.ctx().clone();
                self.retry_comment_ids(&ctx, slot);
            }
        }
        let v = &self.views[slot];
        let waiting = match (&v.data, &v.comments.target) {
            (None, _) => Some("파일을 열면 여기에 나와요."),
            (Some(_), None) => Some("Comment 파일을 읽는 중이에요."),
            _ => None,
        };
        if let Some(t) = waiting {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(t).color(theme::TEXT_DIM));
            });
            return;
        }
        let scroll_to = self.comment_scroll.filter(|(f, _)| *f == v.file_id).map(|(_, id)| id);
        // During a marker edit its cell shows the selection now.
        let live_markers = (self.marker_edit() == Some((slot, v.file_id))).then(|| v.selected_names().join(", "));
        let (first, last) = (v.pb.first, v.pb.last);
        let input = comment_list::ListIn { list: &v.comments.list, file_id: v.file_id, frame: v.pb.frame, sort: self.comment_sort, write, scroll_to, picked: v.picked, selection: &v.comment_selection, live_markers, first, last };
        let show = if v.remote.is_some() { comment_list::show_remote } else { comment_list::show };
        let mut out = show(ui, input, &mut self.comment_order, &mut self.comment_edit);
        #[cfg(test)]
        {
            self.comment_rows = out.rows.clone();
            self.type_items = out.items.clone();
        }
        if scroll_to.is_some() {
            self.comment_scroll = None;
        }
        out.slot = slot;
        self.comment_out = out;
    }

    /// The COMMENTS pane for a TAK (no Comments): why, and the `.takm으로 저장` button (dim with
    /// the reason under it when it cannot be used now). No hover text.
    fn tak_comments_tab(&mut self, ui: &mut egui::Ui, slot: usize) {
        let tak = self.views[slot].path().map(Path::to_path_buf);
        let allowed = self.takm_allowed(tak.as_deref());
        let line = |ui: &mut egui::Ui, t: String, color| {
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(t).size(comment_list::LINE_FONT).color(color)).wrap());
            });
        };
        ui.add_space(6.0);
        line(ui, views::TAK_NO_COMMENTS.to_owned(), theme::WARN);
        ui.add_space(4.0);
        let mut clicked = false;
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            clicked = ui.add_enabled(allowed.is_ok(), egui::Button::new(".takm으로 저장")).clicked();
        });
        match (&allowed, self.takm_working()) {
            (_, Some(w)) => line(ui, w, theme::TEXT_DIM),
            (Err(why), None) => line(ui, why.clone(), theme::TEXT_DIM),
            (Ok(_), None) => line(ui, "TAK 옆에 같은 이름의 .takm을 만들고 이 화면에 열어요. TAK는 그대로예요.".to_owned(), theme::TEXT_DIM),
        }
        if let (true, Ok(tak)) = (clicked, allowed) {
            let ctx = ui.ctx().clone();
            self.save_takm(&ctx, tak);
        }
    }

    fn after_comments_tab(&mut self, now: f64, ctx: &egui::Context) {
        let out = std::mem::take(&mut self.comment_out);
        if !out.drawn {
            self.cancel_comment_edit();
        }
        if let Some(frame) = out.scrub {
            self.scrub_comment_frame(frame, now);
        }
        let slot = out.slot;
        let editing = self.comment_edit.as_ref().map(|e| e.id);
        match out.action {
            Some(ListAction::Select(id) | ListAction::Goto(id)) if editing == Some(id) => {}
            Some(ListAction::MultiSelect { id, ctrl, shift }) => {
                self.cancel_comment_edit();
                self.comment_gesture(slot, id, ctrl, shift);
            }
            Some(ListAction::Select(id)) => {
                self.cancel_comment_edit();
                self.select_row(slot, id);
            }
            Some(ListAction::Goto(id)) => {
                self.cancel_comment_edit();
                let file_id = self.views[slot].file_id;
                if let Some((s, f, i, number)) = self.row_select.take() {
                    if (s, f, i) == (slot, file_id, id) {
                        self.reopen_row_select(slot, id, number);
                    }
                }
                self.views[slot].goto_comment(id, now);
            }
            Some(ListAction::Delete(id)) => self.delete_comment(ctx, slot, id),
            Some(ListAction::Sort(key)) => self.set_comment_sort(self.comment_sort.clicked(key)),
            Some(ListAction::Edit(id, field)) if editing != Some(id) => {
                if slot == self.active {
                    self.start_comment_edit(id, field);
                }
            }
            // A click on a Type box (type-combo): a row's single click (select, no frame move
            // since v0.27.3; another edit going on is cancelled first) and its list opens. A
            // single click on the box whose list is open closes it; the second click of a
            // double click keeps it open (and does not move either).
            Some(ListAction::TypeBox(id, double)) => {
                let open = self.comment_edit.as_ref().is_some_and(|e| e.id == id && e.field == Field::Type);
                if open && double {
                    return; // (the last thing here)
                }
                self.cancel_comment_edit();
                self.select_row(slot, id);
                if !open && slot == self.active {
                    self.start_comment_edit(id, Field::Type);
                }
            }
            // A click on an item of the type list (it is in the edit already): saved at once.
            Some(ListAction::SaveEdit) => self.save_comment_edit(ctx),
            Some(ListAction::Edit(..)) | None => {}
        }
    }

    /// A single click on comment `id`'s row (or Type box) in view `slot` (v0.27.3): the comment
    /// and its markers are selected, the frame and playback stay (`View::select_comment`). The
    /// selection is recorded at the end of the frame as usual; which action number it gets is
    /// kept for a double click on `#` right after (`row_select`).
    fn select_row(&mut self, slot: usize, id: u64) {
        let file_id = self.views[slot].file_id;
        self.views[slot].select_comment(id);
        self.row_select = Some((slot, file_id, id, self.history.pushed + 1));
    }

    /// The graph is in one place: while its pane is open (menu, Ctrl+J, a dragged pane) it does
    /// not float. Run at the start and again at the end of a frame, so a layout change and this
    /// follow-up are one undo action.
    fn graph_in_one_place(&mut self) {
        if self.settings.graph_float && dock::has(&self.dock, Pane::Graph) {
            self.settings.graph_float = false;
        }
    }

    /// Motive Shift+F: zoom to fit all (applied when the 3D view size is known).
    fn fit_all(&mut self) {
        self.view_mut().fit_pending = true;
    }

    /// Keys act on the active view.
    fn handle_keys(&mut self, ctx: &egui::Context, now: f64) {
        if self.typing(ctx) || self.dirty_prompt.is_some() || ctx.memory(|m| m.any_popup_open()) {
            return;
        }
        // Ctrl+Z / Ctrl+Y work on the whole app (v0.16.0), also with no file in the active view.
        self.undo_keys(ctx);
        // A focused 3D tab takes precedence, including the final view;
        // a panel keeps the last chosen 3D view as the shortcut's target.
        if self.hold.is_none() && self.comment_edit.is_none() && !ctx.input(|i| i.key_down(Key::M))
            && close_view_key(ctx)
        {
            let open = self.open_views();
            if !open.is_empty() {
                let slot = match self.dock.find_active_focused().map(|(_, tab)| *tab) {
                    Some(Pane::View(slot)) => usize::from(slot),
                    _ => self.active,
                };
                if open.contains(&slot) {
                    self.close_view(slot);
                }
            }
            return;
        }
        if self.view().data.is_none() {
            return;
        }
        if exact_key(ctx, Key::S, true, false, false) && self.hold.is_none() && self.comment_edit.is_none() && !ctx.memory(|m| m.any_popup_open()) {
            self.save_markers(ctx, self.active, false, false);
        }
        self.marker_keys(ctx);
        // `T` (step 5): the selected comment's next type. A real press only (an auto-repeat
        // of a held T is used up and does nothing more).
        let t = ctx.input_mut(|i| {
            let mut hit = false;
            i.events.retain(|e| match e {
                egui::Event::Key { key: Key::T, pressed: true, repeat, modifiers, .. } if modifiers.is_none() => {
                    hit |= !repeat;
                    false
                }
                _ => true,
            });
            hit
        });
        if t {
            self.cycle_type(ctx);
        }
        let edit_keys_allowed = self.hold.is_none() && self.comment_edit.is_none();
        if edit_keys_allowed && exact_key(ctx, Key::L, false, false, false) {
            self.settings.scene.names = !self.settings.scene.names;
        }
        let pointer = ctx.input(|input| input.pointer.hover_pos());
        let fit_all = edit_keys_allowed && exact_key(ctx, Key::F, false, true, false);
        let fit_selected = edit_keys_allowed && exact_key(ctx, Key::F, false, false, false);
        if fit_all || fit_selected {
            let slot = pointer.and_then(|p| self.open_views().into_iter().find(|&slot| {
                let view = &self.views[slot];
                view.scene_rect.contains(p) && !view.graph_rect.is_some_and(|r| r.contains(p)) && !view.plate_rect.is_some_and(|r| r.contains(p))
            }));
            if let Some(slot) = slot {
                if fit_all { self.views[slot].fit_pending = true; } else { self.views[slot].focus_selected(); }
            }
        }
        let v = &mut self.views[self.active];
        // Consume keys before widgets so a focused button/slider never steals Space or arrows.
        let pressed = |key: Key| exact_key(ctx, key, false, false, false);
        if pressed(Key::Space) {
            v.pb.toggle(now);
        }
        if pressed(Key::ArrowRight) {
            v.pb.step(1, now);
        }
        if pressed(Key::ArrowLeft) {
            v.pb.step(-1, now);
        }
        if pressed(Key::ArrowUp) {
            v.pb.step(10, now);
        }
        if pressed(Key::ArrowDown) {
            v.pb.step(-10, now);
        }
        if pressed(Key::Home) {
            v.pb.step(i64::MIN / 2, now);
        }
        if pressed(Key::End) {
            v.pb.step(i64::MAX / 2, now);
        }
        // Motive Z / Shift+Z: nearest boundary across all selected trajectories.
        let shift_z = edit_keys_allowed && exact_key(ctx, Key::Z, false, true, false);
        let z = edit_keys_allowed && pressed(Key::Z);
        let mut no_gap = false;
        if z || shift_z {
            if let Some(f) = v.next_selected_gap(z) {
                v.pb.step(f - v.pb.frame, now);
            } else {
                no_gap = true;
            }
        }
        // Frame range of the playback bar's band strip: `[` = start here, `]` = end here.
        if pressed(Key::OpenBracket) {
            v.range_start_here();
        }
        if pressed(Key::CloseBracket) {
            v.range_end_here();
        }
        if edit_keys_allowed && pressed(Key::V) { v.range_start_here(); }
        if edit_keys_allowed && pressed(Key::B) { v.range_end_here(); }
        if edit_keys_allowed && exact_key(ctx, Key::B, false, true, false) { v.range = None; }
        if edit_keys_allowed && exact_key(ctx, Key::V, false, true, false) {
            if let Some(data) = &v.data { v.range = Some((data.start_frame, data.end_frame)); }
        }
        // Plan step 7: `1` / `2` hide or show the markers of this view's own file / the file laid
        // over it (as a click on name tag 1 / 2). Nothing without an overlay.
        for (key, which) in [(Key::Num1, 0), (Key::Num2, 1)] {
            if pressed(key) && v.overlay.is_some() {
                v.shown[which] = !v.shown[which];
            }
        }
        if no_gap {
            self.takm_note = Some(("빈 구간을 찾지 못했어요.".into(), now));
        }
    }

    fn undo_keys(&mut self, ctx: &egui::Context) {
        exact_key(ctx, Key::Z, true, true, false);
        let redo = ctrl_key(ctx, Key::Y);
        let undo = ctrl_key(ctx, Key::Z) || exact_key(ctx, Key::Z, true, false, true);
        // Not while a cell of the COMMENTS pane is edited (Enter / Esc first; its line at the
        // top says so).
        if self.hold.is_none() && self.comment_edit.is_none() {
            if undo {
                self.undo_redo(ctx, false);
            }
            if redo {
                self.undo_redo(ctx, true);
            }
        }
    }

    fn apply(&mut self, action: Option<Action>, now: f64) {
        if self.dirty_prompt.is_some() { return; }
        match action {
            Some(Action::Select { marker, additive }) => {
                self.view_mut().select(marker, additive);
            }
            Some(Action::GotoFrame(f)) => {
                let v = self.view_mut();
                v.pb.playing = false;
                v.pb.set_frame(f, now);
            }
            Some(Action::GraphOver3d) => self.set_graph_float(true),
            Some(Action::ChoosePlate(p)) => self.view_mut().toggle_plate(p),
            None => {}
        }
    }

    fn menu_bar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::side_top_panel(&ctx.style()).fill(theme::MENU_BG);
        egui::TopBottomPanel::top("menu").frame(frame).show(ctx, |ui| {
            if self.dirty_prompt.is_some() { ui.disable(); }
            egui::menu::bar(ui, |ui| {
                ui.menu_button("파일", |ui| {
                    if ui.button("파일 열기…  (Ctrl+O)").clicked() {
                        ui.close_menu();
                        self.pick_file(ctx, false);
                    }
                    if ui
                        .button(format!("옆 화면에 파일 열기…  · 3D 화면 최대 {MAX_VIEWS}개"))
                        .on_hover_text("Explorer에서 Ctrl+클릭해도 옆 화면에 열려요.")
                        .clicked()
                    {
                        ui.close_menu();
                        self.pick_file(ctx, true);
                    }
                    if ui
                        .add_enabled(!self.workspace.is_full(), egui::Button::new(format!("폴더 추가…  (Ctrl+Shift+O)  · 최대 {MAX_ROOTS}개")))
                        .on_disabled_hover_text(format!("저장 폴더 {MAX_ROOTS}개가 다 찼어요. Explorer에서 ✕로 하나 빼세요."))
                        .clicked()
                    {
                        ui.close_menu();
                        self.pick_folder();
                    }
                    if ui.button("경로로 열기…  (Ctrl+L)  · WSL 경로 가능").clicked() {
                        ui.close_menu();
                        self.open_path_dialog();
                    }
                    if ui.add_enabled(self.view().data.is_some(), egui::Button::new("저장  (Ctrl+S)")).clicked() {
                        ui.close_menu();
                        self.save_markers(ctx, self.active, false, false);
                    }
                    if ui.add_enabled(self.view().data.is_some(), egui::Button::new("다른 이름으로 저장…")).clicked() {
                        ui.close_menu();
                        self.save_markers(ctx, self.active, true, false);
                    }
                    // `.takm` step ③: the active view's TAK as `.takm` (dim, with the reason).
                    let takm = self.takm_allowed(self.current_path());
                    if ui.add_enabled(takm.is_ok(), egui::Button::new(".takm으로 저장")).clicked() {
                        ui.close_menu();
                        if let Ok(t) = takm.clone() {
                            self.save_takm(ctx, t);
                        }
                    }
                    if let Err(why) = &takm {
                        ui.label(RichText::new(why).size(11.5).color(theme::TEXT_DIM));
                    }
                    ui.separator();
                    ui.menu_button("최근 폴더", |ui| {
                        if self.settings.recent_workspaces.is_empty() {
                            ui.label("없음");
                        }
                        for r in self.settings.recent_workspaces.clone() {
                            if ui.button(&r).on_hover_text("저장 폴더에 추가").clicked() {
                                ui.close_menu();
                                self.open_workspace(PathBuf::from(r));
                            }
                        }
                    });
                    ui.menu_button("최근 파일", |ui| {
                        if self.settings.recent.is_empty() {
                            ui.label("없음");
                        }
                        for r in self.settings.recent.clone() {
                            if ui.button(&r).clicked() {
                                ui.close_menu();
                                self.open(ctx, PathBuf::from(r));
                            }
                        }
                    });
                    if ui
                        .add_enabled(self.view().data.is_some() && self.capture.is_none(), egui::Button::new("화면 캡처 저장 (지금 + 위·옆·앞)  (Ctrl+Shift+S)"))
                        .on_hover_text("repo 안 experiments/tak_captures/ 에 PNG, sheet.png, captures.json 저장")
                        .clicked()
                    {
                        ui.close_menu();
                        self.start_menu_capture();
                    }
                    if let Some(p) = self.current_path().and_then(workspace::to_wsl_path) {
                        if ui.button("현재 파일 WSL 경로 복사").on_hover_text(&p).clicked() {
                            ui.close_menu();
                            ctx.output_mut(|o| o.copied_text = p);
                        }
                    }
                    ui.separator();
                    if ui.button("끝내기").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("보기", |ui| {
                    ui.label(RichText::new("패널 한쪽 전체  (오른쪽 위 버튼과 같아요)").color(theme::TEXT_DIM));
                    for (side, label) in SIDE_TOGGLES {
                        let mut on = dock::side_open(&self.dock, self.dock_area, side);
                        if ui.checkbox(&mut on, label).changed() {
                            self.toggle_side(side);
                        }
                    }
                    ui.separator();
                    ui.label(RichText::new("패널 하나씩  (제목을 마우스로 끌어 옮겨요)").color(theme::TEXT_DIM));
                    for (pane, label) in [
                        (Pane::Explorer, "Explorer (폴더 트리)"),
                        (Pane::Labeling, "Labeling"),
                        (Pane::EditTools, "Edit Tools"),
                        (Pane::Selected, "선택 마커"),
                        (Pane::Devices, "장치 (forceplate · EMG)"),
                        (Pane::Comments, "Comments 목록"),
                        (Pane::Graph, "Graph 패널 (선택 마커 X·Y·Z, 3D 아래)"),
                    ] {
                        let mut on = dock::has(&self.dock, pane);
                        if ui.checkbox(&mut on, label).changed() {
                            if on {
                                self.show_pane(pane);
                            } else {
                                self.hide_pane(pane);
                            }
                        }
                    }
                    let mut float = self.settings.graph_float;
                    if ui.checkbox(&mut float, "그래프를 3D 화면 위에 띄우기").on_hover_text("끄면 그래프가 3D 아래 Graph 패널로 돌아가요.").changed() {
                        self.set_graph_float(float);
                    }
                    if ui.button("패널 배치 처음대로").clicked() {
                        ui.close_menu();
                        let open: Vec<u8> = dock::views(&self.dock);
                        self.dock = dock::default_layout_for(&open);
                        self.hidden = dock::Hidden::default();
                        self.settings.graph_float = true;
                        self.activate(self.active);
                    }
                    ui.separator();
                    ui.label(RichText::new("3D 화면").color(theme::TEXT_DIM));
                    let s = &mut self.settings.scene;
                    ui.checkbox(&mut s.grid, "바닥 격자");
                    ui.checkbox(&mut s.axes, "원점 축");
                    ui.checkbox(&mut s.names, "마커 이름 모두 표시");
                    ui.checkbox(&mut s.unlabeled, "Unlabeled 마커");
                    ui.checkbox(&mut s.sticks, "Marker Sticks (마커 사이 선)");
                    ui.add_enabled_ui(s.sticks, |ui| ui.checkbox(&mut s.standard_sticks, "  파일에 선이 없으면 표준 선 표시"));
                    ui.checkbox(&mut s.plates, "Force plate");
                    ui.checkbox(&mut s.asset_names, "Asset 이름표");
                    ui.checkbox(&mut s.history, "선택 마커 지나온 길 (250 프레임)");
                    ui.checkbox(&mut s.marker_colors, "마커 색 (끄면 흰색)");
                    ui.add_enabled(false, egui::Checkbox::new(&mut s.bones, "스켈레톤 뼈 (준비 중)"));
                    ui.separator();
                    if ui.button("화면 맞춤 (Shift+F)").clicked() {
                        ui.close_menu();
                        self.fit_all();
                    }
                    ui.separator();
                    if ui.button("설정…").on_hover_text("3D 위에 뜬 그래프의 바탕 진하기").clicked() {
                        ui.close_menu();
                        self.show_settings = true;
                    }
                });
                ui.menu_button("도움말", |ui| {
                    if ui.button("조작키").clicked() {
                        ui.close_menu();
                        self.show_help = true;
                    }
                    ui.separator();
                    ui.label(RichText::new(format!("TAK 뷰어 v{}", crate::version::VERSION)).weak());
                });
                // Side toggles at the right end (VS Code layout controls): left, bottom, right.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(4.0);
                    for (side, label) in SIDE_TOGGLES.into_iter().rev() {
                        let on = dock::side_open(&self.dock, self.dock_area, side);
                        let tip = format!("{label}\n{}", if on { "누르면 숨겨요. 다시 누르면 그 자리로 돌아와요." } else { "누르면 다시 보여요." });
                        if side_toggle(ui, side, on).on_hover_text(tip).clicked() && self.capture.is_none() {
                            self.toggle_side(side);
                        }
                    }
                });
            });
        });
    }

    /// Bottom line of the window (VS Code status bar): the active view's file, its Progress,
    /// frame rate, the unit note and the read warnings.
    fn status_bar(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let frame = egui::Frame::none().fill(theme::STATUS_BG).inner_margin(egui::Margin::symmetric(10.0, 0.0));
        let mut show_warnings = false;
        let mut open_comments = false;
        egui::TopBottomPanel::bottom("status").exact_height(STATUS_H).frame(frame).show(ctx, |ui| {
            if self.dirty_prompt.is_some() { ui.disable(); }
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                let text = |s: String| RichText::new(s).size(12.0).color(theme::STATUS_TEXT);
                if !self.ssh_status(ui) {
                match &self.views[self.active].data {
                    Some(d) => {
                        let open = self.open_views();
                        ui.label(text(self.view_label(self.active, &open).unwrap_or_else(|| d.file_name())).strong());
                        if let Some(p) = d.take.meta("Progress") {
                            ui.label(text(format!("Progress {p}")));
                        }
                        ui.label(text(format!("{:.0} Hz", d.take.frame_rate)));
                        if (d.unit_scale - 1.0).abs() > f32::EPSILON {
                            ui.label(text("좌표를 mm로 보고 m로 환산".to_owned()));
                        }
                        if !d.take.warnings.is_empty() {
                            let warn = egui::Label::new(text(format!("⚠ 경고 {}", d.take.warnings.len())).strong()).sense(egui::Sense::click());
                            if ui.add(warn).on_hover_text("읽기 경고 보기").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                show_warnings = true;
                            }
                        }
                        // Comments of this file, and how saving them went.
                        if let Some((s, tip, warn)) = self.comment_status(self.active, now) {
                            let label = if warn { text(format!("⚠ {s}")).strong() } else { text(s) };
                            let item = ui.add(egui::Label::new(label).sense(egui::Sense::click())).on_hover_text(format!("{tip}\n누르기 = Comments 목록 보기")).on_hover_cursor(egui::CursorIcon::PointingHand);
                            if item.clicked() {
                                open_comments = true;
                            }
                        }
                        if let Some(t) = self.views[self.active].comments.saved_at {
                            if now - t < SAVED_NOTE_S {
                                ctx.request_repaint_after(Duration::from_secs_f64(SAVED_NOTE_S - (now - t) + 0.05));
                            }
                        }
                        // After a delete, undo or redo: what happened, for a few seconds.
                        if let Some((note, t)) = &self.history.note {
                            if now - t < CHANGE_NOTE_S {
                                ui.label(text(note.clone()));
                                ctx.request_repaint_after(Duration::from_secs_f64(CHANGE_NOTE_S - (now - t) + 0.05));
                            }
                        }
                    }
                    None => {
                        ui.label(text("파일을 열면 여기에 파일 정보가 나와요.".to_owned()));
                    }
                }
                }
                if let Some((note, t)) = &self.ssh_note {
                    if now - t < 8.0 && self.view().remote_warning.as_ref() != Some(note) {
                        ui.label(text(note.clone()).strong());
                        ctx.request_repaint_after(Duration::from_secs_f64(8.0 - (now - t) + 0.05));
                    }
                }
                // "`.takm`으로 저장" running, then its result; or why a TAK takes no Comment.
                if let Some(w) = self.takm_working() {
                    ui.label(text(w).strong());
                } else if let Some((note, t)) = &self.takm_note {
                    if now - t < takm_save::NOTE_S {
                        ui.label(text(note.clone()).strong());
                        ctx.request_repaint_after(Duration::from_secs_f64(takm_save::NOTE_S - (now - t) + 0.05));
                    }
                }
                // Why a file could not be laid over a view (plan step 7), for a few seconds.
                if let Some((note, t)) = &self.overlay_note {
                    if now - t < overlay::NOTE_S {
                        ui.label(text(note.clone()).strong());
                        ctx.request_repaint_after(Duration::from_secs_f64(overlay::NOTE_S - (now - t) + 0.05));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(text(format!("TAK 뷰어 v{}", crate::version::VERSION)));
                });
            });
        });
        open_comments |= ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::new("ssh_open_comments"))).unwrap_or(false);
        if open_comments {
            self.show_pane(Pane::Comments);
        }
        if show_warnings {
            self.show_warnings = true;
        }
    }

    /// What a click in the Explorer asked for (applied after drawing).
    fn explorer_action(&mut self, ctx: &egui::Context, a: WsAction) {
        if self.dirty_prompt.is_some() { return; }
        match a {
            WsAction::Open(p) => self.open(ctx, p),
            WsAction::OpenSide(p) => self.open_side(ctx, p),
            WsAction::OpenNewTab(p) => {
                if self.allow_open_file(&p, None) {
                    let id = self.new_layout_tab();
                    self.open(ctx, p);
                    // The context-menu command creates the tab and opens its file as one
                    // action; update that tab's lifecycle snapshot instead of recording a
                    // second layout action when the view appears.
                    self.observe(false);
                    if let Some(snapshot) = self.tab_undo_snapshot(id) {
                        if let Some([undo::Part::TabLifecycle { id: action_id, snapshot: saved, .. }]) =
                            self.history.undo.last_mut().map(Vec::as_mut_slice) {
                            if *action_id == id { *saved = snapshot; }
                        }
                    }
                }
            }
            WsAction::PickFolder => self.pick_folder(),
            WsAction::EnterPath => self.open_path_dialog(),
            WsAction::Overlay(p) => self.overlay_on(ctx, p),
            WsAction::OverlayOff => self.overlay_off(self.active),
            WsAction::ToTakm(p) => self.save_takm(ctx, p),
        }
    }

    /// File dialog; `side` = open in a new view next to the others. The first filter shows both
    /// kinds the viewer opens (`OPEN_FILTERS`).
    fn pick_file(&mut self, ctx: &egui::Context, side: bool) {
        let mut dialog = rfd::FileDialog::new();
        for (name, exts) in OPEN_FILTERS {
            dialog = dialog.add_filter(name, exts);
        }
        if let Some(dir) = self.settings.recent.first().and_then(|r| PathBuf::from(r).parent().map(|p| p.to_path_buf())) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            if side {
                self.open_side(ctx, path);
            } else {
                self.open(ctx, path);
            }
        }
    }

    /// One 3D view tab: the 3D (framing, picking, click / box selection, click on a force plate)
    /// with its own playback bar under it and, floating over the 3D (`graph_float.rs`), the
    /// selected marker's X / Y / Z graph (not while the graph is in the GRAPH pane) and the
    /// chosen plate's graph (the lanes Fx … Mz chosen for this view slot, once the forces are
    /// in). With `tag` (a view alone in its place)
    /// the 3D also covers the tab row and the tab is painted on it as a name tag; the real tab
    /// underneath still takes the drag, ✕ and click, the rest of the row is 3D (mouse and
    /// floating graphs) like the space below it.
    /// Pressing anywhere in it makes it the active view; with several views the
    /// active one has a blue edge around the 3D and its bar.
    fn scene_tab(&mut self, ui: &mut egui::Ui, slot: usize, several: bool, tag: Option<dock::ViewTag>) -> bool {
        let body = ui.available_rect_before_wrap();
        let deck_top = (body.bottom() - panels::DECK_H).max(body.top());
        let deck_rect = egui::Rect::from_min_max(egui::pos2(body.left(), deck_top), body.max);
        let top = if tag.is_some() { body.top() - dock::TAB_H } else { body.top() };
        // The 3D on screen. It takes the mouse below the tab row (`input`) and in the row right
        // of the name tag (`row`); on the name tag the tab under it keeps the mouse.
        let rect = egui::Rect::from_min_max(egui::pos2(body.left(), top), egui::pos2(body.right(), deck_top));
        let input = egui::Rect::from_min_max(body.min, rect.max);
        let name_tag = tag.as_ref().map_or(egui::Rect::NOTHING, |t| t.rect);
        let row = tag.as_ref().map_or(egui::Rect::NOTHING, |t| egui::Rect::from_min_max(egui::pos2(t.rect.right(), rect.top()), egui::pos2(rect.right(), body.top())));
        let whole = rect.union(deck_rect);
        let capturing = self.capture.is_some();
        // Pictures show the 3D only: no name tag or blue edge while capturing.
        let tag = tag.filter(|t| !capturing && !t.dragged);
        let mut tag_size = tag.as_ref().map_or(egui::Vec2::ZERO, |t| t.rect.max - rect.min);
        // A file laid over this view (plan step 7): name tags 1 / 2 next to the file name tag (not
        // in pictures of a capture). The HUD and the floating graphs' tags keep clear of them.
        let overlay_tags = if capturing { None } else { self.overlay_tags(ui.ctx(), slot, rect, tag.as_ref().map(|t| t.rect)) };
        let mut name_tag = name_tag;
        if let Some(t) = &overlay_tags {
            tag_size = tag_size.max(t.area().max - rect.min);
            name_tag = if name_tag.is_positive() { name_tag.union(t.area()) } else { t.area() };
        }
        key_focus::region(ui, row, key_focus::Target::Markers(slot));
        key_focus::region(ui, name_tag, key_focus::Target::Blocked);
        let chosen = several && slot == self.active && !capturing;
        let hud = match &self.capture {
            Some(c) if slot == self.active => c.hud_label(),
            _ => "Perspective".to_owned(),
        };
        let now = ui.input(|i| i.time);
        let make = self.comment_ready(slot);
        let can_delete = self.comment_target(slot).is_some();
        let held = self.hold.as_ref().filter(|h| h.slot == slot).map(|h| h.start);
        let editing = self.comment_edit.is_some();
        let draft = self.frames_draft(slot);
        // The type strip of a comment just made in this view (not in pictures of a capture).
        let file_now = self.views[slot].file_id;
        let strip = self.type_strip.as_ref().filter(|s| s.slot == slot && s.file_id == file_now && !capturing).map(|s| (s.id, panels::type_strip_rects(ui.ctx(), deck_rect)));
        let quick_available = self.quick_label_available(ui.ctx(), slot);
        let v = &mut self.views[slot];
        if v.fit_pending {
            if let Some((lo, hi)) = v.data.as_ref().and_then(|d| d.bounds) {
                v.cam.frame_box(v3(lo.x, 0.0_f32.min(lo.y), lo.z), hi, rect.width() / rect.height().max(1.0));
            }
            v.fit_pending = false;
        }
        v.scene_rect = rect;
        // A press on this view (3D, bar or name tag), not on a menu or list popped over it.
        let press = ui.input(|i| if i.pointer.any_pressed() { i.pointer.press_origin() } else { None });
        let pressed = press.is_some_and(|p| whole.contains(p) && ui.ctx().layer_id_at(p).map_or(true, |l| l == ui.layer_id()));
        let scene_opts = &self.settings.scene;
        // The selected marker's graph and the chosen plate's graph float over this 3D (not in
        // pictures), anywhere in the 3D as drawn (tab row too, under the name tag).
        let gbox = &mut self.settings.graph_boxes[slot];
        let pbox = &mut self.settings.plate_boxes[slot];
        let pset = &mut self.settings.plate_channels[slot];
        let float = self.settings.graph_float && !capturing;
        let graph_marker = v.data.as_ref().and_then(|d| v.primary.filter(|m| float && *m < d.markers.len()));
        // No plate graph (and no note) while the forces are still loading. Its lanes: those
        // chosen for this view slot that the plate's device has.
        let graph_plate = v.data.as_ref().and_then(|d| {
            let pi = v.plate.filter(|p| !capturing && !d.analog_pending && *p < d.plates.len())?;
            let offer = panels::plate_offer(d, pi);
            Some((pi, Kind::Plate { shown: pset.shown(offer), offer }))
        });
        v.graph_rect = graph_marker.filter(|_| !gbox.collapsed).map(|_| graph_float::open_rect(Kind::Marker, gbox, rect));
        v.plate_rect = graph_plate.filter(|_| !pbox.collapsed).map(|(_, kind)| graph_float::open_rect(kind, pbox, rect));
        let strip_rect = strip.as_ref().map(|(_, b)| panels::type_strip_rect(b));
        let covers: Vec<egui::Rect> = v.graph_rect.into_iter().chain(v.plate_rect).chain(strip_rect).collect();
        // Plan step 7: whose markers are drawn (the laid-over file at the same place from the
        // first frame, `View::overlay_frame`).
        let overlay_frame = v.overlay_frame();
        let layers = scene::Layers {
            own: v.shown[0] || v.overlay.is_none(),
            overlay: v.overlay.as_ref().zip(overlay_frame).filter(|_| v.shown[1]).map(|(o, f)| (o.data.as_ref(), f)),
        };
        let out = ui
            .allocate_new_ui(egui::UiBuilder::new().max_rect(input), |ui| {
                scene::show(ui, rect, row, tag_size, &covers, v.data.as_deref(), v.pb.frame, &mut v.cam, scene_opts, &v.selected, v.primary, v.plate, &hud, layers)
            })
            .inner;
        let quick = quick_available && self.marker_ui[slot].labeling.mode == crate::ui::labeling::LabelingMode::QuickLabel && v.armed_label.is_some();
        if let Some(index) = out.context_clicked {
            if !v.selected.contains(&index) { v.select(index, false); }
        }
        if out.remove_marker { self.marker_delete = Some(slot); }
        let is_unlabeled = |index: usize| v.data.as_ref().and_then(|d| d.markers.get(index)).is_some_and(|m| m.kind == tak_core::tak::MarkerKind::Unlabeled);
        let quick_click = out.clicked.flatten().filter(|&index| quick && is_unlabeled(index));
        let quick_double = out.double_clicked.is_some_and(|index| quick && is_unlabeled(index));
        if let Some(index) = quick_click.filter(|_| out.double_clicked.is_none()) {
            if let Some(marker) = v.data.as_ref().and_then(|data| data.markers.get(index)) { self.marker_click = Some((slot, marker.uid)); }
        }
        match (out.clicked.filter(|_| quick_click.is_none()), out.plate) {
            (Some(Some(m)), _) => v.select(m, out.ctrl),
            // On a plate: choose / un-choose it; the marker selection stays (both graphs can be up).
            (Some(None), Some(p)) => v.toggle_plate(p),
            (Some(None), None) if !out.ctrl => v.clear_selection(),
            _ => {}
        }
        if let Some((markers, primary)) = out.box_selected {
            v.select_many(markers, primary, out.ctrl);
        }
        if let Some(m) = out.double_clicked.filter(|_| !quick_double) {
            v.select(m, false);
            v.focus_selected();
        }
        if quick {
            if let (Some(key), Some(pointer)) = (v.armed_label, ui.ctx().pointer_hover_pos().filter(|p| rect.contains(*p))) {
                if let Some(name) = self.marker_ui[slot].labeling.groups.iter().flat_map(|g| &g.labels).find(|row| row.key == Some(key)).map(|row| &row.name) {
                    ui.painter().text(pointer + egui::vec2(14.0, 16.0), egui::Align2::LEFT_TOP, name, egui::FontId::proportional(12.0), theme::ACCENT);
                }
            }
        }
        if let Some((path, _)) = &v.loading {
            // Reading a file into this view: a small note in its middle.
            let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let box_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width().min(360.0), 34.0));
            ui.painter().rect_filled(box_rect, 4.0, theme::PANEL_BG);
            let spin = egui::Rect::from_center_size(box_rect.left_center() + egui::vec2(18.0, 0.0), egui::vec2(16.0, 16.0));
            ui.put(spin, egui::Spinner::new());
            ui.painter().with_clip_rect(box_rect).text(
                box_rect.left_center() + egui::vec2(34.0, 0.0),
                egui::Align2::LEFT_CENTER,
                format!("불러오는 중: {name}"),
                egui::FontId::proportional(14.0),
                theme::TEXT,
            );
        }
        let bar = CommentBar { list: &v.comments.list, blocks: &mut v.comments.blocks, make, can_delete, held, picked: v.picked, draft };
        // (Undo: a band drag, like any drag, is one action when the mouse is let go, `app_undo.rs`.)
        let deck = panels::deck(ui, deck_rect, slot, v.data.as_deref(), &mut v.pb, &mut v.range, v.primary, &mut v.heat, v.file_id, now, bar);
        match deck.action {
            // During an edit of the COMMENTS pane a block click only moves the frame (the
            // selection is the edit's).
            Some(DeckAction::GotoComment(id)) if editing => {
                if let Some(c) = v.comments.list.get(id) {
                    v.pb.playing = false;
                    v.pb.set_frame(c.start, now);
                }
            }
            Some(DeckAction::GotoComment(id)) => {
                v.goto_comment(id, now);
                self.comment_scroll = Some((v.file_id, id));
            }
            Some(other) => self.deck_action = Some((slot, other)),
            None => {}
        }
        // Last, so they are on top of the 3D for the mouse and no other widget's id moves (the
        // plate graph after the marker graph: on top where they overlap). Own child ui each,
        // whose clip rect holds the tab row (egui clips the mouse to the clip rect).
        let opacity = self.settings.graph_opacity;
        let frame = v.pb.frame;
        let mut goto = None;
        if let (Some(mi), Some(d)) = (graph_marker, v.data.as_ref()) {
            let m = &d.markers[mi];
            // v0.27.0: every selected marker (up to `panels::GRAPH_MAX`) in the same lanes.
            let (graphed, total) = panels::graphed(d, &v.selected, mi);
            // Plan step 9: the file laid over the view, dashed in the same lanes.
            v.lanes.pair(d, v.file_id, &graphed, v.overlay.as_ref().map(|o| (o.path.as_path(), o.data.as_ref())), v.shown);
            let (lanes, gaps, drawn) = v.lanes.get(d, v.file_id, &graphed);
            let names: Vec<&str> = graphed.iter().map(|&k| d.markers[k].name.as_str()).collect();
            let spans = v.comments.spans.get_many(&v.comments.list, v.file_id, &names, v.picked);
            let keys = panels::graph_keys(d, &graphed, frame);
            let g = graph_float::Graph {
                name: &m.name,
                name_color: if m.present(frame) { theme::LABEL_TRACKED } else { theme::LABEL_UNTRACKED },
                mark: theme::color(m.color),
                lanes,
                drawn,
                gaps,
                plot_id: egui::Id::new(("sel_graph", v.file_id)),
                first: d.start_frame,
                last: d.end_frame,
                frame,
                selection: v.range,
                spans,
                keys: &keys,
                left_out: total - graphed.len(),
            };
            let mut over_row = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            over_row.set_clip_rect(rect);
            let out = graph_float::show(&mut over_row, rect, name_tag, slot, Kind::Marker, gbox, opacity, g);
            goto = out.goto;
            self.graph_to_panel |= out.to_panel;
        }
        if let (Some((pi, kind @ Kind::Plate { shown, .. })), Some(d)) = (graph_plate, v.data.as_ref()) {
            let (lanes, gaps, drawn) = v.plate_lanes.get(d, v.file_id, pi, shown);
            let g = graph_float::Graph {
                name: &d.plates[pi].name,
                name_color: theme::LABEL_TRACKED,
                mark: theme::PLATE_CHOSEN,
                lanes,
                drawn,
                gaps,
                plot_id: egui::Id::new(("plate_graph", v.file_id, pi)),
                first: d.start_frame,
                last: d.end_frame,
                frame,
                selection: None,
                spans: &[],
                keys: &[],
                left_out: 0,
            };
            let mut over_row = ui.new_child(egui::UiBuilder::new().id_salt("plate_float").max_rect(rect));
            over_row.set_clip_rect(rect);
            let out = graph_float::show(&mut over_row, rect, name_tag, slot, kind, pbox, opacity, g);
            goto = goto.or(out.goto);
            if out.close {
                v.plate = None;
            }
            if let Some(chosen) = out.lanes {
                *pset = chosen;
            }
        }
        if let Some(f) = goto {
            v.pb.playing = false;
            v.pb.set_frame(f, now);
        }
        // The type strip, over the 3D above the bar (after the graphs: on top for the mouse;
        // nothing moves for it).
        if let (Some((id, buttons)), Some(r)) = (strip, strip_rect) {
            let type_now = v.comments.list.get(id).map(|c| c.comment_type.clone()).unwrap_or_default();
            let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("type_strip", slot)).max_rect(r));
            if let Some(t) = panels::type_strip(&mut child, &buttons, &type_now) {
                self.strip_pick = Some(t);
            }
            if let Some(s) = self.type_strip.as_mut() {
                (s.rect, s.buttons) = (r, buttons);
            }
        }
        // Name tag and blue edge may reach over the tab row: own clip rect. Painted after the
        // floating graphs, so the name tag is on top of them.
        let mut over = ui.painter().clone();
        over.set_clip_rect(whole);
        // An Explorer file dragged over this view: where it would go; let go = open it there.
        if let Some(file) = egui::DragAndDrop::payload::<explorer::FileDrag>(ui.ctx()) {
            let at = ui.ctx().pointer_hover_pos().filter(|&p| whole.contains(p) && ui.ctx().layer_id_at(p).map_or(true, |l| l == ui.layer_id()));
            if let Some(p) = at {
                let zone = dock::drop_zone(whole, p);
                let r = dock::drop_preview(whole, zone);
                over.rect_filled(r, 0.0, theme::DROP_FILL);
                over.rect_stroke(r.shrink(1.0), 0.0, egui::Stroke::new(1.5_f32, theme::ACTIVE_VIEW));
                if ui.input(|i| i.pointer.any_released()) {
                    self.file_drop = Some((slot, zone, file.0.clone()));
                }
            }
        }
        if let Some(t) = &tag {
            dock::paint_view_tag(ui, &over, t, chosen, true);
        }
        if let Some(t) = &overlay_tags {
            self.overlay_tags_ui(ui, &over, slot, rect, t);
        }
        if chosen {
            over.rect_stroke(whole.shrink(1.0), 0.0, egui::Stroke::new(2.0_f32, theme::ACTIVE_VIEW));
        }
        pressed
    }

}

/// Draws each pane inside its dock tab (the layout itself is in `ui/dock.rs`).
struct Tabs<'a> {
    app: &'a mut TakViewerApp,
    action: Option<Action>,
    ws_action: Option<WsAction>,
    /// View pressed this frame (becomes the active one).
    pressed_view: Option<usize>,
    /// View whose tab ✕ was pressed this frame.
    closed_view: Option<usize>,
    /// Slots of the views on screen (the layout itself is out of the app while drawing).
    open: Vec<usize>,
    /// Views alone in their place: drawn over their tab row, the tab as a name tag.
    lone: [bool; MAX_VIEWS],
    /// Where egui_dock put those views' tabs this frame (`on_tab_button`, before the view).
    tags: [Option<dock::ViewTag>; MAX_VIEWS],
    /// The views on screen in reading order (OPEN EDITORS groups) and what a click there asked.
    order: Vec<usize>,
    ed_action: Option<crate::ui::open_editors::EditorAction>,
}

impl Tabs<'_> {
    /// A 3D view's tab text: its file name ("…" while reading).
    fn view_title(&self, k: usize) -> Option<String> {
        let label = self.app.view_label(k, &self.open)?;
        Some(if self.app.views[k].loading.is_some() { format!("{label} …") } else { label })
    }
}

impl egui_dock::TabViewer for Tabs<'_> {
    type Tab = Pane;

    /// A 3D view's tab shows its file name ("…" while reading).
    fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
        let Pane::View(k) = *tab else { return RichText::new(tab.title()).size(11.5).into() };
        self.view_title(usize::from(k)).unwrap_or_else(|| tab.title().to_owned()).into()
    }

    fn id(&mut self, tab: &mut Pane) -> egui::Id {
        egui::Id::new(("pane", *tab))
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Pane) {
        let app = &mut *self.app;
        if app.dirty_prompt.is_some() { ui.disable(); }
        let key_target = match *tab {
            Pane::View(slot) => key_focus::Target::Markers(usize::from(slot)),
            Pane::Graph => key_focus::Target::Markers(app.active),
            Pane::Comments => key_focus::Target::Comments,
            _ => key_focus::Target::Blocked,
        };
        key_focus::region(ui, ui.available_rect_before_wrap(), key_target);
        if self.open.is_empty() && !matches!(tab, Pane::Empty | Pane::LayoutTabs | Pane::Explorer) {
            crate::ui::empty::panel(ui, *tab);
            return;
        }
        match tab {
            Pane::LayoutTabs => return,
            Pane::Empty => {
                let body = ui.available_rect_before_wrap();
                let rect = egui::Rect::from_min_max(body.min - egui::vec2(0.0, dock::TAB_H), body.max);
                crate::ui::empty::editor_at(ui, rect);
                if let Some(file) = egui::DragAndDrop::payload::<explorer::FileDrag>(ui.ctx()) {
                    let hovered = ui.ctx().pointer_hover_pos().is_some_and(|p| rect.contains(p)
                        && ui.ctx().layer_id_at(p).map_or(true, |layer| layer == ui.layer_id()));
                    if hovered {
                        ui.painter().rect_filled(rect, 0.0, theme::DROP_FILL);
                        ui.painter().rect_stroke(rect.shrink(1.0), 0.0, egui::Stroke::new(1.5, theme::ACTIVE_VIEW));
                        if ui.input(|i| i.pointer.any_released()) {
                            app.file_drop = Some((app.active, dock::DropZone::Center, file.0.clone()));
                        }
                    }
                }
                return;
            }
            Pane::View(k) => {
                let k = usize::from(*k);
                let tag = self.tags[k].take();
                if app.scene_tab(ui, k, self.open.len() > 1, tag) {
                    self.pressed_view = Some(k);
                }
                return;
            }
            Pane::Explorer => {
                // OPEN EDITORS on top (`ui/open_editors.rs`), the WORKSPACE tree under it.
                let groups = app.editor_groups(&self.order);
                app.editor_selection.retain(&crate::ui::open_editors::displayed_rows(&groups));
                let editors_min = ui.cursor().min;
                if let Some(a) = crate::ui::open_editors::show(ui, &groups, app.workspace.is_full(), &mut app.explorer.editors_collapsed, &app.editor_selection) {
                    self.ed_action = Some(a);
                }
                let editors_rect = egui::Rect::from_min_max(editors_min, egui::pos2(ui.max_rect().right(), ui.cursor().min.y));
                key_focus::region(ui, editors_rect, key_focus::Target::Editors);
                let current = app.view().target().map(Path::to_path_buf);
                // Other open files: the other views' files and every laid-over file.
                let mut others: Vec<PathBuf> = self
                    .open
                    .iter()
                    .copied()
                    .filter(|&k| k != app.active)
                    .filter_map(|k| app.views[k].target().map(Path::to_path_buf))
                    .collect();
                others.extend(self.open.iter().flat_map(|&k| app.views[k].overlay_paths().map(Path::to_path_buf).collect::<Vec<_>>()));
                // The menu offers "얹은 파일 내리기" while the active view has one.
                let overlaid = app.view().overlay_target().is_some();
                let revealing = app.explorer.reveal.is_some().then(|| app.explorer.folds().0.clone());
                // WORKSPACE keeps its existing height when short; a long tree reserves room
                // for the SSH section, so all three section headers remain reachable.
                let available = ui.available_height();
                let reserve = crate::ui::ssh_explorer::desired_height(&app.ssh).min((available * 0.7).max(22.0));
                let workspace_h = (available - reserve).max(26.0);
                let max = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), workspace_h));
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(max), |ui| {
                    if let Some(a) = explorer::show(ui, &mut app.workspace, current.as_deref(), &others, overlaid, &mut app.explorer) {
                        self.ws_action = Some(a);
                    }
                });
                app.ssh_section(ui);
                // Folders the Explorer expanded to show the open file are no action of their own
                // (a folder added in the same frame still is).
                if let Some(before) = revealing.filter(|_| app.explorer.reveal.is_none()) {
                    let added: Vec<PathBuf> = app.explorer.folds().0.difference(&before).cloned().collect();
                    app.history.absorb_expanded(added);
                }
                return;
            }
            Pane::Comments => {
                app.comments_tab(ui);
                return;
            }
            Pane::Labels | Pane::Labeling => {
                app.labeling_tab(ui);
                return;
            }
            Pane::EditTools => {
                app.edit_tools_tab(ui);
                return;
            }
            _ => {}
        }
        let v = &mut app.views[app.active];
        let Some(d) = &v.data else {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("파일을 열면 여기에 나와요.").color(theme::TEXT_DIM));
            });
            return;
        };
        let frame = v.pb.frame;
        let action = match tab {
            Pane::Selected => {
                ui.add_space(4.0);
                panels::selected_pane(ui, d, frame, &v.selected, v.primary)
            }
            Pane::Graph => {
                // v0.27.0: every selected marker (up to `panels::GRAPH_MAX`) in the same lanes.
                let (graphed, total) = v.primary.filter(|m| *m < d.markers.len()).map_or((vec![], 0), |mi| panels::graphed(d, &v.selected, mi));
                // Plan step 9: the file laid over the view, dashed in the same lanes.
                v.lanes.pair(d, v.file_id, &graphed, v.overlay.as_ref().map(|o| (o.path.as_path(), o.data.as_ref())), v.shown);
                // The Comment bands of the graphed markers (step 6).
                let names: Vec<&str> = graphed.iter().map(|&k| d.markers[k].name.as_str()).collect();
                let spans = v.comments.spans.get_many(&v.comments.list, v.file_id, &names, v.picked);
                panels::graph_pane(ui, d, frame, &graphed, total, v.file_id, &mut v.lanes, spans, &mut v.range)
            }
            Pane::Devices => {
                ui.add_space(4.0);
                panels::devices_pane(ui, d, frame, &mut v.devices, v.file_id, v.plate, v.emg_failure.as_deref())
            }
            Pane::Empty | Pane::LayoutTabs | Pane::View(_) | Pane::Explorer | Pane::Comments | Pane::Labels | Pane::Labeling | Pane::EditTools => None,
        };
        if action.is_some() {
            self.action = action;
        }
    }

    /// Every 3D view can close; the empty editor is a permanent layout placeholder.
    fn closeable(&mut self, tab: &mut Pane) -> bool {
        self.app.dirty_prompt.is_none() && !matches!(tab, Pane::Empty | Pane::LayoutTabs)
    }

    fn on_close(&mut self, tab: &mut Pane) -> bool {
        if self.app.dirty_prompt.is_some() { return false; }
        if let Pane::View(k) = tab {
            // The app closes it after drawing (remember spot, free the file).
            self.closed_view = Some(usize::from(*k));
            return false;
        }
        true
    }

    /// 3D views stay in the main window.
    fn allowed_in_windows(&self, tab: &mut Pane) -> bool {
        !matches!(tab, Pane::View(_) | Pane::Empty | Pane::LayoutTabs)
    }

    /// 3D view and Explorer paint their own background.
    fn clear_background(&self, tab: &Pane) -> bool {
        !matches!(tab, Pane::View(_) | Pane::Empty | Pane::Explorer)
    }

    /// 3D views have file tabs (the active view's is bright), the other panes a plain title.
    /// 3D view and Explorer fill their tab edge to edge (no margin).
    fn tab_style_override(&self, tab: &Pane, global: &egui_dock::TabStyle) -> Option<egui_dock::TabStyle> {
        let mut s = match tab {
            Pane::View(k) => dock::view_tab_style(global, usize::from(*k) == self.app.active),
            _ => dock::panel_tab_style(global),
        };
        if matches!(tab, Pane::View(_) | Pane::Empty | Pane::Explorer) {
            s.tab_body.inner_margin = egui::Margin::ZERO;
        }
        Some(s)
    }

    /// A view alone in its place keeps where its tab is (the view paints it as a name tag).
    /// Other views: blue line on top of the active view's tab (VS Code active editor tab).
    fn on_tab_button(&mut self, tab: &mut Pane, response: &egui::Response) {
        let Pane::View(k) = *tab else { return };
        let k = usize::from(k);
        if self.lone[k] {
            let label = self.view_title(k).unwrap_or_else(|| tab.title().to_owned());
            self.tags[k] = Some(dock::ViewTag { rect: response.rect, label, hovered: response.hovered(), dragged: response.dragged() });
        } else if k == self.app.active {
            let r = response.rect;
            let line = egui::Stroke::new(2.0_f32, theme::ACTIVE_VIEW);
            response.ctx.layer_painter(response.layer_id).hline(r.x_range(), r.top() + 1.0, line);
        }
    }

    fn scroll_bars(&self, tab: &Pane) -> [bool; 2] {
        match tab {
            Pane::Selected | Pane::Devices => [false, true],
            _ => [false, false],
        }
    }
}

/// Ends the graphs' work budget of a frame (`graph::end_frame`) however `frame` returns.
struct GraphFrame;

impl Drop for GraphFrame {
    fn drop(&mut self) {
        graph::end_frame();
    }
}

impl TakViewerApp {
    /// One UI frame. Called by the window backend (`eframe::App::update`) and by `--headless`.
    pub fn frame(&mut self, ctx: &egui::Context) {
        for (slot, view) in self.views.iter_mut().enumerate() {
            view.comment_marker_draft = self.comment_edit.as_ref().is_some_and(|edit|
                edit.field == Field::Markers && edit.slot == slot && edit.file_id == view.file_id);
        }
        let initial_focus = match self.dock.find_active_focused().map(|(_, pane)| *pane) {
            Some(Pane::View(slot)) => key_focus::Target::Markers(usize::from(slot)),
            Some(Pane::Graph) => key_focus::Target::Markers(self.active),
            Some(Pane::Comments) => key_focus::Target::Comments,
            _ => key_focus::Target::Blocked,
        };
        key_focus::begin_frame(ctx, self.capture.is_some() || self.dirty_prompt.is_some()
            || self.hold.is_some() || self.comment_edit.is_some() || self.layout_strip.is_renaming(), initial_focus);
        let now = ctx.input(|i| i.time);
        self.layout_time = now;
        if !self.no_persist && !self.exit_approved && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_exit(ctx);
        }
        // v0.27.0 speed fix: the graphs' building and thinning share a budget per frame
        // (`graph::spend`; the passes of one frame have the same input time); none outside this
        // frame (`GraphFrame`). A picture run (`--screenshot`, `--capture`, menu capture) has no
        // budget: no one watches its frames, and its pictures come in the same frames as before
        // (later frames would put the text's glyphs in the font atlas in another order, which
        // moves edge pixels of text by 1/255). `--bench` has the budget, as a window.
        let budget = (self.auto.is_none() && self.capture.is_none()).then_some(graph::FRAME_WORK);
        self.graph_put_off = graph::start_frame(now.to_bits(), budget);
        let _graph_frame = GraphFrame;
        self.watch_other_windows(ctx);

        // Drag & drop: folder -> workspace; files -> the view under the mouse (else the active
        // one), more files next to it.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() && self.dirty_prompt.is_none() {
            let at = ctx.input(|i| i.pointer.latest_pos());
            if let Some(k) = at.and_then(|p| self.open_views().into_iter().find(|&k| self.views[k].scene_rect.contains(p))) {
                self.activate(k);
            }
            let mut first = true;
            for p in dropped {
                if self.dirty_prompt.is_some() { break; }
                if p.is_dir() {
                    self.open_workspace(p);
                } else if std::mem::take(&mut first) {
                    self.open(ctx, p);
                } else {
                    self.open_side(ctx, p);
                }
            }
        }
        let global_keys = !self.typing(ctx) && self.dirty_prompt.is_none() && self.hold.is_none() && self.comment_edit.is_none()
            && self.capture.is_none()
            && !ctx.input(|input| input.key_down(Key::M)) && !ctx.memory(|m| m.any_popup_open());
        if global_keys && exact_key(ctx, Key::O, true, true, false) {
            self.pick_folder();
        }
        if global_keys && exact_key(ctx, Key::O, true, false, false) {
            self.pick_file(ctx, false);
        }
        if global_keys && exact_key(ctx, Key::L, true, false, false) {
            self.open_path_dialog();
        }
        let tab_keys = global_keys && self.capture.is_none();
        if tab_keys && exact_key(ctx, Key::T, true, true, false) { self.reopen_layout_item_with_context(ctx); }
        if tab_keys && exact_key(ctx, Key::T, true, false, false) { self.new_layout_tab(); }
        if tab_keys && (exact_key(ctx, Key::Tab, true, true, false) || exact_key(ctx, Key::PageUp, true, false, false)) {
            self.step_layout_tab(-1);
        }
        if tab_keys && (exact_key(ctx, Key::Tab, true, false, false) || exact_key(ctx, Key::PageDown, true, false, false)) {
            self.step_layout_tab(1);
        }
        if tab_keys && exact_key(ctx, Key::F2, false, false, false) {
            if let Some(id) = self.layout_tabs.active_id() { self.layout_strip.begin_rename(&self.layout_tabs, id); }
        }
        // Side toggles (VS Code keys). Ctrl+Alt+B first: Ctrl+B also matches it.
        for (key, alt, side) in [(Key::B, true, Side::Right), (Key::B, false, Side::Left), (Key::J, false, Side::Bottom)] {
            if global_keys && exact_key(ctx, key, true, false, alt) && self.capture.is_none() {
                self.toggle_side(side);
            }
        }
        if global_keys {
            if exact_key(ctx, Key::PageDown, false, false, false) {
                self.step_file(ctx, 1);
            }
            if exact_key(ctx, Key::PageUp, false, false, false) {
                self.step_file(ctx, -1);
            }
        }

        // Folder listings finished? Changes the folder watcher found on disk swap in here too;
        // then the watcher gets the folders shown expanded (only when they changed) and whether
        // the window is minimized (no re-reading then; egui `None` = not minimized).
        self.workspace.poll();
        if self.workspace.is_watching() {
            let minimized = ctx.input(|i| i.viewport().minimized).unwrap_or(false);
            self.workspace.set_minimized(minimized);
            explorer::keep_watch(&mut self.explorer, &mut self.workspace);
        }
        if self.workspace.is_loading() || self.ssh.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        if self.ssh.is_busy() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.ssh.cancel_all();
        }
        // Background loads: markers ready? Then force plates / EMG.
        self.poll_loads(ctx);
        for path in std::mem::take(&mut self.pending_shared_promotions) { self.promote_shared_waiter(ctx, &path); }
        self.poll_hidden_file_work(ctx);
        self.poll_marker_jobs(ctx);
        self.poll_marker_saves(ctx);
        self.poll_overlay_marker_work(ctx);
        self.poll_hidden_marker_work(ctx);
        for slot in self.open_views() { self.views[slot].sync_shared(); }
        self.poll_exit_wait(ctx);

        self.apply_ssh_demo(ctx);
        self.drive_auto_shot(ctx, now);
        self.drive_capture(ctx, now);
        // A cell edit of the COMMENTS pane first (its Enter / Esc), then `M` held (comment while
        // watching): its Esc and its key-up come before the rest.
        // The type strip closes on the next action elsewhere (it uses nothing up).
        self.type_strip_input(ctx);
        if self.capture.is_none() {
            self.comment_edit_keys(ctx);
        }
        self.handle_hold(ctx);
        if self.capture.is_none() {
            self.handle_keys(ctx, now);
        }
        if global_keys && exact_key(ctx, Key::S, true, true, false) {
            self.start_menu_capture();
        }
        // Every view plays on its own (no frame matching between views).
        for k in self.open_views() {
            let pb = &mut self.views[k].pb;
            if pb.tick(now) || pb.playing {
                ctx.request_repaint();
            }
        }
        self.close_hold_on_wrap(ctx);
        self.settings.speed = self.view().pb.speed;
        self.dirty_dialog(ctx);
        self.graph_in_one_place();

        self.menu_bar(ctx);
        self.status_bar(ctx);

        if self.capture.is_none() && !dock::has(&self.dock, Pane::LayoutTabs) {
            dock::ensure_layout_strip(&mut self.dock, self.active as u8);
        }
        dock::size_layout_strip(&mut self.dock);

        // Panes (Explorer, 3D views, Labels, graph, devices): tabs that can be dragged anywhere.
        // The dock is taken out while drawing so the tabs can use the rest of the app.
        let open = self.open_views();
        // Only views drawn this frame have a place on screen (a view behind another tab has none).
        for v in &mut self.views {
            v.scene_rect = egui::Rect::NOTHING;
            v.graph_rect = None;
            v.plate_rect = None;
        }
        let mut lone = [false; MAX_VIEWS];
        for k in dock::lone_views(&self.dock) {
            if let Some(l) = lone.get_mut(usize::from(k)) {
                *l = true;
            }
        }
        let order = dock::views_reading_order(&self.dock).into_iter().map(usize::from).collect();
        let mut dock_state = std::mem::replace(&mut self.dock, dock::scene_only(0));
        let style = dock::style(&ctx.style());
        let dock_enabled = self.dirty_prompt.is_none();
        let mut tabs = Tabs { app: self, action: None, ws_action: None, pressed_view: None, closed_view: None, open, lone, tags: Default::default(), order, ed_action: None };
        let area = egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                if !dock_enabled { ui.disable(); }
                DockArea::new(&mut dock_state)
                    .id(egui::Id::new("panes"))
                    .style(style)
                    .show_add_buttons(false)
                    .show_window_close_buttons(dock_enabled)
                    .show_inside(ui, &mut tabs);
            })
            .response
            .rect;
        let (action, ws_action) = (tabs.action.take(), tabs.ws_action.take());
        let (pressed_view, closed_view) = (tabs.pressed_view, tabs.closed_view);
        let ed_action = tabs.ed_action.take();
        self.dock = dock_state;
        self.dock_area = area;
        if let Some(rect) = dock::layout_strip_rect(&self.dock).filter(|_| self.capture.is_none()) {
            let actions = egui::Area::new(egui::Id::new("whole_layout_strip"))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.min)
                .show(ctx, |ui| {
                    ui.set_min_width(rect.width());
                    ui.set_max_width(rect.width());
                    key_focus::region(ui, rect, key_focus::Target::Blocked);
                    ui.add_enabled_ui(self.dirty_prompt.is_none(), |ui| self.layout_strip.show(ui, &self.layout_tabs)).inner
                }).inner;
            for action in actions { self.layout_strip_action(ctx, action); }
        }
        // A click on a view's tab or inside a view makes it the active one.
        let focused = self.dock.find_active_focused().map(|(_, t)| *t);
        if self.dirty_prompt.is_none() && focused != self.focused_tab {
            self.focused_tab = focused;
            if let Some(Pane::View(k)) = focused {
                self.activate(usize::from(k));
            }
        }
        if let Some(k) = pressed_view.filter(|_| self.dirty_prompt.is_none()) {
            if k != self.active {
                self.activate(k);
            }
        }
        if let Some(k) = closed_view {
            self.close_view(k);
        }
        if std::mem::take(&mut self.graph_to_panel) {
            self.set_graph_float(false);
        }
        self.after_comments_tab(now, ctx);
        // A type clicked on the strip: that type for its comment, and the strip closes.
        if let Some(t) = self.strip_pick.take() {
            if let Some(s) = self.type_strip.take().filter(|s| !s.shot) {
                if self.views[s.slot].file_id == s.file_id {
                    self.set_comment_type(ctx, s.slot, s.id, t);
                }
            }
        }
        let deck_action = self.deck_action.take().filter(|_| self.dirty_prompt.is_none());
        match deck_action {
            Some((slot, DeckAction::MakeComment)) => self.make_comment(ctx, slot),
            Some((slot, DeckAction::DeleteComment(id))) => self.delete_comment(ctx, slot, id),
            Some((_, DeckAction::GotoComment(_))) | None => {}
        }
        self.apply(action, now);
        if let Some((slot, uid)) = self.marker_click.take().filter(|_| self.dirty_prompt.is_none()) { self.label_sample(ctx, slot, uid); }
        if let Some(slot) = self.marker_delete.take().filter(|_| self.dirty_prompt.is_none()) { self.delete_selected(ctx, slot); }
        if let Some(a) = ws_action {
            self.explorer_action(ctx, a);
        }
        if let Some(a) = ed_action {
            self.editor_action(ctx, a);
        }
        // A name tag of a laid-over view clicked (plan step 7): hide / show, ✕ = take it off.
        // (The press on the tag chose its view above, `pressed_view`.)
        if let Some((slot, which, close)) = self.overlay_click.take() {
            self.name_tag_click(slot, which, close);
        }
        // Explorer file being dragged: its name follows the pointer.
        if let (Some(file), Some(p)) = (egui::DragAndDrop::payload::<explorer::FileDrag>(ctx), ctx.pointer_hover_pos()) {
            let name = file.0.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("file_drag")));
            let galley = painter.layout_no_wrap(name, egui::FontId::proportional(13.0), theme::TEXT);
            let r = egui::Rect::from_min_size(p + egui::vec2(14.0, 10.0), galley.size() + egui::vec2(16.0, 8.0));
            painter.rect_filled(r, 4.0, theme::PANEL_BG);
            painter.rect_stroke(r, 4.0, egui::Stroke::new(1.0_f32, theme::ACTIVE_VIEW));
            painter.galley(r.min + egui::vec2(8.0, 4.0), galley, theme::TEXT);
        }
        if let Some((slot, zone, path)) = self.file_drop.take() {
            egui::DragAndDrop::clear_payload(ctx);
            self.drop_file(ctx, slot, zone, path);
        }
        let label = self.view().data.as_ref().and_then(|_| self.view_label(self.active, &self.open_views()));
        let title = crate::version::window_title(label.as_deref());
        if title != self.title_shown {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title_shown = title;
        }

        if let Some(e) = self.error.clone() {
            let mut open = true;
            egui::Window::new("파일을 열 수 없어요").open(&mut open).collapsible(false).show(ctx, |ui| {
                ui.label(e);
            });
            if !open {
                self.error = None;
            }
        }
        if self.show_warnings {
            let mut open = true;
            egui::Window::new("읽기 경고").open(&mut open).default_width(520.0).show(ctx, |ui| {
                if let Some(d) = &self.views[self.active].data {
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        for w in &d.take.warnings {
                            ui.label(w);
                        }
                    });
                }
            });
            self.show_warnings = open;
        }
        if let Some(mut text) = self.path_input.take() {
            let mut open = true;
            let mut submit = false;
            let locked = self.dirty_prompt.is_some();
            let (focus, name) = (&mut self.path_focus, &mut self.path_name);
            egui::Window::new("경로로 열기").open(&mut open).collapsible(false).default_width(620.0).show(ctx, |ui| {
                if locked { ui.disable(); }
                ui.label("폴더나 .tak 파일 경로를 붙여 넣으세요. WSL 경로도 됩니다.");
                ui.label(
                    RichText::new("예: /home/user/workspace/tak-viewer   ·   /mnt/c/Users/User/…   ·   \\\\wsl.localhost\\Ubuntu\\…   ·   C:\\…")
                        .small()
                        .color(theme::TEXT_DIM),
                );
                let resp = ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
                if !locked && std::mem::take(focus) {
                    resp.request_focus();
                }
                let enter = |ui: &egui::Ui| ui.input(|i| i.key_pressed(Key::Enter));
                if resp.lost_focus() && enter(ui) {
                    submit = true;
                }
                if !text.trim().is_empty() {
                    let p = workspace::resolve_path(&text);
                    let (kind, color) = if p.is_dir() {
                        ("폴더 → 저장 폴더(Explorer)에 추가해요", theme::LABEL_TRACKED)
                    } else if p.is_file() {
                        ("파일 → 바로 열어요", theme::LABEL_TRACKED)
                    } else {
                        ("찾을 수 없어요", theme::WARN)
                    };
                    ui.label(RichText::new(format!("{}  ({kind})", p.display())).color(color));
                    if p.is_dir() {
                        // Optional display name for the saved folder (the folder keeps its name).
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("표시 이름 (선택)");
                            let r = ui.add(egui::TextEdit::singleline(name).hint_text(workspace::folder_name(&p)).desired_width(280.0));
                            if r.lost_focus() && enter(ui) {
                                submit = true;
                            }
                        });
                        ui.label(
                            RichText::new("비워 두면 폴더 이름 그대로예요. 여기 보이는 이름만 바뀌고 폴더는 그대로예요. 나중에 Explorer에서 연필 버튼으로 바꿀 수 있어요.")
                                .small()
                                .color(theme::TEXT_DIM),
                        );
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("열기").clicked() {
                        submit = true;
                    }
                });
            });
            if submit && !text.trim().is_empty() {
                let p = workspace::resolve_path(&text);
                if p.is_dir() {
                    let name = std::mem::take(&mut self.path_name);
                    if self.open_workspace(p.clone()) && !name.trim().is_empty() {
                        self.workspace.set_name(&p, &name);
                    }
                } else if p.exists() {
                    self.open_any(ctx, p);
                } else {
                    self.error = Some(format!("경로를 찾을 수 없어요:\n{}", p.display()));
                }
            } else if open {
                self.path_input = Some(text);
            }
        }
        if let Some((dir, lines)) = self.capture_done.clone() {
            let mut open = true;
            egui::Window::new("캡처를 저장했어요").open(&mut open).collapsible(false).default_width(560.0).show(ctx, |ui| {
                for l in &lines {
                    ui.label(l);
                }
                if let Some(dir) = &dir {
                    ui.horizontal(|ui| {
                        if ui.button("폴더 열기").clicked() {
                            let _ = std::process::Command::new("explorer").arg(dir).spawn();
                        }
                        if let Some(w) = workspace::to_wsl_path(dir) {
                            if ui.button("WSL 경로 복사").clicked() {
                                ctx.output_mut(|o| o.copied_text = w);
                            }
                        }
                    });
                }
            });
            if !open {
                self.capture_done = None;
            }
        }
        if let Some(msg) = self.notice.clone() {
            let mut open = true;
            egui::Window::new("알림").open(&mut open).collapsible(false).default_width(460.0).show(ctx, |ui| {
                ui.label(msg);
            });
            if !open {
                self.notice = None;
            }
        }
        if self.show_settings {
            let mut open = true;
            egui::Window::new("설정").open(&mut open).collapsible(false).resizable(false).default_width(420.0).show(ctx, |ui| {
                if self.dirty_prompt.is_some() { ui.disable(); }
                ui.label(RichText::new("3D 위에 뜬 그래프 (마커 · forceplate)").color(theme::TEXT_DIM));
                ui.horizontal(|ui| {
                    ui.label("바탕 진하기");
                    ui.add(egui::Slider::new(&mut self.settings.graph_opacity, 0..=100).suffix(" %"));
                });
                ui.label(RichText::new("0 % = 바탕 없음 (선만 보여요) · 100 % = 뒤가 안 비쳐요 · 숫자 칸에 직접 써도 돼요").small().color(theme::TEXT_DIM));
                ui.add_space(4.0);
                if ui.button(format!("처음 값으로 ({} %)", graph_float::DEFAULT_OPACITY)).clicked() {
                    self.settings.graph_opacity = graph_float::DEFAULT_OPACITY;
                }
            });
            self.show_settings = open;
        }
        if self.show_help {
            let mut open = true;
            egui::Window::new("조작키").open(&mut open).collapsible(false).show(ctx, |ui| {
                egui::Grid::new("help").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                    for (k, v) in [
                        ("왼쪽 클릭", "마커 선택 (Ctrl+클릭 = 여러 개)"),
                        ("왼쪽 드래그", "네모 안 마커 모두 선택 (Ctrl = 기존 선택에 더하기)"),
                        ("3D의 forceplate 클릭", "그 판의 그래프(처음은 Fx · Fy · Fz)를 3D 위에 띄우기 · 다시 누르면 닫기 (마커 선택은 그대로, 장치 패널의 \"3D 위로\"도 같아요)"),
                        ("오른쪽 드래그", "가운데 점을 중심으로 회전"),
                        ("가운데(휠) 드래그", "화면 이동"),
                        ("휠 굴리기", "확대 / 축소"),
                        ("패널 제목 끌기", "패널 옮기기 · 다른 패널과 탭으로 겹치기 · 떠 있는 창으로 떼기"),
                        ("Ctrl+B / Ctrl+J / Ctrl+Alt+B", "왼쪽 / 아래 / 오른쪽 패널 전체 켜고 끄기 (오른쪽 위 버튼 3개와 같음)"),
                        ("3D 위 그래프의 이름표 끌기", "그래프 옮기기 (3D 화면 안 어디든). 두 번 누르면 접기 / 펴기"),
                        ("3D 위 그래프의 가장자리 · 모서리 끌기", "그래프 크기 바꾸기. 마우스를 올리면 버튼: 마커 그래프 = 아래 Graph 패널로 · 접기, forceplate 그래프 = 줄 · 접기 · 닫기(✕)"),
                        ("forceplate 그래프의 \"줄\" 버튼", "보일 줄을 골라요: 힘 Fx · Fy · Fz, 모멘트 Mx · My · Mz (누를 때마다 바로 바뀌고, 하나는 꼭 남아요. 다른 곳을 누르거나 Esc = 목록 닫기)"),
                        ("3D 위 그래프 안 클릭 · 드래그 / 휠", "프레임 이동 / 가로 확대 (바탕 진하기는 보기 → 설정…)"),
                        ("더블클릭 / F", "선택 마커로 확대 (회전 중심)"),
                        ("Shift+F", "전체 보기"),
                        ("Z / Shift+Z", "선택 마커의 다음 / 이전 빈 구간으로"),
                        ("Space", "재생 / 정지"),
                        ("→ / ←", "다음 / 이전 1 프레임"),
                        ("↑ / ↓", "다음 / 이전 10 프레임"),
                        ("Home / End", "처음 / 끝"),
                        ("재생 막대 위 칸 누르기 · 끌기", "그 프레임으로 이동"),
                        ("재생 막대 아래 띠 끌기 / 누르기", "구간 잡기 (주황 점선, 프레임은 그대로) / 구간 풀기"),
                        ("[ / ]", "지금 프레임을 구간 시작 / 끝으로 (구간이 없으면 지금 프레임 한 장)"),
                        ("Esc", "Comment를 고치는 중이면 취소. 아니면 구간 풀기 (구간이 없으면 마커 선택과 고른 Comment 해제). M을 누르고 있는 중이면 그 Comment 취소"),
                        ("M 누르고 있기", "Comment: 누른 프레임 ~ 뗀 프레임 + 누를 때 고른 마커(없어도 돼요)를 바로 저장 (재생 · 이동하면서 누르고 있기, TAK 옆 <이름>.comments.csv)"),
                        ("M 짧게", "Comment: 구간(없으면 지금 프레임) + 고른 마커(없어도 돼요)를 바로 저장 (재생 막대의 Comment 버튼과 같아요)"),
                        ("유형 버튼 3개 / T", "Comment를 만든 바로 뒤 재생 막대 위에 Jitter · Marker swap · Needs visual check 버튼이 떠요: 누르면 그 유형이 붙어요 (다른 키 · 다른 곳 누르기 = 그냥 닫힘). T = 고른 Comment의 유형을 차례로 바꾸기: 없음 → Jitter → Marker swap → Needs visual check → 없음"),
                        ("재생 막대 띠의 주황 칸 누르기 / 오른쪽 클릭", "그 Comment의 첫 프레임으로 + 그 마커 선택 (마커 없는 Comment는 프레임만) / 바로 지우기"),
                        ("COMMENTS 탭", "보기 메뉴 · 상태 줄 Comments N으로 열기. 줄 누르기 = 그 Comment와 마커 고르기(프레임 · 재생은 그대로) · # 두 번 누르기 = 그 Comment 첫 프레임으로 · 칸 두 번 누르기 = 고치기(Markers: 3D · Labeling에서 마커를 고르고 Enter, Frames · Comment: 쓰고 Enter; 프레임은 그대로), Esc = 취소 · Type ▼ 상자 한 번 누르기 = 유형 목록(누르면 바로 저장, ↑ ↓ Enter, Esc · 다른 곳 = 닫기) · ✕ = 지우기 · 칸 이름 = 정렬 (한 번 더 = 거꾸로)"),
                        ("Ctrl+Z / Ctrl+Y", "한 번 = 동작 하나 되돌리기 / 다시 하기: 프레임 · 시점 · 선택 · 구간 · Comment · 설정 · 패널 · 파일 열기 · 폴더를 한 일의 차례대로 (앱 전체, 이어진 움직임은 하나, 최근 동작 500개까지, 글 칸에 커서가 있으면 그 글의 되돌리기)"),
                        ("Ctrl+O", "파일 열기 (창에 끌어다 놓아도 됨)"),
                        ("Ctrl+T / Ctrl+Shift+T", "빈 탭 만들기 / 마지막으로 닫은 화면 또는 탭 다시 열기"),
                        ("Ctrl+Tab / Ctrl+Shift+Tab", "다음 / 이전 탭"),
                        ("Ctrl+PageDown / Ctrl+PageUp", "다음 / 이전 탭"),
                        ("F2", "현재 탭 이름 바꾸기 (Enter 확정, Esc 취소)"),
                        ("Ctrl+W", "선택한 3D 화면 닫기 (마지막 화면이면 탭 닫기)"),
                        ("Explorer 클릭 / Ctrl+클릭", "지금 화면에 열기 / 옆 화면에 열기 (3D 화면 최대 4개, 2×2까지)"),
                        ("Explorer 파일 오른쪽 클릭 → 이 화면에 얹기", "그 파일의 마커를 고른 화면 위에 흰 고리로 겹쳐 보기 (같은 프레임, 프레임 수 · 속도가 같은 파일만). 얹은 파일 내리기 = 같은 메뉴 또는 이름표 2의 ✕. 서로 다른 파일은 전체에서 12개까지"),
                        ("1 / 2 (이름표 1 / 2 누르기)", "얹은 화면에서 처음 파일 / 얹은 파일의 마커 숨기기 · 다시 보이기"),
                        ("3D 화면 누르기 · 탭 ✕", "그 화면 고르기 (탭이 밝아지고 파란 테두리, 패널·재생 막대·키가 그 화면 기준) · 그 화면 닫기"),
                        ("Ctrl+Shift+O", "폴더 추가 → Explorer 트리 (최대 10개, 펼칠 때만 그 폴더를 읽음)"),
                        ("저장 폴더 연필 / 오른쪽 클릭", "이름 바꾸기 (보이는 이름만, 폴더는 그대로). Enter = 저장, Esc = 취소"),
                        ("Ctrl+L", "경로로 열기 (WSL 경로 /home/…, /mnt/c/… 가능)"),
                        ("PageDown / PageUp", "같은 폴더의 다음 / 이전 파일"),
                        ("Ctrl+Shift+S", "화면 캡처 저장 (지금 각도 + 위·옆·앞, repo 안)"),
                    ] {
                        ui.label(RichText::new(k).strong());
                        ui.label(v);
                        ui.end_row();
                    }
                });
            });
            self.show_help = open;
        }
        // Current click geometry and higher-priority widgets settle before destructive keys.
        key_focus::finish_frame(ctx);
        self.comment_list_keys(ctx);
        self.editor_list_keys(ctx);
        self.marker_focus_keys(ctx);
        // Last: what a person changed this frame becomes an undo action (`app_undo.rs`).
        self.graph_in_one_place();
        self.observe_frame(ctx);
    }

}

/// `--bench` (`headless.rs`): load state and timing of each view, read-only.
pub struct BenchState {
    /// Markers of every file are on screen.
    pub loaded: bool,
    /// Force plates / EMG still coming in some view.
    pub analog_pending: bool,
    /// A file to lay over a view (plan step 7) still coming.
    pub overlay_pending: bool,
    pub error: Option<String>,
    pub views: Vec<BenchView>,
}

pub struct BenchView {
    pub file: String,
    /// ms from the open to markers on screen / force plates attached.
    pub markers_ms: Option<f64>,
    pub analog_ms: Option<f64>,
    pub lazy_analog: bool,
    /// A requested lazy read can finish with an error even after pending becomes false.
    pub emg_failure: Option<String>,
    pub fallback: Option<String>,
    /// Step times of the open in ms (container, channels, markers, view, analog).
    pub steps: Vec<(&'static str, f64)>,
    /// The file laid over this view and ms from asking until it was on screen (plan step 7).
    pub overlay: Option<(String, Option<f64>)>,
}

impl TakViewerApp {
    pub fn bench_state(&self) -> BenchState {
        let open = self.open_views();
        let views = open
            .iter()
            .map(|&k| {
                let v = &self.views[k];
                BenchView {
                    file: v.target().map(|p| p.display().to_string()).unwrap_or_default(),
                    markers_ms: v.markers_ms,
                    analog_ms: v.analog_ms,
                    lazy_analog: v.data.as_ref().is_some_and(|d| d.emg_pending),
                    emg_failure: v.emg_failure.clone(),
                    fallback: v.analog_fallback.clone(),
                    steps: v.load_steps.clone(),
                    overlay: v.overlay_target().map(|p| (p.display().to_string(), v.overlay_ms)),
                }
            })
            .collect();
        BenchState {
            loaded: open.iter().all(|&k| self.views[k].data.is_some() && self.views[k].loading.is_none()),
            analog_pending: open.iter().any(|&k| self.views[k].analog_rx.is_some() || self.views[k].data.as_ref().is_some_and(|d| d.analog_pending)),
            overlay_pending: open.iter().any(|&k| self.views[k].overlay_load.is_some()),
            error: self.error.clone(),
            views,
        }
    }

    /// Open files the way the Explorer does: the first with a click, the others with
    /// Ctrl+click (next to it).
    pub fn bench_open(&mut self, ctx: &egui::Context, files: &[PathBuf]) {
        for (k, f) in files.iter().enumerate() {
            if k == 0 {
                self.open(ctx, f.clone());
            } else {
                self.open_side(ctx, f.clone());
            }
        }
    }

    /// Start playing every view from its first frame (it loops), with its first marker selected
    /// (trail, name tag and the marker graph floating over each view are drawn too) and, when
    /// the file has force plates, its first plate chosen (plate graph floating too), for the
    /// per-frame time. Returns how many views have a plate chosen.
    pub fn bench_play(&mut self, now: f64) -> usize {
        let mut plates = 0;
        for k in self.open_views() {
            let v = &mut self.views[k];
            if v.data.as_ref().is_some_and(|d| !d.markers.is_empty()) {
                v.select(0, false);
            }
            // `--bench-select N` (v0.27.0): the first N markers of the LABELS list instead.
            let n = crate::headless::BENCH_SELECT.load(std::sync::atomic::Ordering::Relaxed);
            if n > 1 {
                let first: Vec<usize> = v.data.as_ref().map_or_else(Vec::new, |d| d.groups.iter().flat_map(|g| g.members.iter().copied()).take(n).collect());
                v.select_many(first, None, false);
                v.primary = v.selected.iter().next().copied();
            }
            if v.data.as_ref().is_some_and(|d| !d.plates.is_empty()) {
                v.plate = Some(0);
                plates += 1;
            }
            let pb = &mut v.pb;
            pb.set_frame(pb.first, now);
            pb.play(now);
        }
        plates
    }

    /// Open the existing Devices tab/header and trigger the ordinary lazy path while timing.
    pub fn bench_emg(&mut self, ctx: &egui::Context) -> usize {
        dock::show(&mut self.dock, Pane::Devices);
        let mut count = 0;
        for slot in self.open_views() {
            let view = &mut self.views[slot];
            view.devices.open_device = view.data.as_ref().and_then(|d| d.devices.iter().position(|device| device.plate.is_none()));
            if view.devices.open_device.is_some() {
                view.devices.request_emg = true;
                view.request_emg(ctx);
                count += 1;
            }
        }
        count
    }

    pub fn bench_emg_pending(&self) -> bool {
        self.open_views().into_iter().any(|slot| {
            let view = &self.views[slot];
            (view.emg_requested && view.data.as_ref().is_some_and(|d| d.emg_pending))
                || view.analog_work_pending() || view.devices.graphs_pending()
        })
    }

    /// `--bench-hover` (v0.29.0): before playing frame `i`, every view's marker graph hovered as if
    /// the mouse were on the curve of its graphed marker `(i / 2) % (n + 1)` (`n` = off every
    /// curve): another curve, or none, every second frame.
    pub fn bench_hover(&mut self, i: usize) {
        for k in self.open_views() {
            let v = &mut self.views[k];
            let n = match (v.data.as_ref(), v.primary) {
                (Some(d), Some(p)) => panels::graphed(d, &v.selected, p).0.len(),
                _ => 0,
            };
            let at = (i / 2) % (n + 1);
            v.lanes.hover_at = (at < n).then_some(at);
        }
    }
}

impl eframe::App for TakViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }

    /// Several windows share one settings file. Called when the window closes and, while it
    /// is the only open window, every `AUTO_SAVE`. Every call writes the file (`SAVED_KEY`),
    /// so the window that closes last leaves its settings.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if !self.no_persist {
            self.sync_settings();
            eframe::set_value(storage, eframe::APP_KEY, &self.settings);
            self.saves += 1;
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            storage.set_string(SAVED_KEY, format!("{}-{}-{nanos}", std::process::id(), self.saves));
        }
    }

    /// No periodic save while another window is open: windows would overwrite each other
    /// every 30 s. Command line runs never save.
    fn auto_save_interval(&self) -> Duration {
        if self.no_persist || self.others_open {
            Duration::MAX
        } else {
            AUTO_SAVE
        }
    }

    /// Command line runs leave the settings file untouched (eframe would otherwise write its
    /// own UI memory, together with the settings read at start, back into the shared file).
    fn persist_egui_memory(&self) -> bool {
        !self.no_persist
    }
}

impl TakViewerApp {
    /// Copy saved folders, their open state and the pane layout into the settings.
    fn sync_settings(&mut self) {
        let s = |p: &PathBuf| p.to_string_lossy().into_owned();
        self.settings.workspace_roots = self.workspace.roots().iter().map(s).collect();
        self.settings.workspace_open = self.explorer.open_roots(&self.workspace).iter().map(s).collect();
        self.settings.workspace_names = self.workspace.names().map(|(p, n)| (s(p), n.clone())).collect();
        self.settings.layout = Some(self.dock.clone());
        self.settings.layout_version = dock::LAYOUT_VERSION;
        self.settings.hidden = self.hidden.clone();
        // Views and where each file is being looked at; the active view is remembered last so
        // it is the newest entry.
        let open = self.open_views();
        self.settings.views = open
            .iter()
            .filter(|&&k| self.views[k].remote.is_none())
            .filter_map(|&k| self.views[k].path().map(|p| (k as u8, p.to_string_lossy().into_owned())))
            .collect();
        self.settings.ssh_python = self.ssh.python_settings.iter().map(|(k,v)| (k.clone(),v.clone())).collect();
        self.settings.ssh_platforms = self.ssh.platform_overrides.iter().map(|(k,v)| (k.clone(),v.clone())).collect();
        self.settings.active_view = self.active as u8;
        let active = self.active;
        for k in open.into_iter().filter(|&k| k != active).chain([active]) {
            self.remember(k);
        }
        self.sync_layout_settings();
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::{App, Storage};

    include!("app_key_focus_tests.rs");
    include!("app_merge_tests.rs");
    include!("app_graph_selection_tests.rs");
    include!("app_empty_ui_tests.rs");
    include!("app_close_tests.rs");
    include!("app_layout_tabs_tests.rs");
    include!("app_freshness_tests.rs");
    include!("app_hidden_work_tests.rs");

    /// eframe's settings file in memory. Like eframe's `FileStorage`, a value marks the file
    /// changed only when it differs, and `flush` writes (counted here) only then.
    #[derive(Default)]
    struct FileLike {
        kv: BTreeMap<String, String>,
        dirty: bool,
        writes: usize,
    }

    impl Storage for FileLike {
        fn get_string(&self, key: &str) -> Option<String> {
            self.kv.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            if self.kv.get(key) != Some(&value) {
                self.kv.insert(key.to_owned(), value);
                self.dirty = true;
            }
        }
        fn flush(&mut self) {
            if self.dirty {
                self.writes += 1;
                self.dirty = false;
            }
        }
    }

    fn window_app() -> TakViewerApp {
        TakViewerApp::build(&egui::Context::default(), None, vec![], None, None)
    }

    #[test]
    fn every_window_save_writes_the_file_even_without_changes() {
        // The last window to close must overwrite what another window saved before.
        let mut app = window_app();
        let mut st = FileLike::default();
        for _ in 0..3 {
            app.save(&mut st);
            st.flush();
        }
        assert_eq!(st.writes, 3);
        assert!(st.kv.contains_key(eframe::APP_KEY));
    }

    #[test]
    fn command_line_runs_leave_the_settings_file_alone() {
        let shot = AutoShot { out: PathBuf::from("x.png"), frame: None, select: None, plate: None, range: None, menu_capture: false, edit_tools: false, edit_pattern: false, edit_layout: 0, comments: false, pick_comment: None, edit_comment: None, type_strip: None, overlays: vec![], hide: vec![], hover: None, stage: 0 };
        let mut app = TakViewerApp::build(&egui::Context::default(), None, vec![], Some(shot), None);
        let mut st = FileLike::default();
        app.save(&mut st);
        st.flush();
        assert_eq!(st.writes, 0);
        assert!(st.kv.is_empty());
        assert!(!app.persist_egui_memory());
        assert_eq!(app.auto_save_interval(), Duration::MAX);
    }

    #[test]
    fn periodic_save_only_while_no_other_window_is_open() {
        let mut app = window_app();
        assert!(app.persist_egui_memory());
        assert_eq!(app.auto_save_interval(), AUTO_SAVE);
        app.set_registration(None, true);
        assert_eq!(app.auto_save_interval(), Duration::MAX);
        assert!(app.shift_window, "a window opened next to another moves aside once");
        app.set_registration(None, false);
        assert_eq!(app.auto_save_interval(), AUTO_SAVE);
    }

    #[test]
    fn old_settings_move_the_one_folder_into_saved_folders() {
        let old = r#"(pane_labels: true, pane_selected: false, pane_devices: true, pane_workspace: true, workspace_right: true,
            recent: [], workspace: Some("C:\\data\\gait"), recent_workspaces: ["C:\\data\\gait"], speed: 1.0, looping: true)"#;
        let mut s: Settings = ron::from_str(old).expect("old settings still load");
        s.migrate();
        assert_eq!(s.workspace_roots, vec![r"C:\data\gait".to_owned()]);
        assert_eq!(s.workspace_open, s.workspace_roots);
        assert!(s.workspace.is_none());
        assert!(s.layout.is_none(), "old settings get the default layout");
        let again = ron::to_string(&s).unwrap();
        assert!(!again.contains("workspace:"), "old field is not written back: {again}");
    }

    #[test]
    fn migrate_keeps_existing_saved_folders() {
        let mut s = Settings { workspace: Some("old".into()), workspace_roots: vec!["a".into(), "b".into()], ..Settings::default() };
        s.migrate();
        assert_eq!(s.workspace_roots, vec!["a".to_owned(), "b".to_owned()]);
        assert!(s.workspace.is_none());
    }

    #[test]
    fn settings_from_before_several_views_still_load() {
        let mut s = Settings { workspace_roots: vec![r"C:\data".into()], ..Settings::default() };
        s.layout = Some(dock::default_layout());
        let text = ron::to_string(&s).unwrap();
        let new_fields = ["views:[],", "active_view:0,", "file_memory:[],"];
        assert!(new_fields.iter().all(|f| text.contains(f)), "{text}");
        let old = new_fields.iter().fold(text, |t, f| t.replace(f, "")).replace("View(0)", "Scene");
        let back: Settings = ron::from_str(&old).expect("v0.5.0 settings still load");
        assert_eq!(back.workspace_roots, s.workspace_roots, "saved folders kept");
        assert_eq!(dock::views(&dock::sanitize(back.layout.unwrap())), vec![0]);
        assert!(back.views.is_empty() && back.file_memory.is_empty());
    }

    #[test]
    fn settings_from_before_the_toggles_keep_their_arrangement_and_views() {
        // v0.6.0 text: no layout_version / hidden, no Graph pane, three 3D views.
        let mut layout = dock::default_layout();
        dock::hide(&mut layout, Pane::Graph);
        dock::add_view(&mut layout, 1, dock::NOMINAL_AREA);
        dock::add_view(&mut layout, 3, dock::NOMINAL_AREA);
        let s = Settings { workspace_roots: vec![r"C:\data".into()], layout: Some(layout), ..Settings::default() };
        let text = ron::to_string(&s).unwrap();
        let new_fields = ["layout_version:0,", "hidden:(left:None,bottom:None,right:None),"];
        assert!(new_fields.iter().all(|f| text.contains(f)), "{text}");
        let old = new_fields.iter().fold(text, |t, f| t.replace(f, ""));
        assert!(!old.contains("Graph"));
        let mut back: Settings = ron::from_str(&old).expect("v0.6.0 settings still load");
        assert_eq!(back.workspace_roots, s.workspace_roots, "saved folders kept");
        let (dock, hidden) = back.take_layout();
        assert_eq!(dock::views(&dock), vec![0, 1, 3], "the 3D views stay");
        assert!(!dock::has(&dock, Pane::Graph) && dock::has(&dock, Pane::Devices), "new first arrangement: no graph pane, the graph floats");
        assert!(back.graph_float);
        assert_eq!(hidden, dock::Hidden::default());
        assert_eq!(back.layout_version, dock::LAYOUT_VERSION);
    }

    #[test]
    fn settings_from_before_the_floating_graph_keep_hidden_panes_and_views() {
        // v0.9.0 text: the graph pane below two 3D views, hidden by the bottom toggle, and
        // none of the floating-graph settings.
        let mut layout = dock::default_layout_for(&[0, 2]);
        dock::show(&mut layout, Pane::Graph);
        let mut hidden = dock::Hidden::default();
        dock::toggle_side(&mut layout, &mut hidden, dock::NOMINAL_AREA, Side::Bottom);
        let saved_hidden = hidden.clone();
        let s = Settings { workspace_roots: vec![r"C:\data".into()], layout: Some(layout), layout_version: 1, hidden, ..Settings::default() };
        let text = ron::to_string(&s).unwrap();
        let new_fields = ["graph_float:true,", "graph_opacity:60,"];
        assert!(new_fields.iter().all(|f| text.contains(f)) && text.contains("layout_version:1,"), "{text}");
        let at = text.find("graph_boxes:").expect("graph_boxes is saved");
        let old = new_fields.iter().fold(text[..at].to_owned(), |t, f| t.replace(f, ""));
        let old = format!("{})", old.trim_end_matches(','));
        assert!(!old.contains("graph_") && old.contains("Graph"), "{old}");
        let mut back: Settings = ron::from_str(&old).expect("v0.9.0 settings still load");
        back.migrate();
        assert_eq!(back.workspace_roots, s.workspace_roots, "saved folders kept");
        assert_eq!((back.graph_float, back.graph_opacity), (true, graph_float::DEFAULT_OPACITY));
        assert_eq!(back.graph_boxes, vec![GraphBox::default(); MAX_VIEWS]);
        let (dock, hidden) = back.take_layout();
        assert_eq!(dock::views(&dock), vec![0, 2], "the 3D views stay");
        assert!(!dock::has(&dock, Pane::Graph) && dock::has(&dock, Pane::Devices) && back.graph_float);
        assert_eq!(hidden, saved_hidden, "the hidden bottom panel keeps its saved placement");
        // From then on the layout is used as saved: a graph pane brought back stays a pane.
        let mut dock = dock;
        dock::show(&mut dock, Pane::Graph);
        let mut again = Settings { layout: Some(dock), layout_version: dock::LAYOUT_VERSION, ..back.clone() };
        let (dock, _) = again.take_layout();
        assert!(dock::has(&dock, Pane::Graph) && !again.graph_float, "graph in its pane, not floating");
    }

    #[test]
    fn graph_floats_over_its_view_takes_the_mouse_and_goes_back_to_the_panel() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_data("a.tak", &["M"]), rx, 10, 1.0, 0.0);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].graph_rect, None, "no marker selected: no graph");
        let whole = app.views[0].scene_rect;
        assert!(whole.bottom() > 850.0, "nothing below the 3D but its playback bar: {whole:?}");
        app.views[0].select(0, false);
        frame_with(&ctx, &mut app, vec![]);
        let g = app.views[0].graph_rect.expect("a selected marker's graph floats over the 3D");
        assert!(app.views[0].scene_rect == whole && whole.contains_rect(g), "the 3D keeps its size, the graph is inside it: {g:?} in {whole:?}");
        assert!((g.bottom() - (whole.bottom() - 6.0)).abs() < 0.5 && (g.width() - (whole.width() - 12.0)).abs() < 0.5, "first place: along the bottom, as wide as the view");
        // A click inside the graph goes to that frame; the 3D under it does not get the click
        // (a click on empty 3D would clear the selection).
        click_at(&ctx, &mut app, egui::pos2(g.right() - 12.0, g.center().y - 10.0));
        assert_eq!(app.views[0].pb.frame, 9, "click near the right end of the graph: last frame");
        assert_eq!(app.views[0].primary, Some(0), "the selection is kept");
        // Folded: only the name tag is left, the graph itself is gone.
        app.settings.graph_boxes[0].collapsed = true;
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].graph_rect, None);
        app.settings.graph_boxes[0].collapsed = false;
        // Back to the GRAPH pane: it opens below the view, the 3D gets lower, nothing floats.
        app.set_graph_float(false);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(dock::has(&app.dock, Pane::Graph) && app.views[0].graph_rect.is_none());
        assert!(app.views[0].scene_rect.bottom() < whole.bottom() - 100.0, "{:?}", app.views[0].scene_rect);
        // "3D 위로" in the pane: the pane closes and the graph floats where it was.
        app.apply(Some(Action::GraphOver3d), 0.0);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(!dock::has(&app.dock, Pane::Graph) && app.settings.graph_float);
        assert_eq!(app.views[0].graph_rect, Some(g));
        // The graph pane opened some other way (menu, Ctrl+J): the graph is there, not floating.
        app.show_pane(Pane::Graph);
        frame_with(&ctx, &mut app, vec![]);
        assert!(!app.settings.graph_float && app.views[0].graph_rect.is_none());
    }

    #[test]
    fn floating_graph_resizes_by_its_corner_moves_by_its_tag_and_its_buttons_fold_it_and_send_it_to_the_pane() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_data("a.tak", &["M"]), rx, 10, 1.0, 0.0);
        app.views[0].select(0, false);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        // The graph lives in the whole 3D as drawn (the view is alone: no tab row).
        let area = app.views[0].scene_rect;
        let view_tag = view_tab(&ctx, &app, 0);
        let name_w = marker_name_w(&ctx);
        let points = |app: &TakViewerApp| graph_float::test_points(Kind::Marker, &app.settings.graph_boxes[0], area, view_tag, name_w);
        let near = |a: egui::Rect, b: egui::Rect| (a.min - b.min).length() < 1.0 && (a.max - b.max).length() < 1.0;
        let cam = (app.views[0].cam.yaw, app.views[0].cam.pitch);
        let g = app.views[0].graph_rect.expect("the graph floats");

        // Lower-right corner: 400 narrower, 20 lower in height; the top-left corner stays.
        drag(&ctx, &mut app, g.max - egui::vec2(3.0, 3.0), g.max - egui::vec2(403.0, 23.0));
        let small = app.views[0].graph_rect.unwrap();
        assert!(near(small, egui::Rect::from_min_max(g.min, g.max - egui::vec2(400.0, 20.0))), "{small:?} from {g:?}");

        // The name tag drags the graph: it follows the mouse exactly.
        let tag = points(&app)[0];
        drag(&ctx, &mut app, tag, tag + egui::vec2(150.0, -200.0));
        let moved = app.views[0].graph_rect.unwrap();
        assert!(near(moved, small.translate(egui::vec2(150.0, -200.0))), "{moved:?} from {small:?}");
        assert!(area.contains_rect(moved));
        assert_eq!(app.views[0].primary, Some(0), "the 3D under the graph got no click or box selection");
        assert_eq!((app.views[0].cam.yaw, app.views[0].cam.pitch), cam);

        // ▼ folds it to the tag, which stays where it is; ▲ unfolds it there again.
        let fold = points(&app)[2];
        click_at(&ctx, &mut app, fold);
        assert!(app.settings.graph_boxes[0].collapsed && app.views[0].graph_rect.is_none());
        assert!((points(&app)[0] - (tag + egui::vec2(150.0, -200.0))).length() < 1.0, "the folded tag is where the tag was");
        // Folded, the tag is dragged the same way, also to where the open graph would not fit.
        let folded = points(&app)[0];
        drag(&ctx, &mut app, folded, area.max - egui::vec2(120.0, 30.0));
        assert!((points(&app)[0] - (area.max - egui::vec2(120.0, 30.0))).length() < 1.0);
        let unfold = points(&app)[2];
        click_at(&ctx, &mut app, unfold);
        let reopened = app.views[0].graph_rect.expect("unfolded");
        assert!(reopened.size() == moved.size() && area.contains_rect(reopened), "{reopened:?}");

        // The other button sends the graph to the GRAPH pane below the view.
        let to_pane = points(&app)[1];
        click_at(&ctx, &mut app, to_pane);
        frame_with(&ctx, &mut app, vec![]);
        assert!(dock::has(&app.dock, Pane::Graph) && !app.settings.graph_float && app.views[0].graph_rect.is_none());
        // Place, size and folded state are saved with the settings.
        app.sync_settings();
        let back: Settings = ron::from_str(&ron::to_string(&app.settings).unwrap()).unwrap();
        assert_eq!(back.graph_boxes, app.settings.graph_boxes);
        assert!(back.graph_boxes[0].pos.is_some() && back.graph_boxes[0].size.is_some());
    }

    /// Width of the floating graph's name for the sample marker "M".
    fn marker_name_w(ctx: &egui::Context) -> f32 {
        ctx.fonts(|f| f.layout_no_wrap("M".to_owned(), egui::FontId::proportional(12.0), egui::Color32::WHITE)).size().x
    }

    /// egui_dock's tab of view `k` as egui saw it in the last frame (under the view's name tag
    /// when the view is alone in its place).
    fn view_tab_response(ctx: &egui::Context, app: &TakViewerApp, k: u8) -> egui::Response {
        let (surface, node, tab) = app.dock.find_tab(&Pane::View(k)).expect("the view is in the layout");
        // egui_dock 0.14's id of a tab (`show/leaf.rs`, `tabs`) inside `DockArea::id("panes")`.
        let id = egui::Id::new("panes").with((surface, "surface")).with((node, "node")).with((tab.0, "tab"));
        ctx.read_response(id).expect("the tab was drawn")
    }

    /// Where view `k`'s tab is, as egui hit-tests it.
    fn view_tab(ctx: &egui::Context, app: &TakViewerApp, k: u8) -> egui::Rect {
        let r = view_tab_response(ctx, app, k).rect;
        let place = place_of(app, Pane::View(k));
        assert!(place.contains(r.center()) && (r.height() - dock::TAB_H).abs() < 0.5, "the tab is within its view: {r:?} in {place:?}");
        r
    }

    /// One view with file a.tak (markers "M", "N"), marker 0 selected (its graph floats), drawn
    /// twice. Returns the view's tab (under its name tag).
    fn one_view_with_graph(ctx: &egui::Context) -> (TakViewerApp, egui::Rect) {
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_data("a.tak", &["M", "N"]), rx, 10, 1.0, 0.0);
        app.views[0].select(0, false);
        frame_with(ctx, &mut app, vec![]);
        frame_with(ctx, &mut app, vec![]);
        let tab = view_tab(ctx, &app, 0);
        (app, tab)
    }

    #[test]
    fn the_graph_goes_up_into_the_tab_row_of_a_view_alone_and_works_there() {
        let ctx = egui::Context::default();
        let (mut app, tab) = one_view_with_graph(&ctx);
        let whole = app.views[0].scene_rect;
        let row_bottom = whole.top() + dock::TAB_H;
        assert!((tab.top() - whole.top()).abs() < 0.5, "the 3D is drawn up over the tab row");
        let name_w = marker_name_w(&ctx);
        let points = |app: &TakViewerApp| graph_float::test_points(Kind::Marker, &app.settings.graph_boxes[0], whole, tab, name_w);
        // Smaller, then dragged by its tag to the top, right of the view's name tag.
        app.settings.graph_boxes[0].size = Some([400.0, 120.0]);
        frame_with(&ctx, &mut app, vec![]);
        let before = app.views[0].graph_rect.expect("the graph floats");
        let grip = points(&app)[0];
        let to = egui::pos2(tab.right() + 300.0, 1.0);
        drag(&ctx, &mut app, grip, to);
        let g = app.views[0].graph_rect.unwrap();
        assert!((g.top() - (whole.top() + 6.0)).abs() < 0.5, "as high as the 3D goes: in the tab row: {g:?} in {whole:?}");
        assert!((g.left() - (before.left() + to.x - grip.x)).abs() < 0.5 && g.left() > tab.right(), "beside the name tag, where it was let go: {g:?}");
        assert_eq!(g.size(), before.size());
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].graph_rect, Some(g), "it stays there");
        assert!(points(&app)[0].y < row_bottom, "its own tag is in the row too");

        // Inside the graph, in the tab row: a click goes to that frame (the 3D under it and the
        // dock do not take it: a click on empty 3D would clear the selection).
        let at = egui::pos2(g.right() - 12.0, g.top() + 14.0);
        assert!(at.y < row_bottom);
        click_at(&ctx, &mut app, at);
        assert_eq!(app.views[0].pb.frame, 9, "click near the right end of the graph: last frame");
        assert_eq!(app.views[0].primary, Some(0));
        // Wheel there zooms the graph, not the 3D.
        let dist = app.views[0].cam.distance;
        let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, 50.0), modifiers: Modifiers::NONE };
        frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(at), wheel]);
        assert_eq!(app.views[0].cam.distance, dist, "the 3D under the graph does not zoom");
        // Its top edge in the row resizes it (the row is inside its area): drag it down 40.
        let edge = egui::pos2(g.center().x, g.top());
        drag(&ctx, &mut app, edge, edge + egui::vec2(0.0, 40.0));
        let r = app.views[0].graph_rect.unwrap();
        assert!((r.top() - (g.top() + 40.0)).abs() < 0.5 && (r.bottom() - g.bottom()).abs() < 0.5, "{r:?} from {g:?}");
        // Folded in the row: its tag stays there.
        app.settings.graph_boxes[0].collapsed = true;
        frame_with(&ctx, &mut app, vec![]);
        let folded = points(&app)[0];
        drag(&ctx, &mut app, folded, egui::pos2(whole.right() - 60.0, whole.top()));
        assert!(points(&app)[0].y < row_bottom, "the folded tag goes into the row too: {:?}", points(&app)[0]);

        // The default (wide) graph pushed all the way up goes under the name tag; its own tag
        // stays clear of the view's name tag, just below it, and still moves the graph.
        app.settings.graph_boxes[0] = graph_float::GraphBox::default();
        frame_with(&ctx, &mut app, vec![]);
        let wide = app.views[0].graph_rect.unwrap();
        let grip = points(&app)[0];
        drag(&ctx, &mut app, grip, egui::pos2(grip.x, 1.0));
        let up = app.views[0].graph_rect.unwrap();
        assert!((up.top() - (whole.top() + 6.0)).abs() < 0.5 && up.size() == wide.size(), "{up:?}");
        assert!(up.intersects(tab), "under the name tag");
        let own = points(&app)[0];
        assert!(!tab.expand(2.0).contains(own) && own.y > tab.bottom() && own.y < tab.bottom() + 20.0, "its tag right below the name tag: {own:?} vs {tab:?}");
        drag(&ctx, &mut app, own, own + egui::vec2(0.0, 100.0));
        assert!((app.views[0].graph_rect.unwrap().top() - (up.top() + 100.0)).abs() < 0.5, "the moved tag still drags the graph");
    }

    #[test]
    fn the_3d_takes_the_mouse_in_the_tab_row_right_of_the_name_tag() {
        let ctx = egui::Context::default();
        let (mut app, tab) = one_view_with_graph(&ctx);
        let whole = app.views[0].scene_rect;
        let row = egui::pos2(tab.right() + 200.0, whole.top() + dock::TAB_H / 2.0);
        assert!(app.views[0].graph_rect.is_some_and(|g| !g.contains(row)), "the graph is along the bottom");
        // Left click on empty 3D in the row clears the selection (as below the row).
        click_at(&ctx, &mut app, row);
        assert_eq!(app.views[0].primary, None);
        // A left drag from the row down over every marker: box selection.
        let shown = {
            let v = &app.views[0];
            let d = v.data.as_ref().unwrap();
            scene::project_markers(&v.cam.view(v.scene_rect), d, v.pb.frame, &app.settings.scene, &v.selected)
        };
        assert_eq!(shown.len(), 2, "both sample markers are on screen");
        let (lo, hi) = shown.iter().fold((whole.max, whole.min), |(lo, hi), p| (lo.min(p.pos), hi.max(p.pos)));
        let start = egui::pos2((hi.x + 10.0).max(tab.right() + 10.0), row.y);
        assert!(start.x < whole.right() && lo.y > whole.top() + dock::TAB_H, "markers below the row: {lo:?} {hi:?}");
        drag(&ctx, &mut app, start, egui::pos2(lo.x - 10.0, hi.y + 10.0));
        assert_eq!(app.views[0].selected.len(), 2, "box selection started in the row");
        // On the name tag the 3D does not take the mouse: a click there (egui_dock's tab) keeps
        // the selection (a click on empty 3D would clear it).
        click_at(&ctx, &mut app, tab.center());
        assert_eq!(app.views[0].selected.len(), 2);
        // Right drag = orbit.
        let cam = (app.views[0].cam.yaw, app.views[0].cam.pitch);
        drag_with(&ctx, &mut app, row, row + egui::vec2(120.0, 60.0), egui::PointerButton::Secondary);
        assert!(app.views[0].cam.yaw != cam.0 && app.views[0].cam.pitch != cam.1, "the 3D turned");
        // Wheel = zoom.
        let dist = app.views[0].cam.distance;
        let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, 50.0), modifiers: Modifiers::NONE };
        frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(row), wheel]);
        assert!(app.views[0].cam.distance < dist, "the 3D zoomed in");
    }

    #[test]
    fn the_name_tag_keeps_its_drag_close_and_click_with_a_graph_parked_under_it() {
        let ctx = egui::Context::default();
        let mut app = two_views(&ctx);
        app.views[1].select(0, false);
        app.views[1].pb.set_frame(5, 0.0);
        app.activate(0);
        // View 1's graph (as wide as the view) parked at the very top: under its name tag.
        app.settings.graph_boxes[1].pos = Some([0.0, 0.0]);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        let tab = view_tab(&ctx, &app, 1);
        let close = egui::pos2(tab.right() - 12.0, tab.center().y);
        let g = app.views[1].graph_rect.expect("view 1's graph floats");
        assert!(g.contains(tab.center()) && g.contains(close), "the graph is under the name tag: {g:?} {tab:?}");
        let frame = app.views[1].pb.frame;
        let gbox = app.settings.graph_boxes[1];
        // The name tag is painted over the graph (after it).
        let out = frame_with(&ctx, &mut app, vec![]);
        let name = text_rects(&out, "b.tak").into_iter().find(|t| tab.contains(t.center())).expect("name tag painted");
        let last_name = out.shapes.iter().rposition(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == "b.tak")).unwrap();
        let graph_tag = out.shapes.iter().rposition(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == "M" && g.contains(t.pos))).expect("the graph's tag is painted");
        assert!(last_name > graph_tag, "the name tag is on top of the graph");
        // The mouse on the name (and on its ✕) is the tab's, not the graph's.
        for p in [name.center(), close] {
            frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(p)]);
            frame_with(&ctx, &mut app, vec![]);
            assert!(view_tab_response(&ctx, &app, 1).hovered(), "egui hit-tests the tab at {p:?}");
        }
        // Click on the name: view 1 becomes the chosen one; the graph under it got nothing.
        click_at(&ctx, &mut app, name.center());
        assert_eq!(app.active, 1);
        assert_eq!(app.dock.find_active_focused().map(|(_, t)| *t), Some(Pane::View(1)), "egui_dock's tab took the click");
        assert_eq!((app.views[1].pb.frame, app.settings.graph_boxes[1]), (frame, gbox), "the graph under the tag did not take the click");
        // Drag the name tag near the left edge of view 0: view 1 moves there (v0.9.0).
        let p0 = place_of(&app, Pane::View(0));
        drag(&ctx, &mut app, name.center(), egui::pos2(p0.left() + 0.08 * p0.width(), p0.center().y));
        frame_with(&ctx, &mut app, vec![]);
        let (r0, r1) = (place_of(&app, Pane::View(0)), place_of(&app, Pane::View(1)));
        assert!(r1.right() <= r0.left() + 1.0, "view 1 moved left of view 0: {r0:?} {r1:?}");
        assert_eq!((app.views[1].pb.frame, app.settings.graph_boxes[1]), (frame, gbox), "the graph did not move or scrub");
        // ✕ on the name tag (egui_dock's close button under it) closes view 1.
        frame_with(&ctx, &mut app, vec![]);
        let tab = view_tab(&ctx, &app, 1);
        let close = egui::pos2(tab.right() - 12.0, tab.center().y);
        assert!(app.views[1].graph_rect.is_some_and(|g| g.contains(close)), "✕ still over the graph");
        click_at(&ctx, &mut app, close);
        assert_eq!(dock::views(&app.dock), vec![0], "✕ on the name tag closed view 1");
    }

    #[test]
    fn a_view_tabbed_with_a_pane_keeps_its_graph_below_the_tab_row() {
        let ctx = egui::Context::default();
        let (mut app, _) = one_view_with_graph(&ctx);
        let loc = app.dock.find_tab(&Pane::Labeling).unwrap();
        app.dock.remove_tab(loc);
        let (node, _) = app.dock.main_surface().find_tab(&Pane::View(0)).unwrap();
        if let egui_dock::Node::Leaf { tabs, .. } = &mut app.dock.main_surface_mut()[node] {
            tabs.push(Pane::Labeling);
        }
        app.settings.graph_boxes[0].size = Some([400.0, 120.0]);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        let place = place_of(&app, Pane::View(0));
        let whole = app.views[0].scene_rect;
        assert!((whole.top() - (place.top() + dock::TAB_H)).abs() < 0.5, "tab row kept: {whole:?} in {place:?}");
        // Dragged by its tag far up: it stops below the tab row.
        let points = |app: &TakViewerApp| graph_float::test_points(Kind::Marker, &app.settings.graph_boxes[0], whole, egui::Rect::NOTHING, marker_name_w(&ctx));
        let grip = points(&app)[0];
        drag(&ctx, &mut app, grip, grip + egui::vec2(100.0, -2000.0));
        let g = app.views[0].graph_rect.unwrap();
        assert!((g.top() - (whole.top() + 6.0)).abs() < 0.5, "as high as it goes: below the tab row: {g:?} in {place:?}");
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].graph_rect, Some(g));
        // A right drag in the tab row is not the 3D's.
        let cam = (app.views[0].cam.yaw, app.views[0].cam.pitch);
        let row = egui::pos2(place.right() - 40.0, place.top() + dock::TAB_H / 2.0);
        drag_with(&ctx, &mut app, row, row + egui::vec2(120.0, 60.0), egui::PointerButton::Secondary);
        assert_eq!((app.views[0].cam.yaw, app.views[0].cam.pitch), cam);
    }

    /// `n` views side by side, files a.tak, b.tak … with markers "M", "N" and force plates "P1",
    /// "P2" (forces read, `views::sample_with_plates`), marker 0 selected in each (its graph
    /// floats), view 0 active, each 3D turned around P1 (it is in the middle of the 3D). Drawn
    /// three times.
    fn views_with_plates(ctx: &egui::Context, n: usize) -> TakViewerApp {
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        for k in 1..n {
            dock::add_view(&mut app.dock, k as u8, dock::NOMINAL_AREA);
        }
        for k in 0..n {
            let (_tx, rx) = std::sync::mpsc::channel();
            app.views[k].show(views::sample_with_plates(["a.tak", "b.tak", "c.tak", "d.tak"][k], &["M", "N"]), rx, 10 + k, 1.0, 0.0);
            app.views[k].select(0, false);
        }
        app.activate(0);
        frame_with(ctx, &mut app, vec![]);
        frame_with(ctx, &mut app, vec![]);
        for v in &mut app.views[..n] {
            v.cam.pivot = v3(-0.35, 0.0, 0.0);
        }
        frame_with(ctx, &mut app, vec![]);
        app
    }

    /// Force plates of view `k` as drawn in the last frame.
    fn plate_quads(app: &TakViewerApp, k: usize) -> Vec<scene::PlateQuad> {
        let v = &app.views[k];
        let view = v.cam.view(v.scene_rect);
        let d = v.data.as_ref().expect("a file");
        let quad = |(pi, p): (usize, &tak_core::view_data::PlateView)| {
            let c: Vec<egui::Pos2> = p.corners.iter().filter_map(|&c| view.project(c).map(|x| x.0)).collect();
            (c.len() == 4).then(|| scene::PlateQuad { plate: pi, corners: [c[0], c[1], c[2], c[3]] })
        };
        d.plates.iter().enumerate().filter_map(quad).collect()
    }

    /// Middle of plate `pi` of view `k` on screen; nothing else to click there (no marker).
    fn plate_middle(app: &TakViewerApp, k: usize, pi: usize) -> egui::Pos2 {
        let q = plate_quads(app, k).into_iter().find(|q| q.plate == pi).expect("the plate is on screen");
        let at = q.corners.iter().fold(egui::Pos2::ZERO, |s, c| s + c.to_vec2() / 4.0);
        assert_eq!(markers_at(app, k, at), None, "no marker there");
        at
    }

    /// The marker of view `k` a click at `at` would pick.
    fn markers_at(app: &TakViewerApp, k: usize, at: egui::Pos2) -> Option<usize> {
        let v = &app.views[k];
        let d = v.data.as_ref().unwrap();
        scene::pick(&scene::project_markers(&v.cam.view(v.scene_rect), d, v.pb.frame, &app.settings.scene, &v.selected), at)
    }

    /// Frames without input for longer than egui's double-click time (1/60 s each), so the next
    /// click is a click of its own.
    fn idle(ctx: &egui::Context, app: &mut TakViewerApp) {
        for _ in 0..30 {
            frame_with(ctx, app, vec![]);
        }
    }

    /// Width of a floating graph's name.
    fn name_w(ctx: &egui::Context, name: &str) -> f32 {
        ctx.fonts(|f| f.layout_no_wrap(name.to_owned(), egui::FontId::proportional(12.0), egui::Color32::WHITE)).size().x
    }

    #[test]
    fn a_click_on_a_plate_chooses_it_keeps_the_markers_and_floats_its_fz_over_that_view_only() {
        let ctx = egui::Context::default();
        let mut app = views_with_plates(&ctx, 2);
        assert!(app.views[0].graph_rect.is_some() && app.views[0].plate_rect.is_none(), "no plate chosen: no plate graph");
        let p1 = plate_middle(&app, 0, 0);
        click_at(&ctx, &mut app, p1);
        assert_eq!(app.views[0].plate, Some(0), "the click chose P1");
        assert_eq!((app.views[0].primary, app.views[0].selected.len()), (Some(0), 1), "the marker selection stays");
        assert_eq!(app.views[1].plate, None, "the other view has its own choice");
        // Both graphs float over view 0, each in its own place: the plate's along the top just
        // below the tab-row strip, the marker's along the bottom. None over view 1.
        let (whole, g, p) = (app.views[0].scene_rect, app.views[0].graph_rect.unwrap(), app.views[0].plate_rect.expect("P1's Fz floats"));
        assert!((p.top() - (whole.top() + dock::TAB_H)).abs() < 0.5 && (p.width() - (whole.width() - 12.0)).abs() < 0.5, "{p:?} in {whole:?}");
        assert!(p.bottom() < g.top() && whole.contains_rect(p), "{p:?} {g:?}");
        assert!(app.views[1].plate_rect.is_none() && app.views[1].graph_rect.is_some());
        let tags: Vec<egui::Rect> = text_rects(&frame_with(&ctx, &mut app, vec![]), "P1");
        assert!(tags.iter().any(|t| p.contains(t.center())), "■ P1 name tag on the plate graph");
        // A click inside it goes to that frame in that view only (after the double-click time:
        // a double click shows the whole range instead).
        idle(&ctx, &mut app);
        click_at(&ctx, &mut app, egui::pos2(p.right() - 12.0, p.center().y));
        assert_eq!((app.views[0].pb.frame, app.views[1].pb.frame), (9, 0));
        assert_eq!((app.views[0].plate, app.views[0].primary), (Some(0), Some(0)));
        // P1 again: un-chosen, markers still selected.
        click_at(&ctx, &mut app, p1);
        assert_eq!((app.views[0].plate, app.views[0].primary), (None, Some(0)));
        assert!(app.views[0].plate_rect.is_none());
        // Chosen again, then a click on empty 3D: the markers go, the plate stays.
        click_at(&ctx, &mut app, p1);
        let empty = egui::pos2(whole.left() + 20.0, whole.center().y);
        assert!(markers_at(&app, 0, empty).is_none() && scene::pick_plate(&plate_quads(&app, 0), empty).is_none());
        click_at(&ctx, &mut app, empty);
        assert_eq!((app.views[0].plate, app.views[0].primary), (Some(0), None));
        // Ctrl+click on the plate does not choose it (Ctrl-click is for markers).
        let ctrl = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
        let button = |pressed| egui::Event::PointerButton { pos: p1, button: egui::PointerButton::Primary, pressed, modifiers: ctrl };
        let with_ctrl = |events| egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0))), focused: true, modifiers: ctrl, events, ..Default::default() };
        for events in [vec![egui::Event::PointerMoved(p1)], vec![button(true)], vec![button(false)], vec![]] {
            let _ = ctx.run(with_ctrl(events), |ctx| app.frame(ctx));
        }
        assert_eq!(app.views[0].plate, Some(0));
    }

    #[test]
    fn the_plate_graph_moves_by_its_tag_resizes_folds_and_its_x_closes_it() {
        let ctx = egui::Context::default();
        let mut app = views_with_plates(&ctx, 1);
        app.views[0].plate = Some(0);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        let area = app.views[0].scene_rect;
        let tab = view_tab(&ctx, &app, 0);
        let w = name_w(&ctx, "P1");
        let points = |app: &TakViewerApp| graph_float::test_points(P1_FORCES, &app.settings.plate_boxes[0], area, tab, w);
        let near = |a: egui::Rect, b: egui::Rect| (a.min - b.min).length() < 1.0 && (a.max - b.max).length() < 1.0;
        let g = app.views[0].plate_rect.expect("P1's graph floats");
        let marker = app.views[0].graph_rect.expect("the marker graph floats too");
        assert!(points(&app)[0].y > tab.bottom(), "its tag is clear of the file name tag");

        // Lower-right corner: 300 narrower, 40 higher; the top-left corner stays.
        drag(&ctx, &mut app, g.max - egui::vec2(3.0, 3.0), g.max + egui::vec2(-303.0, 37.0));
        let r = app.views[0].plate_rect.unwrap();
        assert!(near(r, egui::Rect::from_min_max(g.min, g.max + egui::vec2(-300.0, 40.0))), "{r:?} from {g:?}");
        // The tag moves it, exactly with the mouse.
        let tag = points(&app)[0];
        drag(&ctx, &mut app, tag, tag + egui::vec2(100.0, 150.0));
        let moved = app.views[0].plate_rect.unwrap();
        assert!(near(moved, r.translate(egui::vec2(100.0, 150.0))), "{moved:?} from {r:?}");
        assert_eq!(app.views[0].graph_rect, Some(marker), "the marker graph keeps its own box");
        assert_eq!(app.settings.graph_boxes[0], GraphBox::default());
        // ▼ folds it to its tag, ▲ opens it there again.
        let fold = points(&app)[2];
        click_at(&ctx, &mut app, fold);
        assert!(app.settings.plate_boxes[0].collapsed && app.views[0].plate_rect.is_none() && app.views[0].plate == Some(0));
        let unfold = points(&app)[2];
        click_at(&ctx, &mut app, unfold);
        assert_eq!(app.views[0].plate_rect, Some(moved));
        // The lanes (Fx, Fy, Fz) were made once each, not again on every frame.
        assert_eq!(app.views[0].plate_lanes.builds, 3);
        // ✕: the plate is no longer chosen; the marker and its graph stay.
        let close = points(&app)[3];
        click_at(&ctx, &mut app, close);
        assert_eq!((app.views[0].plate, app.views[0].primary), (None, Some(0)));
        assert!(app.views[0].plate_rect.is_none() && app.views[0].graph_rect.is_some());
        // Place and size are saved with the settings.
        app.sync_settings();
        let back: Settings = ron::from_str(&ron::to_string(&app.settings).unwrap()).unwrap();
        assert_eq!(back.plate_boxes, app.settings.plate_boxes);
        assert!(back.plate_boxes[0].pos.is_some() && back.plate_boxes[0].size.is_some());
    }

    /// P1's graph with the first choice of lanes: Fx · Fy · Fz of six (`views::sample_with_plates`).
    const P1_FORCES: Kind = Kind::Plate { shown: LaneSet(0b000_111), offer: LaneSet(0b111_111) };

    /// Where the open "줄" list painted the lane `name` (None while the list is closed): below
    /// its header "보일 줄" and right of where the graph paints its big lane letters.
    fn list_item(out: &egui::FullOutput, name: &str) -> Option<egui::Rect> {
        let head = text_rects(out, "보일 줄").into_iter().next()?;
        text_rects(out, name).into_iter().find(|r| r.left() > head.left() && r.top() > head.bottom())
    }

    /// Big lane letters the graph `r` painted (at its left edge, not the "줄" list's), top to
    /// bottom.
    fn lane_letters(out: &egui::FullOutput, r: egui::Rect) -> Vec<&'static str> {
        // The big faded lane letters (a low first lane has its letter right of the name tag),
        // not the same names in the "줄" list: told apart by the faded color.
        fn walk(shape: &egui::Shape, r: egui::Rect, found: &mut Vec<(f32, &'static str)>) {
            match shape {
                egui::Shape::Text(t) => {
                    let faded = t.galley.job.sections.first().is_some_and(|s| s.format.color == egui::Color32::from_white_alpha(22));
                    let name = graph_float::PLATE_LANES.iter().map(|&(name, _)| name).find(|name| t.galley.text() == *name);
                    let rect = egui::Rect::from_min_size(t.pos, t.galley.size());
                    if let Some(name) = name.filter(|_| faded && r.contains(rect.center())) {
                        found.push((rect.top(), name));
                    }
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, r, found)),
                _ => {}
            }
        }
        let mut found: Vec<(f32, &'static str)> = vec![];
        out.shapes.iter().for_each(|s| walk(&s.shape, r, &mut found));
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        found.into_iter().map(|f| f.1).collect()
    }

    /// One view with P1 chosen (its graph floats) at frame 5, the graph `size` big at its first
    /// place. Returns the app and the view's tab.
    fn plate_graph_at_frame_5(ctx: &egui::Context, size: Option<[f32; 2]>) -> (TakViewerApp, egui::Rect) {
        let mut app = views_with_plates(ctx, 1);
        app.views[0].plate = Some(0);
        app.views[0].pb.set_frame(5, 0.0);
        app.settings.plate_boxes[0].size = size;
        frame_with(ctx, &mut app, vec![]);
        frame_with(ctx, &mut app, vec![]);
        let tab = view_tab(ctx, &app, 0);
        (app, tab)
    }

    #[test]
    fn the_plate_graph_shows_fx_fy_fz_and_its_lane_list_adds_and_removes_lanes_at_once() {
        let ctx = egui::Context::default();
        // Low graph: the lower half of the list lies over the 3D, the upper half over the graph.
        let (mut app, tab) = plate_graph_at_frame_5(&ctx, Some([600.0, 100.0]));
        let area = app.views[0].scene_rect;
        let w = name_w(&ctx, "P1");
        let points = graph_float::test_points(P1_FORCES, &app.settings.plate_boxes[0], area, tab, w);
        let g = app.views[0].plate_rect.expect("P1's graph floats");
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(lane_letters(&out, g), ["Fx", "Fy", "Fz"], "first choice: the forces, top to bottom");
        assert!(list_item(&out, "Mx").is_none(), "the list is closed");
        let unmoved = |app: &TakViewerApp| {
            assert_eq!((app.views[0].pb.frame, app.views[0].primary, app.views[0].plate), (5, Some(0), Some(0)), "the graph and the 3D under the list got nothing");
            assert_eq!(app.views[0].plate_rect, Some(g), "the graph stays where it is, as high as it is");
        };
        // "줄" opens the list right under it: six lanes, those shown checked.
        let out = click_at(&ctx, &mut app, points[1]);
        let head = text_rects(&out, "보일 줄");
        assert_eq!(head.len(), 1, "the list is open");
        assert!((head[0].left() - points[1].x).abs() < 30.0 && head[0].top() > points[1].y, "right under the button: {head:?}");
        let items: Vec<egui::Rect> = graph_float::PLATE_LANES.iter().map(|l| list_item(&out, l.0).expect("every lane is in the list")).collect();
        assert!(items.windows(2).all(|w| w[0].top() < w[1].top()), "Fx … Mz top to bottom");
        assert!(items[5].center().y > g.bottom() && items[0].center().y < g.bottom(), "test set-up: the list reaches over the 3D");
        unmoved(&app);
        // Clicks on the list itself, not on a check box (its header over the graph, the divider
        // over the 3D), stay on the list: nothing under it moves, the list stays open.
        let divider = egui::pos2(items[2].center().x, (items[2].bottom() + items[3].top()) / 2.0);
        assert!(g.contains(head[0].center()) && !g.contains(divider), "test set-up");
        click_at(&ctx, &mut app, head[0].center());
        let out = click_at(&ctx, &mut app, divider);
        unmoved(&app);
        assert!(list_item(&out, "Mx").is_some(), "the list stays open");
        assert_eq!(app.settings.plate_channels[0], LaneSet::default());
        // Mx (over the 3D) checked: a lane more, at once; the list stays open.
        let out = click_at(&ctx, &mut app, items[3].center());
        assert_eq!(app.settings.plate_channels[0], LaneSet(0b001_111));
        assert_eq!(lane_letters(&out, g), ["Fx", "Fy", "Fz", "Mx"]);
        assert!(list_item(&out, "Mx").is_some(), "still open");
        assert!(!text_rects(&out, "줄").is_empty(), "the buttons stay while the list is open, the mouse on the list");
        unmoved(&app);
        // Fy (over the graph) unchecked: away.
        let out = click_at(&ctx, &mut app, items[1].center());
        assert_eq!(lane_letters(&out, g), ["Fx", "Fz", "Mx"]);
        unmoved(&app);
        // Down to Fz alone; Fz then cannot be unchecked.
        click_at(&ctx, &mut app, items[0].center());
        click_at(&ctx, &mut app, items[3].center());
        assert_eq!(app.settings.plate_channels[0], LaneSet::only(2));
        let out = click_at(&ctx, &mut app, items[2].center());
        assert_eq!(app.settings.plate_channels[0], LaneSet::only(2), "the last lane stays");
        assert_eq!(lane_letters(&out, g), ["Fz"]);
        unmoved(&app);
        assert_eq!(app.views[0].plate_lanes.builds, 4, "Fx Fy Fz once, Mx once: nothing built again");
        // Esc closes the list only (the marker selection stays).
        let esc = egui::Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        frame_with(&ctx, &mut app, vec![esc]);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert!(list_item(&out, "Mx").is_none(), "Esc closed the list");
        unmoved(&app);
        // Open again; a click elsewhere (here: the graph, which goes to that frame) closes it.
        let out = click_at(&ctx, &mut app, points[1]);
        assert!(list_item(&out, "Mx").is_some());
        idle(&ctx, &mut app);
        let out = click_at(&ctx, &mut app, egui::pos2(g.right() - 12.0, g.center().y));
        assert!(list_item(&out, "Mx").is_none(), "a click elsewhere closed the list");
        assert_eq!(app.views[0].pb.frame, 9, "that click went to the graph");
    }

    #[test]
    fn the_plate_graph_keeps_its_height_when_lanes_change_and_grows_only_to_the_smallest() {
        let ctx = egui::Context::default();
        let (mut app, tab) = plate_graph_at_frame_5(&ctx, Some([600.0, 100.0]));
        let area = app.views[0].scene_rect;
        let points = graph_float::test_points(P1_FORCES, &app.settings.plate_boxes[0], area, tab, name_w(&ctx, "P1"));
        let g = app.views[0].plate_rect.unwrap();
        assert_eq!(g.height(), 100.0);
        let out = click_at(&ctx, &mut app, points[1]);
        let item = |name| list_item(&out, name).expect("in the list").center();
        let (mx, my, mz) = (item("Mx"), item("My"), item("Mz"));
        // Four and five lanes fit in 100: the height stays, the lanes share it.
        click_at(&ctx, &mut app, mx);
        click_at(&ctx, &mut app, my);
        assert_eq!((app.views[0].plate_rect, app.settings.plate_boxes[0].size), (Some(g), Some([600.0, 100.0])));
        // Six lanes need more: it grows to their smallest height, the top stays.
        click_at(&ctx, &mut app, mz);
        let six = app.views[0].plate_rect.unwrap();
        assert!(six.height() > 100.0 && six.min == g.min, "{six:?}");
        assert_eq!(app.settings.plate_boxes[0].size, Some([600.0, six.height()]));
        // Back to five: it keeps that height.
        click_at(&ctx, &mut app, mz);
        assert_eq!(app.views[0].plate_rect, Some(six));
        // Never resized (first size): the same first height for one lane and for three.
        app.settings.plate_boxes[0] = GraphBox::default();
        app.settings.plate_channels[0] = LaneSet::default();
        frame_with(&ctx, &mut app, vec![]);
        let three = app.views[0].plate_rect.unwrap();
        app.settings.plate_channels[0] = LaneSet::only(2);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].plate_rect, Some(three));
        assert_eq!(three.height(), app.views[0].graph_rect.unwrap().height(), "as high as the marker graph's first height");
    }

    #[test]
    fn the_chosen_lanes_belong_to_their_view_and_are_saved_with_the_settings() {
        let ctx = egui::Context::default();
        let mut app = views_with_plates(&ctx, 2);
        for v in &mut app.views[..2] {
            v.plate = Some(0);
        }
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        let area = app.views[0].scene_rect;
        let points = graph_float::test_points(P1_FORCES, &app.settings.plate_boxes[0], area, view_tab(&ctx, &app, 0), name_w(&ctx, "P1"));
        let out = click_at(&ctx, &mut app, points[1]);
        let mx = list_item(&out, "Mx").expect("view 0's list").center();
        let out = click_at(&ctx, &mut app, mx);
        assert_eq!(app.settings.plate_channels[..2], [LaneSet(0b001_111), LaneSet::default()], "view 0 only");
        assert_eq!(lane_letters(&out, app.views[0].plate_rect.unwrap()), ["Fx", "Fy", "Fz", "Mx"]);
        assert_eq!(lane_letters(&out, app.views[1].plate_rect.unwrap()), ["Fx", "Fy", "Fz"]);
        app.sync_settings();
        let back: Settings = ron::from_str(&ron::to_string(&app.settings).unwrap()).unwrap();
        assert_eq!(back.plate_channels, app.settings.plate_channels);
        let esc = egui::Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        frame_with(&ctx, &mut app, vec![esc]);
        // Another file in view 0 (still a plate chosen): the same lanes.
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_with_plates("c.tak", &["M"]), rx, 30, 1.0, 0.0);
        app.views[0].plate = Some(0);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert!(list_item(&out, "Mx").is_none(), "the list was closed");
        assert_eq!(lane_letters(&out, app.views[0].plate_rect.unwrap()), ["Fx", "Fy", "Fz", "Mx"]);
        // A plate whose device has three channels shows what it has, and its list offers only those.
        app.views[0].plate = Some(1);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(lane_letters(&out, app.views[0].plate_rect.unwrap()), ["Fx", "Fy", "Fz"]);
        let p2 = graph_float::test_points(P1_FORCES, &app.settings.plate_boxes[0], area, view_tab(&ctx, &app, 0), name_w(&ctx, "P2"));
        let out = click_at(&ctx, &mut app, p2[1]);
        assert!(list_item(&out, "Fz").is_some() && list_item(&out, "Mx").is_none(), "its list offers Fx Fy Fz only");
        assert_eq!(app.settings.plate_channels[0], LaneSet(0b001_111), "what view 0 wants stays for a plate with moments");
    }

    #[test]
    fn settings_from_before_the_plate_lanes_still_load() {
        // v0.11.0 text: no plate_channels.
        let s = Settings { workspace_roots: vec![r"C:\data".into()], ..Settings::default() };
        let text = ron::to_string(&s).unwrap();
        let at = text.find(",plate_channels:").expect("plate_channels is saved");
        let old = format!("{})", &text[..at]);
        assert!(!old.contains("plate_channels") && old.contains("plate_boxes"), "{old}");
        let mut back: Settings = ron::from_str(&old).expect("v0.11.0 settings still load");
        back.migrate();
        assert_eq!(back.workspace_roots, s.workspace_roots, "saved folders kept");
        assert_eq!(back.plate_channels, vec![LaneSet::default(); MAX_VIEWS], "Fx · Fy · Fz in every view");
        // A short list from a hand-edited file: every view slot gets one.
        let mut short: Settings = ron::from_str(&format!("{},plate_channels:[40])", &text[..at])).unwrap();
        short.migrate();
        assert_eq!(short.plate_channels, [LaneSet(40), LaneSet::default(), LaneSet::default(), LaneSet::default()]);
    }

    #[test]
    fn the_devices_pane_3d_button_chooses_the_plate_for_the_active_view() {
        let ctx = egui::Context::default();
        let mut app = views_with_plates(&ctx, 2);
        app.activate(1);
        // The devices pane is the tab next to the selected marker: to the front.
        let loc = app.dock.find_tab(&Pane::Devices).expect("devices pane in the first layout");
        app.dock.set_active_tab(loc);
        frame_with(&ctx, &mut app, vec![]);
        let place = place_of(&app, Pane::Devices);
        let buttons = |app: &mut TakViewerApp| {
            let mut b: Vec<egui::Rect> = text_rects(&frame_with(&ctx, app, vec![]), "3D 위로").into_iter().filter(|r| place.contains(r.center())).collect();
            b.sort_by(|x, y| x.top().total_cmp(&y.top()));
            b
        };
        let b = buttons(&mut app);
        assert_eq!(b.len(), 2, "one per plate: {b:?}");
        click_at(&ctx, &mut app, b[1].center());
        assert_eq!((app.views[1].plate, app.views[0].plate), (Some(1), None), "P2 for the active view only");
        assert!(app.views[1].plate_rect.is_some() && app.views[0].plate_rect.is_none());
        assert_eq!(app.active, 1);
        // Pressed again: un-chosen.
        let again = buttons(&mut app)[1].center();
        click_at(&ctx, &mut app, again);
        assert_eq!(app.views[1].plate, None);
    }

    #[test]
    fn collapsed_device_manifest_opens_loading_emg_and_channel_choice_undo_redo() {
        use tak_core::tak::{AnalogDevice, AnalogChannel};
        let ctx = egui::Context::default();
        let mut app = views_with_plates(&ctx, 1);
        let mut take = views::sample_data("a.tak", &["M"]).take;
        take.analog.push(AnalogDevice { channels: (0..2).map(|i| AnalogChannel {
            name: format!("AnalogChannel{i}"), kind: "Analog".into(), dim: 1,
            frames: vec![], values: vec![],
        }).collect() });
        let mut data = tak_core::view_data::ViewData::build(take);
        data.emg_pending = true;
        app.views[0].data = Some(std::sync::Arc::new(data));
        dock::show(&mut app.dock, Pane::Devices);
        let out = frame_with(&ctx, &mut app, vec![]);
        let header = text_rects(&out, "아날로그 장치 1");
        assert_eq!(header.len(), 1, "manifest header is visible before any sample read");
        assert!(text_rects(&out, "Loading EMG…").is_empty());
        assert!(!app.views[0].devices.request_emg);
        let mut out = click_at(&ctx, &mut app, header[0].center());
        for _ in 0..3 { out = frame_with(&ctx, &mut app, vec![]); }
        assert!(!text_rects(&out, "Loading EMG…").is_empty());
        assert!(app.views[0].devices.request_emg, "opening the existing header must request lazy samples");
        assert_eq!(app.views[0].devices.channel.get(&0), Some(&0));
        let choose = text_rects(&out, "AnalogChannel0");
        let out = click_at(&ctx, &mut app, choose.last().unwrap().center());
        let choice = text_rects(&out, "AnalogChannel1 (1값/프레임)");
        assert!(!choice.is_empty(), "channel headers remain selectable while loading");
        click_at(&ctx, &mut app, choice[0].center());
        assert_eq!(app.views[0].devices.channel.get(&0), Some(&1));
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[0].devices.channel.get(&0), Some(&0));
        app.undo_redo(&ctx, true);
        assert_eq!(app.views[0].devices.channel.get(&0), Some(&1));
    }

    #[test]
    fn actual_lazy_file_close_undo_redo_preserves_jobs_before_and_during_emg_read() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../input/static/000000_S01_static.tak");
        if !path.exists() { return; }
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let ready = |app: &mut TakViewerApp| {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                frame_with(&ctx, app, vec![]);
                if app.views[0].data.is_some() && app.views[0].loading.is_none() && !app.views[0].analog_work_pending() { break; }
                assert!(Instant::now() < deadline, "file must reopen without forcing EMG");
                std::thread::sleep(Duration::from_millis(1));
            }
            frame_with(&ctx, app, vec![]);
            assert!(app.views[0].data.as_ref().unwrap().emg_pending && !app.views[0].emg_requested);
        };
        app.open(&ctx, path.clone()); ready(&mut app);
        let first_id = app.views[0].file_id;
        let first_data = app.views[0].data.clone().unwrap();
        let document = app.views[0].edit_document.clone().unwrap();
        app.close_view(0); frame_with(&ctx, &mut app, vec![]);
        assert!(app.open_views().is_empty() && app.views[0].data.is_none());
        app.undo_redo(&ctx, false); ready(&mut app);
        assert_eq!(app.views[0].file_id, first_id, "reopening retains the same view and reader");
        assert!(std::sync::Arc::ptr_eq(&first_data, app.views[0].data.as_ref().unwrap()),
            "last-close undo keeps the loaded marker and forceplate data");
        assert!(std::sync::Arc::ptr_eq(&document, app.views[0].edit_document.as_ref().unwrap()));
        app.views[0].devices.request_emg = true;
        app.views[0].request_emg(&ctx);
        assert!(app.views[0].emg_requested && app.views[0].emg_rx.is_some());
        app.close_view(0); frame_with(&ctx, &mut app, vec![]);
        app.undo_redo(&ctx, false);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].file_id, first_id);
        assert!(std::sync::Arc::ptr_eq(&document, app.views[0].edit_document.as_ref().unwrap()));
        assert!(app.views[0].emg_requested || !app.views[0].data.as_ref().unwrap().emg_pending,
            "the in-flight read must survive closing and reopening");
        app.undo_redo(&ctx, true); frame_with(&ctx, &mut app, vec![]);
        assert!(app.views[0].data.is_none(), "redo closes the same restored file");
        app.undo_redo(&ctx, false);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].file_id, first_id);
        assert!(std::sync::Arc::ptr_eq(&document, app.views[0].edit_document.as_ref().unwrap()));
        app.views[0].devices.request_emg = true;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            frame_with(&ctx, &mut app, vec![]);
            if !app.views[0].data.as_ref().unwrap().emg_pending { break; }
            assert!(Instant::now() < deadline, "restored file must still load EMG on access");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(app.views[0].load_steps.iter().filter(|(stage, _)| *stage == "emg").count(), 1);
    }

    #[test]
    fn bench_state_retains_lazy_emg_failure_after_loading_finishes() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let mut data = views::sample_data("failed_emg.tak", &["M"]);
        data.emg_pending = true;
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(data, rx, 17, 1.0, 0.0);
        app.views[0].emg_requested = true;
        let (tx, rx) = std::sync::mpsc::channel();
        app.views[0].emg_rx = Some((17, rx));
        tx.send(views::LoadMsg::Analog(Err("TAK changed during marker extraction".into()), 1.0)).unwrap();
        app.views[0].poll_analog();
        assert!(!app.bench_emg_pending(), "a completed failure is no longer pending");
        let state = app.bench_state();
        assert_eq!(state.views.len(), 1);
        assert!(!state.views[0].lazy_analog);
        assert!(state.views[0].emg_failure.as_ref().is_some_and(|error| error.contains("TAK changed")),
            "the benchmark must distinguish successful completion from a failed lazy read");
        assert!(state.views[0].file.ends_with("failed_emg.tak"));
    }

    #[test]
    fn capture_takes_precedence_over_path_and_tab_shortcuts() {
        let ctx = egui::Context::default();
        let mut app = views_with_plates(&ctx, 1);
        // Keep capture waiting for the outstanding force data during these key presses.
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].analog_rx = Some((app.views[0].file_id, rx));
        app.start_menu_capture();
        assert!(app.capture.is_some());
        let tab = app.layout_tabs.active_id();
        let count = app.layout_tabs.tabs().len();
        run_frame(&ctx, &mut app, &[(ctrl(), Key::L)]);
        assert!(app.capture.is_some());
        assert!(app.path_input.is_none(), "capture must not open the path dialog");
        run_frame(&ctx, &mut app, &[(ctrl(), Key::T)]);
        assert_eq!(app.layout_tabs.active_id(), tab);
        assert_eq!(app.layout_tabs.tabs().len(), count);
    }

    #[test]
    fn no_floating_graph_while_capturing() {
        // Pictures show the 3D only.
        let ctx = egui::Context::default();
        let mut app = views_with_plates(&ctx, 1);
        app.views[0].plate = Some(0);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.views[0].graph_rect.is_some() && app.views[0].plate_rect.is_some());
        // A menu capture waiting for this view's forces: nothing is saved yet.
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].analog_rx = Some((app.views[0].file_id, rx));
        app.start_menu_capture();
        frame_with(&ctx, &mut app, vec![]);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert!(app.capture.is_some());
        assert!(app.views[0].graph_rect.is_none() && app.views[0].plate_rect.is_none());
        assert!(text_rects(&out, "P1").is_empty(), "no plate graph name tag painted");
        app.capture = None;
        app.views[0].analog_rx = None;
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.views[0].graph_rect.is_some() && app.views[0].plate_rect.is_some());
    }

    #[test]
    fn no_plate_graph_while_the_forces_load() {
        let ctx = egui::Context::default();
        let mut app = views_with_plates(&ctx, 1);
        app.views[0].plate = Some(0);
        std::sync::Arc::get_mut(app.views[0].data.as_mut().unwrap()).unwrap().analog_pending = true;
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.views[0].plate_rect.is_none() && app.views[0].graph_rect.is_some());
        assert_eq!(app.views[0].plate_lanes.builds, 0, "no lane made from forces not read yet");
        std::sync::Arc::get_mut(app.views[0].data.as_mut().unwrap()).unwrap().analog_pending = false;
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.views[0].plate_rect.is_some());
    }

    #[test]
    fn screenshot_plate_argument_chooses_that_plate_in_every_view() {
        let ctx = egui::Context::default();
        let shot = AutoShot { out: PathBuf::from("x.png"), frame: None, select: None, plate: Some("P2".into()), range: None, menu_capture: false, edit_tools: false, edit_pattern: false, edit_layout: 0, comments: false, pick_comment: None, edit_comment: None, type_strip: None, overlays: vec![], hide: vec![], hover: None, stage: 0 };
        let mut app = TakViewerApp::build(&ctx, None, vec![], Some(shot), None);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        for (k, name) in [(0, "a.tak"), (1, "b.tak")] {
            let (_tx, rx) = std::sync::mpsc::channel();
            app.views[k].show(views::sample_with_plates(name, &["M"]), rx, 10 + k, 1.0, 0.0);
        }
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!((app.views[0].plate, app.views[1].plate), (Some(1), Some(1)));
        assert!(app.views[0].plate_rect.is_some() && app.views[1].plate_rect.is_some());
    }

    #[test]
    fn settings_from_before_the_plate_graph_still_load() {
        // v0.10.0 text: no plate_boxes, remembered spots without a plate.
        let mut v = View::default();
        let (_tx, rx) = std::sync::mpsc::channel();
        v.show(views::sample_data("a.tak", &["M"]), rx, 1, 1.0, 0.0);
        let s = Settings { workspace_roots: vec![r"C:\data".into()], file_memory: vec![v.spot().unwrap()], ..Settings::default() };
        let text = ron::to_string(&s).unwrap();
        assert!(text.contains(",plate:None"), "{text}");
        let text = text.replace(",plate:None", "");
        let at = text.find("plate_boxes:").expect("plate_boxes is saved");
        let old = format!("{})", text[..at].trim_end_matches(','));
        assert!(!old.contains("plate_boxes") && !old.contains("plate:") && old.contains("graph_boxes"), "{old}");
        let mut back: Settings = ron::from_str(&old).expect("v0.10.0 settings still load");
        back.migrate();
        assert_eq!(back.workspace_roots, s.workspace_roots, "saved folders kept");
        assert_eq!(back.file_memory, s.file_memory, "spots kept");
        assert_eq!(back.plate_boxes, vec![GraphBox::default(); MAX_VIEWS]);
    }

    #[test]
    fn layout_and_hidden_panes_come_back_as_saved() {
        let mut app = window_app();
        dock::hide(&mut app.dock, Pane::Devices);
        app.toggle_side(Side::Right);
        assert!(!dock::has(&app.dock, Pane::Labeling));
        app.sync_settings();
        let text = ron::to_string(&app.settings).unwrap();
        let mut back: Settings = ron::from_str(&text).unwrap();
        let (dock, mut hidden) = back.take_layout();
        assert!(!dock::has(&dock, Pane::Labeling) && !dock::has(&dock, Pane::Devices), "a layout of this version is used as saved");
        assert_eq!(hidden, app.hidden);
        let mut dock = dock;
        dock::toggle_side(&mut dock, &mut hidden, dock::NOMINAL_AREA, Side::Right);
        assert!(dock::has(&dock, Pane::Labeling) && dock::has(&dock, Pane::Selected), "the toggle brings them back after a restart");
        // The menu shows a hidden pane right away; it is then no longer the toggle's.
        app.show_pane(Pane::Labeling);
        assert!(dock::has(&app.dock, Pane::Labeling));
        app.toggle_side(Side::Right);
        app.toggle_side(Side::Right);
        assert!(dock::has(&app.dock, Pane::Labeling) && dock::has(&app.dock, Pane::Selected), "no pane is lost or doubled");
        assert_eq!(app.active, 0);
    }

    #[test]
    fn version_two_layout_migrates_label_tabs_without_resetting_custom_panes() {
        let mut layout = dock::default_layout();
        let (surface, node, _) = layout.find_tab(&Pane::Labeling).unwrap();
        if let egui_dock::Node::Leaf { tabs, active, .. } = &mut layout[surface][node] {
            *tabs = vec![Pane::Labels];
            *active = egui_dock::TabIndex(0);
        }
        dock::show(&mut layout, Pane::Graph);
        let mut settings = Settings { layout: Some(layout), layout_version: 2, ..Settings::default() };
        let (layout, _) = settings.take_layout();
        assert!(dock::has(&layout, Pane::Graph));
        assert!(dock::has(&layout, Pane::Labeling) && dock::has(&layout, Pane::EditTools));
        assert!(!dock::has(&layout, Pane::Labels));
        let a = layout.find_tab(&Pane::Labeling).unwrap();
        let b = layout.find_tab(&Pane::EditTools).unwrap();
        assert_eq!((a.0, a.1), (b.0, b.1));
    }

    #[test]
    fn saved_label_panels_merge_before_validation_and_keep_files_and_floating_placement() {
        let mut layout = DockState::new(vec![Pane::View(1), Pane::View(3), Pane::Labels, Pane::Graph]);
        let window = layout.add_window(vec![Pane::Labeling, Pane::EditTools, Pane::Labeling]);
        layout.get_window_state_mut(window).unwrap().set_position(egui::pos2(120.0, 140.0)).set_size(egui::vec2(390.0, 620.0));
        let window_state = ron::to_string(layout.get_window_state(window).unwrap()).unwrap();
        let files = vec![(1, "first.takm".to_owned()), (3, "second.tak".to_owned())];
        let settings = Settings { layout: Some(layout), layout_version: 3, views: files.clone(), active_view: 3, ..Settings::default() };
        let saved = ron::to_string(&settings).unwrap();
        let mut back: Settings = ron::from_str(&saved).unwrap();
        let (mut layout, hidden) = back.take_layout();
        assert_eq!(dock::views(&layout), vec![1, 3], "merge happens before duplicate validation can reset the layout");
        assert_eq!(back.views, files);
        assert_eq!(back.active_view, 3);
        assert!(dock::has(&layout, Pane::Graph));
        assert!(!back.graph_float);
        assert!(!dock::has(&layout, Pane::Labels));
        assert_eq!(layout.iter_all_tabs().filter(|(_, pane)| **pane == Pane::Labeling).count(), 1);
        assert_eq!(layout.find_tab(&Pane::Labeling).unwrap().0, window);
        assert_eq!(ron::to_string(layout.get_window_state(window).unwrap()).unwrap(), window_state);
        assert_eq!(hidden, dock::Hidden::default());
    }

    #[test]
    fn saved_layout_without_either_marker_panel_stays_closed() {
        for version in 0..=3 {
            let layout = dock::scene_only(2);
            let settings = Settings { layout: Some(layout), layout_version: version, ..Settings::default() };
            let mut back: Settings = ron::from_str(&ron::to_string(&settings).unwrap()).unwrap();
            let (layout, hidden) = back.take_layout();
            assert_eq!(dock::views(&layout), vec![2]);
            assert!(!dock::has(&layout, Pane::Labels) && !dock::has(&layout, Pane::Labeling), "the marker list stays closed in version {version}");
            assert_eq!(dock::has(&layout, Pane::EditTools), version < 3, "older layouts gain only Edit Tools");
            assert_eq!(hidden, dock::Hidden::default());
        }
    }

    #[test]
    fn serialized_version_one_labels_layout_migrates_without_resetting_its_arrangement() {
        let mut layout = DockState::new(vec![Pane::View(1), Pane::View(3)]);
        let [_, labels] = layout.main_surface_mut().split_left(egui_dock::NodeIndex::root(), 0.27, vec![Pane::Labels, Pane::Devices]);
        layout.set_active_tab((egui_dock::SurfaceIndex::main(), labels, egui_dock::TabIndex(1)));
        dock::show(&mut layout, Pane::Graph);
        let mut expected = layout.clone();
        if let egui_dock::Node::Leaf { tabs, active, .. } = &mut expected.main_surface_mut()[labels] {
            *tabs = vec![Pane::Labeling, Pane::EditTools, Pane::Devices];
            *active = egui_dock::TabIndex(2);
        }
        let settings = Settings { layout: Some(layout), layout_version: 1, ..Settings::default() };
        let saved = ron::to_string(&settings).unwrap();
        assert!(saved.contains("Labels") && !saved.contains("Labeling") && !saved.contains("EditTools"));
        let mut back: Settings = ron::from_str(&saved).unwrap();
        let (layout, hidden) = back.take_layout();
        assert_eq!(dock::views(&layout), vec![1, 3]);
        assert_eq!(dock::shape(&layout), dock::shape(&expected), "split shares, tab positions and unrelated panes stay");
        assert_eq!(layout.find_tab(&Pane::Labeling).unwrap().1, labels);
        assert!(!dock::has(&layout, Pane::Labels));
        assert!(!back.graph_float, "the saved Graph pane stays open");
        assert_eq!(hidden, dock::Hidden::default());
    }

    #[test]
    fn marker_panel_migration_is_not_undoable_and_layout_undo_keeps_one_panel() {
        let ctx = egui::Context::default();
        let mut layout = dock::default_layout();
        layout.add_window(vec![Pane::Labels]);
        let settings = Settings { layout: Some(layout), layout_version: 3, ..Settings::default() };
        let mut storage = FileLike::default();
        eframe::set_value(&mut storage, eframe::APP_KEY, &settings);
        let mut app = TakViewerApp::build(&ctx, Some(&storage), vec![], None, None);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.history.kinds().is_empty(), "startup migration adds no history action");
        let check = |app: &TakViewerApp| {
            let visible = app.dock.iter_all_tabs().map(|(_, pane)| *pane);
            let hidden = [&app.hidden.left, &app.hidden.bottom, &app.hidden.right].into_iter().flatten()
                .flat_map(|stash| &stash.leaves).flat_map(|leaf| leaf.tabs.iter().copied());
            let panes: Vec<Pane> = visible.chain(hidden).collect();
            assert!(!panes.contains(&Pane::Labels));
            assert_eq!(panes.iter().filter(|p| **p == Pane::Labeling).count(), 1);
        };
        check(&app);
        app.toggle_side(Side::Right);
        app.observe(true);
        assert_eq!(app.history.kinds(), vec!["Layout"]);
        assert!(!dock::has(&app.dock, Pane::Labeling));
        check(&app);
        app.undo_redo(&ctx, false);
        assert!(dock::has(&app.dock, Pane::Labeling));
        check(&app);
        app.undo_redo(&ctx, true);
        assert!(!dock::has(&app.dock, Pane::Labeling));
        check(&app);
        app.show_pane(Pane::Labeling);
        app.observe(true);
        check(&app);
        app.undo_redo(&ctx, false);
        assert!(!dock::has(&app.dock, Pane::Labeling));
        check(&app);
        app.undo_redo(&ctx, true);
        assert!(dock::has(&app.dock, Pane::Labeling));
        check(&app);
    }

    /// One UI frame on a 1600×960 screen with these keys tapped: each pressed and let go in the
    /// same frame (as `--headless` runs frames). Since v0.15.0 a key is also let go, so egui does
    /// not take the next press of it as an auto-repeat; `key_down` / `key_up` hold a key over
    /// several frames.
    fn run_frame(ctx: &egui::Context, app: &mut TakViewerApp, keys: &[(Modifiers, Key)]) {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 960.0));
        let events = keys
            .iter()
            .flat_map(|&(modifiers, key)| [true, false].map(|pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }))
            .collect();
        let modifiers = keys.first().map_or(Modifiers::NONE, |k| k.0);
        let raw = egui::RawInput { screen_rect: Some(screen), focused: true, modifiers, events, ..Default::default() };
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
    }

    #[test]
    fn toggle_keys_hide_and_restore_the_sides_on_the_drawn_screen() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        // Panes on all three sides: the graph is in its pane below the view.
        app.set_graph_float(false);
        let ctrl = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
        let ctrl_alt = Modifiers { alt: true, ..ctrl };
        // Where the 3D view is drawn after a key press (one more frame lets the layout settle).
        let view_after = |app: &mut TakViewerApp, keys: &[(Modifiers, Key)]| {
            run_frame(&ctx, app, keys);
            run_frame(&ctx, app, &[]);
            app.views[0].scene_rect
        };
        view_after(&mut app, &[]);
        let all = view_after(&mut app, &[]);
        assert!(all.left() > 250.0 && all.right() < 1300.0 && all.bottom() < 700.0, "panes on three sides: {all:?}");
        let no_left = view_after(&mut app, &[(ctrl, Key::B)]);
        assert!(no_left.left() < 1.0 && (no_left.bottom() - all.bottom()).abs() < 1.0, "Ctrl+B hides the left side only: {no_left:?}");
        assert!(!dock::has(&app.dock, Pane::Explorer) && dock::has(&app.dock, Pane::Labeling));
        let no_right = view_after(&mut app, &[(ctrl_alt, Key::B)]);
        assert!(no_right.left() < 1.0 && no_right.right() > 1599.0, "Ctrl+Alt+B hides the right side, not the left again: {no_right:?}");
        let alone = view_after(&mut app, &[(ctrl, Key::J)]);
        assert!(alone.bottom() > 850.0 && alone.width() > 1599.0, "Ctrl+J hides the bottom panel: {alone:?}");
        assert_eq!(dock::views(&app.dock).len() + app.hidden.left.iter().chain(&app.hidden.bottom).chain(&app.hidden.right).count(), 4, "three sides are remembered");
        // The same keys bring every side back to where it was.
        view_after(&mut app, &[(ctrl_alt, Key::B)]);
        view_after(&mut app, &[(ctrl, Key::B)]);
        let back = view_after(&mut app, &[(ctrl, Key::J)]);
        assert!((back.min - all.min).length() < 1.5 && (back.max - all.max).length() < 1.5, "{back:?} != {all:?}");
        assert_eq!(app.hidden, dock::Hidden::default());
        assert_eq!(app.active, 0);
        assert_eq!(app.dock.find_active_focused().map(|(_, t)| *t), Some(Pane::View(0)), "the 3D view keeps the keyboard");
    }

    #[test]
    fn a_view_behind_another_tab_has_no_place_on_screen() {
        // A dropped file goes to the view under the mouse: a hidden view must not be found.
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        run_frame(&ctx, &mut app, &[]);
        run_frame(&ctx, &mut app, &[]);
        assert!(app.views[0].scene_rect.is_positive() && app.views[1].scene_rect.is_positive());
        // Drag view 1's tab onto view 0's tab row: view 0 stays the shown tab.
        let loc = app.dock.find_tab(&Pane::View(1)).unwrap();
        app.dock.remove_tab(loc);
        let (node, _) = app.dock.main_surface().find_tab(&Pane::View(0)).unwrap();
        if let egui_dock::Node::Leaf { tabs, .. } = &mut app.dock.main_surface_mut()[node] {
            tabs.push(Pane::View(1));
        }
        run_frame(&ctx, &mut app, &[]);
        run_frame(&ctx, &mut app, &[]);
        assert!(app.views[0].scene_rect.is_positive());
        assert!(!app.views[1].scene_rect.is_positive(), "{:?}", app.views[1].scene_rect);
    }

    /// Rectangle of the place (tab row included) a pane is in, as last drawn.
    fn place_of(app: &TakViewerApp, pane: Pane) -> egui::Rect {
        let (node, _) = app.dock.main_surface().find_tab(&pane).expect("pane is in the main window");
        app.dock.main_surface()[node].rect().expect("a place")
    }

    #[test]
    fn a_view_alone_covers_its_tab_row_and_has_its_own_bar() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        run_frame(&ctx, &mut app, &[]);
        run_frame(&ctx, &mut app, &[]);
        for k in [0, 1] {
            let (place, r) = (place_of(&app, Pane::View(k as u8)), app.views[k].scene_rect);
            assert!((r.top() - place.top()).abs() < 0.5, "view {k}: 3D up to the top of its place (name tag, no tab row): {r:?} in {place:?}");
            assert!((place.bottom() - r.bottom() - panels::DECK_H).abs() < 0.5, "view {k}: its playback bar under it: {r:?} in {place:?}");
        }
        // A pane tabbed together with view 0: that place keeps its tab row.
        let loc = app.dock.find_tab(&Pane::Labeling).unwrap();
        app.dock.remove_tab(loc);
        let (node, _) = app.dock.main_surface().find_tab(&Pane::View(0)).unwrap();
        if let egui_dock::Node::Leaf { tabs, .. } = &mut app.dock.main_surface_mut()[node] {
            tabs.push(Pane::Labeling);
        }
        run_frame(&ctx, &mut app, &[]);
        run_frame(&ctx, &mut app, &[]);
        let (place, r) = (place_of(&app, Pane::View(0)), app.views[0].scene_rect);
        assert!((r.top() - place.top() - dock::TAB_H).abs() < 0.5, "tab row kept: {r:?} in {place:?}");
        let r1 = app.views[1].scene_rect;
        assert!((r1.top() - place_of(&app, Pane::View(1)).top()).abs() < 0.5, "the other view is still alone");
    }

    /// One frame with these input events; returns what was painted.
    fn frame_with(ctx: &egui::Context, app: &mut TakViewerApp, events: Vec<egui::Event>) -> egui::FullOutput {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 960.0));
        ctx.run(egui::RawInput { screen_rect: Some(screen), focused: true, events, ..Default::default() }, |ctx| app.frame(ctx))
    }

    /// Mouse over `at`, press, release, then one more frame (its output is returned).
    fn click_at(ctx: &egui::Context, app: &mut TakViewerApp, at: egui::Pos2) -> egui::FullOutput {
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame_with(ctx, app, vec![egui::Event::PointerMoved(at)]);
        frame_with(ctx, app, vec![button(true)]);
        frame_with(ctx, app, vec![button(false)]);
        frame_with(ctx, app, vec![])
    }

    /// Where a text was painted (every place it appears).
    fn text_rects(out: &egui::FullOutput, text: &str) -> Vec<egui::Rect> {
        fn walk(shape: &egui::Shape, text: &str, found: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Text(t) if t.galley.text() == text => found.push(egui::Rect::from_min_size(t.pos, t.galley.size())),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, text, found)),
                _ => {}
            }
        }
        let mut found = vec![];
        out.shapes.iter().for_each(|s| walk(&s.shape, text, &mut found));
        found
    }

    #[test]
    fn a_views_own_bar_moves_only_that_view_and_its_name_tag_closes_it() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        for (k, name) in [(0, "a.tak"), (1, "b.tak")] {
            let (_tx, rx) = std::sync::mpsc::channel();
            app.views[k].show(views::sample_data(name, &["M"]), rx, 10 + k, 1.0, 0.0);
        }
        app.activate(0);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        // Two views side by side: narrow bars, the rightmost button is ⋯.
        let r = app.views[1].scene_rect;
        assert!(!panels::deck_is_wide(r.width()), "{r:?}");
        let dots = egui::pos2(r.right() - 6.0 - 15.0, r.bottom() + 4.0 + panels::TIMELINE_H + 3.0 + 12.0);
        let out = click_at(&ctx, &mut app, dots);
        assert_eq!(app.active, 1, "pressing a view's bar chooses that view");
        let end = text_rects(&out, "⏭  끝");
        assert_eq!(end.len(), 1, "⋯ opens its menu");
        click_at(&ctx, &mut app, end[0].center());
        assert_eq!(app.views[1].pb.frame, 9, "끝 in view 1's menu goes to its last frame");
        assert_eq!(app.views[0].pb.frame, 0, "view 0 does not move");
        assert!(app.views[1].pb.looping, "playback always loops");

        // The name tag's ✕ (where egui_dock's close button is under it) closes the view.
        let place = place_of(&app, Pane::View(1));
        let out = frame_with(&ctx, &mut app, vec![]);
        let name = text_rects(&out, "b.tak").into_iter().find(|t| place.contains(t.center()) && t.center().y < place.top() + dock::TAB_H);
        let name = name.expect("view 1's name tag is painted at the top of its place");
        let tab_right = 2.0 * name.center().x - place.left() + 24.0;
        click_at(&ctx, &mut app, egui::pos2(tab_right - 12.0, place.top() + dock::TAB_H / 2.0));
        assert_eq!(dock::views(&app.dock), vec![0], "✕ on the name tag closed view 1");
    }

    /// Two views with files a.tak (0) and b.tak (1) side by side, drawn twice.
    fn two_views(ctx: &egui::Context) -> TakViewerApp {
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        for (k, name) in [(0, "a.tak"), (1, "b.tak")] {
            let (_tx, rx) = std::sync::mpsc::channel();
            app.views[k].show(views::sample_data(name, &["M"]), rx, 10 + k, 1.0, 0.0);
        }
        frame_with(ctx, &mut app, vec![]);
        frame_with(ctx, &mut app, vec![]);
        app
    }

    #[test]
    fn ctrl_w_closes_the_focused_view_and_panel_focus_uses_the_last_chosen_view() {
        let ctx = egui::Context::default();
        let mut app = two_views(&ctx);
        app.activate(0);
        // Dock focus can change before the next drawing pass synchronizes `active`.
        dock::focus_view(&mut app.dock, 1);
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        assert_eq!(app.open_views(), vec![0]);
        assert_eq!(app.views[0].path(), Some(Path::new("a.tak")));
        assert!(app.views[1].target().is_none());
        assert_eq!(app.active, 0);

        let ctx = egui::Context::default();
        let mut app = two_views(&ctx);
        let tab = view_tab(&ctx, &app, 1);
        click_at(&ctx, &mut app, tab.left_center() + egui::vec2(20.0, 0.0));
        assert_eq!(app.active, 1, "clicking a 3D tab chooses it");
        let (surface, node, tab) = app.dock.find_tab(&Pane::Explorer).unwrap();
        app.dock.set_active_tab((surface, node, tab));
        app.dock.set_focused_node_and_surface((surface, node));
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        assert_eq!(app.open_views(), vec![0], "panel focus keeps the last chosen 3D view");
        // The final view closes through the same shortcut.
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        assert!(app.views[0].path().is_none());
        assert!(app.open_views().is_empty());
    }

    #[test]
    fn ctrl_w_close_and_undo_restore_the_view_spot_range_and_overlay_as_one_action() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("ctrl_w_undo");
        let dir = _dir_guard.to_path_buf();
        let (mut app, files) = editors_app(&ctx, &dir);
        app.activate(0);
        app.views[0].pb.set_frame(6, 0.0);
        app.views[0].select(1, false);
        app.views[0].range = Some((2, 7));
        app.views[0].shown = [false, true];
        app.views[0].cam.yaw += 0.4;
        let yaw = app.views[0].cam.yaw;
        let data = app.views[0].data.clone().unwrap();
        let document = app.views[0].edit_document.clone().unwrap();
        let file_id = app.views[0].file_id;
        let overlay_data = app.views[0].overlay.as_ref().unwrap().data.clone();
        let overlay_document = app.views[0].overlay_owner().unwrap().edit_document.clone().unwrap();
        frame_with(&ctx, &mut app, vec![]);
        let before = steps(&app, 0).0;
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        assert_eq!(app.open_views(), vec![1]);
        assert_eq!(app.file_count(), 1);
        assert!(app.views[0].target().is_none() && app.views[0].overlay_target().is_none());
        assert_eq!(steps(&app, 0), (before + 1, 0));
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Layout"));
        undo_key(&ctx, &mut app);
        assert_eq!(app.open_views(), vec![0, 1]);
        assert_eq!(app.active, 0);
        assert_eq!(app.views[0].target(), Some(files[0].as_path()));
        assert_eq!(app.views[0].overlay_target(), Some(files[1].as_path()));
        assert!(app.views[0].loading.is_none() && app.views[0].overlay_load.is_none(),
            "undo restores both live files without starting another read");
        assert_eq!(app.views[0].file_id, file_id);
        assert!(std::sync::Arc::ptr_eq(&data, app.views[0].data.as_ref().unwrap()));
        assert!(std::sync::Arc::ptr_eq(&document, app.views[0].edit_document.as_ref().unwrap()));
        assert!(std::sync::Arc::ptr_eq(&overlay_data, &app.views[0].overlay.as_ref().unwrap().data),
            "reopening retains the same overlay without another read");
        assert!(std::sync::Arc::ptr_eq(&overlay_document,
            app.views[0].overlay_owner().unwrap().edit_document.as_ref().unwrap()));
        assert_eq!(app.views[0].pb.frame, 6);
        assert_eq!(app.views[0].range, Some((2, 7)));
        assert_eq!(app.views[0].primary, Some(1));
        assert_eq!(app.views[0].shown, [false, true]);
        assert_eq!(app.views[0].cam.yaw, yaw);
        assert_eq!(steps(&app, 0), (before, 1));
        redo_key(&ctx, &mut app);
        assert_eq!(app.open_views(), vec![1]);
        assert_eq!(app.file_count(), 1);
    }

    #[test]
    fn ctrl_w_respects_exact_modifiers_text_popup_comment_edit_and_held_m() {
        let ctx = egui::Context::default();
        let mut app = two_views(&ctx);
        app.activate(1);
        for modifiers in [Modifiers::NONE, Modifiers::SHIFT, Modifiers::ALT,
            Modifiers { shift: true, ..ctrl() }, Modifiers { alt: true, ..ctrl() },
            Modifiers { shift: true, alt: true, ..ctrl() },
            Modifiers { command: true, mac_cmd: true, ..Modifiers::NONE },
            Modifiers { mac_cmd: true, ..ctrl() }]
        {
            run_frame(&ctx, &mut app, &[(modifiers, Key::W)]);
            assert_eq!(app.open_views(), vec![0, 1], "{modifiers:?}");
        }
        ctx.memory_mut(|m| m.open_popup(egui::Id::new("ctrl_w_popup")));
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        assert_eq!(app.open_views(), vec![0, 1]);
        ctx.memory_mut(|m| m.close_popup());
        key_down(&ctx, &mut app, Key::M);
        assert!(app.hold.is_none(), "TAK cannot start a Comment hold");
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        assert_eq!(app.open_views(), vec![0, 1], "physical M still has priority");
        key_up(&ctx, &mut app, Key::M);
        app.hold = Some(Hold { slot: 1, file_id: app.views[1].file_id, start: 0, markers: vec![], wraps: 0 });
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        assert_eq!(app.open_views(), vec![0, 1]);
        assert!(app.hold.is_some());
        app.hold = None;
        let comment = Comment { id: 1, markers: vec![], start: 0, end: 1, comment_type: String::new(), text: String::new() };
        app.views[1].comments.list.apply(&Event::of(EventKind::Add, &comment, "t".into(), "b.tak|0".into()));
        app.comment_edit = Some(comment_list::Edit::new(1, app.views[1].file_id, &comment, comment_list::Field::Markers, app.views[1].selection()));
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        assert_eq!(app.open_views(), vec![0, 1]);
        app.comment_edit = None;
        app.open_path_dialog();
        frame_with(&ctx, &mut app, vec![]);
        assert!(ctx.wants_keyboard_input());
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        assert_eq!(app.open_views(), vec![0, 1]);
        assert!(app.path_input.is_some());
    }

    #[test]
    fn ctrl_w_with_no_3d_view_does_nothing_and_can_close_a_loading_view() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.dock = DockState::new(vec![Pane::Explorer]);
        let _ = ctx.run(egui::RawInput { events: vec![key_event(Key::W, true, ctrl())], ..Default::default() }, |ctx| app.handle_keys(ctx, 0.0));
        assert!(app.open_views().is_empty());
        let ctx = egui::Context::default();
        let mut app = two_views(&ctx);
        app.hold_loads = true;
        app.activate(1);
        app.open_in(&ctx, 1, "loading.tak".into());
        // No marker data is needed to close a tab whose file is still loading.
        drop_later(app.views[1].data.take());
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        assert_eq!(app.open_views(), vec![0]);
        assert!(app.views[1].loading.is_none());
    }

    #[test]
    fn ctrl_w_pending_or_failed_comments_use_the_existing_close_dialog() {
        for pending in [true, false] {
            let ctx = egui::Context::default();
            let mut app = two_views(&ctx);
            app.activate(1);
            if pending { app.views[1].comments.pending = 1; }
            else { app.views[1].comments.failed = Some("write failed".into()); }
            let before = steps(&app, 0);
            run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
            assert!(app.dirty_prompt.is_some(), "pending={pending}");
            assert_eq!(app.open_views(), vec![0, 1]);
            assert_eq!(app.views[1].path(), Some(Path::new("b.tak")));
            assert_eq!(steps(&app, 0), before, "a blocked close is not an action");
        }
    }

    /// Press at `from`, move in steps to `to`, rest there, let go (the release frame is the
    /// last one run).
    fn drag(ctx: &egui::Context, app: &mut TakViewerApp, from: egui::Pos2, to: egui::Pos2) {
        drag_with(ctx, app, from, to, egui::PointerButton::Primary);
    }

    /// `drag` with another mouse button.
    fn drag_with(ctx: &egui::Context, app: &mut TakViewerApp, from: egui::Pos2, to: egui::Pos2, button: egui::PointerButton) {
        let button = |pos, pressed| egui::Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE };
        frame_with(ctx, app, vec![egui::Event::PointerMoved(from)]);
        frame_with(ctx, app, vec![button(from, true)]);
        for i in 1..=12 {
            frame_with(ctx, app, vec![egui::Event::PointerMoved(from.lerp(to, i as f32 / 12.0))]);
        }
        for _ in 0..3 {
            frame_with(ctx, app, vec![egui::Event::PointerMoved(to)]);
        }
        frame_with(ctx, app, vec![button(to, false)]);
    }

    /// Where the timeline of view `k`'s playback bar is: x of frame `f` (frames 0–9 of the
    /// sample file), and the middle heights of the seek zone and of the band strip.
    struct Bar {
        left: f32,
        width: f32,
        seek_y: f32,
        band_y: f32,
    }

    impl Bar {
        fn of(app: &TakViewerApp, k: usize) -> Bar {
            // The bar is right under the 3D, as wide; frames run 6 + 6 pt in from its sides.
            let r = app.views[k].scene_rect;
            let top = r.bottom() + 4.0;
            Bar { left: r.left() + 12.0, width: r.width() - 24.0, seek_y: top + panels::SEEK_H / 2.0, band_y: top + panels::SEEK_H + panels::BAND_H / 2.0 }
        }
        fn seek(&self, f: f32) -> egui::Pos2 {
            egui::pos2(self.left + f / 9.0 * self.width, self.seek_y)
        }
        fn band(&self, f: f32) -> egui::Pos2 {
            egui::pos2(self.left + f / 9.0 * self.width, self.band_y)
        }
    }

    /// One view showing a.tak (frames 0–9), drawn twice.
    fn one_view(ctx: &egui::Context) -> TakViewerApp {
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_data("a.tak", &["M", "N"]), rx, 10, 1.0, 0.0);
        frame_with(ctx, &mut app, vec![]);
        frame_with(ctx, &mut app, vec![]);
        app
    }

    #[test]
    fn a_drag_in_the_band_strip_picks_a_range_and_leaves_the_frame() {
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        app.views[0].pb.set_frame(5, 0.0);
        let bar = Bar::of(&app, 0);
        drag(&ctx, &mut app, bar.band(2.0), bar.band(7.0));
        assert_eq!(app.views[0].range, Some((2, 7)), "from the frame under the press to the frame under the pointer");
        assert_eq!(app.views[0].pb.frame, 5, "the band strip does not move the frame");
        // Backwards: kept in ascending order (a new drag replaces the range).
        drag(&ctx, &mut app, bar.band(8.0), bar.band(3.0));
        assert_eq!(app.views[0].range, Some((3, 8)));
        // Past the ends: clamped to the file's first / last frame.
        drag(&ctx, &mut app, bar.band(6.0), egui::pos2(bar.left + bar.width + 40.0, bar.band_y));
        assert_eq!(app.views[0].range, Some((6, 9)));
        drag(&ctx, &mut app, bar.band(4.0), egui::pos2(bar.left - 30.0, bar.band_y));
        assert_eq!(app.views[0].range, Some((0, 4)));
        assert_eq!(app.views[0].pb.frame, 5);
        // A plain click in the band strip clears the range; the frame still stays.
        click_at(&ctx, &mut app, bar.band(6.0));
        assert_eq!(app.views[0].range, None);
        assert_eq!(app.views[0].pb.frame, 5);
    }

    #[test]
    fn the_seek_zone_moves_the_frame_and_leaves_the_range() {
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        app.views[0].range = Some((2, 7));
        let bar = Bar::of(&app, 0);
        drag(&ctx, &mut app, bar.seek(1.0), bar.seek(8.0));
        assert_eq!(app.views[0].pb.frame, 8, "a drag in the seek zone moves the frame");
        assert_eq!(app.views[0].range, Some((2, 7)), "and does not change the range");
        click_at(&ctx, &mut app, bar.seek(4.0));
        assert_eq!(app.views[0].pb.frame, 4, "a click there goes to that frame");
        assert_eq!(app.views[0].range, Some((2, 7)));
        // Both ends of the seek zone.
        click_at(&ctx, &mut app, bar.seek(0.0));
        assert_eq!(app.views[0].pb.frame, 0);
        click_at(&ctx, &mut app, bar.seek(9.0));
        assert_eq!(app.views[0].pb.frame, 9);
        assert_eq!(app.views[0].range, Some((2, 7)));
    }

    #[test]
    fn pressing_either_zone_of_a_bar_chooses_that_view_and_moves_only_it() {
        let ctx = egui::Context::default();
        let mut app = two_views(&ctx);
        app.activate(0);
        frame_with(&ctx, &mut app, vec![]);
        let bar = Bar::of(&app, 1);
        click_at(&ctx, &mut app, bar.band(3.0));
        assert_eq!(app.active, 1, "a press in the band strip chooses the view");
        app.activate(0);
        frame_with(&ctx, &mut app, vec![]);
        drag(&ctx, &mut app, bar.band(2.0), bar.band(6.0));
        assert_eq!(app.active, 1);
        assert_eq!((app.views[1].range, app.views[0].range), (Some((2, 6)), None), "the range is that view's own");
        app.activate(0);
        frame_with(&ctx, &mut app, vec![]);
        click_at(&ctx, &mut app, bar.seek(7.0));
        assert_eq!(app.active, 1, "a press in the seek zone chooses the view");
        assert_eq!((app.views[1].pb.frame, app.views[0].pb.frame), (7, 0));
    }

    #[test]
    fn bracket_keys_set_the_range_ends_at_the_current_frame() {
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        let key = |k| [(Modifiers::NONE, k)];
        let at = |app: &mut TakViewerApp, f: i64, k: Key| {
            app.views[0].pb.set_frame(f, 0.0);
            run_frame(&ctx, app, &key(k));
            app.views[0].range
        };
        // No range yet: `]` (or `[`) makes a one-frame range at the current frame.
        assert_eq!(at(&mut app, 4, Key::CloseBracket), Some((4, 4)));
        assert_eq!(at(&mut app, 7, Key::CloseBracket), Some((4, 7)), "] = end here");
        assert_eq!(at(&mut app, 2, Key::CloseBracket), Some((2, 4)), "an end before the start: swapped");
        assert_eq!(at(&mut app, 3, Key::OpenBracket), Some((3, 4)), "[ = start here");
        assert_eq!(at(&mut app, 8, Key::OpenBracket), Some((4, 8)), "a start after the end: swapped");
        app.views[0].range = None;
        assert_eq!(at(&mut app, 6, Key::OpenBracket), Some((6, 6)));
        assert_eq!(app.views[0].pb.frame, 6, "the keys do not move the frame");
    }

    #[test]
    fn esc_clears_the_range_first_then_the_selection() {
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        app.views[0].select(1, false);
        app.views[0].range = Some((2, 5));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        assert_eq!(app.views[0].range, None, "the first Esc clears the range");
        assert_eq!(app.views[0].primary, Some(1), "and keeps the selection");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        assert_eq!(app.views[0].primary, None, "with no range Esc clears the selection, as before");
        assert!(app.views[0].selected.is_empty());
    }

    include!("app_comment_multi_tests.rs");

    // ───────────── Comments (v0.14.0) ─────────────

    /// A fresh folder under the system temp folder (outside the repository) for comment files.
    fn comment_dir(name: &str) -> tak_core::test_support::TempDir {
        tak_core::test_support::TempDir::new(&format!("tak_viewer_comments_{name}"))
    }

    /// Bytes of a `.takm` made from `tak_core`'s sample TAK (made once per test run).
    fn fixture_takm() -> &'static [u8] {
        static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        BYTES.get_or_init(|| {
            let _temp = tak_core::test_support::TempDir::new("tak_viewer_fixture_takm");
            let d = _temp.to_path_buf();
            std::fs::write(d.join("fixture.tak"), tak_core::sample_tak::sample_tak()).unwrap();
            let made = tak_core::takm::convert(&d.join("fixture.tak"), None).unwrap();
            let bytes = std::fs::read(made.out).unwrap();
            bytes
        })
    }

    /// A `.takm` at `path` (made from the fixture, no Comments) unless one is there.
    fn make_takm(path: &Path) {
        if !path.exists() {
            std::fs::write(path, fixture_takm()).unwrap();
        }
    }

    /// `file_a` of lines written for the fixture `.takm` named `name` (`<name>|<fixed length>`).
    fn takm_label(name: &str) -> String {
        let fixed = u64::from_le_bytes(fixture_takm()[32..40].try_into().unwrap());
        format!("{name}|{fixed}")
    }

    /// The Comments of a `.takm` as the comment file's read gave them (None: no such file).
    fn tail_read(path: &Path) -> std::io::Result<Option<comments::Parsed>> {
        if !path.exists() {
            return Ok(None);
        }
        tak_core::takm::read_comments(path).map(Some).map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// Append events to the tail of the `.takm` at `path` (made from the fixture if needed).
    fn tail_append(path: &Path, events: &[comments::Event]) -> std::io::Result<()> {
        make_takm(path);
        tak_core::takm::append_events(path, events).map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// No Comment was written to the `.takm` at `path` (none there, or a tail without events).
    fn no_events(path: &Path) -> bool {
        tail_read(path).unwrap().is_none_or(|p| p.events.is_empty())
    }

    /// `CommentsLoad::read` of `path`, a `.takm` made from the fixture first if it is missing.
    fn load_c(path: &Path) -> views::CommentsLoad {
        if tak_core::takm::is_takm_path(path) {
            make_takm(path);
        }
        views::CommentsLoad::read(path)
    }

    /// One view showing `<dir>/a.takm` (markers M and N, frames 0–9) with its Comments read
    /// the way a real open does (`CommentsLoad::read`), drawn twice. (A TAK takes no Comments
    /// since `.takm` step ③: the Comment tests run on a `.takm` made from the fixture.)
    fn comment_app(ctx: &egui::Context, dir: &Path, command_run: bool) -> TakViewerApp {
        comment_app_named(ctx, dir, command_run, &["M", "N"])
    }

    /// `comment_app` with these marker names.
    fn comment_app_named(ctx: &egui::Context, dir: &Path, command_run: bool, names: &[&str]) -> TakViewerApp {
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        if command_run {
            app.set_command_run();
        }
        let tak = dir.join("a.takm");
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_data(tak.to_str().unwrap(), names), rx, 10, 1.0, 0.0);
        app.views[0].comments.take_load(load_c(&tak));
        frame_with(ctx, &mut app, vec![]);
        frame_with(ctx, &mut app, vec![]);
        if !command_run && matches!(app.views[0].comments.target, Some(Ok(_))) {
            wait_comment_ids(ctx, &mut app, 0);
        }
        app
    }

    fn wait_comment_ids(ctx: &egui::Context, app: &mut TakViewerApp, slot: usize) {
        // Reserving IDs is asynchronous disk work, independent of the rendering budget.
        let started = Instant::now();
        let deadline = started + Duration::from_secs(60);
        loop {
            if app.views[slot].comments.id_pool.as_ref().is_some_and(|ids| !ids.is_empty()) { return; }
            assert!(app.views[slot].comments.id_error.is_none(), "{:?}", app.views[slot].comments.id_error);
            assert!(Instant::now() < deadline,
                "Comment IDs not ready after {:?}: target={:?}, receiving={}, registration={}, error={:?}",
                started.elapsed(), app.views[slot].comments.target,
                app.views[slot].comments.id_rx.is_some(), app.comment_registration.is_some(), app.error);
            frame_with(ctx, app, vec![]);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Frames until the writer thread has confirmed every write.
    fn until_saved(ctx: &egui::Context, app: &mut TakViewerApp) {
        // These fixtures await disk completion, including contended UNC I/O. Rendering
        // latency is measured separately; a frame count is not an asynchronous deadline.
        let started = Instant::now();
        let deadline = started + Duration::from_secs(60);
        loop {
            frame_with(ctx, app, vec![]);
            if app.views.iter().all(|v| v.comments.pending == 0) {
                return;
            }
            assert!(Instant::now() < deadline,
                "comment writes not confirmed after {:?}: pending={:?}, outstanding={}, queued={}, registration={}, registration_error={:?}, failed={}, error={:?}",
                started.elapsed(), app.views.iter().enumerate().map(|(slot, view)|
                    (slot, view.file_id, view.comments.pending, view.comments.failed.as_deref())).collect::<Vec<_>>(),
                app.comment_outstanding, app.comment_waiting.len(), app.comment_registration.is_some(),
                app.comment_registration_error, app.comment_failed.len(), app.error);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn failed_comments_block_every_close_path_and_retry_in_order() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("failed_retry_close");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        let path = dir.join("a.takm");
        let original = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        app.add_comment(&ctx, 0, vec!["M".into()], 1, 1);
        app.add_comment(&ctx, 0, vec!["N".into()], 2, 2);
        until_saved(&ctx, &mut app);
        assert_eq!(app.comment_failed.len(), 2);
        assert!(app.views[0].comments.failed.is_some());
        app.close_editor(0);
        assert!(app.dirty_prompt.is_some());
        app.dirty_prompt = None;
        app.open_in(&ctx, 0, dir.join("other.tak"));
        assert!(app.dirty_prompt.is_some());
        assert!(app.views[0].loading.is_none());
        app.dirty_prompt = None;
        app.request_exit(&ctx);
        assert!(app.dirty_prompt.is_some());
        assert!(!app.exit_approved);
        app.dirty_prompt = None;
        app.views[0].comments.pending = 1;
        app.close_editor(0);
        assert!(app.dirty_prompt.is_some());
        app.dirty_prompt = None;
        app.views[0].comments.pending = 0;
        std::fs::write(&path, original).unwrap();
        app.close_editor(0);
        app.retry_comments_and_transition(&ctx, 0);
        for _ in 0..2000 {
            frame_with(&ctx, &mut app, vec![]);
            if app.views[0].data.is_none() { break; }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(app.views[0].data.is_none(), "successful retry closes the view");
        assert!(app.comment_failed.is_empty());
        let parsed = tak_core::takm::read_comments(&path).unwrap();
        assert_eq!(parsed.events.iter().map(|e| e.markers.clone()).collect::<Vec<_>>(), vec![vec!["M"], vec!["N"]]);
    }

    #[test]
    fn pending_comments_block_tab_close_replacement_and_exit() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("pending_comment_close");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        app.views[0].comments.pending = 1;
        app.close_view(0);
        assert!(app.dirty_prompt.is_some());
        app.dirty_prompt = None;
        app.open_in(&ctx, 0, dir.join("other.tak"));
        assert!(app.dirty_prompt.is_some());
        assert!(app.views[0].loading.is_none());
        app.dirty_prompt = None;
        app.request_exit(&ctx);
        assert!(app.dirty_prompt.is_some());
        assert!(!app.exit_approved);
        app.dirty_prompt = None;
        app.views[0].comments.pending = 0;
    }

    #[test]
    fn failed_comment_can_be_discarded_after_write_finishes() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("failed_comment_discard");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        let path = dir.join("a.takm");
        let original = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        app.add_comment(&ctx, 0, vec!["M".into()], 1, 1);
        until_saved(&ctx, &mut app);
        assert_eq!(app.comment_failed.len(), 1);
        std::fs::write(&path, original).unwrap();
        app.close_editor(0);
        assert!(app.dirty_prompt.is_some());
        app.discard_pending_transition(&ctx);
        assert!(app.views[0].data.is_none());
        assert!(app.comment_failed.is_empty());
        assert!(tak_core::takm::read_comments(&path).unwrap().events.is_empty());
    }

    #[test]
    fn failed_id_reservation_can_retry_on_create_and_visible_retry() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("id_reservation_retry");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        for use_create in [true, false] {
            app.views[0].comments.id_pool = None;
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(Err("injected reservation failure".into())).unwrap();
            app.views[0].comments.id_rx = Some(rx);
            app.poll_comment_ids(&ctx);
            assert!(app.views[0].comments.id_error.is_some());
            if use_create { app.make_comment(&ctx, 0); }
            else {
                app.show_pane(Pane::Comments);
                let out = frame_with(&ctx, &mut app, vec![]);
                let retry = text_rects(&out, "Retry Comment IDs")[0].center();
                click_at(&ctx, &mut app, retry);
            }
            wait_comment_ids(&ctx, &mut app, 0);
            assert!(app.views[0].comments.id_error.is_none());
        }
        app.make_comment(&ctx, 0);
        until_saved(&ctx, &mut app);
        assert_eq!(tak_core::takm::read_comments(&dir.join("a.takm")).unwrap().comments().len(), 1);
    }

    #[test]
    fn stale_other_window_edit_and_delete_reload_the_latest_comment() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("stale_window_reload");
        let dir = _dir_guard.to_path_buf();
        let mut a = comment_app(&ctx, &dir, false);
        a.add_comment(&ctx, 0, vec!["M".into()], 1, 1);
        until_saved(&ctx, &mut a);
        let id = a.views[0].comments.list.list()[0].id;
        let mut b = comment_app(&ctx, &dir, false);
        let mut c = comment_app(&ctx, &dir, false);
        let mut updated = a.views[0].comments.list.get(id).unwrap().clone();
        updated.text = "A text".into();
        assert!(a.edit_comment(&ctx, 0, updated.clone()));
        until_saved(&ctx, &mut a);
        let mut stale = b.views[0].comments.list.get(id).unwrap().clone();
        stale.markers = vec!["N".into()];
        assert!(b.edit_comment(&ctx, 0, stale));
        until_saved(&ctx, &mut b);
        assert_eq!(b.views[0].comments.list.get(id), Some(&updated));
        assert_eq!(b.notice.as_deref(), Some(format!("Comment {id} changed in another window · reloaded").as_str()));
        assert!(b.comment_failed.is_empty());
        c.delete_comment(&ctx, 0, id);
        until_saved(&ctx, &mut c);
        assert_eq!(c.views[0].comments.list.get(id), Some(&updated));
        assert_eq!(tak_core::takm::read_comments(&dir.join("a.takm")).unwrap().comments().get(id), Some(&updated));
    }

    #[cfg(unix)]
    #[test]
    fn held_lock_shows_stall_and_allows_cancel_then_exit_discard() {
        use sha2::{Digest, Sha256};
        use std::fs::OpenOptions;
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("held_lock_discard");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        app.ensure_comment_writer(&ctx);
        for _ in 0..2000 {
            app.poll_comment_writes(&ctx);
            if app.comment_registration.is_none() { break; }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(app.comment_registration.is_none());
        let path = dir.join("a.takm");
        let canonical = std::fs::canonicalize(&path).unwrap();
        let key = format!("{:x}", Sha256::digest(canonical.as_os_str().as_encoded_bytes()));
        let lock = OpenOptions::new().read(true).write(true).open(tak_core::test_support::coordination_root().join(format!(".tak_viewer_{key}.lock"))).unwrap();
        lock.lock().unwrap();
        app.add_comment(&ctx, 0, vec![], 1, 1);
        assert_eq!(app.views[0].comments.pending, 1);
        app.close_editor(0);
        assert!(app.dirty_prompt.is_some());
        app.retry_comments_and_transition(&ctx, 0);
        if std::env::var_os("TAK_FIX2_REAL_WAIT").is_some() {
            std::thread::sleep(Duration::from_millis(10_100));
        } else {
            app.dirty_prompt.as_mut().unwrap().started = Instant::now() - Duration::from_secs(11);
        }
        frame_with(&ctx, &mut app, vec![]);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert!(!text_rects(&out, "Saving has waited more than 10 seconds.").is_empty());
        assert!(!text_rects(&out, "Discard loses unsaved Comments and marker edits. A write already in progress may still finish.").is_empty());
        let cancel = text_rects(&out, "Cancel")[0].center();
        click_at(&ctx, &mut app, cancel);
        assert!(app.dirty_prompt.is_none());
        app.request_exit(&ctx);
        frame_with(&ctx, &mut app, vec![]);
        let out = frame_with(&ctx, &mut app, vec![]);
        let discard = text_rects(&out, "Discard")[0].center();
        click_at(&ctx, &mut app, discard);
        assert!(app.exit_approved, "exit must not wait for the held lock; prompt={} pending={}", app.dirty_prompt.is_some(), app.views[0].comments.pending);
        drop(lock);
        for _ in 0..2000 {
            app.poll_comment_writes(&ctx);
            if app.comment_outstanding == 0 { break; }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.comment_outstanding, 0);
        assert!(tak_core::takm::read_comments(&path).unwrap().events.is_empty());
    }

    #[test]
    fn legacy_duplicate_ids_are_read_but_not_edited_ambiguously() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("legacy_duplicate");
        let dir = _dir_guard.to_path_buf();
        let path = dir.join("a.takm");
        let a = Comment { id: 1, markers: vec!["M".into()], start: 0, end: 0, comment_type: String::new(), text: String::new() };
        let b = Comment { markers: vec!["N".into()], ..a.clone() };
        tail_append(&path, &[Event::of(EventKind::Add, &a, "t".into(), String::new()), Event::of(EventKind::Add, &b, "t".into(), String::new())]).unwrap();
        let mut app = comment_app(&ctx, &dir, false);
        assert_eq!(app.views[0].comments.list.len(), 2);
        assert!(!app.set_comment_type(&ctx, 0, 1, "Jitter"));
        assert_eq!(app.views[0].comments.list.len(), 2);
        assert!(app.notice.as_deref().is_some_and(|text| text.contains("duplicate Comment IDs")));
    }

    fn file_events(dir: &Path) -> Vec<comments::Event> {
        tail_read(&dir.join("a.takm")).unwrap().expect("comment file").events
    }

    fn ids(app: &TakViewerApp) -> Vec<u64> {
        app.views[0].comments.list.list().iter().map(|c| c.id).collect()
    }

    #[test]
    fn m_saves_the_selected_markers_and_the_range_then_clears_the_range() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("m_key");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        assert!(no_events(&dir.join("a.takm")), "no file before the first comment");
        // (Since v0.15.2 M without a selection makes a comment too: see
        // `comments_without_a_marker_…`.)
        // Markers and a range: one comment with both, the range goes, the selection stays.
        app.views[0].select(0, false);
        app.views[0].select(1, true);
        app.views[0].range = Some((2, 7));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        let c = app.views[0].comments.list.list().to_vec();
        assert_eq!(c.len(), 1, "shown at once");
        assert_eq!((c[0].id, c[0].markers.clone(), c[0].start, c[0].end), (1, vec!["M".to_owned(), "N".to_owned()], 2, 7));
        assert_eq!(app.views[0].range, None);
        assert_eq!(app.views[0].selected, BTreeSet::from([0, 1]));
        until_saved(&ctx, &mut app);
        // No range: the current frame only.
        app.views[0].pb.set_frame(4, 0.0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        until_saved(&ctx, &mut app);
        assert_eq!(app.views[0].comments.list.get(2).map(|c| (c.start, c.end)), Some((4, 4)));
        // The file: two add lines, as documented.
        let ev = file_events(&dir);
        assert_eq!(ev.len(), 2);
        assert_eq!((ev[0].kind, ev[0].id, ev[0].markers.clone(), ev[0].start, ev[0].end), (comments::Kind::Add, 1, vec!["M".to_owned(), "N".to_owned()], 2, 7));
        assert_eq!((ev[1].id, ev[1].start, ev[1].end), (2, 4, 4));
        assert_eq!((ev[0].shown_a.as_str(), ev[0].shown_b.as_str(), ev[0].file_a.as_str(), ev[0].file_b.as_str()), ("1", "", takm_label("a.takm").as_str(), ""));
        assert!(ev[0].comment_type.is_empty() && ev[0].text.is_empty(), "type and text come in step 3");
        let t = ev[0].time.as_bytes();
        assert!(t.len() == 25 && t[10] == b'T' && (t[19] == b'+' || t[19] == b'-') && t[22] == b':', "local time with offset: {}", ev[0].time);
        let (status, _, warn) = app.comment_status(0, ctx.input(|i| i.time)).unwrap();
        assert_eq!((status.as_str(), warn), ("Comments 2 · saved", false));
    }

    #[test]
    fn the_comment_button_makes_a_comment_with_or_without_a_selection() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("button");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        let out = frame_with(&ctx, &mut app, vec![]);
        let button = text_rects(&out, "Comment");
        assert_eq!(button.len(), 1, "a wide bar has the Comment button in its row");
        let r = app.views[0].scene_rect;
        assert!(button[0].left() > r.right() - 150.0 && button[0].top() > r.bottom(), "at the right end of the button row: {:?}", button[0]);
        // No marker selected: the button works (v0.15.2), the hover text says so.
        let tip = app.comment_ready(0).unwrap();
        assert!(tip.contains("지금 프레임 0 + 마커 없이"), "{tip}");
        click_at(&ctx, &mut app, button[0].center());
        app.views[0].select(1, false);
        app.views[0].pb.set_frame(6, 0.0);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.comment_ready(0).unwrap().contains("지금 프레임 6 + 고른 마커 1개"));
        click_at(&ctx, &mut app, button[0].center());
        let made: Vec<(Vec<String>, i64)> = app.views[0].comments.list.list().iter().map(|c| (c.markers.clone(), c.start)).collect();
        assert_eq!(made, vec![(vec![], 0), (vec!["N".to_owned()], 6)]);
        until_saved(&ctx, &mut app);
        assert_eq!(file_events(&dir).len(), 2);
    }

    #[test]
    fn comments_without_a_marker_are_made_shown_by_number_keep_the_selection_on_click_and_undo() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("no_marker");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        let comment = |app: &TakViewerApp, id| app.views[0].comments.list.get(id).map(|c| (c.markers.clone(), c.start, c.end));
        // Short M: the current frame.
        app.views[0].pb.set_frame(3, 0.0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        assert_eq!(comment(&app, 1), Some((vec![], 3, 3)));
        // M held from 2 to 5: no markers captured at key down.
        app.views[0].pb.set_frame(2, 0.0);
        key_down(&ctx, &mut app, Key::M);
        assert!(app.hold.as_ref().is_some_and(|h| h.markers.is_empty()), "a hold starts without markers");
        app.views[0].pb.set_frame(5, 0.0);
        key_up(&ctx, &mut app, Key::M);
        assert_eq!(comment(&app, 2), Some((vec![], 2, 5)));
        // The Comment button with a picked range: that range, which is then cleared.
        app.views[0].range = Some((6, 8));
        let out = frame_with(&ctx, &mut app, vec![]);
        let button = text_rects(&out, "Comment")[0];
        click_at(&ctx, &mut app, button.center());
        assert_eq!((comment(&app, 3), app.views[0].range), (Some((vec![], 6, 8)), None));
        // The file: empty markers cells, read back as no markers.
        file_matches_memory(&ctx, &mut app, &dir);
        let text = tak_core::takm::read_tail_text(&dir.join("a.takm")).unwrap();
        assert!(text.contains(",add,1,,3,3,") && text.contains(",add,2,,2,5,") && text.contains(",add,3,,6,8,"), "{text}");
        assert!(file_events(&dir).iter().all(|e| e.markers.is_empty()));
        // The block shows its number only (block 3 is 6–8, wide enough for any text; block 2
        // shares the band with 1 since step 6, too low for text).
        let bar = Bar::of(&app, 0);
        let out = frame_with(&ctx, &mut app, vec![]);
        let on_block = |t: &egui::Rect| (t.center().y - bar.band_y).abs() < panels::BAND_H && t.center().x > bar.band(6.0).x && t.center().x < bar.band(8.0).x;
        assert_eq!(text_rects(&out, "3").iter().filter(|t| on_block(t)).count(), 1, "block 3 reads \"3\"");
        // A click on block 2 (the upper row) goes to its first frame and keeps the selection:
        // one frame action (v0.15.2 had no undo entry; since v0.16.0 a frame move is undone too).
        app.views[0].select(1, false);
        app.quiet();
        let before = steps(&app, 0);
        click_at(&ctx, &mut app, bar.band(4.5) - egui::vec2(0.0, 2.5));
        assert_eq!((app.views[0].pb.frame, sel(&app), steps(&app, 0)), (2, (vec![1], Some(1)), (before.0 + 1, 0)));
        undo_key(&ctx, &mut app);
        // The direct marker pick cleared the prior Comment selection. Undo the band click
        // restores that marker-only selection together with its frame.
        assert_eq!((app.views[0].pb.frame, note(&app).as_str()), (5, "Undo · Frame 5 · Comment deselected"));
        assert_eq!(app.views[0].picked, None);
        // Undo / redo / right-click delete as for any comment.
        undo_key(&ctx, &mut app);
        assert_eq!((comment(&app, 3), app.views[0].range, note(&app)), (None, Some((6, 8)), "Undo · Comment 3 removed".to_owned()));
        redo_key(&ctx, &mut app);
        assert_eq!((comment(&app, 3), app.views[0].range), (Some((vec![], 6, 8)), None));
        right_click_at(&ctx, &mut app, bar.band(4.5) - egui::vec2(0.0, 2.5));
        assert_eq!(comment(&app, 2), None);
        assert_eq!(note(&app), "Comment 2 deleted · Ctrl+Z");
        undo_key(&ctx, &mut app);
        assert_eq!(comment(&app, 2), Some((vec![], 2, 5)));
        file_matches_memory(&ctx, &mut app, &dir);
    }

    #[test]
    fn a_narrow_bar_has_comment_in_its_more_menu() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("narrow");
        let dir = _dir_guard.to_path_buf();
        let mut app = two_views(&ctx);
        let tak = dir.join("a.takm");
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[1].show(views::sample_data(tak.to_str().unwrap(), &["M"]), rx, 20, 1.0, 0.0);
        app.views[1].comments.take_load(load_c(&tak));
        app.views[1].select(0, false);
        frame_with(&ctx, &mut app, vec![]);
        wait_comment_ids(&ctx, &mut app, 1);
        let r = app.views[1].scene_rect;
        let dots = egui::pos2(r.right() - 6.0 - 15.0, r.bottom() + 4.0 + panels::TIMELINE_H + 3.0 + 12.0);
        let out = click_at(&ctx, &mut app, dots);
        let item = text_rects(&out, "Comment").into_iter().find(|t| t.top() > 0.0).expect("Comment in the ⋯ menu");
        click_at(&ctx, &mut app, item.center());
        assert_eq!(app.views[1].comments.list.len(), 1);
        until_saved(&ctx, &mut app);
        assert_eq!(file_events(&dir).len(), 1);
    }

    #[test]
    fn takes_under_the_repository_input_folder_get_no_comment_file() {
        let ctx = egui::Context::default();
        let _root_guard = comment_dir("input_repo");
        let root = _root_guard.to_path_buf();
        std::fs::create_dir_all(root.join("apps")).unwrap();
        std::fs::create_dir_all(root.join("input").join("raw")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "x").unwrap();
        std::fs::write(root.join("apps").join("AGENTS.md"), "x").unwrap();
        let mut app = comment_app(&ctx, &root.join("input").join("raw"), false);
        app.views[0].select(0, false);
        let why = app.comment_ready(0).unwrap_err();
        assert!(why.contains("input/"), "{why}");
        let out = frame_with(&ctx, &mut app, vec![]);
        let button = text_rects(&out, "Comment");
        click_at(&ctx, &mut app, button[0].center());
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.views[0].comments.list.is_empty(), "neither the button nor M");
        assert!(no_events(&root.join("input").join("raw").join("a.takm")));
        assert!(app.comment_writer.is_none(), "nothing was sent to be written");
    }

    /// `<dir>/a.comments.csv` with comment 1 (N, M; frames 3–6) and comment 2 (M; frame 8),
    /// written the way the app writes.
    fn two_comments_on_disk(dir: &Path) {
        let ev = |id, markers: &[&str], start, end| Event {
            time: "2026-11-13T14:05:09+09:00".into(),
            kind: EventKind::Add,
            id,
            markers: markers.iter().map(|m| m.to_string()).collect(),
            start,
            end,
            comment_type: String::new(),
            text: String::new(),
            shown_a: "1".into(),
            shown_b: String::new(),
            file_a: "a.tak|0".into(),
            file_b: String::new(),
            operation: None,
            expected: None,
        };
        tail_append(&dir.join("a.takm"), &[ev(1, &["N", "GONE", "M"], 3, 6), ev(2, &["M"], 8, 8)]).unwrap();
    }

    #[test]
    fn a_click_on_a_block_goes_there_and_a_drag_from_it_picks_a_range() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("block_click");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let mut app = comment_app(&ctx, &dir, false);
        assert_eq!(ids(&app), vec![1, 2]);
        let bar = Bar::of(&app, 0);
        app.views[0].range = Some((0, 1));
        click_at(&ctx, &mut app, bar.band(4.6));
        let v = &app.views[0];
        assert_eq!(v.pb.frame, 3, "to the comment's first frame");
        assert_eq!((v.selected.clone(), v.primary), (BTreeSet::from([0, 1]), Some(1)), "its markers by name, the missing one skipped, the first found is primary");
        assert_eq!(v.range, Some((0, 1)), "a block click leaves the range");
        // A drag that starts on a block picks a range; the frame stays.
        drag(&ctx, &mut app, bar.band(4.0), bar.band(8.0));
        assert_eq!((app.views[0].range, app.views[0].pb.frame), (Some((4, 8)), 3));
        // A one-frame comment (8) is clickable too; empty band still clears the range.
        app.views[0].pb.set_frame(0, 0.0);
        click_at(&ctx, &mut app, bar.band(8.0));
        assert_eq!((app.views[0].pb.frame, app.views[0].primary), (8, Some(0)));
        click_at(&ctx, &mut app, bar.band(1.0));
        assert_eq!(app.views[0].range, None);
        // The file was only read.
        assert_eq!(file_events(&dir).len(), 2);
    }

    /// Right click at `at` (move there, press, let go, one more frame); returns that frame.
    fn right_click_at(ctx: &egui::Context, app: &mut TakViewerApp, at: egui::Pos2) -> egui::FullOutput {
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed, modifiers: Modifiers::NONE };
        frame_with(ctx, app, vec![egui::Event::PointerMoved(at)]);
        frame_with(ctx, app, vec![button(true)]);
        frame_with(ctx, app, vec![button(false)]);
        frame_with(ctx, app, vec![])
    }

    #[test]
    fn a_right_click_on_a_block_deletes_it_at_once_the_one_in_the_row_under_the_pointer() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("delete");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        // Comment 3 (frames 4–5) overlaps comment 1 (3–6): since step 6 they share the band's
        // height, 1 in the upper row, 3 in the lower one.
        let three = Comment { id: 3, markers: vec!["N".into()], start: 4, end: 5, comment_type: String::new(), text: String::new() };
        tail_append(&dir.join("a.takm"), &[Event::of(EventKind::Add, &three, "t".into(), "a.tak|0".into())]).unwrap();
        let before = std::fs::read(dir.join("a.takm")).unwrap();
        let mut app = comment_app(&ctx, &dir, false);
        assert_eq!(ids(&app), vec![1, 2, 3]);
        let bar = Bar::of(&app, 0);
        let lower = bar.band(4.5) + egui::vec2(0.0, 2.5);
        let out = right_click_at(&ctx, &mut app, lower);
        assert_eq!(ids(&app), vec![1, 2], "no menu: the block under the pointer (3) is gone at once");
        assert!(text_rects(&out, "Delete").is_empty(), "no Delete menu any more");
        assert_eq!(app.views[0].pb.frame, 0, "a right click does not jump");
        assert_eq!(text_rects(&out, "Comment 3 deleted · Ctrl+Z").len(), 1, "a note in the status bar");
        // Comment 1 alone again: the full height, the middle of the band hits it.
        right_click_at(&ctx, &mut app, bar.band(4.5));
        assert_eq!(ids(&app), vec![2]);
        until_saved(&ctx, &mut app);
        let after = std::fs::read(dir.join("a.takm")).unwrap();
        assert_eq!(&after[..before.len()], &before[..], "earlier lines are untouched");
        let ev = file_events(&dir);
        assert_eq!(ev.len(), 5);
        assert_eq!((ev[3].kind, ev[3].id, ev[3].start, ev[3].end), (comments::Kind::Delete, 3, 4, 5), "a delete line with the comment's fields");
        assert_eq!((ev[4].kind, ev[4].id, ev[4].markers.len()), (comments::Kind::Delete, 1, 3));
        // Read again (the file opened later): still only comment 2, and the next id is 4.
        let again = comment_app(&ctx, &dir, false);
        assert_eq!((ids(&again), again.views[0].comments.list.next_id()), (vec![2], 4));
    }

    #[test]
    fn overlapping_blocks_have_rows_and_a_click_or_right_click_hits_the_one_in_its_row() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("band_rows");
        let dir = _dir_guard.to_path_buf();
        // Three comments over frames 3–6: three rows (by start frame), 10/3 pt each.
        comments_on_disk(&dir, &[(1, &["M"], 1, 8), (2, &["N"], 2, 7), (3, &[], 3, 6)]);
        let mut app = comment_app(&ctx, &dir, false);
        let bar = Bar::of(&app, 0);
        let row = |k: f32| bar.band(4.5) + egui::vec2(0.0, -5.0 + 10.0 / 3.0 * (k + 0.5));
        let out = frame_with(&ctx, &mut app, vec![]);
        let blocks = filled(&out, theme::COMMENT).into_iter().filter(|r| (r.center().y - bar.band_y).abs() < panels::BAND_H / 2.0).collect::<Vec<_>>();
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        assert!(blocks.iter().all(|r| (r.height() - 10.0 / 3.0).abs() < 0.01), "{blocks:?}");
        for (k, (id, frame)) in [(1, 1), (2, 2), (3, 3)].into_iter().enumerate() {
            tap(&ctx, &mut app, row(k as f32));
            assert_eq!((app.views[0].picked, app.views[0].pb.frame), (Some(id), frame), "row {k}");
        }
        // The middle row's right click deletes 2; 1 and 3 still overlap: two rows.
        right_click_at(&ctx, &mut app, row(1.0));
        assert_eq!(ids(&app), vec![1, 3]);
        right_click_at(&ctx, &mut app, bar.band(4.5) + egui::vec2(0.0, 2.5));
        assert_eq!(ids(&app), vec![1], "the lower of two rows: 3");
        // Alone: the full height, as before.
        let out = frame_with(&ctx, &mut app, vec![]);
        let blocks = filled(&out, theme::COMMENT).into_iter().filter(|r| (r.center().y - bar.band_y).abs() < panels::BAND_H / 2.0).collect::<Vec<_>>();
        assert_eq!(blocks.iter().map(|r| r.height()).collect::<Vec<_>>(), vec![panels::BAND_H - 2.0]);
        right_click_at(&ctx, &mut app, bar.band(4.5));
        assert!(ids(&app).is_empty());
        until_saved(&ctx, &mut app);
    }

    #[test]
    fn the_marker_graph_shows_the_comments_of_its_marker_and_follows_add_edit_delete_and_undo() {
        use crate::ui::graph::{SPAN_FILL, SPAN_FILL_PICKED};
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("graph_spans");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        let m = app.views[0].data.as_ref().unwrap().markers.iter().position(|x| x.name == "M").unwrap();
        app.views[0].select(m, false);
        // (plain bands, selected bands) painted in the floating graph.
        let bands = |ctx: &egui::Context, app: &mut TakViewerApp| {
            let out = frame_with(ctx, app, vec![]);
            let (plain, picked) = (filled(&out, SPAN_FILL), filled(&out, SPAN_FILL_PICKED));
            assert!(plain.iter().chain(&picked).all(|r| r.width() >= 2.0 - 1e-3), "{plain:?} {picked:?}");
            (plain.len(), picked.len())
        };
        assert_eq!(bands(&ctx, &mut app), (0, 0), "no comments: no bands");
        assert!(app.views[0].graph_rect.is_some(), "the floating graph is up");
        // M on frame 3 with M selected: comment 1, selected, in the graph at once.
        app.views[0].pb.set_frame(3, 0.0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        assert_eq!((ids(&app), app.views[0].picked), (vec![1], Some(1)));
        assert_eq!(bands(&ctx, &mut app), (0, 1), "a one-frame comment, selected");
        app.views[0].pb.set_frame(6, 0.0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        assert_eq!(bands(&ctx, &mut app), (1, 1), "1 plain, 2 selected");
        undo_key(&ctx, &mut app);
        assert_eq!((ids(&app), app.views[0].picked), (vec![1], Some(1)));
        assert_eq!(bands(&ctx, &mut app), (0, 1), "undo took 2 away");
        redo_key(&ctx, &mut app);
        assert_eq!(bands(&ctx, &mut app), (1, 1), "redo brought it back");
        // An edit that takes M off comment 1: gone from M's graph.
        let one = Comment { id: 1, markers: vec!["N".into()], start: 3, end: 3, comment_type: String::new(), text: String::new() };
        app.views[0].comments.list.apply(&Event::of(EventKind::Edit, &one, "t".into(), "a.tak|0".into()));
        assert_eq!(bands(&ctx, &mut app), (0, 1));
        // A comment without markers: not in the graph.
        let none = Comment { id: 9, markers: vec![], start: 0, end: 9, comment_type: String::new(), text: String::new() };
        app.views[0].comments.list.apply(&Event::of(EventKind::Add, &none, "t".into(), "a.tak|0".into()));
        assert_eq!(bands(&ctx, &mut app), (0, 1));
        // Delete 2 with a right click on its block (frame 6, the lower row under 9): no bands.
        let bar = Bar::of(&app, 0);
        right_click_at(&ctx, &mut app, bar.band(6.0) + egui::vec2(0.0, 2.5));
        assert_eq!(ids(&app), vec![1, 9]);
        assert_eq!(bands(&ctx, &mut app), (0, 0));
        until_saved(&ctx, &mut app);
    }

    // ───────────── Comment while watching: M held (v0.15.0) ─────────────

    fn key_event(key: Key, pressed: bool, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }
    }

    /// One frame in which `key` goes down (and stays down).
    fn key_down(ctx: &egui::Context, app: &mut TakViewerApp, key: Key) {
        frame_with(ctx, app, vec![key_event(key, true, Modifiers::NONE)]);
    }

    /// One frame in which `key` goes up.
    fn key_up(ctx: &egui::Context, app: &mut TakViewerApp, key: Key) {
        frame_with(ctx, app, vec![key_event(key, false, Modifiers::NONE)]);
    }

    fn ctrl() -> Modifiers {
        Modifiers { ctrl: true, command: true, ..Modifiers::NONE }
    }

    fn spans(app: &TakViewerApp) -> Vec<(u64, i64, i64)> {
        app.views[0].comments.list.list().iter().map(|c| (c.id, c.start, c.end)).collect()
    }

    /// The painted range box of the band strip (its faint orange fill), if any.
    fn range_box(out: &egui::FullOutput) -> Option<egui::Rect> {
        fn walk(shape: &egui::Shape, found: &mut Option<egui::Rect>) {
            match shape {
                egui::Shape::Rect(r) if r.fill == theme::RANGE.gamma_multiply(0.18) => *found = Some(r.rect),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, found)),
                _ => {}
            }
        }
        let mut found = None;
        out.shapes.iter().for_each(|s| walk(&s.shape, &mut found));
        found
    }

    #[test]
    fn holding_m_while_the_frame_moves_saves_that_range_with_the_markers_selected_at_key_down() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("hold");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        app.views[0].select(0, false);
        app.views[0].select(1, true);
        app.views[0].pb.set_frame(2, 0.0);
        key_down(&ctx, &mut app, Key::M);
        assert_eq!(app.hold.as_ref().map(|h| (h.start, h.markers.clone())), Some((2, vec!["M".to_owned(), "N".to_owned()])));
        assert!(app.views[0].comments.list.is_empty(), "nothing yet while M is down");
        // Anything that moves the frame: arrow keys, Space (M does not touch playback), the seek zone.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Space)]);
        assert!(app.views[0].pb.playing, "Space still plays while M is down");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Space)]);
        assert!(!app.views[0].pb.playing);
        let bar = Bar::of(&app, 0);
        drag(&ctx, &mut app, bar.seek(1.0), bar.seek(7.0));
        assert_eq!(app.views[0].pb.frame, 7);
        // The band shows the held range from the start to the current frame (no range was picked).
        let out = frame_with(&ctx, &mut app, vec![]);
        let shown = range_box(&out).expect("the held range is drawn");
        assert!((shown.left() - bar.band(2.0).x).abs() < 2.0 && (shown.right() - bar.band(7.0).x).abs() < 2.0, "{shown:?}");
        assert_eq!(app.views[0].range, None, "the picked range is not touched");
        // The selection changes while M is down: the markers at key down count.
        app.views[0].select(1, false);
        key_up(&ctx, &mut app, Key::M);
        assert!(app.hold.is_none());
        let c = app.views[0].comments.list.list().to_vec();
        assert_eq!((c.len(), c[0].start, c[0].end, c[0].markers.clone()), (1, 2, 7, vec!["M".to_owned(), "N".to_owned()]));
        assert_eq!(app.views[0].selected, BTreeSet::from([1]), "the selection stays as it is");
        assert!(range_box(&frame_with(&ctx, &mut app, vec![])).is_none(), "no range drawn after the save");
        // Let go before the start: the range is put in order.
        app.views[0].pb.set_frame(6, 0.0);
        key_down(&ctx, &mut app, Key::M);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowDown)]);
        assert_eq!(app.views[0].pb.frame, 0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        key_up(&ctx, &mut app, Key::M);
        assert_eq!(spans(&app), vec![(1, 2, 7), (2, 2, 6)]);
        until_saved(&ctx, &mut app);
        let ev = file_events(&dir);
        assert_eq!(ev.iter().map(|e| (e.kind, e.id, e.start, e.end)).collect::<Vec<_>>(), vec![(comments::Kind::Add, 1, 2, 7), (comments::Kind::Add, 2, 2, 6)]);
        assert_eq!(ev[1].markers, vec!["N".to_owned()]);
    }

    #[test]
    fn auto_repeat_while_m_is_down_neither_starts_nor_ends_anything() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("repeat");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        app.views[0].select(0, false);
        app.views[0].pb.set_frame(2, 0.0);
        key_down(&ctx, &mut app, Key::M);
        for f in 3..=5 {
            app.views[0].pb.set_frame(f, 0.0);
            // The OS repeats the press while the key is held: egui marks it as a repeat.
            key_down(&ctx, &mut app, Key::M);
            assert_eq!(app.hold.as_ref().map(|h| h.start), Some(2), "a repeat does not start again");
            assert!(app.views[0].comments.list.is_empty(), "a repeat does not save");
        }
        key_up(&ctx, &mut app, Key::M);
        assert_eq!(spans(&app), vec![(1, 2, 5)], "one comment");
        // A new real press after the release starts a new one.
        key_down(&ctx, &mut app, Key::M);
        assert!(app.hold.is_some());
        key_up(&ctx, &mut app, Key::M);
        assert_eq!(spans(&app), vec![(1, 2, 5), (2, 5, 5)]);
        until_saved(&ctx, &mut app);
    }

    #[test]
    fn a_short_press_is_the_v0_14_comment_and_a_held_range_beats_a_picked_one() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("short");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        app.views[0].select(0, false);
        // Down and up at the same frame (in two frames): no range = that frame.
        app.views[0].pb.set_frame(4, 0.0);
        key_down(&ctx, &mut app, Key::M);
        key_up(&ctx, &mut app, Key::M);
        assert_eq!(spans(&app), vec![(1, 4, 4)]);
        // With a picked range: that range, and it is cleared.
        app.views[0].range = Some((1, 3));
        key_down(&ctx, &mut app, Key::M);
        key_up(&ctx, &mut app, Key::M);
        assert_eq!(spans(&app)[1], (2, 1, 3));
        assert_eq!(app.views[0].range, None);
        // A picked range, but the frame moved while M was down: the held range wins.
        app.views[0].range = Some((1, 2));
        key_down(&ctx, &mut app, Key::M);
        let out = frame_with(&ctx, &mut app, vec![]);
        let bar = Bar::of(&app, 0);
        let shown = range_box(&out).expect("a range is drawn");
        assert!((shown.center().x - bar.band(4.0).x).abs() < 3.0, "the held one (frame 4), not the picked 1–2: {shown:?}");
        app.views[0].pb.set_frame(8, 0.0);
        key_up(&ctx, &mut app, Key::M);
        assert_eq!(spans(&app)[2], (3, 4, 8));
        assert_eq!(app.views[0].range, None, "the earlier range is dropped");
        until_saved(&ctx, &mut app);
        assert_eq!(file_events(&dir).len(), 3);
    }

    #[test]
    fn a_wrap_past_the_last_frame_closes_the_held_range_there_and_the_later_release_adds_nothing() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("wrap");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        app.views[0].select(0, false);
        app.views[0].pb.set_frame(6, 0.0);
        key_down(&ctx, &mut app, Key::M);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Space)]);
        // Frames of 1/60 s at 100 frames/s: the 10-frame file wraps within a few frames.
        for _ in 0..30 {
            if app.hold.is_none() {
                break;
            }
            frame_with(&ctx, &mut app, vec![]);
        }
        assert!(app.hold.is_none(), "the hold ended at the wrap");
        assert_eq!(spans(&app), vec![(1, 6, 9)], "saved at once: start .. last frame");
        assert!(app.views[0].pb.playing, "playback goes on");
        // Holding on (repeats too) and letting go later add nothing.
        for _ in 0..5 {
            key_down(&ctx, &mut app, Key::M);
        }
        key_up(&ctx, &mut app, Key::M);
        assert_eq!(spans(&app).len(), 1);
        // Space at the last frame starts again from the first: also a wrap.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Space)]);
        app.views[0].pb.set_frame(9, 0.0);
        key_down(&ctx, &mut app, Key::M);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Space)]);
        assert_eq!(spans(&app)[1], (2, 9, 9));
        key_up(&ctx, &mut app, Key::M);
        assert_eq!(spans(&app).len(), 2);
        until_saved(&ctx, &mut app);
        assert_eq!(file_events(&dir).len(), 2);
    }

    #[test]
    fn esc_focus_loss_another_view_or_another_file_cancel_a_hold() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("cancel");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        app.views[0].select(0, false);
        let start = |app: &mut TakViewerApp| {
            app.views[0].pb.set_frame(2, 0.0);
            key_down(&ctx, app, Key::M);
            assert!(app.hold.is_some());
            app.views[0].pb.set_frame(5, 0.0);
        };
        // Esc: no comment; the Esc is used up (the range and the selection stay).
        app.views[0].range = Some((1, 3));
        start(&mut app);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        assert!(app.hold.is_none());
        assert_eq!((app.views[0].range, app.views[0].primary), (Some((1, 3)), Some(0)), "Esc only cancelled");
        key_up(&ctx, &mut app, Key::M);
        assert!(app.views[0].comments.list.is_empty());
        app.views[0].range = None;
        // The window loses focus (its fake key-up comes in the same frame): cancelled.
        start(&mut app);
        frame_with(&ctx, &mut app, vec![egui::Event::WindowFocused(false), key_event(Key::M, false, Modifiers::NONE)]);
        assert!(app.hold.is_none() && app.views[0].comments.list.is_empty());
        start(&mut app);
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 960.0));
        let _ = ctx.run(egui::RawInput { screen_rect: Some(screen), focused: false, ..Default::default() }, |ctx| app.frame(ctx));
        assert!(app.hold.is_none(), "an unfocused window cancels too");
        key_up(&ctx, &mut app, Key::M);
        assert!(app.views[0].comments.list.is_empty());
        // Another view becomes the active one.
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[1].show(views::sample_data("b.tak", &["M"]), rx, 30, 1.0, 0.0);
        app.activate(0);
        frame_with(&ctx, &mut app, vec![]);
        start(&mut app);
        app.activate(1);
        frame_with(&ctx, &mut app, vec![]);
        app.activate(0);
        key_up(&ctx, &mut app, Key::M);
        assert!(app.views[0].comments.list.is_empty() && app.views[1].comments.list.is_empty());
        // Another file in the view (here the same one read again: a new file_id).
        start(&mut app);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_data(dir.join("a.takm").to_str().unwrap(), &["M", "N"]), rx, 31, 1.0, 0.0);
        app.views[0].comments.take_load(load_c(&dir.join("a.takm")));
        key_up(&ctx, &mut app, Key::M);
        assert!(app.hold.is_none() && app.views[0].comments.list.is_empty());
        assert!(app.comment_writer.is_none(), "nothing was ever sent");
        assert!(no_events(&dir.join("a.takm")));
    }

    #[test]
    fn m_does_nothing_in_a_text_field_or_in_a_command_run() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("hold_none");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        // (Since v0.15.2 no marker selected is no reason any more: `comments_without_a_marker_…`.)
        // A text field has the keyboard (path dialog): M is a letter there.
        app.views[0].select(0, false);
        app.open_path_dialog();
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(ctx.wants_keyboard_input(), "the path field has focus");
        key_down(&ctx, &mut app, Key::M);
        assert!(app.hold.is_none());
        key_up(&ctx, &mut app, Key::M);
        assert!(app.views[0].comments.list.is_empty());
        assert!(no_events(&dir.join("a.takm")));
        // A command run: nothing written.
        two_comments_on_disk(&dir);
        let before = std::fs::read(dir.join("a.takm")).unwrap();
        let mut app = comment_app(&ctx, &dir, true);
        app.views[0].select(0, false);
        key_down(&ctx, &mut app, Key::M);
        app.views[0].pb.set_frame(5, 0.0);
        key_up(&ctx, &mut app, Key::M);
        run_frame(&ctx, &mut app, &[(ctrl(), Key::Z)]);
        let at = Bar::of(&app, 0).band(4.6);
        right_click_at(&ctx, &mut app, at);
        assert_eq!(ids(&app), vec![1, 2]);
        assert!(app.comment_writer.is_none());
        assert_eq!(std::fs::read(dir.join("a.takm")).unwrap(), before);
    }

    // ───────────── Undo / redo (v0.15.0) ─────────────

    /// The comments the file folds into equal the ones in memory.
    fn file_matches_memory(ctx: &egui::Context, app: &mut TakViewerApp, dir: &Path) {
        until_saved(ctx, app);
        let p = tail_read(&dir.join("a.takm")).unwrap().unwrap_or_default();
        assert_eq!(p.comments().list(), app.views[0].comments.list.list(), "file and memory agree");
        assert_eq!(p.comments().next_id(), app.views[0].comments.list.next_id());
    }

    fn note(app: &TakViewerApp) -> String {
        app.history.note.as_ref().map(|n| n.0.clone()).unwrap_or_default()
    }

    #[test]
    fn ctrl_z_and_ctrl_y_undo_and_redo_adds_and_deletes_by_appending_lines() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("undo");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        let undo = |app: &mut TakViewerApp| run_frame(&ctx, app, &[(ctrl(), Key::Z)]);
        let redo = |app: &mut TakViewerApp| run_frame(&ctx, app, &[(ctrl(), Key::Y)]);
        let ctrl_shift_z = |app: &mut TakViewerApp| run_frame(&ctx, app, &[(Modifiers { shift: true, ..ctrl() }, Key::Z)]);
        // Nothing to undo / redo yet: nothing happens, no file.
        undo(&mut app);
        redo(&mut app);
        assert!(app.views[0].comments.list.is_empty() && no_events(&dir.join("a.takm")));
        app.views[0].select(1, false);
        app.views[0].pb.set_frame(2, 0.0);
        key_down(&ctx, &mut app, Key::M);
        app.views[0].pb.set_frame(5, 0.0);
        key_up(&ctx, &mut app, Key::M);
        let first = app.views[0].comments.list.get(1).cloned().unwrap();
        // Add → Ctrl+Z: gone, a delete line.
        undo(&mut app);
        assert!(app.views[0].comments.list.is_empty());
        assert_eq!(note(&app), "Undo · Comment 1 removed");
        file_matches_memory(&ctx, &mut app, &dir);
        // Ctrl+Y: back with the same id and fields, an add line.
        redo(&mut app);
        assert_eq!(app.views[0].comments.list.list(), &[first.clone()]);
        assert_eq!(note(&app), "Redo · Comment 1 added");
        file_matches_memory(&ctx, &mut app, &dir);
        undo(&mut app);
        ctrl_shift_z(&mut app);
        assert!(app.views[0].comments.list.is_empty(), "Ctrl+Shift+Z does not redo");
        redo(&mut app);
        assert_eq!(app.views[0].comments.list.list(), &[first.clone()]);
        // Delete → Ctrl+Z restores it.
        app.delete_comment(&ctx, 0, 1);
        assert!(app.views[0].comments.list.is_empty());
        undo(&mut app);
        assert_eq!(app.views[0].comments.list.list(), &[first.clone()]);
        assert_eq!(note(&app), "Undo · Comment 1 added");
        redo(&mut app);
        assert_eq!(note(&app), "Redo · Comment 1 removed");
        undo(&mut app);
        file_matches_memory(&ctx, &mut app, &dir);
        let ev = file_events(&dir);
        let kinds: Vec<&str> = ev.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["add", "delete", "add", "delete", "add", "delete", "add", "delete", "add"]);
        assert!(ev.iter().all(|e| (e.id, e.markers.clone(), e.start, e.end) == (1, vec!["N".to_owned()], 2, 5)), "same id and fields on every line");
        // Several steps: comments 2 and 3, back three steps, forward two.
        for f in [7, 8] {
            app.views[0].pb.set_frame(f, 0.0);
            app.quiet(); // (setup: since v0.16.0 a frame move is an action too)
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        }
        assert_eq!(ids(&app), vec![1, 2, 3]);
        for _ in 0..3 {
            undo(&mut app);
        }
        assert_eq!(ids(&app), Vec::<u64>::new(), "3, 2, then 1 removed");
        redo(&mut app);
        redo(&mut app);
        assert_eq!(ids(&app), vec![1, 2]);
        file_matches_memory(&ctx, &mut app, &dir);
        // A new change after an undo drops the redo part.
        app.views[0].pb.set_frame(0, 0.0);
        app.quiet();
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        assert_eq!(ids(&app), vec![1, 2, 4], "a new id: 3 was seen in the file");
        assert!(app.history.redo.is_empty());
        until_saved(&ctx, &mut app);
        let lines = file_events(&dir).len();
        redo(&mut app);
        assert_eq!(ids(&app), vec![1, 2, 4], "nothing to redo");
        until_saved(&ctx, &mut app);
        assert_eq!(file_events(&dir).len(), lines, "and no line written");
        // Undo everything, then once more: nothing more happens.
        for _ in 0..20 {
            undo(&mut app);
        }
        until_saved(&ctx, &mut app);
        let lines = file_events(&dir).len();
        undo(&mut app);
        until_saved(&ctx, &mut app);
        assert_eq!(file_events(&dir).len(), lines);
        file_matches_memory(&ctx, &mut app, &dir);
        // The status bar shows the note.
        redo(&mut app);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, &note(&app)).len(), 1, "{} painted", note(&app));
        file_matches_memory(&ctx, &mut app, &dir);
    }

    #[test]
    fn one_history_for_the_app_kept_with_a_new_file_and_off_while_m_is_held_or_typing() {
        let ctx = egui::Context::default();
        let (_dir_a_guard, _dir_b_guard) = (comment_dir("undo_a"), comment_dir("undo_b"));
        let (dir_a, dir_b) = (_dir_a_guard.to_path_buf(), _dir_b_guard.to_path_buf());
        let mut app = comment_app(&ctx, &dir_a, false);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        let tak_b = dir_b.join("a.takm");
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[1].show(views::sample_data(tak_b.to_str().unwrap(), &["M"]), rx, 40, 1.0, 0.0);
        app.views[1].comments.take_load(load_c(&tak_b));
        app.quiet();
        frame_with(&ctx, &mut app, vec![]);
        wait_comment_ids(&ctx, &mut app, 1);
        app.quiet();
        for k in [0, 1] {
            app.activate(k);
            app.views[k].select(0, false);
            app.quiet();
            frame_with(&ctx, &mut app, vec![]);
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        }
        assert_eq!((app.views[0].comments.list.len(), app.views[1].comments.list.len()), (1, 1));
        assert_eq!(steps(&app, 0), (2, 0), "two comments, nothing else (choosing a view is no action): {:?}", app.history.kinds());
        // Ctrl+Z with view 0 active undoes the newest action of the app: view 1's comment.
        app.activate(0);
        run_frame(&ctx, &mut app, &[(ctrl(), Key::Z)]);
        assert_eq!((app.views[0].comments.list.len(), app.views[1].comments.list.len()), (1, 0));
        run_frame(&ctx, &mut app, &[(ctrl(), Key::Z)]);
        assert_eq!((app.views[0].comments.list.len(), app.views[1].comments.list.len()), (0, 0));
        // Not while M is held.
        run_frame(&ctx, &mut app, &[(ctrl(), Key::Y)]);
        key_down(&ctx, &mut app, Key::M);
        run_frame(&ctx, &mut app, &[(ctrl(), Key::Z)]);
        assert_eq!(app.views[0].comments.list.len(), 1, "Ctrl+Z ignored while M is down");
        key_up(&ctx, &mut app, Key::M);
        assert_eq!(app.views[0].comments.list.len(), 2, "the short press saved");
        assert_eq!(steps(&app, 0), (2, 0), "the new comment dropped the redo side (view 1's comment)");
        // A text field keeps Ctrl+Z for itself.
        app.open_path_dialog();
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(ctx.wants_keyboard_input());
        run_frame(&ctx, &mut app, &[(ctrl(), Key::Z)]);
        assert_eq!(app.views[0].comments.list.len(), 2);
        app.path_input = None;
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(!ctx.wants_keyboard_input(), "the field is gone");
        until_saved(&ctx, &mut app);
        // Another file shown in view 0 (set up directly): the history stays. Actions on a.tak,
        // now on no view, are dropped when reached and the same key goes on.
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_data(dir_b.join("c.tak").to_str().unwrap(), &["M"]), rx, 41, 1.0, 0.0);
        app.quiet();
        frame_with(&ctx, &mut app, vec![]);
        app.quiet();
        assert_eq!(steps(&app, 0), (2, 0), "a new file does not empty the history");
        run_frame(&ctx, &mut app, &[(ctrl(), Key::Z)]);
        assert_eq!(steps(&app, 0), (0, 0));
        assert_eq!(note(&app), "Undo · 2 skipped");
        assert_eq!(tail_read(&dir_a.join("a.takm")).unwrap().unwrap().comments().len(), 2, "a.tak's comments stay");
    }

    // ───────────── Undo / redo of the range and the selection too (v0.15.1) ─────────────

    fn undo_key(ctx: &egui::Context, app: &mut TakViewerApp) {
        run_frame(ctx, app, &[(ctrl(), Key::Z)]);
    }

    fn redo_key(ctx: &egui::Context, app: &mut TakViewerApp) {
        run_frame(ctx, app, &[(ctrl(), Key::Y)]);
    }

    /// (undo actions, redo actions) of the app (one history since v0.16.0; `_k` is kept from
    /// the per-view v0.15.x tests).
    fn steps(app: &TakViewerApp, _k: usize) -> (usize, usize) {
        (app.history.undo.len(), app.history.redo.len())
    }

    /// A click on a marker row of the Labels pane (what it hands the app), then the end of
    /// that frame (where the undo history records it).
    fn label_click(app: &mut TakViewerApp, marker: usize, additive: bool) {
        app.apply(Some(Action::Select { marker, additive }), 0.0);
        app.observe(true);
    }

    /// (selected markers, primary) of view 0.
    fn sel(app: &TakViewerApp) -> (Vec<usize>, Option<usize>) {
        (app.views[0].selected.iter().copied().collect(), app.views[0].primary)
    }

    /// Where marker `m` of view 0 is drawn now.
    fn marker_on_screen(app: &TakViewerApp, m: usize) -> egui::Pos2 {
        let v = &app.views[0];
        let d = v.data.as_ref().unwrap();
        let p = v.cam.view(v.scene_rect).project(d.markers[m].position(v.pb.frame).unwrap()).unwrap().0;
        assert_eq!(markers_at(app, 0, p), Some(m), "a click there picks marker {m}");
        p
    }

    /// `click_at` with modifier keys held (e.g. Ctrl+click).
    fn click_with(ctx: &egui::Context, app: &mut TakViewerApp, at: egui::Pos2, modifiers: Modifiers) {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 960.0));
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers };
        for events in [vec![egui::Event::PointerMoved(at)], vec![button(true)], vec![button(false)], vec![]] {
            let _ = ctx.run(egui::RawInput { screen_rect: Some(screen), focused: true, modifiers, events, ..Default::default() }, |ctx| app.frame(ctx));
        }
    }

    #[test]
    fn a_band_drag_is_one_undo_step_brackets_are_two_and_clears_come_back() {
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        assert!(app.views[0].comments.target.is_none(), "comment file not read: no comment can be written, the range still undoes");
        let bar = Bar::of(&app, 0);
        let range = |app: &TakViewerApp| app.views[0].range;
        app.views[0].pb.set_frame(5, 0.0);
        app.quiet(); // (setup: since v0.16.0 a frame move is an action too)
        drag(&ctx, &mut app, bar.band(2.0), bar.band(7.0));
        assert_eq!((range(&app), steps(&app, 0)), (Some((2, 7)), (1, 0)), "a whole drag (14 frames) is one step");
        drag(&ctx, &mut app, bar.band(8.0), bar.band(3.0));
        assert_eq!((range(&app), steps(&app, 0)), (Some((3, 8)), (2, 0)));
        undo_key(&ctx, &mut app);
        assert_eq!((range(&app), note(&app).as_str()), (Some((2, 7)), "Undo · Range 2–7"), "back to the range before the drag");
        undo_key(&ctx, &mut app);
        assert_eq!((range(&app), note(&app).as_str()), (None, "Undo · Range cleared"));
        undo_key(&ctx, &mut app);
        assert_eq!((range(&app), steps(&app, 0)), (None, (0, 2)), "nothing more to undo: nothing happens");
        redo_key(&ctx, &mut app);
        assert_eq!((range(&app), note(&app).as_str()), (Some((2, 7)), "Redo · Range 2–7"));
        redo_key(&ctx, &mut app);
        assert_eq!(range(&app), Some((3, 8)));
        assert_eq!(app.views[0].pb.frame, 5, "the frame is not part of it");
        // A plain click in the band clears the range: one step at once, undone.
        click_at(&ctx, &mut app, bar.band(6.0));
        assert_eq!((range(&app), steps(&app, 0)), (None, (3, 0)));
        undo_key(&ctx, &mut app);
        assert_eq!(range(&app), Some((3, 8)));
        // Esc clears it: one step (the redo side goes), undone.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        assert_eq!((range(&app), steps(&app, 0)), (None, (3, 0)));
        // `[` then `]`: two steps.
        app.views[0].pb.set_frame(4, 0.0);
        app.quiet();
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::OpenBracket)]);
        app.views[0].pb.set_frame(6, 0.0);
        app.quiet();
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::CloseBracket)]);
        assert_eq!((range(&app), steps(&app, 0)), (Some((4, 6)), (5, 0)));
        undo_key(&ctx, &mut app);
        assert_eq!((range(&app), note(&app).as_str()), (Some((4, 4)), "Undo · Range 4"));
        undo_key(&ctx, &mut app);
        assert_eq!(range(&app), None);
        undo_key(&ctx, &mut app);
        assert_eq!(range(&app), Some((3, 8)), "the Esc clear undone");
        for _ in 0..3 {
            redo_key(&ctx, &mut app);
        }
        assert_eq!((range(&app), steps(&app, 0)), (Some((4, 6)), (5, 0)));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        undo_key(&ctx, &mut app);
        assert_eq!((range(&app), note(&app).as_str()), (Some((4, 6)), "Undo · Range 4–6"));
        // The note is painted in the status bar.
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, "Undo · Range 4–6").len(), 1);
    }

    #[test]
    fn selection_clicks_ctrl_clicks_boxes_labels_and_clears_are_undone_and_redone() {
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        let empty = egui::pos2(app.views[0].scene_rect.left() + 30.0, app.views[0].scene_rect.center().y);
        assert_eq!(markers_at(&app, 0, empty), None);
        // 3D click, Ctrl+click, Labels click, box, click on empty space: five steps.
        let m = marker_on_screen(&app, 0);
        click_at(&ctx, &mut app, m);
        assert_eq!(sel(&app), (vec![0], Some(0)));
        idle(&ctx, &mut app);
        let n = marker_on_screen(&app, 1);
        click_with(&ctx, &mut app, n, ctrl());
        assert_eq!(sel(&app), (vec![0, 1], Some(1)));
        idle(&ctx, &mut app);
        label_click(&mut app, 0, false);
        assert_eq!(sel(&app), (vec![0], Some(0)));
        let (m, n) = (marker_on_screen(&app, 0), marker_on_screen(&app, 1));
        let (lo, hi) = (m.min(n) - egui::vec2(25.0, 25.0), m.max(n) + egui::vec2(25.0, 25.0));
        drag(&ctx, &mut app, lo, hi);
        assert_eq!(sel(&app).0, vec![0, 1], "the box picked both");
        let boxed = sel(&app);
        idle(&ctx, &mut app);
        click_at(&ctx, &mut app, empty);
        assert_eq!(sel(&app), (vec![], None));
        assert_eq!(steps(&app, 0), (5, 0));
        // Back step by step; every place that shows the selection follows (as after a click).
        undo_key(&ctx, &mut app);
        assert_eq!(sel(&app), boxed);
        assert!(note(&app).starts_with("Undo · Selection ") && note(&app).ends_with(" +1"), "{}", note(&app));
        assert!(app.views[0].graph_rect.is_some(), "the floating graph is back");
        undo_key(&ctx, &mut app);
        assert_eq!((sel(&app), note(&app).as_str()), ((vec![0], Some(0)), "Undo · Selection M"));
        // (Called directly here: the Labels pane takes `scroll_to` when it is drawn.)
        app.undo_redo(&ctx, false);
        assert_eq!((sel(&app), note(&app).as_str()), ((vec![0, 1], Some(1)), "Undo · Selection N +1"));
        assert_eq!(app.views[0].labels.scroll_to, Some(1), "Labels goes to it, as after a click");
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].spot().unwrap().primary.as_deref(), Some("N"), "the remembered spot too");
        undo_key(&ctx, &mut app);
        assert_eq!(sel(&app), (vec![0], Some(0)));
        undo_key(&ctx, &mut app);
        assert_eq!((sel(&app), note(&app).as_str()), ((vec![], None), "Undo · Selection cleared"));
        assert!(app.views[0].graph_rect.is_none(), "no marker, no graph");
        undo_key(&ctx, &mut app);
        assert_eq!((sel(&app), steps(&app, 0)), ((vec![], None), (0, 5)));
        // Forward again to the end.
        for want in [(vec![0], Some(0)), (vec![0, 1], Some(1)), (vec![0], Some(0)), boxed.clone(), (vec![], None)] {
            redo_key(&ctx, &mut app);
            assert_eq!(sel(&app), want);
        }
        assert_eq!(note(&app), "Redo · Selection cleared");
        redo_key(&ctx, &mut app);
        assert_eq!(steps(&app, 0), (5, 0), "nothing more to redo");
    }

    #[test]
    fn select_range_add_and_delete_go_back_and_forward_in_order_by_appending_lines() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("undo_mixed");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        let bar = Bar::of(&app, 0);
        let state = |app: &TakViewerApp| (sel(app).0, app.views[0].range, ids(app));
        label_click(&mut app, 1, false);
        drag(&ctx, &mut app, bar.band(2.0), bar.band(7.0));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        assert_eq!(state(&app), (vec![1], None, vec![1]), "the comment took the range and cleared it");
        right_click_at(&ctx, &mut app, bar.band(4.5));
        assert_eq!(state(&app), (vec![1], None, vec![]));
        assert_eq!(steps(&app, 0), (4, 0), "select, range, add, delete");
        // Back: delete, add (its range comes back), range, selection.
        let back = [
            ((vec![1], None, vec![1]), "Undo · Comment 1 added"),
            ((vec![1], Some((2, 7)), vec![]), "Undo · Comment 1 removed"),
            ((vec![1], None, vec![]), "Undo · Range cleared"),
            ((vec![], None, vec![]), "Undo · Selection cleared"),
        ];
        for (want, words) in back {
            undo_key(&ctx, &mut app);
            assert_eq!((state(&app), note(&app)), (want, words.to_owned()));
        }
        // Forward: selection, range, add (the range is cleared again), delete.
        let forward = [
            ((vec![1], None, vec![]), "Redo · Selection N"),
            ((vec![1], Some((2, 7)), vec![]), "Redo · Range 2–7"),
            ((vec![1], None, vec![1]), "Redo · Comment 1 added"),
            ((vec![1], None, vec![]), "Redo · Comment 1 removed"),
        ];
        for (want, words) in forward {
            redo_key(&ctx, &mut app);
            assert_eq!((state(&app), note(&app)), (want, words.to_owned()));
        }
        // Only comment steps wrote lines, appended in order, all about comment 1 (N, 2–7).
        file_matches_memory(&ctx, &mut app, &dir);
        let ev = file_events(&dir);
        let kinds: Vec<&str> = ev.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["add", "delete", "add", "delete", "add", "delete"]);
        assert!(ev.iter().all(|e| (e.id, e.markers.clone(), e.start, e.end) == (1, vec!["N".to_owned()], 2, 7)));
    }

    #[test]
    fn a_block_click_is_one_action_with_its_frame_move_and_selection() {
        // v0.15.1 left the frame move out; since v0.16.0 every frame move is undone too.
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("undo_block");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let mut app = comment_app(&ctx, &dir, false);
        let bar = Bar::of(&app, 0);
        click_at(&ctx, &mut app, bar.band(4.6));
        assert_eq!((sel(&app), app.views[0].pb.frame, steps(&app, 0)), ((vec![0, 1], Some(1)), 3, (1, 0)));
        undo_key(&ctx, &mut app);
        assert_eq!((sel(&app), app.views[0].pb.frame), ((vec![], None), 0), "the selection and the frame go back");
        redo_key(&ctx, &mut app);
        assert_eq!((sel(&app), app.views[0].pb.frame, note(&app).as_str()), ((vec![0, 1], Some(1)), 3, "Redo · Frame 3 · Selection N +1"));
        // The same block again selects the same markers: no new step.
        click_at(&ctx, &mut app, bar.band(4.6));
        assert_eq!(steps(&app, 0), (1, 0));
        assert_eq!(file_events(&dir).len(), 2, "nothing written");
    }

    #[test]
    fn actions_that_change_nothing_add_no_step_and_keep_the_redo_side() {
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        let bar = Bar::of(&app, 0);
        // Nothing selected, no range: Esc and a plain band click change nothing.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        click_at(&ctx, &mut app, bar.band(3.0));
        assert_eq!(steps(&app, 0), (0, 0));
        label_click(&mut app, 0, false);
        app.views[0].pb.set_frame(2, 0.0);
        app.quiet(); // (setup: since v0.16.0 a frame move is an action too)
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::CloseBracket)]);
        undo_key(&ctx, &mut app);
        assert_eq!(steps(&app, 0), (1, 1));
        // The marker already selected alone, the same marker in Labels, Esc-less nothing: no step.
        // (Idle first: a click soon after the band click would be a double click = focus.)
        idle(&ctx, &mut app);
        let m = marker_on_screen(&app, 0);
        click_at(&ctx, &mut app, m);
        idle(&ctx, &mut app);
        label_click(&mut app, 0, false);
        assert_eq!(steps(&app, 0), (1, 1), "redo kept: {:?}", app.history.kinds());
        // `[` / `]` that give the range it already has, a drag to the same range: no step.
        redo_key(&ctx, &mut app);
        assert_eq!(app.views[0].range, Some((2, 2)));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::CloseBracket)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::OpenBracket)]);
        drag(&ctx, &mut app, bar.band(2.0), bar.band(2.2));
        assert_eq!((app.views[0].range, steps(&app, 0)), (Some((2, 2)), (2, 0)));
        // A real change still clears the redo side.
        undo_key(&ctx, &mut app);
        label_click(&mut app, 1, false);
        assert_eq!(steps(&app, 0), (2, 0));
    }

    #[test]
    fn the_history_keeps_the_newest_500_actions_and_a_new_file_does_not_empty_it() {
        // v0.15.1 emptied a view's history with a new file; since v0.16.0 opening is an action.
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        for k in 0..510 {
            label_click(&mut app, k % 2, false);
        }
        assert_eq!(steps(&app, 0), (undo::MAX_UNDO, 0));
        for _ in 0..undo::MAX_UNDO + 5 {
            app.undo_redo(&ctx, false);
        }
        // The 10 oldest actions were dropped: back to what action 10 started from (marker 1).
        assert_eq!((sel(&app), steps(&app, 0)), ((vec![1], Some(1)), (0, undo::MAX_UNDO)));
        // Another file shown directly in the view (setup): the history stays; redoing actions
        // of a.tak, which no view shows, drops them.
        redo_key(&ctx, &mut app);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_data("b.tak", &["M", "N"]), rx, 11, 1.0, 0.0);
        app.quiet();
        frame_with(&ctx, &mut app, vec![]);
        app.quiet();
        assert_eq!(steps(&app, 0), (1, undo::MAX_UNDO - 1));
        redo_key(&ctx, &mut app);
        assert_eq!((sel(&app), steps(&app, 0)), ((vec![], None), (1, 0)), "all dropped, b.tak untouched");
        assert_eq!(note(&app), format!("Redo · {} skipped", undo::MAX_UNDO - 1));
    }

    #[test]
    fn selection_and_range_undo_work_where_comments_cannot_be_written() {
        let ctx = egui::Context::default();
        let _root_guard = comment_dir("undo_input_repo");
        let root = _root_guard.to_path_buf();
        std::fs::create_dir_all(root.join("apps")).unwrap();
        std::fs::create_dir_all(root.join("input").join("raw")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "x").unwrap();
        std::fs::write(root.join("apps").join("AGENTS.md"), "x").unwrap();
        let mut app = comment_app(&ctx, &root.join("input").join("raw"), false);
        assert!(matches!(app.views[0].comments.target, Some(Err(_))), "the comment file is refused");
        label_click(&mut app, 0, false);
        let bar = Bar::of(&app, 0);
        drag(&ctx, &mut app, bar.band(1.0), bar.band(4.0));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        assert_eq!(steps(&app, 0), (2, 0), "M made no comment and no step");
        undo_key(&ctx, &mut app);
        undo_key(&ctx, &mut app);
        assert_eq!((sel(&app), app.views[0].range), ((vec![], None), None));
        redo_key(&ctx, &mut app);
        redo_key(&ctx, &mut app);
        assert_eq!((sel(&app), app.views[0].range), ((vec![0], Some(0)), Some((1, 4))));
        assert!(app.comment_writer.is_none() && no_events(&root.join("input").join("raw").join("a.takm")), "nothing written");
        // A comment step that cannot be done now is dropped and the next one is taken.
        let file = app.views[0].path().unwrap().to_path_buf();
        app.history.push(vec![undo::Part::Comment { file, change: Change::Deleted(Comment { id: 9, markers: vec![], start: 0, end: 0, comment_type: String::new(), text: String::new() }), cleared: None }]);
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].range, steps(&app, 0)), (None, (1, 1)), "the comment step went, the range step was undone");
        assert!(app.views[0].comments.list.is_empty());
    }

    #[test]
    fn no_undo_while_m_is_held_or_a_text_field_has_the_keys_and_ctrl_shift_z_does_nothing() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("undo_off");
        let dir = _dir_guard.to_path_buf();
        // Marker M has no frames 4–6: Z / Shift+Z jump to that gap.
        let mut take = views::sample_take(dir.join("a.takm").to_str().unwrap(), &["M", "N"]);
        let t = &mut take.trajectories[0];
        let keep: Vec<usize> = (0..t.frames.len()).filter(|&i| !(4..=6).contains(&t.frames[i])).collect();
        t.xyz = keep.iter().map(|&i| t.xyz[i]).collect();
        t.frames = keep.iter().map(|&i| t.frames[i]).collect();
        let mut app = comment_app(&ctx, &dir, false);
        let reserved_ids = app.views[0].comments.id_pool.take();
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(tak_core::view_data::ViewData::build(take), rx, 12, 1.0, 0.0);
        app.views[0].comments.take_load(load_c(&dir.join("a.takm")));
        app.views[0].comments.id_pool = reserved_ids;
        frame_with(&ctx, &mut app, vec![]);
        label_click(&mut app, 0, false);
        // Shift+Z goes to the gap (so a Ctrl+Shift+Z falling through would show); since v0.16.0
        // that jump is an action too (the direct `set_frame`s are setup).
        app.views[0].pb.set_frame(8, 0.0);
        app.quiet();
        run_frame(&ctx, &mut app, &[(Modifiers::SHIFT, Key::Z)]);
        assert_eq!(app.views[0].pb.frame, 6);
        app.views[0].pb.set_frame(8, 0.0);
        app.quiet();
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::CloseBracket)]);
        app.views[0].pb.set_frame(2, 0.0);
        app.quiet();
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::CloseBracket)]);
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].range, steps(&app, 0)), (Some((8, 8)), (3, 1)));
        // Ctrl+Shift+Z: no redo, no undo, no gap jump, no note.
        app.views[0].pb.set_frame(8, 0.0);
        app.quiet();
        app.history.note = None;
        run_frame(&ctx, &mut app, &[(Modifiers { shift: true, ..ctrl() }, Key::Z)]);
        assert_eq!((app.views[0].range, steps(&app, 0), app.views[0].pb.frame, note(&app)), (Some((8, 8)), (3, 1), 8, String::new()));
        // While M is held: Ctrl+Z / Ctrl+Y do nothing.
        key_down(&ctx, &mut app, Key::M);
        assert!(app.hold.is_some());
        undo_key(&ctx, &mut app);
        redo_key(&ctx, &mut app);
        assert_eq!((app.views[0].range, steps(&app, 0)), (Some((8, 8)), (3, 1)));
        key_up(&ctx, &mut app, Key::M);
        assert_eq!((ids(&app), app.views[0].range, steps(&app, 0)), (vec![1], None, (4, 0)), "the short press saved, with the range");
        // A text field has the keys: its own Ctrl+Z.
        app.open_path_dialog();
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(ctx.wants_keyboard_input());
        undo_key(&ctx, &mut app);
        assert_eq!((ids(&app), steps(&app, 0)), (vec![1], (4, 0)));
        app.path_input = None;
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        undo_key(&ctx, &mut app);
        assert_eq!((ids(&app), app.views[0].range), (vec![], Some((8, 8))), "the add undone, its range back");
        until_saved(&ctx, &mut app);
    }


    /// Do `change`, end the frame, then Ctrl+Z (state back, `undo_note`) and Ctrl+Y (state as
    /// after the change). The change must be exactly one new action.
    fn round_trip<T: PartialEq + std::fmt::Debug>(
        ctx: &egui::Context,
        app: &mut TakViewerApp,
        what: &str,
        change: impl FnOnce(&egui::Context, &mut TakViewerApp),
        state: impl Fn(&TakViewerApp) -> T,
        undo_note: &str,
    ) {
        frame_with(ctx, app, vec![]);
        let before = state(app);
        let n = app.history.undo.len();
        change(ctx, app);
        frame_with(ctx, app, vec![]);
        let after = state(app);
        assert_ne!(before, after, "{what}: something changed");
        assert_eq!((app.history.undo.len(), app.history.redo.len()), (n + 1, 0), "{what}: one action {:?}", app.history.kinds());
        undo_key(ctx, app);
        assert_eq!(state(app), before, "{what}: undone");
        assert!(note(app).starts_with(undo_note), "{what}: {} (want {undo_note})", note(app));
        redo_key(ctx, app);
        assert_eq!(state(app), after, "{what}: redone");
        assert!(note(app).starts_with("Redo · "), "{}", note(app));
    }

    #[test]
    fn every_kind_of_action_is_undone_and_redone() {
        let ctx = egui::Context::default();
        let mut app = views_with_plates(&ctx, 1);
        idle(&ctx, &mut app);
        // (Setup moved the camera directly: that was recorded; start counting here.)
        let n0 = steps(&app, 0).0;
        fn v0(app: &TakViewerApp) -> &View {
            &app.views[0]
        }
        round_trip(&ctx, &mut app, "frame", |c, a| run_frame(c, a, &[(Modifiers::NONE, Key::ArrowRight)]), |a| v0(a).pb.frame, "Undo · Frame 0");
        let mid = app.views[0].scene_rect.center();
        round_trip(
            &ctx,
            &mut app,
            "camera (right drag = orbit)",
            |c, a| drag_with(c, a, mid, mid + egui::vec2(80.0, 30.0), egui::PointerButton::Secondary),
            |a| v0(a).cam,
            "Undo · Camera",
        );
        round_trip(&ctx, &mut app, "selection", |_, a| label_click(a, 1, false), |a| sel(a), "Undo · Selection M");
        round_trip(&ctx, &mut app, "range", |c, a| run_frame(c, a, &[(Modifiers::NONE, Key::OpenBracket)]), |a| v0(a).range, "Undo · Range cleared");
        idle(&ctx, &mut app);
        let p = plate_middle(&app, 0, 0);
        round_trip(&ctx, &mut app, "plate", |c, a| { click_at(c, a, p); }, |a| v0(a).plate, "Undo · Plate none");
        round_trip(&ctx, &mut app, "speed", |_, a| a.views[0].pb.speed = 2.0, |a| v0(a).pb.speed, "Undo · Speed 1x");
        round_trip(&ctx, &mut app, "Labeling folds", |_, a| {
            // (A group's fold arrow: open ↔ folded.)
            let c = &mut a.views[0].labels.collapsed;
            if !c.remove(&0) {
                c.insert(0);
            }
        }, |a| v0(a).labels.collapsed.clone().into_iter().collect::<Vec<_>>(), "Undo · Labeling");
        round_trip(&ctx, &mut app, "device channel", |_, a| { a.views[0].devices.channel.insert(0, 2); }, |a| v0(a).devices.channel.get(&0).copied(), "Undo · Device channel");
        round_trip(&ctx, &mut app, "display option", |_, a| a.settings.scene.grid = !a.settings.scene.grid, |a| a.settings.scene.grid, "Undo · Display");
        let opacity = app.settings.graph_opacity;
        round_trip(&ctx, &mut app, "graph opacity", |_, a| a.settings.graph_opacity = 40, |a| a.settings.graph_opacity, &format!("Undo · Opacity {opacity}%"));
        round_trip(&ctx, &mut app, "layout (Ctrl+B)", |c, a| run_frame(c, a, &[(ctrl(), Key::B)]), |a| (dock::shape(&a.dock), a.hidden.clone()), "Undo · Layout");
        let _dir_guard = comment_dir("undo_folder");
        let dir = _dir_guard.to_path_buf();
        round_trip(&ctx, &mut app, "saved folder", |_, a| { a.open_workspace(dir.clone()); }, |a| a.workspace.roots().to_vec(), "Undo · ");
        assert!(note(&app).ends_with("Folders"), "{}", note(&app));
        // Each was one action of its own: these 12 and nothing else.
        assert_eq!(steps(&app, 0), (n0 + 12, 0), "{:?}", app.history.kinds());
    }

    #[test]
    fn moves_in_a_row_are_one_action_arrows_a_seek_drag_and_a_play_run() {
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        app.quiet();
        let frame = |app: &TakViewerApp| app.views[0].pb.frame;
        // 30 arrow presses (frames 0–9: right 9, left 7, right 14): one action.
        for (key, n) in [(Key::ArrowRight, 9), (Key::ArrowLeft, 7), (Key::ArrowRight, 14)] {
            for _ in 0..n {
                run_frame(&ctx, &mut app, &[(Modifiers::NONE, key)]);
            }
        }
        assert_eq!((frame(&app), steps(&app, 0)), (9, (1, 0)));
        undo_key(&ctx, &mut app);
        assert_eq!((frame(&app), note(&app).as_str()), (0, "Undo · Frame 0"));
        redo_key(&ctx, &mut app);
        assert_eq!((frame(&app), note(&app).as_str()), (9, "Redo · Frame 9"));
        // A seek drag: one action (after a redo nothing merges into the old one).
        let bar = Bar::of(&app, 0);
        drag(&ctx, &mut app, bar.seek(2.0), bar.seek(6.0));
        assert_eq!((frame(&app), steps(&app, 0)), (6, (2, 0)));
        undo_key(&ctx, &mut app);
        assert_eq!(frame(&app), 9);
        // Something else in between, then a play run (Space … Space): one action.
        label_click(&mut app, 1, false);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Space)]);
        assert!(app.views[0].pb.playing);
        for _ in 0..3 {
            frame_with(&ctx, &mut app, vec![]);
        }
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Space)]);
        let stopped = frame(&app);
        assert!(!app.views[0].pb.playing && stopped != 9, "it played: {stopped}");
        assert_eq!(app.history.kinds(), vec!["Frame", "Selection", "Frame"]);
        undo_key(&ctx, &mut app);
        assert_eq!((frame(&app), note(&app).as_str()), (9, "Undo · Frame 9"));
        // Ctrl+Z while playing stops playback; the run so far is the action it undoes.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Space)]);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].pb.playing, frame(&app), steps(&app, 0)), (false, 9, (2, 1)));
        // A move that changes nothing (End at the last frame) adds nothing and keeps the redo side.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::End)]);
        assert_eq!(steps(&app, 0), (2, 1));
        // Moving away and back again (merged) is no action either; the redo side went with the
        // first move, as with any new action.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowLeft)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        assert_eq!((frame(&app), steps(&app, 0)), (9, (2, 0)));
    }

    #[test]
    fn actions_of_two_views_and_the_app_go_back_and_forward_in_the_order_done() {
        let ctx = egui::Context::default();
        let mut app = two_views(&ctx);
        app.quiet();
        app.activate(0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        app.activate(1);
        label_click(&mut app, 0, false);
        app.activate(0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::CloseBracket)]);
        let opacity = app.settings.graph_opacity;
        app.settings.graph_opacity = 30;
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.history.kinds(), vec!["Frame", "Selection", "Range", "Options"]);
        let state = |app: &TakViewerApp| (app.views[0].pb.frame, app.views[1].selected.len(), app.views[0].range, app.settings.graph_opacity);
        let all = state(&app);
        assert_eq!(all, (2, 1, Some((2, 2)), 30));
        let back = [(2, 1, Some((2, 2)), opacity), (2, 1, None, opacity), (2, 0, None, opacity), (0, 0, None, opacity)];
        for want in back {
            undo_key(&ctx, &mut app);
            assert_eq!(state(&app), want, "{}", note(&app));
        }
        assert_eq!(note(&app), "Undo · Frame 0");
        for want in back.iter().rev().skip(1).chain([&all]) {
            redo_key(&ctx, &mut app);
            assert_eq!(&state(&app), want, "{}", note(&app));
        }
        assert_eq!(note(&app), "Redo · Opacity 30%");
    }

    /// The file view `k` is reading arrives (a small sample with its path, markers M and N)
    /// with its comment file read from disk; two frames. Needs `hold_loads`: no reading thread
    /// runs on the stub files (it would fail on them at a random time and close or keep views).
    fn arrive(ctx: &egui::Context, app: &mut TakViewerApp, k: usize) {
        if let Some(path) = app.views[k].waiting_shared_path().map(Path::to_path_buf) {
            app.promote_shared_waiter(ctx, &path);
        }
        // A closed view can now return with its original live data instead of a new read.
        if app.views[k].loading.is_none() && app.views[k].waiting_shared_path().is_none() && app.views[k].data.is_some() {
            // Conversion switches as soon as markers arrive. Its real Comment reader
            // may still be running, so visible data alone does not complete this fixture.
            for _ in 0..2000 {
                frame_with(ctx, app, vec![]);
                if app.views[k].comments_rx.is_none() { break; }
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(app.views[k].comments_rx.is_none(), "the shown file's Comments did not arrive");
            if !app.no_persist && matches!(app.views[k].comments.target, Some(Ok(_))) {
                wait_comment_ids(ctx, app, k);
            }
            return;
        }
        let p = app.views[k].loading.as_ref().expect("a file is being read").0.clone();
        let tx = app.views[k].held.take().expect("a held load (`hold_loads`)");
        tx.send(views::LoadMsg::Markers(Ok((views::sample_data(p.to_str().unwrap(), &["M", "N"]), vec![])))).unwrap();
        let (ctx_tx, comments_rx) = std::sync::mpsc::channel();
        ctx_tx.send(load_c(&p)).unwrap();
        app.views[k].comments_rx = Some(comments_rx);
        frame_with(ctx, app, vec![]);
        frame_with(ctx, app, vec![]);
        if !app.no_persist && matches!(app.views[k].comments.target, Some(Ok(_))) {
            wait_comment_ids(ctx, app, k);
        }
    }

    /// Every view reading a file gets it (`arrive`).
    fn arrive_all(ctx: &egui::Context, app: &mut TakViewerApp) {
        for k in 0..MAX_VIEWS {
            if app.views[k].loading.is_some() {
                arrive(ctx, app, k);
            }
        }
    }

    #[test]
    fn arrive_waits_for_comments_after_the_staged_markers_are_already_visible() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("staged_comments_arrive");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let path = dir.join("a.takm");
        let (_, markers) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_data(path.to_str().unwrap(), &["M", "N"]), markers, 1, 1.0, 0.0);
        let comments = load_c(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        app.views[0].comments_rx = Some(rx);
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            tx.send(comments).unwrap();
        });
        arrive(&ctx, &mut app, 0);
        sender.join().unwrap();
        assert_eq!(ids(&app), vec![1, 2]);
        assert!(app.views[0].comments_rx.is_none());
        drop(app);
    }

    /// Files that exist (`not a take`: the tests hand the view a sample instead, `arrive`). A
    /// `.takm` name gets a real `.takm` made from the fixture (its Comment tail is used).
    fn take_files(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|n| {
                let p = dir.join(n);
                if tak_core::takm::is_takm_path(&p) {
                    make_takm(&p);
                } else {
                    std::fs::write(&p, b"not a take").unwrap();
                }
                p
            })
            .collect()
    }

    #[test]
    fn undoing_an_open_shows_the_file_before_again_and_its_actions_come_back_once_it_is_read() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("undo_open");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let f = take_files(&dir, &["a.takm", "b.takm"]);
        let (a, b) = (f[0].clone(), f[1].clone());
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.hold_loads = true;
        frame_with(&ctx, &mut app, vec![]);
        app.open(&ctx, a.clone());
        frame_with(&ctx, &mut app, vec![]);
        arrive(&ctx, &mut app, 0);
        assert_eq!((app.views[0].path(), ids(&app)), (Some(a.as_path()), vec![1, 2]));
        // Actions on a.tak: frame, selection, a comment (written to its file).
        for _ in 0..3 {
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        }
        label_click(&mut app, 0, false);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        until_saved(&ctx, &mut app);
        assert_eq!(ids(&app), vec![1, 2, 3]);
        // b.tak in the same view.
        app.open(&ctx, b.clone());
        frame_with(&ctx, &mut app, vec![]);
        arrive(&ctx, &mut app, 0);
        assert_eq!((app.views[0].path(), ids(&app)), (Some(b.as_path()), vec![]));
        // (Since v0.17.1 a new comment is selected in the same action: Comment+Selection.)
        assert_eq!(app.history.kinds(), vec!["Layout", "Frame", "Selection", "Comment+Selection", "Layout"]);
        // Ctrl+Z: a.tak is read again; while it comes, Ctrl+Z waits and drops nothing.
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].target(), note(&app).as_str()), (Some(a.as_path()), "Undo · Open a.takm"));
        undo_key(&ctx, &mut app);
        assert_eq!((note(&app).as_str(), steps(&app, 0)), ("Wait · loading a.takm", (4, 1)));
        arrive(&ctx, &mut app, 0);
        // Back at its remembered spot, its comments read again from the file (same ids).
        assert_eq!((app.views[0].pb.frame, sel(&app), ids(&app)), (3, (vec![0], Some(0)), vec![1, 2, 3]));
        assert_eq!(steps(&app, 0), (4, 1), "showing it again is no action");
        undo_key(&ctx, &mut app);
        assert_eq!((ids(&app), note(&app).as_str()), (vec![1, 2], "Undo · Comment 3 removed"));
        until_saved(&ctx, &mut app);
        assert!(app.comment_failed.is_empty(), "the undo event must be saved before closing the file");
        undo_key(&ctx, &mut app);
        assert_eq!(sel(&app), (vec![], None));
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].pb.frame, note(&app).as_str()), (0, "Undo · Frame 0"));
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].target(), note(&app).as_str()), (None, "Undo · Close a.takm"));
        // And all forward again.
        redo_key(&ctx, &mut app);
        assert_eq!(note(&app), "Redo · Open a.takm");
        redo_key(&ctx, &mut app);
        assert_eq!(note(&app), "Wait · loading a.takm");
        arrive(&ctx, &mut app, 0);
        for _ in 0..3 {
            redo_key(&ctx, &mut app);
        }
        assert_eq!((app.views[0].pb.frame, sel(&app), ids(&app)), (3, (vec![0], Some(0)), vec![1, 2, 3]));
        until_saved(&ctx, &mut app);
        assert!(app.comment_failed.is_empty(), "the redo event must be saved before switching files");
        redo_key(&ctx, &mut app);
        arrive(&ctx, &mut app, 0);
        assert_eq!((app.views[0].path(), steps(&app, 0)), (Some(b.as_path()), (5, 0)));
        until_saved(&ctx, &mut app);
        let kinds: Vec<String> = file_events(&dir).iter().map(|e| format!("{} {}", e.kind.as_str(), e.id)).collect();
        assert_eq!(kinds, vec!["add 1", "add 2", "add 3", "delete 3", "add 3"]);
    }

    #[test]
    fn an_action_on_a_file_that_is_gone_is_dropped_and_the_same_key_goes_on() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("undo_gone");
        let dir = _dir_guard.to_path_buf();
        let f = take_files(&dir, &["a.tak", "b.tak"]);
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.hold_loads = true;
        frame_with(&ctx, &mut app, vec![]);
        app.open(&ctx, f[0].clone());
        frame_with(&ctx, &mut app, vec![]);
        arrive(&ctx, &mut app, 0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        app.open(&ctx, f[1].clone());
        frame_with(&ctx, &mut app, vec![]);
        arrive(&ctx, &mut app, 0);
        assert_eq!(app.history.kinds(), vec!["Layout", "Frame", "Layout"]);
        std::fs::remove_file(&f[0]).unwrap();
        // Showing a.tak again cannot be done, nor its frame move: both dropped, and the same
        // Ctrl+Z takes the next one (the first open: the view had no file).
        undo_key(&ctx, &mut app);
        assert_eq!((note(&app).as_str(), steps(&app, 0)), ("Undo · Close b.tak · 2 skipped", (0, 1)));
        assert_eq!(app.views[0].target(), None);
    }

    #[test]
    fn undo_and_redo_keep_four_views_at_most_and_share_repeated_files() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("undo_views");
        let dir = _dir_guard.to_path_buf();
        let f = take_files(&dir, &["a.tak", "b.tak", "c.tak", "d.tak", "e.tak"]);
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.hold_loads = true;
        frame_with(&ctx, &mut app, vec![]);
        let check = |app: &TakViewerApp| {
            let open = app.open_views();
            let files: Vec<&Path> = open.iter().filter_map(|&k| app.views[k].target()).collect();
            assert!(open.len() <= MAX_VIEWS, "{open:?} {files:?}");
            for &a in &open {
                for &b in &open {
                    if a < b && app.views[a].path().is_some() && app.views[a].path() == app.views[b].path() {
                        assert!(app.views[a].shares_document(&app.views[b]));
                    }
                }
            }
        };
        for p in &f[..4] {
            app.open_side(&ctx, p.clone());
            frame_with(&ctx, &mut app, vec![]);
            arrive_all(&ctx, &mut app);
        }
        assert_eq!(app.open_views(), vec![0, 1, 2, 3]);
        // Close d's view, show d in view 0 instead of a, then e in a new view.
        app.close_view(3);
        frame_with(&ctx, &mut app, vec![]);
        app.activate(0);
        app.open(&ctx, f[3].clone());
        frame_with(&ctx, &mut app, vec![]);
        arrive_all(&ctx, &mut app);
        app.open_side(&ctx, f[4].clone());
        frame_with(&ctx, &mut app, vec![]);
        arrive_all(&ctx, &mut app);
        let end: Vec<Option<PathBuf>> = (0..MAX_VIEWS).map(|k| app.views[k].target().map(Path::to_path_buf)).collect();
        assert_eq!(end, vec![Some(f[3].clone()), Some(f[1].clone()), Some(f[2].clone()), Some(f[4].clone())]);
        let n = steps(&app, 0).0;
        assert_eq!(n, 7, "{:?}", app.history.kinds());
        for _ in 0..n {
            undo_key(&ctx, &mut app);
            check(&app);
            arrive_all(&ctx, &mut app);
            check(&app);
        }
        assert_eq!((app.open_views(), app.views[0].target(), steps(&app, 0)), (vec![0], None, (0, n)));
        for _ in 0..n {
            redo_key(&ctx, &mut app);
            check(&app);
            arrive_all(&ctx, &mut app);
            check(&app);
        }
        let again: Vec<Option<PathBuf>> = (0..MAX_VIEWS).map(|k| app.views[k].target().map(Path::to_path_buf)).collect();
        assert_eq!((again, steps(&app, 0)), (end, (n, 0)));
    }

    #[test]
    fn command_runs_record_no_action() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("undo_command_run");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, true);
        for _ in 0..3 {
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        }
        label_click(&mut app, 1, false);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::OpenBracket)]);
        app.settings.scene.grid = !app.settings.scene.grid;
        run_frame(&ctx, &mut app, &[(ctrl(), Key::B)]);
        assert_eq!(steps(&app, 0), (0, 0));
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].pb.frame, sel(&app).0, app.views[0].range), (3, vec![1], Some((3, 3))), "nothing undone");
    }

    #[test]
    fn the_settings_after_undo_equal_what_putting_things_back_by_hand_saves() {
        let ctx = egui::Context::default();
        let saved = |app: &mut TakViewerApp| {
            app.sync_settings();
            let mut settings = app.settings.clone();
            let layout = dock::shape(settings.layout.as_ref().unwrap());
            settings.layout = None;
            (ron::to_string(&settings).unwrap(), layout)
        };
        let mut by_hand = one_view(&ctx);
        let mut undone = one_view(&ctx);
        let grid = by_hand.settings.scene.grid;
        let opacity = by_hand.settings.graph_opacity;
        let start = saved(&mut by_hand);
        for app in [&mut by_hand, &mut undone] {
            app.quiet();
            run_frame(&ctx, app, &[(ctrl(), Key::B)]);
            app.settings.scene.grid = !grid;
            frame_with(&ctx, app, vec![]);
            app.settings.graph_opacity = 40;
            frame_with(&ctx, app, vec![]);
            run_frame(&ctx, app, &[(Modifiers::NONE, Key::ArrowRight)]);
        }
        assert_ne!(saved(&mut by_hand), start);
        run_frame(&ctx, &mut by_hand, &[(Modifiers::NONE, Key::ArrowLeft)]);
        by_hand.settings.graph_opacity = opacity;
        frame_with(&ctx, &mut by_hand, vec![]);
        by_hand.settings.scene.grid = grid;
        frame_with(&ctx, &mut by_hand, vec![]);
        run_frame(&ctx, &mut by_hand, &[(ctrl(), Key::B)]);
        for _ in 0..4 {
            undo_key(&ctx, &mut undone);
        }
        frame_with(&ctx, &mut by_hand, vec![]);
        frame_with(&ctx, &mut undone, vec![]);
        assert_eq!(saved(&mut undone), saved(&mut by_hand));
        assert_eq!(saved(&mut undone), start);
    }

    // ───────────── COMMENTS pane (v0.17.0, Comment plan step 4; v0.17.1 select · edit) ─────────────

    /// `comment_app` with the COMMENTS pane opened (in front) and drawn; setup is no action.
    fn comments_tab_app(ctx: &egui::Context, dir: &Path, command_run: bool) -> TakViewerApp {
        tab_app_named(ctx, dir, command_run, &["M", "N"])
    }

    #[test]
    fn quick_label_defers_real_scene_clicks_to_comment_marker_edit_and_held_m() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("quick_comment_click");
        let dir = _dir_guard.to_path_buf();
        let mut app = comments_tab_app(&ctx, &dir, false);
        app.add_comment(&ctx, 0, vec!["M".into()], 0, 0);
        until_saved(&ctx, &mut app);
        app.start_comment_edit(1, Field::Markers);
        app.views[0].armed_label = Some(tak_core::edit::LabelKey { parent: [7; 16], slot: 1 });
        assert!(!app.quick_label_available(&ctx, 0));
        let point = marker_on_screen(&app, 1);
        click_at(&ctx, &mut app, point);
        assert_eq!(app.views[0].primary, Some(1));
        assert!(app.comment_edit.is_some());
        assert!(!app.marker_job_running(0));
        app.cancel_comment_edit();
        key_down(&ctx, &mut app, Key::M);
        assert!(app.hold.is_some());
        assert!(!app.quick_label_available(&ctx, 0));
        let point = marker_on_screen(&app, 0);
        click_at(&ctx, &mut app, point);
        assert_eq!(app.views[0].primary, Some(0));
        assert!(!app.marker_job_running(0));
        key_up(&ctx, &mut app, Key::M);
        until_saved(&ctx, &mut app);
    }

    /// `comments_tab_app` with these marker names.
    fn tab_app_named(ctx: &egui::Context, dir: &Path, command_run: bool, names: &[&str]) -> TakViewerApp {
        let mut app = comment_app_named(ctx, dir, command_run, names);
        app.show_pane(Pane::Comments);
        app.quiet();
        frame_with(ctx, &mut app, vec![]);
        frame_with(ctx, &mut app, vec![]);
        app.quiet();
        app
    }

    /// A click of its own (not the second of a double click): idle frames, then `click_at`.
    fn tap(ctx: &egui::Context, app: &mut TakViewerApp, at: egui::Pos2) {
        idle(ctx, app);
        click_at(ctx, app, at);
    }

    /// A point in view 0's 3D with no marker and no floating graph under it.
    fn empty_3d(app: &TakViewerApp) -> egui::Pos2 {
        let v = &app.views[0];
        let r = v.scene_rect.shrink(30.0);
        for i in 1..20 {
            for j in 1..20 {
                let p = egui::pos2(r.left() + r.width() * i as f32 / 20.0, r.top() + r.height() * j as f32 / 20.0);
                let covered = v.graph_rect.is_some_and(|g| g.expand(8.0).contains(p)) || v.plate_rect.is_some_and(|g| g.expand(8.0).contains(p));
                if !covered && markers_at(app, 0, p).is_none() && j > 3 {
                    return p;
                }
            }
        }
        panic!("no empty place in the 3D");
    }

    /// `<dir>/a.comments.csv` with these comments (id, markers, start, end), as the app writes.
    fn comments_on_disk(dir: &Path, list: &[(u64, &[&str], i64, i64)]) {
        let ev: Vec<Event> = list
            .iter()
            .map(|&(id, markers, start, end)| {
                let c = Comment { id, markers: markers.iter().map(|m| m.to_string()).collect(), start, end, comment_type: String::new(), text: String::new() };
                Event::of(EventKind::Add, &c, "2026-11-13T14:05:09+09:00".into(), "a.tak|0".into())
            })
            .collect();
        tail_append(&dir.join("a.takm"), &ev).unwrap();
    }

    /// The COMMENTS row of comment `id` as drawn now (the first one with that id).
    fn row_of(ctx: &egui::Context, app: &mut TakViewerApp, id: u64) -> egui::Rect {
        frame_with(ctx, app, vec![]);
        app.comment_rows.iter().find(|r| r.0 == id).unwrap_or_else(|| panic!("row {id} shown")).1
    }

    /// Points of the COMMENTS row of comment `id`: [Markers cell, Frames cell, Comment cell, ✕].
    fn cells_of(ctx: &egui::Context, app: &mut TakViewerApp, id: u64) -> [egui::Pos2; 4] {
        comment_list::cell_points(row_of(ctx, app, id))
    }

    /// Comment ids in the order the rows are shown.
    fn shown_ids(ctx: &egui::Context, app: &mut TakViewerApp) -> Vec<u64> {
        frame_with(ctx, app, vec![]);
        let mut rows = app.comment_rows.clone();
        rows.sort_by(|a, b| a.1.top().total_cmp(&b.1.top()));
        rows.into_iter().map(|r| r.0).collect()
    }

    fn header(ctx: &egui::Context, app: &mut TakViewerApp, name: &str) -> egui::Pos2 {
        let out = frame_with(ctx, app, vec![]);
        let place = place_of(app, Pane::Comments);
        text_rects(&out, name).into_iter().find(|t| place.contains(t.center())).unwrap_or_else(|| panic!("column {name}")).center()
    }

    /// Two clicks at `at` (the first is a plain click), then one more frame.
    fn double_click(ctx: &egui::Context, app: &mut TakViewerApp, at: egui::Pos2) {
        let b = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        // Long enough after any click that egui counts neither a double nor a triple click.
        idle(ctx, app);
        idle(ctx, app);
        frame_with(ctx, app, vec![egui::Event::PointerMoved(at), b(true), b(false)]);
        frame_with(ctx, app, vec![b(true), b(false)]);
        frame_with(ctx, app, vec![]);
    }

    /// Select all in the box with the keyboard and type `text` over it.
    fn type_over(ctx: &egui::Context, app: &mut TakViewerApp, text: &str) {
        let a = |pressed| egui::Event::Key { key: Key::A, physical_key: None, pressed, repeat: false, modifiers: ctrl() };
        frame_with(ctx, app, vec![a(true), a(false), egui::Event::Text(text.into())]);
        frame_with(ctx, app, vec![]);
    }

    fn picked(app: &TakViewerApp) -> Option<u64> {
        app.views[0].picked
    }

    fn comment(app: &TakViewerApp, id: u64) -> Comment {
        app.views[0].comments.list.get(id).cloned().expect("comment")
    }

    fn line_count(dir: &Path) -> usize {
        file_events(dir).len()
    }

    /// Filled rectangles of this color that were painted.
    fn filled(out: &egui::FullOutput, color: egui::Color32) -> Vec<egui::Rect> {
        fn walk(shape: &egui::Shape, color: egui::Color32, found: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Rect(r) if r.fill == color => found.push(r.rect),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, color, found)),
                _ => {}
            }
        }
        let mut found = vec![];
        out.shapes.iter().for_each(|s| walk(&s.shape, color, &mut found));
        found
    }

    #[test]
    fn the_comments_tab_lists_sorts_goes_to_and_deletes_like_the_band() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_list");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let mut app = comments_tab_app(&ctx, &dir, false);
        assert_eq!(shown_ids(&ctx, &mut app), vec![1, 2], "first: # from the small end");
        // Frames ▲, then ▼: each click one action, undone one by one.
        let h = header(&ctx, &mut app, "Frames");
        tap(&ctx, &mut app, h);
        let h = header(&ctx, &mut app, "Frames ▲");
        tap(&ctx, &mut app, h);
        assert_eq!(app.comment_sort, comment_list::Sort { key: comment_list::SortKey::Frames, desc: true });
        assert_eq!(shown_ids(&ctx, &mut app), vec![2, 1]);
        assert_eq!(app.history.kinds(), vec!["Sort", "Sort"]);
        undo_key(&ctx, &mut app);
        assert_eq!(note(&app), "Undo · Sort Frames ▲");
        undo_key(&ctx, &mut app);
        assert_eq!((app.comment_sort, note(&app).as_str()), (comment_list::Sort::default(), "Undo · Sort # ▲"));
        redo_key(&ctx, &mut app);
        // A single row click (v0.27.3): the comment and its markers selected, the frame stays;
        // one action.
        let [_, row, _, _] = cells_of(&ctx, &mut app, 1);
        tap(&ctx, &mut app, row);
        assert_eq!((app.views[0].pb.frame, sel(&app), picked(&app)), (0, (vec![0, 1], Some(1)), Some(1)));
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Selection"));
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].pb.frame, sel(&app), picked(&app)), (0, (vec![], None), None));
        // A double click on its `#` = the band block's click: the comment selected, first
        // frame + its markers, one action (with the selection its first click made).
        let n = app.history.undo.len();
        let number = comment_list::id_point(row_of(&ctx, &mut app, 1));
        double_click(&ctx, &mut app, number);
        let _band_guard = comment_dir("tab_list_band");
        let mut band = comment_app(&ctx, &_band_guard, false);
        band.views[0].comments.list = app.views[0].comments.list.clone();
        band.quiet();
        let bar = Bar::of(&band, 0);
        tap(&ctx, &mut band, bar.band(4.6));
        assert_eq!((app.views[0].pb.frame, sel(&app), picked(&app)), (band.views[0].pb.frame, sel(&band), picked(&band)), "same as the band");
        assert_eq!((app.views[0].pb.frame, sel(&app), picked(&app)), (3, (vec![0, 1], Some(1)), Some(1)));
        assert_eq!(app.history.undo.len(), n + 1, "one action: {:?}", app.history.kinds());
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Frame+Selection"));
        assert_eq!(band.history.kinds(), vec!["Frame+Selection"]);
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].pb.frame, sel(&app), picked(&app)), (0, (vec![], None), None));
        // ✕ = the band's right click: a delete line, the same note, undone. Deleting the
        // selected comment lets go of it in the same action; Ctrl+Z brings both back.
        let [_, row, _, del] = cells_of(&ctx, &mut app, 2);
        tap(&ctx, &mut app, row);
        assert_eq!(picked(&app), Some(2));
        tap(&ctx, &mut app, del);
        assert_eq!((ids(&app), note(&app).as_str(), picked(&app)), (vec![1], "Comment 2 deleted · Ctrl+Z", None));
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Comment+Selection"));
        undo_key(&ctx, &mut app);
        assert_eq!((ids(&app), picked(&app)), (vec![1, 2], Some(2)));
        until_saved(&ctx, &mut app);
        let kinds: Vec<String> = file_events(&dir).iter().map(|e| format!("{} {}", e.kind.as_str(), e.id)).collect();
        assert_eq!(kinds, vec!["add 1", "add 2", "delete 2", "add 2"]);
    }

    #[test]
    fn a_row_click_selects_without_moving_or_pausing_and_only_a_double_click_on_the_number_goes_there() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_row_dbl");
        let dir = _dir_guard.to_path_buf();
        let mut app = long_tab_app(&ctx, &dir);
        let play = |ctx: &egui::Context, app: &mut TakViewerApp| {
            let now = ctx.input(|i| i.time);
            app.views[0].pb.play(now);
        };
        let state = |app: &TakViewerApp| (picked(app), sel(app));
        // Playing from 1500: a single click on row 2 selects comment 2 and its marker N; the
        // view keeps playing where it was (nowhere near 2500).
        play(&ctx, &mut app);
        let [_, frames2, _, _] = cells_of(&ctx, &mut app, 2);
        tap(&ctx, &mut app, frames2);
        let f = app.views[0].pb.frame;
        assert!(app.views[0].pb.playing && (1500..2400).contains(&f), "still playing: {f}");
        assert_eq!(state(&app), (Some(2), (vec![1], Some(1))));
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Selection"));
        // A double click on Markers / Frames / Comment: the edit, still playing, no move.
        let [m1, f1, t1, _] = cells_of(&ctx, &mut app, 1);
        for (at, field) in [(m1, Field::Markers), (f1, Field::Frames), (t1, Field::Text)] {
            double_click(&ctx, &mut app, at);
            assert_eq!(app.comment_edit.as_ref().map(|e| (e.id, e.field)), Some((1, field)));
            let f = app.views[0].pb.frame;
            assert!(app.views[0].pb.playing && !(1000..=1200).contains(&f), "{field:?}: still playing, not gone there: {f}");
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
            assert!(app.comment_edit.is_none());
        }
        assert_eq!(state(&app), (Some(1), (vec![0], Some(0))), "the first click selected it");
        // A double click on `#` of row 2 while playing: playback stops at its first frame,
        // comment 2 and N selected; with the selection of its first click one action.
        let n = app.history.undo.len();
        let before = (app.history.kinds(), state(&app));
        let number = comment_list::id_point(row_of(&ctx, &mut app, 2));
        double_click(&ctx, &mut app, number);
        assert_eq!((app.views[0].pb.frame, app.views[0].pb.playing), (2500, false));
        assert_eq!(state(&app), (Some(2), (vec![1], Some(1))));
        assert_eq!(app.history.undo.len(), n + 1, "one action: {:?} after {:?}", app.history.kinds(), before.0);
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Frame+Selection"));
        // One Ctrl+Z: the frame from before the playing, comment 1 selected again; Ctrl+Y again.
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].pb.frame, state(&app)), (1500, before.1.clone()));
        assert!(note(&app).starts_with("Undo · Frame 1500"), "{}", note(&app));
        redo_key(&ctx, &mut app);
        assert_eq!((app.views[0].pb.frame, state(&app)), (2500, (Some(2), (vec![1], Some(1)))));
        // Paused: a single click is one selection action; Ctrl+Z puts the selection back only.
        let [_, row1, _, _] = cells_of(&ctx, &mut app, 1);
        tap(&ctx, &mut app, row1);
        assert_eq!((app.views[0].pb.frame, state(&app)), (2500, (Some(1), (vec![0], Some(0)))));
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Selection"));
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].pb.frame, state(&app)), (2500, (Some(2), (vec![1], Some(1)))));
        // The `#` of the comment already selected: only the frame moves, one action.
        app.views[0].pb.set_frame(2700, 0.0);
        app.quiet();
        let n = app.history.undo.len();
        let number = comment_list::id_point(row_of(&ctx, &mut app, 2));
        double_click(&ctx, &mut app, number);
        assert_eq!((app.views[0].pb.frame, app.history.undo.len()), (2500, n + 1));
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Frame"));
        // The band block still goes there with one click (not part of the change).
        let bar = Bar::of(&app, 0);
        let at = egui::pos2(panels::x_of_frame(1100, 0, 2999, bar.left, bar.width), bar.band_y);
        tap(&ctx, &mut app, at);
        assert_eq!((app.views[0].pb.frame, state(&app)), (1000, (Some(1), (vec![0], Some(0)))));
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Frame+Selection"));
        // The line at the top says what `#` does.
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, "Comment 1 선택됨 · # 두 번 = 첫 프레임 · 칸 두 번 = 고치기").len(), 1);
    }

    #[test]
    fn identical_comments_are_selected_one_by_one_and_the_selected_row_stands_out() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_twins");
        let dir = _dir_guard.to_path_buf();
        // Two comments with the same markers and frames (the user's case).
        comments_on_disk(&dir, &[(1, &["M"], 2, 5), (2, &["M"], 2, 5)]);
        let mut app = comments_tab_app(&ctx, &dir, false);
        app.views[0].pb.set_frame(3, 0.0);
        app.quiet();
        for id in [2, 1, 2] {
            let [_, at, _, _] = cells_of(&ctx, &mut app, id);
            tap(&ctx, &mut app, at);
            assert_eq!(picked(&app), Some(id));
            let out = frame_with(&ctx, &mut app, vec![]);
            let (rows, fills) = (app.comment_rows.clone(), filled(&out, theme::PICKED_ROW));
            let marked: Vec<u64> = rows.iter().filter(|r| fills.contains(&r.1)).map(|r| r.0).collect();
            assert_eq!(marked, vec![id], "only the clicked one is marked, though both hold the current frame");
            // The "current frame is inside" mark is the thin bar only (both rows have it).
            assert_eq!(filled(&out, theme::COMMENT).iter().filter(|r| r.width() == 3.0 && rows.iter().any(|row| row.1.left() == r.left())).count(), 2);
            let words = text_rects(&out, &format!("Comment {id} 선택됨 · # 두 번 = 첫 프레임 · 칸 두 번 = 고치기"));
            assert_eq!(words.len(), 1, "the line at the top names it");
        }
        // A click on a band block selects that comment too (the twins share the band since step
        // 6: 1 in the upper row, 2 in the lower one).
        let bar = Bar::of(&app, 0);
        let [_, at, _, _] = cells_of(&ctx, &mut app, 1);
        tap(&ctx, &mut app, at);
        tap(&ctx, &mut app, bar.band(3.5) + egui::vec2(0.0, 2.5));
        assert_eq!(picked(&app), Some(2));
        // M makes a comment and selects it in the same action; Ctrl+Z goes back to 2.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        assert_eq!((ids(&app), picked(&app)), (vec![1, 2, 3], Some(3)));
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Comment+Selection"));
        undo_key(&ctx, &mut app);
        assert_eq!((ids(&app), picked(&app)), (vec![1, 2], Some(2)));
        // Esc: (no range) the marker selection and the selected comment go together.
        app.views[0].range = Some((1, 2));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        assert_eq!((app.views[0].range, picked(&app)), (None, Some(2)), "the range first");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        assert_eq!((sel(&app), picked(&app)), ((vec![], None), None));
        // A new file in the view: nothing selected.
        let [_, at, _, _] = cells_of(&ctx, &mut app, 1);
        tap(&ctx, &mut app, at);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_data(dir.join("b.tak").to_str().unwrap(), &["M"]), rx, 77, 1.0, 0.0);
        assert_eq!(picked(&app), None);
        until_saved(&ctx, &mut app);
    }

    #[test]
    fn a_double_click_on_markers_edits_them_and_enter_saves_one_action() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_markers");
        let dir = _dir_guard.to_path_buf();
        comments_on_disk(&dir, &[(1, &["M", "N", "O"], 2, 5), (2, &["N"], 7, 8)]);
        let mut app = tab_app_named(&ctx, &dir, false, &["M", "N", "O"]);
        let place = place_of(&app, Pane::Comments);
        let shown = |out: &egui::FullOutput, s: &str| text_rects(out, s).iter().any(|t| place.contains(t.center()));
        // The user's way: double click Markers → pick markers → Enter.
        let [markers, ..] = cells_of(&ctx, &mut app, 1);
        double_click(&ctx, &mut app, markers);
        assert_eq!(app.comment_edit.as_ref().map(|e| (e.id, e.field)), Some((1, Field::Markers)));
        assert_eq!((sel(&app), picked(&app)), ((vec![0, 1, 2], Some(0)), Some(1)), "the first click selected it and its markers");
        let before_edit = app.history.kinds();
        assert_eq!(before_edit, vec!["Selection"], "the first click selected; nothing moved (v0.27.3)");
        assert_eq!(app.views[0].pb.frame, 0, "a double click on a cell does not go to the comment");
        let out = frame_with(&ctx, &mut app, vec![]);
        assert!(shown(&out, "Comment 1 마커 고치는 중 · Enter = 저장 · Esc = 취소"), "the line at the top");
        // Labels path: Ctrl+click N away; the cell follows, nothing is written or recorded.
        label_click(&mut app, 1, true);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert!(shown(&out, "M, O"), "the cell shows the selection now");
        // 3D path: a click on empty 3D, then a plain click on one marker; the edit stays on.
        let empty = empty_3d(&app);
        tap(&ctx, &mut app, empty);
        assert!(app.comment_edit.is_some() && sel(&app).0.is_empty(), "a 3D click does not end the edit");
        let o = marker_on_screen(&app, 2);
        tap(&ctx, &mut app, o);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert!(app.comment_edit.is_some() && shown(&out, "O"));
        // A click on the playback bar's seek zone moves the frame and keeps the edit.
        let bar = Bar::of(&app, 0);
        tap(&ctx, &mut app, egui::pos2(bar.band(6.0).x, bar.seek_y));
        assert!(app.comment_edit.is_some());
        assert_eq!(app.history.kinds().len(), before_edit.len() + 1, "only the frame move is an action");
        assert_eq!(line_count(&dir), 2, "nothing written yet");
        // Enter: the comment has O only; one edit line; one action with the selection.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Enter)]);
        assert!(app.comment_edit.is_none());
        assert_eq!(comment(&app, 1).markers, vec!["O".to_owned()]);
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Comment+Selection"));
        file_matches_memory(&ctx, &mut app, &dir);
        let ev = file_events(&dir);
        assert_eq!((ev.len(), ev[2].kind, ev[2].id, ev[2].markers.clone(), (ev[2].start, ev[2].end)), (3, comments::Kind::Edit, 1, vec!["O".to_owned()], (2, 5)));
        // One Ctrl+Z: the comment and the selection as before the edit; Ctrl+Y again.
        undo_key(&ctx, &mut app);
        assert_eq!((comment(&app, 1).markers.len(), sel(&app), picked(&app)), (3, (vec![0, 1, 2], Some(0)), Some(1)));
        redo_key(&ctx, &mut app);
        assert_eq!((comment(&app, 1).markers.clone(), sel(&app).0), (vec!["O".to_owned()], vec![2]));
        file_matches_memory(&ctx, &mut app, &dir);
        // A second edit through the Labels path only: 1 marker → 2 (Labels order: M, O).
        let [markers, ..] = cells_of(&ctx, &mut app, 1);
        double_click(&ctx, &mut app, markers);
        label_click(&mut app, 0, true);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Enter)]);
        assert_eq!(comment(&app, 1).markers, vec!["M".to_owned(), "O".to_owned()]);
        // No change = no line, no action.
        until_saved(&ctx, &mut app);
        let (lines, steps_now) = (line_count(&dir), steps(&app, 0));
        let [markers, ..] = cells_of(&ctx, &mut app, 1);
        double_click(&ctx, &mut app, markers);
        let steps_edit = steps(&app, 0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Enter)]);
        until_saved(&ctx, &mut app);
        assert_eq!((line_count(&dir), steps(&app, 0)), (lines, steps_edit));
        assert!(steps_edit.0 <= steps_now.0 + 1, "at most the first click's selection");
        // Enter with nothing selected: a comment without markers (on purpose).
        let [markers, ..] = cells_of(&ctx, &mut app, 2);
        double_click(&ctx, &mut app, markers);
        tap(&ctx, &mut app, empty);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Enter)]);
        assert!(comment(&app, 2).markers.is_empty());
        file_matches_memory(&ctx, &mut app, &dir);
    }

    #[test]
    fn esc_cancels_an_edit_and_leaves_nothing_to_undo_and_the_selection_never_edits_by_itself() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_esc");
        let dir = _dir_guard.to_path_buf();
        comments_on_disk(&dir, &[(1, &["M", "N", "O"], 2, 5), (2, &["N"], 7, 8)]);
        let mut app = tab_app_named(&ctx, &dir, false, &["M", "N", "O"]);
        // Outside an edit, marker selection changes (Labels, 3D, Esc) never change a comment,
        // also with a comment selected; nor do undo and redo or row clicks.
        let [_, row, _, _] = cells_of(&ctx, &mut app, 1);
        tap(&ctx, &mut app, row);
        label_click(&mut app, 1, true);
        let n = marker_on_screen(&app, 1);
        tap(&ctx, &mut app, n);
        undo_key(&ctx, &mut app);
        redo_key(&ctx, &mut app);
        let [_, row2, _, _] = cells_of(&ctx, &mut app, 2);
        tap(&ctx, &mut app, row2);
        tap(&ctx, &mut app, row);
        until_saved(&ctx, &mut app);
        assert!(no_events(&dir.join("a.takm")) || line_count(&dir) == 2, "no edit line");
        assert_eq!(comment(&app, 1).markers.len(), 3);
        // Esc during a marker edit: the selection from its start comes back, nothing written,
        // nothing in the history; a range stays (the edit takes that Esc).
        app.views[0].range = Some((1, 2));
        app.quiet();
        let [markers, ..] = cells_of(&ctx, &mut app, 1);
        double_click(&ctx, &mut app, markers);
        let (kinds, sel_start) = (app.history.kinds(), sel(&app));
        label_click(&mut app, 0, true);
        label_click(&mut app, 2, true);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        assert!(app.comment_edit.is_none());
        assert_eq!((sel(&app), picked(&app), app.views[0].range), (sel_start, Some(1), Some((1, 2))));
        let mut want = kinds;
        want.push("Frame".into());
        assert_eq!(app.history.kinds(), want, "only the frame key is an action");
        assert_eq!(line_count(&dir), 2);
        // M, Ctrl+Z and Ctrl+Y do nothing during an edit; frame keys still work.
        double_click(&ctx, &mut app, markers);
        let (ids_now, steps_now) = (ids(&app), steps(&app, 0));
        assert!(app.comment_ready(0).is_err(), "the Comment button is greyed out");
        let f = app.views[0].pb.frame;
        for k in [(Modifiers::NONE, Key::M), (ctrl(), Key::Z), (ctrl(), Key::Y)] {
            run_frame(&ctx, &mut app, &[k]);
        }
        assert_eq!((ids(&app), steps(&app, 0), app.hold.is_none()), (ids_now, steps_now, true));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        assert_eq!(app.views[0].pb.frame, f + 1);
        assert!(app.comment_edit.is_some());
        // Comments focus: cancel the edit first, then clear Comment/marker selection while
        // keeping the frame range. Switching to 3D restores the normal range-first Esc.
        for _ in 0..3 {
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        }
        assert_eq!((app.comment_edit.is_none(), app.views[0].range, sel(&app), picked(&app)), (true, Some((1, 2)), (vec![], None), None));
        let empty = empty_3d(&app);
        tap(&ctx, &mut app, empty);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        assert_eq!(app.views[0].range, None);
        until_saved(&ctx, &mut app);
        assert_eq!(line_count(&dir), 2);
    }

    #[test]
    fn another_row_the_tab_going_away_or_a_new_file_cancel_an_edit() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_cancel");
        let dir = _dir_guard.to_path_buf();
        comments_on_disk(&dir, &[(1, &["M", "N", "O"], 2, 5), (2, &["N"], 7, 8)]);
        let mut app = tab_app_named(&ctx, &dir, false, &["M", "N", "O"]);
        let [markers, ..] = cells_of(&ctx, &mut app, 1);
        // Another row: the edit is cancelled (selection back, nothing written), then that row
        // is clicked as usual (one action).
        double_click(&ctx, &mut app, markers);
        label_click(&mut app, 0, true);
        let kinds = app.history.kinds();
        let [_, row2, _, _] = cells_of(&ctx, &mut app, 2);
        tap(&ctx, &mut app, row2);
        assert!(app.comment_edit.is_none());
        assert_eq!((sel(&app), picked(&app), comment(&app, 1).markers.len()), ((vec![1], Some(1)), Some(2), 3));
        let mut want = kinds;
        want.push("Selection".into());
        assert_eq!(app.history.kinds(), want);
        // A click on the edited row itself does nothing.
        let [markers, frames, ..] = cells_of(&ctx, &mut app, 1);
        double_click(&ctx, &mut app, markers);
        label_click(&mut app, 2, true);
        tap(&ctx, &mut app, frames);
        assert_eq!(app.comment_edit.as_ref().map(|e| e.field), Some(Field::Markers));
        // The tab hidden: cancelled.
        app.hide_pane(Pane::Comments);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.comment_edit.is_none());
        assert_eq!(sel(&app), (vec![0, 1, 2], Some(0)), "the selection from the edit's start");
        // Another file in the view: cancelled.
        app.show_pane(Pane::Comments);
        let [markers, ..] = cells_of(&ctx, &mut app, 1);
        double_click(&ctx, &mut app, markers);
        assert!(app.comment_edit.is_some());
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(views::sample_data(dir.join("b.tak").to_str().unwrap(), &["M"]), rx, 78, 1.0, 0.0);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.comment_edit.is_none());
        assert_eq!(line_count(&dir), 2, "nothing written");
    }

    #[test]
    fn typed_frames_are_saved_exactly_clamped_and_swapped_and_the_sort_follows() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_frames");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let mut app = comments_tab_app(&ctx, &dir, false);
        let h = header(&ctx, &mut app, "Frames");
        tap(&ctx, &mut app, h);
        assert_eq!(shown_ids(&ctx, &mut app), vec![1, 2]);
        // Double click Frames: two boxes, the start one has the keys.
        let edit = |app: &mut TakViewerApp, start: &str, end: &str| {
            let [_, frames, _, _] = cells_of(&ctx, app, 1);
            double_click(&ctx, app, frames);
            assert_eq!(app.comment_edit.as_ref().map(|e| e.field), Some(Field::Frames));
            frame_with(&ctx, app, vec![]);
            assert!(ctx.wants_keyboard_input(), "the start box has the keys");
            type_over(&ctx, app, start);
            let [_, end_box, _] = comment_list::box_rects(row_of(&ctx, app, 1));
            tap(&ctx, app, end_box.center());
            assert!(app.comment_edit.is_some(), "going to the other box does not end it");
            type_over(&ctx, app, end);
            run_frame(&ctx, app, &[(Modifiers::NONE, Key::Enter)]);
            assert!(app.comment_edit.is_none() && !ctx.wants_keyboard_input());
            let c = comment(app, 1);
            (c.start, c.end)
        };
        assert_eq!(edit(&mut app, "8", "9"), (8, 9), "exact");
        assert_eq!(shown_ids(&ctx, &mut app), vec![2, 1], "the sort follows at once");
        assert_eq!(edit(&mut app, "7", "1"), (1, 7), "start after end: swapped");
        assert_eq!(edit(&mut app, "-5", "400"), (0, 9), "kept inside the file's frames");
        assert_eq!(edit(&mut app, "abc", "4"), (0, 4), "not a number: the old start stays");
        assert_eq!(app.history.kinds().iter().filter(|k| k.as_str() == "Comment").count(), 4, "one action each");
        // Esc in a box: nothing.
        until_saved(&ctx, &mut app);
        let lines = line_count(&dir);
        let [_, frames, _, _] = cells_of(&ctx, &mut app, 1);
        double_click(&ctx, &mut app, frames);
        frame_with(&ctx, &mut app, vec![]);
        type_over(&ctx, &mut app, "6");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!((app.comment_edit.is_none(), (comment(&app, 1).start, comment(&app, 1).end)), (true, (0, 4)));
        until_saved(&ctx, &mut app);
        assert_eq!(line_count(&dir), lines);
        undo_key(&ctx, &mut app);
        assert_eq!((comment(&app, 1).start, comment(&app, 1).end), (0, 9));
        file_matches_memory(&ctx, &mut app, &dir);
        let ev = file_events(&dir);
        assert!(ev[2..].iter().all(|e| e.kind == comments::Kind::Edit && e.markers == vec!["N", "GONE", "M"]), "the markers as they were");
    }

    // ───────────── dragging a frame number ("hot text", v0.17.3) ─────────────

    /// A long file: `<dir>/a.tak`, markers M and N, frames 0–2999, comment 1 (M; 1000–1200) and
    /// comment 2 (N; 2500–2900) on disk, the COMMENTS pane in front, the view at frame 1500.
    fn long_tab_app(ctx: &egui::Context, dir: &Path) -> TakViewerApp {
        comments_on_disk(dir, &[(1, &["M"], 1000, 1200), (2, &["N"], 2500, 2900)]);
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        let tak = dir.join("a.takm");
        let mut take = views::sample_take(tak.to_str().unwrap(), &["M", "N"]);
        take.end_frame = 2999;
        for (k, t) in take.trajectories.iter_mut().enumerate() {
            t.frames = (0..3000).collect();
            t.xyz = (0..3000).map(|f| [k as f32 * 0.1, 1.0, f as f32 * 0.001]).collect();
        }
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(tak_core::view_data::ViewData::build(take), rx, 10, 1.0, 0.0);
        app.views[0].comments.take_load(load_c(&tak));
        app.show_pane(Pane::Comments);
        for _ in 0..4 {
            frame_with(ctx, &mut app, vec![]);
        }
        app.views[0].pb.set_frame(1500, 0.0);
        app.quiet();
        frame_with(ctx, &mut app, vec![]);
        app.quiet();
        assert_eq!((app.views[0].pb.first, app.views[0].pb.last, app.views[0].pb.frame), (0, 2999, 1500));
        app
    }

    /// One frame with these events and Shift held or not.
    fn frame_shift(ctx: &egui::Context, app: &mut TakViewerApp, events: Vec<egui::Event>, shift: bool) -> egui::FullOutput {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 960.0));
        let modifiers = if shift { Modifiers::SHIFT } else { Modifiers::NONE };
        ctx.run(egui::RawInput { screen_rect: Some(screen), focused: true, modifiers, events, ..Default::default() }, |ctx| app.frame(ctx))
    }

    /// Press at `from` (a press of its own, not part of a double click) and move to `to` in
    /// steps, Shift held or not; the button stays down. The last frame's output.
    fn press_and_move(ctx: &egui::Context, app: &mut TakViewerApp, from: egui::Pos2, to: egui::Pos2, shift: bool) -> egui::FullOutput {
        idle(ctx, app);
        press_and_move_now(ctx, app, from, to, shift)
    }

    /// `press_and_move` without the idle frames before it.
    fn press_and_move_now(ctx: &egui::Context, app: &mut TakViewerApp, from: egui::Pos2, to: egui::Pos2, shift: bool) -> egui::FullOutput {
        let down = egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE };
        frame_shift(ctx, app, vec![egui::Event::PointerMoved(from)], shift);
        frame_shift(ctx, app, vec![down], shift);
        move_to(ctx, app, from, to, shift)
    }

    /// The button still down: move from `from` to `to` in steps. The last frame's output.
    fn move_to(ctx: &egui::Context, app: &mut TakViewerApp, from: egui::Pos2, to: egui::Pos2, shift: bool) -> egui::FullOutput {
        for i in 1..=8 {
            frame_shift(ctx, app, vec![egui::Event::PointerMoved(from.lerp(to, i as f32 / 8.0))], shift);
        }
        frame_shift(ctx, app, vec![], shift)
    }

    /// Let go of the button at `at`, then one more frame.
    fn let_go(ctx: &egui::Context, app: &mut TakViewerApp, at: egui::Pos2) {
        let up = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE };
        frame_with(ctx, app, vec![up]);
        frame_with(ctx, app, vec![]);
    }

    /// The boxes of the Frames edit of comment `id` (start, end).
    fn frame_boxes_of(ctx: &egui::Context, app: &mut TakViewerApp, id: u64) -> (egui::Rect, egui::Rect) {
        let [s, e, _] = comment_list::box_rects(row_of(ctx, app, id));
        (s, e)
    }

    /// The typed boxes of the edit going on (start, end).
    fn boxes_text(app: &TakViewerApp) -> (String, String) {
        let e = app.comment_edit.as_ref().expect("an edit");
        (e.start.clone(), e.end.clone())
    }

    #[test]
    fn a_dragged_frame_box_moves_its_number_and_the_view_and_enter_saves_it_as_one_action() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_scrub");
        let dir = _dir_guard.to_path_buf();
        let mut app = long_tab_app(&ctx, &dir);
        let up = egui::vec2(0.0, -1.0);
        // Double click Frames: the edit; the view stays at 1500 (v0.27.3: only `#` goes there).
        let [_, frames, _, _] = cells_of(&ctx, &mut app, 1);
        double_click(&ctx, &mut app, frames);
        assert_eq!(app.comment_edit.as_ref().map(|e| e.field), Some(Field::Frames));
        assert_eq!(app.views[0].pb.frame, 1500);
        let kinds = app.history.kinds();
        let (s, e) = frame_boxes_of(&ctx, &mut app, 1);
        // Start box 40 pt up: +40, the view is at that frame while the button is still down,
        // and the band block of comment 1 is drawn from 1040.
        let out = press_and_move(&ctx, &mut app, s.center(), s.center() + up * 40.0, false);
        assert_eq!(boxes_text(&app), ("1040".to_owned(), "1200".to_owned()));
        assert_eq!((app.views[0].pb.frame, app.views[0].pb.playing), (1040, false), "the view follows, paused");
        let bar = Bar::of(&app, 0);
        let x = |f: i64| panels::x_of_frame(f, 0, 2999, bar.left, bar.width);
        let block = |out: &egui::FullOutput, a: i64, b: i64| filled(out, theme::COMMENT).iter().any(|r| (r.left() - x(a)).abs() < 0.6 && (r.right() - x(b)).abs() < 0.6);
        assert!(block(&out, 1040, 1200), "the band shows the dragged start: {:?}", filled(&out, theme::COMMENT));
        // Back down past the press: 10 pt below it = −10 from where it started.
        move_to(&ctx, &mut app, s.center() + up * 40.0, s.center() - up * 10.0, false);
        assert_eq!((boxes_text(&app).0.as_str(), app.views[0].pb.frame), ("990", 990));
        let_go(&ctx, &mut app, s.center() - up * 10.0);
        assert!(app.comment_edit.is_some(), "letting go does not end the edit");
        assert_eq!(app.history.kinds(), kinds, "nothing recorded while the edit is on");
        // End box with Shift: 8 pt up = +80; 3 pt right = +30.
        press_and_move(&ctx, &mut app, e.center(), e.center() + up * 8.0 + egui::vec2(3.0, 0.0), true);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(boxes_text(&app), ("990".to_owned(), "1310".to_owned()));
        assert_eq!(app.views[0].pb.frame, 1310, "the view follows the end box too");
        assert!(block(&out, 990, 1310));
        let_go(&ctx, &mut app, e.center() + up * 8.0 + egui::vec2(3.0, 0.0));
        assert_eq!(app.history.kinds(), kinds);
        // A plain click still puts the caret in the box for typing.
        tap(&ctx, &mut app, e.center());
        assert!(ctx.wants_keyboard_input(), "a click without a drag types");
        type_over(&ctx, &mut app, "1305");
        assert_eq!(app.views[0].pb.frame, 1310, "typing does not move the view");
        // Enter: saved exactly, the view stays, one action = the edit + the frame move.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Enter)]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.comment_edit.is_none());
        assert_eq!((comment(&app, 1).start, comment(&app, 1).end, app.views[0].pb.frame), (990, 1305, 1310));
        let mut want = kinds.clone();
        want.push("Comment+Frame".into());
        assert_eq!(app.history.kinds(), want);
        until_saved(&ctx, &mut app);
        let last = file_events(&dir).pop().unwrap();
        assert_eq!((last.kind, last.id, last.start, last.end), (comments::Kind::Edit, 1, 990, 1305));
        file_matches_memory(&ctx, &mut app, &dir);
        // One Ctrl+Z: the comment and the frame as before the edit; Ctrl+Y: both again.
        undo_key(&ctx, &mut app);
        assert_eq!((comment(&app, 1).start, comment(&app, 1).end, app.views[0].pb.frame), (1000, 1200, 1500));
        assert!(note(&app).contains("Comment 1 edited") && note(&app).contains("Frame 1500"), "{}", note(&app));
        assert_eq!(app.history.kinds(), kinds);
        redo_key(&ctx, &mut app);
        assert_eq!((comment(&app, 1).start, comment(&app, 1).end, app.views[0].pb.frame), (990, 1305, 1310));
        until_saved(&ctx, &mut app);
        file_matches_memory(&ctx, &mut app, &dir);
    }

    #[test]
    fn a_dragged_frame_box_stops_at_the_file_ends_and_at_the_other_box_and_esc_puts_all_back() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_scrub_ends");
        let dir = _dir_guard.to_path_buf();
        let mut app = long_tab_app(&ctx, &dir);
        let (up, right) = (egui::vec2(0.0, -1.0), egui::vec2(1.0, 0.0));
        let [_, frames, _, _] = cells_of(&ctx, &mut app, 1);
        double_click(&ctx, &mut app, frames);
        assert_eq!(app.views[0].pb.frame, 1500, "a double click on a cell does not move (v0.27.3)");
        let kinds = app.history.kinds();
        until_saved(&ctx, &mut app);
        let lines = line_count(&dir);
        let (s, e) = frame_boxes_of(&ctx, &mut app, 1);
        // Start box far left with Shift: it stops at the first frame, and comes back at once.
        let far = s.center() - right * 150.0;
        press_and_move(&ctx, &mut app, s.center(), far, true);
        assert_eq!((boxes_text(&app).0.as_str(), app.views[0].pb.frame), ("0", 0));
        move_to(&ctx, &mut app, far, far + right * 10.0, false);
        assert_eq!((boxes_text(&app).0.as_str(), app.views[0].pb.frame), ("10", 10), "10 pt back = 10");
        let_go(&ctx, &mut app, far + right * 10.0);
        // Start box far up: it stops at the end box's frame (they do not cross).
        press_and_move(&ctx, &mut app, s.center(), s.center() + up * 150.0, true);
        assert_eq!(boxes_text(&app), ("1200".to_owned(), "1200".to_owned()));
        let_go(&ctx, &mut app, s.center() + up * 150.0);
        // End box down: it stops at the start box's frame.
        press_and_move(&ctx, &mut app, e.center(), e.center() - up * 50.0, false);
        assert_eq!(boxes_text(&app), ("1200".to_owned(), "1200".to_owned()));
        let_go(&ctx, &mut app, e.center() - up * 50.0);
        // End box far up with Shift: it stops at the last frame.
        press_and_move(&ctx, &mut app, e.center(), e.center() + up * 200.0, true);
        assert_eq!((boxes_text(&app).1.as_str(), app.views[0].pb.frame), ("2999", 2999));
        let_go(&ctx, &mut app, e.center() + up * 200.0);
        assert_eq!(app.history.kinds(), kinds);
        // Esc: the numbers and the frame as before the dragging, no line, nothing to undo.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.comment_edit.is_none());
        assert_eq!((comment(&app, 1).start, comment(&app, 1).end, app.views[0].pb.frame), (1000, 1200, 1500));
        assert_eq!(app.history.kinds(), kinds, "a cancelled edit leaves nothing to undo");
        until_saved(&ctx, &mut app);
        assert_eq!(line_count(&dir), lines);
        // The next Ctrl+Z is the action before the edit (the first click: comment 1 selected).
        assert_eq!((picked(&app), sel(&app)), (Some(1), (vec![0], Some(0))));
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].pb.frame, picked(&app), sel(&app)), (1500, None, (vec![], None)));
    }

    #[test]
    fn the_playback_bars_frame_box_drags_the_frame_as_one_action_and_still_types() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("bar_scrub");
        let dir = _dir_guard.to_path_buf();
        let mut app = long_tab_app(&ctx, &dir);
        let (up, right) = (egui::vec2(0.0, -1.0), egui::vec2(1.0, 0.0));
        let hot = || ctx.read_response(panels::frame_box_id(0).with("hot")).expect("the frame box is drawn").rect;
        let n = app.history.undo.len();
        // 40 pt up = 40 frames later, while the button is down.
        let b = hot();
        press_and_move(&ctx, &mut app, b.center(), b.center() + up * 40.0, false);
        assert_eq!((app.views[0].pb.frame, app.views[0].pb.playing), (1540, false));
        move_to(&ctx, &mut app, b.center() + up * 40.0, b.center() + up * 40.0 - right * 15.0, false);
        assert_eq!(app.views[0].pb.frame, 1525, "left = earlier");
        let_go(&ctx, &mut app, b.center() + up * 40.0 - right * 15.0);
        assert_eq!(app.history.undo.len(), n + 1, "one drag = one action: {:?}", app.history.kinds());
        undo_key(&ctx, &mut app);
        assert_eq!(app.views[0].pb.frame, 1500, "Ctrl+Z: the frame before the drag");
        // A playing view pauses when the drag starts (from the frame it had reached then).
        idle(&ctx, &mut app);
        app.views[0].pb.play(ctx.input(|i| i.time));
        press_and_move_now(&ctx, &mut app, b.center(), b.center() + up * 40.0, false);
        let f = app.views[0].pb.frame;
        assert!(!app.views[0].pb.playing && (1540..1560).contains(&f), "paused at the drag: {f}");
        for _ in 0..30 {
            frame_shift(&ctx, &mut app, vec![], false);
        }
        assert_eq!(app.views[0].pb.frame, f, "still paused");
        let_go(&ctx, &mut app, b.center() + up * 40.0);
        undo_key(&ctx, &mut app);
        assert_eq!(app.views[0].pb.frame, 1500, "Ctrl+Z: the frame before playing and dragging");
        // Shift: 10 frames a point; the ends hold.
        let b = hot();
        press_and_move(&ctx, &mut app, b.center(), b.center() - up * 8.0, true);
        assert_eq!(app.views[0].pb.frame, 1420);
        move_to(&ctx, &mut app, b.center() - up * 8.0, b.center() - right * 200.0, true);
        assert_eq!(app.views[0].pb.frame, 0, "stops at the first frame");
        move_to(&ctx, &mut app, b.center() - right * 200.0, b.center() + up * 400.0, true);
        assert_eq!(app.views[0].pb.frame, 2999, "stops at the last frame");
        let_go(&ctx, &mut app, b.center() + up * 400.0);
        // A click without a drag types; Enter goes there, Esc does not.
        tap(&ctx, &mut app, hot().center());
        assert!(ctx.wants_keyboard_input(), "a click types");
        type_over(&ctx, &mut app, "2345");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Enter)]);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].pb.frame, 2345);
        assert!(!ctx.wants_keyboard_input());
        tap(&ctx, &mut app, hot().center());
        type_over(&ctx, &mut app, "7");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].pb.frame, 2345, "Esc: never mind");
    }

    #[test]
    fn a_note_is_written_after_a_double_click_saved_with_enter_and_undone_with_another_line() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_note");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let mut app = comments_tab_app(&ctx, &dir, false);
        let text = |app: &TakViewerApp, id| app.views[0].comments.list.get(id).unwrap().text.clone();
        // A single click on the Comment cell is a row click (no box).
        let [_, _, cell, _] = cells_of(&ctx, &mut app, 1);
        tap(&ctx, &mut app, cell);
        assert!(app.comment_edit.is_none() && picked(&app) == Some(1));
        // A double click opens the note box with the keyboard.
        double_click(&ctx, &mut app, cell);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.comment_edit.is_some() && ctx.wants_keyboard_input(), "the note box has the keys");
        // App keys do nothing while it does (M, Space, →, `[`, Ctrl+Z, Ctrl+Y).
        app.views[0].range = Some((1, 2));
        app.quiet();
        let before = (app.views[0].pb.frame, app.views[0].pb.playing, app.views[0].range, ids(&app), steps(&app, 0));
        for (m, k) in [(Modifiers::NONE, Key::M), (Modifiers::NONE, Key::Space), (Modifiers::NONE, Key::ArrowRight), (Modifiers::NONE, Key::OpenBracket), (ctrl(), Key::Z), (ctrl(), Key::Y)] {
            run_frame(&ctx, &mut app, &[(m, k)]);
        }
        assert_eq!((app.views[0].pb.frame, app.views[0].pb.playing, app.views[0].range, ids(&app), steps(&app, 0)), before, "no app key fired");
        assert!(app.hold.is_none());
        // Type (pasted line break, comma, quotes, Korean), Enter: saved as one line.
        frame_with(&ctx, &mut app, vec![egui::Event::Text("교정, \"swap\"\nnext".into())]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Enter)]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.comment_edit.is_none() && !ctx.wants_keyboard_input());
        assert_eq!(text(&app, 1), "교정, \"swap\" next");
        assert_eq!(app.views[0].range, Some((1, 2)), "the range stays");
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Comment"));
        // Esc in the box only cancels it: no line, no action, the range stays.
        let [_, _, cell2, _] = cells_of(&ctx, &mut app, 2);
        double_click(&ctx, &mut app, cell2);
        let kinds_after_click = app.history.kinds();
        frame_with(&ctx, &mut app, vec![egui::Event::Text("drop me".into())]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.comment_edit.is_none());
        assert_eq!((text(&app, 2), app.views[0].range), (String::new(), Some((1, 2))), "cancelled, and Esc did not clear the range");
        assert_eq!(app.history.kinds(), kinds_after_click, "the cancelled note is no action");
        // Undo / redo of the note: an edit line each (old text / new text).
        for _ in 0..10 {
            if text(&app, 1).is_empty() {
                break;
            }
            undo_key(&ctx, &mut app);
        }
        assert_eq!(text(&app, 1), "");
        assert!(note(&app).contains("Comment 1 edited"), "{}", note(&app));
        redo_key(&ctx, &mut app);
        assert_eq!((text(&app, 1), note(&app).as_str()), ("교정, \"swap\" next".to_owned(), "Redo · Comment 1 edited"));
        file_matches_memory(&ctx, &mut app, &dir);
        let lines: Vec<(String, String)> = file_events(&dir).iter().map(|e| (format!("{} {}", e.kind.as_str(), e.id), e.text.clone())).collect();
        let want = |k: &str, t: &str| (k.to_owned(), t.to_owned());
        assert_eq!(lines, vec![want("add 1", ""), want("add 2", ""), want("edit 1", "교정, \"swap\" next"), want("edit 1", ""), want("edit 1", "교정, \"swap\" next")]);
    }

    #[test]
    fn nothing_is_editable_where_comments_cannot_be_written_and_the_reason_is_shown() {
        let ctx = egui::Context::default();
        // A command run, and a file in the repository's input/ folder.
        let _dir_guard = comment_dir("tab_off");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let _root_guard = comment_dir("tab_off_repo");
        let root = _root_guard.to_path_buf();
        std::fs::create_dir_all(root.join("apps")).unwrap();
        std::fs::create_dir_all(root.join("input").join("raw")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "x").unwrap();
        std::fs::write(root.join("apps").join("AGENTS.md"), "x").unwrap();
        // (`comments::append` refuses input/: the test copies the file there itself.)
        std::fs::copy(dir.join("a.takm"), root.join("input").join("raw").join("a.takm")).unwrap();
        for (folder, command_run) in [(dir.clone(), true), (root.join("input").join("raw"), false)] {
            let before = std::fs::read(folder.join("a.takm")).unwrap();
            let mut app = comments_tab_app(&ctx, &folder, command_run);
            let why = app.comment_writable(0).expect_err("cannot be written");
            let out = frame_with(&ctx, &mut app, vec![]);
            assert_eq!(text_rects(&out, &format!("고칠 수 없어요: {why} · # 두 번 = 첫 프레임")).len(), 1, "the reason, as a line in the tab");
            let cells = cells_of(&ctx, &mut app, 1);
            for at in &cells[..3] {
                double_click(&ctx, &mut app, *at);
                assert!(app.comment_edit.is_none(), "no edit");
            }
            assert_eq!((app.views[0].pb.frame, picked(&app)), (0, Some(1)), "a click still selects, a cell double clicked does not move");
            let number = comment_list::id_point(row_of(&ctx, &mut app, 1));
            double_click(&ctx, &mut app, number);
            assert_eq!((app.views[0].pb.frame, picked(&app)), (3, Some(1)), "`#` double clicked still goes there");
            let [.., del2] = cells_of(&ctx, &mut app, 2);
            for d in [cells[3], del2] {
                tap(&ctx, &mut app, d);
            }
            assert_eq!(ids(&app), vec![1, 2], "nothing deleted");
            assert!(app.comment_writer.is_none());
            assert_eq!(std::fs::read(folder.join("a.takm")).unwrap(), before, "nothing written");
        }
    }

    #[test]
    fn the_tab_has_no_hover_texts_and_fits_a_narrow_place() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_narrow");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let mut app = comments_tab_app(&ctx, &dir, false);
        // Under the mouse on every cell and column name: no tooltip is shown.
        let [m, f, t, d] = cells_of(&ctx, &mut app, 1);
        let ty = comment_list::type_point(row_of(&ctx, &mut app, 1));
        let h = header(&ctx, &mut app, "Markers");
        let th = header(&ctx, &mut app, "Type");
        for at in [m, f, t, d, h, ty, th] {
            frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
            for _ in 0..3 {
                let out = frame_with(&ctx, &mut app, vec![]);
                let _ = out;
                let tip = ctx.memory(|m| m.areas().visible_layer_ids().iter().any(|l| l.order == egui::Order::Tooltip));
                assert!(!tip, "no hover text at {at:?}");
            }
        }
        // ~380 pt: the lines at the top fit on one line and five-digit frames fit their boxes.
        let width = |s: &str, size: f32| ctx.fonts(|f| f.layout_no_wrap(s.to_owned(), egui::FontId::proportional(size), egui::Color32::WHITE).size().x);
        for field in [Field::Markers, Field::Frames, Field::Text, Field::Type] {
            for (edit, picked) in [(Some((2000, field)), None), (None, Some(2000)), (None, None)] {
                let (line, _) = comment_list::top_line(edit, picked, &Ok(()));
                assert!(width(&line, comment_list::LINE_FONT) + 12.0 <= 380.0, "{line}: {}", width(&line, comment_list::LINE_FONT));
            }
        }
        let row = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(380.0, comment_list::ROW_H));
        let [s, e, note] = comment_list::box_rects(row);
        for r in [s, e] {
            assert!(width("99999", 12.5) + 6.0 <= r.width(), "a five-digit frame fits: {r:?}");
        }
        assert!(note.width() >= 80.0 && note.right() <= 380.0 - 20.0, "the note box and the ✕: {note:?}");
        // The Type column (type-combo): its name when sorted fits, and each short form fits its
        // box before the ▼ (text from 5 pt in, ▼ room 15 pt).
        let cell = comment_list::type_cell(row);
        assert!(width("Type ▼", 12.5) + 6.0 <= cell.width(), "Type ▼ fits: {}", cell.width());
        let tbox = comment_list::type_box_of(row);
        for s in ["Jitter", "Check", "Swap", "—"] {
            assert!(width(s, 12.5) + 5.0 + 15.0 <= tbox.width(), "{s} fits the Type box: {}", tbox.width());
        }
        // The type list: its items have no hover text either, and the list fits ~380 pt.
        let ty = comment_list::type_point(row_of(&ctx, &mut app, 1));
        tap(&ctx, &mut app, ty);
        frame_with(&ctx, &mut app, vec![]);
        let items = app.type_items.clone();
        assert_eq!(items.len(), 4, "the list is open");
        for (_, r) in items {
            frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(r.center())]);
            for _ in 0..3 {
                frame_with(&ctx, &mut app, vec![]);
                assert!(!ctx.memory(|m| m.areas().visible_layer_ids().iter().any(|l| l.order == egui::Order::Tooltip)), "no hover text on an item");
            }
        }
        let name_w = comment_list::TYPE_LIST.iter().map(|v| width(comment_list::item_name(v), 12.5)).fold(0.0, f32::max);
        let size = comment_list::list_size(name_w);
        let room = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(380.0, 300.0));
        let at = comment_list::list_place(tbox.translate(egui::vec2(0.0, 100.0)), size, room);
        assert!(room.contains_rect(at) && size.x <= 200.0, "the list fits a 380 pt pane: {at:?}");
    }

    #[test]
    fn a_new_comment_scrolls_into_the_long_list_and_only_rows_on_screen_are_drawn() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_long");
        let dir = _dir_guard.to_path_buf();
        let mut app = comments_tab_app(&ctx, &dir, false);
        assert!(text_rects(&frame_with(&ctx, &mut app, vec![]), "아직 Comment가 없어요. M을 누르면 생겨요.").len() == 1, "empty: one line");
        // 2,000 comments in memory (as if read from the file).
        let mut list = comments::Comments::default();
        for id in 1..=2000u64 {
            let c = Comment { id, markers: vec!["M".into()], start: id as i64 % 9, end: id as i64 % 9, comment_type: String::new(), text: format!("note {id}") };
            list.apply(&Event::of(EventKind::Add, &c, "t".into(), "a.tak|0".into()));
        }
        app.views[0].comments.list = list;
        // The fixture above is deliberately in-memory only, so supply its corresponding range.
        app.views[0].comments.id_pool = Some(2001..=3000);
        let out = frame_with(&ctx, &mut app, vec![]);
        let place = place_of(&app, Pane::Comments);
        let drawn = (1..=2000).filter(|k| text_rects(&out, &format!("note {k}")).iter().any(|t| place.contains(t.center()))).count();
        assert!(drawn > 3 && drawn < 80, "only the rows on screen: {drawn}");
        // A Type box (here `—`, no type) only in the rows on screen (type-combo).
        let boxes = text_rects(&out, "—").iter().filter(|t| place.contains(t.center())).count();
        assert!(boxes >= drawn && boxes <= drawn + 2, "a box per row drawn: {boxes} for {drawn}");
        let builds = app.comment_order.builds;
        // A row selected and its marker cell edited: still no sorting while nothing changes.
        let [markers, ..] = cells_of(&ctx, &mut app, 2);
        double_click(&ctx, &mut app, markers);
        assert!(app.comment_edit.is_some());
        for _ in 0..5 {
            frame_with(&ctx, &mut app, vec![]);
        }
        assert_eq!(app.comment_order.builds, builds, "no sorting while nothing changes");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        // M: comment 2001, and the list shows it.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        frame_with(&ctx, &mut app, vec![]);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert!(text_rects(&out, "2001").iter().any(|t| place.contains(t.center())), "scrolled to the new comment");
        until_saved(&ctx, &mut app);
    }

    // ───────────── Comment types (Comment plan step 5) ─────────────

    /// The comment the type strip is for (None = no strip).
    fn strip_for(app: &TakViewerApp) -> Option<u64> {
        app.type_strip.as_ref().map(|s| s.id)
    }

    /// Middle of type button `k` (0 Jitter, 1 Marker swap, 2 Needs visual check; type-combo order) of the strip as
    /// drawn now.
    fn strip_button(ctx: &egui::Context, app: &mut TakViewerApp, k: usize) -> egui::Pos2 {
        frame_with(ctx, app, vec![]);
        app.type_strip.as_ref().expect("a strip").buttons[k].center()
    }

    /// The `Comment` button of view 0's bar.
    fn comment_button_at(ctx: &egui::Context, app: &mut TakViewerApp) -> egui::Pos2 {
        let out = frame_with(ctx, app, vec![]);
        let r = app.views[0].scene_rect;
        text_rects(&out, "Comment").into_iter().find(|t| t.top() > r.bottom()).expect("Comment button").center()
    }

    fn type_of(app: &TakViewerApp, id: u64) -> String {
        comment(app, id).comment_type
    }

    /// `<dir>/a.comments.csv` with these comments (id, frames, type), no markers.
    fn typed_on_disk(dir: &Path, list: &[(u64, i64, &str)]) {
        let ev: Vec<Event> = list
            .iter()
            .map(|&(id, f, t)| {
                let c = Comment { id, markers: vec![], start: f, end: f, comment_type: t.into(), text: String::new() };
                Event::of(EventKind::Add, &c, "2026-11-14T10:00:00+09:00".into(), "a.tak|0".into())
            })
            .collect();
        tail_append(&dir.join("a.takm"), &ev).unwrap();
    }

    #[test]
    fn a_type_strip_follows_each_new_comment_a_click_sets_the_type_and_the_next_action_closes_it() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("strip");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        // Select before creating the Comment: later direct marker picks now clear its
        // representative, while this test needs T to keep targeting the new Comment.
        app.views[0].select(0, false);
        let button_before = comment_button_at(&ctx, &mut app);
        let scene_before = app.views[0].scene_rect;
        // M short: comment 1, its strip with the three full names, over the 3D right above the
        // bar's right end; nothing in the bar or the 3D moved.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        assert_eq!((ids(&app), strip_for(&app)), (vec![1], Some(1)));
        let out = frame_with(&ctx, &mut app, vec![]);
        for t in comments::TYPES {
            assert_eq!(text_rects(&out, t).len(), 1, "{t} on the strip");
        }
        let s = app.type_strip.clone().unwrap();
        let scene = app.views[0].scene_rect;
        assert_eq!(scene, scene_before, "the 3D keeps its place");
        assert_eq!(comment_button_at(&ctx, &mut app), button_before, "the bar keeps its place");
        assert!(scene.contains_rect(s.rect) && s.rect.bottom() > scene.bottom() - 40.0 && s.rect.right() > scene.right() - 20.0, "over the 3D, right above the bar's right end: {:?} in {scene:?}", s.rect);
        assert!((s.rect.right() - button_before.x).abs() < 80.0, "by the Comment button");
        // No hover text on it, no action, nothing written for showing it.
        until_saved(&ctx, &mut app);
        let lines = line_count(&dir);
        let kinds = app.history.kinds();
        // Mouse moves and playback running leave it open.
        frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(scene.center())]);
        app.views[0].pb.toggle(ctx.input(|i| i.time));
        for _ in 0..5 {
            frame_with(&ctx, &mut app, vec![]);
        }
        app.views[0].pb.playing = false;
        // A click on the strip must not reach the selected marker in the 3D under it.
        assert_eq!(app.views[0].selected, BTreeSet::from([0]));
        app.quiet();
        assert_eq!(strip_for(&app), Some(1), "still open");
        // T (the keyboard path, the new comment is the selected one): the next type, the strip
        // stays open and marks it.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        assert_eq!((type_of(&app, 1).as_str(), strip_for(&app)), ("Jitter", Some(1)));
        // A click on Marker swap: that type, one edit line; the strip closes. With the T just
        // before it is one action (type changes of one comment in a row).
        let at = strip_button(&ctx, &mut app, 1);
        tap(&ctx, &mut app, at);
        assert_eq!((type_of(&app, 1).as_str(), strip_for(&app)), ("Marker swap", None));
        assert_eq!(note(&app), "Comment 1 · Marker swap");
        assert_eq!(app.history.kinds().len(), kinds.len() + 1);
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Comment"));
        file_matches_memory(&ctx, &mut app, &dir);
        let ev = file_events(&dir);
        let types: Vec<(comments::Kind, &str)> = ev[lines..].iter().map(|e| (e.kind, e.comment_type.as_str())).collect();
        assert_eq!(types, vec![(comments::Kind::Edit, "Jitter"), (comments::Kind::Edit, "Marker swap")]);
        assert_eq!(app.views[0].selected, BTreeSet::from([0]), "the click on the strip did not reach the 3D");
        // Ctrl+Z / Ctrl+Y: the type goes and comes back (edit lines).
        undo_key(&ctx, &mut app);
        assert_eq!((type_of(&app, 1).as_str(), note(&app).as_str()), ("", "Undo · Comment 1 edited"));
        redo_key(&ctx, &mut app);
        assert_eq!(type_of(&app, 1), "Marker swap");
        file_matches_memory(&ctx, &mut app, &dir);
        // M held over frames: the strip comes when M goes up.
        app.views[0].pb.set_frame(2, 0.0);
        app.quiet();
        key_down(&ctx, &mut app, Key::M);
        assert_eq!(strip_for(&app), None);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        key_up(&ctx, &mut app, Key::M);
        assert_eq!((ids(&app), strip_for(&app)), (vec![1, 2], Some(2)));
        // A key elsewhere closes it and still does its job (→ steps a frame); no type.
        let f = app.views[0].pb.frame;
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        assert_eq!((app.views[0].pb.frame, strip_for(&app), type_of(&app, 2).as_str()), (f + 1, None, ""));
        // The Comment button: a strip for comment 3; a click in the seek zone closes it and
        // still moves the frame.
        let b = comment_button_at(&ctx, &mut app);
        tap(&ctx, &mut app, b);
        assert_eq!((ids(&app), strip_for(&app)), (vec![1, 2, 3], Some(3)));
        let bar = Bar::of(&app, 0);
        tap(&ctx, &mut app, bar.seek(5.0));
        assert_eq!((app.views[0].pb.frame, strip_for(&app)), (5, None));
        // The wheel closes it too; Esc closes it and still clears the range.
        tap(&ctx, &mut app, b);
        assert_eq!(strip_for(&app), Some(4));
        frame_with(&ctx, &mut app, vec![egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: egui::vec2(0.0, 1.0), modifiers: Modifiers::NONE }]);
        assert_eq!(strip_for(&app), None);
        tap(&ctx, &mut app, b);
        app.views[0].range = Some((1, 2));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        assert_eq!((strip_for(&app), app.views[0].range), (None, None));
        // Its comment deleted (Ctrl+Z of the add): it closes.
        tap(&ctx, &mut app, b);
        assert_eq!(strip_for(&app), Some(6));
        let last =app.views[0].comments.list.get(6).cloned().unwrap();
        let _ = app.write_change(&ctx, 0, EventKind::Delete, &last);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(strip_for(&app), None);
        assert!(file_events(&dir).iter().all(|e| e.kind != comments::Kind::Edit || e.id == 1), "only comment 1 got a type");
        file_matches_memory(&ctx, &mut app, &dir);
    }

    #[test]
    fn no_type_strip_where_comments_cannot_be_written_and_a_picture_strip_writes_nothing() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("strip_off");
        let dir = _dir_guard.to_path_buf();
        typed_on_disk(&dir, &[(1, 3, "")]);
        let before = std::fs::read(dir.join("a.takm")).unwrap();
        // A command run: M and the button make nothing, so no strip.
        let mut app = comment_app(&ctx, &dir, true);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        assert_eq!((ids(&app), strip_for(&app)), (vec![1], None));
        // `--type-strip` (pictures): drawn, but a click on it writes nothing.
        app.type_strip = Some(TypeStrip { slot: 0, file_id: app.views[0].file_id, id: 1, rect: egui::Rect::NOTHING, buttons: [egui::Rect::NOTHING; 3], shot: true });
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, "Needs visual check").len(), 1, "shown for the picture");
        let at = strip_button(&ctx, &mut app, 0);
        tap(&ctx, &mut app, at);
        assert_eq!(type_of(&app, 1), "");
        assert!(app.comment_writer.is_none());
        // T in a command run: nothing either.
        app.views[0].picked = Some(1);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        assert_eq!(type_of(&app, 1), "");
        assert_eq!(std::fs::read(dir.join("a.takm")).unwrap(), before, "nothing written");
    }

    #[test]
    fn t_cycles_the_selected_comments_type_and_presses_in_a_row_are_one_action() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("t_key");
        let dir = _dir_guard.to_path_buf();
        typed_on_disk(&dir, &[(1, 2, ""), (2, 7, "")]);
        let mut app = comments_tab_app(&ctx, &dir, false);
        // Nothing selected: T does nothing.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        assert_eq!((type_of(&app, 1), type_of(&app, 2), line_count(&dir)), (String::new(), String::new(), 2));
        assert!(app.history.kinds().is_empty());
        // Comment 2 selected (a row click): T goes round; back at none the presses cancel out.
        let [_, at, _, _] = cells_of(&ctx, &mut app, 2);
        tap(&ctx, &mut app, at);
        let kinds = app.history.kinds();
        for want in ["Jitter", "Marker swap", "Needs visual check", ""] {
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
            assert_eq!(type_of(&app, 2), want);
            assert_eq!(type_of(&app, 1), "", "only the selected one");
        }
        assert_eq!(app.history.kinds(), kinds, "four presses back to the start: no action left");
        until_saved(&ctx, &mut app);
        assert_eq!(line_count(&dir), 6, "one edit line per press");
        // Two presses: one action, its note; the tab shows the short form.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        assert_eq!((type_of(&app, 2).as_str(), note(&app).as_str()), ("Marker swap", "Comment 2 · Marker swap"));
        assert_eq!(app.history.kinds().len(), kinds.len() + 1);
        let out = frame_with(&ctx, &mut app, vec![]);
        let cell = comment_list::type_cell(app.comment_rows.iter().find(|r| r.0 == 2).unwrap().1);
        assert_eq!(text_rects(&out, "Swap").iter().filter(|t| cell.contains(t.center())).count(), 1, "short form in the Type box");
        // One Ctrl+Z takes both presses back; Ctrl+Y does them again.
        undo_key(&ctx, &mut app);
        assert_eq!(type_of(&app, 2), "");
        redo_key(&ctx, &mut app);
        assert_eq!(type_of(&app, 2), "Marker swap");
        // After a frame move the next press is a new action.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowRight)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        assert_eq!(type_of(&app, 2), "Needs visual check");
        let k = app.history.kinds();
        assert_eq!(k[k.len() - 2..].to_vec(), vec!["Frame".to_owned(), "Comment".to_owned()]);
        // A held T repeats (egui marks the presses while it is down): only the real press counts.
        key_down(&ctx, &mut app, Key::T);
        assert_eq!(type_of(&app, 2), "");
        for _ in 0..3 {
            frame_with(&ctx, &mut app, vec![key_event(Key::T, true, Modifiers::NONE)]);
        }
        key_up(&ctx, &mut app, Key::T);
        assert_eq!(type_of(&app, 2), "", "the auto-repeat did nothing");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        assert_eq!(type_of(&app, 2), "Needs visual check");
        // During a cell edit (here the note box has the keys): T does nothing.
        let [_, _, note_cell, _] = cells_of(&ctx, &mut app, 2);
        double_click(&ctx, &mut app, note_cell);
        assert_eq!(app.comment_edit.as_ref().map(|e| e.field), Some(Field::Text));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        assert_eq!(type_of(&app, 2), "Needs visual check");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        // The type list open (type-combo; no text box): T and M do nothing, the list stays.
        let t = comment_list::type_point(row_of(&ctx, &mut app, 2));
        tap(&ctx, &mut app, t);
        assert_eq!(app.comment_edit.as_ref().map(|e| e.field), Some(Field::Type));
        assert!(!ctx.wants_keyboard_input());
        until_saved(&ctx, &mut app);
        let lines = line_count(&dir);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!((type_of(&app, 2).as_str(), ids(&app), line_count(&dir)), ("Needs visual check", vec![1, 2], lines));
        assert_eq!(app.comment_edit.as_ref().map(|e| (e.id, e.field)), Some((2, Field::Type)), "still open");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        assert!(app.comment_edit.is_none());
        // A text field has the keys (path dialog): T is a letter there.
        app.open_path_dialog();
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(ctx.wants_keyboard_input());
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        assert_eq!(type_of(&app, 2), "Needs visual check");
        file_matches_memory(&ctx, &mut app, &dir);
    }


    /// Open the type list of comment `id` with one click on its Type box, then one more frame
    /// (a list shown for the first time is measured, unseen, in its first frame).
    fn open_type_list(ctx: &egui::Context, app: &mut TakViewerApp, id: u64) {
        let at = comment_list::type_point(row_of(ctx, app, id));
        tap(ctx, app, at);
        frame_with(ctx, app, vec![]);
    }

    /// Middle of the item `value` of the type list as drawn now.
    fn item_at(ctx: &egui::Context, app: &mut TakViewerApp, value: &str) -> egui::Pos2 {
        frame_with(ctx, app, vec![]);
        app.type_items.iter().find(|i| i.0 == value).unwrap_or_else(|| panic!("item {value:?}")).1.center()
    }

    /// The comment whose type list is open.
    fn list_open(app: &TakViewerApp) -> Option<u64> {
        app.comment_edit.as_ref().filter(|e| e.field == Field::Type).map(|e| e.id)
    }

    /// The item highlighted in the open list (None: no list, or nothing highlighted).
    fn highlighted(app: &TakViewerApp) -> Option<String> {
        app.comment_edit.as_ref().filter(|e| e.field == Field::Type && comment_list::TYPE_LIST.contains(&e.choice.as_str())).map(|e| e.choice.clone())
    }

    /// Whether `text` was painted with its left end inside `r`.
    fn text_in_box(out: &egui::FullOutput, text: &str, r: egui::Rect) -> bool {
        text_rects(out, text).iter().any(|t| r.contains(t.left_center() + egui::vec2(1.0, 0.0)))
    }

    #[test]
    fn every_row_has_a_type_box_and_one_click_opens_the_list_under_it() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tab_type");
        let dir = _dir_guard.to_path_buf();
        typed_on_disk(&dir, &[(1, 1, "Jitter"), (2, 2, ""), (3, 3, "Needs visual check"), (4, 4, "Marker swap"), (5, 5, "Later kind")]);
        let mut app = comments_tab_app(&ctx, &dir, false);
        let place = place_of(&app, Pane::Comments);
        // Every row: its box with the short form (`—` for none, an unknown value as written).
        let out = frame_with(&ctx, &mut app, vec![]);
        let rows = app.comment_rows.clone();
        assert_eq!(rows.len(), 5);
        for (id, short) in [(1, "Jitter"), (2, "—"), (3, "Check"), (4, "Swap"), (5, "Later kind")] {
            let b = comment_list::type_box_of(rows.iter().find(|r| r.0 == id).unwrap().1);
            assert!(text_in_box(&out, short, b), "{short} in the box of row {id}");
        }
        for long in ["Marker swap", "Needs visual check", "None"] {
            assert!(text_rects(&out, long).is_empty(), "no full name while the list is closed: {long}");
        }
        assert!(app.type_items.is_empty() && list_open(&app).is_none());
        // The Type column sorts in the one order: none, Jitter, Marker swap, Needs visual check,
        // then unknown values.
        let h = header(&ctx, &mut app, "Type");
        tap(&ctx, &mut app, h);
        assert_eq!(shown_ids(&ctx, &mut app), vec![2, 1, 4, 3, 5]);
        let h = header(&ctx, &mut app, "Type ▲");
        tap(&ctx, &mut app, h);
        assert_eq!(shown_ids(&ctx, &mut app), vec![5, 3, 4, 1, 2]);
        let h = header(&ctx, &mut app, "Type ▼");
        tap(&ctx, &mut app, h);
        assert_eq!(shown_ids(&ctx, &mut app), vec![2, 1, 4, 3, 5]);
        // One click on the box of comment 4 (a middle row): it is selected and gone to (a row
        // click), and its list opens right under the box, inside the tab: the full names in
        // the one order, None last, its type highlighted. Nothing written.
        let lines = line_count(&dir);
        let f0 = app.views[0].pb.frame;
        open_type_list(&ctx, &mut app, 4);
        assert_eq!((list_open(&app), picked(&app), app.views[0].pb.frame), (Some(4), Some(4), f0), "selected, not gone to (v0.27.3)");
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, "Comment 4 유형 · ↑ ↓ Enter · Esc 닫기").len(), 1, "the line at the top");
        let items = app.type_items.clone();
        assert_eq!(items.iter().map(|i| i.0.as_str()).collect::<Vec<_>>(), ["Jitter", "Marker swap", "Needs visual check", ""]);
        let bx = comment_list::type_box_of(row_of(&ctx, &mut app, 4));
        let list = items.iter().fold(egui::Rect::NOTHING, |a, i| a.union(i.1));
        assert!(place.contains_rect(list), "inside the tab: {list:?} in {place:?}");
        assert!(list.top() >= bx.bottom() - 0.5 && list.top() <= bx.bottom() + 6.0, "right under the box: {list:?} / {bx:?}");
        for k in 1..4 {
            assert!(items[k].1.top() >= items[k - 1].1.bottom() - 0.5, "top to bottom");
        }
        for name in ["Jitter", "Marker swap", "Needs visual check", "None"] {
            assert!(text_rects(&out, name).iter().any(|t| list.contains(t.center())), "{name} in the list");
        }
        assert_eq!(highlighted(&app).as_deref(), Some("Marker swap"), "the current one highlighted");
        until_saved(&ctx, &mut app);
        assert_eq!(line_count(&dir), lines, "opening writes nothing");
        // A click elsewhere in a row (not on the box) is a plain row click: the list closes and
        // that comment is selected; no list opens for it.
        let [_, frames3, _, _] = cells_of(&ctx, &mut app, 3);
        tap(&ctx, &mut app, frames3);
        assert_eq!((list_open(&app), picked(&app), app.views[0].pb.frame), (None, Some(3), f0));
        tap(&ctx, &mut app, frames3);
        assert_eq!(list_open(&app), None, "only the box opens the list");
        // A double click on a closed box opens it (the second click keeps it open); no move.
        let b = comment_list::type_point(row_of(&ctx, &mut app, 1));
        double_click(&ctx, &mut app, b);
        assert_eq!((list_open(&app), picked(&app), app.views[0].pb.frame), (Some(1), Some(1), f0));
        // A double click on Markers / Frames / Comment still edits that cell (the first click
        // closes the list); no move.
        let cells = cells_of(&ctx, &mut app, 1);
        for (at, field) in cells[..3].iter().zip([Field::Markers, Field::Frames, Field::Text]) {
            double_click(&ctx, &mut app, *at);
            assert_eq!(app.comment_edit.as_ref().map(|e| (e.id, e.field)), Some((1, field)));
            assert_eq!(app.views[0].pb.frame, f0);
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
            assert!(app.comment_edit.is_none());
        }
        until_saved(&ctx, &mut app);
        assert_eq!(line_count(&dir), lines, "nothing written");
    }

    #[test]
    fn a_click_on_an_item_saves_at_once_and_ctrl_z_takes_it_back() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("type_pick");
        let dir = _dir_guard.to_path_buf();
        typed_on_disk(&dir, &[(1, 1, "Jitter"), (2, 2, ""), (3, 3, "Needs visual check"), (4, 4, "")]);
        let mut app = comments_tab_app(&ctx, &dir, false);
        let f0 = app.views[0].pb.frame;
        open_type_list(&ctx, &mut app, 2);
        assert_eq!(highlighted(&app).as_deref(), Some(""), "none: None highlighted");
        let (lines, kinds, rev) = (line_count(&dir), app.history.kinds().len(), app.views[0].comments.list.revision());
        // Marker swap: saved at once (one edit line, one action), the list closes; the click
        // does not reach the row under the list.
        let at = item_at(&ctx, &mut app, "Marker swap");
        tap(&ctx, &mut app, at);
        assert_eq!((list_open(&app), type_of(&app, 2).as_str()), (None, "Marker swap"));
        assert_eq!((picked(&app), app.views[0].pb.frame), (Some(2), f0), "the row under the list got nothing");
        assert_eq!(note(&app), "Comment 2 · Marker swap");
        assert_eq!(app.history.kinds().len(), kinds + 1);
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Comment"));
        assert!(app.views[0].comments.list.revision() != rev, "the list and the band are made again");
        until_saved(&ctx, &mut app);
        assert_eq!(line_count(&dir), lines + 1);
        let last = file_events(&dir).pop().unwrap();
        assert_eq!((last.kind, last.id, last.comment_type.as_str()), (comments::Kind::Edit, 2, "Marker swap"));
        file_matches_memory(&ctx, &mut app, &dir);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert!(text_in_box(&out, "Swap", comment_list::type_box_of(row_of(&ctx, &mut app, 2))), "the box shows it");
        // Ctrl+Z / Ctrl+Y.
        undo_key(&ctx, &mut app);
        assert_eq!((type_of(&app, 2).as_str(), note(&app).as_str()), ("", "Undo · Comment 2 edited"));
        redo_key(&ctx, &mut app);
        assert_eq!(type_of(&app, 2), "Marker swap");
        file_matches_memory(&ctx, &mut app, &dir);
        // The same type again: no line, no action; the list closes.
        open_type_list(&ctx, &mut app, 2);
        let (lines, kinds) = (line_count(&dir), app.history.kinds().len());
        let at = item_at(&ctx, &mut app, "Marker swap");
        tap(&ctx, &mut app, at);
        until_saved(&ctx, &mut app);
        assert_eq!((list_open(&app), line_count(&dir), app.history.kinds().len()), (None, lines, kinds));
        // None takes the type off; Jitter on another comment.
        open_type_list(&ctx, &mut app, 3);
        let at = item_at(&ctx, &mut app, "");
        tap(&ctx, &mut app, at);
        assert_eq!(type_of(&app, 3), "");
        open_type_list(&ctx, &mut app, 4);
        let at = item_at(&ctx, &mut app, "Jitter");
        tap(&ctx, &mut app, at);
        assert_eq!(type_of(&app, 4), "Jitter");
        file_matches_memory(&ctx, &mut app, &dir);
    }

    #[test]
    fn up_down_and_enter_pick_with_the_keys_and_esc_closes_without_change() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("type_keys");
        let dir = _dir_guard.to_path_buf();
        typed_on_disk(&dir, &[(1, 1, ""), (2, 2, "Needs visual check"), (3, 3, "Later kind")]);
        let mut app = comments_tab_app(&ctx, &dir, false);
        open_type_list(&ctx, &mut app, 2);
        let (frame, lines, kinds) = (app.views[0].pb.frame, line_count(&dir), app.history.kinds().len());
        // ↓ to None (stops there), ↑ ↑ to Marker swap; the frame stays (↑ ↓ go to the list).
        let mut seen = vec![];
        for key in [Key::ArrowDown, Key::ArrowDown, Key::ArrowUp, Key::ArrowUp] {
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, key)]);
            seen.push(highlighted(&app).unwrap());
        }
        assert_eq!(seen, ["", "", "Needs visual check", "Marker swap"]);
        assert_eq!((app.views[0].pb.frame, type_of(&app, 2).as_str()), (frame, "Needs visual check"), "not saved yet");
        // Enter picks it: one edit line, one action, the list closes.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Enter)]);
        until_saved(&ctx, &mut app);
        assert_eq!((list_open(&app), type_of(&app, 2).as_str()), (None, "Marker swap"));
        assert_eq!((line_count(&dir), app.history.kinds().len()), (lines + 1, kinds + 1));
        // Esc: closed with nothing changed, no action; the Esc does not also clear the
        // selection.
        open_type_list(&ctx, &mut app, 2);
        let (lines, kinds) = (line_count(&dir), app.history.kinds().len());
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowUp)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        until_saved(&ctx, &mut app);
        assert_eq!((list_open(&app), type_of(&app, 2).as_str(), picked(&app)), (None, "Marker swap", Some(2)));
        assert_eq!((line_count(&dir), app.history.kinds().len()), (lines, kinds));
        // A value the app does not know: the list opens with nothing highlighted; Enter changes
        // nothing; ↓ starts at the first item, and any item replaces the value.
        open_type_list(&ctx, &mut app, 3);
        assert_eq!((list_open(&app), highlighted(&app)), (Some(3), None));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Enter)]);
        until_saved(&ctx, &mut app);
        assert_eq!((type_of(&app, 3).as_str(), line_count(&dir)), ("Later kind", lines));
        open_type_list(&ctx, &mut app, 3);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowDown)]);
        assert_eq!(highlighted(&app).as_deref(), Some("Jitter"));
        let at = item_at(&ctx, &mut app, "Needs visual check");
        tap(&ctx, &mut app, at);
        assert_eq!(type_of(&app, 3), "Needs visual check");
        // With the list closed, ↑ ↓ step frames as always.
        let f = app.views[0].pb.frame;
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::ArrowUp)]);
        assert_ne!(app.views[0].pb.frame, f);
        file_matches_memory(&ctx, &mut app, &dir);
    }

    #[test]
    fn a_click_or_the_wheel_outside_the_list_closes_it_and_still_does_its_job() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("type_outside");
        let dir = _dir_guard.to_path_buf();
        typed_on_disk(&dir, &[(1, 1, ""), (2, 2, "")]);
        let mut app = comments_tab_app(&ctx, &dir, false);
        let lines = line_count(&dir);
        // A click in the bar's seek zone: closed, and the frame moves.
        open_type_list(&ctx, &mut app, 1);
        let bar = Bar::of(&app, 0);
        tap(&ctx, &mut app, bar.seek(5.0));
        assert_eq!((list_open(&app), app.views[0].pb.frame, type_of(&app, 1).as_str()), (None, 5, ""));
        // A click on another row: closed, and that row is selected (not gone to, v0.27.3).
        open_type_list(&ctx, &mut app, 1);
        let [_, frames2, _, _] = cells_of(&ctx, &mut app, 2);
        tap(&ctx, &mut app, frames2);
        assert_eq!((list_open(&app), picked(&app), app.views[0].pb.frame), (None, Some(2), 5));
        // A click on the box whose list is open closes it (and is that row's click).
        open_type_list(&ctx, &mut app, 1);
        let b = comment_list::type_point(row_of(&ctx, &mut app, 1));
        tap(&ctx, &mut app, b);
        assert_eq!((list_open(&app), picked(&app)), (None, Some(1)));
        // The wheel closes it.
        open_type_list(&ctx, &mut app, 1);
        frame_with(&ctx, &mut app, vec![egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: egui::vec2(0.0, 1.0), modifiers: Modifiers::NONE }]);
        assert_eq!(list_open(&app), None);
        // Another box (not under the list): closed, then that one's list opens.
        open_type_list(&ctx, &mut app, 2);
        open_type_list(&ctx, &mut app, 1);
        assert_eq!((list_open(&app), picked(&app)), (Some(1), Some(1)));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        until_saved(&ctx, &mut app);
        assert_eq!((type_of(&app, 1), type_of(&app, 2), line_count(&dir)), (String::new(), String::new(), lines), "nothing written");
    }

    #[test]
    fn the_list_of_the_last_row_on_screen_opens_upward_inside_the_tab() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("type_last_row");
        let dir = _dir_guard.to_path_buf();
        let list: Vec<(u64, i64, &str)> = (1..=60u64).map(|k| (k, (k % 10) as i64, ["", "Jitter", "Marker swap", "Needs visual check"][k as usize % 4])).collect();
        typed_on_disk(&dir, &list);
        let mut app = comments_tab_app(&ctx, &dir, false);
        let place = place_of(&app, Pane::Comments);
        let out = frame_with(&ctx, &mut app, vec![]);
        let rows = app.comment_rows.clone();
        assert!(rows.len() < 60, "the tab is full of rows: {}", rows.len());
        // A box in every row on screen.
        for (id, row) in &rows {
            let short = ["—", "Jitter", "Swap", "Check"][*id as usize % 4];
            let b = comment_list::type_box_of(*row);
            if place.contains_rect(b) {
                assert!(text_in_box(&out, short, b), "{short} in row {id}");
            }
        }
        // The last row whose box can be clicked.
        let (id, row) = rows.iter().filter(|r| place.contains(comment_list::type_point(r.1))).max_by(|a, b| a.1.top().total_cmp(&b.1.top())).copied().unwrap();
        assert!(place.bottom() - row.bottom() < 4.0 * comment_list::ITEM_H, "near the bottom: {row:?} in {place:?}");
        open_type_list(&ctx, &mut app, id);
        assert_eq!(list_open(&app), Some(id));
        let bx = comment_list::type_box_of(row_of(&ctx, &mut app, id));
        let list = app.type_items.iter().fold(egui::Rect::NOTHING, |a, i| a.union(i.1));
        assert!(place.contains_rect(list), "inside the tab: {list:?} in {place:?}");
        assert!(list.bottom() <= bx.top() + 0.5 && list.bottom() >= bx.top() - 6.0, "right above the box: {list:?} / {bx:?}");
        // Its items can be clicked there.
        let at = item_at(&ctx, &mut app, "Jitter");
        tap(&ctx, &mut app, at);
        assert_eq!(type_of(&app, id), "Jitter");
    }

    #[test]
    fn where_comments_cannot_be_written_the_boxes_are_dim_and_a_click_is_a_row_click() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("type_off");
        let dir = _dir_guard.to_path_buf();
        typed_on_disk(&dir, &[(1, 3, "Jitter"), (2, 5, "")]);
        let before = std::fs::read(dir.join("a.takm")).unwrap();
        let mut app = comments_tab_app(&ctx, &dir, true);
        let why = app.comment_writable(0).expect_err("a command run");
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, &format!("고칠 수 없어요: {why} · # 두 번 = 첫 프레임")).len(), 1, "the reason at the top");
        for (id, short) in [(1, "Jitter"), (2, "—")] {
            assert!(text_in_box(&out, short, comment_list::type_box_of(row_of(&ctx, &mut app, id))), "{short} still shown");
        }
        // A click or a double click on a box: a row click (select, no move), no list.
        let f0 = app.views[0].pb.frame;
        let b = comment_list::type_point(row_of(&ctx, &mut app, 2));
        tap(&ctx, &mut app, b);
        assert_eq!((list_open(&app), picked(&app), app.views[0].pb.frame), (None, Some(2), f0));
        double_click(&ctx, &mut app, b);
        assert_eq!((list_open(&app), app.views[0].pb.frame), (None, f0));
        // `--edit-comment 1:type` (agent pictures): the list is shown; a click on it writes
        // nothing.
        let v = &app.views[0];
        app.comment_edit = Some(comment_list::Edit::new(0, v.file_id, v.comments.list.get(1).unwrap(), Field::Type, v.selection()));
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.type_items.len(), 4, "shown for the picture");
        let at = item_at(&ctx, &mut app, "Marker swap");
        tap(&ctx, &mut app, at);
        assert_eq!(type_of(&app, 1), "Jitter");
        assert!(app.comment_writer.is_none());
        assert_eq!(std::fs::read(dir.join("a.takm")).unwrap(), before, "nothing written");
    }

    #[test]
    fn settings_saved_before_the_comments_pane_still_load_with_folders_and_layout() {
        // v0.16.0 text: no COMMENTS pane in the layout (it is not in the first arrangement).
        let mut layout = dock::default_layout();
        dock::add_view(&mut layout, 1, dock::NOMINAL_AREA);
        let s = Settings { workspace_roots: vec![r"C:\data".into()], layout: Some(layout), layout_version: dock::LAYOUT_VERSION, ..Settings::default() };
        let text = ron::to_string(&s).unwrap();
        assert!(!text.contains("Comments") && text.contains("Devices"), "{text}");
        let mut st = FileLike::default();
        eframe::set_value(&mut st, eframe::APP_KEY, &s);
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, Some(&st), vec![], None, None);
        app.hold_loads = true;
        assert_eq!(app.workspace.roots(), &[PathBuf::from(r"C:\data")], "saved folders kept");
        for pane in [Pane::Explorer, Pane::Labeling, Pane::EditTools, Pane::Selected, Pane::Devices] {
            assert!(dock::has(&app.dock, pane), "layout kept: {pane:?}");
        }
        assert!(!dock::has(&app.dock, Pane::Comments));
        // Opened from the menu: a tab next to the selected marker; saved and read back.
        app.show_pane(Pane::Comments);
        app.sync_settings();
        let again: Settings = ron::from_str(&ron::to_string(&app.settings).unwrap()).unwrap();
        assert!(dock::has(again.layout.as_ref().unwrap(), Pane::Comments));
        // The four-view limit and shared-file rule do not depend on the new pane.
        for k in 1..6 {
            app.open_side(&ctx, missing(k));
        }
        assert_eq!(app.open_views().len(), MAX_VIEWS);
        app.open(&ctx, missing(2));
        let files: Vec<_> = app.open_views().iter().filter_map(|&k| app.views[k].target().map(Path::to_path_buf)).collect();
        assert_eq!(files.len(), 4);
        assert_eq!(files.iter().filter(|path| **path == missing(2)).count(), 2, "the same file may occupy two views");
    }

    #[test]
    fn command_runs_read_comment_files_but_never_write_them() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("command_run");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let before = std::fs::read(dir.join("a.takm")).unwrap();
        let mut app = comment_app(&ctx, &dir, true);
        assert_eq!(ids(&app), vec![1, 2], "read, so --screenshot shows the blocks");
        app.views[0].select(0, false);
        assert!(app.comment_ready(0).unwrap_err().contains("명령 실행"));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        app.delete_comment(&ctx, 0, 1);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(ids(&app), vec![1, 2]);
        assert!(app.comment_writer.is_none());
        assert_eq!(std::fs::read(dir.join("a.takm")).unwrap(), before);
        // A screenshot run is a command run too.
        let shot = AutoShot { out: PathBuf::from("x.png"), frame: None, select: None, plate: None, range: None, menu_capture: false, edit_tools: false, edit_pattern: false, edit_layout: 0, comments: false, pick_comment: None, edit_comment: None, type_strip: None, overlays: vec![], hide: vec![], hover: None, stage: 0 };
        assert!(TakViewerApp::build(&ctx, None, vec![], Some(shot), None).no_persist);
    }

    #[test]
    fn the_status_bar_counts_comments_and_says_when_a_save_failed() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("status");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let mut app = comment_app(&ctx, &dir, false);
        let now = || ctx.input(|i| i.time);
        assert_eq!(app.comment_status(0, now()).map(|s| s.0), Some("Comments 2".to_owned()));
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, "Comments 2").len(), 1, "painted in the status bar");
        // A write not confirmed yet.
        app.views[0].comments.pending = 1;
        assert_eq!(app.comment_status(0, now()).map(|s| s.0), Some("Comments 2 · saving…".to_owned()));
        app.views[0].comments.pending = 0;
        // Saving fails (the folder is gone): the comment stays, the status bar says so.
        app.views[0].comments.target = Some(Ok(dir.join("gone").join("a.takm")));
        app.views[0].select(0, false);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        until_saved(&ctx, &mut app);
        let (text, tip, warn) = app.comment_status(0, now()).unwrap();
        assert_eq!((text.as_str(), warn), ("Comments 3 · 저장 못 함", true));
        assert!(tip.contains("저장하지 못했어요"), "{tip}");
        assert_eq!(ids(&app), vec![1, 2, 3], "the comment stays on screen");
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, "⚠ Comments 3 · 저장 못 함").len(), 1);
        // While the comment file is read there is no item yet.
        app.views[0].comments.target = None;
        assert!(app.comment_status(0, now()).is_none());
    }

    #[test]
    fn the_block_layout_is_made_again_only_when_the_comments_change() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("layout");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        let mut app = comment_app(&ctx, &dir, false);
        let builds = app.views[0].comments.blocks.builds;
        assert!(builds >= 1);
        for _ in 0..10 {
            frame_with(&ctx, &mut app, vec![]);
        }
        app.views[0].pb.set_frame(5, 0.0);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].comments.blocks.builds, builds, "frames and a frame change reuse it");
        app.views[0].select(0, false);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        until_saved(&ctx, &mut app);
        assert_eq!(app.views[0].comments.blocks.builds, builds + 1, "a new comment: once more");
    }

    /// Center of a painted text inside `area`.
    fn text_in(ctx: &egui::Context, app: &mut TakViewerApp, text: &str, area: egui::Rect) -> egui::Pos2 {
        let out = frame_with(ctx, app, vec![]);
        text_rects(&out, text).into_iter().map(|t| t.center()).find(|c| area.contains(*c)).unwrap_or_else(|| panic!("{text} not painted in {area:?}"))
    }

    #[test]
    fn a_name_tag_dropped_near_a_side_of_another_view_goes_there() {
        // VS Code way: let go near the left edge of view 0 (not on a small button).
        let ctx = egui::Context::default();
        let mut app = two_views(&ctx);
        let p0 = place_of(&app, Pane::View(0));
        let p1 = place_of(&app, Pane::View(1));
        let from = text_in(&ctx, &mut app, "b.tak", egui::Rect::from_min_size(p1.min, egui::vec2(p1.width(), dock::TAB_H)));
        drag(&ctx, &mut app, from, egui::pos2(p0.left() + 0.08 * p0.width(), p0.center().y));
        frame_with(&ctx, &mut app, vec![]);
        let (r0, r1) = (place_of(&app, Pane::View(0)), place_of(&app, Pane::View(1)));
        assert!(r1.right() <= r0.left() + 1.0, "view 1 moved left of view 0: {r0:?} {r1:?}");
    }

    #[test]
    fn an_explorer_file_dropped_on_a_view_opens_there_or_beside_it() {
        use crate::workspace::{Listing, WsFile};
        let dir = PathBuf::from("no_such_dir_tak_viewer_test");
        let file = |name: &str| WsFile { path: dir.join(name), name: name.into(), size: 0 };
        let with_folder = |ctx: &egui::Context| {
            let mut app = two_views(ctx);
            let listing = Listing::Ready { dirs: vec![], files: vec![file("z.tak"), file("y.tak")] };
            app.workspace = Workspace::from_listings(vec![dir.clone()], std::collections::HashMap::from([(dir.clone(), listing)]));
            app.explorer.expand(&mut app.workspace, &dir);
            frame_with(ctx, &mut app, vec![]);
            app
        };
        // Near the right edge of view 0: a new view between view 0 and view 1.
        let ctx = egui::Context::default();
        let mut app = with_folder(&ctx);
        let explorer = place_of(&app, Pane::Explorer);
        let p0 = place_of(&app, Pane::View(0));
        let from = text_in(&ctx, &mut app, "z.tak", explorer);
        drag(&ctx, &mut app, from, egui::pos2(p0.right() - 0.06 * p0.width(), p0.center().y));
        let mut shown = dock::views(&app.dock);
        shown.sort();
        assert_eq!(shown, vec![0, 1, 2], "a third view");
        assert_eq!(app.views[2].target(), Some(dir.join("z.tak").as_path()));
        assert_eq!(app.active, 2);
        let rects = dock::view_rects(&app.dock, dock::NOMINAL_AREA);
        let r = |k: u8| rects.iter().find(|(v, _)| *v == k).unwrap().1;
        assert!(r(2).left() >= r(0).right() - 0.5 && r(2).right() <= r(1).left() + 0.5, "right of view 0, left of view 1: {rects:?}");
        assert!(egui::DragAndDrop::payload::<explorer::FileDrag>(&ctx).is_none(), "drag over");

        // In the middle of view 1: view 1 shows the file, no new view.
        let ctx = egui::Context::default();
        let mut app = with_folder(&ctx);
        let explorer = place_of(&app, Pane::Explorer);
        let p1 = place_of(&app, Pane::View(1));
        let from = text_in(&ctx, &mut app, "y.tak", explorer);
        drag(&ctx, &mut app, from, p1.center());
        let mut shown = dock::views(&app.dock);
        shown.sort();
        assert_eq!(shown, vec![0, 1]);
        assert_eq!(app.views[1].target(), Some(dir.join("y.tak").as_path()));
        assert_eq!(app.views[0].path(), Some(Path::new("a.tak")), "view 0 keeps its file");
        assert_eq!(app.active, 1);
    }

    // ── `.takm` in the window (sub-task 2): every place that opens a `.tak` opens a `.takm` too ──

    /// A folder (not under any repo's `input/`) with a TAK, its `.takm`, another `.takm` in
    /// capitals and files the viewer does not open. Contents are not real takes: the tests hand
    /// the view a sample (`takm_arrive`).
    fn takm_dir(name: &str) -> (tak_core::test_support::TempDir, Vec<PathBuf>) {
        let dir = tak_core::test_support::TempDir::new(&format!("tak_viewer_takm_{name}"));
        let f = take_files(&dir, &["a.tak", "a.takm", "b.TAKM", "notes.md", "a.takm.part"]);
        (dir, f)
    }

    /// View `k`'s held load gets its markers (a sample, 10 frames) and two frames run.
    fn takm_arrive(ctx: &egui::Context, app: &mut TakViewerApp, k: usize) {
        let p = app.views[k].loading.as_ref().expect("a file is being read").0.clone();
        let tx = app.views[k].held.take().expect("a held load (`hold_loads`)");
        tx.send(views::LoadMsg::Markers(Ok((views::sample_data(p.to_str().unwrap(), &["M", "N"]), vec![])))).unwrap();
        frame_with(ctx, app, vec![]);
        frame_with(ctx, app, vec![]);
    }

    /// Frames until the Explorer has read its folders (the listing runs on its own thread).
    fn until_listed(ctx: &egui::Context, app: &mut TakViewerApp) {
        for _ in 0..500 {
            frame_with(ctx, app, vec![]);
            if !app.workspace.is_loading() {
                frame_with(ctx, app, vec![]);
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("folder not listed");
    }

    #[test]
    fn the_explorer_lists_takm_next_to_tak_opens_it_by_click_and_page_keys_step_over_both() {
        let ctx = egui::Context::default();
        let (_dir_guard, f) = takm_dir("explorer");
        let dir = _dir_guard.to_path_buf();
        let (tak, takm, big) = (f[0].clone(), f[1].clone(), f[2].clone());
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.hold_loads = true;
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.open_workspace(dir.clone()));
        until_listed(&ctx, &mut app);
        // Both kinds as rows with their full names; other endings not listed.
        let explorer = place_of(&app, Pane::Explorer);
        let out = frame_with(&ctx, &mut app, vec![]);
        for name in ["a.tak", "a.takm", "b.TAKM"] {
            assert!(text_rects(&out, name).iter().any(|r| explorer.contains(r.center())), "{name} listed");
        }
        for name in ["notes.md", "a.takm.part"] {
            assert!(text_rects(&out, name).is_empty(), "{name} not listed");
        }
        let names: Vec<&str> = app.workspace.siblings(&tak).unwrap().iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["a.tak", "a.takm", "b.TAKM"], "name order");
        // A click on the .takm row opens it in the active view.
        let at = text_in(&ctx, &mut app, "a.takm", explorer);
        click_at(&ctx, &mut app, at);
        assert_eq!(app.views[0].target(), Some(takm.as_path()));
        takm_arrive(&ctx, &mut app, 0);
        assert_eq!(app.views[0].path(), Some(takm.as_path()));
        // PageDown / PageUp step over both kinds in name order, and stop at the ends.
        for (key, want) in [(Key::PageDown, &big), (Key::PageDown, &big), (Key::PageUp, &takm), (Key::PageUp, &tak), (Key::PageUp, &tak)] {
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, key)]);
            if app.views[0].loading.is_some() {
                takm_arrive(&ctx, &mut app, 0);
            }
            assert_eq!(app.views[0].path(), Some(want.as_path()), "{key:?}");
        }
        // The TAK and its .takm are two files; another reference opens in the active view.
        app.open_side(&ctx, takm.clone());
        frame_with(&ctx, &mut app, vec![]);
        takm_arrive(&ctx, &mut app, 1);
        assert_eq!((app.views[0].path(), app.views[1].path()), (Some(tak.as_path()), Some(takm.as_path())));
        app.activate(0);
        app.open(&ctx, takm.clone());
        assert_eq!((app.active, app.open_views()), (0, vec![0, 1]));
        assert_eq!((app.views[0].path(), app.views[1].path()), (Some(takm.as_path()), Some(takm.as_path())));
        assert_eq!(app.views[0].document_id(), app.views[1].document_id());
        assert!(std::sync::Arc::ptr_eq(app.views[0].data.as_ref().unwrap(), app.views[1].data.as_ref().unwrap()));
    }

    #[test]
    fn a_takm_dropped_from_the_desktop_opens_and_its_tak_beside_it() {
        let ctx = egui::Context::default();
        let (_dir_guard, f) = takm_dir("drop");
        let dir = _dir_guard.to_path_buf();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.hold_loads = true;
        frame_with(&ctx, &mut app, vec![]);
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 960.0));
        let dropped = [&f[1], &f[0]].map(|p| egui::DroppedFile { path: Some(p.clone()), ..Default::default() }).to_vec();
        let _ = ctx.run(egui::RawInput { screen_rect: Some(screen), focused: true, dropped_files: dropped, ..Default::default() }, |ctx| app.frame(ctx));
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.open_views(), vec![0, 1]);
        assert_eq!((app.views[0].target(), app.views[1].target()), (Some(f[1].as_path()), Some(f[0].as_path())), "first dropped: the .takm, next to it the .tak");
    }

    #[test]
    fn a_takm_lays_over_its_tak_and_a_tak_over_a_takm() {
        let ctx = egui::Context::default();
        let (_dir_guard, f) = takm_dir("overlay");
        let dir = _dir_guard.to_path_buf();
        let (tak, takm) = (f[0].clone(), f[1].clone());
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.hold_loads = true;
        frame_with(&ctx, &mut app, vec![]);
        app.open(&ctx, tak.clone());
        frame_with(&ctx, &mut app, vec![]);
        takm_arrive(&ctx, &mut app, 0);
        lay_over(&ctx, &mut app, &takm);
        overlay_arrive(&ctx, &mut app, 0);
        assert_eq!((app.views[0].path(), app.views[0].overlay_path()), (Some(tak.as_path()), Some(takm.as_path())));
        // The other way round: the .takm shown, the TAK laid over it.
        app.open(&ctx, f[2].clone());
        frame_with(&ctx, &mut app, vec![]);
        takm_arrive(&ctx, &mut app, 0);
        lay_over(&ctx, &mut app, &tak);
        overlay_arrive(&ctx, &mut app, 0);
        assert_eq!((app.views[0].path(), app.views[0].overlay_path()), (Some(f[2].as_path()), Some(tak.as_path())));
    }

    #[test]
    fn saved_views_and_spots_of_takm_files_come_back() {
        let (_dir_guard, f) = takm_dir("restore");
        let dir = _dir_guard.to_path_buf();
        let (tak, takm) = (f[0].clone(), f[1].clone());
        let mut layout = dock::default_layout();
        dock::add_view(&mut layout, 1, dock::NOMINAL_AREA);
        let spot = views::Spot {
            path: takm.to_string_lossy().into(),
            frame: 4,
            pivot: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            distance: 2.0,
            fov_y: 0.7,
            selected: vec!["M".into()],
            primary: Some("M".into()),
            plate: None,
        };
        let s = Settings {
            layout: Some(layout),
            views: vec![(0, tak.to_string_lossy().into()), (1, takm.to_string_lossy().into())],
            active_view: 1,
            file_memory: vec![spot.clone()],
            ..Settings::default()
        };
        let mut st = FileLike::default();
        eframe::set_value(&mut st, eframe::APP_KEY, &s);
        let app = TakViewerApp::build(&egui::Context::default(), Some(&st), vec![], None, None);
        assert_eq!(app.open_views(), vec![0, 1]);
        assert_eq!((app.views[0].target(), app.views[1].target()), (Some(tak.as_path()), Some(takm.as_path())));
        assert_eq!(app.views[1].restore.as_ref(), Some(&spot), "the .takm goes back to its remembered spot");
        assert!(app.views[0].restore.is_none(), "the TAK has its own spot (none here)");
        assert_eq!(app.active, 1);
    }

    #[test]
    fn the_open_dialog_shows_both_kinds_first_then_each_alone() {
        assert_eq!(OPEN_FILTERS[0].1, &["tak", "takm"][..]);
        assert_eq!((OPEN_FILTERS[1].1, OPEN_FILTERS[2].1, OPEN_FILTERS[3].1), (&["tak"][..], &["takm"][..], &["*"][..]));
        assert!(OPEN_FILTERS[0].0.contains(".tak") && OPEN_FILTERS[0].0.contains(".takm"), "the kinds are written in the filter name");
    }

    #[test]
    fn settings_with_the_old_loop_option_still_load() {
        // Up to v0.7.0 the settings kept the loop checkbox (gone: playback always loops).
        let text = ron::to_string(&Settings::default()).unwrap();
        let old = text.replacen("speed:", "looping:false,speed:", 1);
        assert!(old.contains("looping:false,speed:"), "{old}");
        let back: Settings = ron::from_str(&old).expect("settings with the loop option still load");
        assert_eq!(back.speed, 1.0);
        assert!(!ron::to_string(&back).unwrap().contains("looping"));
    }

    fn missing(k: usize) -> PathBuf {
        PathBuf::from(format!("no_such_dir_tak_viewer_test/{k}.tak"))
    }

    #[test]
    fn open_beside_fills_four_views_and_a_repeat_opens_in_the_active_view() {
        let ctx = egui::Context::default();
        let mut app = window_app();
        app.hold_loads = true;
        app.open_side(&ctx, missing(0));
        assert_eq!(app.open_views(), vec![0], "the first file goes into the empty view");
        for k in 1..4 {
            app.open_side(&ctx, missing(k));
        }
        assert_eq!(app.open_views(), vec![0, 1, 2, 3]);
        assert_eq!(app.active, 3, "the new view is the active one");
        app.open_side(&ctx, missing(4));
        assert_eq!(app.open_views().len(), MAX_VIEWS);
        assert!(app.notice.as_deref().is_some_and(|n| n.contains("4개까지")));
        app.open(&ctx, missing(1));
        assert_eq!(app.active, 3);
        assert_eq!(app.views[3].waiting_shared_path(), Some(missing(1).as_path()), "a repeat uses the existing read");
        app.open(&ctx, missing(9));
        assert_eq!(app.views[3].target(), Some(missing(9).as_path()), "a click replaces the active view's file");
        app.close_view(3);
        assert_eq!(app.open_views(), vec![0, 1, 2]);
        assert!(app.views[3].target().is_none());
        assert!(app.open_views().contains(&app.active));
    }

    #[test]
    fn replaced_file_is_remembered_and_comes_back_to_its_spot() {
        let ctx = egui::Context::default();
        let mut app = window_app();
        let v = &mut app.views[0];
        v.data = Some(std::sync::Arc::new(views::sample_data("a.tak", &["LKNE", "RKNE"])));
        v.pb = tak_core::playback::Playback::new(0, 9, 100.0);
        v.pb.set_frame(3, 0.0);
        v.select(1, false);
        v.cam.distance = 2.0;
        app.open(&ctx, PathBuf::from("b.tak"));
        let spot = views::recall(&app.settings.file_memory, Path::new("a.tak")).cloned().expect("a.tak remembered");
        assert_eq!((spot.frame, spot.distance), (3, 2.0));
        assert_eq!(spot.selected, vec!["RKNE".to_owned()]);
        app.open(&ctx, PathBuf::from("a.tak"));
        assert_eq!(app.view().restore.as_ref(), Some(&spot), "reading a.tak again goes back there");
        // Saving keeps the views and the active one.
        app.sync_settings();
        assert_eq!(app.settings.views, vec![(0, "a.tak".to_owned())]);
        assert_eq!(app.settings.active_view, 0);
    }

    #[test]
    fn command_line_runs_do_not_use_or_fill_the_memory() {
        let ctx = egui::Context::default();
        let shot = AutoShot { out: PathBuf::from("x.png"), frame: None, select: None, plate: None, range: None, menu_capture: false, edit_tools: false, edit_pattern: false, edit_layout: 0, comments: false, pick_comment: None, edit_comment: None, type_strip: None, overlays: vec![], hide: vec![], hover: None, stage: 0 };
        let mut app = TakViewerApp::build(&ctx, None, vec![], Some(shot), None);
        app.settings.file_memory = vec![views::Spot {
            path: "a.tak".into(),
            frame: 5,
            pivot: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            distance: 3.0,
            fov_y: 0.7,
            selected: vec![],
            primary: None,
            plate: None,
        }];
        app.views[0].data = Some(std::sync::Arc::new(views::sample_data("b.tak", &["X"])));
        app.open(&ctx, PathBuf::from("a.tak"));
        assert!(app.view().restore.is_none());
        assert_eq!(app.settings.file_memory.len(), 1, "b.tak not remembered");
    }

    #[test]
    fn saved_views_open_again_and_missing_files_leave_no_empty_view() {
        let _temp = tak_core::test_support::TempDir::new("tak_viewer_views");
        let dir = _temp.to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.takm"), dir.join("b.tak"));
        std::fs::write(&a, b"not a take").unwrap();
        std::fs::write(&b, b"not a take").unwrap();
        let mut layout = dock::default_layout();
        for k in 1..4 {
            dock::add_view(&mut layout, k, dock::NOMINAL_AREA);
        }
        let s = Settings {
            layout: Some(layout),
            views: vec![(0, a.to_string_lossy().into()), (2, dir.join("gone.tak").to_string_lossy().into()), (3, b.to_string_lossy().into())],
            active_view: 2,
            ..Settings::default()
        };
        let mut st = FileLike::default();
        eframe::set_value(&mut st, eframe::APP_KEY, &s);
        let app = TakViewerApp::build(&egui::Context::default(), Some(&st), vec![], None, None);
        assert_eq!(app.open_views(), vec![0, 3], "views 1 (empty) and 2 (file gone) are not made");
        assert_eq!(app.views[0].target(), Some(a.as_path()));
        assert_eq!(app.views[3].target(), Some(b.as_path()));
        assert_eq!(app.active, 0, "the active view's file was gone");
    }

    #[test]
    fn settings_with_layout_round_trip() {
        let mut s = Settings { workspace_roots: vec!["a".into()], ..Settings::default() };
        s.workspace_names.insert("a".into(), "보행 10명".into());
        s.layout = Some(dock::default_layout());
        let text = ron::to_string(&s).unwrap();
        let back: Settings = ron::from_str(&text).unwrap();
        assert_eq!(back.workspace_roots, s.workspace_roots);
        assert_eq!(back.workspace_names, s.workspace_names);
        assert_eq!(ron::to_string(&back).unwrap(), text);
    }

    // ───────────── OPEN EDITORS at the top of the Explorer (v0.26.0) ─────────────

    /// Texts painted in the Explorer above its WORKSPACE header, top to bottom, left to right:
    /// (place, text, first color).
    fn editor_texts(out: &egui::FullOutput, explorer: egui::Rect) -> Vec<(egui::Rect, String, egui::Color32)> {
        fn walk(shape: &egui::Shape, found: &mut Vec<(egui::Rect, String, egui::Color32)>) {
            match shape {
                egui::Shape::Text(t) => {
                    let color = t.galley.job.sections.first().map_or(t.fallback_color, |s| s.format.color);
                    found.push((egui::Rect::from_min_size(t.pos, t.galley.size()), t.galley.text().to_owned(), color));
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, found)),
                _ => {}
            }
        }
        let mut found = vec![];
        out.shapes.iter().for_each(|s| walk(&s.shape, &mut found));
        found.retain(|t| explorer.contains(t.0.center()));
        let ws_top = found.iter().find(|t| t.1.starts_with("WORKSPACE (")).map_or(f32::INFINITY, |t| t.0.top());
        // (Not the pane's tab title "EXPLORER" above the section.)
        let top = found.iter().find(|t| t.1.starts_with("OPEN EDITORS (")).map_or(f32::NEG_INFINITY, |t| t.0.top());
        found.retain(|t| t.0.center().y < ws_top && t.0.top() >= top);
        found.sort_by(|a, b| (a.0.center().y.round(), a.0.left()).partial_cmp(&(b.0.center().y.round(), b.0.left())).unwrap());
        found
    }

    include!("app_editor_batch_tests.rs");

    fn editor_lines(ctx: &egui::Context, app: &mut TakViewerApp) -> Vec<String> {
        let explorer = place_of(app, Pane::Explorer);
        let out = frame_with(ctx, app, vec![]);
        editor_texts(&out, explorer).into_iter().map(|t| t.1).filter(|text| !text.starts_with("선택 ") && !text.starts_with("Ctrl+A 모두")).collect()
    }

    /// Where an OPEN EDITORS text is (its middle).
    fn editor_at(ctx: &egui::Context, app: &mut TakViewerApp, text: &str) -> egui::Pos2 {
        let explorer = place_of(app, Pane::Explorer);
        let out = frame_with(ctx, app, vec![]);
        editor_texts(&out, explorer).into_iter().find(|t| t.1 == text).unwrap_or_else(|| panic!("{text} not in OPEN EDITORS")).0.center()
    }

    /// The × of the OPEN EDITORS row with this text.
    fn editor_x(ctx: &egui::Context, app: &mut TakViewerApp, text: &str) -> egui::Pos2 {
        egui::pos2(place_of(app, Pane::Explorer).left() + 26.0, editor_at(ctx, app, text).y)
    }

    /// a.tak in view 0 with b.tak laid over it, c.tak in view 1 (the active one), all arrived.
    fn editors_app(ctx: &egui::Context, dir: &Path) -> (TakViewerApp, Vec<PathBuf>) {
        let (mut app, f) = overlay_app(ctx, dir, &["a.tak", "b.tak", "c.tak"]);
        lay_over(ctx, &mut app, &f[1]);
        overlay_arrive(ctx, &mut app, 0);
        app.open_side(ctx, f[2].clone());
        frame_with(ctx, &mut app, vec![]);
        arrive(ctx, &mut app, 1);
        (app, f)
    }

    #[test]
    fn open_editors_lists_every_open_file_by_view_with_the_laid_over_ones() {
        use crate::ui::open_editors::{ADD_FOLDER, CHOSEN, CHOSEN_VIEW};
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("editors_list");
        let dir = _dir_guard.to_path_buf();
        // The long temp folder name does not fit beside the name and `폴더 추가`, so the row
        // leaves it out (v0.27.1: the folder goes first, the name stays whole).
        let row = |name: &str| name.to_owned();
        let (mut app, f) = overlay_app(&ctx, &dir, &["a.tak", "b.tak", "c.tak"]);
        // Even one view keeps its whole-layout tab/group title and active-view label.
        assert_eq!(editor_lines(&ctx, &mut app), vec!["OPEN EDITORS (1)".to_owned(), "tab1 · View 1".into(), CHOSEN_VIEW.into(), row("a.tak"), ADD_FOLDER.into()]);
        // A laid-over file is listed at once, while it is read.
        lay_over(&ctx, &mut app, &f[1]);
        assert_eq!(editor_lines(&ctx, &mut app), vec!["OPEN EDITORS (2)".to_owned(), "tab1 · View 1".into(), CHOSEN_VIEW.into(), row("a.tak"), ADD_FOLDER.into(), row("b.tak …"), ADD_FOLDER.into()]);
        overlay_arrive(&ctx, &mut app, 0);
        // A file opened beside it: listed at once too, then two groups in screen order.
        app.open_side(&ctx, f[2].clone());
        assert!(editor_lines(&ctx, &mut app).contains(&row("c.tak …")));
        arrive(&ctx, &mut app, 1);
        assert_eq!(app.active, 1);
        let want = |chosen: [bool; 2]| {
            let mut w = vec!["OPEN EDITORS (3)".to_owned(), "tab1 · View 1".into()];
            if chosen[0] {
                w.push(CHOSEN_VIEW.into());
            }
            w.extend([row("a.tak"), ADD_FOLDER.into(), row("b.tak"), ADD_FOLDER.into(), "tab1 · View 2".into()]);
            if chosen[1] {
                w.push(CHOSEN_VIEW.into());
            }
            w.extend([row("c.tak"), ADD_FOLDER.into()]);
            w
        };
        assert_eq!(editor_lines(&ctx, &mut app), want([false, true]));
        app.activate(0);
        assert_eq!(editor_lines(&ctx, &mut app), want([true, false]));
        // The chosen view's header is blue; the marks match the name tags (dot = a, ring = b).
        let explorer = place_of(&app, Pane::Explorer);
        let out = frame_with(&ctx, &mut app, vec![]);
        let t = editor_texts(&out, explorer);
        let color = |s: &str| t.iter().find(|x| x.1 == s).unwrap().2;
        assert_eq!((color("tab1 · View 1"), color("tab1 · View 2") == CHOSEN), (CHOSEN, false));
        let y = |s: &str| t.iter().find(|x| x.1 == s).unwrap().0.center().y;
        let mark = |s: &str| circles_at(&out, egui::pos2(explorer.left() + 44.0, y(s)));
        assert_eq!((mark(&row("a.tak")), mark(&row("b.tak")), mark(&row("c.tak"))), ((1, 0), (0, 1), (0, 0)));
        // Key 2 hides b: dim, with " · 숨김" like its name tag.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num2)]);
        let out = frame_with(&ctx, &mut app, vec![]);
        let hidden = editor_texts(&out, explorer).into_iter().find(|x| x.1 == row("b.tak · 숨김")).expect("b listed as hidden");
        assert_ne!(hidden.2, color(&row("a.tak")), "dim");
    }

    #[test]
    fn open_editors_row_chooses_its_view_and_x_closes_or_takes_off_and_undo_brings_each_back() {
        use crate::ui::open_editors::EMPTY;
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("editors_x");
        let dir = _dir_guard.to_path_buf();
        // (The long temp folder name is left out of the row: no room beside `폴더 추가`.)
        let row = |name: &str| name.to_owned();
        let (mut app, f) = editors_app(&ctx, &dir);
        app.quiet();
        // A row of a view with a file laid over it chooses the view and hides / shows that file
        // (as its name tag, below); clicked twice it is shown again.
        let at = editor_at(&ctx, &mut app, &row("b.tak"));
        click_at(&ctx, &mut app, at);
        assert_eq!((app.active, dock::views(&app.dock), app.file_count(), app.views[0].shown), (0, vec![0, 1], 3, [true, false]));
        click_at(&ctx, &mut app, at);
        assert_eq!(app.views[0].shown, [true, true]);
        // A row of a view without one only chooses it.
        let at = editor_at(&ctx, &mut app, &row("c.tak"));
        let n = steps(&app, 0);
        click_at(&ctx, &mut app, at);
        assert_eq!((app.active, app.views[1].shown, steps(&app, 0)), (1, [true, true], n), "no action");
        // × on the laid-over file takes it off; Ctrl+Z puts it back.
        let x = editor_x(&ctx, &mut app, &row("b.tak"));
        click_at(&ctx, &mut app, x);
        assert_eq!((app.views[0].overlay_target(), app.views[0].target()), (None, Some(f[0].as_path())));
        assert_eq!(editor_lines(&ctx, &mut app)[0], "OPEN EDITORS (2)");
        undo_key(&ctx, &mut app);
        assert_eq!(app.views[0].overlay_target(), Some(f[1].as_path()), "{}", note(&app));
        overlay_arrive(&ctx, &mut app, 0);
        // × on a view's file closes that view (its tab ✕); Ctrl+Z opens it again.
        let x = editor_x(&ctx, &mut app, &row("c.tak"));
        click_at(&ctx, &mut app, x);
        assert_eq!(dock::views(&app.dock), vec![0]);
        assert_eq!(editor_lines(&ctx, &mut app)[0], "OPEN EDITORS (2)");
        undo_key(&ctx, &mut app);
        assert_eq!((dock::views(&app.dock), app.views[1].target()), (vec![0, 1], Some(f[2].as_path())), "{}", note(&app));
        arrive(&ctx, &mut app, 1);
        assert_eq!(editor_lines(&ctx, &mut app)[0], "OPEN EDITORS (3)");
        // The only view: × closes it and its overlay, leaving the empty editor.
        let x = editor_x(&ctx, &mut app, &row("c.tak"));
        click_at(&ctx, &mut app, x);
        let x = editor_x(&ctx, &mut app, &row("a.tak"));
        click_at(&ctx, &mut app, x);
        assert_eq!((dock::views(&app.dock), app.views[0].target(), app.file_count()), (vec![], None, 0));
        assert_eq!(editor_lines(&ctx, &mut app), vec!["OPEN EDITORS (0)", EMPTY]);
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].target(), app.views[0].overlay_target()), (Some(f[0].as_path()), Some(f[1].as_path())), "{}", note(&app));
    }

    #[test]
    fn open_editors_row_of_a_laid_over_view_hides_its_file_exactly_like_its_name_tag() {
        // Two same apps (view 0 = a.tak with b.tak laid over it, view 1 = c.tak, the active
        // one): one clicks OPEN EDITORS rows, the other the name tags in the 3D view.
        let (ctx, ctx2) = (egui::Context::default(), egui::Context::default());
        let (_dir_guard, _dir2_guard) = (comment_dir("editors_hide"), comment_dir("editors_hide_tag"));
        let (dir, dir2) = (_dir_guard.to_path_buf(), _dir2_guard.to_path_buf());
        let (mut by_row, _) = editors_app(&ctx, &dir);
        let (mut by_tag, _) = editors_app(&ctx2, &dir2);
        by_row.quiet();
        by_tag.quiet();
        let n = steps(&by_row, 0);
        let state = |app: &TakViewerApp| (app.active, app.views[0].shown, app.views[1].shown, steps(app, 0), app.history.kinds(), app.overlay_note.as_ref().map(|o| o.0.clone()));
        assert_eq!(state(&by_row), state(&by_tag));
        let hidden = |app: &TakViewerApp| app.editor_groups(&[0, 1]).iter().map(|g| g.entries.iter().map(|e| e.hidden).collect::<Vec<_>>()).collect::<Vec<_>>();
        let tag_click = |ctx: &egui::Context, app: &mut TakViewerApp, text: &str| {
            let out = frame_with(ctx, app, vec![]);
            let t = text_rects(&out, text)[0];
            click_at(ctx, app, t.center());
        };
        // Row b.tak (view 0 is not the active one) = name tag 2 of view 0: the view is chosen
        // and b's markers are hidden, one action, no status-bar note.
        let at = editor_at(&ctx, &mut by_row, "b.tak");
        click_at(&ctx, &mut by_row, at);
        tag_click(&ctx2, &mut by_tag, "2 b.tak");
        assert_eq!(state(&by_row), state(&by_tag), "the row = the name tag");
        assert_eq!((by_row.active, by_row.views[0].shown, steps(&by_row, 0)), (0, [true, false], (n.0 + 1, 0)));
        assert_eq!(by_row.overlay_note, None);
        assert_eq!(hidden(&by_row), vec![vec![false, true], vec![false]]);
        assert!(editor_lines(&ctx, &mut by_row).contains(&"b.tak · 숨김".to_owned()), "dim with · 숨김 at once");
        // One Ctrl+Z puts it back, one Ctrl+Y hides it again, with the same notes.
        for (c, app) in [(&ctx, &mut by_row), (&ctx2, &mut by_tag)] {
            undo_key(c, app);
            assert_eq!((app.views[0].shown, note(app).as_str()), ([true, true], "Undo · b.tak shown"));
            redo_key(c, app);
            assert_eq!((app.views[0].shown, note(app).as_str()), ([true, false], "Redo · b.tak hidden"));
        }
        assert_eq!(state(&by_row), state(&by_tag));
        // Row a.tak = name tag 1 (the view's own file).
        let at = editor_at(&ctx, &mut by_row, "a.tak");
        click_at(&ctx, &mut by_row, at);
        tag_click(&ctx2, &mut by_tag, "1 a.tak");
        assert_eq!(state(&by_row), state(&by_tag));
        assert_eq!((by_row.views[0].shown, steps(&by_row, 0).0), ([false, false], n.0 + 2));
        assert_eq!(hidden(&by_row), vec![vec![true, true], vec![false]]);
        undo_key(&ctx, &mut by_row);
        assert_eq!((by_row.views[0].shown, note(&by_row).as_str()), ([true, false], "Undo · a.tak shown"));
        // A file still being read over view 1 has no name tag yet: its rows only choose view 1.
        let d = take_files(&dir, &["d.tak"]).remove(0);
        by_row.activate(1);
        lay_over(&ctx, &mut by_row, &d);
        by_row.activate(0);
        by_row.quiet();
        let k = steps(&by_row, 0);
        for text in ["d.tak …", "c.tak"] {
            let at = editor_at(&ctx, &mut by_row, text);
            by_row.activate(0);
            click_at(&ctx, &mut by_row, at);
            assert_eq!((by_row.active, by_row.views[1].shown, steps(&by_row, 0)), (1, [true, true], k), "{text}");
        }
        // Once it is on screen (laying it over = one action), its row hides it like name tag 2.
        overlay_arrive(&ctx, &mut by_row, 1);
        assert_eq!(steps(&by_row, 0).0, k.0 + 1);
        by_row.activate(0);
        let at = editor_at(&ctx, &mut by_row, "d.tak");
        click_at(&ctx, &mut by_row, at);
        assert_eq!((by_row.active, by_row.views[1].shown, steps(&by_row, 0).0), (1, [true, false], k.0 + 2));
    }

    #[test]
    fn open_editors_add_folder_only_outside_workspace_adds_it_with_undo_and_says_why_when_full() {
        use crate::ui::open_editors::{ADD_FOLDER, DIM};
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("editors_add");
        let dir = _dir_guard.to_path_buf();
        let (mut app, f) = overlay_app(&ctx, &dir, &["a.tak"]);
        app.quiet();
        assert!(editor_lines(&ctx, &mut app).contains(&ADD_FOLDER.to_owned()));
        let at = editor_at(&ctx, &mut app, ADD_FOLDER);
        click_at(&ctx, &mut app, at);
        assert_eq!(app.workspace.roots(), &[dir.clone()]);
        until_listed(&ctx, &mut app);
        assert!(!editor_lines(&ctx, &mut app).contains(&ADD_FOLDER.to_owned()), "inside WORKSPACE now");
        // The tree shows the folder with the file in it (below the WORKSPACE header).
        let explorer = place_of(&app, Pane::Explorer);
        let out = frame_with(&ctx, &mut app, vec![]);
        let ws = text_rects(&out, &format!("WORKSPACE (1/{MAX_ROOTS})"))[0];
        assert!(text_rects(&out, "a.tak").iter().any(|r| explorer.contains(r.center()) && r.top() > ws.bottom()), "a.tak in the tree");
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Folders"), "one action, like adding a folder");
        undo_key(&ctx, &mut app);
        assert!(app.workspace.roots().is_empty());
        assert!(editor_lines(&ctx, &mut app).contains(&ADD_FOLDER.to_owned()), "outside again");
        redo_key(&ctx, &mut app);
        assert_eq!(app.workspace.roots(), &[dir.clone()]);
        undo_key(&ctx, &mut app);
        // WORKSPACE full: the button is dim and a click says why in the status bar.
        for i in 0..MAX_ROOTS {
            app.workspace.add_root(PathBuf::from(format!("/no_such_root_{i}")));
        }
        app.quiet();
        let out = frame_with(&ctx, &mut app, vec![]);
        let button = editor_texts(&out, explorer).into_iter().find(|t| t.1 == ADD_FOLDER).expect("still shown");
        assert_eq!(button.2, DIM);
        click_at(&ctx, &mut app, button.0.center());
        assert_eq!(app.workspace.roots().len(), MAX_ROOTS);
        let why = app.overlay_note.clone().expect("a reason").0;
        assert!(why.contains("다 찼어요"), "{why}");
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, &why).len(), 1, "in the status bar");
        assert_eq!(app.views[0].target(), Some(f[0].as_path()));
    }

    #[test]
    fn folding_open_editors_is_one_action_that_undo_unfolds() {
        use crate::ui::open_editors::{ADD_FOLDER, CHOSEN_VIEW};
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("editors_fold");
        let dir = _dir_guard.to_path_buf();
        let (mut app, _) = overlay_app(&ctx, &dir, &["a.tak"]);
        app.quiet();
        let shown = editor_lines(&ctx, &mut app);
        assert_eq!(shown, vec!["OPEN EDITORS (1)", "tab1 · View 1", CHOSEN_VIEW, "a.tak", ADD_FOLDER]);
        let before = steps(&app, 0).0;
        let at = editor_at(&ctx, &mut app, "OPEN EDITORS (1)");
        click_at(&ctx, &mut app, at);
        assert!(app.explorer.editors_collapsed);
        assert_eq!(editor_lines(&ctx, &mut app), vec!["OPEN EDITORS (1)"]);
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("Folders"));
        assert_eq!(steps(&app, 0), (before + 1, 0), "folding adds one action");
        let explorer = place_of(&app, Pane::Explorer);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert!(text_rects(&out, &format!("WORKSPACE (0/{MAX_ROOTS})")).iter().any(|r| explorer.contains(r.center())), "WORKSPACE stays");
        undo_key(&ctx, &mut app);
        assert!(!app.explorer.editors_collapsed, "{}", note(&app));
        assert_eq!(editor_lines(&ctx, &mut app), shown, "undo restores group, active label, file and add-folder row");
        assert_eq!(steps(&app, 0), (before, 1));
        redo_key(&ctx, &mut app);
        assert!(app.explorer.editors_collapsed);
        assert_eq!(editor_lines(&ctx, &mut app), vec!["OPEN EDITORS (1)"]);
        assert_eq!(steps(&app, 0), (before + 1, 0));
    }

    // ───────────── Plan step 7: a second file laid over a 3D view ─────────────

    /// Files `names` in `dir` (stubs), the first open in view 0 (held loads, it arrived).
    fn overlay_app(ctx: &egui::Context, dir: &Path, names: &[&str]) -> (TakViewerApp, Vec<PathBuf>) {
        let f = take_files(dir, names);
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        app.hold_loads = true;
        frame_with(ctx, &mut app, vec![]);
        app.open(ctx, f[0].clone());
        frame_with(ctx, &mut app, vec![]);
        arrive(ctx, &mut app, 0);
        (app, f)
    }

    /// A sample take (markers M, N, frames 0–9, 100 Hz) with its last frame and rate changed and
    /// marker N moved 5 cm along x (a marker that differs from `sample_data`).
    fn overlay_take(path: &Path, end_frame: i64, rate: f64) -> tak_core::tak::Take {
        let mut t = views::sample_take(path.to_str().unwrap(), &["M", "N"]);
        t.end_frame = end_frame;
        t.frame_rate = rate;
        for p in &mut t.trajectories[1].xyz {
            p[0] += 0.05;
        }
        t
    }

    /// The file view `k` is reading for its overlay arrives as `take`; two frames.
    fn overlay_arrive_as(ctx: &egui::Context, app: &mut TakViewerApp, k: usize, take: tak_core::tak::Take) {
        let tx = app.views[k].overlay_load.as_mut().expect("an overlay being read").held.take().expect("a held overlay load");
        tx.send(views::LoadMsg::Markers(Ok((tak_core::view_data::ViewData::build(take), vec![])))).unwrap();
        frame_with(ctx, app, vec![]);
        frame_with(ctx, app, vec![]);
    }

    /// The overlay of view `k` arrives fitting its file (10 frames, 100 Hz).
    fn overlay_arrive(ctx: &egui::Context, app: &mut TakViewerApp, k: usize) {
        let p = app.views[k].overlay_load.as_ref().expect("an overlay being read").path.clone();
        overlay_arrive_as(ctx, app, k, overlay_take(&p, 9, 100.0));
    }

    /// Lay `path` over the active view the way the Explorer menu does, then one frame.
    fn lay_over(ctx: &egui::Context, app: &mut TakViewerApp, path: &Path) {
        app.explorer_action(ctx, WsAction::Overlay(path.to_path_buf()));
        frame_with(ctx, app, vec![]);
    }

    /// (filled circles, white rings) painted around `at` (within 1 pt of the center).
    fn circles_at(out: &egui::FullOutput, at: egui::Pos2) -> (usize, usize) {
        fn walk(shape: &egui::Shape, at: egui::Pos2, found: &mut (usize, usize)) {
            match shape {
                egui::Shape::Circle(c) if c.center.distance(at) < 1.0 => {
                    if c.fill != egui::Color32::TRANSPARENT {
                        found.0 += 1;
                    } else if c.stroke.color == theme::OVERLAY_RING && c.stroke.width == scene::OVERLAY_RING_W {
                        found.1 += 1;
                    }
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, at, found)),
                _ => {}
            }
        }
        let mut found = (0, 0);
        out.shapes.iter().for_each(|s| walk(&s.shape, at, &mut found));
        found
    }

    /// Where marker `m` of view `k`'s overlay is drawn now.
    fn ring_on_screen(app: &TakViewerApp, k: usize, m: usize) -> egui::Pos2 {
        let v = &app.views[k];
        let o = v.overlay.as_ref().expect("an overlay");
        v.cam.view(v.scene_rect).project(o.data.markers[m].position(v.overlay_frame().unwrap()).unwrap()).unwrap().0
    }

    /// Where marker `m` of view `k`'s own file is drawn now (no pick check).
    fn dot_on_screen(app: &TakViewerApp, k: usize, m: usize) -> egui::Pos2 {
        let v = &app.views[k];
        let d = v.data.as_ref().unwrap();
        v.cam.view(v.scene_rect).project(d.markers[m].position(v.pb.frame).unwrap()).unwrap().0
    }

    #[test]
    fn the_explorer_lays_a_file_over_the_active_view_as_white_rings_that_follow_its_frame() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_on");
        let dir = _dir_guard.to_path_buf();
        let (mut app, f) = overlay_app(&ctx, &dir, &["a.tak", "b.tak"]);
        let before = app.history.kinds();
        lay_over(&ctx, &mut app, &f[1]);
        assert_eq!(app.views[0].overlay_target(), Some(f[1].as_path()), "being read on its own (background) load");
        assert!(app.views[0].overlay.is_none() && app.history.kinds() == before, "not taken before it is on screen");
        overlay_arrive(&ctx, &mut app, 0);
        assert_eq!(app.views[0].overlay_path(), Some(f[1].as_path()));
        assert_eq!((app.views[0].shown, app.file_count()), ([true, true], 2));
        let mut kinds = before.clone();
        kinds.push("Layout".into());
        assert_eq!(app.history.kinds(), kinds, "laying a file over is one action");
        // The same frame as the view, whatever the view does; M sits on the dot, N (moved in b)
        // stands apart.
        for frame in [3, 7] {
            app.views[0].pb.set_frame(frame, 0.0);
            let out = frame_with(&ctx, &mut app, vec![]);
            assert_eq!(app.views[0].overlay_frame(), Some(frame));
            let (m_dot, m_ring) = (dot_on_screen(&app, 0, 0), ring_on_screen(&app, 0, 0));
            assert!(m_dot.distance(m_ring) < 0.5, "same place in both files");
            assert_eq!(circles_at(&out, m_dot), (1, 1), "a dot with a white ring on it");
            let (n_dot, n_ring) = (dot_on_screen(&app, 0, 1), ring_on_screen(&app, 0, 1));
            assert!(n_dot.distance(n_ring) > 5.0, "the moved marker stands apart");
            assert_eq!((circles_at(&out, n_dot), circles_at(&out, n_ring)), ((1, 0), (0, 1)));
        }
        // The two name tags, next to the file name tag.
        let out = frame_with(&ctx, &mut app, vec![]);
        let (t1, t2) = (text_rects(&out, "1 a.tak"), text_rects(&out, "2 b.tak"));
        assert!(t1.len() == 1 && t2.len() == 1 && t1[0].right() < t2[0].left(), "{t1:?} {t2:?}");
        assert!(t1[0].top() < app.views[0].scene_rect.top() + dock::TAB_H, "in the tab row of a view alone");
        // Undo / redo of laying it over (the frame moves above are one action of their own).
        undo_key(&ctx, &mut app);
        assert_eq!(note(&app), "Undo · Frame 0");
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].overlay_target(), note(&app).as_str()), (None, "Undo · Overlay off"));
        redo_key(&ctx, &mut app);
        assert_eq!((app.views[0].overlay_target(), note(&app).as_str()), (Some(f[1].as_path()), "Redo · Overlay b.tak"));
        overlay_arrive(&ctx, &mut app, 0);
        assert_eq!((app.views[0].overlay_path(), steps(&app, 0)), (Some(f[1].as_path()), (kinds.len(), 1)), "its arrival is no action");
    }

    #[test]
    fn a_file_of_another_frame_count_or_rate_is_refused_with_both_numbers() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_refused");
        let dir = _dir_guard.to_path_buf();
        let (mut app, f) = overlay_app(&ctx, &dir, &["a.tak", "b.tak", "c.tak"]);
        let before = app.history.kinds();
        lay_over(&ctx, &mut app, &f[1]);
        overlay_arrive_as(&ctx, &mut app, 0, overlay_take(&f[1], 19, 100.0));
        assert!(app.views[0].overlay.is_none() && app.views[0].overlay_load.is_none());
        let note = app.overlay_note.clone().expect("a reason in the status bar").0;
        assert_eq!(note, "얹을 수 없어요: 프레임 수가 달라요 (a.tak 10개, b.tak 20개)");
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, &note).len(), 1, "shown in the status bar");
        lay_over(&ctx, &mut app, &f[2]);
        overlay_arrive_as(&ctx, &mut app, 0, overlay_take(&f[2], 9, 120.0));
        assert!(app.views[0].overlay.is_none());
        assert_eq!(app.overlay_note.clone().unwrap().0, "얹을 수 없어요: 프레임 속도가 달라요 (a.tak 100 Hz, c.tak 120 Hz)");
        assert_eq!(app.history.kinds(), before, "a refused file is no action");
        assert_eq!(app.file_count(), 1);
    }

    #[test]
    fn repeated_views_and_overlays_share_one_document_and_self_overlay_is_refused() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_twice");
        let dir = _dir_guard.to_path_buf();
        let (mut app, f) = overlay_app(&ctx, &dir, &["a.tak", "b.tak", "c.tak"]);
        app.open_side(&ctx, f[1].clone());
        frame_with(&ctx, &mut app, vec![]);
        arrive_all(&ctx, &mut app);
        let primary_data = app.views[1].data.clone().unwrap();
        let primary_document = app.views[1].edit_document.clone().unwrap();
        app.activate(0);
        lay_over(&ctx, &mut app, &f[0]);
        assert!(app.views[0].overlay.is_none() && app.views[0].overlay_load.is_none());
        assert!(app.overlay_note.take().unwrap().0.contains("이 화면에 열린 파일"));

        // A primary in another view supplies this overlay without another reader.
        lay_over(&ctx, &mut app, &f[1]);
        assert!(app.views[0].overlay_load.is_none());
        assert!(std::sync::Arc::ptr_eq(&primary_data, &app.views[0].overlay.as_ref().unwrap().data));
        assert!(std::sync::Arc::ptr_eq(&primary_document,
            app.views[0].overlay_owner().unwrap().edit_document.as_ref().unwrap()));

        // An overlay can in turn supply other overlays and ordinary primary views.
        lay_over(&ctx, &mut app, &f[2]);
        overlay_arrive(&ctx, &mut app, 0);
        let shared_data = app.views[0].overlay.as_ref().unwrap().data.clone();
        let shared_document = app.views[0].overlay_owner().unwrap().edit_document.clone().unwrap();
        lay_over(&ctx, &mut app, &f[2]);
        assert!(app.views[0].overlay_load.is_none());
        assert!(app.overlay_note.take().unwrap().0.contains("이미 이 화면에 얹혀"));
        app.activate(1);
        lay_over(&ctx, &mut app, &f[2]);
        assert!(app.views[1].overlay_load.is_none());
        assert!(std::sync::Arc::ptr_eq(&shared_data, &app.views[1].overlay.as_ref().unwrap().data));
        assert!(std::sync::Arc::ptr_eq(&shared_document, app.views[1].overlay_owner().unwrap().edit_document.as_ref().unwrap()));

        app.open_side(&ctx, f[2].clone());
        assert_eq!((app.active, app.open_views()), (2, vec![0, 1, 2]));
        assert!(app.views[2].loading.is_none());
        assert!(std::sync::Arc::ptr_eq(&shared_data, app.views[2].data.as_ref().unwrap()));
        assert!(std::sync::Arc::ptr_eq(&shared_document, app.views[2].edit_document.as_ref().unwrap()));
        app.open_side(&ctx, f[1].clone());
        assert_eq!((app.active, app.open_views()), (3, vec![0, 1, 2, 3]));
        assert!(app.views[3].loading.is_none());
        assert!(std::sync::Arc::ptr_eq(&primary_data, app.views[3].data.as_ref().unwrap()));
        assert!(std::sync::Arc::ptr_eq(&primary_document, app.views[3].edit_document.as_ref().unwrap()));
    }

    #[test]
    fn four_views_with_overlays_obey_the_app_wide_twelve_distinct_file_limit() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_four");
        let dir = _dir_guard.to_path_buf();
        let names: Vec<String> = (0..13).map(|i| format!("source_{i}.tak")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (mut app, f) = overlay_app(&ctx, &dir, &names);
        let first_tab = app.layout_tabs.active_id().unwrap();
        for p in &f[1..3] {
            app.open_side(&ctx, p.clone());
            frame_with(&ctx, &mut app, vec![]);
            arrive_all(&ctx, &mut app);
        }
        app.activate(0);
        lay_over(&ctx, &mut app, &f[3]);
        overlay_arrive(&ctx, &mut app, 0);
        let overlay_data = app.views[0].overlay.as_ref().unwrap().data.clone();
        let overlay_document = app.views[0].overlay_owner().unwrap().edit_document.clone().unwrap();

        // An overlay uses a file reference, not one of the four 3D view slots.
        app.open_side(&ctx, f[4].clone());
        frame_with(&ctx, &mut app, vec![]);
        arrive_all(&ctx, &mut app);
        assert_eq!(app.open_views(), vec![0, 1, 2, 3]);
        for (slot, path) in (1..4).zip(&f[5..8]) {
            app.activate(slot);
            lay_over(&ctx, &mut app, path);
            overlay_arrive(&ctx, &mut app, slot);
        }
        assert_eq!(app.file_count(), 8, "all four views keep their four overlay references");
        app.open_side(&ctx, f[0].clone());
        assert_eq!(app.open_views(), vec![0, 1, 2, 3]);
        assert!(app.notice.take().is_some_and(|n| n.contains("4개까지")));

        // Hidden pending primary reads also reserve distinct sources, bringing the total to 12.
        app.new_layout_tab();
        for (i, path) in f[8..12].iter().enumerate() {
            if i == 0 { app.open(&ctx, path.clone()); } else { app.open_side(&ctx, path.clone()); }
        }
        assert_eq!(app.open_views(), vec![0, 1, 2, 3]);
        assert!(app.views.iter().all(|view| view.loading.is_some()));
        app.new_layout_tab();
        app.open(&ctx, f[12].clone());
        assert!(app.open_views().is_empty(), "the thirteenth source must not create a view");
        assert!(app.notice.take().is_some_and(|n| n.contains("12개까지")));

        // Repeated sources still open at the cap, including a ready overlay and a pending read.
        app.open(&ctx, f[3].clone());
        assert!(std::sync::Arc::ptr_eq(&overlay_data, app.views[0].data.as_ref().unwrap()));
        assert!(std::sync::Arc::ptr_eq(&overlay_document, app.views[0].edit_document.as_ref().unwrap()));
        app.open_side(&ctx, f[8].clone());
        assert_eq!(app.views[1].waiting_shared_path(), Some(f[8].as_path()));
        app.activate(0);
        lay_over(&ctx, &mut app, &f[12]);
        assert!(app.views[0].overlay_load.is_none() && app.views[0].overlay.is_none());
        assert!(app.notice.take().is_some_and(|n| n.contains("12개까지")));
        app.switch_layout_tab(first_tab);
        assert_eq!(app.open_views(), vec![0, 1, 2, 3]);
        assert_eq!(app.file_count(), 8, "refused opens leave existing views and overlays intact");
        assert!(std::sync::Arc::ptr_eq(&overlay_data, &app.views[0].overlay.as_ref().unwrap().data));
    }

    #[test]
    fn name_tags_and_keys_1_2_hide_and_show_each_file_and_each_is_one_action() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_tags");
        let dir = _dir_guard.to_path_buf();
        let (mut app, f) = overlay_app(&ctx, &dir, &["a.tak", "b.tak"]);
        lay_over(&ctx, &mut app, &f[1]);
        overlay_arrive(&ctx, &mut app, 0);
        let n = steps(&app, 0).0;
        // Tag 1: the first file's markers go; the tag stays, dim, saying so.
        let out = frame_with(&ctx, &mut app, vec![]);
        let t1 = text_rects(&out, "1 a.tak")[0];
        let out = click_at(&ctx, &mut app, t1.center());
        assert_eq!(app.views[0].shown, [false, true]);
        assert_eq!(text_rects(&out, "1 a.tak · 숨김").len(), 1);
        let out = frame_with(&ctx, &mut app, vec![]);
        let n_dot = dot_on_screen(&app, 0, 1);
        assert_eq!(circles_at(&out, n_dot), (0, 0), "no dot of the hidden file");
        assert_eq!(circles_at(&out, ring_on_screen(&app, 0, 1)), (0, 1), "the rings stay");
        // Click on it again: back.
        idle(&ctx, &mut app);
        let t1 = text_rects(&out, "1 a.tak · 숨김")[0];
        click_at(&ctx, &mut app, t1.center());
        assert_eq!(app.views[0].shown, [true, true]);
        // Keys: 2 hides the rings, 1 the dots.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num2)]);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].shown, [true, false]);
        assert_eq!(circles_at(&out, ring_on_screen(&app, 0, 1)), (0, 0));
        assert_eq!(text_rects(&out, "2 b.tak · 숨김").len(), 1);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num1)]);
        assert_eq!(app.views[0].shown, [false, false]);
        assert_eq!(steps(&app, 0).0, n + 4, "each hide / show is one action");
        // Undo / redo one by one.
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].shown, note(&app).as_str()), ([true, false], "Undo · a.tak shown"));
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].shown, note(&app).as_str()), ([true, true], "Undo · b.tak shown"));
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].shown, note(&app).as_str()), ([false, true], "Undo · a.tak hidden"));
        redo_key(&ctx, &mut app);
        redo_key(&ctx, &mut app);
        assert_eq!((app.views[0].shown, note(&app).as_str()), ([true, false], "Redo · b.tak hidden"));
        // Without an overlay the keys do nothing.
        app.overlay_off(0);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].shown, [true, true], "taking it off shows the first file again");
        let k = steps(&app, 0).0;
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num1)]);
        assert_eq!((app.views[0].shown, steps(&app, 0).0), ([true, true], k));
    }

    #[test]
    fn the_x_on_tag_2_and_the_explorer_take_it_off_and_undo_puts_it_back_as_it_was() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_off");
        let dir = _dir_guard.to_path_buf();
        let (mut app, f) = overlay_app(&ctx, &dir, &["a.tak", "b.tak"]);
        lay_over(&ctx, &mut app, &f[1]);
        overlay_arrive(&ctx, &mut app, 0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num1)]);
        let n = steps(&app, 0).0;
        // ✕ at the right end of tag 2.
        let out = frame_with(&ctx, &mut app, vec![]);
        let t2 = text_rects(&out, "2 b.tak")[0];
        let x = egui::pos2(t2.right() + 4.0 + 10.0, t2.center().y);
        // A click on tag 2 left of the ✕ only hides; the ✕ takes it off.
        click_at(&ctx, &mut app, x);
        assert_eq!((app.views[0].overlay_target(), app.views[0].shown), (None, [true, true]));
        assert_eq!(steps(&app, 0).0, n + 1, "one action");
        let out = frame_with(&ctx, &mut app, vec![]);
        assert!(text_rects(&out, "2 b.tak").is_empty() && text_rects(&out, "1 a.tak · 숨김").is_empty());
        // Undo: b.tak read again, arriving quietly; a.tak hidden again as it was.
        undo_key(&ctx, &mut app);
        assert_eq!(note(&app), "Undo · Overlay b.tak");
        overlay_arrive(&ctx, &mut app, 0);
        assert_eq!((app.views[0].overlay_path(), app.views[0].shown), (Some(f[1].as_path()), [false, true]));
        assert_eq!(steps(&app, 0), (n, 1), "its arrival is no action and keeps the redo side");
        redo_key(&ctx, &mut app);
        assert_eq!((app.views[0].overlay_target(), note(&app).as_str()), (None, "Redo · Overlay off"));
        undo_key(&ctx, &mut app);
        overlay_arrive(&ctx, &mut app, 0);
        // Explorer "얹은 파일 내리기".
        app.explorer_action(&ctx, WsAction::OverlayOff);
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].overlay_target(), None);
    }

    #[test]
    fn the_overlay_goes_with_its_views_file_and_comes_back_with_it() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_replace");
        let dir = _dir_guard.to_path_buf();
        let (mut app, f) = overlay_app(&ctx, &dir, &["a.tak", "b.tak", "c.tak", "d.tak"]);
        lay_over(&ctx, &mut app, &f[1]);
        overlay_arrive(&ctx, &mut app, 0);
        let n = steps(&app, 0).0;
        // Another file in the view: the overlay goes at once, one action with the open.
        app.open(&ctx, f[2].clone());
        frame_with(&ctx, &mut app, vec![]);
        assert_eq!(app.views[0].overlay_target(), None);
        arrive(&ctx, &mut app, 0);
        assert_eq!(steps(&app, 0).0, n + 1);
        undo_key(&ctx, &mut app);
        assert_eq!(note(&app), "Undo · Open a.tak · Overlay b.tak");
        assert_eq!(app.views[0].overlay_target(), Some(f[1].as_path()), "coming again");
        // The overlay waits for its view's own file (they are compared on arrival).
        let tx = app.views[0].overlay_load.as_mut().unwrap().held.take().unwrap();
        tx.send(views::LoadMsg::Markers(Ok((tak_core::view_data::ViewData::build(overlay_take(&f[1], 9, 100.0)), vec![])))).unwrap();
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.views[0].overlay.is_none() && app.views[0].overlay_load.is_some());
        arrive(&ctx, &mut app, 0);
        assert_eq!((app.views[0].path(), app.views[0].overlay_path()), (Some(f[0].as_path()), Some(f[1].as_path())));
        assert_eq!(steps(&app, 0), (n, 1));
        // A closed view takes its overlay with it.
        app.open_side(&ctx, f[3].clone());
        frame_with(&ctx, &mut app, vec![]);
        arrive_all(&ctx, &mut app);
        app.close_view(0);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.views[0].overlay_target().is_none() && app.views[0].target().is_none());
        assert_eq!(app.file_count(), 1);
    }

    #[test]
    fn rings_are_never_clicked_and_a_hidden_files_markers_neither() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_click");
        let dir = _dir_guard.to_path_buf();
        let (mut app, f) = overlay_app(&ctx, &dir, &["a.tak", "b.tak"]);
        lay_over(&ctx, &mut app, &f[1]);
        overlay_arrive(&ctx, &mut app, 0);
        frame_with(&ctx, &mut app, vec![]);
        let n = steps(&app, 0).0;
        // A click on b's ring of N (apart from a's N): nothing is selected.
        let ring = ring_on_screen(&app, 0, 1);
        assert_eq!(markers_at(&app, 0, ring), None, "no marker of a.tak there");
        click_at(&ctx, &mut app, ring);
        assert_eq!((sel(&app), steps(&app, 0).0), ((vec![], None), n));
        // a's own markers are clicked as before; hidden, they are not.
        idle(&ctx, &mut app);
        let dot = marker_on_screen(&app, 1);
        click_at(&ctx, &mut app, dot);
        assert_eq!(sel(&app), (vec![1], Some(1)));
        idle(&ctx, &mut app);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num1)]);
        idle(&ctx, &mut app);
        click_at(&ctx, &mut app, dot);
        assert_eq!(sel(&app), (vec![], None));
    }

    #[test]
    fn comments_made_with_an_overlay_say_which_files_were_shown_and_old_lines_read_the_same() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_comments");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        app.hold_loads = true;
        // Without an overlay: as before.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        until_saved(&ctx, &mut app);
        let b = dir.join("b.tak");
        lay_over(&ctx, &mut app, &b);
        overlay_arrive(&ctx, &mut app, 0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        until_saved(&ctx, &mut app);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num1)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        until_saved(&ctx, &mut app);
        // Undo of that comment writes its delete line with what is shown now.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num2)]);
        undo_key(&ctx, &mut app);
        undo_key(&ctx, &mut app);
        until_saved(&ctx, &mut app);
        let ev = file_events(&dir);
        let fa = takm_label("a.takm");
        let fa = fa.as_str();
        let cols: Vec<(String, u64, &str, &str, &str, &str)> =
            ev.iter().map(|e| (e.kind.as_str().to_owned(), e.id, e.shown_a.as_str(), e.shown_b.as_str(), e.file_a.as_str(), e.file_b.as_str())).collect();
        assert_eq!(
            cols,
            vec![
                ("add".to_owned(), 1, "1", "", fa, ""),
                ("add".to_owned(), 2, "1", "1", fa, "b.tak|0"),
                ("add".to_owned(), 3, "0", "1", fa, "b.tak|0"),
                ("delete".to_owned(), 3, "0", "1", fa, "b.tak|0"),
            ]
        );
        // The file reads back the same (no column added), old line and new lines alike.
        assert_eq!(ids(&app), vec![1, 2]);
        let text = tak_core::takm::read_tail_text(&dir.join("a.takm")).unwrap();
        assert!(text.starts_with("time,kind,comment_id,markers,frame_start,frame_end,comment_type,comment_text,shown_a,shown_b,file_a,file_b\r\n"));
        let folded = tail_read(&dir.join("a.takm")).unwrap().unwrap();
        assert_eq!(folded.skipped, 0);
    }

    #[test]
    fn same_names_get_their_folder_and_a_narrow_view_shortens_the_names_but_keeps_both_tags() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_names");
        let dir = _dir_guard.to_path_buf();
        let name = "000000_S01_perturb_20_001.tak";
        for sub in ["copy", "raw"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let (mut app, _) = overlay_app(&ctx, &dir.join("copy"), &[name]);
        lay_over(&ctx, &mut app, &dir.join("raw").join(name));
        overlay_arrive(&ctx, &mut app, 0);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, &format!("1 {name} · copy")).len(), 1);
        assert_eq!(text_rects(&out, &format!("2 {name} · raw")).len(), 1);
        let wide = app.views[0].scene_rect;
        let narrow = egui::Rect::from_min_size(wide.min, egui::vec2(420.0, wide.height()));
        let t = app.overlay_tags(&ctx, 0, narrow, None).unwrap();
        assert!(t.tags[1].right() <= narrow.right() && t.close.right() <= narrow.right(), "{:?} {narrow:?}", t.tags);
        assert!(t.labels[0].starts_with("1 00") && t.labels[0].contains('…') && t.labels[0].ends_with(".tak · copy"), "{}", t.labels[0]);
        assert!(t.labels[1].starts_with("2 00") && t.labels[1].ends_with(".tak · raw"), "{}", t.labels[1]);
    }

    // ───────────── Plan step 8: the laid-over file's sticks (thin white lines) ─────────────

    /// Color of the first file's sticks in `stick_take`.
    const STICK_RGB: [f32; 3] = [1.0, 0.0, 0.0];

    /// A take of frames 0–9 with skeleton asset "S": markers A, B, C (slots 1–3, sticks A–B and
    /// B–C stored in the file) and an unlabeled marker "U". `dx` = how far (m) A, B, C move along
    /// x; `gone` = (marker, frame) samples left out.
    fn stick_take(path: &Path, dx: [f32; 3], gone: &[(usize, u32)]) -> tak_core::tak::Take {
        use tak_core::tak::{MarkerKind, MarkerStick, SkeletonAsset, Trajectory};
        let asset = [9u8; 16];
        let base = [[0.0f32, 1.0, 0.0], [0.2, 1.2, 0.0], [0.4, 1.0, 0.0], [0.6, 1.0, 0.0]];
        let mut t = views::sample_take(path.to_str().unwrap(), &["A", "B", "C", "U"]);
        t.assets = vec![SkeletonAsset {
            uid: asset,
            name: "S".into(),
            markerset_name: None,
            model: None,
            sticks: vec![MarkerStick { slot_a: 1, slot_b: 2, rgb: STICK_RGB }, MarkerStick { slot_a: 2, slot_b: 3, rgb: STICK_RGB }],
            marker_colors: vec![],
        }];
        t.trajectories = (0..4usize)
            .map(|k| {
                let named = k < 3;
                let frames: Vec<u32> = (0..10).filter(|f| !gone.contains(&(k, *f))).collect();
                let x = base[k][0] + if named { dx[k] } else { 0.0 };
                Trajectory {
                    kind: if named { MarkerKind::Template } else { MarkerKind::Unlabeled },
                    parent_uid: named.then_some(asset),
                    asset_uid: named.then_some(asset),
                    slot: named.then_some(k as i32 + 1),
                    xyz: frames.iter().map(|_| [x, base[k][1], base[k][2]]).collect(),
                    frames,
                    ..t.trajectories[k].clone()
                }
            })
            .collect();
        t
    }

    /// One view showing `a` (a.tak) with `b` (b.tak) laid over it, both arrived (held loads).
    fn stick_overlay_app(ctx: &egui::Context, dir: &Path, a: [f32; 3], a_gone: &[(usize, u32)], b: [f32; 3], b_gone: &[(usize, u32)]) -> TakViewerApp {
        let f = take_files(dir, &["a.tak", "b.tak"]);
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        app.hold_loads = true;
        frame_with(ctx, &mut app, vec![]);
        app.open(ctx, f[0].clone());
        frame_with(ctx, &mut app, vec![]);
        let tx = app.views[0].held.take().expect("a held load");
        tx.send(views::LoadMsg::Markers(Ok((tak_core::view_data::ViewData::build(stick_take(&f[0], a, a_gone)), vec![])))).unwrap();
        let (ctx_tx, comments_rx) = std::sync::mpsc::channel();
        ctx_tx.send(load_c(&f[0])).unwrap();
        app.views[0].comments_rx = Some(comments_rx);
        frame_with(ctx, &mut app, vec![]);
        frame_with(ctx, &mut app, vec![]);
        lay_over(ctx, &mut app, &f[1]);
        overlay_arrive_as(ctx, &mut app, 0, stick_take(&f[1], b, b_gone));
        assert!(app.views[0].overlay.is_some());
        app
    }

    /// Every shape painted, in paint order (nested lists flattened).
    fn painted(out: &egui::FullOutput) -> Vec<egui::Shape> {
        fn walk(s: &egui::Shape, all: &mut Vec<egui::Shape>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, all)),
                s => all.push(s.clone()),
            }
        }
        let mut all = Vec::new();
        out.shapes.iter().for_each(|c| walk(&c.shape, &mut all));
        all
    }

    /// Paint-order places of the line segments in `color` and `width` from `a` to `b` (either way,
    /// within 1 pt).
    fn lines_between(shapes: &[egui::Shape], a: egui::Pos2, b: egui::Pos2, color: egui::Color32, width: f32) -> Vec<usize> {
        let near = |p: egui::Pos2, q: egui::Pos2| p.distance(q) < 1.0;
        shapes
            .iter()
            .enumerate()
            .filter(|(_, s)| match s {
                egui::Shape::LineSegment { points: [p, q], stroke } => {
                    stroke.color == egui::epaint::ColorMode::Solid(color) && stroke.width == width && ((near(*p, a) && near(*q, b)) || (near(*p, b) && near(*q, a)))
                }
                _ => false,
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Paint-order places of all line segments in `color` and `width`.
    fn lines_in(shapes: &[egui::Shape], color: egui::Color32, width: f32) -> Vec<usize> {
        shapes
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s, egui::Shape::LineSegment { stroke, .. } if stroke.color == egui::epaint::ColorMode::Solid(color) && stroke.width == width))
            .map(|(i, _)| i)
            .collect()
    }

    /// Paint-order places of the filled circles (dots) and of the white rings centered on the
    /// markers of view 0 (both files; not the icons of the name tags).
    fn circles_in(app: &TakViewerApp, shapes: &[egui::Shape]) -> (Vec<usize>, Vec<usize>) {
        let v = &app.views[0];
        let view = v.cam.view(v.scene_rect);
        let mut at: Vec<egui::Pos2> = v.data.iter().flat_map(|d| d.markers.iter().filter_map(|m| m.position(v.pb.frame))).filter_map(|p| view.project(p).map(|x| x.0)).collect();
        if let (Some(o), Some(f)) = (v.overlay.as_ref(), v.overlay_frame()) {
            at.extend(o.data.markers.iter().filter_map(|m| m.position(f)).filter_map(|p| view.project(p).map(|x| x.0)));
        }
        let (mut dots, mut rings) = (vec![], vec![]);
        for (i, s) in shapes.iter().enumerate() {
            if let egui::Shape::Circle(c) = s {
                if !at.iter().any(|p| p.distance(c.center) < 1.0) {
                    continue;
                }
                if c.fill != egui::Color32::TRANSPARENT {
                    dots.push(i);
                } else if c.stroke.color == theme::OVERLAY_RING && c.stroke.width == scene::OVERLAY_RING_W {
                    rings.push(i);
                }
            }
        }
        (dots, rings)
    }

    fn white_line(shapes: &[egui::Shape], a: egui::Pos2, b: egui::Pos2) -> Vec<usize> {
        lines_between(shapes, a, b, theme::OVERLAY_STICK, scene::OVERLAY_STICK_W)
    }

    fn own_line(shapes: &[egui::Shape], a: egui::Pos2, b: egui::Pos2) -> Vec<usize> {
        lines_between(shapes, a, b, theme::color(tak_core::view_data::ColorSource::File(STICK_RGB)), 1.6)
    }

    #[test]
    fn the_laid_over_files_sticks_are_white_under_the_first_files() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_sticks");
        let dir = _dir_guard.to_path_buf();
        // b's C sits 5 cm off a's C; A and B are at the same place in both.
        let mut app = stick_overlay_app(&ctx, &dir, [0.0; 3], &[], [0.0, 0.0, 0.05], &[]);
        app.views[0].pb.set_frame(3, 0.0);
        let shapes = painted(&frame_with(&ctx, &mut app, vec![]));
        let (dot, ring) = (|m| dot_on_screen(&app, 0, m), |m| ring_on_screen(&app, 0, m));
        let white = [white_line(&shapes, ring(0), ring(1)), white_line(&shapes, ring(1), ring(2))];
        let own = [own_line(&shapes, dot(0), dot(1)), own_line(&shapes, dot(1), dot(2))];
        assert!(white.iter().chain(&own).all(|l| l.len() == 1), "each file's two sticks once: {white:?} {own:?}");
        // No line of any kind from a's C to b's C (the pink "same name, apart" lines of v0.21.0
        // were taken out again).
        let near = |p: egui::Pos2, q: egui::Pos2| p.distance(q) < 1.0;
        let (c_a, c_b) = (dot(2), ring(2));
        let joins = shapes.iter().any(|s| matches!(s, egui::Shape::LineSegment { points: [p, q], .. } if (near(*p, c_a) && near(*q, c_b)) || (near(*p, c_b) && near(*q, c_a))));
        assert!(!joins, "no line between the two files' C");
        // Paint order: white sticks < the first file's sticks < dots < rings.
        let (dots, rings) = circles_in(&app, &shapes);
        assert_eq!((dots.len(), rings.len()), (4, 4), "A B C U in both files");
        let last_white = white.iter().flatten().max().unwrap();
        let (first_own, last_own) = (own.iter().flatten().min().unwrap(), own.iter().flatten().max().unwrap());
        assert!(last_white < first_own && last_own < &dots[0] && dots[3] < rings[0], "{white:?} {own:?} {dots:?} {rings:?}");
        // Key 2 hides b: no white sticks, no rings; a's sticks stay.
        // (white lines, a's sticks, rings) drawn now.
        let shown = |app: &mut TakViewerApp| {
            let s = painted(&frame_with(&ctx, app, vec![]));
            let own = (0..2).map(|k| own_line(&s, dot_on_screen(app, 0, k), dot_on_screen(app, 0, k + 1)).len()).sum::<usize>();
            (lines_in(&s, theme::OVERLAY_STICK, scene::OVERLAY_STICK_W).len(), own, circles_in(app, &s).1.len())
        };
        assert_eq!(shown(&mut app), (2, 2, 4));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num2)]);
        assert_eq!(app.views[0].shown, [true, false]);
        assert_eq!(shown(&mut app), (0, 2, 0), "b hidden: no white sticks, no rings");
        // Shown again; then key 1 hides a: b's white sticks stay, a's sticks go.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num2)]);
        assert_eq!(shown(&mut app), (2, 2, 4));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num1)]);
        assert_eq!(shown(&mut app), (2, 0, 4), "a hidden");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Num1)]);
        assert_eq!(app.views[0].shown, [true, true]);
        // Taken off: none of b's lines.
        app.overlay_off(0);
        assert_eq!(shown(&mut app), (0, 2, 0));
    }

    #[test]
    fn a_stick_with_a_missing_marker_is_not_drawn() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_gone");
        let dir = _dir_guard.to_path_buf();
        // b's C 5 cm off and missing at frame 4; a's C missing at frame 6; b's A 10 mm off.
        let mut app = stick_overlay_app(&ctx, &dir, [0.0; 3], &[(2, 6)], [0.010, 0.0, 0.05], &[(2, 4)]);
        for (frame, white_bc, own_bc) in [(3, 1, 1), (4, 0, 1), (6, 1, 0)] {
            app.views[0].pb.set_frame(frame, 0.0);
            let shapes = painted(&frame_with(&ctx, &mut app, vec![]));
            let ring = |m| ring_on_screen(&app, 0, m);
            let dot = |m| dot_on_screen(&app, 0, m);
            assert_eq!(white_line(&shapes, ring(0), ring(1)).len(), 1, "frame {frame}: b's A–B");
            let b_c = app.views[0].overlay.as_ref().unwrap().data.markers[2].position(frame).is_some();
            let wbc = if b_c { white_line(&shapes, ring(1), ring(2)).len() } else { 0 };
            let a_c = app.views[0].data.as_ref().unwrap().markers[2].position(frame).is_some();
            let obc = if a_c { own_line(&shapes, dot(1), dot(2)).len() } else { 0 };
            assert_eq!((wbc, obc), (white_bc, own_bc), "frame {frame}");
            // Of b's two sticks only those with both ends there; a's likewise.
            assert_eq!(lines_in(&shapes, theme::OVERLAY_STICK, scene::OVERLAY_STICK_W).len(), 1 + white_bc, "frame {frame}");
            assert_eq!(lines_in(&shapes, theme::color(tak_core::view_data::ColorSource::File(STICK_RGB)), 1.6).len(), 1 + own_bc, "frame {frame}");
        }
    }

    #[test]
    fn without_an_overlay_the_first_file_is_drawn_as_before() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_none");
        let dir = _dir_guard.to_path_buf();
        let mut app = stick_overlay_app(&ctx, &dir, [0.0; 3], &[], [0.0, 0.0, 0.05], &[]);
        app.views[0].pb.set_frame(3, 0.0);
        app.overlay_off(0);
        idle(&ctx, &mut app);
        let shapes = painted(&frame_with(&ctx, &mut app, vec![]));
        let dot = |m| dot_on_screen(&app, 0, m);
        assert_eq!((own_line(&shapes, dot(0), dot(1)).len(), own_line(&shapes, dot(1), dot(2)).len()), (1, 1));
        let (dots, rings) = circles_in(&app, &shapes);
        assert_eq!((lines_in(&shapes, theme::OVERLAY_STICK, scene::OVERLAY_STICK_W).len(), dots.len(), rings.len()), (0, 4, 0));
    }

    #[test]
    fn overlay_options_of_command_runs() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let (s, rest) = shot::parse(&args(&["--screenshot", "o.png", "--overlay", "b.tak", "--frame", "5", "a.tak", "c.tak", "--overlay", "d.tak", "--hide", "2", "--hide", "x"]));
        let s = s.unwrap();
        assert_eq!(s.overlays, vec![PathBuf::from("b.tak"), PathBuf::from("d.tak")], "the n-th onto the n-th view");
        assert_eq!((s.hide, s.frame), (vec![1], Some(5)));
        assert_eq!(rest, args(&["a.tak", "c.tak"]), "laid-over files are no views");
        let (files, rest) = shot::take_overlays(&args(&["tak_viewer", "--bench", "600", "a.tak", "--overlay", "b.tak"]));
        assert_eq!((files, rest), (vec![PathBuf::from("b.tak")], args(&["tak_viewer", "--bench", "600", "a.tak"])));
        // `--screenshot … --overlay`: laid over the first view; a refused one stops the run.
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("overlay_shot");
        let dir = _dir_guard.to_path_buf();
        let f = take_files(&dir, &["a.tak", "b.tak"]);
        let shot = AutoShot { out: dir.join("x.png"), frame: None, select: None, plate: None, range: None, menu_capture: false, edit_tools: false, edit_pattern: false, edit_layout: 0, comments: false, pick_comment: None, edit_comment: None, type_strip: None, overlays: vec![f[0].clone()], hide: vec![], hover: None, stage: 0 };
        let app = TakViewerApp::build(&ctx, None, vec![f[0].clone()], Some(shot), None);
        assert!(app.views[0].overlay_target().is_none());
        assert!(app.error.as_deref().is_some_and(|e| e.contains("이 화면에 열린 파일")), "{:?}", app.error);
    }

    // ───────────── Plan step 9: the second file's curves dashed in the graph ─────────────

    /// A sample take with markers `names` (frames 0–9, 100 Hz), all labeled but "U". `second`:
    /// marker N is 5 cm further along x and has no samples at frames 4–5.
    fn pair_take(path: &Path, names: &[&str], second: bool) -> tak_core::tak::Take {
        let mut t = views::sample_take(path.to_str().unwrap(), names);
        for tr in t.trajectories.iter_mut().filter(|tr| tr.name != "U") {
            tr.kind = tak_core::tak::MarkerKind::OtherParent;
            tr.parent_uid = Some([0xAA; 16]);
        }
        if second {
            let n = t.trajectories.iter_mut().find(|tr| tr.name == "N").unwrap();
            let keep: Vec<usize> = (0..n.frames.len()).filter(|&i| !(4..=5).contains(&n.frames[i])).collect();
            n.frames = keep.iter().map(|&i| n.frames[i]).collect();
            n.xyz = keep.iter().map(|&i| [n.xyz[i][0] + 0.05, n.xyz[i][1], n.xyz[i][2]]).collect();
        }
        t
    }

    /// a.tak (markers M, N, U, Q) open in view 0 (held load), b.tak a stub to lay over it.
    fn pair_app(ctx: &egui::Context, dir: &Path) -> (TakViewerApp, Vec<PathBuf>) {
        pair_app_named(ctx, dir, "a.tak")
    }

    /// `pair_app` with the first file named `first` (a `.takm` takes Comments, a `.tak` does not).
    fn pair_app_named(ctx: &egui::Context, dir: &Path, first: &str) -> (TakViewerApp, Vec<PathBuf>) {
        let f = take_files(dir, &[first, "b.tak"]);
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        app.hold_loads = true;
        frame_with(ctx, &mut app, vec![]);
        app.open(ctx, f[0].clone());
        frame_with(ctx, &mut app, vec![]);
        let tx = app.views[0].held.take().expect("a held load (`hold_loads`)");
        tx.send(views::LoadMsg::Markers(Ok((tak_core::view_data::ViewData::build(pair_take(&f[0], &["M", "N", "U", "Q"], false)), vec![])))).unwrap();
        let (ctx_tx, comments_rx) = std::sync::mpsc::channel();
        ctx_tx.send(load_c(&f[0])).unwrap();
        app.views[0].comments_rx = Some(comments_rx);
        frame_with(ctx, &mut app, vec![]);
        frame_with(ctx, &mut app, vec![]);
        (app, f)
    }

    fn marker_index(app: &TakViewerApp, name: &str) -> usize {
        app.views[0].data.as_ref().unwrap().markers.iter().position(|m| m.name == name).unwrap()
    }

    #[test]
    fn the_graph_draws_the_laid_over_files_marker_of_the_same_name_dashed_on_one_scale() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("pair_graph");
        let dir = _dir_guard.to_path_buf();
        let (mut app, f) = pair_app(&ctx, &dir);
        let n = marker_index(&app, "N");
        app.views[0].select(n, false);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.views[0].graph_rect.is_some(), "the graph floats");
        // Without an overlay: solid lines only, no legend.
        let alone = app.views[0].lanes.drew().clone();
        assert_eq!((alone.solid, alone.dashed, alone.legend), (3, 0, false));
        assert!((alone.ranges[0].0 - 100.0).abs() < 1e-3 && alone.ranges[0].0 == alone.ranges[0].1, "x of N = 0.1 m: {:?}", alone.ranges[0]);
        assert!(app.views[0].lanes.pair_now().is_none());

        // b.tak laid over: N of b dashed in the same lanes, one range over both.
        lay_over(&ctx, &mut app, &f[1]);
        overlay_arrive_as(&ctx, &mut app, 0, pair_take(&f[1], &["M", "N", "U"], true));
        frame_with(&ctx, &mut app, vec![]);
        let pair = app.views[0].lanes.pair_now().expect("a second file");
        assert_eq!(pair.lanes.len(), 3);
        let frames = |run: &Vec<[f64; 2]>| (run[0][0], run.last().unwrap()[0]);
        assert_eq!(pair.lanes[0].runs.iter().map(frames).collect::<Vec<_>>(), vec![(0.0, 3.0), (6.0, 9.0)], "its gap breaks the dashed line");
        let both = app.views[0].lanes.drew().clone();
        assert_eq!((both.solid, both.dashed, both.legend), (3, 6, true));
        assert!(both.dashes > 6, "drawn as dashes: {}", both.dashes);
        let (lo, hi) = both.ranges[0];
        assert!((lo - 100.0).abs() < 1e-3 && (hi - 150.0).abs() < 1e-3, "x covers both files: {lo} {hi}");
        let builds = (app.views[0].lanes.builds, app.views[0].lanes.pair_builds);
        assert_eq!(builds.1, 1);

        // Key 2 hides the dashed line, key 1 the solid one; the range never moves.
        for (key, shown, solid, dashed) in [(Key::Num2, [true, false], 3, 0), (Key::Num1, [false, false], 0, 0), (Key::Num2, [false, true], 0, 6), (Key::Num1, [true, true], 3, 6)] {
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, key)]);
            frame_with(&ctx, &mut app, vec![]);
            let d = app.views[0].lanes.drew().clone();
            assert_eq!((app.views[0].shown, d.solid, d.dashed, d.legend), (shown, solid, dashed, true), "{key:?}");
            assert_eq!(d.ranges, both.ranges, "same scale shown or hidden ({shown:?})");
        }
        assert_eq!((app.views[0].lanes.builds, app.views[0].lanes.pair_builds), builds, "nothing built again for frames or hide / show");

        // The GRAPH pane draws the same.
        app.set_graph_float(false);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        let pane = app.views[0].lanes.drew().clone();
        assert!(dock::has(&app.dock, Pane::Graph) && app.views[0].graph_rect.is_none());
        assert_eq!((pane.solid, pane.dashed, pane.legend, pane.ranges), (3, 6, true, both.ranges.clone()));
        app.set_graph_float(true);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);

        // M is the same in both files: dashed under the solid line.
        app.views[0].select(marker_index(&app, "M"), false);
        frame_with(&ctx, &mut app, vec![]);
        let m = app.views[0].lanes.drew().clone();
        assert_eq!((m.solid, m.dashed, m.legend, m.ranges[0]), (3, 3, true, (0.0, 0.0)), "x of M = 0 in both");
        // Q is not in b, U is unlabeled: nothing extra, the graph as without an overlay.
        for name in ["Q", "U"] {
            app.views[0].select(marker_index(&app, name), false);
            frame_with(&ctx, &mut app, vec![]);
            let d = app.views[0].lanes.drew().clone();
            assert!(app.views[0].lanes.pair_now().is_some_and(|p| p.lanes.is_empty()), "{name}");
            assert_eq!((d.solid, d.dashed, d.legend), (3, 0, false), "{name}");
        }
        // Taken off: N's graph exactly as before the overlay.
        app.views[0].select(n, false);
        frame_with(&ctx, &mut app, vec![]);
        app.overlay_off(0);
        frame_with(&ctx, &mut app, vec![]);
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.views[0].lanes.pair_now().is_none());
        assert_eq!(*app.views[0].lanes.drew(), alone);
    }

    // ───────────── `.takm` step ③: Comments in the `.takm`, none on a TAK, save as `.takm` ─────────────

    /// The fixed part of a `.takm` (every byte before its Comment tail).
    fn fixed_part(path: &Path) -> Vec<u8> {
        let start = tak_core::takm::read_index_path(path).unwrap().tail_start() as usize;
        std::fs::read(path).unwrap()[..start].to_vec()
    }

    /// The bytes of a file (compared whole: same content).
    fn sha(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap()
    }

    /// Frames until the `.takm` conversion has finished.
    fn until_converted(ctx: &egui::Context, app: &mut TakViewerApp) {
        // A conversion may wait for the shared Comment writer's disk barrier. Keep its
        // completion wait independent of the functional frame count and latency checks.
        let started = Instant::now();
        let deadline = started + Duration::from_secs(60);
        loop {
            frame_with(ctx, app, vec![]);
            if app.takm_job.is_none() {
                return;
            }
            assert!(Instant::now() < deadline,
                "conversion not finished after {:?}: working={:?}, note={:?}, error={:?}, notice={:?}, outstanding_comments={}",
                started.elapsed(), app.takm_working(), app.takm_note, app.error, app.notice, app.comment_outstanding);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn takm_note(app: &TakViewerApp) -> String {
        app.takm_note.as_ref().map(|n| n.0.clone()).unwrap_or_default()
    }

    /// `<dir>/a.tak` (a real TAK: the sample) with `a.comments.csv` beside it (comments 1, 2 and
    /// a cut-off third line), opened in view 0 (held load, markers M and N).
    fn tak_app(ctx: &egui::Context, dir: &Path) -> (TakViewerApp, PathBuf) {
        let tak = dir.join("a.tak");
        std::fs::write(&tak, tak_core::sample_tak::sample_tak()).unwrap();
        two_comments_on_disk(dir);
        let mut csv = Vec::new();
        for (k, e) in tail_read(&dir.join("a.takm")).unwrap().unwrap().events.iter().enumerate() {
            if k == 0 {
                csv.extend(comments::header_line().as_bytes());
            }
            csv.extend(comments::encode(e).as_bytes());
        }
        csv.extend(b"2026-11-13T14:05:09+09:00,add,3,M,1,");
        std::fs::remove_file(dir.join("a.takm")).unwrap();
        std::fs::write(dir.join("a.comments.csv"), csv).unwrap();
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        app.hold_loads = true;
        frame_with(ctx, &mut app, vec![]);
        app.open(ctx, tak.clone());
        frame_with(ctx, &mut app, vec![]);
        arrive(ctx, &mut app, 0);
        (app, tak)
    }

    #[test]
    fn comments_on_a_takm_go_into_its_tail_with_undo_and_redo_and_read_back_the_same() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("takm_tail");
        let dir = _dir_guard.to_path_buf();
        let mut app = comment_app(&ctx, &dir, false);
        let takm = dir.join("a.takm");
        let fixed = fixed_part(&takm);
        // Add (M twice), type (T on the new comment), note, delete, undo, redo.
        app.views[0].select(0, false);
        app.views[0].range = Some((2, 5));
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        app.views[0].pb.set_frame(7, 0.0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        let mut two = app.views[0].comments.list.get(2).cloned().unwrap();
        assert_eq!(two.comment_type, comments::TYPES[0]);
        two.text = "흔들림".into();
        assert!(app.edit_comment(&ctx, 0, two));
        frame_with(&ctx, &mut app, vec![]);
        app.delete_comment(&ctx, 0, 1);
        frame_with(&ctx, &mut app, vec![]);
        undo_key(&ctx, &mut app);
        assert_eq!(ids(&app), vec![1, 2]);
        redo_key(&ctx, &mut app);
        assert_eq!(ids(&app), vec![2]);
        until_saved(&ctx, &mut app);
        // Every change is a line in the tail; the fixed part is untouched; no CSV.
        let ev = file_events(&dir);
        let kinds: Vec<(&str, u64)> = ev.iter().map(|e| (e.kind.as_str(), e.id)).collect();
        assert_eq!(kinds, vec![("add", 1), ("add", 2), ("edit", 2), ("edit", 2), ("delete", 1), ("add", 1), ("delete", 1)]);
        assert!(ev.iter().all(|e| e.file_a == takm_label("a.takm")), "file_a = name|fixed length");
        assert_eq!(fixed_part(&takm), fixed);
        tak_core::takm::verify_fixed(&takm).unwrap();
        assert!(!dir.join("a.comments.csv").exists());
        // Read again (as an open does): the same list.
        let again = views::CommentsLoad::read(&takm).parsed.unwrap().comments();
        assert_eq!(again.list(), app.views[0].comments.list.list());
        assert_eq!(app.views[0].comments.list.get(2).map(|c| (c.comment_type.as_str(), c.text.as_str())), Some((comments::TYPES[0], "흔들림")));
    }

    #[test]
    fn a_comment_csv_next_to_a_takm_is_neither_read_nor_written() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("takm_csv_beside");
        let dir = _dir_guard.to_path_buf();
        two_comments_on_disk(&dir);
        std::fs::write(dir.join("a.comments.csv"), tak_core::takm::read_tail_text(&dir.join("a.takm")).unwrap()).unwrap();
        std::fs::remove_file(dir.join("a.takm")).unwrap();
        let before = std::fs::read(dir.join("a.comments.csv")).unwrap();
        let mut app = comment_app(&ctx, &dir, false);
        assert!(app.views[0].comments.list.is_empty(), "the CSV is not read");
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        until_saved(&ctx, &mut app);
        assert_eq!(ids(&app), vec![1]);
        assert_eq!(std::fs::read(dir.join("a.comments.csv")).unwrap(), before, "nor written");
        assert_eq!(file_events(&dir).len(), 1, "the comment is in the .takm");
    }

    #[test]
    fn a_tak_has_no_comments_and_m_the_button_and_t_write_nothing_and_say_why() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("tak_no_comments");
        let dir = _dir_guard.to_path_buf();
        let (mut app, tak) = tak_app(&ctx, &dir);
        let csv = std::fs::read(dir.join("a.comments.csv")).unwrap();
        let tak_sha = sha(&tak);
        assert!(app.views[0].comments.list.is_empty(), "its CSV is not read: no blocks on the band");
        assert_eq!(app.comment_writable(0).unwrap_err(), views::TAK_NO_COMMENTS);
        // M (tap and hold), the Comment button (dim, still says why), T.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        assert_eq!(takm_note(&app), views::TAK_NO_COMMENTS);
        app.takm_note = None;
        key_down(&ctx, &mut app, Key::M);
        app.views[0].pb.set_frame(5, 0.0);
        key_up(&ctx, &mut app, Key::M);
        assert!(app.hold.is_none());
        assert_eq!(takm_note(&app), views::TAK_NO_COMMENTS);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, views::TAK_NO_COMMENTS).len(), 1, "the status bar says why");
        app.takm_note = None;
        let button = text_rects(&out, "Comment")[0];
        click_at(&ctx, &mut app, button.center());
        assert_eq!(takm_note(&app), views::TAK_NO_COMMENTS);
        app.takm_note = None;
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::T)]);
        assert_eq!(takm_note(&app), views::TAK_NO_COMMENTS);
        app.takm_note = None;
        // The range on the band strip works as before.
        app.views[0].pb.set_frame(2, 0.0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::OpenBracket)]);
        app.views[0].pb.set_frame(6, 0.0);
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::CloseBracket)]);
        assert_eq!(app.views[0].range, Some((2, 6)));
        // The COMMENTS pane: the reason and the button, no list.
        app.show_pane(Pane::Comments);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, views::TAK_NO_COMMENTS).len(), 1, "in the pane");
        assert_eq!(text_rects(&out, ".takm으로 저장").len(), 1);
        assert!(text_rects(&out, "아직 Comment가 없어요. M을 누르면 생겨요.").is_empty());
        // Nothing written anywhere.
        frame_with(&ctx, &mut app, vec![]);
        assert!(app.views[0].comments.list.is_empty() && app.comment_writer.is_none());
        assert_eq!(std::fs::read(dir.join("a.comments.csv")).unwrap(), csv);
        assert!(!dir.join("a.takm").exists());
        assert_eq!(sha(&tak), tak_sha);
    }

    #[test]
    fn the_comments_pane_button_saves_the_tak_as_takm_without_blocking_and_the_view_switches_keeping_its_place() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("takm_save_pane");
        let dir = _dir_guard.to_path_buf();
        let (mut app, tak) = tak_app(&ctx, &dir);
        let (csv, tak_sha, tak_meta) = (sha(&dir.join("a.comments.csv")), sha(&tak), std::fs::metadata(&tak).unwrap());
        // Where the view is: frame, camera, selection, range.
        app.views[0].pb.set_frame(5, 0.0);
        label_click(&mut app, 1, false);
        app.views[0].range = Some((2, 7));
        app.views[0].cam.yaw += 0.5;
        let cam = app.views[0].cam.yaw;
        app.show_pane(Pane::Comments);
        let out = frame_with(&ctx, &mut app, vec![]);
        let button = text_rects(&out, ".takm으로 저장")[0];
        // The conversion thread waits until the test lets it go: frames go on meanwhile.
        let (go, gate) = std::sync::mpsc::channel();
        app.takm_gate = Some(gate);
        let out = click_at(&ctx, &mut app, button.center());
        assert!(app.takm_job.is_some() && !dir.join("a.takm").exists());
        assert_eq!(text_rects(&out, "a.tak → .takm 만드는 중…").len(), 2, "the status bar and the pane say it is working");
        for _ in 0..5 {
            frame_with(&ctx, &mut app, vec![]);
        }
        assert!(app.takm_job.is_some(), "frames went on while it waited");
        go.send(()).unwrap();
        until_converted(&ctx, &mut app);
        let line = takm_note(&app);
        assert!(line.starts_with("a.takm 만들었어요 · ") && line.contains(" MB → ") && line.contains("Comment 줄 2개를 넣었어요"), "{line}");
        // The view reads the .takm at the same place, with the CSV's comments.
        assert_eq!(app.views[0].target(), Some(dir.join("a.takm").as_path()));
        arrive(&ctx, &mut app, 0);
        let v = &app.views[0];
        assert_eq!((v.pb.frame, v.cam.yaw, v.range), (5, cam, Some((2, 7))));
        assert_eq!(sel(&app), (vec![1], Some(1)));
        assert_eq!(ids(&app), vec![1, 2]);
        // TAK and CSV untouched; the new file holds the two comment lines.
        let meta = std::fs::metadata(&tak).unwrap();
        assert_eq!((sha(&tak), meta.len(), meta.modified().unwrap()), (tak_sha, tak_meta.len(), tak_meta.modified().unwrap()));
        assert_eq!(sha(&dir.join("a.comments.csv")), csv);
        assert_eq!(file_events(&dir).len(), 2);
        // M works at once.
        run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M)]);
        until_saved(&ctx, &mut app);
        assert_eq!(file_events(&dir).len(), 3);
        // Ctrl+Z: the comment, then the switch (the TAK again at its place; the .takm stays).
        undo_key(&ctx, &mut app);
        until_saved(&ctx, &mut app);
        assert!(app.comment_failed.is_empty(), "the undo event must be saved before switching files");
        undo_key(&ctx, &mut app);
        assert_eq!((app.views[0].target(), note(&app).as_str()), (Some(tak.as_path()), "Undo · Open a.tak"));
        arrive(&ctx, &mut app, 0);
        assert_eq!((app.views[0].pb.frame, app.views[0].range), (5, Some((2, 7))));
        assert!(dir.join("a.takm").exists());
        until_saved(&ctx, &mut app);
    }

    #[test]
    fn the_file_menu_item_is_dim_with_its_reason_and_saves_the_active_tak() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("takm_save_menu");
        let dir = _dir_guard.to_path_buf();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.hold_loads = true;
        let out = frame_with(&ctx, &mut app, vec![]);
        let out = click_at(&ctx, &mut app, text_rects(&out, "파일")[0].center());
        assert_eq!(text_rects(&out, "지금 화면에 TAK를 열면 쓸 수 있어요.").len(), 1, "dim, the reason under it");
        click_at(&ctx, &mut app, text_rects(&out, ".takm으로 저장")[0].center());
        assert!(app.takm_job.is_none());
        let tak = dir.join("a.tak");
        std::fs::write(&tak, tak_core::sample_tak::sample_tak()).unwrap();
        app.open(&ctx, tak.clone());
        frame_with(&ctx, &mut app, vec![]);
        arrive(&ctx, &mut app, 0);
        // (The menu may still be open after the click on the dim item.)
        let mut out = frame_with(&ctx, &mut app, vec![]);
        if text_rects(&out, ".takm으로 저장").is_empty() {
            out = click_at(&ctx, &mut app, text_rects(&out, "파일")[0].center());
        }
        assert!(text_rects(&out, "지금 화면에 TAK를 열면 쓸 수 있어요.").is_empty());
        click_at(&ctx, &mut app, text_rects(&out, ".takm으로 저장")[0].center());
        until_converted(&ctx, &mut app);
        assert_eq!(app.views[0].target(), Some(dir.join("a.takm").as_path()));
        arrive(&ctx, &mut app, 0);
        // On a .takm the item is dim and says why.
        let out = frame_with(&ctx, &mut app, vec![]);
        let out = click_at(&ctx, &mut app, text_rects(&out, "파일")[0].center());
        assert_eq!(text_rects(&out, "지금 화면의 파일이 TAK가 아니에요(이미 .takm).").len(), 1);
    }

    #[test]
    fn the_explorer_menu_saves_a_tak_no_view_shows_and_lists_its_folder_again() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("takm_save_explorer");
        let dir = _dir_guard.to_path_buf();
        let tak = dir.join("b.tak");
        std::fs::write(&tak, tak_core::sample_tak::sample_tak()).unwrap();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.hold_loads = true;
        app.open_workspace(dir.clone());
        // The folder is listed (background), then the file's row is right-clicked.
        let mut row = None;
        for _ in 0..2000 {
            let out = frame_with(&ctx, &mut app, vec![]);
            if let Some(r) = text_rects(&out, "b.tak").first() {
                row = Some(*r);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let out = right_click_at(&ctx, &mut app, row.expect("the file row").center());
        click_at(&ctx, &mut app, text_rects(&out, ".takm으로 저장")[0].center());
        until_converted(&ctx, &mut app);
        assert!(takm_note(&app).starts_with("b.takm 만들었어요"), "{}", takm_note(&app));
        assert!(dir.join("b.takm").exists());
        assert_eq!(app.views[0].target(), None, "converted only: no view showed it");
        assert!(app.workspace.listing(&dir).is_some(), "its folder is read again");
    }

    #[test]
    fn an_existing_takm_is_opened_not_overwritten() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("takm_exists");
        let dir = _dir_guard.to_path_buf();
        let (mut app, tak) = tak_app(&ctx, &dir);
        two_comments_on_disk(&dir);
        let before = std::fs::read(dir.join("a.takm")).unwrap();
        app.save_takm(&ctx, tak.clone());
        until_converted(&ctx, &mut app);
        assert_eq!(takm_note(&app), "a.takm이(가) 이미 있어서 덮어쓰지 않고 그 파일을 열었어요.");
        assert_eq!(std::fs::read(dir.join("a.takm")).unwrap(), before);
        assert_eq!(app.views[0].target(), Some(dir.join("a.takm").as_path()));
        arrive(&ctx, &mut app, 0);
        assert_eq!(ids(&app), vec![1, 2], "its own comments");
    }

    #[test]
    fn no_takm_is_made_under_input_or_in_a_command_run() {
        let ctx = egui::Context::default();
        let _root_guard = comment_dir("takm_input");
        let root = _root_guard.to_path_buf();
        std::fs::create_dir_all(root.join("apps")).unwrap();
        std::fs::create_dir_all(root.join("input").join("raw")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "x").unwrap();
        std::fs::write(root.join("apps").join("AGENTS.md"), "x").unwrap();
        let tak = root.join("input").join("raw").join("a.tak");
        std::fs::write(&tak, tak_core::sample_tak::sample_tak()).unwrap();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.save_takm(&ctx, tak.clone());
        until_converted(&ctx, &mut app);
        assert!(takm_note(&app).contains("input/ 폴더 안이라 .takm을 만들지 않아요"), "{}", takm_note(&app));
        assert!(!root.join("input").join("raw").join("a.takm").exists());
        // A command run converts nothing.
        let other = root.join("a.tak");
        std::fs::write(&other, tak_core::sample_tak::sample_tak()).unwrap();
        app.set_command_run();
        app.save_takm(&ctx, other);
        assert!(app.takm_job.is_none());
        assert_eq!(takm_note(&app), "명령 실행은 파일을 만들지 않아요.");
        assert!(!root.join("a.takm").exists());
    }

    // ───────────── v0.27.0: every selected marker in the X / Y / Z graph ─────────────

    #[test]
    fn every_selected_marker_is_in_the_graph_lanes_in_the_lane_colors_with_overlay_bands_and_both_places() {
        let lane_colors = |n: usize| vec![vec![theme::AXIS_X; n], vec![theme::AXIS_Y; n], vec![theme::AXIS_Z; n]];
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("multi_graph");
        let dir = _dir_guard.to_path_buf();
        let (mut app, f) = pair_app_named(&ctx, &dir, "a.takm");
        wait_comment_ids(&ctx, &mut app, 0);
        let (m, n, q) = (marker_index(&app, "M"), marker_index(&app, "N"), marker_index(&app, "Q"));
        let two_frames = |app: &mut TakViewerApp| {
            frame_with(&ctx, app, vec![]);
            frame_with(&ctx, app, vec![]);
            app.views[0].lanes.drew().clone()
        };
        // One marker: X red, Y green, Z blue, as before.
        app.views[0].select(n, false);
        let one = two_frames(&mut app);
        assert_eq!((one.solid, one.colors.clone()), (3, vec![vec![theme::AXIS_X], vec![theme::AXIS_Y], vec![theme::AXIS_Z]]));
        // Ctrl+click Q, then M: three curves in every lane (LABELS order M, N, Q whatever the
        // clicks), all in the lane's colour; one range over all (x of M, N, Q = 0, 100, 300 mm).
        app.views[0].select(q, true);
        app.views[0].select(m, true);
        let three = two_frames(&mut app);
        assert!(app.views[0].graph_rect.is_some(), "the graph floats");
        assert_eq!(three.colors, lane_colors(3));
        assert_eq!((three.solid, three.dashed, three.legend), (9, 0, false));
        let near = |(lo, hi): (f64, f64), want: (f64, f64)| (lo - want.0).abs() < 1e-3 && (hi - want.1).abs() < 1e-3;
        assert!(near(three.ranges[0], (0.0, 300.0)), "{:?}", three.ranges[0]);
        assert_eq!(app.views[0].primary, Some(m), "the other panes keep following the last one clicked");
        let builds = (app.views[0].lanes.builds, app.views[0].comments.spans.builds);
        for _ in 0..5 {
            frame_with(&ctx, &mut app, vec![]);
        }
        assert_eq!((app.views[0].lanes.builds, app.views[0].comments.spans.builds), builds, "nothing built again frame by frame");
        // Taking M away keeps N and Q's lanes; back to N alone: exactly as before.
        app.views[0].select(m, true);
        assert_eq!(two_frames(&mut app).colors, lane_colors(2));
        app.views[0].select(n, false);
        assert_eq!(two_frames(&mut app), one);

        // Comment bands: one for each Comment naming any graphed marker, once each.
        app.views[0].select(m, false);
        app.views[0].select(n, true);
        app.views[0].select(q, true);
        app.add_comment(&ctx, 0, vec!["N".into(), "Q".into()], 2, 4);
        app.add_comment(&ctx, 0, vec!["U".into()], 5, 6);
        app.add_comment(&ctx, 0, vec!["M".into()], 7, 7);
        assert_eq!(two_frames(&mut app).bands, 2, "N+Q once, M once, U is not graphed");

        // b.tak laid over: M and N of b dashed in the lanes' colours (Q is not in b); keys 1 / 2.
        lay_over(&ctx, &mut app, &f[1]);
        overlay_arrive_as(&ctx, &mut app, 0, pair_take(&f[1], &["M", "N", "U"], true));
        let both = two_frames(&mut app);
        assert_eq!(both.dash_colors, lane_colors(2));
        assert_eq!(both.colors, three.colors);
        assert!(both.legend && both.dashed > 0);
        assert!(near(both.ranges[0], (0.0, 300.0)), "x of N in b (150 mm) is inside: {:?}", both.ranges[0]);
        for (key, shown) in [(Key::Num2, [true, false]), (Key::Num1, [false, false]), (Key::Num2, [false, true]), (Key::Num1, [true, true])] {
            run_frame(&ctx, &mut app, &[(Modifiers::NONE, key)]);
            let d = two_frames(&mut app);
            assert_eq!(app.views[0].shown, shown);
            assert_eq!((d.colors.iter().all(Vec::is_empty), d.dash_colors.iter().all(Vec::is_empty)), (!shown[0], !shown[1]), "{key:?}");
            assert_eq!(d.ranges, both.ranges, "the range does not jump ({shown:?})");
        }

        // The GRAPH pane draws the same.
        app.set_graph_float(false);
        let pane = two_frames(&mut app);
        assert!(dock::has(&app.dock, Pane::Graph) && app.views[0].graph_rect.is_none());
        assert_eq!((pane.colors, pane.dash_colors, pane.ranges, pane.bands), (both.colors, both.dash_colors, both.ranges, 2));

        // v0.29.0 hover: the mouse over N's curve (x of N = 100 mm, lane X) in the GRAPH pane and
        // in the floating graph: N thick in the three lanes and its name by the pointer. Display
        // only: no action to undo, the frame and the selection stay.
        let x_of_n = 100.0;
        for float in [false, true] {
            app.set_graph_float(float);
            let before = two_frames(&mut app);
            let (undo, frame, selected) = (app.history.undo.len(), app.views[0].pb.frame, app.views[0].selected.clone());
            let f = app.views[0].pb.frame as f64 + 0.3;
            let p = before.point(0, f, x_of_n).expect("lane X drawn");
            frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(p)]);
            let h = two_frames(&mut app);
            assert_eq!((h.hovered, h.hover_lanes), (Some(graph::Curve { marker: 1, laid_over: false }), 3), "float {float}");
            assert_eq!(h.hover_text.map(|t| t.0).as_deref(), Some("N"), "float {float}");
            assert_eq!((h.colors, h.ranges), (before.colors.clone(), before.ranges.clone()), "nothing else changes");
            assert_eq!((app.history.undo.len(), app.views[0].pb.frame, &app.views[0].selected), (undo, frame, &selected), "float {float}");
            frame_with(&ctx, &mut app, vec![egui::Event::PointerGone]);
            assert_eq!(two_frames(&mut app).hovered, None);
        }
        // `--hover N` / `--bench-hover` (agent only): the same without a mouse.
        app.views[0].lanes.hover_at = Some(1);
        let h = two_frames(&mut app);
        assert_eq!((h.hovered.map(|c| c.marker), h.hover_text.map(|t| t.0)), (Some(1), Some("N".to_string())));
        app.views[0].lanes.hover_at = None;
        // `--bench-hover`: another curve (or none) every second frame, M, N, Q, none, M …
        let at: Vec<Option<usize>> = (0..10).map(|i| {
            app.bench_hover(i);
            app.views[0].lanes.hover_at
        }).collect();
        assert_eq!(at, [Some(0), Some(0), Some(1), Some(1), Some(2), Some(2), None, None, Some(0), Some(0)]);
        app.views[0].lanes.hover_at = None;
    }

    #[test]
    fn a_box_of_twelve_markers_draws_the_first_eight_in_labels_order() {
        let ctx = egui::Context::default();
        let _dir_guard = comment_dir("multi_graph_limit");
        let dir = _dir_guard.to_path_buf();
        let f = take_files(&dir, &["a.tak"]);
        let names: Vec<String> = (0..12).map(|k| format!("P{k}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.hold_loads = true;
        frame_with(&ctx, &mut app, vec![]);
        app.open(&ctx, f[0].clone());
        frame_with(&ctx, &mut app, vec![]);
        let tx = app.views[0].held.take().expect("a held load (`hold_loads`)");
        tx.send(views::LoadMsg::Markers(Ok((tak_core::view_data::ViewData::build(pair_take(&f[0], &names, false)), vec![])))).unwrap();
        frame_with(&ctx, &mut app, vec![]);
        app.views[0].select_many((0..12).collect(), Some(11), false);
        for float in [true, false] {
            app.set_graph_float(float);
            frame_with(&ctx, &mut app, vec![]);
            frame_with(&ctx, &mut app, vec![]);
            let d = app.views[0].lanes.drew().clone();
            assert_eq!(d.colors, vec![vec![theme::AXIS_X; 8], vec![theme::AXIS_Y; 8], vec![theme::AXIS_Z; 8]], "float {float}");
            let (lo, hi) = d.ranges[0];
            assert!(lo.abs() < 1e-3 && (hi - 700.0).abs() < 1e-3, "P0 … P7 (x = 0 … 700 mm), not P8 … P11: {lo} {hi}");
        }
        let v = &app.views[0];
        let (graphed, total) = panels::graphed(v.data.as_ref().unwrap(), &v.selected, 11);
        assert_eq!((graphed, total), ((0..8).collect::<Vec<_>>(), 12));
    }
}

#[cfg(test)]
mod ssh_settings_tests {
    use super::*;
    #[test]
    fn python_settings_round_trip_and_old_settings_default() {
        let mut settings = Settings::default();
        settings.ssh_platforms.insert("lab".into(), "windows".into());
        settings.ssh_python.insert("lab".into(), crate::ssh::PythonSetting { explicit:Some(r"codex sandbox -c sandbox_mode=read-only -- C:\Python313\python.exe".into()), remembered:Some("py -3".into()) });
        let decoded: Settings = ron::from_str(&ron::to_string(&settings).unwrap()).unwrap();
        assert_eq!(settings.ssh_python, decoded.ssh_python);
        assert_eq!(settings.ssh_platforms, decoded.ssh_platforms);
        let old: Settings = ron::from_str("(ssh_platforms:{\"lab\":\"windows\"})").unwrap();
        assert!(old.ssh_python.is_empty());
    }
}
