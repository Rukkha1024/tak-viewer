//! Visual check captures (`--capture`, and the menu "화면 캡처 저장").
//! Renders chosen frames from chosen angles to PNG inside the repository
//! (`experiments/tak_captures/<time>_<file>/`), plus `sheet.png` (all shots in one image) and
//! `captures.json` (frame, view, camera, marker counts, missing marker-set markers, selected
//! marker positions). Read-only: never writes a `.tak`, never writes under `input/`.

use crate::camera::Camera;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tak_core::geom::{v3, Vec3};
use tak_core::tak::MarkerKind;
use tak_core::view_data::ViewData;

pub const USAGE: &str = "\
tak_viewer --capture <file.tak> [options]      (WSL paths are fine)
  --frames 600 | 100,200,300 | 100:1000:100 | even:5    frames (default even:5)
  --views persp,top,side,front   angles (default). Also: bottom (= down), +x, -x, +z, -z, all
          side  = looks across the walking direction (long horizontal axis of the whole take)
          front = looks along the walking direction; top / bottom = from above / below
  --select LKNE,RKNE   highlight markers (cyan = last, yellow = others)
  --trail N            path of the selected markers over the previous N frames
  --fit frame|take|selected   what the camera frames (default frame = markers of that frame)
  --no-names           hide marker names (default: names of labeled markers are shown)
  --ui                 keep the side panels (default: 3D view only)
  --size 1600x1000     window size in points
  --out DIR            output folder (default <repo>/experiments/tak_captures/<time>_<file>)
  --no-sheet           do not write sheet.png
  --headless           no window: works without a desktop (agent shells). Pictures are painted on
                       the CPU: same content as the window capture, pixels can differ slightly
Writes one PNG per shot, sheet.png and captures.json, then prints the folder. Never writes a .tak.
Exit code (with --headless): 0 = done, 1 = failed, 2 = bad arguments.";

/// Upper limits so one command cannot produce thousands of images.
pub const MAX_FRAMES: usize = 60;
pub const MAX_SHOTS: usize = 120;
/// Width of one picture in sheet.png.
pub const SHEET_CELL_W: usize = 640;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewPreset {
    /// Keep the camera as it is (menu capture).
    Current,
    /// Motive-like perspective, framed like Shift+F.
    Persp,
    Top,
    Bottom,
    Side,
    Front,
    PlusX,
    MinusX,
    PlusZ,
    MinusZ,
}

