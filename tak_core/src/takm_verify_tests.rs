use super::*;
use crate::sample_tak::{cfb, sample_tak};
use crate::takm;
use crate::test_support::TempDir;

fn converted(tag: &str) -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new(tag);
    let source = dir.join("walk.tak");
    std::fs::write(&source, sample_tak()).unwrap();
    let result = convert(&source, None).unwrap();
    (dir, source, result.out)
}

fn write_legacy_fixture(source: &Path) -> PathBuf {
    let path = source.with_extension("takm");
    write(
        &mut File::create(&path).unwrap(),
        &std::fs::read(source).unwrap(),
        source.file_name().unwrap().to_str().unwrap(),
        "test",
    )
    .unwrap();
    path
}

fn legacy_converted(tag: &str) -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new(tag);
    let source = dir.join("walk.tak");
    std::fs::write(&source, sample_tak()).unwrap();
    let path = write_legacy_fixture(&source);
    (dir, source, path)
}

fn reseal(bytes: &mut [u8]) {
    let fixed = u64_le(bytes, 32) as usize;
    let digest = Sha256::digest(&bytes[..fixed]);
    bytes[fixed..fixed + 32].copy_from_slice(&digest);
}

fn edit_table(path: &Path, change: impl FnOnce(&mut Value)) {
    let raw = std::fs::read(path).unwrap();
    let table_off = u64_le(&raw, 16) as usize;
    let previous_fixed = u64_le(&raw, 32) as usize;
    let mut table: Value = serde_json::from_slice(&raw[table_off..previous_fixed]).unwrap();
    change(&mut table);
    write_with_table(path, raw[..table_off].to_vec(), &table, &raw[previous_fixed + SEAL_LEN as usize..]);
}

fn write_with_table(path: &Path, mut raw: Vec<u8>, table: &Value, tail: &[u8]) {
    let table_off = raw.len();
    let table = serde_json::to_vec(&table).unwrap();
    let fixed = table_off + table.len();
    raw[16..24].copy_from_slice(&(table_off as u64).to_le_bytes());
    raw[24..32].copy_from_slice(&(table.len() as u64).to_le_bytes());
    raw[32..40].copy_from_slice(&(fixed as u64).to_le_bytes());
    raw.extend_from_slice(&table);
    raw.resize(fixed + SEAL_LEN as usize, 0);
    reseal(&mut raw);
    raw.extend_from_slice(tail);
    std::fs::write(path, raw).unwrap();
}

fn wrap_legacy(legacy: &[u8], path: &Path) -> Result<()> {
    write_v2_from_v1(&mut File::create(path).unwrap(), legacy).map(|_| ())
}

#[test]
fn explicit_markers_only_derivative_keeps_the_existing_bytes_and_fails_full_source_check() {
    let (dir, source, original) = legacy_converted("verify_markers_only");
    let index = read_index_path(&original).unwrap();
    let raw = std::fs::read(original).unwrap();
    let table_at = u64_le(&raw, 16) as usize;
    let mut table: Value = serde_json::from_slice(&raw[table_at..index.fixed_len as usize]).unwrap();
    let mut shortened = raw[..HEADER_LEN as usize].to_vec();
    let mut kept = Vec::new();
    for (stream, row) in index.streams.iter().zip(table["streams"].as_array().unwrap().clone()) {
        let body = &raw[stream.offset as usize..(stream.offset + stream.size) as usize];
        if matches!(stream.name.as_str(), "Nodes.dat" | "Channels.dat" | "MetaData.dat") {
            let mut row = row;
            row["offset"] = json!(shortened.len());
            kept.push(row);
            shortened.extend_from_slice(body);
        } else {
            table["dropped"].as_array_mut().unwrap().push(json!({
                "name":stream.name, "index":stream.index, "size":stream.size,
                "sha256":format!("{:x}", Sha256::digest(body)),
            }));
        }
    }
    table["streams"] = json!(kept);
    let v1 = dir.join("derivative-v1.takm");
    let v2 = dir.join("derivative-v2.takm");
    let tail = &raw[index.tail_start() as usize..];
    write_with_table(&v1, shortened.clone(), &table, tail);
    assert_eq!(verify(&v1, None).verdict, Verdict::Failed);
    assert!(wrap_legacy(&std::fs::read(&v1).unwrap(), &v2).is_err());

    table["content_profile"] = json!("markers-only");
    write_with_table(&v1, shortened, &table, tail);
    let legacy = std::fs::read(&v1).unwrap();
    wrap_legacy(&legacy, &v2).unwrap();
    let file = File::open(&v2).unwrap();
    let (restored, journal, _) = restore_for_write(&file, file.metadata().unwrap().len()).unwrap();
    assert_eq!([restored, journal].concat(), legacy);
    for path in [v1, v2] {
        let archive_only = verify(&path, None);
        assert_eq!(archive_only.verdict, Verdict::NoSource);
        assert_eq!(archive_only.checks.dropped_streams.pass, None);
        let full = verify(&path, Some(&source));
        assert_eq!(full.verdict, Verdict::Failed);
        assert_eq!(full.checks.retained_streams.pass, Some(true));
        assert_eq!(full.checks.dropped_streams.pass, Some(false));
        assert!(full.checks.dropped_streams.detail.contains("마커 전용"));
    }
}

