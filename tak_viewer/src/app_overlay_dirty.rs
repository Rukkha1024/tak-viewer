//! Last-reference protection for primary and laid-over shared documents.
//!
//! Overlay owners temporarily occupy their host slot only while existing marker/save
//! code runs. The real host is restored before drawing or completing a layout action.
use super::*;
use marker_edit::PendingTransition;
use std::collections::HashSet;
use tak_core::edit::DocumentId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Reference { Primary(usize), Overlay(usize) }

pub(super) struct LayoutRestoreFile {
    view: View,
    marker_ui: marker_edit::MarkerUi,
    closed: Option<(u64, usize, dock::EditorLayout)>,
}

pub(super) struct OverlayDirty {
    slot: usize,
    transition: PendingTransition,
    complete: bool,
}

impl TakViewerApp {
    pub(super) fn with_overlay_owner<R>(&mut self, ctx: &egui::Context, slot: usize, run: impl FnOnce(&mut Self) -> R) -> Option<R> {
        let mut overlay = self.views.get_mut(slot)?.overlay.take()?;
        let Some(document) = overlay.document_id() else { self.views[slot].overlay = Some(overlay); return None };
        let had_overlay_prompt = self.overlay_dirty.is_some();
        if overlay.owner().file_id == 0 {
            self.next_file_id += 1;
            let owner = overlay.owner_mut();
            owner.file_id = self.next_file_id;
            if let Some((tag, _)) = &mut owner.analog_rx { *tag = owner.file_id; }
            if let Some((tag, _)) = &mut owner.emg_rx { *tag = owner.file_id; }
        }
        std::mem::swap(&mut self.views[slot], overlay.owner_mut());
        let mut ui = self.overlay_marker_ui.remove(&document).unwrap_or_default();
        ui.bind_file(self.views[slot].file_id);
        std::mem::swap(&mut self.marker_ui[slot], &mut ui);
        self.overlay_owner_scope = true;
        let result = run(self);
        self.overlay_owner_scope = false;
        std::mem::swap(&mut self.marker_ui[slot], &mut ui);
        if ui.has_pending_work() { self.overlay_marker_ui.insert(document, ui); } else { drop_later(Some(ui)); }
        std::mem::swap(&mut self.views[slot], overlay.owner_mut());
        self.views[slot].overlay = Some(overlay);
        self.views[slot].sync_shared();
        if self.overlay_dirty.as_ref().is_some_and(|state| state.complete) {
            let state = self.overlay_dirty.take().expect("completed overlay prompt");
            self.finish_transition(ctx, state.transition);
        } else if had_overlay_prompt && self.overlay_dirty.is_none() && self.dirty_prompt.is_none() { self.restore_tab_close_origin(); }
        Some(result)
    }

    pub(super) fn overlay_prompt_slot(&self) -> Option<usize> { self.overlay_dirty.as_ref().map(|state| state.slot) }

    pub(super) fn finish_overlay_dirty_bridge(&mut self, _ctx: &egui::Context) {
        if let Some(state) = &mut self.overlay_dirty { state.complete = true; }
    }

    pub(super) fn cancel_overlay_dirty_bridge(&mut self, _ctx: &egui::Context) { self.overlay_dirty = None; }

    pub(super) fn cancel_dirty_transition(&mut self, ctx: &egui::Context) {
        self.dirty_prompt = None;
        self.cancel_overlay_dirty_bridge(ctx);
        if !self.overlay_owner_scope { self.restore_tab_close_origin(); }
    }

    pub(super) fn guard_overlay_exit(&mut self, slot: usize) -> bool { self.guard_overlay_dirty(slot, PendingTransition::Exit) }

    fn guard_overlay_dirty(&mut self, slot: usize, transition: PendingTransition) -> bool {
        let ctx = egui::Context::default();
        self.overlay_dirty = Some(OverlayDirty { slot, transition, complete: false });
        let dirty = self.with_overlay_owner(&ctx, slot, |app| app.guard_primary_dirty(slot, PendingTransition::OverlayBridgeComplete)).unwrap_or(false);
        if !dirty { self.overlay_dirty = None; }
        dirty
    }

