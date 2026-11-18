//! Headless take check. Capture runs use the same CLI path and artifacts as standalone captures.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tak_core::comparison::{
    self, AutoFrames, Comparison, FrameCoverage, FrameReason, KindCounts, PickedFrame, SampleStats,
    SlotComparison, StructuralRole,
};
use tak_core::summary::{self, FrameRange, SummaryOptions, TakeSummary};
use tak_core::{tak, takm};

use crate::capture::{self, FrameSpec, ViewPreset, MAX_FRAMES, MAX_SHOTS};
use crate::workspace;

pub const USAGE: &str = "\
사용법: tak_viewer --check <file.tak|file.takm> [<file2> … 최대 4개] --out <폴더>
  [--roles raw,copy,agent,…] [--frames <캡처 범위>|auto[:N]] [--views persp,top,side,front]
  [--select A,B] [--top N] [--no-pictures]
믿을 기준 파일을 먼저 써 주세요. 라벨링한 복사본이 없으면 원본을 써 주세요.
마커 세트는 같은 slot끼리, 같은 궤적의 이동과 라벨 변경은 UID끼리 비교해요.
기본 프레임은 auto:12, 기본 --top은 10이에요.
창 없이 숫자와 그림을 저장해요. --out은 새 폴더나 빈 폴더로 정해 주세요.";

const UNIT_NOTE: &str = "coordinate unit unverified";

#[derive(Debug)]
enum Frames {
    Auto(usize),
    Explicit(FrameSpec),
}

#[derive(Debug)]
struct Input {
    path: PathBuf,
    role: String,
    folder: String,
}

struct Options {
    inputs: Vec<Input>,
    out: PathBuf,
    frames: Frames,
    views_arg: Option<String>,
    views: Vec<ViewPreset>,
    select: Vec<String>,
    top: usize,
    pictures: bool,
}

#[derive(Debug)]
enum Error {
    Argument(String),
    Output(String),
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut args = args.iter().map(String::as_str).peekable();
    let mut paths = Vec::new();
    while let Some(path) = args.next_if(|p| !p.starts_with("--")) {
        let resolved = workspace::resolve_path(path);
        if !tak::is_take_path(&resolved) && !takm::is_takm_path(&resolved) {
            return Err(format!(".tak 또는 .takm 파일을 지정해 주세요. ({path})"));
        }
        paths.push(resolved);
    }
    if !(1..=4).contains(&paths.len()) {
        return Err("확인할 파일을 1개부터 4개까지 지정해 주세요.".into());
    }

    let mut flags = HashSet::new();
    let mut out = None;
    let mut roles = None;
    let mut frames = Frames::Auto(12);
    let mut views_arg = None;
    let mut select = Vec::new();
    let mut top = 10;
    let mut pictures = true;
    while let Some(flag) = args.next() {
        if !flags.insert(flag) {
            return Err(format!("{flag}는 한 번만 쓸 수 있어요."));
        }
        let mut value = || {
            args.next_if(|v| !v.starts_with("--"))
                .ok_or_else(|| format!("{flag} 뒤에 값을 써 주세요."))
        };
        match flag {
            "--out" => {
                let path = workspace::resolve_path(value()?);
                if path.as_os_str().is_empty() {
                    return Err("--out 뒤에 저장할 폴더를 써 주세요.".into());
                }
                out = Some(path);
            }
            "--roles" => {
                let supplied: Vec<_> = value()?.split(',').map(str::trim).collect();
                if supplied.len() != paths.len() || supplied.contains(&"") {
                    return Err("--roles에 파일마다 이름 하나를 쉼표로 나눠 써 주세요.".into());
                }
                roles = Some(supplied);
            }
            "--frames" => {
                let spec = value()?.trim();
                frames = if spec == "auto" {
                    Frames::Auto(12)
                } else if let Some(n) = spec.strip_prefix("auto:") {
                    let n = n.parse().ok().filter(|n| (1..=MAX_FRAMES).contains(n));
                    Frames::Auto(n.ok_or_else(|| {
                        format!("auto: 뒤에 1부터 {MAX_FRAMES}까지의 프레임 수를 써 주세요.")
                    })?)
                } else {
                    Frames::Explicit(FrameSpec::parse(spec)?)
                };
            }
            "--views" => views_arg = Some(value()?),
            "--select" => select = capture::parse_selection(value()?),
            "--top" => {
                top = value()?
                    .parse()
                    .map_err(|_| "--top 뒤에 0 이상의 정수를 써 주세요.")?
            }
            "--no-pictures" => pictures = false,
            _ => return Err(format!("알 수 없는 확인 옵션이에요. ({flag})")),
        }
    }
    let out = out.ok_or("--out으로 저장할 폴더를 지정해 주세요.")?;
    let views = views_arg
        .map(capture::parse_views)
        .unwrap_or_else(|| Ok(capture::DEFAULT_VIEWS.to_vec()))?;
    if views.iter().map(|v| v.key()).collect::<HashSet<_>>().len() != views.len() {
        return Err("--views에 같은 각도를 두 번 쓸 수 없어요.".into());
    }
    let mut folders = HashSet::new();
    let inputs = paths
        .into_iter()
        .enumerate()
        .map(|(i, path)| {
            let role = roles
                .as_ref()
                .map_or_else(|| format!("file{}", i + 1), |r| r[i].to_owned());
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let base = safe_folder(&format!("{role}_{stem}"));
            let mut folder = base.clone();
            for suffix in 2.. {
                if folders.insert(folder.to_lowercase()) {
                    break;
                }
                folder = format!("{base}_{suffix}");
            }
            Input { path, role, folder }
        })
        .collect();
    Ok(Options {
        inputs,
        out,
        frames,
        views_arg: views_arg.map(str::to_owned),
        views,
        select,
        top,
        pictures,
    })
}

