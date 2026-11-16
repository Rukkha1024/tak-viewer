// Actual App frames: a selected marker must never receive Delete from a different panel.
fn delete_focus_app(ctx: &egui::Context) -> TakViewerApp {
    let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
    let (_tx, rx) = std::sync::mpsc::channel();
    app.views[0].show(views::sample_data("focus-delete.tak", &["M", "N"]), rx, 10, 1.0, 0.0);
    app.views[0].select(0, false);
    for _ in 0..3 { frame_with(ctx, &mut app, vec![]); }
    app
}

fn delete_focus_event() -> egui::Event {
    egui::Event::Key { key: Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
}

fn delete_focus_click(at: egui::Pos2) -> Vec<egui::Event> {
    vec![egui::Event::PointerMoved(at),
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE },
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]
}

#[test]
fn delete_focus_workspace_click_and_delete_in_same_frame_keeps_markers() {
    let ctx = egui::Context::default();
    let mut app = delete_focus_app(&ctx);
    let at = place_of(&app, Pane::Explorer).center();
    let mut events = delete_focus_click(at);
    events.push(delete_focus_event());
    frame_with(&ctx, &mut app, events);
    assert!(!app.marker_job_running(0), "Delete leaked from WORKSPACE to selected markers in the same frame");
}

#[test]
fn delete_focus_clicked_blocked_panel_keeps_delete_when_pointer_moves_to_graph() {
    for pane in [Pane::Explorer, Pane::Selected, Pane::Devices, Pane::EditTools] {
        let ctx = egui::Context::default();
        let mut app = delete_focus_app(&ctx);
        let (surface, node, tab) = app.dock.find_tab(&pane).unwrap();
        app.dock.set_active_tab((surface, node, tab));
        frame_with(&ctx, &mut app, vec![]);
        let at = place_of(&app, pane).center_bottom() - egui::vec2(0.0, 12.0);
        frame_with(&ctx, &mut app, delete_focus_click(at));
        assert!(app.views[0].selected.contains(&0));
        let graph = app.views[0].graph_rect.unwrap().center();
        frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(graph), delete_focus_event()]);
        assert!(!app.marker_job_running(0), "Delete leaked from clicked {pane:?}; hover must not choose its recipient");
    }
}

#[test]
fn delete_focus_clicked_graph_keeps_delete_when_pointer_moves_to_workspace() {
    let ctx = egui::Context::default();
    let mut app = delete_focus_app(&ctx);
    let graph = app.views[0].graph_rect.unwrap().center();
    frame_with(&ctx, &mut app, delete_focus_click(graph));
    let workspace = place_of(&app, Pane::Explorer).center();
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(workspace), delete_focus_event()]);
    assert!(app.marker_job_running(0), "clicked floating marker graph must own Delete across pointer movement");
}

#[test]
fn delete_focus_initial_active_3d_keeps_marker_delete_without_a_pointer_click() {
    let ctx = egui::Context::default();
    let mut app = delete_focus_app(&ctx);
    frame_with(&ctx, &mut app, vec![delete_focus_event()]);
    assert!(app.marker_job_running(0));
}

#[test]
fn delete_focus_marker_click_and_delete_in_same_frame_uses_current_selection() {
    let ctx = egui::Context::default();
    let mut app = delete_focus_app(&ctx);
    let at = marker_on_screen(&app, 1);
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
    frame_with(&ctx, &mut app, vec![egui::Event::PointerButton {
        pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE,
    }]);
    frame_with(&ctx, &mut app, vec![egui::Event::PointerButton {
        pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE,
    }, delete_focus_event()]);
    assert_eq!(app.views[0].selected, BTreeSet::from([1]));
    assert!(app.marker_job_running(0));
}

