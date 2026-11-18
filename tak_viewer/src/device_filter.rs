//! Per-file, per-device display processing. Never mutates Take or edit/save state.
use eframe::egui::{self, Color32, RichText, Ui};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc::{self, Receiver},
    Arc, Mutex,
};
use tak_core::{
    signal::{self, Step},
    view_data::{SignalView, ViewData},
};

pub type SharedFilters = Arc<Mutex<Filters>>;
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub show_raw: bool,
    pub steps: Vec<Step>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            show_raw: true,
            steps: Vec::new(),
        }
    }
}
#[derive(Default)]
pub struct Filters {
    pub devices: HashMap<usize, Device>,
}
pub struct Device {
    pub settings: Settings,
    pub error: Option<String>,
    pub ready: Option<Arc<Output>>,
    requested: Option<(u64, Vec<Step>)>,
    cache: std::collections::VecDeque<((u64, Vec<Step>), Arc<Output>)>,
    generation: Arc<AtomicUsize>,
    running: Option<Receiver<(usize, Result<Arc<Output>, String>)>>,
}
impl Default for Device {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            error: None,
            ready: None,
            requested: None,
            cache: std::collections::VecDeque::new(),
            generation: Arc::default(),
            running: None,
        }
    }
}
pub struct Output {
    pub channels: Vec<Arc<SignalView>>,
    pub bands: Vec<Option<(Arc<SignalView>, Arc<SignalView>)>>,
    pub badge: String,
    pub skipped_runs: usize,
}
#[derive(Clone, Default)]
pub struct Display {
    pub output: Option<Arc<Output>>,
    pub show_raw: bool,
}
impl Drop for Device {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}
impl Device {
    pub fn pending(&self) -> bool {
        self.running.is_some()
    }
    pub fn display(&self) -> Display {
        Display {
            output: (self.settings.enabled && !self.settings.steps.is_empty())
                .then(|| self.ready.clone())
                .flatten(),
            show_raw: self.settings.show_raw,
        }
    }
    /// Bounded UI work: poll one result, compare small settings, clone one Arc. One running
    /// worker per device; an obsolete job is cancelled and the latest starts after it exits.
    pub fn update(&mut self, data: &Arc<ViewData>, di: usize, ctx: &egui::Context) {
        if let Some(rx) = &self.running {
            match rx.try_recv() {
                Ok((generation, result)) => {
                    self.running = None;
                    if generation == self.generation.load(Ordering::Relaxed) {
                        match result {
                            Ok(output) => {
                                if let Some(key) = self.requested.clone() {
                                    self.cache.push_back((key, output.clone()));
                                    if self.cache.len() > 4 {
                                        crate::views::drop_later(self.cache.pop_front());
                                    }
                                }
                                crate::views::drop_later(self.ready.replace(output));
                                self.error = None;
                            }
                            Err(error) => self.error = Some(error),
                        }
                    } else {
                        crate::views::drop_later(Some(result));
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.running = None;
                    self.error = Some("Processing worker stopped".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let fs = sample_rate(data, di);
        let validation = validate_settings(&self.settings, fs);
        // Forceplate samples are immutable and already complete before this path. A later
        // demand-only EMG attachment must not recompute that device's cached filter.
        let source_generation = if data.devices[di].plate.is_some() {
            0
        } else {
            data.analog_generation()
        };
        let key = (source_generation, self.settings.steps.clone());
        if !self.settings.enabled || self.settings.steps.is_empty() || validation.is_err() {
            if let Err(error) = validation {
                self.error = Some(error);
            }
            if self.running.is_some() {
                self.generation.fetch_add(1, Ordering::Relaxed);
                self.requested = None;
            }
            return;
        }
        if self.requested.as_ref() == Some(&key) {
            if self.cache.iter().any(|(old, _)| old == &key) {
                self.error = None;
            }
            return;
        }
        if self.running.is_some() {
            self.generation.fetch_add(1, Ordering::Relaxed);
            self.requested = None;
            ctx.request_repaint();
            return;
        }
        if let Some((_, output)) = self.cache.iter().find(|(old, _)| old == &key) {
            crate::views::drop_later(self.ready.replace(output.clone()));
            self.requested = Some(key);
            self.error = None;
            return;
        }
        self.error = None;
        let Some(fs) = fs else { return };
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let cancelled = self.generation.clone();
        let data = data.clone();
        let steps = self.settings.steps.clone();
        let context = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.running = Some(rx);
        self.requested = Some(key);
        std::thread::spawn(move || {
            let result = process_device(&data, di, fs, &steps, || {
                cancelled.load(Ordering::Relaxed) != generation
            })
            .map(Arc::new);
            let _ = tx.send((generation, result));
            context.request_repaint();
        });
    }
}
pub fn sample_rate(data: &ViewData, di: usize) -> Option<f64> {
    let device = data.devices.get(di)?;
    if device.plate.is_some() {
        (data.take.frame_rate > 0.0).then_some(data.take.frame_rate)
    } else {
        let first = device.channels.first()?.sample_rate_hz?;
        device
            .channels
            .iter()
            .all(|channel| channel.sample_rate_hz == Some(first))
            .then_some(first)
    }
}
pub fn validate_settings(settings: &Settings, fs: Option<f64>) -> Result<(), String> {
    if settings.steps.is_empty() {
        return Ok(());
    }
    signal::validate(&settings.steps, fs.ok_or("Sample rate unavailable")?)
}
fn process_device(
    data: &ViewData,
    di: usize,
    fs: f64,
    steps: &[Step],
    cancelled: impl Fn() -> bool,
) -> Result<Output, String> {
    let device = &data.devices[di];
    let mut channels = Vec::new();
    let mut bands = Vec::new();
    let mut skipped_runs = 0;
    for (ci, channel) in device.channels.iter().enumerate() {
        if cancelled() {
            return Err("Superseded".into());
        }
        if channel.sample_rate_hz != Some(fs) {
            return Err("Channels have different sample rates".into());
        }
        let (raw, count) = if device.plate.is_some() {
            (channel.per_frame.iter().map(|v| *v as f64).collect(), 1)
        } else {
            (
                channel
                    .full_rate(
                        &data.take.analog[di].channels[ci],
                        data.start_frame,
                        data.end_frame,
                    )
                    .ok_or("Sample layout unavailable")?,
                channel.samples_per_frame,
            )
        };
        let result = signal::process_cancellable(&raw, fs, steps, &cancelled)?;
        skipped_runs += result.skipped_runs;
        let reduced = tak_core::view_data::reduce_full_rate(
            &result.values,
            count,
            data.start_frame,
            data.end_frame,
        );
        bands.push((count > 1).then_some((reduced.min, reduced.max)));
        channels.push(reduced.signal);
    }
    Ok(Output {
        channels,
        bands,
        badge: summary(steps),
        skipped_runs,
    })
}
pub fn summary(steps: &[Step]) -> String {
    let text = steps
        .iter()
        .map(|step| match step {
            Step::Demean => "Mean−".into(),
            Step::Rectify => "Rect".into(),
            Step::LowPass { cutoff_hz, .. } => format!("LP {cutoff_hz}"),
            Step::HighPass { cutoff_hz, .. } => format!("HP {cutoff_hz}"),
            Step::BandPass {
                low_hz, high_hz, ..
            } => format!("BP {low_hz}–{high_hz}"),
            Step::BandStop {
                low_hz, high_hz, ..
            } => format!("BS {low_hz}–{high_hz}"),
            Step::MovingRms { window_ms } => format!("RMS {window_ms} ms"),
        })
        .collect::<Vec<String>>()
        .join(" · ");
    if text.chars().count() > 36 {
        format!("{}…", text.chars().take(35).collect::<String>())
    } else {
        text
    }
}
const NAMES: [&str; 7] = [
    "Demean",
    "Low-pass",
    "High-pass",
    "Band-pass",
    "Band-stop",
    "Rectify",
    "Moving RMS",
];
pub fn new_step(kind: usize, fs: Option<f64>) -> Step {
    let top = fs
        .filter(|fs| fs.is_finite() && *fs > 0.0)
        .map_or(500.0, |fs| fs * 0.49);
    let low = |v: f64| v.min(top * 0.5);
    let high = |v: f64| v.min(top);
    match kind {
        0 => Step::Demean,
        1 => Step::LowPass {
            cutoff_hz: high(6.0),
            order: 4,
            zero_phase: true,
        },
        2 => Step::HighPass {
            cutoff_hz: high(20.0),
            order: 4,
            zero_phase: true,
        },
        3 => Step::BandPass {
            low_hz: low(20.0),
            high_hz: high(450.0),
            order: 4,
            zero_phase: true,
        },
        4 => Step::BandStop {
            low_hz: low(59.0),
            high_hz: high(61.0),
            order: 4,
            zero_phase: true,
        },
        5 => Step::Rectify,
        _ => Step::MovingRms { window_ms: 50.0 },
    }
}
/// Each step remains one readable sentence. Horizontal scrolling preserves it on narrow panes.
pub fn controls(ui: &mut Ui, device: &mut Device, fs: Option<f64>) {
    let _timing = crate::bench_timing::enabled().then(|| crate::bench_timing::span("device_filter.controls"));
    let computing = device.pending();
    ui.horizontal(|ui| {
        ui.checkbox(&mut device.settings.enabled, "Filter");
        ui.label(
            RichText::new(fs.map_or("fs = ? Hz".into(), |v| format!("fs = {v} Hz")))
                .small()
                .color(crate::ui::theme::TEXT_DIM),
        );
        ui.checkbox(&mut device.settings.show_raw, "Show raw");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_sized(
                [96.0, 18.0],
                egui::Label::new(
                    RichText::new(if computing { "◌ Computing" } else { "" })
                        .small()
                        .weak(),
                )
                .selectable(false),
            );
        });
    });
    let mut remove = None;
    let mut swap = None;
    let len = device.settings.steps.len();
    egui::ScrollArea::horizontal()
        .id_salt("filter_rows")
        .show(ui, |ui| {
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for style in [
                    egui::TextStyle::Body,
                    egui::TextStyle::Button,
                    egui::TextStyle::Monospace,
                ] {
                    ui.style_mut()
                        .text_styles
                        .insert(style, egui::FontId::proportional(10.0));
                }
                egui::Grid::new("steps")
                    .num_columns(5)
                    .spacing([4.0, 3.0])
                    .show(ui, |ui| {
                        for (i, step) in device.settings.steps.iter_mut().enumerate() {
                            let name = match step {
                                Step::Demean => NAMES[0],
                                Step::LowPass { .. } => NAMES[1],
                                Step::HighPass { .. } => NAMES[2],
                                Step::BandPass { .. } => NAMES[3],
                                Step::BandStop { .. } => NAMES[4],
                                Step::Rectify => NAMES[5],
                                Step::MovingRms { .. } => NAMES[6],
                            };
                            ui.label(name);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().interact_size.x = 18.0;
                                match step {
                                    Step::MovingRms { window_ms } => number(
                                        ui,
                                        window_ms,
                                        " ms",
                                        !window_ms.is_finite() || *window_ms <= 0.0,
                                    ),
                                    Step::LowPass { cutoff_hz, .. }
                                    | Step::HighPass { cutoff_hz, .. } => {
                                        number(ui, cutoff_hz, " Hz", cutoff_invalid(*cutoff_hz, fs))
                                    }
                                    Step::BandPass {
                                        low_hz, high_hz, ..
                                    }
                                    | Step::BandStop {
                                        low_hz, high_hz, ..
                                    } => {
                                        number(
                                            ui,
                                            low_hz,
                                            "",
                                            cutoff_invalid(*low_hz, fs) || *low_hz >= *high_hz,
                                        );
                                        ui.label("–");
                                        number(
                                            ui,
                                            high_hz,
                                            " Hz",
                                            cutoff_invalid(*high_hz, fs) || *low_hz >= *high_hz,
                                        );
                                    }
                                    _ => {
                                        ui.label("");
                                    }
                                }
                            });
                            match step {
                                Step::LowPass {
                                    order, zero_phase, ..
                                }
                                | Step::HighPass {
                                    order, zero_phase, ..
                                }
                                | Step::BandPass {
                                    order, zero_phase, ..
                                }
                                | Step::BandStop {
                                    order, zero_phase, ..
                                } => {
                                    ui.horizontal(|ui| {
                                        ui.label("order");
                                        if !(1..=8).contains(order) {
                                            ui.visuals_mut().override_text_color =
                                                Some(Color32::LIGHT_RED);
                                        }
                                        typed(ui, order, !(1..=8).contains(order), 28.0);
                                    });
                                    ui.checkbox(zero_phase, "Zero-lag");
                                }
                                _ => {
                                    ui.label("");
                                    ui.label("");
                                }
                            }
                            ui.horizontal(|ui| {
                                ui.spacing_mut().button_padding = egui::vec2(1.0, 0.0);
                                if ui
                                    .add_enabled(i > 0, egui::Button::new("↑").small())
                                    .clicked()
                                {
                                    swap = Some((i, i - 1));
                                }
                                if ui
                                    .add_enabled(i + 1 < len, egui::Button::new("↓").small())
                                    .clicked()
                                {
                                    swap = Some((i, i + 1));
                                }
                                if ui.small_button("×").clicked() {
                                    remove = Some(i);
                                }
                            });
                            ui.end_row();
                            let error = signal::validate(
                                std::slice::from_ref(step),
                                fs.unwrap_or(f64::NAN),
                            )
                            .err()
                            .unwrap_or_default();
                            // Paint across the reserved row without widening the stage-name column.
                            let (slot, _) =
                                ui.allocate_exact_size(egui::vec2(1.0, 14.0), egui::Sense::hover());
                            ui.painter().text(
                                slot.left_center(),
                                egui::Align2::LEFT_CENTER,
                                error,
                                egui::FontId::proportional(9.0),
                                Color32::LIGHT_RED,
                            );
                            for _ in 0..4 {
                                ui.label("");
                            }
                            ui.end_row();
                        }
                    });
            });
        });
    if let Some(i) = remove {
        device.settings.steps.remove(i);
    } else if let Some((a, b)) = swap {
        device.settings.steps.swap(a, b);
    }
    ui.menu_button("+ Add", |ui| {
        for (i, name) in NAMES.iter().enumerate() {
            if ui.button(*name).clicked() {
                device.settings.steps.push(new_step(i, fs));
                device.settings.enabled = true;
                ui.close_menu();
            }
        }
    });
    ui.label(
        RichText::new("Display only · file values unchanged")
            .small()
            .color(crate::ui::theme::TEXT_DIM),
    );
    if computing {
        ui.ctx().request_repaint();
    }
    let error = device
        .error
        .as_deref()
        .filter(|_| signal::validate(&device.settings.steps, fs.unwrap_or(f64::NAN)).is_ok())
        .unwrap_or("");
    ui.add_sized(
        [ui.available_width(), 14.0],
        egui::Label::new(RichText::new(error).small().color(Color32::LIGHT_RED)).truncate(),
    );
    let short = device
        .ready
        .as_ref()
        .filter(|o| o.skipped_runs > 0)
        .map_or(String::new(), |o| {
            format!("{} short runs unchanged", o.skipped_runs)
        });
    ui.add_sized(
        [ui.available_width(), 14.0],
        egui::Label::new(RichText::new(short).small().weak()).truncate(),
    );
}
fn cutoff_invalid(value: f64, fs: Option<f64>) -> bool {
    !value.is_finite() || value <= 0.0 || fs.is_none_or(|fs| value >= fs / 2.0)
}
#[derive(Clone)]
struct NumericDraft {
    text: String,
    source: String,
}
fn typed<T: Copy + ToString + std::str::FromStr>(
    ui: &mut Ui,
    value: &mut T,
    invalid: bool,
    width: f32,
) {
    let id = ui.next_auto_id();
    let source = value.to_string();
    let mut draft = ui
        .ctx()
        .data_mut(|d| d.get_temp::<NumericDraft>(id))
        .filter(|d| d.source == source)
        .unwrap_or_else(|| NumericDraft {
            text: source.clone(),
            source,
        });
    let bad_text = draft.text.parse::<T>().is_err();
    let response = ui.add(
        egui::TextEdit::singleline(&mut draft.text)
            .desired_width(width)
            .text_color(if invalid || bad_text {
                Color32::LIGHT_RED
            } else {
                crate::ui::theme::TEXT
            }),
    );
    if response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        draft.text = draft.source.clone();
        response.surrender_focus();
    } else if response.lost_focus()
        || response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
    {
        if let Ok(parsed) = draft.text.parse::<T>() {
            *value = parsed;
            draft.source = value.to_string();
        }
        response.surrender_focus();
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, draft));
}
fn number(ui: &mut Ui, value: &mut f64, suffix: &str, invalid: bool) {
    typed(ui, value, invalid, 48.0);
    if !suffix.is_empty() {
        ui.label(suffix.trim());
    }
}
pub fn badge(ui: &mut Ui, text: &str) {
    egui::Frame::none()
        .fill(Color32::from_rgb(9, 71, 113))
        .rounding(2.0)
        .inner_margin(egui::Margin::symmetric(4.0, 1.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.5).color(Color32::WHITE));
        });
}
/// Agent-only chain syntax, same freely ordered steps as the UI. `:c` selects causal mode.
pub fn parse_chain(text: &str) -> Result<Vec<Step>, String> {
    if text.is_empty() || text == "raw" {
        return Ok(Vec::new());
    }
    text.split(',')
        .map(|part| {
            let p: Vec<_> = part.split(':').collect();
            let number = |i: usize| {
                p.get(i)
                    .ok_or("Missing number")?
                    .parse::<f64>()
                    .map_err(|_| "Invalid number")
            };
            let order = |i: usize| {
                p.get(i)
                    .ok_or("Missing order")?
                    .parse::<usize>()
                    .map_err(|_| "Invalid order")
            };
            let zero = !p.contains(&"c") && !p.contains(&"causal");
            Ok(match p[0] {
                "demean" => Step::Demean,
                "rect" => Step::Rectify,
                "rms" => Step::MovingRms {
                    window_ms: number(1)?,
                },
                "lp" => Step::LowPass {
                    cutoff_hz: number(1)?,
                    order: order(2)?,
                    zero_phase: zero,
                },
                "hp" => Step::HighPass {
                    cutoff_hz: number(1)?,
                    order: order(2)?,
                    zero_phase: zero,
                },
                "bp" => Step::BandPass {
                    low_hz: number(1)?,
                    high_hz: number(2)?,
                    order: order(3)?,
                    zero_phase: zero,
                },
                "bs" => Step::BandStop {
                    low_hz: number(1)?,
                    high_hz: number(2)?,
                    order: order(3)?,
                    zero_phase: zero,
                },
                _ => return Err("Unknown step".into()),
            })
        })
        .collect()
}