impl ViewPreset {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "persp" | "perspective" | "3d" => Self::Persp,
            "top" => Self::Top,
            "bottom" | "down" => Self::Bottom,
            "side" => Self::Side,
            "front" => Self::Front,
            "+x" | "x" => Self::PlusX,
            "-x" => Self::MinusX,
            "+z" | "z" => Self::PlusZ,
            "-z" => Self::MinusZ,
            "current" => Self::Current,
            _ => return None,
        })
    }

    /// File-name part.
    pub fn key(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Persp => "persp",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Side => "side",
            Self::Front => "front",
            Self::PlusX => "xpos",
            Self::MinusX => "xneg",
            Self::PlusZ => "zpos",
            Self::MinusZ => "zneg",
        }
    }

    /// Text in the top-left corner of the 3D view.
    pub fn label(self) -> &'static str {
        match self {
            Self::Current | Self::Persp => "Perspective",
            Self::Top => "Top · 위에서",
            Self::Bottom => "Bottom · 아래에서",
            Self::Side => "Side · 옆에서 (진행 방향이 가로)",
            Self::Front => "Front · 진행 방향에서",
            Self::PlusX => "+X 쪽에서",
            Self::MinusX => "-X 쪽에서",
            Self::PlusZ => "+Z 쪽에서",
            Self::MinusZ => "-Z 쪽에서",
        }
    }

    /// Camera (yaw, pitch, vertical fov) in radians; None = Motive default framing (Persp) or
    /// keep the camera (Current). `walk_x` = the long horizontal axis of the take is X.
    /// Fixed views use a narrow field of view (12°), close to an orthographic drawing.
    pub fn angles(self, walk_x: bool) -> Option<(f32, f32, f32)> {
        let d = |deg: f32| deg.to_radians();
        let narrow = d(12.0);
        // Camera yaw 0 = on +Z looking toward -Z (screen right = +X); 90° = on +X.
        let (side, front) = if walk_x { (0.0, 90.0) } else { (90.0, 0.0) };
        Some(match self {
            Self::Current | Self::Persp => return None,
            Self::Top => (d(side), d(89.0), narrow),
            Self::Bottom => (d(side), d(-89.0), narrow),
            Self::Side => (d(side), 0.0, narrow),
            Self::Front => (d(front), 0.0, narrow),
            Self::PlusX => (d(90.0), 0.0, narrow),
            Self::MinusX => (d(-90.0), 0.0, narrow),
            Self::PlusZ => (0.0, 0.0, narrow),
            Self::MinusZ => (d(180.0), 0.0, narrow),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Markers present in that frame (labeled ones when there are at least 3).
    Frame,
    /// The whole take (like Shift+F).
    Take,
    /// Selected markers in that frame.
    Selected,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FrameSpec {
    List(Vec<i64>),
    /// start, end, step
    Range(i64, i64, i64),
    /// n frames evenly spread over the take
    Even(usize),
}

impl FrameSpec {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let bad = || format!("--frames 형식을 모르겠어요: {s} (예: 600, 100,200, 100:1000:100, even:5)");
        if let Some(n) = s.strip_prefix("even:") {
            let n: usize = n.trim().parse().map_err(|_| bad())?;
            return if (1..=MAX_FRAMES).contains(&n) { Ok(Self::Even(n)) } else { Err(bad()) };
        }
        if s.contains(':') {
            let parts: Vec<i64> = s.split(':').map(|p| p.trim().parse::<i64>()).collect::<Result<_, _>>().map_err(|_| bad())?;
            return match parts[..] {
                [a, b] if a <= b => Ok(Self::Range(a, b, 1)),
                [a, b, step] if a <= b && step > 0 => Ok(Self::Range(a, b, step)),
                _ => Err(bad()),
            };
        }
        let list: Vec<i64> = s.split(',').map(|p| p.trim().parse::<i64>()).collect::<Result<_, _>>().map_err(|_| bad())?;
        if list.is_empty() {
            return Err(bad());
        }
        Ok(Self::List(list))
    }

    /// Concrete frames inside [first, last]: sorted, unique, at most MAX_FRAMES.
    pub fn resolve(&self, first: i64, last: i64) -> Vec<i64> {
        let last = last.max(first);
        let mut v: Vec<i64> = match self {
            Self::List(l) => l.iter().map(|&f| f.clamp(first, last)).collect(),
            Self::Range(a, b, step) => {
                let (a, b) = ((*a).max(first), (*b).min(last));
                if a > b {
                    vec![]
                } else {
                    (a..=b).step_by((*step).max(1) as usize).take(MAX_FRAMES).collect()
                }
            }
            Self::Even(n) => {
                let n = (*n).max(1) as i64;
                let (first, last) = (i128::from(first), i128::from(last));
                if n == 1 {
                    vec![((first + last) / 2) as i64]
                } else {
                    (0..n).map(|i| (first + (last - first) * i128::from(i) / i128::from(n - 1)) as i64).collect()
                }
            }
        };
        v.sort_unstable();
        v.dedup();
        v.truncate(MAX_FRAMES);
        v
    }
}

#[derive(Clone, Debug)]
pub struct CapturePlan {
    pub tak: PathBuf,
    pub out: Option<PathBuf>,
    pub frames: FrameSpec,
    pub views: Vec<ViewPreset>,
    pub select: Vec<String>,
    pub fit: Fit,
    pub names: bool,
    pub trail: Option<i64>,
    pub full_ui: bool,
    pub sheet: bool,
    pub size: [f32; 2],
    /// Command line run: close the window when done. Menu capture: keep the app open.
    pub exit_when_done: bool,
}

pub const DEFAULT_VIEWS: [ViewPreset; 4] = [ViewPreset::Persp, ViewPreset::Top, ViewPreset::Side, ViewPreset::Front];

pub fn parse_views(value: &str) -> Result<Vec<ViewPreset>, String> {
    if value.trim().eq_ignore_ascii_case("all") {
        return Ok(vec![ViewPreset::Persp, ViewPreset::Top, ViewPreset::Side, ViewPreset::Front, ViewPreset::Bottom]);
    }
    value.split(',').map(|s| ViewPreset::parse(s).ok_or_else(|| format!("모르는 각도: {s}"))).collect()
}

pub fn parse_selection(value: &str) -> Vec<String> {
    value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

pub fn shot_file_name(frame: i64, view: ViewPreset) -> String {
    format!("f{frame:06}_{}.png", view.key())
}

pub fn parse_size(value: &str) -> Result<[f32; 2], String> {
    let (w, h) = value.split_once(['x', 'X']).ok_or("--size에는 1400x860처럼 폭x높이를 써 주세요.")?;
    let w = w.trim().parse::<f32>().map_err(|_| "--size의 폭을 숫자로 써 주세요.")?;
    let h = h.trim().parse::<f32>().map_err(|_| "--size의 높이를 숫자로 써 주세요.")?;
    if !(400.0..=8000.0).contains(&w) || !(300.0..=8000.0).contains(&h) {
        return Err("--size는 400x300 ~ 8000x8000 포인트예요.".into());
    }
    Ok([w, h])
}

/// `Ok(None)` when `--capture` is absent. `Err` carries a message for the user (print USAGE too).
pub fn parse(args: &[String]) -> Result<Option<CapturePlan>, String> {
    if !args.iter().any(|a| a == "--capture") {
        return Ok(None);
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Err(String::new());
    }
    let mut plan = CapturePlan {
        tak: PathBuf::new(),
        out: None,
        frames: FrameSpec::Even(5),
        views: DEFAULT_VIEWS.to_vec(),
        select: vec![],
        fit: Fit::Frame,
        names: true,
        trail: None,
        full_ui: false,
        sheet: true,
        size: [1600.0, 1000.0],
        exit_when_done: true,
    };
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    let value = |i: usize, name: &str| -> Result<String, String> {
        args.get(i + 1).cloned().ok_or_else(|| format!("{name} 뒤에 값이 필요해요"))
    };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--capture" => {}
            "--frames" => {
                plan.frames = FrameSpec::parse(&value(i, a)?)?;
                i += 1;
            }
            "--views" | "--view" => {
                plan.views = parse_views(&value(i, a)?)?;
                i += 1;
            }
            "--select" => {
                plan.select = parse_selection(&value(i, a)?);
                i += 1;
            }
            "--trail" => {
                let n: i64 = value(i, a)?.trim().parse().map_err(|_| "--trail 뒤에는 프레임 수(정수)".to_owned())?;
                plan.trail = Some(n.clamp(1, 100_000));
                i += 1;
            }
            "--fit" => {
                plan.fit = match value(i, a)?.trim() {
                    "frame" => Fit::Frame,
                    "take" => Fit::Take,
                    "selected" => Fit::Selected,
                    other => return Err(format!("--fit은 frame, take, selected 중 하나: {other}")),
                };
                i += 1;
            }
            "--no-names" => plan.names = false,
            "--ui" => plan.full_ui = true,
            "--no-sheet" => plan.sheet = false,
            "--size" => {
                plan.size = parse_size(&value(i, a)?)?;
                i += 1;
            }
            "--out" => {
                plan.out = Some(PathBuf::from(value(i, a)?));
                i += 1;
            }
            _ if a.starts_with("--") => return Err(format!("모르는 옵션: {a}")),
            _ => positional.push(a.to_owned()),
        }
        i += 1;
    }
    match positional.len() {
        1 => plan.tak = PathBuf::from(&positional[0]),
        0 => return Err(".tak 파일 경로가 필요해요".into()),
        _ => return Err(format!(".tak 파일은 하나만: {positional:?}")),
    }
    Ok(Some(plan))
}

use tak_core::tak::repo_root;

/// Absolute form of `p` (relative paths are taken from the current folder).
pub fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Check both the requested destination and an existing link's target.
pub fn check_output_path(path: &Path) -> std::io::Result<()> {
    use tak_core::{tak, takm};
    let target = takm::canonical_target(path)?;
    if [path, &target].iter().any(|p| tak::is_take_path(p) || takm::is_takm_path(p)) {
        return Err(std::io::Error::other("TAK나 TAKM 경로에는 저장하지 않아요."));
    }
    if takm::in_input(path) {
        return Err(std::io::Error::other("input/ 폴더 안에는 저장하지 않아요."));
    }
    Ok(())
}

/// Refuse output folders inside the repository's `input/` (originals are read-only; the same
/// rule as the comment files, `tak_core::tak::in_repo_input`).
pub fn check_out_dir(out: &Path, tak: &Path) -> Result<(), String> {
    check_output_path(out).map_err(|e| format!("{} ({e})", out.display()))?;
    let roots = [repo_root(tak), out.ancestors().find(|a| a.exists()).and_then(repo_root)];
    for root in roots.into_iter().flatten() {
        if tak_core::tak::in_repo_input(out, &root) {
            return Err(format!("input/ 안에는 저장하지 않아요: {}", out.display()));
        }
    }
    Ok(())
}

/// Default output folder: `<repo>/experiments/tak_captures/<time>_<file stem>`, with the repo
/// found above the TAK, else above the current folder.
pub fn default_out_dir(tak: &Path, stamp: &str) -> Result<PathBuf, String> {
    let root = repo_root(tak)
        .or_else(|| std::env::current_dir().ok().as_deref().and_then(repo_root));
    Ok(default_out_dir_for_root(tak, stamp, root.as_deref(), &std::env::temp_dir()))
}

fn default_out_dir_for_root(tak: &Path, stamp: &str, root: Option<&Path>, temp: &Path) -> PathBuf {
    let stem = tak.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "take".into());
    let base = match root {
        Some(root) => root.join("experiments").join("tak_captures"),
        None => temp.join("tak_viewer").join("captures"),
    };
    base.join(format!("{stamp}_{stem}"))
}

