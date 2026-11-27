
use super::theme;
use crate::views::MAX_VIEWS;
use eframe::egui::{self, pos2, Color32, Pos2, Rect, Rounding, Stroke};
use egui_dock::{DockState, Node, NodeIndex, OverlayType, Style, SurfaceIndex, TabIndex, TabInteractionStyle, TabStyle, Translations, Tree};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Saved-layout schema; valid older layouts migrate in place and keep their panels/views.
/// 1 = v0.7.0: graph / devices below the views, graph is its own pane.
/// 2 = v0.10.0: the graph floats over the 3D views, so no pane below them; devices is a tab
/// next to the selected marker.
/// 3 = v0.32.0: Labeling / Edit Tools tabs; v0.32.5 also merges any legacy Labels in place.
pub const LAYOUT_VERSION: u32 = 3;
/// Height of the tab row (file tabs of the 3D views, pane titles).
pub const TAB_H: f32 = 30.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "SavedPane")]
pub enum Pane {
    /// Editor area with no open 3D views; keeps the surrounding pane positions.
    Empty,
    /// Fixed-height host for the top-level layout tabs, above all 3D views.
    LayoutTabs,
    /// 3D view of one file; the number is its slot (0..MAX_VIEWS) in `TakViewerApp::views`.
    View(u8),
    Explorer,
    /// Legacy saved marker-list pane; merged into `Labeling` before layout validation.
    Labels,
    Labeling,
    EditTools,
    /// Facts about the selected marker (its X/Y/Z graph is `Graph`).
    Selected,
    Graph,
    Devices,
    /// The Comments of the active view's file (v0.17.0, `ui/comment_list.rs`).
    Comments,
}

/// Panes as read from the settings. Before v0.6.0 there was one 3D view, saved as `Scene`.
#[derive(Deserialize)]
enum SavedPane {
    Empty,
    LayoutTabs,
    Scene,
    View(u8),
    Explorer,
    Labels,
    Labeling,
    EditTools,
    Selected,
    Graph,
    Devices,
    Comments,
}

impl From<SavedPane> for Pane {
    fn from(p: SavedPane) -> Pane {
        match p {
            SavedPane::Empty => Pane::Empty,
            SavedPane::LayoutTabs => Pane::LayoutTabs,
            SavedPane::Scene => Pane::View(0),
            SavedPane::View(k) => Pane::View(k),
            SavedPane::Explorer => Pane::Explorer,
            SavedPane::Labels => Pane::Labels,
            SavedPane::Labeling => Pane::Labeling,
            SavedPane::EditTools => Pane::EditTools,
            SavedPane::Selected => Pane::Selected,
            SavedPane::Graph => Pane::Graph,
            SavedPane::Devices => Pane::Devices,
            SavedPane::Comments => Pane::Comments,
        }
    }
}

/// The places around the 3D views that a toggle shows / hides as a whole (VS Code: primary
/// side bar, panel, secondary side bar).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Left,
    Bottom,
    Right,
}

impl Pane {
    /// Panes other than the 3D views (each at most once, can be closed from the "보기" menu).
    pub const PANELS: [Pane; 7] = [Pane::Explorer, Pane::Labeling, Pane::EditTools, Pane::Selected, Pane::Graph, Pane::Devices, Pane::Comments];

    /// Title on the pane's tab (a 3D view's tab shows its file name instead, `app.rs`).
    pub fn title(self) -> &'static str {
        match self {
            Pane::Empty => "",
            Pane::LayoutTabs => "",
            Pane::View(_) => "3D View",
            Pane::Explorer => "EXPLORER",
            Pane::Labels | Pane::Labeling => "Labeling",
            Pane::EditTools => "Edit Tools",
            Pane::Selected => "Marker Info",
            Pane::Graph => "GRAPH",
            Pane::Devices => "Devices",
            Pane::Comments => "COMMENTS",
        }
    }

    /// Where the pane sits in the first arrangement (None for the 3D views).
    pub fn home(self) -> Option<Side> {
        match self {
            Pane::View(_) | Pane::Empty | Pane::LayoutTabs => None,
            Pane::Explorer => Some(Side::Left),
            Pane::Labels | Pane::Labeling | Pane::EditTools | Pane::Selected | Pane::Devices | Pane::Comments => Some(Side::Right),
            Pane::Graph => Some(Side::Bottom),
        }
    }

    /// The pane this one shares a place with (as tabs) in the first arrangement.
    fn tab_mate(self) -> Option<Pane> {
        match self {
            Pane::Labeling => Some(Pane::EditTools),
            Pane::EditTools => Some(Pane::Labeling),
            Pane::Selected => Some(Pane::Devices),
            Pane::Devices => Some(Pane::Selected),
            // Not in the first arrangement (v0.17.0): opened, it joins the selected marker.
            Pane::Comments => Some(Pane::Selected),
            _ => None,
        }
    }
}

fn translations() -> Translations {
    let mut t = Translations::english();
    t.tab_context_menu.close_button = "Close".to_owned();
    t.tab_context_menu.eject_button = "Float Panel".to_owned();
    t.window.close_button_tooltip = "Close Window".to_owned();
    t
}

/// Screen area used to place new views before the first frame is drawn (window default size).
pub const NOMINAL_AREA: Rect = Rect { min: pos2(0.0, 0.0), max: pos2(1600.0, 900.0) };

/// First arrangement with one 3D view.
pub fn default_layout() -> DockState<Pane> {
    default_layout_for(&[0])
}

/// The first arrangement with these 3D views (first one where the single view sits): Explorer
/// left, views in the middle down to the bottom (the graph floats over them), Labeling / Edit
/// Tools on the right with selected marker / devices below them. Split fractions are the share of
/// the left / upper part.
pub fn default_layout_for(slots: &[u8]) -> DockState<Pane> {
    let first = slots.first().copied().unwrap_or(0);
    let mut d = DockState::new(vec![if slots.is_empty() { Pane::Empty } else { Pane::View(first) }]);
    let s = d.main_surface_mut();
    let [scene, _explorer] = s.split_left(NodeIndex::root(), 0.19, vec![Pane::Explorer]);
    let [scene, right] = s.split_right(scene, 0.70, vec![Pane::Labeling, Pane::EditTools]);
    s.split_below(right, 0.67, vec![Pane::Selected, Pane::Devices]);
    s.set_focused_node(scene);
    d.translations = translations();
    for &k in &slots[1.min(slots.len())..] {
        add_view(&mut d, k, NOMINAL_AREA);
    }
    focus_view(&mut d, first);
    d
}

/// Only one 3D view (command line captures without side panes).
pub fn scene_only(slot: u8) -> DockState<Pane> {
    let mut d = DockState::new(vec![Pane::View(slot)]);
    d.translations = translations();
    d
}

/// A saved layout requires a main-window editor (a view or the empty placeholder), valid
/// view slots and no duplicate panes; otherwise the default layout is used.
pub fn sanitize(dock: DockState<Pane>) -> DockState<Pane> {
    let mut counts: HashMap<Pane, usize> = HashMap::new();
    let mut view_elsewhere = false;
    for ((surface, _), tab) in dock.iter_all_tabs() {
        *counts.entry(*tab).or_default() += 1;
        match tab {
            Pane::View(k) => view_elsewhere |= surface != SurfaceIndex::main() || usize::from(*k) >= MAX_VIEWS,
            Pane::Empty => view_elsewhere |= surface != SurfaceIndex::main(),
            _ => {}
        }
    }
    let slots = views(&dock);
    let empty = counts.contains_key(&Pane::Empty);
    if (slots.is_empty() != empty) || view_elsewhere || counts.values().any(|&c| c > 1) {
        return default_layout();
    }
    let mut d = dock;
    d.translations = translations();
    d
}

/// Slots of the 3D views in the layout, smallest first.
pub fn views(dock: &DockState<Pane>) -> Vec<u8> {
    let mut v: Vec<u8> = dock
        .iter_all_tabs()
        .filter_map(|(_, t)| match t {
            Pane::View(k) => Some(*k),
            _ => None,
        })
        .collect();
    v.sort_unstable();
    v
}

/// Slots of the 3D views in reading order on screen: top to bottom, then left to right (from
/// the split fractions, so known before drawing); views tabbed in one place in tab order.
/// OPEN EDITORS numbers its groups "화면 1" … this way.
pub fn views_reading_order(dock: &DockState<Pane>) -> Vec<u8> {
    let s = dock.main_surface();
    let mut places: Vec<(Rect, Vec<u8>)> = leaf_rects(s, NOMINAL_AREA)
        .into_iter()
        .filter_map(|(n, r)| {
            let slots: Vec<u8> = s[n]
                .tabs()?
                .iter()
                .filter_map(|t| match t {
                    Pane::View(k) => Some(*k),
                    _ => None,
                })
                .collect();
            (!slots.is_empty()).then_some((r, slots))
        })
        .collect();
    places.sort_by(|a, b| a.0.top().round().total_cmp(&b.0.top().round()).then(a.0.left().total_cmp(&b.0.left())));
    places.into_iter().flat_map(|(_, k)| k).collect()
}

/// The smallest slot that has no view yet (None when `MAX_VIEWS` are open).
pub fn free_slot(dock: &DockState<Pane>) -> Option<u8> {
    let used = views(dock);
    (0..MAX_VIEWS as u8).find(|k| !used.contains(k))
}

fn has_view(node: &Node<Pane>) -> bool {
    node.tabs().is_some_and(|t| t.iter().any(|p| matches!(p, Pane::View(_) | Pane::Empty)))
}

