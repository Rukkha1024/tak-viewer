//! 3D Perspective view in the Motive 3 style: black background, white/grey floor grid,
//! constraint-colored markers (14 mm), marker sticks, asset name pills, cyan primary /
//! yellow secondary selection, selected-marker history trail, Motive HUD
//! ("Perspective", marker count, frame readout, axis triad).
//! Mouse (Motive): left click = select, left drag = box select (Ctrl adds), right drag =
//! orbit, middle drag = pan, wheel = zoom. A left click on a force plate (no marker there)
//! chooses / un-chooses that plate for its floating graph (`graph_float.rs`).

use super::theme;
use super::scene_measurement::Measurement;
use crate::camera::{Camera, Preset, View};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, PointerButton, Pos2, Rect, Sense, Shape, Stroke, Ui};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use tak_core::geom::{v3, Vec3};
use tak_core::tak::MarkerKind;
use tak_core::view_data::{GroupKind, ViewData};

/// Settings default marker size 14 mm -> radius 7 mm.
pub const MARKER_RADIUS_M: f32 = 0.007;
pub const PICK_MIN_PX: f32 = 7.0;
/// Settings default History Length 250 frames.
pub const HISTORY_FRAMES: i64 = 250;
const TRAIL_MARKER_LIMIT: usize = 8;
/// Height of the strip along the bottom of the 3D where the HUD is (axis, frame, marker count).
const HUD_BAND: f32 = 64.0;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SceneOptions {
    pub grid: bool,
    pub axes: bool,
    pub names: bool,
    pub unlabeled: bool,
    pub sticks: bool,
    /// Draw the standard sticks of the marker set when the file stores none.
    pub standard_sticks: bool,
    pub plates: bool,
    pub bones: bool,
    pub asset_names: bool,
    pub history: bool,
    pub marker_colors: bool,
    pub measurements: bool,
    /// History trail length in frames (Motive default 250).
    pub trail_frames: i64,
    /// Trail for every selected marker instead of the primary one only (captures).
    pub trail_all: bool,
}

