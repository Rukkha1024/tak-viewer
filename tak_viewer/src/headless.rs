//! `--headless`: run `--capture` / `--screenshot` without a window, so it works where there is
//! no desktop (e.g. a Windows session-0 shell used by agents).
//! The same app code runs (`TakViewerApp::frame`: menus, panels, 3D view, capture steps). Only
//! the window part is replaced: a screenshot requested with `ViewportCommand::Screenshot` is
//! painted on the CPU (`soft_render`) and handed back as `Event::Screenshot`, as the window
//! backend does. Saved settings are neither read nor written. Never writes a `.tak`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Color32, Event, RawInput, Rect, ViewportCommand, ViewportId, ViewportInfo};

use crate::app::TakViewerApp;
use crate::bench_load::ProcessMemory;
use crate::bench_timing::{BenchTrace, FrameTiming};
use crate::capture::CapturePlan;
use crate::shot::AutoShot;
use crate::soft_render::{self, Textures};

/// Give up after this long (a stuck load or capture must not hang an agent forever).
const TIMEOUT: Duration = Duration::from_secs(600);
/// eframe's default clear color (`App::clear_color`), premultiplied. Panels cover it anyway.
const CLEAR: Color32 = Color32::from_rgba_premultiplied(8, 8, 8, 180);

