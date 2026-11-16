//! Atomic gap filling for one editable document. Run on a worker thread.
use super::{
    AffectedFrames, DocumentSnapshot, EditError, EditPatch, EditResult, EditSelection, RawSample,
    RawTrack, TrackChange,
};
use crate::tak::Uid;
use std::collections::HashSet;
use std::sync::Arc;

pub const DEFAULT_MAX_GAP_FRAMES: u32 = 75;
const CUBIC_HARD_LIMIT: u32 = 75;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillMethod {
    Cubic,
    PatternBased,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillScope {
    Selected,
    All,
    Everything,
}
#[derive(Clone, Debug)]
pub struct FillRequest {
    pub method: FillMethod,
    pub targets: Vec<Uid>,
    pub references: Vec<Uid>,
    pub range: (u32, u32),
    pub max_gap: u32,
    pub selection: EditSelection,
}
impl FillRequest {
    /// Resolve the UI scope against this document, retaining its original marker order.
    pub fn for_scope(
        input: &DocumentSnapshot,
        method: FillMethod,
        scope: FillScope,
        range: Option<(u32, u32)>,
        max_gap: u32,
        selection: EditSelection,
    ) -> Result<Self, EditError> {
        let doc = input.input().ok_or(EditError::NotLoaded)?;
        if method == FillMethod::PatternBased && scope == FillScope::Everything {
            return Err(EditError::Other(
                "Pattern Fill Everything is disabled: choose a target and references".into(),
            ));
        }
        let actual_range = if scope == FillScope::Selected {
            range.ok_or_else(|| EditError::Other("Fill Selected requires a range".into()))?
        } else {
            (doc.first_frame, doc.last_frame)
        };
        if scope != FillScope::Everything
            && selection
                .selected
                .iter()
                .any(|id| input.track(id).is_none())
        {
            return Err(EditError::Other(
                "Selected marker is not in the active document".into(),
            ));
        }
        let chosen: Vec<_> = input
            .track_order()
            .iter()
            .copied()
            .filter(|id| scope == FillScope::Everything || selection.selected.contains(id))
            .collect();
        if chosen.is_empty() {
            return Err(EditError::Other("Select at least one marker".into()));
        }
        let (targets, references) = if method == FillMethod::PatternBased {
            if chosen.len() < 2 {
                return Err(EditError::Other(
                    "Pattern Based: select reference markers, then the target last".into(),
                ));
            }
            let target = selection
                .primary
                .filter(|id| chosen.contains(id))
                .ok_or_else(|| {
                    EditError::Other("Pattern fill requires a last-selected marker".into())
                })?;
            (
                vec![target],
                chosen.into_iter().filter(|id| *id != target).collect(),
            )
        } else {
            (chosen, Vec::new())
        };
        Ok(Self {
            method,
            targets,
            references,
            range: actual_range,
            max_gap,
            selection,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FillSkip {
    pub uid: Uid,
    pub first: u32,
    pub last: u32,
    pub reason: String,
}
#[derive(Clone, Debug)]
pub struct FillOutcome {
    pub patch: Option<EditPatch>,
    pub result: EditResult,
    pub skipped: Vec<FillSkip>,
}

/// Compatibility contract. Use `fill_detailed` to retain skip reasons even when no patch exists.
pub trait FillOperations {
    fn fill(
        &self,
        input: &DocumentSnapshot,
        request: &FillRequest,
    ) -> Result<Option<EditPatch>, EditError>;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct NumericalFill;
impl FillOperations for NumericalFill {
    fn fill(
        &self,
        input: &DocumentSnapshot,
        request: &FillRequest,
    ) -> Result<Option<EditPatch>, EditError> {
        Ok(self.fill_detailed(input, request)?.patch)
    }
}

fn point(sample: RawSample) -> Result<[f64; 3], &'static str> {
    let p = sample.xyz().map(f64::from);
    if p.iter().all(|v| v.is_finite()) {
        Ok(p)
    } else {
        Err("non-finite input")
    }
}
fn sample_at(track: &RawTrack, frame: u32) -> Option<RawSample> {
    track
        .samples
        .binary_search_by_key(&frame, |s| s.frame())
        .ok()
        .map(|i| track.samples[i])
}
fn packed(frame: u32, p: [f64; 3]) -> Result<RawSample, &'static str> {
    if !p.iter().all(|v| v.is_finite()) {
        return Err("non-finite result");
    }
    let out = p.map(|v| v as f32);
    if !out.iter().all(|v| v.is_finite()) {
        return Err("result exceeds float32 range");
    }
    Ok(RawSample::new(frame, out))
}
fn validate_request<'a>(
    input: &'a DocumentSnapshot,
    request: &FillRequest,
) -> Result<Vec<&'a Arc<RawTrack>>, EditError> {
    let doc = input.input().ok_or(EditError::NotLoaded)?;
    if request.range.0 > request.range.1
        || request.range.0 < doc.first_frame
        || request.range.1 > doc.last_frame
    {
        return Err(EditError::InvalidRange);
    }
    if request.max_gap == 0 {
        return Err(EditError::Other("Max. Gap Size must be positive".into()));
    }
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
    if request.method == FillMethod::PatternBased {
        if request.targets.len() != 1 {
            return Err(EditError::Other("Pattern fill requires one target".into()));
        }
        if request.references.is_empty() {
            return Err(EditError::Other(
                "Pattern Based: select reference markers, then the target last".into(),
            ));
        }
        for uid in &request.references {
            if !seen.insert(*uid) {
                return Err(EditError::DuplicateUid);
            }
        }
        request
            .references
            .iter()
            .map(|id| {
                input.track(id).ok_or_else(|| {
                    EditError::Other("Reference is not in the active document".into())
                })
            })
            .collect()
    } else {
        Ok(Vec::new())
    }
}

impl NumericalFill {
    /// Build at most one atomic patch. Every skipped intersecting gap has an explicit reason.
    pub fn fill_detailed(
        &self,
        input: &DocumentSnapshot,
        request: &FillRequest,
    ) -> Result<FillOutcome, EditError> {
        let refs = validate_request(input, request)?;
        let doc = input.input().ok_or(EditError::NotLoaded)?;
        let (lo, hi) = request.range;
        let mut skipped = Vec::new();
        let mut changes = Vec::new();
        let mut result = EditResult::default();
        let mut affected: Option<(u32, u32)> = None;
        for uid in &request.targets {
            let track = input.track(uid).expect("validated target");
            let mut additions = Vec::new();
            // u64 cursor permits a take ending at u32::MAX without wrapping.
            let mut cursor = u64::from(doc.first_frame);
            for next in track
                .samples
                .iter()
                .map(|s| u64::from(s.frame()))
                .chain(std::iter::once(u64::from(doc.last_frame) + 1))
            {
                if next > cursor {
                    let a = cursor as u32;
                    let b = (next - 1) as u32;
                    if b >= lo && a <= hi {
                        let size = u64::from(b) - u64::from(a) + 1;
                        let rejected = if a < lo || b > hi {
                            Some("gap not wholly inside selection")
                        } else if size > u64::from(request.max_gap) {
                            Some("gap exceeds Max. Gap Size")
                        } else if request.method == FillMethod::Cubic
                            && size > u64::from(CUBIC_HARD_LIMIT)
                        {
                            Some("cubic gap exceeds 75 frames")
                        } else if a == doc.first_frame || b == doc.last_frame {
                            Some("boundary gap has no two-sided anchors")
                        } else if !track.records.serialization_verified {
                            Some("marker layout is not verified for editing")
                        } else {
                            None
                        };
                        let pending = match rejected {
                            Some(reason) => Err(reason),
                            None => match request.method {
                                FillMethod::Cubic => cubic_gap(track, a, b),
                                FillMethod::PatternBased => pattern_gap(track, &refs, a, b),
                            },
                        };
                        match pending {
                            Ok(samples) => {
                                result.gaps += 1;
                                result.samples += samples.len();
                                affected = Some(match affected {
                                    Some((first, last)) => (first.min(a), last.max(b)),
                                    None => (a, b),
                                });
                                additions.extend(samples);
                            }
                            Err(reason) => skipped.push(FillSkip {
                                uid: *uid,
                                first: a,
                                last: b,
                                reason: reason.into(),
                            }),
                        }
                    }
                }
                cursor = next + 1;
            }
            if !additions.is_empty() {
                let mut samples = track.samples.to_vec();
                samples.extend(additions);
                samples.sort_unstable_by_key(|s| s.frame());
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
        let method_name = match request.method {
            FillMethod::Cubic => "Cubic",
            FillMethod::PatternBased => "Pattern Based",
        };
        result.message = format!(
            "Fill · {} · {} gaps · {} frames · {} skipped",
            method_name, result.gaps, result.samples, result.skipped
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
        Ok(FillOutcome {
            patch,
            result,
            skipped,
        })
    }
}

/// Natural spline with Python's exact anchor choice and tridiagonal solve.
fn spline(xs: &[f64], ys: &[f64], query: f64) -> f64 {
    let n = xs.len();
    let h: Vec<_> = xs.windows(2).map(|w| w[1] - w[0]).collect();
    let mut second = vec![0.0; n];
    let (mut diag, mut rhs, mut upper): (Vec<f64>, Vec<f64>, Vec<f64>) = (vec![], vec![], vec![]);
    for i in 1..n - 1 {
        let mut d = 2.0 * (h[i - 1] + h[i]);
        let mut r = 6.0 * ((ys[i + 1] - ys[i]) / h[i] - (ys[i] - ys[i - 1]) / h[i - 1]);
        if !diag.is_empty() {
            let scale = h[i - 1] / diag[diag.len() - 1];
            d -= scale * upper[upper.len() - 1];
            r -= scale * rhs[rhs.len() - 1];
        }
        diag.push(d);
        rhs.push(r);
        upper.push(h[i]);
    }
    for j in (0..diag.len()).rev() {
        second[j + 1] = (rhs[j] - upper[j] * second[j + 2]) / diag[j];
    }
    let i = (0..n - 1)
        .find(|&i| xs[i] <= query && query <= xs[i + 1])
        .expect("query lies between anchors");
    let width = h[i];
    let a = (xs[i + 1] - query) / width;
    let b = (query - xs[i]) / width;
    a * ys[i]
        + b * ys[i + 1]
        + ((a.powi(3) - a) * second[i] + (b.powi(3) - b) * second[i + 1]) * width.powi(2) / 6.0
}
fn cubic_point(track: &RawTrack, a: u32, b: u32, frame: u32) -> Result<[f64; 3], &'static str> {
    let anchors: Vec<_> = [
        a.checked_sub(2),
        a.checked_sub(1),
        b.checked_add(1),
        b.checked_add(2),
    ]
    .into_iter()
    .flatten()
    .filter_map(|f| sample_at(track, f).map(|s| (f, s)))
    .collect();
    if anchors.len() < 2 {
        return Err("missing spline anchors");
    }
    let xs: Vec<_> = anchors.iter().map(|(f, _)| f64::from(*f)).collect();
    let points: Vec<_> = anchors
        .iter()
        .map(|(_, s)| point(*s))
        .collect::<Result<_, _>>()?;
    Ok(std::array::from_fn(|axis| {
        spline(
            &xs,
            &points.iter().map(|p| p[axis]).collect::<Vec<_>>(),
            f64::from(frame),
        )
    }))
}
fn cubic_gap(track: &RawTrack, a: u32, b: u32) -> Result<Vec<RawSample>, &'static str> {
    (a..=b)
        .map(|f| packed(f, cubic_point(track, a, b, f)?))
        .collect()
}

fn pattern_point(
    track: &RawTrack,
    refs: &[&Arc<RawTrack>],
    a: u32,
    b: u32,
    frame: u32,
) -> Result<[f64; 3], &'static str> {
    let left = a - 1;
    let right = b + 1;
    let visible: Vec<_> = refs
        .iter()
        .filter_map(|r| {
            Some((
                sample_at(r, left)?,
                sample_at(r, frame)?,
                sample_at(r, right)?,
            ))
        })
        .collect();
    let need = refs.len().min(3);
    if visible.len() < need {
        return Err("reference gap: insufficient common references");
    }
    let ltarget = point(sample_at(track, left).ok_or("missing left target anchor")?)?;
    let rtarget = point(sample_at(track, right).ok_or("missing right target anchor")?)?;
    let lpoints: Vec<_> = visible
        .iter()
        .map(|v| point(v.0))
        .collect::<Result<_, _>>()?;
    let cpoints: Vec<_> = visible
        .iter()
        .map(|v| point(v.1))
        .collect::<Result<_, _>>()?;
    let rpoints: Vec<_> = visible
        .iter()
        .map(|v| point(v.2))
        .collect::<Result<_, _>>()?;
    let weight = f64::from(frame - left) / f64::from(right - left);
    if refs.len() == 2 {
        return twist_predict(&lpoints, &cpoints, &rpoints, ltarget, rtarget, weight);
    }
    let (l, r) = if refs.len() == 1 {
        (
            std::array::from_fn(|i| ltarget[i] + cpoints[0][i] - lpoints[0][i]),
            std::array::from_fn(|i| rtarget[i] + cpoints[0][i] - rpoints[0][i]),
        )
    } else {
        (
            rigid_predict(&lpoints, &cpoints, ltarget)?,
            rigid_predict(&rpoints, &cpoints, rtarget)?,
        )
    };
    Ok(std::array::from_fn(|i| l[i] + weight * (r[i] - l[i])))
}
fn pattern_gap(
    track: &RawTrack,
    refs: &[&Arc<RawTrack>],
    a: u32,
    b: u32,
) -> Result<Vec<RawSample>, &'static str> {
    (a..=b)
        .map(|f| packed(f, pattern_point(track, refs, a, b, f)?))
        .collect()
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn unit(v: [f64; 3], reason: &'static str) -> Result<[f64; 3], &'static str> {
    let size = dot(v, v).sqrt();
    if !size.is_finite() || size <= 1e-12 {
        return Err(reason);
    }
    Ok(v.map(|x| x / size))
}
fn pair_center(pair: &[[f64; 3]]) -> [f64; 3] {
    std::array::from_fn(|i| (pair[0][i] + pair[1][i]) / 2.0)
}
fn pair_axis(pair: &[[f64; 3]]) -> Result<[f64; 3], &'static str> {
    unit(
        std::array::from_fn(|i| pair[1][i] - pair[0][i]),
        "coincident references: two-reference line has no direction",
    )
}
/// Column-vector, right-hand axis-angle rotation; matches candidates.py::axis_rotation.
fn axis_rotated(axis: [f64; 3], angle: f64, p: [f64; 3]) -> Result<[f64; 3], &'static str> {
    let [x, y, z] = unit(axis, "zero or nonfinite direction")?;
    let skew = [[0.0, -z, y], [z, 0.0, -x], [-y, x, 0.0]];
    let (sin, cos) = angle.sin_cos();
    let rotation: [[f64; 3]; 3] = std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            let square: f64 = (0..3).map(|k| skew[i][k] * skew[k][j]).sum();
            let identity = if i == j { 1.0 } else { 0.0 };
            identity + sin * skew[i][j] + (1.0 - cos) * square
        })
    });
    Ok(rotation.map(|row| dot(row, p)))
}
fn minimal_pair_predict(
    reference: &[[f64; 3]],
    observed: &[[f64; 3]],
    target: [f64; 3],
) -> Result<[f64; 3], &'static str> {
    let (a, b) = (pair_axis(reference)?, pair_axis(observed)?);
    let normal = cross(a, b);
    let sin_angle = dot(normal, normal).sqrt();
    let cos_angle = dot(a, b).clamp(-1.0, 1.0);
    let ca = pair_center(reference);
    let offset = std::array::from_fn(|i| target[i] - ca[i]);
    let rotated = if sin_angle <= 1e-12 {
        if cos_angle < 0.0 {
            return Err("antiparallel references: shortest arc has no unique axis");
        }
        offset
    } else {
        axis_rotated(
            normal.map(|x| x / sin_angle),
            sin_angle.atan2(cos_angle),
            offset,
        )?
    };
    let cb = pair_center(observed);
    Ok(std::array::from_fn(|i| cb[i] + rotated[i]))
}
/// Exact two-reference policy from methods/candidates.py::predict_gap(twist_blend).
/// Both endpoint offsets use direct shortest arcs, then axial position, radius and
/// the shortest signed radial angle are blended. Ambiguous geometry skips the gap.
fn twist_predict(
    left: &[[f64; 3]],
    current: &[[f64; 3]],
    right: &[[f64; 3]],
    ltarget: [f64; 3],
    rtarget: [f64; 3],
    weight: f64,
) -> Result<[f64; 3], &'static str> {
    let lpred = minimal_pair_predict(left, current, ltarget)?;
    let rpred = minimal_pair_predict(right, current, rtarget)?;
    let center = pair_center(current);
    let axis = pair_axis(current)?;
    let loff = std::array::from_fn(|i| lpred[i] - center[i]);
    let roff = std::array::from_fn(|i| rpred[i] - center[i]);
    let (lz, rz) = (dot(loff, axis), dot(roff, axis));
    let lr = std::array::from_fn(|i| loff[i] - lz * axis[i]);
    let rr = std::array::from_fn(|i| roff[i] - rz * axis[i]);
    let (lsize, rsize) = (dot(lr, lr).sqrt(), dot(rr, rr).sqrt());
    if lsize.min(rsize) <= 1e-12 {
        return Err("twist angle undefined for on-axis target");
    }
    let theta = dot(axis, cross(lr, rr)).atan2(dot(lr, rr));
    if (theta.abs() - std::f64::consts::PI).abs() <= 1e-12 {
        return Err("180 degree endpoint twist has ambiguous branch");
    }
    let radial = axis_rotated(axis, weight * theta, lr.map(|x| x / lsize))?;
    let axial = (1.0 - weight) * lz + weight * rz;
    let radius = (1.0 - weight) * lsize + weight * rsize;
    Ok(std::array::from_fn(|i| {
        center[i] + axial * axis[i] + radius * radial[i]
    }))
}

