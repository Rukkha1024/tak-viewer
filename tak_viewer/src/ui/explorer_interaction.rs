//! Session-only WORKSPACE selection, clipboard, and inline disk name draft.

use super::*;
use crate::file_ops::{self, Op};
use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub selected: BTreeSet<PathBuf>,
    anchor: Option<PathBuf>,
    range_basis: Option<BTreeSet<PathBuf>>,
}

impl Selection {
    pub fn click(&mut self, order: &[PathBuf], path: PathBuf, ctrl: bool, shift: bool) {
        if shift {
            if let Some((start, end)) = self.anchor.as_ref().and_then(|anchor| {
                Some((order.iter().position(|p| p == anchor)?, order.iter().position(|p| p == &path)?))
            }) {
                let basis = if ctrl {
                    self.range_basis.get_or_insert_with(|| self.selected.clone()).clone()
                } else {
                    self.range_basis = None;
                    BTreeSet::new()
                };
                self.selected = basis;
                self.selected.extend(order[start.min(end)..=start.max(end)].iter().cloned());
                return;
            }
        }
        self.range_basis = None;
        if ctrl {
            if !self.selected.remove(&path) { self.selected.insert(path.clone()); }
            self.anchor = if self.selected.contains(&path) { Some(path) } else { self.selected.first().cloned() };
        } else {
            self.anchor = Some(path.clone());
            let old = std::mem::replace(&mut self.selected, BTreeSet::from([path]));
            if old.len() > 512 { crate::views::drop_later(Some(old)); }
        }
    }

    pub fn all(&mut self, rows: &[PathBuf]) {
        self.selected = rows.iter().cloned().collect();
        if !self.anchor.as_ref().is_some_and(|p| self.selected.contains(p)) { self.anchor = rows.first().cloned(); }
        self.range_basis = None;
    }

