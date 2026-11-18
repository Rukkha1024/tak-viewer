use super::{
    merged_samples, other, select_samples, source_change, strict_track, validate_scope,
    AffectedFrames, DocumentSnapshot, EditError, EditPatch, EditResult, EditSelection, LabelEditor,
    LabelScope, TrackChange, Uid,
};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct MergeRequest {
    pub source: Uid,
    pub destination: Uid,
    pub range: Option<(u32, u32)>,
    pub selection: EditSelection,
}

impl LabelEditor<'_> {
    /// Move exact sample records between existing identities. Run on the edit worker.
    pub fn merge_keys(
        &self,
        input: &DocumentSnapshot,
        request: &MergeRequest,
    ) -> Result<EditPatch, EditError> {
        let first = input.input().ok_or(EditError::NotLoaded)?.first_frame;
        let scope = LabelScope::SelectedOrAll {
            range: request.range,
        };
        validate_scope(input, first, &scope, true, true)?;
        if request.source == request.destination
            || request.selection.selected.len() != 2
            || !request.selection.selected.contains(&request.source)
            || !request.selection.selected.contains(&request.destination)
        {
            return Err(other("Select two marker rows"));
        }
        let track = |uid| {
            let track = input
                .track(&uid)
                .ok_or_else(|| other("Selected marker missing"))?;
            strict_track(track)?;
            Ok::<_, EditError>(track)
        };
        let source = track(request.source)?;
        let destination = track(request.destination)?;
        let (moved, retained) = select_samples(source, input, first, &scope, true, true)?;
        let (Some(first), Some(last)) = (moved.first(), moved.last()) else {
            return Err(other("No points to move"));
        };
        let affected = AffectedFrames {
            first: first.frame(),
            last: last.frame(),
        };
        let mut destination_after = (**destination).clone();
        destination_after.samples = merged_samples(&destination.samples, &moved)
            .map_err(|overlap| EditError::Other(format!(
                "Can't merge: {} frames overlap (first {})",
                overlap.count, overlap.first,
            )))?.into();
        let changes = vec![
            source_change(input, source.clone(), retained),
            TrackChange {
                uid: destination.uid,
                before: Some(destination.clone()),
                after: Some(Arc::new(destination_after)),
            },
        ];
        let selection_after = EditSelection {
            selected: [destination.uid].into(),
            order: vec![destination.uid],
            primary: Some(destination.uid),
            armed_label: request.selection.armed_label,
        };
        let result = EditResult {
            tracks: 2,
            samples: moved.len(),
            message: format!("Moved {} points to {}", moved.len(), destination.name),
            ..EditResult::default()
        };
        EditPatch::prepare(
            input,
            changes,
            request.selection.clone(),
            selection_after,
            affected,
            result,
        )
    }
}
