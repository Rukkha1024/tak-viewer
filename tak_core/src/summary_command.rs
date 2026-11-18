//! Headless command options, text formatting and JSON export for `--summary`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::cli_json::{replace_file, Envelope};
use crate::summary::{
    self, Coordinates, Coverage, FrameRange, MarkerGaps, SummaryError, SummaryOptions, TakeSummary,
};
use crate::{tak, takm};

pub const USAGE: &str =
    "Usage: tak_viewer --summary <file.tak|file.takm> [<file2> …] [--markers A,B,…] [--frames a:b] [--top N] [--json [out.json] [--envelope]]
         Positions: tak_viewer --summary <file> --xyz A,B [--frames a:b|n,m,…] [--json [out.json] [--envelope]]";

#[derive(Debug)]
struct Command<'a> {
    files: Vec<&'a str>,
    options: SummaryOptions,
    json: Option<Option<&'a str>>,
    envelope: bool,
    xyz: Option<Vec<String>>,
    frame_list: Option<Vec<i64>>,
}

const MAX_NAMES: usize = 8;
const MAX_FRAMES: usize = 500;
const FRAME_CAP: &str =
    "좌표는 한 번에 500프레임까지 볼 수 있어요. 범위를 줄이거나 --dump 또는 --capture를 써 주세요.";

