use crate::instances::LoadState;
use crate::window_command::{StartupFile, StartupView};

fn startup_tab(name: &str, files: &[PathBuf], options: StartupView) -> StartupTab {
    StartupTab {
        name: name.into(),
        files: files
            .iter()
            .map(|path| StartupFile {
                path: path.clone(),
                identity: tak_core::takm::canonical_target(path).unwrap(),
            })
            .collect(),
        options,
    }
}

fn launch_state(app: &TakViewerApp, tab: usize, slot: usize) -> Option<LoadState> {
    app.window_loads().tabs[tab].views[slot].state
}

#[test]
fn only_build_uses_saved_panels_and_opens_four_named_files_in_order() {
    let dir = tak_core::test_support::TempDir::new("tak_viewer_only_build");
    let named = take_files(&dir, &["d.tak", "b.tak", "c.tak", "a.tak"]);
    let old = take_files(&dir, &["saved.tak"]);
    let mut layout = dock::default_layout();
    dock::add_view(&mut layout, 2, dock::NOMINAL_AREA);
    dock::hide(&mut layout, Pane::Explorer);
    let saved_tab = SavedLayoutTab {
        name: "Previous layout".into(),
        layout: dock::EditorLayout::Leaf {
            slots: vec![2],
            active: 2,
        },
        views: vec![SavedLayoutView {
            slot: 2,
            path: old[0].to_string_lossy().into_owned(),
            spot: None,
        }],
        ..Default::default()
    };
    let settings = Settings {
        layout: Some(layout),
        layout_version: dock::LAYOUT_VERSION,
        views: vec![(2, old[0].to_string_lossy().into_owned())],
        active_view: 2,
        layout_tabs: Some(vec![
            SavedLayoutTab {
                name: "Earlier layout".into(),
                ..Default::default()
            },
            saved_tab,
        ]),
        active_layout_tab: 1,
        speed: 1.5,
        ..Default::default()
    };
    let mut storage = FileLike::default();
    eframe::set_value(&mut storage, eframe::APP_KEY, &settings);
    storage.flush();
    let saved_bytes = storage.get_string(eframe::APP_KEY);

    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build_window(
        &ctx,
        Some(&storage),
        vec![],
        None,
        None,
        Some(vec![startup_tab(
            "tab1",
            &named,
            StartupView {
                frame: Some(3),
                select: Some("M,N".into()),
                comments: true,
                ..Default::default()
            },
        )]),
    );
    assert!(!dock::has(&app.dock, Pane::Explorer));
    assert!(dock::has(&app.dock, Pane::Comments));
    assert_eq!(app.settings.speed, 1.5);
    assert_eq!(app.layout_tabs.tabs().len(), 1);
    assert_eq!(app.layout_tabs.active().unwrap().name, "tab1");
    let paths: Vec<_> = dock::views_reading_order(&app.dock)
        .into_iter()
        .map(|slot| app.views[usize::from(slot)].target().unwrap())
        .collect();
    assert_eq!(
        paths,
        named.iter().map(PathBuf::as_path).collect::<Vec<_>>()
    );
    assert!(
        app.window_tabs()[0].views.is_empty(),
        "pending loads cannot confirm a launch"
    );

    // Feed ready marker data before drawing. No worker timing controls these assertions.
    for (slot, path) in named.iter().enumerate() {
        let view = &mut app.views[slot];
        view.cancel_load();
        let (_, rx) = std::sync::mpsc::channel();
        drop(view.show(
            views::sample_data(path.to_str().unwrap(), &["M", "N"]),
            rx,
            slot + 1,
            1.5,
            0.0,
        ));
    }
    frame_with(&ctx, &mut app, vec![]);
    assert!(app.window_loads().tabs[0]
        .views
        .iter()
        .all(|view| view.state == Some(LoadState::Ready)));
    for view in &app.views {
        assert_eq!(view.pb.frame, 3);
        assert_eq!(view.selected.len(), 2);
        assert_eq!(view.primary, Some(1));
    }
    let before_switch = app.window_tabs();
    assert_eq!(
        before_switch[0]
            .views
            .iter()
            .map(|v| Path::new(&v.file))
            .collect::<Vec<_>>(),
        named.iter().map(PathBuf::as_path).collect::<Vec<_>>()
    );
    app.new_layout_tab();
    assert_eq!(
        app.window_tabs()[0],
        before_switch[0],
        "inactive tabs keep their loaded files"
    );

    app.others_open = true;
    app.save(&mut storage);
    storage.flush();
    assert_eq!(storage.get_string(eframe::APP_KEY), saved_bytes);
    assert_eq!(storage.writes, 1);
    let reopened = TakViewerApp::build(&ctx, Some(&storage), vec![], None, None);
    assert_eq!(reopened.layout_tabs.tabs().len(), 2);
    assert_eq!(
        reopened.layout_tabs.active().unwrap().name,
        "Previous layout"
    );
    assert_eq!(reopened.views[2].target(), Some(old[0].as_path()));
}

