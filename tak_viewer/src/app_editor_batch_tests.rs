// Included in app::tests to exercise the existing real frame/load/undo fixtures.

#[test]
fn editor_batch_close_is_one_action_and_reopens_as_one_recent_item() {
    let ctx = egui::Context::default();
    let _dir_guard = comment_dir("editor_batch_close");
    let dir = _dir_guard.to_path_buf();
    let (mut app, files) = editors_app(&ctx, &dir);
    app.views[0].pb.set_frame(6, 0.0);
    app.views[0].select(1, false);
    app.views[0].range = Some((2, 7));
    app.views[0].labels.list_selection[0].anchor = Some([2; 16]);
    let anchors = app.views[0].labels.list_selection.clone();
    app.views[0].shown = [false, true];
    app.select_all_editors();
    let selection = app.editor_selection.clone();
    let layout = dock::shape(&app.dock);
    let before = steps(&app, 0).0;
    app.close_selected_editors(&ctx);
    assert!(app.open_views().is_empty());
    assert_eq!(steps(&app, 0).0, before + 1, "one close action including the list selection");
    undo_key(&ctx, &mut app);
    arrive_all(&ctx, &mut app);
    if app.views[0].overlay_load.is_some() { overlay_arrive(&ctx, &mut app, 0); }
    assert_eq!(app.open_views().len(), 2);
    assert_eq!(app.views[0].pb.frame, 6);
    assert_eq!(app.views[0].primary, Some(1));
    assert_eq!(app.views[0].range, Some((2, 7)));
    assert_eq!(app.views[0].labels.list_selection, anchors);
    assert_eq!(app.views[0].overlay_target(), Some(files[1].as_path()));
    assert_eq!(app.views[0].shown, [false, true]);
    assert_eq!(app.editor_selection, selection);
    assert_eq!(dock::shape(&app.dock), layout);
    redo_key(&ctx, &mut app);
    assert!(app.open_views().is_empty());
    run_frame(&ctx, &mut app, &[(Modifiers { ctrl: true, shift: true, ..Modifiers::NONE }, Key::T)]);
    arrive_all(&ctx, &mut app);
    if app.views[0].overlay_load.is_some() { overlay_arrive(&ctx, &mut app, 0); }
    assert_eq!(app.open_views().len(), 2);
    assert_eq!(app.editor_selection, selection);
    assert_eq!(app.views[0].labels.list_selection, anchors);
    let reopened = steps(&app, 0).0;
    app.reopen_editor_batch(&ctx);
    assert_eq!(steps(&app, 0).0, reopened, "the whole batch occupied only one recent entry");
}

#[test]
fn editor_batch_overlay_only_keeps_both_base_views_and_restores_with_one_undo() {
    use crate::ui::open_editors::EditorAction;
    let ctx = egui::Context::default();
    let _dir_guard = comment_dir("editor_batch_overlay");
    let dir = _dir_guard.to_path_buf();
    let (mut app, files) = editors_app(&ctx, &dir);
    let id = app.editor_rows().into_iter().find(|id| id.overlay).unwrap();
    let shown = app.views[0].shown;
    app.editor_action(&ctx, EditorAction::Select { id: id.clone(), ctrl: true, shift: false });
    assert_eq!(app.views[0].shown, shown, "modified selection must not toggle visibility");
    let before = steps(&app, 0).0;
    app.close_selected_editors(&ctx);
    assert_eq!(app.open_views().len(), 2);
    assert!(app.views[0].overlay_target().is_none());
    assert_eq!(steps(&app, 0).0, before + 1);
    undo_key(&ctx, &mut app);
    if app.views[0].overlay_load.is_some() { overlay_arrive(&ctx, &mut app, 0); }
    assert_eq!(app.views[0].overlay_target(), Some(files[1].as_path()));
    assert!(app.editor_selection.selected.contains(&id));
}

