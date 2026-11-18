fn merge_request(source: Uid, destination: Uid, range: Option<(u32, u32)>) -> MergeRequest {
    MergeRequest {
        source,
        destination,
        range,
        selection: EditSelection {
            selected: [source, destination].into(),
            order: vec![destination, source],
            primary: Some(source),
            armed_label: Some(key(1)),
        },
    }
}

#[test]
fn merge_keys_keeps_either_destination_identity_and_round_trips_raw_samples() {
    for (source_label, destination_label) in [
        (None, Some(key(2))),
        (None, None),
        (Some(key(1)), Some(key(2))),
        (Some(key(1)), None),
    ] {
        // Duplicate display names still refer to different rows by UID.
        let source = marker(1, "Same name", source_label, &[1, 3, 5]);
        let destination = marker(2, "Same name", destination_label, &[0, 2, 4]);
        let (mut doc, _, _) =
            build_fixture(vec![source.clone(), destination.clone()], vec![], vec![]);
        let base = doc.snapshot();
        let request = merge_request(source.uid, destination.uid, None);
        let patch = LabelEditor::new().merge_keys(&base, &request).unwrap();
        let expected = [
            destination.samples[0],
            source.samples[0],
            destination.samples[1],
            source.samples[1],
            destination.samples[2],
            source.samples[2],
        ];
        for redo in [true, false, true] {
            doc.apply(&patch, redo).unwrap();
            let edited = doc.snapshot();
            let donor = edited.track(&source.uid).unwrap();
            let receiver = edited.track(&destination.uid).unwrap();
            assert_eq!(receiver.records, destination.records);
            assert_eq!(receiver.label, destination.label);
            assert_eq!(receiver.name, destination.name);
            if redo {
                assert_eq!(receiver.samples.as_ref(), &expected);
                assert!(donor.samples.is_empty());
            } else {
                assert_eq!(receiver.as_ref(), &destination);
                assert_eq!(donor.as_ref(), &source);
            }
        }
        assert_eq!(patch.selection_before, request.selection);
        assert_eq!(patch.selection_after.selected, [destination.uid].into());
        assert_eq!(patch.selection_after.primary, Some(destination.uid));
        assert_eq!(
            patch.selection_after.armed_label,
            request.selection.armed_label
        );
    }
}

#[test]
fn merge_keys_scopes_overlap_refusal_and_move_to_inclusive_range() {
    let mut source = marker(1, "A", None, &[1, 2, 3, 5, 7, 11]);
    Arc::make_mut(&mut source.samples)[2] = RawSample::new(3, [-0.0, 2.5, 0.125]);
    let destination = marker(2, "B", Some(key(2)), &[2, 4, 7, 11]);
    let (doc, _, _) = build_fixture(vec![source.clone(), destination.clone()], vec![], vec![]);
    let base = doc.snapshot();
    let editor = LabelEditor::new();
    let request = |range| merge_request(source.uid, destination.uid, range);
    let refusal = editor
        .merge_keys(&base, &request(Some((3, 11))))
        .unwrap_err();
    let EditError::Other(reason) = refusal else {
        panic!("{refusal:?}")
    };
    assert!(reason.contains("겹치는 프레임이 2개"));
    assert!(reason.contains("처음 겹치는 프레임은 7"));
    assert_eq!(doc.snapshot().revision(), base.revision());
    assert!(!doc.is_dirty());

    let patch = editor.merge_keys(&base, &request(Some((3, 5)))).unwrap();
    let after = patch.after_snapshot();
    assert_eq!(patch.affected, AffectedFrames { first: 3, last: 5 });
    assert_eq!(
        after.track(&source.uid).unwrap().samples.as_ref(),
        &[
            source.samples[0],
            source.samples[1],
            source.samples[4],
            source.samples[5],
        ]
    );
    assert_eq!(
        after.track(&destination.uid).unwrap().samples.as_ref(),
        &[
            destination.samples[0],
            source.samples[2],
            destination.samples[1],
            source.samples[3],
            destination.samples[2],
            destination.samples[3],
        ]
    );
    assert_eq!(
        editor
            .merge_keys(&base, &request(Some((5, 3))))
            .unwrap_err(),
        EditError::InvalidRange
    );
    assert_eq!(
        editor
            .merge_keys(&base, &request(Some((8, 10))))
            .unwrap_err(),
        other("보내는 줄에 옮길 점이 없어요.")
    );
    let mut one = request(None);
    one.selection.selected.remove(&destination.uid);
    assert!(editor.merge_keys(&base, &one).is_err());
    let missing = merge_request(source.uid, uid(9), None);
    assert_eq!(
        editor.merge_keys(&base, &missing).unwrap_err(),
        other("선택한 마커가 없어요.")
    );
}

