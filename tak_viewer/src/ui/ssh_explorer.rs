//! Third EXPLORER section (Figma option ②). Only visible cached file rows are painted.

use super::explorer::{
    chevron, file_icon, ACTIVE_SEL_BG, BUTTON_BG, FG, FG_DIM, HOVER_BG, ICON_FG, SIDEBAR_BG,
    TAK_ICON,
};
use crate::ssh::{
    is_drive_root, parent_path, HostState, RemoteLocation, SshAction, SshManager, TreeRow, REMOTE_DRAG_NOTE,
    REMOTE_OVERLAY_NOTE,
};
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontId, Rect, RichText, ScrollArea, Sense, Stroke, Ui,
};

const ROW_H: f32 = 22.0;
const GREEN: Color32 = Color32::from_rgb(0x46, 0xc8, 0x5a);
const AMBER: Color32 = Color32::from_rgb(0xe8, 0xa3, 0x3d);
const DISCONNECT_BG: Color32 = Color32::from_rgb(0x3a, 0x3d, 0x41);
const INPUT_BG: Color32 = Color32::from_rgb(0x3c, 0x3c, 0x3c);

enum Input {
    Connect(String),
    Disconnect(String),
    Platform(String, String),
    Python(String, String),
    Navigate(String, String),
    Refresh(String),
    Folder(String, String),
}

fn path_input(ui: &mut Ui, rect: Rect, path: &mut String, windows: bool) -> (egui::Response, Option<String>) {
    let hint = if windows { "맨 위 · 드라이브 목록" } else { "/" };
    let response = ui.put(rect, egui::TextEdit::singleline(path).id_source("path")
        .hint_text(RichText::new(hint).color(FG_DIM))
        .font(FontId::proportional(11.0)).text_color(FG).frame(false).margin(vec2(4.0, 2.0)));
    let submitted = (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))).then(|| path.clone());
    (response, submitted)
}

fn folder_input(host: &str, current_path: &str, path: &str) -> Input {
    if current_path.is_empty() && is_drive_root(path) {
        Input::Navigate(host.into(), path.into())
    } else {
        Input::Folder(host.into(), path.into())
    }
}

fn chip(
    ui: &mut Ui,
    rect: Rect,
    id: impl std::hash::Hash,
    text: &str,
    fill: Color32,
) -> egui::Response {
    let response = ui.interact(rect, ui.id().with(id), Sense::click());
    ui.painter().rect_filled(
        rect,
        2.0,
        if response.hovered() {
            fill.gamma_multiply(1.18)
        } else {
            fill
        },
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(11.0),
        Color32::WHITE,
    );
    response
}

fn fitted(
    ui: &Ui,
    text: &str,
    width: f32,
    font: FontId,
    color: Color32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), font, color, f32::INFINITY);
    job.wrap.max_width = width.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    ui.fonts(|fonts| fonts.layout_job(job))
}

fn label(ui: &Ui, text: &str, rect: Rect, font: FontId, color: Color32) {
    let galley = fitted(ui, text, rect.width(), font, color);
    ui.painter()
        .with_clip_rect(ui.clip_rect().intersect(rect))
        .galley(
            pos2(rect.left(), rect.center().y - galley.size().y * 0.5),
            galley,
            color,
        );
}

/// Reserve actual section content, rather than taking half the local tree when SSH is off.
pub fn desired_height(manager: &SshManager) -> f32 {
    if manager.collapsed {
        return ROW_H;
    }
    if manager.hosts.is_empty() {
        return ROW_H + 32.0;
    }
    ROW_H
        + manager
            .hosts
            .iter()
            .map(|h| {
                let children = h.shows_rows();
                ROW_H
                    + if children {
                        ROW_H
                            + ROW_H
                            + (h.rows.len() as f32 * ROW_H).min(220.0)
                            + if h.note.is_some() { 32.0 } else { 0.0 }
                    } else {
                        0.0
                    }
            })
            .sum::<f32>()
}

