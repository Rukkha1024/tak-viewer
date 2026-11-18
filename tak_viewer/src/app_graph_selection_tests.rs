// Included in app::tests to exercise the actual docked / detached panes, playback band,
// per-view range and undo history with the same input events as a person.
fn graph_selection_setup(ctx: &egui::Context, detached: bool) -> TakViewerApp {
    let mut app = one_view(ctx);
    app.views[0].select(0, false);
    app.views[0].select(1, true);
    app.views[0].pb.set_frame(5, 0.0);
    app.set_graph_float(false);
    if detached {
        dock::hide(&mut app.dock, Pane::Graph);
        let window = app.dock.add_window(vec![Pane::Graph]);
        app.dock.get_window_state_mut(window).unwrap()
            .set_position(egui::pos2(300.0, 150.0))
            .set_size(egui::vec2(800.0, 400.0));
    }
    wait_marker_graphs(ctx, &mut app);
    app.quiet();
    app.history.undo.clear();
    app.history.redo.clear();
    app
}

fn graph_selection_point(app: &TakViewerApp, frame: f64) -> egui::Pos2 {
    let d = app.views[0].lanes.drew();
    let (lo, hi) = d.ranges[1];
    d.point(1, frame, (lo + hi) / 2.0).unwrap()
}

fn graph_selection_primary(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
}

fn graph_selection_fill_rects(out: &egui::FullOutput, fill: egui::Color32) -> Vec<egui::Rect> {
    out.shapes.iter().filter_map(|s| match &s.shape {
        egui::Shape::Rect(r) if r.fill == fill => Some(r.rect),
        _ => None,
    }).collect()
}

/// Optional review artifact from an actual GRAPH drag. Normal test runs never write files.
fn graph_selection_capture(ctx: &egui::Context, app: &mut TakViewerApp) {
    let Some(path) = std::env::var_os("TAK_GRAPH_SELECTION_CAPTURE") else { return };
    ctx.set_fonts(egui::FontDefinitions::default());
    frame_with(ctx, app, vec![]);
    theme::install_fonts(ctx);
    let out = frame_with(ctx, app, vec![]);
    let mut textures = crate::soft_render::Textures::default();
    textures.set(&out.textures_delta);
    let ppp = out.pixels_per_point;
    let size = [(1600.0 * ppp).round() as usize, (960.0 * ppp).round() as usize];
    let primitives = ctx.tessellate(out.shapes, ppp);
    let image = crate::soft_render::paint(&primitives, &textures, size, ppp, egui::Color32::from_rgb(25, 25, 25));
    let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
    crate::shot::save_png(std::path::Path::new(&path), size[0], size[1], &rgba).unwrap();
}

