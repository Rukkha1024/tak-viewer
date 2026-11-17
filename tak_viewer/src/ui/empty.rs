//! Fileless editor and panes. No view data, playback, or editing controls are drawn here.

use super::{dock::Pane, theme};
use eframe::egui::{self, FontId, RichText, Sense, Ui};

pub const EDITOR_HINT: &str = "Open a file from the Explorer (or drop a .tak / .takm here)";
pub const REOPEN_HINT: &str = "Ctrl+Shift+T   Reopen the last closed view or tab";

/// Keep a neutral editor canvas when the last file is closed, without a 3D scene or deck.
pub fn editor(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
    editor_at(ui, rect);
}

/// Cover the empty dock leaf's otherwise-unused internal tab row too.
pub fn editor_at(ui: &Ui, rect: egui::Rect) {
    let mut painter = ui.painter().clone();
    painter.set_clip_rect(rect.intersect(ui.ctx().screen_rect()));
    painter.rect_filled(rect, 0.0, theme::PANEL_BG_DARK);
    let text = painter.layout(
        EDITOR_HINT.to_owned(),
        FontId::proportional(13.0),
        theme::TEXT_DIM,
        (rect.width() - 48.0).max(1.0),
    );
    let extra = painter.layout(
        REOPEN_HINT.to_owned(), FontId::proportional(11.0), theme::TEXT_DIM,
        (rect.width() - 48.0).max(1.0),
    );
    let height = text.size().y + 8.0 + extra.size().y;
    let top = rect.center().y - height / 2.0;
    painter.galley(egui::pos2(rect.center().x - text.size().x / 2.0, top), text.clone(), theme::TEXT_DIM);
    painter.galley(egui::pos2(rect.center().x - extra.size().x / 2.0, top + text.size().y + 8.0), extra, theme::TEXT_DIM);
}

fn panel_hint(pane: Pane) -> &'static str {
    match pane {
        Pane::Labels | Pane::Labeling | Pane::EditTools | Pane::Devices => "Open a file from the Explorer.",
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
            assert!((bounds.center().x - canvas.center().x).abs() < 0.1);
            assert!(bounds.center().y < canvas.center().y);
            assert_eq!(
                hint.galley.elided, false,
                "the hint wraps instead of losing words"
            );
            assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.rect == canvas && r.fill == theme::PANEL_BG_DARK)));
        }
    }
}
