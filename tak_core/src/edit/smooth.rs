//! Atomic, run-context Butterworth smoothing for an editable marker document.
use super::{
    AffectedFrames, DocumentSnapshot, EditError, EditPatch, EditResult, EditSelection, RawSample,
    TrackChange,
};
use crate::tak::Uid;
use std::collections::HashSet;
use std::sync::Arc;

pub const DEFAULT_UI_CUTOFF_HZ: f64 = 6.0;
/// The entered Hz is the design cutoff; the default is 6 Hz.
pub fn ui_cutoff_to_design_hz(ui_hz: f64) -> f64 {
    ui_hz
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmoothScope {
    Selection,
    Track,
    AllInRange,
    AllTracks,
}
#[derive(Clone, Debug)]
pub struct SmoothRequest {
    pub targets: Vec<Uid>,
    pub range: (u32, u32),
    pub ui_cutoff_hz: f64,
    pub sample_rate_hz: f64,
    pub selection: EditSelection,
}
impl SmoothRequest {
    /// Resolve the UI scope; `All*` means active-document marker tracks only.
    pub fn for_scope(
        input: &DocumentSnapshot,
        scope: SmoothScope,
        range: Option<(u32, u32)>,
        ui_cutoff_hz: f64,
        sample_rate_hz: f64,
        selection: EditSelection,
    ) -> Result<Self, EditError> {
        let doc = input.input().ok_or(EditError::NotLoaded)?;
        let all = matches!(scope, SmoothScope::AllInRange | SmoothScope::AllTracks);
        if !all
            && selection
                .selected
                .iter()
                .any(|id| input.track(id).is_none())
        {
            return Err(EditError::Other(
                "Selected marker is not in the active document".into(),
            ));
        }
        let targets: Vec<_> = input
            .track_order()
            .iter()
            .copied()
            .filter(|id| all || selection.selected.contains(id))
            .collect();
        if targets.is_empty() {
            return Err(EditError::Other("No marker tracks selected".into()));
        }
        let actual_range = if matches!(scope, SmoothScope::Selection | SmoothScope::AllInRange) {
            range.ok_or_else(|| EditError::Other("Smoothing this scope requires a range".into()))?
        } else {
            (doc.first_frame, doc.last_frame)
        };
        Ok(Self {
            targets,
            range: actual_range,
            ui_cutoff_hz,
            sample_rate_hz,
            selection,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmoothSkip {
    pub uid: Uid,
    pub first: u32,
    pub last: u32,
    pub reason: String,
}
#[derive(Clone, Debug)]
pub struct SmoothOutcome {
    pub patch: Option<EditPatch>,
    pub result: EditResult,
    pub skipped: Vec<SmoothSkip>,
}
/// Use complete observed runs as context and output only requested frames.
pub trait SmoothOperations {
    fn smooth(
        &self,
        input: &DocumentSnapshot,
        request: &SmoothRequest,
    ) -> Result<Option<EditPatch>, EditError>;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct NumericalSmooth;
impl SmoothOperations for NumericalSmooth {
    fn smooth(
        &self,
        input: &DocumentSnapshot,
        request: &SmoothRequest,
    ) -> Result<Option<EditPatch>, EditError> {
        Ok(self.smooth_detailed(input, request)?.patch)
    }
}
fn validate_cutoff(rate: f64, cutoff: f64) -> Result<(), EditError> {
    if !rate.is_finite() || !cutoff.is_finite() || cutoff <= 0.0 || cutoff >= rate / 2.0 {
        Err(EditError::Other(
            "Require finite 0 < cutoff < sampling rate / 2".into(),
        ))
    } else {
        Ok(())
    }
}
/// Double-precision port of `src/trajectory.py::butterworth1`; endpoints stay pinned.
fn butterworth1(values: &[f64], rate: f64, cutoff: f64) -> Result<Vec<f64>, &'static str> {
    if values.len() < 3 {
        return Err("fewer than three contiguous samples");
    }
    if !values.iter().all(|v| v.is_finite()) {
        return Err("non-finite input");
    }
    let k = (std::f64::consts::PI * cutoff / rate).tan();
    let (b0, a) = (k / (1.0 + k), (k - 1.0) / (1.0 + k));
    let (diag, scale) = (1.0 + a * a, b0 * b0);
    let n = values.len();
    let mut rhs: Vec<_> = (1..n - 1)
        .map(|i| scale * (values[i - 1] + 2.0 * values[i] + values[i + 1]))
        .collect();
    rhs[0] -= a * values[0];
    let last = rhs.len() - 1;
    rhs[last] -= a * values[n - 1];
    let mut upper: Vec<f64> = Vec::with_capacity(rhs.len());
    let mut solved: Vec<f64> = Vec::with_capacity(rhs.len());
    for r in rhs {
        let d = diag - a * upper.last().copied().unwrap_or(0.0);
        upper.push(a / d);
        solved.push((r - a * solved.last().copied().unwrap_or(0.0)) / d);
    }
    for i in (0..solved.len().saturating_sub(1)).rev() {
        solved[i] -= upper[i] * solved[i + 1];
    }
    let mut result = Vec::with_capacity(n);
    result.push(values[0]);
    result.extend(solved);
    result.push(values[n - 1]);
    if !result.iter().all(|v| v.is_finite()) {
        return Err("non-finite filtering result");
    }
    Ok(result)
}
#[cfg(test)]
pub(super) fn crosscheck_butterworth(
    values: &[f64],
    rate: f64,
    cutoff: f64,
) -> Result<Vec<f64>, &'static str> {
    butterworth1(values, rate, cutoff)
}
impl NumericalSmooth {
    /// Build one patch for all changed samples and retain skipped runs for status reporting.
    pub fn smooth_detailed(
        &self,
        input: &DocumentSnapshot,
        request: &SmoothRequest,
    ) -> Result<SmoothOutcome, EditError> {
        let doc = input.input().ok_or(EditError::NotLoaded)?;
        let (lo, hi) = request.range;
        if lo > hi || lo < doc.first_frame || hi > doc.last_frame {
            return Err(EditError::InvalidRange);
        }
        let cutoff = ui_cutoff_to_design_hz(request.ui_cutoff_hz);
        validate_cutoff(request.sample_rate_hz, cutoff)?;
        if request.targets.is_empty() {
            return Err(EditError::Other("Select at least one marker".into()));
        }
        let mut seen = HashSet::new();
        for uid in &request.targets {
            if !seen.insert(*uid) {
                return Err(EditError::DuplicateUid);
            }
            if input.track(uid).is_none() {
                return Err(EditError::Other(
                    "Target is not in the active document".into(),
                ));
            }
        }
        let mut result = EditResult::default();
        let mut skipped = Vec::new();
        let mut changes = Vec::new();
        let mut affected: Option<(u32, u32)> = None;
        for uid in &request.targets {
            let track = input.track(uid).expect("validated target");
            let mut samples = track.samples.to_vec();
            let mut begin = 0;
            let mut track_changed = false;
            while begin < track.samples.len() {
                let mut end = begin + 1;
                while end < track.samples.len()
                    && track.samples[end - 1].frame().checked_add(1)
                        == Some(track.samples[end].frame())
                {
                    end += 1;
                }
                let first = track.samples[begin].frame();
                let last = track.samples[end - 1].frame();
                if first <= hi && last >= lo {
                    let reason = if end - begin < 3 {
                        Some("fewer than three contiguous samples")
                    } else if !track.records.serialization_verified {
                        Some("marker layout is not verified for editing")
                    } else {
                        None
                    };
                    let filtered: Result<Vec<Vec<f64>>, &'static str> = match reason {
                        Some(r) => Err(r),
                        None => (0..3)
                            .map(|axis| {
                                let values: Vec<_> = track.samples[begin..end]
                                    .iter()
                                    .map(|s| f64::from(s.xyz()[axis]))
                                    .collect();
                                butterworth1(&values, request.sample_rate_hz, cutoff)
                            })
                            .collect(),
                    };
                    match filtered {
                        Ok(curves) => {
                            // Assemble the entire run first: a non-finite float32 conversion rejects it whole.
                            let mut pending = Vec::new();
                            let mut overflow = false;
                            for (i, sample) in track.samples[begin..end].iter().enumerate() {
                                let frame = sample.frame();
                                if frame < lo || frame > hi {
                                    continue;
                                }
                                // Pinned endpoints stay byte-identical, including signed zero.
                                if i == 0 || i + 1 == end - begin {
                                    continue;
                                }
                                let xyz = [
                                    curves[0][i] as f32,
                                    curves[1][i] as f32,
                                    curves[2][i] as f32,
                                ];
                                if !xyz.iter().all(|v| v.is_finite()) {
                                    overflow = true;
                                    break;
                                }
                                let new = RawSample::new(frame, xyz);
                                if new != *sample {
                                    pending.push((begin + i, new));
                                }
                            }
                            if overflow {
                                skipped.push(SmoothSkip {
                                    uid: *uid,
                                    first,
                                    last,
                                    reason: "result exceeds float32 range".into(),
                                });
                            } else {
                                for (i, sample) in pending {
                                    let frame = sample.frame();
                                    samples[i] = sample;
                                    result.samples += 1;
                                    track_changed = true;
                                    affected = Some(match affected {
                                        Some((a, b)) => (a.min(frame), b.max(frame)),
                                        None => (frame, frame),
                                    });
                                }
                            }
                        }
                        Err(reason) => skipped.push(SmoothSkip {
                            uid: *uid,
                            first,
                            last,
                            reason: reason.into(),
                        }),
                    }
                }
                begin = end;
            }
            if track_changed {
                let mut after = (**track).clone();
                after.samples = samples.into();
                changes.push(TrackChange {
                    uid: *uid,
                    before: Some(track.clone()),
                    after: Some(Arc::new(after)),
                });
                result.tracks += 1;
            }
        }
        result.skipped = skipped.len();
        result.message = format!(
            "Smooth · {} tracks · {} samples · {} skipped",
            result.tracks, result.samples, result.skipped
        );
        let patch = match affected {
            Some((first, last)) => Some(EditPatch::prepare(
                input,
                changes,
                request.selection.clone(),
                request.selection.clone(),
                AffectedFrames { first, last },
                result.clone(),
            )?),
            None => None,
        };
        Ok(SmoothOutcome {
            patch,
            result,
            skipped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{DocumentInput, EditDocument, RawTrack, RecordBytes};
    use super::*;
    use std::path::PathBuf;
    fn uid(n: u8) -> Uid {
        [n; 16]
    }
    fn track(n: u8, points: &[(u32, [f32; 3])]) -> RawTrack {
        RawTrack {
            uid: uid(n),
            name: format!("m{n}"),
            label: None,
            records: RecordBytes {
                node: Arc::from([]),
                translation_header: Arc::from([]),
                serialization_verified: true,
            },
            samples: points
                .iter()
                .map(|(f, p)| RawSample::new(*f, *p))
                .collect::<Vec<_>>()
                .into(),
        }
    }
    fn document(tracks: Vec<RawTrack>, last: u32) -> EditDocument {
        let input = DocumentInput {
            streams: Arc::from([]),
            nodes_prefix: Arc::from([]),
            channels_prefix: Arc::from([]),
            tracks,
            labels: vec![],
            first_frame: 0,
            last_frame: last,
        };
        let mut d = EditDocument::unloaded(PathBuf::from("fixture.tak"));
        d.install_input(d.revision(), input).unwrap();
        d
    }
    fn request(targets: Vec<Uid>, range: (u32, u32), cutoff: f64, rate: f64) -> SmoothRequest {
        SmoothRequest {
            targets,
            range,
            ui_cutoff_hz: cutoff,
            sample_rate_hz: rate,
            selection: EditSelection::default(),
        }
    }
    #[test]
    fn matches_python_at_literal_six_and_five_and_pins_endpoints() {
        let x = [0.0, 10.0, 0.0, 10.0, 0.0];
        let y6 = butterworth1(&x, 100.0, ui_cutoff_to_design_hz(6.0)).unwrap();
        let y5 = butterworth1(&x, 100.0, ui_cutoff_to_design_hz(5.0)).unwrap();
        assert!((y6[2] - 1.1934285710498673).abs() < 1e-12);
        assert!((y5[2] - 0.8716772568966966).abs() < 1e-12);
        assert_eq!((y6[0], y6[4], y5[0], y5[4]), (0.0, 0.0, 0.0, 0.0));
    }
    #[test]
    fn selection_uses_full_run_context_and_leaves_short_runs_and_other_samples_exact() {
        let pts = [
            (0, [0.0, 0.0, 0.0]),
            (1, [10.0, 1.0, 0.0]),
            (2, [0.0, 2.0, 0.0]),
            (3, [10.0, 3.0, 0.0]),
            (4, [0.0, 4.0, 0.0]),
            (7, [1.0, 0.0, 0.0]),
            (8, [2.0, 0.0, 0.0]),
        ];
        let mut doc = document(vec![track(1, &pts)], 8);
        let before = doc.snapshot();
        let out = NumericalSmooth
            .smooth_detailed(&before, &request(vec![uid(1)], (2, 8), 6.0, 100.0))
            .unwrap();
        assert_eq!(
            (out.result.tracks, out.result.samples, out.result.skipped),
            (1, 2, 1)
        );
        let patch = out.patch.unwrap();
        doc.apply(&patch, true).unwrap();
        let after = doc.snapshot();
        let values = &after.track(&uid(1)).unwrap().samples;
        assert_eq!(values[0], before.track(&uid(1)).unwrap().samples[0]);
        assert_eq!(values[1], before.track(&uid(1)).unwrap().samples[1]);
        assert_eq!(values[4..], before.track(&uid(1)).unwrap().samples[4..]);
        assert!((f64::from(values[2].xyz()[0]) - 1.1934285710498673).abs() < 1e-6);
        assert_eq!(
            values.iter().map(|s| s.frame()).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 7, 8]
        );
        doc.apply(&patch, false).unwrap();
        assert_eq!(
            doc.snapshot().track(&uid(1)).unwrap().samples,
            before.track(&uid(1)).unwrap().samples
        );
        doc.apply(&patch, true).unwrap();
        assert_eq!(
            doc.snapshot().track(&uid(1)).unwrap().samples,
            after.track(&uid(1)).unwrap().samples
        );
    }
    #[test]
    fn rejects_invalid_rate_cutoff_range_and_target() {
        let doc = document(
            vec![track(1, &[(0, [0.0; 3]), (1, [1.0; 3]), (2, [0.0; 3])])],
            2,
        );
        let snap = doc.snapshot();
        for (cutoff, rate) in [
            (0.0, 100.0),
            (-1.0, 100.0),
            (50.0, 100.0),
            (51.0, 100.0),
            (f64::NAN, 100.0),
            (5.0, f64::INFINITY),
            (5.0, -100.0),
        ] {
            assert!(NumericalSmooth
                .smooth(&snap, &request(vec![uid(1)], (0, 2), cutoff, rate))
                .is_err());
        }
        assert!(NumericalSmooth
            .smooth(&snap, &request(vec![uid(1)], (0, 3), 6.0, 100.0))
            .is_err());
        assert!(NumericalSmooth
            .smooth(&snap, &request(vec![uid(2)], (0, 2), 6.0, 100.0))
            .is_err());
        assert!(NumericalSmooth
            .smooth(&snap, &request(vec![uid(1), uid(1)], (0, 2), 6.0, 100.0))
            .is_err());
    }
    #[test]
    fn no_op_or_bad_run_never_creates_patch() {
        let constant = track(1, &[(0, [0.0; 3]), (1, [0.0; 3]), (2, [0.0; 3])]);
        let invalid = track(
            2,
            &[(0, [0.0; 3]), (1, [f32::NAN, 0.0, 0.0]), (2, [1.0; 3])],
        );
        let mut unsafe_track = track(3, &[(0, [0.0; 3]), (1, [2.0; 3]), (2, [1.0; 3])]);
        unsafe_track.records.serialization_verified = false;
        let doc = document(vec![constant, invalid, unsafe_track], 2);
        let snap = doc.snapshot();
        let out = NumericalSmooth
            .smooth_detailed(
                &snap,
                &request(vec![uid(1), uid(2), uid(3)], (0, 2), 6.0, 100.0),
            )
            .unwrap();
        assert!(out.patch.is_none());
        assert_eq!((out.result.samples, out.result.skipped), (0, 2));
        assert!(out.skipped.iter().any(|s| s.reason.contains("non-finite")));
    }
    #[test]
    fn scope_resolves_selected_tracks_all_tracks_and_ranges() {
        let pts = [(0, [0.0; 3]), (1, [10.0; 3]), (2, [0.0; 3])];
        let doc = document(vec![track(1, &pts), track(2, &pts)], 2);
        let snap = doc.snapshot();
        let mut sel = EditSelection::default();
        sel.selected.insert(uid(1));
        let selected = SmoothRequest::for_scope(
            &snap,
            SmoothScope::Selection,
            Some((1, 1)),
            6.0,
            100.0,
            sel.clone(),
        )
        .unwrap();
        assert_eq!(selected.targets, vec![uid(1)]);
        assert_eq!(selected.range, (1, 1));
        let track = SmoothRequest::for_scope(
            &snap,
            SmoothScope::Track,
            Some((1, 1)),
            6.0,
            100.0,
            sel.clone(),
        )
        .unwrap();
        assert_eq!(track.range, (0, 2));
        let all = SmoothRequest::for_scope(
            &snap,
            SmoothScope::AllInRange,
            Some((1, 1)),
            6.0,
            100.0,
            sel.clone(),
        )
        .unwrap();
        assert_eq!(all.targets, vec![uid(1), uid(2)]);
        assert_eq!(all.range, (1, 1));
        let all =
            SmoothRequest::for_scope(&snap, SmoothScope::AllTracks, None, 6.0, 100.0, sel.clone())
                .unwrap();
        assert_eq!(all.range, (0, 2));
        assert!(
            SmoothRequest::for_scope(&snap, SmoothScope::AllInRange, None, 6.0, 100.0, sel)
                .is_err()
        );
    }
}