fn check_xyz_range(range: FrameRange) -> Result<(), SummaryError> {
    if i128::from(range.last) - i128::from(range.first) >= MAX_FRAMES as i128 {
        return Err(SummaryError::Argument(FRAME_CAP.into()));
    }
    Ok(())
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
    let mut envelope = false;
    let mut xyz = None;
    let mut frames = None;
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
            "--xyz" if xyz.is_none() => {
                let names: Vec<_> = value()?
                    .split(',')
                    .map(str::trim)
                    .map(str::to_owned)
                    .collect();
                if names.iter().any(String::is_empty) {
                    return Err(SummaryError::Argument(
                        "--xyz에 마커 이름을 쉼표로 나눠 써 주세요.".into(),
                    ));
                }
                if names.len() > MAX_NAMES {
                    return Err(SummaryError::Argument(
                        "좌표는 한 번에 마커 이름 8개까지 볼 수 있어요. --dump 또는 --capture를 써 주세요.".into(),
                    ));
                }
                let mut seen = std::collections::HashSet::new();
                xyz = Some(
                    names
                        .into_iter()
                        .filter(|name| seen.insert(name.clone()))
                        .collect(),
                );
            }
            "--frames" if frames.is_none() => frames = Some(value()?),
            "--envelope" if !envelope => envelope = true,
            "--top" if top.is_none() => {
                top = Some(value()?.parse().map_err(|_| {
                    SummaryError::Argument("--top 뒤에 0 이상의 정수를 써 주세요.".into())
                })?);
            }
            "--json" if json.is_none() => json = Some(args.next_if(|s| !s.starts_with('-'))),
            "--json" | "--top" | "--frames" | "--markers" | "--xyz" | "--envelope" => {
                return Err(SummaryError::Argument(format!(
                    "{flag}는 한 번만 쓸 수 있어요."
                )));
            }
            _ => {
                return Err(SummaryError::Argument(format!(
                    "알 수 없는 요약 옵션이에요. ({flag})"
                )));
            }
        }
    }
    if envelope && json.is_none() {
        return Err(SummaryError::Argument(
            "--envelope는 --json과 함께 써 주세요.".into(),
        ));
    }
    if xyz.is_some() && files.len() != 1 {
        return Err(SummaryError::Argument(
            "--xyz는 한 번에 파일 하나에 써 주세요.".into(),
        ));
    }
    let mut frame_list = None;
    if let Some(frames) = frames {
        if xyz.is_some() && !frames.contains(':') {
            let mut selected = frames
                .split(',')
                .take(MAX_FRAMES + 1)
                .map(|frame| frame.trim().parse::<i64>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| {
                    SummaryError::Argument("--frames에 프레임 번호를 쉼표로 나눠 써 주세요.".into())
                })?;
            if selected.len() > MAX_FRAMES {
                return Err(SummaryError::Argument(FRAME_CAP.into()));
            }
            selected.sort_unstable();
            selected.dedup();
            frame_list = Some(selected);
        } else {
            let range = frames
                .split_once(':')
                .and_then(|(a, b)| {
                    Some(FrameRange {
                        first: a.parse().ok()?,
                        last: b.parse().ok()?,
                    })
                })
                .filter(|range| range.first <= range.last)
                .ok_or_else(|| {
                    SummaryError::Argument(
                        "--frames는 시작:끝 형식으로 써 주세요. 시작은 끝보다 클 수 없어요.".into(),
                    )
                })?;
            if xyz.is_some() {
                check_xyz_range(range)?;
            }
            options.frames = Some(range);
        }
    }
    options.top = top.unwrap_or(options.top);
    Ok(Command {
        files,
        options,
        json,
        envelope,
        xyz,
        frame_list,
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
            let read = || {
                let Some(names) = &command.xyz else {
                    return summary::read_summary(&path, &command.options);
                };
                let (take, mut summary) = summary::read_summary_with_take(&path, &command.options)?;
                let bounds = summary.file.frames;
                let frames = if let Some(frames) = &command.frame_list {
                    if let Some(frame) = frames
                        .iter()
                        .find(|&&f| f < bounds.first || f > bounds.last)
                    {
                        return Err(SummaryError::Argument(format!(
                            "프레임 {frame}은 파일 범위 {bounds} 밖이에요."
                        )));
                    }
                    frames.clone()
                } else {
                    let range = command.options.frames.unwrap_or(bounds);
                    check_xyz_range(range)?;
                    (range.first..=range.last).collect()
                };
                summary.xyz = Some(Coordinates::read(&take, names, &frames)?);
                Ok(summary)
            };
            read().map_err(|error| match error {
                SummaryError::Argument(message) => {
                    SummaryError::Argument(format!("{file} {message}"))
                }
                SummaryError::Read(message) => SummaryError::Read(format!("{file} {message}")),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let write = if command.json.is_some() {
        let mut json = if command.envelope {
            serde_json::to_vec_pretty(&Envelope::new(1, summaries))
        } else {
            serde_json::to_vec_pretty(&summaries)
        }
        .map_err(|e| SummaryError::Read(format!("요약을 JSON으로 만들지 못했어요. ({e})")))?;
        json.push(b'\n');
        match destination {
            Some(path) => replace_file(&path, &json),
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
        .unwrap_or_else(|| "Unknown".into())
}

fn text(out: &mut impl Write, summary: &TakeSummary) -> io::Result<()> {
    let file = &summary.file;
    let progress = &file.progress;
    let progress_label = match progress.value {
        Some(1) => "Recorded",
        Some(4) => "Labeled",
        _ => "Unknown",
    };
    writeln!(
        out,
        "{} ({}, {} bytes)",
        file.name, file.kind, file.size_bytes
    )?;
    writeln!(
        out,
        "  Progress {} · {progress_label}",
        value_or_unknown(progress.value)
    )?;
    writeln!(
        out,
        "  Frames {} ({}) · {} Hz · {} s",
        file.frames,
        file.frame_count,
        value_or_unknown(file.frame_rate),
        value_or_unknown(file.duration_seconds.map(|secs| format!("{secs:.3}")))
    )?;
    if let Some(takm) = &summary.takm {
        let seal = if takm.seal_ok {
            "OK"
        } else {
            "Check failed"
        };
        writeln!(
            out,
            "  Seal {seal} · Comments {} · Source {}",
            takm.comment_count, takm.source_name
        )?;
    }
    let markers = &summary.markers;
    writeln!(
        out,
        "  Marker set {} · Extra {} · No label {} · Unknown kind {}",
        markers.template, markers.extra, markers.unlabeled, markers.unknown
    )?;
    if !markers.assets.is_empty() {
        let assets: Vec<_> = markers
            .assets
            .iter()
            .map(|asset| match &asset.markerset_name {
                Some(set) => format!("{} ({set}) {}", asset.name, asset.template_count),
                None => format!("{} {}", asset.name, asset.template_count),
            })
            .collect();
        writeln!(out, "  By marker set {}", assets.join(" · "))?;
    }
    coverage_text(out, "All frames", &summary.coverage)?;
    if let Some(window) = &summary.window_coverage {
        coverage_text(out, "Picked frames", window)?;
    }
    let gaps = &summary.gaps;
    gap_text(out, "Marker set gaps", &gaps.template, gaps.template_more)?;
    gap_text(out, "Extra marker gaps", &gaps.extra, gaps.extra_more)?;
    if !gaps.other.is_empty() {
        gap_text(out, "Other picked marker gaps", &gaps.other, 0)?;
    }
    let devices = &summary.devices;
    let analog: Vec<_> = devices
        .analog
        .iter()
        .enumerate()
        .map(|(i, device)| {
            let name = device.name.clone().unwrap_or_else(|| format!("#{}", i + 1));
            // Short text: channels sharing one rate collapse to "N samples per frame"; JSON keeps every channel.
            let mut rates: Vec<(String, usize)> = Vec::new();
            for channel in &device.channels {
                let rate = value_or_unknown(channel.samples_per_frame);
                match rates.iter_mut().find(|(known, _)| *known == rate) {
                    Some((_, count)) => *count += 1,
                    None => rates.push((rate, 1)),
                }
            }
            let rates = match rates.as_slice() {
                [] => "No channel info".to_owned(),
                [(rate, _)] => format!("{rate} samples per frame"),
                many => many
                    .iter()
                    .map(|(rate, count)| format!("{count} with {rate} samples per frame"))
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            format!("{name} (channels {} · {rates})", device.channel_count)
        })
        .collect();
    writeln!(
        out,
        "  Force plates {} · Analog devices {}",
        devices.force_plate_count, devices.analog_device_count
    )?;
    if !analog.is_empty() {
        writeln!(out, "  Analog {}", analog.join(" · "))?;
    }
    for warning in &summary.warnings {
        writeln!(out, "  Read note · {warning}")?;
    }
    if let Some(xyz) = &summary.xyz {
        writeln!(out, "  Saved positions · mm (unit not checked)")?;
        for track in &xyz.trajectories {
            let parent = track.parent_uid_hex.as_deref().unwrap_or("None");
            let slot = value_or_unknown(track.slot);
            writeln!(
                out,
                "    {} · {} · {} · parent {parent} · slot {slot}",
                track.name, track.uid_hex, track.kind
            )?;
            for sample in &track.samples {
                match sample.xyz_mm {
                    Some([x, y, z]) => {
                        writeln!(out, "      {} · {x:.2}, {y:.2}, {z:.2}", sample.frame)?
                    }
                    None => writeln!(out, "      {} · No sample", sample.frame)?,
                }
            }
        }
    }
    Ok(())
}

fn coverage_text(out: &mut impl Write, label: &str, stats: &Coverage) -> io::Result<()> {
    writeln!(
        out,
        "  {label} {} · Fewest seen {}/{} · Median {}/{} · All seen in {} frames · Missing cells {}",
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
        return writeln!(out, "  {label} · None");
    }
    writeln!(out, "  {label}")?;
    for row in rows {
        write!(
            out,
            "    {} · Gaps {} · Missing frames {}",
            row.name, row.gap_count, row.missing_frames
        )?;
        if let Some(gap) = &row.longest_gap {
            write!(out, " · Longest gap {} frames ({})", gap.length, gap.frames)?;
        }
        if let Some(ranges) = &row.ranges {
            let ranges: Vec<_> = ranges.iter().map(ToString::to_string).collect();
            if !ranges.is_empty() {
                write!(out, " · All ranges {}", ranges.join(", "))?;
            }
        }
        writeln!(out)?;
    }
    if more > 0 {
        writeln!(out, "    {more} more")?;
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
    use std::fs;

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
        assert!(warnings.contains("TAKM seal check failed") && warnings.contains("cut-off Comment"));
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
        assert!(text.contains("Fewest seen 1/2"));
        assert!(text.contains(
            "RKNE · Gaps 1 · Missing frames 40 · Longest gap 40 frames (100–139) · All ranges 100–139"
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
    #[test]
    fn xyz_samples_are_sparse_and_opt_in_with_a_legacy_array_or_envelope() {
        let scratch = TempDir::new("summary_xyz");
        write_takm(&scratch.join("a.takm"), "a.tak");
        let read = |extra: &[&str]| -> Value {
            let mut stdout = Vec::new();
            let words = ["a.takm", "--json"]
                .into_iter()
                .chain(extra.iter().copied())
                .collect::<Vec<_>>();
            in_folder(&scratch, &words, &mut stdout).unwrap();
            serde_json::from_slice(&stdout).unwrap()
        };
        let legacy = read(&[]);
        assert!(legacy[0].get("xyz").is_none());
        let envelope = read(&["--envelope"]);
        assert_eq!(envelope["items"], legacy);
        assert_eq!(envelope["schema"], 1);
        assert_eq!(envelope["producer"]["name"], "tak_viewer");

        let result = read(&["--frames", "100,99,99,140", "--xyz", "RKNE"]);
        let track = &result[0]["xyz"]["trajectories"][0];
        assert_eq!(track["kind"], "template");
        assert!(track["slot"].is_number() && track["parent_uid_hex"].is_string());
        assert_eq!(track["samples"][0]["frame"], 99);
        assert_eq!(track["samples"][0]["present"], true);
        assert!(track["samples"][0]["xyz_mm"].is_array());
        assert_eq!(
            track["samples"][1],
            serde_json::json!({"frame":100,"present":false,"xyz_mm":null})
        );
        assert_eq!(track["samples"][2]["frame"], 140);
        assert_eq!(
            read(&["--xyz", "RKNE"])[0]["xyz"]["trajectories"][0]["samples"]
                .as_array()
                .unwrap()
                .len(),
            300
        );
        assert_eq!(
            read(&["--xyz", "RKNE", "--frames", "99:100"])[0]["xyz"]["trajectories"][0]["samples"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let mut stdout = Vec::new();
        in_folder(
            &scratch,
            &["a.takm", "--xyz", "RKNE", "--frames", "100"],
            &mut stdout,
        )
        .unwrap();
        assert!(String::from_utf8(stdout)
            .unwrap()
            .contains("100 · No sample"));
        for frame in ["-1", "300", "299:300"] {
            assert!(in_folder(
                &scratch,
                &["a.takm", "--xyz", "RKNE", "--frames", frame],
                &mut Vec::new()
            )
            .is_err());
        }
    }

    #[test]
    fn xyz_limits_refuse_oversized_queries_before_reading() {
        for words in [
            &["a.tak", "--xyz", "a,b,c,d,e,f,g,h,i"][..],
            &["a.tak", "--xyz", "A", "--frames", "1:501"],
            &[
                "a.tak",
                "--xyz",
                "A",
                "--frames",
                "-9223372036854775808:9223372036854775807",
            ],
        ] {
            assert!(
                matches!(parse(&args(words)), Err(SummaryError::Argument(e)) if e.contains("--dump"))
            );
        }
        let list = (1..=501)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            matches!(parse(&args(&["a.tak", "--xyz", "A", "--frames", &list])), Err(SummaryError::Argument(e)) if e.contains("--dump"))
        );
        assert!(parse(&args(&[
            "a.tak",
            "--xyz",
            "a,b,c,d,e,f,g,h",
            "--frames",
            "1:500"
        ]))
        .is_ok());
        assert!(parse(&args(&[
            "a.tak",
            "--xyz",
            "A",
            "--frames",
            &list[..list.rfind(',').unwrap()]
        ]))
        .is_ok());
        for words in [
            &["a.tak", "--frames", "1,2"][..],
            &["a.tak", "--xyz", "A,"],
            &["a.tak", "--xyz", "A", "--xyz", "B"],
            &["a.tak", "--envelope"],
            &["a.tak", "b.tak", "--xyz", "A"],
        ] {
            assert!(parse(&args(words)).is_err(), "{words:?}");
        }
    }

    #[test]
    fn xyz_keeps_each_trajectory_with_a_matching_name_and_uses_capture_scaling() {
        let scratch = TempDir::new("summary_xyz_duplicates");
        let path = scratch.join("a.takm");
        write_takm(&path, "a.tak");
        let (mut take, _) =
            summary::read_summary_with_take(&path, &SummaryOptions::default()).unwrap();
        let mut duplicate = take
            .trajectories
            .iter()
            .find(|t| t.name == "RKNE")
            .unwrap()
            .clone();
        duplicate.uid[0] ^= 1;
        duplicate.slot = Some(-7);
        take.trajectories.push(duplicate);
        let xyz = Coordinates::read(&take, &["RKNE".to_owned()], &[99, 100]).unwrap();
        assert_eq!(xyz.trajectories.len(), 2);
        assert_eq!(xyz.trajectories[1].slot, Some(-7));
        assert_ne!(xyz.trajectories[0].uid_hex, xyz.trajectories[1].uid_hex);
        let data = crate::view_data::ViewData::build(take);
        let sample = data
            .markers
            .iter()
            .find(|m| m.name == "RKNE")
            .unwrap()
            .position(99)
            .unwrap();
        let expected = crate::view_data::capture_xyz_mm([sample.x, sample.y, sample.z]);
        assert_eq!(xyz.trajectories[0].samples[0].xyz_mm, Some(expected));
        assert!(xyz.trajectories[1].samples[1].xyz_mm.is_none());
    }
}
