mod label_assignment {
    use super::*;
    use super::close_wait_until as wait;
    use crate::label_increment::IncrementMode;
    use crate::ui::labeling::LabelingMode;
    use tak_core::edit::{DocumentSnapshot, LabelKey};
    use tak_core::tak::Uid;
    use tak_core::test_support::TempDir;

    const A: Uid = [1; 16];
    const B: Uid = [2; 16];
    const KEY_A: LabelKey = key(1);
    const KEY_B: LabelKey = key(2);

    const fn key(slot: i32) -> LabelKey {
        LabelKey { parent: [90; 16], slot }
    }

    include!("app_scene_marker_menu_tests.rs");

    fn fixture(ctx: &egui::Context) -> (TempDir, TakViewerApp) {
        fixture_from(ctx, tak_core::sample_tak::sample_tak_for_label_assignments())
    }

    fn fixture_from(ctx: &egui::Context, bytes: Vec<u8>) -> (TempDir, TakViewerApp) {
        let dir = TempDir::new("tak_viewer_label_assign");
        let path = dir.join("labels.takm");
        tak_core::takm::write(
            &mut std::fs::File::create(&path).unwrap(),
            &bytes,
            "synthetic.tak",
            "",
        )
        .unwrap();
        let mut app = TakViewerApp::build(ctx, None, vec![path.clone()], None, None);
        wait(ctx, &mut app, |app| {
            app.views[0].path() == Some(path.as_path()) && !app.views[0].busy()
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
            .contains("3 to Unlabeled"));

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
    fn same_label_click_advances_with_one_view_undo_and_no_document_edit() {
        let ctx = egui::Context::default();
        let (_dir, mut app) = fixture(&ctx);
        let before = app.views[0].edit_selection_state();
        let revision = snapshot(&app).revision();
        let history = steps(&app, 0);
        let point = marker_on_screen(&app, 0);
        click_at(&ctx, &mut app, point);
        assert_eq!(app.takm_note.as_ref().unwrap().0, "Label · A · already set");
        let mut after = before.clone();
        after.armed_label = Some(KEY_B);
        assert_eq!(app.views[0].edit_selection_state(), after);
        assert_eq!(steps(&app, 0), (history.0 + 1, 0));

        undo_key(&ctx, &mut app);
        assert_eq!(app.views[0].edit_selection_state(), before);
        assert_eq!(steps(&app, 0), (history.0, 1));
        redo_key(&ctx, &mut app);
        assert_eq!(app.views[0].edit_selection_state(), after);
        assert_eq!(steps(&app, 0), (history.0 + 1, 0));
        assert_eq!(snapshot(&app).revision(), revision);
        assert!(!snapshot(&app).is_loaded(), "confirmation needs no edit preparation");
        assert!(!app.views[0].marker_dirty());
        assert!(!app.marker_job_running(0));
    }

    #[test]
    fn same_label_next_missing_skips_present_labels() {
        let ctx = egui::Context::default();
        let (_dir, mut app) = fixture_from(&ctx, tak_core::sample_tak::sample_tak_for_label_walk());
        app.settings.labeling_increment = IncrementMode::NextMissing;
        app.views[0].pb.set_frame(8, 0.0);
        app.views[0].set_range(8, 8);
        frame_with(&ctx, &mut app, vec![]);
        app.quiet();
        let data = app.views[0].data.as_ref().unwrap();
        assert!(data.markers[1].present(8));
        assert!(!data.markers[2].present(8));
        let point = marker_on_screen(&app, 0);
        click_at(&ctx, &mut app, point);
        assert_eq!(app.views[0].armed_label, Some(key(3)));
        assert!(!app.views[0].marker_dirty());
    }

    #[test]
    fn same_label_stay_and_last_row_keep_the_label_without_history() {
        for (mode, at, key, status) in [
            (IncrementMode::Stay, 0, KEY_A, "Label · A · already set"),
            (IncrementMode::Next, 1, KEY_B, "Label · B · already set · No next label"),
            (IncrementMode::NextMissing, 0, KEY_A, "Label · A · already set · No next label"),
        ] {
            let ctx = egui::Context::default();
            let (_dir, mut app) = fixture(&ctx);
            app.settings.labeling_increment = mode;
            app.views[0].armed_label = Some(key);
            app.observe(false);
            let history = steps(&app, 0);
            let point = marker_on_screen(&app, at);
            click_at(&ctx, &mut app, point);
            assert_eq!(app.takm_note.as_ref().unwrap().0, status);
            assert_eq!(app.views[0].armed_label, Some(key));
            assert_eq!(steps(&app, 0), history);
            assert!(!app.views[0].marker_dirty());
        }
    }

    #[test]
    fn same_label_hidden_target_starts_before_first_visible_row() {
        let ctx = egui::Context::default();
        let (_dir, mut app) = fixture(&ctx);
        app.views[0].labels.filter = "A".into();
        app.views[0].armed_label = Some(KEY_B);
        app.refresh_marker_ui(0);
        assert_eq!(crate::ui::labeling::visible_labels(&app.marker_ui[0].labeling), [KEY_A]);
        app.observe(false);
        let point = marker_on_screen(&app, 1);
        click_at(&ctx, &mut app, point);
        assert_eq!(app.views[0].armed_label, Some(KEY_A));
        assert!(!app.views[0].marker_dirty());
    }

    #[test]
    fn same_label_confirms_only_when_a_point_exists_in_scope() {
        for (range, forward, expected, status) in [
            ((6, 6), true, KEY_A, "선택한 범위에 바꿀 점이 없어요."),
            ((7, 7), false, KEY_A, "선택한 범위에 바꿀 점이 없어요."),
            ((7, 7), true, KEY_B, "Label · A · already set"),
        ] {
            let ctx = egui::Context::default();
            let (_dir, mut app) = fixture(&ctx);
            app.marker_ui[0].labeling.forward = forward;
            app.views[0].set_range(range.0, range.1);
            app.observe(false);
            let count = steps(&app, 0).0;
            let point = marker_on_screen(&app, 0);
            click_at(&ctx, &mut app, point);
            assert_eq!(app.takm_note.as_ref().unwrap().0, status);
            assert_eq!(app.views[0].armed_label, Some(expected));
            assert_eq!(steps(&app, 0).0, count + usize::from(expected != KEY_A));
            assert!(!snapshot(&app).is_loaded());
            assert!(!app.views[0].marker_dirty());
        }
    }

    #[test]
    fn five_scene_clicks_reach_sixth_label_with_three_document_edits() {
        let ctx = egui::Context::default();
        let (_dir, mut app) = fixture_from(&ctx, tak_core::sample_tak::sample_tak_for_label_walk());
        app.settings.graph_float = false;
        let clicked_position = |app: &TakViewerApp, marker: usize| {
            app.views[0].data.as_ref().unwrap().markers[marker].position(5).unwrap()
        };
        // Assign C's point to B, then assign the displaced B point to C.
        let positions = [0, 2, 1, 3, 6].map(|marker| clicked_position(&app, marker));
        // Frame both ends of the fixture, including the farthest Unlabeled point.
        let view = &mut app.views[0];
        view.cam.frame_box(positions[0], positions[4], view.scene_rect.aspect_ratio());
        frame_with(&ctx, &mut app, vec![]);
        app.quiet();
        let undo_count = steps(&app, 0).0;
        let edits = [false, true, true, false, true];
        let mut revisions = vec![snapshot(&app).revision()];
        for (index, position) in positions.into_iter().enumerate() {
            idle(&ctx, &mut app);
            assert_eq!(app.views[0].armed_label, Some(key(index as i32 + 1)));
            let data = app.views[0].data.as_ref().unwrap();
            let marker = data.markers.iter().position(|m| m.position(5) == Some(position)).unwrap();
            let point = marker_on_screen(&app, marker);
            assert!(app.views[0].scene_rect.contains(point), "click {} is outside 3D", index + 1);
            click_at(&ctx, &mut app, point);
            if edits[index] {
                wait_for_close_edit(&ctx, &mut app, revisions[index]);
            }
            assert_eq!(app.views[0].armed_label, Some(key(index as i32 + 2)), "{:?}", app.takm_note);
            assert_eq!(steps(&app, 0).0, undo_count + index + 1);
            revisions.push(snapshot(&app).revision());
        }
        for (pair, edited) in revisions.windows(2).zip(edits) {
            assert_eq!(pair[0] != pair[1], edited);
        }
        // Undo all five steps. Each edit must be one history entry, with its armed row.
        for index in (0..5).rev() {
            undo_key(&ctx, &mut app);
            if edits[index] {
                close_wait_until(&ctx, &mut app, |app| {
                    let view = &app.views[0];
                    view.data.as_ref().unwrap().edit_revision == Some(revisions[index])
                        && view.edit_refresh.is_none()
                });
            }
            assert_eq!(app.views[0].armed_label, Some(key(index as i32 + 1)));
            assert_eq!(snapshot(&app).revision(), revisions[index]);
        }
        assert!(!app.views[0].marker_dirty());
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