#[test]
fn only_closing_last_keeps_newest_normal_save_and_reads_legacy_ron() {
    let dir = tak_core::test_support::TempDir::new("tak_viewer_only_last");
    let named = take_files(&dir, &["only.tak"]);
    let mut storage = FileLike::default();
    let old =
        r#"(views:[(0,"a.tak")],active_view:0,workspace:Some("old workspace"),graph_opacity:47)"#;
    storage.set_string(eframe::APP_KEY, old.into());
    storage.set_string("window", "saved window".into());
    storage.set_string("egui", "saved UI memory".into());
    storage.flush();

    let ctx = egui::Context::default();
    let mut only = TakViewerApp::build_window(
        &ctx,
        Some(&storage),
        vec![],
        None,
        None,
        Some(vec![startup_tab("tab1", &named, StartupView::default())]),
    );
    assert_eq!(only.settings.graph_opacity, 47);
    assert_eq!(only.settings.workspace_roots, ["old workspace"]);
    assert_eq!(only.settings.views, [(0, "a.tak".into())]);
    assert_eq!(only.auto_save_interval(), Duration::MAX);
    assert!(!only.persist_egui_memory());
    assert!(
        !only.no_persist,
        "normal edit and undo paths remain enabled"
    );

    let mut normal = TakViewerApp::build(&ctx, Some(&storage), vec![], None, None);
    let latest = take_files(&dir, &["latest.tak"]);
    normal.open(&ctx, latest[0].clone());
    normal.views[0].cancel_load();
    let (_, rx) = std::sync::mpsc::channel();
    drop(normal.views[0].show(
        views::sample_data(latest[0].to_str().unwrap(), &["M"]),
        rx,
        1,
        1.0,
        0.0,
    ));
    normal.settings.graph_opacity = 63;
    normal.others_open = true;
    assert_eq!(normal.auto_save_interval(), Duration::MAX);
    normal.save(&mut storage);
    storage.flush();
    let saved = storage.kv.clone();
    drop(normal);

    only.others_open = false;
    only.settings.graph_opacity = 11;
    only.save(&mut storage);
    storage.flush();
    drop(only);
    assert_eq!(storage.kv, saved);
    assert_eq!(storage.writes, 2);
    let reopened = TakViewerApp::build(&ctx, Some(&storage), vec![], None, None);
    assert_eq!(reopened.views[0].target(), Some(latest[0].as_path()));
    assert_eq!(reopened.settings.graph_opacity, 63);
}

#[test]
fn only_picks_a_loaded_comment_once_and_explicit_frame_and_marker_win() {
    let dir = comment_dir("only_comment");
    two_comments_on_disk(&dir);
    let path = dir.join("a.takm");
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build_window(
        &ctx,
        None,
        vec![],
        None,
        None,
        Some(vec![startup_tab(
            "tab1",
            std::slice::from_ref(&path),
            StartupView {
                frame: Some(6),
                select: Some("N".into()),
                comments: true,
                pick_comment: Some(2),
            },
        )]),
    );
    let view = &mut app.views[0];
    view.cancel_load();
    let (_, rx) = std::sync::mpsc::channel();
    drop(view.show(
        views::sample_data(path.to_str().unwrap(), &["M", "N"]),
        rx,
        1,
        1.0,
        0.0,
    ));
    app.apply_startup_views(0.0);
    assert_eq!(launch_state(&app, 0, 0), Some(LoadState::Loading));
    assert_eq!(app.views[0].picked, None);

    let mut loaded = views::CommentsLoad::read(&path);
    // This test consumes the comments without requesting a writable ID pool.
    loaded.target = Err("fixture".into());
    app.views[0].comments.take_load(loaded);
    app.apply_startup_views(0.0);
    assert_eq!(launch_state(&app, 0, 0), Some(LoadState::Ready));
    let view = &mut app.views[0];
    assert_eq!(view.picked, Some(2));
    assert_eq!(view.pb.frame, 6);
    assert_eq!(view.selected, BTreeSet::from([1]));
    view.pb.set_frame(7, 0.0);
    app.apply_startup_views(0.0);
    assert_eq!(app.views[0].pb.frame, 7);
}

