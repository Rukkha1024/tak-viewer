//! Progress for many TAKM files at once (backend; the menu comes later): runs the normal
//! one-file Progress write (`app_progress.rs`) file after file and records ONE undo step. Ctrl+Z /
//! Ctrl+Y write every file back / again the same way.

use super::undo::Part;
use super::*;
use crate::take_progress::Pick;
use std::collections::VecDeque;

pub(super) struct Batch {
    pending: VecDeque<Pick>,
    done: Vec<Pick>,
    failed: Vec<String>,
    /// None = a new change; Some(redo) = undo (false) / redo (true) of `action`.
    replay: Option<bool>,
    /// The history step being replayed (its original picks, as stored).
    action: Vec<Pick>,
    after: crate::take_progress::State,
}

impl TakViewerApp {
    /// Set Progress on many files. TAK files are left out (Progress lives only in TAKM).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn set_progress_many(&mut self, ctx: &egui::Context, picks: Vec<Pick>) {
        let picks: Vec<Pick> = picks.into_iter().filter(|p| tak_core::takm::is_takm_path(p.target.path())).collect();
        let Some(after) = picks.first().map(|p| p.after) else {
            self.takm_note = Some(("진행 상태는 TAKM에서만 바꿀 수 있어요.".into(), ctx.input(|i| i.time)));
            return;
        };
        self.start_progress_batch(ctx, picks.clone(), picks, None, after);
    }

    fn start_progress_batch(&mut self, ctx: &egui::Context, run: Vec<Pick>, action: Vec<Pick>, replay: Option<bool>, after: crate::take_progress::State) {
        if self.progress_batch.is_some() || self.progress_job.is_some() {
            self.takm_note = Some(("진행 중인 작업이 끝난 뒤 진행 상태를 바꿔 주세요.".into(), ctx.input(|i| i.time)));
            return;
        }
        if replay.is_none() {
            self.observe(true);
        }
        self.progress_batch = Some(Batch { pending: run.into(), done: vec![], failed: vec![], replay, action, after });
        self.advance_progress_batch(ctx);
    }

    /// Ctrl+Z / Ctrl+Y on a several-file Progress step.
    pub(super) fn replay_progress_batch(&mut self, ctx: &egui::Context, action: Vec<Pick>, redo: bool) {
        let run = action.iter().map(|p| {
            let mut p = p.clone();
            if !redo { std::mem::swap(&mut p.before, &mut p.after); }
            p.stamp = None;
            p
        }).collect::<Vec<_>>();
        let after = run.first().map_or(action[0].after, |p| p.after);
        self.start_progress_batch(ctx, run, action, Some(redo), after);
    }

    /// A one-file write inside a batch finished (`finish_progress_history` hands it here).
    pub(super) fn progress_batch_took(&mut self, pick: Pick) -> bool {
        match &mut self.progress_batch {
            Some(batch) => { batch.done.push(pick); true }
            None => false,
        }
    }

    /// A one-file write inside a batch failed: keep its reason for the summary.
    pub(super) fn progress_batch_failed(&mut self, pick: &Pick, reason: &str) {
        if let Some(batch) = &mut self.progress_batch {
            batch.failed.push(format!("{}: {reason}", crate::file_ops::name_of(pick.target.path())));
        }
    }

    /// Each frame after the Progress poll: start the next file or finish the step.
    pub(super) fn advance_progress_batch(&mut self, ctx: &egui::Context) {
        if self.progress_job.is_some() { return; }
        let Some(mut batch) = self.progress_batch.take() else { return };
        while let Some(pick) = batch.pending.pop_front() {
            let name = crate::file_ops::name_of(pick.target.path());
            self.takm_note = None;
            // `replay: Some(..)` keeps the single-file history untouched; the batch records it.
            self.progress_batch = Some(batch);
            self.start_progress(ctx, pick, Some(true));
            batch = self.progress_batch.take().expect("batch kept");
            if self.progress_job.is_some() {
                self.progress_batch = Some(batch);
                return;
            }
            let why = self.takm_note.as_ref().map_or_else(String::new, |(line, _)| line.clone());
            batch.failed.push(format!("{name}: {why}"));
        }
        let now = ctx.input(|i| i.time);
        let changed = batch.done.len();
        let mut line = format!("{} · {changed} {}", batch.after.label(), if changed == 1 { "file" } else { "files" });
        if !batch.failed.is_empty() {
            line += &format!(" · 못 바꾼 파일 {}개: {}", batch.failed.len(), batch.failed[0]);
        }
        match batch.replay {
            None => {
                // Only the files that were written belong to the step.
                if !batch.done.is_empty() {
                    self.history.push(batch.done.into_iter().map(Part::Progress).collect());
                    self.history.merge_open = false;
                }
                self.takm_note = Some((line, now));
            }
            Some(redo) => {
                let (from, to) = if redo { (&mut self.history.redo, &mut self.history.undo) } else { (&mut self.history.undo, &mut self.history.redo) };
                let same = |a: &Vec<Part>| a.len() == batch.action.len() && a.iter().zip(&batch.action).all(|(p, q)| matches!(p, Part::Progress(p) if p.target == q.target && p.before == q.before && p.after == q.after));
                if let Some(k) = from.iter().rposition(same) {
                    let action = from.remove(k);
                    to.push(action);
                }
                let word = if redo { "Redo" } else { "Undo" };
                self.history.note = Some((format!("{word} · {line}"), now));
                self.history.merge_open = false;
            }
        }
    }
}
