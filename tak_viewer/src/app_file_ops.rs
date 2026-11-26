//! EXPLORER file operations inside the app: run
//! [`crate::file_ops`] on a worker thread, refuse files that are open or busy, re-read the changed
//! folders, and make each change one undo step (Ctrl+Z / Ctrl+Y run the inverse on the worker).

use super::*;
use super::undo::Part;
use crate::file_ops::{Done, Op, Outcome};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

pub(super) struct Job {
    rx: Receiver<Outcome>,
    /// None = a new change; Some(redo) = an undo (false) or redo (true) of `record`.
    replay: Option<bool>,
    record: Option<Arc<Change>>,
    pub(super) name_edit: bool,
    cut_clipboard: Option<explorer::Clipboard>,
}

pub(super) struct Change {
    done: Done,
    cut_clipboard: Option<explorer::Clipboard>,
}

enum Work {
    Run(Op),
    Undo(Arc<Change>),
}

impl TakViewerApp {
    /// Start a file change (new folder, rename, paste, trash). The result, or why not, goes to
    /// the status bar.
    pub(super) fn file_op(&mut self, ctx: &egui::Context, op: Op) -> bool {
        let cut = matches!(&op, Op::Paste { sources, cut: true, .. }
            if self.explorer.clipboard.as_ref().is_some_and(|clip| clip.cut && &clip.paths == sources));
        let files = op.files();
        let started = self.start_file_work(ctx, &files, Work::Run(op), None);
        if started && cut {
            self.file_job.as_mut().unwrap().cut_clipboard = self.explorer.clipboard.clone();
        }
        started
    }

    pub(super) fn file_working(&self) -> bool {
        self.file_job.is_some()
    }

    /// Why `files` cannot change now (plain Korean), or None.
    fn file_busy_reason(&self, files: &[PathBuf]) -> Option<String> {
        if self.no_persist {
            return Some("명령 실행은 파일을 바꾸지 않아요.".into());
        }
        if self.file_job.is_some() || self.takm_job.is_some() || self.progress_job.is_some() || self.dirty_prompt.is_some() {
            return Some("진행 중인 작업이 끝난 뒤 다시 해 주세요.".into());
        }
        let key = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
        let open: HashSet<String> = self.editor_rows().into_iter().map(|row| key(&row.path)).collect();
        files.iter().find(|f| open.contains(&key(f)))
            .map(|f| format!("열려 있는 파일은 닫은 뒤 해 주세요: {}", crate::file_ops::name_of(f)))
    }

    fn start_file_work(&mut self, ctx: &egui::Context, files: &[PathBuf], work: Work, replay: Option<bool>) -> bool {
        let now = ctx.input(|i| i.time);
        if let Some(why) = self.file_busy_reason(files) {
            if replay.is_some() { self.history.note = Some((why, now)); } else { self.takm_note = Some((why, now)); }
            return false;
        }
        let record = match &work { Work::Undo(done) => Some(done.clone()), Work::Run(_) => None };
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = ctx.clone();
        let started = std::thread::Builder::new().name("explorer-files".into()).spawn(move || {
            let out = match work {
                Work::Run(op) => crate::file_ops::run(&op),
                Work::Undo(change) => crate::file_ops::undo(&change.done),
            };
            let _ = tx.send(out);
            wake.request_repaint();
        });
        if started.is_err() {
            self.takm_note = Some(("파일 작업을 시작할 수 없어요.".into(), now));
            return false;
        }
        if replay.is_none() {
            self.observe(true);
        }
        self.file_job = Some(Job { rx, replay, record, name_edit: false, cut_clipboard: None });
        true
    }

    /// Ctrl+Z / Ctrl+Y on a file change: run its inverse (`file_ops::undo`) on the worker.
    pub(super) fn replay_file_change(&mut self, ctx: &egui::Context, change: Arc<Change>, redo: bool) {
        let files = change.done.files();
        self.start_file_work(ctx, &files, Work::Undo(change), Some(redo));
    }

