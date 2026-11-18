fn graph_convenience_tap(ctx: &egui::Context, app: &mut TakViewerApp, key: Key, modifiers: Modifiers) {
    frame_with(ctx, app, vec![key_event(key, true, modifiers), key_event(key, false, modifiers)]);
}

#[test]
fn graph_fit_uses_last_clicked_plot_and_exact_keys() {
    use graph::tracks::{zoom_extent, Layout};
    for (layout, detached) in [(Layout::Channels, false), (Layout::Combined, true), (Layout::Tracks, false)] {
        let ctx = egui::Context::default();
        let mut app = graph_selection_setup(&ctx, detached);
        app.settings.graph_layout = layout;
        let id = app.views[0].file_id;
        let shot = frame_with(&ctx, &mut app, vec![]);
        if layout == Layout::Combined {
            assert!(text_rects(&shot, "Combined").len() >= 1);
        }
        let (surface, node, _) = app.dock.find_tab(&Pane::Graph).unwrap();
        let plot = app.dock[surface][node].rect().unwrap().center();
        let full = Some((-0.5, 9.5));
        app.views[0].set_range(3, 5);
        frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(plot)]);
        graph_convenience_tap(&ctx, &mut app, Key::F, Modifiers::NONE);
        assert_eq!(zoom_extent(&ctx, id), full, "{layout:?} hover cannot claim F");
        let mut click = delete_focus_click(plot);
        click.push(key_event(Key::F, true, Modifiers::NONE));
        click.push(key_event(Key::F, false, Modifiers::NONE));
        frame_with(&ctx, &mut app, click);
        assert_eq!(key_focus::target(&ctx), key_focus::Target::Graph(0));
        assert_eq!(zoom_extent(&ctx, id), Some((2.5, 5.5)));

        let scene = app.views[0].scene_rect.left_center() + egui::vec2(20.0, 0.0);
        frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(scene)]);
        let camera = app.views[0].cam;
        let ctrl_shift = Modifiers::SHIFT | Modifiers::CTRL;
        graph_convenience_tap(&ctx, &mut app, Key::F, ctrl_shift);
        assert_eq!(zoom_extent(&ctx, id), Some((2.5, 5.5)));
        app.views[0].range = None;
        graph_convenience_tap(&ctx, &mut app, Key::F, Modifiers::NONE);
        assert_eq!(zoom_extent(&ctx, id), Some((2.5, 5.5)), "no edit range");

        app.views[0].graph_view.zoom(false).magnify(4.0);
        let frame = app.views[0].pb.frame;
        frame_with(&ctx, &mut app, vec![key_event(Key::M, true, Modifiers::NONE),
            key_event(Key::F, true, Modifiers::SHIFT), key_event(Key::F, false, Modifiers::SHIFT)]);
        assert_eq!(zoom_extent(&ctx, id), Some((2.5, 5.5)), "held M owns input");
        frame_with(&ctx, &mut app, vec![key_event(Key::Escape, true, Modifiers::NONE),
            key_event(Key::M, false, Modifiers::NONE)]);
        graph_convenience_tap(&ctx, &mut app, Key::F, Modifiers::SHIFT);
        assert_eq!(zoom_extent(&ctx, id), full);
        assert_eq!(app.views[0].cam, camera, "leaving the graph with the pointer keeps its focus");
        assert_eq!(app.views[0].pb.frame, frame);
        assert!(app.views[0].graph_view.zoom(false).is_auto());
    }
}