#[test]
fn only_stages_hidden_tabs_and_applies_their_startup_state_without_switching() {
    let ctx = egui::Context::default();
    let dir = comment_dir("only_hidden_tabs");
    two_comments_on_disk(&dir);
    let files = take_files(&dir, &["first.tak", "third.tak"]);
    let edited = dir.join("a.takm");
    let mut app = TakViewerApp::build_window(
        &ctx,
        None,
        vec![],
        None,
        None,
        Some(vec![
            startup_tab(
                "첫 탭",
                &files[..1],
                StartupView {
                    frame: Some(4),
                    select: Some("M,N".into()),
                    ..Default::default()
                },
            ),
            startup_tab(
                "비교",
                &[edited.clone(), files[0].clone()],
                StartupView {
                    frame: Some(7),
                    select: Some("N".into()),
                    pick_comment: Some(2),
                    comments: true,
                },
            ),
            startup_tab("마지막", &files[1..], StartupView::default()),
        ]),
    );
    let ids: Vec<_> = app.layout_tabs.tabs().iter().map(|tab| tab.id).collect();
    assert_eq!(app.layout_tabs.active_id(), Some(ids[0]));
    assert_eq!(
        app.layout_tabs
            .tabs()
            .iter()
            .map(|tab| tab.name.as_str())
            .collect::<Vec<_>>(),
        ["첫 탭", "비교", "마지막"]
    );
    let queued = &app
        .layout_tabs
        .tab(ids[1])
        .unwrap()
        .layout
        .as_ref()
        .unwrap()
        .views;
    assert_eq!(queued[0].target(), Some(edited.as_path()));
    assert_eq!(queued[1].target(), Some(files[0].as_path()));
    assert!(
        queued.iter().all(|view| view.loading.is_none()),
        "hidden reads must wait"
    );

    let inject = |view: &mut View, path: &Path| {
        view.held
            .as_ref()
            .unwrap()
            .send(views::LoadMsg::Markers(Ok((
                views::sample_data(path.to_str().unwrap(), &["M", "N"]),
                vec![],
            ))))
            .unwrap();
    };
    // The first owner publishes marker data and the absent-Comments result to its duplicate.
    inject(&mut app.views[0], &files[0]);
    let (sender, receiver) = std::sync::mpsc::channel();
    sender.send(views::CommentsLoad::read(&files[0])).unwrap();
    app.views[0].comments_rx = Some(receiver);
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(app.views[0].pb.frame, 4);
    assert_eq!(app.views[0].selected, BTreeSet::from([0, 1]));
    assert_eq!(launch_state(&app, 0, 0), Some(LoadState::Ready));
    assert!(app.window_loads().first_tab_draw_ms.is_some());
    assert!(app
        .layout_tabs
        .tab(ids[1])
        .unwrap()
        .layout
        .as_ref()
        .unwrap()
        .views[0]
        .loading
        .is_none());
    frame_with(&ctx, &mut app, vec![]);
    assert!(app
        .layout_tabs
        .tab(ids[2])
        .unwrap()
        .layout
        .as_ref()
        .unwrap()
        .views[0]
        .loading
        .is_none());

    let runtime = app
        .layout_tabs
        .tab_mut(ids[1])
        .unwrap()
        .layout
        .as_mut()
        .unwrap();
    inject(&mut runtime.views[0], &edited);
    let (sender, receiver) = std::sync::mpsc::channel();
    runtime.views[0].comments_rx = Some(receiver);
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(
        launch_state(&app, 1, 0),
        Some(LoadState::Loading),
        "wait for requested Comment"
    );
    let mut comments = views::CommentsLoad::read(&edited);
    comments.target = Err("fixture".into());
    sender.send(comments).unwrap();
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(launch_state(&app, 1, 0), Some(LoadState::Ready));
    assert_eq!(launch_state(&app, 1, 1), Some(LoadState::Ready));
    assert_eq!(app.layout_tabs.active_id(), Some(ids[0]));
    let hidden = &app
        .layout_tabs
        .tab(ids[1])
        .unwrap()
        .layout
        .as_ref()
        .unwrap()
        .views;
    assert_eq!(hidden[0].picked, Some(2));
    assert_eq!(hidden[0].pb.frame, 7);
    assert_eq!(hidden[0].selected, BTreeSet::from([1]));
    assert_eq!(hidden[1].pb.frame, 7);
    assert_eq!(hidden[1].selected, BTreeSet::from([1]));
    assert!(std::sync::Arc::ptr_eq(
        hidden[1].data.as_ref().unwrap(),
        app.views[0].data.as_ref().unwrap()
    ));

    assert!(app.switch_layout_tab_internal(ids[1]));
    frame_with(&ctx, &mut app, vec![]);
    assert!(dock::has(&app.dock, Pane::Comments));
    assert!(
        app.history.kinds().is_empty(),
        "startup state adds no undo action"
    );
}

