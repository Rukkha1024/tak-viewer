
use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use tak_core::edit::DocumentId;
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
    had_source: bool,
    phase: Phase,
    identities: Identities,
    resolving: Option<Receiver<Identities>>,
    waiting: Option<Location>,
}

type Identities = HashMap<PathBuf, PathBuf>;

enum Phase {
    Prepare,
    Convert(Receiver<Outcome>),
    Read { out: PathBuf, line: String, view: Option<Box<View>> },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Location { tab: Option<layout_tabs::TabId>, slot: usize }

struct Occurrence {
    location: Location,
    current: DocumentId,
    // Keep the complete inactive primary, including deferred readers. The real overlay
    // always moves with the live occurrence, rather than being reloaded from a path.
    other: View,
}

pub(super) struct SwitchBatch { occurrences: Vec<Occurrence>, identities: Identities }
pub(super) type SharedBatch = Arc<Mutex<SwitchBatch>>;

fn path_identity(path: &Path, identities: &Identities) -> PathBuf {
    identities.get(path).cloned().unwrap_or_else(|| path.components().collect())
}
fn same_path(a: &Path, b: &Path, identities: &Identities) -> bool {
    path_identity(a, identities) == path_identity(b, identities)
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
        if remote_ui::is_remote_key(tak) {
            let slot=self.views.iter().position(|view|view.path()==Some(tak)).ok_or("이 원격 TAK를 먼저 화면에 열어 주세요.")?;
            if self.no_persist && !self.remote_write_probe {return Err("명령 실행은 서버에 쓰지 않아요.".into());}
            if !tak_core::tak::is_take_path(tak) {return Err("이미 .takm 파일이에요.".into());}
            if self.remote_saving(slot) {return Err("서버에 저장하는 중이에요.".into());}
            return Ok(tak.to_path_buf());
        }
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
        if remote_ui::is_remote_key(&tak) {
            if let Some(slot)=self.views.iter().position(|view|view.path()==Some(tak.as_path())) {self.save_remote(ctx,slot,false);}
            return;
        }
        let identities = self.takm_known_identities();
        if let Err(reason) = self.takm_source_ready(&tak, &identities) {
            self.takm_note = Some((reason, now));
            return;
        }
        let locations = self.takm_locations(&tak, &identities);
        self.takm_note = None;
        self.takm_job = Some(Job { tak, had_source: !locations.is_empty(), phase: Phase::Prepare, identities, resolving: None, waiting: None });
        self.poll_takm(ctx);
    }

    /// Status bar words while a conversion runs.
    pub(super) fn takm_working(&self) -> Option<String> {
        self.takm_job.as_ref().map(|j| format!("{} → .takm 만드는 중…", name_of(&j.tak)))
    }