impl Default for SceneOptions {
    fn default() -> Self {
        SceneOptions {
            grid: true,
            axes: true,
            names: false,
            unlabeled: true,
            sticks: true,
            standard_sticks: true,
            plates: true,
            bones: true,
            asset_names: true,
            history: true,
            marker_colors: true,
            measurements: true,
            trail_frames: HISTORY_FRAMES,
            trail_all: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkerClick {
    #[default]
    Plain,
    Toggle,
    Add,
    Inspect,
    Ignore,
}

fn marker_click(modifiers: egui::Modifiers) -> MarkerClick {
    match (modifiers.ctrl || modifiers.command, modifiers.shift, modifiers.alt) {
        (false, false, false) => MarkerClick::Plain,
        (true, false, false) => MarkerClick::Toggle,
        (false, true, false) => MarkerClick::Add,
        (false, false, true) => MarkerClick::Inspect,
        _ => MarkerClick::Ignore,
    }
}

#[derive(Default)]
pub struct SceneOutput {
    /// Left click without drag: Some(marker) or None (empty space).
    pub clicked: Option<Option<usize>>,
    pub marker_click: MarkerClick,
    /// Left drag finished: markers inside the box and the primary one (nearest to where
    /// the mouse was let go).
    pub box_selected: Option<(Vec<usize>, Option<usize>)>,
    /// Ctrl held for a box selection: add instead of replacing.
    pub ctrl: bool,
    pub double_clicked: Option<usize>,
    /// Right-click point: select it only if it is not already selected.
    pub context_clicked: Option<usize>,
    pub marker_menu: Option<egui::Response>,
    /// Left click without Ctrl on no marker but inside this force plate (as drawn): choose or
    /// un-choose it. `clicked` is then `Some(None)`; the marker selection stays.
    pub plate: Option<usize>,
}

/// Box selection: markers whose drawn center lies inside the box from `from` to `to`
/// (either drag direction), in index order, plus the one nearest to `to` (primary).
pub fn pick_in_box(projected: &[Projected], from: Pos2, to: Pos2) -> (Vec<usize>, Option<usize>) {
    let rect = Rect::from_two_pos(from, to);
    let inside: Vec<&Projected> = projected.iter().filter(|p| rect.contains(p.pos)).collect();
    let primary = inside
        .iter()
        .min_by(|a, b| a.pos.distance(to).partial_cmp(&b.pos.distance(to)).unwrap_or(std::cmp::Ordering::Equal))
        .map(|p| p.marker);
    let mut markers: Vec<usize> = inside.iter().map(|p| p.marker).collect();
    markers.sort_unstable();
    (markers, primary)
}

/// A left drag shorter than this (both ways) counts as a click (long press without moving).
const BOX_MIN_PX: f32 = 3.0;

pub struct Projected {
    pub marker: usize,
    pub pos: Pos2,
    pub depth: f32,
    pub radius: f32,
}

/// Marker under `pointer`: nearest projected center within its radius (min PICK_MIN_PX);
/// ties -> nearer to the camera.
pub fn pick(projected: &[Projected], pointer: Pos2) -> Option<usize> {
    let mut best: Option<(f32, f32, usize)> = None;
    for p in projected {
        let d = p.pos.distance(pointer);
        if d <= p.radius.max(PICK_MIN_PX) {
            let better = match best {
                None => true,
                Some((bd, bz, _)) => d < bd - 0.5 || ((d - bd).abs() <= 0.5 && p.depth < bz),
            };
            if better {
                best = Some((d, p.depth, p.marker));
            }
        }
    }
    best.map(|(_, _, m)| m)
}

pub fn project_markers(view: &View, data: &ViewData, frame: i64, opts: &SceneOptions, selected: &BTreeSet<usize>) -> Vec<Projected> {
    let mut out = Vec::new();
    for (i, m) in data.markers.iter().enumerate() {
        if m.kind == MarkerKind::Unlabeled && !opts.unlabeled && !selected.contains(&i) {
            continue;
        }
        if let Some(p) = m.position(frame) {
            if let Some((pos, depth)) = view.project(p) {
                let radius = view.radius_px(MARKER_RADIUS_M, depth).clamp(2.2, 40.0);
                out.push(Projected { marker: i, pos, depth, radius });
            }
        }
    }
    out
}

fn line3(painter: &egui::Painter, view: &View, a: Vec3, b: Vec3, stroke: Stroke) {
    if let Some((pa, pb)) = view.project_segment(a, b) {
        painter.line_segment([pa, pb], stroke);
    }
}

/// Floor grid on y = 0: Motive default 6 m x 6 m, widened to cover the take.
/// Strong lines every 1 m, faint subdivisions every 0.25 m.
fn grid(painter: &egui::Painter, view: &View, bounds: Option<(Vec3, Vec3)>) {
    let (mut lo_x, mut hi_x, mut lo_z, mut hi_z) = (-3.0f32, 3.0f32, -3.0f32, 3.0f32);
    if let Some((lo, hi)) = bounds {
        lo_x = lo_x.min((lo.x - 1.0).floor());
        hi_x = hi_x.max((hi.x + 1.0).ceil());
        lo_z = lo_z.min((lo.z - 1.0).floor());
        hi_z = hi_z.max((hi.z + 1.0).ceil());
    }
    let (lo_x, hi_x, lo_z, hi_z) = (lo_x.max(-40.0), hi_x.min(40.0), lo_z.max(-40.0), hi_z.min(40.0));
    let minor = Stroke::new(1.0_f32, theme::GRID_MINOR);
    let major = Stroke::new(1.0_f32, theme::GRID_MAJOR);
    for pass_major in [false, true] {
        let mut k = 0;
        loop {
            let x = lo_x + k as f32 * 0.25;
            if x > hi_x + 1e-3 {
                break;
            }
            let is_major = k % 4 == 0;
            if is_major == pass_major {
                line3(painter, view, v3(x, 0.0, lo_z), v3(x, 0.0, hi_z), if is_major { major } else { minor });
            }
            k += 1;
        }
        let mut k = 0;
        loop {
            let z = lo_z + k as f32 * 0.25;
            if z > hi_z + 1e-3 {
                break;
            }
            let is_major = k % 4 == 0;
            if is_major == pass_major {
                line3(painter, view, v3(lo_x, 0.0, z), v3(hi_x, 0.0, z), if is_major { major } else { minor });
            }
            k += 1;
        }
    }
}

fn axes(painter: &egui::Painter, view: &View) {
    let o = v3(0.0, 0.0, 0.0);
    line3(painter, view, o, v3(0.5, 0.0, 0.0), Stroke::new(2.0_f32, theme::AXIS_X));
    line3(painter, view, o, v3(0.0, 0.5, 0.0), Stroke::new(2.0_f32, theme::AXIS_Y));
    line3(painter, view, o, v3(0.0, 0.0, 0.5), Stroke::new(2.0_f32, theme::AXIS_Z));
}

/// HUD coordinate axis, lower-left (Motive default on). `bottom` = lower edge it sits on.
fn gizmo(painter: &egui::Painter, view: &View, rect: Rect, bottom: f32) {
    let origin = pos2(rect.left() + 34.0, bottom - 30.0);
    let len = 20.0;
    let mut axes = [
        (v3(1.0, 0.0, 0.0), theme::AXIS_X, "X"),
        (v3(0.0, 1.0, 0.0), theme::AXIS_Y, "Y"),
        (v3(0.0, 0.0, 1.0), theme::AXIS_Z, "Z"),
    ]
    .map(|(d, c, n)| (view.gizmo_dir(d), c, n));
    axes.sort_by(|a, b| b.0 .2.partial_cmp(&a.0 .2).unwrap_or(std::cmp::Ordering::Equal));
    for ((x, y, _), color, name) in axes {
        let tip = origin + vec2(x, y) * len;
        painter.line_segment([origin, tip], Stroke::new(1.5_f32, color));
        painter.text(origin + vec2(x, y) * (len + 7.0), Align2::CENTER_CENTER, name, FontId::proportional(10.0), color);
    }
}

/// A force plate as drawn on screen (all four corners in front of the camera).
pub struct PlateQuad {
    pub plate: usize,
    pub corners: [Pos2; 4],
}

/// The plate whose drawn quad holds `pointer` (the first one when quads overlap on screen).
pub fn pick_plate(quads: &[PlateQuad], pointer: Pos2) -> Option<usize> {
    let inside = |c: &[Pos2; 4]| {
        // Convex quad: the point is on the same side of all four edges.
        let side = |k: usize| (c[(k + 1) % 4] - c[k]).x * (pointer - c[k]).y - (c[(k + 1) % 4] - c[k]).y * (pointer - c[k]).x;
        let s = [side(0), side(1), side(2), side(3)];
        // Two triangles give the signed area. An edge-on plate has no interior.
        let area = |a: Pos2, b: Pos2| {
            let (a, b) = (a - c[0], b - c[0]);
            a.x * b.y - a.y * b.x
        };
        if area(c[1], c[2]) + area(c[2], c[3]) == 0.0 {
            return false;
        }
        s.iter().all(|&v| v >= 0.0) || s.iter().all(|&v| v <= 0.0)
    };
    quads.iter().find(|q| inside(&q.corners)).map(|q| q.plate)
}

/// Force plates on the floor (fill brighter with |Fz|), the `chosen` one with a bright edge.
/// Returns the plates as drawn (for clicks).
fn plates(painter: &egui::Painter, view: &View, data: &ViewData, frame: i64, chosen: Option<usize>) -> Vec<PlateQuad> {
    // One scale for all plates, so an idle plate's noise never looks like contact.
    let global_max = data.plates.iter().map(|p| p.fz_abs_max).fold(0.0f32, f32::max);
    let mut quads = Vec::new();
    for (pi, p) in data.plates.iter().enumerate() {
        let mut t = 0.0;
        let idx = frame - data.start_frame;
        if global_max > 0.0 && idx >= 0 && (idx as usize) < p.fz.len() {
            let v = p.fz[idx as usize];
            if v.is_finite() {
                t = (v.abs() / global_max).clamp(0.0, 1.0);
            }
        }
        let projected: Vec<Option<(Pos2, f32)>> = p.corners.iter().map(|c| view.project(*c)).collect();
        if projected.iter().all(|x| x.is_some()) {
            let pts: Vec<Pos2> = projected.iter().map(|x| x.unwrap().0).collect();
            quads.push(PlateQuad { plate: pi, corners: [pts[0], pts[1], pts[2], pts[3]] });
            let fill = if t > 0.05 {
                let a = theme::PLATE_ACTIVE;
                Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), (50.0 + 150.0 * t) as u8)
            } else {
                theme::PLATE_FILL
            };
            painter.add(Shape::convex_polygon(pts, fill, Stroke::NONE));
        }
        for k in 0..4 {
            line3(painter, view, p.corners[k], p.corners[(k + 1) % 4], Stroke::new(1.5_f32, theme::PLATE_EDGE));
        }
    }
    // Last, so a neighbour's edge never covers it.
    if let Some(p) = chosen.and_then(|c| data.plates.get(c)) {
        for k in 0..4 {
            line3(painter, view, p.corners[k], p.corners[(k + 1) % 4], Stroke::new(3.0_f32, theme::PLATE_CHOSEN));
        }
    }
    quads
}

fn sticks(painter: &egui::Painter, view: &View, data: &ViewData, frame: i64, standard: bool) {
    sticks_in(painter, view, data, frame, standard, None);
}

/// The sticks of `data` at `frame`; a stick whose end marker is missing is not drawn. `one` =
/// one stroke for all of them (the laid-over file's thin white lines, plan step 8), else each in
/// its own color.
fn sticks_in(painter: &egui::Painter, view: &View, data: &ViewData, frame: i64, standard: bool, one: Option<Stroke>) {
    for s in data.sticks.iter().filter(|s| standard || !s.standard) {
        let a = s.a.iter().find_map(|&m| data.markers[m].position(frame));
        let b = s.b.iter().find_map(|&m| data.markers[m].position(frame));
        if let (Some(a), Some(b)) = (a, b) {
            line3(painter, view, a, b, one.unwrap_or_else(|| Stroke::new(1.6_f32, theme::color(s.color))));
        }
    }
}

/// Line width of the laid-over file's sticks (plan step 8, points).
pub const OVERLAY_STICK_W: f32 = 1.0;

fn history(painter: &egui::Painter, view: &View, data: &ViewData, m: usize, frame: i64, length: i64) {
    let length = length.max(1);
    let from = (frame - length).max(data.start_frame);
    let mut prev: Option<Vec3> = None;
    for f in from..=frame {
        let cur = data.markers[m].position(f);
        if let (Some(a), Some(b)) = (prev, cur) {
            let t = (f - from) as f32 / length as f32;
            line3(painter, view, a, b, Stroke::new(1.5_f32, theme::SELECT_SECONDARY.gamma_multiply(0.25 + 0.75 * t)));
        }
        prev = cur;
    }
}

fn marker_dot(painter: &egui::Painter, center: Pos2, r: f32, color: Color32) {
    painter.circle_filled(center, r, color);
}

fn name_tag(painter: &egui::Painter, at: Pos2, text: &str, color: Color32, size: f32) {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(size), color);
    let rect = Rect::from_min_size(at, galley.size()).expand2(vec2(3.0, 1.0));
    painter.rect_filled(rect, 2.0, theme::NAME_TAG_BG);
    painter.galley(at, galley, color);
}

