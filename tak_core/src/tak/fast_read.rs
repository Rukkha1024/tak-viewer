//! Markers-first TAK reader. Stage 1 reads the container, metadata, nodes, and marker
//! channels using positional reads, skipping camera and analog data. Stage 2 reads analog
//! channels on a background thread. Both stages reuse the standard container and channel
//! parsers; unexpected layouts fall back to `read_take`. TAK reads fail if file size or
//! modification time changes. TAKM reads allow a growing comment tail, but require that
//! the fixed portion remain present.

use super::channels::{channel_dim, CHANNELS_HEADER};
use super::container::{u16_at, u32_at, utf16le_strict, Container, END, MAGIC};
use super::{decode_analog, decode_markers, err, AnalogDevice, Result, Take};
use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

/// Largest single positional read; big ranges are cut into pieces of this size.
const CHUNK: usize = 2 << 20;
/// Threads for big reads (the WSL share gives ~2x the single-thread speed with 2-8 readers).
const THREADS: usize = 8;
/// First read at a channel record (one read usually covers the whole record header).
const PROBE: u64 = 4096;
/// Once a run of non-analog records grows past this, the rest of the stream is read in one
/// parallel bulk read (marker channels sit at the end of `Channels.dat` in Motive files).
const BULK_AFTER: u64 = 64 << 10;

