//! Lossless USTAR encoding of the sealed logical layout 1 body.
use super::{err, parse_header, v2, Index, Result, TakError, HEADER_LEN};
use crate::tak::{channels, metadata, nodes, Uid};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

const BLOCK: usize = 512;
pub(crate) const MAX_LOGICAL: usize = 1 << 30;
pub(crate) const MAX_JOURNAL: usize = 64 << 20;
const PREVIEW_STRIDE: u32 = 8;
const END: [u8; 2 * BLOCK] = [0; 2 * BLOCK];

pub(crate) struct Written {
    pub size: u64,
    pub fixed_identity: u64,
}

pub(crate) struct Logical<'a> {
    pub raw: &'a [u8],
    pub table_at: usize,
    pub table_end: usize,
    pub table: Value,
    pub index: Index,
}

impl<'a> Logical<'a> {
    pub(crate) fn parse(raw: &'a [u8]) -> Result<Self> {
        if raw.len() > MAX_LOGICAL || raw.len() < HEADER_LEN as usize {
            return err("TAKM logical body is outside the supported size");
        }
        let (table_at, _, table_end) = parse_header(raw, raw.len() as u64)?;
        let (table_at, table_end) = (table_at as usize, table_end as usize);
        let index = v2::legacy_index(raw)?;
        let table = serde_json::from_slice(&raw[table_at..table_end])
            .map_err(|e| TakError(format!("TAKM logical table: {e}")))?;
        Ok(Self {
            raw,
            table_at,
            table_end,
            table,
            index,
        })
    }

    pub(crate) fn stream(&self, row: &super::Stream) -> &'a [u8] {
        &self.raw[row.offset as usize..(row.offset + row.size) as usize]
    }
}

struct Member {
    kind: &'static str,
    name: &'static str,
    codec: &'static str,
    expanded: usize,
    packed: Vec<u8>,
}

fn zstd(kind: &'static str, name: &'static str, codec: &'static str, raw: &[u8]) -> Result<Member> {
    let mut writer = zstd::stream::Encoder::new(Vec::new(), 3)?;
    writer.include_checksum(true)?;
    writer.set_pledged_src_size(Some(raw.len() as u64))?;
    writer.write_all(raw)?;
    Ok(Member {
        kind,
        name,
        codec,
        expanded: raw.len(),
        packed: writer.finish()?,
    })
}

fn put_blob(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let size = 8 + bytes.len();
    if out.len().saturating_add(size) > MAX_LOGICAL {
        return err("TAKM expanded member exceeds the size limit");
    }
    out.try_reserve(size)
        .map_err(|e| TakError(format!("TAKM member allocation: {e}")))?;
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn blobs(parts: impl IntoIterator<Item = impl AsRef<[u8]>>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for part in parts {
        put_blob(&mut bytes, part.as_ref())?;
    }
    Ok(bytes)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Spatial,
    Plates,
    Analog,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Spatial => "spatial",
            Self::Plates => "plates",
            Self::Analog => "analog",
        }
    }
}

struct Record<'a> {
    raw: &'a [u8],
    head: &'a [u8],
    width: usize,
    count: usize,
    role: Role,
    marker: bool,
}

impl<'a> Record<'a> {
    fn rows(&self) -> impl Iterator<Item = &'a [u8]> {
        self.raw.chunks_exact(self.width * 4)
    }
}

struct Hybrid<'a> {
    start: i64,
    stop: i64,
    first: i64,
    stream_ids: [usize; 3],
    nodes: &'a [u8],
    meta: &'a [u8],
    channels: &'a [u8],
    records: Vec<Record<'a>>,
}

