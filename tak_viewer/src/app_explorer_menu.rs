//! Explorer input dispatch. Disk mutations use the existing file and save workers.

use super::*;
use super::undo::Part;
use crate::file_ops::{self, Op};
use crate::take_progress::{Pick, State, Target};
use crate::ui::explorer::{Clipboard, EditKind};
use crate::ui::explorer_menu::{self as menu, Command};

pub(super) struct ProgressRead {
    local: Option<std::sync::mpsc::Receiver<Vec<Pick>>>,
    remote: std::collections::VecDeque<(crate::ssh::RemoteLocation, Option<State>)>,
    picks: Vec<Pick>,
    after: State,
    total: usize,
}

impl TakViewerApp {
    pub(super) fn workspace_pick(&mut self, ctx: &egui::Context, path: PathBuf, ctrl: bool, shift: bool, open: bool, plain: bool) {
        let before = (!plain).then(|| self.explorer.selection.clone());
        self.explorer.pick(path.clone(), ctrl, shift);
        if let Some(before) = before { self.record_workspace_selection(before); }
        if open { self.explorer_action(ctx, WsAction::Open(path)); }
    }

    fn record_workspace_selection(&mut self, before: explorer::Selection) {
        if self.no_persist || before == self.explorer.selection { return; }
        self.observe(true);
        self.history.push(vec![Part::WorkspaceSelection { before, after: self.explorer.selection.clone() }]);
        self.history.merge_open = false;
    }

    pub(super) fn change_file_clipboard(&mut self, after: Option<Clipboard>) {
        if self.explorer.clipboard == after { return; }
        let before = std::mem::replace(&mut self.explorer.clipboard, after.clone());
        if !self.no_persist {
            self.observe(true);
            self.history.push(vec![Part::FileClipboard { before, after }]);
            self.history.merge_open = false;
        }
    }

    pub(super) fn set_folder_scope(&mut self, after: Option<PathBuf>, focus: bool) {
        self.explorer.scope = after;
        self.explorer.focus_search = focus;
        self.explorer.invalidate_rows();
        self.folder_find = None;
    }

    pub(super) fn change_folder_scope(&mut self, after: Option<PathBuf>) {
        let before = self.explorer.scope.clone();
        if before == after { return; }
        self.set_folder_scope(after.clone(), after.is_some());
        if !self.no_persist {
            self.observe(true);
            self.history.push(vec![Part::FolderScope { before, after }]);
            self.history.merge_open = false;
        }
    }