/// Rectangles of the main-window leaves, from the split fractions inside `area`
/// (known before anything is drawn; tab bars are ignored).
fn leaf_rects(tree: &Tree<Pane>, area: Rect) -> Vec<(NodeIndex, Rect)> {
    fn walk(tree: &Tree<Pane>, at: NodeIndex, r: Rect, out: &mut Vec<(NodeIndex, Rect)>) {
        if at.0 >= tree.len() {
            return;
        }
        match &tree[at] {
            Node::Leaf { .. } => out.push((at, r)),
            Node::Horizontal { fraction, .. } => {
                let x = r.left() + r.width() * fraction;
                walk(tree, at.left(), Rect::from_min_max(r.min, pos2(x, r.bottom())), out);
                walk(tree, at.right(), Rect::from_min_max(pos2(x, r.top()), r.max), out);
            }
            Node::Vertical { fraction, .. } => {
                let y = r.top() + r.height() * fraction;
                walk(tree, at.left(), Rect::from_min_max(r.min, pos2(r.right(), y)), out);
                walk(tree, at.right(), Rect::from_min_max(pos2(r.left(), y), r.max), out);
            }
            Node::Empty => {}
        }
    }
    let mut out = Vec::new();
    if !tree.is_empty() {
        walk(tree, NodeIndex::root(), area, &mut out);
    }
    out
}

/// The smallest part of the layout that holds every 3D view (a new bottom pane goes below it,
/// so it sits between the side panes like the VS Code panel).
fn views_root(tree: &Tree<Pane>) -> NodeIndex {
    let mut top: Option<usize> = None;
    for (i, node) in tree.iter().enumerate() {
        if has_view(node) {
            top = Some(match top {
                None => i,
                Some(mut a) => {
                    let mut b = i;
                    while a != b {
                        if a > b {
                            a = (a - 1) / 2;
                        } else {
                            b = (b - 1) / 2;
                        }
                    }
                    a
                }
            });
        }
    }
    NodeIndex(top.unwrap_or(0))
}

/// The editor's split tree, without the surrounding panels. It is small enough to save with
/// each top-level layout tab; the view data and view state stay in their own runtime slots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EditorLayout {
    Empty,
    Leaf { slots: Vec<u8>, active: u8 },
    Horizontal { fraction: f32, left: Box<EditorLayout>, right: Box<EditorLayout> },
    Vertical { fraction: f32, top: Box<EditorLayout>, bottom: Box<EditorLayout> },
}

/// Ignore corrupt, repeated or out-of-range slots in a saved layout, collapsing empty splits.
pub fn valid_editor_layout(layout: &EditorLayout) -> EditorLayout {
    fn clean(layout: &EditorLayout, seen: &mut [bool; MAX_VIEWS]) -> Option<EditorLayout> {
        match layout {
            EditorLayout::Empty => None,
            EditorLayout::Leaf { slots, active } => {
                let mut kept = Vec::new();
                for &slot in slots {
                    let Some(found) = seen.get_mut(usize::from(slot)) else { continue };
                    if !*found { *found = true; kept.push(slot); }
                }
                if kept.is_empty() { return None; }
                let chosen = if kept.contains(active) { *active } else { kept[0] };
                Some(EditorLayout::Leaf { slots: kept, active: chosen })
            }
            EditorLayout::Horizontal { fraction, left, right } => {
                let left = clean(left, seen);
                let right = clean(right, seen);
                match (left, right) {
                    (Some(left), Some(right)) => Some(EditorLayout::Horizontal {
                        fraction: if fraction.is_finite() { fraction.clamp(0.05, 0.95) } else { 0.5 },
                        left: Box::new(left), right: Box::new(right),
                    }),
                    (left, right) => left.or(right),
                }
            }
            EditorLayout::Vertical { fraction, top, bottom } => {
                let top = clean(top, seen);
                let bottom = clean(bottom, seen);
                match (top, bottom) {
                    (Some(top), Some(bottom)) => Some(EditorLayout::Vertical {
                        fraction: if fraction.is_finite() { fraction.clamp(0.05, 0.95) } else { 0.5 },
                        top: Box::new(top), bottom: Box::new(bottom),
                    }),
                    (top, bottom) => top.or(bottom),
                }
            }
        }
    }
    clean(layout, &mut [false; MAX_VIEWS]).unwrap_or(EditorLayout::Empty)
}

/// Extract just the views. A panel between two view leaves stays in the main dock and is
/// omitted from the tab's tree; the view leaves on its sides remain in the same order.
pub fn editor_layout(dock: &DockState<Pane>) -> EditorLayout {
    fn walk(tree: &Tree<Pane>, at: NodeIndex) -> Option<EditorLayout> {
        if at.0 >= tree.len() { return None; }
        match &tree[at] {
            Node::Empty => None,
            Node::Leaf { tabs, active, .. } => {
                let slots: Vec<u8> = tabs.iter().filter_map(|p| match p { Pane::View(k) => Some(*k), _ => None }).collect();
                if slots.is_empty() {
                    return tabs.contains(&Pane::Empty).then_some(EditorLayout::Empty);
                }
                let chosen = tabs.get(active.0).and_then(|p| match p { Pane::View(k) => Some(*k), _ => None }).unwrap_or(slots[0]);
                Some(EditorLayout::Leaf { slots, active: chosen })
            }
            Node::Horizontal { fraction, .. } | Node::Vertical { fraction, .. } => {
                let left = walk(tree, at.left());
                let right = walk(tree, at.right());
                match (left, right) {
                    (None, None) => None,
                    (Some(one), None) | (None, Some(one)) => Some(one),
                    (Some(left), Some(right)) => Some(match &tree[at] {
                        Node::Horizontal { .. } => EditorLayout::Horizontal { fraction: *fraction, left: Box::new(left), right: Box::new(right) },
                        _ => EditorLayout::Vertical { fraction: *fraction, top: Box::new(left), bottom: Box::new(right) },
                    }),
                }
            }
        }
    }
    walk(dock.main_surface(), NodeIndex::root()).unwrap_or(EditorLayout::Empty)
}

/// Replace only the view tree, leaving all side/bottom panels and their state in the dock.
/// The last view becomes the existing `Empty` placeholder before new splits are installed.
pub fn set_editor_layout(dock: &mut DockState<Pane>, layout: &EditorLayout) {
    for slot in views(dock) { remove_view(dock, slot); }
    let Some((surface, node, _)) = dock.find_tab(&Pane::Empty) else { return };
    if surface != SurfaceIndex::main() { return; }
    fn put(tree: &mut Tree<Pane>, node: NodeIndex, layout: &EditorLayout) {
        match layout {
            EditorLayout::Empty => {}
            EditorLayout::Leaf { slots, active } => {
                if let Node::Leaf { tabs, active: selected, .. } = &mut tree[node] {
                    let Some(at) = tabs.iter().position(|p| *p == Pane::Empty) else { return };
                    let mut unique = Vec::new();
                    for &slot in slots {
                        if usize::from(slot) < MAX_VIEWS && !unique.contains(&slot) { unique.push(slot); }
                    }
                    if unique.is_empty() { return; }
                    tabs.splice(at..=at, unique.iter().copied().map(Pane::View));
                    *selected = TabIndex(at + unique.iter().position(|slot| slot == active).unwrap_or(0));
                }
            }
            EditorLayout::Horizontal { fraction, left, right } => {
                let [a, b] = tree.split_right(node, fraction.clamp(0.05, 0.95), vec![Pane::Empty]);
                put(tree, a, left);
                put(tree, b, right);
            }
            EditorLayout::Vertical { fraction, top, bottom } => {
                let [a, b] = tree.split_below(node, fraction.clamp(0.05, 0.95), vec![Pane::Empty]);
                put(tree, a, top);
                put(tree, b, bottom);
            }
        }
    }
    put(dock.main_surface_mut(), node, &valid_editor_layout(layout));
}

/// Add a small, non-closeable row above the whole editor, never above the side panels.
/// Older saved docks have no row; adding it here migrates them without rearranging panels.
pub fn ensure_layout_strip(dock: &mut DockState<Pane>, active: u8) {
    if has(dock, Pane::LayoutTabs) { return; }
    let tree = dock.main_surface_mut();
    let root = views_root(tree);
    tree.split_above(root, 0.05, vec![Pane::LayoutTabs]);
    focus_view(dock, active);
}

/// Keep the host row one tab-bar high when the window or a side panel is resized.
pub fn size_layout_strip(dock: &mut DockState<Pane>) {
    let Some((surface, node, _)) = dock.find_tab(&Pane::LayoutTabs) else { return };
    if surface != SurfaceIndex::main() || node.0 == 0 || node.0 % 2 == 0 { return; }
    let parent = NodeIndex((node.0 - 1) / 2);
    if let Node::Vertical { rect, fraction } = &mut dock[surface][parent] {
        if rect.height().is_finite() && rect.height() > TAB_H * 2.0 {
            *fraction = (TAB_H / rect.height()).clamp(0.0, 0.5);
        }
    }
}

/// The pane whose dock tab egui is dragging (`ctx.dragged_id()`). Uses egui_dock 0.14's tab id
/// inside `DockArea::id("panes")` (`show/leaf.rs`).
pub fn dragged_pane(dock: &DockState<Pane>, id: egui::Id) -> Option<Pane> {
    let root = egui::Id::new("panes");
    dock.iter_surfaces().enumerate().find_map(|(s, surface)| {
        surface.node_tree()?.iter().enumerate().find_map(|(n, node)| {
            node.tabs()?.iter().enumerate().find_map(|(t, tab)| {
                (root.with((SurfaceIndex(s), "surface")).with((NodeIndex(n), "node")).with((t, "tab")) == id).then_some(*tab)
            })
        })
    })
}

/// Screen rectangle for the strip. `None` until the dock has been drawn at least once.
pub fn layout_strip_rect(dock: &DockState<Pane>) -> Option<Rect> {
    let (surface, node, _) = dock.find_tab(&Pane::LayoutTabs)?;
    let rect = dock[surface][node].rect()?;
    (rect.is_positive() && rect.is_finite()).then_some(rect)
}

pub fn empty_rect(dock: &DockState<Pane>) -> Option<Rect> {
    let (surface, node, _) = dock.find_tab(&Pane::Empty)?;
    dock[surface][node].rect().filter(|r| r.is_positive() && r.is_finite())
}

