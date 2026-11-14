//! Panes (Labels, selected marker, its X/Y/Z graph, devices) and the playback bar of a 3D view,
//! styled after Motive 3 (Labels pane, Graph View pane, Edit-mode control deck).

use super::graph::{self, Lane, Look};
use super::graph_float::{LaneSet, PLATE_LANES};
use super::theme;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Id, Rect, RichText, ScrollArea, Sense, Stroke, Ui};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use tak_core::comments::Comments;
use tak_core::playback::Playback;
use tak_core::tak::{uid_hex, MarkerKind};
use tak_core::view_data::{DeviceView, GroupKind, ViewData, FZ_CHANNEL, VALID_SUBSAMPLES};

pub const ROW_H: f32 = 20.0;

/// User action produced by a pane.
pub enum Action {
    Select { marker: usize, additive: bool },
    GotoFrame(i64),
    /// GRAPH pane button "3D 위로": the graph floats over the 3D views again.
    GraphOver3d,
    /// Devices pane, a plate's "3D 위로": choose that plate for the active view (its graph
    /// floats over the 3D), or un-choose it when it is the chosen one.
    ChoosePlate(usize),
}

pub fn kind_label(kind: MarkerKind) -> &'static str {
    match kind {
        MarkerKind::Template => "marker set 마커",
        MarkerKind::SkeletonExtra => "추가 마커 (Skeleton)",
        MarkerKind::OtherParent => "추가 마커 (LabelGroup·RigidBody)",
        MarkerKind::Unlabeled => "Unlabeled",
        MarkerKind::Unknown => "구조 미확인",
    }
}

fn gaps_text(fill_pct: f32) -> String {
    let g = 100.0 - fill_pct;
    if g <= 0.0 {
        String::new()
    } else if g < 0.5 {
        "<1%".to_owned()
    } else {
        format!("{:.0}%", g)
    }
}

// ───────────────────────────── Labels ─────────────────────────────

#[derive(Default)]
pub struct LabelsState {
    pub filter: String,
    pub collapsed: HashSet<usize>,
    pub scroll_to: Option<usize>,
    initialized_for: Option<usize>,
}

/// Use one column below 260 pt of list width.
const LABELS_TWO_COL_MIN: f32 = 260.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Header(usize),
    Markers(usize, Option<usize>),
}

impl LabelsState {
    /// New file: Unlabeled collapsed by default (Motive default layout shows labeled only).
    pub fn reset(&mut self, data: &ViewData, file_id: usize) {
        self.collapsed.clear();
        self.filter.clear();
        self.scroll_to = None;
        for (gi, g) in data.groups.iter().enumerate() {
            if g.kind == GroupKind::Unlabeled {
                self.collapsed.insert(gi);
            }
        }
        self.initialized_for = Some(file_id);
    }
}

fn label_columns(width: f32) -> usize {
    if width < LABELS_TWO_COL_MIN { 1 } else { 2 }
}

/// Per group: down the left column, then down the right; searching divides only the matches.
fn rows(data: &ViewData, st: &LabelsState, columns: usize) -> Vec<Row> {
    let needle = st.filter.trim().to_lowercase();
    let mut out = Vec::new();
    for (gi, g) in data.groups.iter().enumerate() {
        let members: Vec<usize> = g
            .members
            .iter()
            .copied()
            .filter(|&m| needle.is_empty() || data.markers[m].name.to_lowercase().contains(&needle))
            .collect();
        if !needle.is_empty() && members.is_empty() {
            continue;
        }
        out.push(Row::Header(gi));
        if needle.is_empty() && st.collapsed.contains(&gi) {
            continue;
        }
        let height = members.len().div_ceil(columns);
        for r in 0..height {
            out.push(Row::Markers(members[r], if columns == 2 { members.get(r + height).copied() } else { None }));
        }
    }
    out
}

struct Cols {
    name: f32,
    gaps_r: f32,
}

fn cols(rect: Rect) -> Cols {
    Cols { name: rect.left() + 24.0, gaps_r: rect.right() - 8.0 }
}

fn label_cell(rect: Rect, column: usize, columns: usize) -> Rect {
    let width = rect.width() / columns as f32;
    Rect::from_min_size(pos2(rect.left() + column as f32 * width, rect.top()), vec2(width, rect.height()))
}

fn label_scroll_offset(rows: &[Row], target: usize, pitch: f32) -> Option<f32> {
    rows.iter().position(|r| matches!(*r, Row::Markers(left, right) if left == target || right == Some(target)))
        .map(|r| ((r as f32 - 3.0) * pitch).max(0.0))
}

fn paint_label(ui: &Ui, rect: Rect, m: &tak_core::view_data::MarkerView, frame: i64, bg: Color32) {
    let p = ui.painter();
    let c = cols(rect);
    p.rect_filled(rect, 0.0, bg);
    p.circle_filled(pos2(rect.left() + 13.0, rect.center().y), 4.0, theme::color(m.color));
    let name_color = if m.present(frame) { theme::LABEL_TRACKED } else { theme::LABEL_UNTRACKED };
    let gaps = p.layout_no_wrap(gaps_text(m.fill_pct), FontId::proportional(12.5), theme::TEXT);
    // Reserve the number first, including its real font width; elide the name, never Gaps.
    let name_width = (c.gaps_r - 44.0_f32.max(gaps.size().x + 6.0) - c.name).max(0.0);
    let mut job = egui::text::LayoutJob::simple_singleline(m.name.clone(), FontId::proportional(13.0), name_color);
    job.wrap.max_width = name_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let name = p.layout_job(job);
    let clip = Rect::from_min_size(pos2(c.name, rect.top()), vec2(name_width, rect.height())).intersect(rect);
    p.with_clip_rect(clip).galley(pos2(c.name, rect.center().y - name.size().y * 0.5), name, name_color);
    p.galley(pos2(c.gaps_r - gaps.size().x, rect.center().y - gaps.size().y * 0.5), gaps, theme::TEXT);
}

/// LABELS shows gap percentages in two columns, reading down then across. Split each group
/// at ceil(n / 2); use one column below 260 pt. Group headers remain full width, names are
/// elided to fit, and only visible rows are painted.
#[allow(clippy::too_many_arguments)]
pub fn labels_pane(
    ui: &mut Ui,
    data: &ViewData,
    frame: i64,
    st: &mut LabelsState,
    selected: &BTreeSet<usize>,
    primary: Option<usize>,
    file_id: usize,
    max_height: f32,
) -> Option<Action> {
    if st.initialized_for != Some(file_id) {
        st.reset(data, file_id);
    }
    let mut action = None;
    let labeled_total = data.markers.iter().filter(|m| m.kind != MarkerKind::Unlabeled).count();
    let labeled_now = data.markers.iter().filter(|m| m.kind != MarkerKind::Unlabeled && m.present(frame)).count();
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.label(RichText::new("Labeled").strong().size(14.0));
        ui.label(RichText::new(format!("{labeled_now} / {labeled_total}")).color(theme::TEXT_DIM).size(14.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let width = 130.0_f32.min(ui.available_width()).max(0.0);
            ui.add(egui::TextEdit::singleline(&mut st.filter).hint_text("검색").desired_width(width));
        });
    });
    let columns = label_columns(ui.available_width());
    // Allocate the header now; paint it against the actual list width after the scroll area.
    let (hrect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());

    // Measure the wrapped legend before filling the remaining height with the list.
    let mut legend = egui::text::LayoutJob::default();
    let font = egui::TextStyle::Small.resolve(ui.style());
    for (text, color) in [
        ("흰 이름", theme::LABEL_TRACKED),
        (" = 현재 프레임에 있음 · ", theme::TEXT_DIM),
        ("자홍 이름", theme::LABEL_UNTRACKED),
        (" = 없음 · Gaps = 비어 있는 프레임 비율(100% 채움이면 빈칸) · 빈구간 수 · 최대 길이는 마커를 고르면 '선택 마커' 패널에 나와요.", theme::TEXT_DIM),
    ] {
        legend.append(text, 0.0, egui::TextFormat::simple(font.clone(), color));
    }
    legend.wrap.max_width = ui.available_width();
    let legend = ui.painter().layout_job(legend);
    let list_height = (ui.available_height() - legend.size().y - ui.spacing().item_spacing.y).max(0.0).min(max_height);
    let rows = rows(data, st, columns);
    let mut scroll = ScrollArea::vertical().id_salt("labels_rows").max_height(list_height).auto_shrink([false, false]);
    if let Some(target) = st.scroll_to.take() {
        if let Some(offset) = label_scroll_offset(&rows, target, ROW_H + ui.spacing().item_spacing.y) {
            scroll = scroll.vertical_scroll_offset(offset);
        }
    }
    let mut toggle_group = None;
    let list = scroll.show_rows(ui, ROW_H, rows.len(), |ui, range| {
        let start = range.start;
        for (k, row) in rows[range].iter().enumerate() {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
            let p = ui.painter();
            let c = cols(rect);
            match *row {
                Row::Header(gi) => {
                    let resp = ui.interact(rect, ui.make_persistent_id(("labels_group", file_id, gi)), Sense::click());
                    let g = &data.groups[gi];
                    p.rect_filled(rect, 0.0, theme::HEADER_BG);
                    let collapsed = st.collapsed.contains(&gi) && st.filter.trim().is_empty();
                    let present = g.members.iter().filter(|&&m| data.markers[m].present(frame)).count();
                    let c0 = pos2(rect.left() + 12.0, rect.center().y);
                    let tri = if collapsed {
                        vec![c0 + vec2(-3.0, -4.5), c0 + vec2(4.0, 0.0), c0 + vec2(-3.0, 4.5)]
                    } else {
                        vec![c0 + vec2(-4.5, -3.0), c0 + vec2(4.5, -3.0), c0 + vec2(0.0, 4.0)]
                    };
                    p.add(egui::Shape::convex_polygon(tri, theme::TEXT_DIM, Stroke::NONE));
                    let title = p.text(pos2(c.name, rect.center().y), Align2::LEFT_CENTER, &g.title, FontId::proportional(13.0), theme::TEXT);
                    p.text(
                        pos2(title.right() + 8.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        format!("{present} / {}", g.members.len()),
                        FontId::proportional(12.0),
                        theme::TEXT_DIM,
                    );
                    if resp.clicked() {
                        toggle_group = Some(gi);
                    }
                }
                Row::Markers(left, right) => {
                    for (column, mi) in [Some(left), right].into_iter().enumerate() {
                        let Some(mi) = mi else { continue };
                        let cell = label_cell(rect, column, columns);
                        let resp = ui.interact(cell, ui.make_persistent_id(("labels_marker", file_id, mi)), Sense::click());
                        let bg = if Some(mi) == primary {
                            theme::SELECT_PRIMARY.gamma_multiply(0.28)
                        } else if selected.contains(&mi) {
                            theme::SELECT_SECONDARY.gamma_multiply(0.22)
                        } else if resp.hovered() {
                            Color32::from_rgb(0x3a, 0x3a, 0x3a)
                        } else if (start + k) % 2 == 1 {
                            theme::ROW_ALT
                        } else {
                            theme::PANEL_BG
                        };
                        paint_label(ui, cell, &data.markers[mi], frame, bg);
                        if resp.clicked() {
                            let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
                            action = Some(Action::Select { marker: mi, additive });
                        }
                    }
                    if columns == 2 {
                        p.line_segment([pos2(rect.center().x, rect.top()), pos2(rect.center().x, rect.bottom())], Stroke::new(1.0_f32, theme::GRAPH_SEP));
                    }
                }
            }
        }
    });
    let hrect = Rect::from_min_max(hrect.min, pos2(list.inner_rect.right(), hrect.bottom()));
    let p = ui.painter();
    for column in 0..columns {
        let cell = label_cell(hrect, column, columns);
        let c = cols(cell);
        let f = FontId::proportional(12.5);
        p.text(pos2(c.name, cell.center().y), Align2::LEFT_CENTER, "Labels", f.clone(), theme::ACCENT);
        p.text(pos2(c.gaps_r, cell.center().y), Align2::RIGHT_CENTER, "Gaps", f, theme::TEXT_DIM);
    }
    p.line_segment([hrect.left_bottom(), hrect.right_bottom()], Stroke::new(1.0_f32, theme::GRAPH_SEP));
    if columns == 2 {
        p.line_segment([pos2(hrect.center().x, hrect.top()), pos2(hrect.center().x, hrect.bottom())], Stroke::new(1.0_f32, theme::GRAPH_SEP));
    }
    if let Some(gi) = toggle_group {
        if !st.collapsed.remove(&gi) {
            st.collapsed.insert(gi);
        }
    }
    let (rect, _) = ui.allocate_exact_size(legend.size(), Sense::hover());
    ui.painter().galley(rect.min, legend, theme::TEXT_DIM);
    action
}

#[cfg(test)]
mod labels_tests {
    use super::*;
    use tak_core::view_data::{ColorSource, Group};

    fn data(groups: &[&[&str]]) -> ViewData {
        let names: Vec<&str> = groups.iter().flat_map(|g| g.iter().copied()).collect();
        let mut take = crate::views::sample_take("labels.tak", &names);
        for t in &mut take.trajectories {
            t.kind = MarkerKind::Template;
        }
        let mut d = ViewData::build(take);
        let mut first = 0;
        d.groups = groups.iter().enumerate().map(|(gi, names)| {
            let members: Vec<usize> = (first..first + names.len()).collect();
            first += names.len();
            for &mi in &members {
                d.markers[mi].group = gi;
            }
            Group { title: format!("Group{gi}"), kind: GroupKind::AssetTemplate, color: ColorSource::Asset(gi), members, asset: None }
        }).collect();
        d
    }

