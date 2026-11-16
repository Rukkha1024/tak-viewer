//! Motive-style read-only TAK marker viewer.
//! `tak_viewer [file.tak …]` opens the GUI (up to 4 files side by side, `views.rs`);
//! `tak_viewer --dump <file.tak> <out.json>` writes the reader comparison dump (no window; a `.takm`
//! gives the same numbers); `--to-takm <a.tak> … [--out-dir <dir>]` writes `<stem>.takm` files
//! (`tak_core::takm`, no window);
//! `--screenshot out.png [--frame N] [--select NAME[,NAME…]] [--plate N|NAME] [--range A-B] [--comments] <file.tak …>`
//! renders one frame to PNG and exits (verification; several files = views side by side; `--plate`
//! = a force plate's graph over the 3D, `--range` = a frame range in the playback bars, `shot.rs`;
//! `--overlay <b.tak>` = that file laid over the first view, again for the next view, `--hide 1|2`
//! = that file's markers hidden there, plan step 7; `--hover NAME` = the marker graph drawn as if
//! the mouse were on that marker's curve, v0.29.0); `--capture <file.tak> [options]` saves
//! frames x angles as PNG + sheet + JSON in the repo for visual checks (`capture.rs`).
//! `--headless` (with `--capture` or `--screenshot`) runs the same app without a window and paints
//! the pictures on the CPU (`headless.rs`, `soft_render.rs`): works without a desktop.
//! `--version` prints the program version (`version.rs`) and exits.
//! `--bench-load <file.tak|file.takm>…` retains up to twelve marker/forceplate display models
//! without rendering; reports phase times and native process memory (`bench_load.rs`).
//! Several copies may run at once (the user's windows next to agent runs, `instances.rs`).
//! Where no window can be shown (Windows session 0), `--capture`/`--screenshot` run headless by
//! themselves and a plain window run stops with exit 2 instead of waiting forever unseen.
//! TAK reading and calculations live in the `tak_core` crate (no GUI); this program only draws.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod bench_load;
mod bench_timing;
mod camera;
mod capture;
mod headless;
mod instances;
mod shot;
mod ssh;
mod ssh_probe;
mod soft_render;
mod ui;
mod version;
mod views;
mod workspace;

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let mut args: Vec<String> = std::env::args().collect();
    if matches!(args.get(1).map(String::as_str), Some("--version" | "-V")) {
        println!("{}", version::VERSION);
        std::process::exit(0);
    }
    if matches!(args.get(1).map(String::as_str), Some("--dump" | "--dump-fast")) {
        // WSL paths (/home/..., /mnt/c/...) -> Windows paths, like the other commands.
        // Without this, an output under /mnt/c/... was written via \\wsl.localhost\...\mnt\c and failed.
        let paths = args[2..].iter().map(|a| workspace::resolve_path(a).to_string_lossy().into_owned());
        let dump_args: Vec<String> = std::iter::once(args[1].clone()).chain(paths).collect();
        std::process::exit(tak_core::dump::run(&dump_args));
    }
    if args.get(1).map(String::as_str) == Some("--to-takm") {
        // `--to-takm <a.tak> … [--out-dir <dir>]`: TAK -> `.takm` (`tak_core::takm`, no window).
        let paths = args[2..].iter().map(|a| if a.starts_with("--") { a.clone() } else { workspace::resolve_path(a).to_string_lossy().into_owned() });
        let takm_args: Vec<String> = std::iter::once(args[1].clone()).chain(paths).collect();
        std::process::exit(tak_core::takm::run(&takm_args));
    }
    if args.iter().any(|a| matches!(a.as_str(), "--ssh-open" | "--ssh-list")) {
        std::process::exit(ssh_probe::run(&args[1..]));
    }
    if let Some(i) = args.iter().position(|a| a == "--ssh-demo") {
        let state = args.get(i + 1).map(String::as_str).unwrap_or("");
        if !matches!(state, "off" | "connecting" | "connected" | "markers_ready" | "lost" | "top_windows" | "top_linux") || !args.iter().any(|a| a == "--screenshot") {
            eprintln!("--ssh-demo는 --screenshot과 함께 off|connecting|connected|markers_ready|lost|top_windows|top_linux를 써요.");
            std::process::exit(2);
        }
        ssh::set_demo_state(state);
        args.drain(i..i + 2);
    }
    let mut headless = args.iter().skip(1).any(|a| a == "--headless");
    if headless {
        args.retain(|a| a != "--headless");
    }
    if args.iter().any(|a| a == "--bench-load") {
        let files = match bench_load::parse(&args[1..]) {
            Ok(files) => files.iter().map(|p| workspace::resolve_path(&p.to_string_lossy())).collect(),
            Err(error) => {
                eprintln!("{error}\n{}", bench_load::USAGE);
                std::process::exit(2);
            }
        };
        std::process::exit(bench_load::run(files));
    }
    if args.iter().any(|a| a == "--bench") {
        // `--bench [frames] <file.tak>…`: open speed and frame time, up to 4 files side by side
        // (`headless::bench`, never a window).
        // `--overlay <b.tak>` (plan step 7, may repeat): the n-th laid over the n-th view.
        let (overlays, mut args) = shot::take_overlays(&args);
        // `--bench-select N` (v0.27.0, agent-only): play with the first N markers selected.
        if let Some(i) = args.iter().position(|a| a == "--bench-select") {
            let n = args.get(i + 1).and_then(|a| a.parse::<usize>().ok()).unwrap_or(1);
            headless::BENCH_SELECT.store(n.max(1), std::sync::atomic::Ordering::Relaxed);
            args.drain(i..(i + 2).min(args.len()));
        }
        // `--bench-hover` (v0.29.0, agent-only): while playing, the marker graphs' hover moves from
        // curve to curve and off them (`headless::bench`).
        if let Some(i) = args.iter().position(|a| a == "--bench-hover") {
            headless::BENCH_HOVER.store(true, std::sync::atomic::Ordering::Relaxed);
            args.remove(i);
        }
        // Exercise real lazy non-forceplate/device graph access while frames keep running.
        if let Some(i) = args.iter().position(|a| a == "--bench-emg") {
            headless::BENCH_EMG.store(true, std::sync::atomic::Ordering::Relaxed);
            args.remove(i);
        }
        if let Some(i) = args.iter().position(|a| a == "--bench-trace") {
            headless::BENCH_TRACE.store(true, std::sync::atomic::Ordering::Relaxed);
            args.remove(i);
        }
        let rest: Vec<&String> = args.iter().skip(1).filter(|a| *a != "--bench").collect();
        let frames = rest.first().and_then(|a| a.parse::<usize>().ok());
        let files: Vec<PathBuf> =
            rest.iter().skip(usize::from(frames.is_some())).map(|a| workspace::resolve_path(a)).collect();
        if files.is_empty() {
            eprintln!("usage: tak_viewer --bench [frames] <file.tak> [more.tak …] [--overlay <b.tak> …] [--bench-select N] [--bench-hover] [--bench-emg] [--bench-trace] (up to 4 views, one overlay per view)");
            std::process::exit(2);
        }
        if let Err(error) = bench_load::validate_ui_count(files.len(), overlays.len()) {
            eprintln!("{error}");
            std::process::exit(2);
        }
        std::process::exit(headless::bench(files, overlays, frames.unwrap_or(300), [1600.0, 960.0]));
    }
    let no_desktop = !headless && instances::no_desktop();
    let capture = match capture::parse(&args[1..]) {
        Ok(c) => c,
        Err(e) if e.is_empty() => {
            println!("{}", capture::USAGE);
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("capture: {e}\n\n{}", capture::USAGE);
            std::process::exit(2);
        }
    };
    let (auto, rest) = if capture.is_some() { (None, vec![]) } else { shot::parse(&args[1..]) };
    let initial: Vec<PathBuf> = match &capture {
        Some(c) => vec![c.tak.clone()],
        None => rest.iter().map(PathBuf::from).collect(),
    };
    let size = capture.as_ref().map_or([1600.0, 960.0], |c| c.size);
    let command_run = capture.is_some() || auto.is_some();
    if no_desktop {
        if !command_run {
            eprintln!(
                "화면이 없는 곳(Windows 0번 세션)이라 창을 띄울 수 없어요. 창은 보이지 않은 채 멈춰 있게 돼요.\n\
                 그림은 --headless와 --capture나 --screenshot으로 찍어요.\n\n{}",
                capture::USAGE
            );
            std::process::exit(2);
        }
        eprintln!("화면이 없는 곳이라 --headless로 찍어요.");
        headless = true;
    }
    if headless {
        if capture.is_none() && auto.is_none() {
            eprintln!("--headless는 --capture나 --screenshot과 함께 써요.

{}", capture::USAGE);
            std::process::exit(2);
        }
        std::process::exit(headless::run(initial, auto, capture, size));
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(version::window_title(None))
            .with_inner_size(size)
            .with_min_inner_size([900.0, 600.0])
            .with_drag_and_drop(true),
        persist_window: !command_run,
        ..Default::default()
    };
    // A window the user works in joins the open-window list (settings rule in `app.rs`).
    // Command line runs do not: they never save settings.
    let registration = if command_run { None } else { instances::default_dir().and_then(|d| instances::register(&d)) };
    let others_at_start = registration.as_ref().is_some_and(|r| r.others_alive());
    eframe::run_native(
        "tak_viewer",
        options,
        Box::new(move |cc| {
            let mut app = app::TakViewerApp::new(cc, initial, auto, capture);
            app.set_registration(registration, others_at_start);
            // WORKSPACE follows the disk in the user's window only; command runs
            // (`--screenshot`, `--capture`) never start the folder watcher.
            if !command_run {
                app.start_folder_watch();
            }
            Ok(Box::new(app))
        }),
    )
}
