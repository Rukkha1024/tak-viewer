//! Headless screenshots of real in-memory label assignments.
use super::{egui, Pane, TakViewerApp};
use crate::ui::labeling::LabelingAction;
use std::sync::OnceLock;
use tak_core::edit::{LabelKey, Revision};

struct Step {
    source: String,
    target: String,
}

static STEPS: OnceLock<Vec<Step>> = OnceLock::new();

pub(crate) fn enabled() -> bool {
    STEPS.get().is_some()
}

pub(crate) fn take_probe(args: &mut Vec<String>, headless: bool) -> Result<(), &'static str> {
    let mut steps = Vec::new();
    while let Some(index) = args.iter().position(|arg| arg == "--shot-label") {
        let (source, target) = args
            .get(index + 1)
            .and_then(|arg| arg.split_once('='))
            .filter(|(source, target)| !source.is_empty() && !target.is_empty())
            .ok_or("--shot-label requires SOURCE=TARGET")?;
        steps.push(Step {
            source: source.into(),
            target: target.into(),
        });
        args.drain(index..index + 2);
    }
    if steps.is_empty() {
        return Ok(());
    }
    if !headless
        || !args.iter().any(|arg| arg == "--screenshot")
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--capture" | "--menu-capture" | "--overlay"))
    {
        return Err("--shot-label requires --headless --screenshot on one local take");
    }
    STEPS
        .set(steps)
        .map_err(|_| "--shot-label already configured")
}

#[derive(Default)]
pub(super) struct Progress {
    next: usize,
    pending: Option<Pending>,
    settle_frames: u8,
}

struct Pending {
    revision: Option<Revision>,
    marker_count: usize,
}

impl TakViewerApp {
    pub(super) fn label_shot_in_progress(&self, slot: usize) -> bool {
        self.auto.is_some()
            && self.label_shot.pending.is_some()
            && self.active == slot
            && self
                .views
                .get(slot)
                .is_some_and(|view| view.remote.is_none())
    }

    pub(super) fn drive_label_shot(&mut self, ctx: &egui::Context) -> bool {
        let Some(steps) = STEPS.get() else {
            return false;
        };
        let slot = self.active;
        if self.marker_job_running(slot) || self.views[slot].analog_work_pending() {
            return true;
        }
        let view = &mut self.views[slot];
        let Some(data) = &view.data else {
            return false;
        };
        if let Some(pending) = self.label_shot.pending.take() {
            let status = self.takm_note.as_ref().map_or("", |(text, _)| text);
            eprintln!("--shot-label: {status}");
            if data.edit_revision == pending.revision {
                return self.fail_label_shot(
                    ctx,
                    &steps[self.label_shot.next - 1],
                    "assignment did not change the take",
                );
            }
            if self.label_shot.next == steps.len() {
                // Rebuilds append new tracks. Keep the edited marker selected and reveal
                // the displaced row when this last step added one.
                if data.markers.len() > pending.marker_count
                    && data
                        .markers
                        .last()
                        .is_some_and(|marker| marker.kind == tak_core::tak::MarkerKind::Unlabeled)
                {
                    view.labels.scroll_to = Some(data.markers.len() - 1);
                }
                // Give the recreated graph cache time to schedule work before capture.
                self.label_shot.settle_frames = 5;
            }
        }
        if self.label_shot.settle_frames > 0 {
            self.label_shot.settle_frames -= 1;
            return true;
        }
        let Some(step) = steps.get(self.label_shot.next) else {
            return false;
        };
        let mut sources = data
            .markers
            .iter()
            .filter(|marker| marker.name == step.source);
        let Some(source) = sources
            .next()
            .filter(|marker| marker.present(view.pb.frame))
            .map(|marker| marker.uid)
            .filter(|_| sources.next().is_none())
        else {
            return self.fail_label_shot(
                ctx,
                step,
                "expected one source track with a point at this frame",
            );
        };
        let mut targets = data
            .markers
            .iter()
            .filter(|marker| marker.name == step.target);
        let target = targets
            .next()
            .and_then(|marker| {
                let (parent, slot) = marker.parent.zip(marker.slot)?;
                Some((LabelKey { parent, slot }, marker.uid))
            })
            .filter(|_| targets.next().is_none());
        let Some((target, track_uid)) = target else {
            return self.fail_label_shot(ctx, step, "expected one existing named target track");
        };
        self.label_shot.next += 1;
        self.label_shot.pending = Some(Pending {
            revision: data.edit_revision,
            marker_count: data.markers.len(),
        });
        self.show_pane(Pane::Labeling);
        self.labeling_action(
            ctx,
            slot,
            LabelingAction::SelectLabel {
                key: Some(target),
                track_uid: Some(track_uid),
                additive: false,
            },
        );
        self.assign_label_to_point(ctx, slot, source, target);
        true
    }

    fn fail_label_shot(&mut self, ctx: &egui::Context, step: &Step, reason: &str) -> bool {
        eprintln!("--shot-label {}={}: {reason}", step.source, step.target);
        self.failed = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        true
    }
}
