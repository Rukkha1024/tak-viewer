use eframe::egui::{self, scroll_area, Event, MouseWheelUnit, RawInput, Rect, Vec2};

const POINTS_PER_NOTCH: f32 = 120.0;

#[derive(Debug, Default)]
pub(in crate::app) struct TabWheel {
    region: Option<(Rect, egui::LayerId)>,
    state: scroll_area::State,
    discrete: f32,
    precise: f32,
}

impl TabWheel {
    pub(in crate::app) fn clear_region(&mut self) {
        self.region = None;
        self.stop_gesture();
    }

    pub(super) fn stop_gesture(&mut self) {
        self.discrete = 0.0;
        self.precise = 0.0;
    }

    pub(in crate::app) fn center(&self) -> Option<egui::Pos2> {
        self.region.map(|(rect, _)| rect.center())
    }

    #[cfg(test)]
    pub(in crate::app) fn offset(&self) -> f32 {
        self.state.offset.x
    }

    /// Route before egui computes raw deltas, smoothing and modifier-based zoom.
    /// Even on an edge, none of this gesture should reach the other panes.
    pub(in crate::app) fn route(&mut self, ctx: &egui::Context, raw: &mut RawInput) {
        let Some((strip, layer)) = self.region else {
            return;
        };
        let mut pointer = ctx.input(|input| input.pointer.hover_pos());
        raw.events.retain(|event| {
            match *event {
                Event::PointerMoved(pos) | Event::PointerButton { pos, .. } => pointer = Some(pos),
                Event::PointerGone => pointer = None,
                Event::MouseWheel { unit, delta, .. }
                    if pointer.is_some_and(|pos| {
                        strip.contains(pos) && ctx.layer_id_at(pos) == Some(layer)
                    }) =>
                {
                    let axis = if delta.x.abs() >= delta.y.abs() {
                        delta.x
                    } else {
                        delta.y
                    };
                    match unit {
                        MouseWheelUnit::Point => self.precise += axis,
                        MouseWheelUnit::Line => self.discrete += axis * POINTS_PER_NOTCH,
                        MouseWheelUnit::Page => self.discrete += axis * strip.width(),
                    }
                    return false;
                }
                _ => {}
            }
            true
        });
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, bar: Rect, draw: impl FnOnce(&mut egui::Ui)) {
        self.region = ui
            .is_enabled()
            .then_some((bar.intersect(ui.clip_rect()), ui.layer_id()));
        let mut delta = 0.0;
        if ui.is_enabled()
            && ui.rect_contains_pointer(bar)
            && (self.discrete != 0.0 || self.precise != 0.0)
        {
            let dt = ui.input(|input| input.stable_dt);
            let step = if self.discrete.abs() < 1.0 {
                self.discrete
            } else {
                self.discrete * (1.0 - (-dt / 0.040).exp())
            };
            self.discrete -= step;
            delta = step + std::mem::take(&mut self.precise);
        } else {
            self.stop_gesture();
        }

        // egui uses persisted storage for scroll areas. Keep this one in the strip's runtime.
        let id = ui.make_persistent_id(egui::Id::new("tab_scroll"));
        self.state.store(ui.ctx(), id);
        let previous_offset = self.state.offset.x;
        // Only the strip's routed gesture goes into its ScrollArea. Restore unrelated input
        // afterwards, including any smoothing from gestures that started elsewhere.
        let previous_scroll = ui.input_mut(|input| {
            std::mem::replace(&mut input.smooth_scroll_delta, Vec2::new(delta, 0.0))
        });
        let output = egui::ScrollArea::horizontal()
            .id_salt("tab_scroll")
            .max_height(bar.height())
            .auto_shrink([false, false])
            .scroll_bar_visibility(scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, draw);
        ui.input_mut(|input| input.smooth_scroll_delta = previous_scroll);
        ui.data_mut(|data| data.remove::<scroll_area::State>(output.id));
        self.state = output.state;
        let offset = self.state.offset.x;
        let end = (output.content_size.x - output.inner_rect.width()).max(0.0);
        if (offset <= 0.0 && self.discrete > 0.0) || (offset >= end && self.discrete < 0.0) {
            self.discrete = 0.0;
        }
        if offset != previous_offset || self.discrete != 0.0 {
            ui.ctx().request_repaint();
        }
    }
}
