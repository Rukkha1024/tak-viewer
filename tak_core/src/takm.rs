//! `.takm`: a TAK without its camera record, plus a Comment tail (layout: `docs/takm.md`).
//!
//! Every stream of the TAK container except the camera record (`DeviceData/Track 0.trk`) is
//! kept byte for byte, one after another, uncompressed; the dropped stream and the source TAK
//! are recorded by name, size and SHA-256. Layout, little-endian:
//!
//! ```text
//! [0, 64)              header: magic "TAKM-2\r\n", layout 1, table offset / length, fixed length F
//! [64, T)              the kept streams' bytes, in container order
//! [T, F)               stream table, UTF-8 JSON
//! [F, F+32)            SHA-256 of [0, F)
//! [F+32, end)          Comment tail: the lines of a `<stem>.comments.csv` (no BOM), only appended
//! ```
//!
//! [0, F+32) is written once (`convert`) and never rewritten (`verify_fixed` checks it); the
//! tail only grows (`append_lines` / `append_events`, read with `read_tail` / `read_comments`).
//! Reading gives the same `Take` as the TAK (`read_take` here; markers first in
//! `tak::fast_read`, through the same stream walk).

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::comments;
use crate::tak::container::{sha256_hex, Container};
use crate::tak::fast_read::read_exact_at;
use crate::tak::{err, in_repo_input, is_take_path, repo_root, Result, Take, TakError};

/// First 8 bytes. The first `.takm` (markers only, removed in v0.4.0) began `TAKM\r\n\x1a\n`.
pub const MAGIC: [u8; 8] = *b"TAKM-2\r\n";
const OLD_MAGIC: [u8; 8] = *b"TAKM\r\n\x1a\n";
/// Layout number in the header (bytes 8..12).
pub const LAYOUT: u32 = 1;
pub const HEADER_LEN: u64 = 64;
/// SHA-256 of the fixed part, right after it.
pub const SEAL_LEN: u64 = 32;
/// The camera record: the only stream not copied.
pub const CAMERA_STREAM: &str = "DeviceData/Track 0.trk";
const EOL: &str = "\r\n";
/// Largest stream table accepted when reading (a real one is under 1 KB).
const MAX_TABLE: u64 = 16 << 20;

/// `.takm` extension (any case).
pub fn is_takm_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("takm"))
}

/// `<stem>.takm` next to the TAK, or in `out_dir`.
pub fn path_for(tak: &Path, out_dir: Option<&Path>) -> PathBuf {
    let stem = tak.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "take".into());
    let dir = out_dir.map(Path::to_path_buf).unwrap_or_else(|| tak.parent().map(Path::to_path_buf).unwrap_or_default());
    dir.join(format!("{stem}.takm"))
}

/// A kept stream: its name in the TAK (`Folder/Name`), its place in the TAK's stream order,
/// where its bytes are in the `.takm`.
#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    pub name: String,
    pub index: usize,
    pub offset: u64,
    pub size: u64,
}

/// A stream left out (the camera record).
#[derive(Clone, Debug, PartialEq)]
pub struct Dropped {
    pub name: String,
    pub index: usize,
    pub size: u64,
    pub sha256: String,
}

/// The TAK the `.takm` was made from.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

/// Header + stream table of a `.takm`.
#[derive(Clone, Debug, PartialEq)]
pub struct Index {
    /// Length of the fixed part (header + streams + table); the SHA-256 follows.
    pub fixed_len: u64,
    pub streams: Vec<Stream>,
    pub dropped: Vec<Dropped>,
    pub source: Source,
    pub made_by: String,
    pub made_at: String,
}

impl Index {
    /// First byte of the Comment tail.
    pub fn tail_start(&self) -> u64 {
        self.fixed_len + SEAL_LEN
    }

    /// The stream of that name when there is exactly one (the fast read's rule).
    pub fn unique(&self, name: &str) -> Option<&Stream> {
        let mut hits = self.streams.iter().filter(|s| s.name == name);
        match (hits.next(), hits.next()) {
            (Some(s), None) => Some(s),
            _ => None,
        }
    }

    /// The last stream of that name (the full read's rule, like `Container::read_named`).
    fn last(&self, name: &str) -> Option<&Stream> {
        self.streams.iter().rev().find(|s| s.name == name)
    }
}

