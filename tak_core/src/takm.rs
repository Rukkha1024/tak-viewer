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
//! Conversion writes the fixed part once; a marker save replaces it after validation and
//! recomputes its seal. Comment appends and marker saves share a per-path OS lock.
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

#[path = "takm_verify.rs"]
pub mod verify;

#[path = "takm_progress.rs"]
pub(crate) mod progress;

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
const IO_CHUNK: usize = 2 << 20;

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
    header_bounds(head, file_len)
}

fn header_bounds(head: &[u8], file_len: u64) -> Result<(u64, u64, u64)> {
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
fn decode_table(v: &Value, table_off: u64, fixed_len: u64) -> Result<Index> {
    let bad = |what: &str| TakError(format!(".takm 표가 맞지 않아요({what})."));
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| bad(k));
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).ok_or_else(|| bad(k));
    let list = |k: &str| v.get(k).and_then(Value::as_array).ok_or_else(|| bad(k));
    let mut streams = Vec::new();
    for s in list("streams")? {
        let index = usize::try_from(num(s, "index")?).map_err(|_| bad("index"))?;
        let st = Stream { name: text(s, "name")?, index, offset: num(s, "offset")?, size: num(s, "size")? };
        if st.offset < HEADER_LEN || st.offset.checked_add(st.size).is_none_or(|e| e > table_off) {
            return Err(bad(&format!("{} 자리", st.name)));
        }
        streams.push(st);
    }
    let mut dropped = Vec::new();
    for d in list("dropped")? {
        let index = usize::try_from(num(d, "index")?).map_err(|_| bad("index"))?;
        dropped.push(Dropped { name: text(d, "name")?, index, size: num(d, "size")?, sha256: text(d, "sha256")? });
    }
    let src = v.get("source").ok_or_else(|| bad("source"))?;
    Ok(Index {
        fixed_len,
        streams,
        dropped,
        source: Source { name: text(src, "name")?, size: num(src, "size")?, sha256: text(src, "sha256")? },
        made_by: text(v, "made_by")?,
        made_at: text(v, "made_at")?,
    })
}

fn read_table(file: &File, table_off: u64, table_len: u64) -> Result<Value> {
    read_table_at(table_off, table_len, |buf, offset| read_exact_at(file, buf, offset))
}

fn read_table_at(table_off: u64, table_len: u64, read: impl FnOnce(&mut [u8], u64) -> Result<()>) -> Result<Value> {
    let mut table = vec![0u8; table_len as usize];
    read(&mut table, table_off)?;
    serde_json::from_slice(&table).map_err(|_| TakError(".takm 표가 맞지 않아요(JSON).".into()))
}

/// Header and stream table of an open `.takm` of `file_len` bytes (two positional reads).
pub fn read_index(file: &File, file_len: u64) -> Result<Index> {
    read_index_at(file_len, MAX_TABLE, |buf, offset| read_exact_at(file, buf, offset))
}

pub(crate) fn read_index_at(file_len: u64, table_limit: u64, mut read: impl FnMut(&mut [u8], u64) -> Result<()>) -> Result<Index> {
    let mut head = vec![0u8; HEADER_LEN.min(file_len) as usize];
    read(&mut head, 0)?;
    let (table_off, table_len, fixed_len) = parse_header(&head, file_len)?;
    if table_len > table_limit {
        return err(".takm stream table exceeds read budget");
    }
    decode_table(&read_table_at(table_off, table_len, read)?, table_off, fixed_len)
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
    let source = Source { name: source_name.into(), size: tak.len() as u64, sha256: sha256_hex(tak) };
    write_streams(w, list.iter().map(|(name, entry)| (name.as_str(), c.entries[*entry].size)), &source, made_at,
        |i, consume| consume(&c.read_stream(list[i].1)?))
}

