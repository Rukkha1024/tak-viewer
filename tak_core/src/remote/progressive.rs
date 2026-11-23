//! Version 1 SSH motion packets. Wire bytes and expanded bytes have independent limits.
use super::{transport::Frame, Result};
use crate::tak::{
    self,
    channels::{channel_inventory, iter_channel_inventory, ChannelRecord},
};
use flate2::{Decompress, FlushDecompress, Status};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};

pub(crate) const INTERVAL: i64 = 8;
const SOURCE_MARGIN: usize = 64 << 10;

type Runs = Vec<(i64, i64)>;

pub struct PreviewMotion {
    pub(crate) requested: i64,
    pub(crate) runs: Vec<Runs>,
}

#[cfg(feature = "test-support")]
impl PreviewMotion {
    pub fn synthetic(requested: i64, runs: Vec<Runs>) -> Self {
        Self { requested, runs }
    }
}

pub(super) fn payload_limit(file_size: u64, ceiling: usize) -> Result<usize> {
    usize::try_from(file_size)
        .ok()
        .and_then(|n| n.checked_add(SOURCE_MARGIN))
        .map(|n| n.min(ceiling))
        .ok_or("Invalid source file size".into())
}

pub(super) fn size(value: &Value, limit: usize) -> Result<usize> {
    value
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&n| n <= limit)
        .ok_or("Invalid expanded SSH payload length".into())
}

pub(super) fn expanded_sizes(header: &Value, blocks: usize, limit: usize) -> Result<Vec<usize>> {
    let values = header["expanded_lengths"]
        .as_array()
        .filter(|v| v.len() == blocks)
        .ok_or("Invalid expanded SSH payload lengths")?;
    let mut available = limit;
    values
        .iter()
        .map(|v| {
            let n = size(v, available)?;
            available -= n;
            Ok(n)
        })
        .collect()
}

pub(super) fn expand(frame: Frame, count: usize, limit: usize) -> Result<Vec<Vec<u8>>> {
    expand_pco(frame, count, limit, None)
}

pub(super) fn expand_pco(frame: Frame, count: usize, limit: usize, packed_axes: Option<usize>) -> Result<Vec<Vec<u8>>> {
    let (header, blocks) = frame;
    if blocks.len() != count {
        return Err("Wrong number of compressed SSH blocks".into());
    }
    let sizes = expanded_sizes(&header, count, limit)?;
    blocks
        .into_iter()
        .zip(sizes)
        .enumerate()
        .map(|(index, (bytes, n))| if packed_axes == Some(index) { unpack_pco(&bytes, n) } else { inflate(&bytes, n) })
        .collect()
}

fn inflate(bytes: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(expected + 1)
        .map_err(|_| "Cannot allocate expanded SSH payload")?;
    let mut decoder = Decompress::new(true);
    let status = decoder
        .decompress_vec(bytes, &mut output, FlushDecompress::Finish)
        .map_err(|e| format!("Invalid compressed SSH block: {e}"))?;
    if status != Status::StreamEnd
        || decoder.total_in() != bytes.len() as u64
        || output.len() != expected
    {
        return Err("Truncated or oversized compressed SSH block".into());
    }
    Ok(output)
}

fn unpack_pco(mut packet: &[u8], expected: usize) -> Result<Vec<u8>> {
    fn read_word(input: &mut &[u8]) -> Result<usize> {
        Ok(word(take(input, 4)?) as usize)
    }

    fn take<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
        let (head, rest) = input.split_at_checked(n).ok_or("Truncated SSH PCo block")?;
        *input = rest;
        Ok(head)
    }

    let side_size = read_word(&mut packet)?;
    let side_packed = read_word(&mut packet)?;
    let count = read_word(&mut packet)?;
    if !(1..=4).contains(&count) || side_size > expected {
        return Err("Invalid SSH PCo layout length".into());
    }
    let sizes = (0..count).map(|_| Ok((read_word(&mut packet)?, read_word(&mut packet)?)))
        .collect::<Result<Vec<_>>>()?;
    let words = (expected - side_size) / 4;
    if side_size + 4 * words != expected
        || sizes.iter().try_fold(0usize, |total, (n, _)| total.checked_add(*n)) != Some(words)
    {
        return Err("Invalid SSH PCo coordinate count".into());
    }
    let side = inflate(take(&mut packet, side_packed)?, side_size)?;
    let rows = channel_inventory(&side, true).map_err(|e| e.to_string())?;
    let samples: usize = rows.iter().filter(|row| row.is_translation()).map(|row| row.n_samples).sum();
    if samples.checked_mul(3) != Some(words) {
        return Err("SSH PCo coordinates differ from channel counts".into());
    }

    let mut values = Vec::new();
    values.try_reserve_exact(words).map_err(|_| "Cannot allocate SSH PCo coordinates")?;
    values.resize(words, 0.0f32);
    let mut offset = 0;
    for (n, packed) in sizes {
        let src = take(&mut packet, packed)?;
        decode_pco(src, &mut values[offset..offset + n])?;
        offset += n;
    }
    if !packet.is_empty() {
        return Err("Trailing bytes in SSH PCo block".into());
    }

    let mut raw = Vec::new();
    raw.try_reserve_exact(expected).map_err(|_| "Cannot allocate SSH PCo channel bytes")?;
    raw.extend_from_slice(&side[..10]);
    let mut offset = 0;
    for row in rows {
        raw.extend_from_slice(&side[row.start..row.data_start]);
        let data = &side[row.data_start..row.data_end];
        if row.is_translation() {
            let n = row.n_samples;
            let mut frame = 0;
            for i in 0..n {
                frame ^= u32::from_le_bytes([data[i], data[n + i], data[2 * n + i], data[3 * n + i]]);
                raw.extend_from_slice(&frame.to_le_bytes());
                for axis in 0..3 {
                    raw.extend_from_slice(&values[offset + axis * n + i].to_bits().to_le_bytes());
                }
            }
            offset += 3 * n;
        } else {
            raw.extend_from_slice(data);
        }
    }
    Ok(raw)
}

