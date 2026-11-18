//! Per-file TAK/TAKM facts shared by summary and comparison commands.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

use serde::Serialize;

use crate::tak::{self, uid_hex, MarkerKind, Take, Trajectory};
use crate::{takm, view_data};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FrameRange {
    pub first: i64,
    pub last: i64,
}

impl FrameRange {
    fn count(self) -> Option<u64> {
        u64::try_from(i128::from(self.last) - i128::from(self.first) + 1)
            .ok()
            .filter(|&n| n > 0)
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let first = self.first.max(other.first);
        let last = self.last.min(other.last);
        (first <= last).then_some(Self { first, last })
    }
}

impl fmt::Display for FrameRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}–{}", self.first, self.last)
    }
}

#[derive(Debug)]
pub struct SummaryOptions {
    pub frames: Option<FrameRange>,
    pub markers: Vec<String>,
    pub top: usize,
}

impl Default for SummaryOptions {
    fn default() -> Self {
        Self {
            frames: None,
            markers: Vec::new(),
            top: 10,
        }
    }
}

#[derive(Debug)]
pub enum SummaryError {
    Argument(String),
    Read(String),
}

impl fmt::Display for SummaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Argument(message) | Self::Read(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SummaryError {}

#[derive(Debug, Serialize)]
pub struct TakeSummary {
    pub file: FileSummary,
    pub takm: Option<TakmSummary>,
    pub markers: MarkerSummary,
    pub coverage: Coverage,
    pub window_coverage: Option<Coverage>,
    pub gaps: GapLists,
    pub devices: DeviceSummary,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xyz: Option<Coordinates>,
}

#[derive(Debug, Serialize)]
pub struct Coordinates {
    coordinate_units: &'static str,
    pub trajectories: Vec<CoordinateTrack>,
}

#[derive(Debug, Serialize)]
pub struct CoordinateTrack {
    pub name: String,
    pub uid_hex: String,
    pub kind: &'static str,
    pub parent_uid_hex: Option<String>,
    pub slot: Option<i32>,
    pub samples: Vec<CoordinateSample>,
}

#[derive(Debug, Serialize)]
pub struct CoordinateSample {
    pub frame: i64,
    pub present: bool,
    pub xyz_mm: Option<[f64; 3]>,
}

impl Coordinates {
    pub fn read(take: &Take, names: &[String], frames: &[i64]) -> Result<Self, SummaryError> {
        let scale = view_data::detect_unit_scale(take);
        let mut trajectories = Vec::new();
        for name in names {
            let before = trajectories.len();
            for track in take.trajectories.iter().filter(|track| &track.name == name) {
                let samples = frames
                    .iter()
                    .map(|&frame| {
                        let xyz_mm = u32::try_from(frame)
                            .ok()
                            .and_then(|frame| track.frames.binary_search(&frame).ok())
                            .map(|i| track.xyz[i])
                            .filter(view_data::display_sample_present)
                            .map(|xyz| view_data::capture_xyz_mm(xyz.map(|v| v * scale)));
                        CoordinateSample {
                            frame,
                            present: xyz_mm.is_some(),
                            xyz_mm,
                        }
                    })
                    .collect();
                trajectories.push(CoordinateTrack {
                    name: track.name.clone(),
                    uid_hex: uid_hex(&track.uid),
                    kind: track.kind.as_str(),
                    parent_uid_hex: track.parent_uid.as_ref().map(uid_hex),
                    slot: track.slot,
                    samples,
                });
            }
            if trajectories.len() == before {
                return Err(SummaryError::Argument(format!(
                    "파일에 {name:?} 마커가 없어요."
                )));
            }
        }
        Ok(Self {
            coordinate_units: "mm by viewer convention (coordinate unit unverified)",
            trajectories,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct FileSummary {
    pub name: String,
    pub kind: &'static str,
    pub size_bytes: u64,
    pub progress: Progress,
    pub frames: FrameRange,
    pub frame_count: u64,
    pub frame_rate: Option<f64>,
    pub duration_seconds: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Progress {
    pub value: Option<i64>,
    pub meaning: &'static str,
}

impl Progress {
    fn from_meta(raw: Option<&str>) -> Self {
        let value = raw.and_then(|raw| tak::metadata::python_int(raw).ok());
        let meaning = match value {
            Some(1) => "Recorded",
            Some(4) => "Labeled",
            _ => "Unknown",
        };
        Self { value, meaning }
    }
}

#[derive(Debug, Serialize)]
pub struct TakmSummary {
    pub seal_ok: bool,
    pub comment_count: usize,
    pub source_name: String,
}

#[derive(Debug, Default, Serialize)]
pub struct MarkerSummary {
    pub template: usize,
    pub extra: usize,
    pub unlabeled: usize,
    pub unknown: usize,
    pub assets: Vec<AssetSummary>,
}

#[derive(Debug, Serialize)]
pub struct AssetSummary {
    pub name: String,
    pub markerset_name: Option<String>,
    pub template_count: usize,
}

#[derive(Debug, Serialize)]
pub struct Coverage {
    pub frames: FrameRange,
    pub marker_total: usize,
    pub min_present: usize,
    pub median_present: f64,
    pub complete_frames: u64,
    pub missing_cells: u64,
}

#[derive(Debug, Serialize)]
pub struct GapLists {
    pub frames: FrameRange,
    pub template: Vec<MarkerGaps>,
    pub extra: Vec<MarkerGaps>,
    pub other: Vec<MarkerGaps>,
    pub template_more: usize,
    pub extra_more: usize,
}

#[derive(Debug, Serialize)]
pub struct MarkerGaps {
    pub name: String,
    pub uid_hex: String,
    pub kind: &'static str,
    pub gap_count: usize,
    pub missing_frames: u64,
    pub longest_gap: Option<Gap>,
    pub ranges: Option<Vec<FrameRange>>,
}

#[derive(Debug, Serialize)]
pub struct Gap {
    pub frames: FrameRange,
    pub length: u64,
}

#[derive(Debug, Serialize)]
pub struct DeviceSummary {
    pub force_plate_count: usize,
    pub analog_device_count: usize,
    pub analog: Vec<AnalogDeviceSummary>,
}

#[derive(Debug, Serialize)]
pub struct AnalogDeviceSummary {
    pub name: Option<String>,
    pub channel_count: usize,
    pub channels: Vec<AnalogChannelSummary>,
}

#[derive(Debug, Serialize)]
pub struct AnalogChannelSummary {
    pub name: String,
    pub samples_per_frame: Option<usize>,
}

/// Read a single file and return the same facts used by the text and JSON formatters.
/// Reads analog headers on the fast path. TAKM also verifies its fixed part and reads
/// the live Comment count. No files, settings, or coordination state are written.
pub fn read_summary(path: &Path, options: &SummaryOptions) -> Result<TakeSummary, SummaryError> {
    read_summary_with_take(path, options).map(|(_, summary)| summary)
}

/// Reuse the same decoded samples for summary and comparison.
pub fn read_summary_with_take(
    path: &Path,
    options: &SummaryOptions,
) -> Result<(Take, TakeSummary), SummaryError> {
    let is_takm = takm::is_takm_path(path);
    if !is_takm && !tak::is_take_path(path) {
        return Err(SummaryError::Argument(
            ".tak 또는 .takm 파일을 지정해 주세요.".into(),
        ));
    }

    let mut fallback = None;
    let read = tak::fast_read::read_take_fast_with_fallback(path, false, |reason| {
        fallback = Some(reason);
    })
    .map_err(|e| SummaryError::Read(format!("파일을 읽지 못했어요. ({e})")))?;
    let mut take = read.take;
    if let Some(reason) = fallback {
        take.warnings
            .push(format!("빠른 읽기 대신 전체를 읽었어요. ({reason})"));
    }
    if let Some(job) = read.analog {
        if let Some(reason) = job.fallback_reason().map(str::to_owned) {
            let (analog, warning) = job
                .run()
                .map_err(|e| SummaryError::Read(format!("아날로그 장치를 읽지 못했어요. ({e})")))?;
            take.analog = analog;
            take.warnings.extend(warning);
            take.warnings
                .push(format!("장치 데이터를 전체 읽었어요. ({reason})"));
        }
    }
    let mut summary = summarize(&take, options)?;
    if is_takm {
        let index = takm::read_index_path(path)
            .map_err(|e| SummaryError::Read(format!("TAKM 정보를 읽지 못했어요. ({e})")))?;
        let seal_ok = takm::verify_fixed(path)
            .map_err(|e| {
                summary
                    .warnings
                    .push(format!("TAKM 봉인을 확인하지 못했어요. ({e})"));
            })
            .is_ok();
        let parsed = takm::read_comments(path)
            .map_err(|e| SummaryError::Read(format!("Comment를 읽지 못했어요. ({e})")))?;
        if parsed.skipped > 0 {
            summary.warnings.push(format!(
                "읽을 수 없는 Comment 줄 {}개를 건너뛰었어요.",
                parsed.skipped
            ));
        }
        if parsed.cut {
            summary
                .warnings
                .push("끝에서 잘린 Comment 줄을 건너뛰었어요.".into());
        }
        summary.takm = Some(TakmSummary {
            seal_ok,
            comment_count: parsed.comments().len(),
            source_name: index.source.name,
        });
    }
    Ok((take, summary))
}

fn summarize(take: &Take, options: &SummaryOptions) -> Result<TakeSummary, SummaryError> {
    let frames = FrameRange {
        first: take.start_frame,
        last: take.end_frame.max(take.start_frame),
    };
    let frame_count = frames
        .count()
        .ok_or_else(|| SummaryError::Read("파일의 프레임 수가 너무 커요.".into()))?;
    let gap_frames = options.frames.unwrap_or(frames);
    if gap_frames.first > gap_frames.last || frames.intersection(gap_frames) != Some(gap_frames) {
        return Err(SummaryError::Argument(format!(
            "프레임 범위 {gap_frames}이 파일 범위 {frames} 안에 있어야 해요."
        )));
    }
    let mut names: HashSet<&str> = options.markers.iter().map(String::as_str).collect();
    if !names.is_empty() {
        for trajectory in &take.trajectories {
            names.remove(trajectory.name.as_str());
        }
        if let Some(name) = options
            .markers
            .iter()
            .find(|name| names.contains(name.as_str()))
        {
            return Err(SummaryError::Argument(format!(
                "파일에 {name:?} 마커가 없어요."
            )));
        }
    }

    let markers = marker_summary(take);
    if frame_count.checked_mul(markers.template as u64).is_none() {
        return Err(SummaryError::Read(
            "프레임 수와 마커 수의 곱이 너무 커요.".into(),
        ));
    }
    let (gaps, events) = marker_gaps(take, options, frames, gap_frames);
    let (coverage, window_coverage) = coverage(frames, options.frames, markers.template, events);
    let mut warnings = take.warnings.clone();
    let recovered = take
        .trajectories
        .iter()
        .filter(|t| t.lenient_membership)
        .count();
    if recovered > 0 {
        warnings.push(format!(
            "보완 읽기로 마커 {recovered}개의 구조를 확인했어요."
        ));
    }
    let frame_rate = take.frame_rate.is_finite().then_some(take.frame_rate);
    let duration_seconds = frame_rate
        .filter(|&rate| rate > 0.0)
        .map(|rate| frame_count as f64 / rate)
        .filter(|secs| secs.is_finite());

    Ok(TakeSummary {
        file: FileSummary {
            name: take
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            kind: if takm::is_takm_path(&take.path) {
                "takm"
            } else {
                "tak"
            },
            size_bytes: take.file_size,
            progress: Progress::from_meta(take.meta("Progress")),
            frames,
            frame_count,
            frame_rate,
            duration_seconds,
        },
        takm: None,
        markers,
        coverage,
        window_coverage,
        gaps,
        devices: devices(take),
        warnings,
        xyz: None,
    })
}

fn marker_summary(take: &Take) -> MarkerSummary {
    let mut summary = MarkerSummary::default();
    let mut template_counts = HashMap::new();
    for trajectory in &take.trajectories {
        match trajectory.kind {
            MarkerKind::Template => {
                summary.template += 1;
                if let Some(uid) = trajectory.asset_uid {
                    *template_counts.entry(uid).or_insert(0) += 1;
                }
            }
            MarkerKind::SkeletonExtra | MarkerKind::OtherParent => summary.extra += 1,
            MarkerKind::Unlabeled => summary.unlabeled += 1,
            MarkerKind::Unknown => summary.unknown += 1,
        }
    }
    summary.assets = take
        .assets
        .iter()
        .map(|asset| AssetSummary {
            name: asset.name.clone(),
            markerset_name: asset.markerset_name.clone(),
            template_count: template_counts.get(&asset.uid).copied().unwrap_or(0),
        })
        .collect();
    summary
}

type CoverageEvents = Vec<(u64, i64)>;

fn frame_offset(frame: i64, first: i64) -> u64 {
    (i128::from(frame) - i128::from(first)) as u64
}

fn marker_gaps(
    take: &Take,
    options: &SummaryOptions,
    frames: FrameRange,
    gap_frames: FrameRange,
) -> (GapLists, CoverageEvents) {
    let names: HashSet<&str> = options.markers.iter().map(String::as_str).collect();
    let mut lists = GapLists {
        frames: gap_frames,
        template: Vec::new(),
        extra: Vec::new(),
        other: Vec::new(),
        template_more: 0,
        extra_more: 0,
    };
    let mut events = Vec::new();
    for trajectory in &take.trajectories {
        let template = trajectory.kind == MarkerKind::Template;
        let extra = matches!(
            trajectory.kind,
            MarkerKind::SkeletonExtra | MarkerKind::OtherParent
        );
        let selected = if names.is_empty() {
            template || extra
        } else {
            names.contains(trajectory.name.as_str())
        };
        if !template && !selected {
            continue;
        }

        let mut ranges = Vec::new();
        for (first, last) in view_data::gaps(&trajectory.frames, frames.first, frames.last) {
            if template {
                events.push((frame_offset(first, frames.first), 1));
                events.push((frame_offset(last, frames.first) + 1, -1));
            }
            if let Some(range) = (FrameRange { first, last })
                .intersection(gap_frames)
                .filter(|_| selected)
            {
                ranges.push(range);
            }
        }
        if selected && (!ranges.is_empty() || !names.is_empty()) {
            let row = gap_summary(trajectory, ranges, !names.is_empty());
            if template {
                lists.template.push(row);
            } else if extra {
                lists.extra.push(row);
            } else {
                lists.other.push(row);
            }
        }
    }
    for rows in [&mut lists.template, &mut lists.extra, &mut lists.other] {
        rows.sort_unstable_by(|a, b| {
            b.missing_frames
                .cmp(&a.missing_frames)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.uid_hex.cmp(&b.uid_hex))
        });
    }
    if names.is_empty() {
        lists.template_more = lists.template.len().saturating_sub(options.top);
        lists.extra_more = lists.extra.len().saturating_sub(options.top);
        lists.template.truncate(options.top);
        lists.extra.truncate(options.top);
    }
    (lists, events)
}

fn gap_summary(trajectory: &Trajectory, ranges: Vec<FrameRange>, show_ranges: bool) -> MarkerGaps {
    let mut missing_frames = 0;
    let mut longest_gap: Option<Gap> = None;
    for &frames in &ranges {
        let length = frames.count().unwrap();
        missing_frames += length;
        if longest_gap.as_ref().is_none_or(|gap| length > gap.length) {
            longest_gap = Some(Gap { frames, length });
        }
    }
    MarkerGaps {
        name: trajectory.name.clone(),
        uid_hex: uid_hex(&trajectory.uid),
        kind: trajectory.kind.as_str(),
        gap_count: ranges.len(),
        missing_frames,
        longest_gap,
        ranges: show_ranges.then_some(ranges),
    }
}

// Sweep gap endpoints. Each run contributes to the histogram by its length, so even a long
// recording with no samples uses space proportional to markers and gaps, not frame count.
fn coverage(
    frames: FrameRange,
    window: Option<FrameRange>,
    total: usize,
    mut events: CoverageEvents,
) -> (Coverage, Option<Coverage>) {
    events.sort_unstable_by_key(|&(offset, _)| offset);
    let mut full = vec![0; total + 1];
    let mut window_counts = window.map(|_| vec![0; total + 1]);
    let window_offsets = window.map(|range| {
        (
            frame_offset(range.first, frames.first),
            frame_offset(range.last, frames.first) + 1,
        )
    });
    let mut cursor = 0;
    let mut missing = 0;
    for (next, change) in events.into_iter().chain([(frames.count().unwrap(), 0)]) {
        if cursor < next {
            let present = total - missing as usize;
            full[present] += next - cursor;
            if let (Some((first, end)), Some(counts)) = (window_offsets, &mut window_counts) {
                counts[present] += next.min(end).saturating_sub(cursor.max(first));
            }
        }
        cursor = next;
        missing += change;
    }

    (
        coverage_stats(frames, full),
        window
            .zip(window_counts)
            .map(|(range, counts)| coverage_stats(range, counts)),
    )
}

fn coverage_stats(frames: FrameRange, counts: Vec<u64>) -> Coverage {
    let total = counts.len() - 1;
    let frame_count = frames.count().unwrap();
    let mut cumulative = 0;
    let mut lower = None;
    let mut upper = None;
    let mut missing_cells = 0;
    for (present, &n) in counts.iter().enumerate() {
        cumulative += n;
        if cumulative > (frame_count - 1) / 2 {
            lower.get_or_insert(present);
        }
        if cumulative > frame_count / 2 {
            upper.get_or_insert(present);
        }
        missing_cells += (total - present) as u64 * n;
    }
    Coverage {
        frames,
        marker_total: total,
        min_present: counts.iter().position(|&n| n != 0).unwrap_or(0),
        median_present: (lower.unwrap_or(0) as f64 + upper.unwrap_or(0) as f64) / 2.0,
        complete_frames: if total == 0 { 0 } else { counts[total] },
        missing_cells,
    }
}

fn devices(take: &Take) -> DeviceSummary {
    let analog = take
        .analog
        .iter()
        .enumerate()
        .map(|(i, device)| {
            let channels = device
                .channels
                .iter()
                .map(|channel| {
                    let samples_per_frame = if channel.dim == 1 {
                        Some(1)
                    } else {
                        take.device_sampling
                            .get(i)
                            .copied()
                            .flatten()
                            .filter(|sampling| {
                                let expected_rate =
                                    take.frame_rate * sampling.samples_per_frame as f64;
                                (1..=channel.dim).contains(&sampling.samples_per_frame)
                                    && sampling.rate_hz > 0.0
                                    && sampling.rate_hz.is_finite()
                                    && (sampling.rate_hz - expected_rate).abs()
                                        <= sampling.rate_hz * 1e-6
                            })
                            .map(|sampling| sampling.samples_per_frame)
                    };
                    AnalogChannelSummary {
                        name: channel.name.clone(),
                        samples_per_frame,
                    }
                })
                .collect();
            AnalogDeviceSummary {
                name: take.device_names.get(i).cloned().flatten(),
                channel_count: device.channels.len(),
                channels,
            }
        })
        .collect();
    DeviceSummary {
        force_plate_count: take.plates.len(),
        analog_device_count: take.analog.len(),
        analog,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tak::{AnalogChannel, AnalogDevice, DeviceSampling, SkeletonAsset};

    fn marker(id: u8, name: &str, kind: MarkerKind, frames: &[u32]) -> Trajectory {
        let asset_uid =
            matches!(kind, MarkerKind::Template | MarkerKind::SkeletonExtra).then_some([42; 16]);
        Trajectory {
            uid: [id; 16],
            name: name.into(),
            parent_uid: asset_uid,
            slot_raw: None,
            slot: None,
            kind,
            asset_uid,
            frames: frames.to_vec().into(),
            xyz: vec![[0.0; 3]; frames.len()].into(),
            raw16_sha256: String::new(),
            lenient_membership: false,
        }
    }

    fn fixture() -> Take {
        use MarkerKind::*;
        Take {
            path: "trial.tak".into(),
            file_size: 1000,
            metadata: vec![
                ("Progress".into(), "4".into()),
                ("WorkingStartFrame".into(), "102".into()),
            ],
            frame_rate: 100.0,
            start_frame: 100,
            end_frame: 105,
            trajectories: vec![
                marker(1, "A", Template, &[101, 103, 104]),
                marker(2, "B", Template, &[100, 101, 102, 103, 104, 105]),
                marker(3, "C7", SkeletonExtra, &[102, 104]),
                marker(4, "CLAV", OtherParent, &[101, 102, 103, 104]),
                marker(5, "LTHI", Unlabeled, &[101]),
                marker(6, "RTHI", Unknown, &[103]),
            ],
            assets: vec![SkeletonAsset {
                uid: [42; 16],
                name: "asset".into(),
                markerset_name: Some("recorded set".into()),
                model: None,
                slot_comparison_model: None,
                sticks: vec![],
                marker_colors: vec![],
            }],
            label_groups: vec![],
            plates: vec![],
            analog: vec![],
            device_sampling: vec![],
            device_names: vec![],
            warnings: vec![],
        }
    }

    #[test]
    fn structural_counts_coverage_and_gaps_include_take_edges() {
        let take = fixture();
        let report = summarize(
            &take,
            &SummaryOptions {
                top: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            report.file.frames,
            FrameRange {
                first: 100,
                last: 105
            }
        );
        assert_eq!(report.file.frame_count, 6);
        assert_eq!(report.file.duration_seconds, Some(0.06));
        assert_eq!(report.markers.template, 2);
        assert_eq!(report.markers.extra, 2);
        assert_eq!(report.markers.unlabeled, 1);
        assert_eq!(report.markers.unknown, 1);
        assert_eq!(
            report.markers.assets[0].markerset_name.as_deref(),
            Some("recorded set")
        );
        assert_eq!(report.markers.assets[0].template_count, 2);
        assert_eq!(report.coverage.marker_total, 2);
        assert_eq!(report.coverage.min_present, 1);
        assert_eq!(report.coverage.median_present, 1.5);
        assert_eq!(report.coverage.complete_frames, 3);
        assert_eq!(report.coverage.missing_cells, 3);
        assert_eq!(report.gaps.template.len(), 1);
        let a = &report.gaps.template[0];
        assert_eq!((&*a.name, a.gap_count, a.missing_frames), ("A", 3, 3));
        assert_eq!(
            a.longest_gap.as_ref().unwrap().frames,
            FrameRange {
                first: 100,
                last: 100
            }
        );
        assert!(a.ranges.is_none());
        assert_eq!(report.gaps.extra[0].name, "C7");
        assert_eq!(report.gaps.extra[0].missing_frames, 4);
        assert_eq!(report.gaps.extra_more, 1);
        assert!(report.gaps.other.is_empty());
    }

    #[test]
    fn window_clips_gaps_and_named_search_keeps_gapless_and_all_matching_kinds() {
        let mut take = fixture();
        take.trajectories[5].name = "B".into();
        let options = SummaryOptions {
            frames: Some(FrameRange {
                first: 102,
                last: 104,
            }),
            markers: vec!["B".into(), "C7".into()],
            top: 0,
        };
        let report = summarize(&take, &options).unwrap();
        assert_eq!(report.coverage.missing_cells, 3);
        let window = report.window_coverage.unwrap();
        assert_eq!(window.frames, options.frames.unwrap());
        assert_eq!(window.marker_total, 2);
        assert_eq!(window.min_present, 1);
        assert_eq!(window.median_present, 2.0);
        assert_eq!(window.complete_frames, 2);
        assert_eq!(window.missing_cells, 1);
        assert_eq!(report.gaps.template[0].ranges, Some(vec![]));
        assert_eq!(
            report.gaps.extra[0].ranges,
            Some(vec![FrameRange {
                first: 103,
                last: 103
            }])
        );
        assert_eq!(
            report.gaps.other[0].ranges,
            Some(vec![
                FrameRange {
                    first: 102,
                    last: 102
                },
                FrameRange {
                    first: 104,
                    last: 104
                },
            ])
        );
        assert!(matches!(
            summarize(&take, &SummaryOptions { frames: Some(FrameRange { first: 99, last: 108 }), ..Default::default() }),
            Err(SummaryError::Argument(message)) if message.contains("파일 범위")
        ));
        assert!(matches!(
            summarize(&take, &SummaryOptions { markers: vec!["무명".into()], ..Default::default() }),
            Err(SummaryError::Argument(message)) if message == "파일에 \"무명\" 마커가 없어요."
        ));
    }

    #[test]
    fn empty_template_and_progress_values_are_explicit() {
        let mut take = fixture();
        take.trajectories.clear();
        let report = summarize(&take, &SummaryOptions::default()).unwrap();
        assert_eq!(report.coverage.marker_total, 0);
        assert_eq!(report.coverage.complete_frames, 0);
        assert_eq!(report.coverage.min_present, 0);
        assert_eq!(report.coverage.median_present, 0.0);
        assert_eq!(report.coverage.missing_cells, 0);
        for (raw, value, meaning) in [
            (Some("1"), Some(1), "Recorded"),
            (Some(" 4 "), Some(4), "Labeled"),
            (Some("9"), Some(9), "Unknown"),
            (Some("bad"), None, "Unknown"),
            (None, None, "Unknown"),
        ] {
            let progress = Progress::from_meta(raw);
            assert_eq!((progress.value, progress.meaning), (value, meaning));
        }
    }

    #[test]
    fn device_sample_counts_use_declared_sampling_and_scalar_width() {
        let mut take = fixture();
        let channel = |dim| AnalogChannel {
            name: "AnalogChannel0".into(),
            kind: "FloatChannel".into(),
            dim,
            frames: Vec::new().into(),
            values: Vec::new().into(),
        };
        take.analog = vec![
            AnalogDevice {
                channels: vec![channel(1)],
            },
            AnalogDevice {
                channels: vec![channel(16)],
            },
            AnalogDevice {
                channels: vec![channel(16)],
            },
            AnalogDevice {
                channels: vec![channel(16)],
            },
        ];
        take.device_sampling = vec![
            None,
            Some(DeviceSampling {
                rate_hz: 800.0,
                samples_per_frame: 8,
            }),
            Some(DeviceSampling {
                rate_hz: 700.0,
                samples_per_frame: 8,
            }),
            None,
        ];
        let report = summarize(&take, &SummaryOptions::default()).unwrap();
        let samples: Vec<_> = report
            .devices
            .analog
            .iter()
            .map(|device| device.channels[0].samples_per_frame)
            .collect();
        assert_eq!(samples, [Some(1), Some(8), None, None]);
    }
}
