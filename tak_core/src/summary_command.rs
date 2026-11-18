//! Headless command options, text formatting and JSON export for `--summary`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::summary::{
    self, Coverage, FrameRange, MarkerGaps, SummaryError, SummaryOptions, TakeSummary,
};
use crate::{tak, takm};

pub const USAGE: &str =
    "사용법: tak_viewer --summary <file.tak|file.takm> [<file2> …] [--markers A,B,…] [--frames a:b] [--top N] [--json [out.json]]";

#[derive(Debug)]
struct Command<'a> {
    files: Vec<&'a str>,
    options: SummaryOptions,
    json: Option<Option<&'a str>>,
}

fn parse(args: &[String]) -> Result<Command<'_>, SummaryError> {
    let mut args = args.iter().map(String::as_str).peekable();
    let mut files = Vec::new();
    while let Some(file) = args.next_if(|arg| !arg.starts_with('-')) {
        files.push(file);
    }
    if files.is_empty() {
        return Err(SummaryError::Argument(
            "요약할 .tak 또는 .takm 파일을 먼저 지정해 주세요.".into(),
        ));
    }
    let mut options = SummaryOptions::default();
    let mut top = None;
    let mut json = None;
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next_if(|s| !s.starts_with("--"))
                .ok_or_else(|| SummaryError::Argument(format!("{flag} 뒤에 값을 써 주세요.")))
        };
        match flag {
            "--markers" if options.markers.is_empty() => {
                options.markers = value()?
                    .split(',')
                    .map(|name| name.trim().to_owned())
                    .collect();
                if options.markers.iter().any(String::is_empty) {
                    return Err(SummaryError::Argument(
                        "--markers에 마커 이름을 쉼표로 나눠 써 주세요.".into(),
                    ));
                }
            }
            "--frames" if options.frames.is_none() => {
                let range = value()?
                    .split_once(':')
                    .and_then(|(a, b)| {
                        Some(FrameRange {
                            first: a.parse().ok()?,
                            last: b.parse().ok()?,
                        })
                    })
                    .filter(|range| range.first <= range.last);
                options.frames = Some(range.ok_or_else(|| {
                    SummaryError::Argument(
                        "--frames는 시작:끝 형식으로 써 주세요. 시작은 끝보다 클 수 없어요.".into(),
                    )
                })?);
            }
            "--top" if top.is_none() => {
                top = Some(value()?.parse().map_err(|_| {
                    SummaryError::Argument("--top 뒤에 0 이상의 정수를 써 주세요.".into())
                })?);
            }
            "--json" if json.is_none() => json = Some(args.next_if(|s| !s.starts_with('-'))),
            "--json" | "--top" | "--frames" | "--markers" => {
                return Err(SummaryError::Argument(format!(
                    "{flag}는 한 번만 쓸 수 있어요."
                )));
            }
            _ => {
                return Err(SummaryError::Argument(format!(
                    "알 수 없는 요약 옵션이에요. ({flag})"
                )))
            }
        }
    }
    options.top = top.unwrap_or(options.top);
    Ok(Command {
        files,
        options,
        json,
    })
}

fn check_output(path: &Path) -> Result<PathBuf, SummaryError> {
    let actual = takm::canonical_target(path).map_err(|e| {
        SummaryError::Argument(format!("JSON을 저장할 경로를 확인하지 못했어요. ({e})"))
    })?;
    if [path, &actual]
        .iter()
        .any(|p| tak::is_take_path(p) || takm::is_takm_path(p))
    {
        return Err(SummaryError::Argument(
            "JSON을 .tak나 .takm 파일에 저장할 수 없어요.".into(),
        ));
    }
    if takm::in_input(path) || takm::in_input(&actual) {
        return Err(SummaryError::Argument(
            "input/ 폴더에는 JSON을 저장하지 않아요.".into(),
        ));
    }
    Ok(actual)
}

