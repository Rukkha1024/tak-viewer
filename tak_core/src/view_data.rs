//! Display model derived from a read-only `Take`: per-marker dense frame lookup, Labels-pane
//! statistics, groups, marker sticks, force plates and analog channels.
//! Colors are recorded as a [`ColorSource`] only; the viewer theme turns them into screen colors.

use crate::shared_vec::SharedVec;
use crate::geom::{v3, Vec3};
use crate::edit::{DocumentSnapshot, EditError, Revision};
use crate::marker_sets;
use crate::tak::{uid_hex, AnalogChannel, AnalogDevice, DeviceSampling, ForcePlate, MarkerKind, Take, Trajectory, Uid};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

/// Where a marker / group / stick color comes from. The viewer (`ui/theme.rs`) picks the
/// actual screen color, so the core never depends on the GUI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorSource {
    /// RGB stored in the file (0..1): per-marker display table, LabelGroup member or stick color.
    File([f32; 3]),
    /// Fallback palette color of skeleton asset `i` (template markers).
    Asset(usize),
    /// Asset `i` color tinted grey (extra markers of that asset).
    AssetExtra(usize),
    /// LabelGroup / other parent without a file color.
    OtherParent,
    /// Structure not recognised.
    Unknown,
    Unlabeled,
}

/// Forceplate vertical force channel and valid subsamples per frame
/// (`scripts/export_forceplate_inputs.py` FZ_CHANNEL / FZ_SUBSAMPLES).
pub const FZ_CHANNEL: usize = 2;
pub const VALID_SUBSAMPLES: usize = 10;

