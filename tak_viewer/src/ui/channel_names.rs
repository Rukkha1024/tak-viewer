//! Settings-only names. Draft text stays outside Settings until Enter/blur.
use crate::device_grid::ChannelNames;
use eframe::egui::{self, RichText, Ui};
use std::collections::BTreeMap;

#[derive(Clone, Default)]
struct Editor {
    selected: String,
    drafts: BTreeMap<(String, usize), String>,
}

pub fn show(ui: &mut Ui, mut catalog: BTreeMap<String, (String, usize)>, names: &mut ChannelNames) {
    let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("settings.channel_names"));
    for (device, channels) in names.iter() {
        let count = channels.keys().max().map_or(0, |n| n + 1);
        let entry = catalog.entry(device.clone()).or_insert_with(|| (device.clone(), 0));
        entry.1 = entry.1.max(count);
    }
    ui.separator();
    ui.strong("Channel names");
    if catalog.is_empty() {
        ui.label("Open a file with analog devices");
        return;
    }
    let id = ui.id().with("channel_name_editor");
    let mut editor = ui
        .ctx()
        .data_mut(|d| d.get_temp::<Editor>(id).unwrap_or_default());
    if !catalog.contains_key(&editor.selected) {
        editor.selected = catalog.keys().next().unwrap().clone();
    }
    egui::ComboBox::from_id_salt("channel_name_device")
        .selected_text(&catalog[&editor.selected].0)
        .show_ui(ui, |ui| {
            for (device, (caption, _)) in &catalog {
                ui.selectable_value(&mut editor.selected, device.clone(), caption);
            }
        });
    let device = editor.selected.clone();
    egui::ScrollArea::vertical()
        .id_salt("channel_name_rows")
        .max_height(300.0)
        .show(ui, |ui| {
            egui::Grid::new("channel_name_table")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Ch");
                    ui.label("Name");
                    ui.end_row();
                    for ci in 0..catalog[&device].1 {
                        ui.label(ci.to_string());
                        let key = (device.clone(), ci);
                        let current = names
                            .get(&device)
                            .and_then(|n| n.get(&ci))
                            .cloned()
                            .unwrap_or_default();
                        let draft = editor.drafts.entry(key).or_insert_with(|| current.clone());
                        let response = ui.add(
                            egui::TextEdit::singleline(draft)
                                .id_salt((&device, ci))
                                .desired_width(220.0)
                                .char_limit(64),
                        );
                        let escape =
                            response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
                        let enter =
                            response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if escape {
                            *draft = current;
                            response.surrender_focus();
                        } else if enter || response.lost_focus() {
                            let value = draft.trim().to_owned();
                            if value.is_empty() {
                                if let Some(channels) = names.get_mut(&device) {
                                    channels.remove(&ci);
                                }
                                if names.get(&device).is_some_and(BTreeMap::is_empty) {
                                    names.remove(&device);
                                }
                            } else {
                                names
                                    .entry(device.clone())
                                    .or_default()
                                    .insert(ci, value.clone());
                            }
                            *draft = value;
                            if enter {
                                response.surrender_focus();
                            }
                        } else if !response.has_focus() {
                            *draft = current;
                        }
                        ui.end_row();
                    }
                });
        });
    if ui.button("Reset").clicked() {
        names.remove(&device);
        editor.drafts.retain(|(d, _), _| d != &device);
    }
    ui.label(
        RichText::new("Applies to every file recorded with this device · file values unchanged")
            .small()
            .weak(),
    );
    ui.ctx().data_mut(|d| d.insert_temp(id, editor));
}
