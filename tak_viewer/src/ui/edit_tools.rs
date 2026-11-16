//! Independent Edit Tools pane. The caller owns state, dispatch, and undo.

use super::theme;
use eframe::egui::{self, RichText, Ui};
use tak_core::edit::fill::{FillMethod, DEFAULT_MAX_GAP_FRAMES};
use tak_core::edit::smooth::{ui_cutoff_to_design_hz, DEFAULT_UI_CUTOFF_HZ};
use tak_core::tak::{uid_hex, Uid};

#[derive(Clone, Debug)]
pub struct TrackChoice {
    pub uid: Uid,
    pub name: String,
}

/// Selected tracks are ordered by selection time, with the Pattern target last.
/// Text inputs remain strings so invalid or incomplete input can be displayed and rejected.
#[derive(Clone, Debug)]
pub struct EditToolsState {
    pub interpolation: FillMethod,
    pub max_gap: String,
    pub max_frequency: String,
    pub selected_tracks: Vec<TrackChoice>,
    pub editable_track_count: usize,
    pub range: Option<(u32, u32)>,
    pub sample_rate_hz: Option<f64>,
    /// A caller-provided reason, e.g. a remote/read-only document or an edit in progress.
    pub blocked_reason: Option<String>,
}

impl Default for EditToolsState {
    fn default() -> Self {
        Self {
            interpolation: FillMethod::Cubic,
            max_gap: DEFAULT_MAX_GAP_FRAMES.to_string(),
            max_frequency: format!("{DEFAULT_UI_CUTOFF_HZ}"),
            selected_tracks: Vec::new(),
            editable_track_count: 0,
            range: None,
            sample_rate_hz: None,
            blocked_reason: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillScope {
    SelectedRange,
    SelectedTracksAllFrames,
    Everything,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmoothScope {
    SelectedRange,
    SelectedTracksAllFrames,
    AllTracksSelectedRange,
    AllTracksAllFrames,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EditToolsAction {
    SetInterpolation(FillMethod),
    SetMaxGap(String),
    SetMaxFrequency(String),
    Fill(FillScope),
    Smooth(SmoothScope),
}

impl EditToolsState {
    fn pattern_selection_reason(&self) -> Option<&'static str> {
        match self.selected_tracks.len() {
            0 | 1 => Some("Pattern Based: select reference markers, then the target last"),
            _ => None,
        }
    }

    pub fn parsed_max_gap(&self) -> Option<u32> {
        self.max_gap.trim().parse::<u32>().ok().filter(|&v| v > 0)
    }

    /// Return the entered UI value. The core maps it once to the design cutoff.
    pub fn parsed_ui_cutoff(&self) -> Option<f64> {
        let ui_hz = self.max_frequency.trim().parse::<f64>().ok()?;
        let rate = self.sample_rate_hz?;
        let design_hz = ui_cutoff_to_design_hz(ui_hz);
        (ui_hz.is_finite()
            && ui_hz > 0.0
            && rate.is_finite()
            && rate > 0.0
            && design_hz.is_finite()
            && design_hz > 0.0
            && design_hz < rate / 2.0)
            .then_some(ui_hz)
    }

    /// The method is explicit so the C / Shift+C shortcuts can ignore the dropdown.
    pub fn fill_reason_for(&self, scope: FillScope, method: FillMethod) -> Option<&str> {
        if let Some(reason) = self.blocked_reason.as_deref() {
            return Some(reason);
        }
        if self.parsed_max_gap().is_none() {
            return Some("Enter a positive whole number for Max. Gap Size.");
        }
        if scope == FillScope::Everything {
            if method == FillMethod::PatternBased {
                return Some("Fill Everything is unavailable for Pattern Based.");
            }
            return (self.editable_track_count == 0)
                .then_some("There are no editable marker tracks.");
        }
        if method == FillMethod::PatternBased {
            if let Some(reason) = self.pattern_selection_reason() {
                return Some(reason);
            }
        }
        if self.selected_tracks.is_empty() {
            return Some("Select marker tracks.");
        }
        if scope == FillScope::SelectedRange && self.range.is_none() {
            return Some("Select a frame range.");
        }
        None
    }

    pub fn smooth_reason(&self, scope: SmoothScope) -> Option<&str> {
        if let Some(reason) = self.blocked_reason.as_deref() {
            return Some(reason);
        }
        if self.sample_rate_hz.is_none() {
            return Some("Open a take to read its frame rate.");
        }
        if self.parsed_ui_cutoff().is_none() {
            return Some("Enter a finite Max. Freq above 0 and below half the frame rate.");
        }
        if matches!(
            scope,
            SmoothScope::AllTracksSelectedRange | SmoothScope::AllTracksAllFrames
        ) {
            if self.editable_track_count == 0 {
                return Some("There are no editable marker tracks.");
            }
            if scope == SmoothScope::AllTracksSelectedRange && self.range.is_none() {
                return Some("Select a frame range.");
            }
            return None;
        }
        if self.selected_tracks.is_empty() {
            return Some("Select marker tracks.");
        }
        if scope == SmoothScope::SelectedRange && self.range.is_none() {
            return Some("Select a frame range.");
        }
        None
    }
}

fn track_name(track: &TrackChoice, tracks: &[TrackChoice]) -> String {
    if tracks
        .iter()
        .filter(|other| other.name == track.name)
        .count()
        > 1
    {
        format!("{} · {}", track.name, uid_hex(&track.uid))
    } else {
        track.name.clone()
    }
}

fn target_text(state: &EditToolsState) -> String {
    state.selected_tracks.last()
        .map(|track| track_name(track, &state.selected_tracks))
        .unwrap_or_else(|| "—".to_owned())
}

fn reason(ui: &mut Ui, label: &str, text: Option<&str>) {
    if let Some(text) = text {
        ui.label(
            RichText::new(format!("{label}: {text}"))
                .size(11.0)
                .color(theme::TEXT_DIM),
        );
    }
}

fn button_row(ui: &mut Ui, labels: &[(&str, bool)]) -> Vec<bool> {
    let mut clicked = Vec::with_capacity(labels.len());
    let gap = ui.spacing().item_spacing.x;
    if ui.available_width() < 330.0 {
        for (label, enabled) in labels {
            clicked.push(
                ui.add_enabled(
                    *enabled,
                    egui::Button::new(RichText::new(*label).size(11.0))
                        .min_size(egui::vec2(ui.available_width(), 31.0)),
                )
                .clicked(),
            );
        }
    } else {
        let width = ((ui.available_width() - gap * (labels.len() - 1) as f32)
            / labels.len() as f32)
            .max(0.0);
        ui.horizontal(|ui| {
            for (label, enabled) in labels {
                clicked.push(
                    ui.add_enabled(
                        *enabled,
                        egui::Button::new(RichText::new(*label).size(11.0))
                            .min_size(egui::vec2(width, 34.0)),
                    )
                    .clicked(),
                );
            }
        });
    }
    clicked
}

/// The same controls in two short rows when the pane is dragged above the graph.
fn show_compact(ui: &mut Ui, state: &EditToolsState) -> Option<EditToolsAction> {
    let mut action = None;
    if let Some(reason) = state.blocked_reason.as_deref() {
        ui.label(RichText::new(reason).size(11.0).color(theme::TEXT_DIM));
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Fill Gaps").strong());
        ui.label("Interpolation");
        egui::ComboBox::from_id_salt("fill_interpolation")
            .selected_text(match state.interpolation {
                FillMethod::Cubic => "Cubic",
                FillMethod::PatternBased => "Pattern Based",
            })
            .width(112.0)
            .show_ui(ui, |ui| {
                for (method, name) in [
                    (FillMethod::Cubic, "Cubic"),
                    (FillMethod::PatternBased, "Pattern Based"),
                ] {
                    if ui
                        .selectable_label(state.interpolation == method, name)
                        .clicked()
                    {
                        action = Some(EditToolsAction::SetInterpolation(method));
                    }
                }
            });
        ui.label("Max. Gap Size");
        let mut value = state.max_gap.clone();
        if ui
            .add(egui::TextEdit::singleline(&mut value).desired_width(42.0))
            .changed()
        {
            action = Some(EditToolsAction::SetMaxGap(value));
        }
        ui.label("Fill Target");
        ui.label(target_text(state));
        for (label, scope) in [
            ("Fill Selected", FillScope::SelectedRange),
            ("Fill All", FillScope::SelectedTracksAllFrames),
            ("Fill Everything", FillScope::Everything),
        ] {
            let why = state.fill_reason_for(scope, state.interpolation);
            let response = ui.add_enabled(why.is_none(), egui::Button::new(label));
            if response.clicked() {
                action = Some(EditToolsAction::Fill(scope));
            }
            if let Some(why) = why {
                response.on_hover_text(why);
            }
        }
    });
    if state.interpolation == FillMethod::PatternBased {
        if let Some(why) = state.pattern_selection_reason() {
            ui.label(RichText::new(why).size(11.0).color(theme::TEXT_DIM));
        }
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Smooth").strong());
        ui.label("Max. Freq (Hz)");
        let mut value = state.max_frequency.clone();
        if ui
            .add(egui::TextEdit::singleline(&mut value).desired_width(48.0))
            .changed()
        {
            action = Some(EditToolsAction::SetMaxFrequency(value));
        }
        for (label, scope) in [
            ("Smooth Selection", SmoothScope::SelectedRange),
            ("Smooth Track", SmoothScope::SelectedTracksAllFrames),
            ("Smooth All Tracks", SmoothScope::AllTracksAllFrames),
        ] {
            let why = state.smooth_reason(scope);
            let response = ui.add_enabled(why.is_none(), egui::Button::new(label));
            if response.clicked() {
                action = Some(EditToolsAction::Smooth(scope));
            }
            if let Some(why) = why {
                response.on_hover_text(why);
            }
        }
    });
    action
}

/// Draw the pane and return at most one user action. Execution happens in the caller.
pub fn show(ui: &mut Ui, state: &EditToolsState) -> Option<EditToolsAction> {
    if ui.available_width() >= 740.0 {
        return show_compact(ui, state);
    }
    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt("edit_tools_pane")
        .show(ui, |ui| {
            ui.add_space(4.0);
            if let Some(reason) = state.blocked_reason.as_deref() {
                ui.label(RichText::new(reason).size(11.0).color(theme::TEXT_DIM));
            }
            ui.label(RichText::new("Fill Gaps").strong().size(14.0));
            let fill_scopes = [
                FillScope::SelectedRange,
                FillScope::SelectedTracksAllFrames,
                FillScope::Everything,
            ];
            let fill_reasons = fill_scopes.map(|s| state.fill_reason_for(s, state.interpolation));
            let fill_key = if state.interpolation == FillMethod::Cubic {
                "C"
            } else {
                "Shift+C"
            };
            let fill_selected = format!("Fill Selected\n{fill_key}");
            let clicked = button_row(
                ui,
                &[
                    (&fill_selected, fill_reasons[0].is_none()),
                    ("Fill All", fill_reasons[1].is_none()),
                    ("Fill Everything", fill_reasons[2].is_none()),
                ],
            );
            for (i, did_click) in clicked.into_iter().enumerate() {
                if did_click {
                    action = Some(EditToolsAction::Fill(fill_scopes[i]));
                }
            }
            if state.interpolation == FillMethod::PatternBased {
                if let Some(why) = state.pattern_selection_reason() {
                    ui.label(RichText::new(why).size(11.0).color(theme::TEXT_DIM));
                }
            }
            if state.blocked_reason.is_none() {
                for (name, why) in ["Fill Selected", "Fill All", "Fill Everything"]
                    .into_iter()
                    .zip(fill_reasons)
                {
                    if why != state.pattern_selection_reason() || state.interpolation != FillMethod::PatternBased {
                        reason(ui, name, why);
                    }
                }
            }

            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(RichText::new("Settings").color(theme::TEXT_DIM).size(11.0));
                let label_width = (ui.available_width() * 0.43).clamp(90.0, 130.0);
                ui.horizontal(|ui| {
                    ui.add_sized([label_width, 20.0], egui::Label::new("Max. Gap Size"));
                    let mut value = state.max_gap.clone();
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut value)
                                .desired_width(ui.available_width()),
                        )
                        .changed()
                    {
                        action = Some(EditToolsAction::SetMaxGap(value));
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_sized([label_width, 20.0], egui::Label::new("Interpolation"));
                    let width = (ui.available_width() - 24.0).max(1.0);
                    egui::ComboBox::from_id_salt("fill_interpolation")
                        .selected_text(match state.interpolation {
                            FillMethod::Cubic => "Cubic",
                            FillMethod::PatternBased => "Pattern Based",
                        })
                        .width(width)
                        .show_ui(ui, |ui| {
                            for (method, name) in [
                                (FillMethod::Cubic, "Cubic"),
                                (FillMethod::PatternBased, "Pattern Based"),
                            ] {
                                if ui
                                    .selectable_label(state.interpolation == method, name)
                                    .clicked()
                                {
                                    action = Some(EditToolsAction::SetInterpolation(method));
                                }
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.add_sized([label_width, 20.0], egui::Label::new("Fill Target"));
                    ui.label(target_text(state));
                });
                if state.interpolation == FillMethod::Cubic {
                    ui.label(
                        RichText::new("Fill Target is used by Pattern Based.")
                            .size(11.0)
                            .color(theme::TEXT_DIM),
                    );
                }
            });

            ui.add_space(18.0);
            ui.label(RichText::new("Smooth").strong().size(14.0));
            let smooth_scopes = [
                SmoothScope::SelectedRange,
                SmoothScope::SelectedTracksAllFrames,
                SmoothScope::AllTracksAllFrames,
            ];
            let smooth_reasons = smooth_scopes.map(|s| state.smooth_reason(s));
            let clicked = button_row(
                ui,
                &[
                    ("Smooth Selection\nX", smooth_reasons[0].is_none()),
                    ("Smooth Track", smooth_reasons[1].is_none()),
                    ("Smooth All Tracks\nCtrl+Alt+X", smooth_reasons[2].is_none()),
                ],
            );
            for (i, did_click) in clicked.into_iter().enumerate() {
                if did_click {
                    action = Some(EditToolsAction::Smooth(smooth_scopes[i]));
                }
            }
            if state.blocked_reason.is_none() {
                for (name, why) in ["Smooth Selection", "Smooth Track", "Smooth All Tracks"]
                    .into_iter()
                    .zip(smooth_reasons)
                {
                    reason(ui, name, why);
                }
            }
            ui.add_space(10.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(
                    RichText::new("Filter Cutoff Frequency")
                        .color(theme::TEXT_DIM)
                        .size(11.0),
                );
                let label_width = (ui.available_width() * 0.43).clamp(90.0, 130.0);
                ui.horizontal(|ui| {
                    ui.add_sized([label_width, 20.0], egui::Label::new("Max. Freq (Hz)"));
                    let mut value = state.max_frequency.clone();
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut value)
                                .desired_width(ui.available_width()),
                        )
                        .changed()
                    {
                        action = Some(EditToolsAction::SetMaxFrequency(value));
                    }
                });
            });
        });
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, vec2, Color32, Event, Modifiers, PointerButton, RawInput, Rect};

    fn state() -> EditToolsState {
        EditToolsState {
            selected_tracks: vec![
                TrackChoice {
                    uid: [1; 16],
                    name: "LASI".into(),
                },
                TrackChoice {
                    uid: [2; 16],
                    name: "RASI".into(),
                },
            ],
            editable_track_count: 4,
            range: Some((10, 30)),
            sample_rate_hz: Some(100.0),
            ..Default::default()
        }
    }

    fn draw(
        ctx: &egui::Context,
        state: &EditToolsState,
        width: f32,
        events: Vec<Event>,
    ) -> (egui::FullOutput, Option<EditToolsAction>) {
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 900.0))),
            focused: true,
            events,
            ..Default::default()
        };
        let mut action = None;
        let output = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = show(ui, state);
            });
        });
        (output, action)
    }

    fn texts(output: &egui::FullOutput) -> Vec<(String, Rect)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, Rect)>) {
            match shape {
                egui::Shape::Text(t) => out.push((
                    t.galley.text().to_owned(),
                    Rect::from_min_size(t.pos, t.galley.size()),
                )),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for shape in &output.shapes {
            walk(&shape.shape, &mut found);
        }
        found
    }

    fn click(ctx: &egui::Context, state: &EditToolsState, name: &str) -> Option<EditToolsAction> {
        let (output, _) = draw(ctx, state, 440.0, vec![]);
        let at = texts(&output)
            .iter()
            .find(|(text, _)| text.contains(name))
            .unwrap()
            .1
            .center();
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        draw(ctx, state, 440.0, vec![Event::PointerMoved(at)]);
        draw(ctx, state, 440.0, vec![button(true)]);
        draw(ctx, state, 440.0, vec![button(false)]).1
    }

    fn save_snapshot(ctx: &egui::Context, output: egui::FullOutput) {
        let Some(dir) = std::env::var_os("TAK_W2_2D_SNAPSHOT_DIR") else {
            return;
        };
        let mut textures = crate::soft_render::Textures::default();
        textures.set(&output.textures_delta);
        let prims = ctx.tessellate(output.shapes, output.pixels_per_point);
        let image = crate::soft_render::paint(
            &prims,
            &textures,
            [440, 900],
            output.pixels_per_point,
            Color32::BLACK,
        );
        let rgba: Vec<_> = image
            .pixels
            .iter()
            .flat_map(|pixel| pixel.to_array())
            .collect();
        crate::shot::save_png(
            &std::path::PathBuf::from(dir).join("edit_tools.png"),
            440,
            900,
            &rgba,
        )
        .unwrap();
    }

    #[test]
    fn defaults_and_literal_cutoff_mapping_remain_explicit() {
        let mut st = state();
        assert_eq!(st.parsed_max_gap(), Some(75));
        assert_eq!(st.parsed_ui_cutoff(), Some(6.0));
        assert_eq!(ui_cutoff_to_design_hz(6.0), 6.0);
        st.max_frequency = "5".into();
        assert_eq!(st.parsed_ui_cutoff(), Some(5.0));
        assert_eq!(ui_cutoff_to_design_hz(5.0), 5.0);
        for value in ["0", "-1", "NaN", "inf", "50", "51", "broken"] {
            st.max_frequency = value.into();
            assert_eq!(st.parsed_ui_cutoff(), None, "{value}");
        }
        for value in ["0", "-1", "1.5", "4294967296", ""] {
            st.max_gap = value.into();
            assert_eq!(st.parsed_max_gap(), None, "{value}");
        }
    }

    #[test]
    fn fill_and_smooth_gate_each_scope_and_pattern_target() {
        let mut st = state();
        for scope in [
            FillScope::SelectedRange,
            FillScope::SelectedTracksAllFrames,
            FillScope::Everything,
        ] {
            assert_eq!(st.fill_reason_for(scope, st.interpolation), None);
        }
        for scope in [
            SmoothScope::SelectedRange,
            SmoothScope::SelectedTracksAllFrames,
            SmoothScope::AllTracksAllFrames,
        ] {
            assert_eq!(st.smooth_reason(scope), None);
        }
        assert_eq!(st.smooth_reason(SmoothScope::AllTracksSelectedRange), None);
        assert_eq!(st.fill_reason_for(FillScope::SelectedRange, FillMethod::PatternBased), None);
        assert_eq!(target_text(&st), "RASI");
        assert_eq!(st.interpolation, FillMethod::Cubic);
        st.range = None;
        assert!(st
            .fill_reason_for(FillScope::SelectedRange, st.interpolation)
            .is_some());
        assert!(st.smooth_reason(SmoothScope::SelectedRange).is_some());
        assert_eq!(
            st.fill_reason_for(FillScope::SelectedTracksAllFrames, st.interpolation),
            None
        );
        assert_eq!(st.smooth_reason(SmoothScope::SelectedTracksAllFrames), None);
        assert!(st
            .smooth_reason(SmoothScope::AllTracksSelectedRange)
            .is_some());
        st.interpolation = FillMethod::PatternBased;
        assert!(st
            .fill_reason_for(FillScope::Everything, st.interpolation)
            .unwrap()
            .contains("Pattern Based"));
        assert_eq!(
            st.fill_reason_for(FillScope::SelectedTracksAllFrames, st.interpolation),
            None
        );
        st.selected_tracks.pop();
        assert_eq!(st.fill_reason_for(FillScope::SelectedTracksAllFrames, st.interpolation),
            Some("Pattern Based: select reference markers, then the target last"));
        assert_eq!(target_text(&st), "LASI");
        st.selected_tracks.clear();
        assert_eq!(target_text(&st), "—");
        assert_eq!(st.fill_reason_for(FillScope::SelectedRange, st.interpolation),
            Some("Pattern Based: select reference markers, then the target last"));
        st.selected_tracks = vec![
            TrackChoice { uid: [1; 16], name: "Same".into() },
            TrackChoice { uid: [2; 16], name: "Same".into() },
            TrackChoice { uid: [3; 16], name: "Last".into() },
        ];
        assert_eq!(st.fill_reason_for(FillScope::SelectedTracksAllFrames, st.interpolation), None);
        st.range = Some((2, 4));
        assert_eq!(st.fill_reason_for(FillScope::SelectedRange, st.interpolation), None);
        assert_eq!(target_text(&st), "Last");
        st.selected_tracks.pop();
        assert!(target_text(&st).contains(&uid_hex(&[2; 16])));
        st.blocked_reason = Some("Read-only take".into());
        assert_eq!(
            st.smooth_reason(SmoothScope::AllTracksAllFrames),
            Some("Read-only take")
        );
    }

    #[test]
    fn headless_controls_emit_immediate_scope_actions_and_show_reasons() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut st = state();
        assert_eq!(
            click(&ctx, &st, "Fill Selected"),
            Some(EditToolsAction::Fill(FillScope::SelectedRange))
        );
        assert_eq!(
            click(&ctx, &st, "Fill All"),
            Some(EditToolsAction::Fill(FillScope::SelectedTracksAllFrames))
        );
        assert_eq!(
            click(&ctx, &st, "Fill Everything"),
            Some(EditToolsAction::Fill(FillScope::Everything))
        );
        assert_eq!(
            click(&ctx, &st, "Smooth Selection"),
            Some(EditToolsAction::Smooth(SmoothScope::SelectedRange))
        );
        assert_eq!(
            click(&ctx, &st, "Smooth Track"),
            Some(EditToolsAction::Smooth(
                SmoothScope::SelectedTracksAllFrames
            ))
        );
        assert_eq!(
            click(&ctx, &st, "Smooth All Tracks"),
            Some(EditToolsAction::Smooth(SmoothScope::AllTracksAllFrames))
        );
        st.interpolation = FillMethod::PatternBased;
        st.selected_tracks.push(TrackChoice { uid: [3; 16], name: "Target".into() });
        assert_eq!(target_text(&st), "Target");
        assert_eq!(click(&ctx, &st, "Fill Selected"),
            Some(EditToolsAction::Fill(FillScope::SelectedRange)));
        assert_eq!(click(&ctx, &st, "Fill All"),
            Some(EditToolsAction::Fill(FillScope::SelectedTracksAllFrames)));
        st.selected_tracks.pop();
        st.selected_tracks.pop();
        let (output, _) = draw(&ctx, &st, 440.0, vec![]);
        let labels = texts(&output);
        assert!(labels.iter().any(|(text, _)| text.contains("Shift+C")));
        assert!(labels.iter().any(|(text, _)| text.contains("Pattern Based: select reference markers, then the target last")));
        assert_eq!(click(&ctx, &st, "Fill Selected"), None);
        assert_eq!(click(&ctx, &st, "Fill All"), None);
        assert!(labels
            .iter()
            .any(|(text, _)| text.contains("Fill Everything is unavailable for Pattern Based")));
        assert_eq!(click(&ctx, &st, "Fill Everything"), None);
        st.max_gap = "0".into();
        let (output, _) = draw(&ctx, &st, 440.0, vec![]);
        assert!(texts(&output).iter().any(|(text, _)| text.contains("Pattern Based: select reference markers, then the target last")));
        assert!(!labels
            .iter()
            .any(|(text, _)| text.contains("Preview") || text.contains("Apply")));
    }

    #[test]
    fn headless_snapshot_uses_the_existing_cpu_painter() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let (output, _) = draw(&ctx, &state(), 440.0, vec![]);
        let labels = texts(&output);
        for name in [
            "Fill Gaps",
            "Max. Gap Size",
            "Interpolation",
            "Fill Target",
            "Smooth",
            "Max. Freq (Hz)",
            "75",
            "6",
        ] {
            assert!(
                labels.iter().any(|(text, _)| text == name),
                "missing {name}"
            );
        }
        save_snapshot(&ctx, output);
    }

    #[test]
    fn wide_pane_keeps_controls_and_dispatches_fill() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let state = state();
        let (output, _) = draw(&ctx, &state, 1200.0, vec![]);
        let labels = texts(&output);
        for name in [
            "Fill Gaps",
            "Max. Gap Size",
            "Interpolation",
            "Fill Target",
            "Smooth",
            "Max. Freq (Hz)",
        ] {
            assert!(
                labels.iter().any(|(text, _)| text == name),
                "missing {name}"
            );
        }
        let at = labels
            .iter()
            .find(|(text, _)| text == "Fill All")
            .unwrap()
            .1
            .center();
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        draw(&ctx, &state, 1200.0, vec![Event::PointerMoved(at)]);
        draw(&ctx, &state, 1200.0, vec![button(true)]);
        assert_eq!(
            draw(&ctx, &state, 1200.0, vec![button(false)]).1,
            Some(EditToolsAction::Fill(FillScope::SelectedTracksAllFrames))
        );
    }

    #[test]
    fn wide_pattern_explains_missing_references_and_target_is_read_only() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut st = state();
        st.interpolation = FillMethod::PatternBased;
        st.selected_tracks.pop();
        let (output, _) = draw(&ctx, &st, 1200.0, vec![]);
        let labels = texts(&output);
        assert!(labels.iter().any(|(text, _)| text.contains("Pattern Based: select reference markers, then the target last")));
        let at = labels.iter().find(|(text, _)| text == "LASI").unwrap().1.center();
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        draw(&ctx, &st, 1200.0, vec![Event::PointerMoved(at)]);
        draw(&ctx, &st, 1200.0, vec![button(true)]);
        assert_eq!(draw(&ctx, &st, 1200.0, vec![button(false)]).1, None);
    }
}