#[derive(Clone, Debug)]
pub struct MarkerView {
    pub traj: usize,
    pub uid: Uid,
    pub name: String,
    pub kind: MarkerKind,
    pub slot: Option<i32>,
    pub parent: Option<Uid>,
    pub color: ColorSource,
    pub group: usize,
    /// First/last sampled frame; None when the trajectory has no samples.
    pub span: Option<(i64, i64)>,
    /// Dense positions over `span` (NaN = no sample), display units (metres).
    dense: SharedVec<[f32; 3]>,
    pub n_samples: usize,
    pub fill_pct: f32,
    /// Missing ranges inside [start_frame, end_frame] (`src/trajectory.py` `gaps`).
    pub gaps: Vec<(i64, i64)>,
    pub max_gap: i64,
    /// Present runs (`src/trajectory.py` `segments`).
    pub segments: SharedVec<(i64, i64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupKind {
    AssetTemplate,
    AssetExtra,
    LabelGroup,
    OtherParent,
    Unknown,
    Unlabeled,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub title: String,
    pub kind: GroupKind,
    pub color: ColorSource,
    pub members: Vec<usize>,
    /// Skeleton asset index for asset groups.
    pub asset: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct StickView {
    pub a: Vec<usize>,
    pub b: Vec<usize>,
    pub color: ColorSource,
    /// true = the file stores no sticks for this asset; drawn from the standard table of its
    /// marker set (`marker_sets`). The viewer can hide these.
    pub standard: bool,
}

#[derive(Clone, Debug)]
pub struct PlateView {
    pub name: String,
    pub corners: [Vec3; 4],
    /// Per-frame Fz (mean of valid subsamples), NaN = missing. Units unverified.
    pub fz: SharedVec<f32>,
    pub fz_abs_max: f32,
    /// Prepared on the reader, including the no-device display fallback.
    pub fz_signal: Arc<SignalView>,
}

#[derive(Clone, Debug)]
pub struct ChannelView {
    pub name: String,
    pub dim: usize,
    /// Per-frame mean of declared valid slots (legacy first min(dim, VALID_SUBSAMPLES)
    /// fallback when metadata is unproven), NaN = missing.
    pub per_frame: SharedVec<f32>,
    /// Plot topology prepared by the reader, shared without copying samples on the UI thread.
    pub signal: Arc<SignalView>,
    /// True input rate for graph processing. Forceplates use their frame-average series.
    pub sample_rate_hz: Option<f64>,
    /// Declared valid slots per frame for nonplates; 1 for frame-average forceplates,
    /// 0 when full-rate sampling cannot be proven from device metadata.
    pub samples_per_frame: usize,
    /// Frame extrema preserve raw analog bursts that a signed mean would cancel.
    pub raw_min: Arc<SignalView>,
    pub raw_max: Arc<SignalView>,
}

impl ChannelView {
    /// Expand only the declared valid slots, preserving missing frames as NaN. Call on a
    /// processing worker; the immutable raw `AnalogChannel` is never changed. Forceplates
    /// use `per_frame` directly rather than this nonplate full-rate accessor.
    pub fn full_rate(&self, raw: &AnalogChannel, start: i64, end: i64) -> Option<Vec<f64>> {
        let valid = self.samples_per_frame;
        self.sample_rate_hz?;
        if valid == 0 || valid > raw.dim || raw.dim == 0
            || raw.values.len() != raw.frames.len().checked_mul(raw.dim)? { return None; }
        let frames = usize::try_from(end.checked_sub(start)?.checked_add(1)?).ok()?;
        let mut values = vec![f64::NAN; frames.checked_mul(valid)?];
        for (row, &frame) in raw.frames.iter().enumerate() {
            let frame = i64::from(frame);
            if frame < start || frame > end { continue; }
            let to = (frame - start) as usize * valid;
            for j in 0..valid { values[to + j] = raw.values[row * raw.dim + j] as f64; }
        }
        Some(values)
    }
}

/// Frame-rate drawing data reduced on a worker after full-rate processing. Min/max and
/// mean use finite values only; an empty frame remains a gap in every curve.
#[derive(Clone, Debug)]
pub struct FrameSignal {
    pub mean: Vec<f32>,
    pub signal: Arc<SignalView>,
    pub min: Arc<SignalView>,
    pub max: Arc<SignalView>,
}

pub fn reduce_full_rate(values: &[f64], samples_per_frame: usize, start: i64, end: i64) -> FrameSignal {
    let frames = (end - start + 1).max(0) as usize;
    let mut mean = vec![f32::NAN; frames];
    let mut min = vec![f32::NAN; frames];
    let mut max = vec![f32::NAN; frames];
    if samples_per_frame != 0 {
        for (i, row) in values.chunks(samples_per_frame).take(frames).enumerate() {
            let mut count = 0usize;
            let (mut sum, mut lo, mut hi) = (0.0f64, f64::INFINITY, f64::NEG_INFINITY);
            for &value in row.iter().filter(|v| v.is_finite()) {
                count += 1;
                sum += value;
                lo = lo.min(value);
                hi = hi.max(value);
            }
            if count != 0 {
                mean[i] = (sum / count as f64) as f32;
                min[i] = lo as f32;
                max[i] = hi as f32;
            }
        }
    }
    FrameSignal {
        signal: Arc::new(SignalView::prepare(&mean, start, end)),
        min: Arc::new(SignalView::prepare(&min, start, end)),
        max: Arc::new(SignalView::prepare(&max, start, end)),
        mean,
    }
}

#[derive(Clone, Debug, Default)]
pub struct SignalView {
    pub runs: Arc<Vec<Vec<[f64; 2]>>>,
    pub gaps: Arc<Vec<(i64, i64)>>,
}

impl SignalView {
    pub fn prepare(values: &[f32], start: i64, end: i64) -> Self {
        let mut runs = Vec::new();
        let mut run = Vec::new();
        let mut gaps = Vec::new();
        let mut gap = None;
        for (i, &v) in values.iter().enumerate() {
            let f = start + i as i64;
            if v.is_finite() {
                run.push([f as f64, v as f64]);
                if let Some(a) = gap.take() { gaps.push((a, f - 1)); }
            } else {
                if !run.is_empty() { runs.push(std::mem::take(&mut run)); }
                gap.get_or_insert(f);
            }
        }
        if !run.is_empty() { runs.push(run); }
        if let Some(a) = gap { gaps.push((a, end)); }
        Self { runs: Arc::new(runs), gaps: Arc::new(gaps) }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceView {
    pub title: String,
    pub plate: Option<usize>,
    pub channels: Vec<ChannelView>,
}

#[derive(Clone)]
pub struct ViewData {
    pub take: Take,
    pub markers: Vec<MarkerView>,
    pub groups: Vec<Group>,
    pub sticks: Vec<StickView>,
    pub plates: Vec<PlateView>,
    pub devices: Vec<DeviceView>,
    /// File units -> metres. 1.0 unless positions look like millimetres.
    pub unit_scale: f32,
    pub bounds: Option<(Vec3, Vec3)>,
    pub start_frame: i64,
    pub end_frame: i64,
    /// Per frame: share of labeled markers without a sample (Motive navigation-bar red marks).
    pub occlusion: SharedVec<f32>,
    /// Force plate forces / EMG are still being read (markers-first open, `tak::fast_read`).
    /// Set by the caller; `attach_analog` clears it.
    pub analog_pending: bool,
    /// Non-forceplate samples have not been requested / completed yet.
    pub emg_pending: bool,
    /// Plate geometry index -> original analog manifest index, when structurally verified.
    plate_devices: Option<Vec<usize>>,
    analog_generation: u64,
    /// Marker revision used to build this view; None for the initial read-only display.
    pub edit_revision: Option<Revision>,
    marker_order: Arc<OnceLock<MarkerOrder>>,
}

/// Marker indices in Labeling order, with constant-time row and UID lookup.
pub struct MarkerOrder {
    pub indices: Vec<usize>,
    pub uids: Vec<Uid>,
    pub row_by_marker: Vec<usize>,
    pub marker_by_uid: HashMap<Uid, usize>,
}

impl MarkerOrder {
    fn build(data: &ViewData) -> Self {
        let indices: Vec<_> = data.groups.iter().flat_map(|group| group.members.iter().copied())
            .filter(|&index| {
                let marker = &data.markers[index];
                marker.kind != MarkerKind::Unlabeled || marker.n_samples != 0
            }).collect();
        let mut row_by_marker = vec![usize::MAX; data.markers.len()];
        let mut marker_by_uid = HashMap::with_capacity(indices.len());
        let mut uids = Vec::with_capacity(indices.len());
        for (row, &marker) in indices.iter().enumerate() {
            let uid = data.markers[marker].uid;
            row_by_marker[marker] = row;
            marker_by_uid.entry(uid).or_insert(marker);
            uids.push(uid);
        }
        Self { indices, uids, row_by_marker, marker_by_uid }
    }

    pub fn row(&self, marker: usize) -> Option<usize> {
        self.row_by_marker.get(marker).copied().filter(|&row| row != usize::MAX)
    }
}

/// Frame range and plates of a `ViewData` (`ViewData::analog_target`).
#[derive(Clone, Debug)]
pub struct AnalogTarget {
    plates: Vec<ForcePlate>,
    start: i64,
    end: i64,
    scale: f32,
    plate_devices: Option<Vec<usize>>,
    frame_rate: f64,
    device_sampling: Vec<Option<DeviceSampling>>,
    device_names: Vec<Option<String>>,
}

/// Force plates / EMG of stage 2, prepared off the drawing thread (`ViewData::attach_prepared`).
#[derive(Clone)]
pub struct AnalogPart {
    analog: Vec<AnalogDevice>,
    warning: Option<String>,
    devices: Vec<DeviceView>,
    plates: Vec<PlateView>,
    selected: Option<Vec<usize>>,
    initial: bool,
}

impl AnalogPart {
    pub fn prepare(analog: Vec<AnalogDevice>, warning: Option<String>, target: &AnalogTarget) -> AnalogPart {
        let (devices, plates) = analog_views(&analog, &target.plates, target.start, target.end, target.scale, target.plate_devices.as_deref(), target.frame_rate, &target.device_sampling, &target.device_names);
        AnalogPart { analog, warning, devices, plates, selected: None, initial: true }
    }

    /// A selective stage carries a complete header manifest; only these indices replace samples.
    pub fn prepare_selected(analog: Vec<AnalogDevice>, warning: Option<String>, target: &AnalogTarget, selected: Vec<usize>, initial: bool) -> Self {
        let mut part = Self::prepare(analog, warning, target);
        part.selected = Some(selected);
        part.initial = initial;
        part
    }
}

/// Present runs of sorted frames (Python `segments`).
pub fn segments(frames: &[u32]) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    for &f in frames {
        let f = f as i64;
        match out.last_mut() {
            Some(last) if f == last.1 + 1 => last.1 = f,
            Some(last) if f <= last.1 => {}
            _ => out.push((f, f)),
        }
    }
    out
}

/// Missing ranges inside [first, last] (Python `gaps`).
pub fn gaps(frames: &[u32], first: i64, last: i64) -> Vec<(i64, i64)> {
    let inside: Vec<u32> = frames
        .iter()
        .copied()
        .filter(|&f| first <= f as i64 && f as i64 <= last)
        .collect();
    let mut out = Vec::new();
    let mut cursor = first;
    for (s, e) in segments(&inside) {
        if cursor < s {
            out.push((cursor, s - 1));
        }
        cursor = e + 1;
    }
    if cursor <= last {
        out.push((cursor, last));
    }
    out
}

fn detect_unit_scale(take: &Take) -> f32 {
    let mut values: Vec<f32> = take
        .trajectories
        .iter()
        .filter(|t| t.kind != MarkerKind::Unlabeled)
        .flat_map(|t| t.xyz.iter().step_by(50).map(|p| p[1].abs()))
        .filter(|v| v.is_finite())
        .take(20_000)
        .collect();
    if values.is_empty() {
        return 1.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = values[values.len() / 2];
    if median > 50.0 {
        0.001
    } else {
        1.0
    }
}

impl MarkerView {
    pub fn position(&self, frame: i64) -> Option<Vec3> {
        let (s, e) = self.span?;
        if frame < s || frame > e {
            return None;
        }
        let p = self.dense[(frame - s) as usize];
        if p[0].is_nan() {
            None
        } else {
            Some(Vec3::from_arr(p))
        }
    }

    pub fn present(&self, frame: i64) -> bool {
        self.position(frame).is_some()
    }
}

/// Per-trajectory display tables (independent of the other trajectories).
struct Track {
    span: Option<(i64, i64)>,
    dense: SharedVec<[f32; 3]>,
    /// Box around the finite samples (display units).
    bounds: Option<(Vec3, Vec3)>,
    in_range: usize,
    gaps: Vec<(i64, i64)>,
    max_gap: i64,
    segments: SharedVec<(i64, i64)>,
}

fn union(a: Option<(Vec3, Vec3)>, b: Option<(Vec3, Vec3)>) -> Option<(Vec3, Vec3)> {
    match (a, b) {
        (Some((alo, ahi)), Some((blo, bhi))) => Some((
            v3(alo.x.min(blo.x), alo.y.min(blo.y), alo.z.min(blo.z)),
            v3(ahi.x.max(bhi.x), ahi.y.max(bhi.y), ahi.z.max(bhi.z)),
        )),
        (x, None) | (None, x) => x,
    }
}

fn track(t: &Trajectory, start: i64, end: i64, scale: f32, runs: Option<&SharedVec<(i64, i64)>>) -> Track {
    let span = match (t.frames.first(), t.frames.last()) {
        (Some(&a), Some(&b)) => Some((a as i64, b as i64)),
        _ => None,
    };
    let mut dense = Vec::new();
    let mut bounds = None;
    if let Some((s, e)) = span {
        dense = vec![[f32::NAN; 3]; (e - s + 1) as usize];
        for (f, p) in t.frames.iter().zip(&t.xyz) {
            let q = [p[0] * scale, p[1] * scale, p[2] * scale];
            dense[(*f as i64 - s) as usize] = q;
            if q.iter().all(|v| v.is_finite()) {
                let v = Vec3::from_arr(q);
                bounds = union(bounds, Some((v, v)));
            }
        }
    }
    let segments = runs.cloned().unwrap_or_else(|| segments(&t.frames).into());
    let mut cursor = start;
    let mut gap_list = Vec::new();
    for &(first, last) in segments.iter().skip_while(|&&(_, last)| last < start).take_while(|&&(first, _)| first <= end) {
        if cursor < first { gap_list.push((cursor, first - 1)); }
        cursor = last + 1;
    }
    if cursor <= end { gap_list.push((cursor, end)); }
    let in_range = t.frames.iter().filter(|&&f| start <= f as i64 && f as i64 <= end).count();
    let max_gap = gap_list.iter().map(|(a, b)| b - a + 1).max().unwrap_or(0);
    Track { span, dense: dense.into(), bounds, in_range, gaps: gap_list, max_gap, segments }
}

/// Preserve the original finite-value summation order without allocating one vector per row.
fn finite_mean(row: &[f32]) -> Option<f32> {
    let mut count = 0usize;
    let sum = row.iter().filter(|value| value.is_finite()).inspect(|_| count += 1).sum::<f32>();
    (count != 0).then(|| sum / count as f32)
}

/// Analog devices -> per-frame means, and force plates (corners in display units, Fz per frame
/// from the device of the same index). Shared by `build` and `AnalogPart::prepare`.
fn analog_views(analog: &[AnalogDevice], plates: &[ForcePlate], start: i64, end: i64, scale: f32, plate_devices: Option<&[usize]>, frame_rate: f64, device_sampling: &[Option<DeviceSampling>], device_names: &[Option<String>]) -> (Vec<DeviceView>, Vec<PlateView>) {
    let n = (end - start + 1) as usize;
    let mut devices = Vec::new();
    for (di, d) in analog.iter().enumerate() {
        let plate = match plate_devices {
            Some(map) => map.iter().position(|&device| device == di),
            None => (di < plates.len()).then_some(di),
        };
        let channels = d
            .channels
            .iter()
            .map(|c| {
                // Header-only channels must not allocate or scan one value per file frame.
                let mut per_frame = if c.frames.is_empty() { Vec::new() } else { vec![f32::NAN; n] };
                let sampling = device_sampling.get(di).copied().flatten().filter(|sampling| {
                    sampling.samples_per_frame > 0 && sampling.samples_per_frame <= c.dim
                        && frame_rate.is_finite() && frame_rate > 0.0
                        && (sampling.rate_hz - frame_rate * sampling.samples_per_frame as f64).abs()
                            <= sampling.rate_hz * 1e-6
                });
                let input_rate = if plate.is_some() || c.dim == 1 {
                    (frame_rate.is_finite() && frame_rate > 0.0).then_some(frame_rate)
                } else { sampling.map(|sampling| sampling.rate_hz) };
                let samples_per_frame = if plate.is_some() || c.dim == 1 { 1 }
                    else { sampling.map_or(0, |sampling| sampling.samples_per_frame) };
                let valid = sampling.map_or(c.dim.min(VALID_SUBSAMPLES).max(1), |sampling| sampling.samples_per_frame);
                let mut raw_min = if plate.is_none() && !c.frames.is_empty() { vec![f32::NAN; n] } else { Vec::new() };
                let mut raw_max = raw_min.clone();
                for (k, &f) in c.frames.iter().enumerate() {
                    let f = f as i64;
                    if f < start || f > end {
                        continue;
                    }
                    let row = &c.values[k * c.dim..k * c.dim + valid];
                    if let Some(mean) = finite_mean(row) {
                        let index = (f - start) as usize;
                        per_frame[index] = mean;
                        if plate.is_none() {
                            raw_min[index] = row.iter().copied().filter(|v| v.is_finite()).fold(f32::INFINITY, f32::min);
                            raw_max[index] = row.iter().copied().filter(|v| v.is_finite()).fold(f32::NEG_INFINITY, f32::max);
                        }
                    }
                }
                let signal = Arc::new(SignalView::prepare(&per_frame, start, end));
                let (raw_min, raw_max) = if plate.is_some() || c.dim == 1 {
                    (signal.clone(), signal.clone())
                } else {
                    (Arc::new(SignalView::prepare(&raw_min, start, end)), Arc::new(SignalView::prepare(&raw_max, start, end)))
                };
                ChannelView { name: c.name.clone(), dim: c.dim, per_frame: per_frame.into(), signal,
                    sample_rate_hz: input_rate, samples_per_frame, raw_min, raw_max }
            })
            .collect();
        let title = match plate {
            Some(p) => format!("Force plate: {}", plates[p].name),
            None => device_names.get(di).and_then(Option::as_ref).cloned().unwrap_or_else(|| format!("아날로그 장치 {}", di + 1)),
        };
        devices.push(DeviceView { title, plate, channels });
    }

    let plates = plates
        .iter()
        .enumerate()
        .map(|(pi, p)| {
            let corners = p.corners.map(|c| Vec3::from_arr([c[0] * scale, c[1] * scale, c[2] * scale]));
            let device = plate_devices.map_or_else(|| (pi < devices.len()).then_some(pi), |map| map.get(pi).copied());
            let fz = device
                .and_then(|d| devices.get(d)).and_then(|d| d.channels.get(FZ_CHANNEL))
                .map(|c| c.per_frame.clone())
                .unwrap_or_default();
            let fz_abs_max = fz.iter().filter(|v| v.is_finite()).fold(0.0f32, |m, v| m.max(v.abs()));
            let fz_signal = device.and_then(|d| devices.get(d)).and_then(|d| d.channels.get(FZ_CHANNEL))
                .map(|c| c.signal.clone()).unwrap_or_default();
            PlateView { name: p.name.clone(), corners, fz, fz_abs_max, fz_signal }
        })
        .collect();
    (devices, plates)
}

impl ViewData {
    pub fn build(take: Take) -> ViewData {
        Self::build_with_plate_devices(take, None)
    }

    pub fn build_with_plate_devices(take: Take, plate_devices: Option<Vec<usize>>) -> ViewData {
        Self::build_with_previous_markers(take, plate_devices, None)
    }

    fn build_with_previous_markers(take: Take, plate_devices: Option<Vec<usize>>, previous: Option<&Self>) -> ViewData {
        let start = take.start_frame;
        let end = take.end_frame.max(start);
        let frame_count = (end - start + 1) as f32;
        let scale = detect_unit_scale(&take);

        let asset_index: HashMap<Uid, usize> =
            take.assets.iter().enumerate().map(|(i, a)| (a.uid, i)).collect();
        let label_group_index: HashMap<Uid, usize> =
            take.label_groups.iter().enumerate().map(|(i, g)| (g.uid, i)).collect();

        // Groups: per asset (template, extra), per LabelGroup, other parents, unknown, unlabeled.
        let mut groups: Vec<Group> = Vec::new();
        let mut asset_groups = Vec::new();
        for (i, a) in take.assets.iter().enumerate() {
            let color = ColorSource::Asset(i);
            let title = match &a.markerset_name {
                Some(set) => format!("{} · {}", a.name, set),
                None => a.name.clone(),
            };
            groups.push(Group { title, kind: GroupKind::AssetTemplate, color, members: vec![], asset: Some(i) });
            groups.push(Group {
                title: format!("{} · 추가 마커", a.name),
                kind: GroupKind::AssetExtra,
                color: ColorSource::AssetExtra(i),
                members: vec![], asset: Some(i),
            });
            asset_groups.push((groups.len() - 2, groups.len() - 1));
        }
        let mut lg_groups = Vec::new();
        for g in &take.label_groups {
            groups.push(Group {
                title: format!("{} (LabelGroup)", g.name),
                kind: GroupKind::LabelGroup,
                color: ColorSource::OtherParent,
                members: vec![], asset: None,
            });
            lg_groups.push(groups.len() - 1);
        }
        let mut other_parent_groups: HashMap<Uid, usize> = HashMap::new();

        // Per-trajectory tables on several threads; groups and colors below in file order.
        let previous_tracks: HashMap<_, _> = previous.into_iter().flat_map(|data|
            data.markers.iter().map(|marker| (marker.uid, (&data.take.trajectories[marker.traj].frames, &marker.segments)))
        ).collect();
        let tracks = crate::par::par_map(&take.trajectories, |t| {
            let runs = previous_tracks.get(&t.uid).filter(|(frames, _)| t.frames.shares_storage(frames)).map(|(_, runs)| *runs);
            track(t, start, end, scale, runs)
        });
        let mut markers = Vec::with_capacity(take.trajectories.len());
        let mut bounds: Option<(Vec3, Vec3)> = None;
        let mut bounds_all: Option<(Vec3, Vec3)> = None;
        for ((ti, t), tr) in take.trajectories.iter().enumerate().zip(tracks) {
            let slot = t.slot;
            let (group, color) = match t.kind {
                MarkerKind::Template | MarkerKind::SkeletonExtra => {
                    let ai = t.asset_uid.and_then(|u| asset_index.get(&u).copied());
                    match ai {
                        Some(ai) => {
                            let (tg, eg) = asset_groups[ai];
                            let gi = if t.kind == MarkerKind::Template { tg } else { eg };
                            let file_color = slot.and_then(|s| {
                                take.assets[ai].marker_colors.iter().find(|(cs, _)| *cs == s).map(|(_, c)| ColorSource::File(*c))
                            });
                            (gi, file_color.unwrap_or(groups[gi].color))
                        }
                        None => (usize::MAX, ColorSource::Unknown),
                    }
                }
                MarkerKind::OtherParent => {
                    let parent = t.parent_uid.unwrap_or([0; 16]);
                    if let Some(&li) = label_group_index.get(&parent) {
                        let lg = &take.label_groups[li];
                        let c = slot
                            .and_then(|s| lg.member_rgb.iter().find(|(ms, _)| *ms as i64 == s as i64))
                            .map(|(_, c)| ColorSource::File(*c))
                            .unwrap_or(ColorSource::OtherParent);
                        (lg_groups[li], c)
                    } else {
                        let gi = *other_parent_groups.entry(parent).or_insert_with(|| {
                            groups.push(Group {
                                title: format!("기타 소속 {}", &uid_hex(&parent)[..8]),
                                kind: GroupKind::OtherParent,
                                color: ColorSource::OtherParent,
                                members: vec![], asset: None,
                            });
                            groups.len() - 1
                        });
                        (gi, ColorSource::OtherParent)
                    }
                }
                MarkerKind::Unknown => (usize::MAX, ColorSource::Unknown),
                MarkerKind::Unlabeled => (usize::MAX - 1, ColorSource::Unlabeled),
            };

            if t.kind != MarkerKind::Unlabeled {
                bounds = union(bounds, tr.bounds);
            }
            bounds_all = union(bounds_all, tr.bounds);
            markers.push(MarkerView {
                traj: ti,
                uid: t.uid,
                name: t.name.clone(),
                kind: t.kind,
                slot,
                parent: t.parent_uid,
                color,
                group,
                span: tr.span,
                dense: tr.dense,
                n_samples: t.frames.len(),
                fill_pct: tr.in_range as f32 / frame_count * 100.0,
                gaps: tr.gaps,
                max_gap: tr.max_gap,
                segments: tr.segments,
            });
        }

        // Unknown / unlabeled groups last.
        let unknown_members: Vec<usize> =
            markers.iter().enumerate().filter(|(_, m)| m.group == usize::MAX).map(|(i, _)| i).collect();
        if !unknown_members.is_empty() {
            groups.push(Group {
                title: "구조 미확인".to_owned(),
                kind: GroupKind::Unknown,
                color: ColorSource::Unknown,
                members: vec![], asset: None,
            });
            let gi = groups.len() - 1;
            for i in unknown_members {
                markers[i].group = gi;
            }
        }
        groups.push(Group {
            title: "Unlabeled".to_owned(),
            kind: GroupKind::Unlabeled,
            color: ColorSource::Unlabeled,
            members: vec![], asset: None,
        });
        let unlabeled_gi = groups.len() - 1;
        for m in markers.iter_mut() {
            if m.group == usize::MAX - 1 {
                m.group = unlabeled_gi;
            }
        }
        for (i, m) in markers.iter().enumerate() {
            groups[m.group].members.push(i);
        }
        for g in groups.iter_mut() {
            match g.kind {
                GroupKind::AssetExtra => g.members.sort_by_key(|&i| std::cmp::Reverse(markers[i].slot.unwrap_or(0))),
                GroupKind::Unlabeled => {}
                _ => g.members.sort_by_key(|&i| markers[i].slot.unwrap_or(i32::MAX)),
            }
        }
        groups.retain(|g| !g.members.is_empty() || g.kind == GroupKind::AssetTemplate);
        // Re-point marker.group after retain.
        for (gi, g) in groups.iter().enumerate() {
            for &m in &g.members {
                markers[m].group = gi;
            }
        }

        // Marker sticks: asset slot pairs -> marker indices.
        let mut sticks = Vec::new();
        for a in &take.assets {
            let mut by_slot: HashMap<i32, Vec<usize>> = HashMap::new();
            for (i, m) in markers.iter().enumerate() {
                if m.kind == MarkerKind::Template && take.trajectories[m.traj].asset_uid == Some(a.uid) {
                    if let Some(s) = m.slot {
                        by_slot.entry(s).or_default().push(i);
                    }
                }
            }
            // Zero sticks stored -> standard table of the same marker set, if known.
            let standard = if a.sticks.is_empty() {
                marker_sets::standard_sticks(a.markerset_name.as_deref())
            } else {
                None
            };
            let from_standard = standard.is_some();
            for s in standard.as_deref().unwrap_or(&a.sticks) {
                let ea = by_slot.get(&(s.slot_a as i32)).cloned().unwrap_or_default();
                let eb = by_slot.get(&(s.slot_b as i32)).cloned().unwrap_or_default();
                if !ea.is_empty() && !eb.is_empty() {
                    sticks.push(StickView { a: ea, b: eb, color: ColorSource::File(s.rgb), standard: from_standard });
                }
            }
        }

        let n = (end - start + 1) as usize;
        let (devices, plates) = analog_views(&take.analog, &take.plates, start, end, scale, plate_devices.as_deref(), take.frame_rate, &take.device_sampling, &take.device_names);

        // Robust bounds (2–98 percentile per axis) of labeled markers, so stray points do not
        // shrink the initial view.
        let robust = {
            let mut xs: [Vec<f32>; 3] = [vec![], vec![], vec![]];
            for m in markers.iter().filter(|m| !matches!(m.kind, MarkerKind::Unlabeled | MarkerKind::Unknown)) {
                for p in m.dense.iter().step_by(5) {
                    if p.iter().all(|v| v.is_finite()) {
                        for a in 0..3 {
                            xs[a].push(p[a]);
                        }
                    }
                }
            }
            if xs[0].len() >= 10 {
                // k-th smallest value (the same value a full sort puts at index k).
                let q = |v: &mut Vec<f32>, t: f32| {
                    let k = ((v.len() - 1) as f32 * t) as usize;
                    *v.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap()).1
                };
                let lo = v3(q(&mut xs[0], 0.02), q(&mut xs[1], 0.02), q(&mut xs[2], 0.02));
                let hi = v3(q(&mut xs[0], 0.98), q(&mut xs[1], 0.98), q(&mut xs[2], 0.98));
                Some((lo, hi))
            } else {
                None
            }
        };

        // Occlusion per frame over labeled markers (template preferred, else any labeled).
        let has_template = markers.iter().any(|m| m.kind == MarkerKind::Template);
        let labeled: Vec<&MarkerView> = markers
            .iter()
            .filter(|m| if has_template { m.kind == MarkerKind::Template } else { !matches!(m.kind, MarkerKind::Unlabeled | MarkerKind::Unknown) })
            .collect();
        let mut occlusion = vec![0.0f32; n];
        if !labeled.is_empty() {
            let mut missing = vec![0u32; n];
            for m in &labeled {
                for &(a, b) in &m.gaps {
                    for f in a..=b {
                        missing[(f - start) as usize] += 1;
                    }
                }
            }
            let total = labeled.len() as f32;
            for (o, c) in occlusion.iter_mut().zip(missing) {
                *o = c as f32 / total;
            }
        }

        ViewData {
            markers,
            groups,
            sticks,
            plates,
            devices,
            unit_scale: scale,
            bounds: robust.or(bounds).or(bounds_all),
            start_frame: start,
            end_frame: end,
            occlusion: occlusion.into(),
            take,
            analog_pending: false,
            emg_pending: false,
            plate_devices,
            analog_generation: 0,
            edit_revision: None,
            marker_order: Arc::new(OnceLock::new()),
        }
    }

    /// Clones and analog attachment share the lazy index. Marker rebuilds start a new index.
    pub fn marker_order_handle(&self) -> &Arc<OnceLock<MarkerOrder>> { &self.marker_order }

    /// First initialization scans marker metadata. Call on a worker.
    pub fn prepare_marker_order(&self) { self.marker_order.get_or_init(|| MarkerOrder::build(self)); }

    /// Rebuild marker-derived display data from an immutable document snapshot. Run on a worker
    /// with an owned Take whose analog stage is complete. Publication must check the revision.
    /// The initial fast open does not call this and does not copy an editable document.
    pub fn rebuild_edited(take: Take, snapshot: &DocumentSnapshot) -> Result<ViewData, EditError> {
        Self::rebuild_edited_with_layout(take, snapshot, None, None)
    }

    /// Run on a worker: retain currently loaded analog stages during marker edit / undo / save.
    pub fn rebuild_markers(&self, snapshot: &DocumentSnapshot) -> Result<ViewData, EditError> {
        let mut view = Self::rebuild_edited_with_layout(self.take.clone(), snapshot, self.plate_devices.clone(), Some(self))?;
        view.analog_pending = self.analog_pending;
        view.emg_pending = self.emg_pending;
        view.analog_generation = self.analog_generation;
        Ok(view)
    }

    fn rebuild_edited_with_layout(mut take: Take, snapshot: &DocumentSnapshot, plate_devices: Option<Vec<usize>>, previous: Option<&Self>) -> Result<ViewData, EditError> {
        let input = snapshot.input().ok_or(EditError::NotLoaded)?;
        let mut existing = HashMap::new();
        for track in std::mem::take(&mut take.trajectories) {
            if existing.insert(track.uid, track).is_some() { return Err(EditError::DuplicateUid); }
        }
        let order = snapshot.track_order();
        let original: std::collections::HashSet<Uid> = input.tracks.iter().map(|t| t.uid).collect();
        let mut tracks = Vec::with_capacity(order.len());
        for &uid in order {
            let Some(raw) = snapshot.track(&uid) else { continue };
            let mut trajectory = match existing.remove(&uid) {
                Some(t) => t,
                None if original.contains(&uid) => return Err(EditError::UnsupportedLayout),
                None => {
                    let (parent_uid, slot, kind, asset_uid) = match raw.label {
                        None => (Some([0; 16]), Some(0), MarkerKind::Unlabeled, None),
                        Some(label) => {
                            let asset = take.assets.iter().any(|a| a.uid == label.parent);
                            let kind = if asset && label.slot > 0 { MarkerKind::Template } else if asset { MarkerKind::SkeletonExtra } else { MarkerKind::OtherParent };
                            (Some(label.parent), Some(label.slot), kind, asset.then_some(label.parent))
                        }
                    };
                    Trajectory { uid, name: raw.name.clone(), parent_uid, slot_raw: slot.map(|s| s as u32), slot, kind, asset_uid, frames: Vec::new().into(), xyz: Vec::new().into(), raw16_sha256: String::new(), lenient_membership: false }
                }
            };
            // Read XYZ as before and detect presence changes in the same pass. Smoothing and
            // untouched tracks can keep their frame storage and already built presence runs.
            let mut same_frames = previous.is_some() && trajectory.frames.len() == raw.samples.len();
            trajectory.xyz = raw.samples.iter().enumerate().map(|(i, sample)| {
                same_frames = same_frames && trajectory.frames[i] == sample.frame();
                sample.xyz()
            }).collect();
            if !same_frames {
                trajectory.frames = raw.samples.iter().map(|s| s.frame()).collect();
            }
            trajectory.raw16_sha256.clear();
            tracks.push(trajectory);
        }
        take.trajectories = tracks;
        let mut view = ViewData::build_with_previous_markers(take, plate_devices, previous);
        view.edit_revision = Some(snapshot.revision());
        Ok(view)
    }

    /// What `AnalogPart::prepare` needs from this view data (small: plate corners and the
    /// frame range), so stage 2 can be prepared on another thread.
    pub fn analog_generation(&self) -> u64 { self.analog_generation }

    pub fn analog_target(&self) -> AnalogTarget {
        AnalogTarget { plates: self.take.plates.clone(), start: self.start_frame, end: self.end_frame, scale: self.unit_scale, plate_devices: self.plate_devices.clone(), frame_rate: self.take.frame_rate, device_sampling: self.take.device_sampling.clone(), device_names: self.take.device_names.clone() }
    }

    /// Stage 2 of the markers-first open: the analog devices arrive. Gives the same devices and
    /// plates as `build` with the analog already in the take.
    pub fn attach_analog(&mut self, analog: Vec<AnalogDevice>, warning: Option<String>) {
        let part = AnalogPart::prepare(analog, warning, &self.analog_target());
        self.attach_prepared(part);
    }

    /// Like `attach_analog`, with the per-frame work already done (`AnalogPart::prepare` on the
    /// loading thread). Drops the previous buffers here; use `swap_prepared` to retire them elsewhere.
    pub fn attach_prepared(&mut self, part: AnalogPart) {
        drop(self.swap_prepared(part));
    }

    /// Move prepared stage-2 data into this view and return the previous buffers without
    /// dropping them. The caller can retire that part off the drawing thread.
    pub fn swap_prepared(&mut self, mut part: AnalogPart) -> AnalogPart {
        self.analog_generation = self.analog_generation.wrapping_add(1);
        self.take.warnings.extend(part.warning.take());
        if part.initial { self.analog_pending = false; }
        if !part.initial || part.selected.is_none() { self.emg_pending = false; }
        if let Some(selected) = &part.selected {
            // Full ordered manifests keep device/channel selections stable through both stages.
            for &i in selected {
                if i < self.take.analog.len() && i < part.analog.len() && i < self.devices.len() && i < part.devices.len() {
                    std::mem::swap(&mut self.take.analog[i], &mut part.analog[i]);
                    std::mem::swap(&mut self.devices[i], &mut part.devices[i]);
                }
            }
            for (pi, plate) in self.plates.iter_mut().enumerate() {
                let device = self.plate_devices.as_ref().and_then(|map| map.get(pi)).copied().unwrap_or(pi);
                if selected.contains(&device) {
                    if let Some(incoming) = part.plates.get_mut(pi) { std::mem::swap(plate, incoming); }
                }
            }
            part
        } else {
            AnalogPart {
                analog: std::mem::replace(&mut self.take.analog, part.analog),
                warning: None,
                devices: std::mem::replace(&mut self.devices, part.devices),
                plates: std::mem::replace(&mut self.plates, part.plates),
                selected: None,
                initial: true,
            }
        }
    }

    /// Centroid and top (max Y) of an asset's template markers present at `frame`.
    pub fn asset_anchor(&self, group: usize, frame: i64) -> Option<(Vec3, f32)> {
        let mut sum = v3(0.0, 0.0, 0.0);
        let mut top = f32::NEG_INFINITY;
        let mut n = 0;
        for &m in &self.groups[group].members {
            if let Some(p) = self.markers[m].position(frame) {
                sum = sum + p;
                top = top.max(p.y);
                n += 1;
            }
        }
        (n > 0).then(|| (sum * (1.0 / n as f32), top))
    }

    /// Start of the next / previous gap of marker `m` relative to `frame` (Motive Z / Shift+Z).
    pub fn next_gap(&self, m: usize, frame: i64, forward: bool) -> Option<i64> {
        let gaps = &self.markers[m].gaps;
        if forward {
            gaps.iter().map(|g| g.0).find(|&s| s > frame)
        } else {
            gaps.iter().map(|g| g.0).filter(|&s| s < frame).last()
        }
    }

    pub fn file_name(&self) -> String {
        self.take
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn present_count(&self, frame: i64, kind: Option<MarkerKind>) -> usize {
        self.markers
            .iter()
            .filter(|m| kind.map_or(true, |k| m.kind == k) && m.present(frame))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tak::AnalogChannel;

    #[test]
    fn analog_prepared_elsewhere_equals_analog_read_with_the_markers() {
        let channel = |name: &str, base: f32| AnalogChannel {
            name: name.into(),
            kind: "Analog".into(),
            dim: 2,
            frames: vec![0, 1, 3].into(),
            values: vec![base, base + 2.0, f32::NAN, base, base + 1.0, base + 1.0].into(),
        };
        let device = AnalogDevice { channels: (0..3).map(|k| channel(&format!("AnalogChannel{k}"), k as f32)).collect() };
        let take = Take {
            path: "x.tak".into(),
            file_size: 0,
            metadata: vec![],
            frame_rate: 100.0,
            start_frame: 0,
            end_frame: 4,
            trajectories: vec![],
            assets: vec![],
            label_groups: vec![],
            plates: vec![ForcePlate { name: "P1".into(), corners: [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]] }],
            analog: vec![device.clone(), device],
            device_sampling: vec![], device_names: vec![],
            warnings: vec![],
        };
        let full = ViewData::build(take.clone());
        let mut later = ViewData::build(Take { analog: vec![], ..take.clone() });
        later.analog_pending = true;
        let part = AnalogPart::prepare(take.analog.clone(), Some("w".into()), &later.analog_target());
        later.attach_prepared(part);
        assert!(!later.analog_pending);
        assert_eq!(format!("{:?}", later.devices), format!("{:?}", full.devices));
        assert_eq!(format!("{:?}", later.plates), format!("{:?}", full.plates));
        assert_eq!(later.take.analog.len(), 2);
        assert_eq!(later.take.warnings, vec!["w".to_owned()]);
        assert_eq!(full.plates[0].fz.len(), 5);

        // Swapping has the same result and returns ownership of all previous allocations.
        let old_analog = later.take.analog.as_ptr();
        let old_devices = later.devices.as_ptr();
        let old_plates = later.plates.as_ptr();
        let part = AnalogPart::prepare(take.analog.clone(), None, &later.analog_target());
        later.analog_pending = true;
        let retired = later.swap_prepared(part);
        assert!(!later.analog_pending);
        assert_eq!(retired.analog.as_ptr(), old_analog);
        assert_eq!(retired.devices.as_ptr(), old_devices);
        assert_eq!(retired.plates.as_ptr(), old_plates);
        assert!(retired.warning.is_none());
        assert_eq!(format!("{:?}", later.devices), format!("{:?}", full.devices));
        assert_eq!(format!("{:?}", later.plates), format!("{:?}", full.plates));
        assert_eq!(later.take.warnings, vec!["w".to_owned()]);
    }

    #[test]
    fn selective_stages_preserve_ordered_manifest_forceplate_mapping_and_prior_samples() {
        let device = |base: f32| AnalogDevice { channels: (0..3).map(|i| AnalogChannel {
            name: format!("AnalogChannel{i}"), kind: "Analog".into(), dim: 1,
            frames: vec![0, 2].into(), values: vec![base + i as f32, base + i as f32 + 1.0].into(),
        }).collect() };
        let full = vec![device(10.0), device(100.0), device(20.0)];
        let mut manifest = full.clone();
        for d in &mut manifest { for c in &mut d.channels { c.frames.clear(); c.values.clear(); } }
        let take = Take {
            path: "x.tak".into(), file_size: 0, metadata: vec![], frame_rate: 100.0,
            start_frame: 0, end_frame: 2, trajectories: vec![], assets: vec![], label_groups: vec![],
            plates: vec![ForcePlate { name: "P1".into(), corners: [[0.0; 3]; 4] }],
            analog: manifest.clone(), device_sampling: vec![], device_names: vec![], warnings: vec![],
        };
        let mut view = ViewData::build_with_plate_devices(take, Some(vec![1]));
        view.analog_pending = true;
        view.emg_pending = true;
        assert_eq!(view.devices.iter().map(|d| d.plate).collect::<Vec<_>>(), vec![None, Some(0), None]);
        assert!(view.devices.iter().flat_map(|d| &d.channels).all(|c| c.per_frame.is_empty() && c.signal.runs.is_empty()));
        let target = view.analog_target();
        let mut initial = manifest.clone(); initial[1] = full[1].clone();
        view.attach_prepared(AnalogPart::prepare_selected(initial, None, &target, vec![1], true));
        assert!(!view.analog_pending && view.emg_pending);
        assert_eq!(view.plates[0].fz[0], 102.0);
        let force_buffer = view.plates[0].fz.as_ptr();
        let mut lazy = manifest; lazy[0] = full[0].clone(); lazy[2] = full[2].clone();
        view.attach_prepared(AnalogPart::prepare_selected(lazy, None, &target, vec![0, 2], false));
        assert!(!view.emg_pending);
        assert_eq!(view.plates[0].fz.as_ptr(), force_buffer, "EMG must not replace or drop forceplate buffers");
        assert_eq!(view.take.analog[0].channels[0].values, vec![10.0, 11.0]);
        assert_eq!(view.take.analog[1].channels[0].values, vec![100.0, 101.0]);
        assert_eq!(view.devices[2].channels[0].signal.gaps.as_ref(), &vec![(1, 1)]);
        assert_eq!(view.devices[2].channels[0].signal.runs.as_ref(), &vec![vec![[0.0, 20.0]], vec![[2.0, 21.0]]]);
        // Malformed/IO-failed selective decode can contain no devices despite a valid map.
        let failed = AnalogPart::prepare_selected(vec![], Some("failed".into()), &target, vec![0, 2], false);
        view.attach_prepared(failed);
        assert_eq!(view.plates[0].fz.as_ptr(), force_buffer);
        assert_eq!(view.take.warnings, vec!["failed"]);
    }

    fn analog_test_take(channel: AnalogChannel, sampling: Option<DeviceSampling>) -> Take {
        Take {
            path: "fullrate.tak".into(), file_size: 0, metadata: vec![], frame_rate: 100.0,
            start_frame: 100, end_frame: 103, trajectories: vec![], assets: vec![], label_groups: vec![],
            plates: vec![], analog: vec![AnalogDevice { channels: vec![channel] }],
            device_sampling: vec![sampling], device_names: vec![], warnings: vec![],
        }
    }

    #[test]
    fn full_rate_declared_slots_preserve_gaps_extrema_and_raw_bytes() {
        let mut values = vec![f32::MAX; 32];
        values[..3].copy_from_slice(&[-3.0, 0.0, 3.0]);
        values[16..19].copy_from_slice(&[-6.0, f32::NAN, 9.0]);
        let channel = AnalogChannel { name: "AnalogChannel0".into(), kind: "Float16Channel".into(),
            dim: 16, frames: vec![100, 102].into(), values: values.into() };
        let take = analog_test_take(channel, Some(DeviceSampling { rate_hz: 300.0, samples_per_frame: 3 }));
        let raw_bits: Vec<_> = take.analog[0].channels[0].values.iter().map(|v| v.to_bits()).collect();
        let view = ViewData::build(take);
        let channel = &view.devices[0].channels[0];
        assert_eq!(channel.sample_rate_hz, Some(300.0));
        assert_eq!(channel.samples_per_frame, 3);
        let full = channel.full_rate(&view.take.analog[0].channels[0], 100, 103).unwrap();
        assert_eq!(&full[..3], &[-3.0, 0.0, 3.0]);
        assert!(full[3..6].iter().all(|v| v.is_nan()));
        assert_eq!((full[6], full[8]), (-6.0, 9.0));
        assert!(full[7].is_nan() && full[9..].iter().all(|v| v.is_nan()));
        assert_eq!(channel.raw_min.runs.as_ref(), &vec![vec![[100.0, -3.0]], vec![[102.0, -6.0]]]);
        assert_eq!(channel.raw_max.runs.as_ref(), &vec![vec![[100.0, 3.0]], vec![[102.0, 9.0]]]);
        // Worker-owned processing/reduction changes its copy only, never ViewData/Take.
        let processed = crate::signal::process(&full, 300.0, &[crate::signal::Step::Rectify]).unwrap();
        let reduced = reduce_full_rate(&processed.values, 3, 100, 103);
        assert_eq!(reduced.mean[0], 2.0);
        assert_eq!(reduced.mean[2], 7.5);
        assert_eq!(reduced.min.runs.as_ref(), &vec![vec![[100.0, 0.0]], vec![[102.0, 6.0]]]);
        assert_eq!(reduced.max.runs.as_ref(), &vec![vec![[100.0, 3.0]], vec![[102.0, 9.0]]]);
        assert_eq!(reduced.signal.gaps.as_ref(), &vec![(101, 101), (103, 103)]);
        assert_eq!(raw_bits, view.take.analog[0].channels[0].values.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
        assert_eq!(channel.per_frame[0], 0.0);
    }

    #[test]
    fn full_rate_filter_chain_never_changes_view_data_raw_values() {
        let frames: Vec<_> = (100..200).collect();
        let mut values = Vec::new();
        for &frame in &frames {
            for j in 0..10 {
                let phase = ((frame - 100) * 10 + j) as f32;
                values.push((phase * 0.1).sin() + 0.25);
            }
            values.extend([f32::MAX, f32::NAN, f32::INFINITY, -0.0, 123.0, -456.0]);
        }
        values[50 * 16 + 2] = f32::NAN;
        let channel = AnalogChannel { name: "AnalogChannel0".into(), kind: "Float16Channel".into(), dim: 16, frames: frames.into(), values: values.into() };
        let mut take = analog_test_take(channel, Some(DeviceSampling { rate_hz: 1000.0, samples_per_frame: 10 }));
        take.end_frame = 199;
        let view = ViewData::build(take);
        let raw = &view.take.analog[0].channels[0];
        let channel = &view.devices[0].channels[0];
        let raw_bits: Vec<_> = raw.values.iter().map(|v| v.to_bits()).collect();
        let mean_bits: Vec<_> = channel.per_frame.iter().map(|v| v.to_bits()).collect();
        let full = channel.full_rate(raw, 100, 199).unwrap();
        let chain = [crate::signal::Step::Demean,
            crate::signal::Step::HighPass { cutoff_hz: 20.0, order: 3, zero_phase: true },
            crate::signal::Step::Rectify,
            crate::signal::Step::LowPass { cutoff_hz: 6.0, order: 3, zero_phase: true }];
        let processed = crate::signal::process(&full, 1000.0, &chain).unwrap();
        let reduced = reduce_full_rate(&processed.values, 10, 100, 199);
        assert!(processed.values[502].is_nan());
        assert!(processed.values.iter().zip(&full).any(|(a, b)| a.is_finite() && (a - b).abs() > 0.01));
        assert!(!reduced.signal.runs.is_empty());
        assert_eq!(raw_bits, raw.values.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
        assert_eq!(mean_bits, channel.per_frame.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
    }

    #[test]
    fn full_rate_requires_sampling_metadata_matching_frame_rate_and_capacity() {
        let channel = AnalogChannel { name: "AnalogChannel0".into(), kind: "Float16Channel".into(),
            dim: 16, frames: vec![100].into(), values: vec![1.0; 16].into() };
        for sampling in [None,
            Some(DeviceSampling { rate_hz: 1000.0, samples_per_frame: 9 }),
            Some(DeviceSampling { rate_hz: 1700.0, samples_per_frame: 17 })] {
            let view = ViewData::build(analog_test_take(channel.clone(), sampling));
            let c = &view.devices[0].channels[0];
            assert_eq!(c.sample_rate_hz, None);
            assert_eq!(c.samples_per_frame, 0);
            assert!(c.full_rate(&view.take.analog[0].channels[0], 100, 103).is_none());
        }
        let view = ViewData::build(analog_test_take(channel, Some(DeviceSampling { rate_hz: 1000.0, samples_per_frame: 10 })));
        assert_eq!(view.devices[0].channels[0].sample_rate_hz, Some(1000.0));
        assert_eq!(view.devices[0].channels[0].samples_per_frame, 10);
        let scalar = AnalogChannel { name: "AnalogChannel0".into(), kind: "FloatChannel".into(),
            dim: 1, frames: vec![100].into(), values: vec![1.0].into() };
        let view = ViewData::build(analog_test_take(scalar, None));
        assert_eq!(view.devices[0].channels[0].sample_rate_hz, Some(100.0));
    }

    #[test]
    fn full_rate_manifest_stays_allocation_free_for_unrequested_channels() {
        let channel = AnalogChannel { name: "AnalogChannel0".into(), kind: "Float16Channel".into(),
            dim: 16, frames: vec![].into(), values: vec![].into() };
        let mut take = analog_test_take(channel, Some(DeviceSampling { rate_hz: 1000.0, samples_per_frame: 10 }));
        take.end_frame = 100_000_000;
        // Avoid the marker occlusion allocation in build: directly exercise analog preparation.
        let (devices, _) = analog_views(&take.analog, &[], 100, take.end_frame, 1.0, None, 100.0, &take.device_sampling, &take.device_names);
        let c = &devices[0].channels[0];
        assert_eq!(c.sample_rate_hz, Some(1000.0));
        assert!(c.per_frame.is_empty() && c.signal.runs.is_empty() && c.raw_min.runs.is_empty() && c.raw_max.runs.is_empty());
    }

    #[test]
    fn finite_channel_mean_without_row_allocation_matches_original_bits() {
        let reference = |row: &[f32]| {
            let values: Vec<f32> = row.iter().copied().filter(|value| value.is_finite()).collect();
            (!values.is_empty()).then(|| (values.iter().sum::<f32>() / values.len() as f32).to_bits())
        };
        let cases: &[&[f32]] = &[
            &[], &[f32::NAN, f32::INFINITY, f32::NEG_INFINITY],
            &[0.0], &[-0.0], &[-0.0, -0.0], &[0.0, -0.0],
            &[1e20, 1.0, -1e20, 3.0], &[f32::MAX, f32::MAX, -f32::MAX],
            &[f32::from_bits(1), -f32::from_bits(1), f32::MIN_POSITIVE],
            &[f32::NAN, 2.0, f32::INFINITY, -1.0, f32::NEG_INFINITY, 5.0],
        ];
        for row in cases { assert_eq!(finite_mean(row).map(f32::to_bits), reference(row)); }
        // Include arbitrary exponent/sign/mantissa patterns in every supported subsample count.
        let mut state = 0x83a9_1c25u32;
        for len in 1..=VALID_SUBSAMPLES {
            for _ in 0..1024 {
                let row: Vec<f32> = (0..len).map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    f32::from_bits(state)
                }).collect();
                assert_eq!(finite_mean(&row).map(f32::to_bits), reference(&row), "row length {len}");
            }
        }
    }

    #[test]
    fn gaps_match_python_definition() {
        // Python: gaps([2,3,4,8,9], 0, 12) -> [(0,1),(5,7),(10,12)]
        assert_eq!(gaps(&[2, 3, 4, 8, 9], 0, 12), vec![(0, 1), (5, 7), (10, 12)]);
        assert_eq!(gaps(&[], 5, 9), vec![(5, 9)]);
        assert_eq!(gaps(&[5, 6, 7, 8, 9], 5, 9), vec![]);
        // Frames outside the range are ignored.
        assert_eq!(gaps(&[1, 6, 20], 5, 9), vec![(5, 5), (7, 9)]);
    }

    #[test]
    fn segments_merge_consecutive() {
        assert_eq!(segments(&[1, 2, 3, 7, 9, 10]), vec![(1, 3), (7, 7), (9, 10)]);
        assert_eq!(segments(&[]), vec![]);
    }
}

#[cfg(test)]
#[path = "view_data_tracks_tests.rs"]
mod tracks_tests;
