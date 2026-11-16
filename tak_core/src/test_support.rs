//! Test-only scratch ownership. Declare guards before files, workers, and app fixtures.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// An exclusively created scratch directory, removed on success and panic unwinding.
/// It deliberately is not Clone: cloning its dereferenced path does not transfer ownership.
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(prefix: &str) -> Self {
        let parent = std::env::var_os("TAK_TEST_SCRATCH_ROOT")
            .map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        Self::new_in(&parent, prefix)
    }

    pub fn new_in(parent: &Path, prefix: &str) -> Self {
        let prefix: String = prefix.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        for _ in 0..100 {
            let path = parent.join(format!("{prefix}_{}_{}_{}", std::process::id(), nonce, NEXT.fetch_add(1, Ordering::Relaxed)));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("cannot create test scratch {}: {e}", path.display()),
            }
        }
        panic!("cannot allocate unique test scratch under {}", parent.display());
    }
}

impl std::ops::Deref for TempDir {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf { &self.0 }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path { &self.0 }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Background file handles can close just after a fixture is dropped on Windows.
        for attempt in 0..20 {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => return,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) if attempt < 19 => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(e) if std::thread::panicking() => {
                    eprintln!("test scratch cleanup failed for {}: {e}", self.0.display());
                }
                Err(e) => panic!("test scratch cleanup failed for {}: {e}", self.0.display()),
            }
        }
    }
}

/// Close subprocess handles before the directory guard runs, including after assertions fail.
pub struct ChildGuard(pub std::process::Child);
impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Supplied before a test process starts; never mutate environment in parallel tests.
pub(crate) fn coordination_root_override() -> Option<PathBuf> {
    std::env::var_os("TAK_TEST_COORDINATION_ROOT").map(PathBuf::from)
}

pub fn coordination_root() -> PathBuf { crate::takm::coordination_root() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_cleanup_unwinds_after_open_file_handles() {
        let mut path = PathBuf::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let directory = TempDir::new("tak_test_guard_panic");
            path = directory.to_path_buf();
            let _file = std::fs::File::create(directory.join("open.bin")).unwrap();
            panic!("exercise fixture failure");
        }));
        assert!(result.is_err());
        assert!(!path.exists(), "scratch survived panic: {}", path.display());
    }
}
