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
Usage: tak_viewer --check <file.tak|file.takm> [<file2> … up to 4 files] --out <folder>
  [--roles raw,copy,agent,…] [--frames <frames>|auto[:N]] [--views persp,top,side,front]
  [--select A,B] [--top N] [--no-pictures]
  [--size WxH] [--fit frame|take|selected] [--trail N] [--no-names] [--no-sheet] [--ui]
Put the file you trust first. Use the labeled copy if you have one. Otherwise use the original.
Match marker sets by slot. Match tracks by UID to see moves and label changes.
Defaults are --frames auto:12 and --top 10.
Save numbers and pictures without opening a window. Use a new or empty folder for --out.";

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
    picture_args: Vec<String>,
    picture_options: PictureOptions,
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
    let mut picture_args = Vec::new();
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
            "--size" | "--fit" | "--trail" => {
                picture_args.extend([flag.to_owned(), value()?.to_owned()]);
            }
            "--no-names" | "--no-sheet" | "--ui" => picture_args.push(flag.to_owned()),
            _ => return Err(format!("알 수 없는 확인 옵션이에요. ({flag})")),
        }
    }
    let capture_args = ["--capture".to_owned(), "file.tak".to_owned()]
        .into_iter()
        .chain(picture_args.iter().cloned())
        .collect::<Vec<_>>();
    let capture_plan = capture::parse(&capture_args)?.expect("capture flag supplied");
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
        picture_args,
        picture_options: PictureOptions::from(&capture_plan),
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

#[derive(Debug, Serialize)]
struct PictureOptions {
    size: [f32; 2],
    fit: &'static str,
    trail: Option<i64>,
    names: bool,
    sheet: bool,
    ui: bool,
}

impl From<&capture::CapturePlan> for PictureOptions {
    fn from(plan: &capture::CapturePlan) -> Self {
        Self {
            size: plan.size,
            fit: match plan.fit {
                capture::Fit::Frame => "frame",
                capture::Fit::Take => "take",
                capture::Fit::Selected => "selected",
            },
            trail: plan.trail,
            names: plan.names,
            sheet: plan.sheet,
            ui: plan.full_ui,
        }
    }
}

#[derive(Serialize)]
struct Report {
    #[serde(flatten)]
    version: tak_core::cli_json::Version,
    picture_options: PictureOptions,
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
                errors.push("The source file size or time changed while taking pictures.".into());
            }
            audit.notes = manifest.notes;
            errors.extend(audit.notes.iter().cloned());
            let expected_views: Vec<_> = inputs.views.iter().map(|v| v.key()).collect();
            if (manifest.start_frame, manifest.end_frame) != (bounds.first, bounds.last)
                || manifest.views != expected_views
                || manifest.select != inputs.select
            {
                errors
                    .push("The frame range, views, or picked markers in captures.json differ from the request.".into());
            }
            if inputs.picture_options.sheet && manifest.sheet.as_deref() != Some("sheet.png") {
                errors.push("captures.json has no entry for sheet.png.".into());
            }
            if !inputs.picture_options.sheet && manifest.sheet.is_some() {
                errors.push("captures.json lists an extra sheet.png.".into());
            }
            for shot in manifest.shots {
                if records.insert((shot.frame, shot.view), shot.file).is_some() {
                    errors.push(format!(
                        "captures.json lists the same view of frame {} twice.",
                        shot.frame
                    ));
                }
            }
        }
        Err(e) => errors.push(format!("captures.json을 읽지 못했어요. ({e})")),
    }
    if inputs.picture_options.sheet && !nonempty_file(&dir.join("sheet.png")) {
        errors.push("sheet.png is missing or empty.".into());
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
                errors.push(format!("{filename} is missing or empty."));
            } else if !recorded {
                errors.push(format!("The entry for {filename} in captures.json is missing or different."));
            }
        }
    }
    if !records.is_empty() {
        errors.push("captures.json lists extra pictures.".into());
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
    command.args(&options.picture_args);
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
                .push("The file size or time changed during the check.".into());
        }
    }
    let complete = files.iter().all(|file| file.errors.is_empty());
    let report = Report {
        version: tak_core::cli_json::Version::new(1),
        picture_options: options.picture_options,
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
        "Only in one file {}/{} · In both {} (measured {}) · Max {max} · Mean {mean} · Over 1/20 mm {}/{}",
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
        FrameReason::Explicit => "Picked by you",
        FrameReason::GapEdge => "Gap edge",
        FrameReason::LargestDistance => "Largest distance",
        FrameReason::PresenceStart => "Start of one-file run",
        FrameReason::PresenceMiddle => "Middle of one-file run",
        FrameReason::PresenceEnd => "End of one-file run",
        FrameReason::Control => "Even spacing",
    }
}

