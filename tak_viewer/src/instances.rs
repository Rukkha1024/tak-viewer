//! Multiple viewers coordinate settings through per-process lock files. A live window
//! holds its lock; stale locks left after a crash are removed. While multiple windows are
//! open, periodic settings writes are suppressed; the last window saves on close. Command
//! runs do not register. Windows session 0 cannot display an interactive window.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

/// One open window's entry. Dropping it removes the entry.
pub struct Registration {
    path: PathBuf,
    file: Option<File>,
}

/// Folder for the entries, next to eframe's settings file
/// (`%APPDATA%\tak_viewer\data\windows`).
pub fn default_dir() -> Option<PathBuf> {
    eframe::storage_dir("tak_viewer").map(|d| d.join("windows"))
}

/// Register this process as an open window. None = could not; settings must not be saved.
pub fn register(dir: &Path) -> Option<Registration> {
    register_as(dir, &format!("{}.lock", std::process::id()))
}

fn register_as(dir: &Path, name: &str) -> Option<Registration> {
    fs::create_dir_all(dir).ok()?;
    #[cfg(unix)]
    let _gate = registry_gate(dir).ok()?;
    let path = dir.join(name);
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0);
    }
    let file = o.open(&path).ok()?;
    #[cfg(unix)]
    file.try_lock().ok()?;
    Some(Registration { path, file: Some(file) })
}

// Keep this gate file permanently: unlinking it could split concurrent callers across
// different inodes. All cooperating Unix registration/removal operations hold this lock.
#[cfg(unix)]
fn registry_gate(dir: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new().write(true).create(true).truncate(false).open(dir.join(".registry"))?;
    file.lock()?;
    Ok(file)
}

impl Registration {
    /// True if another viewer window is open. Removes entries of windows that crashed.
    pub fn others_alive(&self) -> bool {
        let Some(dir) = self.path.parent() else { return true };
        #[cfg(unix)]
        let Ok(_gate) = registry_gate(dir) else { return true };
        let Ok(entries) = fs::read_dir(dir) else { return true };
        let mut alive = false;
        for entry in entries {
            let Ok(e) = entry else { alive = true; continue };
            let p = e.path();
            if p == self.path || p.extension().map_or(true, |x| x != "lock") {
                continue;
            }
            #[cfg(unix)]
            {
                // Only an unlocked regular file is stale. Unknown failures and symlinks
                // are conservatively treated as live so settings cannot be overwritten.
                if !e.file_type().is_ok_and(|kind| kind.is_file()) {
                    alive = true;
                    continue;
                }
                let Ok(file) = OpenOptions::new().write(true).open(&p) else { alive = true; continue };
                if file.try_lock().is_err() {
                    alive = true;
                    continue;
                }
                // The gate prevents another registration of this name until unlink finishes.
                if fs::remove_file(&p).is_err() { alive = true; }
            }
            #[cfg(not(unix))]
            if fs::remove_file(&p).is_err() && p.exists() {
                alive = true;
            }
        }
        alive
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // Remove while still holding the lifetime lock and the directory gate.
            // If the gate cannot be obtained, leave an unlocked stale entry for cleanup.
            if let Some(dir) = self.path.parent() {
                if let Ok(_gate) = registry_gate(dir) {
                    let _ = fs::remove_file(&self.path);
                }
            }
            self.file.take();
        }
        #[cfg(not(unix))]
        {
            self.file.take();
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// True in Windows session 0, where no window can be shown.
pub fn no_desktop() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn ProcessIdToSessionId(process_id: u32, session_id: *mut u32) -> i32;
        }
        let mut session = u32::MAX;
        // SAFETY: the out pointer is a local u32 that lives for the call.
        let ok = unsafe { ProcessIdToSessionId(std::process::id(), &mut session) } != 0;
        ok && session == 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tak_viewer_instances_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_crashed_window_entry_is_cleaned_and_not_counted() {
        let dir = temp_dir("stale");
        let me = register_as(&dir, "1.lock").expect("register");
        fs::write(dir.join("2.lock"), b"").unwrap(); // left behind, nobody holds it
        fs::write(dir.join("notes.txt"), b"x").unwrap(); // not an entry, left alone
        assert!(!me.others_alive());
        assert!(!dir.join("2.lock").exists(), "stale entry removed");
        assert!(dir.join("notes.txt").exists());
        drop(me);
        assert!(!dir.join("1.lock").exists(), "own entry removed on drop");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_open_windows_see_each_other_until_one_closes() {
        let dir = temp_dir("two");
        let a = register_as(&dir, "1.lock").expect("register a");
        let b = register_as(&dir, "2.lock").expect("register b");
        assert!(a.others_alive());
        assert!(b.others_alive());
        assert!(dir.join("1.lock").exists() && dir.join("2.lock").exists(), "checks never remove an open entry");
        drop(b);
        assert!(!a.others_alive());
        drop(a);
        let _ = fs::remove_dir_all(&dir);
    }
    #[cfg(unix)]
    #[test]
    fn cannot_register_an_already_live_name() {
        let dir = temp_dir("duplicate");
        let a = register_as(&dir, "1.lock").unwrap();
        assert!(register_as(&dir, "1.lock").is_none());
        assert!(dir.join("1.lock").exists());
        drop(a);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_registry_is_conservatively_shared() {
        let dir = temp_dir("missing_registry");
        let a = register_as(&dir, "1.lock").unwrap();
        let moved = dir.with_extension("moved");
        fs::rename(&dir, &moved).unwrap();
        assert!(a.others_alive());
        drop(a);
        fs::remove_dir_all(moved).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn subprocess_lock_survives_probes_and_crash_is_cleaned() {
        use std::process::Command;
        use std::time::{Duration, Instant};
        if let Some(dir) = std::env::var_os("TAK_LOCK_CHILD_DIR") {
            let dir = PathBuf::from(dir);
            let _registration = register_as(&dir, "child.lock").unwrap();
            fs::write(dir.join("ready"), b"ready").unwrap();
            loop { std::thread::sleep(Duration::from_millis(20)); }
        }
        let dir = temp_dir("subprocess");
        let a = register_as(&dir, "parent.lock").unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "instances::tests::subprocess_lock_survives_probes_and_crash_is_cleaned"])
            .env("TAK_LOCK_CHILD_DIR", &dir).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !dir.join("ready").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let ready = dir.join("ready").exists();
        let alive = a.others_alive();
        let still_exists = dir.join("child.lock").exists();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(ready && alive && still_exists, "cross-process lock must remain visible");
        assert!(!a.others_alive(), "crashed process must release its lock");
        assert!(!dir.join("child.lock").exists());
        drop(a);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_cannot_unlink_a_concurrently_registered_window() {
        let dir = temp_dir("race");
        let observer = register_as(&dir, "observer.lock").unwrap();
        std::thread::scope(|scope| {
            let scan = scope.spawn(move || {
                for _ in 0..1000 { observer.others_alive(); }
            });
            for _ in 0..200 {
                let window = register_as(&dir, "window.lock").unwrap();
                assert!(dir.join("window.lock").exists());
                std::thread::yield_now();
                assert!(dir.join("window.lock").exists());
                drop(window);
            }
            scan.join().unwrap();
        });
        fs::remove_dir_all(dir).unwrap();
    }

}