    pub(super) fn workspace_menu_command(&mut self, ctx: &egui::Context, command: Command, paths: Vec<PathBuf>) {
        use Command::*;
        let Some(path) = paths.first() else { return };
        let local = |p: &Path| !workspace::is_remote_root_key(p);
        let remote = |p: &Path| workspace::remote_address(p).map(|(_, location)| location);
        let mutable_paths = |app: &Self| {
            let open: Vec<_> = app.editor_rows().into_iter().map(|row| row.path).collect();
            paths.iter().filter(|p| explorer::can_change_file(p, &open)).cloned().collect::<Vec<_>>()
        };
        match command {
            Open => self.explorer_action(ctx, WsAction::Open(path.clone())),
            Beside => {
                for path in paths {
                    self.explorer_action(ctx, WsAction::OpenSide(path));
                    if dock::free_slot(&self.dock).is_none() { break; }
                }
            }
            NewTab => self.explorer_action(ctx, WsAction::OpenNewTab(path.clone())),
            Overlay => self.explorer_action(ctx, WsAction::Overlay(path.clone())),
            Convert if paths.len() == 1 => {
                if let Some(location) = remote(path) { self.save_remote_takm(ctx, location); }
                else { self.save_takm(ctx, path.clone()); }
            }
            Convert => {
                let local_paths = paths.iter().filter(|p| local(p)).cloned().collect();
                let remote_paths = paths.iter().filter(|p| menu::take(p)).filter_map(|p| remote(p)).collect();
                self.convert_explorer_batch(ctx, local_paths, remote_paths);
            }
            ShowFiles => {
                for p in paths.iter().filter(|p| local(p)) { file_ops::show_in_files(p); }
            }
            CopyPath => {
                ctx.copy_text(paths.iter().map(|p| remote(p).map_or_else(|| file_ops::paths_text(std::slice::from_ref(p)),
                    |location| file_ops::remote_path_text(&location.host, &location.path))).collect::<Vec<_>>().join("\n"));
            }
            CopyName => {
                let name = remote(path).map_or_else(|| file_ops::name_of(path), |loc| remote_name(&loc.path).to_owned());
                ctx.copy_text(name);
            }
            Copy | Cut => {
                let paths = if command == Cut { mutable_paths(self) }
                    else { paths.iter().filter(|p| local(p)).cloned().collect() };
                if !paths.is_empty() { self.change_file_clipboard(Some(Clipboard { paths, cut: command == Cut })); }
            }
            Paste => {
                let dest = if self.explorer.focused_folder.as_ref() == Some(path) { Some(path.as_path()) } else { path.parent() };
                if let (Some(clip), Some(dest)) = (&self.explorer.clipboard, dest.filter(|p| local(p) && menu::may_write(p))) {
                    self.file_op(ctx, Op::Paste { sources: clip.paths.clone(), dest: dest.to_path_buf(), cut: clip.cut });
                }
            }
            Rename if paths.len() == 1 && !mutable_paths(self).is_empty() => {
                self.explorer.start_disk_edit(ctx, EditKind::Rename, path.clone());
            }
            Delete => {
                let paths = mutable_paths(self);
                if !paths.is_empty() { self.file_op(ctx, Op::Trash { paths }); }
            }
            NewFolder => self.explorer.start_disk_edit(ctx, EditKind::NewFolder, path.clone()),
            Find if local(path) => self.change_folder_scope(Some(path.clone())),
            ConvertFolder => self.save_folder_takm(ctx, path.clone()),
            AddRoot => self.add_folder_to_workspace(ctx, path.clone()),
            Progress(value) => {
                let (local_paths, remote_paths) = paths.into_iter().filter(|p| menu::takm(p))
                    .partition::<Vec<_>, _>(|p| local(p));
                self.prepare_explorer_progress(ctx, local_paths,
                    remote_paths.iter().filter_map(|p| remote(p)).map(|location| (location, None)).collect(), value);
            }
            _ => {}
        }
    }

    pub(super) fn convert_explorer_batch(&mut self, ctx: &egui::Context, local: Vec<PathBuf>, remote: Vec<crate::ssh::RemoteLocation>) {
        if !local.is_empty() { self.save_takm_many(ctx, local); }
        self.explorer_remote_saves.extend(remote);
        self.poll_explorer_saves(ctx);
    }

    pub(super) fn poll_explorer_saves(&mut self, ctx: &egui::Context) {
        if self.remote_convert.is_none() && self.takm_job.is_none() && self.takm_batch.is_none() {
            if let Some(location) = self.explorer_remote_saves.pop_front() { self.save_remote_takm(ctx, location); }
        }
    }

