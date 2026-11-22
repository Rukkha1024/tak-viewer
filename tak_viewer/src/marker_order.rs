//! Order of rows in the Labeling pane's "Labeled" list.
//!
//! Default = the order the file declares its labels (Skeleton asset constraints, then
//! LabelGroup labels), which is the order Motive lists them. A person can put names first in
//! Labeling → Reorder for a marker set, extras, or a LabelGroup. Remaining names keep
//! file order. Names only sort rows on screen. Marker kind comes from parent + slot.

use crate::app::Settings;
use crate::ui::labeling::{LabelGroup, LabelRow};
use std::collections::{BTreeMap, HashMap, HashSet};
use tak_core::edit::LabelKey;
use tak_core::tak::{SkeletonAsset, Take, Uid};
use tak_core::view_data::{GroupKind, ViewData};

pub const EXTRA: &str = "extra";

fn set_scope(asset: &SkeletonAsset) -> String {
    format!("set:{}", asset.markerset_name.as_deref().filter(|name| !name.is_empty()).unwrap_or(&asset.name))
}

fn label_scope(name: &str) -> String {
    format!("labels:{name}")
}

pub fn group_scope(take: &Take, kind: GroupKind, parent: Option<Uid>) -> Option<String> {
    match kind {
        GroupKind::AssetTemplate => take.assets.iter().find(|asset| Some(asset.uid) == parent).map(set_scope),
        GroupKind::AssetExtra => Some(EXTRA.into()),
        GroupKind::LabelGroup => take.label_groups.iter().find(|group| Some(group.uid) == parent).map(|group| label_scope(&group.name)),
        _ => None,
    }
}

pub fn saved_scope<'a>(orders: &'a BTreeMap<String, Vec<String>>, scope: &str) -> Option<&'a String> {
    orders.get_key_value(scope).map(|(key, _)| key).or_else(|| {
        let folded = scope.to_lowercase();
        orders.keys().find(|key| key.to_lowercase() == folded)
    })
}

pub fn effective_order<'a>(orders: &'a BTreeMap<String, Vec<String>>, legacy: &'a [String], scope: &str) -> &'a [String] {
    saved_scope(orders, scope).map_or(legacy, |key| orders[key].as_slice())
}

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
    let mut seen = HashSet::new();
    order.retain_mut(|name| {
        *name = name.trim().to_owned();
        !name.is_empty() && seen.insert(name_key(name))
    });
}

fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

fn name_ranks(order: &[String]) -> HashMap<String, usize> {
    order.iter().enumerate().rev().map(|(i, name)| (name_key(name), i)).collect()
}

/// Sort each group's rows: names in `custom` first (in that order), then the file order.
/// Rows the file does not declare keep their place after the declared ones (stable sort).
pub fn order_label_rows(groups: &mut [LabelGroup], take: &Take, settings: &Settings) {
    let rank = declared_rank(take);
    for group in groups {
        group.labels.sort_by_key(|row| row.key.and_then(|key| rank.get(&key).copied()).unwrap_or(usize::MAX));
        for (index, row) in group.labels.iter_mut().enumerate() {
            row.file_index = index;
        }
        let Some(scope) = &group.order_scope else { continue };
        let order = settings.marker_order_for(scope);
        if order.is_empty() { continue; }
        let custom = name_ranks(order);
        group.labels.sort_by_cached_key(|row| custom.get(&name_key(&row.name)).copied().unwrap_or(usize::MAX));
        mark_moved(&mut group.labels);
    }
}

/// Rows outside a longest increasing subsequence must move to make the current order.
/// Keep the smallest file-order tail for each length to handle ties deterministically.
pub fn mark_moved(rows: &mut [LabelRow]) {
    let mut tails = Vec::<usize>::new();
    let mut previous = vec![None; rows.len()];
    for (index, row) in rows.iter().enumerate() {
        let length = tails.partition_point(|&tail| rows[tail].file_index < row.file_index);
        previous[index] = length.checked_sub(1).map(|i| tails[i]);
        if length == tails.len() {
            tails.push(index);
        } else {
            tails[length] = index;
        }
    }
    for row in rows.iter_mut() {
        row.moved = true;
    }
    let mut tail = tails.pop();
    while let Some(index) = tail {
        rows[index].moved = false;
        tail = previous[index];
    }
}