/// Asset name pill above the skeleton (Motive "Skeleton 1" label).
fn asset_pill(painter: &egui::Painter, center: Pos2, text: &str) {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(12.5), theme::ASSET_TAG_TEXT);
    let size = galley.size();
    let rect = Rect::from_center_size(center, size + vec2(14.0, 6.0));
    painter.rect_filled(rect, 4.0, theme::ASSET_TAG_BG);
    painter.galley(rect.center() - size * 0.5, galley, theme::ASSET_TAG_TEXT);
}

/// What of the two files of a view with a file laid over it (plan step 7) is drawn.
#[derive(Clone, Copy)]
pub struct Layers<'a> {
    /// The view's own file: its markers (and sticks, trail, names, asset tags) are drawn.
    pub own: bool,
    /// The laid-over file shown, and its frame: its markers as white rings (and its sticks as
    /// thin white lines, plan step 8).
    pub overlay: Option<(&'a ViewData, i64)>,
}

impl Default for Layers<'_> {
    fn default() -> Self {
        Layers { own: true, overlay: None }
    }
}

/// A marker of a file laid over the view (plan step 7): a white ring, same radius as a dot.
fn overlay_ring(painter: &egui::Painter, center: Pos2, r: f32) {
    painter.circle_stroke(center, r, Stroke::new(OVERLAY_RING_W, theme::OVERLAY_RING));
}