fn safe_folder(name: &str) -> String {
    let mut safe: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let len = safe.trim_end_matches(['.', ' ']).len();
    safe.truncate(len);
    let device = safe
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let numbered_device = ["COM", "LPT"].iter().any(|p| {
        device.strip_prefix(*p).is_some_and(|s| {
            ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"].contains(&s)
        })
    });
    if numbered_device || matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        safe.insert(0, '_');
    }
    safe
}

fn validate_output(path: &Path) -> Result<PathBuf, Error> {
    let actual = takm::canonical_target(path)
        .map_err(|e| Error::Output(format!("저장할 경로를 확인하지 못했어요. ({e})")))?;
    if [path, &actual]
        .iter()
        .any(|p| tak::is_take_path(p) || takm::is_takm_path(p))
    {
        return Err(Error::Argument(
            "출력 폴더 이름에 .tak나 .takm 확장자를 쓸 수 없어요.".into(),
        ));
    }
    if takm::in_input(path) || takm::in_input(&actual) {
        return Err(Error::Argument("input/ 폴더에는 저장할 수 없어요.".into()));
    }
    match fs::read_dir(&actual) {
        Ok(mut entries) => match entries.next() {
            Some(Ok(_)) => Err(Error::Argument("새 폴더나 빈 폴더를 지정해 주세요.".into())),
            Some(Err(e)) => Err(Error::Output(format!("저장할 폴더를 읽지 못했어요. ({e})"))),
            None => Ok(actual),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(actual),
        Err(e) if e.kind() == io::ErrorKind::NotADirectory => Err(Error::Argument(
            "파일 경로 대신 저장할 폴더를 지정해 주세요.".into(),
        )),
        Err(e) => Err(Error::Output(format!(
            "저장할 폴더를 확인하지 못했어요. ({e})"
        ))),
    }
}

#[derive(Serialize)]
struct Report {
    reference_role: String,
    coordinate_units: &'static str,
    frames: Vec<PickedFrame>,
    views: Vec<&'static str>,
    select: Vec<String>,
    pictures_requested: bool,
    files: Vec<FileReport>,
    comparisons: Vec<Pair>,
    complete: bool,
}

#[derive(Serialize)]
struct FileReport {
    path: PathBuf,
    role: String,
    summary: Option<TakeSummary>,
    frames: Vec<FrameCoverage>,
    pictures: Option<PictureAudit>,
    tak_unchanged: bool,
    errors: Vec<String>,
}

#[derive(Serialize)]
struct Pair {
    file_index: usize,
    role: String,
    slot_comparison: SlotComparison,
    #[serde(skip_serializing_if = "Option::is_none")]
    uid_error: Option<String>,
    #[serde(flatten)]
    comparison: Option<Comparison>,
}

#[derive(Serialize)]
struct PictureAudit {
    folder: String,
    expected_shots: usize,
    complete_shots: usize,
    notes: Vec<String>,
    tak_unchanged: Option<bool>,
}

#[derive(Deserialize)]
struct Manifest {
    tak_unchanged: bool,
    start_frame: i64,
    end_frame: i64,
    views: Vec<String>,
    select: Vec<String>,
    sheet: Option<String>,
    notes: Vec<String>,
    shots: Vec<Shot>,
}

#[derive(Deserialize)]
struct Shot {
    frame: i64,
    view: String,
    file: String,
}

fn nonempty_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file() && m.len() != 0)
}

