//! Devices pane: a metadata-only list and at most sixteen cached, demand-loaded graphs.
use super::{
    graph::{self, Lane, Look},
    graph_float::PLATE_LANES,
    panels::Action,
    theme,
};
use crate::{
    device_filter::{self, Display, SharedFilters},
    device_grid::{self as model, Cell, ChannelNames, Layout},
};
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontId, Id, Rect, RichText, Sense, Stroke, Ui,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tak_core::view_data::{SignalView, ViewData};

#[derive(Default)]
pub struct State {
    // Retained for migration of existing in-memory history snapshots.
    pub channel: HashMap<usize, usize>,
    pub request_emg: bool,
    pub open_device: Option<usize>,
    pub only_device: bool,
    pub layout: Option<Layout>,
    pub folded: Option<bool>,
    expanded: HashSet<usize>,
    pub note: Option<&'static str>,
    save_name: Option<String>,
    focus_name: bool,
    graphs: HashMap<usize, CellGraph>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Arrangement {
    pub layout: Option<Layout>,
    pub folded: Option<bool>,
}
#[derive(Clone)]
struct Payload {
    label: String,
    items: Vec<Cell>,
    from: Option<usize>,
}
impl State {
    pub fn arrangement(&self) -> Arrangement {
        Arrangement {
            layout: self.layout.clone(),
            folded: self.folded,
        }
    }
    pub fn restore(&mut self, next: &Arrangement) {
        self.layout = next.layout.clone();
        self.folded = next.folded;
        self.reconcile();
    }
    pub fn reconcile(&mut self) {
        let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("device_grid.reconcile"));
        let Some(layout) = &self.layout else {
            return;
        };
        let stale: Vec<_> = self
            .graphs
            .iter()
            .filter(|(at, graph)| {
                layout.cells.get(**at).and_then(Option::as_ref) != Some(&graph.key.1)
            })
            .map(|(&at, _)| at)
            .collect();
        for at in stale {
            crate::views::drop_later(self.graphs.remove(&at));
        }
    }

    pub fn reset_graphs(&mut self) {
        crate::views::drop_later(Some(std::mem::take(self)));
    }
    pub fn graphs_pending(&self) -> bool {
        self.graphs.values().any(|g| g.drawn.background_pending())
    }
    pub fn ready_cells(&self) -> (usize, usize) {
        (
            self.graphs.len(),
            self.graphs
                .values()
                .filter(|g| g._display.output.is_some())
                .count(),
        )
    }
}

struct CellGraph {
    key: (usize, Cell, usize, bool, u64),
    // Retain sources so pointer cache keys cannot be reused after a result is evicted.
    _display: Display,
    signal: Arc<SignalView>,
    lanes: Vec<Lane>,
    drawn: graph::Drawn,
}
impl CellGraph {
    fn new(
        data: &ViewData,
        cell: &Cell,
        di: usize,
        channels: &[usize],
        display: &Display,
        file_id: usize,
    ) -> Self {
        let d = &data.devices[di];
        let processed = display.output.is_some();
        let raw = processed && display.show_raw;
        let make = |ci: usize, signal: &Arc<SignalView>, shade: f32| {
            let (letter, color) = if d.plate.is_some() && ci < 6 {
                (PLATE_LANES[ci].0.to_owned(), PLATE_LANES[ci].1)
            } else {
                (
                    d.channels[ci]
                        .name
                        .trim_start_matches("AnalogChannel")
                        .to_owned(),
                    Color32::from_rgb(200, 200, 200),
                )
            };
            Lane {
                letter,
                color: color.gamma_multiply(shade),
                runs: signal.runs.clone(),
                unit: "",
            }
        };
        let output = |ci: usize| {
            display
                .output
                .as_ref()
                .and_then(|o| o.channels.get(ci))
                .unwrap_or(&d.channels[ci].signal)
        };
        let lanes = channels
            .iter()
            .map(|&ci| {
                if raw {
                    make(ci, &d.channels[ci].signal, 0.22)
                } else {
                    make(ci, output(ci), 1.0)
                }
            })
            .collect();
        let mut drawn = graph::Drawn::background();
        if d.plate.is_none() && (!processed || raw) {
            let shade = if raw { 0.14 } else { 0.35 };
            drawn.push_more(
                channels
                    .iter()
                    .map(|&ci| make(ci, &d.channels[ci].raw_min, shade))
                    .collect(),
            );
            drawn.push_more(
                channels
                    .iter()
                    .map(|&ci| make(ci, &d.channels[ci].raw_max, shade))
                    .collect(),
            );
        }
        if raw {
            drawn.push_more(
                channels
                    .iter()
                    .map(|&ci| make(ci, output(ci), 1.0))
                    .collect(),
            );
        }
        if d.plate.is_none() && processed {
            for upper in [false, true] {
                drawn.push_more(
                    channels
                        .iter()
                        .map(|&ci| {
                            let band = display
                                .output
                                .as_ref()
                                .and_then(|o| o.bands.get(ci))
                                .and_then(Option::as_ref);
                            make(
                                ci,
                                band.map_or(output(ci), |b| if upper { &b.1 } else { &b.0 }),
                                0.35,
                            )
                        })
                        .collect(),
                );
            }
        }
        Self {
            key: graph_key(data, cell, display, file_id),
            _display: display.clone(),
            signal: output(channels[0]).clone(),
            lanes,
            drawn,
        }
    }
}
fn graph_key(
    data: &ViewData,
    cell: &Cell,
    display: &Display,
    file: usize,
) -> (usize, Cell, usize, bool, u64) {
    (
        file,
        cell.clone(),
        display
            .output
            .as_ref()
            .map_or(0, |o| Arc::as_ptr(o) as usize),
        display.show_raw,
        data.analog_generation(),
    )
}

