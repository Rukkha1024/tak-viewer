//! Application-wide undo and redo records user actions in order. `observe` compares
//! small state and layout changes at the end of each frame; drags, text edits, and playback
//! are grouped. Automatic state updates are absorbed without recording an action. Comment
//! events are recorded when written. Consecutive frame or camera moves can be merged.
//! Overlay changes are layout actions and undo may reload the affected file.

use super::*;
use crate::views::{Range, Selection};
use std::collections::{HashMap, HashSet};

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
    primary: Option<usize>,
    /// The selected comment (v0.17.1).
    picked: Option<u64>,
    range: Range,
    plate: Option<usize>,
    speed: f64,
    folds: HashSet<usize>,
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
            primary: v.primary,
            picked: v.picked,
            range: v.range,
            plate: v.plate,
            speed: v.pb.speed,
            folds: v.labels.collapsed.clone(),
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
    /// The range each of those files had (a file read again gets it back).
    ranges: [Range; MAX_VIEWS],
    /// The active view then (put back with the layout; choosing a view is no action itself).
    active: usize,
    /// The file laid over each view slot (plan step 7; only once it is on screen) and whose
    /// markers were shown there ([own file, overlay]; both true without an overlay).
    overlays: [Option<PathBuf>; MAX_VIEWS],
    shown: [[bool; 2]; MAX_VIEWS],
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
    Range { file: PathBuf, before: Range, after: Range },
    Plate { file: PathBuf, before: Option<usize>, after: Option<usize> },
    Speed { file: PathBuf, before: f64, after: f64 },
    Folds { file: PathBuf, before: HashSet<usize>, after: HashSet<usize> },
    Channels { file: PathBuf, before: HashMap<usize, usize>, after: HashMap<usize, usize> },
    /// A comment added or deleted; `cleared` = the picked range adding it cleared.
    Comment { file: PathBuf, change: Change, cleared: Range },
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
            | Part::Range { file, .. }
            | Part::Plate { file, .. }
            | Part::Speed { file, .. }
            | Part::Folds { file, .. }
            | Part::Channels { file, .. }
            | Part::Comment { file, .. } => Some(file),
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
            self.undo.remove(0);
        }
        self.redo.clear();
        self.merge_open = true;
        self.pushed += 1;
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
            Part::Range { .. } => "Range",
            Part::Plate { .. } => "Plate",
            Part::Speed { .. } => "Speed",
            Part::Folds { .. } => "Folds",
            Part::Channels { .. } => "Channels",
            Part::Comment { .. } => "Comment",
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
            ranges,
            active: self.active,
            overlays: std::array::from_fn(|k| self.views[k].overlay_path().map(Path::to_path_buf)),
            shown: std::array::from_fn(|k| self.views[k].shown),
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
        let marker_edit = self.marker_edit();
        let frame_edit = self.frame_edit();
        // Layout and files first: their "before" takes each file's range as last recorded.
        let quiet_layout = std::mem::take(&mut self.history.quiet_layout);
        let shape = dock::shape(&self.dock);
        let same_files = (0..MAX_VIEWS).all(|k| {
            let v = &self.views[k];
            v.target() == base.layout.files[k].as_deref() && v.overlay_path() == base.layout.overlays[k].as_deref() && v.shown == base.layout.shown[k]
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
                }
                continue;
            }
            let Some(b) = base.views[k].as_mut().filter(|b| b.file_id == v.file_id) else {
                base.views[k] = FileBase::of(v);
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
            if !drafting && (v.selected != b.selected || v.primary != b.primary || v.picked != b.picked) {
                let before = Selection { selected: std::mem::replace(&mut b.selected, v.selected.clone()), primary: b.primary, comment: b.picked };
                b.primary = v.primary;
                b.picked = v.picked;
                parts.push(Part::Selection { file: b.file.clone(), before, after: v.selection() });
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
            if v.labels.collapsed != b.folds {
                let before = std::mem::replace(&mut b.folds, v.labels.collapsed.clone());
                parts.push(Part::Folds { file: b.file.clone(), before, after: v.labels.collapsed.clone() });
            }
            if v.devices.channel != b.channels {
                let before = std::mem::replace(&mut b.channels, v.devices.channel.clone());
                parts.push(Part::Channels { file: b.file.clone(), before, after: v.devices.channel.clone() });
            }
            if record {
                file_parts.extend(parts);
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
            let before = Selection { selected: b.selected.clone(), primary: b.primary, comment: b.picked };
            if before != now {
                (b.selected, b.primary, b.picked) = (now.selected.clone(), now.primary, now.comment);
                parts.push(Part::Selection { file, before, after: now });
            }
        }
        if !self.no_persist {
            self.history.push(parts);
        }
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
            (b.selected, b.primary, b.picked) = (before.selected, before.primary, before.comment);
        }
    }

    /// The view showing (or reading) this file.
    fn slot_of(&self, file: &Path) -> Option<usize> {
        (0..MAX_VIEWS).find(|&k| self.views[k].target() == Some(file))
    }

    fn ready(&self, parts: &[Part], redo: bool) -> Ready {
        for p in parts {
            if let Part::Layout { before, after } = p {
                let to = if redo { after } else { before };
                for (k, f) in to.files.iter().enumerate() {
                    if let Some(f) = f {
                        if self.views[k].target() != Some(f.as_path()) && self.slot_of(f).is_none() && !f.is_file() {
                            return Ready::Gone;
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
            let Some(k) = self.slot_of(file) else { return Ready::Gone };
            let v = &self.views[k];
            if v.loading.is_some() || v.data.is_none() {
                return Ready::Wait(file_name(file));
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
                            if matches!(p, Part::Selection { before, after, .. } if (&before.selected, before.primary) == (&after.selected, after.primary)) {
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
        let slot = part.file().and_then(|f| self.slot_of(f));
        match part {
            Part::Layout { before, after } => {
                let to = if redo { after } else { before };
                let words = self.layout_words(to);
                self.restore_layout(ctx, to);
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
                if (&before.selected, before.primary) == (&after.selected, after.primary) {
                    v.picked_words()
                } else {
                    v.selection_words()
                }
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
                self.views[k].labels.collapsed = if redo { after } else { before }.clone();
                "Labels".into()
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
                    self.remember(k);
                    drop_later(self.views[k].clear());
                }
                Some(p) => {
                    self.open_in(ctx, k, p.clone());
                    self.views[k].restore_range = Some(snap.ranges[k]);
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
        let active = if open.contains(&snap.active) { snap.active } else { open[0] };
        self.activate(active);
    }

    /// Tests: take everything as it is now (setup that is not a person's action).
    #[cfg(test)]
    pub(super) fn quiet(&mut self) {
        self.observe(false);
    }
}