/// Line width of an overlay ring (points).
pub const OVERLAY_RING_W: f32 = 1.5;

/// Draw the 3D into `rect` and take the mouse in the space `ui` has left and in `row`. `rect`
/// may reach above that space (a view alone in its place is drawn over its tab row,
/// `TakViewerApp::scene_tab`); `row` = the part of that tab row right of the name tag, where
/// the 3D takes the mouse too (`Rect::NOTHING` = none; the tab under the name tag keeps its own).
/// `tag` = size of the name tag at its top-left corner, kept free of HUD text (zero = none).
/// `covers` = the open floating graphs over this 3D (`graph_float.rs`): HUD items at the
/// bottom that one would hide are drawn just above it. `plate` = the chosen force plate
/// (bright edge). `layers` = a second file laid over this one (plan step 7): whether this file's
/// markers are shown (hidden ones are not drawn and cannot be clicked: no sticks, trail, names
/// or asset tags either) and the laid-over file's markers at its frame, drawn as white rings
/// (display only, never clicked); plan step 8 adds its sticks (thin white, under this file's).
#[allow(clippy::too_many_arguments)]
fn trail_markers(selected: &BTreeSet<usize>) -> impl Iterator<Item = usize> + '_ {
    selected.iter().copied().take(TRAIL_MARKER_LIMIT)
}