#[test]
fn a_failed_launch_file_keeps_its_reason_after_its_slot_disappears() {
    let ctx = egui::Context::default();
    let dir = tak_core::test_support::TempDir::new("tak_viewer_only_failure");
    let paths = take_files(&dir, &["good.tak", "bad.tak"]);
    let mut app = TakViewerApp::build_window(
        &ctx,
        None,
        vec![],
        None,
        None,
        Some(vec![
            startup_tab("first", &paths, StartupView::default()),
            startup_tab("duplicate", &paths[1..], StartupView::default()),
        ]),
    );
    app.views[1]
        .held
        .as_ref()
        .unwrap()
        .send(views::LoadMsg::Markers(Err("fixture read failed".into())))
        .unwrap();
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(app.open_views(), [0]);
    let loads = app.window_loads();
    for (tab, slot) in [(0, 1), (1, 0)] {
        let file = &loads.tabs[tab].views[slot];
        assert_eq!(Path::new(&file.file), paths[1]);
        assert_eq!(file.state, Some(LoadState::Failed));
        assert_eq!(file.error.as_deref(), Some("fixture read failed"));
    }
}

#[test]
#[ignore = "needs twelve real TAK copies in TAK_VIEWER_STARTUP_BENCH_DIR"]
fn only_startup_first_marker_frame_benchmark() {
    let root = std::env::var("TAK_VIEWER_STARTUP_BENCH_DIR").expect("benchmark input folder");
    let root = workspace::resolve_path(&root);
    let files: Vec<_> = (1..=12).map(|n| root.join(format!("{n:02}.tak"))).collect();
    for count in [4, 12, 12, 4, 4, 12] {
        let tabs: Vec<_> = files[..count]
            .chunks(MAX_VIEWS)
            .enumerate()
            .map(|(n, files)| startup_tab(&format!("tab{}", n + 1), files, StartupView::default()))
            .collect();
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let started = Instant::now();
        app.only = true;
        app.prepare_launch_tabs(&ctx, tabs, started);
        while started.elapsed() < Duration::from_secs(120) {
            frame_with(&ctx, &mut app, vec![]);
            let loads = app.window_loads();
            assert!(
                !loads
                    .tabs
                    .iter()
                    .flat_map(|tab| &tab.views)
                    .any(|view| view.state == Some(LoadState::Failed)),
                "{:?}",
                loads.tabs
            );
            let all_ready = loads
                .tabs
                .iter()
                .flat_map(|tab| &tab.views)
                .all(|view| view.state == Some(LoadState::Ready));
            if all_ready {
                let ms = loads.first_tab_draw_ms.expect("first marker frame");
                let all_ms = started.elapsed().as_secs_f64() * 1000.0;
                println!(
                    "files={count} tabs={} first_tab_draw_ms={ms:.2} all_ready_ms={all_ms:.2}",
                    count / MAX_VIEWS
                );
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(120), "{loads:?}");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app
            .window_loads()
            .tabs
            .iter()
            .flat_map(|tab| &tab.views)
            .all(|view| view.state == Some(LoadState::Ready)));
    }
}
