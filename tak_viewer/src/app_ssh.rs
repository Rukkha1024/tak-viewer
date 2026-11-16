//! Remote files use typed addresses. Their Take.path is an opaque in-memory key, never a disk path.
use super::*;
use crate::ssh::{RemoteLocation, SshAction, SshErrorKind, SshEvent};
use std::sync::Arc;
use tak_core::view_data::ViewData;

pub(super) const REMOTE_NO_COMMENTS: &str = "원격 파일에는 Comment를 달 수 없어요.";
const LOST: &str = "⚠ 연결이 끊겼어요. 받은 마커는 계속 볼 수 있어요.";
const CHANGED: &str = "⚠ 서버의 파일이 바뀌었어요. 다시 열면 처음부터 읽어요.";
/// Recent remote snapshots only (at most four). Undo can re-read older ones while connected.
pub(super) struct Cached {
    pub(super) key: PathBuf,
    data: Arc<ViewData>,
    comments: comments::Comments,
    spot: Option<Spot>,
}

pub(super) fn is_remote_key(path: &Path) -> bool {
    path.components()
        .next()
        .is_some_and(|c| c.as_os_str() == "@ssh")
}
pub(super) fn remote_name(location: &RemoteLocation) -> &str {
    location
        .path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&location.path)
}

impl TakViewerApp {
    pub fn ssh_probe_start(&mut self, _ctx: &egui::Context, probe: &crate::ssh_probe::Probe) -> Result<(), String> {
        self.set_command_run();
        self.ssh.add_host(&probe.alias);
        self.ssh.set_platform(&probe.alias, &probe.platform);
        self.ssh.set_python(&probe.alias, &probe.python)?;
        self.ssh.connect_at(&probe.alias, &probe.folder())
    }
    pub fn ssh_probe_open(&mut self, ctx: &egui::Context, host: &str, path: &str) {
        self.remote_action(ctx, SshAction::Open { location: RemoteLocation { host:host.into(), path:path.into() }, side:false });
    }
    pub fn ssh_probe_state(&self, alias: &str) -> crate::ssh_probe::State {
        let host = self.ssh.hosts.iter().find(|h| h.alias == alias);
        let view = self.view();
        crate::ssh_probe::State {
            connected: self.ssh.is_connected(alias), listing:self.ssh.listing_json(alias),
            error:host.and_then(|h|h.note.clone()).or_else(||self.ssh_note.as_ref().map(|n|n.0.clone())),
            diagnostic:host.and_then(|h|h.diagnostic.clone()), markers:view.remote.is_some() && view.data.is_some(),
            pending:view.data.as_ref().is_some_and(|d|d.analog_pending), busy:self.ssh.is_busy(),
            workers:self.ssh.workers_running(), bytes:self.ssh.received_bytes(alias),
            python:self.ssh.python_settings.get(alias).and_then(|s|s.remembered.clone()),
        }
    }
    pub fn ssh_probe_dump(&self, path: &Path) -> Result<(), String> {
        if tak_core::tak::is_take_path(path) || tak_core::takm::is_takm_path(path) { return Err("TAK · TAKM 경로에는 출력할 수 없어요.".into()); }
        let data = self.view().data.as_ref().ok_or("읽은 마커가 없어요.")?;
        let file = std::fs::File::create(path).map_err(|e|e.to_string())?;
        use std::io::Write;
        let mut writer = std::io::BufWriter::new(file);
        serde_json::to_writer(&mut writer, &tak_core::remote::verification_data(&data.take)).map_err(|e|e.to_string())?;
        writer.flush().map_err(|e|e.to_string())
    }
    pub fn ssh_probe_capture(&mut self, auto: crate::shot::AutoShot) { self.auto = Some(auto); }
    pub fn ssh_probe_disconnect(&mut self, alias: &str) { self.ssh.disconnect(alias); }
    pub fn ssh_probe_workers_running(&self) -> bool { self.ssh.workers_running() }

