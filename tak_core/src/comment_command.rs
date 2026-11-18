//! Local TAKM Comment commands. The viewer supplies path resolution and its Comment clock.

use crate::comments::{self, Comment, Event, Kind};
use crate::tak::{fast_read::read_take_fast, is_take_path, MarkerKind, Take};
use crate::takm;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub const USAGE: &str = "\
tak_viewer --add-comments <file.takm> --from <rows.json> [--dry-run] [--allow-unknown-markers] [--allow-duplicates]
tak_viewer --list-comments <file.takm> [out.json]";

#[derive(Clone, Copy, Debug, Default)]
pub struct AddOptions {
    pub dry_run: bool,
    pub allow_unknown_markers: bool,
    pub allow_duplicates: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Row {
    markers: Vec<String>,
    start: i64,
    end: i64,
    #[serde(rename = "type")]
    comment_type: String,
    text: String,
}

impl Row {
    fn from_comment(c: &Comment) -> Self {
        Self {
            markers: c.markers.clone(),
            start: c.start,
            end: c.end,
            comment_type: c.comment_type.clone(),
            text: c.text.clone(),
        }
    }

    fn duplicate_key(&self) -> Self {
        let mut key = self.clone();
        key.markers.sort_unstable();
        if let Some(kind) = known_type(&key.comment_type) {
            key.comment_type = kind.to_owned();
        }
        key
    }

    fn into_comment(self, id: u64) -> Comment {
        Comment {
            id,
            markers: self.markers,
            start: self.start,
            end: self.end,
            comment_type: self.comment_type,
            text: self.text,
        }
    }
}

fn known_type(text: &str) -> Option<&'static str> {
    comments::TYPES
        .into_iter()
        .find(|kind| kind.eq_ignore_ascii_case(text))
}

#[derive(Debug, Serialize)]
pub struct Added {
    id: Option<u64>,
    #[serde(flatten)]
    content: Row,
}

#[derive(Debug, Serialize)]
pub struct Skipped {
    row: usize,
    id: Option<u64>,
    #[serde(flatten)]
    content: Row,
}

#[derive(Debug, Serialize)]
pub struct AddResult {
    file: String,
    added: Vec<Added>,
    skipped_existing: Vec<Skipped>,
    live_total: usize,
    fixed_unchanged: bool,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
pub struct Listed {
    id: u64,
    #[serde(flatten)]
    content: Row,
    time: String,
    by: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ListResult {
    file: String,
    comments: Vec<Listed>,
    skipped_lines: usize,
    malformed_lines: usize,
    cut_tail: bool,
}

fn target(path: &Path, writing: bool) -> Result<PathBuf, String> {
    if !takm::is_takm_path(path) {
        return Err("기존 .takm 파일을 지정해 주세요.".into());
    }
    if writing {
        takm::check_tail(path)?;
    }
    let actual = takm::canonical_target(path)
        .map_err(|e| format!(".takm 경로를 확인하지 못했어요. ({e})"))?;
    if !takm::is_takm_path(&actual) || !actual.is_file() {
        return Err(format!(
            "기존 .takm 파일을 찾지 못했어요. ({})",
            path.display()
        ));
    }
    Ok(actual)
}

fn input_rows(bytes: &[u8]) -> Result<Vec<Value>, String> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "Comment 목록을 UTF-8로 저장해 주세요.".to_owned())?;
    serde_json::from_str(text).map_err(|e| {
        format!(
            "Comment 목록을 JSON 배열로 읽지 못했어요. ({}번째 줄, {}번째 칸)",
            e.line(),
            e.column()
        )
    })
}

