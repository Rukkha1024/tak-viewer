
use super::channels::{channel_dim, CHANNELS_HEADER};
use super::container::{u16_at, u32_at, utf16le_strict, Container, END, MAGIC};
use super::{decode_analog, decode_markers, err, AnalogChannel, AnalogDevice, Result, Take, Uid};
use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

/// Largest single positional read; big ranges are cut into pieces of this size.
const CHUNK: usize = 2 << 20;
/// Maximum single-file read width; extra readers share a process-wide budget.
const THREADS: usize = 8;
/// Every supported kind has at least this many header bytes, even with an empty property:
/// uint16 type length + FloatChannel (12) + version/name length (3) + UID (16) + f32/count (8).
const MIN_HEADER: u64 = 41;
/// Stage 1 result.
pub struct FastRead {
    /// Markers, assets, plates' geometry, label groups, and header-only analog device manifest.
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
    records: Arc<Vec<Record>>,
    manifest: Vec<AnalogDevice>,
    /// Channel-record indices for each device, in original channel order.
    device_records: Arc<Vec<Vec<usize>>>,
    plate_devices: Option<Vec<usize>>,
    fallback: Option<String>,
    /// None = unchanged complete decode (the `--dump-fast` path).
    selected: Option<Vec<usize>>,
    indices: Vec<usize>,
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
        fallback => {
            let reason = match fallback {
                Err(e) => e.to_string(),
                _ => "unsupported container or marker layout".into(),
            };
            eprintln!("selective analog fallback ({}): {reason}", path.display());
            let take = super::read_take(path)?;
            Ok(FastRead {
                take,
                analog: None,
                times: vec![("full read (fallback)", ms(t0))],
            })
        }
    }
}

/// Refresh demand-only analog work after saving a `.takm`. Rewriting marker streams can
/// move both the channel stream and its records, invalidating every previously deferred job.
/// Read only the current stream table, device nodes and channel headers; marker, forceplate
/// and other analog sample payloads are skipped. Call on the save worker, replacing the old
/// deferred job. `None` means no non-forceplate devices remain. Ambiguous layout is an error;
/// this function never calls the full reader or starts an analog sample read.
pub fn deferred_job_after_save(path: &Path) -> Result<Option<AnalogJob>> {
    if !crate::takm::is_takm_path(path) {
        return err("deferred analog metadata refresh requires a .takm file");
    }
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let index = crate::takm::read_index(&file, len)?;
    let (Some(nodes), Some(channels)) = (index.unique("Nodes.dat"), index.unique("Channels.dat")) else {
        return err("deferred analog metadata refresh requires unique Nodes.dat and Channels.dat streams");
    };
    if nodes.offset < channels.offset + channels.size && channels.offset < nodes.offset + nodes.size {
        return err("deferred analog metadata streams overlap");
    }
    let size = usize::try_from(nodes.size).map_err(|_| super::TakError("device metadata is too large".into()))?;
    let mut bytes = vec![0u8; size];
    read_exact_at(&file, &mut bytes, nodes.offset)?;
    let typed = super::nodes::analog_device_nodes(&bytes)?;
    let device_uids = typed.iter().map(|device| device.uid).collect();
    let map = StreamMap { sector: 0, chain: Vec::new(), len: channels.size, flat: Some(channels.offset) };
    let Some(walked) = walk_channel_records(&file, &map, &device_uids, None, false)? else {
        return err("unsupported channel layout during deferred analog metadata refresh");
    };
    let (manifest, device_records) = device_manifest(&walked.records)?;
    let plates = selective_layout_for_nodes(&typed, &walked.records, &device_records)?;
    let before = (index.tail_start(), None);
    unchanged(path, before, true)?;
    let indices = (0..manifest.len()).collect();
    let job = AnalogJob {
        path: path.to_path_buf(), before, grows: true, map,
        kept: Vec::new(), analog: Vec::new(), records: Arc::new(walked.records), manifest,
        device_records: Arc::new(device_records), plate_devices: Some(plates), fallback: None,
        selected: None, indices,
    };
    Ok(job.split().1)
}

impl AnalogJob {
    /// Device positions loaded by this job. Indices remain stable through both stages.
    pub fn device_indices(&self) -> &[usize] {
        &self.indices
    }

    /// Forceplate geometry index -> analog device index, established by structural UID.
    pub fn plate_devices(&self) -> Option<&[usize]> {
        self.plate_devices.as_deref()
    }