#[derive(Default)]
pub struct Probe {
    pub chains: Vec<(String, String)>,
    pub open: Option<String>,
    pub bypass: bool,
}
pub static PROBE: std::sync::OnceLock<Probe> = std::sync::OnceLock::new();
pub fn take_probe(args: &mut Vec<String>) -> Result<(), String> {
    let mut probe = Probe::default();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--device-chain" | "--device-open" => {
                let flag = args[i].clone();
                let value = args.get(i + 1).ok_or("Missing device argument")?.clone();
                if flag == "--device-chain" {
                    let (device, chain) = value.split_once('=').ok_or("Use device=lp:6:4")?;
                    parse_chain(chain)?;
                    probe.chains.push((device.into(), chain.into()));
                } else {
                    probe.open = Some(value);
                }
                args.drain(i..i + 2);
            }
            "--device-bypass" => {
                probe.bypass = true;
                args.remove(i);
            }
            _ => i += 1,
        }
    }
    if (!probe.chains.is_empty() || probe.open.is_some())
        && !args
            .iter()
            .any(|a| matches!(a.as_str(), "--bench" | "--screenshot"))
    {
        return Err("Device probe requires --bench or --screenshot".into());
    }
    let _ = PROBE.set(probe);
    Ok(())
}
pub fn find_device(data: &ViewData, want: &str) -> Option<usize> {
    data.devices
        .iter()
        .position(|d| d.title == want)
        .or_else(|| {
            want.parse::<usize>()
                .ok()
                .filter(|i| *i < data.devices.len())
        })
}
pub fn apply_probe(view: &mut crate::views::View, ctx: &egui::Context, only: bool) -> bool {
    let Some(probe) = PROBE.get() else {
        return false;
    };
    let Some(data) = &view.data else { return false };
    let mut open = None;
    for (want, chain) in &probe.chains {
        if let Some(di) = find_device(data, want) {
            let mut filters = view.filters.lock().unwrap();
            let device = filters.devices.entry(di).or_default();
            device.settings.steps = parse_chain(chain).unwrap();
            device.settings.enabled = !probe.bypass;
            open = Some(di);
            if data.devices[di].plate.is_none() {
                view.devices.request_emg = true;
            }
        } else {
            eprintln!("Unknown device: {want}");
        }
    }
    if let Some(want) = &probe.open {
        open = find_device(data, want);
    }
    if let Some(di) = open {
        view.devices.open_device = Some(di);
        view.devices.only_device = only;
        if data.devices[di].plate.is_none() {
            view.devices.request_emg = true;
        }
    }
    view.request_emg(ctx);
    open.is_some()
}
/// Agent-only, read-only export: --dump-device-signal file device channel chain out.json.
pub fn dump(args: &[String]) -> Result<(), String> {
    if args.len() != 5 {
        return Err("Use --dump-device-signal file device channel chain out.json".into());
    }
    let input = crate::workspace::resolve_path(&args[0]);
    let out = crate::workspace::resolve_path(&args[4]);
    if tak_core::tak::is_take_path(&out) || tak_core::takm::is_takm_path(&out) {
        return Err("Output cannot be TAK/TAKM".into());
    }
    let data = ViewData::build(tak_core::tak::read_take(&input).map_err(|e| e.to_string())?);
    let di = find_device(&data, &args[1]).ok_or("Device not found")?;
    let ci = args[2].parse::<usize>().map_err(|_| "Invalid channel")?;
    let channel = data.devices[di]
        .channels
        .get(ci)
        .ok_or("Channel not found")?;
    let fs = sample_rate(&data, di).ok_or("Sample rate unavailable")?;
    let steps = parse_chain(&args[3])?;
    let raw = if data.devices[di].plate.is_some() {
        channel.per_frame.iter().map(|v| *v as f64).collect()
    } else {
        channel
            .full_rate(
                &data.take.analog[di].channels[ci],
                data.start_frame,
                data.end_frame,
            )
            .ok_or("Sample layout unavailable")?
    };
    let result = signal::process(&raw, fs, &steps)?;
    let output = serde_json::json!({"fs":fs,"samples_per_frame":if data.devices[di].plate.is_some(){1}else{channel.samples_per_frame},"device":data.devices[di].title,"channel":channel.name,"chain":args[3],"raw":raw,"processed":result.values,"skipped_runs":result.skipped_runs});
    std::fs::write(out, serde_json::to_vec(&output).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data() -> Arc<ViewData> {
        let mut take = crate::views::sample_take("filter.tak", &["A"]);
        take.start_frame = 0;
        take.end_frame = 199;
        take.frame_rate = 1000.0;
        take.device_sampling = vec![Some(tak_core::tak::DeviceSampling {
            rate_hz: 1000.0,
            samples_per_frame: 1,
        })];
        take.analog = vec![tak_core::tak::AnalogDevice {
            channels: vec![tak_core::tak::AnalogChannel {
                name: "AnalogChannel0".into(),
                kind: "FloatChannel".into(),
                dim: 1,
                frames: (0..200).collect(),
                values: (0..200).map(|i| (i as f32 * 0.3).sin() - 0.2).collect(),
            }],
        }];
        Arc::new(ViewData::build(take))
    }
    fn wait(device: &mut Device, data: &Arc<ViewData>, ctx: &egui::Context) {
        let watchdog = crate::work_queue::TestWatchdog::new();
        let key = (data.analog_generation(), device.settings.steps.clone());
        loop {
            device.update(data, 0, ctx);
            let installed = device.cache.iter().any(|(cached, output)| {
                cached == &key && device.ready.as_ref().is_some_and(|ready| Arc::ptr_eq(ready, output))
            });
            if installed && !device.pending() {
                return;
            }
            watchdog.pause(|| format!("device filter worker did not finish. Generation={}, pending={}, requested={:?}, wanted={:?}, error={:?}",
                device.generation.load(Ordering::Relaxed), device.pending(), device.requested, device.settings.steps, device.error));
        }
    }
    #[test]
    fn supersedes_old_settings_bypasses_and_reuses_cached_output() {
        let data = data();
        let ctx = egui::Context::default();
        let mut device = Device::default();
        device.settings.enabled = true;
        device.settings.steps = vec![new_step(1, Some(1000.0))];
        device.update(&data, 0, &ctx);
        device.settings.steps = vec![Step::Rectify];
        device.update(&data, 0, &ctx);
        wait(&mut device, &data, &ctx);
        assert_eq!(device.ready.as_ref().unwrap().badge, "Rect");
        let saved = device.ready.clone().unwrap();
        device.settings.steps = vec![Step::Demean];
        wait(&mut device, &data, &ctx);
        device.settings.steps = vec![Step::Rectify];
        wait(&mut device, &data, &ctx);
        assert!(Arc::ptr_eq(device.ready.as_ref().unwrap(), &saved));
        device.settings.enabled = false;
        device.update(&data, 0, &ctx);
        assert!(device.display().output.is_none());
        assert_eq!(device.settings.steps, vec![Step::Rectify]);
        device.settings.enabled = true;
        device.settings.steps = vec![Step::LowPass {
            cutoff_hz: 500.0,
            order: 4,
            zero_phase: true,
        }];
        device.update(&data, 0, &ctx);
        assert!(device.error.is_some());
        assert!(Arc::ptr_eq(device.ready.as_ref().unwrap(), &saved));
        device.settings.steps = vec![Step::Rectify];
        device.update(&data, 0, &ctx);
        assert!(device.error.is_none());
    }
    #[test]
    fn parse_free_chain_and_add_values_work_at_low_sample_rates() {
        let chain = parse_chain("bp:20:450:3,rect,lp:6:7:c,lp:6:1").unwrap();
        assert_eq!(chain.len(), 4);
        assert!(summary(&chain).starts_with("BP 20–450 · Rect"));
        for kind in 0..7 {
            signal::validate(&[new_step(kind, Some(10.0))], 10.0).unwrap();
        }
    }
    #[test]
    fn mixed_channel_rates_are_rejected_before_processing() {
        let mut data = (*data()).clone();
        let mut second = data.devices[0].channels[0].clone();
        second.sample_rate_hz = Some(100.0);
        data.devices[0].channels.push(second);
        assert_eq!(sample_rate(&data, 0), None);
    }
    #[test]
    fn repeated_views_share_settings_and_separate_files_do_not() {
        let mut first = crate::views::View::default();
        let (_, rx) = mpsc::channel();
        first.show_shared(data(), rx, 1, 1.0, 0.0);
        let shared = first.shared_file().unwrap();
        let mut second = crate::views::View::default();
        second.show_shared_file(shared, 2, 1.0, 0.0);
        assert!(Arc::ptr_eq(&first.filters, &second.filters));
        first
            .filters
            .lock()
            .unwrap()
            .devices
            .entry(0)
            .or_default()
            .settings
            .steps
            .push(Step::Rectify);
        assert_eq!(
            second.filters.lock().unwrap().devices[&0].settings.steps,
            vec![Step::Rectify]
        );
        let (_, rx) = mpsc::channel();
        second.show_shared(data(), rx, 3, 1.0, 0.0);
        assert!(!Arc::ptr_eq(&first.filters, &second.filters));
    }
    #[test]
    fn plate_cache_is_not_invalidated_by_other_analog_arrival() {
        let mut source = (*data()).clone();
        source.devices[0].plate = Some(0);
        let source = Arc::new(source);
        let ctx = egui::Context::default();
        let mut device = Device::default();
        device.settings.enabled = true;
        device.settings.steps = vec![Step::Rectify];
        wait(&mut device, &source, &ctx);
        let ready = device.ready.clone().unwrap();
        let mut later = (*source).clone();
        later.attach_analog(later.take.analog.clone(), None);
        later.devices[0].plate = Some(0);
        let later = Arc::new(later);
        device.update(&later, 0, &ctx);
        assert!(!device.pending());
        assert!(Arc::ptr_eq(&ready, device.ready.as_ref().unwrap()));
    }
    #[test]
    fn independent_devices_keep_separate_settings() {
        let mut filters = Filters::default();
        filters
            .devices
            .entry(0)
            .or_default()
            .settings
            .steps
            .push(Step::Rectify);
        assert!(filters
            .devices
            .entry(1)
            .or_default()
            .settings
            .steps
            .is_empty());
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;
    fn render(
        ctx: &egui::Context,
        device: &mut Device,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    controls(ui, device, Some(1000.0));
                    ui.label("after_controls");
                });
            },
        )
    }
    fn text(out: &egui::FullOutput, want: &str) -> egui::Rect {
        out.shapes
            .iter()
            .find_map(|s| {
                if let egui::Shape::Text(t) = &s.shape {
                    (t.galley.text() == want)
                        .then(|| egui::Rect::from_min_size(t.pos, t.galley.size()))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("missing {want}"))
    }
    #[test]
    fn pending_errors_and_short_runs_keep_existing_rows_stationary() {
        let ctx = egui::Context::default();
        let mut device = Device::default();
        device.settings.steps = vec![
            Step::BandPass {
                low_hz: 20.0,
                high_hz: 450.0,
                order: 4,
                zero_phase: true,
            },
            Step::Rectify,
        ];
        render(&ctx, &mut device, vec![]);
        let out = render(&ctx, &mut device, vec![]);
        let row = text(&out, "Rectify");
        let end = text(&out, "after_controls");
        let (_tx, rx) = std::sync::mpsc::channel();
        device.running = Some(rx);
        device.error = Some("worker failure".into());
        device.ready = Some(Arc::new(Output {
            channels: vec![],
            bands: vec![],
            badge: String::new(),
            skipped_runs: 3,
        }));
        if let Step::BandPass { high_hz, .. } = &mut device.settings.steps[0] {
            *high_hz = 600.0;
        }
        let out = render(&ctx, &mut device, vec![]);
        assert_eq!(text(&out, "Rectify"), row);
        assert_eq!(text(&out, "after_controls"), end);
        assert!(out
            .shapes
            .iter()
            .any(|s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.text()=="◌ Computing")));
    }
}