    pub(super) fn poll_overlay_marker_work(&mut self, ctx: &egui::Context) {
        if self.hold.is_some() || self.comment_edit.is_some()
            || (self.typing(ctx) && self.dirty_prompt.is_none()) || ctx.input(|input| input.pointer.any_down()) { return; }
        if self.overlay_marker_ui.is_empty() { return; }
        let original = self.layout_tabs.active_id();
        let pending = self.overlay_work_locations();
        let reveal = self.explorer.reveal.clone();
        let focused = self.focused_tab;
        let strip = self.type_strip.take();
        let prompt = self.dirty_prompt.take();
        for (tab, slot) in pending {
            if self.layout_tabs.active_id() != Some(tab) { self.switch_layout_tab_internal(tab); }
            self.with_overlay_owner(ctx, slot, |app| {
                app.views[slot].poll_comments();
                app.views[slot].poll_analog();
                app.poll_marker_jobs(ctx);
                app.poll_marker_saves(ctx);
            });
        }
        if let Some(tab) = original { if self.layout_tabs.active_id() != Some(tab) { self.switch_layout_tab_internal(tab); } }
        self.explorer.reveal = reveal;
        self.focused_tab = focused;
        self.type_strip = strip;
        self.dirty_prompt = prompt;
        // Save completion above ran without the dialog so it could not resume against a
        // temporary tab/owner. Recheck only its document after restoring the real runtime.
        if let Some(slot) = self.overlay_prompt_slot() {
            self.with_overlay_owner(ctx, slot, |app| {
                if app.dirty_prompt.as_ref().is_some_and(|prompt| prompt.saving)
                    && !app.marker_saving(slot) && !app.marker_job_running(slot) {
                    if let Some(prompt) = &mut app.dirty_prompt { prompt.saving = false; }
                    if !app.views[slot].marker_dirty() && !app.comments_unsaved(slot) {
                        if let Some(prompt) = app.dirty_prompt.take() { app.finish_transition(ctx, prompt.transition); }
                    }
                }
            });
        } else if let Some(slot) = self.dirty_prompt.as_ref().filter(|prompt| prompt.saving).map(|prompt| prompt.slot) {
            self.views[slot].sync_shared();
            if !self.marker_saving(slot) && !self.marker_job_running(slot) {
                if let Some(prompt) = &mut self.dirty_prompt { prompt.saving = false; }
                if !self.views[slot].marker_dirty() && !self.comments_unsaved(slot) {
                    if let Some(prompt) = self.dirty_prompt.take() { self.finish_transition(ctx, prompt.transition); }
                }
            }
        }
    }

    pub(super) fn guard_overlay_removal(&mut self, slot: usize, transition: PendingTransition) -> bool {
        self.guard_reference_removal(&[Reference::Overlay(slot)], &[], transition)
    }

    pub(super) fn guard_view_removal(&mut self, slot: usize, transition: PendingTransition) -> bool {
        self.guard_reference_removal(&[Reference::Primary(slot), Reference::Overlay(slot)], &[], transition)
    }

    pub(super) fn guard_tab_removal(&mut self, transition: PendingTransition) -> bool {
        let removed: Vec<_> = self.open_views().into_iter().flat_map(|slot| [Reference::Primary(slot), Reference::Overlay(slot)]).collect();
        self.guard_reference_removal(&removed, &[], transition)
    }

    /// `kept_paths` are references in the destination of an undo layout operation.
    pub(super) fn guard_reference_removal(&mut self, removed: &[Reference], kept_paths: &[PathBuf], transition: PendingTransition) -> bool {
        if self.dirty_prompt.is_some() { return true; }
        let ordered = removed.to_vec();
        let removed: HashSet<_> = removed.iter().copied().collect();
        let mut checked = HashSet::new();
        for reference in ordered {
            let (slot, owner) = match reference {
                Reference::Primary(slot) => (slot, &self.views[slot]),
                Reference::Overlay(slot) => { let Some(owner) = self.views[slot].overlay_owner() else { continue }; (slot, owner) }
            };
            let Some(document) = owner.document_id() else { continue };
            let path = owner.shared_target();
            let survives_target = path.as_ref().is_some_and(|path| kept_paths.iter().any(|kept| Self::same_local_path(path, kept)));
            let survivor = self.reference_survivor(document, &removed);
            if let Some(survivor) = survivor {
                if !self.handoff_removed_reference(reference, survivor) {
                    self.notice = Some("읽기나 저장을 마친 뒤 다시 닫아 주세요.".into());
                    return true;
                }
            } else if survives_target {
                let busy = match reference {
                    Reference::Primary(_) => self.marker_ui[slot].has_pending_work(),
                    Reference::Overlay(_) => self.overlay_marker_ui.get(&document).is_some_and(marker_edit::MarkerUi::has_pending_work),
                };
                let owner = match reference { Reference::Primary(_) => &self.views[slot], Reference::Overlay(_) => self.views[slot].overlay_owner().expect("overlay source") };
                if busy || owner.comments.pending > 0 {
                    self.notice = Some("읽기나 저장을 마친 뒤 다시 시도해 주세요.".into());
                    return true;
                }
            } else if checked.insert(document) {
                self.collect_removed_marker_work(document, reference, &removed);
                if match reference {
                    Reference::Primary(_) => self.guard_primary_dirty(slot, transition.clone()),
                    Reference::Overlay(_) => self.guard_overlay_dirty(slot, transition.clone()),
                } { return true; }
            }
        }
        false
    }

