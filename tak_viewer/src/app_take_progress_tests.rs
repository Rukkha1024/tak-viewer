#[test]
fn progress_worker_waits_for_explorer_and_foreground_open() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let folder = PathBuf::from("progress_fixture");
    let files = (0..1000)
        .map(|n| workspace::WsFile {
            path: folder.join(format!("{n:04}.takm")),
            name: format!("{n:04}.takm"),
            size: 100,
            modified: Some(std::time::UNIX_EPOCH),
        })
        .collect();
    let listings = [(
        folder.clone(),
        workspace::Listing::Ready {
            dirs: vec![],
            files,
        },
    )]
    .into();
    app.workspace = workspace::Workspace::from_listings(vec![folder.clone()], listings);
    app.explorer.expand(&mut app.workspace, &folder);

    dock::hide(&mut app.dock, Pane::Explorer);
    frame_with(&ctx, &mut app, vec![]);
    assert!(!app.explorer.progress.worker_started());

    app.hold_loads = true;
    app.open_in(&ctx, 0, folder.join("0000.takm"));
    app.show_pane(Pane::Explorer);
    frame_with(&ctx, &mut app, vec![]);
    assert!(app.views[0].loading.is_some());
    assert!(!app.explorer.progress.worker_started());

    app.views[0].loading = None;
    frame_with(&ctx, &mut app, vec![]);
    assert!(app.explorer.progress.worker_started());
}

#[test]
fn picked_progress_writes_unopened_and_clean_open_takm_with_undo_redo() {
    use crate::take_progress::{Pick, Stamp, State, Target};
    let ctx = egui::Context::default();
    let temp = comment_dir("progress_history");
    let path = temp.join("progress.takm");
    make_takm(&path);
    let saved = || tak_core::tak::read_take(&path).unwrap().meta("Progress").map(str::to_owned);
    let stamp = || Stamp::of(std::fs::metadata(&path).unwrap()).unwrap();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    let pick = |before, after| Pick { target: Target::Local(path.clone()), before: State(before), after: State(after), stamp: Some(stamp()) };

    app.pick_progress(&ctx, pick(None, Some(2)));
    wait_for_progress(&ctx, &mut app, &path);
    assert_eq!(saved().as_deref(), Some("2"));
    assert_eq!(app.history.kinds().last().map(String::as_str), Some("Progress"));
    app.undo_redo(&ctx, false);
    wait_for_progress(&ctx, &mut app, &path);
    assert_eq!(saved(), None);
    app.undo_redo(&ctx, true);
    wait_for_progress(&ctx, &mut app, &path);
    assert_eq!(saved().as_deref(), Some("2"));

    app.open_in(&ctx, 0, path.clone());
    wait_for_progress_view(&ctx, &mut app, &path);
    assert_eq!(app.views[0].take_progress(), Some(State(Some(2))));
    let old = stamp();
    app.pick_progress(&ctx, pick(Some(2), Some(5)));
    wait_for_progress(&ctx, &mut app, &path);
    assert_eq!(saved().as_deref(), Some("5"));
    assert_eq!(app.views[0].take_progress(), Some(State(Some(5))));
    assert!(!app.views[0].marker_dirty());
    assert_eq!(app.explorer.progress.visible_row(&path, old), Some((stamp(), State(Some(5)))));
}

fn wait_for_progress(ctx: &egui::Context, app: &mut TakViewerApp, path: &Path) {
    assert!(app.progress_writing(path), "no write started for {path:?}: {:?}", app.takm_note);
    let watchdog = crate::work_queue::TestWatchdog::new();
    while app.progress_writing(path) {
        watchdog.pause(|| format!("progress write for {path:?}: {:?}", app.takm_note));
        frame_with(ctx, app, vec![]);
    }
}

fn wait_for_progress_view(ctx: &egui::Context, app: &mut TakViewerApp, path: &Path) {
    let watchdog = crate::work_queue::TestWatchdog::new();
    while app.views[0].path() != Some(path) || app.views[0].analog_work_pending() {
        watchdog.pause(|| format!("loading {path:?}: shown={:?}, target={:?}, analog={}, error={:?}",
            app.views[0].path(), app.views[0].target(), app.views[0].analog_work_pending(), app.error));
        frame_with(ctx, app, vec![]);
    }
}

