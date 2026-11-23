//! Explicit, background updates from the checkout used to build this viewer.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;

pub(crate) const MENU_ITEM: &str = "Check for Updates…";

const SOURCE_MISSING: &str = "뷰어를 빌드한 소스 폴더를 찾을 수 없어요.";
const NOT_A_CHECKOUT: &str = "소스 폴더에 Git 작업 기록이 없어요.";
const GIT_MISSING: &str = "Git을 찾을 수 없어요. Git을 설치한 뒤 다시 확인해 주세요.";
const NO_UPSTREAM: &str = "현재 작업에서 업데이트를 받을 곳이 설정되어 있지 않아요.";
const DIRTY: &str =
    "소스 폴더에 아직 정리되지 않은 코드 변경이 있어요. 변경을 정리한 뒤 다시 확인해 주세요.";
const AHEAD: &str = "이 컴퓨터에만 있는 코드 변경이 있어요. 변경을 정리한 뒤 다시 확인해 주세요.";
const DIVERGED: &str = "이 컴퓨터와 업데이트를 받을 곳에서 코드가 서로 다르게 바뀌었어요. 변경을 정리한 뒤 다시 확인해 주세요.";
const FETCH_FAILED: &str =
    "새 버전 정보를 받지 못했어요. 인터넷 연결과 저장소 접근 권한을 확인해 주세요.";
const READ_FAILED: &str = "소스 폴더의 Git 상태를 읽을 수 없어요.";
const BAD_VERSION: &str = "업데이트의 버전 번호를 읽을 수 없어요.";
const CHANGED: &str = "확인한 뒤 소스가 바뀌었어요. 새 버전을 다시 확인해 주세요.";
const MAC_BUNDLE: &str = "Finder에서 TAK Viewer.app을 연 뒤 다시 시도해 주세요.";
const NO_INSTALL_PATH: &str = "새 뷰어를 설치할 폴더를 찾을 수 없어요.";
const UNSUPPORTED: &str = "이 운영체제에서는 앱 안에서 업데이트할 수 없어요.";
const LOG_FAILED: &str = "업데이트 기록을 저장할 수 없어요.";
const BUSY: &str = "다른 뷰어 창에서 업데이트 중이에요. 끝난 뒤 다시 확인해 주세요.";
const PULL_FAILED: &str = "새 코드를 받지 못했어요. 업데이트 기록을 확인해 주세요.";
const BUILD_FAILED: &str = "새 뷰어를 만들지 못했어요. 업데이트 기록을 확인해 주세요.";
const VERIFY_FAILED: &str =
    "설치한 뷰어의 버전을 확인하지 못했어요. 업데이트 기록을 확인해 주세요.";
const SESSION_FAILED: &str =
    "열어 둔 파일 목록을 저장하지 못했어요. 이 창은 계속 사용할 수 있어요.";
const RESTART_FAILED: &str = "새 뷰어를 열 수 없어요. 이 창은 계속 사용할 수 있어요.";
const WORKER_FAILED: &str = "업데이트 작업을 진행할 수 없어요. 다시 시도해 주세요.";

#[derive(Clone, Debug, PartialEq, Eq)]
enum RestartTarget {
    Windows(PathBuf),
    Mac(PathBuf),
}