#[test]
fn delete_focus_graph_panel_and_detached_graph_window_allow_marker_delete() {
    for floating in [false, true] {
        let ctx = egui::Context::default();
        let mut app = delete_focus_app(&ctx);
        app.settings.graph_float = false;
        if floating {
            let surface = app.dock.add_window(vec![Pane::Graph]);
            app.dock.get_window_state_mut(surface).unwrap()
                .set_position(egui::pos2(410.0, 180.0)).set_size(egui::vec2(500.0, 400.0));
        } else { dock::show(&mut app.dock, Pane::Graph); }
        for _ in 0..3 { frame_with(&ctx, &mut app, vec![]); }
        let (surface, node, _) = app.dock.find_tab(&Pane::Graph).unwrap();
        let at = app.dock[surface][node].rect().unwrap().center();
        let mut events = delete_focus_click(at);
        events.push(delete_focus_event());
        frame_with(&ctx, &mut app, events);
        assert!(app.marker_job_running(0), "GRAPH floating={floating} must accept Delete in the click frame");
    }
}

#[test]
fn delete_focus_view_tab_ssh_and_labeling_legend_block_marker_delete() {
    for surface in ["tab", "ssh", "legend"] {
        let ctx = egui::Context::default();
        let mut app = delete_focus_app(&ctx);
        let out = frame_with(&ctx, &mut app, vec![]);
        let at = match surface {
            "tab" => text_rects(&out, "focus-delete.tak").into_iter()
                .find(|r| r.center().y < app.views[0].scene_rect.top() + dock::TAB_H).expect("view name tag").center(),
            "ssh" => text_rects(&out, "SSH (0)").first().expect("SSH section").center(),
            _ => {
                let text = "White = present at current frame · Red = absent · Gaps = missing frames (blank when complete). Gap count and longest gap: select a marker in the Selected Marker panel.";
                text_rects(&out, text).first().expect("Labeling legend").center()
            }
        };
        let mut events = delete_focus_click(at);
        events.push(delete_focus_event());
        frame_with(&ctx, &mut app, events);
        assert!(!app.marker_job_running(0), "Delete leaked from {surface}");
        assert_eq!(key_focus::target(&ctx), key_focus::Target::Blocked, "{surface} must own a blocked marker shortcut target");
    }
}

#[test]
fn delete_focus_settings_help_notice_error_and_warning_windows_block_marker_delete() {
    for title in ["설정", "조작키", "알림", "파일을 열 수 없어요", "읽기 경고"] {
        let ctx = egui::Context::default();
        let mut app = delete_focus_app(&ctx);
        match title {
            "설정" => app.show_settings = true,
            "조작키" => app.show_help = true,
            "알림" => app.notice = Some("focus notice".into()),
            "파일을 열 수 없어요" => app.error = Some("focus error".into()),
            _ => app.show_warnings = true,
        }
        let mut out = frame_with(&ctx, &mut app, vec![]);
        for _ in 0..2 { out = frame_with(&ctx, &mut app, vec![]); }
        let at = text_rects(&out, title).first().expect(title).center();
        let mut events = delete_focus_click(at);
        events.push(delete_focus_event());
        frame_with(&ctx, &mut app, events);
        assert!(!app.marker_job_running(0), "Delete leaked through {title} window");
        assert_eq!(key_focus::target(&ctx), key_focus::Target::Blocked);
    }
}

#[test]
fn delete_focus_search_input_and_open_popup_keep_priority_in_click_frame() {
    let ctx = egui::Context::default();
    let mut app = delete_focus_app(&ctx);
    let out = frame_with(&ctx, &mut app, vec![]);
    let at = text_rects(&out, "Search").first().expect("Labeling search input").center();
    let mut events = delete_focus_click(at);
    events.push(delete_focus_event());
    frame_with(&ctx, &mut app, events);
    assert!(ctx.wants_keyboard_input());
    assert!(!app.marker_job_running(0));

    let ctx = egui::Context::default();
    let mut app = delete_focus_app(&ctx);
    ctx.memory_mut(|memory| memory.open_popup(egui::Id::new("delete_focus_popup")));
    let at = app.views[0].graph_rect.unwrap().center();
    let mut events = delete_focus_click(at);
    events.push(delete_focus_event());
    frame_with(&ctx, &mut app, events);
    assert!(!app.marker_job_running(0), "a popup present at frame start keeps priority even if a click dismisses it");
}

