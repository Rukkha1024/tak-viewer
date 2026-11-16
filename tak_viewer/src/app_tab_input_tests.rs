// Ported from audit_tabs/independent_probe.rs. Use loaded fixtures so these probes
// run on both platforms without external TAK files or environment variables.
fn tab_input_frame(
    ctx: &egui::Context,
    app: &mut TakViewerApp,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 960.0),
        )),
        focused: true,
        events,
        ..Default::default()
    };
    eframe::App::raw_input_hook(app, ctx, &mut raw);
    ctx.run(raw, |ctx| app.frame(ctx))
}

fn tab_key_press(ctx: &egui::Context, app: &mut TakViewerApp, modifiers: Modifiers, key: Key) {
    for pressed in [true, false] {
        tab_input_frame(
            ctx,
            app,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            }],
        );
    }
}

fn middle_click_tab(ctx: &egui::Context, app: &mut TakViewerApp, name: &str) {
    let out = tab_input_frame(ctx, app, vec![]);
    let pos = text_rects(&out, name)[0].center();
    for pressed in [true, false] {
        tab_input_frame(
            ctx,
            app,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Middle,
                    pressed,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
    }
}

#[test]
fn auditor_real_ctrl_tab_repeat_with_separate_key_frames() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    let second = app.new_layout_tab();
    app.open(&ctx, PathBuf::from("a.tak"));
    assert!(app.views[0].data.is_some());
    for expected in [first, second, first, second] {
        tab_key_press(&ctx, &mut app, ctrl(), Key::Tab);
        assert_eq!(
            app.layout_tabs.active_id(),
            Some(expected),
            "every separate down/up press switches"
        );
        assert!(
            ctx.memory(|m| m.focused()).is_none(),
            "Ctrl+Tab must never focus menus"
        );
    }
}

#[test]
fn auditor_switch_shortcut_sequences() {
    for click_before in [false, true] {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let first = app.layout_tabs.active_id().unwrap();
        let second = app.new_layout_tab();
        let third = app.new_layout_tab();
        for _ in 0..3 {
            tab_input_frame(&ctx, &mut app, vec![]);
        }
        for (key, mods, expected) in [
            (Key::Tab, ctrl(), first),
            (Key::Tab, ctrl(), second),
            (
                Key::Tab,
                Modifiers {
                    shift: true,
                    ..ctrl()
                },
                first,
            ),
            (
                Key::Tab,
                Modifiers {
                    shift: true,
                    ..ctrl()
                },
                third,
            ),
            (Key::PageDown, ctrl(), first),
            (Key::PageDown, ctrl(), second),
            (Key::PageUp, ctrl(), first),
            (Key::PageUp, ctrl(), third),
        ] {
            if click_before {
                click_at(&ctx, &mut app, egui::pos2(750.0, 400.0));
            }
            tab_key_press(&ctx, &mut app, mods, key);
            assert_eq!(
                app.layout_tabs.active_id(),
                Some(expected),
                "click={click_before}, key={key:?}, shift={}",
                mods.shift
            );
            assert!(ctx.memory(|m| m.focused()).is_none());
        }
    }
}

#[test]
fn auditor_middle_click_active_and_inactive_tabs() {
    for active in [true, false] {
        let ctx = egui::Context::default();
        let mut app = two_views(&ctx);
        let first = app.layout_tabs.active_id().unwrap();
        let second = app.new_layout_tab();
        let target = if active { second } else { first };
        for _ in 0..3 {
            tab_input_frame(&ctx, &mut app, vec![]);
        }
        let name = app.layout_tabs.tab(target).unwrap().name.clone();
        let count = app.history.undo.len();
        middle_click_tab(&ctx, &mut app, &name);
        assert!(
            app.layout_tabs.tab(target).is_none(),
            "middle click closes active={active}"
        );
        assert_eq!(app.layout_tabs.tabs().len(), 1);
        assert_eq!(
            app.history.undo.len(),
            count + 1,
            "close adds only one action, no activation"
        );
        assert_eq!(app.layout_tabs.closed_items().count(), 1);
        if !active {
            assert_eq!(app.layout_tabs.active_id(), Some(second));
        }
        app.undo_redo(&ctx, false);
        assert!(app.layout_tabs.tab(target).is_some());
        assert_eq!(
            app.layout_tabs.active_id(),
            Some(second),
            "undo preserves the pre-close active tab"
        );
        app.undo_redo(&ctx, true);
        tab_key_press(
            &ctx,
            &mut app,
            Modifiers {
                shift: true,
                ..ctrl()
            },
            Key::T,
        );
        assert_eq!(app.layout_tabs.active_id(), Some(target));
        assert_eq!(
            app.open_views().len(),
            if active { 0 } else { 2 },
            "reopen restores the whole layout"
        );
    }
}

