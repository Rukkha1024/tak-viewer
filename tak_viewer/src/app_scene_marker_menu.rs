use super::TakViewerApp;
use crate::ui::labeling::LabelGroup;
use eframe::egui::{self, menu, Button, Pos2, Response, RichText, ScrollArea, Ui};
use tak_core::edit::LabelKey;
use tak_core::tak::{MarkerKind, Uid};
use tak_core::view_data::{GroupKind, MarkerView, ViewData};

#[derive(Default)]
pub(super) struct Menu {
    root: menu::MenuRootManager,
    owner: Option<(usize, usize)>,
    catalog: Option<Vec<CatalogRow>>,
    pending: Option<(usize, usize, Action)>,
    drawn: bool,
    shot_hover: Option<Pos2>,
    shot_visible_since: Option<f64>,
}

impl Menu {
    pub(super) fn is_open(&self) -> bool {
        self.root.is_some()
    }
}

enum Action {
    Label(Uid, LabelKey),
    Unlabel,
    Remove,
}

struct Selection {
    count: usize,
    source: Option<Uid>,
    named: bool,
}

impl Selection {
    fn read(data: &ViewData, selected: &std::collections::BTreeSet<usize>) -> Self {
        let mut markers = selected.iter().filter_map(|&index| data.markers.get(index));
        let first = markers.next();
        let source = first
            .filter(|_| selected.len() == 1)
            .map(|marker| marker.uid);
        let named = first
            .into_iter()
            .chain(markers)
            .any(|marker| label_key(marker, data.groups[marker.group].kind).is_some());
        Self {
            count: selected.len(),
            source,
            named,
        }
    }
}

pub(super) fn label_key(marker: &MarkerView, group: GroupKind) -> Option<LabelKey> {
    if marker.kind == MarkerKind::Unlabeled || group == GroupKind::Unlabeled {
        return None;
    }
    let (parent, slot) = marker.parent.filter(|p| *p != [0; 16]).zip(marker.slot)?;
    Some(LabelKey { parent, slot })
}

enum CatalogRow {
    Group(String),
    Label(String, LabelKey),
}

fn label_catalog(groups: &[LabelGroup]) -> Vec<CatalogRow> {
    let mut rows = Vec::new();
    for group in groups {
        let mut labels = group
            .labels
            .iter()
            .filter_map(|row| Some(CatalogRow::Label(row.name.clone(), row.key?)))
            .peekable();
        if labels.peek().is_some() {
            rows.push(CatalogRow::Group(group.title.clone()));
            rows.extend(labels);
        }
    }
    rows
}

fn show_labels(ui: &mut Ui, rows: &[CatalogRow]) -> Option<LabelKey> {
    const WIDTH: f32 = 200.0;
    ui.set_width(WIDTH);
    if rows.is_empty() {
        ui.label("No labels to assign");
        return None;
    }
    let row_height = ui.spacing().interact_size.y;
    ScrollArea::vertical()
        .max_height(285.0)
        .show_rows(ui, row_height, rows.len(), |ui, visible| {
            let mut label = None;
            for row in &rows[visible] {
                match row {
                    CatalogRow::Group(name) => {
                        ui.add_sized(
                            [ui.available_width(), row_height],
                            egui::Label::new(RichText::new(name).strong()).truncate(),
                        );
                    }
                    CatalogRow::Label(name, key) => {
                        let response = ui.add_sized(
                            [ui.available_width(), row_height],
                            Button::new(name).truncate(),
                        );
                        if response.on_hover_text(name).clicked() {
                            label = Some(*key);
                            ui.close_menu();
                        }
                    }
                }
            }
            label
        })
        .inner
}

