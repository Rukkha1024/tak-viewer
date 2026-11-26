//! Whole-layout tabs (approved feedback 16–18 in the v2 UI feedback).
//!
//! This module owns names, order, selection and the mixed close history. `L` is a whole
//! 3D layout; `V` is one closed view. Neither needs to be cloned. The caller owns loading,
//! dirty-file confirmation, global shortcuts, persistence and swapping the active layout.
//! `TabStrip` only emits actions, so clicking a close button cannot bypass confirmation.

use eframe::egui::{self, Color32, Key, Modifiers, PointerButton, Rect, Sense, Stroke};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, VecDeque};

#[path = "app_tab_wheel.rs"]
mod wheel;

pub type TabId = u64;
pub const MAX_CLOSED: usize = 20;
pub const TAB_STRIP_HEIGHT: f32 = 32.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayoutTab<L> {
    pub id: TabId,
    /// A layout name, independent of its files and of the active view.
    pub name: String,
    pub layout: L,
}

#[derive(Debug)]
pub struct ClosedTab<L> {
    pub tab: LayoutTab<L>,
    /// Original position in the top-level tab strip.
    pub index: usize,
}

#[derive(Debug)]
pub enum ClosedItem<L, V> {
    Tab(ClosedTab<L>),
    View {
        tab_id: TabId,
        index: usize,
        view: V,
    },
    /// One gesture; nested runtimes remain available to ordinary undo and worker handoff.
    Batch { id: u64, items: Vec<ClosedItem<L, V>> },
}

impl<L, V> ClosedItem<L, V> {
    fn refers_to_tab(&self, id: TabId) -> bool {
        match self {
            Self::Tab(tab) => tab.tab.id == id,
            Self::View { tab_id, .. } => *tab_id == id,
            Self::Batch { items, .. } => items.iter().any(|item| item.refers_to_tab(id)),
        }
    }
    fn leaves(&self) -> Box<dyn DoubleEndedIterator<Item = &Self> + '_> {
        match self {
            Self::Batch { items, .. } => Box::new(items.iter().flat_map(Self::leaves)),
            _ => Box::new(std::iter::once(self)),
        }
    }
    fn leaves_mut(&mut self) -> Box<dyn DoubleEndedIterator<Item = &mut Self> + '_> {
        match self {
            Self::Batch { items, .. } => Box::new(items.iter_mut().flat_map(Self::leaves_mut)),
            _ => Box::new(std::iter::once(self)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameError {
    Empty,
    Duplicate,
    MissingTab,
}

impl RenameError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "탭 이름을 입력해 주세요.",
            Self::Duplicate => "같은 이름의 탭이 있어요.",
            Self::MissingTab => "이미 닫힌 탭이에요.",
        }
    }
}

/// The history is deliberately not serialized. Taking and remembering a closed item are
/// separate operations: `remember_closed` returns an eviction so large resources can be
/// released by the caller away from the drawing thread.
#[derive(Debug)]
pub struct TabModel<L, V = ()> {
    tabs: Vec<LayoutTab<L>>,
    active: Option<TabId>,
    next_id: TabId,
    closed: VecDeque<ClosedItem<L, V>>,
    recording_batch: bool,
    /// Payload changes are events. The app indexes these layouts once, never by a frame scan.
    dirty_payloads: BTreeSet<TabId>,
    payload_versions: HashMap<TabId, u64>,
    payload_clock: u64,
}

impl<L, V> Default for TabModel<L, V> {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            closed: VecDeque::new(),
            recording_batch: false,
            dirty_payloads: BTreeSet::new(),
            payload_versions: HashMap::new(),
            payload_clock: 0,
        }
    }
}

impl<L, V> TabModel<L, V> {
    pub fn new(layout: L) -> Self {
        let mut model = Self::default();
        model.add(layout);
        model
    }

    pub fn tabs(&self) -> &[LayoutTab<L>] {
        &self.tabs
    }

    /// Change layout payloads here; use the model methods for names, ids and order.
    pub fn tabs_mut(&mut self) -> &mut [LayoutTab<L>] {
        for id in self.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>() { self.invalidate_payload(id); }
        &mut self.tabs
    }

    pub fn tab(&self, id: TabId) -> Option<&LayoutTab<L>> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut LayoutTab<L>> {
        self.invalidate_payload(id);
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    fn invalidate_payload(&mut self, id: TabId) {
        self.dirty_payloads.insert(id);
        self.payload_clock = self.payload_clock.wrapping_add(1);
        self.payload_versions.insert(id, self.payload_clock);
    }

    pub(super) fn payload_version(&self, id: TabId) -> u64 {
        self.payload_versions.get(&id).copied().unwrap_or(0)
    }

    pub fn take_dirty_payloads(&mut self) -> BTreeSet<TabId> {
        std::mem::take(&mut self.dirty_payloads)
    }

    pub(super) fn dirty_payloads(&self) -> &BTreeSet<TabId> { &self.dirty_payloads }

    /// A scheduler visit updates its own pending entry before returning. Other callers must
    /// use tab_mut/tabs_mut so work transferred into an idle hidden layout cannot be missed.
    pub(super) fn work_payload_mut(&mut self, id: TabId) -> Option<&mut LayoutTab<L>> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active
    }

    pub fn active(&self) -> Option<&LayoutTab<L>> {
        self.active.and_then(|id| self.tab(id))
    }

    pub fn active_mut(&mut self) -> Option<&mut LayoutTab<L>> {
        self.active.and_then(|id| self.tab_mut(id))
    }

    pub fn activate(&mut self, id: TabId) -> bool {
        if self.active == Some(id) || self.tab(id).is_none() {
            return false;
        }
        self.active = Some(id);
        true
    }

    /// Select in visual order, wrapping at either end. Empty models stay empty.
    pub fn next(&mut self) -> Option<TabId> {
        self.step(1)
    }

    pub fn previous(&mut self) -> Option<TabId> {
        self.step(-1)
    }

