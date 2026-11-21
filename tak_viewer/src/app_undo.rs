
use super::*;
use crate::views::{Range, Selection};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tak_core::edit::{DocumentId, EditError, EditPatch, EditSelection, LabelKey, Revision};
use tak_core::playback::PlaybackControls;
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
    comments: crate::comment_selection::CommentSelection,
    list_anchors: [panels::ListSelectionState; 2],
    armed_label: Option<LabelKey>,
    linked_pick: panels::LinkedLabelPick,
    range: Range,
    playback_controls: PlaybackControls,
    plate: Option<usize>,
    speed: f64,
    folds: HashSet<FoldKey>,
    channels: HashMap<usize, usize>,
    grid: crate::ui::device_grid::Arrangement,
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
            comments: v.comment_selection.clone(),
            list_anchors: v.labels.list_selection.clone(),
            armed_label: v.armed_label,
            linked_pick: v.labels.linked_pick,
            range: v.range,
            playback_controls: v.pb.controls(),
            plate: v.plate,
            speed: v.pb.speed,
            folds: v.data.as_ref().map(|data| fold_keys(data, &v.labels.collapsed)).unwrap_or_default(),
            channels: v.devices.channel.clone(),
            grid: v.devices.arrangement(),
        })
    }
}

/// The pane layout with the file of each view slot (what an open / close / move puts back).
#[derive(Clone)]
pub(super) struct LayoutSnap {
    /// Stable owner; swapping whole layouts must never apply this to another tab.
    tab: Option<layout_tabs::TabId>,
    dock: DockState<Pane>,
    shape: u64,
    hidden: dock::Hidden,
    /// File shown or being read in each view slot.
    files: [Option<PathBuf>; MAX_VIEWS],
    /// The particular closed view, so a reused same-file slot cannot consume an old item.
    file_ids: [usize; MAX_VIEWS],
    /// Each layout owns its viewing position; later opens must not replace it in file memory.
    spots: [Option<Spot>; MAX_VIEWS],
    picked: [Option<u64>; MAX_VIEWS],
    comments: [crate::comment_selection::CommentSelection; MAX_VIEWS],
    list_anchors: [[panels::ListSelectionState; 2]; MAX_VIEWS],
    linked_picks: [panels::LinkedLabelPick; MAX_VIEWS],
    /// Keep editable state across reversible layout changes without sharing ViewData.
    documents: [Option<crate::views::EditSession>; MAX_VIEWS],
    /// Kept with the document even after heavy close-history runtimes are evicted.
    /// A fallback disk read may reuse that identity only after its worker verifies freshness.
    source_stamps: [Option<crate::views::SourceStamp>; MAX_VIEWS],
    edit_selections: [Option<EditSelection>; MAX_VIEWS],
    /// The range each of those files had (a file read again gets it back).
    ranges: [Range; MAX_VIEWS],
    playback_controls: [PlaybackControls; MAX_VIEWS],
    /// The active view then (put back with the layout; choosing a view is no action itself).
    active: usize,
    /// The file laid over each view slot (plan step 7; only once it is on screen) and whose
    /// markers were shown there ([own file, overlay]; both true without an overlay).
    overlays: [Option<PathBuf>; MAX_VIEWS],
    shown: [[bool; 2]; MAX_VIEWS],
    /// List selection travels with the existing layout action; plain row activation does
    /// not gain a separate undo step.
    editor_selection: crate::ui::open_editors::EditorSelection,
}

#[derive(Clone)]
pub(super) struct EditorBatchArchive {
    pub(super) id: u64,
    open: Vec<Box<TabSnapshot>>,
    closed: Vec<Box<TabSnapshot>>,
    selection: crate::ui::open_editors::EditorSelection,
    open_active: Option<u64>,
    closed_active: Option<u64>,
    pub(super) rows: Vec<crate::ui::open_editors::EditorRowId>,
}

impl EditorBatchArchive {
    pub(super) fn new(id: u64, open: Vec<Box<TabSnapshot>>, closed: Vec<Box<TabSnapshot>>,
        selection: crate::ui::open_editors::EditorSelection, open_active: Option<u64>, closed_active: Option<u64>,
        rows: Vec<crate::ui::open_editors::EditorRowId>) -> Self {
        Self { id, open, closed, selection, open_active, closed_active, rows }
    }
    pub(super) fn rebind(&mut self, old: &Path, new: &Path) {
        for tab in self.open.iter_mut().chain(&mut self.closed) {
            let snap = &mut tab.layout;
            snap.editor_selection.rebind(old, new);
            for slot in 0..MAX_VIEWS {
                if snap.files[slot].as_deref() == Some(old) { snap.files[slot] = Some(new.into()); }
                if snap.overlays[slot].as_deref() == Some(old) { snap.overlays[slot] = Some(new.into()); }
                if let Some(spot) = &mut snap.spots[slot] { if Path::new(&spot.path) == old { spot.path = new.to_string_lossy().into_owned(); } }
            }
        }
        self.selection.rebind(old, new);
        for row in &mut self.rows { if row.path == old { row.path = new.into(); } }
    }
}

/// Independent of the 20-item reopen menu: app undo keeps the small restore state for
/// its full 500-action lifetime. Heavy ViewData stays only in live/recently closed runtimes.
#[derive(Clone)]
pub(super) struct TabSnapshot {
    name: String,
    index: usize,
    layout: LayoutSnap,
    graph_boxes: Vec<GraphBox>,
    plate_boxes: Vec<GraphBox>,
    plate_channels: Vec<LaneSet>,
}

/// During replacement the old file can still be drawn. Undo must retain the incoming
/// file's restore state, rather than assigning the old file's frame or selection to it.
fn pending_view(v: &View) -> bool {
    v.loading.is_some() || v.remote_loading.is_some() || v.waiting_shared_path().is_some()
}

fn layout_spot(v: &View) -> Option<Spot> {
    if pending_view(v) { v.restore.clone() } else { v.spot() }
}

fn layout_overlay(v: &View) -> Option<&Path> {
    v.overlay_load.as_ref().filter(|load| load.quiet).map(|load| load.path.as_path()).or_else(|| v.overlay_path())
}

fn layout_document(v: &View) -> Option<crate::views::EditSession> {
    if pending_view(v) { v.restore_edit_document.clone().or_else(|| v.fresh_open_document()) } else { v.edit_document.clone() }
}

fn layout_source_stamp(v: &View) -> Option<crate::views::SourceStamp> {
    if pending_view(v) { v.fresh_open_source_stamp().or_else(|| v.source_stamp.clone()) } else { v.source_stamp.clone() }
}

fn layout_edit_selection(v: &View) -> Option<EditSelection> {
    if pending_view(v) { v.restore_edit_selection.clone() } else { v.data.as_ref().map(|_| v.edit_selection_state()) }
}

fn layout_range(v: &View, shown: Range) -> Range {
    if pending_view(v) { v.restore_range.unwrap_or(None) } else { shown }
}

fn layout_playback_controls(v: &View) -> PlaybackControls {
    if pending_view(v) { v.restore_playback_controls.unwrap_or_default() } else { v.pb.controls() }
}

/// Display options and floating graph places (saved in the settings).
#[derive(Clone, PartialEq)]
pub(super) struct Options {
    scene: SceneOptions,
    graph_float: bool,
    graph_layout: crate::ui::graph::tracks::Layout,
    opacity: u8,
    graph_boxes: Vec<GraphBox>,
    plate_boxes: Vec<GraphBox>,
    plate_channels: Vec<LaneSet>,
    device_layouts: Vec<crate::device_grid::Layout>,
    channel_names: crate::device_grid::ChannelNames,
    labeling_increment: crate::label_increment::IncrementMode,
    labeling_link: bool,
    marker_order: Vec<String>,
}

impl Options {
    fn of(s: &Settings) -> Options {
        Options {
            scene: s.scene.clone(),
            graph_float: s.graph_float,
            graph_layout: s.graph_layout,
            opacity: s.graph_opacity,
            graph_boxes: s.graph_boxes.clone(),
            plate_boxes: s.plate_boxes.clone(),
            plate_channels: s.plate_channels.clone(),
            device_layouts: s.device_layouts.clone(),
            channel_names: s.channel_names.clone(),
            labeling_increment: s.labeling_increment,
            labeling_link: s.labeling_link,
            marker_order: s.marker_order.clone(),
        }
    }

    fn same(&self, s: &Settings) -> bool {
        self.scene == s.scene
            && self.graph_float == s.graph_float
            && self.graph_layout == s.graph_layout
            && self.opacity == s.graph_opacity
            && self.graph_boxes == s.graph_boxes
            && self.plate_boxes == s.plate_boxes
            && self.plate_channels == s.plate_channels
            && self.device_layouts == s.device_layouts
            && self.channel_names == s.channel_names
            && self.labeling_increment == s.labeling_increment
            && self.labeling_link == s.labeling_link
            && self.marker_order == s.marker_order
    }