fn audit_pictures(
    out: &Path,
    folder: &str,
    expected: &[FrameCoverage],
    inputs: &Options,
    bounds: FrameRange,
    errors: &mut Vec<String>,
) -> PictureAudit {
    let dir = out.join(folder);
    let mut audit = PictureAudit {
        folder: folder.into(),
        expected_shots: expected.len() * inputs.views.len(),
        complete_shots: 0,
        notes: Vec::new(),
        tak_unchanged: None,
    };
    let manifest_path = dir.join("captures.json");
    let manifest = File::open(manifest_path)
        .map(BufReader::new)
        .map_err(|e| e.to_string())
        .and_then(|reader| {
            serde_json::from_reader::<_, Manifest>(reader).map_err(|e| e.to_string())
        });
    let mut records = HashMap::new();
    match manifest {
        Ok(manifest) => {
            audit.tak_unchanged = Some(manifest.tak_unchanged);
            if !manifest.tak_unchanged {
                errors.push("캡처 중에 원본 파일의 크기나 수정 시간이 바뀌었어요.".into());
            }
            audit.notes = manifest.notes;
            errors.extend(audit.notes.iter().cloned());
            let expected_views: Vec<_> = inputs.views.iter().map(|v| v.key()).collect();
            if (manifest.start_frame, manifest.end_frame) != (bounds.first, bounds.last)
                || manifest.views != expected_views
                || manifest.select != inputs.select
            {
                errors
                    .push("captures.json의 범위, 각도 또는 마커 선택이 확인 요청과 달라요.".into());
            }
            if manifest.sheet.as_deref() != Some("sheet.png") {
                errors.push("captures.json에 sheet.png 기록이 없어요.".into());
            }
            for shot in manifest.shots {
                if records.insert((shot.frame, shot.view), shot.file).is_some() {
                    errors.push(format!(
                        "captures.json에 {} 프레임 기록이 겹쳐요.",
                        shot.frame
                    ));
                }
            }
        }
        Err(e) => errors.push(format!("captures.json을 읽지 못했어요. ({e})")),
    }
    if !nonempty_file(&dir.join("sheet.png")) {
        errors.push("sheet.png 파일이 없거나 비어 있어요.".into());
    }
    for frame in expected {
        for &view in &inputs.views {
            let filename = capture::shot_file_name(frame.frame, view);
            let found = records.remove(&(frame.frame, view.key().to_owned()));
            let recorded = found.as_deref() == Some(filename.as_str());
            let exists = nonempty_file(&dir.join(&filename));
            if recorded && exists {
                audit.complete_shots += 1;
            }
            if !exists {
                errors.push(format!("{filename} 파일이 없거나 비어 있어요."));
            } else if !recorded {
                errors.push(format!("captures.json에 {filename} 기록이 없거나 달라요."));
            }
        }
    }
    if !records.is_empty() {
        errors.push("captures.json에 요청하지 않은 장면이 있어요.".into());
    }
    audit
}

fn capture_pictures(
    input: &Input,
    frames: &[i64],
    options: &Options,
    out: &Path,
) -> Result<(), String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("뷰어 경로를 확인하지 못했어요. ({e})"))?;
    let frames = frames
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let selected = options.select.join(",");
    let mut command = Command::new(exe);
    command
        .arg("--capture")
        .arg(&input.path)
        .args([
            "--headless",
            "--frames",
            &frames,
            "--select",
            &selected,
            "--out",
        ])
        .arg(out.join(&input.folder));
    if let Some(views) = &options.views_arg {
        command.args(["--views", views]);
    }
    let child = command
        .output()
        .map_err(|e| format!("캡처를 시작하지 못했어요. ({e})"))?;
    if child.status.success() {
        Ok(())
    } else {
        Err(format!(
            "캡처를 마치지 못했어요. ({}) {}",
            child.status,
            String::from_utf8_lossy(&child.stderr).trim()
        ))
    }
}