    /// Complete conversion and the staged marker read before touching any live view.
    pub(super) fn poll_takm(&mut self, ctx: &egui::Context) {
        let Some(mut job) = self.takm_job.take() else { return };
        let now = ctx.input(|input| input.time);
        // A staged read only polls its own receiver. Hidden tabs and the filesystem are
        // untouched on every waiting frame; full preflight runs on arrivals/topology changes.
        if let Phase::Read { out, view: Some(staged), .. } = &mut job.phase {
            if staged.data.is_none() {
                let Some(loaded) = staged.poll_markers() else { self.takm_job = Some(job); return };
                match loaded.result {
                    Ok((data, steps)) => {
                        staged.load_steps = steps;
                        self.next_file_id += 1;
                        staged.show(data, loaded.rx, self.next_file_id, self.settings.speed, now);
                        staged.poll_comments();
                    }
                    Err(reason) => {
                        self.takm_note = Some((format!("{}을(를) 읽지 못해 화면은 그대로예요: {reason}", name_of(out)), now));
                        return;
                    }
                }
            }
        }
        if let Some(waiting) = job.waiting {
            let out = match &job.phase { Phase::Read { out, .. } => out, _ => &job.tak };
            if self.takm_view(waiting).is_some_and(|view| {
                ((view.loading.is_some() || view.waiting_shared_path().is_some()) && view.shared_target().is_some_and(|path| same_path(&path, out, &job.identities)))
                    || view.overlay_load.as_ref().is_some_and(|load| same_path(&load.path, out, &job.identities))
            }) {
                self.takm_job = Some(job);
                return;
            }
            job.waiting = None;
        }
        if let Phase::Convert(rx) = &job.phase {
            let outcome = match rx.try_recv() {
                Ok(outcome) => outcome,
                Err(TryRecvError::Empty) => { self.takm_job = Some(job); return; }
                Err(TryRecvError::Disconnected) => Outcome::Failed("만드는 작업이 중간에 끝났어요.".into()),
            };
            let (out, line, exists) = match outcome {
                Outcome::Made(c) => {
                    let mut line = format!("{} 만들었어요 · {} → {}", name_of(&c.out), mb(c.tak_size), mb(c.takm_size));
                    if c.csv_beside.is_some() { line += &format!(" · 옆 CSV의 Comment 줄 {}개를 넣었어요", c.imported); }
                    if let Some(dir) = c.out.parent() { self.workspace.relist(dir); }
                    (c.out, line, false)
                }
                Outcome::Exists(out) => {
                    let line = format!("{}이(가) 이미 있어서 덮어쓰지 않고 그 파일을 열었어요.", name_of(&out));
                    (out, line, true)
                }
                Outcome::Failed(reason) => {
                    self.takm_note = Some((format!("{}을(를) .takm으로 만들지 못했어요: {reason}", name_of(&job.tak)), now));
                    return;
                }
            };
            if self.takm_locations(&job.tak, &job.identities).is_empty() {
                if exists && !job.had_source { self.open(ctx, out); }
                self.takm_note = Some((line, now));
                return;
            }
            job.phase = Phase::Read { out, line, view: None };
        }
        if !self.resolve_takm_identities(ctx, &mut job) { self.takm_job = Some(job); return; }
        let locations = self.takm_locations(&job.tak, &job.identities);
        if let Err(reason) = self.takm_source_ready(&job.tak, &job.identities) {
            self.takm_note = Some((reason, now));
            return;
        }
        if matches!(job.phase, Phase::Prepare) {
            if !locations.is_empty() && !self.allow_takm_batch(&locations, &takm::path_for(&job.tak, None), &job.identities, false) { return; }
            job.had_source |= !locations.is_empty();
            let (tx, rx) = channel();
            let wake = ctx.clone();
            let source = job.tak.clone();
            #[cfg(test)] let gate = self.takm_gate.take();
            std::thread::spawn(move || {
                #[cfg(test)] if let Some(gate) = gate { let _ = gate.recv(); }
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| convert(&source)))
                    .unwrap_or_else(|_| Outcome::Failed("만드는 중 내부 오류가 났어요.".into()));
                let _ = tx.send(outcome);
                wake.request_repaint();
            });
            job.phase = Phase::Convert(rx);
            self.takm_job = Some(job);
            return;
        }
        let Phase::Read { out, line, view } = &mut job.phase else { unreachable!() };
        if locations.is_empty() { self.takm_note = Some((line.clone(), now)); return; }
        if !self.allow_takm_batch(&locations, out, &job.identities, false) { return; }
        if self.save_destination_reserved(out) {
            self.takm_note = Some(("이 파일을 저장 중이에요. 끝난 뒤 다시 바꿔 주세요.".into(), now));
            return;
        }
        if let Some(shared) = self.takm_shared_file(out, &job.identities) {
            drop_later(view.take());
            self.apply_takm_switch(ctx, locations, shared, None, job.identities);
            self.takm_note = Some((line.clone(), now));
            return;
        }
        if let Some(waiting) = self.takm_loading_location(out, &job.identities) {
            job.waiting = Some(waiting);
            self.takm_job = Some(job);
            return;
        }
        if let Some(staged) = view.as_mut().filter(|staged| staged.data.is_some()) {
            let shared = staged.shared_file().expect("loaded TAKM document");
            self.apply_takm_switch(ctx, locations, shared, view.take().map(|view| *view), job.identities);
            self.takm_note = Some((line.clone(), now));
            return;
        }
        let mut staged = Box::new(View::default());
        staged.start_load(ctx, out.clone(), None);
        *view = Some(staged);
        self.takm_job = Some(job);
    }

    /// Canonicalization is confined to this worker. In-memory stamps resolve loaded views;
    /// newly opened/renamed/pending paths are resolved again before any file or view mutation.
    fn resolve_takm_identities(&self, ctx: &egui::Context, job: &mut Job) -> bool {
        if let Some(rx) = &job.resolving {
            match rx.try_recv() {
                Ok(paths) => { job.identities.extend(paths); job.resolving = None; }
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => { job.resolving = None; return false; }
            }
        }
        job.identities.extend(self.takm_known_identities());
        let mut paths = vec![job.tak.clone(), match &job.phase { Phase::Read { out, .. } => out.clone(), _ => takm::path_for(&job.tak, None) }];
        for location in self.takm_live_locations() {
            let Some(view) = self.takm_view(location) else { continue };
            paths.extend(view.path().map(Path::to_path_buf));
            paths.extend(view.shared_target());
            paths.extend(view.overlay_path().map(Path::to_path_buf));
            paths.extend(view.overlay_shared_target());
            paths.extend(view.overlay_load.as_ref().map(|load| load.path.clone()));
        }
        paths.retain(|path| !job.identities.contains_key(path));
        paths.sort(); paths.dedup();
        if paths.is_empty() { return true; }
        let (tx, rx) = channel();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let identities = paths.into_iter().map(|path| {
                let canonical = takm::canonical_target(&path).unwrap_or_else(|_| path.clone());
                (path, canonical)
            }).collect();
            let _ = tx.send(identities); wake.request_repaint();
        });
        job.resolving = Some(rx);
        false
    }

    fn takm_known_identities(&self) -> Identities {
        let mut identities = Identities::new();
        let mut add = |view: &View| {
            if view.loading.is_some() || view.waiting_shared_path().is_some() { return; }
            if let Some(canonical) = view.source_stamp.as_ref().and_then(|stamp| stamp.lock().ok().and_then(|stamp| stamp.as_ref().map(|stamp| stamp.0.clone()))) {
                if let Some(path) = view.path() { identities.insert(path.to_path_buf(), canonical.clone()); }
                if let Some(path) = view.shared_target() { identities.insert(path, canonical); }
            }
        };
        for location in self.takm_live_locations() {
            if let Some(view) = self.takm_view(location) { add(view); if let Some(owner) = view.overlay_owner() { add(owner); } }
        }
        identities
    }

    fn takm_shared_file(&mut self, out: &Path, identities: &Identities) -> Option<views::SharedFile> {
        for location in self.takm_live_locations() {
            let Some(view) = self.takm_view(location) else { continue };
            let primary = view.shared_target().is_some_and(|path| same_path(&path, out, identities)) && view.loading.is_none() && view.waiting_shared_path().is_none();
            let overlay = view.overlay_shared_target().is_some_and(|path| same_path(&path, out, identities));
            if !primary && !overlay { continue; }
            let view = self.takm_view_mut(location).expect("matched live view");
            if primary { if let Some(shared) = view.shared_file() { return Some(shared); } }
            if overlay { if let Some(shared) = view.overlay_owner_mut().and_then(View::shared_file) { return Some(shared); } }
        }
        None
    }

    fn takm_loading_location(&self, out: &Path, identities: &Identities) -> Option<Location> {
        self.takm_live_locations().into_iter().find(|&location| self.takm_view(location).is_some_and(|view| {
            ((view.loading.is_some() || view.waiting_shared_path().is_some()) && view.shared_target().is_some_and(|path| same_path(&path, out, identities)))
                || view.overlay_load.as_ref().is_some_and(|load| same_path(&load.path, out, identities))
        }))
    }

    /// Same path aliases and the shared edit identity identify a source occurrence.
    fn takm_locations(&self, tak: &Path, identities: &Identities) -> Vec<Location> {
        let source_ids: HashSet<_> = self.takm_live_locations().into_iter().filter_map(|location| {
            let view = self.takm_view(location)?;
            view.shared_target().filter(|path| same_path(path, tak, identities)).and_then(|_| view.document_id())
        }).collect();
        self.takm_live_locations().into_iter().filter(|&location| self.takm_view(location).is_some_and(|view| {
            view.shared_target().is_some_and(|path| same_path(&path, tak, identities))
                || view.document_id().is_some_and(|id| source_ids.contains(&id))
        })).collect()
    }

    fn takm_live_locations(&self) -> Vec<Location> {
        let mut locations: Vec<_> = self.views.iter().enumerate().map(|(slot, _)| Location { tab: self.layout_tabs.active_id(), slot }).collect();
        for tab in self.layout_tabs.tabs() {
            if tab.layout.is_some() {
                locations.extend((0..MAX_VIEWS).map(|slot| Location { tab: Some(tab.id), slot }));
            }
        }
        locations
    }

    fn takm_view(&self, location: Location) -> Option<&View> {
        if location.tab == self.layout_tabs.active_id() { self.views.get(location.slot) }
        else { self.layout_tabs.tab(location.tab?).and_then(|tab| tab.layout.as_ref())?.views.get(location.slot) }
    }

    fn takm_view_mut(&mut self, location: Location) -> Option<&mut View> {
        if location.tab == self.layout_tabs.active_id() { self.views.get_mut(location.slot) }
        else { self.layout_tabs.tab_mut(location.tab?).and_then(|tab| tab.layout.as_mut())?.views.get_mut(location.slot) }
    }

    fn takm_ui_busy(&self, location: Location) -> bool {
        if location.tab == self.layout_tabs.active_id() { self.marker_ui[location.slot].has_pending_work() }
        else { self.layout_tabs.tab(location.tab.unwrap()).and_then(|tab| tab.layout.as_ref()).is_some_and(|runtime| runtime.marker_ui[location.slot].has_pending_work()) }
    }

    /// Refuse before creating a raw sibling: Ctrl+S is the path that saves marker edits.
    fn takm_source_ready(&self, tak: &Path, identities: &Identities) -> Result<(), String> {
        for location in self.takm_locations(tak, identities) {
            let view = self.takm_view(location).expect("live location");
            if view.marker_dirty() || view.comments.pending > 0 || view.comments.failed.is_some()
                || view.edit_refresh.is_some() || self.takm_ui_busy(location)
                || view.document_id().is_some_and(|id| self.overlay_marker_ui.get(&id).is_some_and(marker_edit::MarkerUi::has_pending_work)) {
                return Err("편집이나 저장이 아직 끝나지 않았어요. Ctrl+S로 편집을 저장한 뒤 .takm으로 바꿔 주세요.".into());
            }
            if view.loading.is_some() || view.remote_loading.is_some() || view.waiting_shared_path().is_some() || view.data.is_none() {
                return Err("TAK를 읽는 중이에요. 다 읽은 뒤 .takm으로 바꿔 주세요.".into());
            }
        }
        Ok(())
    }

    /// Staging counts displayed and pending sources together; a history swap can use its
    /// final set because retained Views swap atomically without starting another reader.
    fn allow_takm_batch(&mut self, locations: &[Location], out: &Path, identities: &Identities, replacing: bool) -> bool {
        if locations.iter().any(|&location| self.takm_view(location).is_some_and(|view| {
            view.overlay_shared_target().is_some_and(|path| same_path(&path, out, identities))
                || view.overlay_load.as_ref().is_some_and(|load| same_path(&load.path, out, identities))
        })) {
            self.notice = Some("바꾸려는 .takm을 이미 이 화면에 얹어 보고 있어요. 그 파일을 내린 뒤 다시 바꿔 주세요.".into());
            return false;
        }
        let removed: HashSet<_> = locations.iter().copied().collect();
        let mut files = HashSet::new();
        for location in self.takm_live_locations() {
            let Some(view) = self.takm_view(location) else { continue };
            if !replacing || !removed.contains(&location) {
                if let Some(path) = view.path() { files.insert(path_identity(path, identities)); }
                if let Some(path) = view.shared_target() { files.insert(path_identity(&path, identities)); }
            }
            if let Some(path) = view.overlay_path() { files.insert(path_identity(path, identities)); }
            if let Some(path) = view.overlay_shared_target() { files.insert(path_identity(&path, identities)); }
            if let Some(load) = &view.overlay_load { files.insert(path_identity(&load.path, identities)); }
        }
        files.insert(path_identity(out, identities));
        if files.len() <= 12 { return true; }
        self.notice = Some("서로 다른 파일은 전체에서 12개까지 열 수 있어요. 다른 파일을 닫은 뒤 다시 바꿔 주세요.".into());
        false
    }

    /// Staging also serves the old internal transition entry point. A refused switch is
    /// handled here, so Explorer must not fall through to a destructive single-view open.
    pub(super) fn switch_to_takm(&mut self, ctx: &egui::Context, tak: &Path, out: &Path) -> bool {
        let identities = self.takm_known_identities();
        let locations = self.takm_locations(tak, &identities);
        if locations.is_empty() { return false; }
        if let Err(reason) = self.takm_source_ready(tak, &identities) {
            self.takm_note = Some((reason, ctx.input(|i| i.time)));
            return true;
        }
        if self.takm_job.is_some() || self.save_destination_reserved(out) { return true; }
        self.takm_job = Some(Job { tak: tak.to_path_buf(), had_source: true, phase: Phase::Read { out: out.to_path_buf(), line: format!("{} 열었어요.", name_of(out)), view: None }, identities, resolving: None, waiting: None });
        self.poll_takm(ctx);
        true
    }

    fn apply_takm_switch(&mut self, ctx: &egui::Context, locations: Vec<Location>, shared: views::SharedFile, mut staged: Option<View>, identities: Identities) {
        let now = ctx.input(|i| i.time);
        self.observe(true);
        let mut occurrences = Vec::new();
        for location in locations {
            self.next_file_id += 1;
            let mut incoming = staged.take().unwrap_or_else(|| {
                let mut view = View::default();
                drop_later(view.show_shared_file(shared.clone(), self.next_file_id, self.settings.speed, now));
                view
            });
            let old = self.takm_view_mut(location).expect("preflighted occurrence");
            incoming.cam = old.cam;
            incoming.follow_target = old.follow_target;
            incoming.pb.speed = old.pb.speed;
            incoming.pb.set_frame(old.pb.frame, now);
            incoming.pb.playing = old.pb.playing;
            incoming.range = old.range;
            incoming.apply_uid_selection(&old.edit_selection_state());
            incoming.armed_label = old.armed_label;
            // Forceplate values may still be arriving. Their selected index belongs to
            // the occurrence and must not disappear during the markers-first switch.
            incoming.plate = old.plate.filter(|&plate| incoming.data.as_ref()
                .is_some_and(|data| data.analog_pending || plate < data.plates.len()));
            incoming.fresh = false;
            incoming.fit_pending = false;
            incoming.overlay = old.overlay.take();
            incoming.overlay_load = old.overlay_load.take();
            incoming.shown = old.shown;
            incoming.overlay_ms = old.overlay_ms;
            let current = incoming.document_id().expect("TAKM document");
            let other = std::mem::replace(old, incoming);
            occurrences.push(Occurrence { location, current, other });
        }
        self.handoff_takm_readers(&mut occurrences);
        self.error = None;
        self.history.push(vec![undo::Part::TakmSwitch { batch: Arc::new(Mutex::new(SwitchBatch { occurrences, identities })) }]);
        self.observe(false);
    }

    pub(super) fn takm_batch_ready(&self, batch: &SharedBatch) -> bool {
        batch.lock().ok().is_some_and(|batch| batch.occurrences.iter().all(|entry| {
            self.takm_view(entry.location).is_some_and(|view| view.document_id() == Some(entry.current)
                && view.loading.is_none() && view.waiting_shared_path().is_none())
        }))
    }

    /// Other opens must reserve the pending destination against the same final file set.
    /// A source closed/replaced while the worker ran is never reopened by its completion.
    pub(super) fn takm_reserved_replacement(&self) -> Option<(PathBuf, PathBuf)> {
        let job = self.takm_job.as_ref()?;
        if !job.had_source { return None; }
        let out = match &job.phase {
            Phase::Prepare | Phase::Convert(_) => takm::path_for(&job.tak, None),
            Phase::Read { out, .. } => out.clone(),
        };
        Some((job.tak.clone(), path_identity(&out, &job.identities)))
    }

    /// Undo removes all affected primaries together. A surviving overlay or unaffected
    /// primary keeps the document alive; otherwise ask once for the last dirty document.
    pub(super) fn guard_takm_batch(&mut self, batch: &SharedBatch, redo: bool) -> bool {
        if self.dirty_prompt.is_some() { return true; }
        let Ok(batch) = batch.lock() else { return true };
        let removed: HashSet<_> = batch.occurrences.iter().map(|entry| entry.location).collect();
        let locations = self.takm_live_locations();
        let mut checked = HashSet::new();
        for entry in &batch.occurrences {
            if let Some(view) = self.takm_view_mut(entry.location) { view.sync_shared(); }
            let Some(view) = self.takm_view(entry.location) else { continue };
            if self.takm_ui_busy(entry.location)
                || self.overlay_marker_ui.get(&entry.current).is_some_and(marker_edit::MarkerUi::has_pending_work) {
                self.notice = Some("편집이나 저장이 끝난 뒤 되돌려 주세요.".into());
                return true;
            }
            if !checked.insert(entry.current) { continue; }
            let survives = locations.iter().any(|&location| self.takm_view(location).is_some_and(|view| {
                (!removed.contains(&location) && view.document_id() == Some(entry.current))
                    || view.overlay_document_id() == Some(entry.current)
            }));
            if !survives && (view.marker_dirty() || view.comments.pending > 0 || view.comments.failed.is_some()) {
                if let Some(tab) = entry.location.tab {
                    let original = self.layout_tabs.active_id();
                    if original != Some(tab) && self.tab_close_origin.is_none() { self.tab_close_origin = Some((tab, original)); }
                    self.switch_layout_tab_internal(tab);
                }
                self.observe(false);
                return self.guard_primary_dirty(entry.location.slot, marker_edit::PendingTransition::Undo { redo });
            }
        }
        let mut identities = batch.identities.clone();
        identities.extend(self.takm_known_identities());
        let targets: Vec<_> = batch.occurrences.iter().filter_map(|entry| entry.other.shared_target()).collect();
        // Include each destination, since a save may have renamed one retained side.
        for target in &targets {
            if !self.allow_takm_batch(&removed.iter().copied().collect::<Vec<_>>(), target, &identities, true) { return true; }
        }
        false
    }

    pub(super) fn takm_batch_words(&self, batch: &SharedBatch) -> String {
        let Ok(batch) = batch.lock() else { return "TAKM 파일 전환".into() };
        let Some(first) = batch.occurrences.first() else { return "TAKM 파일 전환".into() };
        let name = self.takm_view(first.location).and_then(View::path).map(name_of).unwrap_or_default();
        if batch.occurrences.len() == 1 { format!("Open {name}") }
        else { format!("Open {name} · 화면 {}개", batch.occurrences.len()) }
    }

    pub(super) fn swap_takm_batch(&mut self, batch: &SharedBatch, now: f64) -> bool {
        let Ok(mut batch) = batch.lock() else { return false };
        let mut identities = batch.identities.clone();
        identities.extend(self.takm_known_identities());
        // Join any copy opened since the switch; never resurrect an independent document.
        for entry in &mut batch.occurrences {
            if let Some(path) = entry.other.shared_target() {
                if let Some(shared) = self.takm_shared_file(&path, &identities) {
                    if !entry.other.adopt_shared_preserving_state(shared, now) { return false; }
                }
            }
        }
        for entry in &mut batch.occurrences {
            let Some(live) = self.takm_view_mut(entry.location) else { return false };
            entry.other.overlay = live.overlay.take();
            entry.other.overlay_load = live.overlay_load.take();
            entry.other.shown = live.shown;
            entry.other.overlay_ms = live.overlay_ms;
            entry.other.sync_shared();
            entry.other.pb.set_frame(entry.other.pb.frame, now);
            std::mem::swap(live, &mut entry.other);
            entry.current = live.document_id().expect("retained document");
        }
        self.handoff_takm_readers(&mut batch.occurrences);
        true
    }

    /// An old primary can still own the sole EMG/Comment/analog reader for an unchanged
    /// live overlay. History is not polled, so move that reader to its live document owner.
    fn handoff_takm_readers(&mut self, occurrences: &mut [Occurrence]) {
        let live = self.takm_live_locations();
        for entry in occurrences {
            if !entry.other.owns_shared_file_work() { continue; }
            let document = entry.other.document_id();
            for &location in &live {
                let Some(target) = self.takm_view_mut(location) else { continue };
                if document.is_some() && target.document_id() == document {
                    if entry.other.transfer_shared_loads(target) { break; }
                } else if document.is_some() && target.overlay_document_id() == document
                    && entry.other.transfer_primary_work_to_overlay(target) { break; }
            }
        }
    }
}

#[cfg(test)]
#[path = "app_takm_tests.rs"]
mod tests;