fn decode_pco(src: &[u8], mut output: &mut [f32]) -> Result<()> {
    use pco::standalone::{DecompressorItem, FileDecompressor};
    let error = |err| format!("Invalid SSH PCo stream: {err}");
    let (decoder, mut src) = FileDecompressor::new(src).map_err(error)?;
    loop {
        match decoder.chunk_decompressor::<f32, _>(src).map_err(error)? {
            DecompressorItem::Chunk(mut chunk) => {
                let n = chunk.n();
                let dst = output.get_mut(..n).ok_or("Too many SSH PCo coordinates")?;
                let progress = chunk.read(dst).map_err(error)?;
                if !progress.finished || progress.n_processed != n {
                    return Err("Truncated SSH PCo coordinates".into());
                }
                output = &mut output[n..];
                src = chunk.into_src();
            }
            DecompressorItem::EndOfData(rest) => {
                if !rest.is_empty() || !output.is_empty() {
                    return Err("Unexpected SSH PCo stream length".into());
                }
                return Ok(());
            }
        }
    }
}

pub(super) fn expand_zstd(frame: Frame, limit: usize) -> Result<Vec<u8>> {
    let (header, blocks) = frame;
    if blocks.len() != 1 {
        return Err("Wrong number of SSH Zstandard blocks".into());
    }
    let size = expanded_sizes(&header, 1, limit)?[0];
    let input = &blocks[0];
    if zstd::zstd_safe::find_frame_compressed_size(input).ok() != Some(input.len()) {
        return Err("Invalid SSH Zstandard frame length".into());
    }
    let mut decoder = zstd::bulk::Decompressor::new().map_err(|e| e.to_string())?;
    // Level-3 helper emits a normal window (at most 2 MiB), including large takes.
    decoder.window_log_max(23).map_err(|e| e.to_string())?;
    let output = decoder.decompress(input, size).map_err(|e| format!("Invalid SSH Zstandard block: {e}"))?;
    if output.len() != size {
        return Err("Wrong expanded SSH Zstandard block length".into());
    }
    Ok(output)
}

pub(super) fn inventory(data: &[u8]) -> Result<Vec<ChannelRecord>> {
    iter_channel_inventory(data).map_err(|e| e.to_string())
}

pub(super) fn unshuffle(bytes: &mut [u8]) -> Result<Vec<ChannelRecord>> {
    restore_grouped(bytes, true)
}