    /// Why selective reading was disabled; report this when starting the full fallback.
    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback.as_deref()
    }

    /// Split normal viewer opening into forceplates and one owned, deferred non-plate job.
    /// Ambiguous files keep the previous full analog read. The unsplit job's `run` always
    /// decodes the entire original stream, preserving dump parity and validation.
    pub fn split(mut self) -> (Self, Option<Self>) {
        if self.selected.is_some() {
            return (self, None);
        }
        let Some(plates) = self.plate_devices.clone() else { return (self, None) };
        let lazy: Vec<usize> = (0..self.manifest.len()).filter(|i| !plates.contains(i)).collect();
        self.kept.clear();
        self.analog.clear();
        self.indices = plates;
        self.selected = Some(self.indices.clone());
        let deferred = if lazy.is_empty() {
            None
        } else {
            Some(Self {
                path: self.path.clone(),
                before: self.before,
                grows: self.grows,
                map: self.map.clone(),
                kept: Vec::new(),
                analog: Vec::new(),
                records: Arc::clone(&self.records),
                manifest: self.manifest.clone(),
                device_records: Arc::clone(&self.device_records),
                plate_devices: self.plate_devices.clone(),
                fallback: None,
                selected: Some(lazy.clone()),
                indices: lazy,
            })
        };
        (self, deferred)
    }

    /// Full job: identical to `read_take`. Split job: all device headers in original order,
    /// with samples filled only for `device_indices()` (all other channels remain empty).
    /// Split jobs return an error for invalid device data; full jobs keep legacy warnings.
    pub fn run(self) -> Result<(Vec<AnalogDevice>, Option<String>)> {
        unchanged(&self.path, self.before, self.grows)?;
        let file = File::open(&self.path)?;
        if let Some(indices) = &self.selected {
            let mut manifest = self.manifest;
            for &di in indices {
                let records = &self.device_records[di];
                let mut stream = CHANNELS_HEADER.to_vec();
                stream.extend_from_slice(&(records.len() as u32).to_le_bytes());
                for &ri in records {
                    let record = &self.records[ri];
                    stream.extend(self.map.read(&file, record.start, record.end)?);
                }
                let (mut decoded, warning) = decode_analog(&stream);
                if warning.is_some() || decoded.len() != 1 {
                    unchanged(&self.path, self.before, self.grows)?;
                    return err(warning.unwrap_or_else(|| "analog device decode count changed".into()));
                }
                manifest[di] = decoded.remove(0);
            }
            unchanged(&self.path, self.before, self.grows)?;
            return Ok((manifest, None));
        }
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
    // Validate structural device metadata before reading any channel payload. A registry
    // error must never turn device channels into marker data through a name-only guess.
    let typed = super::nodes::analog_device_nodes(nodes)?;
    let device_uids = typed.iter().map(|n| n.uid).collect();
    let tail_plan = marker_tail_plan(nodes, &typed)?;
    let Some(walked) = walk_channels(file, &map, &device_uids, tail_plan.as_ref())? else {
        return Ok(None);
    };
    let t_channels = ms(t1);
    let bulk_ms = walked.bulk_ms;

    // Stage-1 stream: the header with the non-analog record count, then those records in order.
    let t2 = Instant::now();
    let keep: Vec<(u64, u64)> = walked.records.iter().filter(|r| !r.analog).map(|r| (r.start, r.end)).collect();
    let mut stream = Vec::with_capacity(10 + keep.iter().map(|(a, b)| (b - a) as usize).sum::<usize>());
    stream.extend_from_slice(&CHANNELS_HEADER);
    stream.extend_from_slice(&(keep.len() as u32).to_le_bytes());
    for &(a, b) in &keep {
        let Some((off, bytes)) = walked.kept.iter().find(|(o, v)| *o <= a && b <= *o + v.len() as u64) else {
            return Ok(None);
        };
        stream.extend_from_slice(&bytes[(a - off) as usize..(b - off) as usize]);
    }
    let mut take = decode_markers(path, len as usize, nodes, &stream, meta, sha)?;
    unchanged(path, before, grows)?;
    let t_decode = ms(t2);

    let analog = walked.records.iter().filter(|r| r.analog).map(|r| (r.start, r.end)).collect();
    let (manifest, device_records) = device_manifest(&walked.records).unwrap_or_default();
    let (plate_devices, fallback) = match selective_layout_for_nodes(&typed, &walked.records, &device_records) {
        Ok(plates) => (Some(plates), None),
        Err(e) => (None, Some(e.0)),
    };
    // Headers alone are safe to publish; full `run` still reproduces the original decode.
    take.device_sampling = if fallback.is_none() { device_records.iter().map(|rows| {
        let uid = walked.records[rows[0]].uid;
        if rows.iter().any(|&ri| walked.records[ri].uid != uid) { return None; }
        typed.iter().find(|node| node.uid == uid).and_then(|node| node.sampling)
            .filter(|sampling| rows.iter().all(|&ri| sampling.samples_per_frame <= walked.records[ri].dim))
    }).collect() } else { vec![None; manifest.len()] };
    take.analog = manifest.clone();
    let indices = (0..manifest.len()).collect();
    let job = AnalogJob {
        path: path.to_path_buf(), before, grows, map, kept: walked.kept, analog,
        records: Arc::new(walked.records), manifest,
        device_records: Arc::new(device_records), plate_devices, fallback,
        selected: None, indices,
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

#[derive(Clone, Debug)]
struct Record {
    start: u64,
    end: u64,
    #[cfg_attr(not(test), allow(dead_code))]
    data_start: u64,
    analog: bool,
    name: String,
    kind: String,
    uid: Uid,
    dim: usize,
}

struct Walked {
    records: Vec<Record>,
    kept: Vec<(u64, Vec<u8>)>,
    bulk_ms: f64,
}

fn device_manifest(records: &[Record]) -> Result<(Vec<AnalogDevice>, Vec<Vec<usize>>)> {
    let mut devices: Vec<AnalogDevice> = Vec::new();
    let mut device_records: Vec<Vec<usize>> = Vec::new();
    for (ri, r) in records.iter().enumerate().filter(|(_, r)| r.analog) {
        let suffix = r.name.strip_prefix("AnalogChannel").unwrap_or("");
        let index = super::metadata::python_int(suffix)?;
        if index == 0 {
            devices.push(AnalogDevice { channels: Vec::new() });
            device_records.push(Vec::new());
        }
        let Some(d) = devices.last_mut() else { return err("analog channel before AnalogChannel0") };
        // A single device's channels must have unambiguous consecutive structural indices.
        if index != d.channels.len() as i64 {
            return err("ambiguous analog channel numbering");
        }
        d.channels.push(AnalogChannel {
            name: r.name.clone(),
            kind: r.kind.clone(),
            dim: r.dim,
            frames: Vec::new(),
            values: Vec::new(),
        });
        device_records.last_mut().unwrap().push(ri);
    }
    Ok((devices, device_records))
}

#[cfg(test)]
fn selective_layout(nodes: &[u8], records: &[Record], devices: &[Vec<usize>]) -> Result<Vec<usize>> {
    selective_layout_for_nodes(&super::nodes::analog_device_nodes(nodes)?, records, devices)
}

fn selective_layout_for_nodes(typed: &[super::nodes::DeviceNode], records: &[Record], devices: &[Vec<usize>]) -> Result<Vec<usize>> {
    if records.iter().any(|r| typed.iter().any(|n| n.uid == r.uid) && !r.name.starts_with("AnalogChannel")) {
        return err("unsupported structural device channel property; full analog read required");
    }
    if records.iter().any(|r| r.analog) && devices.is_empty() {
        return err("analog device headers cannot be grouped unambiguously");
    }
    let mut plate_devices = vec![usize::MAX; typed.iter().filter(|n| n.forceplate).count()];
    let mut used = std::collections::HashSet::new();
    for (di, rows) in devices.iter().enumerate() {
        let uid = records[rows[0]].uid;
        if rows.iter().any(|&ri| records[ri].uid != uid) || !used.insert(uid) {
            return err("inconsistent or reused analog channel device UID");
        }
        let Some(node) = typed.iter().find(|n| n.uid == uid) else {
            return err("analog channel UID has no recognized structural device node");
        };
        if node.channel_count.is_some_and(|n| n != rows.len()) {
            return err("structural device channel count disagrees with channel records");
        }
        if let Some(pi) = node.plate {
            plate_devices[pi] = di;
        }
    }
    if plate_devices.contains(&usize::MAX) {
        return err("forceplate geometry has no analog channel device UID match");
    }
    Ok(plate_devices)
}

/// Independent node metadata permits one bulk read after every device's declared channels
/// has been seen and the remaining record count is exactly the remaining marker UID count.
struct MarkerTailPlan {
    channels: std::collections::HashMap<Uid, usize>,
    markers: std::collections::HashSet<Uid>,
}

fn marker_tail_plan(nodes: &[u8], devices: &[super::nodes::DeviceNode]) -> Result<Option<MarkerTailPlan>> {
    let Some(channels) = devices.iter().map(|d| d.channel_count.map(|n| (d.uid,n))).collect::<Option<_>>() else { return Ok(None) };
    // This proof needs framed marker UIDs only. Decode/validate JSON and classify each
    // marker once, later in decode_markers, rather than doing it twice before readiness.
    let mut records=Vec::new();
    for sig in super::nodes::MARKER_SIGNATURES {
        records.extend(super::container::find_all(nodes,sig,0).into_iter().map(|at|(at,sig.len())));
    }
    records.sort_unstable();records.dedup();
    let mut markers=std::collections::HashSet::new();
    for &(at,sig_len) in &records {
        let chars=u16_at(nodes,at+sig_len)? as usize;
        let Some(end)=(at+sig_len+2).checked_add(chars*2) else {return Ok(None)};
        let Some(tail)=nodes.get(end..end+35) else {return Ok(None)};
        if tail[..19]!=super::nodes::MARKER_PREFIX {return Ok(None);}
        if !markers.insert(super::container::uid_from(&tail[19..35])) {return err("duplicate marker UID prevents proving a marker-only suffix");}
    }
    Ok((!markers.is_empty()).then_some(MarkerTailPlan { channels, markers }))
}

fn channel_record(pos: u64, bytes: &[u8], header: usize, data: u64, legacy_analog: bool, devices: &std::collections::HashSet<Uid>) -> Result<Record> {
    let kind_len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    let cur = 2 + kind_len;
    let chars = u16::from_le_bytes([bytes[cur + 1], bytes[cur + 2]]) as usize;
    let uid_at = cur + 3 + chars * 2;
    let kind = String::from_utf8(bytes[2..cur].to_vec()).map_err(|e| super::TakError(e.to_string()))?;
    let name = utf16le_strict(&bytes[cur + 3..uid_at])?;
    let uid = super::container::uid_from(&bytes[uid_at..uid_at + 16]);
    let dim = channel_dim(&kind).unwrap();
    Ok(Record { start:pos, end:pos + header as u64 + data, data_start:pos + header as u64,
        analog:legacy_analog || devices.contains(&uid), name, kind, uid, dim })
}

fn validate_marker_tail(bytes: &[u8], start: u64, count: usize, mut markers: std::collections::HashSet<Uid>, devices: &std::collections::HashSet<Uid>) -> Result<Vec<Record>> {
    let mut records = Vec::with_capacity(count);
    let mut pos = 0usize;
    for _ in 0..count {
        let Some(remaining) = bytes.get(pos..) else { return err("marker suffix record extends past stream") };
        let Head::Ok { header, data, analog } = parse_head(remaining) else { return err("invalid marker suffix record header") };
        let r = channel_record(start + pos as u64, remaining, header, data, analog, devices)?;
        if r.analog || r.kind != "Vector3fChannel" || r.name != "Translation" || !markers.remove(&r.uid) {
            return err("marker suffix disagrees with device counts or marker UID inventory; full analog read required");
        }
        pos = usize::try_from(r.end - start).map_err(|_| super::TakError("marker suffix length overflow".into()))?;
        if pos > bytes.len() { return err("truncated marker suffix samples") }
        records.push(r);
    }
    if pos != bytes.len() || !markers.is_empty() { return err("marker suffix record count or marker UID inventory mismatch") }
    Ok(records)
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
fn walk_channels(file: &File, map: &StreamMap, device_uids: &std::collections::HashSet<Uid>, tail_plan: Option<&MarkerTailPlan>) -> Result<Option<Walked>> {
    walk_channel_records(file, map, device_uids, tail_plan, true)
}

/// Shared header walk. Post-save refresh skips every sample payload, including marker
/// samples, so it cannot use the marker-only suffix optimization or retained marker runs.
fn walk_channel_records(file: &File, map: &StreamMap, device_uids: &std::collections::HashSet<Uid>, tail_plan: Option<&MarkerTailPlan>, marker_payloads: bool) -> Result<Option<Walked>> {
    let len = map.len;
    if len < 10 {
        return Ok(None);
    }
    let header = map.read(file, 0, 10)?;
    if header[..6] != CHANNELS_HEADER {
        return Ok(None);
    }
    let count = u32::from_le_bytes([header[6], header[7], header[8], header[9]]) as usize;
    let mut records = Vec::new();
    let mut kept = vec![(0, header)];
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    let mut pos = 10u64;
    let mut bulk_ms = 0.0;
    let mut device_seen: std::collections::HashMap<Uid, usize> = std::collections::HashMap::new();
    let mut marker_seen = std::collections::HashSet::new();
    let mut counts_valid = true;
    // Read only exactly requested header bytes. A fixed 4 KiB probe can include samples
    // even for the very first analog channel, and a bulk tail may contain later analogs.
    while records.len() < count {
        if len - pos < MIN_HEADER { return err("channel record is shorter than every supported header"); }
        // The minimum across supported structural kinds is safe even for an empty device
        // property. Unlike a 4 KiB probe it cannot include a supported channel's samples.
        let mut bytes = map.read(file, pos, pos + MIN_HEADER)?;
        let (header, data, analog) = loop {
            match parse_head(&bytes) {
                Head::Need(n) if pos + n as u64 <= len => {
                    let old = bytes.len();
                    bytes.resize(n, 0);
                    let more = map.read(file, pos + old as u64, pos + n as u64)?;
                    bytes[old..].copy_from_slice(&more);
                }
                Head::Ok { header, data, analog } => break (header, data, analog),
                Head::Bad => return err("unsupported structural channel type or malformed channel record header"),
                _ => return err("truncated channel record header"),
            }
        };
        let end = pos + header as u64 + data;
        if end > len { return Ok(None); }
        let record = channel_record(pos, &bytes, header, data, analog, device_uids)?;
        if let Some(plan) = tail_plan.filter(|_| marker_payloads) {
            let counts_complete = counts_valid && plan.channels.iter().all(|(uid,n)| device_seen.get(uid).copied().unwrap_or(0) == *n);
            if counts_complete && record.kind == "Vector3fChannel" && record.name == "Translation"
                && !record.analog && plan.markers.contains(&record.uid) && !marker_seen.contains(&record.uid)
                && count - records.len() == plan.markers.len() - marker_seen.len() {
                let remaining_markers = plan.markers.difference(&marker_seen).copied().collect();
                let t = Instant::now();
                let tail = map.read(file, pos, len)?;
                bulk_ms += ms(t);
                let suffix = validate_marker_tail(&tail,pos,count-records.len(),remaining_markers,device_uids)?;
                records.extend(suffix);
                kept.push((pos,tail));
                pos = len;
                break;
            }
            if record.analog {
                let seen = device_seen.entry(record.uid).or_default();
                if !plan.channels.contains_key(&record.uid) || record.name != format!("AnalogChannel{seen}") { counts_valid = false; }
                *seen += 1;
            } else if plan.markers.contains(&record.uid) && record.kind == "Vector3fChannel" && record.name == "Translation" {
                if !marker_seen.insert(record.uid) { counts_valid = false; }
            }
        }
        if marker_payloads && !record.analog {
            match ranges.last_mut() {
                Some((_, b)) if *b == pos => *b = end,
                _ => ranges.push((pos, end)),
            }
        }
        records.push(record);
        pos = end;
    }
    if pos != len { return Ok(None); }
    // Non-analog runs before the counted/validated suffix retain exact boundary proofs.
    // Without independent device counts, a large marker run never permits a bulk tail.
    for (a, b) in ranges {
        let t = Instant::now();
        kept.push((a, map.read(file, a, b)?));
        bulk_ms += ms(t);
    }
    Ok(Some(Walked { records, kept, bulk_ms }))
}

/// Sparse original channel stream for editing. Unopened non-forceplate samples remain zero
/// placeholders; callers must copy those ranges from the source rather than serialize zeros.
pub(crate) fn edit_channels(file: &File, map: &StreamMap, nodes: &[u8]) -> Result<(Vec<u8>, Vec<(u64,u64)>)> {
    // Editing is stricter than the display fallback: only proven marker/plate samples
    // may be read. Unknown device/asset payloads are opaque passthrough as well.
    let typed = super::nodes::analog_device_nodes(nodes).unwrap_or_default();
    let markers: std::collections::HashSet<_> = super::nodes::property_records(nodes, &super::nodes::MARKER_SIGNATURES)?.iter().map(|r|r.uid).collect();
    let devices = typed.iter().map(|n| n.uid).collect();
    let head = map.read(file, 0, 10)?;
    if head[..6] != CHANNELS_HEADER {return err("unsupported editable channel header");}
    let count = u32::from_le_bytes(head[6..10].try_into().unwrap());
    let mut bytes = vec![0; map.len as usize]; bytes[..10].copy_from_slice(&head);
    let mut deferred = Vec::new(); let mut pos=10;
    for _ in 0..count {
        if pos + MIN_HEADER > map.len {return err("short editable channel header");}
        let mut h=map.read(file,pos,pos+MIN_HEADER)?;
        let (header,data,analog)=loop {match parse_head(&h) {
            Head::Need(n) if pos+n as u64<=map.len => {let old=h.len();h.extend(map.read(file,pos+old as u64,pos+n as u64)?);},
            Head::Ok {header,data,analog} => break (header,data,analog),
            _ => return err("unsupported editable channel layout"),
        }};
        let rec=channel_record(pos,&h,header,data,analog,&devices)?;
        if rec.end>map.len {return err("short editable channel payload");}
        bytes[pos as usize..rec.data_start as usize].copy_from_slice(&h);
        let marker=markers.contains(&rec.uid) && rec.kind=="Vector3fChannel" && rec.name=="Translation";
        let plate=typed.iter().any(|n|n.uid==rec.uid && n.forceplate);
        if marker || plate {
            let data=map.read(file,rec.data_start,rec.end)?;
            bytes[rec.data_start as usize..rec.end as usize].copy_from_slice(&data);
        }else if rec.data_start<rec.end {deferred.push((rec.data_start,rec.end));}
        pos=rec.end;
    }
    if pos!=map.len {return err("editable channel record count mismatch");}
    Ok((bytes, deferred))
}

/// Where a stream lies in the file: a regular container stream = its sectors in order (from
/// `Container::stream_chain`); a `.takm` stream = one piece starting at `flat`.
#[derive(Clone)]
pub(crate) struct StreamMap {
    pub(crate) sector: u64,
    pub(crate) chain: Vec<u32>,
    pub(crate) len: u64,
    /// `.takm`: file offset of the stream's first byte (`sector` / `chain` unused).
    pub(crate) flat: Option<u64>,
}

impl StreamMap {
    /// Stream bytes [a, b) as file pieces (file offset, length): consecutive sectors merged,
    /// pieces at most `CHUNK` long.
    pub(crate) fn pieces(&self, a: u64, b: u64) -> Vec<(u64, usize)> {
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
    pub(crate) fn read(&self, file: &File, a: u64, b: u64) -> Result<Vec<u8>> {
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
    #[cfg(test)]
    read_trace::record(file, off, buf.len());
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

/// Read jobs on the caller plus available helpers; another file never makes this wait.
fn read_parallel(file: &File, jobs: Vec<(u64, &mut [u8])>) -> Result<()> {
    if jobs.len() <= 1 {
        for (off, buf) in jobs {
            read_exact_at(file, buf, off)?;
        }
        return Ok(());
    }
    let helpers = crate::par::io_helpers(THREADS.min(jobs.len()) - 1);
    if helpers.len() == 0 {
        for (off, buf) in jobs { read_exact_at(file, buf, off)?; }
        return Ok(());
    }
    let queue = Mutex::new(jobs.into_iter());
    let failed = Mutex::new(None);
    let work = || loop {
        let job = queue.lock().map(|mut q| q.next()).unwrap_or(None);
        let Some((off, buf)) = job else { break };
        if let Err(e) = read_exact_at(file, buf, off) {
            if let Ok(mut f) = failed.lock() { f.get_or_insert(e); }
            break;
        }
    };
    std::thread::scope(|s| {
        for _ in 0..helpers.len() { s.spawn(&work); }
        work();
    });
    drop(helpers);
    match failed.into_inner().ok().flatten() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Test instrumentation records actual positional-read requests, including parallel reads.
/// Registering by native file identity lets handles/stages share a trace without conflating
/// separate copies that happen to have identical lengths and modification timestamps.
#[cfg(test)]
pub(crate) mod read_trace {
    use super::*;
    type Key = (u64, u64);
    type Ranges = Arc<Mutex<Vec<(u64, u64)>>>;
    static ACTIVE: Mutex<Vec<(Key, Ranges)>> = Mutex::new(Vec::new());
    #[cfg(unix)]
    fn identity(file: &File) -> std::io::Result<Key> {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    fn identity(file: &File) -> std::io::Result<Key> {
        use std::os::windows::io::AsRawHandle;
        // std's volume/file-index accessors are unstable. The native structure consists of
        // 13 DWORDs: attributes, 3 FILETIMEs, volume, size high/low, links, index high/low.
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut [u32; 13]) -> i32;
        }
        let mut info = [0u32; 13];
        // SAFETY: this open File owns a valid handle; info has exactly the documented layout
        // and remains writable for the duration of this synchronous call.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((info[7] as u64, (info[11] as u64) << 32 | info[12] as u64))
    }
    pub fn start(file: &File) -> Ranges {
        let ranges = Arc::new(Mutex::new(Vec::new()));
        ACTIVE.lock().unwrap().push((identity(file).expect("trace file identity"), ranges.clone()));
        ranges
    }
    pub fn record(file: &File, off: u64, n: usize) {
        let key = identity(file).expect("trace read file identity");
        for (_, ranges) in ACTIVE.lock().unwrap().iter().filter(|(k, _)| *k == key) {
            ranges.lock().unwrap().push((off, off + n as u64));
        }
    }
    pub fn stop(ranges: &Ranges) {
        ACTIVE.lock().unwrap().retain(|(_, r)| !Arc::ptr_eq(r, ranges));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_reads_preserve_offsets_and_report_short_input() {
        // Read a stable small source file: this regression needs no writable fixture or
        // version-specific temporary-directory helper.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let bytes = std::fs::read(&path).unwrap();
        let file = File::open(&path).unwrap();
        let mut buffers = vec![[0u8; 16]; 16];
        let jobs = buffers.iter_mut().enumerate().map(|(i, out)| ((i * 3) as u64, out.as_mut_slice())).collect();
        read_parallel(&file, jobs).unwrap();
        for (i, actual) in buffers.iter().enumerate() {
            assert_eq!(actual.as_slice(), &bytes[i * 3..i * 3 + 16]);
        }
        let (mut valid, mut past_end) = ([0u8; 4], [0u8; 1]);
        assert!(read_parallel(&file, vec![(0, &mut valid), (bytes.len() as u64, &mut past_end)]).is_err());
        drop(file);
    }

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
        let _temp = crate::test_support::TempDir::new("tak_fast_read_test");
        let dir = _temp.join(format!("tak_fast_read_test_{}", std::process::id()));
        std::fs::write(&dir, &file_bytes).unwrap();
        let file = File::open(&dir).unwrap();
        let m = map(chain, stream.len() as u64);
        assert_eq!(m.read(&file, 0, stream.len() as u64).unwrap(), stream);
        let w = walk_channels(&file, &m, &std::collections::HashSet::new(), None).unwrap().expect("walked");
        let flags: Vec<bool> = w.records.iter().map(|r| r.analog).collect();
        assert_eq!(flags, [false, true, true, false, false, false]);
        for r in &w.records {
            let (a, b, analog) = (r.start, r.end, r.analog);
            let inside = w.kept.iter().any(|(o, v)| *o <= a && b <= *o + v.len() as u64);
            assert_eq!(inside, !analog, "record {a}..{b}");
            if let Some((o, v)) = w.kept.iter().find(|(o, v)| *o <= a && b <= *o + v.len() as u64) {
                assert_eq!(&v[(a - o) as usize..(b - o) as usize], &stream[a as usize..b as usize]);
            }
        }
        drop(file);
        let _ = std::fs::remove_file(&dir);
    }
    fn device_node(uid: Uid, kind: u32, name: &str) -> Vec<u8> {
        let mut props = vec![
            serde_json::json!({"name":"NodeName", "value":name}),
            serde_json::json!({"name":"DeviceNode_DeviceType", "value":kind.to_string()}),
        ];
        if kind == 2 {
            for i in 0..4 {
                props.push(serde_json::json!({"name":format!("DeviceNode_Corner {i}"), "value":format!("{}, 0, {}", i % 2, i / 2)}));
            }
        }
        let text = format!("{{\n    \"properties\": {}\n}}", serde_json::to_string(&props).unwrap());
        let text = super::super::nodes::utf16(&text);
        let kind_name = if kind == 2 { "ForcePlate" } else { "class MoCapCore::cDeviceNode<float,16>" };
        let mut out = (kind_name.len() as u16).to_le_bytes().to_vec();
        out.extend(kind_name.as_bytes());
        out.extend(((text.len() / 2) as u16).to_le_bytes());
        out.extend(text);
        out.extend([0u8; 19]);
        out.extend(uid);
        out.extend([0u8; 33]);
        out
    }

    fn with_uid(mut bytes: Vec<u8>, uid: Uid) -> Vec<u8> {
        let kind = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
        let at = kind + 2;
        let chars = u16::from_le_bytes([bytes[at + 1], bytes[at + 2]]) as usize;
        let at = at + 3 + 2 * chars;
        bytes[at..at + 16].copy_from_slice(&uid);
        bytes
    }

    fn overlap(reads: &[(u64, u64)], ranges: &[(u64, u64)]) -> u64 {
        reads.iter().map(|&(a, b)| ranges.iter().map(|&(c, d)| b.min(d).saturating_sub(a.max(c))).sum::<u64>()).sum()
    }

    #[test]
    fn exact_headers_and_proven_marker_ranges_never_read_analog_samples() {
        // EMG comes first and again after >64 KiB of marker bytes. Tiny analog records
        // catch both the former 4 KiB probe and the unexamined bulk-tail optimization.
        let recs = [
            with_uid(record("FloatChannel", "AnalogChannel0", 1, 1), [1; 16]),
            record("Vector3fChannel", "Translation", 3, 6000),
            with_uid(record("FloatChannel", "AnalogChannel0", 1, 2), [2; 16]),
            with_uid(record("FloatChannel", "AnalogChannel0", 1, 3), [3; 16]),
        ];
        let mut stream = CHANNELS_HEADER.to_vec();
        stream.extend((recs.len() as u32).to_le_bytes());
        for r in &recs {
            stream.extend(r);
        }
        let _temp = crate::test_support::TempDir::new("tak_selective_interleave");
        let path = _temp.join(format!("tak_selective_interleave_{}", std::process::id()));
        std::fs::write(&path, &stream).unwrap();
        let file = File::open(&path).unwrap();
        let trace = read_trace::start(&file);
        let map = StreamMap { sector: 0, chain: Vec::new(), len: stream.len() as u64, flat: Some(0) };
        let walked = walk_channels(&file, &map, &std::collections::HashSet::new(), None).unwrap().unwrap();
        let samples: Vec<_> = walked.records.iter().filter(|r| r.analog).map(|r| (r.data_start, r.end)).collect();
        assert_eq!(overlap(&trace.lock().unwrap(), &samples), 0);
        let (manifest, groups) = device_manifest(&walked.records).unwrap();
        // Names actively contradict types; plate is the middle analog device.
        let mut nodes = device_node([1; 16], 1, "Force Plate deceptive name");
        nodes.extend(device_node([2; 16], 2, "EMG deceptive name"));
        nodes.extend(device_node([3; 16], 1, "another ForcePlate"));
        let plates = selective_layout(&nodes, &walked.records, &groups).unwrap();
        assert_eq!(plates, [1]);
        let before = stat_key(&file.metadata().unwrap());
        let job = AnalogJob {
            path: path.clone(),
            before,
            grows: false,
            map,
            kept: walked.kept,
            analog: walked.records.iter().filter(|r| r.analog).map(|r| (r.start, r.end)).collect(),
            records: Arc::new(walked.records),
            manifest,
            device_records: Arc::new(groups),
            plate_devices: Some(plates),
            fallback: None,
            selected: None,
            indices: vec![0, 1, 2],
        };
        let (startup, lazy) = job.split();
        assert_eq!(startup.device_indices(), [1]);
        assert_eq!(lazy.as_ref().unwrap().device_indices(), [0, 2]);
        let (initial, warning) = startup.run().unwrap();
        assert!(warning.is_none());
        assert!(initial[0].channels[0].frames.is_empty());
        assert_eq!(initial[1].channels[0].frames.len(), 2);
        assert!(initial[2].channels[0].frames.is_empty());
        assert_eq!(overlap(&trace.lock().unwrap(), &[samples[0], samples[2]]), 0);
        trace.lock().unwrap().clear();
        let (loaded, warning) = lazy.unwrap().run().unwrap();
        assert!(warning.is_none());
        assert_eq!(loaded[0].channels[0].frames.len(), 1);
        assert!(loaded[1].channels[0].frames.is_empty());
        assert_eq!(loaded[2].channels[0].frames.len(), 3);
        assert_eq!(overlap(&trace.lock().unwrap(), &[samples[0], samples[2]]), 32);
        assert_eq!(overlap(&trace.lock().unwrap(), &[samples[1]]), 0);
        read_trace::stop(&trace);
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    fn assert_same_analog(a: &[AnalogDevice], b: &[AnalogDevice]) {
        assert_eq!(a.len(), b.len());
        for (a, b) in a.iter().zip(b) {
            assert_eq!(a.channels.len(), b.channels.len());
            for (a, b) in a.channels.iter().zip(&b.channels) {
                assert_eq!((&a.name, &a.kind, a.dim, &a.frames), (&b.name, &b.kind, b.dim, &b.frames));
                assert_eq!(
                    a.values.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                    b.values.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn real_tak_selective_read_and_unsplit_dump_match_full_reader() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../input");
        let mut paths = vec![
            root.join("raw/000000_S01_perturb_20_001.tak"),
            root.join("raw/000000_S01_perturb_20_004.tak"),
            root.join("static/000000_S01_static.tak"),
        ];
        paths.push(PathBuf::from("/tmp/tak_0328_bench_wsl/gait_1.tak"));
        if let Some(path) = std::env::var_os("TAK_SELECTIVE_TEST_FILE") { paths.push(PathBuf::from(path)); }
        for path in paths {
            let relative = path.display();
            if !path.exists() {
                continue;
            } // Optional local reference data is not distributed.
            let full = super::super::read_take(&path).unwrap();
            let read = read_take_fast(&path, true).unwrap();
            assert_eq!(read.take.device_sampling, full.device_sampling, "sampling descriptors must agree: {relative}");
            for sampling in read.take.device_sampling.iter().flatten() {
                assert!((sampling.rate_hz - sampling.samples_per_frame as f64 * read.take.frame_rate).abs() <= sampling.rate_hz * 1e-6);
            }
            let job = read.analog.expect("reference must use marker-first path");
            assert!(job.fallback_reason().is_none(), "{relative}: {:?}", job.fallback_reason());
            let plate_map = job.plate_devices().unwrap().to_vec();
            let all = job.run().unwrap();
            assert!(all.1.is_none());
            assert_same_analog(&full.analog, &all.0);
            let mut complete = read.take;
            complete.analog = all.0;
            assert_eq!(crate::dump::to_json(&full), crate::dump::to_json(&complete), "dump parity: {relative}");
            let file = File::open(&path).unwrap();
            let trace = read_trace::start(&file);
            let read = read_take_fast(&path, false).unwrap();
            let job = read.analog.unwrap();
            assert_eq!(read.take.analog.len(), full.analog.len());
            assert!(read
                .take
                .analog
                .iter()
                .flat_map(|d| &d.channels)
                .all(|c| c.frames.is_empty() && c.values.is_empty()));
            let nonplate_samples: Vec<(u64, u64)> = job
                .device_records
                .iter()
                .enumerate()
                .filter(|(di, _)| !plate_map.contains(di))
                .flat_map(|(_, rows)| rows.iter())
                .flat_map(|&ri| {
                    let r = &job.records[ri];
                    job.map.pieces(r.data_start, r.end).into_iter().map(|(off, n)| (off, off + n as u64))
                })
                .collect();
            let (startup, lazy) = job.split();
            let (mut initial, warning) = startup.run().unwrap();
            assert!(warning.is_none());
            assert_eq!(overlap(&trace.lock().unwrap(), &nonplate_samples), 0, "{relative}");
            let lazy = lazy.expect("reference has non-forceplate analog");
            let indices = lazy.device_indices().to_vec();
            let (mut loaded, warning) = lazy.run().unwrap();
            assert!(warning.is_none());
            for i in indices {
                initial[i] = std::mem::replace(&mut loaded[i], AnalogDevice { channels: vec![] });
            }
            assert_same_analog(&full.analog, &initial);
            read_trace::stop(&trace);
            eprintln!(
                "selective reference {relative}: {} devices, {} plates, 0 nonplate startup sample bytes",
                full.analog.len(),
                plate_map.len()
            );
        }
    }

    #[test]
    fn structural_ambiguity_falls_back_without_guessing() {
        let r = Record {
            start: 0,
            end: 0,
            data_start: 0,
            analog: true,
            name: "AnalogChannel0".into(),
            kind: "FloatChannel".into(),
            uid: [1; 16],
            dim: 1,
        };
        let records = [r];
        let groups = vec![vec![0]];
        assert!(selective_layout(&[], &records, &groups).is_err());
        let mut duplicate = device_node([1; 16], 1, "a");
        duplicate.extend(device_node([1; 16], 1, "b"));
        assert!(selective_layout(&duplicate, &records, &groups).is_err());
        assert!(selective_layout(&device_node([1; 16], 9, "ForcePlate"), &records, &groups).is_err());
        assert!(selective_layout(&device_node([2; 16], 1, "a"), &records, &groups).is_err());
        let mut reused = records.to_vec();
        reused.push(reused[0].clone());
        assert!(selective_layout(&device_node([1; 16], 1, "a"), &reused, &[vec![0], vec![1]]).is_err());
    }

    fn test_job(path: &Path, map: StreamMap, nodes: &[u8], grows: bool) -> AnalogJob {
        let file = File::open(path).unwrap();
        let typed = super::super::nodes::analog_device_nodes(nodes).unwrap();
        let device_uids = typed.iter().map(|n| n.uid).collect();
        let walked = walk_channels(&file, &map, &device_uids, None).unwrap().unwrap();
        let (manifest, groups) = device_manifest(&walked.records).unwrap_or_default();
        let (plate_devices, fallback) = match selective_layout(nodes, &walked.records, &groups) {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(e.0)),
        };
        let indices = (0..manifest.len()).collect();
        AnalogJob {
            path: path.to_path_buf(),
            before: stat_key(&file.metadata().unwrap()),
            grows,
            map,
            kept: walked.kept,
            analog: walked.records.iter().filter(|r| r.analog).map(|r| (r.start, r.end)).collect(),
            records: Arc::new(walked.records),
            manifest,
            device_records: Arc::new(groups),
            plate_devices,
            fallback,
            selected: None,
            indices,
        }
    }

    fn two_device_stream() -> Vec<u8> {
        let mut stream = CHANNELS_HEADER.to_vec();
        stream.extend(2u32.to_le_bytes());
        stream.extend(with_uid(record("FloatChannel", "AnalogChannel0", 1, 2), [1; 16]));
        stream.extend(with_uid(record("FloatChannel", "AnalogChannel0", 1, 3), [2; 16]));
        stream
    }

    fn refresh_channels(marker_samples: usize) -> Vec<u8> {
        let mut stream = CHANNELS_HEADER.to_vec();
        stream.extend(3u32.to_le_bytes());
        stream.extend(with_uid(record("FloatChannel", "AnalogChannel0", 1, 2), [1; 16]));
        stream.extend(with_uid(record("Vector3fChannel", "Translation", 3, marker_samples), [3; 16]));
        stream.extend(with_uid(record("FloatChannel", "AnalogChannel0", 1, 3), [2; 16]));
        stream
    }

    fn refresh_nodes() -> Vec<u8> {
        let mut nodes = device_node([1; 16], 2, "EMG misleading plate name");
        nodes.extend(device_node([2; 16], 1, "ForcePlate misleading device name"));
        nodes
    }

    /// A sealed stream-table fixture permits changes in stream order/size without reading
    /// any original TAK. Its channel bytes use the production decoder's record format.
    fn write_refresh_takm(path: &Path, streams: &[(&str, &[u8])]) -> crate::takm::Index {
        use sha2::{Digest, Sha256};
        let mut bytes = vec![0u8; crate::takm::HEADER_LEN as usize];
        let mut table_streams = Vec::new();
        for (index, (name, data)) in streams.iter().enumerate() {
            table_streams.push(serde_json::json!({"name":name, "index":index, "offset":bytes.len(), "size":data.len()}));
            bytes.extend_from_slice(data);
        }
        let table_at = bytes.len() as u64;
        let table = serde_json::to_vec(&serde_json::json!({
            "streams":table_streams, "dropped":[],
            "source":{"name":"fixture.tak", "size":0, "sha256":"0".repeat(64)},
            "made_by":"metadata refresh test", "made_at":"2026-11-16T00:00:00Z"
        })).unwrap();
        bytes.extend_from_slice(&table);
        let fixed = bytes.len() as u64;
        bytes[..8].copy_from_slice(&crate::takm::MAGIC);
        bytes[8..12].copy_from_slice(&crate::takm::LAYOUT.to_le_bytes());
        bytes[16..24].copy_from_slice(&table_at.to_le_bytes());
        bytes[24..32].copy_from_slice(&(table.len() as u64).to_le_bytes());
        bytes[32..40].copy_from_slice(&fixed.to_le_bytes());
        let seal = Sha256::digest(&bytes);
        bytes.extend_from_slice(&seal);
        std::fs::write(path, bytes).unwrap();
        crate::takm::read_index_path(path).unwrap()
    }

    fn refresh_sample_ranges(channels: &[u8], base: u64) -> Vec<(u64, u64)> {
        super::super::channels::iter_channel_inventory(channels).unwrap().iter()
            .map(|record| (base + record.data_start as u64, base + record.data_end as u64)).collect()
    }

    #[test]
    fn post_save_metadata_refresh_skips_marker_plate_and_emg_sample_bytes() {
        use std::io::Write;
        let _temp = crate::test_support::TempDir::new("tak_refresh_metadata");
        let path = _temp.join(format!("tak_refresh_metadata_{}.takm", std::process::id()));
        let nodes = refresh_nodes();
        let channels = refresh_channels(6000);
        let index = write_refresh_takm(&path, &[("Nodes.dat", &nodes), ("Channels.dat", &channels)]);
        let samples = refresh_sample_ranges(&channels, index.unique("Channels.dat").unwrap().offset);
        let file = File::open(&path).unwrap();
        let trace = read_trace::start(&file);
        let lazy = deferred_job_after_save(&path).unwrap().unwrap();
        assert_eq!(lazy.device_indices(), [1]);
        assert!(lazy.fallback_reason().is_none());
        assert_eq!(overlap(&trace.lock().unwrap(), &samples), 0);
        assert!(trace.lock().unwrap().iter().map(|(start, end)| end - start).sum::<u64>() < 20_000);
        trace.lock().unwrap().clear();
        // Appended Comments do not invalidate the refreshed stream positions.
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"Comment tail\r\n").unwrap();
        let (loaded, warning) = lazy.run().unwrap();
        assert!(warning.is_none());
        assert!(loaded[0].channels[0].frames.is_empty());
        assert_eq!(loaded[1].channels[0].frames, [0, 1, 2]);
        assert_eq!(overlap(&trace.lock().unwrap(), &[samples[0], samples[1]]), 0);
        assert_eq!(overlap(&trace.lock().unwrap(), &[samples[2]]), 24);
        read_trace::stop(&trace);
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn post_save_refresh_rebuilds_stream_and_record_positions_after_takm_rewrite() {
        let _temp = crate::test_support::TempDir::new("tak_refresh_rewrite");
        let path = _temp.join(format!("tak_refresh_rewrite_{}.takm", std::process::id()));
        let nodes = refresh_nodes();
        let before = refresh_channels(1);
        write_refresh_takm(&path, &[("Nodes.dat", &nodes), ("Channels.dat", &before)]);
        let stale = deferred_job_after_save(&path).unwrap().unwrap();
        let old_base = stale.map.flat.unwrap();
        let old_emg = stale.records[stale.device_records[1][0]].start;
        // Saving changed marker counts can move the stream and every subsequent record.
        let after = refresh_channels(37);
        let padding = vec![0x55; 4096];
        let index = write_refresh_takm(&path, &[("Nodes.dat", &nodes), ("Calibration.dat", &padding), ("Channels.dat", &after)]);
        let samples = refresh_sample_ranges(&after, index.unique("Channels.dat").unwrap().offset);
        let file = File::open(&path).unwrap();
        let trace = read_trace::start(&file);
        let fresh = deferred_job_after_save(&path).unwrap().unwrap();
        assert_ne!(fresh.map.flat.unwrap(), old_base);
        assert_ne!(fresh.records[fresh.device_records[1][0]].start, old_emg);
        assert_eq!(overlap(&trace.lock().unwrap(), &samples), 0);
        let (loaded, warning) = fresh.run().unwrap();
        assert!(warning.is_none());
        let (reference, warning) = decode_analog(&after);
        assert!(warning.is_none());
        assert_same_analog(&loaded[1..], &reference[1..]);
        assert!(loaded[0].channels[0].frames.is_empty());
        assert!(stale.run().is_err(), "the old job must not remain usable after offset changes");
        read_trace::stop(&trace);
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn post_save_refresh_rejects_ambiguous_layout_without_reading_samples_or_fallback() {
        let _temp = crate::test_support::TempDir::new("tak_refresh_ambiguous");
        let path = _temp.join(format!("tak_refresh_ambiguous_{}.takm", std::process::id()));
        let channels = two_device_stream();
        let nodes = refresh_nodes();
        for (case, streams) in [
            ("missing device registry", vec![("Nodes.dat", &[][..]), ("Channels.dat", channels.as_slice())]),
            ("duplicate device registry", vec![("Nodes.dat", nodes.as_slice()), ("Nodes.dat", nodes.as_slice()), ("Channels.dat", channels.as_slice())]),
            ("duplicate channels", vec![("Nodes.dat", nodes.as_slice()), ("Channels.dat", channels.as_slice()), ("Channels.dat", channels.as_slice())]),
        ] {
            let index = write_refresh_takm(&path, &streams);
            let samples: Vec<_> = index.streams.iter().filter(|stream| stream.name == "Channels.dat")
                .flat_map(|stream| refresh_sample_ranges(&channels, stream.offset)).collect();
            let file = File::open(&path).unwrap();
            let trace = read_trace::start(&file);
            assert!(deferred_job_after_save(&path).is_err(), "{case}");
            assert_eq!(overlap(&trace.lock().unwrap(), &samples), 0, "{case}");
            read_trace::stop(&trace);
            drop(file);
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn post_save_refresh_returns_none_for_forceplate_only_and_requires_takm() {
        let _temp = crate::test_support::TempDir::new("tak_refresh_plate_only");
        let path = _temp.join(format!("tak_refresh_plate_only_{}.takm", std::process::id()));
        let nodes = device_node([1; 16], 2, "plate");
        let mut channels = CHANNELS_HEADER.to_vec();
        channels.extend(1u32.to_le_bytes());
        channels.extend(with_uid(record("FloatChannel", "AnalogChannel0", 1, 3), [1; 16]));
        let index = write_refresh_takm(&path, &[("Nodes.dat", &nodes), ("Channels.dat", &channels)]);
        let samples = refresh_sample_ranges(&channels, index.unique("Channels.dat").unwrap().offset);
        let file = File::open(&path).unwrap();
        let trace = read_trace::start(&file);
        assert!(deferred_job_after_save(&path).unwrap().is_none());
        assert_eq!(overlap(&trace.lock().unwrap(), &samples), 0);
        assert!(deferred_job_after_save(&path.with_extension("tak")).err().unwrap().0.contains(".takm"));
        read_trace::stop(&trace);
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unknown_structure_runs_existing_full_analog_fallback() {
        let stream = two_device_stream();
        let _temp = crate::test_support::TempDir::new("tak_unknown_analog");
        let path = _temp.join(format!("tak_unknown_analog_{}", std::process::id()));
        std::fs::write(&path, &stream).unwrap();
        let map = StreamMap { sector: 0, chain: vec![], len: stream.len() as u64, flat: Some(0) };
        let job = test_job(&path, map, &[], false);
        assert!(job.fallback_reason().unwrap().contains("structural device node"));
        let (job, lazy) = job.split();
        assert!(lazy.is_none());
        let expected = decode_analog(&stream);
        let actual = job.run().unwrap();
        assert_eq!(actual.1, expected.1);
        assert_same_analog(&actual.0, &expected.0);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn deferred_read_rejects_changed_tak_and_accepts_only_takm_tail_growth() {
        use std::io::Write;
        let stream = two_device_stream();
        let _temp = crate::test_support::TempDir::new("tak_lazy_unchanged");
        let path = _temp.join(format!("tak_lazy_unchanged_{}", std::process::id()));
        std::fs::write(&path, &stream).unwrap();
        let map = StreamMap { sector: 0, chain: vec![], len: stream.len() as u64, flat: Some(0) };
        let mut nodes = device_node([1; 16], 2, "plate");
        nodes.extend(device_node([2; 16], 1, "other"));
        let strict = test_job(&path, map.clone(), &nodes, false).split().1.unwrap();
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"tail").unwrap();
        assert!(strict.run().unwrap_err().0.contains("changed"));
        std::fs::write(&path, &stream).unwrap();
        let grows = test_job(&path, map.clone(), &nodes, true).split().1.unwrap();
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"tail").unwrap();
        assert!(grows.run().is_ok());
        let shrinks = test_job(&path, map, &nodes, true).split().1.unwrap();
        std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(stream.len() as u64 - 1).unwrap();
        assert!(shrinks.run().unwrap_err().0.contains("shorter"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn actual_takm_uses_same_selective_layout_and_all_analog_parity() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../input/static/000000_S01_static.tak");
        if !path.exists() {
            return;
        }
        let _temp = crate::test_support::TempDir::new("tak_selective_takm");
        let dir = _temp.join(format!("tak_selective_takm_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let converted = crate::takm::convert(&path, Some(&dir)).unwrap();
        let _ = converted;
        let out = crate::takm::path_for(&path, Some(&dir));
        let full = super::super::read_take(&out).unwrap();
        let read = read_take_fast(&out, true).unwrap();
        let job = read.analog.unwrap();
        assert!(job.fallback_reason().is_none(), "{:?}", job.fallback_reason());
        assert_same_analog(&full.analog, &job.run().unwrap().0);
        let file = File::open(&out).unwrap();
        let trace = read_trace::start(&file);
        let read = read_take_fast(&out, false).unwrap();
        let job = read.analog.unwrap();
        let nonplate: Vec<_> = job
            .device_records
            .iter()
            .enumerate()
            .filter(|(di, _)| !job.plate_devices().unwrap().contains(di))
            .flat_map(|(_, rs)| rs)
            .flat_map(|&ri| {
                let r = &job.records[ri];
                job.map.pieces(r.data_start, r.end).into_iter().map(|(off, n)| (off, off + n as u64))
            })
            .collect();
        let (startup, lazy) = job.split();
        startup.run().unwrap();
        assert_eq!(overlap(&trace.lock().unwrap(), &nonplate), 0);
        lazy.unwrap().run().unwrap();
        assert!(overlap(&trace.lock().unwrap(), &nonplate) > 0);
        read_trace::stop(&trace);
        drop(file);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn read_trace_keeps_equal_size_distinct_files_separate() {
        let _temp = crate::test_support::TempDir::new("tak_read_identity");
        let dir = _temp.join(format!("tak_read_identity_{}",std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("a"); let second = dir.join("b");
        std::fs::write(&first, [1u8; 23]).unwrap();
        std::fs::write(&second, [2u8; 23]).unwrap();
        let first = File::open(first).unwrap(); let second = std::fs::OpenOptions::new().read(true).write(true).open(second).unwrap();
        // Same size and mtime cannot serve as a file identity.
        second.set_times(std::fs::FileTimes::new().set_modified(first.metadata().unwrap().modified().unwrap())).unwrap();
        assert_eq!(stat_key(&first.metadata().unwrap()), stat_key(&second.metadata().unwrap()));
        let a = read_trace::start(&first); let b = read_trace::start(&second);
        read_exact_at(&first,&mut [0u8;3],0).unwrap();
        read_exact_at(&second,&mut [0u8;5],1).unwrap();
        assert_eq!(*a.lock().unwrap(),vec![(0,3)]);
        assert_eq!(*b.lock().unwrap(),vec![(1,6)]);
        read_trace::stop(&a); read_trace::stop(&b);
        drop(first); drop(second);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn structurally_known_device_with_renamed_channel_skips_samples_and_reports_fallback() {
        for (kind, property) in [(1, "RenamedEMG"), (2, "RenamedForce")] {
            let uid = [kind as u8;16];
            let mut stream = CHANNELS_HEADER.to_vec();
            stream.extend(2u32.to_le_bytes());
            stream.extend(record("Vector3fChannel", "Translation", 3, 5000));
            stream.extend(with_uid(record("FloatChannel", property, 1, 4), uid));
            let _temp = crate::test_support::TempDir::new("tak_renamed_device");
            let path = _temp.join(format!("tak_renamed_device_{}_{}", std::process::id(), kind));
            std::fs::write(&path,&stream).unwrap();
            let file = File::open(&path).unwrap();
            let trace = read_trace::start(&file);
            let nodes = device_node(uid,kind,"unrelated human name");
            let map = StreamMap {sector:0,chain:vec![],len:stream.len() as u64,flat:Some(0)};
            let job = test_job(&path,map,&nodes,false);
            assert!(job.fallback_reason().unwrap().contains("unsupported structural device channel property"));
            let device = job.records.iter().find(|r|r.uid==uid).unwrap();
            assert!(device.analog);
            let samples = [(device.data_start,device.end)];
            assert_eq!(overlap(&trace.lock().unwrap(),&samples),0);
            let (full,lazy)=job.split();
            assert!(lazy.is_none());
            let expected=decode_analog(&stream);
            let actual=full.run().unwrap();
            assert_eq!(actual.1,expected.1);
            assert_same_analog(&actual.0,&expected.0);
            assert_eq!(overlap(&trace.lock().unwrap(),&samples),32);
            read_trace::stop(&trace);
            drop(file);
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn invalid_device_registry_aborts_before_channel_payload_reads() {
        let stream=two_device_stream();
        let _temp = crate::test_support::TempDir::new("tak_invalid_device_registry");
        let path = _temp.join(format!("tak_invalid_device_registry_{}",std::process::id()));
        std::fs::write(&path,&stream).unwrap();
        let file=File::open(&path).unwrap();
        let trace=read_trace::start(&file);
        let mut malformed=device_node([1;16],1,"a");
        // Corrupt a known-signature JSON block without changing its declared length.
        let at=malformed.windows(2).position(|w|w==b"{\0").unwrap();
        malformed[at]=b'!';
        let mut unknown = device_node([1;16],1,"unknown class");
        unknown[2] = b'x'; // Valid type-1 JSON, unsupported device class signature.
        let mut escaped = unknown.clone();
        let sig_len = u16::from_le_bytes([escaped[0],escaped[1]]) as usize;
        let length_at = 2 + sig_len;
        let chars = u16::from_le_bytes([escaped[length_at],escaped[length_at+1]]) as usize;
        let json_at = length_at + 2;
        let old_end = json_at + chars * 2;
        let json = utf16le_strict(&escaped[json_at..old_end]).unwrap().replace("DeviceNode_DeviceType","DeviceNode_\\u0044eviceType");
        let encoded = super::super::nodes::utf16(&json);
        escaped[length_at..length_at+2].copy_from_slice(&((encoded.len()/2) as u16).to_le_bytes());
        escaped.splice(json_at..old_end,encoded);
        for nodes in [device_node([1;16],9,"unknown kind"),malformed,unknown,escaped] {
            trace.lock().unwrap().clear();
            let map=StreamMap {sector:0,chain:vec![],len:stream.len() as u64,flat:Some(0)};
            let before=stat_key(&file.metadata().unwrap());
            let result=markers_from(&path,&file,(before,false),stream.len() as u64,&nodes,&[],map,0.0,false);
            assert!(result.is_err());
            assert!(trace.lock().unwrap().is_empty(),"registry rejection must precede channel header/payload reads");
        }
        read_trace::stop(&trace);
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn selected_decode_failure_is_an_error_but_full_decode_keeps_legacy_warning() {
        let mut stream = two_device_stream();
        let inventory = super::super::channels::iter_channel_inventory(&stream).unwrap();
        let second_frame = inventory[1].data_start + 8;
        // Duplicate the second device's frame 0. The first device remains valid.
        stream[second_frame..second_frame + 4].copy_from_slice(&0u32.to_le_bytes());
        let _temp = crate::test_support::TempDir::new("tak_selective_decode_error");
        let path = _temp.join(format!("tak_selective_decode_error_{}", std::process::id()));
        std::fs::write(&path, &stream).unwrap();
        let map = StreamMap { sector: 0, chain: vec![], len: stream.len() as u64, flat: Some(0) };
        let mut nodes = device_node([1;16], 2, "plate");
        nodes.extend(device_node([2;16], 1, "other"));
        let legacy = test_job(&path, map.clone(), &nodes, false).run().unwrap();
        assert!(legacy.0.is_empty());
        assert!(legacy.1.as_ref().unwrap().contains("Non-increasing channel frames"));
        let (startup, lazy) = test_job(&path, map.clone(), &nodes, false).split();
        assert!(startup.run().unwrap().1.is_none());
        let failure = lazy.unwrap().run().unwrap_err();
        assert!(failure.0.contains("Non-increasing channel frames"));
        // A selected stage must also reject an unexpected decoded device count.
        let mut inconsistent = test_job(&path, map, &nodes, false);
        Arc::make_mut(&mut inconsistent.device_records)[0].clear();
        let (startup, _) = inconsistent.split();
        assert!(startup.run().unwrap_err().0.contains("device decode count changed"));
        std::fs::remove_file(path).unwrap();
    }

    fn counted_tail_walk(records: &[Vec<u8>], expected_channels: usize, marker_uids: &[Uid], tag: &str) -> (Result<Option<Walked>>, Vec<(u64,u64)>, Vec<u8>) {
        let mut stream=CHANNELS_HEADER.to_vec();
        stream.extend((records.len() as u32).to_le_bytes());
        for r in records {stream.extend(r);}
        let _temp = crate::test_support::TempDir::new("tak_counted_tail");
        let path = _temp.join(format!("tak_counted_tail_{}_{}",std::process::id(),tag));
        std::fs::write(&path,&stream).unwrap();
        let file=File::open(&path).unwrap();
        let trace=read_trace::start(&file);
        let map=StreamMap {sector:0,chain:vec![],len:stream.len() as u64,flat:Some(0)};
        let plan=MarkerTailPlan {channels:std::collections::HashMap::from([([1;16],expected_channels)]),markers:marker_uids.iter().copied().collect()};
        let result=walk_channels(&file,&map,&std::collections::HashSet::from([[1;16]]),Some(&plan));
        let reads=trace.lock().unwrap().clone();
        read_trace::stop(&trace);
        drop(file);
        std::fs::remove_file(path).unwrap();
        (result,reads,stream)
    }

    #[test]
    fn structural_device_counts_allow_bulk_marker_suffix_without_analog_reads() {
        let mut records=vec![with_uid(record("FloatChannel","AnalogChannel0",1,5),[1;16])];
        let markers:Vec<Uid>=(2..32).map(|n|[n;16]).collect();
        records.extend(markers.iter().map(|uid|with_uid(record("Vector3fChannel","Translation",3,100),*uid)));
        let (walked,reads,_)=counted_tail_walk(&records,1,&markers,"valid");
        let walked=walked.unwrap().unwrap();
        assert_eq!(walked.records.len(),31);
        assert!(reads.len()<15,"bulk suffix should avoid per-marker header roundtrips: {}",reads.len());
        let analog=&walked.records[0];
        assert_eq!(overlap(&reads,&[(analog.data_start,analog.end)]),0);
    }

    #[test]
    fn interleaved_device_channels_wait_for_declared_count_before_marker_bulk() {
        let records=[
            with_uid(record("FloatChannel","AnalogChannel0",1,5),[1;16]),
            with_uid(record("Vector3fChannel","Translation",3,5000),[2;16]),
            with_uid(record("FloatChannel","AnalogChannel1",1,7),[1;16]),
            with_uid(record("Vector3fChannel","Translation",3,2),[3;16]),
        ];
        let (walked,reads,_)=counted_tail_walk(&records,2,&[[2;16],[3;16]],"interleaved");
        let walked=walked.unwrap().unwrap();
        let samples:Vec<_>=walked.records.iter().filter(|r|r.analog).map(|r|(r.data_start,r.end)).collect();
        assert_eq!(overlap(&reads,&samples),0);
    }

    #[test]
    fn inconsistent_count_proof_or_marker_suffix_is_rejected_explicitly() {
        let analog=with_uid(record("FloatChannel","AnalogChannel0",1,5),[1;16]);
        let marker=with_uid(record("Vector3fChannel","Translation",3,5000),[2;16]);
        let extra_renamed=with_uid(record("FloatChannel","RenamedEMG",1,3),[1;16]);
        let (bad,_,_)=counted_tail_walk(&[analog.clone(),marker.clone(),extra_renamed],1,&[[2;16],[3;16]],"extra_renamed");
        assert!(bad.err().unwrap().0.contains("marker suffix"));
        let (bad,_,_)=counted_tail_walk(&[analog.clone(),marker.clone(),marker.clone()],1,&[[2;16],[3;16]],"duplicate_marker");
        assert!(bad.err().unwrap().0.contains("marker suffix"));
        let mut invalid=with_uid(record("Vector3fChannel","Translation",3,2),[3;16]);
        let kind_len=u16::from_le_bytes([invalid[0],invalid[1]]) as usize;
        invalid[2+kind_len]=3; // Unsupported record version inside a proposed suffix.
        let (bad,_,_)=counted_tail_walk(&[analog.clone(),marker.clone(),invalid],1,&[[2;16],[3;16]],"invalid_marker");
        assert!(bad.err().unwrap().0.contains("marker suffix"));
        // An extra channel changes the remaining-record count, preventing any bulk proof.
        // The subsequent manifest/metadata comparison must explicitly select full fallback.
        let records=[analog,marker,with_uid(record("FloatChannel","AnalogChannel1",1,2),[1;16]),with_uid(record("Vector3fChannel","Translation",3,2),[3;16])];
        let (walked,reads,_)=counted_tail_walk(&records,1,&[[2;16],[3;16]],"extra_channel");
        let walked=walked.unwrap().unwrap();
        let samples:Vec<_>=walked.records.iter().filter(|r|r.analog).map(|r|(r.data_start,r.end)).collect();
        assert_eq!(overlap(&reads,&samples),0);
        let (_,groups)=device_manifest(&walked.records).unwrap();
        let typed=[super::super::nodes::DeviceNode {uid:[1;16],forceplate:false,channel_count:Some(1),plate:None,sampling:None}];
        assert!(selective_layout_for_nodes(&typed,&walked.records,&groups).unwrap_err().0.contains("channel count"));
    }

    #[test]
    fn duplicate_node_marker_uids_cannot_prove_a_bulk_suffix() {
        use super::super::nodes::{tests::node,MARKER_SIGNATURES};
        let mut nodes=node(MARKER_SIGNATURES[0],"a",[2;16],[0;16],0);
        nodes.extend(node(MARKER_SIGNATURES[0],"a",[2;16],[0;16],0));
        let typed=[super::super::nodes::DeviceNode {uid:[1;16],forceplate:false,channel_count:Some(1),plate:None,sampling:None}];
        let failure=marker_tail_plan(&nodes,&typed).err().unwrap();
        assert!(failure.0.contains("duplicate marker UID"));
    }

    #[test]
    fn minimum_header_probe_skips_samples_for_shortest_supported_device_properties() {
        let recs=[
            with_uid(record("FloatChannel","",1,3),[1;16]),
            with_uid(record("FloatChannel","X",1,2),[1;16]),
            with_uid(record("Float16Channel","",16,1),[1;16]),
        ];
        let mut stream=CHANNELS_HEADER.to_vec();
        stream.extend((recs.len() as u32).to_le_bytes());
        for r in recs {stream.extend(r);}
        let _temp = crate::test_support::TempDir::new("tak_minimum_header");
        let path = _temp.join(format!("tak_minimum_header_{}",std::process::id()));
        std::fs::write(&path,&stream).unwrap();
        let file=File::open(&path).unwrap();
        let trace=read_trace::start(&file);
        let map=StreamMap {sector:0,chain:vec![],len:stream.len() as u64,flat:Some(0)};
        let walked=walk_channels(&file,&map,&std::collections::HashSet::from([[1;16]]),None).unwrap().unwrap();
        let sample_ranges:Vec<_>=walked.records.iter().map(|r|(r.data_start,r.end)).collect();
        assert!(walked.records.iter().all(|r|r.analog));
        assert_eq!(overlap(&trace.lock().unwrap(),&sample_ranges),0);
        assert_eq!(walked.records[0].data_start-walked.records[0].start,MIN_HEADER);
        read_trace::stop(&trace);
        drop(file);
        std::fs::remove_file(path).unwrap();
        for kind in ["FloatChannel","Float16Channel","Vector3fChannel","QuaternionChannel","class MoCapCore::cChannel<struct MoCapCore::sArrayChannelValues<float,16> >"] {
            let dim=channel_dim(kind).unwrap();
            let bytes=record(kind,"",dim,0);
            assert!(bytes.len() as u64>=MIN_HEADER);
        }
    }

    #[test]
    fn unsupported_or_too_short_record_layout_requests_explicit_fallback() {
        for (tag,bytes) in [("unknown",record("X","",1,4)),("short",vec![0u8;30])] {
            let mut stream=CHANNELS_HEADER.to_vec();
            stream.extend(1u32.to_le_bytes());
            stream.extend(bytes);
            let _temp = crate::test_support::TempDir::new("tak_bad_minimum_header");
            let path = _temp.join(format!("tak_bad_minimum_header_{}_{}",std::process::id(),tag));
            std::fs::write(&path,&stream).unwrap();
            let file=File::open(&path).unwrap();
            let map=StreamMap {sector:0,chain:vec![],len:stream.len() as u64,flat:Some(0)};
            let failure=walk_channels(&file,&map,&std::collections::HashSet::new(),None).err().unwrap();
            assert!(failure.0.contains("unsupported structural channel")||failure.0.contains("shorter than"));
            drop(file);
            std::fs::remove_file(path).unwrap();
        }
    }

}
