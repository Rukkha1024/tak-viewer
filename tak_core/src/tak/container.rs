//! Compound-file container reader. Port of `src/container.py` `TakContainer` (lines ~70-175).
//! Accepts exactly what `TakContainer` accepts (not the editor's stricter geometry check).
//! Also holds the small little-endian / text helpers shared by the other reader modules.

use super::{err, Result, TakError};

pub const MAGIC: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];
pub const END: u32 = 0xFFFF_FFFE;
pub const FREE: u32 = 0xFFFF_FFFF;
/// Python's default recursion limit is ~1000; deeper directory trees fail there too.
const MAX_TREE_DEPTH: usize = 900;

// ---------------------------------------------------------------------------------------------
// Shared byte helpers (Python `struct.unpack_from('<...')` raises on truncation; so do these).

pub(crate) fn u16_at(data: &[u8], off: usize) -> Result<u16> {
    match off.checked_add(2).and_then(|e| data.get(off..e)) {
        Some(b) => Ok(u16::from_le_bytes([b[0], b[1]])),
        None => err(format!("unpack requires 2 bytes at offset {off}")),
    }
}

pub(crate) fn u32_at(data: &[u8], off: usize) -> Result<u32> {
    match off.checked_add(4).and_then(|e| data.get(off..e)) {
        Some(b) => Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        None => err(format!("unpack requires 4 bytes at offset {off}")),
    }
}

pub(crate) fn u64_at(data: &[u8], off: usize) -> Result<u64> {
    match off.checked_add(8).and_then(|e| data.get(off..e)) {
        Some(b) => {
            let mut a = [0u8; 8];
            a.copy_from_slice(b);
            Ok(u64::from_le_bytes(a))
        }
        None => err(format!("unpack requires 8 bytes at offset {off}")),
    }
}

pub(crate) fn f32_at(data: &[u8], off: usize) -> Result<f32> {
    u32_at(data, off).map(f32::from_bits)
}

pub(crate) fn f32x3_at(data: &[u8], off: usize) -> Result<[f32; 3]> {
    Ok([f32_at(data, off)?, f32_at(data, off + 4)?, f32_at(data, off + 8)?])
}

/// Python slice semantics: clamps to the buffer instead of failing.
pub(crate) fn slice_clamped(data: &[u8], start: usize, end: usize) -> &[u8] {
    let s = start.min(data.len());
    let e = end.min(data.len()).max(s);
    &data[s..e]
}

/// `bytes.find(needle, from)`.
pub(crate) fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    find_in(hay, needle, from, hay.len())
}

/// `bytes.find(needle, start, end)`: the whole needle must lie inside `hay[start..end]`.
pub(crate) fn find_in(hay: &[u8], needle: &[u8], start: usize, end: usize) -> Option<usize> {
    let end = end.min(hay.len());
    if needle.is_empty() {
        return if start <= end { Some(start) } else { None };
    }
    if start >= end || end - start < needle.len() {
        return None;
    }
    // SIMD substring search (first match fully inside the range, like the plain loop).
    memchr::memmem::find(&hay[start..end], needle).map(|p| start + p)
}

/// All non-overlapping occurrences, left to right (`re.finditer(re.escape(needle))`).
pub(crate) fn find_all(hay: &[u8], needle: &[u8], from: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut pos = from;
    while let Some(at) = find(hay, needle, pos) {
        out.push(at);
        pos = at + needle.len().max(1);
    }
    out
}

/// Strict `bytes.decode('utf-16le')`: odd length or unpaired surrogates are errors.
pub(crate) fn utf16le_strict(bytes: &[u8]) -> Result<String> {
    if bytes.len() % 2 != 0 {
        return err("'utf-16-le' codec can't decode bytes: truncated data");
    }
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16(&units).map_err(|_| TakError("'utf-16-le' codec can't decode bytes: illegal surrogate".into()))
}

/// `bytes.decode('utf-16le', errors='ignore')`.
pub(crate) fn utf16le_ignore(bytes: &[u8]) -> String {
    let units = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]));
    char::decode_utf16(units).filter_map(|r| r.ok()).collect()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(bytes))
}

pub(crate) fn uid_from(bytes: &[u8]) -> super::Uid {
    let mut uid = [0u8; 16];
    uid.copy_from_slice(&bytes[..16]);
    uid
}

