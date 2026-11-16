//! Comments (주석) on a take: the selected markers + a frame range, saved next to the TAK in
//! `<TAK stem>.comments.csv` (`path_for`). No GUI code here: the viewer draws and asks.
//!
//! The file is a log: one CSV line per event (`add` / `edit` / `delete`), only ever appended
//! (`append`), never rewritten. So a save is instant and a crash can only cut the line being
//! written; earlier lines stay as they were. Reading (`read`, `parse`) folds the events into the
//! live comments (`Comments`): `add` adds, `edit` replaces that id's fields, `delete` removes it.
//! A last line cut off in the middle is ignored, other bad lines are skipped, extra columns after
//! the known ones are ignored.
//!
//! Refusals (`check_target`): never a `.tak` path, never inside the repository's `input/`.
//! Writing happens on one background thread (`Writer`), never on the drawing thread.

use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Column names, in file order (first line of the file).
pub const COLUMNS: [&str; 12] =
    ["time", "kind", "comment_id", "markers", "frame_start", "frame_end", "comment_type", "comment_text", "shown_a", "shown_b", "file_a", "file_b"];
/// Ending of a comment file name: `a.tak` -> `a.comments.csv`.
pub const SUFFIX: &str = ".comments.csv";
/// Line ending written (CSV standard, Excel). Reading also takes a bare `\n`.
const EOL: &str = "\r\n";
/// Byte order mark at the start of a new file, so Excel reads it as UTF-8 (Korean text).
const BOM: char = '\u{feff}';
/// A cut record is followed by enough empty fields to put this marker beyond legacy columns.
pub(crate) const INCOMPLETE_SUFFIX: &str = ",,,,,,,,,,,,__takm_truncated_v1__";

/// What a line of the file records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Add,
    Edit,
    Delete,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Add => "add",
            Kind::Edit => "edit",
            Kind::Delete => "delete",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        match s.trim() {
            "add" => Some(Kind::Add),
            "edit" => Some(Kind::Edit),
            "delete" => Some(Kind::Delete),
            _ => None,
        }
    }
}

/// One line of the file.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Local time with its offset, ISO 8601 (`iso_time`).
    pub time: String,
    pub kind: Kind,
    pub id: u64,
    /// Marker names, in the order they were saved.
    pub markers: Vec<String>,
    pub start: i64,
    pub end: i64,
    /// Filled from step 3 (type popup); empty now.
    pub comment_type: String,
    pub text: String,
    /// "1" / "0" / "" (step 6 on: which file was shown); kept as written.
    pub shown_a: String,
    pub shown_b: String,
    /// `file_label`: TAK file name and size, so a CSV is not mixed up with another file.
    pub file_a: String,
    pub file_b: String,
    /// Optional operation key for idempotent retries; absent in older tails.
    pub operation: Option<String>,
    /// In-memory state on which an edit/delete was based; never serialized.
    pub expected: Option<Comment>,
}

impl Event {
    /// A line about comment `c` with its fields as they are (`add` of a new or restored comment,
    /// `delete`), written at `time` for the take `file_a` (`file_label`) shown alone.
    pub fn of(kind: Kind, c: &Comment, time: String, file_a: String) -> Event {
        Event {
            time,
            kind,
            id: c.id,
            markers: c.markers.clone(),
            start: c.start,
            end: c.end,
            comment_type: c.comment_type.clone(),
            text: c.text.clone(),
            shown_a: "1".into(),
            shown_b: String::new(),
            file_a,
            file_b: String::new(),
            operation: Some(new_operation_token()),
            expected: None,
        }
    }
}

/// Give each new event a stable token, retained when the same event is retried.
pub fn new_operation_token() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{}-{nanos}-{sequence}", std::process::id())
}

/// A live comment (after folding the events).
#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub id: u64,
    pub markers: Vec<String>,
    pub start: i64,
    pub end: i64,
    pub comment_type: String,
    pub text: String,
}

impl Comment {
    fn from_event(e: &Event) -> Comment {
        Comment { id: e.id, markers: e.markers.clone(), start: e.start, end: e.end, comment_type: e.comment_type.clone(), text: e.text.clone() }
    }
}

// ───────────────────────────── Types (Comment plan step 5) ─────────────────────────────

/// The comment types (user: `Marker swap` · `Jitter` · `Needs visual check`), in their one
/// order everywhere (the type list of the COMMENTS pane, the strip of three buttons, `T`, the
/// sort): Jitter → Marker swap → Needs visual check, the order of the user's sketch and of the
/// chosen mockup (type-combo; v0.18.0 had Marker swap first). Written as these names in the
/// `comment_type` column; empty = no type. Any other value (written by hand or by a later
/// version) is kept as it is.
pub const TYPES: [&str; 3] = ["Jitter", "Marker swap", "Needs visual check"];

/// Short form of a type for a narrow column: `Swap` · `Jitter` · `Check`; no type = empty; a
/// value the app does not know as it is.
pub fn type_short(t: &str) -> &str {
    match t {
        "Marker swap" => "Swap",
        "Jitter" => "Jitter",
        "Needs visual check" => "Check",
        _ => t,
    }
}

/// Place of a type in the sort order: no type 0, the known types 1..=3 in their one order,
/// any other value 4 (those then go by text).
pub fn type_rank(t: &str) -> usize {
    if t.is_empty() {
        0
    } else {
        TYPES.iter().position(|k| *k == t).map_or(TYPES.len() + 1, |k| k + 1)
    }
}

/// The type after `t` going round: none → Jitter → Marker swap → Needs visual check → none.
/// (The key `T`.) A value the app does not know counts as standing before none (→ none).
pub fn step_type(t: &str) -> &'static str {
    // Choices in order: "" then the three types.
    let n = TYPES.len() + 1;
    let choice = |k: usize| if k == 0 { "" } else { TYPES[k - 1] };
    let at = if t.is_empty() { Some(0) } else { TYPES.iter().position(|k| *k == t).map(|k| k + 1) };
    match at {
        Some(k) => choice((k + 1) % n),
        None => choice(0),
    }
}

/// The live comments of one take, folded from its events, in id order (a comment brought back
/// by an undo goes back to its place, so the larger id stays drawn on top).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Comments {
    list: Vec<Comment>,
    /// Largest id seen in any event (deleted ones too): ids are never used twice.
    max_id: u64,
    /// Goes up with every change (the viewer's block layout is made again only then).
    revision: u64,
}

impl Comments {
    pub fn from_events(events: &[Event]) -> Comments {
        let mut c = Comments::default();
        for e in events {
            c.apply(e);
        }
        c
    }

