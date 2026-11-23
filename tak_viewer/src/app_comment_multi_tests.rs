fn multi_drain(ctx: &egui::Context, app: &mut TakViewerApp) {
    let watchdog = crate::work_queue::TestWatchdog::new();
    while app.comment_outstanding > 0 {
        app.poll_comment_writes(ctx);
        if app.comment_outstanding == 0 { break; }
        watchdog.pause(|| format!("Comment writer did not finish. Outstanding={}, queued={}, registration={}, error={:?}",
            app.comment_outstanding, app.comment_waiting.len(), app.comment_registration.is_some(), app.comment_registration_error));
    }
}
fn multi_frame(ctx: &egui::Context, app: &mut TakViewerApp, modifiers: Modifiers, events: Vec<egui::Event>) -> egui::FullOutput {
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0));
    ctx.run(egui::RawInput { screen_rect: Some(screen), focused: true, modifiers, events, ..Default::default() }, |ctx| app.frame(ctx))
}
fn multi_click(ctx: &egui::Context, app: &mut TakViewerApp, id: u64, modifiers: Modifiers, cell: usize) {
    let at = cells_of(ctx, app, id)[cell];
    idle(ctx, app);
    let b = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers };
    multi_frame(ctx, app, modifiers, vec![egui::Event::PointerMoved(at), b(true), b(false)]);
}
fn multi_ids(app: &TakViewerApp) -> Vec<u64> { app.views[0].comment_selection.ids().collect() }
fn multi_app(ctx: &egui::Context, name: &str) -> (tak_core::test_support::TempDir, TakViewerApp) {
    let dir = tak_core::test_support::TempDir::new(&format!("tak_viewer_comments_{name}"));
    comments_on_disk(&dir, &[(1, &["M"], 4, 5), (2, &["N"], 1, 2), (3, &["O"], 7, 8), (4, &["M"], 2, 3), (5, &[], 6, 7)]);
    let app = comments_tab_app(ctx, &dir, false);
    (dir, app)
}
#[test]
fn multicomment_shift_ctrl_sort_and_one_undo_redo() {
    let ctx = egui::Context::default();
    let (dir, mut app) = multi_app(&ctx, "multi_undo");
    multi_click(&ctx, &mut app, 2, Modifiers::NONE, 2);
    multi_click(&ctx, &mut app, 4, Modifiers::SHIFT, 2);
    assert_eq!(multi_ids(&app), vec![2, 3, 4]);
    assert_eq!(picked(&app), Some(4));
    let prior = app.views[0].selection();
    let old = app.history.undo.len();
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Delete)]);
    assert_eq!(app.views[0].comments.list.len(), 2);
    assert!(multi_ids(&app).is_empty());
    assert_eq!(app.history.undo.len(), old + 1);
    assert!(!app.marker_job_running(0));
    run_frame(&ctx, &mut app, &[(ctrl(), Key::Z)]);
    assert_eq!(app.views[0].comments.list.len(), 5);
    assert_eq!(app.views[0].selection(), prior);
    run_frame(&ctx, &mut app, &[(ctrl(), Key::Y)]);
    assert_eq!(app.views[0].comments.list.len(), 2);
    multi_drain(&ctx, &mut app);
    let parsed = tail_read(&dir.join("a.takm")).unwrap().unwrap();
    assert_eq!(parsed.comments().list().iter().map(|c| c.id).collect::<Vec<_>>(), vec![1, 5]);
    assert_eq!(parsed.events.len(), 14);
}
#[test]
fn multicomment_modifiers_win_over_cross_and_type_and_single_cross_keeps_others() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = multi_app(&ctx, "multi_cells");
    multi_click(&ctx, &mut app, 1, Modifiers::NONE, 2);
    multi_click(&ctx, &mut app, 3, ctrl(), 3);
    assert_eq!(multi_ids(&app), vec![1, 3]);
    assert_eq!(app.views[0].comments.list.len(), 5);
    assert!(app.comment_edit.is_none());
    let at = comment_list::type_point(row_of(&ctx, &mut app, 4));
    idle(&ctx, &mut app);
    let b = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: ctrl() };
    multi_frame(&ctx, &mut app, ctrl(), vec![egui::Event::PointerMoved(at), b(true), b(false)]);
    assert_eq!(multi_ids(&app), vec![1, 3, 4]);
    assert!(app.comment_edit.is_none());
    multi_click(&ctx, &mut app, 4, Modifiers::NONE, 3);
    assert_eq!(multi_ids(&app), vec![1, 3]);
    assert_eq!(picked(&app), Some(3));
}
#[test]
fn multicomment_all_focus_escape_keeps_range_and_marker_pick_clears_with_undo() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = multi_app(&ctx, "multi_focus");
    multi_click(&ctx, &mut app, 2, Modifiers::NONE, 2);
    run_frame(&ctx, &mut app, &[(ctrl(), Key::A)]);
    assert_eq!(multi_ids(&app), vec![1, 2, 3, 4, 5]);
    assert_eq!(picked(&app), Some(2));
    app.views[0].range = Some((1, 7));
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
    assert!(multi_ids(&app).is_empty());
    assert!(app.views[0].selected.is_empty());
    assert_eq!(app.views[0].range, Some((1, 7)));
    run_frame(&ctx, &mut app, &[(ctrl(), Key::Z)]);
    assert_eq!(multi_ids(&app).len(), 5);
    let prior = app.views[0].selection();
    app.views[0].select(2, false);
    app.observe_frame(&ctx);
    assert!(multi_ids(&app).is_empty());
    assert_eq!(picked(&app), None);
    run_frame(&ctx, &mut app, &[(ctrl(), Key::Z)]);
    assert_eq!(app.views[0].selection(), prior);
    let at = place_of(&app, Pane::Explorer).center();
    tap(&ctx, &mut app, at);
    let previous = multi_ids(&app);
    run_frame(&ctx, &mut app, &[(ctrl(), Key::A)]);
    assert_eq!(multi_ids(&app), previous);
}
#[test]
fn multicomment_readonly_saving_and_write_failure_are_honest() {
    let ctx = egui::Context::default();
    let (dir, mut app) = multi_app(&ctx, "multi_refusal");
    app.comment_gesture(0, 1, false, false);
    app.comment_gesture(0, 3, true, false);
    let before = app.views[0].selection();
    app.views[0].comments.target = Some(Err("읽기 전용".into()));
    app.delete_comments(&ctx, 0, &[1, 3], false);
    assert_eq!(app.views[0].comments.list.len(), 5);
    assert_eq!(app.views[0].selection(), before);
    assert!(takm_note(&app).contains("읽기 전용"));
    app.views[0].comments.target = Some(Ok(dir.join("missing").join("a.takm")));
    app.delete_comments(&ctx, 0, &[1, 3], false);
    multi_drain(&ctx, &mut app);
    app.poll_comment_writes(&ctx);
    assert!(app.views[0].comments.failed.is_some());
    assert_eq!(tail_read(&dir.join("a.takm")).unwrap().unwrap().comments().len(), 5);
}