impl RestartTarget {
    fn current() -> Result<Self, &'static str> {
        if cfg!(target_os = "macos") {
            let exe = std::env::current_exe().map_err(|_| MAC_BUNDLE)?;
            return mac_bundle(&exe).map(Self::Mac).ok_or(MAC_BUNDLE);
        }
        if cfg!(windows) {
            return windows_install_path(
                std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .as_deref(),
            )
            .map(Self::Windows)
            .ok_or(NO_INSTALL_PATH);
        }
        Err(UNSUPPORTED)
    }

    fn binary(&self) -> PathBuf {
        match self {
            Self::Windows(exe) => exe.clone(),
            Self::Mac(bundle) => bundle.join("Contents/MacOS/tak_viewer"),
        }
    }

    fn command(&self) -> Command {
        match self {
            Self::Windows(exe) => Command::new(exe),
            Self::Mac(bundle) => {
                let mut command = Command::new("/usr/bin/open");
                command.arg("-n").arg(bundle);
                command
            }
        }
    }

    fn launch(&self) -> Result<(), &'static str> {
        let mut command = self.command();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match self {
            Self::Windows(_) => command.spawn().map(|_| ()).map_err(|_| RESTART_FAILED),
            Self::Mac(_) => command
                .status()
                .ok()
                .filter(|status| status.success())
                .map(|_| ())
                .ok_or(RESTART_FAILED),
        }
    }
}

fn windows_install_path(local_app_data: Option<&Path>) -> Option<PathBuf> {
    let base = local_app_data.filter(|path| path.is_absolute())?;
    // The build launcher always writes this name, even if the running copy was
    // archived or renamed aside by an earlier build.
    Some(
        base.join("tak_viewer")
            .join("target")
            .join("release")
            .join("tak_viewer.exe"),
    )
}

fn mac_bundle(exe: &Path) -> Option<PathBuf> {
    if !exe.is_absolute() || !exe.ends_with("TAK Viewer.app/Contents/MacOS/tak_viewer") {
        return None;
    }
    Some(exe.ancestors().nth(3)?.to_owned())
}

fn workspace_version(manifest: &str) -> Option<String> {
    let mut workspace_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            workspace_package = line.split('#').next()?.trim() == "[workspace.package]";
        } else if workspace_package {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() != "version" {
                continue;
            }
            let value = value.split('#').next()?.trim();
            let version = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))?;
            return valid_version(version).then(|| version.to_owned());
        }
    }
    None
}

fn valid_version(version: &str) -> bool {
    let numbers: Vec<_> = version.split('.').collect();
    numbers.len() == 3
        && numbers.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

type GitResult = io::Result<Option<String>>;

fn quiet(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command.stdin(Stdio::null())
}

fn source_command(program: impl AsRef<std::ffi::OsStr>, source: &Path) -> Command {
    let mut command = Command::new(program);
    quiet(&mut command)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(variable);
    }
    #[cfg(windows)]
    {
        // Windows Git can see a WSL checkout with a different owner. Trust only
        // this embedded checkout, for this command and its children.
        let count = std::env::var("GIT_CONFIG_COUNT")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        command
            .env("GIT_CONFIG_COUNT", (u64::from(count) + 1).to_string())
            .env(format!("GIT_CONFIG_KEY_{count}"), "safe.directory")
            .env(format!("GIT_CONFIG_VALUE_{count}"), source);
    }
    #[cfg(not(windows))]
    command.current_dir(source);
    command
}

fn git_command(source: &Path, args: &[&str]) -> Command {
    let mut command = source_command("git", source);
    command.arg("-C").arg(source).args(args);
    command
}

fn run_git(source: &Path, args: &[&str]) -> GitResult {
    let output = git_command(source, args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
}

fn git_text(
    run: &mut impl FnMut(&[&str]) -> GitResult,
    args: &[&str],
    reason: &'static str,
) -> Result<String, &'static str> {
    match run(args) {
        Ok(Some(text)) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(GIT_MISSING),
        _ => Err(reason),
    }
}

fn clean_upstream(run: &mut impl FnMut(&[&str]) -> GitResult) -> Result<(), &'static str> {
    git_text(run, &["rev-parse", "--show-prefix"], READ_FAILED).and_then(|prefix| {
        if prefix.is_empty() {
            Ok(())
        } else {
            Err(NOT_A_CHECKOUT)
        }
    })?;
    if !git_text(
        run,
        &["status", "--porcelain", "--untracked-files=normal"],
        READ_FAILED,
    )?
    .is_empty()
    {
        return Err(DIRTY);
    }
    git_text(
        run,
        &["rev-parse", "--symbolic-full-name", "@{u}"],
        NO_UPSTREAM,
    )?;
    Ok(())
}