/// Make a new, empty folder `base`, or `base_2`, `base_3`, … when taken. The default folder
/// name only has seconds, and several viewers may capture the same file at once; making the
/// folder is the claim (it fails if the folder exists), so two runs never share one.
pub fn new_dir(base: &Path) -> Result<PathBuf, String> {
    let fail = |p: &Path, e: std::io::Error| format!("폴더를 만들 수 없어요: {} ({e})", p.display());
    if let Some(parent) = base.parent() {
        std::fs::create_dir_all(parent).map_err(|e| fail(parent, e))?;
    }
    let name = base.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for i in 1..=1000 {
        let p = if i == 1 { base.to_path_buf() } else { base.with_file_name(format!("{name}_{i}")) };
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(fail(&p, e)),
        }
    }
    Err(format!("같은 이름 폴더가 너무 많아요: {}", base.display()))
}

/// The wall clock now: [year, month, day, hour, minute, second] in local time and the local
/// offset from UTC in minutes. Without a local clock (not Windows): UTC and None.
pub fn local_clock() -> ([i64; 6], Option<i32>) {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct SystemTime {
            year: u16,
            month: u16,
            day_of_week: u16,
            day: u16,
            hour: u16,
            minute: u16,
            second: u16,
            milliseconds: u16,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetLocalTime(st: *mut SystemTime);
            fn GetSystemTime(st: *mut SystemTime);
        }
        let zero = || SystemTime { year: 0, month: 0, day_of_week: 0, day: 0, hour: 0, minute: 0, second: 0, milliseconds: 0 };
        let (mut local, mut utc) = (zero(), zero());
        // SAFETY: both calls only fill the struct we pass.
        unsafe {
            GetLocalTime(&mut local);
            GetSystemTime(&mut utc);
        }
        let civil = |s: &SystemTime| [s.year, s.month, s.day, s.hour, s.minute, s.second].map(i64::from);
        let (local, utc) = (civil(&local), civil(&utc));
        // The two calls may straddle a second: offsets are whole minutes.
        let diff = tak_core::comments::unix_from_civil(local) - tak_core::comments::unix_from_civil(utc);
        (local, Some((diff as f64 / 60.0).round() as i32))
    }
    #[cfg(not(windows))]
    {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
        (tak_core::comments::civil_from_unix(secs), None)
    }
}

