
use super::dock;
use super::graph::{self, Drawn, Lane, Look};
use super::theme;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Id, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, Vec2};
use serde::{Deserialize, Serialize};

/// Background darkness in percent the user asked for as the first value.
pub const DEFAULT_OPACITY: u8 = 60;
/// Gap between the graph and the edge of the 3D view.
const MARGIN: f32 = 6.0;
const MIN_W: f32 = 150.0;
const TAG_H: f32 = 20.0;
/// The name tag sits this far inside the graph's top-left corner.
const TAG_PAD: f32 = 3.0;
const BTN_W: f32 = 18.0;
const BTN_H: f32 = 14.0;
const BTN_GAP: f32 = 3.0;
/// Width of the plate graph's "줄 ▼" button (lanes list).
const LANES_W: f32 = 30.0;
/// Half width of the strips along the edges that resize the graph.
const EDGE: f32 = 4.0;
const CORNER: f32 = 12.0;
/// Smallest height of one lane (marker: three of them).
const LANE_MIN_H: f32 = 16.0;
/// Room kept right of a tag of several markers (v0.27.0) for the first lane's max value text.
const VALUE_ROOM: f32 = 90.0;

/// A force plate graph's lanes, top to bottom: the plate device's analog channels 0..6. The
/// file names them only `AnalogChannel0` … `AnalogChannel5`; this order (forces, then moments)
/// was checked on real files (work log 2026-11-11, v0.12.0). Colors as the marker lanes: x red,
/// y green, z blue.
pub const PLATE_LANES: [(&str, Color32); 6] =
    [("Fx", theme::AXIS_X), ("Fy", theme::AXIS_Y), ("Fz", theme::AXIS_Z), ("Mx", theme::AXIS_X), ("My", theme::AXIS_Y), ("Mz", theme::AXIS_Z)];

/// Which of a force plate's lanes (`PLATE_LANES`) a graph shows or a plate offers: bit k =
/// channel k. Saved per view slot (`Settings::plate_channels`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LaneSet(pub u8);

impl Default for LaneSet {
    /// First choice: the forces Fx · Fy · Fz.
    fn default() -> Self {
        LaneSet(0b000_111)
    }
}

impl LaneSet {
    /// Lane `k` alone.
    pub fn only(k: usize) -> LaneSet {
        LaneSet(1 << k)
    }

    /// The first `n` lanes (a device with `n` channels; at most six).
    pub fn first(n: usize) -> LaneSet {
        LaneSet(((1u16 << n.min(PLATE_LANES.len())) - 1) as u8)
    }

    pub fn has(self, k: usize) -> bool {
        k < PLATE_LANES.len() && self.0 & (1 << k) != 0
    }

    pub fn len(self) -> usize {
        (self.0 & 0b111_111).count_ones() as usize
    }

    /// Lane numbers, top to bottom.
    pub fn iter(self) -> impl Iterator<Item = usize> {
        (0..PLATE_LANES.len()).filter(move |&k| self.has(k))
    }

    /// What a graph shows when these lanes are wanted and the plate offers `offer`: the wanted
    /// ones it has, else the first choice it has, else its first lane. Never none while it
    /// offers some.
    pub fn shown(self, offer: LaneSet) -> LaneSet {
        let first = LaneSet::default().0 & offer.0;
        match (self.0 & offer.0, first) {
            (0, 0) => LaneSet(offer.0 & offer.0.wrapping_neg()),
            (0, f) => LaneSet(f),
            (s, _) => LaneSet(s),
        }
    }
}

/// Which floating graph. What differs: the mark in front of the name (● / ■), the buttons,
/// the first place and the smallest height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The selected marker's X / Y / Z: first along the bottom of the 3D; buttons "back to
    /// the GRAPH pane" and fold.
    Marker,
    /// A chosen force plate's lanes `shown` (of those its device has, `offer`): first along
    /// the top, just below the tab-row strip (clear of the file name tag and "Perspective");
    /// buttons lanes ("줄": a list to choose them), fold and close (the devices pane keeps every
    /// plate, so there is no "back to the pane").
    Plate { shown: LaneSet, offer: LaneSet },
}

impl Kind {
    /// Smallest height: every lane still readable.
    fn min_h(self) -> f32 {
        match self {
            Kind::Marker => 3.0 * LANE_MIN_H + graph::NAV_H,
            Kind::Plate { shown, .. } => (shown.len() as f32 * LANE_MIN_H).max(24.0) + graph::NAV_H,
        }
    }

    /// First size: as wide as the view, about a quarter of it high (both kinds the same, the
    /// lanes share it).
    fn first_size(self, area: Rect) -> Vec2 {
        vec2(area.width() - 2.0 * MARGIN, (area.height() * 0.26).clamp(96.0, 190.0))
    }

    fn first_corner(self, area: Rect, size: Vec2) -> Pos2 {
        match self {
            Kind::Marker => pos2(area.left() + MARGIN, area.bottom() - MARGIN - size.y),
            Kind::Plate { .. } => pos2(area.left() + MARGIN, area.top() + dock::TAB_H),
        }
    }

    fn id(self, slot: usize) -> Id {
        match self {
            Kind::Marker => Id::new(("graph_float", slot)),
            Kind::Plate { .. } => Id::new(("plate_float", slot)),
        }
    }

    /// Widths of the buttons right of the name tag, left to right: marker = back to the pane ·
    /// fold; plate = lanes ("줄 ▼") · fold · ✕.
    fn button_ws(self) -> &'static [f32] {
        match self {
            Kind::Marker => &[BTN_W, BTN_W],
            Kind::Plate { .. } => &[LANES_W, BTN_W, BTN_W],
        }
    }
}

/// The buttons' rectangles right of the name tag's handle part `handle` (`Kind::button_ws`).
fn button_rects(kind: Kind, handle: Rect) -> Vec<Rect> {
    let y = handle.center().y - BTN_H / 2.0;
    let mut x = handle.right();
    kind.button_ws()
        .iter()
        .map(|&w| {
            let r = Rect::from_min_size(pos2(x, y), vec2(w, BTN_H));
            x += w + BTN_GAP;
            r
        })
        .collect()
}

