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

pub fn bench(files: Vec<PathBuf>, overlays: Vec<PathBuf>, frames: usize, size: [f32; 2]) -> i32 {
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
    let started = ms(start);
    let opened = Instant::now();
    app.bench_open(&ctx, &files);
    if !overlays.is_empty() {
        app.overlay_args(&ctx, &overlays);
    }
    let (mut markers, mut analog) = (None, None);
    // Views with a force plate chosen while playing (plate graph floating).
    let mut plate_views = 0;
    let (mut loading, mut playing): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    // The app UI part of each playing frame (the rest is tessellation).
    let mut playing_ui: Vec<f64> = Vec::new();
    // Playing frames until every graph was complete (v0.27.0 speed fix: the markers past a
    // frame's graph work come in the next frames, `graph::spend`).
    let mut complete: Option<usize> = None;
    loop {
        if opened.elapsed() > TIMEOUT {
            eprintln!("bench: stopped after {} s", TIMEOUT.as_secs());
            return 1;
        }
        if analog.is_some() && BENCH_HOVER.load(std::sync::atomic::Ordering::Relaxed) {
            app.bench_hover(playing.len());
        }
        let t = Instant::now();
        let out = ctx.run(frame_input(screen, start, vec![]), |ctx| app.frame(ctx));
        let ui_took = ms(t);
        let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
        let took = ms(t);
        let st = app.bench_state();
        if let Some(e) = st.error {
            eprintln!("bench: {e}");
            return 1;
        }
        if markers.is_none() && st.loaded {
            markers = Some(ms(opened));
        }
        if analog.is_some() {
            // `graph_put_off` is about the frame before this one.
            if complete.is_none() && !playing.is_empty() && !app.graph_put_off {
                complete = Some(playing.len());
            }
            playing.push(took);
            playing_ui.push(ui_took);
            if playing.len() >= frames.max(1) {
                break;
            }
            continue;
        }
        loading.push(took);
        if markers.is_some() && !st.analog_pending && !st.overlay_pending {
            analog = Some(ms(opened));
            plate_views = app.bench_play(start.elapsed().as_secs_f64());
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let st = app.bench_state();
    let stats = |v: &[f64]| (v.iter().sum::<f64>() / v.len().max(1) as f64, v.iter().cloned().fold(0.0, f64::max));
    println!("bench: {} file(s), window {}x{}", st.views.len(), size[0], size[1]);
    println!("  app start (no file): {started:.0} ms");
    for v in &st.views {
        let steps: Vec<String> = v.steps.iter().map(|(k, x)| format!("{k} {x:.1}")).collect();
        println!("  {}", v.file);
        println!("    markers on screen: {:.1} ms after open", v.markers_ms.unwrap_or(f64::NAN));
        println!("    force plates / EMG: {:.1} ms after open", v.analog_ms.unwrap_or(f64::NAN));
        println!("    steps (ms): {}", steps.join(", "));
        if let Some((file, ms)) = &v.overlay {
            println!("    laid over it: {file}, on screen {:.1} ms after asking", ms.unwrap_or(f64::NAN));
        }
    }
    println!("  all markers on screen: {:.1} ms after open", markers.unwrap_or(f64::NAN));
    println!("  all force plates / EMG: {:.1} ms after open", analog.unwrap_or(f64::NAN));
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
    0
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
        let raw = frame_input(screen, start, std::mem::take(&mut events));
        let out = ctx.run(raw, |ctx| app.frame(ctx));
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
