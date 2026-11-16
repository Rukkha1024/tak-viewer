//! Fileless editor and panes. No view data, playback, or editing controls are drawn here.

use super::{dock::Pane, theme};
use eframe::egui::{self, FontId, RichText, Sense, Ui};

pub const EDITOR_HINT: &str = "Open a file from the Explorer (or drop a .tak / .takm here)";

/// Keep a neutral editor canvas when the last file is closed, without a 3D scene or deck.
pub fn editor(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::PANEL_BG_DARK);
    let text = ui.painter().layout(
        EDITOR_HINT.to_owned(),
        FontId::proportional(13.0),
        theme::TEXT_DIM,
        (rect.width() - 48.0).max(1.0),
    );
    ui.painter()
        .galley(rect.center() - text.size() / 2.0, text, theme::TEXT_DIM);
}

fn panel_hint(pane: Pane) -> &'static str {
    match pane {
        Pane::Labels | Pane::Labeling | Pane::EditTools => "Open a file from the Explorer.",
        _ => "파일을 열면 여기에 나와요.",
    }
}

/// File-dependent panes stay available in the layout, with no stale values or active tools.
pub fn panel(ui: &mut Ui, pane: Pane) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.add(egui::Label::new(RichText::new(panel_hint(pane)).color(theme::TEXT_DIM)).wrap());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_editor_centers_the_complete_hint_and_wraps_in_a_narrow_area() {
        for width in [140.0, 380.0, 900.0] {
            let ctx = egui::Context::default();
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 300.0));
            let mut canvas = egui::Rect::NOTHING;
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::none())
                        .show(ctx, |ui| {
                            canvas = ui.available_rect_before_wrap();
                            editor(ui);
                        });
                },
            );
            let hint = out
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == EDITOR_HINT => Some(t),
                    _ => None,
                })
                .expect("the complete open-file hint is painted");
            let bounds = egui::Rect::from_min_size(hint.pos, hint.galley.size());
            assert!(
                canvas.contains_rect(bounds),
                "{width}: {bounds:?} in {canvas:?}"
            );
            assert!(bounds.center().distance(canvas.center()) < 0.1);
            assert_eq!(
                hint.galley.elided, false,
                "the hint wraps instead of losing words"
            );
            assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.rect == canvas && r.fill == theme::PANEL_BG_DARK)));
        }
    }
}
