//! Bounded read of the saved Progress property. No marker, analog, or comment streams.

use super::container::{directory_entries, u16_at, u32_at, Container, END, MAGIC};
use super::fast_read::read_exact_at;
use super::metadata::{parse_properties, python_int};
use super::{err, Result, TakError};
use std::collections::HashSet;
use std::fs::File;

const MAX_BYTES: usize = 1024 * 1024;
const MAX_METADATA: u64 = 256 * 1024;
const MAX_READS: usize = 128;
const META: &str = "MetaData.dat";

/// Parse the same property on the list and on an already opened take.
pub fn number(value: &str) -> Option<i64> {
    python_int(value).ok()
}

/// Read a single property and return the positional I/O byte count. The caller can stop
/// between reads to give foreground work priority. Size must come from the open file.
pub fn read(file: &File, size: u64, can_read: impl Fn() -> bool) -> Result<(Option<i64>, usize)> {
    read_with(size, |buf, offset| {
        if !can_read() {
            return err("Progress read interrupted");
        }
        read_exact_at(file, buf, offset)
    })
}

fn read_with(
    size: u64,
    mut read: impl FnMut(&mut [u8], u64) -> Result<()>,
) -> Result<(Option<i64>, usize)> {
    let mut bytes = 0;
    let mut calls = 0;
    let mut bounded_read = |buf: &mut [u8], offset: u64| {
        bytes += buf.len();
        calls += 1;
        if bytes > MAX_BYTES || calls > MAX_READS {
            return err("Progress read exceeds budget");
        }
        if offset > size || buf.len() as u64 > size - offset {
            return err("Progress extent outside file");
        }
        read(buf, offset)
    };

    let mut magic = [0; 8];
    bounded_read(&mut magic, 0)?;
    let raw = if magic == MAGIC {
        let mut header = [0; 512];
        header[..8].copy_from_slice(&magic);
        bounded_read(&mut header[8..], 8)?;
        compound_metadata(&header, bounded_read)?
    } else if magic == crate::takm::MAGIC {
        let index = crate::takm::read_index_at(size, MAX_METADATA, &mut bounded_read)?;
        let Some(entry) = index.unique(META) else {
            return err("Missing or ambiguous metadata stream");
        };
        let mut meta = metadata_buffer(entry.size)?;
        bounded_read(&mut meta, entry.offset)?;
        meta
    } else if crate::takm::v2::has_magic(&magic) {
        let limits = crate::takm::v2::Limits { packed: MAX_BYTES, expanded: MAX_BYTES };
        let meta = crate::takm::v2::metadata_at(size, limits, &mut bounded_read)?;
        if meta.len() as u64 > MAX_METADATA {
            return err("Progress metadata exceeds read budget");
        }
        meta
    } else {
        return err("Unknown take format");
    };
    if raw.is_empty() {
        return Ok((None, bytes));
    }
    let properties = parse_properties(&raw)?;
    let progress = properties.iter().find(|(key, _)| key == "Progress");
    let value = progress.map(|(_, value)| number(value)
        .ok_or_else(|| TakError("Invalid Progress value".into()))).transpose()?;
    Ok((value, bytes))
}

fn metadata_buffer(size: u64) -> Result<Vec<u8>> {
    if size > MAX_METADATA {
        return err("Progress metadata exceeds read budget");
    }
    Ok(vec![0; size as usize])
}

fn sector_offset(id: u32, unit: usize) -> u64 {
    (u64::from(id) + 1) * unit as u64
}

/// Walk only the required chains. A declared length must agree with the FAT end marker.
fn chain(
    mut id: u32,
    count: Option<usize>,
    mut next: impl FnMut(u32) -> Result<u32>,
) -> Result<Vec<u32>> {
    if count == Some(0) {
        return Ok(Vec::new());
    }
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    while id != END {
        if id >= 0xffff_fffa
            || ids.len() >= MAX_READS
            || count == Some(ids.len())
            || !seen.insert(id)
        {
            return err("Invalid or oversized progress sector chain");
        }
        ids.push(id);
        id = next(id)?;
    }
    if count.is_some_and(|n| n != ids.len()) {
        return err("Short progress sector chain");
    }
    Ok(ids)
}