    #[test]
    fn layout_reads_down_first_per_group_in_existing_order_with_odd_single_and_empty_groups() {
        let mut d = data(&[&["M0", "M1", "M2", "M3", "M4"], &["M5"], &["M6", "M7", "M8", "M9"], &[]]);
        // Existing group order, rather than numeric marker order, is the reading order.
        d.groups[0].members = vec![2, 0, 4, 1, 3];
        let st = LabelsState::default();
        assert_eq!(rows(&d, &st, 2), vec![
            Row::Header(0), Row::Markers(2, Some(1)), Row::Markers(0, Some(3)), Row::Markers(4, None),
            Row::Header(1), Row::Markers(5, None),
            Row::Header(2), Row::Markers(6, Some(8)), Row::Markers(7, Some(9)), Row::Header(3),
        ]);
        let single = rows(&d, &st, 1);
        let markers: Vec<usize> = single.iter().filter_map(|r| match *r {
            Row::Markers(left, None) => Some(left),
            Row::Markers(_, Some(_)) => panic!("one column has no right marker"),
            Row::Header(_) => None,
        }).collect();
        assert_eq!(markers, [2, 0, 4, 1, 3, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn collapsed_groups_and_search_keep_the_existing_rules_and_split_only_matches() {
        let mut d = data(&[&["keep A", "skip", "Keep B", "keep C"], &["other"], &["keep D"]]);
        d.groups[2].kind = GroupKind::Unlabeled;
        let mut st = LabelsState::default();
        st.reset(&d, 7);
        assert!(st.collapsed.contains(&2), "Unlabeled still starts collapsed");
        st.collapsed.insert(0);
        assert_eq!(rows(&d, &st, 2), [Row::Header(0), Row::Header(1), Row::Markers(4, None), Row::Header(2)]);
        st.filter = "  KEEP  ".into();
        assert_eq!(rows(&d, &st, 2), [
            Row::Header(0), Row::Markers(0, Some(3)), Row::Markers(2, None),
            Row::Header(2), Row::Markers(5, None),
        ], "search opens matching collapsed groups and hides groups without matches");
        st.filter = "no match".into();
        assert!(rows(&d, &st, 2).is_empty());
    }

    #[test]
    fn column_threshold_and_scroll_offsets_use_both_cells_and_the_real_pitch() {
        assert_eq!(label_columns(259.9), 1);
        assert_eq!(label_columns(260.0), 2);
        assert_eq!(label_columns(380.0), 2);
        let list = [Row::Header(0), Row::Markers(0, Some(4)), Row::Markers(1, Some(5)), Row::Markers(2, Some(6)), Row::Markers(3, None)];
        assert_eq!(label_scroll_offset(&list, 4, 29.0), Some(0.0));
        assert_eq!(label_scroll_offset(&list, 6, 29.0), Some(0.0));
        assert_eq!(label_scroll_offset(&list, 3, 29.0), Some(29.0));
        assert_eq!(label_scroll_offset(&list, 99, 29.0), None);
        assert_eq!(label_scroll_offset(&[Row::Header(0)], 0, 29.0), None);
    }

    struct PaneTest {
        ctx: egui::Context,
        data: ViewData,
        state: LabelsState,
        selected: BTreeSet<usize>,
        primary: Option<usize>,
        frame: i64,
        width: f32,
        height: f32,
        spacing: f32,
    }

    impl PaneTest {
        fn new(groups: &[&[&str]]) -> Self {
            let data = data(groups);
            let mut state = LabelsState::default();
            state.reset(&data, 7);
            Self { ctx: egui::Context::default(), data, state, selected: BTreeSet::new(), primary: None, frame: 0, width: 380.0, height: 220.0, spacing: 3.0 }
        }

        fn draw(&mut self, modifiers: egui::Modifiers, events: Vec<egui::Event>) -> (egui::FullOutput, Option<Action>) {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(self.width + 16.0, 700.0))),
                focused: true, modifiers, events, ..Default::default()
            };
            let mut action = None;
            let ctx = self.ctx.clone();
            let out = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_width(self.width);
                    ui.spacing_mut().item_spacing.y = self.spacing;
                    action = labels_pane(ui, &self.data, self.frame, &mut self.state, &self.selected, self.primary, 7, self.height);
                });
            });
            (out, action)
        }

        fn idle(&mut self) -> egui::FullOutput {
            self.draw(egui::Modifiers::NONE, vec![]).0
        }

        fn click(&mut self, at: egui::Pos2, modifiers: egui::Modifiers) -> Option<Action> {
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers };
            self.draw(modifiers, vec![egui::Event::PointerMoved(at)]);
            self.draw(modifiers, vec![button(true)]);
            self.draw(modifiers, vec![button(false)]).1
        }
    }

    struct Painted {
        text: String,
        glyphs: String,
        rect: Rect,
        clip: Rect,
        color: Color32,
        elided: bool,
    }

    fn texts(out: &egui::FullOutput) -> Vec<Painted> {
        fn walk(shape: &egui::Shape, clip: Rect, found: &mut Vec<Painted>) {
            match shape {
                egui::Shape::Text(t) => found.push(Painted {
                    text: t.galley.text().to_owned(),
                    glyphs: t.galley.rows.iter().flat_map(|r| r.glyphs.iter().map(|g| g.chr)).collect(),
                    rect: Rect::from_min_size(t.pos, t.galley.size()), clip,
                    color: t.galley.job.sections.first().map_or(t.fallback_color, |s| s.format.color),
                    elided: t.galley.elided,
                }),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, clip, found)),
                _ => {}
            }
        }
        let mut found = vec![];
        for s in &out.shapes {
            walk(&s.shape, s.clip_rect, &mut found);
        }
        found
    }

    fn at<'a>(texts: &'a [Painted], name: &str) -> &'a Painted {
        texts.iter().find(|t| t.text == name).unwrap_or_else(|| panic!("{name} was not painted"))
    }

    #[test]
    fn at_380pt_each_row_has_the_column_major_names_and_right_aligned_gaps_only() {
        let mut p = PaneTest::new(&[&["M0", "M1", "M2", "M3", "M4"]]);
        for (i, m) in p.data.markers.iter_mut().enumerate() {
            m.fill_pct = if i == 2 { 100.0 } else { 90.0 - i as f32 * 10.0 };
            m.gaps = vec![(2, 3), (5, 6)];
            m.max_gap = 123;
        }
        let painted = texts(&p.idle());
        assert_eq!(painted.iter().filter(|t| t.text == "Labels").count(), 2);
        let mut headers: Vec<_> = painted.iter().filter(|t| t.text == "Gaps").collect();
        headers.sort_by(|a, b| a.rect.left().total_cmp(&b.rect.left()));
        assert_eq!(headers.len(), 2);
        for (left, right) in [("M0", "M3"), ("M1", "M4")] {
            assert!((at(&painted, left).rect.center().y - at(&painted, right).rect.center().y).abs() < 0.1);
            assert!((at(&painted, right).rect.left() - at(&painted, left).rect.left() - 190.0).abs() < 0.5);
        }
        assert!(at(&painted, "M0").rect.top() < at(&painted, "M1").rect.top());
        assert!(at(&painted, "M1").rect.top() < at(&painted, "M2").rect.top());
        for (value, col) in [("10%", 0), ("20%", 0), ("40%", 1), ("50%", 1)] {
            assert!((at(&painted, value).rect.right() - headers[col].rect.right()).abs() < 0.5, "{value} right-aligned to its header");
        }
        assert!(!painted.iter().any(|t| ["빈구간", "최대", "123", "2", "0%"].contains(&t.text.as_str())), "old header/cell numbers are gone");
        assert!(painted.iter().any(|t| t.text.contains("빈구간 수 · 최대 길이는 마커를 고르면 '선택 마커' 패널에 나와요.")), "the legend tells where the other numbers are");
    }

    #[test]
    fn right_column_click_ctrl_command_and_shift_use_the_existing_selection_actions() {
        let mut p = PaneTest::new(&[&["M0", "M1", "M2", "M3", "M4"]]);
        let painted = texts(&p.idle());
        let mut v = crate::views::View::default();
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        let command = egui::Modifiers { command: true, ..Default::default() };
        let shift = egui::Modifiers { shift: true, ..Default::default() };
        for (name, modifiers, want, additive, selection) in [
            ("M3", egui::Modifiers::NONE, 3, false, vec![3]),
            ("M0", ctrl, 0, true, vec![0, 3]),
            ("M3", ctrl, 3, true, vec![0]),
            ("M4", command, 4, true, vec![0, 4]),
            ("M3", shift, 3, false, vec![3]),
        ] {
            let action = p.click(at(&painted, name).rect.center(), modifiers);
            let Some(Action::Select { marker, additive: got }) = action else { panic!("{name} did not select a marker") };
            assert_eq!((marker, got), (want, additive));
            v.select(marker, got);
            assert_eq!(v.selected.iter().copied().collect::<Vec<_>>(), selection);
            p.selected = v.selected.clone();
            p.primary = v.primary;
        }
    }

    #[test]
    fn odd_empty_cell_is_not_a_click_target_and_group_header_keeps_full_width() {
        let mut p = PaneTest::new(&[&["M0", "M1", "M2", "M3", "M4"]]);
        let painted = texts(&p.idle());
        let empty = at(&painted, "M2").rect.center() + vec2(190.0, 0.0);
        assert!(p.click(empty, egui::Modifiers::NONE).is_none());
        let header_right = pos2(at(&painted, "M3").rect.center().x, at(&painted, "Group0").rect.center().y);
        assert!(p.click(header_right, egui::Modifiers::NONE).is_none());
        assert!(p.state.collapsed.contains(&0));
        let after = texts(&p.idle());
        assert!(!after.iter().any(|t| t.text == "M0" || t.text == "M3"));
        p.click(header_right, egui::Modifiers::NONE);
        assert!(!p.state.collapsed.contains(&0));
    }

    #[test]
    fn long_names_in_both_cells_are_font_elided_and_the_gaps_stay_visible() {
        let left = "A_very_long_marker_name_abcdefghijklmnopqrstuvwxyz";
        let right = "B_very_long_marker_name_abcdefghijklmnopqrstuvwxyz";
        let mut p = PaneTest::new(&[&[left, right]]);
        p.data.markers[0].fill_pct = 83.0;
        p.data.markers[1].fill_pct = 57.0;
        let painted = texts(&p.idle());
        for (name, value) in [(left, "17%"), (right, "43%")] {
            let (name, value) = (at(&painted, name), at(&painted, value));
            assert!(name.elided);
            assert!(name.glyphs.ends_with('…'), "inspect painted glyphs, not the original LayoutJob text");
            assert!(name.rect.right() <= name.clip.right() + 0.5);
            assert!(name.rect.right() < value.rect.left());
            assert!(!value.elided);
            assert_eq!(value.glyphs, value.text);
            assert!(value.clip.contains_rect(value.rect));
        }
    }

    #[test]
    fn narrow_pane_paints_one_column_and_resizing_to_260pt_restores_two() {
        let mut p = PaneTest::new(&[&["M0", "M1", "M2", "M3", "M4"]]);
        p.width = 259.0;
        let painted = texts(&p.idle());
        assert_eq!(painted.iter().filter(|t| t.text == "Gaps").count(), 1);
        assert_eq!(painted.iter().filter(|t| t.text == "Labels").count(), 1);
        for i in 1..5 {
            let (a, b) = (at(&painted, &format!("M{}", i - 1)), at(&painted, &format!("M{i}")));
            assert!((a.rect.left() - b.rect.left()).abs() < 0.1);
            assert!(a.rect.top() < b.rect.top());
        }
        p.width = 260.0;
        let painted = texts(&p.idle());
        assert_eq!(painted.iter().filter(|t| t.text == "Gaps").count(), 2);
        assert!((at(&painted, "M0").rect.top() - at(&painted, "M3").rect.top()).abs() < 0.1);
    }

    #[test]
    fn right_column_scroll_target_is_visible_with_nondefault_spacing_and_only_nearby_rows_are_painted() {
        let names: Vec<String> = (0..200).map(|i| format!("M{i:03}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut p = PaneTest::new(&[&refs]);
        p.height = 130.0;
        p.spacing = 9.0;
        let before = texts(&p.idle());
        assert!(!before.iter().any(|t| t.text == "M180"));
        p.state.scroll_to = Some(180);
        let out = p.idle();
        let painted = texts(&out);
        let target = at(&painted, "M180");
        assert!(target.clip.contains_rect(target.rect));
        assert!((target.rect.top() - at(&painted, "M080").rect.top()).abs() < 0.1);
        // The name has its own cell clip. Use the row background's clip for the viewport.
        let (row_top, viewport_top) = out.shapes.iter().find_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.rect.contains(target.rect.center()) && (r.rect.height() - ROW_H).abs() < 0.1 => Some((r.rect.top(), s.clip_rect.top())),
            _ => None,
        }).expect("the target's background is painted");
        assert!((row_top - viewport_top - 3.0 * (ROW_H + p.spacing)).abs() < 6.0, "three actual row pitches below the viewport top");
        assert_eq!(p.state.scroll_to, None);
        assert!(!painted.iter().any(|t| t.text == "M000" || t.text == "M199"));
        assert!(painted.iter().filter(|t| t.text.starts_with('M')).count() < 20, "virtualized instead of painting all 200 markers");
        assert!(texts(&p.idle()).iter().any(|t| t.text == "M180"));
    }

    #[test]
    fn legend_keeps_space_below_a_list_that_fills_the_remaining_pane() {
        let names: Vec<String> = (0..200).map(|i| format!("M{i:03}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut p = PaneTest::new(&[&refs]);
        p.height = f32::INFINITY;
        let painted = texts(&p.idle());
        let legend = painted.iter().find(|t| t.text.contains("빈구간 수 · 최대 길이")).expect("the legend is painted");
        assert!(legend.clip.contains_rect(legend.rect), "the docked pane must not clip its legend below the scroll area");
        assert!(legend.rect.bottom() <= 692.5, "inside the 700 pt screen's panel margin");
    }

    #[test]
    fn marker_colors_absence_and_primary_secondary_highlights_stay_per_cell() {
        let mut p = PaneTest::new(&[&["M0", "M1", "M2", "M3"]]);
        p.selected = [0, 2].into_iter().collect();
        p.primary = Some(2);
        let out = p.idle();
        let painted = texts(&out);
        assert_eq!(at(&painted, "M0").color, theme::LABEL_TRACKED);
        assert_eq!(at(&painted, "M2").color, theme::LABEL_TRACKED);
        for (name, fill) in [("M0", theme::SELECT_SECONDARY.gamma_multiply(0.22)), ("M2", theme::SELECT_PRIMARY.gamma_multiply(0.28))] {
            assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == fill && r.rect.contains(at(&painted, name).rect.center()) && (r.rect.width() - 190.0).abs() < 0.5)));
        }
        for m in &p.data.markers {
            assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Circle(c) if c.fill == theme::color(m.color) && (c.radius - 4.0).abs() < 0.1)));
        }
        p.frame = 10;
        let absent = texts(&p.idle());
        assert_eq!(at(&absent, "M0").color, theme::LABEL_UNTRACKED);
        assert_eq!(at(&absent, "M2").color, theme::LABEL_UNTRACKED);
    }
}

// ───────────────────────── Selected marker ─────────────────────────

/// Lanes of the selected markers' X / Y / Z graph, built once per (file, graphed markers)
/// instead of every frame, and what the graph worked out to draw them (`graph::Drawn`). One
/// cache per graph (pane or floating). Several markers (v0.27.0, `graphed`): the first one's
/// lanes here, the others' in `graph::Drawn::push_more`, every marker in the lanes' colours
/// X red, Y green, Z blue (v0.29.0; v0.27.0 gave each marker a colour of its own).
/// The first marker is built at once; the others as far as the frame's work allows
/// (`graph::spend`), the rest in the next frames (v0.27.0 speed fix).
#[derive(Default)]
pub struct LaneCache {
    /// `--hover` / `--bench-hover` (agent only, v0.29.0): the graph is drawn as if the pointer
    /// were on this graphed marker's curve (its place in `graphed`), `graph::Drawn::set_hover_force`.
    pub hover_at: Option<usize>,
    key: Option<(usize, Vec<usize>)>,
    lanes: Vec<Lane>,
    /// Frames where a graphed marker has no value (the frame bar's red marks): the one marker's
    /// gaps, or those of any of several.
    gaps: Vec<(i64, i64)>,
    drawn: graph::Drawn,
    /// Markers of `key` whose lanes are built (the first one's are `lanes`, the others' in
    /// `drawn`).
    built: usize,
    /// How many times lanes were built (tests: once per file and marker).
    #[cfg(test)]
    pub builds: usize,
    /// The file laid over the view whose lanes are in `drawn` (`pair`); None = none.
    pair_key: Option<PathBuf>,
    /// Markers whose dashed lanes (of `pair_key`'s file) are in `drawn`.
    pairs_built: usize,
    /// How many times the second file's lanes were built (tests).
    #[cfg(test)]
    pub pair_builds: usize,
}