/// Put a new 3D view next to the others: the biggest view is cut in half. The second view
/// goes to the right of the first (VS Code "open to the side"); later ones go below unless the
/// view is clearly wider than tall (1 → 2 side by side → 3 → 2×2). The new view gets the focus.
pub fn add_view(dock: &mut DockState<Pane>, slot: u8, area: Rect) {
    if dock.find_tab(&Pane::View(slot)).is_some() {
        return;
    }
    if views(dock).is_empty() {
        *dock = default_layout_for(&[slot]);
        return;
    }
    let count = views(dock).len();
    let s = dock.main_surface_mut();
    let rects = leaf_rects(s, area);
    let mut best: Option<(NodeIndex, Rect)> = None;
    for &(n, r) in rects.iter().filter(|(n, _)| has_view(&s[*n])) {
        if best.map_or(true, |(_, b)| r.area() > b.area() * 1.001) {
            best = Some((n, r));
        }
    }
    let new = vec![Pane::View(slot)];
    let [_, added] = match best {
        Some((n, r)) if count >= 2 && r.height() > r.width() * 0.8 => s.split_below(n, 0.5, new),
        Some((n, _)) => s.split_right(n, 0.5, new),
        None => s.split_right(NodeIndex::root(), 0.5, new),
    };
    s.set_focused_node(added);
}

/// Take a 3D view out of the layout. The last one becomes an empty editor in the same place.
pub fn remove_view(dock: &mut DockState<Pane>, slot: u8) {
    if let Some((surface, node, tab)) = dock.find_tab(&Pane::View(slot)) {
        if views(dock).len() == 1 {
            if let Node::Leaf { tabs, .. } = &mut dock[surface][node] {
                tabs[tab.0] = Pane::Empty;
            }
        } else {
            dock.remove_tab((surface, node, tab));
        }
    }
}

/// Make a 3D view the shown tab of its place and give that place the focus.
pub fn focus_view(dock: &mut DockState<Pane>, slot: u8) {
    if let Some((surface, node, tab)) = dock.find_tab(&Pane::View(slot)) {
        dock.set_active_tab((surface, node, tab));
        dock.set_focused_node_and_surface((surface, node));
    }
}

pub fn has(dock: &DockState<Pane>, pane: Pane) -> bool {
    dock.find_tab(&pane).is_some()
}

/// Close a pane (3D views are closed with their tab ✕, `remove_view`).
pub fn hide(dock: &mut DockState<Pane>, pane: Pane) {
    if matches!(pane, Pane::View(_) | Pane::Empty) {
        return;
    }
    if let Some(loc) = dock.find_tab(&pane) {
        dock.remove_tab(loc);
    }
}

/// Bring a pane back: shown as the active tab if it is open; otherwise it goes to its first
/// place (`Pane::home`): Explorer on the left edge; selected marker / devices as a tab next to
/// the other one when that is in the main window; else right-side panes below another
/// right-side pane (or on the right edge); the graph pane below the 3D views.
pub fn show(dock: &mut DockState<Pane>, pane: Pane) {
    if let Some(loc) = dock.find_tab(&pane) {
        dock.set_active_tab(loc);
        return;
    }
    let s = dock.main_surface_mut();
    if let Some((node, _)) = pane.tab_mate().and_then(|m| s.find_tab(&m)) {
        if let Node::Leaf { tabs, active, .. } = &mut s[node] {
            tabs.push(pane);
            *active = TabIndex(tabs.len() - 1);
            return;
        }
    }
    let mate = Pane::PANELS.iter().filter(|p| p.home() == pane.home()).find_map(|p| s.find_tab(p)).map(|(node, _)| node);
    match (pane.home(), mate) {
        (Some(Side::Right), Some(node)) => {
            s.split_below(node, 0.5, vec![pane]);
        }
        (Some(Side::Right), None) => {
            s.split_right(NodeIndex::root(), 0.75, vec![pane]);
        }
        (Some(Side::Bottom), Some(node)) => {
            if let Node::Leaf { tabs, active, .. } = &mut s[node] {
                tabs.push(pane);
                *active = TabIndex(tabs.len() - 1);
            }
        }
        (Some(Side::Bottom), None) => {
            let top = views_root(s);
            s.split_below(top, 0.72, vec![pane]);
        }
        _ => {
            s.split_left(NodeIndex::root(), 0.19, vec![pane]);
        }
    }
}

// ───────────────────────────── Toggles ─────────────────────────────

/// One hidden place of panes: its tabs, which one was shown, and its height (side panes) or
/// width (bottom panes) in points, to give the places their old proportions back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StashLeaf {
    pub tabs: Vec<Pane>,
    pub active: usize,
    pub size: f32,
}

/// What a toggle took off one side: the places top to bottom (side) or left to right (bottom),
/// and the share of the window (side) or of the views + panes height (bottom) they had.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stash {
    pub share: f32,
    pub leaves: Vec<StashLeaf>,
}

/// Panes hidden by the toggles, per side. Saved with the settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hidden {
    pub left: Option<Stash>,
    pub bottom: Option<Stash>,
    pub right: Option<Stash>,
}

/// Merge the former marker-list pane before validating a saved layout. Prefer an existing
/// `Labeling` pane, even when a side toggle has hidden it; otherwise rename the old pane in
/// place. Remove other copies without moving the retained docked or floating pane. A closed
/// marker-list pane stays closed. This runs before the app starts recording layout history.
pub fn merge_label_panels(dock: &mut DockState<Pane>, hidden: &mut Hidden) {
    let existing = has(dock, Pane::Labeling) || hidden.panes().any(|p| p == Pane::Labeling);
    let mut retained = false;
    let mut merge = |pane: &mut Pane| {
        if *pane == Pane::Labeling || (!existing && *pane == Pane::Labels) {
            *pane = if retained { Pane::Labels } else { Pane::Labeling };
            retained = true;
        }
    };
    for (_, pane) in dock.iter_all_tabs_mut() {
        merge(pane);
    }
    for stash in [&mut hidden.left, &mut hidden.bottom, &mut hidden.right].into_iter().flatten() {
        for pane in stash.leaves.iter_mut().flat_map(|leaf| &mut leaf.tabs) {
            merge(pane);
        }
    }
    // Removing through DockState also removes empty leaves/windows and repairs active tabs.
    while let Some(loc) = dock.find_tab(&Pane::Labels) {
        dock.remove_tab(loc);
    }
    hidden.forget(Pane::Labels);
}

/// Add Edit Tools to a pre-version-3 arrangement without reopening a closed marker list.
pub fn migrate_labeling(dock: &mut DockState<Pane>, hidden: &mut Hidden) {
    merge_label_panels(dock, hidden);
    if has(dock, Pane::EditTools) || hidden.panes().any(|p| p == Pane::EditTools) {
        return;
    }
    if let Some((surface, node, tab)) = dock.find_tab(&Pane::Labeling) {
        if let Node::Leaf { tabs, active, .. } = &mut dock[surface][node] {
            tabs.insert(tab.0 + 1, Pane::EditTools);
            if active.0 > tab.0 {
                active.0 += 1;
            }
            return;
        }
    }
    for stash in [&mut hidden.left, &mut hidden.bottom, &mut hidden.right].into_iter().flatten() {
        for leaf in &mut stash.leaves {
            if let Some(at) = leaf.tabs.iter().position(|p| *p == Pane::Labeling) {
                leaf.tabs.insert(at + 1, Pane::EditTools);
                if leaf.active > at {
                    leaf.active += 1;
                }
                return;
            }
        }
    }
    show(dock, Pane::EditTools);
}

impl Hidden {
    fn slot(&mut self, side: Side) -> &mut Option<Stash> {
        match side {
            Side::Left => &mut self.left,
            Side::Bottom => &mut self.bottom,
            Side::Right => &mut self.right,
        }
    }

    fn panes(&self) -> impl Iterator<Item = Pane> + '_ {
        [&self.left, &self.bottom, &self.right].into_iter().flatten().flat_map(|s| s.leaves.iter()).flat_map(|l| l.tabs.iter().copied())
    }

    /// A pane shown or closed some other way is no longer hidden by a toggle.
    pub fn forget(&mut self, pane: Pane) {
        self.keep(|p| p != pane);
    }

    fn keep(&mut self, mut keep: impl FnMut(Pane) -> bool) {
        for side in [Side::Left, Side::Bottom, Side::Right] {
            let slot = self.slot(side);
            let mut empty = false;
            if let Some(stash) = slot.as_mut() {
                for leaf in &mut stash.leaves {
                    let mut at = 0;
                    let mut active = 0;
                    leaf.tabs.retain(|p| {
                        let retained = keep(*p);
                        if at < leaf.active && retained {
                            active += 1;
                        }
                        at += 1;
                        retained
                    });
                    leaf.active = active.min(leaf.tabs.len().saturating_sub(1));
                }
                stash.leaves.retain(|l| !l.tabs.is_empty());
                empty = stash.leaves.is_empty();
            }
            if empty {
                *slot = None;
            }
        }
    }

    /// Settings as read: no 3D views, nothing that is on screen, nothing twice.
    pub fn clean(&mut self, dock: &DockState<Pane>) {
        let mut seen: Vec<Pane> = dock.iter_all_tabs().map(|(_, t)| *t).collect();
        self.keep(|p| {
            let ok = !matches!(p, Pane::View(_)) && !seen.contains(&p);
            seen.push(p);
            ok
        });
    }
}

fn side_of(center: Pos2, views: Rect) -> Option<Side> {
    if center.x < views.left() {
        Some(Side::Left)
    } else if center.x > views.right() {
        Some(Side::Right)
    } else if center.y > views.bottom() {
        Some(Side::Bottom)
    } else {
        None
    }
}

/// Places without a 3D view that lie on `side` of the views (top to bottom for the sides,
/// left to right for the bottom), and the rectangle around all views.
fn side_leaves(tree: &Tree<Pane>, area: Rect, side: Side) -> (Vec<(NodeIndex, Rect)>, Rect) {
    let rects = leaf_rects(tree, area);
    let mut views = Rect::NOTHING;
    for (n, r) in &rects {
        if has_view(&tree[*n]) {
            views = views.union(*r);
        }
    }
    let mut out: Vec<(NodeIndex, Rect)> = rects.into_iter().filter(|(n, r)| !has_view(&tree[*n]) && side_of(r.center(), views) == Some(side)).collect();
    let key = |r: &Rect| if side == Side::Bottom { r.left() } else { r.top() };
    out.sort_by(|a, b| key(&a.1).total_cmp(&key(&b.1)));
    (out, views)
}