fn compound_metadata(
    header: &[u8],
    mut read: impl FnMut(&mut [u8], u64) -> Result<()>,
) -> Result<Vec<u8>> {
    let unit = match u16_at(header, 30)? {
        9 => 512,
        12 => 4096,
        15 => 32768,
        _ => return err("Unsupported progress sector geometry"),
    };
    let mini_shift = u16_at(header, 32)?;
    if mini_shift > u16_at(header, 30)? || u32_at(header, 72)? != 0 {
        return err("Unsupported progress sector geometry");
    }
    let mini_unit = 1usize << mini_shift;
    let fat_count = u32_at(header, 44)? as usize;
    if fat_count > 109 {
        return err("Too many progress FAT sectors");
    }

    // Keep the most recently used FAT page. Successive chains usually share this page.
    let mut fat_page = vec![0; unit];
    let mut loaded_page = None;
    let mut fat_next =
        |id: u32, read: &mut dyn FnMut(&mut [u8], u64) -> Result<()>| -> Result<u32> {
            let byte = id as usize * 4;
            let page = byte / unit;
            if page >= fat_count {
                return err("Progress sector outside FAT");
            }
            if loaded_page != Some(page) {
                let sector = u32_at(header, 76 + page * 4)?;
                read(&mut fat_page, sector_offset(sector, unit))?;
                loaded_page = Some(page);
            }
            u32_at(&fat_page, byte % unit)
        };

    let dir = chain(u32_at(header, 48)?, None, |id| fat_next(id, &mut read))?;
    let length = dir.len() * unit;
    if length > MAX_BYTES {
        return err("Progress directory exceeds read budget");
    }
    let mut bytes = vec![0; length];
    for (&id, buf) in dir.iter().zip(bytes.chunks_exact_mut(unit)) {
        read(buf, sector_offset(id, unit))?;
    }
    let entries = directory_entries(&bytes)?;
    let streams = Container::streams_from_entries(&entries)?;
    let mut candidates = streams.iter().filter(|(name, _)| name == META);
    let Some((_, index)) = candidates.next() else {
        return Ok(Vec::new());
    };
    if candidates.next().is_some() {
        return err("Duplicate progress metadata stream");
    }
    let entry = &entries[*index];
    let mut output = metadata_buffer(entry.size)?;
    if output.is_empty() {
        return Ok(output);
    }

    if entry.size >= u64::from(u32_at(header, 56)?) {
        let ids = chain(entry.start, Some(output.len().div_ceil(unit)), |id| {
            fat_next(id, &mut read)
        })?;
        for (&id, buf) in ids.iter().zip(output.chunks_mut(unit)) {
            read(buf, sector_offset(id, unit))?;
        }
        return Ok(output);
    }

    let root = &entries[0];
    let root_count = usize::try_from(root.size.div_ceil(unit as u64))
        .map_err(|_| TakError("Oversized progress mini root".into()))?;
    let roots = chain(root.start, Some(root_count), |id| fat_next(id, &mut read))?;
    let mini_fat = chain(
        u32_at(header, 60)?,
        Some(u32_at(header, 64)? as usize),
        |id| fat_next(id, &mut read),
    )?;
    let mini_ids = chain(entry.start, Some(output.len().div_ceil(mini_unit)), |id| {
        let byte = id as usize * 4;
        let page = mini_fat
            .get(byte / unit)
            .ok_or_else(|| TakError("Progress mini sector outside FAT".into()))?;
        let mut word = [0; 4];
        read(&mut word, sector_offset(*page, unit) + (byte % unit) as u64)?;
        Ok(u32::from_le_bytes(word))
    })?;
    for (&id, buf) in mini_ids.iter().zip(output.chunks_mut(mini_unit)) {
        let byte = u64::from(id) * mini_unit as u64;
        if byte + buf.len() as u64 > root.size {
            return err("Progress mini sector outside root");
        }
        let page = roots
            .get((byte / unit as u64) as usize)
            .ok_or_else(|| TakError("Short progress mini root".into()))?;
        read(buf, sector_offset(*page, unit) + byte % unit as u64)?;
    }
    Ok(output)
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;