/// Local time `YYYYMMDD-HHMMSS` (UTC with a `Z` suffix when the local clock is unavailable).
pub fn local_stamp() -> String {
    let (c, offset) = local_clock();
    let z = if offset.is_none() { "Z" } else { "" };
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}{z}", c[0], c[1], c[2], c[3], c[4], c[5])
}

/// Local time with its offset, ISO 8601 (`2026-11-13T14:05:09+09:00`; UTC `+00:00` without a
/// local clock): the `time` column of comment files.
pub fn local_iso_time() -> String {
    let (c, offset) = local_clock();
    tak_core::comments::iso_time(c, offset.unwrap_or(0))
}

/// true when the take moves more along X than along Z (walking direction for gait).
pub fn walk_along_x(data: &ViewData) -> bool {
    data.bounds.map_or(true, |(lo, hi)| (hi.x - lo.x) >= (hi.z - lo.z))
}

/// Box for the camera to frame (floor y = 0 included), or None when nothing is visible.
pub fn fit_bounds(data: &ViewData, frame: i64, fit: Fit, selected: &[usize]) -> Option<(Vec3, Vec3)> {
    let bbox = |pts: &[Vec3], pad: f32| -> Option<(Vec3, Vec3)> {
        let first = *pts.first()?;
        let (mut lo, mut hi) = (first, first);
        for p in pts {
            lo = v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
            hi = v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
        }
        Some((v3(lo.x - pad, lo.y.min(0.0), lo.z - pad), v3(hi.x + pad, hi.y + pad, hi.z + pad)))
    };
    let present = |keep: &dyn Fn(usize) -> bool| -> Vec<Vec3> {
        data.markers.iter().enumerate().filter(|(i, _)| keep(*i)).filter_map(|(_, m)| m.position(frame)).collect()
    };
    let take = || data.bounds.map(|(lo, hi)| (v3(lo.x, lo.y.min(0.0), lo.z), hi));
    let frame_box = || {
        let labeled = present(&|i| !matches!(data.markers[i].kind, MarkerKind::Unlabeled | MarkerKind::Unknown));
        let pts = if labeled.len() >= 3 { labeled } else { present(&|_| true) };
        bbox(&pts, 0.15).or_else(take)
    };
    match fit {
        Fit::Take => take(),
        Fit::Frame => frame_box(),
        Fit::Selected => bbox(&present(&|i| selected.contains(&i)), 0.35).or_else(frame_box),
    }
}