/// Stage 1 result.
pub struct FastRead {
    /// Markers, assets, plates' geometry, label groups; `analog` empty until stage 2.
    pub take: Take,
    /// Stage 2 work; None when the analog part is already in `take` (full-read fallback).
    pub analog: Option<AnalogJob>,
    /// Stage timings in ms (for `--bench`).
    pub times: Vec<(&'static str, f64)>,
}

/// Stage 2: the analog records of `Channels.dat`.
pub struct AnalogJob {
    path: PathBuf,
    before: (u64, Option<SystemTime>),
    /// `.takm`: the file may grow (Comment tail); `before.0` is the least length it must keep.
    grows: bool,
    map: StreamMap,
    /// Non-analog parts already read in stage 1 (stream offset, bytes), incl. the 10-byte header.
    kept: Vec<(u64, Vec<u8>)>,
    /// Analog record ranges [start, end) in the stream, ascending.
    analog: Vec<(u64, u64)>,
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn stat_key(m: &Metadata) -> (u64, Option<SystemTime>) {
    (m.len(), m.modified().ok())
}

fn unchanged(path: &Path, before: (u64, Option<SystemTime>), grows: bool) -> Result<()> {
    let now = stat_key(&std::fs::metadata(path)?);
    if grows {
        if now.0 < before.0 {
            return err(".takm became shorter than its fixed part while reading");
        }
    } else if now != before {
        return err("TAK changed during marker extraction");
    }
    Ok(())
}

/// Stage 1. `sha`: also compute each trajectory's `raw16_sha256` (only `--dump` needs it).
/// Falls back to the full `read_take` (analog included, `analog` None) when the file does not
/// have the expected layout; the result is then exactly `read_take`'s.
/// A `.takm` path (`crate::takm::is_takm_path`) is read through its stream table.
pub fn read_take_fast(path: &Path, sha: bool) -> Result<FastRead> {
    let t0 = Instant::now();
    let first = if crate::takm::is_takm_path(path) { markers_first_takm(path, sha) } else { markers_first(path, sha) };
    match first {
        Ok(Some(read)) => Ok(read),
        Ok(None) | Err(_) => {
            let take = super::read_take(path)?;
            Ok(FastRead {
                take,
                analog: None,
                times: vec![("full read (fallback)", ms(t0))],
            })
        }
    }
}

impl AnalogJob {
    /// Stage 2: (analog devices, warning) exactly as `read_take` would give them.
    pub fn run(self) -> Result<(Vec<AnalogDevice>, Option<String>)> {
        let file = File::open(&self.path)?;
        let mut full = vec![0u8; self.map.len as usize];
        for (off, bytes) in &self.kept {
            full[*off as usize..*off as usize + bytes.len()].copy_from_slice(bytes);
        }
        let mut jobs = Vec::new();
        let mut rest: &mut [u8] = &mut full;
        let mut base = 0u64;
        for &(a, b) in &self.analog {
            let (_, r) = std::mem::take(&mut rest).split_at_mut((a - base) as usize);
            let (mut mid, r) = r.split_at_mut((b - a) as usize);
            for (off, n) in self.map.pieces(a, b) {
                let (piece, m) = std::mem::take(&mut mid).split_at_mut(n);
                jobs.push((off, piece));
                mid = m;
            }
            rest = r;
            base = b;
        }
        read_parallel(&file, jobs)?;
        unchanged(&self.path, self.before, self.grows)?;
        Ok(decode_analog(&full))
    }
}

/// `Ok(None)` = not the expected layout (caller falls back to `read_take`).
fn markers_first(path: &Path, sha: bool) -> Result<Option<FastRead>> {
    let t0 = Instant::now();
    let before = stat_key(&std::fs::metadata(path)?);
    let len = before.0;
    let file = File::open(path)?;
    if len < 512 {
        return Ok(None);
    }
    // Container skeleton into a sparse copy of the file.
    let mut img = vec![0u8; len as usize];
    read_exact_at(&file, &mut img[..512], 0)?;
    if img[..8] != MAGIC || u32_at(&img, 72)? != 0 {
        return Ok(None);
    }
    let ss: u64 = match u16_at(&img, 30)? {
        9 => 512,
        12 => 4096,
        15 => 32768,
        _ => return Ok(None),
    };
    let count = u32_at(&img, 44)? as usize;
    if count > 109 {
        return Ok(None);
    }
    let fat_sectors: Vec<u32> = (0..count).map(|i| u32_at(&img, 76 + i * 4)).collect::<Result<_>>()?;
    fill(&file, &mut img, ss, &fat_sectors)?;
    let mut fat = Vec::new();
    for &s in &fat_sectors {
        let off = (s as u64 + 1) * ss;
        if off >= len {
            return Ok(None);
        }
        let end = (off + ss).min(len) as usize;
        fat.extend(
            img[off as usize..end]
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])),
        );
    }
    let dir = follow(&fat, u32_at(&img, 48)?);
    fill(&file, &mut img, ss, &dir)?;
    let mut small = follow(&fat, u32_at(&img, 60)?);
    if let Some(&first) = dir.first() {
        let root = (first as u64 + 1) * ss;
        if root + 128 <= len {
            small.extend(follow(&fat, u32_at(&img, root as usize + 116)?));
        }
    }
    fill(&file, &mut img, ss, &small)?;

    // Directory with the same parser as `read_take`; each wanted stream must appear once.
    let (nodes_i, meta_i, chan_i, chan_chain, more) = {
        let c = Container::parse(&img)?;
        let streams = c.streams()?;
        let find = |name: &str| {
            let mut hits = streams.iter().filter(|(p, _)| p == name).map(|(_, i)| *i);
            match (hits.next(), hits.next()) {
                (Some(i), None) => Some(i),
                _ => None,
            }
        };
        let (Some(n), Some(m), Some(ch)) = (find("Nodes.dat"), find("MetaData.dat"), find("Channels.dat")) else {
            return Ok(None);
        };
        if c.entries[ch].size < c.cutoff as u64 {
            return Ok(None);
        }
        let mut more = Vec::new();
        for i in [n, m] {
            if c.entries[i].size >= c.cutoff as u64 {
                more.extend(c.stream_chain(i)?);
            }
        }
        (n, m, ch, c.stream_chain(ch)?, more)
    };
    fill(&file, &mut img, ss, &more)?;
    let (nodes, meta, chan_len) = {
        let c = Container::parse(&img)?;
        (c.read_stream(nodes_i)?, c.read_stream(meta_i)?, c.entries[chan_i].size)
    };
    drop(img);
    let map = StreamMap {
        sector: ss,
        chain: chan_chain,
        len: chan_len,
        flat: None,
    };
    let t_container = ms(t0);
    markers_from(path, &file, (before, false), len, &nodes, &meta, map, t_container, sha)
}

/// `.takm` stage 1: the stream table gives where `Nodes.dat`, `MetaData.dat` and
/// `Channels.dat` lie (each once, else `Ok(None)` = full read); then as for a TAK.
fn markers_first_takm(path: &Path, sha: bool) -> Result<Option<FastRead>> {
    let t0 = Instant::now();
    let len = std::fs::metadata(path)?.len();
    let file = File::open(path)?;
    let index = crate::takm::read_index(&file, len)?;
    let (Some(n), Some(m), Some(ch)) = (index.unique("Nodes.dat"), index.unique("MetaData.dat"), index.unique("Channels.dat"))
    else {
        return Ok(None);
    };
    let mut nodes = vec![0u8; n.size as usize];
    let mut meta = vec![0u8; m.size as usize];
    read_parallel(&file, vec![(n.offset, &mut nodes[..]), (m.offset, &mut meta[..])])?;
    let map = StreamMap {
        sector: 0,
        chain: Vec::new(),
        len: ch.size,
        flat: Some(ch.offset),
    };
    let t_container = ms(t0);
    markers_from(path, &file, ((index.tail_start(), None), true), len, &nodes, &meta, map, t_container, sha)
}

