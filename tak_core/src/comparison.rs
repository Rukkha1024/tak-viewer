//! Sparse sample comparison and bounded frame candidates for matched captures.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::iter::Peekable;

use serde::Serialize;

use crate::summary::FrameRange;
use crate::tak::{uid_hex, MarkerKind, Take, Trajectory, Uid};
use crate::view_data::{detect_unit_scale, display_sample_present};

mod slots;
pub use slots::{SlotComparison, SlotStats};

#[derive(Debug, Default, Serialize)]
pub struct KindCounts {
    pub template: usize,
    pub skeleton_extra: usize,
    pub other_parent: usize,
    pub unlabeled: usize,
    pub unknown: usize,
}

impl KindCounts {
    fn add(&mut self, kind: MarkerKind) {
        *match kind {
            MarkerKind::Template => &mut self.template,
            MarkerKind::SkeletonExtra => &mut self.skeleton_extra,
            MarkerKind::OtherParent => &mut self.other_parent,
            MarkerKind::Unlabeled => &mut self.unlabeled,
            MarkerKind::Unknown => &mut self.unknown,
        } += 1;
    }
}

#[derive(Debug, Serialize)]
pub struct Rename {
    pub uid_hex: String,
    pub reference_name: String,
    pub other_name: String,
}

#[derive(Debug, Serialize)]
pub struct RoleChange {
    pub uid_hex: String,
    pub reference_name: String,
    pub other_name: String,
    pub reference: StructuralRole,
    pub other: StructuralRole,
}

#[derive(Debug, Serialize)]
pub struct StructuralRole {
    pub kind: &'static str,
    pub parent_uid_hex: Option<String>,
    pub slot: Option<i32>,
}