fn u32_le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64_le(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

/// (table offset, table length, fixed length) from the 64-byte header, with clear refusals.
fn parse_header(head: &[u8], file_len: u64) -> Result<(u64, u64, u64)> {
    if head.len() >= 8 && head[..8] == OLD_MAGIC {
        return err("예전 .takm(v0.3.0 이하의 마커만 든 파일)이에요. 지금 .takm과 모양이 달라 읽지 않아요. TAK에서 다시 만들어 주세요(--to-takm).");
    }
    if head.len() < HEADER_LEN as usize || head[..8] != MAGIC {
        return err(".takm 파일이 아니에요(파일 앞부분이 TAKM-2가 아님).");
    }
    let layout = u32_le(head, 8);
    if layout != LAYOUT {
        return err(format!("이 뷰어가 모르는 .takm 모양(번호 {layout})이에요. 더 새 뷰어로 여세요."));
    }
    let (table_off, table_len, fixed_len) = (u64_le(head, 16), u64_le(head, 24), u64_le(head, 32));
    let ok = table_off >= HEADER_LEN
        && table_len <= MAX_TABLE
        && table_off.checked_add(table_len) == Some(fixed_len)
        && fixed_len.checked_add(SEAL_LEN).is_some_and(|e| e <= file_len);
    if !ok {
        return err(".takm 머리 정보가 맞지 않아요(잘렸거나 망가진 파일).");
    }
    Ok((table_off, table_len, fixed_len))
}

/// The stream table (JSON) -> `Index`, every stream inside [64, table offset).
fn parse_table(table: &[u8], table_off: u64, fixed_len: u64) -> Result<Index> {
    let bad = |what: &str| TakError(format!(".takm 표가 맞지 않아요({what})."));
    let v: Value = serde_json::from_slice(table).map_err(|_| bad("JSON"))?;
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| bad(k));
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).ok_or_else(|| bad(k));
    let list = |k: &str| v.get(k).and_then(Value::as_array).ok_or_else(|| bad(k));
    let mut streams = Vec::new();
    for s in list("streams")? {
        let st = Stream { name: text(s, "name")?, index: num(s, "index")? as usize, offset: num(s, "offset")?, size: num(s, "size")? };
        if st.offset < HEADER_LEN || st.offset.checked_add(st.size).is_none_or(|e| e > table_off) {
            return Err(bad(&format!("{} 자리", st.name)));
        }
        streams.push(st);
    }
    let mut dropped = Vec::new();
    for d in list("dropped")? {
        dropped.push(Dropped { name: text(d, "name")?, index: num(d, "index")? as usize, size: num(d, "size")?, sha256: text(d, "sha256")? });
    }
    let src = v.get("source").ok_or_else(|| bad("source"))?;
    Ok(Index {
        fixed_len,
        streams,
        dropped,
        source: Source { name: text(src, "name")?, size: num(src, "size")?, sha256: text(src, "sha256")? },
        made_by: text(&v, "made_by")?,
        made_at: text(&v, "made_at")?,
    })
}

/// Header and stream table of an open `.takm` of `file_len` bytes (two positional reads).
pub fn read_index(file: &File, file_len: u64) -> Result<Index> {
    let mut head = vec![0u8; HEADER_LEN.min(file_len) as usize];
    read_exact_at(file, &mut head, 0)?;
    let (table_off, table_len, fixed_len) = parse_header(&head, file_len)?;
    let mut table = vec![0u8; table_len as usize];
    read_exact_at(file, &mut table, table_off)?;
    parse_table(&table, table_off, fixed_len)
}

/// `read_index` of a path.
pub fn read_index_path(path: &Path) -> Result<Index> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    read_index(&file, len)
}

/// Full read: the same `Take` as `tak::read_take` of the source TAK (path and `file_size` are
/// the `.takm`'s). Later streams of the same name win, as in the TAK read.
pub fn read_take(path: &Path) -> Result<Take> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let index = read_index(&file, len)?;
    let get = |name: &str| -> Result<Vec<u8>> {
        let s = index.last(name).ok_or_else(|| TakError("Required marker streams missing".into()))?;
        let mut buf = vec![0u8; s.size as usize];
        read_exact_at(&file, &mut buf, s.offset)?;
        Ok(buf)
    };
    let (nodes, channels, meta) = (get("Nodes.dat")?, get("Channels.dat")?, get("MetaData.dat")?);
    if std::fs::metadata(path)?.len() < index.tail_start() {
        return err(".takm became shorter than its fixed part while reading");
    }
    crate::tak::decode_streams(path, len as usize, &nodes, &channels, &meta)
}

// ───────────────────────────── Writing (conversion) ─────────────────────────────

/// Writer that hashes what goes through it.
struct Sealing<W: Write> {
    w: W,
    hash: Sha256,
    written: u64,
}

impl<W: Write> Sealing<W> {
    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.hash.update(b);
        self.written += b.len() as u64;
        self.w.write_all(b)
    }
}

/// Write a whole new `.takm` for the TAK bytes `tak` (named `source_name`): fixed part, its
/// SHA-256, and a tail holding only the Comment CSV header line. Returns the bytes written.
/// Peak memory: the TAK plus its largest stream.
pub fn write(w: &mut impl Write, tak: &[u8], source_name: &str, made_at: &str) -> Result<u64> {
    let c = Container::parse(tak)?;
    let list = c.streams()?;
    // Dropped streams first: their SHA-256 goes into the table.
    let mut dropped = Vec::new();
    let mut kept: Vec<(usize, &String, usize, u64)> = Vec::new();
    let mut offset = HEADER_LEN;
    for (i, (name, idx)) in list.iter().enumerate() {
        if name == CAMERA_STREAM {
            let raw = c.read_stream(*idx)?;
            dropped.push(json!({"name": name, "index": i, "size": raw.len(), "sha256": sha256_hex(&raw)}));
        } else {
            let size = c.entries[*idx].size;
            kept.push((i, name, *idx, offset));
            offset += size;
        }
    }
    let streams: Vec<Value> = kept
        .iter()
        .map(|&(i, name, idx, off)| json!({"name": name, "index": i, "offset": off, "size": c.entries[idx].size}))
        .collect();
    let table = serde_json::to_vec(&json!({
        "format": "takm",
        "layout": LAYOUT,
        "made_by": format!("tak_viewer {}", env!("CARGO_PKG_VERSION")),
        "made_at": made_at,
        "source": {"name": source_name, "size": tak.len(), "sha256": sha256_hex(tak)},
        "streams": streams,
        "dropped": dropped,
    }))
    .map_err(|e| TakError(format!("table: {e}")))?;
    let table_off = offset;
    let fixed_len = table_off + table.len() as u64;
    let mut head = [0u8; HEADER_LEN as usize];
    head[..8].copy_from_slice(&MAGIC);
    head[8..12].copy_from_slice(&LAYOUT.to_le_bytes());
    head[16..24].copy_from_slice(&table_off.to_le_bytes());
    head[24..32].copy_from_slice(&(table.len() as u64).to_le_bytes());
    head[32..40].copy_from_slice(&fixed_len.to_le_bytes());
    let mut s = Sealing { w, hash: Sha256::new(), written: 0 };
    s.put(&head)?;
    for &(_, _, idx, off) in &kept {
        let raw = c.read_stream(idx)?;
        if s.written != off || raw.len() as u64 != c.entries[idx].size {
            return err("stream size changed while writing");
        }
        s.put(&raw)?;
    }
    s.put(&table)?;
    if s.written != fixed_len {
        return err("fixed part length mismatch");
    }
    let seal = s.hash.finalize();
    s.w.write_all(&seal)?;
    let tail = comments::header_line();
    s.w.write_all(tail.as_bytes())?;
    Ok(fixed_len + SEAL_LEN + tail.len() as u64)
}

