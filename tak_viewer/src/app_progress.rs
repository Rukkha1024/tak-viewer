use super::*;
use crate::take_progress::{Pick, Stamp, State, Target};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use tak_core::edit::save::{FileVersion, ProgressSaved};
use tak_core::tak::fast_read::AnalogJob;

type LocalResult = Result<(ProgressSaved, Result<Option<AnalogJob>, String>), String>;
type RemoteResult = Result<tak_core::remote::RemoteWriteOutcome, String>;

enum Work {
    Local(Receiver<LocalResult>),
    Remote(Receiver<RemoteResult>),
}

pub(super) struct Job {
    pick: Pick,
    replay: Option<bool>,
    work: Work,
}

#[derive(Default)]
struct OpenFile {
    loaded: bool,
    busy: bool,
    stamp: Option<(u64, Option<std::time::SystemTime>)>,
    version: Option<FileVersion>,
    emg: bool,
}

fn matches(view: &View, target: &Target) -> bool {
    match target {
        Target::Remote(location) => {
            view.remote.as_ref() == Some(location)
                || view
                    .remote_loading
                    .as_ref()
                    .is_some_and(|(_, pending, _)| pending == location)
        }
        Target::Local(path) => {
            view.remote.is_none()
                && (view.path() == Some(path)
                    || view.target() == Some(path)
                    || view
                        .source_stamp
                        .as_ref()
                        .and_then(|s| s.lock().ok()?.as_ref().map(|s| s.0 == *path))
                        .unwrap_or(false))
        }
    }
}

impl TakViewerApp {
    fn progress_open_file(&self, target: &Target) -> OpenFile {
        let mut result = OpenFile::default();
        let mut collect = |view: &View, pending: bool| {
            if !matches(view, target) {
                return;
            }
            let id = view.document_id();
            result.loaded |= view.data.is_some();
            result.busy |= pending
                || view.loading.is_some()
                || view.remote_loading.is_some()
                || view.is_ssh_preview()
                || view.waiting_shared_path().is_some()
                || view.analog_work_pending()
                || view.edit_refresh.is_some()
                || view.comments.pending != 0
                || id.is_some_and(|id| {
                    self.overlay_marker_ui
                        .get(&id)
                        .is_some_and(marker_edit::MarkerUi::has_pending_work)
                });
            if let Some(stamp) = view
                .source_stamp
                .as_ref()
                .and_then(|s| s.lock().ok()?.clone())
            {
                result.stamp = Some((stamp.1, stamp.2));
            }
            if let Some(version) = id
                .and_then(|id| self.marker_sessions.get(&id))
                .and_then(|s| s.current_version.as_ref())
            {
                result.version = Some(version.clone());
            }
            result.emg |= view.data.as_ref().is_some_and(|d| d.emg_pending);
        };
        let mut views = |views: &[View], ui: &[marker_edit::MarkerUi]| {
            for (view, ui) in views.iter().zip(ui) {
                collect(view, ui.has_pending_work());
                if let Some(owner) = view.overlay_owner() {
                    collect(owner, false);
                }
                if let Target::Local(path) = target {
                    if view.overlay_target() == Some(path.as_path()) && view.overlay_load.is_some()
                    {
                        collect(view, true);
                        // A loading overlay need not have an owner yet.
                        return true;
                    }
                }
            }
            false
        };
        let mut overlay_loading = views(&self.views, &self.marker_ui);
        for tab in self.layout_tabs.tabs() {
            if let Some(runtime) = &tab.layout {
                overlay_loading |= views(&runtime.views, &runtime.marker_ui);
            }
        }
        result.busy |= overlay_loading;
        result
    }

    pub(super) fn progress_writing(&self, path: &Path) -> bool {
        self.progress_job
            .as_ref()
            .is_some_and(|job| match &job.pick.target {
                Target::Local(target) => target == path,
                Target::Remote(location) => location.memory_key() == path,
            })
    }

    pub(super) fn pick_progress(&mut self, ctx: &egui::Context, pick: Pick) {
        self.start_progress(ctx, pick, None);
    }

    pub(super) fn start_progress(&mut self, ctx: &egui::Context, pick: Pick, replay: Option<bool>) {
        let open = self.progress_open_file(&pick.target);
        let refused = if self.no_persist {
            Some("명령 실행에서는 진행 상태를 저장할 수 없어요.")
        } else if !tak_core::takm::is_takm_path(pick.target.path()) {
            Some("진행 상태는 TAKM에서만 바꿀 수 있어요.")
        } else if self.progress_job.is_some()
            || self.takm_job.is_some()
            || open.busy
            || self.dirty_prompt.is_some()
            || matches!(&pick.target, Target::Local(p) if self.save_destination_reserved(p))
            || matches!(&pick.target, Target::Remote(l) if self.ssh_reads.values().any(|r| &r.location == l))
        {
            Some("진행 중인 작업이 끝난 뒤 진행 상태를 바꿔 주세요.")
        } else {
            None
        };
        if let Some(message) = refused {
            self.takm_note = Some((message.into(), ctx.input(|i| i.time)));
            return;
        }

        let before = pick.before.0;
        let after = pick.after.0;
        let work = match &pick.target {
            Target::Local(path) => {
                let path = path.clone();
                let wake = ctx.clone();
                let (tx, rx) = mpsc::channel();
                let stamp = open
                    .stamp
                    .or_else(|| pick.stamp.map(|s| (s.size, Some(s.modified))));
                let started = std::thread::Builder::new().name("save-progress".into()).spawn(move || {
                    let result = tak_core::edit::save::save_progress(&path, before, after, stamp, open.version.as_ref())
                        .map(|saved| {
                            let job = if open.emg {
                                tak_core::tak::fast_read::deferred_job_after_save(&saved.path)
                                    .map_err(|_| "파일을 다시 열어 근전도와 다른 장치 자료를 읽어 주세요.".into())
                            } else { Ok(None) };
                            (saved, job)
                        }).map_err(|error| match error {
                            tak_core::edit::EditError::FileChangedOnDisk =>
                                "파일이 바뀌었어요. 다시 연 뒤 진행 상태를 바꿔 주세요.".into(),
                            _ => "진행 상태를 저장할 수 없어요. 파일이 열리는지와 쓰기 권한을 확인해 주세요.".into(),
                        });
                    let _ = tx.send(result);
                    wake.request_repaint();
                });
                if started.is_err() {
                    self.takm_note = Some((
                        "저장 작업을 시작할 수 없어요.".into(),
                        ctx.input(|i| i.time),
                    ));
                    return;
                }
                Work::Local(rx)
            }
            Target::Remote(location) => {
                match self
                    .ssh
                    .write_progress(location.clone(), before, after, open.loaded)
                {
                    Ok(rx) => Work::Remote(rx),
                    Err(reason) => {
                        self.takm_note = Some((reason, ctx.input(|i| i.time)));
                        return;
                    }
                }
            }
        };
        if replay.is_none() {
            self.observe(true);
        }
        self.progress_job = Some(Job { pick, replay, work });
    }