fn heads(run: &mut impl FnMut(&[&str]) -> GitResult) -> Result<(String, String), &'static str> {
    let refs = git_text(run, &["rev-parse", "HEAD", "@{u}"], READ_FAILED)?;
    let (head, upstream) = refs.split_once('\n').ok_or(READ_FAILED)?;
    Ok((head.trim().to_owned(), upstream.trim().to_owned()))
}

#[derive(Debug, PartialEq, Eq)]
struct Available {
    old_head: String,
    upstream: String,
    version: String,
}

fn check_git(mut run: impl FnMut(&[&str]) -> GitResult) -> Result<Option<Available>, &'static str> {
    clean_upstream(&mut run)?;
    git_text(
        &mut run,
        &["fetch", "--no-recurse-submodules"],
        FETCH_FAILED,
    )?;
    let (head, upstream) = heads(&mut run)?;
    if head == upstream {
        return Ok(None);
    }

    let range = format!("{head}...{upstream}");
    let counts = git_text(
        &mut run,
        &["rev-list", "--left-right", "--count", &range],
        READ_FAILED,
    )?;
    let (ahead, behind) = counts.split_once('\t').ok_or(READ_FAILED)?;
    let ahead: u64 = ahead.parse().map_err(|_| READ_FAILED)?;
    let behind: u64 = behind.parse().map_err(|_| READ_FAILED)?;
    match (ahead, behind) {
        (1.., 0) => return Err(AHEAD),
        (1.., 1..) => return Err(DIVERGED),
        (0, 0) => return Err(READ_FAILED),
        (0, 1..) => {}
    }

    let manifest = git_text(
        &mut run,
        &["show", &format!("{upstream}:Cargo.toml")],
        BAD_VERSION,
    )?;
    let version = workspace_version(&manifest).ok_or(BAD_VERSION)?;
    Ok(Some(Available {
        old_head: head,
        upstream,
        version,
    }))
}

struct Plan {
    source: PathBuf,
    available: Available,
    target: RestartTarget,
}

fn check() -> Result<Option<Plan>, &'static str> {
    if !cfg!(any(windows, target_os = "macos")) {
        return Err(UNSUPPORTED);
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or(SOURCE_MISSING)?;
    if !source.is_dir() || !source.join("Cargo.toml").is_file() {
        return Err(SOURCE_MISSING);
    }
    if !source.join(".git").exists() {
        return Err(NOT_A_CHECKOUT);
    }
    let target = RestartTarget::current()?;
    let Some(available) = check_git(|args| run_git(source, args))? else {
        return Ok(None);
    };
    Ok(Some(Plan {
        source: source.to_owned(),
        available,
        target,
    }))
}

struct Installed {
    version: String,
    target: RestartTarget,
    log: Option<PathBuf>,
}

struct Failure {
    reason: &'static str,
    log: Option<PathBuf>,
}

fn wait_for_saved_session(stamp: &str) -> Result<(), &'static str> {
    let data = crate::instances::data_dir_override().map_err(|_| SESSION_FAILED)?;
    let path = crate::instances::storage_dir(data.as_deref())
        .ok_or(SESSION_FAILED)?
        .join("app.ron");
    let value = format!("\"{stamp}\"");
    // eframe flushes app.ron on a worker. Read the new save marker before the
    // replacement starts so it restores the just-approved local session.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        if fs::read_to_string(&path)
            .is_ok_and(|text| text.trim_end().ends_with('}') && text.contains(&value))
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(SESSION_FAILED)
}