#[test]
fn progress_updates_save_baseline_and_leaves_unsaved_marker_edit_pending() {
    use crate::take_progress::{Pick, State, Target};
    let ctx = egui::Context::default();
    let temp = comment_dir("progress_dirty");
    let path = temp.join("progress.takm");
    make_takm(&path);
    let disk_before = tak_core::tak::read_take(&path).unwrap();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.open_in(&ctx, 0, path.clone());
    wait_for_progress_view(&ctx, &mut app, &path);
    let marker = app.views[0].data.as_ref().unwrap().markers.iter()
        .position(|marker| marker.name == "LKNE").unwrap();
    let uid = app.views[0].data.as_ref().unwrap().markers[marker].uid;
    app.views[0].select(marker, false);
    app.views[0].range = Some((5, 10));
    let before = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().revision();
    app.delete_selected(&ctx, 0);
    wait_for_close_edit(&ctx, &mut app, before);
    assert!(app.views[0].marker_dirty(), "{:?}", app.takm_note);
    let expected = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
    app.pick_progress(&ctx, Pick { target: Target::Local(path.clone()), before: State(None), after: State(Some(3)), stamp: None });
    wait_for_progress(&ctx, &mut app, &path);
    let progress_only = tak_core::tak::read_take(&path).unwrap();
    assert_eq!(progress_only.meta("Progress"), Some("3"), "{:?}", app.takm_note);
    assert_eq!(progress_only.trajectories.iter().find(|row| row.uid == uid).unwrap().frames,
               disk_before.trajectories.iter().find(|row| row.uid == uid).unwrap().frames);
    assert!(app.views[0].marker_dirty());

    app.save_markers(&ctx, 0, false, false);
    assert!(app.marker_saving(0), "no marker save started for {path:?}: {:?}", app.takm_note);
    close_wait_until(&ctx, &mut app, |app| !app.marker_saving(0));
    assert!(!app.views[0].marker_dirty(), "{:?}", app.takm_note);
    assert_eq!(tak_core::tak::read_take(&path).unwrap().meta("Progress"), Some("3"));
    let (input, _) = tak_core::edit::PreparedInput::read_from_path(&path).unwrap();
    let mut reopened = tak_core::edit::EditDocument::unloaded(path);
    reopened.install_prepared(reopened.revision(), input).unwrap();
    assert_eq!(reopened.snapshot().track(&uid).unwrap().samples, expected.track(&uid).unwrap().samples);
}

#[test]
fn busy_or_failed_progress_write_keeps_state_and_history() {
    use crate::take_progress::{Pick, State, Target};
    let ctx = egui::Context::default();
    let temp = comment_dir("progress_busy");
    let path = temp.join("progress.takm");
    make_takm(&path);
    let original = std::fs::read(&path).unwrap();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    let pick = |p| Pick { target: Target::Local(p), before: State(None), after: State(Some(4)), stamp: None };
    app.hold_loads = true;
    app.open_in(&ctx, 0, path.clone());
    let count = app.history.undo.len();
    app.pick_progress(&ctx, pick(path.clone()));
    assert!(app.progress_job.is_none());
    assert!(app.takm_note.as_ref().unwrap().0.contains("진행 중인"));
    assert_eq!(app.history.undo.len(), count);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let gone = temp.join("missing.takm");
    app.pick_progress(&ctx, pick(gone.clone()));
    wait_for_progress(&ctx, &mut app, &gone);
    assert!(!gone.exists());
    assert!(app.takm_note.as_ref().unwrap().0.contains("저장할 수 없어요"));
    assert!(!app.history.kinds().iter().any(|k| k == "Progress"));
}

#[test]
fn progress_leaves_legacy_app_ron_settings_readable() {
    let settings: Settings = ron::from_str(r#"(workspace_roots: ["old/takes"], workspace_open: ["old/takes"])"#).unwrap();
    assert_eq!(settings.workspace_roots, ["old/takes"]);
    assert_eq!(settings.workspace_open, ["old/takes"]);
}
