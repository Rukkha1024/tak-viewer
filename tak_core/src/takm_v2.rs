//! TAKM layout 2: seekable USTAR and lossless packed channel stages.

use pco::standalone::{DecompressorItem, FileDecompressor};
use serde::Deserialize;
use std::{collections::HashSet, fs::File};

use super::*;
use crate::tak::{channels, metadata, nodes};

const BLOCK: usize = 512;
const LIMIT: usize = 1 << 30;
const CATALOG_LIMIT: usize = 16 << 10;
const JOURNAL_LIMIT: usize = 64 << 20;
const STRIDE: u32 = 8;
const CATALOG: &str = "takm.json";
const SEAL: &str = "seal.sha256";
const BASE: &str = "comments.csv";
const PARTS: [(&str, &str, &str); 8] = [
    ("manifest", "manifest.bin.zst", "zstd"),
    ("presence", "presence.bin.zst", "zstd"),
    ("first", "first.pco.parts", "pco"),
    ("preview", "preview.pco.parts", "pco"),
    ("exact", "exact.pco.parts", "pco"),
    ("plates", "plates.xor.zst", "xor-zstd"),
    ("analog", "analog.pco.parts", "pco"),
    ("recovery", "recovery.bin.zst", "zstd"),
];

/// Independent ceilings for packed member bytes and expanded data.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub packed: usize,
    pub expanded: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            packed: LIMIT,
            expanded: LIMIT,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Profile {
    #[default]
    Hybrid,
    OpaqueV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HybridParts {
    manifest: Descriptor,
    presence: Descriptor,
    first: Descriptor,
    preview: Descriptor,
    exact: Descriptor,
    plates: Descriptor,
    analog: Descriptor,
    recovery: Descriptor,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpaqueParts {
    recovery: Descriptor,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Parts {
    Hybrid(Box<HybridParts>),
    Opaque(OpaqueParts),
}

#[derive(Deserialize)]
struct Descriptor(String, String, usize, String);

#[derive(Deserialize)]
struct Catalog {
    takm: u32,
    #[serde(default)]
    profile: Profile,
    parts: Parts,
    tar_comment_header: Option<String>,
    tar_comment_size: Option<u64>,
}

impl Catalog {
    fn parse(raw: &[u8]) -> Result<Self> {
        if raw.len() > CATALOG_LIMIT {
            return err("TAKM2 catalog is too large");
        }
        let catalog: Self = serde_json::from_slice(raw).map_err(invalid)?;
        if catalog.takm != 2
            || (catalog.profile == Profile::Hybrid) != matches!(catalog.parts, Parts::Hybrid(_))
        {
            return err("Invalid TAKM2 version or profile");
        }
        for (i, part) in catalog.parts().iter().enumerate() {
            if let Some(Descriptor(name, codec, expanded, sha)) = part {
                let (_, expected_name, expected_codec) = PARTS[i];
                if name != expected_name
                    || codec != expected_codec
                    || (codec == "pco" && *expanded != 0)
                    || !lower_hex_digest(sha)
                {
                    return err("Invalid TAKM2 part descriptor");
                }
            }
        }
        Ok(catalog)
    }

    fn parts(&self) -> [Option<&Descriptor>; 8] {
        match &self.parts {
            Parts::Hybrid(p) => [
                Some(&p.manifest),
                Some(&p.presence),
                Some(&p.first),
                Some(&p.preview),
                Some(&p.exact),
                Some(&p.plates),
                Some(&p.analog),
                Some(&p.recovery),
            ],
            Parts::Opaque(p) => [None, None, None, None, None, None, None, Some(&p.recovery)],
        }
    }

    fn part(
        &self,
        index: usize,
        limits: Limits,
        member: &mut MemberInput<'_>,
    ) -> Result<(Vec<u8>, usize)> {
        let Some(Descriptor(name, _, expanded, digest)) = self.parts()[index] else {
            return err("Missing TAKM2 part");
        };
        if *expanded > limits.expanded {
            return err("TAKM2 member exceeds expanded byte limit");
        }
        let data = member(name)?;
        if data.len() > limits.packed {
            return err("TAKM2 member exceeds packed byte limit");
        }
        if sha256_hex(&data) != *digest {
            return err(format!("TAKM2 packed SHA-256 mismatch ({name})"));
        }
        Ok((data, *expanded))
    }

    fn expanded(
        &self,
        index: usize,
        limits: Limits,
        member: &mut MemberInput<'_>,
    ) -> Result<Vec<u8>> {
        let (data, size) = self.part(index, limits, member)?;
        expand_zstd(&data, size)
    }
}

type MemberInput<'a> = dyn FnMut(&str) -> Result<Vec<u8>> + 'a;

fn invalid(error: impl std::fmt::Display) -> TakError {
    TakError(format!("Invalid TAKM2 data: {error}"))
}

fn lower_hex_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn expand_zstd(packed: &[u8], size: usize) -> Result<Vec<u8>> {
    if zstd::zstd_safe::find_frame_compressed_size(packed).ok() != Some(packed.len()) {
        return err("Invalid TAKM2 Zstandard frame length");
    }
    let mut decoder = zstd::bulk::Decompressor::new().map_err(invalid)?;
    decoder.window_log_max(23).map_err(invalid)?;
    let out = decoder.decompress(packed, size).map_err(invalid)?;
    if out.len() != size {
        return err("Wrong expanded TAKM2 member length");
    }
    Ok(out)
}

fn decode_pco<T: pco::data_types::Number + Default + Clone>(
    src: &[u8],
    count: usize,
) -> Result<Vec<T>> {
    if src.is_empty() && count == 0 {
        return Ok(Vec::new());
    }
    let (decoder, mut src) = FileDecompressor::new(src).map_err(invalid)?;
    let mut output = Vec::new();
    output.try_reserve_exact(count).map_err(invalid)?;
    loop {
        match decoder.chunk_decompressor::<T, _>(src).map_err(invalid)? {
            DecompressorItem::Chunk(mut chunk) => {
                let n = chunk.n();
                let end = output
                    .len()
                    .checked_add(n)
                    .filter(|&end| end <= count)
                    .ok_or_else(|| invalid("too many PCo words"))?;
                let start = output.len();
                output.resize(end, T::default());
                let progress = chunk.read(&mut output[start..]).map_err(invalid)?;
                if !progress.finished || progress.n_processed != n {
                    return err("Truncated TAKM2 PCo words");
                }
                src = chunk.into_src();
            }
            DecompressorItem::EndOfData(rest) => {
                if !rest.is_empty() || output.len() != count {
                    return err("Unexpected TAKM2 PCo stream length");
                }
                return Ok(output);
            }
        }
    }
}

fn blobs(mut raw: &[u8], count: usize) -> Result<Vec<&[u8]>> {
    if count > raw.len() / 8 {
        return err("Truncated TAKM2 blob list");
    }
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        let head = raw
            .get(..8)
            .ok_or_else(|| invalid("truncated blob length"))?;
        let size = usize::try_from(u64_le(head, 0)).map_err(invalid)?;
        let (blob, rest) = raw[8..]
            .split_at_checked(size)
            .ok_or_else(|| invalid("truncated blob"))?;
        result.push(blob);
        raw = rest;
    }
    if !raw.is_empty() {
        return err("Unexpected trailing TAKM2 blob bytes");
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Role {
    Spatial,
    Plates,
    Analog,
}

#[derive(Deserialize)]
struct Info {
    start: u32,
    stop: u32,
    first: u32,
    preview_stride: u32,
    records: Vec<(usize, usize, Role, bool)>,
    stream_ids: [usize; 3],
    streams: usize,
}

struct Record {
    width: usize,
    count: usize,
    role: Role,
    marker: bool,
    head: Vec<u8>,
    runs: Vec<(u32, u32)>,
}

struct Manifest {
    info: Info,
    nodes: Vec<u8>,
    metadata: Vec<u8>,
    header: [u8; 10],
    records: Vec<Record>,
    channel_size: usize,
}

impl Manifest {
    fn read(raw: &[u8], expanded: usize) -> Result<Self> {
        let parts = blobs(raw, 5)?;
        let info: Info = serde_json::from_slice(parts[0]).map_err(invalid)?;
        let header: [u8; 10] = parts[3].try_into().map_err(invalid)?;
        let stream_ids: HashSet<_> = info.stream_ids.into_iter().collect();
        if info.preview_stride != STRIDE
            || !(info.start..=info.stop).contains(&info.first)
            || u64::from(info.stop) - u64::from(info.start) + 1 > (expanded / 4) as u64
            || stream_ids.len() != 3
            || stream_ids.iter().any(|&i| i >= info.streams)
            || !header.starts_with(&channels::CHANNELS_HEADER)
            || u32_le(&header, 6) as usize != info.records.len()
        {
            return err("Invalid TAKM2 channel manifest");
        }
        let (nodes, metadata) = (parts[1].to_vec(), parts[2].to_vec());
        let heads = blobs(parts[4], info.records.len())?;
        let device_uids: HashSet<_> = nodes::analog_device_nodes(&nodes)?
            .into_iter()
            .map(|device| (device.uid, device.forceplate))
            .collect();
        let assets = nodes::skeleton_asset_set(&nodes)?;
        let marker_uids: HashSet<_> = nodes::parse_marker_nodes(&nodes, &assets)?
            .into_iter()
            .map(|marker| marker.uid)
            .collect();
        let props = metadata::parse_properties(&metadata)?;
        for (key, frame) in [("StartFrame", info.start), ("EndFrame", info.stop)] {
            if props
                .iter()
                .find(|(name, _)| name == key)
                .and_then(|(_, value)| metadata::python_int(value).ok())
                != Some(i64::from(frame))
            {
                return err("TAKM2 frame bounds disagree with metadata");
            }
        }

        // Validate the stored record headers with the existing channel parser.
        let mut channel_size = header.len();
        let mut records = Vec::with_capacity(heads.len());
        let mut record_bytes = header.to_vec();
        record_bytes[6..].copy_from_slice(&1u32.to_le_bytes());
        for (&(width, count, role, marker), head) in info.records.iter().zip(heads) {
            let count_at = head
                .len()
                .checked_sub(4)
                .ok_or_else(|| invalid("short channel header"))?;
            if u32_le(head, count_at) as usize != count {
                return err("TAKM2 channel header count mismatch");
            }
            record_bytes.truncate(header.len());
            record_bytes.extend_from_slice(head);
            record_bytes[header.len() + count_at..].fill(0);
            let record = channels::iter_channel_inventory(&record_bytes)?
                .pop()
                .ok_or_else(|| invalid("missing channel header"))?;
            let expected_role = if device_uids.contains(&(record.uid, true)) {
                Role::Plates
            } else if device_uids.contains(&(record.uid, false))
                || record.name.starts_with("AnalogChannel")
            {
                Role::Analog
            } else {
                Role::Spatial
            };
            let expected_marker = expected_role == Role::Spatial
                && record.name == "Translation"
                && marker_uids.contains(&record.uid);
            if width != record.dim() + 1 || role != expected_role || marker != expected_marker {
                return err("TAKM2 channel header disagrees with manifest");
            }
            channel_size = count
                .checked_mul(width * 4)
                .and_then(|bytes| bytes.checked_add(head.len()))
                .and_then(|bytes| bytes.checked_add(channel_size))
                .filter(|&bytes| bytes <= expanded)
                .ok_or_else(|| invalid("legacy channels exceed expanded byte limit"))?;
            records.push(Record {
                width,
                count,
                role,
                marker,
                head: head.to_vec(),
                runs: Vec::new(),
            });
        }
        Ok(Self {
            info,
            nodes,
            metadata,
            header,
            records,
            channel_size,
        })
    }

    fn read_presence(&mut self, raw: &[u8]) -> Result<()> {
        let count = self
            .records
            .iter()
            .filter(|r| r.role == Role::Spatial)
            .count();
        let mut first = None;
        for (rec, raw_runs) in self
            .records
            .iter_mut()
            .filter(|r| r.role == Role::Spatial)
            .zip(blobs(raw, count)?)
        {
            if raw_runs.len() % 8 != 0 || raw_runs.len() / 8 > rec.count {
                return err("Invalid TAKM2 presence run length");
            }
            let mut previous_end = None;
            let mut total = 0usize;
            for run in raw_runs.chunks_exact(8) {
                let (frame, n) = (u32_le(run, 0), u32_le(run, 4));
                let last = frame
                    .checked_add(n.saturating_sub(1))
                    .ok_or_else(|| invalid("presence frame overflow"))?;
                if n == 0
                    || previous_end.is_some_and(|end| end >= frame)
                    || (rec.marker && (frame < self.info.start || last > self.info.stop))
                {
                    return err("Invalid TAKM2 presence run");
                }
                total = total
                    .checked_add(n as usize)
                    .ok_or_else(|| invalid("presence count overflow"))?;
                previous_end = Some(last);
                rec.runs.push((frame, n));
                if rec.marker {
                    first = Some(first.map_or(frame, |f: u32| f.min(frame)));
                }
            }
            if total != rec.count {
                return err("TAKM2 presence count differs from channel header");
            }
        }
        if first.unwrap_or(self.info.start) != self.info.first {
            return err("TAKM2 first frame disagrees with marker presence");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Motion {
    First = 0,
    Preview = 1,
    Exact = 2,
}

impl Motion {
    fn class(frame: u32, info: &Info) -> Self {
        if frame == info.first {
            Self::First
        } else if frame % STRIDE == info.start % STRIDE {
            Self::Preview
        } else {
            Self::Exact
        }
    }

    fn count(self, record: &Record, info: &Info) -> usize {
        let first = usize::from(
            record
                .runs
                .iter()
                .any(|&(lo, n)| info.first >= lo && info.first - lo < n),
        );
        if self == Self::First {
            return first;
        }
        let preview: usize = record
            .runs
            .iter()
            .map(|&(lo, n)| {
                let skip = (STRIDE + info.start % STRIDE - lo % STRIDE) % STRIDE;
                n.saturating_sub(skip).div_ceil(STRIDE) as usize
            })
            .sum::<usize>()
            - first * usize::from(info.first % STRIDE == info.start % STRIDE);
        if self == Self::Preview {
            preview
        } else {
            record.count - first - preview
        }
    }

    fn frames<'a>(self, record: &'a Record, info: &'a Info) -> impl Iterator<Item = u32> + 'a {
        record.runs.iter().flat_map(move |&(lo, n)| {
            let start = match self {
                Self::First => info.first.checked_sub(lo).unwrap_or(n),
                Self::Preview => (STRIDE + info.start % STRIDE - lo % STRIDE) % STRIDE,
                Self::Exact => 0,
            };
            let step = if self == Self::Preview {
                STRIDE as usize
            } else {
                1
            };
            (start..n)
                .step_by(step)
                .map(move |offset| lo + offset)
                .take(if self == Self::First { 1 } else { usize::MAX })
                .filter(move |&frame| Self::class(frame, info) == self)
        })
    }
}

/// Analog channels selected by the original device roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalogKind {
    Plates,
    Other,
    All,
}

/// Reads and reuses decoded motion stages from an in-memory packed-member supplier.
pub struct Decoder {
    catalog: Catalog,
    limits: Limits,
    manifest: Manifest,
    motion: [Option<Vec<Vec<u8>>>; 3],
}

impl Decoder {
    /// Decode manifest and exact presence. Opaque layouts instead use full recovery.
    pub fn read(
        catalog: &[u8],
        limits: Limits,
        mut member: impl FnMut(&str) -> Result<Vec<u8>>,
    ) -> Result<Self> {
        let mut decoder = Self::read_manifest(catalog, limits, &mut member)?;
        let presence_cap = decoder
            .manifest
            .records
            .iter()
            .filter(|r| r.role == Role::Spatial)
            .fold(0usize, |total, r| {
                total.saturating_add(r.count.saturating_add(1).saturating_mul(8))
            });
        let presence_limits = Limits {
            expanded: limits.expanded.min(presence_cap),
            ..limits
        };
        decoder.manifest.read_presence(&decoder.catalog.expanded(
            1,
            presence_limits,
            &mut member,
        )?)?;
        Ok(decoder)
    }

    fn read_manifest(catalog: &[u8], limits: Limits, member: &mut MemberInput<'_>) -> Result<Self> {
        let catalog = Catalog::parse(catalog)?;
        if catalog.profile == Profile::OpaqueV1 {
            return err("TAKM2 opaque-v1 uses the full-read path");
        }
        let manifest = Manifest::read(&catalog.expanded(0, limits, member)?, limits.expanded)?;
        Ok(Self {
            catalog,
            limits,
            manifest,
            motion: [None, None, None],
        })
    }

    pub(crate) fn clear_motion(&mut self) {
        self.motion = [None, None, None];
        for record in &mut self.manifest.records {
            record.runs = Vec::new();
        }
    }

    pub fn nodes(&self) -> &[u8] {
        &self.manifest.nodes
    }
    pub fn metadata(&self) -> &[u8] {
        &self.manifest.metadata
    }
    pub fn first_frame(&self) -> u32 {
        self.manifest.info.first
    }

    /// Exact presence in spatial channel order, paired with each unmodified header.
    pub fn spatial_presence(&self) -> impl Iterator<Item = (&[u8], &[(u32, u32)])> {
        self.manifest
            .records
            .iter()
            .filter(|r| r.role == Role::Spatial)
            .map(|r| (r.head.as_slice(), r.runs.as_slice()))
    }

    pub(crate) fn record_heads(&self) -> impl Iterator<Item = (&[u8], Option<AnalogKind>)> {
        self.manifest.records.iter().map(|r| {
            let kind = match r.role {
                Role::Spatial => None,
                Role::Plates => Some(AnalogKind::Plates),
                Role::Analog => Some(AnalogKind::Other),
            };
            (r.head.as_slice(), kind)
        })
    }

    pub fn first_channels(
        &mut self,
        mut member: impl FnMut(&str) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        self.spatial(Motion::First, &mut member)
    }

    pub fn preview_channels(
        &mut self,
        mut member: impl FnMut(&str) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        self.spatial(Motion::Preview, &mut member)
    }

    pub fn exact_channels(
        &mut self,
        mut member: impl FnMut(&str) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        self.spatial(Motion::Exact, &mut member)
    }

    fn load_motion(&mut self, stage: Motion, member: &mut MemberInput<'_>) -> Result<()> {
        if self.motion[stage as usize].is_some() {
            return Ok(());
        }
        let (packed, _) = self.catalog.part(2 + stage as usize, self.limits, member)?;
        let records: Vec<_> = self
            .manifest
            .records
            .iter()
            .filter(|r| r.role == Role::Spatial)
            .collect();
        let info = &self.manifest.info;
        let counts: Vec<_> = records.iter().map(|r| stage.count(r, info)).collect();
        let words = counts
            .iter()
            .zip(&records)
            .map(|(&n, r)| n * (r.width - 1))
            .sum();
        let parts = blobs(&packed, 1)?;
        let values = decode_pco::<f32>(parts[0], words)?;
        let mut offset = 0;
        let rows = records
            .iter()
            .zip(counts)
            .map(|(r, n)| {
                let end = offset + n * (r.width - 1);
                let out = pco_rows(r.width, n, stage.frames(r, info), &values[offset..end]);
                offset = end;
                out
            })
            .collect::<Result<_>>()?;
        self.motion[stage as usize] = Some(rows);
        Ok(())
    }

    fn load_spatial(&mut self, through: Motion, member: &mut MemberInput<'_>) -> Result<()> {
        for stage in [Motion::First, Motion::Preview, Motion::Exact]
            .into_iter()
            .take(through as usize + 1)
        {
            self.load_motion(stage, member)?;
        }
        Ok(())
    }

    fn spatial(&mut self, through: Motion, member: &mut MemberInput<'_>) -> Result<Vec<u8>> {
        self.load_spatial(through, member)?;
        let count = self
            .manifest
            .records
            .iter()
            .filter(|r| r.role == Role::Spatial)
            .count();
        let mut output = make_header(self.manifest.header, count);
        for (index, r) in self
            .manifest
            .records
            .iter()
            .filter(|r| r.role == Role::Spatial)
            .enumerate()
        {
            self.append_spatial(&mut output, r, index, through);
        }
        Ok(output)
    }

    fn append_spatial(&self, out: &mut Vec<u8>, r: &Record, index: usize, through: Motion) {
        let width = r.width * 4;
        let records = self
            .motion
            .each_ref()
            .map(|rows| rows.as_ref().map_or(&[][..], |rows| rows[index].as_slice()));
        let size = records
            .iter()
            .take(through as usize + 1)
            .map(|r| r.len())
            .sum::<usize>();
        let count_at = r.head.len() - 4;
        out.extend_from_slice(&r.head[..count_at]);
        out.extend_from_slice(&((size / width) as u32).to_le_bytes());
        out.reserve(size);
        if through == Motion::First {
            out.extend_from_slice(records[0]);
            return;
        }
        let mut rows = records.map(|bytes| bytes.chunks_exact(width).peekable());
        // Merge the sorted streams without walking the missing frames.
        while let Some(stage) = rows
            .iter_mut()
            .take(through as usize + 1)
            .enumerate()
            .filter_map(|(i, it)| it.peek().map(|row| (i, u32_le(row, 0))))
            .min_by_key(|&(_, frame)| frame)
            .map(|(i, _)| i)
        {
            out.extend_from_slice(rows[stage].next().expect("peeked channel row"));
        }
    }

    pub fn analog_channels(
        &self,
        kind: AnalogKind,
        mut member: impl FnMut(&str) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        let mut plates = if kind != AnalogKind::Other {
            self.analog_rows(Role::Plates, &mut member)?
        } else {
            Vec::new()
        }
        .into_iter();
        let mut analog = if kind != AnalogKind::Plates {
            self.analog_rows(Role::Analog, &mut member)?
        } else {
            Vec::new()
        }
        .into_iter();
        let mut out = make_header(self.manifest.header, plates.len() + analog.len());
        for r in &self.manifest.records {
            let payload = match r.role {
                Role::Plates => plates.next(),
                Role::Analog => analog.next(),
                Role::Spatial => None,
            };
            if let Some(bytes) = payload {
                out.extend_from_slice(&r.head);
                out.extend_from_slice(&bytes);
            }
        }
        Ok(out)
    }

    fn analog_rows(&self, role: Role, member: &mut MemberInput<'_>) -> Result<Vec<Vec<u8>>> {
        let records: Vec<_> = self
            .manifest
            .records
            .iter()
            .filter(|r| r.role == role)
            .collect();
        let frames: usize = records.iter().map(|r| r.count).sum();
        let words: usize = records.iter().map(|r| r.width * r.count).sum();
        let mut offset = 0;
        if role == Role::Plates {
            let (packed, size) = self.catalog.part(5, self.limits, member)?;
            if size != words * 4 {
                return err("TAKM2 forceplate length differs from manifest");
            }
            let data = expand_zstd(&packed, size)?;
            return Ok(records
                .iter()
                .map(|r| {
                    let out = xor_rows(&data, words, offset, r.width, r.count);
                    offset += r.width * r.count;
                    out
                })
                .collect());
        }
        let (packed, _) = self.catalog.part(6, self.limits, member)?;
        let parts = blobs(&packed, 2)?;
        let frame_words = decode_pco::<u32>(parts[0], frames)?;
        let values = decode_pco::<f32>(parts[1], words - frames)?;
        let mut frame_offset = 0;
        records
            .iter()
            .map(|r| {
                let end = offset + r.count * (r.width - 1);
                let frame_end = frame_offset + r.count;
                let out = pco_rows(
                    r.width,
                    r.count,
                    frame_words[frame_offset..frame_end].iter().copied(),
                    &values[offset..end],
                );
                offset = end;
                frame_offset = frame_end;
                out
            })
            .collect()
    }

    fn all_channels(&mut self, member: &mut MemberInput<'_>) -> Result<Vec<u8>> {
        self.load_spatial(Motion::Exact, member)?;
        let mut plates = self.analog_rows(Role::Plates, member)?.into_iter();
        let mut analog = self.analog_rows(Role::Analog, member)?.into_iter();
        let mut out = Vec::with_capacity(self.manifest.channel_size);
        out.extend_from_slice(&self.manifest.header);
        let mut index = 0;
        for r in &self.manifest.records {
            match r.role {
                Role::Spatial => {
                    self.append_spatial(&mut out, r, index, Motion::Exact);
                    index += 1;
                }
                Role::Plates | Role::Analog => {
                    out.extend_from_slice(&r.head);
                    out.extend_from_slice(
                        &if r.role == Role::Plates {
                            plates.next()
                        } else {
                            analog.next()
                        }
                        .ok_or_else(|| invalid("missing analog channel"))?,
                    );
                }
            }
        }
        if out.len() != self.manifest.channel_size {
            return err("Rebuilt TAKM2 channel length mismatch");
        }
        Ok(out)
    }
}

fn make_header(mut header: [u8; 10], count: usize) -> Vec<u8> {
    header[6..].copy_from_slice(&(count as u32).to_le_bytes());
    header.to_vec()
}

fn pco_rows(
    width: usize,
    count: usize,
    frames: impl Iterator<Item = u32>,
    values: &[f32],
) -> Result<Vec<u8>> {
    let mut result = Vec::with_capacity(width * count * 4);
    for (i, frame) in frames.enumerate() {
        if i >= count {
            return err("Too many TAKM2 PCo sample frames");
        }
        result.extend_from_slice(&frame.to_le_bytes());
        for lane in 0..width - 1 {
            result.extend_from_slice(&values[lane * count + i].to_bits().to_le_bytes());
        }
    }
    if result.len() != width * count * 4 {
        return err("TAKM2 PCo sample count mismatch");
    }
    Ok(result)
}

fn xor_rows(data: &[u8], plane: usize, offset: usize, width: usize, count: usize) -> Vec<u8> {
    let mut result = vec![0; width * count * 4];
    for lane in 0..width {
        let mut last = [0; 4];
        for i in 0..count {
            let word = offset + lane * count + i;
            for byte in 0..4 {
                last[byte] ^= data[byte * plane + word];
            }
            let out = (i * width + lane) * 4;
            result[out..out + 4].copy_from_slice(&last);
        }
    }
    result
}

/// Detect the opaque profile from a packed-member catalog.
pub fn is_opaque(catalog: &[u8]) -> Result<bool> {
    Ok(Catalog::parse(catalog)?.profile == Profile::OpaqueV1)
}

#[derive(Clone, Copy)]
struct Extent {
    header: u64,
    size: u64,
    next: u64,
}

impl Extent {
    fn data(self) -> u64 {
        self.header + BLOCK as u64
    }
    fn end(self) -> u64 {
        self.data() + self.size
    }
}

fn tar_header(block: &[u8; BLOCK], offset: u64, len: u64) -> Result<(String, Extent)> {
    let header = tar::Header::from_byte_slice(block);
    if header.as_ustar().is_none()
        || !header.entry_type().is_file()
        || block[345..500].iter().any(|&b| b != 0)
        || block[124] & 0x80 != 0
    {
        return err("TAKM2 expected an ordinary USTAR member");
    }
    let checksum: u32 = block
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u32::from(b)
            }
        })
        .sum();
    if header.cksum()? != checksum {
        return err("TAKM2 USTAR header checksum mismatch");
    }
    let name = header.path_bytes();
    let name = std::str::from_utf8(&name).map_err(invalid)?.to_owned();
    let size = header.entry_size()?;
    let data = offset
        .checked_add(BLOCK as u64)
        .ok_or_else(|| invalid("USTAR offset overflow"))?;
    let padded = size
        .checked_add(BLOCK as u64 - 1)
        .ok_or_else(|| invalid("USTAR size overflow"))?
        / BLOCK as u64
        * BLOCK as u64;
    let next = data
        .checked_add(padded)
        .filter(|&end| end <= len)
        .ok_or_else(|| invalid("truncated USTAR member"))?;
    Ok((
        name,
        Extent {
            header: offset,
            size,
            next,
        },
    ))
}

fn named_header(name: &str, pos: u64, len: u64, read: &mut ReadAt<'_>) -> Result<Extent> {
    let mut block = [0u8; BLOCK];
    read(&mut block, pos)?;
    let (actual, extent) = tar_header(&block, pos, len)?;
    if actual != name {
        return err(format!("Unexpected TAKM2 member {actual}, expected {name}"));
    }
    Ok(extent)
}

type ReadAt<'a> = dyn FnMut(&mut [u8], u64) -> Result<()> + 'a;

fn read_slice(start: u64, size: u64, limit: usize, read: &mut ReadAt<'_>) -> Result<Vec<u8>> {
    let size = usize::try_from(size).map_err(invalid)?;
    if size > limit {
        return err("TAKM2 read exceeds byte limit");
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(invalid)?;
    bytes.resize(size, 0);
    read(&mut bytes, start)?;
    Ok(bytes)
}

struct Directory {
    catalog_extent: Extent,
    catalog: Vec<u8>,
    parts: [Option<Extent>; 8],
    seal: Extent,
}

impl Directory {
    fn read(len: u64, read: &mut ReadAt<'_>) -> Result<Self> {
        let catalog_extent = named_header(CATALOG, 0, len, read)?;
        let catalog = read_slice(
            catalog_extent.data(),
            catalog_extent.size,
            CATALOG_LIMIT,
            read,
        )?;
        let parsed = Catalog::parse(&catalog)?;
        let mut pos = catalog_extent.next;
        let mut parts = [None; 8];
        for (i, descriptor) in parsed.parts().into_iter().enumerate() {
            if let Some(Descriptor(name, _, expanded, _)) = descriptor {
                let entry = named_header(name, pos, len, read)?;
                if entry.size > LIMIT as u64 || *expanded > LIMIT || entry.next > LIMIT as u64 {
                    return err("TAKM2 archive exceeds fixed-body byte limit");
                }
                pos = entry.next;
                parts[i] = Some(entry);
            }
        }
        let seal = named_header(SEAL, pos, len, read)?;
        if seal.size != SEAL_LEN {
            return err("Invalid TAKM2 seal size");
        }
        // The measured catalog may label the base Comment header with a decimal offset.
        if let Some(offset) = parsed.tar_comment_header {
            if offset.parse::<u64>().ok() != Some(seal.next) {
                return err("TAKM2 catalog Comment offset disagrees with USTAR headers");
            }
        }
        Ok(Self {
            catalog_extent,
            catalog,
            parts,
            seal,
        })
    }

    fn member(&self, name: &str, limit: usize, read: &mut ReadAt<'_>) -> Result<Vec<u8>> {
        let index = PARTS
            .iter()
            .position(|p| p.1 == name)
            .ok_or_else(|| invalid("unknown packed member"))?;
        let entry = self.parts[index].ok_or_else(|| invalid("missing packed member"))?;
        read_slice(entry.data(), entry.size, limit, read)
    }
}

/// A seekable layout-2 archive. Opening scans only the catalog and fixed member headers.
pub struct Archive<'a> {
    file: &'a File,
    len: u64,
    directory: Directory,
}

impl<'a> Archive<'a> {
    pub fn open(file: &'a File, file_len: u64) -> Result<Self> {
        let directory = Directory::read(file_len, &mut |buf, at| read_exact_at(file, buf, at))?;
        Ok(Self {
            file,
            len: file_len,
            directory,
        })
    }

    pub fn catalog_bytes(&self) -> &[u8] {
        &self.directory.catalog
    }
    pub fn member(&self, name: &str) -> Result<Vec<u8>> {
        self.directory.member(name, LIMIT, &mut |buf, at| {
            read_exact_at(self.file, buf, at)
        })
    }

    pub fn decoder(&self) -> Result<Decoder> {
        Decoder::read(self.catalog_bytes(), Limits::default(), |name| {
            self.member(name)
        })
    }

    pub(crate) fn analog_decoder(&self) -> Result<Decoder> {
        Decoder::read_manifest(self.catalog_bytes(), Limits::default(), &mut |name| {
            self.member(name)
        })
    }

    pub fn fixed_len(&self) -> u64 {
        self.directory.seal.header
    }

    /// Physical immutable prefix length and stored seal (without hashing the prefix).
    pub fn fixed_identity(&self) -> Result<(u64, [u8; 32])> {
        let mut seal = [0; 32];
        read_exact_at(self.file, &mut seal, self.directory.seal.data())?;
        Ok((self.fixed_len(), seal))
    }

    /// Validate the immutable USTAR prefix. Logical recovery separately validates the v1 seal.
    pub fn verify_seal(&self) -> Result<(u64, [u8; 32])> {
        self.verify_fixed_body(None)
    }

    /// Check the seal and cataloged packed bytes before writing any Comment suffix.
    pub(super) fn verify_append_body(&self) -> Result<()> {
        let catalog = Catalog::parse(self.catalog_bytes())?;
        self.verify_fixed_body(Some(&catalog)).map(|_| ())
    }

    fn verify_fixed_body(&self, catalog: Option<&Catalog>) -> Result<(u64, [u8; 32])> {
        let (len, seal) = self.fixed_identity()?;
        let mut buf = vec![0; IO_CHUNK];
        let mut outer = Sha256::new();
        let mut pos = 0;
        let mut hash_through = |end, mut packed: Option<&mut Sha256>| {
            read_file_chunks(self.file, pos, end, &mut buf, |bytes| {
                outer.update(bytes);
                if let Some(digest) = &mut packed {
                    digest.update(bytes);
                }
            })?;
            pos = end;
            Ok::<_, TakError>(())
        };
        let mut mismatch = None;
        if let Some(catalog) = catalog {
            for (descriptor, extent) in catalog.parts().into_iter().zip(self.directory.parts) {
                if let (Some(Descriptor(name, _, _, expected)), Some(extent)) = (descriptor, extent)
                {
                    hash_through(extent.data(), None)?;
                    let mut packed = Sha256::new();
                    hash_through(extent.end(), Some(&mut packed))?;
                    if format!("{:x}", packed.finalize()) != *expected && mismatch.is_none() {
                        mismatch = Some(name);
                    }
                }
            }
        }
        hash_through(len, None)?;
        if outer.finalize().as_slice() != seal {
            return err("TAKM2 immutable SHA-256 mismatch");
        }
        let extents = std::iter::once(self.directory.catalog_extent)
            .chain(self.directory.parts.into_iter().flatten())
            .chain(std::iter::once(self.directory.seal));
        let zeros = [0; BLOCK];
        let mut padding = [0; BLOCK];
        for extent in extents {
            let n = (extent.next - extent.end()) as usize;
            read_exact_at(self.file, &mut padding[..n], extent.end())?;
            if padding[..n] != zeros[..n] {
                return err("Invalid TAKM2 USTAR padding");
            }
        }
        if let Some(name) = mismatch {
            return err(format!("TAKM2 packed SHA-256 mismatch ({name})"));
        }
        Ok((len, seal))
    }

    /// Read the full Comment range in one positional read, then replay complete members.
    pub fn journal(&self) -> Result<Journal> {
        let start = self.directory.seal.next;
        let data = read_slice(start, self.len - start, JOURNAL_LIMIT, &mut |buf, at| {
            read_exact_at(self.file, buf, at)
        })?;
        let expected_base = Catalog::parse(self.catalog_bytes())?.tar_comment_size;
        parse_journal(&data, start, expected_base)
    }

    /// Restore the logical layout-1 header, streams, table and seal.
    pub fn restore(&self) -> Result<Vec<u8>> {
        self.verify_seal()?;
        self.restore_logical()
    }

    pub(crate) fn restore_logical(&self) -> Result<Vec<u8>> {
        let catalog = Catalog::parse(self.catalog_bytes())?;
        if catalog.profile == Profile::OpaqueV1 {
            return restore_opaque(self.catalog_bytes(), Limits::default(), |name| {
                self.member(name)
            });
        }
        let recovery = catalog.expanded(7, Limits::default(), &mut |name| self.member(name))?;
        let mut decoder = self.decoder()?;
        let channels = decoder.all_channels(&mut |name| self.member(name))?;
        rebuild(&decoder.manifest, &channels, &recovery)
    }

    pub(crate) fn read_take(&self, path: &Path) -> Result<Take> {
        if is_opaque(self.catalog_bytes())? {
            return decode_legacy_take(path, self.len, &self.restore()?);
        }
        let mut decoder = self.decoder()?;
        let channels = decoder.all_channels(&mut |name| self.member(name))?;
        crate::tak::decode_streams(
            path,
            self.len as usize,
            decoder.nodes(),
            &channels,
            decoder.metadata(),
        )
    }
}

impl Decoder {
    /// Decode a first-pose take after validating the selected compressed member.
    pub fn first_take(
        &mut self,
        path: &Path,
        file_len: usize,
        member: impl FnMut(&str) -> Result<Vec<u8>>,
    ) -> Result<Take> {
        let channels = self.first_channels(member)?;
        crate::tak::decode_markers(
            path,
            file_len,
            self.nodes(),
            &channels,
            self.metadata(),
            false,
        )
    }

    /// Decode the stride-eight preview plus the original first pose.
    pub fn preview_take(
        &mut self,
        path: &Path,
        file_len: usize,
        member: impl FnMut(&str) -> Result<Vec<u8>>,
    ) -> Result<Take> {
        let channels = self.preview_channels(member)?;
        crate::tak::decode_markers(
            path,
            file_len,
            self.nodes(),
            &channels,
            self.metadata(),
            false,
        )
    }
}

/// The logical legacy Comment CSV and the physical next append location.
pub struct Journal {
    pub bytes: Vec<u8>,
    pub append_offset: u64,
    pub next_ordinal: u64,
    pub cut: bool,
}

fn journal_entry(data: &[u8], at: usize) -> Result<(String, Extent)> {
    let header = data
        .get(at..at + BLOCK)
        .ok_or_else(|| invalid("short Comment header"))?;
    let (name, extent) = tar_header(
        header.try_into().expect("whole USTAR block"),
        at as u64,
        data.len() as u64,
    )?;
    if data[extent.end() as usize..extent.next as usize]
        .iter()
        .any(|&b| b != 0)
    {
        return err("Invalid TAKM2 Comment member padding");
    }
    Ok((name, extent))
}

fn event_name(name: &str) -> Option<(u64, &str)> {
    let (ordinal, digest) = name
        .strip_prefix("comments/")?
        .strip_suffix(".csv")?
        .split_once('-')?;
    if ordinal.len() != 16
        || !ordinal.bytes().all(|b| b.is_ascii_digit())
        || !lower_hex_digest(digest)
    {
        return None;
    }
    Some((ordinal.parse().ok()?, digest))
}

fn event_at(data: &[u8], at: usize) -> Result<(u64, Extent)> {
    let (name, entry) = journal_entry(data, at)?;
    let (ordinal, digest) =
        event_name(&name).ok_or_else(|| invalid("invalid Comment member name"))?;
    if sha256_hex(&data[entry.data() as usize..entry.end() as usize]) != digest {
        return err("TAKM2 Comment member SHA-256 mismatch");
    }
    Ok((ordinal, entry))
}

fn parse_journal(data: &[u8], start: u64, expected_base: Option<u64>) -> Result<Journal> {
    let (name, base) = journal_entry(data, 0)?;
    if name != BASE || expected_base.is_some_and(|size| size != base.size) {
        return err("Invalid TAKM2 base Comment member");
    }
    let mut bytes = data[base.data() as usize..base.end() as usize].to_vec();
    let mut pos = base.next as usize;
    let mut ordinal = 1u64;
    let cut = loop {
        let tail = &data[pos..];
        if tail.iter().all(|&b| b == 0) {
            break tail.len() < BLOCK * 2 || tail.len() % BLOCK != 0;
        }
        if let Ok((number, entry)) = event_at(data, pos) {
            if number != ordinal {
                return err("TAKM2 Comment member ordinal is out of sequence");
            }
            bytes.extend_from_slice(&data[entry.data() as usize..entry.end() as usize]);
            ordinal += 1;
            pos = entry.next as usize;
            continue;
        }
        // A valid later member establishes corruption before the final append.
        if (pos + BLOCK..data.len()).step_by(BLOCK).any(|offset| {
            data[offset..].starts_with(b"comments/") && event_at(data, offset).is_ok()
        }) {
            return err("TAKM2 Comment damage precedes a later committed member");
        }
        break true;
    };
    Ok(Journal {
        bytes,
        append_offset: start + pos as u64,
        next_ordinal: ordinal,
        cut,
    })
}

/// Restore an opaque-v1 payload supplied by the local archive or a relay.
pub fn restore_opaque(
    catalog: &[u8],
    limits: Limits,
    mut member: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    let catalog = Catalog::parse(catalog)?;
    if catalog.profile != Profile::OpaqueV1 {
        return err("Expected opaque-v1 TAKM2 profile");
    }
    let raw = catalog.expanded(7, limits, &mut member)?;
    legacy_index(&raw)?;
    Ok(raw)
}

pub(crate) fn legacy_index(raw: &[u8]) -> Result<Index> {
    let (table, _, fixed) = parse_header(raw, raw.len() as u64)?;
    if fixed + SEAL_LEN != raw.len() as u64 {
        return err("Unexpected bytes after recovered TAKM1 seal");
    }
    if Sha256::digest(&raw[..fixed as usize]).as_slice() != &raw[fixed as usize..] {
        return err("Rebuilt TAKM1 SHA-256 mismatch");
    }
    let value: Value =
        serde_json::from_slice(&raw[table as usize..fixed as usize]).map_err(invalid)?;
    if raw[12..16]
        .iter()
        .chain(&raw[40..HEADER_LEN as usize])
        .any(|&b| b != 0)
        || value["format"] != "takm"
        || value["layout"] != LAYOUT
    {
        return err("Invalid rebuilt TAKM1 header or table version");
    }
    let index = decode_table(&value, table, fixed)?;
    super::validate_ledger(&index)?;
    validate_extents(&index)?;
    Ok(index)
}

fn sorted_extents(index: &Index) -> Vec<usize> {
    let mut order: Vec<_> = (0..index.streams.len()).collect();
    order.sort_by_key(|&i| index.streams[i].offset);
    order
}

pub(super) fn validate_extents(index: &Index) -> Result<()> {
    let mut pos = HEADER_LEN;
    for i in sorted_extents(index) {
        let stream = &index.streams[i];
        if stream.offset < pos {
            return err("Overlapping recovered TAKM1 streams");
        }
        pos = stream.offset + stream.size;
    }
    Ok(())
}

fn rebuild(manifest: &Manifest, channels: &[u8], recovery: &[u8]) -> Result<Vec<u8>> {
    let opaque_count = manifest.info.streams - 3;
    let parts = blobs(
        recovery,
        4usize
            .checked_add(opaque_count)
            .ok_or_else(|| invalid("too many streams"))?,
    )?;
    let (header, table, seal) = (parts[0], parts[1], parts[2]);
    if header.len() != HEADER_LEN as usize || seal.len() != SEAL_LEN as usize {
        return err("Invalid recovered TAKM1 header or seal length");
    }
    let fixed = u64_le(header, 32);
    let end = fixed
        .checked_add(SEAL_LEN)
        .filter(|&end| end <= LIMIT as u64)
        .ok_or_else(|| invalid("recovered TAKM1 source exceeds byte limit"))?;
    let (table_at, table_size, _) = parse_header(header, end)?;
    if table_size != table.len() as u64 {
        return err("Recovered TAKM1 table length mismatch");
    }
    let value: Value = serde_json::from_slice(table).map_err(invalid)?;
    let index = decode_table(&value, table_at, fixed)?;
    if index.streams.len() != manifest.info.streams {
        return err("Recovered TAKM1 stream count mismatch");
    }
    validate_extents(&index)?;
    let mut streams = vec![None; index.streams.len()];
    for ((id, name), bytes) in manifest
        .info
        .stream_ids
        .into_iter()
        .zip(["Nodes.dat", "MetaData.dat", "Channels.dat"])
        .zip([
            manifest.nodes.as_slice(),
            manifest.metadata.as_slice(),
            channels,
        ])
    {
        if index.streams.iter().rposition(|s| s.name == name) != Some(id) {
            return err("TAKM2 stream ID differs from recovered TAKM1 table");
        }
        streams[id] = Some(bytes);
    }
    for (slot, bytes) in streams.iter_mut().filter(|s| s.is_none()).zip(&parts[4..]) {
        *slot = Some(bytes);
    }
    let gaps = blobs(parts[3], index.streams.len() + 1)?;
    let mut raw = Vec::new();
    raw.try_reserve_exact(end as usize).map_err(invalid)?;
    raw.extend_from_slice(header);
    for (ordinal, gap) in sorted_extents(&index).into_iter().zip(&gaps) {
        let stream = &index.streams[ordinal];
        let bytes = streams[ordinal].ok_or_else(|| invalid("missing recovered stream"))?;
        if (raw.len() as u64).checked_add(gap.len() as u64) != Some(stream.offset)
            || bytes.len() as u64 != stream.size
        {
            return err("Recovered TAKM1 stream length or gap mismatch");
        }
        raw.extend_from_slice(gap);
        raw.extend_from_slice(bytes);
    }
    let tail_gap = gaps[gaps.len() - 1];
    if (raw.len() as u64).checked_add(tail_gap.len() as u64) != Some(table_at) {
        return err("Recovered TAKM1 final gap mismatch");
    }
    raw.extend_from_slice(tail_gap);
    raw.extend_from_slice(table);
    raw.extend_from_slice(seal);
    legacy_index(&raw)?;
    Ok(raw)
}

/// Decode a restored opaque layout without a staging file. Use the archive path and size.
pub fn decode_legacy_take(path: &Path, len: u64, raw: &[u8]) -> Result<Take> {
    let index = legacy_index(raw)?;
    let get = |name: &str| -> Result<&[u8]> {
        let s = index
            .last(name)
            .ok_or_else(|| invalid("required marker streams missing"))?;
        Ok(&raw[s.offset as usize..(s.offset + s.size) as usize])
    };
    crate::tak::decode_streams(
        path,
        len as usize,
        get("Nodes.dat")?,
        get("Channels.dat")?,
        get("MetaData.dat")?,
    )
}

pub(crate) fn metadata_at(
    len: u64,
    limits: Limits,
    mut read: impl FnMut(&mut [u8], u64) -> Result<()>,
) -> Result<Vec<u8>> {
    let archive = Directory::read(len, &mut read)?;
    let catalog = Catalog::parse(&archive.catalog)?;
    let mut member = |name: &str| archive.member(name, limits.packed, &mut read);
    if catalog.profile == Profile::OpaqueV1 {
        let raw = restore_opaque(&archive.catalog, limits, member)?;
        let index = legacy_index(&raw)?;
        let s = index
            .last("MetaData.dat")
            .ok_or_else(|| invalid("missing metadata"))?;
        return Ok(raw[s.offset as usize..(s.offset + s.size) as usize].to_vec());
    }
    let raw = catalog.expanded(0, limits, &mut member)?;
    Ok(blobs(&raw, 5)?[2].to_vec())
}

pub(crate) fn has_magic(magic: &[u8]) -> bool {
    magic.starts_with(b"takm.jso")
}

pub(crate) fn is_archive(file: &File, len: u64) -> Result<bool> {
    if len < 8 {
        return Ok(false);
    }
    let mut magic = [0; 8];
    read_exact_at(file, &mut magic, 0)?;
    Ok(has_magic(&magic))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(dst: &mut Vec<u8>, name: &str, data: &[u8]) {
        let mut header = tar::Header::new_ustar();
        header.set_path(name).unwrap();
        header.set_size(data.len() as u64);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        dst.extend_from_slice(header.as_bytes());
        dst.extend_from_slice(data);
        dst.resize(dst.len().next_multiple_of(BLOCK), 0);
    }

    #[test]
    fn journal_keeps_exact_csv_at_each_torn_append_boundary() {
        let base = b"base\r\n";
        let first = b"\"multiline\r\nfield\",one\r\n";
        let second = b"second member\r\n";
        let mut raw = Vec::new();
        member(&mut raw, BASE, base);
        member(
            &mut raw,
            &format!("comments/{:016}-{}.csv", 1, sha256_hex(first)),
            first,
        );
        let old_end = raw.len();
        member(
            &mut raw,
            &format!("comments/{:016}-{}.csv", 2, sha256_hex(second)),
            second,
        );
        let new_end = raw.len();
        raw.resize(new_end + 2 * BLOCK, 0);
        for n in old_end..=raw.len() {
            let journal = parse_journal(&raw[..n], 5000, Some(base.len() as u64)).unwrap();
            let bytes = if n < new_end {
                [&base[..], &first[..]].concat()
            } else {
                [&base[..], &first[..], &second[..]].concat()
            };
            assert_eq!(journal.bytes, bytes, "at byte {n}");
            assert_eq!(
                journal.append_offset as usize,
                5000 + if n < new_end { old_end } else { new_end }
            );
            assert_eq!(journal.cut, n != raw.len());
        }
        raw[3 * BLOCK] ^= 1;
        assert!(parse_journal(&raw, 5000, None).is_err());
    }

    #[test]
    fn packed_pco_keeps_float_bits_and_refuses_wrong_count_or_trailing_data() {
        let bits = [0, 0x8000_0000, 0x7fc0_1234, 0xff80_0000, 0x0000_0001];
        let values = bits.map(f32::from_bits);
        let raw = pco::standalone::simple_compress(&values, &pco::ChunkConfig::default()).unwrap();
        let restored = decode_pco::<f32>(&raw, bits.len()).unwrap();
        assert_eq!(
            restored.into_iter().map(f32::to_bits).collect::<Vec<_>>(),
            bits
        );
        assert!(decode_pco::<f32>(&raw, bits.len() - 1).is_err());
        assert!(decode_pco::<f32>(&raw, bits.len() + 1).is_err());
        let mut surplus = raw;
        surplus.push(0);
        assert!(decode_pco::<f32>(&surplus, bits.len()).is_err());
    }

    #[test]
    fn recovered_v1_accepts_sparse_ordinals_and_duplicate_names() {
        let tables = |second_index| {
            json!({
                "format": "takm", "layout": 1, "made_by": "test", "made_at": "test",
                "source": {"name":"take.tak", "size":128, "sha256":"0".repeat(64)},
                "streams": [
                    {"name":"opaque", "index":101, "offset":66, "size":1},
                    {"name":"opaque", "index":second_index, "offset":69, "size":1}
                ], "dropped": []
            })
        };
        let logical = |second_index| {
            let mut bytes = vec![0; 64];
            bytes[..8].copy_from_slice(&MAGIC);
            bytes[8..12].copy_from_slice(&LAYOUT.to_le_bytes());
            bytes.extend_from_slice(b"  x  y ");
            let off = bytes.len() as u64;
            let table = serde_json::to_vec(&tables(second_index)).unwrap();
            let fixed = off + table.len() as u64;
            bytes[16..24].copy_from_slice(&off.to_le_bytes());
            bytes[24..32].copy_from_slice(&(table.len() as u64).to_le_bytes());
            bytes[32..40].copy_from_slice(&fixed.to_le_bytes());
            bytes.extend_from_slice(&table);
            bytes.extend_from_slice(&Sha256::digest(&bytes));
            bytes
        };
        let index = legacy_index(&logical(102)).unwrap();
        assert_eq!(
            index
                .streams
                .iter()
                .map(|row| row.index)
                .collect::<Vec<_>>(),
            [101, 102]
        );
        assert_eq!(index.last("opaque").unwrap().offset, 69);
        assert!(legacy_index(&logical(101)).is_err());
    }
}