#[test]
fn graph_lock_button_captures_selection_and_undo_and_redo_restore_it() {
    let ctx = egui::Context::default();
    let mut app = graph_selection_setup(&ctx, false);
    app.views[0].clear_selection();
    let out = frame_with(&ctx, &mut app, vec![]);
    let button = text_rects(&out, "Lock Selection")[0].center();
    click_at(&ctx, &mut app, button);
    assert!(app.views[0].graph_view.lock.is_none(), "empty selection cannot lock");

    app.views[0].select(0, false);
    frame_with(&ctx, &mut app, vec![]);
    click_at(&ctx, &mut app, button);
    assert!(app.views[0].graph_view.lock.is_some());
    assert_eq!(app.history.kinds().last().map(String::as_str), Some("GraphLock"));
    app.views[0].select(1, false);
    app.quiet();
    let out = frame_with(&ctx, &mut app, vec![]);
    let graph = place_of(&app, Pane::Graph);
    let (first, second) = {
        let data = app.views[0].data.as_ref().unwrap();
        (data.markers[0].name.clone(), data.markers[1].name.clone())
    };
    let shown = |out: &egui::FullOutput, name| text_rects(out, name).iter().any(|r| graph.contains(r.center()));
    assert!(shown(&out, &first) && !shown(&out, &second));
    graph_convenience_tap(&ctx, &mut app, Key::Z, ctrl());
    let out = frame_with(&ctx, &mut app, vec![]);
    assert!(!shown(&out, &first) && shown(&out, &second));
    graph_convenience_tap(&ctx, &mut app, Key::Y, ctrl());
    let out = frame_with(&ctx, &mut app, vec![]);
    assert!(shown(&out, &first) && !shown(&out, &second));

    click_at(&ctx, &mut app, button);
    assert!(app.views[0].graph_view.lock.is_none(), "second click follows the scene again");
    graph_convenience_tap(&ctx, &mut app, Key::Z, ctrl());
    assert!(app.views[0].graph_view.lock.is_some());
    app.views[0].graph_view.zoom(false).magnify(8.0);
    let (_tx, rx) = std::sync::mpsc::channel();
    app.views[0].show(views::sample_data("new-graph-file.tak", &["A", "B"]), rx, 100, 1.0, 0.0);
    assert!(app.views[0].graph_view.lock.is_none());
    assert!(app.views[0].graph_view.zoom(false).is_auto());
}

#[test]
fn combined_layout_reads_and_writes_through_settings_and_history() {
    let ctx = egui::Context::default();
    let mut app = graph_selection_setup(&ctx, false);
    app.settings.graph_layout = graph::tracks::Layout::Combined;
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(ron::from_str::<Settings>(&ron::to_string(&app.settings).unwrap()).unwrap().graph_layout,
        graph::tracks::Layout::Combined);
    graph_convenience_tap(&ctx, &mut app, Key::Z, ctrl());
    assert_eq!(app.settings.graph_layout, graph::tracks::Layout::Channels);
    graph_convenience_tap(&ctx, &mut app, Key::Y, ctrl());
    assert_eq!(app.settings.graph_layout, graph::tracks::Layout::Combined);
}

#[test]
fn graph_value_drag_changes_only_the_pressed_lane_and_toolbar_resets_it() {
    let ctx = egui::Context::default();
    let mut app = graph_selection_setup(&ctx, false);
    let before = app.views[0].lanes.drew().ranges.clone();
    let at = graph_selection_point(&app, 5.0);
    let down = |pos, pressed| egui::Event::PointerButton {
        pos, pressed, button: egui::PointerButton::Secondary, modifiers: Modifiers::NONE,
    };
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(at), down(at, true)]);
    let source_builds = app.views[0].lanes.builds;
    let mut above = at;
    let mut last_range = before[1];
    for dy in [22.0, 13.0, -5.0] {
        above.y -= dy;
        frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(above)]);
        let drew = app.views[0].lanes.drew();
        assert_ne!(drew.ranges[1], last_range, "every move changes the live value extent");
        assert_eq!(drew.remade, 0, "drag reuses the prepared curve shapes");
        assert_eq!(app.views[0].lanes.builds, source_builds);
        last_range = drew.ranges[1];
    }
    let out = frame_with(&ctx, &mut app, vec![down(above, false)]);
    assert_eq!(app.views[0].lanes.drew().remade, 1, "release prepares exact shapes once for the dragged lane");
    let after = &app.views[0].lanes.drew().ranges;
    assert_eq!(after[0], before[0]);
    assert_eq!(after[2], before[2]);
    assert_ne!(after[1], before[1]);
    assert_eq!(app.views[0].range, None);
    let extent = text_rects(&out, "Auto Extents")[0].center();
    click_at(&ctx, &mut app, extent);
    assert_eq!(app.views[0].lanes.drew().ranges, before);
    assert!(app.views[0].graph_view.zoom(false).is_auto());
    assert_eq!(app.views[0].lanes.builds, source_builds, "reset reuses the source curves");

    app.views[0].set_range(4, 4);
    let out = frame_with(&ctx, &mut app, vec![]);
    click_at(&ctx, &mut app, text_rects(&out, "Zoom Fit")[0].center());
    assert_eq!(graph::tracks::zoom_extent(&ctx, app.views[0].file_id), Some((3.5, 4.5)));
    assert_eq!(app.views[0].range, Some((4, 4)));
}