    fn step(&mut self, direction: isize) -> Option<TabId> {
        if self.tabs.is_empty() {
            return None;
        }
        let current = self.tabs.iter().position(|tab| Some(tab.id) == self.active);
        let index = match current {
            Some(index) => {
                (index as isize + direction).rem_euclid(self.tabs.len() as isize) as usize
            }
            None if direction < 0 => self.tabs.len() - 1,
            None => 0,
        };
        self.active = Some(self.tabs[index].id);
        self.active
    }

    /// The caller supplies a fresh empty layout; this never copies the active one.
    pub fn add(&mut self, layout: L) -> TabId {
        let id = self.allocate_id();
        let name = self.unused_name();
        self.tabs.push(LayoutTab { id, name, layout });
        self.invalidate_payload(id);
        self.active = Some(id);
        id
    }

    /// Only live names reserve a number. `tab1`, `tab3` therefore yield `tab2`.
    pub fn unused_name(&self) -> String {
        for number in 1_u64.. {
            let candidate = format!("tab{number}");
            if !self.tabs.iter().any(|tab| tab.name == candidate) {
                return candidate;
            }
        }
        unreachable!("there cannot be u64::MAX live tabs")
    }

    fn allocate_id(&mut self) -> TabId {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            let live = self.tabs.iter().any(|tab| tab.id == id);
            // A closed view still refers to its owning tab, even after its tab is evicted.
            let remembered = self.closed.iter().any(|item| match item {
                ClosedItem::Tab(tab) => tab.tab.id == id,
                ClosedItem::View { tab_id, .. } => *tab_id == id,
                ClosedItem::Batch { items, .. } => items.iter().any(|item| item.refers_to_tab(id)),
            });
            if !live && !remembered {
                return id;
            }
        }
    }

    /// Whitespace-only is empty. Otherwise preserve spelling verbatim and compare exact
    /// names (case-sensitive); layout names have no filesystem-name restrictions.
    pub fn validate_name(&self, id: TabId, name: &str) -> Result<(), RenameError> {
        if self.tab(id).is_none() {
            return Err(RenameError::MissingTab);
        }
        if name.trim().is_empty() {
            return Err(RenameError::Empty);
        }
        if self.tabs.iter().any(|tab| tab.id != id && tab.name == name) {
            return Err(RenameError::Duplicate);
        }
        Ok(())
    }

    /// `Ok(false)` means the spelling did not change, so no undo action is needed.
    pub fn rename(&mut self, id: TabId, name: &str) -> Result<bool, RenameError> {
        self.validate_name(id, name)?;
        let tab = self.tab_mut(id).expect("validated tab exists");
        if tab.name == name {
            return Ok(false);
        }
        tab.name = name.to_owned();
        Ok(true)
    }

    /// `to_index` is the final index after moving, not the insertion gap before removal.
    pub fn move_tab(&mut self, id: TabId, to_index: usize) -> bool {
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        let to = to_index.min(self.tabs.len() - 1);
        if from == to {
            return false;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.invalidate_payload(id);
        true
    }

    /// Call after dirty-file confirmation. No payload is dropped or cloned here.
    /// The right neighbor becomes active, or the left neighbor when closing the last one.
    pub fn take_tab(&mut self, id: TabId) -> Option<ClosedTab<L>> {
        let index = self.tabs.iter().position(|tab| tab.id == id)?;
        let tab = self.tabs.remove(index);
        self.invalidate_payload(id);
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(index.min(self.tabs.len().saturating_sub(1)))
                .map(|tab| tab.id);
        }
        Some(ClosedTab { tab, index })
    }

    /// Restore the payload and its old position, selecting the restored tab. A name newly
    /// reused by another live tab gets the smallest unused default. An id collision also
    /// receives a fresh id; the returned id is the one the caller should use afterwards.
    pub fn restore_tab(&mut self, mut closed: ClosedTab<L>) -> TabId {
        if self.tab(closed.tab.id).is_some() {
            closed.tab.id = self.allocate_id();
        }
        if closed.tab.name.trim().is_empty()
            || self.tabs.iter().any(|tab| tab.name == closed.tab.name)
        {
            closed.tab.name = self.unused_name();
        }
        let id = closed.tab.id;
        self.next_id = self.next_id.max(id.saturating_add(1));
        self.tabs
            .insert(closed.index.min(self.tabs.len()), closed.tab);
        self.active = Some(id);
        self.invalidate_payload(id);
        id
    }

    /// Newest at the back. Return the oldest overflow item for off-thread disposal.
    #[must_use = "release an evicted layout/view away from the UI thread"]
    pub fn remember_closed(&mut self, item: ClosedItem<L, V>) -> Option<ClosedItem<L, V>> {
        self.closed.push_back(item);
        if !self.recording_batch && self.closed.len() > MAX_CLOSED {
            self.closed.pop_front()
        } else {
            None
        }
    }

    #[must_use = "release an evicted layout/view away from the UI thread"]
    pub fn record_closed_view(
        &mut self,
        tab_id: TabId,
        index: usize,
        view: V,
    ) -> Option<ClosedItem<L, V>> {
        self.remember_closed(ClosedItem::View {
            tab_id,
            index,
            view,
        })
    }

    /// One call takes exactly one closed view or whole tab, newest first. The app checks
    /// file limits and restores a view's containing tab/layout before consuming this item.
    pub fn pop_closed(&mut self) -> Option<ClosedItem<L, V>> {
        self.closed.pop_back()
    }

    pub fn last_closed(&self) -> Option<&ClosedItem<L, V>> {
        self.closed.back()
    }

    /// Leaf runtimes, oldest to newest. Batches remain one top-level recent entry.
    pub fn closed_items(&self) -> impl DoubleEndedIterator<Item = &ClosedItem<L, V>> {
        self.closed.iter().flat_map(ClosedItem::leaves)
    }

    pub fn closed_items_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut ClosedItem<L, V>> {
        self.closed.iter_mut().flat_map(ClosedItem::leaves_mut)
    }

    pub fn has_closed_tab(&self, id: TabId) -> bool {
        self.closed_items().any(|item| matches!(item, ClosedItem::Tab(closed) if closed.tab.id == id))
    }

    pub fn take_closed_tab(&mut self, id: TabId) -> Option<ClosedTab<L>> {
        match self.take_closed_where(|item| matches!(item, ClosedItem::Tab(closed) if closed.tab.id == id))? {
            ClosedItem::Tab(tab) => Some(tab),
            _ => unreachable!("predicate only matches tabs"),
        }
    }

    pub fn has_closed_view(&self, tab_id: TabId, index: usize) -> bool {
        self.closed_items().any(|item| matches!(item, ClosedItem::View { tab_id: owner, index: slot, .. } if *owner == tab_id && *slot == index))
    }

    pub fn take_closed_view(&mut self, tab_id: TabId, index: usize) -> Option<ClosedItem<L, V>> {
        self.take_closed_where(|item| matches!(item, ClosedItem::View { tab_id: owner, index: slot, .. } if *owner == tab_id && *slot == index))
    }

    pub fn take_closed_where(&mut self, mut matches: impl FnMut(&ClosedItem<L, V>) -> bool) -> Option<ClosedItem<L, V>> {
        fn take<L, V>(items: &mut Vec<ClosedItem<L, V>>, matches: &mut impl FnMut(&ClosedItem<L, V>) -> bool) -> Option<ClosedItem<L, V>> {
            for index in (0..items.len()).rev() {
                if matches(&items[index]) { return Some(items.remove(index)); }
                if let ClosedItem::Batch { items: nested, .. } = &mut items[index] {
                    if let Some(found) = take(nested, matches) {
                        if nested.is_empty() { items.remove(index); }
                        return Some(found);
                    }
                }
            }
            None
        }
        for index in (0..self.closed.len()).rev() {
            if matches(&self.closed[index]) { return self.closed.remove(index); }
            if let ClosedItem::Batch { items, .. } = &mut self.closed[index] {
                if let Some(found) = take(items, &mut matches) {
                    if items.is_empty() { self.closed.remove(index); }
                    return Some(found);
                }
            }
        }
        None
    }

    /// Coalesce newly closed runtimes without evicting an older recent entry mid-gesture.
    pub fn take_recent_items(&mut self) -> VecDeque<ClosedItem<L, V>> { self.recording_batch = true; std::mem::take(&mut self.closed) }
    pub fn put_recent_items(&mut self, items: VecDeque<ClosedItem<L, V>>) { self.closed = items; self.recording_batch = false; }

    pub(super) fn recent_batch_ids(&self) -> BTreeSet<u64> {
        self.closed.iter().filter_map(|item| match item { ClosedItem::Batch { id, .. } => Some(*id), _ => None }).collect()
    }

    pub fn closed_len(&self) -> usize {
        self.closed.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabAction {
    New,
    Activate(TabId),
    Close(TabId),
    Rename {
        id: TabId,
        name: String,
    },
    /// Destination is the final index, as in `TabModel::move_tab`.
    Move {
        id: TabId,
        to_index: usize,
    },
}

#[derive(Debug)]
struct RenameState {
    id: TabId,
    text: String,
    error: Option<RenameError>,
    select_all: bool,
    input_id: Option<egui::Id>,
}

/// Standalone egui 0.29 tab strip; draw in the center editor region, above the 3D layout.
/// Global Ctrl+T/Tab/Shift+Tab/Shift+T and dirty transitions belong to the caller. Suppress
/// other app keyboard shortcuts while `is_renaming()` (including the first F2 frame).
#[derive(Default, Debug)]
pub struct TabStrip {
    rename: Option<RenameState>,
    dragging: Option<TabId>,
    last_active: Option<TabId>,
    pub(super) wheel: wheel::TabWheel,
}

fn consume_plain_key(ui: &egui::Ui, key: Key) -> bool {
    // egui's consume_key permits extra Shift/Alt; app shortcuts require exact modifiers.
    ui.input_mut(|input| {
        let mut consumed = false;
        input.events.retain(|event| {
            let matches = matches!(event, egui::Event::Key { key: event_key, pressed: true, modifiers, .. } if *event_key == key && *modifiers == Modifiers::NONE);
            consumed |= matches;
            !matches
        });
        consumed
    })
}

impl TabStrip {
    pub fn is_renaming(&self) -> bool {
        self.rename.is_some()
    }

    pub fn cancel_rename(&mut self) {
        self.rename = None;
    }

    pub fn begin_rename<L, V>(&mut self, model: &TabModel<L, V>, id: TabId) -> bool {
        let Some(tab) = model.tab(id) else {
            return false;
        };
        self.rename = Some(RenameState {
            id,
            text: tab.name.clone(),
            error: None,
            select_all: true,
            input_id: None,
        });
        self.dragging = None;
        true
    }

    pub fn show<L, V>(&mut self, ui: &mut egui::Ui, model: &TabModel<L, V>, rename_shortcut: bool) -> Vec<TabAction> {
        const BG: Color32 = Color32::from_rgb(0x25, 0x25, 0x26);
        const INACTIVE: Color32 = Color32::from_rgb(0x2d, 0x2d, 0x2d);
        const ACTIVE: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x1c);
        const BLUE: Color32 = Color32::from_rgb(0x00, 0x7f, 0xd4);
        const RED: Color32 = Color32::from_rgb(0xf0, 0x5a, 0x5a);

        if self
            .rename
            .as_ref()
            .is_some_and(|rename| model.tab(rename.id).is_none())
        {
            self.rename = None;
        }
        if rename_shortcut && ui.is_enabled()
            && self.rename.is_none()
            && !ui.ctx().wants_keyboard_input()
            && !ui.memory(|memory| memory.any_popup_open())
            && consume_plain_key(ui, Key::F2)
        {
            if let Some(id) = model.active_id() {
                self.begin_rename(model, id);
            }
        }
        let cancel = ui.is_enabled() && self.rename.is_some() && consume_plain_key(ui, Key::Escape);
        let enter = ui.is_enabled() && self.rename.is_some() && consume_plain_key(ui, Key::Enter);
        if cancel {
            if let Some(rename) = self.rename.take() {
                if let Some(input_id) = rename.input_id {
                    ui.memory_mut(|memory| memory.surrender_focus(input_id));
                }
            }
        }

        let show_error = self
            .rename
            .as_ref()
            .and_then(|rename| rename.error)
            .is_some();
        let height = TAB_STRIP_HEIGHT + if show_error { 26.0 } else { 0.0 };
        let (outer, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
        let bar = Rect::from_min_size(outer.min, egui::vec2(outer.width(), TAB_STRIP_HEIGHT));
        ui.painter().rect_filled(outer, 0.0, BG);
        ui.painter().hline(
            bar.x_range(),
            bar.bottom() - 0.5,
            Stroke::new(1.0, Color32::from_gray(60)),
        );

        let mut actions = Vec::new();
        let mut context_rename = None;
        let mut confirmation = None;
        let mut field_rect = None;
        let mut drawn = Vec::with_capacity(model.tabs().len());
        let mut rename = self.rename.take();
        let scroll_to_active = self.last_active != model.active_id();
        self.last_active = model.active_id();
        // An explicit tab switch or rename start takes priority over a coasting wheel.
        if scroll_to_active || rename.as_ref().is_some_and(|field| field.select_all) {
            self.wheel.stop_gesture();
        }
        let mut bar_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar)
                .id_salt("whole_layout_tabs"),
        );
        {
            self.wheel
                .show(&mut bar_ui, bar, |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.horizontal(|ui| {
                        for tab in model.tabs() {
                            let active = model.active_id() == Some(tab.id);
                            let editing = rename.as_ref().is_some_and(|rename| rename.id == tab.id);
                            let font = egui::TextStyle::Button.resolve(ui.style());
                            let text_width = ui.fonts(|fonts| {
                                fonts
                                    .layout_no_wrap(tab.name.clone(), font.clone(), Color32::WHITE)
                                    .size()
                                    .x
                            });
                            let width = (text_width + 52.0).clamp(112.0, 260.0);
                            let (rect, chip) = ui.allocate_exact_size(
                                egui::vec2(width, TAB_STRIP_HEIGHT),
                                Sense::click(),
                            );
                            drawn.push((tab.id, rect));
                            ui.painter().rect_filled(
                                rect,
                                0.0,
                                if active { ACTIVE } else { INACTIVE },
                            );
                            ui.painter().vline(
                                rect.right() - 0.5,
                                rect.y_range(),
                                Stroke::new(1.0, Color32::from_gray(24)),
                            );
                            if active {
                                ui.painter().hline(
                                    rect.x_range(),
                                    rect.top() + 1.0,
                                    Stroke::new(2.0, BLUE),
                                );
                            }
                            // Middle-click owns the complete chip, including the name,
                            // rename field, padding and ×. Do not emit Activate first.
                            let mut middle_close = chip.clicked_by(PointerButton::Middle);
                            let close_rect = Rect::from_center_size(
                                egui::pos2(rect.right() - 16.0, rect.center().y),
                                egui::vec2(24.0, 24.0),
                            );
                            let name_rect = Rect::from_min_max(
                                rect.min + egui::vec2(10.0, 4.0),
                                egui::pos2(close_rect.left() - 3.0, rect.bottom() - 4.0),
                            );

                            if editing {
                                let rename = rename.as_mut().expect("editing tab has rename state");
                                let input_id = ui.id().with(("layout_tab_name", tab.id));
                                rename.input_id = Some(input_id);
                                let first_frame = rename.select_all;
                                let output = {
                                    let mut field_ui =
                                        ui.new_child(egui::UiBuilder::new().max_rect(name_rect));
                                    let mut output = egui::TextEdit::singleline(&mut rename.text)
                                        .id(input_id)
                                        .desired_width(name_rect.width())
                                        .margin(egui::vec2(3.0, 2.0))
                                        .show(&mut field_ui);
                                    if first_frame {
                                        output.response.request_focus();
                                        output.response.scroll_to_me(Some(egui::Align::Center));
                                        output.state.cursor.set_char_range(Some(
                                            egui::text::CCursorRange::two(
                                                egui::text::CCursor::new(0),
                                                egui::text::CCursor::new(
                                                    rename.text.chars().count(),
                                                ),
                                            ),
                                        ));
                                        output.state.store(ui.ctx(), output.response.id);
                                    }
                                    output.response
                                };
                                middle_close |= output.clicked_by(PointerButton::Middle);
                                rename.select_all = false;
                                if output.changed() {
                                    rename.error = model.validate_name(tab.id, &rename.text).err();
                                }
                                if enter || (!first_frame && output.clicked_elsewhere()) {
                                    confirmation = Some(tab.id);
                                }
                                if rename.error.is_some() {
                                    ui.painter().rect_stroke(
                                        output.rect,
                                        0.0,
                                        Stroke::new(1.0, RED),
                                    );
                                }
                                field_rect = Some(output.rect);
                            } else {
                                let hit = Rect::from_min_max(
                                    rect.min,
                                    egui::pos2(close_rect.left() - 1.0, rect.bottom()),
                                );
                                let response = ui.interact(
                                    hit,
                                    ui.id().with(("layout_tab", tab.id)),
                                    Sense::click_and_drag(),
                                );
                                if response.hovered() && !active {
                                    ui.painter().rect_filled(hit, 0.0, Color32::from_gray(51));
                                }
                                let galley = ui.fonts(|fonts| {
                                    let mut job = egui::text::LayoutJob::simple_singleline(
                                        tab.name.clone(),
                                        font,
                                        if active {
                                            Color32::WHITE
                                        } else {
                                            Color32::from_gray(160)
                                        },
                                    );
                                    job.wrap.max_width = name_rect.width();
                                    job.wrap.max_rows = 1;
                                    fonts.layout_job(job)
                                });
                                ui.painter().galley(
                                    egui::pos2(
                                        name_rect.left(),
                                        rect.center().y - galley.size().y * 0.5,
                                    ),
                                    galley,
                                    Color32::WHITE,
                                );
                                middle_close |= response.clicked_by(PointerButton::Middle);
                                if response.clicked() {
                                    actions.push(TabAction::Activate(tab.id));
                                }
                                if response.drag_started_by(PointerButton::Primary) {
                                    self.dragging = Some(tab.id);
                                }
                                response.context_menu(|ui| {
                                    if ui.button("Rename").clicked() {
                                        actions.push(TabAction::Activate(tab.id));
                                        context_rename = Some(tab.id);
                                        ui.close_menu();
                                    }
                                    if ui.button("Close Tab").clicked() {
                                        actions.push(TabAction::Close(tab.id));
                                        ui.close_menu();
                                    }
                                });
                                if active && scroll_to_active {
                                    response.scroll_to_me(Some(egui::Align::Center));
                                }
                            }

                            let close = ui.interact(
                                close_rect,
                                ui.id().with(("layout_tab_close", tab.id)),
                                Sense::click(),
                            );
                            if close.hovered() {
                                ui.painter().rect_filled(
                                    close_rect.shrink(3.0),
                                    3.0,
                                    Color32::from_gray(67),
                                );
                            }
                            let x = close_rect.center();
                            let stroke = Stroke::new(1.0, Color32::from_gray(190));
                            ui.painter().line_segment(
                                [x - egui::vec2(3.0, 3.0), x + egui::vec2(3.0, 3.0)],
                                stroke,
                            );
                            ui.painter().line_segment(
                                [x + egui::vec2(-3.0, 3.0), x + egui::vec2(3.0, -3.0)],
                                stroke,
                            );
                            if close.clicked() || close.clicked_by(PointerButton::Middle) || middle_close {
                                actions.push(TabAction::Close(tab.id));
                            }
                        }
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(34.0, TAB_STRIP_HEIGHT),
                            Sense::click(),
                        );
                        if response.hovered() {
                            ui.painter().rect_filled(rect, 0.0, INACTIVE);
                        }
                        let p = rect.center();
                        let stroke = Stroke::new(1.0, Color32::from_gray(190));
                        ui.painter().line_segment(
                            [p - egui::vec2(5.0, 0.0), p + egui::vec2(5.0, 0.0)],
                            stroke,
                        );
                        ui.painter().line_segment(
                            [p - egui::vec2(0.0, 5.0), p + egui::vec2(0.0, 5.0)],
                            stroke,
                        );
                        if response.clicked() {
                            actions.push(TabAction::New);
                        }
                    });
                });
        }

        if let Some(id) = confirmation {
            let state = rename
                .as_mut()
                .expect("confirmation comes from the name field");
            match model.validate_name(id, &state.text) {
                Ok(()) => {
                    actions.insert(
                        0,
                        TabAction::Rename {
                            id,
                            name: state.text.clone(),
                        },
                    );
                    if let Some(input_id) = state.input_id {
                        ui.memory_mut(|memory| memory.surrender_focus(input_id));
                    }
                    rename = None;
                }
                Err(error) => {
                    state.error = Some(error);
                    if let Some(input_id) = state.input_id {
                        ui.memory_mut(|memory| memory.request_focus(input_id));
                    }
                    // Invalid names remain editable; a tab click must not abandon them.
                    actions.clear();
                }
            }
        }
        if let (Some(state), Some(field)) = (&rename, field_rect) {
            if let Some(error) = state.error {
                // This uses the parent painter: the error stays under the field and is not
                // clipped by the horizontally scrolling tab row.
                let error_rect = Rect::from_min_max(
                    egui::pos2(field.left().max(outer.left()), bar.bottom()),
                    egui::pos2(outer.right(), bar.bottom() + 26.0),
                );
                if !show_error {
                    // An error may appear while typing or confirming in this frame.
                    // Reserve its line immediately so the 3D layout cannot cover it.
                    ui.advance_cursor_after_rect(outer.union(error_rect));
                }
                ui.painter().with_clip_rect(outer.union(error_rect)).text(
                    error_rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    error.message(),
                    egui::TextStyle::Small.resolve(ui.style()),
                    RED,
                );
                if !show_error {
                    ui.ctx().request_repaint();
                }
            }
        }
        self.rename = rename;
        if let Some(id) = context_rename {
            if self.rename.is_none() {
                self.begin_rename(model, id);
            }
        }

        if let Some(id) = self.dragging {
            if let Some(pos) = ui.input(|input| input.pointer.interact_pos()) {
                if bar.contains(pos) && !drawn.is_empty() {
                    let gap = drawn
                        .iter()
                        .position(|(_, rect)| pos.x < rect.center().x)
                        .unwrap_or(drawn.len());
                    let x = drawn
                        .get(gap)
                        .map_or_else(|| drawn.last().unwrap().1.right(), |(_, rect)| rect.left());
                    ui.painter().vline(
                        x.clamp(bar.left(), bar.right().max(bar.left())),
                        bar.y_range(),
                        Stroke::new(2.0, BLUE),
                    );
                    if ui.input(|input| input.pointer.button_released(PointerButton::Primary)) {
                        if let Some(from) = drawn.iter().position(|(tab_id, _)| *tab_id == id) {
                            let to_index = gap.saturating_sub(usize::from(gap > from));
                            if to_index != from {
                                actions.push(TabAction::Move { id, to_index });
                            }
                        }
                    }
                }
            }
            if !ui.input(|input| input.pointer.primary_down()) {
                self.dragging = None;
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_numbers_reuse_the_smallest_live_hole() {
        let mut model: TabModel<usize> = TabModel::new(10);
        let second = model.add(20);
        model.add(30);
        let closed = model.take_tab(second).unwrap();
        assert!(model.remember_closed(ClosedItem::Tab(closed)).is_none());
        let replacement = model.add(40);
        assert_eq!(model.tab(replacement).unwrap().name, "tab2");
        assert_ne!(
            second, replacement,
            "view history must not target a newly created tab"
        );
        assert_eq!(model.active().unwrap().layout, 40);
    }

    #[test]
    fn name_validation_is_exact_and_preserves_spelling() {
        let mut model: TabModel<()> = TabModel::new(());
        let first = model.active_id().unwrap();
        let second = model.add(());
        for empty in ["", " ", "\t\n", "\u{2003}"] {
            assert_eq!(model.rename(first, empty), Err(RenameError::Empty));
        }
        assert_eq!(model.rename(first, "tab2"), Err(RenameError::Duplicate));
        assert_eq!(model.tab(first).unwrap().name, "tab1");
        assert_eq!(model.rename(first, "tab1"), Ok(false));
        assert_eq!(model.rename(first, " Tab2 "), Ok(true));
        assert_eq!(model.tab(first).unwrap().name, " Tab2 ");
        assert_eq!(model.rename(second, "Tab2"), Ok(true));
        assert_eq!(model.rename(900, "new"), Err(RenameError::MissingTab));
    }

    #[test]
    fn cycle_and_reorder_follow_visual_order_without_changing_payloads() {
        let mut model: TabModel<_> = TabModel::new("first");
        let first = model.active_id().unwrap();
        let second = model.add("second");
        let third = model.add("third");
        assert_eq!(model.next(), Some(first));
        assert_eq!(model.previous(), Some(third));
        assert!(model.move_tab(third, 0));
        assert_eq!(model.active_id(), Some(third));
        assert_eq!(model.next(), Some(first));
        assert_eq!(model.next(), Some(second));
        assert_eq!(model.next(), Some(third));
        assert_eq!(model.tab(third).unwrap().layout, "third");
        assert!(!model.move_tab(third, 0));
        assert!(!model.activate(999));
    }

    #[test]
    fn closing_selects_neighbor_and_all_tabs_can_be_closed() {
        let mut model: TabModel<_> = TabModel::new(1);
        let first = model.active_id().unwrap();
        let second = model.add(2);
        let third = model.add(3);
        model.activate(second);
        assert_eq!(model.take_tab(second).unwrap().index, 1);
        assert_eq!(model.active_id(), Some(third));
        model.take_tab(first).unwrap();
        assert_eq!(model.active_id(), Some(third));
        model.take_tab(third).unwrap();
        assert!(model.tabs().is_empty());
        assert_eq!(model.active_id(), None);
        assert_eq!(model.next(), None);
        assert_eq!(model.previous(), None);
        assert!(model.take_tab(third).is_none());
    }

    #[test]
    fn reopen_keeps_move_only_layout_and_original_position() {
        #[derive(Debug)]
        struct NoClone(Box<[u8; 3]>);
        let mut model: TabModel<_> = TabModel::new(NoClone(Box::new([1, 2, 3])));
        let first = model.active_id().unwrap();
        let second = model.add(NoClone(Box::new([4, 5, 6])));
        let address = model.tab(first).unwrap().layout.0.as_ptr();
        let closed = model.take_tab(first).unwrap();
        assert!(model.remember_closed(ClosedItem::Tab(closed)).is_none());
        let ClosedItem::Tab(closed) = model.pop_closed().unwrap() else {
            panic!("whole tab")
        };
        assert_eq!(model.restore_tab(closed), first);
        assert_eq!(
            model.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(),
            [first, second]
        );
        assert_eq!(model.active_id(), Some(first));
        assert_eq!(model.tab(first).unwrap().layout.0.as_ptr(), address);
    }

    #[test]
    fn reopened_name_conflict_gets_a_free_number_without_losing_data() {
        let mut model: TabModel<_> = TabModel::new(12);
        let first = model.active_id().unwrap();
        let closed = model.take_tab(first).unwrap();
        let replacement = model.add(45);
        model.restore_tab(closed);
        assert_eq!(model.tab(replacement).unwrap().name, "tab1");
        assert_eq!(model.tab(first).unwrap().name, "tab2");
        assert_eq!(model.tab(first).unwrap().layout, 12);
    }

    #[test]
    fn mixed_history_is_bounded_and_reopens_exactly_one_item_newest_first() {
        let mut model: TabModel<(), usize> = TabModel::new(());
        let id = model.active_id().unwrap();
        for index in 0..MAX_CLOSED {
            assert!(model.record_closed_view(id, index, index).is_none());
        }
        let closed = model.take_tab(id).unwrap();
        let Some(ClosedItem::View { view: 0, .. }) = model.remember_closed(ClosedItem::Tab(closed))
        else {
            panic!("oldest view is returned, not dropped")
        };
        assert_eq!(model.closed_len(), MAX_CLOSED);
        assert!(matches!(model.last_closed(), Some(ClosedItem::Tab(_))));
        assert!(matches!(model.pop_closed(), Some(ClosedItem::Tab(_))));
        assert_eq!(model.closed_len(), MAX_CLOSED - 1);
        for expected in (1..MAX_CLOSED).rev() {
            let Some(ClosedItem::View {
                tab_id,
                index,
                view,
            }) = model.pop_closed()
            else {
                panic!("view")
            };
            assert_eq!((tab_id, index, view), (id, expected, expected));
        }
        assert!(model.pop_closed().is_none());
    }

    #[test]
    fn identity_takes_leave_other_close_history_in_order() {
        let mut model: TabModel<usize, &'static str> = TabModel::new(1);
        let first = model.active_id().unwrap();
        let second = model.add(2);
        assert!(model.record_closed_view(first, 0, "old").is_none());
        let closed = model.take_tab(second).unwrap();
        assert!(model.remember_closed(ClosedItem::Tab(closed)).is_none());
        assert!(model.record_closed_view(first, 0, "new").is_none());
        assert!(model.has_closed_tab(second));
        assert_eq!(model.take_closed_tab(second).unwrap().tab.layout, 2);
        assert!(!model.has_closed_tab(second));
        assert!(model.has_closed_view(first, 0));
        assert_eq!(model.closed_len(), 2);
        assert!(matches!(
            model.take_closed_where(|item| matches!(item, ClosedItem::View { view: "old", .. })),
            Some(ClosedItem::View { view: "old", .. })
        ));
        assert!(matches!(
            model.take_closed_view(first, 0),
            Some(ClosedItem::View { view: "new", .. })
        ));
        assert!(!model.has_closed_view(first, 0));
        assert!(model.closed_items().next().is_none());
    }

    #[test]
    fn overflow_returns_ownership_without_running_payload_destructors() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        #[derive(Debug)]
        struct CountDrop(Arc<AtomicUsize>);
        impl Drop for CountDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let mut model: TabModel<(), CountDrop> = TabModel::new(());
        let id = model.active_id().unwrap();
        for index in 0..MAX_CLOSED {
            assert!(model
                .record_closed_view(id, index, CountDrop(drops.clone()))
                .is_none());
        }
        let overflow = model.record_closed_view(id, MAX_CLOSED, CountDrop(drops.clone()));
        assert!(overflow.is_some());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(overflow);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    fn key(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    fn draw<L, V>(
        ctx: &egui::Context,
        strip: &mut TabStrip,
        model: &TabModel<L, V>,
        events: Vec<egui::Event>,
    ) -> (Vec<TabAction>, egui::FullOutput) {
        let mut actions = Vec::new();
        let out = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 400.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    actions = strip.show(ui, model, true);
                    ui.label("below tabs");
                });
            },
        );
        (actions, out)
    }

    fn pointer(pos: egui::Pos2, button: PointerButton, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn f2_selects_the_whole_name_enter_commits_and_escape_cancels() {
        let ctx = egui::Context::default();
        let model: TabModel<()> = TabModel::new(());
        let id = model.active_id().unwrap();
        let mut strip = TabStrip::default();
        draw(&ctx, &mut strip, &model, vec![]);
        draw(&ctx, &mut strip, &model, vec![key(Key::F2)]);
        assert!(strip.is_renaming());
        draw(
            &ctx,
            &mut strip,
            &model,
            vec![egui::Event::Text("walk".into())],
        );
        assert_eq!(
            strip.rename.as_ref().unwrap().text,
            "walk",
            "typing replaces the selected full name"
        );
        let (actions, _) = draw(&ctx, &mut strip, &model, vec![key(Key::Enter)]);
        assert_eq!(
            actions,
            [TabAction::Rename {
                id,
                name: "walk".into()
            }]
        );
        assert!(!strip.is_renaming());
        strip.begin_rename(&model, id);
        draw(&ctx, &mut strip, &model, vec![]);
        draw(
            &ctx,
            &mut strip,
            &model,
            vec![egui::Event::Text("cancelled".into())],
        );
        let (actions, _) = draw(&ctx, &mut strip, &model, vec![key(Key::Escape)]);
        assert!(actions.is_empty());
        assert!(!strip.is_renaming());
        assert_eq!(
            model.active().unwrap().name,
            "tab1",
            "the UI never mutates the model"
        );
    }

    #[test]
    fn invalid_inline_name_stays_editing_and_emits_no_rename() {
        let ctx = egui::Context::default();
        let mut model: TabModel<()> = TabModel::new(());
        let id = model.add(());
        let mut strip = TabStrip::default();
        strip.begin_rename(&model, id);
        draw(&ctx, &mut strip, &model, vec![]);
        draw(
            &ctx,
            &mut strip,
            &model,
            vec![egui::Event::Text("tab1".into())],
        );
        let (actions, _) = draw(&ctx, &mut strip, &model, vec![key(Key::Enter)]);
        assert!(actions.is_empty());
        assert!(strip.is_renaming());
        assert_eq!(
            strip.rename.as_ref().unwrap().error,
            Some(RenameError::Duplicate)
        );
        strip.rename.as_mut().unwrap().text = " ".into();
        let (actions, out) = draw(&ctx, &mut strip, &model, vec![key(Key::Enter)]);
        assert!(actions.is_empty());
        assert_eq!(
            strip.rename.as_ref().unwrap().error,
            Some(RenameError::Empty)
        );
        assert!(out.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == RenameError::Empty.message())));
    }

    #[test]
    fn invalid_blur_refocuses_the_textbox_and_blocks_tab_actions() {
        let ctx = egui::Context::default();
        let mut model: TabModel<()> = TabModel::new(());
        let id = model.add(());
        let mut strip = TabStrip::default();
        strip.begin_rename(&model, id);
        draw(&ctx, &mut strip, &model, vec![]);
        draw(
            &ctx,
            &mut strip,
            &model,
            vec![egui::Event::Text("tab1".into())],
        );
        let first_tab = egui::pos2(30.0, 22.0);
        draw(
            &ctx,
            &mut strip,
            &model,
            pointer(first_tab, PointerButton::Primary, true),
        );
        let (actions, _) = draw(
            &ctx,
            &mut strip,
            &model,
            pointer(first_tab, PointerButton::Primary, false),
        );
        assert!(actions.is_empty());
        assert!(strip.is_renaming());
        let rename = strip.rename.as_ref().unwrap();
        assert_eq!(rename.error, Some(RenameError::Duplicate));
        assert_eq!(ctx.memory(|memory| memory.focused()), rename.input_id);
        let outside = egui::pos2(500.0, 200.0);
        draw(
            &ctx,
            &mut strip,
            &model,
            pointer(outside, PointerButton::Primary, true),
        );
        draw(
            &ctx,
            &mut strip,
            &model,
            pointer(outside, PointerButton::Primary, false),
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            strip.rename.as_ref().unwrap().input_id
        );
    }

    #[test]
    fn strip_emits_new_close_and_drag_move_without_mutating_layouts() {
        let ctx = egui::Context::default();
        let mut strip = TabStrip::default();
        let empty: TabModel<()> = TabModel::default();
        draw(&ctx, &mut strip, &empty, vec![]);
        let plus = egui::pos2(25.0, 22.0);
        draw(
            &ctx,
            &mut strip,
            &empty,
            pointer(plus, PointerButton::Primary, true),
        );
        assert_eq!(
            draw(
                &ctx,
                &mut strip,
                &empty,
                pointer(plus, PointerButton::Primary, false)
            )
            .0,
            [TabAction::New]
        );
        let mut model: TabModel<()> = TabModel::new(());
        let first = model.active_id().unwrap();
        model.add(());
        model.add(());
        draw(&ctx, &mut strip, &model, vec![]);
        let close = egui::pos2(104.0, 22.0);
        draw(
            &ctx,
            &mut strip,
            &model,
            pointer(close, PointerButton::Primary, true),
        );
        assert_eq!(
            draw(
                &ctx,
                &mut strip,
                &model,
                pointer(close, PointerButton::Primary, false)
            )
            .0,
            [TabAction::Close(first)]
        );
        let start = egui::pos2(30.0, 22.0);
        let end = egui::pos2(330.0, 22.0);
        draw(
            &ctx,
            &mut strip,
            &model,
            pointer(start, PointerButton::Primary, true),
        );
        draw(
            &ctx,
            &mut strip,
            &model,
            vec![egui::Event::PointerMoved(end)],
        );
        assert_eq!(
            draw(
                &ctx,
                &mut strip,
                &model,
                pointer(end, PointerButton::Primary, false)
            )
            .0,
            [TabAction::Move {
                id: first,
                to_index: 2
            }]
        );
        assert_eq!(model.tabs().len(), 3);
        assert_eq!(model.tabs()[0].id, first);
    }

    #[test]
    fn middle_click_any_chip_region_emits_only_close_for_active_or_inactive_tab() {
        for active in [true, false] {
            for pos in [egui::pos2(30.0, 22.0), egui::pos2(104.0, 22.0), egui::pos2(118.0, 9.0)] {
                let ctx = egui::Context::default();
                let mut strip = TabStrip::default();
                let mut model: TabModel<()> = TabModel::new(());
                let first = model.active_id().unwrap();
                model.add(());
                if active { model.activate(first); }
                for _ in 0..3 { draw(&ctx, &mut strip, &model, vec![]); }
                draw(&ctx, &mut strip, &model, pointer(pos, PointerButton::Middle, true));
                let actions = draw(&ctx, &mut strip, &model, pointer(pos, PointerButton::Middle, false)).0;
                assert_eq!(actions, [TabAction::Close(first)], "active={active}, pos={pos:?}");
                assert_eq!(model.tabs().len(), 2, "the caller owns the existing close path");
            }
        }
    }

    #[test]
    fn right_click_rename_activates_target_and_opens_inline_editor() {
        let ctx = egui::Context::default();
        let mut strip = TabStrip::default();
        let mut model: TabModel<()> = TabModel::new(());
        let first = model.active_id().unwrap();
        model.add(());
        draw(&ctx, &mut strip, &model, vec![]);
        let pos = egui::pos2(30.0, 22.0);
        draw(
            &ctx,
            &mut strip,
            &model,
            pointer(pos, PointerButton::Secondary, true),
        );
        draw(
            &ctx,
            &mut strip,
            &model,
            pointer(pos, PointerButton::Secondary, false),
        );
        let (_, out) = draw(&ctx, &mut strip, &model, vec![]);
        let rename = out
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "Rename" => {
                    Some(Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
            .expect("Rename context menu item is visible");
        let pos = rename.center();
        draw(
            &ctx,
            &mut strip,
            &model,
            pointer(pos, PointerButton::Primary, true),
        );
        let (actions, _) = draw(
            &ctx,
            &mut strip,
            &model,
            pointer(pos, PointerButton::Primary, false),
        );
        assert_eq!(actions, [TabAction::Activate(first)]);
        assert_eq!(strip.rename.as_ref().unwrap().id, first);
    }

    #[test]
    fn rename_error_line_reserves_space_before_the_following_layout() {
        let ctx = egui::Context::default();
        let mut model: TabModel<()> = TabModel::new(());
        let first = model.active_id().unwrap();
        model.add(());
        let mut strip = TabStrip::default();
        strip.begin_rename(&model, first);
        draw(&ctx, &mut strip, &model, vec![]);
        for events in [vec![egui::Event::Text("tab2".into())], vec![]] {
            let (_, out) = draw(&ctx, &mut strip, &model, events);
            let text_rect = |name| {
                out.shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == name => {
                            Some(Rect::from_min_size(text.pos, text.galley.size()))
                        }
                        _ => None,
                    })
                    .expect("text is drawn")
            };
            let error = text_rect(RenameError::Duplicate.message());
            let following = text_rect("below tabs");
            assert!(
                following.top() >= error.bottom(),
                "error {error:?} is above following layout {following:?}"
            );
            // Editing the first tab must not shift the second tab into its close button.
            assert!(out.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "tab2" && text.pos.x >= 120.0)));
        }
    }

    #[test]
    fn f2_requires_exact_modifiers_and_escape_releases_text_focus() {
        let ctx = egui::Context::default();
        let model: TabModel<()> = TabModel::new(());
        let mut strip = TabStrip::default();
        for modifiers in [Modifiers::SHIFT, Modifiers::ALT, Modifiers::CTRL] {
            draw(
                &ctx,
                &mut strip,
                &model,
                vec![egui::Event::Key {
                    key: Key::F2,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
            );
            assert!(!strip.is_renaming());
        }
        draw(&ctx, &mut strip, &model, vec![key(Key::F2)]);
        assert!(strip.is_renaming());
        draw(&ctx, &mut strip, &model, vec![key(Key::Escape)]);
        assert!(!strip.is_renaming());
        assert!(!ctx.wants_keyboard_input());
    }

    #[test]
    fn click_elsewhere_confirms_valid_rename_and_double_click_does_not_start_it() {
        let ctx = egui::Context::default();
        let model: TabModel<()> = TabModel::new(());
        let id = model.active_id().unwrap();
        let mut strip = TabStrip::default();
        strip.begin_rename(&model, id);
        draw(&ctx, &mut strip, &model, vec![]);
        draw(
            &ctx,
            &mut strip,
            &model,
            vec![egui::Event::Text("renamed".into())],
        );
        let outside = egui::pos2(400.0, 200.0);
        draw(
            &ctx,
            &mut strip,
            &model,
            vec![
                egui::Event::PointerMoved(outside),
                egui::Event::PointerButton {
                    pos: outside,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        let (actions, _) = draw(
            &ctx,
            &mut strip,
            &model,
            vec![egui::Event::PointerButton {
                pos: outside,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert_eq!(
            actions,
            [TabAction::Rename {
                id,
                name: "renamed".into()
            }]
        );
        for _ in 0..2 {
            let pos = egui::pos2(30.0, 22.0);
            draw(
                &ctx,
                &mut strip,
                &model,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    },
                ],
            );
            draw(
                &ctx,
                &mut strip,
                &model,
                vec![egui::Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                }],
            );
        }
        assert!(!strip.is_renaming());
    }
}