/// Where a view's floating graph is. `None` = the first place / size of its kind (`Kind`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphBox {
    /// Top-left corner as a share of the 3D view's width and height (0..1), so the graph
    /// keeps its place when the view changes size.
    pub pos: Option<[f32; 2]>,
    /// Width and height in points.
    pub size: Option<[f32; 2]>,
    /// Folded: only the name tag is shown.
    pub collapsed: bool,
}

/// Edges a drag moves (resize).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    pub left: bool,
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
}

/// What the graph shows: its lanes and its name tag.
pub struct Graph<'a> {
    pub badge: Option<&'a str>,
    /// Name on the tag, its color, and the color of the mark (● / ■) in front of it.
    pub name: &'a str,
    pub name_color: Color32,
    pub mark: Color32,
    pub lanes: &'a [Lane],
    /// What the plot worked out for these lanes, kept between frames (`graph::Drawn`).
    pub drawn: &'a mut Drawn,
    /// Frames without a value: red marks in the frame bar.
    pub gaps: &'a [(i64, i64)],
    /// Id of the plot (its zoom is kept under it).
    pub plot_id: Id,
    pub first: i64,
    pub last: i64,
    pub frame: i64,
    pub selection: Option<(i64, i64)>,
    /// Comment bands behind the lanes (marker graph, step 6; empty for a plate graph).
    pub spans: &'a [graph::Span],
    /// v0.27.0, a marker graph of several markers: their names, shown in the name tag instead of
    /// `name` (`multi_tag`). Empty = one marker or a plate: the tag as before.
    pub keys: &'a [graph::Key<'a>],
    /// How many selected markers the graph leaves out (`panels::GRAPH_MAX`): the tag ends with
    /// "선택 N개 중 8개".
    pub left_out: usize,
}

/// The name tag of a graph of several markers (v0.27.0): the markers' names, as many as fit in
/// `max_w`, then "+N" for the others and, when the graph leaves selected markers out,
/// "선택 N개 중 8개". v0.29.0: the names only, `LASI · LKNE · RKNE` (no colour line: every curve is
/// in its lane's colour); which curve is which marker shows when the mouse is over a curve
/// (`graph::show`).
struct MultiTag {
    /// (x from the tag's left, the name) of each marker shown.
    names: Vec<(f32, std::sync::Arc<egui::Galley>)>,
    /// (x, "·") between two names.
    seps: Vec<(f32, std::sync::Arc<egui::Galley>)>,
    /// (x, text) of "+N" and of the left-out note.
    notes: Vec<(f32, std::sync::Arc<egui::Galley>)>,
    /// Width of the tag without its buttons.
    w: f32,
}

/// Gap on each side of the "·" between two names, and before a note.
const KEY_GAP: f32 = 5.0;
const KEY_SEP: f32 = 10.0;