/// One shot's facts for `captures.json` (positions in mm).
pub fn shot_record(data: &ViewData, frame: i64, view: ViewPreset, file: &str, cam: &Camera, selected: &[usize]) -> Value {
    let count = |k: MarkerKind, present: bool| {
        data.markers.iter().filter(|m| m.kind == k && (!present || m.present(frame))).count()
    };
    let missing: Vec<&str> = data
        .markers
        .iter()
        .filter(|m| m.kind == MarkerKind::Template && !m.present(frame))
        .map(|m| m.name.as_str())
        .collect();
    let r2 = |v: f32| (f64::from(v) * 100.0).round() / 100.0;
    let mm = |p: Vec3| json!([r2(p.x * 1000.0), r2(p.y * 1000.0), r2(p.z * 1000.0)]);
    let sel: Vec<Value> = selected
        .iter()
        .map(|&i| {
            let m = &data.markers[i];
            json!({"name": m.name, "kind": m.kind.as_str(), "present": m.present(frame), "xyz_mm": m.position(frame).map(mm)})
        })
        .collect();
    let rate = if data.take.frame_rate > 0.0 { data.take.frame_rate } else { 100.0 };
    json!({
        "file": file,
        "frame": frame,
        "time_s": ((frame - data.start_frame) as f64 / rate * 1000.0).round() / 1000.0,
        "view": view.key(),
        "view_label": view.label(),
        "camera": {
            "pivot_mm": mm(cam.pivot),
            "yaw_deg": r2(cam.yaw.to_degrees()),
            "pitch_deg": r2(cam.pitch.to_degrees()),
            "distance_m": r2(cam.distance),
            "fov_deg": r2(cam.fov_y.to_degrees()),
        },
        "markers": {
            "template_present": count(MarkerKind::Template, true),
            "template_total": count(MarkerKind::Template, false),
            "extra_present": count(MarkerKind::SkeletonExtra, true) + count(MarkerKind::OtherParent, true),
            "unlabeled_present": count(MarkerKind::Unlabeled, true),
            "unknown_present": count(MarkerKind::Unknown, true),
        },
        "missing_template": missing,
        "selected": sel,
    })
}

/// RGBA8 image.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn crop(&self, x0: usize, y0: usize, w: usize, h: usize) -> Image {
        let x0 = x0.min(self.w);
        let y0 = y0.min(self.h);
        let w = w.min(self.w - x0);
        let h = h.min(self.h - y0);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in y0..y0 + h {
            let row = (y * self.w + x0) * 4;
            rgba.extend_from_slice(&self.rgba[row..row + w * 4]);
        }
        Image { w, h, rgba }
    }

    /// Box-filter shrink to at most `max_w` wide (aspect kept); unchanged when already small.
    pub fn shrink(&self, max_w: usize) -> Image {
        if self.w <= max_w || self.w == 0 || self.h == 0 {
            return self.clone();
        }
        let nw = max_w.max(1);
        let nh = ((self.h * nw) as f64 / self.w as f64).round().max(1.0) as usize;
        let mut rgba = vec![0u8; nw * nh * 4];
        for y in 0..nh {
            let (sy0, sy1) = (y * self.h / nh, ((y + 1) * self.h / nh).max(y * self.h / nh + 1));
            for x in 0..nw {
                let (sx0, sx1) = (x * self.w / nw, ((x + 1) * self.w / nw).max(x * self.w / nw + 1));
                let mut acc = [0u32; 4];
                for sy in sy0..sy1 {
                    for sx in sx0..sx1 {
                        let k = (sy * self.w + sx) * 4;
                        for c in 0..4 {
                            acc[c] += self.rgba[k + c] as u32;
                        }
                    }
                }
                let n = ((sy1 - sy0) * (sx1 - sx0)) as u32;
                let k = (y * nw + x) * 4;
                for c in 0..4 {
                    rgba[k + c] = (acc[c] / n) as u8;
                }
            }
        }
        Image { w: nw, h: nh, rgba }
    }
}

