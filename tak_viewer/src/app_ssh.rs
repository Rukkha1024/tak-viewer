//! Remote files use typed addresses. Their Take.path is an opaque in-memory key, never a disk path.
use super::*;
use crate::ssh::{RemoteLocation, SshAction, SshErrorKind, SshEvent};
use std::sync::Arc;
use tak_core::view_data::ViewData;

pub(super) const REMOTE_NO_COMMENTS: &str = "원격 파일에는 Comment를 달 수 없어요.";
const LOST: &str = "⚠ 연결이 끊겼어요. 받은 마커는 계속 볼 수 있어요.";
const CHANGED: &str = "⚠ 서버의 파일이 바뀌었어요. 다시 열면 처음부터 읽어요.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RemoteOwner {
    tab: Option<layout_tabs::TabId>,
    slot: usize,
}

/// Requests keep their original layout identity while other layouts occupy the same slots.
#[derive(Clone)]
pub(super) struct RemoteRead {
    owner: RemoteOwner,
    location: RemoteLocation,
    file_id: Option<usize>,
    document: Option<tak_core::edit::DocumentId>,
}

impl RemoteRead {
    fn matches(&self, view: &View, request: u64) -> bool {
        if let Some(file_id) = self.file_id {
            view.file_id == file_id && view.remote.as_ref() == Some(&self.location)
                && view.remote_loading.is_none()
        } else {
            view.remote_loading.as_ref().is_some_and(|(id, location, _)| *id == request && location == &self.location)
        }
    }
}

