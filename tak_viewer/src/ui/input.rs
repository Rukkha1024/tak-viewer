//! Shared scene and graph gestures. Keep navigation latched until the primary release.
use eframe::egui::{Event, PointerButton, Response, Ui};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrimaryNavigation {
    Orbit,
    Pan,
}

/// macOS trackpads use Option+primary; releasing Option before the pointer keeps navigation.
pub fn primary_navigation(ui: &Ui, response: &Response) -> Option<PrimaryNavigation> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let id = response.id.with("primary_navigation");
    let (down, pressed, modifiers) = ui.input(|i| (i.pointer.primary_down(), i.pointer.button_pressed(PointerButton::Primary), i.modifiers));
    if pressed && (response.is_pointer_button_down_on() || response.clicked_by(PointerButton::Primary)) {
        ui.data_mut(|d| {
            d.remove::<PrimaryNavigation>(id);
            if modifiers.alt {
                d.insert_temp(id, if modifiers.shift { PrimaryNavigation::Pan } else { PrimaryNavigation::Orbit });
            }
        });
    }
    let navigation = ui.data(|d| d.get_temp(id));
    if !down {
        ui.data_mut(|d| d.remove::<PrimaryNavigation>(id));
    }
    navigation
}

/// Wheel points plus native pinch events. `zoom_delta()` also synthesizes wheel zoom,
/// so adding it to raw scrolling would apply Command/Ctrl-scroll twice.
pub fn zoom_scroll(ui: &Ui) -> f32 {
    ui.input(|i| {
        i.raw_scroll_delta.y + i.events.iter().filter_map(|event| match event {
            Event::Zoom(factor) => Some(factor.ln() / crate::camera::ZOOM_SPEED),
            _ => None,
        }).sum::<f32>()
    })
}
