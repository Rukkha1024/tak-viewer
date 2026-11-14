//! A small TAK built in memory (compound file, 512-byte sectors, 64-byte mini sectors) for
//! tests of this crate and of the viewer (`tak_viewer` tests turn it into a `.takm`). Not used
//! by the programs.

use crate::tak::channels::{CHANNELS_HEADER, TRANSLATION};
use crate::tak::container::{END, FREE, MAGIC as CFB_MAGIC};
use crate::tak::nodes::{MARKER_MIDDLE, MARKER_PREFIX};

fn chain(data: &[u8], unit: usize, sectors: &mut Vec<Vec<u8>>, fat: &mut Vec<u32>) -> u32 {
    if data.is_empty() {
        return END;
    }
    let first = sectors.len() as u32;
    let n = data.len().div_ceil(unit);
    for k in 0..n {
        let mut s = data[k * unit..((k + 1) * unit).min(data.len())].to_vec();
        s.resize(unit, 0);
        sectors.push(s);
        fat.push(if k + 1 < n { first + k as u32 + 1 } else { END });
    }
    first
}

fn words(w: &[u32], unit: usize) -> Vec<u8> {
    let mut b: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
    b.resize(b.len().div_ceil(unit).max(1) * unit, 0xff);
    b
}

/// Compound file holding `streams` (`Folder/Name` paths), siblings in the given order.
fn cfb(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
    const SS: usize = 512;
    struct E {
        name: String,
        kind: u8,
        kids: Vec<usize>,
        data: Option<usize>,
        start: u32,
        size: u64,
    }
    let mut ents = vec![E { name: "Root Entry".into(), kind: 5, kids: vec![], data: None, start: END, size: 0 }];
    for (k, (path, _)) in streams.iter().enumerate() {
        let parts: Vec<&str> = path.split('/').collect();
        let mut parent = 0;
        for (j, part) in parts.iter().enumerate() {
            let last = j + 1 == parts.len();
            let found = ents[parent].kids.iter().copied().find(|&i| ents[i].name == *part);
            let i = found.unwrap_or_else(|| {
                ents.push(E { name: part.to_string(), kind: if last { 2 } else { 1 }, kids: vec![], data: None, start: END, size: 0 });
                let i = ents.len() - 1;
                ents[parent].kids.push(i);
                i
            });
            if last {
                ents[i].data = Some(k);
            }
            parent = i;
        }
    }
    let (mut sectors, mut fat) = (Vec::new(), Vec::new());
    let (mut mini, mut minifat) = (Vec::<u8>::new(), Vec::<u32>::new());
    for e in ents.iter_mut() {
        let Some(k) = e.data else { continue };
        let d = &streams[k].1;
        e.size = d.len() as u64;
        if d.len() >= 4096 {
            e.start = chain(d, SS, &mut sectors, &mut fat);
        } else if !d.is_empty() {
            let mut ms = Vec::new();
            e.start = chain(d, 64, &mut ms, &mut minifat);
            let base = (mini.len() / 64) as u32;
            for w in minifat.iter_mut().skip(base as usize) {
                if *w != END {
                    *w += base;
                }
            }
            e.start += base;
            mini.extend(ms.concat());
        }
    }
    ents[0].start = chain(&mini, SS, &mut sectors, &mut fat);
    ents[0].size = mini.len() as u64;
    let minifat_bytes = words(&minifat, SS);
    let n_minifat = if minifat.is_empty() { 0 } else { minifat_bytes.len() / SS };
    let minifat_start = if minifat.is_empty() { END } else { chain(&minifat_bytes, SS, &mut sectors, &mut fat) };
    let mut dir = Vec::new();
    for (i, e) in ents.iter().enumerate() {
        let mut b = [0u8; 128];
        let units: Vec<u16> = e.name.encode_utf16().collect();
        for (j, u) in units.iter().enumerate() {
            b[j * 2..j * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        b[64..66].copy_from_slice(&(((units.len() + 1) * 2) as u16).to_le_bytes());
        b[66] = e.kind;
        b[67] = 1;
        let parent_kids = ents.iter().find(|p| p.kids.contains(&i)).map(|p| p.kids.clone()).unwrap_or_default();
        let pos = parent_kids.iter().position(|&x| x == i);
        let right = pos.and_then(|p| parent_kids.get(p + 1)).map_or(FREE, |&x| x as u32);
        b[68..72].copy_from_slice(&FREE.to_le_bytes());
        b[72..76].copy_from_slice(&right.to_le_bytes());
        b[76..80].copy_from_slice(&e.kids.first().map_or(FREE, |&x| x as u32).to_le_bytes());
        b[116..120].copy_from_slice(&e.start.to_le_bytes());
        b[120..128].copy_from_slice(&e.size.to_le_bytes());
        dir.extend(b);
    }
    while dir.len() % SS != 0 {
        let mut b = [0u8; 128];
        b[68..80].copy_from_slice(&[0xff; 12]);
        dir.extend(b);
    }
    let dir_start = chain(&dir, SS, &mut sectors, &mut fat);
    let mut nfat = 1;
    while sectors.len() + nfat > nfat * 128 {
        nfat += 1;
    }
    let fat_first = sectors.len() as u32;
    fat.extend(std::iter::repeat_n(0xFFFF_FFFD, nfat));
    let fat_bytes = words(&fat, SS);
    assert_eq!(fat_bytes.len(), nfat * SS);
    let mut h = vec![0u8; SS];
    h[..8].copy_from_slice(&CFB_MAGIC);
    h[24..26].copy_from_slice(&0x3eu16.to_le_bytes());
    h[26..28].copy_from_slice(&3u16.to_le_bytes());
    h[28..30].copy_from_slice(&0xfffeu16.to_le_bytes());
    h[30..32].copy_from_slice(&9u16.to_le_bytes());
    h[32..34].copy_from_slice(&6u16.to_le_bytes());
    h[44..48].copy_from_slice(&(nfat as u32).to_le_bytes());
    h[48..52].copy_from_slice(&dir_start.to_le_bytes());
    h[56..60].copy_from_slice(&4096u32.to_le_bytes());
    h[60..64].copy_from_slice(&minifat_start.to_le_bytes());
    h[64..68].copy_from_slice(&(n_minifat as u32).to_le_bytes());
    h[68..72].copy_from_slice(&END.to_le_bytes());
    for k in 0..109 {
        let v = if k < nfat { fat_first + k as u32 } else { FREE };
        h[76 + k * 4..80 + k * 4].copy_from_slice(&v.to_le_bytes());
    }
    let mut out = h;
    for s in sectors {
        out.extend(s);
    }
    out.extend(fat_bytes);
    out
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

fn marker_node(name: &str, uid: u8) -> Vec<u8> {
    let mut r = b"\x06\x00Marker".to_vec();
    let text = format!(r#"{{"properties":[{{"name":"NodeName","value":"{name}"}}]}}"#);
    r.extend((text.encode_utf16().count() as u16).to_le_bytes());
    r.extend(utf16(&text));
    r.extend(MARKER_PREFIX);
    r.extend([uid; 16]);
    r.extend(MARKER_MIDDLE);
    r.extend([0u8; 16]);
    r.extend(0u32.to_le_bytes());
    r
}

fn translation(uid: u8, frames: impl Iterator<Item = u32>) -> Vec<u8> {
    let samples: Vec<u32> = frames.collect();
    let mut r = TRANSLATION.to_vec();
    r.extend([uid; 16]);
    r.extend([0u8; 12]);
    r.extend((samples.len() as u32).to_le_bytes());
    for f in samples {
        r.extend(f.to_le_bytes());
        for k in 0..3 {
            r.extend((f as f32 * 0.001 + uid as f32 * 0.1 + k as f32).to_le_bytes());
        }
    }
    r
}

fn analog(n: usize, frames: u32) -> Vec<u8> {
    let mut r = 12u16.to_le_bytes().to_vec();
    r.extend(b"FloatChannel");
    r.push(4);
    let name = format!("AnalogChannel{n}");
    r.extend((name.len() as u16).to_le_bytes());
    r.extend(utf16(&name));
    r.extend([9u8; 16]);
    r.extend([0u8; 4]);
    r.extend(frames.to_le_bytes());
    for f in 0..frames {
        r.extend(f.to_le_bytes());
        r.extend((f as f32 * 0.5 + n as f32).to_le_bytes());
    }
    r
}

fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

/// A TAK with 3 markers (one with a gap), 2 analog channels, a camera record and the other
/// streams of a real TAK, in a real TAK's order.
pub fn sample_tak() -> Vec<u8> {
    let mut nodes = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
    for (name, uid) in [("LKNE", 1u8), ("RKNE", 2), ("Unl", 3)] {
        nodes.extend(marker_node(name, uid));
    }
    let mut meta = vec![0u8; 6];
    meta.extend(utf16(
        "<properties><property><name>StartFrame</name><value>0</value></property>\
         <property><name>EndFrame</name><value>299</value></property>\
         <property><name>FrameRate</name><value>100</value></property></properties>",
    ));
    let recs = [
        analog(0, 300),
        analog(1, 300),
        translation(1, 0..300),
        translation(2, (0..300).filter(|f| !(100..140).contains(f))),
        translation(3, 50..60),
    ];
    let mut channels = CHANNELS_HEADER.to_vec();
    channels.extend((recs.len() as u32).to_le_bytes());
    for r in &recs {
        channels.extend(r);
    }
    cfb(&[
        ("Nodes.dat", nodes),
        ("DeviceData/Track 0.trk", noise(20_000, 7)),
        ("DeviceData/Track 1.trk", noise(132, 8)),
        ("Channels.dat", channels),
        ("MetaData.dat", meta),
        ("Calibration.dat", noise(1000, 9)),
    ])
}
