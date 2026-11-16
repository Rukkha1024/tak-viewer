//! Delete selected trajectory samples without changing their raw bytes outside the scope.
use super::{AffectedFrames, DocumentSnapshot, EditError, EditPatch, EditResult, EditSelection, TrackChange};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct DeleteRequest {
    pub selection: EditSelection,
    pub range: Option<(u32, u32)>,
    pub current_frame: u32,
}

pub struct DeleteOutcome {
    pub patch: Option<EditPatch>,
    pub result: EditResult,
}

/// Prepare on a worker thread. Missing and empty tracks are counted, not edited.
pub fn delete_samples(snapshot: &DocumentSnapshot, request: &DeleteRequest) -> Result<DeleteOutcome, EditError> {
    let input = snapshot.input().ok_or(EditError::NotLoaded)?;
    let (first, last) = request.range.unwrap_or((request.current_frame, request.current_frame));
    if first > last || first < input.first_frame || last > input.last_frame {
        return Err(EditError::InvalidRange);
    }
    let mut changes = Vec::new();
    let mut samples = 0;
    let mut skipped = 0;
    for uid in &request.selection.selected {
        let Some(before) = snapshot.track(uid) else { skipped += 1; continue };
        let start = before.samples.partition_point(|sample| sample.frame() < first);
        let end = before.samples.partition_point(|sample| sample.frame() <= last);
        if start == end { skipped += 1; continue; }
        samples += end - start;
        let mut after = (**before).clone();
        after.samples = before.samples[..start].iter().chain(&before.samples[end..]).copied().collect::<Vec<_>>().into();
        changes.push(TrackChange { uid: *uid, before: Some(before.clone()), after: Some(Arc::new(after)) });
    }
    let tracks = changes.len();
    let mut message = format!("Delete · {tracks} tracks · {samples} samples");
    if request.range.is_none() { message.push_str(&format!(" · frame {}", request.current_frame)); }
    if skipped > 0 { message.push_str(&format!(" · {skipped} skipped")); }
    if tracks == 0 {
        message.push_str(if request.selection.selected.is_empty() { " · Select markers" } else { " · No samples in scope" });
    }
    let result = EditResult { tracks, samples, skipped, message, ..Default::default() };
    let patch = if changes.is_empty() { None } else {
        Some(EditPatch::prepare(snapshot, changes, request.selection.clone(), request.selection.clone(), AffectedFrames { first, last }, result.clone())?)
    };
    Ok(DeleteOutcome { patch, result })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{DocumentInput, EditDocument, LabelKey, RawSample, RawTrack, RecordBytes};
    use std::collections::BTreeSet;

    fn uid(n: u8) -> [u8; 16] { [n; 16] }
    fn track(n: u8, frames: &[u32], labeled: bool) -> RawTrack {
        RawTrack {
            uid: uid(n), name: format!("track {n}"),
            label: labeled.then_some(LabelKey { parent: uid(9), slot: 1 }),
            records: RecordBytes { node: Arc::from([n]), translation_header: Arc::from([n]), serialization_verified: true },
            samples: frames.iter().map(|&frame| RawSample::new(frame, [f32::from_bits(0x7fc0_1234), -0.0, frame as f32])).collect::<Vec<_>>().into(),
        }
    }
    fn document() -> EditDocument {
        let mut doc = EditDocument::unloaded("fixture.tak".into());
        let input = DocumentInput { streams: Arc::from([]), nodes_prefix: Arc::from([]), channels_prefix: Arc::from([]),
            tracks: vec![track(1, &[1, 2, 3, 4, 5], true), track(2, &[2, 3, 6], false), track(3, &[8], false)],
            labels: vec![], first_frame: 0, last_frame: 10 };
        doc.install_input(doc.revision(), input).unwrap();
        doc
    }
    fn request(ids: &[u8], range: Option<(u32, u32)>, current_frame: u32) -> DeleteRequest {
        DeleteRequest { selection: EditSelection { selected: ids.iter().map(|&n| uid(n)).collect::<BTreeSet<_>>(), ..Default::default() }, range, current_frame }
    }
    fn frames(doc: &EditDocument, n: u8) -> Vec<u32> {
        doc.snapshot().track(&uid(n)).unwrap().samples.iter().map(|s| s.frame()).collect()
    }

    #[test]
    fn inclusive_range_multi_track_skip_undo_redo_exact_bytes() {
        let mut doc = document();
        let before = doc.snapshot();
        let outcome = delete_samples(&before, &request(&[1, 2, 3], Some((2, 3)), 1)).unwrap();
        assert_eq!((outcome.result.tracks, outcome.result.samples, outcome.result.skipped), (2, 4, 1));
        assert_eq!(outcome.result.message, "Delete · 2 tracks · 4 samples · 1 skipped");
        let patch = outcome.patch.unwrap();
        doc.apply(&patch, true).unwrap();
        assert_eq!(frames(&doc, 1), [1, 4, 5]);
        assert_eq!(frames(&doc, 2), [6]);
        assert_eq!(frames(&doc, 3), [8]);
        doc.apply(&patch, false).unwrap();
        for n in 1..=3 { assert_eq!(doc.snapshot().track(&uid(n)), before.track(&uid(n))); }
        doc.apply(&patch, true).unwrap();
        assert_eq!(frames(&doc, 1), [1, 4, 5]);
    }

    #[test]
    fn no_range_only_current_frame_and_empty_no_patch() {
        let mut doc = document();
        let outcome = delete_samples(&doc.snapshot(), &request(&[1, 2], None, 2)).unwrap();
        assert_eq!(outcome.result.message, "Delete · 2 tracks · 2 samples · frame 2");
        doc.apply(&outcome.patch.unwrap(), true).unwrap();
        assert_eq!(frames(&doc, 1), [1, 3, 4, 5]);
        let no_op = delete_samples(&doc.snapshot(), &request(&[1, 2], None, 2)).unwrap();
        assert!(no_op.patch.is_none());
        assert_eq!((no_op.result.samples, no_op.result.skipped), (0, 2));
        assert!(no_op.result.message.contains("No samples in scope"));
        assert!(delete_samples(&doc.snapshot(), &request(&[], None, 2)).unwrap().result.message.contains("Select markers"));
        assert!(matches!(delete_samples(&doc.snapshot(), &request(&[1], Some((4, 3)), 2)), Err(EditError::InvalidRange)));
    }
}