    pub(super) fn poll_file_job(&mut self, ctx: &egui::Context) {
        let Some(job) = self.file_job.take() else { return };
        let out = match job.rx.try_recv() {
            Ok(out) => out,
            Err(TryRecvError::Empty) => {
                self.file_job = Some(job);
                return;
            }
            Err(TryRecvError::Disconnected) => Outcome { done: None, skipped: vec!["파일 작업이 끝나지 않았어요. 폴더를 확인해 주세요.".into()] },
        };
        let now = ctx.input(|i| i.time);
        if let Some(done) = &out.done {
            if let Done::Moved(pairs) = done {
                for (from, to) in pairs { self.explorer.selection.moved(from, to); }
            }
            for dir in done.folders() {
                self.workspace.relist(&dir);
            }
        }
        let skipped = match out.skipped.as_slice() {
            [] => None,
            [one] => Some(one.clone()),
            [first, rest @ ..] => Some(format!("{first} (외 {}개)", rest.len())),
        };
        if job.name_edit {
            self.explorer.disk_edit_result(skipped.clone());
        }
        match (job.replay, job.record) {
            (Some(redo), Some(record)) => {
                let (from, to) = if redo { (&mut self.history.redo, &mut self.history.undo) } else { (&mut self.history.undo, &mut self.history.redo) };
                if let Some(done) = out.done {
                    if let Some(k) = from.iter().rposition(|action| matches!(action.as_slice(), [Part::Files(d)] if Arc::ptr_eq(d, &record))) {
                        from.remove(k);
                    }
                    let label = done.label();
                    let cut_clipboard = record.cut_clipboard.clone();
                    if let Some(clipboard) = &cut_clipboard {
                        if redo && self.explorer.clipboard.as_ref() == Some(clipboard) {
                            self.explorer.clipboard = None;
                        } else if !redo && self.explorer.clipboard.is_none() {
                            self.explorer.clipboard = Some(clipboard.clone());
                        }
                    }
                    to.push(vec![Part::Files(Arc::new(Change { done, cut_clipboard }))]);
                    let word = if redo { "Redo" } else { "Undo" };
                    self.history.note = Some((skipped.map_or_else(|| format!("{word} · {label}"), |s| format!("{word} · {label} · {s}")), now));
                } else {
                    // Nothing went back: the step stays where it is, the reason is shown.
                    self.history.note = Some((skipped.unwrap_or_else(|| "되돌릴 것이 없어요.".into()), now));
                }
                self.history.merge_open = false;
            }
            _ => {
                if let Some(done) = out.done {
                    let label = done.label();
                    let cut_clipboard = job.cut_clipboard.filter(|clip| self.explorer.clipboard.as_ref() == Some(clip));
                    if cut_clipboard.is_some() { self.explorer.clipboard = None; }
                    self.history.push(vec![Part::Files(Arc::new(Change { done, cut_clipboard }))]);
                    self.history.merge_open = false;
                    self.takm_note = Some((skipped.map_or_else(|| format!("{label} · Ctrl+Z"), |s| format!("{label} · Ctrl+Z · {s}")), now));
                } else if let Some(s) = skipped {
                    self.takm_note = Some((s, now));
                }
            }
        }
    }
}

/// `Find in Folder…`: every subfolder of `dir`, on a worker; the screen reads `found`.
pub(super) struct FolderFind {
    pub(super) dir: PathBuf,
    pub(super) needle: String,
    rx: Option<Receiver<(Vec<PathBuf>, bool)>>,
    /// Matching `.tak` / `.takm` files, sorted; `cut` = stopped at the cap.
    pub(super) found: Vec<PathBuf>,
    pub(super) cut: bool,
}

/// Most results one search keeps.
pub(super) const FIND_CAP: usize = 2000;

impl TakViewerApp {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn find_in_folder(&mut self, ctx: &egui::Context, dir: PathBuf, needle: String) {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = ctx.clone();
        let (d, n) = (dir.clone(), needle.clone());
        std::thread::spawn(move || {
            let _ = tx.send(crate::file_ops::find_in_folder(&d, &n, FIND_CAP));
            wake.request_repaint();
        });
        // A new search replaces the old one (its worker's answer is dropped).
        self.folder_find = Some(FolderFind { dir, needle, rx: Some(rx), found: vec![], cut: false });
    }

    pub(super) fn poll_folder_find(&mut self) {
        let Some(find) = &mut self.folder_find else { return };
        let Some(rx) = &find.rx else { return };
        match rx.try_recv() {
            Ok((found, cut)) => { find.found = found; find.cut = cut; find.rx = None; self.explorer.invalidate_rows(); }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => { find.rx = None; self.explorer.invalidate_rows(); }
        }
    }

    pub(super) fn folder_find_running(&self) -> bool {
        self.folder_find.as_ref().is_some_and(|f| f.rx.is_some())
    }
}
