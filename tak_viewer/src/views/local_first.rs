//! Adapt the packed reader's stages to the viewer's existing preview data.

use super::{egui, CommentsLoad, Duration, Instant, Path, Sender, ViewData};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use tak_core::tak::fast_read::{read_take_fast, read_take_staged, FastRead, LocalStageKind};
use tak_core::tak::{Result, TakError, Uid};
use tak_core::takm::{self, v2::Decoder};

// Reserve 200 ms of the 300 ms open-to-scene budget for a frame and its geometry.
const FIRST_POSE_AFTER: Duration = Duration::from_millis(100);
type Runs = Vec<Vec<(i64, i64)>>;

pub(super) enum Stage {
    First(ViewData, CommentsLoad),
    Preview(ViewData),
}

pub(super) fn pose_wait(started: Instant) -> Duration {
    FIRST_POSE_AFTER.saturating_sub(started.elapsed())
}

pub(super) fn read(
    path: &Path,
    ctx: &egui::Context,
    cancel: &AtomicBool,
    tx: Sender<Stage>,
) -> Result<FastRead> {
    let mut sent = false;
    let result = read_take_staged(path, false, |stage| {
        let stopped = || TakError("파일 읽기를 멈췄어요.".into());
        if cancel.load(Ordering::Acquire) {
            return Err(stopped());
        }
        let first_frame = i64::from(stage.first_frame);
        let runs = exact_runs(stage.decoder, stage.take.trajectories.iter().map(|t| t.uid))?;
        let first = matches!(stage.kind, LocalStageKind::FirstPose);
        let data = ViewData::build_local_preview(stage.take, first_frame, runs, first);
        let msg = match stage.kind {
            LocalStageKind::FirstPose => {
                let comments = CommentsLoad {
                    parsed: Some(stage.comments.clone()),
                    label: Some(tak_core::comments::file_label(path, takm::fixed_len(path)?)),
                    target: takm::check_tail(path).map(|()| path.to_path_buf()),
                };
                Stage::First(data, comments)
            }
            LocalStageKind::Preview => Stage::Preview(data),
        };
        tx.send(msg).map_err(|_| stopped())?;
        sent = true;
        ctx.request_repaint();
        Ok(())
    });
    if result.is_err() && !sent && !cancel.load(Ordering::Acquire) {
        // For example, a damaged journal can block the combined first pose and Comments.
        // Exact marker reads still validate the archive and each used member.
        return read_take_fast(path, false);
    }
    result
}

fn exact_runs(decoder: &Decoder, uids: impl Iterator<Item = Uid>) -> Result<Runs> {
    let mut by_uid: HashMap<Uid, VecDeque<_>> = HashMap::new();
    for (header, runs) in decoder.spatial_presence() {
        let Some(record) = header.strip_prefix(tak_core::tak::channels::TRANSLATION) else {
            continue;
        };
        let uid: Uid = record
            .get(..16)
            .and_then(|uid| uid.try_into().ok())
            .ok_or_else(|| TakError("Missing TAKM2 spatial channel UID".into()))?;
        by_uid.entry(uid).or_default().push_back(runs);
    }
    uids.map(|uid| {
        by_uid
            .get_mut(&uid)
            .and_then(VecDeque::pop_front)
            .map(|runs| {
                runs.iter()
                    .map(|&(first, count)| {
                        let first = i64::from(first);
                        (first, first + i64::from(count) - 1)
                    })
                    .collect()
            })
            .ok_or_else(|| TakError("Missing TAKM2 marker presence".into()))
    })
    .collect()
}