fn validate_row(
    value: &Value,
    names: &HashSet<&str>,
    first: i64,
    last: i64,
    allow_unknown: bool,
) -> Result<Row, String> {
    let object = value.as_object().ok_or("각 행을 JSON 객체로 써 주세요.")?;
    let mut reasons = Vec::new();
    let mut markers = Vec::new();
    if let Some(values) = object.get("markers").and_then(Value::as_array) {
        for (index, v) in values.iter().enumerate() {
            match v.as_str() {
                Some(name) if !name.is_empty() && !name.contains(['\r', '\n']) => {
                    if !allow_unknown && !names.contains(name) {
                        reasons.push(format!("파일의 라벨 마커 목록에 {name:?}이 없어요."));
                    }
                    markers.push(name.to_owned());
                }
                _ => reasons.push(format!(
                    "markers의 {}번째 이름은 비어 있지 않은 한 줄 글이어야 해요.",
                    index + 1
                )),
            }
        }
    } else {
        reasons.push("markers에 마커 이름 목록을 써 주세요. 마커가 없으면 []를 써요.".into());
    }
    let mut frame = |key: &str| match object.get(key).and_then(Value::as_i64) {
        Some(n) => {
            if !(first..=last).contains(&n) {
                reasons.push(format!(
                    "{key} 프레임 {n}은 파일 범위({first}~{last}) 밖이에요."
                ));
            }
            Some(n)
        }
        None => {
            reasons.push(format!("{key} 프레임은 정수로 써 주세요."));
            None
        }
    };
    let start = frame("start");
    let end = frame("end");
    if start.zip(end).is_some_and(|(start, end)| start > end) {
        reasons.push("시작 프레임이 끝 프레임보다 늦어요.".into());
    }
    let comment_type = match object.get("type") {
        None => "",
        Some(v) => match v.as_str() {
            Some("") => "",
            Some(raw) => known_type(raw).unwrap_or_else(|| {
                reasons.push(format!(
                    "type 값 {raw:?}을 쓸 수 없어요. {} 중 하나를 골라 주세요.",
                    comments::TYPES.join(", ")
                ));
                ""
            }),
            None => {
                reasons.push("type 값은 글로 써 주세요.".into());
                ""
            }
        },
    };
    let text = match object.get("text").and_then(Value::as_str) {
        Some(text) if !text.contains(['\r', '\n']) => comments::clean_text(text),
        Some(_) => {
            reasons.push("text에 줄바꿈을 넣을 수 없어요.".into());
            String::new()
        }
        None => {
            reasons.push("text에 한 줄 글을 써 주세요. 내용이 없으면 빈 글을 써요.".into());
            String::new()
        }
    };
    if !reasons.is_empty() {
        return Err(reasons.join(" · "));
    }
    Ok(Row {
        markers,
        start: start.expect("validated start"),
        end: end.expect("validated end"),
        comment_type: comment_type.to_owned(),
        text,
    })
}

fn validate_rows(rows: &[Value], take: &Take, allow_unknown: bool) -> Result<Vec<Row>, String> {
    let names = take
        .trajectories
        .iter()
        .filter(|t| {
            matches!(
                t.kind,
                MarkerKind::Template | MarkerKind::SkeletonExtra | MarkerKind::OtherParent
            )
        })
        .map(|t| t.name.as_str())
        .collect();
    let mut valid = Vec::with_capacity(rows.len());
    let mut errors = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match validate_row(
            row,
            &names,
            take.start_frame,
            take.end_frame.max(take.start_frame),
            allow_unknown,
        ) {
            Ok(row) => valid.push(row),
            Err(reason) => errors.push(format!("{}번째 행: {reason}", index + 1)),
        }
    }
    if errors.is_empty() {
        Ok(valid)
    } else {
        Err(errors.join("\n"))
    }
}

