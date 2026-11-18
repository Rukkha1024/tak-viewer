//! Opt-in benchmark diagnostics. Main-thread CPU clocks distinguish CPU work from time off
//! CPU; the latter can include scheduling, blocking and clock granularity, not just preemption.
//! No clock syscalls, trace allocation or trace output when disabled. Output waits until the
//! benchmark finishes, so terminal/file I/O is outside the measured frames.

use serde::Serialize;
use std::{cell::RefCell, time::Instant};

#[derive(Debug, Serialize)]
pub struct Detail {
    pub name: &'static str,
    pub view: Option<usize>,
    pub wall_ms: f64,
    pub self_ms: f64,
    pub points: usize,
    pub vertices: usize,
    pub background: bool,
}
#[derive(Default)]
struct DetailFrame {
    enabled: bool,
    view: Option<usize>,
    children: Vec<f64>,
    events: Vec<Detail>,
}
thread_local! { static DETAIL: RefCell<DetailFrame> = RefCell::new(DetailFrame::default()); }

pub fn enabled() -> bool {
    DETAIL.with(|s| s.borrow().enabled)
}

pub fn begin_details(enabled: bool) {
    DETAIL.with(|s| {
        let mut s = s.borrow_mut();
        s.enabled = enabled;
        s.view = None;
        s.children.clear();
        s.events.clear();
    });
}
pub fn take_details() -> Vec<Detail> {
    DETAIL.with(|s| {
        let mut s = s.borrow_mut();
        s.enabled = false;
        std::mem::take(&mut s.events)
    })
}
pub fn background(name: &'static str, wall_ms: f64) {
    DETAIL.with(|s| {
        let mut s = s.borrow_mut();
        if s.enabled {
            let view = s.view;
            s.events.push(Detail {
                name,
                view,
                wall_ms,
                self_ms: 0.0,
                points: 0,
                vertices: 0,
                background: true,
            });
        }
    });
}
pub struct Span {
    start: Option<Instant>,
    name: &'static str,
    view: Option<usize>,
    previous: Option<usize>,
    points: usize,
    vertices: usize,
}
pub fn span(name: &'static str) -> Span {
    span_view(name, None)
}
pub fn span_view(name: &'static str, view: Option<usize>) -> Span {
    DETAIL.with(|s| {
        let mut s = s.borrow_mut();
        let previous = s.view;
        if s.enabled {
            if view.is_some() {
                s.view = view;
            }
            s.children.push(0.0);
        }
        Span {
            start: s.enabled.then(Instant::now),
            name,
            view: s.view,
            previous,
            points: 0,
            vertices: 0,
        }
    })
}
impl Span {
    pub fn counts(&mut self, f: impl FnOnce() -> (usize, usize)) {
        if self.start.is_some() {
            (self.points, self.vertices) = f();
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        let Some(start) = self.start else { return };
        let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
        DETAIL.with(|s| {
            let mut s = s.borrow_mut();
            let child = s.children.pop().unwrap_or(0.0);
            if let Some(parent) = s.children.last_mut() {
                *parent += wall_ms;
            }
            s.events.push(Detail {
                name: self.name,
                view: self.view,
                wall_ms,
                self_ms: (wall_ms - child).max(0.0),
                points: self.points,
                vertices: self.vertices,
                background: false,
            });
            s.view = self.previous;
        });
    }
}

#[derive(Debug, Serialize)]
pub struct FrameTiming {
    pub frame: usize,
    pub phase: &'static str,
    pub since_open_ms: f64,
    pub ui_passes: usize,
    pub shapes: usize,
    pub primitives: usize,
    pub path_points: usize,
    pub cached_mesh_vertices: usize,
    pub output_vertices: usize,
    pub details: Vec<Detail>,
    pub wall_ms: f64,
    pub ui_wall_ms: f64,
    pub tessellation_wall_ms: f64,
    pub thread_cpu_ms: Option<f64>,
    pub ui_thread_cpu_ms: Option<f64>,
    pub tessellation_thread_cpu_ms: Option<f64>,
    pub off_cpu_ms: Option<f64>,
    pub ui_off_cpu_ms: Option<f64>,
    pub tessellation_off_cpu_ms: Option<f64>,
}

impl FrameTiming {
    pub fn new(
        frame: usize,
        phase: &'static str,
        since_open_ms: f64,
        shapes: usize,
        primitives: usize,
        ui_wall_ms: f64,
        wall_ms: f64,
        cpu: [Option<u64>; 3],
    ) -> Self {
        let elapsed = |a: Option<u64>, b: Option<u64>| {
            a.zip(b)
                .and_then(|(a, b)| b.checked_sub(a))
                .map(|n| n as f64 / 1_000_000.0)
        };
        let thread_cpu_ms = elapsed(cpu[0], cpu[2]);
        let ui_thread_cpu_ms = elapsed(cpu[0], cpu[1]);
        let tessellation_thread_cpu_ms = elapsed(cpu[1], cpu[2]);
        let tessellation_wall_ms = (wall_ms - ui_wall_ms).max(0.0);
        Self {
            frame,
            phase,
            since_open_ms,
            ui_passes: 0,
            shapes,
            primitives,
            path_points: 0,
            cached_mesh_vertices: 0,
            output_vertices: 0,
            details: vec![],
            wall_ms,
            ui_wall_ms,
            tessellation_wall_ms,
            thread_cpu_ms,
            ui_thread_cpu_ms,
            tessellation_thread_cpu_ms,
            off_cpu_ms: thread_cpu_ms.map(|n| (wall_ms - n).max(0.0)),
            ui_off_cpu_ms: ui_thread_cpu_ms.map(|n| (ui_wall_ms - n).max(0.0)),
            tessellation_off_cpu_ms: tessellation_thread_cpu_ms
                .map(|n| (tessellation_wall_ms - n).max(0.0)),
        }
    }
}

pub struct BenchTrace {
    enabled: bool,
    records: Vec<FrameTiming>,
    dropped: usize,
}

impl BenchTrace {
    pub fn new(enabled: bool, frames: usize) -> Self {
        Self {
            enabled,
            records: if enabled {
                Vec::with_capacity(frames.saturating_add(1024).min(100_000))
            } else {
                vec![]
            },
            dropped: 0,
        }
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn cpu_mark(&self) -> Option<u64> {
        if self.enabled {
            thread_cpu_ns()
        } else {
            None
        }
    }
    pub fn push(&mut self, record: FrameTiming) {
        if !self.enabled {
            return;
        }
        if self.records.len() < 100_000 {
            self.records.push(record);
        } else {
            self.dropped += 1;
        }
    }
    pub fn print(&self) {
        if !self.enabled {
            return;
        }
        println!("  timing trace (--bench-trace): wall time and native main-thread CPU per UI/tessellation phase; off-CPU = max(wall - CPU, 0), includes preemption/blocking/clock granularity; GPU rendering excluded");
        println!("  native thread CPU clock: {}", cpu_clock_name());
        for r in self.records.iter().filter(|r| r.wall_ms > 16.0) {
            let value =
                |n: Option<f64>| n.map_or_else(|| "unavailable".to_owned(), |n| format!("{n:.3}"));
            println!("  slow frame {} ({}): wall {:.3} ms, thread CPU {} ms, off-CPU {} ms; UI wall {:.3}/CPU {}/off {} ({} passes); tessellation wall {:.3}/CPU {}/off {}; shapes {}, primitives {}", r.frame, r.phase, r.wall_ms, value(r.thread_cpu_ms), value(r.off_cpu_ms), r.ui_wall_ms, value(r.ui_thread_cpu_ms), value(r.ui_off_cpu_ms), r.ui_passes, r.tessellation_wall_ms, value(r.tessellation_thread_cpu_ms), value(r.tessellation_off_cpu_ms), r.shapes, r.primitives);
        }
        println!(
            "bench-trace-json: {}",
            serde_json::json!({"schema": 1, "cpu_clock": cpu_clock_name(), "off_cpu_meaning": "wall minus main-thread CPU; scheduling, blocking and clock granularity cannot be separated", "dropped_frames": self.dropped, "frames": self.records})
        );
    }
}

#[cfg(target_os = "linux")]
fn cpu_clock_name() -> &'static str {
    "clock_gettime(CLOCK_THREAD_CPUTIME_ID)"
}
#[cfg(windows)]
fn cpu_clock_name() -> &'static str {
    "GetThreadTimes (native accounting can be coarser than the wall clock)"
}
#[cfg(not(any(target_os = "linux", windows)))]
fn cpu_clock_name() -> &'static str {
    "unavailable on this platform"
}

