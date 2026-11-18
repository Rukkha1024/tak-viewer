#[test]
fn playback_working_range_handle_drag_sorts_clamps_and_undoes_independently() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.views[0].pb.set_working_range(2, 6, 0.0);
    app.views[0].pb.set_frame(9, 0.0);
    app.views[0].set_range(1, 4);
    app.quiet();
    frame_with(&ctx, &mut app, vec![]);
    let bar = Bar::of(&app, 0);
    let handle = |frame| bar.band(frame) + egui::vec2(0.0, panels::BAND_H / 2.0 + 4.5);

    drag(&ctx, &mut app, handle(2.0), handle(12.0));
    assert_eq!(app.views[0].pb.working_range(), (6, 9));
    assert_eq!(app.views[0].range, Some((1, 4)));
    assert_eq!(app.views[0].pb.frame, 9);
    assert_eq!(app.history.kinds(), ["PlaybackControls"]);
    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].pb.working_range(), (2, 6));
    redo_key(&ctx, &mut app);
    assert_eq!(app.views[0].pb.working_range(), (6, 9));

    drag(&ctx, &mut app, handle(9.0), handle(-3.0));
    assert_eq!(app.views[0].pb.working_range(), (0, 6));
}

#[test]
fn playback_working_range_reset_keys_are_exact_and_follow_input_priority() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    app.views[0].pb.set_working_range(2, 8, 0.0);
    app.views[0].pb.set_frame(3, 0.0);
    app.quiet();

    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::OpenBracket)]);
    assert_eq!(app.views[0].range, Some((3, 3)));
    assert_eq!(app.views[0].pb.working_range(), (2, 8));
    run_frame(
        &ctx,
        &mut app,
        &[(
            Modifiers {
                shift: true,
                ..ctrl()
            },
            Key::OpenBracket,
        )],
    );
    assert_eq!(app.views[0].pb.working_range(), (2, 8));
    key_down(&ctx, &mut app, Key::M);
    run_frame(&ctx, &mut app, &[(shift, Key::OpenBracket)]);
    assert_eq!(app.views[0].pb.working_range(), (2, 8));
    key_up(&ctx, &mut app, Key::M);

    for key in [Key::OpenBracket, Key::CloseBracket] {
        run_frame(&ctx, &mut app, &[(shift, key)]);
        assert_eq!(app.views[0].pb.working_range(), (0, 9));
        assert_eq!(app.views[0].range, Some((3, 3)));
        undo_key(&ctx, &mut app);
        assert_eq!(app.views[0].pb.working_range(), (2, 8));
    }
}

