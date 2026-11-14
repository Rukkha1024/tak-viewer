//! `Channels.dat` parsing for marker translation channels and analog devices.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::container::{f32_at, u32_at, uid_from};
use super::metadata::python_int;
use super::nodes::MarkerNode;
use super::{err, AnalogChannel, AnalogDevice, Result, TakError, Uid};

pub const CHANNELS_HEADER: [u8; 6] = [0xf3, 0x0a, 0x39, 0x4c, 0x02, 0x00];
pub const TRANSLATION: &[u8] =
    b"\x0f\x00Vector3fChannel\x04\x0b\x00T\x00r\x00a\x00n\x00s\x00l\x00a\x00t\x00i\x00o\x00n\x00";
/// Python `normalize_trajectories`: UInt16 identity range.
const MAX_TRAJECTORIES: usize = 65536;

/// `CHANNEL_DIMENSIONS`.
pub fn channel_dim(kind: &str) -> Option<usize> {
    match kind {
        "FloatChannel" => Some(1),
        "Float16Channel" => Some(16),
        "Vector3fChannel" => Some(3),
        "QuaternionChannel" => Some(4),
        "class MoCapCore::cChannel<struct MoCapCore::sArrayChannelValues<float,16> >" => Some(16),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct ChannelRecord {
    pub start: usize,
    pub end: usize,
    pub kind: String,
    pub name: String,
    pub uid: Uid,
    pub default_start: usize,
    pub n_samples: usize,
    pub data_start: usize,
    pub data_end: usize,
}

impl ChannelRecord {
    pub fn dim(&self) -> usize {
        channel_dim(&self.kind).unwrap_or(0)
    }

    pub fn default<'c>(&self, channels: &'c [u8]) -> &'c [u8] {
        &channels[self.default_start..self.default_start + 4 * self.dim()]
    }
}

/// `iter_channel_inventory`: walk declared records by lengths; whole stream must be consumed.
pub fn iter_channel_inventory(channels: &[u8]) -> Result<Vec<ChannelRecord>> {
    if channels.len() < 10 || channels[..6] != CHANNELS_HEADER {
        return err("Unknown Channels version");
    }
    let count = u32_at(channels, 6)?;
    let len = channels.len();
    let mut pos = 10usize;
    let mut out = Vec::new();
    for _ in 0..count {
        let start = pos;
        if pos + 2 > len {
            return err("Truncated channel type length");
        }
        let length = u16::from_le_bytes([channels[pos], channels[pos + 1]]) as usize;
        let cur = pos + 2 + length;
        if cur + 3 > len {
            return err("Truncated channel type/version");
        }
        let kind_bytes = &channels[pos + 2..cur];
        if !kind_bytes.is_ascii() {
            return err("Invalid channel type encoding");
        }
        let kind = String::from_utf8(kind_bytes.to_vec()).map_err(|_| TakError("Invalid channel type encoding".into()))?;
        let Some(dim) = channel_dim(&kind) else {
            return err(format!("Unsupported structural channel type: {kind}"));
        };
        if channels[cur] != 4 {
            return err("Unknown channel record version");
        }
        let chars = u16::from_le_bytes([channels[cur + 1], channels[cur + 2]]) as usize;
        let uid_offset = cur + 3 + chars * 2;
        let count_offset = uid_offset + 16 + 4 * dim;
        if count_offset + 4 > len {
            return err("Truncated channel property/UID/default/count");
        }
        let name = super::container::utf16le_strict(&channels[cur + 3..uid_offset])
            .map_err(|_| TakError("Invalid channel property encoding".into()))?;
        let n = u32_at(channels, count_offset)? as u64;
        let data_start = count_offset + 4;
        let end = data_start as u64 + n * (4 + 4 * dim as u64);
        if end > len as u64 {
            return err("Truncated channel samples");
        }
        let end = end as usize;
        out.push(ChannelRecord {
            start,
            end,
            kind,
            name,
            uid: uid_from(&channels[uid_offset..uid_offset + 16]),
            default_start: uid_offset + 16,
            n_samples: n as usize,
            data_start,
            data_end: end,
        });
        pos = end;
    }
    if pos != len {
        return err("Unexpected trailing channel bytes");
    }
    Ok(out)
}

