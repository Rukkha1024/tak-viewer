use super::*;
use crate::edit::save::{FileVersion, SaveOperation, SaveRequest, TakmSave};
use crate::edit::{EditDocument, PreparedInput};
use crate::test_support::TempDir;

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

fn load(path: &Path) -> EditDocument {
    let (prepared, _) = PreparedInput::read_from_path(path).unwrap();
    let mut document = EditDocument::unloaded(path.to_path_buf());
    document
        .install_prepared(document.revision(), prepared)
        .unwrap();
    document
}

fn fixture() -> (TempDir, EditDocument, CreationStreams) {
    let dir = TempDir::new("tak_label_assignment");
    let source = dir.join("source.takm");
    let mut file = std::fs::File::create(&source).unwrap();
    crate::takm::write(
        &mut file,
        &crate::sample_tak::sample_tak_for_label_assignments(),
        "synthetic.tak",
        "",
    )
    .unwrap();
    drop(file);
    (
        dir,
        load(&source),
        CreationStreams::read_from_path(&source).unwrap(),
    )
}

fn assignment(source: Uid, target: LabelKey) -> LabelRequest {
    LabelRequest {
        source,
        target,
        current_frame: 5,
        scope: LabelScope::SelectedOrAll {
            range: Some((3, 7)),
        },
        backward: true,
        forward: true,
        increment: true,
        visible_labels: vec![KEY_A, KEY_B],
        selection: EditSelection {
            selected: [A, B].into_iter().collect(),
            order: vec![A, B],
            primary: Some(B),
            armed_label: Some(target),
        },
    }
}

fn sample(doc: &DocumentSnapshot, uid: Uid, frame: u32) -> Option<RawSample> {
    doc.track(&uid)?
        .samples
        .iter()
        .copied()
        .find(|s| s.frame() == frame)
}

fn displacements(patch: &EditPatch) -> &RawTrack {
    patch
        .changes
        .iter()
        .find(|change| change.before.is_none())
        .and_then(|change| change.after.as_deref())
        .unwrap()
}

fn assert_state(actual: &DocumentSnapshot, expected: &DocumentSnapshot) {
    assert_eq!(actual.track_order(), expected.track_order());
    for uid in expected.track_order() {
        assert_eq!(actual.track(uid), expected.track(uid));
    }
}

fn save_and_reopen(dir: &Path, document: &EditDocument, name: &str) {
    let source = document.source_path();
    let before = std::fs::read(source).unwrap();
    let destination = dir.join(name);
    let snapshot = document.snapshot();
    TakmSave
        .save(SaveRequest {
            snapshot: snapshot.clone(),
            source: source.to_path_buf(),
            expected_source: FileVersion::capture(source).unwrap(),
            expected_destination: FileVersion::capture(&destination).unwrap(),
            destination: destination.clone(),
        })
        .unwrap();
    crate::takm::verify_fixed(&destination).unwrap();
    let reopened = load(&destination).snapshot();
    assert_eq!(reopened.track_order(), snapshot.track_order());
    for track in snapshot.tracks() {
        let actual = reopened.track(&track.uid).unwrap();
        assert_eq!(actual.samples, track.samples);
        assert_eq!((&actual.name, actual.label), (&track.name, track.label));
        assert_eq!(actual.records.node, track.records.node);
        let header = |track: &RawTrack| track.records.translation_header.len() - 4;
        assert_eq!(
            actual.records.translation_header[..header(actual)],
            track.records.translation_header[..header(track)]
        );
    }
    assert_eq!(std::fs::read(source).unwrap(), before);
}