/// Grid of images (row-major, `cols` per row) on a dark background.
pub fn sheet(cells: &[Image], cols: usize, gap: usize) -> Option<Image> {
    if cells.is_empty() {
        return None;
    }
    let cols = cols.clamp(1, cells.len());
    let rows = cells.len().div_ceil(cols);
    let cw = cells.iter().map(|c| c.w).max()?;
    let ch = cells.iter().map(|c| c.h).max()?;
    let w = cols * cw + (cols + 1) * gap;
    let h = rows * ch + (rows + 1) * gap;
    let mut rgba = [0x10u8, 0x10, 0x10, 0xff].repeat(w * h);
    for (k, c) in cells.iter().enumerate() {
        let (r, col) = (k / cols, k % cols);
        let (x0, y0) = (gap + col * (cw + gap), gap + r * (ch + gap));
        for y in 0..c.h {
            let dst = ((y0 + y) * w + x0) * 4;
            rgba[dst..dst + c.w * 4].copy_from_slice(&c.rgba[y * c.w * 4..(y + 1) * c.w * 4]);
        }
    }
    Some(Image { w, h, rgba })
}

pub fn write_png(path: &Path, [w, h]: [usize; 2], color: png::ColorType, pixels: &[u8]) -> std::io::Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w as u32, h as u32);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    let to_io = |e: png::EncodingError| std::io::Error::new(std::io::ErrorKind::Other, e.to_string());
    let mut writer = enc.write_header().map_err(to_io)?;
    writer.write_image_data(pixels).map_err(to_io)?;
    writer.finish().map_err(to_io)
}

/// Size and modification time, to show the TAK did not change.
pub fn stat_key(p: &Path) -> Option<(u64, std::time::SystemTime)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// State of one capture run, driven frame by frame by the app.
pub struct CaptureRun {
    pub plan: CapturePlan,
    pub out_dir: Option<PathBuf>,
    pub shots: Vec<(i64, ViewPreset)>,
    pub index: usize,
    pub stage: u32,
    pub started: bool,
    pub selected: Vec<usize>,
    pub cells: Vec<Image>,
    pub records: Vec<Value>,
    pub notes: Vec<String>,
    pub output_failed: bool,
    pub pending_image: Option<PendingImage>,
    pub tak_before: Option<(u64, std::time::SystemTime)>,
    /// Menu capture: camera to restore afterwards.
    pub restore_cam: Option<Camera>,
    pub restore_frame: Option<i64>,
}

pub struct PendingImage {
    pub record: Option<Value>,
    pub result: std::sync::mpsc::Receiver<Result<Image, String>>,
}

impl CaptureRun {
    pub fn new(plan: CapturePlan) -> Self {
        CaptureRun {
            plan,
            out_dir: None,
            shots: vec![],
            index: 0,
            stage: 0,
            started: false,
            selected: vec![],
            cells: vec![],
            records: vec![],
            notes: vec![],
            output_failed: false,
            pending_image: None,
            tak_before: None,
            restore_cam: None,
            restore_frame: None,
        }
    }

    pub fn hud_label(&self) -> String {
        self.shots.get(self.index).map_or("Perspective", |s| s.1.label()).to_owned()
    }

    /// Shots after the take is loaded: frames x views, capped at MAX_SHOTS.
    pub fn plan_shots(&mut self, first: i64, last: i64) {
        let frames = self.plan.frames.resolve(first, last);
        self.shots = frames.iter().flat_map(|&f| self.plan.views.iter().map(move |&v| (f, v))).take(MAX_SHOTS).collect();
        if frames.len() * self.plan.views.len() > MAX_SHOTS {
            self.notes.push(format!("장면이 많아 앞의 {MAX_SHOTS}장만 찍었어요"));
        }
    }

    pub fn file_name(&self, frame: i64, view: ViewPreset) -> String {
        shot_file_name(frame, view)
    }

    pub fn image_result(&mut self, result: Result<Image, String>, record: Option<Value>) {
        match result {
            Ok(cell) => {
                self.records.extend(record);
                self.cells.push(cell);
            }
            Err(error) => {
                self.output_failed = true;
                self.notes.push(error);
            }
        }
    }