/// One trajectory in channel order: node descriptor + its `<Ifff` sample bytes.
#[derive(Clone, Debug)]
pub struct MarkerRow {
    pub node: MarkerNode,
    pub data_start: usize,
    pub data_end: usize,
}

/// Frame order/range check of `<I12x` records; returns (first, last) frame when non-empty.
fn check_frames(raw16: &[u8], lo: i64, hi: i64) -> Result<Option<(u32, u32)>> {
    let mut previous: i64 = -1;
    let mut first = None;
    for rec in raw16.chunks_exact(16) {
        let frame = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
        let f = frame as i64;
        if !(lo <= f && f <= hi) || f <= previous {
            return err("Invalid marker frame order or range");
        }
        first.get_or_insert(frame);
        previous = f;
    }
    Ok(first.map(|f| (f, previous as u32)))
}

fn uid_counts<'a>(uids: impl Iterator<Item = &'a Uid>) -> HashMap<Uid, usize> {
    let mut counts = HashMap::new();
    for uid in uids {
        *counts.entry(*uid).or_insert(0) += 1;
    }
    counts
}

fn suffix_error(detail: &str) -> TakError {
    TakError(format!("Marker/channel suffix mapping is incomplete or ambiguous: {detail}"))
}

/// `_read_marker_suffix`.
fn read_marker_suffix(channels: &[u8], markers: &[MarkerNode], lo: i64, hi: i64) -> Result<Vec<MarkerRow>> {
    if channels.len() < 10 || !channels.starts_with(&CHANNELS_HEADER) {
        return err("Unknown Channels version");
    }
    if lo > hi {
        return err("Invalid metadata frame range");
    }
    let mut by_uid: HashMap<Uid, &MarkerNode> = HashMap::new();
    for marker in markers {
        match by_uid.get(&marker.uid) {
            Some(first) if *first != marker => {
                return err(format!(
                    "ambiguous duplicate Marker UID: differing node descriptors for {}",
                    super::uid_hex(&marker.uid)
                ))
            }
            Some(_) => {}
            None => {
                by_uid.insert(marker.uid, marker);
            }
        }
    }
    let expected = uid_counts(markers.iter().map(|m| &m.uid));
    let len = channels.len();
    // pos -> (end, uid, data_start)
    let mut candidates: BTreeMap<usize, (usize, Uid, usize)> = BTreeMap::new();
    let mut ends: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut errors: Vec<String> = Vec::new();
    for pos in super::container::find_all(channels, TRANSLATION, 10) {
        let uid_start = pos + TRANSLATION.len();
        if uid_start + 16 > len {
            continue;
        }
        let uid = uid_from(&channels[uid_start..uid_start + 16]);
        if !by_uid.contains_key(&uid) {
            continue;
        }
        let attempt = (|| -> Result<(usize, usize)> {
            let data_start = uid_start + 32;
            if data_start > len {
                return err("Truncated marker channel header");
            }
            if channels[uid_start + 16..uid_start + 28] != [0u8; 12] {
                return err("Unknown marker channel flags");
            }
            let n = u32_at(channels, uid_start + 28)? as u64;
            let end = data_start as u64 + n * 16;
            if end > len as u64 {
                return err("Truncated marker samples");
            }
            let end = end as usize;
            if end != len && !channels[end..].starts_with(TRANSLATION) {
                return err("Unexpected record boundary in marker channel suffix");
            }
            check_frames(&channels[data_start..end], lo, hi)?;
            Ok((data_start, end))
        })();
        match attempt {
            Ok((data_start, end)) => {
                candidates.insert(pos, (end, uid, data_start));
                ends.entry(end).or_default().push(pos);
            }
            Err(e) => errors.push(e.0),
        }
    }
    let mut lengths: HashMap<usize, usize> = HashMap::from([(len, 0)]);
    let mut starts = Vec::new();
    for (&pos, &(end, _, _)) in candidates.iter().rev() {
        if let Some(&l) = lengths.get(&end) {
            lengths.insert(pos, l + 1);
            if l + 1 == markers.len() {
                starts.push(pos);
            }
        }
    }
    let mut valid: Vec<(usize, Vec<usize>)> = Vec::new();
    for start in starts {
        let mut chain = Vec::new();
        let mut pos = start;
        while pos < len {
            chain.push(pos);
            pos = candidates[&pos].0;
        }
        if uid_counts(chain.iter().map(|p| &candidates[p].1)) == expected {
            valid.push((start, chain));
        }
    }
    if valid.len() != 1 {
        let detail = errors.first().map_or("UID counts or record boundaries differ", |s| s.as_str());
        return Err(suffix_error(detail));
    }
    let (start, chain) = valid.pop().unwrap();
    if ends.get(&start).is_some_and(|v| !v.is_empty()) {
        return err("ambiguous marker channel suffix: extra preceding known UID channel");
    }
    if chain.len() > MAX_TRAJECTORIES {
        return err("Trajectory count exceeds UInt16 identity range");
    }
    Ok(chain
        .into_iter()
        .map(|pos| {
            let (end, uid, data_start) = candidates[&pos];
            MarkerRow { node: by_uid[&uid].clone(), data_start, data_end: end }
        })
        .collect())
}

