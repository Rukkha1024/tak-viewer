//! The user-owned type list, shared across windows and read by Comment commands.

use crate::{instances, workspace};
use eframe::egui;
use serde::Serialize;
use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};
use tak_core::comment_command;
use tak_core::comment_types::TypeList;

pub const LIST_USAGE: &str = "tak_viewer --list-comment-types [--json [out.json]]";
pub const FILE_NAME: &str = "comment_types.json";
const CHECK_EVERY: Duration = Duration::from_secs(1);
const MAX_FILE: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    Saved,
    Missing,
    Broken,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub types: TypeList,
    source: Source,
}

impl Snapshot {
    fn defaults() -> Self {
        Self {
            types: TypeList::default(),
            source: Source::Missing,
        }
    }

    pub fn saved(types: TypeList) -> Self {
        Self {
            types,
            source: Source::Saved,
        }
    }

    pub fn message(&self) -> Option<&'static str> {
        match self.source {
            Source::Saved => None,
            Source::Missing => Some("Comment 종류 파일이 아직 없어서 기본 목록을 써요."),
            Source::Broken => Some("Comment 종류 파일을 읽을 수 없어서 기본 목록을 써요."),
        }
    }

    pub fn is_broken(&self) -> bool {
        self.source == Source::Broken
    }
}

pub fn path(data_dir: Option<&Path>) -> Option<PathBuf> {
    instances::storage_dir(data_dir).map(|dir| dir.join(FILE_NAME))
}

pub fn read(path: Option<&Path>) -> Snapshot {
    let Some(path) = path else {
        return Snapshot::defaults();
    };
    let load = || -> std::io::Result<TypeList> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.len() > MAX_FILE {
            return Err(ErrorKind::InvalidData.into());
        }
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_FILE + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(ErrorKind::InvalidData.into());
        }
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        let rows = serde_json::from_slice(bytes)?;
        TypeList::validate(rows).map_err(|_| ErrorKind::InvalidData.into())
    };
    match load() {
        Ok(types) => Snapshot::saved(types),
        Err(error) if error.kind() == ErrorKind::NotFound => Snapshot::defaults(),
        Err(_) => Snapshot {
            source: Source::Broken,
            ..Snapshot::defaults()
        },
    }
}

pub fn run_list(args: &[String]) -> Result<(), (i32, String)> {
    let output = match args {
        [] => None,
        [flag] if flag == "--json" => None,
        [flag, file] if flag == "--json" && !file.starts_with('-') && !file.trim().is_empty() => {
            Some(workspace::resolve_path(file))
        }
        _ => {
            return Err((
                2,
                format!("Comment 종류 목록 옵션을 확인해 주세요.\n{LIST_USAGE}"),
            ))
        }
    };
    let dir = instances::data_dir_override().map_err(|msg| (2, msg))?;
    let path = path(dir.as_deref());
    let snapshot = read(path.as_deref());
    if args.is_empty() {
        let file = path
            .as_deref()
            .map_or_else(|| "(없음)".into(), |p| p.display().to_string());
        println!("{file}");
        if let Some(message) = snapshot.message() {
            println!("{message}");
        }
        for row in snapshot.types.rows() {
            println!("{}\t{}", row.name, row.meaning);
        }
        return Ok(());
    }
    let output = output
        .as_deref()
        .map(comment_command::check_output)
        .transpose()
        .map_err(|msg| (1, msg))?;
    // Keep the settings file reserved for confirmed window writes, including through aliases.
    if output
        .as_deref()
        .and_then(Path::file_name)
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(FILE_NAME))
    {
        return Err((
            1,
            "Comment 종류 파일은 설정 창에서만 바꿀 수 있어요.".into(),
        ));
    }
    #[derive(Serialize)]
    struct Listed<'a> {
        schema_version: u32,
        path: Option<&'a Path>,
        source: Source,
        message: Option<&'static str>,
        types: &'a TypeList,
    }
    let listed = Listed {
        schema_version: 1,
        path: path.as_deref(),
        source: snapshot.source,
        message: snapshot.message(),
        types: &snapshot.types,
    };
    comment_command::json_output(&listed, output.as_deref()).map_err(|msg| (1, msg))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub before: TypeList,
    pub after: TypeList,
}

fn save(path: &Path, next: TypeList, expected: Option<&TypeList>) -> Result<Change, String> {
    let error = |e| format!("Comment 종류를 저장할 수 없어요. ({e})");
    let dir = path
        .parent()
        .ok_or("Comment 종류를 저장할 폴더가 없어요.")?;
    if tak_core::takm::in_input(path)
        || fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err("이 곳에는 Comment 종류를 저장할 수 없어요.".into());
    }
    fs::create_dir_all(dir).map_err(error)?;
    let _lock = tak_core::takm::lock_path(path).map_err(error)?;
    let current = read(Some(path));
    if expected
        .is_some_and(|expected| current.source != Source::Saved || *expected != current.types)
    {
        return Err("다른 창에서 Comment 종류가 바뀌었어요.".into());
    }
    let mut bytes = serde_json::to_vec_pretty(&next)
        .map_err(|e| format!("Comment 종류를 저장할 수 없어요. ({e})"))?;
    bytes.push(b'\n');
    tak_core::cli_json::replace_file(path, &bytes).map_err(error)?;
    Ok(Change {
        before: current.types,
        after: next,
    })
}