    pub fn list(&self) -> &[Comment] {
        &self.list
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Id for a new comment: largest id seen + 1.
    pub fn next_id(&self) -> u64 {
        self.max_id + 1
    }

    pub fn get(&self, id: u64) -> Option<&Comment> {
        self.list.iter().find(|c| c.id == id)
    }

    /// Replace only this id after a concurrent edit, preserving other rows and cache revision.
    pub fn reload_one(&mut self, id: u64, latest: Option<Comment>) {
        self.list.retain(|comment| comment.id != id);
        if let Some(comment) = latest {
            let at = self.list.iter().position(|c| c.id > id).unwrap_or(self.list.len());
            self.list.insert(at, comment);
        }
        self.revision += 1;
    }

    /// Old files can contain duplicate live ids, which cannot identify one selected row.
    pub fn id_is_ambiguous(&self, id: u64) -> bool {
        self.list.iter().filter(|c| c.id == id).take(2).count() > 1
    }

    /// Fold one event in. `edit` / `delete` of an id that is not live change nothing (a deleted
    /// comment stays deleted). An `add` of a deleted id brings that comment back (undo of a
    /// delete, v0.15.0); it goes in at its id's place, after any comment with the same or a
    /// smaller id. Legacy events retain their original folding behavior when ids collided.
    /// New events with an operation key affect at most one live entry.
    pub fn apply(&mut self, e: &Event) {
        self.max_id = self.max_id.max(e.id);
        self.revision += 1;
        match e.kind {
            Kind::Add => {
                let at = self.list.iter().rposition(|c| c.id <= e.id).map_or(0, |k| k + 1);
                self.list.insert(at, Comment::from_event(e));
            }
            Kind::Edit => {
                for c in self.list.iter_mut().filter(|c| c.id == e.id) {
                    *c = Comment::from_event(e);
                    if e.operation.is_some() { break; }
                }
            }
            Kind::Delete => {
                if e.operation.is_none() { self.list.retain(|c| c.id != e.id); }
                else if let Some(index) = self.list.iter().position(|c| c.id == e.id) { self.list.remove(index); }
            }
        }
    }
}

/// Reserve a durable, disjoint range of ids for one TAKM. Run on a worker: the per-path lock
/// also covers appends and marker saves in other instances of the same operating system.
pub fn reserve_takm_ids(path: &Path, count: u64) -> io::Result<std::ops::RangeInclusive<u64>> {
    if count == 0 { return Err(io::Error::other("empty Comment id reservation")); }
    let target = crate::takm::canonical_target(path)?;
    if !crate::takm::is_takm_path(&target) || crate::takm::in_input(&target) {
        return Err(io::Error::other("unsafe Comment target"));
    }
    let _lock = crate::takm::lock_path(&target)?;
    let parsed = crate::takm::read_comments(&target).map_err(io::Error::other)?;
    let tail_max = parsed.events.iter().map(|event| event.id).max().unwrap_or(0);
    let key = crate::takm::path_lock_key(&target)?;
    let ledger = crate::takm::coordination_root().join(format!(".tak_viewer_comment_ids_{key}"));
    let mut f = OpenOptions::new().read(true).append(true).create(true).open(ledger)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    let complete_end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
    let complete = std::str::from_utf8(&bytes[..complete_end]).map_err(io::Error::other)?;
    let mut reserved_max = 0u64;
    for line in complete.lines().filter(|line| !line.is_empty()) {
        let value = line.parse::<u64>().map_err(io::Error::other)?;
        reserved_max = reserved_max.max(value);
    }
    let start = tail_max.max(reserved_max).checked_add(1).ok_or_else(|| io::Error::other("Comment ids exhausted"))?;
    let end = start.checked_add(count - 1).ok_or_else(|| io::Error::other("Comment ids exhausted"))?;
    // A cut record never produced a returned reservation. Complete it as an empty separator.
    if complete_end < bytes.len() { f.write_all(b"\n")?; }
    writeln!(f, "{end}")?;
    f.sync_all()?;
    Ok(start..=end)
}

// ───────────────────────────── Paths and refusals ─────────────────────────────

/// The comment file of a take: same folder, `<stem>.comments.csv` (`a.tak` -> `a.comments.csv`,
/// `A.TAK` -> `A.comments.csv`). Never a `.tak` path.
pub fn path_for(tak: &Path) -> PathBuf {
    let stem = tak.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "take".into());
    tak.with_file_name(format!("{stem}{SUFFIX}"))
}

/// Why the app may not write `csv`, in plain Korean (None = it may). Looks at the disk (finds the
/// repository above it): call it off the drawing thread.
pub fn check_target(csv: &Path) -> Result<(), String> {
    if crate::takm::in_input(csv) {
        return Err("input/ 폴더 안 원본 파일이라 Comment를 쓰지 않아요.".into());
    }
    if crate::tak::is_take_path(csv) {
        return Err(format!("Comment 파일 이름이 .tak으로 끝나서 쓰지 않아요: {}", csv.display()));
    }
    let start = csv.parent().unwrap_or(csv);
    if let Some(root) = start.ancestors().find(|a| a.exists()).and_then(crate::tak::repo_root) {
        if crate::tak::in_repo_input(csv, &root) {
            return Err("input/ 폴더 안 원본 파일이라 옆에 Comment 파일을 만들지 않아요. 파일을 다른 폴더에 복사해서 여세요.".into());
        }
    }
    Ok(())
}

/// `file_a` / `file_b` value: the TAK file name and its size in bytes, `name|size`
/// (Windows file names cannot hold `|`).
pub fn file_label(tak: &Path, size: u64) -> String {
    let name = tak.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    format!("{name}|{size}")
}

// ───────────────────────────── Time ─────────────────────────────

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Seconds since 1970 of [year, month, day, hour, minute, second] (no offset applied).
pub fn unix_from_civil(c: [i64; 6]) -> i64 {
    days_from_civil(c[0], c[1], c[2]) * 86_400 + c[3] * 3600 + c[4] * 60 + c[5]
}

/// [year, month, day, hour, minute, second] of seconds since 1970.
pub fn civil_from_unix(secs: i64) -> [i64; 6] {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    [y, m, d, rem / 3600, rem % 3600 / 60, rem % 60]
}

/// ISO 8601 local time with its offset from UTC in minutes: `2026-11-13T14:05:09+09:00`.
pub fn iso_time(c: [i64; 6], offset_min: i32) -> String {
    let sign = if offset_min < 0 { '-' } else { '+' };
    let o = offset_min.unsigned_abs();
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}", c[0], c[1], c[2], c[3], c[4], c[5], o / 60, o % 60)
}

// ───────────────────────────── CSV lines ─────────────────────────────

/// One CSV field: quoted when it holds a comma, a quote or a line break (quotes doubled).
fn field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// A note as it is saved (v0.17.0, the Comments tab): one line, so pasted line breaks and tabs
/// become spaces (a CRLF one space). Everything else (leading spaces, commas, quotes, `=`,
/// Korean) is kept as typed: `field` quotes what CSV needs.
pub fn clean_text(s: &str) -> String {
    s.replace("\r\n", " ").replace(['\r', '\n', '\t'], " ")
}

/// Marker names joined with `;`; a `;` or `\` inside a name gets a `\` before it.
pub fn join_markers(names: &[String]) -> String {
    names.iter().map(|n| n.replace('\\', "\\\\").replace(';', "\\;")).collect::<Vec<_>>().join(";")
}

/// The other way round of `join_markers`. An empty field = no markers.
pub fn split_markers(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => cur.push(chars.next().unwrap_or('\\')),
            ';' => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !s.is_empty() {
        out.push(cur);
    }
    out
}

/// The header line, with its line ending.
pub fn header_line() -> String {
    COLUMNS.join(",") + EOL
}