/// Sequential reader (`src/skeleton.py` `Reader`).
pub(crate) struct Reader<'a> {
    pub raw: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(raw: &'a [u8], pos: usize) -> Result<Self> {
        if pos > raw.len() {
            return err("Invalid skeleton read offset");
        }
        Ok(Reader { raw, pos })
    }

    pub fn read(&mut self, count: usize) -> Result<&'a [u8]> {
        match self.pos.checked_add(count).and_then(|e| self.raw.get(self.pos..e)) {
            Some(v) => {
                self.pos += count;
                Ok(v)
            }
            None => err("Truncated skeleton definition"),
        }
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.read(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        let b = self.read(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32> {
        let b = self.read(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn f32(&mut self) -> Result<f32> {
        self.u32().map(f32::from_bits)
    }

    pub fn f32x3(&mut self) -> Result<[f32; 3]> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }

    /// uint16 character count + UTF-16LE text (strict decode).
    pub fn text(&mut self) -> Result<String> {
        let n = self.u16()? as usize;
        let b = self.read(n * 2)?;
        utf16le_strict(b)
    }
}

// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub kind: u8,
    pub left: u32,
    pub right: u32,
    pub child: u32,
    pub start: u32,
    pub size: u64,
}

pub struct Container<'a> {
    data: &'a [u8],
    pub sector_size: usize,
    pub mini_size: usize,
    pub cutoff: u32,
    fat: Vec<u32>,
    pub entries: Vec<DirEntry>,
    mini_stream: Vec<u8>,
    mini_fat: Vec<u32>,
}

fn words(raw: &[u8]) -> Vec<u32> {
    raw.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// `TakContainer.read_chain`.
fn read_chain<'r>(
    start: u32,
    table: &[u32],
    unit: usize,
    reader: impl Fn(u32) -> Result<&'r [u8]>,
    size: Option<u64>,
) -> Result<Vec<u8>> {
    if size == Some(0) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut seen = vec![false; table.len()];
    let mut nchunks: u64 = 0;
    let mut current = start;
    while current != END {
        let idx = current as usize;
        if idx >= table.len() || seen[idx] {
            return err("Invalid/cyclic sector chain");
        }
        seen[idx] = true;
        let chunk = reader(current)?;
        let expected: i128 = match size {
            None => unit as i128,
            Some(s) => (unit as i128).min(s as i128 - nchunks as i128 * unit as i128),
        };
        if expected <= 0 || (chunk.len() as i128) < expected {
            return err("Stream size and sector chain disagree");
        }
        out.extend_from_slice(&chunk[..expected as usize]);
        nchunks += 1;
        current = table[idx];
    }
    if let Some(s) = size {
        if out.len() as u64 != s {
            return err("Stream shorter than declared size");
        }
    }
    Ok(out)
}