/// Replace the group's names in the scope's saved slots, then append remaining names.
/// The drop boundary counts rows in the full group, including any hidden by search.
pub fn drop_order(order: &[String], group: &LabelGroup, source: usize, boundary: usize) -> Option<Vec<String>> {
    let from = group.labels.iter().position(|row| row.file_index == source)?;
    if boundary > group.labels.len() || boundary == from || boundary == from + 1 {
        return None;
    }
    let mut rows: Vec<_> = group.labels.iter().collect();
    let row = rows.remove(from);
    rows.insert(boundary - usize::from(from < boundary), row);

    let mut names: Vec<_> = rows.iter().map(|row| row.name.clone()).collect();
    clean(&mut names);
    let ranks = name_ranks(&names);
    let row_rank = |row: &LabelRow| {
        (ranks.get(&name_key(&row.name)).copied().unwrap_or(usize::MAX), row.file_index)
    };
    // Two rows with the same name share a rank, with file order breaking the tie.
    if rows.windows(2).any(|w| row_rank(w[0]) > row_rank(w[1])) {
        return None;
    }
    let mut names = names.into_iter();
    let mut next = Vec::with_capacity(order.len().max(group.labels.len()));
    for name in order {
        if ranks.contains_key(&name_key(name)) {
            next.extend(names.next());
        } else {
            next.push(name.clone());
        }
    }
    next.extend(names);
    Some(next)
}

pub fn summary(groups: &[LabelGroup], settings: &Settings) -> Vec<String> {
    let mut scopes = Vec::<(&str, HashSet<String>)>::new();
    let mut positions = HashMap::new();
    for group in groups {
        let Some(scope) = group.order_scope.as_deref() else { continue };
        let index = *positions.entry(scope.to_lowercase()).or_insert_with(|| {
            scopes.push((scope, HashSet::new()));
            scopes.len() - 1
        });
        scopes[index].1.extend(group.labels.iter().filter(|row| row.moved).map(|row| name_key(&row.name)));
    }
    scopes.into_iter().map(|(scope, moved)| {
        let name = scope.split_once(':').map_or("Extra", |(_, name)| name);
        if settings.marker_order_for(scope).is_empty() {
            format!("{name}: file order")
        } else {
            let suffix = if moved.len() == 1 { "name" } else { "names" };
            format!("{name}: {} moved {suffix}", moved.len())
        }
    }).collect()
}

/// Apply the agent screenshot's temporary order only where a requested name is present.
pub fn preview(groups: &[LabelGroup], names: &[String]) -> BTreeMap<String, Vec<String>> {
    let ranks = name_ranks(names);
    groups.iter().filter_map(|group| {
        let scope = group.order_scope.as_ref()?;
        group.labels.iter().any(|row| ranks.contains_key(&name_key(&row.name)))
            .then(|| (scope.clone(), names.to_vec()))
    }).collect()
}

pub fn append_missing(order: &mut Vec<String>, names: Vec<String>) {
    let mut seen: HashSet<_> = order.iter().map(|name| name_key(name)).collect();
    order.extend(names.into_iter().filter(|name| seen.insert(name_key(name))));
}

/// Append declared names first, then named tracks with no declaration, within each scope.
pub fn collect_names(data: &ViewData, catalog: &mut BTreeMap<String, Vec<String>>) {
    let mut add = |scope: String, mut names: Vec<String>| {
        clean(&mut names);
        if names.is_empty() { return; }
        let key = saved_scope(catalog, &scope).cloned().unwrap_or(scope);
        append_missing(catalog.entry(key).or_default(), names);
    };
    for asset in &data.take.assets {
        if let Some(model) = asset.labeling_model() {
            let primary = || model.constraints.iter().filter(|c| c.primary);
            add(set_scope(asset), primary().filter(|c| c.slot > 0).map(|c| c.name.clone()).collect());
            add(EXTRA.into(), primary().filter(|c| c.slot <= 0).map(|c| c.name.clone()).collect());
        }
    }
    for group in &data.take.label_groups {
        add(label_scope(&group.name), group.labels.iter().map(|(name, _)| name.clone()).collect());
    }
    for group in &data.groups {
        let parent = group.asset.map(|i| data.take.assets[i].uid)
            .or_else(|| group.members.first().and_then(|&i| data.markers[i].parent));
        if let Some(scope) = group_scope(&data.take, group.kind, parent) {
            add(scope, group.members.iter().map(|&i| data.markers[i].name.clone()).collect());
        }
    }
}