/// The marker of `other` with the name of `data`'s marker `mi` (plan step 9, dashed lanes): none
/// for an unlabeled marker (its name is no label) and none among `other`'s unlabeled ones; of
/// several, the one in a group of the same title (the same asset), else the first.
fn same_name(data: &ViewData, mi: usize, other: &ViewData) -> Option<usize> {
    let m = &data.markers[mi];
    if m.kind == MarkerKind::Unlabeled {
        return None;
    }
    let found: Vec<usize> = (0..other.markers.len()).filter(|&k| other.markers[k].kind != MarkerKind::Unlabeled && other.markers[k].name == m.name).collect();
    let title = &data.groups[m.group].title;
    found.iter().copied().find(|&k| other.groups[other.markers[k].group].title == *title).or(found.first().copied())
}

/// Samples of marker `mi` (its frames with a value): the cost of building its lanes
/// (`graph::spend`).
fn marker_cost(data: &ViewData, mi: usize) -> usize {
    data.markers[mi].segments.iter().map(|&(s, e)| (e - s + 1).max(0) as usize).sum()
}

impl LaneCache {
    /// Plan step 9: with a file laid over the view (`over` = its path and data), its marker of the
    /// same name goes into the lanes of each graphed marker (`markers`, `graphed`; the first
    /// one's `graph::Pair`, the others' `Drawn::push_more_pair`) as dashed lines in the lane's
    /// color (v0.29.0), and `shown` = which of the two files are shown (name tags 1 / 2). Call it every
    /// frame before `get`: the second file's lanes are built only for another file, markers or
    /// overlay (the first marker's at once, the others' as far as the frame's work allows, after
    /// their solid lanes).
    pub fn pair(&mut self, data: &ViewData, file_id: usize, markers: &[usize], over: Option<(&Path, &ViewData)>, shown: [bool; 2]) {
        if markers.is_empty() || markers.iter().any(|m| *m >= data.markers.len()) {
            return;
        }
        let fresh = !self.is_for(file_id, markers);
        self.get(data, file_id, markers);
        // Frames counted as the first file's (`View::overlay_frame`).
        let dashed = |o: &ViewData, mi: usize| {
            let shift = (data.start_frame - o.start_frame) as f64;
            let mut lanes = same_name(data, mi, o).map_or_else(Vec::new, |k| marker_lanes(o, k));
            for p in lanes.iter_mut().flat_map(|l| l.runs.iter_mut()).flatten() {
                p[0] += shift;
            }
            lanes
        };
        let cost = |o: &ViewData, mi: usize| same_name(data, mi, o).map_or(0, |k| marker_cost(o, k));
        if fresh || self.pair_key.as_deref() != over.map(|(p, _)| p) {
            let pair = over.map(|(_, o)| {
                #[cfg(test)]
                {
                    self.pair_builds += 1;
                }
                graph::spend(cost(o, markers[0]), true);
                graph::Pair { lanes: dashed(o, markers[0]), shown }
            });
            self.pairs_built = usize::from(pair.is_some());
            self.drawn.set_pair(pair);
            self.drawn.set_more_pairs(vec![]);
            self.pair_key = over.map(|(p, _)| p.to_path_buf());
        }
        if let Some((_, o)) = over {
            while self.pairs_built < self.built && graph::spend(cost(o, markers[self.pairs_built]), false) {
                self.drawn.push_more_pair(dashed(o, markers[self.pairs_built]));
                self.pairs_built += 1;
            }
        }
        self.drawn.set_shown(shown);
    }

    fn is_for(&self, file_id: usize, markers: &[usize]) -> bool {
        self.key.as_ref().is_some_and(|(f, m)| *f == file_id && m == markers)
    }

    /// Something of the graph is not built or not drawn as it will be yet (a few markers per
    /// frame, `graph::spend`; tests).
    #[cfg(test)]
    pub fn pending(&self) -> bool {
        let n = self.key.as_ref().map_or(0, |(_, m)| m.len());
        self.built < n || (self.pair_key.is_some() && self.pairs_built < n) || self.drawn.pending()
    }

    /// Tests: the second file's lanes and shown state in the graph.
    #[cfg(test)]
    pub fn pair_now(&self) -> Option<&graph::Pair> {
        self.drawn.pair()
    }

    /// Tests: what the graph painted last.
    #[cfg(test)]
    pub fn drew(&self) -> &graph::Drew {
        &self.drawn.drew
    }

    /// Tests: how many markers' lanes and dashed lanes are built.
    #[cfg(test)]
    pub fn built(&self) -> (usize, usize) {
        (self.built, self.pairs_built)
    }

    /// X / Y / Z of the graphed markers `markers` (`graphed`, not empty), and the frames where
    /// one of them has no value; made again only for another file or other markers. Every lane
    /// in its colour, X red, Y green, Z blue, whether one marker or several (v0.29.0, user
    /// "graph 색깔은 지금 x,y,z 색깔대로"): every lane holds each marker's curve in that colour.
    /// The first marker's lanes are built at once, the others' as far as the frame's work
    /// allows (`graph::spend`), the rest at the next calls.
    pub fn get(&mut self, data: &ViewData, file_id: usize, markers: &[usize]) -> (&[Lane], &[(i64, i64)], &mut graph::Drawn) {
        let several = markers.len() > 1;
        let lanes_of = |k: usize| marker_lanes(data, markers[k]);
        if !self.is_for(file_id, markers) {
            graph::spend(marker_cost(data, markers[0]), true);
            self.lanes = lanes_of(0);
            self.gaps = if several { merged(markers.iter().map(|&mi| &data.markers[mi].gaps[..])) } else { data.markers[markers[0]].gaps.clone() };
            self.drawn = graph::Drawn::default();
            // v0.29.0 hover: the names painted next to the pointer over a curve.
            self.drawn.set_names(markers.iter().map(|&mi| data.markers[mi].name.clone()).collect());
            self.built = 1;
            self.key = Some((file_id, markers.to_vec()));
            #[cfg(test)]
            {
                self.builds += 1;
            }
        }
        while self.built < markers.len() && graph::spend(marker_cost(data, markers[self.built]), false) {
            self.drawn.push_more(lanes_of(self.built));
            self.built += 1;
        }
        self.drawn.set_hover_force(self.hover_at);
        (&self.lanes, &self.gaps, &mut self.drawn)
    }
}
// Most markers one graph draws (v0.27.0): a box selection can take 40, but more curves than
/// this cannot be told apart and cost frame time. Those past it are left out, said in text
/// ("선택 23개 중 8개").
pub const GRAPH_MAX: usize = 8;

/// The markers the X / Y / Z graph draws (v0.27.0), for `primary` and the selection: every
/// selected marker in LABELS order (groups top to bottom, in each its markers), so the same
/// markers are always listed in the same order whatever order they were clicked in; at most
/// `GRAPH_MAX`, the first in that order. Also how many are selected (`primary` counted).
pub fn graphed(data: &ViewData, selected: &BTreeSet<usize>, primary: usize) -> (Vec<usize>, usize) {
    let n = data.markers.len();
    let wanted = |m: usize| m < n && (m == primary || selected.contains(&m));
    let mut out: Vec<usize> = Vec::new();
    for &m in data.groups.iter().flat_map(|g| &g.members) {
        if wanted(m) && !out.contains(&m) {
            out.push(m);
        }
    }
    // A marker in no group (none in real files): after the others, by number.
    let rest: Vec<usize> = selected.iter().copied().chain([primary]).filter(|&m| wanted(m) && !out.contains(&m)).collect::<BTreeSet<_>>().into_iter().collect();
    out.extend(rest);
    let total = out.len();
    out.truncate(GRAPH_MAX);
    (out, total)
}

/// The names of a graph of several markers (`graph::Key`): name and whether it is tracked at
/// `frame`, in `graphed` order. Empty for one marker (its tag shows its name as before).
/// v0.29.0: no colour per marker any more (every curve in its lane's colour), so these are only
/// the names, written `LASI · LKNE · RKNE`.
pub fn graph_keys<'a>(data: &'a ViewData, markers: &[usize], frame: i64) -> Vec<graph::Key<'a>> {
    if markers.len() < 2 {
        return Vec::new();
    }
    markers
        .iter()
        .map(|&mi| {
            let m = &data.markers[mi];
            let name_color = if m.present(frame) { theme::LABEL_TRACKED } else { theme::LABEL_UNTRACKED };
            graph::Key { name: &m.name, name_color }
        })
        .collect()
}

/// Between two names of a graph of several markers (v0.29.0): `LASI · LKNE · RKNE`.
pub const NAME_SEP: &str = "·";

/// "선택 23개 중 8개": the graph draws `drawn` of `total` selected markers (`GRAPH_MAX`).
pub fn left_out_text(drawn: usize, total: usize) -> String {
    format!("선택 {total}개 중 {drawn}개")
}

/// The Comment bands of a marker graph (Comment plan step 6, `graph::Span`): the frames of each
/// Comment that names the graphed marker (by name, as the band strip and the COMMENTS pane do),
/// in list order, the selected one (`View::picked`) marked. Comments without markers are not
/// here (they are about the whole frame; the band strip shows them). Made again only when the
/// file, the marker, the comment list (`Comments::revision`: add / edit / delete / undo) or the
/// selected Comment changes, so a frame only paints the few bands in view.
#[derive(Default)]
pub struct SpanCache {
    key: Option<(usize, Vec<String>, u64, Option<u64>)>,
    spans: Vec<graph::Span>,
    /// How many times the bands were made (tests).
    #[cfg(test)]
    pub builds: usize,
}

impl SpanCache {
    /// One marker's bands (tests; the graphs use `get_many`).
    #[cfg(test)]
    pub fn get(&mut self, list: &Comments, file_id: usize, marker: &str, picked: Option<u64>) -> &[graph::Span] {
        self.get_many(list, file_id, &[marker], picked)
    }

    /// v0.27.0, a graph of several markers: one band for each Comment that names at least one
    /// of `markers` (once per Comment, however many of them it names).
    pub fn get_many(&mut self, list: &Comments, file_id: usize, markers: &[&str], picked: Option<u64>) -> &[graph::Span] {
        let fresh = match &self.key {
            Some((f, m, r, p)) => *f != file_id || !m.iter().map(String::as_str).eq(markers.iter().copied()) || *r != list.revision() || *p != picked,
            None => true,
        };
        if fresh {
            self.key = Some((file_id, markers.iter().map(|m| (*m).to_owned()).collect(), list.revision(), picked));
            self.spans = list
                .list()
                .iter()
                .filter(|c| c.markers.iter().any(|m| markers.contains(&m.as_str())))
                .map(|c| graph::Span { start: c.start, end: c.end, picked: picked == Some(c.id) })
                .collect();
            #[cfg(test)]
            {
                self.builds += 1;
            }
        }
        &self.spans
    }
}

/// The device of force plate `pi` (`DeviceView::plate`), when it has channels.
fn plate_device(data: &ViewData, pi: usize) -> Option<&DeviceView> {
    data.devices.iter().find(|d| d.plate == Some(pi) && !d.channels.is_empty())
}

/// Lanes plate `pi`'s graph can show (`graph_float::PLATE_LANES`): its device's channels 0..6
/// (Fx · Fy · Fz · Mx · My · Mz; a device with fewer channels offers fewer lanes). Without a
/// device only Fz (`PlateView::fz`).
pub fn plate_offer(data: &ViewData, pi: usize) -> LaneSet {
    plate_device(data, pi).map_or(LaneSet::only(FZ_CHANNEL), |d| LaneSet::first(d.channels.len()))
}

/// Lanes of a chosen force plate's graph (`graph_float::Kind::Plate`). Each channel's lane is
/// built once per (file, plate, channel), when it is first shown, and kept while that plate
/// stays: choosing one more lane builds only that one, and a lane taken away is kept for later.
/// What the graph worked out (`graph::Drawn`) is worked out again only when the lanes change.
/// Call it only once the forces are in (`ViewData::analog_pending` false).
#[derive(Default)]
pub struct PlateCache {
    key: Option<(usize, usize)>,
    /// The lanes in `lanes`.
    set: LaneSet,
    /// Shown lanes, top to bottom (channel order), and per lane its channel and its frames
    /// without a value.
    lanes: Vec<Lane>,
    shown: Vec<(usize, Vec<(i64, i64)>)>,
    /// Lanes built before and not shown now: (channel, lane, frames without a value).
    kept: Vec<(usize, Lane, Vec<(i64, i64)>)>,
    /// Frames where a shown lane has no value (red marks of the frame bar).
    gaps: Vec<(i64, i64)>,
    drawn: graph::Drawn,
    /// How many lanes were built (tests: once per file, plate and channel).
    #[cfg(test)]
    pub builds: usize,
}