    pub(super) fn prepare_explorer_progress(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>,
        remote: Vec<(crate::ssh::RemoteLocation, Option<State>)>, value: i64) {
        if self.explorer_progress_read.is_some() || self.progress_batch.is_some() || self.progress_job.is_some() {
            self.takm_note = Some(("진행 중인 작업이 끝난 뒤 진행 상태를 바꿔 주세요.".into(), ctx.input(|i| i.time)));
            return;
        }
        let total = paths.len() + remote.len();
        let after = State(Some(value));
        let (tx, local) = std::sync::mpsc::channel();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(paths.into_iter().filter_map(|path| crate::take_progress::read_pick(path, after)).collect());
            wake.request_repaint();
        });
        self.explorer_progress_read = Some(ProgressRead { local: Some(local), remote: remote.into(), picks: vec![], after, total });
    }

    pub(super) fn poll_explorer_progress(&mut self, ctx: &egui::Context) {
        use std::sync::mpsc::TryRecvError;
        let Some(mut read) = self.explorer_progress_read.take() else { return };
        if let Some(rx) = &read.local {
            match rx.try_recv() {
                Ok(picks) => { read.picks.extend(picks); read.local = None; }
                Err(TryRecvError::Disconnected) => read.local = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        for _ in 0..read.remote.len().min(8) {
            let (location, before) = read.remote.pop_front().unwrap();
            match before.map(|value| Some(Some(value))).unwrap_or_else(|| self.ssh.queued_progress(&location)) {
                Some(Some(before)) => read.picks.push(Pick {
                    target: Target::Remote(location), before, after: read.after, stamp: None,
                }),
                Some(None) => {}
                None => read.remote.push_back((location, before)),
            }
        }
        if read.local.is_some() || !read.remote.is_empty() {
            self.explorer_progress_read = Some(read);
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        let missing = read.total - read.picks.len();
        if read.picks.is_empty() || missing > 0 {
            self.takm_note = Some((format!("진행 상태를 읽지 못한 파일 {missing}개가 있어요."), ctx.input(|i| i.time)));
        }
        read.picks.retain(|p| p.before.0.unwrap_or(0) != read.after.0.unwrap_or(0));
        if !read.picks.is_empty() { self.set_progress_many(ctx, read.picks); }
    }

    pub(super) fn workspace_list_keys(&mut self, ctx: &egui::Context) {
        if key_focus::target(ctx) != key_focus::Target::Workspace || key_focus::keys_blocked(ctx)
            || self.typing(ctx) || self.capture.is_some() || self.hold.is_some() || self.comment_edit.is_some()
            || self.dirty_prompt.is_some() {
            self.explorer_paste_seen = false;
            return;
        }
        let hit = |key, ctrl, shift, alt| exact_key(ctx, key, ctrl, shift, alt);
        let all = hit(Key::A, true, false, false);
        let clear = hit(Key::Escape, false, false, false);
        if all || clear {
            let before = self.explorer.selection.clone();
            if all && !clear {
                self.explorer.pick_all_files();
                self.explorer.focused_folder = None;
            } else {
                self.explorer.selection = Default::default();
                self.explorer.focused_folder = None;
            }
            self.record_workspace_selection(before);
        }
        let mut clipboard_events = [false; 4];
        let mut released_with_command = false;
        ctx.input_mut(|input| input.events.retain(|event| {
            let index = match event {
                egui::Event::Copy => 0,
                egui::Event::Cut => 1,
                egui::Event::Paste(_) => 2,
                egui::Event::Key { key: Key::V, pressed: false, modifiers, .. } => {
                    released_with_command = (modifiers.command || modifiers.ctrl) && !modifiers.alt && !modifiers.shift;
                    3
                }
                _ => return true,
            };
            clipboard_events[index] = true;
            false
        }));
        let [copy, cut, paste_event, paste_released] = clipboard_events;
        let paste_pressed = paste_event || hit(Key::V, true, false, false);
        let paste = paste_pressed || (released_with_command && !self.explorer_paste_seen);
        self.explorer_paste_seen = !paste_released && (self.explorer_paste_seen || paste_pressed);
        let command = if hit(Key::Enter, true, false, false) { Command::Beside }
            else if hit(Key::Enter, false, false, false) { Command::Open }
            else if hit(Key::F2, false, false, false) { Command::Rename }
            else if hit(Key::Delete, false, false, false) { Command::Delete }
            else if hit(Key::C, false, true, true) { Command::CopyPath }
            else if hit(Key::R, false, true, true) { Command::ShowFiles }
            else if hit(Key::F, false, true, true) { Command::Find }
            else if cut || hit(Key::X, true, false, false) { Command::Cut }
            else if copy || hit(Key::C, true, false, false) { Command::Copy }
            else if paste { Command::Paste }
            else { return };
        let paths = if let Some(folder) = &self.explorer.focused_folder {
            if !matches!(command, Command::Find | Command::Paste | Command::CopyPath | Command::ShowFiles) { return; }
            vec![folder.clone()]
        } else {
            if command == Command::Find { return; }
            let mut picked = self.explorer.picked_paths();
            if matches!(command, Command::Open | Command::Paste | Command::ShowFiles) { picked.truncate(1); }
            picked
        };
        self.workspace_menu_command(ctx, command, paths);
    }
}

pub(super) fn remote_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path)
}
