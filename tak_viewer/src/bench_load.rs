//! Load-only benchmark: retain up to twelve independent display models without creating UI
//! views. The real UI allows four views per layout tab (`--bench` measures one layout).
//! Memory is the OS process resident/working set, never a sum of Rust buffer estimates.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use std::time::{Duration, Instant};

use serde::Serialize;
use tak_core::tak::fast_read::{read_take_fast, AnalogJob};
use tak_core::tak::{AnalogDevice, Take};
use tak_core::view_data::{AnalogPart, AnalogTarget, ViewData};

pub const USAGE: &str = "tak_viewer --bench-load <a.tak|a.takm> [more files …] (1–12 files; load only, maximum 4 views per layout tab)";
const MAX_FILES: usize = 12;
const TIMEOUT: Duration = Duration::from_secs(600);

/// All values are bytes. The peak is the OS high-water mark for this process's lifetime,
/// including transient buffers between our readiness samples.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ProcessMemory {
    pub resident_bytes: u64,
    pub peak_resident_bytes: u64,
}

impl ProcessMemory {
    pub fn sample() -> Option<Self> {
        native_memory()
    }

    pub fn describe(value: Option<Self>) -> String {
        value.map_or_else(|| "unavailable on this platform".to_owned(), |m| {
            format!("resident {} bytes ({:.2} MiB), process lifetime peak {} bytes ({:.2} MiB)", m.resident_bytes, m.resident_bytes as f64 / 1_048_576.0, m.peak_resident_bytes, m.peak_resident_bytes as f64 / 1_048_576.0)
        })
    }
}

#[cfg(target_os = "linux")]
fn native_memory() -> Option<ProcessMemory> {
    parse_linux_status(&std::fs::read_to_string("/proc/self/status").ok()?)
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_status(text: &str) -> Option<ProcessMemory> {
    let bytes = |key: &str| -> Option<u64> {
        let line = text.lines().find_map(|s| s.strip_prefix(key))?;
        let mut fields = line.split_whitespace();
        let n = fields.next()?.parse::<u64>().ok()?;
        if fields.next()? != "kB" { return None; }
        n.checked_mul(1024)
    };
    Some(ProcessMemory { resident_bytes: bytes("VmRSS:")?, peak_resident_bytes: bytes("VmHWM:")? })
}

#[cfg(windows)]
fn native_memory() -> Option<ProcessMemory> {
    // PROCESS_MEMORY_COUNTERS, documented by Windows PSAPI. SIZE_T follows pointer width.
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" { fn GetCurrentProcess() -> *mut std::ffi::c_void; }
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(process: *mut std::ffi::c_void, counters: *mut Counters, size: u32) -> i32;
    }
    // SAFETY: Counters is the C layout, its initialized buffer and byte count agree, and the
    // current-process pseudo handle is valid without ownership or CloseHandle.
    unsafe {
        let mut counters: Counters = std::mem::zeroed();
        counters.cb = std::mem::size_of::<Counters>() as u32;
        let size = counters.cb;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) == 0 { return None; }
        Some(ProcessMemory { resident_bytes: counters.working_set_size as u64, peak_resident_bytes: counters.peak_working_set_size as u64 })
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn native_memory() -> Option<ProcessMemory> { None }

pub fn parse(args: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut seen = false;
    for arg in args {
        match arg.as_str() {
            "--bench-load" if !seen => seen = true,
            "--headless" => (),
            flag if flag.starts_with("--") => return Err(format!("--bench-load와 함께 쓸 수 없는 옵션: {flag}")),
            _ => files.push(PathBuf::from(arg)),
        }
    }
    if !seen || files.is_empty() || files.len() > MAX_FILES {
        return Err("--bench-load에는 파일을 1개부터 12개까지 넣어요.".to_owned());
    }
    if let Some(path) = files.iter().find(|p| !tak_core::tak::is_take_path(p) && !tak_core::takm::is_takm_path(p)) {
        return Err(format!("TAK 또는 TAKM 파일을 넣어요: {}", path.display()));
    }
    Ok(files)
}

pub fn validate_ui_count(files: usize, overlays: usize) -> Result<(), &'static str> {
    if files == 0 || files > 4 || overlays > files {
        Err("--bench는 3D 화면을 4개까지 열고 화면마다 파일 하나를 겹쳐 볼 수 있어요. 12개 동시 읽기는 --bench-load를 써요.")
    } else { Ok(()) }
}

