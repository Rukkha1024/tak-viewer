mod scene_marker_menu {
    use super::*;
    use eframe::egui::{Event, PointerButton, Pos2, Rect, Shape};
    use tak_core::edit::{EditDocument, PreparedInput, RawSample, RawTrack};

    fn menu_item(output: &egui::FullOutput, name: &str) -> Rect {
        fn find(shape: &Shape, name: &str) -> Option<Rect> {
            match shape {
                Shape::Text(text) if text.galley.text() == name => {
                    Some(Rect::from_min_size(text.pos, text.galley.size()))
                }
                Shape::Vec(items) => items.iter().find_map(|shape| find(shape, name)),
                _ => None,
            }
        }
        output
            .shapes
            .iter()
            .rev()
            .find_map(|shape| find(&shape.shape, name))
            .unwrap_or_else(|| panic!("missing menu item {name}"))
    }

    fn open(ctx: &egui::Context, app: &mut TakViewerApp, at: Pos2) -> egui::FullOutput {
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        frame_with(ctx, app, vec![Event::PointerMoved(at)]);
        frame_with(ctx, app, vec![button(true)]);
        frame_with(ctx, app, vec![button(false)]);
        assert!(app.scene_marker_menu.is_open());
        frame_with(ctx, app, vec![])
    }

    fn hover(ctx: &egui::Context, app: &mut TakViewerApp, at: Pos2) -> egui::FullOutput {
        frame_with(ctx, app, vec![Event::PointerMoved(at)]);
        frame_with(ctx, app, vec![])
    }

    fn sample(track: &RawTrack, frame: u32) -> Option<RawSample> {
        track
            .samples
            .iter()
            .find(|sample| sample.frame() == frame)
            .copied()
    }

    fn read_saved(path: PathBuf) -> DocumentSnapshot {
        let (prepared, _) = PreparedInput::read_from_path(&path).unwrap();
        let mut document = EditDocument::unloaded(path);
        document
            .install_prepared(document.revision(), prepared)
            .unwrap();
        document.snapshot()
    }

    fn assert_same_tracks(left: &DocumentSnapshot, right: &DocumentSnapshot) {
        assert_eq!(left.tracks().count(), right.tracks().count());
        for track in left.tracks() {
            let other = right.track(&track.uid).expect("same UID");
            assert_eq!(track.label, other.label);
            assert_eq!(track.samples, other.samples, "exact 16-byte samples");
        }
    }

    fn save_and_read(
        ctx: &egui::Context,
        app: &mut TakViewerApp,
        path: PathBuf,
    ) -> DocumentSnapshot {
        app.save_markers(ctx, 0, false, false);
        wait(ctx, app, |app| !app.marker_saving(0));
        assert!(!app.views[0].marker_dirty(), "{:?}", app.takm_note);
        read_saved(path)
    }

    #[test]
    fn both_label_menus_preserve_collision_samples_with_next_missing_enabled() {
        for scene in [false, true] {
            let ctx = egui::Context::default();
            let (dir, mut app) = fixture(&ctx);
            app.settings.graph_float = false;
            app.settings.labeling_increment = crate::label_increment::IncrementMode::NextMissing;
            app.views[0].select(1, false);
            app.views[0].armed_label = None;
            frame_with(&ctx, &mut app, vec![]);
            app.quiet();
            let count = steps(&app, 0).0;
            let before = snapshot(&app).revision();
            let original = read_saved(dir.join("labels.takm"));

            if scene {
                let point = marker_on_screen(&app, 1);
                let output = open(&ctx, &mut app, point);
                let submenu = hover(&ctx, &mut app, menu_item(&output, "Label Marker").center());
                click_at(&ctx, &mut app, menu_item(&submenu, "A").center());
            } else {
                app.labeling_action(&ctx, 0, crate::ui::labeling::LabelingAction::LabelMarker(KEY_A));
            }

            wait_for_close_edit(&ctx, &mut app, before);
            assert_eq!(steps(&app, 0).0, count + 1, "{:?}", app.takm_note);
            assert_eq!(app.views[0].armed_label, None);
            let edited = snapshot(&app);
            let displaced = edited.tracks().find(|track| track.label.is_none()).unwrap();
            assert_eq!(sample(edited.track(&A).unwrap(), 5), sample(original.track(&B).unwrap(), 5));
            assert_eq!(sample(displaced, 5), sample(original.track(&A).unwrap(), 5));
        }
    }

    #[test]
    fn both_label_menus_keep_armed_label_on_same_label() {
        for scene in [false, true] {
            let ctx = egui::Context::default();
            let (_dir, mut app) = fixture(&ctx);
            app.settings.graph_float = false;
            let revision = snapshot(&app).revision();
            frame_with(&ctx, &mut app, vec![]);
            app.quiet();
            let history = steps(&app, 0);

            if scene {
                let point = marker_on_screen(&app, 0);
                let output = open(&ctx, &mut app, point);
                let submenu = hover(&ctx, &mut app, menu_item(&output, "Label Marker").center());
                click_at(&ctx, &mut app, menu_item(&submenu, "A").center());
            } else {
                app.labeling_action(&ctx, 0, crate::ui::labeling::LabelingAction::LabelMarker(KEY_A));
            }

            assert_eq!(app.takm_note.as_ref().unwrap().0, "Label already set");
            assert_eq!(app.views[0].armed_label, Some(KEY_A));
            assert_eq!(steps(&app, 0), history);
            assert_eq!(snapshot(&app).revision(), revision);
            assert!(!app.views[0].marker_dirty());
        }
    }

    #[test]
    fn marker_menu_label_matches_quicklabel_bytes_and_keeps_the_armed_label() {
        let ctx = egui::Context::default();
        let (dir, mut app) = fixture(&ctx);
        app.settings.graph_float = false;
        let path = dir.join("labels.takm");
        let original = read_saved(path.clone());
        app.views[0].select(1, false);
        app.views[0].armed_label = Some(KEY_B);
        frame_with(&ctx, &mut app, vec![]);
        app.quiet();

        let before_count = steps(&app, 0).0;
        let before_selection = app.views[0].edit_selection_state();
        let point = marker_on_screen(&app, 1);
        let output = open(&ctx, &mut app, point);
        assert_eq!(app.views[0].edit_selection_state(), before_selection);
        menu_item(&output, "Selected Markers (1)");

        let submenu = hover(&ctx, &mut app, menu_item(&output, "Label Marker").center());
        let destination = menu_item(&submenu, "A").center();
        click_at(&ctx, &mut app, destination);
        wait(&ctx, &mut app, |app| !app.marker_job_running(0));
        assert_eq!(steps(&app, 0), (before_count + 1, 0), "{:?}", app.takm_note);
        let edited = snapshot(&app);
        let after_selection = app.views[0].edit_selection_state();
        assert_eq!(app.views[0].armed_label, Some(KEY_B));

        let source = original.track(&B).unwrap();
        let target = original.track(&A).unwrap();
        let displaced = edited.tracks().find(|track| track.label.is_none()).unwrap();
        for frame in [3, 5, 7] {
            assert_eq!(
                sample(edited.track(&A).unwrap(), frame),
                sample(source, frame)
            );
            assert_eq!(sample(displaced, frame), sample(target, frame));
        }
        assert_eq!(sample(edited.track(&A).unwrap(), 4), sample(target, 4));
        assert_eq!(sample(edited.track(&B).unwrap(), 8), sample(source, 8));
        undo_key(&ctx, &mut app);
        wait(&ctx, &mut app, |app| app.views[0].edit_refresh.is_none());
        assert_same_tracks(&snapshot(&app), &original);
        assert_eq!(app.views[0].edit_selection_state(), before_selection);
        redo_key(&ctx, &mut app);
        wait(&ctx, &mut app, |app| app.views[0].edit_refresh.is_none());
        assert_same_tracks(&snapshot(&app), &edited);
        assert_eq!(app.views[0].edit_selection_state(), after_selection);
        assert_same_tracks(&save_and_read(&ctx, &mut app, path), &edited);

        let quick_ctx = egui::Context::default();
        let (_dir, mut quick) = fixture(&quick_ctx);
        let point = marker_on_screen(&quick, 1);
        click_at(&quick_ctx, &mut quick, point);
        wait(&quick_ctx, &mut quick, |app| !app.marker_job_running(0));
        let quick_edit = snapshot(&quick);
        for uid in [A, B] {
            assert_eq!(
                edited.track(&uid).unwrap().samples,
                quick_edit.track(&uid).unwrap().samples
            );
        }
        assert_eq!(
            displaced.samples,
            quick_edit
                .tracks()
                .find(|t| t.label.is_none())
                .unwrap()
                .samples
        );
        assert_eq!(quick.views[0].armed_label, Some(KEY_B));
    }

    #[test]
    fn marker_menu_unlabels_the_named_selection_and_reopens_exact_samples() {
        let ctx = egui::Context::default();
        let (dir, mut app) = fixture(&ctx);
        app.settings.graph_float = false;
        let path = dir.join("labels.takm");
        app.views[0].select(1, true);
        frame_with(&ctx, &mut app, vec![]);
        app.quiet();
        let before = app.views[0].edit_selection_state();
        let point = marker_on_screen(&app, 0);
        let output = open(&ctx, &mut app, point);
        assert_eq!(app.views[0].edit_selection_state(), before);
        menu_item(&output, "Selected Markers (2)");

        let info = hover(&ctx, &mut app, menu_item(&output, "Marker Info").center());
        menu_item(&info, "2 selected · last picked");
        let p = app.views[0].data.as_ref().unwrap().markers[1]
            .position(5)
            .unwrap();
        menu_item(&info, &format!("X  {:.2} mm", p.x * 1000.0));
        menu_item(&info, &format!("Y  {:.2} mm", p.y * 1000.0));
        menu_item(&info, &format!("Z  {:.2} mm", p.z * 1000.0));
        let undo = steps(&app, 0).0;
        let unlabel = menu_item(&output, "Unlabel Marker(s)").center();
        hover(&ctx, &mut app, unlabel);
        click_at(&ctx, &mut app, unlabel);
        wait(&ctx, &mut app, |app| !app.marker_job_running(0));
        assert_eq!(steps(&app, 0), (undo, 0));
        assert!(app
            .takm_note
            .as_ref()
            .unwrap()
            .0
            .contains("이름 없는 원본 마커"));

        // Keep both named points at frame 5, and create a writable Unlabeled donor at frame 3.
        app.views[0].set_range(3, 3);
        app.assign_label_to_point(&ctx, 0, B, KEY_A);
        wait(&ctx, &mut app, |app| !app.marker_job_running(0));
        let original = save_and_read(&ctx, &mut app, path.clone());
        assert_eq!(
            original
                .tracks()
                .filter(|track| track.label.is_none())
                .count(),
            1
        );
        app.views[0].set_range(3, 7);
        app.views[0].select_many(vec![0, 1], Some(1), false);
        frame_with(&ctx, &mut app, vec![]);
        app.quiet();
        let before = app.views[0].edit_selection_state();
        let undo = steps(&app, 0).0;
        let point = marker_on_screen(&app, 0);
        let output = open(&ctx, &mut app, point);
        click_at(
            &ctx,
            &mut app,
            menu_item(&output, "Unlabel Marker(s)").center(),
        );
        wait(&ctx, &mut app, |app| !app.marker_job_running(0));
        let edited = snapshot(&app);
        assert_eq!(steps(&app, 0), (undo + 1, 0), "{:?}", app.takm_note);
        for uid in [A, B] {
            let track = original.track(&uid).unwrap();
            for sample in track
                .samples
                .iter()
                .filter(|sample| (3..=7).contains(&sample.frame()))
            {
                assert_eq!(
                    self::sample(edited.track(&uid).unwrap(), sample.frame()),
                    None
                );
                assert!(edited
                    .tracks()
                    .any(|t| t.label.is_none() && t.samples.contains(sample)));
            }
        }
        undo_key(&ctx, &mut app);
        wait(&ctx, &mut app, |app| app.views[0].edit_refresh.is_none());
        assert_same_tracks(&snapshot(&app), &original);
        assert_eq!(app.views[0].edit_selection_state(), before);
        redo_key(&ctx, &mut app);
        wait(&ctx, &mut app, |app| app.views[0].edit_refresh.is_none());
        assert_same_tracks(&snapshot(&app), &edited);
        assert_same_tracks(&save_and_read(&ctx, &mut app, path), &edited);
    }

    #[test]
    fn marker_menu_remove_uses_the_right_clicked_view_and_ignores_popup_undo() {
        let ctx = egui::Context::default();
        let (dir, mut app) = fixture(&ctx);
        app.settings.graph_float = false;
        let path = dir.join("other.takm");
        std::fs::copy(dir.join("labels.takm"), &path).unwrap();
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        app.open_in(&ctx, 1, path);
        wait(&ctx, &mut app, |app| {
            app.views[1].data.is_some() && !app.views[1].busy()
        });
        app.activate(0);
        app.views[1].pb.set_frame(5, 0.0);
        frame_with(&ctx, &mut app, vec![]);
        app.quiet();
        let view = &app.views[1];
        let xyz = view.data.as_ref().unwrap().markers[1].position(5).unwrap();
        let point = view.cam.view(view.scene_rect).project(xyz).unwrap().0;
        assert_eq!(markers_at(&app, 1, point), Some(1));
        let output = open(&ctx, &mut app, point);
        menu_item(&output, "Selected Markers (1)");
        assert_eq!(app.views[1].primary, Some(1));
        let history = steps(&app, 1);
        undo_key(&ctx, &mut app);
        assert_eq!(steps(&app, 1), history);
        assert_eq!(app.views[1].primary, Some(1));
        assert!(app.scene_marker_menu.is_open());

        app.active = 0;
        click_at(&ctx, &mut app, menu_item(&output, "Remove Marker").center());
        wait(&ctx, &mut app, |app| !app.marker_job_running(1));
        assert!(app.views[1].marker_dirty(), "{:?}", app.takm_note);
        assert!(!app.views[0].marker_dirty());
        assert!(!app.views[1].data.as_ref().unwrap().markers[1].present(5));
        assert!(app.views[0].data.as_ref().unwrap().markers[1].present(5));
    }
}