impl<'a> Hybrid<'a> {
    fn parse(logical: &Logical<'a>) -> Result<Self> {
        let stream_id = |name| {
            logical
                .index
                .streams
                .iter()
                .rposition(|s| s.name == name)
                .ok_or_else(|| TakError(format!("Missing TAKM {name}")))
        };
        let stream_ids = [
            stream_id("Nodes.dat")?,
            stream_id("MetaData.dat")?,
            stream_id("Channels.dat")?,
        ];
        let [nodes, meta, channels] = stream_ids.map(|i| logical.stream(&logical.index.streams[i]));
        let props = metadata::parse_properties(meta)?;
        let frame = |key: &str| {
            let (_, value) = props
                .iter()
                .find(|(k, _)| k == key)
                .ok_or_else(|| TakError(format!("Missing TAKM {key}")))?;
            metadata::python_int(value)
        };
        let (start, stop) = (frame("StartFrame")?, frame("EndFrame")?);
        if start < 0
            || stop < start
            || stop > u32::MAX.into()
            || stop - start + 1 > (MAX_LOGICAL / 4) as i64
        {
            return err("Unsupported TAKM frame extent");
        }
        let assets = nodes::skeleton_asset_set(nodes)?;
        let marker_uids: HashSet<_> = nodes::parse_marker_nodes(nodes, &assets)?
            .into_iter()
            .map(|marker| marker.uid)
            .collect();
        let devices: HashMap<_, _> = nodes::analog_device_nodes(nodes)?
            .into_iter()
            .map(|device| (device.uid, device.forceplate))
            .collect();

        let mut seen = HashSet::new();
        let mut group: Option<(Uid, i64)> = None;
        let mut records = Vec::new();
        let mut first = None;
        for row in channels::iter_channel_inventory(channels)? {
            let device = devices.get(&row.uid).copied();
            let role = if let Some(suffix) = row.name.strip_prefix("AnalogChannel") {
                let plate =
                    device.ok_or_else(|| TakError("Analog channel has no device node".into()))?;
                let lane = metadata::python_int(suffix)?;
                if lane == 0 && seen.insert(row.uid) {
                    group = Some((row.uid, 0));
                }
                if group != Some((row.uid, lane)) || lane < 0 {
                    return err("Ambiguous analog channel group");
                }
                group = Some((
                    row.uid,
                    lane.checked_add(1)
                        .ok_or_else(|| TakError("Analog channel index overflow".into()))?,
                ));
                if plate {
                    Role::Plates
                } else {
                    Role::Analog
                }
            } else if device.is_some() {
                return err("Unrecognized device channel");
            } else {
                Role::Spatial
            };
            let record = Record {
                raw: &channels[row.data_start..row.data_end],
                head: &channels[row.start..row.data_start],
                width: row.dim() + 1,
                count: row.n_samples,
                role,
                marker: role == Role::Spatial
                    && row.name == "Translation"
                    && marker_uids.contains(&row.uid),
            };
            if record.marker {
                if let Some(sample) = record.rows().next() {
                    let frame = i64::from(word(sample, 0));
                    first = Some(first.map_or(frame, |old: i64| old.min(frame)));
                }
            }
            records.push(record);
        }
        if devices
            .iter()
            .any(|(uid, plate)| *plate && !seen.contains(uid))
        {
            return err("Forceplate node has no matching analog device");
        }
        let first = first.unwrap_or(start);
        if first < start || first > stop {
            return err("TAKM first marker lies outside the take");
        }
        Ok(Self {
            start,
            stop,
            first,
            stream_ids,
            nodes,
            meta,
            channels,
            records,
        })
    }