/// Why `out` may not be written for `tak` (plain Korean), None = it may: not inside a
/// repository's `input/` (same rule as captures and comment files), not over an existing file.
pub fn check_out(out: &Path, tak: &Path) -> std::result::Result<(), String> {
    let dir = out.parent().unwrap_or(out);
    let roots = [repo_root(tak), dir.ancestors().find(|a| a.exists()).and_then(repo_root)];
    for root in roots.into_iter().flatten() {
        if in_repo_input(out, &root) {
            return Err(format!("input/ 폴더 안에는 .takm을 만들지 않아요: {}. --out-dir로 다른 폴더를 정해 주세요.", out.display()));
        }
    }
    if is_take_path(out) {
        return Err("만들 파일 이름이 .tak으로 끝나요".into());
    }
    if out.exists() {
        return Err(format!(
            "이미 있어요: {}. Comment가 들어 있을 수 있어 덮어쓰지 않아요. 지우거나 --out-dir로 다른 폴더를 정해 주세요.",
            out.display()
        ));
    }
    Ok(())
}

/// `path` is inside a repository's `input/` (the repository found above it on disk). Looks at
/// the disk: call it off the drawing thread.
pub fn in_input(path: &Path) -> bool {
    path.parent().and_then(|d| d.ancestors().find(|a| a.exists())).and_then(repo_root).is_some_and(|root| in_repo_input(path, &root))
}

/// Why the viewer may not append Comments to the tail of `takm` (plain Korean), Ok when it may.
/// Looks at the disk: call it off the drawing thread.
pub fn check_tail(takm: &Path) -> std::result::Result<(), String> {
    if in_input(takm) {
        return Err("input/ 폴더 안 파일이라 Comment를 쓰지 않아요. 파일을 다른 폴더에 복사해서 여세요.".into());
    }
    Ok(())
}

/// What one conversion did.
#[derive(Clone, Debug)]
pub struct Converted {
    pub out: PathBuf,
    pub tak_size: u64,
    pub takm_size: u64,
    /// (name, size) of the streams left out.
    pub dropped: Vec<(String, u64)>,
    pub secs: f64,
    /// `<stem>.comments.csv` next to the TAK: its event lines were copied into the tail (the
    /// CSV is only read).
    pub csv_beside: Option<PathBuf>,
    /// Event lines copied from that CSV into the tail.
    pub imported: usize,
    /// Lines of that CSV left out (not events, or a cut-off last line).
    pub import_skipped: usize,
}

fn stat(p: &Path) -> io::Result<(u64, Option<SystemTime>)> {
    let m = std::fs::metadata(p)?;
    Ok((m.len(), m.modified().ok()))
}

