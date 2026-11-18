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
        named.clone(),
        None,
        None,
        Some(StartupView {
            frame: Some(3),
            select: Some("M,N".into()),
            comments: true,
            ..Default::default()
        }),
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
    assert!(app.startup_views.is_none());
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
        named,
        None,
        None,
        Some(StartupView::default()),
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
        vec![path.clone()],
        None,
        None,
        Some(StartupView {
            frame: Some(6),
            select: Some("N".into()),
            comments: true,
            pick_comment: Some(2),
        }),
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
    assert!(app.startup_views.is_some());
    assert_eq!(app.views[0].picked, None);

    let mut loaded = views::CommentsLoad::read(&path);
    // This test consumes the comments without requesting a writable ID pool.
    loaded.target = Err("fixture".into());
    app.views[0].comments.take_load(loaded);
    app.apply_startup_views(0.0);
    assert!(app.startup_views.is_none());
    let view = &mut app.views[0];
    assert_eq!(view.picked, Some(2));
    assert_eq!(view.pb.frame, 6);
    assert_eq!(view.selected, BTreeSet::from([1]));
    view.pb.set_frame(7, 0.0);
    app.apply_startup_views(0.0);
    assert_eq!(app.views[0].pb.frame, 7);
}