fn device_title(data: &ViewData, di: usize) -> String {
    let d = &data.devices[di];
    d.plate
        .and_then(|pi| data.plates.get(pi))
        .map(|p| p.name.clone())
        .unwrap_or_else(|| {
            data.take
                .device_names
                .get(di)
                .and_then(Option::as_ref)
                .map_or_else(
                    || format!("Analog device {} ({} ch)", di + 1, d.channels.len()),
                    |name| format!("{name} ({} ch)", d.channels.len()),
                )
        })
}
fn cell_title(
    data: &ViewData,
    cell: &Cell,
    di: usize,
    channels: &[usize],
    names: &ChannelNames,
) -> String {
    match cell {
        Cell::Plate { .. } if channels.len() == 1 => format!(
            "{} · {}",
            device_title(data, di),
            PLATE_LANES[channels[0]].0
        ),
        Cell::Plate { .. } => device_title(data, di),
        _ => model::display_name(names, data, di, channels[0]).to_owned(),
    }
}
pub fn display_badge(display: &Display) -> Option<&str> {
    display
        .output
        .as_ref()
        .map(|o| o.badge.as_str())
        .filter(|s| !s.is_empty())
}

fn source(ui: &mut Ui, text: String, items: impl FnOnce() -> Vec<Cell>, suffix: Option<String>) {
    let (grip, _) = ui.allocate_exact_size(vec2(9.0, 16.0), Sense::hover());
    for x in [2.0, 6.0] {
        for y in [4.0, 8.0, 12.0] {
            ui.painter()
                .circle_filled(grip.min + vec2(x, y), 0.8, theme::TEXT_DIM);
        }
    }
    let response = ui
        .add(
            egui::Label::new(RichText::new(text.as_str()).size(11.0))
                .selectable(false)
                .truncate()
                .sense(Sense::drag()),
        )
        .on_hover_cursor(egui::CursorIcon::Grab);
    let grip_id = response.id.with("grip");
    let response = response.union(ui.interact(grip, grip_id, Sense::drag()).on_hover_cursor(egui::CursorIcon::Grab));
    if response.drag_started() {
        response.dnd_set_drag_payload(Payload {
            label: text.trim_start_matches('●').trim().to_owned(),
            items: items(),
            from: None,
        });
    }
    if let Some(suffix) = suffix {
        ui.label(RichText::new(suffix).size(9.0).color(theme::TEXT_DIM));
    }
}
fn presets(ui: &mut Ui, st: &mut State, saved: &mut Vec<Layout>, data: &ViewData) {
    ui.menu_button(RichText::new("Saved layouts").size(11.0), |ui| {
        if saved.is_empty() {
            ui.label(RichText::new("No saved layouts").weak());
        }
        let mut remove = None;
        for (i, layout) in saved.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.button(&layout.name).clicked() {
                    let mut next = layout.clone();
                    next.normalize();
                    next.canonicalize(data);
                    if st.layout.as_ref() != Some(&next) {
                        st.layout = Some(next);
                        st.note = Some("Layout loaded");
                        st.reconcile();
                    }
                    ui.close_menu();
                }
                if ui.small_button("×").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            saved.remove(i);
        }
    });
}
fn save_button(ui: &mut Ui, st: &mut State) {
    if ui
        .small_button(RichText::new("+ Save layout").size(11.0))
        .clicked()
    {
        st.save_name = Some(String::new());
        st.focus_name = true;
        ui.close_menu();
    }
}
fn sizes(ui: &mut Ui, st: &mut State) {
    ui.label(RichText::new("Grid").small().color(theme::TEXT_DIM));
    for n in 1..=4 {
        let on = st
            .layout
            .as_ref()
            .is_some_and(|l| l.rows == n && l.cols == n);
        let button = egui::Button::new(RichText::new(format!("{n}×{n}")).size(11.0))
            .small()
            .fill(if on {
                theme::ACTIVE_VIEW
            } else {
                Color32::from_gray(54)
            });
        if ui.add(button).clicked() && !on {
            st.layout.as_mut().unwrap().resize(n);
            st.note = Some("Grid resized");
            st.reconcile();
        }
    }
}
fn toolbar(ui: &mut Ui, st: &mut State, saved: &mut Vec<Layout>, data: &ViewData) {
    let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("device_grid.toolbar"));
    let width = ui.available_width();
    let row = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            if width >= 235.0 {
                sizes(ui, st);
            }
            if width >= 450.0 {
                presets(ui, st, saved, data);
                save_button(ui, st);
            } else {
                ui.menu_button("···", |ui| {
                    if width < 235.0 {
                        sizes(ui, st);
                    }
                    presets(ui, st, saved, data);
                    save_button(ui, st);
                });
            }
        })
        .response
        .rect;
    if let Some(name) = &mut st.save_name {
        let mut commit = false;
        let mut cancel = false;
        egui::Area::new(ui.id().with("save_layout_popover"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(
                (row.right() - 230.0).max(row.left()),
                row.bottom() + 3.0,
            ))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(210.0);
                    ui.label("Layout name");
                    let response = ui.add(
                        egui::TextEdit::singleline(name)
                            .desired_width(200.0)
                            .char_limit(64),
                    );
                    if st.focus_name {
                        response.request_focus();
                        st.focus_name = false;
                    }
                    ui.horizontal(|ui| {
                        commit = ui
                            .add_enabled(!name.trim().is_empty(), egui::Button::new("Save"))
                            .clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        commit = !name.trim().is_empty();
                    }
                    cancel |= ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if commit || cancel {
                        response.surrender_focus();
                    }
                });
            });
        if cancel {
            st.save_name = None;
        } else if commit {
            let mut layout = st.layout.as_ref().unwrap().clone();
            layout.name = name.trim().to_owned();
            layout.canonicalize(data);
            if let Some(i) = saved.iter().position(|l| l.name == layout.name) {
                saved.remove(i);
            }
            saved.push(layout);
            st.save_name = None;
        }
    }
}

