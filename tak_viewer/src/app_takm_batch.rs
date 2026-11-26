//! Save many TAK files as `.takm`: a queue that runs the normal
//! one-at-a-time conversion (`app_takm.rs`) file after file, skips files that already have a
//! `.takm` (never opened, never overwritten), and ends with one summary in the status bar.
//! `Save All TAK as .takm` on a folder lists it on a worker first.

use super::*;
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, TryRecvError};

#[derive(Default)]
pub(super) struct Batch {
    pending: VecDeque<PathBuf>,
    /// The file being converted now.
    current: Option<PathBuf>,
    /// TAK files queued (the `n` of `k/n`).
    total: usize,
    saved: usize,
    /// Already `.takm` (the row itself or its sibling).
    skipped: usize,
    failed: Vec<String>,
    /// A folder being listed for `Save All TAK as .takm`.
    listing: Option<Receiver<Vec<PathBuf>>>,
}

/// A server TAK being saved as `.takm` on its server (not open in a view).
pub(super) struct RemoteConvert {
    location: crate::ssh::RemoteLocation,
    rx: Receiver<Result<tak_core::remote::RemoteWriteOutcome, String>>,
}

impl TakViewerApp {
    /// Queue `paths` for conversion: TAK files without a sibling `.takm`. Disk checks (sibling
    /// exists) run when each file's turn comes, on the conversion's own worker path.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn save_takm_many(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        let now = ctx.input(|i| i.time);
        if self.takm_batch.is_some() || self.takm_job.is_some() {
            self.takm_note = Some(("다른 파일을 .takm으로 만드는 중이에요. 끝난 뒤 다시 해 주세요.".into(), now));
            return;
        }
        let mut batch = Batch::default();
        let mut seen = std::collections::HashSet::new();
        for p in paths {
            if !seen.insert(p.clone()) { continue; }
            if tak_core::tak::is_take_path(&p) { batch.pending.push_back(p); } else { batch.skipped += 1; }
        }
        batch.total = batch.pending.len();
        self.takm_batch = Some(batch);
        self.advance_takm_batch(ctx);
    }

    /// `Save All TAK as .takm` on a folder: the TAK files directly inside it (not subfolders).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn save_folder_takm(&mut self, ctx: &egui::Context, dir: PathBuf) {
        if self.takm_batch.is_some() || self.takm_job.is_some() {
            self.takm_note = Some(("다른 파일을 .takm으로 만드는 중이에요. 끝난 뒤 다시 해 주세요.".into(), ctx.input(|i| i.time)));
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let mut found: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten()
                .map(|e| e.path()).filter(|p| tak_core::tak::is_take_path(p) && p.is_file()).collect();
            found.sort();
            let _ = tx.send(found);
            wake.request_repaint();
        });
        self.takm_batch = Some(Batch { listing: Some(rx), ..Batch::default() });
    }

    /// `Save as .takm` on a server TAK: an open one uses the open file's server save; an unopened
    /// one is converted by the server helper. An existing `.takm` there is never overwritten.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn save_remote_takm(&mut self, ctx: &egui::Context, location: crate::ssh::RemoteLocation) {
        let now = ctx.input(|i| i.time);
        if !tak_core::tak::is_take_path(Path::new(&location.path)) {
            self.takm_note = Some(("이미 .takm 파일이에요.".into(), now));
            return;
        }
        let key = location.memory_key();
        if self.views.iter().any(|view| view.path() == Some(key.as_path())) {
            self.save_takm(ctx, key);
            return;
        }
        if self.no_persist {
            self.takm_note = Some(("명령 실행은 서버에 쓰지 않아요.".into(), now));
            return;
        }
        if self.remote_convert.is_some() {
            self.takm_note = Some(("서버에서 다른 파일을 .takm으로 만드는 중이에요. 끝난 뒤 다시 해 주세요.".into(), now));
            return;
        }
        match self.ssh.convert(location.clone()) {
            Ok(rx) => {
                self.takm_note = Some((format!("Saving {} as .takm on the server…", crate::file_ops::name_of(Path::new(&location.path))), now));
                self.remote_convert = Some(RemoteConvert { location, rx });
            }
            Err(why) => self.takm_note = Some((why, now)),
        }
    }

    pub(super) fn poll_remote_convert(&mut self, ctx: &egui::Context) {
        let Some(job) = self.remote_convert.take() else { return };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => { self.remote_convert = Some(job); return; }
            Err(TryRecvError::Disconnected) => Err("SSH 연결이 끝났어요. 다시 연결해 주세요.".into()),
        };
        let name = crate::file_ops::name_of(&tak_core::takm::path_for(Path::new(&job.location.path), None));
        let line = match result {
            Ok(outcome) if outcome.existing => format!("Already on the server: {name}"),
            Ok(_) => {
                let _ = self.ssh.refresh(&job.location.host);
                format!("Saved {name} on the server")
            }
            Err(why) => why,
        };
        self.takm_note = Some((line, ctx.input(|i| i.time)));
    }

    /// Status bar words while a batch runs: `Saving 2/5 as .takm… b.tak`.
    pub(super) fn takm_batch_working(&self) -> Option<String> {
        let b = self.takm_batch.as_ref()?;
        if b.listing.is_some() {
            return Some("Listing TAK files…".into());
        }
        let current = b.current.as_ref()?;
        let k = b.total - b.pending.len();
        Some(format!("Saving {k}/{} as .takm… {}", b.total, crate::file_ops::name_of(current)))
    }

    /// Each frame after the conversion poll: count the finished file, start the next one, or
    /// show the summary when all are done.
    pub(super) fn advance_takm_batch(&mut self, ctx: &egui::Context) {
        let Some(mut batch) = self.takm_batch.take() else { return };
        if let Some(rx) = &batch.listing {
            match rx.try_recv() {
                Ok(found) => {
                    batch.listing = None;
                    batch.total = found.len();
                    batch.pending = found.into();
                }
                Err(TryRecvError::Empty) => { self.takm_batch = Some(batch); return; }
                Err(TryRecvError::Disconnected) => batch.listing = None,
            }
        }
        if self.takm_job.is_some() {
            self.takm_batch = Some(batch);
            return;
        }
        let now = ctx.input(|i| i.time);
        if let Some(done) = batch.current.take() {
            // The conversion wrote its own line (`Saved …` or the reason) into the note.
            let line = self.takm_note.as_ref().map_or("", |(line, _)| line.as_str());
            if line.starts_with("Saved ") { batch.saved += 1; }
            else if line.starts_with("Already .takm: ") { batch.skipped += 1; }
            else {
                batch.failed.push(format!("{}: {line}", crate::file_ops::name_of(&done)));
            }
        }
        while let Some(next) = batch.pending.pop_front() {
            batch.current = Some(next.clone());
            self.takm_note = None;
            self.save_takm(ctx, next);
            if self.takm_job.is_none() {
                // Refused at once: count it with its reason and go on.
                let why = self.takm_note.as_ref().map_or_else(String::new, |(line, _)| line.clone());
                let name = batch.current.take().map(|p| crate::file_ops::name_of(&p)).unwrap_or_default();
                batch.failed.push(format!("{name}: {why}"));
                continue;
            }
            self.takm_batch = Some(batch);
            return;
        }
        let mut line = format!("Saved {}", batch.saved);
        if batch.skipped > 0 { line += &format!(" · Skipped {} (already .takm)", batch.skipped); }
        if !batch.failed.is_empty() {
            line += &format!(" · 못 만든 파일 {}개: {}", batch.failed.len(), batch.failed[0]);
        }
        if batch.total + batch.skipped == 0 { line = "TAK 파일이 없어요.".into(); }
        self.takm_note = Some((line, now));
    }
}