/// Horn proper rigid transform; same degeneracy rule and Jacobi sweep as `trajectory.py`.
fn rigid_predict(
    reference: &[[f64; 3]],
    observed: &[[f64; 3]],
    target: [f64; 3],
) -> Result<[f64; 3], &'static str> {
    if reference.len() != observed.len() || reference.len() < 3 {
        return Err("rigid fit needs at least three references");
    }
    let center = |p: &[[f64; 3]]| -> ([f64; 3], Vec<[f64; 3]>) {
        let mean = std::array::from_fn(|i| p.iter().map(|v| v[i]).sum::<f64>() / p.len() as f64);
        (
            mean,
            p.iter()
                .map(|v| std::array::from_fn(|i| v[i] - mean[i]))
                .collect(),
        )
    };
    let (ca, a) = center(reference);
    let (cb, b) = center(observed);
    for cloud in [&a, &b] {
        let scale = cloud
            .iter()
            .map(|p| p.iter().map(|v| v * v).sum::<f64>())
            .fold(0.0_f64, f64::max);
        let mut best = 0.0_f64;
        for p in cloud.iter() {
            for q in cloud.iter() {
                let cross = [
                    p[1] * q[2] - p[2] * q[1],
                    p[2] * q[0] - p[0] * q[2],
                    p[0] * q[1] - p[1] * q[0],
                ];
                best = best.max(cross.iter().map(|v| v * v).sum());
            }
        }
        if scale == 0.0 || best <= scale * scale * 1e-12 || !scale.is_finite() || !best.is_finite()
        {
            return Err("rigid reference geometry is collinear or degenerate");
        }
    }
    let s: [[f64; 3]; 3] = std::array::from_fn(|i| {
        std::array::from_fn(|j| a.iter().zip(&b).map(|(p, q)| p[i] * q[j]).sum())
    });
    let [[xx, xy, xz], [yx, yy, yz], [zx, zy, zz]] = s;
    let mut m = [
        [xx + yy + zz, yz - zy, zx - xz, xy - yx],
        [yz - zy, xx - yy - zz, xy + yx, zx + xz],
        [zx - xz, xy + yx, -xx + yy - zz, yz + zy],
        [xy - yx, zx + xz, yz + zy, -xx - yy + zz],
    ];
    let mut eig: [[f64; 4]; 4] =
        std::array::from_fn(|i| std::array::from_fn(|j| if i == j { 1.0 } else { 0.0 }));
    for _ in 0..80 {
        let (mut p, mut q) = (0, 1);
        for i in 0..4 {
            for j in i + 1..4 {
                if m[i][j].abs() > m[p][q].abs() {
                    (p, q) = (i, j);
                }
            }
        }
        let scale = (0..4).map(|i| m[i][i].abs()).fold(1e-300_f64, f64::max);
        if m[p][q].abs() <= 1e-14 * scale {
            break;
        }
        let angle = 0.5 * (2.0 * m[p][q]).atan2(m[q][q] - m[p][p]);
        let (si, co) = angle.sin_cos();
        let (pp, qq, pq) = (m[p][p], m[q][q], m[p][q]);
        for i in 0..4 {
            if i != p && i != q {
                let (ip, iq) = (m[i][p], m[i][q]);
                m[i][p] = co * ip - si * iq;
                m[p][i] = m[i][p];
                m[i][q] = si * ip + co * iq;
                m[q][i] = m[i][q];
            }
            let (vp, vq) = (eig[i][p], eig[i][q]);
            eig[i][p] = co * vp - si * vq;
            eig[i][q] = si * vp + co * vq;
        }
        m[p][p] = co * co * pp - 2.0 * co * si * pq + si * si * qq;
        m[q][q] = si * si * pp + 2.0 * co * si * pq + co * co * qq;
        m[p][q] = 0.0;
        m[q][p] = 0.0;
    }
    let mut index = 0;
    for i in 1..4 {
        if m[i][i] > m[index][index] {
            index = i;
        }
    }
    let [w, x, y, z] = std::array::from_fn(|i| eig[i][index]);
    let rot = [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ];
    let out = std::array::from_fn(|i| {
        cb[i] + (0..3).map(|j| rot[i][j] * (target[j] - ca[j])).sum::<f64>()
    });
    if out.iter().all(|v| v.is_finite()) {
        Ok(out)
    } else {
        Err("non-finite rigid result")
    }
}

