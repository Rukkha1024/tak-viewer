use super::*;
use crate::edit::delete::{delete_samples, DeleteRequest};
use crate::edit::fill::{FillMethod, FillRequest, NumericalFill};
use crate::edit::label::{LabelEditor, LabelRequest, LabelScope, UnlabelRequest, VerifiedCreation};
use crate::edit::save::{read_creation_streams, FileVersion, SaveOperation, SaveRequest, TakmSave};
use crate::edit::{EditDocument, EditPatch, EditSelection, PreparedInput};

fn runs(data: &ViewData, uid: Uid) -> &SharedVec<(i64, i64)> {
    &data
        .markers
        .iter()
        .find(|marker| marker.uid == uid)
        .unwrap()
        .segments
}

fn apply(doc: &mut EditDocument, view: &mut ViewData, patch: &EditPatch, redo: bool) {
    doc.apply(patch, redo).unwrap();
    *view = view.rebuild_markers(&doc.snapshot()).unwrap();
}

#[test]
fn tracks_presence_follows_label_unlabel_fill_delete_undo_and_takm_roundtrip() {
    let temp = crate::test_support::TempDir::new("tracks_presence");
    let source = temp.join("tracks_source.takm");
    let output = temp.join("tracks_output.takm");
    crate::takm::write(
        &mut std::fs::File::create(&source).unwrap(),
        &crate::sample_tak::sample_tak_with_verified_marker_suffix(),
        "tracks.tak",
        "2026-11-18",
    )
    .unwrap();
    let source_version = FileVersion::capture(&source).unwrap();
    let (input, take) = PreparedInput::read_from_path(&source).unwrap();
    let mut doc = EditDocument::unloaded(source.clone());
    doc.install_prepared(doc.revision(), input).unwrap();
    let base = doc.snapshot();
    let mut view = ViewData::build(take);
    let l = base.tracks().find(|t| t.name == "LKNE").unwrap().uid;
    let r_track = base.tracks().find(|t| t.name == "RKNE").unwrap();
    let (r, label) = (r_track.uid, r_track.label.unwrap());
    let raw = base.tracks().find(|t| t.label.is_none()).unwrap().uid;
    let base_raw_runs = runs(&view, raw).clone();
    assert_eq!(runs(&view, l), &vec![(0, 299)]);
    assert_eq!(runs(&view, r), &vec![(0, 99), (140, 299)]);

    let (nodes, channels) = read_creation_streams(&source, &source_version).unwrap();
    let proof = VerifiedCreation::from_source_streams(&base, &nodes, &channels).unwrap();
    let sample = base.track(&l).unwrap().samples[120];
    let unlabel = LabelEditor::with_creation(&proof)
        .unlabel_with_outcome(
            &base,
            &UnlabelRequest {
                tracks: vec![l],
                current_frame: 120,
                scope: LabelScope::SelectedOrAll {
                    range: Some((120, 120)),
                },
                backward: true,
                forward: true,
                selection: EditSelection::default(),
            },
        )
        .unwrap()
        .patch
        .unwrap();
    let new = unlabel
        .changes
        .iter()
        .find(|change| change.before.is_none())
        .unwrap()
        .uid;
    apply(&mut doc, &mut view, &unlabel, true);
    assert_eq!(runs(&view, new), &vec![(120, 120)]);
    assert_eq!(runs(&view, l), &vec![(0, 119), (121, 299)]);
    assert_eq!(&*doc.snapshot().track(&new).unwrap().samples, &[sample]);

    let relabel = LabelEditor::new()
        .label_with_outcome(
            &doc.snapshot(),
            &LabelRequest {
                source: new,
                target: label,
                current_frame: 120,
                scope: LabelScope::default(),
                backward: true,
                forward: true,
                increment: false,
                visible_labels: vec![],
                selection: EditSelection::default(),
            },
        )
        .unwrap()
        .patch
        .unwrap();
    apply(&mut doc, &mut view, &relabel, true);
    assert_eq!(runs(&view, r), &vec![(0, 99), (120, 120), (140, 299)]);
    assert!(view.markers.iter().all(|marker| marker.uid != new));
    assert_eq!(doc.snapshot().track(&r).unwrap().samples[100], sample);
    apply(&mut doc, &mut view, &relabel, false);
    assert_eq!(runs(&view, new), &vec![(120, 120)]);
    apply(&mut doc, &mut view, &unlabel, false);
    assert_eq!(runs(&view, l), &vec![(0, 299)]);

    let selection = EditSelection {
        selected: [r].into(),
        order: vec![r],
        primary: Some(r),
        armed_label: None,
    };
    let fill = NumericalFill
        .fill_detailed(
            &doc.snapshot(),
            &FillRequest {
                method: FillMethod::Cubic,
                targets: vec![r],
                references: vec![],
                range: (0, 299),
                max_gap: 75,
                selection: selection.clone(),
            },
        )
        .unwrap()
        .patch
        .unwrap();
    apply(&mut doc, &mut view, &fill, true);
    assert_eq!(runs(&view, r), &vec![(0, 299)]);
    let delete = delete_samples(
        &doc.snapshot(),
        &DeleteRequest {
            selection,
            range: None,
            current_frame: 121,
        },
    )
    .unwrap()
    .patch
    .unwrap();
    apply(&mut doc, &mut view, &delete, true);
    assert_eq!(runs(&view, r), &vec![(0, 120), (122, 299)]);
    apply(&mut doc, &mut view, &delete, false);
    assert_eq!(runs(&view, r), &vec![(0, 299)]);
    apply(&mut doc, &mut view, &fill, false);
    assert_eq!(runs(&view, r), &vec![(0, 99), (140, 299)]);
    apply(&mut doc, &mut view, &fill, true);
    apply(&mut doc, &mut view, &delete, true);
    assert!(runs(&view, raw).shares_storage(&base_raw_runs));

    let expected = doc.snapshot();
    TakmSave
        .save(SaveRequest {
            snapshot: expected.clone(),
            source: source.clone(),
            destination: output.clone(),
            expected_source: source_version.clone(),
            expected_destination: FileVersion::capture(&output).unwrap(),
        })
        .unwrap();
    let (input, take) = PreparedInput::read_from_path(&output).unwrap();
    let reopened = ViewData::build(take);
    assert_eq!(runs(&reopened, r), &vec![(0, 120), (122, 299)]);
    let mut reopened_doc = EditDocument::unloaded(output);
    reopened_doc
        .install_prepared(reopened_doc.revision(), input)
        .unwrap();
    for track in expected.tracks() {
        assert_eq!(
            reopened_doc.snapshot().track(&track.uid).unwrap().samples,
            track.samples
        );
    }
    assert_eq!(FileVersion::capture(&source).unwrap(), source_version);
}