    /// Write sheet.png and captures.json; returns the lines to print.
    pub fn finish(&mut self, data: &ViewData, wsl_path: Option<String>) -> Vec<String> {
        let Some(out) = self.out_dir.clone() else { return vec![] };
        let mut lines = Vec::new();
        let mut sheet_name = Value::Null;
        if self.plan.sheet {
            let cols = self.plan.views.len().clamp(1, 6);
            if let Some(img) = sheet(&self.cells, cols, 6) {
                let p = out.join("sheet.png");
                match save_image(&p, &img, self.plan.exit_when_done) {
                    Ok(()) => sheet_name = json!("sheet.png"),
                    Err(error) => {
                        self.output_failed = true;
                        self.notes.push(error);
                    }
                }
            }
        }
        let after = stat_key(&data.take.path);
        let unchanged = self.tak_before.is_some() && after == self.tak_before;
        let doc = json!({
            "tool": "tak_viewer --capture (read-only)",
            "tak": data.take.path.to_string_lossy(),
            "tak_wsl": wsl_path,
            "tak_unchanged": unchanged,
            "frame_rate": data.take.frame_rate,
            "start_frame": data.start_frame,
            "end_frame": data.end_frame,
            "units": "positions in mm (file values converted to metres first when they look like mm)",
            "views": self.plan.views.iter().map(|v| v.key()).collect::<Vec<_>>(),
            "fit": format!("{:?}", self.plan.fit).to_lowercase(),
            "select": self.plan.select,
            "sheet": sheet_name,
            "notes": self.notes,
            "shots": self.records,
        });
        let json_path = out.join("captures.json");
        let write_json = || {
            let json = serde_json::to_vec_pretty(&doc)?;
            if self.plan.exit_when_done {
                check_output_path(&json_path)?;
            }
            std::fs::write(&json_path, &json)?;
            if self.plan.exit_when_done && std::fs::metadata(&json_path)?.len() != json.len() as u64 {
                return Err(std::io::Error::other("captures.json 크기가 달라요."));
            }
            Ok::<_, std::io::Error>(())
        };
        match write_json() {
            Ok(()) => {}
            Err(e) => {
                self.output_failed = true;
                lines.push(format!("captures.json 저장 실패: {e}"));
            }
        }
        lines.push(format!("capture: {} shots -> {}", self.records.len(), out.display()));
        if let Some(w) = crate::workspace::to_wsl_path(&out) {
            lines.push(format!("capture (WSL): {w}"));
        }
        lines.extend(self.notes.iter().map(|n| format!("note: {n}")));
        if !unchanged {
            lines.push("warning: TAK size/mtime changed during capture (another program?)".into());
        }
        lines
    }
}

