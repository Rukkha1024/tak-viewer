//! Tracks shows sample presence using the display model's prepared marker runs.
use super::{
    paint_selection, paint_spans, select_drag, theme, DecorationGeometry, Decorations,
    NavDecorationKey, RangeDragAxis, ViewRange, NAV_H,
};
use crate::ui::panels::{self, ListSelectionState, SelectionGesture, SpanCache};
use eframe::egui::{self, pos2, vec2, Color32, FontId, Id, Rect, Sense, Stroke, Ui};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::{Arc, OnceLock};
use tak_core::comments::Comments;
use tak_core::tak::Uid;
use tak_core::view_data::{MarkerOrder, ViewData};

const ROW_H: f32 = 19.0;
const PRESENT: Color32 = Color32::from_rgb(0x48, 0xb4, 0x67);
const SELECTED: Color32 = Color32::from_rgb(0xf8, 0xce, 0x46);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum Layout {
    #[default]
    Channels,
    Tracks,
    Combined,
}

impl Layout {
    fn name(self) -> &'static str {
        match self {
            Self::Channels => "Channels",
            Self::Tracks => "Tracks",
            Self::Combined => "Combined",
        }
    }
}

pub(crate) fn selector(ui: &mut Ui, layout: &mut Layout, controls: impl FnOnce(&mut Ui, Layout)) -> bool {
    let mut over_3d = false;
    let row = ui.horizontal(|ui| {
        if *layout == Layout::Tracks {
            over_3d = panels::over_3d_button(ui);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt("graph_layout")
                .width(95.0)
                .selected_text(layout.name())
                .show_ui(ui, |ui| {
                    for choice in [Layout::Channels, Layout::Tracks, Layout::Combined] {
                        ui.selectable_value(layout, choice, choice.name());
                    }
                });
            controls(ui, *layout);
        });
    });
    crate::app::key_focus::region(ui, row.response.rect, crate::app::key_focus::Target::Blocked);
    over_3d
}

pub(crate) fn graph_id(file_id: usize) -> Id {
    Id::new(("sel_graph", file_id))
}

#[derive(Default)]
pub(crate) struct State {
    file_id: Option<usize>,
    order: Option<Arc<OnceLock<MarkerOrder>>>,
    selection: ListSelectionState,
    last_primary: Option<Uid>,
    offset: f32,
    spans: SpanCache,
    decorations: Decorations,
}