fn execute(
    options: Options,
    render: impl Fn(&Input, &[i64], &Options, &Path) -> Result<(), String>,
) -> Result<(PathBuf, Report), Error> {
    let out = validate_output(&options.out)?;
    let summary_options = SummaryOptions {
        top: options.top,
        ..Default::default()
    };
    let before: Vec<_> = options
        .inputs
        .iter()
        .map(|input| capture::stat_key(&input.path))
        .collect();
    let mut loaded: Vec<_> = options
        .inputs
        .iter()
        .map(|input| {
            if input.path.is_dir() {
                return Err(summary::SummaryError::Read(
                    "폴더 대신 .tak 또는 .takm 파일을 지정해 주세요.".into(),
                ));
            }
            summary::read_summary_with_take(&input.path, &summary_options)
        })
        .collect();
    let range = loaded
        .iter()
        .filter_map(|result| result.as_ref().ok().map(|(_, s)| s.file.frames))
        .reduce(|a, b| FrameRange {
            first: a.first.max(b.first),
            last: a.last.min(b.last),
        })
        .filter(|r| r.first <= r.last);
    let mut auto = match (&options.frames, range) {
        (Frames::Auto(n), Some(range)) => Some(AutoFrames::new(
            range,
            (*n).min(MAX_SHOTS / options.views.len()),
        )),
        _ => None,
    };
    if let Some(auto) = &mut auto {
        for (take, _) in loaded.iter().filter_map(|result| result.as_ref().ok()) {
            auto.add_gaps(take);
        }
    }
    let mut comparisons = Vec::new();
    let mut comparison_errors = vec![Vec::new(); loaded.len()];
    if let Ok((reference, _)) = &loaded[0] {
        for (file_index, result) in loaded.iter().enumerate().skip(1) {
            if let Ok((other, _)) = result {
                let (slot_comparison, uid) =
                    comparison::compare_pair(reference, other, auto.as_mut(), options.top);
                let (comparison, uid_error) = match uid {
                    Ok(comparison) => (Some(comparison), None),
                    Err(error) => {
                        let index = if error.in_reference { 0 } else { file_index };
                        let message = format!(
                            "UID {}가 겹쳐 어느 마커끼리 비교할지 정할 수 없어요.",
                            tak::uid_hex(&error.uid)
                        );
                        add_comparison_error(&mut comparison_errors[index], message.clone());
                        (None, Some(message))
                    }
                };
                if let SlotComparison::Incomplete(result) = &slot_comparison {
                    for slot in result.slots.iter().filter(|slot| slot.samples.is_none()) {
                        let asset = &result.assets[slot.id.asset_index];
                        for (index, uids, asset_uid) in [
                            (0, &slot.reference_uids, &asset.reference_asset_uid_hex),
                            (file_index, &slot.other_uids, &asset.other_asset_uid_hex),
                        ] {
                            if uids.len() > 1 {
                                add_comparison_error(
                                    &mut comparison_errors[index],
                                    format!(
                                        "스켈레톤 {asset_uid}의 slot {}에 궤적이 여러 개 있어요.",
                                        slot.id.slot
                                    ),
                                );
                            }
                        }
                    }
                }
                comparisons.push(Pair {
                    file_index,
                    role: options.inputs[file_index].role.clone(),
                    slot_comparison,
                    uid_error,
                    comparison,
                });
            }
        }
    }
    let frames = match (auto, &options.frames, range) {
        (Some(auto), _, _) => auto.finish(),
        (_, Frames::Explicit(spec), Some(r)) => spec
            .resolve(r.first, r.last)
            .into_iter()
            .map(|frame| PickedFrame {
                frame,
                reason: FrameReason::Explicit,
            })
            .collect(),
        _ => Vec::new(),
    };
    if frames.len() * options.views.len() > MAX_SHOTS {
        return Err(Error::Argument(format!(
            "파일마다 최대 {MAX_SHOTS}장까지 찍을 수 있어요. 프레임이나 각도 수를 줄여 주세요."
        )));
    }
    if frames.is_empty() && matches!(options.frames, Frames::Explicit(_)) && range.is_some() {
        return Err(Error::Argument(
            "지정한 프레임이 파일들의 공통 범위에 없어요.".into(),
        ));
    }
    let frame_ids: Vec<_> = frames.iter().map(|pick| pick.frame).collect();
    let mut files: Vec<_> = options
        .inputs
        .iter()
        .zip(loaded.drain(..).zip(comparison_errors))
        .map(|(input, (loaded, errors))| {
            let mut report = FileReport {
                path: input.path.clone(),
                role: input.role.clone(),
                summary: None,
                frames: Vec::new(),
                pictures: None,
                tak_unchanged: false,
                errors,
            };
            match loaded {
                Ok((take, summary)) => {
                    report.frames = comparison::frame_coverage(&take, &frame_ids);
                    report.summary = Some(summary);
                }
                Err(error) => report.errors.push(error.to_string()),
            }
            report
        })
        .collect();

    // Claim the output before any capture. Concurrent checks cannot share a folder.
    let out = validate_output(&out)?;
    fs::create_dir_all(&out)
        .map_err(|e| Error::Output(format!("저장 폴더를 만들지 못했어요. ({e})")))?;
    let manifest = File::options()
        .write(true)
        .create_new(true)
        .open(out.join("check.json"))
        .map_err(|e| Error::Output(format!("check.json을 만들지 못했어요. ({e})")))?;
    for (input, file) in options.inputs.iter().zip(&mut files) {
        if let Some(summary) = &file.summary {
            if options.pictures {
                if frames.is_empty() {
                    file.errors.push(
                        "모든 파일에 공통으로 있는 프레임이 없어 그림을 찍지 못했어요.".into(),
                    );
                } else {
                    if let Err(error) = render(input, &frame_ids, &options, &out) {
                        file.errors.push(error);
                    }
                    file.pictures = Some(audit_pictures(
                        &out,
                        &input.folder,
                        &file.frames,
                        &options,
                        summary.file.frames,
                        &mut file.errors,
                    ));
                }
            }
        }
    }
    for ((input, file), before) in options.inputs.iter().zip(&mut files).zip(before) {
        file.tak_unchanged = before.is_some() && before == capture::stat_key(&input.path);
        if !file.tak_unchanged && file.summary.is_some() {
            file.errors
                .push("확인 중에 파일의 크기나 수정 시간이 바뀌었어요.".into());
        }
    }
    let complete = files.iter().all(|file| file.errors.is_empty());
    let report = Report {
        reference_role: options.inputs[0].role.clone(),
        coordinate_units: "mm by viewer convention (coordinate unit unverified)",
        frames,
        views: options.views.iter().map(|v| v.key()).collect(),
        select: options.select,
        pictures_requested: options.pictures,
        files,
        comparisons,
        complete,
    };
    let mut writer = BufWriter::new(manifest);
    serde_json::to_writer_pretty(&mut writer, &report)
        .map_err(|e| Error::Output(format!("check.json을 저장하지 못했어요. ({e})")))?;
    writer
        .write_all(b"\n")
        .and_then(|_| writer.flush())
        .map_err(|e| Error::Output(format!("check.json을 저장하지 못했어요. ({e})")))?;
    Ok((out, report))
}