/// Validate and append one batch, using the same lock, ID ledger, and events as a window.
pub fn add(
    path: &Path,
    json: &[u8],
    options: AddOptions,
    now: impl FnOnce() -> String,
) -> Result<AddResult, String> {
    let actual = target(path, true)?;
    let read_error = |e| format!(".takm을 읽지 못했어요. ({e})");
    let before = takm::verify_fixed_seal(&actual).map_err(read_error)?;
    let rows = input_rows(json)?;
    let take = read_take_fast(&actual, false).map_err(read_error)?.take;
    let rows = validate_rows(&rows, &take, options.allow_unknown_markers)?;
    if !rows.is_empty() {
        OpenOptions::new()
            .read(true)
            .append(true)
            .open(&actual)
            .map_err(|e| format!(".takm에 Comment를 쓸 수 없어요. ({e})"))?;
    }
    // A dry run must not create even a lock file. Real adds keep the lookup and append
    // together so two command processes cannot both accept the same new row.
    let _lock = if options.dry_run || rows.is_empty() {
        None
    } else {
        Some(takm::lock_path(&actual).map_err(|e| format!(".takm을 잠그지 못했어요. ({e})"))?)
    };
    if takm::fixed_seal(&actual).map_err(read_error)? != before {
        return Err(".takm 고정 부분이 읽는 동안 바뀌었어요. 명령을 다시 실행해 주세요.".into());
    }

    let current = takm::read_comments(&actual).map_err(read_error)?.comments();
    let mut result = AddResult {
        file: path.display().to_string(),
        added: Vec::new(),
        skipped_existing: Vec::new(),
        live_total: current.len(),
        fixed_unchanged: true,
        dry_run: options.dry_run,
    };
    let mut seen = HashMap::new();
    if !options.allow_duplicates {
        for c in current.list() {
            seen.entry(Row::from_comment(c).duplicate_key())
                .or_insert(Some(c.id));
        }
    }
    for (index, content) in rows.into_iter().enumerate() {
        if !options.allow_duplicates {
            match seen.entry(content.duplicate_key()) {
                std::collections::hash_map::Entry::Occupied(previous) => {
                    result.skipped_existing.push(Skipped {
                        row: index + 1,
                        id: *previous.get(),
                        content,
                    });
                    continue;
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(None);
                }
            }
        }
        result.added.push(Added { id: None, content });
    }
    if options.dry_run || result.added.is_empty() {
        return Ok(result);
    }

    let ids = comments::reserve_takm_ids_locked(&actual, result.added.len() as u64)
        .map_err(|e| format!("Comment 번호를 확보하지 못했어요. ({e})"))?;
    let time = now();
    let label = comments::file_label(path, before.0);
    let mut events = Vec::with_capacity(result.added.len());
    for (added, id) in result.added.iter_mut().zip(ids) {
        added.id = Some(id);
        let mut event = Event::of(
            Kind::Add,
            &added.content.clone().into_comment(id),
            time.clone(),
            label.clone(),
        );
        // Mark the writer: result extraction must not take an agent's note for a user interval.
        event.operation = Some(format!(
            "{}{}",
            comments::AGENT_OPERATION_PREFIX,
            comments::new_operation_token()
        ));
        events.push(event);
    }
    let (conflict, _) = takm::append_events_locked(&actual, &events, None)
        .map_err(|e| format!("Comment를 저장하지 못했어요. ({e})"))?;
    if conflict.is_some() {
        return Err("다른 창에서 Comment가 바뀌었어요. 명령을 다시 실행해 주세요.".into());
    }

    let saved = takm::read_comments(&actual)
        .map_err(|e| format!("저장한 Comment를 다시 읽지 못했어요. ({e})"))?
        .comments();
    for added in &result.added {
        let id = added.id.expect("reserved ID");
        if saved.id_is_ambiguous(id)
            || saved.get(id).map(Row::from_comment).as_ref() != Some(&added.content)
        {
            return Err(format!("저장한 Comment {id}의 내용이 요청과 달라요."));
        }
    }
    let after = takm::verify_fixed_seal(&actual)
        .map_err(|e| format!("저장 뒤 .takm 고정 부분을 확인하지 못했어요. ({e})"))?;
    if after != before {
        return Err("저장 뒤 .takm 고정 부분의 SHA-256이 달라요.".into());
    }
    result.live_total = saved.len();
    Ok(result)
}

/// Read live rows with timestamps, including legacy rows with duplicate IDs.
pub fn list(path: &Path) -> Result<ListResult, String> {
    let actual = target(path, false)?;
    let read_error = |e| format!(".takm을 읽지 못했어요. ({e})");
    takm::verify_fixed(&actual).map_err(read_error)?;
    let parsed = takm::read_comments(&actual).map_err(read_error)?;
    let (live, sources) = parsed.comments_with_sources();
    let comments = live
        .list()
        .iter()
        .zip(sources)
        .map(|(c, source)| Listed {
            id: c.id,
            content: Row::from_comment(c),
            time: source.time.clone(),
            by: comments::writer_of(source),
        })
        .collect();
    Ok(ListResult {
        file: path.display().to_string(),
        comments,
        skipped_lines: parsed.skipped + usize::from(parsed.cut),
        malformed_lines: parsed.skipped,
        cut_tail: parsed.cut,
    })
}

