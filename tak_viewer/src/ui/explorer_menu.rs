//! Explorer menu words, applicability, and shared painting. Callers own the row actions.

use eframe::egui::{self, pos2, vec2, Align2, FontId, Response, Sense, Ui};
use std::path::Path;

use super::explorer::{ACTIVE_SEL_BG, FG, FG_DIM, SIDEBAR_BG};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Open, Beside, NewTab, Overlay, Convert, ShowWorkspace, ShowFiles, CopyPath, CopyName,
    Cut, Copy, Paste, Rename, Delete, NewFolder, Find, ConvertFolder, RenameShown, ResetShown,
    RemoveRoot, AddRoot, Save, Close, CloseOthers, CloseSaved, CloseAll, Progress(i64),
}

#[derive(Debug, PartialEq)]
enum Item {
    Action(String, &'static str, Command),
    Progress,
    Separator,
}

pub struct Menu(Vec<Item>, f32);

impl Menu {
    fn new(width: f32) -> Self { Self(Vec::new(), width) }
}

impl Menu {
    fn group(&mut self, add: impl FnOnce(&mut Self)) {
        let mut group = Self::new(self.1);
        add(&mut group);
        if group.0.is_empty() { return; }
        if !self.0.is_empty() { self.0.push(Item::Separator); }
        self.0.extend(group.0);
    }

    fn item(&mut self, word: impl Into<String>, key: &'static str, action: Command) {
        self.0.push(Item::Action(word.into(), key, action));
    }

    fn paint(self, ui: &mut Ui, preview: bool) -> Option<Command> {
        let width = self.1;
        ui.set_width(width);
        let spacing = ui.spacing_mut();
        spacing.item_spacing.y = 0.0;
        spacing.interact_size.y = 22.0;
        spacing.button_padding.x = 8.0;
        ui.visuals_mut().widgets.hovered.weak_bg_fill = ACTIVE_SEL_BG;
        ui.visuals_mut().widgets.open.weak_bg_fill = ACTIVE_SEL_BG;
        ui.visuals_mut().widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        let mut action = None;
        for item in self.0 {
            match item {
                Item::Separator => { ui.separator(); }
                Item::Progress => {
                    let highlight = ui.painter().add(egui::Shape::Noop);
                    let label = ui.menu_button(egui::RichText::new("Set Progress").size(13.0).color(FG), |ui| {
                        action = Self::progress_choices().paint(ui, false);
                    });
                    let rect = label.response.rect;
                    let center = pos2(rect.left() + width - 12.0, rect.center().y);
                    let background = if preview || label.response.hovered() || label.inner.is_some() { ACTIVE_SEL_BG } else { SIDEBAR_BG };
                    if preview {
                        let row = egui::Rect::from_min_size(rect.min, vec2(width, rect.height()));
                        ui.painter().set(highlight, egui::Shape::rect_filled(row, 2.0, ACTIVE_SEL_BG));
                        egui::Area::new(ui.id().with("progress-submenu-preview"))
                            .order(egui::Order::Foreground).interactable(false).fade_in(false)
                            .fixed_pos(pos2(row.right() + 10.0, row.top()))
                            .show(ui.ctx(), |ui| egui::Frame::menu(ui.style()).fill(SIDEBAR_BG).show(ui, |ui| {
                                Self::progress_choices().paint(ui, false);
                            }));
                    }
                    ui.painter().rect_filled(egui::Rect::from_center_size(center, vec2(16.0, 15.0)), 0.0, background);
                    ui.painter().add(egui::Shape::convex_polygon(vec![
                        center + vec2(-2.0, -3.5), center + vec2(-2.0, 3.5), center + vec2(3.0, 0.0),
                    ], FG, egui::Stroke::NONE));
                }
                Item::Action(word, key, command) => {
                    let (rect, response) = ui.allocate_exact_size(vec2(width, 22.0), Sense::click());
                    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &word));
                    if response.hovered() {
                        ui.painter().rect_filled(rect, 2.0, ACTIVE_SEL_BG);
                    }
                    let font = FontId::proportional(13.0);
                    let key_width = if key.is_empty() { 0.0 } else {
                        ui.painter().text(pos2(rect.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER,
                            key, font.clone(), FG_DIM).width() + 8.0
                    };
                    let mut clip = rect;
                    clip.max.x -= key_width;
                    let label = ui.painter().with_clip_rect(clip).text(pos2(rect.left() + 8.0, rect.center().y),
                        Align2::LEFT_CENTER, &word, font, FG);
                    if label.right() > clip.right() {
                        response.clone().on_hover_text(word);
                    }
                    if response.clicked() {
                        action = Some(command);
                        ui.close_menu();
                    }
                }
            }
        }
        action
    }

