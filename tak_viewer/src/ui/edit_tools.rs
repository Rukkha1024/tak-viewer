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
            0 | 1 => Some("기준 마커를 먼저 고르고 채울 마커를 마지막에 골라 주세요."),
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
            return Some("Max. Gap Size에 1 이상의 정수를 써 주세요.");
        }
        if scope == FillScope::Everything {
            if method == FillMethod::PatternBased {
                return Some("Pattern Based에서는 Fill Everything을 쓸 수 없어요.");
            }
            return (self.editable_track_count == 0)
                .then_some("편집할 수 있는 마커 줄이 없어요.");
        }
        if method == FillMethod::PatternBased {
            if let Some(reason) = self.pattern_selection_reason() {
                return Some(reason);
            }
        }
        if self.selected_tracks.is_empty() {
            return Some("마커 줄을 선택해 주세요.");
        }
        if scope == FillScope::SelectedRange && self.range.is_none() {
            return Some("프레임 범위를 선택해 주세요.");
        }
        None
    }

    pub fn smooth_reason(&self, scope: SmoothScope) -> Option<&str> {
        if let Some(reason) = self.blocked_reason.as_deref() {
            return Some(reason);
        }
        if self.sample_rate_hz.is_none() {
            return Some("프레임 속도를 확인하려면 파일을 먼저 열어 주세요.");
        }
        if self.parsed_ui_cutoff().is_none() {
            return Some("Max. Freq는 0보다 크고 프레임 속도의 절반보다 작아야 해요.");
        }
        if matches!(
            scope,
            SmoothScope::AllTracksSelectedRange | SmoothScope::AllTracksAllFrames
        ) {
            if self.editable_track_count == 0 {
                return Some("편집할 수 있는 마커 줄이 없어요.");
            }
            if scope == SmoothScope::AllTracksSelectedRange && self.range.is_none() {
                return Some("프레임 범위를 선택해 주세요.");
            }
            return None;
        }
        if self.selected_tracks.is_empty() {
            return Some("마커 줄을 선택해 주세요.");
        }
        if scope == SmoothScope::SelectedRange && self.range.is_none() {
            return Some("프레임 범위를 선택해 주세요.");
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

struct ToolButton<'a> {
    name: &'static str,
    shortcut: Option<&'static str>,
    reason: Option<&'a str>,
    action: EditToolsAction,
}

fn fill_buttons(state: &EditToolsState) -> [ToolButton<'_>; 3] {
    let key = match state.interpolation {
        FillMethod::Cubic => "C",
        FillMethod::PatternBased => "Shift+C",
    };
    [
        ("Fill Selected", Some(key), FillScope::SelectedRange),
        ("Fill All", None, FillScope::SelectedTracksAllFrames),
        ("Fill Everything", None, FillScope::Everything),
    ].map(|(name, shortcut, scope)| ToolButton {
        name, shortcut, reason: state.fill_reason_for(scope, state.interpolation),
        action: EditToolsAction::Fill(scope),
    })
}

fn smooth_buttons(state: &EditToolsState) -> [ToolButton<'_>; 3] {
    [
        ("Smooth Selection", Some("X"), SmoothScope::SelectedRange),
        ("Smooth Track", None, SmoothScope::SelectedTracksAllFrames),
        ("Smooth All Tracks", Some("Ctrl+Alt+X"), SmoothScope::AllTracksAllFrames),
    ].map(|(name, shortcut, scope)| ToolButton {
        name, shortcut, reason: state.smooth_reason(scope),
        action: EditToolsAction::Smooth(scope),
    })
}

fn tool_buttons(
    ui: &mut Ui,
    buttons: [ToolButton<'_>; 3],
    size: egui::Vec2,
    action: &mut Option<EditToolsAction>,
) {
    for button in buttons {
        let response = ui.add_enabled(
            button.reason.is_none(),
            egui::Button::new(RichText::new(button.name).size(11.0)).min_size(size),
        );
        if response.clicked() {
            *action = Some(button.action);
        }
        if let Some(why) = button.reason {
            response.on_disabled_hover_ui(|ui| {
                if let Some(key) = button.shortcut {
                    ui.label(format!("{} ({key})", button.name));
                }
                ui.label(why);
            });
        } else if let Some(key) = button.shortcut {
            response.on_hover_ui(|ui| { ui.label(format!("{} ({key})", button.name)); });
        }
    }
}

fn tool_row(ui: &mut Ui, buttons: [ToolButton<'_>; 3], action: &mut Option<EditToolsAction>) {
    let width = ui.available_width();
    if width < 330.0 {
        tool_buttons(ui, buttons, egui::vec2(width, 32.0), action);
    } else {
        let button_width = (width - 2.0 * ui.spacing().item_spacing.x) / 3.0;
        ui.horizontal(|ui| tool_buttons(ui, buttons, egui::vec2(button_width, 32.0), action));
    }
}

fn interpolation_picker(ui: &mut Ui, state: &EditToolsState, width: f32, action: &mut Option<EditToolsAction>) {
    egui::ComboBox::from_id_salt("fill_interpolation")
        .selected_text(match state.interpolation {
            FillMethod::Cubic => "Cubic",
            FillMethod::PatternBased => "Pattern Based",
        })
        .width(width)
        .truncate()
        .show_ui(ui, |ui| {
            for (method, name) in [
                (FillMethod::Cubic, "Cubic"),
                (FillMethod::PatternBased, "Pattern Based"),
            ] {
                if ui.selectable_label(state.interpolation == method, name).clicked() {
                    *action = Some(EditToolsAction::SetInterpolation(method));
                }
            }
        });
}

fn value_field(ui: &mut Ui, value: &str, width: f32) -> Option<String> {
    let mut value = value.to_owned();
    ui.add(egui::TextEdit::singleline(&mut value)
        .font(egui::FontId::proportional(12.0))
        .min_size(egui::vec2(0.0, ui.spacing().interact_size.y))
        .desired_width(width))
        .changed().then_some(value)
}

/// Keep a short layout for the wide dock above the graph.
fn show_compact(ui: &mut Ui, state: &EditToolsState) -> Option<EditToolsAction> {
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Fill Gaps").strong());
        ui.label("Interpolation");
        interpolation_picker(ui, state, 112.0, &mut action);
        ui.label("Max. Gap Size");
        if let Some(value) = value_field(ui, &state.max_gap, 42.0) {
            action = Some(EditToolsAction::SetMaxGap(value));
        }
        ui.label("Fill Target");
        ui.label(target_text(state));
        tool_buttons(ui, fill_buttons(state), egui::Vec2::ZERO, &mut action);
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Smooth").strong());
        ui.label("Max. Freq (Hz)");
        if let Some(value) = value_field(ui, &state.max_frequency, 48.0) {
            action = Some(EditToolsAction::SetMaxFrequency(value));
        }
        tool_buttons(ui, smooth_buttons(state), egui::Vec2::ZERO, &mut action);
    });
    action
}

fn settings_label(ui: &mut Ui, width: f32, label: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 24.0),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| ui.label(RichText::new(label).size(11.0)),
    );
}