fn add_comparison_error(errors: &mut Vec<String>, message: String) {
    if !errors.contains(&message) {
        errors.push(message);
    }
}

impl Report {
    fn exit_code(&self) -> i32 {
        i32::from(!self.complete)
    }
}

fn kind_counts(k: &KindCounts) -> String {
    format!(
        "{}/{}/{}/{}/{}",
        k.template, k.skeleton_extra, k.other_parent, k.unlabeled, k.unknown
    )
}

fn stats_text(s: &SampleStats) -> String {
    let max = s
        .max
        .map_or("—".into(), |p| format!("{:.2} mm @{}", p.mm, p.frame));
    let mean = s.mean_mm.map_or("—".into(), |mean| format!("{mean:.2} mm"));
    format!(
        "한쪽만 {}/{} · 공통 {} (거리 {}) · 최대 {max} · 평균 {mean} · 1/20 mm 초과 {}/{}",
        s.reference_only,
        s.other_only,
        s.both_present,
        s.compared_frames,
        s.above_1_mm,
        s.above_20_mm
    )
}

fn reason_text(reason: FrameReason) -> &'static str {
    match reason {
        FrameReason::Explicit => "직접 지정",
        FrameReason::GapEdge => "빈 구간 경계",
        FrameReason::LargestDistance => "거리 차이",
        FrameReason::PresenceStart => "한쪽만 있는 구간 시작",
        FrameReason::PresenceMiddle => "한쪽만 있는 구간 중간",
        FrameReason::PresenceEnd => "한쪽만 있는 구간 끝",
        FrameReason::Control => "고른 간격",
    }
}

fn short_list<T>(items: &[T], limit: usize, label: impl Fn(&T) -> String) -> String {
    let mut parts: Vec<_> = items.iter().take(limit).map(label).collect();
    if items.len() > limit {
        parts.push(format!("외 {}개", items.len() - limit));
    }
    if parts.is_empty() {
        "—".into()
    } else {
        parts.join(", ")
    }
}

fn structural_role_text(role: &StructuralRole) -> String {
    let kind = match role.kind {
        "template" => "마커 세트",
        "skeleton_extra" => "스켈레톤 추가",
        "other_parent" => "기타 소속",
        "unlabeled" => "라벨 없음",
        _ => "구조 미확인",
    };
    role.slot
        .map_or_else(|| kind.into(), |slot| format!("{kind} slot {slot}"))
}