/// `_read_marker_uid_join`: interleaved/trailing empty Skeleton pose channels.
fn read_marker_uid_join(
    channels: &[u8],
    markers: &[MarkerNode],
    lo: i64,
    hi: i64,
    skeleton_uids: &HashSet<Uid>,
) -> Result<Vec<MarkerRow>> {
    if lo > hi {
        return err("Invalid metadata frame range");
    }
    let mut by_uid: HashMap<Uid, &MarkerNode> = HashMap::new();
    for m in markers {
        by_uid.insert(m.uid, m); // dict comprehension: last wins
    }
    if markers.is_empty() || by_uid.len() != markers.len() || by_uid.contains_key(&[0u8; 16]) {
        return err("Structural mapping requires unique nonzero Marker UIDs");
    }
    if by_uid.keys().any(|u| skeleton_uids.contains(u)) {
        return err("Marker/Skeleton UID collision");
    }
    let mut matched: HashMap<Uid, ChannelRecord> = HashMap::new();
    let mut foreign: HashMap<Uid, Vec<ChannelRecord>> = HashMap::new();
    for record in iter_channel_inventory(channels)? {
        let uid = record.uid;
        if by_uid.contains_key(&uid) {
            if matched.contains_key(&uid) {
                return err("Multiple channels share a Marker UID");
            }
            if record.kind != "Vector3fChannel" || record.name != "Translation" {
                return err("Marker UID belongs to a non-Translation channel");
            }
            if record.default(channels) != [0u8; 12] {
                return err("Unknown marker channel flags");
            }
            matched.insert(uid, record);
        } else if record.kind == "Vector3fChannel" || record.kind == "QuaternionChannel" {
            if !skeleton_uids.contains(&uid) {
                return err("Foreign spatial channel lacks Skeleton UID ownership");
            }
            foreign.entry(uid).or_default().push(record);
        }
    }
    if matched.len() != by_uid.len() {
        return err("Marker/channel UID mapping is incomplete");
    }
    let rotation_default: Vec<u8> = [0f32, 0.0, 0.0, 1.0].iter().flat_map(|v| v.to_le_bytes()).collect();
    for records in foreign.values() {
        let mut observed: HashSet<(&str, &str, &[u8])> = HashSet::new();
        for r in records {
            observed.insert((r.kind.as_str(), r.name.as_str(), r.default(channels)));
        }
        let expected: HashSet<(&str, &str, &[u8])> = HashSet::from([
            ("Vector3fChannel", "Translation", &[0u8; 12][..]),
            ("QuaternionChannel", "Rotation", rotation_default.as_slice()),
        ]);
        if records.len() != 2 || observed != expected || records.iter().any(|r| r.n_samples != 0) {
            return err("Unsupported Skeleton spatial channel pair");
        }
    }
    let mut ordered: Vec<(Uid, ChannelRecord)> = matched.into_iter().collect();
    ordered.sort_by_key(|(_, r)| r.start);
    if ordered.len() > MAX_TRAJECTORIES {
        return err("Trajectory count exceeds UInt16 identity range");
    }
    let mut rows = Vec::with_capacity(ordered.len());
    for (uid, record) in ordered {
        check_frames(&channels[record.data_start..record.data_end], lo, hi)?;
        rows.push(MarkerRow { node: by_uid[&uid].clone(), data_start: record.data_start, data_end: record.data_end });
    }
    Ok(rows)
}

