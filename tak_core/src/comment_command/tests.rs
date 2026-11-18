use super::*;
use crate::sample_tak::sample_tak_with_devices;
use crate::test_support::TempDir;
use serde_json::json;

const TIME: &str = "2026-11-18T12:00:00+09:00";
const ROW: &[u8] =
    br#"[{"markers":["LKNE","RKNE"],"start":0,"end":299,"type":"rAnGe BaD","text":"check"}]"#;

fn fixture(name: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new(name);
    let path = dir.join("a.takm");
    let mut output = fs::File::create(&path).unwrap();
    takm::write(&mut output, &sample_tak_with_devices(), "a.tak", TIME).unwrap();
    (dir, path)
}

fn add_default(path: &Path, rows: &[u8]) -> Result<AddResult, String> {
    add(path, rows, AddOptions::default(), || TIME.into())
}

fn coordination_files(path: &Path) -> (PathBuf, PathBuf) {
    let key = takm::path_lock_key(path).unwrap();
    let root = takm::coordination_root();
    (
        root.join(format!(".tak_viewer_{key}.lock")),
        root.join(format!(".tak_viewer_comment_ids_{key}")),
    )
}

#[test]
fn parses_bom_optional_type_and_korean_csv_fields() {
    let (_dir, path) = fixture("comment_command_parse");
    let original = fs::read(&path).unwrap();
    let rows = concat!(
        "\u{feff}",
        r#"[{"markers":["LKNE","RKNE"],"start":0,"end":299,
        "type":"needs VISUAL check","text":"한 줄, \"확인\"\t "},{"markers":[],"start":25,"end":25,"text":""}]"#
    );
    let result = add_default(&path, rows.as_bytes()).unwrap();
    assert_eq!(result.live_total, 2);
    let events = takm::read_comments(&path).unwrap().events;
    assert_eq!(events[0].comment_type, "Needs visual check");
    assert_eq!(events[0].text, "한 줄, \"확인\"  ");
    assert_eq!(events[1].comment_type, "");
    assert_eq!(events[1].text, "");
    assert!(events[1].markers.is_empty());
    assert!(events.iter().all(|e| e.time == TIME
        && e.operation.as_deref().is_some_and(|token| token.starts_with("agent-"))
        && comments::writer_of(e) == "agent"
        && e.file_a
            == comments::file_label(&path, takm::read_index_path(&path).unwrap().fixed_len)
        && e.shown_a == "1"
        && e.shown_b.is_empty()
        && e.file_b.is_empty()));
    assert!(fs::read(&path).unwrap().starts_with(&original));
    assert!(result.fixed_unchanged);
}