/// Panes are docked on this side of the 3D views now (the toggle is "on").
pub fn side_open(dock: &DockState<Pane>, area: Rect, side: Side) -> bool {
    !side_leaves(dock.main_surface(), area, side).0.is_empty()
}

/// Take every pane docked on this side off the screen. None when there is none.
fn hide_side(dock: &mut DockState<Pane>, area: Rect, side: Side) -> Option<Stash> {
    let s = dock.main_surface();
    let (leaves, views) = side_leaves(s, area, side);
    if leaves.is_empty() {
        return None;
    }
    let around = leaves.iter().fold(Rect::NOTHING, |a, (_, r)| a.union(*r));
    let share = match side {
        Side::Bottom => around.height() / (views.height() + around.height()).max(1.0),
        _ => around.width() / area.width().max(1.0),
    };
    let stash = Stash {
        share,
        leaves: leaves
            .iter()
            .map(|(n, r)| StashLeaf {
                tabs: s[*n].tabs().map(<[Pane]>::to_vec).unwrap_or_default(),
                active: match &s[*n] {
                    Node::Leaf { active, .. } => active.0,
                    _ => 0,
                },
                size: if side == Side::Bottom { r.width() } else { r.height() },
            })
            .collect(),
    };
    for pane in stash.leaves.iter().flat_map(|l| l.tabs.iter()) {
        if let Some(loc) = dock.find_tab(pane) {
            dock.remove_tab(loc);
        }
    }
    Some(stash)
}

/// Put hidden panes back on their side: a column on the left / right edge of the window, or
/// a row below the 3D views, with the proportions they had.
fn show_side(dock: &mut DockState<Pane>, stash: &Stash, side: Side) {
    let leaves: Vec<&StashLeaf> = stash.leaves.iter().filter(|l| !l.tabs.is_empty()).collect();
    let Some(first) = leaves.first() else { return };
    let share = stash.share.clamp(0.05, 0.7);
    let s = dock.main_surface_mut();
    let [_, mut node] = match side {
        Side::Left => s.split_left(NodeIndex::root(), share, first.tabs.clone()),
        Side::Right => s.split_right(NodeIndex::root(), 1.0 - share, first.tabs.clone()),
        Side::Bottom => {
            let top = views_root(s);
            s.split_below(top, 1.0 - share, first.tabs.clone())
        }
    };
    let set_active = |s: &mut Tree<Pane>, node: NodeIndex, k: usize| {
        if let Node::Leaf { tabs, active, .. } = &mut s[node] {
            *active = TabIndex(k.min(tabs.len().saturating_sub(1)));
        }
    };
    set_active(s, node, first.active);
    let mut rest: f32 = leaves.iter().map(|l| l.size.max(1.0)).sum();
    for pair in leaves.windows(2) {
        let kept = pair[0].size.max(1.0);
        let fraction = (kept / rest).clamp(0.05, 0.95);
        rest -= kept;
        let [_, new] = match side {
            Side::Bottom => s.split_right(node, fraction, pair[1].tabs.clone()),
            _ => s.split_below(node, fraction, pair[1].tabs.clone()),
        };
        node = new;
        set_active(s, node, pair[1].active);
    }
}

/// The panes that belong to this side in the first arrangement and are neither on screen nor
/// hidden by another toggle.
fn home_stash(dock: &DockState<Pane>, hidden: &Hidden, side: Side) -> Option<Stash> {
    let free: Vec<Pane> = Pane::PANELS.iter().copied().filter(|p| p.home() == Some(side) && !has(dock, *p) && !hidden.panes().any(|h| h == *p)).collect();
    if free.is_empty() {
        return None;
    }
    let leaf = |tabs: Vec<Pane>, size: f32| StashLeaf { tabs, active: 0, size };
    Some(match side {
        Side::Left => Stash { share: 0.19, leaves: vec![leaf(free, 1.0)] },
        Side::Bottom => Stash { share: 0.28, leaves: vec![leaf(free, 1.0)] },
        Side::Right => {
            // Editing tabs on top; selected marker / devices share one place below them.
            let (labels, rest): (Vec<Pane>, Vec<Pane>) = free.into_iter().partition(|p| matches!(p, Pane::Labeling | Pane::EditTools));
            Stash { share: 0.25, leaves: [(labels, 0.6), (rest, 0.4)].into_iter().filter(|(tabs, _)| !tabs.is_empty()).map(|(tabs, size)| leaf(tabs, size)).collect() }
        }
    })
}

/// Toggle one side: panes docked there are hidden as a whole; when there are none, the ones
/// hidden before come back where they were (or, the first time, the side's own panes).
/// Floating windows and panes stacked on a 3D view are left alone.
pub fn toggle_side(dock: &mut DockState<Pane>, hidden: &mut Hidden, area: Rect, side: Side) {
    if let Some(mut stash) = hide_side(dock, area, side) {
        if let Some(older) = hidden.slot(side).take() {
            stash.leaves.extend(older.leaves);
        }
        *hidden.slot(side) = Some(stash);
        return;
    }
    let before = hidden.slot(side).take();
    let stash = before.or_else(|| home_stash(dock, hidden, side));
    if let Some(stash) = stash {
        show_side(dock, &stash, side);
    }
}

// ───────────────────────────── Tab look ─────────────────────────────

fn tab(bg: Color32, text: Color32) -> TabInteractionStyle {
    TabInteractionStyle { outline_color: Color32::TRANSPARENT, rounding: Rounding::ZERO, bg_fill: bg, text_color: text }
}

fn with_states(base: &TabStyle, shown: TabInteractionStyle, shown_unfocused: TabInteractionStyle, other: TabInteractionStyle, hovered: TabInteractionStyle) -> TabStyle {
    let mut t = base.clone();
    t.focused = shown.clone();
    t.focused_with_kb_focus = shown;
    t.active = shown_unfocused.clone();
    t.active_with_kb_focus = shown_unfocused;
    t.inactive = other.clone();
    t.inactive_with_kb_focus = other;
    t.hovered = hovered;
    t.hline_below_active_tab_name = false;
    t
}

/// File tab of a 3D view (VS Code editor tab). `chosen` = the app's active view: its tab is
/// bright whichever pane has the keyboard; the other views' tabs are dim.
pub fn view_tab_style(base: &TabStyle, chosen: bool) -> TabStyle {
    let bright = tab(theme::VIEW_BG, theme::TAB_TEXT_ACTIVE);
    let dim = tab(theme::VIEW_BG, theme::TAB_TEXT_DIM);
    let shown = if chosen { bright } else { dim };
    with_states(base, shown.clone(), shown, tab(theme::TAB_BG_INACTIVE, theme::TAB_TEXT_DIM), tab(theme::TAB_BG_INACTIVE, theme::TEXT))
}

/// Title of a side / bottom pane: plain text on the tab row, no tab shape (VS Code view title).
pub fn panel_tab_style(base: &TabStyle) -> TabStyle {
    let shown = tab(theme::TAB_BAR_BG, theme::TEXT);
    with_states(base, shown.clone(), shown, tab(theme::TAB_BAR_BG, theme::TAB_TEXT_DIM), tab(theme::TAB_BAR_BG, theme::TAB_TEXT_ACTIVE))
}

/// Width of egui_dock's close-button square at the right end of a tab
/// (`Style::TAB_CLOSE_BUTTON_SIZE`, crate-private in egui_dock 0.14) and the size of its ✕.
const TAB_CLOSE_W: f32 = 24.0;
const TAB_CLOSE_X: f32 = 9.0;

#[derive(Clone, Debug)]
pub struct ViewTag {
    pub rect: Rect,
    pub label: String,
    pub hovered: bool,
    pub dragged: bool,
}

/// A number that changes when the pane layout a person sees changes (v0.16.0 undo,
/// `app_undo.rs`): which panes are where, the tab shown in each place, the split shares and
/// where floating pane windows are. Not the focus, tab-bar scroll or the rects egui_dock works
/// out every frame (those change with the window size). No allocation, so it can run every frame.
pub fn shape(dock: &DockState<Pane>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let tree = |t: &Tree<Pane>, h: &mut std::collections::hash_map::DefaultHasher| {
        // egui_dock may retain unused trailing slots after rebuilding a split. Those slots
        // do not change the visible tree and must not manufacture a new undo action.
        let used = t.iter().enumerate().filter(|(_, n)| !matches!(n, Node::Empty)).map(|(i, _)| i + 1).last().unwrap_or(0);
        for (index, n) in t.iter().take(used).enumerate() {
            match n {
                Node::Empty => 0u8.hash(h),
                Node::Leaf { tabs, active, .. } => {
                    1u8.hash(h);
                    tabs.hash(h);
                    active.0.hash(h);
                }
                Node::Vertical { fraction, .. } => {
                    2u8.hash(h);
                    // The strip has a fixed height. Its fraction changes on a window resize,
                    // which must not create an undo action.
                    let left = NodeIndex(index).left();
                    let fixed_strip = left.0 < t.len() && t[left].tabs().is_some_and(|tabs| tabs.contains(&Pane::LayoutTabs));
                    if !fixed_strip { fraction.to_bits().hash(h); }
                }
                Node::Horizontal { fraction, .. } => {
                    3u8.hash(h);
                    fraction.to_bits().hash(h);
                }
            }
        }
    };
    for s in dock.iter_surfaces() {
        match s {
            egui_dock::Surface::Empty => 0u8.hash(&mut h),
            egui_dock::Surface::Main(t) => {
                1u8.hash(&mut h);
                tree(t, &mut h);
            }
            egui_dock::Surface::Window(t, w) => {
                2u8.hash(&mut h);
                let r = w.rect();
                for v in [r.min.x, r.min.y, r.max.x, r.max.y] {
                    v.to_bits().hash(&mut h);
                }
                tree(t, &mut h);
            }
        }
    }
    h.finish()
}