#[test]
fn legacy_lettered_and_sparse_ordinals_survive_wrapping_without_source_equivalence() {
    let (dir, source, path) = legacy_converted("verify_legacy_ordinals");
    let v1 = dir.join("legacy.takm");
    std::fs::rename(path, &v1).unwrap();
    edit_table(&v1, |table| {
        table["streams"][0]["index"] = json!("A");
        table["streams"][1]["index"] = json!(101);
        table["dropped"][0]["index"] = json!("C");
    });
    let legacy = std::fs::read(&v1).unwrap();
    let v2 = dir.join("wrapped.takm");
    wrap_legacy(&legacy, &v2).unwrap();
    let file = File::open(&v2).unwrap();
    let (body, journal, _) = restore_for_write(&file, file.metadata().unwrap().len()).unwrap();
    assert_eq!([body, journal].concat(), legacy);
    for path in [&v1, &v2] {
        assert_eq!(verify(path, None).verdict, Verdict::NoSource);
        let against_source = verify(path, Some(&source));
        assert_eq!(against_source.verdict, Verdict::Failed);
        assert_eq!(against_source.checks.source_sha256.pass, Some(true));
        assert_eq!(against_source.checks.retained_streams.pass, Some(false));
    }
    edit_table(&v1, |table| table["dropped"][0]["index"] = json!("A"));
    assert_eq!(verify(&v1, None).verdict, Verdict::Failed);
    assert!(wrap_legacy(&std::fs::read(v1).unwrap(), &v2).is_err());
}

#[test]
fn verifies_identical_conversion_and_reports_source_selection() {
    let (_dir, source, path) = converted("verify_identical");
    let auto = verify(&path, None);
    assert_eq!(auto.verdict, Verdict::Identical);
    assert_eq!(auto.source_selection, "sibling");
    assert_eq!(auto.source.as_deref(), Some(source.as_path()));
    assert_eq!(auto.source_unchanged, Some(true));
    assert_eq!(auto.source_before, auto.source_after);
    assert!(auto.required_checks().iter().all(|c| c.pass == Some(true)));
    assert_eq!(auto.comments.live_count, Some(0));

    let explicit = verify(&path, Some(&source));
    assert_eq!(explicit.verdict, Verdict::Identical);
    assert_eq!(explicit.source_selection, "explicit");

    std::fs::remove_file(&source).unwrap();
    let absent = verify(&path, None);
    assert_eq!(absent.verdict, Verdict::NoSource);
    assert_eq!(absent.source_selection, "none");
    assert_eq!(absent.source_unchanged, None);
    assert_eq!(absent.checks.retained_streams.pass, None);
    assert_eq!(verify(&path, Some(&source)).verdict, Verdict::Failed);
}

