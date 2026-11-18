//! Comment list gestures and one-job/one-history batch writes.
use super::*;

pub(super) struct PendingBatch {
    pub tag: usize,
    pub tokens: Vec<String>,
    pub before: Vec<(u64, Option<Comment>)>,
    pub selection: views::Selection,
}

impl TakViewerApp {
    pub(super) fn set_comment_sort(&mut self, sort: comment_list::Sort) {
        self.comment_sort = sort;
        let hidden = self.layout_tabs.tabs_mut().iter_mut().filter_map(|tab| tab.layout.as_mut()).flat_map(|runtime| runtime.views.iter_mut());
        for host in self.views.iter_mut().chain(hidden) {
            let update = |view: &mut View| {
                let list = view.comments.list.list();
                let order: Vec<_> = comment_list::sorted(list, sort).into_iter().map(|i| list[i].id).collect();
                view.comment_selection.set_order(&order);
            };
            update(host);
            if let Some(owner) = host.overlay_owner_mut() { update(owner); }
        }
    }

    fn comment_ids_in_order(&self, slot: usize) -> Vec<u64> {
        let list = self.views[slot].comments.list.list();
        comment_list::sorted(list, self.comment_sort).into_iter().map(|i| list[i].id).collect()
    }

    pub(super) fn comment_gesture(&mut self, slot: usize, id: u64, ctrl: bool, shift: bool) {
        let order = self.comment_ids_in_order(slot);
        let v = &mut self.views[slot];
        v.comment_selection.click(id, ctrl, shift, &order);
        self.follow_comment_representative(slot);
    }

    fn follow_comment_representative(&mut self, slot: usize) {
        let v = &mut self.views[slot];
        let state = v.comment_selection.clone();
        v.picked = state.representative;
        if let Some(id) = v.picked { v.select_comment(id); }
        v.comment_selection = state;
    }

    pub(super) fn comment_list_keys(&mut self, ctx: &egui::Context) {
        if key_focus::target(ctx) != key_focus::Target::Comments || key_focus::keys_blocked(ctx) { return; }
        let slot = self.active;
        if exact_key(ctx, Key::A, true, false, false) {
            let ids = self.comment_ids_in_order(slot);
            self.views[slot].comment_selection.all(&ids);
            self.follow_comment_representative(slot);
        }
        if exact_key(ctx, Key::Escape, false, false, false) {
            self.views[slot].clear_selection();
        }
        if exact_key(ctx, Key::Delete, false, false, false) {
            let ids: Vec<_> = self.views[slot].comment_selection.ids().collect();
            self.delete_comments(ctx, slot, &ids, false);
        }
    }

    pub(super) fn delete_comments(&mut self, ctx: &egui::Context, slot: usize, ids: &[u64], single: bool) {
        if ids.is_empty() { return; }
        if let Err(reason) = self.comment_writable(slot) {
            self.takm_note = Some((reason, ctx.input(|i| i.time)));
            return;
        }
        self.cancel_comment_edit();
        self.observe(true);
        let items: Vec<_> = self.views[slot].comments.list.list().iter().filter(|c| ids.contains(&c.id)).cloned().collect();
        if items.is_empty() || !self.write_comment_batch(ctx, slot, EventKind::Delete, &items) { return; }
        let order = self.comment_ids_in_order(slot);
        if single {
            for id in ids { self.views[slot].comment_selection.remove(*id, &order); }
            self.follow_comment_representative(slot);
        } else {
            self.views[slot].comment_selection.clear();
            self.views[slot].picked = None;
        }
        let count = items.len();
        let label = if count == 1 { format!("Comment {} deleted · Ctrl+Z", items[0].id) } else { format!("{count} Comments deleted · Ctrl+Z") };
        self.push_comment(slot, if count == 1 && single { Change::Deleted(items[0].clone()) } else { Change::DeletedMany(items) }, None);
        self.history.merge_open = false;
        self.history.note = Some((label, ctx.input(|i| i.time)));
    }

    pub(super) fn write_comment_batch(&mut self, ctx: &egui::Context, slot: usize, kind: EventKind, items: &[Comment]) -> bool {
        if self.dirty_prompt.is_some() { return false; }
        self.views[slot].sync_shared();
        if items.is_empty() || self.comment_writable(slot).is_err() { return false; }
        let v = &self.views[slot];
        if kind != EventKind::Add && items.iter().any(|c| v.comments.list.id_is_ambiguous(c.id)) {
            self.notice = Some("Comment 번호가 겹쳐서 안전하게 지울 수 없어요.".into()); return false;
        }
        let Some((path, file_a)) = self.comment_target(slot) else { return false; };
        let before = items.iter().map(|c| (c.id, v.comments.list.get(c.id).cloned())).collect();
        let selection = v.selection();
        let tag = v.file_id;
        let mut events = Vec::with_capacity(items.len());
        for c in items {
            let mut event = Event::of(kind, c, capture::local_iso_time(), file_a.clone());
            if kind != EventKind::Add { event.expected = v.comments.list.get(c.id).cloned(); }
            if let Some(o) = &v.overlay {
                event.shown_a = if v.shown[0] { "1" } else { "0" }.into();
                event.shown_b = if v.shown[1] { "1" } else { "0" }.into();
                event.file_b = comments::file_label(&o.path, o.data.take.file_size);
            }
            events.push(event);
        }
        self.comment_batches.push(PendingBatch { tag, tokens: events.iter().filter_map(|e| e.operation.clone()).collect(), before, selection });
        for event in &events { self.views[slot].comments.list.apply(event); }
        self.send_comment_events(ctx, slot, path, events);
        self.views[slot].publish_shared_file();
        true
    }

