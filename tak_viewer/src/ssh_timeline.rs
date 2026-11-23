//! Opt-in headless evidence for the normal SSH open. Store only milestone frames.
use crate::{
    app::TakViewerApp,
    soft_render::{self, Textures},
    ui::theme,
};
use eframe::egui::{self, Color32, ColorImage, Rect, Shape};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tak_core::remote::{ReadMoment, ReadReceipt};

pub(crate) struct Timeline {
    path: PathBuf,
    shots: Option<PathBuf>,
    state: Mutex<Option<(Instant, Vec<Value>)>>,
}

impl Timeline {
    pub fn new(path: PathBuf, shots: Option<PathBuf>) -> Result<Arc<Self>, String> {
        for target in std::iter::once(&path).chain(shots.iter()) {
            crate::capture::check_output_path(target).map_err(|e| e.to_string())?;
            if target.exists() || target.symlink_metadata().is_ok() {
                return Err(format!("SSH evidence already exists: {}", target.display()));
            }
        }
        if shots.as_ref() == Some(&path) {
            return Err("SSH timeline and screenshot directory need different paths".into());
        }
        Ok(Arc::new(Self {
            path,
            shots,
            state: Mutex::new(None),
        }))
    }

    pub fn click(&self) {
        *self.state.lock().unwrap() = Some((
            Instant::now(),
            vec![json!({
                "schema": 1, "event": "click", "ms_from_click": 0.0
            })],
        ));
    }

    fn started(&self) -> bool {
        self.state.lock().unwrap().is_some()
    }

    pub fn record(&self, event: &'static str, fields: Value) {
        self.record_at(event, Instant::now(), fields);
    }

    fn record_at(&self, event: &'static str, time: Instant, fields: Value) {
        let mut state = self.state.lock().unwrap();
        let Some((click, events)) = &mut *state else {
            return;
        };
        let mut row = fields.as_object().cloned().unwrap_or_default();
        row.insert("event".into(), json!(event));
        row.insert(
            "ms_from_click".into(),
            json!(millis(time.saturating_duration_since(*click))),
        );
        events.push(Value::Object(row));
    }

    pub fn read(&self, moment: ReadMoment, receipt: ReadReceipt) {
        let event = match moment {
            ReadMoment::OpenRequested => "open_request",
            ReadMoment::FirstPose => "first_pose_received",
            ReadMoment::FirstPacket => "first_packet_received",
            ReadMoment::Remainder => "remainder_received",
            ReadMoment::AnalogRequested => "forceplate_request",
            ReadMoment::AnalogPacket => "forceplate_packet_received",
        };
        let fields = if matches!(moment, ReadMoment::OpenRequested | ReadMoment::AnalogRequested) {
            Value::Null
        } else {
            let first_byte_ms = receipt.first_byte.and_then(|byte| {
                self.state.lock().ok()?.as_ref().map(|(click, _)| millis(byte.saturating_duration_since(*click)))
            });
            json!({"bytes": receipt.bytes, "codec": receipt.codec,
                "server_wall_ms": receipt.server_wall_ms, "server_cpu_ms": receipt.server_cpu_ms,
                "first_byte_ms_from_click": first_byte_ms})
        };
        self.record(event, fields);
    }

    fn save(&self, frames: Evidence, code: i32, workers_running: bool) -> io::Result<()> {
        if let Some(dir) = &self.shots {
            fs::create_dir(dir)?;
            for (name, image) in frames.images {
                let file = dir.join(format!("{name}.png"));
                save_png(&file, &image)?;
            }
        }
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)?;
        let mut state = self.state.lock().unwrap();
        if let Some((_, events)) = state.as_mut() {
            events.sort_by(|a, b| {
                a["ms_from_click"]
                    .as_f64()
                    .unwrap()
                    .total_cmp(&b["ms_from_click"].as_f64().unwrap())
            });
            for event in events.drain(..) {
                write_event(&mut f, &event)?;
            }
        }
        write_event(
            &mut f,
            &json!({
                "schema": 1, "event": "summary", "exit_code": code, "workers_running": workers_running,
                "slowest_frame_ms": frames.slowest, "swap_frame_ms": frames.swap,
                "first_marker_pixels": frames.marker, "first_forceplate_pixels": frames.force,
                "frames_measured": frames.frames
            }),
        )
    }
}

fn write_event(file: &mut fs::File, row: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *file, row)?;
    file.write_all(b"\n")
}

fn millis(t: Duration) -> f64 {
    t.as_secs_f64() * 1000.0
}

fn save_png(path: &Path, image: &ColorImage) -> io::Result<()> {
    let bytes: Vec<_> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
    crate::capture::write_png(path, image.size, png::ColorType::Rgba, &bytes)?;
    crate::shot_output::verify_png(path, image.size)
}

