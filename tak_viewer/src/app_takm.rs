
use super::*;
use std::sync::mpsc::{channel, Receiver};
use tak_core::takm;

/// How long the result line stays in the status bar (s).
pub(super) const NOTE_S: f64 = 8.0;

/// What a conversion thread found.
pub(super) enum Outcome {
    /// Made (the conversion's numbers).
    Made(takm::Converted),
    /// `<stem>.takm` is there already: not touched, opened instead.
    Exists(PathBuf),
    Failed(String),
}

/// A conversion running in the background.
pub(super) struct Job {
    pub(super) tak: PathBuf,
    rx: Receiver<Outcome>,
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The conversion of one TAK (background thread).
fn convert(tak: &Path) -> Outcome {
    let out = takm::path_for(tak, None);
    if out.exists() {
        return Outcome::Exists(out);
    }
    if takm::in_input(&out) {
        return Outcome::Failed("input/ 폴더 안이라 .takm을 만들지 않아요. TAK를 다른 폴더에 복사해서 여세요.".into());
    }
    match takm::convert(tak, None) {
        Ok(c) => Outcome::Made(c),
        // Made by someone else in between: still never overwritten.
        Err(_) if out.exists() => Outcome::Exists(out),
        Err(e) => Outcome::Failed(e),
    }
}

fn mb(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1e6)
}

impl TakViewerApp {
    /// Why `tak` cannot be saved as `.takm` now (plain Korean), Ok when it can. Disk is not
    /// looked at here (the thread does).
    pub(super) fn takm_allowed(&self, tak: Option<&Path>) -> Result<PathBuf, String> {
        let Some(tak) = tak else { return Err("지금 화면에 TAK를 열면 쓸 수 있어요.".into()) };
        if remote_ui::is_remote_key(tak) { return Err("원격 파일은 서버에서 .takm으로 만들어 주세요.".into()); }
        if !tak_core::tak::is_take_path(tak) {
            return Err("지금 화면의 파일이 TAK가 아니에요(이미 .takm).".into());
        }
        if self.no_persist {
            return Err("명령 실행은 파일을 만들지 않아요.".into());
        }
        if let Some(j) = &self.takm_job {
            return Err(format!("{}을(를) .takm으로 만드는 중이에요. 끝난 뒤에 다시 누르세요.", name_of(&j.tak)));
        }
        Ok(tak.to_path_buf())
    }

    /// Save `tak` as `.takm` (COMMENTS pane button, `파일` menu, Explorer menu): starts the
    /// background conversion, or says why not in the status bar.
    pub(super) fn save_takm(&mut self, ctx: &egui::Context, tak: PathBuf) {
        let now = ctx.input(|i| i.time);
        let tak = match self.takm_allowed(Some(&tak)) {
            Ok(t) => t,
            Err(why) => {
                self.takm_note = Some((why, now));
                return;
            }
        };
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        let t = tak.clone();
        #[cfg(test)]
        let gate = self.takm_gate.take();
        std::thread::spawn(move || {
            #[cfg(test)]
            if let Some(g) = gate {
                let _ = g.recv();
            }
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| convert(&t))).unwrap_or_else(|_| Outcome::Failed("만드는 중 내부 오류(panic)가 났어요.".into()));
            let _ = tx.send(outcome);
            ctx2.request_repaint();
        });
        self.takm_note = None;
        self.takm_job = Some(Job { tak, rx });
    }

    /// Status bar words while a conversion runs.
    pub(super) fn takm_working(&self) -> Option<String> {
        self.takm_job.as_ref().map(|j| format!("{} → .takm 만드는 중…", name_of(&j.tak)))
    }

    /// A finished conversion: the result line; views showing the TAK switch to the `.takm`.
    pub(super) fn poll_takm(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.takm_job else { return };
        let Ok(outcome) = job.rx.try_recv() else { return };
        let tak = self.takm_job.take().map(|j| j.tak).unwrap_or_default();
        let now = ctx.input(|i| i.time);
        let line = match outcome {
            Outcome::Made(c) => {
                let mut line = format!("{} 만들었어요 · {} → {}", name_of(&c.out), mb(c.tak_size), mb(c.takm_size));
                if c.csv_beside.is_some() {
                    line += &format!(" · 옆 CSV의 Comment 줄 {}개를 넣었어요", c.imported);
                }
                if let Some(dir) = c.out.parent() {
                    self.workspace.relist(dir);
                }
                self.switch_to_takm(ctx, &tak, &c.out);
                line
            }
            Outcome::Exists(out) => {
                // Explorer on a TAK no view shows: the existing file opens in the active view
                // (or its view is chosen).
                if !self.switch_to_takm(ctx, &tak, &out) {
                    self.open(ctx, out.clone());
                }
                format!("{}이(가) 이미 있어서 덮어쓰지 않고 그 파일을 열었어요.", name_of(&out))
            }
            Outcome::Failed(e) => format!("{}을(를) .takm으로 만들지 못했어요: {e}", name_of(&tak)),
        };
        self.takm_note = Some((line, now));
    }

    /// Every view showing `tak` shows `takm` instead, at the same spot (frame, camera, marker
    /// selection, plate), range and file laid over it. A file open (Ctrl+Z puts the TAK back).
    /// When `takm` is already open in a view, that view is chosen instead. False when no view
    /// showed `tak`.
    pub(super) fn switch_to_takm(&mut self, ctx: &egui::Context, tak: &Path, takm: &Path) -> bool {
        if self.save_destination_reserved(takm) {
            self.takm_note = Some(("Save in progress for this file.".into(), ctx.input(|i| i.time)));
            return true;
        }
        let slots: Vec<usize> = self.open_views().into_iter().filter(|&k| self.views[k].path() == Some(tak)).collect();
        if let Some(&slot) = slots.first() {
            if self.guard_dirty(slot, marker_edit::PendingTransition::SwitchTakm { tak: tak.to_path_buf(), takm: takm.to_path_buf() }) { return true; }
            self.cancel_pending_marker_job(slot);
        }
        if let Some(k) = self.view_of(takm) {
            if !slots.is_empty() {
                self.activate(k);
            }
            return !slots.is_empty();
        }
        let Some(&slot) = slots.first() else { return false };
        let v = &self.views[slot];
        let spot = v.spot().map(|s| Spot { path: takm.to_string_lossy().into_owned(), ..s });
        let (range, overlay, shown) = (v.range, v.overlay_target().map(Path::to_path_buf), v.shown);
        self.remember(slot);
        drop_later(self.views[slot].take_overlay());
        self.load_into(ctx, slot, takm.to_path_buf(), spot);
        self.error = None;
        self.views[slot].restore_range = Some(range);
        if let Some(o) = overlay {
            self.start_overlay(ctx, slot, o, true);
            self.views[slot].shown = shown;
        }
        true
    }

    fn load_into(&mut self, ctx: &egui::Context, slot: usize, path: PathBuf, spot: Option<Spot>) {
        #[cfg(test)]
        if self.hold_loads {
            self.views[slot].hold_load(path, spot);
            return;
        }
        self.views[slot].start_load(ctx, path, spot);
    }
}
