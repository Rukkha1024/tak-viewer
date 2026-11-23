//! Read-only SSH Explorer state. Every SSH session and large snapshot is owned by a worker.
//! Construction is dormant: only `discover_hosts` reads config and only `connect` starts SSH.

use eframe::egui;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{
    mpsc::{self, Receiver, Sender},
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use tak_core::{
    comments::{self, Comments},
    remote::{CancelToken, RemoteConfig, RemoteSession, RemoteStage, RemoteTake},
    view_data::{AnalogPart, AnalogTarget, ViewData},
};

pub const REMOTE_OVERLAY_NOTE: &str =
    "원격 파일은 이 화면에 얹을 수 없어요. 눌러서 다른 화면에 열어 주세요.";
pub const REMOTE_DRAG_NOTE: &str =
    "원격 파일은 끌어 놓을 수 없어요. 눌러서 열거나 Ctrl+클릭으로 옆 화면에 열어 주세요.";
pub const LOST_NOTE: &str = "연결이 끊겼어요. 받은 마커는 계속 볼 수 있어요.";
pub const CHANGED_NOTE: &str = "서버의 파일이 바뀌었어요. 다시 열면 처음부터 읽어요.";

static DEMO_STATE: OnceLock<String> = OnceLock::new();
pub fn set_demo_state(state: &str) {
    let _ = DEMO_STATE.set(state.to_owned());
}
pub fn demo_state() -> Option<String> {
    DEMO_STATE.get().cloned()
}

#[derive(Clone, Debug, PartialEq)]
pub enum SshAction {
    Progress(crate::take_progress::Pick),
    AddWorkspace { location: RemoteLocation, platform: String },
    Open {
        location: RemoteLocation,
        side: bool,
    },
    Notice(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct RemoteLocation {
    pub host: String,
    pub path: String,
}

impl RemoteLocation {
    /// Opaque identity for the current run; never a path passed to a filesystem API.
    pub fn memory_key(&self) -> PathBuf {
        fn hex(s: &str) -> String {
            const DIGITS: &[u8] = b"0123456789abcdef";
            let mut out = String::with_capacity(s.len() * 2);
            for b in s.bytes() {
                out.push(DIGITS[(b >> 4) as usize] as char);
                out.push(DIGITS[(b & 15) as usize] as char);
            }
            out
        }
        let name = self
            .path
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty() && *s != "." && *s != "..")
            .unwrap_or("remote.tak");
        PathBuf::from("@ssh")
            .join(hex(&self.host))
            .join(hex(&self.path))
            .join(name)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostState {
    Off,
    Connecting,
    Connected,
    Lost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshErrorKind {
    Failed,
    Lost,
    Changed,
}

pub enum SshEvent {
    Markers {
        request_id: u64,
        location: RemoteLocation,
        data: Arc<ViewData>,
        comments: Comments,
    },
    ExactMarkers {
        request_id: u64,
        location: RemoteLocation,
        data: Arc<ViewData>,
    },
    Analog {
        request_id: u64,
        location: RemoteLocation,
        part: AnalogPart,
    },
    Error {
        request_id: Option<u64>,
        host: String,
        message: String,
        kind: SshErrorKind,
    },
    Cancelled {
        request_id: u64,
    },
}

#[derive(Clone, Debug)]
pub enum TreeRow {
    Dir {
        path: String,
        name: String,
        depth: usize,
        expanded: bool,
    },
    File {
        path: String,
        name: String,
        depth: usize,
    },
    Note {
        text: String,
        depth: usize,
    },
}

#[derive(Clone, Debug)]
struct RemoteEntry {
    path: String,
    name: String,
    directory: bool,
}

#[derive(Clone, Debug)]
struct DirectoryListing {
    path: String,
    entries: Vec<RemoteEntry>,
    truncated: bool,
    drive_source: Option<String>,
}

enum Listing {
    Loading,
    Ready(DirectoryListing),
    Failed(String),
}

pub struct Host {
    pub alias: String,
    pub state: HostState,
    pub expanded: bool,
    pub path: String,
    pub path_input: String,
    /// Flattened only when listings or folds change; painting never walks directory trees.
    pub rows: Vec<TreeRow>,
    pub note: Option<String>,
    pub received_bytes: u64,
    received_counter: Option<Arc<AtomicU64>>,
    pub diagnostic: Option<String>,
    generation: u64,
    expanded_dirs: HashSet<String>,
    listings: HashMap<String, Listing>,
    worker: Option<WorkerHandle>,
    progress: HashMap<String, Option<tak_core::remote::RemoteProgress>>,
    progress_visible: Vec<String>,
    demo_progress: HashMap<String, crate::take_progress::State>,
}

impl Host {
    fn new(alias: String) -> Self {
        Self {
            alias,
            state: HostState::Off,
            expanded: false,
            path: "~".into(),
            path_input: "~".into(),
            rows: Vec::new(),
            note: None,
            received_bytes: 0,
            received_counter: None,
            diagnostic: None,
            generation: 0,
            expanded_dirs: HashSet::new(),
            listings: HashMap::new(),
            worker: None,
            progress: HashMap::new(),
            progress_visible: Vec::new(),
            demo_progress: HashMap::new(),
        }
    }

    pub fn shows_rows(&self) -> bool {
        self.expanded && (matches!(self.state, HostState::Connected | HostState::Lost)
            || (self.state == HostState::Off && !self.rows.is_empty()))
    }

    pub fn visible_progress(&mut self, path: &str) -> Option<crate::take_progress::State> {
        if let Some(value) = self.demo_progress.get(path) { return Some(*value); }
        if let Some(value) = self.progress.get(path) {
            return value.as_ref().map(|row| crate::take_progress::State(row.value));
        }
        if self.state == HostState::Connected && self.progress_visible.len() < 8
            && !self.progress_visible.iter().any(|p| p == path) {
            self.progress_visible.push(path.into());
        }
        None
    }

    fn cache_progress(&mut self, path: String, row: Option<tak_core::remote::RemoteProgress>) {
        // Keep current visible results when the user scrolls through a long listing.
        if self.progress.len() >= 1024 && !self.progress.contains_key(&path) {
            let old = self.progress.keys().find(|p| !self.progress_visible.contains(p)).cloned();
            if let Some(old) = old { self.progress.remove(&old); }
        }
        self.progress.insert(path, row);
    }

    fn rebuild_rows(&mut self) {
        fn append(
            host: &Host,
            path: &str,
            depth: usize,
            rows: &mut Vec<TreeRow>,
            visited: &mut HashSet<String>,
        ) {
            // Malformed/cyclic server listings must never recurse indefinitely.
            if depth > 64 || !visited.insert(path.into()) {
                return;
            }
            match host.listings.get(path) {
                Some(Listing::Ready(list)) => {
                    for e in &list.entries {
                        if e.directory {
                            let expanded = host.expanded_dirs.contains(&e.path);
                            rows.push(TreeRow::Dir {
                                path: e.path.clone(),
                                name: e.name.clone(),
                                depth,
                                expanded,
                            });
                            if expanded {
                                append(host, &e.path, depth + 1, rows, visited);
                            }
                        } else {
                            rows.push(TreeRow::File {
                                path: e.path.clone(),
                                name: e.name.clone(),
                                depth,
                            });
                        }
                    }
                    if list.truncated {
                        rows.push(TreeRow::Note {
                            text: format!("Showing the first {} items.", list.entries.len()),
                            depth,
                        });
                    }
                    if list.entries.is_empty() {
                        rows.push(TreeRow::Note {
                            text: "No folders or takes".into(),
                            depth,
                        });
                    }
                }
                Some(Listing::Failed(e)) => rows.push(TreeRow::Note {
                    text: e.clone(),
                    depth,
                }),
                _ => rows.push(TreeRow::Note {
                    text: "Loading list…".into(),
                    depth,
                }),
            }
            visited.remove(path);
        }
        let mut rows = Vec::new();
        append(self, &self.path, 0, &mut rows, &mut HashSet::new());
        self.rows = rows;
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum OperationKind {
    Connect,
    List,
    Open,
    Progress,
    ProgressWrite,
}
#[derive(Clone, Copy)]
enum AnalogStage { WaitingForPaint { full: bool }, Initial, Full, OnAccess }
struct Operation {
    cancel: CancelToken,
    kind: OperationKind,
}
struct WorkerHandle {
    tx: Sender<Command>,
    operations: HashMap<u64, Operation>,
}

#[derive(Clone)]
pub struct RemoteDocument {
    pub source: tak_core::remote::RemoteSource,
    pub streams: tak_core::remote::RemoteEditStreams,
    pub fixed_len: u64,
    pub file_size: usize,
    pub next_comment_id:u64,
}
pub struct RemoteWritten {
    pub outcome: tak_core::remote::RemoteWriteOutcome,
    pub loaded: RemoteTake,
    pub view: ViewData,
    pub prepared: Option<tak_core::edit::PreparedInput>,
    pub comments: Comments,
    pub creation: Arc<tak_core::edit::label::CreationStreams>,
}
enum Command {
    ProgressRead { id: u64, paths: Vec<String>, cancel: CancelToken },
    ProgressWrite {
        id: u64, location: RemoteLocation, source: Option<tak_core::remote::RemoteSource>, use_open: bool,
        before: Option<i64>, after: Option<i64>,
        reply: Sender<Result<tak_core::remote::RemoteWriteOutcome, String>>, cancel: CancelToken,
    },
    List {
        id: u64,
        path: String,
        navigate: bool,
        cancel: CancelToken,
    },
    Open {
        id: u64,
        location: RemoteLocation,
        cancel: CancelToken,
        full: bool,
        frame: Option<i64>,
    },
    Analog {
        id: u64,
        location: RemoteLocation,
        target: AnalogTarget,
        cancel: CancelToken,
    },
    Write {
        id: u64, location: RemoteLocation, snapshot: Option<tak_core::edit::DocumentSnapshot>,
        events: Vec<comments::Event>, data: Arc<ViewData>, document:RemoteDocument, reply: Sender<Result<RemoteWritten, String>>, cancel: CancelToken,
    },
    ReleaseAnalog(u64),
    Stop,
}

enum WorkerEvent {
    ProgressRows { id: u64, rows: Vec<(String, Option<tak_core::remote::RemoteProgress>)> },
    Editable(RemoteLocation, RemoteDocument),
    Aliases(Result<Vec<String>, String>),
    Connected(String),
    Metrics(u64),
    Progress(Arc<AtomicU64>),
    Diagnostic(String),
    Listed {
        id: u64,
        requested: String,
        navigate: bool,
        result: Result<DirectoryListing, String>,
    },
    Markers {
        id: u64,
        location: RemoteLocation,
        data: Arc<ViewData>,
        comments: Comments,
    },
    ExactMarkers {
        id: u64,
        location: RemoteLocation,
        data: Arc<ViewData>,
    },
    Analog {
        id: u64,
        location: RemoteLocation,
        part: AnalogPart,
    },
    Done(u64),
    Error {
        id: Option<u64>,
        message: String,
        kind: SshErrorKind,
    },
    Stopped {
        cancelled: bool,
    },
}
struct Envelope {
    host: String,
    generation: u64,
    event: WorkerEvent,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PythonSetting {
    pub explicit: Option<String>,
    pub remembered: Option<String>,
}

pub struct SshManager {
    pub hosts: Vec<Host>,
    pub platform_overrides: HashMap<String, String>,
    pub python_settings: HashMap<String, PythonSetting>,
    workers: Arc<std::sync::atomic::AtomicUsize>,
    pub collapsed: bool,
    pub discovery_pending: bool,
    pub discovery_note: Option<String>,
    pub full_analog_probe: bool,
    pub(crate) timeline: Option<Arc<crate::ssh_timeline::Timeline>>,
    pub documents: HashMap<RemoteLocation, RemoteDocument>,
    ctx: egui::Context,
    tx: Sender<Envelope>,
    rx: Receiver<Envelope>,
    next_id: u64,
    discovery_started: bool,
    demo: bool,
    emitted: Vec<SshEvent>,
    garbage: Option<Sender<WorkerEvent>>,
}

impl SshManager {
    pub fn new(ctx: &egui::Context, platform_overrides: HashMap<String, String>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            hosts: Vec::new(),
            platform_overrides,
            python_settings: HashMap::new(),
            workers: Arc::default(),
            collapsed: false,
            discovery_pending: false,
            discovery_note: None,
            full_analog_probe: false,
            timeline: None,
            documents: HashMap::new(),
            ctx: ctx.clone(),
            tx,
            rx,
            next_id: 1,
            discovery_started: false,
            demo: false,
            emitted: Vec::new(),
            garbage: None,
        }
    }

    /// Human-window startup only. This reads SSH config but starts no process or network.
    pub fn discover_hosts(&mut self) {
        if self.discovery_started || self.demo {
            return;
        }
        self.discovery_started = true;
        self.discovery_pending = true;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let result = discover_aliases();
            let _ = tx.send(Envelope {
                host: String::new(),
                generation: 0,
                event: WorkerEvent::Aliases(result),
            });
            ctx.request_repaint();
        });
    }

    pub fn platform(&self, alias: &str) -> &str {
        match self.platform_overrides.get(alias).map(String::as_str) {
            Some("windows") => "windows",
            _ => "linux",
        }
    }

    pub fn set_platform(&mut self, alias: &str, platform: &str) {
        if matches!(platform, "linux" | "windows")
            && self
                .hosts
                .iter()
                .any(|h| h.alias == alias && matches!(h.state, HostState::Off | HostState::Lost))
        {
            self.platform_overrides
                .insert(alias.into(), platform.into());
            if let Some(setting) = self.python_settings.get_mut(alias) { setting.remembered = None; }
        }
    }

    pub fn add_host(&mut self, alias: &str) {
        if !self.hosts.iter().any(|h| h.alias == alias) { self.hosts.push(Host::new(alias.into())); }
    }
    pub fn set_python(&mut self, alias: &str, command: &str) -> Result<(), String> {
        if self.hosts.iter().any(|h| h.alias == alias && !matches!(h.state, HostState::Off | HostState::Lost)) {
            return Err("연결을 끊은 뒤 바꿀 수 있어요.".into());
        }
        let command = command.trim();
        if command.is_empty() { self.python_settings.remove(alias); }
        else {
            tak_core::remote::python_tokens(command, self.platform(alias))
                .map_err(|_| "Python 명령의 따옴표와 문자를 확인해 주세요.".to_owned())?;
            self.python_settings.insert(alias.into(), PythonSetting { explicit: Some(command.into()), remembered: None });
        }
        Ok(())
    }
    pub fn workers_running(&self) -> bool { self.workers.load(std::sync::atomic::Ordering::Acquire) != 0 }
    pub fn listing_json(&self, alias: &str) -> Option<serde_json::Value> {
        let host = self.hosts.iter().find(|h| h.alias == alias)?;
        match host.listings.get(&host.path)? {
            Listing::Ready(list) => Some(serde_json::json!({"path":list.path,"count":list.entries.len(),"truncated":list.truncated,"drive_source":list.drive_source,"entries":list.entries.iter().map(|e|serde_json::json!({"path":e.path,"name":e.name,"directory":e.directory})).collect::<Vec<_>>()})),
            _ => None,
        }
    }

    /// WORKSPACE shares this host's worker and listing cache. Only called for an explicitly
    /// expanded/refreshed remote folder; constructing/restoring saved roots never calls it.
    pub fn workspace_list(&mut self, root: &crate::workspace::RemoteRoot, location: &RemoteLocation,
        key: &std::path::Path, refresh: bool) -> Result<Option<crate::workspace::Listing>, String> {
        self.add_host(&root.alias);
        let index = self.hosts.iter().position(|h| h.alias == root.alias).unwrap();
        match self.hosts[index].state {
            HostState::Off | HostState::Lost => {
                if refresh {
                    self.set_platform(&root.alias, &root.platform);
                    self.connect_at(&root.alias, &location.path)?;
                    return Ok(None);
                }
                if !matches!(self.hosts[index].listings.get(&location.path), Some(Listing::Ready(_))) {
                    return Err(self.hosts[index].note.clone().unwrap_or_else(|| "SSH 연결이 끝났어요. ↻로 다시 연결해 주세요.".into()));
                }
            }
            HostState::Connecting => return Ok(None),
            HostState::Connected => {}
        }
        if refresh || !self.hosts[index].listings.contains_key(&location.path) {
            // The connection's initial list is already in flight; don't enqueue it twice.
            if !matches!(self.hosts[index].listings.get(&location.path), Some(Listing::Loading)) {
                self.request_list(&root.alias, location.path.clone(), false)?;
            }
            return Ok(None);
        }
        match self.hosts[index].listings.get(&location.path) {
            Some(Listing::Ready(list)) => {
                let mut dirs = Vec::new();
                let mut files = Vec::new();
                for entry in &list.entries {
                    if entry.name.contains(['/', '\\', '\0']) || matches!(entry.name.as_str(), "." | "..") { continue; }
                    let path = key.join(&entry.name);
                    if entry.directory { dirs.push(path); }
                    else { files.push(crate::workspace::WsFile { path, name: entry.name.clone(), size: 0, modified: None }); }
                }
                Ok(Some(crate::workspace::Listing::Ready { dirs, files }))
            }
            Some(Listing::Failed(message)) => Err(message.clone()),
            _ => Ok(None),
        }
    }

    fn id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    pub fn connect(&mut self, alias: &str) -> Result<(), String> {
        self.connect_at(alias, "~")
    }
    pub fn connect_at(&mut self, alias: &str, initial_path: &str) -> Result<(), String> {
        if self.demo {
            return Ok(());
        }
        let index = self
            .hosts
            .iter()
            .position(|h| h.alias == alias)
            .ok_or("SSH 별칭을 찾을 수 없어요.")?;
        if matches!(
            self.hosts[index].state,
            HostState::Connecting | HostState::Connected
        ) {
            return Ok(());
        }
        self.disconnect(alias);
        let id = self.id();
        let platform = self.platform(alias).to_owned();
        let python = self.python_settings.get(alias).cloned().unwrap_or_default();
        let initial_path = initial_path.to_owned();
        let host = &mut self.hosts[index];
        host.generation = host.generation.wrapping_add(1);
        host.state = HostState::Connecting;
        host.expanded = true;
        host.note = None;
        host.path = initial_path.clone();
        host.path_input = initial_path.clone();
        host.received_bytes = 0;
        host.received_counter = None;
        host.diagnostic = None;
        host.listings.clear();
        host.expanded_dirs.clear();
        host.rows.clear();
        let cancel = CancelToken::default();
        let (tx, rx) = mpsc::channel();
        host.worker = Some(WorkerHandle {
            tx,
            operations: HashMap::from([(
                id,
                Operation {
                    cancel: cancel.clone(),
                    kind: OperationKind::Connect,
                },
            )]),
        });
        let (alias, generation, events, ctx) = (
            alias.to_owned(),
            host.generation,
            self.tx.clone(),
            self.ctx.clone(),
        );
        let workers = self.workers.clone();
        let timeline = self.timeline.clone();
        workers.fetch_add(1, std::sync::atomic::Ordering::Release);
        std::thread::spawn(move || {
            struct Finished(Arc<std::sync::atomic::AtomicUsize>);
            impl Drop for Finished { fn drop(&mut self) { self.0.fetch_sub(1, std::sync::atomic::Ordering::Release); } }
            let _finished = Finished(workers);
            host_worker(alias, platform, python, initial_path, generation, id, cancel, rx, events, ctx, timeline)
        });
        Ok(())
    }

    pub fn disconnect(&mut self, alias: &str) {
        if let Some(host) = self.hosts.iter_mut().find(|h| h.alias == alias) {
            host.generation = host.generation.wrapping_add(1);
            if let Some(worker) = host.worker.take() {
                for (id, op) in worker.operations {
                    op.cancel.cancel();
                    if op.kind == OperationKind::Open {
                        self.emitted.push(SshEvent::Cancelled { request_id: id });
                    }
                }
                let _ = worker.tx.send(Command::Stop);
            }
            host.state = HostState::Off;
            host.note = None;
        }
    }

    pub fn open_at(&mut self, location: RemoteLocation, frame: Option<i64>) -> Result<u64, String> {
        let full = self.full_analog_probe;

        if self.demo {
            return Err("그림 확인용 SSH 목록이에요. 실제 서버에는 연결하지 않아요.".into());
        }
        let id = self.id();
        let host = self
            .hosts
            .iter_mut()
            .find(|h| h.alias == location.host && h.state == HostState::Connected)
            .ok_or("먼저 호스트의 연결 버튼을 눌러 주세요.")?;
        let worker = host
            .worker
            .as_mut()
            .ok_or("SSH 연결이 끝났어요. 다시 연결해 주세요.")?;
        let cancel = CancelToken::default();
        worker
            .tx
            .send(Command::Open {
                id,
                location,
                cancel: cancel.clone(),
                full,
                frame,
            })
            .map_err(|_| "SSH 연결이 끝났어요. 다시 연결해 주세요.")?;
        worker.operations.insert(
            id,
            Operation {
                cancel,
                kind: OperationKind::Open,
            },
        );
        Ok(id)
    }

    /// Send an on-access request to the existing worker; no network work here.
    pub fn request_analog(&mut self, location: RemoteLocation, target: AnalogTarget) -> Result<u64, String> {
        let id = self.id();
        let worker = self.hosts.iter_mut().find(|h| h.alias == location.host && h.state == HostState::Connected)
            .and_then(|h| h.worker.as_mut()).ok_or("연결이 끊겼어요. EMG를 읽으려면 다시 연결해 주세요.")?;
        let cancel = CancelToken::default();
        worker.tx.send(Command::Analog { id, location, target, cancel: cancel.clone() })
            .map_err(|_| "SSH 연결이 끝났어요. 다시 연결해 주세요.")?;
        worker.operations.insert(id, Operation { cancel, kind: OperationKind::Open });
        Ok(id)
    }

    /// Queue a write on the same SSH worker as reads; the caller only polls this receiver.
    pub fn begin_progress_frame(&mut self) {
        for host in &mut self.hosts { host.progress_visible.clear(); }
    }

    pub fn progress_visible(&self) -> bool {
        !self.demo && self.hosts.iter().any(|host| !host.progress_visible.is_empty())
    }

    pub fn finish_progress_frame(&mut self, opening: bool) {
        if self.demo || opening { return; }
        for host in &mut self.hosts {
            let Some(worker) = &mut host.worker else { continue };
            if host.state != HostState::Connected || !worker.operations.is_empty() || host.progress_visible.is_empty() { continue; }
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            let cancel = CancelToken::default();
            if worker.tx.send(Command::ProgressRead { id, paths: std::mem::take(&mut host.progress_visible), cancel: cancel.clone() }).is_ok() {
                worker.operations.insert(id, Operation { cancel, kind: OperationKind::Progress });
            }
            // At most one new batch per frame, even with many hosts.
            break;
        }
    }

    pub fn visible_progress(&mut self, location: &RemoteLocation) -> Option<crate::take_progress::State> {
        self.hosts.iter_mut().find(|h| h.alias == location.host)?.visible_progress(&location.path)
    }

    pub fn write_progress(&mut self, location: RemoteLocation, before: Option<i64>, after: Option<i64>, use_open: bool)
        -> Result<Receiver<Result<tak_core::remote::RemoteWriteOutcome, String>>, String> {
        let id = self.id();
        let host = self.hosts.iter_mut().find(|h| h.alias == location.host && h.state == HostState::Connected)
            .ok_or("SSH를 다시 연결한 뒤 진행 상태를 바꿔 주세요.")?;
        let source = self.documents.get(&location).filter(|_| use_open).map(|doc| doc.source.clone()).or_else(||
            host.progress.get(&location.path).and_then(Option::as_ref).map(|p| p.source.clone()));
        let worker = host.worker.as_mut().ok_or("SSH를 다시 연결해 주세요.")?;
        if worker.operations.values().any(|op| op.kind != OperationKind::Progress) {
            return Err("서버 작업이 끝난 뒤 진행 상태를 바꿔 주세요.".into());
        }
        let cancel = CancelToken::default();
        let (reply, rx) = mpsc::channel();
        worker.tx.send(Command::ProgressWrite { id, location, source, use_open, before, after, reply, cancel: cancel.clone() })
            .map_err(|_| "SSH 연결이 끝났어요. 다시 연결해 주세요.")?;
        worker.operations.insert(id, Operation { cancel, kind: OperationKind::ProgressWrite });
        Ok(rx)
    }

    pub fn cache_progress_write(&mut self, location: &RemoteLocation, source: tak_core::remote::RemoteSource, value: Option<i64>) {
        if let Some(host) = self.hosts.iter_mut().find(|h| h.alias == location.host) {
            host.cache_progress(location.path.clone(), Some(tak_core::remote::RemoteProgress { source, value }));
        }
    }

    pub fn write(&mut self, location: RemoteLocation, snapshot: Option<tak_core::edit::DocumentSnapshot>,
                 events: Vec<comments::Event>, data:Arc<ViewData>) -> Result<Receiver<Result<RemoteWritten, String>>, String> {
        let id = self.id();
        let document=self.documents.get(&location).cloned().ok_or("원격 편집 자료가 없어요. 파일을 다시 열어 주세요.")?;
        let worker = self.hosts.iter_mut().find(|h| h.alias == location.host && h.state == HostState::Connected)
            .and_then(|h| h.worker.as_mut()).ok_or("서버에 저장하려면 다시 연결해 주세요.")?;
        let cancel = CancelToken::default();
        let (reply, rx) = mpsc::channel();
        worker.tx.send(Command::Write { id, location, snapshot, events, data, document, reply, cancel: cancel.clone() })
            .map_err(|_| "SSH 연결이 끝났어요. 다시 연결해 주세요.")?;
        worker.operations.insert(id, Operation { cancel, kind: OperationKind::Open });
        Ok(rx)
    }

    pub fn navigate(&mut self, alias: &str, path: String) -> Result<(), String> {
        self.request_list(alias, path, true)
    }
    pub fn refresh(&mut self, alias: &str) -> Result<(), String> {
        let path = self
            .hosts
            .iter()
            .find(|h| h.alias == alias)
            .ok_or("SSH 별칭을 찾을 수 없어요.")?
            .path
            .clone();
        self.request_list(alias, path, true)
    }

    fn request_list(&mut self, alias: &str, path: String, navigate: bool) -> Result<(), String> {
        if self.demo {
            return Ok(());
        }
        if (!path.is_empty() && path.trim().is_empty()) || path.len() > 32768 || path.contains('\0') {
            return Err("원격 폴더 경로를 확인해 주세요.".into());
        }
        let id = self.id();
        let host = self
            .hosts
            .iter_mut()
            .find(|h| h.alias == alias && h.state == HostState::Connected)
            .ok_or("먼저 호스트의 연결 버튼을 눌러 주세요.")?;
        let worker = host
            .worker
            .as_mut()
            .ok_or("SSH 연결이 끝났어요. 다시 연결해 주세요.")?;
        let cancel = CancelToken::default();
        worker
            .tx
            .send(Command::List {
                id,
                path: path.clone(),
                navigate,
                cancel: cancel.clone(),
            })
            .map_err(|_| "SSH 연결이 끝났어요. 다시 연결해 주세요.")?;
        worker.operations.insert(
            id,
            Operation {
                cancel,
                kind: OperationKind::List,
            },
        );
        if navigate {
            host.path = path.clone();
            host.path_input = path.clone();
        }
        host.listings.insert(path, Listing::Loading);
        host.rebuild_rows();
        Ok(())
    }

    pub fn toggle_folder(&mut self, alias: &str, path: &str) -> Result<(), String> {
        let host = self
            .hosts
            .iter_mut()
            .find(|h| h.alias == alias)
            .ok_or("SSH 별칭을 찾을 수 없어요.")?;
        let expanded = !host.expanded_dirs.remove(path);
        if expanded {
            host.expanded_dirs.insert(path.into());
        }
        let needs_list = expanded && !host.listings.contains_key(path);
        host.rebuild_rows();
        if needs_list {
            self.request_list(alias, path.into(), false)?;
        }
        Ok(())
    }

    /// Wake the host worker after the frame containing this open's markers is complete.
    pub fn markers_painted(&self, id: u64) {
        for worker in self.hosts.iter().filter_map(|host| host.worker.as_ref()) {
            if worker.operations.contains_key(&id) {
                let _ = worker.tx.send(Command::ReleaseAnalog(id));
                break;
            }
        }
    }

    pub fn cancel_request(&mut self, id: u64) {
        for worker in self.hosts.iter_mut().filter_map(|host| host.worker.as_mut()) {
            if let Some(op) = worker.operations.remove(&id) {
                op.cancel.cancel();
                if op.kind == OperationKind::Open {
                    let _ = worker.tx.send(Command::ReleaseAnalog(id));
                    self.emitted.push(SshEvent::Cancelled { request_id: id });
                }
            }
        }
    }

    /// Esc and cancel chips. Already delivered marker snapshots stay with their views.
    pub fn cancel_all(&mut self) {
        let aliases: Vec<_> = self
            .hosts
            .iter()
            .filter(|h| h.worker.as_ref().is_some_and(|w| !w.operations.is_empty()))
            .map(|h| h.alias.clone())
            .collect();
        for alias in aliases {
            self.disconnect(&alias);
        }
    }

    pub fn is_busy(&self) -> bool {
        self.hosts
            .iter()
            .any(|h| h.worker.as_ref().is_some_and(|w| !w.operations.is_empty()))
    }

    pub fn is_connected(&self, alias: &str) -> bool {
        self.hosts.iter().any(|host| {
            host.alias == alias && host.state == HostState::Connected && host.worker.is_some()
        })
    }

    pub fn progress_note(&self) -> Option<String> {
        for host in &self.hosts {
            if host.state == HostState::Connecting {
                return Some(format!("Connecting to {}…", host.alias));
            }
            if host.worker.as_ref().is_some_and(|w| {
                w.operations
                    .values()
                    .any(|op| op.kind == OperationKind::List || op.kind == OperationKind::Connect)
            }) {
                return Some(format!("{} · Loading list… {:.1} MB", host.alias, self.received_bytes(&host.alias) as f64 / 1e6));
            }
        }
        None
    }

    fn dispose(&mut self, event: WorkerEvent) {
        let tx = self.garbage.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for value in rx {
                    drop(value);
                }
            });
            tx
        });
        let _ = tx.send(event);
    }

    pub fn received_bytes(&self, alias: &str) -> u64 {
        self.hosts.iter().find(|h| h.alias == alias).map_or(0, |h| {
            h.received_counter.as_ref().map_or(h.received_bytes, |c| c.load(Ordering::Relaxed))
        })
    }

    pub fn drain_events(&mut self) -> Vec<SshEvent> {
        for host in &mut self.hosts {
            if let Some(counter) = &host.received_counter {
                host.received_bytes = counter.load(Ordering::Relaxed);
            }
        }
        while let Ok(envelope) = self.rx.try_recv() {
            if let WorkerEvent::Aliases(result) = envelope.event {
                if !self.demo {
                    self.discovery_pending = false;
                    match result {
                        Ok(aliases) => {
                            for alias in aliases {
                                if !self.hosts.iter().any(|h| h.alias == alias) {
                                    self.hosts.push(Host::new(alias));
                                }
                            }
                        }
                        Err(message) => self.discovery_note = Some(message),
                    }
                }
                continue;
            }
            let Some(index) = self
                .hosts
                .iter()
                .position(|h| h.alias == envelope.host && h.generation == envelope.generation)
            else {
                self.dispose(envelope.event);
                continue;
            };
            let event_id = match &envelope.event {
                WorkerEvent::Markers { id, .. }
                | WorkerEvent::ExactMarkers { id, .. }
                | WorkerEvent::Analog { id, .. }
                | WorkerEvent::Done(id)
                | WorkerEvent::Listed { id, .. } => Some(*id),
                WorkerEvent::Error { id, .. } => *id,
                _ => None,
            };
            let stale = event_id.is_some_and(|id| {
                self.hosts[index]
                    .worker
                    .as_ref()
                    .and_then(|w| w.operations.get(&id))
                    .is_none_or(|op| op.cancel.is_cancelled())
            });
            if stale {
                self.dispose(envelope.event);
                continue;
            }
            let host = &mut self.hosts[index];
            match envelope.event {
                WorkerEvent::ProgressRows { id, rows } => {
                    if host.worker.as_mut().and_then(|w| w.operations.remove(&id)).is_some() {
                        for (path, row) in rows { host.cache_progress(path, row); }
                    }
                }
                WorkerEvent::Metrics(bytes) => host.received_bytes = bytes,
                WorkerEvent::Progress(counter) => host.received_counter = Some(counter),
                WorkerEvent::Diagnostic(error) => host.diagnostic = Some(error),
                WorkerEvent::Editable(location, document) => { self.documents.insert(location, document); },
                WorkerEvent::Connected(command) => {
                    self.python_settings.entry(host.alias.clone()).or_default().remembered = Some(command);
                    host.state = HostState::Connected;
                    host.note = None;
                    host.listings.insert(host.path.clone(), Listing::Loading);
                    host.rebuild_rows();
                }
                WorkerEvent::Listed {
                    id,
                    requested,
                    navigate,
                    result,
                } => {
                    if let Some(w) = &mut host.worker {
                        w.operations.remove(&id);
                    }
                    match result {
                        Ok(list) => {
                            let path = list.path.clone();
                            if navigate && host.path == requested {
                                host.path = path.clone();
                                host.path_input = path.clone();
                            }
                            for entry in &list.entries { host.progress.remove(&entry.path); }
                            host.listings.remove(&requested);
                            host.listings.insert(path, Listing::Ready(list));
                        }
                        Err(e) => {
                            host.listings.insert(requested, Listing::Failed(e));
                        }
                    }
                    host.rebuild_rows();
                }
                WorkerEvent::Markers {
                    id,
                    location,
                    data,
                    comments,
                } => self.emitted.push(SshEvent::Markers {
                    request_id: id,
                    location,
                    data,
                    comments,
                }),
                WorkerEvent::ExactMarkers { id, location, data } => self.emitted.push(SshEvent::ExactMarkers {
                    request_id: id,
                    location,
                    data,
                }),
                WorkerEvent::Analog { id, location, part } => self.emitted.push(SshEvent::Analog {
                    request_id: id,
                    location,
                    part,
                }),
                WorkerEvent::Done(id) => {
                    if let Some(w) = &mut host.worker {
                        w.operations.remove(&id);
                    }
                }
                WorkerEvent::Error { id, message, kind } => {
                    host.note = Some(message.clone());
                    if let Some(id) = id {
                        if let Some(w) = &mut host.worker {
                            w.operations.remove(&id);
                        }
                    }
                    self.emitted.push(SshEvent::Error {
                        request_id: id,
                        host: host.alias.clone(),
                        message,
                        kind,
                    });
                }
                WorkerEvent::Stopped { cancelled } => {
                    host.state = if cancelled {
                        HostState::Off
                    } else {
                        HostState::Lost
                    };
                    if !cancelled {
                        host.note = Some(LOST_NOTE.into());
                    }
                    if let Some(worker) = host.worker.take() {
                        for (id, op) in worker.operations {
                            op.cancel.cancel();
                            if op.kind == OperationKind::Open {
                                self.emitted.push(SshEvent::Cancelled { request_id: id });
                            }
                        }
                    }
                }
                WorkerEvent::Aliases(_) => unreachable!(),
            }
        }
        std::mem::take(&mut self.emitted)
    }

    #[cfg(test)]
    pub(crate) fn fake_open_list(&mut self) -> FakeOpenList {
        self.set_demo("connected");
        self.demo = false;
        self.collapsed = false;
        let (tx, rx) = mpsc::channel();
        self.hosts[0].worker = Some(WorkerHandle { tx, operations: HashMap::new() });
        FakeOpenList(rx)
    }

    #[cfg(test)]
    pub(crate) fn fake_marker_reply(&self, id: u64, location: RemoteLocation, data: ViewData) {
        self.fake_marker_reply_with_comments(id, location, data, Comments::default());
    }

    #[cfg(test)]
    pub(crate) fn fake_marker_reply_with_comments(&self, id: u64, location: RemoteLocation, data: ViewData, comments: Comments) {
        self.tx.send(Envelope {
            host: location.host.clone(),
            generation: 0,
            event: WorkerEvent::Markers { id, location, data: Arc::new(data), comments },
        }).unwrap();
    }

    #[cfg(test)]
    pub(crate) fn fake_exact_reply(&self, id: u64, location: RemoteLocation, data: ViewData) {
        self.tx.send(Envelope {
            host: location.host.clone(),
            generation: 0,
            event: WorkerEvent::ExactMarkers { id, location, data: Arc::new(data) },
        }).unwrap();
    }

    /// Screenshot-only fake state; no config, SSH, process or network activity.
    pub fn set_demo(&mut self, state: &str) {
        self.cancel_all();
        self.demo = true;
        self.discovery_started = true;
        self.discovery_pending = false;
        self.hosts = vec![
            Host::new("CHO_workstation".into()),
            Host::new("workstation_window".into()),
        ];
        self.platform_overrides
            .insert("CHO_workstation".into(), "linux".into());
        self.platform_overrides
            .insert("workstation_window".into(), "windows".into());
        if matches!(state, "top_windows" | "top_linux") {
            let windows = state == "top_windows";
            let h = &mut self.hosts[usize::from(windows)];
            h.expanded = true;
            h.state = HostState::Connected;
            h.path = if windows { "" } else { "/" }.into();
            h.path_input = h.path.clone();
            let roots: &[&str] = if windows { &["C:\\", "E:\\"] } else { &["home", "Users", "Volumes"] };
            let entries = roots.iter().map(|name| RemoteEntry {
                path: if windows { (*name).into() } else { format!("/{name}") },
                name: (*name).into(), directory: true,
            }).collect();
            h.listings.insert(h.path.clone(), Listing::Ready(DirectoryListing {
                path: h.path.clone(), entries, truncated: false, drive_source: None,
            }));
            h.rebuild_rows();
            return;
        }
        let h = &mut self.hosts[0];
        h.expanded = state != "off";
        h.state = match state {
            "connecting" => HostState::Connecting,
            "connected" | "markers_ready" | "progress" => HostState::Connected,
            "lost" => HostState::Lost,
            _ => HostState::Off,
        };
        if matches!(state, "connected" | "markers_ready" | "lost" | "progress") {
            h.path = "~/data/2_gait".into();
            h.path_input = h.path.clone();
            let entries = [
                "gait_001.tak",
                "gait_002.tak",
                "gait_001.takm",
                "static_001.tak",
            ]
            .into_iter()
            .map(|name| RemoteEntry {
                path: format!("{}/{name}", h.path),
                name: name.into(),
                directory: false,
            })
            .collect();
            h.listings.insert(
                h.path.clone(),
                Listing::Ready(DirectoryListing {
                    path: h.path.clone(),
                    entries,
                    truncated: false,
                    drive_source: None,
                }),
            );
            h.rebuild_rows();
            for (name, value) in [("gait_001.tak", Some(1)), ("gait_002.tak", Some(2)),
                    ("gait_001.takm", Some(4)), ("static_001.tak", None)] {
                h.demo_progress.insert(format!("{}/{name}", h.path), crate::take_progress::State(value));
            }
        }
        if state == "lost" {
            h.note = Some(LOST_NOTE.into());
        }
    }
}

#[cfg(test)]
pub(crate) struct FakeOpenList(mpsc::Receiver<Command>);

#[cfg(test)]
impl FakeOpenList {
    pub(crate) fn opened(&self) -> (u64, RemoteLocation, CancelToken) {
        match self.0.try_recv().expect("clicked SSH row sends Open") {
            Command::Open { id, location, cancel, .. } => (id, location, cancel),
            _ => panic!("expected SSH marker request"),
        }
    }

    pub(crate) fn analog_released(&self) -> Option<u64> {
        match self.0.try_recv() {
            Ok(Command::ReleaseAnalog(id)) => Some(id),
            Err(mpsc::TryRecvError::Empty) => None,
            _ => panic!("unexpected SSH command"),
        }
    }
}

impl Drop for SshManager {
    fn drop(&mut self) {
        for host in &mut self.hosts {
            if let Some(worker) = host.worker.take() {
                for op in worker.operations.values() {
                    op.cancel.cancel();
                }
                let _ = worker.tx.send(Command::Stop);
            }
        }
        // Undelivered Arc snapshots are destroyed away from the drawing thread too.
        let (_, rx) = mpsc::channel();
        let rx = std::mem::replace(&mut self.rx, rx);
        std::thread::spawn(move || {
            for value in rx {
                drop(value);
            }
        });
    }
}

fn parse_listing(value: serde_json::Value) -> Result<DirectoryListing, String> {
    let path = value["path"]
        .as_str()
        .filter(|s| !s.contains('\0') && s.len() <= 32768)
        .ok_or("서버의 폴더 응답을 읽을 수 없어요.")?
        .to_owned();
    let input = value["entries"]
        .as_array()
        .filter(|rows| rows.len() <= 10000)
        .ok_or("서버의 폴더 목록이 올바르지 않아요.")?;
    let mut entries = Vec::with_capacity(input.len());
    let top = path.is_empty();
    for row in input {
        let drive = top
            && row["directory"] == true
            && row["name"] == row["path"]
            && row["name"].as_str().is_some_and(is_drive_root);
        let name = row["name"]
            .as_str()
            .filter(|s| drive || (!s.is_empty() && !s.contains(['/', '\\', '\0']) && *s != "." && *s != ".."))
            .ok_or("서버의 파일 이름이 올바르지 않아요.")?;
        let path = row["path"]
            .as_str()
            .filter(|s| !s.is_empty() && !s.contains('\0'))
            .ok_or("서버의 파일 경로가 올바르지 않아요.")?;
        let directory = row["directory"]
            .as_bool()
            .ok_or("서버의 폴더 응답이 올바르지 않아요.")?;
        if top && !drive {
            return Err("서버의 드라이브 목록이 올바르지 않아요.".into());
        }
        if directory
            || name.to_ascii_lowercase().ends_with(".tak")
            || name.to_ascii_lowercase().ends_with(".takm")
        {
            entries.push(RemoteEntry {
                name: name.into(),
                path: path.into(),
                directory,
            });
        }
    }
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(DirectoryListing {
        path,
        entries,
        truncated: value["truncated"].as_bool().unwrap_or(false),
        drive_source: value["drive_source"].as_str().map(str::to_owned),
    })
}

fn send(
    events: &Sender<Envelope>,
    ctx: &egui::Context,
    host: &str,
    generation: u64,
    event: WorkerEvent,
) -> bool {
    let result = events
        .send(Envelope {
            host: host.into(),
            generation,
            event,
        })
        .is_ok();
    ctx.request_repaint();
    result
}

fn classify_error(error: &str) -> (SshErrorKind, String) {
    if error.contains("source_changed") || error.starts_with("progress_changed:") {
        (SshErrorKind::Changed, CHANGED_NOTE.into())
    } else if error.contains("SSH")
        || error.contains("EOF")
        || error.contains("timed out")
        || error.contains("Broken pipe")
        || error.contains("transport")
    {
        (SshErrorKind::Lost, LOST_NOTE.into())
    } else if error.contains("payload_too_large") {
        (
            SshErrorKind::Failed,
            "서버가 보내는 데이터가 너무 커서 읽지 못했어요.".into(),
        )
    } else if error.contains("io_error") {
        (
            SshErrorKind::Failed,
            "서버의 파일이나 폴더를 읽을 수 없어요. 경로와 읽기 권한을 확인해 주세요.".into(),
        )
    } else {
        (
            SshErrorKind::Failed,
            "원격 파일을 읽을 수 없어요. TAK · TAKM 파일인지 확인해 주세요.".into(),
        )
    }
}

fn host_worker(
    alias: String,
    platform: String,
    python: PythonSetting,
    initial_path: String,
    generation: u64,
    connect_id: u64,
    connect_cancel: CancelToken,
    commands: Receiver<Command>,
    events: Sender<Envelope>,
    ctx: egui::Context,
    timeline: Option<Arc<crate::ssh_timeline::Timeline>>,
) {
    let emit = |event| send(&events, &ctx, &alias, generation, event);
    let mut config = RemoteConfig::default();
    if let Some(command) = &python.explicit {
        if platform == "windows" { config.windows_python = command.clone(); }
        else { config.linux_python = command.clone(); }
    }
    let mut session = match RemoteSession::connect_remembered(&alias, &platform, config, python.remembered.as_deref(), &connect_cancel) {
        Ok(session) => session,
        Err(error) => {
            emit(WorkerEvent::Diagnostic(error.clone()));
            if !connect_cancel.is_cancelled() {
                emit(WorkerEvent::Error {
                    id: Some(connect_id),
                    message: if error.contains(tak_core::remote::PYTHON_NOT_FOUND) { tak_core::remote::PYTHON_NOT_FOUND.into() } else { format!("{alias}에 연결할 수 없어요. SSH 설정을 확인해 주세요.") },
                    kind: SshErrorKind::Failed,
                });
            }
            // A failed first connection returns to the off state and retains its reason.
            emit(WorkerEvent::Stopped { cancelled: true });
            return;
        }
    };
    if connect_cancel.is_cancelled() {
        emit(WorkerEvent::Stopped { cancelled: true });
        return;
    }
    if let Some(trace) = timeline {
        session.observe_reads(Arc::new(move |moment, bytes| trace.read(moment, bytes)));
    }
    if let Some(counter) = session.received_counter() { emit(WorkerEvent::Progress(counter)); }
    emit(WorkerEvent::Metrics(session.received_bytes()));
    if !emit(WorkerEvent::Connected(session.python_command().to_owned())) {
        return;
    }
    let result = session.list(&initial_path, &connect_cancel).and_then(parse_listing);
    emit(WorkerEvent::Metrics(session.received_bytes()));
    if connect_cancel.is_cancelled() {
        emit(WorkerEvent::Stopped { cancelled: true });
        return;
    }
    let lost = result
        .as_ref()
        .err()
        .is_some_and(|error| classify_error(error).0 == SshErrorKind::Lost);
    let result = result.map_err(|error| classify_error(&error).1);
    if !emit(WorkerEvent::Listed {
        id: connect_id,
        requested: initial_path,
        navigate: true,
        result,
    }) {
        return;
    }
    if lost {
        emit(WorkerEvent::Error {
            id: None,
            message: LOST_NOTE.into(),
            kind: SshErrorKind::Lost,
        });
        emit(WorkerEvent::Stopped { cancelled: false });
        return;
    }
    let mut analog: VecDeque<(u64, RemoteLocation, RemoteTake, AnalogTarget, CancelToken, AnalogStage)> =
        VecDeque::new();
    let mut deferred: HashMap<RemoteLocation, RemoteTake> = HashMap::new();
    let mut editable: HashMap<RemoteLocation, RemoteDocument> = HashMap::new();
    let mut pending_access: VecDeque<(u64, RemoteLocation, AnalogTarget, CancelToken)> = VecDeque::new();
    loop {
        pending_access.retain(|(_, _, _, cancel)| !cancel.is_cancelled());
        if let Some(index) = pending_access.iter().position(|(_, l, _, _)| !analog.iter().any(|(_, active, _, _, _, _)| active == l)) {
            let (id, location, target, cancel) = pending_access.remove(index).unwrap();
            if let Some(loaded) = deferred.remove(&location) {
                analog.push_back((id, location, loaded, target, cancel, AnalogStage::OnAccess));
            } else {
                emit(WorkerEvent::Error { id:Some(id), message:"EMG를 읽을 준비가 없어요. 파일을 다시 열어 주세요.".into(), kind:SshErrorKind::Failed });
            }
        }
        // A hidden tab may wait indefinitely. Leave its job queued while servicing this host.
        let runnable = analog.iter().position(|(_, _, _, _, cancel, stage)| {
            cancel.is_cancelled() || !matches!(stage, AnalogStage::WaitingForPaint { .. })
        });
        // Any queued marker/list request goes before analog work from an earlier open.
        let command = match commands.try_recv() {
            Ok(cmd) => Some(cmd),
            Err(mpsc::TryRecvError::Disconnected) => return,
            Err(mpsc::TryRecvError::Empty) if runnable.is_none() => {
                match commands.recv_timeout(std::time::Duration::from_millis(250)) {
                    Ok(cmd) => Some(cmd),
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if !session.is_alive() {
                            emit(WorkerEvent::Error {
                                id: None,
                                message: LOST_NOTE.into(),
                                kind: SshErrorKind::Lost,
                            });
                            emit(WorkerEvent::Stopped { cancelled: false });
                            return;
                        }
                        continue;
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => None,
        };
        match command {
            Some(Command::Stop) => return,
            Some(Command::ProgressRead { id, paths, cancel }) => {
                if cancel.is_cancelled() { emit(WorkerEvent::Done(id)); continue; }
                let result = session.progress_batch(&paths, &cancel);
                if cancel.is_cancelled() || !session.is_alive() {
                    emit(WorkerEvent::Stopped { cancelled: cancel.is_cancelled() });
                    return;
                }
                let rows = result.unwrap_or_else(|_| vec![None; paths.len()]);
                emit(WorkerEvent::ProgressRows { id, rows: paths.into_iter().zip(rows).collect() });
            }
            Some(Command::ProgressWrite { id, location, source, use_open, before, after, reply, cancel }) => {
                if cancel.is_cancelled() { emit(WorkerEvent::Done(id)); continue; }
                let source = editable.get(&location).filter(|_| use_open).map(|doc| doc.source.clone()).or(source);
                let source = source.map_or_else(|| session.stat(&location.path, &cancel), Ok);
                let result = source.as_ref().map_err(Clone::clone)
                    .and_then(|source| session.write_progress(source, before, after, &cancel));
                if let Ok(outcome) = &result {
                    if let Some(doc) = editable.get_mut(&location).filter(|doc| source.as_ref() == Ok(&doc.source)) {
                        doc.source = outcome.source.clone();
                        doc.fixed_len = outcome.fixed_len;
                        doc.file_size = outcome.output_bytes as usize;
                        emit(WorkerEvent::Editable(location.clone(), doc.clone()));
                    }
                    if let Some(loaded) = deferred.get_mut(&location).filter(|loaded| source.as_ref() == Ok(&loaded.source)) {
                        loaded.source = outcome.source.clone();
                    }
                    for (_, job_location, loaded, _, _, _) in &mut analog {
                        if *job_location == location && source.as_ref() == Ok(&loaded.source) {
                            loaded.source = outcome.source.clone();
                        }
                    }
                }
                emit(WorkerEvent::Done(id));
                let _ = reply.send(result.map_err(|error| {
                    match classify_error(&error).0 {
                        SshErrorKind::Changed => CHANGED_NOTE.to_owned(),
                        SshErrorKind::Lost => LOST_NOTE.to_owned(),
                        SshErrorKind::Failed => "서버 파일에 진행 상태를 저장할 수 없어요. 파일과 폴더의 쓰기 권한을 확인해 주세요.".into(),
                    }
                }));
                ctx.request_repaint();
            }
            Some(Command::List {
                id,
                path,
                navigate,
                cancel,
            }) => {
                if cancel.is_cancelled() {
                    emit(WorkerEvent::Stopped { cancelled: true });
                    return;
                }
                let result = session.list(&path, &cancel).and_then(parse_listing);
                emit(WorkerEvent::Metrics(session.received_bytes()));
                if cancel.is_cancelled() {
                    emit(WorkerEvent::Stopped { cancelled: true });
                    return;
                }
                let lost = result
                    .as_ref()
                    .err()
                    .is_some_and(|error| classify_error(error).0 == SshErrorKind::Lost);
                let result = result.map_err(|error| classify_error(&error).1);
                if !emit(WorkerEvent::Listed {
                    id,
                    requested: path,
                    navigate,
                    result,
                }) {
                    return;
                }
                if lost {
                    emit(WorkerEvent::Error {
                        id: None,
                        message: LOST_NOTE.into(),
                        kind: SshErrorKind::Lost,
                    });
                    emit(WorkerEvent::Stopped { cancelled: false });
                    return;
                }
            }
            Some(Command::Open {
                id,
                location,
                cancel,
                full,
                frame,
            }) => {
                if cancel.is_cancelled() {
                    emit(WorkerEvent::Done(id));
                    continue;
                }
                let mut published_preview = false;
                let opened = session.open_staged(&location.path, frame, &cancel, |stage| {
                    let (data, comments) = match stage {
                        RemoteStage::FirstPose(mut take, at) => {
                            take.path = location.memory_key();
                            (ViewData::build_ssh_first_pose(take, at), Comments::default())
                        }
                        RemoteStage::ArchiveFirstPose(mut loaded, motion) => {
                            let comments = comments::parse(&String::from_utf8_lossy(&loaded.comments)).comments();
                            loaded.take.path = location.memory_key();
                            (ViewData::build_archive_first_pose(loaded.take, motion), comments)
                        }
                        RemoteStage::Preview(mut loaded, motion) => {
                            let plate_devices = loaded.plate_devices().map(<[usize]>::to_vec);
                            let comments = comments::parse(&String::from_utf8_lossy(&loaded.comments)).comments();
                            loaded.take.path = location.memory_key();
                            published_preview = true;
                            (ViewData::build_ssh_preview(loaded.take, plate_devices, motion), comments)
                        }
                    };
                    data.prepare_marker_order();
                    if !cancel.is_cancelled() && !emit(WorkerEvent::Markers {
                        id,
                        location: location.clone(),
                        data: Arc::new(data),
                        comments,
                    }) {
                        cancel.cancel();
                    }
                });
                emit(WorkerEvent::Metrics(session.received_bytes()));
                if let Err(error) = &opened { emit(WorkerEvent::Diagnostic(error.clone())); }
                match opened {
                    Ok(mut loaded) => {
                        if cancel.is_cancelled() {
                            emit(WorkerEvent::Stopped { cancelled: true });
                            return;
                        }
                        let document = RemoteDocument { source: loaded.source.clone(), streams: loaded.edit_streams.clone(),
                            fixed_len: loaded.fixed_len, file_size: loaded.take.file_size as usize,next_comment_id:loaded.next_comment_id };
                        editable.insert(location.clone(), document.clone());
                        emit(WorkerEvent::Editable(location.clone(), document));
                        loaded.take.path = location.memory_key();
                        deferred.remove(&location);
                        let pending = if full { loaded.analog_pending() } else { loaded.initial_analog_pending() };
                        let lazy = !full && loaded.lazy_analog_pending();
                        let plate_devices = loaded.plate_devices().map(<[usize]>::to_vec);
                        let fallback = loaded.fallback_reason().map(str::to_owned);
                        // Keep the one large marker allocation in the snapshot sent to the
                        // view. The analog worker only retains the small metadata take.
                        let trajectories = std::mem::take(&mut loaded.take.trajectories);
                        let mut marker_take = loaded.take.clone();
                        marker_take.trajectories = trajectories;
                        let mut data = ViewData::build_with_plate_devices(marker_take, plate_devices);
                        data.analog_pending = pending;
                        data.emg_pending = lazy;
                        data.ssh_marker_proof = loaded.marker_proof.take();
                        if !full {
                            if let Some(reason) = fallback {
                                emit(WorkerEvent::Diagnostic(reason.clone()));
                                data.take.warnings.push(format!("장치를 확실히 나누지 못해 아날로그를 모두 읽어요: {reason}"));
                            }
                        }
                        let target = data.analog_target();
                        loaded.take.warnings.clear();
                        let event = if published_preview {
                            data.prepare_marker_order();
                            WorkerEvent::ExactMarkers { id, location: location.clone(), data: Arc::new(data) }
                        } else {
                            data.prepare_marker_order();
                            let comments = comments::parse(&String::from_utf8_lossy(&loaded.comments)).comments();
                            WorkerEvent::Markers { id, location: location.clone(), data: Arc::new(data), comments }
                        };
                        // The parsed list is already on its way to the view; do not retain
                        // the original tail allocation until the analog read finishes.
                        drop(std::mem::take(&mut loaded.comments));
                        if cancel.is_cancelled() {
                            emit(WorkerEvent::Stopped { cancelled: true });
                            return;
                        }
                        if !emit(event) {
                            return;
                        }
                        if loaded.next_comment_id==0 {
                            match session.comment_id_floor(&loaded.source,&cancel) {
                                Ok(next)=>{if let Some(document)=editable.get_mut(&location) {document.next_comment_id=next;emit(WorkerEvent::Editable(location.clone(),document.clone()));}},
                                Err(error)=>{emit(WorkerEvent::Diagnostic(format!("Comment 번호를 준비하지 못했어요: {error}")));}
                            }
                        }
                        if pending {
                            analog.push_back((id, location, loaded, target, cancel, AnalogStage::WaitingForPaint { full }));
                        } else {
                            if lazy { deferred.insert(location, loaded); }
                            emit(WorkerEvent::Done(id));
                        }
                    }
                    Err(error) => {
                        if cancel.is_cancelled() {
                            emit(WorkerEvent::Stopped { cancelled: true });
                            return;
                        }
                        let (kind, message) = classify_error(&error);
                        emit(WorkerEvent::Error {
                            id: Some(id),
                            message,
                            kind,
                        });
                        if matches!(kind, SshErrorKind::Lost | SshErrorKind::Changed) {
                            emit(WorkerEvent::Stopped { cancelled: false });
                            return;
                        }
                    }
                }
            }
            Some(Command::Write { id, location, snapshot, events, data, document, reply, cancel }) => {
                editable.entry(location.clone()).or_insert(document);
                let mut run = || -> Result<RemoteWritten, String> {
                    let mut document = editable.get(&location).cloned().ok_or("파일을 다시 열어 저장 정보를 준비해 주세요.")?;
                    let parts=snapshot.as_ref().map(|snapshot|document.streams.serialize(snapshot)).transpose()?;
                    let outcome = if let Some(snapshot) = snapshot.as_ref() {
                        session.save_patch(&document.source, snapshot, &document.streams, &events, &cancel)?
                    } else if document.source.path.to_ascii_lowercase().ends_with(".takm") {
                        session.append(&document.source, &events, &cancel)?
                    } else {
                        session.save_patch_parts(&document.source,&[],&[],&events,&cancel)?
                    };
                    // Record the verified successor before reopening: a failed display read
                    // must not force a later retry to use the pre-write source identity.
                    if !outcome.existing {
                        document.source=outcome.source.clone(); document.fixed_len=outcome.fixed_len;
                        document.file_size=outcome.output_bytes as usize;
                        if let Some((nodes,channels))=parts {document.streams.nodes=nodes.into();document.streams.channels=channels.into();}
                        editable.insert(location.clone(),document.clone());
                        emit(WorkerEvent::Editable(location.clone(),document));
                    }
                    let loaded = session.open(&outcome.path, &cancel).map_err(|error|format!("서버 저장은 끝났지만 다시 읽지 못했어요: {error}"))?;
                    let mut view = if let Some(snapshot) = snapshot.as_ref() { data.rebuild_markers(snapshot).map_err(|e| format!("{e:?}"))? } else { (*data).clone() };
                    view.take.path = RemoteLocation {host:location.host.clone(),path:outcome.path.clone()}.memory_key();
                    view.take.file_size = outcome.output_bytes;
                    let prepared = if snapshot.is_none() && !loaded.edit_streams.channels.is_empty() {
                        Some(loaded.edit_streams.prepare(&data.take.path, loaded.take.file_size as usize)?.0)
                    } else { None };
                    let comments=comments::parse(&String::from_utf8_lossy(&loaded.comments)).comments();
                    let creation=Arc::new(tak_core::edit::label::CreationStreams::from_streams(loaded.edit_streams.nodes.to_vec(),loaded.edit_streams.channels.to_vec()));
                    Ok(RemoteWritten { outcome, loaded, view, prepared, comments, creation })
                };
                let result = run();
                if let Ok(written) = &result {
                    let next = RemoteLocation { host:location.host.clone(), path:written.outcome.path.clone() };
                    let document = RemoteDocument { source:written.loaded.source.clone(), streams:written.loaded.edit_streams.clone(),
                        fixed_len:written.loaded.fixed_len, file_size:written.loaded.take.file_size as usize,next_comment_id:written.loaded.next_comment_id };
                    editable.insert(next.clone(), document.clone());
                    deferred.remove(&location);
                    if written.loaded.lazy_analog_pending() {deferred.insert(next.clone(),written.loaded.clone());}
                    // Writes wait for initial analog completion. A queued user-demand job
                    // keeps its target but receives the verified successor source.
                    for (_,job_location,loaded,_,_,_) in &mut analog {
                        if *job_location==location {*job_location=next.clone();*loaded=written.loaded.clone();}
                    }
                    emit(WorkerEvent::Editable(next, document));
                }
                let _ = reply.send(result);
                emit(WorkerEvent::Metrics(session.received_bytes()));
                emit(WorkerEvent::Done(id));
            }
            Some(Command::Analog { id, location, target, cancel }) => {
                if cancel.is_cancelled() { emit(WorkerEvent::Done(id)); continue; }
                // An immediate access can arrive while the initial plate job is queued.
                if let Some(index) = analog.iter().position(|(_, l, _, _, _, _)| *l == location) {
                    let old = analog.remove(index).unwrap();
                    analog.push_front(old);
                    pending_access.push_back((id, location, target, cancel));
                } else if let Some(loaded) = deferred.remove(&location) {
                    analog.push_back((id, location, loaded, target, cancel, AnalogStage::OnAccess));
                } else {
                    emit(WorkerEvent::Error { id:Some(id), message:"EMG를 읽을 준비가 없어요. 파일을 다시 열어 주세요.".into(), kind:SshErrorKind::Failed });
                }
            }
            Some(Command::ReleaseAnalog(request)) => {
                if let Some((_, _, _, _, _, stage)) = analog.iter_mut().find(|job| job.0 == request) {
                    if let AnalogStage::WaitingForPaint { full } = *stage {
                        *stage = if full { AnalogStage::Full } else { AnalogStage::Initial };
                    }
                }
            }
            None => {
                let (id, location, mut loaded, target, cancel, stage) =
                    analog.remove(runnable.expect("ready analog work")).expect("queued analog work");
                if cancel.is_cancelled() {
                    if matches!(stage, AnalogStage::OnAccess) {
                        deferred.insert(location, loaded);
                    }
                    emit(WorkerEvent::Done(id));
                    continue;
                }
                let indices = if matches!(stage, AnalogStage::OnAccess) { loaded.lazy_device_indices() } else { loaded.initial_device_indices() };
                let selective = loaded.plate_devices().is_some();
                let read = match stage {
                    AnalogStage::WaitingForPaint { .. } => unreachable!("paint is pending"),
                    AnalogStage::Full => session.read_analog(&mut loaded, &cancel),
                    AnalogStage::Initial => session.read_initial_analog(&mut loaded, &cancel),
                    AnalogStage::OnAccess => session.read_lazy_analog(&mut loaded, &cancel),
                };
                emit(WorkerEvent::Metrics(session.received_bytes()));
                if let Err(error) = &read { emit(WorkerEvent::Diagnostic(error.clone())); }
                match read {
                    Ok(()) => {
                        if cancel.is_cancelled() {
                            emit(WorkerEvent::Stopped { cancelled: true });
                            return;
                        }
                        let warning = if loaded.take.warnings.is_empty() {
                            None
                        } else {
                            Some(loaded.take.warnings.join("\n"))
                        };
                        let values = std::mem::take(&mut loaded.take.analog);
                        let mut part = if matches!(stage, AnalogStage::Full) || !selective { AnalogPart::prepare(values, warning, &target) }
                            else { AnalogPart::prepare_selected(values, warning, &target, indices, matches!(stage, AnalogStage::Initial)) };
                        part.ssh_plate_proof = loaded.plate_proof.take();
                        if cancel.is_cancelled() {
                            emit(WorkerEvent::Stopped { cancelled: true });
                            return;
                        }
                        if !emit(WorkerEvent::Analog { id, location:location.clone(), part }) {
                            return;
                        }
                        if loaded.lazy_analog_pending() { deferred.insert(location, loaded); }
                        emit(WorkerEvent::Done(id));
                    }
                    Err(error) => {
                        if cancel.is_cancelled() {
                            emit(WorkerEvent::Stopped { cancelled: true });
                            return;
                        }
                        let (kind, message) = classify_error(&error);
                        emit(WorkerEvent::Error {
                            id: Some(id),
                            message,
                            kind,
                        });
                        if matches!(kind, SshErrorKind::Lost | SshErrorKind::Changed) {
                            emit(WorkerEvent::Stopped { cancelled: false });
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// Concrete Host aliases only. Parsing never expands substitutions or runs a command.
fn aliases_from_config(text: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    let mut routes = HashMap::<String, SshRoute>::new();
    let mut dns_aliases = HashSet::new();
    let mut active = Vec::new();
    let mut used_fields = HashSet::new();
    let mut literal_block = false;
    let mut can_merge = true;
    for line in text.lines() {
        let mut words = Vec::new();
        let (mut word, mut quoted) = (String::new(), None);
        for c in line.trim_start_matches('\u{feff}').chars() {
            if quoted == Some(c) {
                quoted = None;
            } else if quoted.is_some() {
                word.push(c);
            } else if matches!(c, '\'' | '"') {
                quoted = Some(c);
            } else if c == '#' {
                break;
            } else if c.is_whitespace() || c == '=' && words.is_empty() {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            } else {
                word.push(c);
            }
        }
        if !word.is_empty() {
            words.push(word);
        }
        let Some((key, args)) = words.split_first() else {
            continue;
        };
        let args = if args.first().is_some_and(|value| value == "=") {
            &args[1..]
        } else {
            args
        };
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                active.clear();
                used_fields.clear();
                active.extend(
                    args.iter()
                        .filter(|alias| {
                            !alias.starts_with('-')
                                && !alias.contains(['*', '?', '!', '[', ']', '\0', '/', '\\', '='])
                                && !alias.chars().any(char::is_whitespace)
                        })
                        .cloned(),
                );
                literal_block = !active.is_empty() && active.len() == args.len();
                for alias in &active {
                    if !routes.contains_key(alias) {
                        if !dns_aliases.insert(alias.to_ascii_lowercase()) {
                            can_merge = false;
                        }
                        aliases.push(alias.clone());
                        routes.insert(alias.clone(), SshRoute::default());
                    }
                }
            }
            "include" | "match" | "canonicalizehostname" => can_merge = false,
            field @ ("hostname" | "user" | "port" | "proxycommand" | "proxyjump") => {
                if !literal_block {
                    can_merge = false;
                    continue;
                }
                // First value wins. Visit a Host block's aliases at most once per field.
                if used_fields.contains(field) {
                    continue;
                }
                used_fields.insert(field.to_owned());
                let value = args.first().cloned().unwrap_or_default();
                for alias in &active {
                    let route = routes.get_mut(alias).expect("listed alias has a route");
                    match field {
                        "hostname" => {
                            route.hostname.get_or_insert_with(|| value.clone());
                        }
                        "user" => {
                            route.user.get_or_insert_with(|| value.clone());
                        }
                        "port" => {
                            route.port.get_or_insert_with(|| value.clone());
                        }
                        _ => route.has_proxy = true,
                    }
                }
            }
            _ => {}
        }
    }
    if can_merge {
        aliases.retain(|alias| !has_matching_short_alias(alias, &routes));
    }
    aliases
}

#[derive(Default)]
struct SshRoute {
    hostname: Option<String>,
    user: Option<String>,
    port: Option<String>,
    has_proxy: bool,
}

fn has_matching_short_alias(full: &str, routes: &HashMap<String, SshRoute>) -> bool {
    let Some((short, network)) = full
        .strip_suffix(".ts.net")
        .and_then(|name| name.split_once('.'))
    else {
        return false;
    };
    if short.is_empty() || network.is_empty() || network.contains('.') {
        return false;
    }
    let Some(short_route) = routes.get(short) else {
        return false;
    };
    let full_route = &routes[full];
    let port = |route: &SshRoute| {
        route
            .port
            .as_deref()
            .unwrap_or("22")
            .parse::<u16>()
            .ok()
            .filter(|&number| number != 0)
    };
    !short_route.has_proxy
        && !full_route.has_proxy
        && short_route
            .hostname
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(full))
        && full_route
            .hostname
            .as_deref()
            .unwrap_or(full)
            .eq_ignore_ascii_case(full)
        && short_route.user == full_route.user
        && port(short_route).is_some_and(|number| Some(number) == port(full_route))
}

fn discover_aliases() -> Result<Vec<String>, String> {
    let profile_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let Some(profile) = std::env::var_os(profile_var).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    let path = PathBuf::from(profile).join(".ssh").join("config");
    let metadata = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("SSH config를 읽을 수 없어요.".into()),
    };
    if metadata.len() > 1 << 20 {
        return Err("SSH config가 너무 커서 별칭을 읽지 않았어요.".into());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "SSH config를 읽을 수 없어요.")?
        .take((1 << 20) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "SSH config를 읽을 수 없어요.")?;
    if bytes.len() > 1 << 20 {
        return Err("SSH config가 너무 커서 별칭을 읽지 않았어요.".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "SSH config를 UTF-8로 읽을 수 없어요.")?;
    Ok(aliases_from_config(text))
}

/// Remote parent path, computed lexically. It never touches the local filesystem.
pub fn parent_path(path: &str) -> String {
    if path.is_empty() || is_drive_root(path) {
        return String::new();
    }
    let path = path.trim_end_matches(['/', '\\']);
    if path.is_empty() {
        return "/".into();
    }
    if path == "~" {
        return "~".into();
    }
    if path.len() == 2 && path.as_bytes()[1] == b':' {
        return format!("{path}\\");
    }
    match path.rfind(['/', '\\']) {
        Some(0) => path[..1].into(),
        Some(2) if path.as_bytes().get(1) == Some(&b':') => path[..3].into(),
        Some(i) => path[..i].into(),
        None => ".".into(),
    }
}

pub fn is_drive_root(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_reads_the_live_counter_before_a_response_completes() {
        let mut manager = SshManager::new(&egui::Context::default(), HashMap::new());
        let counter = Arc::new(AtomicU64::new(123));
        let mut host = Host::new("box".into());
        host.received_counter = Some(counter.clone());
        manager.hosts.push(host);
        assert_eq!(manager.received_bytes("box"), 123);
        counter.store(12_400_000, Ordering::Relaxed);
        assert_eq!(manager.received_bytes("box"), 12_400_000);
        manager.drain_events();
        assert_eq!(manager.hosts[0].received_bytes, 12_400_000);
        assert_eq!(manager.received_bytes("unknown"), 0);
    }

    #[test]
    fn config_lists_only_concrete_aliases_without_evaluating_directives() {
        let text = "\u{feff}Host CHO_workstation workstation_window # comment\nHost * !private bad? [abc]\nhOsT=\"quoted-name\"\nHost CHO_workstation -oProxyCommand=bad\nHostName not-an-alias\nInclude ~/.ssh/company\nMatch exec dangerous\nHost 한글\n";
        assert_eq!(
            aliases_from_config(text),
            [
                "CHO_workstation",
                "workstation_window",
                "quoted-name",
                "한글"
            ]
        );
    }

    #[test]
    fn config_prefers_short_alias_for_the_same_direct_tailscale_login() {
        let text = "Host station.tail123.ts.net\n  User operator\n  IdentityFile key-full\n\
                    Host lab\n  HostName 192.0.2.7\n\
                    Host station\n  HostName station.tail123.ts.net\n  User operator\n  Port 22\n  IdentityFile key-short\n\
                    Host helper.tail123.ts.net\n";
        assert_eq!(aliases_from_config(text), ["lab", "station", "helper.tail123.ts.net"]);
    }

    #[test]
    fn config_keeps_aliases_with_different_or_ambiguous_routes() {
        let pair = "Host station\n  HostName station.tail123.ts.net\n  User operator\n\
                    Host station.tail123.ts.net\n";
        for full_host_options in [
            "  User observer\n",
            "  User operator\n  Port 2222\n",
            "  User operator\n  HostName another.tail123.ts.net\n",
            "  User operator\n  ProxyJump gateway\n",
            "  User operator\nInclude ~/.ssh/other\n",
            "  User operator\nHost *.ts.net\n  Port 2222\n",
        ] {
            assert_eq!(
                aliases_from_config(&format!("{pair}{full_host_options}")),
                ["station", "station.tail123.ts.net"],
                "{full_host_options}"
            );
        }
    }

    #[test]
    fn discovery_reads_the_native_user_folder() {
        const CHILD: &str = "TAK_TEST_NATIVE_SSH_DISCOVERY_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert_eq!(discover_aliases().unwrap(), ["native-host"]);
            return;
        }
        let temp = tak_core::test_support::TempDir::new("native_ssh_home");
        let native = temp.join("native");
        let other = temp.join("other");
        for (folder, alias) in [(&native, "native-host"), (&other, "other-host")] {
            std::fs::create_dir_all(folder.join(".ssh")).unwrap();
            std::fs::write(folder.join(".ssh/config"), format!("Host {alias}\n")).unwrap();
        }
        let (home, userprofile) = if cfg!(windows) { (&other, &native) } else { (&native, &other) };
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ssh::tests::discovery_reads_the_native_user_folder"])
            .env("HOME", home)
            .env("USERPROFILE", userprofile)
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn remote_identity_is_stable_distinct_and_keeps_filename() {
        let a = RemoteLocation {
            host: "box".into(),
            path: "C:\\data\\a.takm".into(),
        };
        assert_eq!(a.memory_key(), a.clone().memory_key());
        assert_eq!(a.memory_key().file_name().unwrap(), "a.takm");
        assert_ne!(
            a.memory_key(),
            RemoteLocation {
                host: "box2".into(),
                ..a.clone()
            }
            .memory_key()
        );
        assert_ne!(
            a.memory_key(),
            RemoteLocation {
                path: "/data/a.takm".into(),
                ..a
            }
            .memory_key()
        );
    }

    #[test]
    fn directory_response_is_bounded_sorted_and_rejects_invalid_rows() {
        let data = serde_json::json!({"path":"/data","entries":[{"name":"b.TAKM","path":"/data/b.TAKM","directory":false},{"name":"folder","path":"/data/folder","directory":true},{"name":"a.tak","path":"/data/a.tak","directory":false},{"name":"image.png","path":"/data/image.png","directory":false}],"truncated":true});
        let list = parse_listing(data).unwrap();
        assert_eq!(
            list.entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["folder", "a.tak", "b.TAKM"]
        );
        assert!(list.truncated);
        assert!(parse_listing(serde_json::json!({"path":"/data","entries":[{"name":"../evil.tak","path":"/data/evil.tak","directory":false}]})).is_err());
    }

    #[test]
    fn lexical_parent_preserves_remote_roots() {
        for (path, parent) in [
            ("/data/gait", "/data"),
            ("/data", "/"),
            ("/", "/"),
            ("~/data/2_gait", "~/data"),
            ("~", "~"),
            ("C:\\data", "C:\\"),
            ("C:\\", ""),
            ("E:\\", ""),
            ("E:\\a", "E:\\"),
            ("e:/", ""),
            ("e:/a", "e:/"),
            ("", ""),
        ] {
            assert_eq!(parent_path(path), parent);
        }
    }

    #[test]
    fn top_level_drive_rows_are_the_only_separator_names_allowed() {
        let list = parse_listing(serde_json::json!({"path":"","drive_source":"os.listdrives","entries":[
            {"name":"E:\\","path":"E:\\","directory":true},
            {"name":"C:\\","path":"C:\\","directory":true}]})).unwrap();
        assert_eq!(list.path, "");
        assert_eq!(list.entries[0].name, "C:\\");
        assert_eq!(list.drive_source.as_deref(), Some("os.listdrives"));
        for (root, name, path, directory) in [
            ("", "E:\\", "E:\\", false), ("", "E:\\", "C:\\", true),
            ("", "../bad", "../bad", true), ("", "folder", "folder", true),
            ("/data", "E:\\", "E:\\", true), ("", "1:\\", "1:\\", true),
        ] {
            assert!(parse_listing(serde_json::json!({"path":root,"entries":[
                {"name":name,"path":path,"directory":directory}]})).is_err(), "{root:?} {name:?} {path:?} {directory}");
        }
    }

    #[test]
    fn empty_navigation_reaches_the_worker_and_updates_the_path_box() {
        let mut manager = SshManager::new(&egui::Context::default(), HashMap::new());
        let mut host = Host::new("lab".into());
        let (tx, rx) = mpsc::channel();
        host.state = HostState::Connected;
        host.worker = Some(WorkerHandle { tx, operations: HashMap::new() });
        manager.hosts.push(host);
        manager.navigate("lab", String::new()).unwrap();
        let Command::List { id, path, navigate, .. } = rx.try_recv().unwrap() else { panic!("expected list") };
        assert!(path.is_empty() && navigate);
        assert!(manager.hosts[0].path_input.is_empty());
        manager.tx.send(Envelope { host: "lab".into(), generation: 0, event: WorkerEvent::Listed {
            id, requested: path, navigate, result: parse_listing(serde_json::json!({"path":"", "entries":[
                {"name":"E:\\","path":"E:\\","directory":true}]})),
        }}).unwrap();
        manager.drain_events();
        assert!(manager.hosts[0].path.is_empty());
        assert!(manager.hosts[0].path_input.is_empty());
        assert!(matches!(&manager.hosts[0].rows[0], TreeRow::Dir { path, .. } if path == "E:\\"));
        for bad in [" ", "a\0b"] { assert!(manager.navigate("lab", bad.into()).is_err()); }
        assert!(manager.navigate("lab", "a".repeat(32769)).is_err());
    }

    #[test]
    fn open_only_queues_markers_until_explicit_analog_access_and_dump_requests_full() {
        let mut manager = SshManager::new(&egui::Context::default(), HashMap::new());
        let mut host = Host::new("lab".into());
        let (tx, rx) = mpsc::channel();
        host.state = HostState::Connected;
        host.worker = Some(WorkerHandle { tx, operations: HashMap::new() });
        manager.hosts.push(host);
        let location = RemoteLocation { host:"lab".into(), path:"/data/a.tak".into() };
        let opened = manager.open_at(location.clone(), None).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Command::Open { id, full:false, .. } if id == opened));
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let data = crate::views::sample_data("test.tak", &["marker"]);
        let id = manager.request_analog(location.clone(), data.analog_target()).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Command::Analog { id: sent, location: sent_location, .. } if sent == id && sent_location == location));
        manager.cancel_request(id);
        assert!(matches!(rx.try_recv().unwrap(), Command::ReleaseAnalog(sent) if sent == id));
        assert!(manager.drain_events().iter().any(|event| matches!(event, SshEvent::Cancelled { request_id } if *request_id == id)));
        manager.full_analog_probe = true;
        manager.open_at(location, None).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Command::Open { full:true, .. }));
    }

    #[test]
    fn stale_cancelled_and_disconnected_events_do_not_escape() {
        let ctx = egui::Context::default();
        let mut manager = SshManager::new(&ctx, HashMap::new());
        assert!(!manager.discovery_started && manager.hosts.is_empty());
        let mut host = Host::new("box".into());
        host.generation = 4;
        let (tx, _rx) = mpsc::channel();
        let cancel = CancelToken::default();
        cancel.cancel();
        host.worker = Some(WorkerHandle {
            tx,
            operations: HashMap::from([(
                7,
                Operation {
                    cancel,
                    kind: OperationKind::Open,
                },
            )]),
        });
        manager.hosts.push(host);
        for generation in [3, 4] {
            manager
                .tx
                .send(Envelope {
                    host: "box".into(),
                    generation,
                    event: WorkerEvent::Error {
                        id: Some(7),
                        message: "old failure".into(),
                        kind: SshErrorKind::Failed,
                    },
                })
                .unwrap();
        }
        assert!(manager.drain_events().is_empty());
        manager.cancel_all();
        assert!(matches!(
            manager.drain_events().as_slice(),
            [SshEvent::Cancelled { request_id: 7 }]
        ));
        assert_eq!(manager.hosts[0].state, HostState::Off);
    }

    #[test]
    fn demo_never_discovers_or_connects_and_has_cached_rows() {
        for state in ["off", "connecting", "connected", "markers_ready", "lost"] {
            let mut manager = SshManager::new(&egui::Context::default(), HashMap::new());
            manager.set_demo(state);
            manager.discover_hosts();
            manager.connect("CHO_workstation").unwrap();
            assert_eq!(manager.hosts.len(), 2);
            assert!(manager.hosts.iter().all(|h| h.worker.is_none()));
            assert!(!manager.discovery_pending);
            assert_eq!(
                manager.hosts[0].rows.len(),
                if matches!(state, "connected" | "markers_ready" | "lost" | "progress") {
                    4
                } else {
                    0
                }
            );
        }
    }
}

#[cfg(test)]
mod python_tests {
    use super::*;
    #[test]
    fn remembered_command_is_per_host_and_empty_clears_both_modes() {
        let mut ssh = SshManager::new(&egui::Context::default(), HashMap::new());
        ssh.add_host("box"); ssh.add_host("other");
        ssh.tx.send(Envelope { host:"box".into(), generation:0, event:WorkerEvent::Connected("py -3".into()) }).unwrap();
        ssh.drain_events();
        assert_eq!(ssh.python_settings["box"].remembered.as_deref(), Some("py -3"));
        assert!(ssh.python_settings["box"].explicit.is_none());
        assert!(!ssh.python_settings.contains_key("other"));
        ssh.hosts[0].state = HostState::Off;
        ssh.set_python("box", "custom -I").unwrap();
        assert_eq!(ssh.python_settings["box"].explicit.as_deref(), Some("custom -I"));
        assert!(ssh.python_settings["box"].remembered.is_none());
        ssh.set_python("box", "   ").unwrap();
        assert!(!ssh.python_settings.contains_key("box"));
        ssh.set_platform("box", "windows");
        assert!(ssh.set_python("box", "python & whoami").is_err());
        ssh.hosts[0].state = HostState::Connected;
        assert!(ssh.set_python("box", "py -3").is_err());
    }
}
