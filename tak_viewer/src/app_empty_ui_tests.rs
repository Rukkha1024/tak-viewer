// Included in app::tests: exercise the real post-close dock and every file-dependent pane.
fn empty_ui_after_close(ctx: &egui::Context) -> TakViewerApp {
    let mut app = one_view(ctx);
    app.set_command_run();
    app.close_view(0);
    assert!(app.open_views().is_empty());
    for _ in 0..3 {
        frame_with(ctx, &mut app, vec![]);
    }
    app
}

fn empty_ui_output(ctx: &egui::Context, app: &mut TakViewerApp) -> egui::FullOutput {
    frame_with(ctx, app, vec![]);
    frame_with(ctx, app, vec![])
}

#[test]
fn empty_ui_after_last_close_has_hint_explorer_and_no_playback_or_scene() {
    let ctx = egui::Context::default();
    let mut app = empty_ui_after_close(&ctx);
    let out = empty_ui_output(&ctx, &mut app);
    assert_eq!(text_rects(&out, crate::ui::empty::EDITOR_HINT).len(), 1);
    assert_eq!(text_rects(&out, "EXPLORER").len(), 1);
    assert_eq!(text_rects(&out, "OPEN EDITORS (0)").len(), 1);
    assert!(text_rects(&out, "Open a file from the Explorer.").len() >= 1);
    assert!(text_rects(&out, "Perspective").is_empty());
    assert!(app
        .views
        .iter()
        .all(|v| !v.scene_rect.is_positive() && v.graph_rect.is_none()));
    assert!(app.views.iter().all(|v| v.data.is_none() && !v.pb.playing));
    assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == theme::PANEL_BG_DARK && r.rect.width() > 400.0 && r.rect.height() > 400.0)));
    empty_ui_capture(&ctx, &mut app);
}

#[test]
fn empty_ui_all_panes_render_as_active_tabs_and_detached_windows() {
    for detached in [false, true] {
        for pane in [
            Pane::Labeling,
            Pane::EditTools,
            Pane::Selected,
            Pane::Graph,
            Pane::Devices,
            Pane::Comments,
        ] {
            let ctx = egui::Context::default();
            let mut app = empty_ui_after_close(&ctx);
            if detached {
                dock::hide(&mut app.dock, pane);
                let window = app.dock.add_window(vec![pane]);
                app.dock
                    .get_window_state_mut(window)
                    .unwrap()
                    .set_position(egui::pos2(400.0, 150.0))
                    .set_size(egui::vec2(700.0, 420.0));
            } else {
                app.show_pane(pane);
            }
            let out = empty_ui_output(&ctx, &mut app);
            let hint = if matches!(pane, Pane::Labeling | Pane::EditTools) {
                "Open a file from the Explorer."
            } else {
                "파일을 열면 여기에 나와요."
            };
            let (surface, node, _) = app.dock.find_tab(&pane).unwrap();
            let place = app.dock[surface][node]
                .rect()
                .expect("the pane has been drawn");
            assert!(
                text_rects(&out, hint)
                    .iter()
                    .any(|r| place.contains_rect(*r)),
                "{pane:?} detached={detached}: empty hint in {place:?}"
            );
            assert_eq!(text_rects(&out, crate::ui::empty::EDITOR_HINT).len(), 1);
            assert!(app.open_views().is_empty());
            assert!(app.views.iter().all(|v| v.data.is_none()));
        }
    }
}

#[test]
fn empty_ui_file_shortcuts_and_panel_toggles_leave_the_editor_empty() {
    let ctx = egui::Context::default();
    let mut app = empty_ui_after_close(&ctx);
    app.quiet();
    app.history.undo.clear();
    app.history.redo.clear();
    for (modifiers, key) in [
        (ctrl(), Key::S),
        (ctrl(), Key::W),
        (Modifiers::NONE, Key::PageUp),
        (Modifiers::NONE, Key::PageDown),
        (Modifiers::NONE, Key::Delete),
        (Modifiers::NONE, Key::Space),
        (Modifiers::NONE, Key::F),
        (Modifiers::SHIFT, Key::F),
        (Modifiers::NONE, Key::M),
        (Modifiers::NONE, Key::T),
        (Modifiers::NONE, Key::D),
        (Modifiers::NONE, Key::Q),
        (Modifiers::NONE, Key::C),
        (Modifiers::SHIFT, Key::C),
    ] {
        run_frame(&ctx, &mut app, &[(modifiers, key)]);
        assert!(app.open_views().is_empty(), "{modifiers:?} {key:?}");
        assert!(app.views.iter().all(|v| v.data.is_none() && !v.pb.playing));
        assert!(app.comment_edit.is_none() && app.hold.is_none() && app.dirty_prompt.is_none());
    }
    assert!(
        app.history.undo.is_empty(),
        "file shortcuts without a file do not create actions"
    );
    for (modifiers, key) in [
        (ctrl(), Key::B),
        (ctrl(), Key::B),
        (ctrl(), Key::J),
        (ctrl(), Key::J),
        (
            Modifiers {
                alt: true,
                ..ctrl()
            },
            Key::B,
        ),
        (
            Modifiers {
                alt: true,
                ..ctrl()
            },
            Key::B,
        ),
    ] {
        run_frame(&ctx, &mut app, &[(modifiers, key)]);
        let out = empty_ui_output(&ctx, &mut app);
        assert!(app.open_views().is_empty());
        assert_eq!(text_rects(&out, crate::ui::empty::EDITOR_HINT).len(), 1);
    }
}

