
use super::*;

/// Seconds the status bar keeps why a file could not be laid over a view.
pub(super) const NOTE_S: f64 = 8.0;
/// Overlay name tags: height, gap, inner margin, icon place, ✕ place (points).
const TAG_H: f32 = 22.0;
const GAP: f32 = 6.0;
const PAD: f32 = 8.0;
const ICON_W: f32 = 16.0;
const CLOSE_W: f32 = 20.0;
const TAG_FONT: f32 = 12.5;

fn name_of(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod shared_overlay_tests {
    use super::*;
    use std::sync::{Arc, mpsc::channel};

    fn show(view: &mut View, path: &str, id: usize) {
        let (_tx, rx) = channel();
        drop_later(view.show(views::sample_data(path, &["M"]), rx, id, 1.0, 0.0));
    }

    fn app(ctx: &egui::Context) -> TakViewerApp {
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        app.set_command_run();
        app.hold_loads = true;
        show(&mut app.views[0], "overlay_host.tak", 1);
        app
    }

    #[test]
    fn hidden_primary_and_multiple_overlays_share_one_data_document_without_reading() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        show(&mut app.views[1], "overlay_shared.tak", 2);
        let document = app.views[1].document_id();
        let data = app.views[1].data.as_ref().unwrap().clone();
        app.new_layout_tab();
        show(&mut app.views[0], "another_host.tak", 3);
        app.overlay_on(&ctx, "overlay_shared.tak".into());
        assert!(app.views[0].overlay_reading_path().is_none(), "the hidden primary already owns the file");
        app.poll_overlays(&ctx);
        assert_eq!(app.views[0].overlay_document_id(), document);
        assert!(Arc::ptr_eq(&data, &app.views[0].overlay.as_ref().unwrap().data));
        show(&mut app.views[1], "third_host.tak", 4);
        app.active = 1;
        app.overlay_on(&ctx, "overlay_shared.tak".into());
        app.poll_overlays(&ctx);
        assert_eq!(app.views[1].overlay_document_id(), document);
        assert!(Arc::ptr_eq(&app.views[0].overlay.as_ref().unwrap().data, &app.views[1].overlay.as_ref().unwrap().data));
    }

    #[test]
    fn overlay_primary_of_same_view_is_still_refused_and_shared_mismatch_keeps_old_overlay() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        app.overlay_on(&ctx, "overlay_host.tak".into());
        assert!(app.views[0].overlay_load.is_none());
        assert!(app.overlay_note.take().unwrap().0.contains("이 화면에 열린 파일"));
        app.error = None;
        app.views[0].overlay = Some(views::Overlay::from_data("old_overlay.tak".into(), views::sample_data("old_overlay.tak", &["M"])));
        let mut incompatible = views::sample_take("different_rate.tak", &["M"]);
        incompatible.frame_rate = 120.0;
        let (_tx, rx) = channel();
        app.views[1].show(tak_core::view_data::ViewData::build(incompatible), rx, 2, 1.0, 0.0);
        app.overlay_on(&ctx, "different_rate.tak".into());
        app.poll_overlays(&ctx);
        assert_eq!(app.views[0].overlay_path(), Some(Path::new("old_overlay.tak")));
        let why = app.overlay_note.as_ref().unwrap().0.as_str();
        assert!(why.contains("100 Hz") && why.contains("120 Hz"));
    }

    #[test]
    fn in_flight_overlay_is_one_reader_and_follower_waits_for_it() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        app.overlay_on(&ctx, "waiting_overlay.tak".into());
        assert_eq!(app.views[0].overlay_reading_path(), Some(Path::new("waiting_overlay.tak")));
        show(&mut app.views[1], "other_host.tak", 2);
        app.active = 1;
        app.overlay_on(&ctx, "waiting_overlay.tak".into());
        assert!(app.views[1].overlay_reading_path().is_none());
        assert_eq!(app.views[1].overlay_waiting_shared_path(), Some(Path::new("waiting_overlay.tak")));
        let sender = app.views[0].overlay_load.as_mut().unwrap().held.take().unwrap();
        sender.send(views::LoadMsg::Markers(Ok((views::sample_data("waiting_overlay.tak", &["M"]), vec![])))).unwrap();
        app.poll_overlays(&ctx);
        app.poll_overlays(&ctx);
        assert!(Arc::ptr_eq(&app.views[0].overlay.as_ref().unwrap().data, &app.views[1].overlay.as_ref().unwrap().data));
    }

    #[test]
    fn same_view_primary_refusal_uses_live_saved_path_before_the_view_syncs() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let shared = app.views[0].shared_file().unwrap();
        app.views[1].show_shared_file(shared, 2, 1.0, 0.0);
        let mut renamed = app.views[1].data.as_ref().unwrap().as_ref().clone();
        renamed.take.path = "renamed_host.takm".into();
        app.views[1].data = Some(Arc::new(renamed));
        assert!(app.views[1].publish_shared_file());
        assert_eq!(app.views[0].path(), Some(Path::new("overlay_host.tak")));
        app.overlay_on(&ctx, "renamed_host.takm".into());
        assert!(app.views[0].overlay_load.is_none());
        assert!(app.overlay_note.as_ref().unwrap().0.contains("이 화면에 열린 파일"));
    }
}