#[derive(Default)]
pub(crate) struct Evidence {
    slowest: f64,
    swap: Option<f64>,
    frames: usize,
    first_pose: bool,
    preview: bool,
    exact: bool,
    marker: bool,
    force: bool,
    force_installed: bool,
    images: Vec<(&'static str, Arc<ColorImage>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Painted {
    Other,
    Marker,
    Force,
}

fn classify(shape: &egui::epaint::ClippedShape, scene: Rect) -> Painted {
    match &shape.shape {
        Shape::Circle(c)
            if c.radius >= 2.2
                && c.stroke.width == 0.0
                && c.fill.a() > 0
                && scene.contains(c.center)
                && shape.clip_rect.contains(c.center) =>
        {
            Painted::Marker
        }
        Shape::Path(p)
            if p.closed
                && p.points.len() == 4
                && p.points.iter().any(|point| scene.contains(*point)) =>
        {
            let [r, g, b, a] = p.fill.to_srgba_unmultiplied();
            let color = theme::PLATE_ACTIVE;
            let active = a > 0 && r.abs_diff(color.r()) <= 3
                && g.abs_diff(color.g()) <= 3 && b.abs_diff(color.b()) <= 3;
            if active || p.fill == theme::PLATE_FILL {
                Painted::Force
            } else {
                Painted::Other
            }
        }
        _ => Painted::Other,
    }
}

impl Evidence {
    pub fn frame(
        &mut self,
        trace: &Timeline,
        app: &TakViewerApp,
        ctx: &egui::Context,
        out: &egui::FullOutput,
        textures: &Textures,
        frame_time: Duration,
        clear: Color32,
    ) {
        if !trace.started() {
            return;
        }
        let t0 = Instant::now();
        let ppp = out.pixels_per_point;
        let prims = ctx.tessellate(out.shapes.clone(), ppp);
        let ui_ms = millis(frame_time + t0.elapsed());
        self.frames += 1;
        self.slowest = self.slowest.max(ui_ms);

        let Some((data, scene, frame)) = app.ssh_probe_scene() else {
            return;
        };
        if !self.force_installed && !data.analog_pending {
            if let Some(proof) = &data.ssh_plate_proof {
                trace.record("forceplates_installed", json!(proof));
                self.force_installed = true;
            }
        }
        let is_preview = data.is_preview();
        let is_first_pose = data.is_first_pose();
        let first_pose = is_first_pose && !self.first_pose;
        let first_preview = is_preview && !is_first_pose && !self.preview;
        let first_exact = !is_preview && !self.exact;
        if first_exact {
            self.swap = (self.first_pose || self.preview).then_some(ui_ms);
        }
        self.first_pose |= first_pose;
        self.preview |= first_preview;
        self.exact |= first_exact;

        let draw_frame = data.display_frame(frame);
        let has_markers = !self.marker
            && data
                .markers
                .iter()
                .any(|m| m.position(draw_frame).is_some());
        let has_force = !self.force && self.force_installed;
        let kinds = (has_markers || has_force).then(|| {
            out.shapes
                .iter()
                .map(|s| classify(s, scene))
                .collect::<Vec<_>>()
        });
        let marker_candidate =
            has_markers && kinds.as_ref().is_some_and(|k| k.contains(&Painted::Marker));
        let force_candidate =
            has_force && kinds.as_ref().is_some_and(|k| k.contains(&Painted::Force));
        if !(first_pose || first_preview || first_exact || marker_candidate || force_candidate) {
            return;
        }

        let size = ctx.screen_rect().size();
        let px = [
            (size.x * ppp).round() as usize,
            (size.y * ppp).round() as usize,
        ];
        let raster_start = Instant::now();
        let image = Arc::new(soft_render::paint(&prims, textures, px, ppp, clear));
        let raster_done = Instant::now();
        let raster_ms = millis(raster_done - raster_start);
        let mut record = |event, pixels: Option<usize>| {
            let png = trace
                .shots
                .as_ref()
                .map(|folder| folder.join(format!("{event}.png")));
            trace.record_at(
                event,
                raster_done,
                json!({"frame": frame, "draw_frame": draw_frame, "preview": is_preview, "first_pose": is_first_pose,
                "ui_ms": ui_ms, "raster_ms": raster_ms, "pixels": pixels, "png": png}),
            );
            if trace.shots.is_some() {
                self.images.push((event, image.clone()));
            }
        };
        if first_pose {
            record("first_pose_frame", None);
        }
        if first_preview {
            record("preview_frame", None);
        }
        if first_exact {
            record("exact_frame", None);
        }

        let different_pixels = |kind| {
            let reduced = out
                .shapes
                .iter()
                .zip(kinds.as_ref().unwrap())
                .filter_map(|(s, &k)| (k != kind).then_some(s.clone()))
                .collect();
            let reduced = ctx.tessellate(reduced, ppp);
            let without = soft_render::paint(&reduced, textures, px, ppp, clear);
            image
                .pixels
                .iter()
                .zip(&without.pixels)
                .filter(|(a, b)| a != b)
                .count()
        };
        if marker_candidate {
            let n = different_pixels(Painted::Marker);
            if n > 0 {
                record("first_marker_pixels", Some(n));
                self.marker = true;
            }
        }
        if force_candidate {
            let n = different_pixels(Painted::Force);
            if n > 0 {
                record("first_forceplate_pixels", Some(n));
                self.force = true;
            }
        }
    }

    pub fn finish(self, trace: &Timeline, code: i32, workers_running: bool) -> i32 {
        match trace.save(self, code, workers_running) {
            Ok(()) => code,
            Err(e) => {
                eprintln!("SSH timeline: {e}");
                1
            }
        }
    }
}