#[test]
fn two_click_swap_preserves_raw_bytes_and_round_trips_each_state() {
    let (dir, mut document, streams) = fixture();
    let original = document.snapshot();
    let editor = LabelEditor::with_creation_streams(&streams);
    let request = assignment(B, KEY_A);
    let first = editor.label(&original, &request).unwrap().unwrap();
    let displaced = displacements(&first);
    assert_eq!(displaced.label, None);
    assert_eq!(displaced.name, "Unlabeled 1");
    assert_eq!(
        displaced.samples.as_ref(),
        &[3, 5, 7].map(|f| sample(&original, A, f).unwrap())
    );
    assert!(first
        .result
        .message
        .contains("4프레임 이름 변경 · 3프레임 Unlabeled"));
    assert_eq!(first.selection_before, request.selection);
    assert_eq!(first.selection_after.order, vec![A]);
    assert_eq!(first.selection_after.primary, Some(A));
    assert_eq!(first.selection_after.armed_label, Some(KEY_B));

    document.apply(&first, true).unwrap();
    let interim = document.snapshot();
    for frame in [3, 5, 6, 7] {
        assert_eq!(sample(&interim, A, frame), sample(&original, B, frame));
        assert!(sample(&interim, B, frame).is_none());
    }
    assert_eq!(sample(&interim, A, 4), sample(&original, A, 4));
    save_and_reopen(&dir, &document, "after-first.takm");

    let mut request = assignment(displaced.uid, KEY_B);
    request.selection.selected = [displaced.uid].into_iter().collect();
    request.selection.primary = Some(displaced.uid);
    request.selection.order = vec![displaced.uid];
    let second = editor.label(&interim, &request).unwrap().unwrap();
    document.apply(&second, true).unwrap();
    let repaired = document.snapshot();
    assert!(repaired.track(&displaced.uid).is_none());
    assert_eq!(second.selection_after.primary, Some(B));
    assert_eq!(second.selection_after.armed_label, Some(KEY_B));
    assert!(second
        .result
        .message
        .contains("3프레임 이름 변경 · 0프레임 Unlabeled"));
    for frame in [3, 5, 7] {
        assert_eq!(sample(&repaired, B, frame), sample(&original, A, frame));
    }
    for uid in [A, B] {
        for frame in [0, 2, 8, 12] {
            assert_eq!(sample(&repaired, uid, frame), sample(&original, uid, frame));
        }
    }
    save_and_reopen(&dir, &document, "after-second.takm");

    document.apply(&second, false).unwrap();
    assert_state(&document.snapshot(), &interim);
    document.apply(&first, false).unwrap();
    assert_state(&document.snapshot(), &original);
    document.apply(&first, true).unwrap();
    assert_state(&document.snapshot(), &interim);
    document.apply(&second, true).unwrap();
    assert_state(&document.snapshot(), &repaired);
}

#[test]
fn collision_respects_direction_and_keeps_target_on_unsampled_frames() {
    let (_dir, document, streams) = fixture();
    let before = document.snapshot();
    let editor = LabelEditor::with_creation_streams(&streams);
    for (backward, forward, moved, displaced) in [
        (true, false, vec![3, 5], vec![3, 5]),
        (false, true, vec![5, 6, 7], vec![5, 7]),
    ] {
        let mut request = assignment(B, KEY_A);
        request.backward = backward;
        request.forward = forward;
        let patch = editor.label(&before, &request).unwrap().unwrap();
        assert_eq!(patch.result.samples, moved.len());
        assert_eq!(
            displacements(&patch)
                .samples
                .iter()
                .map(|s| s.frame())
                .collect::<Vec<_>>(),
            displaced
        );
        for frame in 0..=12 {
            let expected = if moved.contains(&frame) {
                sample(&before, B, frame)
            } else {
                sample(&before, A, frame)
            };
            assert_eq!(sample(&patch.after_snapshot(), A, frame), expected);
        }
    }
    let mut request = assignment(B, KEY_A);
    request.scope = LabelScope::default();
    let patch = editor.label(&before, &request).unwrap().unwrap();
    assert_eq!(patch.affected, AffectedFrames { first: 0, last: 12 });
    assert!(patch.after_snapshot().track(&B).unwrap().samples.is_empty());
}

#[test]
fn same_label_and_empty_scope_do_not_create_history_and_invalid_edits_are_atomic() {
    let (_dir, document, streams) = fixture();
    let before = document.snapshot();
    let editor = LabelEditor::with_creation_streams(&streams);
    let same = editor
        .label_with_outcome(&before, &assignment(B, KEY_B))
        .unwrap();
    assert!(same.already_assigned && same.patch.is_none());
    assert_eq!(same.increment, IncrementResult::NotApplied);
    let mut request = assignment(B, KEY_A);
    request.scope = LabelScope::SelectedOrAll {
        range: Some((4, 4)),
    };
    let empty = editor.label_with_outcome(&before, &request).unwrap();
    assert!(!empty.already_assigned && empty.patch.is_none());
    request.scope = LabelScope::default();
    assert!(LabelEditor::new().label(&before, &request).is_err());
    request.current_frame = 4;
    assert!(editor.label(&before, &request).is_err());
    request.current_frame = 5;
    request.backward = false;
    request.forward = false;
    assert!(editor.label(&before, &request).is_err());
    assert_eq!(document.revision(), before.revision());
    assert!(!document.is_dirty());
}