#[test]
fn playback_working_range_controls_commit_cancel_and_restore() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.views[0].pb.set_working_range(2, 5, 0.0);
    app.views[0].set_range(1, 3);
    app.quiet();
    let find = |app: &mut TakViewerApp, text: &str| {
        let screen = app.views[0].scene_rect;
        let out = frame_with(&ctx, app, vec![]);
        text_rects(&out, text)
            .into_iter()
            .find(|r| {
                r.center().y > screen.bottom() + panels::TRANSPORT_TOP
                    && screen.left() < r.center().x
                    && r.center().x < screen.right()
            })
            .unwrap()
            .center()
    };
    let type_start = |app: &mut TakViewerApp, text: &str| {
        let value = app.views[0].pb.working_range().0.to_string();
        let start = find(app, &value);
        click_at(&ctx, app, start);
        assert!(app.typing(&ctx));
        frame_with(&ctx, app, vec![egui::Event::Text(text.into())]);
    };

    type_start(&mut app, "8");
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Enter)]);
    assert_eq!(app.views[0].pb.working_range(), (2, 5));
    assert!(takm_note(&app).contains("Start must be before or at end"));
    assert!(app.history.kinds().is_empty());

    type_start(&mut app, "4");
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
    assert_eq!(app.views[0].pb.working_range(), (2, 5));
    assert_eq!(app.views[0].range, Some((1, 3)));

    type_start(&mut app, "4");
    run_frame(
        &ctx,
        &mut app,
        &[(
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
            Key::CloseBracket,
        )],
    );
    assert_eq!(app.views[0].pb.working_range(), (2, 5));
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Enter)]);
    assert_eq!(app.views[0].pb.working_range(), (4, 5));
    undo_key(&ctx, &mut app);
    assert_eq!(app.views[0].pb.working_range(), (2, 5));
    redo_key(&ctx, &mut app);
    assert_eq!(app.views[0].pb.working_range(), (4, 5));

    type_start(&mut app, "3");
    let blank = app.views[0].scene_rect.center();
    click_at(&ctx, &mut app, blank);
    assert_eq!(
        app.views[0].pb.working_range(),
        (3, 5),
        "outside click commits"
    );

    let reverse = find(&mut app, "Reverse");
    click_at(&ctx, &mut app, reverse);
    assert!(app.views[0].pb.reverse());
    undo_key(&ctx, &mut app);
    assert!(!app.views[0].pb.reverse());
    redo_key(&ctx, &mut app);
    assert!(app.views[0].pb.reverse());

    let reset = find(&mut app, "Reset");
    click_at(&ctx, &mut app, reset);
    assert_eq!(app.views[0].pb.working_range(), (0, 9));
    assert!(app.views[0].pb.reverse());
}

#[test]
fn playback_working_range_is_shared_by_overlay_and_reset_on_new_file() {
    let ctx = egui::Context::default();
    let dir = comment_dir("working_range_overlay");
    for name in ["a.takm", "b.takm"] {
        working_range_takm(&dir.join(name));
    }
    let (mut app, files) = overlay_app(&ctx, &dir, &["a.takm", "b.takm"]);
    app.views[0].pb.set_working_range(3, 5, 0.0);
    app.views[0].pb.set_reverse(true, 0.0);
    lay_over(&ctx, &mut app, &files[1]);
    overlay_arrive(&ctx, &mut app, 0);
    assert_eq!(app.views[0].pb.working_range(), (3, 5));
    let pb = &mut app.views[0].pb;
    pb.set_frame(3, 0.0);
    pb.play(0.0);
    assert_eq!(app.views[0].overlay_frame(), Some(5));
    app.views[0].pb.tick(0.025);
    assert_eq!(app.views[0].overlay_frame(), Some(3));
    app.views[0].pb.tick(0.035);
    assert_eq!(app.views[0].overlay_frame(), Some(5));

    let (_, rx) = std::sync::mpsc::channel();
    app.views[0].show(views::sample_data("new.takm", &["M"]), rx, 55, 1.0, 0.0);
    assert_eq!(app.views[0].pb.working_range(), (0, 9));
    assert!(!app.views[0].pb.reverse());
}

fn working_range_takm(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    tak_core::takm::write(
        &mut bytes,
        &tak_core::sample_tak::sample_tak(),
        "sample.tak",
        "test",
    )
    .unwrap();
    std::fs::write(path, &bytes).unwrap();
    bytes
}

#[test]
fn playback_working_range_held_comment_closes_at_reverse_boundary_and_reopens() {
    let ctx = egui::Context::default();
    let dir = comment_dir("working_range_hold");
    let file = dir.join("a.takm");
    let original = working_range_takm(&file);
    let mut app = comment_app(&ctx, &dir, false);
    app.views[0].pb.set_working_range(2, 7, 0.0);
    app.views[0].pb.set_reverse(true, 0.0);
    app.views[0].pb.set_frame(4, 0.0);
    app.views[0].select(0, false);
    key_down(&ctx, &mut app, Key::M);
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Space)]);
    for _ in 0..20 {
        if app.hold.is_none() {
            break;
        }
        frame_with(&ctx, &mut app, vec![]);
    }
    key_up(&ctx, &mut app, Key::M);
    assert_eq!(spans(&app), [(1, 2, 4)]);
    assert!(app.views[0].pb.playing);
    until_saved(&ctx, &mut app);
    let saved = std::fs::read(&file).unwrap();
    assert_eq!(&saved[..original.len()], original.as_slice());
    drop(app);
    let reopened = comment_app(&egui::Context::default(), &dir, true);
    assert_eq!(spans(&reopened), [(1, 2, 4)]);
    assert_eq!(reopened.views[0].pb.working_range(), (0, 9));
    assert!(!reopened.views[0].pb.reverse());
}