fn group_title(ui: &mut Ui, title: &str) -> f32 {
    let width = ui.available_width();
    ui.set_min_width(width);
    ui.label(RichText::new(title).color(theme::TEXT_DIM).size(11.0));
    ui.add_space(4.0);
    (width * 0.43).clamp(70.0, 144.0)
}

fn show_standard(ui: &mut Ui, state: &EditToolsState) -> Option<EditToolsAction> {
    let mut action = None;
    ui.spacing_mut().item_spacing.x = 8.0;
    ui.spacing_mut().interact_size.y = 24.0;
    ui.label(RichText::new("Fill Gaps").size(14.0));
    tool_row(ui, fill_buttons(state), &mut action);

    ui.add_space(12.0);
    egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
        let label_width = group_title(ui, "Settings");
        ui.horizontal(|ui| {
            settings_label(ui, label_width, "Max. Gap Size");
            if let Some(value) = value_field(ui, &state.max_gap, ui.available_width()) {
                action = Some(EditToolsAction::SetMaxGap(value));
            }
        });
        ui.horizontal(|ui| {
            settings_label(ui, label_width, "Interpolation");
            interpolation_picker(ui, state, ui.available_width(), &mut action);
        });
        ui.horizontal(|ui| {
            settings_label(ui, label_width, "Fill Target");
            let target = target_text(state);
            ui.add(egui::Label::new(RichText::new(&target).size(12.0)).truncate()).on_hover_text(target);
        });
        ui.label(RichText::new("Fill Target is used by Pattern Based.").size(11.0).color(theme::TEXT_DIM));
    });

    ui.add_space(20.0);
    ui.label(RichText::new("Smooth").size(14.0));
    tool_row(ui, smooth_buttons(state), &mut action);

    ui.add_space(12.0);
    egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
        let label_width = group_title(ui, "Filter Cutoff Frequency");
        ui.horizontal(|ui| {
            settings_label(ui, label_width, "Max. Freq (Hz)");
            if let Some(value) = value_field(ui, &state.max_frequency, ui.available_width()) {
                action = Some(EditToolsAction::SetMaxFrequency(value));
            }
        });
    });
    action
}