    fn reference_survivor(&self, document: DocumentId, removed: &HashSet<Reference>) -> Option<(Option<u64>, Reference)> {
        for (slot, view) in self.views.iter().enumerate() {
            if !removed.contains(&Reference::Primary(slot)) && view.document_id() == Some(document) { return Some((None, Reference::Primary(slot))); }
            if !removed.contains(&Reference::Overlay(slot)) && view.overlay_document_id() == Some(document) { return Some((None, Reference::Overlay(slot))); }
        }
        for tab in self.layout_tabs.tabs() {
            let Some(runtime) = &tab.layout else { continue };
            for (slot, view) in runtime.views.iter().enumerate() {
                if view.document_id() == Some(document) { return Some((Some(tab.id), Reference::Primary(slot))); }
                if view.overlay_document_id() == Some(document) { return Some((Some(tab.id), Reference::Overlay(slot))); }
            }
        }
        None
    }

    fn handoff_removed_reference(&mut self, source: Reference, target: (Option<u64>, Reference)) -> bool {
        let source_slot = match source { Reference::Primary(slot) | Reference::Overlay(slot) => slot };
        let mut source_view = match source {
            Reference::Primary(_) => std::mem::take(&mut self.views[source_slot]),
            Reference::Overlay(_) => { let Some(owner) = self.views[source_slot].overlay_owner_mut() else { return true }; std::mem::take(owner) }
        };
        let Some(document) = source_view.document_id() else { return true };
        let mut source_ui = match source {
            Reference::Primary(_) => std::mem::take(&mut self.marker_ui[source_slot]),
            Reference::Overlay(_) => self.overlay_marker_ui.remove(&document).unwrap_or_default(),
        };
        let (tab, reference) = target;
        let slot = match reference { Reference::Primary(slot) | Reference::Overlay(slot) => slot };
        let (views, uis) = if let Some(tab) = tab {
            let runtime = self.layout_tabs.tab_mut(tab).and_then(|tab| tab.layout.as_mut()).expect("live survivor");
            (&mut runtime.views, &mut runtime.marker_ui)
        } else { (&mut self.views, &mut self.marker_ui) };
        let target_view = match reference { Reference::Primary(_) => &mut views[slot], Reference::Overlay(_) => views[slot].overlay_owner_mut().expect("overlay survivor") };
        let mut target_ui = match reference {
            Reference::Primary(_) => std::mem::take(&mut uis[slot]),
            Reference::Overlay(_) => self.overlay_marker_ui.remove(&document).unwrap_or_default(),
        };
        let safe = source_view.comments.pending == 0
            && !(source_ui.has_pending_work() && target_ui.has_pending_work())
            && (!source_view.owns_shared_file_work() || source_view.transfer_shared_loads(target_view));
        if safe { source_ui.transfer_work_to(&mut target_ui, target_view.file_id); }
        match reference {
            Reference::Primary(_) => uis[slot] = target_ui,
            Reference::Overlay(_) => if target_ui.has_pending_work() { self.overlay_marker_ui.insert(document, target_ui); },
        }
        match source {
            Reference::Primary(_) => { self.views[source_slot] = source_view; self.marker_ui[source_slot] = source_ui; },
            Reference::Overlay(_) => {
                *self.views[source_slot].overlay_owner_mut().expect("source overlay") = source_view;
                if source_ui.has_pending_work() { self.overlay_marker_ui.insert(document, source_ui); }
            }
        }
        safe
    }