fn text(out: &Path, report: &Report) {
    println!("파일 확인 · 기준 {:?} · {UNIT_NOTE}", report.reference_role);
    for file in &report.files {
        if let Some(summary) = &file.summary {
            let f = &summary.file;
            let k = &summary.markers;
            let rate = f.frame_rate.map_or("—".into(), |v| v.to_string());
            let progress = match f.progress.value {
                Some(1) => "녹화됨",
                Some(4) => "라벨링 완료",
                _ => "진행 상태 알 수 없음",
            };
            println!("{:?} {:?} · {}바이트 · {} · {} Hz · {progress} · 마커 세트 {} / 추가 {} / 라벨 없음 {} / 구조 미확인 {}",
                file.role, f.name, f.size_bytes, f.frames, rate, k.template, k.extra, k.unlabeled, k.unknown);
            let pics = match &file.pictures {
                Some(p) => format!("그림 {}/{}", p.complete_shots, p.expected_shots),
                None if report.pictures_requested => "그림 없음".into(),
                None => "숫자만".into(),
            };
            println!("  {pics} · 원본 크기/수정 시간 {} · 가장 적은 프레임 {}/{} · 빈 칸 {} · 힘판 {} / 장치 {} · 읽기 참고 {}건",
                if file.tak_unchanged { "같음" } else { "바뀜" }, summary.coverage.min_present,
                summary.coverage.marker_total, summary.coverage.missing_cells, summary.devices.force_plate_count,
                summary.devices.analog_device_count, summary.warnings.len());
            if let Some(takm) = &summary.takm {
                let seal = if takm.seal_ok {
                    "정상"
                } else {
                    "확인 실패"
                };
                println!(
                    "  봉인 {seal} · Comment {}개 · 원본 {:?}",
                    takm.comment_count, takm.source_name
                );
            }
        }
        if !file.errors.is_empty() {
            println!(
                "  {:?} 확인 실패 · {}",
                file.role,
                file.errors.join(" / ").replace(['\r', '\n'], " ")
            );
        }
    }
    for pair in &report.comparisons {
        println!("{:?} 비교", pair.role);
        match &pair.slot_comparison {
            SlotComparison::Skipped { reason } => {
                println!("  마커 세트가 달라 slot 비교 안 함 · {}", reason.text());
            }
            SlotComparison::Compared(result) | SlotComparison::Incomplete(result) => {
                let count = result
                    .slots
                    .iter()
                    .filter(|row| row.samples.is_none())
                    .count();
                let missing = if count != 0 {
                    format!(" · 겹친 slot {count}개 제외")
                } else {
                    String::new()
                };
                println!(
                    "  마커 세트(slot) · {}개{missing} · {}",
                    result.slots.len(),
                    stats_text(&result.totals)
                );
                println!(
                    "  slot 최대 거리 순 · {}",
                    short_list(
                        &result.slots[..result.top_distance_slots.len()],
                        10,
                        |row| {
                            let peak = row.samples.as_ref().and_then(|s| s.max).unwrap();
                            format!(
                                "{}:{:02} {:?} {:.2} mm @{}",
                                row.id.asset_index + 1,
                                row.id.slot,
                                row.reference_name,
                                peak.mm,
                                peak.frame
                            )
                        }
                    )
                );
                if !result.name_differences.is_empty() {
                    println!(
                        "  같은 slot의 이름 차이 {}건 · {}",
                        result.name_differences.len(),
                        short_list(&result.name_differences, 10, |row| {
                            format!(
                                "{}:{:02} {:?} → {:?}",
                                row.id.asset_index + 1,
                                row.id.slot,
                                row.reference_name,
                                row.other_name
                            )
                        })
                    );
                }
            }
        }
        let Some(c) = &pair.comparison else {
            println!("  같은 궤적(UID) · 비교 미완료");
            continue;
        };
        let equality = |same| if same { "같음" } else { "다름" };
        println!(
            "  같은 궤적(UID) · 범위 {} · 속도 {} · 마커 세트 {} / {}",
            equality(c.frame_range_equal),
            equality(c.frame_rate_equal),
            c.reference_template,
            c.other_template
        );
        println!("  한 파일에만 있는 UID (세트/스켈레톤 추가/기타 부모/미라벨/미확인) · 기준 {} · 비교 {}",
            kind_counts(&c.reference_only), kind_counts(&c.other_only));
        println!(
            "  같은 UID의 이름 차이 {}건 · {}",
            c.rename_count,
            short_list(&c.renames, 10, |r| format!(
                "{:?} → {:?}",
                r.reference_name, r.other_name
            ))
        );
        if c.role_change_count > 0 {
            println!(
                "  같은 UID의 마커 소속·slot 변경 {}건 · {}",
                c.role_change_count,
                short_list(&c.role_changes, 4, |r| {
                    let parent = if r.reference.parent_uid_hex != r.other.parent_uid_hex {
                        " · 부모 변경"
                    } else {
                        ""
                    };
                    format!(
                        "{:?} {} → {:?} {}{parent}",
                        r.reference_name,
                        structural_role_text(&r.reference),
                        r.other_name,
                        structural_role_text(&r.other)
                    )
                })
            );
        }
        println!(
            "  다른 UID의 같은 이름 {}건 · {}",
            c.shared_name_count,
            short_list(&c.shared_names, 10, |n| format!("{:?}", n.name))
        );
        println!(
            "  세트 관련 UID 합계 (이름·소속 변경 포함) · {}",
            stats_text(&c.marker_set_totals)
        );
        println!(
            "  UID 최대 이동 순 · {}",
            short_list(&c.marker_set[..c.top_distance_uids.len()], 10, |m| {
                let peak = m.samples.max.unwrap();
                let mut labels = Vec::new();
                if m.reference_name != m.other_name {
                    labels.push("이름 변경");
                }
                if m.structural_role_changed {
                    labels.push("소속·slot 변경");
                }
                let label = if labels.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", labels.join(", "))
                };
                format!("{:?} {:.2} mm @{}{label}", m.name(), peak.mm, peak.frame)
            })
        );
        println!(
            "  세트 밖 마커 · 기준 {} · 비교 {} · {}",
            kind_counts(&c.extra_markers.reference),
            kind_counts(&c.extra_markers.other),
            stats_text(&c.extra_markers.samples)
        );
    }
    if !report.frames.is_empty() {
        println!("프레임 선택과 마커 세트 수 (보임/전체)");
    }
    for (i, picked) in report.frames.iter().enumerate() {
        let values: Vec<_> = report
            .files
            .iter()
            .filter_map(|file| {
                file.frames.get(i).map(|f| {
                    format!(
                        "{:?} {}/{}{}",
                        file.role,
                        f.template_present,
                        f.template_total,
                        if f.missing_template.is_empty() {
                            String::new()
                        } else {
                            format!(
                                " (빈 마커 {})",
                                short_list(&f.missing_template, 2, |n| format!("{n:?}"))
                            )
                        }
                    )
                })
            })
            .collect();
        println!(
            "  {} · {} · {}",
            picked.frame,
            reason_text(picked.reason),
            values.join(" / ")
        );
    }
    println!(
        "{} · {}",
        if report.complete {
            "확인 완료"
        } else {
            "확인 미완료"
        },
        out.join("check.json").display()
    );
}