fn multi_tag(painter: &egui::Painter, keys: &[graph::Key<'_>], left_out: usize, max_w: f32) -> MultiTag {
    let font = FontId::proportional(12.0);
    let total = keys.len() + left_out;
    let note = (left_out > 0).then(|| painter.layout_no_wrap(super::panels::left_out_text(keys.len(), total), font.clone(), theme::TEXT));
    let note_w = note.as_ref().map_or(0.0, |g| KEY_SEP + g.size().x);
    let plus_w = |n: usize| painter.layout_no_wrap(format!("+{n}"), font.clone(), theme::TEXT).size().x + KEY_SEP;
    let dot = painter.layout_no_wrap(super::panels::NAME_SEP.to_owned(), font.clone(), theme::TEXT_DIM);
    let sep_w = KEY_GAP + dot.size().x + KEY_GAP;
    let mut x = 8.0;
    let (mut names, mut seps) = (Vec::new(), Vec::new());
    for (k, key) in keys.iter().enumerate() {
        let galley = painter.layout_no_wrap(key.name.to_owned(), font.clone(), key.name_color);
        let before = if k > 0 { sep_w } else { 0.0 };
        let w = galley.size().x;
        let after = keys.len() - k - 1;
        let room = x + before + w + if after > 0 { plus_w(after) } else { 0.0 } + note_w + 8.0;
        // The first one always, so the tag is never empty.
        if k > 0 && room > max_w {
            break;
        }
        if k > 0 {
            seps.push((x + KEY_GAP, dot.clone()));
        }
        names.push((x + before, galley));
        x += before + w;
    }
    let mut notes = Vec::new();
    let hidden = keys.len() - names.len();
    if hidden > 0 {
        let g = painter.layout_no_wrap(format!("+{hidden}"), font.clone(), theme::TEXT);
        notes.push((x + KEY_SEP, g.clone()));
        x += KEY_SEP + g.size().x;
    }
    if let Some(g) = note {
        notes.push((x + KEY_SEP, g.clone()));
        x += KEY_SEP + g.size().x;
    }
    MultiTag { names, seps, notes, w: x + 8.0 }
}

/// What the user did on the floating graph this frame.
#[derive(Default)]
pub struct Out {
    /// Clicked / dragged inside the graph: go to this frame.
    pub goto: Option<i64>,
    /// Marker graph, "패널로" button: the graph goes back to the GRAPH pane.
    pub to_panel: bool,
    /// Plate graph, ✕ button: the plate is no longer chosen.
    pub close: bool,
    /// Plate graph, its "줄" list: the lanes to show from now on (one was checked or unchecked).
    pub lanes: Option<LaneSet>,
}

/// Size of the open graph inside `area`: the saved size, never smaller than the minimum and
/// never bigger than the 3D view.
fn size_in(kind: Kind, b: &GraphBox, area: Rect) -> Vec2 {
    let min = vec2(MIN_W, kind.min_h());
    let max = (area.size() - Vec2::splat(2.0 * MARGIN)).max(min);
    let s = match b.size {
        Some([w, h]) if w.is_finite() && h.is_finite() => vec2(w, h),
        _ => kind.first_size(area),
    };
    vec2(s.x.clamp(min.x, max.x), s.y.clamp(min.y, max.y))
}

/// Keep a box of `size` whose top-left corner is `p` inside `area`.
fn fit(p: Pos2, size: Vec2, area: Rect) -> Pos2 {
    let lo = area.min + Vec2::splat(MARGIN);
    let hi = (area.max - Vec2::splat(MARGIN) - size).max(lo);
    pos2(p.x.clamp(lo.x, hi.x), p.y.clamp(lo.y, hi.y))
}

/// The corner the box asks for, before it is kept inside the view.
fn wanted_corner(kind: Kind, b: &GraphBox, area: Rect, size: Vec2) -> Pos2 {
    match b.pos {
        Some([fx, fy]) if fx.is_finite() && fy.is_finite() => area.min + vec2(fx.clamp(0.0, 1.0) * area.width(), fy.clamp(0.0, 1.0) * area.height()),
        _ => kind.first_corner(area, size),
    }
}

fn tag_box(tag: Vec2) -> Vec2 {
    tag + Vec2::splat(2.0 * TAG_PAD)
}

/// Top-left of the graph's name tag (`tag` = its size with the buttons) for the corner `c`:
/// just inside the corner, or right below the view's own name tag `view_tag` when that would
/// hide it (so the graph can always be moved, also when it is parked under the view's tag).
fn tag_at(c: Pos2, tag: Vec2, view_tag: Rect) -> Pos2 {
    let at = c + Vec2::splat(TAG_PAD);
    if Rect::from_min_size(at, tag).intersect(view_tag).is_positive() {
        pos2(at.x, view_tag.bottom())
    } else {
        at
    }
}

/// The bigger part of the thin strip `r` (a resize edge) that the view's name tag leaves free.
fn clear_of(r: Rect, view_tag: Rect) -> Rect {
    let [a, b] = graph::around(r, view_tag);
    let size = |r: Rect| if r.is_positive() { r.area() } else { 0.0 };
    if size(b) > size(a) {
        b
    } else {
        a
    }
}

/// Top-left corner on screen: of the open graph, or of the folded tag's place (the tag itself
/// is `TAG_PAD` inside it, exactly where it is on the open graph).
fn corner(kind: Kind, b: &GraphBox, area: Rect, tag: Vec2) -> Pos2 {
    let size = size_in(kind, b, area);
    let want = wanted_corner(kind, b, area, size);
    fit(want, if b.collapsed { tag_box(tag) } else { size }, area)
}

/// Rectangle of the open graph inside the 3D view `area`.
pub fn open_rect(kind: Kind, b: &GraphBox, area: Rect) -> Rect {
    let size = size_in(kind, b, area);
    Rect::from_min_size(fit(wanted_corner(kind, b, area, size), size, area), size)
}

fn set_corner(b: &mut GraphBox, area: Rect, p: Pos2) {
    b.pos = Some([(p.x - area.left()) / area.width().max(1.0), (p.y - area.top()) / area.height().max(1.0)]);
}

/// Drag of the name tag: the graph's (or the folded tag's) top-left corner goes to `to`,
/// staying inside the 3D view.
pub fn move_to(kind: Kind, b: &mut GraphBox, area: Rect, tag: Vec2, to: Pos2) {
    let size = if b.collapsed { tag_box(tag) } else { size_in(kind, b, area) };
    set_corner(b, area, fit(to, size, area));
}

#[cfg(test)]
fn move_by(kind: Kind, b: &mut GraphBox, area: Rect, tag: Vec2, delta: Vec2) {
    move_to(kind, b, area, tag, corner(kind, b, area, tag) + delta);
}

/// Width of the name tag without its buttons, for a name this wide.
fn plain_tag_w(name_w: f32) -> f32 {
    8.0 + 8.0 + 5.0 + name_w + 8.0
}

/// Width of the name tag with its buttons.
fn full_tag_w(kind: Kind, name_w: f32) -> f32 {
    plain_tag_w(name_w) + kind.button_ws().iter().map(|w| w + BTN_GAP).sum::<f32>()
}

/// Where to press for a test: middle of the name tag and of its buttons (there while the
/// mouse is over the graph): marker = [tag, back to the pane, fold], plate = [tag, lanes "줄",
/// fold, ✕].
#[cfg(test)]
pub(crate) fn test_points(kind: Kind, b: &GraphBox, area: Rect, view_tag: Rect, name_w: f32) -> Vec<Pos2> {
    let plain = plain_tag_w(name_w);
    let tag = vec2(full_tag_w(kind, name_w), TAG_H);
    let tag_min = tag_at(corner(kind, b, area, tag), tag, view_tag);
    let handle = Rect::from_min_size(tag_min, vec2(plain, TAG_H));
    std::iter::once(handle.center()).chain(button_rects(kind, handle).iter().map(|r| r.center())).collect()
}

/// Drag of an edge or the corner: those edges follow, the others stay. Never smaller than the
/// minimum, never outside the 3D view.
pub fn resize(kind: Kind, b: &mut GraphBox, area: Rect, edges: Edges, delta: Vec2) {
    let mut r = open_rect(kind, b, area);
    let min_h = kind.min_h();
    let (lo, hi) = (area.min + Vec2::splat(MARGIN), area.max - Vec2::splat(MARGIN));
    if edges.left {
        r.min.x = (r.min.x + delta.x).min(r.max.x - MIN_W).max(lo.x);
    }
    if edges.right {
        r.max.x = (r.max.x + delta.x).max(r.min.x + MIN_W).min(hi.x.max(r.min.x + MIN_W));
    }
    if edges.top {
        r.min.y = (r.min.y + delta.y).min(r.max.y - min_h).max(lo.y);
    }
    if edges.bottom {
        r.max.y = (r.max.y + delta.y).max(r.min.y + min_h).min(hi.y.max(r.min.y + min_h));
    }
    b.size = Some([r.width(), r.height()]);
    set_corner(b, area, r.min);
}

/// Small square button next to the name tag. `icon` paints into the button's rectangle.
fn button(ui: &mut Ui, painter: &egui::Painter, rect: Rect, id: Id, tip: &str, icon: impl FnOnce(&egui::Painter, Rect, Color32)) -> bool {
    let resp = ui.interact(rect, id, Sense::click()).on_hover_text(tip);
    painter.rect_filled(rect, 3.0, Color32::from_white_alpha(if resp.hovered() { 70 } else { 36 }));
    icon(painter, rect, if resp.hovered() { theme::TAB_TEXT_ACTIVE } else { theme::TEXT });
    resp.clicked()
}

/// Window outline with its bottom strip filled: the GRAPH pane below the 3D views.
fn panel_icon(p: &egui::Painter, r: Rect, color: Color32) {
    let w = Rect::from_center_size(r.center(), vec2(10.0, 8.0));
    p.rect_stroke(w, 1.0, Stroke::new(1.0_f32, color));
    p.rect_filled(Rect::from_min_max(pos2(w.left() + 1.0, w.bottom() - 3.0), w.max - vec2(1.0, 1.0)), 0.0, color);
}

/// ▼ (fold) on the open graph, ▲ (unfold) on the folded tag.
fn fold_icon(p: &egui::Painter, r: Rect, color: Color32, open: bool) {
    let (c, s) = (r.center(), if open { 1.0 } else { -1.0 });
    p.add(Shape::convex_polygon(vec![c + vec2(-4.0, -2.5 * s), c + vec2(4.0, -2.5 * s), c + vec2(0.0, 3.0 * s)], color, Stroke::NONE));
}

/// ✕ painted as two lines (the font may not have it).
fn close_icon(p: &egui::Painter, r: Rect, color: Color32) {
    let x = Rect::from_center_size(r.center(), Vec2::splat(7.0));
    let stroke = Stroke::new(1.3_f32, color);
    p.line_segment([x.left_top(), x.right_bottom()], stroke);
    p.line_segment([x.right_top(), x.left_bottom()], stroke);
}

/// "줄" and a small ▼ (painted: the font may not have it): opens the list of lanes.
fn lanes_icon(p: &egui::Painter, r: Rect, color: Color32) {
    p.text(pos2(r.left() + 4.0, r.center().y), Align2::LEFT_CENTER, "줄", FontId::proportional(11.0), color);
    let c = pos2(r.right() - 7.0, r.center().y + 0.5);
    p.add(Shape::convex_polygon(vec![c + vec2(-3.0, -1.5), c + vec2(3.0, -1.5), c + vec2(0.0, 2.0)], color, Stroke::NONE));
}

/// The plate graph's list of lanes under its "줄" button: header "보일 줄", a check box per lane
/// the plate offers in its lane color, forces and moments apart. Returns the lanes to show when
/// one was checked or unchecked; the last one shown cannot be unchecked.
fn lanes_list(ui: &mut Ui, shown: LaneSet, offer: LaneSet) -> Option<LaneSet> {
    ui.set_min_width(96.0);
    ui.label(RichText::new("보일 줄").small().color(theme::TEXT_DIM));
    let mut out = None;
    for k in offer.iter() {
        if k == 3 && offer.has(0) {
            ui.separator();
        }
        let (name, color) = PLATE_LANES[k];
        let mut on = shown.has(k);
        let last = on && shown.len() == 1;
        let resp = ui.checkbox(&mut on, RichText::new(name).color(color).strong());
        let resp = if last { resp.on_hover_text("줄이 하나는 있어야 해요") } else { resp };
        if resp.clicked() && !last {
            out = Some(LaneSet(if on { shown.0 | 1 << k } else { shown.0 & !(1 << k) }));
        }
    }
    out
}

/// Draw a floating graph of `kind` over the 3D view `area` and take the mouse on it.
/// Call it after everything else of that view, so it is on top of the 3D for the mouse, with a
/// `ui` whose clip rect holds `area` (egui clips the mouse to it). `view_tag` = the view's own
/// name tag at the top-left of `area` (`Rect::NOTHING` when the view has a tab row): the
/// caller paints it over the graph, and the graph leaves the mouse to it there.
#[allow(clippy::too_many_arguments)]
pub fn show(ui: &mut Ui, area: Rect, view_tag: Rect, slot: usize, kind: Kind, b: &mut GraphBox, opacity: u8, g: Graph<'_>) -> Out {
    let mut out = Out::default();
    let id = kind.id(slot);
    let galley = ui.painter().layout_no_wrap(g.name.to_owned(), FontId::proportional(12.0), g.name_color);
    // Tag: pad, mark, gap, name, pad; with the mouse over the graph also the buttons.
    // v0.27.0, several markers: their names instead (`multi_tag`), as many as leave
    // room for the buttons and the first lane's max value text right of the tag.
    let buttons_w: f32 = kind.button_ws().iter().map(|w| w + BTN_GAP).sum();
    let multi = (!g.keys.is_empty()).then(|| multi_tag(ui.painter(), g.keys, g.left_out, size_in(kind, b, area).x - 2.0 * TAG_PAD - buttons_w - VALUE_ROOM));
    let badge=g.badge.map(|text|ui.painter().layout_no_wrap(text.to_owned(),FontId::proportional(10.5),Color32::WHITE));
    let badge_w=badge.as_ref().map_or(0.0,|g|g.size().x+12.0);
    let plain_w = badge_w + multi.as_ref().map_or_else(|| plain_tag_w(galley.size().x), |t| t.w);
    let full_w = plain_w + buttons_w;
    let tag_size = vec2(full_w, TAG_H);
    // The plate graph's lane list (an egui popup above everything, so the graph and the 3D
    // under it do not get its clicks); while it is open the buttons stay.
    let list_id = id.with("lanes");
    let list_open = matches!(kind, Kind::Plate { .. }) && ui.memory(|m| m.is_popup_open(list_id));
    let open = !b.collapsed;
    let top_left = corner(kind, b, area, tag_size);
    let rect = Rect::from_min_size(top_left, size_in(kind, b, area));
    let tag_min = tag_at(top_left, tag_size, view_tag);
    let handles: [(&str, Rect, Edges, CursorIcon); 5] = [
        ("left", Rect::from_min_max(pos2(rect.left() - EDGE, rect.top() + CORNER), pos2(rect.left() + EDGE - 1.0, rect.bottom() - CORNER)), Edges { left: true, ..Edges::default() }, CursorIcon::ResizeHorizontal),
        ("right", Rect::from_min_max(pos2(rect.right() - EDGE, rect.top() + CORNER), pos2(rect.right() + EDGE, rect.bottom() - CORNER)), Edges { right: true, ..Edges::default() }, CursorIcon::ResizeHorizontal),
        ("top", Rect::from_min_max(pos2(rect.left() + CORNER, rect.top() - EDGE), pos2(rect.right() - CORNER, rect.top() + EDGE - 1.0)), Edges { top: true, ..Edges::default() }, CursorIcon::ResizeVertical),
        ("bottom", Rect::from_min_max(pos2(rect.left() + CORNER, rect.bottom() - EDGE), pos2(rect.right() - CORNER, rect.bottom() + EDGE)), Edges { bottom: true, ..Edges::default() }, CursorIcon::ResizeVertical),
        ("corner", Rect::from_min_max(rect.max - Vec2::splat(CORNER), rect.max + Vec2::splat(EDGE)), Edges { right: true, bottom: true, ..Edges::default() }, CursorIcon::ResizeNwSe),
    ];
    // Buttons and the resize corner show while the mouse is over the graph (or drags it).
    let busy = ui.ctx().is_being_dragged(id.with("tag")) || handles.iter().any(|h| ui.ctx().is_being_dragged(id.with(h.0)));
    let hover_rect = if open { rect.expand(EDGE) } else { Rect::from_min_size(tag_min, tag_size) };
    let on_view_tag = ui.ctx().pointer_hover_pos().is_some_and(|p| view_tag.contains(p));
    let hot = busy || list_open || (!on_view_tag && ui.rect_contains_pointer(hover_rect));
    let clip = ui.clip_rect().intersect(area);
    let mut painter = ui.painter().clone();
    painter.set_clip_rect(clip);
    let edge = Stroke::new(1.0_f32, Color32::from_white_alpha(if hot { 120 } else { 60 }));

    if open {
        if matches!(kind, Kind::Plate { .. }) {
            crate::app::key_focus::region(ui, rect.intersect(clip), crate::app::key_focus::Target::Blocked);
        }
        let lane_h = (rect.height() - graph::NAV_H) / g.lanes.len().max(1) as f32;
        let alpha = (f32::from(opacity.min(100)) * 2.55).round() as u8;
        let look = Look {
            bg: Color32::from_rgba_unmultiplied(14, 14, 14, alpha),
            nav_bg: Color32::from_rgba_unmultiplied(0, 0, 0, alpha),
            watermark: Color32::from_white_alpha(22),
            // Lower lanes would put the first lane's min text under the name tag.
            values: lane_h >= 40.0 && rect.width() >= 260.0,
            tag_w: TAG_PAD + if hot { full_w } else { plain_w },
            keep_out: view_tag,
            selection: g.selection,
        };
        let Graph { lanes, drawn, gaps, plot_id, first, last, frame, spans, .. } = g;
        out.goto = ui
            .allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.set_clip_rect(clip);
                graph::show(ui, plot_id, lanes, first, last, frame, gaps, lane_h, &look, Some(drawn), spans)
            })
            .inner;
        painter.rect_stroke(rect, 2.0, edge);
    }

    // Name tag: the handle. The same tag on the open graph and when folded.
    let tag_rect = Rect::from_min_size(tag_min, vec2(if hot { full_w } else { plain_w }, TAG_H));
    crate::app::key_focus::region(ui, tag_rect, crate::app::key_focus::Target::Blocked);
    let handle = Rect::from_min_size(tag_min, vec2(plain_w, TAG_H));
    let tag = ui.interact(handle, id.with("tag"), Sense::click_and_drag()).on_hover_cursor(CursorIcon::Grab);
    // The corner keeps its distance to the mouse from where the tag was pressed.
    let grab_id = id.with("grab");
    if tag.drag_started() {
        let pressed = ui.input(|i| i.pointer.press_origin()).or(tag.interact_pointer_pos());
        ui.data_mut(|d| d.insert_temp(grab_id, pressed.map(|p| p - top_left)));
    }
    if tag.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        let grab: Option<Vec2> = ui.data(|d| d.get_temp::<Option<Vec2>>(grab_id)).flatten();
        if let (Some(p), Some(grab)) = (tag.interact_pointer_pos(), grab) {
            move_to(kind, b, area, tag_size, p - grab);
        }
    } else if tag.double_clicked() {
        b.collapsed = !b.collapsed;
    } else {
        tag.on_hover_text("끌어서 옮겨요 · 두 번 누르면 접거나 펴요");
    }
    painter.rect_filled(tag_rect, 3.0, Color32::from_black_alpha(if open { 150 } else { 190 }));
    if !open {
        painter.rect_stroke(tag_rect, 3.0, edge);
    }
    let mark_at = pos2(tag_min.x + 12.0, tag_rect.center().y);
    if let Some(t) = &multi {
        let y = tag_rect.center().y;
        for (x, name) in t.names.iter().chain(&t.seps) {
            painter.galley(pos2(tag_min.x + x, y - name.size().y / 2.0), name.clone(), theme::TEXT);
        }
        for (x, note) in &t.notes {
            painter.galley(pos2(tag_min.x + x, y - note.size().y / 2.0), note.clone(), theme::TEXT);
        }
    } else {
        match kind {
            Kind::Marker => {
                painter.circle_filled(mark_at, 4.0, g.mark);
            }
            Kind::Plate { .. } => {
                painter.rect_filled(Rect::from_center_size(mark_at, Vec2::splat(8.0)), 1.0, g.mark);
            }
        }
        painter.galley(pos2(tag_min.x + 21.0, tag_rect.center().y - galley.size().y / 2.0), galley, g.name_color);
    }
    if let Some(badge)=badge {
        let r=Rect::from_min_size(pos2(handle.right()-badge_w,tag_rect.top()+2.0),vec2(badge_w-3.0,TAG_H-4.0));
        painter.rect_filled(r,2.0,Color32::from_rgb(9,71,113));
        painter.galley(r.min+vec2(4.0,(r.height()-badge.size().y)/2.0),badge,Color32::WHITE);
    }
    if hot {
        let rects = button_rects(kind, handle);
        let fold_tip = if open { "접어요 (이름표만 남아요)" } else { "펴요" };
        let fold = |ui: &mut Ui, r: Rect| button(ui, &painter, r, id.with("fold"), fold_tip, |p, r, color| fold_icon(p, r, color, open));
        let folded = match kind {
            Kind::Marker => {
                out.to_panel = button(ui, &painter, rects[0], id.with("panel"), "아래 GRAPH 패널로 되돌려요", panel_icon);
                fold(ui, rects[1])
            }
            Kind::Plate { shown, offer } => {
                let resp = ui.interact(rects[0], id.with("lanes_button"), Sense::click()).on_hover_text("보일 줄을 골라요 (Fx · Fy · Fz · Mx · My · Mz)");
                let lit = resp.hovered() || list_open;
                painter.rect_filled(rects[0], 3.0, Color32::from_white_alpha(if lit { 70 } else { 36 }));
                lanes_icon(&painter, rects[0], if lit { theme::TAB_TEXT_ACTIVE } else { theme::TEXT });
                if resp.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(list_id));
                }
                let close_list = egui::popup::PopupCloseBehavior::CloseOnClickOutside;
                out.lanes = egui::popup::popup_below_widget(ui, list_id, &resp, close_list, |ui| lanes_list(ui, shown, offer)).flatten();
                if let Some(new) = out.lanes {
                    // The graph keeps its height and the lanes share it; only when it is lower
                    // than the new smallest height it grows to that.
                    let min = Kind::Plate { shown: new, offer }.min_h();
                    if let Some([w, _]) = b.size.filter(|s| s[1] < min) {
                        b.size = Some([w, min]);
                    }
                }
                let folded = fold(ui, rects[1]);
                out.close = button(ui, &painter, rects[2], id.with("close"), "닫아요 (장치 패널에는 그대로 있어요)", close_icon);
                folded
            }
        };
        if folded {
            b.collapsed = !b.collapsed;
        }
    }

    if open {
        for (name, r, edges, cursor) in handles {
            crate::app::key_focus::region(ui, clear_of(r, view_tag).intersect(clip), crate::app::key_focus::Target::Blocked);
            let resp = ui.interact(clear_of(r, view_tag), id.with(name), Sense::drag()).on_hover_cursor(cursor);
            if resp.dragged() {
                ui.ctx().set_cursor_icon(cursor);
                resize(kind, b, area, edges, resp.drag_delta());
            }
        }
        if hot {
            let c = rect.max - vec2(3.0, 3.0);
            let grip = Stroke::new(1.2_f32, Color32::from_white_alpha(170));
            painter.line_segment([c - vec2(8.0, 0.0), c - vec2(0.0, 8.0)], grip);
            painter.line_segment([c - vec2(4.0, 0.0), c - vec2(0.0, 4.0)], grip);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { min: pos2(300.0, 30.0), max: pos2(1200.0, 630.0) };
    const TAG: Vec2 = vec2(90.0, TAG_H);
    const M: Kind = Kind::Marker;

    fn inside(r: Rect, area: Rect) -> bool {
        r.left() >= area.left() + MARGIN - 0.01 && r.top() >= area.top() + MARGIN - 0.01 && r.right() <= area.right() - MARGIN + 0.01 && r.bottom() <= area.bottom() - MARGIN + 0.01
    }

    #[test]
    fn first_place_is_along_the_bottom_as_wide_as_the_view() {
        let b = GraphBox::default();
        let r = open_rect(M, &b, AREA);
        assert_eq!((r.left(), r.right(), r.bottom()), (AREA.left() + MARGIN, AREA.right() - MARGIN, AREA.bottom() - MARGIN));
        assert!(r.height() > 90.0 && r.height() < AREA.height() / 3.0, "about a quarter of the 3D: {r:?}");
        // It follows the view: a bigger view, still the whole width along the bottom.
        let big = Rect::from_min_max(pos2(0.0, 0.0), pos2(1600.0, 900.0));
        let r = open_rect(M, &b, big);
        assert_eq!((r.left(), r.right(), r.bottom()), (MARGIN, 1600.0 - MARGIN, 900.0 - MARGIN));
    }

    /// A plate graph showing `shown` of all six lanes.
    fn plate(shown: LaneSet) -> Kind {
        Kind::Plate { shown, offer: LaneSet::first(6) }
    }

    #[test]
    fn plate_graph_starts_along_the_top_below_the_tab_row_strip_as_high_as_the_marker_graph() {
        let b = GraphBox::default();
        for area in [AREA, Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 300.0))] {
            let r = open_rect(plate(LaneSet::default()), &b, area);
            assert_eq!((r.left(), r.right(), r.top()), (area.left() + MARGIN, area.right() - MARGIN, area.top() + dock::TAB_H), "{r:?} in {area:?}");
            assert_eq!(r.height(), open_rect(M, &b, area).height(), "three lanes, first as high as the marker graph");
            // Clear of the marker graph's first place along the bottom.
            assert!(r.bottom() < open_rect(M, &b, area).top(), "{r:?}");
        }
        // Smallest with one lane: it still fits; smaller than the marker graph's smallest.
        let one = plate(LaneSet::only(2));
        let mut b = GraphBox { size: Some([300.0, 120.0]), ..GraphBox::default() };
        resize(one, &mut b, AREA, Edges { bottom: true, ..Edges::default() }, vec2(0.0, -5000.0));
        let h = open_rect(one, &b, AREA).height();
        assert!(h - graph::NAV_H >= 24.0 && h < M.min_h(), "{h}");
        // Moved and folded like the marker graph.
        move_by(one, &mut b, AREA, TAG, vec2(0.0, 5000.0));
        assert!(inside(open_rect(one, &b, AREA), AREA));
    }

    #[test]
    fn plate_lanes_share_the_height_and_the_smallest_height_follows_their_number() {
        // The same box with one, three or six lanes: the same height while that is above the
        // smallest height of the lanes; else that smallest height.
        let b = GraphBox { size: Some([400.0, 100.0]), ..GraphBox::default() };
        let heights: Vec<f32> = [LaneSet::only(2), LaneSet::default(), LaneSet::first(6)].iter().map(|&s| open_rect(plate(s), &b, AREA).height()).collect();
        assert_eq!(heights[..2], [100.0, 100.0], "one or three lanes: the height stays");
        assert_eq!(heights[2], 6.0 * LANE_MIN_H + graph::NAV_H, "six lanes need more: the smallest height for six");
        assert!(plate(LaneSet::first(6)).min_h() > plate(LaneSet::default()).min_h() && plate(LaneSet::default()).min_h() > plate(LaneSet::only(0)).min_h());
        assert_eq!(plate(LaneSet::default()).min_h(), M.min_h(), "three lanes: as the marker graph's three");
        // Resizing stops at the smallest height of the lanes shown.
        let mut b = GraphBox { size: Some([400.0, 300.0]), ..GraphBox::default() };
        resize(plate(LaneSet::first(6)), &mut b, AREA, Edges { bottom: true, ..Edges::default() }, vec2(0.0, -5000.0));
        assert_eq!(b.size.unwrap()[1], 6.0 * LANE_MIN_H + graph::NAV_H);
    }

    #[test]
    fn plate_lane_set_first_choice_is_the_forces_and_never_shows_nothing() {
        let names = |s: LaneSet| s.iter().map(|k| PLATE_LANES[k].0).collect::<Vec<_>>();
        assert_eq!(names(LaneSet::default()), ["Fx", "Fy", "Fz"]);
        assert_eq!(names(LaneSet::first(6)), ["Fx", "Fy", "Fz", "Mx", "My", "Mz"]);
        assert_eq!(names(LaneSet::first(9)), names(LaneSet::first(6)), "at most six");
        let colors: Vec<Color32> = PLATE_LANES.iter().map(|l| l.1).collect();
        assert_eq!(colors, [theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z, theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z], "x red, y green, z blue, as the marker lanes");
        // What is shown: the wanted lanes the plate has, else the first choice it has, else its first.
        let mx_mz = LaneSet(0b101_000);
        assert_eq!(mx_mz.shown(LaneSet::first(6)), mx_mz);
        assert_eq!(mx_mz.shown(LaneSet::first(3)), LaneSet::default(), "a device with three channels: no moments");
        assert_eq!(LaneSet::default().shown(LaneSet::first(2)), LaneSet::first(2));
        assert_eq!(LaneSet(0).shown(LaneSet::first(6)), LaneSet::default());
        assert_eq!(mx_mz.shown(LaneSet::only(4)), LaneSet::only(4), "only My offered");
        assert_eq!(LaneSet(0xff).shown(LaneSet::first(6)).len(), 6, "bits past Mz are not lanes");
        // Saved as a plain number.
        assert_eq!(ron::to_string(&mx_mz).unwrap(), "40");
        assert_eq!(ron::from_str::<LaneSet>("7").unwrap(), LaneSet::default());
    }

    #[test]
    fn the_tag_drags_the_graph_anywhere_but_never_out_of_the_view() {
        let mut b = GraphBox { size: Some([300.0, 120.0]), ..GraphBox::default() };
        move_by(M, &mut b, AREA, TAG, vec2(400.0, -300.0));
        let r = open_rect(M, &b, AREA);
        assert!((r.size() - vec2(300.0, 120.0)).length() < 0.01);
        assert!((r.left() - (AREA.left() + MARGIN + 400.0)).abs() < 0.01 && (r.top() - (AREA.bottom() - MARGIN - 120.0 - 300.0)).abs() < 0.01, "{r:?}");
        for d in [vec2(-5000.0, -5000.0), vec2(5000.0, 0.0), vec2(0.0, 5000.0)] {
            move_by(M, &mut b, AREA, TAG, d);
            assert!(inside(open_rect(M, &b, AREA), AREA), "{:?}", open_rect(M, &b, AREA));
        }
        // The place is a share of the view: half the view, the graph is still inside.
        let small = Rect::from_min_max(AREA.min, AREA.min + AREA.size() / 2.0);
        assert!(inside(open_rect(M, &b, small), small));
    }

    #[test]
    fn folded_tag_goes_anywhere_too_and_unfolds_where_it_is() {
        // A graph as wide as the view cannot move sideways; its folded tag can.
        let mut b = GraphBox { collapsed: true, ..GraphBox::default() };
        let start = corner(M, &b, AREA, TAG);
        assert_eq!(start, open_rect(M, &GraphBox::default(), AREA).min, "folding keeps the tag where it was on the open graph");
        move_by(M, &mut b, AREA, TAG, vec2(5000.0, 5000.0));
        let at = corner(M, &b, AREA, TAG);
        let tag = Rect::from_min_size(at, tag_box(TAG));
        assert!((tag.max - (AREA.max - Vec2::splat(MARGIN))).length() < 0.01, "lower-right corner of the 3D: {tag:?}");
        move_by(M, &mut b, AREA, TAG, vec2(-200.0, -200.0));
        let at = corner(M, &b, AREA, TAG);
        b.collapsed = false;
        b.size = Some([200.0, 100.0]);
        assert!((open_rect(M, &b, AREA).min - at).length() < 0.01, "unfolds at the tag");
        // Unfolded where the whole graph does not fit: it moves in just enough.
        b.collapsed = true;
        move_by(M, &mut b, AREA, TAG, vec2(5000.0, 5000.0));
        b.collapsed = false;
        assert!(inside(open_rect(M, &b, AREA), AREA));
    }

    #[test]
    fn edges_resize_within_the_minimum_and_the_view() {
        let mut b = GraphBox { pos: Some([0.3, 0.3]), size: Some([300.0, 120.0]), collapsed: false };
        let before = open_rect(M, &b, AREA);
        resize(M, &mut b, AREA, Edges { right: true, bottom: true, ..Edges::default() }, vec2(50.0, 30.0));
        let r = open_rect(M, &b, AREA);
        assert!((r.min - before.min).length() < 0.01 && (r.size() - vec2(350.0, 150.0)).length() < 0.01, "{r:?}");
        resize(M, &mut b, AREA, Edges { left: true, top: true, ..Edges::default() }, vec2(-20.0, -10.0));
        let r2 = open_rect(M, &b, AREA);
        assert!((r2.max - r.max).length() < 0.01 && (r2.size() - vec2(370.0, 160.0)).length() < 0.01, "the other edges stay: {r2:?}");
        resize(M, &mut b, AREA, Edges { left: true, top: true, ..Edges::default() }, vec2(5000.0, 5000.0));
        assert!((open_rect(M, &b, AREA).size() - vec2(MIN_W, M.min_h())).length() < 0.01, "not smaller than the minimum");
        resize(M, &mut b, AREA, Edges { right: true, bottom: true, ..Edges::default() }, vec2(5000.0, 5000.0));
        resize(M, &mut b, AREA, Edges { left: true, top: true, ..Edges::default() }, vec2(-5000.0, -5000.0));
        let (r, whole) = (open_rect(M, &b, AREA), AREA.shrink(MARGIN));
        assert!((r.min - whole.min).length() < 0.01 && (r.max - whole.max).length() < 0.01, "at most the whole 3D view: {r:?}");
    }

    #[test]
    fn under_the_views_name_tag_the_graphs_own_tag_moves_below_it_and_the_mouse_stays_the_tags() {
        // The view's name tag at the top-left of the 3D, as high as the tab row.
        let view_tag = Rect::from_min_size(AREA.min, vec2(200.0, 30.0));
        let mut b = GraphBox::default();
        move_to(M, &mut b, AREA, TAG, AREA.min);
        let r = open_rect(M, &b, AREA);
        assert_eq!(r.min, AREA.min + Vec2::splat(MARGIN), "the graph may go under the name tag");
        let at = tag_at(r.min, TAG, view_tag);
        assert_eq!(at, pos2(r.left() + TAG_PAD, view_tag.bottom()), "its own tag: right below the name tag");
        assert!(Rect::from_min_size(at, TAG).max.y < r.bottom(), "and still on the graph");
        // Right of the name tag nothing moves.
        let beside = pos2(view_tag.right() + 10.0, r.top());
        assert_eq!(tag_at(beside, TAG, view_tag), beside + Vec2::splat(TAG_PAD));
        assert_eq!(tag_at(r.min, TAG, Rect::NOTHING), r.min + Vec2::splat(TAG_PAD), "no name tag: at the corner");
        // The plot and the resize edges take the mouse around the name tag only.
        let [right, below] = graph::around(r, view_tag);
        assert_eq!((right.left(), right.top(), below.left(), below.top()), (view_tag.right(), r.top(), r.left(), view_tag.bottom()));
        assert!(!right.intersects(view_tag.shrink(0.5)) && !below.intersects(view_tag.shrink(0.5)));
        assert_eq!(graph::around(r, Rect::NOTHING), [r, Rect::NOTHING]);
        let top_edge = Rect::from_min_max(pos2(r.left() + CORNER, r.top() - EDGE), pos2(r.right() - CORNER, r.top() + EDGE - 1.0));
        assert_eq!(clear_of(top_edge, view_tag), Rect::from_min_max(pos2(view_tag.right(), top_edge.top()), top_edge.max));
        let left_edge = Rect::from_min_max(pos2(r.left() - EDGE, r.top() + CORNER), pos2(r.left() + EDGE - 1.0, r.bottom() - CORNER));
        assert_eq!(clear_of(left_edge, view_tag), Rect::from_min_max(pos2(left_edge.left(), view_tag.bottom()), left_edge.max));
        assert_eq!(clear_of(left_edge, Rect::NOTHING), left_edge);
    }

    /// v0.27.0: what the name tag of a graph of `n` markers (`left_out` more selected) shows in
    /// `max_w`: the names, the notes ("+N", "선택 N개 중 8개") and the tag's width. v0.29.0: also
    /// the whole tag as read left to right (names, "·" between them, notes).
    fn tag_of(n: usize, left_out: usize, max_w: f32) -> (Vec<String>, Vec<String>, f32) {
        let (names, notes, w, _) = tag_read(n, left_out, max_w);
        (names, notes, w)
    }

    fn tag_read(n: usize, left_out: usize, max_w: f32) -> (Vec<String>, Vec<String>, f32, String) {
        let names: Vec<String> = (0..n).map(|k| format!("LKNE{k}")).collect();
        let keys: Vec<graph::Key> = names.iter().map(|name| graph::Key { name, name_color: theme::TEXT }).collect();
        let ctx = egui::Context::default();
        let mut out = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let t = multi_tag(&ctx.layer_painter(egui::LayerId::background()), &keys, left_out, max_w);
            let mut all: Vec<(f32, String)> = t.names.iter().chain(&t.seps).chain(&t.notes).map(|(x, g)| (*x, g.text().to_owned())).collect();
            all.sort_by(|a, b| a.0.total_cmp(&b.0));
            let read = all.into_iter().map(|(_, s)| s).collect::<Vec<_>>().join(" ");
            out = Some((t.names.iter().map(|n| n.1.text().to_owned()).collect(), t.notes.iter().map(|n| n.1.text().to_owned()).collect(), t.w, read));
        });
        out.unwrap()
    }

    #[test]
    fn tag_of_several_markers_names_each_and_says_what_does_not_fit_or_is_left_out() {
        // Room enough: every name, no note; v0.29.0: the names only, "·" between them, nothing
        // else painted (no colour lines).
        let (names, notes, w) = tag_of(3, 0, 800.0);
        assert_eq!((names, notes.len()), (vec!["LKNE0".to_string(), "LKNE1".into(), "LKNE2".into()], 0));
        assert!(w < 800.0);
        assert_eq!(tag_read(3, 0, 800.0).3, "LKNE0 · LKNE1 · LKNE2");
        assert_eq!(tag_read(8, 4, 2000.0).3, "LKNE0 · LKNE1 · LKNE2 · LKNE3 · LKNE4 · LKNE5 · LKNE6 · LKNE7 선택 12개 중 8개");
        // Eight drawn of twelve selected: all eight names and the note.
        let (names, notes, _) = tag_of(8, 4, 2000.0);
        assert_eq!((names.len(), notes), (8, vec!["선택 12개 중 8개".to_string()]));
        // Narrow: as many names as fit, then "+N" for the others; never wider than the room
        // (so the first lane's max value stays clear), never without a name.
        let (names, notes, w) = tag_of(8, 4, 260.0);
        assert!(names.len() >= 1 && names.len() < 8, "{names:?}");
        assert_eq!(notes, vec![format!("+{}", 8 - names.len()), "선택 12개 중 8개".to_string()]);
        assert!(w <= 260.0, "{w}");
        let (names, _, _) = tag_of(3, 0, 10.0);
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn saved_text_reads_back_and_bad_numbers_fall_back_to_the_first_place() {
        let b = GraphBox { pos: Some([0.25, 0.5]), size: Some([320.0, 140.0]), collapsed: true };
        let back: GraphBox = ron::from_str(&ron::to_string(&b).unwrap()).unwrap();
        assert_eq!(back, b);
        let empty: GraphBox = ron::from_str("()").unwrap();
        assert_eq!(empty, GraphBox::default());
        let bad = GraphBox { pos: Some([f32::NAN, 0.2]), size: Some([f32::INFINITY, -4.0]), collapsed: false };
        for kind in [M, plate(LaneSet::default())] {
            assert_eq!(open_rect(kind, &bad, AREA), open_rect(kind, &GraphBox::default(), AREA));
        }
    }
}
