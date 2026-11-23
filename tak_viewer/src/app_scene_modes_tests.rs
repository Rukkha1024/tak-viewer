#[test]
fn scene_modes_follow_only_the_focused_scene_and_respect_exact_keys() {
    let ctx = egui::Context::default();
    let mut app = views_with_plates(&ctx, 2);
    app.set_graph_float(false);
    frame_with(&ctx, &mut app, vec![]);
    let second_scene = app.views[1].scene_rect.center();
    click_at(&ctx, &mut app, second_scene);
    run_frame(&ctx, &mut app, &[(ctrl(), Key::G)]);
    assert!(!app.views[1].cam.follow);
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::M), (Modifiers::NONE, Key::G)]);
    assert!(!app.views[1].cam.follow, "M owns the keyboard on its first frame");
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::G)]);
    assert!(!app.views[0].cam.follow);
    assert!(app.views[1].cam.follow);
    undo_key(&ctx, &mut app);
    assert!(!app.views[1].cam.follow);
    redo_key(&ctx, &mut app);
    assert!(app.views[1].cam.follow);

    // Clicking a different panel changes focus. Merely moving back never changes it.
    let explorer = place_of(&app, Pane::Explorer).center();
    click_at(&ctx, &mut app, explorer);
    let other_scene = app.views[0].scene_rect.center();
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(other_scene)]);
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::G)]);
    assert!(!app.views[0].cam.follow);
    assert!(app.views[1].cam.follow);
}

#[test]
fn scene_modes_follow_averages_present_samples_and_holds_through_gaps() {
    let mut take = views::sample_take("follow-a.tak", &["M", "N"]);
    take.trajectories[1].frames.remove(1);
    take.trajectories[1].xyz.remove(1);
    let mut view = View::default();
    view.toggle_follow();
    assert!(!view.cam.follow, "empty view cannot follow");
    let (_tx, rx) = std::sync::mpsc::channel();
    view.show(tak_core::view_data::ViewData::build(take), rx, 1, 1.0, 0.0);
    view.select(0, false);
    view.select(1, true);
    view.toggle_follow();
    let manual = view.cam;
    let first = view.update_follow_target().unwrap();
    assert!((first - v3(0.05, 1.0, 0.0)).length() < 1e-5);
    view.pb.set_frame(1, 0.0);
    let second = view.update_follow_target().unwrap();
    assert!((second - v3(0.0, 1.0, 0.01)).length() < 1e-5);
    assert_eq!(view.cam, manual, "automatic motion leaves the undo state alone");

    view.select(1, false);
    assert_eq!(view.update_follow_target(), Some(second), "all selected samples are missing");
    view.clear_selection();
    assert_eq!(view.update_follow_target(), Some(second), "empty selection keeps last target");
    view.toggle_follow();
    assert!(!view.cam.follow);
    assert_eq!(view.cam.pivot, second);

    view.toggle_follow();
    let (_tx, rx) = std::sync::mpsc::channel();
    view.show(views::sample_data("follow-b.tak", &["M"]), rx, 2, 1.0, 0.0);
    assert!(!view.cam.follow);
    assert!(view.follow_target.is_none());
}

#[test]
fn scene_modes_projection_and_names_round_trip_through_app_history() {
    use crate::camera::Preset;
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    round_trip(&ctx, &mut app, "projection", |_, a| a.views[0].cam.preset = Preset::Top,
        |a| a.views[0].cam.preset, "Undo · Camera");
    round_trip(&ctx, &mut app, "3D names", |c, a| run_frame(c, a, &[(Modifiers::NONE, Key::L)]),
        |a| a.settings.scene.names, "Undo · Display");
}

