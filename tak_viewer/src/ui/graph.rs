
use super::theme;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Id, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};

#[path = "graph_tracks.rs"]
pub(crate) mod tracks;
#[path = "graph_conveniences.rs"]
pub(crate) mod conveniences;

#[derive(Clone)]
pub struct Lane {
    /// Short label drawn as the faded watermark (e.g. "X").
    pub letter: String,
    pub color: Color32,
    /// Runs of (frame, value) without gaps.
    pub runs: std::sync::Arc<Vec<Vec<[f64; 2]>>>,
    pub unit: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key<'a> {
    pub name: &'a str,
    /// Its name's color: tracked or not at the current frame, as the one marker's name.
    pub name_color: Color32,
}

/// A curve of a marker graph (v0.29.0 hover): the marker, as its place in the graph (0 = the
/// marker whose lanes `show` gets, k = the k-th of `Drawn::more`), and whether it is the curve
/// of the file laid over the view (dashed) rather than the view's own file (solid).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Curve {
    pub marker: usize,
    pub laid_over: bool,
}

/// Width of a curve (points) and of a one-sample dot's radius.
pub const LINE_W: f32 = 1.4;
const DOT_R: f32 = 1.5;
pub const HOVER_W: f32 = 2.0 * LINE_W;
const HOVER_DOT_R: f32 = 2.0 * DOT_R;
pub const HOVER_LIGHTEN: f32 = 0.3;
/// The hit rule (orchestrator's choice, user not asked yet): a curve is under the mouse when it
/// passes within this many points (vertically) of the pointer at the pointer's x, in the lane
/// under the pointer; the nearest wins, ties go to the earlier marker.
pub const HOVER_REACH: f32 = 5.0;
/// Half the width (points) around the pointer's x in which a curve's points count (a thinned
/// column is `COLUMN_W` wide, so a steep stroke is found over its whole height).
const HOVER_HALF_W: f32 = COLUMN_W / 2.0;
/// The hovered marker's name next to the pointer (Figma mockup ①, box removed by the user):
/// 11 pt, light (`theme::HUD_TEXT` = #e8e8e8), its top-left this far from the pointer tip; to the
/// left / above the pointer where it would leave the lanes.
pub const HOVER_FONT: f32 = 11.0;
pub const HOVER_TEXT_AT: Vec2 = vec2(14.0, 16.0);
const HOVER_TEXT_FLIP: Vec2 = vec2(6.0, 4.0);
/// `--hover` (agent pictures) and `--bench-hover`: the pointer put this far right of the
/// current-frame line, on the marker's own curve in the Y lane.
pub const FORCED_HOVER_DX: f32 = 12.0;

#[derive(Clone, Copy, Debug, Default)]
struct ViewRange {
    lo: f64,
    hi: f64,
}

impl ViewRange {
    fn full(first: i64, last: i64) -> Self {
        Self { lo: first as f64 - 0.5, hi: last as f64 + 0.5 }
    }

    fn load(ui: &Ui, id: Id, full: Self) -> Self {
        ui.data(|d| d.get_temp::<Self>(id))
            .filter(|vr| vr.hi > vr.lo && vr.lo >= full.lo - 1.0 && vr.hi <= full.hi + 1.0)
            .unwrap_or(full)
    }

    fn x(self, plot: Rect, frame: f64) -> f32 {
        plot.left() + ((frame - self.lo) / (self.hi - self.lo)) as f32 * plot.width()
    }

    fn at(self, plot: Rect, x: f32) -> f64 {
        self.lo + ((x - plot.left()) / plot.width()) as f64 * (self.hi - self.lo)
    }

    fn navigate(&mut self, full: Self, plot: Rect, zoom: Option<(f32, f32)>, pan: f32) {
        if plot.width() <= 0.0 { return; }
        if let Some((x, scroll)) = zoom {
            let center = self.at(plot, x);
            let k = (-scroll as f64 * 0.002).exp();
            let max_span = full.hi - full.lo;
            let span = ((self.hi - self.lo) * k).clamp(10.0_f64.min(max_span), max_span);
            let t = (center - self.lo) / (self.hi - self.lo);
            self.lo = center - t * span;
            self.hi = self.lo + span;
        }
        let df = -(pan / plot.width()) as f64 * (self.hi - self.lo);
        self.lo += df;
        self.hi += df;
        let span = self.hi - self.lo;
        if self.lo < full.lo {
            self.lo = full.lo;
            self.hi = self.lo + span;
        }
        if self.hi > full.hi {
            self.hi = full.hi;
            self.lo = (self.hi - span).max(full.lo);
        }
    }
}

pub const NAV_H: f32 = 18.0;

/// How a graph is painted: solid in a pane, or see-through when it floats over a 3D view
/// (`graph_float.rs`).
#[derive(Clone, Copy)]
pub struct Look {
    pub bg: Color32,
    pub nav_bg: Color32,
    pub watermark: Color32,
    /// Min / max value texts at the left of each lane.
    pub values: bool,
    /// Width kept free at the top-left corner (the floating graph's name tag): the first
    /// lane's max text starts right of it.
    pub tag_w: f32,
    /// Something drawn over the graph's top-left corner that keeps the mouse there (the 3D
    /// view's own name tag over a floating graph): the graph does not take it there.
    /// `Rect::NOTHING` = none.
    pub keep_out: Rect,
    /// Plot height for pointer input while marker topology is pending. Paint stays unchanged.
    pub pending_plot_height: f32,
    /// Inclusive edit range shared with the timeline; absent for other graph types.
    pub selection: Option<(i64, i64)>,
}

impl Look {
    /// In a pane (GRAPH, devices): Motive Graph View colors.
    pub fn pane() -> Look {
        Look { bg: theme::GRAPH_BG, nav_bg: theme::NAV_BG, watermark: theme::GRAPH_WATERMARK, values: true, tag_w: 0.0, keep_out: Rect::NOTHING, pending_plot_height: 0.0, selection: None }
    }
}

/// A Comment's frames in a marker graph (Comment plan step 6): a pale orange band behind the
/// curves, across the lanes, for each Comment that names the graphed marker
/// (`panels::SpanCache`). Display only: no click, no hover text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: i64,
    pub end: i64,
    /// The selected Comment (`View::picked`): a little stronger, with an orange edge line.
    pub picked: bool,
}

/// A one-frame Comment is still this wide (points), around its frame.
pub const SPAN_MIN_W: f32 = 2.0;
/// Fill of a Comment's band (`theme::COMMENT` #ff9800, about 16 % / 28 % opaque, premultiplied)
/// and the edge line of the selected one.
pub const SPAN_FILL: Color32 = Color32::from_rgba_premultiplied(40, 24, 0, 40);
pub const SPAN_FILL_PICKED: Color32 = Color32::from_rgba_premultiplied(72, 43, 0, 72);
pub const SPAN_EDGE: Color32 = theme::COMMENT;

/// x of a Comment's band in a plot from `left` over `width` showing frames `lo..hi` (a frame
/// `f` covers `f − 0.5 .. f + 0.5`): at least `SPAN_MIN_W` wide around its middle. None when it
/// is outside the frames in view.
fn span_x(s: &Span, lo: f64, hi: f64, left: f32, width: f32) -> Option<(f32, f32)> {
    let (a, b) = (s.start as f64 - 0.5, s.end as f64 + 0.5);
    if b < lo || a > hi {
        return None;
    }
    let x = |f: f64| left + ((f - lo) / (hi - lo)) as f32 * width;
    let (x0, x1) = (x(a), x(b));
    Some(if x1 - x0 < SPAN_MIN_W {
        let m = (x0 + x1) / 2.0;
        (m - SPAN_MIN_W / 2.0, m + SPAN_MIN_W / 2.0)
    } else {
        (x0, x1)
    })
}

/// Paint the Comment bands over the lanes' area `plot` (before the lanes, so behind the grid
/// and the curves): the others first, the selected one last with an edge line at both ends.
fn paint_spans(painter: &egui::Painter, plot: Rect, lo: f64, hi: f64, spans: &[Span]) {
    let clipped = painter.with_clip_rect(plot);
    for picked in [false, true] {
        for s in spans.iter().filter(|s| s.picked == picked) {
            let Some((x0, x1)) = span_x(s, lo, hi, plot.left(), plot.width()) else { continue };
            let r = Rect::from_min_max(pos2(x0, plot.top()), pos2(x1, plot.bottom()));
            clipped.rect_filled(r, 0.0, if picked { SPAN_FILL_PICKED } else { SPAN_FILL });
            if picked {
                for x in [x0 + 0.5, x1 - 0.5] {
                    clipped.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(1.0_f32, SPAN_EDGE));
                }
            }
        }
    }
}

fn paint_selection(painter: &egui::Painter, plot: Rect, lo: f64, hi: f64, range: Option<(i64, i64)>) {
    let Some((start, end)) = range else { return };
    let selection = Span { start, end, picked: false };
    let Some((left, right)) = span_x(&selection, lo, hi, plot.left(), plot.width()) else { return };
    let painter = painter.with_clip_rect(plot);
    painter.rect_filled(Rect::from_min_max(pos2(left, plot.top()), pos2(right, plot.bottom())), 0.0, theme::RANGE.gamma_multiply(0.12));
    for x in [left, right] {
        painter.extend(Shape::dashed_line(&[pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(1.0, theme::RANGE), 4.0, 2.0));
    }
}

/// The parts of `r` that `hole` leaves free, for a `hole` that covers the top-left corner of
/// `r` (the 3D view's name tag over a floating graph): right of it and below it (the two
/// overlap). `[r, NOTHING]` when the hole misses `r`.
pub fn around(r: Rect, hole: Rect) -> [Rect; 2] {
    if !r.intersect(hole).is_positive() {
        return [r, Rect::NOTHING];
    }
    [Rect::from_min_max(pos2(hole.right(), r.top()), r.max), Rect::from_min_max(pos2(r.left(), hole.bottom()), r.max)]
}

fn nice_step(span: f64, target: f64) -> f64 {
    let raw = (span / target.max(1.0)).max(1.0);
    let mag = 10f64.powf(raw.log10().floor());
    for k in [1.0, 2.0, 5.0, 10.0] {
        if k * mag >= raw {
            return k * mag;
        }
    }
    10.0 * mag
}

/// One lane as it is drawn: min / max over the frames in view and its lines thinned out to
/// at most four points per pixel column, in (frame, value).
#[derive(Clone)]
struct DrawnLane {
    lo: f64,
    hi: f64,
    runs: std::sync::Arc<Vec<Vec<[f64; 2]>>>,
}

/// Frame range in view (bits of lo, hi) and the number of columns `thin` worked for.
type ThinKey = (u64, u64, usize);

/// One more marker's lanes as drawn (`thin`, v0.27.0) and the range and width they were thinned
/// for: thinned again when they changed, as far as the frame's work allows (`spend`); until then
/// the curve is drawn as thinned before (None = not yet: not drawn yet).
#[derive(Default)]
struct Thinned {
    key: Option<ThinKey>,
    lanes: Vec<DrawnLane>,
}

/// Source descriptors travel with a background result so colors, curve counts and samples
/// are published together. Pending settings never recolor the previous complete graph.
#[derive(Clone, Default)]
struct BackgroundSources {
    pair: Option<Pair>,
    more: Vec<Vec<Lane>>,
    more_pairs: Vec<Vec<Lane>>,
}

/// One worker prepares every curve of a device graph together, so faint raw extrema and
/// processed overlays never enter the ordinary marker path's UI thinning loop.
struct BackgroundPrepared {
    primary: Vec<Lane>,
    sources: BackgroundSources,
    lanes: Vec<DrawnLane>,
    pair_lanes: Vec<DrawnLane>,
    more_lanes: Vec<Thinned>,
    more_pair_lanes: Vec<Thinned>,
}

/// One lane's complete curves: compact solid points first, then a mesh for one marker when
/// its later-frame promotion is allowed; dots and dashed meshes retain painter order.
/// Shapes are rebuilt only when their inputs (`ShapesKey`) change.
struct LaneShapes {
    key: ShapesKey,
    shapes: std::sync::Arc<Vec<Shape>>,
    stream: Option<ShapeStream>,
    /// A complete compact single-marker lane may become a mesh after this application frame.
    promote_after: Option<u64>,
    #[cfg(test)]
    drew: Drew,
}

impl LaneShapes {
    /// Promote only cached screen points; no trajectory samples or hover shapes are revisited.
    fn promote(&mut self) {
        let _span=crate::bench_timing::span("graph.mesh_promote");
        let mut tess = egui::epaint::Tessellator::new(self.key.pixels_per_point, self.key.options, [1, 1], vec![]);
        tess.set_clip_rect(self.key.clip);
        let mut mesh = egui::Mesh::default();
        let mut promoted = Vec::with_capacity(self.shapes.len());
        let flush = |mesh: &mut egui::Mesh, shapes: &mut Vec<Shape>| {
            if !mesh.is_empty() { shapes.push(Shape::mesh(std::mem::take(mesh))); }
        };
        for shape in std::sync::Arc::unwrap_or_clone(std::mem::take(&mut self.shapes)) {
            match shape {
                Shape::Path(path) => tess.tessellate_shape(Shape::Path(path), &mut mesh),
                other => { flush(&mut mesh, &mut promoted); promoted.push(other); }
            }
        }
        flush(&mut mesh, &mut promoted);
        self.shapes = std::sync::Arc::new(promoted);
        self.promote_after = None;
    }
}

/// Epaint owns mesh buffers. Prepare two exact frame copies ahead, then move them
/// into the painter; cloning the cached mesh on the UI would copy megabytes per frame.
struct ShapeStream {
    tx: std::sync::mpsc::SyncSender<Vec<Shape>>,
    rx: std::sync::mpsc::Receiver<Vec<Shape>>,
}
impl LaneShapes {
    fn stream(&mut self) {
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        for _ in 0..2 { let _ = tx.send(self.shapes.as_ref().clone()); }
        self.stream = Some(ShapeStream { tx, rx });
    }
    fn paint(&mut self, painter: &egui::Painter, value_transform: Option<(f32, f32, f32)>) {
        let transform = |mut shape| {
            if let Some((pivot, scale, offset)) = value_transform {
                let shift = |p: &mut Pos2| p.y = (p.y - pivot) * scale + pivot + offset;
                match &mut shape {
                    Shape::Mesh(mesh) => mesh.vertices.iter_mut().for_each(|v| shift(&mut v.pos)),
                    Shape::Path(path) => path.points.iter_mut().for_each(shift),
                    Shape::Circle(dot) => shift(&mut dot.center),
                    _ => {}
                }
            }
            shape
        };
        if let Some(stream) = &self.stream {
            if let Ok(shapes) = stream.rx.try_recv() {
                let source = self.shapes.clone();
                let tx = stream.tx.clone();
                crate::work_queue::geometry(move || { let _ = tx.send(source.as_ref().clone()); });
                painter.extend(shapes.into_iter().map(transform));
                return;
            }
        }
        // Complete identical geometry remains visible if a background copy is late.
        let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("graph.stream_fallback_copy"));
        painter.extend(self.shapes.iter().cloned().map(transform));
    }
}

/// What a lane's painted curves depend on (`LaneShapes`).
#[derive(Clone, PartialEq)]
struct ShapesKey {
    /// `Drawn::version`: the thinned points.
    version: u64,
    lane: Rect,
    /// Frames in view and the lane's value range (with its padding and the legend strip).
    x: (f64, f64),
    y: (f64, f64, f32),
    shown: [bool; 2],
    /// What the cached/promoted triangles depend on too (`curve_shapes`).
    clip: Rect,
    pixels_per_point: f32,
    options: egui::epaint::TessellationOptions,
    /// Multiple tinted curves retain paths so egui applies tint before AA tessellation.
    untinted: bool,
}

impl ShapesKey {
    fn same_except_value(&self, other: &Self) -> bool {
        Self { y: other.y, ..self.clone() } == *other && self.y.2 == other.y.2
    }

    /// Convert the cached Y mapping to the live one. Approximate stroke joins and dash
    /// lengths only until the exact mesh for the new value extent is installed.
    fn value_transform(&self, target: &Self) -> (f32, f32, f32) {
        let (lo, hi, keep) = self.y;
        let (target_lo, target_hi, _) = target.y;
        let span = target_hi - target_lo;
        let offset = ((target_lo - lo) / span) as f32 * (self.lane.height() - 6.0 - keep);
        (self.lane.bottom() - 3.0, ((hi - lo) / span) as f32, offset)
    }
}

type FontAtlas = egui::epaint::mutex::Mutex<egui::epaint::TextureAtlas>;

#[derive(Clone, PartialEq)]
struct DecorationGeometry {
    rect: Rect,
    clip: Rect,
    /// Preserve the original plot arithmetic even for fractional floating-graph rectangles.
    plot_x: (f32, f32),
    frames: (u64, u64),
    pixels_per_point: f32,
    options: egui::epaint::TessellationOptions,
}

impl DecorationGeometry {
    fn x(&self, frame: f64) -> f32 {
        let (lo, hi) = (f64::from_bits(self.frames.0), f64::from_bits(self.frames.1));
        self.plot_x.0 + ((frame - lo) / (hi - lo)) as f32 * self.plot_x.1
    }

    fn ticks(&self) -> Vec<f64> {
        let (lo, hi) = (f64::from_bits(self.frames.0), f64::from_bits(self.frames.1));
        let step = nice_step(hi - lo, (self.plot_x.1 / 80.0) as f64);
        let mut ticks = Vec::new();
        let first = (lo / step).ceil() * step;
        let mut t = if first == 0.0 { 0.0 } else { first };
        while t <= hi { ticks.push(t); t += step; }
        ticks
    }
}

#[derive(Clone, PartialEq)]
struct LaneDecorationKey {
    geometry: DecorationGeometry,
    index: usize,
    lane_h: f32,
    range: (u64, u64),
    values: bool,
    tag_w: f32,
    watermark: Color32,
}

struct LaneDecoration {
    key: LaneDecorationKey,
    letter: String,
    unit: &'static str,
    shapes: Vec<Shape>,
}

#[derive(Clone, PartialEq)]
struct NavDecorationKey {
    geometry: DecorationGeometry,
    bg: Color32,
}

struct NavDecoration {
    key: NavDecorationKey,
    background: Shape,
    ticks: Vec<Shape>,
}

/// Raw, untinted shapes: the current Painter applies disabled/fade/opacity exactly once.
/// A Weak retains only atlas identity, preventing address reuse without retaining its image.
#[derive(Default)]
struct Decorations {
    atlas: std::sync::Weak<FontAtlas>,
    lanes: Vec<Option<LaneDecoration>>,
    nav: Option<NavDecoration>,
    #[cfg(test)]
    force_uncached: bool,
    #[cfg(test)]
    builds: usize,
}

fn decoration_text(shapes: &mut Vec<Shape>, painter: &egui::Painter, pos: Pos2, anchor: Align2, text: impl ToString, font: FontId, color: Color32) {
    let galley = painter.layout_no_wrap(text.to_string(), font, color);
    if !galley.is_empty() {
        let rect = anchor.anchor_size(pos, galley.size());
        shapes.push(Shape::galley(rect.min, galley, color));
    }
}

fn lane_decoration(painter: &egui::Painter, key: &LaneDecorationKey, lane: &Lane) -> Vec<Shape> {
    let mut shapes = Vec::new();
    let lr = key.geometry.rect;
    if key.index > 0 { shapes.push(Shape::line_segment([lr.left_top(), lr.right_top()], Stroke::new(1.0_f32, theme::GRAPH_SEP))); }
    let letter_x = if key.index == 0 && key.tag_w > 0.0 && key.lane_h < 48.0 { key.tag_w + 12.0 } else { 18.0 };
    decoration_text(&mut shapes, painter, lr.left_center() + vec2(letter_x, 0.0), Align2::LEFT_CENTER,
        &lane.letter, FontId::proportional((key.lane_h * 0.72).min(72.0)), key.watermark);
    // Keep compact segments, rather than expanded triangles: tint must precede egui's
    // antialiasing exactly as in the uncached painter, including disabled panels.
    for tk in key.geometry.ticks() {
        let x = key.geometry.x(tk);
        shapes.extend(Shape::dashed_line(&[pos2(x, lr.top()), pos2(x, lr.bottom())], Stroke::new(1.0_f32, theme::GRAPH_GRID), 2.0, 3.0));
    }
    let (lo, hi) = (f64::from_bits(key.range.0), f64::from_bits(key.range.1));
    if lo.is_finite() && key.values {
        let font = FontId::proportional(11.0);
        let top = if key.index == 0 && key.tag_w > 0.0 { vec2(key.tag_w + 6.0, 6.0) } else { vec2(4.0, 2.0) };
        decoration_text(&mut shapes, painter, lr.left_top() + top, Align2::LEFT_TOP, format!("{hi:.2} {}", lane.unit), font.clone(), theme::TEXT_DIM);
        decoration_text(&mut shapes, painter, lr.left_bottom() + vec2(4.0, -2.0), Align2::LEFT_BOTTOM, format!("{lo:.2} {}", lane.unit), font, theme::TEXT_DIM);
    }
    shapes
}