fn list(
    ui: &mut Ui,
    data: &Arc<ViewData>,
    st: &mut State,
    file_id: usize,
    chosen: Option<usize>,
    filters: &SharedFilters,
    names: &ChannelNames,
) -> Option<Action> {
    let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("device_grid.list"));
    let mut action = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Devices").small().color(theme::TEXT_DIM));
        if ui.small_button("◀").clicked() {
            st.folded = Some(true);
        }
    });
    egui::ScrollArea::vertical()
        .id_salt(("device_list", file_id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (di, d) in data.devices.iter().enumerate() {
                if st.open_device == Some(di) {
                    st.expanded.insert(di);
                }
                ui.push_id(di, |ui| {
                    ui.horizontal(|ui| {
                        let expanded = st.expanded.contains(&di);
                        if ui.small_button(if expanded { "▼" } else { "▶" }).clicked() {
                            if expanded {
                                st.expanded.remove(&di);
                                if st.open_device == Some(di) {
                                    st.open_device = None;
                                }
                            } else {
                                st.expanded.insert(di);
                            }
                        }
                        let display = filters
                            .lock()
                            .unwrap()
                            .devices
                            .get(&di)
                            .map(|f| f.display())
                            .unwrap_or_default();
                        // Reserve actions before truncating the draggable title/badge.
                        let badge = display_badge(&display);
                        let badge_w = badge.map_or(0.0, |text| {
                            (text.chars().count() as f32 * 5.0 + 8.0).min(80.0)
                        });
                        let action_w = if d.plate.is_none() { 52.0 } else { 28.0 };
                        let gaps =
                            ui.spacing().item_spacing.x * if badge.is_some() { 2.0 } else { 1.0 };
                        let name_w = (ui.available_width() - badge_w - action_w - gaps).max(28.0);
                        ui.allocate_ui_with_layout(
                            vec2(name_w, 18.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                source(ui, device_title(data, di), || model::items(data, di), None);
                            },
                        );
                        if let Some(badge) = badge {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(badge_w, 17.0), Sense::hover());
                            ui.painter()
                                .rect_filled(rect, 2.0, Color32::from_rgb(9, 71, 113));
                            ui.put(
                                rect,
                                egui::Label::new(
                                    RichText::new(badge).size(9.5).color(Color32::WHITE),
                                )
                                .selectable(false)
                                .truncate(),
                            );
                        }
                        if d.plate.is_none()
                            && ui
                                .add_sized(
                                    [action_w, 18.0],
                                    egui::Button::new(RichText::new("Fill grid").size(10.5))
                                        .small(),
                                )
                                .clicked()
                        {
                            let before = st.layout.clone();
                            st.layout.as_mut().unwrap().fill_device(data, di);
                            if st.layout != before {
                                st.note = Some("Grid filled");
                                st.reconcile();
                            }
                        }
                        if let Some(pi) = d.plate {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new("3D")
                                                .small()
                                                .fill(if chosen == Some(pi) {
                                                    theme::ACTIVE_VIEW
                                                } else {
                                                    Color32::from_gray(54)
                                                })
                                                .selected(chosen == Some(pi)),
                                        )
                                        .clicked()
                                    {
                                        action = Some(Action::ChoosePlate(pi));
                                    }
                                },
                            );
                        }
                    });
                    if st.expanded.contains(&di) {
                        ui.indent("body", |ui| {
                            let mut guard = filters.lock().unwrap();
                            let filter = guard.devices.entry(di).or_default();
                            device_filter::controls(
                                ui,
                                filter,
                                device_filter::sample_rate(data, di),
                            );
                            // Expanding a list uses metadata only; a visible cell demands samples.
                            drop(guard);
                            for (ci, _) in d.channels.iter().enumerate() {
                                let name = if d.plate.is_some() && ci < 6 {
                                    PLATE_LANES[ci].0
                                } else {
                                    model::display_name(names, data, di, ci)
                                };
                                if let Some(cell) = model::channel(data, di, ci) {
                                    ui.horizontal(|ui| {
                                        source(
                                            ui,
                                            format!("●  {name}"),
                                            || vec![cell],
                                            d.plate.is_none().then(|| format!("ch {ci}")),
                                        )
                                    });
                                }
                            }
                        });
                    }
                });
            }
        });
    action
}
fn empty(ui: &mut Ui, rect: Rect, text: &str) {
    dashed(ui, rect, Color32::from_gray(65));
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(11.0),
        theme::TEXT_DIM,
    );
}
fn dashed(ui: &mut Ui, rect: Rect, color: Color32) {
    let stroke = Stroke::new(1.0_f32, color);
    for [a, b] in [
        [rect.left_top(), rect.right_top()],
        [rect.right_top(), rect.right_bottom()],
        [rect.right_bottom(), rect.left_bottom()],
        [rect.left_bottom(), rect.left_top()],
    ] {
        let delta = b - a;
        let len = delta.length();
        for at in (0..len as usize).step_by(9) {
            let at = at as f32;
            ui.painter().line_segment(
                [
                    a + delta * (at / len),
                    a + delta * ((at + 4.0).min(len) / len),
                ],
                stroke,
            );
        }
    }
}
fn header(
    ui: &mut Ui,
    title: &str,
    suffix: Option<String>,
    badge: Option<&str>,
    plate: bool,
    at: usize,
    cell: &Cell,
) -> bool {
    let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("device_grid.chip"));
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let painter = ui.painter().with_clip_rect(r);
    let font = FontId::proportional(10.5);
    let title_w = painter
        .layout_no_wrap(title.to_owned(), font.clone(), theme::TEXT)
        .size()
        .x;
    let suffix_w = suffix.as_ref().map_or(0.0, |s| {
        painter
            .layout_no_wrap(s.clone(), FontId::proportional(9.0), theme::TEXT_DIM)
            .size()
            .x
            + 5.0
    });
    let name_w = (title_w + suffix_w + 33.0).min(r.width()).max(24.0);
    let name = Rect::from_min_size(r.min + vec2(2.0, 1.0), vec2(name_w - 2.0, 17.0));
    painter.rect_filled(name, 2.0, Color32::from_gray(18));
    painter.circle_filled(
        name.left_center() + vec2(6.0, 0.0),
        2.5,
        if plate {
            theme::ACCENT
        } else {
            Color32::from_rgb(192, 128, 192)
        },
    );
    let close = Rect::from_min_size(name.right_top() - vec2(18.0, 0.0), vec2(18.0, 17.0));
    let drag_rect = Rect::from_min_max(name.min, close.left_bottom());
    let drag = ui
        .interact(drag_rect, ui.id().with(("cell_chip", at)), Sense::drag())
        .on_hover_cursor(egui::CursorIcon::Grab);
    if drag.drag_started() {
        drag.dnd_set_drag_payload(Payload {
            label: title.to_owned(),
            items: vec![cell.clone()],
            from: Some(at),
        });
    }
    let clip = painter.with_clip_rect(name);
    let title_room = (drag_rect.width() - suffix_w - 13.0).max(0.0);
    let text_rect = Rect::from_min_size(name.min + vec2(13.0, 0.0), vec2(title_room, 17.0));
    if title_room > 0.0 {
        let mut job =
            egui::text::LayoutJob::simple(title.to_owned(), font, theme::TEXT, f32::INFINITY);
        job.wrap.max_width = title_room;
        job.wrap.max_rows = 1;
        let galley = ui.fonts(|fonts| fonts.layout_job(job));
        let pos = text_rect.left_center() - vec2(0.0, galley.size().y / 2.0);
        clip.galley(pos, galley, theme::TEXT);
    }
    if let Some(suffix) = suffix {
        clip.text(
            pos2(text_rect.right() + 3.0, name.center().y),
            Align2::LEFT_CENTER,
            suffix,
            FontId::proportional(9.0),
            theme::TEXT_DIM,
        );
    }
    let clear = ui
        .interact(close, ui.id().with(("clear", at)), Sense::click())
        .on_hover_text("Clear cell");
    clip.text(
        close.center(),
        Align2::CENTER_CENTER,
        "×",
        FontId::proportional(13.0),
        if clear.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        },
    );
    if let Some(text) = badge {
        let badge_rect = Rect::from_min_max(pos2(name.right() + 4.0, name.top()), r.right_bottom());
        if badge_rect.width() > 25.0 {
            let p = painter.with_clip_rect(badge_rect);
            let w = (text.chars().count() as f32 * 5.0 + 7.0).min(badge_rect.width());
            p.rect_filled(
                Rect::from_min_size(badge_rect.min, vec2(w, 17.0)),
                2.0,
                Color32::from_rgb(9, 71, 113),
            );
            p.text(
                badge_rect.left_top() + vec2(3.0, 8.5),
                Align2::LEFT_CENTER,
                text,
                FontId::proportional(9.5),
                Color32::WHITE,
            );
        }
    }
    clear.clicked()
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    data: &Arc<ViewData>,
    frame: i64,
    st: &mut State,
    file_id: usize,
    chosen: Option<usize>,
    emg_failure: Option<&str>,
    filters: &SharedFilters,
    saved: &mut Vec<Layout>,
    names: &ChannelNames,
) -> Option<Action> {
    let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("device_grid.show"));
    if !ui.is_visible() {
        return None;
    }
    if data.analog_pending {
        ui.label(RichText::new("Loading force plates…").small().weak());
        return None;
    }
    st.layout
        .get_or_insert_with(|| Layout::initial(data, saved));
    toolbar(ui, st, saved, data);
    ui.add_space(3.0);
    let area = ui.available_rect_before_wrap();
    let folded = st.folded.unwrap_or(area.width() < 620.0);
    let list_w = if folded {
        22.0
    } else {
        330.0_f32.min((area.width() - 80.0).max(100.0))
    };
    let left = Rect::from_min_max(area.min, pos2(area.left() + list_w, area.bottom()));
    let mut action = None;
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(left), |ui| {
        ui.set_clip_rect(left.intersect(ui.clip_rect()));
        if folded {
            if ui.small_button("▶").clicked() {
                st.folded = Some(false);
            }
        } else {
            action = list(ui, data, st, file_id, chosen, filters, names);
        }
    });
    let grid = Rect::from_min_max(pos2(left.right() + 5.0, area.top()), area.max);
    ui.painter().vline(
        left.right() + 2.0,
        area.y_range(),
        Stroke::new(1.0_f32, Color32::from_gray(60)),
    );
    let layout = st.layout.as_ref().unwrap();
    let (rows, cols) = (layout.rows, layout.cols);
    let cells = layout.cells.clone(); // <=16 small names, never samples.
    let size = vec2(
        ((grid.width() - 4.0 * (cols - 1) as f32) / cols as f32).max(1.0),
        ((grid.height() - 4.0 * (rows - 1) as f32) / rows as f32).max(1.0),
    );
    let mut displays: HashMap<usize, Display> = HashMap::new();
    let mut placement = None;
    let mut clear = None;
    let cell_rect = |at: usize| {
        Rect::from_min_size(
            grid.min
                + vec2(
                    (at % cols) as f32 * (size.x + 4.0),
                    (at / cols) as f32 * (size.y + 4.0),
                ),
            size,
        )
    };
    let probe = model::PROBE.get().and_then(|p| p.drag);
    let payload = egui::DragAndDrop::payload::<Payload>(ui.ctx()).or_else(|| {
        probe.map(|(di, _)| {
            Arc::new(Payload {
                label: data
                    .devices
                    .get(di)
                    .map_or(String::new(), |_| device_title(data, di)),
                items: model::items(data, di),
                from: None,
            })
        })
    });
    let target = probe.map(|(_, at)| at).or_else(|| {
        ui.input(|i| i.pointer.hover_pos())
            .and_then(|p| (0..cells.len()).find(|&at| cell_rect(at).contains(p)))
    });
    let placing = payload.is_some();
    let destinations = payload
        .as_ref()
        .zip(target)
        .map_or_else(Vec::new, |(p, at)| {
            if p.from.is_some() {
                vec![at]
            } else {
                layout.destinations(at, p.items.len())
            }
        });
    for (at, cell) in cells.iter().enumerate() {
        let rect = cell_rect(at);
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let response = ui.interact(rect, ui.id().with(("grid_cell", at)), Sense::hover());
        if placing && target == Some(at) {
            if let Some(items) = response.dnd_release_payload::<Payload>() {
                placement = Some((at, items.as_ref().clone()));
            }
        }
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.set_clip_rect(rect.intersect(ui.clip_rect()));
            let Some(cell) = cell else {
                empty(ui, rect.shrink(1.0), "Drop here");
                return;
            };
            let resolved = {
                let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("device_grid.resolve"));
                cell.resolve(data)
            };
            let Some((di, channels)) = resolved else {
                let title = match cell {
                    Cell::Channel { channel, .. } => channel.as_str(),
                    _ => cell.device(),
                };
                if header(
                    ui,
                    title,
                    None,
                    None,
                    matches!(cell, Cell::Plate { .. }),
                    at,
                    cell,
                ) {
                    clear = Some(at);
                }
                let body = Rect::from_min_max(rect.min + vec2(0.0, 20.0), rect.max);
                empty(ui, body.shrink(1.0), "Not in this file");
                return;
            };
            let d = &data.devices[di];
            let display = displays.entry(di).or_insert_with(|| {
                let mut guard = filters.lock().unwrap();
                let f = guard.devices.entry(di).or_default();
                if !(d.plate.is_none() && data.emg_pending)
                    && !ui.is_sizing_pass()
                    && !ui.ctx().will_discard()
                {
                    f.update(data, di, ui.ctx());
                }
                f.display()
            });
            let title = {
                let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("device_grid.channel_name"));
                cell_title(data, cell, di, &channels, names)
            };
            if header(
                ui,
                &title,
                d.plate.is_none().then(|| format!("ch {}", channels[0])),
                display_badge(display),
                d.plate.is_some(),
                at,
                cell,
            ) {
                clear = Some(at);
            }
            if let Some(plate) = d.plate.and_then(|pi| data.plates.get(pi)) {
                let current = frame
                    .checked_sub(data.start_frame)
                    .and_then(|i| usize::try_from(i).ok())
                    .and_then(|i| plate.fz.get(i))
                    .filter(|v| v.is_finite());
                response.clone().on_hover_text(format!(
                    "Fz: {} · max |Fz|: {:.1}",
                    current.map_or("—".into(), |v| format!("{v:.1}")),
                    plate.fz_abs_max
                ));
            }
            if d.plate.is_none() && data.emg_pending {
                if emg_failure.is_none() && !ui.is_sizing_pass() && !ui.ctx().will_discard() {
                    st.request_emg = true;
                }
                ui.label(
                    RichText::new(if emg_failure.is_some() {
                        "장치 자료를 읽지 못했어요."
                    } else {
                        "Loading EMG…"
                    })
                    .small()
                    .weak(),
                );
                return;
            }
            if ui.is_sizing_pass() || ui.ctx().will_discard() {
                return;
            }
            let key = graph_key(data, cell, display, file_id);
            if st.graphs.get(&at).is_none_or(|g| g.key != key) {
                let mut replacement = CellGraph::new(data, cell, di, &channels, display, file_id);
                if let Some(old) = st.graphs.get_mut(&at) {
                    let mut previous = std::mem::take(&mut old.drawn);
                    previous.retarget_background(std::mem::take(&mut replacement.drawn));
                    replacement.drawn = previous;
                }
                crate::views::drop_later(st.graphs.insert(at, replacement));
            }
            let graph = st.graphs.get_mut(&at).unwrap();
            if d.plate.is_none() {
                let name = model::display_name(names, data, di, channels[0]);
                if graph.lanes[0].letter != name {
                    graph.lanes[0].letter = name.to_owned();
                }
            }
            let lane_h =
                ((rect.height() - 24.0 - graph::NAV_H) / graph.lanes.len().max(1) as f32).max(1.0);
            let mut look = Look::pane();
            look.values = rect.width() > 210.0;
            if placing {
                look.keep_out = rect;
            }
            if let Some(f) = graph::show(
                ui,
                Id::new(("device_cell_graph", file_id, at)),
                &graph.lanes,
                data.start_frame,
                data.end_frame,
                frame,
                &graph.signal.gaps,
                lane_h,
                &look,
                Some(&mut graph.drawn),
                &[],
            ) {
                action = Some(Action::GotoFrame(f));
            }
        });
        if placing {
            let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("device_grid.drop_overlay"));
            if cell.is_none() {
                dashed(ui, rect.shrink(1.0), theme::ACCENT);
            }
            if destinations.contains(&at) {
                ui.painter()
                    .rect_filled(rect, 0.0, theme::ACCENT.gamma_multiply(0.15));
                ui.painter()
                    .rect_stroke(rect.shrink(1.0), 0.0, Stroke::new(1.5, theme::ACCENT));
            }
            if target == Some(at) {
                let payload = payload.as_ref().unwrap();
                let title = cell.as_ref().map(|cell| {
                    cell.resolve(data).map_or_else(
                        || cell.device().to_owned(),
                        |(di, ch)| cell_title(data, cell, di, &ch, names),
                    )
                });
                let label = if payload.from == Some(at) {
                    "".into()
                } else if payload.from.is_some() && cell.is_some() {
                    "Swap".into()
                } else if let Some(title) = title {
                    format!("Replace: {title}")
                } else {
                    "Drop to place".into()
                };
                let galley =
                    ui.painter()
                        .layout_no_wrap(label, FontId::proportional(11.0), Color32::WHITE);
                let chip = Rect::from_center_size(rect.center(), galley.size() + vec2(12.0, 8.0));
                ui.painter()
                    .rect_filled(chip, 2.0, Color32::from_rgb(9, 71, 113));
                ui.painter()
                    .galley(chip.min + vec2(6.0, 4.0), galley, Color32::WHITE);
            }
        }
    }
    if let Some(payload) = &payload {
        let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("device_grid.ghost"));
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if let Some(pointer) = probe
            .map(|(_, at)| cell_rect(at.min(cells.len() - 1)).center())
            .or_else(|| ui.input(|i| i.pointer.hover_pos()))
        {
            let text = format!("● {}", payload.label);
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                ui.id().with("drag_ghost"),
            ));
            let galley = painter.layout_no_wrap(text, FontId::proportional(11.0), theme::TEXT);
            let rect =
                Rect::from_min_size(pointer + vec2(14.0, 16.0), galley.size() + vec2(10.0, 6.0));
            painter.rect_filled(rect, 2.0, Color32::from_gray(18));
            painter.galley(rect.min + vec2(5.0, 3.0), galley, theme::TEXT);
        }
    }
    if let Some((at, payload)) = placement {
        let layout = st.layout.as_mut().unwrap();
        let before = layout.clone();
        let message = if let Some(from) = payload.from {
            let swap = layout.cells[at].is_some();
            layout.move_or_swap(from, at);
            if swap {
                "Cells swapped"
            } else {
                "Cell moved"
            }
        } else {
            let replace = layout.cells[at].is_some();
            layout.place(at, &payload.items);
            if replace {
                "Cell replaced"
            } else {
                "Cell placed"
            }
        };
        if *layout != before {
            st.note = Some(message);
            st.reconcile();
        }
    }
    if let Some(at) = clear {
        st.layout.as_mut().unwrap().cells[at] = None;
        st.note = Some("Cell cleared");
        st.reconcile();
    }
    ui.advance_cursor_after_rect(area);
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Harness {
        ctx: egui::Context,
        data: Arc<ViewData>,
        state: State,
        saved: Vec<Layout>,
        filters: SharedFilters,
        frame: usize,
        width: f32,
    }
    impl Harness {
        fn new() -> Self {
            let mut state = State::default();
            state.layout = Some(Layout::default());
            state.folded = Some(false);
            Self {
                ctx: Default::default(),
                data: Arc::new(crate::views::sample_with_plates("grid.tak", &["M"])),
                state,
                saved: vec![],
                filters: Default::default(),
                frame: 0,
                width: 960.0,
            }
        }
        fn render(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            self.frame += 1;
            self.ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        vec2(self.width, 640.0),
                    )),
                    time: Some(self.frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show(
                            ui,
                            &self.data,
                            0,
                            &mut self.state,
                            1,
                            None,
                            None,
                            &self.filters,
                            &mut self.saved,
                            &ChannelNames::default(),
                        );
                    });
                },
            )
        }
        fn text(out: &egui::FullOutput, want: &str) -> Vec<Rect> {
            out.shapes
                .iter()
                .filter_map(|s| {
                    if let egui::Shape::Text(t) = &s.shape {
                        (t.galley.text() == want)
                            .then(|| Rect::from_min_size(t.pos, t.galley.size()))
                    } else {
                        None
                    }
                })
                .collect()
        }
        fn click(&mut self, pos: egui::Pos2) -> egui::FullOutput {
            self.render(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ]);
            self.render(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }])
        }
    }

    #[test]
    fn automatic_fold_tracks_width_manual_override_and_save_keeps_grid_geometry() {
        let mut h = Harness::new();
        h.state.folded = None;
        h.width = 600.0;
        h.render(vec![]);
        let out = h.render(vec![]);
        assert!(Harness::text(&out, "Devices").is_empty());
        assert_eq!(h.state.folded, None);
        h.width = 960.0;
        let out = h.render(vec![]);
        assert_eq!(Harness::text(&out, "Devices").len(), 1);
        h.click(Harness::text(&out, "◀")[0].center());
        h.width = 800.0;
        let out = h.render(vec![]);
        assert!(Harness::text(&out, "Devices").is_empty());
        assert_eq!(h.state.folded, Some(true));
        let before = Harness::text(&out, "Drop here");
        h.state.save_name = Some(String::new());
        h.state.focus_name = true;
        h.render(vec![]);
        let out = h.render(vec![]);
        assert_eq!(
            Harness::text(&out, "Drop here"),
            before,
            "popover never moves grid"
        );
        h.state.save_name = None;
        h.width = 389.0;
        h.render(vec![]);
        let out = h.render(vec![]);
        assert_eq!(Harness::text(&out, "···").len(), 1);
        assert!(Harness::text(&out, "+ Save layout").is_empty());
        assert!(
            Harness::text(&out, "1×1")[0].center().y - Harness::text(&out, "4×4")[0].center().y
                < 1.0
        );
    }
    #[test]
    fn unchanged_cell_keeps_prepared_graph_across_resize_place_and_preset_load() {
        let mut h = Harness::new();
        h.state
            .layout
            .as_mut()
            .unwrap()
            .place(0, &model::items(&h.data, 0));
        h.render(vec![]);
        h.render(vec![]);
        h.state.graphs.get_mut(&0).unwrap().drawn.push_more(vec![]);
        let sentinel = h.state.graphs[&0].drawn.more_len();
        let key = h.state.graphs[&0].key.clone();
        h.state.layout.as_mut().unwrap().resize(3);
        h.state.reconcile();
        assert_eq!(h.state.graphs[&0].key, key);
        assert_eq!(h.state.graphs[&0].drawn.more_len(), sentinel);
        h.state
            .layout
            .as_mut()
            .unwrap()
            .place(1, &model::items(&h.data, 1));
        h.state.reconcile();
        assert_eq!(h.state.graphs[&0].drawn.more_len(), sentinel);
        let next = h.state.arrangement();
        h.state.restore(&next);
        assert_eq!(h.state.graphs[&0].drawn.more_len(), sentinel);
        h.state
            .layout
            .as_mut()
            .unwrap()
            .place(0, &model::items(&h.data, 1));
        h.state.reconcile();
        assert!(!h.state.graphs.contains_key(&0));
    }
    #[test]
    fn real_chip_drag_swaps_then_moves_and_click_never_places() {
        let mut h = Harness::new();
        h.state
            .layout
            .as_mut()
            .unwrap()
            .place(0, &model::items(&h.data, 0));
        h.state
            .layout
            .as_mut()
            .unwrap()
            .place(1, &model::items(&h.data, 1));
        h.render(vec![]);
        let out = h.render(vec![]);
        let from = Harness::text(&out, "P1")
            .into_iter()
            .max_by(|a, b| a.left().total_cmp(&b.left()))
            .unwrap()
            .center();
        let to = Harness::text(&out, "P2")
            .into_iter()
            .max_by(|a, b| a.left().total_cmp(&b.left()))
            .unwrap()
            .center()
            + vec2(0.0, 50.0);
        let drag = |h: &mut Harness, from: egui::Pos2, to: egui::Pos2| {
            h.render(vec![
                egui::Event::PointerMoved(from),
                egui::Event::PointerButton {
                    pos: from,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ]);
            h.render(vec![egui::Event::PointerMoved(from + vec2(15.0, 0.0))]);
            let preview = h.render(vec![egui::Event::PointerMoved(to)]);
            h.render(vec![egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }]);
            preview
        };
        let before = h.state.layout.clone().unwrap();
        let preview = drag(&mut h, from, to);
        assert!(!Harness::text(&preview, "Swap").is_empty());
        assert_eq!(h.state.layout.as_ref().unwrap().cells[0], before.cells[1]);
        assert_eq!(h.state.layout.as_ref().unwrap().cells[1], before.cells[0]);
        let out = h.render(vec![]);
        let from = Harness::text(&out, "P1")
            .into_iter()
            .max_by(|a, b| a.left().total_cmp(&b.left()))
            .unwrap()
            .center();
        let to = Harness::text(&out, "Drop here")[0].center();
        drag(&mut h, from, to);
        assert!(h.state.layout.as_ref().unwrap().cells[1].is_none());
        assert_eq!(h.state.layout.as_ref().unwrap().cells[2], before.cells[0]);
    }
    #[test]
    fn actual_device_drag_replaces_a_grid_cell_and_hover_close_clears_it() {
        let mut h = Harness::new();
        h.render(vec![]);
        let out = h.render(vec![]);
        let from = Harness::text(&out, "P1")[0].center();
        let to = Harness::text(&out, "Drop here")[0].center();
        h.render(vec![
            egui::Event::PointerMoved(from),
            egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ]);
        h.render(vec![egui::Event::PointerMoved(from + vec2(20.0, 0.0))]);
        h.render(vec![egui::Event::PointerMoved(to)]);
        h.render(vec![egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }]);
        assert!(matches!(
            h.state.layout.as_ref().unwrap().cells[0],
            Some(Cell::Plate { .. })
        ));
        let out = h.render(vec![egui::Event::PointerMoved(to)]);
        let close = Harness::text(&out, "×");
        assert_eq!(close.len(), 1);
        h.click(close[0].center());
        assert!(h.state.layout.as_ref().unwrap().cells[0].is_none());
    }

    #[test]
    fn saved_layout_inline_enter_esc_load_and_delete_follow_real_controls() {
        let mut h = Harness::new();
        h.render(vec![]);
        let out = h.render(vec![]);
        h.click(Harness::text(&out, "+ Save layout")[0].center());
        h.render(vec![egui::Event::Text("Gait".into())]);
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        h.render(vec![key(egui::Key::Enter)]);
        assert_eq!(h.saved.len(), 1);
        assert_eq!(h.saved[0].name, "Gait");
        let encoded = ron::to_string(&h.saved).unwrap();
        assert_eq!(ron::from_str::<Vec<Layout>>(&encoded).unwrap(), h.saved);
        let out = h.render(vec![]);
        h.click(Harness::text(&out, "+ Save layout")[0].center());
        h.render(vec![
            egui::Event::Text("Cancel".into()),
            key(egui::Key::Escape),
        ]);
        assert_eq!(h.saved.len(), 1);
        h.state.layout.as_mut().unwrap().resize(4);
        let out = h.render(vec![]);
        h.click(Harness::text(&out, "Saved layouts")[0].center());
        let out = h.render(vec![]);
        h.click(Harness::text(&out, "Gait")[0].center());
        assert_eq!(h.state.layout.as_ref().unwrap().rows, 2);
        let out = h.render(vec![]);
        h.click(Harness::text(&out, "Saved layouts")[0].center());
        let out = h.render(vec![]);
        h.click(Harness::text(&out, "×")[0].center());
        assert!(h.saved.is_empty());
    }
    #[test]
    fn every_cell_from_one_device_uses_its_completed_badge_and_bypass_hides_it() {
        let out = Arc::new(device_filter::Output {
            channels: vec![],
            bands: vec![],
            badge: "BP 20–450 · Rect · LP 6".into(),
            skipped_runs: 0,
        });
        let mut filter = device_filter::Device::default();
        filter.ready = Some(out.clone());
        filter.settings.enabled = true;
        filter.settings.steps = vec![tak_core::signal::Step::Rectify];
        for _ in 0..16 {
            assert_eq!(display_badge(&filter.display()), Some(out.badge.as_str()));
        }
        filter.settings.enabled = false;
        assert_eq!(display_badge(&filter.display()), None);
        filter.settings.enabled = true;
        filter.settings.steps.clear();
        assert_eq!(display_badge(&filter.display()), None);
    }
}