#[test]
fn tab_input_nontext_focus_keeps_switching_without_moving_focus() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    let second = app.new_layout_tab();
    tab_key_press(&ctx, &mut app, Modifiers::NONE, Key::Tab);
    let focused = ctx
        .memory(|m| m.focused())
        .expect("plain Tab still traverses widgets");
    assert!(ctx.wants_keyboard_input());
    assert!(egui::TextEdit::load_state(&ctx, focused).is_none());
    assert!(!app.typing(&ctx), "a menu/button is not a text editor");
    for (key, modifiers, expected) in [
        (Key::Tab, ctrl(), first),
        (Key::Tab, ctrl(), second),
        (
            Key::Tab,
            Modifiers {
                shift: true,
                ..ctrl()
            },
            first,
        ),
        (Key::PageDown, ctrl(), second),
        (Key::PageUp, ctrl(), first),
    ] {
        tab_key_press(&ctx, &mut app, modifiers, key);
        assert_eq!(app.layout_tabs.active_id(), Some(expected));
        assert_eq!(ctx.memory(|m| m.focused()), Some(focused));
    }
}

#[test]
fn tab_input_actual_text_fields_keep_focus_and_block_switches() {
    for field in ["rename", "search", "numeric", "path"] {
        let ctx = egui::Context::default();
        let mut app = one_view(&ctx);
        let second = app.new_layout_tab();
        app.open(&ctx, PathBuf::from("a.tak"));
        for _ in 0..3 {
            tab_input_frame(&ctx, &mut app, vec![]);
        }
        match field {
            "rename" => tab_key_press(&ctx, &mut app, Modifiers::NONE, Key::F2),
            "search" => {
                let out = tab_input_frame(&ctx, &mut app, vec![]);
                click_at(&ctx, &mut app, text_rects(&out, "Search")[0].center());
            }
            "numeric" => {
                let rect = ctx
                    .read_response(panels::frame_box_id(0).with("hot"))
                    .unwrap()
                    .rect;
                click_at(&ctx, &mut app, rect.center());
            }
            _ => {
                app.open_path_dialog();
                tab_input_frame(&ctx, &mut app, vec![]);
            }
        }
        let focused = ctx.memory(|m| m.focused()).expect(field);
        assert!(app.typing(&ctx), "{field} blocks app shortcuts");
        for (key, mods) in [
            (Key::Tab, ctrl()),
            (
                Key::Tab,
                Modifiers {
                    shift: true,
                    ..ctrl()
                },
            ),
            (Key::PageDown, ctrl()),
            (Key::PageUp, ctrl()),
            (Key::T, ctrl()),
        ] {
            tab_key_press(&ctx, &mut app, mods, key);
            assert_eq!(
                app.layout_tabs.active_id(),
                Some(second),
                "{field}: {key:?}"
            );
            assert_eq!(app.layout_tabs.tabs().len(), 2);
            assert_eq!(
                ctx.memory(|m| m.focused()),
                Some(focused),
                "{field}: keep editing focus"
            );
        }
        // Text events still reach the editor, including after blocked Ctrl+Tab.
        tab_key_press(&ctx, &mut app, ctrl(), Key::A);
        tab_input_frame(&ctx, &mut app, vec![egui::Event::Text("7".into())]);
        if field == "search" {
            assert_eq!(app.views[0].labels.filter, "7");
        }
        if field == "rename" {
            tab_key_press(&ctx, &mut app, Modifiers::NONE, Key::Enter);
            assert_eq!(app.layout_tabs.active().unwrap().name, "7");
            tab_key_press(&ctx, &mut app, ctrl(), Key::Tab);
            assert_ne!(
                app.layout_tabs.active_id(),
                Some(second),
                "switching resumes after editing"
            );
        }
    }
}