impl Decorations {
    fn begin(&mut self, ui: &Ui) {
        let atlas = ui.fonts(|fonts| std::sync::Arc::downgrade(&fonts.texture_atlas()));
        let reset = !self.atlas.ptr_eq(&atlas);
        #[cfg(test)] let reset = reset || self.force_uncached;
        if reset { self.lanes.clear(); self.nav = None; self.atlas = atlas; }
    }

    fn paint_lane(&mut self, painter: &egui::Painter, key: LaneDecorationKey, lane: &Lane) {
        let index = key.index;
        if self.lanes.len() <= index { self.lanes.resize_with(index + 1, || None); }
        if !self.lanes[index].as_ref().is_some_and(|cached| cached.key == key && cached.letter == lane.letter && cached.unit == lane.unit) {
            let shapes = lane_decoration(painter, &key, lane);
            self.lanes[index] = Some(LaneDecoration { key, letter: lane.letter.clone(), unit: lane.unit, shapes });
            #[cfg(test)] { self.builds += 1; }
        }
        painter.extend(self.lanes[index].as_ref().unwrap().shapes.iter().cloned());
    }

    fn prepare_nav(&mut self, painter: &egui::Painter, key: NavDecorationKey) {
        if self.nav.as_ref().is_some_and(|cached| cached.key == key) { return; }
        let nav = key.geometry.rect;
        let background = Shape::rect_filled(nav, 0.0, key.bg);
        let mut ticks = Vec::new();
        for tk in key.geometry.ticks() {
            let x = key.geometry.x(tk);
            ticks.push(Shape::line_segment([pos2(x, nav.top()), pos2(x, nav.top() + 4.0)], Stroke::new(1.0_f32, theme::TEXT_DIM)));
            decoration_text(&mut ticks, painter, pos2(x, nav.center().y + 1.0), Align2::CENTER_CENTER,
                format!("{tk:.0}"), FontId::monospace(10.0), theme::TEXT);
        }
        self.nav = Some(NavDecoration { key, background, ticks });
        #[cfg(test)] { self.builds += 1; }
    }
}

/// The hovered marker's curve in one lane, thick and lighter (v0.29.0), painted over the lane's
/// own shapes (which stay as they are: hovering never makes them again). Kept while the same
/// curve is hovered over the same lane, and after the mouse leaves, so hovering in and out
/// again costs nothing; another curve makes only this one lane's one curve again.
struct HoverShapes {
    key: ShapesKey,
    curve: Curve,
    shapes: Vec<Shape>,
}

/// Where a lane's frames and values are on screen (as `show` draws them), for the hover hit
/// test (`hit`) and the hovered curve.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Map {
    left: f32,
    width: f32,
    lo: f64,
    hi: f64,
    bottom: f32,
    /// Height the values use (the lane less its padding and the legend strip).
    span: f32,
    ylo: f64,
    yhi: f64,
}

impl Map {
    fn x(&self, f: f64) -> f32 {
        self.left + ((f - self.lo) / (self.hi - self.lo)) as f32 * self.width
    }

    fn frame(&self, x: f32) -> f64 {
        self.lo + ((x - self.left) / self.width) as f64 * (self.hi - self.lo)
    }

    fn y(&self, v: f64) -> f32 {
        self.bottom - 3.0 - ((v - self.ylo) / (self.yhi - self.ylo)) as f32 * self.span
    }
}

/// One lane as the hover hit test sees it: its rectangle, where its frames and values are, and
/// the curves drawn in it (as thinned for the screen: what is on screen, not the file's samples),
/// in the graph's marker order, each marker's own curve before its laid-over one.
struct HitLane<'a> {
    rect: Rect,
    map: Map,
    curves: Vec<(Curve, &'a DrawnLane)>,
}

/// Top and bottom (y, points) of the curve `d` between frames `fa ..= fb` (`fa` ≤ `fb`), as
/// drawn: its points there and the line where it crosses `fa` and `fb`. None where it is not
/// drawn (outside its runs, in a gap). Halving into the runs and the points: a few steps per
/// curve, whatever the file length.
fn span_at(d: &DrawnLane, fa: f64, fb: f64, map: &Map) -> Option<(f32, f32)> {
    let mut out: Option<(f32, f32)> = None;
    let mut add = |v: f64| {
        let y = map.y(v);
        out = Some(out.map_or((y, y), |(a, b)| (a.min(y), b.max(y))));
    };
    let first = d.runs.partition_point(|r| r.last().is_some_and(|p| p[0] < fa));
    for run in d.runs[first..].iter().take_while(|r| r.first().is_some_and(|p| p[0] <= fb)) {
        let a = run.partition_point(|p| p[0] < fa);
        let b = run.partition_point(|p| p[0] <= fb);
        for p in &run[a..b] {
            add(p[1]);
        }
        // Where the line between two points crosses the window's edges.
        let at = |f: f64, k: usize| {
            let (p, q) = (run[k - 1], run[k]);
            p[1] + (q[1] - p[1]) * (f - p[0]) / (q[0] - p[0])
        };
        if a > 0 && a < run.len() {
            add(at(fa, a));
        }
        if b > 0 && b < run.len() {
            add(at(fb, b));
        }
    }
    out
}

/// The curve under the pointer `p` (v0.29.0 hover): in the lane under it, the curve that passes
/// nearest to it vertically at its x, if within `HOVER_REACH`; of curves equally near, the one
/// first in `curves` (the earlier marker, its own curve before its laid-over one). None outside
/// every lane or when no curve is that near. Looks only at the pointer's x of each curve.
fn hit(lanes: &[HitLane<'_>], p: Pos2) -> Option<Curve> {
    let lane = lanes.iter().find(|l| l.rect.contains(p))?;
    let (fa, fb) = (lane.map.frame(p.x - HOVER_HALF_W), lane.map.frame(p.x + HOVER_HALF_W));
    let mut best: Option<(f32, Curve)> = None;
    for &(curve, d) in &lane.curves {
        let Some((top, bottom)) = span_at(d, fa, fb, &lane.map) else { continue };
        let dist = if p.y < top {
            top - p.y
        } else if p.y > bottom {
            p.y - bottom
        } else {
            0.0
        };
        if dist <= HOVER_REACH && best.map_or(true, |(b, _)| dist < b) {
            best = Some((dist, curve));
        }
    }
    best.map(|(_, c)| c)
}

/// Top-left of the hovered marker's name (`size`) for the pointer `p`: `HOVER_TEXT_AT` right of
/// and below it, to the left of it / above it where the name would leave `inside` (the lanes),
/// and never outside `inside` (a name wider than the graph starts at its left).
fn hover_text_at(p: Pos2, size: Vec2, inside: Rect) -> Pos2 {
    let mut at = p + HOVER_TEXT_AT;
    if at.x + size.x > inside.right() - 2.0 {
        at.x = p.x - HOVER_TEXT_FLIP.x - size.x;
    }
    if at.y + size.y > inside.bottom() - 2.0 {
        at.y = p.y - HOVER_TEXT_FLIP.y - size.y;
    }
    pos2(at.x.min(inside.right() - 2.0 - size.x).max(inside.left() + 2.0), at.y.min(inside.bottom() - 2.0 - size.y).max(inside.top() + 2.0))
}

//// Samples of graph work one frame may do (v0.27.0 speed fix, "a frame never over 16 ms"):
/// building a marker's lanes from the file (`panels::LaneCache`) and thinning them for a new
/// range or width (`show`) each cost the marker's samples, about 11 ns a sample each (measured,
/// 122 MB file, 21 708 frames). With 8 markers in each of 4 graphs everything at once took 23 ms
/// in the frame after the selection; now what does not fit comes in the next frames (a frame or
/// a few later). Initial marker and forceplate thinning shares this limit across views and
/// sizing passes; screenshots and captures use no limit.
pub const FRAME_WORK: usize = 128_000;

/// The graph work of the current frame (`WORK`).
#[derive(Clone, Copy)]
struct Work {
    /// The frame (`start_frame`): egui may run the app twice for one frame (a pass whose picture
    /// is thrown away, `Context::request_discard`); both passes share the frame's budget.
    frame: u64,
    /// Samples still allowed (None = no limit: outside `TakViewerApp::frame`, e.g. tests that
    /// draw a graph once).
    left: Option<usize>,
    /// What `left` was at the end of the last pass of the frame.
    kept: Option<usize>,
    /// Work that may wait was done in this frame (`spend`).
    some_done: bool,
    /// Work was put off in this frame, and in the frame before.
    put_off: bool,
    put_off_before: bool,
    mesh_promoted: bool,
    active: bool,
    started: Option<std::time::Instant>,
}

thread_local! {
    static WORK: std::cell::Cell<Work> =
        const { std::cell::Cell::new(Work { frame: u64::MAX, left: None, kept: None, some_done: false, put_off: false, put_off_before: false, mesh_promoted: false, active: false, started: None }) };
}

/// Start of a pass of the app's frame `frame` (the input time's bits: the same for the passes of
/// one frame): `budget` samples of graph work allowed in the frame (`FRAME_WORK`; None = no
/// limit). Returns whether work was put off in the frame before, i.e. a graph drawn then was not
/// complete yet (`--screenshot` waits for that).
pub fn start_frame(frame: u64, budget: Option<usize>) -> bool {
    WORK.with(|w| {
        let mut work = w.get();
        if work.frame == frame {
            work.left = work.kept;
        } else {
            work = Work { frame, left: budget, kept: budget, some_done: false, put_off: false, put_off_before: work.put_off, mesh_promoted: false, active: true, started: Some(std::time::Instant::now()) };
        }
        work.active = true;
        w.set(work);
        work.put_off_before
    })
}

/// End of a pass of the app's frame: no limit outside it (what is left is kept for another pass
/// of the same frame).
pub fn end_frame() {
    WORK.with(|w| {
        let work = w.get();
        w.set(Work { kept: work.left, left: None, active: false, ..work });
    });
}

/// One optional mesh optimization across every view and every pass of an application frame.
/// The complete compact curve is already visible while its promotion waits.
/// Real wall-time budget shared by all views and sizing passes. Work itself is on workers.
fn claim_geometry_publication() -> bool {
    WORK.with(|cell| {
        let work = cell.get();
        !work.active || (!work.mesh_promoted && work.started.is_none_or(|start| start.elapsed().as_secs_f64() < 0.004))
    })
}

fn mark_geometry_published() {
    WORK.with(|cell| {
        let mut work = cell.get();
        if work.active { work.mesh_promoted = true; cell.set(work); }
    });
}

fn mark_geometry_pending() {
    WORK.with(|cell| {
        let mut work = cell.get();
        if work.active { work.put_off = true; cell.set(work); }
    });
}

fn claim_mesh_promotion(created: u64) -> bool {
    WORK.with(|cell| {
        let mut work = cell.get();
        if work.frame == created || work.mesh_promoted { return false; }
        work.mesh_promoted = true;
        cell.set(work);
        true
    })
}

/// May graph work of `cost` samples be done now? `must`: done anyway (counted), for the first
/// marker of a graph. Work that may wait: yes when it fits in what is left of the frame's budget,
/// and the first such work of a frame always (so one marker longer than the budget is still done,
/// one per frame); no = put off to the next frame.
pub fn spend(cost: usize, must: bool) -> bool {
    WORK.with(|w| {
        let mut work = w.get();
        let Some(left) = work.left else { return true };
        let ok = must || cost <= left || !work.some_done;
        if ok {
            work.left = Some(left.saturating_sub(cost));
            work.some_done |= !must;
        } else {
            work.put_off = true;
        }
        w.set(work);
        ok
    })
}
/// Samples `thin` goes through for `lanes` in view `vr` (the cost of thinning them, `spend`).
fn thin_cost(lanes: &[Lane], vr: &ViewRange) -> usize {
    // Counted as a marker's samples, as building its lanes: one lane's (X, Y and Z have the same).
    lanes.first().map_or(0, |l| l.runs.iter().map(|run| in_view(run, vr).len()).sum())
}

/// What `show` worked out for the same lanes, frame range and width, kept by the caller
/// between frames: going through every sample happens once, not every frame, so the work of
/// one frame does not grow with the file length (several graphs can be on screen, one per
/// 3D view). Use a new `Drawn` when the lanes change.
#[derive(Default)]
pub struct Drawn {
    /// Device graphs share source runs and thin them on a worker, including zoom/resize.
    background: bool,
    background_rx: Option<(ThinKey, u64, std::sync::mpsc::Receiver<BackgroundPrepared>)>,
    background_generation: u64,
    background_primary: Vec<Lane>,
    background_sources: Option<BackgroundSources>,
    /// Frame range in view and the number of columns of the lanes below.
    key: Option<ThinKey>,
    lanes: Vec<DrawnLane>,
    /// A second file laid over the view (plan step 9), set by the caller (`set_pair`): its
    /// curves are drawn dashed in the same lanes.
    pair: Option<Pair>,
    /// Its lanes as drawn (`thin`), worked out with `lanes`; empty when it has none.
    pair_lanes: Vec<DrawnLane>,
    /// Tests: what the last `show` painted.
    #[cfg(test)]
    pub drew: Drew,
    #[cfg(test)]
    pub(crate) complete_pass: Option<u64>,
    /// v0.27.0: the other markers of a graph of several (after the one whose lanes are passed to
    /// `show`), each its lanes (X, Y, Z; in the lanes' colours since v0.29.0) (`set_more`).
    /// Empty = one marker.
    more: Vec<Vec<Lane>>,
    /// Their laid-over file's marker of the same name, dashed, in the same order (`set_more_pairs`;
    /// an empty list = that marker has none).
    more_pairs: Vec<Vec<Lane>>,
    /// Both as drawn (`thin`), each marker thinned on its own, a few per frame (`Thinned`).
    more_lanes: Vec<Thinned>,
    more_pair_lanes: Vec<Thinned>,
    /// Counts every change of the thinned points (`ShapesKey::version`).
    version: u64,
    /// Per lane, its curves as painted (`LaneShapes`).
    shapes: Vec<Option<LaneShapes>>,
    mesh_rx: Vec<Option<(ShapesKey, std::sync::mpsc::Receiver<LaneShapes>)>>,
    decorations: Decorations,
    #[cfg(test)]
    force_immediate_mesh: bool,
    #[cfg(test)]
    force_worker_mesh: bool,
    /// v0.29.0 hover: the graphed markers' names in graph order (`set_names`; a marker graph).
    /// Empty = no hover (force plate and device graphs).
    names: Vec<String>,
    /// `--hover` / `--bench-hover` (agent only): as if the pointer were on this marker's own
    /// curve in the Y lane, `FORCED_HOVER_DX` right of the current-frame line.
    hover_force: Option<usize>,
    /// The curve hovered in the last `show` (a repaint is asked for only when it changes).
    hovered: Option<Curve>,
    /// Per lane, the hovered curve drawn thick (`HoverShapes`).
    hover_shapes: Vec<Option<HoverShapes>>,
}

/// The second file laid over a view, for the graph of the selected marker (plan step 9,
/// "겹쳐 보기"): its marker of the same name is drawn as a dashed line of the lane's color under
/// the first file's solid line. Each lane's vertical range covers both curves whether they are
/// shown or hidden, so hiding one (name tag 1 / 2, key 1 / 2) does not move the other.
#[derive(Clone, Default)]
pub struct Pair {
    /// The second file's lanes, in the order of the first file's (X, Y, Z), frames counted as
    /// the first file's; empty when it has no marker of that name (nothing extra is drawn).
    pub lanes: Vec<Lane>,
    /// Shown: [first file (solid), second file (dashed)].
    pub shown: [bool; 2],
}

/// Tests: what one `show` painted.
#[derive(Default, Debug, Clone, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Drew {
    /// Runs drawn solid (first file) and dashed (second file), over all lanes.
    pub solid: usize,
    pub dashed: usize,
    /// Dash pieces drawn.
    pub dashes: usize,
    pub legend: bool,
    /// Per lane: the value range the lane is scaled to (before its padding).
    pub ranges: Vec<(f64, f64)>,
    /// v0.27.0, per lane: the color of each marker's solid curve and of each dashed one drawn.
    pub colors: Vec<Vec<Color32>>,
    pub dash_colors: Vec<Vec<Color32>>,
    /// Comment bands given to `show` (step 6).
    pub bands: usize,
    /// Lanes whose curves were made again in this `show` (`LaneShapes`).
    pub remade: usize,
    /// v0.29.0 hover: the curve under the pointer, the lanes it is drawn thick in, the lanes whose
    /// thick curve was made again in this `show` (`HoverShapes`), and the name painted (its text
    /// and rectangle).
    pub hovered: Option<Curve>,
    pub hover_lanes: usize,
    pub hover_made: usize,
    pub hover_text: Option<(String, Rect)>,
    /// Per lane, where its frames and values are on screen (tests put the pointer on a curve).
    maps: Vec<Option<Map>>,
}

#[cfg(test)]
impl Drew {
    /// Tests: where value `v` at frame `f` is on screen in lane `li`, as drawn (to put the
    /// pointer on a curve).
    pub fn point(&self, li: usize, f: f64, v: f64) -> Option<Pos2> {
        let m = self.maps.get(li).copied().flatten()?;
        Some(pos2(m.x(f), m.y(v)))
    }

    /// Add one lane's counts (`LaneShapes::drew`) to the graph's.
    fn add(&mut self, lane: &Drew) {
        self.solid += lane.solid;
        self.dashed += lane.dashed;
        self.dashes += lane.dashes;
        self.colors.push(lane.colors.first().cloned().unwrap_or_default());
        self.dash_colors.push(lane.dash_colors.first().cloned().unwrap_or_default());
    }
}

impl Drawn {
    /// Initial marker / plate graphs share the same per-frame budget as later curves.
    /// The frame budget survives egui sizing passes, so opening several graphs cannot repeat it.
    fn prepare_primary(&mut self, lanes: &[Lane], vr: ViewRange, cols: usize, key: ThinKey) -> bool {
        if self.key == Some(key) && self.lanes.len() == lanes.len() { return true; }
        let cost = |source: &[Lane]| source.iter().map(|lane| lane.runs.iter().map(|run| in_view(run, &vr).len()).sum::<usize>()).sum::<usize>();
        if !spend(cost(lanes) + self.pair.as_ref().map_or(0, |pair| cost(&pair.lanes)), false) { return false; }
        self.lanes = lanes.iter().map(|lane| thin(lane, &vr, cols)).collect();
        self.pair_lanes = self.pair.as_ref().map_or_else(Vec::new, |pair| pair.lanes.iter().map(|lane| thin(lane, &vr, cols)).collect());
        self.key = Some(key);
        self.version += 1;
        true
    }

    pub fn background() -> Self { Self { background: true, ..Self::default() } }

    /// Replace a device graph's sources without dropping its last complete picture. The
    /// caller passes the new primary lanes to `show`; replacement supplies pair/extra lanes.
    /// The worker publishes their samples and descriptor colors together when ready.
    pub fn retarget_background(&mut self, mut replacement: Drawn) {
        if !replacement.background {
            crate::views::drop_later(Some(std::mem::replace(self, replacement)));
            return;
        }
        // A raw forceplate cache can opt into workers at its first processed result.
        // Ordinary marker caches never change mode unless this method is called explicitly.
        self.background = true;
        let sources = BackgroundSources {
            pair: replacement.pair.take(),
            more: std::mem::take(&mut replacement.more),
            more_pairs: std::mem::take(&mut replacement.more_pairs),
        };
        crate::views::drop_later(self.background_sources.replace(sources));
        self.invalidate_background_source();
        crate::views::drop_later(Some(replacement));
    }