impl PlateCache {
    /// The lanes `set` (`LaneSet::shown`) of plate `pi`, the frames where one of them has no
    /// value, and what the graph worked out for them.
    pub fn get(&mut self, data: &ViewData, file_id: usize, pi: usize, set: LaneSet) -> (&[Lane], &[(i64, i64)], &mut graph::Drawn) {
        if self.key != Some((file_id, pi)) {
            *self = PlateCache { key: Some((file_id, pi)), set: LaneSet(0), #[cfg(test)] builds: self.builds, ..PlateCache::default() };
        }
        if self.set != set {
            let mut pool: Vec<(usize, Lane, Vec<(i64, i64)>)> = self.shown.drain(..).zip(self.lanes.drain(..)).map(|((ch, gaps), lane)| (ch, lane, gaps)).collect();
            pool.append(&mut self.kept);
            for ch in set.iter() {
                let (lane, gaps) = match pool.iter().position(|p| p.0 == ch) {
                    Some(at) => {
                        let (_, lane, gaps) = pool.swap_remove(at);
                        (lane, gaps)
                    }
                    None => self.build(data, pi, ch),
                };
                self.lanes.push(lane);
                self.shown.push((ch, gaps));
            }
            self.kept = pool;
            self.gaps = merged(self.shown.iter().map(|(_, g)| g.as_slice()));
            self.drawn = graph::Drawn::default();
            self.set = set;
        }
        (&self.lanes, &self.gaps, &mut self.drawn)
    }

    /// Lane of channel `ch` of plate `pi` and its frames without a value. Units are not
    /// verified in this project: no unit text (as in the devices pane).
    fn build(&mut self, data: &ViewData, pi: usize, ch: usize) -> (Lane, Vec<(i64, i64)>) {
        #[cfg(test)]
        {
            self.builds += 1;
        }
        let empty = Vec::new();
        let per_frame = match plate_device(data, pi) {
            Some(d) => d.channels.get(ch).map_or(&empty, |c| &c.per_frame),
            None if ch == FZ_CHANNEL => &data.plates[pi].fz,
            None => &empty,
        };
        let (runs, gaps) = runs_and_gaps(per_frame, data.start_frame, data.end_frame);
        let (letter, color) = PLATE_LANES[ch];
        (Lane { letter: letter.to_owned(), color, runs, unit: "" }, gaps)
    }
}

/// Frame ranges of several lists as one sorted list without overlaps.
fn merged<'a>(lists: impl Iterator<Item = &'a [(i64, i64)]>) -> Vec<(i64, i64)> {
    let mut all: Vec<(i64, i64)> = lists.flatten().copied().collect();
    all.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::with_capacity(all.len());
    for (a, b) in all {
        match out.last_mut() {
            Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// Per-frame values (NaN = none, first value at frame `start`) -> runs of (frame, value)
/// without gaps, and the gaps as (first, last) frame.
fn runs_and_gaps(per_frame: &[f32], start: i64, end: i64) -> (Vec<Vec<[f64; 2]>>, Vec<(i64, i64)>) {
    let mut runs: Vec<Vec<[f64; 2]>> = vec![];
    let mut cur: Vec<[f64; 2]> = vec![];
    let mut gaps = vec![];
    let mut gap_start: Option<i64> = None;
    for (k, v) in per_frame.iter().enumerate() {
        let f = start + k as i64;
        if v.is_finite() {
            cur.push([f as f64, *v as f64]);
            if let Some(s) = gap_start.take() {
                gaps.push((s, f - 1));
            }
        } else {
            if !cur.is_empty() {
                runs.push(std::mem::take(&mut cur));
            }
            gap_start.get_or_insert(f);
        }
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    if let Some(s) = gap_start {
        gaps.push((s, end));
    }
    (runs, gaps)
}

fn marker_lanes(data: &ViewData, mi: usize) -> Vec<Lane> {
    let m = &data.markers[mi];
    let t = &data.take.trajectories[m.traj];
    // Motive Graph View shows millimetres; file units assumed metres (unverified).
    let to_mm = data.unit_scale as f64 * 1000.0;
    let mut lanes = Vec::new();
    for (axis, (letter, color)) in [("X", theme::AXIS_X), ("Y", theme::AXIS_Y), ("Z", theme::AXIS_Z)].into_iter().enumerate() {
        let mut runs = Vec::new();
        let mut k = 0usize;
        for &(s, e) in &m.segments {
            let n = (e - s + 1) as usize;
            runs.push((0..n).map(|j| [t.frames[k + j] as f64, t.xyz[k + j][axis] as f64 * to_mm]).collect());
            k += n;
        }
        lanes.push(Lane { letter: letter.to_owned(), color, runs, unit: "mm" });
    }
    lanes
}

/// Facts about the selected marker (its graph is `graph_pane`).
pub fn selected_pane(ui: &mut Ui, data: &ViewData, frame: i64, selected: &BTreeSet<usize>, primary: Option<usize>) -> Option<Action> {
    let Some(mi) = primary.filter(|m| *m < data.markers.len()) else {
        ui.add_space(4.0);
        ui.label(RichText::new("3D 화면이나 Labels에서 마커를 클릭하세요.").color(theme::TEXT_DIM));
        return None;
    };
    let m = &data.markers[mi];
    if selected.len() > 1 {
        ui.label(RichText::new(format!("선택 {}개 · 아래는 마지막으로 고른 마커(하늘색 테두리)", selected.len())).color(theme::TEXT_DIM));
    }
    egui::Grid::new("sel_info").num_columns(2).spacing([10.0, 3.0]).striped(true).show(ui, |ui| {
        let row = |ui: &mut Ui, k: &str, v: String| {
            ui.label(RichText::new(k).color(theme::TEXT_DIM));
            ui.label(v);
            ui.end_row();
        };
        ui.label(RichText::new("이름").color(theme::TEXT_DIM));
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 5.0, theme::color(m.color));
            let color = if m.present(frame) { theme::LABEL_TRACKED } else { theme::LABEL_UNTRACKED };
            ui.label(RichText::new(&m.name).strong().color(color));
        });
        ui.end_row();
        let lenient = data.take.trajectories[m.traj].lenient_membership;
        row(ui, "종류", format!("{}{}", kind_label(m.kind), if lenient { " (copy 파일 형식에서 읽음)" } else { "" }));
        row(ui, "소속", data.groups[m.group].title.clone());
        row(ui, "slot", m.slot.map_or("-".into(), |s| s.to_string()));
        row(ui, "UID", uid_hex(&m.uid));
        row(ui, "부모 UID", m.parent.map_or("-".into(), |p| uid_hex(&p)));
        row(ui, "점 수 / Gaps", format!("{} / {}", m.n_samples, if m.gaps.is_empty() { "0%".into() } else { gaps_text(m.fill_pct) }));
        row(ui, "빈 구간 / 최대", format!("{}개 / {} 프레임", m.gaps.len(), m.max_gap));
        match m.position(frame) {
            Some(p) => row(ui, "현재 X Y Z (mm)", format!("{:.2}  {:.2}  {:.2}", p.x * 1000.0, p.y * 1000.0, p.z * 1000.0)),
            None => row(ui, "현재 X Y Z", "없음 (빈 구간)".into()),
        }
    });
    None
}

/// GRAPH pane button: put the graph back over the 3D views (`graph_float.rs`).
fn over_3d_button(ui: &mut Ui) -> bool {
    ui.add(egui::Button::new(RichText::new("3D 위로").small()).small()).on_hover_text("그래프를 3D 화면 위에 띄워요. 이 패널은 닫혀요.").clicked()
}

/// X / Y / Z graph of the selected marker in the GRAPH pane (the graph is here instead of
/// floating over the 3D views). The three lanes share the height the pane has, so a low
/// panel shows all of them. `spans`: the Comment bands of the graphed markers (step 6),
/// worked out by the caller from `SpanCache`. v0.27.0: `markers` = the graphed markers
/// (`graphed`, empty = none selected) of `total` selected; with several, the row under the graph
/// names them, `LASI · LKNE · RKNE` (v0.29.0: no colour lines, the curves are all in the lanes'
/// colours; the mouse over a curve shows its marker's name, `graph::show`), and
/// "선택 N개 중 8개" when some are left out.
#[allow(clippy::too_many_arguments)]
pub fn graph_pane(ui: &mut Ui, data: &ViewData, frame: i64, markers: &[usize], total: usize, file_id: usize, cache: &mut LaneCache, spans: &[graph::Span]) -> Option<Action> {
    let Some(mi) = markers.first().copied().filter(|m| *m < data.markers.len()) else {
        ui.add_space(4.0);
        let mut over = false;
        ui.horizontal(|ui| {
            over = over_3d_button(ui);
            ui.label(RichText::new("3D 화면이나 Labels에서 마커를 클릭하면 X·Y·Z 그래프가 나와요.").color(theme::TEXT_DIM));
        });
        return over.then_some(Action::GraphOver3d);
    };
    let m = &data.markers[mi];
    let (lanes, gaps, drawn) = cache.get(data, file_id, markers);
    const HINT_H: f32 = 22.0;
    let lane_h = ((ui.available_height() - graph::NAV_H - HINT_H) / lanes.len().max(1) as f32).clamp(34.0, 140.0);
    let goto = graph::show(ui, Id::new(("sel_graph", file_id)), lanes, data.start_frame, data.end_frame, frame, gaps, lane_h, &Look::pane(), Some(drawn), spans);
    let mut over = false;
    let keys = graph_keys(data, markers, frame);
    ui.horizontal(|ui| {
        over = over_3d_button(ui);
        let color = if m.present(frame) { theme::LABEL_TRACKED } else { theme::LABEL_UNTRACKED };
        if keys.is_empty() {
            ui.label(RichText::new(&m.name).small().strong().color(color));
        }
        // v0.27.0: the graphed markers' names; v0.29.0: plain, `LASI · LKNE · RKNE`.
        for (i, k) in keys.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new(NAME_SEP).small().color(theme::TEXT_DIM));
            }
            ui.label(RichText::new(k.name).small().strong().color(k.name_color));
        }
        if total > markers.len() {
            ui.label(RichText::new(left_out_text(markers.len(), total)).small().color(theme::TEXT));
        }
        ui.label(RichText::new("· 선 위에 마우스 = 마커 이름 · 클릭·드래그 = 프레임 이동 · 휠 = 확대 · 가운데 드래그 = 옆으로 · 더블클릭 = 전체 · 아래 빨간 칸 = 빈 구간").small().color(theme::TEXT_DIM));
    });
    if over {
        return Some(Action::GraphOver3d);
    }
    goto.map(Action::GotoFrame)
}

// ───────────────────────────── Devices ─────────────────────────────

#[derive(Default)]
pub struct DevicesState {
    pub channel: HashMap<usize, usize>,
}

/// Show force plates and analog devices. A chosen plate can float over the active 3D view;
/// other analog devices remain in the devices pane.
pub fn devices_pane(ui: &mut Ui, data: &ViewData, frame: i64, st: &mut DevicesState, file_id: usize, chosen: Option<usize>) -> Option<Action> {
    if data.analog_pending {
        ui.add_space(4.0);
        ui.label(RichText::new("힘판 힘·EMG를 불러오는 중이에요 (마커를 먼저 보여 줘요).").color(theme::TEXT_DIM));
        return None;
    }
    if data.plates.is_empty() && data.devices.is_empty() {
        ui.add_space(4.0);
        ui.label(RichText::new("이 파일에는 forceplate·아날로그 장치가 없어요.").color(theme::TEXT_DIM));
        return None;
    }
    let idx = frame - data.start_frame;
    let mut action = None;
    if !data.plates.is_empty() {
        egui::Grid::new("plates").num_columns(4).spacing([12.0, 3.0]).striped(true).show(ui, |ui| {
            ui.label(RichText::new("Force plate").color(theme::TEXT_DIM));
            ui.label(RichText::new("현재 Fz").color(theme::TEXT_DIM));
            ui.label(RichText::new("최대 |Fz|").color(theme::TEXT_DIM));
            ui.label("");
            ui.end_row();
            for (pi, p) in data.plates.iter().enumerate() {
                let on = chosen == Some(pi);
                ui.label(if on { RichText::new(&p.name).color(theme::PLATE_CHOSEN).strong() } else { RichText::new(&p.name) });
                let v = if idx >= 0 { p.fz.get(idx as usize).copied() } else { None };
                ui.label(match v {
                    Some(v) if v.is_finite() => format!("{v:.1}"),
                    _ => "-".into(),
                });
                ui.label(format!("{:.1}", p.fz_abs_max));
                let tip = if on { "3D 위 그래프를 닫아요" } else { "이 판의 그래프를 지금 3D 화면 위에 띄워요 (3D에서 판을 눌러도 돼요)" };
                if ui.add(egui::Button::new(RichText::new("3D 위로").small()).small().selected(on)).on_hover_text(tip).clicked() {
                    action = Some(Action::ChoosePlate(pi));
                }
                ui.end_row();
            }
        });
        ui.label(RichText::new("바닥의 판 색 진하기 = |Fz| ÷ 모든 판 중 최대 |Fz| (단위 미확인)").small().color(theme::TEXT_DIM));
    }
    for (di, d) in data.devices.iter().enumerate() {
        egui::CollapsingHeader::new(&d.title).id_salt(("device", di)).default_open(d.plate.is_some()).show(ui, |ui| {
            if d.channels.is_empty() {
                ui.label("채널 없음");
                return;
            }
            let default = if d.plate.is_some() { FZ_CHANNEL.min(d.channels.len() - 1) } else { 0 };
            let ch = st.channel.entry(di).or_insert(default);
            *ch = (*ch).min(d.channels.len() - 1);
            egui::ComboBox::from_id_salt(("device_ch", di)).selected_text(d.channels[*ch].name.clone()).show_ui(ui, |ui| {
                for (k, c) in d.channels.iter().enumerate() {
                    ui.selectable_value(ch, k, format!("{} ({}값/프레임)", c.name, c.dim));
                }
            });
            let c = &d.channels[*ch];
            let (runs, gaps) = runs_and_gaps(&c.per_frame, data.start_frame, data.end_frame);
            let lane = Lane { letter: c.name.trim_start_matches("AnalogChannel").to_owned(), color: theme::ACCENT, runs, unit: "" };
            if let Some(f) = graph::show(ui, Id::new(("device_graph", file_id, di, *ch)), &[lane], data.start_frame, data.end_frame, frame, &gaps, 110.0, &Look::pane(), None, &[]) {
                action = Some(Action::GotoFrame(f));
            }
            ui.label(
                RichText::new(format!("프레임마다 앞 {}개 값의 평균 · EMG 장치인지는 파일에서 확인되지 않아요", c.dim.min(VALID_SUBSAMPLES)))
                    .small()
                    .color(theme::TEXT_DIM),
            );
        });
    }
    action
}

// ─────────────────────────── Playback bar (one under each 3D view) ───────────────────────────

/// Height of the seek zone of a timeline (top part): ticks, played part, red marks, the
/// selected marker's gaps, the current-frame line. A press or a drag here moves the frame.
pub const SEEK_H: f32 = 28.0;
/// Height of the band strip: dragging selects a frame range and clicking clears it.
pub const BAND_H: f32 = 12.0;
/// Height of the timeline of a playback bar: seek zone + band strip.
pub const TIMELINE_H: f32 = SEEK_H + BAND_H;
/// Frames of a timeline run from this far right of its left edge to this far left of its
/// right edge (both zones, so a range lines up with the ticks).
const TIMELINE_PAD: f32 = 6.0;
/// Hover text of the band strip.
const BAND_TIP: &str = "끌기 = 구간 잡기 · 그냥 누르기 = 구간 풀기 (프레임은 그대로)\n[ / ] = 지금 프레임을 구간 시작 / 끝으로 · Esc = 구간 풀기\nM을 누른 채 재생하다 떼기 = 그 구간으로 Comment (주황 칸, 고른 마커가 있으면 함께)\nM 짧게 = 잡은 구간 + 고른 마커 · 주황 칸 오른쪽 클릭 = 지우기 · Ctrl+Z / Ctrl+Y = 되돌리기 / 다시 하기";
/// A comment block is at least this wide (a one-frame comment stays visible and clickable).
pub const BLOCK_MIN_W: f32 = 4.0;
/// Text on a comment block (dark on orange).
const BLOCK_FONT: f32 = 9.0;
/// Height of a view's playback bar: margin, timeline, gap, button row, margin.
pub const DECK_H: f32 = 4.0 + TIMELINE_H + 3.0 + 24.0 + 4.0;
/// Above this width the playback bar shows all buttons. Narrower bars keep the essential
/// controls visible and place the rest in a menu.
pub const WIDE_DECK_MIN: f32 = 700.0;

pub fn deck_is_wide(width: f32) -> bool {
    width >= WIDE_DECK_MIN
}

/// Red occlusion marks of a timeline, one value per pixel column. Made again only when the
/// file or the width changes, so the work of one frame does not grow with the file length.
#[derive(Default)]
pub struct HeatCache {
    key: (usize, usize),
    cols: Vec<f32>,
}

impl HeatCache {
    pub fn get(&mut self, occlusion: &[f32], file_id: usize, cols: usize) -> &[f32] {
        if self.key != (file_id, cols) || self.cols.len() != cols {
            self.key = (file_id, cols);
            self.cols = heat_columns(occlusion, cols);
        }
        &self.cols
    }
}