#[test]
fn accepts_current_and_retired_types_and_lists_only_current_choices_in_refusal() {
    let (_dir, path) = fixture("comment_command_types");
    let retired = [
        "Jitter", "Jump", "Visual check", "Agree", "No problem", "Keep this only",
        "Needs visual check", "Marker swap",
    ];
    let expected = comments::TYPES.iter().copied().chain(retired);
    let rows: Vec<_> = expected.clone().enumerate().map(|(frame, kind)| {
        json!({"markers": [], "start": frame, "end": frame, "type": kind.to_ascii_lowercase(), "text": ""})
    }).collect();
    add_default(&path, &serde_json::to_vec(&rows).unwrap()).unwrap();
    let all = list(&path).unwrap();
    assert!(all.comments.iter().map(|row| row.content.comment_type.as_str()).eq(expected));

    let before = fs::read(&path).unwrap();
    let error = add_default(&path, br#"[{"markers":[],"start":1,"end":1,"type":"Other","text":""}]"#).unwrap_err();
    assert!(error.contains(&comments::TYPES.join(", ")), "{error}");
    assert!(retired.iter().all(|name| !error.contains(name)), "{error}");
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn reports_every_bad_row_without_writing_any_valid_rows() {
    let (_dir, path) = fixture("comment_command_invalid_rows");
    let original = fs::read(&path).unwrap();
    let rows = json!([
        {"markers":[],"start":0,"end":0,"text":"valid"},
        {"markers":[],"start":1.0,"end":"2","text":""},
        {"markers":[],"start":4,"end":3,"text":""},
        {"markers":[],"start":-1,"end":300,"text":""},
        {"markers":["NOT_PRESENT","Unl"],"start":1,"end":1,"text":""},
        {"markers":[],"start":1,"end":1,"type":"unknown","text":""},
        {"markers":[],"start":1,"end":1,"type":null,"text":""},
        {"markers":[],"start":1,"end":1,"text":"one\ntwo"},
        {"markers":[],"start":1,"end":1,"text":"one\rtwo"},
        {"markers":[false,"","bad\nname"],"start":0,"end":0,"text":false},
        {"start":0,"end":0,"text":""},
        null
    ]);
    let error = add_default(&path, &serde_json::to_vec(&rows).unwrap()).unwrap_err();
    assert_eq!(error.lines().count(), 11, "{error}");
    for (index, reason) in [
        "정수",
        "시작 프레임",
        "범위",
        "NOT_PRESENT",
        "unknown",
        "type 값",
        "줄바꿈",
        "줄바꿈",
        "번째 이름",
        "markers",
        "JSON 객체",
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            error
                .lines()
                .any(|line| line.starts_with(&format!("{}번째 행:", index + 2))
                    && line.contains(reason)),
            "{error}"
        );
    }
    assert!(error.contains("Unl") && error.contains("start") && error.contains("end"));
    for invalid in [b"{}".as_slice(), b"[invalid]", b"\xff"] {
        assert!(add_default(&path, invalid).is_err());
    }
    assert_eq!(fs::read(&path).unwrap(), original);
    let (lock, ledger) = coordination_files(&path);
    assert!(!lock.exists() && !ledger.exists());
}

#[test]
fn refuses_missing_tak_non_takm_obsolete_corrupt_and_repo_input() {
    let (dir, path) = fixture("comment_command_targets");
    let original = fs::read(&path).unwrap();
    for (name, bytes, reason) in [
        ("original.TAK", original.clone(), "기존 .takm"),
        ("old.takm", b"TAKM\r\n\x1a\n".to_vec(), "예전 .takm"),
        ("fake.takm", b"not a TAKM".to_vec(), "TAKM-2"),
    ] {
        let target = dir.join(name);
        fs::write(&target, &bytes).unwrap();
        assert!(add_default(&target, ROW).unwrap_err().contains(reason));
        assert_eq!(fs::read(target).unwrap(), bytes);
    }
    assert!(add_default(&dir.join("missing.takm"), ROW)
        .unwrap_err()
        .contains("찾지 못"));
    let mut corrupt = original.clone();
    corrupt[takm::HEADER_LEN as usize] ^= 1;
    fs::write(&path, &corrupt).unwrap();
    assert!(add_default(&path, ROW).unwrap_err().contains("SHA-256"));
    assert_eq!(fs::read(path).unwrap(), corrupt);

    let repo = dir.join("repo");
    fs::create_dir_all(repo.join("apps")).unwrap();
    fs::create_dir_all(repo.join("input")).unwrap();
    fs::write(repo.join("AGENTS.md"), "").unwrap();
    fs::write(repo.join("apps/AGENTS.md"), "").unwrap();
    let input = repo.join("input/protected.takm");
    fs::write(&input, &original).unwrap();
    assert!(add_default(&input, ROW).unwrap_err().contains("input/"));
    assert!(check_output(&repo.join("input/out.json")).is_err());
    assert_eq!(fs::read(input).unwrap(), original);
    for name in ["out.TAK", "out.TAKM"] {
        assert!(check_output(&dir.join(name)).is_err());
    }
}

#[test]
fn skips_live_and_batch_duplicates_and_can_append_them_explicitly() {
    let (_dir, path) = fixture("comment_command_duplicates");
    let rows = br#"[
        {"markers":["LKNE","RKNE"],"start":0,"end":299,"type":"range bad","text":"check"},
        {"markers":["RKNE","LKNE"],"start":0,"end":299,"type":"Range bad","text":"check"}
    ]"#;
    let first = add_default(&path, rows).unwrap();
    assert_eq!(first.added.len(), 1);
    assert_eq!(first.skipped_existing.len(), 1);
    assert_eq!(first.skipped_existing[0].row, 2);
    let old = fs::read(&path).unwrap();
    let again = add_default(&path, rows).unwrap();
    assert!(again.added.is_empty());
    assert_eq!(
        again
            .skipped_existing
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        [Some(1), Some(1)]
    );
    assert_eq!(fs::read(&path).unwrap(), old);
    let result = add(
        &path,
        rows,
        AddOptions {
            allow_duplicates: true,
            ..Default::default()
        },
        || TIME.into(),
    )
    .unwrap();
    assert_eq!(result.live_total, 3);
    assert_eq!(
        result.added.iter().map(|row| row.id).collect::<Vec<_>>(),
        [Some(2), Some(3)]
    );
    assert!(result.skipped_existing.is_empty());
}