    pub(super) fn cache_remote(&mut self, slot: usize) {
        if self.ssh_reads.values().any(|(k, _)| *k == slot) {
            return;
        }
        let v = &self.views[slot];
        if v.remote.is_none() {
            return;
        }
        let Some(data) = &v.data else {
            return;
        };
        let key = data.take.path.clone();
        if let Some(i) = self.ssh_cache.iter().position(|c| c.key == key) {
            drop_later(self.ssh_cache.remove(i));
        }
        self.ssh_cache.push_back(Cached {
            key,
            data: data.clone(),
            comments: v.comments.list.clone(),
            spot: v.spot(),
        });
        while self.ssh_cache.len() > MAX_VIEWS {
            drop_later(self.ssh_cache.pop_front());
        }
    }

    pub(super) fn cancel_remote_load(&mut self, slot: usize) {
        if let Some((id, _, _)) = self.views[slot].remote_loading.take() {
            self.ssh.cancel_request(id);
            self.ssh_reads.remove(&id);
        }
        let ids: Vec<u64> = self
            .ssh_reads
            .iter()
            .filter(|(_, (k, _))| *k == slot)
            .map(|(&id, _)| id)
            .collect();
        for id in ids {
            self.ssh.cancel_request(id);
            self.ssh_reads.remove(&id);
        }
    }

    fn remote_action(&mut self, ctx: &egui::Context, action: SshAction) {
        match action {
            SshAction::Notice(s) => self.ssh_note = Some((s, ctx.input(|i| i.time))),
            SshAction::Open { location, side } => {
                let key = location.memory_key();
                self.ssh_sources.insert(key.clone(), location.clone());
                if let Some(k) = self.view_of(&key) {
                    if k == self.active && self.views[k].remote_warning.is_some() {
                        self.open_remote_in(ctx, k, location, false);
                    } else {
                        self.activate(k);
                    }
                    return;
                }
                let mut slot = self.active;
                if side && !self.open_views().is_empty() && !(self.open_views().len() == 1 && self.view().target().is_none()) {
                    let Some(new) =
                        dock::free_slot(&self.dock).filter(|_| self.file_count() < MAX_VIEWS)
                    else {
                        self.ssh_note = Some((
                            "파일은 화면에 얹은 것까지 4개만 열 수 있어요.".into(),
                            ctx.input(|i| i.time),
                        ));
                        return;
                    };
                    dock::add_view(&mut self.dock, new, self.dock_area);
                    slot = usize::from(new);
                    self.activate(slot);
                }
                self.open_remote_in(ctx, slot, location, false);
            }
        }
    }

    pub(super) fn open_remote_in(
        &mut self,
        ctx: &egui::Context,
        slot: usize,
        location: RemoteLocation,
        cached: bool,
    ) {
        if !self.restoring_layout && self.guard_dirty(slot, marker_edit::PendingTransition::Remote { slot, location: location.clone(), cached }) { return; }
        self.ensure_open_view(slot);
        self.cancel_pending_marker_job(slot);
        let key = location.memory_key();
        self.cancel_remote_load(slot);
        self.ssh_sources.insert(key.clone(), location.clone());
        self.remember(slot);
        let restore = self.views[slot]
            .spot()
            .filter(|_| self.views[slot].path() == Some(key.as_path()));
        if cached {
            if let Some(other) = self
                .open_views()
                .into_iter()
                .find(|&k| k != slot && self.views[k].path() == Some(key.as_path()))
            {
                self.cache_remote(other);
            }
            if let Some(i) = self.ssh_cache.iter().position(|c| c.key == key) {
                let c = self.ssh_cache.remove(i).unwrap();
                drop_later(self.views[slot].take_overlay());
                self.views[slot].cancel_load();
                self.views[slot].restore = c.spot.or(restore);
                self.show_remote(ctx, slot, location, c.data, c.comments);
                return;
            }
        }
        match self.ssh.open(location.clone()) {
            Ok(id) => {
                self.views[slot].cancel_load();
                self.views[slot].restore = restore;
                self.views[slot].remote_loading = Some((id, location.clone(), key));
                self.ssh_reads.insert(id, (slot, location));
                self.ssh_note = None;
            }
            Err(why) => {
                self.ssh_note = Some((why, ctx.input(|i| i.time)));
                self.history.quiet_layout = true;
                if self.views[slot].data.is_none() && self.open_views().len() > 1 {
                    self.close_view(slot);
                }
            }
        }
    }