#[test]
fn tab_input_middle_close_checks_only_last_dirty_reference_and_cancel_restores_origin() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    app.views[0].comments.failed = Some("write failed".into());
    assert!(app.views[0].publish_shared_file());
    let second = app.new_layout_tab();
    app.open(&ctx, PathBuf::from("a.tak"));
    tab_input_frame(&ctx, &mut app, vec![]);
    let count = app.history.undo.len();
    middle_click_tab(&ctx, &mut app, "tab1");
    assert!(app.layout_tabs.tab(first).is_none());
    assert!(
        app.dirty_prompt.is_none(),
        "another view keeps the dirty document"
    );
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    assert_eq!(app.history.undo.len(), count + 1);
    let third = app.new_layout_tab();
    tab_input_frame(&ctx, &mut app, vec![]);
    let before = app.history.undo.len();
    middle_click_tab(&ctx, &mut app, "tab2");
    assert!(
        app.dirty_prompt.is_some(),
        "the final dirty reference prompts"
    );
    assert!(app.layout_tabs.tab(second).is_some());
    assert_eq!(
        app.history.undo.len(),
        before,
        "prompt is not a completed close or activation"
    );
    tab_input_frame(&ctx, &mut app, vec![]);
    let out = tab_input_frame(&ctx, &mut app, vec![]);
    click_at(&ctx, &mut app, text_rects(&out, "Cancel")[0].center());
    assert!(app.dirty_prompt.is_none());
    assert_eq!(
        app.layout_tabs.active_id(),
        Some(third),
        "Cancel restores the original active tab"
    );
    assert_eq!(app.history.undo.len(), before);
    middle_click_tab(&ctx, &mut app, "tab2");
    tab_input_frame(&ctx, &mut app, vec![]);
    let out = tab_input_frame(&ctx, &mut app, vec![]);
    click_at(&ctx, &mut app, text_rects(&out, "Discard")[0].center());
    assert!(app.layout_tabs.tab(second).is_none());
    assert_eq!(app.layout_tabs.active_id(), Some(third));
    assert_eq!(app.history.undo.len(), before + 1);
}

#[test]
fn tab_input_popups_and_held_m_still_block_switches() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let second = app.new_layout_tab();
    for key in [Key::Tab, Key::PageDown] {
        ctx.memory_mut(|m| m.open_popup(egui::Id::new("tab_input_popup")));
        tab_key_press(&ctx, &mut app, ctrl(), key);
        assert_eq!(app.layout_tabs.active_id(), Some(second));
        ctx.memory_mut(|m| m.close_popup());
        key_down(&ctx, &mut app, Key::M);
        tab_key_press(&ctx, &mut app, ctrl(), key);
        assert_eq!(app.layout_tabs.active_id(), Some(second));
        key_up(&ctx, &mut app, Key::M);
    }
    // Extra Alt is not one of the tab-switch shortcuts.
    for key in [Key::Tab, Key::PageDown, Key::PageUp] {
        tab_key_press(
            &ctx,
            &mut app,
            Modifiers {
                alt: true,
                ..ctrl()
            },
            key,
        );
        assert_eq!(app.layout_tabs.active_id(), Some(second));
    }
}