    fn progress_choices() -> Self {
        let mut choices = Self::new(138.0);
        for value in 0..6 {
            choices.item(crate::take_progress::State(Some(value)).label(), "", Command::Progress(value));
        }
        choices
    }
}

#[cfg(target_os = "macos")]
const CMD: [&str; 5] = ["Cmd+Enter", "Cmd+X", "Cmd+C", "Cmd+V", "Cmd+S"];
#[cfg(not(target_os = "macos"))]
const CMD: [&str; 5] = ["Ctrl+Enter", "Ctrl+X", "Ctrl+C", "Ctrl+V", "Ctrl+S"];

pub fn workspace_file(tak: bool, writable: bool, paste: bool, overlay: bool, beside: bool) -> Menu {
    use Command::*;
    let mut menu = Menu::new(282.0);
    menu.item("Open Here", "", Open);
    if beside { menu.item("Open Beside", CMD[0], Beside); }
    menu.item("Open in New Tab", "", NewTab);
    if overlay { menu.group(|m| m.item("Add Overlay", "", Overlay)); }
    if tak && writable { menu.group(|m| m.item("Save as .takm", "", Convert)); }
    menu.group(|m| {
        m.item("Show in File Explorer", "Shift+Alt+R", ShowFiles);
        m.item("Copy Path", "Shift+Alt+C", CopyPath);
        m.item("Copy Name", "", CopyName);
    });
    menu.group(|m| {
        if !tak && writable { m.item("Cut", CMD[1], Cut); }
        m.item("Copy", CMD[2], Copy);
        if paste { m.item("Paste", CMD[3], Paste); }
    });
    if !tak && writable {
        menu.group(|m| {
            m.item("Rename…", "F2", Rename);
            m.item("Delete", "Delete", Delete);
        });
    }
    menu
}

pub fn workspace_folder(root: bool, named: Option<&str>, available: bool, writable: bool, paste: bool, can_add: bool) -> Menu {
    use Command::*;
    let mut menu = Menu::new(198.0);
    if available && writable { menu.item("New Folder…", "", NewFolder); }
    if available {
        menu.group(|m| {
            m.item("Find in Folder…", "Shift+Alt+F", Find);
            if writable { m.item("Save All TAK as .takm", "", ConvertFolder); }
        });
    }
    menu.group(|m| {
        if available { m.item("Show in File Explorer", "", ShowFiles); }
        m.item("Copy Path", "", CopyPath);
    });
    if available && writable && paste { menu.group(|m| m.item("Paste", "", Paste)); }
    menu.group(|m| {
        if root {
            m.item("Rename Shown Name…", "", RenameShown);
            if let Some(own) = named { m.item(format!("Reset Name ({own})"), "", ResetShown); }
            m.item("Remove from WORKSPACE", "", RemoveRoot);
        } else if can_add {
            m.item("Add to WORKSPACE", "", AddRoot);
        }
    });
    menu
}

pub fn editors_file(tak: bool, convertible: bool, dirty: bool, overlay: bool, in_workspace: bool,
    local: bool, others: bool, saved: bool) -> Menu {
    use Command::*;
    let mut menu = Menu::new(258.0);
    if dirty { menu.item("Save", CMD[4], Save); }
    if tak && convertible { menu.item("Save as .takm", "", Convert); }
    menu.group(|m| {
        if in_workspace { m.item("Show in WORKSPACE", "", ShowWorkspace); }
        if local { m.item("Show in File Explorer", "", ShowFiles); }
        m.item("Copy Path", "", CopyPath);
        m.item("Copy Name", "", CopyName);
    });
    menu.group(|m| {
        m.item(if overlay { "Remove Overlay" } else { "Close" }, "", Close);
        if others { m.item("Close Others", "", CloseOthers); }
        if saved { m.item("Close Saved", "", CloseSaved); }
        m.item("Close All", "", CloseAll);
    });
    menu
}

pub fn remote_file(tak: bool, connected: bool, beside: bool) -> Menu {
    use Command::*;
    let mut menu = Menu::new(210.0);
    if connected {
        menu.item("Open", "", Open);
        if beside { menu.item("Open Beside", "", Beside); }
        if tak { menu.group(|m| m.item("Save as .takm", "", Convert)); }
    }
    menu.group(|m| {
        m.item("Copy Path", "", CopyPath);
        m.item("Copy Name", "", CopyName);
    });
    menu
}

pub fn remote_folder(can_add: bool, root: bool, named: Option<&str>) -> Menu {
    use Command::*;
    let mut menu = Menu::new(210.0);
    if can_add { menu.item("Add to WORKSPACE", "", AddRoot); }
    menu.item("Copy Path", "", CopyPath);
    if root {
        menu.group(|m| {
            m.item("Rename Shown Name…", "", RenameShown);
            if let Some(own) = named { m.item(format!("Reset Name ({own})"), "", ResetShown); }
            m.item("Remove from WORKSPACE", "", RemoveRoot);
        });
    }
    menu
}