/// `read_marker_channels`: suffix decoding first, then the structural UID fallback.
pub fn read_marker_channels(
    channels: &[u8],
    markers: &[MarkerNode],
    lo: i64,
    hi: i64,
    skeleton_uids: &HashSet<Uid>,
) -> Result<Vec<MarkerRow>> {
    match read_marker_suffix(channels, markers, lo, hi) {
        Ok(rows) => Ok(rows),
        Err(suffix_error) => read_marker_uid_join(channels, markers, lo, hi, skeleton_uids)
            .map_err(|e| TakError(format!("{}; structural UID fallback: {}", suffix_error.0, e.0))),
    }
}

/// `<Ifff` little-endian records -> (frames, xyz) with the original float32 bits.
pub fn decode_samples(raw16: &[u8]) -> (Vec<u32>, Vec<[f32; 3]>) {
    let n = raw16.len() / 16;
    let mut frames = Vec::with_capacity(n);
    let mut xyz = Vec::with_capacity(n);
    for rec in raw16.chunks_exact(16) {
        let w = |i: usize| u32::from_le_bytes([rec[i], rec[i + 1], rec[i + 2], rec[i + 3]]);
        frames.push(w(0));
        xyz.push([f32::from_bits(w(4)), f32::from_bits(w(8)), f32::from_bits(w(12))]);
    }
    (frames, xyz)
}