fn marker_info(ui: &mut Ui, count: usize, marker: &MarkerView, frame: i64) {
    if count > 1 {
        ui.label(format!("{count} selected · last picked"));
    }
    let name = if marker.kind == MarkerKind::Unlabeled {
        "Unlabeled"
    } else {
        &marker.name
    };
    ui.label(RichText::new(name).strong());
    if let Some(p) = marker.position(frame) {
        for (axis, value) in [("X", p.x), ("Y", p.y), ("Z", p.z)] {
            ui.label(format!("{axis}  {:.2} mm", value * 1000.0));
        }
    } else {
        ui.label("No point at this frame");
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shot {
    Menu,
    Info,
    Label,
}

static SHOT: std::sync::OnceLock<Shot> = std::sync::OnceLock::new();

pub(crate) fn take_probe(args: &mut Vec<String>, headless: bool) -> Result<(), &'static str> {
    let Some(i) = args.iter().position(|arg| arg == "--shot-marker-menu") else {
        return Ok(());
    };
    let shot = match args.get(i + 1).map(String::as_str) {
        Some("menu") => Shot::Menu,
        Some("info") => Shot::Info,
        Some("label") => Shot::Label,
        _ => return Err("--shot-marker-menu: use menu, info or label"),
    };
    if !headless
        || !args.iter().any(|arg| arg == "--screenshot")
        || !args.iter().any(|arg| arg == "--select")
        || args
            .iter()
            .filter(|arg| *arg == "--shot-marker-menu")
            .count()
            != 1
    {
        return Err("--shot-marker-menu requires --headless --screenshot and --select");
    }
    let _ = SHOT.set(shot);
    args.drain(i..i + 2);
    Ok(())
}

impl TakViewerApp {
    pub(super) fn draw_scene_marker_menu(
        &mut self,
        slot: usize,
        response: &Response,
        picked: Option<usize>,
    ) {
        let ctx = &response.ctx;
        let file_id = self.views[slot].file_id;
        let shot = SHOT.get().copied().filter(|_| {
            slot == self.active && self.auto.as_ref().is_some_and(|auto| auto.stage >= 6)
        });
        if picked.is_none() && shot.is_none() && !self.scene_marker_menu.is_open() {
            return;
        }
        let blocked = self.capture.is_some()
            || self.hold.is_some()
            || self.comment_edit.is_some()
            || self.dirty_prompt.is_some()
            || self.typing(ctx)
            || ctx.memory(|m| m.any_popup_open())
            || ctx.input(|input| !input.focused || input.key_down(egui::Key::M));
        if blocked {
            self.scene_marker_menu.root = menu::MenuRootManager::default();
            return;
        }
        let mut root = std::mem::take(&mut self.scene_marker_menu.root);
        if let Some((index, pos)) = picked.zip(response.interact_pointer_pos()) {
            let view = &mut self.views[slot];
            if !view.selected.contains(&index) {
                view.select(index, false);
            }
            self.scene_marker_menu.owner = Some((slot, file_id));
            self.scene_marker_menu.catalog = None;
            *root = Some(menu::MenuRoot::new(pos, response.id));
        } else if root.is_none() && shot.is_some() {
            let view = &self.views[slot];
            let marker = view
                .primary
                .and_then(|i| view.data.as_ref()?.markers.get(i));
            let pos = marker
                .and_then(|marker| {
                    view.cam
                        .view(view.scene_rect)
                        .project(marker.position(view.pb.frame)?)
                        .map(|p| p.0)
                })
                .filter(|p| view.scene_rect.contains(*p));
            if let Some(pos) = pos {
                *root = Some(menu::MenuRoot::new(pos, response.id));
                self.scene_marker_menu.owner = Some((slot, file_id));
            } else {
                self.fail_scene_menu_shot(ctx, "select a point visible in the active 3D view");
            }
        } else if self.scene_marker_menu.owner == Some((slot, file_id)) {
            if response.secondary_clicked() {
                root = menu::MenuRootManager::default();
            } else {
                menu::MenuRoot::context_click_interaction(response, &mut root);
            }
        }
        if root.is_none() || self.scene_marker_menu.owner != Some((slot, file_id)) {
            self.scene_marker_menu.root = root;
            return;
        }

        let view = &self.views[slot];
        let data = view.data.as_ref().unwrap().clone();
        let selection = Selection::read(&data, &view.selected);
        let primary = view.primary.filter(|index| view.selected.contains(index));
        let frame = view.pb.frame;
        let labeling = &self.marker_ui[slot].labeling;
        let can_edit = (labeling.backward || labeling.forward)
            && (!self.no_persist || shot.is_some())
            && view.edit_document.is_some()
            && view.edit_refresh.is_none()
            && view.loading.is_none()
            && view.remote_loading.is_none()
            && view.waiting_shared_path().is_none()
            && view.analog_rx.is_none()
            && !data.analog_pending
            && !self.marker_job_running(slot)
            && !self.marker_saving(slot);
        let mut catalog = self.scene_marker_menu.catalog.take();
        root.show(response, |ui| {
            self.scene_marker_menu.drawn = true;
            ui.label(RichText::new(format!("Selected Markers ({})", selection.count)).strong());
            ui.separator();
            let info = ui
                .add_enabled_ui(primary.is_some(), |ui| {
                    ui.menu_button("Marker Info", |ui| {
                        if let Some(marker) = primary.and_then(|index| data.markers.get(index)) {
                            marker_info(ui, selection.count, marker, frame);
                        }
                    })
                })
                .inner;
            let label = ui
                .add_enabled_ui(can_edit && selection.source.is_some(), |ui| {
                    ui.menu_button("Label Marker", |ui| {
                        if !can_edit || selection.source.is_none() {
                            ui.close_menu();
                            return None;
                        }
                        let rows = catalog.get_or_insert_with(|| {
                            self.refresh_marker_ui(slot);
                            label_catalog(&self.marker_ui[slot].labeling.groups)
                        });
                        show_labels(ui, rows)
                    })
                })
                .inner;

            let action = if let Some((uid, key)) = selection.source.zip(label.inner.flatten()) {
                Some(Action::Label(uid, key))
            } else if ui
                .add_enabled(
                    can_edit && selection.named,
                    Button::new("Unlabel Marker(s)"),
                )
                .clicked()
            {
                Some(Action::Unlabel)
            } else if ui.button("Remove Marker").clicked() {
                Some(Action::Remove)
            } else {
                None
            };
            if let Some(action) = action {
                self.scene_marker_menu.pending = Some((slot, file_id, action));
                ui.close_menu();
            }

            let shot_ready = match shot {
                Some(Shot::Info) => info.inner.is_some(),
                Some(Shot::Label) => label.inner.is_some(),
                Some(Shot::Menu) => true,
                None => false,
            };
            if shot_ready {
                self.scene_marker_menu
                    .shot_visible_since
                    .get_or_insert_with(|| ctx.input(|input| input.time));
            } else {
                self.scene_marker_menu.shot_visible_since = None;
            }
            self.scene_marker_menu.shot_hover = match shot {
                Some(Shot::Info) if !shot_ready => Some(info.response.rect.center()),
                Some(Shot::Label) if !shot_ready && can_edit && selection.source.is_some() => {
                    Some(label.response.rect.center())
                }
                Some(Shot::Label) if !can_edit || selection.source.is_none() => {
                    self.fail_scene_menu_shot(
                        ctx,
                        "Label Marker needs one selected point and a ready file",
                    );
                    None
                }
                _ => None,
            };
        });
        self.scene_marker_menu.catalog = catalog;
        self.scene_marker_menu.root = root;
    }

    pub(super) fn apply_scene_marker_menu(&mut self, ctx: &egui::Context) {
        if !std::mem::take(&mut self.scene_marker_menu.drawn) || !self.scene_marker_menu.is_open() {
            self.scene_marker_menu.root = menu::MenuRootManager::default();
            self.scene_marker_menu.catalog = None;
        }
        if let Some((slot, file_id, action)) = self.scene_marker_menu.pending.take() {
            if self.views[slot].file_id != file_id || self.dirty_prompt.is_some() {
                return;
            }
            match action {
                Action::Label(uid, key) => self.assign_point_label(ctx, slot, uid, key, true),
                Action::Unlabel => self.unlabel_selected(ctx, slot),
                Action::Remove => self.delete_selected(ctx, slot),
            }
        }
    }

    pub(super) fn wait_for_scene_menu_shot(&self, ctx: &egui::Context) -> bool {
        // The real egui Areas size on their first frame and then fade in.
        if SHOT.get().is_some()
            && self
                .scene_marker_menu
                .shot_visible_since
                .is_none_or(|since| {
                    ctx.input(|input| input.time)
                        < since + 2.0 * f64::from(ctx.style().animation_time)
                })
        {
            ctx.request_repaint();
            return true;
        }
        false
    }

    pub(crate) fn scene_menu_shot_hover(&self) -> Option<Pos2> {
        self.scene_marker_menu.shot_hover
    }

    fn fail_scene_menu_shot(&mut self, ctx: &egui::Context, message: &str) {
        eprintln!("--shot-marker-menu: {message}");
        self.failed = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

#[cfg(test)]
#[path = "app_scene_marker_menu_unit_tests.rs"]
mod tests;