    pub fn moved(&mut self, from: &Path, to: &Path) {
        if self.selected.remove(from) { self.selected.insert(to.to_path_buf()); }
        if self.anchor.as_deref() == Some(from) { self.anchor = Some(to.to_path_buf()); }
        if let Some(basis) = &mut self.range_basis {
            if basis.remove(from) { basis.insert(to.to_path_buf()); }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clipboard {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind { Rename, NewFolder }

pub(super) struct DiskEdit {
    kind: EditKind,
    anchor: PathBuf,
    name: String,
    initial: Option<Receiver<String>>,
    focus: bool,
    pending: bool,
    error: Option<String>,
}

pub(super) struct RowEdit {
    kind: EditKind,
    anchor: PathBuf,
    error: Option<String>,
}

impl RowEdit {
    pub(super) fn insert(self, rows: &mut Vec<Row>) {
        let Some(index) = rows.iter().position(|row| row.path() == Some(self.anchor.as_path())) else { return };
        let depth = rows[index].depth();
        let index = match self.kind {
            EditKind::Rename => index + 1,
            EditKind::NewFolder => {
                rows.insert(index + 1, Row::NewFolder { depth: depth + 1, path: self.anchor });
                index + 2
            }
        };
        if let Some(text) = self.error { rows.insert(index, Row::EditError { depth, text }); }
    }
}

impl DiskEdit {
    fn new(ctx: &egui::Context, kind: EditKind, anchor: PathBuf) -> Self {
        let (name, initial) = match kind {
            EditKind::Rename => (file_ops::name_of(&anchor), None),
            EditKind::NewFolder => {
                let (tx, rx) = mpsc::channel();
                let ctx = ctx.clone();
                let parent = anchor.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(file_ops::free_folder_name(&parent, "New Folder"));
                    ctx.request_repaint();
                });
                (String::new(), Some(rx))
            }
        };
        Self { kind, anchor, name, initial, focus: true, pending: false, error: None }
    }
}

impl ExplorerState {
    pub fn start_disk_edit(&mut self, ctx: &egui::Context, kind: EditKind, path: PathBuf) {
        self.disk_edit = Some(DiskEdit::new(ctx, kind, path));
        self.invalidate_rows();
    }

    pub fn disk_edit_result(&mut self, error: Option<String>) {
        if let Some(edit) = &mut self.disk_edit {
            if !edit.pending { return; }
            if let Some(why) = error {
                edit.pending = false;
                edit.focus = true;
                edit.error = Some(why);
            } else {
                self.disk_edit = None;
            }
            self.invalidate_rows();
        }
    }

    pub(super) fn row_edit(&self) -> Option<RowEdit> {
        self.disk_edit.as_ref().map(|edit| RowEdit { kind: edit.kind, anchor: edit.anchor.clone(), error: edit.error.clone() })
    }

    pub(super) fn paint_disk_edit(&mut self, ui: &mut Ui, rect: Rect, left: f32, path: &Path, kind: EditKind) -> Option<WsAction> {
        let edit = self.disk_edit.as_mut().filter(|edit| edit.anchor == path && edit.kind == kind)?;
        if let Some(rx) = &edit.initial {
            match rx.try_recv() {
                Ok(name) => edit.name = name,
                Err(mpsc::TryRecvError::Empty) => {
                    ui.painter().text(pos2(left, rect.center().y), Align2::LEFT_CENTER, "New Folder…", FontId::proportional(13.0), FG);
                    return None;
                }
                Err(mpsc::TryRecvError::Disconnected) => edit.name = "New Folder".into(),
            }
            edit.initial = None;
        }
        let box_rect = Rect::from_min_max(pos2(left, rect.top() + 1.0), pos2(rect.right() - 6.0, rect.bottom() - 1.0));
        let id = egui::Id::new(("ws_disk_edit", path, kind == EditKind::Rename));
        let edge = if edit.error.is_some() { Color32::LIGHT_RED } else { FOCUS_OUTLINE };
        ui.painter().rect(box_rect, 1.0, INPUT_BG, Stroke::new(1.0, edge));
        let response = ui.put(box_rect, egui::TextEdit::singleline(&mut edit.name)
            .id(id).clip_text(true).frame(false).font(FontId::proportional(12.5)).interactive(!edit.pending));
        if edit.pending { return None; }
        let focus = std::mem::take(&mut edit.focus);
        if focus {
            response.request_focus();
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(edit.name.chars().count()));
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), id);
            }
            return None;
        }
        let key = |key| ui.input(|i| i.raw.events.iter().any(|event| matches!(event,
            egui::Event::Key { key: pressed, modifiers, pressed: true, .. } if *pressed == key && modifiers.is_none())));
        if key(egui::Key::Escape) {
            self.disk_edit = None;
            self.invalidate_rows();
            return None;
        }
        let outside = ui.input(|i| i.focused && i.pointer.any_pressed()
            && i.pointer.interact_pos().is_some_and(|pos| !box_rect.contains(pos)));
        if !key(egui::Key::Enter) && !outside { return None; }
        let name = match file_ops::check_name(&edit.name) {
            Ok(name) => name.to_owned(),
            Err(why) => {
                edit.error = Some(why);
                response.request_focus();
                self.invalidate_rows();
                ui.ctx().request_repaint();
                return None;
            }
        };
        edit.pending = true;
        self.invalidate_rows();
        Some(WsAction::CommitName(match kind {
            EditKind::Rename => Op::Rename { from: path.to_path_buf(), name },
            EditKind::NewFolder => Op::NewFolder { parent: path.to_path_buf(), name },
        }))
    }

    pub(super) fn editing_disk_name(&self, path: &Path, kind: EditKind) -> bool {
        self.disk_edit.as_ref().is_some_and(|edit| edit.anchor == path && edit.kind == kind)
    }

    #[cfg(test)]
    pub fn draft(&self) -> Option<(&str, Option<&str>, bool)> {
        self.disk_edit.as_ref().map(|edit| (edit.name.as_str(), edit.error.as_deref(), edit.pending))
    }
}
