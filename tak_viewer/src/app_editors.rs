//! The app side of OPEN EDITORS (`ui/open_editors.rs`): what it lists and what its clicks do.
//! Every click reuses an action that already exists, so undo works as for them: × on a view's
//! file = the view tab's ✕ (`close_view`; the only view keeps its place and shows nothing, as
//! when undo puts back a layout without that file), × on a laid-over file = `overlay_off`,
//! `폴더 추가` = `open_workspace` (the add-folder action), a row = `activate`. A row of a view
//! with a file laid over it = a click on that file's name tag 1 / 2: `activate` (what the press
//! on the tag does) and `name_tag_click` (hide / show, the same layout undo action and notes);
//! a row being read does what its tag does then (no tag yet = nothing to hide, only `activate`).

use super::*;
use crate::ui::open_editors::{EditorAction, Entry, Group};

impl TakViewerApp {
    /// Each view on screen with a file, in `order` (reading order): its file (on screen or being
    /// read) and the file laid over it.
    pub(super) fn editor_groups(&self, order: &[usize]) -> Vec<Group> {
        order
            .iter()
            .filter_map(|&k| {
                let v = &self.views[k];
                let own = v.target()?;
                let over = v.overlay_target();
                let entry = |path: &Path, i: usize| Entry {
                    path: path.to_path_buf(),
                    mark: over.map(|_| i),
                    // Keys 1 / 2 hide a file only while one is laid over the view.
                    hidden: v.overlay.is_some() && !v.shown[i],
                    loading: if i == 0 { v.loading.is_some() } else { v.overlay_load.is_some() },
                    add_folder: self.folder_outside_workspace(path),
                };
                let mut entries = vec![entry(own, 0)];
                entries.extend(over.map(|p| entry(p, 1)));
                Some(Group { slot: k, active: k == self.active, entries })
            })
            .collect()
    }

    /// The folder of `file` when no WORKSPACE folder holds it (path comparison, the folder
    /// itself or any folder above it counts).
    fn folder_outside_workspace(&self, file: &Path) -> Option<PathBuf> {
        let dir = file.parent().filter(|d| d.is_absolute())?;
        self.workspace.root_of(file).is_none().then(|| dir.to_path_buf())
    }

    /// What a click in OPEN EDITORS asked for (applied after drawing).
    pub(super) fn editor_action(&mut self, ctx: &egui::Context, a: EditorAction) {
        match a {
            EditorAction::Activate(k) => self.activate(k),
            // As a click on name tag `which + 1`: its press chooses the view (`pressed_view`),
            // then `name_tag_click`.
            EditorAction::ToggleShown(k, which) => {
                self.activate(k);
                self.name_tag_click(k, which, false);
            }
            EditorAction::Close(k) => self.close_editor(k),
            EditorAction::OverlayOff(k) => self.overlay_off(k),
            EditorAction::AddFolder(dir) => {
                self.open_workspace(dir);
            }
            EditorAction::Full => {
                let why = format!("폴더를 추가할 수 없어요: WORKSPACE가 다 찼어요 ({MAX_ROOTS}/{MAX_ROOTS}). WORKSPACE에서 폴더를 오른쪽 클릭 → 목록에서 빼기로 하나 뺀 뒤 다시 누르세요.");
                self.overlay_note = Some((why, ctx.input(|i| i.time)));
            }
        }
    }

    /// × on a view's file: the view closes like with its tab ✕; the only view stays, empty.
    fn close_editor(&mut self, slot: usize) {
        if self.open_views().len() > 1 {
            self.close_view(slot);
        } else {
            self.remember(slot);
            drop_later(self.views[slot].clear());
        }
    }
}