    fn show_remote(
        &mut self,
        ctx: &egui::Context,
        slot: usize,
        location: RemoteLocation,
        data: Arc<ViewData>,
        list: comments::Comments,
    ) {
        self.next_file_id += 1;
        let (_, rx) = std::sync::mpsc::channel();
        let v = &mut self.views[slot];
        drop_later(v.take_overlay());
        drop_later(v.show_shared(
            data,
            rx,
            self.next_file_id,
            self.settings.speed,
            ctx.input(|i| i.time),
        ));
        // Remote analog comes as a new worker-prepared Arc, never through the local reader.
        v.analog_rx = None;
        v.comments_rx = None;
        v.comments.list = list;
        v.comments.target = Some(Err(REMOTE_NO_COMMENTS.into()));
        v.remote = Some(location);
        v.remote_warning = None;
    }

    pub(super) fn poll_ssh(&mut self, ctx: &egui::Context) {
        let events = self.ssh.drain_events();
        self.apply_ssh_events(ctx, events);
    }

    fn apply_ssh_events(&mut self, ctx: &egui::Context, events: Vec<SshEvent>) {
        for event in events {
            match event {
                SshEvent::Markers {
                    request_id,
                    location,
                    data,
                    comments,
                } => {
                    let want = self.ssh_reads.get(&request_id).cloned();
                    let Some((slot, expected)) = want else {
                        drop_later(Some(data));
                        continue;
                    };
                    if expected != location
                        || self.views[slot].remote_loading.as_ref().map(|l| l.0) != Some(request_id)
                    {
                        drop_later(Some(data));
                        continue;
                    }
                    let pending = data.analog_pending;
                    self.show_remote(ctx, slot, location, data, comments);
                    if !pending {
                        self.ssh_reads.remove(&request_id);
                    }
                }
                SshEvent::Analog {
                    request_id,
                    location,
                    part,
                } => {
                    let Some((slot, expected)) = self.ssh_reads.remove(&request_id) else {
                        drop_later(Some(part));
                        continue;
                    };
                    if expected == location
                        && self.views[slot].remote.as_ref() == Some(&location)
                        && self.views[slot].remote_loading.is_none()
                    {
                        if let Some(data) = self.views[slot].data.as_mut().and_then(Arc::get_mut) {
                            drop_later(Some(data.swap_prepared(part)));
                        } else {
                            drop_later(Some(part));
                            self.ssh_note = Some((
                                "아날로그를 붙이지 못했어요. 파일을 다시 열어 주세요.".into(),
                                ctx.input(|i| i.time),
                            ));
                        }
                    } else {
                        drop_later(Some(part));
                    }
                }
                SshEvent::Cancelled { request_id } => {
                    if let Some((slot, _)) = self.ssh_reads.remove(&request_id) {
                        if self.views[slot].remote_loading.as_ref().map(|l| l.0) == Some(request_id)
                        {
                            self.views[slot].remote_loading = None;
                            self.history.quiet_layout = true;
                            if self.views[slot].data.is_none() && self.open_views().len() > 1 {
                                self.close_view(slot);
                            }
                        }
                    }
                    if !self.views.iter().any(|v| v.remote_warning.is_some()) {
                        self.ssh_note =
                            Some(("SSH 읽기를 취소했어요.".into(), ctx.input(|i| i.time)));
                    }
                }
                SshEvent::Error {
                    request_id,
                    host,
                    message,
                    kind,
                } => {
                    let warning = match kind {
                        SshErrorKind::Lost => Some(LOST),
                        SshErrorKind::Changed => Some(CHANGED),
                        _ => None,
                    };
                    let changed = request_id
                        .and_then(|id| self.ssh_reads.get(&id))
                        .map(|(_, l)| l.clone());
                    for v in &mut self.views {
                        let applies = match kind {
                            SshErrorKind::Lost => v.remote.as_ref().is_some_and(|l| l.host == host),
                            SshErrorKind::Changed => {
                                v.remote.as_ref() == changed.as_ref() && changed.is_some()
                            }
                            _ => false,
                        };
                        if applies {
                            v.remote_warning = warning.map(str::to_owned);
                        }
                    }
                    if let Some(id) = request_id {
                        if let Some((slot, _)) = self.ssh_reads.remove(&id) {
                            if self.views[slot].remote_loading.as_ref().map(|l| l.0) == Some(id) {
                                self.views[slot].remote_loading = None;
                                self.history.quiet_layout = true;
                                if self.views[slot].data.is_none() && self.open_views().len() > 1 {
                                    self.close_view(slot);
                                }
                            }
                        }
                    }
                    self.ssh_note = Some((
                        warning.unwrap_or(&message).to_owned(),
                        ctx.input(|i| i.time),
                    ));
                }
            }
        }
    }