/// Put the finished temporary file at `out` without ever replacing a file that is there.
fn place(tmp: &Path, out: &Path) -> std::result::Result<(), String> {
    let exists = || format!("이미 있어요: {}. 덮어쓰지 않아요.", out.display());
    match std::fs::hard_link(tmp, out) {
        Ok(()) => {
            let _ = std::fs::remove_file(tmp);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(exists()),
        // No hard links here (e.g. the WSL share): check, then rename.
        Err(_) if out.exists() => Err(exists()),
        Err(_) => std::fs::rename(tmp, out).map_err(|e| format!("이름을 바꾸지 못했어요: {e}")),
    }
}

/// TAK -> `<stem>.takm` next to it (or in `out_dir`). The TAK is only read (size and
/// modification time are checked after); the `.takm` is written under a temporary name and
/// renamed, never over an existing file, never inside a repository's `input/`.
pub fn convert(tak: &Path, out_dir: Option<&Path>) -> std::result::Result<Converted, String> {
    let t0 = Instant::now();
    if !is_take_path(tak) {
        return Err(".tak 파일이 아니에요".into());
    }
    let out = path_for(tak, out_dir);
    check_out(&out, tak)?;
    let before = stat(tak).map_err(|e| format!("TAK를 열지 못했어요: {e}"))?;
    // Comments made before (v0.14.0 – v0.23.x) in `<stem>.comments.csv` go into the tail, so
    // none is lost; the CSV is only read.
    let csv = comments::path_for(tak);
    let old = comments::read(&csv).map_err(|e| format!("옆의 Comment 파일을 읽지 못했어요: {} ({e})", csv.display()))?;
    let bytes = std::fs::read(tak).map_err(|e| format!("TAK를 읽지 못했어요: {e}"))?;
    if let Some(dir) = out_dir {
        std::fs::create_dir_all(dir).map_err(|e| format!("폴더를 만들지 못했어요: {} ({e})", dir.display()))?;
    }
    let name = tak.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = out.with_file_name(format!(".{}.{}.part", out.file_name().map(|s| s.to_string_lossy()).unwrap_or_default(), std::process::id()));
    let made_at = {
        let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        comments::iso_time(comments::civil_from_unix(secs), 0)
    };
    let written = (|| -> std::result::Result<u64, String> {
        let file = OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e| format!("임시 파일을 만들지 못했어요: {e}"))?;
        let mut w = BufWriter::with_capacity(4 << 20, file);
        let mut n = write(&mut w, &bytes, &name, &made_at).map_err(|e| format!("TAK를 읽지 못했어요: {e}"))?;
        for e in old.iter().flat_map(|p| &p.events) {
            let line = comments::encode(e);
            w.write_all(line.as_bytes()).map_err(|e| format!("쓰지 못했어요: {e}"))?;
            n += line.len() as u64;
        }
        let file = w.into_inner().map_err(|e| format!("쓰지 못했어요: {e}"))?;
        file.sync_all().map_err(|e| format!("쓰지 못했어요: {e}"))?;
        if stat(tak).ok() != Some(before) || bytes.len() as u64 != before.0 {
            return Err("읽는 동안 TAK가 바뀌었어요".into());
        }
        Ok(n)
    })()
    .and_then(|n| place(&tmp, &out).map(|()| n));
    let takm_size = match written {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let dropped = Container::parse(&bytes)
        .and_then(|c| {
            Ok(c.streams()?.into_iter().filter(|(n, _)| n == CAMERA_STREAM).map(|(n, i)| (n, c.entries[i].size)).collect())
        })
        .unwrap_or_default();
    Ok(Converted {
        out,
        tak_size: before.0,
        takm_size,
        dropped,
        secs: t0.elapsed().as_secs_f64(),
        imported: old.as_ref().map_or(0, |p| p.events.len()),
        import_skipped: old.as_ref().map_or(0, |p| p.skipped + usize::from(p.cut)),
        csv_beside: old.is_some().then_some(csv),
    })
}

/// Usage line of `--to-takm`.
pub const USAGE: &str = "사용법: tak_viewer --to-takm <a.tak> [<b.tak> …] [--out-dir <폴더>]";

/// `--to-takm` options after the flag: (TAK files, `--out-dir`).
pub fn parse_args(args: &[String]) -> std::result::Result<(Vec<PathBuf>, Option<PathBuf>), String> {
    let mut files = Vec::new();
    let mut out_dir = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out-dir" => {
                let Some(d) = it.next() else { return Err("--out-dir 뒤에 폴더가 없어요".into()) };
                if out_dir.replace(PathBuf::from(d)).is_some() {
                    return Err("--out-dir은 한 번만 써요".into());
                }
            }
            s if s.starts_with("--") => return Err(format!("모르는 옵션: {s}")),
            s => files.push(PathBuf::from(s)),
        }
    }
    if files.is_empty() {
        return Err("TAK 파일이 없어요".into());
    }
    Ok((files, out_dir))
}

fn mb(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1e6)
}

/// One plain line about a finished conversion.
pub fn report_line(tak: &Path, c: &Converted) -> String {
    let name = |p: &Path| p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let dropped: u64 = c.dropped.iter().map(|d| d.1).sum();
    let mut line = format!(
        "{} → {} · {} → {} ({:.0} %) · 뺀 카메라 기록 {} · {:.2}초",
        name(tak),
        c.out.display(),
        mb(c.tak_size),
        mb(c.takm_size),
        100.0 * c.takm_size as f64 / c.tak_size.max(1) as f64,
        mb(dropped),
        c.secs
    );
    if let Some(csv) = &c.csv_beside {
        line += &format!(" · 옆의 {}에서 Comment 줄 {}개를 넣었어요(CSV는 그대로)", name(csv), c.imported);
        if c.import_skipped > 0 {
            line += &format!(" · 못 읽은 줄 {}개는 뺐어요", c.import_skipped);
        }
    }
    line
}

/// `args` = `["--to-takm", files…, "--out-dir", dir]`. One line per file; exit code 0 = all
/// made, 1 = some failed, 2 = bad options.
pub fn run(args: &[String]) -> i32 {
    let (files, out_dir) = match parse_args(args.get(1..).unwrap_or(&[])) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return 2;
        }
    };
    let mut failed = 0;
    for tak in &files {
        match convert(tak, out_dir.as_deref()) {
            Ok(c) => println!("{}", report_line(tak, &c)),
            Err(e) => {
                failed += 1;
                eprintln!("{}: 만들지 못했어요: {e}", tak.display());
            }
        }
    }
    i32::from(failed > 0)
}

// ───────────────────────────── Comment tail ─────────────────────────────

