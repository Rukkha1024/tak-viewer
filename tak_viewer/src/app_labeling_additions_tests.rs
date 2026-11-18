use super::integration_tests::{settled, step, wait_for};
use super::*;
use tak_core::tak;
use tak_core::test_support::TempDir;
use tak_core::view_data::ViewData;

#[test]
fn labeling_settings_read_absent_fields_and_legacy_increment_bools() {
    let old: Settings = ron::from_str(r#"(workspace_roots: ["C:/takes"], speed: 0.75)"#).unwrap();
    assert_eq!(old.workspace_roots, ["C:/takes"]);
    assert_eq!(old.speed, 0.75);
    assert!(!old.labeling_link);
    assert_eq!(old.labeling_increment, IncrementMode::Next);
    for (value, mode) in [("true", IncrementMode::Next), ("false", IncrementMode::Stay)] {
        let old: Settings = ron::from_str(&format!("(labeling_increment: {value})")).unwrap();
        assert_eq!(old.labeling_increment, mode);
    }
    for mode in IncrementMode::ALL {
        let saved = Settings { labeling_link: true, labeling_increment: mode, ..old.clone() };
        let loaded: Settings = ron::from_str(&ron::to_string(&saved).unwrap()).unwrap();
        assert_eq!((loaded.labeling_link, loaded.labeling_increment), (true, mode));
        assert_eq!(loaded.workspace_roots, ["C:/takes"]);
    }
}

fn labeling_view(ctx: &egui::Context) -> TakViewerApp {
    let mut take = views::sample_take("labeling_additions.takm", &["Full", "Half", "Empty", "Other", "Unlabeled"]);
    for (index, marker) in take.trajectories[..4].iter_mut().enumerate() {
        let parent = [if index == 3 { 8 } else { 7 }; 16];
        marker.kind = MarkerKind::OtherParent;
        marker.parent_uid = Some(parent);
        marker.slot = Some(index as i32 + 1);
    }
    take.trajectories[1].frames.truncate(5);
    take.trajectories[1].xyz.truncate(5);
    take.trajectories[2].frames.clear();
    take.trajectories[2].xyz.clear();
    for (name, uid) in [("First", [7; 16]), ("Second", [8; 16])] {
        take.label_groups.push(tak::LabelGroup { uid, name: name.into(), labels: vec![], member_rgb: vec![] });
    }
    let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
    let (_, rx) = mpsc::channel();
    app.views[0].show(ViewData::build(take), rx, 10, 1.0, 0.0);
    step(ctx, &mut app, 0.0);
    app.refresh_marker_ui(0);
    app.observe(false);
    app
}

#[test]
fn labeling_increment_and_coverage_use_current_frame_and_visible_order() {
    let ctx = egui::Context::default();
    let mut app = labeling_view(&ctx);
    let state = &mut app.marker_ui[0].labeling;
    let [full, half, empty, other] = visible_labels(state).try_into().unwrap();
    let rows: Vec<_> = state.groups.iter().flat_map(|group| &group.labels)
        .map(|row| (row.name.as_str(), row.fill_pct)).collect();
    assert_eq!(rows, [("Full", 100.0), ("Half", 50.0), ("Empty", 0.0), ("Other", 100.0)]);

    state.increment = IncrementMode::Stay;
    assert!(increment_order(state, full, None).is_empty());
    state.increment = IncrementMode::Next;
    assert_eq!(increment_order(state, full, None), [full, half, empty, other]);
    state.increment = IncrementMode::NextMissing;
    assert_eq!(increment_order(state, full, None), [full, empty]);
    // After a named point moves away, its row becomes eligible if no other track holds the label.
    assert_eq!(increment_order(state, full, Some([2; 16])), [full, half, empty]);

    state.filter = "empty".into();
    for mode in [IncrementMode::Next, IncrementMode::NextMissing] {
        state.increment = mode;
        assert_eq!(increment_order(state, half, None), [half, empty]);
        assert_eq!(increment_order(state, empty, None), [empty]);
    }
    state.increment = IncrementMode::Stay;
    assert!(increment_order(state, half, None).is_empty());
    assert!(increment_order(state, empty, None).is_empty());
    state.filter.clear();
    state.increment = IncrementMode::NextMissing;
    assert_eq!(increment_order(state, other, None), [empty, other], "the last label never wraps");

    app.views[0].pb.set_frame(5, 0.0);
    app.settings.labeling_increment = IncrementMode::NextMissing;
    app.refresh_marker_ui(0);
    assert_eq!(increment_order(&app.marker_ui[0].labeling, full, None), [full, half, empty]);
}

fn pick_in_scene(app: &mut TakViewerApp, index: usize) {
    let view = &mut app.views[0];
    view.select(index, false);
    view.link_labeling_to_scene(app.settings.labeling_link);
    app.refresh_marker_ui(0);
    app.observe(true);
}

#[test]
fn labeling_link_follows_3d_pick_and_undo_restores_the_fold_and_filter() {
    let ctx = egui::Context::default();
    let mut app = labeling_view(&ctx);
    let group = app.views[0].data.as_ref().unwrap().markers[3].group;
    app.labeling_action(&ctx, 0, LabelingAction::SetAssetFilter(Some([7; 16])));
    app.views[0].labels.filter = "full".into();
    app.views[0].labels.collapsed.insert(group);
    app.observe(false);
    pick_in_scene(&mut app, 3);
    assert!(app.views[0].labels.collapsed.contains(&group));
    assert_eq!(app.views[0].labels.linked_pick.asset, None);
    assert_eq!(visible_labels(&app.marker_ui[0].labeling).len(), 1);

    app.labeling_action(&ctx, 0, LabelingAction::SetLinkTo3d(true));
    let history_before = app.history.undo.len();
    pick_in_scene(&mut app, 3);
    assert_eq!(app.history.undo.len(), history_before + 1);
    assert!(!app.views[0].labels.collapsed.contains(&group));
    assert_eq!(app.views[0].labels.linked_pick.asset, Some([8; 16]));
    assert_eq!(app.views[0].labels.scroll_to, Some(3));
    assert_eq!(visible_labels(&app.marker_ui[0].labeling), [LabelKey { parent: [8; 16], slot: 4 }]);

    app.undo_redo(&ctx, false);
    assert!(app.views[0].labels.collapsed.contains(&group));
    assert_eq!(app.views[0].labels.linked_pick.asset, None);
    app.undo_redo(&ctx, true);
    assert!(!app.views[0].labels.collapsed.contains(&group));
    assert_eq!(app.views[0].labels.linked_pick.asset, Some([8; 16]));

    app.labeling_action(&ctx, 0, LabelingAction::SetLinkTo3d(false));
    app.refresh_marker_ui(0);
    assert_eq!(visible_labels(&app.marker_ui[0].labeling), [LabelKey { parent: [7; 16], slot: 1 }]);
    app.undo_redo(&ctx, false);
    assert!(app.settings.labeling_link);
    app.undo_redo(&ctx, true);
    assert!(!app.settings.labeling_link);
    app.labeling_action(&ctx, 0, LabelingAction::SetIncrement(IncrementMode::NextMissing));
    app.undo_redo(&ctx, false);
    assert_eq!(app.settings.labeling_increment, IncrementMode::Next);
    app.undo_redo(&ctx, true);
    assert_eq!(app.settings.labeling_increment, IncrementMode::NextMissing);
}

#[test]
fn labeling_context_refuses_empty_multiple_and_absent_selection() {
    let ctx = egui::Context::default();
    let mut app = labeling_view(&ctx);
    let target = LabelKey { parent: [7; 16], slot: 3 };
    for selected in [vec![], vec![0, 1], vec![2]] {
        app.views[0].select_many(selected.clone(), selected.last().copied(), false);
        app.refresh_marker_ui(0);
        let reason = app.marker_ui[0].labeling.label_marker_reason.unwrap();
        let before = app.history.undo.len();
        app.labeling_action(&ctx, 0, LabelingAction::LabelMarker(target));
        assert_eq!(app.history.undo.len(), before);
        assert!(settled(&app));
        assert_eq!(app.takm_note.as_ref().unwrap().0, reason);
    }
    app.views[0].select(0, false);
    assert_eq!(label_marker_source(&app.views[0]), Ok([1; 16]));
}

fn snapshot_from_disk(path: &Path) -> DocumentSnapshot {
    let (input, _) = PreparedInput::read_from_path(path).unwrap();
    let mut document = tak_core::edit::EditDocument::unloaded(path.to_owned());
    document.install_prepared(document.revision(), input).unwrap();
    document.snapshot()
}

#[test]
fn labeling_context_assigns_exact_samples_in_one_undo_and_saves_takm() {
    let scratch = TempDir::new("tak_labeling_context");
    let path = scratch.join("context.takm");
    let fixture = tak_core::sample_tak::sample_tak_with_unlabeled_frames(100..110);
    tak_core::takm::write(&mut std::fs::File::create(&path).unwrap(), &fixture, "fixture.tak", "test").unwrap();
    let original = snapshot_from_disk(&path);
    let source = [3; 16];
    let destination = [2; 16];
    let moved_frame = 102;
    let target = LabelKey { parent: [4; 16], slot: 2 };
    let original_source = &original.track(&source).unwrap().samples;
    let original_destination = &original.track(&destination).unwrap().samples;
    let moved = original_source.iter().find(|sample| sample.frame() == moved_frame).unwrap();
    let expected_source: Vec<_> = original_source.iter().copied().filter(|sample| sample.frame() != moved_frame).collect();
    let mut expected_destination = original_destination.to_vec();
    expected_destination.insert(expected_destination.partition_point(|sample| sample.frame() < moved_frame), *moved);

    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.open_in(&ctx, 0, path.clone());
    wait_for(&ctx, &mut app, |app| app.views[0].data.is_some() && !app.views[0].analog_work_pending());
    let index = app.views[0].data.as_ref().unwrap().markers.iter().position(|marker| marker.uid == source).unwrap();
    app.views[0].select(index, false);
    app.views[0].pb.set_frame(i64::from(moved_frame), 0.0);
    app.views[0].set_range(i64::from(moved_frame), i64::from(moved_frame));
    assert_eq!(label_marker_source(&app.views[0]), Ok(source));
    let session = app.views[0].edit_document.clone().unwrap();
    let expected_before_selection = app.views[0].edit_selection_state();
    app.settings.labeling_increment = IncrementMode::NextMissing;
    app.observe(false);
    let count = app.history.undo.len();

    app.labeling_action(&ctx, 0, LabelingAction::LabelMarker(target));
    wait_for(&ctx, &mut app, settled);
    let assigned = session.lock().unwrap().snapshot();
    let check_assigned = |snapshot: &DocumentSnapshot| {
        let actual = snapshot.track(&destination).unwrap_or_else(|| panic!("assignment has no destination"));
        assert_eq!(actual.samples.as_ref(), expected_destination.as_slice());
        assert_eq!(snapshot.track(&source).unwrap().samples.as_ref(), expected_source.as_slice());
    };
    assert!(assigned.is_loaded(), "{:?}", app.takm_note);
    check_assigned(&assigned);
    assert_eq!(app.views[0].armed_label, Some(target));
    assert!(app.takm_note.as_ref().is_some_and(|(note, _)| note.contains("No next label")));
    assert_eq!(app.history.undo.len(), count + 1);

    app.undo_redo(&ctx, false);
    wait_for(&ctx, &mut app, settled);
    let undone = session.lock().unwrap().snapshot();
    for track in [destination, source] {
        assert_eq!(undone.track(&track).unwrap().samples, original.track(&track).unwrap().samples);
    }
    assert_eq!(app.views[0].edit_selection_state(), expected_before_selection);
    app.undo_redo(&ctx, true);
    wait_for(&ctx, &mut app, settled);
    check_assigned(&session.lock().unwrap().snapshot());
    assert_eq!(app.views[0].armed_label, Some(target));

    app.save_markers(&ctx, 0, false, false);
    wait_for(&ctx, &mut app, |app| !app.marker_saving(0));
    assert!(!app.views[0].marker_dirty(), "{:?}", app.takm_note);
    let reopened = snapshot_from_disk(&path);
    check_assigned(&reopened);
}