pub(super) fn restore_grouped(bytes: &mut [u8], xor: bool) -> Result<Vec<ChannelRecord>> {
    let rows = inventory(bytes)?;
    let mut encoded = Vec::new();
    for row in &rows {
        let stride = (row.dim() + 1) * 4;
        let data = &mut bytes[row.data_start..row.data_end];
        encoded.clear();
        encoded.extend_from_slice(data);
        for (lane, column) in encoded.chunks(row.n_samples.max(1)).enumerate() {
            let mut previous = 0;
            for (sample, &delta) in data[lane..].iter_mut().step_by(stride).zip(column) {
                previous = if xor { previous ^ delta } else { delta };
                *sample = previous;
            }
        }
    }
    Ok(rows)
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn verify_digest(bytes: &[u8], digest: &Value) -> Result<()> {
    if digest.as_str() != Some(sha256(bytes).as_str()) {
        return Err("SSH channel digest mismatch".into());
    }
    Ok(())
}

fn is_translation(row: &ChannelRecord) -> bool {
    row.is_translation()
}

fn word(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}

fn on_grid(frame: i64, first: i64, requested: i64) -> bool {
    (frame - first) % INTERVAL == 0 || frame == requested
}

pub(super) struct PreviewChannels {
    bytes: Vec<u8>,
    rows: Vec<ChannelRecord>,
    runs: Vec<Runs>,
    requested: i64,
}

impl PreviewChannels {
    pub(super) fn decode(
        mut bytes: Vec<u8>,
        mut presence: &[u8],
        requested: i64,
        take_range: (i64, i64),
        plain_channels: bool,
    ) -> Result<Self> {
        let (first, last) = take_range;
        let rows = if plain_channels { inventory(&bytes)? } else { unshuffle(&mut bytes)? };
        let mut all_runs = Vec::with_capacity(rows.len());
        for row in &rows {
            if presence.len() < 4 {
                return Err("Truncated SSH presence count".into());
            }
            let n = word(presence) as usize;
            presence = &presence[4..];
            if n > presence.len() / 8 || !is_translation(row) && (n != 0 || row.n_samples != 0) {
                return Err("Invalid SSH presence count".into());
            }
            let mut runs = Vec::with_capacity(n);
            let mut prior = first - 2;
            let mut expected = 0usize;
            for pair in presence[..n * 8].chunks_exact(8) {
                let (a, b) = (i64::from(word(pair)), i64::from(word(&pair[4..])));
                if a <= prior + 1 || a > b || b > last {
                    return Err("Invalid SSH presence run".into());
                }
                let grid_count = (b - first) / INTERVAL - (a - 1 - first).div_euclid(INTERVAL);
                expected += grid_count as usize
                    + usize::from(
                        a <= requested && requested <= b && (requested - first) % INTERVAL != 0,
                    );
                prior = b;
                runs.push((a, b));
            }
            presence = &presence[n * 8..];
            if expected != row.n_samples {
                return Err("SSH preview count differs from presence".into());
            }
            if is_translation(row) {
                let mut prior = first - 1;
                for sample in bytes[row.data_start..row.data_end].chunks_exact(16) {
                    let f = i64::from(word(sample));
                    let i = runs.partition_point(|&(_, b)| b < f);
                    if f <= prior
                        || !on_grid(f, first, requested)
                        || runs.get(i).is_none_or(|&(a, _)| f < a)
                    {
                        return Err("SSH preview frame differs from presence".into());
                    }
                    prior = f;
                }
            }
            all_runs.push(runs);
        }
        if !presence.is_empty() {
            return Err("Unexpected SSH presence bytes".into());
        }
        Ok(Self {
            bytes,
            rows,
            runs: all_runs,
            requested,
        })
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn motion(&self, take: &tak::Take) -> Result<PreviewMotion> {
        let mut by_uid: HashMap<tak::Uid, VecDeque<&Runs>> = HashMap::new();
        for (row, runs) in self
            .rows
            .iter()
            .zip(&self.runs)
            .filter(|(row, _)| is_translation(row))
        {
            by_uid.entry(row.uid).or_default().push_back(runs);
        }
        let runs = take
            .trajectories
            .iter()
            .map(|t| {
                by_uid
                    .get_mut(&t.uid)
                    .and_then(VecDeque::pop_front)
                    .cloned()
                    .ok_or_else(|| "Missing SSH marker presence".to_string())
            })
            .collect::<Result<_>>()?;
        Ok(PreviewMotion {
            requested: self.requested,
            runs,
        })
    }

    pub(super) fn merge(self, mut tail: Vec<u8>, expected: usize, plain_channels: bool) -> Result<Vec<u8>> {
        let late = if plain_channels { inventory(&tail)? } else { unshuffle(&mut tail)? };
        if self.rows.len() != late.len() || expected < 10 {
            return Err("Wrong SSH remainder channel count".into());
        }
        let mut full = Vec::new();
        full.try_reserve_exact(expected)
            .map_err(|_| "Cannot allocate exact SSH channels")?;
        full.extend_from_slice(&self.bytes[..10]);
        for ((a, b), runs) in self.rows.iter().zip(&late).zip(&self.runs) {
            let head = &self.bytes[a.start..a.data_start - 4];
            let a_data = &self.bytes[a.data_start..a.data_end];
            let b_data = &tail[b.data_start..b.data_end];
            let count = a
                .n_samples
                .checked_add(b.n_samples)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("Invalid exact SSH sample count")?;
            let remaining = expected - full.len();
            if head != &tail[b.start..b.data_start - 4]
                || head.len() + 4 + a_data.len() + b_data.len() > remaining
            {
                return Err("SSH remainder channel header or length mismatch".into());
            }
            full.extend_from_slice(head);
            full.extend_from_slice(&count.to_le_bytes());
            if !is_translation(a) {
                full.extend_from_slice(b_data);
                continue;
            }
            let mut early = a_data.chunks_exact(16).peekable();
            let mut late = b_data.chunks_exact(16).peekable();
            let mut seen = 0u32;
            for f in runs.iter().flat_map(|&(a, b)| a..=b) {
                // Both input tracks are in source order. Advancing them against the
                // manifest validates every count, including empty and off-grid tracks.
                let sample = if early.peek().is_some_and(|p| i64::from(word(p)) == f) {
                    early.next()
                } else {
                    late.next().filter(|p| i64::from(word(p)) == f)
                }
                .ok_or("SSH remainder frame differs from presence")?;
                full.extend_from_slice(sample);
                seen += 1;
                if seen > count {
                    return Err("SSH remainder exceeds channel sample count".into());
                }
            }
            if seen != count || early.peek().is_some() || late.peek().is_some() {
                return Err("SSH remainder sample count differs from presence".into());
            }
        }
        if full.len() != expected {
            return Err("SSH exact channel length mismatch".into());
        }
        Ok(full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    #[test]
    fn pco_channel_layout_preserves_bits_and_bounds() {
        use crate::tak::channels::{CHANNELS_HEADER, TRANSLATION};
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write as _;

        let mut side = CHANNELS_HEADER.to_vec();
        side.extend_from_slice(&2u32.to_le_bytes());
        let mut row = TRANSLATION.to_vec();
        row.extend_from_slice(&[0; 28]); // UID and three default words.
        side.extend_from_slice(&row);
        side.extend_from_slice(&2u32.to_le_bytes());
        let head = side.clone();
        side.extend_from_slice(&[7, 12, 0, 0, 0, 0, 0, 0]); // Frame 7, then frame 11.
        side.extend_from_slice(&row);
        side.extend_from_slice(&0u32.to_le_bytes());

        let bits = [0, 0x80000000, 0x7f812345, 0x7fc54321, 0x00000001, 0xff800000];
        let values = bits.map(f32::from_bits);
        let packed = pco::standalone::simple_compress(&values, &pco::ChunkConfig::default()).unwrap();
        let mut compressor = ZlibEncoder::new(Vec::new(), Compression::fast());
        compressor.write_all(&side).unwrap();
        let compressed_side = compressor.finish().unwrap();

        let header = [side.len(), compressed_side.len(), 1, values.len(), packed.len()];
        let mut block = header.into_iter().flat_map(|n| (n as u32).to_le_bytes()).collect::<Vec<_>>();
        block.extend(compressed_side);
        block.extend(packed);

        let mut expected = head;
        for (i, frame) in [7u32, 11].into_iter().enumerate() {
            expected.extend_from_slice(&frame.to_le_bytes());
            for axis in 0..3 {
                expected.extend_from_slice(&bits[axis * 2 + i].to_le_bytes());
            }
        }
        expected.extend_from_slice(&row);
        expected.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(unpack_pco(&block, expected.len()).unwrap(), expected);
        assert!(unpack_pco(&block, expected.len() - 4).is_err());
        assert!(unpack_pco(&block[..block.len() - 1], expected.len()).is_err());
        block.push(0);
        assert!(unpack_pco(&block, expected.len()).is_err());
    }

    #[test]
    fn zstd_packet_rejects_extra_frames_and_mismatched_lengths() {
        let plain = b"lossless block";
        let packed = zstd::bulk::compress(plain, 3).unwrap();
        let frame = |size, data| (serde_json::json!({"expanded_lengths": [size]}), vec![data]);
        assert_eq!(expand_zstd(frame(plain.len(), packed.clone()), 100).unwrap(), plain);
        assert!(expand_zstd(frame(plain.len() + 1, packed.clone()), 100).is_err());
        assert!(expand_zstd(frame(plain.len(), packed.clone()), 8).is_err());
        assert!(expand_zstd(frame(plain.len() * 2, [packed.as_slice(), &packed].concat()), 100).is_err());
    }

    #[test]
    fn bounded_inflate_refuses_truncated_oversized_and_trailing_remainders() {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(b"complete").unwrap();
        let mut payload = enc.finish().unwrap();
        assert_eq!(inflate(&payload, 8).unwrap(), b"complete");
        assert!(inflate(&payload[..payload.len() - 1], 8).is_err());
        assert!(inflate(&payload, 7).is_err());
        payload.push(0);
        assert!(inflate(&payload, 8).is_err());
    }
}