#[test]
fn swapping_equal_length_duplicate_names_fails_at_each_original_index() {
    let dir = TempDir::new("verify_duplicates");
    let source = dir.join("duplicate.tak");
    let mut raw = cfb(&[
        ("Nodes.dat", vec![1; 71]),
        ("NodeX.dat", vec![2; 71]),
        (CAMERA_STREAM, vec![3; 5000]),
    ]);
    let encoded = |name: &str| {
        name.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    };
    let from = encoded("NodeX.dat");
    let at = raw.windows(from.len()).position(|b| b == from).unwrap();
    raw[at..at + from.len()].copy_from_slice(&encoded("Nodes.dat"));
    std::fs::write(&source, raw).unwrap();
    let path = write_legacy_fixture(&source);
    assert_eq!(verify(&path, None).verdict, Verdict::Identical);
    let index = read_index_path(&path).unwrap();
    assert_eq!(index.streams.len(), 2);
    let mut raw = std::fs::read(&path).unwrap();
    let a = index.streams[0].offset as usize;
    let b = index.streams[1].offset as usize;
    for i in 0..71 {
        raw.swap(a + i, b + i);
    }
    reseal(&mut raw);
    std::fs::write(&path, raw).unwrap();

    let result = verify(&path, None);
    assert_eq!(result.verdict, Verdict::Failed);
    assert_eq!(result.checks.fixed_seal.pass, Some(true));
    assert_eq!(result.checks.retained_streams.pass, Some(false));
    assert!(result.checks.retained_streams.detail.contains("Nodes.dat"));
    assert_eq!(
        result
            .changed_streams
            .iter()
            .map(|s| s.index)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn flipped_retained_byte_fails_and_names_stream_even_when_seal_fails() {
    let (_dir, _source, path) = legacy_converted("verify_flipped");
    let index = read_index_path(&path).unwrap();
    let stream = index.unique("Calibration.dat").unwrap();
    let mut raw = std::fs::read(&path).unwrap();
    raw[stream.offset as usize] ^= 1;
    std::fs::write(&path, raw).unwrap();

    let result = verify(&path, None);
    assert_eq!(result.verdict, Verdict::Failed);
    assert_eq!(result.checks.fixed_seal.pass, Some(false));
    assert_eq!(result.checks.retained_streams.pass, Some(false));
    assert!(result
        .checks
        .retained_streams
        .detail
        .contains("Calibration.dat"));
    assert_eq!(result.changed_streams[0].index, stream.index);
}

#[test]
fn wrong_source_fails_even_if_its_container_streams_match() {
    let (dir, source, path) = converted("verify_wrong_source");
    let other = dir.join("mismatch").join("walk.tak");
    std::fs::create_dir(other.parent().unwrap()).unwrap();
    let mut raw = std::fs::read(&source).unwrap();
    raw[15] ^= 1; // Unused CFB header byte. All payloads still match.
    std::fs::write(&other, raw).unwrap();
    let result = verify(&path, Some(&other));
    assert_eq!(result.verdict, Verdict::Failed);
    assert_eq!(result.checks.source_sha256.pass, Some(false));
    assert_eq!(result.checks.source_name.pass, Some(true));
    assert_eq!(result.checks.source_size.pass, Some(true));
    assert_eq!(result.checks.retained_streams.pass, Some(true));
    assert_eq!(result.source_unchanged, Some(true));

    let renamed = dir.join("renamed.tak");
    std::fs::copy(&source, &renamed).unwrap();
    let result = verify(&path, Some(&renamed));
    assert_eq!(result.verdict, Verdict::Failed);
    assert_eq!(result.checks.source_name.pass, Some(false));
    assert_eq!(result.checks.source_sha256.pass, Some(true));
}

#[test]
fn checks_structure_layout_camera_hash_and_seal_independently() {
    let (_dir, _source, path) = legacy_converted("verify_fixed");
    let original = std::fs::read(&path).unwrap();
    let fixed = u64_le(&original, 32) as usize;
    let mut damaged = original.clone();
    damaged[fixed] ^= 1;
    std::fs::write(&path, damaged).unwrap();
    let result = verify(&path, None);
    assert_eq!(result.verdict, Verdict::Failed);
    assert_eq!(result.checks.header_table.pass, Some(true));
    assert_eq!(result.checks.retained_streams.pass, Some(true));
    assert_eq!(result.checks.fixed_seal.pass, Some(false));

    std::fs::write(&path, &original).unwrap();
    edit_table(&path, |table| table["layout"] = json!(2));
    let result = verify(&path, None);
    assert_eq!(result.verdict, Verdict::Failed);
    assert_eq!(result.checks.layout_version.pass, Some(false));
    assert_eq!(result.checks.fixed_seal.pass, Some(true));

    std::fs::write(&path, &original).unwrap();
    edit_table(&path, |table| table["streams"][1]["index"] = json!(0));
    let result = verify(&path, None);
    assert_eq!(result.verdict, Verdict::Failed);
    assert_eq!(result.checks.header_table.pass, Some(false));
    assert_eq!(result.checks.layout_version.pass, Some(true));
    assert_eq!(result.checks.fixed_seal.pass, Some(true));

    std::fs::write(&path, &original).unwrap();
    edit_table(&path, |table| {
        table["dropped"][0]["sha256"] = json!("0".repeat(64))
    });
    let result = verify(&path, None);
    assert_eq!(result.verdict, Verdict::Failed);
    assert_eq!(result.checks.dropped_streams.pass, Some(false));
    assert_eq!(result.checks.retained_streams.pass, Some(true));

    std::fs::write(&path, &original).unwrap();
    edit_table(&path, |table| table["dropped"][0]["size"] = json!(1));
    assert_eq!(verify(&path, None).checks.dropped_streams.pass, Some(false));

    std::fs::write(&path, original).unwrap();
    edit_table(&path, |table| {
        table["dropped"][0]["name"] = json!("MetaData.dat")
    });
    assert_eq!(verify(&path, None).checks.header_table.pass, Some(false));
}

#[test]
fn live_comments_and_tail_damage_do_not_change_fixed_verdict() {
    let (_dir, _source, path) = legacy_converted("verify_tail");
    let event = "t,add,1,,0,0,,note,1,,walk.tak|1,\r\n";
    let tail = format!("{event}bad\r\nt,add,2");
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(tail.as_bytes())
        .unwrap();
    let result = verify(&path, None);
    assert_eq!(result.verdict, Verdict::Identical);
    assert_eq!(result.comments.status.pass, Some(false));
    assert_eq!(result.comments.live_count, Some(1));
    assert_eq!(result.comments.malformed_lines, Some(1));
    assert_eq!(result.comments.skipped_lines, Some(2));
    assert_eq!(result.comments.cut_tail, Some(true));

    let mut raw = std::fs::read(&path).unwrap();
    let note = raw.windows(4).rposition(|b| b == b"note").unwrap();
    raw[note] = 0xff;
    std::fs::write(&path, raw).unwrap();
    let result = verify(&path, None);
    assert_eq!(result.verdict, Verdict::Identical);
    assert_eq!(result.comments.status.pass, Some(false));
    assert!(result.comments.status.detail.contains("UTF-8"));
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).into()).collect()
}