#[test]
fn playback_working_range_screenshot_options_keep_edit_selection_separate() {
    let args = [
        "--screenshot",
        "out.png",
        "--working-range",
        "800-200",
        "--range",
        "400-500",
        "--reverse-playing",
        "sample.takm",
    ]
    .map(str::to_owned);
    let (auto, files) = shot::parse(&args);
    let auto = auto.unwrap();
    assert_eq!(auto.range, Some((400, 500)));
    assert_eq!(auto.working_range, Some((800, 200)));
    assert!(auto.reverse_playing);
    assert_eq!(files, ["sample.takm"]);
}

#[test]
fn tracks_can_seek_and_set_edit_range_outside_the_working_range() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.set_graph_float(false);
    app.settings.graph_layout = graph::tracks::Layout::Tracks;
    app.views[0].data.as_ref().unwrap().prepare_marker_order();
    app.views[0].pb.set_working_range(2, 5, 0.0);
    app.views[0].pb.set_frame(3, 0.0);
    app.quiet();
    for _ in 0..3 {
        frame_with(&ctx, &mut app, vec![]);
    }
    let out = frame_with(&ctx, &mut app, vec![]);
    let green = egui::Color32::from_rgb(0x48, 0xb4, 0x67);
    let row = graph_selection_fill_rects(&out, green)[0];
    let body = graph_selection_fill_rects(&out, theme::GRAPH_BG)
        .into_iter()
        .find(|rect| rect.contains(row.center()))
        .unwrap();
    let band = graph_selection_fill_rects(&out, theme::BAND_BG)
        .into_iter()
        .find(|rect| (rect.left() - row.left()).abs() < 1.0 && rect.top() > body.bottom())
        .unwrap();
    let x = |frame: f32| band.left() + band.width() * (frame + 0.5) / 10.0;

    drag(&ctx, &mut app, egui::pos2(x(7.0), band.center().y), egui::pos2(x(9.0), band.center().y));
    click_at(&ctx, &mut app, egui::pos2(x(8.0), band.top() - graph::NAV_H / 2.0));
    let view = &app.views[0];
    assert_eq!(view.range, Some((7, 9)));
    assert_eq!(view.pb.frame, 8);
    assert_eq!(view.pb.working_range(), (2, 5));
}

#[test]
fn wide_deck_keeps_working_bounds_on_the_transport_row() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.views[0].pb = tak_core::playback::Playback::new(0, 12_901, 100.0);
    app.views[0].pb.set_frame(2_600, 0.0);
    app.views[0].pb.set_working_range(1_800, 3_400, 0.0);
    let out = frame_with(&ctx, &mut app, vec![]);
    let bounds = text_rects(&out, "Working Range")[0];
    let reverse = text_rects(&out, "Reverse")[0];
    assert!((bounds.center().y - reverse.center().y).abs() < 3.0);
    assert!(bounds.left() > reverse.right());
    let reset = text_rects(&out, "Reset")[0];
    let comment = text_rects(&out, "Comment")[0];
    assert!(reset.right() < comment.left(), "reset {reset:?}, Comment {comment:?}");
    assert!(comment.right() < app.views[0].scene_rect.right());
    click_at(&ctx, &mut app, reset.center());
    assert_eq!(app.views[0].pb.working_range(), (0, 12_901));
}