/// The rest of stage 1, the same for a TAK and a `.takm`: walk `Channels.dat` (`map`), decode
/// the markers, prepare stage 2. `guard` = (stat before, `.takm` that may grow); `len` = the
/// file size when reading started.
#[allow(clippy::too_many_arguments)]
fn markers_from(
    path: &Path,
    file: &File,
    guard: ((u64, Option<SystemTime>), bool),
    len: u64,
    nodes: &[u8],
    meta: &[u8],
    map: StreamMap,
    t_container: f64,
    sha: bool,
) -> Result<Option<FastRead>> {
    let (before, grows) = guard;
    let t1 = Instant::now();
    let Some(walked) = walk_channels(file, &map)? else {
        return Ok(None);
    };
    let t_channels = ms(t1);
    let bulk_ms = walked.bulk_ms;

    // Stage-1 stream: the header with the non-analog record count, then those records in order.
    let t2 = Instant::now();
    let keep: Vec<(u64, u64)> = walked.records.iter().filter(|r| !r.2).map(|r| (r.0, r.1)).collect();
    let mut stream = Vec::with_capacity(10 + keep.iter().map(|(a, b)| (b - a) as usize).sum::<usize>());
    stream.extend_from_slice(&CHANNELS_HEADER);
    stream.extend_from_slice(&(keep.len() as u32).to_le_bytes());
    for &(a, b) in &keep {
        let Some((off, bytes)) = walked.kept.iter().find(|(o, v)| *o <= a && b <= *o + v.len() as u64) else {
            return Ok(None);
        };
        stream.extend_from_slice(&bytes[(a - off) as usize..(b - off) as usize]);
    }
    let take = decode_markers(path, len as usize, nodes, &stream, meta, sha)?;
    unchanged(path, before, grows)?;
    let t_decode = ms(t2);

    let analog: Vec<(u64, u64)> = walked
        .records
        .iter()
        .filter(|r| r.2 && !walked.kept.iter().any(|(o, v)| *o <= r.0 && r.1 <= *o + v.len() as u64))
        .map(|r| (r.0, r.1))
        .collect();
    let job = AnalogJob {
        path: path.to_path_buf(),
        before,
        grows,
        map,
        kept: walked.kept,
        analog,
    };
    let times = vec![
        ("container", t_container),
        ("record heads", t_channels - bulk_ms),
        ("marker bytes", bulk_ms),
        ("markers", t_decode),
    ];
    Ok(Some(FastRead {
        take,
        analog: Some(job),
        times,
    }))
}

/// Channel records found by `walk_channels`: (start, end, analog) in stream offsets, plus the
/// stream bytes read so far (the 10-byte header and every non-analog record are inside `kept`).
struct Walked {
    records: Vec<(u64, u64, bool)>,
    kept: Vec<(u64, Vec<u8>)>,
    /// ms spent in the bulk read of the marker records.
    bulk_ms: f64,
}

/// Record header at the start of `buf` (layout of `channels::iter_channel_inventory`).
enum Head {
    /// `buf` too short: need at least this many bytes.
    Need(usize),
    /// Not a record the reader knows (the full read reports it).
    Bad,
    Ok {
        header: usize,
        data: u64,
        analog: bool,
    },
}

fn parse_head(buf: &[u8]) -> Head {
    if buf.len() < 2 {
        return Head::Need(2);
    }
    let length = u16::from_le_bytes([buf[0], buf[1]]) as usize;
    let cur = 2 + length;
    if buf.len() < cur + 3 {
        return Head::Need(cur + 3);
    }
    let kind = &buf[2..cur];
    let Some(dim) = std::str::from_utf8(kind).ok().filter(|_| kind.is_ascii()).and_then(channel_dim) else {
        return Head::Bad;
    };
    if buf[cur] != 4 {
        return Head::Bad;
    }
    let chars = u16::from_le_bytes([buf[cur + 1], buf[cur + 2]]) as usize;
    let uid = cur + 3 + chars * 2;
    let count = uid + 16 + 4 * dim;
    if buf.len() < count + 4 {
        return Head::Need(count + 4);
    }
    let Ok(name) = utf16le_strict(&buf[cur + 3..uid]) else {
        return Head::Bad;
    };
    let n = u32::from_le_bytes([buf[count], buf[count + 1], buf[count + 2], buf[count + 3]]) as u64;
    Head::Ok {
        header: count + 4,
        data: n * (4 + 4 * dim as u64),
        analog: name.starts_with("AnalogChannel"),
    }
}