fn write_streams<'a>(
    w: &mut impl Write,
    list: impl Iterator<Item = (&'a str, u64)>,
    source: &Source,
    made_at: &str,
    mut visit: impl FnMut(usize, &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()>,
) -> Result<u64> {
    // Dropped streams first: their SHA-256 goes into the table.
    let mut dropped = Vec::new();
    let mut kept = Vec::new();
    let mut offset = HEADER_LEN;
    for (i, (name, size)) in list.enumerate() {
        if name == CAMERA_STREAM {
            let mut hash = Sha256::new();
            visit(i, &mut |chunk| { hash.update(chunk); Ok(()) })?;
            dropped.push(json!({"name": name, "index": i, "size": size, "sha256": format!("{:x}", hash.finalize())}));
        } else {
            kept.push((i, name, offset, size));
            offset = offset.checked_add(size).ok_or_else(|| TakError("TAKM 크기가 너무 커요.".into()))?;
        }
    }
    let streams: Vec<Value> = kept
        .iter()
        .map(|&(i, name, off, size)| json!({"name": name, "index": i, "offset": off, "size": size}))
        .collect();
    let table = serde_json::to_vec(&json!({
        "format": "takm",
        "layout": LAYOUT,
        "made_by": format!("tak_viewer {}", env!("CARGO_PKG_VERSION")),
        "made_at": made_at,
        "source": {"name": source.name, "size": source.size, "sha256": source.sha256},
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
    for &(idx, _, off, size) in &kept {
        visit(idx, &mut |chunk| Ok(s.put(chunk)?))?;
        if s.written != off + size {
            return err("stream size changed while writing");
        }
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
    if in_input(out) {
        return Err(format!("input/ 폴더 안에는 .takm을 만들지 않아요: {}. --out-dir로 다른 폴더를 정해 주세요.", out.display()));
    }
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
    fn under_input(p: &Path) -> bool {
        p.parent().and_then(|d| d.ancestors().find(|a| a.exists())).and_then(repo_root)
            .is_some_and(|root| in_repo_input(p, &root))
    }
    // Resolve directory and file symlinks before checking; also retain the lexical check.
    under_input(path) || match canonical_target(path) { Ok(p) => under_input(&p), Err(_) => true }
}

/// Resolve symlinks through the nearest existing ancestor, including missing descendants.
pub fn canonical_target(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    let mut current = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(current) {
            Ok(mut base) => {
                for part in suffix.iter().rev() {
                    if part == ".." { base.pop(); }
                    else if part != "." { base.push(part); }
                }
                return Ok(base);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let name = current.file_name().ok_or(e)?;
                suffix.push(name.to_os_string());
                current = current.parent().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing ancestor"))?;
            }
            Err(e) => return Err(e),
        }
    }
}

/// The stable lock file is intentionally retained: removing it can split waiting processes
/// across different inodes. Its key is the canonical destination, shared by app instances.
pub(crate) fn coordination_root() -> PathBuf {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(root) = crate::test_support::coordination_root_override() { return root; }
    #[cfg(unix)]
    { PathBuf::from("/tmp") }
    #[cfg(windows)]
    { std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir) }
    #[cfg(not(any(unix, windows)))]
    { std::env::temp_dir() }
}

pub(crate) fn path_lock_key(path: &Path) -> io::Result<String> {
    let target = canonical_target(path)?;
    #[cfg(windows)]
    let key = target.to_string_lossy().to_lowercase().into_bytes();
    #[cfg(not(windows))]
    let key = target.as_os_str().as_encoded_bytes().to_vec();
    Ok(format!("{:x}", Sha256::digest(key)))
}

pub(crate) fn lock_path(path: &Path) -> io::Result<File> {
    let target = canonical_target(path)?;
    if in_input(&target) { return Err(io::Error::new(io::ErrorKind::PermissionDenied, "input/ 보호")); }
    let lock = coordination_root().join(format!(".tak_viewer_{}.lock", path_lock_key(&target)?));
    let f = OpenOptions::new().read(true).write(true).create(true).open(lock)?;
    f.lock()?;
    Ok(f)
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

#[cfg(not(windows))]
pub(crate) fn place_new(tmp: &Path, dest: &Path) -> std::io::Result<()> { std::fs::hard_link(tmp, dest) }
#[cfg(windows)]
pub(crate) fn place_new(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    // MoveFileW is a same-volume move that fails if the destination exists. This supports
    // Windows filesystems without hard links while preserving create-if-absent semantics.
    #[link(name = "kernel32")]
    unsafe extern "system" { fn MoveFileW(from: *const u16, to: *const u16) -> i32; }
    // Rust fs accepts long local/UNC paths, while raw Win32 calls require the extended
    // path prefix. Resolve the missing destination through its existing parent too.
    let from_path = std::fs::canonicalize(tmp)?;
    let to_path = canonical_target(dest)?;
    let from: Vec<u16> = from_path.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to_path.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileW(from.as_ptr(), to.as_ptr()) } != 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}
/// Put the finished temporary file at `out` without ever replacing a file that is there.
fn place(tmp: &Path, out: &Path) -> std::result::Result<(), String> {
    let exists = || format!("이미 있어요: {}. 덮어쓰지 않아요.", out.display());
    match place_new(tmp, out) {
        Ok(()) => {
            let _ = std::fs::remove_file(tmp);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(exists()),
        // A check followed by rename can overwrite a file created between the two calls.
        Err(e) => Err(format!("덮어쓰지 않는 저장을 할 수 없어요: {e}")),
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
    if let Some(dir) = out_dir {
        std::fs::create_dir_all(dir).map_err(|e| format!("폴더를 만들지 못했어요: {} ({e})", dir.display()))?;
    }
    comments::drain_shared_writers(&[&out, &csv]).map_err(|e| format!("Comment 저장을 기다리지 못했어요: {e}"))?;
    let mut paths = vec![canonical_target(&out).map_err(|e| e.to_string())?];
    if !in_input(&csv) { paths.push(canonical_target(&csv).map_err(|e| e.to_string())?); }
    paths.sort(); paths.dedup();
    let _locks: Vec<_> = paths.iter().map(|p| lock_path(p)).collect::<io::Result<_>>()
        .map_err(|e| format!("저장 잠금을 얻지 못했어요: {e}"))?;
    check_out(&out, tak)?;
    let old = comments::read(&csv).map_err(|e| format!("옆의 Comment 파일을 읽지 못했어요: {} ({e})", csv.display()))?;
    let input = File::open(tak).map_err(|e| format!("TAK를 읽지 못했어요: {e}"))?;
    let list = Container::file_streams(&input, before.0).map_err(|e| format!("TAK를 읽지 못했어요: {e}"))?;
    let mut buf = vec![0; IO_CHUNK];
    let digest = hash_file_prefix(&input, before.0, &mut buf).map_err(|e| format!("TAK를 읽지 못했어요: {e}"))?;
    let name = tak.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let source = Source { name, size: before.0, sha256: format!("{digest:x}") };
    let tmp = out.with_file_name(format!(".{}.{}.part", out.file_name().map(|s| s.to_string_lossy()).unwrap_or_default(), std::process::id()));
    let made_at = {
        let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        comments::iso_time(comments::civil_from_unix(secs), 0)
    };
    let written = (|| -> std::result::Result<u64, String> {
        let file = OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e| format!("임시 파일을 만들지 못했어요: {e}"))?;
        let mut w = BufWriter::with_capacity(4 << 20, file);
        let mut n = write_streams(&mut w, list.iter().map(|(name, map)| (name.as_str(), map.len)), &source, &made_at,
            |i, consume| list[i].1.visit(&input, &mut buf, consume))
            .map_err(|e| format!("TAKM을 만들지 못했어요: {e}"))?;
        for e in old.iter().flat_map(|p| &p.events) {
            let line = comments::encode(e);
            w.write_all(line.as_bytes()).map_err(|e| format!("쓰지 못했어요: {e}"))?;
            n += line.len() as u64;
        }
        let file = w.into_inner().map_err(|e| format!("쓰지 못했어요: {e}"))?;
        file.sync_all().map_err(|e| format!("쓰지 못했어요: {e}"))?;
        if stat(tak).ok() != Some(before) {
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
    let dropped = list.into_iter().filter(|(name, _)| name == CAMERA_STREAM).map(|(name, map)| (name, map.len)).collect();
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
pub const USAGE: &str = "사용법: tak_viewer --to-takm <a.tak> [<b.tak> …] [--out-dir <폴더>] [--verify]";

/// `--to-takm` options after the flag: (TAK files, `--out-dir`, `--verify`).
pub fn parse_args(args: &[String]) -> std::result::Result<(Vec<PathBuf>, Option<PathBuf>, bool), String> {
    let mut files = Vec::new();
    let mut out_dir = None;
    let mut verify = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out-dir" => {
                let Some(d) = it.next().filter(|s| !s.is_empty() && !s.starts_with("--")) else { return Err("--out-dir 뒤에 폴더가 없어요".into()) };
                if out_dir.replace(PathBuf::from(d)).is_some() {
                    return Err("--out-dir은 한 번만 써요".into());
                }
            }
            "--verify" => {
                if verify { return Err("--verify는 한 번만 써요".into()); }
                verify = true;
            }
            s if s.starts_with("--") => return Err(format!("모르는 옵션: {s}")),
            s => files.push(PathBuf::from(s)),
        }
    }
    if files.is_empty() {
        return Err("TAK 파일이 없어요".into());
    }
    Ok((files, out_dir, verify))
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
    let (files, out_dir, verify) = match parse_args(args.get(1..).unwrap_or(&[])) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return 2;
        }
    };
    let mut failed = 0;
    for tak in &files {
        match convert(tak, out_dir.as_deref()) {
            Ok(c) => {
                let line = report_line(tak, &c);
                if verify {
                    let result = verify::verify(&c.out, Some(tak));
                    println!("{line} · 확인 {}", result.verdict.as_str());
                    if result.verdict == verify::Verdict::Failed {
                        failed += 1;
                        eprintln!("{}: {}", c.out.display(), result.failure_details());
                    }
                } else {
                    println!("{line}");
                }
            }
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
    read_comments_with_health(path).map(|(parsed, _)| parsed)
}

pub(crate) fn read_comments_with_health(path: &Path) -> Result<(comments::Parsed, Option<&'static str>)> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let index = read_index(&file, len)?;
    read_comments_at(&file, len, index.tail_start())
}

fn read_comments_at(file: &File, len: u64, start: u64) -> Result<(comments::Parsed, Option<&'static str>)> {
    let size = usize::try_from(len - start).map_err(|_| TakError("Comment 영역이 너무 커요.".into()))?;
    let mut tail = Vec::new();
    tail.try_reserve_exact(size).map_err(|_| TakError("Comment 영역을 읽을 메모리가 부족해요.".into()))?;
    tail.resize(size, 0);
    read_exact_at(file, &mut tail, start)?;
    let text = String::from_utf8_lossy(&tail);
    let has_header = text.split_once('\n').is_some_and(|(line, _)| {
        line.strip_suffix('\r').unwrap_or(line) == comments::header_line().trim_end()
    });
    let damage = if !has_header {
        Some("Comment 머리글이 없거나 맞지 않아요.")
    } else if matches!(text, std::borrow::Cow::Owned(_)) {
        Some("Comment 꼬리에 잘못된 UTF-8 글자가 있어요.")
    } else {
        None
    };
    Ok((comments::parse(&text), damage))
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
    let target = canonical_target(path)?;
    if in_input(&target) || !is_takm_path(&target) { return err("안전하지 않은 Comment 대상이에요"); }
    let _lock = lock_path(&target)?;
    let mut f = OpenOptions::new().read(true).append(true).open(&target)?;
    let (_, tail) = tail_bytes(&f)?;
    let mut out = String::new();
    if tail.last().is_some_and(|&b| b != b'\n') {
        if comments::ends_in_quote(&String::from_utf8_lossy(&tail)) {
            out.push('"');
        }
        out += comments::INCOMPLETE_SUFFIX;
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
#[derive(Clone, Debug)]
pub struct CommentConflict {
    pub id: u64,
    pub latest: Option<comments::Comment>,
}

pub fn append_events(path: &Path, events: &[comments::Event]) -> Result<()> {
    if append_events_guarded(path, events, None)?.is_some() {
        return err("Comment changed in another window");
    }
    Ok(())
}

/// Compare the view's base under the same lock as the append. Cancellation is checked after
/// lock acquisition, so a discarded write waiting on a lock cannot later alter the file.
/// Every event is preflighted against the staged tail before the first write; a conflict or
/// validation error leaves the file byte-for-byte unchanged. A write/flush error can still
/// leave a prefix, so retry the same operation tokens rather than making replacement events.
pub fn append_events_guarded(path: &Path, events: &[comments::Event], cancelled: Option<&std::sync::atomic::AtomicBool>) -> Result<Option<CommentConflict>> {
    append_events_guarded_stamped(path, events, cancelled).map(|(conflict, _)| conflict)
}

/// Metadata observed under the same file lock before and after our append. The viewer
/// accepts this transition only when its known source stamp still equals `before`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentWriteStamp {
    pub before: (PathBuf, u64, Option<SystemTime>),
    pub after: (PathBuf, u64, Option<SystemTime>),
}

pub fn append_events_guarded_stamped(path: &Path, events: &[comments::Event], cancelled: Option<&std::sync::atomic::AtomicBool>) -> Result<(Option<CommentConflict>, Option<CommentWriteStamp>)> {
    if let Some(root) = path.parent().and_then(|d| d.ancestors().find(|a| a.exists())).and_then(repo_root) {
        if in_repo_input(path, &root) { return err("input/ 폴더 안 파일에는 쓰지 않아요"); }
    }
    let target = canonical_target(path)?;
    if in_input(&target) || !is_takm_path(&target) { return err("안전하지 않은 Comment 대상이에요"); }
    let _lock = lock_path(&target)?;
    append_events_locked(&target, events, cancelled)
}

/// Caller checked the canonical target and holds its path lock.
pub(crate) fn append_events_locked(target: &Path, events: &[comments::Event], cancelled: Option<&std::sync::atomic::AtomicBool>) -> Result<(Option<CommentConflict>, Option<CommentWriteStamp>)> {
    if cancelled.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)) { return err("Comment write cancelled"); }
    let mut file = OpenOptions::new().read(true).append(true).open(target)?;
    let before = file.metadata()?;
    let before = (target.to_path_buf(), before.len(), before.modified().ok());
    let (_, mut tail) = tail_bytes(&file)?;
    let original = comments::parse(&String::from_utf8_lossy(&tail)).comments();
    let mut pending = Vec::new();
    for event in events {
        if event.operation.as_ref().is_some_and(|id| comments::parse(&String::from_utf8_lossy(&tail))
            .events.iter().any(|old| old.operation.as_ref() == Some(id))) {
            continue;
        }
        if let Some(expected) = &event.expected {
            let live = comments::parse(&String::from_utf8_lossy(&tail)).comments();
            let latest = live.get(event.id).cloned();
            if live.id_is_ambiguous(event.id) || latest.as_ref() != Some(expected) {
                return Ok((Some(CommentConflict { id: event.id, latest: original.get(event.id).cloned() }), None));
            }
        }
        let encoded = comments::encode(event);
        let bytes = encoded.as_bytes();
        let mut out = Vec::new();
        if tail.last().is_some_and(|&b| b != b'\n') || comments::ends_in_quote(&String::from_utf8_lossy(&tail)) {
            // A retry after a partial write completes that same line, including a cut quote.
            let matching = event.operation.as_ref().and_then(|_| {
                (1..bytes.len().min(tail.len() + 1)).rev().find(|&n| {
                    tail.ends_with(&bytes[..n]) && (tail.len() == n || tail[tail.len() - n - 1] == b'\n')
                })
            });
            if let Some(n) = matching {
                out.extend_from_slice(&bytes[n..]);
            } else {
                if comments::ends_in_quote(&String::from_utf8_lossy(&tail)) { out.push(b'"'); }
                // The marker must land after all twelve legacy fields, even if the cut was
                // in the first field. Otherwise an old fragment could look complete.
                out.extend_from_slice(comments::INCOMPLETE_SUFFIX.as_bytes());
                out.extend_from_slice(EOL.as_bytes());
                out.extend_from_slice(bytes);
            }
        } else {
            out.extend_from_slice(bytes);
        }
        if cancelled.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)) { return err("Comment write cancelled"); }
        tail.extend_from_slice(&out);
        pending.extend_from_slice(&out);
    }
    if cancelled.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)) { return err("Comment write cancelled"); }
    file.write_all(&pending)?;
    file.flush()?;
    let after = file.metadata()?;
    let after = (target.to_path_buf(), after.len(), after.modified().ok());
    Ok((None, Some(CommentWriteStamp { before, after })))
}

