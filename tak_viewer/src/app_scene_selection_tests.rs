fn scene_selection_app(ctx: &egui::Context) -> TakViewerApp {
    let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
    app.settings.graph_float = false;
    let (_tx, rx) = std::sync::mpsc::channel();
    app.views[0].show(views::sample_data("scene-selection.tak", &["M", "N", "O"]), rx, 10, 1.0, 0.0);
    for _ in 0..3 { frame_with(ctx, &mut app, vec![]); }
    app.history.undo.clear();
    app
}

#[test]
fn scene_shift_click_adds_in_order_refuses_duplicates_and_undoes() {
    let ctx = egui::Context::default();
    let mut app = scene_selection_app(&ctx);
    let [m, n, o] = [0, 1, 2].map(|index| marker_on_screen(&app, index));
    click_at(&ctx, &mut app, o);
    app.marker_ui[0].labeling.mode = crate::ui::labeling::LabelingMode::QuickLabel;
    let armed = Some(tak_core::edit::LabelKey { parent: [7; 16], slot: 1 });
    app.views[0].armed_label = armed;
    app.quiet();
    let shift = Modifiers { shift: true, ..Modifiers::NONE };
    click_with(&ctx, &mut app, m, shift);
    click_with(&ctx, &mut app, n, shift);
    assert_eq!(app.views[0].ordered_selection(), [2, 0, 1]);
    assert_eq!(app.views[0].armed_label, armed);
    assert!(!app.marker_job_running(0));
    let before = app.views[0].selection();
    let history = steps(&app, 0);
    click_with(&ctx, &mut app, n, shift);
    let blank = app.views[0].scene_rect.left_top() + egui::vec2(80.0, 120.0);
    click_with(&ctx, &mut app, blank, shift);
    click_with(&ctx, &mut app, m, Modifiers { shift: true, ..ctrl() });
    assert_eq!(app.views[0].selection(), before);
    assert_eq!(steps(&app, 0), history);
    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].ordered_selection(), [2, 0]);
    redo_key(&ctx, &mut app);
    assert_eq!(app.views[0].selection(), before);
    click_with(&ctx, &mut app, n, ctrl());
    assert_eq!(app.views[0].ordered_selection(), [2, 0]);
    assert_eq!(app.views[0].armed_label, armed);
}

#[test]
fn scene_ctrl_a_follows_last_click_and_text_input_keeps_ctrl_a() {
    let ctx = egui::Context::default();
    let mut app = scene_selection_app(&ctx);
    let ctrl_a = || key_event(Key::A, true, ctrl());
    let at = marker_on_screen(&app, 1);
    click_at(&ctx, &mut app, at);
    let before = app.views[0].selection();
    let workspace = place_of(&app, Pane::Explorer).center();
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(workspace), ctrl_a()]);
    assert_eq!(app.views[0].ordered_selection(), [0, 2, 1]);
    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].selection(), before);
    redo_key(&ctx, &mut app);
    assert_eq!(app.views[0].ordered_selection(), [0, 2, 1]);
    undo_key(&ctx, &mut app);

    let mut events = delete_focus_click(workspace);
    events.push(ctrl_a());
    frame_with(&ctx, &mut app, events);
    assert_eq!(app.views[0].selection(), before);
    let bar = Bar::of(&app, 0).seek(0.0);
    let mut events = delete_focus_click(bar);
    events.push(ctrl_a());
    frame_with(&ctx, &mut app, events);
    assert_eq!(app.views[0].selection(), before);
    let out = frame_with(&ctx, &mut app, vec![]);
    let search = text_rects(&out, "Search")[0].center();
    click_at(&ctx, &mut app, search);
    assert!(key_focus::text_input_focused(&ctx));
    type_over(&ctx, &mut app, "first");
    type_over(&ctx, &mut app, "replacement");
    assert_eq!(app.views[0].labels.filter, "replacement");
    assert_eq!(app.views[0].selection(), before);

    click_at(&ctx, &mut app, at);
    frame_with(&ctx, &mut app, vec![key_event(Key::A, true, Modifiers { shift: true, ..ctrl() })]);
    assert_eq!(app.views[0].selection(), before);
    ctx.memory_mut(|memory| memory.open_popup(egui::Id::new("scene_selection_test_popup")));
    frame_with(&ctx, &mut app, vec![ctrl_a()]);
    assert_eq!(app.views[0].selection(), before);
}