struct Request {
    next: TypeList,
    expected: Option<TypeList>,
}

pub enum Update {
    Loaded(Snapshot),
    Saved(Result<Change, String>),
}

pub struct Store {
    requests: Option<Sender<Request>>,
    pub updates: Receiver<Update>,
    worker: Option<JoinHandle<()>>,
}

impl Store {
    pub fn start(path: PathBuf, wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (tx, requests) = mpsc::channel::<Request>();
        let (updates, rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("comment-types".into())
            .spawn(move || {
                let publish = |event| {
                    if updates.send(event).is_ok() {
                        wake();
                    }
                };
                let mut observed = None;
                let mut stamp = None;
                loop {
                    let next_stamp = file_stamp(&path);
                    if observed.as_ref().is_none_or(Snapshot::is_broken) || stamp != next_stamp {
                        let snapshot = read(Some(&path));
                        if observed.as_ref() != Some(&snapshot) {
                            observed = Some(snapshot.clone());
                            publish(Update::Loaded(snapshot));
                        }
                        stamp = next_stamp;
                    }
                    match requests.recv_timeout(CHECK_EVERY) {
                        Ok(request) => {
                            let result = save(&path, request.next, request.expected.as_ref());
                            observed = None;
                            publish(Update::Saved(result));
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok(Self {
            requests: Some(tx),
            updates: rx,
            worker: Some(worker),
        })
    }

    pub fn save(&self, next: TypeList, expected: Option<TypeList>) -> bool {
        self.requests
            .as_ref()
            .is_some_and(|tx| tx.send(Request { next, expected }).is_ok())
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Drain a confirmed Save before an exiting window drops its process.
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn file_stamp(path: &Path) -> Option<(u64, Option<SystemTime>, Option<SystemTime>, bool)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    Some((
        metadata.len(),
        metadata.modified().ok(),
        metadata.created().ok(),
        metadata.is_file(),
    ))
}

pub fn watch(path: PathBuf, ctx: &egui::Context) -> std::io::Result<Store> {
    let wake = ctx.clone();
    Store::start(path, move || wake.request_repaint())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tak_core::comment_types::Definition;
    use tak_core::test_support::TempDir;

    fn example(name: &str) -> TypeList {
        TypeList::validate(vec![Definition {
            name: name.into(),
            meaning: "정상, \"확인\"".into(),
        }])
        .unwrap()
    }

    #[test]
    fn missing_and_broken_lists_use_defaults_and_require_an_explicit_save() {
        let dir = TempDir::new("comment_types");
        let file = path(Some(dir.as_ref())).unwrap();
        assert_eq!(file, dir.join(FILE_NAME));
        let missing = read(Some(&file));
        assert_eq!(missing, Snapshot::defaults());
        assert!(missing.message().is_some());
        assert!(!file.exists());
        for bad in [
            b"{bad}".as_slice(),
            b"\xff",
            br#"[{"name":" A ","meaning":""},{"name":"a","meaning":""}]"#,
        ] {
            fs::write(&file, bad).unwrap();
            let broken = read(Some(&file));
            assert!(broken.is_broken());
            assert!(broken.message().is_some());
            assert_eq!(broken.types, missing.types);
            assert_eq!(fs::read(&file).unwrap(), bad);
        }
        let custom = example("Start");
        let change = save(&file, custom.clone(), None).unwrap();
        assert_eq!(
            (change.before, change.after),
            (missing.types, custom.clone())
        );
        assert_eq!(read(Some(&file)), Snapshot::saved(custom));
    }

    fn await_snapshot(store: &Store, want: &TypeList) {
        loop {
            match store.updates.recv_timeout(Duration::from_secs(30)).unwrap() {
                Update::Loaded(snapshot) if snapshot.types == *want => return,
                Update::Loaded(_) => continue,
                Update::Saved(result) => {
                    result.unwrap();
                }
            }
        }
    }

    #[test]
    fn two_windows_refresh_and_replace_whole_lists_without_losing_utf8() {
        let dir = TempDir::new("comment_types_refresh");
        let file = dir.join(FILE_NAME);
        let first = Store::start(file.clone(), || {}).unwrap();
        let second = Store::start(file.clone(), || {}).unwrap();
        await_snapshot(&first, &TypeList::default());
        await_snapshot(&second, &TypeList::default());
        let one = example("One");
        let two = example("Two");
        assert!(first.save(one.clone(), None));
        await_snapshot(&second, &one);
        assert!(second.save(two.clone(), None));
        await_snapshot(&first, &two);
        assert_eq!(read(Some(&file)), Snapshot::saved(two.clone()));
        assert!(
            save(&file, one.clone(), Some(&one)).is_err(),
            "stale undo has no write"
        );
        assert_eq!(read(Some(&file)), Snapshot::saved(two));
    }
}
