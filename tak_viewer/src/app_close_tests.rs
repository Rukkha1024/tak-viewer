// Included in app::tests; closes exercise the real keys, tab and Explorer actions.

#[test]
fn last_close_all_three_entry_points_restore_the_whole_view_with_one_undo() {
    for entry in ["key", "tab", "editors"] {
        let ctx = egui::Context::default();
        let dir = comment_dir(&format!("last_close_{entry}"));
        let (mut app, files) = editors_app(&ctx, &dir);
        app.close_view(1);
        app.views[0].pb.set_frame(6, 0.0);
        app.views[0].select(1, false);
        app.views[0].select(0, true);
        app.views[0].range = Some((2, 7));
        app.views[0].cam.yaw += 0.4;
        app.views[0].shown = [false, true];
        frame_with(&ctx, &mut app, vec![]);
        app.quiet();
        let spot = app.views[0].spot().unwrap();
        let selected = app.views[0].selection();
        let before = steps(&app, 0).0;
        let layout = dock::shape(&app.dock);
        match entry {
            "key" => run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]),
            "tab" => {
                let place = place_of(&app, Pane::View(0));
                let tab = view_tab(&ctx, &app, 0);
                let x = egui::pos2(tab.right() - 13.0, place.top() + dock::TAB_H / 2.0);
                click_at(&ctx, &mut app, x);
            }
            _ => {
                let x = editor_x(&ctx, &mut app, "a.tak · 숨김");
                click_at(&ctx, &mut app, x);
            }
        }
        assert!(app.open_views().is_empty(), "{entry}");
        assert_eq!(app.file_count(), 0);
        assert!(dock::has(&app.dock, Pane::Empty));
        assert_eq!(steps(&app, 0), (before + 1, 0), "{entry}: close is one action");
        undo_key(&ctx, &mut app);
        arrive(&ctx, &mut app, 0);
        overlay_arrive(&ctx, &mut app, 0);
        assert_eq!(app.open_views(), vec![0]);
        assert_eq!(dock::shape(&app.dock), layout);
        assert_eq!(app.views[0].spot(), Some(spot));
        assert_eq!(app.views[0].selection(), selected);
        assert_eq!(app.views[0].range, Some((2, 7)));
        assert_eq!(app.views[0].overlay_path(), Some(files[1].as_path()));
        assert_eq!(app.views[0].shown, [false, true]);
        assert_eq!(steps(&app, 0), (before, 1));
        redo_key(&ctx, &mut app);
        assert!(app.open_views().is_empty());
        assert_eq!(steps(&app, 0), (before + 1, 0));
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn last_close_snapshot_survives_later_file_memory_changes() {
    let ctx = egui::Context::default();
    let dir = comment_dir("last_close_snapshot");
    let (mut app, _) = editors_app(&ctx, &dir);
    app.close_view(1);
    app.views[0].pb.set_frame(3, 0.0);
    app.views[0].select(1, false);
    frame_with(&ctx, &mut app, vec![]);
    run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
    // File memory is for future fresh opens; changing it cannot rewrite a past close.
    app.settings.file_memory[0].frame = 8;
    app.settings.file_memory[0].selected.clear();
    undo_key(&ctx, &mut app);
    arrive(&ctx, &mut app, 0);
    overlay_arrive(&ctx, &mut app, 0);
    assert_eq!(app.views[0].pb.frame, 3);
    assert_eq!(app.views[0].primary, Some(1));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn last_close_while_undo_is_loading_keeps_pending_range_overlay_and_selection() {
    let ctx = egui::Context::default();
    let dir = comment_dir("last_close_pending_undo");
    let (mut app, files) = editors_app(&ctx, &dir);
    app.close_view(1);
    app.views[0].pb.set_frame(6, 0.0);
    app.views[0].select(1, false);
    app.views[0].range = Some((2, 7));
    app.views[0].shown = [false, true];
    frame_with(&ctx, &mut app, vec![]);
    let document = app.views[0].edit_document.clone().unwrap();
    let selected = app.views[0].edit_selection_state();
    run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
    undo_key(&ctx, &mut app);
    assert!(app.views[0].loading.is_some());
    assert!(app.views[0].overlay_load.is_some());
    run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
    assert!(app.open_views().is_empty());
    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].overlay_target(), Some(files[1].as_path()));
    assert_eq!(app.views[0].restore_range, Some(Some((2, 7))));
    assert!(std::sync::Arc::ptr_eq(app.views[0].restore_edit_document.as_ref().unwrap(), &document));
    assert_eq!(app.views[0].restore_edit_selection.as_ref(), Some(&selected));
    arrive(&ctx, &mut app, 0);
    overlay_arrive(&ctx, &mut app, 0);
    assert_eq!(app.views[0].pb.frame, 6);
    assert_eq!(app.views[0].primary, Some(1));
    assert_eq!(app.views[0].range, Some((2, 7)));
    assert_eq!(app.views[0].shown, [false, true]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn last_close_incoming_file_uses_its_pending_spot_instead_of_the_displayed_file() {
    let ctx = egui::Context::default();
    let dir = comment_dir("last_close_incoming");
    let path = dir.join("incoming.tak");
    std::fs::write(&path, b"held test file").unwrap();
    let mut app = one_view(&ctx);
    app.hold_loads = true;
    app.views[0].pb.set_frame(1, 0.0);
    app.views[0].select(0, false);
    app.views[0].range = Some((0, 2));
    let mut incoming = app.views[0].spot().unwrap();
    incoming.path = path.to_string_lossy().into_owned();
    incoming.frame = 8;
    incoming.selected = vec!["N".into()];
    incoming.primary = Some("N".into());
    views::remember(&mut app.settings.file_memory, incoming.clone());
    app.open(&ctx, path.clone());
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(app.views[0].path(), Some(Path::new("a.tak")));
    run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].restore, Some(incoming));
    arrive(&ctx, &mut app, 0);
    assert_eq!(app.views[0].path(), Some(path.as_path()));
    assert_eq!(app.views[0].pb.frame, 8);
    assert_eq!(app.views[0].primary, Some(1));
    assert_eq!(app.views[0].range, None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn last_close_undo_restores_the_selected_comment_and_its_markers() {
    let ctx = egui::Context::default();
    let dir = comment_dir("last_close_picked");
    two_comments_on_disk(&dir);
    let mut app = comment_app(&ctx, &dir, false);
    app.hold_loads = true;
    app.views[0].select_comment(2);
    let selected = app.views[0].selection();
    frame_with(&ctx, &mut app, vec![]);
    run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
    undo_key(&ctx, &mut app);
    // Close a second time before arrival to check the pending Comment selection too.
    run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
    undo_key(&ctx, &mut app);
    arrive(&ctx, &mut app, 0);
    assert!(app.views[0].comments.list.get(2).is_some());
    assert_eq!(app.views[0].selection(), selected);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn last_close_saved_empty_session_restarts_empty_and_fresh_open_defaults_layout() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.hide_pane(Pane::Selected);
    run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
    let mut storage = FileLike::default();
    app.save(&mut storage);
    let saved: Settings = eframe::get_value(&storage, eframe::APP_KEY).unwrap();
    assert!(saved.views.is_empty());
    assert!(dock::views(saved.layout.as_ref().unwrap()).is_empty());
    for side in [false, true] {
        let ctx = egui::Context::default();
        let mut again = TakViewerApp::build(&ctx, Some(&storage), vec![], None, None);
        again.hold_loads = true;
        assert!(again.open_views().is_empty());
        frame_with(&ctx, &mut again, vec![]);
        assert!(again.views.iter().all(|v| v.data.is_none()));
        let path = missing(30 + usize::from(side));
        if side { again.open_side(&ctx, path.clone()); } else { again.open(&ctx, path.clone()); }
        assert_eq!(again.open_views(), vec![0]);
        assert_eq!(again.views[0].target(), Some(path.as_path()));
        assert_eq!(dock::shape(&again.dock), dock::shape(&dock::default_layout()));
        assert_eq!(again.hidden, dock::Hidden::default());
    }
    let fresh = TakViewerApp::build(&egui::Context::default(), None, vec![], None, None);
    assert_eq!(fresh.open_views(), vec![0], "old unsaved settings keep the initial default view");
}

#[test]
fn last_close_empty_editor_opens_explorer_drag_and_os_dropped_files() {
    use crate::workspace::{Listing, WsFile};
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.hold_loads = true;
    app.close_view(0);
    let dir = PathBuf::from("no_such_dir_tak_viewer_test");
    let path = dir.join("drop.takm");
    let listing = Listing::Ready { dirs: vec![], files: vec![WsFile { path: path.clone(), name: "drop.takm".into(), size: 0 }] };
    app.workspace = Workspace::from_listings(vec![dir.clone()], std::collections::HashMap::from([(dir.clone(), listing)]));
    app.explorer.expand(&mut app.workspace, &dir);
    frame_with(&ctx, &mut app, vec![]);
    let explorer = place_of(&app, Pane::Explorer);
    let from = text_in(&ctx, &mut app, "drop.takm", explorer);
    let to = place_of(&app, Pane::Empty).center();
    drag(&ctx, &mut app, from, to);
    assert_eq!(app.views[0].target(), Some(path.as_path()));
    assert_eq!(app.open_views(), vec![0]);
    assert!(egui::DragAndDrop::payload::<explorer::FileDrag>(&ctx).is_none());
    app.close_view(0);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0));
    let dropped = [missing(50), missing(51)];
    let _ = ctx.run(egui::RawInput {
        screen_rect: Some(screen), focused: true,
        dropped_files: dropped.iter().map(|p| egui::DroppedFile { path: Some(p.clone()), ..Default::default() }).collect(),
        ..Default::default()
    }, |ctx| app.frame(ctx));
    assert_eq!(app.open_views(), vec![0, 1]);
    assert_eq!(app.views[0].target(), Some(dropped[0].as_path()));
    assert_eq!(app.views[1].target(), Some(dropped[1].as_path()));
}