fn short_list<T>(items: &[T], limit: usize, label: impl Fn(&T) -> String) -> String {
    let mut parts: Vec<_> = items.iter().take(limit).map(label).collect();
    if items.len() > limit {
        parts.push(format!("{} more", items.len() - limit));
    }
    if parts.is_empty() {
        "—".into()
    } else {
        parts.join(", ")
    }
}

fn structural_role_text(role: &StructuralRole) -> String {
    let kind = match role.kind {
        "template" => "Marker set",
        "skeleton_extra" => "Skeleton extra",
        "other_parent" => "Other parent",
        "unlabeled" => "No label",
        _ => "Unknown kind",
    };
    role.slot
        .map_or_else(|| kind.into(), |slot| format!("{kind} slot {slot}"))
}

fn text(out: &Path, report: &Report) {
    println!("File check · First file {:?} · {UNIT_NOTE}", report.reference_role);
    for file in &report.files {
        if let Some(summary) = &file.summary {
            let f = &summary.file;
            let k = &summary.markers;
            let rate = f.frame_rate.map_or("—".into(), |v| v.to_string());
            let progress = match f.progress.value {
                Some(1) => "Recorded",
                Some(4) => "Labeled",
                _ => "Unknown progress",
            };
            println!("{:?} {:?} · {} bytes · {} · {} Hz · {progress} · Marker set {} / Extra {} / No label {} / Unknown kind {}",
                file.role, f.name, f.size_bytes, f.frames, rate, k.template, k.extra, k.unlabeled, k.unknown);
            let pics = match &file.pictures {
                Some(p) => format!("Pictures {}/{}", p.complete_shots, p.expected_shots),
                None if report.pictures_requested => "No pictures".into(),
                None => "Numbers only".into(),
            };
            println!("  {pics} · Source size and time {} · Fewest seen {}/{} · Missing cells {} · Force plates {} / Analog devices {} · Read notes {}",
                if file.tak_unchanged { "same" } else { "changed" }, summary.coverage.min_present,
                summary.coverage.marker_total, summary.coverage.missing_cells, summary.devices.force_plate_count,
                summary.devices.analog_device_count, summary.warnings.len());
            if let Some(takm) = &summary.takm {
                let seal = if takm.seal_ok {
                    "OK"
                } else {
                    "Check failed"
                };
                println!(
                    "  Seal {seal} · Comments {} · Source {:?}",
                    takm.comment_count, takm.source_name
                );
            }
        }
        if !file.errors.is_empty() {
            println!(
                "  {:?} check failed · {}",
                file.role,
                file.errors.join(" / ").replace(['\r', '\n'], " ")
            );
        }
    }
    for pair in &report.comparisons {
        println!("Compare {:?}", pair.role);
        match &pair.slot_comparison {
            SlotComparison::Skipped { reason } => {
                println!("  Slot check skipped · {}", reason.text());
            }
            SlotComparison::Compared(result) | SlotComparison::Incomplete(result) => {
                let count = result
                    .slots
                    .iter()
                    .filter(|row| row.samples.is_none())
                    .count();
                let missing = if count != 0 {
                    format!(" · Skipped {count} slots with more than one track")
                } else {
                    String::new()
                };
                println!(
                    "  Marker set slots · {}{missing} · {}",
                    result.slots.len(),
                    stats_text(&result.totals)
                );
                println!(
                    "  Slots with biggest moves · {}",
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
                        "  Same slot, different name ({}) · {}",
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
            println!("  Same tracks (UID) · Check incomplete");
            continue;
        };
        let equality = |same| if same { "same" } else { "different" };
        println!(
            "  Same tracks (UID) · Frame range {} · Frame rate {} · Marker sets {} / {}",
            equality(c.frame_range_equal),
            equality(c.frame_rate_equal),
            c.reference_template,
            c.other_template
        );
        println!("  UIDs in one file (set/extra on skeleton/other parent/no label/unknown) · First {} · Other {}",
            kind_counts(&c.reference_only), kind_counts(&c.other_only));
        println!(
            "  Same UID, different name ({}) · {}",
            c.rename_count,
            short_list(&c.renames, 10, |r| format!(
                "{:?} → {:?}",
                r.reference_name, r.other_name
            ))
        );
        if c.role_change_count > 0 {
            println!(
                "  Same UID, parent or slot changed ({}) · {}",
                c.role_change_count,
                short_list(&c.role_changes, 4, |r| {
                    let parent = if r.reference.parent_uid_hex != r.other.parent_uid_hex {
                        " · Parent changed"
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
            "  Different UID, same name ({}) · {}",
            c.shared_name_count,
            short_list(&c.shared_names, 10, |n| format!("{:?}", n.name))
        );
        println!(
            "  Marker set UID totals (with name or parent changes) · {}",
            stats_text(&c.marker_set_totals)
        );
        println!(
            "  UIDs with biggest moves · {}",
            short_list(&c.marker_set[..c.top_distance_uids.len()], 10, |m| {
                let peak = m.samples.max.unwrap();
                let mut labels = Vec::new();
                if m.reference_name != m.other_name {
                    labels.push("Name changed");
                }
                if m.structural_role_changed {
                    labels.push("Parent or slot changed");
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
            "  Markers outside the set · First {} · Other {} · {}",
            kind_counts(&c.extra_markers.reference),
            kind_counts(&c.extra_markers.other),
            stats_text(&c.extra_markers.samples)
        );
    }
    if !report.frames.is_empty() {
        println!("Picked frames and marker set counts (seen/total)");
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
                                " (Missing markers {})",
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
            "Check done"
        } else {
            "Check incomplete"
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
            "f000002_top.png is missing or empty."
        );
    }

    #[test]
    fn passes_capture_options_through_and_accepts_missing_sheet_on_request() {
        let temp = TempDir::new("check_picture_options");
        let source = temp.join("source.tak");
        fs::write(&source, sample_tak_with_devices()).unwrap();
        let out = temp.join("out");
        let picture_args = [
            "--size",
            "1200X800",
            "--fit",
            "selected",
            "--trail",
            "999999",
            "--no-names",
            "--no-sheet",
            "--ui",
        ];
        let source_arg = source.to_string_lossy();
        let out_arg = out.to_string_lossy();
        let words = [
            source_arg.as_ref(),
            "--out",
            out_arg.as_ref(),
            "--frames",
            "2",
            "--views",
            "top",
        ]
        .into_iter()
        .chain(picture_args)
        .collect::<Vec<_>>();
        let opts = parse(&args(&words)).unwrap();
        let (_, report) = execute(opts, |input, _, options, out| {
            assert_eq!(options.picture_args, picture_args);
            let dir = out.join(&input.folder);
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("f000002_top.png"), "shot").unwrap();
            fs::write(
                dir.join("captures.json"),
                serde_json::to_vec(&json!({
                    "tak_unchanged":true, "start_frame":0, "end_frame":299,
                    "views":["top"], "select":[], "sheet":null, "notes":[],
                    "shots":[{"frame":2,"view":"top","file":"f000002_top.png"}],
                }))
                .unwrap(),
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(report.exit_code(), 0);
        let receipt: Value =
            serde_json::from_slice(&fs::read(out.join("check.json")).unwrap()).unwrap();
        assert_eq!(
            receipt["picture_options"],
            json!({
                "size":[1200.0,800.0], "fit":"selected", "trail":100000,
                "names":false, "sheet":false, "ui":true,
            })
        );
        assert_eq!(receipt["schema"], 1);
        assert_eq!(receipt["producer"]["version"], crate::version::VERSION);
        for (flag, value) in [
            ("--size", "399x300"),
            ("--size", "1200xNaN"),
            ("--fit", "unknown"),
            ("--trail", "1.1"),
        ] {
            let expected = capture::parse(&args(&["--capture", "a.tak", flag, value])).unwrap_err();
            assert_eq!(
                parse(&args(&["a.tak", "--out", "out", flag, value])).err(),
                Some(expected)
            );
        }
        for flag in ["--size", "--fit", "--trail"] {
            assert!(parse(&args(&["a.tak", "--out", "out", flag])).is_err());
        }
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