pub fn batch(total: usize, tak: usize, progress: bool, mutable: usize, copy: usize, beside: bool, editors: bool) -> Menu {
    use Command::*;
    let mut menu = Menu::new(232.0);
    if beside && !editors { menu.item("Open Beside", "", Beside); }
    if tak > 0 { menu.item(format!("Save {tak} TAK as .takm"), "", Convert); }
    if progress { menu.0.push(Item::Progress); }
    menu.group(|m| m.item("Copy Paths", "", CopyPath));
    menu.group(|m| {
        if editors {
            m.item(format!("Close {total}"), "", Close);
        } else {
            if mutable > 0 { m.item(format!("Cut {mutable} .takm"), "", Cut); }
            if copy > 0 { m.item(format!("Copy {copy}"), "", Copy); }
            if mutable > 0 { m.item(format!("Delete {mutable} .takm"), "", Delete); }
        }
    });
    menu
}

pub fn take(path: &Path) -> bool { tak_core::tak::is_take_path(path) }
pub fn takm(path: &Path) -> bool { tak_core::takm::is_takm_path(path) }

/// Lexical row guard. The existing worker also checks resolved paths and open files.
pub fn may_write(path: &Path) -> bool {
    !path.components().any(|part| part.as_os_str().eq_ignore_ascii_case("input"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section { Workspace, Editors, Ssh }

static PREVIEW: std::sync::OnceLock<(Section, Vec<String>)> = std::sync::OnceLock::new();

pub fn parse_preview(args: &mut Vec<String>, headless: bool) -> Result<(), &'static str> {
    let Some(i) = args.iter().position(|arg| arg == "--context-menu") else { return Ok(()) };
    let Some(value) = args.get(i + 1).filter(|word| !word.starts_with("--")
        && headless && args.iter().any(|arg| arg == "--screenshot")) else {
        return Err("--context-menu는 --headless --screenshot과 함께 행 이름을 넣어 주세요.");
    };
    let (section, names) = match value.split_once(':') {
        Some(("workspace", names)) => (Section::Workspace, names),
        Some(("editors", names)) => (Section::Editors, names),
        Some(("ssh", names)) => (Section::Ssh, names),
        _ => (Section::Workspace, value.as_str()),
    };
    let names: Vec<_> = names.split(',').map(str::trim).map(str::to_owned).collect();
    if names.iter().any(String::is_empty) { return Err("--context-menu에 행 이름을 넣어 주세요."); }
    let _ = PREVIEW.set((section, names));
    args.drain(i..i + 2);
    Ok(())
}

pub fn preview(section: Section) -> Option<&'static [String]> {
    PREVIEW.get().filter(|(target, _)| *target == section).map(|(_, names)| names.as_slice())
}

pub fn matches_preview(section: Section, name: &str) -> bool {
    preview(section).and_then(|names| names.last()).is_some_and(|row| row.eq_ignore_ascii_case(name))
}

pub fn popup(ui: &Ui, row: &Response, section: Section, preview: bool, contents: impl Fn() -> Menu) -> Option<Command> {
    let mut action = None;
    let mut painted = false;
    row.context_menu(|ui| {
        painted = true;
        let background = ui.painter().add(egui::Shape::Noop);
        let start = ui.cursor().min;
        action = contents().paint(ui, false);
        let rect = egui::Rect::from_min_max(start, ui.min_rect().max);
        ui.painter().set(background, egui::Shape::rect_filled(rect, 0.0, SIDEBAR_BG));
        use crate::app::key_focus::{self, Target};
        let target = match section { Section::Workspace => Target::Workspace, Section::Editors => Target::Editors, Section::Ssh => Target::Blocked };
        key_focus::region(ui, rect, target);
    });
    if painted {
        let pass = ui.ctx().cumulative_pass_nr();
        let open = row.context_menu_opened();
        ui.ctx().data_mut(|data| {
            let key = egui::Id::new("explorer-menu-open");
            if open { data.insert_temp(key, pass); } else { data.remove::<u64>(key); }
        });
    }
    if preview && ui.is_rect_visible(row.rect) {
        ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("explorer-menu-preview"), true));
        egui::Area::new(row.id.with("context-menu-preview"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(row.rect.left() + 146.0, row.rect.bottom()))
            .fade_in(false)
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::menu(ui.style()).fill(SIDEBAR_BG).show(ui, |ui| {
                    ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| { contents().paint(ui, true); });
                });
            });
    }
    action
}

pub fn is_open(ctx: &egui::Context) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|data| data.get_temp::<u64>(egui::Id::new("explorer-menu-open")).is_some_and(|seen| seen + 1 >= pass))
}