/// One comment's block in the band strip: its x span and its texts (long: `<id> <first marker>`
/// and ` +N` for more markers; short: the id).
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub id: u64,
    pub x0: f32,
    pub x1: f32,
    pub label: String,
    pub short: String,
    /// Its row in the band and the number of rows of its group of overlapping blocks (step 6,
    /// `band_rows`): 0 of 1 = the full height as before; otherwise the band's height is shared.
    pub row: u8,
    pub rows: u8,
}

/// At most this many rows in the band where blocks overlap (step 6); further ones share the
/// last row (the one drawn later on top there).
pub const BAND_ROWS: u8 = 3;

/// Rows of overlapping blocks in the band (Comment plan step 6), one (row, rows) per item of
/// `items` = (start frame, id, x0, x1). Blocks overlap when their drawn spans do (so a one-frame
/// block widened to `BLOCK_MIN_W` counts by its drawn width). Going by start frame, then id, each
/// block takes the first row free at its x (row 0 = top), else the last of `BAND_ROWS`; a group
/// of blocks that overlap one another in a chain shares the band's height by the rows it uses,
/// and a block keeps one height for its whole length. A block that overlaps no other: (0, 1).
/// A new Comment that overlaps none of a group leaves that group's rows as they were.
fn band_rows(items: &[(i64, u64, f32, f32)]) -> Vec<(u8, u8)> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| (items[i].0, items[i].1));
    let mut out = vec![(0u8, 1u8); items.len()];
    let mut group: Vec<usize> = Vec::new();
    let mut reach = f32::NEG_INFINITY;
    let mut ends = [f32::NEG_INFINITY; BAND_ROWS as usize];
    let close = |group: &mut Vec<usize>, out: &mut Vec<(u8, u8)>| {
        let rows = group.iter().map(|&i| out[i].0 + 1).max().unwrap_or(1);
        for i in group.drain(..) {
            out[i].1 = rows;
        }
    };
    for i in order {
        let (_, _, x0, x1) = items[i];
        if x0 >= reach {
            close(&mut group, &mut out);
            ends = [f32::NEG_INFINITY; BAND_ROWS as usize];
            reach = f32::NEG_INFINITY;
        }
        let row = (0..BAND_ROWS as usize).find(|&r| ends[r] <= x0).unwrap_or(BAND_ROWS as usize - 1);
        ends[row] = ends[row].max(x1);
        reach = reach.max(x1);
        out[i].0 = row as u8;
        group.push(i);
    }
    close(&mut group, &mut out);
    out
}

/// A block's rect in the band strip `band`, x from `x0` to `x1`, at its row (`Block::row` of
/// `Block::rows`); the full height 1 pt in from the band's edges when it overlaps nothing.
pub fn block_row_rect(band: Rect, x0: f32, x1: f32, row: u8, rows: u8) -> Rect {
    let h = (band.height() - 2.0) / rows.max(1) as f32;
    let top = band.top() + 1.0 + row as f32 * h;
    Rect::from_min_max(pos2(x0, top), pos2(x1, top + h))
}

/// Comment blocks of a band strip. Made again only when the file, the comment list
/// (`Comments::revision`), the bar's place or width or the frames change, so the work of one
/// frame does not grow with the file length (as `HeatCache`).
#[derive(Default)]
pub struct BlockCache {
    key: Option<(usize, u64, u32, u32, i64, i64)>,
    blocks: Vec<Block>,
    /// How many times the blocks were laid out (tests).
    #[cfg(test)]
    pub builds: usize,
}

impl BlockCache {
    /// Blocks of `list` on a timeline whose frames `first..=last` run from `left` over `width`.
    /// In the order the comments were added (a later one is drawn on top).
    pub fn get(&mut self, list: &Comments, file_id: usize, first: i64, last: i64, left: f32, width: f32) -> &[Block] {
        let key = (file_id, list.revision(), left.to_bits(), width.to_bits(), first, last);
        if self.key != Some(key) {
            self.key = Some(key);
            self.blocks = blocks(list, first, last, left, width);
            #[cfg(test)]
            {
                self.builds += 1;
            }
        }
        &self.blocks
    }
}

/// Lay out the blocks (see `BlockCache`): x of the first frame to x of the last, at least
/// `BLOCK_MIN_W` wide around the middle; overlapping ones get rows (`band_rows`, step 6).
fn blocks(list: &Comments, first: i64, last: i64, left: f32, width: f32) -> Vec<Block> {
    let mut out = blocks_flat(list, first, last, left, width);
    let items: Vec<(i64, u64, f32, f32)> = list.list().iter().zip(&out).map(|(c, b)| (c.start, c.id, b.x0, b.x1)).collect();
    for (b, (row, rows)) in out.iter_mut().zip(band_rows(&items)) {
        (b.row, b.rows) = (row, rows);
    }
    out
}

/// The blocks of `blocks`, all at the full height (before rows).
fn blocks_flat(list: &Comments, first: i64, last: i64, left: f32, width: f32) -> Vec<Block> {
    list.list()
        .iter()
        .map(|c| {
            let (x0, x1) = block_span(c.start, c.end, first, last, left, width);
            let more = match c.markers.len() {
                0 | 1 => String::new(),
                n => format!(" +{}", n - 1),
            };
            let label = match c.markers.first() {
                Some(m) => format!("{} {m}{more}", c.id),
                None => c.id.to_string(),
            };
            Block { id: c.id, x0, x1, label, short: c.id.to_string(), row: 0, rows: 1 }
        })
        .collect()
}

/// x span of a block of frames `start..=end`: at least `BLOCK_MIN_W` wide around the middle.
fn block_span(start: i64, end: i64, first: i64, last: i64, left: f32, width: f32) -> (f32, f32) {
    let (x0, x1) = (x_of_frame(start, first, last, left, width), x_of_frame(end, first, last, left, width));
    if x1 - x0 < BLOCK_MIN_W {
        let m = (x0 + x1) / 2.0;
        (m - BLOCK_MIN_W / 2.0, m + BLOCK_MIN_W / 2.0)
    } else {
        (x0, x1)
    }
}

/// Largest occlusion value under each of `cols` pixel columns.
fn heat_columns(occlusion: &[f32], cols: usize) -> Vec<f32> {
    let n = occlusion.len();
    if n == 0 {
        return vec![0.0; cols];
    }
    (0..cols)
        .map(|cx| {
            let a = ((cx as f32 / cols as f32 * n as f32) as usize).min(n - 1);
            let b = (((cx + 1) as f32 / cols as f32 * n as f32).ceil() as usize).clamp(a + 1, n);
            occlusion[a..b].iter().cloned().fold(0.0f32, f32::max)
        })
        .collect()
}

/// x of frame `f` on a timeline whose frames run from `left` (first frame) to `left + width`
/// (last frame).
pub fn x_of_frame(f: i64, first: i64, last: i64, left: f32, width: f32) -> f32 {
    let span = (last - first).max(1) as f32;
    left + (f - first) as f32 / span * width
}

/// The frame nearest to `x` on that timeline; left of it = first frame, right of it = last.
pub fn frame_at_x(x: f32, first: i64, last: i64, left: f32, width: f32) -> i64 {
    let span = (last - first).max(1) as f32;
    let t = ((x - left) / width.max(1.0)).clamp(0.0, 1.0);
    (first + (t * span).round() as i64).clamp(first, last.max(first))
}

/// A frame range from two ends given in any order: start <= end, both inside first..=last.
pub fn ordered_range(a: i64, b: i64, first: i64, last: i64) -> (i64, i64) {
    let inside = |f: i64| f.clamp(first, last.max(first));
    let (a, b) = (inside(a), inside(b));
    (a.min(b), a.max(b))
}

/// Height of the type strip (Comment plan step 5).
pub const TYPE_STRIP_H: f32 = 26.0;
const STRIP_FONT: f32 = 13.0;
const STRIP_PAD: f32 = 3.0;
const STRIP_GAP: f32 = 4.0;

/// The type strip's buttons for a bar at `deck` (one per type, full names), in a row over the
/// bottom right of the 3D just above the bar, its right end at the `Comment` button's: floating,
/// so nothing in the bar or the 3D moves.
pub fn type_strip_rects(ctx: &egui::Context, deck: Rect) -> [Rect; 3] {
    let widths = tak_core::comments::TYPES.map(|t| ctx.fonts(|f| f.layout_no_wrap(t.to_owned(), FontId::proportional(STRIP_FONT), egui::Color32::WHITE).size().x) + 14.0);
    let h = TYPE_STRIP_H - 2.0 * STRIP_PAD;
    let mut right = deck.right() - 6.0 - STRIP_PAD;
    let top = deck.top() - 4.0 - TYPE_STRIP_H + STRIP_PAD;
    let mut out = [Rect::NOTHING; 3];
    for k in (0..3).rev() {
        out[k] = Rect::from_min_size(pos2(right - widths[k], top), vec2(widths[k], h));
        right -= widths[k] + STRIP_GAP;
    }
    out
}

/// The whole strip (its buttons and a small margin): clicks inside it do not close it.
pub fn type_strip_rect(buttons: &[Rect; 3]) -> Rect {
    buttons[0].union(buttons[2]).expand(STRIP_PAD)
}

/// Draw the type strip: three text buttons, the comment's type now marked. Returns the type
/// clicked. No hover texts.
pub fn type_strip(ui: &mut Ui, buttons: &[Rect; 3], now: &str) -> Option<&'static str> {
    let whole = type_strip_rect(buttons);
    ui.painter().rect_filled(whole, 4.0, theme::PANEL_BG);
    ui.painter().rect_stroke(whole, 4.0, Stroke::new(1.0_f32, theme::COMMENT));
    let mut picked = None;
    for (t, r) in tak_core::comments::TYPES.into_iter().zip(buttons) {
        let on = now == t;
        let color = if on { theme::TAB_TEXT_ACTIVE } else { theme::TEXT };
        let b = egui::Button::new(RichText::new(t).size(STRIP_FONT).color(color)).wrap_mode(egui::TextWrapMode::Extend).selected(on).min_size(r.size());
        if ui.put(*r, b).clicked() {
            picked = Some(t);
        }
    }
    picked
}

/// What a playback bar asks the app to do with comments (the app owns the file writer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeckAction {
    /// `Comment` button (or its ⋯ menu item): a comment of the range and the selected markers
    /// (none is fine).
    MakeComment,
    /// Plain click on a block: go to that comment (first frame, its markers).
    GotoComment(u64),
    /// Right click on a block: delete it at once (v0.15.0; a menu with `Delete` before).
    DeleteComment(u64),
}

/// The comment part of a view's playback bar.
pub struct CommentBar<'a> {
    pub list: &'a Comments,
    pub blocks: &'a mut BlockCache,
    /// Ok(hover text) = the `Comment` button works; Err(why not, plain Korean) = greyed out.
    pub make: Result<String, String>,
    /// Comments of this file may be deleted (its comment file can be written).
    pub can_delete: bool,
    /// `M` is held in this view since this frame: the band shows that frame to the current one
    /// as the range (instead of the picked range) until `M` goes up.
    pub held: Option<i64>,
    /// The selected comment (v0.17.1): its block gets a white edge.
    pub picked: Option<u64>,
    /// A Frames edit of the COMMENTS pane going on (v0.17.3): (comment id, start, end) as in its
    /// boxes now; that comment's block is drawn there (typed or dragged), not where it is saved.
    pub draft: Option<(u64, i64, i64)>,
}

/// What a playback bar gives back after a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeckOut {
    /// Something for the app to do with comments.
    pub action: Option<DeckAction>,
}

/// A 3D view's playback bar, drawn in `rect` under the view: timeline (seek zone + band strip
/// with the view's comment blocks and frame `range`), then the buttons (`Comment` at the right
/// end of a wide bar, in the ⋯ menu of a narrow one). `slot` keeps the widget ids of the bars
/// apart.
#[allow(clippy::too_many_arguments)]
pub fn deck(
    ui: &mut Ui,
    rect: Rect,
    slot: usize,
    data: Option<&ViewData>,
    pb: &mut Playback,
    range: &mut Option<(i64, i64)>,
    primary: Option<usize>,
    heat: &mut HeatCache,
    file_id: usize,
    now: f64,
    mut comments: CommentBar,
) -> DeckOut {
    ui.painter().rect_filled(rect, 0.0, theme::PANEL_BG_DARK);
    let inner = rect.shrink2(vec2(6.0, 4.0));
    let mut out = DeckOut::default();
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        match data {
            Some(d) => out = timeline(ui, slot, d, pb, range, primary, heat, file_id, now, &mut comments),
            None => {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), TIMELINE_H), Sense::hover());
                ui.painter().rect_filled(r, 2.0, theme::NAV_BG);
            }
        }
        ui.add_enabled_ui(data.is_some(), |ui| {
            ui.horizontal(|ui| {
                let make = if deck_is_wide(rect.width()) {
                    transport(ui, pb, slot, now);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| comment_button(ui, &comments.make)).inner
                } else {
                    transport_narrow(ui, pb, slot, now, &comments.make)
                };
                if make {
                    out.action = Some(DeckAction::MakeComment);
                }
            });
        });
    });
    out
}

