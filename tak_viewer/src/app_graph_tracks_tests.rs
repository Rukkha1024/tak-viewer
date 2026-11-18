#[test]
fn tracks_settings_migrate_and_layout_choice_round_trips_through_undo() {
    use graph::tracks::Layout;
    let old: Settings =
        ron::from_str("(graph_float:false,graph_opacity:37,workspace_roots:[\"my-workspace\"])")
            .unwrap();
    assert_eq!(old.graph_layout, Layout::Channels);
    assert_eq!(old.graph_opacity, 37);
    assert_eq!(old.workspace_roots, ["my-workspace"]);
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.set_graph_float(false);
    app.quiet();
    app.settings.graph_layout = Layout::Tracks;
    let text = ron::to_string(&app.settings).unwrap();
    assert_eq!(
        ron::from_str::<Settings>(&text).unwrap().graph_layout,
        Layout::Tracks
    );
    app.observe(true);
    undo_key(&ctx, &mut app);
    assert_eq!(app.settings.graph_layout, Layout::Channels);
    redo_key(&ctx, &mut app);
    assert_eq!(app.settings.graph_layout, Layout::Tracks);
}

#[test]
fn tracks_row_click_uses_shared_selection_and_history_without_seeking() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.set_graph_float(false);
    app.settings.graph_layout = graph::tracks::Layout::Tracks;
    // Prepare synchronously for this input test. The product prepares the same index lazily.
    app.views[0].data.as_ref().unwrap().prepare_marker_order();
    app.views[0].pb.set_frame(5, 0.0);
    app.views[0].select(0, false);
    app.quiet();
    let mut out = frame_with(&ctx, &mut app, vec![]);
    for _ in 0..3 {
        out = frame_with(&ctx, &mut app, vec![]);
    }
    let presence_green = egui::Color32::from_rgb(0x48, 0xb4, 0x67);
    let bar = graph_selection_fill_rects(&out, presence_green)
        .first()
        .copied()
        .expect("unselected Tracks row");
    click_at(&ctx, &mut app, bar.center());
    assert_eq!(app.views[0].ordered_selection(), [1]);
    assert_eq!(app.views[0].pb.frame, 5);
    assert!(app.views[0].range.is_none());
    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].ordered_selection(), [0]);
    redo_key(&ctx, &mut app);
    assert_eq!(app.views[0].ordered_selection(), [1]);
}

#[test]
fn tracks_tick_strip_seeks_and_lower_band_changes_only_the_edit_range() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.set_graph_float(false);
    app.settings.graph_layout = graph::tracks::Layout::Tracks;
    app.views[0].data.as_ref().unwrap().prepare_marker_order();
    app.views[0].pb.set_frame(5, 0.0);
    app.quiet();
    for _ in 0..3 {
        frame_with(&ctx, &mut app, vec![]);
    }
    let out = frame_with(&ctx, &mut app, vec![]);
    let green = egui::Color32::from_rgb(0x48, 0xb4, 0x67);
    let bar = graph_selection_fill_rects(&out, green)[0];
    let body = graph_selection_fill_rects(&out, theme::GRAPH_BG)
        .into_iter()
        .find(|r| r.contains(bar.center()))
        .unwrap();
    let band = graph_selection_fill_rects(&out, theme::BAND_BG)
        .into_iter()
        .find(|r| (r.left() - bar.left()).abs() < 1.0 && r.top() > body.bottom())
        .unwrap();
    let x = |frame: f32| band.left() + band.width() * (frame + 0.5) / 10.0;
    drag(
        &ctx,
        &mut app,
        egui::pos2(x(2.0), band.center().y),
        egui::pos2(x(7.0), band.center().y),
    );
    assert_eq!(
        (app.views[0].range, app.views[0].pb.frame),
        (Some((2, 7)), 5)
    );
    let tick_y = band.top() - graph::NAV_H / 2.0;
    click_at(&ctx, &mut app, egui::pos2(x(3.0), tick_y));
    assert_eq!(
        (app.views[0].range, app.views[0].pb.frame),
        (Some((2, 7)), 3)
    );
    undo_key(&ctx, &mut app);
    assert_eq!(
        (app.views[0].range, app.views[0].pb.frame),
        (Some((2, 7)), 5)
    );
    undo_key(&ctx, &mut app);
    assert_eq!((app.views[0].range, app.views[0].pb.frame), (None, 5));
}

#[test]
fn tracks_row_click_moves_ctrl_a_focus_out_of_the_3d_view() {
    let ctx = egui::Context::default();
    let mut app = scene_selection_app(&ctx);
    app.set_graph_float(false);
    app.settings.graph_layout = graph::tracks::Layout::Tracks;
    app.views[0].data.as_ref().unwrap().prepare_marker_order();
    frame_with(&ctx, &mut app, vec![]);
    let point = marker_on_screen(&app, 0);
    click_at(&ctx, &mut app, point);
    assert_eq!(key_focus::target(&ctx), key_focus::Target::Scene(0));

    let out = frame_with(&ctx, &mut app, vec![]);
    let green = egui::Color32::from_rgb(0x48, 0xb4, 0x67);
    let row = graph_selection_fill_rects(&out, green)[0].center();
    click_at(&ctx, &mut app, row);
    assert_eq!(key_focus::target(&ctx), key_focus::Target::Graph(0));
    assert_eq!(app.views[0].ordered_selection(), [1]);
    frame_with(&ctx, &mut app, vec![key_event(Key::A, true, ctrl())]);
    assert_eq!(app.views[0].ordered_selection(), [1]);
}
