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
mod device_filter;
mod desktop_drop;
mod device_grid;
mod comment_selection;
mod capture;
mod check_command;
mod headless;
mod help_shot;
mod instances;
mod merge_keys;
mod shot;
mod label_increment;
mod ssh;
mod ssh_probe;
mod soft_render;
mod take_progress;
mod ui;
mod version;
mod views;
mod window_command;
mod work_queue;
mod workspace;

use std::path::PathBuf;
use tak_core::comment_command;
use tak_core::summary_command;

fn main() -> eframe::Result<()> {
    let mut args: Vec<String> = std::env::args().collect();
    if matches!(args.get(1).map(String::as_str), Some("--help" | "-h")) {
        println!("{}", usage());
        std::process::exit(0);
    }
    if let Some(request) = help_shot::parse(&args[1..]).unwrap_or_else(|error| cli_exit(2, error)) {
        std::process::exit(headless::run_help_shot(&request));
    }
    match window_command::parse(&args[1..]).unwrap_or_else(|error| cli_exit(2, &error)) {
        Some(window_command::Command::Only { paths, startup }) => {
            if instances::no_desktop() { cli_exit(2, window_command::NO_DESKTOP); }
            let paths = window_command::resolve_files(&paths).unwrap_or_else(|error| cli_exit(1, &error));
            return run_window(paths, None, None, [1600.0, 960.0], Some(startup));
        }
        Some(window_command::Command::List) => {
            let data = instances::data_dir_override().unwrap_or_else(|error| cli_exit(2, &error));
            let dir = instances::registry_dir(data.as_deref());
            if let Err(error) = instances::write_list(dir.as_deref(), std::io::stdout().lock()) {
                cli_exit(1, &format!("뷰어 창 목록을 읽을 수 없어요. {error}"));
            }
            return Ok(());
        }
        None => {}
    }
    if args.get(1).map(String::as_str) == Some("--verify-takm") {
        std::process::exit(tak_core::takm::verify::run(&args[2..], workspace::resolve_path));
    }
    if matches!(args.get(1).map(String::as_str), Some("--add-comments" | "--list-comments")) {
        std::process::exit(comment_command::run(&args[1..], workspace::resolve_path, capture::local_iso_time));
    }
    if args.get(1).map(String::as_str) == Some("--summary") {
        std::process::exit(summary_command::run(&args[2..], workspace::resolve_path));
    }
    if args.get(1).map(String::as_str) == Some("--check") {
        std::process::exit(check_command::run(&args[2..]));
    }
    if let Err(error) = desktop_drop::take_preview(&mut args) { eprintln!("{error}"); std::process::exit(2); }
    if args.get(1).map(String::as_str)==Some("--dump-device-signal") {
        let result=device_filter::dump(&args[2..]);if let Err(error)=&result{eprintln!("{error}");}std::process::exit(if result.is_ok(){0}else{1});
    }
    if let Err(error)=device_grid::take_probe(&mut args){eprintln!("{error}");std::process::exit(2);}
    if let Err(error)=device_filter::take_probe(&mut args){eprintln!("{error}");std::process::exit(2);}
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
    if args.iter().any(|a| a == "--ssh-workspace") {
        std::process::exit(ssh_probe::run_workspace(&args[1..]));
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
    if let Err(error) = ui::graph::tracks::parse_probe(&mut args) {
        eprintln!("{error}");
        std::process::exit(2);
    }
    if let Err(error) = merge_keys::parse_probe(&mut args, headless) {
        eprintln!("{error}");
        std::process::exit(2);
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
    if args.get(1).is_some_and(|a| a == "--bench-progress") {
        std::process::exit(take_progress::bench(&args[2..]));
    }
    if args.iter().any(|a| a == "--bench") {
        // `--bench [frames] <file.tak>…`: open speed and frame time, up to 4 files side by side
        // (`headless::bench`, never a window).
        // `--overlay <b.tak>` (plan step 7, may repeat): the n-th laid over the n-th view.
        let (overlays, mut args) = shot::take_overlays(&args);
        let mut folder = None;
        if let Some(i) = args.iter().position(|a| a == "--bench-workspace") {
            let Some(path) = args.get(i + 1) else {
                eprintln!("--bench-workspace 뒤에 폴더 경로를 넣으세요.");
                std::process::exit(2);
            };
            folder = Some(workspace::resolve_path(path));
            args.drain(i..i + 2);
        }
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
        std::process::exit(headless::bench(files, overlays, frames.unwrap_or(300), [1600.0, 960.0], folder));
    }
    if let Err(error) = app::label_shot::take_probe(&mut args, headless) {
        eprintln!("{error}");
        std::process::exit(2);
    }
    if let Err(error) = app::scene_marker_menu::take_probe(&mut args, headless) {
        eprintln!("{error}");
        std::process::exit(2);
    }
    let tab_shot = app::tab_shot::TabShot::take(&mut args, headless)
        .unwrap_or_else(|error| cli_exit(2, error));
    let no_desktop = !headless && instances::no_desktop();
    let capture = match capture::parse(&args[1..]) {
        Ok(c) => c,
        Err(e) if e.is_empty() => {
            println!("{}", usage());
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
    if app::label_shot::enabled() && (initial.len() != 1 || auto.is_none()) {
        eprintln!("--shot-label requires one local take and --screenshot");
        std::process::exit(2);
    }
    if tab_shot.is_some() && (initial.len() != 1 || auto.is_none()) {
        cli_exit(2, "--shot-tabs는 --screenshot과 파일 하나를 함께 넣어 주세요.");
    }
    let size = capture.as_ref().map_or([1600.0, 960.0], |c| c.size);
    let command_run = capture.is_some() || auto.is_some();
    if no_desktop {
        if !command_run {
            cli_exit(2, window_command::NO_DESKTOP);
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
        std::process::exit(headless::run(initial, auto, capture, size, tab_shot));
    }
    run_window(initial, auto, capture, size, None)
}

/// Usage of every command, one place (each new command adds its text here).
fn usage() -> String {
    [
        capture::USAGE,
        "Headless label screenshot: tak_viewer --headless --screenshot out.png --range A-B --shot-label SOURCE=TARGET file.takm",
        "Headless 3D marker menu: --shot-marker-menu menu|info|label with --select and --frame",
        help_shot::USAGE,
        "Headless layout tabs: --shot-tabs N[:POINTS] with --screenshot and one take",
        "Headless Labeling link: --labeling-link with --select NAME",
        "Headless 3D selection: --select-visible or --select A,B[,C]",
        "Headless 3D camera: --scene-view perspective|front|back|left|right|top|bottom and --scene-follow",
        "Saved progress benchmark: tak_viewer --bench-progress file.tak [file.takm …]",
        "Workspace benchmark: tak_viewer --bench 600 file.tak --bench-workspace folder",
        "GRAPH screenshot or benchmark: --graph-layout channels|tracks|combined and --graph-zoom A-B",
        "Headless Tracks merge: --merge-keys preview|up|down with --graph-layout tracks and --select A,B",
        "Headless playback: --working-range A-B and --reverse-playing",
        "GRAPH screenshot: --graph-fit-range, --graph-y-zoom N and --graph-lock NAME[,NAME…] with --graph-layout",
        comment_command::USAGE,
        window_command::USAGE,
        summary_command::USAGE,
        tak_core::takm::USAGE,
        tak_core::takm::verify::USAGE,
        check_command::USAGE,
    ]
    .join("\n\n")
}

fn cli_exit(code: i32, message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(code);
}

fn run_window(
    initial: Vec<PathBuf>,
    auto: Option<shot::AutoShot>,
    capture: Option<capture::CapturePlan>,
    size: [f32; 2],
    startup: Option<window_command::StartupView>,
) -> eframe::Result<()> {
    let command_run = capture.is_some() || auto.is_some();
    let only = startup.is_some();
    let data = if command_run { None } else {
        instances::data_dir_override().unwrap_or_else(|error| cli_exit(2, &error))
    };
    let registry = if command_run { None } else { instances::registry_dir(data.as_deref()) };
    let registration = registry.and_then(|dir| match instances::register(&dir, only) {
        Ok(entry) => Some(entry),
        Err(error) if only => cli_exit(1, &format!("뷰어 창 목록에 이 창을 등록할 수 없어요. {error}")),
        Err(_) => None,
    });
    if only && registration.is_none() { cli_exit(1, "뷰어 창 목록 폴더를 찾을 수 없어요."); }
    let others_at_start = registration.as_ref().is_some_and(|r| r.others_alive());
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(version::window_title(None))
            .with_inner_size(size)
            .with_min_inner_size([900.0, 600.0])
            .with_drag_and_drop(true),
        persistence_path: data.map(|dir| dir.join("app.ron")),
        persist_window: !command_run && !only,
        ..Default::default()
    };
    eframe::run_native(
        "tak_viewer",
        options,
        Box::new(move |cc| {
            let mut app = app::TakViewerApp::new(cc, initial, auto, capture, startup);
            app.set_registration(registration, others_at_start);
            // WORKSPACE follows the disk in the user's window only; command runs
            // (`--screenshot`, `--capture`) never start the folder watcher.
            if !command_run {
                app.start_folder_watch();
            }
            if only {
                println!("{}", std::process::id());
            }
            Ok(Box::new(app))
        }),
    )
}