    fn encode(&self, logical: &Logical<'a>) -> Result<Vec<Member>> {
        let info = serde_json::to_vec(&json!({
            "start": self.start, "stop": self.stop, "first": self.first, "preview_stride": PREVIEW_STRIDE,
            "records": self.records.iter().map(|r| json!([r.width, r.count, r.role.name(), r.marker])).collect::<Vec<_>>(),
            "stream_ids": self.stream_ids, "streams": logical.index.streams.len(),
        })).map_err(|e| TakError(e.to_string()))?;
        let headers = blobs(self.records.iter().map(|r| r.head))?;
        let manifest = blobs([
            info.as_slice(),
            self.nodes,
            self.meta,
            &self.channels[..10],
            &headers,
        ])?;
        let spatial = || self.records.iter().filter(|r| r.role == Role::Spatial);
        let mut presence = Vec::new();
        for record in spatial() {
            let mut runs = Vec::new();
            let mut start = 0u32;
            let mut count = 0u32;
            for row in record.rows() {
                let frame = word(row, 0);
                if record.marker && !(self.start..=self.stop).contains(&i64::from(frame)) {
                    return err("Spatial marker frame lies outside the take");
                }
                match start.checked_add(count) {
                    Some(end) if count == 0 || frame > end => {
                        put_run(&mut runs, start, count);
                        start = frame;
                        count = 1;
                    }
                    Some(end) if frame == end => count += 1,
                    _ => return err("Spatial frames do not increase"),
                }
            }
            put_run(&mut runs, start, count);
            put_blob(&mut presence, &runs)?;
        }
        let first_frame = self.first as u32;
        let is_first = |frame| frame == first_frame;
        let is_preview = |frame| {
            !is_first(frame) && frame % PREVIEW_STRIDE == self.start as u32 % PREVIEW_STRIDE
        };
        let mut members = vec![
            zstd("manifest", "manifest.bin.zst", "zstd", &manifest)?,
            zstd("presence", "presence.bin.zst", "zstd", &presence)?,
        ];
        for (kind, name, select) in [
            (
                "first",
                "first.pco.parts",
                &is_first as &dyn Fn(u32) -> bool,
            ),
            ("preview", "preview.pco.parts", &is_preview),
            ("exact", "exact.pco.parts", &|frame| {
                !is_first(frame) && !is_preview(frame)
            }),
        ] {
            members.push(pco(kind, name, spatial(), select, false)?);
        }
        let plate_bytes = xor_plates(self.records.iter().filter(|r| r.role == Role::Plates));
        members.push(zstd("plates", "plates.xor.zst", "xor-zstd", &plate_bytes)?);
        members.push(pco(
            "analog",
            "analog.pco.parts",
            self.records.iter().filter(|r| r.role == Role::Analog),
            |_| true,
            true,
        )?);
        let mut ordered: Vec<_> = logical.index.streams.iter().collect();
        ordered.sort_by_key(|s| s.offset);
        let mut cursor = HEADER_LEN as usize;
        let mut gaps = Vec::new();
        for stream in ordered {
            let gap = logical
                .raw
                .get(cursor..stream.offset as usize)
                .ok_or_else(|| TakError("Overlapping logical TAKM streams".into()))?;
            put_blob(&mut gaps, gap)?;
            cursor = (stream.offset + stream.size) as usize;
        }
        put_blob(&mut gaps, &logical.raw[cursor..logical.table_at])?;
        let mut recovery = blobs([
            &logical.raw[..HEADER_LEN as usize],
            &logical.raw[logical.table_at..logical.table_end],
            &logical.raw[logical.table_end..],
            gaps.as_slice(),
        ])?;
        for (i, stream) in logical.index.streams.iter().enumerate() {
            if !self.stream_ids.contains(&i) {
                put_blob(&mut recovery, logical.stream(stream))?;
            }
        }
        members.push(zstd("recovery", "recovery.bin.zst", "zstd", &recovery)?);
        Ok(members)
    }
}

fn word(row: &[u8], lane: usize) -> u32 {
    u32::from_le_bytes(row[4 * lane..4 * lane + 4].try_into().unwrap())
}

fn put_run(out: &mut Vec<u8>, start: u32, count: u32) {
    if count != 0 {
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
    }
}

fn pco<'a>(
    kind: &'static str,
    name: &'static str,
    records: impl Iterator<Item = &'a Record<'a>>,
    select: impl Fn(u32) -> bool,
    include_frames: bool,
) -> Result<Member> {
    fn compress<T: pco::data_types::Number>(lane: &[T]) -> Result<Vec<u8>> {
        if lane.is_empty() {
            return Ok(Vec::new());
        }
        let config = pco::ChunkConfig::default().with_compression_level(8);
        pco::standalone::simple_compress(lane, &config)
            .map_err(|e| TakError(format!("TAKM PCo: {e}")))
    }
    let mut frames = Vec::new();
    let mut values = Vec::new();
    for record in records {
        let selected: Vec<_> = record.rows().filter(|row| select(word(row, 0))).collect();
        if include_frames {
            frames.extend(selected.iter().map(|row| word(row, 0)));
        }
        for lane in 1..record.width {
            values.extend(selected.iter().map(|row| f32::from_bits(word(row, lane))));
        }
    }
    let mut packed = Vec::new();
    if include_frames {
        put_blob(&mut packed, &compress(&frames)?)?;
    }
    put_blob(&mut packed, &compress(&values)?)?;
    Ok(Member {
        kind,
        name,
        codec: "pco",
        expanded: 0,
        packed,
    })
}

fn xor_plates<'a>(records: impl Iterator<Item = &'a Record<'a>>) -> Vec<u8> {
    let mut words = Vec::new();
    for record in records {
        for lane in 0..record.width {
            let mut previous = 0;
            for row in record.rows() {
                let bits = word(row, lane);
                words.push((bits ^ previous).to_le_bytes());
                previous = bits;
            }
        }
    }
    (0..4)
        .flat_map(|byte| words.iter().map(move |word| word[byte]))
        .collect()
}

fn padded(size: usize) -> usize {
    size.next_multiple_of(BLOCK)
}

fn span(size: usize) -> usize {
    BLOCK + padded(size)
}