#[test]
fn scene_modes_screenshot_options_and_existing_settings() {
    use crate::camera::Preset;
    let args = ["--screenshot", "out.png", "--scene-view", "Top", "--scene-follow", "walk.tak"].map(String::from);
    let (shot, remaining) = shot::parse(&args);
    let shot = shot.unwrap();
    assert_eq!(shot.scene_view, Some(Preset::Top));
    assert!(shot.scene_follow);
    assert_eq!(remaining, vec!["walk.tak"]);

    let args = ["--screenshot", "out.png", "--scene-view", "diagonal", "walk.tak"].map(String::from);
    let (shot, remaining) = shot::parse(&args);
    assert_eq!(shot.unwrap().scene_view, None);
    assert_eq!(remaining, vec!["walk.tak"]);

    let old: Settings = ron::from_str(r#"(recent:["old.tak"], file_memory:[(
        path:"old.tak", frame:5, pivot:(0.0,1.0,2.0), yaw:0.5, pitch:0.3,
        distance:3.0, fov_y:0.7, selected:[], primary:None
    )])"#).unwrap();
    assert_eq!(old.recent, ["old.tak"]);
    assert_eq!(old.file_memory[0].camera_mode, (Preset::Perspective, false));
    let mut changed = old.file_memory[0].clone();
    changed.camera_mode = (Preset::Top, true);
    let serialized = ron::to_string(&changed).unwrap();
    assert!(!serialized.contains("camera_mode"));
    let mut memory = Vec::new();
    views::remember(&mut memory, changed);
    assert_eq!(memory[0].camera_mode, (Preset::Perspective, false));
}

#[test]
fn scene_modes_orthographic_click_and_marker_name_hover_match_what_is_drawn() {
    use crate::camera::Preset;
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.set_graph_float(false);
    app.views[0].cam.preset = Preset::Front;
    app.views[0].cam.pivot = v3(0.0, 1.0, 0.0);
    frame_with(&ctx, &mut app, vec![]);
    let rect = app.views[0].scene_rect;
    let point = app.views[0].data.as_ref().unwrap().markers[0].position(0).unwrap();
    let at = app.views[0].cam.view(rect).project(point).unwrap().0;
    click_at(&ctx, &mut app, at);
    assert_eq!(app.views[0].primary, Some(0));

    let drawn_in_scene = |out: &egui::FullOutput| text_rects(out, "M")
        .iter().any(|label| rect.contains(label.center()));
    let at_corner = rect.left_top() + egui::vec2(25.0, 50.0);
    let output = frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(at_corner)]);
    assert!(!drawn_in_scene(&output), "selection alone does not reveal name");
    let output = frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
    assert!(drawn_in_scene(&output), "hover displays the matching name");
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::L)]);
    let output = frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(at_corner)]);
    assert!(drawn_in_scene(&output), "L displays a selected name without hover");
}

#[test]
fn macos_option_drag_binds_camera_motion_to_the_initial_modifiers() {
    use egui::{Event, PointerButton};
    for shift in [false, true] {
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        app.set_graph_float(false);
        frame_with(&ctx, &mut app, vec![]);
        let at = app.views[0].scene_rect.center();
        frame_with(&ctx, &mut app, vec![Event::PointerMoved(at)]);
        let initial = app.views[0].cam;
        let selected = app.views[0].selected.clone();
        frame_with(&ctx, &mut app, vec![Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers { alt: true, shift, ..Modifiers::NONE },
        }]);
        let end = at + egui::vec2(60.0, 25.0);
        // The move and the release occur after the user lets go of Option and Shift.
        frame_with(&ctx, &mut app, vec![Event::PointerMoved(end)]);
        frame_with(&ctx, &mut app, vec![Event::PointerButton {
            pos: end,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
        let moved = app.views[0].cam;
        if cfg!(target_os = "macos") {
            assert_eq!(moved.yaw != initial.yaw, !shift);
            assert_eq!(moved.pivot != initial.pivot, shift);
            assert_eq!(app.views[0].selected, selected);
        } else {
            assert_eq!(moved, initial);
        }
    }
}