/// Walk the records by their declared lengths, reading only record headers plus the
/// non-analog records (the analog data is skipped). `None` = unexpected layout.
fn walk_channels(file: &File, map: &StreamMap) -> Result<Option<Walked>> {
    let len = map.len;
    if len < 10 {
        return Ok(None);
    }
    let header = map.read(file, 0, 10)?;
    if header[..6] != CHANNELS_HEADER {
        return Ok(None);
    }
    let count = u32::from_le_bytes([header[6], header[7], header[8], header[9]]) as usize;
    let mut records: Vec<(u64, u64, bool)> = Vec::new();
    let mut kept: Vec<(u64, Vec<u8>)> = vec![(0, header)];
    // Bytes read at the latest probe: (stream offset, bytes).
    let mut window: (u64, Vec<u8>) = (0, Vec::new());
    // Current run of non-analog records: (start offset, index of its first record).
    let mut run: Option<(u64, usize)> = None;
    let mut pos = 10u64;
    // In-memory tail after a bulk read: (stream offset, bytes).
    let mut tail: Option<(u64, Vec<u8>)> = None;
    let mut bulk_ms = 0.0;
    while records.len() < count {
        let head = loop {
            let (base, buf) = match &tail {
                Some((o, v)) => (*o, v.as_slice()),
                None => (window.0, window.1.as_slice()),
            };
            let have = if pos >= base && pos <= base + buf.len() as u64 {
                &buf[(pos - base) as usize..]
            } else {
                &[][..]
            };
            match parse_head(have) {
                Head::Need(n) if tail.is_none() && pos + (n as u64) <= len => {
                    let want = (n as u64).max(PROBE).min(len - pos);
                    window = (pos, map.read(file, pos, pos + want)?);
                }
                Head::Need(_) | Head::Bad => return Ok(None),
                ok => break ok,
            }
        };
        let Head::Ok { header, data, analog } = head else { return Ok(None) };
        let end = pos + header as u64 + data;
        if end > len {
            return Ok(None);
        }
        if tail.is_none() {
            if analog {
                if let Some((start, _)) = run.take() {
                    kept.push((start, map.read(file, start, pos)?));
                }
            } else {
                let (start, first) = *run.get_or_insert((pos, records.len()));
                if end - start > BULK_AFTER {
                    // Read the rest of the stream at once and walk it again in memory.
                    let t = Instant::now();
                    tail = Some((start, map.read(file, start, len)?));
                    bulk_ms = ms(t);
                    records.truncate(first);
                    pos = start;
                    run = None;
                    continue;
                }
            }
        }
        records.push((pos, end, analog));
        pos = end;
    }
    if pos != len {
        return Ok(None);
    }
    if let Some((start, _)) = run {
        kept.push((start, map.read(file, start, len)?));
    }
    kept.extend(tail);
    Ok(Some(Walked {
        records,
        kept,
        bulk_ms,
    }))
}

/// Where a stream lies in the file: a regular container stream = its sectors in order (from
/// `Container::stream_chain`); a `.takm` stream = one piece starting at `flat`.
struct StreamMap {
    sector: u64,
    chain: Vec<u32>,
    len: u64,
    /// `.takm`: file offset of the stream's first byte (`sector` / `chain` unused).
    flat: Option<u64>,
}

impl StreamMap {
    /// Stream bytes [a, b) as file pieces (file offset, length): consecutive sectors merged,
    /// pieces at most `CHUNK` long.
    fn pieces(&self, a: u64, b: u64) -> Vec<(u64, usize)> {
        let mut out: Vec<(u64, usize)> = Vec::new();
        let mut pos = a;
        if let Some(base) = self.flat {
            while pos < b {
                let n = (b - pos).min(CHUNK as u64);
                out.push((base + pos, n as usize));
                pos += n;
            }
            return out;
        }
        let ss = self.sector;
        while pos < b {
            let within = pos % ss;
            let n = (ss - within).min(b - pos);
            let off = (self.chain[(pos / ss) as usize] as u64 + 1) * ss + within;
            match out.last_mut() {
                Some((o, l)) if *o + *l as u64 == off && *l + n as usize <= CHUNK => *l += n as usize,
                _ => out.push((off, n as usize)),
            }
            pos += n;
        }
        out
    }

