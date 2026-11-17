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
    pub comment:Option<String>,
    pub label:bool,
    pub save:bool,
    pub markers_only:bool,
    markers_only_phase:u8,
    markers_only_stop:Option<Instant>,
    edit_phase:u8,
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

#[derive(Default)]
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

#[derive(Debug,PartialEq,Eq)]
enum MarkersOnlyAction { Wait, Escape, Disconnect, Reconnect, Ready }

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
        if self.comment.is_some() || self.label || self.save {app.ssh_probe_enable_writes();}
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
    /// Agent-only sequence: ordinary marker open -> normal Escape cancellation ->
    /// worker shutdown -> explicit reconnect for writes. The retained Take stays in memory.
    fn markers_only_action(&mut self,state:&State,editable_ready:bool)->Result<MarkersOnlyAction,String>{
        match self.markers_only_phase {
            0 if state.markers && editable_ready => {
                self.markers_only_phase=1;
                self.markers_only_stop=Some(Instant::now());
                Ok(if state.busy {MarkersOnlyAction::Escape} else {MarkersOnlyAction::Disconnect})
            }
            0=>Ok(MarkersOnlyAction::Wait),
            1 if !state.workers=>{
                self.markers_only_phase=if self.comment.is_some()||self.label||self.save {2} else {3};
                Ok(if self.markers_only_phase==2 {MarkersOnlyAction::Reconnect} else {MarkersOnlyAction::Ready})
            }
            1 if self.markers_only_stop.is_some_and(|since|since.elapsed()>Duration::from_secs(15))=>Err("마커만 읽는 시험에서 SSH 취소가 15초 안에 끝나지 않았어요.".into()),
            1=>Ok(MarkersOnlyAction::Wait),
            2 if state.connected && !state.busy=>{self.markers_only_phase=3;Ok(MarkersOnlyAction::Ready)},
            2=>Ok(MarkersOnlyAction::Wait),
            _=>Ok(MarkersOnlyAction::Ready),
        }
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
        if self.markers.is_none() && state.markers {
            self.markers=Some(self.ms());self.print("markers_on_screen",&state);
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
        if self.markers_only && (self.markers_only_phase>0 || state.markers) {
            let editable=app.ssh_probe_edit_metadata_ready();
            match self.markers_only_action(&state,editable) {
                Ok(MarkersOnlyAction::Escape)=>{self.print("markers_only_cancel_requested",&state);return Step::Escape;}
                Ok(MarkersOnlyAction::Disconnect)=>{app.ssh_probe_disconnect(&self.alias);self.print("markers_only_disconnect",&state);return Step::Wait;}
                Ok(MarkersOnlyAction::Reconnect)=>{
                    self.print("markers_only_stopped",&state);
                    if let Err(error)=app.ssh_probe_reconnect(&self.alias,&self.folder()){eprintln!("{error}");return Step::Done(1);}
                    return Step::Wait;
                }
                Ok(MarkersOnlyAction::Wait)=>{
                    if let Some(error)=&state.error {eprintln!("{error}");self.print("markers_only_error",&state);return Step::Done(1);}
                    return Step::Wait;
                }
                Ok(MarkersOnlyAction::Ready)=>{},
                Err(error)=>{eprintln!("{error}");return Step::Done(1);}
            }
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
        if state.markers && !state.pending && !state.busy && !app.ssh_probe_edit_busy() {
            if self.edit_phase==0 {
                self.edit_phase=1;
                if let Some(text)=&self.comment {if let Err(e)=app.ssh_probe_add_comment(ctx,text){eprintln!("{e}");return Step::Done(1);}}
                if self.label {if let Err(e)=app.ssh_probe_unlabel(ctx){eprintln!("{e}");return Step::Done(1);}return Step::Wait;}
            }
            if self.edit_phase==1 {
                self.edit_phase=2;
                if self.label && app.ssh_probe_edit_result()["dirty"]!=true {eprintln!("라벨 편집이 적용되지 않았어요.");return Step::Done(1);}
                if self.save {app.ssh_probe_save(ctx);return Step::Wait;}
            }
            if self.edit_phase==2 {
                if self.save && (app.ssh_probe_edit_result()["failed"].is_string() || app.ssh_probe_edit_result()["dirty"]==true || app.ssh_probe_edit_result()["pending_comments"].as_u64().unwrap_or(0)>0) {
                    eprintln!("{}",app.ssh_probe_edit_result());return Step::Done(1);
                }
                println!("{}",json!({"stage":"remote_edit_result","result":app.ssh_probe_edit_result()}));
                self.edit_phase=3;
            }
            self.print(if self.markers_only {"markers_only_done"} else {"analog_done"}, &state);
            if self.cancel_after.is_some() {
                eprintln!("취소 전에 읽기가 끝났어요. 더 짧은 --ssh-cancel-after-ms 값으로 시험해 주세요.");
                return Step::Done(1);
            }
            if let Some(path) = &self.dump {
                if let Err(e) = app.ssh_probe_dump(path,self.markers_only) {
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
    let (mut comment,mut label,mut save)=(None,false,false);
    let mut markers_only=false;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let key = args[i].as_str();
        if key=="--ssh-save" {save=true;i+=1;continue;}
        if key=="--ssh-markers-only" {markers_only=true;i+=1;continue;}
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
                | "--ssh-comment" | "--ssh-label"
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
                "--ssh-comment"=>comment=Some(value.clone()),
                "--ssh-label"=>{if value!="unlabel"{return Err("--ssh-label 값은 unlabel이에요.".into());} label=true;},
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
    if markers_only && (list_only || cancel_after.is_some()) {return Err("--ssh-markers-only는 --ssh-open과 함께 쓰고 --ssh-cancel-after-ms와는 나눠 써 주세요.".into());}
    if list_only && (comment.is_some() || label || save) {return Err("목록 시험은 서버에 쓰지 않아요.".into());}
    if (comment.is_some() || label) && !save {return Err("편집 시험은 --ssh-save와 함께 써 주세요.".into());}
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
            comment,label,save,markers_only,markers_only_phase:0,markers_only_stop:None,edit_phase:0,
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
    fn markers_only_requires_open_and_separates_normal_cancel_mode(){
        let (probe,_)=parse(&args(&["--ssh-open","lab","--ssh-path","/a.tak","--screenshot","out.png","--ssh-markers-only","--ssh-comment","test","--ssh-label","unlabel","--ssh-save"])).unwrap();
        assert!(probe.markers_only && probe.save && probe.label);
        assert!(parse(&args(&["--ssh-list","lab","--ssh-path","/","--ssh-markers-only"])).is_err());
        assert!(parse(&args(&["--ssh-open","lab","--ssh-path","/a.tak","--screenshot","out.png","--ssh-markers-only","--ssh-cancel-after-ms","100"])).is_err());
        let (normal,_)=parse(&args(&["--ssh-open","lab","--ssh-path","/a.tak","--screenshot","out.png","--ssh-cancel-after-ms","100"])).unwrap();
        assert!(!normal.markers_only && normal.cancel_after.is_some());
    }
    #[test]
    fn markers_only_flow_waits_for_metadata_cancels_and_reconnects_before_save(){
        let (mut probe,_)=parse(&args(&["--ssh-open","lab","--ssh-path","/a.tak","--screenshot","out.png","--ssh-markers-only","--ssh-save"])).unwrap();
        let mut state=State{markers:true,pending:true,busy:true,workers:true,connected:true,..Default::default()};
        assert_eq!(probe.markers_only_action(&state,false).unwrap(),MarkersOnlyAction::Wait);
        assert_eq!(probe.markers_only_action(&state,true).unwrap(),MarkersOnlyAction::Escape);
        assert_eq!(probe.markers_only_action(&state,true).unwrap(),MarkersOnlyAction::Wait);
        state.workers=false;state.connected=false;state.busy=false;state.pending=false;
        assert_eq!(probe.markers_only_action(&state,true).unwrap(),MarkersOnlyAction::Reconnect);
        state.workers=true;state.busy=true;state.connected=true;
        assert_eq!(probe.markers_only_action(&state,true).unwrap(),MarkersOnlyAction::Wait);
        state.busy=false;
        assert_eq!(probe.markers_only_action(&state,true).unwrap(),MarkersOnlyAction::Ready);
    }
    #[test]
    fn markers_only_reopen_capture_stops_without_an_unneeded_connection(){
        let (mut probe,_)=parse(&args(&["--ssh-open","lab","--ssh-path","/a.takm","--screenshot","out.png","--ssh-markers-only"])).unwrap();
        let mut state=State{markers:true,busy:true,workers:true,connected:true,..Default::default()};
        assert_eq!(probe.markers_only_action(&state,true).unwrap(),MarkersOnlyAction::Escape);
        state.workers=false;state.connected=false;state.busy=false;
        assert_eq!(probe.markers_only_action(&state,true).unwrap(),MarkersOnlyAction::Ready);
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

/// Opt-in saved-root probe, including a real in-memory app.ron round trip before any connect.
pub fn run_workspace(args: &[String]) -> i32 {
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let Some(alias) = value("--ssh-workspace").filter(|s| !s.trim().is_empty()) else { eprintln!("--ssh-workspace 별칭이 필요해요."); return 2; };
    let Some(path) = value("--ssh-path").filter(|s| !s.trim().is_empty() && !s.contains('\0')) else { eprintln!("--ssh-path 폴더가 필요해요."); return 2; };
    let platform = value("--ssh-platform").unwrap_or_else(|| "linux".into());
    if !matches!(platform.as_str(), "linux" | "windows") { eprintln!("플랫폼은 linux 또는 windows예요."); return 2; }
    let python = value("--ssh-python").unwrap_or_default();
    let Some(output) = value("--screenshot") else { eprintln!("--screenshot 그림 경로가 필요해요."); return 2; };
    let output = workspace::resolve_path(&output);
    if tak_core::tak::is_take_path(&output) || tak_core::takm::is_takm_path(&output) { eprintln!("TAK · TAKM에는 그림을 쓰지 않아요."); return 2; }
    let expand = args.iter().any(|a| a == "--ssh-workspace-expand");
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    let key = match app.ssh_workspace_probe(&ctx, &alias, &path, &platform, &python, expand) {
        Ok(key) => key, Err(message) => { eprintln!("{message}"); return 1; }
    };
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0));
    let start = Instant::now();
    let mut textures = crate::soft_render::Textures::default();
    let mut ready_frames = 0;
    let mut code = 0;
    loop {
        let raw = egui::RawInput { screen_rect: Some(screen), time: Some(start.elapsed().as_secs_f64()), focused: true, max_texture_side: Some(8192), ..Default::default() };
        let out = ctx.run(raw, |ctx| app.frame(ctx));
        textures.set(&out.textures_delta);
        let ready = if !expand { true } else { match app.ssh_workspace_probe_ready(&key) {
            Ok(ready) => ready,
            Err(message) => { eprintln!("{message}"); code = 1; true }
        }};
        if ready { ready_frames += 1; }
        if ready_frames >= 3 {
            let ppp = out.pixels_per_point;
            let size = [(1600.0 * ppp).round() as usize, (960.0 * ppp).round() as usize];
            let primitives = ctx.tessellate(out.shapes, ppp);
            let image = crate::soft_render::paint(&primitives, &textures, size, ppp, egui::Color32::from_rgb(8, 8, 8));
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            if let Err(message) = shot::save_png(&output, size[0], size[1], &rgba) { eprintln!("{message}"); code = 1; }
            break;
        }
        textures.free(&out.textures_delta);
        std::thread::sleep(Duration::from_millis(5));
    }
    app.ssh_probe_disconnect(&alias);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.ssh_probe_workers_running() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(10)); }
    if app.ssh_probe_workers_running() { return 1; }
    code
}