    /// Root lays out WORKSPACE above this section with enough reserved space for SSH.
    pub(super) fn ssh_section(&mut self, ui: &mut egui::Ui) {
        let current = self.view().remote.clone();
        let actions = crate::ui::ssh_explorer::show(ui, &mut self.ssh, current.as_ref());
        let ctx = ui.ctx().clone();
        for action in actions {
            self.remote_action(&ctx, action);
        }
    }

    pub(super) fn ssh_status(&self, ui: &mut egui::Ui) -> bool {
        let text = |s: String| RichText::new(s).size(12.0).color(theme::STATUS_TEXT);
        if let Some(note) = self.ssh.progress_note() {
            ui.label(text(note).strong());
            ui.label(text("Esc = 취소".into()));
            return true;
        }
        let v = self.view();
        if let Some((_, location, _)) = &v.remote_loading {
            ui.label(
                text(format!(
                    "{} · {} · 마커 읽는 중… {:.1} MB",
                    remote_name(location),
                    location.host,
                    self.ssh.received_bytes(&location.host) as f64 / 1e6
                ))
                .strong(),
            );
            ui.label(text("Esc = 취소".into()));
            return true;
        }
        let Some(location) = &v.remote else {
            return false;
        };
        ui.label(text(format!(
            "{} · {} · 읽기만",
            remote_name(location),
            location.host
        )));
        if let Some(warning) = &v.remote_warning {
            ui.label(text(warning.clone()).strong());
            return true;
        }
        if let Some(data) = &v.data {
            if let Some(p) = data.take.meta("Progress") {
                ui.label(text(format!("Progress {p}")));
            }
            ui.label(text(format!("{:.0} Hz", data.take.frame_rate)));
            let pending = self
                .ssh_reads
                .values()
                .any(|(k, l)| *k == self.active && l == location);
            let demo_pending = crate::ssh::demo_state().as_deref() == Some("markers_ready");
            if data.analog_pending && pending || demo_pending {
                ui.label(text(format!("포스플레이트 · EMG 읽는 중… {:.1} MB", self.ssh.received_bytes(&location.host) as f64 / 1e6)).strong());
                ui.label(text("Esc = 취소".into()));
            } else {
                let r = ui.add(
                    egui::Label::new(text(format!("Comments {}", v.comments.list.list().len())))
                        .sense(egui::Sense::click()),
                );
                if r.clicked() {
                    ui.ctx()
                        .data_mut(|d| d.insert_temp(egui::Id::new("ssh_open_comments"), true));
                }
            }
        }
        true
    }

