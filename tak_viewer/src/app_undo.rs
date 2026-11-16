
use super::*;
use crate::views::{Range, Selection};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tak_core::edit::{DocumentId, EditError, EditPatch, EditSelection, LabelKey, Revision};
use super::marker_edit::PanelSetting;
use crate::ui::panels::{fold_indices, fold_keys, FoldKey};

/// At most this many actions are kept (the oldest go first). Not a limit on comments.
pub(super) const MAX_UNDO: usize = 500;

/// What a person can change about a shown file, as last recorded (the view's own types).
#[derive(Clone)]
struct FileBase {
    file: PathBuf,
    file_id: usize,
    frame: i64,
    cam: Camera,
    selected: BTreeSet<usize>,
    order: Vec<usize>,
    primary: Option<usize>,
    /// The selected comment (v0.17.1).
    picked: Option<u64>,
    armed_label: Option<LabelKey>,
    range: Range,
    plate: Option<usize>,
    speed: f64,
    folds: HashSet<FoldKey>,
    channels: HashMap<usize, usize>,
}

impl FileBase {
    fn of(v: &View) -> Option<FileBase> {
        Some(FileBase {
            file: v.path()?.to_path_buf(),
            file_id: v.file_id,
            frame: v.pb.frame,
            cam: v.cam,
            selected: v.selected.clone(),
            order: v.ordered_selection(),
            primary: v.primary,
            picked: v.picked,
            armed_label: v.armed_label,
            range: v.range,
            plate: v.plate,
            speed: v.pb.speed,
            folds: v.data.as_ref().map(|data| fold_keys(data, &v.labels.collapsed)).unwrap_or_default(),
            channels: v.devices.channel.clone(),
        })
    }
}

/// The pane layout with the file of each view slot (what an open / close / move puts back).
#[derive(Clone)]
pub(super) struct LayoutSnap {
    dock: DockState<Pane>,
    shape: u64,
    hidden: dock::Hidden,
    /// File shown or being read in each view slot.
    files: [Option<PathBuf>; MAX_VIEWS],
    /// Each layout owns its viewing position; later opens must not replace it in file memory.
    spots: [Option<Spot>; MAX_VIEWS],
    picked: [Option<u64>; MAX_VIEWS],
    /// Keep editable state across reversible layout changes without sharing ViewData.
    documents: [Option<crate::views::EditSession>; MAX_VIEWS],
    edit_selections: [Option<EditSelection>; MAX_VIEWS],
    /// The range each of those files had (a file read again gets it back).
    ranges: [Range; MAX_VIEWS],
    /// The active view then (put back with the layout; choosing a view is no action itself).
    active: usize,
    /// The file laid over each view slot (plan step 7; only once it is on screen) and whose
    /// markers were shown there ([own file, overlay]; both true without an overlay).
    overlays: [Option<PathBuf>; MAX_VIEWS],
    shown: [[bool; 2]; MAX_VIEWS],
}

/// During replacement the old file can still be drawn. Undo must retain the incoming
/// file's restore state, rather than assigning the old file's frame or selection to it.
fn pending_view(v: &View) -> bool {
    v.loading.is_some() || v.remote_loading.is_some()
}

fn layout_spot(v: &View) -> Option<Spot> {
    if pending_view(v) { v.restore.clone() } else { v.spot() }
}

fn layout_overlay(v: &View) -> Option<&Path> {
    v.overlay_load.as_ref().filter(|load| load.quiet).map(|load| load.path.as_path()).or_else(|| v.overlay_path())
}

fn layout_document(v: &View) -> Option<crate::views::EditSession> {
    if pending_view(v) { v.restore_edit_document.clone() } else { v.edit_document.clone() }
}

fn layout_edit_selection(v: &View) -> Option<EditSelection> {
    if pending_view(v) { v.restore_edit_selection.clone() } else { v.data.as_ref().map(|_| v.edit_selection_state()) }
}

fn layout_range(v: &View, shown: Range) -> Range {
    if pending_view(v) { v.restore_range.unwrap_or(None) } else { shown }
}

/// Display options and floating graph places (saved in the settings).
#[derive(Clone, PartialEq)]
pub(super) struct Options {
    scene: SceneOptions,
    graph_float: bool,
    opacity: u8,
    graph_boxes: Vec<GraphBox>,
    plate_boxes: Vec<GraphBox>,
    plate_channels: Vec<LaneSet>,
}

impl Options {
    fn of(s: &Settings) -> Options {
        Options {
            scene: s.scene.clone(),
            graph_float: s.graph_float,
            opacity: s.graph_opacity,
            graph_boxes: s.graph_boxes.clone(),
            plate_boxes: s.plate_boxes.clone(),
            plate_channels: s.plate_channels.clone(),
        }
    }

    fn same(&self, s: &Settings) -> bool {
        self.scene == s.scene
            && self.graph_float == s.graph_float
            && self.opacity == s.graph_opacity
            && self.graph_boxes == s.graph_boxes
            && self.plate_boxes == s.plate_boxes
            && self.plate_channels == s.plate_channels
    }

    /// Status bar word for going from `self` to `to`.
    fn words(&self, to: &Options) -> String {
        if self.scene != to.scene {
            "Display".into()
        } else if self.opacity != to.opacity {
            format!("Opacity {}%", to.opacity)
        } else if self.plate_channels != to.plate_channels {
            "Plate lanes".into()
        } else {
            "Graph".into()
        }
    }
}

/// Saved folders, their names and what is expanded in the Explorer.
#[derive(Clone, PartialEq)]
pub(super) struct Folders {
    roots: Vec<PathBuf>,
    names: HashMap<PathBuf, String>,
    expanded: HashSet<PathBuf>,
    section_collapsed: bool,
    /// The OPEN EDITORS section folded (v0.26.0).
    editors_collapsed: bool,
}

impl Folders {
    fn of(ws: &Workspace, ex: &ExplorerState) -> Folders {
        let (expanded, section_collapsed) = ex.folds();
        Folders { roots: ws.roots().to_vec(), names: ws.name_map().clone(), expanded: expanded.clone(), section_collapsed, editors_collapsed: ex.editors_collapsed }
    }

    fn same(&self, ws: &Workspace, ex: &ExplorerState) -> bool {
        let (expanded, collapsed) = ex.folds();
        self.roots == ws.roots() && &self.names == ws.name_map() && &self.expanded == expanded && self.section_collapsed == collapsed && self.editors_collapsed == ex.editors_collapsed
    }
}

/// One change inside an action. File parts name the file (found again by path, not by view slot).
#[derive(Clone)]
pub(super) enum Part {
    Frame { file: PathBuf, before: i64, after: i64 },
    Camera { file: PathBuf, before: Camera, after: Camera },
    Selection { file: PathBuf, before: Selection, after: Selection },
    PanelSetting { file: PathBuf, before: PanelSetting, after: PanelSetting },
    Range { file: PathBuf, before: Range, after: Range },
    Plate { file: PathBuf, before: Option<usize>, after: Option<usize> },
    Speed { file: PathBuf, before: f64, after: f64 },
    Folds { file: PathBuf, before: HashSet<FoldKey>, after: HashSet<FoldKey> },
    Channels { file: PathBuf, before: HashMap<usize, usize>, after: HashMap<usize, usize> },
    /// A comment added or deleted; `cleared` = the picked range adding it cleared.
    Comment { file: PathBuf, change: Change, cleared: Range },
    /// One entire marker command, including selection / armed label, in global order.
    MarkerEdit { file: PathBuf, patch: Arc<EditPatch> },
    Layout { before: Box<LayoutSnap>, after: Box<LayoutSnap> },
    Options { before: Box<Options>, after: Box<Options> },
    Folders { before: Box<Folders>, after: Box<Folders> },
    /// How the COMMENTS pane is sorted (v0.17.0; not saved in the settings).
    Sort { before: comment_list::Sort, after: comment_list::Sort },
}

impl Part {
    fn file(&self) -> Option<&Path> {
        match self {
            Part::Frame { file, .. }
            | Part::Camera { file, .. }
            | Part::Selection { file, .. }
            | Part::PanelSetting { file, .. }
            | Part::Range { file, .. }
            | Part::Plate { file, .. }
            | Part::Speed { file, .. }
            | Part::Folds { file, .. }
            | Part::Channels { file, .. }
            | Part::Comment { file, .. }
            | Part::MarkerEdit { file, .. } => Some(file),
            Part::Layout { .. } | Part::Options { .. } | Part::Folders { .. } | Part::Sort { .. } => None,
        }
    }

