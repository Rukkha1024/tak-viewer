//! Devices pane: a metadata-only list and at most sixteen cached, demand-loaded graphs.
use super::{
    graph::{self, Lane, Look},
    graph_float::PLATE_LANES,
    panels::Action,
    theme,
};
use crate::{
    device_filter::{self, Display, SharedFilters},
    device_grid::{self as model, Cell, Layout},
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
    picked: Option<Vec<Cell>>,
    save_name: Option<String>,
    focus_name: bool,
    graphs: HashMap<usize, CellGraph>,
}
impl State {
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
        .unwrap_or_else(|| format!("Analog device {} ({} ch)", di + 1, d.channels.len()))
}
fn cell_title(data: &ViewData, cell: &Cell, di: usize, channels: &[usize]) -> String {
    let d = &data.devices[di];
    match cell {
        Cell::Plate { .. } => {
            if channels.len() == 1 {
                format!(
                    "{} · {}",
                    device_title(data, di),
                    PLATE_LANES[channels[0]].0
                )
            } else {
                device_title(data, di)
            }
        }
        Cell::Channel { channel, .. } => {
            if d.plate.is_some() {
                format!("{} · {channel}", device_title(data, di))
            } else {
                channel.clone()
            }
        }
    }
}
pub fn display_badge(display: &Display) -> Option<&str> {
    display
        .output
        .as_ref()
        .map(|o| o.badge.as_str())
        .filter(|s| !s.is_empty())
}