pub fn run(args: &[String]) -> i32 {
    if matches!(args, [help] if help == "--help" || help == "-h") {
        println!("{USAGE}");
        return 0;
    }
    match parse(args)
        .map_err(Error::Argument)
        .and_then(|options| execute(options, capture_pictures))
    {
        Ok((out, report)) => {
            text(&out, &report);
            report.exit_code()
        }
        Err(Error::Argument(error)) => {
            eprintln!("{error}\n{USAGE}");
            2
        }
        Err(Error::Output(error)) => {
            eprintln!("{error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tak_core::sample_tak::sample_tak_with_devices;
    use tak_core::test_support::TempDir;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn parses_capture_options_and_rejects_bad_arguments_as_exit_two() {
        for bad in [
            &["raw.tak"][..],
            &["raw.tak", "copy.takm", "--out", "dst", "--roles", "raw"][..],
            &["raw.tak", "--out", "dst", "--roles", ""][..],
            &["raw.tak", "--out", "dst", "--frames", "auto:0"][..],
            &["raw.tak", "--out", "dst", "--frames", "auto:61"][..],
            &["raw.tak", "--out", "dst", "--views", "top,top"][..],
            &["raw.tak", "--out", "dst", "--views", "sky"][..],
            &["raw.tak", "--out", "dst", "--top", "-1"][..],
            &["raw.tak", "--out", "dst", "--out", "other"][..],
            &["raw.tak", "--out", "dst", "--headless"][..],
            &[
                "raw.tak", "b.tak", "c.tak", "d.tak", "e.tak", "--out", "dst",
            ][..],
        ] {
            assert_eq!(run(&args(bad)), 2, "{bad:?}");
        }
        let options = parse(&args(&[
            "raw.tak",
            "copy.takm",
            "--out",
            "dst",
            "--roles",
            "Raw take,copy",
            "--frames",
            "3:15:3",
            "--views",
            "all",
            "--select",
            "LKNE, RKNE",
            "--top",
            "0",
            "--no-pictures",
        ]))
        .unwrap();
        assert!(matches!(
            options.frames,
            Frames::Explicit(FrameSpec::Range(3, 15, 3))
        ));
        assert_eq!(options.inputs[0].folder, "Raw take_raw");
        assert_eq!(options.inputs[1].role, "copy");
        assert_eq!(options.views.len(), 5);
        assert_eq!(options.select, ["LKNE", "RKNE"]);
        assert_eq!(options.top, 0);
        assert!(!options.pictures);
        let duplicate = parse(&args(&[
            "a.tak", "a.tak", "--out", "dst", "--roles", "x/y,X_Y",
        ]))
        .unwrap();
        assert_eq!(duplicate.inputs[0].folder, "x_y_a");
        assert_eq!(duplicate.inputs[1].folder, "X_Y_a_2");
    }

    #[test]
    fn refuses_input_take_paths_and_nonempty_folders() {
        let temp = TempDir::new("check_out");
        let root = temp.join("fake-repo");
        let input = root.join("input");
        let nonempty = root.join("nonempty");
        let empty = root.join("empty");
        for path in [&input, &nonempty, &empty, &root.join("apps")] {
            fs::create_dir_all(path).unwrap();
        }
        for file in [
            root.join("AGENTS.md"),
            root.join("apps/AGENTS.md"),
            nonempty.join(".keep"),
        ] {
            fs::write(file, "").unwrap();
        }
        let refusals = [
            input.clone(),
            input.join("nested/result"),
            root.join("other.TAK"),
            root.join("other.TaKm"),
            nonempty,
            root.join("AGENTS.md"),
        ];
        for path in refusals {
            assert_eq!(run(&args(&["a.tak", "--out", &path.to_string_lossy()])), 2);
        }
        assert!(validate_output(&empty).is_ok());
        assert!(validate_output(&root.join("new")).is_ok());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&input, root.join("alias")).unwrap();
            assert!(matches!(
                validate_output(&root.join("alias/output")),
                Err(Error::Argument(_))
            ));
        }
    }

    #[test]
    fn missing_png_is_listed_and_fails_even_when_capture_exit_succeeds() {
        let temp = TempDir::new("check_missing_png");
        let source = temp.join("source.tak");
        fs::write(&source, sample_tak_with_devices()).unwrap();
        let out = temp.join("out");
        let options = parse(&args(&[
            &source.to_string_lossy(),
            "--out",
            &out.to_string_lossy(),
            "--frames",
            "2",
            "--views",
            "top,front",
        ]))
        .unwrap();
        let (_, report) = execute(options, |input, _, _, output| {
            let dir = output.join(&input.folder);
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("sheet.png"), "sheet").unwrap();
            fs::write(dir.join("f000002_front.png"), "shot").unwrap();
            let manifest = json!({
                "tak_unchanged": true, "start_frame": 0, "end_frame": 299,
                "views": ["top", "front"], "select": [], "sheet": "sheet.png", "notes": [],
                "shots": [
                    {"frame": 2, "view": "top", "file": "f000002_top.png"},
                    {"frame": 2, "view": "front", "file": "f000002_front.png"},
                ],
            });
            fs::write(
                dir.join("captures.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(report.exit_code(), 1);
        let saved: Value =
            serde_json::from_slice(&fs::read(out.join("check.json")).unwrap()).unwrap();
        assert_eq!(saved["complete"], false);
        let file = &saved["files"][0];
        assert_eq!(file["pictures"]["expected_shots"], 2);
        assert_eq!(file["pictures"]["complete_shots"], 1);
        assert_eq!(
            file["errors"][0],
            "f000002_top.png 파일이 없거나 비어 있어요."
        );
    }

    #[test]
    fn numbers_only_keeps_summary_and_frame_facts_and_returns_success() {
        let temp = TempDir::new("check_numbers");
        let source = temp.join("source.tak");
        fs::write(&source, sample_tak_with_devices()).unwrap();
        let out = temp.join("out");
        fs::create_dir(&out).unwrap();
        let options = parse(&args(&[
            &source.to_string_lossy(),
            &source.to_string_lossy(),
            "--out",
            &out.to_string_lossy(),
            "--frames",
            "auto:6",
            "--no-pictures",
        ]))
        .unwrap();
        let (_, report) = execute(options, |_, _, _, _| panic!("numbers only rendered")).unwrap();
        assert_eq!(report.exit_code(), 0);
        assert!(report
            .frames
            .iter()
            .any(|p| p.reason == FrameReason::GapEdge));
        assert!(report.files.iter().all(|f| f.tak_unchanged
            && f.summary.is_some()
            && f.pictures.is_none()
            && f.frames.len() == report.frames.len()));
        assert_eq!(
            report.comparisons[0]
                .comparison
                .as_ref()
                .unwrap()
                .marker_set_totals
                .mean_mm,
            Some(0.0)
        );
        assert_eq!(fs::read_dir(out).unwrap().count(), 1);
    }

    #[test]
    fn explicit_frame_and_view_product_must_fit_the_shot_limit() {
        let temp = TempDir::new("check_limits");
        let source = temp.join("source.tak");
        fs::write(&source, sample_tak_with_devices()).unwrap();
        let out = temp.join("out");
        let args = args(&[
            &source.to_string_lossy(),
            "--out",
            &out.to_string_lossy(),
            "--frames",
            "0:31",
        ]);
        assert_eq!(run(&args), 2);
        assert!(!out.exists());
    }
}
