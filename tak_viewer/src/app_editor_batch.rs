//! Atomic OPEN EDITORS close: collect occurrence references and every last-document decision
//! before discarding or closing anything. Normal close paths retain worker ownership.
use super::*;
use crate::ui::open_editors::EditorRowId;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Occurrence { pub tab: u64, pub slot: usize, pub overlay: bool }

impl Occurrence {
    fn of(row: &EditorRowId) -> Self { Self { tab: row.tab_id, slot: row.slot, overlay: row.overlay } }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ClosePlan { rows: Vec<EditorRowId>, views: BTreeSet<(u64, usize)>, overlays: BTreeSet<(u64, usize)> }

impl ClosePlan {
    fn gather(selected: &BTreeSet<EditorRowId>, displayed: &[EditorRowId]) -> Self {
        let rows: Vec<_> = displayed.iter().filter(|row| selected.contains(*row)).cloned().collect();
        let views: BTreeSet<_> = rows.iter().filter(|row| !row.overlay).map(|row| (row.tab_id, row.slot)).collect();
        let overlays = rows.iter().filter(|row| row.overlay && !views.contains(&(row.tab_id, row.slot))).map(|row| (row.tab_id, row.slot)).collect();
        Self { rows, views, overlays }
    }
    fn removes(&self, occurrence: Occurrence) -> bool {
        self.views.contains(&(occurrence.tab, occurrence.slot))
            || occurrence.overlay && self.overlays.contains(&(occurrence.tab, occurrence.slot))
    }
    fn valid(&self, displayed: &[EditorRowId]) -> bool { self.rows.iter().all(|row| displayed.contains(row)) }
    fn rebind(&mut self, old: &Path, new: &Path) { for row in &mut self.rows { if row.path == old { row.path = new.into(); } } }
    fn tabs(&self) -> BTreeSet<u64> { self.rows.iter().map(|row| row.tab_id).collect() }
}

struct PendingBatch {
    plan: ClosePlan,
    origin: Option<u64>,
    prompts: Vec<Occurrence>,
    next: usize,
    current: Option<Occurrence>,
    discards: BTreeSet<Occurrence>,
    replay: Option<bool>,
}

#[derive(Default)]
pub(super) struct EditorBatchState {
    pending: Option<PendingBatch>,
    pub(super) archives: HashMap<u64, Arc<undo::EditorBatchArchive>>,
    next_id: u64,
}

impl EditorBatchState {
    pub(super) fn rebind(&mut self, old: &Path, new: &Path) {
        if let Some(pending) = &mut self.pending { pending.plan.rebind(old, new); }
        for archive in self.archives.values_mut() { Arc::make_mut(archive).rebind(old, new); }
    }
}

impl TakViewerApp {
    fn batch_owner(&self, occurrence: Occurrence) -> Option<&View> {
        let views = if self.layout_tabs.active_id() == Some(occurrence.tab) { &self.views }
            else { &self.layout_tabs.tab(occurrence.tab)?.layout.as_ref()?.views };
        let view = views.get(occurrence.slot)?;
        if occurrence.overlay { view.overlay_owner() } else { Some(view) }
    }

    fn batch_owner_busy(&self, occurrence: Occurrence) -> bool {
        let Some(view) = self.batch_owner(occurrence) else { return false };
        if view.comments.pending > 0 { return true; }
        if occurrence.overlay {
            view.document_id().and_then(|id| self.overlay_marker_ui.get(&id)).is_some_and(marker_edit::MarkerUi::has_pending_work)
        } else if self.layout_tabs.active_id() == Some(occurrence.tab) {
            self.marker_ui[occurrence.slot].has_pending_work()
        } else {
            self.layout_tabs.tab(occurrence.tab).and_then(|tab| tab.layout.as_ref())
                .is_some_and(|runtime| runtime.marker_ui[occurrence.slot].has_pending_work())
        }
    }

    fn batch_discards_owner(&self, owner: Occurrence, discards: &BTreeSet<Occurrence>) -> bool {
        if discards.contains(&owner) { return true; }
        let document = self.batch_owner(owner).and_then(View::document_id);
        document.is_some() && discards.iter().any(|&discard| self.batch_owner(discard).and_then(View::document_id) == document)
    }

    fn batch_switch(&mut self, tab: u64) -> bool {
        if self.layout_tabs.active_id() == Some(tab) { return true; }
        if !self.switch_layout_tab_internal(tab) { return false; }
        self.observe(false);
        true
    }

    pub(super) fn close_selected_editors(&mut self, ctx: &egui::Context) {
        self.begin_editor_batch(ctx, self.editor_selection.selected.clone(), None);
    }

