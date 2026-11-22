//! Independent Labeling pane. Rows carry stable identities, not just display names.

use super::panels::{label_cell, label_columns, label_row_pairs, LinkedLabelPick, LABELS_TWO_COL_MIN};
use super::theme;
use crate::label_increment::IncrementMode;
pub use super::panels::{MarkerList, SelectionGesture};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, Ui};
use tak_core::edit::LabelKey;
use tak_core::tak::{uid_hex, Uid};

const ROW_H: f32 = 20.0;
pub(crate) const LEGEND: &str = "Red = missing at this frame. % = share of take frames with a point.";
const REORDER_TWO_COL_MIN: f32 = 360.0;

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
    pub file_index: usize,
    pub moved: bool,
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
    pub order_scope: Option<String>,
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
            label_marker_reason: Some("3D에서 점을 하나만 선택해 주세요."),
            blocked_reason: None,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Reorder {
    pub enabled: bool,
    pub file_id: usize,
    pub frame: i64,
    pub preview_menu: bool,
    pub can_reset: bool,
    pub can_add: bool,
}

#[derive(Clone, Copy)]
struct RowDrag {
    file_id: usize,
    group: u64,
    file_index: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LabelingAction {
    SetReorder(bool),
    MoveLabel { group: u64, source: usize, boundary: usize },
    UseFileOrder,
    AddOpenNames,
    SetFilter(String),
    SetMode(LabelingMode),
    #[allow(dead_code, reason = "Keep the existing setting action when its control is hidden")]
    SetIncrement(IncrementMode),
    #[allow(dead_code, reason = "Keep the existing setting action when its control is hidden")]
    SetLinkTo3d(bool),
    #[allow(dead_code, reason = "Keep the existing setting action when its control is hidden")]
    SetBackward(bool),
    #[allow(dead_code, reason = "Keep the existing setting action when its control is hidden")]
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
    pub(crate) fn shown_asset(&self) -> Option<Uid> {
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

    #[cfg(test)]
    fn selection_summary(&self, reorder: bool) -> (String, String) {
        let selected: std::collections::HashSet<_> = self.groups.iter().flat_map(|g| &g.labels)
            .filter(|row| row.selected).filter_map(|row| row.track_uid)
            .chain(self.unlabeled.iter().filter(|row| row.selected).map(|row| row.uid)).collect();
        let mut visible = std::collections::HashSet::new();
        for row in list_rows(self, 1) {
            if let ListRow::Labels(group, index, _) = row {
                if let Some(uid) = self.groups[group].labels[index].track_uid { visible.insert(uid); }
            }
        }
        if !reorder {
            let needle = self.filter.trim().to_lowercase();
            visible.extend(self.unlabeled.iter().filter(|row| self.matches_row(Some(row.uid), &row.name, &needle)).map(|row| row.uid));
        }
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
            return Some("앞이나 뒤 방향을 하나 이상 선택해 주세요.");
        }
        (self.selected_labeled_count == 0).then_some("이름이 붙은 마커를 선택해 주세요.")
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
    moved: Option<bool>,
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
    let grip_width = if moved.is_some() { 16.0 } else { 0.0 };
    if moved.is_some() {
        for x in [8.0, 11.0] {
            for y in [-4.0, 0.0, 4.0] {
                p.circle_filled(pos2(rect.left() + x, rect.center().y + y), 0.85, theme::TEXT_DIM);
            }
        }
    }
    p.circle_filled(pos2(rect.left() + 12.0 + grip_width, rect.center().y), 4.0, dot);
    let color = if present {
        theme::LABEL_TRACKED
    } else {
        theme::LABEL_ABSENT
    };
    let percent = p.layout_no_wrap(coverage_text(fill_pct), FontId::proportional(12.5), theme::TEXT);
    let left = rect.left() + 24.0 + grip_width;
    let percent_r = rect.right() - 8.0;
    let percent_left = percent_r - 44.0_f32.max(percent.size().x + 6.0);
    let mut name_right = percent_left;
    if moved == Some(true) {
        let badge = p.layout_no_wrap("Moved".into(), FontId::proportional(10.0), theme::ACCENT);
        name_right -= badge.size().x + 5.0;
        p.galley(pos2(percent_left - badge.size().x, rect.center().y - badge.size().y * 0.5), badge, theme::ACCENT);
    }
    let name_width = (name_right - left).max(0.0);
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

fn draw_headers(ui: &Ui, rect: Rect, count: usize, reorder: bool) {
    let p = ui.painter();
    for col in 0..count {
        let r = label_cell(rect, col, count);
        p.text(
            pos2(r.left() + if reorder { 40.0 } else { 24.0 }, r.center().y),
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

fn reorder_target(ui: &Ui, resp: &egui::Response, row: RowDrag, index: usize) -> Option<LabelingAction> {
    if resp.drag_started_by(egui::PointerButton::Primary) {
        egui::DragAndDrop::set_payload(ui.ctx(), row);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    let source = resp.dnd_hover_payload::<RowDrag>()?;
    if source.file_id != row.file_id || source.group != row.group || source.file_index == row.file_index {
        return None;
    }
    let pointer = ui.input(|i| i.pointer.interact_pos())?;
    let after = pointer.y >= resp.rect.center().y;
    let y = if after { resp.rect.bottom() } else { resp.rect.top() };
    ui.painter().line_segment(
        [pos2(resp.rect.left(), y), pos2(resp.rect.right(), y)],
        Stroke::new(2.0, theme::ACCENT),
    );
    if !ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary)) {
        return None;
    }
    let source = resp.dnd_release_payload::<RowDrag>()?;
    Some(LabelingAction::MoveLabel {
        group: row.group,
        source: source.file_index,
        boundary: index + usize::from(after),
    })
}

fn order_menu(ui: &mut Ui, rect: Rect, reorder: Reorder, action: &mut Option<LabelingAction>) {
    let popup_id = ui.make_persistent_id("labeling_order_options");
    let response = ui.interact(rect, ui.make_persistent_id("order_options_button"), Sense::click());
    if reorder.preview_menu {
        ui.memory_mut(|memory| memory.open_popup(popup_id));
    } else if response.clicked() {
        ui.memory_mut(|memory| memory.toggle_popup(popup_id));
    }
    let open = ui.memory(|memory| memory.is_popup_open(popup_id));
    let fill = if open { theme::PICKED_ROW } else { ui.style().interact(&response).weak_bg_fill };
    ui.painter().rect_filled(response.rect, 2.0, fill);
    let mut anchor = response.clone();
    anchor.rect = Rect::from_min_max(
        pos2(response.rect.right() - 200.0, response.rect.bottom() + 12.0),
        pos2(response.rect.right(), response.rect.bottom() + 12.0),
    );
    egui::popup::popup_below_widget(
        ui, popup_id, &anchor, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            for (enabled, label, reason, next) in [
                (reorder.can_reset, "Use file order", "이미 파일 순서예요.", LabelingAction::UseFileOrder),
                (reorder.can_add, "Add names from open files", "열린 파일에 추가할 마커 이름이 없어요.", LabelingAction::AddOpenNames),
            ] {
                let item = ui.add_enabled(enabled, egui::Button::new(RichText::new(label).size(12.0))
                    .frame(false).min_size(vec2(0.0, 20.0)));
                if item.clicked() {
                    *action = Some(next);
                    ui.memory_mut(|memory| memory.close_popup());
                }
                item.on_disabled_hover_text(reason);
            }
        },
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Order options"));
    let center = response.rect.center();
    let at = |x, y| center + vec2(x, y);
    let stroke = Stroke::new(1.3_f32, theme::TEXT);
    let painter = ui.painter();
    for y in [-4.0, 0.0, 4.0] {
        painter.line_segment([at(-7.0, y), at(0.0, y)], stroke);
    }
    painter.line_segment([at(4.0, -5.0), at(4.0, 5.0)], stroke);
    painter.add(egui::Shape::line(vec![at(1.0, 2.0), at(4.0, 5.0), at(7.0, 2.0)], stroke));
    if !open { response.on_hover_text("Order options"); }
}

fn asset_picker(ui: &mut Ui, state: &LabelingState, width: f32, action: &mut Option<LabelingAction>) {
    let selected = state.shown_asset()
        .and_then(|id| state.assets.iter().find(|asset| asset.uid == id))
        .map(|asset| asset_name(asset, &state.assets))
        .unwrap_or_else(|| "All".to_owned());
    let response = ui.add_enabled_ui(!state.link_to_3d, |ui| {
        egui::ComboBox::from_id_salt("labeling_asset")
            .selected_text(RichText::new(&selected).size(12.0))
            .width(width)
            .truncate()
            .show_ui(ui, |ui| {
                if ui.selectable_label(state.asset_filter.is_none(), "All").clicked() {
                    *action = Some(LabelingAction::SetAssetFilter(None));
                }
                for asset in &state.assets {
                    if ui.selectable_label(state.asset_filter == Some(asset.uid), asset_name(asset, &state.assets)).clicked() {
                        *action = Some(LabelingAction::SetAssetFilter(Some(asset.uid)));
                    }
                }
            })
    }).inner.response;
    response.on_hover_text(selected).on_disabled_hover_text("3D 선택에 맞춰 표시하고 있어요.");
}

fn search(ui: &mut Ui, state: &LabelingState, width: f32, action: &mut Option<LabelingAction>) {
    let mut filter = state.filter.clone();
    let response = ui.add_sized([width, 24.0], egui::TextEdit::singleline(&mut filter)
        .id(ui.make_persistent_id("labeling_search"))
        .font(FontId::proportional(12.0))
        .hint_text("Search")
        .min_size(vec2(0.0, 24.0))
        .margin(egui::Margin { left: 26.0, right: 6.0, top: 4.0, bottom: 4.0 }));
    // egui 0.29 returns the text rect, inside the field's 26 pt leading margin.
    let center = pos2(response.rect.left() - 13.0, response.rect.center().y - 1.0);
    let stroke = Stroke::new(1.0_f32, theme::TEXT_DIM);
    ui.painter().circle_stroke(center, 2.5, stroke);
    ui.painter().line_segment([center + vec2(2.0, 2.0), center + vec2(4.0, 4.0)], stroke);
    if response.changed() {
        *action = Some(LabelingAction::SetFilter(filter));
    }
}

fn labeling_header(ui: &mut Ui, state: &LabelingState, reorder: Reorder, action: &mut Option<LabelingAction>) {
    let narrow = ui.available_width() < 400.0;
    let mode_size = if narrow { 10.0 } else { 11.0 };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.spacing_mut().interact_size.y = 24.0;
        if narrow {
            ui.spacing_mut().button_padding.x = 2.0;
        }
        let wide = ui.available_width() >= 464.0;
        let min_size = |width| vec2(if wide { width } else { 0.0 }, 24.0);
        for (mode, caption, width) in [
            (LabelingMode::QuickLabel, "QuickLabel Mode  D", 128.0),
            (LabelingMode::Select, "Select Mode  Q", 104.0),
        ] {
            let button = egui::Button::new(RichText::new(caption).size(mode_size))
                .frame(!reorder.enabled && state.mode == mode)
                .selected(!reorder.enabled && state.mode == mode)
                .min_size(min_size(width));
            let response = ui.add_enabled(!reorder.enabled, button);
            if response.clicked() {
                *action = Some(LabelingAction::SetMode(mode));
            }
            response.on_disabled_hover_text("순서 변경을 끝낸 뒤 모드를 바꿀 수 있어요.");
        }
        let why = if reorder.enabled {
            Some("순서 변경을 끝낸 뒤 라벨을 지울 수 있어요.")
        } else {
            state.unlabel_reason()
        };
        let unlabel = ui.add_enabled(why.is_none(), egui::Button::new(RichText::new("Unlabel  Shift+D").size(mode_size)).min_size(min_size(112.0)));
        if let Some(why) = why {
            unlabel.on_disabled_hover_text(why);
        } else if unlabel.clicked() {
            *action = Some(LabelingAction::Unlabel);
        }
        let toggle = if reorder.enabled {
            egui::Button::new(RichText::new("Reorder: On").size(mode_size)).fill(theme::PICKED_ROW)
        } else {
            egui::Button::new(RichText::new("Reorder: Off").size(mode_size))
        };
        if ui.add(toggle.min_size(min_size(96.0))).clicked() {
            *action = Some(LabelingAction::SetReorder(!reorder.enabled));
        }
    });
    ui.horizontal(|ui| {
        let total: usize = state.groups.iter().map(|group| group.labels.len()).sum();
        let present = state.groups.iter().flat_map(|group| &group.labels)
            .filter(|row| row.track_uid.is_some() && row.present).count();
        ui.label(RichText::new("Labeled").size(14.0));
        let coverage = format!("{present} / {total} at frame {}", reorder.frame);
        ui.label(RichText::new(coverage).color(theme::TEXT_DIM).size(11.0));
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().interact_size.y = 24.0;
        let width = ui.available_width();
        let gap = (width * (17.0 / 479.0)).clamp(8.0, 17.0);
        let search_width = (width * (182.0 / 479.0)).clamp(60.0, 182.0);
        ui.label(RichText::new("Selected").size(12.0).color(theme::TEXT_DIM));
        let left = ui.available_width() - search_width - 24.0 - gap - 8.0;
        asset_picker(ui, state, left.clamp(28.0, 96.0), action);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (slot, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
            if reorder.enabled {
                order_menu(ui, slot, reorder, action);
            }
            ui.add_space(gap);
            search(ui, state, search_width.min(ui.available_width()), action);
        });
    });
}

fn group_title<'a>(state: &LabelingState, group: &'a LabelGroup) -> &'a str {
    let scope = group.order_scope.as_deref().unwrap_or_default();
    let mut title = group.title.as_str();
    if state.assets.len() == 1 && (scope == "extra" || scope.starts_with("set:")) {
        let asset = &state.assets[0];
        if group.asset_uid == Some(asset.uid) {
            title = title.strip_prefix(&asset.name)
                .and_then(|rest| rest.strip_prefix(" · "))
                .unwrap_or(title);
        }
    }
    if scope.starts_with("set:") {
        if let Some((name, count)) = title.rsplit_once(" (") {
            if let Some(count) = count.strip_suffix(')') {
                if !count.is_empty() && count.bytes().all(|c| c.is_ascii_digit()) {
                    return name;
                }
            }
        }
    }
    title
}