#[test]
fn last_close_empty_editor_opens_explorer_click_path_dialog_and_recent_menu() {
    use crate::workspace::{Listing, WsFile};
    for route in ["explorer", "path", "recent"] {
        let ctx = egui::Context::default();
        let dir = comment_dir(&format!("last_close_open_{route}"));
        let path = dir.join("open.takm");
        std::fs::write(&path, b"held test file").unwrap();
        let mut app = one_view(&ctx);
        app.hold_loads = true;
        app.close_view(0);
        let mut out = frame_with(&ctx, &mut app, vec![]);
        match route {
            "explorer" => {
                let listing = Listing::Ready { dirs: vec![], files: vec![WsFile { path: path.clone(), name: "open.takm".into(), size: 0 }] };
                app.workspace = Workspace::from_listings(vec![dir.clone()], std::collections::HashMap::from([(dir.clone(), listing)]));
                app.explorer.expand(&mut app.workspace, &dir);
                let place = place_of(&app, Pane::Explorer);
                let file = text_in(&ctx, &mut app, "open.takm", place);
                click_at(&ctx, &mut app, file);
            }
            "path" => {
                run_frame(&ctx, &mut app, &[(ctrl(), Key::L)]);
                assert!(app.path_input.is_some());
                app.path_input = Some(path.to_string_lossy().into_owned());
                out = frame_with(&ctx, &mut app, vec![]);
                let button = text_rects(&out, "열기")[0].center();
                click_at(&ctx, &mut app, button);
            }
            _ => {
                app.settings.recent = vec![path.to_string_lossy().into_owned()];
                out = click_at(&ctx, &mut app, text_rects(&out, "파일")[0].center());
                out = click_at(&ctx, &mut app, text_rects(&out, "최근 파일")[0].center());
                let file = text_rects(&out, &path.to_string_lossy())[0].center();
                click_at(&ctx, &mut app, file);
            }
        }
        assert_eq!(app.open_views(), vec![0], "{route}");
        assert_eq!(app.views[0].target(), Some(path.as_path()), "{route}");
        assert_eq!(dock::shape(&app.dock), dock::shape(&dock::default_layout()), "{route}: default pane layout");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn last_close_fresh_open_defaults_graph_and_undo_redo_keep_it_one_action() {
    let ctx = egui::Context::default();
    let dir = comment_dir("last_close_graph_default");
    let path = dir.join("new.tak");
    std::fs::write(&path, b"held test file").unwrap();
    let mut app = one_view(&ctx);
    app.hold_loads = true;
    app.set_graph_float(false);
    frame_with(&ctx, &mut app, vec![]);
    app.close_view(0);
    frame_with(&ctx, &mut app, vec![]);
    let layout = dock::shape(&app.dock);
    let before = steps(&app, 0).0;
    assert!(!app.settings.graph_float);
    app.open(&ctx, path);
    frame_with(&ctx, &mut app, vec![]);
    assert!(app.settings.graph_float);
    assert!(!dock::has(&app.dock, Pane::Graph));
    assert_eq!(steps(&app, 0), (before + 1, 0));
    undo_key(&ctx, &mut app);
    assert!(app.open_views().is_empty());
    assert!(!app.settings.graph_float);
    assert_eq!(dock::shape(&app.dock), layout);
    redo_key(&ctx, &mut app);
    assert_eq!(app.open_views(), vec![0]);
    assert!(app.settings.graph_float);
    assert!(!dock::has(&app.dock, Pane::Graph));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn last_close_during_quiet_overlay_replacement_preserves_the_incoming_overlay() {
    let ctx = egui::Context::default();
    let dir = comment_dir("last_close_pending_overlay");
    let (mut app, files) = overlay_app(&ctx, &dir, &["a.tak", "b.tak", "c.tak"]);
    lay_over(&ctx, &mut app, &files[1]);
    overlay_arrive(&ctx, &mut app, 0);
    lay_over(&ctx, &mut app, &files[2]);
    overlay_arrive(&ctx, &mut app, 0);
    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].overlay_path(), Some(files[2].as_path()));
    assert_eq!(app.views[0].overlay_target(), Some(files[1].as_path()));
    run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].overlay_target(), Some(files[1].as_path()));
    arrive(&ctx, &mut app, 0);
    overlay_arrive(&ctx, &mut app, 0);
    assert_eq!(app.views[0].overlay_path(), Some(files[1].as_path()));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn last_close_dirty_dialog_cancel_and_discard_work_for_all_three_entry_points() {
    for entry in ["key", "tab", "editors"] {
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        app.views[0].comments.failed = Some("write failed".into());
        let before = steps(&app, 0);
        match entry {
            "key" => run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]),
            "tab" => {
                let place = place_of(&app, Pane::View(0));
                let tab = view_tab(&ctx, &app, 0);
                click_at(&ctx, &mut app, egui::pos2(tab.right() - 13.0, place.top() + dock::TAB_H / 2.0));
            }
            _ => {
                let x = editor_x(&ctx, &mut app, "a.tak");
                click_at(&ctx, &mut app, x);
            }
        }
        assert!(app.dirty_prompt.is_some(), "{entry}");
        assert_eq!(app.open_views(), vec![0]);
        assert_eq!(steps(&app, 0), before);
        let out = frame_with(&ctx, &mut app, vec![]);
        let cancel = text_rects(&out, "Cancel")[0].center();
        click_at(&ctx, &mut app, cancel);
        assert!(app.dirty_prompt.is_none());
        assert_eq!(app.open_views(), vec![0]);
        assert!(app.views[0].comments.failed.is_some());
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        let out = frame_with(&ctx, &mut app, vec![]);
        let discard = text_rects(&out, "Discard")[0].center();
        click_at(&ctx, &mut app, discard);
        assert!(app.dirty_prompt.is_none());
        assert!(app.open_views().is_empty());
        assert_eq!(steps(&app, 0), (before.0 + 1, before.1));
    }
}

