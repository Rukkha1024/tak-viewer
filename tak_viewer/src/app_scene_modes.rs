use super::*;
use crate::camera::Preset;

/// A toggle uses the first key-down, never OS repeat or modified G/L.
pub(super) fn plain_scene_press(ctx: &egui::Context, key: Key) -> bool {
    ctx.input_mut(|input| {
        let mut pressed = false;
        input.events.retain(|event| match event {
            egui::Event::Key { key: actual, pressed: true, repeat, modifiers, .. }
                if *actual == key && modifiers.is_none() =>
            {
                pressed |= !repeat;
                false
            }
            _ => true,
        });
        pressed
    })
}

impl TakViewerApp {
    pub(super) fn follow_scene_key(&mut self, ctx: &egui::Context) {
        if key_focus::keys_blocked(ctx) || self.typing(ctx) || self.capture.is_some() || self.dirty_prompt.is_some()
            || self.comment_edit.is_some() || self.hold.is_some()
        {
            return;
        }
        if let key_focus::Target::Scene(slot) = key_focus::target(ctx) {
            if plain_scene_press(ctx, Key::G) {
                self.views[slot].toggle_follow();
                ctx.request_repaint();
            }
        }
    }

    pub(super) fn camera_menu(&mut self, ui: &mut egui::Ui) {
        let view = self.view_mut();
        ui.add_enabled_ui(view.camera_ready(), |ui| {
            ui.menu_button("View Direction", |ui| {
                for preset in Preset::ALL {
                    if ui.selectable_label(view.cam.preset == preset, preset.label()).clicked() {
                        view.cam.preset = preset;
                        ui.close_menu();
                    }
                }
            });
            let mut follow = view.cam.follow;
            if ui.checkbox(&mut follow, "Follow Selected (G)").changed() {
                view.toggle_follow();
            }
        });
    }
}