fn group_header(ui: &Ui, rect: Rect, title: &str, total: usize, collapsed: bool) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, theme::HEADER_BG);
    let center = pos2(rect.left() + 10.0, rect.center().y);
    let chevron = if collapsed {
        [vec2(-1.0, -2.5), vec2(1.5, 0.0), vec2(-1.0, 2.5)]
    } else {
        [vec2(-2.5, -1.0), vec2(0.0, 1.5), vec2(2.5, -1.0)]
    };
    painter.add(egui::Shape::line(chevron.map(|p| center + p).to_vec(), Stroke::new(1.0_f32, theme::TEXT_DIM)));
    let total = painter.text(
        pos2(rect.right() - 8.0, center.y), Align2::RIGHT_CENTER,
        total, FontId::proportional(11.0), theme::TEXT_DIM,
    );
    painter.with_clip_rect(Rect::from_min_max(rect.min, pos2(total.left() - 6.0, rect.bottom())))
        .text(pos2(rect.left() + 24.0, center.y), Align2::LEFT_CENTER,
            title, FontId::proportional(12.0), theme::TEXT);
}

/// Draw the pane and return at most one user action. The caller applies selection and edits.
pub(crate) fn show(ui: &mut Ui, state: &LabelingState, reorder: Reorder) -> Option<LabelingAction> {
    let pane_width = ui.available_width();
    // Use the same inset in both modes so the Search field keeps its position.
    let inset = ((pane_width - LABELS_TWO_COL_MIN) * 0.5).clamp(0.0, 16.0);
    egui::Frame::none().inner_margin(egui::Margin::symmetric(inset, 16.0))
        .show(ui, |ui| show_content(ui, state, reorder, pane_width)).inner
}