#[test]
fn dry_run_checks_and_plans_without_lock_ledger_or_append() {
    let (_dir, path) = fixture("comment_command_dry_run");
    let old = fs::read(&path).unwrap();
    let (lock, ledger) = coordination_files(&path);
    let options = AddOptions {
        dry_run: true,
        allow_unknown_markers: true,
        ..Default::default()
    };
    let rows = br#"[{"markers":["foreign;name","a\\b"],"start":5,"end":6,"type":"","text":""}]"#;
    assert!(add_default(&path, rows).is_err());
    let result = add(&path, rows, options, || {
        panic!("dry run must not create a timestamp")
    })
    .unwrap();
    assert!(result.dry_run && result.fixed_unchanged);
    assert_eq!(
        (result.added.len(), result.added[0].id, result.live_total),
        (1, None, 0)
    );
    assert!(!lock.exists() && !ledger.exists());
    assert_eq!(fs::read(&path).unwrap(), old);
    let result = add(
        &path,
        rows,
        AddOptions {
            dry_run: false,
            ..options
        },
        || TIME.into(),
    )
    .unwrap();
    assert_eq!(result.added[0].id, Some(1));
    assert_eq!(
        list(&path).unwrap().comments[0].content.markers,
        ["foreign;name", "a\\b"]
    );
}

#[test]
fn command_and_window_writer_share_the_path_lock_and_id_reservations() {
    let (_dir, path) = fixture("comment_command_window_writer");
    let writer = comments::Writer::spawn(Box::new(|| {}));
    writer.drain().unwrap();
    let ids = comments::reserve_takm_ids(&path, 1024).unwrap();
    let window = Comment {
        id: *ids.start(),
        markers: vec!["LKNE".into()],
        start: 2,
        end: 3,
        comment_type: String::new(),
        text: "window".into(),
    };
    let event = Event::of(Kind::Add, &window, TIME.into(), "a.takm".into());
    let barrier = std::sync::Barrier::new(2);
    let command = std::thread::scope(|scope| {
        let pending = scope.spawn(|| {
            barrier.wait();
            add_default(&path, ROW).unwrap()
        });
        barrier.wait();
        assert!(writer.send(1, path.clone(), event));
        writer.drain().unwrap();
        assert!(writer.poll().pop().unwrap().result.is_ok());
        pending.join().unwrap()
    });
    assert_eq!((window.id, command.added[0].id), (1, Some(1025)));
    assert!(!ids.contains(&command.added[0].id.unwrap()));
    let parsed = takm::read_comments(&path).unwrap().comments();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed.get(window.id), Some(&window));
}

#[test]
fn simultaneous_equal_batches_add_only_once() {
    let (_dir, path) = fixture("comment_command_simultaneous");
    let barrier = std::sync::Barrier::new(2);
    let import = || {
        barrier.wait();
        add_default(&path, ROW).unwrap().added.len()
    };
    assert_eq!(
        std::thread::scope(|scope| {
            let pending = scope.spawn(import);
            let local = import();
            local + pending.join().unwrap()
        }),
        1
    );
    assert_eq!(list(&path).unwrap().comments.len(), 1);
}

#[test]
fn cuts_tail_before_append_and_lists_real_timestamps_and_bad_rows() {
    let (dir, path) = fixture("comment_command_list");
    let fixed_len = takm::read_index_path(&path).unwrap().tail_start() as usize;
    let fixed = fs::read(&path).unwrap()[..fixed_len].to_vec();
    let mut append = OpenOptions::new().append(true).open(&path).unwrap();
    append
        .write_all(b"bad line\r\ncut,add,33,\"unterminated")
        .unwrap();
    drop(append);
    let before = list(&path).unwrap();
    assert_eq!(
        (
            before.cut_tail,
            before.skipped_lines,
            before.malformed_lines
        ),
        (true, 2, 1)
    );
    add_default(&path, ROW).unwrap();
    // A command-written Comment says so; a later window edit makes it the person's.
    assert_eq!(list(&path).unwrap().comments[0].by, "agent");
    let edited = Comment {
        id: 1,
        markers: vec![],
        start: 3,
        end: 4,
        comment_type: "Jitter".into(),
        text: "edited".into(),
    };
    takm::append_events(
        &path,
        &[Event::of(
            Kind::Edit,
            &edited,
            "edited-time".into(),
            "a.takm".into(),
        )],
    )
    .unwrap();
    let out = dir.join("out.json");
    assert_eq!(
        run(
            &[
                "--list-comments".into(),
                path.display().to_string(),
                out.display().to_string()
            ],
            |s| PathBuf::from(s),
            || panic!("listing does not use a clock")
        ),
        0
    );
    let output: Value = serde_json::from_slice(&fs::read(out).unwrap()).unwrap();
    assert_eq!(
        output["comments"],
        json!([{"id":1,"markers":[],"start":3,"end":4,
        "type":"Jitter","text":"edited","time":"edited-time","by":"viewer"}])
    );
    assert_eq!(output["cut_tail"], false);
    assert_eq!(output["skipped_lines"], 2);
    assert_eq!(output["malformed_lines"], 2);
    assert_eq!(fs::read(path).unwrap()[..fixed_len], fixed);
}