fn show_marker_name(names: bool, selected: bool, labeled: bool, hovered: bool) -> bool {
    hovered || (names && (selected || labeled))
}

pub fn show(
    ui: &mut Ui,
    rect: Rect,
    row: Rect,
    tag: egui::Vec2,
    covers: &[Rect],
    data: Option<&ViewData>,
    frame: i64,
    cam: &mut Camera,
    opts: &SceneOptions,
    selected: &BTreeSet<usize>,
    primary: Option<usize>,
    measurement: Option<Measurement>,
    plate: Option<usize>,
    capture_label: Option<&str>,
    legacy_capture: bool,
    follow_target: Option<Vec3>,
    layers: Layers,
) -> SceneOutput {
    let (_, mut response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    if row.is_positive() {
        // Own child ui: egui clips the mouse to the ui's clip rect, which ends below the row.
        let mut row_ui = ui.new_child(egui::UiBuilder::new().max_rect(row));
        row_ui.set_clip_rect(row);
        response = response.union(row_ui.interact(row, response.id.with("tab_row"), Sense::click_and_drag()));
    }
    let mut out = SceneOutput::default();

    // Mouse (Motive): left click / left drag box = select markers, right drag = orbit
    // around the pivot, middle drag = pan, wheel = zoom.
    let delta = response.drag_delta();
    if response.dragged_by(PointerButton::Secondary) && delta != egui::Vec2::ZERO {
        cam.orbit(delta.x, delta.y);
    }
    let box_id = response.id.with("box_select");
    if response.drag_started_by(PointerButton::Primary) {
        let start = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos());
        ui.data_mut(|d| d.insert_temp(box_id, start));
    }
    let box_start: Option<Pos2> = ui.data(|d| d.get_temp::<Option<Pos2>>(box_id)).flatten();
    let box_end = response.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.latest_pos()));
    let box_drag = match (box_start, box_end) {
        (Some(a), Some(b)) if response.dragged_by(PointerButton::Primary) || response.drag_stopped_by(PointerButton::Primary) => Some((a, b)),
        _ => None,
    };
    let box_done = response.drag_stopped_by(PointerButton::Primary);
    let box_active = response.dragged_by(PointerButton::Primary) || response.drag_started_by(PointerButton::Primary);
    if box_done || !box_active {
        ui.data_mut(|d| d.remove::<Option<Pos2>>(box_id));
    }
    if response.dragged_by(PointerButton::Middle) && follow_target.is_none() {
        cam.pan(delta.x, delta.y, rect);
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            cam.zoom(scroll);
        }
    }

    // Own clip rect: `rect` can be taller than the space this ui was given.
    let mut painter = ui.painter().clone();
    painter.set_clip_rect(rect);
    painter.rect_filled(rect, 0.0, theme::VIEW_BG);
    let mut display_camera = *cam;
    display_camera.pivot = follow_target.unwrap_or(cam.pivot);
    let view = display_camera.view(rect);
    if opts.grid {
        grid(&painter, &view, data.and_then(|d| d.bounds));
    }
    if opts.axes {
        axes(&painter, &view);
    }
    // "Perspective" right of the name tag; notes below it.
    let (hud_x, note_y) = if tag.x > 0.0 { (tag.x + 10.0, tag.y + 4.0) } else { (10.0, 26.0) };
    let hud_y = if tag.y > 0.0 { (tag.y - 13.0) / 2.0 } else { 8.0 };
    let hud_label = capture_label.unwrap_or_else(|| cam.preset.label());
    painter.text(rect.left_top() + vec2(hud_x, hud_y), Align2::LEFT_TOP, hud_label, FontId::proportional(13.0), theme::HUD_TEXT);
    let show_follow = cam.follow && capture_label.is_none();
    if show_follow {
        painter.text(rect.left_top() + vec2(10.0, note_y), Align2::LEFT_TOP, "Follow Selected", FontId::proportional(12.0), theme::ACCENT);
    }

    let Some(data) = data else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "파일 → 열기, 또는 .tak 파일을 이 창에 끌어다 놓으세요",
            FontId::proportional(18.0),
            theme::TEXT_DIM,
        );
        gizmo(&painter, &view, rect, rect.bottom());
        return out;
    };

    // Plates hidden in the menu cannot be clicked either.
    let plate_quads = if opts.plates { plates(&painter, &view, data, frame, plate) } else { Vec::new() };
    // This file's markers hidden (name tag 1 / key 1): nothing of them is drawn or clicked.
    let own = layers.own;
    // Plan step 8: the laid-over file's sticks, thin white, under everything of this file.
    if let (Some((od, of)), true) = (layers.overlay, opts.sticks) {
        sticks_in(&painter, &view, od, of, opts.standard_sticks, Some(Stroke::new(OVERLAY_STICK_W, theme::OVERLAY_STICK)));
    }
    if own && opts.sticks {
        sticks(&painter, &view, data, frame, opts.standard_sticks);
        if opts.standard_sticks && data.sticks.iter().any(|s| s.standard) {
            painter.text(
                rect.left_top() + vec2(10.0, note_y + if show_follow { 16.0 } else { 0.0 }),
                Align2::LEFT_TOP,
                "선: 파일에 없어 표준 선(Conventional Full Body 39)으로 표시",
                FontId::proportional(12.0),
                theme::TEXT_DIM,
            );
        }
    }
    if own && opts.history {
        if !legacy_capture {
            for m in trail_markers(selected) {
                history(&painter, &view, data, m, frame, HISTORY_FRAMES);
            }
        } else if opts.trail_all {
            for &m in selected {
                history(&painter, &view, data, m, frame, opts.trail_frames);
            }
        } else if let Some(m) = primary {
            history(&painter, &view, data, m, frame, opts.trail_frames);
        }
    }

    let mut projected = if own { project_markers(&view, data, frame, opts, selected) } else { Vec::new() };
    projected.sort_by(|a, b| b.depth.partial_cmp(&a.depth).unwrap_or(std::cmp::Ordering::Equal));
    for p in &projected {
        let m = &data.markers[p.marker];
        let color = if opts.marker_colors || m.kind == MarkerKind::Unlabeled { theme::color(m.color) } else { theme::LABELED_DEFAULT };
        marker_dot(&painter, p.pos, p.radius, color);
        if Some(p.marker) == primary {
            painter.circle_stroke(p.pos, p.radius + 3.0, Stroke::new(2.0_f32, theme::SELECT_PRIMARY));
        } else if selected.contains(&p.marker) {
            painter.circle_stroke(p.pos, p.radius + 3.0, Stroke::new(2.0_f32, theme::SELECT_SECONDARY));
        }
    }
    // The laid-over file: white rings over the dots (a marker in the same place in both files is
    // a dot with a white edge; one that differs stands apart). Not in `projected`: never clicked.
    if let Some((od, of)) = layers.overlay {
        for p in project_markers(&view, od, of, opts, &BTreeSet::new()) {
            overlay_ring(&painter, p.pos, p.radius);
        }
    }

    if own && opts.asset_names {
        for (gi, g) in data.groups.iter().enumerate() {
            if g.kind != GroupKind::AssetTemplate {
                continue;
            }
            let Some(ai) = g.asset else { continue };
            let anchor = if matches!(cam.preset, Preset::Top | Preset::Bottom) {
                // World Y projects into depth here. Put the pill above the points on screen.
                let bounds = g.members.iter().filter_map(|&m| {
                    view.project(data.markers[m].position(frame)?).map(|(pos, _)| pos)
                }).fold(Rect::NOTHING, |bounds, pos| bounds.union(Rect::from_min_max(pos, pos)));
                (bounds.min.x <= bounds.max.x).then(|| pos2(bounds.center().x, bounds.top() - 18.0))
            } else {
                data.asset_anchor(gi, frame).and_then(|(c, top)| {
                    view.project(v3(c.x, top + 0.22, c.z)).map(|(pos, _)| pos)
                })
            };
            if let Some(pos) = anchor {
                asset_pill(&painter, pos, &data.take.assets[ai].name);
            }
        }
    }

    let hover = response.hover_pos()
        .filter(|p| legacy_capture || !covers.iter().any(|cover| cover.contains(*p)))
        .and_then(|p| pick(&projected, p));
    for p in &projected {
        let m = &data.markers[p.marker];
        let labeled = !matches!(data.groups[m.group].kind, GroupKind::Unlabeled);
        let sel = selected.contains(&p.marker);
        if show_marker_name(opts.names, sel, labeled, Some(p.marker) == hover) || (legacy_capture && sel) {
            let color = if Some(p.marker) == primary {
                theme::SELECT_PRIMARY
            } else if sel {
                theme::SELECT_SECONDARY
            } else {
                theme::HUD_TEXT
            };
            name_tag(&painter, p.pos + vec2(p.radius + 4.0, -7.0), &m.name, color, if sel { 13.0 } else { 11.5 });
        }
    }
    if let Some(measurement) = measurement.filter(|_| own) {
        let _span = crate::bench_timing::span("scene.measurement_paint");
        measurement.paint(&painter, &view);
    }

    // Left drag: selection box (translucent fill + outline) while dragging.
    if let Some((a, b)) = box_drag {
        let r = Rect::from_two_pos(a, b);
        painter.rect_filled(r, 0.0, theme::BOX_SELECT_FILL);
        painter.rect_stroke(r, 0.0, Stroke::new(1.0_f32, theme::BOX_SELECT_EDGE));
    }

    let gesture = ui.input(|i| marker_click(i.modifiers));
    // A covering graph owns clicks even before egui has registered its latest geometry.
    let scene_pointer = response.interact_pointer_pos()
        .filter(|p| !covers.iter().any(|cover| cover.contains(*p)));
    // A plain click may pick a plate when there is no marker underneath.
    let mut click = |at: Pos2| {
        if gesture == MarkerClick::Ignore { return; }
        let marker = pick(&projected, at);
        out.clicked = Some(marker);
        out.marker_click = gesture;
        out.plate = if marker.is_none() && gesture == MarkerClick::Plain { pick_plate(&plate_quads, at) } else { None };
    };
    if response.clicked() {
        if let Some(pointer) = scene_pointer {
            click(pointer);
        }
    }
    if let (true, Some((a, b))) = (box_done, box_drag) {
        if (b.x - a.x).abs() < BOX_MIN_PX && (b.y - a.y).abs() < BOX_MIN_PX {
            click(b);
        } else if matches!(gesture, MarkerClick::Plain | MarkerClick::Toggle) {
            out.box_selected = Some(pick_in_box(&projected, a, b));
            out.ctrl = gesture == MarkerClick::Toggle;
        }
    }
    if gesture == MarkerClick::Plain && response.double_clicked() {
        if let Some(pointer) = scene_pointer {
            out.double_clicked = pick(&projected, pointer);
        }
    }
    if response.secondary_clicked() {
        out.context_clicked = scene_pointer.and_then(|p| pick(&projected, p));
    }
    out.marker_menu = own.then_some(response);

    // HUD: axis triad (lower-left), marker details (lower-right), frame readout (bottom center).
    // An item between x0 and x1 sits on the bottom edge, or just above the floating graphs
    // that cover its place (and leave room above).
    let bottom_at = |x0: f32, x1: f32| {
        covers
            .iter()
            .filter(|c| c.bottom() > rect.bottom() - HUD_BAND && c.left() < x1 && c.right() > x0 && c.top() - rect.top() > 2.0 * HUD_BAND)
            .map(|c| c.top() - 2.0)
            .fold(rect.bottom(), f32::min)
    };
    gizmo(&painter, &view, rect, bottom_at(rect.left(), rect.left() + 70.0));
    let shown = data.markers.iter().filter(|m| m.present(frame)).count();
    let right = pos2(rect.right(), bottom_at(rect.right() - 110.0, rect.right()));
    painter.text(
        right + vec2(-10.0, -26.0),
        Align2::RIGHT_BOTTOM,
        format!("{shown} Markers"),
        FontId::proportional(13.0),
        theme::HUD_TEXT,
    );
    painter.text(
        right + vec2(-10.0, -8.0),
        Align2::RIGHT_BOTTOM,
        format!("{} Selected", selected.len()),
        FontId::proportional(13.0),
        theme::HUD_TEXT,
    );
    let rate = if data.take.frame_rate > 0.0 { data.take.frame_rate } else { 100.0 };
    let secs = (frame - data.start_frame) as f64 / rate;
    let mid = pos2(rect.center().x, bottom_at(rect.center().x - 60.0, rect.center().x + 60.0));
    painter.text(
        mid + vec2(0.0, -10.0),
        Align2::CENTER_BOTTOM,
        format!("{frame}"),
        FontId::proportional(28.0),
        theme::HUD_TEXT,
    );
    painter.text(
        mid + vec2(0.0, -44.0),
        Align2::CENTER_BOTTOM,
        format!("{:02}:{:06.3}", (secs / 60.0).floor() as i64, secs % 60.0),
        FontId::monospace(12.0),
        theme::TEXT_DIM,
    );
    out
}