fn write_json(destination: &Path, json: &[u8]) -> io::Result<()> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let temporary = destination.with_file_name(format!(
        ".tak-summary-{}-{}.part",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let written = file.write_all(json);
    drop(file);
    let result = written.and_then(|()| fs::rename(&temporary, destination));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn execute(
    args: &[String],
    resolve: impl Fn(&str) -> PathBuf,
    stdout: &mut impl Write,
) -> Result<(), SummaryError> {
    let command = parse(args)?;
    let destination = command
        .json
        .flatten()
        .map(&resolve)
        .as_deref()
        .map(check_output)
        .transpose()?;
    let summaries = command
        .files
        .into_iter()
        .map(|file| {
            let path = resolve(file);
            summary::read_summary(&path, &command.options).map_err(|error| match error {
                SummaryError::Argument(message) => {
                    SummaryError::Argument(format!("{file} {message}"))
                }
                SummaryError::Read(message) => SummaryError::Read(format!("{file} {message}")),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let write = if command.json.is_some() {
        let mut json = serde_json::to_vec_pretty(&summaries)
            .map_err(|e| SummaryError::Read(format!("요약을 JSON으로 만들지 못했어요. ({e})")))?;
        json.push(b'\n');
        match destination {
            Some(path) => write_json(&path, &json),
            None => stdout.write_all(&json),
        }
    } else {
        summaries.iter().enumerate().try_for_each(|(i, summary)| {
            if i > 0 {
                writeln!(stdout)?;
            }
            text(stdout, summary)
        })
    };
    write.map_err(|e| SummaryError::Read(format!("요약을 출력하지 못했어요. ({e})")))
}

/// Run before any window or settings setup. `args` excludes the `--summary` flag.
pub fn run(args: &[String], resolve: impl Fn(&str) -> PathBuf) -> i32 {
    if matches!(args, [help] if help == "--help" || help == "-h") {
        println!("{USAGE}");
        return 0;
    }
    let mut stdout = io::stdout().lock();
    match execute(args, resolve, &mut stdout) {
        Ok(()) => 0,
        Err(SummaryError::Argument(message)) => {
            eprintln!("{message}\n{USAGE}");
            2
        }
        Err(SummaryError::Read(message)) => {
            eprintln!("{message}");
            1
        }
    }
}

fn value_or_unknown<T: ToString>(value: Option<T>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "알 수 없음".into())
}

fn text(out: &mut impl Write, summary: &TakeSummary) -> io::Result<()> {
    let file = &summary.file;
    let progress = &file.progress;
    let progress_label = match progress.value {
        Some(1) => "녹화됨",
        Some(4) => "라벨링 완료",
        _ => "알 수 없음",
    };
    writeln!(
        out,
        "{} ({}, {}바이트)",
        file.name, file.kind, file.size_bytes
    )?;
    writeln!(
        out,
        "  Progress {} · {progress_label}",
        value_or_unknown(progress.value)
    )?;
    writeln!(
        out,
        "  프레임 {} ({}개) · {} Hz · {}초",
        file.frames,
        file.frame_count,
        value_or_unknown(file.frame_rate),
        value_or_unknown(file.duration_seconds.map(|secs| format!("{secs:.3}")))
    )?;
    if let Some(takm) = &summary.takm {
        let seal = if takm.seal_ok {
            "정상"
        } else {
            "확인 실패"
        };
        writeln!(
            out,
            "  봉인 {seal} · Comment {}개 · 원본 {}",
            takm.comment_count, takm.source_name
        )?;
    }
    let markers = &summary.markers;
    writeln!(
        out,
        "  마커 세트 {} · 추가 {} · 라벨 없음 {} · 구조 미확인 {}",
        markers.template, markers.extra, markers.unlabeled, markers.unknown
    )?;
    if !markers.assets.is_empty() {
        let assets: Vec<_> = markers
            .assets
            .iter()
            .map(|asset| match &asset.markerset_name {
                Some(set) => format!("{} ({set}) {}개", asset.name, asset.template_count),
                None => format!("{} {}개", asset.name, asset.template_count),
            })
            .collect();
        writeln!(out, "  마커 세트별 {}", assets.join(" · "))?;
    }
    coverage_text(out, "전체", &summary.coverage)?;
    if let Some(window) = &summary.window_coverage {
        coverage_text(out, "지정 구간", window)?;
    }
    let gaps = &summary.gaps;
    gap_text(out, "마커 세트 빈 구간", &gaps.template, gaps.template_more)?;
    gap_text(out, "추가 마커 빈 구간", &gaps.extra, gaps.extra_more)?;
    if !gaps.other.is_empty() {
        gap_text(out, "그 외 지정 마커 빈 구간", &gaps.other, 0)?;
    }
    let devices = &summary.devices;
    let analog: Vec<_> = devices
        .analog
        .iter()
        .enumerate()
        .map(|(i, device)| {
            let name = device.name.clone().unwrap_or_else(|| format!("#{}", i + 1));
            // Short text: channels sharing one rate collapse to "프레임당 N샘플"; JSON keeps every channel.
            let mut rates: Vec<(String, usize)> = Vec::new();
            for channel in &device.channels {
                let rate = value_or_unknown(channel.samples_per_frame);
                match rates.iter_mut().find(|(known, _)| *known == rate) {
                    Some((_, count)) => *count += 1,
                    None => rates.push((rate, 1)),
                }
            }
            let rates = match rates.as_slice() {
                [] => "채널 정보 없음".to_owned(),
                [(rate, _)] => format!("프레임당 {rate}샘플"),
                many => many
                    .iter()
                    .map(|(rate, count)| format!("프레임당 {rate}샘플 채널 {count}개"))
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            format!("{name} (채널 {}개 · {rates})", device.channel_count)
        })
        .collect();
    writeln!(
        out,
        "  힘판 {}개 · 아날로그 장치 {}개",
        devices.force_plate_count, devices.analog_device_count
    )?;
    if !analog.is_empty() {
        writeln!(out, "  아날로그 {}", analog.join(" · "))?;
    }
    for warning in &summary.warnings {
        writeln!(out, "  읽기 참고 · {warning}")?;
    }
    Ok(())
}

fn coverage_text(out: &mut impl Write, label: &str, stats: &Coverage) -> io::Result<()> {
    writeln!(
        out,
        "  {label} {} · 관측 최소 {}/{} · 중앙값 {}/{} · 모두 관측 {}프레임 · 누락 {}칸",
        stats.frames,
        stats.min_present,
        stats.marker_total,
        stats.median_present,
        stats.marker_total,
        stats.complete_frames,
        stats.missing_cells
    )
}

fn gap_text(out: &mut impl Write, label: &str, rows: &[MarkerGaps], more: usize) -> io::Result<()> {
    if rows.is_empty() && more == 0 {
        return writeln!(out, "  {label} 없음");
    }
    writeln!(out, "  {label}")?;
    for row in rows {
        write!(
            out,
            "    {} · {}구간 · 누락 {}프레임",
            row.name, row.gap_count, row.missing_frames
        )?;
        if let Some(gap) = &row.longest_gap {
            write!(out, " · 최장 {}프레임 ({})", gap.length, gap.frames)?;
        }
        if let Some(ranges) = &row.ranges {
            let ranges: Vec<_> = ranges.iter().map(ToString::to_string).collect();
            if !ranges.is_empty() {
                write!(out, " · 전체 범위 {}", ranges.join(", "))?;
            }
        }
        writeln!(out)?;
    }
    if more > 0 {
        writeln!(out, "    그 외 {more}개")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comments::{self, Event, Kind};
    use crate::sample_tak::sample_tak_with_devices;
    use crate::test_support::TempDir;
    use serde_json::Value;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    fn in_folder(folder: &Path, words: &[&str], stdout: &mut Vec<u8>) -> Result<(), SummaryError> {
        execute(&args(words), |name| folder.join(name), stdout)
    }

    fn write_takm(path: &Path, source: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        takm::write(&mut bytes, &sample_tak_with_devices(), source, "2026-11-18").unwrap();
        fs::write(path, &bytes).unwrap();
        bytes
    }

    #[test]
    fn parses_multiple_files_and_supports_zero_top_and_stdout_json() {
        let args = args(&[
            "a.tak",
            "b.takm",
            "--frames",
            "-1:22",
            "--markers",
            "A, B",
            "--json",
            "--top",
            "0",
        ]);
        let parsed = parse(&args).unwrap();
        assert_eq!(parsed.files, ["a.tak", "b.takm"]);
        assert_eq!(
            parsed.options.frames,
            Some(FrameRange {
                first: -1,
                last: 22
            })
        );
        assert_eq!(parsed.options.markers, ["A", "B"]);
        assert_eq!(parsed.options.top, 0);
        assert_eq!(parsed.json, Some(None));
    }

    #[test]
    fn rejects_bad_option_syntax() {
        for words in [
            &[][..],
            &["--json"],
            &["a.tak", "--frames"],
            &["a.tak", "--frames", "1-2"],
            &["a.tak", "--frames", "10:1"],
            &["a.tak", "--frames", "1:2:3"],
            &["a.tak", "--markers", "A, "],
            &["a.tak", "--markers", "--json"],
            &["a.tak", "--top", "-1"],
            &["a.tak", "--top", "1.2"],
            &["a.tak", "--json", "--json"],
            &["a.tak", "--bogus"],
        ] {
            assert!(
                matches!(parse(&args(words)), Err(SummaryError::Argument(_))),
                "{words:?}"
            );
        }
    }

    #[test]
    fn two_files_json_reports_live_comments_seal_and_devices() {
        let scratch = TempDir::new("summary_json");
        let first = scratch.join("a.takm");
        let second = scratch.join("b.takm");
        let baseline = write_takm(&first, "source-a.tak");
        let mut bytes = write_takm(&second, "source-b.tak");
        let idx = takm::read_index_path(&second).unwrap();
        bytes[idx.fixed_len as usize] ^= 1;
        let event = |kind, id| Event {
            time: "2026-11-18T10:00:00+09:00".into(),
            kind,
            id,
            markers: Vec::new(),
            start: 0,
            end: 1,
            comment_type: String::new(),
            text: "example".into(),
            shown_a: "1".into(),
            shown_b: String::new(),
            file_a: String::new(),
            file_b: String::new(),
            operation: None,
            expected: None,
        };
        for event in [
            event(Kind::Add, 1),
            event(Kind::Add, 2),
            event(Kind::Delete, 1),
        ] {
            bytes.extend_from_slice(comments::encode(&event).as_bytes());
        }
        bytes.extend_from_slice(b"bad row\ncut");
        fs::write(&second, &bytes).unwrap();

        let mut stdout = Vec::new();
        in_folder(
            &scratch,
            &["a.takm", "b.takm", "--frames", "99:101", "--json"],
            &mut stdout,
        )
        .unwrap();
        let json: Vec<Value> = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(json.len(), 2);
        assert_eq!(json[0]["file"]["name"], "a.takm");
        assert_eq!(json[0]["file"]["frame_count"], 300);
        assert_eq!(
            json[0]["takm"],
            serde_json::json!({
                "seal_ok": true, "comment_count": 0, "source_name": "source-a.tak"
            })
        );
        assert_eq!(json[0]["markers"]["template"], 2);
        assert_eq!(json[0]["coverage"]["missing_cells"], 40);
        assert_eq!(json[0]["window_coverage"]["missing_cells"], 2);
        assert_eq!(json[0]["gaps"]["template"][0]["missing_frames"], 2);
        assert_eq!(json[0]["devices"]["force_plate_count"], 1);
        assert_eq!(json[0]["devices"]["analog_device_count"], 2);
        assert_eq!(
            json[0]["devices"]["analog"][0]["channels"][0]["samples_per_frame"],
            1
        );
        assert_eq!(json[1]["file"]["name"], "b.takm");
        assert_eq!(
            json[1]["takm"],
            serde_json::json!({
                "seal_ok": false, "comment_count": 1, "source_name": "source-b.tak"
            })
        );
        let warnings = json[1]["warnings"].to_string();
        assert!(warnings.contains("봉인을 확인하지") && warnings.contains("잘린 Comment"));
        assert_eq!(fs::read(first).unwrap(), baseline);
        assert_eq!(fs::read(second).unwrap(), bytes);
    }

    #[test]
    fn text_and_file_export_are_headless_and_bad_input_leaves_output_untouched() {
        let scratch = TempDir::new("summary_export");
        write_takm(&scratch.join("real.takm"), "source.tak");
        let mut stdout = Vec::new();
        in_folder(&scratch, &["real.takm", "--markers", "RKNE"], &mut stdout).unwrap();
        let text = String::from_utf8(stdout).unwrap();
        assert!(text.contains("관측 최소 1/2"));
        assert!(text.contains(
            "RKNE · 1구간 · 누락 40프레임 · 최장 40프레임 (100–139) · 전체 범위 100–139"
        ));
        let mut stdout = Vec::new();
        let output = scratch.join("output.json");
        fs::write(&output, "old").unwrap();
        assert!(matches!(
            in_folder(&scratch, &["real.takm", "--markers", "없는마커", "--json", "output.json"], &mut stdout),
            Err(SummaryError::Argument(message)) if message.contains("파일에 \"없는마커\" 마커가 없어요.")
        ));
        assert!(matches!(
            in_folder(
                &scratch,
                &["real.takm", "missing.tak", "--json", "output.json"],
                &mut stdout
            ),
            Err(SummaryError::Read(_))
        ));
        assert_eq!(fs::read_to_string(&output).unwrap(), "old");
        in_folder(
            &scratch,
            &["real.takm", "--json", "output.json"],
            &mut stdout,
        )
        .unwrap();
        assert!(stdout.is_empty());
        let json: Vec<Value> = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
        assert_eq!(json[0]["file"]["name"], "real.takm");
    }

    #[test]
    fn refuses_protected_destinations_and_replaces_a_hard_link_safely() {
        let scratch = TempDir::new("summary_refusals");
        let real = scratch.join("real.takm");
        let baseline = write_takm(&real, "source.tak");
        let input = scratch.join("input");
        fs::create_dir(&input).unwrap();
        fs::create_dir(scratch.join("apps")).unwrap();
        fs::write(scratch.join("AGENTS.md"), "").unwrap();
        fs::write(scratch.join("apps/AGENTS.md"), "").unwrap();
        let mut stdout = Vec::new();
        for protected in [
            "overwrite.TAK",
            "real.takm",
            "new.TAKM",
            "input",
            "input/deep/file.json",
        ] {
            assert!(
                matches!(
                    in_folder(&scratch, &["real.takm", "--json", protected], &mut stdout),
                    Err(SummaryError::Argument(_))
                ),
                "{protected}"
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real, scratch.join("alias.json")).unwrap();
            std::os::unix::fs::symlink(input, scratch.join("alias-folder")).unwrap();
            for protected in ["alias.json", "alias-folder/out.json"] {
                assert!(
                    matches!(
                        in_folder(&scratch, &["real.takm", "--json", protected], &mut stdout),
                        Err(SummaryError::Argument(_))
                    ),
                    "{protected}"
                );
            }
        }
        let link = scratch.join("linked.json");
        fs::hard_link(&real, &link).unwrap();
        in_folder(
            &scratch,
            &["real.takm", "--json", "linked.json"],
            &mut stdout,
        )
        .unwrap();
        assert_eq!(fs::read(real).unwrap(), baseline);
        assert!(fs::read_to_string(link).unwrap().starts_with('['));
        assert!(stdout.is_empty());
    }
}