    fn prepare_background(&mut self, lanes: &[Lane], vr: ViewRange, cols: usize, key: ThinKey, ctx: &egui::Context) {
        if let Some((pending_key, pending_generation, rx)) = &self.background_rx {
            match rx.try_recv() {
                Ok(prepared) => {
                    let (prepared_key, prepared_generation) = (*pending_key, *pending_generation);
                    self.background_rx = None;
                    if prepared_key == key && prepared_generation == self.background_generation {
                        let retired = (
                            std::mem::replace(&mut self.lanes, prepared.lanes),
                            std::mem::replace(&mut self.pair_lanes, prepared.pair_lanes),
                            std::mem::replace(&mut self.more_lanes, prepared.more_lanes),
                            std::mem::replace(&mut self.more_pair_lanes, prepared.more_pair_lanes),
                            std::mem::replace(&mut self.background_primary, prepared.primary),
                            std::mem::replace(&mut self.pair, prepared.sources.pair),
                            std::mem::replace(&mut self.more, prepared.sources.more),
                            std::mem::replace(&mut self.more_pairs, prepared.sources.more_pairs),
                            self.background_sources.take(),
                        );
                        crate::views::drop_later(Some(retired));
                        self.key = Some(key);
                        self.version += 1;
                    } else { crate::views::drop_later(Some(prepared)); }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.background_rx = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.key != Some(key) && self.background_rx.is_none() {
            // Lane copies share Arc-backed samples. The UI visits curve descriptors only;
            // all sample scanning, including extrema and overlays, happens on this worker.
            let source = lanes.to_vec();
            let sources = self.background_sources.clone().unwrap_or_else(|| BackgroundSources {
                pair: self.pair.clone(), more: self.more.clone(), more_pairs: self.more_pairs.clone(),
            });
            let wake = ctx.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            self.background_rx = Some((key, self.background_generation, rx));
            std::thread::spawn(move || {
                let all = |sources: &[Vec<Lane>]| sources.iter().map(|lanes| Thinned {
                    key: Some(key), lanes: lanes.iter().map(|lane| thin(lane, &vr, cols)).collect(),
                }).collect();
                let result = BackgroundPrepared {
                    lanes: source.iter().map(|lane| thin(lane, &vr, cols)).collect(),
                    pair_lanes: sources.pair.as_ref().map_or_else(Vec::new, |pair| pair.lanes.iter().map(|lane| thin(lane, &vr, cols)).collect()),
                    more_lanes: all(&sources.more),
                    more_pair_lanes: all(&sources.more_pairs),
                    primary: source,
                    sources,
                };
                let _ = tx.send(result);
                wake.request_repaint();
            });
        }
    }

    fn invalidate_background_source(&mut self) {
        if self.background {
            self.background_generation = self.background_generation.wrapping_add(1);
            self.key = None;
        }
    }

    pub fn enable_background(&mut self) { self.background = true; self.key = None; }

    pub fn background_pending(&self) -> bool {
        self.background && (self.background_rx.is_some() || self.key.is_none() || self.mesh_rx.iter().any(Option::is_some))
    }

    /// The second file laid over the view (None = none); what was worked out is done again.
    pub fn set_pair(&mut self, pair: Option<Pair>) {
        self.invalidate_background_source();
        self.pair = pair;
        self.key = None;
    }

    /// Which of the two files are shown now (cheap, every frame).
    pub fn set_shown(&mut self, shown: [bool; 2]) {
        if let Some(p) = &mut self.pair {
            p.shown = shown;
        }
    }

    /// Tests: the second file's lanes and shown state.
    #[cfg(test)]
    pub fn pair(&self) -> Option<&Pair> {
        self.pair.as_ref()
    }

    /// v0.27.0: the other markers' lanes of a graph of several (`Drawn::more`).
    #[cfg(test)]
    pub fn set_more(&mut self, more: Vec<Vec<Lane>>) {
        self.invalidate_background_source();
        self.more_lanes = more.iter().map(|_| Thinned::default()).collect();
        self.more = more;
        self.version += 1;
    }

    /// One more marker's lanes, after those given (`panels::LaneCache` builds them a few per
    /// frame); thinned and drawn from the next `show` on.
    pub fn push_more(&mut self, lanes: Vec<Lane>) {
        self.invalidate_background_source();
        self.more.push(lanes);
        self.more_lanes.push(Thinned::default());
    }

    /// v0.27.0: their laid-over file's lanes (`Drawn::more_pairs`).
    pub fn set_more_pairs(&mut self, pairs: Vec<Vec<Lane>>) {
        self.invalidate_background_source();
        self.more_pair_lanes = pairs.iter().map(|_| Thinned::default()).collect();
        self.more_pairs = pairs;
        self.version += 1;
    }

    /// The laid-over file's lanes of the next marker (`push_more`).
    pub fn push_more_pair(&mut self, lanes: Vec<Lane>) {
        self.invalidate_background_source();
        self.more_pairs.push(lanes);
        self.more_pair_lanes.push(Thinned::default());
    }

    /// v0.29.0: the graphed markers' names, in graph order (the name painted next to the pointer
    /// over a curve). Set by the marker graph's cache (`panels::LaneCache`) only: a graph without
    /// names has no hover.
    pub fn set_names(&mut self, names: Vec<String>) {
        self.names = names;
    }

    /// `--hover` / `--bench-hover` (agent only): draw the graph as if the pointer were on marker
    /// `k`'s own curve (graph order) in the Y lane, a little right of the current-frame line.
    pub fn set_hover_force(&mut self, k: Option<usize>) {
        self.hover_force = k;
    }

    /// How many more markers' lanes (`set_more` / `push_more`) are here (tests).
    #[cfg(test)]
    pub fn more_len(&self) -> usize {
        self.more.len()
    }

    /// Something is not drawn as it will be: a marker's lanes not thinned for the range and
    /// width of the last `show` yet (put off by `spend`).
    #[cfg(test)]
    pub fn pending(&self) -> bool {
        self.more_lanes.iter().chain(&self.more_pair_lanes).any(|t| t.key != self.key)
    }

    #[cfg(test)]
    pub(crate) fn background_state(&self) -> String {
        let job = self.background_rx.as_ref().map(|(key, generation, _)| (key, generation));
        format!("thinning generation={}, job={job:?}, installed={:?}, mesh jobs={}, painted pass={:?}",
            self.background_generation, self.key,
            self.mesh_rx.iter().filter(|job| job.is_some()).count(), self.complete_pass)
    }
}

/// Dashed line of the second file (plan step 9): dash and gap along the line, in points.
const DASH: f32 = 6.0;
const DASH_GAP: f32 = 4.0;
/// Height kept free at the top of the first lane for the legend (`legend`) when the second
/// file's curves are drawn: the curves stay below it.
const LEGEND_H: f32 = 13.0;

/// The dashes of the line through `pts`: pieces `dash` long with `gap` between them, measured
/// along the line (a dash goes on around a corner), each piece a polyline.
fn dashes(pts: &[Pos2], dash: f32, gap: f32) -> Vec<Vec<Pos2>> {
    let mut out = Vec::new();
    let Some(&first) = pts.first() else { return out };
    let mut piece = vec![first];
    // On a dash, and how far the current dash or gap still goes.
    let (mut on, mut left) = (true, dash);
    for w in pts.windows(2) {
        let (mut a, b) = (w[0], w[1]);
        let mut len = a.distance(b);
        while len > left {
            let p = a + (b - a) * (left / len);
            if on {
                piece.push(p);
                out.push(std::mem::take(&mut piece));
            } else {
                piece.push(p);
            }
            len -= left;
            a = p;
            on = !on;
            left = if on { dash } else { gap };
        }
        left -= len;
        if on {
            piece.push(b);
        } else {
            piece.clear();
        }
    }
    if on && piece.len() > 1 {
        out.push(piece);
    }
    out
}

/// Legend of the two line styles at the top right of the first lane `lr` (in the strip kept
/// free for it): a short solid line and `1`, a short dashed line and `2`, the numbers of the
/// name tags. A hidden file's part is dim, like its name tag. Shapes and two digits, no hover.
fn legend_rect(lr: Rect) -> Rect {
    const SAMPLE: f32 = 18.0;
    let w = 4.0 + SAMPLE + 4.0 + 7.0 + 10.0 + SAMPLE + 4.0 + 7.0 + 4.0;
    Rect::from_min_size(pos2(lr.right() - w - 4.0, lr.top() + 1.0), vec2(w, LEGEND_H - 2.0))
}

fn legend(painter: &egui::Painter, lr: Rect, shown: [bool; 2]) -> Rect {
    const SAMPLE: f32 = 18.0;
    let font = FontId::proportional(11.0);
    let r = legend_rect(lr);
    painter.rect_filled(r, 2.0, Color32::from_black_alpha(120));
    let y = r.center().y;
    let mut x = r.left() + 4.0;
    for (i, on) in shown.into_iter().enumerate() {
        let color = if on { theme::TEXT } else { theme::TEXT.gamma_multiply(0.35) };
        let stroke = Stroke::new(1.4_f32, color);
        let line = [pos2(x, y), pos2(x + SAMPLE, y)];
        if i == 0 {
            painter.line_segment(line, stroke);
        } else {
            for piece in dashes(&line, 5.0, 3.0) {
                painter.add(Shape::line(piece, stroke));
            }
        }
        painter.text(pos2(x + SAMPLE + 4.0, y), Align2::LEFT_CENTER, if i == 0 { "1" } else { "2" }, font.clone(), color);
        x += SAMPLE + 4.0 + 7.0 + 10.0;
    }
    r
}

/// Width of one column of `thin`, in points: the lines are drawn 1.4 wide, so two samples
/// closer than this cannot be told apart, and making the triangles of a line is the
/// expensive part of a frame with four graphs on screen.
const COLUMN_W: f32 = 2.0;

/// Thin a lane out for a plot `cols` columns wide: of the samples that fall into one column
/// only the first, the lowest, the highest and the last are kept (in time order), so a spike
/// is never dropped and the number of points depends on the width (and the number of gaps),
/// not on how many samples the file has. Also the min / max over the frames in view.
fn thin(lane: &Lane, vr: &ViewRange, cols: usize) -> DrawnLane {
    let mut span=crate::bench_timing::span("graph.decimate");span.counts(||(lane.runs.iter().map(Vec::len).sum(),0));
    // (column, first, lowest, highest, last)
    type Column = (i64, [f64; 2], [f64; 2], [f64; 2], [f64; 2]);
    fn flush(out: &mut Vec<[f64; 2]>, (_, first, min, max, last): Column) {
        out.push(first);
        let (a, b) = if min[0] <= max[0] { (min, max) } else { (max, min) };
        for p in [a, b] {
            if p != first && p != last && out.last() != Some(&p) {
                out.push(p);
            }
        }
        if last != first {
            out.push(last);
        }
    }
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let per_frame = cols.max(1) as f64 / (vr.hi - vr.lo);
    let mut runs = Vec::new();
    for run in lane.runs.iter() {
        let mut out = Vec::new();
        let mut cur: Option<Column> = None;
        for p in in_view(run, vr) {
            if p[0] < vr.lo - 1.0 || p[0] > vr.hi + 1.0 || !p[1].is_finite() {
                continue;
            }
            if p[0] >= vr.lo && p[0] <= vr.hi {
                lo = lo.min(p[1]);
                hi = hi.max(p[1]);
            }
            let col = ((p[0] - vr.lo) * per_frame).floor() as i64;
            match &mut cur {
                Some((c, _, min, max, last)) if *c == col => {
                    if p[1] < min[1] {
                        *min = *p;
                    }
                    if p[1] > max[1] {
                        *max = *p;
                    }
                    *last = *p;
                }
                _ => {
                    if let Some(done) = cur.take() {
                        flush(&mut out, done);
                    }
                    cur = Some((col, *p, *p, *p, *p));
                }
            }
        }
        if let Some(done) = cur.take() {
            flush(&mut out, done);
        }
        if !out.is_empty() {
            runs.push(out);
        }
    }
    DrawnLane { lo, hi, runs: runs.into() }
}

/// The points of a run (frames in increasing order) that `thin` looks at: frames in view and one
/// frame on each side. Found by halving, so a zoomed-in graph does not go through the whole file.
fn in_view<'a>(run: &'a [[f64; 2]], vr: &ViewRange) -> &'a [[f64; 2]] {
    let a = run.partition_point(|p| p[0] < vr.lo - 1.0);
    let b = run.partition_point(|p| p[0] <= vr.hi + 1.0);
    &run[a..b.max(a)]
}

/// One lane's curves (`LaneShapes`): the second file's dashed curves first (the marker's, then the
/// other markers'), then the solid ones on top, each in its marker's color, a run of one point as
/// a dot. Also what they are (`Drew`, for tests). The dashes (thousands of little lines with 8
/// markers) are made into triangles here, once, with egui's own tessellator and settings
/// (`tess`, the lane's clip rect). `mesh_single` requests the prior single-marker mesh form;
/// otherwise solid Shape::line points are complete immediately and can be promoted later.
/// `w` / `dot`: line width and one-sample dot radius (`LINE_W`, or `HOVER_W` for a hovered curve).
#[allow(clippy::too_many_arguments)]
fn curve_shapes<'a>(
    tess: &mut egui::epaint::Tessellator,
    dashed: impl Iterator<Item = (&'a DrawnLane, Color32)>,
    solid: impl Iterator<Item = (&'a DrawnLane, Color32)>,
    x_of: impl Fn(f64) -> f32,
    y_of: impl Fn(f64) -> f32,
    w: f32,
    dot: f32,
    mesh_single: bool,
) -> (Vec<Shape>, Drew) {
    let _span=crate::bench_timing::span("graph.build_shapes");
    let mut shapes: Vec<Shape> = Vec::new();
    let mut drew = Drew { colors: vec![vec![]], dash_colors: vec![vec![]], ..Drew::default() };
    let mut mesh = egui::Mesh::default();
    let flush = |mesh: &mut egui::Mesh, shapes: &mut Vec<Shape>| {
        if !mesh.is_empty() {
            shapes.push(Shape::mesh(std::mem::take(mesh)));
        }
    };
    for (d, color) in dashed {
        if !d.runs.is_empty() {
            drew.dash_colors[0].push(color);
        }
        for run in d.runs.iter() {
            let pts: Vec<Pos2> = run.iter().map(|p| pos2(x_of(p[0]), y_of(p[1]))).collect();
            drew.dashed += 1;
            if pts.len() == 1 {
                flush(&mut mesh, &mut shapes);
                shapes.push(Shape::circle_filled(pts[0], dot, color));
                continue;
            }
            for piece in dashes(&pts, DASH, DASH_GAP) {
                drew.dashes += 1;
                tess.tessellate_shape(Shape::line(piece, Stroke::new(w, color)), &mut mesh);
            }
        }
    }
    flush(&mut mesh, &mut shapes);
    let solid: Vec<_> = solid.collect();
    let single_solid = mesh_single && w == LINE_W && solid.iter().filter(|(lane, _)| !lane.runs.is_empty()).count() == 1;
    for (t, color) in solid {
        if !t.runs.is_empty() {
            drew.colors[0].push(color);
        }
        for run in t.runs.iter() {
            drew.solid += 1;
            let pts: Vec<Pos2> = run.iter().map(|p| pos2(x_of(p[0]), y_of(p[1]))).collect();
            if pts.len() == 1 {
                flush(&mut mesh, &mut shapes);
                shapes.push(Shape::circle_filled(pts[0], dot, color));
            } else if single_solid {
                tess.tessellate_shape(Shape::line(pts, Stroke::new(w, color)), &mut mesh);
            } else { shapes.push(Shape::line(pts, Stroke::new(w, color))); }
        }
    }
    flush(&mut mesh, &mut shapes);
    (shapes, drew)
}

/// Returns a frame to jump to when the user clicked / dragged in the graph. Devices and 3D
/// overlays retain scrubbing; the GRAPH panel opts into `show_select` instead.
/// `cache`: see `Drawn` (None = worked out again every frame). `spans`: Comment bands behind
/// the lanes (`Span`; empty = none, the graph as before step 6).
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    id: Id,
    lanes: &[Lane],
    first: i64,
    last: i64,
    frame: i64,
    gaps: &[(i64, i64)],
    lane_h: f32,
    look: &Look,
    cache: Option<&mut Drawn>,
    spans: &[Span],
) -> Option<i64> {
    show_inner(ui, id, lanes, first, last, frame, gaps, lane_h, look, cache, spans, None, None)
}

/// GRAPH panel, including its detached window: a click moves the frame; a primary drag
/// starting in the plot beyond 4 pt selects an inclusive edit range. The bottom navigation
/// strip still scrubs. The caller owns the same range as the playback band.
#[allow(clippy::too_many_arguments)]
pub fn show_select(
    ui: &mut Ui,
    id: Id,
    lanes: &[Lane],
    first: i64,
    last: i64,
    frame: i64,
    gaps: &[(i64, i64)],
    lane_h: f32,
    look: &Look,
    cache: Option<&mut Drawn>,
    spans: &[Span],
    range: &mut Option<(i64, i64)>,
    value_zoom: Option<&mut conveniences::ValueZoom>,
) -> Option<i64> {
    show_inner(ui, id, lanes, first, last, frame, gaps, lane_h, look, cache, spans, Some(range), value_zoom)
}

const RANGE_DRAG_THRESHOLD: f32 = 4.0;

#[derive(Clone, Copy)]
enum RangeDragAxis {
    Any,
    Horizontal,
}

#[derive(Clone, Copy)]
struct RangeDrag {
    origin: Pos2,
    first: i64,
    selecting: bool,
}

/// Track the press ourselves: egui's click/drag threshold is independent of GRAPH's 4 pt
/// rule. Once a drag crosses the threshold it remains a selection even if it returns to
/// the press. Only events between its press and release count; the release owns the final
/// endpoint. Egui does not own a complete press/move/release within one frame, so those
/// unrecognized gestures stay ignored rather than bypassing popup / widget precedence.
#[allow(clippy::too_many_arguments)]
fn select_drag(ui: &mut Ui, resp: &egui::Response, id: Id, plot: Rect, axis: RangeDragAxis, frame_at: impl Fn(f32) -> i64, first: i64, last: i64, range: &mut Option<(i64, i64)>) -> (bool, bool) {
    let id = id.with("range_drag");
    if !ui.is_enabled() {
        ui.data_mut(|data| { data.remove::<RangeDrag>(id); });
        return (false, false);
    }
    let mut drag: Option<RangeDrag> = ui.data(|d| d.get_temp(id));
    // The panel may have been hidden when the button was released. A later pointer move
    // must not resume the abandoned gesture when the panel is shown again.
    if !ui.input(|i| i.pointer.primary_down() || i.pointer.primary_released()) {
        drag = None;
    }
    let owns_press = resp.is_pointer_button_down_on() || resp.clicked_by(egui::PointerButton::Primary) || resp.drag_stopped_by(egui::PointerButton::Primary);
    let mut range_press = drag.is_some();
    let mut selecting = drag.is_some_and(|d| d.selecting);
    let down = ui.input(|i| {
        for event in &i.events {
            match event {
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, .. } => {
                    drag = (owns_press && plot.contains(*pos)).then(|| RangeDrag { origin: *pos, first: frame_at(pos.x), selecting: false });
                    range_press |= drag.is_some();
                }
                egui::Event::PointerMoved(pos) | egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, .. } => {
                    if let Some(drag) = &mut drag {
                        let distance = match axis {
                            RangeDragAxis::Any => drag.origin.distance(*pos),
                            RangeDragAxis::Horizontal => (pos.x - drag.origin.x).abs(),
                        };
                        drag.selecting |= distance > RANGE_DRAG_THRESHOLD;
                        selecting |= drag.selecting;
                        if drag.selecting {
                            *range = Some(super::panels::ordered_range(drag.first, frame_at(pos.x), first, last));
                        }
                    }
                    if matches!(event, egui::Event::PointerButton { .. }) { drag = None; }
                }
                _ => {}
            }
        }
        i.pointer.primary_down()
    });
    ui.data_mut(|d| {
        if let Some(drag) = drag.filter(|_| down) {
            d.insert_temp(id, drag);
        } else {
            d.remove::<RangeDrag>(id);
        }
    });
    (range_press, selecting)
}