fn multi_capture(ctx: &egui::Context, app: &mut TakViewerApp, path: &Path) {
    let mut textures = crate::soft_render::Textures::default();
    for _ in 0..3 {
        let out = frame_with(ctx, app, vec![]);
        textures.set(&out.textures_delta);
        let atlas = egui::epaint::textures::TexturesDelta { set: vec![(egui::TextureId::default(), egui::epaint::ImageDelta::full(ctx.fonts(|f| f.image()), egui::TextureOptions::LINEAR))], free: vec![] };
        textures.set(&atlas);
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        let image = crate::soft_render::paint(&prims, &textures, [1600, 960], out.pixels_per_point, theme::PANEL_BG);
        let rgba = image.pixels.iter().flat_map(|p| p.to_array()).collect::<Vec<_>>();
        crate::shot::save_png(path, 1600, 960, &rgba).unwrap();
    }
}

#[test]
fn multicomment_conflict_reloads_all_batch_rows_without_partial_delete() {
    let ctx = egui::Context::default();
    let (dir, mut app) = multi_app(&ctx, "multi_conflict");
    let path = dir.join("a.takm");
    app.comment_gesture(0, 1, false, false);
    app.comment_gesture(0, 3, true, false);
    let prior = app.views[0].selection();
    let mut changed = comment(&app, 3);
    changed.text = "Changed by another window".into();
    let event = Event::of(EventKind::Edit, &changed, "t".into(), takm_label(&path));
    tak_core::takm::append_events(&path, &[event]).unwrap();
    let before_bytes = std::fs::read(&path).unwrap();
    app.delete_comments(&ctx, 0, &[1, 3], false);
    assert_eq!(app.views[0].comments.list.len(), 3);
    multi_drain(&ctx, &mut app);
    assert_eq!(app.views[0].comments.list.len(), 5);
    assert_eq!(comment(&app, 3), changed);
    assert!(app.views[0].comments.list.get(1).is_some());
    assert_eq!(app.views[0].selection(), prior);
    assert_eq!(std::fs::read(&path).unwrap(), before_bytes);
    assert!(app.comment_failed.is_empty());
    assert!(app.views[0].comments.failed.is_none());
    assert!(!app.history.kinds().iter().any(|kind| kind.contains("Comment")), "conflicting batch must not remain replayable");
}

