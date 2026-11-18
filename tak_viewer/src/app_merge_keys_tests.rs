#[test]
fn merge_keys_buttons_move_by_row_order_and_use_one_history_action() {
    use tak_core::edit::{DocumentSnapshot, PreparedInput};

    let temp = tak_core::test_support::TempDir::new("merge_keys_viewer");
    let path = temp.join("keys.takm");
    tak_core::takm::write(
        &mut std::fs::File::create(&path).unwrap(),
        &tak_core::sample_tak::sample_tak_with_devices(),
        "keys.tak",
        "2026-11-18",
    )
    .unwrap();
    let (input, _) = PreparedInput::read_from_path(&path).unwrap();
    let mut document = tak_core::edit::EditDocument::unloaded(path.clone());
    document
        .install_prepared(document.revision(), input)
        .unwrap();
    let base = document.snapshot();
    let l = base
        .tracks()
        .find(|track| track.name == "LKNE")
        .unwrap()
        .uid;
    let r = base
        .tracks()
        .find(|track| track.name == "RKNE")
        .unwrap()
        .uid;
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![path], None, None);
    app.set_graph_float(false);
    app.settings.graph_layout = graph::tracks::Layout::Tracks;
    let wait = |app: &mut TakViewerApp, ready: &dyn Fn(&TakViewerApp) -> bool| {
        let start = Instant::now();
        loop {
            let out = frame_with(&ctx, app, vec![]);
            if ready(app) {
                return out;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "{:?}",
                app.takm_note
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    let settled =
        |app: &TakViewerApp| {
            app.view().data.as_ref().is_some_and(|data| {
                !data.analog_pending && data.marker_order_handle().get().is_some()
            }) && !app.marker_job_running(0)
                && app.view().edit_refresh.is_none()
        };
    let button = |out: &egui::FullOutput, label| text_rects(out, label)[0].center();
    let snapshot = |app: &TakViewerApp| {
        app.view()
            .edit_document
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .snapshot()
    };
    let assert_base = |snapshot: &DocumentSnapshot| {
        for uid in [l, r] {
            assert_eq!(
                snapshot.track(&uid).unwrap().samples,
                base.track(&uid).unwrap().samples
            );
        }
    };
    let select = |app: &mut TakViewerApp| {
        let data = app.view().data.as_ref().unwrap();
        let index = |uid| data.markers.iter().position(|m| m.uid == uid).unwrap();
        let (upper, lower) = (index(l), index(r));
        // Deliberately select from bottom to top.
        app.views[0].select_many(vec![lower, upper], Some(upper), false);
        app.quiet();
    };

    let out = wait(&mut app, &settled);
    click_at(&ctx, &mut app, button(&out, "Merge Keys Up"));
    assert!(
        !snapshot(&app).is_loaded(),
        "empty selection must disable the button"
    );

    select(&mut app);
    let history = steps(&app, 0);
    let out = wait(&mut app, &settled);
    click_at(&ctx, &mut app, button(&out, "Merge Keys Up"));
    wait(&mut app, &settled);
    let note = &app.takm_note.as_ref().unwrap().0;
    assert!(note.contains("260 frames overlap"), "{note}");
    assert!(note.contains("first 0"), "{note}");
    assert_eq!(steps(&app, 0), history);
    assert_base(&snapshot(&app));

    app.views[0].pb.set_frame(50, 0.0);
    app.quiet();
    let (body, band) = tracks_body_and_band(&wait(&mut app, &settled));
    let data = app.view().data.as_ref().unwrap();
    let point = |f| egui::pos2(tracks_frame_x(band, data.start_frame, data.end_frame, f), body.top() + 9.5);
    let (from, to) = (point(100), point(139));
    drag(&ctx, &mut app, from, to);
    assert_eq!(app.view().range, Some((100, 139)));
    assert_eq!(app.view().edit_selection_state().order, [r, l]);
    let history = steps(&app, 0).0;
    let out = wait(&mut app, &settled);
    click_at(&ctx, &mut app, button(&out, "Merge Keys Down"));
    wait(&mut app, &settled);
    let merged = snapshot(&app);
    assert_eq!(
        merged.track(&r).unwrap().samples[100..140],
        base.track(&l).unwrap().samples[100..140]
    );
    assert_eq!(merged.track(&l).unwrap().samples.len(), 260);
    assert_eq!(app.view().edit_selection_state().primary, Some(r));
    assert_eq!(app.view().pb.frame, 50);
    assert_eq!(app.view().range, Some((100, 139)));
    assert_eq!(steps(&app, 0).0, history + 1);

    undo_key(&ctx, &mut app);
    wait(&mut app, &settled);
    assert_base(&snapshot(&app));
    assert_eq!(app.view().edit_selection_state().order, [r, l]);
    redo_key(&ctx, &mut app);
    wait(&mut app, &settled);
    for uid in [l, r] {
        assert_eq!(
            snapshot(&app).track(&uid).unwrap().samples,
            merged.track(&uid).unwrap().samples
        );
    }

    select(&mut app);
    let out = wait(&mut app, &settled);
    click_at(&ctx, &mut app, button(&out, "Merge Keys Up"));
    wait(&mut app, &settled);
    assert_base(&snapshot(&app));
    assert_eq!(app.view().edit_selection_state().primary, Some(l));
}