/// After a layout was put back (undo): floating pane windows go back to where they were
/// (egui keeps a window's place in its own memory, so it is set again explicitly).
pub fn place_windows(dock: &mut DockState<Pane>) {
    for s in dock.iter_surfaces_mut() {
        if let egui_dock::Surface::Window(_, w) = s {
            let r = w.rect();
            if r.is_finite() && r.is_positive() {
                w.set_position(r.min);
                w.set_size(r.size());
            }
        }
    }
}

/// 3D views alone in their place in the main window (no other pane tabbed with them): these
/// get a name tag instead of a tab row.
pub fn lone_views(dock: &DockState<Pane>) -> Vec<u8> {
    dock.main_surface()
        .iter()
        .filter_map(|node| match node.tabs() {
            Some([Pane::View(k)]) => Some(*k),
            _ => None,
        })
        .collect()
}

/// Paint a view's name tag over its 3D: dark, or blue on the chosen view when there are
/// several. The name and ✕ sit where egui_dock has them, so the tag works like the tab.
pub fn paint_view_tag(ui: &egui::Ui, painter: &egui::Painter, tag: &ViewTag, chosen: bool, closeable: bool) {
    let r = tag.rect;
    let close_w = if closeable { TAB_CLOSE_W.min(TAB_H) } else { 0.0 };
    let fill = if chosen {
        theme::ACTIVE_VIEW
    } else if tag.hovered {
        Color32::from_black_alpha(210)
    } else {
        Color32::from_black_alpha(160)
    };
    painter.rect_filled(r.shrink2(egui::vec2(2.0, 4.0)), 4.0, fill);
    let text_rect = Rect::from_min_max(r.min, pos2(r.right() - close_w, r.bottom()));
    let color = if chosen { theme::TAB_TEXT_ACTIVE } else { theme::TEXT };
    painter.text(text_rect.center(), egui::Align2::CENTER_CENTER, &tag.label, egui::TextStyle::Button.resolve(ui.style()), color);
    if closeable {
        let c = Rect::from_center_size(pos2((text_rect.right() + r.right()) / 2.0, r.center().y), egui::Vec2::splat(close_w));
        let hot = ui.ctx().pointer_hover_pos().is_some_and(|p| c.contains(p));
        if hot {
            painter.rect_filled(c.shrink(4.0), 3.0, Color32::from_white_alpha(28));
        }
        let x = Rect::from_center_size(c.center(), egui::Vec2::splat(TAB_CLOSE_X));
        let stroke = Stroke::new(1.0_f32, if hot || chosen { theme::TAB_TEXT_ACTIVE } else { theme::TAB_TEXT_DIM });
        painter.line_segment([x.left_top(), x.right_bottom()], stroke);
        painter.line_segment([x.right_top(), x.left_bottom()], stroke);
    }
}

/// Where a file dropped on a 3D view goes, from the pointer's place in the view (the same
/// split as egui_dock's drop preview for tabs): the middle half = into that view, else the
/// side whose edge is nearest along the diagonals (VS Code editor drop).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropZone {
    Center,
    Left,
    Right,
    Above,
    Below,
}

pub fn drop_zone(rect: Rect, p: Pos2) -> DropZone {
    let a = egui::vec2((p.x - rect.left()) / rect.width().max(1.0) - 0.5, (p.y - rect.top()) / rect.height().max(1.0) - 0.5);
    if a.x.abs() < 0.25 && a.y.abs() < 0.25 {
        return DropZone::Center;
    }
    match (a.x - a.y > 0.0, -a.x - a.y > 0.0) {
        (true, true) => DropZone::Above,
        (false, true) => DropZone::Left,
        (true, false) => DropZone::Right,
        (false, false) => DropZone::Below,
    }
}

/// The part of a view's place a dropped file would fill (shown while dragging).
pub fn drop_preview(rect: Rect, zone: DropZone) -> Rect {
    let c = rect.center();
    match zone {
        DropZone::Center => rect,
        DropZone::Left => Rect::from_min_max(rect.min, pos2(c.x, rect.bottom())),
        DropZone::Right => Rect::from_min_max(pos2(c.x, rect.top()), rect.max),
        DropZone::Above => Rect::from_min_max(rect.min, pos2(rect.right(), c.y)),
        DropZone::Below => Rect::from_min_max(pos2(rect.left(), c.y), rect.max),
    }
}

/// Put 3D view `slot` beside view `target`, in half of its place on the `zone` side. The new
/// view gets the focus. False (nothing changed) for `Center`, a missing target or a slot
/// already shown.
pub fn add_view_at(dock: &mut DockState<Pane>, slot: u8, target: u8, zone: DropZone) -> bool {
    if dock.find_tab(&Pane::View(slot)).is_some() {
        return false;
    }
    let Some((node, _)) = dock.main_surface().find_tab(&Pane::View(target)) else { return false };
    let s = dock.main_surface_mut();
    let new = vec![Pane::View(slot)];
    let [_, added] = match zone {
        DropZone::Left => s.split_left(node, 0.5, new),
        DropZone::Right => s.split_right(node, 0.5, new),
        DropZone::Above => s.split_above(node, 0.5, new),
        DropZone::Below => s.split_below(node, 0.5, new),
        DropZone::Center => return false,
    };
    s.set_focused_node(added);
    true
}

/// One edge drop: incoming views share that half evenly, perpendicular to the edge.
pub fn add_views_at(dock: &mut DockState<Pane>, slots: &[u8], target: u8, zone: DropZone) -> bool {
    let Some(&first) = slots.first() else { return false };
    if zone == DropZone::Center || slots.len() + views(dock).len() > MAX_VIEWS
        || slots.iter().enumerate().any(|(i, k)| usize::from(*k) >= MAX_VIEWS
            || slots[..i].contains(k) || dock.find_tab(&Pane::View(*k)).is_some()) { return false; }
    if !add_view_at(dock, first, target, zone) { return false; }
    let mut last = first;
    for (i, &slot) in slots.iter().enumerate().skip(1) {
        let (node, _) = dock.main_surface().find_tab(&Pane::View(last)).expect("just added view");
        let fraction = 1.0 / (slots.len() - i + 1) as f32;
        let tree = dock.main_surface_mut();
        let [_, added] = if matches!(zone, DropZone::Left | DropZone::Right) {
            tree.split_below(node, fraction, vec![Pane::View(slot)])
        } else {
            tree.split_right(node, fraction, vec![Pane::View(slot)])
        };
        tree.set_focused_node(added);
        last = slot;
    }
    true
}

pub fn style(egui_style: &egui::Style) -> Style {
    let mut s = Style::from_egui(egui_style);
    s.main_surface_border_stroke = Stroke::NONE;
    s.tab_bar.bg_fill = theme::TAB_BAR_BG;
    s.tab_bar.height = TAB_H;
    s.tab_bar.hline_color = theme::TAB_BAR_BG;
    s.tab_bar.rounding = Rounding::ZERO;
    s.tab = panel_tab_style(&s.tab);
    s.tab.tab_body.stroke = Stroke::NONE;
    s.tab.tab_body.rounding = Rounding::ZERO;
    s.buttons.close_tab_color = theme::TAB_TEXT_DIM;
    s.buttons.close_tab_active_color = theme::TAB_TEXT_ACTIVE;
    s.separator.color_idle = theme::GRAPH_SEP;
    s.separator.color_hovered = theme::ACTIVE_VIEW;
    s.separator.color_dragged = theme::ACTIVE_VIEW;
    s.overlay.overlay_type = OverlayType::HighlightedAreas;
    s.overlay.selection_color = theme::DROP_FILL;
    s
}