/// An analog device starts at `AnalogChannel0`; each sample is a frame number and
/// floating-point values. Non-increasing frame numbers are rejected.
pub fn analog_devices(channels: &[u8], inventory: &[ChannelRecord]) -> Result<Vec<AnalogDevice>> {
    let mut devices: Vec<AnalogDevice> = Vec::new();
    for record in inventory {
        let stride = 4 + 4 * record.dim();
        let mut previous: Option<u32> = None;
        for i in 0..record.n_samples {
            let frame = u32_at(channels, record.data_start + i * stride)?;
            if previous.is_some_and(|p| p >= frame) {
                return err(format!("Non-increasing channel frames ({})", record.name));
            }
            previous = Some(frame);
        }
    }
    for record in inventory {
        let Some(suffix) = record.name.strip_prefix("AnalogChannel") else { continue };
        let index = python_int(suffix)?;
        if index == 0 {
            devices.push(AnalogDevice { channels: Vec::new() });
        }
        let Some(device) = devices.last_mut() else {
            return err("analog channel before AnalogChannel0");
        };
        let dim = record.dim();
        let stride = 4 + 4 * dim;
        let mut frames = Vec::with_capacity(record.n_samples);
        let mut values = Vec::with_capacity(record.n_samples * dim);
        for i in 0..record.n_samples {
            let at = record.data_start + i * stride;
            frames.push(u32_at(channels, at)?);
            for d in 0..dim {
                values.push(f32_at(channels, at + 4 + 4 * d)?);
            }
        }
        device.channels.push(AnalogChannel { name: record.name.clone(), kind: record.kind.clone(), dim, frames, values });
    }
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tak::MarkerKind;

    fn node(uid: Uid) -> MarkerNode {
        MarkerNode {
            uid,
            name: format!("m{}", uid[0]),
            parent: Some([0; 16]),
            slot_raw: Some(0),
            slot: Some(0),
            kind: MarkerKind::Unlabeled,
            asset_uid: None,
            descriptor_sha256: format!("{:02x}", uid[0]),
        }
    }

    fn translation(uid: Uid, samples: &[(u32, [f32; 3])]) -> Vec<u8> {
        let mut out = TRANSLATION.to_vec();
        out.extend_from_slice(&uid);
        out.extend_from_slice(&[0u8; 12]);
        out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        for (f, p) in samples {
            out.extend_from_slice(&f.to_le_bytes());
            for v in p {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    fn stream(records: &[Vec<u8>], prefix: &[u8]) -> Vec<u8> {
        let mut out = CHANNELS_HEADER.to_vec();
        out.extend_from_slice(&(records.len() as u32).to_le_bytes());
        out.extend_from_slice(prefix);
        for r in records {
            out.extend_from_slice(r);
        }
        out
    }

    #[test]
    fn decodes_samples_with_exact_bits() {
        let odd = f32::from_bits(0x7fc0_0001); // NaN payload must survive
        let a = translation([1; 16], &[(10, [1.5, -2.25, odd]), (12, [0.0, -0.0, 3.0])]);
        let b = translation([2; 16], &[]);
        let ch = stream(&[a, b], &[]);
        let markers = [node([1; 16]), node([2; 16])];
        let rows = read_marker_channels(&ch, &markers, 0, 100, &HashSet::new()).unwrap();
        assert_eq!(rows.len(), 2);
        let (frames, xyz) = decode_samples(&ch[rows[0].data_start..rows[0].data_end]);
        assert_eq!(frames, vec![10, 12]);
        assert_eq!(xyz[0][0], 1.5);
        assert_eq!(xyz[0][2].to_bits(), 0x7fc0_0001);
        assert_eq!(xyz[1][1].to_bits(), (-0.0f32).to_bits());
        assert_eq!(rows[1].data_start, rows[1].data_end);
    }

    #[test]
    fn rejects_out_of_range_frames() {
        let ch = stream(&[translation([1; 16], &[(10, [0.0; 3]), (200, [0.0; 3])])], &[]);
        let e = read_marker_channels(&ch, &[node([1; 16])], 0, 100, &HashSet::new()).unwrap_err();
        assert!(e.0.contains("Invalid marker frame order or range"), "{}", e.0);
    }

    #[test]
    fn inventory_and_analog_devices() {
        fn analog(name: &str, samples: &[(u32, f32)]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&12u16.to_le_bytes());
            out.extend_from_slice(b"FloatChannel");
            out.push(4);
            let n: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            out.extend_from_slice(&((n.len() / 2) as u16).to_le_bytes());
            out.extend_from_slice(&n);
            out.extend_from_slice(&[3u8; 16]);
            out.extend_from_slice(&[0u8; 4]);
            out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
            for (f, v) in samples {
                out.extend_from_slice(&f.to_le_bytes());
                out.extend_from_slice(&v.to_le_bytes());
            }
            out
        }
        let recs = [
            analog("AnalogChannel0", &[(1, 0.5), (2, 0.25)]),
            analog("AnalogChannel1", &[(1, 1.0)]),
            analog("AnalogChannel0", &[]),
            translation([1; 16], &[(1, [1.0, 2.0, 3.0])]),
        ];
        let ch = stream(&recs, &[]);
        let inv = iter_channel_inventory(&ch).unwrap();
        assert_eq!(inv.len(), 4);
        assert_eq!(inv[3].kind, "Vector3fChannel");
        assert_eq!(inv[3].name, "Translation");
        let devices = analog_devices(&ch, &inv).unwrap();
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].channels.len(), 2);
        assert_eq!(devices[0].channels[0].values, vec![0.5, 0.25]);
        assert_eq!(devices[0].channels[0].frames, vec![1, 2]);
        assert_eq!(devices[1].channels[0].frames.len(), 0);
    }
}