fn token_marker(token: &str) -> String {
    token.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One event as a CSV line, with its line ending.
pub fn encode(e: &Event) -> String {
    let fields = [
        e.operation.as_ref().map_or_else(|| e.time.clone(), |token| format!("@event:{}:{}", token_marker(token), e.time)),
        e.kind.as_str().to_owned(),
        e.id.to_string(),
        join_markers(&e.markers),
        e.start.to_string(),
        e.end.to_string(),
        e.comment_type.clone(),
        e.text.clone(),
        e.shown_a.clone(),
        e.shown_b.clone(),
        e.file_a.clone(),
        e.file_b.clone(),
    ];
    let mut line = fields.iter().map(|f| field(f)).collect::<Vec<_>>().join(",");
    if let Some(operation) = &e.operation {
        line.push(',');
        line.push_str(&field(&format!("operation={operation}")));
    }
    line + EOL
}

/// An event from the fields of one record (at least the known columns; more are ignored).
/// None = not an event (bad kind, id or frames).
fn decode(f: &[String]) -> Option<Event> {
    if f.len() < COLUMNS.len() {
        return None;
    }
    if f.len() > COLUMNS.len()
        && f.last().is_some_and(|s| s == "__takm_truncated_v1__")
        && f[f.len() - 12..f.len() - 1].iter().all(String::is_empty)
    { return None; }
    let operation = f.get(12).and_then(|s| s.strip_prefix("operation=")).map(str::to_owned);
    let time = if let Some(tagged) = f[0].strip_prefix("@event:") {
        let (token, time) = tagged.split_once(':')?;
        if token.is_empty() || operation.as_deref().is_none_or(|op| token_marker(op) != token) { return None; }
        time.to_owned()
    } else {
        f[0].clone()
    };
    let kind = Kind::parse(&f[1])?;
    let id: u64 = f[2].trim().parse().ok()?;
    let frame = |s: &str| s.trim().parse::<i64>().ok();
    // A delete needs only its id; add / edit need both frames.
    let (start, end) = match (frame(&f[4]), frame(&f[5]), kind) {
        (Some(a), Some(b), _) => (a.min(b), a.max(b)),
        (_, _, Kind::Delete) => (0, 0),
        _ => return None,
    };
    Some(Event {
        time,
        kind,
        id,
        markers: split_markers(&f[3]),
        start,
        end,
        comment_type: f[6].clone(),
        text: f[7].clone(),
        shown_a: f[8].clone(),
        shown_b: f[9].clone(),
        file_a: f[10].clone(),
        file_b: f[11].clone(),
        operation,
        expected: None,
    })
}

/// Records of a CSV text: (fields, ended by a line break). Quoted fields may hold commas,
/// doubled quotes and line breaks. The last record has `false` when the text stops without a
/// line break (a line cut off by a crash).
fn records(text: &str) -> Vec<(Vec<String>, bool)> {
    let mut out = Vec::new();
    let mut fields: Vec<String> = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut started) = (false, false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        started = true;
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cur.push('"');
                } else {
                    quoted = false;
                }
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            '"' if cur.is_empty() => quoted = true,
            ',' => fields.push(std::mem::take(&mut cur)),
            '\r' if chars.peek() != Some(&'\n') => cur.push(c),
            '\r' | '\n' => {
                if c == '\r' { chars.next(); }
                fields.push(std::mem::take(&mut cur));
                out.push((std::mem::take(&mut fields), true));
                started = false;
            }
            _ => cur.push(c),
        }
    }
    if started {
        fields.push(cur);
        out.push((fields, false));
    }
    out
}

/// What a comment file holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    pub events: Vec<Event>,
    /// Lines that are not events (wrong column count, bad kind / id / frames), not counting the
    /// header, blank lines and a cut-off last line.
    pub skipped: usize,
    /// The text stopped in the middle of a line (no line break at the end): that line is ignored.
    pub cut: bool,
}

impl Parsed {
    pub fn comments(&self) -> Comments {
        Comments::from_events(&self.events)
    }
}

/// Events of a comment file's text.
pub fn parse(text: &str) -> Parsed {
    let text = text.strip_prefix(BOM).unwrap_or(text);
    let mut p = Parsed::default();
    for (k, (fields, ended)) in records(text).into_iter().enumerate() {
        if !ended {
            p.cut = true;
            continue;
        }
        if fields.len() == 1 && fields[0].trim().is_empty() {
            continue;
        }
        if k == 0 && fields.first().map(|f| f.trim()) == Some(COLUMNS[0]) {
            continue;
        }
        match decode(&fields) {
            Some(e) => p.events.push(e),
            None => p.skipped += 1,
        }
    }
    p
}

/// Read a comment file. None when there is none.
pub fn read(path: &Path) -> io::Result<Option<Parsed>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(parse(&String::from_utf8_lossy(&bytes)))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The text stops inside a quoted field (a line cut off by a crash in the middle of a quote).
pub(crate) fn ends_in_quote(text: &str) -> bool {
    let mut quoted = false;
    let mut at_field_start = true;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                } else {
                    quoted = false;
                }
            }
            continue;
        }
        match c {
            '"' if at_field_start => quoted = true,
            ',' | '\r' | '\n' => {
                at_field_start = true;
                continue;
            }
            _ => {}
        }
        at_field_start = false;
    }
    quoted
}

/// Append events to a comment file: a new file gets the header first; earlier bytes are never
/// changed. When the file does not end with a line break (a line was cut off), a line break
/// (after a closing quote when the cut was inside one) goes first, so the new line stands alone.
/// Refuses `.tak` paths and the repository's `input/` (`check_target`).
pub fn append(path: &Path, events: &[Event]) -> io::Result<()> {
    check_target(path).map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e))?;
    let target = crate::takm::canonical_target(path)?;
    if crate::tak::is_take_path(&target) || crate::takm::is_takm_path(&target) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Comment 대상이 원본 파일을 가리켜요"));
    }
    let _lock = crate::takm::lock_path(&target)?;
    let mut f = OpenOptions::new().read(true).append(true).create(true).open(target)?;
    let len = f.metadata()?.len();
    let mut out = String::new();
    if len == 0 {
        out.push(BOM);
        out += &header_line();
    } else {
        let mut last = [0u8; 1];
        f.seek(SeekFrom::End(-1))?;
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            let mut text = Vec::new();
            f.seek(SeekFrom::Start(0))?;
            f.read_to_end(&mut text)?;
            if ends_in_quote(&String::from_utf8_lossy(&text)) {
                out.push('"');
            }
            // Even a fragment in the first field must put the discard marker beyond all
            // twelve legacy columns.
            out += INCOMPLETE_SUFFIX;
            out += EOL;
        }
    }
    for e in events {
        out += &encode(e);
    }
    f.write_all(out.as_bytes())?;
    f.flush()
}

// ───────────────────────────── Background writer ─────────────────────────────

/// A finished write: the `tag` it was sent with, the file, and the error if it failed.
#[derive(Debug)]
pub struct Done {
    pub tag: usize,
    pub path: PathBuf,
    pub event: Event,
    pub result: Result<(), String>,
    pub conflict: Option<crate::takm::CommentConflict>,
    pub cancelled: bool,
    pub source_stamp: Option<crate::takm::CommentWriteStamp>,
}

enum Job {
    Event { tag: usize, path: PathBuf, event: Event, cancelled: std::sync::Arc<std::sync::atomic::AtomicBool> },
    Retry(PathBuf),
    Barrier(Sender<Result<(), String>>),
}

/// One thread that appends events in the order they were sent (`append`, or
/// `takm::append_events` for a `.takm`), so the drawing
/// thread never touches the disk. `wake` runs after each write (the viewer asks for a repaint).
pub struct Writer {
    tx: Option<Sender<Job>>,
    done: Receiver<Done>,
    thread: Option<JoinHandle<()>>,
    cancellations: std::sync::Mutex<std::collections::HashMap<usize, Vec<std::sync::Weak<std::sync::atomic::AtomicBool>>>>,
}