impl<'a> Container<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < 8 || data[..8] != MAGIC {
            return err("Not the observed compound container");
        }
        let sector_shift = u16_at(data, 30)? as u32;
        let mini_shift = u16_at(data, 32)? as u32;
        let cutoff = u32_at(data, 56)?;
        let sector_size: usize = match sector_shift {
            9 => 512,
            12 => 4096,
            15 => 32768,
            _ => return err("Unsupported sector geometry"),
        };
        // mini_size > sector_size  <=>  mini_shift > sector_shift (both powers of two).
        if mini_shift > sector_shift {
            return err("Unsupported sector geometry");
        }
        let mini_size = 1usize << mini_shift;
        if u32_at(data, 72)? != 0 {
            return err("Extended DIFAT not implemented");
        }
        let count = u32_at(data, 44)?;
        if count > 109 {
            return err("Too many FAT sectors");
        }
        let sector = |index: u32| -> Result<&'a [u8]> {
            let offset = (index as u64 + 1) * sector_size as u64;
            if offset >= data.len() as u64 {
                return err("Sector outside file");
            }
            let o = offset as usize;
            Ok(slice_clamped(data, o, o + sector_size))
        };
        let mut fat = Vec::new();
        for i in 0..count as usize {
            let s = sector(u32_at(data, 76 + i * 4)?)?;
            fat.extend(words(s));
        }
        let raw = read_chain(u32_at(data, 48)?, &fat, sector_size, sector, None)?;
        let mut entries = Vec::with_capacity(raw.len() / 128);
        for entry in raw.chunks(128) {
            if entry.len() != 128 {
                return err("Truncated directory entry");
            }
            let name_length = u16_at(entry, 64)? as usize;
            let kind = entry[66];
            if kind != 0 && !(2..=64).contains(&name_length) {
                return err("Invalid directory name length");
            }
            let name = utf16le_strict(slice_clamped(entry, 0, name_length.saturating_sub(2)))?;
            entries.push(DirEntry {
                name,
                kind,
                left: u32_at(entry, 68)?,
                right: u32_at(entry, 72)?,
                child: u32_at(entry, 76)?,
                start: u32_at(entry, 116)?,
                size: u64_at(entry, 120)?,
            });
        }
        let root = match entries.first() {
            Some(r) => r.clone(),
            None => return err("Missing root directory entry"),
        };
        let mini_stream = read_chain(root.start, &fat, sector_size, sector, Some(root.size))?;
        let mini_fat_size = u32_at(data, 64)? as u64 * sector_size as u64;
        let mini_fat_raw = read_chain(u32_at(data, 60)?, &fat, sector_size, sector, Some(mini_fat_size))?;
        let mini_fat = words(&mini_fat_raw);
        Ok(Container { data, sector_size, mini_size, cutoff, fat, entries, mini_stream, mini_fat })
    }

    fn sector(&self, index: u32) -> Result<&'a [u8]> {
        let offset = (index as u64 + 1) * self.sector_size as u64;
        if offset >= self.data.len() as u64 {
            return err("Sector outside file");
        }
        let o = offset as usize;
        Ok(slice_clamped(self.data, o, o + self.sector_size))
    }

    /// `TakContainer.streams()`: (path, entry index) for every stream, in-order walk.
    /// The whole tree is validated, as the Python caller consumes the full generator.
    pub fn streams(&self) -> Result<Vec<(String, usize)>> {
        let mut visited = vec![false; self.entries.len()];
        let mut out = Vec::new();
        let root_child = self.entries[0].child;
        self.walk(root_child, &[], &mut visited, &mut out, 0)?;
        Ok(out)
    }

    fn walk(
        &self,
        index: u32,
        parents: &[String],
        visited: &mut [bool],
        out: &mut Vec<(String, usize)>,
        depth: usize,
    ) -> Result<()> {
        if index == FREE {
            return Ok(());
        }
        let i = index as usize;
        if i >= self.entries.len() || visited[i] || depth > MAX_TREE_DEPTH {
            return err("Invalid directory tree");
        }
        visited[i] = true;
        let entry = &self.entries[i];
        self.walk(entry.left, parents, visited, out, depth + 1)?;
        let mut path = parents.to_vec();
        path.push(entry.name.clone());
        match entry.kind {
            2 => out.push((path.join("/"), i)),
            1 => self.walk(entry.child, &path, visited, out, depth + 1)?,
            _ => return err("Unexpected entry type"),
        }
        self.walk(entry.right, parents, visited, out, depth + 1)
    }

    /// `TakContainer.read_stream`.
    pub fn read_stream(&self, index: usize) -> Result<Vec<u8>> {
        let entry = &self.entries[index];
        if entry.size < self.cutoff as u64 {
            let ms = self.mini_size;
            let mini = &self.mini_stream;
            let mini_sector = |i: u32| -> Result<&[u8]> {
                let a = (i as usize).saturating_mul(ms);
                Ok(slice_clamped(mini, a, a.saturating_add(ms)))
            };
            return read_chain(entry.start, &self.mini_fat, ms, mini_sector, Some(entry.size));
        }
        read_chain(entry.start, &self.fat, self.sector_size, |i| self.sector(i), Some(entry.size))
    }

    /// Sector indices of a regular (not mini) stream, in order, with the same checks as
    /// `read_stream` (cyclic chain, sectors inside the file, length matches the declared size).
    /// Used by the fast reader (`fast_read.rs`) to read only parts of a big stream.
    pub fn stream_chain(&self, index: usize) -> Result<Vec<u32>> {
        let entry = &self.entries[index];
        if entry.size < self.cutoff as u64 {
            return err("Stream is in the mini stream");
        }
        let unit = self.sector_size as u64;
        let mut out = Vec::new();
        let mut seen = vec![false; self.fat.len()];
        let mut current = entry.start;
        while current != END {
            let idx = current as usize;
            if idx >= self.fat.len() || seen[idx] {
                return err("Invalid/cyclic sector chain");
            }
            seen[idx] = true;
            let offset = (current as u64 + 1) * unit;
            if offset >= self.data.len() as u64 {
                return err("Sector outside file");
            }
            let done = out.len() as u64 * unit;
            if done >= entry.size || (self.data.len() as u64 - offset).min(unit) < unit.min(entry.size - done) {
                return err("Stream size and sector chain disagree");
            }
            out.push(current);
            current = self.fat[idx];
        }
        if (out.len() as u64 * unit) < entry.size {
            return err("Stream shorter than declared size");
        }
        Ok(out)
    }

    /// Read the named top-level streams (`'/'.join(parts)`); later duplicates win like a dict.
    /// Python reads every matching stream inside the dict comprehension, so a broken
    /// earlier duplicate still fails the read.
    pub fn read_named(&self, wanted: &[&str]) -> Result<Vec<(String, Vec<u8>)>> {
        let mut out: Vec<(String, Vec<u8>)> = Vec::new();
        for (path, idx) in self.streams()? {
            if wanted.contains(&path.as_str()) {
                let raw = self.read_stream(idx)?;
                match out.iter_mut().find(|(p, _)| *p == path) {
                    Some(slot) => slot.1 = raw,
                    None => out.push((path, raw)),
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put16(b: &mut [u8], off: usize, v: u16) {
        b[off..off + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn put32(b: &mut [u8], off: usize, v: u32) {
        b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put64(b: &mut [u8], off: usize, v: u64) {
        b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn dir_entry(b: &mut [u8], off: usize, name: &str, kind: u8, child: u32, start: u32, size: u64) {
        let units: Vec<u16> = name.encode_utf16().collect();
        for (i, u) in units.iter().enumerate() {
            put16(b, off + i * 2, *u);
        }
        put16(b, off + 64, ((units.len() + 1) * 2) as u16);
        b[off + 66] = kind;
        put32(b, off + 68, FREE);
        put32(b, off + 72, FREE);
        put32(b, off + 76, child);
        put32(b, off + 116, start);
        put64(b, off + 120, size);
    }

    /// 512-byte sectors: 0 = FAT, 1 = directory, 2-3 = big stream, 4 = mini stream, 5 = MiniFAT.
    fn synthetic(fat_tail: u32) -> Vec<u8> {
        let mut b = vec![0u8; 512 * 7];
        b[..8].copy_from_slice(&MAGIC);
        put16(&mut b, 30, 9);
        put16(&mut b, 32, 6);
        put32(&mut b, 44, 1);
        put32(&mut b, 48, 1);
        put32(&mut b, 56, 256); // cutoff
        put32(&mut b, 60, 5); // MiniFAT start
        put32(&mut b, 64, 1); // MiniFAT sectors
        put32(&mut b, 72, 0);
        put32(&mut b, 76, 0);
        let fat = 512;
        for i in 0..128 {
            put32(&mut b, fat + i * 4, FREE);
        }
        put32(&mut b, fat, 0xFFFF_FFFD);
        put32(&mut b, fat + 4, END);
        put32(&mut b, fat + 8, 3);
        put32(&mut b, fat + 12, fat_tail);
        put32(&mut b, fat + 16, END);
        put32(&mut b, fat + 20, END);
        let dir = 1024;
        dir_entry(&mut b, dir, "Root Entry", 5, 1, 4, 128);
        dir_entry(&mut b, dir + 128, "Channels.dat", 2, FREE, 2, 700);
        put32(&mut b, dir + 128 + 72, 2); // right sibling
        dir_entry(&mut b, dir + 256, "MetaData.dat", 2, FREE, 0, 100);
        for i in 0..700 {
            b[1536 + i] = (i % 251) as u8;
        }
        for i in 0..100 {
            b[2560 + i] = (200 - i) as u8;
        }
        let minifat = 3072;
        for i in 0..128 {
            put32(&mut b, minifat + i * 4, FREE);
        }
        put32(&mut b, minifat, 1);
        put32(&mut b, minifat + 4, END);
        b
    }

    #[test]
    fn reads_fat_and_mini_chains() {
        let b = synthetic(END);
        let c = Container::parse(&b).unwrap();
        let streams = c.streams().unwrap();
        let names: Vec<&str> = streams.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(names, ["Channels.dat", "MetaData.dat"]);
        let big = c.read_stream(streams[0].1).unwrap();
        assert_eq!(big.len(), 700);
        assert!(big.iter().enumerate().all(|(i, &v)| v == (i % 251) as u8));
        let small = c.read_stream(streams[1].1).unwrap();
        assert_eq!(small.len(), 100);
        assert!(small.iter().enumerate().all(|(i, &v)| v == (200 - i) as u8));
    }

    #[test]
    fn rejects_cyclic_chain() {
        let b = synthetic(2); // sector 3 -> 2 -> 3 ...
        let c = Container::parse(&b).unwrap();
        let streams = c.streams().unwrap();
        let e = c.read_stream(streams[0].1).unwrap_err();
        assert!(e.0.contains("cyclic") || e.0.contains("disagree"), "{}", e.0);
    }

    #[test]
    fn rejects_bad_magic_and_geometry() {
        let mut b = synthetic(END);
        b[0] = 0;
        assert!(Container::parse(&b).is_err());
        let mut b = synthetic(END);
        put16(&mut b, 30, 10);
        assert!(Container::parse(&b).is_err());
    }

    #[test]
    fn find_helpers() {
        let h = b"abcabcab";
        assert_eq!(find(h, b"cab", 0), Some(2));
        assert_eq!(find_in(h, b"cab", 3, 7), None);
        assert_eq!(find_in(h, b"cab", 3, 8), Some(5));
        assert_eq!(find_all(h, b"ab", 0), vec![0, 3, 6]);
    }
}