/// Draw the pane and return at most one user action. Execution happens in the caller.
pub fn show(ui: &mut Ui, state: &EditToolsState) -> Option<EditToolsAction> {
    if ui.available_width() >= 740.0 {
        return show_compact(ui, state);
    }
    egui::ScrollArea::vertical()
        .id_salt("edit_tools_pane")
        .show(ui, |ui| egui::Frame::none().inner_margin(egui::Margin::same(16.0))
            .show(ui, |ui| show_standard(ui, state)).inner)
        .inner
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
            Some("기준 마커를 먼저 고르고 채울 마커를 마지막에 골라 주세요."));
        assert_eq!(target_text(&st), "LASI");
        st.selected_tracks.clear();
        assert_eq!(target_text(&st), "—");
        assert_eq!(st.fill_reason_for(FillScope::SelectedRange, st.interpolation),
            Some("기준 마커를 먼저 고르고 채울 마커를 마지막에 골라 주세요."));
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
    fn headless_controls_emit_immediate_scope_actions() {
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
        assert!(!labels.iter().any(|(text, _)| text.contains("Shift+C")));
        assert!(!labels.iter().any(|(text, _)| text.contains("기준 마커를 먼저 고르고 채울 마커를 마지막에 골라 주세요.")));
        assert_eq!(click(&ctx, &st, "Fill Selected"), None);
        assert_eq!(click(&ctx, &st, "Fill All"), None);
        assert!(!labels
            .iter()
            .any(|(text, _)| text.contains("Pattern Based에서는 Fill Everything을 쓸 수 없어요")));
        assert_eq!(click(&ctx, &st, "Fill Everything"), None);
        st.max_gap = "0".into();
        let (output, _) = draw(&ctx, &st, 440.0, vec![]);
        assert!(!texts(&output).iter().any(|(text, _)| text.contains("기준 마커를 먼저 고르고 채울 마커를 마지막에 골라 주세요.")));
        assert!(!labels
            .iter()
            .any(|(text, _)| text.contains("Preview") || text.contains("Apply")));
    }

    #[test]
    fn disabled_fill_shows_its_korean_reason_and_shortcut_on_hover() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        ctx.style_mut(|style| style.interaction.tooltip_delay = 0.0);
        let mut st = state();
        st.range = None;
        let why = st.fill_reason_for(FillScope::SelectedRange, st.interpolation).unwrap();
        let (out, _) = draw(&ctx, &st, 376.0, vec![]);
        let labels = texts(&out);
        assert!(!labels.iter().any(|(text, _)| text == why || text == "Fill Selected (C)"));
        let pos = labels.iter().find(|(text, _)| text == "Fill Selected").unwrap().1.center();
        draw(&ctx, &st, 376.0, vec![Event::PointerMoved(pos)]);
        let (out, _) = draw(&ctx, &st, 376.0, vec![]);
        let labels = texts(&out);
        for tip in [why, "Fill Selected (C)"] {
            assert!(labels.iter().any(|(text, _)| text == tip), "missing tooltip {tip}");
        }
    }

    #[test]
    fn default_width_keeps_equal_buttons_in_each_action_row() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let (out, _) = draw(&ctx, &state(), 376.0 + 16.0, vec![]);
        let labels = texts(&out);
        for names in [
            ["Fill Selected", "Fill All", "Fill Everything"],
            ["Smooth Selection", "Smooth Track", "Smooth All Tracks"],
        ] {
            let buttons = names.map(|name| {
                let center = labels.iter().find(|(text, _)| text == name).unwrap().1.center();
                out.shapes.iter().find_map(|s| match &s.shape {
                    egui::Shape::Rect(r) if r.rect.contains(center)
                        && (r.rect.height() - 32.0).abs() < 1.0 => Some(r.rect),
                    _ => None,
                }).unwrap_or_else(|| panic!("missing button background for {name}"))
            });
            for pair in buttons.windows(2) {
                assert_eq!(pair[0].top(), pair[1].top());
                assert!((pair[0].width() - pair[1].width()).abs() < 1.0);
                assert!((pair[1].left() - pair[0].right() - 8.0).abs() < 1.0);
            }
        }
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
    fn wide_pattern_target_remains_read_only_without_inline_reason() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut st = state();
        st.interpolation = FillMethod::PatternBased;
        st.selected_tracks.pop();
        let (output, _) = draw(&ctx, &st, 1200.0, vec![]);
        let labels = texts(&output);
        assert!(!labels.iter().any(|(text, _)| text.contains("기준 마커를 먼저 고르고 채울 마커를 마지막에 골라 주세요.")));
        let at = labels.iter().find(|(text, _)| text == "LASI").unwrap().1.center();
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        draw(&ctx, &st, 1200.0, vec![Event::PointerMoved(at)]);
        draw(&ctx, &st, 1200.0, vec![button(true)]);
        assert_eq!(draw(&ctx, &st, 1200.0, vec![button(false)]).1, None);
    }
}
