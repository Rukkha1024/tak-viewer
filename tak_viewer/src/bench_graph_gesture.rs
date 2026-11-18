//! Exercise GRAPH with pointer events through the normal app frame, without a desktop.

use super::{frame_input, ms, BenchTrace, FrameTiming, TakViewerApp, Textures, CLEAR};
use crate::bench_timing;
use crate::ui::graph::tracks::{self, Layout};
use eframe::egui::{self, pos2, vec2, Event, Modifiers, PointerButton, Pos2, Rect};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SIZE: [usize; 2] = [1600, 960];
const FRAME: Duration = Duration::from_nanos(16_666_667);

#[derive(Clone, Copy, Debug)]
pub(crate) enum Gesture {
    Value,
    Wheel,
    Pan,
}

impl Gesture {
    pub(crate) fn parse(arg: &str) -> Option<Self> {
        match arg {
            "--bench-value-zoom" => Some(Self::Value),
            "--bench-time-zoom" => Some(Self::Wheel),
            "--bench-pan" => Some(Self::Pan),
            _ => None,
        }
    }

    fn button(self) -> Option<PointerButton> {
        match self {
            Self::Value => Some(PointerButton::Secondary),
            Self::Wheel => None,
            Self::Pan => Some(PointerButton::Middle),
        }
    }
}

struct Options {
    frames: usize,
    file: PathBuf,
    layout: Layout,
    markers: usize,
    detached: bool,
    trace: bool,
    screenshot: Option<PathBuf>,
}

impl Options {
    fn parse(args: &[String]) -> Option<Self> {
        let mut args = args.iter().map(String::as_str);
        let frames = args
            .next()?
            .parse::<usize>()
            .ok()
            .filter(|n| (4..=10_000).contains(n))?;
        let mut opt = Self {
            frames,
            file: crate::workspace::resolve_path(args.next()?),
            layout: Layout::Channels,
            markers: 1,
            detached: false,
            trace: false,
            screenshot: None,
        };
        while let Some(arg) = args.next() {
            match arg {
                "--graph-layout" => {
                    opt.layout = match args.next()? {
                        "channels" => Layout::Channels,
                        "combined" => Layout::Combined,
                        _ => return None,
                    }
                }
                "--bench-select" => {
                    opt.markers = args
                        .next()?
                        .parse::<usize>()
                        .ok()
                        .filter(|n| (1..=8).contains(n))?
                }
                "--bench-detached" => opt.detached = true,
                "--bench-trace" => opt.trace = true,
                "--bench-screenshot" => {
                    let path = crate::workspace::resolve_path(args.next()?);
                    if !path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
                    {
                        return None;
                    }
                    opt.screenshot = Some(path);
                }
                _ => return None,
            }
        }
        if !opt.file.is_file() || !tak_core::tak::is_take_path(&opt.file) {
            return None;
        }
        Some(opt)
    }
}

pub(crate) fn run(gesture: Gesture, args: &[String]) -> i32 {
    let Some(opt) = Options::parse(args) else {
        eprintln!("GRAPH 검사에는 프레임 수(4~10000), 로컬 TAK 파일, Channels 또는 Combined 배치를 써요. 그림 경로는 .png여야 해요.");
        return 2;
    };
    match measure(gesture, opt) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("GRAPH 검사를 마치지 못했어요. {error}");
            1
        }
    }
}

