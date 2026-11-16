//! Agent-only opt-in probe. Every network operation goes through the real app's SSH worker.
use crate::{app::TakViewerApp, headless, shot, ssh::parent_path, workspace};
use eframe::egui;
use serde_json::json;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub struct Probe {
    pub alias: String,
    pub platform: String,
    pub python: String,
    pub path: String,
    pub list_only: bool,
    pub cancel_after: Option<Duration>,
    pub dump: Option<PathBuf>,
    stall_after: Option<Duration>,
    last_bytes: u64,
    last_received: Instant,
    last_progress: Instant,
    start: Instant,
    opened: Option<Instant>,
    connected: Option<f64>,
    listed: Option<f64>,
    markers: Option<f64>,
    cancelled: bool,
    cancel_started: Option<Instant>,
    pub finished: bool,
}

pub struct State {
    pub connected: bool,
    pub listing: Option<serde_json::Value>,
    pub error: Option<String>,
    pub diagnostic: Option<String>,
    pub markers: bool,
    pub pending: bool,
    pub busy: bool,
    pub workers: bool,
    pub bytes: u64,
    pub python: Option<String>,
}
pub enum Step {
    Wait,
    Escape,
    Done(i32),
    Capture,
}

impl Probe {
    pub fn folder(&self) -> String {
        if self.list_only {
            self.path.clone()
        } else {
            parent_path(&self.path)
        }
    }
    pub fn start(&mut self, app: &mut TakViewerApp, ctx: &egui::Context) -> Result<(), String> {
        self.start = Instant::now();
        self.last_received = self.start;
        self.last_progress = self.start;
        app.ssh_probe_start(ctx, self)
    }
    fn ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
    fn print(&self, stage: &str, state: &State) {
        println!(
            "{}",
            json!({"stage":stage,"elapsed_ms":self.ms(),"connect_ms":self.connected,"list_ms":self.listed,"markers_on_screen_ms":self.markers,"open_elapsed_ms":self.opened.map(|t|t.elapsed().as_secs_f64()*1000.0),"bytes_received":state.bytes,"python_command":state.python,"markers_kept":state.markers,"worker_running":state.workers,"note":state.error,"diagnostic":state.diagnostic})
        );
    }
    pub fn tick(&mut self, app: &mut TakViewerApp, ctx: &egui::Context) -> Step {
        if self.finished {
            return Step::Wait;
        }
        let state = app.ssh_probe_state(&self.alias);
        if state.bytes != self.last_bytes {
            self.last_bytes = state.bytes;
            self.last_received = Instant::now();
        }
        if self.last_progress.elapsed() >= Duration::from_secs(5) {
            self.print("progress", &state);
            self.last_progress = Instant::now();
        }
        if !self.cancelled && state.busy && self.opened.is_some()
            && self.stall_after.is_some_and(|d| self.last_received.elapsed() >= d) {
            self.cancelled = true;
            self.cancel_started = Some(Instant::now());
            self.print("stall_cancel_requested", &state);
            return Step::Escape;
        }
        if self.cancelled {
            if !state.workers {
                self.print("cancelled", &state);
                self.finished = true;
                return Step::Done(0);
            }
            if self
                .cancel_started
                .is_some_and(|t| t.elapsed() > Duration::from_secs(10))
            {
                self.print("cancel_shutdown_timeout", &state);
                return Step::Done(1);
            }
            return Step::Wait;
        }
        if let Some(error) = &state.error {
            eprintln!("{error}");
            self.print("error", &state);
            return Step::Done(1);
        }
        if self.connected.is_none() && state.connected {
            self.connected = Some(self.ms());
            self.print("connected", &state);
        }
        if self.listed.is_none() {
            if let Some(listing) = &state.listing {
                self.listed = Some(self.ms());
                println!(
                    "{}",
                    json!({"stage":"listed","elapsed_ms":self.listed,"listing":listing,"bytes_received":state.bytes,"python_command":state.python})
                );
                if self.list_only {
                    self.finished = true;
                    return Step::Done(0);
                }
                app.ssh_probe_open(ctx, &self.alias, &self.path);
                self.opened = Some(Instant::now());
            }
            return Step::Wait;
        }
        if self.markers.is_none() && state.markers {
            self.markers = Some(self.ms());
            self.print("markers_on_screen", &state);
        }
        if self
            .cancel_after
            .is_some_and(|d| self.opened.is_some_and(|t| t.elapsed() >= d))
            && state.busy
        {
            self.cancelled = true;
            self.cancel_started = Some(Instant::now());
            self.print("cancel_requested", &state);
            return Step::Escape;
        }
        if state.markers && !state.pending && !state.busy {
            self.print("analog_done", &state);
            if self.cancel_after.is_some() {
                eprintln!("취소 전에 읽기가 끝났어요. 더 짧은 --ssh-cancel-after-ms 값으로 시험해 주세요.");
                return Step::Done(1);
            }
            if let Some(path) = &self.dump {
                if let Err(e) = app.ssh_probe_dump(path) {
                    eprintln!("{e}");
                    return Step::Done(1);
                }
            }
            self.finished = true;
            return Step::Capture;
        }
        Step::Wait
    }
}