#[test]
fn scene_ctrl_a_checks_current_frame_projection_and_layer_visibility() {
    let ctx = egui::Context::default();
    let mut app = scene_selection_app(&ctx);
    let mut take = views::sample_take("visible.tak", &["Named", "Loose", "Gap", "Offscreen", "Behind"]);
    take.trajectories[0].kind = tak_core::tak::MarkerKind::OtherParent;
    take.trajectories[0].parent_uid = Some([7; 16]);
    take.trajectories[0].slot = Some(1);
    take.trajectories[2].frames = vec![1].into();
    take.trajectories[2].xyz = vec![[0.2, 1.0, 0.0]].into();
    take.trajectories[3].xyz = vec![[10.0, 1.0, 0.0]; 10].into();
    take.trajectories[4].xyz = vec![[0.0, 1.0, 10.0]; 10].into();
    let (_tx, rx) = std::sync::mpsc::channel();
    let view = &mut app.views[0];
    view.show(tak_core::view_data::ViewData::build(take), rx, 11, 1.0, 0.0);
    view.cam = Camera { yaw: 0.0, pitch: 0.0, distance: 2.0, pivot: v3(0.0, 1.0, 0.0), ..Camera::default() };
    view.fit_pending = false;
    view.overlay = Some(views::Overlay::from_data("overlay.tak".into(), views::sample_data("overlay.tak", &["Other"])));
    view.select(1, false);
    key_focus::select_visible(view, &app.settings.scene);
    let names = |view: &View| -> Vec<String> {
        view.ordered_selection().into_iter().map(|index| view.data.as_ref().unwrap().markers[index].name.clone()).collect()
    };
    assert_eq!(names(view), ["Named", "Loose"]);
    assert_eq!(view.primary, Some(1));
    app.settings.scene.unlabeled = false;
    key_focus::select_visible(view, &app.settings.scene);
    assert_eq!(names(view), ["Named"]);
    view.shown[0] = false;
    key_focus::select_visible(view, &app.settings.scene);
    assert!(view.selected.is_empty());
}

#[test]
fn scene_ctrl_a_keeps_the_floating_marker_graph_outside_scene_focus() {
    let ctx = egui::Context::default();
    let mut app = scene_selection_app(&ctx);
    app.views[0].select(0, false);
    app.settings.graph_float = true;
    frame_with(&ctx, &mut app, vec![]);
    let at = app.views[0].graph_rect.unwrap().center();
    let mut events = delete_focus_click(at);
    events.push(key_event(Key::A, true, ctrl()));
    frame_with(&ctx, &mut app, events);
    assert_eq!(key_focus::target(&ctx), key_focus::Target::Markers(0));
    assert_eq!(app.views[0].ordered_selection(), [0]);
}

#[test]
fn scene_measurements_read_older_settings_and_toggle_with_undo() {
    let mut settings: Settings = ron::from_str(r#"(scene: (grid: false), workspace_roots: ["prev"])"#).unwrap();
    assert!(settings.scene.measurements);
    assert!(!settings.scene.grid);
    assert_eq!(settings.workspace_roots, ["prev"]);
    settings.scene.measurements = false;
    let reloaded: Settings = ron::from_str(&ron::to_string(&settings).unwrap()).unwrap();
    assert!(!reloaded.scene.measurements);

    let ctx = egui::Context::default();
    let mut app = scene_selection_app(&ctx);
    app.views[0].select_many(vec![0, 1], Some(1), false);
    let out = frame_with(&ctx, &mut app, vec![]);
    assert_eq!(text_rects(&out, "100.00 mm").len(), 2);
    app.settings.scene.measurements = false;
    let out = frame_with(&ctx, &mut app, vec![]);
    assert!(text_rects(&out, "100.00 mm").is_empty());
    undo_key(&ctx, &mut app);
    assert!(app.settings.scene.measurements);
    redo_key(&ctx, &mut app);
    assert!(!app.settings.scene.measurements);
}

#[test]
fn scene_screenshot_select_visible_arg_is_consumed() {
    let args = ["--screenshot", "visible.png", "--select-visible", "gait.tak"].map(String::from);
    let (option, rest) = shot::parse(&args);
    assert_eq!(option.unwrap().select, Some(shot::ShotSelection::Visible));
    assert_eq!(rest, ["gait.tak"]);
}

#[test]
fn scene_ctrl_a_relinks_labeling_and_undo_restores_both() {
    let ctx = egui::Context::default();
    let mut app = scene_selection_app(&ctx);
    app.settings.labeling_link = true;
    let blank = app.views[0].scene_rect.left_top() + egui::vec2(80.0, 120.0);
    click_at(&ctx, &mut app, blank);
    let before = app.views[0].selection();
    assert!(before.primary.is_none());

    frame_with(&ctx, &mut app, vec![key_event(Key::A, true, ctrl())]);
    let view = &app.views[0];
    let primary = view.primary.unwrap();
    let uid = view.data.as_ref().unwrap().markers[primary].uid;
    assert_eq!(view.labels.linked_pick.track, Some(uid));
    assert_eq!(view.ordered_selection().len(), 3);

    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].selection(), before);
}

#[test]
fn alt_inspects_a_marker_without_gesture_side_effects() {
    let ctx = egui::Context::default();
    let mut app = scene_selection_app(&ctx);
    app.views[0].armed_label = Some(tak_core::edit::LabelKey { parent: [7; 16], slot: 1 });
    let point = marker_on_screen(&app, 1);
    click_with(&ctx, &mut app, point, Modifiers::ALT);
    assert_eq!(app.views[0].ordered_selection(), [1]);
    assert!(!app.marker_job_running(0));

    let before = app.views[0].selection();
    let history = steps(&app, 0);
    let blank = app.views[0].scene_rect.left_top() + egui::vec2(80.0, 120.0);
    click_with(&ctx, &mut app, blank, Modifiers::ALT);
    assert_eq!(app.views[0].selection(), before);
    assert_eq!(steps(&app, 0), history);
}
