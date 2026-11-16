//! Comment selection is per view/file. Ranks preserve recency; a range has one rank,
//! so its representative fallback follows the current sorted order.
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Eq)]
pub struct CommentSelection {
    ranks: BTreeMap<u64, u64>,
    pub anchor: Option<u64>,
    pub representative: Option<u64>,
    clock: u64,
    order: Vec<u64>,
    range_base: Option<BTreeMap<u64, u64>>,
    range_additive: Option<bool>,
}

// Sort order is fallback metadata, not a separate selection gesture in undo history.
impl PartialEq for CommentSelection {
    fn eq(&self, other: &Self) -> bool {
        self.ranks == other.ranks && self.anchor == other.anchor
            && self.representative == other.representative && self.clock == other.clock
            && self.range_base == other.range_base && self.range_additive == other.range_additive
    }
}

impl CommentSelection {
    pub fn set_order(&mut self, order: &[u64]) { self.order = order.to_vec(); }
    pub fn prune(&mut self, live: &[u64]) {
        let live_set: std::collections::BTreeSet<_> = live.iter().copied().collect();
        let mut seen = std::collections::BTreeSet::new();
        let mut order: Vec<_> = self.order.iter().copied()
            .filter(|id| live_set.contains(id) && seen.insert(*id)).collect();
        for id in live { if seen.insert(*id) { order.push(*id); } }
        let gone: Vec<_> = self.ids().filter(|id| !live_set.contains(id)).collect();
        for id in gone { self.remove(id, &order); }
        self.order = order;
    }
    pub fn len(&self) -> usize { self.ranks.len() }
    pub fn contains(&self, id: u64) -> bool { self.ranks.contains_key(&id) }
    pub fn ids(&self) -> impl Iterator<Item = u64> + '_ { self.ranks.keys().copied() }
    pub fn clear(&mut self) { *self = Self::default(); }
    pub fn single(&mut self, id: u64) {
        self.clear();
        self.ranks.insert(id, 1);
        self.clock = 1;
        self.anchor = Some(id);
        self.representative = Some(id);
    }
    fn fallback(&self, order: &[u64]) -> Option<u64> {
        let newest = self.ranks.values().copied().max()?;
        order.iter().copied().find(|id| self.ranks.get(id) == Some(&newest))
            .or_else(|| self.ranks.keys().copied().next())
    }
    pub fn remove(&mut self, id: u64, order: &[u64]) {
        self.ranks.remove(&id);
        if self.representative == Some(id) { self.representative = self.fallback(order); }
        if self.anchor == Some(id) { self.anchor = self.representative; }
        self.range_base = None;
        self.range_additive = None;
    }
    pub fn click(&mut self, id: u64, ctrl: bool, shift: bool, order: &[u64]) {
        self.set_order(order);
        if !order.contains(&id) { return; }
        if shift {
            if let Some((a, b)) = self.anchor.and_then(|anchor| order.iter().position(|x| *x == anchor))
                .zip(order.iter().position(|x| *x == id)) {
                if self.range_additive != Some(ctrl) { self.range_base = None; }
                self.range_additive = Some(ctrl);
                let base = self.range_base.get_or_insert_with(|| if ctrl { self.ranks.clone() } else { BTreeMap::new() }).clone();
                self.clock += 1;
                self.ranks = base;
                for item in &order[a.min(b)..=a.max(b)] { self.ranks.insert(*item, self.clock); }
                self.representative = Some(id);
                return;
            }
        }
        self.range_base = None;
        self.range_additive = None;
        if ctrl {
            if self.contains(id) { self.remove(id, order); }
            else {
                self.clock += 1;
                self.ranks.insert(id, self.clock);
                self.anchor = Some(id);
                self.representative = Some(id);
            }
        } else { self.single(id); self.set_order(order); }
    }
    pub fn all(&mut self, order: &[u64]) {
        self.set_order(order);
        self.range_base = None;
        self.range_additive = None;
        self.clock += 1;
        for id in order { self.ranks.entry(*id).or_insert(self.clock); }
        self.ranks.retain(|id, _| order.contains(id));
        if !self.representative.is_some_and(|id| self.contains(id)) {
            self.representative = order.first().copied();
            self.anchor = self.representative;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn range_uses_sort_and_preserves_anchor_and_shrinks() {
        let order = [7, 2, 9, 1, 4];
        let mut s = CommentSelection::default();
        s.click(2, false, false, &order);
        s.click(4, false, true, &order);
        assert_eq!(s.ids().collect::<Vec<_>>(), vec![1, 2, 4, 9]);
        s.click(9, false, true, &order);
        assert_eq!(s.ids().collect::<Vec<_>>(), vec![2, 9]);
        assert_eq!((s.anchor, s.representative), (Some(2), Some(9)));
        s.click(7, true, false, &order);
        s.click(4, true, true, &order);
        s.click(2, true, true, &order);
        assert_eq!(s.ids().collect::<Vec<_>>(), vec![2, 7, 9]);
    }
    #[test]
    fn toggle_fallback_is_recency_then_current_sort_order() {
        let order = [5, 4, 3, 2, 1];
        let mut s = CommentSelection::default();
        s.click(4, false, false, &order);
        s.click(1, false, true, &order);
        s.click(1, true, false, &order);
        assert_eq!((s.representative, s.anchor), (Some(4), Some(4)));
        s.click(5, true, false, &order);
        s.click(5, true, false, &order);
        assert_eq!(s.representative, Some(4));
        s.all(&order);
        assert_eq!(s.representative, Some(4));
        for id in order { s.remove(id, &order); }
        assert_eq!((s.len(), s.representative, s.anchor), (0, None, None));
    }
    #[test]
    fn first_shift_and_all_choose_sort_first_without_a_representative() {
        let mut s = CommentSelection::default();
        s.click(3, false, true, &[3, 2, 1]);
        assert_eq!(s.ids().collect::<Vec<_>>(), vec![3]);
        s.clear();
        s.all(&[3, 2, 1]);
        assert_eq!(s.representative, Some(3));
    }
}