#[test]
fn editor_batch_discard_then_cancel_preserves_all_views_and_both_pending_documents() {
    let ctx = egui::Context::default();
    let _dir_guard = comment_dir("editor_batch_cancel");
    let dir = _dir_guard.to_path_buf();
    let (mut app, _) = editors_app(&ctx, &dir);
    for slot in [0, 1] { app.views[slot].comments.failed = Some(format!("unsaved-{slot}")); }
    app.select_all_editors();
    let before = steps(&app, 0).0;
    app.close_selected_editors(&ctx);
    assert!(app.dirty_prompt.is_some());
    app.discard_pending_transition(&ctx);
    assert!(app.dirty_prompt.is_some(), "second document must still ask");
    assert_eq!(app.views[0].comments.failed.as_deref(), Some("unsaved-0"), "first Discard is only staged");
    frame_with(&ctx, &mut app, vec![]);
    let out = frame_with(&ctx, &mut app, vec![]);
    let cancel = text_rects(&out, "Cancel")[0].center();
    click_at(&ctx, &mut app, cancel);
    assert!(app.dirty_prompt.is_none());
    assert_eq!(app.open_views().len(), 2);
    assert_eq!(app.views[0].comments.failed.as_deref(), Some("unsaved-0"));
    assert_eq!(app.views[1].comments.failed.as_deref(), Some("unsaved-1"));
    assert_eq!(steps(&app, 0).0, before, "cancel creates no close action");
    // A new request must ask again; a cancelled staged discard is never reused.
    app.close_selected_editors(&ctx);
    app.discard_pending_transition(&ctx);
    assert!(app.dirty_prompt.is_some());
    app.discard_pending_transition(&ctx);
    assert!(app.open_views().is_empty());
    assert_eq!(steps(&app, 0).0, before + 1);
    undo_key(&ctx, &mut app);
    arrive_all(&ctx, &mut app);
    if app.views[0].overlay_load.is_some() { overlay_arrive(&ctx, &mut app, 0); }
    assert!(app.views[0].comments.failed.is_none(), "discarded edits stay discarded after undo");
    assert!(app.views[1].comments.failed.is_none());
}

#[test]
fn editor_batch_reopen_uses_free_slots_and_preserves_a_file_opened_later() {
    let ctx = egui::Context::default();
    let _dir_guard = comment_dir("editor_batch_reopen_later");
    let dir = _dir_guard.to_path_buf();
    let (mut app, _) = editors_app(&ctx, &dir);
    app.select_all_editors();
    app.close_selected_editors(&ctx);
    let later = dir.join("later.tak");
    std::fs::write(&later, b"held test file").unwrap();
    app.open(&ctx, later.clone());
    arrive_all(&ctx, &mut app);
    dock::hide(&mut app.dock, Pane::Labeling);
    assert!(app.dock.find_tab(&Pane::Labeling).is_none());
    app.reopen_editor_batch(&ctx);
    assert!(app.dock.find_tab(&Pane::Labeling).is_none(), "reopening tab layouts preserves current shared side panels");
    arrive_all(&ctx, &mut app);
    let rows = app.editor_rows();
    let overlay = rows.iter().find(|row| row.overlay).expect("the original overlay occurrence is restored");
    assert_eq!(app.layout_tabs.active_id(), Some(overlay.tab_id));
    if app.views[overlay.slot].overlay_load.is_some() { overlay_arrive(&ctx, &mut app, overlay.slot); }
    assert!(app.views[overlay.slot].overlay_target().is_some());
    assert_eq!(rows.len(), 4, "two original base views, their overlay, and the later file");
    assert_eq!(app.open_views().len(), 2, "the original two-view tab is restored separately");
    let later_row = rows.iter().find(|row| row.path == later && !row.overlay).expect("the later file keeps its own tab/view occurrence");
    assert_ne!(later_row.tab_id, overlay.tab_id);
    assert_eq!(app.editor_selection.selected.len(), 3);
}