    pub(super) fn apply_ssh_demo(&mut self, ctx: &egui::Context) {
        if self.ssh_demo_applied {
            return;
        }
        let Some(state) = crate::ssh::demo_state() else {
            return;
        };
        if !self.no_persist {
            return;
        }
        if self.views[0].busy() || self.views[0].data.is_none() {
            return;
        }
        self.ssh.set_demo(&state);
        if matches!(state.as_str(), "connected" | "markers_ready" | "lost") {
            let location = RemoteLocation {
                host: "CHO_workstation".into(),
                path: "~/data/2_gait/gait_001.tak".into(),
            };
            let key = location.memory_key();
            let v = &mut self.views[0];
            if let Some(data) = v.data.as_mut().and_then(Arc::get_mut) {
                data.take.path = key.clone();
            }
            v.comments.target = Some(Err(REMOTE_NO_COMMENTS.into()));
            for (id, start, end, text) in [
                (1, 450, 620, "서버의 기존 Comment"),
                (2, 900, 930, "읽기 전용 예시"),
            ] {
                let c = Comment {
                    id,
                    markers: vec!["LKNE".into()],
                    start,
                    end,
                    comment_type: String::new(),
                    text: text.into(),
                };
                v.comments
                    .list
                    .apply(&Event::of(EventKind::Add, &c, String::new(), String::new()));
            }
            v.remote = Some(location.clone());
            v.remote_warning = (state == "lost").then(|| LOST.to_owned());
            self.ssh_sources.insert(key, location);
        }
        self.ssh_demo_applied = true;
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn remote(ctx: &egui::Context) -> TakViewerApp {
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        let location = RemoteLocation {
            host: "CHO_workstation".into(),
            path: "/data/gait_001.takm".into(),
        };
        let key = location.memory_key();
        let data = Arc::new(views::sample_data(key.to_str().unwrap(), &["LKNE", "RKNE"]));
        let c = Comment {
            id: 1,
            markers: vec!["LKNE".into()],
            start: 2,
            end: 4,
            comment_type: "Jitter".into(),
            text: "서버의 Comment".into(),
        };
        let mut list = comments::Comments::default();
        list.apply(&Event::of(EventKind::Add, &c, String::new(), String::new()));
        app.ssh_sources.insert(key, location.clone());
        app.show_remote(ctx, 0, location, data, list);
        app
    }

    #[test]
    fn remote_boundaries_refuse_every_local_write_and_persistence_entry() {
        let ctx = egui::Context::default();
        let mut app = remote(&ctx);
        let key = app.views[0].path().unwrap().to_path_buf();
        assert_eq!(app.comment_writable(0).unwrap_err(), REMOTE_NO_COMMENTS);
        assert!(app.comment_target(0).is_none());
        assert!(app.takm_allowed(Some(&key)).unwrap_err().contains("원격"));
        app.overlay_on(&ctx, PathBuf::from("b.tak"));
        assert!(app.overlay_note.as_ref().unwrap().0.contains("원격"));
        let c = app.views[0].comments.list.get(1).unwrap().clone();
        assert!(!app.write_change(&ctx, 0, EventKind::Delete, &c));
        assert_eq!(app.views[0].comments.list.list().len(), 1);
        app.start_menu_capture();
        assert!(app.capture.is_none());
        app.sync_settings();
        assert!(app.settings.views.is_empty());
        assert!(app.settings.recent.is_empty());
        assert!(app.settings.file_memory.is_empty());
        app.say_no_comments(&ctx, 0);
        assert_eq!(app.ssh_note.as_ref().unwrap().0, REMOTE_NO_COMMENTS);
        assert!(app.comment_writer.is_none() && app.takm_job.is_none());
    }

    #[test]
    fn remote_close_undo_redo_restores_snapshot_without_network() {
        let ctx = egui::Context::default();
        let mut app = remote(&ctx);
        app.views[0].pb.set_frame(3, 0.0);
        app.views[0].range = Some((2, 4));
        app.quiet();
        app.remember(0);
        drop_later(app.views[0].clear());
        app.observe(true);
        app.undo_redo(&ctx, false);
        assert!(app.views[0].remote.is_some());
        assert_eq!(app.views[0].pb.frame, 3);
        assert_eq!(app.views[0].range, Some((2, 4)));
        assert_eq!(app.views[0].comments.list.list().len(), 1);
        assert!(app.views[0].loading.is_none() && app.views[0].remote_loading.is_none());
        app.undo_redo(&ctx, true);
        assert!(app.views[0].data.is_none());
    }

    #[test]
    fn unregistered_remote_key_never_falls_through_to_local_reader() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let location = RemoteLocation {
            host: "host".into(),
            path: "C:\\data\\a.tak".into(),
        };
        app.open_in(&ctx, 0, location.memory_key());
        assert!(app.views[0].loading.is_none());
        assert!(app.ssh_note.as_ref().unwrap().0.contains("SSH 목록"));
    }