struct StartupPlan {
    job: AnalogJob,
    lazy: Option<AnalogJob>,
    deferred_devices: usize,
    fallback: Option<String>,
    indices: Option<Vec<usize>>,
}

// BASELINE_ADAPT_START: On v0.32.7 replace these three helper bodies as noted. Everything
// else (barrier, retained ViewData, timings and native memory) stays identical.
// Baseline: StartupPlan { job, lazy: None, deferred_devices: 0, fallback: None, indices: None }
fn startup_plan(job: AnalogJob) -> StartupPlan {
    let (job, lazy) = job.split();
    let deferred_devices = lazy.as_ref().map_or(0, |j| j.device_indices().len());
    let fallback = job.fallback_reason().map(str::to_owned);
    let indices = job.plate_devices().is_some().then(|| job.device_indices().to_vec());
    StartupPlan { job, lazy, deferred_devices, fallback, indices }
}

// Baseline: ViewData::build(take)
fn marker_view(take: Take, job: Option<&AnalogJob>) -> ViewData {
    let pending = job.is_some_and(|j| j.plate_devices().is_some_and(|plates| j.device_indices().iter().any(|i| !plates.contains(i))));
    let mut data = ViewData::build_with_plate_devices(take, job.and_then(|j| j.plate_devices()).map(<[usize]>::to_vec));
    data.emg_pending = pending;
    data
}

// Baseline: AnalogPart::prepare(analog, warning, target)
fn prepare_startup(analog: Vec<AnalogDevice>, warning: Option<String>, target: &AnalogTarget, indices: Option<Vec<usize>>) -> AnalogPart {
    match indices {
        Some(indices) => AnalogPart::prepare_selected(analog, warning, target, indices, true),
        None => AnalogPart::prepare(analog, warning, target),
    }
}
// BASELINE_ADAPT_END