fn member(out: &mut impl Write, name: &str, data: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_path(name)?;
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(data.len() as u64);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    out.write_all(header.as_bytes())?;
    out.write_all(data)?;
    out.write_all(&END[..padded(data.len()) - data.len()])?;
    Ok(())
}

fn catalog(
    parts: &[Member],
    opaque: bool,
    comment_header: usize,
    comment_len: usize,
) -> Result<Vec<u8>> {
    let mut value = json!({
        "takm": 2,
        "parts": parts.iter().map(|m| (
            m.kind.to_owned(), json!([m.name, m.codec, m.expanded, format!("{:x}", Sha256::digest(&m.packed))])
        )).collect::<serde_json::Map<_, _>>(),
        "tar_comment_header": format!("{comment_header:012}"),
        "tar_comment_size": comment_len,
    });
    if opaque {
        value["profile"] = "opaque-v1".into();
    }
    serde_json::to_vec(&value).map_err(|e| TakError(e.to_string()))
}

struct DigestWriter<'a, W> {
    out: &'a mut W,
    hash: Sha256,
}

impl<W: Write> Write for DigestWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.out.write_all(bytes)?;
        self.hash.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

pub(crate) fn archive(out: &mut impl Write, fixed: &[u8], tail: &[u8]) -> Result<Written> {
    check_append_size(0, tail.len())?;
    let logical = Logical::parse(fixed)?;
    let hybrid = Hybrid::parse(&logical).and_then(|recipe| recipe.encode(&logical));
    let opaque = hybrid.is_err();
    let parts = match hybrid {
        Ok(parts) => parts,
        Err(_) => vec![zstd("recovery", "recovery.bin.zst", "zstd", fixed)?],
    };
    let placeholder = catalog(&parts, opaque, 0, tail.len())?;
    let fixed_identity =
        span(placeholder.len()) + parts.iter().map(|m| span(m.packed.len())).sum::<usize>();
    if fixed_identity > MAX_LOGICAL {
        return err("TAKM archive exceeds the fixed-body byte limit");
    }
    let comment_header = fixed_identity + span(32);
    let catalog = catalog(&parts, opaque, comment_header, tail.len())?;
    if catalog.len() != placeholder.len() {
        return err("TAKM catalog offset exceeds its field width");
    }
    let mut writer = DigestWriter {
        out,
        hash: Sha256::new(),
    };
    member(&mut writer, "takm.json", &catalog)?;
    for part in parts {
        member(&mut writer, part.name, &part.packed)?;
    }
    let seal = writer.hash.finalize();
    member(out, "seal.sha256", &seal)?;
    member(out, "comments.csv", tail)?;
    out.write_all(&END)?;
    Ok(Written {
        size: (comment_header + span(tail.len()) + END.len()) as u64,
        fixed_identity: fixed_identity as u64,
    })
}

pub(crate) fn comment_header(fixed_identity: u64) -> u64 {
    fixed_identity + span(32) as u64
}

pub(super) fn check_append_size(used: u64, bytes: usize) -> Result<()> {
    if bytes > MAX_JOURNAL || used + (span(bytes) + END.len()) as u64 > MAX_JOURNAL as u64 {
        return err("TAKM Comment journal exceeds the byte limit");
    }
    Ok(())
}