fn show_content(ui: &mut Ui, state: &LabelingState, reorder: Reorder, pane_width: f32) -> Option<LabelingAction> {
    let mut action = None;
    let panel_height = ui.available_height();
    egui::ScrollArea::vertical()
        .id_salt("labeling_pane")
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            let panel_top = ui.cursor().min.y;
            labeling_header(ui, state, reorder, &mut action);
            let width = ui.available_width();
            let count = if reorder.enabled && pane_width < REORDER_TWO_COL_MIN { 1 } else { label_columns(width) };
            let (header, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
            let rows = list_rows(state, count);
            // Reserve the measured legend and the unlabeled section before filling the list.
            let legend = ui.painter().layout(
                LEGEND.to_owned(),
                FontId::proportional(10.0), theme::TEXT_DIM, ui.available_width(),
            );
            let needle = state.filter.trim().to_lowercase();
            let unlabeled: Vec<_> = if reorder.enabled {
                Vec::new()
            } else {
                state.unlabeled.iter().enumerate()
                    .filter(|(_, row)| state.matches_row(Some(row.uid), &row.name, &needle))
                    .map(|(index, _)| index).collect()
            };
            let pitch = ROW_H;
            let unlabeled_height = (unlabeled.len() as f32 * pitch).min(120.0);
            let footer = if reorder.enabled { 24.0 + ui.spacing().item_spacing.y } else { 0.0 };
            let reserve = legend.size().y + ROW_H + unlabeled_height + footer + 6.0 * ui.spacing().item_spacing.y;
            let height = (panel_height - (ui.cursor().min.y - panel_top) - reserve).max(40.0);
            let mut label_scroll = egui::ScrollArea::vertical()
                .id_salt("labeling_labels")
                .max_height(height)
                .auto_shrink([false, true]);
            if let Some(offset) = label_scroll_offset(&rows, state, pitch) {
                label_scroll = label_scroll.vertical_scroll_offset(offset);
            }
            ui.spacing_mut().item_spacing.y = 0.0;
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
                            group_header(ui, rect, group_title(state, group), group.labels.len(),
                                group.collapsed && needle.is_empty());
                            if resp.clicked() {
                                action = Some(LabelingAction::ToggleGroup(group.id));
                            }
                        }
                        ListRow::Labels(gi, left, right) => {
                            for (col, index) in [Some(left), right].into_iter().enumerate() {
                                let Some(index) = index else { continue };
                                let row = &state.groups[gi].labels[index];
                                let draggable = reorder.enabled && state.groups[gi].order_scope.is_some();
                                let rect = label_cell(rect, col, count);
                                let resp = ui.interact(
                                    rect,
                                    ui.make_persistent_id(("label", row.key, row.track_uid)),
                                    if draggable { Sense::click_and_drag() } else if reorder.enabled { Sense::hover() } else { Sense::click() },
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
                                    !reorder.enabled && row.key.is_some() && state.armed == row.key,
                                    resp.hovered(),
                                    (first + offset) % 2 == 1,
                                    draggable.then_some(row.moved),
                                );
                                if draggable {
                                    let drag = RowDrag {
                                        file_id: reorder.file_id,
                                        group: state.groups[gi].id,
                                        file_index: row.file_index,
                                    };
                                    if let Some(next) = reorder_target(ui, &resp, drag, index) {
                                        action = Some(next);
                                    }
                                }
                                if reorder.enabled { continue; }
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
                                        Some("이 행에 지정할 수 있는 이름이 없어요.")
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
                                    let remove_reason = if row.track_uid.is_none() {
                                        Some("이 라벨에 연결된 마커가 없어요.")
                                    } else {
                                        state.blocked_reason.as_deref()
                                    };
                                    let remove = ui.add_enabled(remove_reason.is_none(), egui::Button::new("Remove Marker"));
                                    if let Some(reason) = remove_reason {
                                        remove.on_disabled_hover_text(reason);
                                    } else if remove.clicked() {
                                        action = Some(context_track_action(row, false));
                                        ui.close_menu();
                                    }
                                    let why = if row.track_uid.is_none() {
                                        Some("이 라벨에 연결된 마커가 없어요.")
                                    } else if row.selected {
                                        state.unlabel_reason()
                                    } else if row.key.is_none() {
                                        Some("이 마커의 라벨을 찾을 수 없어요.")
                                    } else {
                                        state.blocked_reason.as_deref()
                                    };
                                    let unlabel = ui.add_enabled(why.is_none(), egui::Button::new("Unlabel  Shift+D"));
                                    if let Some(why) = why {
                                        unlabel.on_disabled_hover_text(why);
                                    } else if unlabel.clicked() {
                                        action = Some(context_track_action(row, true));
                                        ui.close_menu();
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
            ui.spacing_mut().item_spacing.y = 8.0;
            use crate::app::key_focus::{region, Target};
            region(ui, list.inner_rect, if reorder.enabled { Target::Blocked } else { Target::NamedMarkers(state.view_slot) });
            draw_headers(
                ui,
                Rect::from_min_max(header.min, pos2(list.inner_rect.right(), header.bottom())),
                count,
                reorder.enabled,
            );
            ui.add_space(16.0);
            let (rect, _) = ui.allocate_exact_size(legend.size(), Sense::hover());
            crate::app::key_focus::region(ui, rect, crate::app::key_focus::Target::Blocked);
            ui.painter().galley(rect.min, legend, theme::TEXT_DIM);
            if reorder.enabled {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Changes apply to all files.").size(11.0).color(theme::TEXT_DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_sized([64.0, 24.0], egui::Button::new(RichText::new("Done").size(11.0))).clicked() {
                            action = Some(LabelingAction::SetReorder(false));
                        }
                    });
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Unlabeled Markers").size(12.0).color(theme::TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("%").color(theme::TEXT_DIM));
                });
            });
            if reorder.enabled { return; }
            let mut unlabeled_scroll = egui::ScrollArea::vertical()
                .id_salt("labeling_unlabeled")
                .max_height(120.0)
                .auto_shrink([false, true]);
            if let Some(index) = state.scroll_to_track.and_then(|uid| unlabeled.iter().position(|&i| state.unlabeled[i].uid == uid)) {
                unlabeled_scroll = unlabeled_scroll.vertical_scroll_offset(((index as f32 - 3.0) * pitch).max(0.0));
            }
            ui.spacing_mut().item_spacing.y = 0.0;
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
                            None,
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
                            let remove = ui.add_enabled(state.blocked_reason.is_none(), egui::Button::new("Remove Marker"));
                            if let Some(reason) = state.blocked_reason.as_deref() {
                                remove.on_disabled_hover_text(reason);
                            } else if remove.clicked() {
                                action = Some(LabelingAction::RemoveMarker);
                                ui.close_menu();
                            }
                        }); }
                    }
                });
            region(ui, unlabeled_list.inner_rect, Target::UnlabeledMarkers(state.view_slot));
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
            file_index: usize::from(index),
            moved: false,
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
                order_scope: Some("set:Asset".into()),
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
        draw_with_reorder(ctx, state, width, events, Reorder::default())
    }

    fn draw_with_reorder(
        ctx: &egui::Context,
        state: &LabelingState,
        width: f32,
        events: Vec<Event>,
        reorder: Reorder,
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
                action = show(ui, state, reorder);
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
    fn reorder_drags_between_columns_and_ignores_row_clicks_and_other_groups() {
        let ctx = egui::Context::default();
        let mut st = state(5);
        st.groups[0].labels.swap(0, 1);
        crate::marker_order::mark_moved(&mut st.groups[0].labels);
        st.groups.push(LabelGroup {
            id: 2,
            asset_uid: None,
            order_scope: Some("extra".into()),
            title: "Extra Markers".into(),
            collapsed: false,
            labels: vec![LabelRow { file_index: 0, ..label(9, true) }],
        });
        let reorder = Reorder { enabled: true, file_id: 3, ..Default::default() };
        let draw = |events| draw_with_reorder(&ctx, &st, 440.0, events, reorder);
        let (output, _) = draw(vec![]);
        let painted = texts(&output);
        assert_eq!(painted.iter().filter(|item| item.text == "Moved").count(), 1);
        assert!(painted.iter().all(|item| item.text != "Unlabeled 99"));
        let point = |name| painted.iter().find(|item| item.text == name).unwrap().rect.center();
        let source = point("L03");
        let first = point("L01") - vec2(0.0, 4.0);
        assert!(source.x > first.x, "drag starts in the right column");
        let button = |pos, pressed| Event::PointerButton {
            pos, pressed, button: PointerButton::Primary, modifiers: Modifiers::NONE,
        };
        draw(vec![Event::PointerMoved(source)]);
        assert_eq!(draw(vec![button(source, true)]).1, None);
        assert_eq!(draw(vec![button(source, false)]).1, None);
        for (target, expected) in [
            (point("L09"), None),
            (first, Some(LabelingAction::MoveLabel { group: 1, source: 3, boundary: 0 })),
        ] {
            draw(vec![Event::PointerMoved(source)]);
            assert_eq!(draw(vec![button(source, true)]).1, None);
            draw(vec![Event::PointerMoved(target)]);
            assert_eq!(draw(vec![button(target, false)]).1, expected);
        }
        let done = point("Done");
        draw(vec![Event::PointerMoved(done)]);
        draw(vec![button(done, true)]);
        assert_eq!(draw(vec![button(done, false)]).1, Some(LabelingAction::SetReorder(false)));
    }

    #[test]
    fn order_menu_aligns_to_icon_and_dispatches_both_actions() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        ctx.style_mut(|style| style.animation_time = 0.0);
        let st = state(5);
        let reorder = Reorder {
            enabled: true,
            can_reset: true,
            can_add: true,
            ..Default::default()
        };
        let draw = |reorder, events| draw_with_reorder(&ctx, &st, 515.0, events, reorder);
        let click_at = |reorder, pos| {
            let button = |pressed| Event::PointerButton {
                pos, pressed, button: PointerButton::Primary, modifiers: Modifiers::NONE,
            };
            draw(reorder, vec![Event::PointerMoved(pos)]);
            draw(reorder, vec![button(true)]);
            draw(reorder, vec![button(false)]).1
        };

        let (out, _) = draw(reorder, vec![]);
        let options = out.shapes.iter().find_map(|s| match &s.shape {
            egui::Shape::Path(path) if path.points.len() == 3 && path.stroke.width == 1.3
                && path.points[0].x < path.points[1].x && path.points[1].x < path.points[2].x =>
            {
                Some(path.points[1] - vec2(4.0, 5.0))
            }
            _ => None,
        }).expect("painted down arrow");
        let search = texts(&out).into_iter().find(|t| t.text == "Search").unwrap();
        assert!(options.x > 470.0 && options.x > search.rect.right());
        assert!((options.y - search.rect.center().y).abs() < 2.0);
        for (item, expected) in [
            ("Use file order", LabelingAction::UseFileOrder),
            ("Add names from open files", LabelingAction::AddOpenNames),
        ] {
            assert!(!texts(&draw(reorder, vec![]).0).iter().any(|t| t.text == item));
            assert_eq!(click_at(reorder, options), None);
            let (out, _) = draw(reorder, vec![]);
            let picked = out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r)
                if r.fill == theme::PICKED_ROW && r.rect.center().distance(options) < 1.0));
            assert!(picked, "open Order options icon is highlighted");
            let popup = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Vec(shapes) => Some(shapes),
                _ => None,
            }).find_map(|shapes| match shapes.as_slice() {
                [_, egui::Shape::Rect(frame)] if frame.rect.top() > options.y
                    && (frame.rect.width() - 200.0).abs() < 3.0 => Some(frame.rect),
                _ => None,
            }).expect("floating menu frame with shadow");
            assert!((popup.right() - (options.x + 12.0)).abs() < 2.0, "{popup:?} vs {options:?}");
            let pos = texts(&out).iter().find(|t| t.text == item)
                .unwrap_or_else(|| panic!("missing {item} in Order options")).rect.center();
            assert_eq!(click_at(reorder, pos), Some(expected));
        }
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
    fn search_filters_both_lists_but_totals_include_declarations_and_ignore_filters() {
        let ctx = egui::Context::default();
        let mut st = state(5);
        st.groups[0].labels[4].track_uid = None;
        st.groups[0].labels[4].present = false;
        st.filter = "unlabeled".into();
        let out = draw(&ctx, &st, 440.0, vec![]).0;
        let painted = texts(&out);
        assert!(painted.iter().any(|t| t.text == "3 / 5 at frame 0"));
        assert!(painted.iter().any(|t| t.text == "Unlabeled 99"));
        assert!(!painted.iter().any(|t| t.text == "L00"));
        st.filter = "L00".into();
        let out = draw(&ctx, &st, 440.0, vec![]).0;
        let painted = texts(&out);
        assert!(painted.iter().any(|t| t.text == "3 / 5 at frame 0"));
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
    fn group_totals_include_declarations_and_global_presence_counts_actual_tracks() {
        let ctx = egui::Context::default();
        let mut st = state(5);
        st.groups[0].labels[4].track_uid = None;
        st.groups[0].labels[4].present = false;
        let out = draw(&ctx, &st, 440.0, vec![]).0;
        let painted = texts(&out);
        for coverage in ["5", "3 / 5 at frame 0"] {
            assert!(painted.iter().any(|t| t.text == coverage), "missing {coverage}");
        }
        assert!(!painted.iter().any(|t| t.text == "3 / 4"));
    }

    #[test]
    fn headless_clicks_return_stable_ids() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let st = state(5);
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
            order_scope: None,
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
    fn mode_button_still_dispatches_selection_mode() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let st = state(5);
        assert_eq!(
            click(&ctx, &st, "Select Mode  Q", Modifiers::NONE, PointerButton::Primary),
            Some(LabelingAction::SetMode(LabelingMode::Select))
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
            "Search",
            "Unlabeled Markers",
        ] {
            assert!(
                labels.iter().any(|item| item.text == name),
                "missing {name}"
            );
        }
        save_snapshot(&ctx, output);
    }

    #[test]
    fn mode_buttons_and_search_fit_the_default_dock_width() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let st = state(5);
        // The central panel's padding is 8 pt on each side.
        let geometry = |enabled| {
            let (out, _) = draw_with_reorder(&ctx, &st, 376.0 + 16.0, vec![],
                Reorder { enabled, ..Default::default() });
            let painted = texts(&out);
            let at = |text| painted.iter().find(|item| item.text == text)
                .unwrap_or_else(|| panic!("missing {text}")).rect;
            let mode = if enabled { "Reorder: On" } else { "Reorder: Off" };
            assert!((at("QuickLabel Mode  D").center().y - at(mode).center().y).abs() < 1.0);
            assert!((at("Selected").center().y - at("Search").center().y).abs() < 2.0);
            assert!(at("Search").left() > at("All").right() + 20.0);
            assert!(at("Search").top() > at("4 / 5 at frame 0").bottom());
            assert!(at(mode).right() <= 376.0);
            for hidden in ["A Prev", "S Next", "Increment", "Back", "Fwd", "Link to 3D Selection",
                "Pattern target", "selected (", "Undo", "Redo"] {
                assert!(!painted.iter().any(|item| item.text.contains(hidden)), "unexpected {hidden}");
            }
            assert_eq!(painted.iter().any(|item| item.text == "Done"), enabled);
            let grip_dots = out.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Circle(c)
                if c.radius == 0.85 && c.center.y >= at("L00").top() - 4.0)).count();
            assert_eq!(grip_dots, if enabled { 30 } else { 0 });
            (at("Search"), at("L00").left())
        };
        let (off_search, off_name) = geometry(false);
        let (on_search, on_name) = geometry(true);
        assert_eq!(off_search, on_search);
        assert!((on_name - off_name - 16.0).abs() < 1.0);
    }

    #[test]
    fn group_header_separates_the_total_from_the_title_even_with_search() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut st = state(39);
        st.groups[0].title = "Asset · Conventional Full Body (39)".into();
        st.groups[0].labels[2].track_uid = None;
        st.filter = "L01".into();
        for (title, second_asset) in [
            ("Conventional Full Body", false),
            ("Asset · Conventional Full Body", true),
        ] {
            if second_asset {
                st.assets.push(AssetChoice { uid: [8; 16], name: "Second".into() });
                st.asset_filter = Some([7; 16]);
            }
            let (out, _) = draw(&ctx, &st, 511.0 + 16.0, vec![]);
            let painted = texts(&out);
            let at = |text| painted.iter().find(|item| item.text == text)
                .unwrap_or_else(|| panic!("missing {text}")).rect;
            assert!((at(title).center().y - at("39").center().y).abs() < 2.0);
            let header_right = painted.iter().filter(|item| item.text == "%"
                && (item.rect.center().y - at("Labels").center().y).abs() < 1.0)
                .map(|item| item.rect.right()).fold(0.0, f32::max);
            assert!((at("39").right() - header_right).abs() < 1.0, "{:?} vs {header_right}", at("39"));
            assert!(at("39").left() > at(title).right());
            assert!(!painted.iter().any(|item| item.text.contains("(39)")));
            assert!(painted.iter().any(|item| item.text == "38 / 39 at frame 0"));
        }
    }

    #[test]
    fn disabled_unlabel_reason_is_visible_only_on_hover() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        ctx.style_mut(|style| style.interaction.tooltip_delay = 0.0);
        let mut st = state(5);
        st.blocked_reason = Some("편집 작업이 끝날 때까지 기다려 주세요.".into());
        let why = st.unlabel_reason().unwrap();
        let (out, _) = draw(&ctx, &st, 440.0, vec![]);
        let painted = texts(&out);
        assert!(!painted.iter().any(|item| item.text == why));
        let pos = painted.iter().find(|item| item.text == "Unlabel  Shift+D").unwrap().rect.center();
        draw(&ctx, &st, 440.0, vec![Event::PointerMoved(pos)]);
        let (out, _) = draw(&ctx, &st, 440.0, vec![]);
        assert!(texts(&out).iter().any(|item| item.text == why));
    }

}

#[cfg(test)]
#[path = "labeling_labels_tests.rs"]
mod labels_tests;