pub fn preview_missing(ctx: &egui::Context) -> bool {
    PREVIEW.get().is_some() && !ctx.data(|data| data.get_temp::<bool>(egui::Id::new("explorer-menu-preview")).unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(menu: Menu) -> Vec<String> {
        menu.0.into_iter().map(|item| match item {
            Item::Action(word, _, _) => word,
            Item::Progress => "Set Progress".into(),
            Item::Separator => "—".into(),
        }).collect()
    }

    #[test]
    fn workspace_tak_and_takm_menu_order_and_paste_visibility() {
        assert_eq!(labels(workspace_file(true, true, false, true, true)), [
            "Open Here", "Open Beside", "Open in New Tab", "—", "Add Overlay", "—", "Save as .takm",
            "—", "Show in File Explorer", "Copy Path", "Copy Name", "—", "Copy",
        ]);
        assert_eq!(labels(workspace_file(false, true, true, true, true)), [
            "Open Here", "Open Beside", "Open in New Tab", "—", "Add Overlay",
            "—", "Show in File Explorer", "Copy Path", "Copy Name", "—", "Cut", "Copy", "Paste",
            "—", "Rename…", "Delete",
        ]);
        assert_eq!(labels(workspace_file(false, false, true, false, false)), [
            "Open Here", "Open in New Tab", "—", "Show in File Explorer", "Copy Path", "Copy Name", "—", "Copy", "Paste",
        ]);
    }

    #[test]
    fn saved_folder_subfolder_and_missing_folder_menu_order() {
        assert_eq!(labels(workspace_folder(true, Some("captured"), true, true, true, false)), [
            "New Folder…", "—", "Find in Folder…", "Save All TAK as .takm", "—",
            "Show in File Explorer", "Copy Path", "—", "Paste", "—",
            "Rename Shown Name…", "Reset Name (captured)", "Remove from WORKSPACE",
        ]);
        assert_eq!(labels(workspace_folder(false, None, true, true, false, true)), [
            "New Folder…", "—", "Find in Folder…", "Save All TAK as .takm", "—",
            "Show in File Explorer", "Copy Path", "—", "Add to WORKSPACE",
        ]);
        assert_eq!(labels(workspace_folder(true, None, false, true, true, false)), [
            "Copy Path", "—", "Rename Shown Name…", "Remove from WORKSPACE",
        ]);
    }

    #[test]
    fn open_editors_dirty_tak_saved_takm_and_overlay_menu_order() {
        assert_eq!(labels(editors_file(true, true, true, false, true, true, true, true)), [
            "Save", "Save as .takm", "—", "Show in WORKSPACE", "Show in File Explorer",
            "Copy Path", "Copy Name", "—", "Close", "Close Others", "Close Saved", "Close All",
        ]);
        assert_eq!(labels(editors_file(false, true, false, true, false, true, true, true)), [
            "Show in File Explorer", "Copy Path", "Copy Name", "—", "Remove Overlay", "Close Others", "Close Saved", "Close All",
        ]);
        assert_eq!(labels(editors_file(false, false, false, false, false, false, false, false)), [
            "Copy Path", "Copy Name", "—", "Close", "Close All",
        ]);
    }

    #[test]
    fn ssh_file_and_folder_menu_order_for_connected_and_lost_hosts() {
        assert_eq!(labels(remote_file(true, true, true)), [
            "Open", "Open Beside", "—", "Save as .takm", "—", "Copy Path", "Copy Name",
        ]);
        assert_eq!(labels(remote_file(false, true, true)), [
            "Open", "Open Beside", "—", "Copy Path", "Copy Name",
        ]);
        assert_eq!(labels(remote_file(true, false, true)), ["Copy Path", "Copy Name"]);
        assert_eq!(labels(remote_folder(true, false, None)), ["Add to WORKSPACE", "Copy Path"]);
        assert_eq!(labels(remote_folder(false, false, None)), ["Copy Path"]);
    }

    #[test]
    fn batch_menu_counts_match_mixed_file_actions() {
        assert_eq!(labels(batch(3, 2, true, 1, 3, true, false)), [
            "Open Beside", "Save 2 TAK as .takm", "Set Progress", "—", "Copy Paths", "—", "Cut 1 .takm", "Copy 3", "Delete 1 .takm",
        ]);
        assert_eq!(labels(batch(3, 2, true, 0, 0, false, true)), [
            "Save 2 TAK as .takm", "Set Progress", "—", "Copy Paths", "—", "Close 3",
        ]);
        assert_eq!(labels(batch(2, 2, false, 0, 2, true, false)), [
            "Open Beside", "Save 2 TAK as .takm", "—", "Copy Paths", "—", "Copy 2",
        ]);
    }
}