impl From<&Trajectory> for StructuralRole {
    fn from(t: &Trajectory) -> Self {
        Self {
            kind: t.kind.as_str(),
            parent_uid_hex: t.parent_uid.as_ref().map(uid_hex),
            slot: t.slot,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SharedName {
    pub name: String,
    pub reference_uids: Vec<String>,
    pub other_uids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Peak {
    pub frame: i64,
    pub mm: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SampleStats {
    pub reference_only: u64,
    pub other_only: u64,
    pub both_present: u64,
    pub compared_frames: u64,
    pub max: Option<Peak>,
    pub mean_mm: Option<f64>,
    pub above_1_mm: u64,
    pub above_20_mm: u64,
    #[serde(skip)]
    sum_mm: f64,
}

impl SampleStats {
    fn distance(&mut self, frame: i64, mm: f64) {
        self.compared_frames += 1;
        self.sum_mm += mm;
        self.above_1_mm += u64::from(mm > 1.0);
        self.above_20_mm += u64::from(mm > 20.0);
        self.keep_peak(Peak { frame, mm });
    }

    fn keep_peak(&mut self, peak: Peak) {
        if self
            .max
            .is_none_or(|old| (peak.mm, Reverse(peak.frame)) > (old.mm, Reverse(old.frame)))
        {
            self.max = Some(peak);
        }
    }

    fn add(&mut self, other: &Self) {
        self.reference_only += other.reference_only;
        self.other_only += other.other_only;
        self.both_present += other.both_present;
        self.compared_frames += other.compared_frames;
        self.sum_mm += other.sum_mm;
        self.above_1_mm += other.above_1_mm;
        self.above_20_mm += other.above_20_mm;
        if let Some(peak) = other.max {
            self.keep_peak(peak);
        }
    }

    fn finish(&mut self) {
        self.mean_mm =
            (self.compared_frames > 0).then(|| self.sum_mm / self.compared_frames as f64);
    }
}

#[derive(Debug, Serialize)]
pub struct MarkerStats {
    pub uid_hex: String,
    pub reference_name: Option<String>,
    pub reference_kind: Option<&'static str>,
    pub other_name: Option<String>,
    pub other_kind: Option<&'static str>,
    pub structural_role_changed: bool,
    pub samples: SampleStats,
}

impl MarkerStats {
    pub fn name(&self) -> &str {
        self.other_name
            .as_deref()
            .or(self.reference_name.as_deref())
            .unwrap_or_default()
    }
}

#[derive(Debug, Default, Serialize)]
pub struct NonTemplate {
    pub reference: KindCounts,
    pub other: KindCounts,
    pub samples: SampleStats,
}

#[derive(Debug, Serialize)]
pub struct Comparison {
    pub frame_range_equal: bool,
    pub frame_rate_equal: bool,
    pub reference_template: usize,
    pub other_template: usize,
    pub reference_only: KindCounts,
    pub other_only: KindCounts,
    pub rename_count: usize,
    pub renames: Vec<Rename>,
    pub role_change_count: usize,
    pub role_changes: Vec<RoleChange>,
    pub shared_name_count: usize,
    pub shared_names: Vec<SharedName>,
    pub marker_set: Vec<MarkerStats>,
    pub top_distance_uids: Vec<String>,
    pub marker_set_totals: SampleStats,
    pub extra_markers: NonTemplate,
}

#[derive(Debug, Serialize)]
pub struct FrameCoverage {
    pub frame: i64,
    pub template_present: usize,
    pub template_total: usize,
    pub missing_template: Vec<String>,
}

pub fn frame_coverage(take: &Take, frames: &[i64]) -> Vec<FrameCoverage> {
    let template = take
        .trajectories
        .iter()
        .filter(|t| t.kind == MarkerKind::Template);
    let total = template.clone().count();
    frames
        .iter()
        .map(|&frame| {
            let missing: Vec<_> = template
                .clone()
                .filter(|t| {
                    let present = u32::try_from(frame)
                        .ok()
                        .and_then(|f| t.frames.binary_search(&f).ok())
                        .is_some_and(|i| display_sample_present(&t.xyz[i]));
                    !present
                })
                .map(|t| t.name.clone())
                .collect();
            FrameCoverage {
                frame,
                template_total: total,
                template_present: total - missing.len(),
                missing_template: missing,
            }
        })
        .collect()
}

/// Stable, bounded rankings. Keep the best score for each frame, with early frames winning ties.
#[derive(Default)]
struct RankedFrames {
    frames: HashMap<i64, u64>,
    scores: BTreeSet<(u64, Reverse<i64>)>,
}

impl RankedFrames {
    fn insert(&mut self, limit: usize, score: u64, frame: i64) {
        let key = (score, Reverse(frame));
        if self.scores.len() == limit && self.scores.first().is_some_and(|worst| *worst >= key) {
            return;
        }
        if let Some(&old) = self.frames.get(&frame) {
            if old >= score {
                return;
            }
            self.scores.remove(&(old, Reverse(frame)));
        }
        self.frames.insert(frame, score);
        self.scores.insert(key);
        if self.scores.len() > limit {
            let (_, Reverse(last)) = self.scores.pop_first().unwrap();
            self.frames.remove(&last);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameReason {
    Explicit,
    GapEdge,
    LargestDistance,
    PresenceStart,
    PresenceMiddle,
    PresenceEnd,
    Control,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PickedFrame {
    pub frame: i64,
    pub reason: FrameReason,
}

pub struct AutoFrames {
    range: FrameRange,
    limit: usize,
    gaps: RankedFrames,
    distance: RankedFrames,
    stretches: BTreeSet<(u64, Reverse<i64>, i64)>,
}

impl AutoFrames {
    pub fn new(range: FrameRange, limit: usize) -> Self {
        Self {
            range,
            limit,
            gaps: RankedFrames::default(),
            distance: RankedFrames::default(),
            stretches: BTreeSet::new(),
        }
    }

    fn in_range(&self, frame: i64) -> bool {
        (self.range.first..=self.range.last).contains(&frame)
    }

    pub fn add_gaps(&mut self, take: &Take) {
        for marker in take
            .trajectories
            .iter()
            .filter(|t| t.kind == MarkerKind::Template)
        {
            let mut previous = None;
            for (frame, _) in samples(Some(marker)) {
                let frame = i64::from(frame);
                let start = previous.map_or(take.start_frame, |prev| prev + 1);
                if start < frame {
                    self.add_gap(start, frame - 1, previous, Some(frame));
                }
                previous = Some(frame);
            }
            if let Some(last) = previous.filter(|last| *last < take.end_frame) {
                self.add_gap(last + 1, take.end_frame, Some(last), None);
            }
        }
    }

    fn add_gap(&mut self, first: i64, last: i64, before: Option<i64>, after: Option<i64>) {
        let score = first.abs_diff(last).saturating_add(1);
        for frame in [before, Some(first), Some(last), after]
            .into_iter()
            .flatten()
        {
            if self.in_range(frame) {
                self.gaps.insert(self.limit, score, frame);
            }
        }
    }

    fn add_distance(&mut self, frame: i64, mm: f64) {
        if mm > 0.0 && self.in_range(frame) {
            self.distance.insert(self.limit * 2, mm.to_bits(), frame);
        }
    }

    fn add_stretch(&mut self, first: i64, last: i64) {
        let (first, last) = (first.max(self.range.first), last.min(self.range.last));
        if first <= last {
            self.stretches
                .insert((last.abs_diff(first), Reverse(first), last));
            if self.stretches.len() > self.limit * 2 {
                self.stretches.pop_first();
            }
        }
    }

    pub fn finish(self) -> Vec<PickedFrame> {
        let mut chosen = BTreeMap::new();
        let mut insert = |frame, reason| {
            if chosen.len() < self.limit {
                chosen.entry(frame).or_insert(reason);
            }
        };
        for (ranked, reason) in [
            (self.gaps, FrameReason::GapEdge),
            (self.distance, FrameReason::LargestDistance),
        ] {
            for (_, Reverse(frame)) in ranked.scores.into_iter().rev() {
                insert(frame, reason);
            }
        }
        for (_, Reverse(first), last) in self.stretches.into_iter().rev() {
            insert(first, FrameReason::PresenceStart);
            insert(midpoint(first, last), FrameReason::PresenceMiddle);
            insert(last, FrameReason::PresenceEnd);
        }
        let remaining = self.limit - chosen.len();
        if remaining > 0 {
            for frame in even_frames(self.range, remaining) {
                chosen.entry(frame).or_insert(FrameReason::Control);
            }
            // Existing picks can coincide with controls. Use the full evenly spaced grid to fill.
            for frame in even_frames(self.range, self.limit) {
                if chosen.len() == self.limit {
                    break;
                }
                chosen.entry(frame).or_insert(FrameReason::Control);
            }
        }
        chosen
            .into_iter()
            .map(|(frame, reason)| PickedFrame { frame, reason })
            .collect()
    }
}

fn midpoint(first: i64, last: i64) -> i64 {
    (i128::from(first) + (i128::from(last) - i128::from(first)) / 2) as i64
}

fn even_frames(range: FrameRange, n: usize) -> impl Iterator<Item = i64> {
    let first = i128::from(range.first);
    let span = i128::from(range.last) - first;
    (0..n).map(move |i| {
        if n == 1 {
            midpoint(range.first, range.last)
        } else {
            (first + span * i as i128 / (n - 1) as i128) as i64
        }
    })
}

fn samples(marker: Option<&Trajectory>) -> impl Iterator<Item = (u32, &[f32; 3])> {
    marker
        .into_iter()
        .flat_map(|t| t.frames.iter().copied().zip(t.xyz.iter()))
        .filter(|(_, xyz)| display_sample_present(xyz))
}

fn shared_names(reference: &Take, other: &Take, top: usize) -> (usize, Vec<SharedName>) {
    let names = |take: &Take| {
        let mut names = BTreeMap::<String, BTreeSet<Uid>>::new();
        for marker in &take.trajectories {
            names
                .entry(marker.name.clone())
                .or_default()
                .insert(marker.uid);
        }
        names
    };
    let right = names(other);
    let mut observations = Vec::new();
    let mut count = 0;
    for (name, left_uids) in names(reference) {
        if let Some(right_uids) = right.get(&name).filter(|&uids| *uids != left_uids) {
            count += 1;
            if observations.len() < top {
                observations.push(SharedName {
                    name,
                    reference_uids: left_uids.iter().map(uid_hex).collect(),
                    other_uids: right_uids.iter().map(uid_hex).collect(),
                });
            }
        }
    }
    (count, observations)
}

pub fn compare_pair(
    reference: &Take,
    other: &Take,
    mut auto: Option<&mut AutoFrames>,
    top: usize,
) -> (SlotComparison, Result<Comparison, DuplicateUid>) {
    let scales = [detect_unit_scale(reference), detect_unit_scale(other)];
    let primary = slots::compare(reference, other, scales, auto.as_deref_mut(), top);
    let uid_auto = if matches!(primary, SlotComparison::Skipped { .. }) {
        auto
    } else {
        None
    };
    let secondary = compare_uid(reference, other, scales, uid_auto, top);
    (primary, secondary)
}

fn compare_uid(
    reference: &Take,
    other: &Take,
    scales: [f32; 2],
    mut auto: Option<&mut AutoFrames>,
    top: usize,
) -> Result<Comparison, DuplicateUid> {
    let left = index_by_uid(reference, true)?;
    let right = index_by_uid(other, false)?;
    let (shared_name_count, shared_names) = shared_names(reference, other, top);
    let mut comparison = Comparison {
        frame_range_equal: (reference.start_frame, reference.end_frame)
            == (other.start_frame, other.end_frame),
        frame_rate_equal: reference.frame_rate == other.frame_rate,
        reference_template: 0,
        other_template: 0,
        reference_only: KindCounts::default(),
        other_only: KindCounts::default(),
        rename_count: 0,
        renames: Vec::new(),
        role_change_count: 0,
        role_changes: Vec::new(),
        shared_name_count,
        shared_names,
        marker_set: Vec::new(),
        top_distance_uids: Vec::new(),
        marker_set_totals: SampleStats::default(),
        extra_markers: NonTemplate::default(),
    };
    for marker in &reference.trajectories {
        let matched = right.get(&marker.uid).copied();
        comparison.add_marker(Some(marker), matched, scales, auto.as_deref_mut());
    }
    for marker in other
        .trajectories
        .iter()
        .filter(|m| !left.contains_key(&m.uid))
    {
        comparison.add_marker(None, Some(marker), scales, auto.as_deref_mut());
    }
    comparison
        .renames
        .sort_unstable_by(|a, b| a.uid_hex.cmp(&b.uid_hex));
    comparison.renames.truncate(top);
    comparison
        .role_changes
        .sort_unstable_by(|a, b| a.uid_hex.cmp(&b.uid_hex));
    comparison.role_changes.truncate(top);
    comparison.marker_set.sort_unstable_by(|a, b| {
        let peak = |m: &MarkerStats| m.samples.max.map_or(-1.0, |p| p.mm);
        peak(b)
            .total_cmp(&peak(a))
            .then_with(|| a.uid_hex.cmp(&b.uid_hex))
    });
    comparison.top_distance_uids = comparison
        .marker_set
        .iter()
        .filter(|m| m.samples.max.is_some())
        .take(top)
        .map(|m| m.uid_hex.clone())
        .collect();
    comparison.marker_set_totals.finish();
    comparison.extra_markers.samples.finish();
    Ok(comparison)
}

#[derive(Debug)]
pub struct DuplicateUid {
    pub in_reference: bool,
    pub uid: Uid,
}

fn index_by_uid(
    take: &Take,
    in_reference: bool,
) -> Result<HashMap<Uid, &Trajectory>, DuplicateUid> {
    let mut index = HashMap::with_capacity(take.trajectories.len());
    for marker in &take.trajectories {
        if index.insert(marker.uid, marker).is_some() {
            return Err(DuplicateUid {
                in_reference,
                uid: marker.uid,
            });
        }
    }
    Ok(index)
}

impl Comparison {
    fn add_marker(
        &mut self,
        reference: Option<&Trajectory>,
        other: Option<&Trajectory>,
        scales: [f32; 2],
        auto: Option<&mut AutoFrames>,
    ) {
        let a_set = reference.is_some_and(|t| t.kind == MarkerKind::Template);
        let b_set = other.is_some_and(|t| t.kind == MarkerKind::Template);
        self.reference_template += usize::from(a_set);
        self.other_template += usize::from(b_set);
        let mut structural_role_changed = false;
        match (reference, other) {
            (Some(a), None) => self.reference_only.add(a.kind),
            (None, Some(b)) => self.other_only.add(b.kind),
            (Some(a), Some(b)) => {
                if a.name != b.name {
                    self.rename_count += 1;
                    self.renames.push(Rename {
                        uid_hex: uid_hex(&a.uid),
                        reference_name: a.name.clone(),
                        other_name: b.name.clone(),
                    });
                }
                structural_role_changed =
                    (a.kind, a.parent_uid, a.slot) != (b.kind, b.parent_uid, b.slot);
                if structural_role_changed {
                    self.role_change_count += 1;
                    self.role_changes.push(RoleChange {
                        uid_hex: uid_hex(&a.uid),
                        reference_name: a.name.clone(),
                        other_name: b.name.clone(),
                        reference: a.into(),
                        other: b.into(),
                    });
                }
            }
            (None, None) => unreachable!("a comparison row needs a trajectory"),
        }
        if a_set || b_set {
            let stats = sample_stats(reference, other, scales, auto);
            self.marker_set_totals.add(&stats);
            self.marker_set.push(MarkerStats {
                uid_hex: uid_hex(&reference.or(other).unwrap().uid),
                reference_name: reference.map(|m| m.name.clone()),
                reference_kind: reference.map(|m| m.kind.as_str()),
                other_name: other.map(|m| m.name.clone()),
                other_kind: other.map(|m| m.kind.as_str()),
                structural_role_changed,
                samples: stats,
            });
        } else {
            if let Some(m) = reference {
                self.extra_markers.reference.add(m.kind);
            }
            if let Some(m) = other {
                self.extra_markers.other.add(m.kind);
            }
            self.extra_markers
                .samples
                .add(&sample_stats(reference, other, scales, None));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Reference,
    Other,
}

#[derive(Default)]
struct PresenceStretch(Option<(Side, i64, i64)>);

impl PresenceStretch {
    fn sample(&mut self, side: Side, frame: i64, auto: &mut AutoFrames) {
        if let Some((old_side, _, last)) = &mut self.0 {
            if *old_side == side && *last + 1 == frame {
                *last = frame;
                return;
            }
        }
        self.flush(auto);
        self.0 = Some((side, frame, frame));
    }

    fn flush(&mut self, auto: &mut AutoFrames) {
        if let Some((_, first, last)) = self.0.take() {
            auto.add_stretch(first, last);
        }
    }
}

fn sample_stats(
    reference: Option<&Trajectory>,
    other: Option<&Trajectory>,
    scales: [f32; 2],
    mut auto: Option<&mut AutoFrames>,
) -> SampleStats {
    let mut a = samples(reference).peekable();
    let mut b = samples(other).peekable();
    let mut stats = SampleStats::default();
    let mut stretch = PresenceStretch::default();
    while let Some(order) = next_sample_order(&mut a, &mut b) {
        let (side, frame) = match order {
            Ordering::Less => {
                stats.reference_only += 1;
                (Side::Reference, a.next().unwrap().0)
            }
            Ordering::Greater => {
                stats.other_only += 1;
                (Side::Other, b.next().unwrap().0)
            }
            Ordering::Equal => {
                let (frame, p) = a.next().unwrap();
                let (_, q) = b.next().unwrap();
                stats.both_present += 1;
                let delta = |i: usize| f64::from(p[i] * scales[0]) - f64::from(q[i] * scales[1]);
                let (x, y, z) = (delta(0), delta(1), delta(2));
                let mm = (x * x + y * y + z * z).sqrt() * 1000.0;
                if mm.is_finite() {
                    stats.distance(i64::from(frame), mm);
                    if let Some(picks) = auto.as_deref_mut() {
                        picks.add_distance(i64::from(frame), mm);
                    }
                }
                if let Some(picks) = auto.as_deref_mut() {
                    stretch.flush(picks);
                }
                continue;
            }
        };
        if let Some(picks) = auto.as_deref_mut() {
            stretch.sample(side, i64::from(frame), picks);
        }
    }
    if let Some(picks) = auto {
        stretch.flush(picks);
    }
    stats.finish();
    stats
}

fn next_sample_order<'a>(
    a: &mut Peekable<impl Iterator<Item = (u32, &'a [f32; 3])>>,
    b: &mut Peekable<impl Iterator<Item = (u32, &'a [f32; 3])>>,
) -> Option<Ordering> {
    match (a.peek(), b.peek()) {
        (Some((a, _)), Some((b, _))) => Some(a.cmp(b)),
        (Some(_), None) => Some(Ordering::Less),
        (None, Some(_)) => Some(Ordering::Greater),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MarkerKind::{OtherParent, Template, Unlabeled};

    fn compare(
        reference: &Take,
        other: &Take,
        auto: Option<&mut AutoFrames>,
        top: usize,
    ) -> Result<Comparison, DuplicateUid> {
        let scales = [detect_unit_scale(reference), detect_unit_scale(other)];
        compare_uid(reference, other, scales, auto, top)
    }

    fn marker(uid: u8, name: &str, kind: MarkerKind, points: &[(u32, f32)]) -> Trajectory {
        Trajectory {
            uid: [uid; 16],
            name: name.into(),
            kind,
            parent_uid: None,
            asset_uid: None,
            slot: None,
            slot_raw: None,
            frames: points.iter().map(|p| p.0).collect::<Vec<_>>().into(),
            xyz: points
                .iter()
                .map(|p| [p.1, 1.0, 0.0])
                .collect::<Vec<_>>()
                .into(),
            raw16_sha256: String::new(),
            lenient_membership: false,
        }
    }

    fn take(trajectories: Vec<Trajectory>) -> Take {
        Take {
            path: "test.tak".into(),
            file_size: 0,
            metadata: vec![],
            frame_rate: 100.0,
            start_frame: 100,
            end_frame: 110,
            trajectories,
            assets: vec![],
            label_groups: vec![],
            plates: vec![],
            analog: vec![],
            device_sampling: vec![],
            device_names: vec![],
            warnings: vec![],
        }
    }

    #[test]
    fn uids_control_matching_despite_name_and_kind_changes() {
        let a = take(vec![
            marker(1, "old", Unlabeled, &[(100, 0.0), (101, 0.0), (102, 0.0)]),
            marker(2, "alias", Template, &[(100, 0.0)]),
            marker(4, "extra", OtherParent, &[(105, 2.0)]),
        ]);
        let mut b = take(vec![
            marker(
                1,
                "new",
                Template,
                &[(101, 0.0005), (102, 0.025), (103, 0.0)],
            ),
            marker(3, "alias", Template, &[(100, 9.0)]),
            marker(4, "extra", OtherParent, &[(105, 2.0)]),
        ]);
        b.frame_rate = 120.0;
        b.end_frame = 109;

        let report = compare(&a, &b, None, 1).unwrap();
        assert!(!report.frame_rate_equal && !report.frame_range_equal);
        assert_eq!((report.reference_template, report.other_template), (1, 2));
        assert_eq!(
            (report.reference_only.template, report.other_only.template),
            (1, 1)
        );
        assert_eq!(report.renames[0].uid_hex, uid_hex(&[1; 16]));
        assert_eq!(report.renames[0].other_name, "new");
        assert_eq!(
            report.shared_names[0].reference_uids,
            vec![uid_hex(&[2; 16])]
        );
        assert_eq!(report.shared_names[0].other_uids, vec![uid_hex(&[3; 16])]);
        assert_eq!(report.top_distance_uids, vec![uid_hex(&[1; 16])]);
        let samples = &report.marker_set[0].samples;
        assert_eq!(
            (
                samples.reference_only,
                samples.other_only,
                samples.compared_frames
            ),
            (1, 1, 2)
        );
        assert_eq!((samples.above_1_mm, samples.above_20_mm), (1, 1));
        assert_eq!(samples.max.unwrap().frame, 102);
        assert!((samples.max.unwrap().mm - 25.0).abs() < 1e-5);
        assert!((samples.mean_mm.unwrap() - 12.75).abs() < 1e-5);
        let totals = &report.marker_set_totals;
        assert_eq!(
            (
                totals.reference_only,
                totals.other_only,
                totals.compared_frames
            ),
            (2, 2, 2)
        );
        assert_eq!(report.extra_markers.reference.other_parent, 1);
        assert_eq!(report.extra_markers.samples.max.unwrap().mm, 0.0);

        let no_lists = compare(&a, &b, None, 0).unwrap();
        assert_eq!((no_lists.rename_count, no_lists.shared_name_count), (1, 1));
        assert!(no_lists.renames.is_empty() && no_lists.shared_names.is_empty());
        assert!(no_lists.top_distance_uids.is_empty());
    }

    fn with_asset(mut take: Take, uid: u8, slots: i32) -> Take {
        use crate::tak::{Constraint, Joint, SkeletonAsset, SkeletonModel};
        take.assets.push(SkeletonAsset {
            uid: [uid; 16],
            name: "Subject".into(),
            markerset_name: Some(format!("Template ({slots})")),
            model: Some(SkeletonModel {
                serialization_version: 123,
                template_code: 2039,
                joints: vec![Joint {
                    id: 1,
                    parent: 0,
                    offset: [0.0; 3],
                }],
                constraints: (1..=slots)
                    .map(|slot| Constraint {
                        joint_id: 1,
                        slot,
                        slot_raw: slot as u32,
                        name: format!("A{slot}"),
                        local: [0.0; 3],
                        enabled: true,
                        primary: true,
                    })
                    .collect(),
            }),
            slot_comparison_model: None,
            sticks: vec![],
            marker_colors: vec![],
        });
        take
    }

    fn slot_marker(uid: u8, asset: u8, slot: i32, name: &str, points: &[(u32, f32)]) -> Trajectory {
        let mut track = marker(uid, name, Template, points);
        track.parent_uid = Some([asset; 16]);
        track.asset_uid = track.parent_uid;
        track.slot = Some(slot);
        track.slot_raw = Some(slot as u32);
        track
    }

    #[test]
    fn slots_compare_with_different_uids_and_report_renamed_slots() {
        let a = with_asset(
            take(vec![slot_marker(
                1,
                31,
                1,
                "A1",
                &[(100, 0.0), (101, 0.0), (102, 0.0)],
            )]),
            31,
            1,
        );
        let b = with_asset(
            take(vec![slot_marker(
                2,
                32,
                1,
                "new name",
                &[(101, 0.002), (102, 0.025), (103, 0.0)],
            )]),
            32,
            1,
        );
        let (slot, uid) = compare_pair(&a, &b, None, 1);
        let SlotComparison::Compared(slot) = slot else {
            panic!("{slot:?}")
        };
        assert_eq!(uid.unwrap().marker_set_totals.both_present, 0);
        assert_eq!(slot.name_differences[0].id.slot, 1);
        assert_eq!(slot.name_differences[0].reference_name, "A1");
        assert_eq!(slot.name_differences[0].other_name, "new name");
        assert_eq!(slot.top_distance_slots[0].slot, 1);
        let stats = &slot.totals;
        assert_eq!(
            (stats.reference_only, stats.other_only, stats.both_present),
            (1, 1, 2)
        );
        assert_eq!(
            (stats.compared_frames, stats.above_1_mm, stats.above_20_mm),
            (2, 2, 1)
        );
        assert_eq!(stats.max.unwrap().frame, 102);
        assert!((stats.max.unwrap().mm - 25.0).abs() < 1e-5);
        assert!((stats.mean_mm.unwrap() - 13.5).abs() < 1e-5);
        assert_eq!(slot.slots[0].reference_name, "A1");
        assert_eq!(slot.slots[0].samples.as_ref().unwrap().max, stats.max);
    }

    #[test]
    fn full_definition_pairs_fourteen_out_of_thirty_nine_recorded_slots() {
        let make = |count, offset| {
            with_asset(
                take(
                    (1..=count)
                        .map(|slot| {
                            slot_marker(
                                offset + slot as u8,
                                31,
                                slot,
                                &format!("A{slot}"),
                                &[(100, 0.0)],
                            )
                        })
                        .collect(),
                ),
                31,
                39,
            )
        };
        let (slot, _) = compare_pair(&make(39, 5), &make(14, 160), None, 10);
        let SlotComparison::Compared(slot) = slot else {
            panic!("{slot:?}")
        };
        assert_eq!(slot.assets[0].template_size, 39);
        assert_eq!(slot.slots.len(), 39);
        assert_eq!(slot.top_distance_slots.len(), 10);
        assert_eq!(
            (
                slot.totals.both_present,
                slot.totals.reference_only,
                slot.totals.other_only
            ),
            (14, 25, 0)
        );
        let unrecorded = slot.slots.iter().find(|s| s.id.slot == 39).unwrap();
        assert_eq!(unrecorded.other_name, "A39");
        assert!(unrecorded.other_uids.is_empty());
    }

    #[test]
    fn differing_or_unreadable_templates_skip_slots_and_keep_uid_results() {
        let a = with_asset(take(vec![slot_marker(1, 31, 1, "A", &[(100, 0.0)])]), 31, 1);
        let mut b = a.clone();
        b.assets[0].model.as_mut().unwrap().template_code = 2016;
        let (slot, uid) = compare_pair(&a, &b, None, 0);
        assert!(matches!(
            slot,
            SlotComparison::Skipped {
                reason: slots::SkipReason::DifferentTemplate
            }
        ));
        assert_eq!(uid.unwrap().marker_set_totals.compared_frames, 1);
        b.assets[0].model = None;
        let (slot, _) = compare_pair(&a, &b, None, 0);
        assert!(matches!(
            slot,
            SlotComparison::Skipped {
                reason: slots::SkipReason::UndeterminedTemplate
            }
        ));
    }

    #[test]
    fn shared_asset_ids_disambiguate_multiple_instances_of_one_template() {
        let a = with_asset(with_asset(take(vec![]), 31, 1), 32, 1);
        let mut b = a.clone();
        b.assets.reverse();
        b.assets[0].uid = [33; 16];
        let (slot, _) = compare_pair(&a, &b, None, 0);
        let SlotComparison::Compared(slot) = slot else {
            panic!("{slot:?}")
        };
        assert_eq!(slot.assets[0].other_asset_uid_hex, uid_hex(&[31; 16]));
        assert_eq!(slot.assets[1].other_asset_uid_hex, uid_hex(&[33; 16]));
        b.assets[1].uid = [34; 16];
        let (slot, _) = compare_pair(&a, &b, None, 0);
        assert!(matches!(
            slot,
            SlotComparison::Skipped {
                reason: slots::SkipReason::AmbiguousAssets
            }
        ));
    }

    #[test]
    fn duplicate_slot_leaves_unambiguous_slots_comparable() {
        let a = with_asset(
            take(vec![
                slot_marker(1, 31, 1, "A1", &[(100, 0.0)]),
                slot_marker(2, 31, 2, "A2", &[(100, 0.0)]),
            ]),
            31,
            2,
        );
        let mut b = a.clone();
        b.trajectories
            .push(slot_marker(3, 31, 1, "duplicate", &[(100, 0.5)]));
        let (slot, uid) = compare_pair(&a, &b, None, 10);
        let SlotComparison::Incomplete(slot) = slot else {
            panic!("{slot:?}")
        };
        let ambiguous = slot.slots.iter().find(|r| r.id.slot == 1).unwrap();
        assert_eq!(ambiguous.other_uids.len(), 2);
        assert!(ambiguous.samples.is_none());
        assert_eq!(slot.totals.both_present, 1);
        assert_eq!(slot.top_distance_slots[0].slot, 2);
        assert_eq!(uid.unwrap().marker_set_totals.both_present, 2);
    }

    #[test]
    fn automatic_distance_uses_slots_then_falls_back_to_uid_for_different_templates() {
        let points = |slot, edited| {
            (100..=110)
                .map(|f| {
                    (
                        f,
                        match (slot, f, edited) {
                            (2, 101, _) => 20.0,
                            (2, _, _) => 1.0,
                            (1, 105, true) => 0.025,
                            _ => 0.0,
                        },
                    )
                })
                .collect::<Vec<_>>()
        };
        let make = |edited| {
            with_asset(
                take(
                    (1..=2)
                        .map(|slot| {
                            let uid = if edited { 3 - slot } else { slot };
                            slot_marker(uid as u8, 31, slot, "same", &points(slot, edited))
                        })
                        .collect(),
                ),
                31,
                2,
            )
        };
        let a = make(false);
        let mut b = make(true);
        let range = FrameRange {
            first: 100,
            last: 110,
        };
        let mut auto = AutoFrames::new(range, 1);
        let (_, uid) = compare_pair(&a, &b, Some(&mut auto), 1);
        let uid = uid.unwrap();
        assert_eq!(uid.role_change_count, 2);
        assert_eq!(uid.role_changes.len(), 1);
        assert_eq!(uid.rename_count, 0);
        assert_eq!(uid.marker_set_totals.max.unwrap().frame, 101);
        assert_eq!(
            auto.finish(),
            vec![PickedFrame {
                frame: 105,
                reason: FrameReason::LargestDistance
            }]
        );

        b.assets[0].markerset_name = Some("Different (2)".into());
        let mut auto = AutoFrames::new(range, 1);
        let (slot, uid) = compare_pair(&a, &b, Some(&mut auto), 1);
        assert!(matches!(slot, SlotComparison::Skipped { .. }));
        assert!(uid.is_ok());
        assert_eq!(
            auto.finish(),
            vec![PickedFrame {
                frame: 101,
                reason: FrameReason::LargestDistance
            }]
        );
    }

    #[test]
    fn identical_takes_are_zero_and_scale_matches_display() {
        let a = take(vec![
            marker(1, "same", Template, &[(100, 1.0), (102, 1.5)]),
            marker(2, "same", Template, &[(102, 1.5)]),
        ]);
        let c = compare(&a, &a, None, 10).unwrap();
        assert!(c.frame_rate_equal && c.frame_range_equal);
        assert_eq!((c.rename_count, c.shared_name_count), (0, 0));
        let s = c.marker_set_totals;
        assert_eq!(
            (s.reference_only, s.other_only, s.compared_frames),
            (0, 0, 3)
        );
        assert_eq!((s.above_1_mm, s.above_20_mm), (0, 0));
        assert_eq!(s.mean_mm, Some(0.0));
        assert_eq!(
            s.max,
            Some(Peak {
                frame: 100,
                mm: 0.0
            })
        );

        let mut b = a.clone();
        for m in &mut b.trajectories {
            m.xyz = m
                .xyz
                .iter()
                .map(|p| p.map(|v| v * 1000.0))
                .collect::<Vec<_>>()
                .into();
        }
        assert!(
            compare(&a, &b, None, 10)
                .unwrap()
                .marker_set_totals
                .mean_mm
                .unwrap()
                < 0.001
        );
    }

    #[test]
    fn auto_frames_are_deterministic_and_prioritize_gap_edges_and_distances() {
        let range = FrameRange {
            first: 100,
            last: 110,
        };
        let a = take(vec![marker(
            1,
            "A",
            Template,
            &[
                (100, 0.0),
                (101, 0.0),
                (102, 0.0),
                (106, 0.0),
                (107, 0.0),
                (108, 0.0),
                (109, 0.0),
                (110, 0.0),
            ],
        )]);
        let mut b = a.clone();
        b.trajectories[0].xyz = a.trajectories[0]
            .xyz
            .iter()
            .enumerate()
            .map(|(i, p)| if i == 6 { [0.03, 1.0, 0.0] } else { *p })
            .collect::<Vec<_>>()
            .into();
        let select = || {
            let mut picks = AutoFrames::new(range, 6);
            picks.add_gaps(&a);
            picks.add_gaps(&b);
            compare(&a, &b, Some(&mut picks), 1).unwrap();
            picks.finish()
        };
        let frames = select();
        assert_eq!(frames, select());
        assert_eq!(
            frames
                .iter()
                .filter(|p| p.reason == FrameReason::GapEdge)
                .map(|p| p.frame)
                .collect::<Vec<_>>(),
            vec![102, 103, 105, 106]
        );
        assert!(frames.contains(&PickedFrame {
            frame: 109,
            reason: FrameReason::LargestDistance
        }));
        assert_eq!(frames.len(), 6);
        let f = frame_coverage(&a, &[102, 103]);
        assert_eq!((f[0].template_present, f[1].template_present), (1, 0));
        assert_eq!(f[1].missing_template, vec!["A"]);
    }

    #[test]
    fn auto_selects_stretch_start_middle_end_within_shared_range() {
        let a = take(vec![marker(
            1,
            "A",
            Template,
            &[(102, 0.0), (103, 0.0), (104, 0.0), (105, 0.0), (106, 0.0)],
        )]);
        let b = take(vec![]);
        let mut picks = AutoFrames::new(
            FrameRange {
                first: 103,
                last: 110,
            },
            3,
        );
        compare(&a, &b, Some(&mut picks), 1).unwrap();
        assert_eq!(
            picks.finish(),
            vec![
                PickedFrame {
                    frame: 103,
                    reason: FrameReason::PresenceStart
                },
                PickedFrame {
                    frame: 104,
                    reason: FrameReason::PresenceMiddle
                },
                PickedFrame {
                    frame: 106,
                    reason: FrameReason::PresenceEnd
                },
            ]
        );
    }

    #[test]
    fn duplicate_uid_is_ambiguous() {
        let a = take(vec![marker(2, "A", Template, &[])]);
        let b = take(vec![
            marker(2, "B", Template, &[]),
            marker(2, "C", Template, &[]),
        ]);
        let err = compare(&a, &b, None, 10).unwrap_err();
        assert!(!err.in_reference);
        assert_eq!(err.uid, [2; 16]);
    }

    #[test]
    fn thresholds_are_strict_and_max_ties_pick_the_first_frame() {
        let mut stats = SampleStats::default();
        for (frame, mm) in [(4, 21.0), (2, 21.0), (3, 20.0), (1, 1.0)] {
            stats.distance(frame, mm);
        }
        stats.finish();
        assert_eq!((stats.above_1_mm, stats.above_20_mm), (3, 2));
        assert_eq!(stats.max, Some(Peak { frame: 2, mm: 21.0 }));
        assert_eq!(stats.mean_mm, Some(15.75));
    }

    #[test]
    fn presence_matches_capture_and_nonfinite_distances_have_no_value() {
        let a = take(vec![marker(
            1,
            "A",
            Template,
            &[(100, f32::NAN), (101, f32::INFINITY)],
        )]);
        let b = take(vec![marker(1, "A", Template, &[(100, 0.0), (101, 0.0)])]);
        let stats = compare(&a, &b, None, 10).unwrap().marker_set_totals;
        assert_eq!(
            (stats.other_only, stats.both_present, stats.compared_frames),
            (1, 1, 0)
        );
        assert_eq!((stats.max, stats.mean_mm), (None, None));
    }
}