#[test]
fn merge_keys_removes_session_unlabeled_and_saves_exact_takm_samples() {
    use crate::edit::save::{FileVersion, SaveOperation, SaveRequest, TakmSave};
    use crate::view_data::ViewData;

    let temp = crate::test_support::TempDir::new("merge_keys_save");
    let source = temp.join("source.takm");
    let output = temp.join("merged.takm");
    crate::takm::write(
        &mut std::fs::File::create(&source).unwrap(),
        &crate::sample_tak::sample_tak_with_verified_marker_suffix(),
        "merge_source.tak",
        "2026-11-18",
    )
    .unwrap();
    let original = std::fs::read(&source).unwrap();
    let (input, take) = PreparedInput::read_from_path(&source).unwrap();
    let mut doc = EditDocument::unloaded(source.clone());
    doc.install_prepared(doc.revision(), input).unwrap();
    let base = doc.snapshot();
    let view = ViewData::build(take);
    let left = base.tracks().find(|track| track.name == "LKNE").unwrap();
    let right = base.tracks().find(|track| track.name == "RKNE").unwrap();
    let creation = CreationStreams::read_from_path(&source).unwrap();
    let proof = creation.verify(&base).unwrap();
    let split = LabelEditor::with_creation(&proof)
        .unlabel_with_outcome(
            &base,
            &UnlabelRequest {
                tracks: vec![left.uid],
                current_frame: 110,
                scope: LabelScope::SelectedOrAll {
                    range: Some((110, 120)),
                },
                backward: true,
                forward: true,
                selection: EditSelection::default(),
            },
        )
        .unwrap()
        .patch
        .unwrap();
    doc.apply(&split, true).unwrap();
    let piece = split
        .changes
        .iter()
        .find(|change| change.before.is_none())
        .unwrap()
        .uid;
    let split_view = view.rebuild_markers(&doc.snapshot()).unwrap();
    let merge = LabelEditor::new()
        .merge_keys(&doc.snapshot(), &merge_request(piece, right.uid, None))
        .unwrap();
    doc.apply(&merge, true).unwrap();
    let merged = doc.snapshot();
    assert!(merged.track(&piece).is_none());
    assert_eq!(
        merged.track(&right.uid).unwrap().samples[100..111],
        left.samples[110..121]
    );
    let updated = split_view.rebuild_markers(&merged).unwrap();
    let receiver = updated.markers.iter().find(|m| m.uid == right.uid).unwrap();
    assert_eq!(
        receiver.segments.as_slice(),
        &[(0, 99), (110, 120), (140, 299)]
    );
    assert!(updated.markers.iter().all(|m| m.uid != piece));
    doc.apply(&merge, false).unwrap();
    assert_eq!(
        doc.snapshot().track(&piece).unwrap().samples.as_ref(),
        &left.samples[110..121]
    );
    doc.apply(&merge, true).unwrap();

    TakmSave
        .save(SaveRequest {
            snapshot: doc.snapshot(),
            source: source.clone(),
            destination: output.clone(),
            expected_source: FileVersion::capture(&source).unwrap(),
            expected_destination: FileVersion::capture(&output).unwrap(),
        })
        .unwrap();
    let (input, _) = PreparedInput::read_from_path(&output).unwrap();
    let mut reopened = EditDocument::unloaded(output);
    reopened
        .install_prepared(reopened.revision(), input)
        .unwrap();
    for expected in merged.tracks() {
        let actual = reopened.snapshot().track(&expected.uid).unwrap().clone();
        assert_eq!(actual.samples, expected.samples);
        assert_eq!(actual.label, expected.label);
    }
    assert!(reopened.snapshot().track(&piece).is_none());
    assert_eq!(std::fs::read(&source).unwrap(), original);
}

#[test]
fn assignment_and_merge_keys_share_the_same_overlap_frames() {
    let existing = marker(1, "A", Some(key(1)), &[1, 3, 4, 7]);
    let moved = marker(2, "B", Some(key(2)), &[2, 3, 5, 7]);
    let (assigned, displaced) = merge_assignment_samples(&existing.samples, &moved.samples);
    let refusal = merged_samples(&existing.samples, &moved.samples).err().unwrap();

    assert_eq!(displaced, [existing.samples[1], existing.samples[3]]);
    assert_eq!((refusal.first, refusal.count), (displaced[0].frame(), displaced.len()));
    assert_eq!(assigned, [
        existing.samples[0], moved.samples[0], moved.samples[1],
        existing.samples[2], moved.samples[2], moved.samples[3],
    ]);
}
