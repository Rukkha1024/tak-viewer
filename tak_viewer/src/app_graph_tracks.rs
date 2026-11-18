use super::{graph::tracks, Action, TakViewerApp, View};
use crate::merge_keys::{self, Direction};
use eframe::egui;
use tak_core::edit::label::MergeRequest;

impl TakViewerApp {
    pub(super) fn graph_tracks_tab(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let slot = self.active;
        let selection = self.merge_keys_rows(slot, ui.ctx());
        if let Some(direction) = merge_keys::toolbar(ui, selection.as_ref().err().copied()) {
            if let Ok(rows) = selection {
                self.merge_keys(ui.ctx(), slot, rows, direction);
            }
        }
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
        self.apply_graph_convenience_probe(ctx, probe);
    }

    pub(super) fn merge_keys_capture(&self) -> bool {
        self.no_persist
            && self.auto.is_some()
            && self.view().remote.is_none()
            && merge_keys::PROBE.get().is_some()
    }

    fn merge_keys_rows(
        &self,
        slot: usize,
        ctx: &egui::Context,
    ) -> Result<[usize; 2], &'static str> {
        let view = &self.views[slot];
        let data = view.data.as_ref().ok_or("Open a take first")?;
        if view.selected.len() != 2 {
            return Err("Select two marker rows");
        }
        if let Some(reason) = self.marker_command_block(slot, self.merge_keys_capture(), false) {
            return Err(reason);
        }
        if self.typing(ctx)
            || ctx.memory(|m| m.any_popup_open())
            || ctx.input(|i| i.key_down(egui::Key::M))
        {
            return Err("Finish the current action");
        }
        let rows = view.tracks
            .selected_pair(data, &view.selected)
            .ok_or("Select two marker rows")?;
        if rows.iter().any(|&row| {
            let trajectory = &data.take.trajectories[data.markers[row].traj];
            trajectory.parent_uid.is_none() || trajectory.lenient_membership
        }) {
            return Err("Can't edit these rows");
        }
        Ok(rows)
    }

    fn merge_keys(
        &mut self,
        ctx: &egui::Context,
        slot: usize,
        rows: [usize; 2],
        direction: Direction,
    ) {
        let view = &self.views[slot];
        let Some(data) = &view.data else { return };
        let (source, destination) =
            direction.source_and_destination(rows.map(|row| data.markers[row].uid));
        let range = view
            .range
            .map(|(a, b)| u32::try_from(a).ok().zip(u32::try_from(b).ok()));
        let range = match range {
            None => None,
            Some(Some(range)) => Some(range),
            Some(None) => {
                self.takm_note = Some((
                    "Invalid frame range".into(),
                    ctx.input(|i| i.time),
                ));
                return;
            }
        };
        let request = MergeRequest {
            source,
            destination,
            range,
            selection: view.edit_selection_state(),
        };
        self.merge_selected_keys(ctx, slot, request);
    }

    /// Return true while the Tracks index is still being prepared.
    pub(super) fn start_merge_keys_capture(&mut self, ctx: &egui::Context) -> bool {
        let Some(Some(direction)) = merge_keys::PROBE.get().copied() else {
            return false;
        };
        if self
            .view()
            .data
            .as_ref()
            .is_some_and(|data| data.marker_order_handle().get().is_none())
        {
            return true;
        }
        match self.merge_keys_rows(self.active, ctx) {
            Ok(rows) => self.merge_keys(ctx, self.active, rows, direction),
            Err(reason) => {
                eprintln!("--merge-keys: {reason}");
                self.failed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        false
    }

    pub(super) fn wait_merge_keys_capture(&self) -> bool {
        if !matches!(merge_keys::PROBE.get(), Some(Some(_))) {
            return false;
        }
        if self.marker_job_running(self.active) {
            return true;
        }
        if let Some((note, _)) = &self.takm_note {
            println!("--merge-keys: {note}");
        }
        false
    }
}

fn select_rows(view: &mut View, selected: Vec<usize>) {
    view.clear_selection();
    view.primary = selected.last().copied();
    view.selected = selected.iter().copied().collect();
    view.selection_order = selected;
    view.labels.scroll_to = view.primary;
}
