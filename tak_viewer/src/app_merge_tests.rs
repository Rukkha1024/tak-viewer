#[test]
fn merged_layout_strip_click_and_delete_never_erase_selected_marker_samples() {
    let ctx = egui::Context::default();
    let mut app = delete_focus_app(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    let out = frame_with(&ctx, &mut app, vec![]);
    let name = app.layout_tabs.tab(first).unwrap().name.clone();
    let at = text_rects(&out, &name).first().expect("top-level tab").center();
    let mut events = delete_focus_click(at);
    events.push(delete_focus_event());
    frame_with(&ctx, &mut app, events);
    assert_eq!(key_focus::target(&ctx), key_focus::Target::Blocked);
    assert!(!app.marker_job_running(0));
    assert_eq!(app.views[0].selected, BTreeSet::from([0]));
}

#[test]
fn merged_layout_rename_keeps_delete_away_from_marker_keys() {
    let ctx = egui::Context::default();
    let mut app = delete_focus_app(&ctx);
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::F2)]);
    assert!(app.layout_strip.is_renaming());
    frame_with(&ctx, &mut app, vec![delete_focus_event()]);
    assert!(!app.marker_job_running(0));
    assert_eq!(app.views[0].selected, BTreeSet::from([0]));
}

#[test]
fn merged_nonmarker_floating_forceplate_plot_blocks_delete() {
    let ctx = egui::Context::default();
    let mut app = views_with_plates(&ctx, 1);
    app.views[0].plate = Some(0);
    for _ in 0..3 { frame_with(&ctx, &mut app, vec![]); }
    let at = app.views[0].plate_rect.expect("forceplate graph").center();
    let mut events = delete_focus_click(at);
    events.push(delete_focus_event());
    frame_with(&ctx, &mut app, events);
    assert!(!app.marker_job_running(0), "forceplate plot Delete leaked to markers");
    assert_eq!(key_focus::target(&ctx), key_focus::Target::Blocked);
}

#[test]
fn merged_nonmarker_floating_graph_resize_edge_blocks_delete() {
    let ctx = egui::Context::default();
    let mut app = delete_focus_app(&ctx);
    let at = app.views[0].graph_rect.expect("marker graph").center_bottom();
    let mut events = delete_focus_click(at);
    events.push(delete_focus_event());
    frame_with(&ctx, &mut app, events);
    assert!(!app.marker_job_running(0), "graph resize handle Delete leaked to markers");
    assert_eq!(key_focus::target(&ctx), key_focus::Target::Blocked);
}