    pub(super) fn begin_editor_batch(&mut self, ctx: &egui::Context, selected: BTreeSet<EditorRowId>, replay: Option<bool>) {
        if self.dirty_prompt.is_some() || self.editor_batch.pending.is_some() { return; }
        let displayed = self.editor_rows();
        let plan = ClosePlan::gather(&selected, &displayed);
        if plan.rows.is_empty() { return; }
        self.observe(true);
        let refs: Vec<_> = displayed.iter().map(Occurrence::of).collect();
        let mut groups = HashMap::<tak_core::edit::DocumentId, Vec<Occurrence>>::new();
        let mut fallback = BTreeMap::<PathBuf, Vec<Occurrence>>::new();
        for row in &displayed {
            let occurrence = Occurrence::of(row);
            if !plan.removes(occurrence) { continue; }
            if let Some(document) = self.batch_owner(occurrence).and_then(View::document_id) {
                groups.entry(document).or_default().push(occurrence);
            } else { fallback.entry(self.known_identity(&row.path)).or_default().push(occurrence); }
        }
        // A failed/pending Comment writer still belongs to its primary occurrence. Main's
        // single-view close refuses to abandon it while an alias survives. Refuse the whole
        // gesture now, before prompts or any clean row closes; marker-dirty aliases may close.
        for row in displayed.iter().filter(|row| !row.overlay && plan.removes(Occurrence::of(row))) {
            let owner = Occurrence::of(row);
            let Some(view) = self.batch_owner(owner) else { continue };
            if view.comments.pending == 0 && view.comments.failed.is_none() { continue; }
            let document = view.document_id();
            let identity = self.known_identity(&row.path);
            let survives = displayed.iter().any(|other| {
                let occurrence = Occurrence::of(other);
                !plan.removes(occurrence) && if let Some(document) = document {
                    self.batch_owner(occurrence).is_some_and(|view| view.document_id() == Some(document))
                } else { self.known_identity(&other.path) == identity }
            });
            if survives {
                self.notice = Some("Comment 저장이 대기 중이거나 실패한 파일이 있어 파일 묶음을 닫지 않았어요. Comment 저장을 마친 뒤 다시 닫아 주세요.".into());
                return;
            }
        }
        let mut prompts = Vec::new();
        for (document, mut removed) in groups {
            let remains = refs.iter().any(|&other| !plan.removes(other)
                && self.batch_owner(other).is_some_and(|view| view.document_id() == Some(document)));
            if remains { continue; }
            // The prompt owns the actual running command, including a hidden-tab owner.
            removed.sort_by_key(|&owner| (!self.batch_owner_busy(owner), owner.overlay, owner));
            prompts.push(removed[0]);
        }
        for (path, mut removed) in fallback {
            if displayed.iter().any(|row| !plan.removes(Occurrence::of(row)) && self.known_identity(&row.path) == path) { continue; }
            removed.sort_by_key(|&owner| (!self.batch_owner_busy(owner), owner.overlay, owner));
            prompts.push(removed[0]);
        }
        prompts.sort_by_key(|owner| displayed.iter().position(|row| Occurrence::of(row) == *owner).unwrap_or(usize::MAX));
        self.editor_batch.pending = Some(PendingBatch { plan, origin: self.layout_tabs.active_id(), prompts, next: 0, current: None, discards: BTreeSet::new(), replay });
        self.advance_editor_batch(ctx);
    }