/// The two name tags of a view with a file laid over it: where they are this frame, their text
/// and whose markers are shown.
pub(super) struct OverlayTags {
    pub(super) tags: [egui::Rect; 2],
    /// The ✕ at the right end of tag 2 (takes the overlay off).
    pub(super) close: egui::Rect,
    pub(super) labels: [String; 2],
    pub(super) shown: [bool; 2],
}

impl OverlayTags {
    /// Both tags together.
    pub(super) fn area(&self) -> egui::Rect {
        self.tags[0].union(self.tags[1])
    }
}

/// Tag texts of two files: their names, with the folder when the names are the same (a raw copy
/// and an agent's result usually are).
fn tag_names(a: &Path, b: &Path) -> [String; 2] {
    let (na, nb) = (name_of(a), name_of(b));
    if na != nb {
        return [na, nb];
    }
    let folder = |p: &Path| p.parent().map(name_of).unwrap_or_default();
    [format!("{na} · {}", folder(a)), format!("{nb} · {}", folder(b))]
}

impl TakViewerApp {
    /// Files on screen or coming: each open view's file and the file laid over it (at most
    /// `MAX_VIEWS` together).
    pub(super) fn file_count(&self) -> usize {
        self.open_views().iter().map(|&k| usize::from(self.views[k].target().is_some()) + usize::from(self.views[k].overlay_target().is_some())).sum()
    }

    /// Why `path` cannot be laid over view `slot` now (plain Korean), else Ok.
    fn overlay_allowed(&self, slot: usize, path: &Path) -> Result<(), String> {
        let v = &self.views[slot];
        if v.remote.is_some() || v.remote_loading.is_some() || remote_ui::is_remote_key(path) {
            return Err("원격 파일의 겹쳐 보기는 아직 지원하지 않아요.".into());
        }
        let name = name_of(path);
        let Some(own) = v.shared_target() else { return Err("얹을 수 없어요: 이 화면에 먼저 파일을 여세요.".into()) };
        let wanted = self.known_identity(path);
        if Self::bound_primary_identity(v, &own) == wanted {
            return Err(format!("얹을 수 없어요: {name}은(는) 이 화면에 열린 파일이에요."));
        }
        if v.overlay_paths().any(|p| Self::bound_overlay_identity(v, p) == wanted)
            || v.overlay_shared_target().is_some_and(|p| Self::bound_overlay_identity(v, &p) == wanted) {
            return Err(format!("{name}은(는) 이미 이 화면에 얹혀 있어요."));
        }
        Ok(())
    }

    /// Say why a file could not be laid over a view: the status bar for a few seconds; a command
    /// run (`--screenshot`, `--bench`) stops with it.
    fn overlay_say(&mut self, ctx: &egui::Context, why: String) {
        if self.no_persist {
            eprintln!("overlay: {why}");
            self.error = Some(why.clone());
        }
        self.overlay_note = Some((why, ctx.input(|i| i.time)));
    }

    /// Explorer "이 화면에 얹기": lay `path` over the active view (replacing the one laid over it).
    /// Read on a background thread; it is checked and shown when its markers are in.
    pub(super) fn overlay_on(&mut self, ctx: &egui::Context, path: PathBuf) {
        let slot = self.active;
        // An explicit open rechecks a symlink; previously admitted aliases stay bound.
        self.admit_identity(&path);
        match self.overlay_allowed(slot, &path) {
            Ok(()) => {
                self.overlay_note = None;
                self.start_overlay(ctx, slot, path, false);
            }
            Err(why) => { self.discard_identity_candidate(); self.overlay_say(ctx, why); }
        }
    }