/// Check the fixed part against its SHA-256 in bounded chunks.
pub fn verify_fixed(path: &Path) -> Result<()> {
    verify_fixed_seal(path).map(|_| ())
}

fn read_seal(file: &File) -> Result<(u64, [u8; SEAL_LEN as usize])> {
    let fixed_len = read_index(file, file.metadata()?.len())?.fixed_len;
    let mut seal = [0; SEAL_LEN as usize];
    read_exact_at(file, &mut seal, fixed_len)?;
    Ok((fixed_len, seal))
}

pub(crate) fn fixed_seal(path: &Path) -> Result<(u64, [u8; SEAL_LEN as usize])> {
    read_seal(&File::open(path)?)
}

pub(crate) fn verify_fixed_seal(path: &Path) -> Result<(u64, [u8; SEAL_LEN as usize])> {
    let file = File::open(path)?;
    let (fixed_len, seal) = read_seal(&file)?;
    let mut buf = vec![0; IO_CHUNK];
    if hash_file_prefix(&file, fixed_len, &mut buf)?.as_slice() != seal {
        return err(".takm 고정 부분이 바뀌었어요(SHA-256이 달라요).");
    }
    Ok((fixed_len, seal))
}

fn hash_file_prefix(file: &File, len: u64, buf: &mut [u8]) -> Result<sha2::digest::Output<Sha256>> {
    let mut hash = Sha256::new();
    let mut pos = 0u64;
    while pos < len {
        let n = (len - pos).min(buf.len() as u64) as usize;
        read_exact_at(file, &mut buf[..n], pos)?;
        hash.update(&buf[..n]);
        pos += n as u64;
    }
    Ok(hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tak::fast_read::read_take_fast;
    use crate::view_data::ViewData;

    use crate::sample_tak::sample_tak;


    struct TempDir(crate::test_support::TempDir);
    impl TempDir {
        fn new(tag: &str) -> TempDir {
            TempDir(crate::test_support::TempDir::new(&format!("takm_test_{tag}")))
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

    #[cfg(windows)]
    #[test]
    fn windows_conversion_and_create_if_absent_support_paths_longer_than_max_path() {
        use std::os::windows::ffi::OsStrExt;
        let root = TempDir::new("windows_long_paths");
        let mut dir = root.0.clone();
        while dir.as_os_str().encode_wide().count() < 300 {
            dir.push("long_path_component_for_conversion_and_atomic_move");
        }
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.tak");
        let original = sample_tak();
        std::fs::write(&source, &original).unwrap();
        let made = convert(&source, None).unwrap();
        verify_fixed(&made.out).unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), original);
        let destination = std::fs::read(&made.out).unwrap();
        assert!(convert(&source, None).is_err(), "conversion must still refuse an existing destination");
        let tmp = dir.join("another.part");
        std::fs::write(&tmp, b"replacement").unwrap();
        assert_eq!(place_new(&tmp, &made.out).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&made.out).unwrap(), destination, "the final atomic move must never overwrite");
        assert_eq!(std::fs::read(&tmp).unwrap(), b"replacement");
        let new_destination = dir.join("new_destination.takm");
        place_new(&tmp, &new_destination).unwrap();
        assert_eq!(std::fs::read(new_destination).unwrap(), b"replacement");
        assert!(!tmp.exists());
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
            operation: None,
            expected: None,
        }
    }

    #[test]
    fn retry_finishes_partial_event_and_does_not_duplicate_a_completed_event() {
        let (_dir, _source, path) = converted("retry_event");
        let mut item = event(17, "a, quoted note");
        item.operation = Some("process-unique-operation".into());
        let line = comments::encode(&item);
        let cut = line.find("operation=").unwrap() + 4;
        OpenOptions::new().append(true).open(&path).unwrap().write_all(&line.as_bytes()[..cut]).unwrap();
        assert!(read_comments(&path).unwrap().events.is_empty());
        append_events(&path, &[item.clone()]).unwrap();
        append_events(&path, &[item.clone()]).unwrap();
        assert_eq!(read_comments(&path).unwrap().events, vec![item]);
    }

    #[test]
    fn every_interrupted_byte_with_an_interleaved_writer_retries_once() {
        let (dir, _source, original) = converted("every_cut_interleaved");
        let mut a = event(41, "한글, \"quoted\" text");
        a.operation = Some("writer-a".into());
        let mut b = event(42, "other writer");
        b.operation = Some("writer-b".into());
        let encoded = comments::encode(&a);
        for cut in 0..encoded.len() {
            let path = dir.0.join(format!("cut-{cut}.takm"));
            std::fs::copy(&original, &path).unwrap();
            OpenOptions::new().append(true).open(&path).unwrap().write_all(&encoded.as_bytes()[..cut]).unwrap();
            assert!(read_comments(&path).unwrap().events.is_empty(), "before interleave at {cut}");
            append_events(&path, &[b.clone()]).unwrap();
            let after_b = read_comments(&path).unwrap();
            assert!(after_b.events.iter().all(|e| e.id != a.id), "foreign write completed fragment at {cut}");
            append_events(&path, &[a.clone()]).unwrap();
            append_events(&path, &[a.clone()]).unwrap();
            let events = read_comments(&path).unwrap().events;
            assert_eq!(events.iter().filter(|e| e.id == a.id).count(), 1, "cut {cut}");
            assert_eq!(events.iter().filter(|e| e.id == b.id).count(), 1, "cut {cut}");
        }
    }

    #[test]
    fn every_legacy_fragment_is_discarded_before_a_foreign_event() {
        let (dir, _source, original) = converted("every_legacy_cut");
        let legacy = event(51, "legacy, \"quoted\" text");
        let mut other = event(52, "new writer");
        other.operation = Some("new-writer".into());
        let encoded = comments::encode(&legacy);
        for cut in 1..encoded.len() {
            let path = dir.0.join(format!("legacy-{cut}.takm"));
            std::fs::copy(&original, &path).unwrap();
            OpenOptions::new().append(true).open(&path).unwrap().write_all(&encoded.as_bytes()[..cut]).unwrap();
            append_events(&path, &[other.clone()]).unwrap();
            let events = read_comments(&path).unwrap().events;
            assert!(events.iter().all(|e| e.id != legacy.id), "legacy fragment at {cut}");
            assert_eq!(events.iter().filter(|e| e.id == other.id).count(), 1, "cut {cut}");
        }
    }

    #[test]
    fn guarded_edit_and_delete_reject_stale_bases_under_lock() {
        let (_dir, _source, path) = converted("stale_guard");
        let mut add = event(9, "original");
        add.operation = Some("base".into());
        append_events(&path, &[add]).unwrap();
        let base = read_comments(&path).unwrap().comments().get(9).unwrap().clone();
        let mut a = base.clone();
        a.text = "A text".into();
        let mut change_a = comments::Event::of(comments::Kind::Edit, &a, "t".into(), "a".into());
        change_a.expected = Some(base.clone());
        assert!(append_events_guarded(&path, &[change_a], None).unwrap().is_none());
        let mut stale = base.clone();
        stale.markers = vec!["B marker".into()];
        for kind in [comments::Kind::Edit, comments::Kind::Delete] {
            let mut change_b = comments::Event::of(kind, &stale, "t".into(), "b".into());
            change_b.expected = Some(base.clone());
            let conflict = append_events_guarded(&path, &[change_b], None).unwrap().unwrap();
            assert_eq!(conflict.latest, Some(a.clone()));
            assert_eq!(read_comments(&path).unwrap().comments().get(9), Some(&a));
        }
    }

    #[test]
    fn cancelled_writer_does_not_write_after_held_lock_is_released() {
        let (_dir, _source, path) = converted("discard_held_lock");
        let writer = comments::Writer::spawn(Box::new(|| {}));
        writer.drain().unwrap();
        let lock = lock_path(&path).unwrap();
        let mut item = event(13, "discarded");
        item.operation = Some("discarded-operation".into());
        assert!(writer.send(7, path.clone(), item));
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(writer.poll().is_empty());
        writer.cancel_tag(7);
        drop(lock);
        let until = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let done = writer.poll();
            if !done.is_empty() { assert!(done[0].cancelled); break; }
            assert!(Instant::now() < until);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(read_comments(&path).unwrap().events.is_empty());
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
        assert_eq!(parse_args(&s(&["a.tak"])).unwrap(), (vec![PathBuf::from("a.tak")], None, false));
        assert_eq!(
            parse_args(&s(&["a.tak", "--out-dir", "o", "b.tak", "--verify"])).unwrap(),
            (vec![PathBuf::from("a.tak"), PathBuf::from("b.tak")], Some(PathBuf::from("o")), true)
        );
        assert!(parse_args(&s(&[])).is_err());
        assert!(parse_args(&s(&["--out-dir", "o"])).is_err());
        assert!(parse_args(&s(&["a.tak", "--out-dir"])).is_err());
        assert!(parse_args(&s(&["a.tak", "--out-dir", "--verify"])).is_err());
        assert!(parse_args(&s(&["a.tak", "--verify", "--verify"])).is_err());
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