#[test]
fn graph_tracks_wheel_scrolls_rows_without_changing_time_extent() {
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    let names: Vec<_> = (0..80).map(|i| format!("Track {i:02}")).collect();
    let refs: Vec<_> = names.iter().map(String::as_str).collect();
    let (_tx, rx) = std::sync::mpsc::channel();
    app.views[0].show(views::sample_data("scroll.tak", &refs), rx, 10, 1.0, 0.0);
    app.settings.graph_layout = graph::tracks::Layout::Tracks;
    app.set_graph_float(false);
    let file_id = app.views[0].file_id;
    let plot = graph::tracks::graph_id(file_id).with("tracks_body");
    let wait = std::time::Instant::now();
    let body = loop {
        let out = frame_with(&ctx, &mut app, vec![]);
        if !text_rects(&out, &names[0]).is_empty() {
            break ctx.read_response(plot).unwrap().interact_rect;
        }
        assert!(wait.elapsed() < std::time::Duration::from_secs(3));
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    let extent = graph::tracks::zoom_extent(&ctx, file_id);
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(body.center())]);
    frame_with(&ctx, &mut app, vec![egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -620.0), modifiers: Modifiers::NONE,
    }]);
    let mut out = frame_with(&ctx, &mut app, vec![]);
    let later_row_visible = |out: &egui::FullOutput| names[20..].iter().any(|name|
        text_rects(out, name).iter().any(|rect| body.contains(rect.center())));
    for _ in 0..12 {
        if later_row_visible(&out) { break; }
        out = frame_with(&ctx, &mut app, vec![]);
    }
    assert!(text_rects(&out, &names[0]).iter().all(|rect| !body.contains(rect.center())));
    assert!(later_row_visible(&out));
    assert_eq!(graph::tracks::zoom_extent(&ctx, file_id), extent);
}

#[test]
fn tracks_toolbar_shows_zoom_fit_in_both_graph_positions() {
    for detached in [false, true] {
        let ctx = egui::Context::default();
        let mut app = graph_selection_setup(&ctx, detached);
        app.settings.graph_layout = graph::tracks::Layout::Tracks;
        app.views[0].set_range(3, 5);
        let out = frame_with(&ctx, &mut app, vec![]);
        assert_eq!(text_rects(&out, "Zoom Fit").len(), 1, "detached {detached}");
    }
}

#[test]
fn graph_fit_keeps_orthographic_camera_and_scene_fit_keeps_graph_extent() {
    let ctx = egui::Context::default();
    let mut app = graph_selection_setup(&ctx, false);
    let view = &mut app.views[0];
    view.cam.preset = crate::camera::Preset::Top;
    view.cam.distance = 20.0;
    view.pb.set_working_range(0, 2, 0.0);
    view.set_range(3, 5);
    let id = view.file_id;
    frame_with(&ctx, &mut app, vec![]);

    let camera = app.views[0].cam;
    let graph = place_of(&app, Pane::Graph).center();
    click_at(&ctx, &mut app, graph);
    graph_convenience_tap(&ctx, &mut app, Key::F, Modifiers::NONE);
    assert_eq!(app.views[0].cam, camera);
    let extent = graph::tracks::zoom_extent(&ctx, id);
    assert_eq!(extent, Some((2.5, 5.5)));

    let scene = app.views[0].scene_rect.left_center() + egui::vec2(20.0, 0.0);
    click_at(&ctx, &mut app, scene);
    graph_convenience_tap(&ctx, &mut app, Key::F, Modifiers::SHIFT);
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(app.views[0].cam.preset, crate::camera::Preset::Top);
    assert_ne!(app.views[0].cam.distance, camera.distance);
    assert_eq!(graph::tracks::zoom_extent(&ctx, id), extent);
}
