
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

/// Register this process as an open window. None = could not (then the app acts as if alone).
pub fn register(dir: &Path) -> Option<Registration> {
    register_as(dir, &format!("{}.lock", std::process::id()))
}

fn register_as(dir: &Path, name: &str) -> Option<Registration> {
    fs::create_dir_all(dir).ok()?;
    let path = dir.join(name);
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0);
    }
    let file = o.open(&path).ok()?;
    Some(Registration { path, file: Some(file) })
}

impl Registration {
    /// True if another viewer window is open. Removes entries of windows that crashed.
    pub fn others_alive(&self) -> bool {
        let Some(dir) = self.path.parent() else { return false };
        let Ok(entries) = fs::read_dir(dir) else { return false };
        let mut alive = false;
        for e in entries.flatten() {
            let p = e.path();
            if p == self.path || p.extension().map_or(true, |x| x != "lock") {
                continue;
            }
            if fs::remove_file(&p).is_err() && p.exists() {
                alive = true;
            }
        }
        alive
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
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

    fn temp_dir(tag: &str) -> tak_core::test_support::TempDir {
        tak_core::test_support::TempDir::new(&format!("tak_viewer_instances_{tag}"))
    }

    #[test]
    fn a_crashed_window_entry_is_cleaned_and_not_counted() {
        let _dir_guard = temp_dir("stale");
        let dir = _dir_guard.to_path_buf();
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

    #[cfg(windows)]
    #[test]
    fn two_open_windows_see_each_other_until_one_closes() {
        let _dir_guard = temp_dir("two");
        let dir = _dir_guard.to_path_buf();
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
}