    /// Stream bytes [a, b), read by several threads when big.
    fn read(&self, file: &File, a: u64, b: u64) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; (b - a) as usize];
        let mut jobs = Vec::new();
        let mut rest: &mut [u8] = &mut buf;
        for (off, n) in self.pieces(a, b) {
            let (piece, r) = std::mem::take(&mut rest).split_at_mut(n);
            jobs.push((off, piece));
            rest = r;
        }
        read_parallel(file, jobs)?;
        Ok(buf)
    }
}

/// FAT chain from `start` (stops at the end mark, a bad index or a loop; `Container::parse`
/// then reports the problem like the full read does).
fn follow(fat: &[u32], start: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut seen = vec![false; fat.len()];
    let mut current = start;
    while current != END && (current as usize) < fat.len() && !seen[current as usize] {
        seen[current as usize] = true;
        out.push(current);
        current = fat[current as usize];
    }
    out
}

/// Read whole sectors into the sparse file copy (sectors past the end are left out).
fn fill(file: &File, img: &mut [u8], ss: u64, sectors: &[u32]) -> Result<()> {
    let len = img.len() as u64;
    let mut sorted: Vec<u64> = sectors.iter().map(|&s| (s as u64 + 1) * ss).filter(|&o| o < len).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    for o in sorted {
        let e = (o + ss).min(len);
        match ranges.last_mut() {
            Some((s, last)) if *last == o && e - *s <= CHUNK as u64 => *last = e,
            _ => ranges.push((o, e)),
        }
    }
    let mut jobs = Vec::new();
    let mut rest: &mut [u8] = img;
    let mut base = 0u64;
    for (a, b) in ranges {
        let (_, r) = std::mem::take(&mut rest).split_at_mut((a - base) as usize);
        let (piece, r) = r.split_at_mut((b - a) as usize);
        jobs.push((a, piece));
        rest = r;
        base = b;
    }
    read_parallel(file, jobs)
}

/// Positional read of exactly `buf.len()` bytes (no shared file cursor, safe across threads).
pub(crate) fn read_exact_at(file: &File, mut buf: &mut [u8], mut off: u64) -> Result<()> {
    while !buf.is_empty() {
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(file, buf, off)?;
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(file, buf, off)?;
        if n == 0 {
            return err("file is shorter than its container says");
        }
        buf = &mut std::mem::take(&mut buf)[n..];
        off += n as u64;
    }
    Ok(())
}