impl State {
    pub(super) fn prepare(&mut self, data: &Arc<ViewData>, file_id: usize, ui: &Ui) {
        if self.file_id != Some(file_id) {
            self.file_id = Some(file_id);
            self.selection = ListSelectionState::default();
            self.offset = 0.0;
        }
        let handle = data.marker_order_handle();
        if self
            .order
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(current, handle))
        {
            crate::views::drop_later(self.order.replace(handle.clone()));
            self.last_primary = None;
            if handle.get().is_none() {
                let data = data.clone();
                let ctx = ui.ctx().clone();
                crate::work_queue::geometry(move || {
                    data.prepare_marker_order();
                    crate::work_queue::retire(data);
                    ctx.request_repaint();
                });
            }
        }
    }

    pub(super) fn order(&self, data: &ViewData) -> Option<&MarkerOrder> {
        let handle = self.order.as_ref()?;
        Arc::ptr_eq(handle, data.marker_order_handle())
            .then(|| handle.get())
            .flatten()
    }

    pub(crate) fn select(
        &mut self,
        data: &ViewData,
        current: &[usize],
        click: Click,
    ) -> Option<Vec<usize>> {
        let order = self.order.as_ref()?;
        if !Arc::ptr_eq(order, data.marker_order_handle()) {
            return None;
        }
        let order = order.get()?;
        let clicked = data.markers.get(click.marker)?.uid;
        let current: Vec<_> = current
            .iter()
            .filter_map(|&i| data.markers.get(i).map(|m| m.uid))
            .collect();
        self.selection
            .apply(&order.uids, &current, Some(clicked), click.gesture)
            .map(|uids| {
                uids.iter()
                    .filter_map(|uid| order.marker_by_uid.get(uid).copied())
                    .collect()
            })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Click {
    marker: usize,
    gesture: SelectionGesture,
}

#[derive(Default)]
pub(crate) struct Interaction {
    pub click: Option<Click>,
    pub seek: Option<i64>,
}

fn gesture(m: egui::Modifiers) -> Option<SelectionGesture> {
    if m.alt {
        return None;
    }
    let additive = m.ctrl || m.command;
    Some(if m.shift {
        SelectionGesture::Range { additive }
    } else if additive {
        SelectionGesture::Toggle
    } else {
        SelectionGesture::Plain
    })
}

fn visible_rows(offset: f32, height: f32, rows: usize) -> Range<usize> {
    let first = (offset / ROW_H).floor() as usize;
    let end = ((offset + height) / ROW_H).ceil() as usize;
    first.min(rows)..end.min(rows)
}

fn reveal_offset(row: usize, offset: f32, height: f32) -> Option<f32> {
    let top = row as f32 * ROW_H;
    if top < offset {
        Some(top)
    } else if top + ROW_H > offset + height {
        Some((top + ROW_H - height).max(0.0))
    } else {
        None
    }
}

fn hit(order: &MarkerOrder, local_y: f32, modifiers: egui::Modifiers) -> Option<Click> {
    if local_y < 0.0 {
        return None;
    }
    let marker = *order.indices.get((local_y / ROW_H).floor() as usize)?;
    Some(Click {
        marker,
        gesture: gesture(modifiers)?,
    })
}

/// Rasterize inclusive frame runs to occupied columns. Once a column is covered, binary
/// search past any remaining fragments that end in it. Output and loop count are bounded
/// by screen width even for alternating samples across a long take.
fn pixel_runs(runs: &[(i64, i64)], vr: ViewRange, columns: u32) -> Vec<Range<u32>> {
    if columns == 0 {
        return Vec::new();
    }
    let mut result: Vec<Range<u32>> = Vec::new();
    let scale = f64::from(columns) / (vr.hi - vr.lo);
    let mut i = runs.partition_point(|&(_, end)| end as f64 + 0.5 <= vr.lo);
    while let Some(&(start, end)) = runs.get(i) {
        let start = start as f64 - 0.5;
        if start >= vr.hi {
            break;
        }
        let left = ((start - vr.lo) * scale).floor().max(0.0) as u32;
        let right = ((end as f64 + 0.5 - vr.lo) * scale)
            .ceil()
            .min(f64::from(columns)) as u32;
        if let Some(last) = result.last_mut().filter(|last| last.end >= left) {
            last.end = last.end.max(right);
        } else if left < right {
            result.push(left..right);
        }
        if right == columns {
            break;
        }
        i += 1;
        // Use the same forward mapping for the skip predicate at exact pixel boundaries.
        let covered = |end: i64| ((end as f64 + 0.5 - vr.lo) * scale).ceil() <= f64::from(right);
        if runs.get(i).is_some_and(|&(_, end)| covered(end)) {
            i += runs[i..].partition_point(|&(_, end)| covered(end));
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn show(
    ui: &mut Ui,
    state: &mut State,
    data: &Arc<ViewData>,
    file_id: usize,
    frame: i64,
    selected: &BTreeSet<usize>,
    primary: Option<usize>,
    range: &mut Option<(i64, i64)>,
    comments: &Comments,
    picked: Option<u64>,
) -> Interaction {
    if ui.is_sizing_pass() || !ui.is_visible() || ui.ctx().will_discard() {
        ui.allocate_space(ui.available_size());
        return Interaction::default();
    }

    state.prepare(data, file_id, ui);
    let order = state.order.as_ref().and_then(|order| order.get());
    let count = order.map_or(0, |order| order.indices.len());
    let first = data.start_frame;
    let last = data.end_frame;
    let id = graph_id(file_id);
    let full = ViewRange::full(first, last);
    let mut vr = ViewRange::load(ui, id, full);
    let height =
        (ui.available_height() - NAV_H - panels::BAND_H - 2.0 * ui.spacing().item_spacing.y)
            .max(ROW_H);
    let primary_uid = primary.and_then(|m| data.markers.get(m)).map(|m| m.uid);
    let offset = match (primary, order) {
        (Some(m), Some(order)) if state.last_primary != primary_uid => {
            state.last_primary = primary_uid;
            order.row(m).and_then(|row| reveal_offset(row, state.offset, height))
        }
        (None, _) => {
            state.last_primary = None;
            None
        }
        _ => None,
    };
    let max_offset = (count as f32 * ROW_H - height).max(0.0);
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(id.with("tracks_rows"))
        .max_height(height)
        .min_scrolled_height(height)
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .drag_to_scroll(false);
    if let Some(offset) = offset.or((state.offset > max_offset).then_some(max_offset)) {
        scroll = scroll.vertical_scroll_offset(offset);
    }
    let out = ui
        .scope(|ui| {
            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
            scroll.show_viewport(ui, |ui, viewport| {
                ui.set_min_size(vec2(viewport.width(), height.max(count as f32 * ROW_H)));
                let top = ui.max_rect().top();
                let body = Rect::from_min_size(
                    ui.max_rect().min + vec2(0.0, viewport.top()),
                    viewport.size(),
                );
                let response = ui.interact(body, id.with("tracks_body"), Sense::click_and_drag());
                (response, top, viewport.top())
            })
        })
        .inner;
    state.offset = out.state.offset.y;
    let body = out.inner_rect;
    let (rows, top, offset) = out.inner;
    let name_width = (body.width() * 0.25)
        .clamp(84.0, 148.0)
        .min(body.width() * 0.52);
    let plot = Rect::from_min_max(body.min + vec2(name_width, 0.0), body.max);
    let (_, strips) = ui.allocate_space(vec2(ui.available_width(), NAV_H + panels::BAND_H));
    let nav = Rect::from_min_max(
        pos2(plot.left(), strips.top()),
        pos2(plot.right(), strips.top() + NAV_H),
    );
    let band = Rect::from_min_max(nav.left_bottom(), pos2(nav.right(), strips.bottom()));
    let ticks = ui.interact(nav, id.with("tracks_ticks"), Sense::click_and_drag());
    let range_strip = ui.interact(band, id.with("tracks_range"), Sense::click_and_drag());
    let zoom = ticks
        .hover_pos()
        .or_else(|| range_strip.hover_pos())
        .and_then(|p| {
            let dy = ui.input(|i| i.raw_scroll_delta.y);
            (dy != 0.0).then_some((p.x, dy))
        });
    let pan = [&rows, &ticks, &range_strip]
        .iter()
        .find(|r| r.dragged_by(egui::PointerButton::Middle))
        .map_or(0.0, |r| r.drag_delta().x);
    vr.navigate(full, plot, zoom, pan);
    let reset = ticks.double_clicked();
    if reset {
        vr = full;
    }
    ui.data_mut(|d| d.insert_temp(id, vr));
    let frame_at = |x| vr.at(plot, x).round().clamp(first as f64, last as f64) as i64;
    let mut action = Interaction::default();
    if !reset && (ticks.clicked() || ticks.dragged_by(egui::PointerButton::Primary)) {
        action.seek = ticks.interact_pointer_pos().map(|p| frame_at(p.x));
    }
    let (_, selecting) = select_drag(
        ui,
        &range_strip,
        id.with("tracks_range"),
        band,
        RangeDragAxis::Any,
        frame_at,
        first,
        last,
        range,
    );
    if !selecting && range_strip.clicked() {
        *range = None;
    }
    let (_, selecting_rows) = select_drag(
        ui,
        &rows,
        rows.id,
        plot,
        RangeDragAxis::Horizontal,
        frame_at,
        first,
        last,
        range,
    );
    if let (Some(order), Some(p)) = (
        order,
        rows.interact_pointer_pos()
            .filter(|_| !selecting_rows && rows.clicked_by(egui::PointerButton::Primary)),
    ) {
        action.click = hit(order, p.y - top, ui.input(|i| i.modifiers));
    }

    let painter = ui.painter_at(body);
    painter.rect_filled(body, 0.0, theme::GRAPH_BG);
    let strip_painter = ui.painter_at(strips);
    strip_painter.rect_filled(strips, 0.0, theme::NAV_BG);
    strip_painter.rect_filled(band, 0.0, theme::BAND_BG);
    if plot.is_positive() {
        state.decorations.begin(ui);
        let geometry = DecorationGeometry {
            rect: nav,
            clip: strip_painter.clip_rect(),
            plot_x: (plot.left(), plot.width()),
            frames: (vr.lo.to_bits(), vr.hi.to_bits()),
            pixels_per_point: ui.ctx().pixels_per_point(),
            options: ui.ctx().tessellation_options(Clone::clone),
        };
        state.decorations.prepare_nav(
            &strip_painter,
            NavDecorationKey {
                geometry,
                bg: theme::NAV_BG,
            },
        );
        let nav_shapes = state.decorations.nav.as_ref().unwrap();
        strip_painter.add(nav_shapes.background.clone());
        strip_painter.extend(nav_shapes.ticks.iter().cloned());
        paint_selection(&strip_painter, band, vr.lo, vr.hi, *range);
    }

    if let Some(order) = order {
        let visible = visible_rows(offset, body.height(), count);
        let displayed = &order.indices[visible.clone()];
        let names: Vec<_> = displayed
            .iter()
            .map(|&m| data.markers[m].name.as_str())
            .collect();
        let spans = state.spans.get_many(comments, file_id, &names, picked);
        let time_painter = painter.with_clip_rect(plot);
        let label_painter = painter.with_clip_rect(Rect::from_min_max(
            body.min,
            pos2(plot.left(), body.bottom()),
        ));
        paint_spans(&time_painter, plot, vr.lo, vr.hi, spans);
        let columns = (plot.width() * ui.ctx().pixels_per_point()).ceil() as u32;
        let column_w = plot.width() / columns.max(1) as f32;
        for (row, &m) in visible.zip(displayed) {
            let y = top + row as f32 * ROW_H;
            let bar_color = if selected.contains(&m) {
                SELECTED
            } else {
                PRESENT
            };
            let name_color = if selected.contains(&m) {
                SELECTED
            } else {
                theme::TEXT
            };
            label_painter.text(
                pos2(body.left() + 6.0, y + ROW_H / 2.0),
                egui::Align2::LEFT_CENTER,
                &data.markers[m].name,
                FontId::proportional(11.0),
                name_color,
            );
            painter.hline(
                body.x_range(),
                y + ROW_H,
                Stroke::new(1.0_f32, theme::GRAPH_SEP),
            );
            for pixels in pixel_runs(&data.markers[m].segments, vr, columns) {
                let x = |c: u32| plot.left() + c as f32 * column_w;
                time_painter.rect_filled(
                    Rect::from_min_max(
                        pos2(x(pixels.start), y + 5.0),
                        pos2(x(pixels.end), y + ROW_H - 5.0),
                    ),
                    0.0,
                    bar_color,
                );
            }
        }
        if let Some(p) = rows.hover_pos().filter(|p| plot.contains(*p)) {
            let row = ((p.y - top) / ROW_H).floor() as usize;
            if let Some(&m) = order.indices.get(row) {
                rows.on_hover_text(format!("{} · {}", data.markers[m].name, frame_at(p.x)));
            }
        }
        if count == 0 {
            painter.text(
                body.center(),
                egui::Align2::CENTER_CENTER,
                "No markers in this take",
                FontId::proportional(12.0),
                theme::TEXT_DIM,
            );
        }
    } else {
        super::mark_geometry_pending();
        painter.text(
            body.center(),
            egui::Align2::CENTER_CENTER,
            "Loading tracks…",
            FontId::proportional(12.0),
            theme::TEXT_DIM,
        );
    }
    paint_selection(&painter, plot, vr.lo, vr.hi, *range);
    let cursor = vr.x(plot, frame as f64);
    if cursor >= plot.left() && cursor <= plot.right() {
        painter.vline(
            cursor,
            body.y_range(),
            Stroke::new(1.0_f32, theme::GRAPH_CURSOR),
        );
    }
    action
}

pub(crate) struct Probe {
    pub layout: Layout,
    pub zoom: Option<(i64, i64)>,
    pub fit_range: bool,
    pub magnification: f64,
    pub lock: Option<String>,
}

pub(crate) static PROBE: OnceLock<Probe> = OnceLock::new();

pub(crate) fn parse_probe(args: &mut Vec<String>) -> Result<(), &'static str> {
    let Some(index) = args.iter().position(|a| a == "--graph-layout") else {
        return if args.iter().any(|a| matches!(a.as_str(), "--graph-zoom" | "--graph-fit-range" | "--graph-y-zoom" | "--graph-lock")) {
            Err("그래프 확인 옵션은 --graph-layout과 함께 써요.")
        } else {
            Ok(())
        };
    };
    if !args.iter().any(|a| a == "--screenshot" || a == "--bench") {
        return Err("--graph-layout은 --screenshot 또는 --bench와 함께 써요.");
    }
    let layout = match args.get(index + 1).map(String::as_str) {
        Some("channels") => Layout::Channels,
        Some("tracks") => Layout::Tracks,
        Some("combined") => Layout::Combined,
        _ => return Err("--graph-layout 뒤에 channels, tracks, combined 중 하나를 써요."),
    };
    args.drain(index..index + 2);
    let zoom = if let Some(index) = args.iter().position(|a| a == "--graph-zoom") {
        let range = args
            .get(index + 1)
            .and_then(|a| crate::shot::parse_range(a))
            .ok_or("--graph-zoom 뒤에 시작-끝 프레임을 써요. 예: 300-900")?;
        args.drain(index..index + 2);
        Some(range)
    } else {
        None
    };
    let fit_range = args.iter().position(|a| a == "--graph-fit-range").map(|index| args.remove(index)).is_some();
    let magnification = if let Some(index) = args.iter().position(|a| a == "--graph-y-zoom") {
        let by = args.get(index + 1).and_then(|a| a.parse::<f64>().ok())
            .filter(|n| n.is_finite() && *n > 0.0)
            .ok_or("--graph-y-zoom 뒤에 0보다 큰 배율을 써요.")?;
        args.drain(index..index + 2);
        Some(by)
    } else {
        None
    };
    let lock = if let Some(index) = args.iter().position(|a| a == "--graph-lock") {
        let names = args.get(index + 1).filter(|a| !a.starts_with("--") && !a.trim().is_empty())
            .ok_or("--graph-lock 뒤에 마커 이름을 써요.")?.clone();
        args.drain(index..index + 2);
        Some(names)
    } else {
        None
    };
    if layout == Layout::Tracks && (magnification.is_some() || lock.is_some()) {
        return Err("값 확대와 마커 고정은 Channels와 Combined에서 써요.");
    }
    let _ = PROBE.set(Probe { layout, zoom, fit_range, magnification: magnification.unwrap_or(1.0), lock });
    Ok(())
}

pub(crate) fn set_zoom(ctx: &egui::Context, file_id: usize, (a, b): (i64, i64), data: &ViewData) {
    let (a, b) = panels::ordered_range(a, b, data.start_frame, data.end_frame);
    ctx.data_mut(|d| d.insert_temp(graph_id(file_id), ViewRange::full(a, b)));
}

#[cfg(test)]
pub(crate) fn zoom_extent(ctx: &egui::Context, file_id: usize) -> Option<(f64, f64)> {
    ctx.data(|d| d.get_temp::<ViewRange>(graph_id(file_id))).map(|vr| (vr.lo, vr.hi))
}

#[cfg(test)]
#[path = "graph_tracks_tests.rs"]
mod tests;