/// The Comment tail's complete lines (line endings removed; the header line included) and
/// whether a cut-off last line (no line break) was left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tail {
    pub lines: Vec<String>,
    pub cut: bool,
}

/// (index, tail bytes) of a `.takm`.
fn tail_bytes(file: &File) -> Result<(Index, Vec<u8>)> {
    let len = file.metadata()?.len();
    let index = read_index(file, len)?;
    let mut tail = vec![0u8; (len - index.tail_start()) as usize];
    read_exact_at(file, &mut tail, index.tail_start())?;
    Ok((index, tail))
}

/// The tail as text (UTF-8; bad bytes replaced).
pub fn read_tail_text(path: &Path) -> Result<String> {
    let (_, tail) = tail_bytes(&File::open(path)?)?;
    Ok(String::from_utf8_lossy(&tail).into_owned())
}

/// The tail's lines; a last line without a line break is ignored (as in the CSV).
pub fn read_tail(path: &Path) -> Result<Tail> {
    let text = read_tail_text(path)?;
    let mut lines: Vec<String> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned()).collect();
    let rest = lines.pop().unwrap_or_default();
    Ok(Tail { lines, cut: !rest.is_empty() })
}

/// The tail read as a comment file (`comments::parse`: header, cut-off last line and bad lines
/// skipped).
pub fn read_comments(path: &Path) -> Result<comments::Parsed> {
    Ok(comments::parse(&read_tail_text(path)?))
}