impl Writer {
    pub fn spawn(wake: Box<dyn Fn() + Send>) -> Writer {
        let (tx, rx) = channel::<Job>();
        let (done_tx, done) = channel();
        let thread = std::thread::spawn(move || {
            let registration = shared_writers::Registration::new();
            let registration_error = registration.as_ref().err().map(|e| e.to_string());
            let mut registration = registration.ok();
            let mut failed = std::collections::HashMap::<String, String>::new();
            #[cfg(test)]
            let mut timeout_pause_done = false;
            loop {
                let job = match rx.recv_timeout(Duration::from_millis(30)) {
                    Ok(job) => job,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        #[cfg(test)]
                        if !timeout_pause_done {
                            if let (Some(paused), Some(release)) = (std::env::var_os("TAK_2C_TIMEOUT_PAUSED"), std::env::var_os("TAK_2C_TIMEOUT_RELEASE")) {
                                let _ = std::fs::write(paused, b"paused");
                                let until = Instant::now() + Duration::from_secs(10);
                                while !Path::new(&release).exists() && Instant::now() < until { std::thread::sleep(Duration::from_millis(2)); }
                            }
                            timeout_pause_done = true;
                        }
                        let Some(r) = &registration else { continue };
                        let Ok(requests) = r.requests() else { continue };
                        if requests.is_empty() { continue; }
                        // The request existed before this queue check. Any event submitted
                        // before that save must already be queued; handle it before replying.
                        match rx.try_recv() {
                            Ok(job) => job,
                            Err(std::sync::mpsc::TryRecvError::Empty) => {
                                let _ = r.answer_requests(&requests, &failed);
                                continue;
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                if let Some(r) = registration.take() { r.retire(&failed); }
                                break;
                            }
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        if let Some(r) = registration.take() { r.retire(&failed); }
                        break;
                    }
                };
                let (tag, path, event, cancelled) = match job {
                    Job::Event { tag, path, event, cancelled } => (tag, path, event, cancelled),
                    Job::Barrier(done) => {
                        let result = if let Some(e) = &registration_error { Err(e.clone()) }
                                     else { failed.values().next().cloned().map_or(Ok(()), Err) };
                        let _ = done.send(result);
                        continue;
                    }
                    Job::Retry(path) => {
                        let key = crate::takm::path_lock_key(&path).unwrap_or_else(|_| path.to_string_lossy().into_owned());
                        failed.remove(&key);
                        continue;
                    }
                };
                // The writer must register successfully before any Comment can be appended.
                let one = std::slice::from_ref(&event);
                let key = crate::takm::path_lock_key(&path).unwrap_or_else(|_| path.to_string_lossy().into_owned());
                let mut conflict = None;
                let mut source_stamp = None;
                let result = if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                    Err("Comment write cancelled".into())
                } else if let Some(e) = &registration_error {
                    Err(e.clone())
                } else if let Some(reason) = failed.get(&key) {
                    Err(reason.clone())
                } else if crate::takm::is_takm_path(&path) {
                    match crate::takm::append_events_guarded_stamped(&path, one, Some(&cancelled)) {
                        Ok((Some(found), _)) => { conflict = Some(found); Err("Comment changed in another window".into()) }
                        Ok((None, stamp)) => { source_stamp = stamp; Ok(()) },
                        Err(e) => Err(e.to_string()),
                    }
                } else {
                    append(&path, one).map_err(|e| e.to_string())
                };
                let was_cancelled = cancelled.load(std::sync::atomic::Ordering::Acquire);
                if conflict.is_none() && !was_cancelled {
                    if let Err(reason) = &result {
                        failed.entry(key).or_insert_with(|| format!("{}: {reason}", path.display()));
                    }
                }
                let _ = done_tx.send(Done { tag, path, event, result, conflict, cancelled: was_cancelled, source_stamp });
                wake();
            }
        });
        Writer { tx: Some(tx), done, thread: Some(thread), cancellations: std::sync::Mutex::new(std::collections::HashMap::new()) }
    }

    /// Queue one event for `path`. False when the thread is gone.
    pub fn send(&self, tag: usize, path: PathBuf, mut event: Event) -> bool {
        if event.operation.is_none() { event.operation = Some(new_operation_token()); }
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Ok(mut all) = self.cancellations.lock() {
            let entries = all.entry(tag).or_default();
            entries.retain(|old| old.strong_count() > 0);
            entries.push(std::sync::Arc::downgrade(&cancelled));
        }
        self.tx.as_ref().is_some_and(|tx| tx.send(Job::Event { tag, path, event, cancelled }).is_ok())
    }

    /// Do not wait for a locked disk; queued work checks cancellation before writing.
    pub fn cancel_tag(&self, tag: usize) {
        if let Ok(mut all) = self.cancellations.lock() {
            if let Some(entries) = all.remove(&tag) {
                for entry in entries {
                    if let Some(flag) = entry.upgrade() { flag.store(true, std::sync::atomic::Ordering::Release); }
                }
            }
        }
    }

    /// Allow a failed path to accept a retry, before re-enqueuing its events in order.
    pub fn retry(&self, path: PathBuf) -> bool {
        self.tx.as_ref().is_some_and(|tx| tx.send(Job::Retry(path)).is_ok())
    }

    /// Queue a barrier without blocking the UI. Move its receiver to a worker and wait there.
    /// The first successful barrier also proves this instance is registered; enable Comment
    /// submission only after that acknowledgement. Earlier failures also remain in `poll()`.
    pub fn barrier(&self) -> io::Result<Receiver<Result<(), String>>> {
        let (done, wait) = channel();
        self.tx.as_ref().ok_or_else(|| io::Error::other("Comment writer stopped"))?
            .send(Job::Barrier(done)).map_err(|_| io::Error::other("Comment writer stopped"))?;
        Ok(wait)
    }

    /// Blocking convenience for non-UI callers.
    pub fn drain(&self) -> io::Result<()> {
        self.barrier()?.recv().map_err(|_| io::Error::other("Comment writer stopped"))?
            .map_err(io::Error::other)
    }

    /// Writes finished since the last call.
    pub fn poll(&self) -> Vec<Done> {
        self.done.try_iter().collect()
    }
}

impl Drop for Writer {
    /// Let queued writes finish (they take milliseconds), but never hang the app's exit on a
    /// stuck disk: wait at most 2 s.
    fn drop(&mut self) {
        self.tx = None;
        let Some(t) = self.thread.take() else { return };
        let until = Instant::now() + Duration::from_secs(2);
        while !t.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        if t.is_finished() {
            let _ = t.join();
        }
    }
}