    /// A final-reference prompt owns the document's worker, so Discard can cancel it and
    /// Save waits for it even when its previous owner is another closing view.
    fn collect_removed_marker_work(&mut self, document: DocumentId, target: Reference, removed: &HashSet<Reference>) {
        let target_slot = match target { Reference::Primary(slot) | Reference::Overlay(slot) => slot };
        let mut work = self.overlay_marker_ui.remove(&document).unwrap_or_default();
        for reference in removed {
            let Reference::Primary(slot) = *reference else { continue };
            if self.views[slot].document_id() == Some(document) {
                let mut ui = std::mem::take(&mut self.marker_ui[slot]);
                ui.transfer_work_to(&mut work, self.views[slot].file_id);
                self.marker_ui[slot] = ui;
            }
        }
        match target {
            Reference::Primary(_) => { work.transfer_work_to(&mut self.marker_ui[target_slot], self.views[target_slot].file_id); },
            Reference::Overlay(_) => if work.has_pending_work() { self.overlay_marker_ui.insert(document, work); },
        }
    }

    /// Keep sources alive only while undo installs the destination references.
    pub(super) fn hold_layout_reference(&mut self, slot: usize, primary: bool, remember_close: bool) {
        let (view, ui) = if primary {
            (std::mem::take(&mut self.views[slot]), std::mem::take(&mut self.marker_ui[slot]))
        } else {
            let mut view = View::default();
            view.overlay = self.views[slot].overlay.take();
            (view, marker_edit::MarkerUi::default())
        };
        let closed = remember_close.then(|| self.layout_tabs.active_id().map(|tab| (tab, slot, dock::editor_layout(&self.dock)))).flatten();
        self.layout_restore_files.push(LayoutRestoreFile { view, marker_ui: ui, closed });
    }

    pub(super) fn find_layout_restore_file(&mut self, path: &Path) -> Option<views::SharedFile> {
        if !self.restoring_layout { return None; }
        for held in &mut self.layout_restore_files {
            let view = &mut held.view;
            if view.shared_target().is_some_and(|target| Self::same_local_path(&target, path)) {
                if let Some(shared) = view.shared_file() { return Some(shared); }
            }
            if let Some(shared) = view.shared_overlay_file(path) { return Some(shared); }
        }
        None
    }

    /// A restored overlay already has loaded shared markers. Install its owner now,
    /// even while its primary host is loading, so cancellation still sees the live dirty
    /// reference and the sole device readers can transfer before the source retires.
    pub(super) fn install_layout_restore_overlay(&mut self, slot: usize, path: &Path) -> bool {
        let Some(shared) = self.find_layout_restore_file(path) else { return false };
        let mut temporary = View::default();
        drop_later(temporary.show_shared_file(shared.clone(), 0, 1.0, self.layout_time));
        temporary.start_shared_overlay(path.to_path_buf(), shared, true);
        let Some((_, Ok(overlay), _)) = temporary.poll_overlay() else { return false };
        drop_later(self.views[slot].overlay.replace(overlay));
        self.views[slot].cancel_overlay_load();
        true
    }

    pub(super) fn finish_layout_reference_handoffs(&mut self) {
        let retained = std::mem::take(&mut self.layout_restore_files);
        for LayoutRestoreFile { view: mut source, marker_ui: mut ui, closed } in retained {
            for (slot, target) in self.views.iter_mut().enumerate() {
                if source.shares_document(target) {
                    source.transfer_shared_loads(target);
                    ui.transfer_work_to(&mut self.marker_ui[slot], target.file_id);
                } else if source.document_id().is_some() && source.document_id() == target.overlay_document_id() {
                    source.transfer_primary_work_to_overlay(target);
                }
                source.transfer_overlay_work_to(target);
            }
            // Unwanted primaries may travel with a wanted overlay. They are clean by
            // preflight and must not pin a second, stale cache after the restore returns.
            if let Some((tab, slot, layout)) = closed {
                let evicted = self.layout_tabs.record_closed_view(tab, slot, ClosedLayoutView { view: source, marker_ui: ui, slot, layout });
                drop_later(evicted);
            } else { drop_later(Some((source, ui))); }
        }
    }

    pub(super) fn restore_tab_close_origin(&mut self) {
        if let Some((_, Some(original))) = self.tab_close_origin.take() {
            if self.layout_tabs.active_id() != Some(original) { self.switch_layout_tab_internal(original); self.observe(false); }
        }
    }
}
