//! A held PID lock identifies a live window. A worker replaces its JSON snapshot
//! atomically, so listing never needs to unlock, delete or alter a registry entry.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowTab {
    pub name: String,
    pub views: Vec<WindowView>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowView {
    pub slot: usize,
    pub file: String,
    pub overlay: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: String,
    only: bool,
    tabs: Vec<WindowTab>,
}

#[derive(Serialize)]
struct Window {
    pid: u32,
    session_id: Option<u32>,
    interactive_desktop: Option<bool>,
    version: Option<String>,
    only: Option<bool>,
    tabs: Option<Vec<WindowTab>>,
}

/// One window's entry. Hold the lock until the snapshot worker has stopped.
pub struct Registration {
    path: PathBuf,
    file: Option<File>,
    updates: Option<Sender<Vec<WindowTab>>>,
    worker: Option<JoinHandle<()>>,
}

/// Optional process-scoped root for isolated window and registry runs.
pub fn data_dir_override() -> Result<Option<PathBuf>, String> {
    let Some(dir) = std::env::var_os("TAK_VIEWER_DATA_DIR").map(PathBuf::from) else {
        return Ok(None);
    };
    let invalid = || {
        format!(
            "TAK_VIEWER_DATA_DIR에 쓸 수 있는 폴더의 전체 경로를 적어 주세요. {}",
            dir.display()
        )
    };
    if !dir.is_absolute() || !dir.is_dir() {
        return Err(invalid());
    }
    let dir = fs::canonicalize(&dir).map_err(|_| invalid())?;
    let settings = dir.join("app.ron");
    let registry = dir.join("windows");
    if tak_core::takm::in_input(&settings)
        || tak_core::takm::in_input(&registry)
        || fs::symlink_metadata(&settings).is_ok_and(|m| m.file_type().is_symlink())
        || fs::symlink_metadata(&registry).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(format!(
            "여기에 뷰어 설정을 둘 수 없어요. {}",
            dir.display()
        ));
    }
    Ok(Some(dir))
}

pub fn registry_dir(data_dir: Option<&Path>) -> Option<PathBuf> {
    data_dir
        .map(Path::to_path_buf)
        .or_else(|| eframe::storage_dir("tak_viewer"))
        .map(|d| d.join("windows"))
}

pub fn register(dir: &Path, only: bool) -> io::Result<Registration> {
    let mut registration = lock_entry(dir, std::process::id())?;
    let snapshot_path = registration.path.with_extension("json");
    let initial = Snapshot {
        version: crate::version::VERSION.to_owned(),
        only,
        tabs: Vec::new(),
    };
    if let Err(err) = write_snapshot(&snapshot_path, &initial) {
        remove_snapshot(&registration.path);
        return if only { Err(err) } else { Ok(registration) };
    }
    let (tx, rx) = mpsc::channel::<Vec<WindowTab>>();
    let worker = thread::Builder::new()
        .name("viewer_registry".into())
        .spawn(move || {
            let mut published = initial;
            while let Ok(tabs) = rx.recv() {
                let tabs = rx.try_iter().last().unwrap_or(tabs);
                if published.tabs != tabs {
                    let previous = std::mem::replace(&mut published.tabs, tabs);
                    if write_snapshot(&snapshot_path, &published).is_err() {
                        published.tabs = previous;
                    }
                }
            }
        });
    let worker = match worker {
        Ok(worker) => worker,
        Err(err) => {
            remove_snapshot(&registration.path);
            return if only { Err(err) } else { Ok(registration) };
        }
    };
    registration.updates = Some(tx);
    registration.worker = Some(worker);
    Ok(registration)
}

fn write_snapshot(path: &Path, snapshot: &Snapshot) -> io::Result<()> {
    let bytes = serde_json::to_vec(snapshot)?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

fn lock_entry(dir: &Path, pid: u32) -> io::Result<Registration> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{pid}.lock"));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options.open(&path)?;
    #[cfg(not(windows))]
    file.try_lock()?;
    Ok(Registration {
        path,
        file: Some(file),
        updates: None,
        worker: None,
    })
}

fn locked(path: &Path) -> bool {
    let file = File::open(path);
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;
        matches!(file, Err(err) if err.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32))
    }
    #[cfg(not(windows))]
    {
        file.is_ok_and(|file| matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock)))
    }
}

fn entry_pid(path: &Path) -> Option<u32> {
    if path.extension()? != "lock" {
        return None;
    }
    path.file_stem()?
        .to_str()?
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid != 0)
}

impl Registration {
    pub fn publish(&self, tabs: Vec<WindowTab>) {
        if let Some(tx) = &self.updates {
            let _ = tx.send(tabs);
        }
    }

