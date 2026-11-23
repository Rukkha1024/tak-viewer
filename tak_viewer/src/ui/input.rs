//! Viewport navigation shared by the 3D scene and marker graph.

use eframe::egui::{Event, PointerButton, Response, Ui};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PrimaryNavigation {
    Orbit,
    Pan,
}

/// Latch the navigation mode from the press through the release. Releasing Option
/// first must not turn the rest of a drag into marker or frame selection.
pub(super) fn primary_navigation(ui: &Ui, response: &Response) -> Option<PrimaryNavigation> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let id = response.id.with("primary_navigation");
    let owns_press = ui.is_enabled()
        && (response.is_pointer_button_down_on()
            || response.clicked_by(PointerButton::Primary)
            || response.drag_stopped_by(PointerButton::Primary));
    let (press, down, released) = ui.input(|input| {
        let press = input.events.iter().rev().find_map(|event| match event {
            Event::PointerButton {
                button: PointerButton::Primary,
                pressed: true,
                modifiers,
                ..
            } => Some(*modifiers),
            _ => None,
        });
        (
            press,
            input.pointer.primary_down(),
            input.pointer.primary_released(),
        )
    });
    ui.data_mut(|data| {
        let navigation = match press {
            Some(modifiers) => {
                (owns_press && modifiers.alt && !modifiers.command && !modifiers.ctrl).then_some(
                    if modifiers.shift {
                        PrimaryNavigation::Pan
                    } else {
                        PrimaryNavigation::Orbit
                    },
                )
            }
            None if ui.is_enabled() && (down || released) => data.get_temp(id),
            None => None,
        };
        if let Some(navigation) = navigation.filter(|_| down) {
            data.insert_temp(id, navigation);
        } else {
            data.remove::<PrimaryNavigation>(id);
        }
        navigation
    })
}

/// Scroll points plus native pinch. Egui's computed zoom also counts Command-scroll,
/// so use the raw pinch events to avoid applying the same wheel movement twice.
pub(super) fn zoom_scroll(ui: &Ui) -> f32 {
    ui.input(|input| {
        let mut scroll = input.raw_scroll_delta.y;
        if cfg!(target_os = "macos") {
            scroll += input
                .events
                .iter()
                .filter_map(|event| match event {
                    Event::Zoom(factor) if factor.is_finite() && *factor > 0.0 => {
                        Some(factor.ln() / crate::camera::ZOOM_SPEED)
                    }
                    _ => None,
                })
                .sum::<f32>();
        }
        scroll
    })
}