#[test]
fn graph_selection_docked_drag_is_live_shared_with_band_and_one_undo_action() {
    let ctx = egui::Context::default();
    let mut app = graph_selection_setup(&ctx, false);
    app.views[0].range = Some((1, 3));
    app.quiet();
    let selected = app.views[0].ordered_selection();
    let (from, to) = (graph_selection_point(&app, 7.0), graph_selection_point(&app, 2.0));
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(from)]);
    frame_with(&ctx, &mut app, vec![graph_selection_primary(from, true)]);
    assert_eq!((app.views[0].range, app.views[0].pb.frame), (Some((1, 3)), 5));
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(to)]);
    assert_eq!((app.views[0].range, app.views[0].pb.frame), (Some((2, 7)), 5), "range is live while the mouse is still held");
    assert_eq!(steps(&app, 0), (0, 0), "no intermediate undo actions while dragging");
    let live = frame_with(&ctx, &mut app, vec![]);
    assert_eq!(graph_selection_fill_rects(&live, theme::RANGE.gamma_multiply(0.12)).len(), 1, "one shared range across all graph lanes");
    assert_eq!(graph_selection_fill_rects(&live, theme::RANGE.gamma_multiply(0.18)).len(), 1, "the same live range is also in the playback band");
    frame_with(&ctx, &mut app, vec![graph_selection_primary(to, false)]);
    assert_eq!(app.history.kinds(), vec!["Range"], "release records just the range, no frame or marker movement");
    assert_eq!(app.views[0].ordered_selection(), selected, "multi-marker selection remains intact");
    assert_eq!(app.views[0].lanes.drew().solid, 6, "both selected markers remain in all three lanes");
    graph_selection_capture(&ctx, &mut app);
    undo_key(&ctx, &mut app);
    assert_eq!((app.views[0].range, app.views[0].pb.frame), (Some((1, 3)), 5));
    redo_key(&ctx, &mut app);
    assert_eq!((app.views[0].range, app.views[0].pb.frame), (Some((2, 7)), 5));

    let band_ctx = egui::Context::default();
    let mut band = graph_selection_setup(&band_ctx, false);
    let bar = Bar::of(&band, 0);
    drag(&band_ctx, &mut band, bar.band(7.0), bar.band(2.0));
    assert_eq!((band.views[0].range, band.views[0].pb.frame), (app.views[0].range, 5), "band and GRAPH choose identical inclusive ranges");
    assert_eq!(band.history.kinds(), vec!["Range"]);

    let (from, to) = (graph_selection_point(&app, 4.0), graph_selection_point(&app, 8.0));
    drag(&ctx, &mut app, from, to);
    assert_eq!((app.views[0].range, app.views[0].pb.frame), (Some((4, 8)), 5));
    assert_eq!(app.history.kinds(), vec!["Range", "Range"], "separate drags remain separate undo actions");
    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].range, Some((2, 7)));
}

#[test]
fn graph_selection_detached_graph_uses_the_same_range_click_and_undo_behavior() {
    let ctx = egui::Context::default();
    let mut app = graph_selection_setup(&ctx, true);
    assert!(app.dock.main_surface().find_tab(&Pane::Graph).is_none(), "GRAPH is detached");
    assert!(app.views[0].graph_rect.is_none(), "this is the detached GRAPH panel, not a 3D overlay");
    let (from, to) = (graph_selection_point(&app, 2.0), graph_selection_point(&app, 7.0));
    drag(&ctx, &mut app, from, to);
    assert_eq!((app.views[0].range, app.views[0].pb.frame), (Some((2, 7)), 5));
    assert_eq!(app.history.kinds(), vec!["Range"]);
    undo_key(&ctx, &mut app);
    assert_eq!((app.views[0].range, app.views[0].pb.frame), (None, 5));
    redo_key(&ctx, &mut app);
    assert_eq!(app.views[0].range, Some((2, 7)));
    idle(&ctx, &mut app);
    let p = graph_selection_point(&app, 3.0);
    click_at(&ctx, &mut app, p);
    assert_eq!((app.views[0].range, app.views[0].pb.frame), (Some((2, 7)), 3), "plain click still seeks without clearing the selection");
}

#[test]
fn graph_selection_keeps_3d_overlay_drag_as_frame_scrubbing() {
    let ctx = egui::Context::default();
    let (mut app, _) = one_view_with_graph(&ctx);
    app.views[0].range = Some((1, 3));
    app.views[0].pb.set_frame(5, 0.0);
    wait_marker_graphs(&ctx, &mut app);
    app.quiet();
    app.history.undo.clear();
    app.history.redo.clear();
    let (from, to) = (graph_selection_point(&app, 2.0), graph_selection_point(&app, 7.0));
    drag(&ctx, &mut app, from, to);
    assert_eq!((app.views[0].range, app.views[0].pb.frame), (Some((1, 3)), 7), "graph_float.rs retains frame scrubbing");
    assert_eq!(app.history.kinds(), vec!["Frame"]);
    undo_key(&ctx, &mut app);
    assert_eq!((app.views[0].range, app.views[0].pb.frame), (Some((1, 3)), 5));
}