pub(super) fn append_member(out: &mut impl Write, ordinal: u64, suffix: &[u8]) -> Result<()> {
    if !suffix.is_empty() {
        let name = format!("comments/{ordinal:016}-{:x}.csv", Sha256::digest(suffix));
        member(out, &name, suffix)?;
    }
    out.write_all(&END)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{comments, edit, sample_tak, takm, test_support::TempDir};
    use std::{fs, path::Path};

    fn legacy(take: &[u8]) -> Vec<u8> {
        let mut raw = Vec::new();
        takm::write(&mut raw, take, "sample.tak", "test").unwrap();
        raw
    }

    fn write_v2(path: &Path, raw: &[u8]) {
        let mut out = fs::File::create(path).unwrap();
        takm::write_v2_from_v1(&mut out, raw).unwrap();
    }

    fn event(id: u64) -> comments::Event {
        comments::Event {
            time: "2026-01-01T00:00:00+00:00".into(),
            kind: comments::Kind::Add,
            id,
            markers: vec!["LKNE".into()],
            start: 1,
            end: 2,
            comment_type: String::new(),
            text: format!("첫 줄 {id}\n둘째 줄"),
            shown_a: "1".into(),
            shown_b: String::new(),
            file_a: "sample.takm|123".into(),
            file_b: String::new(),
            operation: Some(format!("test-operation-{id}")),
            expected: None,
        }
    }

    #[test]
    fn hybrid_and_opaque_restore_exact_logical_bytes() {
        let dir = TempDir::new("takm2_profiles");
        let mut raw = legacy(&sample_tak::sample_tak_for_label_assignments());
        raw.extend_from_slice(b"\xffpartial-comment");
        for opaque in [false, true] {
            if opaque {
                let (fixed, _) = takm::v2_input(&raw).unwrap();
                let logical = Logical::parse(fixed).unwrap();
                let channel = logical.index.unique("Channels.dat").unwrap().offset as usize;
                let seal = logical.table_end;
                // Unknown channel format with a valid v1 seal. Preserve the bytes as opaque.
                raw[channel] ^= 1;
                let digest = Sha256::digest(&raw[..seal]);
                raw[seal..seal + digest.len()].copy_from_slice(&digest);
            }
            let path = dir.join(format!("profile-{opaque}.takm"));
            write_v2(&path, &raw);
            let file = fs::File::open(&path).unwrap();
            let archive = takm::v2::Archive::open(&file, file.metadata().unwrap().len()).unwrap();
            assert_eq!(
                takm::v2::is_opaque(archive.catalog_bytes()).unwrap(),
                opaque
            );
            let (fixed, tail) = takm::v2_input(&raw).unwrap();
            assert_eq!(archive.restore().unwrap(), fixed);
            assert_eq!(archive.journal().unwrap().bytes, tail);
        }
    }

    #[test]
    fn guarded_append_repairs_final_cut_and_retries_once() {
        let dir = TempDir::new("takm2_guarded_append");
        let path = dir.join("sample.takm");
        let raw = legacy(&sample_tak::sample_tak_with_devices());
        write_v2(&path, &raw);
        let identity = takm::fixed_len(&path).unwrap();
        let first = event(1);
        let second = event(2);
        takm::append_events(&path, std::slice::from_ref(&first)).unwrap();
        let committed = fs::read(&path).unwrap();
        takm::append_events(&path, std::slice::from_ref(&second)).unwrap();
        let full = fs::read(&path).unwrap();
        let append_at = committed.len() - END.len();
        fs::write(&path, &full[..append_at + BLOCK + 24]).unwrap();
        let ids = || {
            takm::read_comments(&path)
                .unwrap()
                .events
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(), [1]);
        takm::append_events(&path, &[first, second.clone()]).unwrap();
        assert_eq!(ids(), [1, 2]);
        assert_eq!(fs::read(&path).unwrap(), full);
        takm::append_events(&path, &[second]).unwrap();
        assert_eq!(fs::read(&path).unwrap(), full);
        assert_eq!(takm::fixed_len(&path).unwrap(), identity);

        let first_at = comment_header(identity) as usize + span(comments::header_line().len());
        let mut damaged = full;
        damaged[first_at + BLOCK] ^= 1;
        fs::write(&path, &damaged).unwrap();
        assert!(takm::append_events(&path, &[event(3)]).is_err());
        assert_eq!(fs::read(&path).unwrap(), damaged);
    }

    #[test]
    fn progress_save_preserves_other_streams_and_comment_bytes() {
        let dir = TempDir::new("takm2_progress");
        let path = dir.join("sample.takm");
        write_v2(&path, &legacy(&sample_tak::sample_tak_with_devices()));
        takm::append_events(&path, &[event(1)]).unwrap();
        let restore = || {
            let file = fs::File::open(&path).unwrap();
            takm::restore_for_write(&file, file.metadata().unwrap().len()).unwrap()
        };
        let (before, comments, _) = restore();
        let original = Logical::parse(&before).unwrap();
        let outcome = edit::save::save_progress(&path, None, Some(4), None, None).unwrap();
        let (after, saved_comments, identity) = restore();
        assert_eq!(outcome.fixed_len, identity);
        assert_eq!(saved_comments, comments);
        let updated = Logical::parse(&after).unwrap();
        for (old, new) in original.index.streams.iter().zip(&updated.index.streams) {
            assert_eq!((old.index, &old.name), (new.index, &new.name));
            if old.name != "MetaData.dat" {
                assert_eq!(original.stream(old), updated.stream(new));
            }
        }
        assert_eq!(
            crate::tak::read_take(&path).unwrap().meta("Progress"),
            Some("4")
        );
        edit::save::save_progress(&path, Some(4), None, None, None).unwrap();
        assert_eq!(restore().0, before);
    }
}