/// Read (file offset, destination) jobs; several threads when there is more than one job.
fn read_parallel(file: &File, jobs: Vec<(u64, &mut [u8])>) -> Result<()> {
    if jobs.len() <= 1 {
        for (off, buf) in jobs {
            read_exact_at(file, buf, off)?;
        }
        return Ok(());
    }
    let threads = THREADS.min(jobs.len());
    let queue = Mutex::new(jobs.into_iter());
    let failed = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let job = queue.lock().map(|mut q| q.next()).unwrap_or(None);
                let Some((off, buf)) = job else { break };
                if let Err(e) = read_exact_at(file, buf, off) {
                    if let Ok(mut f) = failed.lock() {
                        f.get_or_insert(e);
                    }
                    break;
                }
            });
        }
    });
    match failed.into_inner().ok().flatten() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(chain: Vec<u32>, len: u64) -> StreamMap {
        StreamMap { sector: 512, chain, len, flat: None }
    }

    #[test]
    fn flat_pieces_are_one_run_cut_at_chunk_size() {
        let m = StreamMap { sector: 0, chain: Vec::new(), len: 3 * CHUNK as u64, flat: Some(1000) };
        assert_eq!(m.pieces(10, 20), vec![(1010, 10)]);
        assert_eq!(m.pieces(0, CHUNK as u64 + 5), vec![(1000, CHUNK), (1000 + CHUNK as u64, 5)]);
        assert!(m.pieces(7, 7).is_empty());
    }

    #[test]
    fn pieces_merge_consecutive_sectors_and_split_elsewhere() {
        let m = map(vec![4, 5, 6, 9, 10], 5 * 512);
        // Sectors 4,5,6 are next to each other in the file; 9,10 too.
        assert_eq!(m.pieces(0, 5 * 512), vec![(5 * 512, 3 * 512), (10 * 512, 2 * 512)]);
        assert_eq!(m.pieces(100, 700), vec![(5 * 512 + 100, 600)]);
        assert_eq!(m.pieces(3 * 512 - 10, 3 * 512 + 10), vec![(8 * 512 - 10, 10), (10 * 512, 10)]);
    }

    #[test]
    fn follow_stops_at_end_loop_and_bad_index() {
        assert_eq!(follow(&[1, END, 0], 0), vec![0, 1]);
        assert_eq!(follow(&[1, 0], 0), vec![0, 1]);
        assert_eq!(follow(&[5], 0), vec![0]);
        assert_eq!(follow(&[END], END), Vec::<u32>::new());
    }

    fn record(kind: &str, name: &str, dim: usize, samples: usize) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend((kind.len() as u16).to_le_bytes());
        r.extend(kind.as_bytes());
        r.push(4);
        let units: Vec<u16> = name.encode_utf16().collect();
        r.extend((units.len() as u16).to_le_bytes());
        for u in units {
            r.extend(u.to_le_bytes());
        }
        r.extend([7u8; 16]);
        r.extend(vec![0u8; 4 * dim]);
        r.extend((samples as u32).to_le_bytes());
        for i in 0..samples {
            r.extend((i as u32).to_le_bytes());
            r.extend(vec![(i % 200) as u8; 4 * dim]);
        }
        r
    }

    #[test]
    fn head_parses_record_and_asks_for_more_bytes() {
        let r = record("Vector3fChannel", "Translation", 3, 2);
        match parse_head(&r) {
            Head::Ok { header, data, analog } => {
                assert_eq!(header as u64 + data, r.len() as u64);
                assert!(!analog);
            }
            _ => panic!("record not parsed"),
        }
        assert!(matches!(parse_head(&r[..5]), Head::Need(_)));
        let a = record("FloatChannel", "AnalogChannel3", 1, 1);
        assert!(matches!(parse_head(&a), Head::Ok { analog: true, .. }));
        assert!(matches!(parse_head(&record("NoSuchChannel", "x", 0, 0)), Head::Bad));
    }

    /// A file whose single stream is laid out over non-consecutive sectors, walked and read
    /// back through the positional reader.
    #[test]
    fn walk_skips_analog_data_and_keeps_the_rest() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&CHANNELS_HEADER);
        let recs = [
            record("FloatChannel", "Error", 1, 1),
            record("FloatChannel", "AnalogChannel0", 1, 300),
            record("FloatChannel", "AnalogChannel1", 1, 300),
            record("QuaternionChannel", "Rotation", 4, 0),
            record("Vector3fChannel", "Translation", 3, 5000),
            record("Vector3fChannel", "Translation", 3, 3),
        ];
        stream.extend((recs.len() as u32).to_le_bytes());
        for r in &recs {
            stream.extend(r);
        }
        // Sectors of the stream are stored in reverse order after one spare sector.
        let ss = 512usize;
        let n = stream.len().div_ceil(ss);
        let chain: Vec<u32> = (0..n as u32).rev().map(|i| i + 1).collect();
        let mut file_bytes = vec![0u8; (n + 2) * ss];
        for (k, &s) in chain.iter().enumerate() {
            let piece = &stream[k * ss..((k + 1) * ss).min(stream.len())];
            let off = (s as usize + 1) * ss;
            file_bytes[off..off + piece.len()].copy_from_slice(piece);
        }
        let dir = std::env::temp_dir().join(format!("tak_fast_read_test_{}", std::process::id()));
        std::fs::write(&dir, &file_bytes).unwrap();
        let file = File::open(&dir).unwrap();
        let m = map(chain, stream.len() as u64);
        assert_eq!(m.read(&file, 0, stream.len() as u64).unwrap(), stream);
        let w = walk_channels(&file, &m).unwrap().expect("walked");
        let flags: Vec<bool> = w.records.iter().map(|r| r.2).collect();
        assert_eq!(flags, [false, true, true, false, false, false]);
        for &(a, b, analog) in &w.records {
            let inside = w.kept.iter().any(|(o, v)| *o <= a && b <= *o + v.len() as u64);
            assert_eq!(inside, !analog, "record {a}..{b}");
            if let Some((o, v)) = w.kept.iter().find(|(o, v)| *o <= a && b <= *o + v.len() as u64) {
                assert_eq!(&v[(a - o) as usize..(b - o) as usize], &stream[a as usize..b as usize]);
            }
        }
        drop(file);
        let _ = std::fs::remove_file(&dir);
    }
}
