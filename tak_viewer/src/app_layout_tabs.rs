//! Integration of whole-layout tabs with the existing dock and view slots.
use super::*;
use crate::ui::dock::EditorLayout;
use layout_tabs::{ClosedItem, TabId};
use std::collections::HashSet;

#[cfg(test)]
thread_local! { static FILE_IDENTITY_FALLBACKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
pub(super) fn reset_identity_fallbacks() { FILE_IDENTITY_FALLBACKS.with(|count| count.set(0)); }

#[cfg(test)]
pub(super) fn identity_fallbacks() -> usize { FILE_IDENTITY_FALLBACKS.with(std::cell::Cell::get) }

fn file_identity(path: &Path) -> PathBuf {
    #[cfg(test)] FILE_IDENTITY_FALLBACKS.with(|count| count.set(count.get() + 1));
    tak_core::takm::canonical_target(path).unwrap_or_else(|_| path.to_path_buf())
}

impl TakViewerApp {
    pub(super) fn notice_open_file_limit(&mut self) {
        self.notice = Some(format!("서로 다른 파일은 전체에서 {MAX_OPEN_FILES}개까지 열 수 있어요. 다른 파일을 닫고 다시 시도해 주세요."));
    }

    pub(crate) fn open_shot_tabs(&mut self, ctx: &egui::Context, files: &[PathBuf]) {
        for (index, group) in files.chunks(MAX_VIEWS).enumerate() {
            if index != 0 { self.new_layout_tab(); }
            for file in group {
                self.open_side(ctx, workspace::resolve_path(&file.to_string_lossy()));
            }
        }
    }

    pub(crate) fn shot_tabs_ready(&self, count: usize) -> bool {
        self.views.iter().chain(self.layout_tabs.tabs().iter()
            .filter_map(|tab| tab.layout.as_ref()).flat_map(|runtime| runtime.views.iter()))
            .filter(|view| view.data.is_some()).count() == count
    }

    /// A fresh open resolves its requested path once. Bound views are identified by the
    /// canonical path captured by their reader, so a later symlink change is never hidden
    /// behind a process-wide path cache.
    pub(super) fn admit_identity(&self, path: &Path) -> PathBuf {
        let identity = file_identity(path);
        *self.identity_candidate.borrow_mut() = Some((path.to_path_buf(), identity.clone()));
        identity
    }

    pub(super) fn discard_identity_candidate(&self) { self.identity_candidate.borrow_mut().take(); }

    pub(super) fn discard_identity_candidate_for(&self, path: &Path) {
        let mut candidate = self.identity_candidate.borrow_mut();
        if candidate.as_ref().is_some_and(|(requested, _)| requested == path) { candidate.take(); }
    }

    pub(super) fn known_identity(&self, path: &Path) -> PathBuf {
        if let Some((candidate, identity)) = self.identity_candidate.borrow().as_ref() {
            if candidate == path { return identity.clone(); }
        }
        for view in &self.views {
            if let Some(identity) = view.bound_identity(path).or_else(|| view.overlay_bound_identity(path)) {
                return identity;
            }
        }
        for tab in self.layout_tabs.tabs() {
            if let Some(runtime) = &tab.layout {
                for view in &runtime.views {
                    if let Some(identity) = view.bound_identity(path).or_else(|| view.overlay_bound_identity(path)) {
                        return identity;
                    }
                }
            }
        }
        file_identity(path)
    }

    pub(super) fn bound_primary_identity(view: &View, path: &Path) -> PathBuf {
        view.bound_identity(path).unwrap_or_else(|| file_identity(path))
    }

    pub(super) fn bound_overlay_identity(view: &View, path: &Path) -> PathBuf {
        view.overlay_bound_identity(path).unwrap_or_else(|| file_identity(path))
    }

    fn finish_layout_playback(&mut self) {
        let playing: Vec<usize> = self.open_views().into_iter().filter(|&slot| self.views[slot].pb.playing).collect();
        for &slot in &playing { self.views[slot].pb.pause(); }
        self.observe(true);
        // Preserve each tab's play state while hidden; activation resets its time anchor so
        // no frames elapse for a tab that was not drawn.
        for slot in playing { self.views[slot].pb.playing = true; }
    }
    pub(super) fn add_view_file_identities(view: &View, files: &mut HashSet<PathBuf>) {
        // The displayed source and pending replacement both count until the switch completes.
        if let Some(path) = view.path() { files.insert(Self::bound_primary_identity(view, path)); }
        if let Some(path) = view.shared_target() { files.insert(Self::bound_primary_identity(view, &path)); }
        if let Some(path) = view.overlay_path() { files.insert(Self::bound_overlay_identity(view, path)); }
        if let Some(path) = view.overlay_shared_target() { files.insert(Self::bound_overlay_identity(view, &path)); }
        if let Some(load) = &view.overlay_load { files.insert(Self::bound_overlay_identity(view, &load.path)); }
    }

    pub(super) fn open_file_identities(&self, _exclude_active_slot: Option<usize>) -> HashSet<PathBuf> {
        let mut files = HashSet::new();
        let hidden = self.layout_tabs.tabs().iter().filter_map(|tab| tab.layout.as_ref())
            .flat_map(|runtime| runtime.views.iter());
        for view in self.views.iter().chain(hidden) { Self::add_view_file_identities(view, &mut files); }
        if let Some((_, destination)) = self.takm_reserved_replacement() { files.insert(destination); }
        files
    }

    pub(super) fn allow_open_file(&mut self, path: &Path, exclude_active_slot: Option<usize>) -> bool {
        self.allow_open_files(&[path.to_path_buf()], exclude_active_slot)
    }

    pub(super) fn allow_replace_overlay(&mut self, path: &Path, _slot: usize) -> bool {
        let mut files = self.open_file_identities(None);
        files.insert(self.admit_identity(path));
        if files.len() <= MAX_OPEN_FILES { return true; }
        self.discard_identity_candidate();
        self.notice_open_file_limit();
        false
    }

    pub(super) fn allow_open_files(&mut self, paths: &[PathBuf], exclude_active_slot: Option<usize>) -> bool {
        let mut files = self.open_file_identities(exclude_active_slot);
        files.extend(paths.iter().map(|path| self.admit_identity(path)));
        if files.len() <= MAX_OPEN_FILES { return true; }
        self.discard_identity_candidate();
        self.notice_open_file_limit();
        false
    }

    pub(super) fn file_has_other_view(&self, slot: usize, path: &Path) -> bool {
        let id = self.known_identity(path);
        self.views.iter().enumerate().any(|(k, view)| k != slot && view.shared_target().is_some_and(|p| Self::bound_primary_identity(view, &p) == id))
            || self.layout_tabs.tabs().iter().filter_map(|tab| tab.layout.as_ref()).any(|runtime| {
                runtime.views.iter().any(|view| view.shared_target().is_some_and(|p| Self::bound_primary_identity(view, &p) == id))
            })
    }

    /// Read-only lookup in the same source order as `find_shared_file`. A source with no
    /// wrapper yet still exposes its document identity without allocating or publishing.
    fn retained_adoption_ready(&self, retained: &View, path: &Path) -> Option<bool> {
        let wanted = self.known_identity(path);
        let live = || self.views.iter().chain(self.layout_tabs.tabs().iter()
            .filter_map(|tab| tab.layout.as_ref()).flat_map(|runtime| runtime.views.iter()));
        for source in live() {
            if source.shared_target().is_some_and(|p| Self::bound_primary_identity(source, &p) == wanted)
                && source.waiting_shared_path().is_none() {
                if let Some(ready) = retained.can_adopt_from_view(source) { return Some(ready); }
            }
        }
        for host in live() {
            if host.overlay_shared_target().is_some_and(|p| Self::bound_overlay_identity(host, &p) == wanted) {
                if let Some(owner) = host.overlay_owner() {
                    if let Some(ready) = retained.can_adopt_from_view(owner) { return Some(ready); }
                }
            }
        }
        if self.restoring_layout {
            for held in &self.layout_restore_files {
                let source = held.view();
                if source.shared_target().is_some_and(|p| Self::same_local_path(&p, path)) {
                    if let Some(ready) = retained.can_adopt_from_view(source) { return Some(ready); }
                }
                if source.overlay_path().is_some_and(|p| Self::same_local_path(p, path)) {
                    if let Some(owner) = source.overlay_owner() {
                        if let Some(ready) = retained.can_adopt_from_view(owner) { return Some(ready); }
                    }
                }
            }
        }
        None
    }

    /// Check every adoption before a batch removes any retained runtime from history.
    pub(super) fn retained_view_ready(&self, view: &View) -> bool {
        if view.waiting_shared_path().is_none() {
            if let Some(path) = view.shared_target() {
                if let Some(ready) = self.retained_adoption_ready(view, &path) {
                    if !ready { return false; }
                } else if self.file_is_loading_elsewhere(&path, usize::MAX) { return false; }
            }
        }
        if let Some(path) = view.overlay_shared_target() {
            let Some(owner) = view.overlay_owner() else { return false; };
            if let Some(ready) = self.retained_adoption_ready(owner, &path) {
                if !ready { return false; }
            } else if self.file_is_loading_elsewhere(&path, usize::MAX) { return false; }
        }
        true
    }

    /// A file may have been opened afresh after this view was closed. Reuse that live
    /// document instead of resurrecting a second independent document for the same path.
    fn reconcile_retained_view(&mut self, view: &mut View) -> bool {
        if !self.retained_view_ready(view) {
            self.notice = Some("닫은 화면이나 겹쳐 본 파일의 작업이 아직 끝나지 않았어요. 읽기·편집·저장이 끝난 뒤 다시 열어 주세요.".into());
            return false;
        }
        if view.waiting_shared_path().is_none() {
            if let Some(path) = view.shared_target() {
                if let Some(shared) = self.find_shared_file(&path, usize::MAX) {
                    if !view.adopt_shared_preserving_state(shared, self.layout_time) {
                        self.notice = Some("닫은 화면의 편집이나 저장이 아직 끝나지 않았어요. 작업을 마친 뒤 다시 열어 주세요.".into());
                        return false;
                    }
                } else if self.file_is_loading_elsewhere(&path, usize::MAX) {
                    self.notice = Some("같은 파일을 읽는 중이에요. 읽기가 끝난 뒤 다시 열어 주세요.".into());
                    return false;
                }
            }
        }
        if let Some(path) = view.overlay_shared_target() {
            if let Some(shared) = self.find_shared_file(&path, usize::MAX) {
                if !view.overlay_owner_mut().is_some_and(|owner| owner.adopt_shared_preserving_state(shared, self.layout_time)) {
                    self.notice = Some("겹쳐 본 파일의 편집이나 저장이 아직 끝나지 않았어요. 작업을 마친 뒤 다시 열어 주세요.".into());
                    return false;
                }
                view.sync_shared();
            } else if self.file_is_loading_elsewhere(&path, usize::MAX) {
                self.notice = Some("겹쳐 본 파일을 읽는 중이에요. 읽기가 끝난 뒤 다시 열어 주세요.".into());
                return false;
            }
        }
        true
    }

    /// Move a shared document's running workers to a surviving view before its owner closes.
    pub(super) fn handoff_shared_owner(&mut self, slot: usize, outside_tab: bool) -> bool {
        let Some(document) = self.views[slot].document_id() else { return false };
        if let Some(other) = (!outside_tab).then(|| self.views.iter().enumerate().find_map(|(index, view)| {
            (index != slot && view.document_id() == Some(document)).then_some(index)
        })).flatten() {
            if self.marker_ui[slot].has_pending_work() && self.marker_ui[other].has_pending_work() { return false; }
            let (source, target) = if slot < other {
                let (left, right) = self.views.split_at_mut(other);
                (&mut left[slot], &mut right[0])
            } else {
                let (left, right) = self.views.split_at_mut(slot);
                (&mut right[0], &mut left[other])
            };
            let needs_transfer = source.owns_shared_file_work();
            if needs_transfer && !source.transfer_shared_loads(target) { return false; }
            let target_id = target.file_id;
            let (source_ui, target_ui) = if slot < other {
                let (left, right) = self.marker_ui.split_at_mut(other);
                (&mut left[slot], &mut right[0])
            } else {
                let (left, right) = self.marker_ui.split_at_mut(slot);
                (&mut right[0], &mut left[other])
            };
            source_ui.transfer_work_to(target_ui, target_id);
            return true;
        }
        for tab in self.layout_tabs.tabs_mut() {
            let Some(runtime) = &mut tab.layout else { continue };
            for other in 0..MAX_VIEWS {
                if runtime.views[other].document_id() != Some(document) { continue; }
                if self.marker_ui[slot].has_pending_work() && runtime.marker_ui[other].has_pending_work() { return false; }
                let needs_transfer = self.views[slot].owns_shared_file_work();
                if needs_transfer && !self.views[slot].transfer_shared_loads(&mut runtime.views[other]) { return false; }
                let target_id = runtime.views[other].file_id;
                self.marker_ui[slot].transfer_work_to(&mut runtime.marker_ui[other], target_id);
                return true;
            }
        }
        false
    }

    /// Transfer the only deferred reader from a closing primary to a surviving overlay.
    pub(super) fn handoff_primary_to_overlay(&mut self, slot: usize, outside_tab: bool) -> bool {
        let Some(document) = self.views[slot].document_id() else { return false };
        if self.marker_ui[slot].has_pending_work() { return false; }
        if !outside_tab {
            for other in 0..MAX_VIEWS {
                if other == slot || self.views[other].overlay_document_id() != Some(document) { continue; }
                let (source, target) = if slot < other {
                    let (left, right) = self.views.split_at_mut(other); (&mut left[slot], &mut right[0])
                } else {
                    let (left, right) = self.views.split_at_mut(slot); (&mut right[0], &mut left[other])
                };
                if source.transfer_primary_work_to_overlay(target) { return true; }
            }
        }
        for tab in self.layout_tabs.tabs_mut() {
            let Some(runtime) = &mut tab.layout else { continue };
            for target in &mut runtime.views {
                if target.overlay_document_id() == Some(document) && self.views[slot].transfer_primary_work_to_overlay(target) { return true; }
            }
        }
        false
    }

    pub(super) fn has_other_overlay_for_primary(&self, slot: usize, outside_tab: bool) -> bool {
        let Some(document) = self.views[slot].document_id() else { return false };
        (!outside_tab && self.views.iter().enumerate().any(|(other, view)| other != slot && view.overlay_document_id() == Some(document)))
            || self.layout_tabs.tabs().iter().filter_map(|tab| tab.layout.as_ref()).flat_map(|runtime| runtime.views.iter())
                .any(|view| view.overlay_document_id() == Some(document))
    }

    pub(super) fn has_other_reference_for_overlay(&self, slot: usize, outside_tab: bool) -> bool {
        let Some(document) = self.views[slot].overlay_document_id() else { return false };
        (!outside_tab && self.views.iter().enumerate().any(|(other, view)| other != slot
            && (view.document_id() == Some(document) || view.overlay_document_id() == Some(document))))
            || self.layout_tabs.tabs().iter().filter_map(|tab| tab.layout.as_ref()).flat_map(|runtime| runtime.views.iter())
                .any(|view| view.document_id() == Some(document) || view.overlay_document_id() == Some(document))
    }

    /// Restore exactly the view that was moved to reopen history by a close action.
    pub(super) fn restore_closed_view_for_history(&mut self, tab: TabId, slot: usize, path: &Path, file_id: usize) -> bool {
        let item = self.layout_tabs.take_closed_where(|item| match item {
            ClosedItem::View { tab_id, index, view } => *tab_id == tab && *index == slot
                && view.view.file_id == file_id && view.view.target() == Some(path),
            _ => false,
        });
        let Some(ClosedItem::View { tab_id, index, view: mut closed }) = item else { return false };
        if closed.marker_ui.has_pending_work() || !self.reconcile_retained_view(&mut closed.view) {
            if self.notice.is_none() { self.notice = Some("닫힌 화면의 편집이나 저장이 아직 끝나지 않았어요.".into()); }
            let evicted = self.layout_tabs.remember_closed(ClosedItem::View { tab_id, index, view: closed });
            drop_later(evicted);
            return false;
        }
        closed.view.sync_shared();
        drop_later(Some(std::mem::replace(&mut self.views[slot], closed.view)));
        drop_later(Some(std::mem::replace(&mut self.marker_ui[slot], closed.marker_ui)));
        self.handoff_closed_file_work(slot);
        true
    }

    /// Return a view to the same reopen history when a close is redone.
    pub(super) fn remember_closed_view_for_history(&mut self, tab: TabId, slot: usize) -> bool {
        if slot >= MAX_VIEWS || self.views[slot].target().is_none() { return false; }
        let layout = dock::editor_layout(&self.dock);
        let view = std::mem::take(&mut self.views[slot]);
        let marker_ui = std::mem::take(&mut self.marker_ui[slot]);
        let evicted = self.layout_tabs.record_closed_view(tab, slot, ClosedLayoutView { view, marker_ui, slot, layout });
        drop_later(evicted);
        true
    }

    /// Find an already-loaded copy; the returned handle shares both data and edit document.
    pub(super) fn find_shared_file(&mut self, path: &Path, exclude: usize) -> Option<views::SharedFile> {
        let wanted = self.known_identity(path);
        for (slot, view) in self.views.iter_mut().enumerate() {
            if slot != exclude && view.shared_target().is_some_and(|p| Self::bound_primary_identity(view, &p) == wanted) && view.waiting_shared_path().is_none() {
                if let Some(shared) = view.shared_file() { return Some(shared); }
            }
        }
        for tab in self.layout_tabs.tabs_mut() {
            let Some(runtime) = &mut tab.layout else { continue };
            for view in &mut runtime.views {
                if view.shared_target().is_some_and(|p| Self::bound_primary_identity(view, &p) == wanted) && view.waiting_shared_path().is_none() {
                    if let Some(shared) = view.shared_file() { return Some(shared); }
                }
            }
        }
        if let Some(shared) = self.find_shared_overlay_file(path) { return Some(shared); }
        if let Some(shared) = self.find_layout_restore_file(path) { return Some(shared); }
        None
    }

    /// Ordinary opens reread closed-only files. Keep only a candidate undo identity, which
    /// the new reader may restore after its independent source stamp proves unchanged.
    pub(super) fn find_retained_document(&mut self, path: &Path) -> Option<views::RetainedDocument> {
        let wanted = self.known_identity(path);
        let candidate = |view: &View| {
            view.shared_target().filter(|p| Self::bound_primary_identity(view, p) == wanted)
                .and_then(|_| view.retained_document())
        };
        for item in self.layout_tabs.closed_items_mut().rev() {
            match item {
                ClosedItem::View { view: closed, .. } => {
                    if !closed.marker_ui.has_pending_work() {
                        if let Some(candidate) = candidate(&closed.view) { return Some(candidate); }
                    }
                    if let Some(candidate) = closed.view.overlay_owner().and_then(candidate) { return Some(candidate); }
                }
                ClosedItem::Batch { .. } => unreachable!("closed_items_mut exposes leaves"),
                ClosedItem::Tab(closed) => {
                    let Some(runtime) = &mut closed.tab.layout else { continue };
                    for (slot, view) in runtime.views.iter().enumerate() {
                        if !runtime.marker_ui[slot].has_pending_work() {
                            if let Some(candidate) = candidate(view) { return Some(candidate); }
                        }
                        if let Some(candidate) = view.overlay_owner().and_then(candidate) { return Some(candidate); }
                    }
                }
            }
        }
        None
    }

    /// A closed runtime can own the sole deferred force/EMG reader. Move it to a freshly
    /// opened or restored primary/overlay sharing its document before the view is used.
    pub(super) fn handoff_closed_file_work(&mut self, slot: usize) {
        let primary = self.views[slot].document_id();
        let overlay = self.views[slot].overlay_document_id();
        if primary.is_none() && overlay.is_none() { return; }
        for item in self.layout_tabs.closed_items_mut().rev() {
            let mut transfer = |source: &mut View| {
                if source.owns_shared_file_work() {
                    if source.document_id().is_some() && source.document_id() == primary {
                        source.transfer_shared_loads(&mut self.views[slot]);
                    } else if source.document_id().is_some() && source.document_id() == overlay {
                        source.transfer_primary_work_to_overlay(&mut self.views[slot]);
                    }
                }
                if source.overlay_document_id().is_some()
                    && (source.overlay_document_id() == primary || source.overlay_document_id() == overlay) {
                    source.transfer_overlay_work_to(&mut self.views[slot]);
                }
            };
            match item {
                ClosedItem::View { view: closed, .. } => transfer(&mut closed.view),
                ClosedItem::Batch { .. } => unreachable!("closed_items_mut exposes leaves"),
                ClosedItem::Tab(closed) => {
                    if let Some(runtime) = &mut closed.tab.layout { for view in &mut runtime.views { transfer(view); } }
                }
            }
        }
    }

    pub(super) fn file_is_loading_elsewhere(&self, path: &Path, exclude: usize) -> bool {
        let wanted = self.known_identity(path);
        let matches = |view: &View| view.loading.as_ref().is_some_and(|(p, _)| Self::bound_primary_identity(view, p) == wanted);
        self.views.iter().enumerate().any(|(slot, view)| slot != exclude && matches(view))
            || self.layout_tabs.tabs().iter().filter_map(|tab| tab.layout.as_ref()).any(|runtime| runtime.views.iter().any(matches))
            || self.overlay_file_is_loading(path)
    }

    /// The original read finished: attach all waiting views without another file read.
    pub(super) fn attach_shared_waiters(&mut self, ctx: &egui::Context, path: &Path) {
        let Some(shared) = self.find_shared_file(path, usize::MAX) else { return };
        let wanted = self.known_identity(path);
        let now = ctx.input(|input| input.time);
        let speed = self.settings.speed;
        // A fresh read supersedes old closed aliases and their deferred readers. Otherwise
        // reopening an older alias after this owner closes could revive stale data/jobs,
        // even when an unchanged source deliberately preserved its edit document identity.
        let blocked: HashSet<_> = self.overlay_marker_ui.iter().filter(|(_, ui)| ui.has_pending_work()).map(|(id, _)| *id).collect();
        let reconcile = |view: &mut View| {
            if !view.document_id().is_some_and(|id| blocked.contains(&id))
                && view.shared_target().is_some_and(|p| Self::bound_primary_identity(view, &p) == wanted)
                && view.adopt_shared_preserving_state(shared.clone(), now) {
                view.cancel_load();
            }
        };
        for item in self.layout_tabs.closed_items_mut() {
            match item {
                ClosedItem::View { view: closed, .. } => {
                    if !closed.marker_ui.has_pending_work() { reconcile(&mut closed.view); }
                    if let Some(owner) = closed.view.overlay_owner_mut() { reconcile(owner); }
                }
                ClosedItem::Batch { .. } => unreachable!("closed_items_mut exposes leaves"),
                ClosedItem::Tab(closed) => {
                    if let Some(runtime) = &mut closed.tab.layout {
                        for (slot, view) in runtime.views.iter_mut().enumerate() {
                            if !runtime.marker_ui[slot].has_pending_work() { reconcile(view); }
                            if let Some(owner) = view.overlay_owner_mut() { reconcile(owner); }
                        }
                    }
                }
            }
        }
        let attach = |view: &mut View, next: &mut usize| {
            if view.waiting_shared_path().is_some_and(|p| Self::bound_primary_identity(view, p) == wanted) {
                *next += 1;
                drop_later(view.show_shared_file(shared.clone(), *next, speed, now));
            }
        };
        for view in &mut self.views { attach(view, &mut self.next_file_id); }
        for tab in self.layout_tabs.tabs_mut() {
            if let Some(runtime) = &mut tab.layout {
                for view in &mut runtime.views { attach(view, &mut self.next_file_id); }
            }
        }
        self.discard_identity_candidate_for(path);
    }

    /// A failed shared read must release every duplicate waiter; none may spin forever.
    pub(super) fn fail_shared_waiters(&mut self, path: &Path) {
        let wanted = self.known_identity(path);
        let clear = |view: &mut View| {
            if view.waiting_shared_path().is_some_and(|p| Self::bound_primary_identity(view, p) == wanted) { view.clear_shared_wait(); }
        };
        for view in &mut self.views { clear(view); }
        for tab in self.layout_tabs.tabs_mut() {
            if let Some(runtime) = &mut tab.layout { for view in &mut runtime.views { clear(view); } }
        }
        self.discard_identity_candidate_for(path);
    }

    /// If the sole in-flight owner closes, let one waiting view finish the shared open.
    pub(super) fn promote_shared_waiter(&mut self, ctx: &egui::Context, path: &Path) {
        if self.promote_remote_shared_waiter(ctx, path) { return; }
        if self.find_shared_file(path, usize::MAX).is_some() {
            self.attach_shared_waiters(ctx, path);
            return;
        }
        if self.file_is_loading_elsewhere(path, usize::MAX) { return; }
        let wanted = self.known_identity(path);
        #[cfg(test)]
        let hold_loads = self.hold_loads;
        for view in &mut self.views {
            if view.waiting_shared_path().is_some_and(|p| Self::bound_primary_identity(view, p) == wanted) {
                let restore = view.restore.clone();
                if let Some(path) = view.clear_shared_wait() {
                    #[cfg(test)]
                    if hold_loads { view.hold_load(path, restore); view.set_pending_identity(wanted.clone()); return; }
                    view.start_load(ctx, path, restore);
                    view.set_pending_identity(wanted.clone());
                }
                return;
            }
        }
        for tab in self.layout_tabs.tabs_mut() {
            let Some(runtime) = &mut tab.layout else { continue };
            for view in &mut runtime.views {
                if view.waiting_shared_path().is_some_and(|p| Self::bound_primary_identity(view, p) == wanted) {
                    let restore = view.restore.clone();
                    if let Some(path) = view.clear_shared_wait() {
                        #[cfg(test)]
                        if hold_loads { view.hold_load(path, restore); view.set_pending_identity(wanted.clone()); return; }
                        view.start_load(ctx, path, restore);
                        view.set_pending_identity(wanted.clone());
                    }
                    return;
                }
            }
        }
    }

    /// Only tabs with already-running file reads or an explicit EMG request are visited.
    /// No playback, 3D scene, graph or idle hidden view is processed here.
    pub(super) fn poll_hidden_file_work(&mut self, ctx: &egui::Context) {
        let mut arrived = Vec::new();
        let mut failed = Vec::new();
        let now = ctx.input(|input| input.time);
        let speed = self.settings.speed;
        let saving = self.saving_documents();
        for (tab, slot) in self.hidden_file_locations() {
            let Some(runtime) = self.layout_tabs.work_payload_mut(tab).and_then(|tab| tab.layout.as_mut()) else { continue };
            let view = &mut runtime.views[slot];
            if let Some(Loaded { path, result, rx }) = view.poll_markers() {
                match result {
                    Ok((data, steps)) => {
                        self.next_file_id += 1;
                        view.load_steps = steps;
                        drop_later(view.show(data, rx, self.next_file_id, speed, now));
                        arrived.push(path);
                    }
                    Err(reason) => {
                        self.error = Some(format!("{}\n\n{}", path.display(), reason));
                        failed.push(path);
                    }
                }
            }
            view.poll_started_comment_ids();
            view.poll_comments();
            view.poll_analog();
            if !view.document_id().is_some_and(|id| saving.contains(&id)) { view.request_emg(ctx); }
            let overlay_can_start = !view.overlay_document_id().is_some_and(|id| saving.contains(&id));
            view.poll_overlay_file_work_gated(ctx, overlay_can_start);
            self.reindex_hidden_slot(tab, slot);
        }
        for path in arrived {
            if self.find_shared_file(&path, usize::MAX).is_some() {
                self.attach_shared_waiters(ctx, &path);
            } else {
                self.promote_shared_waiter(ctx, &path);
            }
        }
        for path in failed { self.fail_shared_waiters(&path); }
    }

    /// A marker/save worker started in a tab can finish after that tab is hidden. Visit only
    /// tabs that own such workers, and only between gestures, so history remains ordered.
    pub(super) fn poll_hidden_marker_work(&mut self, ctx: &egui::Context) {
        if self.hold.is_some() || self.comment_edit.is_some()
            || (self.typing(ctx) && self.dirty_prompt.is_none()) || ctx.input(|input| input.pointer.any_down()) { return; }
        let pending = self.hidden_marker_tabs();
        if pending.is_empty() { return; }
        let Some(original) = self.layout_tabs.active_id() else { return };
        self.observe(true);
        let reveal = self.explorer.reveal.clone();
        let focused = self.focused_tab;
        let strip = self.type_strip.take();
        // A save belonging to a hidden sibling must finish even while a dirty dialog is open.
        // Keep the dialog out of the temporary runtime and complete it only after returning.
        let prompt = self.dirty_prompt.take();
        for id in pending {
            if !self.switch_layout_tab_internal(id) { continue; }
            self.observe(false);
            self.poll_marker_jobs(ctx);
            self.poll_marker_saves(ctx);
        }
        self.switch_layout_tab_internal(original);
        self.observe(false);
        self.explorer.reveal = reveal;
        self.focused_tab = focused;
        self.type_strip = strip;
        if let Some(mut prompt) = prompt {
            if self.overlay_prompt_slot().is_some() { self.dirty_prompt = Some(prompt); return; }
            self.views[prompt.slot].sync_shared();
            if prompt.saving && !self.marker_saving(prompt.slot) {
                prompt.saving = false;
                if !self.views[prompt.slot].marker_dirty() && !self.comments_unsaved(prompt.slot)
                    && !self.marker_job_running(prompt.slot) && self.views[prompt.slot].edit_refresh.is_none() {
                    self.finish_transition(ctx, prompt.transition);
                    return;
                }
            }
            self.dirty_prompt = Some(prompt);
        }
    }

    /// A restore cannot exceed the same distinct-file cap as a fresh open.
    fn allow_restore_tab(&mut self, id: TabId) -> bool {
        let mut files = self.open_file_identities(None);
        let Some(closed) = self.layout_tabs.closed_items().find_map(|item| match item {
            ClosedItem::Tab(closed) if closed.tab.id == id => Some(closed),
            _ => None,
        }) else { return false };
        if let Some(runtime) = &closed.tab.layout {
            for view in &runtime.views { Self::add_view_file_identities(view, &mut files); }
        }
        if files.len() <= MAX_OPEN_FILES { return true; }
        self.notice_open_file_limit();
        false
    }

    /// Rebuild an evicted closed tab for the independent 500-action undo history.
    pub(super) fn create_layout_tab_for_undo(&mut self, id: TabId, name: &str, index: usize, paths: &[PathBuf]) -> bool {
        if self.layout_tabs.tab(id).is_some() { return false; }
        let mut files = self.open_file_identities(None);
        files.extend(paths.iter().map(|path| self.known_identity(path)));
        if files.len() > MAX_OPEN_FILES {
            self.notice_open_file_limit();
            return false;
        }
        self.stash_layout_runtime();
        let closed = layout_tabs::ClosedTab {
            tab: layout_tabs::LayoutTab { id, name: name.to_owned(), layout: Some(LayoutRuntime::default()) },
            index,
        };
        let restored = self.layout_tabs.restore_tab(closed);
        let runtime = self.layout_tabs.tab_mut(restored).and_then(|tab| tab.layout.take()).unwrap_or_default();
        self.install_layout_runtime(runtime);
        true
    }
    /// Migrate the single-layout settings or restore every persisted top tab.
    pub(super) fn restore_layout_tabs(&mut self, ctx: &egui::Context) {
        let Some(saved) = self.settings.layout_tabs.take() else {
            self.restore_views(ctx);
            if self.open_views().is_empty() { self.layout_tabs = Default::default(); }
            return;
        };
        let desired = self.settings.active_layout_tab;
        self.layout_tabs = Default::default();
        self.install_layout_runtime(LayoutRuntime::default());
        let mut ids = Vec::new();
        for saved_tab in saved {
            self.stash_layout_runtime();
            let id = self.layout_tabs.add(None);
            if !saved_tab.name.trim().is_empty() { let _ = self.layout_tabs.rename(id, &saved_tab.name); }
            ids.push(id);
            let mut runtime = LayoutRuntime::default();
            runtime.layout = dock::valid_editor_layout(&saved_tab.layout);
            runtime.active = usize::from(saved_tab.active).min(MAX_VIEWS - 1);
            runtime.graph_boxes = saved_tab.graph_boxes;
            runtime.graph_boxes.resize(MAX_VIEWS, GraphBox::default());
            runtime.plate_boxes = saved_tab.plate_boxes;
            runtime.plate_boxes.resize(MAX_VIEWS, GraphBox::default());
            runtime.plate_channels = saved_tab.plate_channels;
            runtime.plate_channels.resize(MAX_VIEWS, LaneSet::default());
            self.install_layout_runtime(runtime);
            let open = self.open_views();
            for item in saved_tab.views {
                let slot = usize::from(item.slot);
                let path = PathBuf::from(item.path);
                if !open.contains(&slot) || remote_ui::is_remote_key(&path) || !path.is_file() { continue; }
                self.open_in(ctx, slot, path);
                if item.spot.is_some() { self.views[slot].restore = item.spot; }
            }
            // Missing files do not create phantom 3D views.
            for slot in self.open_views() {
                if self.views[slot].target().is_none() { dock::remove_view(&mut self.dock, slot as u8); }
            }
        }
        if let Some(&selected) = ids.get(desired.min(ids.len().saturating_sub(1))) {
            if self.layout_tabs.active_id() != Some(selected) { self.switch_layout_tab_internal(selected); }
        }
    }

    /// Record each tab's independent small viewing state, not its large data buffers.
    pub(super) fn sync_layout_settings(&mut self) {
        fn save_tab(name: &str, layout: EditorLayout, active: usize, views: &[View; MAX_VIEWS],
                    graph_boxes: &[GraphBox], plate_boxes: &[GraphBox], plate_channels: &[LaneSet]) -> SavedLayoutTab {
            let saved_views = views.iter().enumerate().filter_map(|(slot, view)| {
                if view.remote.is_some() || view.remote_loading.is_some() { return None; }
                let path = view.target()?;
                if remote_ui::is_remote_key(path) { return None; }
                Some(SavedLayoutView {
                    slot: slot as u8,
                    path: path.to_string_lossy().into_owned(),
                    spot: view.restore.clone().or_else(|| view.spot()),
                })
            }).collect();
            SavedLayoutTab {
                name: name.to_owned(), layout, views: saved_views, active: active as u8,
                graph_boxes: graph_boxes.to_vec(), plate_boxes: plate_boxes.to_vec(), plate_channels: plate_channels.to_vec(),
            }
        }
        let active = self.layout_tabs.active_id();
        let mut saved = Vec::with_capacity(self.layout_tabs.tabs().len());
        for tab in self.layout_tabs.tabs() {
            if Some(tab.id) == active {
                saved.push(save_tab(&tab.name, dock::editor_layout(&self.dock), self.active, &self.views,
                    &self.settings.graph_boxes, &self.settings.plate_boxes, &self.settings.plate_channels));
            } else if let Some(runtime) = &tab.layout {
                saved.push(save_tab(&tab.name, runtime.layout.clone(), runtime.active, &runtime.views,
                    &runtime.graph_boxes, &runtime.plate_boxes, &runtime.plate_channels));
            }
        }
        self.settings.active_layout_tab = self.layout_tabs.tabs().iter().position(|tab| Some(tab.id) == active).unwrap_or(0);
        self.settings.layout_tabs = Some(saved);
    }

    /// Take the active layout out of the app without copying view data or cancelling jobs.
    fn take_layout_runtime(&mut self) -> LayoutRuntime {
        LayoutRuntime {
            views: std::mem::replace(&mut self.views, std::array::from_fn(|_| View::default())),
            marker_ui: std::mem::replace(&mut self.marker_ui, std::array::from_fn(|_| marker_edit::MarkerUi::default())),
            layout: dock::editor_layout(&self.dock),
            active: self.active,
            graph_boxes: std::mem::replace(&mut self.settings.graph_boxes, vec![GraphBox::default(); MAX_VIEWS]),
            plate_boxes: std::mem::replace(&mut self.settings.plate_boxes, vec![GraphBox::default(); MAX_VIEWS]),
            plate_channels: std::mem::replace(&mut self.settings.plate_channels, vec![LaneSet::default(); MAX_VIEWS]),
        }
    }

    fn install_layout_runtime(&mut self, mut runtime: LayoutRuntime) {
        for view in &mut runtime.views {
            view.sync_shared();
            // A hidden tab does no playback stepping; resume at the last shown frame.
            if view.pb.playing { view.pb.set_frame(view.pb.frame, self.layout_time); }
        }
        dock::set_editor_layout(&mut self.dock, &runtime.layout);
        self.views = runtime.views;
        self.marker_ui = runtime.marker_ui;
        self.settings.graph_boxes = runtime.graph_boxes;
        self.settings.plate_boxes = runtime.plate_boxes;
        self.settings.plate_channels = runtime.plate_channels;
        let open = self.open_views();
        self.active = if open.contains(&runtime.active) { runtime.active } else { open.first().copied().unwrap_or(0) };
        if !open.is_empty() { self.activate(self.active); }
        self.focused_tab = self.dock.find_active_focused().map(|(_, p)| *p);
        self.explorer.reveal = self.views[self.active].target().map(Path::to_path_buf);
        self.hold = None;
        self.type_strip = None;
        self.comment_edit = None;
    }

    fn stash_layout_runtime(&mut self) {
        let Some(id) = self.layout_tabs.active_id() else { return };
        let runtime = self.take_layout_runtime();
        if let Some(tab) = self.layout_tabs.tab_mut(id) { tab.layout = Some(runtime); }
    }

    /// Switch without recording an action. Undo/redo calls this directly.
    pub(super) fn switch_layout_tab_internal(&mut self, id: TabId) -> bool {
        if self.layout_tabs.active_id() == Some(id) || self.layout_tabs.tab(id).is_none() { return false; }
        self.stash_layout_runtime();
        if !self.layout_tabs.activate(id) { return false; }
        let runtime = self.layout_tabs.active_mut().and_then(|tab| tab.layout.take()).unwrap_or_default();
        self.install_layout_runtime(runtime);
        true
    }

    /// Create an empty top tab. Its predecessor keeps its whole live layout.
    pub(super) fn new_layout_tab(&mut self) -> TabId {
        self.finish_layout_playback();
        let before_active = self.layout_tabs.active_id();
        self.stash_layout_runtime();
        let id = self.layout_tabs.add(None);
        self.install_layout_runtime(LayoutRuntime::default());
        self.observe(false);
        if !self.no_persist {
            if let Some(snapshot) = self.tab_undo_snapshot(id) {
                self.history.push(vec![undo::Part::TabLifecycle { id, before_open: false, after_open: true, before_active, after_active: Some(id), snapshot }]);
            }
        }
        id
    }

    /// Opening from zero tabs creates its tab and file as one undoable action.
    pub(super) fn record_implicit_file_open(&mut self, created: bool) {
        if !created { return; }
        self.observe(false);
        if self.no_persist { return; }
        let Some(id) = self.layout_tabs.active_id() else { return };
        if let Some(snapshot) = self.tab_undo_snapshot(id) {
            self.history.push(vec![undo::Part::TabLifecycle {
                id, before_open: false, after_open: true,
                before_active: None, after_active: Some(id), snapshot,
            }]);
        }
    }

    pub(super) fn switch_layout_tab(&mut self, id: TabId) {
        if self.dirty_prompt.is_some() { return; }
        let Some(before) = self.layout_tabs.active_id() else { return };
        if before == id || self.layout_tabs.tab(id).is_none() { return; }
        self.finish_layout_playback();
        if self.switch_layout_tab_internal(id) {
            self.observe(false);
            if !self.no_persist { self.history.push(vec![undo::Part::TabSwitch { before, after: id }]); }
        }
    }

    /// Move a closed whole tab back to the strip without rereading its loaded file data.
    pub(super) fn restore_closed_tab_internal(&mut self, id: TabId) -> bool {
        if !self.allow_restore_tab(id) { return false; }
        let Some(mut closed) = self.layout_tabs.take_closed_tab(id) else { return false };
        if let Some(runtime) = &mut closed.tab.layout {
            for (slot, view) in runtime.views.iter_mut().enumerate() {
                if runtime.marker_ui[slot].has_pending_work() || !self.reconcile_retained_view(view) {
                    if self.notice.is_none() { self.notice = Some("닫힌 탭의 편집이나 저장이 아직 끝나지 않았어요.".into()); }
                    let evicted = self.layout_tabs.remember_closed(ClosedItem::Tab(closed));
                    drop_later(evicted);
                    return false;
                }
            }
        }
        self.stash_layout_runtime();
        let restored = self.layout_tabs.restore_tab(closed);
        let runtime = self.layout_tabs.tab_mut(restored).and_then(|tab| tab.layout.take()).unwrap_or_default();
        self.install_layout_runtime(runtime);
        for slot in 0..MAX_VIEWS { self.handoff_closed_file_work(slot); }
        self.pending_shared_promotions.extend(self.views.iter().filter_map(|v| v.waiting_shared_path().map(Path::to_path_buf)));
        true
    }

    /// Move a tab to the bounded reopen history, without asking or recording undo.
    pub(super) fn close_layout_tab_internal(&mut self, id: TabId) -> bool {
        if self.layout_tabs.tab(id).is_none() { return false; }
        let was_active = self.layout_tabs.active_id() == Some(id);
        if was_active {
            for slot in self.open_views() {
                let remote = self.views[slot].remote_loading.as_ref().map(|(_, _, path)| path.clone());
                self.prepare_close_remote_view(slot);
                if let Some(path) = remote { self.pending_shared_promotions.push(path); }
                if let Some(path) = self.views[slot].loading.as_ref().map(|(path, _)| path.clone()) {
                    let restore = self.views[slot].restore.clone();
                    let identity = Self::bound_primary_identity(&self.views[slot], &path);
                    self.views[slot].wait_for_shared(path.clone(), restore);
                    self.views[slot].set_pending_identity(identity);
                    self.pending_shared_promotions.push(path);
                }
            }
        }
        if was_active { self.stash_layout_runtime(); }
        let Some(closed) = self.layout_tabs.take_tab(id) else { return false };
        let evicted = self.layout_tabs.remember_closed(ClosedItem::Tab(closed));
        drop_later(evicted);
        if was_active {
            let runtime = self.layout_tabs.active_mut().and_then(|tab| tab.layout.take()).unwrap_or_default();
            self.install_layout_runtime(runtime);
        }
        true
    }

    /// Prompt for dirty files in this tab only when closing removes their last view.
    pub(super) fn close_layout_tab(&mut self, id: TabId) {
        if self.dirty_prompt.is_some() { return; }
        if self.layout_tabs.tab(id).is_none() { return; }
        let original = self.tab_close_origin.filter(|(closing, _)| *closing == id)
            .map(|(_, original)| original).unwrap_or_else(|| self.layout_tabs.active_id());
        self.tab_close_origin = Some((id, original));
        if self.layout_tabs.active_id() != Some(id) {
            self.observe(true);
            self.switch_layout_tab_internal(id);
            self.observe(false);
        }
        if self.guard_tab_removal(marker_edit::PendingTransition::CloseLayoutTab { id }) {
            if self.dirty_prompt.is_none() { self.restore_tab_close_origin(); }
            return;
        }
        self.finish_layout_playback();
        let before_active = original;
        let snapshot = self.tab_undo_snapshot(id);
        if self.close_layout_tab_internal(id) {
            self.restore_tab_close_origin();
            self.observe(false);
            if !self.no_persist {
                if let Some(snapshot) = snapshot {
                    self.history.push(vec![undo::Part::TabLifecycle { id, before_open: true, after_open: false, before_active, after_active: self.layout_tabs.active_id(), snapshot }]);
                }
            }
        }
    }

    /// Undo must leave its history entry in place until all last-reference prompts finish.
    pub(super) fn guard_layout_tab_undo(&mut self, id: TabId, redo: bool) -> bool {
        if self.layout_tabs.tab(id).is_none() { return false; }
        if self.layout_tabs.active_id() != Some(id) {
            self.switch_layout_tab_internal(id);
            self.observe(false);
        }
        if self.guard_tab_removal(marker_edit::PendingTransition::Undo { redo }) {
            if self.dirty_prompt.is_none() { self.restore_tab_close_origin(); }
            return true;
        }
        false
    }

    pub(super) fn path_in_other_tab(&self, id: TabId, path: &Path) -> bool {
        let wanted = self.known_identity(path);
        self.layout_tabs.tabs().iter().any(|tab| {
            if tab.id == id { return false; }
            let views = if Some(tab.id) == self.layout_tabs.active_id() { Some(&self.views) } else { tab.layout.as_ref().map(|runtime| &runtime.views) };
            views.is_some_and(|views| views.iter().any(|v| v.shared_target().is_some_and(|p| Self::bound_primary_identity(v, &p) == wanted)))
        })
    }

    pub(super) fn step_layout_tab(&mut self, delta: isize) {
        let tabs = self.layout_tabs.tabs();
        let Some(index) = tabs.iter().position(|tab| Some(tab.id) == self.layout_tabs.active_id()) else { return };
        if tabs.is_empty() { return; }
        let next = (index as isize + delta).rem_euclid(tabs.len() as isize) as usize;
        self.switch_layout_tab(tabs[next].id);
    }

    pub(super) fn layout_strip_action(&mut self, _ctx: &egui::Context, action: layout_tabs::TabAction) {
        if self.dirty_prompt.is_some() { return; }
        match action {
            layout_tabs::TabAction::New => { self.new_layout_tab(); }
            layout_tabs::TabAction::Activate(id) => self.switch_layout_tab(id),
            layout_tabs::TabAction::Close(id) => self.close_layout_tab(id),
            layout_tabs::TabAction::Rename { id, name } => {
                let Some(before) = self.layout_tabs.tab(id).map(|tab| tab.name.clone()) else { return };
                if self.layout_tabs.rename(id, &name) == Ok(true) && !self.no_persist {
                    self.observe(true);
                    self.history.push(vec![undo::Part::TabRename { id, before, after: name }]);
                }
            }
            layout_tabs::TabAction::Move { id, to_index } => {
                let Some(before) = self.layout_tabs.tabs().iter().position(|tab| tab.id == id) else { return };
                if self.layout_tabs.move_tab(id, to_index) && !self.no_persist {
                    let after = self.layout_tabs.tabs().iter().position(|tab| tab.id == id).unwrap_or(before);
                    self.observe(true);
                    self.history.push(vec![undo::Part::TabMove { id, before, after }]);
                }
            }
        }
    }

    pub(super) fn reopen_layout_item(&mut self) { self.reopen_layout_item_with_context(&egui::Context::default()); }

    pub(super) fn reopen_layout_item_with_context(&mut self, ctx: &egui::Context) {
        if matches!(self.layout_tabs.last_closed(), Some(ClosedItem::Batch { .. })) { self.reopen_editor_batch(ctx); return; }
        enum Item { Tab(TabId), View(TabId, HashSet<PathBuf>) }
        let Some(item) = self.layout_tabs.last_closed().map(|item| match item {
            ClosedItem::Tab(closed) => Item::Tab(closed.tab.id),
            ClosedItem::View { tab_id, view, .. } => {
                let mut files = self.open_file_identities(None);
                Self::add_view_file_identities(&view.view, &mut files);
                Item::View(*tab_id, files)
            },
            ClosedItem::Batch { .. } => unreachable!("batch dispatched above"),
        }) else { return };
        match item {
            Item::Tab(id) => {
                let before_active = self.layout_tabs.active_id();
                self.observe(true);
                if self.restore_closed_tab_internal(id) {
                    self.observe(false);
                    if !self.no_persist {
                        if let Some(snapshot) = self.tab_undo_snapshot(id) {
                            self.history.push(vec![undo::Part::TabLifecycle {
                                id, before_open: false, after_open: true,
                                before_active, after_active: self.layout_tabs.active_id(), snapshot,
                            }]);
                        }
                    }
                }
            }
            Item::View(id, files) => {
                if files.len() > MAX_OPEN_FILES { self.notice_open_file_limit(); return; }
                if self.layout_tabs.tab(id).is_none() {
                    self.notice = Some("먼저 이 화면이 속한 탭을 다시 열어 주세요.".into());
                    return;
                }
                // Reopening a hidden tab's view is one action, including the tab switch.
                // Move internally so the switch cannot create a separate undo entry.
                let before_active = self.layout_tabs.active_id();
                self.finish_layout_playback();
                if before_active != Some(id) {
                    self.switch_layout_tab_internal(id);
                    self.observe(false);
                }
                let Some(slot) = dock::free_slot(&self.dock).map(usize::from) else {
                    self.notice = Some("한 탭에는 3D 화면을 네 개까지 열 수 있어요.".into());
                    if let Some(before) = before_active { self.switch_layout_tab_internal(before); self.observe(false); }
                    return;
                };
                self.observe(true);
                let pushed = self.history.pushed;
                let Some(ClosedItem::View { tab_id, index, view: mut closed }) = self.layout_tabs.pop_closed() else {
                    if let Some(before) = before_active { self.switch_layout_tab_internal(before); self.observe(false); }
                    return;
                };
                if closed.marker_ui.has_pending_work() || !self.reconcile_retained_view(&mut closed.view) {
                    if self.notice.is_none() { self.notice = Some("닫힌 화면의 편집이나 저장이 아직 끝나지 않았어요.".into()); }
                    let evicted = self.layout_tabs.remember_closed(ClosedItem::View { tab_id, index, view: closed });
                    drop_later(evicted);
                    if let Some(before) = before_active { self.switch_layout_tab_internal(before); self.observe(false); }
                    return;
                }
                let use_slot = if !self.open_views().contains(&closed.slot) { closed.slot } else { slot };
                closed.view.sync_shared();
                self.views[use_slot] = closed.view;
                self.marker_ui[use_slot] = closed.marker_ui;
                self.handoff_closed_file_work(use_slot);
                if let Some(path) = self.views[use_slot].waiting_shared_path() { self.pending_shared_promotions.push(path.to_path_buf()); }
                fn slots(layout: &EditorLayout, out: &mut Vec<u8>) {
                    match layout {
                        EditorLayout::Empty => {},
                        EditorLayout::Leaf { slots, .. } => out.extend(slots.iter().copied()),
                        EditorLayout::Horizontal { left, right, .. } => { slots(left, out); slots(right, out); },
                        EditorLayout::Vertical { top, bottom, .. } => { slots(top, out); slots(bottom, out); },
                    }
                }
                let mut wanted = Vec::new();
                slots(&closed.layout, &mut wanted);
                wanted.sort_unstable();
                let mut live: Vec<u8> = self.open_views().into_iter().map(|k| k as u8).chain([use_slot as u8]).collect();
                live.sort_unstable();
                if use_slot == closed.slot && wanted == live {
                    dock::set_editor_layout(&mut self.dock, &closed.layout);
                } else if self.active_layout_is_empty() {
                    dock::set_editor_layout(&mut self.dock, &EditorLayout::Leaf { slots: vec![use_slot as u8], active: use_slot as u8 });
                } else {
                    dock::add_view(&mut self.dock, use_slot as u8, self.dock_area);
                }
                self.activate(use_slot);
                self.observe(true);
                if !self.no_persist && self.history.pushed > pushed {
                    if let (Some(before), Some(action)) = (before_active.filter(|&before| before != id), self.history.undo.last_mut()) {
                        // Layout first: undo restores it in its owner tab, then returns to the
                        // original active tab. Redo activates the owner before replaying it.
                        if action.iter().any(|part| matches!(part, undo::Part::Layout { .. })) {
                            action.push(undo::Part::TabSwitch { before, after: id });
                        }
                    }
                }
            }
        }
    }

    fn active_layout_is_empty(&self) -> bool {
        matches!(dock::editor_layout(&self.dock), EditorLayout::Empty)
    }
}