#[test]
fn editor_batch_hidden_tabs_keep_duplicate_file_occurrences_and_one_recent_entry() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    app.views[0].pb.set_frame(2, 0.0);
    app.views[0].range = Some((1, 3));
    app.views[0].labels.list_selection[0].anchor = Some([1; 16]);
    let second = app.new_layout_tab();
    app.open(&ctx, PathBuf::from("a.tak"));
    app.views[0].pb.set_frame(7, 0.0);
    app.views[0].range = Some((5, 8));
    let document = app.views[0].edit_document.as_ref().unwrap().clone();
    app.observe(false);
    app.select_all_editors();
    assert_eq!(app.editor_selection.selected.len(), 2);
    let selected = app.editor_selection.clone();
    let before = steps(&app, 0).0;
    app.close_selected_editors(&ctx);
    assert!(app.layout_tabs.tabs().is_empty());
    assert_eq!(steps(&app, 0).0, before + 1);
    assert!(matches!(app.layout_tabs.last_closed(), Some(layout_tabs::ClosedItem::Batch { .. })));
    app.undo_redo(&ctx, false);
    assert_eq!(app.layout_tabs.tabs().len(), 2);
    assert_eq!(app.editor_selection, selected);
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    assert_eq!(app.views[0].pb.frame, 7);
    assert_eq!(app.views[0].range, Some((5, 8)));
    assert!(std::sync::Arc::ptr_eq(&document, app.views[0].edit_document.as_ref().unwrap()));
    app.switch_layout_tab_internal(first);
    assert_eq!(app.views[0].pb.frame, 2);
    assert_eq!(app.views[0].range, Some((1, 3)));
    assert_eq!(app.views[0].labels.list_selection[0].anchor, Some([1; 16]));
    app.switch_layout_tab_internal(second);
    app.observe(false);
    app.undo_redo(&ctx, true);
    assert!(app.layout_tabs.tabs().is_empty());
    let recent = app.layout_tabs.closed_len();
    app.reopen_layout_item_with_context(&ctx);
    assert_eq!(app.layout_tabs.tabs().len(), 2);
    assert_eq!(app.editor_selection.selected.len(), 2);
    assert_eq!(app.layout_tabs.closed_len(), recent - 1);
}