pub fn run(args: &[String]) -> i32 {
    match parse(args) {
        Ok((probe, auto)) => headless::run_ssh(probe, auto),
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}
fn parse(args: &[String]) -> Result<(Probe, Option<shot::AutoShot>), String> {
    let (mut alias, mut platform, mut python, mut path) =
        (None, "linux".to_owned(), String::new(), None);
    let (mut list_only, mut cancel_after, mut dump, mut stall_after) = (false, None, None, None);
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let key = args[i].as_str();
        if key == "--headless" {
            i += 1;
            continue;
        }
        if matches!(
            key,
            "--ssh-open"
                | "--ssh-list"
                | "--ssh-platform"
                | "--ssh-python"
                | "--ssh-path"
                | "--ssh-cancel-after-ms"
                | "--ssh-dump"
                | "--ssh-stall-cancel-after-ms"
        ) {
            let value = args
                .get(i + 1)
                .ok_or_else(|| format!("{key}: 값이 필요해요."))?;
            match key {
                "--ssh-open" | "--ssh-list" => {
                    if alias.is_some() {
                        return Err("SSH 모드는 하나만 써 주세요.".into());
                    }
                    alias = Some(value.clone());
                    list_only = key == "--ssh-list";
                }
                "--ssh-platform" => platform = value.clone(),
                "--ssh-python" => python = value.clone(),
                "--ssh-path" => path = Some(value.clone()),
                "--ssh-cancel-after-ms" => {
                    cancel_after = Some(Duration::from_millis(
                        value
                            .parse::<u64>()
                            .ok()
                            .filter(|n| *n > 0 && *n <= 600000)
                            .ok_or("취소 시간은 1~600000 ms예요.")?,
                    ))
                }
                "--ssh-stall-cancel-after-ms" => {
                    stall_after = Some(Duration::from_millis(value.parse::<u64>().ok()
                        .filter(|n| *n > 0 && *n <= 600000).ok_or("시험 중 무수신 취소 시간은 1~600000 ms예요.")?));
                }
                "--ssh-dump" => dump = Some(workspace::resolve_path(value)),
                _ => unreachable!(),
            }
            i += 2;
        } else if matches!(key, "--screenshot" | "--frame" | "--select" | "--plate") {
            rest.push(args[i].clone());
            rest.push(args.get(i + 1).ok_or("그림 옵션 값이 필요해요.")?.clone());
            i += 2;
        } else {
            return Err(format!("SSH 시험에서 모르는 옵션이에요: {key}"));
        }
    }
    let alias = alias
        .filter(|s| !s.is_empty())
        .ok_or("--ssh-open 또는 --ssh-list 별칭이 필요해요.")?;
    let path = path
        .filter(|s| (list_only && s.is_empty() || !s.trim().is_empty()) && !s.contains('\0') && s.len() <= 32768)
        .ok_or("--ssh-path 경로가 필요해요.")?;
    if !matches!(platform.as_str(), "linux" | "windows") {
        return Err("플랫폼은 linux 또는 windows예요.".into());
    }
    if !python.trim().is_empty() {
        tak_core::remote::python_tokens(&python, &platform)?;
    }
    if dump
        .as_ref()
        .is_some_and(|p| tak_core::tak::is_take_path(p) || tak_core::takm::is_takm_path(p))
    {
        return Err("TAK · TAKM 경로에는 출력할 수 없어요.".into());
    }
    let (auto, leftovers) = shot::parse(&rest);
    if !leftovers.is_empty()
        || (!list_only && auto.is_none())
        || (list_only && (auto.is_some() || dump.is_some() || cancel_after.is_some() || stall_after.is_some()))
    {
        return Err(
            "--ssh-open은 --screenshot과 함께, --ssh-list는 그림·덤프·취소 없이 써 주세요.".into(),
        );
    }
    Ok((
        Probe {
            alias,
            platform,
            python,
            path,
            list_only,
            cancel_after,
            dump,
            stall_after,
            last_bytes: 0,
            last_received: Instant::now(),
            last_progress: Instant::now(),
            start: Instant::now(),
            opened: None,
            connected: None,
            listed: None,
            markers: None,
            cancelled: false,
            cancel_started: None,
            finished: false,
        },
        auto,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn list_accepts_explicit_empty_path_but_open_and_missing_paths_do_not() {
        for platform in ["windows", "linux"] {
            let (probe, _) = parse(&args(&["--ssh-list", "lab", "--ssh-platform", platform, "--ssh-path", ""])).unwrap();
            assert!(probe.folder().is_empty());
        }
        assert!(parse(&args(&["--ssh-list", "lab"])).is_err());
        for path in ["", " ", "a\0b"] {
            assert!(parse(&args(&["--ssh-open", "lab", "--ssh-path", path, "--screenshot", "out.png"])).is_err());
        }
        for path in [" ", "a\0b"] {
            assert!(parse(&args(&["--ssh-list", "lab", "--ssh-path", path])).is_err());
        }
    }
    #[test]
    fn explicit_modes_and_safe_local_outputs_are_required_before_network() {
        assert!(parse(&args(&["--ssh-open", "lab", "--ssh-path", r"E:\raw\a.tak"])).is_err());
        assert!(parse(&args(&["--ssh-list", "imac", "--ssh-path", "/"])).is_ok());
        assert!(parse(&args(&[
            "--ssh-list",
            "lab",
            "--ssh-open",
            "lab",
            "--ssh-path",
            "/"
        ]))
        .is_err());
        assert!(parse(&args(&[
            "--ssh-open",
            "lab",
            "--ssh-path",
            r"E:\raw\a.tak",
            "--screenshot",
            "out.png",
            "--ssh-dump",
            "bad.TAKM"
        ]))
        .is_err());
        let (p, shot) = parse(&args(&[
            "--headless",
            "--ssh-open",
            "lab",
            "--ssh-platform",
            "windows",
            "--ssh-python",
            "py -3",
            "--ssh-path",
            r"E:\한글 자료\a.tak",
            "--screenshot",
            "out.png",
            "--frame",
            "100",
        ]))
        .unwrap();
        assert_eq!(p.folder(), r"E:\한글 자료");
        assert_eq!(shot.unwrap().frame, Some(100));
        assert!(p.stall_after.is_none());
        let (p, _) = parse(&args(&["--ssh-open", "lab", "--ssh-path", "/a.tak", "--screenshot", "out.png", "--ssh-stall-cancel-after-ms", "300000"])).unwrap();
        assert_eq!(p.stall_after, Some(Duration::from_secs(300)));
        assert!(parse(&args(&["--ssh-list", "lab", "--ssh-path", "/", "--ssh-stall-cancel-after-ms", "300000"])).is_err());
    }
}