/// Append lines (each without a line break; CRLF is added) to the tail. Only appends: the
/// fixed part is never touched. When the tail ends in a cut-off line, a line break (after a
/// closing quote when the cut was inside one) goes first, as in `comments::append`. Refuses a
/// file that is not a `.takm` and a repository's `input/`.
pub fn append_lines(path: &Path, lines: &[String]) -> Result<()> {
    if lines.iter().any(|l| l.contains(['\r', '\n'])) {
        return err("한 줄에 줄바꿈이 들어 있어요");
    }
    if let Some(root) = path.parent().and_then(|d| d.ancestors().find(|a| a.exists())).and_then(repo_root) {
        if in_repo_input(path, &root) {
            return err("input/ 폴더 안 파일에는 쓰지 않아요");
        }
    }
    let mut f = OpenOptions::new().read(true).append(true).open(path)?;
    let (_, tail) = tail_bytes(&f)?;
    let mut out = String::new();
    if tail.last().is_some_and(|&b| b != b'\n') {
        if comments::ends_in_quote(&String::from_utf8_lossy(&tail)) {
            out.push('"');
        }
        out += EOL;
    }
    for l in lines {
        out += l;
        out += EOL;
    }
    f.write_all(out.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// Append comment events (`comments::encode` lines) to the tail.
pub fn append_events(path: &Path, events: &[comments::Event]) -> Result<()> {
    let lines: Vec<String> = events.iter().map(|e| comments::encode(e).trim_end_matches(['\r', '\n']).to_owned()).collect();
    append_lines(path, &lines)
}

/// Check the fixed part against its SHA-256 (reads the whole fixed part).
pub fn verify_fixed(path: &Path) -> Result<()> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let index = read_index(&file, len)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 4 << 20];
    let mut pos = 0u64;
    while pos < index.fixed_len {
        let n = (index.fixed_len - pos).min(buf.len() as u64) as usize;
        read_exact_at(&file, &mut buf[..n], pos)?;
        hash.update(&buf[..n]);
        pos += n as u64;
    }
    let mut seal = [0u8; SEAL_LEN as usize];
    read_exact_at(&file, &mut seal, index.fixed_len)?;
    if hash.finalize().as_slice() != seal {
        return err(".takm 고정 부분이 바뀌었어요(SHA-256이 달라요).");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tak::fast_read::read_take_fast;
    use crate::view_data::ViewData;

    use crate::sample_tak::sample_tak;


    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let d = std::env::temp_dir().join(format!("takm_test_{}_{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// (dir, tak path, takm path) after a conversion.
    fn converted(tag: &str) -> (TempDir, PathBuf, PathBuf) {
        let d = TempDir::new(tag);
        let tak = d.0.join("walk_001.tak");
        std::fs::write(&tak, sample_tak()).unwrap();
        let c = convert(&tak, None).unwrap();
        assert_eq!(c.out, d.0.join("walk_001.takm"));
        (d, tak, c.out)
    }

    fn file_sha(p: &Path) -> String {
        sha256_hex(&std::fs::read(p).unwrap())
    }

    #[test]
    fn the_synthetic_tak_reads_through_the_fast_path() {
        let d = TempDir::new("fast");
        let tak = d.0.join("a.tak");
        std::fs::write(&tak, sample_tak()).unwrap();
        let f = read_take_fast(&tak, false).unwrap();
        assert_eq!(f.times[0].0, "container");
        assert_eq!(f.take.trajectories.len(), 3);
        let (analog, _) = f.analog.unwrap().run().unwrap();
        assert_eq!(analog.len(), 1);
        assert_eq!(analog[0].channels.len(), 2);
    }

    #[test]
    fn conversion_keeps_every_stream_but_the_camera_byte_for_byte_and_leaves_the_tak_alone() {
        let d = TempDir::new("keep");
        let tak = d.0.join("walk_001.tak");
        let bytes = sample_tak();
        std::fs::write(&tak, &bytes).unwrap();
        let before = (stat(&tak).unwrap(), file_sha(&tak));
        let c = convert(&tak, None).unwrap();
        assert_eq!((stat(&tak).unwrap(), file_sha(&tak)), before);
        assert_eq!(c.dropped, vec![(CAMERA_STREAM.to_string(), 20_000)]);
        assert!(c.csv_beside.is_none());
        let takm = std::fs::read(&c.out).unwrap();
        assert_eq!(takm.len() as u64, c.takm_size);
        let index = read_index_path(&c.out).unwrap();
        let container = Container::parse(&bytes).unwrap();
        let all = container.streams().unwrap();
        assert_eq!(all.len(), 6);
        let names: Vec<&str> = index.streams.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Nodes.dat", "DeviceData/Track 1.trk", "Channels.dat", "MetaData.dat", "Calibration.dat"]);
        for s in &index.streams {
            let (name, i) = &all[s.index];
            assert_eq!(name, &s.name);
            let raw = container.read_stream(*i).unwrap();
            assert_eq!(&takm[s.offset as usize..(s.offset + s.size) as usize], &raw[..], "{name}");
        }
        assert_eq!(index.dropped.len(), 1);
        let cam = &index.dropped[0];
        let raw = container.read_stream(all[cam.index].1).unwrap();
        assert_eq!((cam.name.as_str(), cam.index, cam.size, cam.sha256.clone()), (CAMERA_STREAM, 1, 20_000, sha256_hex(&raw)));
        assert!(takm.windows(64).all(|w| w != &raw[1000..1064]), "camera bytes left in the .takm");
        assert_eq!(index.source, Source { name: "walk_001.tak".into(), size: bytes.len() as u64, sha256: sha256_hex(&bytes) });
        assert!(index.made_by.starts_with("tak_viewer "));
        // Fixed part sealed, tail = the header line only.
        assert_eq!(&takm[..8], b"TAKM-2\r\n");
        let f = index.fixed_len as usize;
        assert_eq!(&takm[f..f + 32], &Sha256::digest(&takm[..f])[..]);
        assert_eq!(&takm[f + 32..], comments::header_line().as_bytes());
        verify_fixed(&c.out).unwrap();
        assert_eq!(read_tail(&c.out).unwrap(), Tail { lines: vec![comments::COLUMNS.join(",")], cut: false });
        assert!(read_comments(&c.out).unwrap().events.is_empty());
    }

    fn same_view(a: Take, b: Take) {
        let (a, mut b) = (a, b);
        b.path = a.path.clone();
        b.file_size = a.file_size;
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
        let (va, vb) = (ViewData::build(a), ViewData::build(b));
        let key = |v: &ViewData| {
            format!(
                "{:?}",
                (&v.take, &v.markers, &v.groups, &v.sticks, &v.plates, &v.devices, v.unit_scale, v.bounds, v.start_frame, v.end_frame, &v.occlusion)
            )
        };
        assert_eq!(key(&va), key(&vb));
    }

    fn fast(path: &Path) -> (Take, Take) {
        let f = read_take_fast(path, true).unwrap();
        assert_eq!(f.times[0].0, "container", "fell back to the full read: {path:?}");
        let markers = f.take.clone();
        let mut take = f.take;
        let (analog, warning) = f.analog.unwrap().run().unwrap();
        take.analog = analog;
        take.warnings.extend(warning);
        (markers, take)
    }

    #[test]
    fn a_takm_gives_the_same_take_and_view_data_as_its_tak_full_and_fast() {
        let (_d, tak, takm) = converted("same");
        let full_tak = crate::tak::read_take(&tak).unwrap();
        let full_takm = crate::tak::read_take(&takm).unwrap();
        assert_eq!(full_takm.path, takm);
        assert_eq!(full_takm.file_size, std::fs::metadata(&takm).unwrap().len());
        assert_eq!(full_tak.trajectories.len(), 3);
        assert_eq!(full_tak.analog.len(), 1);
        let gap = &full_tak.trajectories[1];
        assert_eq!(crate::view_data::gaps(&gap.frames, 0, 299), vec![(100, 139)]);
        same_view(full_tak.clone(), full_takm.clone());
        let (m_tak, fast_tak) = fast(&tak);
        let (m_takm, fast_takm) = fast(&takm);
        same_view(m_tak, m_takm);
        same_view(fast_tak.clone(), fast_takm.clone());
        same_view(full_tak, fast_takm);
    }

    #[test]
    fn verify_finds_a_flipped_byte_in_the_fixed_part_but_not_in_the_tail() {
        let (_d, _tak, takm) = converted("flip");
        let good = std::fs::read(&takm).unwrap();
        let index = read_index_path(&takm).unwrap();
        let stream = &index.streams[2];
        for at in [9usize, (stream.offset + 5) as usize, index.fixed_len as usize - 3, index.fixed_len as usize + 4] {
            let mut b = good.clone();
            b[at] ^= 0x01;
            std::fs::write(&takm, &b).unwrap();
            assert!(verify_fixed(&takm).is_err(), "byte {at} flipped, not found");
        }
        let mut b = good.clone();
        let last = b.len() - 3;
        b[last] ^= 0x01;
        std::fs::write(&takm, &b).unwrap();
        verify_fixed(&takm).unwrap();
    }

    fn event(id: u64, text: &str) -> comments::Event {
        comments::Event {
            time: "2026-11-14T10:00:00+09:00".into(),
            kind: comments::Kind::Add,
            id,
            markers: vec!["LKNE".into()],
            start: 10,
            end: 20 + id as i64,
            comment_type: "Jitter".into(),
            text: text.into(),
            shown_a: "1".into(),
            shown_b: String::new(),
            file_a: "walk_001.takm|1".into(),
            file_b: String::new(),
        }
    }

    #[test]
    fn the_tail_takes_appended_lines_and_ignores_a_cut_off_last_line_and_the_fixed_part_stays() {
        let (_d, _tak, takm) = converted("tail");
        let index = read_index_path(&takm).unwrap();
        let fixed = |p: &Path| sha256_hex(&std::fs::read(p).unwrap()[..index.tail_start() as usize]);
        let before = fixed(&takm);
        let header = comments::COLUMNS.join(",");
        // Empty (header only), then several lines.
        append_lines(&takm, &[]).unwrap();
        assert_eq!(read_tail(&takm).unwrap().lines, vec![header.clone()]);
        append_events(&takm, &[event(1, "a, \"b\""), event(2, "둘")]).unwrap();
        append_lines(&takm, &["x,y".to_string()]).unwrap();
        let t = read_tail(&takm).unwrap();
        assert_eq!(t.lines.len(), 4);
        assert_eq!(t.lines[3], "x,y");
        assert!(!t.cut);
        let parsed = read_comments(&takm).unwrap();
        assert_eq!(parsed.events, vec![event(1, "a, \"b\""), event(2, "둘")]);
        assert_eq!(parsed.skipped, 1);
        // A line cut off inside a quote: ignored when read; the next append stands alone.
        OpenOptions::new().append(true).open(&takm).unwrap().write_all(b"2026,add,3,\"LK").unwrap();
        let t = read_tail(&takm).unwrap();
        assert_eq!((t.lines.len(), t.cut), (4, true));
        let p = read_comments(&takm).unwrap();
        assert!(p.cut);
        assert_eq!(p.events.len(), 2);
        append_events(&takm, &[event(4, "")]).unwrap();
        let p = read_comments(&takm).unwrap();
        assert_eq!(p.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2, 4]);
        assert!(!read_tail(&takm).unwrap().cut);
        // The fixed part did not change.
        assert_eq!(fixed(&takm), before);
        verify_fixed(&takm).unwrap();
        assert!(append_lines(&takm, &["a\r\nb".into()]).is_err());
        // The markers still read (the file grew).
        assert_eq!(crate::tak::read_take(&takm).unwrap().trajectories.len(), 3);
        assert_eq!(fast(&takm).1.analog.len(), 1);
    }

    #[test]
    fn a_takm_that_is_there_is_never_overwritten() {
        let (d, tak, takm) = converted("again");
        append_lines(&takm, &["kept".into()]).unwrap();
        let before = std::fs::read(&takm).unwrap();
        let e = convert(&tak, None).unwrap_err();
        assert!(e.contains("이미 있어요") && e.contains("덮어쓰지 않아요"), "{e}");
        assert_eq!(std::fs::read(&takm).unwrap(), before);
        let names: Vec<String> = std::fs::read_dir(&d.0).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        // Into another folder it works; there it is made once too.
        let other = d.0.join("out");
        assert_eq!(convert(&tak, Some(&other)).unwrap().out, other.join("walk_001.takm"));
        assert!(convert(&tak, Some(&other)).is_err());
    }

    #[test]
    fn nothing_is_written_under_a_repository_input_folder() {
        let d = TempDir::new("input");
        let repo = d.0.join("repo");
        std::fs::create_dir_all(repo.join("apps")).unwrap();
        std::fs::write(repo.join("AGENTS.md"), "").unwrap();
        std::fs::write(repo.join("apps").join("AGENTS.md"), "").unwrap();
        let raw = repo.join("input").join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        let tak = raw.join("a.tak");
        std::fs::write(&tak, sample_tak()).unwrap();
        let e = convert(&tak, None).unwrap_err();
        assert!(e.contains("input/"), "{e}");
        assert!(!raw.join("a.takm").exists());
        // Out of input/ with --out-dir: fine. Into input/ from outside: refused.
        let out = repo.join("experiments");
        let made = convert(&tak, Some(&out)).unwrap().out;
        let outside = d.0.join("b.tak");
        std::fs::write(&outside, sample_tak()).unwrap();
        assert!(convert(&outside, Some(&repo.join("input").join("new"))).unwrap_err().contains("input/"));
        assert!(!repo.join("input").join("new").exists());
        // Appending to a .takm inside input/ is refused too.
        let inside = raw.join("c.takm");
        std::fs::copy(&made, &inside).unwrap();
        assert!(append_lines(&inside, &["x".into()]).is_err());
        assert_eq!(std::fs::read(&inside).unwrap(), std::fs::read(&made).unwrap());
    }

    #[test]
    fn old_foreign_and_broken_files_are_refused_clearly() {
        let (d, tak, takm) = converted("refuse");
        let check = |bytes: &[u8], want: &str| {
            let p = d.0.join("x.takm");
            std::fs::write(&p, bytes).unwrap();
            let e = crate::tak::read_take(&p).unwrap_err().0;
            assert!(e.contains(want), "{want}: {e}");
            let e = read_take_fast(&p, false).err().map(|e| e.0).unwrap_or_default();
            assert!(e.contains(want), "fast {want}: {e}");
            assert!(verify_fixed(&p).is_err() && read_tail(&p).is_err() && append_lines(&p, &[]).is_err());
        };
        let mut old = b"TAKM\r\n\x1a\n".to_vec();
        old.extend([0u8; 200]);
        check(&old, "예전 .takm");
        check(&std::fs::read(&tak).unwrap(), ".takm 파일이 아니에요");
        check(b"short", ".takm 파일이 아니에요");
        let good = std::fs::read(&takm).unwrap();
        let mut b = good.clone();
        b[8] = 2;
        check(&b, "모르는 .takm 모양(번호 2)");
        let f = read_index_path(&takm).unwrap().fixed_len as usize;
        check(&good[..f + 10], "머리 정보가 맞지 않아요");
        let mut b = good.clone();
        let t = u64_le(&good, 16) as usize;
        b[t] = b'[';
        check(&b, "표가 맞지 않아요");
        // A TAK path is still read as a TAK, a .takm renamed .tak is not a TAK.
        let as_tak = d.0.join("y.tak");
        std::fs::write(&as_tak, &good).unwrap();
        assert!(crate::tak::read_take(&as_tak).is_err());
        assert!(convert(&takm, None).unwrap_err().contains(".tak 파일이 아니에요"));
    }

    #[test]
    fn options_and_paths() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_args(&s(&["a.tak"])).unwrap(), (vec![PathBuf::from("a.tak")], None));
        assert_eq!(
            parse_args(&s(&["a.tak", "--out-dir", "o", "b.tak"])).unwrap(),
            (vec![PathBuf::from("a.tak"), PathBuf::from("b.tak")], Some(PathBuf::from("o")))
        );
        assert!(parse_args(&s(&[])).is_err());
        assert!(parse_args(&s(&["--out-dir", "o"])).is_err());
        assert!(parse_args(&s(&["a.tak", "--out-dir"])).is_err());
        assert!(parse_args(&s(&["a.tak", "--out-dir", "o", "--out-dir", "p"])).is_err());
        assert!(parse_args(&s(&["a.tak", "--force"])).is_err());
        assert_eq!(run(&s(&["--to-takm"])), 2);
        assert_eq!(run(&s(&["--to-takm", "/no/such/file.tak"])), 1);
        assert!(is_takm_path(Path::new("a.TAKM")) && is_takm_path(Path::new("d/b.takm")));
        assert!(!is_takm_path(Path::new("a.tak")) && !is_takm_path(Path::new("a.takm.part")));
        assert_eq!(path_for(Path::new("d/a.b.tak"), None), Path::new("d").join("a.b.takm"));
        assert_eq!(path_for(Path::new("d/a.tak"), Some(Path::new("o"))), Path::new("o").join("a.takm"));
    }

    #[test]
    fn the_report_line_names_sizes_and_a_comment_file_left_beside() {
        let (d, tak, _takm) = converted("line");
        std::fs::write(d.0.join("walk_001.comments.csv"), "x").unwrap();
        let c = convert(&tak, Some(&d.0.join("o"))).unwrap();
        assert_eq!(c.csv_beside, Some(d.0.join("walk_001.comments.csv")));
        let line = report_line(&tak, &c);
        assert!(line.starts_with("walk_001.tak → ") && line.contains("뺀 카메라 기록 0.0 MB"), "{line}");
        assert!(line.contains("walk_001.comments.csv에서 Comment 줄 0개를 넣었어요(CSV는 그대로) · 못 읽은 줄 1개는 뺐어요"), "{line}");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn conversion_puts_the_complete_lines_of_the_comment_csv_into_the_tail_and_leaves_the_csv_alone() {
        let d = TempDir::new("import");
        let tak = d.0.join("walk_001.tak");
        std::fs::write(&tak, sample_tak()).unwrap();
        let csv = d.0.join("walk_001.comments.csv");
        let (one, two) = (event(1, "first, \"quoted\""), event(2, "두 번째"));
        comments::append(&csv, &[one.clone(), two.clone()]).unwrap();
        // A line cut off in the middle (a crash while writing): left out.
        let mut f = OpenOptions::new().append(true).open(&csv).unwrap();
        f.write_all(b"2026-11-14T10:00:00+09:00,add,3,M,1,").unwrap();
        drop(f);
        let csv_before = file_sha(&csv);
        let c = convert(&tak, None).unwrap();
        assert_eq!((c.imported, c.import_skipped, c.csv_beside.clone()), (2, 1, Some(csv.clone())));
        assert_eq!(file_sha(&csv), csv_before, "the CSV is only read");
        assert_eq!(std::fs::metadata(&c.out).unwrap().len(), c.takm_size);
        verify_fixed(&c.out).unwrap();
        let tail = read_tail(&c.out).unwrap();
        assert!(!tail.cut);
        assert_eq!(tail.lines.len(), 3, "the header once (no BOM), then the two events: {:?}", tail.lines);
        assert_eq!(tail.lines[0], comments::header_line().trim_end());
        assert_eq!(read_comments(&c.out).unwrap().events, vec![one, two]);
        let line = report_line(&tak, &c);
        assert!(line.contains("Comment 줄 2개를 넣었어요") && line.contains("못 읽은 줄 1개"), "{line}");
    }
}