/// Button actions execute after painting, so visible rows borrow only the cached model.
/// Root handles file opens, notices and the normal layout undo.
pub fn show(
    ui: &mut Ui,
    manager: &mut SshManager,
    current: Option<&RemoteLocation>,
) -> Vec<SshAction> {
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let mut actions = Vec::new();
    let mut inputs = Vec::new();
    let (header, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    ui.painter().rect_filled(header, 0.0, SIDEBAR_BG);
    chevron(
        ui.painter(),
        pos2(header.left() + 10.0, header.center().y),
        !manager.collapsed,
        ICON_FG,
    );
    let mut job = egui::text::LayoutJob::simple(
        format!("SSH ({})", manager.hosts.len()),
        FontId::proportional(11.0),
        FG_DIM,
        f32::INFINITY,
    );
    for section in &mut job.sections {
        section.format.extra_letter_spacing = 0.35;
    }
    let title = ui.fonts(|fonts| fonts.layout_job(job));
    let title_pos = pos2(
        header.left() + 22.0,
        header.center().y - title.size().y * 0.5,
    );
    ui.painter().galley(title_pos, title.clone(), FG_DIM);
    ui.painter()
        .galley(title_pos + vec2(0.35, 0.0), title, FG_DIM);
    if response.clicked() {
        manager.collapsed = !manager.collapsed;
    }
    if manager.collapsed {
        return actions;
    }
    if manager.hosts.is_empty() {
        let text = if manager.discovery_pending {
            "SSH 별칭 읽는 중…"
        } else {
            manager
                .discovery_note
                .as_deref()
                .unwrap_or("SSH config에 저장한 Host 별칭이 없어요.")
        };
        ui.add(egui::Label::new(RichText::new(text).size(11.0).color(FG_DIM)).wrap());
        return actions;
    }
    // Host rows are small. Directory contents alone use show_rows, so file count does not
    // determine frame work and connecting hosts remain visible above their own tree.
    let max_h = (ui.available_height() * 0.8).max(88.0);
    ScrollArea::vertical().id_salt("ssh_hosts").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
        for host in &mut manager.hosts {
            let platform = match manager.platform_overrides.get(&host.alias).map(String::as_str) { Some("windows") => "Windows", _ => "Linux" };
            let status = match host.state { HostState::Connected => format!("{platform} · 연결됨"), HostState::Connecting => "연결 중…".to_owned(), HostState::Lost => format!("{platform} · 끊김"), HostState::Off => platform.to_owned() };
            ui.push_id(host.alias.clone(), |ui| {
                let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                if response.hovered() { ui.painter().rect_filled(rect, 0.0, HOVER_BG); }
                chevron(ui.painter(), pos2(rect.left() + 18.0, rect.center().y), host.expanded, ICON_FG);
                let dot = pos2(rect.left() + 33.0, rect.center().y);
                match host.state {
                    HostState::Connected => { ui.painter().circle_filled(dot, 3.0, GREEN); }
                    HostState::Connecting => { ui.painter().circle_filled(dot, 3.0, AMBER); }
                    _ => { ui.painter().circle_stroke(dot, 3.0, Stroke::new(1.1_f32, FG_DIM)); }
                }
                let (button, fill) = match host.state { HostState::Connected => ("끊기", DISCONNECT_BG), HostState::Connecting => ("취소", DISCONNECT_BG), _ => ("연결", BUTTON_BG) };
                let chip_rect = Rect::from_min_max(pos2((rect.right() - 40.0).max(rect.left()), rect.top() + 3.0), pos2(rect.right() - 3.0, rect.bottom() - 3.0));
                let chip_response = chip(ui, chip_rect, "connect", button, fill);
                let alias_left = rect.left() + 44.0;
                let right = chip_rect.left() - 7.0;
                let platform_w = ui.fonts(|f| f.layout_no_wrap(status.clone(), FontId::proportional(11.0), FG_DIM).size().x);
                let alias_rect = Rect::from_min_max(pos2(alias_left, rect.top()), pos2((right - platform_w - 8.0).max(alias_left), rect.bottom()));
                label(ui, &host.alias, alias_rect, FontId::proportional(12.0), FG);
                if right > alias_left + platform_w { ui.painter().text(pos2(right, rect.center().y), Align2::RIGHT_CENTER, &status, FontId::proportional(11.0), FG_DIM); }
                if chip_response.clicked() {
                    inputs.push(if matches!(host.state, HostState::Connected | HostState::Connecting) { Input::Disconnect(host.alias.clone()) } else { Input::Connect(host.alias.clone()) });
                } else if response.clicked() { host.expanded = !host.expanded; }
                response.context_menu(|ui| {
                    ui.label(RichText::new("서버 종류").size(11.0).color(FG_DIM));
                    let enabled = matches!(host.state, HostState::Off | HostState::Lost);
                    for (name, value) in [("Linux", "linux"), ("Windows", "windows")] {
                        if ui.add_enabled(enabled, egui::SelectableLabel::new(platform == name, name)).clicked() { inputs.push(Input::Platform(host.alias.clone(), value.into())); ui.close_menu(); }
                    }
                    ui.separator();
                    let setting = manager.python_settings.get(&host.alias);
                    let remembered = setting.and_then(|s| s.remembered.as_deref());
                    ui.label(RichText::new(remembered.unwrap_or("자동 탐색")).size(11.0).color(FG_DIM));
                    ui.add_enabled_ui(enabled, |ui| {
                        ui.menu_button("Python 명령…", |ui| {
                            let id = ui.id().with("python_command");
                            let stored = ui.data_mut(|d| d.get_temp::<String>(id));
                            let fresh = stored.is_none();
                            let mut command = stored.unwrap_or_else(|| setting.and_then(|s| s.explicit.clone()).unwrap_or_default());
                            let edit = ui.add(egui::TextEdit::singleline(&mut command).desired_width(360.0));
                            if fresh { edit.request_focus(); }
                            ui.label(RichText::new("Enter 저장 · Esc 취소 · 빈칸은 자동 탐색").size(11.0).color(FG_DIM));
                            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                ui.data_mut(|d| d.remove::<String>(id));
                                ui.close_menu();
                            } else if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                inputs.push(Input::Python(host.alias.clone(), command));
                                ui.data_mut(|d| d.remove::<String>(id));
                                ui.close_menu();
                            } else { ui.data_mut(|d| d.insert_temp(id, command)); }
                        });
                    });
                    if !enabled { ui.label(RichText::new("연결을 끊은 뒤 바꿀 수 있어요.").size(11.0).color(FG_DIM)); }
                });
                if !host.shows_rows() {
                    if host.expanded {
                        if let Some(note) = &host.note { ui.add(egui::Label::new(RichText::new(note).size(11.0).color(AMBER)).wrap()); }
                    }
                    return;
                }
                if let Some(note) = &host.note { ui.add(egui::Label::new(RichText::new(note).size(11.0).color(AMBER)).wrap()); }
                if !matches!(host.state, HostState::Connected | HostState::Lost) { return; }
                let connected = host.state == HostState::Connected;
                let (path_rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
                ui.allocate_ui_with_layout(vec2(ui.available_width(), ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if ui.add_enabled(!host.path.is_empty(), egui::Button::new(RichText::new("Add to WORKSPACE").size(11.0))).clicked() {
                        actions.push(SshAction::AddWorkspace { location: RemoteLocation { host: host.alias.clone(), path: host.path.clone() }, platform: if platform == "Windows" { "windows" } else { "linux" }.into() });
                    }
                });
                let refresh_rect = Rect::from_min_max(pos2(path_rect.right() - 23.0, path_rect.top()), path_rect.right_bottom());
                let up_rect = refresh_rect.translate(vec2(-24.0, 0.0));
                let input_rect = Rect::from_min_max(pos2(path_rect.left() + 26.0, path_rect.top() + 1.0), pos2((up_rect.left() - 4.0).max(path_rect.left() + 26.0), path_rect.bottom() - 1.0));
                ui.painter().rect_filled(input_rect, 2.0, INPUT_BG);
                ui.add_enabled_ui(connected, |ui| {
                    if let (_, Some(path)) = path_input(ui, input_rect, &mut host.path_input, platform == "Windows") { inputs.push(Input::Navigate(host.alias.clone(), path)); }
                    if ui.put(up_rect, egui::Button::new(RichText::new("↑").size(13.0)).frame(false)).clicked() { inputs.push(Input::Navigate(host.alias.clone(), parent_path(&host.path))); }
                    if ui.put(refresh_rect, egui::Button::new(RichText::new("↻").size(13.0)).frame(false)).clicked() { inputs.push(Input::Refresh(host.alias.clone())); }
                });
                let tree_height = (host.rows.len() as f32 * ROW_H).min(220.0);
                ScrollArea::vertical().id_salt("tree").max_height(tree_height.max(ROW_H)).auto_shrink([false, true]).show_rows(ui, ROW_H, host.rows.len(), |ui, range| {
                    for index in range {
                        let row = &host.rows[index];
                        let depth = match row { TreeRow::Dir { depth, .. } | TreeRow::File { depth, .. } | TreeRow::Note { depth, .. } => *depth };
                        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
                        let x = rect.left() + 26.0 + depth as f32 * 12.0;
                        let selected = matches!(row, TreeRow::File { path, .. } if current.is_some_and(|r| r.host == host.alias && r.path == *path));
                        if selected { ui.painter().rect_filled(rect, 0.0, ACTIVE_SEL_BG); }
                        else if response.hovered() { ui.painter().rect_filled(rect, 0.0, HOVER_BG); }
                        match row {
                            TreeRow::Dir { path, name, expanded, .. } => {
                                chevron(ui.painter(), pos2(x + 7.0, rect.center().y), *expanded, ICON_FG);
                                label(ui, name, Rect::from_min_max(pos2(x + 20.0, rect.top()), rect.right_bottom()), FontId::proportional(12.0), FG);
                                if response.clicked() && connected { inputs.push(folder_input(&host.alias, &host.path, path)); }
                                response.context_menu(|ui| {
                                    if ui.button("Add to WORKSPACE").clicked() {
                                        actions.push(SshAction::AddWorkspace { location: RemoteLocation { host: host.alias.clone(), path: path.clone() }, platform: if platform == "Windows" { "windows" } else { "linux" }.into() });
                                        ui.close_menu();
                                    }
                                });
                            }
                            TreeRow::File { path, name, .. } => {
                                let icon_x = x + crate::take_progress::SLOT;
                                file_icon(ui.painter(), pos2(icon_x + 8.0, rect.center().y), TAK_ICON);
                                let avail = (rect.right() - icon_x - 20.0).max(0.0);
                                let (text, _) = super::open_editors::fit(name, "", 0.0, avail, |s| ui.fonts(|f| f.layout_no_wrap(s.into(), FontId::proportional(12.0), FG).size().x));
                                label(ui, &text, Rect::from_min_max(pos2(icon_x + 20.0, rect.top()), rect.right_bottom()), FontId::proportional(12.0), FG);
                                let location = RemoteLocation { host: host.alias.clone(), path: path.clone() };
                                if ui.is_rect_visible(rect) {
                                    let state = host.visible_progress(&location.path);
                                    let id = ui.id().with(("ssh-progress", &location.path));
                                    if let Some(pick) = crate::take_progress::row_mark(ui, rect, x, id,
                                        crate::take_progress::Target::Remote(location.clone()), state, None, connected) {
                                        actions.push(SshAction::Progress(pick));
                                    }
                                }
                                if response.clicked() {
                                    if connected { actions.push(SshAction::Open { location: location.clone(), side: ui.input(|i| i.modifiers.ctrl) }); }
                                    else { actions.push(SshAction::Notice("연결 버튼을 눌러 다시 연결한 뒤 열어 주세요.".into())); }
                                }
                                if response.drag_started() { actions.push(SshAction::Notice(REMOTE_DRAG_NOTE.into())); }
                                response.context_menu(|ui| {
                                    if ui.add_enabled(connected, egui::Button::new("열기")).clicked() { actions.push(SshAction::Open { location: location.clone(), side: false }); ui.close_menu(); }
                                    if ui.add_enabled(connected, egui::Button::new("옆 화면에 열기")).clicked() { actions.push(SshAction::Open { location: location.clone(), side: true }); ui.close_menu(); }
                                    ui.separator();
                                    ui.add(egui::Label::new(RichText::new("원격 파일은 얹거나 끌어 놓을 수 없어요.").size(11.0).color(FG_DIM)).wrap());
                                    if ui.button("이유 보기").clicked() { actions.push(SshAction::Notice(format!("{REMOTE_OVERLAY_NOTE} {REMOTE_DRAG_NOTE}"))); ui.close_menu(); }
                                });
                            }
                            TreeRow::Note { text, .. } => label(ui, text, Rect::from_min_max(pos2(x + 4.0, rect.top()), rect.right_bottom()), FontId::proportional(11.0), FG_DIM),
                        }
                    }
                });
            });
        }
    });
    for input in inputs {
        let result = match input {
            Input::Connect(host) => manager.connect(&host),
            Input::Disconnect(host) => {
                manager.disconnect(&host);
                Ok(())
            }
            Input::Platform(host, platform) => {
                manager.set_platform(&host, &platform);
                Ok(())
            }
            Input::Python(host, command) => manager.set_python(&host, &command),
            Input::Navigate(host, path) => manager.navigate(&host, path),
            Input::Refresh(host) => manager.refresh(&host),
            Input::Folder(host, path) => manager.toggle_folder(&host, &path),
        };
        if let Err(message) = result {
            actions.push(SshAction::Notice(message));
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_enter_submits_top_level_from_the_real_path_widget() {
        for windows in [false, true] {
            let ctx = egui::Context::default();
            let mut path = String::new();
            let mut submitted = None;
            for enter in [false, true] {
                let mut input = egui::RawInput::default();
                if enter {
                    input.events.push(egui::Event::Key { key: egui::Key::Enter, physical_key: None,
                        pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
                }
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let rect = Rect::from_min_size(ui.min_rect().min, vec2(220.0, ROW_H));
                        let (response, value) = path_input(ui, rect, &mut path, windows);
                        submitted = value;
                        if !enter { response.request_focus(); }
                    });
                });
                if !enter { assert!(submitted.is_none()); }
            }
            assert_eq!(submitted, Some(String::new()));
            assert!(path.is_empty());
        }
    }

    #[test]
    fn drive_click_navigates_and_normal_folders_still_expand() {
        assert!(matches!(folder_input("lab", "", "E:\\"), Input::Navigate(host, path) if host == "lab" && path == "E:\\"));
        assert!(matches!(folder_input("lab", "E:\\", "E:\\raw"), Input::Folder(_, path) if path == "E:\\raw"));
    }
}