#[test]
fn bad_arguments_are_distinct_from_failed_operations() {
    for args in [
        vec!["--add-comments", "a.takm"],
        vec!["--add-comments", "a.takm", "--from", "--dry-run"],
        vec!["--add-comments", "a.takm", "--from", "a.json", "--bogus"],
        vec!["--list-comments"],
        vec!["--list-comments", "a.takm", "out.json", "extra"],
    ] {
        let args = args.into_iter().map(String::from).collect::<Vec<_>>();
        assert_eq!(
            run(&args, |_| panic!("bad arguments"), || unreachable!()),
            2
        );
    }
    let args = ["--list-comments".into(), "no-such-file.takm".into()];
    assert_eq!(run(&args, |s| PathBuf::from(s), || unreachable!()), 1);
}

#[test]
fn batch_preflights_every_file_and_reserves_no_ids_for_invalid_rows() {
    let (dir, a) = fixture("comment_batch");
    let b = dir.join("b.takm");
    let original = fs::read(&a).unwrap();
    fs::write(&b, &original).unwrap();
    let valid: Value = serde_json::from_slice(ROW).unwrap();
    let input = |second| {
        let bytes = serde_json::to_vec(&json!({"a.takm": valid, "b.takm": second})).unwrap();
        read_input::<batch::BatchRows>(&bytes, "파일별 객체").unwrap()
    };
    let resolve = |file: &str| dir.join(file);
    let (items, failed) = batch::add(
        input(json!([{"markers":[],"start":300,"end":300,"text":"bad"}])),
        resolve,
        AddOptions::default(),
        || panic!("must preflight both files"),
    );
    let items = serde_json::to_value(items).unwrap();
    assert!(failed);
    assert_eq!(items[0]["status"], "not_written");
    assert!(items[0]["error"].is_null());
    assert!(items[1]["error"].as_str().unwrap().contains("1번째 행"));
    for file in [&a, &b] {
        assert_eq!(fs::read(file).unwrap(), original);
        let (lock, ledger) = coordination_files(file);
        assert!(!lock.exists() && !ledger.exists());
    }

    let (items, failed) = batch::add(input(valid.clone()), resolve, AddOptions::default(), || {
        TIME.into()
    });
    assert!(!failed);
    let doc = serde_json::to_value(Envelope::new(1, items)).unwrap();
    for item in doc["items"].as_array().unwrap() {
        assert_eq!(item["schema"], 1);
        assert_eq!(item["producer"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(item["added"][0]["id"], 1);
        assert_eq!(item["live_total"], 1);
    }
    assert_eq!(doc["schema"], 1);

    let options = AddOptions {
        dry_run: true,
        allow_duplicates: true,
        ..Default::default()
    };
    let (items, failed) = batch::add(input(valid.clone()), resolve, options, || panic!("dry run"));
    assert!(!failed);
    let doc = serde_json::to_value(items).unwrap();
    assert!(doc[0]["added"][0]["id"].is_null());
    assert_eq!(list(&a).unwrap().comments.len(), 1);
    assert_eq!(list(&b).unwrap().comments.len(), 1);
}

#[test]
fn batch_rejects_duplicate_keys_and_two_paths_to_one_file() {
    for raw in [br#"{"a.takm":[],"a.takm":[]}"#.as_slice(), b"{}", b"[]"] {
        assert!(read_input::<batch::BatchRows>(raw, "파일별 객체").is_err());
    }
    let (dir, a) = fixture("comment_batch_alias");
    let alias = dir.join("b.takm");
    fs::hard_link(&a, &alias).unwrap();
    let original = fs::read(&a).unwrap();
    let valid: Value = serde_json::from_slice(ROW).unwrap();
    let rows = serde_json::to_vec(&json!({"a.takm":valid,"b.takm":valid})).unwrap();
    let (items, failed) = batch::add(
        read_input(&rows, "파일별 객체").unwrap(),
        |p| dir.join(p),
        AddOptions::default(),
        || panic!("aliased file"),
    );
    assert!(failed);
    assert_eq!(
        serde_json::to_value(items).unwrap()[1]["status"],
        "not_written"
    );
    assert_eq!(fs::read(a).unwrap(), original);
}

#[test]
fn directory_list_reports_writer_filter_and_tail_damage_without_changing_the_single_shape() {
    let (dir, a) = fixture("comment_list_batch");
    let nested = dir.join("nested");
    fs::create_dir(&nested).unwrap();
    let b = nested.join("b.TAKM");
    fs::copy(&a, &b).unwrap();
    let c = dir.join("c.takm");
    fs::write(&c, b"broken").unwrap();
    add_default(&a, ROW).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&a)
        .unwrap()
        .write_all(b"bad row\r\ncut")
        .unwrap();
    let plain = serde_json::to_value(list(&a).unwrap()).unwrap();
    assert_eq!(
        plain,
        json!({
            "file": a.display().to_string(),
            "comments": [{"id":1,"markers":["LKNE","RKNE"],"start":0,"end":299,
                          "type":"Range bad","text":"check","time":TIME,"by":"agent"}],
            "skipped_lines":2,"malformed_lines":1,"cut_tail":true,
        })
    );
    assert_eq!(list_by(&a, Some("agent")).unwrap().0.comments.len(), 1);
    assert_eq!(list_by(&a, Some("viewer")).unwrap().0.comments.len(), 0);
    assert_eq!(list_by(&a, Some("legacy")).unwrap().0.comments.len(), 0);
    assert_eq!(
        batch::discover(&dir, false).unwrap(),
        [a.clone(), c.clone()]
    );
    let (items, failed) = batch::list(batch::discover(&dir, true).unwrap(), Some("viewer"));
    let doc = serde_json::to_value(Envelope::new(1, items)).unwrap();
    assert!(failed);
    assert_eq!(doc["items"][0]["live_count"], 1);
    assert_eq!(doc["items"][0]["comments"], json!([]));
    assert_eq!(doc["items"][0]["cut_tail"], true);
    assert_eq!(doc["items"][1]["file"], c.display().to_string());
    assert!(doc["items"][1]["error"].is_string());
    assert!(doc["items"][1]["malformed_lines"].is_null());
    assert_eq!(doc["items"][2]["file"], b.display().to_string());
    assert_eq!(doc["items"][2]["live_count"], 0);
}

#[test]
fn comment_batch_arguments_accept_both_modes_and_refuse_mixed_modes() {
    let parse = |words: &[&str]| -> String {
        let args = words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        format!("{:?}", parse_args(&args).unwrap())
    };
    assert!(
        parse(&["--list-comments", "a.takm", "out.json"]).contains("output: Some(\"out.json\")")
    );
    assert!(
        parse(&["--list-comments", "a.takm", "b.takm", "--json", "out.json"])
            .contains("files: [\"a.takm\", \"b.takm\"]")
    );
    assert!(parse(&[
        "--list-comments",
        "--dir",
        "takes",
        "--recursive",
        "--by",
        "agent",
        "--envelope"
    ])
    .contains("recursive: true"));
    assert!(parse(&[
        "--add-comments",
        "--from",
        "rows.json",
        "--dry-run",
        "--envelope"
    ])
    .contains("file: None"));
    for words in [
        &["--list-comments", "a.takm", "--recursive"][..],
        &["--list-comments", "a.takm", "--dir", "takes"],
        &["--list-comments", "--dir", "takes", "--by", "other"],
        &["--list-comments", "--dir"],
        &["--list-comments", "a.takm", "--json"],
        &["--add-comments", "--from"],
        &[
            "--add-comments",
            "a.takm",
            "--from",
            "rows.json",
            "--envelope",
        ],
    ] {
        let args = words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        assert!(parse_args(&args).is_err(), "{words:?}");
    }
}