fn update_log() -> io::Result<(PathBuf, File)> {
    let data = crate::instances::data_dir_override().map_err(io::Error::other)?;
    let dir = crate::instances::storage_dir(data.as_deref())
        .ok_or_else(|| io::Error::other("no app data directory"))?
        .join("updates");
    if tak_core::takm::in_input(&dir) {
        return Err(io::Error::other("protected input directory"));
    }
    fs::create_dir_all(&dir)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = dir.join(format!("update-{now}-{}.log", std::process::id()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    Ok((path, file))
}

fn install_lock(log: &Path) -> io::Result<File> {
    let path = log.with_file_name("install.lock");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options.open(path)?;
    #[cfg(not(windows))]
    file.try_lock()?;
    Ok(file)
}

fn logged(command: &mut Command, log: &mut File) -> io::Result<bool> {
    let status = command
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .status();
    match status {
        Ok(status) => {
            writeln!(log, "\n{status}")?;
            Ok(status.success())
        }
        Err(error) => {
            writeln!(
                log,
                "\n{}: {error}",
                command.get_program().to_string_lossy()
            )?;
            Err(error)
        }
    }
}

#[cfg(windows)]
fn windows_build(source: &Path, exe: &Path) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = source_command("cmd.exe", source);
    // Use a local cwd even when the batch is on a WSL UNC path. cmd strips
    // the outer quote pair under /s; the inner pair quotes the entire path.
    let local = exe.ancestors().nth(4).expect("standard install path");
    command
        .current_dir(local)
        .args(["/d", "/v:off", "/s", "/c"])
        .env("TAK_VIEWER_UPDATE_BATCH", source.join("빌드.cmd"))
        .env_remove("TAK_VIEWER_BUILD_UPDATED")
        .raw_arg(r#"""%TAK_VIEWER_UPDATE_BATCH%" /nopause""#);
    command
}

fn build(
    plan: &Plan,
    mut step: impl FnMut(&'static str),
    log: &mut File,
) -> Result<String, &'static str> {
    let mut git = |args: &[&str]| run_git(&plan.source, args);
    clean_upstream(&mut git)?;
    let (head, upstream) = heads(&mut git)?;
    if upstream != plan.available.upstream || (head != plan.available.old_head && head != upstream)
    {
        return Err(CHANGED);
    }
    let success = match &plan.target {
        #[cfg(windows)]
        RestartTarget::Windows(exe) => {
            step("Pulling and building…");
            logged(&mut windows_build(&plan.source, exe), log)
        }
        RestartTarget::Mac(bundle) => {
            step("Pulling source…");
            if !logged(&mut git_command(&plan.source, &["pull", "--ff-only"]), log).unwrap_or(false)
            {
                return Err(PULL_FAILED);
            }
            step("Building the app…");
            let mut command = source_command("/bin/bash", &plan.source);
            command
                .arg(plan.source.join("scripts/01_build_macos.sh"))
                .arg(bundle);
            logged(&mut command, log)
        }
        #[cfg(not(windows))]
        RestartTarget::Windows(_) => return Err(UNSUPPORTED),
    };
    if !success.unwrap_or(false) {
        return Err(BUILD_FAILED);
    }
    let output = quiet(&mut Command::new(plan.target.binary()))
        .arg("--version")
        .output()
        .map_err(|_| VERIFY_FAILED)?;
    let version = std::str::from_utf8(&output.stdout)
        .map_err(|_| VERIFY_FAILED)?
        .trim();
    if !output.status.success() || !valid_version(version) {
        return Err(VERIFY_FAILED);
    }
    let _ = writeln!(log, "Installed version: {version}");
    Ok(version.to_owned())
}

fn install(plan: &Plan, step: impl FnMut(&'static str)) -> Result<Installed, Failure> {
    let (path, mut log) = update_log().map_err(|_| Failure {
        reason: LOG_FAILED,
        log: None,
    })?;
    let result = match install_lock(&path) {
        Ok(_lock) => {
            let _ = writeln!(
                log,
                "Source: {}\nApproved: v{} ({})",
                plan.source.display(),
                plan.available.version,
                plan.available.upstream
            );
            build(plan, step, &mut log)
        }
        Err(error) => {
            let _ = writeln!(log, "Install lock: {error}");
            let busy = error.kind() == io::ErrorKind::WouldBlock
                || cfg!(windows) && error.raw_os_error() == Some(32);
            Err(if busy { BUSY } else { LOG_FAILED })
        }
    };
    match result {
        Ok(version) => Ok(Installed {
            version,
            target: plan.target.clone(),
            log: Some(path),
        }),
        Err(reason) => {
            let _ = writeln!(log, "{reason}");
            Err(Failure {
                reason,
                log: Some(path),
            })
        }
    }
}

enum Event {
    Checked(Result<Option<Plan>, &'static str>),
    Step(&'static str),
    Built(Plan, Result<Installed, Failure>),
    Restarted(Result<(), &'static str>),
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Checking,
    Latest,
    Ready(Plan),
    Building(&'static str),
    Installed(Installed),
    Failed(Failure, Option<Plan>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Preview {
    Menu,
    Ready,
    Progress,
    Restart,
}

impl Preview {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "menu" => Ok(Self::Menu),
            "ready" => Ok(Self::Ready),
            "progress" => Ok(Self::Progress),
            "restart" => Ok(Self::Restart),
            _ => {
                Err("--shot-update에는 menu, ready, progress, restart 중 하나를 써 주세요.".into())
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Updater {
    open: bool,
    state: State,
    worker: Option<Receiver<Event>>,
    restart_requested: bool,
    restarting: bool,
    restart_error: Option<&'static str>,
}

impl Updater {
    fn worker(&mut self, ctx: &egui::Context, work: impl FnOnce(&dyn Fn(Event)) + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let task = thread::Builder::new()
            .name("viewer_update".into())
            .spawn(move || {
                work(&|event| {
                    let _ = tx.send(event);
                    ctx.request_repaint();
                });
            });
        if task.is_ok() {
            self.worker = Some(rx);
        } else if self.restarting {
            self.restarting = false;
            self.restart_error = Some(RESTART_FAILED);
        } else {
            self.state = State::Failed(
                Failure {
                    reason: WORKER_FAILED,
                    log: None,
                },
                None,
            );
        }
    }

    pub(crate) fn open(&mut self, ctx: &egui::Context) {
        self.open_with(ctx, check);
    }

    fn open_with(
        &mut self,
        ctx: &egui::Context,
        check: impl FnOnce() -> Result<Option<Plan>, &'static str> + Send + 'static,
    ) {
        self.open = true;
        if self.worker.is_none()
            && matches!(
                self.state,
                State::Idle | State::Latest | State::Failed(_, None)
            )
        {
            self.state = State::Checking;
            self.worker(ctx, move |send| send(Event::Checked(check())));
        }
    }

    fn start_build(&mut self, ctx: &egui::Context) {
        let state = std::mem::take(&mut self.state);
        let plan = match state {
            State::Ready(plan) | State::Failed(_, Some(plan)) => plan,
            other => {
                self.state = other;
                return;
            }
        };
        self.state = State::Building("Preparing update…");
        self.worker(ctx, move |send| {
            let result = install(&plan, |step| send(Event::Step(step)));
            send(Event::Built(plan, result));
        });
    }

    pub(crate) fn cancel_restart(&mut self) {
        if std::mem::take(&mut self.restart_requested) {
            self.open = true;
        }
    }

    /// Called only after the normal window exit checks have accepted every view.
    pub(crate) fn restart_if_requested(&mut self, ctx: &egui::Context) -> bool {
        if self.restarting {
            return true;
        }
        if !std::mem::take(&mut self.restart_requested) {
            return false;
        }
        self.restarting = true;
        self.open = true;
        ctx.request_repaint();
        true
    }

    pub(crate) fn restart_needs_launch(&self) -> bool {
        self.restarting && self.worker.is_none()
    }

    pub(crate) fn launch_restart(&mut self, ctx: &egui::Context, saved_session: Option<String>) {
        let State::Installed(installation) = &self.state else {
            return;
        };
        let target = installation.target.clone();
        let log = installation.log.clone();
        self.worker(ctx, move |send| {
            let result = saved_session
                .as_deref()
                .map_or(Ok(()), wait_for_saved_session)
                .and_then(|()| target.launch());
            if let (Err(reason), Some(log)) = (result, log) {
                if let Ok(mut file) = OpenOptions::new().append(true).open(log) {
                    let _ = writeln!(file, "{reason}");
                }
            }
            send(Event::Restarted(result));
        });
    }

    pub(crate) fn restarting(&self) -> bool {
        self.restarting
    }

    pub(crate) fn updating(&self, preview: Option<Preview>) -> bool {
        matches!(self.state, State::Building(_)) || preview == Some(Preview::Progress)
    }

    /// Returns true only after the worker has launched the replacement.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        loop {
            let event = match worker.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => break,
            };
            match event {
                Event::Step(step) => self.state = State::Building(step),
                Event::Checked(result) => {
                    self.state = match result {
                        Ok(Some(plan)) => State::Ready(plan),
                        Ok(None) => State::Latest,
                        Err(reason) => State::Failed(Failure { reason, log: None }, None),
                    };
                    self.worker = None;
                    return false;
                }
                Event::Built(plan, result) => {
                    self.open = true;
                    self.state = match result {
                        Ok(installation) => State::Installed(installation),
                        Err(error) => State::Failed(error, Some(plan)),
                    };
                    self.worker = None;
                    return false;
                }
                Event::Restarted(result) => {
                    self.worker = None;
                    self.restarting = false;
                    self.restart_error = result.err();
                    return result.is_ok();
                }
            }
        }
        // A disconnected sender without a terminal event must not leave a spinner running.
        self.worker = None;
        if self.restarting {
            self.restarting = false;
            self.restart_error = Some(RESTART_FAILED);
        } else {
            self.state = State::Failed(
                Failure {
                    reason: WORKER_FAILED,
                    log: None,
                },
                None,
            );
            self.open = true;
        }
        false
    }

    /// Returns true when Restart was pressed. The caller owns the dirty prompt.
    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        others_open: bool,
        can_restart: bool,
        preview: Option<Preview>,
    ) -> bool {
        let preview = preview.filter(|p| *p != Preview::Menu);
        if !self.open && preview.is_none() {
            return false;
        }
        let mut open = true;
        let mut action = None;
        let mut window = egui::Window::new("Updates")
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .fade_in(false);
        if !self.restarting {
            window = window.open(&mut open);
        }
        window.show(ctx, |ui| {
            ui.set_max_width(400.0);
            if self.restarting {
                progress(ui, "Starting the new viewer…");
            } else if let Some(preview) = preview {
                match preview {
                    Preview::Ready => ready(ui, "99.0.0", &mut action),
                    Preview::Progress => progress(ui, "Pulling and building…"),
                    Preview::Restart => installed(ui, "99.0.0", false, true, &mut action),
                    Preview::Menu => {}
                }
            } else {
                match &self.state {
                    State::Idle => {}
                    State::Checking => progress(ui, "Checking for updates…"),
                    State::Latest => {
                        ui.label(format!(
                            "You have the latest version (v{}).",
                            crate::version::VERSION
                        ));
                    }
                    State::Ready(plan) => ready(ui, &plan.available.version, &mut action),
                    State::Building(step) => progress(ui, step),
                    State::Installed(installation) => {
                        installed(
                            ui,
                            &installation.version,
                            others_open,
                            can_restart,
                            &mut action,
                        );
                        if let Some(error) = self.restart_error {
                            failure(ui, error, installation.log.as_deref());
                        }
                    }
                    State::Failed(error, retry) => {
                        failure(ui, error.reason, error.log.as_deref());
                        if retry.is_some() {
                            ui.horizontal(|ui| {
                                if ui.button("Retry").clicked() {
                                    action = Some(Action::Build);
                                }
                                if ui.button("Check Again").clicked() {
                                    action = Some(Action::Check);
                                }
                            });
                        }
                    }
                }
            }
        });
        if preview.is_some() {
            return false;
        }
        self.open = open;
        match action {
            Some(Action::Check) => {
                self.state = State::Idle;
                self.open(ctx);
            }
            Some(Action::Build) => self.start_build(ctx),
            Some(Action::Decline) => {
                self.state = State::Idle;
                self.open = false;
            }
            Some(Action::Later) => self.open = false,
            Some(Action::Restart) => {
                self.open = false;
                self.restart_error = None;
                self.restart_requested = true;
                return true;
            }
            None => {}
        }
        false
    }
}

enum Action {
    Check,
    Build,
    Decline,
    Restart,
    Later,
}

fn progress(ui: &mut egui::Ui, step: &str) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(step);
    });
}

fn ready(ui: &mut egui::Ui, version: &str, action: &mut Option<Action>) {
    ui.label(format!(
        "v{version} is ready. You have v{}. Update now?",
        crate::version::VERSION
    ));
    ui.horizontal(|ui| {
        if ui.button("Update").clicked() {
            *action = Some(Action::Build);
        }
        if ui.button("Not now").clicked() {
            *action = Some(Action::Decline);
        }
    });
}

fn installed(
    ui: &mut egui::Ui,
    version: &str,
    others_open: bool,
    can_restart: bool,
    action: &mut Option<Action>,
) {
    ui.label(format!("v{version} is installed. Restart now?"));
    if others_open {
        ui.label("Other open viewer windows will keep running their old version.");
    }
    ui.horizontal(|ui| {
        if ui
            .add_enabled(can_restart, egui::Button::new("Restart"))
            .clicked()
        {
            *action = Some(Action::Restart);
        }
        if ui.button("Later").clicked() {
            *action = Some(Action::Later);
        }
    });
}

fn failure(ui: &mut egui::Ui, reason: &str, log: Option<&Path>) {
    ui.label(reason);
    if let Some(log) = log {
        ui.label("Update log");
        ui.add(
            egui::Label::new(log.display().to_string())
                .selectable(true)
                .wrap(),
        );
    }
}

#[derive(Default)]
pub(crate) struct HelpAction {
    pub controls: bool,
    pub check: bool,
}

pub(crate) fn help_menu(
    ui: &mut egui::Ui,
    interactive: bool,
    preview: Option<Preview>,
) -> HelpAction {
    let mut action = HelpAction::default();
    let show_menu = preview == Some(Preview::Menu);
    let menu = ui.menu_button("Help", |ui| {
        if ui.button("Controls").clicked() {
            ui.close_menu();
            action.controls = true;
        }
        if (interactive || show_menu) && ui.button(MENU_ITEM).clicked() && interactive {
            ui.close_menu();
            action.check = true;
        }
        ui.separator();
        ui.label(egui::RichText::new(format!("TAK Viewer v{}", crate::version::VERSION)).weak());
    });
    // Headless screenshot preview uses the real menu, with update actions gated off.
    if show_menu {
        let mut bar = egui::menu::BarState::load(ui.ctx(), ui.id());
        if bar.as_ref().is_none_or(|root| root.id != menu.response.id) {
            let position =
                menu.response.rect.left_bottom() + egui::vec2(0.0, ui.spacing().menu_spacing);
            **bar = Some(egui::menu::MenuRoot::new(position, menu.response.id));
            bar.store(ui.ctx(), ui.id());
            ui.ctx().request_repaint();
        }
    }
    action
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