#[allow(clippy::too_many_arguments)]
fn show_inner(
    ui: &mut Ui,
    id: Id,
    lanes: &[Lane],
    first: i64,
    last: i64,
    frame: i64,
    gaps: &[(i64, i64)],
    lane_h: f32,
    look: &Look,
    cache: Option<&mut Drawn>,
    spans: &[Span],
    mut range: Option<&mut Option<(i64, i64)>>,
    mut value_zoom: Option<&mut conveniences::ValueZoom>,
) -> Option<i64> {
    let full = ViewRange::full(first, last);
    let mut vr = ViewRange::load(ui, id, full);
    let height = lanes.len() as f32 * lane_h + NAV_H;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    // Register the full hit area in the same pass that displays the pending graph.
    let hit_rect = if lanes.is_empty() {
        Rect::from_min_max(rect.min, rect.max + vec2(0.0, look.pending_plot_height))
    } else { rect };
    let [main, below] = around(hit_rect, look.keep_out);
    let mut resp = ui.interact(main, id.with("plot"), Sense::click_and_drag());
    if below.is_positive() {
        resp = resp.union(ui.interact(below, id.with("plot_below"), Sense::click_and_drag()));
    }
    let plot = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - NAV_H));
    let x_of = |f: f64, vr: &ViewRange| vr.x(plot, f);
    let f_of = |x: f32, vr: &ViewRange| vr.at(plot, x);

    // Interaction.
    let mut goto = None;
    let zoom = resp.hover_pos().and_then(|p| {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        (scroll != 0.0).then_some((p.x, scroll))
    });
    let pan = if resp.dragged_by(egui::PointerButton::Middle) { resp.drag_delta().x } else { 0.0 };
    vr.navigate(full, plot, zoom, pan);
    let mut value_drag_lane = None;
    if let Some(zoom) = value_zoom.as_deref_mut().filter(|_|
        ui.is_enabled() && resp.dragged_by(egui::PointerButton::Secondary)
            && ui.input(|i| i.modifiers.is_none()) && !crate::app::key_focus::keys_blocked(ui.ctx())) {
        if let Some(origin) = ui.input(|i| i.pointer.press_origin()).filter(|p| plot.contains(*p)) {
            let lane = ((origin.y - plot.top()) / lane_h) as usize;
            if lane < lanes.len() {
                zoom.drag(lane, resp.drag_delta().y);
                value_drag_lane = Some(lane);
            }
        }
    }
    let (range_press, selecting) = range.as_deref_mut().map_or((false, false), |range| {
        // The empty panel still paints its navigation strip at the top. That strip scrubs.
        let input_plot = if lanes.is_empty() {
            Rect::from_min_max(rect.left_bottom(), hit_rect.right_bottom() - vec2(0.0, NAV_H))
        } else { plot };
        select_drag(ui, &resp, id, input_plot, RangeDragAxis::Any, |x| f_of(x, &vr).round().clamp(first as f64, last as f64) as i64, first, last, range)
    });
    if !selecting && resp.double_clicked() {
        vr = full;
    } else if !selecting && (resp.clicked_by(egui::PointerButton::Primary) || (!range_press && resp.dragged_by(egui::PointerButton::Primary))) {
        if let Some(p) = resp.interact_pointer_pos() {
            goto = Some(f_of(p.x, &vr).round().clamp(first as f64, last as f64) as i64);
        }
    }
    ui.data_mut(|d| d.insert_temp(id, vr));

    // egui can measure a newly opened graph in a pass whose paint output is discarded.
    // Keep its allocation, interaction state and legend's key boundary, but do not build
    // thinned curves, meshes, text or decorations that the retained pass may replace.
    if ui.is_sizing_pass() || !ui.is_visible() || ui.ctx().will_discard() {
        let with_legend = cache.as_ref().is_some_and(|drawn|
            drawn.pair.as_ref().is_some_and(|pair| !pair.lanes.is_empty())
                || drawn.more_pairs.iter().any(|lanes| !lanes.is_empty()));
        if with_legend && !lanes.is_empty() {
            let rect = legend_rect(Rect::from_min_size(plot.min, vec2(plot.width(), lane_h)));
            crate::app::key_focus::region(ui, rect, crate::app::key_focus::Target::Blocked);
        }
        return goto;
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, look.bg);
    paint_spans(&painter, plot, vr.lo, vr.hi, spans);
    paint_selection(&painter, plot, vr.lo, vr.hi, range.as_deref().copied().unwrap_or(look.selection));

    let cols = (plot.width() / COLUMN_W).round().max(25.0) as usize;
    let key = (vr.lo.to_bits(), vr.hi.to_bits(), cols);
    let mut fresh = Drawn::default();
    let drawn = cache.unwrap_or(&mut fresh);
    drawn.decorations.begin(ui);
    if drawn.background {
        drawn.prepare_background(lanes, vr, cols, key, ui.ctx());
    } else if !drawn.prepare_primary(lanes, vr, cols, key) {
        ui.ctx().request_repaint();
    }
    // A retargeted background graph still paints its complete old descriptor colors until
    // the matching source generation arrives. These shallow lane copies never scan samples.
    let retained_primary;
    let lanes = if drawn.background && !drawn.background_primary.is_empty() {
        retained_primary = drawn.background_primary.clone();
        retained_primary.as_slice()
    } else { lanes };
    // v0.27.0: the other markers of a graph of several, and their dashed lanes, as far as the
    // frame's work allows (`spend`), each marker with its dashed lanes; the others in the next
    // frames (drawn as thinned before, or not yet).
    if !drawn.background {
        let mut put_off = false;
        for k in 0..drawn.more_lanes.len().max(drawn.more_pair_lanes.len()) {
            for (all, thinned) in [(&drawn.more, &mut drawn.more_lanes), (&drawn.more_pairs, &mut drawn.more_pair_lanes)] {
                let (Some(m), Some(t)) = (all.get(k), thinned.get_mut(k)) else { continue };
                if t.key != Some(key) && !put_off {
                    if spend(thin_cost(m, &vr), false) {
                        *t = Thinned { key: Some(key), lanes: m.iter().map(|l| thin(l, &vr, cols)).collect() };
                        drawn.version += 1;
                    } else {
                        put_off = true;
                    }
                }
            }
        }
    }
    // Plan step 9: the second file's dashed lines, and which files are shown.
    let [show_first, show_second] = drawn.pair.as_ref().map_or([true, false], |p| p.shown);
    let with_legend = !drawn.pair_lanes.is_empty() || drawn.more_pair_lanes.iter().any(|m| !m.lanes.is_empty());
    #[cfg(test)]
    let mut drew = Drew { legend: with_legend, bands: spans.len(), ..Drew::default() };
    drawn.shapes.resize_with(lanes.len(), || None);
    drawn.hover_shapes.resize_with(lanes.len(), || None);
    let (pixels_per_point, options) = (ui.ctx().pixels_per_point(), ui.ctx().tessellation_options(|o| *o));
    let decoration_geometry = |rect| DecorationGeometry {
        rect, clip: painter.clip_rect(), plot_x: (plot.left(), plot.width()),
        frames: (vr.lo.to_bits(), vr.hi.to_bits()), pixels_per_point, options,
    };

    // Each lane's place, its value range and where its frames and values are on screen (`Map`),
    // worked out before painting: the hover (v0.29.0) needs them first.
    let lane_rect = |li: usize| Rect::from_min_size(pos2(plot.left(), plot.top() + li as f32 * lane_h), vec2(plot.width(), lane_h));
    let ranges: Vec<(f64, f64)> = (0..lanes.len().min(drawn.lanes.len())).map(|li| {
        let bounds = lane_range(drawn, li);
        value_zoom.as_ref().map_or(bounds, |zoom| zoom.bounds(li, bounds))
    }).collect();
    let maps: Vec<Option<Map>> = ranges
        .iter()
        .enumerate()
        .map(|(li, &(lo, hi))| {
            let lr = lane_rect(li);
            let pad = ((hi - lo) * 0.08).max(1e-6);
            let keep = if li == 0 && with_legend { LEGEND_H.min(lr.height() * 0.3) } else { 0.0 };
            lo.is_finite().then_some(Map { left: plot.left(), width: plot.width(), lo: vr.lo, hi: vr.hi, bottom: lr.bottom(), span: lr.height() - 6.0 - keep, ylo: lo - pad, yhi: hi + pad })
        })
        .collect();

    // v0.29.0: the curve under the mouse, in a marker graph (it has the markers' names). Only the
    // pointer's place: no click or drag is taken, and none while a mouse button is down or
    // something is being dragged (scrubbing, the floating graph moved or resized).
    let pointer = if drawn.names.is_empty() {
        None
    } else if let Some(k) = drawn.hover_force {
        forced_pointer(drawn, &maps, k, frame, plot)
    } else if ui.input(|i| i.pointer.any_down()) || ui.ctx().dragged_id().is_some() {
        None
    } else {
        resp.hover_pos()
    };
    let hovered = pointer.and_then(|p| {
        let hit_lanes: Vec<HitLane> = maps
            .iter()
            .enumerate()
            .filter_map(|(li, m)| {
                let rect = lane_rect(li);
                // Only the lane under the pointer is looked into (a few curves, not every lane).
                let curves = if rect.contains(p) { curves_in(drawn, li, [show_first, show_second]) } else { Vec::new() };
                Some(HitLane { rect, map: (*m)?, curves })
            })
            .collect();
        // A forced hover is of its marker even where another curve lies on the same spot (a
        // real pointer there would get the earlier marker).
        drawn.hover_force.map(|k| Curve { marker: k, laid_over: false }).or_else(|| hit(&hit_lanes, p))
    });
    // A repaint only when what is hovered changes (the mouse moving repaints anyway).
    if hovered != drawn.hovered {
        drawn.hovered = hovered;
        ui.ctx().request_repaint();
    }

    for (li, (lane, thinned)) in lanes.iter().zip(&drawn.lanes).enumerate() {
        let lr = lane_rect(li);
        let dashed = drawn.pair_lanes.get(li);
        let (lo, hi) = ranges[li];
        drawn.decorations.paint_lane(&painter, LaneDecorationKey {
            geometry: decoration_geometry(lr), index: li, lane_h,
            range: (lo.to_bits(), hi.to_bits()), values: look.values, tag_w: look.tag_w, watermark: look.watermark,
        }, lane);
        #[cfg(test)]
        drew.ranges.push((lo, hi));
        let Some(map) = maps[li] else {
            #[cfg(test)]
            drew.add(&Drew::default());
            continue;
        };
        let (ylo, yhi) = (map.ylo, map.yhi);
        let keep = if li == 0 && with_legend { LEGEND_H.min(lr.height() * 0.3) } else { 0.0 };
        let y_of = |v: f64| lr.bottom() - 3.0 - ((v - ylo) / (yhi - ylo)) as f32 * (lr.height() - 6.0 - keep);
        let lane_painter = painter.with_clip_rect(lr);
        // Tint before tessellation can round AA colours differently from tinting an existing
        // mesh. Such painters retain the prior immediate single-marker mesh representation.
        let untinted = ui.is_enabled() && lane_painter.opacity() == 1.0;
        let immediate_mesh = !untinted;
        #[cfg(test)] let immediate_mesh = immediate_mesh || drawn.force_immediate_mesh;
        let shapes_key = ShapesKey {
            version: drawn.version,
            lane: lr,
            x: (vr.lo, vr.hi),
            y: (ylo, yhi, keep),
            shown: [show_first, show_second],
            clip: lane_painter.clip_rect(),
            pixels_per_point,
            options,
            untinted,
        };
        // During a value drag, keep one mesh request for this time extent and lane layout.
        // Each pointer step transforms the prepared picture instead of replacing its job.
        let mut mesh_key = shapes_key.clone();
        if value_drag_lane == Some(li) {
            let pending = drawn.mesh_rx.get(li).and_then(Option::as_ref).map(|(key, _)| key);
            let prepared = drawn.shapes[li].as_ref().map(|shapes| &shapes.key);
            if let Some(key) = pending.into_iter().chain(prepared).find(|key| key.same_except_value(&shapes_key)) {
                mesh_key.y = key.y;
            }
        }
        let worker_mesh = untinted && !cfg!(test);
        #[cfg(test)] let worker_mesh = worker_mesh || (untinted && drawn.force_worker_mesh);
        if drawn.mesh_rx.len() <= li { drawn.mesh_rx.resize_with(li + 1, || None); }
        if worker_mesh {
            if drawn.mesh_rx[li].as_ref().is_some_and(|(key, _)| *key != mesh_key) {
                let _span = crate::bench_timing::span("graph.discard_worker_mesh");
                crate::views::drop_later(drawn.mesh_rx[li].take());
            }
            if claim_geometry_publication() {
                if let Some((_, rx)) = &drawn.mesh_rx[li] {
                    if let Ok(ready) = rx.try_recv() {
                        mark_geometry_published();
                        let _timing = crate::bench_timing::span("graph.install_worker_mesh");
                        crate::views::drop_later(drawn.shapes[li].replace(ready));
                        drawn.mesh_rx[li] = None;
                    }
                }
            }
            if drawn.shapes[li].as_ref().map(|m| &m.key) != Some(&mesh_key) && drawn.mesh_rx[li].is_none() {
                let _span = crate::bench_timing::span("graph.queue_worker_mesh");
                let dashed: Vec<_> = dashed.map(|d| (d.clone(),lane.color)).into_iter()
                    .chain(drawn.more_pair_lanes.iter().zip(&drawn.more).filter_map(|(p,m)| Some((p.lanes.get(li)?.clone(),m.get(li)?.color))))
                    .filter(|_|show_second).collect();
                let solid: Vec<_> = std::iter::once((thinned.clone(),lane.color))
                    .chain(drawn.more_lanes.iter().zip(&drawn.more).filter_map(|(p,m)| Some((p.lanes.get(li)?.clone(),m.get(li)?.color))))
                    .filter(|_|show_first).collect();
                let key = mesh_key.clone();
                let map = Map { ylo: key.y.0, yhi: key.y.1, ..map };
                let (tx,rx) = std::sync::mpsc::channel();
                let wake = ui.ctx().clone();
                drawn.mesh_rx[li] = Some((key.clone(),rx));
                crate::work_queue::geometry(move || {
                    let mut tess = egui::epaint::Tessellator::new(key.pixels_per_point,key.options,[1,1],vec![]);
                    tess.set_clip_rect(key.clip);
                    let (shapes,_drew) = curve_shapes(&mut tess, dashed.iter().map(|(d,c)|(d,*c)),solid.iter().map(|(d,c)|(d,*c)), |f|map.x(f), |v|map.y(v),LINE_W,DOT_R,true);
                    let mut ready = LaneShapes {key,shapes:std::sync::Arc::new(shapes),stream:None,promote_after:None, #[cfg(test)] drew:_drew};
                    ready.promote();
                    ready.stream();
                    let _ = tx.send(ready); wake.request_repaint();
                });
            }
            if drawn.mesh_rx[li].is_some() {
                mark_geometry_pending();
                ui.ctx().request_repaint();
            }
        }
        if !worker_mesh && drawn.shapes[li].as_ref().map(|m| &m.key) != Some(&mesh_key) {
            // The second file's curves first, dashed, so the first file's solid lines are on
            // top: the (first) marker's, then the other markers' of a graph of several
            // (v0.27.0), all in the lane's color (v0.29.0; v0.27.0 had a color per marker).
            let more_dashed = drawn.more_pair_lanes.iter().zip(&drawn.more).filter_map(|(p, m)| Some((p.lanes.get(li)?, m.get(li)?.color)));
            let dashed = dashed.map(|d| (d, lane.color)).into_iter().chain(more_dashed).filter(|_| show_second);
            let more_solid = drawn.more_lanes.iter().zip(&drawn.more).filter_map(|(t, m)| Some((t.lanes.get(li)?, m.get(li)?.color)));
            let solid = std::iter::once((thinned, lane.color)).chain(more_solid).filter(|_| show_first);
            // The dashes' triangles as egui makes them for this lane (`curve_shapes`).
            let mut tess = egui::epaint::Tessellator::new(pixels_per_point, options, [1, 1], vec![]);
            tess.set_clip_rect(shapes_key.clip);
            let mesh_map = Map { ylo: mesh_key.y.0, yhi: mesh_key.y.1, ..map };
            let (shapes, _lane_drew) = curve_shapes(&mut tess, dashed, solid, |f| x_of(f, &vr), |v| mesh_map.y(v), LINE_W, DOT_R, immediate_mesh);
            let promote_after = (!immediate_mesh
                && shapes.iter().any(|shape| matches!(shape, Shape::Path(_))))
                .then(|| WORK.with(|work| work.get().frame));
            drawn.shapes[li] = Some(LaneShapes {
                key: mesh_key,
                shapes: std::sync::Arc::new(shapes),
                stream: None,
                promote_after,
                #[cfg(test)]
                drew: _lane_drew,
            });
            #[cfg(test)]
            {
                drew.remade += 1;
            }
        }
        if let Some(cached) = drawn.shapes[li].as_mut() {
            if let Some(created) = cached.promote_after {
                if immediate_mesh || claim_mesh_promotion(created) { cached.promote(); }
                else { ui.ctx().request_repaint(); }
            }
        }
        if let Some(m) = &mut drawn.shapes[li] {
            let value_transform = (value_zoom.is_some() && m.key.y != shapes_key.y && m.key.same_except_value(&shapes_key))
                .then(|| m.key.value_transform(&shapes_key));
            let _preview = value_transform.map(|_| crate::bench_timing::span("graph.preview_value_zoom"));
            if m.key.y != shapes_key.y && value_transform.is_none() {
                let _span = crate::bench_timing::span("graph.curve_extent_pending");
            }
            let mut span=crate::bench_timing::span("graph.publish_shapes");
            span.counts(||(m.shapes.iter().map(|s|if let Shape::Path(p)=s{p.points.len()}else{0}).sum(),m.shapes.iter().map(|s|if let Shape::Mesh(m)=s{m.vertices.len()}else{0}).sum()));
            m.paint(&lane_painter, value_transform);
            #[cfg(test)]
            drew.add(&m.drew);
        }
        // v0.29.0: the hovered marker's curve again over the lane's curves, thicker and lighter
        // (the lane's shapes stay as they are). Made again only for another curve or when the
        // lane's shapes change: one curve of one lane, a small part of what the lane costs.
        let Some(c) = hovered else { continue };
        let kept = drawn.hover_shapes[li].as_ref().is_some_and(|h| h.curve == c && h.key == shapes_key);
        if !kept {
            let made = curves_in(drawn, li, [show_first, show_second]).into_iter().find(|(x, _)| *x == c).and_then(|(_, d)| {
                // Done at once (as the first marker), counted in the frame's graph work by its
                // points on screen (at most four per 2 pt column, not the file's samples).
                spend(d.runs.iter().map(Vec::len).sum(), true);
                let light = theme::lerp(curve_color(drawn, lanes, c.marker, li)?, Color32::WHITE, HOVER_LIGHTEN);
                let mut tess = egui::epaint::Tessellator::new(pixels_per_point, options, [1, 1], vec![]);
                tess.set_clip_rect(shapes_key.clip);
                let one = std::iter::once((d, light));
                let none = std::iter::empty();
                let (shapes, _) = if c.laid_over {
                    curve_shapes(&mut tess, one, none, |f| x_of(f, &vr), y_of, HOVER_W, HOVER_DOT_R, false)
                } else {
                    curve_shapes(&mut tess, none, one, |f| x_of(f, &vr), y_of, HOVER_W, HOVER_DOT_R, false)
                };
                Some(shapes)
            });
            drawn.hover_shapes[li] = made.map(|shapes| HoverShapes { key: shapes_key.clone(), curve: c, shapes });
            #[cfg(test)]
            {
                drew.hover_made += 1;
            }
        }
        if let Some(h) = drawn.hover_shapes[li].as_ref().filter(|h| h.curve == c && h.key == shapes_key) {
            lane_painter.extend(h.shapes.iter().cloned());
            #[cfg(test)]
            {
                drew.hover_lanes += 1;
            }
        }
    }

    // Frame bar with gap marks and tick labels.
    let nav = Rect::from_min_max(pos2(rect.left(), rect.bottom() - NAV_H), rect.max);
    drawn.decorations.prepare_nav(&painter, NavDecorationKey { geometry: decoration_geometry(nav), bg: look.nav_bg });
    painter.add(drawn.decorations.nav.as_ref().unwrap().background.clone());
    for &(a, b) in gaps {
        let (fa, fb) = (a as f64 - 0.5, b as f64 + 0.5);
        if fb < vr.lo || fa > vr.hi {
            continue;
        }
        let r = Rect::from_min_max(
            pos2(x_of(fa.max(vr.lo), &vr), nav.top() + 2.0),
            pos2(x_of(fb.min(vr.hi), &vr).max(x_of(fa.max(vr.lo), &vr) + 1.0), nav.bottom() - 2.0),
        );
        painter.rect_filled(r, 0.0, theme::NAV_GAP);
    }
    painter.extend(drawn.decorations.nav.as_ref().unwrap().ticks.iter().cloned());
    let cx = x_of(frame as f64, &vr);
    if cx >= plot.left() && cx <= plot.right() {
        painter.line_segment([pos2(cx, rect.top()), pos2(cx, nav.top())], Stroke::new(1.0_f32, theme::GRAPH_CURSOR));
    }
    // Plan step 9: which line is which file, over everything (only with the dashed curves).
    if with_legend && !lanes.is_empty() {
        let legend_rect = legend(&painter, Rect::from_min_size(plot.min, vec2(plot.width(), lane_h)), [show_first, show_second]);
        crate::app::key_focus::region(ui, legend_rect, crate::app::key_focus::Target::Blocked);
    }
    // v0.29.0: the hovered marker's name next to the pointer, plain small text (no box: the
    // user's edit of mockup ①), `· 2` for the laid-over file's curve; inside the lanes.
    if let (Some(c), Some(p)) = (hovered, pointer) {
        if let Some(name) = drawn.names.get(c.marker) {
            let text = if c.laid_over { format!("{name} · 2") } else { name.clone() };
            let galley = painter.layout_no_wrap(text, FontId::proportional(HOVER_FONT), theme::HUD_TEXT);
            let at = hover_text_at(p, galley.size(), plot);
            #[cfg(test)]
            {
                drew.hover_text = Some((galley.text().to_owned(), Rect::from_min_size(at, galley.size())));
            }
            painter.galley(at, galley, theme::HUD_TEXT);
        }
    }
    #[cfg(test)]
    {
        drew.hovered = hovered;
        drew.maps = maps;
        drawn.drew = drew;
        drawn.complete_pass = (!lanes.is_empty() && drawn.key == Some(key)
            && !drawn.pending() && drawn.background_rx.is_none()
            && drawn.mesh_rx.iter().all(Option::is_none))
            .then(|| ui.ctx().cumulative_pass_nr());
    }
    goto
}

/// Value range of lane `li` (before its padding): one over every curve drawn in it, both files
/// (plan step 9) and every marker of a graph of several (v0.27.0), shown or not.
fn lane_range(drawn: &Drawn, li: usize) -> (f64, f64) {
    let thinned = &drawn.lanes[li];
    let (lo, hi) = drawn.pair_lanes.get(li).map_or((thinned.lo, thinned.hi), |d| (thinned.lo.min(d.lo), thinned.hi.max(d.hi)));
    let others = drawn.more_lanes.iter().chain(&drawn.more_pair_lanes).filter_map(|m| m.lanes.get(li));
    others.fold((lo, hi), |(lo, hi), d| (lo.min(d.lo), hi.max(d.hi)))
}

/// The curves drawn in lane `li` (v0.29.0 hover), in graph order, each marker's own curve
/// (`shown[0]`) before its laid-over one (`shown[1]`). A marker whose lanes are not built or
/// not thinned yet (put off to a later frame) has none.
fn curves_in(d: &Drawn, li: usize, shown: [bool; 2]) -> Vec<(Curve, &DrawnLane)> {
    let mut out = Vec::new();
    for k in 0..d.names.len() {
        let (own, over) = if k == 0 {
            (d.lanes.get(li), d.pair_lanes.get(li))
        } else {
            (d.more_lanes.get(k - 1).and_then(|t| t.lanes.get(li)), d.more_pair_lanes.get(k - 1).and_then(|t| t.lanes.get(li)))
        };
        for (lane, laid_over, on) in [(own, false, shown[0]), (over, true, shown[1])] {
            if let (Some(lane), true) = (lane, on) {
                out.push((Curve { marker: k, laid_over }, lane));
            }
        }
    }
    out
}