#[test]
fn tab_input_releasing_modifiers_before_tab_does_not_leave_synthetic_keys_held() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    let second = app.new_layout_tab();
    for modifiers in [
        ctrl(),
        Modifiers {
            shift: true,
            ..ctrl()
        },
    ] {
        for expected in [first, second] {
            tab_input_frame(
                &ctx,
                &mut app,
                vec![egui::Event::Key {
                    key: Key::Tab,
                    physical_key: Some(Key::Tab),
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
            );
            assert_eq!(app.layout_tabs.active_id(), Some(expected));
            tab_input_frame(
                &ctx,
                &mut app,
                vec![egui::Event::Key {
                    key: Key::Tab,
                    physical_key: Some(Key::Tab),
                    pressed: false,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
            );
            assert!(ctx.input(|input| !input.key_down(Key::Tab)
                && !input.key_down(Key::PageUp)
                && !input.key_down(Key::PageDown)));
            assert!(ctx.memory(|m| m.focused()).is_none());
        }
    }
}

// A focused non-text egui widget must not suppress the last-clicked panel's list keys.
fn tab_input_focus_nontext_widget(ctx: &egui::Context, app: &mut TakViewerApp) {
    for _ in 0..30 {
        tab_key_press(ctx, app, Modifiers::NONE, Key::Tab);
        if let Some(id) = ctx.memory(|m| m.focused()) {
            if egui::TextEdit::load_state(ctx, id).is_none() {
                return;
            }
        }
    }
    panic!("a non-text widget is reachable by ordinary Tab");
}

#[test]
fn merged_focused_comment_list_keeps_tab_switch_and_own_all_delete() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = multi_app(&ctx, "tab_keys_comments");
    let first = app.layout_tabs.active_id().unwrap();
    let path = app.views[0].path().unwrap().to_path_buf();
    let second = app.new_layout_tab();
    app.open(&ctx, path);
    app.show_pane(Pane::Comments);
    app.switch_layout_tab(first);
    multi_click(&ctx, &mut app, 2, Modifiers::NONE, 2);
    tab_input_focus_nontext_widget(&ctx, &mut app);
    assert_eq!(key_focus::target(&ctx), key_focus::Target::Comments);
    for expected in [second, first] {
        tab_key_press(&ctx, &mut app, ctrl(), Key::Tab);
        assert_eq!(app.layout_tabs.active_id(), Some(expected));
    }
    assert_eq!(key_focus::target(&ctx), key_focus::Target::Comments);
    let marker_count = app.views[0].data.as_ref().unwrap().markers.len();
    tab_key_press(&ctx, &mut app, ctrl(), Key::A);
    assert_eq!(
        multi_ids(&app),
        vec![1, 2, 3, 4, 5],
        "focused list still owns Ctrl+A"
    );
    tab_key_press(&ctx, &mut app, Modifiers::NONE, Key::Delete);
    assert!(app.views[0].comments.list.is_empty());
    assert_eq!(
        app.views[0].data.as_ref().unwrap().markers.len(),
        marker_count
    );
    assert!(
        !app.marker_job_running(0),
        "Comment Delete never reaches markers"
    );
    multi_drain(&ctx, &mut app);
}

#[test]
fn merged_focused_editors_list_keeps_tab_switch_and_own_all_delete() {
    let ctx = egui::Context::default();
    let mut app = two_views(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    let second = app.new_layout_tab();
    app.switch_layout_tab(first);
    let out = frame_with(&ctx, &mut app, vec![]);
    let pane = place_of(&app, Pane::Explorer);
    let at = text_rects(&out, "a.tak")
        .into_iter()
        .find(|r| pane.contains(r.center()))
        .unwrap()
        .center();
    click_at(&ctx, &mut app, at);
    tab_input_focus_nontext_widget(&ctx, &mut app);
    assert_eq!(key_focus::target(&ctx), key_focus::Target::Editors);
    for expected in [second, first] {
        tab_key_press(&ctx, &mut app, ctrl(), Key::Tab);
        assert_eq!(app.layout_tabs.active_id(), Some(expected));
    }
    tab_key_press(&ctx, &mut app, ctrl(), Key::A);
    assert_eq!(app.editor_selection.selected.len(), 2);
    let before = app.history.undo.len();
    tab_key_press(&ctx, &mut app, Modifiers::NONE, Key::Delete);
    assert!(app.layout_tabs.tab(first).is_none());
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    assert_eq!(app.history.undo.len(), before + 1);
    assert!(!app.marker_job_running(0));
}

#[test]
fn merged_focused_marker_lists_keep_own_all_delete() {
    for named in [false, true] {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let mut take = views::sample_take("focus-delete.tak", &["M", "N"]);
        if named {
            for (i, track) in take.trajectories.iter_mut().enumerate() {
                track.kind = tak_core::tak::MarkerKind::OtherParent;
                track.parent_uid = Some([7; 16]);
                track.slot = Some(i as i32 + 1);
            }
        }
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(tak_core::view_data::ViewData::build(take), rx, 10, 1.0, 0.0);
        app.views[0].select(0, false);
        for _ in 0..3 {
            frame_with(&ctx, &mut app, vec![]);
        }
        let out = frame_with(&ctx, &mut app, vec![]);
        let pane = place_of(&app, Pane::Labeling);
        let at = text_rects(&out, "M")
            .into_iter()
            .find(|r| pane.contains(r.center()))
            .unwrap()
            .center();
        click_at(&ctx, &mut app, at);
        tab_input_focus_nontext_widget(&ctx, &mut app);
        assert_eq!(
            key_focus::target(&ctx),
            if named {
                key_focus::Target::NamedMarkers(0)
            } else {
                key_focus::Target::UnlabeledMarkers(0)
            }
        );
        tab_key_press(&ctx, &mut app, ctrl(), Key::A);
        assert_eq!(app.views[0].selected, BTreeSet::from([0, 1]));
        tab_input_frame(&ctx, &mut app, vec![delete_focus_event()]);
        assert!(
            app.marker_job_running(0),
            "marker list owns its marker Delete, named={named}"
        );
        assert!(app.views[0].comments.list.is_empty());
    }
}

#[test]
fn merged_text_search_owns_all_delete_while_tab_switch_stays_blocked() {
    let ctx = egui::Context::default();
    let mut app = delete_focus_app(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    app.new_layout_tab();
    app.switch_layout_tab(first);
    let out = frame_with(&ctx, &mut app, vec![]);
    click_at(&ctx, &mut app, text_rects(&out, "Search")[0].center());
    tab_input_frame(
        &ctx,
        &mut app,
        vec![egui::Event::Text("marker search".into())],
    );
    assert_eq!(app.views[0].labels.filter, "marker search");
    tab_key_press(&ctx, &mut app, ctrl(), Key::Tab);
    assert_eq!(app.layout_tabs.active_id(), Some(first));
    tab_key_press(&ctx, &mut app, ctrl(), Key::A);
    tab_key_press(&ctx, &mut app, Modifiers::NONE, Key::Delete);
    assert!(
        app.views[0].labels.filter.is_empty(),
        "Ctrl+A and Delete belong to the text editor"
    );
    assert_eq!(app.views[0].selected, BTreeSet::from([0]));
    assert!(!app.marker_job_running(0));
}