    /// Existing window-start and autosave check. Clean entries left by closed processes.
    pub fn others_alive(&self) -> bool {
        let Some(dir) = self.path.parent() else {
            return false;
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        let mut alive = false;
        for entry in entries.flatten() {
            let path = entry.path();
            if path == self.path || entry_pid(&path).is_none() {
                continue;
            }
            #[cfg(not(windows))]
            if locked(&path) {
                alive = true;
                continue;
            }
            if fs::remove_file(&path).is_ok() {
                remove_snapshot(&path);
            } else if path.exists() {
                alive = true;
            }
        }
        alive
    }
}

fn remove_snapshot(lock: &Path) {
    let _ = fs::remove_file(lock.with_extension("json"));
    let _ = fs::remove_file(lock.with_extension("tmp"));
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.updates.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        remove_snapshot(&self.path);
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

pub fn write_list(dir: Option<&Path>, mut out: impl Write, envelope: bool) -> io::Result<()> {
    let mut windows = Vec::new();
    if let Some(dir) = dir {
        match fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry?.path();
                    let Some(pid) = entry_pid(&path).filter(|_| locked(&path)) else {
                        continue;
                    };
                    let snapshot: Option<Snapshot> = fs::read(path.with_extension("json"))
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                    let session_id = session_id(pid);
                    let interactive_desktop = session_id.and_then(interactive_desktop);
                    if !locked(&path) {
                        continue;
                    }
                    let (version, only, tabs) = snapshot.map_or((None, None, None), |s| {
                        (Some(s.version), Some(s.only), Some(s.tabs))
                    });
                    windows.push(Window {
                        pid,
                        session_id,
                        interactive_desktop,
                        version,
                        only,
                        tabs,
                    });
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    windows.sort_unstable_by_key(|window| window.pid);
    if envelope {
        serde_json::to_writer(&mut out, &tak_core::cli_json::Envelope::new(1, windows))?;
    } else {
        serde_json::to_writer(&mut out, &windows)?;
    }
    writeln!(out)
}

#[cfg(windows)]
fn session_id(pid: u32) -> Option<u32> {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    let mut id = 0;
    // SAFETY: the out pointer is valid for the call.
    (unsafe { ProcessIdToSessionId(pid, &mut id) } != 0).then_some(id)
}

#[cfg(not(windows))]
fn session_id(_pid: u32) -> Option<u32> {
    None
}

#[cfg(windows)]
fn interactive_desktop(session: u32) -> Option<bool> {
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSActive, WTSConnectState, WTSFreeMemory, WTSQuerySessionInformationW,
    };
    if session == 0 {
        return Some(false);
    }
    let mut buffer = std::ptr::null_mut();
    let mut bytes = 0;
    // SAFETY: both out pointers live for the call. On success Windows owns the
    // returned allocation until WTSFreeMemory. WTSConnectState returns a 32-bit enum.
    unsafe {
        if WTSQuerySessionInformationW(0, session, WTSConnectState, &mut buffer, &mut bytes) == 0 {
            return None;
        }
        let state = if !buffer.is_null() && bytes >= 4 {
            Some(buffer.cast::<i32>().read_unaligned() == WTSActive)
        } else {
            None
        };
        WTSFreeMemory(buffer.cast());
        state
    }
}

#[cfg(not(windows))]
fn interactive_desktop(_session: u32) -> Option<bool> {
    None
}

/// True in Windows session 0, where no window can be shown.
pub fn no_desktop() -> bool {
    session_id(std::process::id()) == Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn temp_dir(tag: &str) -> tak_core::test_support::TempDir {
        tak_core::test_support::TempDir::new(&format!("tak_viewer_instances_{tag}"))
    }

    #[test]
    fn a_crashed_window_entry_is_cleaned_and_not_counted() {
        let dir = temp_dir("stale");
        let me = lock_entry(&dir, 1).unwrap();
        fs::write(dir.join("2.lock"), b"").unwrap();
        fs::write(dir.join("2.json"), b"{}").unwrap();
        fs::write(dir.join("notes.txt"), b"x").unwrap();
        assert!(!me.others_alive());
        assert!(!dir.join("2.lock").exists());
        assert!(!dir.join("2.json").exists());
        assert!(dir.join("notes.txt").exists());
        drop(me);
        assert!(!dir.join("1.lock").exists());
    }

    #[test]
    fn two_open_windows_see_each_other_until_one_closes() {
        let dir = temp_dir("two");
        let a = lock_entry(&dir, 1).unwrap();
        let b = lock_entry(&dir, 2).unwrap();
        assert!(a.others_alive() && b.others_alive());
        assert!(dir.join("1.lock").exists() && dir.join("2.lock").exists());
        drop(b);
        assert!(!a.others_alive());
    }

    #[test]
    fn lists_live_metadata_and_legacy_locks_without_deleting_stale_entries() {
        let dir = temp_dir("list");
        let pid = std::process::id();
        let live = lock_entry(&dir, pid).unwrap();
        let legacy = lock_entry(&dir, u32::MAX - 1).unwrap();
        fs::write(dir.join("4294967295.lock"), b"").unwrap();
        fs::write(dir.join("orphan.json"), b"{}").unwrap();
        let snapshot = Snapshot {
            version: crate::version::VERSION.into(),
            only: true,
            tabs: vec![WindowTab {
                name: "tab1".into(),
                views: vec![WindowView {
                    slot: 0,
                    file: "한 글.tak".into(),
                    overlay: None,
                }],
            }],
        };
        write_snapshot(&live.path.with_extension("json"), &snapshot).unwrap();

        let mut out = Vec::new();
        write_list(Some(&dir), &mut out, false).unwrap();
        let windows: Vec<Value> = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            windows,
            [
                json!({"pid": pid, "session_id": session_id(pid),
                "interactive_desktop": session_id(pid).and_then(interactive_desktop),
                "version": snapshot.version, "only": true, "tabs": snapshot.tabs}),
                json!({"pid": u32::MAX - 1, "session_id": null, "interactive_desktop": null,
                "version": null, "only": null, "tabs": null})
            ]
        );
        assert!(legacy.path.exists() && dir.join("4294967295.lock").exists());
        assert!(dir.join("orphan.json").exists());

        out.clear();
        write_list(Some(&dir.join("missing")), &mut out, false).unwrap();
        assert_eq!(out, b"[]\n");
        let mut wrapped = Vec::new();
        write_list(Some(&dir.join("missing")), &mut wrapped, true).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&wrapped).unwrap();
        assert_eq!(doc, serde_json::json!({
            "producer":{"name":"tak_viewer","version":crate::version::VERSION},
            "schema":1,"items":[],
        }));
        assert!(!dir.join("missing").exists());
    }
}