fn close_wait_until(ctx: &egui::Context, app: &mut TakViewerApp, ready: impl Fn(&TakViewerApp) -> bool) {
    let began = Instant::now();
    while !ready(app) {
        assert!(began.elapsed() < Duration::from_secs(10), "close test timed out: {:?}", app.takm_note);
        frame_with(ctx, app, vec![]);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn last_close_save_button_saves_marker_edits_and_failed_save_keeps_the_view_open() {
    for fail in [false, true] {
        let ctx = egui::Context::default();
        let dir = comment_dir(&format!("last_close_save_{fail}"));
        let source = dir.join("source.takm");
        make_takm(&source);
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.open(&ctx, source.clone());
        close_wait_until(&ctx, &mut app, |app| app.views[0].data.is_some() && !app.views[0].busy());
        app.views[0].select(0, false);
        app.views[0].pb.frame = 10;
        app.delete_selected(&ctx, 0);
        close_wait_until(&ctx, &mut app, |app| !app.marker_job_running(0) && app.views[0].edit_refresh.is_none());
        assert!(app.views[0].marker_dirty(), "Delete made a real unsaved edit: {:?}", app.takm_note);
        run_frame(&ctx, &mut app, &[(ctrl(), Key::W)]);
        assert!(app.dirty_prompt.is_some());
        assert_eq!(app.open_views(), vec![0]);
        if fail { std::fs::remove_file(&source).unwrap(); }
        let out = frame_with(&ctx, &mut app, vec![]);
        click_at(&ctx, &mut app, text_rects(&out, "Save")[0].center());
        close_wait_until(&ctx, &mut app, |app| !app.marker_saving(0));
        if fail {
            assert_eq!(app.open_views(), vec![0]);
            assert!(app.views[0].marker_dirty());
            assert!(app.dirty_prompt.is_some());
        } else {
            assert!(app.open_views().is_empty());
            let (prepared, _) = tak_core::edit::PreparedInput::read_from_path(&source).unwrap();
            let mut document = tak_core::edit::EditDocument::unloaded(source.clone());
            document.install_prepared(document.revision(), prepared).unwrap();
            assert!(!document.snapshot().tracks().any(|track| track.name == "LKNE" && track.samples.iter().any(|sample| sample.frame() == 10)), "the saved marker sample is removed");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