fn source(ui: &mut Ui, text: String, items: Vec<Cell>, picked: &mut Option<Vec<Cell>>) {
    let response = ui.add(
        egui::Label::new(RichText::new(text).size(11.0))
            .truncate()
            .sense(Sense::click_and_drag()),
    );
    response.dnd_set_drag_payload(items.clone());
    if response.clicked() {
        *picked = Some(items);
    }
}
fn toolbar(ui: &mut Ui, st: &mut State, saved: &mut Vec<Layout>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
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
            if ui.add(button).clicked() {
                st.layout.as_mut().unwrap().resize(n);
                st.picked = None;
                crate::views::drop_later(Some(std::mem::take(&mut st.graphs)));
            }
        }
        ui.menu_button(RichText::new("Saved layouts").size(11.0), |ui| {
            if saved.is_empty() {
                ui.label(RichText::new("No saved layouts").weak());
            }
            let mut remove = None;
            for (i, l) in saved.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.button(&l.name).clicked() {
                        let mut next = l.clone();
                        next.normalize();
                        st.layout = Some(next);
                        st.picked = None;
                        crate::views::drop_later(Some(std::mem::take(&mut st.graphs)));
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
        if ui
            .small_button(RichText::new("+ Save layout").size(11.0))
            .clicked()
        {
            st.save_name = Some(String::new());
            st.focus_name = true;
        }
    });
    if let Some(name) = &mut st.save_name {
        let mut commit = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(name)
                    .hint_text("Layout name")
                    .desired_width(150.0)
                    .char_limit(64),
            );
            if st.focus_name {
                response.request_focus();
                st.focus_name = false;
            }
            commit = ui.input(|i| i.key_pressed(egui::Key::Enter)) && !name.trim().is_empty();
            cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
            ui.label(
                RichText::new("Enter to save · Esc to cancel")
                    .small()
                    .weak(),
            );
        });
        if commit {
            let mut layout = st.layout.as_ref().unwrap().clone();
            layout.name = name.trim().to_owned();
            if let Some(i) = saved.iter().position(|l| l.name == layout.name) {
                saved.remove(i);
            }
            saved.push(layout);
            st.save_name = None;
        } else if cancel {
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
) -> Option<Action> {
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
                        source(
                            ui,
                            device_title(data, di),
                            model::items(data, di),
                            &mut st.picked,
                        );
                        let display = filters
                            .lock()
                            .unwrap()
                            .devices
                            .get(&di)
                            .map(|f| f.display())
                            .unwrap_or_default();
                        if let Some(badge) = display_badge(&display) {
                            device_filter::badge(ui, badge);
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
                            for (ci, c) in d.channels.iter().enumerate() {
                                let name = if d.plate.is_some() && ci < 6 {
                                    PLATE_LANES[ci].0
                                } else {
                                    c.name.as_str()
                                };
                                if let Some(cell) = model::channel(data, di, ci) {
                                    source(ui, format!("●  {name}"), vec![cell], &mut st.picked);
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
    let stroke = Stroke::new(1.0_f32, Color32::from_gray(65));
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
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(11.0),
        theme::TEXT_DIM,
    );
}
fn header(ui: &mut Ui, title: &str, badge: Option<&str>, plate: bool) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let painter = ui.painter().with_clip_rect(r.shrink2(vec2(0.0, 1.0)));
    let badge_w = badge.map_or(0.0, |s| {
        (s.chars().count() as f32 * 5.0 + 8.0).min(r.width() * 0.68)
    });
    let title_width = painter
        .layout_no_wrap(title.to_owned(), FontId::proportional(10.5), theme::TEXT)
        .size()
        .x
        + 18.0;
    let name_w = (r.width() - badge_w - 5.0)
        .max(24.0)
        .min(title_width.max(24.0));
    let name = Rect::from_min_size(r.min + vec2(2.0, 1.0), vec2(name_w, 17.0));
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
    painter.with_clip_rect(name.shrink2(vec2(1.0, 0.0))).text(
        name.left_center() + vec2(13.0, 0.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(10.5),
        theme::TEXT,
    );
    if let Some(text) = badge {
        let rect = Rect::from_min_size(pos2(name.right() + 4.0, name.top()), vec2(badge_w, 17.0));
        painter.rect_filled(rect, 2.0, Color32::from_rgb(9, 71, 113));
        painter.with_clip_rect(rect).text(
            rect.left_center() + vec2(3.0, 0.0),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(9.5),
            Color32::WHITE,
        );
    }
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
) -> Option<Action> {
    if !ui.is_visible() {
        return None;
    }
    if data.analog_pending {
        ui.label(RichText::new("Loading force plates…").small().weak());
        return None;
    }
    st.layout
        .get_or_insert_with(|| Layout::initial(data, saved));
    toolbar(ui, st, saved);
    ui.add_space(3.0);
    let area = ui.available_rect_before_wrap();
    let folded = *st.folded.get_or_insert(area.width() < 620.0);
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
            action = list(ui, data, st, file_id, chosen, filters);
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
    for (at, cell) in cells.iter().enumerate() {
        let rect = Rect::from_min_size(
            grid.min
                + vec2(
                    (at % cols) as f32 * (size.x + 4.0),
                    (at / cols) as f32 * (size.y + 4.0),
                ),
            size,
        );
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let payload = egui::DragAndDrop::payload::<Vec<Cell>>(ui.ctx());
        let placing = payload.is_some() || st.picked.is_some();
        let response = ui.interact(
            rect,
            ui.id().with(("grid_cell", at)),
            if placing {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        if response.contains_pointer() && placing {
            ui.painter()
                .rect_stroke(rect, 0.0, Stroke::new(1.0_f32, theme::ACCENT));
            if let Some(items) = response.dnd_release_payload::<Vec<Cell>>() {
                placement = Some((at, items.as_ref().clone()));
            } else if response.clicked() {
                if let Some(items) = st.picked.take() {
                    placement = Some((at, items));
                }
            }
        }
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.set_clip_rect(rect.intersect(ui.clip_rect()));
            let Some(cell) = cell else {
                empty(ui, rect.shrink(1.0), "Drop here");
                return;
            };
            let Some((di, channels)) = cell.resolve(data) else {
                empty(ui, rect.shrink(1.0), "Not in this file");
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
            header(
                ui,
                &cell_title(data, cell, di, &channels),
                display_badge(display),
                d.plate.is_some(),
            );
            if d.plate.is_none() && data.emg_pending {
                if emg_failure.is_none() && !ui.is_sizing_pass() && !ui.ctx().will_discard() {
                    st.request_emg = true;
                }
                ui.label(
                    RichText::new(if emg_failure.is_some() {
                        "Analog data unavailable"
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
        if cell.is_some() && response.contains_pointer() && !placing {
            let x = Rect::from_min_size(rect.right_top() - vec2(18.0, 0.0), vec2(18.0, 18.0));
            if ui.put(x, egui::Button::new("×").small()).clicked() {
                clear = Some(at);
            }
        }
    }
    if let Some((at, items)) = placement {
        st.layout.as_mut().unwrap().place(at, &items);
        st.picked = None;
        crate::views::drop_later(Some(std::mem::take(&mut st.graphs)));
    }
    if let Some(at) = clear {
        st.layout.as_mut().unwrap().cells[at] = None;
        crate::views::drop_later(st.graphs.remove(&at));
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        st.picked = None;
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
            }
        }
        fn render(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            self.frame += 1;
            self.ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(960.0, 640.0))),
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