#[cfg(test)]
mod mode_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(marker: usize, x: f32, y: f32, depth: f32, radius: f32) -> Projected {
        Projected { marker, pos: pos2(x, y), depth, radius }
    }

    #[test]
    fn pick_nearest_within_radius() {
        let ps = vec![proj(0, 100.0, 100.0, 5.0, 6.0), proj(1, 112.0, 100.0, 5.0, 6.0)];
        assert_eq!(pick(&ps, pos2(103.0, 100.0)), Some(0));
        assert_eq!(pick(&ps, pos2(110.0, 101.0)), Some(1));
        assert_eq!(pick(&ps, pos2(150.0, 150.0)), None);
    }

    #[test]
    fn pick_prefers_front_marker_on_overlap() {
        let ps = vec![proj(0, 100.0, 100.0, 9.0, 8.0), proj(1, 100.0, 100.0, 3.0, 8.0)];
        assert_eq!(pick(&ps, pos2(101.0, 100.0)), Some(1));
    }

    #[test]
    fn box_takes_markers_inside_in_either_drag_direction() {
        let ps = vec![
            proj(0, 10.0, 10.0, 5.0, 4.0),
            proj(3, 50.0, 40.0, 5.0, 4.0),
            proj(1, 90.0, 80.0, 5.0, 4.0),
            proj(2, 200.0, 200.0, 5.0, 4.0),
        ];
        let (m, primary) = pick_in_box(&ps, pos2(0.0, 0.0), pos2(100.0, 100.0));
        assert_eq!(m, vec![0, 1, 3]);
        assert_eq!(primary, Some(1), "nearest to where the mouse was let go");
        let (m, primary) = pick_in_box(&ps, pos2(100.0, 100.0), pos2(0.0, 0.0));
        assert_eq!(m, vec![0, 1, 3]);
        assert_eq!(primary, Some(0));
    }

    #[test]
    fn box_edges_count_and_outside_does_not() {
        let ps = vec![proj(5, 100.0, 50.0, 5.0, 4.0), proj(6, 100.5, 50.0, 5.0, 4.0)];
        let (m, _) = pick_in_box(&ps, pos2(0.0, 0.0), pos2(100.0, 50.0));
        assert_eq!(m, vec![5]);
        let (m, primary) = pick_in_box(&ps, pos2(300.0, 300.0), pos2(400.0, 400.0));
        assert!(m.is_empty());
        assert_eq!(primary, None);
    }

    #[test]
    fn pick_small_markers_use_min_radius() {
        let ps = vec![proj(4, 50.0, 50.0, 40.0, 2.0)];
        assert_eq!(pick(&ps, pos2(55.0, 50.0)), Some(4));
    }

    #[test]
    fn click_on_projected_marker_selects_it_and_behind_camera_is_never_projected() {
        let cam = Camera::default();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let view = cam.view(rect);
        let front = cam.pivot;
        let behind = view.eye - view.forward * 2.0;
        assert!(view.project(behind).is_none());
        let (pos, depth) = view.project(front).unwrap();
        let ps = vec![Projected { marker: 7, pos, depth, radius: view.radius_px(MARKER_RADIUS_M, depth) }];
        assert_eq!(pick(&ps, pos), Some(7));
    }

    #[test]
    fn right_click_point_passes_the_context_target_to_the_app() {
        use eframe::egui::{Event, Modifiers, RawInput};
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let data = crate::views::sample_data("scene.tak", &["M"]);
        let selected = BTreeSet::from([0]);
        let state = std::cell::RefCell::new((Camera::default(), Rect::NOTHING, SceneOutput::default()));
        let render = |events: Vec<Event>| {
            ctx.run(RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut state = state.borrow_mut();
                    state.1 = ui.max_rect();
                    let rect = state.1;
                    state.2 = show(ui, rect, Rect::NOTHING, vec2(0.0, 0.0), &[], Some(&data), 0, &mut state.0,
                        &SceneOptions::default(), &selected, Some(0), None, None, None, false, None, Layers { own: true, overlay: None });
                });
            })
        };
        render(vec![]);
        let at = { let state = state.borrow(); project_markers(&state.0.view(state.1), &data, 0, &SceneOptions::default(), &selected)[0].pos };
        let event = |pressed| Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE };
        render(vec![Event::PointerMoved(at)]);
        render(vec![event(true)]);
        render(vec![event(false)]);
        assert_eq!(state.borrow().2.context_clicked, Some(0));
        assert!(state.borrow().2.marker_menu.as_ref().unwrap().secondary_clicked());
    }
}
