//! Pair skeleton templates and compare the positive primary slots they declare.

use std::collections::BTreeMap;

use serde::Serialize;

use super::{sample_stats, AutoFrames, SampleStats};
use crate::tak::{uid_hex, MarkerKind, SkeletonAsset, Take, Trajectory, Uid};

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SlotComparison {
    Compared(SlotResults),
    Incomplete(SlotResults),
    Skipped { reason: SkipReason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    DifferentTemplate,
    UndeterminedTemplate,
    AmbiguousAssets,
    SlotOutsideTemplate,
}

impl SkipReason {
    pub fn text(self) -> &'static str {
        match self {
            Self::DifferentTemplate => "두 파일의 틀이 달라요.",
            Self::UndeterminedTemplate => "공통 틀을 확인할 수 없어요.",
            Self::AmbiguousAssets => "어느 스켈레톤끼리 짝지을지 정할 수 없어요.",
            Self::SlotOutsideTemplate => "틀에 없는 위치가 기록돼 있어요.",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SlotResults {
    pub assets: Vec<SlotAsset>,
    pub slots: Vec<SlotStats>,
    pub totals: SampleStats,
    pub top_distance_slots: Vec<SlotId>,
    pub name_differences: Vec<SlotNameDifference>,
}

#[derive(Debug, Serialize)]
pub struct SlotAsset {
    pub template_name: String,
    pub template_size: usize,
    pub reference_asset_uid_hex: String,
    pub other_asset_uid_hex: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SlotId {
    pub asset_index: usize,
    pub slot: i32,
}

#[derive(Debug, Serialize)]
pub struct SlotStats {
    #[serde(flatten)]
    pub id: SlotId,
    pub reference_name: String,
    pub other_name: String,
    pub reference_uids: Vec<String>,
    pub other_uids: Vec<String>,
    pub samples: Option<SampleStats>,
}

#[derive(Debug, Serialize)]
pub struct SlotNameDifference {
    #[serde(flatten)]
    pub id: SlotId,
    pub reference_name: String,
    pub other_name: String,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct TemplateKey<'a> {
    name: &'a str,
    code: u32,
    hierarchy: Vec<(u32, u32)>,
    slot_joints: Vec<(i32, u32)>,
}

struct Template<'a> {
    uid: Uid,
    key: TemplateKey<'a>,
    slot_names: BTreeMap<i32, &'a str>,
}

impl<'a> Template<'a> {
    fn from_asset(asset: &'a SkeletonAsset) -> Option<Self> {
        let name = asset.markerset_name.as_deref()?.trim();
        let model = asset
            .labeling_model()
            .or(asset.slot_comparison_model.as_ref())?;
        let mut hierarchy: Vec<_> = model.joints.iter().map(|j| (j.id, j.parent)).collect();
        hierarchy.sort_unstable();
        let mut slot_joints = Vec::new();
        let mut slot_names = BTreeMap::new();
        for c in model.constraints.iter().filter(|c| c.primary && c.slot > 0) {
            slot_joints.push((c.slot, c.joint_id));
            if slot_names.insert(c.slot, c.name.as_str()).is_some() {
                return None;
            }
        }
        if name.is_empty() || slot_joints.is_empty() {
            return None;
        }
        slot_joints.sort_unstable();
        let key = TemplateKey {
            name,
            code: model.template_code,
            hierarchy,
            slot_joints,
        };
        Some(Self {
            uid: asset.uid,
            key,
            slot_names,
        })
    }
}

fn pair_assets(left: &[Template], right: &[Template]) -> Result<Vec<(usize, usize)>, SkipReason> {
    if left.len() != right.len() {
        return Err(SkipReason::DifferentTemplate);
    }
    let left_by_uid: BTreeMap<_, _> = left.iter().enumerate().map(|(i, a)| (a.uid, i)).collect();
    let mut right_by_uid: BTreeMap<_, _> =
        right.iter().enumerate().map(|(i, a)| (a.uid, i)).collect();
    if left_by_uid.len() != left.len() || right_by_uid.len() != right.len() {
        return Err(SkipReason::AmbiguousAssets);
    }
    let mut pairs = Vec::with_capacity(left.len());
    let mut remaining_left = Vec::new();
    for (uid, i) in left_by_uid {
        match right_by_uid.remove(&uid) {
            Some(j) if left[i].key == right[j].key => pairs.push((i, j)),
            Some(_) => return Err(SkipReason::DifferentTemplate),
            None => remaining_left.push(i),
        }
    }

    let mut remaining_right = BTreeMap::<_, Vec<_>>::new();
    for j in right_by_uid.into_values() {
        remaining_right.entry(&right[j].key).or_default().push(j);
    }
    for i in remaining_left {
        let matches = remaining_right
            .remove(&left[i].key)
            .ok_or(SkipReason::DifferentTemplate)?;
        let [j] = matches.as_slice() else {
            return Err(SkipReason::AmbiguousAssets);
        };
        pairs.push((i, *j));
    }
    pairs.sort_unstable_by_key(|&(i, _)| left[i].uid);
    Ok(pairs)
}

type RecordedSlots<'a> = BTreeMap<(Uid, i32), Vec<&'a Trajectory>>;

fn recorded_slots<'a>(
    take: &'a Take,
    templates: &[Template],
) -> Result<RecordedSlots<'a>, SkipReason> {
    let assets: BTreeMap<_, _> = templates.iter().map(|a| (a.uid, &a.slot_names)).collect();
    let mut map = RecordedSlots::new();
    for track in take
        .trajectories
        .iter()
        .filter(|t| t.kind == MarkerKind::Template)
    {
        let (Some(uid), Some(slot)) = (track.parent_uid, track.slot) else {
            return Err(SkipReason::SlotOutsideTemplate);
        };
        if !assets
            .get(&uid)
            .is_some_and(|slots| slots.contains_key(&slot))
        {
            return Err(SkipReason::SlotOutsideTemplate);
        }
        map.entry((uid, slot)).or_default().push(track);
    }
    Ok(map)
}

fn single_track<'a>(tracks: &[&'a Trajectory]) -> Option<&'a Trajectory> {
    match tracks {
        [track] => Some(*track),
        _ => None,
    }
}