    /// `next` continues `self` (consecutive frame moves, or camera moves, of one file): the two
    /// as one part. None = not merged.
    fn merged(&self, next: &Part) -> Option<Part> {
        match (self, next) {
            (Part::Frame { file, before, .. }, Part::Frame { file: f, after, .. }) if file == f => {
                Some(Part::Frame { file: file.clone(), before: *before, after: *after })
            }
            (Part::Camera { file, before, .. }, Part::Camera { file: f, after, .. }) if file == f => {
                Some(Part::Camera { file: file.clone(), before: *before, after: *after })
            }
            (Part::PanelSetting { file, before, after: mid }, Part::PanelSetting { file: f, before: mid2, after })
                if file == f && mid == mid2 && std::mem::discriminant(before) == std::mem::discriminant(after) => {
                Some(Part::PanelSetting { file: file.clone(), before: before.clone(), after: after.clone() })
            }
            // Type changes of one comment in a row (step 5: `T` pressed again and again, the
            // strip, the tab's type edit): one action from the first type to the last.
            (
                Part::Comment { file, change: Change::Edited { before, after: mid }, cleared: None },
                Part::Comment { file: f, change: Change::Edited { before: mid2, after }, cleared: None },
            ) if file == f && mid == mid2 && only_type(before, mid) && only_type(mid2, after) => {
                Some(Part::Comment { file: file.clone(), change: Change::Edited { before: before.clone(), after: after.clone() }, cleared: None })
            }
            _ => None,
        }
    }

    /// Before and after are the same (a merge came back to where it started).
    fn is_noop(&self) -> bool {
        match self {
            Part::Frame { before, after, .. } => before == after,
            Part::Camera { before, after, .. } => before == after,
            Part::PanelSetting { before, after, .. } => before == after,
            Part::Comment { change: Change::Edited { before, after }, cleared: None, .. } => before == after,
            _ => false,
        }
    }
}

/// `a` and `b` are the same comment but for its type.
fn only_type(a: &Comment, b: &Comment) -> bool {
    a.comment_type != b.comment_type && Comment { comment_type: b.comment_type.clone(), ..a.clone() } == *b
}

/// The state last recorded.
struct Base {
    views: [Option<FileBase>; MAX_VIEWS],
    layout: LayoutSnap,
    options: Options,
    folders: Folders,
    sort: comment_list::Sort,
}

/// The app's undo / redo history (never saved).
#[derive(Default)]
pub(super) struct History {
    /// Actions, newest last (Ctrl+Z takes the last one); at most `MAX_UNDO`.
    pub(super) undo: Vec<Vec<Part>>,
    /// Actions undone, newest last (Ctrl+Y takes the last one); emptied by a new action.
    pub(super) redo: Vec<Vec<Part>>,
    /// Short status bar note after a delete, undo or redo: (English text, `egui` input time).
    pub(super) note: Option<(String, f64)>,
    /// The newest action may still take a following frame / camera move (false after an undo
    /// or redo).
    merge_open: bool,
    /// How many new actions were pushed so far (a merge into the newest one does not count):
    /// tells whether the newest action is the one a COMMENTS row click made (v0.27.3).
    pub(super) pushed: u64,
    base: Option<Box<Base>>,
    /// The app changed the layout on its own (a file could not be read): no action for it.
    pub(super) quiet_layout: bool,
    /// Take everything as it is (a capture ended).
    pub(super) quiet_all: bool,
}

impl History {
    /// Conflicted edits no longer describe the reloaded row and cannot safely be replayed.
    pub(super) fn forget_comment(&mut self, path: &Path, id: u64) {
        for stack in [&mut self.undo, &mut self.redo] {
            for action in stack.iter_mut() {
                action.retain(|part| match part {
                    Part::Comment { file, change, .. } if file == path => {
                        let comment = match change {
                            Change::Added(c) | Change::Deleted(c) => c,
                            Change::Edited { before, .. } => before,
                        };
                        comment.id != id
                    }
                    _ => true,
                });
            }
            stack.retain(|action| !action.is_empty());
        }
        self.merge_open = false;
    }
    /// A new action (none if empty). A lone frame or camera part continues the newest action
    /// when that is the same kind for the same file; coming back to the start removes it.
    pub(super) fn push(&mut self, parts: Vec<Part>) {
        if parts.is_empty() {
            return;
        }
        if self.merge_open && parts.len() == 1 {
            if let Some(top) = self.undo.last_mut().filter(|t| t.len() == 1) {
                if let Some(m) = top[0].merged(&parts[0]) {
                    if m.is_noop() {
                        self.undo.pop();
                    } else {
                        top[0] = m;
                    }
                    return;
                }
            }
        }
        self.undo.push(parts);
        if self.undo.len() > MAX_UNDO {
            drop_later(Some(self.undo.remove(0)));
        }
        if !self.redo.is_empty() { drop_later(Some(std::mem::take(&mut self.redo))); }
        self.merge_open = true;
        self.pushed += 1;
    }

    /// Discarded marker states cannot be reached again; unrelated files and other parts stay.
    fn forget_marker_document(&mut self, id: DocumentId, saved: Revision) {
        // Keep the still-applied chain ending at the saved state. Redo and descendants of
        // that state can contain discarded changes, so remove those for this document.
        let mut keep = HashSet::new();
        let mut cursor = saved;
        for action in self.undo.iter().rev() {
            for part in action.iter().rev() {
                if let Part::MarkerEdit { patch, .. } = part {
                    if patch.document_id() == id && patch.after_revision() == cursor {
                        keep.insert(cursor);
                        cursor = patch.before_revision();
                    }
                }
            }
        }
        let mut retired = Vec::new();
        for (is_redo, stack) in [(false, &mut self.undo), (true, &mut self.redo)] {
            for action in stack.iter_mut() {
                for part in std::mem::take(action) {
                    if matches!(&part, Part::MarkerEdit { patch, .. } if patch.document_id() == id && (is_redo || !keep.contains(&patch.after_revision()))) { retired.push(part); }
                    else { action.push(part); }
                }
            }
            stack.retain(|action| !action.is_empty());
        }
        self.merge_open = false;
        if !retired.is_empty() { drop_later(Some(retired)); }
    }

    /// Folders the Explorer expanded on its own (revealing the open file): taken into the
    /// base, no action.
    pub(super) fn absorb_expanded(&mut self, added: Vec<PathBuf>) {
        if let Some(b) = self.base.as_mut() {
            b.folders.expanded.extend(added);
        }
    }

    /// Tests: the kinds of the parts of each undo action, oldest first ("Frame+Range", …).
    #[cfg(test)]
    pub(super) fn kinds(&self) -> Vec<String> {
        let name = |p: &Part| match p {
            Part::Frame { .. } => "Frame",
            Part::Camera { .. } => "Camera",
            Part::Selection { .. } => "Selection",
            Part::PanelSetting { .. } => "PanelSetting",
            Part::Range { .. } => "Range",
            Part::Plate { .. } => "Plate",
            Part::Speed { .. } => "Speed",
            Part::Folds { .. } => "Folds",
            Part::Channels { .. } => "Channels",
            Part::Comment { .. } => "Comment",
            Part::MarkerEdit { .. } => "MarkerEdit",
            Part::Layout { .. } => "Layout",
            Part::Options { .. } => "Options",
            Part::Folders { .. } => "Folders",
            Part::Sort { .. } => "Sort",
        };
        self.undo.iter().map(|a| a.iter().map(name).collect::<Vec<_>>().join("+")).collect()
    }
}

/// Can an action be done now?
enum Ready {
    Go,
    /// A file it needs is being read: wait (name of the file).
    Wait(String),
    /// It cannot be done any more: drop it.
    Gone,
}

