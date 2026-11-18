//! Copy the opaque TAKM body and tail around one metadata patch.
use super::*;
use std::io::{Read, Seek, SeekFrom};

const META: &str = "MetaData.dat";

pub(crate) fn write(
    source: &mut File,
    out: &mut File,
    expected: Option<i64>,
    next: Option<i64>,
) -> Result<u64> {
    let len = source.metadata()?.len();
    let mut header = [0; HEADER_LEN as usize];
    source.read_exact(&mut header)?;
    let (table_at, table_len, fixed) = parse_header(&header, len)?;
    let mut table_bytes = vec![0; table_len as usize];
    read_exact_at(source, &mut table_bytes, table_at)?;
    let mut table: Value =
        serde_json::from_slice(&table_bytes).map_err(|e| TakError(e.to_string()))?;
    let index = decode_table(&table, table_at, fixed)?;
    let meta = index
        .unique(META)
        .filter(|s| s.size <= 256 * 1024)
        .ok_or_else(|| TakError("Missing, oversized or ambiguous metadata stream".into()))?;
    let meta_end = meta.offset + meta.size;
    let mut prev = HEADER_LEN;
    for s in &index.streams {
        if s.offset < prev {
            return err("Overlapping or unordered TAKM streams");
        }
        prev = s.offset + s.size;
    }
    let mut raw = vec![0; meta.size as usize];
    read_exact_at(source, &mut raw, meta.offset)?;
    let patched = crate::tak::metadata::patch_progress(&raw, expected, next)?;
    let shift = patched.len() as i64 - meta.size as i64;
    let rows = table["streams"].as_array_mut().expect("validated table");
    let mut changed_table = shift != 0;
    for (s, row) in index.streams.iter().zip(rows) {
        if s.name == META {
            row["size"] = json!(patched.len());
            if row.get("sha256").is_some() {
                row["sha256"] = json!(sha256_hex(&patched));
                changed_table = true;
            }
        } else if shift != 0 && s.offset >= meta_end {
            row["offset"] = json!(shifted(s.offset, shift)?);
        }
    }
    if changed_table {
        table_bytes = serde_json::to_vec(&table).map_err(|e| TakError(e.to_string()))?;
    }
    let new_table_at = shifted(table_at, shift)?;
    let new_fixed = new_table_at + table_bytes.len() as u64;
    header[16..24].copy_from_slice(&new_table_at.to_le_bytes());
    header[24..32].copy_from_slice(&(table_bytes.len() as u64).to_le_bytes());
    header[32..40].copy_from_slice(&new_fixed.to_le_bytes());

    let mut old_hash = Sha256::new();
    let mut new_hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    source.seek(SeekFrom::Start(0))?;
    source.read_exact(&mut buffer[..header.len()])?;
    old_hash.update(&buffer[..header.len()]);
    out.write_all(&header)?;
    new_hash.update(header);
    let mut copy_fixed =
        |source: &mut File, out: &mut File, length, old: &mut Sha256, new: &mut Sha256| {
            copy(source, length, &mut buffer, |bytes| {
                old.update(bytes);
                new.update(bytes);
                out.write_all(bytes)
            })
        };
    copy_fixed(
        source,
        out,
        meta.offset - HEADER_LEN,
        &mut old_hash,
        &mut new_hash,
    )?;
    let mut original = vec![0; raw.len()];
    source.read_exact(&mut original)?;
    if original != raw {
        return err("Metadata changed during save");
    }
    old_hash.update(original);
    out.write_all(&patched)?;
    new_hash.update(patched);
    copy_fixed(
        source,
        out,
        table_at - meta_end,
        &mut old_hash,
        &mut new_hash,
    )?;
    copy(source, table_len, &mut buffer, |bytes| {
        old_hash.update(bytes);
        Ok(())
    })?;
    let mut old_seal = [0; SEAL_LEN as usize];
    source.read_exact(&mut old_seal)?;
    if old_hash.finalize().as_slice() != old_seal {
        return err("TAKM seal does not match");
    }
    out.write_all(&table_bytes)?;
    new_hash.update(&table_bytes);
    out.write_all(&new_hash.finalize())?;
    copy(source, len - index.tail_start(), &mut buffer, |bytes| {
        out.write_all(bytes)
    })?;
    out.sync_all()?;
    Ok(new_fixed)
}

