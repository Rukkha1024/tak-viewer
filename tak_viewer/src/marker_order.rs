//! Order of rows in the Labeling pane's "Labeled" list (v0.72.0).
//!
//! Default = the order the file declares its labels (Skeleton asset constraints, then
//! LabelGroup labels), which is the order Motive lists them. A person can put names first in
//! `View → Settings… → Labeled marker order`; every other name keeps the default order after
//! them. Names only sort rows on screen. Marker kind still comes from parent + slot.

use crate::ui::labeling::LabelGroup;
use std::collections::HashMap;
use tak_core::edit::LabelKey;
use tak_core::tak::Take;

/// Labels in the order the file declares them, one rank each.
fn declared_rank(take: &Take) -> HashMap<LabelKey, usize> {
    let skeleton = take.assets.iter().flat_map(|asset| {
        asset.labeling_model().into_iter().flat_map(move |model| {
            model.constraints.iter().filter(|c| c.primary)
                .map(move |c| LabelKey { parent: asset.uid, slot: c.slot })
        })
    });
    let groups = take.label_groups.iter()
        .flat_map(|group| group.labels.iter().map(move |(_, slot)| LabelKey { parent: group.uid, slot: *slot as i32 }));
    let mut rank = HashMap::new();
    for key in skeleton.chain(groups) {
        let next = rank.len();
        rank.entry(key).or_insert(next);
    }
    rank
}

/// Clean a saved order: trimmed, no blanks, each name once (ignoring case).
pub fn clean(order: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    order.retain_mut(|name| {
        *name = name.trim().to_owned();
        !name.is_empty() && seen.insert(name.to_lowercase())
    });
}

/// Sort each group's rows: names in `custom` first (in that order), then the file order.
/// Rows the file does not declare keep their place after the declared ones (stable sort).
pub fn order_label_rows(groups: &mut [LabelGroup], take: &Take, custom: &[String]) {
    let rank = declared_rank(take);
    let custom: HashMap<String, usize> = custom.iter().enumerate()
        .map(|(i, name)| (name.trim().to_lowercase(), i)).rev().collect();
    for group in groups {
        group.labels.sort_by_cached_key(|row| (
            custom.get(&row.name.to_lowercase()).copied().unwrap_or(usize::MAX),
            row.key.and_then(|key| rank.get(&key).copied()).unwrap_or(usize::MAX),
        ));
    }
}

/// Names a person can add to the custom order: labels of the file in the default list order
/// (set markers, then extra markers, then LabelGroup labels).
pub fn declared_names(take: &Take) -> Vec<String> {
    let mut names = Vec::new();
    for asset in &take.assets {
        if let Some(model) = asset.labeling_model() {
            let primary: Vec<_> = model.constraints.iter().filter(|c| c.primary).collect();
            names.extend(primary.iter().filter(|c| c.slot > 0).map(|c| c.name.clone()));
            names.extend(primary.iter().filter(|c| c.slot <= 0).map(|c| c.name.clone()));
        }
    }
    for group in &take.label_groups {
        names.extend(group.labels.iter().map(|(name, _)| name.clone()));
    }
    clean(&mut names);
    names
}