    pub(super) fn start_overlay(&mut self, ctx: &egui::Context, slot: usize, path: PathBuf, quiet: bool) {
        if !self.allow_replace_overlay(&path, slot) { return; }
        let identity = self.known_identity(&path);
        if self.views[slot].shared_target().is_some_and(|own| Self::bound_primary_identity(&self.views[slot], &own) == identity) {
            self.discard_identity_candidate();
            self.overlay_say(ctx, format!("얹을 수 없어요: {}은(는) 이 화면에 열린 파일이에요.", name_of(&path)));
            return;
        }
        if self.save_destination_reserved(&path) {
            self.discard_identity_candidate();
            self.overlay_say(ctx, "Save in progress for this file.".into());
            return;
        }
        if !self.restoring_layout && self.guard_overlay_removal(slot, marker_edit::PendingTransition::ReplaceOverlay { slot, path: path.clone(), quiet }) {
            self.discard_identity_candidate();
            return;
        }
        if let Some(shared) = self.find_shared_file(&path, usize::MAX).or_else(|| self.find_shared_overlay_file(&path)) {
            self.views[slot].start_shared_overlay(path, shared, quiet);
            self.views[slot].set_overlay_pending_identity(identity);
            ctx.request_repaint();
            return;
        }
        if self.file_is_loading_elsewhere(&path, usize::MAX) || self.overlay_file_is_loading(&path) {
            self.views[slot].wait_overlay_shared(path, quiet);
            self.views[slot].set_overlay_pending_identity(identity);
            return;
        }
        let retained = self.find_retained_document(&path);
        #[cfg(test)]
        if self.hold_loads {
            self.views[slot].hold_overlay(path, quiet);
            self.views[slot].set_overlay_pending_identity(identity);
            self.views[slot].set_overlay_fresh_open_candidate(retained);
            return;
        }
        self.views[slot].start_overlay(ctx, path, quiet);
        self.views[slot].set_overlay_pending_identity(identity);
        self.views[slot].set_overlay_fresh_open_candidate(retained);
    }

    /// Take the file laid over view `slot` off (Explorer "얹은 파일 내리기", ✕ on name tag 2).
    pub(super) fn overlay_off(&mut self, slot: usize) {
        if !self.restoring_layout && self.guard_overlay_removal(slot, marker_edit::PendingTransition::OverlayOff { slot }) { return; }
        self.handoff_overlay_file_work(slot);
        drop_later(self.views[slot].take_overlay());
    }

    /// Canonical overlay-only files are also available to ordinary primary-view opens.
    pub(super) fn find_shared_overlay_file(&mut self, path: &Path) -> Option<views::SharedFile> {
        let wanted = self.known_identity(path);
        for view in &mut self.views {
            if view.overlay_shared_target().is_some_and(|p| Self::bound_overlay_identity(view, &p) == wanted) {
                if let Some(existing) = view.overlay_path().map(Path::to_path_buf) {
                    if let Some(shared) = view.shared_overlay_file(&existing) { return Some(shared); }
                }
            }
        }
        for tab in self.layout_tabs.tabs_mut() {
            if let Some(runtime) = &mut tab.layout {
                for view in &mut runtime.views {
                    if view.overlay_shared_target().is_some_and(|p| Self::bound_overlay_identity(view, &p) == wanted) {
                        if let Some(existing) = view.overlay_path().map(Path::to_path_buf) {
                            if let Some(shared) = view.shared_overlay_file(&existing) { return Some(shared); }
                        }
                    }
                }
            }
        }
        None
    }

    pub(super) fn overlay_file_is_loading(&self, path: &Path) -> bool {
        let wanted = self.known_identity(path);
        self.views.iter().any(|view| view.overlay_reading_path().is_some_and(|p| Self::bound_overlay_identity(view, p) == wanted))
            || self.layout_tabs.tabs().iter().filter_map(|tab| tab.layout.as_ref())
                .flat_map(|runtime| runtime.views.iter()).any(|view| view.overlay_reading_path().is_some_and(|p| Self::bound_overlay_identity(view, p) == wanted))
    }

    /// Preserve the sole deferred-reader owner when an overlay is explicitly removed. Close
    /// whole-view/tab paths call this same hook before removing those overlay references.
    pub(super) fn handoff_overlay_file_work(&mut self, slot: usize) -> bool {
        self.handoff_overlay_file_work_impl(slot, false)
    }