fn templates(assets: &[SkeletonAsset]) -> Result<Vec<Template<'_>>, SkipReason> {
    if assets.is_empty() {
        return Err(SkipReason::UndeterminedTemplate);
    }
    assets
        .iter()
        .map(Template::from_asset)
        .collect::<Option<Vec<_>>>()
        .ok_or(SkipReason::UndeterminedTemplate)
}

pub(super) fn compare(
    reference: &Take,
    other: &Take,
    scales: [f32; 2],
    auto: Option<&mut AutoFrames>,
    top: usize,
) -> SlotComparison {
    match compare_slots(reference, other, scales, auto, top) {
        Ok(result) if result.slots.iter().any(|row| row.samples.is_none()) => {
            SlotComparison::Incomplete(result)
        }
        Ok(result) => SlotComparison::Compared(result),
        Err(reason) => SlotComparison::Skipped { reason },
    }
}

fn compare_slots(
    reference: &Take,
    other: &Take,
    scales: [f32; 2],
    mut auto: Option<&mut AutoFrames>,
    top: usize,
) -> Result<SlotResults, SkipReason> {
    let left = templates(&reference.assets)?;
    let right = templates(&other.assets)?;
    let pairs = pair_assets(&left, &right)?;
    let ref_tracks = recorded_slots(reference, &left)?;
    let other_tracks = recorded_slots(other, &right)?;
    let mut results = SlotResults {
        assets: Vec::new(),
        slots: Vec::new(),
        totals: SampleStats::default(),
        top_distance_slots: Vec::new(),
        name_differences: Vec::new(),
    };
    for (asset_index, (i, j)) in pairs.into_iter().enumerate() {
        let (a, b) = (&left[i], &right[j]);
        results.assets.push(SlotAsset {
            template_name: a.key.name.to_owned(),
            template_size: a.slot_names.len(),
            reference_asset_uid_hex: uid_hex(&a.uid),
            other_asset_uid_hex: uid_hex(&b.uid),
        });
        for (&slot, &declared_name) in &a.slot_names {
            let at = ref_tracks
                .get(&(a.uid, slot))
                .map_or(&[][..], Vec::as_slice);
            let bt = other_tracks
                .get(&(b.uid, slot))
                .map_or(&[][..], Vec::as_slice);
            let (first, second) = (single_track(at), single_track(bt));
            let reference_name = first.map_or(declared_name, |t| &t.name);
            let other_name = second.map_or(b.slot_names[&slot], |t| &t.name);
            let id = SlotId { asset_index, slot };
            let samples = if at.len() <= 1 && bt.len() <= 1 {
                if reference_name != other_name {
                    results.name_differences.push(SlotNameDifference {
                        id,
                        reference_name: reference_name.to_owned(),
                        other_name: other_name.to_owned(),
                    });
                }
                let stats = sample_stats(first, second, scales, auto.as_deref_mut());
                results.totals.add(&stats);
                Some(stats)
            } else {
                None
            };
            results.slots.push(SlotStats {
                id,
                reference_name: reference_name.to_owned(),
                other_name: other_name.to_owned(),
                reference_uids: at.iter().map(|t| uid_hex(&t.uid)).collect(),
                other_uids: bt.iter().map(|t| uid_hex(&t.uid)).collect(),
                samples,
            });
        }
    }
    results.totals.finish();
    let peak = |row: &SlotStats| row.samples.as_ref().and_then(|s| s.max);
    results.slots.sort_unstable_by(|a, b| {
        let mm = |row| peak(row).map_or(-1.0, |p| p.mm);
        mm(b).total_cmp(&mm(a)).then_with(|| a.id.cmp(&b.id))
    });
    results.top_distance_slots = results
        .slots
        .iter()
        .filter(|row| peak(row).is_some())
        .take(top)
        .map(|row| row.id)
        .collect();
    Ok(results)
}
