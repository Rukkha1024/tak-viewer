mod label_assignment {
    use super::*;
    use crate::ui::labeling::LabelingMode;
    use tak_core::edit::{DocumentSnapshot, LabelKey};
    use tak_core::tak::Uid;
    use tak_core::test_support::TempDir;

    const A: Uid = [1; 16];
    const B: Uid = [2; 16];
    const KEY_A: LabelKey = LabelKey {
        parent: [90; 16],
        slot: 1,
    };
    const KEY_B: LabelKey = LabelKey {
        parent: [90; 16],
        slot: 2,
    };

    fn wait(ctx: &egui::Context, app: &mut TakViewerApp, ready: impl Fn(&TakViewerApp) -> bool) {
        let start = Instant::now();
        while !ready(app) {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "{:?}",
                app.takm_note
            );
            frame_with(ctx, app, vec![]);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn fixture(ctx: &egui::Context) -> (TempDir, TakViewerApp) {
        let dir = TempDir::new("tak_viewer_label_assign");
        let path = dir.join("labels.takm");
        let bytes = tak_core::sample_tak::sample_tak_for_label_assignments();
        tak_core::takm::write(
            &mut std::fs::File::create(&path).unwrap(),
            &bytes,
            "synthetic.tak",
            "",
        )
        .unwrap();
        let mut app = TakViewerApp::build(ctx, None, vec![path], None, None);
        wait(ctx, &mut app, |app| {
            app.views[0].data.is_some() && !app.views[0].busy()
        });
        let view = &mut app.views[0];
        view.pb.set_frame(5, 0.0);
        view.set_range(3, 7);
        view.armed_label = Some(KEY_A);
        view.select(0, false);
        frame_with(ctx, &mut app, vec![]);
        app.quiet();
        (dir, app)
    }

    fn snapshot(app: &TakViewerApp) -> DocumentSnapshot {
        app.views[0]
            .edit_document
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .snapshot()
    }

    #[test]
    fn named_scene_click_is_one_edit_and_undo_redo_restore_view_state() {
        let ctx = egui::Context::default();
        let (_dir, mut app) = fixture(&ctx);
        let before = app.views[0].edit_selection_state();
        let undo_count = steps(&app, 0).0;
        let old_b = marker_on_screen(&app, 1);
        click_at(&ctx, &mut app, old_b);
        wait(&ctx, &mut app, |app| !app.marker_job_running(0));
        assert_eq!(steps(&app, 0), (undo_count + 1, 0), "{:?}", app.takm_note);
        assert_eq!(app.views[0].armed_label, Some(KEY_B));
        let after = app.views[0].edit_selection_state();
        let edited = snapshot(&app);
        let displaced = edited.tracks().find(|track| track.label.is_none()).unwrap();
        let uid = displaced.uid;
        assert_eq!(displaced.samples.len(), 3);
        assert_eq!(app.views[0].primary, Some(0));
        assert!(app
            .takm_note
            .as_ref()
            .unwrap()
            .0
            .contains("3프레임 Unlabeled"));

        undo_key(&ctx, &mut app);
        wait(&ctx, &mut app, |app| app.views[0].edit_refresh.is_none());
        let original = snapshot(&app);
        assert!(original.track(&uid).is_none());
        assert_eq!(app.views[0].edit_selection_state(), before);
        for (from, to) in [(A, uid), (B, A)] {
            for sample in edited
                .track(&to)
                .unwrap()
                .samples
                .iter()
                .filter(|s| [3, 5, 7].contains(&s.frame()))
            {
                assert!(original.track(&from).unwrap().samples.contains(sample));
            }
        }

        redo_key(&ctx, &mut app);
        wait(&ctx, &mut app, |app| app.views[0].edit_refresh.is_none());
        assert_eq!(app.views[0].edit_selection_state(), after);
        assert_eq!(snapshot(&app).track(&uid), edited.track(&uid));
        assert_eq!(steps(&app, 0), (undo_count + 1, 0));
    }

    #[test]
    fn same_label_click_leaves_selection_history_and_document_alone() {
        let ctx = egui::Context::default();
        let (_dir, mut app) = fixture(&ctx);
        let before = app.views[0].edit_selection_state();
        let history = steps(&app, 0);
        let point = marker_on_screen(&app, 0);
        click_at(&ctx, &mut app, point);
        assert_eq!(app.takm_note.as_ref().unwrap().0, "이미 이 라벨이에요.");
        assert_eq!(app.views[0].edit_selection_state(), before);
        assert_eq!(steps(&app, 0), history);
        assert!(!snapshot(&app).is_loaded());
        assert!(!app.marker_job_running(0));
    }

    #[test]
    fn select_mode_and_modifier_clicks_only_select() {
        for (mode, modifiers) in [
            (LabelingMode::Select, Modifiers::NONE),
            (LabelingMode::QuickLabel, Modifiers::SHIFT),
            (LabelingMode::QuickLabel, ctrl()),
            (LabelingMode::QuickLabel, Modifiers::ALT),
        ] {
            let ctx = egui::Context::default();
            let (_dir, mut app) = fixture(&ctx);
            app.marker_ui[0].labeling.mode = mode;
            let point = marker_on_screen(&app, 1);
            click_with(&ctx, &mut app, point, modifiers);
            assert_eq!(app.views[0].armed_label, Some(KEY_A));
            assert_eq!(app.views[0].primary, Some(1));
            assert!(!snapshot(&app).is_loaded());
            assert!(!app.marker_job_running(0));
        }
    }
}
