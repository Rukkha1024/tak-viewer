//! Display model derived from a read-only `Take`: per-marker dense frame lookup, Labels-pane
//! statistics, groups, marker sticks, force plates and analog channels.
//! Colors are recorded as a [`ColorSource`] only; the viewer theme turns them into screen colors.

use crate::geom::{v3, Vec3};
use crate::marker_sets;
use crate::tak::{uid_hex, AnalogDevice, ForcePlate, MarkerKind, Take, Trajectory, Uid};
use std::collections::HashMap;

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

/// Forceplate vertical force channel and valid subsamples per frame.
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
    dense: Vec<[f32; 3]>,
    pub n_samples: usize,
    pub fill_pct: f32,
    /// Missing ranges inside [start_frame, end_frame].
    pub gaps: Vec<(i64, i64)>,
    pub max_gap: i64,
    /// Present runs of frames.
    pub segments: Vec<(i64, i64)>,
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
    pub fz: Vec<f32>,
    pub fz_abs_max: f32,
}

#[derive(Clone, Debug)]
pub struct ChannelView {
    pub name: String,
    pub dim: usize,
    /// Per-frame mean of the first min(dim, VALID_SUBSAMPLES) values, NaN = missing.
    pub per_frame: Vec<f32>,
}

#[derive(Clone, Debug)]
pub struct DeviceView {
    pub title: String,
    pub plate: Option<usize>,
    pub channels: Vec<ChannelView>,
}

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
    pub occlusion: Vec<f32>,
    /// Force plate forces / EMG are still being read (markers-first open, `tak::fast_read`).
    /// Set by the caller; `attach_analog` clears it.
    pub analog_pending: bool,
}

/// Frame range and plates of a `ViewData` (`ViewData::analog_target`).
#[derive(Clone, Debug)]
pub struct AnalogTarget {
    plates: Vec<ForcePlate>,
    start: i64,
    end: i64,
    scale: f32,
}

/// Force plates / EMG of stage 2, prepared off the drawing thread (`ViewData::attach_prepared`).
pub struct AnalogPart {
    analog: Vec<AnalogDevice>,
    warning: Option<String>,
    devices: Vec<DeviceView>,
    plates: Vec<PlateView>,
}

impl AnalogPart {
    pub fn prepare(analog: Vec<AnalogDevice>, warning: Option<String>, target: &AnalogTarget) -> AnalogPart {
        let (devices, plates) = analog_views(&analog, &target.plates, target.start, target.end, target.scale);
        AnalogPart { analog, warning, devices, plates }
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
    dense: Vec<[f32; 3]>,
    /// Box around the finite samples (display units).
    bounds: Option<(Vec3, Vec3)>,
    in_range: usize,
    gaps: Vec<(i64, i64)>,
    max_gap: i64,
    segments: Vec<(i64, i64)>,
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

fn track(t: &Trajectory, start: i64, end: i64, scale: f32) -> Track {
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
    let in_range = t.frames.iter().filter(|&&f| start <= f as i64 && f as i64 <= end).count();
    let gap_list = gaps(&t.frames, start, end);
    let max_gap = gap_list.iter().map(|(a, b)| b - a + 1).max().unwrap_or(0);
    Track { span, dense, bounds, in_range, gaps: gap_list, max_gap, segments: segments(&t.frames) }
}

/// Analog devices -> per-frame means, and force plates (corners in display units, Fz per frame
/// from the device of the same index). Shared by `build` and `AnalogPart::prepare`.
fn analog_views(analog: &[AnalogDevice], plates: &[ForcePlate], start: i64, end: i64, scale: f32) -> (Vec<DeviceView>, Vec<PlateView>) {
    let n = (end - start + 1) as usize;
    let mut devices = Vec::new();
    for (di, d) in analog.iter().enumerate() {
        let channels = d
            .channels
            .iter()
            .map(|c| {
                let mut per_frame = vec![f32::NAN; n];
                let valid = c.dim.min(VALID_SUBSAMPLES).max(1);
                for (k, &f) in c.frames.iter().enumerate() {
                    let f = f as i64;
                    if f < start || f > end {
                        continue;
                    }
                    let row = &c.values[k * c.dim..k * c.dim + valid];
                    let vals: Vec<f32> = row.iter().copied().filter(|v| v.is_finite()).collect();
                    if !vals.is_empty() {
                        per_frame[(f - start) as usize] = vals.iter().sum::<f32>() / vals.len() as f32;
                    }
                }
                ChannelView { name: c.name.clone(), dim: c.dim, per_frame }
            })
            .collect();
        let plate = if di < plates.len() { Some(di) } else { None };
        let title = match plate {
            Some(p) => format!("Force plate: {}", plates[p].name),
            None => format!("아날로그 장치 {}", di + 1),
        };
        devices.push(DeviceView { title, plate, channels });
    }

    let plates = plates
        .iter()
        .enumerate()
        .map(|(pi, p)| {
            let corners = p.corners.map(|c| Vec3::from_arr([c[0] * scale, c[1] * scale, c[2] * scale]));
            let device = if pi < devices.len() { Some(pi) } else { None };
            let fz = device
                .and_then(|d| devices[d].channels.get(FZ_CHANNEL))
                .map(|c| c.per_frame.clone())
                .unwrap_or_default();
            let fz_abs_max = fz.iter().filter(|v| v.is_finite()).fold(0.0f32, |m, v| m.max(v.abs()));
            PlateView { name: p.name.clone(), corners, fz, fz_abs_max }
        })
        .collect();
    (devices, plates)
}

impl ViewData {
    pub fn build(take: Take) -> ViewData {
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
        let tracks = crate::par::par_map(&take.trajectories, |t| track(t, start, end, scale));
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
        let (devices, plates) = analog_views(&take.analog, &take.plates, start, end, scale);

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
            occlusion,
            take,
            analog_pending: false,
        }
    }

    /// What `AnalogPart::prepare` needs from this view data (small: plate corners and the
    /// frame range), so stage 2 can be prepared on another thread.
    pub fn analog_target(&self) -> AnalogTarget {
        AnalogTarget { plates: self.take.plates.clone(), start: self.start_frame, end: self.end_frame, scale: self.unit_scale }
    }

    /// Stage 2 of the markers-first open: the analog devices arrive. Gives the same devices and
    /// plates as `build` with the analog already in the take.
    pub fn attach_analog(&mut self, analog: Vec<AnalogDevice>, warning: Option<String>) {
        let part = AnalogPart::prepare(analog, warning, &self.analog_target());
        self.attach_prepared(part);
    }

    /// Like `attach_analog`, with the per-frame work already done (`AnalogPart::prepare` on the
    /// loading thread). Only moves data: cheap on the drawing thread.
    pub fn attach_prepared(&mut self, part: AnalogPart) {
        self.take.analog = part.analog;
        self.take.warnings.extend(part.warning);
        self.devices = part.devices;
        self.plates = part.plates;
        self.analog_pending = false;
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
            frames: vec![0, 1, 3],
            values: vec![base, base + 2.0, f32::NAN, base, base + 1.0, base + 1.0],
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