fn multi_real_wait(
    ctx: &egui::Context,
    app: &mut TakViewerApp,
    description: &str,
    mut ready: impl FnMut(&TakViewerApp) -> bool,
) {
    let until = Instant::now() + Duration::from_secs(30);
    while Instant::now() < until {
        frame_with(ctx, app, vec![]);
        if ready(app) { return; }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("real interaction timed out: {description}; marker note={:?}; notice={:?}", app.takm_note, app.notice);
}

fn multi_real_selected_uids(app: &TakViewerApp, slot: usize) -> Vec<tak_core::tak::Uid> {
    let data = app.views[slot].data.as_ref().unwrap();
    app.views[slot].ordered_selection().iter().map(|&index| data.markers[index].uid).collect()
}

fn multi_real_labeling_and_dirty_batch(
    ctx: &egui::Context,
    app: &mut TakViewerApp,
    path: &Path,
    scratch: &Path,
    artifacts: &Path,
    interaction_bytes: &[u8],
    first_tab: u64,
) {
    use crate::ui::labeling::{LabelingAction, MarkerList, SelectionGesture};
    app.activate(0);
    app.show_pane(Pane::Labeling);
    frame_with(ctx, app, vec![]);
    let named = app.marker_ui[0].labeling.track_order(MarkerList::Named);
    let rows: Vec<_> = app.marker_ui[0].labeling.groups.iter().flat_map(|group| group.labels.iter()).cloned().collect();
    let named_rows: Vec<_> = named.iter().map(|uid| rows.iter().find(|row| row.track_uid == Some(*uid)).unwrap().clone()).collect();
    assert!(named.len() >= 3, "real fixture needs at least three named tracks with samples");
    let empty_count = rows.iter().filter(|row| !row.has_samples).count();
    let empty_uids: std::collections::BTreeSet<_> = rows.iter().filter(|row| !row.has_samples).filter_map(|row| row.track_uid).collect();
    // Pick a real endpoint whose full search text hides the anchor and at least one
    // intermediate eligible UID. Duplicate display names remain identified by their UID.
    let (anchor_index, end_index) = (0..named.len()).find_map(|start| {
        (start + 2..named.len()).find_map(|end| {
            let needle = named_rows[end].name.to_lowercase();
            named_rows[start..end].iter().all(|row| !row.name.to_lowercase().contains(&needle))
                .then_some((start, end))
        })
    }).expect("real named rows must provide a searchable hidden range");
    let anchor = named[anchor_index];
    let endpoint = named[end_index];
    let endpoint_name = named_rows[end_index].name.clone();
    let expected_range = named[anchor_index..=end_index].to_vec();
    app.labeling_action(ctx, 0, LabelingAction::SelectTracks { list: MarkerList::Named, uid: Some(anchor), gesture: SelectionGesture::Plain });
    app.observe(true);
    let armed = app.views[0].armed_label;
    app.labeling_action(ctx, 0, LabelingAction::SetFilter(endpoint_name.clone()));
    // Filtering also paints the endpoint text inside Search. Its galley is not a marker
    // row. Settle the new scroll geometry, then restrict the hit to the named-list body.
    frame_with(ctx, app, vec![]);
    let out = frame_with(ctx, app, vec![]);
    let pane = place_of(app, Pane::Labeling);
    let named_top = text_rects(&out, "Labels").into_iter().filter(|rect| pane.contains(rect.center()))
        .map(|rect| rect.bottom()).reduce(f32::max).expect("named-list column headers are painted");
    let named_bottom = text_rects(&out, "Unlabeled Markers").into_iter().filter(|rect| pane.contains(rect.center()))
        .map(|rect| rect.top()).reduce(f32::min).expect("Unlabeled section bounds the named-list body");
    let row = text_rects(&out, &endpoint_name).into_iter().find(|rect|
        pane.contains(rect.center()) && rect.top() > named_top && rect.bottom() < named_bottom)
        .expect("filtered endpoint is painted below the named-list header, outside Search");
    let at = row.center();
    eprintln!("real Labeling endpoint={endpoint_name} row={row:?} named_body_y={named_top}..{named_bottom} click={at:?}");
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::SHIFT };
    multi_frame(ctx, app, Modifiers::SHIFT, vec![egui::Event::PointerMoved(at)]);
    multi_frame(ctx, app, Modifiers::SHIFT, vec![button(true)]);
    multi_frame(ctx, app, Modifiers::SHIFT, vec![button(false)]);
    assert_eq!(key_focus::target(ctx), key_focus::Target::NamedMarkers(0));
    assert_eq!(multi_real_selected_uids(app, 0), expected_range, "Shift includes hidden anchor/intermediate UIDs in column order");
    assert_eq!(app.views[0].data.as_ref().unwrap().markers[app.views[0].primary.unwrap()].uid, endpoint);
    assert_eq!(app.views[0].armed_label, armed, "Shift changes selection without arming QuickLabel");
    let range_hidden = expected_range.len() - 1;
    let range_line = format!("{} selected ({} hidden)", expected_range.len(), range_hidden);
    let out = frame_with(ctx, app, vec![]);
    assert!(!text_rects(&out, &range_line).is_empty(), "hidden count is always visible for the real range");
    multi_capture(ctx, app, &artifacts.join("shots/labeling_hidden_range.png"));
    // The real row click above sets the named-list key scope. This is the actual Ctrl+A
    // route, not a direct call to the selection helper or to the global marker selection.
    run_frame(ctx, app, &[(ctrl(), Key::A)]);
    let all_selected: std::collections::BTreeSet<_> = multi_real_selected_uids(app, 0).into_iter().collect();
    assert_eq!(all_selected, named.iter().copied().collect());
    assert!(all_selected.is_disjoint(&empty_uids), "names with no take-wide samples are excluded");
    assert_eq!(app.views[0].data.as_ref().unwrap().markers[app.views[0].primary.unwrap()].uid, endpoint, "Ctrl+A retains the included Pattern target");
    assert_eq!(app.views[0].armed_label, armed);
    let visible_named = named_rows.iter().filter(|row| row.name.to_lowercase().contains(&endpoint_name.to_lowercase())).count();
    let all_hidden = named.len() - visible_named;
    let out = frame_with(ctx, app, vec![]);
    assert!(!text_rects(&out, &format!("{} selected ({} hidden)", named.len(), all_hidden)).is_empty());
    multi_capture(ctx, app, &artifacts.join("shots/labeling_hidden_all.png"));
    assert_eq!(std::fs::read(path).unwrap(), interaction_bytes);

    // Dirty the real shared edit document through the product's prepare/delete worker.
    // Discard later must restore its saved state without writing a marker-save file.
    let data = app.views[0].data.as_ref().unwrap();
    let index = data.markers.iter().position(|marker| marker.uid == anchor).unwrap();
    let frame = *data.take.trajectories.iter().find(|track| track.uid == anchor).unwrap().frames.first().unwrap();
    let frame = i64::from(frame);
    app.views[0].select(index, false);
    app.views[0].range = None;
    app.views[0].pb.set_frame(frame, 0.0);
    app.labeling_action(ctx, 0, LabelingAction::SetFilter(String::new()));
    app.delete_selected(ctx, 0);
    multi_real_wait(ctx, app, "real marker Delete", |app| !app.marker_job_running(0) && app.views[0].edit_refresh.is_none());
    assert!(app.views[0].marker_dirty(), "Delete on a real sample dirties the actual file document");
    let document = app.views[0].edit_document.as_ref().unwrap().clone();
    let document_id = app.views[0].document_id();
    assert_eq!(std::fs::read(path).unwrap(), interaction_bytes, "marker edits remain in memory before Discard");

    // The second real file hosts an overlay of the first. Source appears twice in the
    // hidden first tab, once in the active second tab, and once as that tab's overlay.
    let host_path = scratch.join("real_batch_overlay_host.takm");
    std::fs::copy(path, &host_path).unwrap();
    let second_tab = app.new_layout_tab();
    app.open(ctx, path.to_path_buf());
    assert_eq!(app.views[0].document_id(), document_id);
    app.open_side(ctx, host_path.clone());
    multi_real_wait(ctx, app, "real overlay host load", |app| app.open_views().len() == 2
        && app.views[1].path() == Some(host_path.as_path()) && !app.views[1].busy());
    app.start_overlay(ctx, 1, path.to_path_buf(), false);
    multi_real_wait(ctx, app, "real shared overlay", |app| app.views[1].overlay_path() == Some(path)
        && !app.views[1].busy() && app.views[1].overlay_document_id() == document_id);
    app.views[0].pb.set_frame(frame + 1, 0.0);
    app.views[1].shown = [true, false];
    app.observe(false);
    let before_rows = app.editor_rows();
    assert_eq!(before_rows.len(), 5, "four real base occurrences and one real overlay");
    assert_eq!(before_rows.iter().filter(|row| row.path == path).count(), 4);
    assert_eq!(before_rows.iter().filter(|row| row.tab_id == first_tab).count(), 2);
    assert_eq!(before_rows.iter().filter(|row| row.tab_id == second_tab).count(), 3);
    app.select_all_editors();
    let selected_before = app.editor_selection.clone();
    let count_before = app.history.undo.len();
    app.close_selected_editors(ctx);
    assert!(app.dirty_prompt.is_some(), "real marker edit prompts before any batch close");
    assert_eq!(app.editor_rows(), before_rows);
    assert_eq!(app.layout_tabs.tabs().len(), 2);
    multi_capture(ctx, app, &artifacts.join("shots/real_batch_dirty_prompt.png"));
    app.cancel_dirty_transition(ctx);
    assert!(app.dirty_prompt.is_none());
    assert_eq!(app.layout_tabs.active_id(), Some(second_tab));
    assert_eq!(app.editor_rows(), before_rows);
    assert_eq!(app.editor_selection, selected_before);
    assert_eq!(app.history.undo.len(), count_before);
    assert!(document.lock().unwrap().is_dirty(), "Cancel preserves the real edit");
    assert_eq!(std::fs::read(path).unwrap(), interaction_bytes);

    app.close_selected_editors(ctx);
    assert!(app.dirty_prompt.is_some());
    app.discard_pending_transition(ctx);
    assert!(app.dirty_prompt.is_none(), "one shared dirty document receives one decision across all occurrences");
    assert!(app.layout_tabs.tabs().is_empty());
    assert!(!document.lock().unwrap().is_dirty());
    assert!(matches!(app.history.undo.last().unwrap().as_slice(), [undo::Part::EditorBatch { .. }]));
    let closed_count = app.history.undo.len();
    assert!(matches!(app.layout_tabs.last_closed(), Some(layout_tabs::ClosedItem::Batch { .. })));
    app.undo_redo(ctx, false);
    assert_eq!(app.history.undo.len(), closed_count - 1, "one undo restores the real tab batch");
    assert_eq!(app.editor_rows(), before_rows);
    assert_eq!(app.editor_selection, selected_before);
    assert_eq!(app.layout_tabs.active_id(), Some(second_tab));
    assert_eq!(app.views[1].overlay_document_id(), document_id);
    assert_eq!(app.views[1].shown, [true, false]);
    assert!(!document.lock().unwrap().is_dirty(), "undo never resurrects discarded real marker edits");
    multi_real_wait(ctx, app, "discarded real view refresh", |app| app.open_views().iter().all(|&slot|
        !app.views[slot].busy() && !app.marker_job_running(slot) && !app.marker_saving(slot) && app.views[slot].edit_refresh.is_none()));
    app.undo_redo(ctx, true);
    assert!(app.dirty_prompt.is_none());
    assert!(app.layout_tabs.tabs().is_empty());
    let recent = app.layout_tabs.closed_len();
    app.reopen_layout_item_with_context(ctx);
    assert_eq!(app.layout_tabs.closed_len(), recent - 1, "one recent reopen restores the complete real batch");
    assert_eq!(app.editor_rows(), before_rows);
    assert_eq!(app.editor_selection, selected_before);
    assert_eq!(app.views[1].overlay_document_id(), document_id);
    assert!(!document.lock().unwrap().is_dirty());
    multi_real_wait(ctx, app, "real batch reopen", |app| app.open_views().iter().all(|&slot|
        !app.views[slot].busy() && !app.marker_job_running(slot) && app.views[slot].edit_refresh.is_none()));
    multi_capture(ctx, app, &artifacts.join("shots/open_editors_real_batch_reopened.png"));
    let reopened_count = app.history.undo.len();
    app.reopen_editor_batch(ctx);
    assert_eq!(app.history.undo.len(), reopened_count, "the real batch occupied only one recent-history item");
    assert_eq!(std::fs::read(path).unwrap(), interaction_bytes);
    assert_eq!(std::fs::read(&host_path).unwrap(), interaction_bytes);
    let receipt = format!(
        "fixture={}\nlabeling_input=real Shift pointer click plus named-list-focused Ctrl+A\nnamed_with_samples={}\nno_sample_named_rows_excluded={}\nrange_selected={}\nrange_hidden={}\nall_selected={}\nall_hidden={}\npattern_target={}\nquicklabel_armed_unchanged=true\nfirst_tab={}\nsecond_tab={}\nreal_file_occurrence_rows=5\nsource_document_occurrences=4\ndirty_edit=actual Delete worker on real marker sample\ncancel_keeps_all_views_and_dirty_edit=true\ndirty_document_decisions=1\ndiscard_keeps_document_clean_after_undo=true\nclose_undo_actions=1\nrecent_reopen_items=1\nreal_takm_and_comment_bytes_unchanged=true\nreal_host_copy_bytes_unchanged=true\n",
        path.display(), named.len(), empty_count, expected_range.len(), range_hidden,
        named.len(), all_hidden, endpoint_name, first_tab, second_tab,
    );
    std::fs::write(artifacts.join("real_interaction_receipt.txt"), receipt).unwrap();
}