/// Edit-mode timeline, one row in two zones (v0.13.0). Seek zone on top: green scrubber, red
/// occlusion marks (brighter = more labeled markers missing), selected marker's gaps as a bright
/// strip; a press or drag moves the frame. Band strip under it: the comment blocks (v0.14.0),
/// then the frame `range` above them. A drag picks the range (the frame stays; also when it
/// starts on a block), a plain click on empty band clears it, a plain click on a block goes to
/// that comment, a right click on a block deletes it (overlapping blocks have their own rows,
/// step 6; past three, the top one in the last row). While
/// `M` is held the range shown is the held one (`CommentBar::held` to the current frame).
#[allow(clippy::too_many_arguments)]
fn timeline(
    ui: &mut Ui,
    slot: usize,
    data: &ViewData,
    pb: &mut Playback,
    range: &mut Option<(i64, i64)>,
    primary: Option<usize>,
    heat: &mut HeatCache,
    file_id: usize,
    now: f64,
    comments: &mut CommentBar,
) -> DeckOut {
    let (whole, _) = ui.allocate_exact_size(vec2(ui.available_width(), TIMELINE_H), Sense::hover());
    let rect = Rect::from_min_size(whole.min, vec2(whole.width(), SEEK_H));
    let band = Rect::from_min_max(pos2(whole.left(), rect.bottom()), whole.max);
    let resp = ui.interact(rect, ui.id().with(("timeline_seek", slot)), Sense::click_and_drag());
    let band_resp = ui.interact(band, ui.id().with(("timeline_band", slot)), Sense::click_and_drag()).on_hover_text(crate::platform::shortcut_text(BAND_TIP));
    ui.painter().rect_filled(whole, 2.0, theme::NAV_BG);
    let bar = rect.shrink2(vec2(TIMELINE_PAD, 0.0));
    let (first, last) = (pb.first, pb.last);
    let x_of = |f: i64| x_of_frame(f, first, last, bar.left(), bar.width());
    let frame_at = |x: f32| frame_at_x(x, first, last, bar.left(), bar.width());
    // A press or drag in the seek zone moves the frame first, so everything below (a held
    // range too) is drawn at the new frame.
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            pb.set_frame(frame_at(pos.x), now);
        }
    }
    // Band strip: its own shade, the comment blocks, the range as an orange dashed box with a
    // faint fill above them.
    let bp = ui.painter_at(band);
    let corner = egui::Rounding { nw: 0.0, ne: 0.0, sw: 2.0, se: 2.0 };
    bp.rect_filled(band, corner, theme::BAND_BG);
    let blocks = comments.blocks.get(comments.list, file_id, first, last, bar.left(), bar.width());
    // The comment of a Frames edit is drawn at the frames in its boxes (v0.17.3).
    let draft = comments.draft;
    let block_rect = |b: &Block| {
        let (x0, x1) = match draft {
            Some((id, s, e)) if id == b.id => block_span(s, e, first, last, bar.left(), bar.width()),
            _ => (b.x0, b.x1),
        };
        // Its row where blocks overlap (step 6); the full height otherwise, as before.
        block_row_rect(band, x0, x1, b.row, b.rows)
    };
    let font = FontId::proportional(BLOCK_FONT);
    for b in blocks {
        let r = block_rect(b);
        bp.rect_filled(r, 1.0, theme::COMMENT);
        // A thin dark edge, so neighbouring comments read apart.
        bp.rect_stroke(r, 1.0, Stroke::new(1.0_f32, theme::COMMENT_EDGE));
        // `<id> <first marker> +N` when it fits, else the id, else no text. A block sharing the
        // band's height (step 6) is too low for the text: none.
        let fit = |s: &str| Some(bp.layout_no_wrap(s.to_owned(), font.clone(), theme::COMMENT_TEXT)).filter(|g| g.size().x + 4.0 <= r.width());
        if let Some(g) = fit(&b.label).or_else(|| fit(&b.short)).filter(|_| b.rows <= 1) {
            bp.galley(pos2(r.left() + 2.0, r.center().y - g.size().y / 2.0), g, theme::COMMENT_TEXT);
        }
    }
    // The selected comment's block: a white edge over the blocks (v0.17.1); 1 pt in a shared
    // row, so the low block still shows its orange.
    for b in blocks.iter().filter(|b| Some(b.id) == comments.picked) {
        let w = if b.rows <= 1 { 2.0_f32 } else { 1.0 };
        bp.rect_stroke(block_rect(b).shrink(w / 4.0), 1.0, Stroke::new(w, theme::COMMENT_PICKED));
    }
    let shown = comments.held.map(|s| ordered_range(s, pb.frame, first, last)).or(*range);
    if let Some((a, b)) = shown {
        let (mut x0, mut x1) = (x_of(a), x_of(b));
        if x1 - x0 < 3.0 {
            // A single frame (or frames closer than a few pixels) stays visible.
            let c = (x0 + x1) / 2.0;
            (x0, x1) = (c - 1.5, c + 1.5);
        }
        let r = Rect::from_min_max(pos2(x0, band.top() + 1.5), pos2(x1, band.bottom() - 1.5));
        bp.rect_filled(r, 0.0, theme::RANGE.gamma_multiply(0.18));
        let outline = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
        // A dark line under the dashes, so the box also shows on top of an orange block.
        bp.rect_stroke(r, 0.0, Stroke::new(2.0_f32, theme::NAV_BG));
        bp.extend(egui::Shape::dashed_line(&outline, Stroke::new(1.0_f32, theme::RANGE), 4.0, 2.0));
    }
    // Blocks take plain clicks and right clicks (added after the band, so they are on top for
    // clicks); a drag still goes to the band below them, which only they do not sense.
    let mut action = None;
    for b in blocks {
        let r = block_rect(b);
        let block_resp = ui.interact(r, ui.id().with(("comment_block", slot, b.id)), Sense::click());
        let block_resp = block_resp.on_hover_ui(|ui| {
            if let Some(c) = comments.list.get(b.id) {
                let frames = if c.start == c.end { format!("프레임 {}", c.start) } else { format!("프레임 {} – {}", c.start, c.end) };
                ui.label(RichText::new(format!("Comment {} · {}", c.id, frames)).strong());
                // Its type (step 5); the band label stays the number (user decision).
                if !c.comment_type.is_empty() {
                    ui.label(format!("Type: {}", c.comment_type));
                }
                let press = if c.markers.is_empty() {
                    ui.label("마커 없는 Comment예요 (프레임 구간만 있어요).");
                    "누르기 = 첫 프레임으로 (고른 마커는 그대로)"
                } else {
                    ui.label(c.markers.join(", "));
                    "누르기 = 첫 프레임으로 + 이 마커 선택"
                };
                // Its note (written in the COMMENTS pane, v0.17.0); the band label stays the number.
                if !c.text.is_empty() {
                    ui.label(RichText::new(&c.text).italics());
                }
                let right = if comments.can_delete {
                    "오른쪽 클릭 = 지우기 (Ctrl+Z = 되돌리기)"
                } else {
                    "이 파일의 Comment는 쓸 수 없어서 지울 수도 없어요 (상태 줄에 마우스를 올려 이유를 보세요)"
                };
                let right = crate::platform::shortcut_text(right);
                ui.label(RichText::new(format!("{press} · 끌기 = 구간 잡기\n{right}")).small().color(theme::TEXT_DIM));
            }
        });
        if block_resp.clicked() {
            action = Some(DeckAction::GotoComment(b.id));
        }
        // Right-clicking deletes a block immediately. Overlapping blocks give the click to the
        // last drawn block on top.
        if block_resp.secondary_clicked() && comments.can_delete {
            action = Some(DeckAction::DeleteComment(b.id));
        }
    }
    if band_resp.dragged() {
        // From the frame under the press to the frame under the pointer; the frame stays.
        if let (Some(from), Some(to)) = (ui.input(|i| i.pointer.press_origin()), band_resp.interact_pointer_pos()) {
            *range = Some(ordered_range(frame_at(from.x), frame_at(to.x), first, last));
        }
    } else if band_resp.clicked() {
        *range = None;
    }
    // Seek zone, drawn as before v0.13.0.
    let p = ui.painter_at(rect);
    // Occlusion heat, one column per pixel.
    let cols = bar.width().max(1.0) as usize;
    for (cx, &v) in heat.get(&data.occlusion, file_id, cols).iter().enumerate() {
        if v > 0.0 {
            let c = theme::lerp(theme::NAV_GAP, theme::NAV_GAP_BRIGHT, v.sqrt());
            let x = bar.left() + cx as f32;
            p.line_segment([pos2(x, rect.top() + 12.0), pos2(x, rect.bottom() - 9.0)], Stroke::new(1.0_f32, c.gamma_multiply(0.35 + 0.65 * v.sqrt())));
        }
    }
    if let Some(m) = primary.and_then(|m| data.markers.get(m)) {
        for &(a, b) in &m.gaps {
            let r = Rect::from_min_max(pos2(x_of(a) - 0.5, rect.bottom() - 7.0), pos2(x_of(b) + 0.5, rect.bottom() - 2.0));
            p.rect_filled(r, 0.0, theme::NAV_GAP_BRIGHT);
        }
    }
    // Played part in green (Motive Edit-mode timeline).
    p.rect_filled(Rect::from_min_max(pos2(bar.left(), rect.top() + 1.0), pos2(x_of(pb.frame), rect.top() + 4.0)), 0.0, theme::TIMELINE_GREEN);
    // Ticks.
    let total = pb.last - pb.first;
    let target = (bar.width() / 90.0).max(2.0) as i64;
    let raw = (total / target).max(1);
    let mag = 10i64.pow((raw as f64).log10().floor().max(0.0) as u32);
    let step = [1, 2, 5, 10].iter().map(|k| k * mag).find(|s| *s >= raw).unwrap_or(10 * mag);
    let mut f = (pb.first + step - 1).div_euclid(step) * step;
    while f <= pb.last {
        let x = x_of(f);
        p.line_segment([pos2(x, rect.top() + 4.0), pos2(x, rect.top() + 8.0)], Stroke::new(1.0_f32, theme::TEXT_DIM));
        p.text(pos2(x + 2.0, rect.top() + 5.0), Align2::LEFT_TOP, f.to_string(), FontId::monospace(10.0), theme::TEXT_DIM);
        f += step;
    }
    // The current-frame line covers the seek zone only.
    let cx = x_of(pb.frame);
    p.line_segment([pos2(cx, rect.top()), pos2(cx, rect.bottom())], Stroke::new(2.0_f32, theme::TIMELINE_GREEN));
    DeckOut { action }
}

/// `Comment` button at the right end of a wide bar. True when pressed.
fn comment_button(ui: &mut Ui, make: &Result<String, String>) -> bool {
    let button = egui::Button::new(RichText::new("Comment").size(13.0)).shortcut_text("M").min_size(vec2(30.0, 24.0));
    let resp = ui.add_enabled(make.is_ok(), button);
    let resp = match make {
        Ok(tip) => resp.on_hover_text(tip),
        Err(why) => resp.on_disabled_hover_text(why),
    };
    pressed_even_dim(ui, &resp)
}

/// A click on the `Comment` button, also when it is dim: the app then says why nothing happens
/// (a TAK takes no Comment, `.takm` step ③) and does nothing else.
fn pressed_even_dim(ui: &Ui, resp: &egui::Response) -> bool {
    resp.clicked() || (!resp.enabled() && resp.contains_pointer() && ui.input(|i| i.pointer.primary_clicked()))
}

pub const SPEEDS: [f64; 7] = [0.1, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0];

fn btn(ui: &mut Ui, text: &str, tip: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(text).size(15.0)).min_size(vec2(30.0, 24.0))).on_hover_text(tip).clicked()
}

/// −1 · play / pause · +1 (both bar widths).
fn step_buttons(ui: &mut Ui, pb: &mut Playback, now: f64) {
    if btn(ui, "◀", "−1 프레임 (←)") {
        pb.step(-1, now);
    }
    let play = if pb.playing { "⏸" } else { "▶" };
    if btn(ui, play, "재생/정지 (Space)") {
        pb.toggle(now);
    }
    if btn(ui, "▶|", "+1 프레임 (→)") {
        pb.step(1, now);
    }
}

/// Hover text of the playback bar's frame box (v0.17.3).
pub const FRAME_BOX_TIP: &str = "끌기 = 프레임 옮기기: 위 · 오른쪽 = 뒤 프레임, 아래 · 왼쪽 = 앞 프레임\n1 pt = 1프레임, Shift를 누른 채 = 10프레임 · 그냥 누르기 = 숫자 쓰기 (Enter)";

/// The widget id of view `slot`'s frame box (typing and dragging).
pub fn frame_box_id(slot: usize) -> Id {
    Id::new(("deck_frame_box", slot))
}

/// Frame number box and "/ last". A click types a number in it (Enter or leaving the box = go
/// there, Esc = never mind), a drag moves the frame ("hot text", v0.17.3, `scrub`: up or right =
/// later, 1 pt = 1 frame, Shift = 10; a playing view pauses when the drag starts). egui's
/// `DragValue` was here before: its Shift drags slower and it rounds to "nice" numbers.
fn frame_field(ui: &mut Ui, pb: &mut Playback, slot: usize, now: f64) {
    let id = frame_box_id(slot);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let pad = ui.spacing().button_padding;
    // As wide as the last frame's number, so the box does not change size while it moves.
    let digits = ui.fonts(|f| f.layout_no_wrap(pb.last.max(pb.first.abs()).to_string(), font.clone(), egui::Color32::WHITE).size().x);
    let size = vec2((digits + 2.0 * pad.x + 4.0).max(ui.spacing().interact_size.x), ui.spacing().interact_size.y);
    if ui.memory(|m| m.has_focus(id)) {
        let mut text = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| pb.frame.to_string());
        let resp = ui.add_sized(size, egui::TextEdit::singleline(&mut text).id(id).font(font).margin(pad).horizontal_align(egui::Align::Center));
        if resp.lost_focus() && !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Ok(f) = text.trim().parse::<i64>() {
                pb.set_frame(f, now);
            }
        }
        ui.data_mut(|d| d.insert_temp(id, text));
    } else {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let sense = Sense { click: true, drag: true, focusable: false };
        let resp = ui.interact(rect, id.with("hot"), sense).on_hover_cursor(super::scrub::CURSOR).on_hover_text(FRAME_BOX_TIP);
        let look = ui.style().interact(&resp);
        ui.painter().rect(rect.expand(look.expansion), look.rounding, look.weak_bg_fill, look.bg_stroke);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, pb.frame.to_string(), font, look.text_color());
        if resp.clicked() {
            // Typing: the number in the box, all of it selected (typing replaces it).
            let text = pb.frame.to_string();
            let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
            state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()))));
            state.store(ui.ctx(), id);
            ui.data_mut(|d| d.insert_temp(id, text));
            ui.memory_mut(|m| m.request_focus(id));
        }
        if resp.drag_started_by(egui::PointerButton::Primary) {
            pb.playing = false;
        }
        if let Some(f) = super::scrub::drag(ui, &resp, pb.frame, pb.first, pb.last) {
            pb.playing = false;
            pb.set_frame(f, now);
        }
    }
    ui.label(RichText::new(format!("/ {}", pb.last)).color(theme::TEXT_DIM));
}

fn speed_box(ui: &mut Ui, pb: &mut Playback, slot: usize, now: f64) {
    let mut speed = pb.speed;
    egui::ComboBox::from_id_salt(("speed", slot)).width(52.0).selected_text(format!("{speed}x")).show_ui(ui, |ui| {
        for s in SPEEDS {
            ui.selectable_value(&mut speed, s, format!("{s}x"));
        }
    });
    if speed != pb.speed {
        pb.set_speed(speed, now);
    }
}

/// Wide playback bar: all buttons fit in the row, and playback always loops.
fn transport(ui: &mut Ui, pb: &mut Playback, slot: usize, now: f64) {
    if btn(ui, "⏮", "처음 (Home)") {
        pb.step(i64::MIN / 2, now);
    }
    if btn(ui, "⏪", "−10 프레임 (↓)") {
        pb.step(-10, now);
    }
    step_buttons(ui, pb, now);
    if btn(ui, "⏩", "+10 프레임 (↑)") {
        pb.step(10, now);
    }
    if btn(ui, "⏭", "끝 (End)") {
        pb.step(i64::MAX / 2, now);
    }
    ui.separator();
    ui.label(RichText::new("프레임").color(theme::TEXT_DIM));
    frame_field(ui, pb, slot, now);
    ui.separator();
    ui.label(RichText::new(format!("{:.3} s", pb.time_of(pb.frame))).monospace());
    ui.separator();
    speed_box(ui, pb, slot, now);
}

/// Narrow bar: −1 · play · +1 · frame on the left, speed and ⋯ (처음 · −10 · +10 · 끝 · time ·
/// Comment) on the right. True when `Comment` was chosen in the ⋯ menu.
fn transport_narrow(ui: &mut Ui, pb: &mut Playback, slot: usize, now: f64, make: &Result<String, String>) -> bool {
    step_buttons(ui, pb, now);
    frame_field(ui, pb, slot, now);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let made = more_menu(ui, pb, now, make);
        speed_box(ui, pb, slot, now);
        made
    })
    .inner
}