    fn send_comment_events(&mut self, ctx: &egui::Context, slot: usize, path: PathBuf, events: Vec<Event>) {
        if self.views[slot].remote.is_some() { self.queue_remote_comments(ctx, slot, events); return; }
        let tag = self.views[slot].file_id;
        self.ensure_comment_writer(ctx);
        if let Some(reason) = self.comment_registration_error.clone() {
            self.views[slot].comments.failed = Some(reason);
            self.comment_failed.extend(events.into_iter().map(|event| (tag, path.clone(), event)));
        } else {
            let sent = self.comment_registration.is_none() && self.comment_writer.as_ref().is_some_and(|w| w.send_batch(tag, path.clone(), events.clone()));
            if sent || self.comment_registration.is_some() {
                let count = events.len();
                self.views[slot].comments.pending_since.get_or_insert_with(Instant::now);
                self.views[slot].comments.pending += count;
                self.comment_outstanding += count;
                if !sent { self.comment_waiting.push((tag, path, events)); }
                ctx.request_repaint_after(Duration::from_millis(30));
            } else {
                self.views[slot].comments.failed = Some("저장 작업이 멈췄어요.".into());
                self.comment_failed.extend(events.into_iter().map(|event| (tag, path.clone(), event)));
            }
        }
    }

    pub(super) fn retry_comment_events(&mut self, ctx: &egui::Context, slot: usize, retry: Vec<(usize, PathBuf, Event)>) {
        let mut queue = retry.into_iter().peekable();
        while let Some((_, path, first)) = queue.next() {
            let group = first.operation.as_ref().and_then(|token| self.comment_batches.iter().find(|b| b.tokens.contains(token))).map(|b| b.tokens.clone());
            let mut events = vec![first];
            if let Some(tokens) = group {
                while queue.peek().is_some_and(|(_, p, e)| *p == path && e.operation.as_ref().is_some_and(|token| tokens.contains(token))) {
                    events.push(queue.next().unwrap().2);
                }
            }
            self.send_comment_events(ctx, slot, path, events);
        }
    }

    pub(super) fn finish_comment_batch_event(&mut self, done: &comments::Done) {
        let Some(token) = &done.event.operation else { return; };
        let Some(index) = self.comment_batches.iter().position(|b| b.tag == done.tag && b.tokens.contains(token)) else { return; };
        if done.conflict.is_some() {
            let batch = self.comment_batches.remove(index);
            let hidden = self.layout_tabs.tabs_mut().iter_mut().filter_map(|tab| tab.layout.as_mut()).flat_map(|runtime| runtime.views.iter_mut());
            for host in self.views.iter_mut().chain(hidden) {
                let v = if host.file_id == batch.tag { host } else if let Some(owner) = host.overlay_owner_mut().filter(|owner| owner.file_id == batch.tag) { owner } else { continue };
                v.sync_shared();
                if let Some(latest) = &done.batch_latest {
                    for (id, _) in &batch.before { v.comments.list.reload_one(*id, latest.get(*id).cloned()); }
                    v.set_selection(batch.selection.clone());
                } else if batch.before.len() == 1 {
                    for (id, before) in &batch.before { v.comments.list.reload_one(*id, before.clone()); }
                    v.set_selection(batch.selection.clone());
                } else {
                    v.comments.failed = Some("다른 창의 Comment와 달라졌고 파일을 다시 읽지 못했어요. 파일을 다시 열어 주세요.".into());
                }
                let order: Vec<_> = comment_list::sorted(v.comments.list.list(), self.comment_sort).into_iter().map(|i| v.comments.list.list()[i].id).collect();
                let gone: Vec<_> = v.comment_selection.ids().filter(|id| v.comments.list.get(*id).is_none()).collect();
                for id in gone { v.comment_selection.remove(id, &order); }
                v.picked = v.comment_selection.representative;
                v.publish_shared_file();
            }
            for (id, _) in batch.before { self.history.forget_comment(&done.path, id); }
        } else if done.result.is_ok() || done.cancelled {
            self.comment_batches[index].tokens.retain(|t| t != token);
            if self.comment_batches[index].tokens.is_empty() { self.comment_batches.remove(index); }
        }
    }
}
