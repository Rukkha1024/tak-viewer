//! `View → Settings… → Labeled marker order` (v0.72.0). Each click is one change of
//! `Settings::marker_order`; the caller records it for Ctrl+Z / Ctrl+Y.
use super::theme;
use eframe::egui::{self, RichText, Ui};

/// `names` = labels of the open files in the file order (offered by "Add names from open files").
pub fn show(ui: &mut Ui, names: &[String], order: &mut Vec<String>) {
    ui.separator();
    ui.label(RichText::new("Labeled marker order").color(theme::TEXT_DIM));
    ui.label(RichText::new("Names here come first in the Labeled list, top to bottom. Other names keep the file order after them.")
        .small().color(theme::TEXT_DIM));
    if order.is_empty() {
        ui.label("Now: file order.");
    }
    let mut action = None;
    egui::ScrollArea::vertical().id_salt("marker_order_rows").max_height(260.0).show(ui, |ui| {
        egui::Grid::new("marker_order_table").num_columns(3).spacing([8.0, 2.0]).striped(true).show(ui, |ui| {
            let last = order.len().saturating_sub(1);
            for (i, name) in order.iter().enumerate() {
                ui.label(RichText::new(format!("{}", i + 1)).color(theme::TEXT_DIM));
                ui.label(name);
                ui.horizontal(|ui| {
                    if ui.add_enabled(i > 0, egui::Button::new("↑").small()).on_hover_text("Move up").clicked() {
                        action = Some(Action::Move(i, i - 1));
                    }
                    if ui.add_enabled(i < last, egui::Button::new("↓").small()).on_hover_text("Move down").clicked() {
                        action = Some(Action::Move(i, i + 1));
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("Top").small()).on_hover_text("Move to the top").clicked() {
                        action = Some(Action::Move(i, 0));
                    }
                    if ui.small_button("✕").on_hover_text("Remove from this list").clicked() {
                        action = Some(Action::Remove(i));
                    }
                });
                ui.end_row();
            }
        });
    });
    let id = ui.id().with("marker_order_draft");
    let mut draft = ui.ctx().data_mut(|d| d.get_temp::<String>(id).unwrap_or_default());
    ui.horizontal(|ui| {
        let response = ui.add(egui::TextEdit::singleline(&mut draft).hint_text("Marker name").desired_width(160.0).char_limit(64));
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let fresh = !draft.trim().is_empty() && !contains(order, &draft);
        if (ui.add_enabled(fresh, egui::Button::new("Add")).clicked() || enter) && fresh {
            action = Some(Action::Add(vec![draft.trim().to_owned()]));
            draft.clear();
        }
    });
    ui.ctx().data_mut(|d| d.insert_temp(id, draft));
    ui.horizontal(|ui| {
        let missing: Vec<String> = names.iter().filter(|name| !contains(order, name)).cloned().collect();
        if ui.add_enabled(!missing.is_empty(), egui::Button::new("Add names from open files")).clicked() {
            action = Some(Action::Add(missing));
        }
        if ui.add_enabled(!order.is_empty(), egui::Button::new("Use file order")).on_hover_text("Clear this list").clicked() {
            action = Some(Action::Clear);
        }
    });
    if let Some(action) = action {
        apply(order, action);
    }
}

#[derive(Debug)]
enum Action {
    Move(usize, usize),
    Remove(usize),
    Add(Vec<String>),
    Clear,
}

fn contains(order: &[String], name: &str) -> bool {
    let name = name.trim().to_lowercase();
    order.iter().any(|n| n.to_lowercase() == name)
}

fn apply(order: &mut Vec<String>, action: Action) {
    match action {
        Action::Move(from, to) => {
            let name = order.remove(from);
            order.insert(to, name);
        }
        Action::Remove(i) => {
            order.remove(i);
        }
        Action::Add(names) => order.extend(names),
        Action::Clear => order.clear(),
    }
    crate::marker_order::clean(order);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_add_remove_and_clear_keep_one_clean_list() {
        let mut order: Vec<String> = ["LASI", "RASI", "C7"].map(String::from).to_vec();
        apply(&mut order, Action::Move(2, 0));
        assert_eq!(order, ["C7", "LASI", "RASI"]);
        apply(&mut order, Action::Move(0, 1));
        assert_eq!(order, ["LASI", "C7", "RASI"]);
        apply(&mut order, Action::Add(vec!["c7".into(), " T10 ".into()]));
        assert_eq!(order, ["LASI", "C7", "RASI", "T10"], "same name ignoring case is kept once");
        apply(&mut order, Action::Remove(1));
        assert_eq!(order, ["LASI", "RASI", "T10"]);
        assert!(contains(&order, "lasi"));
        apply(&mut order, Action::Clear);
        assert!(order.is_empty());
    }
}