/// ⋯ button (three painted dots: the font may not have "⋯") and its menu. True when its
/// `Comment` item was chosen.
fn more_menu(ui: &mut Ui, pb: &mut Playback, now: f64, make: &Result<String, String>) -> bool {
    let button = egui::Button::new("").min_size(vec2(30.0, 24.0));
    let mut made = false;
    let r = egui::menu::menu_custom_button(ui, button, |ui| {
        let item = |ui: &mut Ui, text: &str, key: &str| ui.add(egui::Button::new(text).shortcut_text(key)).clicked();
        let mut step = None;
        if item(ui, "⏮  처음", "Home") {
            step = Some(i64::MIN / 2);
        }
        if item(ui, "⏪  −10 프레임", "↓") {
            step = Some(-10);
        }
        if item(ui, "⏩  +10 프레임", "↑") {
            step = Some(10);
        }
        if item(ui, "⏭  끝", "End") {
            step = Some(i64::MAX / 2);
        }
        ui.separator();
        ui.label(RichText::new(format!("지금 시간  {:.3} s", pb.time_of(pb.frame))).color(theme::TEXT_DIM));
        ui.separator();
        let comment = ui.add_enabled(make.is_ok(), egui::Button::new("Comment").shortcut_text("M"));
        let comment = match make {
            Ok(tip) => comment.on_hover_text(tip),
            Err(why) => comment.on_disabled_hover_text(why),
        };
        if pressed_even_dim(ui, &comment) {
            made = true;
            ui.close_menu();
        }
        if let Some(d) = step {
            pb.step(d, now);
            ui.close_menu();
        }
    });
    let c = r.response.rect.center();
    let color = if ui.is_enabled() { theme::TEXT } else { theme::TEXT_DIM };
    for dx in [-5.0, 0.0, 5.0] {
        ui.painter().circle_filled(c + vec2(dx, 0.0), 1.6, color);
    }
    r.response.on_hover_text("처음 · 끝 · ±10 프레임 · 지금 시간 · Comment");
    made
}

#[cfg(test)]
mod lane_tests {
    use super::*;

    /// Letters of lanes, top to bottom.
    fn letters(lanes: &[Lane]) -> Vec<&str> {
        lanes.iter().map(|l| l.letter.as_str()).collect()
    }

    #[test]
    fn plate_lanes_are_fx_fy_fz_first_from_channels_0_to_2_and_each_is_built_once() {
        let d = crate::views::sample_with_plates("a.tak", &["M"]);
        assert_eq!(plate_offer(&d, 0), LaneSet::first(6), "P1's device has six channels");
        let mut cache = PlateCache::default();
        let (lanes, gaps, _) = cache.get(&d, 7, 0, LaneSet::default());
        assert_eq!(letters(lanes), ["Fx", "Fy", "Fz"]);
        let colors: Vec<_> = lanes.iter().map(|l| l.color).collect();
        assert_eq!(colors, [theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z], "x red, y green, z blue (Fz blue now)");
        assert!(lanes.iter().all(|l| l.unit.is_empty()), "no unit: units are not verified");
        // Channel k's values: Fx = channel 0 (= frame), Fy = channel 1 (1000 + frame), Fz =
        // channel 2 with its gap.
        assert_eq!(lanes[0].runs, vec![(0..10).map(|f| [f as f64, f as f64]).collect::<Vec<_>>()]);
        assert_eq!(lanes[1].runs, vec![(0..10).map(|f| [f as f64, 1000.0 + f as f64]).collect::<Vec<_>>()]);
        assert_eq!(lanes[2].runs, vec![vec![[0.0, 100.0], [1.0, 110.0], [2.0, 120.0], [3.0, 130.0]], (6..10).map(|f| [f as f64, 100.0 + 10.0 * f as f64]).collect::<Vec<_>>()]);
        assert_eq!(gaps, &[(4, 5)], "frames without Fz");
        // The next frames use what is kept.
        for _ in 0..5 {
            cache.get(&d, 7, 0, LaneSet::default());
        }
        assert_eq!(cache.builds, 3, "one build per lane");
        // Mx more: only Mx is built; it goes below Fz (channel order); its gap joins.
        let with_mx = LaneSet(0b001_111);
        let (lanes, gaps, _) = cache.get(&d, 7, 0, with_mx);
        assert_eq!(letters(lanes), ["Fx", "Fy", "Fz", "Mx"]);
        assert_eq!(lanes[3].runs[0][0], [0.0, 3000.0], "Mx = channel 3");
        assert_eq!(gaps, &[(4, 5), (8, 8)]);
        assert_eq!(cache.builds, 4, "the other lanes are not built again");
        // Fy away and back again: kept, not built again.
        let (lanes, _, _) = cache.get(&d, 7, 0, LaneSet(0b001_101));
        assert_eq!(letters(lanes), ["Fx", "Fz", "Mx"]);
        let (lanes, _, _) = cache.get(&d, 7, 0, with_mx);
        assert_eq!(letters(lanes), ["Fx", "Fy", "Fz", "Mx"]);
        assert_eq!(lanes[1].runs[0][0], [0.0, 1000.0]);
        let (lanes, _, _) = cache.get(&d, 7, 0, LaneSet::first(6));
        assert_eq!(letters(lanes), ["Fx", "Fy", "Fz", "Mx", "My", "Mz"]);
        assert_eq!(cache.builds, 6);
        for _ in 0..5 {
            cache.get(&d, 7, 0, LaneSet::first(6));
        }
        assert_eq!(cache.builds, 6);
        // Another plate, another file: built again (three lanes each).
        cache.get(&d, 7, 1, LaneSet::default());
        assert_eq!(cache.builds, 9, "another plate");
        cache.get(&d, 8, 1, LaneSet::default());
        assert_eq!(cache.builds, 12, "another file");
        // Values missing up to the end: one gap to the last frame.
        assert_eq!(runs_and_gaps(&[1.0, f32::NAN, f32::NAN], 10, 12), (vec![vec![[10.0, 1.0]]], vec![(11, 12)]));
        assert_eq!(merged([&[(1, 3), (8, 9)][..], &[(2, 5), (6, 6), (12, 12)][..]].into_iter()), vec![(1, 6), (8, 9), (12, 12)]);
    }

    #[test]
    fn a_plate_device_with_fewer_channels_offers_fewer_lanes_and_no_device_offers_fz() {
        let mut d = crate::views::sample_with_plates("a.tak", &["M"]);
        assert_eq!(plate_offer(&d, 1), LaneSet::first(3), "P2's device has three channels: Fx Fy Fz, no moments");
        let wanted = LaneSet(0b001_111);
        let mut cache = PlateCache::default();
        let (lanes, _, _) = cache.get(&d, 7, 1, wanted.shown(plate_offer(&d, 1)));
        assert_eq!(letters(lanes), ["Fx", "Fy", "Fz"]);
        // A plate without its device (cannot happen with today's files): Fz from the plate.
        d.devices.clear();
        assert_eq!(plate_offer(&d, 0), LaneSet::only(FZ_CHANNEL));
        let (lanes, gaps, _) = cache.get(&d, 9, 0, wanted.shown(plate_offer(&d, 0)));
        assert_eq!(letters(lanes), ["Fz"]);
        assert_eq!((lanes[0].runs[0][0], gaps), ([0.0, 100.0], &[(4, 5)][..]));
    }
}

#[cfg(test)]
mod deck_tests {
    use super::*;

    #[test]
    fn bar_is_wide_only_when_every_button_fits() {
        assert!(deck_is_wide(1000.0));
        assert!(deck_is_wide(WIDE_DECK_MIN));
        assert!(!deck_is_wide(WIDE_DECK_MIN - 1.0));
        assert!(!deck_is_wide(350.0), "a view of a 2×2 layout gets the narrow bar");
    }

    #[test]
    fn frames_and_x_match_at_both_ends_of_the_timeline() {
        let (first, last, left, width) = (100, 1100, 50.0, 500.0);
        let x = |f| x_of_frame(f, first, last, left, width);
        let f = |x| frame_at_x(x, first, last, left, width);
        assert_eq!((x(100), x(1100)), (50.0, 550.0), "first frame at the left end, last at the right end");
        assert_eq!((f(50.0), f(550.0)), (100, 1100));
        assert_eq!((f(0.0), f(900.0)), (100, 1100), "outside the bar: first / last frame");
        for k in [100, 101, 600, 1099, 1100] {
            assert_eq!(f(x(k)), k, "frame {k} back from its x");
        }
        // A one-frame file never gives a frame outside it.
        assert_eq!(frame_at_x(999.0, 5, 5, left, width), 5);
        // Ranges: ascending, inside the file.
        assert_eq!(ordered_range(900, 300, 0, 1000), (300, 900));
        assert_eq!(ordered_range(-5, 2000, 0, 1000), (0, 1000));
        assert_eq!(ordered_range(7, 7, 0, 10), (7, 7));
        assert_eq!(ordered_range(20, 30, 0, 10), (10, 10));
        // The bar: 28 pt seek zone + 12 pt band strip in one row.
        assert_eq!((SEEK_H, BAND_H, TIMELINE_H), (28.0, 12.0, 40.0));
        assert_eq!(DECK_H, 75.0);
    }

    #[test]
    fn red_marks_are_made_once_per_file_and_width() {
        let occlusion: Vec<f32> = (0..10_000).map(|i| if i % 997 == 0 { 0.5 } else { 0.0 }).collect();
        let mut cache = HeatCache::default();
        let first = cache.get(&occlusion, 7, 300).to_vec();
        assert_eq!(first, heat_columns(&occlusion, 300));
        assert_eq!(first.len(), 300);
        // Same file and width: the kept columns (a different slice would be made again).
        let other: Vec<f32> = vec![1.0; 10_000];
        assert_eq!(cache.get(&other, 7, 300), &first[..]);
        // New width or new file: made again.
        assert_eq!(cache.get(&other, 7, 200), &vec![1.0; 200][..]);
        assert_eq!(cache.get(&occlusion, 8, 200), &heat_columns(&occlusion, 200)[..]);
    }

    fn comment(kind: tak_core::comments::Kind, id: u64, markers: &[&str], start: i64, end: i64) -> tak_core::comments::Event {
        tak_core::comments::Event {
            time: String::new(),
            kind,
            id,
            markers: markers.iter().map(|m| m.to_string()).collect(),
            start,
            end,
            comment_type: String::new(),
            text: String::new(),
            shown_a: "1".into(),
            shown_b: String::new(),
            file_a: String::new(),
            file_b: String::new(),
        }
    }

    #[test]
    fn comment_blocks_span_their_frames_keep_a_minimum_width_and_are_laid_out_only_on_change() {
        use tak_core::comments::Kind::{Add, Delete};
        let mut list = Comments::from_events(&[comment(Add, 1, &["LKNE"], 100, 300), comment(Add, 2, &["RKNE", "LASI", "RASI"], 500, 500), comment(Add, 3, &[], 900, 1000)]);
        let mut cache = BlockCache::default();
        let b = cache.get(&list, 7, 0, 1000, 10.0, 1000.0).to_vec();
        assert_eq!(b.len(), 3);
        assert_eq!((b[0].x0, b[0].x1), (110.0, 310.0), "from the first frame's x to the last frame's x");
        assert_eq!(b[1].x1 - b[1].x0, BLOCK_MIN_W, "a one-frame comment is still 4 pt wide");
        assert_eq!((b[1].x0 + b[1].x1) / 2.0, 510.0, "around its frame");
        assert_eq!((b[0].label.as_str(), b[1].label.as_str(), b[2].label.as_str()), ("1 LKNE", "2 RKNE +2", "3"));
        assert_eq!(b[1].short, "2");
        assert_eq!(cache.builds, 1);
        // Same file, list, place and width: kept (no work that grows with the file).
        for _ in 0..5 {
            cache.get(&list, 7, 0, 1000, 10.0, 1000.0);
        }
        assert_eq!(cache.builds, 1);
        // Each change lays them out again: width, place, file, the list.
        cache.get(&list, 7, 0, 1000, 10.0, 500.0);
        cache.get(&list, 7, 0, 1000, 20.0, 500.0);
        cache.get(&list, 8, 0, 1000, 20.0, 500.0);
        assert_eq!(cache.builds, 4);
        list.apply(&comment(Delete, 2, &[], 0, 0));
        let b = cache.get(&list, 8, 0, 1000, 20.0, 500.0);
        assert_eq!(b.iter().map(|b| b.id).collect::<Vec<_>>(), vec![1, 3]);
        assert_eq!(cache.builds, 5);
    }

    /// (row, rows) of each block of comments (id, start, end) on frames 0–1000 over 1000 pt.
    fn rows_of(list: &[(u64, i64, i64)]) -> Vec<(u64, u8, u8)> {
        use tak_core::comments::Kind::Add;
        let list = Comments::from_events(&list.iter().map(|&(id, s, e)| comment(Add, id, &["M"], s, e)).collect::<Vec<_>>());
        blocks(&list, 0, 1000, 0.0, 1000.0).iter().map(|b| (b.id, b.row, b.rows)).collect()
    }

    #[test]
    fn blocks_that_overlap_nothing_keep_the_full_height_and_the_same_place() {
        use tak_core::comments::Kind::Add;
        // Apart, touching end to start (frames 300 / 301), and a one-frame comment far away.
        let list = Comments::from_events(&[comment(Add, 1, &["LKNE"], 100, 300), comment(Add, 2, &[], 301, 400), comment(Add, 3, &["N"], 700, 700)]);
        let b = blocks(&list, 0, 1000, 10.0, 1000.0);
        assert!(b.iter().all(|b| (b.row, b.rows) == (0, 1)), "{b:?}");
        // The same x spans and texts as before rows (step 6).
        let flat = blocks_flat(&list, 0, 1000, 10.0, 1000.0);
        assert_eq!(b, flat);
        // The same rect as before: 1 pt in from the band's top and bottom.
        let band = Rect::from_min_size(pos2(0.0, 50.0), vec2(1020.0, BAND_H));
        for b in &b {
            assert_eq!(block_row_rect(band, b.x0, b.x1, b.row, b.rows), Rect::from_min_max(pos2(b.x0, band.top() + 1.0), pos2(b.x1, band.bottom() - 1.0)));
        }
    }