    pub(super) fn handoff_overlay_file_work_outside_tab(&mut self, slot: usize) -> bool {
        self.handoff_overlay_file_work_impl(slot, true)
    }

    fn handoff_overlay_file_work_impl(&mut self, slot: usize, outside_tab: bool) -> bool {
        let Some(document) = self.views[slot].overlay_document_id() else { return false };
        for other in 0..MAX_VIEWS {
            if outside_tab || slot == other { continue; }
            if self.views[other].document_id() != Some(document) && self.views[other].overlay_document_id() != Some(document) { continue; }
            let (source, target) = if slot < other {
                let (left, right) = self.views.split_at_mut(other); (&mut left[slot], &mut right[0])
            } else {
                let (left, right) = self.views.split_at_mut(slot); (&mut right[0], &mut left[other])
            };
            if source.transfer_overlay_work_to(target) { return true; }
        }
        for tab in self.layout_tabs.tabs_mut() {
            let Some(runtime) = &mut tab.layout else { continue };
            for target in &mut runtime.views {
                if target.document_id() == Some(document) || target.overlay_document_id() == Some(document) {
                    if self.views[slot].transfer_overlay_work_to(target) { return true; }
                }
            }
        }
        false
    }

    /// Name tag 1 / 2, key 1 / 2: hide or show the markers of view `slot`'s own file (0) or of
    /// the file laid over it (1). Nothing without an overlay.
    pub(super) fn toggle_shown(&mut self, slot: usize, which: usize) {
        let v = &mut self.views[slot];
        if v.overlay.is_some() {
            v.shown[which] = !v.shown[which];
        }
    }

    /// A click on view `slot`'s name tag `which + 1` (`close` = the ✕ of tag 2), applied after
    /// drawing. An OPEN EDITORS row of a view with a file laid over it comes here too
    /// (`app_editors.rs`), so both are the same undo action with the same notes.
    pub(super) fn name_tag_click(&mut self, slot: usize, which: usize, close: bool) {
        if close {
            self.overlay_off(slot);
        } else {
            self.toggle_shown(slot, which);
        }
    }

