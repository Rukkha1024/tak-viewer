use super::{theme, Lane};
use eframe::egui::{self, RichText, Ui};
use tak_core::tak::Uid;
use tak_core::view_data::ViewData;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ValueZoom([f64; 3]);

impl Default for ValueZoom {
    fn default() -> Self {
        Self([1.0; 3])
    }
}

impl ValueZoom {
    pub(crate) fn drag(&mut self, lane: usize, dy: f32) {
        if let Some(scale) = self.0.get_mut(lane).filter(|_| dy.is_finite()) {
            *scale = (*scale * (f64::from(dy) * 0.012).exp()).clamp(1.0 / 64.0, 64.0);
        }
    }

    pub(crate) fn bounds(&self, lane: usize, (lo, hi): (f64, f64)) -> (f64, f64) {
        let scale = self.0[lane];
        if scale == 1.0 || !lo.is_finite() {
            return (lo, hi);
        }
        let center = lo.midpoint(hi);
        let half = ((hi - lo) * 0.5).max(1e-6) * scale;
        (center - half, center + half)
    }

    pub(crate) fn magnify(&mut self, by: f64) {
        self.0.fill(by.recip().clamp(1.0 / 64.0, 64.0));
    }

    pub(crate) fn is_auto(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone)]
pub(crate) struct LockedSelection {
    markers: Vec<(usize, Uid)>,
    total: usize,
}

impl LockedSelection {
    pub(crate) fn new(data: &ViewData, shown: &[usize], total: usize) -> Self {
        Self { markers: shown.iter().map(|&i| (i, data.markers[i].uid)).collect(), total }
    }

    pub(crate) fn shown(
        &self, data: &std::sync::Arc<ViewData>, tracks: &mut super::tracks::State, ui: &Ui,
        file_id: usize,
    ) -> (Vec<usize>, usize) {
        let matches = |&(i, uid): &(usize, Uid)| data.markers.get(i).is_some_and(|m| m.uid == uid);
        if self.markers.iter().all(matches) {
            return (self.markers.iter().map(|&(i, _)| i).collect(), self.total);
        }
        tracks.prepare(data, file_id, ui);
        let order = tracks.order(data);
        let shown: Vec<_> = self.markers.iter().filter_map(|entry @ &(i, uid)| {
            if matches(entry) { Some(i) } else { order?.marker_by_uid.get(&uid).copied() }
        }).collect();
        let omitted = self.total - self.markers.len();
        let total = shown.len() + omitted;
        (shown, total)
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) lock: Option<LockedSelection>,
    channels: ValueZoom,
    combined: ValueZoom,
}

impl State {
    pub(crate) fn zoom(&mut self, combined: bool) -> &mut ValueZoom {
        if combined { &mut self.combined } else { &mut self.channels }
    }

    pub(crate) fn reset_zoom(&mut self) {
        self.channels = ValueZoom::default();
        self.combined = ValueZoom::default();
    }
}

/// Keep sample storage and curve colors. The painter takes one primary lane and
/// further curve sets, each contributing at the same lane index.
pub(in crate::ui) fn combine(sets: impl IntoIterator<Item = Vec<Lane>>) -> impl Iterator<Item = Vec<Lane>> {
    sets.into_iter().flat_map(|lanes| {
        let mut lanes = lanes.into_iter();
        std::array::from_fn::<_, 3, _>(|_| lanes.next().into_iter().collect())
    })
}

pub(in crate::ui) fn axis_legend(ui: &mut Ui) {
    for (axis, color) in [("X", theme::AXIS_X), ("Y", theme::AXIS_Y), ("Z", theme::AXIS_Z)] {
        ui.label(RichText::new(axis).small().strong().color(color));
    }
}

pub(crate) fn fit_button(ui: &mut Ui, has_range: bool) -> bool {
    ui.add_enabled(has_range, egui::Button::new("Zoom Fit").small())
        .on_hover_text("Fit yellow range (F) · Full take (Shift+F)")
        .clicked()
}

pub(crate) fn auto_button(ui: &mut Ui, zoom: &mut ValueZoom) {
    if ui.add_enabled(!zoom.is_auto(), egui::Button::new("Auto Extents").small())
        .on_hover_text("Reset value zoom")
        .clicked()
    {
        *zoom = ValueZoom::default();
    }
}

pub(crate) fn lock_button(ui: &mut Ui, locked: bool, available: bool) -> bool {
    ui.add_enabled(available || locked, egui::Button::new("Lock Selection").small().selected(locked))
        .on_hover_text("Pin graph markers · Click again to unpin")
        .clicked()
}

#[cfg(test)]
#[path = "graph_convenience_tests.rs"]
mod tests;