#[test]
fn empty_ui_reopening_a_file_restores_the_default_panels_and_live_content() {
    let ctx = egui::Context::default();
    let mut app = empty_ui_after_close(&ctx);
    app.show_pane(Pane::Graph);
    app.show_pane(Pane::Comments);
    app.toggle_side(Side::Left);
    app.toggle_side(Side::Right);
    let window = app.dock.add_window(vec![Pane::Devices]);
    app.dock
        .get_window_state_mut(window)
        .unwrap()
        .set_position(egui::pos2(400.0, 150.0));
    app.hold_loads = true;
    app.open_in(&ctx, 0, "reopened.tak".into());
    assert_eq!(app.open_views(), vec![0]);
    assert!(!dock::has(&app.dock, Pane::Empty));
    assert_eq!(app.hidden, dock::Hidden::default());
    for pane in [
        Pane::Explorer,
        Pane::Labeling,
        Pane::EditTools,
        Pane::Selected,
        Pane::Devices,
    ] {
        assert!(
            app.dock.main_surface().find_tab(&pane).is_some(),
            "{pane:?} returned to its default place"
        );
    }
    assert!(!dock::has(&app.dock, Pane::Comments) && !dock::has(&app.dock, Pane::Graph));
    assert_eq!(
        app.dock
            .iter_all_tabs()
            .filter(|(_, p)| **p == Pane::Devices)
            .count(),
        1
    );
    let (_tx, rx) = std::sync::mpsc::channel();
    app.views[0].show(
        views::sample_data("reopened.tak", &["M", "N"]),
        rx,
        12,
        1.0,
        0.0,
    );
    let out = empty_ui_output(&ctx, &mut app);
    assert!(text_rects(&out, crate::ui::empty::EDITOR_HINT).is_empty());
    assert_eq!(text_rects(&out, "QuickLabel Mode  D").len(), 1);
    assert!(app.views[0].scene_rect.is_positive());
    app.views[0].select(0, false);
    let out = empty_ui_output(&ctx, &mut app);
    assert!(
        text_rects(&out, "이름").len() >= 1,
        "the selected-marker pane is live again"
    );
}

#[test]
fn empty_ui_layout_reset_menu_keeps_the_editor_empty() {
    let ctx = egui::Context::default();
    let mut app = empty_ui_after_close(&ctx);
    app.show_pane(Pane::Graph);
    let out = empty_ui_output(&ctx, &mut app);
    let menu = text_rects(&out, "보기")[0].center();
    let out = click_at(&ctx, &mut app, menu);
    let reset = text_rects(&out, "패널 배치 처음대로")[0].center();
    click_at(&ctx, &mut app, reset);
    let out = empty_ui_output(&ctx, &mut app);
    assert!(app.open_views().is_empty());
    assert!(!dock::has(&app.dock, Pane::Graph));
    assert_eq!(text_rects(&out, crate::ui::empty::EDITOR_HINT).len(), 1);
}

/// Optional inspection artifact from the real post-close app. Normal tests write nothing.
fn empty_ui_capture(ctx: &egui::Context, app: &mut TakViewerApp) {
    let Some(path) = std::env::var_os("TAK_EMPTY_UI_CAPTURE") else {
        return;
    };
    ctx.set_fonts(egui::FontDefinitions::default());
    frame_with(ctx, app, vec![]);
    theme::install_fonts(ctx);
    let out = frame_with(ctx, app, vec![]);
    let mut textures = crate::soft_render::Textures::default();
    textures.set(&out.textures_delta);
    let ppp = out.pixels_per_point;
    let size = [
        (1600.0 * ppp).round() as usize,
        (960.0 * ppp).round() as usize,
    ];
    let primitives = ctx.tessellate(out.shapes, ppp);
    let image = crate::soft_render::paint(
        &primitives,
        &textures,
        size,
        ppp,
        egui::Color32::from_rgb(25, 25, 25),
    );
    let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
    crate::shot::save_png(std::path::Path::new(&path), size[0], size[1], &rgba).unwrap();
}