fn shifted(value: u64, delta: i64) -> Result<u64> {
    value
        .checked_add_signed(delta)
        .ok_or_else(|| TakError("Invalid TAKM extent".into()))
}

fn copy(
    input: &mut File,
    mut length: u64,
    buf: &mut [u8],
    mut put: impl FnMut(&[u8]) -> io::Result<()>,
) -> Result<()> {
    while length != 0 {
        let count = length.min(buf.len() as u64) as usize;
        input.read_exact(&mut buf[..count])?;
        put(&buf[..count])?;
        length -= count as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::save::save_progress;

    #[test]
    fn progress_write_preserves_every_other_byte_and_validates_seal() {
        let dir = crate::test_support::TempDir::new("progress-exact-bytes");
        let path = dir.join("sample.takm");
        let mut original = Vec::new();
        let source = dir.join("sample.tak");
        let tak = crate::sample_tak::sample_tak();
        std::fs::write(&source, &tak).unwrap();
        crate::takm::write(&mut original, &tak, "sample.tak", "test").unwrap();
        // Preserve a torn Comment fragment too.
        original.extend_from_slice(b"unfinished,tail");
        std::fs::write(&path, &original).unwrap();
        let absent = read_index_path(&path).unwrap();

        save_progress(&path, None, Some(1), None, None).unwrap();
        let one = std::fs::read(&path).unwrap();
        let after_insert = read_index_path(&path).unwrap();
        for (old, new) in absent.streams.iter().zip(&after_insert.streams) {
            assert_eq!((&old.name, old.index), (&new.name, new.index));
            if old.name != META {
                assert_eq!(
                    &original[old.offset as usize..(old.offset + old.size) as usize],
                    &one[new.offset as usize..(new.offset + new.size) as usize],
                    "{}",
                    old.name
                );
            }
        }
        assert_eq!(
            &original[absent.tail_start() as usize..],
            &one[after_insert.tail_start() as usize..]
        );
        assert_eq!(absent.source, after_insert.source);
        assert_eq!(absent.dropped, after_insert.dropped);
        verify_fixed(&path).unwrap();

        save_progress(&path, Some(1), Some(4), None, None).unwrap();
        let four = std::fs::read(&path).unwrap();
        assert_eq!(one.len(), four.len());
        let fixed = after_insert.fixed_len as usize;
        let changed: Vec<_> = one[..fixed]
            .iter()
            .zip(&four[..fixed])
            .enumerate()
            .filter_map(|(i, (a, b))| (a != b).then_some((i, *a, *b)))
            .collect();
        assert_eq!(changed.len(), 1, "{changed:?}");
        let meta = after_insert.unique(META).unwrap();
        assert!((meta.offset..meta.offset + meta.size).contains(&(changed[0].0 as u64)));
        assert_eq!((changed[0].1, changed[0].2), (b'1', b'4'));
        assert_eq!(
            &one[after_insert.tail_start() as usize..],
            &four[after_insert.tail_start() as usize..]
        );
        verify_fixed(&path).unwrap();
        assert_eq!(
            crate::tak::read_take(&path).unwrap().meta("Progress"),
            Some("4")
        );

        let verified = crate::takm::verify::verify(&path, Some(&source));
        assert_eq!(verified.verdict, crate::takm::verify::Verdict::Edited);
        assert_eq!(verified.changed_streams.len(), 1);
        assert_eq!(verified.changed_streams[0].name, META);

        assert!(save_progress(&path, Some(7), Some(0), None, None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), four);
        save_progress(&path, Some(4), None, None, None).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "undo restores absence and the original bytes"
        );
    }

    #[test]
    fn refuses_tak_and_corrupt_take_without_modifying_either() {
        let dir = crate::test_support::TempDir::new("progress-refusal");
        for name in ["sample.tak", "broken.takm"] {
            let path = dir.join(name);
            std::fs::write(&path, b"bad file").unwrap();
            assert!(save_progress(&path, None, Some(4), None, None).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"bad file");
        }
        assert_eq!(std::fs::read_dir(dir.to_path_buf()).unwrap().count(), 2);
    }
}