    pub(super) fn poll_progress_write(&mut self, ctx: &egui::Context) {
        let Some(job) = self.progress_job.take() else {
            return;
        };
        let now = ctx.input(|i| i.time);
        let state = job.pick.after;
        let failure =
            || "진행 상태 저장이 끝나지 않았어요. 파일을 다시 열어 확인해 주세요.".to_owned();
        let result = match &job.work {
            Work::Local(rx) => match rx.try_recv() {
                Ok(result) => result
                    .map(|(saved, deferred)| self.apply_local_progress(saved, &job.pick, deferred)),
                Err(TryRecvError::Empty) => {
                    self.progress_job = Some(job);
                    return;
                }
                Err(TryRecvError::Disconnected) => Err(failure()),
            },
            Work::Remote(rx) => match rx.try_recv() {
                Ok(result) => result.map(|saved| {
                    if let Target::Remote(location) = &job.pick.target {
                        self.ssh
                            .cache_progress_write(location, saved.source, state.0);
                        self.update_progress_views(
                            &job.pick.target,
                            state,
                            saved.fixed_len,
                            None,
                            Ok(None),
                        );
                    }
                }),
                Err(TryRecvError::Empty) => {
                    self.progress_job = Some(job);
                    return;
                }
                Err(TryRecvError::Disconnected) => Err(failure()),
            },
        };
        if let Err(reason) = result {
            self.takm_note = Some((reason, now));
            return;
        }
        self.finish_progress_history(job.pick, job.replay, now);
    }

    fn apply_local_progress(
        &mut self,
        saved: ProgressSaved,
        pick: &Pick,
        deferred: Result<Option<AnalogJob>, String>,
    ) {
        if let Some(modified) = saved.stamp.1 {
            let stamp = Stamp {
                size: saved.stamp.0,
                modified,
            };
            let old = pick.stamp.or_else(|| {
                self.progress_open_file(&pick.target)
                    .stamp
                    .and_then(|(size, modified)| modified.map(|modified| Stamp { size, modified }))
            });
            self.explorer
                .progress
                .written(pick.target.path().to_owned(), stamp, pick.after, old);
        }
        self.update_progress_views(
            &pick.target,
            pick.after,
            saved.fixed_len,
            Some(&saved),
            deferred,
        );
    }

    fn update_progress_views(
        &mut self,
        target: &Target,
        state: State,
        fixed_len: u64,
        local: Option<&ProgressSaved>,
        deferred: Result<Option<AnalogJob>, String>,
    ) {
        let (mut job, failure) = match deferred {
            Ok(job) => (job, None),
            Err(reason) => (None, Some(reason)),
        };
        let sessions = &mut self.marker_sessions;
        let mut apply = |view: &mut View| {
            if !matches(view, target) || view.data.is_none() {
                return;
            }
            if let Some(saved) = local {
                if let Some(session) = view.document_id().and_then(|id| sessions.get_mut(&id)) {
                    session.current_version = saved.version.clone();
                }
                view.set_progress_stamp(saved.path.clone(), saved.stamp);
                view.clear_deferred_emg_reader();
                if view.data.as_ref().is_some_and(|d| d.emg_pending) {
                    if let Some(job) = job.take() {
                        view.install_deferred_emg_reader(job);
                    }
                    if let Some(reason) = &failure {
                        view.emg_requested = true;
                        view.emg_failure = Some(reason.clone());
                    }
                }
            }
            view.set_take_progress(state, fixed_len);
        };
        let mut apply_host = |view: &mut View| {
            apply(view);
            if let Some(owner) = view.overlay_owner_mut() {
                apply(owner);
            }
        };
        for view in &mut self.views {
            apply_host(view);
        }
        for tab in self.layout_tabs.tabs_mut() {
            if let Some(runtime) = &mut tab.layout {
                for view in &mut runtime.views {
                    apply_host(view);
                }
            }
        }
        for item in self.layout_tabs.closed_items_mut() {
            match item {
                layout_tabs::ClosedItem::View { view, .. } => apply_host(&mut view.view),
                layout_tabs::ClosedItem::Tab(closed) => {
                    if let Some(runtime) = &mut closed.tab.layout {
                        for view in &mut runtime.views {
                            apply_host(view);
                        }
                    }
                }
                layout_tabs::ClosedItem::Batch { .. } => {
                    unreachable!("closed_items_mut returns leaves")
                }
            }
        }
    }
}