    pub(super) fn advance_editor_batch(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.editor_batch.pending.take() else { return };
        if !pending.plan.valid(&self.editor_rows()) {
            self.notice = Some("열린 파일 목록이 바뀌어 묶음 닫기를 취소했어요.".into());
            if let Some(tab) = pending.origin { self.batch_switch(tab); }
            return;
        }
        pending.current = None;
        while let Some(&owner) = pending.prompts.get(pending.next) {
            pending.next += 1;
            if !self.batch_switch(owner.tab) { return; }
            pending.current = Some(owner);
            self.editor_batch.pending = Some(pending);
            let guarded = if owner.overlay { self.guard_overlay_dirty(owner.slot, marker_edit::PendingTransition::EditorBatch) }
                else { self.guard_primary_dirty(owner.slot, marker_edit::PendingTransition::EditorBatch) };
            if guarded { return; }
            pending = self.editor_batch.pending.take().expect("unguarded batch retained");
            pending.current = None;
        }
        // Saved documents may have changed again while another prompt was answered.
        for &owner in &pending.prompts {
            if pending.discards.contains(&owner) { continue; }
            if self.batch_owner(owner).is_some_and(|view| view.marker_dirty() || view.comments.pending > 0 || view.comments.failed.is_some()) || self.batch_owner_busy(owner) {
                self.notice = Some("편집이나 저장이 끝나지 않아 파일 묶음을 닫지 않았어요.".into());
                if let Some(tab) = pending.origin { self.batch_switch(tab); }
                return;
            }
        }
        // No partial close if a surviving alias cannot yet accept the removed worker.
        let rows = self.editor_rows();
        for row in &rows {
            let owner = Occurrence::of(row);
            if pending.plan.removes(owner) && self.batch_owner_busy(owner) && !self.batch_discards_owner(owner, &pending.discards) {
                self.notice = Some("읽기나 저장을 마친 뒤 파일 묶음을 닫아 주세요.".into());
                if let Some(tab) = pending.origin { self.batch_switch(tab); }
                return;
            }
        }
        for &owner in &pending.discards {
            if !self.batch_switch(owner.tab) { return; }
            let discarded = if owner.overlay { self.with_overlay_owner(ctx, owner.slot, |app| app.discard_editor_work(ctx, owner.slot)).unwrap_or(false) }
                else { self.discard_editor_work(ctx, owner.slot) };
            if !discarded { return; }
        }
        let tabs = pending.plan.tabs();
        let before = self.editor_batch_snapshot(&tabs);
        let selected_before = self.editor_selection.clone();
        let old_recent = self.layout_tabs.take_recent_items();
        let saved_undo = std::mem::take(&mut self.history.undo);
        let saved_redo = std::mem::take(&mut self.history.redo);
        let saved_pushed = self.history.pushed;
        // Keep all ordinary close guards and worker handoffs enabled. Preflight above has
        // already resolved every document's decision; the final view closes its whole tab.
        for &(tab, slot) in &pending.plan.overlays {
            if self.batch_switch(tab) { self.overlay_off(slot); self.observe(true); }
        }
        for &(tab, slot) in &pending.plan.views {
            if self.batch_switch(tab) { self.close_view(slot); self.observe(true); }
        }
        let generated = self.layout_tabs.take_recent_items().into_iter().collect::<Vec<_>>();
        self.layout_tabs.put_recent_items(old_recent);
        drop_later(Some(std::mem::replace(&mut self.history.undo, saved_undo)));
        drop_later(Some(std::mem::replace(&mut self.history.redo, saved_redo)));
        self.history.pushed = saved_pushed;
        self.history.merge_open = false;
        self.editor_selection.clear();
        let after = self.editor_batch_snapshot(&tabs);
        let after_active = self.layout_tabs.active_id();
        let replay_id = pending.replay.and_then(|redo| self.editor_batch_replay_id(redo));
        let id = replay_id.unwrap_or_else(|| { self.editor_batch.next_id += 1; self.editor_batch.next_id });
        let archive = Arc::new(undo::EditorBatchArchive::new(id, before, after, selected_before, pending.origin, after_active, pending.plan.rows));
        self.editor_batch.archives.insert(id, archive.clone());
        drop_later(self.layout_tabs.remember_closed(layout_tabs::ClosedItem::Batch { id, items: generated }));
        if let Some(redo) = pending.replay { self.finish_editor_batch_replay(redo); }
        else if !self.no_persist { self.push_editor_batch_history(archive, true); }
        self.prune_editor_batch_archives();
        self.observe(false);
    }

    /// Return true only when this dialog belongs to the current batch. Overlay Discard is
    /// resumed after with_overlay_owner restores the real host; never mutate a swapped host.
    pub(super) fn defer_editor_batch_discard(&mut self, ctx: &egui::Context, slot: usize) -> bool {
        let Some(pending) = &mut self.editor_batch.pending else { return false };
        let Some(owner) = pending.current.filter(|owner| owner.slot == slot) else { return false };
        pending.discards.insert(owner);
        self.dirty_prompt = None;
        if self.overlay_owner_scope { self.finish_overlay_dirty_bridge(ctx); }
        else { self.advance_editor_batch(ctx); }
        true
    }

    pub(super) fn cancel_editor_batch(&mut self) {
        if let Some(pending) = self.editor_batch.pending.take() {
            if self.overlay_owner_scope {
                if let Some(owner) = pending.current { self.tab_close_origin = Some((owner.tab, pending.origin)); }
            } else if let Some(tab) = pending.origin { self.batch_switch(tab); }
        }
    }

    pub(super) fn reopen_editor_batch(&mut self, ctx: &egui::Context) {
        if self.dirty_prompt.is_some() || self.editor_batch.pending.is_some() { return; }
        let Some(layout_tabs::ClosedItem::Batch { id, .. }) = self.layout_tabs.last_closed() else { return };
        let Some(archive) = self.editor_batch.archives.get(id).cloned() else { return };
        self.restore_editor_batch(ctx, &archive, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(tab: u64, slot: usize, overlay: bool, path: &str) -> EditorRowId { EditorRowId { tab_id: tab, slot, overlay, path: path.into() } }
    #[test]
    fn close_plan_deduplicates_per_view_without_collapsing_hidden_tab_occurrences() {
        let displayed = vec![row(1, 0, false, "a.tak"), row(1, 0, true, "b.tak"), row(2, 0, false, "a.tak"), row(2, 0, true, "b.tak")];
        let plan = ClosePlan::gather(&displayed.iter().cloned().collect(), &displayed);
        assert_eq!(plan.views, BTreeSet::from([(1, 0), (2, 0)]));
        assert!(plan.overlays.is_empty());
        let plan = ClosePlan::gather(&BTreeSet::from([displayed[0].clone(), displayed[3].clone()]), &displayed);
        assert_eq!(plan.views, BTreeSet::from([(1, 0)]));
        assert_eq!(plan.overlays, BTreeSet::from([(2, 0)]));
        assert!(plan.removes(Occurrence { tab: 1, slot: 0, overlay: true }));
        assert!(!plan.removes(Occurrence { tab: 2, slot: 0, overlay: false }));
    }
}