/// The side an undo (`redo` false) or a redo puts back.
fn pick<T>(redo: bool, before: T, after: T) -> T {
    if redo {
        after
    } else {
        before
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// What a comment part does going back (`redo` false) or forward: (line kind, comment).
fn comment_kind(change: &Change, redo: bool) -> (EventKind, &Comment) {
    match (change, redo) {
        (Change::Added(c), false) | (Change::Deleted(c), true) => (EventKind::Delete, c),
        (Change::Deleted(c), false) | (Change::Added(c), true) => (EventKind::Add, c),
        (Change::Edited { before, after }, redo) => (EventKind::Edit, pick(redo, before, after)),
    }
}

/// The comment as it must be now for a comment part to be done: an edit needs the other side
/// of it (nobody changed it since); an add needs it gone, a delete needs it there (`None` =
/// only presence counts).
fn comment_now(change: &Change, redo: bool) -> Option<&Comment> {
    match change {
        Change::Edited { before, after } => Some(pick(redo, after, before)),
        _ => None,
    }
}

impl TakViewerApp {
    /// A save changes the path associated with the same editable document, not its history.
    pub(super) fn rebind_saved_history(&mut self, old: &Path, new: &Path, document: DocumentId) {
        fn layout(snap: &mut LayoutSnap, old: &Path, new: &Path, id: DocumentId) {
            for k in 0..MAX_VIEWS {
                let same = snap.documents[k].as_ref().is_some_and(|s| s.lock().ok().is_some_and(|d| d.id() == id));
                if same && snap.files[k].as_deref() == Some(old) {
                    snap.files[k] = Some(new.to_path_buf());
                    if let Some(spot) = &mut snap.spots[k] { spot.path = new.to_string_lossy().into_owned(); }
                }
            }
        }
        for action in self.history.undo.iter_mut().chain(&mut self.history.redo) {
            for part in action {
                match part {
                    Part::Frame { file, .. } | Part::Camera { file, .. } | Part::Selection { file, .. } | Part::PanelSetting { file, .. }
                    | Part::Range { file, .. } | Part::Plate { file, .. } | Part::Speed { file, .. }
                    | Part::Folds { file, .. } | Part::Channels { file, .. } | Part::Comment { file, .. } => {
                        if file == old { *file = new.to_path_buf(); }
                    }
                    Part::MarkerEdit { file, patch } => {
                        if file == old && patch.document_id() == document { *file = new.to_path_buf(); }
                    }
                    Part::Layout { before, after } => { layout(before, old, new, document); layout(after, old, new, document); }
                    _ => {}
                }
            }
        }
        if let Some(base) = &mut self.history.base {
            layout(&mut base.layout, old, new, document);
            for item in base.views.iter_mut().flatten() { if item.file == old { item.file = new.to_path_buf(); } }
        }
    }

    /// The state now, all of it (first frame, or taken as it is).
    fn base_now(&self) -> Base {
        let views: [Option<FileBase>; MAX_VIEWS] = std::array::from_fn(|k| FileBase::of(&self.views[k]));
        let ranges = std::array::from_fn(|k| self.views[k].range);
        Base { layout: self.layout_now(ranges), views, options: Options::of(&self.settings), folders: Folders::of(&self.workspace, &self.explorer), sort: self.comment_sort }
    }

    fn layout_now(&self, ranges: [Range; MAX_VIEWS]) -> LayoutSnap {
        LayoutSnap {
            dock: self.dock.clone(),
            shape: dock::shape(&self.dock),
            hidden: self.hidden.clone(),
            files: std::array::from_fn(|k| self.views[k].target().map(Path::to_path_buf)),
            spots: std::array::from_fn(|k| layout_spot(&self.views[k])),
            picked: std::array::from_fn(|k| if pending_view(&self.views[k]) { self.views[k].restore_picked.unwrap_or(None) } else { self.views[k].picked }),
            documents: std::array::from_fn(|k| layout_document(&self.views[k])),
            edit_selections: std::array::from_fn(|k| layout_edit_selection(&self.views[k])),
            ranges: std::array::from_fn(|k| layout_range(&self.views[k], ranges[k])),
            active: self.active,
            overlays: std::array::from_fn(|k| layout_overlay(&self.views[k]).map(Path::to_path_buf)),
            shown: std::array::from_fn(|k| self.views[k].shown),
        }
    }

    /// Closing a playing view keeps the exact last displayed frame too. Snapshot only small
    /// viewing state; files and buffers still retire off the drawing thread.
    pub(super) fn remember_layout_spot(&mut self, slot: usize) {
        let v = &self.views[slot];
        if let Some(base) = self.history.base.as_mut().filter(|b| b.layout.files[slot].as_deref() == v.target()) {
            base.layout.spots[slot] = layout_spot(v);
            base.layout.picked[slot] = if pending_view(v) { v.restore_picked.unwrap_or(None) } else { v.picked };
            base.layout.documents[slot] = layout_document(v);
            base.layout.edit_selections[slot] = layout_edit_selection(v);
            base.layout.ranges[slot] = layout_range(v, v.range);
            base.layout.overlays[slot] = layout_overlay(v).map(Path::to_path_buf);
            if let Some(file) = base.views[slot].as_mut().filter(|file| Some(file.file.as_path()) == v.target()) {
                file.range = base.layout.ranges[slot];
            }
        }
    }

    /// End of every frame: record what a person changed (see the module text). Not while a
    /// mouse button is down, a text field has the keyboard or a capture runs.
    pub(super) fn observe_frame(&mut self, ctx: &egui::Context) {
        if self.capture.is_some() || ctx.input(|i| i.pointer.any_down()) || self.typing(ctx) {
            return;
        }
        self.observe(true);
    }

    /// Compare the state with the base; `record` = push what changed as actions (false = only
    /// take it as it is, e.g. after an undo). Command runs never record.
    pub(super) fn observe(&mut self, record: bool) {
        let record = record && !self.no_persist;
        let Some(mut base) = self.history.base.take() else {
            self.history.base = Some(Box::new(self.base_now()));
            return;
        };
        if std::mem::take(&mut self.history.quiet_all) {
            *base = self.base_now();
            self.history.base = Some(base);
            return;
        }
        let mut file_parts = Vec::new();
        let mut app_parts = Vec::new();
        // The document arrives after a markers-first open. Retain its handle in the existing
        // layout snapshot, so a later close can restore it without a full data snapshot.
        for k in 0..MAX_VIEWS {
            let v = &self.views[k];
            if base.layout.files[k].as_deref() == v.path() && v.edit_document.is_some()
                && !base.layout.documents[k].as_ref().zip(v.edit_document.as_ref()).is_some_and(|(a, b)| Arc::ptr_eq(a, b)) {
                base.layout.documents[k] = v.edit_document.clone();
            }
        }
        let marker_edit = self.marker_edit();
        let frame_edit = self.frame_edit();
        // Layout and files first: their "before" takes each file's range as last recorded.
        let quiet_layout = std::mem::take(&mut self.history.quiet_layout);
        let shape = dock::shape(&self.dock);
        let same_files = (0..MAX_VIEWS).all(|k| {
            let v = &self.views[k];
            v.target() == base.layout.files[k].as_deref() && layout_overlay(v) == base.layout.overlays[k].as_deref() && v.shown == base.layout.shown[k]
        });
        if shape != base.layout.shape || self.hidden != base.layout.hidden || !same_files {
            let ranges = std::array::from_fn(|k| match &base.views[k] {
                Some(b) if Some(b.file.as_path()) == self.views[k].target() => b.range,
                _ => self.views[k].range,
            });
            let after = self.layout_now(ranges);
            let mut before = std::mem::replace(&mut base.layout, after.clone());
            for k in 0..MAX_VIEWS {
                if let Some(b) = base.views[k].as_ref().filter(|b| Some(&b.file) == before.files[k].as_ref()) {
                    before.ranges[k] = b.range;
                }
            }
            if record && !quiet_layout {
                app_parts.push(Part::Layout { before: Box::new(before), after: Box::new(after) });
            }
        }
        for k in 0..MAX_VIEWS {
            let v = &mut self.views[k];
            if v.data.is_none() {
                base.views[k] = None;
                continue;
            }
            if v.loading.is_some() {
                // Another file is coming: the shown one keeps its base until it is replaced.
                continue;
            }
            if v.fresh {
                // Just shown with its spot / framing: taken as it is once the framing is done.
                if !v.fit_pending {
                    v.fresh = false;
                    base.views[k] = FileBase::of(v);
                    if base.layout.files[k].as_deref() == v.path() { base.layout.edit_selections[k] = Some(v.edit_selection_state()); }
                }
                continue;
            }
            let Some(b) = base.views[k].as_mut().filter(|b| b.file_id == v.file_id) else {
                base.views[k] = FileBase::of(v);
                if base.layout.files[k].as_deref() == v.path() { base.layout.edit_selections[k] = Some(v.edit_selection_state()); }
                continue;
            };
            let mut parts = Vec::new();
            // A Frames edit whose boxes were dragged (v0.17.3): the frame is its draft until
            // Enter (one action with the comment) or Esc (put back), like the marker edit below.
            let frame_draft = frame_edit == Some((k, v.file_id));
            if !frame_draft && !v.pb.playing && v.pb.frame != b.frame {
                parts.push(Part::Frame { file: b.file.clone(), before: b.frame, after: v.pb.frame });
                b.frame = v.pb.frame;
            }
            if v.cam != b.cam {
                parts.push(Part::Camera { file: b.file.clone(), before: b.cam, after: v.cam });
                b.cam = v.cam;
            }
            // The marker edit of a comment (v0.17.1): the selection is its draft until Enter
            // (one action with the comment) or Esc (put back), so it is not compared meanwhile.
            let drafting = marker_edit == Some((k, v.file_id));
            let order = v.ordered_selection();
            if !drafting && (v.selected != b.selected || order != b.order || v.primary != b.primary || v.picked != b.picked || v.armed_label != b.armed_label) {
                let before = Selection { selected: std::mem::replace(&mut b.selected, v.selected.clone()), order: std::mem::replace(&mut b.order, order), primary: b.primary, comment: b.picked, armed_label: b.armed_label };
                b.primary = v.primary;
                b.picked = v.picked;
                b.armed_label = v.armed_label;
                parts.push(Part::Selection { file: b.file.clone(), before, after: v.selection() });
                if base.layout.files[k].as_deref() == v.path() { base.layout.edit_selections[k] = Some(v.edit_selection_state()); }
            }
            if v.range != b.range {
                parts.push(Part::Range { file: b.file.clone(), before: b.range, after: v.range });
                b.range = v.range;
            }
            if v.plate != b.plate {
                parts.push(Part::Plate { file: b.file.clone(), before: b.plate, after: v.plate });
                b.plate = v.plate;
            }
            if v.pb.speed != b.speed {
                parts.push(Part::Speed { file: b.file.clone(), before: b.speed, after: v.pb.speed });
                b.speed = v.pb.speed;
            }
            let folds = v.data.as_ref().map(|data| fold_keys(data, &v.labels.collapsed)).unwrap_or_default();
            if folds != b.folds {
                let before = std::mem::replace(&mut b.folds, folds.clone());
                parts.push(Part::Folds { file: b.file.clone(), before, after: folds });
            }
            if v.devices.channel != b.channels {
                let before = std::mem::replace(&mut b.channels, v.devices.channel.clone());
                parts.push(Part::Channels { file: b.file.clone(), before, after: v.devices.channel.clone() });
            }
            if record {
                file_parts.extend(parts);
            }
        }
        for k in 0..MAX_VIEWS {
            let v = &self.views[k];
            if v.loading.is_none() && v.remote_loading.is_none() && base.layout.files[k].as_deref() == v.path() {
                base.layout.spots[k] = v.spot();
            }
        }
        if !base.options.same(&self.settings) {
            let after = Options::of(&self.settings);
            let before = std::mem::replace(&mut base.options, after.clone());
            if record {
                app_parts.push(Part::Options { before: Box::new(before), after: Box::new(after) });
            }
        }
        if !base.folders.same(&self.workspace, &self.explorer) {
            let after = Folders::of(&self.workspace, &self.explorer);
            let before = std::mem::replace(&mut base.folders, after.clone());
            if record {
                app_parts.push(Part::Folders { before: Box::new(before), after: Box::new(after) });
            }
        }
        if base.sort != self.comment_sort {
            let before = std::mem::replace(&mut base.sort, self.comment_sort);
            if record {
                app_parts.push(Part::Sort { before, after: self.comment_sort });
            }
        }
        self.history.base = Some(base);
        // What belongs to files first, then what belongs to the whole app.
        self.history.push(file_parts);
        self.history.push(app_parts);
    }

    /// A comment was added, edited or deleted in view `slot` (its line is on its way to the
    /// file): one action, with the picked range it cleared and the change of the selection it
    /// made (v0.17.1: a new comment is selected, a deleted one is not any more; the marker
    /// selection picked during a marker edit) and the frame a dragged frame box of a Frames edit
    /// moved (v0.17.3). Call `observe(true)` before the change.
    pub(super) fn push_comment(&mut self, slot: usize, change: Change, cleared: Range) {
        let v = &self.views[slot];
        let Some(file) = v.path().map(Path::to_path_buf) else { return };
        let (range, now) = (v.range, v.selection());
        let mut parts = vec![Part::Comment { file: file.clone(), change, cleared }];
        if let Some(b) = self.history.base.as_mut().and_then(|b| b.views[slot].as_mut()).filter(|b| b.file_id == v.file_id) {
            b.range = range;
            if !v.pb.playing && b.frame != v.pb.frame {
                parts.push(Part::Frame { file: file.clone(), before: b.frame, after: v.pb.frame });
                b.frame = v.pb.frame;
            }
            let before = Selection { selected: b.selected.clone(), order: b.order.clone(), primary: b.primary, comment: b.picked, armed_label: b.armed_label };
            if before != now {
                (b.selected, b.order, b.primary, b.picked, b.armed_label) = (now.selected.clone(), now.order.clone(), now.primary, now.comment, now.armed_label);
                parts.push(Part::Selection { file, before, after: now });
            }
        }
        if !self.no_persist {
            self.history.push(parts);
        }
    }

    /// Publish a prepared marker edit as exactly one global history action. Stale/no-op jobs
    /// never call `History::push`; selection is carried inside the same patch.
    pub(super) fn commit_marker_edit(&mut self, slot: usize, patch: Arc<EditPatch>) -> Result<(), EditError> {
        if self.no_persist { return Err(EditError::Other("명령 실행에서는 편집할 수 없어요".into())); }
        self.observe(true);
        let file = self.views.get(slot).and_then(View::path).ok_or(EditError::NotLoaded)?.to_path_buf();
        self.views[slot].apply_marker_patch(&patch, true)?;
        self.history.push(vec![Part::MarkerEdit { file, patch }]);
        self.observe(false);
        Ok(())
    }

    /// Apply a finished refresh without recording its selection remap as a second user action.
    pub(super) fn publish_marker_view(&mut self, slot: usize, data: tak_core::view_data::ViewData) -> Result<(), EditError> {
        self.observe(true);
        self.views.get_mut(slot).ok_or(EditError::NotLoaded)?.publish_edited_view(data)?;
        self.observe(false);
        Ok(())
    }

    /// Called only after the user chooses Discard. Other files' global history is kept.
    pub(super) fn discard_marker_edits(&mut self, slot: usize) -> Result<(), EditError> {
        self.observe(true);
        let view = self.views.get_mut(slot).ok_or(EditError::NotLoaded)?;
        let session = view.edit_document.as_ref().ok_or(EditError::NotLoaded)?;
        let mut document = session.lock().map_err(|_| EditError::Other("편집 문서를 열 수 없어요".into()))?;
        let discarded = document.snapshot();
        let id = document.id();
        let saved = document.saved_revision();
        document.discard_unsaved();
        view.edit_refresh = Some(document.revision());
        view.edit_selection = None;
        drop(document);
        drop_later(Some(discarded));
        self.history.forget_marker_document(id, saved);
        self.observe(false);
        Ok(())
    }

    /// A double click on the `#` of a COMMENTS row (v0.27.3): its first click selected comment
    /// `id` in view `slot`, and that selection was recorded as the newest action (number
    /// `number` of `History::pushed`, a lone selection part of this file). It is taken out
    /// again and the base gets the selection from before it, so the go-to of the second click
    /// and that selection are recorded as one action at the end of the frame (frame +
    /// selection, as one row click was before v0.27.3). Anything else: nothing done (the go-to is
    /// then an action of its own).
    pub(super) fn reopen_row_select(&mut self, slot: usize, id: u64, number: u64) {
        let h = &mut self.history;
        if !h.merge_open || h.pushed != number {
            return;
        }
        let v = &self.views[slot];
        let Some(file) = v.path() else { return };
        let ours = matches!(h.undo.last().map(Vec::as_slice), Some([Part::Selection { file: f, after, .. }]) if f == file && after.comment == Some(id));
        let Some(b) = h.base.as_mut().and_then(|b| b.views[slot].as_mut()).filter(|b| ours && b.file_id == v.file_id) else { return };
        if let Some(Some(Part::Selection { before, .. })) = h.undo.pop().map(|mut a| a.pop()) {
            (b.selected, b.order, b.primary, b.picked, b.armed_label) = (before.selected, before.order, before.primary, before.comment, before.armed_label);
        }
    }

    /// The view showing (or reading) this file.
    fn slot_of(&self, file: &Path) -> Option<usize> {
        (0..MAX_VIEWS).find(|&k| self.views[k].target() == Some(file))
    }

    fn part_slot(&self, part: &Part) -> Option<usize> {
        if let Part::MarkerEdit { patch, .. } = part {
            return self.views.iter().position(|v| {
                v.edit_document.iter().chain(v.restore_edit_document.iter()).any(|s| s.lock().ok().is_some_and(|d| d.id() == patch.document_id()))
            });
        }
        part.file().and_then(|file| self.slot_of(file))
    }

    fn ready(&self, parts: &[Part], redo: bool) -> Ready {
        for p in parts {
            if let Part::Layout { before, after } = p {
                let to = if redo { after } else { before };
                for (k, f) in to.files.iter().enumerate() {
                    if let Some(f) = f {
                        if self.views[k].target() != Some(f.as_path()) && self.slot_of(f).is_none() {
                            if remote_ui::is_remote_key(f) {
                                let cached = self.ssh_cache.iter().any(|c| c.key == *f);
                                let connected = self.ssh_sources.get(f).is_some_and(|l| self.ssh.is_connected(&l.host));
                                if !cached && !connected { return Ready::Gone; }
                            } else if !f.is_file() { return Ready::Gone; }
                        }
                    }
                }
                // A laid-over file to read again must still be there (plan step 7).
                for f in to.overlays.iter().flatten() {
                    if !self.views.iter().any(|v| v.overlay_paths().any(|p| p == f)) && !f.is_file() {
                        return Ready::Gone;
                    }
                }
                continue;
            }
            let Some(file) = p.file() else { continue };
            let Some(k) = self.part_slot(p) else { return Ready::Gone };
            let v = &self.views[k];
            if v.remote_loading.is_some() || v.loading.is_some() || v.data.is_none() {
                return Ready::Wait(file_name(file));
            }
            if let Part::MarkerEdit { patch, .. } = p {
                let expected = if redo { patch.before_revision() } else { patch.after_revision() };
                if !v.edit_document.as_ref().and_then(|s| s.lock().ok()).is_some_and(|d| d.id() == patch.document_id() && d.revision() == expected) { return Ready::Gone; }
            }
            if let Part::Comment { change, .. } = p {
                match &v.comments.target {
                    None => return Ready::Wait(file_name(file)),
                    Some(Err(_)) => return Ready::Gone,
                    Some(Ok(_)) if self.no_persist => return Ready::Gone,
                    Some(Ok(_)) => {}
                }
                let (kind, c) = comment_kind(change, redo);
                let ok = match comment_now(change, redo) {
                    Some(now) => v.comments.list.get(c.id) == Some(now),
                    None => v.comments.list.get(c.id).is_some() == (kind == EventKind::Delete),
                };
                if !ok {
                    return Ready::Gone;
                }
            }
        }
        Ready::Go
    }

    /// Ctrl+Z (`redo` false) takes back the newest action; Ctrl+Y (`redo` true) does the newest
    /// undone one again. What was changed just before is recorded first. An action on a file
    /// being read waits; one that cannot be done any more is dropped and the next one is taken.
    pub(super) fn undo_redo(&mut self, ctx: &egui::Context, redo: bool) {
        if self.open_views().into_iter().any(|slot| self.marker_saving(slot)) {
            self.history.note = Some(("저장 중에는 되돌릴 수 없어요.".into(), ctx.input(|i| i.time)));
            return;
        }
        // Playback stops first: the play run so far is an action (then undone, or it drops the
        // redo side like any new action).
        for v in &mut self.views {
            v.pb.playing = false;
        }
        self.observe(true);
        self.history.merge_open = false;
        let now = ctx.input(|i| i.time);
        let word = if redo { "Redo" } else { "Undo" };
        let mut skipped = 0;
        loop {
            let list = if redo { &self.history.redo } else { &self.history.undo };
            let Some(top) = list.last() else {
                if skipped > 0 {
                    self.history.note = Some((format!("{word} · {skipped} skipped"), now));
                }
                return;
            };
            let dirty_removed = top.iter().find_map(|part| {
                let Part::Layout { before, after } = part else { return None };
                let target = if redo { after } else { before };
                self.open_views().into_iter().find(|&slot| {
                    let view = &self.views[slot];
                    if !view.marker_dirty() && !self.comments_unsaved(slot) && !self.marker_job_running(slot) { return false; }
                    let id = view.edit_document.as_ref().and_then(|s| s.lock().ok().map(|d| d.id()));
                    !target.documents.iter().flatten().any(|s| s.lock().ok().is_some_and(|d| Some(d.id()) == id))
                })
            });
            if let Some(slot) = dirty_removed {
                self.guard_dirty(slot, marker_edit::PendingTransition::Undo { redo });
                return;
            }
            match self.ready(top, redo) {
                Ready::Wait(name) => {
                    self.history.note = Some((format!("Wait · loading {name}"), now));
                    return;
                }
                Ready::Gone => {
                    if redo {
                        self.history.redo.pop();
                    } else {
                        self.history.undo.pop();
                    }
                    skipped += 1;
                }
                Ready::Go => {
                    let parts = if redo { self.history.redo.pop() } else { self.history.undo.pop() }.unwrap_or_default();
                    let mut words: Vec<String> = parts.iter().map(|p| self.apply_part(ctx, p, redo, now)).collect();
                    // A comment added or deleted carries the change of the selected comment it
                    // made: the comment's own words say enough.
                    if parts.iter().any(|p| matches!(p, Part::Comment { .. })) {
                        for (p, w) in parts.iter().zip(words.iter_mut()) {
                            if matches!(p, Part::Selection { before, after, .. } if (&before.selected, &before.order, before.primary) == (&after.selected, &after.order, after.primary)) {
                                w.clear();
                            }
                        }
                        words.retain(|w| !w.is_empty());
                    }
                    if redo {
                        self.history.undo.push(parts);
                    } else {
                        self.history.redo.push(parts);
                    }
                    // The base takes what this did (not an action of its own).
                    self.observe(false);
                    let more = if skipped > 0 { format!(" · {skipped} skipped") } else { String::new() };
                    self.history.note = Some((format!("{word} · {}{more}", words.join(" · ")), now));
                    return;
                }
            }
        }
    }

    /// Put one part's "before" (undo) or "after" (redo) side back; returns its status bar words.
    fn apply_part(&mut self, ctx: &egui::Context, part: &Part, redo: bool, now: f64) -> String {
        let slot = self.part_slot(part);
        match part {
            Part::Layout { before, after } => {
                let to = if redo { after } else { before };
                let words = self.layout_words(to);
                self.restoring_layout = true;
                self.restore_layout(ctx, to);
                self.restoring_layout = false;
                return words;
            }
            Part::Options { before, after } => {
                let (from, to) = if redo { (before, after) } else { (after, before) };
                let s = &mut self.settings;
                s.scene = to.scene.clone();
                s.graph_float = to.graph_float;
                s.graph_opacity = to.opacity;
                s.graph_boxes = to.graph_boxes.clone();
                s.plate_boxes = to.plate_boxes.clone();
                s.plate_channels = to.plate_channels.clone();
                return from.words(to);
            }
            Part::Sort { before, after } => {
                self.comment_sort = pick(redo, *before, *after);
                return self.comment_sort.words();
            }
            Part::Folders { before, after } => {
                let to = if redo { after } else { before };
                self.workspace.restore(to.roots.clone(), to.names.clone());
                self.explorer.set_folds(&mut self.workspace, to.expanded.clone(), to.section_collapsed);
                self.explorer.editors_collapsed = to.editors_collapsed;
                return "Folders".into();
            }
            _ => {}
        }
        let Some(k) = slot else { return String::new() };
        match part {
            Part::Frame { before, after, .. } => {
                let v = &mut self.views[k];
                v.pb.playing = false;
                v.pb.set_frame(pick(redo, *before, *after), now);
                format!("Frame {}", v.pb.frame)
            }
            Part::Camera { before, after, .. } => {
                self.views[k].cam = pick(redo, *before, *after);
                "Camera".into()
            }
            Part::Selection { before, after, .. } => {
                let v = &mut self.views[k];
                v.set_selection(if redo { after } else { before }.clone());
                // Only the selected comment changed (v0.17.1).
                if (&before.selected, &before.order, before.primary) == (&after.selected, &after.order, after.primary) {
                    v.picked_words()
                } else {
                    v.selection_words()
                }
            }
            Part::PanelSetting { before, after, .. } => {
                self.apply_panel_setting(k, if redo { after } else { before });
                "Panel setting".into()
            }
            Part::Range { before, after, .. } => {
                let v = &mut self.views[k];
                v.range = pick(redo, *before, *after);
                views::range_words(v.range)
            }
            Part::Plate { before, after, .. } => {
                let v = &mut self.views[k];
                v.plate = pick(redo, *before, *after);
                match v.plate.and_then(|p| v.data.as_ref()?.plates.get(p)) {
                    Some(p) => format!("Plate {}", p.name),
                    None => "Plate none".into(),
                }
            }
            Part::Speed { before, after, .. } => {
                let v = &mut self.views[k];
                v.pb.speed = pick(redo, *before, *after);
                format!("Speed {}x", v.pb.speed)
            }
            Part::Folds { before, after, .. } => {
                let folds = if redo { after } else { before };
                let view = &mut self.views[k];
                view.labels.collapsed = view.data.as_ref().map(|data| fold_indices(data, folds)).unwrap_or_default();
                "Labeling".into()
            }
            Part::Channels { before, after, .. } => {
                self.views[k].devices.channel = if redo { after } else { before }.clone();
                "Device channel".into()
            }
            Part::Comment { change, cleared, .. } => {
                let (kind, c) = comment_kind(change, redo);
                let c = c.clone();
                self.write_change(ctx, k, kind, &c);
                if cleared.is_some() {
                    self.views[k].range = if redo { None } else { *cleared };
                }
                let word = match kind {
                    EventKind::Add => "added",
                    EventKind::Edit => "edited",
                    EventKind::Delete => "removed",
                };
                format!("Comment {} {word}", c.id)
            }
            Part::MarkerEdit { patch, .. } => {
                let started = Instant::now();
                let result = self.views[k].apply_marker_patch(patch, redo);
                marker_edit::trace_timing(if redo { "redo commit" } else { "undo commit" }, started);
                match result {
                    Ok(()) => if patch.result.message.is_empty() { "Marker edit restored".into() } else { format!("{} · restored", patch.result.message) },
                    Err(_) => "Marker edit failed".into(),
                }
            }
            Part::Layout { .. } | Part::Options { .. } | Part::Folders { .. } | Part::Sort { .. } => String::new(),
        }
    }

    /// Words for putting a layout back: the files that come (`Open a.tak`) or go
    /// (`Close a.tak`), else `Layout`.
    fn layout_words(&self, to: &LayoutSnap) -> String {
        let mut words = Vec::new();
        for k in 0..MAX_VIEWS {
            let now = self.views[k].target();
            if now == to.files[k].as_deref() {
                continue;
            }
            match (&to.files[k], now) {
                (Some(f), _) => words.push(format!("Open {}", file_name(f))),
                (None, Some(f)) => words.push(format!("Close {}", file_name(f))),
                (None, None) => {}
            }
        }
        // Plan step 7: a file laid over a view comes (`Overlay b.tak`) or goes (`Overlay off`);
        // else a file's markers shown or hidden there (`b.tak shown` / `a.tak hidden`).
        for k in 0..MAX_VIEWS {
            let v = &self.views[k];
            if v.overlay_path() != to.overlays[k].as_deref() {
                words.push(match &to.overlays[k] {
                    Some(f) => format!("Overlay {}", file_name(f)),
                    None => "Overlay off".into(),
                });
                continue;
            }
            let files = [to.files[k].as_deref(), to.overlays[k].as_deref()];
            for i in 0..2 {
                if v.shown[i] != to.shown[k][i] {
                    let name = files[i].map(file_name).unwrap_or_default();
                    words.push(format!("{name} {}", if to.shown[k][i] { "shown" } else { "hidden" }));
                }
            }
        }
        if words.is_empty() {
            "Layout".into()
        } else {
            words.join(" · ")
        }
    }

    /// Put a layout back: the panes, the hidden sides, the file of each view (read again with
    /// its remembered spot and the range it had) and the active view. A past layout always kept
    /// the rules (at most 4 views, no file twice), and the whole of it comes back, so they hold.
    fn restore_layout(&mut self, ctx: &egui::Context, snap: &LayoutSnap) {
        self.dock = snap.dock.clone();
        dock::place_windows(&mut self.dock);
        self.hidden = snap.hidden.clone();
        let open = self.open_views();
        for k in 0..MAX_VIEWS {
            let want = snap.files[k].as_ref().filter(|_| open.contains(&k));
            if self.views[k].target() == want.map(PathBuf::as_path) {
                continue;
            }
            match want {
                None => {
                    self.cancel_remote_load(k);
                    self.remember(k);
                    drop_later(self.views[k].clear());
                }
                Some(p) => {
                    self.open_in(ctx, k, p.clone());
                    self.views[k].restore_edit_document = snap.documents[k].clone();
                    self.views[k].restore_edit_selection = snap.edit_selections[k].clone();
                    if self.views[k].remote.is_some() && self.views[k].remote_loading.is_none() {
                        if let Some(spot) = &snap.spots[k] { self.views[k].apply_spot(spot, ctx.input(|i| i.time)); }
                        self.views[k].range = snap.ranges[k];
                        self.views[k].picked = snap.picked[k];
                    } else {
                        self.views[k].restore = snap.spots[k].clone().or_else(|| self.views[k].restore.take());
                        self.views[k].restore_range = Some(snap.ranges[k]);
                        self.views[k].restore_picked = Some(snap.picked[k]);
                    }
                }
            }
        }
        // The file laid over each view (plan step 7; read again, arriving quietly, checked once
        // the view's own file is in) and whose markers were shown.
        for k in 0..MAX_VIEWS {
            let want = snap.overlays[k].as_ref().filter(|_| open.contains(&k));
            let v = &mut self.views[k];
            match want {
                None => drop_later(v.take_overlay()),
                Some(p) if v.overlay_path() == Some(p.as_path()) => v.overlay_load = None,
                Some(p) if v.overlay_load.as_ref().is_some_and(|l| &l.path == p) => {
                    if let Some(l) = v.overlay_load.as_mut() {
                        l.quiet = true;
                    }
                }
                Some(p) => self.start_overlay(ctx, k, p.clone(), true),
            }
            self.views[k].shown = snap.shown[k];
        }
        if let Some(active) = open.iter().copied().find(|k| *k == snap.active).or_else(|| open.first().copied()) {
            self.activate(active);
        } else {
            self.active = 0;
            self.focused_tab = self.dock.find_active_focused().map(|(_, pane)| *pane);
        }
    }

    /// Tests: take everything as it is now (setup that is not a person's action).
    #[cfg(test)]
    pub(super) fn quiet(&mut self) {
        self.observe(false);
    }
}

#[cfg(test)]
mod marker_edit_tests {
    use super::*;
    use std::sync::mpsc::channel;
    use tak_core::edit::{AffectedFrames, DocumentInput, EditResult, EditSelection, LabelKey, RawSample, RawTrack, RecordBytes, TrackChange};
    use tak_core::view_data::ViewData;

    fn show(app: &mut TakViewerApp, slot: usize, path: &str, id: usize) {
        show_names(app, slot, path, id, &["M"]);
    }
    fn show_names(app: &mut TakViewerApp, slot: usize, path: &str, id: usize, names: &[&str]) {
        let (_tx, rx) = channel();
        app.views[slot].show(crate::views::sample_data(path, names), rx, id, 1.0, 0.0);
        app.views[slot].fresh = false;
        app.views[slot].fit_pending = false;
        let view = &app.views[slot];
        let take = &view.data.as_ref().unwrap().take;
        let tracks = take.trajectories.iter().map(|t| RawTrack {
            uid: t.uid, name: t.name.clone(), label: None,
            records: RecordBytes { node: Arc::from([1]), translation_header: Arc::from([2]), serialization_verified: true },
            samples: t.frames.iter().zip(&t.xyz).map(|(&f, &xyz)| RawSample::new(f, xyz)).collect::<Vec<_>>().into(),
        }).collect();
        let input = DocumentInput { streams: Vec::new().into(), nodes_prefix: Arc::from([]), channels_prefix: Arc::from([]), tracks, labels: Vec::new(), first_frame: 0, last_frame: 9 };
        let mut doc = view.edit_document.as_ref().unwrap().lock().unwrap();
        let revision = doc.revision();
        doc.install_input(revision, input).unwrap();
    }
    fn prepare(app: &TakViewerApp, slot: usize, value: f32) -> Arc<EditPatch> {
        let snapshot = app.views[slot].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let uid = [1; 16];
        let before = snapshot.track(&uid).unwrap().clone();
        let mut after = (*before).clone();
        let mut samples = after.samples.to_vec();
        samples[0] = RawSample::new(0, [value, 1.0, 0.0]);
        after.samples = samples.into();
        let mut selected = EditSelection::default();
        selected.selected.insert(uid);
        selected.primary = Some(uid);
        selected.armed_label = Some(LabelKey { parent: [8; 16], slot: 2 });
        Arc::new(EditPatch::prepare(&snapshot, vec![TrackChange { uid, before: Some(before), after: Some(Arc::new(after)) }], EditSelection::default(), selected, AffectedFrames { first: 0, last: 0 }, EditResult { tracks: 1, samples: 1, message: "Label M".into(), ..EditResult::default() }).unwrap())
    }
    fn app(ctx: &egui::Context) -> TakViewerApp { TakViewerApp::build(ctx, None, vec![], None, None) }

    #[test]
    fn undo_redo_restores_marker_selection_and_armed_label_together() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        show_names(&mut app, 0, "armed.tak", 1, &["A", "B"]);
        app.observe(false);
        let a = LabelKey { parent: [8; 16], slot: 1 };
        let b = LabelKey { parent: [8; 16], slot: 2 };
        app.views[0].select(0, false);
        app.views[0].armed_label = Some(a);
        app.observe(true);
        app.views[0].select(1, false);
        app.views[0].armed_label = Some(b);
        app.observe(true);
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[0].primary, Some(0));
        assert_eq!(app.views[0].armed_label, Some(a));
        app.undo_redo(&ctx, true);
        assert_eq!(app.views[0].primary, Some(1));
        assert_eq!(app.views[0].armed_label, Some(b));
    }

    #[test]
    fn pattern_fill_is_one_undo_step_and_keeps_selection_order() {
        use tak_core::edit::fill::{FillMethod, FillRequest, FillScope, NumericalFill};
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let mut take = crate::views::sample_take("pattern.tak", &["Target", "Reference"]);
        for index in [5, 4] {
            take.trajectories[0].frames.remove(index);
            take.trajectories[0].xyz.remove(index);
        }
        let (_tx, rx) = channel();
        app.views[0].show(ViewData::build(take.clone()), rx, 1, 1.0, 0.0);
        app.views[0].fresh = false;
        app.views[0].fit_pending = false;
        let tracks = take.trajectories.iter().map(|t| RawTrack {
            uid: t.uid, name: t.name.clone(), label: None,
            records: RecordBytes { node: Arc::from([1]), translation_header: Arc::from([2]), serialization_verified: true },
            samples: t.frames.iter().zip(&t.xyz).map(|(&f, &xyz)| RawSample::new(f, xyz)).collect::<Vec<_>>().into(),
        }).collect();
        let input = DocumentInput { streams: Vec::new().into(), nodes_prefix: Arc::from([]), channels_prefix: Arc::from([]), tracks, labels: Vec::new(), first_frame: 0, last_frame: 9 };
        {
            let mut doc = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
            let revision = doc.revision();
            doc.install_input(revision, input).unwrap();
        }
        app.views[0].select(1, false);
        app.views[0].select(0, true);
        let selection = app.views[0].selection();
        app.observe(false);
        let before = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let request = FillRequest::for_scope(&before, FillMethod::PatternBased, FillScope::Selected, Some((4, 5)), 75, app.views[0].edit_selection_state()).unwrap();
        let patch = Arc::new(NumericalFill.fill_detailed(&before, &request).unwrap().patch.unwrap());
        app.commit_marker_edit(0, patch.clone()).unwrap();
        assert_eq!(app.history.kinds(), vec!["MarkerEdit"]);
        assert_eq!(app.views[0].selection(), selection);
        assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().snapshot().track(&[1; 16]).unwrap().samples.len(), 10);
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[0].selection(), selection);
        assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().snapshot().track(&[1; 16]).unwrap().samples.len(), 8);
        app.undo_redo(&ctx, true);
        assert_eq!(app.views[0].selection(), selection);
        assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().snapshot().track(&[1; 16]).unwrap().samples.len(), 10);
    }

    #[test]
    fn dirty_close_replace_and_exit_leave_the_document_open_until_a_choice() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        show(&mut app, 0, "a.tak", 1);
        app.observe(false);
        app.commit_marker_edit(0, prepare(&app, 0, 2.0)).unwrap();
        assert!(app.view_label(0, &[0]).unwrap().ends_with('*'));
        assert!(app.editor_groups(&[0])[0].entries[0].dirty);
        app.close_editor(0);
        assert!(app.dirty_prompt.is_some());
        assert_eq!(app.views[0].path(), Some(Path::new("a.tak")));
        app.dirty_prompt = None;
        app.open_in(&ctx, 0, "b.tak".into());
        assert!(app.dirty_prompt.is_some());
        assert!(app.views[0].loading.is_none());
        app.dirty_prompt = None;
        app.request_exit(&ctx);
        assert!(app.dirty_prompt.is_some());
        assert!(!app.exit_approved);
        assert!(app.views[0].marker_dirty());
        app.dirty_prompt = None;
        let mut raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0))), focused: true, ..Default::default() };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
        let output = ctx.run(raw, |ctx| app.frame(ctx));
        assert!(output.viewport_output.get(&egui::ViewportId::ROOT).unwrap().commands.iter().any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
        assert!(app.dirty_prompt.is_some(), "window close is cancelled until the choice");
    }

    #[test]
    fn ctrl_w_dirty_marker_close_waits_for_the_existing_save_discard_cancel_choice() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        show(&mut app, 0, "a.tak", 1);
        show(&mut app, 1, "b.tak", 2);
        app.activate(1);
        app.observe(false);
        app.commit_marker_edit(1, prepare(&app, 1, 2.0)).unwrap();
        let before = app.history.undo.len();
        let event = egui::Event::Key { key: Key::W, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers { ctrl: true, command: true, ..Modifiers::NONE } };
        let _ = ctx.run(egui::RawInput { events: vec![event], ..Default::default() }, |ctx| app.handle_keys(ctx, 0.0));
        assert!(app.dirty_prompt.is_some());
        assert_eq!(app.open_views(), vec![0, 1]);
        assert!(app.views[1].marker_dirty());
        assert_eq!(app.history.undo.len(), before, "opening the close dialog is not an action");
        app.discard_pending_transition(&ctx);
        assert!(app.dirty_prompt.is_none());
        assert_eq!(app.open_views(), vec![0]);
        assert!(app.views[1].target().is_none());
        assert_eq!(app.views[0].path(), Some(Path::new("a.tak")));
    }

    #[test]
    fn undo_of_a_layout_cannot_remove_an_unsaved_document() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        show(&mut app, 0, "a.tak", 1);
        app.observe(false);
        app.commit_marker_edit(0, prepare(&app, 0, 2.0)).unwrap();
        let after = app.layout_now(std::array::from_fn(|k| app.views[k].range));
        let mut before = after.clone();
        before.files[0] = None;
        before.documents[0] = None;
        app.history.push(vec![Part::Layout { before: Box::new(before), after: Box::new(after) }]);
        let count = app.history.undo.len();
        app.undo_redo(&ctx, false);
        assert!(app.dirty_prompt.is_some());
        assert_eq!(app.history.undo.len(), count);
        assert_eq!(app.views[0].path(), Some(Path::new("a.tak")));
    }

    #[test]
    fn marker_edits_across_two_files_follow_global_order_and_savepoint() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        show(&mut app, 0, "a.tak", 1);
        show(&mut app, 1, "b.tak", 2);
        app.observe(false);
        let a = prepare(&app, 0, 2.0);
        app.commit_marker_edit(0, a.clone()).unwrap();
        {
            let mut doc = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
            let saved = doc.snapshot();
            doc.mark_saved(&saved, "a.takm".into()).unwrap();
        }
        let b = prepare(&app, 1, 3.0);
        app.commit_marker_edit(1, b.clone()).unwrap();
        assert_eq!(app.history.kinds(), vec!["MarkerEdit", "MarkerEdit"]);
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[1].edit_document.as_ref().unwrap().lock().unwrap().revision(), b.before_revision());
        app.undo_redo(&ctx, false);
        assert!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().is_dirty());
        app.undo_redo(&ctx, true);
        assert!(!app.views[0].edit_document.as_ref().unwrap().lock().unwrap().is_dirty());
        app.undo_redo(&ctx, true);
        assert_eq!(app.views[1].edit_document.as_ref().unwrap().lock().unwrap().revision(), b.after_revision());
        assert!(app.commit_marker_edit(0, a).is_err(), "stale job must not create a history action");
        assert_eq!(app.history.kinds(), vec!["MarkerEdit", "MarkerEdit"]);
    }

    #[test]
    fn close_undo_reopens_same_document_and_its_edit_can_be_undone() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let name = std::thread::current().name().unwrap_or("test").replace(':', "_");
        let path = std::env::temp_dir().join(format!("tak_phase01_close_{}_{name}.tak", std::process::id()));
        std::fs::write(&path, b"fixture").unwrap();
        let path_text = path.to_string_lossy().into_owned();
        show(&mut app, 0, &path_text, 1);
        app.observe(false);
        let patch = prepare(&app, 0, 5.0);
        app.commit_marker_edit(0, patch.clone()).unwrap();
        let identity = patch.document_id();
        drop(app.views[0].clear());
        app.observe(true);
        assert_eq!(app.history.kinds(), vec!["MarkerEdit", "Layout"]);
        app.hold_loads = true;
        app.undo_redo(&ctx, false);
        let tx = app.views[0].held.take().unwrap();
        tx.send(crate::views::LoadMsg::Markers(Ok((crate::views::sample_data(&path_text, &["M"]), vec![])))).unwrap();
        let loaded = app.views[0].poll_markers().unwrap();
        app.finish_load(&ctx, 0, loaded);
        app.views[0].fit_pending = false;
        app.views[0].fresh = false;
        assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().id(), identity);
        assert_eq!(app.views[0].edit_refresh, Some(patch.after_revision()));
        assert_eq!(app.views[0].selected.iter().copied().collect::<Vec<_>>(), vec![0]);
        assert_eq!(app.views[0].armed_label, Some(LabelKey { parent: [8; 16], slot: 2 }));
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().revision(), patch.before_revision());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn saved_revision_survives_the_global_five_hundred_action_cap() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        show(&mut app, 0, "a.tak", 1);
        app.observe(false);
        for n in 0..510 {
            let patch = prepare(&app, 0, n as f32 + 10.0);
            app.commit_marker_edit(0, patch).unwrap();
            if n == 9 {
                let mut doc = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
                let saved = doc.snapshot();
                doc.mark_saved(&saved, "a.takm".into()).unwrap();
            }
        }
        assert_eq!(app.history.undo.len(), MAX_UNDO);
        for _ in 0..MAX_UNDO { app.undo_redo(&ctx, false); }
        let doc = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
        assert!(!doc.is_dirty(), "saved revision survives history eviction");
        assert_eq!(app.history.redo.len(), MAX_UNDO);
    }

    #[test]
    fn discard_keeps_other_files_history_and_the_saved_ancestor() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        show(&mut app, 0, "a.tak", 1);
        show(&mut app, 1, "b.tak", 2);
        app.observe(false);
        let saved_patch = prepare(&app, 0, 2.0);
        app.commit_marker_edit(0, saved_patch.clone()).unwrap();
        {
            let mut doc = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
            let saved = doc.snapshot();
            doc.mark_saved(&saved, "a.takm".into()).unwrap();
        }
        let discarded = prepare(&app, 0, 3.0);
        app.commit_marker_edit(0, discarded).unwrap();
        let other = prepare(&app, 1, 4.0);
        app.commit_marker_edit(1, other.clone()).unwrap();
        app.discard_marker_edits(0).unwrap();
        assert_eq!(app.history.kinds(), vec!["MarkerEdit", "MarkerEdit"]);
        assert!(!app.views[0].edit_document.as_ref().unwrap().lock().unwrap().is_dirty());
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[1].edit_document.as_ref().unwrap().lock().unwrap().revision(), other.before_revision());
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().revision(), saved_patch.before_revision());
    }

    #[test]
    fn reopening_the_original_after_save_requests_the_saved_marker_state() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        show(&mut app, 0, "a.tak", 1);
        app.observe(false);
        let patch = prepare(&app, 0, 2.0);
        app.commit_marker_edit(0, patch.clone()).unwrap();
        let session = app.views[0].edit_document.as_ref().unwrap().clone();
        {
            let mut doc = session.lock().unwrap();
            let saved = doc.snapshot();
            doc.mark_saved(&saved, "a.takm".into()).unwrap();
        }
        drop(app.views[0].clear());
        app.views[0].restore_edit_document = Some(session);
        let (_tx, rx) = channel();
        app.views[0].show(crate::views::sample_data("a.tak", &["M"]), rx, 2, 1.0, 0.0);
        assert_eq!(app.views[0].edit_refresh, Some(patch.after_revision()));
        assert!(!app.views[0].edit_document.as_ref().unwrap().lock().unwrap().is_dirty());
    }

    #[test]
    fn stale_refresh_is_rejected_and_matching_refresh_updates_values_and_selection() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        show(&mut app, 0, "a.tak", 1);
        app.observe(false);
        let first = prepare(&app, 0, 2.0);
        app.commit_marker_edit(0, first.clone()).unwrap();
        let old = app.views[0].edit_refresh_snapshot().unwrap();
        let second = prepare(&app, 0, 3.0);
        app.commit_marker_edit(0, second).unwrap();
        let old_view = ViewData::rebuild_edited(crate::views::sample_take("a.tak", &["M"]), &old).unwrap();
        assert!(matches!(app.publish_marker_view(0, old_view), Err(EditError::StaleRevision)));
        let latest = app.views[0].edit_refresh_snapshot().unwrap();
        let wrong_file = ViewData::rebuild_edited(crate::views::sample_take("b.tak", &["M"]), &latest).unwrap();
        assert!(matches!(app.publish_marker_view(0, wrong_file), Err(EditError::WrongDocument)));
        let new_view = ViewData::rebuild_edited(crate::views::sample_take("a.tak", &["M"]), &latest).unwrap();
        app.publish_marker_view(0, new_view).unwrap();
        assert_eq!(app.views[0].data.as_ref().unwrap().markers[0].position(0).unwrap().x, 3.0);
        assert_eq!(app.views[0].selected.iter().copied().collect::<Vec<_>>(), vec![0]);
        assert_eq!(app.views[0].armed_label, Some(LabelKey { parent: [8; 16], slot: 2 }));
        assert_eq!(app.views[0].edit_refresh, None);
    }

    #[test]
    fn later_user_selection_survives_delayed_refresh_and_remains_undoable() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        show_names(&mut app, 0, "a.tak", 1, &["M", "N"]);
        app.observe(false);
        let patch = prepare(&app, 0, 3.0);
        app.commit_marker_edit(0, patch).unwrap();
        let snapshot = app.views[0].edit_refresh_snapshot().unwrap();
        app.views[0].select(1, false);
        let rebuilt = ViewData::rebuild_edited(crate::views::sample_take("a.tak", &["M", "N"]), &snapshot).unwrap();
        app.publish_marker_view(0, rebuilt).unwrap();
        assert_eq!(app.views[0].selected.iter().copied().collect::<Vec<_>>(), vec![1]);
        assert_eq!(app.history.kinds(), vec!["MarkerEdit", "Selection"]);
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[0].selected.iter().copied().collect::<Vec<_>>(), vec![0]);
    }
}
