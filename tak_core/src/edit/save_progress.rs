use super::*;

#[derive(Debug)]
pub struct ProgressSaved {
    pub path: PathBuf,
    pub stamp: (u64, Option<std::time::SystemTime>),
    pub fixed_len: u64,
    pub version: Option<FileVersion>,
}

/// Apply a single property change under the marker/Comment writer lock.
pub fn save_progress(
    path: &Path,
    expected: Option<i64>,
    next: Option<i64>,
    stamp: Option<(u64, Option<std::time::SystemTime>)>,
    version: Option<&FileVersion>,
) -> Result<ProgressSaved, EditError> {
    if !takm::is_takm_path(path) || takm::in_input(path) || path.to_string_lossy().contains("://") {
        return Err(error("진행 상태는 쓰기 가능한 TAKM에서만 바꿀 수 있어요."));
    }
    let path = takm::canonical_target(path).map_err(error)?;
    if !takm::is_takm_path(&path) || takm::in_input(&path) {
        return Err(error("안전하지 않은 저장 경로예요."));
    }
    comments::drain_shared_writers(&[&path]).map_err(error)?;
    let _lock = takm::lock_path(&path).map_err(error)?;
    let read_stamp = |p: &Path| {
        std::fs::metadata(p)
            .map(|meta| (meta.len(), meta.modified().ok()))
            .map_err(error)
    };
    let before = read_stamp(&path)?;
    if stamp.is_some_and(|stamp| stamp != before) && version.is_none() {
        return Err(conflict());
    }
    if let Some(version) = version {
        check_version(&path, version)?;
    }

    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let (tmp, backup_path, mut out) = loop {
        let id = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let stem = format!(".{name}.{}.{id}", std::process::id());
        let tmp = path.with_file_name(format!("{stem}.progress.tmp.takm"));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => {
                break (
                    Temp(tmp),
                    path.with_file_name(format!("{stem}.progress.backup")),
                    file,
                )
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(error(e)),
        }
    };
    let mut source = File::open(&path).map_err(error)?;
    let permissions = source.metadata().map_err(error)?.permissions();
    if permissions.readonly() {
        return Err(error("읽기 전용 파일이에요."));
    }
    let fixed_len = takm::progress::write(&mut source, &mut out, expected, next).map_err(error)?;
    out.set_permissions(permissions).map_err(error)?;
    drop(out);
    drop(source);
    takm::verify_fixed(&tmp.0).map_err(error)?;
    let new_version = if version.is_some() {
        Some(FileVersion {
            path: path.clone(),
            digest: FileVersion::capture(&tmp.0)?.digest,
        })
    } else {
        None
    };
    let stamp = read_stamp(&tmp.0)?;
    if read_stamp(&path)? != before {
        return Err(conflict());
    }
    let backup = backup_destination(&path, backup_path)?;
    if read_stamp(&path)? != before {
        return Err(conflict());
    }
    #[cfg(unix)]
    let parent = File::open(path.parent().ok_or(EditError::UnsupportedLayout)?).map_err(error)?;
    std::fs::rename(&tmp.0, &path).map_err(error)?;
    #[cfg(unix)]
    if let Err(e) = parent.sync_all() {
        if let Err(restore) = std::fs::rename(&backup.0, &path) {
            let old = backup.0.display().to_string();
            std::mem::forget(backup);
            return Err(error(format!(
                "저장 후 복구 실패: {e}; {restore}; 이전 파일: {old}"
            )));
        }
        return Err(error(e));
    }
    drop(backup);
    Ok(ProgressSaved {
        path,
        stamp,
        fixed_len,
        version: new_version,
    })
}