// A writer registers on its own background thread. Save requests ask every registered writer
// to drain its queue before acquiring the target locks. The registration lock makes process
// death detectable; request/answer files are published only after complete writes.
mod shared_writers {
    use std::collections::HashMap;
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    fn nonce() -> String {
        format!("{}-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos(), SEQUENCE.fetch_add(1, Ordering::Relaxed))
    }
    pub(super) fn root() -> PathBuf { crate::takm::coordination_root().join("tak_viewer_comment_registry_v1") }
    fn root_lock(root: &Path) -> io::Result<File> {
        std::fs::create_dir_all(root)?;
        let file = OpenOptions::new().read(true).write(true).create(true).open(root.join("registry.lock"))?;
        file.lock()?;
        Ok(file)
    }
    fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let tmp = path.with_file_name(format!(".publish.{}", nonce()));
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let result = file.write_all(bytes).and_then(|()| file.flush());
        drop(file);
        if let Err(e) = result { let _ = std::fs::remove_file(&tmp); return Err(e); }
        let result = crate::takm::place_new(&tmp, path);
        let _ = std::fs::remove_file(&tmp);
        result
    }
    pub(super) struct Registration { id: String, root: PathBuf, path: PathBuf, file: Option<File> }
    impl Registration {
        pub(super) fn new() -> io::Result<Self> {
            let root = root();
            let _guard = root_lock(&root)?;
            let id = nonce();
            let path = root.join(format!("writer.{id}.lock"));
            let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
            if let Err(e) = file.lock() { drop(file); let _ = std::fs::remove_file(&path); return Err(e); }
            Ok(Self { id, root, path, file: Some(file) })
        }
        pub(super) fn requests(&self) -> io::Result<Vec<PathBuf>> {
            let prefix = format!("request.{}.", self.id);
            Ok(std::fs::read_dir(&self.root)?.filter_map(|item| item.ok())
                .filter(|item| item.file_name().to_string_lossy().starts_with(&prefix))
                .map(|item| item.path()).collect())
        }
        pub(super) fn answer_requests(&self, requests: &[PathBuf], failed: &HashMap<String, String>) -> io::Result<()> {
            for request in requests {
                let name = request.file_name().ok_or_else(|| io::Error::other("missing request name"))?.to_string_lossy();
                let keys = match std::fs::read(&request).and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).map_err(io::Error::other)) {
                    Ok(keys) => keys,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e),
                };
                let failure = keys.iter().find_map(|key| failed.get(key)).cloned();
                let body = serde_json::to_vec(&failure).map_err(io::Error::other)?;
                let answer = self.root.join(name.replacen("request.", "answer.", 1));
                match publish(&answer, &body) {
                    Ok(()) => { let _ = std::fs::remove_file(request); }
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => { let _ = std::fs::remove_file(request); }
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
        pub(super) fn retire(mut self, failed: &HashMap<String, String>) {
            if let Ok(_guard) = root_lock(&self.root) {
                if let Ok(requests) = self.requests() { let _ = self.answer_requests(&requests, failed); }
                self.file.take();
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
    impl Drop for Registration {
        fn drop(&mut self) { self.file.take(); }
    }
    fn writer_active(path: &Path) -> io::Result<bool> {
        let probe = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        match probe.try_lock() {
            Ok(()) => Ok(false),
            Err(e) => {
                let error: io::Error = e.into();
                if error.kind() == io::ErrorKind::WouldBlock { Ok(true) } else { Err(error) }
            }
        }
    }
    struct Pending { request: PathBuf, answer: PathBuf, writer: PathBuf }
    impl Drop for Pending {
        fn drop(&mut self) { let _ = std::fs::remove_file(&self.request); let _ = std::fs::remove_file(&self.answer); }
    }
    pub(super) fn drain(paths: &[&Path]) -> io::Result<()> {
        let mut keys = paths.iter().map(|p| crate::takm::path_lock_key(p)).collect::<io::Result<Vec<_>>>()?;
        keys.sort(); keys.dedup();
        let root = root();
        let guard = root_lock(&root)?;
        let mut pending = Vec::new();
        for item in std::fs::read_dir(&root)? {
            let item = item?;
            let name = item.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_prefix("writer.").and_then(|n| n.strip_suffix(".lock")) else { continue };
            let writer = item.path();
            let probe = match OpenOptions::new().read(true).write(true).open(&writer) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            match probe.try_lock() {
                Ok(()) => { drop(probe); let _ = std::fs::remove_file(writer); continue; }
                Err(e) => {
                    let error: io::Error = e.into();
                    if error.kind() != io::ErrorKind::WouldBlock { return Err(error); }
                }
            }
            let request = root.join(format!("request.{id}.{}", nonce()));
            let answer = root.join(request.file_name().unwrap().to_string_lossy().replacen("request.", "answer.", 1));
            publish(&request, &serde_json::to_vec(&keys).map_err(io::Error::other)?)?;
            pending.push(Pending { request, answer, writer });
        }
        drop(guard);
        let until = Instant::now() + Duration::from_secs(20);
        while !pending.is_empty() {
            let mut i = 0;
            while i < pending.len() {
                match std::fs::read(&pending[i].answer) {
                    Ok(bytes) => {
                        let failure: Option<String> = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                        if let Some(failure) = failure { return Err(io::Error::other(failure)); }
                        pending.swap_remove(i);
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        if !writer_active(&pending[i].writer)? && !pending[i].answer.exists() {
                            return Err(io::Error::other("Comment writer stopped during save barrier"));
                        }
                        i += 1;
                    }
                    Err(e) => return Err(e),
                }
            }
            if !pending.is_empty() {
                if Instant::now() >= until { return Err(io::Error::new(io::ErrorKind::TimedOut, "Comment writer did not drain")); }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }
}

/// Wait on a worker until every registered instance has drained earlier Comment events.
pub(crate) fn drain_shared_writers(paths: &[&Path]) -> io::Result<()> { shared_writers::drain(paths) }

#[cfg(test)]
pub(crate) fn test_shared_request_for_process(pid: u32) -> bool {
    let prefix = format!("request.{pid}-");
    std::fs::read_dir(shared_writers::root()).ok().is_some_and(|entries| entries.flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with(&prefix)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: Kind, id: u64, markers: &[&str], start: i64, end: i64) -> Event {
        Event {
            time: "2026-11-13T14:05:09+09:00".into(),
            kind,
            id,
            markers: markers.iter().map(|m| m.to_string()).collect(),
            start,
            end,
            comment_type: String::new(),
            text: String::new(),
            shown_a: "1".into(),
            shown_b: String::new(),
            file_a: "a.tak|1234".into(),
            file_b: String::new(),
            operation: None,
            expected: None,
        }
    }

    /// A fresh folder under the system temp folder (outside the repository).
    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tak_comments_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_line_round_trips_with_commas_quotes_line_breaks_and_semicolons_in_names() {
        let mut e = ev(Kind::Add, 7, &["LKNE", "a;b", "c\\d", "e,f", "g\"h"], 120, 340);
        e.text = "jitter, then \"swap\"\nsecond line\r\nthird".into();
        e.comment_type = "Marker swap".into();
        e.file_a = "x, y.tak|99".into();
        let line = encode(&e);
        assert!(line.ends_with("\r\n"));
        let p = parse(&(header_line() + &line));
        assert_eq!((p.events.len(), p.skipped, p.cut), (1, 0, false));
        assert_eq!(p.events[0], e);
        // Names round-trip through the markers field on their own too.
        let names: Vec<String> = ["", ";", "\\", "a;;b", "x\\;y"].iter().map(|s| s.to_string()).collect();
        assert_eq!(split_markers(&join_markers(&names)), names);
        assert!(split_markers("").is_empty(), "an empty field = no markers");
        // Plain fields are not quoted; the header is the column list.
        assert_eq!(encode(&ev(Kind::Delete, 3, &["M"], 5, 5)), "2026-11-13T14:05:09+09:00,delete,3,M,5,5,,,1,,a.tak|1234,\r\n");
        assert_eq!(header_line(), "time,kind,comment_id,markers,frame_start,frame_end,comment_type,comment_text,shown_a,shown_b,file_a,file_b\r\n");
    }

    #[test]
    fn old_keyed_and_unkeyed_lines_still_replay_and_new_tokens_are_unique() {
        let old = "t,add,1,M,1,2,,,1,,a.takm|1,,operation=old-token\r\nt,add,2,N,3,4,,,1,,a.takm|1,\r\n";
        let parsed = parse(&(header_line() + old));
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.events[0].operation.as_deref(), Some("old-token"));
        assert!(parsed.events[1].operation.is_none());
        assert_eq!(parsed.comments().len(), 2);
        let base = parsed.comments().get(1).unwrap().clone();
        let a = Event::of(Kind::Edit, &base, "t".into(), "a".into());
        let b = Event::of(Kind::Edit, &base, "t".into(), "a".into());
        assert_ne!(a.operation, b.operation);
        assert_eq!(parse(&encode(&a)).events, vec![a]);
    }

    #[test]
    fn note_text_round_trips_through_edit_lines_and_line_breaks_become_spaces() {
        // v0.17.0: the Comments tab writes the note (`comment_text`) with `edit` lines.
        assert_eq!(clean_text("a\r\nb\nc\rd\te"), "a b c d e");
        let notes = ["  leading spaces", "=SUM(A1)", "comma, and \"quotes\"", "한글 메모, 마커 바뀜", "trailing ", "\"", "=", ""];
        let base = Comment { id: 1, markers: vec!["LKNE".into()], start: 3, end: 9, comment_type: String::new(), text: String::new() };
        let mut text = header_line() + &encode(&Event::of(Kind::Add, &base, "t".into(), "a.tak|1".into()));
        let mut want = Comments::from_events(&parse(&text).events);
        for n in notes {
            let c = Comment { text: clean_text(n), ..base.clone() };
            assert_eq!(c.text, n, "kept as typed");
            let e = Event::of(Kind::Edit, &c, "t".into(), "a.tak|1".into());
            want.apply(&e);
            text += &encode(&e);
            let p = parse(&text);
            assert_eq!((p.skipped, p.cut), (0, false), "{n:?}");
            assert_eq!(p.comments().list(), want.list(), "{n:?}");
            assert_eq!(p.comments().get(1).unwrap().text, n);
        }
        // A pasted multi-line note is one line in the file.
        let c = Comment { text: clean_text("first\r\nsecond"), ..base.clone() };
        let line = encode(&Event::of(Kind::Edit, &c, "t".into(), "a.tak|1".into()));
        assert_eq!(line.matches("\r\n").count(), 1);
        assert_eq!(parse(&(header_line() + &line)).events[0].text, "first second");
        // Old files (no edit lines) read the same; a cut-off last edit line is ignored.
        let add = encode(&Event::of(Kind::Add, &base, "t".into(), "a.tak|1".into()));
        let edit = encode(&Event::of(Kind::Edit, &Comment { text: "x, y".into(), ..base.clone() }, "t".into(), "a.tak|1".into()));
        let p = parse(&(header_line() + &add + &edit[..edit.len() - 6]));
        assert_eq!((p.comments().list(), p.cut), (&[base.clone()][..], true));
    }

    #[test]
    fn a_comment_without_markers_round_trips_as_no_markers_and_old_lines_read_the_same() {
        let c = Comment { id: 4, markers: vec![], start: 2, end: 9, comment_type: String::new(), text: String::new() };
        let mut add = Event::of(Kind::Add, &c, "2026-11-13T14:05:09+09:00".into(), "a.tak|1234".into());
        add.operation = None; // This assertion checks the legacy 12-column format.
        let line = encode(&add);
        assert_eq!(line, "2026-11-13T14:05:09+09:00,add,4,,2,9,,,1,,a.tak|1234,\r\n", "an empty markers cell");
        let del = Event::of(Kind::Delete, &c, "t".into(), "a.tak|1234".into());
        let p = parse(&(header_line() + &line + &encode(&del) + &line));
        assert_eq!((p.events.len(), p.skipped), (3, 0));
        assert!(p.events.iter().all(|e| e.markers.is_empty()), "no markers, not one empty name");
        assert_eq!(p.events[0], add);
        let live = Comments::from_events(&p.events);
        assert_eq!(live.list(), &[c.clone()], "added, deleted, brought back");
        assert_eq!(Comment::from_event(&p.events[2]), c);
        // Lines written before v0.15.2 (always with markers) read as before.
        let old = "2026-11-13T16:01:10+09:00,add,1,LKNE,300,1400,,,1,,a.tak|32663605,\r\n2026-11-13T16:04:40+09:00,add,4,LASI;RASI,3200,3700,,,1,,a.tak|32663605,\r\n";
        let p = parse(&(header_line() + old));
        let names: Vec<Vec<String>> = p.events.iter().map(|e| e.markers.clone()).collect();
        assert_eq!(names, vec![vec!["LKNE".to_owned()], vec!["LASI".to_owned(), "RASI".to_owned()]]);
        assert_eq!(p.events.iter().map(encode).collect::<String>(), old, "and write back the same");
    }

    #[test]
    fn events_fold_into_the_live_comments() {
        let mut edit = ev(Kind::Edit, 1, &["RKNE"], 10, 20);
        edit.text = "fixed".into();
        let events = [ev(Kind::Add, 1, &["LKNE"], 1, 5), ev(Kind::Add, 2, &["M"], 7, 7), edit, ev(Kind::Delete, 2, &[], 0, 0), ev(Kind::Edit, 9, &["X"], 1, 1)];
        let c = Comments::from_events(&events);
        assert_eq!(c.len(), 1);
        assert_eq!(c.list()[0], Comment { id: 1, markers: vec!["RKNE".into()], start: 10, end: 20, comment_type: String::new(), text: "fixed".into() });
        assert_eq!(c.next_id(), 10, "largest id seen + 1, the edit of a missing id counts too");
        // Ids are never used twice: after deleting the newest one the next id is still higher.
        let mut c = Comments::from_events(&[ev(Kind::Add, 1, &["A"], 0, 0), ev(Kind::Add, 2, &["B"], 0, 0)]);
        c.apply(&ev(Kind::Delete, 2, &["B"], 0, 0));
        assert_eq!((c.len(), c.next_id()), (1, 3));
        c.apply(&ev(Kind::Delete, 1, &["A"], 0, 0));
        assert_eq!((c.len(), c.next_id()), (0, 3));
        // A deleted comment stays deleted (a later edit of it changes nothing).
        c.apply(&ev(Kind::Edit, 1, &["A"], 4, 4));
        assert!(c.is_empty());
        // A new operation targets only one entry, even in a legacy collision.
        let mut c = Comments::from_events(&[ev(Kind::Add, 4, &["A"], 0, 0), ev(Kind::Add, 4, &["B"], 1, 1)]);
        assert_eq!(c.len(), 2);
        let r = c.revision();
        let mut delete = ev(Kind::Delete, 4, &[], 0, 0);
        delete.operation = Some("new-operation".into());
        c.apply(&delete);
        assert_eq!(c.len(), 1);
        assert_eq!(c.list()[0].markers, vec!["B"]);
        assert!(c.revision() > r, "every change moves the revision");
        let legacy = Comments::from_events(&[ev(Kind::Add, 4, &["A"], 0, 0), ev(Kind::Add, 4, &["B"], 1, 1), ev(Kind::Delete, 4, &[], 0, 0)]);
        assert!(legacy.is_empty(), "old histories retain their original replay behavior");
        // Frames written in the wrong order are put in order.
        let p = parse(&encode(&ev(Kind::Add, 1, &["A"], 9, 3)));
        assert_eq!((p.events[0].start, p.events[0].end), (3, 9));
    }

    #[test]
    fn a_deleted_comment_restored_with_its_id_comes_back_in_its_place_and_ids_are_not_reused() {
        // add 1, add 2, add 3, then undo / redo of 2: delete → add (same id) → delete → add.
        let two = ev(Kind::Add, 2, &["B"], 4, 9);
        let mut events = vec![ev(Kind::Add, 1, &["A"], 0, 1), two.clone(), ev(Kind::Add, 3, &["C"], 5, 5)];
        let ids = |c: &Comments| c.list().iter().map(|c| c.id).collect::<Vec<_>>();
        let mut c = Comments::from_events(&events);
        for (kind, want) in [(Kind::Delete, vec![1, 3]), (Kind::Add, vec![1, 2, 3]), (Kind::Delete, vec![1, 3]), (Kind::Add, vec![1, 2, 3])] {
            let e = Event { kind, ..two.clone() };
            c.apply(&e);
            events.push(e);
            assert_eq!(ids(&c), want, "after {kind:?}");
            assert_eq!(c.next_id(), 4, "a restored id is the same comment, not a reuse: next is still largest + 1");
            assert_eq!(Comments::from_events(&events).list(), c.list(), "folding the whole log gives the same list");
        }
        assert_eq!(c.get(2), Some(&Comment { id: 2, markers: vec!["B".into()], start: 4, end: 9, comment_type: String::new(), text: String::new() }), "same fields");
        // Undo of the newest add then redo: back on top.
        c.apply(&ev(Kind::Delete, 3, &["C"], 5, 5));
        c.apply(&ev(Kind::Add, 3, &["C"], 5, 5));
        assert_eq!((ids(&c), c.next_id()), (vec![1, 2, 3], 4));
        // `Event::of` writes a comment's fields as they are.
        let e = Event::of(Kind::Delete, c.get(2).unwrap(), "t".into(), "a.tak|7".into());
        assert!(e.operation.is_some());
        assert_eq!(parse(&encode(&e)).events, vec![e]);
        // A file whose ids are out of order (hand edit) folds into id order.
        let c = Comments::from_events(&[ev(Kind::Add, 5, &["A"], 0, 0), ev(Kind::Add, 3, &["B"], 0, 0), ev(Kind::Add, 3, &["C"], 1, 1)]);
        assert_eq!(c.list().iter().map(|c| (c.id, c.markers[0].as_str())).collect::<Vec<_>>(), vec![(3, "B"), (3, "C"), (5, "A")]);
    }

    #[test]
    fn a_cut_off_last_line_and_bad_middle_lines_are_skipped_extra_columns_are_ignored() {
        let good = |id| encode(&ev(Kind::Add, id, &["M"], 1, 2));
        // Cut in the middle of the last line: no line break at the end.
        let full = header_line() + &good(1) + &good(2);
        let cut = &full[..full.len() - 10];
        let p = parse(cut);
        assert_eq!((p.events.len(), p.skipped, p.cut), (1, 0, true));
        // Cut inside a quoted field of the last line.
        let mut q = ev(Kind::Add, 3, &["M"], 1, 2);
        q.text = "a, b".into();
        let line = encode(&q);
        let p = parse(&(header_line() + &good(1) + &line[..line.find("a, b").unwrap() + 2]));
        assert_eq!((p.events.len(), p.cut), (1, true));
        // Bad lines in the middle: too few columns, unknown kind, bad id, bad frames, blank line.
        let text = header_line()
            + &good(1)
            + "2026-11-13T14:05:09+09:00,add,2,M,1\r\n"
            + "2026-11-13T14:05:09+09:00,remove,3,M,1,2,,,1,,a.tak|1,\r\n"
            + "2026-11-13T14:05:09+09:00,add,x,M,1,2,,,1,,a.tak|1,\r\n"
            + "2026-11-13T14:05:09+09:00,add,4,M,one,2,,,1,,a.tak|1,\r\n"
            + "\r\n"
            + &good(5);
        let p = parse(&text);
        assert_eq!(p.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 5]);
        assert_eq!((p.skipped, p.cut), (4, false));
        // A delete needs only its id.
        let p = parse("t,delete,5,,,,,,,,,\n");
        assert_eq!((p.events.len(), p.events[0].kind, p.events[0].id), (1, Kind::Delete, 5));
        // Extra columns after the known ones (a later version) are ignored; bare \n works.
        let p = parse(&format!("{},extra_1,extra_2\n2026-11-13T14:05:09+09:00,add,6,M,3,4,,,1,,a.tak|1,,more,\"x,y\"\n", COLUMNS.join(",")));
        assert_eq!((p.events.len(), p.events[0].id, p.events[0].end, p.skipped), (1, 6, 4, 0));
        // The BOM of a new file is not part of the first column name.
        let p = parse(&format!("\u{feff}{}{}", header_line(), good(8)));
        assert_eq!((p.events.len(), p.skipped), (1, 0));
    }

    #[test]
    fn the_comment_file_sits_next_to_the_take_and_is_never_a_take() {
        let p = |s: &str| path_for(Path::new(s));
        assert_eq!(p("a.tak"), PathBuf::from("a.comments.csv"));
        assert_eq!(p(r"C:\data\x\A.TAK"), PathBuf::from(r"C:\data\x\A.comments.csv"), "upper-case .TAK");
        assert_eq!(p("/d/000000_S01 perturb 20_001.tak"), PathBuf::from("/d/000000_S01 perturb 20_001.comments.csv"));
        assert_eq!(p("/d/a.b.tak"), PathBuf::from("/d/a.b.comments.csv"), "only the last extension goes");
        assert_eq!(p("/d/a.tak.tak"), PathBuf::from("/d/a.tak.comments.csv"));
        assert_eq!(p("/d/noext"), PathBuf::from("/d/noext.comments.csv"));
        assert_eq!(p("/d/한글.tak"), PathBuf::from("/d/한글.comments.csv"));
        for s in ["a.tak", "A.TAK", ".tak", "x.tak.tak"] {
            assert!(!crate::tak::is_take_path(&p(s)), "{s}");
        }
        // A .tak target is refused, also by `append`, which then creates nothing.
        let dir = temp_dir("refuse");
        let tak = dir.join("x.TAK");
        assert!(check_target(&tak).is_err());
        assert!(append(&tak, &[ev(Kind::Add, 1, &["M"], 0, 0)]).is_err());
        assert!(!tak.exists());
        assert!(check_target(&dir.join("x.comments.csv")).is_ok(), "outside any repository");
        let native_take = if cfg!(windows) { Path::new(r"C:\d\a.tak") } else { Path::new("/d/a.tak") };
        assert_eq!(file_label(native_take, 33_121_234), "a.tak|33121234");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_written_inside_the_repository_input_folder() {
        let root = temp_dir("repo");
        std::fs::create_dir_all(root.join("apps")).unwrap();
        std::fs::create_dir_all(root.join("input").join("raw")).unwrap();
        std::fs::create_dir_all(root.join("experiments").join("x")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "x").unwrap();
        std::fs::write(root.join("apps").join("AGENTS.md"), "x").unwrap();
        let inside = path_for(&root.join("input").join("raw").join("a.tak"));
        let err = check_target(&inside).unwrap_err();
        assert!(err.contains("input/"), "{err}");
        assert!(append(&inside, &[ev(Kind::Add, 1, &["M"], 0, 0)]).is_err());
        assert!(!inside.exists(), "no file was made");
        // Other folders of the repository are fine; so is a folder that only looks like input.
        assert!(check_target(&path_for(&root.join("experiments").join("x").join("a.tak"))).is_ok());
        assert!(!crate::tak::in_repo_input(&root.join("inputs").join("a.csv"), &root));
        assert!(crate::tak::in_repo_input(&root.join("INPUT"), &root), "any case (Windows)");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn appending_keeps_earlier_bytes_and_reading_back_gives_the_same_comments() {
        let dir = temp_dir("append");
        let csv = path_for(&dir.join("a.tak"));
        assert!(read(&csv).unwrap().is_none(), "no file yet");
        let events = [ev(Kind::Add, 1, &["LKNE", "RKNE"], 10, 20), ev(Kind::Add, 2, &["M"], 5, 5), ev(Kind::Delete, 1, &["LKNE", "RKNE"], 10, 20)];
        append(&csv, &events[..1]).unwrap();
        let first = std::fs::read(&csv).unwrap();
        assert!(first.starts_with("\u{feff}time,kind,".as_bytes()), "a new file starts with the BOM and the header");
        for e in &events[1..] {
            append(&csv, std::slice::from_ref(e)).unwrap();
        }
        let all = std::fs::read(&csv).unwrap();
        assert_eq!(&all[..first.len()], &first[..], "earlier bytes are never rewritten");
        let p = read(&csv).unwrap().unwrap();
        assert_eq!(p.events, events.to_vec());
        assert_eq!(p.comments(), Comments::from_events(&events));
        assert_eq!(p.comments().list().iter().map(|c| c.id).collect::<Vec<_>>(), vec![2]);
        // After a crash cut the last line (here inside a quote), the next line stands alone.
        let mut q = ev(Kind::Add, 3, &["M"], 1, 2);
        q.text = "a, b".into();
        let line = encode(&q);
        let mut f = OpenOptions::new().append(true).open(&csv).unwrap();
        f.write_all(line[..line.find("a, b").unwrap() + 2].as_bytes()).unwrap();
        drop(f);
        let before = std::fs::read(&csv).unwrap();
        append(&csv, &[ev(Kind::Add, 4, &["N"], 7, 8)]).unwrap();
        let after = std::fs::read(&csv).unwrap();
        assert_eq!(&after[..before.len()], &before[..]);
        let p = read(&csv).unwrap().unwrap();
        assert_eq!(p.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2, 1, 4], "the cut line is skipped, the new one is read");
        assert_eq!((p.skipped, p.cut), (1, false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_writer_thread_appends_in_order_and_reports_failures() {
        let dir = temp_dir("writer");
        let csv = path_for(&dir.join("a.tak"));
        let w = Writer::spawn(Box::new(|| {}));
        for id in 1..=5 {
            assert!(w.send(7, csv.clone(), ev(Kind::Add, id, &["M"], id as i64, id as i64)));
        }
        // A folder that does not exist: that write fails, the others still go.
        let bad = dir.join("missing").join("b.comments.csv");
        assert!(w.send(8, bad.clone(), ev(Kind::Add, 1, &["M"], 0, 0)));
        let mut done = Vec::new();
        let until = Instant::now() + Duration::from_secs(10);
        while done.len() < 6 && Instant::now() < until {
            done.extend(w.poll());
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(done.len(), 6);
        assert!(done[..5].iter().all(|d| d.tag == 7 && d.result.is_ok() && d.path == csv));
        assert!(done[5].tag == 8 && done[5].result.is_err() && done[5].path == bad);
        let ids: Vec<u64> = read(&csv).unwrap().unwrap().events.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5], "in the order sent");
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_writers_reserve_distinct_ids_and_edit_delete_one_comment() {
        let dir = temp_dir("unique_writers");
        let source = dir.join("input.tak");
        std::fs::write(&source, crate::sample_tak::sample_tak()).unwrap();
        let path = crate::takm::convert(&source, None).unwrap().out;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2).map(|n| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let id = *reserve_takm_ids(&path, 8).unwrap().start();
                let writer = Writer::spawn(Box::new(|| {}));
                let mut add = ev(Kind::Add, id, &[if n == 0 { "A" } else { "B" }], 0, 2);
                assert!(writer.send(n, path.clone(), add.clone()));
                writer.drain().unwrap();
                if n == 0 {
                    add.kind = Kind::Edit;
                    add.text = "edited".into();
                    assert!(writer.send(n, path, add));
                    writer.drain().unwrap();
                }
                id
            })
        }).collect();
        let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_ne!(ids[0], ids[1]);
        let before = crate::takm::read_comments(&path).unwrap().comments();
        assert_eq!(before.len(), 2);
        assert_eq!(before.get(ids[0]).unwrap().text, "edited");
        crate::takm::append_events(&path, &[ev(Kind::Delete, ids[0], &[], 0, 0)]).unwrap();
        let after = crate::takm::read_comments(&path).unwrap().comments();
        assert!(after.get(ids[0]).is_none());
        assert_eq!(after.get(ids[1]).unwrap().markers, vec!["B"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn comment_id_reservations_are_shared_between_processes() {
        if let Some(path) = std::env::var_os("TAK_4B_CHILD_COMMENT_PATH") {
            let path = PathBuf::from(path);
            let result = PathBuf::from(std::env::var_os("TAK_4B_CHILD_COMMENT_RESULT").unwrap());
            let id = *reserve_takm_ids(&path, 1).unwrap().start();
            let writer = Writer::spawn(Box::new(|| {}));
            assert!(writer.send(1, path, ev(Kind::Add, id, &["child"], 0, 0)));
            writer.drain().unwrap();
            std::fs::write(result, id.to_string()).unwrap();
            return;
        }
        let dir = temp_dir("unique_processes");
        let source = dir.join("input.tak");
        std::fs::write(&source, crate::sample_tak::sample_tak()).unwrap();
        let path = crate::takm::convert(&source, None).unwrap().out;
        let result = dir.join("child_id");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg("comments::tests::comment_id_reservations_are_shared_between_processes")
            .env("TAK_4B_CHILD_COMMENT_PATH", &path).env("TAK_4B_CHILD_COMMENT_RESULT", &result)
            .stdout(std::process::Stdio::null()).spawn().unwrap();
        let id = *reserve_takm_ids(&path, 1).unwrap().start();
        let writer = Writer::spawn(Box::new(|| {}));
        assert!(writer.send(0, path.clone(), ev(Kind::Add, id, &["parent"], 0, 0)));
        writer.drain().unwrap();
        assert!(child.wait().unwrap().success());
        let other: u64 = std::fs::read_to_string(result).unwrap().parse().unwrap();
        assert_ne!(id, other);
        let live = crate::takm::read_comments(&path).unwrap().comments();
        assert_eq!(live.len(), 2);
        assert_eq!(live.get(id).unwrap().markers, vec!["parent"]);
        assert_eq!(live.get(other).unwrap().markers, vec!["child"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn local_time_is_written_as_iso_8601_with_its_offset() {
        assert_eq!(iso_time([2026, 11, 13, 14, 5, 9], 540), "2026-11-13T14:05:09+09:00");
        assert_eq!(iso_time([2026, 1, 2, 3, 4, 5], -330), "2026-01-02T03:04:05-05:30");
        assert_eq!(iso_time([2026, 1, 2, 3, 4, 5], 0), "2026-01-02T03:04:05+00:00");
        for secs in [0, 951_782_400, 1_794_571_509, -86_400] {
            assert_eq!(unix_from_civil(civil_from_unix(secs)), secs);
        }
        assert_eq!(unix_from_civil([1970, 1, 1, 0, 0, 0]), 0);
        assert_eq!(unix_from_civil([2000, 3, 1, 0, 0, 0]), 951_868_800, "a known date");
        assert_eq!(civil_from_unix(unix_from_civil([2026, 11, 13, 5, 5, 9]) + 9 * 3600), [2026, 11, 13, 14, 5, 9]);
        assert_eq!(civil_from_unix(unix_from_civil([2026, 12, 31, 20, 0, 0]) + 9 * 3600), [2027, 1, 1, 5, 0, 0], "over the new year");
    }

    #[test]
    fn each_type_none_and_an_unknown_value_round_trip_and_survive_edits_of_other_fields() {
        // Comment plan step 5: the type is the readable name in the existing `comment_type`
        // column; empty = none; a value the app does not know is kept as it is.
        let base = Comment { id: 3, markers: vec!["LKNE".into()], start: 10, end: 20, comment_type: String::new(), text: "n".into() };
        for t in ["", "Marker swap", "Jitter", "Needs visual check", "Ghost, \"later\" type"] {
            let c = Comment { comment_type: t.into(), ..base.clone() };
            let add = Event::of(Kind::Add, &c, "t".into(), "a.tak|1".into());
            // Another field edited later: the type stays (an edit line is the whole comment).
            let moved = Comment { start: 12, text: "other".into(), ..c.clone() };
            let edit = Event::of(Kind::Edit, &moved, "t".into(), "a.tak|1".into());
            let text = header_line() + &encode(&add) + &encode(&edit);
            let p = parse(&text);
            assert_eq!((p.skipped, p.cut), (0, false), "{t:?}");
            assert_eq!(p.events[0], add, "{t:?}");
            let live = p.comments();
            assert_eq!(live.get(3).unwrap().comment_type, t, "kept through an edit of another field");
            assert_eq!(live.get(3), Some(&moved));
            assert_eq!(p.events.iter().map(encode).collect::<String>(), text[header_line().len()..], "written back the same");
        }
        // The known names as written (one plain cell each; the unknown one above is quoted).
        let c = Comment { comment_type: "Needs visual check".into(), ..base.clone() };
        let mut legacy_edit = Event::of(Kind::Edit, &c, "t".into(), "a.tak|1".into());
        legacy_edit.operation = None;
        assert_eq!(encode(&legacy_edit), "t,edit,3,LKNE,10,20,Needs visual check,n,1,,a.tak|1,\r\n");
        // Files from before step 5 (always an empty type) read as no type.
        let old = "2026-11-13T16:01:10+09:00,add,1,LKNE,300,1400,,,1,,a.tak|32663605,\r\n";
        let p = parse(&(header_line() + old));
        assert_eq!(p.comments().get(1).unwrap().comment_type, "");
        // One order everywhere (type-combo): Jitter → Marker swap → Needs visual check.
        // Short forms, sort places and the round of `T`.
        assert_eq!(TYPES, ["Jitter", "Marker swap", "Needs visual check"]);
        assert_eq!(TYPES.map(type_short), ["Jitter", "Swap", "Check"]);
        assert_eq!((type_short(""), type_short("Ghost")), ("", "Ghost"));
        assert_eq!(["", "Jitter", "Marker swap", "Needs visual check", "Ghost"].map(type_rank), [0, 1, 2, 3, 4]);
        let mut t = "";
        let mut seen = vec![];
        for _ in 0..4 {
            t = step_type(t);
            seen.push(t);
        }
        assert_eq!(seen, ["Jitter", "Marker swap", "Needs visual check", ""]);
        assert_eq!(step_type("Ghost"), "", "an unknown value goes to none");
    }
}