#[cfg(test)]
mod tests {
    use super::super::{DocumentInput, EditDocument, RecordBytes};
    use super::*;
    use std::path::PathBuf;
    fn uid(n: u8) -> Uid {
        [n; 16]
    }
    fn track(
        n: u8,
        frames: impl IntoIterator<Item = u32>,
        f: impl Fn(u32) -> [f32; 3],
    ) -> RawTrack {
        RawTrack {
            uid: uid(n),
            name: format!("marker{n}"),
            label: None,
            records: RecordBytes {
                node: Arc::from([]),
                translation_header: Arc::from([]),
                serialization_verified: true,
            },
            samples: frames
                .into_iter()
                .map(|frame| RawSample::new(frame, f(frame)))
                .collect::<Vec<_>>()
                .into(),
        }
    }
    fn document(tracks: Vec<RawTrack>, first: u32, last: u32) -> EditDocument {
        let input = DocumentInput {
            streams: Arc::from([]),
            nodes_prefix: Arc::from([]),
            channels_prefix: Arc::from([]),
            tracks,
            labels: vec![],
            first_frame: first,
            last_frame: last,
        };
        let mut doc = EditDocument::unloaded(PathBuf::from("fixture.tak"));
        doc.install_input(doc.revision(), input).unwrap();
        doc
    }
    fn request(
        method: FillMethod,
        targets: Vec<Uid>,
        refs: Vec<Uid>,
        range: (u32, u32),
        max_gap: u32,
    ) -> FillRequest {
        FillRequest {
            method,
            targets,
            references: refs,
            range,
            max_gap,
            selection: EditSelection::default(),
        }
    }
    fn pts(doc: &DocumentSnapshot, id: Uid) -> Vec<RawSample> {
        doc.track(&id).unwrap().samples.to_vec()
    }
    #[test]
    fn cubic_partial_success_is_one_reversible_patch_and_preserves_raw_samples() {
        let t1 = track(1, [0, 1, 4, 5, 6, 7, 8, 9], |f| {
            [f as f32, 2.0 * f as f32, 3.0]
        });
        let t2 = track(2, [0, 1, 6, 7, 8, 9], |f| [f as f32, 0.0, 0.0]);
        let mut doc = document(vec![t1, t2], 0, 9);
        let before = doc.snapshot();
        let out = NumericalFill
            .fill_detailed(
                &before,
                &request(FillMethod::Cubic, vec![uid(1), uid(2)], vec![], (2, 5), 3),
            )
            .unwrap();
        assert_eq!(
            (
                out.result.tracks,
                out.result.gaps,
                out.result.samples,
                out.result.skipped
            ),
            (1, 1, 2, 1)
        );
        assert!(out.skipped[0].reason.contains("Max"));
        let patch = out.patch.unwrap();
        doc.apply(&patch, true).unwrap();
        let after = doc.snapshot();
        let result = pts(&after, uid(1));
        assert_eq!(
            result.iter().map(|s| s.frame()).collect::<Vec<_>>(),
            (0..10).collect::<Vec<_>>()
        );
        for s in pts(&before, uid(1)) {
            assert_eq!(sample_at(after.track(&uid(1)).unwrap(), s.frame()), Some(s));
        }
        assert_eq!(
            sample_at(after.track(&uid(1)).unwrap(), 2).unwrap().xyz(),
            [2.0, 4.0, 3.0]
        );
        assert_eq!(pts(&after, uid(2)), pts(&before, uid(2)));
        doc.apply(&patch, false).unwrap();
        assert_eq!(pts(&doc.snapshot(), uid(1)), pts(&before, uid(1)));
        doc.apply(&patch, true).unwrap();
        assert_eq!(pts(&doc.snapshot(), uid(1)), result);
    }
    #[test]
    fn cubic_75_limit_raised_limit_selection_and_boundary_gaps() {
        let t = track(1, [1, 77, 154], |f| [f as f32, 0.0, 0.0]);
        let doc = document(vec![t], 0, 155);
        let snap = doc.snapshot();
        let out = NumericalFill
            .fill_detailed(
                &snap,
                &request(FillMethod::Cubic, vec![uid(1)], vec![], (0, 155), 100),
            )
            .unwrap();
        assert_eq!(
            (out.result.gaps, out.result.samples, out.result.skipped),
            (1, 75, 3)
        );
        assert!(out.skipped.iter().any(|s| s.reason.contains("75")));
        assert_eq!(
            out.skipped
                .iter()
                .filter(|s| s.reason.contains("boundary"))
                .count(),
            2
        );
        let out = NumericalFill
            .fill_detailed(
                &snap,
                &request(FillMethod::Cubic, vec![uid(1)], vec![], (3, 20), 75),
            )
            .unwrap();
        assert!(out.patch.is_none());
        assert_eq!(out.result.skipped, 1);
        assert!(out.skipped[0].reason.contains("wholly"));
        let out = NumericalFill
            .fill_detailed(
                &snap,
                &request(FillMethod::Cubic, vec![uid(1)], vec![], (2, 76), 74),
            )
            .unwrap();
        assert!(out.patch.is_none());
        assert_eq!(out.result.skipped, 1);
    }
    #[test]
    fn two_anchors_are_linear_and_unverified_or_nonfinite_are_skipped() {
        let t = track(1, [0, 4], |f| [f as f32, -(f as f32), 1.0]);
        let doc = document(vec![t], 0, 4);
        let out = NumericalFill
            .fill_detailed(
                &doc.snapshot(),
                &request(FillMethod::Cubic, vec![uid(1)], vec![], (0, 4), 3),
            )
            .unwrap();
        let after = out.patch.unwrap().after_snapshot();
        assert_eq!(
            sample_at(after.track(&uid(1)).unwrap(), 2).unwrap().xyz(),
            [2.0, -2.0, 1.0]
        );
        let mut t = track(2, [0, 4], |f| [f as f32, 0.0, 0.0]);
        t.records.serialization_verified = false;
        let t3 = track(3, [0, 4], |f| {
            [if f == 0 { f32::NAN } else { 0.0 }, 0.0, 0.0]
        });
        let doc = document(vec![t, t3], 0, 4);
        let out = NumericalFill
            .fill_detailed(
                &doc.snapshot(),
                &request(FillMethod::Cubic, vec![uid(2), uid(3)], vec![], (0, 4), 3),
            )
            .unwrap();
        assert!(out.patch.is_none());
        assert_eq!(out.result.skipped, 2);
    }
    #[test]
    fn pattern_translation_fills_long_gap_when_limit_raised() {
        let target = track(1, [0, 78], |f| [10.0 + f as f32, 2.0, 3.0]);
        let reference = track(2, 0..=78, |f| [f as f32, 0.0, 0.0]);
        let doc = document(vec![target, reference], 0, 78);
        let snap = doc.snapshot();
        let out = NumericalFill
            .fill_detailed(
                &snap,
                &request(
                    FillMethod::PatternBased,
                    vec![uid(1)],
                    vec![uid(2)],
                    (0, 78),
                    77,
                ),
            )
            .unwrap();
        assert_eq!((out.result.gaps, out.result.samples), (1, 77));
        let after = out.patch.unwrap().after_snapshot();
        assert_eq!(
            sample_at(after.track(&uid(1)).unwrap(), 40).unwrap().xyz(),
            [50.0, 2.0, 3.0]
        );
        let out = NumericalFill
            .fill_detailed(
                &snap,
                &request(
                    FillMethod::PatternBased,
                    vec![uid(1)],
                    vec![uid(2)],
                    (0, 78),
                    75,
                ),
            )
            .unwrap();
        assert!(out.patch.is_none());
        assert_eq!(out.result.skipped, 1);
    }
    #[test]
    fn pattern_rigid_rotation_and_reference_dropout_are_per_gap() {
        let target = track(1, [0, 4, 5, 9], |f| [10.0 + f as f32, 10.0, 0.0]);
        let ref2 = track(2, 0..=9, |f| [f as f32, 0.0, 0.0]);
        let ref3 = track(3, 0..=9, |f| [f as f32, 1.0, 0.0]);
        let ref4 = track(4, (0..=9).filter(|f| *f != 2), |f| [f as f32, 0.0, 1.0]);
        let doc = document(vec![target, ref2, ref3, ref4], 0, 9);
        let snap = doc.snapshot();
        let out = NumericalFill
            .fill_detailed(
                &snap,
                &request(
                    FillMethod::PatternBased,
                    vec![uid(1)],
                    vec![uid(2), uid(3), uid(4)],
                    (0, 9),
                    5,
                ),
            )
            .unwrap();
        assert_eq!(
            (out.result.gaps, out.result.samples, out.result.skipped),
            (1, 3, 1)
        );
        let after = out.patch.unwrap().after_snapshot();
        assert!(sample_at(after.track(&uid(1)).unwrap(), 1).is_none());
        assert_eq!(
            sample_at(after.track(&uid(1)).unwrap(), 7).unwrap().xyz(),
            [17.0, 10.0, 0.0]
        );
        let reference = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let observed = [[1.0, 2.0, 0.0], [1.0, 3.0, 0.0], [0.0, 2.0, 0.0]];
        let p = rigid_predict(&reference, &observed, [2.0, 3.0, 4.0]).unwrap();
        assert!(
            (p[0] + 2.0).abs() < 1e-12 && (p[1] - 4.0).abs() < 1e-12 && (p[2] - 4.0).abs() < 1e-12
        );
    }
    #[test]
    fn pattern_two_reference_twist_blends_radius_axial_position_and_is_reversible() {
        // The reference line translates, while the target's radius changes 1 -> 3
        // and its unobserved twist changes from +Y to +Z around the +X axis.
        let target = track(1, [0, 4], |f| {
            if f == 0 {
                [2.0, 1.0, 0.0]
            } else {
                [10.0, 8.0, -1.0]
            }
        });
        let r2 = track(2, 0..=4, |f| [f as f32 - 1.0, 2.0 * f as f32, -(f as f32)]);
        let r3 = track(3, 0..=4, |f| [f as f32 + 1.0, 2.0 * f as f32, -(f as f32)]);
        let mut doc = document(vec![target, r2, r3], 0, 4);
        let before = doc.snapshot();
        let out = NumericalFill
            .fill_detailed(
                &before,
                &request(
                    FillMethod::PatternBased,
                    vec![uid(1)],
                    vec![uid(2), uid(3)],
                    (0, 4),
                    3,
                ),
            )
            .unwrap();
        assert_eq!(
            (out.result.gaps, out.result.samples, out.result.skipped),
            (1, 3, 0)
        );
        let patch = out.patch.unwrap();
        doc.apply(&patch, true).unwrap();
        let after = doc.snapshot();
        for frame in 1..4 {
            let w = f64::from(frame) / 4.0;
            let angle = w * std::f64::consts::FRAC_PI_2;
            let radius = 1.0 + 2.0 * w;
            let expected = [
                f64::from(frame) + 2.0 + 4.0 * w,
                2.0 * f64::from(frame) + radius * angle.cos(),
                -f64::from(frame) + radius * angle.sin(),
            ];
            let actual = point(sample_at(after.track(&uid(1)).unwrap(), frame).unwrap()).unwrap();
            for axis in 0..3 {
                assert!((actual[axis] - expected[axis]).abs() < 5e-7);
            }
        }
        // At the midpoint a Cartesian blend would shrink the radius to sqrt(2.5).
        let mid = point(sample_at(after.track(&uid(1)).unwrap(), 2).unwrap()).unwrap();
        assert!(((mid[1] - 4.0).hypot(mid[2] + 2.0) - 2.0).abs() < 5e-7);
        for original in pts(&before, uid(1)) {
            assert_eq!(
                sample_at(after.track(&uid(1)).unwrap(), original.frame()),
                Some(original)
            );
        }
        for id in [uid(2), uid(3)] {
            assert_eq!(pts(&before, id), pts(&after, id));
        }
        doc.apply(&patch, false).unwrap();
        assert_eq!(pts(&doc.snapshot(), uid(1)), pts(&before, uid(1)));
        doc.apply(&patch, true).unwrap();
        assert_eq!(pts(&doc.snapshot(), uid(1)), pts(&after, uid(1)));
    }
    #[test]
    fn pattern_two_reference_shortest_arc_matches_known_rotation_and_translation() {
        let truth = |f: u32| {
            let angle = f64::from(f) * 0.8 / 12.0;
            let (sin, cos) = angle.sin_cos();
            [
                angle + 0.2 * cos - 0.7 * sin,
                angle * angle + 0.2 * sin + 0.7 * cos,
                -angle + 0.3,
            ]
            .map(|x| x as f32)
        };
        let target = track(1, [0, 12], truth);
        let reference = |n, sign| {
            track(n, 0..=12, |f| {
                let angle = f64::from(f) * 0.8 / 12.0;
                [
                    angle + sign * angle.cos(),
                    angle * angle + sign * angle.sin(),
                    -angle,
                ]
                .map(|x| x as f32)
            })
        };
        let doc = document(vec![target, reference(2, -1.0), reference(3, 1.0)], 0, 12);
        let out = NumericalFill
            .fill_detailed(
                &doc.snapshot(),
                &request(
                    FillMethod::PatternBased,
                    vec![uid(1)],
                    vec![uid(2), uid(3)],
                    (0, 12),
                    11,
                ),
            )
            .unwrap();
        assert_eq!((out.result.gaps, out.result.skipped), (1, 0));
        let after = out.patch.unwrap().after_snapshot();
        for frame in 1..12 {
            let actual = sample_at(after.track(&uid(1)).unwrap(), frame)
                .unwrap()
                .xyz();
            for (actual, expected) in actual.into_iter().zip(truth(frame)) {
                assert!((actual - expected).abs() < 2e-7);
            }
        }
    }
    #[test]
    fn pattern_two_reference_dropout_at_either_anchor_or_interior_skips_whole_gap() {
        for bad_frame in [0, 2, 4] {
            for nonfinite in [false, true] {
                let target = track(1, [0, 4, 5, 9], |f| [f as f32, 1.0, 0.0]);
                let r2 = track(2, 0..=9, |f| [f as f32 - 1.0, 0.0, 0.0]);
                let r3 = track(3, (0..=9).filter(|f| nonfinite || *f != bad_frame), |f| {
                    [
                        if nonfinite && f == bad_frame {
                            f32::NAN
                        } else {
                            f as f32 + 1.0
                        },
                        0.0,
                        0.0,
                    ]
                });
                let doc = document(vec![target, r2, r3], 0, 9);
                let out = NumericalFill
                    .fill_detailed(
                        &doc.snapshot(),
                        &request(
                            FillMethod::PatternBased,
                            vec![uid(1)],
                            vec![uid(2), uid(3)],
                            (0, 9),
                            3,
                        ),
                    )
                    .unwrap();
                assert_eq!(
                    (out.result.gaps, out.result.samples, out.result.skipped),
                    (1, 3, 1)
                );
                assert_eq!((out.skipped[0].first, out.skipped[0].last), (1, 3));
                assert!(out.skipped[0].reason.contains(if nonfinite {
                    "non-finite"
                } else {
                    "reference gap"
                }));
                let after = out.patch.unwrap().after_snapshot();
                for frame in 1..4 {
                    assert!(sample_at(after.track(&uid(1)).unwrap(), frame).is_none());
                }
                assert_eq!(
                    sample_at(after.track(&uid(1)).unwrap(), 7).unwrap().xyz(),
                    [7.0, 1.0, 0.0]
                );
            }
        }
    }
    #[test]
    fn pattern_two_reference_ambiguous_geometry_skips_whole_gap_with_reason() {
        for (case, reason) in [
            (0, "coincident"),
            (1, "antiparallel"),
            (2, "on-axis"),
            (3, "on-axis"),
            (4, "180 degree"),
        ] {
            let target = track(1, [0, 4], |f| {
                if (case == 2 && f == 0) || (case == 3 && f == 4) {
                    [0.0; 3]
                } else {
                    [0.0, if case == 4 && f == 4 { -1.0 } else { 1.0 }, 0.0]
                }
            });
            let line = |f| {
                if f == 2 && case == 0 {
                    0.0
                } else if f == 2 && case == 1 {
                    -1.0
                } else {
                    1.0
                }
            };
            let r2 = track(2, 0..=4, |f| [-line(f), 0.0, 0.0]);
            let r3 = track(3, 0..=4, |f| [line(f), 0.0, 0.0]);
            let doc = document(vec![target, r2, r3], 0, 4);
            let out = NumericalFill
                .fill_detailed(
                    &doc.snapshot(),
                    &request(
                        FillMethod::PatternBased,
                        vec![uid(1)],
                        vec![uid(2), uid(3)],
                        (0, 4),
                        3,
                    ),
                )
                .unwrap();
            assert!(out.patch.is_none(), "case {case}");
            assert_eq!(
                (out.result.gaps, out.result.samples, out.result.skipped),
                (0, 0, 1)
            );
            assert!(
                out.skipped[0].reason.contains(reason),
                "case {case}: {}",
                out.skipped[0].reason
            );
        }
    }
    #[test]
    fn two_reference_thresholds_and_endpoints_match_research_policy() {
        let pair = [[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        let tiny = [[0.0; 3], [1e-12, 0.0, 0.0]];
        assert!(pair_axis(&tiny).is_err());
        assert!(pair_axis(&[[0.0; 3], [1.0001e-12, 0.0, 0.0]]).is_ok());
        let almost_same = [[-1.0, -4e-13, 0.0], [1.0, 4e-13, 0.0]];
        assert_eq!(
            minimal_pair_predict(&pair, &almost_same, [0.0, 1.0, 0.0]).unwrap(),
            [0.0, 1.0, 0.0]
        );
        let almost_opposite = [[1.0, -4e-13, 0.0], [-1.0, 4e-13, 0.0]];
        assert!(
            minimal_pair_predict(&pair, &almost_opposite, [0.0, 1.0, 0.0])
                .unwrap_err()
                .contains("antiparallel")
        );
        let ltarget = [2.0, 1.0, 0.0];
        let rtarget = [6.0, 0.0, 3.0];
        for (weight, expected) in [(0.0, ltarget), (1.0, rtarget)] {
            let actual = twist_predict(&pair, &pair, &pair, ltarget, rtarget, weight).unwrap();
            for axis in 0..3 {
                assert!((actual[axis] - expected[axis]).abs() < 1e-14);
            }
        }
        assert!(
            twist_predict(&pair, &pair, &pair, [0.0, 1e-12, 0.0], rtarget, 0.5)
                .unwrap_err()
                .contains("on-axis")
        );
        for (epsilon, rejected) in [(4e-13_f64, true), (2e-12_f64, false)] {
            let opposite = [0.0, -epsilon.cos(), epsilon.sin()];
            let result = twist_predict(&pair, &pair, &pair, [0.0, 1.0, 0.0], opposite, 0.5);
            assert_eq!(result.is_err(), rejected);
        }
    }
    #[test]
    fn pattern_allows_two_references_rejects_three_collinear_and_allows_three_of_four() {
        let target = track(1, [0, 4], |f| [f as f32, 3.0, 0.5]);
        let r2 = track(2, 0..=4, |f| [f as f32, 0.0, 0.0]);
        let r3 = track(3, 0..=4, |f| [f as f32, 1.0, 0.0]);
        let r4 = track(4, 0..=4, |f| [f as f32, 2.0, 0.0]);
        let r5 = track(5, 0..=4, |f| [f as f32, 0.0, 1.0]);
        let doc = document(vec![target, r2, r3, r4, r5], 0, 4);
        let snap = doc.snapshot();
        assert!(NumericalFill
            .fill(
                &snap,
                &request(
                    FillMethod::PatternBased,
                    vec![uid(1)],
                    vec![uid(2), uid(3)],
                    (0, 4),
                    5
                )
            )
            .unwrap()
            .is_some());
        let out = NumericalFill
            .fill_detailed(
                &snap,
                &request(
                    FillMethod::PatternBased,
                    vec![uid(1)],
                    vec![uid(2), uid(3), uid(4)],
                    (0, 4),
                    5,
                ),
            )
            .unwrap();
        assert!(out.patch.is_none());
        assert!(out.skipped[0].reason.contains("collinear"));
        let out = NumericalFill
            .fill_detailed(
                &snap,
                &request(
                    FillMethod::PatternBased,
                    vec![uid(1)],
                    vec![uid(2), uid(3), uid(5)],
                    (0, 4),
                    5,
                ),
            )
            .unwrap();
        assert_eq!(out.result.gaps, 1);
        let target = track(1, [0, 4], |f| [f as f32, 3.0, 0.0]);
        let r2 = track(2, 0..=4, |f| [f as f32, 0.0, 0.0]);
        let r3 = track(3, 0..=4, |f| [f as f32, 1.0, 0.0]);
        let r4 = track(4, (0..=4).filter(|f| *f != 2), |f| [f as f32, 1.0, 1.0]);
        let r5 = track(5, 0..=4, |f| [f as f32, 0.0, 1.0]);
        let doc = document(vec![target, r2, r3, r4, r5], 0, 4);
        let snap = doc.snapshot();
        let out = NumericalFill
            .fill_detailed(
                &snap,
                &request(
                    FillMethod::PatternBased,
                    vec![uid(1)],
                    vec![uid(2), uid(3), uid(4), uid(5)],
                    (0, 4),
                    5,
                ),
            )
            .unwrap();
        assert_eq!(
            (out.result.gaps, out.result.samples, out.result.skipped),
            (1, 3, 0)
        );
    }
    #[test]
    fn scope_construction_and_validation() {
        let doc = document(
            vec![
                track(1, [0, 2], |_| [0.0; 3]),
                track(2, [0, 1, 2], |_| [0.0; 3]),
            ],
            0,
            2,
        );
        let snap = doc.snapshot();
        let mut selection = EditSelection::default();
        selection.selected.extend([uid(1), uid(2)]);
        selection.primary = Some(uid(1));
        let selected = FillRequest::for_scope(
            &snap,
            FillMethod::Cubic,
            FillScope::Selected,
            Some((1, 1)),
            75,
            selection.clone(),
        )
        .unwrap();
        assert_eq!(selected.targets, vec![uid(1), uid(2)]);
        assert_eq!(selected.range, (1, 1));
        let all = FillRequest::for_scope(
            &snap,
            FillMethod::PatternBased,
            FillScope::All,
            Some((1, 1)),
            75,
            selection.clone(),
        )
        .unwrap();
        assert_eq!(all.targets, vec![uid(1)]);
        assert_eq!(all.references, vec![uid(2)]);
        assert_eq!(all.range, (0, 2));
        assert!(FillRequest::for_scope(
            &snap,
            FillMethod::PatternBased,
            FillScope::Everything,
            None,
            75,
            selection.clone()
        )
        .is_err());
        assert!(FillRequest::for_scope(
            &snap,
            FillMethod::Cubic,
            FillScope::Selected,
            None,
            75,
            selection
        )
        .is_err());
        assert!(NumericalFill
            .fill(
                &snap,
                &request(FillMethod::Cubic, vec![uid(1), uid(1)], vec![], (0, 2), 1)
            )
            .is_err());
        assert!(NumericalFill
            .fill(
                &snap,
                &request(FillMethod::Cubic, vec![uid(1)], vec![], (0, 2), 0)
            )
            .is_err());
        assert!(NumericalFill
            .fill(
                &snap,
                &request(FillMethod::Cubic, vec![uid(1)], vec![], (0, 3), 1)
            )
            .is_err());
    }

    #[test]
    fn last_selected_target_matches_explicit_numeric_fill_and_preserves_selection() {
        let tracks = vec![
            track(1, [0, 1, 4, 5], |f| [f as f32 + 5.0, 2.0, 1.0]),
            track(2, 0..=5, |f| [f as f32, 0.0, 0.0]),
            track(3, 0..=5, |f| [f as f32, 1.0, 0.0]),
            track(4, 0..=5, |f| [f as f32, 0.0, 1.0]),
        ];
        for references in [
            vec![uid(2)],
            vec![uid(2), uid(3)],
            vec![uid(2), uid(3), uid(4)],
        ] {
            for scope in [FillScope::Selected, FillScope::All] {
                let mut doc = document(tracks.clone(), 0, 5);
                let snap = doc.snapshot();
                let mut selection = EditSelection::default();
                selection.selected.extend(references.iter().copied());
                selection.selected.insert(uid(1));
                selection.order = references.iter().rev().copied().chain([uid(1)]).collect();
                selection.primary = Some(uid(1));
                let resolved = FillRequest::for_scope(
                    &snap,
                    FillMethod::PatternBased,
                    scope,
                    Some((2, 3)),
                    75,
                    selection.clone(),
                )
                .unwrap();
                assert_eq!(resolved.targets, vec![uid(1)]);
                assert_eq!(resolved.references, references);
                let mut explicit = request(
                    FillMethod::PatternBased,
                    vec![uid(1)],
                    references.clone(),
                    resolved.range,
                    75,
                );
                explicit.selection = selection.clone();
                let resolved_patch = NumericalFill
                    .fill_detailed(&snap, &resolved)
                    .unwrap()
                    .patch
                    .unwrap();
                let explicit_patch = NumericalFill
                    .fill_detailed(&snap, &explicit)
                    .unwrap()
                    .patch
                    .unwrap();
                assert_eq!(
                    pts(&resolved_patch.after_snapshot(), uid(1)),
                    pts(&explicit_patch.after_snapshot(), uid(1))
                );
                assert_eq!(resolved_patch.selection_before, selection);
                assert_eq!(resolved_patch.selection_after, selection);
                doc.apply(&resolved_patch, true).unwrap();
                doc.apply(&resolved_patch, false).unwrap();
                assert_eq!(pts(&doc.snapshot(), uid(1)), pts(&snap, uid(1)));
            }
        }
    }
    #[test]
    fn empty_track_and_last_representable_frame_do_not_overflow() {
        let empty = track(1, [], |_| [0.0; 3]);
        let doc = document(vec![empty], 0, 9);
        let out = NumericalFill
            .fill_detailed(
                &doc.snapshot(),
                &request(FillMethod::Cubic, vec![uid(1)], vec![], (0, 9), 10),
            )
            .unwrap();
        assert!(out.patch.is_none());
        assert_eq!(out.result.skipped, 1);
        assert!(out.skipped[0].reason.contains("boundary"));
        let last = u32::MAX;
        let edge = track(2, [last - 2, last], |f| {
            [if f == last { 2.0 } else { 0.0 }, 0.0, 0.0]
        });
        let doc = document(vec![edge], last - 2, last);
        let out = NumericalFill
            .fill_detailed(
                &doc.snapshot(),
                &request(FillMethod::Cubic, vec![uid(2)], vec![], (last - 2, last), 1),
            )
            .unwrap();
        let post = out.patch.unwrap().after_snapshot();
        assert_eq!(
            sample_at(post.track(&uid(2)).unwrap(), last - 1)
                .unwrap()
                .xyz(),
            [1.0, 0.0, 0.0]
        );
    }
}

#[cfg(test)]
mod numeric_crosscheck {
    use super::super::smooth::{NumericalSmooth, SmoothRequest};
    use super::super::{DocumentInput, EditDocument, RecordBytes};
    use super::*;
    use serde_json::{json, Value};
    use std::path::PathBuf;
    fn uid(n: usize) -> Uid {
        [(n + 1) as u8; 16]
    }
    fn track(n: usize, rows: &Value) -> RawTrack {
        let samples: Vec<_> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let a = row.as_array().unwrap();
                RawSample::new(
                    a[0].as_u64().unwrap() as u32,
                    [
                        a[1].as_f64().unwrap() as f32,
                        a[2].as_f64().unwrap() as f32,
                        a[3].as_f64().unwrap() as f32,
                    ],
                )
            })
            .collect();
        RawTrack {
            uid: uid(n),
            name: format!("marker{n}"),
            label: None,
            records: RecordBytes {
                node: Arc::from([]),
                translation_header: Arc::from([]),
                serialization_verified: true,
            },
            samples: samples.into(),
        }
    }
    fn number_points(track: &RawTrack, a: u32, b: u32) -> Vec<[f64; 3]> {
        (a..=b)
            .map(|f| sample_at(track, f).unwrap().xyz().map(f64::from))
            .collect()
    }
    /// Used by the Python check only when its private temporary paths are provided.
    #[test]
    fn export_real_sample_numeric_comparisons() {
        let Ok(in_path) = std::env::var("TAK_2B_NUMERIC_INPUT") else {
            return;
        };
        let out_path = std::env::var("TAK_2B_NUMERIC_OUTPUT").unwrap();
        let root: Value = serde_json::from_slice(&std::fs::read(in_path).unwrap()).unwrap();
        let mut results = vec![];
        for case in root["cases"].as_array().unwrap() {
            let a = case["a"].as_u64().unwrap() as u32;
            let b = case["b"].as_u64().unwrap() as u32;
            let first = case["first"].as_u64().unwrap() as u32;
            let last = case["last"].as_u64().unwrap() as u32;
            let tracks: Vec<_> = case["tracks"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, t)| track(i, t))
                .collect();
            let input = DocumentInput {
                streams: Arc::from([]),
                nodes_prefix: Arc::from([]),
                channels_prefix: Arc::from([]),
                tracks,
                labels: vec![],
                first_frame: first,
                last_frame: last,
            };
            let mut doc = EditDocument::unloaded(PathBuf::from("numeric_fixture.tak"));
            doc.install_input(doc.revision(), input).unwrap();
            let snap = doc.snapshot();
            let target = snap.track(&uid(0)).unwrap();
            let mut fills = serde_json::Map::new();
            for (name, method, ids) in [
                ("cubic", FillMethod::Cubic, vec![]),
                ("pattern1", FillMethod::PatternBased, vec![uid(1)]),
                (
                    "pattern3",
                    FillMethod::PatternBased,
                    vec![uid(1), uid(2), uid(3)],
                ),
            ] {
                let req = FillRequest {
                    method,
                    targets: vec![uid(0)],
                    references: ids.clone(),
                    range: (a, b),
                    max_gap: 75,
                    selection: EditSelection::default(),
                };
                let refs: Vec<_> = ids.iter().map(|id| snap.track(id).unwrap()).collect();
                let double: Vec<_> = (a..=b)
                    .map(|f| {
                        match method {
                            FillMethod::Cubic => cubic_point(target, a, b, f),
                            FillMethod::PatternBased => pattern_point(target, &refs, a, b, f),
                        }
                        .unwrap()
                    })
                    .collect();
                let out = NumericalFill.fill_detailed(&snap, &req).unwrap();
                assert_eq!(out.result.gaps, 1);
                let post = out.patch.unwrap().after_snapshot();
                fills.insert(name.into(),json!({"double":double,"stored":number_points(post.track(&uid(0)).unwrap(),a,b)}));
            }
            let smooth_track = snap.track(&uid(1)).unwrap();
            let sa = first + 2;
            let sb = last - 2;
            assert_eq!(smooth_track.samples.len(), (last - first + 1) as usize);
            let mut smooths = serde_json::Map::new();
            for cutoff in [5.0, 6.0] {
                let mut curves = Vec::new();
                for axis in 0..3 {
                    let values: Vec<_> = smooth_track
                        .samples
                        .iter()
                        .map(|s| f64::from(s.xyz()[axis]))
                        .collect();
                    curves.push(
                        super::super::smooth::crosscheck_butterworth(&values, 100.0, cutoff)
                            .unwrap(),
                    );
                }
                let double: Vec<[f64; 3]> = (sa..=sb)
                    .map(|f| {
                        let i = (f - first) as usize;
                        [curves[0][i], curves[1][i], curves[2][i]]
                    })
                    .collect();
                let req = SmoothRequest {
                    targets: vec![uid(1)],
                    range: (sa, sb),
                    ui_cutoff_hz: cutoff,
                    sample_rate_hz: 100.0,
                    selection: EditSelection::default(),
                };
                let out = NumericalSmooth.smooth_detailed(&snap, &req).unwrap();
                let post = out
                    .patch
                    .as_ref()
                    .map(|p| p.after_snapshot())
                    .unwrap_or_else(|| snap.clone());
                smooths.insert(format!("{cutoff:.0}"),json!({"double":double,"stored":number_points(post.track(&uid(1)).unwrap(),sa,sb)}));
            }
            results.push(json!({"name":case["name"],"fills":fills,"smooth":smooths}));
        }
        std::fs::write(
            out_path,
            serde_json::to_vec(&json!({"cases":results})).unwrap(),
        )
        .unwrap();
    }
}