#[test]
fn editor_batch_cancel_in_hidden_tab_restores_origin_and_keeps_all_occurrences() {
    let ctx = egui::Context::default();
    let mut app = two_views(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    app.views[0].comments.failed = Some("first unsaved".into());
    app.views[0].publish_shared_file();
    let second = app.new_layout_tab();
    let (_tx, rx) = std::sync::mpsc::channel();
    app.views[0].show(views::sample_data("c.tak", &["M"]), rx, 50, 1.0, 0.0);
    dock::set_editor_layout(&mut app.dock, &dock::EditorLayout::Leaf { slots: vec![0], active: 0 });
    app.views[0].comments.failed = Some("second unsaved".into());
    app.views[0].publish_shared_file();
    app.observe(false);
    app.select_all_editors();
    assert_eq!(app.editor_rows().len(), 3);
    let before = steps(&app, 0).0;
    app.close_selected_editors(&ctx);
    assert_eq!(app.layout_tabs.active_id(), Some(first));
    app.discard_pending_transition(&ctx);
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    assert!(app.dirty_prompt.is_some());
    app.cancel_dirty_transition(&ctx);
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    assert_eq!(app.layout_tabs.tabs().len(), 2);
    assert_eq!(app.editor_rows().len(), 3);
    assert_eq!(steps(&app, 0).0, before);
    assert_eq!(app.views[0].comments.failed.as_deref(), Some("second unsaved"));
    let hidden = app.layout_tabs.tab(first).unwrap().layout.as_ref().unwrap();
    assert_eq!(hidden.views[0].comments.failed.as_deref(), Some("first unsaved"));
}

fn editor_batch_prepare_dirty(app: &mut TakViewerApp, slot: usize) {
    use tak_core::edit::{DocumentInput, RawSample, RawTrack, RecordBytes};
    let tracks = app.views[slot].data.as_ref().unwrap().take.trajectories.iter().map(|track| RawTrack {
        uid: track.uid, name: track.name.clone(), label: None,
        records: RecordBytes { node: std::sync::Arc::from([1]), translation_header: std::sync::Arc::from([2]), serialization_verified: true },
        samples: track.frames.iter().zip(&track.xyz).map(|(&frame, &xyz)| RawSample::new(frame, xyz)).collect::<Vec<_>>().into(),
    }).collect();
    let mut document = app.views[slot].edit_document.as_ref().unwrap().lock().unwrap();
    let revision = document.revision();
    document.install_input(revision, DocumentInput {
        streams: Vec::new().into(), nodes_prefix: std::sync::Arc::from([]), channels_prefix: std::sync::Arc::from([]),
        tracks, labels: Vec::new(), first_frame: 0, last_frame: 9,
    }).unwrap();
    drop(document);
    app.views[slot].select(0, false);
    let snapshot = app.views[slot].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
    let patch = tak_core::edit::delete::delete_samples(&snapshot, &tak_core::edit::delete::DeleteRequest {
        selection: app.views[slot].edit_selection_state(), range: None, current_frame: 0,
    }).unwrap().patch.unwrap();
    app.commit_marker_edit(slot, std::sync::Arc::new(patch)).unwrap();
}

#[test]
fn editor_batch_overlay_final_dirty_reference_prompts_before_unloading() {
    let ctx = egui::Context::default();
    let _dir_guard = comment_dir("editor_batch_dirty_overlay");
    let (mut app, files) = overlay_app(&ctx, &_dir_guard, &["source.tak", "host.tak"]);
    editor_batch_prepare_dirty(&mut app, 0);
    let document = app.views[0].edit_document.as_ref().unwrap().clone();
    let path = app.views[0].shared_target().unwrap();
    let (_tx, rx) = std::sync::mpsc::channel();
    app.views[1].show(views::sample_data(files[1].to_str().unwrap(), &["M", "N"]), rx, 20, 1.0, 0.0);
    dock::set_editor_layout(&mut app.dock, &dock::EditorLayout::Leaf { slots: vec![0, 1], active: 0 });
    app.start_overlay(&ctx, 1, path, false);
    app.poll_overlays(&ctx);
    assert_eq!(app.views[0].document_id(), app.views[1].overlay_document_id());
    app.close_view(0);
    assert!(app.dirty_prompt.is_none());
    app.observe(false);
    let row = app.editor_rows().into_iter().find(|row| row.overlay).unwrap();
    app.editor_selection.selected.insert(row.clone());
    app.close_selected_editors(&ctx);
    assert!(app.dirty_prompt.is_some());
    assert_eq!(app.overlay_prompt_slot(), Some(1));
    assert!(app.views[1].overlay_target().is_some());
    app.cancel_dirty_transition(&ctx);
    assert!(app.views[1].overlay_target().is_some());
    assert!(document.lock().unwrap().is_dirty());
    app.close_selected_editors(&ctx);
    app.with_overlay_owner(&ctx, 1, |app| app.discard_pending_transition(&ctx));
    assert!(app.dirty_prompt.is_none());
    assert!(app.views[1].overlay_target().is_none());
    assert!(!document.lock().unwrap().is_dirty());
    app.undo_redo(&ctx, false);
    assert_eq!(app.views[1].overlay_target(), Some(files[0].as_path()));
    if app.views[1].overlay_load.is_some() { overlay_arrive(&ctx, &mut app, 1); }
    assert_eq!(app.views[1].overlay_target(), Some(files[0].as_path()));
    assert!(!document.lock().unwrap().is_dirty(), "discarded edits stay discarded after undo");
}


#[test]
fn editor_batch_failed_primary_alias_refuses_every_close_before_any_clean_row() {
    let ctx = egui::Context::default();
    let mut app = two_views(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    app.views[0].comments.failed = Some("writer failed".into());
    app.views[0].publish_shared_file();
    let shared_document = app.views[0].document_id();
    let second = app.new_layout_tab();
    app.open(&ctx, PathBuf::from("a.tak"));
    assert_eq!(app.views[0].document_id(), shared_document);
    let rows = app.editor_rows();
    assert_eq!(rows.len(), 3);
    let removed: Vec<_> = rows.iter().filter(|row| row.tab_id == first).cloned().collect();
    assert_eq!(removed.len(), 2, "one clean row and one failed writer alias are selected");
    app.editor_selection.all(&removed);
    app.observe(false);
    let before = steps(&app, 0);
    let recent = app.layout_tabs.closed_len();
    let selection = app.editor_selection.clone();
    app.close_selected_editors(&ctx);
    assert!(app.dirty_prompt.is_none(), "refuse before collecting any document decision");
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    assert_eq!(app.editor_rows(), rows, "no clean row or failed alias may close");
    assert_eq!(app.editor_selection, selection);
    assert_eq!(steps(&app, 0), before, "refused batch adds no close action");
    assert_eq!(app.layout_tabs.closed_len(), recent);
    assert!(app.notice.as_deref().is_some_and(|reason| reason.contains("Comment 저장") && reason.contains("닫지 않았어요")));
    let hidden = app.layout_tabs.tab(first).unwrap().layout.as_ref().unwrap();
    assert_eq!(hidden.views[0].comments.failed.as_deref(), Some("writer failed"));
    assert!(hidden.views[1].target().is_some(), "the selected clean row is still open");
}


#[test]
fn editor_batch_undo_and_recent_reopen_restore_tab_order_with_shared_overlays() {
    let ctx = egui::Context::default();
    let _dir_guard = comment_dir("editor_batch_tab_order_shared_overlays");
    let (mut app, files) = editors_app(&ctx, &_dir_guard);
    let first = app.layout_tabs.active_id().unwrap();
    let shared_overlay_document = app.views[0].overlay_document_id().unwrap();
    let second = app.new_layout_tab();
    app.open(&ctx, files[0].clone());
    assert!(app.views[0].data.is_some(), "the repeated primary shares its loaded document");
    app.start_overlay(&ctx, 0, files[1].clone(), false);
    app.poll_overlays(&ctx);
    assert_eq!(app.views[0].overlay_document_id(), Some(shared_overlay_document));
    let order_before: Vec<_> = app.layout_tabs.tabs().iter().map(|tab| tab.id).collect();
    assert_eq!(order_before, vec![first, second]);
    let rows_before = app.editor_rows();
    assert_eq!(rows_before.len(), 5);
    assert_eq!(rows_before.iter().filter(|row| row.overlay).count(), 2);
    app.select_all_editors();
    let selected_before = app.editor_selection.clone();
    app.close_selected_editors(&ctx);
    assert!(app.layout_tabs.tabs().is_empty());
    app.undo_redo(&ctx, false);
    let order_after_undo: Vec<_> = app.layout_tabs.tabs().iter().map(|tab| tab.id).collect();
    assert_eq!(order_after_undo, order_before, "sequential native close indices must not reverse a batch's tabs");
    assert_eq!(app.editor_rows(), rows_before, "OPEN EDITORS preserves exact tab/view/overlay row order");
    assert_eq!(app.editor_selection, selected_before);
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    assert_eq!(app.views[0].overlay_document_id(), Some(shared_overlay_document));
    let hidden_first = app.layout_tabs.tab(first).unwrap().layout.as_ref().unwrap();
    assert_eq!(hidden_first.views[0].overlay_document_id(), Some(shared_overlay_document));
    app.undo_redo(&ctx, true);
    assert!(app.layout_tabs.tabs().is_empty());
    let recent_before = app.layout_tabs.closed_len();
    app.reopen_layout_item_with_context(&ctx);
    let order_after_reopen: Vec<_> = app.layout_tabs.tabs().iter().map(|tab| tab.id).collect();
    assert_eq!(order_after_reopen, order_before);
    assert_eq!(app.editor_rows(), rows_before);
    assert_eq!(app.editor_selection, selected_before);
    assert_eq!(app.layout_tabs.closed_len(), recent_before - 1, "the ordered tabs reopen as one recent item");
    assert_eq!(app.views[0].overlay_document_id(), Some(shared_overlay_document));
}


#[test]
fn editor_batch_three_custom_ordered_tabs_restore_after_all_tabs_exist() {
    let ctx = egui::Context::default();
    let _dir_guard = comment_dir("editor_batch_three_custom_tabs");
    let (mut app, files) = editors_app(&ctx, &_dir_guard);
    let first = app.layout_tabs.active_id().unwrap();
    let base_document = app.views[0].document_id().unwrap();
    let overlay_document = app.views[0].overlay_document_id().unwrap();
    let second = app.new_layout_tab();
    app.open(&ctx, files[0].clone());
    app.start_overlay(&ctx, 0, files[1].clone(), false);
    app.poll_overlays(&ctx);
    assert_eq!(app.views[0].document_id(), Some(base_document));
    assert_eq!(app.views[0].overlay_document_id(), Some(overlay_document));
    let third = app.new_layout_tab();
    app.open(&ctx, files[0].clone());
    app.start_overlay(&ctx, 0, files[1].clone(), false);
    app.poll_overlays(&ctx);
    assert_eq!(app.views[0].document_id(), Some(base_document));
    assert_eq!(app.views[0].overlay_document_id(), Some(overlay_document));
    app.layout_tabs.move_tab(third, 0);
    app.layout_tabs.move_tab(second, 1);
    app.observe(false);
    let original_order = vec![third, second, first];
    assert_eq!(app.layout_tabs.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(), original_order);
    let original_rows = app.editor_rows();
    assert_eq!(original_rows.len(), 7);
    assert_eq!(original_rows.iter().filter(|row| row.overlay).count(), 3);
    app.select_all_editors();
    let original_selection = app.editor_selection.clone();
    app.close_selected_editors(&ctx);
    assert!(app.layout_tabs.tabs().is_empty());
    app.undo_redo(&ctx, false);
    assert_eq!(app.layout_tabs.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(), original_order,
        "custom strip order is restored after every batch tab exists");
    assert_eq!(app.editor_rows(), original_rows);
    assert_eq!(app.editor_selection, original_selection);
    assert_eq!(app.views[0].document_id(), Some(base_document));
    assert_eq!(app.views[0].overlay_document_id(), Some(overlay_document));
    app.undo_redo(&ctx, true);
    assert!(app.layout_tabs.tabs().is_empty());
    let recent_before = app.layout_tabs.closed_len();
    app.reopen_layout_item_with_context(&ctx);
    assert_eq!(app.layout_tabs.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(), original_order);
    assert_eq!(app.editor_rows(), original_rows);
    assert_eq!(app.editor_selection, original_selection);
    assert_eq!(app.layout_tabs.closed_len(), recent_before - 1);
    assert_eq!(app.views[0].document_id(), Some(base_document));
    assert_eq!(app.views[0].overlay_document_id(), Some(overlay_document));
}

fn editor_batch_reopen_preflight_fixture(ctx: &egui::Context) -> (
    tak_core::test_support::TempDir, TakViewerApp, Vec<PathBuf>, u64, u64, u64,
) {
    let guard = comment_dir("editor_batch_reopen_readiness");
    let (mut app, files) = overlay_app(ctx, &guard, &["ready_a.tak", "wait_b.tak"]);
    let first = app.layout_tabs.active_id().unwrap();
    let second = app.new_layout_tab();
    app.open(ctx, files[1].clone());
    arrive(ctx, &mut app, 0);
    app.observe(false);
    app.select_all_editors();
    app.close_selected_editors(ctx);
    assert!(app.layout_tabs.tabs().is_empty());
    let batch = match app.layout_tabs.last_closed().unwrap() {
        layout_tabs::ClosedItem::Batch { id, .. } => *id,
        _ => panic!("two tab closes must stay one recent batch"),
    };
    (guard, app, files, first, second, batch)
}

#[test]
fn editor_batch_reopen_preflight_keeps_entire_batch_while_later_source_load_is_held() {
    let ctx = egui::Context::default();
    let (_guard, mut app, files, first, second, batch) = editor_batch_reopen_preflight_fixture(&ctx);
    let loading_tab = app.new_layout_tab();
    app.open(&ctx, files[1].clone());
    assert!(app.views[0].loading.is_some());
    assert!(app.views[0].held.is_some(), "the fresh B reader is deliberately held");
    let rows_before = app.editor_rows();
    let history_before = steps(&app, 0);
    let recent_before = app.layout_tabs.closed_len();
    let selected_before = app.editor_selection.clone();
    app.reopen_layout_item_with_context(&ctx);
    assert_eq!(app.layout_tabs.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(), vec![loading_tab],
        "a later source refusal must not restore the ready A tab first");
    assert_eq!(app.editor_rows(), rows_before);
    assert_eq!(app.editor_selection, selected_before);
    assert_eq!(steps(&app, 0), history_before);
    assert_eq!(app.layout_tabs.closed_len(), recent_before);
    assert!(matches!(app.layout_tabs.last_closed(), Some(layout_tabs::ClosedItem::Batch { id, .. }) if *id == batch));
    assert!(app.layout_tabs.has_closed_tab(first) && app.layout_tabs.has_closed_tab(second));
    assert!(app.notice.is_some());
    arrive(&ctx, &mut app, 0);
    let fresh_b = app.views[0].edit_document.as_ref().unwrap().clone();
    let ready_history = steps(&app, 0);
    app.reopen_layout_item_with_context(&ctx);
    assert_eq!(app.layout_tabs.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(), vec![first, second, loading_tab]);
    assert_eq!(app.editor_rows().len(), 3);
    assert_eq!(app.layout_tabs.closed_len(), recent_before - 1);
    assert_eq!(steps(&app, 0).0, ready_history.0 + 1, "one retry restores one complete batch action after the fresh load is recorded");
    assert_eq!(app.history.kinds().last().map(String::as_str), Some("EditorBatch"));
    assert!(std::sync::Arc::ptr_eq(&fresh_b, app.views[0].edit_document.as_ref().unwrap()), "restored B adopts the completed live document");
}

#[test]
fn editor_batch_reopen_preflight_keeps_entire_batch_when_later_document_cannot_adopt() {
    let ctx = egui::Context::default();
    let (_guard, mut app, files, first, second, batch) = editor_batch_reopen_preflight_fixture(&ctx);
    let live_tab = app.new_layout_tab();
    let (_tx, rx) = std::sync::mpsc::channel();
    app.views[0].show(views::sample_data(files[1].to_str().unwrap(), &["M", "N"]), rx, 902, 1.0, 0.0);
    app.ensure_open_view(0);
    let fresh_b = app.views[0].edit_document.as_ref().unwrap().clone();
    let closed_b = app.layout_tabs.closed_items_mut().find_map(|item| match item {
        layout_tabs::ClosedItem::Tab(closed) if closed.tab.id == second => closed.tab.layout.as_mut().map(|runtime| &mut runtime.views[0]),
        _ => None,
    }).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&fresh_b, closed_b.edit_document.as_ref().unwrap()));
    closed_b.comments.failed = Some("retained B writer has not recovered".into());
    app.observe(false);
    let rows_before = app.editor_rows();
    let history_before = steps(&app, 0);
    let recent_before = app.layout_tabs.closed_len();
    app.reopen_layout_item_with_context(&ctx);
    assert_eq!(app.layout_tabs.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(), vec![live_tab]);
    assert_eq!(app.editor_rows(), rows_before);
    assert_eq!(steps(&app, 0), history_before);
    assert_eq!(app.layout_tabs.closed_len(), recent_before);
    assert!(matches!(app.layout_tabs.last_closed(), Some(layout_tabs::ClosedItem::Batch { id, .. }) if *id == batch));
    assert!(app.layout_tabs.has_closed_tab(first) && app.layout_tabs.has_closed_tab(second));
    let closed_b = app.layout_tabs.closed_items_mut().find_map(|item| match item {
        layout_tabs::ClosedItem::Tab(closed) if closed.tab.id == second => closed.tab.layout.as_mut().map(|runtime| &mut runtime.views[0]),
        _ => None,
    }).unwrap();
    assert_eq!(closed_b.comments.failed.as_deref(), Some("retained B writer has not recovered"));
    closed_b.comments.failed = None;
    app.reopen_layout_item_with_context(&ctx);
    assert_eq!(app.layout_tabs.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(), vec![first, second, live_tab]);
    assert_eq!(app.editor_rows().len(), 3);
    assert_eq!(app.layout_tabs.closed_len(), recent_before - 1);
    assert_eq!(steps(&app, 0).0, history_before.0 + 1);
    assert!(std::sync::Arc::ptr_eq(&fresh_b, app.views[0].edit_document.as_ref().unwrap()));
}
