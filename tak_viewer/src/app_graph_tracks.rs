use super::{graph::tracks, Action, TakViewerApp, View};
use eframe::egui;

impl TakViewerApp {
    pub(super) fn graph_tracks_tab(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let view = &mut self.views[self.active];
        let data = view.data.as_ref()?;
        let input_enabled = self.hold.is_none() && self.comment_edit.is_none();
        let interaction = ui
            .add_enabled_ui(input_enabled, |ui| {
                tracks::show(
                    ui,
                    &mut view.tracks,
                    data,
                    view.file_id,
                    view.pb.frame,
                    &view.selected,
                    view.primary,
                    &mut view.range,
                    &view.comments.list,
                    view.picked,
                )
            })
            .inner;
        if let Some(click) = interaction.click {
            let current = view.ordered_selection();
            if let Some(selection) = view.tracks.select(data, &current, click) {
                select_rows(view, selection);
            }
        }
        interaction.seek.map(Action::GotoFrame)
    }

    pub(crate) fn apply_graph_probe(&mut self, ctx: &egui::Context) {
        let Some(probe) = tracks::PROBE.get() else {
            return;
        };
        self.settings.graph_layout = probe.layout;
        self.set_graph_float(false);
        if let (Some(zoom), Some(data)) = (probe.zoom, &self.view().data) {
            tracks::set_zoom(ctx, self.view().file_id, zoom, data);
        }
    }
}

fn select_rows(view: &mut View, selected: Vec<usize>) {
    view.clear_selection();
    view.primary = selected.last().copied();
    view.selected = selected.iter().copied().collect();
    view.selection_order = selected;
    view.labels.scroll_to = view.primary;
}
