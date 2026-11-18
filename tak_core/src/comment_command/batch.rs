//! Multi-file Comment preflight and receipts.

use super::*;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;

pub(super) struct BatchRows(BTreeMap<String, Vec<Value>>);

impl<'de> Deserialize<'de> for BatchRows {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Files;
        impl<'de> Visitor<'de> for Files {
            type Value = BatchRows;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a nonempty file-to-rows object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut input: M) -> Result<BatchRows, M::Error> {
                let mut rows = BTreeMap::new();
                while let Some((file, values)) = input.next_entry()? {
                    if rows.insert(file, values).is_some() {
                        return Err(de::Error::custom("duplicate file key"));
                    }
                }
                if rows.is_empty() {
                    return Err(de::Error::custom("empty file-to-rows object"));
                }
                Ok(BatchRows(rows))
            }
        }
        deserializer.deserialize_map(Files)
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum AddItem {
    Result(AddResult),
    Error {
        file: String,
        status: &'static str,
        error: Option<String>,
    },
}

fn add_error(path: &Path, status: &'static str, error: Option<String>) -> AddItem {
    if let Some(error) = &error {
        eprintln!("{}: {error}", path.display());
    }
    AddItem::Error {
        file: path.display().to_string(),
        status,
        error,
    }
}

fn abandoned(
    paths: &[PathBuf],
    errors: impl IntoIterator<Item = (usize, String)>,
) -> (Vec<AddItem>, bool) {
    let mut errors: HashMap<_, _> = errors.into_iter().collect();
    let items = paths
        .iter()
        .enumerate()
        .map(|(i, path)| add_error(path, "not_written", errors.remove(&i)))
        .collect();
    (items, true)
}

pub(super) fn add(
    rows: BatchRows,
    resolve: impl Fn(&str) -> PathBuf,
    options: AddOptions,
    now: impl FnOnce() -> String,
) -> (Vec<AddItem>, bool) {
    let (paths, rows): (Vec<_>, Vec<_>) = rows
        .0
        .into_iter()
        .map(|(path, rows)| (resolve(&path), rows))
        .unzip();
    let mut identities = HashSet::new();
    let mut keys = HashSet::new();
    let mut prepared = Vec::new();
    let mut errors = Vec::new();
    for (i, (path, rows)) in paths.iter().zip(rows).enumerate() {
        let prepare = || {
            let target = PreparedAdd::new(path.clone(), &rows, options)?;
            let key = takm::path_lock_key(&target.actual).map_err(read_error)?;
            let identity = same_file::Handle::from_path(&target.actual).map_err(read_error)?;
            Ok::<_, String>((key, identity, target))
        };
        match prepare() {
            Ok((key, identity, target))
                if !keys.contains(&key) && !identities.contains(&identity) =>
            {
                keys.insert(key.clone());
                identities.insert(identity);
                prepared.push((key, target));
            }
            Ok(_) => errors.push((i, "같은 .takm 파일을 두 번 지정했어요.".into())),
            Err(error) => errors.push((i, error)),
        }
    }
    if !errors.is_empty() {
        return abandoned(&paths, errors);
    }

    // Always lock in a shared order across overlapping batches. Keep every lock
    // until the last append, so duplicate detection remains valid for all files.
    let mut order: Vec<_> = (0..prepared.len()).collect();
    order.sort_unstable_by(|&a, &b| prepared[a].0.cmp(&prepared[b].0));
    let mut locks = Vec::new();
    if !options.dry_run {
        for i in order {
            let target = &prepared[i].1;
            if !target.rows.is_empty() {
                match target.lock() {
                    Ok(lock) => locks.push(lock),
                    Err(error) => return abandoned(&paths, [(i, error)]),
                }
            }
        }
    }

    let mut plans = Vec::new();
    for (i, (_, target)) in prepared.iter_mut().enumerate() {
        match target.plan(options) {
            Ok(plan) => plans.push(plan),
            Err(error) => return abandoned(&paths, [(i, error)]),
        }
    }
    for (i, ((_, target), plan)) in prepared.iter().zip(&mut plans).enumerate() {
        if let Err(error) = target.reserve(plan) {
            return abandoned(&paths, [(i, error)]);
        }
    }

    let time = (!options.dry_run && plans.iter().any(|plan| !plan.added.is_empty())).then(now);
    let mut failed = false;
    let items = prepared
        .iter()
        .zip(plans)
        .map(|((_, target), mut plan)| {
            if failed {
                return add_error(&target.path, "not_written", None);
            }
            if let Some(time) = time.as_deref().filter(|_| !plan.added.is_empty()) {
                if let Err(error) = target.append(&mut plan, time) {
                    failed = true;
                    return add_error(&target.path, "failed", Some(error));
                }
            }
            AddItem::Result(plan)
        })
        .collect();
    (items, failed)
}

pub(super) fn discover(root: &Path, recursive: bool) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let error = |e| format!("폴더를 읽지 못했어요. ({}, {e})", dir.display());
        for item in fs::read_dir(&dir).map_err(error)? {
            let item = item.map_err(error)?;
            let kind = item.file_type().map_err(error)?;
            let path = item.path();
            if kind.is_dir() && !kind.is_symlink() {
                if recursive {
                    pending.push(path);
                }
            } else if takm::is_takm_path(&path) {
                paths.push(path);
            }
        }
    }
    paths.sort_unstable();
    Ok(paths)
}

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum ListItem {
    Read {
        #[serde(flatten)]
        list: ListResult,
        live_count: usize,
        tail_note: Option<&'static str>,
        error: Option<String>,
    },
    Unread {
        file: String,
        comments: Vec<Listed>,
        live_count: Option<usize>,
        skipped_lines: Option<usize>,
        malformed_lines: Option<usize>,
        cut_tail: Option<bool>,
        tail_note: Option<&'static str>,
        error: String,
    },
}

pub(super) fn list(paths: Vec<PathBuf>, by: Option<&str>) -> (Vec<ListItem>, bool) {
    let mut failed = false;
    let items = paths
        .into_iter()
        .map(|path| match list_by(&path, by) {
            Ok((list, live_count, tail_note)) => ListItem::Read {
                list,
                live_count,
                tail_note,
                error: None,
            },
            Err(error) => {
                failed = true;
                ListItem::Unread {
                    file: path.display().to_string(),
                    comments: Vec::new(),
                    live_count: None,
                    skipped_lines: None,
                    malformed_lines: None,
                    cut_tail: None,
                    tail_note: None,
                    error,
                }
            }
        })
        .collect();
    (items, failed)
}