/// Where the 3D views are, as rectangles inside `area` (layout tests).
#[cfg(test)]
pub(crate) fn view_rects(dock: &DockState<Pane>, area: Rect) -> Vec<(u8, Rect)> {
    let s = dock.main_surface();
    let mut out: Vec<(u8, Rect)> = leaf_rects(s, area)
        .into_iter()
        .filter_map(|(n, r)| {
            s[n].tabs()?.iter().find_map(|t| match t {
                Pane::View(k) => Some((*k, r)),
                _ => None,
            })
        })
        .collect();
    out.sort_by_key(|(k, _)| *k);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_layout_round_trip_keeps_split_and_the_surrounding_panels() {
        let original = default_layout_for(&[0, 1, 2]);
        let layout = editor_layout(&original);
        let mut another = default_layout_for(&[3]);
        show(&mut another, Pane::Graph);
        set_editor_layout(&mut another, &layout);
        assert_eq!(editor_layout(&another), layout);
        assert_eq!(views(&another), vec![0, 1, 2]);
        for pane in [Pane::Explorer, Pane::Labeling, Pane::EditTools, Pane::Selected, Pane::Devices, Pane::Graph] {
            assert!(has(&another, pane), "panel {pane:?} must remain");
        }
        set_editor_layout(&mut another, &EditorLayout::Empty);
        assert!(views(&another).is_empty());
        assert!(has(&another, Pane::Empty));
        assert!(has(&another, Pane::Graph));
    }

    #[test]
    fn editor_layout_keeps_views_that_share_a_leaf() {
        let mut original = default_layout_for(&[0]);
        let (node, _) = original.main_surface().find_tab(&Pane::View(0)).unwrap();
        if let Node::Leaf { tabs, active, .. } = &mut original.main_surface_mut()[node] {
            tabs.push(Pane::View(2));
            *active = TabIndex(1);
        }
        let layout = editor_layout(&original);
        let mut another = default_layout_for(&[1]);
        set_editor_layout(&mut another, &layout);
        assert_eq!(editor_layout(&another), layout);
        assert_eq!(views(&another), vec![0, 2]);
    }

    fn order(p: &Pane) -> usize {
        match p {
            Pane::View(k) => usize::from(*k),
            other => 10 + Pane::PANELS.iter().position(|q| q == other).unwrap_or(9),
        }
    }

    fn tabs(d: &DockState<Pane>) -> Vec<Pane> {
        let mut v: Vec<Pane> = d.iter_all_tabs().map(|(_, t)| *t).collect();
        v.sort_by_key(order);
        v
    }

    /// Panes of the first arrangement with these views: every pane but the graph pane (the
    /// graph floats over the 3D views) and the COMMENTS pane (v0.17.0: opened from the menu or
    /// the status bar, so the first arrangement and `LAYOUT_VERSION` stay).
    fn all(views: &[u8]) -> Vec<Pane> {
        views.iter().map(|&k| Pane::View(k)).chain(Pane::PANELS).filter(|p| !matches!(p, Pane::Graph | Pane::Comments | Pane::Labels)).collect()
    }

    #[test]
    fn the_comments_pane_opens_as_a_tab_next_to_the_selected_marker_and_reads_back() {
        let mut d = default_layout();
        assert!(!has(&d, Pane::Comments), "not in the first arrangement");
        show(&mut d, Pane::Comments);
        let s = d.main_surface();
        let (node, _) = s.find_tab(&Pane::Selected).unwrap();
        assert_eq!(s[node].tabs(), Some([Pane::Selected, Pane::Devices, Pane::Comments].as_slice()), "a tab of the selected marker's place, in front");
        assert!(matches!(&s[node], Node::Leaf { active, .. } if active.0 == 2), "shown in front");
        let text = ron::to_string(&d).unwrap();
        let back: DockState<Pane> = ron::from_str(&text).unwrap();
        assert!(has(&back, Pane::Comments) && tabs(&back) == tabs(&d));
        // Toggling the right side hides it with its place and brings it back.
        let mut hidden = Hidden::default();
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Right);
        assert!(!has(&d, Pane::Comments));
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Right);
        assert!(has(&d, Pane::Comments));
    }

    /// Rectangle of the place a pane is in.
    fn rect_of(d: &DockState<Pane>, pane: Pane) -> Rect {
        let s = d.main_surface();
        let (node, _) = s.find_tab(&pane).expect("pane is in the main window");
        leaf_rects(s, NOMINAL_AREA).into_iter().find(|(n, _)| *n == node).unwrap().1
    }

    #[test]
    fn a_dropped_file_goes_into_the_middle_or_beside_on_that_side() {
        let r = Rect::from_min_size(pos2(100.0, 50.0), egui::vec2(400.0, 300.0));
        let at = |fx: f32, fy: f32| drop_zone(r, r.lerp_inside(egui::vec2(fx, fy)));
        assert_eq!(at(0.5, 0.5), DropZone::Center);
        assert_eq!(at(0.3, 0.7), DropZone::Center);
        assert_eq!(at(0.05, 0.5), DropZone::Left);
        assert_eq!(at(0.1, 0.2), DropZone::Left, "corner: the nearer edge along the diagonal");
        assert_eq!(at(0.95, 0.4), DropZone::Right);
        assert_eq!(at(0.5, 0.05), DropZone::Above);
        assert_eq!(at(0.6, 0.97), DropZone::Below);
        assert_eq!(drop_preview(r, DropZone::Center), r);
        assert_eq!(drop_preview(r, DropZone::Left), Rect::from_min_max(r.min, pos2(300.0, 350.0)));
        assert_eq!(drop_preview(r, DropZone::Below), Rect::from_min_max(pos2(100.0, 200.0), r.max));
    }

    #[test]
    fn a_view_dropped_beside_another_takes_half_of_its_place_on_that_side() {
        for zone in [DropZone::Left, DropZone::Right, DropZone::Above, DropZone::Below] {
            let mut d = default_layout_for(&[0]);
            assert!(add_view_at(&mut d, 1, 0, zone));
            let v = view_rects(&d, NOMINAL_AREA);
            assert_eq!(v.len(), 2);
            let (r0, r1) = (v[0].1, v[1].1);
            let ok = match zone {
                DropZone::Left => r1.right() <= r0.left() + 0.5 && (r1.top() - r0.top()).abs() < 0.5,
                DropZone::Right => r1.left() >= r0.right() - 0.5 && (r1.top() - r0.top()).abs() < 0.5,
                DropZone::Above => r1.bottom() <= r0.top() + 0.5 && (r1.left() - r0.left()).abs() < 0.5,
                DropZone::Below => r1.top() >= r0.bottom() - 0.5 && (r1.left() - r0.left()).abs() < 0.5,
                DropZone::Center => false,
            };
            assert!(ok, "{zone:?}: {r0:?} {r1:?}");
            assert!((r0.area() - r1.area()).abs() < 2.0, "{zone:?}: halves");
            assert_eq!(d.find_active_focused().map(|(_, t)| *t), Some(Pane::View(1)));
            assert!(!add_view_at(&mut d, 1, 0, DropZone::Left), "a view already shown is not added again");
        }
        let mut d = default_layout_for(&[0]);
        assert!(!add_view_at(&mut d, 1, 0, DropZone::Center));
        assert!(!add_view_at(&mut d, 1, 3, DropZone::Left), "no such view");
        assert_eq!(views(&d), vec![0]);
    }

    #[test]
    fn only_a_view_alone_in_its_place_gets_a_name_tag() {
        let mut views = lone_views(&default_layout_for(&[0, 1, 2]));
        views.sort();
        assert_eq!(views, vec![0, 1, 2], "every view of the first arrangement is alone");
        assert_eq!(lone_views(&scene_only(3)), vec![3]);
        // A pane dragged into a view's place: that place keeps its tab row.
        let shared = DockState::new(vec![Pane::View(0), Pane::Labels]);
        assert!(lone_views(&shared).is_empty());
    }

    #[test]
    fn default_layout_has_every_pane_but_the_graph_once_and_3d_in_main_window() {
        let mut d = default_layout();
        assert_eq!(tabs(&d), all(&[0]));
        assert!(!has(&d, Pane::Graph), "the graph floats over the 3D views");
        assert!(d.find_main_surface_tab(&Pane::View(0)).is_some());
        assert_eq!(d.find_active_focused().map(|(_, t)| *t), Some(Pane::View(0)));
    }

    #[test]
    fn default_layout_is_the_vs_code_arrangement() {
        let d = default_layout();
        let view = rect_of(&d, Pane::View(0));
        let (explorer, labels, selected) = (rect_of(&d, Pane::Explorer), rect_of(&d, Pane::Labeling), rect_of(&d, Pane::Selected));
        assert!(explorer.right() <= view.left() + 0.5 && explorer.height() == NOMINAL_AREA.height(), "Explorer: left, full height");
        assert!(labels.left() >= view.right() - 0.5 && selected.top() >= labels.bottom() - 0.5, "Labels right, selected marker below it");
        assert_eq!(labels, rect_of(&d, Pane::EditTools), "the two editing tabs share a place");
        assert_eq!(selected, rect_of(&d, Pane::Devices), "selected marker and devices are tabs of one place");
        let s = d.main_surface();
        let (node, _) = s.find_tab(&Pane::Selected).unwrap();
        assert!(matches!(&s[node], Node::Leaf { active, .. } if active.0 == 0), "the selected marker is the shown tab");
        assert_eq!(view.height(), NOMINAL_AREA.height(), "nothing below the 3D view: it is as high as the window");
        assert!(side_open(&d, NOMINAL_AREA, Side::Left) && side_open(&d, NOMINAL_AREA, Side::Right) && !side_open(&d, NOMINAL_AREA, Side::Bottom));
        // The graph pane, when the user sends the graph back to it: below the views only.
        let mut d = d;
        show(&mut d, Pane::Graph);
        let (view, graph) = (rect_of(&d, Pane::View(0)), rect_of(&d, Pane::Graph));
        assert!(graph.top() >= view.bottom() - 0.5 && graph.left() == view.left() && graph.right() == view.right(), "panel below the views only");
        assert!(side_open(&d, NOMINAL_AREA, Side::Bottom));
    }

    #[test]
    fn sanitize_repairs_missing_3d_and_duplicates() {
        let no_scene = DockState::new(vec![Pane::Labels]);
        assert_eq!(tabs(&sanitize(no_scene)), all(&[0]));
        let twice = DockState::new(vec![Pane::View(0), Pane::Labels, Pane::Labels]);
        assert_eq!(tabs(&sanitize(twice)), all(&[0]));
        let mut floating_scene = DockState::new(vec![Pane::Labels]);
        floating_scene.add_window(vec![Pane::View(0)]);
        assert_eq!(tabs(&sanitize(floating_scene)), all(&[0]));
        let too_big = DockState::new(vec![Pane::View(MAX_VIEWS as u8)]);
        assert_eq!(tabs(&sanitize(too_big)), all(&[0]));
        let small = DockState::new(vec![Pane::View(2), Pane::Labels]);
        assert_eq!(tabs(&sanitize(small)), vec![Pane::View(2), Pane::Labels], "a valid custom layout is kept");
    }

    #[test]
    fn hide_and_show_panes() {
        let mut d = default_layout();
        hide(&mut d, Pane::View(0));
        assert!(has(&d, Pane::View(0)), "3D view is not closed from the menu");
        for p in Pane::PANELS {
            hide(&mut d, p);
            assert!(!has(&d, p));
        }
        assert_eq!(tabs(&d), vec![Pane::View(0)]);
        show(&mut d, Pane::Labeling);
        show(&mut d, Pane::Devices);
        show(&mut d, Pane::Explorer);
        show(&mut d, Pane::Explorer);
        assert_eq!(tabs(&d), vec![Pane::View(0), Pane::Explorer, Pane::Labeling, Pane::Devices]);
        assert!(rect_of(&d, Pane::Devices).top() >= rect_of(&d, Pane::Labeling).bottom() - 0.5, "devices comes back on the right, below Labeling");
        show(&mut d, Pane::Graph);
        let view = rect_of(&d, Pane::View(0));
        assert!(rect_of(&d, Pane::Graph).top() >= view.bottom() - 0.5, "the graph pane comes back below the views");
        show(&mut d, Pane::Selected);
        assert_eq!(rect_of(&d, Pane::Selected), rect_of(&d, Pane::Devices), "next to devices, as a tab");
        assert!(rect_of(&d, Pane::Selected).top() >= rect_of(&d, Pane::Labeling).bottom() - 0.5);
    }

    /// Centers of the view rectangles, as (column, row) on a 2×2 grid of the 3D area.
    fn grid(d: &DockState<Pane>) -> Vec<(u8, u8, u8)> {
        let rects = view_rects(d, NOMINAL_AREA);
        let (l, r) = rects.iter().fold((f32::MAX, f32::MIN), |(l, r), (_, x)| (l.min(x.left()), r.max(x.right())));
        let (t, b) = rects.iter().fold((f32::MAX, f32::MIN), |(t, b), (_, x)| (t.min(x.top()), b.max(x.bottom())));
        let mid = pos2((l + r) / 2.0, (t + b) / 2.0);
        rects.iter().map(|(k, x)| (*k, u8::from(x.center().x > mid.x), u8::from(x.center().y > mid.y))).collect()
    }

    fn two_by_two(mut d: DockState<Pane>, area: Rect) {
        add_view(&mut d, 1, area);
        assert_eq!(views(&d), vec![0, 1]);
        let r = view_rects(&d, area);
        assert!((r[0].1.height() - r[1].1.height()).abs() < 1.0 && r[0].1.right() <= r[1].1.left() + 0.5, "side by side: {r:?}");
        add_view(&mut d, 2, area);
        add_view(&mut d, 3, area);
        let r = view_rects(&d, area);
        let areas: Vec<f32> = r.iter().map(|(_, r)| r.area()).collect();
        assert!(areas.iter().all(|a| (a - areas[0]).abs() < 1.0), "four equal views: {r:?}");
        assert!(r[2].1.top() >= r[0].1.bottom() - 0.5 && r[3].1.top() >= r[1].1.bottom() - 0.5, "2×2: {r:?}");
    }

    #[test]
    fn views_split_one_two_three_then_two_by_two() {
        let mut d = default_layout();
        add_view(&mut d, 1, NOMINAL_AREA);
        add_view(&mut d, 2, NOMINAL_AREA);
        add_view(&mut d, 3, NOMINAL_AREA);
        assert_eq!(grid(&d), vec![(0, 0, 0), (1, 1, 0), (2, 0, 1), (3, 1, 1)], "2×2");
        assert_eq!(free_slot(&d), None);
        assert_eq!(tabs(&d), all(&[0, 1, 2, 3]), "side panes stay");
        assert_eq!(d.find_active_focused().map(|(_, t)| *t), Some(Pane::View(3)), "new view has the focus");
        // The same on a full-HD window, with the graph panel below the views and without it,
        // and without any side pane (a wide, low view area).
        let wide = Rect::from_min_max(pos2(0.0, 24.0), pos2(1920.0, 920.0));
        two_by_two(default_layout(), NOMINAL_AREA);
        two_by_two(default_layout(), wide);
        let mut with_panel = default_layout();
        show(&mut with_panel, Pane::Graph);
        two_by_two(with_panel.clone(), NOMINAL_AREA);
        two_by_two(with_panel, wide);
        two_by_two(scene_only(0), wide);
    }

    #[test]
    fn closing_a_view_lets_the_neighbour_fill_and_the_last_leaves_an_empty_editor() {
        let mut d = default_layout();
        add_view(&mut d, 1, NOMINAL_AREA);
        let one = view_rects(&default_layout(), NOMINAL_AREA)[0].1;
        remove_view(&mut d, 0);
        assert_eq!(views(&d), vec![1]);
        let left = view_rects(&d, NOMINAL_AREA)[0].1;
        assert!((left.area() - one.area()).abs() < 1.0, "view 1 now fills the place of both");
        remove_view(&mut d, 1);
        assert!(views(&d).is_empty(), "last view closes");
        assert_eq!(free_slot(&d), Some(0));
        assert!(has(&d, Pane::Empty));
        assert_eq!(rect_of(&d, Pane::Empty), left, "the editor keeps its place");
        let saved = sanitize(d.clone());
        assert!(views(&saved).is_empty());
        assert!(has(&saved, Pane::Empty));
        add_view(&mut d, 2, NOMINAL_AREA);
        assert_eq!(tabs(&d), all(&[2]), "a fresh open uses the default layout");
    }

    #[test]
    fn default_layout_for_several_views_and_focus() {
        let mut d = default_layout_for(&[1, 3]);
        assert_eq!(views(&d), vec![1, 3]);
        assert_eq!(tabs(&d), all(&[1, 3]));
        assert_eq!(d.find_active_focused().map(|(_, t)| *t), Some(Pane::View(1)));
        focus_view(&mut d, 3);
        assert_eq!(d.find_active_focused().map(|(_, t)| *t), Some(Pane::View(3)));
    }

    fn close(a: Rect, b: Rect) -> bool {
        (a.min - b.min).length() < 1.0 && (a.max - b.max).length() < 1.0
    }

    #[test]
    fn toggles_hide_a_whole_side_and_bring_it_back_where_it_was() {
        let mut d = default_layout();
        show(&mut d, Pane::Graph);
        add_view(&mut d, 1, NOMINAL_AREA);
        let before: Vec<(Pane, Rect)> = tabs(&d).into_iter().map(|p| (p, rect_of(&d, p))).collect();
        let mut hidden = Hidden::default();
        for (side, gone) in [(Side::Left, vec![Pane::Explorer]), (Side::Bottom, vec![Pane::Graph]), (Side::Right, vec![Pane::Labeling, Pane::EditTools, Pane::Selected, Pane::Devices])] {
            toggle_side(&mut d, &mut hidden, NOMINAL_AREA, side);
            assert!(!side_open(&d, NOMINAL_AREA, side));
            assert!(gone.iter().all(|p| !has(&d, *p)), "{side:?} panes are off the screen");
            assert_eq!(hidden.panes().filter(|p| gone.contains(p)).count(), gone.len(), "and remembered");
            assert_eq!(views(&d), vec![0, 1], "3D views stay");
        }
        assert_eq!(tabs(&d), vec![Pane::View(0), Pane::View(1)]);
        let both = view_rects(&d, NOMINAL_AREA);
        assert!(close(both[0].1.union(both[1].1), NOMINAL_AREA), "the views fill the window");
        // Back in another order: every pane returns to its old rectangle.
        for side in [Side::Right, Side::Left, Side::Bottom] {
            toggle_side(&mut d, &mut hidden, NOMINAL_AREA, side);
            assert!(side_open(&d, NOMINAL_AREA, side));
        }
        assert_eq!(hidden, Hidden::default());
        for (p, r) in before {
            assert!(close(rect_of(&d, p), r), "{p:?}: {:?} != {r:?}", rect_of(&d, p));
        }
        let s = d.main_surface();
        let (node, _) = s.find_tab(&Pane::Selected).unwrap();
        assert_eq!(s[node].tabs(), Some([Pane::Selected, Pane::Devices].as_slice()), "tab order kept");
    }

    #[test]
    fn toggle_follows_dragged_panes_and_keeps_the_shown_tab() {
        // Labeling dragged to the left under Explorer; devices is the shown tab of its place.
        let mut d = default_layout();
        hide(&mut d, Pane::Labeling);
        let (node, _) = d.main_surface().find_tab(&Pane::Explorer).unwrap();
        d.main_surface_mut().split_below(node, 0.5, vec![Pane::Labeling]);
        let loc = d.find_tab(&Pane::Devices).unwrap();
        d.set_active_tab(loc);
        let mut hidden = Hidden::default();
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Left);
        assert!(!has(&d, Pane::Explorer) && !has(&d, Pane::Labeling), "the toggle takes what is on that side now");
        assert!(has(&d, Pane::Selected));
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Left);
        assert!(rect_of(&d, Pane::Labeling).top() >= rect_of(&d, Pane::Explorer).bottom() - 0.5);
        assert!(rect_of(&d, Pane::Labeling).right() <= rect_of(&d, Pane::View(0)).left() + 0.5);
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Right);
        assert!(!has(&d, Pane::Devices) && !has(&d, Pane::Selected));
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Right);
        let s = d.main_surface();
        let (node, _) = s.find_tab(&Pane::Devices).unwrap();
        assert!(matches!(&s[node], Node::Leaf { active, .. } if active.0 == 1), "devices is still the shown tab");
    }

    #[test]
    fn toggle_on_an_empty_side_brings_its_own_panes_and_leaves_windows_alone() {
        let mut d = default_layout();
        for p in Pane::PANELS {
            hide(&mut d, p);
        }
        d.add_window(vec![Pane::Devices]);
        let mut hidden = Hidden::default();
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Bottom);
        assert!(has(&d, Pane::Graph) && side_open(&d, NOMINAL_AREA, Side::Bottom));
        assert!(d.find_main_surface_tab(&Pane::Devices).is_none(), "the floating devices window is not moved");
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Bottom);
        assert!(!has(&d, Pane::Graph) && has(&d, Pane::Devices), "a toggle never hides a floating window");
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Right);
        assert!(has(&d, Pane::Labeling) && has(&d, Pane::Selected));
        assert!(rect_of(&d, Pane::Selected).top() >= rect_of(&d, Pane::Labeling).bottom() - 0.5, "Labeling on top, the selected marker below it");
        assert_eq!(rect_of(&d, Pane::Labeling), rect_of(&d, Pane::EditTools), "the toggle restores one pair of editing tabs");
        assert!(!has(&d, Pane::Labels), "side toggles never recreate the legacy panel");
        // A pane shown from the menu is no longer hidden; a hidden pane is never doubled.
        hidden.forget(Pane::Graph);
        assert_eq!(hidden, Hidden::default());
        let mut twice = Hidden { left: Some(Stash { share: 0.2, leaves: vec![StashLeaf { tabs: vec![Pane::Labeling, Pane::Explorer, Pane::View(1)], active: 2, size: 1.0 }] }), ..Hidden::default() };
        twice.bottom = Some(Stash { share: 0.2, leaves: vec![StashLeaf { tabs: vec![Pane::Explorer], active: 0, size: 1.0 }] });
        twice.clean(&d);
        assert_eq!(twice.panes().collect::<Vec<_>>(), vec![Pane::Explorer], "on screen (Labels), 3D view and the second copy are dropped");
        assert!(twice.bottom.is_none());
    }

    /// In-memory stand-in for eframe's settings file (same RON encoding as the app).
    #[derive(Default)]
    pub(crate) struct MemStorage(pub HashMap<String, String>);

    impl eframe::Storage for MemStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn layout_survives_save_and_load() {
        let mut d = default_layout();
        add_view(&mut d, 2, NOMINAL_AREA);
        hide(&mut d, Pane::Devices);
        d.add_window(vec![Pane::Devices]);
        let mut st = MemStorage::default();
        eframe::set_value(&mut st, "layout", &d);
        let back: DockState<Pane> = eframe::get_value(&st, "layout").expect("layout reads back");
        assert_eq!(tabs(&back), all(&[0, 2]));
        assert!(back.find_main_surface_tab(&Pane::Devices).is_none(), "floating window kept");
        let mut st2 = MemStorage::default();
        eframe::set_value(&mut st2, "layout", &sanitize(back));
        assert_eq!(st2.0["layout"], st.0["layout"]);
    }

    #[test]
    fn hidden_panes_survive_save_and_load() {
        let mut d = default_layout();
        let mut hidden = Hidden::default();
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Right);
        let text = ron::to_string(&hidden).unwrap();
        let back: Hidden = ron::from_str(&text).unwrap();
        assert_eq!(back, hidden);
        let empty: Hidden = ron::from_str("()").unwrap();
        assert_eq!(empty, Hidden::default());
    }

    fn assert_single_labeling(dock: &DockState<Pane>, hidden: &Hidden) {
        let panes: Vec<Pane> = dock.iter_all_tabs().map(|(_, p)| *p).chain(hidden.panes()).collect();
        assert_eq!(panes.iter().filter(|p| **p == Pane::Labeling).count(), 1, "{panes:?}");
        assert!(!panes.contains(&Pane::Labels), "legacy panel is removed: {panes:?}");
    }

    #[test]
    fn legacy_label_panel_is_renamed_in_its_docked_or_floating_place() {
        for floating in [false, true] {
            let mut d = DockState::new(vec![Pane::View(2)]);
            if floating {
                let window = d.add_window(vec![Pane::Labels, Pane::Devices]);
                d.get_window_state_mut(window).unwrap().set_position(pos2(92.0, 84.0)).set_size(egui::vec2(330.0, 510.0));
                d.set_active_tab((window, NodeIndex::root(), TabIndex(1)));
            } else {
                d.main_surface_mut().split_left(NodeIndex::root(), 0.28, vec![Pane::Labels, Pane::Devices]);
            }
            let saved = ron::to_string(&d).unwrap();
            let expected = saved.replace("Labels", "Labeling");
            d = ron::from_str(&saved).unwrap();
            let mut hidden = Hidden::default();
            merge_label_panels(&mut d, &mut hidden);
            assert_single_labeling(&d, &hidden);
            assert_eq!(ron::to_string(&d).unwrap(), expected, "only the pane name changes, floating={floating}");
            merge_label_panels(&mut d, &mut hidden);
            assert_eq!(ron::to_string(&d).unwrap(), expected, "migration is idempotent");
        }
    }

    #[test]
    fn existing_floating_labeling_wins_over_legacy_and_duplicate_panels() {
        let mut d = DockState::new(vec![Pane::View(1), Pane::Labels, Pane::Graph]);
        let retained = d.add_window(vec![Pane::Labeling, Pane::EditTools]);
        d.get_window_state_mut(retained).unwrap().set_position(pos2(80.0, 90.0)).set_size(egui::vec2(320.0, 490.0));
        let window = ron::to_string(&d[retained]).unwrap();
        let place = ron::to_string(d.get_window_state(retained).unwrap()).unwrap();
        d.add_window(vec![Pane::Labeling]);
        let mut hidden = Hidden {
            left: Some(Stash { share: 0.2, leaves: vec![StashLeaf { tabs: vec![Pane::Labels, Pane::Labeling, Pane::Comments], active: 2, size: 400.0 }] }),
            ..Hidden::default()
        };
        merge_label_panels(&mut d, &mut hidden);
        let d = sanitize(d);
        assert_single_labeling(&d, &hidden);
        assert_eq!(views(&d), vec![1], "duplicates do not reset the custom layout");
        assert_eq!(d.find_tab(&Pane::Labeling).unwrap().0, retained);
        assert_eq!(ron::to_string(&d[retained]).unwrap(), window, "the editing tabs stay together");
        let mut d = d;
        assert_eq!(ron::to_string(d.get_window_state(retained).unwrap()).unwrap(), place, "floating position and size stay");
        assert!(has(&d, Pane::Graph));
        assert_eq!(hidden.left.as_ref().unwrap().leaves[0].tabs, vec![Pane::Comments]);
    }

    #[test]
    fn hidden_labeling_wins_over_a_visible_legacy_panel_and_keeps_active_tabs() {
        let mut d = DockState::new(vec![Pane::View(3), Pane::Labels, Pane::Devices]);
        let stash = Stash { share: 0.34, leaves: vec![StashLeaf { tabs: vec![Pane::Labels, Pane::Selected, Pane::Labeling, Pane::Comments], active: 1, size: 360.0 }] };
        let mut hidden = Hidden { left: Some(stash), ..Hidden::default() };
        merge_label_panels(&mut d, &mut hidden);
        hidden.clean(&d);
        assert_single_labeling(&d, &hidden);
        assert!(!has(&d, Pane::Labeling), "the existing panel stays hidden on its own side");
        let stash = hidden.left.as_ref().unwrap();
        assert_eq!(stash.share, 0.34);
        assert_eq!(stash.leaves[0], StashLeaf { tabs: vec![Pane::Selected, Pane::Labeling, Pane::Comments], active: 0, size: 360.0 });
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Left);
        assert_single_labeling(&d, &hidden);
        let (surface, node, _) = d.find_tab(&Pane::Labeling).unwrap();
        assert!(matches!(&d[surface][node], Node::Leaf { tabs, active, .. } if tabs[active.0] == Pane::Selected));
        assert!(rect_of(&d, Pane::Labeling).right() <= rect_of(&d, Pane::View(3)).left() + 0.5);
    }

    #[test]
    fn legacy_hidden_label_panel_keeps_its_side_and_version_two_adds_tools_once() {
        let mut d = scene_only(2);
        let stash = Stash { share: 0.29, leaves: vec![StashLeaf { tabs: vec![Pane::Labels, Pane::Devices], active: 1, size: 480.0 }] };
        let mut hidden = Hidden { right: Some(stash), ..Hidden::default() };
        hidden = ron::from_str(&ron::to_string(&hidden).unwrap()).unwrap();
        migrate_labeling(&mut d, &mut hidden);
        assert_single_labeling(&d, &hidden);
        assert_eq!(hidden.right.as_ref().unwrap().leaves[0], StashLeaf { tabs: vec![Pane::Labeling, Pane::EditTools, Pane::Devices], active: 2, size: 480.0 });
        let before = hidden.clone();
        migrate_labeling(&mut d, &mut hidden);
        assert_eq!(hidden, before, "a repeated migration does not add another Edit Tools tab");
        toggle_side(&mut d, &mut hidden, NOMINAL_AREA, Side::Right);
        assert_single_labeling(&d, &hidden);
        let (surface, node, _) = d.find_tab(&Pane::Labeling).unwrap();
        assert!(matches!(&d[surface][node], Node::Leaf { tabs, active, .. } if tabs[active.0] == Pane::Devices));
    }

    #[test]
    fn marker_panel_merge_keeps_closed_panels_closed_and_panels_list_has_one_name() {
        let mut d = scene_only(1);
        let mut hidden = Hidden::default();
        let before = ron::to_string(&d).unwrap();
        merge_label_panels(&mut d, &mut hidden);
        assert_eq!(ron::to_string(&d).unwrap(), before);
        assert_eq!(hidden, Hidden::default());
        assert!(!Pane::PANELS.contains(&Pane::Labels));
        assert_eq!(Pane::PANELS.iter().filter(|p| p.title() == "Labeling").count(), 1);
    }

    #[test]
    fn layout_saved_before_several_views_reads_its_3d_view_as_view_0() {
        let mut st = MemStorage::default();
        eframe::set_value(&mut st, "layout", &default_layout());
        let old = st.0["layout"].replace("View(0)", "Scene");
        assert!(old.contains("Scene") && !old.contains("View("), "old text: {old}");
        st.0.insert("layout".into(), old);
        let back: DockState<Pane> = eframe::get_value(&st, "layout").expect("old layout still reads");
        assert_eq!(tabs(&sanitize(back)), all(&[0]));
    }

    #[test]
    fn layout_saved_before_the_graph_pane_still_reads() {
        // v0.6.0 layout text: no Graph pane anywhere.
        let mut d = default_layout();
        hide(&mut d, Pane::Graph);
        let mut st = MemStorage::default();
        eframe::set_value(&mut st, "layout", &d);
        assert!(!st.0["layout"].contains("Graph"));
        let back: DockState<Pane> = eframe::get_value(&st, "layout").expect("v0.6.0 layout still reads");
        assert!(!has(&sanitize(back), Pane::Graph));
    }

    #[test]
    fn views_are_numbered_in_reading_order_not_by_slot() {
        // View 1 dropped left of view 0, view 2 below view 0, view 3 tabbed with view 2.
        let mut d = default_layout();
        assert!(add_view_at(&mut d, 1, 0, DropZone::Left));
        assert!(add_view_at(&mut d, 2, 0, DropZone::Below));
        let (node, _) = d.main_surface().find_tab(&Pane::View(2)).unwrap();
        d.main_surface_mut()[node].append_tab(Pane::View(3));
        assert_eq!(views(&d), vec![0, 1, 2, 3]);
        assert_eq!(views_reading_order(&d), vec![1, 0, 2, 3], "top row left to right, then the row below, tabs in tab order");
        assert_eq!(views_reading_order(&default_layout()), vec![0]);
    }
}