/// Color of marker `k`'s curves in lane `li` (both files): the lane's.
fn curve_color(d: &Drawn, lanes: &[Lane], k: usize, li: usize) -> Option<Color32> {
    if k == 0 {
        lanes.get(li).map(|l| l.color)
    } else {
        d.more.get(k - 1)?.get(li).map(|l| l.color)
    }
}

/// `--hover` / `--bench-hover`: where the pointer is put for marker `k` (`Drawn::hover_force`):
/// on its own curve in the Y lane (the middle one), `FORCED_HOVER_DX` right of the
/// current-frame line. None when that curve is not drawn there.
fn forced_pointer(drawn: &Drawn, maps: &[Option<Map>], k: usize, frame: i64, plot: Rect) -> Option<Pos2> {
    let li = 1.min(maps.len().checked_sub(1)?);
    let m = maps[li]?;
    let x = (m.x(frame as f64) + FORCED_HOVER_DX).clamp(plot.left() + 1.0, plot.right() - 1.0);
    let (_, own) = curves_in(drawn, li, [true, false]).into_iter().find(|(c, _)| c.marker == k)?;
    let (top, bottom) = span_at(own, m.frame(x - HOVER_HALF_W), m.frame(x + HOVER_HALF_W), &m)?;
    Some(pos2(x, (top + bottom) / 2.0))
}

#[cfg(test)]
mod tests {
    use super::{nice_step, thin, Lane, ViewRange};
    use eframe::egui::Color32;

    include!("graph_selection_tests.rs");

    struct DecorationGraph {
        ctx: eframe::egui::Context,
        drawn: super::Drawn,
        lanes: Vec<Lane>,
        textures: crate::soft_render::Textures,
        size: eframe::egui::Vec2,
        offset: eframe::egui::Vec2,
        clip: Option<eframe::egui::Rect>,
        look: super::Look,
        lane_h: f32,
        frame: i64,
        gaps: Vec<(i64, i64)>,
        spans: Vec<super::Span>,
        enabled: bool,
        opacity: f32,
        pass: u64,
        sizing: bool,
        invisible: bool,
        discard_first: bool,
        skipped_passes: usize,
        allocated: eframe::egui::Vec2,
        goto: Option<i64>,
    }

    impl DecorationGraph {
        fn new(uncached: bool) -> Self {
            let mut drawn = four_markers();
            drawn.set_names(vec!["M".into(), "N".into(), "P".into(), "Q".into()]);
            drawn.decorations.force_uncached = uncached;
            Self {
                ctx: Default::default(), drawn, lanes: flat(Color32::RED, 1.0), textures: Default::default(),
                size: eframe::egui::vec2(420.0, 260.0), offset: eframe::egui::Vec2::ZERO, clip: None,
                look: super::Look::pane(), lane_h: 60.0, frame: 100, gaps: vec![], spans: vec![],
                enabled: true, opacity: 1.0, pass: 0,
                sizing: false, invisible: false, discard_first: false, skipped_passes: 0,
                allocated: eframe::egui::Vec2::ZERO, goto: None,
            }
        }

