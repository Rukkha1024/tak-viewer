use super::{dock::Pane, exact_key, graph, key_focus, panels, undo, Action, Key, TakViewerApp, View};
use eframe::egui::{self, Ui};
use graph::{
    conveniences::{self, LockedSelection},
    tracks::{self, Layout},
};

impl TakViewerApp {
    pub(crate) fn bench_graph_pane(&mut self, ctx: &egui::Context, layout: Layout, detached: bool, count: usize, pan: bool) -> Option<usize> {
        let view = &mut self.views[self.active];
        let data = view.data.as_ref()?;
        let mut markers: Vec<_> = data.groups.iter().flat_map(|g| g.members.iter().copied()).take(count).collect();
        if count == 1 && !data.markers.is_empty() {
            markers = vec![0];
        }
        if markers.len() != count { return None; }
        let file_id = view.file_id;
        if pan {
            let quarter = (data.end_frame - data.start_frame) / 4;
            tracks::set_zoom(ctx, file_id, (data.start_frame + quarter, data.end_frame - quarter), data);
        }
        view.select_many(markers, None, false);
        view.primary = view.selected.first().copied();
        self.settings.graph_layout = layout;
        self.set_graph_float(false);
        if detached {
            super::dock::hide(&mut self.dock, Pane::Graph);
            let surface = self.dock.add_window(vec![Pane::Graph]);
            self.dock.get_window_state_mut(surface).unwrap()
                .set_position(egui::pos2(365.0, 325.0))
                .set_size(egui::vec2(850.0, 380.0));
        }
        Some(file_id)
    }

    pub(crate) fn bench_graph_zoom(&mut self, layout: Layout) -> conveniences::ValueZoom {
        self.view_mut().graph_view.zoom(layout == Layout::Combined).clone()
    }

    pub(crate) fn bench_graph_ready(&self) -> bool {
        !self.view().lanes.preparing() && !self.graph_put_off
    }

    pub(super) fn graph_tab(&mut self, ui: &mut Ui) -> Option<Action> {
        let slot = self.active;
        let view = &mut self.views[slot];
        let data = view.data.as_ref()?;
        let input_enabled = self.hold.is_none() && self.comment_edit.is_none();
        let mut fit = false;
        let mut toggle_lock = false;
        let can_lock = view.primary.is_some_and(|m| m < data.markers.len());
        let over_3d = tracks::selector(ui, &mut self.settings.graph_layout, |ui, layout| {
            ui.add_enabled_ui(input_enabled, |ui| {
                if layout != Layout::Tracks {
                    toggle_lock = conveniences::lock_button(ui, view.graph_view.lock.is_some(), can_lock);
                    conveniences::auto_button(ui, view.graph_view.zoom(layout == Layout::Combined));
                }
                fit = conveniences::fit_button(ui, view.range.is_some());
            });
        });
        if over_3d {
            return Some(Action::GraphOver3d);
        }
        if fit {
            fit_graph(view, ui.ctx(), false);
        }
        let layout = self.settings.graph_layout;
        if layout == Layout::Tracks {
            return self.graph_tracks_tab(ui);
        }
        if toggle_lock {
            self.toggle_graph_lock(slot);
        }
        let view = &mut self.views[slot];
        let data = view.data.as_ref()?;
        let (graphed, total) = if let Some(lock) = &view.graph_view.lock {
            lock.shown(data, &mut view.tracks, ui, view.file_id)
        } else {
            current_graph_selection(view)
        };
        let combined = layout == Layout::Combined;
        let overlay = view.overlay.as_ref().map(|o| (o.path.as_path(), &o.data));
        view.lanes.prepare_arc(data, view.file_id, &graphed, overlay, view.shown, ui.ctx(), combined);
        let names: Vec<_> = graphed.iter().map(|&k| data.markers[k].name.as_str()).collect();
        let spans = view.comments.spans.get_many(&view.comments.list, view.file_id, &names, view.picked);
        let zoom = Some(view.graph_view.zoom(combined));
        panels::graph_pane(ui, data, view.pb.frame, &graphed, total, view.file_id,
            &mut view.lanes, spans, &mut view.range, combined, zoom)
    }

    fn toggle_graph_lock(&mut self, slot: usize) {
        let view = &mut self.views[slot];
        let Some(data) = &view.data else { return };
        let before = view.graph_view.lock.take();
        if before.is_none() {
            let (shown, total) = current_graph_selection(view);
            if shown.is_empty() {
                return;
            }
            view.graph_view.lock = Some(LockedSelection::new(data, &shown, total));
        }
        if !self.no_persist {
            let action = undo::Part::GraphLock {
                file: data.take.path.clone(),
                before,
                after: view.graph_view.lock.clone(),
            };
            self.history.push(vec![self.view_undo_part(slot, action)]);
        }
    }

    pub(super) fn graph_fit_keys(&mut self, ctx: &egui::Context) {
        if key_focus::keys_blocked(ctx) || self.capture.is_some() || self.dirty_prompt.is_some()
            || self.hold.is_some() || self.comment_edit.is_some() || self.view().data.is_none() {
            return;
        }
        let whole = exact_key(ctx, Key::F, false, true, false);
        let selected = exact_key(ctx, Key::F, false, false, false);
        if !(whole || selected) {
            return;
        }
        if let key_focus::Target::Graph(slot) = key_focus::target(ctx) {
            fit_graph(&mut self.views[slot], ctx, whole);
            return;
        }
        let pointer = ctx.input(|input| input.pointer.hover_pos());
        let slot = pointer.and_then(|p| self.open_views().into_iter().find(|&slot| {
            let view = &self.views[slot];
            view.scene_rect.contains(p) && !view.graph_rect.is_some_and(|r| r.contains(p))
                && !view.plate_rect.is_some_and(|r| r.contains(p))
        }));
        if let Some(slot) = slot {
            let view = &mut self.views[slot];
            if whole { view.fit_pending = true; } else { view.focus_selected(); }
            ctx.request_repaint();
        }
    }

    pub(super) fn apply_graph_convenience_probe(&mut self, ctx: &egui::Context, probe: &tracks::Probe) {
        let view = self.view_mut();
        if probe.fit_range {
            fit_graph(view, ctx, false);
        }
        view.graph_view.zoom(probe.layout == Layout::Combined).magnify(probe.magnification);
        let (Some(names), Some(data)) = (&probe.lock, &view.data) else { return };
        let selected: std::collections::BTreeSet<_> = names.split(',').map(str::trim)
            .filter_map(|name| data.markers.iter().position(|m| m.name == name)).collect();
        if let Some(&first) = selected.first() {
            let (shown, total) = panels::graphed(data, &selected, first);
            view.graph_view.lock = Some(LockedSelection::new(data, &shown, total));
        } else {
            eprintln!("--graph-lock: 이 파일에서 지정한 마커를 찾지 못했어요.");
        }
    }
}

fn current_graph_selection(view: &View) -> (Vec<usize>, usize) {
    view.data.as_ref().zip(view.primary)
        .filter(|(data, marker)| *marker < data.markers.len())
        .map_or_else(|| (Vec::new(), 0), |(data, marker)| panels::graphed(data, &view.selected, marker))
}

fn fit_graph(view: &mut View, ctx: &egui::Context, whole: bool) {
    let Some(data) = &view.data else { return };
    let range = if whole { Some((data.start_frame, data.end_frame)) } else { view.range };
    if let Some(range) = range {
        tracks::set_zoom(ctx, view.file_id, range, data);
        view.graph_view.reset_zoom();
        ctx.request_repaint();
    }
}