/// Finish only the pending startup flag; an empty selection preserves every received sample.
fn stop_remote_analog_wait(view: &mut views::View, location: &RemoteLocation, message: &str) {
    if view.remote.as_ref() != Some(location) || view.remote_loading.is_some() { return; }
    let Some(data) = view.data.as_ref().filter(|data| data.analog_pending) else { return; };
    let target = data.analog_target();
    view.remote_warning.get_or_insert_with(|| message.to_owned());
    view.queue_analog(tak_core::view_data::AnalogPart::prepare_selected(
        Vec::new(), Some(message.to_owned()), &target, Vec::new(), true,
    ));
}
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
    fn remote_owner(&self, slot: usize) -> RemoteOwner {
        RemoteOwner { tab: self.layout_tabs.active_id(), slot }
    }

    fn remote_view(&self, owner: RemoteOwner) -> Option<&View> {
        if owner.tab == self.layout_tabs.active_id() { return self.views.get(owner.slot); }
        self.layout_tabs.tab(owner.tab?)?.layout.as_ref()?.views.get(owner.slot)
    }

    fn remote_view_mut(&mut self, owner: RemoteOwner) -> Option<&mut View> {
        if owner.tab == self.layout_tabs.active_id() { return self.views.get_mut(owner.slot); }
        self.layout_tabs.tab_mut(owner.tab?)?.layout.as_mut()?.views.get_mut(owner.slot)
    }

    fn find_remote_owner(&self, mut matches: impl FnMut(RemoteOwner, &View) -> bool) -> Option<RemoteOwner> {
        let tab = self.layout_tabs.active_id();
        for (slot, view) in self.views.iter().enumerate() {
            let owner = RemoteOwner { tab, slot };
            if matches(owner, view) { return Some(owner); }
        }
        for tab in self.layout_tabs.tabs() {
            let Some(runtime) = &tab.layout else { continue };
            for (slot, view) in runtime.views.iter().enumerate() {
                let owner = RemoteOwner { tab: Some(tab.id), slot };
                if matches(owner, view) { return Some(owner); }
            }
        }
        None
    }

    fn visit_remote_views(&mut self, mut visit: impl FnMut(RemoteOwner, &mut View)) {
        let tab = self.layout_tabs.active_id();
        for (slot, view) in self.views.iter_mut().enumerate() { visit(RemoteOwner { tab, slot }, view); }
        for tab in self.layout_tabs.tabs_mut() {
            let Some(runtime) = &mut tab.layout else { continue };
            for (slot, view) in runtime.views.iter_mut().enumerate() {
                visit(RemoteOwner { tab: Some(tab.id), slot }, view);
            }
        }
    }

    fn track_remote_read(&mut self, request: u64, slot: usize, location: RemoteLocation) {
        let view = &self.views[slot];
        let loaded = view.remote_loading.as_ref().map(|load| load.0) != Some(request);
        self.ssh_reads.insert(request, RemoteRead {
            owner: self.remote_owner(slot), location,
            file_id: loaded.then_some(view.file_id),
            document: loaded.then(|| view.document_id()).flatten(),
        });
    }

    /// Find the original request owner, or an open survivor of that same shared document.
    fn resolve_remote_read(&self, request: u64, read: &RemoteRead) -> Option<RemoteOwner> {
        if self.remote_view(read.owner).is_some_and(|view| read.matches(view, request)) { return Some(read.owner); }
        self.find_remote_owner(|_, view| read.matches(view, request)
            || (read.document.is_some() && view.document_id() == read.document
                && view.remote.as_ref() == Some(&read.location) && view.remote_loading.is_none()))
    }

    fn finish_remote_waiters(&mut self, key: &Path) {
        self.visit_remote_views(|_, view| {
            if view.waiting_shared_path() == Some(key) { view.clear_shared_wait(); }
        });
    }

    /// Generic shared attachment deliberately has no remote policy. Restore that boundary
    /// before returning to input handling; no remote key can reach a local editing API.
    fn attach_remote_waiters(&mut self, ctx: &egui::Context, location: &RemoteLocation) {
        let key = location.memory_key();
        if self.find_remote_owner(|_, view| view.waiting_shared_path() == Some(key.as_path())).is_none() { return; }
        self.attach_shared_waiters(ctx, &key);
        self.visit_remote_views(|_, view| {
            if view.path() == Some(key.as_path()) && view.waiting_shared_path().is_none() && view.remote_loading.is_none() {
                view.remote = Some(location.clone());
                view.comments.target = Some(Err(REMOTE_NO_COMMENTS.into()));
            }
        });
    }

    /// A deliberate reread replaces the live remote file for all its aliases. Its source is
    /// chosen explicitly so an older visible alias cannot satisfy newly attached waiters.
    fn publish_remote_aliases(&mut self, ctx: &egui::Context, source: RemoteOwner, location: &RemoteLocation) {
        let key = location.memory_key();
        let has_alias = self.find_remote_owner(|owner, view| owner != source
            && (view.waiting_shared_path() == Some(key.as_path())
                || (view.remote.as_ref() == Some(location) && view.remote_loading.is_none()))).is_some();
        if !has_alias { return; }
        let Some(shared) = self.remote_view_mut(source).and_then(View::shared_file) else { return };
        let mut next = self.next_file_id;
        let speed = self.settings.speed;
        let now = ctx.input(|input| input.time);
        self.visit_remote_views(|owner, view| {
            if owner == source { return; }
            let waiting = view.waiting_shared_path() == Some(key.as_path());
            if !waiting && (view.remote.as_ref() != Some(location) || view.remote_loading.is_some()) { return; }
            let selection = (!waiting).then(|| view.edit_selection_state());
            let picked = view.picked;
            let range = if waiting { view.restore_range } else { Some(view.range) };
            let restore = if waiting { view.restore.clone() } else { view.spot() };
            view.restore = restore;
            view.restore_range = range;
            next += 1;
            drop_later(view.show_shared_file(shared.clone(), next, speed, now));
            view.remote = Some(location.clone());
            view.remote_warning = None;
            view.comments.target = Some(Err(REMOTE_NO_COMMENTS.into()));
            view.picked = picked.filter(|id| view.comments.list.get(*id).is_some());
            if let (Some(selection), Some(data)) = (selection, view.data.as_ref()) {
                view.selected = data.markers.iter().enumerate().filter_map(|(index, marker)| selection.selected.contains(&marker.uid).then_some(index)).collect();
                view.selection_order = selection.order.iter().filter_map(|uid| data.markers.iter().position(|marker| &marker.uid == uid)).collect();
                view.primary = selection.primary.and_then(|uid| data.markers.iter().position(|marker| marker.uid == uid));
                view.armed_label = selection.armed_label;
                view.labels.scroll_to = view.primary;
            }
        });
        self.next_file_id = next;
    }
    pub fn ssh_probe_start(&mut self, _ctx: &egui::Context, probe: &crate::ssh_probe::Probe) -> Result<(), String> {
        self.set_command_run();
        self.ssh.full_analog_probe = probe.dump.is_some();
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
        if self.ssh_reads.values().any(|read| read.owner == self.remote_owner(slot)) {
            return;
        }
        let v = &self.views[slot];
        if v.remote.is_none() || v.remote_loading.is_some() || v.waiting_shared_path().is_some() || v.analog_work_pending() {
            return;
        }
        let Some(data) = &v.data else {
            return;
        };
        if data.analog_pending { return; }
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
        let owner = self.remote_owner(slot);
        let pending = self.views[slot].remote_loading.take().map(|load| load.0);
        let ids: std::collections::HashSet<u64> = self.ssh_reads.iter().filter(|(_, read)| read.owner == owner)
            .map(|(&id, _)| id).chain(pending).collect();
        for id in ids {
            let Some(mut read) = self.ssh_reads.remove(&id) else { continue };
            let key = read.location.memory_key();
            let next = self.find_remote_owner(|candidate, view| candidate != owner && if read.file_id.is_none() {
                view.waiting_shared_path() == Some(key.as_path())
            } else {
                view.remote.as_ref() == Some(&read.location) && view.remote_loading.is_none()
                    && read.document.is_some() && view.document_id() == read.document
            });
            if let Some(next) = next {
                if let Some(view) = self.remote_view_mut(next) {
                    if read.file_id.is_none() {
                        let restore = view.restore.clone();
                        view.clear_shared_wait();
                        view.restore = restore;
                        view.remote_loading = Some((id, read.location.clone(), key));
                    } else { read.file_id = Some(view.file_id); }
                }
                read.owner = next;
                self.ssh_reads.insert(id, read);
            } else {
                self.ssh.cancel_request(id);
            }
        }
    }

    /// A closed history item owns no SSH request. Keep an unfinished open as a waiter so
    /// reopening can adopt an open sibling or explicitly queue a fresh remote read.
    pub(super) fn prepare_close_remote_view(&mut self, slot: usize) {
        let unfinished = self.views[slot].remote_loading.as_ref().map(|(_, location, _)| location.memory_key())
            .or_else(|| self.views[slot].waiting_shared_path().filter(|path| is_remote_key(path)).map(Path::to_path_buf))
            .or_else(|| self.views[slot].remote.as_ref().filter(|_| self.views[slot].data.as_ref()
                .is_some_and(|data| data.analog_pending || (data.emg_pending && self.views[slot].emg_requested)))
                .map(RemoteLocation::memory_key));
        let restore = self.views[slot].restore.clone().or_else(|| self.views[slot].spot());
        self.cancel_remote_load(slot);
        if let Some(key) = unfinished { self.views[slot].wait_for_shared(key, restore); }
    }

    /// True means the key belongs to SSH, including disconnected/unknown addresses. The
    /// controller must never send those keys to its local start_load fallback.
    pub(super) fn promote_remote_shared_waiter(&mut self, ctx: &egui::Context, path: &Path) -> bool {
        if !is_remote_key(path) { return false; }
        let Some(location) = self.ssh_sources.get(path).cloned() else {
            self.finish_remote_waiters(path);
            self.ssh_note = Some(("이 원격 파일은 SSH 목록에서 다시 여세요.".into(), ctx.input(|i| i.time)));
            return true;
        };
        if self.find_shared_file(path, usize::MAX).is_some() {
            self.attach_remote_waiters(ctx, &location);
            return true;
        }
        if self.ssh_reads.values().any(|read| read.location == location && read.file_id.is_none()) { return true; }
        let Some(owner) = self.find_remote_owner(|_, view| view.waiting_shared_path() == Some(path)) else { return true };
        if !self.allow_open_file(path, None) { return true; }
        match self.ssh.open(location.clone()) {
            Ok(id) => {
                if let Some(view) = self.remote_view_mut(owner) {
                    let restore = view.restore.clone();
                    view.clear_shared_wait();
                    view.restore = restore;
                    view.remote_loading = Some((id, location.clone(), path.to_path_buf()));
                }
                self.ssh_reads.insert(id, RemoteRead { owner, location, file_id: None, document: None });
            }
            Err(reason) => {
                self.finish_remote_waiters(path);
                self.ssh_note = Some((reason, ctx.input(|i| i.time)));
            }
        }
        true
    }

    fn remote_action(&mut self, ctx: &egui::Context, action: SshAction) {
        match action {
            SshAction::Notice(s) => self.ssh_note = Some((s, ctx.input(|i| i.time))),
            SshAction::Open { location, side } => {
                let key = location.memory_key();
                self.ssh_sources.insert(key.clone(), location.clone());
                let mut slot = self.active;
                if side && !self.open_views().is_empty() && !(self.open_views().len() == 1 && self.view().target().is_none()) {
                    if !self.allow_open_file(&key, None) { return; }
                    let Some(new) = dock::free_slot(&self.dock)
                    else {
                        self.ssh_note = Some((
                            "한 탭에는 3D 화면을 4개까지 열 수 있어요.".into(),
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
        let created = self.layout_tabs.active_id().is_none();
        let key = location.memory_key();
        if !self.allow_open_file(&key, Some(slot)) { return; }
        let reload = self.views[slot].remote.as_ref() == Some(&location) && self.views[slot].remote_warning.is_some() && !cached;
        if self.views[slot].target() == Some(key.as_path()) && !reload { return; }
        let shared_elsewhere = self.views[slot].target().is_some_and(|old| self.file_has_other_view(slot, old));
        if !self.restoring_layout
            && self.guard_dirty(slot, marker_edit::PendingTransition::Remote { slot, location: location.clone(), cached }) { return; }
        if shared_elsewhere {
            let work = self.views[slot].owns_shared_file_work() || self.marker_ui[slot].has_pending_work();
            if self.comments_unsaved(slot) || (!self.handoff_shared_owner(slot, false) && work) {
                self.notice = Some("읽기나 편집을 마친 뒤 다시 열어 주세요.".into());
                return;
            }
        }
        if created { self.observe(true); }
        self.ensure_open_view(slot);
        self.cancel_pending_marker_job(slot);
        self.cancel_remote_load(slot);
        self.ssh_sources.insert(key.clone(), location.clone());
        self.remember(slot);
        let restore = self.views[slot]
            .spot()
            .filter(|_| self.views[slot].path() == Some(key.as_path()));
        if !reload {
            if let Some(shared) = self.find_shared_file(&key, slot) {
                self.next_file_id += 1;
                self.views[slot].restore = restore;
                drop_later(self.views[slot].take_overlay());
                drop_later(self.views[slot].show_shared_file(shared, self.next_file_id, self.settings.speed, ctx.input(|i| i.time)));
                self.views[slot].remote = Some(location);
                self.views[slot].comments.target = Some(Err(REMOTE_NO_COMMENTS.into()));
                self.record_implicit_file_open(created);
                return;
            }
            if self.ssh_reads.values().any(|read| read.location == location && read.file_id.is_none()) {
                self.views[slot].wait_for_shared(key, restore);
                self.record_implicit_file_open(created);
                return;
            }
        }
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
                self.record_implicit_file_open(created);
                return;
            }
        }
        match self.ssh.open(location.clone()) {
            Ok(id) => {
                self.views[slot].cancel_load();
                self.views[slot].restore = restore;
                self.views[slot].remote_loading = Some((id, location.clone(), key));
                self.track_remote_read(id, slot, location);
                self.ssh_note = None;
                self.record_implicit_file_open(created);
            }
            Err(why) => {
                self.ssh_note = Some((why, ctx.input(|i| i.time)));
                self.history.quiet_layout = true;
                if self.views[slot].data.is_none() && self.open_views().len() > 1 {
                    self.close_view(slot);
                }
                self.record_implicit_file_open(created);
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
        self.show_remote_at(ctx, self.remote_owner(slot), location, data, list);
    }

    fn show_remote_at(&mut self, ctx: &egui::Context, owner: RemoteOwner, location: RemoteLocation,
                      data: Arc<ViewData>, list: comments::Comments) -> Option<usize> {
        self.next_file_id += 1;
        let file_id = self.next_file_id;
        let speed = self.settings.speed;
        let (_, rx) = std::sync::mpsc::channel();
        let v = self.remote_view_mut(owner)?;
        drop_later(v.take_overlay());
        drop_later(v.show_shared(
            data,
            rx,
            file_id,
            speed,
            ctx.input(|i| i.time),
        ));
        // Remote analog comes as a new worker-prepared Arc, never through the local reader.
        v.analog_rx = None;
        v.comments_rx = None;
        v.comments.list = list;
        v.comments.target = Some(Err(REMOTE_NO_COMMENTS.into()));
        v.remote = Some(location);
        v.remote_warning = None;
        Some(file_id)
    }

    pub(super) fn poll_ssh(&mut self, ctx: &egui::Context) {
        let events = self.ssh.drain_events();
        self.apply_ssh_events(ctx, events);
    }

    /// Device/graph access only queues the existing SSH worker once per open file.
    pub(super) fn poll_remote_emg(&mut self, ctx: &egui::Context) {
        for slot in 0..self.views.len() {
            self.views[slot].sync_shared();
            let Some(location) = self.views[slot].remote.clone() else { continue; };
            if !self.views[slot].devices.request_emg { continue; }
            self.views[slot].devices.request_emg = false;
            if self.views[slot].emg_requested || !self.views[slot].data.as_ref().is_some_and(|data| data.emg_pending) { continue; }
            if self.ssh_reads.values().any(|read| read.location == location) {
                self.views[slot].devices.request_emg = true;
                continue;
            }
            let target = self.views[slot].data.as_ref().unwrap().analog_target();
            let failure = match self.ssh.request_analog(location.clone(), target) {
                Ok(id) => { self.track_remote_read(id, slot, location.clone()); None }
                Err(why) => { self.ssh_note = Some((why.clone(), ctx.input(|i| i.time))); Some(why) }
            };
            self.visit_remote_views(|_, view| {
                if view.remote.as_ref() == Some(&location) {
                    view.emg_requested = true;
                    view.emg_failure = failure.clone();
                }
            });
        }
    }

    fn finish_remote_read(&mut self, request: u64, read: RemoteRead, message: &str) {
        let mut force_wait = false;
        if let Some(owner) = self.resolve_remote_read(request, &read) {
            let mut failed_initial = false;
            if let Some(view) = self.remote_view_mut(owner) {
                force_wait = read.file_id.is_some() && view.data.as_ref().is_some_and(|data| data.analog_pending);
                stop_remote_analog_wait(view, &read.location, message);
                if read.file_id.is_some() && view.remote.as_ref() == Some(&read.location) && view.emg_requested {
                    view.emg_failure = Some(message.to_owned());
                }
                if view.remote_loading.as_ref().map(|load| load.0) == Some(request) {
                    view.remote_loading = None;
                    failed_initial = true;
                }
            }
            if failed_initial { self.history.quiet_layout = true; }
        }
        if read.file_id.is_some() {
            self.visit_remote_views(|_, view| {
                if view.remote.as_ref() == Some(&read.location)
                    && (read.document.is_none() || view.document_id() == read.document) {
                    if force_wait { view.remote_warning.get_or_insert_with(|| message.to_owned()); }
                    if view.emg_requested { view.emg_failure = Some(message.to_owned()); }
                }
            });
        }
        if read.file_id.is_none() { self.finish_remote_waiters(&read.location.memory_key()); }
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
                    let Some(mut read) = self.ssh_reads.get(&request_id).cloned() else {
                        drop_later(Some(data));
                        continue;
                    };
                    let owner = self.resolve_remote_read(request_id, &read);
                    if read.location != location || read.file_id.is_some() || owner.is_none() {
                        drop_later(Some(data));
                        self.ssh_reads.remove(&request_id);
                        continue;
                    }
                    let owner = owner.unwrap();
                    let pending = data.analog_pending;
                    read.owner = owner;
                    read.file_id = self.show_remote_at(ctx, owner, location.clone(), data, comments);
                    read.document = self.remote_view(owner).and_then(View::document_id);
                    if !pending {
                        self.ssh_reads.remove(&request_id);
                    } else { self.ssh_reads.insert(request_id, read); }
                    let older: Vec<_> = self.ssh_reads.iter().filter(|(id, read)| **id != request_id && read.location == location)
                        .map(|(&id, _)| id).collect();
                    for id in older { self.ssh_reads.remove(&id); self.ssh.cancel_request(id); }
                    self.publish_remote_aliases(ctx, owner, &location);
                }
                SshEvent::Analog {
                    request_id,
                    location,
                    part,
                } => {
                    let Some(read) = self.ssh_reads.remove(&request_id) else {
                        drop_later(Some(part));
                        continue;
                    };
                    let owner = self.resolve_remote_read(request_id, &read);
                    if read.location == location && read.file_id.is_some() && owner.is_some() {
                        self.remote_view_mut(owner.unwrap()).unwrap().queue_analog(part);
                    } else {
                        drop_later(Some(part));
                    }
                }
                SshEvent::Cancelled { request_id } => {
                    if let Some(read) = self.ssh_reads.remove(&request_id) {
                        self.finish_remote_read(request_id, read, "SSH 읽기를 취소했어요. 받은 마커와 힘판은 그대로 볼 수 있어요.");
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
                        .map(|read| read.location.clone());
                    self.visit_remote_views(|_, v| {
                        let applies = match kind {
                            SshErrorKind::Lost => v.remote.as_ref().is_some_and(|l| l.host == host),
                            SshErrorKind::Changed => {
                                v.remote.as_ref() == changed.as_ref() && changed.is_some()
                            }
                            _ => false,
                        };
                        if applies {
                            v.remote_warning = warning.map(str::to_owned);
                            if request_id.is_none() && kind == SshErrorKind::Lost {
                                if let Some(location) = v.remote.clone() {
                                    stop_remote_analog_wait(v, &location, LOST);
                                }
                            }
                        }
                    });
                    if let Some(id) = request_id {
                        if let Some(read) = self.ssh_reads.remove(&id) {
                            self.finish_remote_read(id, read, warning.unwrap_or(&message));
                        }
                    } else if kind == SshErrorKind::Lost {
                        let ids: Vec<_> = self.ssh_reads.iter().filter(|(_, read)| read.location.host == host).map(|(&id, _)| id).collect();
                        for id in ids {
                            if let Some(read) = self.ssh_reads.remove(&id) { self.finish_remote_read(id, read, LOST); }
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
                .any(|read| read.owner == self.remote_owner(self.active) && &read.location == location);
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

    fn next_tab(app: &mut TakViewerApp) -> layout_tabs::TabId {
        let previous = app.layout_tabs.active_id().unwrap();
        let next = app.layout_tabs.add(Some(LayoutRuntime::default()));
        app.layout_tabs.activate(previous);
        assert!(app.switch_layout_tab_internal(next));
        next
    }

    fn pending(app: &mut TakViewerApp, request: u64, location: &RemoteLocation) {
        app.views[0].remote_loading = Some((request, location.clone(), location.memory_key()));
        app.track_remote_read(request, 0, location.clone());
    }

    #[test]
    fn markers_and_cancel_route_to_original_hidden_tab_despite_reused_slot() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let first = app.layout_tabs.active_id();
        let location = RemoteLocation { host: "host".into(), path: "/a.takm".into() };
        pending(&mut app, 91, &location);
        next_tab(&mut app);
        let other = RemoteLocation { host: "host".into(), path: "/b.takm".into() };
        let key = other.memory_key();
        app.show_remote(&ctx, 0, other.clone(), Arc::new(views::sample_data(key.to_str().unwrap(), &["B"])), comments::Comments::default());
        let visible = app.views[0].data.clone().unwrap();
        app.cancel_remote_load(0);
        assert!(app.ssh_reads.contains_key(&91), "cancelling reused slot must not cancel another tab");
        let key = location.memory_key();
        let mut data = views::sample_data(key.to_str().unwrap(), &["A"]);
        data.analog_pending = true;
        app.apply_ssh_events(&ctx, vec![SshEvent::Markers { request_id: 91, location: location.clone(), data: Arc::new(data), comments: comments::Comments::default() }]);
        assert!(Arc::ptr_eq(app.views[0].data.as_ref().unwrap(), &visible));
        let owner = RemoteOwner { tab: first, slot: 0 };
        assert_eq!(app.remote_view(owner).unwrap().remote.as_ref(), Some(&location));
        app.apply_ssh_events(&ctx, vec![SshEvent::Cancelled { request_id: 91 }]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.remote_view(owner).unwrap().data.as_ref().unwrap().analog_pending {
            app.poll_hidden_file_work(&ctx);
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(app.remote_view(owner).unwrap().remote_warning.is_some());
        assert!(app.views[0].remote_warning.is_none());
        assert_eq!(app.views[0].remote.as_ref(), Some(&other));
    }

    #[test]
    fn duplicate_remote_open_waits_once_then_shares_force_data_and_read_only_policy() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let first = app.layout_tabs.active_id();
        let location = RemoteLocation { host: "host".into(), path: "/a.takm".into() };
        pending(&mut app, 92, &location);
        next_tab(&mut app);
        app.open_remote_in(&ctx, 0, location.clone(), false);
        assert_eq!(app.views[0].waiting_shared_path(), Some(location.memory_key().as_path()));
        assert_eq!(app.ssh_reads.len(), 1);
        assert!(app.views[0].loading.is_none(), "opaque keys never reach the local reader");
        let key = location.memory_key();
        let mut data = views::sample_data(key.to_str().unwrap(), &["A"]);
        data.analog_pending = true;
        app.apply_ssh_events(&ctx, vec![SshEvent::Markers { request_id: 92, location: location.clone(), data: Arc::new(data), comments: comments::Comments::default() }]);
        let owner = RemoteOwner { tab: first, slot: 0 };
        assert!(Arc::ptr_eq(app.views[0].data.as_ref().unwrap(), app.remote_view(owner).unwrap().data.as_ref().unwrap()));
        assert!(app.views[0].shares_document(app.remote_view(owner).unwrap()));
        assert_eq!(app.views[0].remote.as_ref(), Some(&location));
        assert_eq!(app.comment_writable(0).unwrap_err(), REMOTE_NO_COMMENTS);
        assert!(app.takm_allowed(Some(&key)).unwrap_err().contains("원격"));
        app.views[0].pb.set_frame(7, 0.0);
        app.views[0].select(0, false);
        let target = app.views[0].data.as_ref().unwrap().analog_target();
        let analog = vec![tak_core::tak::AnalogDevice { channels: vec![tak_core::tak::AnalogChannel {
            name: "AnalogChannel0".into(), kind: "Analog".into(), dim: 1, frames: vec![0], values: vec![42.0],
        }] }];
        app.apply_ssh_events(&ctx, vec![SshEvent::Analog { request_id: 92, location: location.clone(), part: tak_core::view_data::AnalogPart::prepare(analog, None, &target) }]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.views[0].data.as_ref().unwrap().analog_pending {
            app.poll_hidden_file_work(&ctx);
            app.views[0].poll_analog();
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(Arc::ptr_eq(app.views[0].data.as_ref().unwrap(), app.remote_view(owner).unwrap().data.as_ref().unwrap()));
        assert_eq!(app.views[0].data.as_ref().unwrap().take.analog[0].channels[0].values, [42.0]);
        assert_eq!(app.views[0].pb.frame, 7);
        assert_eq!(app.views[0].primary, Some(0));
        assert!(app.ssh_reads.is_empty());
    }

    #[test]
    fn closing_initial_remote_owner_hands_request_to_waiting_tab() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let first = app.layout_tabs.active_id().unwrap();
        let location = RemoteLocation { host: "host".into(), path: "/a.takm".into() };
        pending(&mut app, 93, &location);
        let second = next_tab(&mut app);
        app.open_remote_in(&ctx, 0, location.clone(), false);
        app.switch_layout_tab_internal(first);
        app.cancel_remote_load(0);
        drop_later(app.views[0].clear());
        assert_eq!(app.ssh_reads[&93].owner.tab, Some(second));
        let key = location.memory_key();
        app.apply_ssh_events(&ctx, vec![SshEvent::Markers { request_id: 93, location: location.clone(), data: Arc::new(views::sample_data(key.to_str().unwrap(), &["A"])), comments: comments::Comments::default() }]);
        assert!(app.views[0].data.is_none());
        assert_eq!(app.remote_view(RemoteOwner { tab: Some(second), slot: 0 }).unwrap().remote.as_ref(), Some(&location));
    }

    #[test]
    fn hidden_initial_failure_clears_waiters_without_touching_their_previous_file() {
        let ctx = egui::Context::default();
        let mut app = remote(&ctx);
        let previous = app.views[0].data.clone().unwrap();
        let first = app.layout_tabs.active_id().unwrap();
        let location = RemoteLocation { host: "host".into(), path: "/missing.takm".into() };
        pending(&mut app, 94, &location);
        let second = next_tab(&mut app);
        app.open_remote_in(&ctx, 0, location, false);
        app.apply_ssh_events(&ctx, vec![SshEvent::Error { request_id: Some(94), host: "host".into(), message: "읽지 못했어요".into(), kind: SshErrorKind::Failed }]);
        assert!(app.views[0].waiting_shared_path().is_none());
        assert!(app.ssh_reads.is_empty());
        assert_eq!(app.layout_tabs.active_id(), Some(second));
        let owner = app.remote_view(RemoteOwner { tab: Some(first), slot: 0 }).unwrap();
        assert!(owner.remote_loading.is_none());
        assert!(Arc::ptr_eq(owner.data.as_ref().unwrap(), &previous));
    }

    #[test]
    fn prepared_close_and_remote_promotion_keep_opaque_keys_out_of_local_reader() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let location = RemoteLocation { host: "host".into(), path: "/a.takm".into() };
        let key = location.memory_key();
        app.ssh_sources.insert(key.clone(), location.clone());
        pending(&mut app, 95, &location);
        app.prepare_close_remote_view(0);
        assert!(app.ssh_reads.is_empty());
        assert!(app.views[0].remote_loading.is_none());
        assert_eq!(app.views[0].waiting_shared_path(), Some(key.as_path()));
        assert!(app.promote_remote_shared_waiter(&ctx, &key));
        assert!(app.views[0].loading.is_none(), "disconnected SSH must not use local file IO");
        assert!(app.views[0].remote_loading.is_none());
        assert!(app.views[0].waiting_shared_path().is_none());
        assert!(app.ssh_note.as_ref().unwrap().0.contains("연결"));
        assert!(!app.promote_remote_shared_waiter(&ctx, Path::new("local.tak")));
    }

    #[test]
    fn remote_distinct_cap_includes_hidden_tabs_and_allows_repeated_shared_file() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let first = RemoteLocation { host: "host".into(), path: "/0.takm".into() };
        for tab in 0..3 {
            if tab > 0 { next_tab(&mut app); }
            for slot in 0..4 {
                app.ensure_open_view(slot);
                if !app.open_views().contains(&slot) { dock::add_view(&mut app.dock, slot as u8, dock::NOMINAL_AREA); }
                let location = RemoteLocation { host: "host".into(), path: format!("/{}.takm", tab * 4 + slot) };
                let key = location.memory_key();
                app.show_remote(&ctx, slot, location, Arc::new(views::sample_data(key.to_str().unwrap(), &["M"])), comments::Comments::default());
            }
        }
        next_tab(&mut app);
        let extra = RemoteLocation { host: "host".into(), path: "/thirteenth.takm".into() };
        app.open_remote_in(&ctx, 0, extra.clone(), false);
        assert!(app.notice.as_ref().unwrap().contains("12"));
        assert!(app.views[0].data.is_none() && app.ssh_reads.is_empty());
        app.open_remote_in(&ctx, 0, first.clone(), false);
        assert_eq!(app.views[0].remote.as_ref(), Some(&first));
        let opened = app.open_views().len();
        app.remote_action(&ctx, SshAction::Open { location: extra, side: true });
        assert_eq!(app.open_views().len(), opened, "a refused beside-open must not allocate a view");
        app.remote_action(&ctx, SshAction::Open { location: first, side: true });
        assert_eq!(app.open_views().len(), opened + 1);
        assert!(app.views[0].shares_document(&app.views[1]));
        assert_eq!(app.comment_writable(1).unwrap_err(), REMOTE_NO_COMMENTS);
    }

    #[test]
    fn remote_emg_completion_in_hidden_requester_updates_every_shared_view() {
        let ctx = egui::Context::default();
        let mut app = remote(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        let location = app.views[0].remote.clone().unwrap();
        let mut take = app.views[0].data.as_ref().unwrap().take.clone();
        take.analog = vec![tak_core::tak::AnalogDevice { channels: vec![tak_core::tak::AnalogChannel {
            name: "EMG".into(), kind: "Analog".into(), dim: 1, frames: vec![], values: vec![],
        }] }];
        let mut data = ViewData::build(take);
        data.emg_pending = true;
        app.views[0].data = Some(Arc::new(data));
        next_tab(&mut app);
        app.open_remote_in(&ctx, 0, location.clone(), false);
        let second = app.layout_tabs.active_id().unwrap();
        app.views[0].emg_requested = true;
        app.track_remote_read(96, 0, location.clone());
        let target = app.views[0].data.as_ref().unwrap().analog_target();
        app.switch_layout_tab_internal(first);
        let analog = vec![tak_core::tak::AnalogDevice { channels: vec![tak_core::tak::AnalogChannel {
            name: "EMG".into(), kind: "Analog".into(), dim: 1, frames: vec![0], values: vec![17.0],
        }] }];
        app.apply_ssh_events(&ctx, vec![SshEvent::Analog { request_id: 96, location,
            part: tak_core::view_data::AnalogPart::prepare_selected(analog, None, &target, vec![0], false) }]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.views[0].data.as_ref().unwrap().emg_pending {
            app.poll_hidden_file_work(&ctx);
            app.views[0].poll_analog();
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        let hidden = app.remote_view(RemoteOwner { tab: Some(second), slot: 0 }).unwrap();
        assert!(Arc::ptr_eq(app.views[0].data.as_ref().unwrap(), hidden.data.as_ref().unwrap()));
        assert_eq!(app.views[0].data.as_ref().unwrap().take.analog[0].channels[0].values, [17.0]);
        assert!(app.ssh_reads.is_empty());
    }

    #[test]
    fn fresh_remote_markers_replace_old_aliases_and_keep_their_selection_by_uid() {
        let ctx = egui::Context::default();
        let mut app = remote(&ctx);
        let first = app.layout_tabs.active_id();
        let location = app.views[0].remote.clone().unwrap();
        app.views[0].pb.set_frame(6, 0.0);
        app.views[0].select(1, false);
        let previous = app.views[0].data.clone().unwrap();
        next_tab(&mut app);
        app.open_remote_in(&ctx, 0, location.clone(), false);
        pending(&mut app, 97, &location);
        let key = location.memory_key();
        app.apply_ssh_events(&ctx, vec![SshEvent::Markers { request_id: 97, location: location.clone(),
            data: Arc::new(views::sample_data(key.to_str().unwrap(), &["Changed A", "Changed B"])), comments: comments::Comments::default() }]);
        let alias = app.remote_view(RemoteOwner { tab: first, slot: 0 }).unwrap();
        assert!(Arc::ptr_eq(app.views[0].data.as_ref().unwrap(), alias.data.as_ref().unwrap()));
        assert!(!Arc::ptr_eq(alias.data.as_ref().unwrap(), &previous));
        assert_eq!(alias.data.as_ref().unwrap().markers[alias.primary.unwrap()].uid, [2; 16]);
        assert_eq!(alias.pb.frame, 6);
        assert_eq!(alias.remote.as_ref(), Some(&location));
        assert!(matches!(&alias.comments.target, Some(Err(reason)) if reason == REMOTE_NO_COMMENTS));
    }

    #[test]
    fn first_remote_open_from_zero_tabs_is_one_undoable_tab_action() {
        let ctx = egui::Context::default();
        let mut app = remote(&ctx);
        let location = app.views[0].remote.clone().unwrap();
        app.cache_remote(0);
        let previous = app.layout_tabs.active_id().unwrap();
        assert!(app.close_layout_tab_internal(previous));
        app.history = History::default();
        app.observe(false);
        assert!(app.layout_tabs.tabs().is_empty());
        app.open_remote_in(&ctx, 0, location.clone(), true);
        assert_eq!(app.layout_tabs.tabs().len(), 1);
        assert_eq!(app.views[0].remote.as_ref(), Some(&location));
        assert_eq!(app.history.undo.len(), 1);
        assert!(app.ssh_reads.is_empty());
        app.undo_redo(&ctx, false);
        assert!(app.layout_tabs.tabs().is_empty(), "first undo removes the implicit tab and its remote file together");
        app.undo_redo(&ctx, true);
        assert_eq!(app.layout_tabs.tabs().len(), 1);
        assert_eq!(app.views[0].remote.as_ref(), Some(&location));
        assert!(app.views[0].data.is_some());
        assert!(app.views[0].loading.is_none() && app.ssh_reads.is_empty());
        assert_eq!(app.comment_writable(0).unwrap_err(), REMOTE_NO_COMMENTS);
    }
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
        app.track_remote_read(99, 0, location);
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
        app.track_remote_read(5, 0, location.clone());
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
        app.track_remote_read(5, 0, location);
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

    #[test]
    fn disconnected_emg_access_reports_once_and_preserves_cached_undo_markers() {
        let ctx = egui::Context::default();
        let mut app = remote(&ctx);
        Arc::get_mut(app.views[0].data.as_mut().unwrap()).unwrap().emg_pending = true;
        let before = tak_core::remote::verification_data(&app.views[0].data.as_ref().unwrap().take);
        app.quiet();
        app.remember(0);
        drop_later(app.views[0].clear());
        app.observe(true);
        app.undo_redo(&ctx, false);
        assert!(app.views[0].data.as_ref().unwrap().emg_pending);
        app.views[0].devices.request_emg = true;
        app.poll_remote_emg(&ctx);
        assert!(app.views[0].emg_requested);
        assert!(app.views[0].emg_failure.as_ref().unwrap().contains("연결"));
        assert!(app.ssh_reads.is_empty());
        assert_eq!(tak_core::remote::verification_data(&app.views[0].data.as_ref().unwrap().take), before);
        app.ssh_note = None;
        app.views[0].devices.request_emg = true;
        app.poll_remote_emg(&ctx);
        assert!(app.ssh_note.is_none(), "repeated access must not retry a failed request");
    }

    #[test]
    fn initial_remote_analog_error_or_cancel_finishes_wait_and_preserves_samples() {
        for cancelled in [false, true] {
            for shared in [false, true] {
                let ctx = egui::Context::default();
                let mut app = remote(&ctx);
                let location = app.views[0].remote.clone().unwrap();
                let mut take = app.views[0].data.as_ref().unwrap().take.clone();
                take.analog = vec![tak_core::tak::AnalogDevice { channels:(0..3).map(|i| tak_core::tak::AnalogChannel {
                    name:format!("AnalogChannel{i}"), kind:"FloatChannel".into(), dim:1,
                    frames:vec![0, 1], values:vec![i as f32 + 1.0, i as f32 + 2.0],
                }).collect() }];
                take.plates = vec![tak_core::tak::ForcePlate { name:"받은 힘판".into(), corners:[[0.0; 3]; 4] }];
                let mut data = ViewData::build(take);
                data.analog_pending = true;
                data.emg_pending = true;
                app.views[0].data = Some(Arc::new(data));
                let before = tak_core::remote::verification_data(&app.views[0].data.as_ref().unwrap().take);
                let plates = format!("{:?}", app.views[0].data.as_ref().unwrap().plates);
                let _held = shared.then(|| app.views[0].data.clone());
                app.track_remote_read(42, 0, location);
                let event = if cancelled { SshEvent::Cancelled { request_id:42 } }
                    else { SshEvent::Error { request_id:Some(42), host:"CHO_workstation".into(), message:"힘판 읽기에 실패했어요.".into(), kind:SshErrorKind::Failed } };
                app.apply_ssh_events(&ctx, vec![event]);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while app.views[0].data.as_ref().unwrap().analog_pending && std::time::Instant::now() < deadline {
                    app.views[0].poll_analog();
                    std::thread::yield_now();
                }
                let data = app.views[0].data.as_ref().unwrap();
                assert!(!data.analog_pending, "cancelled={cancelled}, shared={shared}");
                assert!(data.emg_pending, "an initial failure must not claim EMG was loaded");
                let after = tak_core::remote::verification_data(&data.take);
                assert_eq!(after["marker_samples"], before["marker_samples"]);
                assert_eq!(after["analog_samples"], before["analog_samples"]);
                assert_eq!(format!("{:?}", data.plates), plates);
                assert!(!data.take.warnings.is_empty());
                assert!(app.views[0].remote_warning.is_some());
                assert!(app.ssh_reads.is_empty());
            }
        }
    }

    #[test]
    fn host_loss_finishes_initial_wait_and_keeps_the_received_markers() {
        let ctx = egui::Context::default();
        let mut app = remote(&ctx);
        Arc::get_mut(app.views[0].data.as_mut().unwrap()).unwrap().analog_pending = true;
        let before = tak_core::remote::verification_data(&app.views[0].data.as_ref().unwrap().take);
        app.apply_ssh_events(&ctx, vec![SshEvent::Error {
            request_id:None, host:"CHO_workstation".into(), message:LOST.into(), kind:SshErrorKind::Lost,
        }]);
        let data = app.views[0].data.as_ref().unwrap();
        assert!(!data.analog_pending);
        assert_eq!(tak_core::remote::verification_data(&data.take)["marker_samples"], before["marker_samples"]);
        assert_eq!(app.views[0].remote_warning.as_deref(), Some(LOST));
    }

    #[test]
    fn old_remote_failure_never_finishes_the_current_files_initial_wait() {
        for cancelled in [false, true] {
            let ctx = egui::Context::default();
            let mut app = remote(&ctx);
            Arc::get_mut(app.views[0].data.as_mut().unwrap()).unwrap().analog_pending = true;
            app.track_remote_read(42, 0, RemoteLocation { host:"CHO_workstation".into(), path:"/data/older.takm".into() });
            let event = if cancelled { SshEvent::Cancelled { request_id:42 } }
                else { SshEvent::Error { request_id:Some(42), host:"CHO_workstation".into(), message:"지난 파일 오류".into(), kind:SshErrorKind::Failed } };
            app.apply_ssh_events(&ctx, vec![event]);
            assert!(app.views[0].data.as_ref().unwrap().analog_pending);
            assert!(app.views[0].data.as_ref().unwrap().take.warnings.is_empty());
            assert!(app.views[0].remote_warning.is_none());
        }
    }
}