fn button(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

fn capture(
    path: &Path,
    out: egui::FullOutput,
    ctx: &egui::Context,
    textures: &Textures,
) -> std::io::Result<()> {
    let ppp = out.pixels_per_point;
    let primitives = ctx.tessellate(out.shapes, ppp);
    let image = crate::soft_render::paint(&primitives, textures, SIZE, ppp, CLEAR);
    let rgba: Vec<_> = image
        .pixels
        .iter()
        .flat_map(|pixel| pixel.to_array())
        .collect();
    crate::shot::save_png(path, SIZE[0], SIZE[1], &rgba)
}

fn measure(gesture: Gesture, opt: Options) -> Result<(), String> {
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.set_command_run();
    let start = Instant::now();
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(SIZE[0] as f32, SIZE[1] as f32));
    let mut textures = Textures::default();
    let draw = |app: &mut TakViewerApp, events| {
        ctx.run(frame_input(screen, start, events), |ctx| {
            let _span = bench_timing::span("app.frame");
            app.frame(ctx);
        })
    };
    let settle = |app: &mut TakViewerApp,
                  textures: &mut Textures,
                  events,
                  shot: Option<&Path>|
     -> Result<(), String> {
        let out = draw(app, events);
        if opt.screenshot.is_some() {
            textures.set(&out.textures_delta);
        }
        if let Some(path) = shot {
            return capture(path, out, &ctx, textures).map_err(|e| e.to_string());
        }
        let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
        textures.free(&out.textures_delta);
        std::thread::sleep(Duration::from_millis(5));
        Ok(())
    };

    app.bench_open(&ctx, std::slice::from_ref(&opt.file));
    loop {
        settle(&mut app, &mut textures, vec![], None)?;
        let state = app.bench_state();
        if let Some(error) = state.error {
            return Err(error);
        }
        if state.loaded && !state.analog_pending {
            break;
        }
        if start.elapsed() > super::TIMEOUT {
            return Err("파일을 여는 시간이 너무 오래 걸렸어요.".into());
        }
    }
    let file_id = app
        .bench_graph_pane(
            &ctx,
            opt.layout,
            opt.detached,
            opt.markers,
            matches!(gesture, Gesture::Pan),
        )
        .ok_or("고를 수 있는 마커가 부족해요.")?;
    let plot_id = tracks::graph_id(file_id).with("plot");
    let origin = loop {
        settle(&mut app, &mut textures, vec![], None)?;
        if let Some(resp) = ctx
            .read_response(plot_id)
            .filter(|_| app.bench_graph_ready())
        {
            break pos2(
                resp.interact_rect.center().x,
                resp.interact_rect.top() + 12.0,
            );
        }
        if start.elapsed() > super::TIMEOUT {
            return Err("그래프를 준비하는 시간이 너무 오래 걸렸어요.".into());
        }
    };
    settle(
        &mut app,
        &mut textures,
        vec![Event::PointerMoved(origin)],
        None,
    )?;
    if let Some(b) = gesture.button() {
        settle(&mut app, &mut textures, vec![button(origin, b, true)], None)?;
    }

    let mut trace = BenchTrace::new(opt.trace, opt.frames);
    let mut sum: f64 = 0.0;
    let mut max: f64 = 0.0;
    let mut changed = 0;
    let mut pos = origin;
    let mut before_zoom = app.bench_graph_zoom(opt.layout);
    let mut before_extent = tracks::zoom_extent(&ctx, file_id);
    let begin = Instant::now();
    for step in 0..opt.frames {
        let up = step < opt.frames / 2;
        let events = match gesture {
            Gesture::Value => {
                // Up, then back down part way so the release stays visibly magnified.
                pos.y += if up {
                    -72.0 / (opt.frames / 2) as f32
                } else {
                    40.0 / (opt.frames - opt.frames / 2) as f32
                };
                vec![Event::PointerMoved(pos)]
            }
            Gesture::Wheel => vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, if up { 6.0 } else { -4.0 }),
                modifiers: Modifiers::NONE,
            }],
            Gesture::Pan => {
                pos.x += if up { 7.0 } else { -7.0 };
                vec![Event::PointerMoved(pos)]
            }
        };
        let due = begin + FRAME * step as u32;
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
        let cpu_start = trace.cpu_mark();
        bench_timing::begin_details(trace.enabled());
        let t = Instant::now();
        let out = draw(&mut app, events);
        let ui = ms(t);
        let cpu_ui = trace.cpu_mark();
        let shapes = out.shapes.len();
        let tess = bench_timing::span("epaint.tessellate");
        let primitives = ctx.tessellate(out.shapes, out.pixels_per_point);
        drop(tess);
        let primitive_count = primitives.len();
        drop(primitives);
        let wall = ms(t);
        let cpu_end = trace.cpu_mark();
        sum += wall;
        max = max.max(wall);
        if trace.enabled() {
            let mut record = FrameTiming::new(
                step,
                "gesture",
                ms(begin),
                shapes,
                primitive_count,
                ui,
                wall,
                [cpu_start, cpu_ui, cpu_end],
            );
            record.ui_passes = out.platform_output.num_completed_passes;
            record.details = bench_timing::take_details();
            trace.push(record);
        }
        if opt.screenshot.is_some() {
            textures.set(&out.textures_delta);
            textures.free(&out.textures_delta);
        }
        let zoom = app.bench_graph_zoom(opt.layout);
        let extent = tracks::zoom_extent(&ctx, file_id);
        changed += usize::from(match gesture {
            Gesture::Value => before_zoom != zoom,
            _ => before_extent != extent,
        });
        before_zoom = zoom;
        before_extent = extent;
    }
    println!("GRAPH {gesture:?} {:?} {} markers={} frames={} changed={changed} avg_ms={:.3} max_ms={max:.3}",
        opt.layout, if opt.detached { "detached" } else { "docked" }, opt.markers, opt.frames, sum / opt.frames as f64);
    trace.print();
    if changed == 0 {
        return Err("그래프 범위가 한 번도 바뀌지 않았어요.".into());
    }

    let events = gesture
        .button()
        .map_or_else(Vec::new, |b| vec![button(pos, b, false)]);
    let released = Instant::now();
    settle(&mut app, &mut textures, events, None)?;
    settle(&mut app, &mut textures, vec![Event::PointerGone], None)?;
    while !app.bench_graph_ready() {
        settle(&mut app, &mut textures, vec![], None)?;
        if released.elapsed() > Duration::from_secs(30) {
            return Err("마지막 그림을 준비하지 못했어요.".into());
        }
    }
    println!(
        "settled_ms={:.3} value={before_zoom:?} time={before_extent:?}",
        ms(released)
    );
    if let Some(path) = &opt.screenshot {
        settle(&mut app, &mut textures, vec![], Some(path))?;
    }
    Ok(())
}
