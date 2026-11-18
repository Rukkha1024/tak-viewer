//! Independent Labeling pane. Rows carry stable identities, not just display names.

use super::panels::{label_cell, label_columns, label_row_pairs, LinkedLabelPick};
use super::theme;
use crate::label_increment::IncrementMode;
pub use super::panels::{MarkerList, SelectionGesture};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, Ui};
use tak_core::edit::LabelKey;
use tak_core::tak::{uid_hex, Uid};

const ROW_H: f32 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelingMode {
    QuickLabel,
    Select,
}

#[derive(Clone, Debug)]
pub struct AssetChoice {
    pub uid: Uid,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct LabelRow {
    /// Unknown memberships have no key and remain identifiable by track UID only.
    pub key: Option<LabelKey>,
    /// None means a declared label has no trajectory yet.
    pub track_uid: Option<Uid>,
    /// Eligibility for multiselect: at least one sample anywhere in this take.
    pub has_samples: bool,
    pub name: String,
    pub color: Color32,
    pub fill_pct: f32,
    pub present: bool,
    pub selected: bool,
    pub primary: bool,
}

#[derive(Clone, Debug)]
pub struct LabelGroup {
    /// Stable within the document, including when groups share an asset.
    pub id: u64,
    /// Unknown memberships have no established asset; they appear under All only.
    pub asset_uid: Option<Uid>,
    pub title: String,
    pub collapsed: bool,
    /// Ordered declared labels and existing labeled trajectories, including missing declarations.
    pub labels: Vec<LabelRow>,
}

#[derive(Clone, Debug)]
pub struct UnlabeledRow {
    pub uid: Uid,
    pub name: String,
    pub fill_pct: f32,
    pub present: bool,
    pub selected: bool,
    pub primary: bool,
}

#[derive(Clone, Debug)]
pub struct LabelingState {
    /// Owning 3D slot, used only to route clicks and keys to the correct list.
    pub view_slot: usize,
    /// View-only search; the owning view keeps it alongside the shared group folds.
    pub filter: String,
    pub mode: LabelingMode,
    pub(crate) increment: IncrementMode,
    pub link_to_3d: bool,
    pub linked_pick: LinkedLabelPick,
    pub backward: bool,
    pub forward: bool,
    pub asset_filter: Option<Uid>,
    pub assets: Vec<AssetChoice>,
    pub groups: Vec<LabelGroup>,
    pub unlabeled: Vec<UnlabeledRow>,
    pub armed: Option<LabelKey>,
    /// One-shot request from the caller when a label is chosen outside this pane.
    pub scroll_to: Option<LabelKey>,
    /// Existing trajectories, including extra and unlabeled markers without a LabelKey.
    pub scroll_to_track: Option<Uid>,
    pub selected_labeled_count: usize,
    pub label_marker_reason: Option<&'static str>,
    /// A caller-provided reason, e.g. a read-only document or a pending edit.
    pub blocked_reason: Option<String>,
}

impl Default for LabelingState {
    fn default() -> Self {
        Self {
            view_slot: 0,
            filter: String::new(),
            mode: LabelingMode::QuickLabel,
            increment: IncrementMode::default(),
            link_to_3d: false,
            linked_pick: LinkedLabelPick::default(),
            backward: true,
            forward: true,
            asset_filter: None,
            assets: Vec::new(),
            groups: Vec::new(),
            unlabeled: Vec::new(),
            armed: None,
            scroll_to: None,
            scroll_to_track: None,
            selected_labeled_count: 0,
            label_marker_reason: Some("Select one 3D point"),
            blocked_reason: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LabelingAction {
    SetFilter(String),
    SetMode(LabelingMode),
    SetIncrement(IncrementMode),
    SetLinkTo3d(bool),
    SetBackward(bool),
    SetForward(bool),
    SetAssetFilter(Option<Uid>),
    ToggleGroup(u64),
    StepLabel {
        forward: bool,
    },
    SelectLabel {
        key: Option<LabelKey>,
        track_uid: Option<Uid>,
        additive: bool,
    },
    SelectUnlabeled {
        uid: Uid,
        additive: bool,
    },
    /// Selection only: never arms or applies a QuickLabel target.
    SelectTracks { list: MarkerList, uid: Option<Uid>, gesture: SelectionGesture },
    LabelMarker(LabelKey),
    EditContextTrack { uid: Uid, unlabel: bool },
    Unlabel,
    RemoveMarker,
}

impl LabelingState {
    fn shown_asset(&self) -> Option<Uid> {
        if self.link_to_3d { self.linked_pick.asset } else { self.asset_filter }
    }

    fn matches_row(&self, track: Option<Uid>, name: &str, needle: &str) -> bool {
        needle.is_empty() || name.to_lowercase().contains(needle)
            || (self.link_to_3d && track.is_some() && track == self.linked_pick.track)
    }

    pub(crate) fn track_order(&self, list: MarkerList) -> Vec<Uid> {
        match list {
            MarkerList::Named => self.groups.iter().flat_map(|group| &group.labels)
                .filter(|row| row.has_samples).filter_map(|row| row.track_uid).collect(),
            MarkerList::Unlabeled => self.unlabeled.iter().map(|row| row.uid).collect(),
        }
    }

    fn selection_summary(&self) -> (String, String) {
        let selected: std::collections::HashSet<_> = self.groups.iter().flat_map(|g| &g.labels)
            .filter(|row| row.selected).filter_map(|row| row.track_uid)
            .chain(self.unlabeled.iter().filter(|row| row.selected).map(|row| row.uid)).collect();
        let mut visible = std::collections::HashSet::new();
        for row in list_rows(self, 1) {
            if let ListRow::Labels(group, index, _) = row {
                if let Some(uid) = self.groups[group].labels[index].track_uid { visible.insert(uid); }
            }
        }
        let needle = self.filter.trim().to_lowercase();
        visible.extend(self.unlabeled.iter().filter(|row| self.matches_row(Some(row.uid), &row.name, &needle)).map(|row| row.uid));
        let hidden = selected.difference(&visible).count();
        let target = self.groups.iter().flat_map(|g| &g.labels).find(|row| row.primary && row.selected)
            .map(|row| row.name.as_str())
            .or_else(|| self.unlabeled.iter().find(|row| row.primary && row.selected).map(|row| row.name.as_str()))
            .unwrap_or("—");
        (format!("{} selected ({} hidden)", selected.len(), hidden), format!("Pattern target: {target}"))
    }

    pub fn unlabel_reason(&self) -> Option<&str> {
        if let Some(reason) = self.blocked_reason.as_deref() {
            return Some(reason);
        }
        if !self.backward && !self.forward {
            return Some("Select at least one labeling direction.");
        }
        (self.selected_labeled_count == 0).then_some("Select labeled marker tracks.")
    }
}

fn asset_name(asset: &AssetChoice, assets: &[AssetChoice]) -> String {
    if assets
        .iter()
        .filter(|other| other.name == asset.name)
        .count()
        > 1
    {
        format!("{} · {}", asset.name, uid_hex(&asset.uid))
    } else {
        asset.name.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListRow {
    Group(usize),
    Labels(usize, usize, Option<usize>),
}

fn list_rows(state: &LabelingState, count: usize) -> Vec<ListRow> {
    let mut rows = Vec::new();
    let needle = state.filter.trim().to_lowercase();
    for (gi, group) in state.groups.iter().enumerate() {
        if state
            .shown_asset()
            .is_some_and(|asset| Some(asset) != group.asset_uid)
        {
            continue;
        }
        let labels: Vec<_> = group.labels.iter().enumerate()
            .filter(|(_, row)| state.matches_row(row.track_uid, &row.name, &needle))
            .map(|(index, _)| index).collect();
        if !needle.is_empty() && labels.is_empty() {
            continue;
        }
        rows.push(ListRow::Group(gi));
        if group.collapsed && needle.is_empty() {
            continue;
        }
        for (left, right) in label_row_pairs(labels.len(), count) {
            rows.push(ListRow::Labels(gi, labels[left], right.map(|i| labels[i])));
        }
    }
    rows
}

pub(crate) fn visible_labels(state: &LabelingState) -> Vec<LabelKey> {
    list_rows(state, 1).into_iter().filter_map(|row| match row {
        ListRow::Labels(group, index, _) => state.groups[group].labels[index].key,
        ListRow::Group(_) => None,
    }).collect()
}

fn label_scroll_offset(rows: &[ListRow], state: &LabelingState, pitch: f32) -> Option<f32> {
    if state.scroll_to.is_none() && state.scroll_to_track.is_none() { return None; }
    rows.iter().position(|row| match *row {
        ListRow::Labels(group, left, right) => [Some(left), right].into_iter().flatten().any(|i| {
            let row = &state.groups[group].labels[i];
            state.scroll_to.is_some_and(|key| row.key == Some(key))
                || state.scroll_to_track.is_some_and(|uid| row.track_uid == Some(uid))
        }),
        ListRow::Group(_) => false,
    }).map(|index| ((index as f32 - 3.0) * pitch).max(0.0))
}

fn coverage_text(fill_pct: f32) -> String {
    if fill_pct > 0.0 && fill_pct <= 0.5 {
        "<1%".into()
    } else if fill_pct >= 99.5 && fill_pct < 100.0 {
        ">99%".into()
    } else {
        format!("{fill_pct:.0}%")
    }
}

fn context_track_action(row: &LabelRow, unlabel: bool) -> LabelingAction {
    if let Some(uid) = row.track_uid.filter(|_| !row.selected) {
        LabelingAction::EditContextTrack { uid, unlabel }
    } else if unlabel {
        LabelingAction::Unlabel
    } else {
        LabelingAction::RemoveMarker
    }
}

fn paint_row(
    ui: &Ui,
    rect: Rect,
    name: &str,
    fill_pct: f32,
    present: bool,
    dot: Color32,
    selected: bool,
    primary: bool,
    armed: bool,
    hover: bool,
    alternate: bool,
) {
    let p = ui.painter();
    let bg = if primary {
        theme::SELECT_PRIMARY.gamma_multiply(0.28)
    } else if selected {
        theme::SELECT_SECONDARY.gamma_multiply(0.22)
    } else if hover {
        Color32::from_rgb(0x3a, 0x3a, 0x3a)
    } else if alternate {
        theme::ROW_ALT
    } else {
        theme::PANEL_BG
    };
    p.rect_filled(rect, 0.0, bg);
    if armed {
        p.rect_stroke(rect.shrink(0.5), 0.0, Stroke::new(1.0, theme::ACCENT));
    }
    p.circle_filled(pos2(rect.left() + 13.0, rect.center().y), 4.0, dot);
    let color = if present {
        theme::LABEL_TRACKED
    } else {
        theme::LABEL_ABSENT
    };
    let percent = p.layout_no_wrap(coverage_text(fill_pct), FontId::proportional(12.5), theme::TEXT);
    let left = rect.left() + 24.0;
    let percent_r = rect.right() - 8.0;
    let name_width = (percent_r - 44.0_f32.max(percent.size().x + 6.0) - left).max(0.0);
    let mut job = egui::text::LayoutJob::simple_singleline(
        name.to_owned(),
        FontId::proportional(13.0),
        color,
    );
    job.wrap.max_width = name_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let name = p.layout_job(job);
    let clip = Rect::from_min_size(pos2(left, rect.top()), vec2(name_width, rect.height()))
        .intersect(rect);
    p.with_clip_rect(clip).galley(
        pos2(left, rect.center().y - name.size().y * 0.5),
        name,
        color,
    );
    p.galley(
        pos2(
            percent_r - percent.size().x,
            rect.center().y - percent.size().y * 0.5,
        ),
        percent,
        theme::TEXT,
    );
}

fn draw_headers(ui: &Ui, rect: Rect, count: usize) {
    let p = ui.painter();
    for col in 0..count {
        let r = label_cell(rect, col, count);
        p.text(
            pos2(r.left() + 24.0, r.center().y),
            Align2::LEFT_CENTER,
            "Labels",
            FontId::proportional(12.5),
            theme::ACCENT,
        );
        p.text(
            pos2(r.right() - 8.0, r.center().y),
            Align2::RIGHT_CENTER,
            "%",
            FontId::proportional(12.5),
            theme::TEXT_DIM,
        );
    }
    if count == 2 {
        p.line_segment(
            [
                pos2(rect.center().x, rect.top()),
                pos2(rect.center().x, rect.bottom()),
            ],
            Stroke::new(1.0, theme::GRAPH_SEP),
        );
    }
}

fn modifier_gesture(modifiers: egui::Modifiers) -> Option<SelectionGesture> {
    let additive = modifiers.ctrl || modifiers.command;
    if modifiers.shift { Some(SelectionGesture::Range { additive }) }
    else if additive { Some(SelectionGesture::Toggle) }
    else if modifiers.any() { Some(SelectionGesture::Plain) }
    else { None }
}

/// Draw the pane and return at most one user action. The caller applies selection and edits.
pub fn show(ui: &mut Ui, state: &LabelingState) -> Option<LabelingAction> {
    let mut action = None;
    let (count, target) = state.selection_summary();
    ui.label(RichText::new(count).size(11.0).color(theme::TEXT_DIM));
    ui.label(RichText::new(target).size(11.0).color(theme::TEXT_DIM));
    let panel_height = ui.available_height();
    egui::ScrollArea::vertical()
        .id_salt("labeling_pane")
        .show(ui, |ui| {
            let panel_top = ui.cursor().min.y;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                if ui
                    .selectable_label(
                        state.mode == LabelingMode::QuickLabel,
                        RichText::new("QuickLabel Mode  D").size(11.0),
                    )
                    .clicked()
                {
                    action = Some(LabelingAction::SetMode(LabelingMode::QuickLabel));
                }
                if ui
                    .selectable_label(
                        state.mode == LabelingMode::Select,
                        RichText::new("Select Mode  Q").size(11.0),
                    )
                    .clicked()
                {
                    action = Some(LabelingAction::SetMode(LabelingMode::Select));
                }
                if ui
                    .add_enabled(
                        state.unlabel_reason().is_none(),
                        egui::Button::new(RichText::new("Unlabel  Shift+D").size(11.0)),
                    )
                    .clicked()
                {
                    action = Some(LabelingAction::Unlabel);
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Increment");
                egui::ComboBox::from_id_salt("labeling_increment")
                    .width(188.0)
                    .selected_text(state.increment.caption())
                    .show_ui(ui, |ui| {
                        for mode in IncrementMode::ALL {
                            if ui.selectable_label(state.increment == mode, mode.caption()).clicked() {
                                action = Some(LabelingAction::SetIncrement(mode));
                            }
                        }
                    });
                if ui
                    .add_enabled(
                        !state.backward || state.forward,
                        egui::Button::new(RichText::new("Back").size(11.0))
                            .selected(state.backward),
                    )
                    .clicked()
                {
                    action = Some(LabelingAction::SetBackward(!state.backward));
                }
                if ui
                    .add_enabled(
                        !state.forward || state.backward,
                        egui::Button::new(RichText::new("Fwd").size(11.0)).selected(state.forward),
                    )
                    .clicked()
                {
                    action = Some(LabelingAction::SetForward(!state.forward));
                }
            });
            if let Some(reason) = state.unlabel_reason() {
                ui.label(
                    RichText::new(format!("Unlabel: {reason}"))
                        .size(11.0)
                        .color(theme::TEXT_DIM),
                );
            }
            if !state.backward || !state.forward {
                ui.label(
                    RichText::new("At least one labeling direction must remain on.")
                        .size(11.0)
                        .color(theme::TEXT_DIM),
                );
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Selected");
                let selected = state
                    .shown_asset()
                    .and_then(|id| state.assets.iter().find(|a| a.uid == id))
                    .map(|asset| asset_name(asset, &state.assets))
                    .unwrap_or_else(|| "All".to_owned());
                ui.add_enabled_ui(!state.link_to_3d, |ui| egui::ComboBox::from_id_salt("labeling_asset")
                    .selected_text(selected)
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(state.asset_filter.is_none(), "All")
                            .clicked()
                        {
                            action = Some(LabelingAction::SetAssetFilter(None));
                        }
                        for asset in &state.assets {
                            if ui
                                .selectable_label(
                                    state.asset_filter == Some(asset.uid),
                                    asset_name(asset, &state.assets),
                                )
                                .clicked()
                            {
                                action = Some(LabelingAction::SetAssetFilter(Some(asset.uid)));
                            }
                        }
                    }));
                if ui.small_button("A Prev").clicked() {
                    action = Some(LabelingAction::StepLabel { forward: false });
                }
                if ui.small_button("S Next").clicked() {
                    action = Some(LabelingAction::StepLabel { forward: true });
                }
            });
            let mut link_to_3d = state.link_to_3d;
            if ui.checkbox(&mut link_to_3d, "Link to 3D Selection").changed() {
                action = Some(LabelingAction::SetLinkTo3d(link_to_3d));
            }

            ui.horizontal(|ui| {
                let total = state.groups.iter().flat_map(|g| &g.labels).filter(|row| row.track_uid.is_some()).count();
                let present = state.groups.iter().flat_map(|g| &g.labels).filter(|row| row.track_uid.is_some() && row.present).count();
                ui.label(RichText::new("Labeled").strong().size(14.0));
                ui.label(RichText::new(format!("{present} / {total}")).color(theme::TEXT_DIM).size(14.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut filter = state.filter.clone();
                    let width = 130.0_f32.min(ui.available_width()).max(0.0);
                    if ui.add(egui::TextEdit::singleline(&mut filter).hint_text("Search").desired_width(width)).changed() {
                        action = Some(LabelingAction::SetFilter(filter));
                    }
                });
            });
            let count = label_columns(ui.available_width());
            let (header, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
            let rows = list_rows(state, count);
            // Reserve the measured legend and the unlabeled section before filling the list.
            let mut legend = egui::text::LayoutJob::default();
            let font = egui::TextStyle::Small.resolve(ui.style());
            for (text, color) in [
                ("White", theme::LABEL_TRACKED),
                (" = present at current frame · ", theme::TEXT_DIM),
                ("Red", theme::LABEL_ABSENT),
                (" = absent · % = share of take frames with a point. Gap count and longest gap in Selected Marker.", theme::TEXT_DIM),
            ] {
                legend.append(text, 0.0, egui::TextFormat::simple(font.clone(), color));
            }
            legend.wrap.max_width = ui.available_width();
            let legend = ui.painter().layout_job(legend);
            let needle = state.filter.trim().to_lowercase();
            let unlabeled: Vec<_> = state.unlabeled.iter().enumerate()
                .filter(|(_, row)| state.matches_row(Some(row.uid), &row.name, &needle))
                .map(|(index, _)| index).collect();
            let pitch = ROW_H + ui.spacing().item_spacing.y;
            let unlabeled_height = (unlabeled.len() as f32 * pitch).min(120.0);
            let reserve = legend.size().y + ROW_H + unlabeled_height + 4.0 * ui.spacing().item_spacing.y;
            let height = (panel_height - (ui.cursor().min.y - panel_top) - reserve).max(40.0);
            let mut label_scroll = egui::ScrollArea::vertical()
                .id_salt("labeling_labels")
                .max_height(height)
                .auto_shrink([false, true]);
            if let Some(offset) = label_scroll_offset(&rows, state, pitch) {
                label_scroll = label_scroll.vertical_scroll_offset(offset);
            }
            let list = label_scroll.show_rows(ui, ROW_H, rows.len(), |ui, visible| {
                let first = visible.start;
                for (offset, row) in rows[visible].iter().enumerate() {
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
                    match *row {
                        ListRow::Group(gi) => {
                            let group = &state.groups[gi];
                            let resp = ui.interact(
                                rect,
                                ui.make_persistent_id(("group", group.id)),
                                Sense::click(),
                            );
                            ui.painter().rect_filled(rect, 0.0, theme::HEADER_BG);
                            let present = group.labels.iter().filter(|row| row.track_uid.is_some() && row.present).count();
                            let total = group.labels.iter().filter(|row| row.track_uid.is_some()).count();
                            ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER,
                                if group.collapsed && needle.is_empty() { "+" } else { "-" },
                                FontId::proportional(12.0), theme::TEXT_DIM);
                            let title = ui.painter().with_clip_rect(rect).text(
                                pos2(rect.left() + 24.0, rect.center().y),
                                Align2::LEFT_CENTER,
                                &group.title,
                                FontId::proportional(12.0),
                                theme::TEXT,
                            );
                            ui.painter().with_clip_rect(rect).text(pos2(title.right() + 8.0, rect.center().y),
                                Align2::LEFT_CENTER, format!("{present} / {total}"),
                                FontId::proportional(12.0), theme::TEXT_DIM);
                            if resp.clicked() {
                                action = Some(LabelingAction::ToggleGroup(group.id));
                            }
                        }
                        ListRow::Labels(gi, left, right) => {
                            for (col, index) in [Some(left), right].into_iter().enumerate() {
                                let Some(index) = index else { continue };
                                let row = &state.groups[gi].labels[index];
                                let rect = label_cell(rect, col, count);
                                let resp = ui.interact(
                                    rect,
                                    ui.make_persistent_id(("label", row.key, row.track_uid)),
                                    Sense::click(),
                                );
                                paint_row(
                                    ui,
                                    rect,
                                    &row.name,
                                    row.fill_pct,
                                    row.present,
                                    row.color,
                                    row.selected,
                                    row.primary,
                                    row.key.is_some() && state.armed == row.key,
                                    resp.hovered(),
                                    (first + offset) % 2 == 1,
                                );
                                let gesture = ui.input(|i| modifier_gesture(i.modifiers));
                                if resp.clicked() {
                                    action = Some(if let Some(gesture) = gesture {
                                        LabelingAction::SelectTracks { list: MarkerList::Named, uid: row.track_uid, gesture }
                                    } else {
                                        LabelingAction::SelectLabel { key: row.key, track_uid: row.track_uid, additive: false }
                                    });
                                }
                                if gesture.is_none() { resp.context_menu(|ui| {
                                    let label_reason = if row.key.is_none() {
                                        Some("No label for this row")
                                    } else {
                                        state.label_marker_reason
                                    };
                                    let assign = ui.add_enabled(label_reason.is_none(), egui::Button::new("Label Marker"));
                                    if let Some(reason) = label_reason {
                                        assign.on_disabled_hover_text(reason);
                                    } else if assign.clicked() {
                                        action = row.key.map(LabelingAction::LabelMarker);
                                        ui.close_menu();
                                    }
                                    if ui.add_enabled(row.track_uid.is_some() && state.blocked_reason.is_none(), egui::Button::new("Remove Marker")).clicked() {
                                        action = Some(context_track_action(row, false));
                                        ui.close_menu();
                                    }
                                    let why = if row.track_uid.is_none() {
                                        Some("This label has no marker track.")
                                    } else if row.selected {
                                        state.unlabel_reason()
                                    } else if row.key.is_none() {
                                        Some("This marker has no known label.")
                                    } else {
                                        state.blocked_reason.as_deref()
                                    };
                                    if ui
                                        .add_enabled(
                                            why.is_none(),
                                            egui::Button::new("Unlabel  Shift+D"),
                                        )
                                        .clicked()
                                    {
                                        action = Some(context_track_action(row, true));
                                        ui.close_menu();
                                    }
                                    if let Some(why) = why {
                                        ui.label(
                                            RichText::new(why).size(11.0).color(theme::TEXT_DIM),
                                        );
                                    }
                                }); }
                            }
                            if count == 2 {
                                ui.painter().line_segment(
                                    [
                                        pos2(rect.center().x, rect.top()),
                                        pos2(rect.center().x, rect.bottom()),
                                    ],
                                    Stroke::new(1.0, theme::GRAPH_SEP),
                                );
                            }
                        }
                    }
                }
            });
            crate::app::key_focus::region(ui, list.inner_rect, crate::app::key_focus::Target::NamedMarkers(state.view_slot));
            draw_headers(
                ui,
                Rect::from_min_max(header.min, pos2(list.inner_rect.right(), header.bottom())),
                count,
            );
            let (rect, _) = ui.allocate_exact_size(legend.size(), Sense::hover());
            crate::app::key_focus::region(ui, rect, crate::app::key_focus::Target::Blocked);
            ui.painter().galley(rect.min, legend, theme::TEXT_DIM);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Unlabeled Markers").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("%").color(theme::TEXT_DIM));
                });
            });
            let mut unlabeled_scroll = egui::ScrollArea::vertical()
                .id_salt("labeling_unlabeled")
                .max_height(120.0)
                .auto_shrink([false, true]);
            if let Some(index) = state.scroll_to_track.and_then(|uid| unlabeled.iter().position(|&i| state.unlabeled[i].uid == uid)) {
                unlabeled_scroll = unlabeled_scroll.vertical_scroll_offset(((index as f32 - 3.0) * pitch).max(0.0));
            }
            let unlabeled_list = unlabeled_scroll.show_rows(ui, ROW_H, unlabeled.len(), |ui, visible| {
                    let first = visible.start;
                    for (offset, &index) in unlabeled[visible].iter().enumerate() {
                        let row = &state.unlabeled[index];
                        let (rect, _) = ui
                            .allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
                        let resp = ui.interact(
                            rect,
                            ui.make_persistent_id(("unlabeled", row.uid)),
                            Sense::click(),
                        );
                        paint_row(
                            ui,
                            rect,
                            &row.name,
                            row.fill_pct,
                            row.present,
                            theme::UNLABELED,
                            row.selected,
                            row.primary,
                            false,
                            resp.hovered(),
                            (first + offset) % 2 == 1,
                        );
                        let gesture = ui.input(|i| modifier_gesture(i.modifiers));
                        if resp.clicked() || (resp.secondary_clicked() && !row.selected) {
                            action = Some(if let Some(gesture) = gesture {
                                LabelingAction::SelectTracks { list: MarkerList::Unlabeled, uid: Some(row.uid), gesture }
                            } else {
                                LabelingAction::SelectUnlabeled { uid: row.uid, additive: false }
                            });
                        }
                        if gesture.is_none() { resp.context_menu(|ui| {
                            if ui.add_enabled(state.blocked_reason.is_none(), egui::Button::new("Remove Marker")).clicked() {
                                action = Some(LabelingAction::RemoveMarker);
                                ui.close_menu();
                            }
                        }); }
                    }
                });
            crate::app::key_focus::region(ui, unlabeled_list.inner_rect, crate::app::key_focus::Target::UnlabeledMarkers(state.view_slot));
        });
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, Modifiers, PointerButton, RawInput};

    fn label(index: u8, present: bool) -> LabelRow {
        LabelRow {
            key: Some(LabelKey {
                parent: [7; 16],
                slot: index as i32 + 1,
            }),
            track_uid: Some([index; 16]),
            has_samples: true,
            name: format!("L{index:02}"),
            color: theme::ACCENT,
            fill_pct: 90.0,
            present,
            selected: false,
            primary: false,
        }
    }

    fn state(count: u8) -> LabelingState {
        let mut labels: Vec<_> = (0..count).map(|i| label(i, i != 2)).collect();
        if count > 2 {
            labels[2].selected = true;
            labels[2].primary = true;
        }
        LabelingState {
            assets: vec![AssetChoice {
                uid: [7; 16],
                name: "Asset".into(),
            }],
            groups: vec![LabelGroup {
                id: 1,
                asset_uid: Some([7; 16]),
                title: "Asset · Markers".into(),
                collapsed: false,
                labels,
            }],
            unlabeled: vec![UnlabeledRow {
                uid: [99; 16],
                name: "Unlabeled 99".into(),
                fill_pct: 99.8,
                present: false,
                selected: false,
                primary: false,
            }],
            armed: (count > 2).then_some(LabelKey {
                parent: [7; 16],
                slot: 3,
            }),
            selected_labeled_count: usize::from(count > 2),
            ..Default::default()
        }
    }

    fn draw(
        ctx: &egui::Context,
        state: &LabelingState,
        width: f32,
        events: Vec<Event>,
    ) -> (egui::FullOutput, Option<LabelingAction>) {
        let modifiers = events
            .iter()
            .filter_map(|event| match event {
                Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .last()
            .unwrap_or_default();
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 900.0))),
            focused: true,
            modifiers,
            events,
            ..Default::default()
        };
        let mut action = None;
        let output = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = show(ui, state);
            });
        });
        (output, action)
    }

    struct Painted {
        text: String,
        rect: Rect,
        color: Color32,
        clip: Rect,
        elided: bool,
    }

    fn texts(output: &egui::FullOutput) -> Vec<Painted> {
        fn walk(shape: &egui::Shape, clip: Rect, out: &mut Vec<Painted>) {
            match shape {
                egui::Shape::Text(t) => out.push(Painted {
                    text: t.galley.text().to_owned(),
                    rect: Rect::from_min_size(t.pos, t.galley.size()),
                    clip,
                    color: t
                        .galley
                        .job
                        .sections
                        .first()
                        .map_or(t.fallback_color, |s| s.format.color),
                    elided: t.galley.elided,
                }),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, clip, out)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for shape in &output.shapes {
            walk(&shape.shape, shape.clip_rect, &mut found);
        }
        found
    }

    fn click(
        ctx: &egui::Context,
        state: &LabelingState,
        name: &str,
        modifiers: Modifiers,
        button: PointerButton,
    ) -> Option<LabelingAction> {
        let (output, _) = draw(ctx, state, 440.0, vec![]);
        let found = texts(&output);
        let at = found
            .iter()
            .find(|item| item.text == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .rect
            .center();
        let event = |pressed| Event::PointerButton {
            pos: at,
            button,
            pressed,
            modifiers,
        };
        draw(ctx, state, 440.0, vec![Event::PointerMoved(at)]);
        draw(ctx, state, 440.0, vec![event(true)]);
        draw(ctx, state, 440.0, vec![event(false)]).1
    }

    fn save_snapshot(ctx: &egui::Context, output: egui::FullOutput) {
        let Some(dir) = std::env::var_os("TAK_W2_2D_SNAPSHOT_DIR") else {
            return;
        };
        let mut textures = crate::soft_render::Textures::default();
        textures.set(&output.textures_delta);
        let prims = ctx.tessellate(output.shapes, output.pixels_per_point);
        let image = crate::soft_render::paint(
            &prims,
            &textures,
            [440, 900],
            output.pixels_per_point,
            Color32::BLACK,
        );
        let rgba: Vec<_> = image
            .pixels
            .iter()
            .flat_map(|pixel| pixel.to_array())
            .collect();
        crate::shot::save_png(
            &std::path::PathBuf::from(dir).join("labeling.png"),
            440,
            900,
            &rgba,
        )
        .unwrap();
    }

    #[test]
    fn column_major_order_filter_and_fold_use_existing_layout_rules() {
        let mut st = state(5);
        assert_eq!((label_columns(259.9), label_columns(260.0)), (1, 2));
        let layout: Vec<_> = list_rows(&st, 2)
            .iter()
            .filter_map(|row| match *row {
                ListRow::Labels(_, left, right) => Some((left, right)),
                _ => None,
            })
            .collect();
        assert_eq!(layout, [(0, Some(3)), (1, Some(4)), (2, None)]);
        assert_eq!(list_rows(&st, 1).len(), 6);
        st.groups[0].collapsed = true;
        assert_eq!(list_rows(&st, 2).len(), 1);
        st.asset_filter = Some([8; 16]);
        assert!(list_rows(&st, 2).is_empty());
    }

    #[test]
    fn search_reveals_folded_matches_and_step_increment_order_matches_the_visible_list() {
        let mut st = state(5);
        for (row, name) in st.groups[0].labels.iter_mut().zip(["keep A", "skip", "Keep B", "keep C", "other"]) {
            row.name = name.into();
        }
        st.groups[0].collapsed = true;
        assert!(visible_labels(&st).is_empty());
        st.filter = "  KEEP  ".into();
        assert_eq!(list_rows(&st, 2), [ListRow::Group(0), ListRow::Labels(0, 0, Some(3)), ListRow::Labels(0, 2, None)]);
        assert_eq!(visible_labels(&st), [0, 2, 3].map(|i| st.groups[0].labels[i].key.unwrap()));
        st.asset_filter = Some([8; 16]);
        assert!(visible_labels(&st).is_empty());
        st.asset_filter = None;
        st.filter = "missing".into();
        assert!(list_rows(&st, 2).is_empty());
    }

    #[test]
    fn search_filters_both_lists_but_count_excludes_declarations_and_ignores_filters() {
        let ctx = egui::Context::default();
        let mut st = state(5);
        st.groups[0].labels[4].track_uid = None;
        st.groups[0].labels[4].present = false;
        st.filter = "unlabeled".into();
        let out = draw(&ctx, &st, 440.0, vec![]).0;
        let painted = texts(&out);
        assert!(painted.iter().any(|t| t.text == "3 / 4"));
        assert!(painted.iter().any(|t| t.text == "Unlabeled 99"));
        assert!(!painted.iter().any(|t| t.text == "L00"));
        st.filter = "L00".into();
        let out = draw(&ctx, &st, 440.0, vec![]).0;
        let painted = texts(&out);
        assert!(painted.iter().any(|t| t.text == "3 / 4"));
        assert!(!painted.iter().any(|t| t.text == "Unlabeled 99"));
    }

    #[test]
    fn search_field_accepts_typing_and_keeps_text_field_undo() {
        let ctx = egui::Context::default();
        let mut st = state(5);
        assert_eq!(click(&ctx, &st, "Search", Modifiers::NONE, PointerButton::Primary), None);
        assert!(ctx.wants_keyboard_input());
        assert_eq!(draw(&ctx, &st, 440.0, vec![Event::Text("L00".into())]).1,
            Some(LabelingAction::SetFilter("L00".into())));
        st.filter = "L00".into();
        let _ = draw(&ctx, &st, 440.0, vec![]);
        let painted = texts(&draw(&ctx, &st, 440.0, vec![]).0);
        assert!(painted.iter().any(|t| t.text == "L00"));
        assert!(!painted.iter().any(|t| t.text == "L01"));
        let undo = Event::Key {
            key: egui::Key::Z, physical_key: None, pressed: true, repeat: false,
            modifiers: Modifiers { ctrl: true, command: true, ..Default::default() },
        };
        assert_eq!(draw(&ctx, &st, 440.0, vec![undo]).1,
            Some(LabelingAction::SetFilter(String::new())));
    }

    #[test]
    fn uid_scroll_reaches_extra_markers_without_label_keys_and_unlabeled_trajectories() {
        for unlabeled in [false, true] {
            let ctx = egui::Context::default();
            let mut st = state(200);
            for row in &mut st.groups[0].labels { row.key = None; }
            if unlabeled {
                st.unlabeled = st.groups.remove(0).labels.into_iter().map(|row| UnlabeledRow {
                    uid: row.track_uid.unwrap(), name: row.name, fill_pct: row.fill_pct,
                    present: row.present, selected: row.selected, primary: row.primary,
                }).collect();
            }
            let before = draw(&ctx, &st, 440.0, vec![]).0;
            assert!(!texts(&before).iter().any(|t| t.text == "L180"));
            st.scroll_to_track = Some([180; 16]);
            let after = draw(&ctx, &st, 440.0, vec![]).0;
            let painted = texts(&after);
            let target = painted.iter().find(|t| t.text == "L180").expect("UID target painted");
            assert!(target.clip.contains_rect(target.rect));
        }
    }

    #[test]
    fn headless_rows_keep_red_absence_separate_from_selection_and_clip_names() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut st = state(5);
        st.groups[0].labels[0].name = "A_very_long_marker_name_abcdefghijklmnopqrstuvwxyz".into();
        let (output, _) = draw(&ctx, &st, 440.0, vec![]);
        let labels = texts(&output);
        let find = |name: &str| {
            labels
                .iter()
                .find(|item| item.text == name)
                .unwrap_or_else(|| panic!("missing {name}"))
        };
        assert_eq!(find("L02").color, theme::LABEL_ABSENT);
        assert_eq!(find("Unlabeled 99").color, theme::LABEL_ABSENT);
        assert_eq!(find("L03").color, theme::LABEL_TRACKED);
        assert!(
            (find("L03").rect.top()
                - find("A_very_long_marker_name_abcdefghijklmnopqrstuvwxyz")
                    .rect
                    .top())
            .abs()
                < 0.5
        );
        let long = find("A_very_long_marker_name_abcdefghijklmnopqrstuvwxyz");
        assert!(long.elided && long.rect.right() <= long.clip.right() + 0.5);
        assert!(output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == theme::SELECT_PRIMARY.gamma_multiply(0.28) && r.rect.contains(find("L02").rect.center()))));
        assert!(output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.stroke.color == theme::ACCENT && r.rect.contains(find("L02").rect.center()))));
    }

    #[test]
    fn armed_outline_preserves_primary_secondary_and_unselected_row_backgrounds() {
        let ctx = egui::Context::default();
        let mut st = state(5);
        st.groups[0].labels[0].selected = true;
        for armed in [2, 0, 1] {
            st.armed = st.groups[0].labels[armed].key;
            let out = draw(&ctx, &st, 440.0, vec![]).0;
            let painted = texts(&out);
            for (index, background) in [(0, theme::SELECT_SECONDARY.gamma_multiply(0.22)), (2, theme::SELECT_PRIMARY.gamma_multiply(0.28))] {
                let name = format!("L{index:02}");
                let center = painted.iter().find(|t| t.text == name).unwrap().rect.center();
                assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == background && r.rect.contains(center))));
            }
            let name = format!("L{armed:02}");
            let center = painted.iter().find(|t| t.text == name).unwrap().rect.center();
            assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.stroke.color == theme::ACCENT && r.rect.contains(center))));
        }
    }

    #[test]
    fn group_and_global_presence_counts_use_actual_tracks_including_extras() {
        let ctx = egui::Context::default();
        let mut st = state(5);
        st.groups[0].labels[4].track_uid = None;
        st.groups[0].labels[4].present = false;
        let out = draw(&ctx, &st, 440.0, vec![]).0;
        let painted = texts(&out);
        assert_eq!(painted.iter().filter(|t| t.text == "3 / 4").count(), 2, "global and group totals exclude the declaration");
        assert!(!painted.iter().any(|t| t.text == "3 / 5"));
    }

    #[test]
    fn headless_clicks_return_stable_ids_and_do_not_disable_the_last_direction() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut st = state(5);
        let ctrl = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(
            click(&ctx, &st, "L03", ctrl, PointerButton::Primary),
            Some(LabelingAction::SelectTracks {
                list: MarkerList::Named, uid: Some([3; 16]), gesture: SelectionGesture::Toggle
            })
        );
        assert_eq!(
            click(
                &ctx,
                &st,
                "Unlabeled 99",
                Modifiers::NONE,
                PointerButton::Primary
            ),
            Some(LabelingAction::SelectUnlabeled {
                uid: [99; 16],
                additive: false
            })
        );
        assert_eq!(
            click(&ctx, &st, "L03", Modifiers::NONE, PointerButton::Secondary),
            None,
            "opening the menu keeps the 3D source selected"
        );
        st.forward = false;
        let (out, _) = draw(&ctx, &st, 440.0, vec![]);
        assert!(texts(&out)
            .iter()
            .any(|item| item.text.contains("At least one labeling direction")));
        assert_eq!(
            click(&ctx, &st, "Back", Modifiers::NONE, PointerButton::Primary),
            None
        );
    }

    #[test]
    fn every_modifier_row_click_is_selection_only_in_quicklabel_mode() {
        let ctx = egui::Context::default();
        let mut st = state(5);
        assert_eq!(st.mode, LabelingMode::QuickLabel);
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        let shift = Modifiers { shift: true, ..Default::default() };
        let ctrl_shift = Modifiers { ctrl: true, shift: true, ..Default::default() };
        let alt = Modifiers { alt: true, ..Default::default() };
        for (modifiers, gesture) in [(ctrl, SelectionGesture::Toggle), (shift, SelectionGesture::Range { additive: false }), (ctrl_shift, SelectionGesture::Range { additive: true }), (alt, SelectionGesture::Plain)] {
            assert_eq!(click(&ctx, &st, "L03", modifiers, PointerButton::Primary), Some(LabelingAction::SelectTracks { list: MarkerList::Named, uid: Some([3; 16]), gesture }));
            assert_eq!(click(&ctx, &st, "Unlabeled 99", modifiers, PointerButton::Primary), Some(LabelingAction::SelectTracks { list: MarkerList::Unlabeled, uid: Some([99; 16]), gesture }));
        }
        st.groups[0].labels[3].track_uid = None;
        st.groups[0].labels[3].has_samples = false;
        assert_eq!(click(&ctx, &st, "L03", shift, PointerButton::Primary), Some(LabelingAction::SelectTracks { list: MarkerList::Named, uid: None, gesture: SelectionGesture::Range { additive: false } }));
    }

    #[test]
    fn duplicate_display_names_select_the_clicked_uid_and_slot() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut st = state(4);
        st.groups[0].labels[0].name = "Same name".into();
        st.groups[0].labels[2].name = "Same name".into();
        let (output, _) = draw(&ctx, &st, 440.0, vec![]);
        let painted = texts(&output);
        let mut same: Vec<_> = painted.iter().filter(|t| t.text == "Same name").collect();
        same.sort_by(|a, b| a.rect.left().total_cmp(&b.rect.left()));
        assert_eq!(same.len(), 2);
        let at = same[1].rect.center();
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        draw(&ctx, &st, 440.0, vec![Event::PointerMoved(at)]);
        draw(&ctx, &st, 440.0, vec![button(true)]);
        let action = draw(&ctx, &st, 440.0, vec![button(false)]).1;
        assert_eq!(
            action,
            Some(LabelingAction::SelectLabel {
                key: st.groups[0].labels[2].key,
                track_uid: Some([2; 16]),
                additive: false
            })
        );
    }

    #[test]
    fn unknown_membership_is_selected_by_uid_without_inventing_a_label() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut st = state(0);
        let mut unknown = label(42, true);
        unknown.key = None;
        unknown.name = "Unknown membership".into();
        st.groups = vec![LabelGroup {
            id: 2,
            asset_uid: None,
            title: "Unknown".into(),
            collapsed: false,
            labels: vec![unknown],
        }];
        assert_eq!(
            click(
                &ctx,
                &st,
                "Unknown membership",
                Modifiers::NONE,
                PointerButton::Primary
            ),
            Some(LabelingAction::SelectLabel {
                key: None,
                track_uid: Some([42; 16]),
                additive: false
            })
        );
        st.asset_filter = Some([7; 16]);
        assert!(list_rows(&st, 2).is_empty());
    }

    #[test]
    fn scroll_to_a_right_column_label_keeps_the_target_visible() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut st = state(200);
        let before = draw(&ctx, &st, 440.0, vec![]).0;
        assert!(!texts(&before).iter().any(|item| item.text == "L180"));
        st.scroll_to = st.groups[0].labels[180].key;
        let after = draw(&ctx, &st, 440.0, vec![]).0;
        let labels = texts(&after);
        let target = labels
            .iter()
            .find(|item| item.text == "L180")
            .expect("target row was painted");
        assert!(target.clip.contains_rect(target.rect));
        assert!(
            labels
                .iter()
                .filter(|item| item.text.starts_with('L') && item.text.len() == 4)
                .count()
                < 60
        );
    }

    #[test]
    fn narrow_and_wide_headless_layouts_paint_the_expected_label_columns() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let st = state(5);
        let narrow = draw(&ctx, &st, 260.0, vec![]).0;
        let items = texts(&narrow);
        assert_eq!(items.iter().filter(|item| item.text == "Labels").count(), 1);
        let first = items.iter().find(|item| item.text == "L00").unwrap();
        let last = items.iter().find(|item| item.text == "L04").unwrap();
        assert!((first.rect.left() - last.rect.left()).abs() < 0.5);
        assert!(first.rect.top() < last.rect.top());
        let wide = draw(&ctx, &st, 440.0, vec![]).0;
        let items = texts(&wide);
        assert_eq!(items.iter().filter(|item| item.text == "Labels").count(), 2);
        let left = items.iter().find(|item| item.text == "L00").unwrap();
        let right = items.iter().find(|item| item.text == "L03").unwrap();
        assert!((left.rect.top() - right.rect.top()).abs() < 0.5);
        assert!(right.rect.left() > left.rect.right());
    }

    #[test]
    fn mode_and_step_controls_return_actions_without_using_keyboard_events() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let st = state(5);
        assert_eq!(
            click(
                &ctx,
                &st,
                "Select Mode  Q",
                Modifiers::NONE,
                PointerButton::Primary
            ),
            Some(LabelingAction::SetMode(LabelingMode::Select))
        );
        assert_eq!(
            click(&ctx, &st, "A Prev", Modifiers::NONE, PointerButton::Primary),
            Some(LabelingAction::StepLabel { forward: false })
        );
        assert_eq!(
            click(&ctx, &st, "S Next", Modifiers::NONE, PointerButton::Primary),
            Some(LabelingAction::StepLabel { forward: true })
        );
    }

    #[test]
    fn selected_row_context_menu_preserves_selection_and_emits_unlabel() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut st = state(5);
        st.selected_labeled_count = 2;
        st.groups[0].labels[0].selected = true;
        assert_eq!(
            click(&ctx, &st, "L02", Modifiers::NONE, PointerButton::Secondary),
            None
        );
        let out = draw(&ctx, &st, 440.0, vec![]).0;
        let painted = texts(&out);
        let mut items: Vec<_> = painted
            .iter()
            .filter(|item| item.text == "Unlabel  Shift+D")
            .collect();
        items.sort_by(|a, b| a.rect.top().total_cmp(&b.rect.top()));
        assert_eq!(items.len(), 2, "toolbar button and row menu");
        let at = items[1].rect.center();
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        draw(&ctx, &st, 440.0, vec![Event::PointerMoved(at)]);
        draw(&ctx, &st, 440.0, vec![button(true)]);
        assert_eq!(
            draw(&ctx, &st, 440.0, vec![button(false)]).1,
            Some(LabelingAction::Unlabel)
        );
    }

    #[test]
    fn remove_marker_menu_keeps_selected_labeled_and_unlabeled_rows() {
        for name in ["L02", "Unlabeled 99"] {
            let ctx = egui::Context::default();
            theme::apply(&ctx);
            let mut st = state(5);
            st.groups[0].labels[0].selected = true;
            st.unlabeled[0].selected = true;
            assert_eq!(click(&ctx, &st, name, Modifiers::NONE, PointerButton::Secondary), None);
            let output = draw(&ctx, &st, 440.0, vec![]).0;
            let at = texts(&output).iter().find(|item| item.text == "Remove Marker").unwrap().rect.center();
            let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
            draw(&ctx, &st, 440.0, vec![Event::PointerMoved(at)]);
            draw(&ctx, &st, 440.0, vec![button(true)]);
            assert_eq!(draw(&ctx, &st, 440.0, vec![button(false)]).1, Some(LabelingAction::RemoveMarker));
        }
    }

    #[test]
    fn headless_snapshot_uses_the_existing_cpu_painter() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let (output, _) = draw(&ctx, &state(20), 440.0, vec![]);
        let labels = texts(&output);
        for name in [
            "QuickLabel Mode  D",
            "Select Mode  Q",
            "Unlabel  Shift+D",
            "A Prev",
            "S Next",
            "Unlabeled Markers",
        ] {
            assert!(
                labels.iter().any(|item| item.text == name),
                "missing {name}"
            );
        }
        save_snapshot(&ctx, output);
    }
}

#[cfg(test)]
#[path = "labeling_labels_tests.rs"]
mod labels_tests;
