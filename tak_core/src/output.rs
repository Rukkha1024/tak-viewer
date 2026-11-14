//! Publish JSON and image output without truncating original takes or following final links.
//! Existing ordinary outputs are replaced only after a complete same-directory write.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

/// Check both the requested path and its physical parent, including not-yet-created folders.
pub(crate) fn in_input(path: &Path) -> io::Result<bool> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    let parent = absolute.parent().ok_or_else(|| refused("output needs a parent directory"))?;
    let ancestor = parent.ancestors().find(|p| p.exists()).ok_or_else(|| refused("output has no existing parent"))?;
    let physical = ancestor.canonicalize()?.join(absolute.strip_prefix(ancestor).map_err(io::Error::other)?);
    for candidate in [&absolute, &physical] {
        let mut normalized = PathBuf::new();
        for part in candidate.components() {
            match part {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => { normalized.pop(); }
                other => normalized.push(other.as_os_str()),
            }
        }
        if crate::tak::repo_root(&normalized).is_some_and(|root| crate::tak::in_repo_input(&normalized, &root)) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn destination(path: &Path) -> io::Result<PathBuf> {
    if crate::tak::is_take_path(path) || crate::takm::is_takm_path(path) {
        return Err(refused("refusing to write output to a .tak/.takm path"));
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().ok_or_else(|| refused("output needs a file name"))?;
    let resolved = parent.canonicalize()?.join(name);
    if in_input(path)? {
        return Err(refused("refusing to write output inside repository input/"));
    }
    match fs::symlink_metadata(&resolved) {
        Ok(meta) => {
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(refused("output must be a regular file, not a symbolic link"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if meta.nlink() != 1 {
                    return Err(refused("refusing to replace an output with multiple hard links"));
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(resolved)
}

/// Write completely, then atomically replace an ordinary output. The destination inode is
/// never opened for writing, so a link introduced during the write cannot truncate its target.
/// A failed write leaves the old output intact. The callback must finish/flush its encoder.
pub fn write_atomic(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let out = destination(path)?;
    let (tmp, mut file) = loop {
        let tmp = out.with_file_name(format!(".tak-viewer-output-{}-{}.part", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = write(&mut file).and_then(|()| file.sync_all());
    drop(file);
    let result = result.and_then(|()| {
        destination(&out)?;
        fs::rename(&tmp, &out)
    });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct Temp(PathBuf);
    impl Temp {
        fn new(tag: &str) -> Self {
            let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir().join(format!("tak_output_{tag}_{}_{unique}", std::process::id()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn failed_encoder_preserves_output_and_removes_temporary_file() {
        let dir = Temp::new("failure");
        let out = dir.0.join("결과 파일.png");
        fs::write(&out, b"old").unwrap();
        assert!(write_atomic(&out, |f| {
            f.write_all(b"partial")?;
            Err(io::Error::other("encoding failed"))
        }).is_err());
        assert_eq!(fs::read(&out).unwrap(), b"old");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn protected_extensions_are_case_insensitive() {
        let dir = Temp::new("extension");
        for name in ["원본.TAK", "원본.TaKm"] {
            let out = dir.0.join(name);
            fs::write(&out, b"original").unwrap();
            assert!(write_atomic(&out, |f| f.write_all(b"bad")).is_err());
            assert_eq!(fs::read(out).unwrap(), b"original");
        }
    }

    #[cfg(unix)]
    #[test]
    fn input_directory_alias_and_dangling_symlink_are_refused() {
        let dir = Temp::new("aliases");
        fs::write(dir.0.join(".tak-viewer-root"), b"tak-viewer\n").unwrap();
        fs::create_dir(dir.0.join("input")).unwrap();
        std::os::unix::fs::symlink(dir.0.join("input"), dir.0.join("alias")).unwrap();
        let out = dir.0.join("alias/결과.json");
        assert!(write_atomic(&out, |f| f.write_all(b"bad")).is_err());
        assert!(!out.exists());
        let missing = dir.0.join("missing.tak");
        let out = dir.0.join("dangling.json");
        std::os::unix::fs::symlink(&missing, &out).unwrap();
        assert!(write_atomic(&out, |f| f.write_all(b"bad")).is_err());
        assert!(!missing.exists());
    }

    #[cfg(unix)]
    #[test]
    fn link_created_during_encoding_never_changes_original() {
        let dir = Temp::new("late_link");
        let source = dir.0.join("원본.TAKM");
        let out = dir.0.join("결과.png");
        fs::write(&source, b"original").unwrap();
        assert!(write_atomic(&out, |f| {
            f.write_all(b"image")?;
            std::os::unix::fs::symlink(&source, &out)
        }).is_err());
        assert_eq!(fs::read(source).unwrap(), b"original");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);
    }
}