fn check_output(path: &Path) -> Result<(), String> {
    let actual = takm::canonical_target(path)
        .map_err(|e| format!("JSON을 저장할 경로를 확인하지 못했어요. ({e})"))?;
    if [path, &actual]
        .iter()
        .any(|p| is_take_path(p) || takm::is_takm_path(p))
    {
        return Err("JSON을 .tak나 .takm 파일에 저장할 수 없어요.".into());
    }
    if takm::in_input(path) {
        return Err("input/ 폴더에는 JSON을 저장하지 않아요.".into());
    }
    Ok(())
}

#[derive(Debug)]
enum Command<'a> {
    Add {
        file: &'a str,
        from: &'a str,
        options: AddOptions,
    },
    List {
        file: &'a str,
        output: Option<&'a str>,
    },
}

fn parse_args(args: &[String]) -> Result<Command<'_>, String> {
    fn path(s: &str) -> bool {
        !s.starts_with('-') && !s.trim().is_empty()
    }
    let [mode, file, rest @ ..] = args else {
        return Err(".takm 파일을 지정해 주세요.".into());
    };
    if !path(file) {
        return Err(".takm 파일을 지정해 주세요.".into());
    }
    match mode.as_str() {
        "--list-comments" => match rest {
            [] => Ok(Command::List { file, output: None }),
            [out] if path(out) => Ok(Command::List {
                file,
                output: Some(out),
            }),
            _ => Err("--list-comments에는 JSON 저장 경로를 하나만 지정할 수 있어요.".into()),
        },
        "--add-comments" => {
            let mut options = AddOptions::default();
            let mut from = None;
            let mut args = rest.iter().map(String::as_str);
            while let Some(arg) = args.next() {
                match arg {
                    "--from" if from.is_none() => {
                        from = Some(
                            args.next()
                                .filter(|s| path(s))
                                .ok_or("--from 뒤에 JSON 파일을 지정해 주세요.")?,
                        );
                    }
                    "--dry-run" if !options.dry_run => options.dry_run = true,
                    "--allow-unknown-markers" if !options.allow_unknown_markers => {
                        options.allow_unknown_markers = true
                    }
                    "--allow-duplicates" if !options.allow_duplicates => {
                        options.allow_duplicates = true
                    }
                    _ => return Err(format!("이 옵션은 쓸 수 없어요. ({arg})")),
                }
            }
            let from = from.ok_or("--from으로 Comment 목록 JSON을 지정해 주세요.")?;
            Ok(Command::Add {
                file,
                from,
                options,
            })
        }
        _ => Err("Comment 명령을 확인해 주세요.".into()),
    }
}

fn json_output(value: &impl Serialize, output: Option<&Path>) -> Result<(), String> {
    let mut json =
        serde_json::to_vec_pretty(value).map_err(|e| format!("JSON을 만들지 못했어요. ({e})"))?;
    json.push(b'\n');
    match output {
        Some(path) => fs::write(path, json),
        None => io::stdout().lock().write_all(&json),
    }
    .map_err(|e| format!("JSON을 쓰지 못했어요. ({e})"))
}

pub fn run(
    args: &[String],
    resolve: impl Fn(&str) -> PathBuf,
    now: impl FnOnce() -> String,
) -> i32 {
    let command = match parse_args(args) {
        Ok(command) => command,
        Err(reason) => {
            eprintln!("{reason}\n{USAGE}");
            return 2;
        }
    };
    let result = match command {
        Command::Add {
            file,
            from,
            options,
        } => fs::read(resolve(from))
            .map_err(|e| format!("Comment 목록 JSON을 읽지 못했어요. ({e})"))
            .and_then(|rows| add(&resolve(file), &rows, options, now))
            .and_then(|report| json_output(&report, None)),
        Command::List { file, output } => {
            let output = output.map(&resolve);
            output
                .as_deref()
                .map_or(Ok(()), check_output)
                .and_then(|()| list(&resolve(file)))
                .and_then(|report| json_output(&report, output.as_deref()))
        }
    };
    match result {
        Ok(()) => 0,
        Err(reason) => {
            eprintln!("{reason}");
            1
        }
    }
}

#[cfg(test)]
mod tests;