/// Input for one headless frame: a fixed screen, time since `start`.
fn frame_input(screen: Rect, start: Instant, events: Vec<Event>) -> RawInput {
    let mut raw = RawInput {
        screen_rect: Some(screen),
        time: Some(start.elapsed().as_secs_f64()),
        max_texture_side: Some(8192),
        focused: true,
        events,
        ..Default::default()
    };
    raw.viewports.insert(
        ViewportId::ROOT,
        ViewportInfo {
            native_pixels_per_point: Some(1.0),
            inner_rect: Some(screen),
            outer_rect: Some(screen),
            focused: Some(true),
            ..Default::default()
        },
    );
    raw
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// `--bench-select N` (v0.27.0, agent-only): the bench plays with the first N markers of the
/// LABELS list selected (the graph of several markers), instead of the first marker alone.
pub static BENCH_SELECT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

/// `--bench-hover` (v0.29.0, agent-only): while playing, every view's marker graph is hovered as
/// if the mouse went from curve to curve and off them (`TakViewerApp::bench_hover`): another
/// curve (or none) every second frame, the worst case for the hover's thick curves.
pub static BENCH_HOVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--bench-emg`: access a real deferred non-forceplate device/graph after startup, keeping
/// frames running until its read, attachment and background graph preparation are complete.
pub static BENCH_EMG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Native per-phase wall/CPU diagnostics; no syscalls or trace allocations while disabled.
pub static BENCH_TRACE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const NO_DEFERRED_EMG: &str = "--bench-emg를 적용할 수 없어요. 열린 파일에 나중에 읽도록 미뤄 둔 근전도·기타 아날로그 장치가 없어요. 근전도 읽기 성능은 측정하지 않았어요.";

fn require_deferred_emg(state: &crate::app::BenchState) -> Result<(), &'static str> {
    if state.views.iter().any(|view| view.lazy_analog) { Ok(()) } else { Err(NO_DEFERRED_EMG) }
}

fn emg_frame_ready(state: &crate::app::BenchState, pending: bool) -> Result<bool, String> {
    if let Some((view, reason)) = state.views.iter().find_map(|view| view.emg_failure.as_ref().map(|reason| (view, reason))) {
        return Err(format!("--bench-emg: {}: 근전도 읽기가 실패해 성능 측정을 마치지 못했어요. {reason}", view.file));
    }
    Ok(!pending)
}

pub fn bench(files: Vec<PathBuf>, overlays: Vec<PathBuf>, frames: usize, size: [f32; 2], folder: Option<PathBuf>) -> i32 {
    let ctx = egui::Context::default();
    let start = Instant::now();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    // Reads only: no comment file is ever written by a bench run.
    app.set_command_run();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(size[0], size[1]));
    for _ in 0..3 {
        let out = ctx.run(frame_input(screen, start, vec![]), |ctx| app.frame(ctx));
        let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
    }
    let memory_baseline = ProcessMemory::sample();
    let mut memory_by_file: std::collections::HashMap<String, (Option<ProcessMemory>, Option<ProcessMemory>)> = std::collections::HashMap::new();
    let started = ms(start);
    if let Some(folder) = folder {
        if crate::workspace::is_remote_root_key(&folder) || !folder.is_dir() || !app.open_workspace(folder.clone()) {
            eprintln!("bench: 로컬 폴더 경로를 확인해 주세요.");
            return 2;
        }
        loop {
            let out = ctx.run(frame_input(screen, start, vec![]), |ctx| app.frame(ctx));
            let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
            match app.bench_workspace_count(&folder) {
                Some(Ok(count)) => {
                    println!("  workspace before open: {count} take rows");
                    break;
                }
                Some(Err(error)) => {
                    eprintln!("bench: {error}");
                    return 1;
                }
                None if start.elapsed() >= TIMEOUT => {
                    eprintln!("bench: 폴더 목록 읽기 시간이 너무 오래 걸려요.");
                    return 1;
                }
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    }
    let opened = Instant::now();
    app.bench_open(&ctx, &files);
    if !overlays.is_empty() {
        app.overlay_args(&ctx, &overlays);
    }
    let (mut markers, mut analog) = (None, None);
    let (mut emg_started, mut emg_ready, mut emg_memory) = (None, None, None);
    let mut emg_views = 0;
    let mut emg_loading: Vec<f64> = Vec::new();
    // Views with a force plate chosen while playing (plate graph floating).
    let mut plate_views = 0;
    let (mut loading, mut playing): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    // The app UI part of each playing frame (the rest is tessellation).
    let mut playing_ui: Vec<f64> = Vec::new();
    // Playing frames until every graph was complete (v0.27.0 speed fix: the markers past a
    // frame's graph work come in the next frames, `graph::spend`).
    let mut complete: Option<usize> = None;
    let mut trace = BenchTrace::new(BENCH_TRACE.load(std::sync::atomic::Ordering::Relaxed), frames);
    loop {
        if opened.elapsed() > TIMEOUT {
            eprintln!("bench: stopped after {} s", TIMEOUT.as_secs());
            return 1;
        }
        if analog.is_some() && BENCH_HOVER.load(std::sync::atomic::Ordering::Relaxed) {
            app.bench_hover(playing.len());
        }
        let emg_was_pending = emg_started.is_some() && app.bench_emg_pending();
        let since_open_ms = if trace.enabled() { ms(opened) } else { 0.0 };
        let cpu_start = trace.cpu_mark();
        crate::bench_timing::begin_details(trace.enabled());
        let t = Instant::now();
        let out = ctx.run(frame_input(screen, start, vec![]), |ctx| {let _span=crate::bench_timing::span("app.frame");app.frame(ctx)});
        let ui_took = ms(t);
        let cpu_ui = trace.cpu_mark();
        let shapes = out.shapes.len();
        fn geometry_counts(shape: &egui::Shape) -> (usize, usize) {
            match shape {
                egui::Shape::Path(path) => (path.points.len(), 0),
                egui::Shape::Mesh(mesh) => (0, mesh.vertices.len()),
                egui::Shape::Vec(shapes) => shapes.iter().map(geometry_counts).fold((0,0), |a,b| (a.0+b.0,a.1+b.1)),
                _ => (0,0),
            }
        }
        let counts = if trace.enabled() {out.shapes.iter().map(|s|geometry_counts(&s.shape)).fold((0,0),|a,b|(a.0+b.0,a.1+b.1))} else {(0,0)};
        let mut tessellation = trace.enabled().then(|| crate::bench_timing::span("epaint.tessellate"));
        let primitives = ctx.tessellate(out.shapes, out.pixels_per_point);
        if let Some(span) = &mut tessellation { span.counts(|| (counts.0, counts.1)); }
        drop(tessellation);
        let primitive_count = primitives.len();
        let output_vertices = if trace.enabled() {primitives.iter().map(|p|match &p.primitive {egui::epaint::Primitive::Mesh(m)=>m.vertices.len(),_=>0}).sum()} else {0};
        drop(primitives);
        let took = ms(t);
        let cpu_end = trace.cpu_mark();
        if trace.enabled() {
            let phase = if analog.is_none() { "startup" } else if emg_started.is_some() && emg_ready.is_none() { "emg" } else { "playing" };
            let mut record = FrameTiming::new(loading.len() + playing.len(), phase, since_open_ms, shapes, primitive_count, ui_took, took, [cpu_start, cpu_ui, cpu_end]);
            record.ui_passes = out.platform_output.num_completed_passes;
            record.path_points = counts.0;
            record.cached_mesh_vertices = counts.1;
            record.output_vertices = output_vertices;
            let details=crate::bench_timing::take_details();
            record.details = if took >= 8.0 || phase == "emg" {
                details
            } else {
                details.into_iter().filter(|d| matches!(d.name, "scene.measurement_samples" | "scene.measurement_paint")).collect()
            };
            trace.push(record);
        }
        let st = app.bench_state();
        for view in &st.views {
            let memory = memory_by_file.entry(view.file.clone()).or_default();
            if view.markers_ms.is_some() && memory.0.is_none() { memory.0 = ProcessMemory::sample(); }
            if view.analog_ms.is_some() && memory.1.is_none() { memory.1 = ProcessMemory::sample(); }
        }
        if let Some(e) = st.error {
            eprintln!("bench: {e}");
            return 1;
        }
        if markers.is_none() && st.loaded {
            markers = Some(ms(opened));
        }
        if analog.is_some() {
            if emg_started.is_some() {
                let emg_pending = app.bench_emg_pending();
                let ready = match emg_frame_ready(&st, emg_pending) {
                    Ok(ready) => ready,
                    Err(reason) => { eprintln!("bench: {reason}"); return 1; }
                };
                if emg_was_pending || emg_pending { emg_loading.push(took); }
                if ready && emg_ready.is_none() {
                    emg_ready = emg_started.map(ms);
                    emg_memory = ProcessMemory::sample();
                }
            }
            // `graph_put_off` is about the frame before this one.
            if complete.is_none() && !playing.is_empty() && !app.graph_put_off {
                complete = Some(playing.len());
            }
            playing.push(took);
            playing_ui.push(ui_took);
            if playing.len() >= frames.max(1) && (emg_started.is_none() || emg_ready.is_some()) {
                break;
            }
            continue;
        }
        loading.push(took);
        if markers.is_some() && !st.analog_pending && !st.overlay_pending {
            analog = Some(ms(opened));
            plate_views = app.bench_play(start.elapsed().as_secs_f64());
            if BENCH_EMG.load(std::sync::atomic::Ordering::Relaxed) {
                if let Err(reason) = require_deferred_emg(&st) {
                    eprintln!("bench: {reason}");
                    return 2;
                }
                emg_started = Some(Instant::now());
                emg_views = app.bench_emg(&ctx);
                if emg_views == 0 {
                    eprintln!("bench: {NO_DEFERRED_EMG}");
                    return 2;
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let st = app.bench_state();
    let stats = |v: &[f64]| (v.iter().sum::<f64>() / v.len().max(1) as f64, v.iter().cloned().fold(0.0, f64::max));
    println!("bench: {} file(s), window {}x{}", st.views.len(), size[0], size[1]);
    if crate::device_grid::PROBE.get().is_some_and(|p|p.size.is_some()) {
        let (cells, filtered) = app.bench_grid_cells();
        println!("  device grid: {cells} graph cells prepared, {filtered} with completed filter output");
    }
    println!("  app start (no file): {started:.0} ms");
    println!("  memory: actual whole-process resident/working set; per-file readiness samples overlap across files");
    println!("  no-file app baseline: {}", ProcessMemory::describe(memory_baseline));
    for v in &st.views {
        let steps: Vec<String> = v.steps.iter().map(|(k, x)| format!("{k} {x:.1}")).collect();
        println!("  {}", v.file);
        println!("    markers on screen: {:.1} ms after open", v.markers_ms.unwrap_or(f64::NAN));
        println!("    forceplates ready: {:.1} ms after open (EMG remains lazy unless fallback)", v.analog_ms.unwrap_or(f64::NAN));
        println!("    lazy analog awaiting access: {}", v.lazy_analog);
        if let Some(reason) = &v.fallback { println!("    analog full-read fallback: {reason}"); }
        if let Some(memory) = memory_by_file.get(&v.file) {
            println!("    process at markers: {}", ProcessMemory::describe(memory.0));
            println!("    process at forceplates: {}", ProcessMemory::describe(memory.1));
        }
        println!("    steps (ms): {}", steps.join(", "));
        if let Some((file, ms)) = &v.overlay {
            println!("    laid over it: {file}, on screen {:.1} ms after asking", ms.unwrap_or(f64::NAN));
        }
    }
    println!("  all markers on screen: {:.1} ms after open", markers.unwrap_or(f64::NAN));
    println!("  all forceplates ready: {:.1} ms after open (EMG remains lazy unless fallback)", analog.unwrap_or(f64::NAN));
    let (avg, max) = stats(&loading);
    println!("  while loading {} frames: avg {avg:.2} ms, max {max:.2} ms per frame", loading.len());
    let selected = match BENCH_SELECT.load(std::sync::atomic::Ordering::Relaxed) {
        1 => "first marker selected".to_owned(),
        n => format!("first {n} markers selected, --bench-select"),
    };
    println!(
        "  playing with floating graphs: marker graph in every view ({selected}), plate graph in {plate_views} of {} view(s) (first force plate chosen)",
        st.views.len()
    );
    if BENCH_HOVER.load(std::sync::atomic::Ordering::Relaxed) {
        println!("  hover (--bench-hover): every marker graph hovered on its markers' curves in turn and off them, another every 2nd frame");
    }
    let laid = st.views.iter().filter(|v| v.overlay.is_some()).count();
    if laid > 0 {
        println!("  files laid over views (white rings drawn while playing): {laid}, files in all: {}", st.views.len() + laid);
    }
    match complete {
        Some(n) => println!("  every graph complete after {n} playing frame(s)"),
        None => println!("  graphs still not complete after {} playing frames", playing.len()),
    }
    let (avg, max) = stats(&playing);
    let (ui_avg, _) = stats(&playing_ui);
    println!("  playing {} frames: avg {avg:.2} ms, max {max:.2} ms per frame (app UI {ui_avg:.2} + tessellation {:.2})", playing.len(), avg - ui_avg);
    if emg_started.is_some() {
        let (avg, max) = stats(&emg_loading);
        println!("  lazy EMG access (--bench-emg), {emg_views} device view(s): read + attachment + device graph ready {:.1} ms after request; {} loading/graph frame(s): avg {avg:.2} ms, max {max:.2} ms", emg_ready.unwrap_or(f64::NAN), emg_loading.len());
        println!("  process after EMG/device graph ready: {}", ProcessMemory::describe(emg_memory));
    }
    println!("  total after playing (all views held): {}", ProcessMemory::describe(ProcessMemory::sample()));
    trace.print();
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{BenchState, BenchView};

    fn ready_state(files: &[(&str, bool, Option<&str>)]) -> BenchState {
        BenchState {
            loaded: true, analog_pending: false, overlay_pending: false, error: None,
            views: files.iter().map(|(file, lazy, fallback)| BenchView {
                file: (*file).to_owned(), markers_ms: Some(20.0), analog_ms: Some(40.0),
                lazy_analog: *lazy, fallback: fallback.map(str::to_owned), emg_failure: None, steps: vec![], overlay: None,
            }).collect(),
        }
    }

    #[test]
    fn emg_benchmark_rejects_files_without_a_deferred_nonplate_job() {
        for state in [ready_state(&[]), ready_state(&[("markers_only.tak", false, None)]), ready_state(&[("fallback.tak", false, Some("ambiguous device structure"))])] {
            let reason = require_deferred_emg(&state).expect_err("EMG cannot be measured without a deferred device");
            assert!(reason.contains("측정하지 않았어요"));
        }
    }

    #[test]
    fn emg_benchmark_allows_a_real_deferred_job_among_other_files() {
        let state = ready_state(&[("markers_only.tak", false, None), ("emg.tak", true, None)]);
        assert!(require_deferred_emg(&state).is_ok());
    }

    #[test]
    fn emg_benchmark_does_not_treat_failed_read_as_ready_when_pending_clears() {
        let mut state = ready_state(&[("good.tak", false, None), ("failed_emg.tak", false, None)]);
        state.views[1].emg_failure = Some("deferred file read failed".to_owned());
        let reason = emg_frame_ready(&state, false).expect_err("cleared pending after failure is not successful readiness");
        assert!(reason.contains("failed_emg.tak"));
        assert!(reason.contains("deferred file read failed"));
    }

    #[test]
    fn emg_benchmark_waits_until_a_successful_read_is_ready() {
        let state = ready_state(&[("emg.tak", true, None)]);
        assert!(!emg_frame_ready(&state, true).unwrap());
        assert!(emg_frame_ready(&state, false).unwrap());
    }

    #[test]
    fn emg_benchmark_returns_nonzero_for_a_real_marker_only_file() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let _temp = tak_core::test_support::TempDir::new("tak_bench_emg_no_deferred");
        let dir = _temp.join(format!("tak_bench_emg_no_deferred_{}_{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("markers_only.tak");
        std::fs::write(&path, tak_core::sample_tak::sample_tak()).unwrap();
        let previous = BENCH_EMG.swap(true, std::sync::atomic::Ordering::Relaxed);
        let code = bench(vec![path], vec![], 1, [900.0, 600.0], None);
        BENCH_EMG.store(previous, std::sync::atomic::Ordering::Relaxed);
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(code, 2, "an inapplicable EMG measurement must not report success");
    }
}

/// Returns the process exit code: 0 = done, 1 = failed or timed out.
pub fn run(initial: Vec<PathBuf>, auto: Option<AutoShot>, capture: Option<CapturePlan>, size: [f32; 2]) -> i32 {
    run_with_probe(initial, auto, capture, size, None)
}

pub fn run_ssh(probe: crate::ssh_probe::Probe, auto: Option<AutoShot>) -> i32 {
    run_with_probe(vec![], auto, None, [1600.0, 960.0], Some(probe))
}
fn run_with_probe(initial: Vec<PathBuf>, mut auto: Option<AutoShot>, capture: Option<CapturePlan>, size: [f32; 2], mut probe: Option<crate::ssh_probe::Probe>) -> i32 {
    let ctx = egui::Context::default();
    let initial_auto = if probe.is_some() { None } else { auto.take() };
    let mut app = TakViewerApp::build(&ctx, None, initial, initial_auto, capture);
    if let Some(p) = &mut probe { if let Err(e) = p.start(&mut app, &ctx) { eprintln!("{e}"); return 1; } }
    let mut textures = Textures::default();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(size[0], size[1]));
    let start = Instant::now();
    let mut events: Vec<Event> = Vec::new();
    loop {
        if probe.is_none() && start.elapsed() > TIMEOUT {
            eprintln!("headless: stopped after {} s without finishing", TIMEOUT.as_secs());
            return 1;
        }
        let mut raw = frame_input(screen, start, std::mem::take(&mut events));
        crate::desktop_drop::inject_preview(&mut raw);
        let out = ctx.run(raw, |ctx| app.frame(ctx));
        events.extend(app.scene_menu_shot_hover().map(Event::PointerMoved));
        if let Some(p) = &mut probe {
            match p.tick(&mut app, &ctx) {
                crate::ssh_probe::Step::Escape => events.push(Event::Key { key:egui::Key::Escape, physical_key:None, pressed:true, repeat:false, modifiers:egui::Modifiers::NONE }),
                crate::ssh_probe::Step::Capture => { if let Some(auto) = auto.take() { app.ssh_probe_capture(auto); } },
                crate::ssh_probe::Step::Done(mut code) => {
                    // Errors and cancellation still produce reviewable evidence of the live view.
                    if let Some(auto) = auto.take() {
                        textures.set(&out.textures_delta);
                        let ppp = out.pixels_per_point;
                        let px = [(size[0] * ppp).round() as usize, (size[1] * ppp).round() as usize];
                        let prims = ctx.tessellate(out.shapes.clone(), ppp);
                        let img = soft_render::paint(&prims, &textures, px, ppp, CLEAR);
                        let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
                        if let Err(e) = crate::shot::save_png(&auto.out, px[0], px[1], &rgba) { eprintln!("screenshot: {e}"); code = 1; }
                    }
                    app.ssh_probe_disconnect(&p.alias);
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while app.ssh_probe_workers_running() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(10)); }
                    return if app.ssh_probe_workers_running() { 1 } else { code };
                }
                crate::ssh_probe::Step::Wait => (),
            }
        }
        textures.set(&out.textures_delta);
        let commands = out.viewport_output.get(&ViewportId::ROOT).map(|v| v.commands.as_slice()).unwrap_or(&[]);
        let close = commands.iter().any(|c| matches!(c, ViewportCommand::Close));
        let shot = commands.iter().any(|c| matches!(c, ViewportCommand::Screenshot));
        if shot && !close {
            let ppp = out.pixels_per_point;
            let px = [(size[0] * ppp).round() as usize, (size[1] * ppp).round() as usize];
            let prims = ctx.tessellate(out.shapes, ppp);
            let image = soft_render::paint(&prims, &textures, px, ppp, CLEAR);
            events.push(Event::Screenshot { viewport_id: ViewportId::ROOT, image: Arc::new(image) });
        }
        textures.free(&out.textures_delta);
        if close {
            if let Some(p) = &probe {
                app.ssh_probe_disconnect(&p.alias);
                let deadline = Instant::now() + Duration::from_secs(10);
                while app.ssh_probe_workers_running() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(10)); }
                if app.ssh_probe_workers_running() { return 1; }
            }
            return i32::from(app.failed);
        }
        if !shot {
            // The file loads on a background thread; don't spin while waiting.
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
