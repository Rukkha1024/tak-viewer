use super::*;
use crate::tak::container::FREE;

const SECTOR: usize = 512;
const METADATA: usize = 3 * SECTOR;
const CHANNELS: usize = 4 * SECTOR;

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn directory_entry(bytes: &mut [u8], name: &str, kind: u8, child: u32, start: u32, size: u64) {
    let name: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    bytes[..name.len()].copy_from_slice(&name);
    bytes[64..66].copy_from_slice(&((name.len() + 2) as u16).to_le_bytes());
    bytes[66] = kind;
    put32(bytes, 68, FREE);
    put32(bytes, 72, FREE);
    put32(bytes, 76, child);
    put32(bytes, 116, start);
    bytes[120..128].copy_from_slice(&size.to_le_bytes());
}

fn compound(xml: &[u8], mini: bool) -> Vec<u8> {
    let mut bytes = vec![0; 6 * SECTOR];
    bytes[..8].copy_from_slice(&MAGIC);
    bytes[30..34].copy_from_slice(&[9, 0, 6, 0]);
    put32(&mut bytes, 44, 1);
    put32(&mut bytes, 48, 0);
    put32(&mut bytes, 56, if mini { 256 } else { 0 });
    put32(&mut bytes, 60, if mini { 4 } else { END });
    put32(&mut bytes, 64, u32::from(mini));
    put32(&mut bytes, 76, 1);
    for sector in [0, 2, 3, 4] {
        put32(&mut bytes, 2 * SECTOR + sector * 4, END);
    }
    let dir = &mut bytes[SECTOR..2 * SECTOR];
    let (root, root_len, start) = if mini { (2, 192, 1) } else { (END, 0, 2) };
    directory_entry(&mut dir[..128], "Root Entry", 5, 1, root, root_len);
    directory_entry(&mut dir[128..256], META, 2, FREE, start, xml.len() as u64);
    put32(dir, 128 + 72, 2);
    directory_entry(
        &mut dir[256..384],
        "Channels.dat",
        2,
        FREE,
        3,
        SECTOR as u64,
    );
    if mini {
        // A fragmented two-sector stream, whose first sector follows the second on disk.
        put32(&mut bytes, 5 * SECTOR + 4, 0);
        put32(&mut bytes, 5 * SECTOR, END);
        bytes[METADATA + 64..METADATA + 128].copy_from_slice(&xml[..64]);
        bytes[METADATA..METADATA + xml.len() - 64].copy_from_slice(&xml[64..]);
    } else {
        bytes[METADATA..METADATA + xml.len()].copy_from_slice(xml);
    }
    bytes[CHANNELS..CHANNELS + SECTOR].fill(0x7a);
    bytes
}

fn read_traced(
    bytes: &[u8],
    allowed: impl Fn(usize, usize) -> bool,
) -> Result<(Option<i64>, usize)> {
    read_with(bytes.len() as u64, |buf, offset| {
        let start = offset as usize;
        let end = start + buf.len();
        assert!(allowed(start, end), "unexpected read of {start}..{end}");
        buf.copy_from_slice(&bytes[start..end]);
        Ok(())
    })
}

#[test]
fn regular_and_fragmented_mini_metadata_skip_the_channel_stream() {
    let raw = b"<properties><property name=\"Progress\" value=\"1\"/></properties>";
    let regular = compound(raw, false);
    let (number, bytes) = read_traced(&regular, |_, end| end <= CHANNELS).unwrap();
    assert_eq!((number, bytes), (Some(1), 3 * SECTOR + raw.len()));

    let xml = "<properties><property name=\"Progress\" value=\"4\"/></properties>";
    let xml: Vec<_> = xml.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mini = compound(&xml, true);
    let (number, bytes) = read_traced(&mini, |start, end| {
        end <= CHANNELS || start >= CHANNELS + SECTOR
    })
    .unwrap();
    assert_eq!((number, bytes), (Some(4), 3 * SECTOR + 8 + xml.len()));
    assert_eq!(
        Container::parse(&mini)
            .unwrap()
            .read_named(&[META])
            .unwrap()[0]
            .1,
        xml
    );
}

#[test]
fn converted_takm_reopens_without_reading_channels_or_tail() {
    let raw = b"<properties><property name=\"Progress\" value=\"4\"/></properties>";
    let mut takm = Vec::new();
    crate::takm::write(
        &mut takm,
        &compound(raw, false),
        "fixture.tak",
        "2026-11-18",
    )
    .unwrap();
    let table_start = crate::takm::HEADER_LEN as usize + raw.len() + SECTOR;
    let data_start = crate::takm::HEADER_LEN as usize;
    let table_end =
        takm.len() - crate::comments::header_line().len() - crate::takm::SEAL_LEN as usize;
    assert_eq!(
        read_traced(&takm, |start, end| end <= data_start + raw.len()
            || (start >= table_start && end <= table_end))
        .unwrap()
        .0,
        Some(4)
    );
    let temp = crate::test_support::TempDir::new("progress_takm");
    let path = temp.join("roundtrip.takm");
    std::fs::write(&path, &takm).unwrap();
    let file = File::open(&path).unwrap();
    assert_eq!(read(&file, takm.len() as u64, || true).unwrap().0, Some(4));
    assert!(read(&file, takm.len() as u64, || false).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), takm);
}

#[test]
fn bad_or_missing_values_are_blank_and_structural_errors_refuse() {
    let raw = compound(
        b"<properties><property name=\"Progress\" value=\"bad\"/></properties>",
        false,
    );
    assert!(read_traced(&raw, |_, _| true).is_err());
    let raw = compound(b"<properties></properties>", false);
    assert_eq!(read_traced(&raw, |_, _| true).unwrap().0, None);
    let mut cyclic = raw.clone();
    put32(&mut cyclic, 2 * SECTOR, 0);
    assert!(read_traced(&cyclic, |_, end| end <= CHANNELS).is_err());
    assert!(read_traced(&raw[..METADATA], |_, end| end <= METADATA).is_err());
    assert!(read_traced(b"not a TAK", |_, _| true).is_err());
}