    #[test]
    fn overlapping_blocks_get_rows_by_start_frame_then_id() {
        // Two overlapping: two rows, the earlier start on top.
        assert_eq!(rows_of(&[(1, 300, 500), (2, 100, 400)]), vec![(1, 1, 2), (2, 0, 2)]);
        // Three overlapping at one place: three rows.
        assert_eq!(rows_of(&[(1, 100, 400), (2, 200, 500), (3, 300, 600)]), vec![(1, 0, 3), (2, 1, 3), (3, 2, 3)]);
        // One inside another: two rows, the outer one (earlier start) on top.
        assert_eq!(rows_of(&[(1, 200, 250), (2, 100, 600)]), vec![(1, 1, 2), (2, 0, 2)]);
        // The same start frame: by id (the list is in id order).
        assert_eq!(rows_of(&[(5, 100, 200), (4, 100, 300)]), vec![(4, 0, 2), (5, 1, 2)]);
        assert_eq!(rows_of(&[(4, 100, 200), (5, 100, 300)]), vec![(4, 0, 2), (5, 1, 2)]);
        // A chain: 1 overlaps 2, 2 overlaps 3, 1 and 3 do not: 3 goes back to row 0, two rows
        // for the whole chain, one height for each block's whole length.
        assert_eq!(rows_of(&[(1, 100, 300), (2, 250, 500), (3, 400, 600)]), vec![(1, 0, 2), (2, 1, 2), (3, 0, 2)]);
        // Four at one place: three rows, the fourth shares the last one.
        assert_eq!(rows_of(&[(1, 100, 400), (2, 110, 400), (3, 120, 400), (4, 130, 400)]), vec![(1, 0, 3), (2, 1, 3), (3, 2, 3), (4, 2, 3)]);
        // Two one-frame comments a frame apart: their 4 pt blocks overlap, so two rows.
        assert_eq!(rows_of(&[(1, 500, 500), (2, 501, 501)]), vec![(1, 0, 2), (2, 1, 2)]);
        // Rows split the band's 10 pt (1 pt in from each edge) without gaps or overlaps.
        let band = Rect::from_min_size(pos2(0.0, 50.0), vec2(1000.0, BAND_H));
        let r: Vec<Rect> = (0..3).map(|k| block_row_rect(band, 0.0, 10.0, k, 3)).collect();
        assert_eq!((r[0].top(), r[2].bottom()), (51.0, 61.0));
        assert!((r[0].bottom() - r[1].top()).abs() < 1e-4 && (r[1].bottom() - r[2].top()).abs() < 1e-4);
        assert!((r[1].height() - 10.0 / 3.0).abs() < 1e-4);
        let half = block_row_rect(band, 0.0, 10.0, 1, 2);
        assert_eq!((half.top(), half.bottom()), (56.0, 61.0));
    }

    #[test]
    fn rows_stay_when_an_unrelated_comment_is_added_and_a_later_one_keeps_the_rows_above() {
        let before = rows_of(&[(1, 100, 400), (2, 200, 500), (3, 800, 900)]);
        // A new comment far from 1 and 2 (it overlaps only 3): 1 and 2 as they were.
        let after = rows_of(&[(1, 100, 400), (2, 200, 500), (3, 800, 900), (4, 850, 950)]);
        assert_eq!(&after[..2], &before[..2]);
        assert_eq!((after[2], after[3]), ((3, 0, 2), (4, 1, 2)));
        // A new one in the group that starts later: the rows above stay where they were (only the
        // height is shared by one more row).
        let more = rows_of(&[(1, 100, 400), (2, 200, 500), (3, 800, 900), (5, 300, 350)]);
        assert_eq!(more.iter().map(|r| (r.0, r.1)).collect::<Vec<_>>(), vec![(1, 0), (2, 1), (3, 0), (5, 2)]);
    }

    #[test]
    fn two_thousand_overlapping_comments_are_laid_out_once_and_quickly() {
        use tak_core::comments::Kind::Add;
        let ev: Vec<_> = (1..=2000u64).map(|id| comment(Add, id, &["M"], (id as i64 * 37) % 4000, (id as i64 * 37) % 4000 + 300)).collect();
        let list = Comments::from_events(&ev);
        let mut cache = BlockCache::default();
        let t = std::time::Instant::now();
        let b = cache.get(&list, 1, 0, 4767, 12.0, 1500.0).to_vec();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(b.len(), 2000);
        assert!(b.iter().all(|b| b.row < BAND_ROWS && b.rows <= BAND_ROWS && b.row < b.rows));
        assert!(ms < 200.0, "laying out 2000 comments took {ms:.1} ms");
        // Then kept: the next frames do no work.
        let t = std::time::Instant::now();
        for _ in 0..100 {
            cache.get(&list, 1, 0, 4767, 12.0, 1500.0);
        }
        assert_eq!(cache.builds, 1);
        assert!(t.elapsed().as_secs_f64() * 1000.0 < 5.0);
    }

    #[test]
    fn graph_bands_are_the_comments_naming_that_marker_and_follow_the_list() {
        use tak_core::comments::Kind::{Add, Delete, Edit};
        let mut list = Comments::from_events(&[
            comment(Add, 1, &["LKNE", "RKNE"], 100, 300),
            comment(Add, 2, &[], 50, 60),
            comment(Add, 3, &["RKNE"], 500, 500),
            comment(Add, 4, &["LKNE"], 700, 720),
        ]);
        let mut cache = SpanCache::default();
        let span = |start, end, picked| graph::Span { start, end, picked };
        assert_eq!(cache.get(&list, 7, "LKNE", None), &[span(100, 300, false), span(700, 720, false)][..], "only those naming LKNE; none for the marker-less one");
        assert_eq!(cache.get(&list, 7, "RKNE", Some(3)), &[span(100, 300, false), span(500, 500, true)][..], "the selected one marked");
        assert!(cache.get(&list, 7, "LASI", Some(3)).is_empty(), "no Comment names it: no bands");
        assert_eq!(cache.builds, 3);
        // Same file, marker, list and selection: kept.
        for _ in 0..5 {
            cache.get(&list, 7, "LASI", Some(3));
        }
        assert_eq!(cache.builds, 3);
        // Add, edit, delete (and undo, which is another add / delete line): at once.
        list.apply(&comment(Add, 5, &["LKNE"], 900, 910));
        assert_eq!(cache.get(&list, 7, "LKNE", None).len(), 3);
        list.apply(&comment(Edit, 4, &["LKNE"], 710, 711));
        assert_eq!(cache.get(&list, 7, "LKNE", None)[1], span(710, 711, false));
        list.apply(&comment(Edit, 4, &["RKNE"], 710, 711));
        assert_eq!(cache.get(&list, 7, "LKNE", None).len(), 2, "its marker taken away: gone from LKNE's graph");
        list.apply(&comment(Delete, 1, &[], 0, 0));
        assert_eq!(cache.get(&list, 7, "LKNE", None), &[span(900, 910, false)][..]);
        list.apply(&comment(Add, 1, &["LKNE", "RKNE"], 100, 300));
        assert_eq!(cache.get(&list, 7, "LKNE", None).len(), 2, "back after an undo of the delete");
        // Another file with the same list revision: made again.
        let builds = cache.builds;
        cache.get(&list, 8, "LKNE", None);
        assert_eq!(cache.builds, builds + 1);
    }

    #[test]
    fn red_mark_columns_keep_every_missing_frame() {
        // Each frame falls in some column, so no gap disappears however narrow the bar is.
        let mut occlusion = vec![0.0f32; 5000];
        for f in [0, 1234, 4999] {
            occlusion[f] = 1.0;
        }
        for cols in [37, 300, 4999, 8000] {
            let c = heat_columns(&occlusion, cols);
            assert_eq!(c.len(), cols);
            assert!(c.iter().filter(|&&v| v > 0.0).count() >= 3.min(cols), "{cols} columns");
            assert_eq!(c[0], 1.0);
            assert_eq!(c[cols - 1], 1.0);
        }
        assert!(heat_columns(&[], 10).iter().all(|&v| v == 0.0));
    }

    /// v0.27.0: a file with markers `names` (frames 0–9, marker k at x = k · 100 mm), LABELS
    /// order reversed against the marker numbers (so the order is not just the numbers).
    fn graph_data(names: &[&str]) -> ViewData {
        let mut d = ViewData::build(crate::views::sample_take("g.tak", names));
        for g in &mut d.groups {
            g.members.reverse();
        }
        d
    }

    #[test]
    fn graphed_markers_are_the_selected_in_labels_order_at_most_eight_and_named_without_colors() {
        let names: Vec<String> = (0..12).map(|k| format!("M{k}")).collect();
        let d = graph_data(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let labels: Vec<usize> = d.groups.iter().flat_map(|g| g.members.iter().copied()).collect();
        let sel = |ms: &[usize]| ms.iter().copied().collect::<BTreeSet<usize>>();
        // One marker: itself.
        assert_eq!(graphed(&d, &sel(&[4]), 4), (vec![4], 1));
        assert!(graph_keys(&d, &[4], 0).is_empty(), "one marker: no legend, its tag as before");
        // Several: in LABELS order whatever was clicked last.
        let (two, n) = graphed(&d, &sel(&[3, 7]), 3);
        assert_eq!((two.clone(), n), (vec![7, 3], 2));
        assert_eq!(graphed(&d, &sel(&[3, 7]), 7).0, two, "the primary does not change the order");
        // Adding one later in the LABELS order keeps the order of those before; taking it
        // away gives the same as before. v0.29.0: the names only (no colour per marker), tracked
        // or not as the one marker's name.
        let (three, _) = graphed(&d, &sel(&[3, 7, 1]), 1);
        assert_eq!(three, vec![7, 3, 1]);
        let keys = graph_keys(&d, &three, 0);
        assert_eq!(keys.iter().map(|k| k.name).collect::<Vec<_>>().join(&format!(" {NAME_SEP} ")), "M7 · M3 · M1");
        assert!(keys.iter().all(|k| k.name_color == theme::LABEL_TRACKED || k.name_color == theme::LABEL_UNTRACKED));
        assert_eq!(graph_keys(&d, &two, 0)[..], keys[..2]);
        // The primary counts even when it is not in the set.
        assert_eq!(graphed(&d, &sel(&[3]), 7), (vec![7, 3], 2));
        // All twelve: the first eight in LABELS order, and the count of all.
        let all = sel(&(0..12).collect::<Vec<_>>());
        assert_eq!(graphed(&d, &all, 0), (labels[..GRAPH_MAX].to_vec(), 12));
        assert_eq!(left_out_text(8, 12), "선택 12개 중 8개");
    }

    #[test]
    fn lanes_of_several_markers_are_built_once_per_selection_in_the_lane_colors() {
        let d = graph_data(&["A", "B", "C"]);
        let mut cache = LaneCache::default();
        // One marker: X red, Y green, Z blue, its own gaps.
        let (lanes, gaps, _) = cache.get(&d, 1, &[0]);
        assert_eq!(lanes.iter().map(|l| l.color).collect::<Vec<_>>(), vec![theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z]);
        assert_eq!(gaps, &d.markers[0].gaps[..]);
        for _ in 0..5 {
            cache.get(&d, 1, &[0]);
        }
        assert_eq!(cache.builds, 1);
        // Multiple markers share axis colors: X red, Y green, and Z blue.
        let (lanes, _, _) = cache.get(&d, 1, &[2, 1, 0]);
        assert_eq!(lanes.len(), 3);
        assert_eq!(lanes.iter().map(|l| l.color).collect::<Vec<_>>(), vec![theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z]);
        assert_eq!(lanes.iter().map(|l| l.letter.as_str()).collect::<Vec<_>>(), ["X", "Y", "Z"], "the lane letters stay");
        for _ in 0..5 {
            cache.get(&d, 1, &[2, 1, 0]);
        }
        assert_eq!(cache.builds, 2, "made again only for other markers");
        cache.get(&d, 1, &[2, 1]);
        cache.get(&d, 2, &[2, 1]);
        assert_eq!(cache.builds, 4, "and for another file");
    }

    /// `names` as markers of one marker set (labeled: a file laid over has its markers of the
    /// same names, `same_name`).
    fn labeled_data(names: &[&str]) -> ViewData {
        use tak_core::tak::{MarkerKind, SkeletonAsset};
        let asset = [9u8; 16];
        let mut t = crate::views::sample_take("g.tak", names);
        t.assets = vec![SkeletonAsset { uid: asset, name: "S".into(), markerset_name: None, model: None, sticks: vec![], marker_colors: vec![] }];
        for (k, tr) in t.trajectories.iter_mut().enumerate() {
            (tr.kind, tr.parent_uid, tr.asset_uid, tr.slot) = (MarkerKind::Template, Some(asset), Some(asset), Some(k as i32 + 1));
        }
        ViewData::build(t)
    }

    #[test]
    fn lanes_of_several_markers_come_a_few_per_frame_within_the_frames_work() {
        use crate::ui::graph;
        let names = ["A", "B", "C", "D", "E", "F", "G", "H"];
        let (d, other) = (labeled_data(&names), labeled_data(&names));
        assert!((0..8).all(|m| same_name(&d, m, &other) == Some(m)));
        let markers: Vec<usize> = (0..8).collect();
        let one = marker_cost(&d, 0);
        assert!(one > 0 && markers.iter().all(|&m| marker_cost(&d, m) == one));
        let over = Some((Path::new("b.tak"), &other));
        // A budget of two markers a frame: the first marker's lanes and dashed lanes at once,
        // then at most two more markers' (solid first, then dashed) in each frame.
        let mut cache = LaneCache::default();
        let mut seen = vec![(0, 0)];
        for frame in 1..=10u64 {
            graph::start_frame(frame, Some(2 * one));
            cache.pair(&d, 1, &markers, over, [true, true]);
            cache.get(&d, 1, &markers);
            graph::end_frame();
            seen.push(cache.built());
        }
        assert_eq!(&seen[..10], &[(0, 0), (2, 1), (4, 1), (6, 1), (8, 1), (8, 3), (8, 5), (8, 7), (8, 8), (8, 8)]);
        assert!(!cache.pending());
        assert_eq!((cache.builds, cache.pair_builds), (1, 1), "one selection, one file laid over");
        let (lanes, _, drawn) = cache.get(&d, 1, &markers);
        assert_eq!((lanes.len(), drawn.more_len()), (3, 7));
        // Without a budget (outside the app's frame): all at once, as before.
        let mut at_once = LaneCache::default();
        at_once.pair(&d, 1, &markers, over, [true, true]);
        assert_eq!((at_once.built(), at_once.pending()), ((8, 8), false));
        // Another selection starts again from its first marker.
        graph::start_frame(11, Some(2 * one));
        cache.pair(&d, 1, &markers[..5], over, [true, true]);
        graph::end_frame();
        assert_eq!((cache.built(), cache.pending()), ((2, 1), true));
    }

    #[test]
    fn graph_bands_of_several_markers_are_each_comment_naming_any_of_them_once() {
        use tak_core::comments::Kind::Add;
        let list = Comments::from_events(&[
            comment(Add, 1, &["LKNE", "RKNE"], 100, 300),
            comment(Add, 2, &[], 50, 60),
            comment(Add, 3, &["RKNE"], 500, 500),
            comment(Add, 4, &["LASI"], 700, 720),
            comment(Add, 5, &["RASI"], 800, 820),
        ]);
        let mut cache = SpanCache::default();
        let span = |start, end, picked| graph::Span { start, end, picked };
        let got = cache.get_many(&list, 7, &["LKNE", "RKNE", "LASI"], Some(3)).to_vec();
        assert_eq!(got, vec![span(100, 300, false), span(500, 500, true), span(700, 720, false)], "Comment 1 names two of them: one band");
        for _ in 0..5 {
            cache.get_many(&list, 7, &["LKNE", "RKNE", "LASI"], Some(3));
        }
        assert_eq!(cache.builds, 1);
        assert_eq!(cache.get_many(&list, 7, &["LKNE", "RKNE"], Some(3)).len(), 2, "another selection: made again");
        assert_eq!(cache.get(&list, 7, "LKNE", None), &[span(100, 300, false)][..], "one marker as before");
        assert!(cache.get_many(&list, 7, &[], None).is_empty());
        assert_eq!(cache.builds, 4);
    }
}
