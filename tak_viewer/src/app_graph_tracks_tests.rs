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

fn tracks_setup(ctx: &egui::Context) -> (TakViewerApp, egui::FullOutput) {
    let mut app = one_view(ctx);
    app.set_graph_float(false);
    app.settings.graph_layout = graph::tracks::Layout::Tracks;
    // Prepare synchronously for this input test. The product prepares the same index lazily.
    app.views[0].data.as_ref().unwrap().prepare_marker_order();
    app.views[0].pb.set_frame(5, 0.0);
    app.views[0].select(0, false);
    app.views[0].range = Some((1, 3));
    app.quiet();
    for _ in 0..3 {
        frame_with(ctx, &mut app, vec![]);
    }
    let out = frame_with(ctx, &mut app, vec![]);
    (app, out)
}

fn tracks_body_and_band(out: &egui::FullOutput) -> (egui::Rect, egui::Rect) {
    let green = egui::Color32::from_rgb(0x48, 0xb4, 0x67);
    let selected = egui::Color32::from_rgb(0xf8, 0xce, 0x46);
    let bar = graph_selection_fill_rects(out, green)
        .into_iter()
        .chain(graph_selection_fill_rects(out, selected))
        .next()
        .expect("a Tracks presence bar");
    let body = graph_selection_fill_rects(out, theme::GRAPH_BG)
        .into_iter()
        .find(|r| r.contains(bar.center()))
        .unwrap();
    let band = graph_selection_fill_rects(out, theme::BAND_BG)
        .into_iter()
        .find(|r| (r.right() - body.right()).abs() < 1.0 && r.top() > body.bottom())
        .unwrap();
    (body, band)
}

fn tracks_frame_x(band: egui::Rect, first: i64, last: i64, frame: i64) -> f32 {
    band.left() + band.width() * ((frame - first) as f32 + 0.5) / (last - first + 1) as f32
}

#[test]
fn tracks_row_click_keeps_range_and_frame_and_uses_selection_history() {
    let ctx = egui::Context::default();
    let (mut app, out) = tracks_setup(&ctx);
    let presence_green = egui::Color32::from_rgb(0x48, 0xb4, 0x67);
    let bar = graph_selection_fill_rects(&out, presence_green)
        .first()
        .copied()
        .expect("unselected Tracks row");
    click_at(&ctx, &mut app, bar.center());
    assert_eq!(app.views[0].ordered_selection(), [1]);
    assert_eq!(app.views[0].pb.frame, 5);
    assert_eq!(app.views[0].range, Some((1, 3)));
    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].ordered_selection(), [0]);
    redo_key(&ctx, &mut app);
    assert_eq!(app.views[0].ordered_selection(), [1]);
}

#[test]
fn tracks_row_drag_is_live_preserves_selection_and_shares_one_range() {
    let ctx = egui::Context::default();
    let (mut app, out) = tracks_setup(&ctx);
    let (body, band) = tracks_body_and_band(&out);
    let green = egui::Color32::from_rgb(0x48, 0xb4, 0x67);
    let y = graph_selection_fill_rects(&out, green)[0].center().y;
    let point = |f| egui::pos2(tracks_frame_x(band, 0, 9, f), y);
    let from = point(7);
    let to = point(2);
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(from)]);
    frame_with(&ctx, &mut app, vec![graph_selection_primary(from, true)]);
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(from - egui::vec2(4.0, 0.0))]);
    assert_eq!(app.view().range, Some((1, 3)), "4 pt is still below the drag threshold");
    let live = frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(to)]);
    assert_eq!((app.view().range, app.view().pb.frame), (Some((2, 7)), 5));
    assert_eq!(steps(&app, 0), (0, 0));
    assert_eq!(app.view().ordered_selection(), [0], "drag started over the unselected row");
    let yellow = graph_selection_fill_rects(&live, theme::RANGE.gamma_multiply(0.12));
    assert!(yellow.iter().any(|r| r.top() == body.top() && r.bottom() == body.bottom()));
    assert!(yellow.iter().any(|r| r.top() == band.top() && r.bottom() == band.bottom()));
    assert_eq!(graph_selection_fill_rects(&live, theme::RANGE.gamma_multiply(0.18)).len(), 1);
    frame_with(&ctx, &mut app, vec![graph_selection_primary(to, false)]);
    assert_eq!(app.history.kinds(), ["Range"]);
    assert_eq!(app.view().ordered_selection(), [0]);
    graph_selection_capture(&ctx, &mut app, "TAK_TRACKS_SELECTION_CAPTURE");

    undo_key(&ctx, &mut app);
    assert_eq!((app.view().range, app.view().pb.frame), (Some((1, 3)), 5));
    redo_key(&ctx, &mut app);
    assert_eq!((app.view().range, app.view().pb.frame), (Some((2, 7)), 5));

    app.settings.graph_layout = graph::tracks::Layout::Channels;
    wait_marker_graphs(&ctx, &mut app);
    let out = frame_with(&ctx, &mut app, vec![]);
    let yellow = graph_selection_fill_rects(&out, theme::RANGE.gamma_multiply(0.12));
    assert_eq!(yellow.len(), 1, "Channels paints the Tracks range");
    assert_eq!(app.view().range, Some((2, 7)));
    let (from, to) = (graph_selection_point(&app, 4.0), graph_selection_point(&app, 8.0));
    drag(&ctx, &mut app, from, to);
    app.settings.graph_layout = graph::tracks::Layout::Tracks;
    let out = frame_with(&ctx, &mut app, vec![]);
    assert_eq!(graph_selection_fill_rects(&out, theme::RANGE.gamma_multiply(0.12)).len(), 2);
    assert_eq!((app.view().range, app.view().pb.frame), (Some((4, 8)), 5));
}

#[test]
fn tracks_name_column_drag_and_vertical_motion_keep_range_and_selection() {
    let ctx = egui::Context::default();
    let (mut app, out) = tracks_setup(&ctx);
    let (body, band) = tracks_body_and_band(&out);
    let y = body.top() + 9.5;
    let left = egui::pos2((body.left() + band.left()) / 2.0, y);
    let time = egui::pos2(tracks_frame_x(band, 0, 9, 3), y);
    drag(&ctx, &mut app, left, time);
    drag(&ctx, &mut app, time, time + egui::vec2(0.0, 40.0));
    assert_eq!((app.view().range, app.view().pb.frame), (Some((1, 3)), 5));
    assert_eq!(app.view().ordered_selection(), [0]);
    assert_eq!(steps(&app, 0), (0, 0));
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
    let (_, band) = tracks_body_and_band(&out);
    let x = |frame| tracks_frame_x(band, 0, 9, frame);
    drag(
        &ctx,
        &mut app,
        egui::pos2(x(2), band.center().y),
        egui::pos2(x(7), band.center().y),
    );
    assert_eq!(
        (app.views[0].range, app.views[0].pb.frame),
        (Some((2, 7)), 5)
    );
    let tick_y = band.top() - graph::NAV_H / 2.0;
    click_at(&ctx, &mut app, egui::pos2(x(3), tick_y));
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