    /// Overlays whose markers arrived: shown when they fit the view's file (same frame count and
    /// frame rate), else refused with the reason. One put back by undo / redo is no action.
    pub(super) fn poll_overlays(&mut self, ctx: &egui::Context) {
        let saving_documents = self.saving_documents();
        let mut notes = Vec::new();
        let mut arrived = Vec::new();
        let mut retired = Vec::new();
        let mut quiet_layout = false;
        let poll = |v: &mut View, notes: &mut Vec<String>, arrived: &mut Vec<PathBuf>, retired: &mut Vec<views::Overlay>, quiet_layout: &mut bool| {
            let allow_emg = !v.overlay_document_id().is_some_and(|document| saving_documents.contains(&document));
            v.poll_overlay_file_work_gated(ctx, allow_emg);
            let Some((path, result, quiet)) = v.poll_overlay() else { return };
            let name = name_of(&path);
            let overlay = match result {
                Ok(d) => d,
                Err(e) => {
                    notes.push(format!("얹을 수 없어요: {name}을(를) 읽지 못했어요 ({e})"));
                    arrived.push(path);
                    return;
                }
            };
            let Some(own) = v.data.as_ref() else { drop_later(Some(overlay)); return };
            if let Some(why) = views::overlay_mismatch(own, &overlay.data) {
                drop_later(Some(overlay));
                notes.push(why);
                arrived.push(path);
                return;
            }
            if let Some(old) = v.overlay.replace(overlay) { retired.push(old); }
            drop_later(Some(std::mem::take(&mut v.lanes)));
            arrived.push(path);
            if quiet {
                *quiet_layout = true;
            } else {
                // A new file laid over: its markers shown.
                v.shown[1] = true;
            }
        };
        for v in &mut self.views { poll(v, &mut notes, &mut arrived, &mut retired, &mut quiet_layout); }
        for (tab, slot) in self.hidden_overlay_locations() {
            if let Some(runtime) = self.layout_tabs.work_payload_mut(tab).and_then(|tab| tab.layout.as_mut()) {
                poll(&mut runtime.views[slot], &mut notes, &mut arrived, &mut retired, &mut quiet_layout);
            }
            self.reindex_hidden_slot(tab, slot);
        }
        self.history.quiet_layout |= quiet_layout;
        for mut old in retired {
            for view in self.views.iter_mut().chain(self.layout_tabs.tabs_mut().iter_mut().filter_map(|tab| tab.layout.as_mut()).flat_map(|runtime| runtime.views.iter_mut())) {
                if old.transfer_work_to(view) { break; }
            }
            drop_later(Some(old));
        }
        for why in notes { self.overlay_say(ctx, why); }
        for path in arrived {
            if self.find_shared_file(&path, usize::MAX).or_else(|| self.find_shared_overlay_file(&path)).is_some() {
                self.attach_shared_waiters(ctx, &path);
            } else {
                self.promote_shared_waiter(ctx, &path);
            }
            self.discard_identity_candidate_for(&path);
        }
        let waiting = self.changed_overlay_wait_paths();
        for path in waiting {
            let wanted = self.known_identity(&path);
            if let Some(shared) = self.find_shared_file(&path, usize::MAX).or_else(|| self.find_shared_overlay_file(&path)) {
                for view in &mut self.views {
                    if view.overlay_waiting_shared_path().is_some_and(|p| Self::bound_overlay_identity(view, p) == wanted) {
                        if let Some(existing) = view.overlay_waiting_shared_path().map(Path::to_path_buf) {
                            view.attach_overlay_shared(&existing, shared.clone());
                        }
                    }
                }
                for tab in self.layout_tabs.tabs_mut() {
                    if let Some(runtime) = &mut tab.layout {
                        for view in &mut runtime.views {
                            if view.overlay_waiting_shared_path().is_some_and(|p| Self::bound_overlay_identity(view, p) == wanted) {
                                if let Some(existing) = view.overlay_waiting_shared_path().map(Path::to_path_buf) {
                                    view.attach_overlay_shared(&existing, shared.clone());
                                }
                            }
                        }
                    }
                }
                ctx.request_repaint();
            } else if !self.file_is_loading_elsewhere(&path, usize::MAX) && !self.overlay_file_is_loading(&path) {
                // The earlier owner failed or closed. Promote just one overlay waiter; all
                // other waiters keep waiting for that single reader.
                let candidate = self.views.iter_mut().chain(self.layout_tabs.tabs_mut().iter_mut().filter_map(|tab| tab.layout.as_mut()).flat_map(|runtime| runtime.views.iter_mut()))
                    .find(|v| v.overlay_waiting_shared_path().is_some_and(|p| Self::bound_overlay_identity(v, p) == wanted));
                if let Some(view) = candidate {
                    let quiet = view.overlay_load.as_ref().is_some_and(|load| load.quiet);
                    let identity = view.overlay_bound_identity(&path).unwrap_or_else(|| wanted.clone());
                    #[cfg(test)]
                    if self.hold_loads { view.hold_overlay(path, quiet); view.set_overlay_pending_identity(identity); continue; }
                    view.start_overlay(ctx, path, quiet);
                    view.set_overlay_pending_identity(identity);
                }
            }
        }
        if self.views.iter().any(|view| view.overlay_load.is_some()) || !self.hidden_overlay_locations().is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    /// `--screenshot` / `--bench … --overlay <b.tak>`: the n-th file laid over the n-th view.
    pub fn overlay_args(&mut self, ctx: &egui::Context, files: &[PathBuf]) {
        let active = self.active;
        for (slot, f) in self.open_views().into_iter().zip(files) {
            self.activate(slot);
            self.overlay_on(ctx, workspace::resolve_path(&f.to_string_lossy()));
        }
        self.activate(active);
    }

    /// Where view `slot`'s overlay name tags go this frame: right of its file name tag (`name_tag`,
    /// a view alone in its place) or at the top left of its 3D. None without an overlay.
    pub(super) fn overlay_tags(&self, ctx: &egui::Context, slot: usize, rect: egui::Rect, name_tag: Option<egui::Rect>) -> Option<OverlayTags> {
        let v = &self.views[slot];
        let (o, own) = (v.overlay.as_ref()?, v.path()?);
        let names = tag_names(own, &o.path);
        let width = |s: &str| ctx.fonts(|f| f.layout_no_wrap(s.to_owned(), egui::FontId::proportional(TAG_FONT), egui::Color32::WHITE).size().x);
        let (x0, top) = match name_tag {
            Some(t) => (t.right() + GAP, t.center().y - TAG_H / 2.0),
            None => (rect.left() + GAP, rect.top() + 4.0),
        };
        // A narrow view: each tag gets half of the room left, long file names lose their middle
        // (the folder that tells two same names apart stays: "2512…001.tak · copy"), so both
        // tags and the ✕ stay on screen.
        let room = ((rect.right() - GAP - x0) - GAP - 2.0 * (PAD + ICON_W + PAD) - 4.0 - CLOSE_W + PAD) / 2.0;
        let labels: [String; 2] = std::array::from_fn(|i| {
            let hidden = if v.shown[i] { "" } else { " · 숨김" };
            let dirty = if if i == 0 { v.marker_dirty() } else { o.marker_dirty() } { " *" } else { "" };
            let (base, folder) = match names[i].rsplit_once(" · ") {
                Some((b, f)) => (b.to_owned(), format!(" · {f}")),
                None => (names[i].clone(), String::new()),
            };
            let label = |name: &str| format!("{} {name}{folder}{dirty}{hidden}", i + 1);
            let chars: Vec<char> = base.chars().collect();
            let mut keep = chars.len();
            let mut text = label(&base);
            while keep > 2 && width(&text) > room.max(40.0) {
                keep -= 1;
                let (head, tail) = (keep / 2, keep - keep / 2);
                let short: String = chars[..head].iter().chain(std::iter::once(&'…')).chain(&chars[chars.len() - tail..]).collect();
                text = label(&short);
            }
            text
        });
        let t0 = egui::Rect::from_min_size(egui::pos2(x0, top), egui::vec2(PAD + ICON_W + width(&labels[0]) + PAD, TAG_H));
        let t1 = egui::Rect::from_min_size(egui::pos2(t0.right() + GAP, top), egui::vec2(PAD + ICON_W + width(&labels[1]) + 4.0 + CLOSE_W, TAG_H));
        let close = egui::Rect::from_min_max(egui::pos2(t1.right() - CLOSE_W, t1.top()), t1.max);
        Some(OverlayTags { tags: [t0, t1], close, labels, shown: v.shown })
    }

    /// Take the mouse on view `slot`'s overlay name tags (after the 3D and the floating graphs,
    /// so they are on top) and paint them. A click is applied after drawing (`overlay_click`).
    pub(super) fn overlay_tags_ui(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, slot: usize, rect: egui::Rect, t: &OverlayTags) {
        let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("overlay_tags", slot)).max_rect(rect));
        child.set_clip_rect(rect);
        let tag = |i: usize| child.interact(t.tags[i], child.id().with(("tag", i)), egui::Sense::click());
        let (r0, r1) = (tag(0), tag(1));
        let close = child.interact(t.close, child.id().with("close"), egui::Sense::click());
        if close.clicked() {
            self.overlay_click = Some((slot, 1, true));
        } else if r1.clicked() {
            self.overlay_click = Some((slot, 1, false));
        } else if r0.clicked() {
            self.overlay_click = Some((slot, 0, false));
        }
        let font = egui::FontId::proportional(TAG_FONT);
        for (i, resp) in [r0, r1].iter().enumerate() {
            let r = t.tags[i];
            let fill = if resp.hovered() && !close.hovered() { egui::Color32::from_black_alpha(210) } else { egui::Color32::from_black_alpha(160) };
            painter.rect_filled(r, 4.0, fill);
            let on = t.shown[i];
            let dim = |c: egui::Color32| if on { c } else { c.gamma_multiply(0.35) };
            let icon = egui::pos2(r.left() + PAD + 5.0, r.center().y);
            if i == 0 {
                // The first file: a colored flat dot, like its markers.
                painter.circle_filled(icon, 4.5, dim(theme::ASSET_COLORS[0]));
            } else {
                painter.circle_stroke(icon, 4.5, egui::Stroke::new(scene::OVERLAY_RING_W, dim(theme::OVERLAY_RING)));
            }
            let color = if on { theme::TEXT } else { theme::TEXT_DIM };
            painter.text(egui::pos2(r.left() + PAD + ICON_W, r.center().y), egui::Align2::LEFT_CENTER, &t.labels[i], font.clone(), color);
        }
        if close.hovered() {
            painter.rect_filled(t.close.shrink(3.0), 3.0, egui::Color32::from_white_alpha(28));
        }
        let x = egui::Rect::from_center_size(t.close.center(), egui::Vec2::splat(8.0));
        let stroke = egui::Stroke::new(1.0_f32, if close.hovered() { theme::TAB_TEXT_ACTIVE } else { theme::TEXT });
        painter.line_segment([x.left_top(), x.right_bottom()], stroke);
        painter.line_segment([x.right_top(), x.left_bottom()], stroke);
    }
}