#[cfg(target_os = "linux")]
fn thread_cpu_ns() -> Option<u64> {
    #[repr(C)]
    struct Timespec {
        sec: std::os::raw::c_long,
        nsec: std::os::raw::c_long,
    }
    extern "C" {
        fn clock_gettime(clock: i32, out: *mut Timespec) -> i32;
    }
    let mut time = Timespec { sec: 0, nsec: 0 };
    // SAFETY: Linux clock ID 3 is CLOCK_THREAD_CPUTIME_ID; time is a valid C timespec buffer.
    if unsafe { clock_gettime(3, &mut time) } != 0 || time.sec < 0 || time.nsec < 0 {
        return None;
    }
    (time.sec as u64)
        .checked_mul(1_000_000_000)?
        .checked_add(time.nsec as u64)
}

#[cfg(windows)]
fn thread_cpu_ns() -> Option<u64> {
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> *mut std::ffi::c_void;
        fn GetThreadTimes(
            thread: *mut std::ffi::c_void,
            created: *mut FileTime,
            exited: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }
    let (mut created, mut exited, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    // SAFETY: the current-thread pseudo handle is valid and all FILETIME outputs point at
    // initialized C-layout storage. The pseudo handle does not need CloseHandle.
    if unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return None;
    }
    let ticks = |t: FileTime| (t.high as u64) << 32 | t.low as u64;
    ticks(kernel).checked_add(ticks(user))?.checked_mul(100)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn thread_cpu_ns() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detailed_trace_is_disabled_by_default_and_nested_times_do_not_double_count() {
        begin_details(false);
        span("disabled").counts(||panic!("disabled trace must not evaluate counts"));
        assert!(take_details().is_empty());
        begin_details(true);
        {let _parent=span_view("parent",Some(2));{let mut child=span("child");child.counts(||(12,34));}background("worker",25.0);}
        let events=take_details();
        assert_eq!(events.len(),3);
        assert_eq!((events[0].name,events[0].view,events[0].points,events[0].vertices),("child",Some(2),12,34));
        assert!(events[1].background);
        assert!((events[2].self_ms+events[0].wall_ms-events[2].wall_ms).abs()<0.000001);
    }
    #[test]
    fn trace_keeps_wall_outliers_and_separates_cpu_from_off_cpu_by_phase() {
        let record = FrameTiming::new(
            7,
            "emg",
            100.0,
            42,
            3,
            45.0,
            49.62,
            [Some(1_000_000), Some(6_000_000), Some(10_000_000)],
        );
        assert_eq!(record.wall_ms, 49.62);
        assert_eq!(record.ui_thread_cpu_ms, Some(5.0));
        assert_eq!(record.tessellation_thread_cpu_ms, Some(4.0));
        assert_eq!(record.ui_off_cpu_ms, Some(40.0));
        assert!((record.off_cpu_ms.unwrap() - 40.62).abs() < 1e-10);
    }
    #[test]
    fn unavailable_or_coarse_cpu_clock_is_not_reported_as_negative_wait() {
        let unavailable = FrameTiming::new(0, "startup", 0.0, 0, 0, 1.0, 2.0, [None; 3]);
        assert!(unavailable.off_cpu_ms.is_none());
        let coarse = FrameTiming::new(
            0,
            "playing",
            0.0,
            0,
            0,
            1.0,
            2.0,
            [Some(0), Some(15_000_000), Some(15_000_000)],
        );
        assert_eq!(coarse.thread_cpu_ms, Some(15.0));
        assert_eq!(coarse.off_cpu_ms, Some(0.0));
    }
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn native_thread_cpu_clock_is_monotonic() {
        let first = thread_cpu_ns().expect("native thread CPU clock");
        let second = thread_cpu_ns().expect("native thread CPU clock");
        assert!(second >= first);
        let trace = BenchTrace::new(false, 600);
        assert!(trace.cpu_mark().is_none());
        assert_eq!(trace.records.capacity(), 0);
    }
}