#[test]
fn conversion_flag_verifies_each_result_and_propagates_failure() {
    let dir = TempDir::new("convert_verify");
    let source = dir.join("good.tak");
    std::fs::write(&source, sample_tak()).unwrap();
    let rc = takm::run(&strings(&[
        "--to-takm",
        "--verify",
        source.to_str().unwrap(),
    ]));
    assert_eq!(rc, 0);
    assert_eq!(
        verify(&source.with_extension("takm"), None).verdict,
        Verdict::Identical
    );

    let no_camera = dir.join("no_camera.tak");
    std::fs::write(&no_camera, cfb(&[("Data", vec![5; 12])])).unwrap();
    let another = dir.join("also_good.tak");
    std::fs::copy(source, &another).unwrap();
    let rc = takm::run(&strings(&[
        "--to-takm",
        "--verify",
        no_camera.to_str().unwrap(),
        another.to_str().unwrap(),
    ]));
    assert_eq!(rc, 1);
    assert!(no_camera.with_extension("takm").exists());
    assert_eq!(
        verify(&another.with_extension("takm"), None).verdict,
        Verdict::Identical
    );
}

#[test]
fn cli_exit_codes_output_and_refusals() {
    let (dir, source, path) = converted("verify_output");
    let run = |args: &[&str]| super::run(&strings(args), |s| PathBuf::from(s));
    let input = path.to_str().unwrap();
    assert_eq!(run(&[]), 2);
    assert_eq!(run(&["bad.tak"]), 2);
    assert_eq!(run(&[input, "--source"]), 2);
    assert_eq!(run(&[input, "--source", "--unknown"]), 2);
    assert_eq!(run(&[input, "--source", "bad.takm"]), 2);
    assert_eq!(run(&[input, "--source", "a.tak", "--source", "b.tak"]), 2);
    assert_eq!(run(&[input, "a.json", "b.json"]), 2);

    let tak_bytes = std::fs::read(&source).unwrap();
    let takm_bytes = std::fs::read(&path).unwrap();
    assert_eq!(run(&[input, source.to_str().unwrap()]), 2);
    assert_eq!(run(&[input, input]), 2);
    assert_eq!(std::fs::read(&source).unwrap(), tak_bytes);
    assert_eq!(std::fs::read(&path).unwrap(), takm_bytes);

    std::fs::create_dir(dir.join("apps")).unwrap();
    std::fs::write(dir.join("AGENTS.md"), "").unwrap();
    std::fs::write(dir.join("apps/AGENTS.md"), "").unwrap();
    std::fs::create_dir(dir.join("input")).unwrap();
    let protected = dir.join("input").join("report.json");
    assert_eq!(run(&[input, protected.to_str().unwrap()]), 2);
    assert!(!protected.exists());

    let output = dir.join("report.json");
    assert_eq!(run(&[input, output.to_str().unwrap()]), 0);
    let report_bytes = std::fs::read(&output).unwrap();
    let report: Value = serde_json::from_slice(&report_bytes).unwrap();
    assert_eq!(report["source_unchanged"], true);
    assert_eq!(report["verdict"], "identical");
    assert_eq!(run(&[input, output.to_str().unwrap()]), 2);
    assert_eq!(std::fs::read(&output).unwrap(), report_bytes);

    let alias = dir.join("alias.json");
    if std::fs::hard_link(&source, &alias).is_ok() {
        assert_eq!(run(&[input, alias.to_str().unwrap()]), 2);
        assert_eq!(std::fs::read(&source).unwrap(), tak_bytes);
    }

    let failed = dir.join("failed.json");
    assert_eq!(
        run(&[input, "--source", "missing.tak", failed.to_str().unwrap()]),
        1
    );
    let report: Value = serde_json::from_slice(&std::fs::read(failed).unwrap()).unwrap();
    assert_eq!(report["verdict"], "failed");
    assert_eq!(report["source_unchanged"], false);
}