    #[test]
    fn saving_settings_does_not_cancel_a_remote_read() {
        let ctx = egui::Context::default();
        let mut app = remote(&ctx);
        let location = app.views[0].remote.clone().unwrap();
        app.views[0].remote_loading = Some((99, location.clone(), location.memory_key()));
        app.ssh_reads.insert(99, (0, location));
        app.sync_settings();
        assert_eq!(app.views[0].remote_loading.as_ref().unwrap().0, 99);
        assert!(app.ssh_reads.contains_key(&99));
        assert!(app.settings.views.is_empty());
    }
    #[test]
    fn evicted_disconnected_remote_undo_skips_instead_of_claiming_success() {
        let ctx = egui::Context::default();
        let mut app = remote(&ctx);
        app.quiet();
        app.remember(0);
        drop_later(app.views[0].clear());
        app.observe(true);
        drop_later(Some(std::mem::take(&mut app.ssh_cache)));
        app.undo_redo(&ctx, false);
        assert!(app.views[0].data.is_none());
        let note = &app.history.note.as_ref().unwrap().0;
        assert!(note.contains("skipped"), "{note}");
        assert!(!note.contains("Open gait"));
    }
    #[test]
    fn staged_events_keep_marker_allocation_and_ignore_late_analog() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let location = RemoteLocation {
            host: "CHO_workstation".into(),
            path: "/data/a.tak".into(),
        };
        let key = location.memory_key();
        let mut data = views::sample_data(key.to_str().unwrap(), &["LKNE"]);
        data.analog_pending = true;
        app.views[0].remote_loading = Some((5, location.clone(), key));
        app.ssh_reads.insert(5, (0, location.clone()));
        app.apply_ssh_events(
            &ctx,
            vec![SshEvent::Markers {
                request_id: 5,
                location: location.clone(),
                data: Arc::new(data),
                comments: comments::Comments::default(),
            }],
        );
        let marker_allocation = Arc::as_ptr(app.views[0].data.as_ref().unwrap());
        let target = app.views[0].data.as_ref().unwrap().analog_target();
        app.cache_remote(0);
        assert!(
            app.ssh_cache.is_empty(),
            "an active analog read keeps its Arc unique"
        );
        let part = tak_core::view_data::AnalogPart::prepare(vec![], None, &target);
        app.apply_ssh_events(
            &ctx,
            vec![SshEvent::Analog {
                request_id: 5,
                location: location.clone(),
                part,
            }],
        );
        assert_eq!(
            Arc::as_ptr(app.views[0].data.as_ref().unwrap()),
            marker_allocation
        );
        assert!(!app.views[0].data.as_ref().unwrap().analog_pending);
        let part = tak_core::view_data::AnalogPart::prepare(vec![], None, &target);
        app.apply_ssh_events(
            &ctx,
            vec![SshEvent::Analog {
                request_id: 99,
                location,
                part,
            }],
        );
        assert_eq!(
            Arc::as_ptr(app.views[0].data.as_ref().unwrap()),
            marker_allocation
        );
    }

    #[test]
    fn source_changed_warning_applies_to_one_file_on_a_shared_host() {
        let ctx = egui::Context::default();
        let mut app = remote(&ctx);
        let location = app.views[0].remote.clone().unwrap();
        let other = RemoteLocation {
            host: location.host.clone(),
            path: "/data/b.takm".into(),
        };
        let key = other.memory_key();
        let data = Arc::new(views::sample_data(key.to_str().unwrap(), &["LKNE"]));
        app.show_remote(&ctx, 1, other, data, comments::Comments::default());
        app.ssh_reads.insert(5, (0, location));
        app.apply_ssh_events(
            &ctx,
            vec![SshEvent::Error {
                request_id: Some(5),
                host: "CHO_workstation".into(),
                message: String::new(),
                kind: SshErrorKind::Changed,
            }],
        );
        assert_eq!(app.views[0].remote_warning.as_deref(), Some(CHANGED));
        assert!(app.views[1].remote_warning.is_none());
    }
}
