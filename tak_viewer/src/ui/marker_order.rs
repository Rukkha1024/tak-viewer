//! Summary and entry point for the Labeling pane's Reorder mode.

use super::theme;
use eframe::egui::{self, RichText, Ui};

pub enum Action {
    GoToLabeling,
    UseFileOrder,
}

pub fn show(ui: &mut Ui, summaries: &[String], can_reset: bool) -> Option<Action> {
    ui.separator();
    ui.label("Labeled marker order");
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        if summaries.is_empty() {
            let summary = if can_reset { "Open a take to see moved names" } else { "File order" };
            ui.label(RichText::new(summary).color(theme::TEXT_DIM));
        } else {
            ui.label(RichText::new(summaries.join(" · ")).color(theme::TEXT_DIM));
        }
        if ui.button("Go to Labeling").clicked() {
            action = Some(Action::GoToLabeling);
        }
        if ui.add_enabled(can_reset, egui::Button::new("Use file order")).clicked() {
            action = Some(Action::UseFileOrder);
        }
    });
    action
}