#[test]
fn batch_verification_matches_the_recorded_source_and_returns_all_verdicts() {
    let (dir, source, first) = converted("verify_batch");
    let sources = dir.join("sources");
    std::fs::create_dir(&sources).unwrap();
    std::fs::rename(&source, sources.join("walk.tak")).unwrap();
    let second = dir.join("renamed.takm");
    let tampered = dir.join("tampered.takm");
    let original = std::fs::read(&first).unwrap();
    std::fs::write(&second, &original).unwrap();
    let mut bad = original.clone();
    bad[HEADER_LEN as usize] ^= 1;
    std::fs::write(&tampered, &bad).unwrap();

    let resolve = |p: &str| dir.join(p);
    let execute = |args: &[&str]| {
        run(
            &args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>(),
            resolve,
        )
    };
    assert_eq!(
        execute(&[
            "walk.takm",
            "renamed.takm",
            "tampered.takm",
            "--source-dir",
            "sources",
            "--json",
            "batch.json"
        ]),
        1
    );
    let read =
        |path| serde_json::from_slice::<Value>(&std::fs::read(dir.join(path)).unwrap()).unwrap();
    let batch = read("batch.json");
    let items = batch.as_array().unwrap();
    assert_eq!(items.len(), 3);
    for item in items {
        assert_eq!(item["source_selection"], "source_dir");
        assert_eq!(item["schema"], 1);
        assert_eq!(item["producer"]["name"], "tak_viewer");
    }
    assert_eq!(items[0]["verdict"], "identical");
    assert_eq!(items[1]["verdict"], "identical");
    assert_eq!(items[2]["verdict"], "failed");
    assert_eq!(
        execute(&[
            "renamed.takm",
            "--source-dir",
            "sources",
            "--json",
            "single.json"
        ]),
        0
    );
    assert_eq!(read("single.json"), items[1]);
    assert_eq!(
        execute(&[
            "renamed.takm",
            "--source-dir",
            "sources",
            "--json",
            "wrapped.json",
            "--envelope"
        ]),
        0
    );
    let wrapped = read("wrapped.json");
    assert_eq!(wrapped["schema"], 1);
    assert_eq!(wrapped["items"], serde_json::json!([items[1]]));
    assert_eq!(
        execute(&["walk.takm", "renamed.takm", "--source", "x.tak"]),
        2
    );
    assert_eq!(execute(&["walk.takm", "--source-dir", "missing"]), 2);
    assert_eq!(
        execute(&["walk.takm", "--source-dir", "sources", "--source", "x.tak"]),
        2
    );
    assert_eq!(execute(&["walk.takm", "--source-dir", "."]), 1);
    assert_eq!(std::fs::read(&first).unwrap(), original);
    assert_eq!(std::fs::read(&second).unwrap(), original);
    assert_eq!(std::fs::read(&tampered).unwrap(), bad);
}

#[test]
fn source_directory_refuses_non_basename_names_in_the_takm_table() {
    let (dir, _, path) = legacy_converted("verify_source_dir_traversal");
    edit_table(&path, |table| {
        table["source"]["name"] = serde_json::json!("../walk.tak")
    });
    let result = verify_with_dir(&path, None, Some(&dir));
    assert_eq!(result.verdict, Verdict::Failed);
    assert!(result.source.is_none());
    assert_eq!(result.checks.source_name.pass, Some(false));
}