        fn render(&mut self, events: Vec<eframe::egui::Event>) -> (Vec<(eframe::egui::Rect, eframe::egui::Mesh)>, eframe::egui::ColorImage) {
            use crate::app::key_focus::{self, Target};
            use eframe::egui;
            let ctx = self.ctx.clone();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
                time: Some(self.pass as f64), events, focused: true, ..Default::default()
            };
            super::start_frame(self.pass, None);
            self.pass += 1;
            let out = ctx.run(input, |ctx| {
                key_focus::begin_frame(ctx, false, Target::Markers(0));
                egui::CentralPanel::default().show(ctx, |ui| {
                    let rect = egui::Rect::from_min_max(ui.max_rect().min + self.offset, ui.max_rect().max);
                    let mut builder = egui::UiBuilder::new().max_rect(rect);
                    if self.sizing { builder = builder.sizing_pass(); }
                    if self.invisible || self.sizing { builder = builder.invisible(); }
                    let mut child = ui.new_child(builder);
                    child.set_clip_rect(ui.clip_rect().intersect(self.clip.unwrap_or(ui.clip_rect())));
                    if !self.enabled { child.disable(); }
                    child.set_opacity(self.opacity);
                    key_focus::region(&child, rect, Target::Markers(0));
                    if self.discard_first && ctx.output(|output| output.num_completed_passes) == 0 {
                        ctx.request_discard("graph test discarded measurement pass");
                        assert!(ctx.will_discard());
                    }
                    let skip = self.sizing || !child.is_visible() || ctx.will_discard();
                    let cache_state = |drawn: &super::Drawn| (drawn.key, drawn.version, drawn.lanes.len(),
                        drawn.shapes.len(), drawn.hover_shapes.len(), drawn.decorations.builds);
                    let before = cache_state(&self.drawn);
                    let atlas = self.drawn.decorations.atlas.clone();
                    self.goto = super::show(&mut child, egui::Id::new("decoration_graph"), &self.lanes, 0, 3999,
                        self.frame, &self.gaps, self.lane_h, &self.look, Some(&mut self.drawn), &self.spans);
                    self.allocated = child.min_rect().size();
                    if skip {
                        self.skipped_passes += 1;
                        assert_eq!(cache_state(&self.drawn), before, "a skipped pass must not build or replace curve/decor caches");
                        assert!(atlas.ptr_eq(&self.drawn.decorations.atlas));
                    }
                });
                key_focus::finish_frame(ctx);
            });
            super::end_frame();
            self.textures.set(&out.textures_delta);
            let primitives = ctx.tessellate(out.shapes, out.pixels_per_point);
            let geometry = primitives.iter().map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => (primitive.clip_rect, mesh.clone()),
                _ => panic!("graph must not add a backend-specific callback"),
            }).collect();
            let size = [(self.size.x * out.pixels_per_point).round() as usize, (self.size.y * out.pixels_per_point).round() as usize];
            let image = crate::soft_render::paint(&primitives, &self.textures, size, out.pixels_per_point, Color32::BLACK);
            (geometry, image)
        }
    }

    fn promotion_graph(immediate: bool) -> DecorationGraph {
        let mut graph = DecorationGraph::new(false);
        graph.drawn = super::Drawn::default();
        graph.drawn.force_immediate_mesh = immediate;
        graph.drawn.set_names(vec!["M".into()]);
        graph.lanes = (0..3).map(|axis| {
            let value = |frame: i64| (frame as f64 * 0.013).sin() * (axis + 1) as f64;
            Lane {
                letter: ["X", "Y", "Z"][axis].into(),
                color: Color32::from_rgba_unmultiplied(210, 70 + axis as u8 * 50, 35, 173),
                runs: vec![
                    (0..40).map(|f| [f as f64, value(f)]).collect(),
                    vec![[66.0, value(66)]],
                    (100..4000).map(|f| [f as f64, value(f)]).collect(),
                ].into(), unit: "mm",
            }
        }).collect();
        graph.drawn.set_pair(Some(super::Pair { lanes: flat(Color32::GREEN, -1.5), shown: [true, true] }));
        graph
    }

    #[test]
    fn worker_geometry_and_owned_frame_copies_match_reference_pixels() {
        let mut reference = promotion_graph(true);
        let mut worker = promotion_graph(true);
        worker.drawn.force_worker_mesh = true;
        worker.pass = 1_000_000;
        let watchdog = crate::work_queue::TestWatchdog::new();
        let (a, b) = loop {
            let a = reference.render(vec![]);
            let b = worker.render(vec![]);
            if worker.drawn.complete_pass == Some(worker.ctx.cumulative_pass_nr() - 1) {
                break (a, b);
            }
            watchdog.pause(|| format!("worker must publish every lane. {}", worker.drawn.background_state()));
        };
        assert_eq!(a.0,b.0,"worker tessellation preserves every vertex and index");
        assert_eq!(a.1.pixels,b.1.pixels,"queued owned copies preserve exact pixels");
        let ready = worker.drawn.shapes.iter().filter(|s| s.is_some()).count() == 3
            && worker.drawn.mesh_rx.iter().all(Option::is_none);
        assert!(ready, "worker must publish every lane");
        for _ in 0..5 {
            let a=reference.render(vec![]);let b=worker.render(vec![]);
            assert_eq!(a.0,b.0);assert_eq!(a.1.pixels,b.1.pixels);
        }
    }

    #[test]
    fn painter_clipping_matches_coarse_culling_for_graph_pixels() {
        let mut culled = promotion_graph(true);
        let mut clipped = promotion_graph(true);
        clipped.ctx.tessellation_options_mut(|o| o.coarse_tessellation_culling = false);
        let clip = eframe::egui::Rect::from_min_max(eframe::egui::pos2(30.0, 20.0), eframe::egui::pos2(250.0, 140.0));
        culled.clip = Some(clip);
        clipped.clip = Some(clip);
        for _ in 0..3 {
            let (_, a) = culled.render(vec![]);
            let (_, b) = clipped.render(vec![]);
            assert_eq!(a.pixels, b.pixels, "painter clip preserves graph pixels");
        }
    }

    #[test]
    fn geometry_publication_obeys_wall_clock_budget_across_views() {
        let frame=0x123456;
        super::start_frame(frame,Some(100));
        super::WORK.with(|w| {let mut state=w.get();state.started=Some(std::time::Instant::now()-std::time::Duration::from_millis(5));w.set(state);});
        assert!(!super::claim_geometry_publication());
        super::start_frame(frame,Some(100));
        assert!(!super::claim_geometry_publication(),"a second layout pass cannot reset the wall budget");
        super::start_frame(frame+1,Some(100));
        assert!(super::claim_geometry_publication());super::end_frame();
    }

    #[test]
    fn worker_publication_is_shared_across_views_and_layout_passes() {
        let frame = 0x654321;
        super::start_frame(frame, Some(100));
        assert!(super::claim_geometry_publication());
        // A pending receiver does not consume the slot until a mesh actually arrives.
        assert!(super::claim_geometry_publication());
        super::mark_geometry_published();
        assert!(!super::claim_geometry_publication());
        super::end_frame();
        assert!(super::claim_geometry_publication(), "standalone graph render is unlimited");
        super::start_frame(frame, Some(100));
        assert!(!super::claim_geometry_publication(), "another sizing pass shares the slot");
        super::start_frame(frame + 1, Some(100));
        assert!(super::claim_geometry_publication(), "next frame gets a new slot");
        super::end_frame();
    }

    #[test]
    fn pending_mesh_keeps_screenshot_waiting_until_a_later_frame() {
        let frame = 0x765432;
        super::start_frame(frame, None);
        super::mark_geometry_pending();
        super::end_frame();
        assert!(super::start_frame(frame + 1, None));
        super::end_frame();
        assert!(!super::start_frame(frame + 2, None));
        super::end_frame();
    }

    fn pending_promotions(graph: &DecorationGraph) -> usize {
        graph.drawn.shapes.iter().filter(|lane| lane.as_ref().is_some_and(|lane| lane.promote_after.is_some())).count()
    }

    #[test]
    fn first_full_paths_and_bounded_promotions_match_immediate_mesh_geometry_and_pixels() {
        let mut delayed = promotion_graph(false);
        let mut reference = promotion_graph(true);
        let mut version = None;
        let mut points = Vec::new();
        for frame in 0..5 {
            for graph in [&mut delayed, &mut reference] {
                graph.frame = 100 + frame;
                if frame == 1 { graph.drawn.set_hover_force(Some(0)); }
                if frame == 4 { graph.drawn.set_hover_force(None); }
            }
            let a = delayed.render(vec![]);
            let b = reference.render(vec![]);
            assert_eq!(a.0, b.0, "first/full/promoted triangles must remain exact at frame {frame}");
            assert_eq!(a.1.pixels, b.1.pixels, "first/full/promoted pixels must remain exact at frame {frame}");
            assert_eq!(delayed.drawn.drew.solid, 9, "every run and dot is present on the first frame too");
            assert_eq!(pending_promotions(&delayed), 3usize.saturating_sub(frame as usize));
            if frame == 0 {
                assert!(delayed.drawn.shapes.iter().all(|lane| lane.as_ref().unwrap().shapes.iter().any(|shape| matches!(shape, eframe::egui::Shape::Path(_)))));
                version = Some(delayed.drawn.version);
                points = delayed.drawn.lanes.iter().flat_map(|lane| lane.runs.iter()).map(|run| run.as_ptr()).collect();
            } else {
                assert_eq!(delayed.drawn.version, version.unwrap(), "promotion must not rebuild or rethin source lanes");
                assert_eq!(delayed.drawn.lanes.iter().flat_map(|lane| lane.runs.iter()).map(|run| run.as_ptr()).collect::<Vec<_>>(), points);
                assert_eq!(delayed.drawn.drew.remade, 0);
            }
            if matches!(frame, 2 | 3) { assert_eq!(delayed.drawn.drew.hover_made, 0, "promotion must retain hover caches"); }
        }
    }

    #[test]
    fn multiple_marker_and_device_overlay_promotions_preserve_all_geometry_and_pixels() {
        for curves in [2, 8] {
            let mut delayed = promotion_graph(false);
            let mut reference = promotion_graph(false);
            for graph in [&mut delayed, &mut reference] {
                for k in 1..curves {
                    let mut lanes = graph.lanes.clone();
                    for lane in &mut lanes {
                        lane.color = lane.color.gamma_multiply(if k % 2 == 0 { 0.22 } else { 0.7 });
                    }
                    graph.drawn.push_more(lanes);
                }
            }
            for frame in 0..8 {
                for graph in [&mut delayed, &mut reference] {
                    if frame == 5 {graph.enabled=false;}
                    if frame == 6 {graph.enabled=true;graph.opacity=0.55;}
                    if frame == 7 {graph.opacity=1.0;}
                }
                // Keep the comparison graph on its complete compact first-frame paths.
                reference.pass = 0;
                let b = reference.render(vec![]);
                delayed.frame = reference.frame;
                let a = delayed.render(vec![]);
                assert!(a.0 == b.0, "geometry: curves={curves}, frame={frame}");
                assert!(a.1.pixels == b.1.pixels, "pixels: curves={curves}, frame={frame}");
                if frame < 5 {assert_eq!(pending_promotions(&delayed), 3usize.saturating_sub(frame));}
            }
        }
    }

    #[test]
    fn single_marker_tint_uses_prior_mesh_before_first_paint_and_on_tint_change() {
        use eframe::egui::{pos2, Rect};
        for (enabled, opacity) in [(false, 1.0), (true, 0.55), (false, 0.55)] {
            for tint_on_first_frame in [false, true] {
                let mut delayed = promotion_graph(false);
                let mut reference = promotion_graph(true);
                for graph in [&mut delayed, &mut reference] {
                    graph.ctx.set_pixels_per_point(1.5);
                    graph.clip = Some(Rect::from_min_max(pos2(24.25, 31.5), pos2(370.5, 220.25)));
                }
                if !tint_on_first_frame {
                    let a = delayed.render(vec![]);
                    let b = reference.render(vec![]);
                    assert_eq!(a.0, b.0); assert_eq!(a.1.pixels, b.1.pixels);
                    assert_eq!(pending_promotions(&delayed), 3);
                }
                for graph in [&mut delayed, &mut reference] { graph.enabled = enabled; graph.opacity = opacity; }
                let a = delayed.render(vec![]);
                let b = reference.render(vec![]);
                assert_eq!(a.0, b.0, "AA triangles/colours must preserve the previous tint order");
                assert_eq!(a.1.pixels, b.1.pixels, "AA pixels must preserve the previous tint order");
                assert_eq!(pending_promotions(&delayed), 0, "non-normal painters must use the prior mesh before painting");
            }
        }
    }

    #[test]
    fn promotion_is_one_lane_globally_per_application_frame_including_repeated_passes() {
        let mut a = promotion_graph(false);
        let mut b = promotion_graph(false);
        for graph in [&mut a, &mut b] { graph.pass = 50_000; graph.render(vec![]); }
        assert_eq!(pending_promotions(&a) + pending_promotions(&b), 6);
        for graph in [&mut a, &mut b] { graph.pass = 50_000; graph.render(vec![]); }
        assert_eq!(pending_promotions(&a) + pending_promotions(&b), 6, "a second sizing/layout pass must not promote first-frame paths");
        for graph in [&mut a, &mut b] { graph.pass = 50_001; graph.render(vec![]); }
        assert_eq!(pending_promotions(&a) + pending_promotions(&b), 5);
        for graph in [&mut b, &mut a] { graph.pass = 50_001; graph.render(vec![]); }
        assert_eq!(pending_promotions(&a) + pending_promotions(&b), 5, "all views and passes share one promotion allowance");
        for graph in [&mut b, &mut a] { graph.pass = 50_002; graph.render(vec![]); }
        assert_eq!((pending_promotions(&a), pending_promotions(&b)), (2, 2));
    }

    #[test]
    fn changed_curve_keys_cancel_old_promotion_eligibility() {
        use eframe::egui::{self, pos2, Rect};
        let mut graph = promotion_graph(false);
        graph.pass = 60_000; graph.render(vec![]);
        graph.pass = 60_001; graph.render(vec![]);
        assert_eq!(pending_promotions(&graph), 2);
        for change in 0..3 {
            // Invalidate within the current application frame: every new lane must remain
            // complete compact paths, even if its old incarnation was already promoted.
            graph.pass -= 1;
            match change {
                0 => graph.ctx.data_mut(|data| data.insert_temp(egui::Id::new("decoration_graph"), ViewRange { lo: 20.25, hi: 3000.75 })),
                1 => graph.clip = Some(Rect::from_min_max(pos2(24.25, 31.5), pos2(370.5, 220.25))),
                2 => graph.ctx.set_pixels_per_point(1.5),
                _ => unreachable!(),
            }
            graph.render(vec![]);
            assert_eq!(pending_promotions(&graph), 3, "new key must reset eligibility at change {change}");
            let keys: Vec<_> = graph.drawn.shapes.iter().map(|lane| lane.as_ref().unwrap().key.clone()).collect();
            graph.render(vec![]);
            let next_keys: Vec<_> = graph.drawn.shapes.iter().map(|lane| lane.as_ref().unwrap().key.clone()).collect();
            if keys != next_keys {
                // egui can apply DPI/viewport geometry over consecutive frames. Each actual
                // key change starts a fresh complete Path frame, rather than promoting stale points.
                assert_eq!(pending_promotions(&graph), 3, "a second geometry change must reset every lane: change {change}");
                graph.render(vec![]);
                assert!(graph.drawn.shapes.iter().zip(&next_keys).all(|(lane, key)| lane.as_ref().unwrap().key == *key),
                    "the following frame must exercise stable keys: change {change}");
            }
            assert_eq!(pending_promotions(&graph), 2, "one stable later frame promotes exactly one lane: change {change}");
        }
    }

    #[test]
    fn decoration_cache_matches_forced_uncached_geometry_pixels_and_all_key_changes() {
        use eframe::egui::{self, pos2, vec2, Rect};
        let mut cached = DecorationGraph::new(false);
        let mut uncached = DecorationGraph::new(true);
        let mut previous_pixels = Vec::new();
        for case in 0..19 {
            let before = cached.drawn.decorations.builds;
            for graph in [&mut cached, &mut uncached] {
                match case {
                    0 | 1 => {},
                    2 => graph.frame = 600,
                    3 => {
                        graph.spans = vec![super::Span { start: 120, end: 800, picked: true }];
                        graph.look.selection = Some((400, 700)); graph.gaps = vec![(50, 100), (1500, 1800)];
                    }
                    4 => graph.look.watermark = Color32::from_white_alpha(55),
                    5 => { graph.lane_h = 45.0; graph.look.tag_w = 73.5; },
                    6 => { graph.size.x = 390.375; graph.offset = vec2(5.125, 9.375); },
                    7 => graph.clip = Some(Rect::from_min_max(pos2(24.0, 31.0), pos2(370.0, 220.0))),
                    8 => graph.ctx.data_mut(|data| data.insert_temp(egui::Id::new("decoration_graph"), ViewRange { lo: 25.0, hi: 3000.0 })),
                    9 => { graph.lanes[0].letter = "changed".into(); graph.lanes[0].unit = "cm"; },
                    10 => graph.drawn.set_more(vec![flat(Color32::BLUE, 130.25)]),
                    11 => { graph.look.values = false; graph.look.nav_bg = Color32::from_rgb(4, 8, 12); },
                    12 => graph.ctx.set_pixels_per_point(1.5),
                    13 => graph.ctx.tessellation_options_mut(|options| options.feathering = false),
                    14 => {
                        let mut fonts = egui::FontDefinitions::default();
                        fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().reverse();
                        graph.ctx.set_fonts(fonts);
                    }
                    15 => graph.ctx.set_fonts(egui::FontDefinitions::default()),
                    16 => { graph.enabled = false; graph.opacity = 0.55; },
                    17 => { graph.enabled = true; graph.opacity = 1.0; graph.drawn.set_hover_force(Some(1)); },
                    18 => { graph.drawn.set_pair(None); graph.drawn.set_more_pairs(vec![]); },
                    _ => unreachable!(),
                }
            }
            let a = cached.render(vec![]);
            let b = uncached.render(vec![]);
            assert_eq!(a.0, b.0, "cached and freshly generated ordered triangles differ at case {case}");
            assert_eq!(a.1.pixels, b.1.pixels, "cached and freshly generated pixels differ at case {case}");
            if matches!(case, 1 | 2 | 3 | 16 | 17) {
                assert_eq!(cached.drawn.decorations.builds, before, "dynamic layers/tint must not rebuild static decorations: case {case}");
            } else {
                assert!(cached.drawn.decorations.builds > before, "changed decoration input must invalidate its cache: case {case}");
            }
            if matches!(case, 2 | 3 | 16 | 17) {
                assert_ne!(a.1.pixels, previous_pixels, "dynamic layer must still change the picture: case {case}");
            }
            previous_pixels = a.1.pixels;
        }
    }

    #[test]
    fn sizing_and_invisible_graph_passes_keep_layout_and_range_without_building_caches() {
        use eframe::egui;
        for sizing in [false, true] {
            let mut measured = DecorationGraph::new(false);
            let mut visible = DecorationGraph::new(false);
            for graph in [&mut measured, &mut visible] {
                graph.ctx.data_mut(|data| data.insert_temp(egui::Id::new("decoration_graph"), ViewRange { lo: 15.25, hi: 3200.75 }));
                graph.look.selection = Some((400, 700));
                graph.spans = vec![super::Span { start: 200, end: 800, picked: true }];
                graph.gaps = vec![(40, 60)];
            }
            measured.invisible = true;
            measured.sizing = sizing;
            measured.render(vec![]);
            assert_eq!(measured.skipped_passes, 1);
            assert!(measured.drawn.key.is_none() && measured.drawn.shapes.is_empty());
            assert_eq!(measured.drawn.decorations.builds, 0);
            assert!(measured.drawn.decorations.nav.is_none() && measured.drawn.decorations.lanes.is_empty());
            let range = measured.ctx.data(|data| data.get_temp::<ViewRange>(egui::Id::new("decoration_graph"))).unwrap();
            assert_eq!((range.lo, range.hi), (15.25, 3200.75));
            let allocated = measured.allocated;
            // The same guard must also avoid launching device-graph thinning workers.
            measured.drawn.background = true;
            measured.render(vec![]);
            assert!(measured.drawn.background_rx.is_none());
            measured.drawn.background = false;
            measured.invisible = false;
            measured.sizing = false;
            let a = measured.render(vec![]);
            let b = visible.render(vec![]);
            assert_eq!(measured.allocated, allocated, "measurement must allocate the retained graph's full size");
            assert_eq!(measured.allocated, visible.allocated);
            assert_eq!(a.0, b.0, "the first retained graph must match ordinary full rendering");
            assert_eq!(a.1.pixels, b.1.pixels);
        }
    }

    #[test]
    fn known_discarded_graph_pass_builds_only_in_retained_pass_and_preserves_pixels() {
        let mut multipass = DecorationGraph::new(false);
        let mut ordinary = DecorationGraph::new(false);
        multipass.discard_first = true;
        let a = multipass.render(vec![]);
        let b = ordinary.render(vec![]);
        assert_eq!(multipass.skipped_passes, 1, "exercise an actual egui discarded pass");
        assert_eq!(multipass.drawn.decorations.builds, ordinary.drawn.decorations.builds,
            "first-visible work stays in the measured frame and must be built just once");
        assert_eq!(multipass.allocated, ordinary.allocated);
        assert_eq!(a.0, b.0);
        assert_eq!(a.1.pixels, b.1.pixels);
    }

    #[test]
    fn cached_decorations_keep_legend_focus_regions_and_hidden_legend_does_not_leak() {
        use crate::app::key_focus::{self, Target};
        use eframe::egui::{self, pos2};
        let mut graph = DecorationGraph::new(false);
        graph.render(vec![]); graph.render(vec![]);
        let builds = graph.drawn.decorations.builds;
        let lane = graph.drawn.decorations.lanes[0].as_ref().unwrap().key.geometry.rect;
        let click = |pos| vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE },
        ];
        let legend = pos2(lane.right() - 10.0, lane.top() + 5.0);
        graph.render(click(legend));
        assert_eq!(key_focus::target(&graph.ctx), Target::Blocked);
        assert_eq!(graph.drawn.decorations.builds, builds, "a cache hit must still register the legend's focus region");
        graph.invisible = true;
        graph.render(click(legend));
        assert_eq!(graph.skipped_passes, 1);
        assert_eq!(key_focus::target(&graph.ctx), Target::Blocked,
            "skipping invisible rendering must retain the legend's keyboard boundary");
        graph.render(click(lane.center()));
        assert_eq!(key_focus::target(&graph.ctx), Target::Markers(0));
        graph.drawn.set_pair(None); graph.drawn.set_more_pairs(vec![]);
        graph.render(vec![]);
        graph.render(click(legend));
        assert_eq!(key_focus::target(&graph.ctx), Target::Markers(0), "a removed legend must leave no stale blocked region");
        graph.invisible = false;
        graph.render(vec![]);
        graph.render(click(lane.center()));
        assert!(graph.goto.is_some(), "the retained visible graph still handles frame clicks");
        assert_eq!(key_focus::target(&graph.ctx), Target::Markers(0));
    }

    #[test]
    fn hybrid_solid_cache_matches_frozen_meshes_in_geometry_and_pixels() {
        use super::{curve_shapes, DrawnLane, DOT_R, HOVER_DOT_R, HOVER_W, LINE_W};
        use eframe::egui::{self, pos2, Color32, Pos2, Rect, Shape, Stroke};

        let dashed = DrawnLane { lo: 0.0, hi: 1.0, runs: vec![vec![[3.0, 14.0], [31.0, 18.0], [58.0, 10.0]]].into() };
        let red = DrawnLane { lo: 0.0, hi: 1.0, runs: vec![
            vec![[2.0, 8.0], [18.0, 15.0], [39.0, 7.0], [60.0, 20.0]],
            vec![[28.0, 24.0]], // A circle must stay between the two red runs.
            vec![[5.0, 36.0], [54.0, 29.0]],
        ].into() };
        let blue = DrawnLane { lo: 0.0, hi: 1.0, runs: vec![vec![[8.0, 34.0], [45.0, 13.0], [62.0, 34.0]]].into() };
        let clip = Rect::from_min_max(pos2(8.0, 4.0), pos2(56.0, 44.0));
        let options = egui::epaint::TessellationOptions::default();
        let tess = || {
            let mut t = egui::epaint::Tessellator::new(1.5, options, [1, 1], vec![]);
            t.set_clip_rect(clip);
            t
        };
        let empty = || std::iter::empty::<(&DrawnLane, Color32)>();
        for count in [1, 2] {
        let solids = || [(&red, Color32::RED), (&blue, Color32::BLUE)].into_iter().take(count);
        let mut maker = tess();
        let (cached, drew) = curve_shapes(&mut maker, std::iter::once((&dashed, Color32::GREEN)), solids(), |x| x as f32, |y| y as f32, LINE_W, DOT_R, true);
        assert_eq!((drew.solid, drew.dashed), (if count == 1 { 3 } else { 4 }, 1));
        assert_eq!(cached.iter().filter(|shape| matches!(shape, Shape::Mesh(_))).count(), if count == 1 { 3 } else { 1 },
            "one marker retains the prior solid meshes; several retain only the dashed mesh");
        assert_eq!(cached.iter().filter(|shape| matches!(shape, Shape::Path(_))).count(), if count == 1 { 0 } else { 3 },
            "multiple markers retain compact points for the per-frame painter copy");

        // Frozen v0.32.10 representation: tessellate solid runs into owned meshes, flushing
        // around dots. Keep this reference independent of the compact solid path under test;
        // dashed mesh generation is unchanged in both forms.
        let mut maker = tess();
        let (mut previous, _) = curve_shapes(&mut maker, std::iter::once((&dashed, Color32::GREEN)), empty(), |x| x as f32, |y| y as f32, LINE_W, DOT_R, true);
        let mut solid_mesh = egui::Mesh::default();
        for (lane, color) in solids() {
            for run in lane.runs.iter() {
                let pts: Vec<Pos2> = run.iter().map(|p| pos2(p[0] as f32, p[1] as f32)).collect();
                if pts.len() == 1 {
                    if !solid_mesh.is_empty() { previous.push(Shape::mesh(std::mem::take(&mut solid_mesh))); }
                    previous.push(Shape::circle_filled(pts[0], DOT_R, color));
                } else {
                    maker.tessellate_shape(Shape::line(pts, Stroke::new(LINE_W, color)), &mut solid_mesh);
                }
            }
        }
        if !solid_mesh.is_empty() { previous.push(Shape::mesh(solid_mesh)); }
        assert_eq!(previous.iter().filter(|shape| matches!(shape, Shape::Mesh(_))).count(), 3,
            "the frozen reference retains dashed and solid meshes on both sides of the dot");
        let flatten = |shapes: Vec<Shape>| {
            let mut t = tess();
            let mut mesh = egui::Mesh::default();
            for shape in shapes { t.tessellate_shape(shape, &mut mesh); }
            mesh
        };
        let old_mesh = flatten(previous);
        let new_mesh = flatten(cached);
        assert!(!new_mesh.indices.is_empty());
        assert_eq!(new_mesh, old_mesh, "egui triangle vertices, colors, and painter order must match exactly");

        // Exercise the same CPU painter used by headless screenshots, with egui's white
        // managed texture for untextured line and circle vertices.
        let mut textures = crate::soft_render::Textures::default();
        let mut delta = egui::epaint::textures::TexturesDelta::default();
        delta.set.push((egui::TextureId::Managed(0), egui::epaint::ImageDelta::full(
            egui::ColorImage::new([1, 1], Color32::WHITE), egui::TextureOptions::NEAREST,
        )));
        textures.set(&delta);
        let image = |mesh: &egui::Mesh| crate::soft_render::paint(
            &[egui::epaint::ClippedPrimitive { clip_rect: clip, primitive: egui::epaint::Primitive::Mesh(mesh.clone()) }],
            &textures, [96, 72], 1.5, Color32::BLACK,
        );
        let old_pixels = image(&old_mesh);
        assert!(old_pixels.pixels.iter().any(|pixel| *pixel != Color32::BLACK));
        assert_eq!(image(&new_mesh).pixels, old_pixels.pixels);

        let mut maker = tess();
        let (hover, _) = curve_shapes(&mut maker, empty(), std::iter::once((&red, Color32::RED)), |x| x as f32, |y| y as f32, HOVER_W, HOVER_DOT_R, false);
        assert!(hover.iter().all(|shape| !matches!(shape, Shape::Mesh(_))), "hover-width solid lines keep their old representation");
        }
    }

    #[test]
    fn initial_marker_and_plate_thinning_share_budget_across_sizing_passes_and_finish() {
        use super::{Drawn, start_frame, end_frame};
        let lanes: Vec<Lane> = ["X", "Y", "Z"].into_iter().map(|letter| Lane {
            letter: letter.into(), color: Color32::RED,
            runs: vec![(0..1000).map(|f| [f as f64, f as f64 * 0.01]).collect()].into(), unit: "",
        }).collect();
        let vr = ViewRange { lo: -0.5, hi: 999.5 };
        let key = (vr.lo.to_bits(), vr.hi.to_bits(), 200);
        let mut graphs = [Drawn::default(), Drawn::default(), Drawn::default()];
        start_frame(20_001, Some(3000));
        for graph in &mut graphs { graph.prepare_primary(&lanes, vr, 200, key); }
        assert_eq!(graphs.iter().filter(|g| g.key == Some(key)).count(), 1);
        end_frame();
        // egui may size and then draw the same floating areas in one application frame.
        start_frame(20_001, Some(3000));
        for graph in &mut graphs { graph.prepare_primary(&lanes, vr, 200, key); }
        assert_eq!(graphs.iter().filter(|g| g.key == Some(key)).count(), 1, "second pass must not reset the budget");
        end_frame();
        assert!(start_frame(20_002, Some(3000)), "deferred primary work must request the following frame");
        for graph in &mut graphs { graph.prepare_primary(&lanes, vr, 200, key); }
        end_frame();
        start_frame(20_003, Some(3000));
        for graph in &mut graphs { graph.prepare_primary(&lanes, vr, 200, key); }
        end_frame();
        assert!(graphs.iter().all(|g| g.key == Some(key)));
        // Capture/screenshot mode is unlimited: exactly the original thin data in one pass.
        start_frame(20_004, None);
        let mut capture = Drawn::default();
        capture.prepare_primary(&lanes, vr, 200, key);
        end_frame();
        for graph in &graphs {
            for (actual, expected) in graph.lanes.iter().zip(&capture.lanes) {
                assert_eq!(actual.runs, expected.runs);
                assert_eq!((actual.lo.to_bits(), actual.hi.to_bits()), (expected.lo.to_bits(), expected.hi.to_bits()));
            }
        }
    }

    #[test]
    fn thinned_lines_keep_spikes_gaps_and_both_ends_whatever_the_file_length() {
        // Two runs with a gap between them: frames 0..=9999 and 12000..=12999, value = frame,
        // one spike at frame 5000.
        let run = |a: i64, b: i64| (a..=b).map(|f| [f as f64, if f == 5000 { 1.0e6 } else { f as f64 }]).collect::<Vec<_>>();
        let lane = Lane { letter: "X".into(), color: Color32::RED, runs: (vec![run(0, 9999), run(12000, 12999)]).into(), unit: "mm" };
        let whole = thin(&lane, &ViewRange { lo: -0.5, hi: 12999.5 }, 100);
        assert_eq!((whole.lo, whole.hi), (0.0, 1.0e6));
        assert_eq!(whole.runs.len(), 2, "the gap stays a gap");
        let points: usize = whole.runs.iter().map(Vec::len).sum();
        assert!(points <= 4 * 100 + 8, "at most four points per column: {points}");
        assert_eq!((whole.runs[0][0], *whole.runs[0].last().unwrap()), ([0.0, 0.0], [9999.0, 9999.0]), "first and last point of a run are kept");
        assert!(whole.runs[0].contains(&[5000.0, 1.0e6]), "the spike is kept");
        assert!(whole.runs[0].windows(2).all(|w| w[0][0] < w[1][0]), "in time order");
        // Ten times as many samples: the same number of points.
        let long = Lane { letter: "X".into(), color: Color32::RED, runs: (vec![run(0, 99_999), run(120_000, 129_999)]).into(), unit: "mm" };
        let long_points: usize = thin(&long, &ViewRange { lo: -0.5, hi: 129_999.5 }, 100).runs.iter().map(Vec::len).sum();
        assert!(long_points <= points + 2, "{long_points} vs {points}");
        // Zoomed in: min / max are of the frames in view only; runs outside are left out.
        let part = thin(&lane, &ViewRange { lo: 100.0, hi: 200.0 }, 1000);
        assert_eq!((part.lo, part.hi), (100.0, 200.0));
        assert_eq!(part.runs.len(), 1);
        assert_eq!(part.runs[0].len(), 103, "every frame in view plus one on each side");
        let none = thin(&lane, &ViewRange { lo: 10500.0, hi: 11000.0 }, 1000);
        assert!(none.runs.is_empty() && !none.lo.is_finite());
    }

    #[test]
    fn comment_bands_cover_their_frames_follow_zoom_and_stay_visible_for_one_frame() {
        use super::{span_x, Span, SPAN_MIN_W};
        let s = |start, end| Span { start, end, picked: false };
        // Frames 0–999 over 1000 pt: frame f covers f − 0.5 .. f + 0.5.
        let (lo, hi) = (-0.5, 999.5);
        assert_eq!(span_x(&s(100, 199), lo, hi, 0.0, 1000.0), Some((100.0, 200.0)));
        // One frame on a long file (0.1 pt a frame): still SPAN_MIN_W wide, around its frame.
        let (x0, x1) = span_x(&s(5000, 5000), -0.5, 9999.5, 0.0, 1000.0).unwrap();
        assert!((x1 - x0 - SPAN_MIN_W).abs() < 1e-4 && ((x0 + x1) / 2.0 - 500.05).abs() < 1e-3, "{x0} {x1}");
        // Zoomed in on 100–200 (wheel / middle drag): the band follows; outside: not drawn.
        assert_eq!(span_x(&s(150, 159), 99.5, 199.5, 0.0, 1000.0), Some((500.0, 600.0)));
        assert_eq!(span_x(&s(300, 400), 99.5, 199.5, 0.0, 1000.0), None);
        assert_eq!(span_x(&s(10, 20), 99.5, 199.5, 0.0, 1000.0), None);
    }

    #[test]
    fn dashes_go_along_the_line_around_corners_and_cost_by_length_not_by_samples() {
        use super::dashes;
        use eframe::egui::pos2;
        // A straight line 100 long: dash 6, gap 4 -> 10 dashes, 6 long each.
        let d = dashes(&[pos2(0.0, 0.0), pos2(100.0, 0.0)], 6.0, 4.0);
        assert_eq!(d.len(), 10);
        assert!(d.iter().all(|p| (p[0].distance(*p.last().unwrap()) - 6.0).abs() < 1e-3));
        assert_eq!((d[1][0].x, d[9].last().unwrap().x), (10.0, 96.0));
        // A corner inside a dash: the dash goes on around it (three points), the length stays.
        let c = dashes(&[pos2(0.0, 0.0), pos2(3.0, 0.0), pos2(3.0, 10.0)], 6.0, 4.0);
        assert_eq!(c[0], vec![pos2(0.0, 0.0), pos2(3.0, 0.0), pos2(3.0, 3.0)]);
        // The same line in 800 tiny steps: the same dashes.
        let fine: Vec<_> = (0..=800).map(|k| pos2(k as f32 * 0.125, 0.0)).collect();
        assert_eq!(dashes(&fine, 6.0, 4.0).len(), 10);
        assert!(dashes(&[pos2(1.0, 1.0)], 6.0, 4.0).is_empty() && dashes(&[], 6.0, 4.0).is_empty());
    }

    /// Shapes `show` paints for lanes of 4000 frames, with `pair` set on its cache.
    fn painted(pair: Option<super::Pair>) -> (Vec<eframe::egui::epaint::ClippedShape>, super::Drew) {
        painted_with(pair, vec![], vec![])
    }

    /// Three lanes, each a curve of `color` at `value` over 4000 frames (v0.27.0 tests).
    fn flat(color: Color32, value: f64) -> Vec<Lane> {
        ["X", "Y", "Z"].iter().map(|l| Lane { letter: (*l).into(), color, runs: (vec![(0..4000).map(|f| [f as f64, value]).collect()]).into(), unit: "mm" }).collect()
    }

    /// `painted`, with the other markers of a graph of several (`Drawn::set_more`) and their
    /// dashed lanes (`set_more_pairs`).
    fn painted_with(pair: Option<super::Pair>, more: Vec<Vec<Lane>>, more_pairs: Vec<Vec<Lane>>) -> (Vec<eframe::egui::epaint::ClippedShape>, super::Drew) {
        use super::{show, Drawn, Look};
        use eframe::egui;
        let lane = |k: f64| Lane { letter: "X".into(), color: Color32::RED, runs: (vec![(0..4000).map(|f| [f as f64, (f as f64 * 0.01).sin() * k]).collect()]).into(), unit: "mm" };
        let lanes = [lane(1.0), lane(2.0), lane(3.0)];
        let mut drawn = Drawn::default();
        drawn.set_pair(pair);
        drawn.set_more(more);
        drawn.set_more_pairs(more_pairs);
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0))), ..Default::default() };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, egui::Id::new("g"), &lanes, 0, 3999, 100, &[], 100.0, &Look::pane(), Some(&mut drawn), &[]);
            });
        });
        (out.shapes, drawn.drew.clone())
    }

    #[test]
    fn without_a_second_curve_the_graph_paints_exactly_as_before_and_with_one_adds_dashes_and_the_legend() {
        let (alone, drew) = painted(None);
        assert_eq!((drew.solid, drew.dashed, drew.legend), (3, 0, false));
        // A file laid over without a marker of that name: nothing extra, not one shape moved.
        let (missing, _) = painted(Some(super::Pair { lanes: vec![], shown: [true, true] }));
        assert_eq!(missing, alone);
        // With one: one range over both, the dashes, the legend; the first lane's curve keeps
        // the legend's strip free.
        let other = |k: f64| Lane { letter: "X".into(), color: Color32::RED, runs: (vec![(0..4000).map(|f| [f as f64, 5.0 + k]).collect()]).into(), unit: "mm" };
        let (both, drew) = painted(Some(super::Pair { lanes: vec![other(1.0), other(2.0), other(3.0)], shown: [true, true] }));
        assert_eq!((drew.solid, drew.dashed, drew.legend), (3, 3, true));
        assert!(drew.dashes > 3 && drew.dashes < 1000, "{}", drew.dashes);
        assert_eq!(drew.ranges[2].1, 8.0, "max of lane Z = the second curve's 5 + 3");
        assert!(both.len() > alone.len());
    }

    #[test]
    fn several_markers_share_the_lanes_each_in_its_color_on_one_range_and_dash_with_a_second_file() {
        let (blue, gold) = (Color32::from_rgb(0, 0, 255), Color32::from_rgb(255, 200, 0));
        // One marker: exactly as before (no other marker given = not one shape more).
        let (alone, _) = painted(None);
        let (none_more, _) = painted_with(None, vec![], vec![]);
        assert_eq!(none_more, alone);
        // Three markers: every lane has three curves, one color per marker, the same in each
        // lane, and one range over all of them.
        let (three, drew) = painted_with(None, vec![flat(blue, 50.0), flat(gold, -40.0)], vec![]);
        assert_eq!(drew.colors, vec![vec![Color32::RED, blue, gold]; 3]);
        assert_eq!((drew.solid, drew.dashed, drew.legend), (9, 0, false));
        assert_eq!(drew.ranges[0], (-40.0, 50.0), "lane X covers the sine (±1) and both flat curves");
        assert_eq!(drew.ranges[2].1, 50.0);
        let vertex_count = |shapes: &[eframe::egui::epaint::ClippedShape]| {
            let mut tess = eframe::egui::epaint::Tessellator::new(1.0, Default::default(), [1, 1], vec![]);
            let mut mesh = eframe::egui::Mesh::default();
            for shape in shapes {
                tess.set_clip_rect(shape.clip_rect);
                tess.tessellate_shape(shape.shape.clone(), &mut mesh);
            }
            mesh.vertices.len()
        };
        let has_color = |shapes: &[eframe::egui::epaint::ClippedShape], color| shapes.iter().any(|shape| {
            match &shape.shape {
                eframe::egui::Shape::Path(path) => path.stroke.color == eframe::egui::epaint::ColorMode::Solid(color),
                eframe::egui::Shape::Mesh(mesh) => mesh.vertices.iter().any(|vertex| vertex.color == color),
                _ => false,
            }
        });
        assert!(vertex_count(&three) > vertex_count(&alone), "two extra markers add drawn line geometry");
        for color in [blue, gold] {
            assert!(has_color(&three, color), "{color:?} marker must reach the painter");
            assert!(!has_color(&alone, color));
        }
        // A second file laid over: the dashed curve of each marker that has one, in its color;
        // the legend `— 1  ┄ 2`; the range covers the dashed ones too.
        let pair = super::Pair { lanes: flat(Color32::RED, 0.5), shown: [true, true] };
        let (_, d) = painted_with(Some(pair), vec![flat(blue, 50.0), flat(gold, -40.0)], vec![flat(blue, 80.0), vec![]]);
        assert_eq!(d.dash_colors, vec![vec![Color32::RED, blue]; 3], "the third marker has none in the second file");
        assert_eq!((d.solid, d.dashed, d.legend), (9, 6, true));
        assert_eq!(d.ranges[1], (-40.0, 80.0));
        // Only the other markers have one: still the legend and the dashes.
        let pair = super::Pair { lanes: vec![], shown: [true, false] };
        let (_, d) = painted_with(Some(pair), vec![flat(blue, 50.0)], vec![flat(blue, 80.0)]);
        assert_eq!((d.dashed, d.legend, d.ranges[0].1), (0, true, 80.0), "hidden by key 2, the range stays");
    }

    #[test]
    fn the_frames_graph_work_is_shared_by_its_passes_and_what_was_put_off_is_told_the_next_frame() {
        use super::{end_frame, spend, start_frame};
        assert!(!start_frame(1, Some(100)), "nothing put off before");
        assert!(spend(500, true), "the first marker of a graph: always, counted");
        assert!(spend(500, false), "the first work that may wait in a frame: always, even past the budget");
        assert!(!spend(1, false), "then nothing more in this frame");
        end_frame();
        assert!(spend(1_000_000, false), "outside the app's frame: no limit");
        // A frame of two passes (egui throws the first picture away): one budget for both.
        assert!(start_frame(2, Some(100)), "frame 1 put work off");
        assert!(spend(60, false) && spend(30, false));
        assert!(!spend(30, false), "past the budget");
        end_frame();
        assert!(start_frame(2, Some(100)), "the second pass: still about frame 1");
        assert!(spend(10, false), "what was left: 10");
        assert!(!spend(1, false));
        end_frame();
        assert!(start_frame(3, Some(100)), "frame 2 put work off");
        assert!(spend(100, false));
        end_frame();
        assert!(!start_frame(4, None), "frame 3 put nothing off");
        assert!(spend(usize::MAX, false) && spend(usize::MAX, false), "no budget: no limit");
        end_frame();
    }

    /// Draws the three sine lanes of `painted_with` with `drawn` in `n` frames of one context,
    /// each frame with `budget` samples of graph work (None = no limit, `super::spend`): what each
    /// frame painted and whether something was not drawn as it will be yet.
    fn frames(drawn: &mut super::Drawn, n: usize, budget: Option<usize>) -> Vec<(Vec<eframe::egui::epaint::ClippedShape>, super::Drew, bool)> {
        use super::{end_frame, show, start_frame, Look};
        use eframe::egui;
        let lane = |k: f64| Lane { letter: "X".into(), color: Color32::RED, runs: (vec![(0..4000).map(|f| [f as f64, (f as f64 * 0.01).sin() * k]).collect()]).into(), unit: "mm" };
        let lanes = [lane(1.0), lane(2.0), lane(3.0)];
        let ctx = egui::Context::default();
        (0..n)
            .map(|k| {
                // These tests isolate thinning/painter reuse; promotion has pixel tests above.
                for lane in drawn.shapes.iter_mut().flatten() {lane.promote_after=None;}
                start_frame(k as u64, budget);
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0))),
                    time: Some(k as f64),
                    ..Default::default()
                };
                let out = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show(ui, egui::Id::new("g"), &lanes, 0, 3999, 100, &[], 100.0, &Look::pane(), Some(&mut *drawn), &[]);
                    });
                });
                end_frame();
                (out.shapes, drawn.drew.clone(), drawn.pending())
            })
            .collect()
    }

    /// A graph of four markers (the sine lanes and three flat ones) with a second file laid over:
    /// dashed lanes for the first marker and two of the others.
    fn four_markers() -> super::Drawn {
        let (blue, gold, green) = (Color32::from_rgb(0, 0, 255), Color32::from_rgb(255, 200, 0), Color32::from_rgb(0, 200, 0));
        let mut drawn = super::Drawn::default();
        drawn.set_pair(Some(super::Pair { lanes: flat(Color32::RED, 0.5), shown: [true, true] }));
        drawn.set_more(vec![flat(blue, 50.0), flat(gold, -40.0), flat(green, 10.0)]);
        drawn.set_more_pairs(vec![flat(blue, 80.0), vec![], flat(green, 20.0)]);
        drawn
    }

    #[test]
    fn markers_past_the_frames_work_are_thinned_in_the_next_frames_and_end_as_drawn_at_once() {
        // No budget (a picture run, tests): every curve in the first frame.
        let mut at_once = four_markers();
        let all = frames(&mut at_once, 6, None);
        assert_eq!((all[0].1.colors[0].len(), all[0].1.dash_colors[0].len(), all[0].2), (4, 3, false));
        // A budget of one marker's samples (4000) a frame: the first solid/dashed pair also
        // spends the shared allowance. Then one more solid or dashed curve can finish per
        // frame (an empty one costs nothing); the last frame matches unlimited capture.
        let mut spread = four_markers();
        let few = frames(&mut spread, 6, Some(4000));
        let solid: Vec<usize> = few.iter().map(|f| f.1.colors[0].len()).collect();
        let dashed: Vec<usize> = few.iter().map(|f| f.1.dash_colors[0].len()).collect();
        assert_eq!((solid, dashed), (vec![1, 2, 2, 3, 4, 4], vec![1, 1, 2, 2, 2, 3]));
        assert_eq!(few.iter().map(|f| f.2).collect::<Vec<_>>(), [true, true, true, true, true, false], "not complete until the last one");
        // Then the very same picture as at once (the texts' glyphs aside: other value texts
        // were laid out on the way, so they sit elsewhere in the font atlas).
        use eframe::egui::{epaint::ClippedShape, Shape};
        let shapes = |s: &[ClippedShape]| s.iter().filter(|c| !matches!(c.shape, Shape::Text(_))).cloned().collect::<Vec<_>>();
        let texts = |s: &[ClippedShape]| {
            s.iter().filter_map(|c| if let Shape::Text(t) = &c.shape { Some((t.pos, t.galley.job.text.clone())) } else { None }).collect::<Vec<_>>()
        };
        assert_eq!(shapes(&few[5].0), shapes(&all[5].0));
        assert_eq!(texts(&few[5].0), texts(&all[5].0));
        assert_eq!((few[5].1.remade, all[5].1.remade), (3, 0), "made again in the last frame, kept at once");
        assert_eq!(super::Drew { remade: 0, ..few[5].1.clone() }, all[5].1);
    }

    #[test]
    fn curves_and_dashes_are_made_once_and_painted_again_while_nothing_changes() {
        let mut drawn = four_markers();
        let f = frames(&mut drawn, 3, None);
        assert_eq!(f.iter().map(|x| x.1.remade).collect::<Vec<_>>(), [3, 0, 0], "every lane made in the first frame only");
        assert_eq!(f[1].0, f[2].0, "and painted the same");
        assert!(f[2].1.dashes > 10 && f[2].1.dashes == f[0].1.dashes, "{}", f[2].1.dashes);
        // Hiding the second file (name tag 2): made again once, without the dashes.
        drawn.set_shown([true, false]);
        let g = frames(&mut drawn, 2, None);
        assert_eq!(g.iter().map(|x| (x.1.remade, x.1.dashes)).collect::<Vec<_>>(), [(3, 0), (0, 0)]);
        // Shown again: made again, as at first.
        drawn.set_shown([true, true]);
        let h = frames(&mut drawn, 1, None);
        assert_eq!((h[0].1.remade, &h[0].0), (3, &f[0].0));
    }

    #[test]
    fn changing_playback_frame_reuses_compact_solids_and_dashed_meshes() {
        use super::{end_frame, show, start_frame, Look};
        use eframe::egui;
        let mut drawn = four_markers();
        let lanes = flat(Color32::RED, 1.0);
        let ctx = egui::Context::default();
        let mut original = None;
        for (iteration, frame) in [100, 101, 2000, 3999].into_iter().enumerate() {
            start_frame(0, None); // keep first-frame representation; promotion parity tested separately
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0))),
                time: Some(iteration as f64),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(ui, egui::Id::new("playback_cache"), &lanes, 0, 3999, frame, &[], 100.0, &Look::pane(), Some(&mut drawn), &[]);
                });
            });
            end_frame();
            assert!(!drawn.pending());
            assert_eq!(drawn.drew.remade, if iteration == 0 { 3 } else { 0 },
                "the moving current-frame line must not rebuild any cached curve");
            let cached: Vec<_> = drawn.shapes.iter().map(|lane| lane.as_ref().unwrap().shapes.clone()).collect();
            if let Some(original) = &original { assert_eq!(&cached, original); }
            else { original = Some(cached); }
        }
    }

    #[test]
    fn background_curves_include_raw_extrema_and_overlays_without_ui_sample_work() {
        use super::{Drawn, Look, Pair, WORK, end_frame, show, start_frame};
        use eframe::egui;
        let source = Lane { letter: "EMG".into(), color: Color32::LIGHT_BLUE, unit: "",
            runs: vec![(0..100_000).map(|i| [i as f64, (i as f64 * 0.05).sin()]).collect()].into() };
        let lanes = vec![source.clone()];
        let mut drawn = Drawn::background();
        drawn.set_pair(Some(Pair { lanes: lanes.clone(), shown: [true, true] }));
        drawn.push_more(lanes.clone()); // Raw minimum.
        drawn.push_more(lanes.clone()); // Raw maximum / processed overlay.
        drawn.push_more_pair(lanes.clone());
        let ctx = egui::Context::default();
        let watchdog = crate::work_queue::TestWatchdog::new();
        for iteration in 0.. {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 300.0))),
                time: Some(iteration as f64 / 100.0), ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                start_frame(iteration as u64, Some(1));
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(ui, egui::Id::new("background_all_curves"), &lanes, 0, 99_999, 100,
                        &[], 110.0, &Look::pane(), Some(&mut drawn), &[]);
                });
                WORK.with(|work| {
                    assert_eq!(work.get().left, Some(1), "background extras must not spend UI samples");
                    assert!(!work.get().some_done, "no primary, pair, extrema or overlay may thin on UI");
                });
                end_frame();
            });
            if drawn.complete_pass == Some(ctx.cumulative_pass_nr() - 1) { break; }
            watchdog.pause(|| format!("background graph did not complete. {}", drawn.background_state()));
        }
        let complete = drawn.complete_pass == Some(ctx.cumulative_pass_nr() - 1);
        assert!(complete, "background graph did not complete");
        let key = drawn.key.unwrap();
        let vr = ViewRange { lo: f64::from_bits(key.0), hi: f64::from_bits(key.1) };
        let expected = thin(&source, &vr, key.2);
        let same = |actual: &super::DrawnLane| {
            assert_eq!((actual.lo, actual.hi), (expected.lo, expected.hi));
            assert_eq!(actual.runs, expected.runs, "worker and ordinary thinning must match exactly");
        };
        same(&drawn.lanes[0]); same(&drawn.pair_lanes[0]);
        assert_eq!(drawn.more_lanes.len(), 2);
        for extra in drawn.more_lanes.iter().chain(&drawn.more_pair_lanes) {
            assert_eq!(extra.key, Some(key)); same(&extra.lanes[0]);
        }
        assert!(!drawn.pending());
    }

    #[test]
    fn background_source_changes_reject_old_jobs_and_zoom_keeps_previous_curves() {
        use super::{BackgroundPrepared, Drawn};
        let source = Lane { letter: "A".into(), color: Color32::RED, unit: "",
            runs: vec![(0..4000).map(|i| [i as f64, (i as f64 * 0.01).sin()]).collect()].into() };
        let lanes = vec![source.clone()];
        let ctx = eframe::egui::Context::default();
        let mut drawn = Drawn::background();
        let range = ViewRange { lo: 0.0, hi: 3999.0 };
        let key = (range.lo.to_bits(), range.hi.to_bits(), 200);
        let (tx, rx) = std::sync::mpsc::channel();
        drawn.background_rx = Some((key, drawn.background_generation, rx));
        drawn.push_more(lanes.clone()); // Invalidates an in-flight result of the same range.
        assert!(tx.send(BackgroundPrepared { primary: Vec::new(), sources: Default::default(),
            lanes: Vec::new(), pair_lanes: Vec::new(),
            more_lanes: Vec::new(), more_pair_lanes: Vec::new() }).is_ok());
        drawn.prepare_background(&lanes, range, 200, key, &ctx);
        assert!(drawn.key.is_none(), "a matching range must not accept an obsolete source generation");
        let watchdog = crate::work_queue::TestWatchdog::new();
        loop {
            drawn.prepare_background(&lanes, range, 200, key, &ctx);
            if drawn.key == Some(key) { break; }
            watchdog.pause(|| format!("thinning did not install {:?}. {}", key, drawn.background_state()));
        }
        assert_eq!(drawn.key, Some(key));
        assert_eq!(drawn.more_lanes.len(), 1);
        let previous = drawn.lanes[0].runs.clone();
        let zoom = ViewRange { lo: 1000.0, hi: 2000.0 };
        let zoom_key = (zoom.lo.to_bits(), zoom.hi.to_bits(), 300);
        drawn.prepare_background(&lanes, zoom, 300, zoom_key, &ctx);
        assert_eq!(drawn.lanes[0].runs, previous, "keep the previous curve until the zoom worker finishes");
        let watchdog = crate::work_queue::TestWatchdog::new();
        loop {
            drawn.prepare_background(&lanes, zoom, 300, zoom_key, &ctx);
            if drawn.key == Some(zoom_key) { break; }
            watchdog.pause(|| format!("thinning did not install {:?}. {}", zoom_key, drawn.background_state()));
        }
        assert_eq!(drawn.key, Some(zoom_key));
        let expected = thin(&source, &zoom, 300);
        assert_eq!(drawn.lanes[0].runs, expected.runs);
        assert_eq!(drawn.more_lanes[0].lanes[0].runs, expected.runs);
        assert_eq!(drawn.more_lanes[0].key, Some(zoom_key));
    }

    #[test]
    fn background_retarget_keeps_complete_old_curves_and_colors_until_atomic_replacement() {
        use super::{Drawn, Look, show};
        use eframe::egui;
        let lane = |color, offset| Lane { letter: "A".into(), color, unit: "",
            runs: vec![(0..4000).map(|i| [i as f64, offset + (i as f64 * 0.01).sin()]).collect()].into() };
        let old = vec![lane(Color32::RED, 0.0)];
        let new = vec![lane(Color32::LIGHT_BLUE.gamma_multiply(0.22), 5.0)];
        let ctx = egui::Context::default();
        let range = ViewRange { lo: 0.0, hi: 3999.0 };
        let key = (range.lo.to_bits(), range.hi.to_bits(), 200);
        let mut drawn = Drawn::background();
        drawn.push_more(vec![lane(Color32::GREEN, 1.0)]);
        drawn.push_more(vec![lane(Color32::YELLOW, 2.0)]);
        let watchdog = crate::work_queue::TestWatchdog::new();
        loop {
            drawn.prepare_background(&old, range, 200, key, &ctx);
            if drawn.key == Some(key) { break; }
            watchdog.pause(|| format!("thinning did not install {:?}. {}", key, drawn.background_state()));
        }
        assert_eq!(drawn.key, Some(key));
        let previous = drawn.lanes[0].runs.clone();
        let previous_extras: Vec<_> = drawn.more_lanes.iter().map(|extra| extra.lanes[0].runs.clone()).collect();
        let mut replacement = Drawn::background();
        replacement.push_more(vec![lane(Color32::WHITE, 6.0)]);
        drawn.retarget_background(replacement);
        assert_eq!(drawn.lanes[0].runs, previous);
        assert_eq!(drawn.more.len(), 2, "pending descriptor count must not hide old extras");
        assert_eq!(drawn.more_lanes.iter().map(|extra| extra.lanes[0].runs.clone()).collect::<Vec<_>>(), previous_extras);
        // Hold the next result so rendering with the new primary input deterministically
        // exercises the pending state, including descriptor colors of all old curves.
        let (hold, rx) = std::sync::mpsc::channel();
        drawn.background_rx = Some((key, drawn.background_generation, rx));
        let _ = ctx.run(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 300.0))),
            ..Default::default()
        }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, egui::Id::new("retarget_continuity"), &new, 0, 3999, 100,
                    &[], 110.0, &Look::pane(), Some(&mut drawn), &[]);
            });
        });
        assert_eq!(drawn.drew.colors[0], vec![Color32::RED, Color32::GREEN, Color32::YELLOW],
            "new faint-primary input must not recolor or hide the previous complete graph");
        drop(hold);
        let watchdog = crate::work_queue::TestWatchdog::new();
        loop {
            drawn.prepare_background(&new, range, 200, key, &ctx);
            if drawn.key == Some(key) { break; }
            watchdog.pause(|| format!("thinning did not install {:?}. {}", key, drawn.background_state()));
        }
        assert_eq!(drawn.key, Some(key));
        assert!(drawn.background_sources.is_none());
        assert_eq!(drawn.background_primary[0].color, new[0].color);
        assert_eq!(drawn.more.len(), 1);
        assert_eq!(drawn.more[0][0].color, Color32::WHITE);
        assert_eq!(drawn.more_lanes.len(), 1);
        assert_eq!(drawn.lanes[0].runs, thin(&new[0], &range, 200).runs);
        assert_eq!(drawn.more_lanes[0].lanes[0].runs, thin(&drawn.more[0][0], &range, 200).runs);
    }

    #[test]
    fn background_retarget_can_preserve_a_previously_synchronous_raw_plate_cache() {
        let source = Lane { letter: "Fz".into(), color: Color32::BLUE, unit: "",
            runs: vec![vec![[0.0, 1.0], [1.0, 2.0]]].into() };
        let mut drawn = super::Drawn::default();
        let vr = ViewRange { lo: 0.0, hi: 1.0 };
        assert!(drawn.prepare_primary(&[source], vr, 200, (0.0f64.to_bits(), 1.0f64.to_bits(), 200)));
        let previous = drawn.lanes[0].runs.clone();
        drawn.retarget_background(super::Drawn::background());
        assert!(drawn.background);
        assert_eq!(drawn.lanes[0].runs, previous);
        assert!(drawn.background_pending());
    }

    #[test]
    fn nice_steps() {
        assert_eq!(nice_step(1000.0, 10.0), 100.0);
        assert_eq!(nice_step(1000.0, 4.0), 500.0);
        assert_eq!(nice_step(37.0, 10.0), 5.0);
        assert_eq!(nice_step(5.0, 10.0), 1.0);
    }

    // ───────────── v0.29.0: the marker's name on curve hover ─────────────

    /// A lane on screen for the pure hit test: frames −0.5 … 999.5 over x 0 … 1000, values 0 … 100
    /// over y 97 … 3 (`Map`), the lane rectangle 0,0 – 1000,100.
    fn test_map() -> super::Map {
        super::Map { left: 0.0, width: 1000.0, lo: -0.5, hi: 999.5, bottom: 100.0, span: 94.0, ylo: 0.0, yhi: 100.0 }
    }

    /// A thinned curve of `runs` (frame, value).
    fn drawn_lane(runs: Vec<Vec<[f64; 2]>>) -> super::DrawnLane {
        super::DrawnLane { lo: 0.0, hi: 100.0, runs: runs.into() }
    }

    fn flat_run(a: i64, b: i64, v: f64) -> Vec<[f64; 2]> {
        (a..=b).map(|f| [f as f64, v]).collect()
    }

    #[test]
    fn hover_hit_finds_the_nearest_curve_at_the_pointers_x_within_reach_and_ties_go_to_the_earlier_marker() {
        use super::{hit, Curve, HitLane, HOVER_REACH};
        use eframe::egui::{pos2, Rect};
        let m = test_map();
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 100.0));
        let own = |k| Curve { marker: k, laid_over: false };
        // Marker 0 at 50 (y 50), marker 1 at 60 (y 40.6), marker 2 at 50 too (a tie with 0),
        // marker 0's laid-over curve at 30 (y 68.8), marker 3 only up to frame 400 (a gap after).
        let (a, b, c) = (drawn_lane(vec![flat_run(0, 999, 50.0)]), drawn_lane(vec![flat_run(0, 999, 60.0)]), drawn_lane(vec![flat_run(0, 999, 50.0)]));
        let over = drawn_lane(vec![flat_run(0, 999, 30.0)]);
        let short = drawn_lane(vec![flat_run(0, 400, 80.0)]);
        let curves = vec![(own(0), &a), (Curve { marker: 0, laid_over: true }, &over), (own(1), &b), (own(2), &c), (own(3), &short)];
        let lanes = [HitLane { rect, map: m, curves: curves.clone() }];
        assert!((m.y(50.0) - 50.0).abs() < 1e-4 && (m.y(60.0) - 40.6).abs() < 1e-4);
        // On a curve; the tie of markers 0 and 2 goes to 0.
        assert_eq!(hit(&lanes, pos2(500.0, 50.0)), Some(own(0)));
        // Between two curves: the nearer one.
        assert_eq!(hit(&lanes, pos2(500.0, 44.0)), Some(own(1)), "3.4 from marker 1, 6 from marker 0");
        assert_eq!(hit(&lanes, pos2(500.0, 46.0)), Some(own(0)), "4 from marker 0, 5.4 from marker 1");
        // The laid-over (dashed) curve.
        assert_eq!(hit(&lanes, pos2(500.0, 70.0)), Some(Curve { marker: 0, laid_over: true }));
        // Farther than the reach: nothing.
        assert_eq!(hit(&lanes, pos2(500.0, 40.6 - HOVER_REACH - 0.5)), None);
        assert_eq!(hit(&lanes, pos2(500.0, 40.6 - HOVER_REACH + 0.5)), Some(own(1)));
        // Outside the lanes (below, left): nothing.
        assert_eq!(hit(&lanes, pos2(500.0, 150.0)), None);
        assert_eq!(hit(&lanes, pos2(-5.0, 50.0)), None);
        // A curve with a gap is not found in its gap (marker 3 ends at frame 400).
        assert_eq!(hit(&lanes, pos2(300.5, m.y(80.0))), Some(own(3)));
        assert_eq!(hit(&lanes, pos2(600.5, m.y(80.0))), None);
        // Ties go to the one first in the list (the earlier marker): the other order, marker 2.
        let mut swapped = curves.clone();
        swapped.swap(0, 3);
        assert_eq!(hit(&[HitLane { rect, map: m, curves: swapped }], pos2(500.0, 50.0)), Some(own(2)));
        // A spike drawn as a steep stroke in one column is found over its whole height.
        let spike = drawn_lane(vec![vec![[499.0, 0.0], [500.0, 0.0], [500.2, 100.0], [500.4, 0.0], [501.0, 0.0]]]);
        let only = [HitLane { rect, map: m, curves: vec![(own(5), &spike)] }];
        assert_eq!(hit(&only, pos2(500.5, 30.0)), Some(own(5)));
        assert_eq!(hit(&only, pos2(520.5, 30.0)), None, "away from the spike the curve is at 0 (y 97)");
        // Not a curve in the list (a marker not built yet): never found.
        assert_eq!(hit(&[HitLane { rect, map: m, curves: vec![] }], pos2(500.0, 50.0)), None);
    }

    #[test]
    fn markers_not_built_or_thinned_yet_and_hidden_files_have_no_curve_to_hover() {
        use super::{curves_in, Curve, Drawn, Thinned};
        let mut d = Drawn::default();
        d.set_names(vec!["A".into(), "B".into(), "C".into()]);
        d.lanes = vec![drawn_lane(vec![flat_run(0, 9, 1.0)])];
        d.pair_lanes = vec![drawn_lane(vec![flat_run(0, 9, 2.0)])];
        // B thinned, C pushed but not thinned yet (put off to a later frame).
        d.more_lanes = vec![Thinned { key: Some((0, 0, 1)), lanes: vec![drawn_lane(vec![flat_run(0, 9, 3.0)])] }, Thinned::default()];
        let got = |shown| curves_in(&d, 0, shown).into_iter().map(|(c, _)| c).collect::<Vec<_>>();
        let c = |marker, laid_over| Curve { marker, laid_over };
        assert_eq!(got([true, true]), vec![c(0, false), c(0, true), c(1, false)]);
        assert_eq!(got([false, true]), vec![c(0, true)], "the view's own file hidden (key 1)");
        assert_eq!(got([true, false]), vec![c(0, false), c(1, false)], "the laid-over file hidden (key 2)");
        // A graph without names (force plate, device): nothing to hover.
        let mut plate = Drawn::default();
        plate.lanes = d.lanes.drain(..).collect();
        assert!(curves_in(&plate, 0, [true, true]).is_empty());
    }

    #[test]
    fn the_hovered_name_sits_right_below_the_pointer_or_flips_to_stay_inside_the_lanes() {
        use super::{hover_text_at, HOVER_TEXT_AT};
        use eframe::egui::{pos2, vec2, Rect};
        let inside = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        let size = vec2(40.0, 13.0);
        assert_eq!(hover_text_at(pos2(100.0, 100.0), size, inside), pos2(100.0, 100.0) + HOVER_TEXT_AT);
        // Near the right edge: left of the pointer; near the bottom: above it; both in the corner.
        let r = hover_text_at(pos2(380.0, 100.0), size, inside);
        assert!(r.x + size.x < 380.0 && r.y == 116.0, "{r:?}");
        let b = hover_text_at(pos2(100.0, 290.0), size, inside);
        assert!(b.x == 114.0 && b.y + size.y < 290.0, "{b:?}");
        let c = hover_text_at(pos2(398.0, 298.0), size, inside);
        assert!(Rect::from_min_size(c, size).max.x <= 398.0 && Rect::from_min_size(c, size).max.y <= 298.0);
        // Never outside, even a name wider than the room.
        for p in [pos2(1.0, 1.0), pos2(399.0, 1.0), pos2(1.0, 299.0), pos2(200.0, 150.0)] {
            for s in [size, vec2(390.0, 13.0)] {
                let at = hover_text_at(p, s, inside);
                assert!(inside.contains(at) && at.x + s.x <= inside.right() && at.y + s.y <= inside.bottom(), "{p:?} {s:?} {at:?}");
            }
        }
    }

    /// Three lanes, each a flat curve at `value` in the lane colours X red, Y green, Z blue.
    fn axis_flat(value: f64) -> Vec<Lane> {
        use super::theme;
        [("X", theme::AXIS_X), ("Y", theme::AXIS_Y), ("Z", theme::AXIS_Z)]
            .iter()
            .map(|(l, color)| Lane { letter: (*l).into(), color: *color, runs: (vec![(0..4000).map(|f| [f as f64, value]).collect()]).into(), unit: "mm" })
            .collect()
    }

    /// A marker graph for the hover tests (v0.29.0): LASI (sine lanes ±1, ±2, ±3 in the lane
    /// colours), LKNE at 50 and RKNE at −40, drawn in one context frame by frame with the pointer
    /// events given; frame 2000 is the current frame.
    struct HoverGraph {
        ctx: eframe::egui::Context,
        drawn: super::Drawn,
        lanes: Vec<Lane>,
    }

    impl HoverGraph {
        fn new(names: bool, laid_over: bool) -> HoverGraph {
            use super::theme;
            let colors = [theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z];
            let lanes = (0..3)
                .map(|k| Lane { letter: ["X", "Y", "Z"][k].into(), color: colors[k], runs: (vec![(0..4000).map(|f| [f as f64, (f as f64 * 0.01).sin() * (k + 1) as f64]).collect()]).into(), unit: "mm" })
                .collect();
            let mut drawn = super::Drawn::default();
            if laid_over {
                drawn.set_pair(Some(super::Pair { lanes: axis_flat(-10.0), shown: [true, true] }));
            }
            drawn.set_more(vec![axis_flat(50.0), axis_flat(-40.0)]);
            if laid_over {
                drawn.set_more_pairs(vec![axis_flat(80.0), vec![]]);
            }
            if names {
                drawn.set_names(vec!["LASI".into(), "LKNE".into(), "RKNE".into()]);
            }
            HoverGraph { ctx: eframe::egui::Context::default(), drawn, lanes }
        }

        fn frame(&mut self, events: Vec<eframe::egui::Event>) -> (Vec<eframe::egui::epaint::ClippedShape>, super::Drew, Option<i64>) {
            use super::{show, Look};
            use eframe::egui;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0))), events, ..Default::default() };
            let (lanes, drawn) = (&self.lanes, &mut self.drawn);
            let mut goto = None;
            let out = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    goto = show(ui, egui::Id::new("g"), lanes, 0, 3999, 2000, &[], 100.0, &Look::pane(), Some(&mut *drawn), &[]);
                });
            });
            (out.shapes, self.drawn.drew.clone(), goto)
        }

        /// The pointer put at `p`, two frames (egui finds what is under the pointer from the
        /// frame before).
        fn hover(&mut self, p: eframe::egui::Pos2) -> (Vec<eframe::egui::epaint::ClippedShape>, super::Drew, Option<i64>) {
            self.frame(vec![eframe::egui::Event::PointerMoved(p)]);
            self.frame(vec![eframe::egui::Event::PointerMoved(p)])
        }

        /// Where marker value `v` is at frame `f` in lane `li`, as drawn.
        fn at(&self, li: usize, f: f64, v: f64) -> eframe::egui::Pos2 {
            let m = self.drawn.drew.maps[li].expect("drawn");
            eframe::egui::pos2(m.x(f), m.y(v))
        }
    }

    fn texts(s: &[eframe::egui::epaint::ClippedShape]) -> Vec<String> {
        use eframe::egui::Shape;
        s.iter().filter_map(|c| if let Shape::Text(t) = &c.shape { Some(t.galley.job.text.clone()) } else { None }).collect()
    }

    /// The paths drawn `HOVER_W` wide and their colours.
    fn thick(s: &[eframe::egui::epaint::ClippedShape]) -> Vec<eframe::egui::Color32> {
        use eframe::egui::{epaint::ColorMode, Shape};
        s.iter()
            .filter_map(|c| match &c.shape {
                Shape::Path(p) if p.stroke.width == super::HOVER_W => match p.stroke.color {
                    ColorMode::Solid(c) => Some(c),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_hovered_curve_is_thick_and_lighter_in_every_lane_and_its_name_plain_text_by_the_pointer() {
        use super::{theme, Curve, HOVER_LIGHTEN, HOVER_TEXT_AT};
        use eframe::egui::{Color32, Shape};
        let mut g = HoverGraph::new(true, false);
        let (base, before, _) = g.frame(vec![]);
        assert_eq!(before.hovered, None);
        assert_eq!(before.colors, vec![vec![theme::AXIS_X; 3], vec![theme::AXIS_Y; 3], vec![theme::AXIS_Z; 3]], "every marker in the lane's colour");
        // The pointer on LKNE's curve in the Y lane.
        let p = g.at(1, 2000.0, 50.0);
        let (shapes, d, goto) = g.hover(p);
        assert_eq!((d.hovered, d.hover_lanes, goto), (Some(Curve { marker: 1, laid_over: false }), 3, None), "hovering takes no click");
        // Its curve again in all three lanes, twice as wide, a little lighter; nothing else changed:
        // without those three lines and the name, the very same shapes.
        let light = |c: Color32| theme::lerp(c, Color32::WHITE, HOVER_LIGHTEN);
        assert_eq!(thick(&shapes), vec![light(theme::AXIS_X), light(theme::AXIS_Y), light(theme::AXIS_Z)]);
        assert!(thick(&base).is_empty());
        let rest: Vec<_> = shapes
            .iter()
            .filter(|c| !matches!(&c.shape, Shape::Path(p) if p.stroke.width == super::HOVER_W))
            .filter(|c| !matches!(&c.shape, Shape::Text(t) if t.galley.job.text == "LKNE"))
            .cloned()
            .collect();
        assert_eq!(rest, base, "the other curves, the texts and the background as they were");
        assert_eq!(d.remade, 0, "the lanes' own shapes were not made again");
        // The name: plain text (no rectangle behind it: nothing but the text was added) at the
        // pointer + (14, 16), 11 pt, light.
        assert_eq!(texts(&shapes).len(), texts(&base).len() + 1);
        let (text, r) = d.hover_text.clone().unwrap();
        assert_eq!(text, "LKNE");
        assert!((r.min - (p + HOVER_TEXT_AT)).length() < 1e-3, "{r:?} {p:?}");
        let galley = shapes.iter().find_map(|c| if let Shape::Text(t) = &c.shape { (t.galley.job.text == "LKNE").then(|| t.clone()) } else { None }).unwrap();
        assert_eq!(galley.galley.job.sections[0].format.font_id.size, super::HOVER_FONT);
        assert_eq!(galley.galley.job.sections[0].format.color, theme::HUD_TEXT);
        // The mouse off every curve: back to the very same picture.
        let (off, d, _) = g.hover(g.at(1, 2000.0, 20.0));
        assert_eq!((d.hovered, d.hover_text, &off), (None, None, &base));
    }

    #[test]
    fn hovering_in_and_out_makes_only_the_hovered_curve_and_keeps_it() {
        use super::Curve;
        let mut g = HoverGraph::new(true, false);
        g.frame(vec![]);
        let (lkne, rkne) = (g.at(1, 2000.0, 50.0), g.at(1, 2000.0, -40.0));
        let (_, d, _) = g.hover(lkne);
        assert_eq!((d.hovered, d.remade), (Some(Curve { marker: 1, laid_over: false }), 0));
        let (_, d, _) = g.frame(vec![eframe::egui::Event::PointerMoved(g.at(1, 2000.0, 20.0))]);
        assert_eq!((d.hovered, d.hover_made, d.remade), (None, 0, 0));
        // Back on LKNE: kept, nothing made; RKNE: its three lanes only.
        g.frame(vec![eframe::egui::Event::PointerMoved(lkne)]);
        assert_eq!((g.drawn.drew.hovered.map(|c| c.marker), g.drawn.drew.hover_made, g.drawn.drew.remade), (Some(1), 0, 0));
        g.frame(vec![eframe::egui::Event::PointerMoved(rkne)]);
        assert_eq!((g.drawn.drew.hovered.map(|c| c.marker), g.drawn.drew.hover_made, g.drawn.drew.remade), (Some(2), 3, 0));
    }

    #[test]
    fn the_laid_over_curve_hovered_says_dot_2_and_its_dashes_thicken() {
        use super::Curve;
        let mut g = HoverGraph::new(true, true);
        let (base, _, _) = g.frame(vec![]);
        // LKNE of the laid-over file at 80.
        let (shapes, d, _) = g.hover(g.at(1, 2000.0, 80.0));
        assert_eq!((d.hovered, d.hover_lanes), (Some(Curve { marker: 1, laid_over: true }), 3));
        assert_eq!(d.hover_text.unwrap().0, "LKNE · 2");
        // Dashes are one mesh per lane: three meshes more.
        let meshes = |s: &[eframe::egui::epaint::ClippedShape]| s.iter().filter(|c| matches!(c.shape, eframe::egui::Shape::Mesh(_))).count();
        assert_eq!(meshes(&shapes), meshes(&base) + 3);
        // The view's own LKNE (50) is its own curve still.
        let (_, d, _) = g.hover(g.at(1, 2000.0, 50.0));
        assert_eq!((d.hovered, d.hover_text.unwrap().0), (Some(Curve { marker: 1, laid_over: false }), "LKNE".to_string()));
    }

    #[test]
    fn the_name_stays_inside_the_graph_near_its_right_and_bottom_edges() {
        let mut g = HoverGraph::new(true, false);
        g.frame(vec![]);
        let m = g.drawn.drew.maps[2].unwrap();
        // RKNE (−40) is the lowest curve; near the right end of the Z lane (the bottom lane).
        let p = eframe::egui::pos2(m.left + m.width - 3.0, m.y(-40.0));
        let (_, d, _) = g.hover(p);
        assert_eq!(d.hovered.map(|c| c.marker), Some(2));
        let (text, r) = d.hover_text.unwrap();
        assert_eq!(text, "RKNE");
        assert!(r.max.x <= m.left + m.width && r.max.x < p.x, "left of the pointer: {r:?} {p:?}");
        assert!(r.max.y <= m.bottom && r.max.y < p.y, "above the pointer: {r:?} {p:?}");
    }

    #[test]
    fn no_hover_while_a_button_is_down_or_dragging_and_clicks_still_go_to_the_frame() {
        use eframe::egui::{Event, Modifiers, PointerButton};
        let mut g = HoverGraph::new(true, false);
        g.frame(vec![]);
        let p = g.at(1, 2000.0, 50.0);
        assert!(g.hover(p).1.hovered.is_some());
        let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        // Pressed on the curve: no hover; dragged along: no hover, and the graph goes to the
        // frames under the pointer as before.
        let (_, d, goto) = g.frame(vec![press(p, true)]);
        assert_eq!((d.hovered, d.hover_text), (None, None));
        let _ = goto;
        let mut last = None;
        for dx in [10.0, 30.0, 60.0] {
            let (_, d, goto) = g.frame(vec![Event::PointerMoved(p + eframe::egui::vec2(dx, 0.0))]);
            assert_eq!((d.hovered, d.hover_lanes), (None, 0), "dragging: no hover");
            last = goto.or(last);
        }
        let m = g.drawn.drew.maps[1].unwrap();
        assert_eq!(last, Some(m.frame(p.x + 60.0).round() as i64), "dragging still scrubs");
        // Let go: the hover comes back where the pointer is (60 pt along LKNE, still on it).
        g.frame(vec![press(p + eframe::egui::vec2(60.0, 0.0), false)]);
        let (_, d, _) = g.hover(p + eframe::egui::vec2(60.0, 0.0));
        assert_eq!(d.hovered.map(|c| c.marker), Some(1));
        // A click on a curve goes to that frame as before (and is not taken by the hover).
        let (_, _, a) = g.frame(vec![press(p, true)]);
        let (_, _, b) = g.frame(vec![press(p, false)]);
        assert_eq!(a.or(b), Some(m.frame(p.x).round() as i64));
    }

    #[test]
    fn one_marker_graphs_show_the_name_too_and_graphs_without_names_never_hover() {
        use super::{Curve, Drawn};
        // One marker (no others): the same hover.
        let mut g = HoverGraph::new(true, false);
        g.drawn = Drawn::default();
        g.drawn.set_names(vec!["LASI".into()]);
        g.frame(vec![]);
        let v = (2000.0_f64 * 0.01).sin() * 2.0;
        let (shapes, d, _) = g.hover(g.at(1, 2000.0, v));
        assert_eq!((d.hovered, d.hover_lanes, d.hover_text.map(|t| t.0)), (Some(Curve { marker: 0, laid_over: false }), 3, Some("LASI".to_string())));
        assert_eq!(thick(&shapes).len(), 3);
        // No names (a force plate or device graph): nothing, ever.
        let mut plate = HoverGraph::new(false, false);
        plate.frame(vec![]);
        let (shapes, d, _) = plate.hover(plate.at(1, 2000.0, 50.0));
        assert_eq!((d.hovered, d.hover_text, thick(&shapes).len()), (None, None, 0));
    }

    #[test]
    fn a_forced_hover_puts_the_pointer_on_the_markers_y_curve_right_of_the_frame_line() {
        use super::{Curve, FORCED_HOVER_DX, HOVER_TEXT_AT};
        let mut g = HoverGraph::new(true, false);
        g.drawn.set_hover_force(Some(2));
        let (_, d, _) = g.frame(vec![]);
        assert_eq!((d.hovered, d.hover_lanes), (Some(Curve { marker: 2, laid_over: false }), 3));
        let m = d.maps[1].unwrap();
        let (text, r) = d.hover_text.unwrap();
        assert_eq!(text, "RKNE");
        let p = eframe::egui::pos2(m.x(2000.0) + FORCED_HOVER_DX, m.y(-40.0));
        assert!((r.min.x - (p + HOVER_TEXT_AT).x).abs() < 1e-3, "{r:?} {p:?}");
        g.drawn.set_hover_force(None);
        assert_eq!(g.frame(vec![]).1.hovered, None);
    }
}