#[derive(Serialize)]
struct FileReport {
    file: String,
    file_bytes: u64,
    markers_ready_ms: f64,
    forceplates_ready_ms: f64,
    markers_memory: Option<ProcessMemory>,
    forceplates_memory: Option<ProcessMemory>,
    deferred_analog_devices: usize,
    analog_values_loaded: usize,
    full_read_fallback: bool,
    analog_fallback_reason: Option<String>,
    steps_ms: Vec<(&'static str, f64)>,
}

struct Retained {
    data: ViewData,
    _lazy: Option<AnalogJob>,
}

struct MarkerLoad(Arc<AtomicUsize>);
impl Drop for MarkerLoad {
    fn drop(&mut self) { self.0.fetch_sub(1, Ordering::SeqCst); }
}

fn load_one(path: PathBuf, opened: Instant, pending: Arc<AtomicUsize>, marker_guard: MarkerLoad) -> Result<(FileReport, Retained), String> {
    let fast = read_take_fast(&path, false).map_err(|e| e.to_string())?;
    let view_started = Instant::now();
    let mut data = marker_view(fast.take, fast.analog.as_ref());
    let plan = fast.analog.map(startup_plan);
    let full_read_fallback = plan.is_none() && fast.times.iter().any(|(name, _)| name.contains("fallback"));
    data.analog_pending = plan.is_some();
    let mut steps_ms = fast.times;
    steps_ms.push(("view", elapsed_ms(view_started)));
    let markers_ready_ms = elapsed_ms(opened);
    let markers_memory = ProcessMemory::sample();
    // Match the viewer: all marker work has priority over startup analog work.
    drop(marker_guard);
    if plan.is_some() {
        while pending.load(Ordering::SeqCst) != 0 { std::thread::sleep(Duration::from_millis(1)); }
    }
    let (lazy, deferred_analog_devices, analog_fallback_reason) = match plan {
        Some(plan) => {
            let analog_started = Instant::now();
            let (analog, warning) = plan.job.run().map_err(|e| e.to_string())?;
            let part = prepare_startup(analog, warning, &data.analog_target(), plan.indices);
            data.attach_prepared(part);
            steps_ms.push(("startup analog", elapsed_ms(analog_started)));
            (plan.lazy, plan.deferred_devices, plan.fallback)
        }
        None => (None, 0, None),
    };
    let forceplates_ready_ms = elapsed_ms(opened);
    let forceplates_memory = ProcessMemory::sample();
    let analog_values_loaded = data.take.analog.iter().flat_map(|d| &d.channels).map(|c| c.values.len()).sum();
    let report = FileReport {
        file: path.display().to_string(), file_bytes: data.take.file_size,
        markers_ready_ms, forceplates_ready_ms, markers_memory, forceplates_memory,
        deferred_analog_devices, analog_values_loaded, full_read_fallback,
        analog_fallback_reason, steps_ms,
    };
    Ok((report, Retained { data, _lazy: lazy }))
}

fn elapsed_ms(start: Instant) -> f64 { start.elapsed().as_secs_f64() * 1000.0 }

/// The process exits directly after this function; no UI, preferences or data are written.
pub fn run(files: Vec<PathBuf>) -> i32 {
    let baseline = ProcessMemory::sample();
    let count = files.len();
    let barrier = Arc::new(Barrier::new(count + 1));
    let pending = Arc::new(AtomicUsize::new(count));
    let (tx, rx) = mpsc::channel();
    let mut threads = Vec::new();
    let opened = Instant::now();
    for (index, file) in files.into_iter().enumerate() {
        let (tx, barrier, pending) = (tx.clone(), barrier.clone(), pending.clone());
        threads.push(std::thread::spawn(move || {
            let guard = MarkerLoad(pending.clone());
            barrier.wait();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load_one(file, opened, pending, guard)))
                .unwrap_or_else(|_| Err("읽는 중 내부 오류(panic)가 났어요.".to_owned()));
            let _ = tx.send((index, result));
        }));
    }
    drop(tx);
    barrier.wait();
    let mut reports: Vec<Option<FileReport>> = (0..count).map(|_| None).collect();
    let mut retained = Vec::new();
    let mut failed = false;
    for _ in 0..count {
        let remaining = TIMEOUT.saturating_sub(opened.elapsed());
        match rx.recv_timeout(remaining) {
            Ok((index, Ok((report, data)))) => { reports[index] = Some(report); retained.push(data); }
            Ok((index, Err(error))) => { eprintln!("bench-load: file {}: {error}", index + 1); failed = true; }
            Err(error) => { eprintln!("bench-load: {error} (limit {} s)", TIMEOUT.as_secs()); return 1; }
        }
    }
    for thread in threads { if thread.join().is_err() { failed = true; } }
    // Keep every model and deferred job alive through the total memory sample.
    std::hint::black_box(&retained);
    let final_memory = ProcessMemory::sample();
    let reports: Vec<FileReport> = reports.into_iter().flatten().collect();
    let all_markers_ms = reports.iter().map(|r| r.markers_ready_ms).fold(0.0, f64::max);
    let all_forceplates_ms = reports.iter().map(|r| r.forceplates_ready_ms).fold(0.0, f64::max);
    println!("bench-load: {} of {count} file(s) retained concurrently; no UI rendered; 3D UI maximum 4", retained.len());
    println!("  memory: actual whole-process resident/working set; readiness samples overlap across files, not individual file allocation sizes");
    println!("  baseline (no files, before workers): {}", ProcessMemory::describe(baseline));
    for report in &reports {
        println!("  {} ({} bytes)", report.file, report.file_bytes);
        println!("    markers ready: {:.1} ms; forceplates ready: {:.1} ms (not on-screen timing)", report.markers_ready_ms, report.forceplates_ready_ms);
        println!("    process at markers: {}", ProcessMemory::describe(report.markers_memory));
        println!("    process at forceplates: {}", ProcessMemory::describe(report.forceplates_memory));
        println!("    deferred analog devices: {}; loaded analog values: {}; full read fallback: {}", report.deferred_analog_devices, report.analog_values_loaded, report.full_read_fallback);
        if let Some(reason) = &report.analog_fallback_reason { println!("    analog fallback: {reason}"); }
    }
    println!("  all markers ready: {all_markers_ms:.1} ms; all forceplates ready: {all_forceplates_ms:.1} ms");
    println!("  total (all completed models and lazy jobs held): {}", ProcessMemory::describe(final_memory));
    let resident_delta = baseline.zip(final_memory).map(|(a, b)| b.resident_bytes as i128 - a.resident_bytes as i128);
    if let Some(delta) = resident_delta {
        println!("  total resident delta from no-file baseline: {delta} bytes ({:.2} MiB); mean per retained file: {:.2} MiB (batch mean, not individual attribution)", delta as f64 / 1_048_576.0, delta as f64 / 1_048_576.0 / retained.len().max(1) as f64);
    }
    println!("bench-load-json: {}", serde_json::json!({
        "schema": 1, "requested_files": count, "retained_files": retained.len(), "ui_rendered": false,
        "ui_file_cap": 4, "memory_semantics": "whole-process resident snapshots; peak is process lifetime high-water; per-file snapshots overlap",
        "all_markers_ready_ms": all_markers_ms, "all_forceplates_ready_ms": all_forceplates_ms,
        "baseline_memory": baseline, "final_memory": final_memory, "resident_delta_bytes": resident_delta.map(|n| n as i64),
        "retained_marker_count": retained.iter().map(|r| r.data.markers.len()).sum::<usize>(),
        "files": reports,
    }));
    i32::from(failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> { values.iter().map(|s| (*s).to_owned()).collect() }

    #[test]
    fn load_parse_accepts_twelve_files_and_takm_without_a_window() {
        let mut input = args(&["--headless", "--bench-load", "a.TAKM"]);
        input.extend((1..12).map(|n| format!("file {n}.tak")));
        assert_eq!(parse(&input).unwrap().len(), 12);
        input.push("thirteen.tak".to_owned());
        assert!(parse(&input).is_err());
    }

    #[test]
    fn load_parse_rejects_empty_unknown_or_render_options() {
        for input in [args(&["--bench-load"]), args(&["--bench-load", "a.txt"]), args(&["--bench-load", "a.tak", "--overlay", "b.tak"]), args(&["--bench-load", "--bench", "a.tak"])] {
            assert!(parse(&input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn ui_bench_accepts_four_views_with_one_overlay_each() {
        assert!(validate_ui_count(4, 0).is_ok());
        assert!(validate_ui_count(2, 2).is_ok());
        assert!(validate_ui_count(4, 4).is_ok());
        for pair in [(12, 0), (5, 0), (4, 5), (1, 2), (0, 0)] { assert!(validate_ui_count(pair.0, pair.1).is_err()); }
    }

    #[test]
    fn linux_resident_and_peak_are_bytes_not_virtual_size() {
        let memory = parse_linux_status("Name:\ttak_viewer\nVmSize:\t9999 kB\nVmHWM:\t123 kB\nVmRSS:\t100 kB\n").unwrap();
        assert_eq!(memory.resident_bytes, 102400);
        assert_eq!(memory.peak_resident_bytes, 125952);
        assert!(parse_linux_status("VmRSS: 1 MB\nVmHWM: 2 kB").is_none());
        assert!(parse_linux_status("VmRSS: 1 kB").is_none());
        assert!(parse_linux_status("VmRSS: 18446744073709551615 kB\nVmHWM: 2 kB").is_none());
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn native_memory_reports_a_real_resident_set_and_lifetime_peak() {
        let memory = ProcessMemory::sample().expect("native process memory query");
        assert!(memory.resident_bytes > 0);
        assert!(memory.peak_resident_bytes >= memory.resident_bytes);
    }
}