    /// Status bar word for going from `self` to `to`.
    fn words(&self, to: &Options) -> String {
        if self.marker_order != to.marker_order {
            "Marker order".into()
        } else if self.channel_names != to.channel_names {
            "Channel names".into()
        } else if self.device_layouts != to.device_layouts {
            "Saved layouts".into()
        } else if self.labeling_increment != to.labeling_increment || self.labeling_link != to.labeling_link {
            "Labeling".into()
        } else if self.scene != to.scene {
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

/// One change inside an action. Viewing actions carry a stable tab + slot in `View`;
/// the nested file path validates the slot. Marker changes still identify one shared document.
#[derive(Clone)]
pub(super) enum Part {
    Progress(crate::take_progress::Pick),
    CommentTypes(crate::comment_types::Change),
    View { tab: Option<layout_tabs::TabId>, slot: usize, part: Box<Part> },
    Frame { file: PathBuf, before: i64, after: i64 },
    Camera { file: PathBuf, before: Camera, after: Camera },
    Selection { file: PathBuf, before: Selection, after: Selection },
    PanelSetting { file: PathBuf, before: PanelSetting, after: PanelSetting },
    Range { file: PathBuf, before: Range, after: Range },
    PlaybackControls { file: PathBuf, before: PlaybackControls, after: PlaybackControls },
    Plate { file: PathBuf, before: Option<usize>, after: Option<usize> },
    GraphLock {
        file: PathBuf,
        before: Option<crate::ui::graph::conveniences::LockedSelection>,
        after: Option<crate::ui::graph::conveniences::LockedSelection>,
    },
    Speed { file: PathBuf, before: f64, after: f64 },
    Folds { file: PathBuf, before: HashSet<FoldKey>, after: HashSet<FoldKey> },
    Channels { file: PathBuf, before: HashMap<usize, usize>, after: HashMap<usize, usize> },
    DeviceGrid { file: PathBuf, before: crate::ui::device_grid::Arrangement, after: crate::ui::device_grid::Arrangement },
    /// A comment added or deleted; `cleared` = the picked range adding it cleared.
    Comment { file: PathBuf, change: Change, cleared: Range },
    /// One entire marker command, including selection / armed label, in global order.
    MarkerEdit { file: PathBuf, patch: Arc<EditPatch> },
    Layout { before: Box<LayoutSnap>, after: Box<LayoutSnap> },
    /// One atomic primary-file conversion across active and hidden layout tabs.
    TakmSwitch { batch: takm_save::SharedBatch },
    Options { tab: Option<layout_tabs::TabId>, before: Box<Options>, after: Box<Options> },
    EditorBatch { archive: Arc<EditorBatchArchive>, close_after: bool },
    EditorSelection { before: crate::ui::open_editors::EditorSelection, after: crate::ui::open_editors::EditorSelection },
    Folders { before: Box<Folders>, after: Box<Folders> },
    /// How the COMMENTS pane is sorted (v0.17.0; not saved in the settings).
    Sort { before: comment_list::Sort, after: comment_list::Sort },
    TabSwitch { before: layout_tabs::TabId, after: layout_tabs::TabId },
    TabRename { id: layout_tabs::TabId, before: String, after: String },
    TabMove { id: layout_tabs::TabId, before: usize, after: usize },
    TabLifecycle {
        id: layout_tabs::TabId,
        before_open: bool,
        after_open: bool,
        before_active: Option<layout_tabs::TabId>,
        after_active: Option<layout_tabs::TabId>,
        snapshot: Box<TabSnapshot>,
    },
}

impl Part {
    fn inner(&self) -> &Part {
        match self { Part::View { part, .. } => part.inner(), _ => self }
    }

    fn inner_mut(&mut self) -> &mut Part {
        match self { Part::View { part, .. } => part.inner_mut(), _ => self }
    }

    fn owner(&self) -> Option<layout_tabs::TabId> {
        match self {
            Part::View { tab, .. } => *tab,
            Part::Layout { before, .. } => before.tab,
            Part::Options { tab, .. } => *tab,
            _ => None,
        }
    }

    fn file(&self) -> Option<&Path> {
        match self.inner() {
            Part::Frame { file, .. }
            | Part::Camera { file, .. }
            | Part::Selection { file, .. }
            | Part::PanelSetting { file, .. }
            | Part::Range { file, .. }
            | Part::PlaybackControls { file, .. }
            | Part::Plate { file, .. }
            | Part::GraphLock { file, .. }
            | Part::Speed { file, .. }
            | Part::Folds { file, .. }
            | Part::DeviceGrid { file, .. } | Part::Channels { file, .. }
            | Part::Comment { file, .. }
            | Part::MarkerEdit { file, .. } => Some(file),
            _ => None,
        }
    }

    /// `next` continues `self` (consecutive frame moves, or camera moves, of one file): the two
    /// as one part. None = not merged.
    fn merged(&self, next: &Part) -> Option<Part> {
        match (self, next) {
            (Part::View { tab, slot, part }, Part::View { tab: t, slot: s, part: n }) if tab == t && slot == s => {
                part.merged(n).map(|merged| Part::View { tab: *tab, slot: *slot, part: Box::new(merged) })
            }
            (Part::TabSwitch { before, after: mid }, Part::TabSwitch { before: mid2, after }) if mid == mid2 => {
                Some(Part::TabSwitch { before: *before, after: *after })
            }
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
        match self.inner() {
            Part::Frame { before, after, .. } => before == after,
            Part::Camera { before, after, .. } => before == after,
            Part::PanelSetting { before, after, .. } => before == after,
            Part::TabSwitch { before, after } => before == after,
            Part::TabRename { before, after, .. } => before == after,
            Part::TabMove { before, after, .. } => before == after,
            Part::TabLifecycle { before_open, after_open, .. } => before_open == after_open,
            Part::Comment { change: Change::Edited { before, after }, cleared: None, .. } => before == after,
            _ => false,
        }
    }
}

/// `a` and `b` are the same comment but for its type.
fn only_type(a: &Comment, b: &Comment) -> bool {
    a.comment_type != b.comment_type && Comment { comment_type: b.comment_type.clone(), type_meaning: b.type_meaning.clone(), ..a.clone() } == *b
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
    pub(super) merge_open: bool,
    /// How many new actions were pushed so far (a merge into the newest one does not count):
    /// tells whether the newest action is the one a COMMENTS row click made (v0.27.3).
    pub(super) pushed: u64,
    base: Option<Box<Base>>,
    /// The app changed the layout on its own (a file could not be read): no action for it.
    pub(super) quiet_layout: bool,
    /// Take everything as it is (a capture ended).
    pub(super) quiet_all: bool,
    /// Disk fallback after mixed-close eviction gets a fresh UI/writer id. Once loaded,
    /// rebind just that old view's snapshots so later close undo consumes the retained view.
    restored_view_ids: HashMap<(Option<layout_tabs::TabId>, usize), (usize, PathBuf)>,
}

impl History {
    /// Conflicted edits no longer describe the reloaded row and cannot safely be replayed.
    pub(super) fn forget_comment(&mut self, path: &Path, id: u64) {
        for stack in [&mut self.undo, &mut self.redo] {
            for action in stack.iter_mut() {
                if action.iter().any(|part| matches!(part.inner(), Part::Comment { file, change: Change::DeletedMany(items), .. } if file == path && items.iter().any(|c| c.id == id))) { action.clear(); continue; }
                action.retain(|part| match part {
                    part if matches!(part.inner(), Part::Comment { file, .. } if file == path) => {
                        let Part::Comment { change, .. } = part.inner() else { unreachable!() };
                        let comment = match change {
                            Change::Added(c) | Change::Deleted(c) => c,
                            Change::DeletedMany(items) => { return !items.iter().any(|c| c.id == id); },
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
        let parts: Vec<_> = parts.into_iter().filter(|part| !part.is_noop()).collect();
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
                if let Part::MarkerEdit { patch, .. } = part.inner() {
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
                    if matches!(part.inner(), Part::MarkerEdit { patch, .. } if patch.document_id() == id && (is_redo || !keep.contains(&patch.after_revision()))) { retired.push(part); }
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
        let name = |p: &Part| match p.inner() {
            Part::Frame { .. } => "Frame",
            Part::Camera { .. } => "Camera",
            Part::Selection { .. } => "Selection",
            Part::PanelSetting { .. } => "PanelSetting",
            Part::Range { .. } => "Range",
            Part::PlaybackControls { .. } => "PlaybackControls",
            Part::Plate { .. } => "Plate",
            Part::GraphLock { .. } => "GraphLock",
            Part::Speed { .. } => "Speed",
            Part::Folds { .. } => "Folds",
            Part::Channels { .. } => "Channels",
            Part::DeviceGrid { .. } => "DeviceGrid",
            Part::Comment { .. } => "Comment",
            Part::MarkerEdit { .. } => "MarkerEdit",
            Part::Progress(_) => "Progress",
            Part::CommentTypes(_) => "Comment types",
            Part::Layout { .. } => "Layout",
            Part::TakmSwitch { .. } => "TakmSwitch",
            Part::EditorSelection { .. } => "EditorSelection",
            Part::EditorBatch { .. } => "EditorBatch",
            Part::Options { .. } => "Options",
            Part::Folders { .. } => "Folders",
            Part::Sort { .. } => "Sort",
            Part::TabSwitch { .. } => "TabSwitch",
            Part::TabRename { .. } => "TabRename",
            Part::TabMove { .. } => "TabMove",
            Part::TabLifecycle { .. } => "TabLifecycle",
            Part::View { .. } => unreachable!("inner removes the view wrapper"),
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
        (Change::DeletedMany(items), redo) => (if redo { EventKind::Delete } else { EventKind::Add }, &items[0]),
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
    /// Explicitly pushed controls use the same per-view identity as observed changes.
    pub(super) fn view_undo_part(&self, slot: usize, part: Part) -> Part {
        Part::View { tab: self.layout_tabs.active_id(), slot, part: Box::new(part) }
    }

    /// Capture after create/reopen, before close. The controller makes this tab active first.
    pub(super) fn tab_undo_snapshot(&self, id: layout_tabs::TabId) -> Option<Box<TabSnapshot>> {
        if self.layout_tabs.active_id() != Some(id) { return None; }
        let tab = self.layout_tabs.tab(id)?;
        Some(Box::new(TabSnapshot {
            name: tab.name.clone(),
            index: self.layout_tabs.tabs().iter().position(|tab| tab.id == id)?,
            layout: self.layout_now(std::array::from_fn(|slot| self.views[slot].range)),
            graph_boxes: self.settings.graph_boxes.clone(),
            plate_boxes: self.settings.plate_boxes.clone(),
            plate_channels: self.settings.plate_channels.clone(),
        }))
    }

    /// A save changes the path associated with the same editable document, not its history.
    pub(super) fn rebind_saved_history(&mut self, old: &Path, new: &Path, document: DocumentId) {
        self.editor_selection.rebind(old, new);
        self.editor_batch.rebind(old, new);
        fn layout(snap: &mut LayoutSnap, old: &Path, new: &Path, id: DocumentId) {
            snap.editor_selection.rebind(old, new);
            for k in 0..MAX_VIEWS {
                if snap.overlays[k].as_deref() == Some(old) { snap.overlays[k] = Some(new.to_path_buf()); }
                let same = snap.documents[k].as_ref().is_some_and(|s| s.lock().ok().is_some_and(|d| d.id() == id));
                if same && snap.files[k].as_deref() == Some(old) {
                    snap.files[k] = Some(new.to_path_buf());
                    if let Some(spot) = &mut snap.spots[k] { spot.path = new.to_string_lossy().into_owned(); }
                }
            }
        }
        for action in self.history.undo.iter_mut().chain(&mut self.history.redo) {
            for part in action {
                match part.inner_mut() {
                    Part::Frame { file, .. } | Part::Camera { file, .. } | Part::Selection { file, .. } | Part::PanelSetting { file, .. }
                    | Part::Range { file, .. } | Part::Plate { file, .. } | Part::GraphLock { file, .. } | Part::Speed { file, .. }
                    | Part::Folds { file, .. } | Part::DeviceGrid { file, .. } | Part::Channels { file, .. } | Part::Comment { file, .. } => {
                        if file == old { *file = new.to_path_buf(); }
                    }
                    Part::MarkerEdit { file, patch } => {
                        if file == old && patch.document_id() == document { *file = new.to_path_buf(); }
                    }
                    Part::Layout { before, after } => { layout(before, old, new, document); layout(after, old, new, document); }
                    Part::TabLifecycle { snapshot, .. } => layout(&mut snapshot.layout, old, new, document),
                    Part::EditorSelection { before, after } => { before.rebind(old, new); after.rebind(old, new); }
                    Part::EditorBatch { archive, .. } => Arc::make_mut(archive).rebind(old, new),
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
            tab: self.layout_tabs.active_id(),
            dock: self.dock.clone(),
            shape: dock::shape(&self.dock),
            hidden: self.hidden.clone(),
            files: std::array::from_fn(|k| self.views[k].target().map(Path::to_path_buf)),
            file_ids: std::array::from_fn(|k| self.views[k].file_id),
            spots: std::array::from_fn(|k| layout_spot(&self.views[k])),
            picked: std::array::from_fn(|k| if pending_view(&self.views[k]) { self.views[k].restore_picked.unwrap_or(None) } else { self.views[k].picked }),
            comments: std::array::from_fn(|k| self.views[k].restore_comment_selection.clone().unwrap_or_else(|| self.views[k].comment_selection.clone())),
            list_anchors: std::array::from_fn(|k| self.views[k].restore_list_anchors.clone().unwrap_or_else(|| self.views[k].labels.list_selection.clone())),
            linked_picks: std::array::from_fn(|k| self.views[k].restore_linked_pick.unwrap_or(self.views[k].labels.linked_pick)),
            documents: std::array::from_fn(|k| layout_document(&self.views[k])),
            source_stamps: std::array::from_fn(|k| layout_source_stamp(&self.views[k])),
            edit_selections: std::array::from_fn(|k| layout_edit_selection(&self.views[k])),
            ranges: std::array::from_fn(|k| layout_range(&self.views[k], ranges[k])),
            playback_controls: std::array::from_fn(|k| layout_playback_controls(&self.views[k])),
            active: self.active,
            overlays: std::array::from_fn(|k| layout_overlay(&self.views[k]).map(Path::to_path_buf)),
            shown: std::array::from_fn(|k| self.views[k].shown),
            editor_selection: self.editor_selection.clone(),
        }
    }

    /// Explicit list gestures share any layout action (plain overlay row click) in this gesture.
    pub(super) fn record_editor_selection(&mut self, before: crate::ui::open_editors::EditorSelection, pushed: u64) {
        if let Some(base) = &mut self.history.base { base.layout.editor_selection = self.editor_selection.clone(); }
        if self.no_persist || before == self.editor_selection { return; }
        // The layout already contains both sides of this list selection.
        if self.history.pushed > pushed && self.history.undo.last().is_some_and(|action| action.iter().any(|part| matches!(part, Part::Layout { .. }))) { return; }
        let part = Part::EditorSelection { before, after: self.editor_selection.clone() };
        if self.history.pushed > pushed {
            if let Some(action) = self.history.undo.last_mut() { action.push(part); }
        } else { self.history.push(vec![part]); }
    }

    /// Plain activation keeps its historical undo behavior while remembering the selected
    /// row for a later close and for any existing layout action.
    pub(super) fn absorb_plain_editor_selection(&mut self) {
        if let Some(base) = &mut self.history.base { base.layout.editor_selection = self.editor_selection.clone(); }
    }

    pub(super) fn editor_layout_snapshot(&self) -> Box<LayoutSnap> {
        Box::new(self.layout_now(std::array::from_fn(|slot| self.views[slot].range)))
    }

    /// Only explicit gestures traverse affected hidden runtimes; idle frames keep the tab cache.
    pub(super) fn editor_batch_snapshot(&mut self, tabs: &BTreeSet<u64>) -> Vec<Box<TabSnapshot>> {
        let original = self.layout_tabs.active_id();
        let mut snapshots = Vec::new();
        for &tab in tabs {
            if self.layout_tabs.tab(tab).is_none() { continue; }
            if self.layout_tabs.active_id() != Some(tab) { self.switch_layout_tab_internal(tab); self.observe(false); }
            if let Some(snapshot) = self.tab_undo_snapshot(tab) { snapshots.push(snapshot); }
        }
        if let Some(tab) = original { if self.layout_tabs.active_id() != Some(tab) { self.switch_layout_tab_internal(tab); self.observe(false); } }
        snapshots.sort_by_key(|tab| tab.index);
        snapshots
    }

    pub(super) fn prune_editor_batch_archives(&mut self) {
        let mut retained = self.layout_tabs.recent_batch_ids();
        for action in self.history.undo.iter().chain(&self.history.redo) {
            for part in action {
                if let Part::EditorBatch { archive, .. } = part.inner() { retained.insert(archive.id); }
            }
        }
        let gone: Vec<_> = self.editor_batch.archives.keys().filter(|id| !retained.contains(id)).copied().collect();
        for id in gone { drop_later(self.editor_batch.archives.remove(&id)); }
    }

    pub(super) fn push_editor_batch_history(&mut self, archive: Arc<EditorBatchArchive>, close_after: bool) {
        self.history.push(vec![Part::EditorBatch { archive, close_after }]);
        self.history.merge_open = false;
        self.prune_editor_batch_archives();
    }

    pub(super) fn editor_batch_replay_id(&self, redo: bool) -> Option<u64> {
        (if redo { self.history.redo.last() } else { self.history.undo.last() }).and_then(|action| match action.as_slice() {
            [Part::EditorBatch { archive, .. }] => Some(archive.id), _ => None,
        })
    }

    pub(super) fn finish_editor_batch_replay(&mut self, redo: bool) {
        let action = if redo { self.history.redo.pop() } else { self.history.undo.pop() };
        if let Some(action) = action {
            let active = action.iter().find_map(|part| match part {
                Part::EditorBatch { archive, close_after } => if redo == *close_after { archive.closed_active } else { archive.open_active },
                _ => None,
            });
            if let Some(tab) = active.filter(|tab| self.layout_tabs.tab(*tab).is_some()) { self.switch_layout_tab_internal(tab); }
            if redo { self.history.undo.push(action); } else { self.history.redo.push(action); }
        }
        self.history.merge_open = false;
        self.observe(false);
    }

    /// Check the exact retained runtimes native restore will consume, without taking any
    /// leaf out of its recent batch. A later refusal must not split the batch or open an
    /// earlier tab. Native marker/save ownership and adoption checks stay authoritative.
    fn editor_batch_restore_ready(&self, desired: &[TabSnapshot], files: &mut HashSet<PathBuf>) -> bool {
        for snapshot in desired {
            let Some(tab) = snapshot.layout.tab else { continue };
            let live = self.layout_tabs.tab(tab);
            let effective_views = if live.is_some() {
                if self.layout_tabs.active_id() == Some(tab) { Some(&self.views) }
                else { live.and_then(|tab| tab.layout.as_ref()).map(|runtime| &runtime.views) }
            } else {
                let retained = self.layout_tabs.closed_items().rev().find_map(|item| match item {
                    layout_tabs::ClosedItem::Tab(closed) if closed.tab.id == tab => Some(closed),
                    _ => None,
                });
                if let Some(runtime) = retained.and_then(|closed| closed.tab.layout.as_ref()) {
                    if runtime.views.iter().enumerate().any(|(slot, view)|
                        runtime.marker_ui[slot].has_pending_work() || !self.retained_view_ready(view)) {
                        return false;
                    }
                    for view in &runtime.views { Self::add_view_file_identities(view, files); }
                    Some(&runtime.views)
                } else { None }
            };
            for slot in 0..MAX_VIEWS {
                let Some(path) = snapshot.layout.files[slot].as_deref() else { continue };
                // restore_layout keeps an occurrence already present at this destination.
                if effective_views.is_some_and(|views| views[slot].target() == Some(path)) { continue; }
                let retained = self.layout_tabs.closed_items().rev().find_map(|item| match item {
                    layout_tabs::ClosedItem::View { tab_id, index, view }
                        if *tab_id == tab && *index == slot && view.view.file_id == snapshot.layout.file_ids[slot]
                            && view.view.target() == Some(path) => Some(view),
                    _ => None,
                });
                if let Some(closed) = retained {
                    if closed.marker_ui.has_pending_work() || !self.retained_view_ready(&closed.view) { return false; }
                    Self::add_view_file_identities(&closed.view, files);
                }
            }
        }
        true
    }

    /// Restore occurrences, never deduplicate by file path. A recent reopen may merge into
    /// free slots; ordinary undo uses the same preflight before any layout is changed.
    pub(super) fn restore_editor_batch(&mut self, ctx: &egui::Context, archive: &EditorBatchArchive, record: bool) -> bool {
        let origin = self.layout_tabs.active_id();
        let tabs: BTreeSet<_> = archive.open.iter().filter_map(|tab| tab.layout.tab).collect();
        let closed_before = self.editor_batch_snapshot(&tabs);
        let mut desired = Vec::new();
        let mut remap = BTreeMap::new();
        let mut paths = Vec::new();
        for original in &archive.open {
            let Some(tab) = original.layout.tab else { continue };
            let prior = archive.closed.iter().find(|state| state.layout.tab == Some(tab));
            let live = closed_before.iter().find(|state| state.layout.tab == Some(tab));
            let exact = match (prior, live) {
                (None, None) => true,
                (Some(a), Some(b)) => a.layout.files == b.layout.files && a.layout.overlays == b.layout.overlays && a.layout.file_ids == b.layout.file_ids && a.layout.shape == b.layout.shape,
                _ => false,
            };
            let mut wanted = if exact || live.is_none() { (**original).clone() } else { (**live.unwrap()).clone() };
            if !exact && live.is_some() {
                let mut used: BTreeSet<_> = dock::views(&wanted.layout.dock).into_iter().map(usize::from).collect();
                for old in 0..MAX_VIEWS {
                    let removed = prior.map_or(true, |after| original.layout.files[old] != after.layout.files[old] || original.layout.file_ids[old] != after.layout.file_ids[old]);
                    if !removed || original.layout.files[old].is_none() { continue; }
                    let Some(slot) = (!used.contains(&old)).then_some(old).or_else(|| (0..MAX_VIEWS).find(|slot| !used.contains(slot))) else {
                        self.notice = Some("화면이 가득 차서 닫은 파일 묶음을 다시 열 수 없어요.".into());
                        if let Some(tab) = origin { self.switch_layout_tab_internal(tab); self.observe(false); }
                        return false;
                    };
                    used.insert(slot);
                    remap.insert((tab, old), slot);
                    wanted.layout.files[slot] = original.layout.files[old].clone();
                    wanted.layout.file_ids[slot] = original.layout.file_ids[old];
                    wanted.layout.spots[slot] = original.layout.spots[old].clone();
                    wanted.layout.picked[slot] = original.layout.picked[old];
                    wanted.layout.comments[slot] = original.layout.comments[old].clone();
                    wanted.layout.list_anchors[slot] = original.layout.list_anchors[old].clone();
                    wanted.layout.linked_picks[slot] = original.layout.linked_picks[old];
                    wanted.layout.documents[slot] = original.layout.documents[old].clone();
                    wanted.layout.source_stamps[slot] = original.layout.source_stamps[old].clone();
                    wanted.layout.edit_selections[slot] = original.layout.edit_selections[old].clone();
                    wanted.layout.ranges[slot] = original.layout.ranges[old];
                    wanted.layout.playback_controls[slot] = original.layout.playback_controls[old];
                    wanted.layout.overlays[slot] = original.layout.overlays[old].clone();
                    wanted.layout.shown[slot] = original.layout.shown[old];
                    dock::add_view(&mut wanted.layout.dock, slot as u8, self.dock_area);
                }
                for old in 0..MAX_VIEWS {
                    if remap.contains_key(&(tab, old)) { continue; }
                    if prior.is_some_and(|after| original.layout.overlays[old] == after.layout.overlays[old]) { continue; }
                    let Some(overlay) = &original.layout.overlays[old] else { continue };
                    let Some(slot) = wanted.layout.files.iter().enumerate().position(|(slot, path)| path == &original.layout.files[old] && wanted.layout.file_ids[slot] == original.layout.file_ids[old]) else {
                        self.notice = Some("얹은 파일의 기본 화면을 먼저 다시 열어 주세요.".into()); return false;
                    };
                    if wanted.layout.overlays[slot].as_ref().is_some_and(|path| path != overlay) {
                        self.notice = Some("다른 파일이 이미 얹혀 있어 닫은 파일 묶음을 다시 열 수 없어요.".into()); return false;
                    }
                    wanted.layout.overlays[slot] = Some(overlay.clone());
                    wanted.layout.shown[slot] = original.layout.shown[old];
                    remap.insert((tab, old), slot);
                }
            } else { for slot in 0..MAX_VIEWS { remap.insert((tab, slot), slot); } }
            paths.extend(wanted.layout.files.iter().chain(&wanted.layout.overlays).flatten().cloned());
            if !self.layout_tabs.has_closed_tab(tab) && !self.layout_snapshot_available(&wanted.layout) {
                self.notice = Some("닫은 파일을 찾을 수 없어 다시 열지 못했어요.".into()); return false;
            }
            desired.push(wanted);
        }
        let mut files = self.open_file_identities(None);
        if !self.editor_batch_restore_ready(&desired, &mut files) {
            self.notice = Some("닫은 파일 묶음의 읽기·편집·저장이 아직 끝나지 않았어요. 작업을 마친 뒤 다시 열어 주세요.".into());
            return false;
        }
        files.extend(paths.iter().map(|path| self.known_identity(path)));
        if files.len() > MAX_OPEN_FILES { self.notice_open_file_limit(); return false; }
        self.observe(true);
        let saved_undo = std::mem::take(&mut self.history.undo);
        let saved_redo = std::mem::take(&mut self.history.redo);
        let saved_pushed = self.history.pushed;
        let applied = (|| {
        let mut restored_positions = Vec::new();
        for wanted in &desired {
            let tab = wanted.layout.tab.expect("batch tab");
            if self.layout_tabs.tab(tab).is_none() {
                if self.layout_tabs.has_closed_tab(tab) {
                    if !self.restore_closed_tab_internal(tab) { return false; }
                } else if !self.create_layout_tab_for_undo(tab, &wanted.name, wanted.index, &paths) { return false; }
                restored_positions.push((tab, wanted.index));
            }
            if self.layout_tabs.active_id() != Some(tab) { self.switch_layout_tab_internal(tab); self.observe(false); }
            self.settings.graph_boxes = wanted.graph_boxes.clone();
            self.settings.plate_boxes = wanted.plate_boxes.clone();
            self.settings.plate_channels = wanted.plate_channels.clone();
            // Side panels are app-wide. A recent reopen restores only this tab's editor
            // layout, retaining shared pane changes made after the batch was closed.
            let mut target = wanted.layout.clone();
            let editor = dock::editor_layout(&target.dock);
            target.dock = self.dock.clone();
            dock::set_editor_layout(&mut target.dock, &editor);
            target.hidden = self.hidden.clone();
            self.restoring_layout = true;
            self.restore_layout(ctx, &target);
            self.restoring_layout = false;
            self.observe(false);
        }
        // Native close indices shift during the gesture. Wait until every missing tab
        // exists before restoring saved positions, so move_tab never clamps an index to an
        // incomplete strip. Already-live tabs retain their relative order.
        restored_positions.sort_by_key(|&(_, index)| index);
        for (tab, index) in restored_positions { self.layout_tabs.move_tab(tab, index); }
        true
        })();
        drop_later(Some(std::mem::replace(&mut self.history.undo, saved_undo)));
        drop_later(Some(std::mem::replace(&mut self.history.redo, saved_redo)));
        self.history.pushed = saved_pushed;
        self.history.merge_open = false;
        if !applied { self.observe(false); return false; }
        let mut selection = archive.selection.clone();
        selection.remap_occurrences(&remap);
        self.editor_selection = selection.clone();
        drop_later(self.layout_tabs.take_closed_where(|item| matches!(item, layout_tabs::ClosedItem::Batch { id, .. } if *id == archive.id)));
        if let Some(tab) = archive.open_active.filter(|tab| self.layout_tabs.tab(*tab).is_some()).or(origin) {
            self.switch_layout_tab_internal(tab);
        }
        self.observe(false);
        if record && !self.no_persist {
            let open = self.editor_batch_snapshot(&tabs);
            let mut rows = archive.rows.clone();
            for row in &mut rows { if let Some(slot) = remap.get(&(row.tab_id, row.slot)) { row.slot = *slot; } }
            let reopened = Arc::new(EditorBatchArchive::new(archive.id, open, closed_before, selection, self.layout_tabs.active_id(), origin, rows));
            self.push_editor_batch_history(reopened, false);
        }
        true
    }

    /// Closing a playing view keeps the exact last displayed frame too. Snapshot only small
    /// viewing state; files and buffers still retire off the drawing thread.
    pub(super) fn remember_layout_spot(&mut self, slot: usize) {
        let v = &self.views[slot];
        if let Some(base) = self.history.base.as_mut().filter(|b| b.layout.files[slot].as_deref() == v.target()) {
            base.layout.spots[slot] = layout_spot(v);
            base.layout.picked[slot] = if pending_view(v) { v.restore_picked.unwrap_or(None) } else { v.picked };
            base.layout.comments[slot] = v.restore_comment_selection.clone().unwrap_or_else(|| v.comment_selection.clone());
            base.layout.list_anchors[slot] = v.restore_list_anchors.clone().unwrap_or_else(|| v.labels.list_selection.clone());
            base.layout.linked_picks[slot] = v.restore_linked_pick.unwrap_or(v.labels.linked_pick);
            base.layout.documents[slot] = layout_document(v);
            base.layout.source_stamps[slot] = layout_source_stamp(v);
            base.layout.edit_selections[slot] = layout_edit_selection(v);
            base.layout.ranges[slot] = layout_range(v, v.range);
            base.layout.playback_controls[slot] = layout_playback_controls(v);
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
        if self.overlay_owner_scope { return; }
        self.rebind_restored_view_ids();
        let record = record && !self.no_persist;
        let Some(mut base) = self.history.base.take() else {
            self.history.base = Some(Box::new(self.base_now()));
            return;
        };
        // Tab switching has its own explicit history part. Comparing the newly active
        // runtime to the previous tab would invent opens, closes and viewing changes.
        if base.layout.tab != self.layout_tabs.active_id() {
            *base = self.base_now();
            self.history.base = Some(base);
            self.history.quiet_layout = false;
            self.history.quiet_all = false;
            return;
        }
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
                base.layout.source_stamps[k] = v.source_stamp.clone();
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
                    before.playback_controls[k] = b.playback_controls;
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
            if !drafting && (v.selected != b.selected || order != b.order || v.primary != b.primary || v.picked != b.picked || v.comment_selection != b.comments || v.labels.list_selection != b.list_anchors || v.armed_label != b.armed_label || v.labels.linked_pick != b.linked_pick) {
                let before = Selection { selected: std::mem::replace(&mut b.selected, v.selected.clone()), order: std::mem::replace(&mut b.order, order), primary: b.primary, comment: b.picked, comments: b.comments.clone(), list_anchors: b.list_anchors.clone(), armed_label: b.armed_label, linked_pick: b.linked_pick };
                b.primary = v.primary;
                b.picked = v.picked;
                b.comments = v.comment_selection.clone();
                b.list_anchors = v.labels.list_selection.clone();
                b.armed_label = v.armed_label;
                b.linked_pick = v.labels.linked_pick;
                parts.push(Part::Selection { file: b.file.clone(), before, after: v.selection() });
                if base.layout.files[k].as_deref() == v.path() { base.layout.edit_selections[k] = Some(v.edit_selection_state()); }
            }
            if v.range != b.range {
                parts.push(Part::Range { file: b.file.clone(), before: b.range, after: v.range });
                b.range = v.range;
            }
            if v.pb.controls() != b.playback_controls {
                let after = v.pb.controls();
                let before = std::mem::replace(&mut b.playback_controls, after);
                parts.push(Part::PlaybackControls { file: b.file.clone(), before, after });
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
            if v.devices.layout != b.grid.layout || v.devices.folded != b.grid.folded {
                let grid = v.devices.arrangement();
                let initialized = b.grid.layout.is_some();
                let before = std::mem::replace(&mut b.grid, grid.clone());
                if initialized { parts.push(Part::DeviceGrid { file: b.file.clone(), before, after: grid }); }
            }
            if v.devices.channel != b.channels {
                let before = std::mem::replace(&mut b.channels, v.devices.channel.clone());
                parts.push(Part::Channels { file: b.file.clone(), before, after: v.devices.channel.clone() });
            }
            if record {
                file_parts.extend(parts.into_iter().map(|part| Part::View {
                    tab: base.layout.tab, slot: k, part: Box::new(part),
                }));
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
                let local = before.graph_boxes != after.graph_boxes || before.plate_boxes != after.plate_boxes || before.plate_channels != after.plate_channels;
                app_parts.push(Part::Options { tab: if local { base.layout.tab } else { None }, before: Box::new(before), after: Box::new(after) });
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

    fn rebind_restored_view_ids(&mut self) {
        let active = self.layout_tabs.active_id();
        self.history.restored_view_ids.retain(|&(tab, slot), (_, path)| {
            tab != active || self.views.get(slot).is_some_and(|view| view.target() == Some(path.as_path()))
        });
        let finished: Vec<_> = self.history.restored_view_ids.iter().filter_map(|(&(tab, slot), (old, path))| {
            if tab != active { return None; }
            let view = self.views.get(slot)?;
            (!pending_view(view) && view.path() == Some(path.as_path()) && view.data.is_some())
                .then_some(((tab, slot), *old, view.file_id, path.clone()))
        }).collect();
        for ((tab, slot), old, new, path) in finished {
            self.history.restored_view_ids.remove(&(tab, slot));
            for action in self.history.undo.iter_mut().chain(&mut self.history.redo) {
                for part in action {
                    let rebind = |snap: &mut LayoutSnap| {
                        if snap.tab == tab && snap.file_ids[slot] == old && snap.files[slot].as_ref() == Some(&path) {
                            snap.file_ids[slot] = new;
                        }
                    };
                    match part.inner_mut() {
                        Part::Layout { before, after } => { rebind(before); rebind(after); }
                        Part::TabLifecycle { snapshot, .. } => rebind(&mut snapshot.layout),
                        _ => {}
                    }
                }
            }
        }
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
            let before = Selection { selected: b.selected.clone(), order: b.order.clone(), primary: b.primary, comment: b.picked, comments: b.comments.clone(), list_anchors: b.list_anchors.clone(), armed_label: b.armed_label, linked_pick: b.linked_pick };
            if before != now {
                (b.selected, b.order, b.primary, b.picked, b.armed_label) = (now.selected.clone(), now.order.clone(), now.primary, now.comment, now.armed_label);
                b.comments = now.comments.clone();
                b.list_anchors = now.list_anchors.clone();
                b.linked_pick = now.linked_pick;
                parts.push(Part::Selection { file, before, after: now });
            }
        }
        if !self.no_persist {
            let parts = parts.into_iter().map(|part| self.view_undo_part(slot, part)).collect();
            self.history.push(parts);
        }
    }

    /// Publish a prepared marker edit as exactly one global history action. Stale/no-op jobs
    /// never call `History::push`; selection is carried inside the same patch.
    pub(super) fn commit_marker_edit(&mut self, slot: usize, patch: Arc<EditPatch>) -> Result<(), EditError> {
        let explicit_remote_probe=self.remote_write_probe && self.views.get(slot).is_some_and(|view|view.remote.is_some());
        if self.no_persist && !explicit_remote_probe && !self.label_shot_in_progress(slot) {
            return Err(EditError::Other("명령 실행에서는 편집할 수 없어요".into()));
        }
        self.observe(true);
        let file = self.views.get(slot).and_then(View::path).ok_or(EditError::NotLoaded)?.to_path_buf();
        self.views[slot].apply_marker_patch(&patch, true)?;
        self.history.push(vec![self.view_undo_part(slot, Part::MarkerEdit { file, patch })]);
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
        view.notify_editor_updates();
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
        let ours = h.undo.last().filter(|action| action.len() == 1).is_some_and(|action| {
            let part = &action[0];
            let same_view = matches!(part, Part::View { tab, slot: s, .. } if *tab == self.layout_tabs.active_id() && *s == slot);
            same_view && matches!(part.inner(), Part::Selection { file: f, after, .. } if f == file && after.comment == Some(id))
        });
        let Some(b) = h.base.as_mut().and_then(|b| b.views[slot].as_mut()).filter(|b| ours && b.file_id == v.file_id) else { return };
        if let Some(Some(part)) = h.undo.pop().map(|mut a| a.pop()) {
            if let Part::Selection { before, .. } = part.inner() {
                (b.selected, b.order, b.primary, b.picked, b.armed_label) = (before.selected.clone(), before.order.clone(), before.primary, before.comment, before.armed_label);
                b.comments = before.comments.clone();
                b.list_anchors = before.list_anchors.clone();
                b.linked_pick = before.linked_pick;
            }
        }
    }

    /// A legacy unscoped part is safe only when exactly one active view has this path.
    fn slot_of(&self, file: &Path) -> Option<usize> {
        let mut slots = (0..MAX_VIEWS).filter(|&k| self.views[k].target() == Some(file));
        let slot = slots.next()?;
        slots.next().is_none().then_some(slot)
    }

    fn part_slot(&self, part: &Part) -> Option<usize> {
        if let Part::View { tab, slot, part } = part {
            if *tab == self.layout_tabs.active_id() && self.views.get(*slot).is_some_and(|view| {
                if let Part::MarkerEdit { patch, .. } = part.inner() {
                    return view.edit_document.iter().chain(view.restore_edit_document.iter()).any(|s| {
                        s.lock().ok().is_some_and(|d| d.id() == patch.document_id())
                    });
                }
                part.file().is_some_and(|file| view.target() == Some(file))
            }) {
                return Some(*slot);
            }
            // Document changes are shared, even after their original view was closed.
            // Viewing changes must never fall through to another occurrence of the file.
            if !matches!(part.inner(), Part::MarkerEdit { .. }) { return None; }
        }
        if let Part::MarkerEdit { patch, .. } = part.inner() {
            return self.views.iter().position(|v| {
                v.edit_document.iter().chain(v.restore_edit_document.iter()).any(|s| s.lock().ok().is_some_and(|d| d.id() == patch.document_id()))
            });
        }
        part.file().and_then(|file| self.slot_of(file))
    }

    /// Prefer the command's original view; a shared document may remain in another tab.
    fn history_owner(&self, parts: &[Part]) -> Option<layout_tabs::TabId> {
        for part in parts {
            if let Part::MarkerEdit { patch, .. } = part.inner() {
                let owns = |view: &View| view.overlay_document_id() == Some(patch.document_id())
                    || view.edit_document.iter().chain(view.restore_edit_document.iter()).any(|session| {
                        session.lock().ok().is_some_and(|doc| doc.id() == patch.document_id())
                    });
                if let Part::View { tab: Some(tab), slot, .. } = part {
                    let view = if Some(*tab) == self.layout_tabs.active_id() { self.views.get(*slot) }
                        else { self.layout_tabs.tab(*tab).and_then(|tab| tab.layout.as_ref()).and_then(|layout| layout.views.get(*slot)) };
                    if view.is_some_and(owns) { return Some(*tab); }
                }
                if self.views.iter().any(owns) { return self.layout_tabs.active_id(); }
                if let Some(tab) = self.layout_tabs.tabs().iter().find(|tab| tab.layout.as_ref().is_some_and(|layout| layout.views.iter().any(owns))) {
                    return Some(tab.id);
                }
            } else if let Some(tab) = part.owner() { return Some(tab); }
        }
        None
    }

    fn layout_snapshot_available(&self, to: &LayoutSnap) -> bool {
        let live = |path: &Path| self.views.iter().any(|view| view.target() == Some(path))
            || self.layout_tabs.tabs().iter().filter_map(|tab| tab.layout.as_ref()).any(|runtime| runtime.views.iter().any(|view| view.target() == Some(path)));
        let retained_view = |slot: usize| self.layout_tabs.closed_items().find_map(|item| match item {
            layout_tabs::ClosedItem::View { tab_id, index, view }
                if Some(*tab_id) == to.tab && *index == slot && view.view.file_id == to.file_ids[slot]
                    && view.view.target() == to.files[slot].as_deref() => Some(&view.view),
            _ => None,
        });
        for (slot, file) in to.files.iter().enumerate() {
            let Some(file) = file else { continue };
            if retained_view(slot).is_some() || live(file) { continue; }
            if remote_ui::is_remote_key(file) {
                let cached = self.ssh_cache.iter().any(|cached| cached.key == *file);
                let connected = self.ssh_sources.get(file).is_some_and(|location| self.ssh.is_connected(&location.host));
                if !cached && !connected { return false; }
            } else if !file.is_file() { return false; }
        }
        for (slot, file) in to.overlays.iter().enumerate() {
            let Some(file) = file else { continue };
            // The exact closed runtime restores its overlay without another disk read;
            // an already-live primary can also supply this overlay's shared document.
            let retained = retained_view(slot).is_some_and(|view| view.overlay_path() == Some(file.as_path()))
                || live(file)
                || self.views.iter().any(|view| view.overlay_paths().any(|path| path == file))
                || self.layout_tabs.tabs().iter().filter_map(|tab| tab.layout.as_ref()).any(|runtime| runtime.views.iter().any(|view| view.overlay_paths().any(|path| path == file)));
            if !retained && !file.is_file() { return false; }
        }
        true
    }

    fn ready(&self, parts: &[Part], redo: bool) -> Ready {
        for scoped in parts {
            let p = scoped.inner();
            if scoped.owner().is_some_and(|tab| self.layout_tabs.active_id() != Some(tab))
                && !matches!(p, Part::MarkerEdit { .. }) { return Ready::Gone; }
            match p {
                Part::TakmSwitch { batch } => {
                    if !self.takm_batch_ready(batch) { return Ready::Gone; }
                    continue;
                }
                Part::TabSwitch { before, after } => {
                    if self.layout_tabs.tab(pick(redo, *before, *after)).is_none() { return Ready::Gone; }
                    continue;
                }
                Part::TabRename { id, before, after } => {
                    if self.layout_tabs.validate_name(*id, if redo { after } else { before }).is_err() { return Ready::Gone; }
                    continue;
                }
                Part::TabMove { id, .. } => {
                    if self.layout_tabs.tab(*id).is_none() { return Ready::Gone; }
                    continue;
                }
                Part::TabLifecycle { id, before_open, after_open, snapshot, .. } => {
                    let open = pick(redo, *before_open, *after_open);
                    if open && !self.layout_tabs.has_closed_tab(*id) && !self.layout_snapshot_available(&snapshot.layout)
                        || !open && self.layout_tabs.tab(*id).is_none() { return Ready::Gone; }
                    continue;
                }
                _ => {}
            }
            if let Part::Layout { before, after } = p {
                let to = if redo { after } else { before };
                if to.tab != self.layout_tabs.active_id() { return Ready::Gone; }
                if !self.layout_snapshot_available(to) { return Ready::Gone; }
                continue;
            }
            let Some(file) = p.file() else { continue };
            let Some(k) = self.part_slot(scoped) else { return Ready::Gone };
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
                if let Change::DeletedMany(items) = change {
                    if items.iter().any(|c| v.comments.list.get(c.id).is_some() != redo) { return Ready::Gone; }
                    continue;
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
    pub(super) fn finish_progress_history(&mut self, pick: crate::take_progress::Pick, replay: Option<bool>, now: f64) {
        match replay {
            None => self.history.push(vec![Part::Progress(pick)]),
            Some(redo) => {
                let (from, to) = if redo { (&mut self.history.redo, &mut self.history.undo) }
                    else { (&mut self.history.undo, &mut self.history.redo) };
                if let Some(index) = from.iter().rposition(|action| matches!(action.as_slice(),
                    [Part::Progress(old)] if old.target == pick.target
                        && (if redo { (old.before, old.after) } else { (old.after, old.before) }) == (pick.before, pick.after))) {
                    let action = from.remove(index);
                    if redo || index == from.len() { to.push(action); }
                }
                let word = if redo { "Redo" } else { "Undo" };
                self.history.note = Some((format!("{word} · {}", pick.after.label()), now));
                self.history.merge_open = false;
            }
        }
    }

    pub(super) fn finish_comment_types_history(
        &mut self, change: crate::comment_types::Change, replay: Option<(crate::comment_types::Change, bool)>, now: f64,
    ) {
        if let Some((original, redo)) = replay {
            let (from, to) = if redo { (&mut self.history.redo, &mut self.history.undo) }
                else { (&mut self.history.undo, &mut self.history.redo) };
            if let Some(index) = from.iter().rposition(|action|
                matches!(action.as_slice(), [Part::CommentTypes(old)] if *old == original))
            {
                let action = from.remove(index);
                if redo || index == from.len() { to.push(action); }
            }
            let word = if redo { "Redo" } else { "Undo" };
            self.history.note = Some((format!("{word} · Comment types"), now));
        } else if change.before != change.after {
            self.history.push(vec![Part::CommentTypes(change)]);
            self.history.note = Some(("Saved Comment types".into(), now));
        }
    }

    pub(super) fn undo_redo(&mut self, ctx: &egui::Context, redo: bool) {
        if self.comment_types_saving() {
            self.history.note = Some(("Comment 종류 저장이 끝난 뒤 다시 해 주세요.".into(), ctx.input(|i| i.time)));
            return;
        }
        if self.progress_job.is_some() {
            self.history.note = Some(("진행 상태 저장이 끝난 뒤 다시 해 주세요.".into(), ctx.input(|i| i.time)));
            return;
        }
        if self.open_views().into_iter().any(|slot| self.marker_saving(slot))
            || self.overlay_marker_ui.values().any(marker_edit::MarkerUi::has_save) {
            self.history.note = Some(("저장 중에는 되돌릴 수 없어요.".into(), ctx.input(|i| i.time)));
            return;
        }
        // Playback stops first: the play run so far is an action (then undone, or it drops the
        // redo side like any new action).
        self.stop_history_playback();
        self.observe(true);
        self.history.merge_open = false;
        let progress = (if redo { self.history.redo.last() } else { self.history.undo.last() })
            .and_then(|action| match action.as_slice() { [Part::Progress(pick)] => Some(pick.clone()), _ => None });
        if let Some(mut pick) = progress {
            if !redo { std::mem::swap(&mut pick.before, &mut pick.after); }
            pick.stamp = None;
            self.start_progress(ctx, pick, Some(redo));
            return;
        }
        let batch = (if redo { self.history.redo.last() } else { self.history.undo.last() }).and_then(|action| match action.as_slice() {
            [Part::EditorBatch { archive, close_after }] => Some((archive.clone(), *close_after)), _ => None,
        });
        if let Some((archive, close_after)) = batch {
            if redo == close_after {
                self.begin_editor_batch(ctx, archive.rows.iter().cloned().collect(), Some(redo));
            } else if self.restore_editor_batch(ctx, &archive, false) { self.finish_editor_batch_replay(redo); }
            return;
        }
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
            if let [Part::CommentTypes(change)] = top.as_slice() {
                self.replay_comment_types(change.clone(), redo);
                return;
            }
            let operation_origin = self.layout_tabs.active_id();
            let owner = self.history_owner(top);
            if let Some(tab) = owner {
                if self.layout_tabs.active_id() != Some(tab) && self.switch_layout_tab_internal(tab) {
                    self.observe(false);
                }
            }
            // The internal switch moves runtimes only. It does not record another action.
            let top = if redo { self.history.redo.last() } else { self.history.undo.last() }.expect("history unchanged by internal switch");
            if let Some(batch) = top.iter().find_map(|part| match part.inner() {
                Part::TakmSwitch { batch } => Some(batch.clone()), _ => None,
            }) {
                if self.guard_takm_batch(&batch, redo) { return; }
            }
            let top = if redo { self.history.redo.last() } else { self.history.undo.last() }.expect("conversion guard keeps history");
            // A worker may finish after its final primary closes. Its latest shared
            // marker action still belongs to the surviving overlay document.
            if !self.overlay_owner_scope {
                let overlay_slot = top.iter().find_map(|part| {
                    let Part::MarkerEdit { patch, .. } = part.inner() else { return None };
                    if self.views.iter().any(|view| view.document_id() == Some(patch.document_id())) { return None; }
                    self.views.iter().position(|view| view.overlay_document_id() == Some(patch.document_id()))
                });
                if let Some(slot) = overlay_slot {
                    self.with_overlay_owner(ctx, slot, |app| { app.undo_redo(ctx, redo); app.poll_marker_jobs(ctx); });
                    return;
                }
            }
            let closing_tab = top.iter().find_map(|part| match part.inner() {
                Part::TabLifecycle { id, before_open, after_open, .. } if !pick(redo, *before_open, *after_open) => Some(*id),
                _ => None,
            });
            let removed_layout = top.iter().find_map(|part| {
                let Part::Layout { before, after } = part.inner() else { return None };
                let target = if redo { after } else { before };
                let mut removed = Vec::new();
                for slot in self.open_views() {
                    let view = &self.views[slot];
                    if view.target() != target.files[slot].as_deref() { removed.push(overlay_dirty::Reference::Primary(slot)); }
                    if view.overlay_path() != target.overlays[slot].as_deref() { removed.push(overlay_dirty::Reference::Overlay(slot)); }
                }
                let kept = target.files.iter().chain(target.overlays.iter()).flatten().cloned().collect::<Vec<_>>();
                Some((removed, kept))
            });
            if let Some((removed, kept)) = removed_layout {
                if self.guard_reference_removal(&removed, &kept, marker_edit::PendingTransition::Undo { redo }) { return; }
            }
            let top = if redo { self.history.redo.last() } else { self.history.undo.last() }.expect("guard keeps history");
            match self.ready(top, redo) {
                Ready::Wait(name) => {
                    self.history.note = Some((format!("{name} 파일을 다 읽은 뒤 다시 해 주세요."), now));
                    return;
                }
                Ready::Gone => {
                    let retired = if redo {
                        self.history.redo.pop()
                    } else {
                        self.history.undo.pop()
                    };
                    drop_later(retired);
                    skipped += 1;
                }
                Ready::Go => {
                    if let Some(id) = closing_tab {
                        if self.tab_close_origin.is_none() { self.tab_close_origin = Some((id, operation_origin)); }
                        if self.guard_layout_tab_undo(id, redo) { return; }
                    }
                    let parts = if redo { self.history.redo.pop() } else { self.history.undo.pop() }.unwrap_or_default();
                    if let [Part::TakmSwitch { batch }] = parts.as_slice() {
                        if !self.swap_takm_batch(batch, now) {
                            if redo { self.history.redo.push(parts); } else { self.history.undo.push(parts); }
                            self.observe(false);
                            self.restore_tab_close_origin();
                            self.history.note = Some((format!("{word} · 지금은 파일을 바꿀 수 없어요."), now));
                            return;
                        }
                        let words = self.takm_batch_words(batch);
                        self.restore_tab_close_origin();
                        self.stop_history_playback();
                        if redo { self.history.undo.push(parts); } else { self.history.redo.push(parts); }
                        self.observe(false);
                        self.history.note = Some((format!("{word} · {words}"), now));
                        return;
                    }
                    // Lifecycle actions stand alone. Keep the source stack intact if the
                    // controller refuses a restore (e.g. the distinct-file cap was reached).
                    if let [Part::TabLifecycle { id, before_open, after_open, before_active, after_active, snapshot }] = parts.as_slice() {
                        let open = pick(redo, *before_open, *after_open);
                        let done = if open {
                            if self.layout_tabs.has_closed_tab(*id) { self.restore_closed_tab_internal(*id) }
                            else {
                                let paths: Vec<_> = snapshot.layout.files.iter().chain(&snapshot.layout.overlays).flatten().cloned().collect();
                                if self.create_layout_tab_for_undo(*id, &snapshot.name, snapshot.index, &paths) {
                                    self.settings.graph_boxes = snapshot.graph_boxes.clone();
                                    self.settings.plate_boxes = snapshot.plate_boxes.clone();
                                    self.settings.plate_channels = snapshot.plate_channels.clone();
                                    self.restoring_layout = true;
                                    self.restore_layout(ctx, &snapshot.layout);
                                    self.restoring_layout = false;
                                    true
                                } else { false }
                            }
                        } else { self.close_layout_tab_internal(*id) };
                        if !done {
                            if redo { self.history.redo.push(parts); } else { self.history.undo.push(parts); }
                            self.observe(false);
                            self.history.note = Some((format!("{word} · 지금은 탭을 복원할 수 없어요."), now));
                            return;
                        }
                        if let Some(active) = pick(redo, *before_active, *after_active) {
                            self.switch_layout_tab_internal(active);
                        }
                        self.tab_close_origin = None;
                        self.stop_history_playback();
                        if redo { self.history.undo.push(parts); } else { self.history.redo.push(parts); }
                        self.observe(false);
                        self.history.note = Some((format!("{word} · Tab {}", if open { "opened" } else { "closed" }), now));
                        return;
                    }
                    let mut words: Vec<String> = parts.iter().map(|p| self.apply_part(ctx, p, redo, now)).collect();
                    self.stop_history_playback();
                    // A comment added or deleted carries the change of the selected comment it
                    // made: the comment's own words say enough.
                    if parts.iter().any(|p| matches!(p.inner(), Part::Comment { .. })) {
                        for (p, w) in parts.iter().zip(words.iter_mut()) {
                            if matches!(p.inner(), Part::Selection { before, after, .. } if (&before.selected, &before.order, before.primary) == (&after.selected, &after.order, after.primary)) {
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

    fn stop_history_playback(&mut self) {
        for view in &mut self.views { view.pb.playing = false; }
        for runtime in self.layout_tabs.tabs_mut().iter_mut().filter_map(|tab| tab.layout.as_mut()) {
            for view in &mut runtime.views { view.pb.playing = false; }
        }
    }

    /// Put one part's "before" (undo) or "after" (redo) side back; returns its status bar words.
    fn apply_part(&mut self, ctx: &egui::Context, part: &Part, redo: bool, now: f64) -> String {
        let slot = self.part_slot(part);
        let original_view = match part { Part::View { tab, slot, .. } => Some((*tab, *slot)), _ => None };
        let part = part.inner();
        match part {
            Part::TabSwitch { before, after } => {
                let id = pick(redo, *before, *after);
                self.switch_layout_tab_internal(id);
                return self.layout_tabs.tab(id).map(|tab| format!("Tab {}", tab.name)).unwrap_or_default();
            }
            Part::TabRename { id, before, after } => {
                let name = if redo { after } else { before };
                let _ = self.layout_tabs.rename(*id, name);
                return format!("Tab {name}");
            }
            Part::TabMove { id, before, after } => {
                self.layout_tabs.move_tab(*id, pick(redo, *before, *after));
                return "Tab order".into();
            }
            Part::Layout { before, after } => {
                let to = if redo { after } else { before };
                let words = self.layout_words(to);
                self.restoring_layout = true;
                self.restore_layout(ctx, to);
                self.restoring_layout = false;
                return words;
            }
            Part::EditorSelection { before, after } => {
                self.editor_selection = if redo { after.clone() } else { before.clone() };
                return format!("{} open files selected", self.editor_selection.selected.len());
            }
            Part::Options { before, after, .. } => {
                let (from, to) = if redo { (before, after) } else { (after, before) };
                let s = &mut self.settings;
                if from.channel_names != to.channel_names { s.channel_names = to.channel_names.clone(); }
                if from.marker_order != to.marker_order { s.marker_order = to.marker_order.clone(); }
                if from.labeling_increment != to.labeling_increment { s.labeling_increment = to.labeling_increment; }
                if from.labeling_link != to.labeling_link { s.labeling_link = to.labeling_link; }
                if from.device_layouts != to.device_layouts { s.device_layouts = to.device_layouts.clone(); }
                if from.scene != to.scene { s.scene = to.scene.clone(); }
                if from.graph_float != to.graph_float { s.graph_float = to.graph_float; }
                if from.graph_layout != to.graph_layout { s.graph_layout = to.graph_layout; }
                if from.opacity != to.opacity { s.graph_opacity = to.opacity; }
                if from.graph_boxes != to.graph_boxes { s.graph_boxes = to.graph_boxes.clone(); }
                if from.plate_boxes != to.plate_boxes { s.plate_boxes = to.plate_boxes.clone(); }
                if from.plate_channels != to.plate_channels { s.plate_channels = to.plate_channels.clone(); }
                return from.words(to);
            }
            Part::Sort { before, after } => {
                self.set_comment_sort(pick(redo, *before, *after));
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
                if before.linked_pick != after.linked_pick {
                    v.selection_words()
                } else if (&before.selected, &before.order, before.primary) == (&after.selected, &after.order, after.primary) {
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
            Part::PlaybackControls { before, after, .. } => {
                let pb = &mut self.views[k].pb;
                let prior = pb.working_range();
                pb.restore_controls(pick(redo, *before, *after), now);
                let (a, b) = pb.working_range();
                if prior != (a, b) { format!("Working Range {a}–{b}") }
                else if pb.reverse() { "Reverse playback".into() }
                else { "Forward playback".into() }
            }
            Part::Plate { before, after, .. } => {
                let v = &mut self.views[k];
                v.plate = pick(redo, *before, *after);
                match v.plate.and_then(|p| v.data.as_ref()?.plates.get(p)) {
                    Some(p) => format!("Plate {}", p.name),
                    None => "Plate none".into(),
                }
            }
            Part::GraphLock { before, after, .. } => {
                let lock = pick(redo, before, after).clone();
                let words = if lock.is_some() { "Selection locked" } else { "Selection unlocked" };
                self.views[k].graph_view.lock = lock;
                words.into()
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
            Part::DeviceGrid { before, after, .. } => {
                self.views[k].devices.restore(if redo { after } else { before });
                "Device grid".into()
            }
            Part::Channels { before, after, .. } => {
                self.views[k].devices.channel = if redo { after } else { before }.clone();
                "Device channel".into()
            }
            Part::Comment { change, cleared, .. } => {
                if let Change::DeletedMany(items) = change {
                    let kind = if redo { EventKind::Delete } else { EventKind::Add };
                    self.write_comment_batch(ctx, k, kind, items);
                    return format!("{} Comments {}", items.len(), if redo { "removed" } else { "restored" });
                }
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
                let preserve = original_view.is_some_and(|owner| owner != (self.layout_tabs.active_id(), k));
                let selection = preserve.then(|| (self.views[k].selection(), self.views[k].edit_selection_state()));
                let result = self.views[k].apply_marker_patch(patch, redo);
                if result.is_ok() {
                    // The document outlived its original view. Replaying its shared edit
                    // must not replace the surviving view's independent selection.
                    if let Some((visible, uid)) = selection {
                        self.views[k].set_selection(visible);
                        self.views[k].edit_selection = Some(uid);
                    }
                }
                marker_edit::trace_timing(if redo { "redo commit" } else { "undo commit" }, started);
                match result {
                    Ok(()) => if patch.result.message.is_empty() { "Marker edit restored".into() } else { format!("{} · restored", patch.result.message) },
                    Err(_) => "Marker edit failed".into(),
                }
            }
            Part::Progress(_) | Part::CommentTypes(_) | Part::EditorBatch { .. } | Part::EditorSelection { .. } | Part::Layout { .. } | Part::TakmSwitch { .. } | Part::Options { .. } | Part::Folders { .. } | Part::Sort { .. }
            | Part::TabSwitch { .. } | Part::TabRename { .. } | Part::TabMove { .. }
            | Part::TabLifecycle { .. } | Part::View { .. } => String::new(),
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
    /// its remembered spot and the range it had) and the active view. A snapshot belongs to
    /// one stable tab and keeps that tab's at-most-four-view split, including repeated files.
    fn restore_layout(&mut self, ctx: &egui::Context, snap: &LayoutSnap) {
        // Keep the shared document and its readers while its final current reference
        // migrates to a different slot in the destination layout.
        let kept: Vec<_> = snap.files.iter().chain(snap.overlays.iter()).flatten().cloned().collect();
        for slot in 0..MAX_VIEWS {
            let keep = |path: Option<PathBuf>| path.is_some_and(|path| kept.iter().any(|kept| Self::same_local_path(&path, kept)));
            if self.views[slot].target() != snap.files[slot].as_deref()
                && (keep(self.views[slot].shared_target()) || keep(self.views[slot].overlay_shared_target())) {
                self.remember(slot);
                self.hold_layout_reference(slot, true, snap.files[slot].is_none() && !dock::views(&snap.dock).contains(&(slot as u8)));
            } else if self.views[slot].overlay_path() != snap.overlays[slot].as_deref()
                && keep(self.views[slot].overlay_shared_target()) { self.hold_layout_reference(slot, false, false); }
        }
        // Save the pre-close split in mixed reopen history before installing the target
        // dock. Redoing a view close is then the same move as closing it by hand.
        if let Some(tab) = snap.tab {
            let wanted = dock::views(&snap.dock);
            for slot in 0..MAX_VIEWS {
                if !wanted.contains(&(slot as u8)) && snap.files[slot].is_none() {
                    self.remember_closed_view_for_history(tab, slot);
                }
            }
        }
        self.editor_selection = snap.editor_selection.clone();
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
                    if let Some(tab) = snap.tab {
                        if self.restore_closed_view_for_history(tab, k, p, snap.file_ids[k]) { continue; }
                    }
                    self.open_in(ctx, k, p.clone());
                    if self.views[k].target() == Some(p.as_path()) {
                        self.history.restored_view_ids.insert((snap.tab, k), (snap.file_ids[k], p.clone()));
                    }
                    if self.views[k].loading.is_some() {
                        let candidate = snap.documents[k].clone().map(|document| crate::views::RetainedDocument::new(document, snap.source_stamps[k].clone()));
                        self.views[k].set_fresh_open_candidate(candidate);
                    }
                    self.views[k].restore_edit_selection = snap.edit_selections[k].clone();
                    if !pending_view(&self.views[k]) && self.views[k].path() == Some(p.as_path()) {
                        // Same-file sharing can complete synchronously, just like a cached
                        // remote open. There will be no future show() to consume restores.
                        let view = &mut self.views[k];
                        if let Some(spot) = &snap.spots[k] {
                            view.fit_pending = false;
                            view.apply_spot(spot, ctx.input(|i| i.time));
                        }
                        view.range = snap.ranges[k];
                        view.pb.restore_controls(snap.playback_controls[k], ctx.input(|i| i.time));
                        view.picked = snap.picked[k];
                        view.comment_selection = snap.comments[k].clone();
                        view.labels.list_selection = snap.list_anchors[k].clone();
                        view.labels.linked_pick = snap.linked_picks[k];
                        if let (Some(selection), Some(data)) = (&snap.edit_selections[k], &view.data) {
                            let find = |uid: &[u8; 16]| data.markers.iter().position(|marker| marker.uid == *uid);
                            let visible = Selection {
                                selected: selection.selected.iter().filter_map(find).collect(),
                                order: selection.order.iter().filter_map(find).collect(),
                                primary: selection.primary.as_ref().and_then(find),
                                comment: snap.picked[k],
                                comments: snap.comments[k].clone(),
                                list_anchors: snap.list_anchors[k].clone(),
                                armed_label: selection.armed_label,
                                linked_pick: snap.linked_picks[k],
                            };
                            view.set_selection(visible);
                            view.edit_selection = Some(selection.clone());
                        }
                        drop_later(view.restore_edit_document.take());
                        view.restore_edit_selection = None;
                        view.restore = None;
                        view.restore_range = None;
                        view.restore_playback_controls = None;
                        view.restore_picked = None;
                        view.restore_comment_selection = None;
                        view.restore_list_anchors = None;
                        view.restore_linked_pick = None;
                    } else {
                        self.views[k].restore = snap.spots[k].clone().or_else(|| self.views[k].restore.take());
                        self.views[k].restore_range = Some(snap.ranges[k]);
                        self.views[k].restore_playback_controls = Some(snap.playback_controls[k]);
                        self.views[k].restore_picked = Some(snap.picked[k]);
                        self.views[k].restore_comment_selection = Some(snap.comments[k].clone());
                        self.views[k].restore_list_anchors = Some(snap.list_anchors[k].clone());
                        self.views[k].restore_linked_pick = Some(snap.linked_picks[k]);
                    }
                }
            }
        }
        // The file laid over each view (plan step 7; read again, arriving quietly, checked once
        // the view's own file is in) and whose markers were shown.
        for k in 0..MAX_VIEWS {
            let want = snap.overlays[k].as_ref().filter(|_| open.contains(&k));
            if let Some(path) = want {
                if self.install_layout_restore_overlay(k, path) {
                    self.views[k].shown = snap.shown[k];
                    continue;
                }
            }
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
        self.finish_layout_reference_handoffs();
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

    fn shared_view(app: &mut TakViewerApp, from: usize, to: usize, id: usize) {
        let shared = app.views[from].shared_file().unwrap();
        app.views[to].show_shared_file(shared, id, 1.0, 0.0);
        app.views[to].fresh = false;
        app.views[to].fit_pending = false;
    }

    fn another_tab(app: &mut TakViewerApp) -> layout_tabs::TabId {
        let before = app.layout_tabs.active_id().unwrap();
        let id = app.layout_tabs.add(Some(LayoutRuntime::default()));
        app.layout_tabs.activate(before);
        assert!(app.switch_layout_tab_internal(id));
        id
    }

    #[test]
    fn command_marker_commit_requires_explicit_remote_write_probe(){
        for remote in [false,true] {for enabled in [false,true] {
            let ctx=egui::Context::default();let mut app=app(&ctx);
            let location=crate::ssh::RemoteLocation{host:"test-host".into(),path:"/a.tak".into()};
            let key=location.memory_key();
            show(&mut app,0,if remote {key.to_str().unwrap()} else {"local.tak"},1);
            if remote {app.views[0].remote=Some(location);}
            app.set_command_run();app.remote_write_probe=enabled;
            let patch=prepare(&app,0,9.0);let written=patch.after_revision();
            let result=app.commit_marker_edit(0,patch);
            assert_eq!(result.is_ok(),remote&&enabled,"remote={remote}, explicit={enabled}");
            assert_eq!(app.views[0].marker_dirty(),remote&&enabled);
            if remote&&enabled {
                assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().revision(),written);
                assert_eq!(app.history.undo.len(),1);
            } else {assert!(app.history.undo.is_empty());}
        }}
    }

    #[test]
    fn redo_view_close_with_shared_overlay_returns_the_exact_view_to_reopen_history() {
        for live_primary in [true, false] {
            let ctx = egui::Context::default();
            let mut app = app(&ctx);
            dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
            show(&mut app, 0, "close_A.tak", 1);
            show(&mut app, 1, "close_B.tak", 2);
            app.start_overlay(&ctx, 0, "close_B.tak".into(), false);
            app.poll_overlays(&ctx);
            let document = app.views[1].document_id();
            if !live_primary { show(&mut app, 1, "close_C.tak", 3); }
            let file_id = app.views[0].file_id;
            let snapshot = app.layout_now(std::array::from_fn(|slot| app.views[slot].range));
            app.observe(false);
            app.close_view(0);
            app.observe(true);
            assert_eq!(app.layout_tabs.closed_len(), 1);
            assert!(app.layout_snapshot_available(&snapshot));
            let mut missing_overlay = snapshot.clone();
            missing_overlay.overlays[0] = Some("close_missing_overlay.tak".into());
            assert!(!app.layout_snapshot_available(&missing_overlay), "a different missing overlay must still require a reader");
            app.undo_redo(&ctx, false);
            assert_eq!(app.views[0].file_id, file_id);
            assert_eq!(app.views[0].overlay_document_id(), document);
            assert_eq!(app.layout_tabs.closed_len(), 0);
            app.undo_redo(&ctx, true);
            assert_eq!(app.layout_tabs.closed_len(), 1, "redo must return the migrated source to the reopen menu");
            assert!(app.layout_restore_files.is_empty());
            app.reopen_layout_item();
            assert_eq!(app.views[0].file_id, file_id);
            assert_eq!(app.views[0].path(), Some(Path::new("close_A.tak")));
            assert_eq!(app.views[0].overlay_document_id(), document);
            assert!(app.views[0].loading.is_none());
            assert_eq!(app.layout_tabs.closed_len(), 0);
        }
    }

    #[test]
    fn evicted_view_close_undo_reuses_live_primaries_for_main_file_and_overlay() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let tab = app.layout_tabs.active_id().unwrap();
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        dock::add_view(&mut app.dock, 2, dock::NOMINAL_AREA);
        show(&mut app, 0, "evicted_overlay_A.tak", 1);
        show(&mut app, 1, "evicted_overlay_B.tak", 2);
        shared_view(&mut app, 0, 2, 3);
        app.next_file_id = 3;
        app.start_overlay(&ctx, 0, "evicted_overlay_B.tak".into(), false);
        app.poll_overlays(&ctx);
        let document = app.views[0].document_id();
        let overlay_document = app.views[0].overlay_document_id();
        assert!(overlay_document.is_some());
        app.observe(false);
        app.close_view(0);
        app.observe(true);
        drop(app.layout_tabs.take_closed_where(|item| matches!(item,
            layout_tabs::ClosedItem::View { tab_id, index: 0, .. } if *tab_id == tab)));
        assert_eq!(app.layout_tabs.closed_len(), 0);
        app.undo_redo(&ctx, false);
        app.poll_overlays(&ctx);
        assert_eq!(app.open_views().len(), 3);
        assert_eq!(app.views[0].document_id(), document);
        assert_eq!(app.views[0].overlay_document_id(), overlay_document);
        assert!(app.views[0].loading.is_none());
        assert!(app.views[0].overlay_load.is_none());
        assert_eq!(app.history.redo.len(), 1);
    }

    #[test]
    fn same_file_frame_moves_keep_their_view_and_do_not_merge_across_views() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        show(&mut app, 0, "same.tak", 1);
        shared_view(&mut app, 0, 1, 2);
        app.observe(false);
        app.views[0].pb.set_frame(2, 0.0);
        app.observe(true);
        app.views[1].pb.set_frame(7, 0.0);
        app.observe(true);
        assert_eq!(app.history.kinds(), ["Frame", "Frame"]);
        app.activate(0);
        app.undo_redo(&ctx, false);
        assert_eq!((app.views[0].pb.frame, app.views[1].pb.frame), (2, 0));
        app.undo_redo(&ctx, false);
        assert_eq!((app.views[0].pb.frame, app.views[1].pb.frame), (0, 0));
        app.undo_redo(&ctx, true);
        app.undo_redo(&ctx, true);
        assert_eq!((app.views[0].pb.frame, app.views[1].pb.frame), (2, 7));
    }

    #[test]
    fn same_file_camera_selection_and_range_restore_only_the_original_view() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        show_names(&mut app, 0, "same.tak", 1, &["A", "B"]);
        shared_view(&mut app, 0, 1, 2);
        app.views[0].select(0, false);
        app.views[0].range = Some((1, 2));
        let original = app.views[1].cam;
        app.observe(false);
        app.views[1].cam.yaw += 0.5;
        app.views[1].select(1, false);
        app.views[1].range = Some((4, 7));
        app.observe(true);
        assert_eq!(app.history.kinds(), ["Camera+Selection+Range"]);
        app.activate(0);
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[0].primary, Some(0));
        assert_eq!(app.views[0].range, Some((1, 2)));
        assert_eq!(app.views[0].cam, original);
        assert_eq!(app.views[1].primary, None);
        assert_eq!(app.views[1].range, None);
        assert_eq!(app.views[1].cam, original);
        app.undo_redo(&ctx, true);
        assert_eq!(app.views[1].primary, Some(1));
        assert_eq!(app.views[1].range, Some((4, 7)));
        assert_ne!(app.views[1].cam, original);
    }

    #[test]
    fn same_file_history_in_a_hidden_tab_switches_to_its_owner_without_an_extra_action() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        show(&mut app, 0, "same.tak", 1);
        let shared = app.views[0].shared_file().unwrap();
        app.observe(false);
        app.views[0].pb.set_frame(6, 0.0);
        app.observe(true);
        let second = another_tab(&mut app);
        dock::add_view(&mut app.dock, 0, dock::NOMINAL_AREA);
        app.views[0].show_shared_file(shared, 2, 1.0, 0.0);
        app.views[0].fresh = false;
        app.views[0].fit_pending = false;
        app.views[0].pb.set_frame(3, 0.0);
        app.observe(false);
        app.views[0].pb.playing = true;
        app.layout_tabs.tab_mut(first).unwrap().layout.as_mut().unwrap().views[0].pb.playing = true;
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.active_id(), Some(first));
        assert_eq!(app.views[0].pb.frame, 0);
        assert_eq!(app.layout_tabs.tab(second).unwrap().layout.as_ref().unwrap().views[0].pb.frame, 3);
        assert!(!app.views[0].pb.playing);
        assert!(!app.layout_tabs.tab(second).unwrap().layout.as_ref().unwrap().views[0].pb.playing);
        assert_eq!(app.history.undo.len(), 0);
        assert_eq!(app.history.redo.len(), 1);
        app.undo_redo(&ctx, true);
        assert_eq!(app.views[0].pb.frame, 6);
        assert_eq!(app.history.kinds(), ["Frame"]);
    }

    #[test]
    fn consecutive_tab_switches_merge_and_returning_to_the_start_removes_the_action() {
        let mut history = History::default();
        history.push(vec![Part::TabSwitch { before: 1, after: 2 }]);
        history.push(vec![Part::TabSwitch { before: 2, after: 3 }]);
        assert!(matches!(history.undo.as_slice(), [action] if matches!(action.as_slice(), [Part::TabSwitch { before: 1, after: 3 }])));
        history.push(vec![Part::TabSwitch { before: 3, after: 1 }]);
        assert!(history.undo.is_empty());
    }

    #[test]
    fn tab_create_close_and_reopen_replay_stable_ids_and_active_tab() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        app.observe(false);
        let second = another_tab(&mut app);
        app.observe(false);
        app.history.push(vec![Part::TabLifecycle {
            id: second, before_open: false, after_open: true,
            before_active: Some(first), after_active: Some(second),
            snapshot: app.tab_undo_snapshot(second).unwrap(),
        }]);
        app.undo_redo(&ctx, false);
        assert!(app.layout_tabs.tab(second).is_none());
        assert_eq!(app.layout_tabs.active_id(), Some(first));
        assert_eq!(app.history.redo.len(), 1);
        let mut closed = app.layout_tabs.take_closed_tab(second).unwrap();
        closed.tab.layout.as_mut().unwrap().views[0].pb.playing = true;
        drop_later(app.layout_tabs.remember_closed(layout_tabs::ClosedItem::Tab(closed)));
        app.undo_redo(&ctx, true);
        assert!(app.layout_tabs.tab(second).is_some());
        assert_eq!(app.layout_tabs.active_id(), Some(second));
        assert!(!app.views[0].pb.playing);
        assert_eq!(app.history.kinds(), ["TabLifecycle"]);
    }

    #[test]
    fn app_tab_undo_survives_more_than_twenty_closes_independently_of_the_reopen_menu() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let mut ids = vec![app.layout_tabs.active_id().unwrap()];
        for _ in 0..24 { ids.push(another_tab(&mut app)); }
        app.observe(false);
        for &id in &ids {
            app.switch_layout_tab_internal(id);
            app.observe(false);
            let snapshot = app.tab_undo_snapshot(id).unwrap();
            assert!(app.close_layout_tab_internal(id));
            app.observe(false);
            app.history.push(vec![Part::TabLifecycle {
                id, before_open: true, after_open: false,
                before_active: Some(id), after_active: app.layout_tabs.active_id(), snapshot,
            }]);
        }
        assert_eq!(app.layout_tabs.closed_len(), layout_tabs::MAX_CLOSED);
        assert!(app.layout_tabs.tabs().is_empty());
        for &id in ids.iter().rev() {
            app.undo_redo(&ctx, false);
            assert_eq!(app.layout_tabs.active_id(), Some(id));
            assert!(app.layout_tabs.tab(id).is_some());
        }
        assert_eq!(app.layout_tabs.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(), ids);
        assert!(app.history.undo.is_empty());
        assert_eq!(app.history.redo.len(), ids.len());
        for _ in &ids { app.undo_redo(&ctx, true); }
        assert!(app.layout_tabs.tabs().is_empty());
        for _ in &ids { app.undo_redo(&ctx, false); }
        assert_eq!(app.layout_tabs.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(), ids);
        assert!(app.history.undo.is_empty());
    }

    #[test]
    fn evicted_tab_undo_rereads_paths_and_restores_document_and_per_view_state() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        let temp_guard = tak_core::test_support::TempDir::new("tak_undo_evicted_tab");
        let path = temp_guard.join("fixture.tak");
        std::fs::write(&path, b"fixture").unwrap();
        let text = path.to_string_lossy().into_owned();
        show(&mut app, 0, &text, 1);
        app.views[0].pb.set_frame(7, 0.0);
        app.views[0].cam.yaw += 0.5;
        app.views[0].select(0, false);
        app.views[0].range = Some((2, 6));
        let camera = app.views[0].cam;
        let document = app.views[0].edit_document.clone().unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let stamp = Arc::new(std::sync::Mutex::new(Some((path.clone(), meta.len(), Some(meta.modified().unwrap())))));
        app.views[0].source_stamp = Some(stamp.clone());
        let anchor = another_tab(&mut app);
        app.switch_layout_tab_internal(first);
        app.observe(false);
        let snapshot = app.tab_undo_snapshot(first).unwrap();
        assert!(app.close_layout_tab_internal(first));
        app.observe(false);
        app.history.push(vec![Part::TabLifecycle {
            id: first, before_open: true, after_open: false,
            before_active: Some(first), after_active: Some(anchor), snapshot,
        }]);
        for _ in 0..=layout_tabs::MAX_CLOSED {
            let id = another_tab(&mut app);
            app.observe(false);
            let snapshot = app.tab_undo_snapshot(id).unwrap();
            assert!(app.close_layout_tab_internal(id));
            app.observe(false);
            app.history.push(vec![Part::TabLifecycle {
                id, before_open: true, after_open: false,
                before_active: Some(id), after_active: app.layout_tabs.active_id(), snapshot,
            }]);
        }
        assert!(!app.layout_tabs.has_closed_tab(first));
        for _ in 0..=layout_tabs::MAX_CLOSED { app.undo_redo(&ctx, false); }
        app.hold_loads = true;
        app.next_file_id = 50;
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.active_id(), Some(first));
        app.views[0].source_stamp = Some(stamp);
        let tx = app.views[0].held.take().unwrap();
        tx.send(crate::views::LoadMsg::Markers(Ok((crate::views::sample_data(&text, &["M"]), vec![])))).unwrap();
        let loaded = app.views[0].poll_markers().unwrap();
        app.finish_load(&ctx, 0, loaded);
        app.views[0].fit_pending = false;
        app.views[0].fresh = false;
        app.observe(false);
        assert!(Arc::ptr_eq(app.views[0].edit_document.as_ref().unwrap(), &document));
        assert_eq!(app.views[0].pb.frame, 7);
        assert_eq!(app.views[0].cam, camera);
        assert_eq!(app.views[0].range, Some((2, 6)));
        assert_eq!(app.views[0].primary, Some(0));
        assert!(app.history.undo.is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn evicted_tab_undo_at_the_file_cap_keeps_the_action_and_live_documents() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        let temp_guard = tak_core::test_support::TempDir::new("tak_undo_capped_tab");
        let path = temp_guard.join("fixture.tak");
        std::fs::write(&path, b"fixture").unwrap();
        show(&mut app, 0, &path.to_string_lossy(), 1);
        let anchor = another_tab(&mut app);
        app.switch_layout_tab_internal(first);
        app.observe(false);
        let snapshot = app.tab_undo_snapshot(first).unwrap();
        assert!(app.close_layout_tab_internal(first));
        drop_later(app.layout_tabs.take_closed_tab(first));
        app.observe(false);
        app.history.push(vec![Part::TabLifecycle {
            id: first, before_open: true, after_open: false,
            before_active: Some(first), after_active: Some(anchor), snapshot,
        }]);
        for start in (0..MAX_OPEN_FILES).step_by(MAX_VIEWS) {
            if start > 0 { another_tab(&mut app); }
            for slot in 0..(MAX_OPEN_FILES - start).min(MAX_VIEWS) {
                if !app.open_views().contains(&slot) { dock::add_view(&mut app.dock, slot as u8, dock::NOMINAL_AREA); }
                let index = start + slot;
                show(&mut app, slot, &format!("live_cap_fixture_{index}.tak"), 100 + index);
            }
            app.observe(false);
        }
        let active = app.layout_tabs.active_id();
        let tab_count = app.layout_tabs.tabs().len();
        assert_eq!(app.open_file_identities(None).len(), MAX_OPEN_FILES);
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.tabs().len(), tab_count);
        assert_eq!(app.layout_tabs.active_id(), active);
        assert!(app.layout_tabs.tab(first).is_none());
        assert_eq!(app.history.undo.len(), 1);
        assert!(app.history.redo.is_empty());
        assert_eq!(app.open_views(), [0, 1]);
        assert!(app.open_views().iter().all(|&slot| app.views[slot].data.is_some()));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn evicted_tab_undo_reuses_a_hidden_shared_source_and_applies_view_restore_immediately() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        show_names(&mut app, 0, "shared_tab_restore_fixture.tak", 1, &["A", "B"]);
        app.views[0].pb.set_frame(7, 0.0);
        app.views[0].cam.yaw += 0.5;
        app.views[0].select(1, false);
        app.views[0].armed_label = Some(LabelKey { parent: [8; 16], slot: 2 });
        app.views[0].range = Some((2, 6));
        let camera = app.views[0].cam;
        let shared = app.views[0].shared_file().unwrap();
        let document = app.views[0].edit_document.clone().unwrap();
        let second = another_tab(&mut app);
        dock::add_view(&mut app.dock, 0, dock::NOMINAL_AREA);
        app.views[0].show_shared_file(shared, 2, 1.0, 0.0);
        app.views[0].fresh = false;
        app.views[0].fit_pending = false;
        app.views[0].pb.set_frame(3, 0.0);
        app.views[0].select(0, false);
        app.views[0].range = Some((0, 1));
        let data = app.views[0].data.clone().unwrap();
        app.switch_layout_tab_internal(first);
        app.observe(false);
        let snapshot = app.tab_undo_snapshot(first).unwrap();
        assert!(app.close_layout_tab_internal(first));
        drop_later(app.layout_tabs.take_closed_tab(first));
        app.observe(false);
        app.history.push(vec![Part::TabLifecycle {
            id: first, before_open: true, after_open: false,
            before_active: Some(first), after_active: Some(second), snapshot,
        }]);
        app.hold_loads = true;
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.active_id(), Some(first));
        assert_eq!(app.views[0].pb.frame, 7);
        assert_eq!(app.views[0].cam, camera);
        assert_eq!(app.views[0].range, Some((2, 6)));
        assert_eq!(app.views[0].primary, Some(1));
        assert_eq!(app.views[0].armed_label, Some(LabelKey { parent: [8; 16], slot: 2 }));
        assert!(Arc::ptr_eq(app.views[0].data.as_ref().unwrap(), &data));
        assert!(Arc::ptr_eq(app.views[0].edit_document.as_ref().unwrap(), &document));
        assert!(app.views[0].held.is_none());
        assert!(app.views[0].restore.is_none());
        assert!(!app.views[0].fit_pending);
        let other = &app.layout_tabs.tab(second).unwrap().layout.as_ref().unwrap().views[0];
        assert_eq!(other.pb.frame, 3);
        assert_eq!(other.range, Some((0, 1)));
        assert_eq!(other.primary, Some(0));
    }

    #[test]
    fn undoing_tab_creation_cannot_drop_its_only_dirty_document_or_pop_the_action() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        let second = another_tab(&mut app);
        dock::add_view(&mut app.dock, 0, dock::NOMINAL_AREA);
        show(&mut app, 0, "dirty.tak", 1);
        app.observe(false);
        app.commit_marker_edit(0, prepare(&app, 0, 8.0)).unwrap();
        app.history.push(vec![Part::TabLifecycle {
            id: second, before_open: false, after_open: true,
            before_active: Some(first), after_active: Some(second),
            snapshot: app.tab_undo_snapshot(second).unwrap(),
        }]);
        let count = app.history.undo.len();
        app.undo_redo(&ctx, false);
        assert!(app.dirty_prompt.is_some());
        assert_eq!(app.history.undo.len(), count);
        assert!(app.history.redo.is_empty());
        assert!(app.layout_tabs.tab(second).is_some());
        assert!(app.views[0].marker_dirty());
    }

    #[test]
    fn tab_lifecycle_can_close_a_dirty_duplicate_when_another_tab_keeps_the_document() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        show(&mut app, 0, "shared.tak", 1);
        app.observe(false);
        app.commit_marker_edit(0, prepare(&app, 0, 8.0)).unwrap();
        let shared = app.views[0].shared_file().unwrap();
        let second = another_tab(&mut app);
        dock::add_view(&mut app.dock, 0, dock::NOMINAL_AREA);
        app.views[0].show_shared_file(shared, 2, 1.0, 0.0);
        app.views[0].fresh = false;
        app.views[0].fit_pending = false;
        app.observe(false);
        app.history.push(vec![Part::TabLifecycle {
            id: second, before_open: false, after_open: true,
            before_active: Some(first), after_active: Some(second),
            snapshot: app.tab_undo_snapshot(second).unwrap(),
        }]);
        app.undo_redo(&ctx, false);
        assert!(app.dirty_prompt.is_none());
        assert!(app.layout_tabs.tab(second).is_none());
        assert_eq!(app.layout_tabs.active_id(), Some(first));
        assert!(app.views[0].marker_dirty());
        assert_eq!(app.history.kinds(), ["MarkerEdit"]);
    }

    #[test]
    fn shared_marker_undo_falls_back_by_document_without_changing_the_surviving_views_selection() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        show_names(&mut app, 0, "shared.tak", 1, &["A", "B"]);
        app.observe(false);
        let patch = prepare(&app, 0, 8.0);
        app.commit_marker_edit(0, patch.clone()).unwrap();
        let shared = app.views[0].shared_file().unwrap();
        let second = another_tab(&mut app);
        dock::add_view(&mut app.dock, 0, dock::NOMINAL_AREA);
        app.views[0].show_shared_file(shared, 2, 1.0, 0.0);
        app.views[0].fresh = false;
        app.views[0].fit_pending = false;
        app.views[0].select(1, false);
        app.views[0].range = Some((4, 6));
        let selection = app.views[0].selection();
        assert!(app.close_layout_tab_internal(first));
        app.observe(false);
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.active_id(), Some(second));
        assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().revision(), patch.before_revision());
        assert_eq!(app.views[0].selection(), selection);
        assert_eq!(app.views[0].range, Some((4, 6)));
        app.undo_redo(&ctx, true);
        assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().revision(), patch.after_revision());
        assert_eq!(app.views[0].selection(), selection);
    }

    #[test]
    fn tab_rename_and_move_use_ids_and_replay_without_extra_actions() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        let second = another_tab(&mut app);
        let third = another_tab(&mut app);
        app.observe(false);
        app.layout_tabs.rename(first, "walk").unwrap();
        app.history.push(vec![Part::TabRename { id: first, before: "tab1".into(), after: "walk".into() }]);
        assert!(app.layout_tabs.move_tab(third, 0));
        app.history.push(vec![Part::TabMove { id: third, before: 2, after: 0 }]);
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(), [first, second, third]);
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.tab(first).unwrap().name, "tab1");
        app.undo_redo(&ctx, true);
        app.undo_redo(&ctx, true);
        assert_eq!(app.layout_tabs.tab(first).unwrap().name, "walk");
        assert_eq!(app.layout_tabs.tabs()[0].id, third);
        assert_eq!(app.history.kinds(), ["TabRename", "TabMove"]);
        assert_eq!(app.layout_tabs.active_id(), Some(third));
    }

    #[test]
    fn view_actions_do_not_redirect_to_a_different_file_in_the_same_slot() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        show(&mut app, 0, "first.tak", 1);
        app.observe(false);
        app.views[0].pb.set_frame(7, 0.0);
        app.observe(true);
        show(&mut app, 0, "replacement.tak", 2);
        app.views[0].pb.set_frame(3, 0.0);
        app.observe(false);
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[0].pb.frame, 3);
        assert!(app.history.undo.is_empty());
        assert!(app.history.redo.is_empty());
        assert!(app.history.note.as_ref().unwrap().0.contains("skipped"));
    }

    #[test]
    fn global_options_do_not_restore_another_tabs_graph_positions() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        app.observe(false);
        let opacity = app.settings.graph_opacity;
        app.settings.graph_opacity = 20;
        app.observe(true);
        let second = another_tab(&mut app);
        app.settings.plate_channels[0] = LaneSet::only(2);
        app.observe(false);
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.active_id(), Some(second));
        assert_eq!(app.settings.graph_opacity, opacity);
        assert_eq!(app.settings.plate_channels[0], LaneSet::only(2));
    }

    #[test]
    fn local_graph_options_restore_their_owning_tab_only() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        let original = app.settings.plate_channels[0];
        app.observe(false);
        app.settings.plate_channels[0] = LaneSet::only(2);
        app.observe(true);
        let second = another_tab(&mut app);
        app.settings.plate_channels[0] = LaneSet::only(3);
        app.observe(false);
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.active_id(), Some(first));
        assert_eq!(app.settings.plate_channels[0], original);
        assert_eq!(app.layout_tabs.tab(second).unwrap().layout.as_ref().unwrap().plate_channels[0], LaneSet::only(3));
    }

    #[test]
    fn layout_close_undo_consumes_the_exact_retained_view_and_redo_returns_it_once() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let tab = app.layout_tabs.active_id().unwrap();
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        show(&mut app, 0, "left_fixture.tak", 1);
        show(&mut app, 1, "closed_fixture.tak", 2);
        app.views[1].pb.set_frame(7, 0.0);
        app.views[1].cam.yaw += 0.5;
        app.views[1].select(0, false);
        app.views[1].range = Some((2, 5));
        app.views[1].pb.playing = true;
        let camera = app.views[1].cam;
        let document = app.views[1].edit_document.clone().unwrap();
        app.observe(false);
        app.remember_layout_spot(1);
        assert!(app.remember_closed_view_for_history(tab, 1));
        dock::remove_view(&mut app.dock, 1);
        app.observe(true);
        assert_eq!(app.layout_tabs.closed_len(), 1);
        app.undo_redo(&ctx, false);
        assert_eq!(app.open_views(), [0, 1]);
        assert_eq!(app.layout_tabs.closed_len(), 0);
        assert_eq!(app.views[1].file_id, 2);
        assert_eq!(app.views[1].pb.frame, 7);
        assert!(!app.views[1].pb.playing);
        assert_eq!(app.views[1].range, Some((2, 5)));
        assert_eq!(app.views[1].primary, Some(0));
        assert_eq!(app.views[1].cam, camera);
        assert!(Arc::ptr_eq(app.views[1].edit_document.as_ref().unwrap(), &document));
        assert!(app.views[1].loading.is_none());
        app.undo_redo(&ctx, true);
        assert_eq!(app.open_views(), [0]);
        assert_eq!(app.layout_tabs.closed_len(), 1);
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.closed_len(), 0);
        assert_eq!(app.views[1].file_id, 2);
        assert_eq!(app.views[1].pb.frame, 7);
        assert_eq!(app.history.redo.len(), 1);
    }

    #[test]
    fn close_history_eviction_rebinds_reloaded_view_ids_for_later_undo_redo_cycles() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let temp_guard = tak_core::test_support::TempDir::new("tak_undo_evicted_view");
        let path = temp_guard.join("fixture.tak");
        std::fs::write(&path, b"fixture").unwrap();
        let text = path.to_string_lossy().into_owned();
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        show(&mut app, 0, "kept_fixture.tak", 1);
        show(&mut app, 1, &text, 2);
        app.views[1].pb.set_frame(7, 0.0);
        app.observe(false);
        drop(app.views[1].clear());
        dock::remove_view(&mut app.dock, 1);
        app.observe(true);
        app.hold_loads = true;
        app.next_file_id = 10;
        app.undo_redo(&ctx, false);
        let tx = app.views[1].held.take().unwrap();
        tx.send(crate::views::LoadMsg::Markers(Ok((crate::views::sample_data(&text, &["M"]), vec![])))).unwrap();
        let loaded = app.views[1].poll_markers().unwrap();
        app.finish_load(&ctx, 1, loaded);
        app.views[1].fit_pending = false;
        app.views[1].fresh = false;
        app.observe(false);
        let reloaded = app.views[1].file_id;
        assert_ne!(reloaded, 2);
        app.undo_redo(&ctx, true);
        assert_eq!(app.layout_tabs.closed_len(), 1);
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.closed_len(), 0);
        assert_eq!(app.views[1].file_id, reloaded);
        assert_eq!(app.views[1].pb.frame, 7);
        assert!(app.views[1].held.is_none());
        assert!(app.views[1].loading.is_none());
        std::fs::remove_file(path).unwrap();
    }

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
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        show(&mut app, 0, "saved_close.takm", 1);
        show(&mut app, 1, "kept_close.tak", 2);
        app.observe(false);
        let patch = prepare(&app, 0, 5.0);
        app.commit_marker_edit(0, patch.clone()).unwrap();
        let identity = patch.document_id();
        let edited = app.views[0].data.as_ref().unwrap().rebuild_markers(&patch.after_snapshot()).unwrap();
        app.views[0].publish_edited_view(edited).unwrap();
        // Simulate the completed save state, then use the real close path. A cleared
        // synthetic view has no worker stamp and cannot prove a disk fallback is fresh.
        {
            let mut document = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
            let saved = document.snapshot();
            document.mark_saved(&saved, "saved_close.takm".into()).unwrap();
        }
        assert!(!app.views[0].marker_dirty());
        app.close_view(0);
        app.observe(true);
        assert_eq!(app.history.kinds(), vec!["MarkerEdit", "Layout"]);
        assert_eq!(app.layout_tabs.closed_len(), 1);
        app.undo_redo(&ctx, false);
        assert_eq!(app.layout_tabs.closed_len(), 0);
        assert!(app.views[0].loading.is_none());
        assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().id(), identity);
        assert_eq!(app.views[0].data.as_ref().unwrap().edit_revision, Some(patch.after_revision()));
        assert_eq!(app.views[0].selected.iter().copied().collect::<Vec<_>>(), vec![0]);
        assert_eq!(app.views[0].armed_label, Some(LabelKey { parent: [8; 16], slot: 2 }));
        assert!(!app.views[0].marker_dirty());
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().revision(), patch.before_revision());
        assert!(app.views[0].marker_dirty(), "undoing the saved edit keeps the original savepoint");
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