#[test]
fn delete_focus_ctrl_a_selects_only_the_clicked_marker_list() {
    for (row, expected) in [("A", vec!["A", "B"]), ("U1", vec!["U1", "U2"])] {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let mut take = views::sample_take("list-focus.tak", &["A", "B", "U1", "U2"]);
        for (index, track) in take.trajectories[..2].iter_mut().enumerate() {
            track.kind = tak_core::tak::MarkerKind::OtherParent;
            track.parent_uid = Some([7; 16]);
            track.slot = Some(index as i32 + 1);
        }
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(tak_core::view_data::ViewData::build(take), rx, 10, 1.0, 0.0);
        let mut out = frame_with(&ctx, &mut app, vec![]);
        for _ in 0..3 { out = frame_with(&ctx, &mut app, vec![]); }
        let at = text_rects(&out, row).first().expect("marker list row").center();
        let mut events = delete_focus_click(at);
        let modifiers = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
        events.push(egui::Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false, modifiers });
        frame_with(&ctx, &mut app, events);
        let data = app.views[0].data.as_ref().unwrap();
        let mut selected: Vec<_> = app.views[0].selected.iter().map(|&index| data.markers[index].name.as_str()).collect();
        selected.sort();
        assert_eq!(selected, expected, "Ctrl+A leaked outside the clicked {row} list");
    }
}

#[test]
fn delete_focus_graph_name_tag_footer_and_overlay_legends_block_marker_delete() {
    for floating in [true, false] {
        for overlay_legend in [false, true] {
            let ctx = egui::Context::default();
            let mut app = delete_focus_app(&ctx);
            if !floating { app.set_graph_float(false); }
            if overlay_legend {
                // Overlay matching intentionally excludes Unlabeled tracks: these fixtures
                // explicitly belong to a label group in both files, with the same names.
                let labeled = |path: &str| {
                    let mut take = views::sample_take(path, &["M", "N"]);
                    for (index, track) in take.trajectories.iter_mut().enumerate() {
                        track.kind = tak_core::tak::MarkerKind::OtherParent;
                        track.parent_uid = Some([7; 16]);
                        track.slot = Some(index as i32 + 1);
                    }
                    tak_core::view_data::ViewData::build(take)
                };
                let (_tx, rx) = std::sync::mpsc::channel();
                app.views[0].show(labeled("focus-delete.tak"), rx, 11, 1.0, 0.0);
                app.views[0].select(0, false);
                app.views[0].overlay = Some(views::Overlay::from_data(
                    "focus-overlay.tak".into(), labeled("focus-overlay.tak"),
                ));
                app.views[0].shown = [true, true];
            }
            let mut out = frame_with(&ctx, &mut app, vec![]);
            for _ in 0..3 { out = frame_with(&ctx, &mut app, vec![]); }
            let bounds = if floating { app.views[0].graph_rect.unwrap() }
                else { place_of(&app, Pane::Graph) };
            let at = if overlay_legend {
                assert!(app.views[0].lanes.drew().legend, "line-style legend must be drawn");
                text_rects(&out, "1").into_iter().filter(|rect| bounds.contains(rect.center())
                    && rect.center().y < bounds.top() + 60.0)
                    .max_by(|a, b| a.center().x.total_cmp(&b.center().x)).expect("overlay curve legend 1").center()
            } else {
                text_rects(&out, "M").into_iter().find(|rect| bounds.contains(rect.center()))
                    .expect("floating graph name tag / GRAPH footer name").center()
            };
            let mut events = delete_focus_click(at);
            events.push(delete_focus_event());
            frame_with(&ctx, &mut app, events);
            assert!(!app.marker_job_running(0), "Delete leaked from graph legend: floating={floating} overlay={overlay_legend}");
            assert_eq!(key_focus::target(&ctx), key_focus::Target::Blocked);
        }
    }
}