/// CLI output gets read back on the worker. Menu capture keeps its existing write path.
pub fn save_image(path: &Path, img: &Image, verify: bool) -> Result<(), String> {
    let work = || {
        if verify {
            check_output_path(path)?;
        }
        let rgb: Vec<u8> = img.rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        let size = [img.w, img.h];
        write_png(path, size, png::ColorType::Rgb, &rgb)?;
        if verify {
            crate::shot_output::verify_png(path, size)?;
        }
        Ok::<_, std::io::Error>(())
    };
    work().map_err(|e| format!("{} 저장 실패: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parse_full_command() {
        let p = parse(&args("--capture x.tak --frames 100:300:100 --views top,side,down,+x --select LKNE,RKNE --trail 50 --fit selected --no-names --size 1200x800 --out D"))
            .unwrap()
            .unwrap();
        assert_eq!(p.tak, PathBuf::from("x.tak"));
        assert_eq!(p.frames, FrameSpec::Range(100, 300, 100));
        assert_eq!(p.views, vec![ViewPreset::Top, ViewPreset::Side, ViewPreset::Bottom, ViewPreset::PlusX]);
        assert_eq!(p.select, vec!["LKNE", "RKNE"]);
        assert_eq!((p.trail, p.fit, p.names, p.size), (Some(50), Fit::Selected, false, [1200.0, 800.0]));
        assert_eq!(p.out, Some(PathBuf::from("D")));
        assert!(parse(&args("x.tak --frame 3")).unwrap().is_none());
        assert!(parse(&args("--capture")).is_err());
        assert!(parse(&args("--capture x.tak --views sky")).is_err());
        assert!(parse(&args("--capture x.tak --bogus")).is_err());
    }

    #[test]
    fn frames_resolve_inside_the_take() {
        assert_eq!(FrameSpec::parse("even:3").unwrap().resolve(0, 100), vec![0, 50, 100]);
        assert_eq!(FrameSpec::parse("even:1").unwrap().resolve(10, 20), vec![15]);
        assert_eq!(FrameSpec::parse("5,-3,500,5").unwrap().resolve(0, 100), vec![0, 5, 100]);
        assert_eq!(FrameSpec::parse("90:130:20").unwrap().resolve(0, 120), vec![90, 110]);
        assert_eq!(FrameSpec::parse("0:10000").unwrap().resolve(0, 10000).len(), MAX_FRAMES);
        assert!(FrameSpec::parse("even:0").is_err());
        assert!(FrameSpec::parse("9:3").is_err());
        assert!(FrameSpec::parse("a,b").is_err());
    }

    #[test]
    fn side_view_looks_across_the_walking_direction() {
        // Walking along X: side camera on +Z (yaw 0), front camera on +X (yaw 90°).
        let (yaw, pitch, _) = ViewPreset::Side.angles(true).unwrap();
        assert_eq!((yaw, pitch), (0.0, 0.0));
        assert!((ViewPreset::Front.angles(true).unwrap().0 - 90f32.to_radians()).abs() < 1e-6);
        // Walking along Z: swapped.
        assert!((ViewPreset::Side.angles(false).unwrap().0 - 90f32.to_radians()).abs() < 1e-6);
        assert!(ViewPreset::Top.angles(true).unwrap().1 > 88f32.to_radians());
        assert!(ViewPreset::Bottom.angles(true).unwrap().1 < -88f32.to_radians());
        assert!(ViewPreset::Persp.angles(true).is_none());
    }

    #[test]
    fn input_folder_is_refused() {
        let _temp = tak_core::test_support::TempDir::new("tak_capture_test");
        let root = _temp.join(format!("tak_capture_test_{}", std::process::id()));
        std::fs::create_dir_all(root.join("apps")).unwrap();
        std::fs::create_dir_all(root.join("input").join("raw")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "x").unwrap();
        std::fs::write(root.join("apps").join("AGENTS.md"), "x").unwrap();
        let tak = root.join("input").join("raw").join("a.tak");
        assert!(check_out_dir(&root.join("input").join("raw").join("shots"), &tak).is_err());
        assert!(check_out_dir(&root.join("experiments").join("tak_captures").join("x"), &tak).is_ok());
        let out = default_out_dir(&tak, "20261110-010203").unwrap();
        assert_eq!(out, root.join("experiments").join("tak_captures").join("20261110-010203_a"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn standalone_root_requires_exact_marker_and_protects_input() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("tak_standalone_test_{}_{unique}", std::process::id()));
        std::fs::create_dir_all(root.join("input").join("raw")).unwrap();
        let marker = root.join(".tak-viewer-root");
        let tak = root.join("input").join("raw").join("a.tak");
        for invalid in ["", "tak-viewer", "tak-viewer\r\n", "other\n", "tak-viewer\nextra"] {
            std::fs::write(&marker, invalid).unwrap();
            assert_ne!(repo_root(&tak).as_deref(), Some(root.as_path()));
        }
        std::fs::write(&marker, b"tak-viewer\n").unwrap();
        assert_eq!(repo_root(&tak).as_deref(), Some(root.as_path()));
        assert!(check_out_dir(&root.join("input").join("raw").join("shots"), &tak).is_err());
        assert!(check_out_dir(&root.join("input"), &tak).is_err());
        assert!(check_out_dir(&root.join("output.TAK"), &tak).is_err());
        assert!(check_out_dir(&root.join("output"), &tak).is_ok());
        assert_eq!(default_out_dir(&tak, "stamp").unwrap(), root.join("experiments/tak_captures/stamp_a"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn default_capture_outside_roots_uses_temp_folder() {
        assert_eq!(
            default_out_dir_for_root(Path::new("/outside/example.tak"), "stamp", None, Path::new("/temp")),
            Path::new("/temp/tak_viewer/captures/stamp_example")
        );
    }

    #[test]
    fn two_captures_in_the_same_second_get_their_own_folders() {
        let _temp = tak_core::test_support::TempDir::new("tak_capture_new_dir");
        let root = _temp.join(format!("tak_capture_new_dir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let base = root.join("tak_captures").join("20261110-010203_a");
        assert_eq!(new_dir(&base).unwrap(), base, "first run: the plain name (parents made)");
        assert_eq!(new_dir(&base).unwrap(), root.join("tak_captures").join("20261110-010203_a_2"));
        assert_eq!(new_dir(&base).unwrap(), root.join("tak_captures").join("20261110-010203_a_3"));
        assert!(base.is_dir());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn crop_shrink_and_sheet() {
        let img = Image { w: 4, h: 2, rgba: (0..32).collect() };
        let c = img.crop(1, 1, 2, 5);
        assert_eq!((c.w, c.h), (2, 1));
        assert_eq!(c.rgba, (20..28).collect::<Vec<u8>>());
        let big = Image { w: 4, h: 4, rgba: [10u8, 20, 30, 255].repeat(16) };
        let s = big.shrink(2);
        assert_eq!((s.w, s.h), (2, 2));
        assert_eq!(&s.rgba[..4], &[10, 20, 30, 255]);
        let sh = sheet(&[s.clone(), s.clone(), s], 2, 1).unwrap();
        assert_eq!((sh.w, sh.h), (2 * 2 + 3, 2 * 2 + 3));
    }
}
