//! Screenshot output options and rectangles from the frame sent to the renderer.

use crate::{capture, shot, workspace};
use eframe::egui::{Rect, pos2, vec2};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{self, BufReader};
use std::path::Path;
use std::sync::mpsc;

pub const USAGE: &str =
    "tak_viewer [--headless] --screenshot OUT.png [options] <file.tak|file.takm> …
  --size WxH          창 크기 (포인트, 400x300 ~ 8000x8000, 기본 1600x960)
  --crop x,y,w,h      창 왼쪽 위 기준 자르기 (포인트)
  --crop-panel NAME   보이는 영역 자르기. 영역 이름:
    EXPLORER, Labeling, Edit Tools, Selected Marker (선택 마커), Devices, COMMENTS, GRAPH,
    Layout Tabs, Settings, footer, timeline, timeline N, 3D view N (N = 1~4)
  --scale N           자른 그림을 정수 배로 확대
  --crop-only         전체 PNG 생략. 자른 그림은 OUT.crop.png
  --regions           영역 좌표를 OUT.regions.json에 저장
  --frame N   --select A,B   --select-visible   --plate INDEX|NAME   --range A-B
  --overlay FILE (repeat)   --hide 1|2   --hover NAME
  --comments   --pick-comment ID   --edit-comment ID[:markers|frames|comment|type]   --type-strip ID
  --edit-tools   --edit-pattern   --edit-split   --edit-bottom   --menu-capture
  --shot-update menu|ready|progress|restart (headless screenshot only)
종료 코드: 0 완료, 1 실패, 2 인자 오류.";

const PANELS: &[&str] = &[
    "EXPLORER",
    "Labeling",
    "Edit Tools",
    "Selected Marker",
    "Devices",
    "COMMENTS",
    "GRAPH",
    "Layout Tabs",
    "Settings",
    "footer",
    "timeline",
    "timeline 1",
    "timeline 2",
    "timeline 3",
    "timeline 4",
    "3D view 1",
    "3D view 2",
    "3D view 3",
    "3D view 4",
];
const MAX_PIXELS: usize = 8000 * 8000;

#[derive(Clone, Debug)]
enum Crop {
    Points(Rect),
    Panel(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkerOrderPane {
    On,
    Menu,
    Off,
    Settings,
}

#[derive(Clone, Debug)]
pub(crate) struct MarkerOrderPreview {
    pub pane: MarkerOrderPane,
    pub names: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub size: Option<[f32; 2]>,
    pub(crate) marker_order: Option<MarkerOrderPreview>,
    pub(crate) update_preview: Option<crate::update::Preview>,
    crop: Option<Crop>,
    scale: usize,
    crop_only: bool,
    regions: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            size: None,
            marker_order: None,
            update_preview: None,
            crop: None,
            scale: 1,
            crop_only: false,
            regions: false,
        }
    }
}

impl Options {
    pub fn needs_regions(&self) -> bool {
        self.regions || matches!(self.crop, Some(Crop::Panel(_)))
    }
}

fn parse_rect(value: &str) -> Option<Rect> {
    let mut parts = value.split(',').map(|s| s.trim().parse::<f32>());
    let [x, y, w, h] =
        std::array::from_fn(|_| parts.next().and_then(Result::ok).unwrap_or(f32::NAN));
    let rect = Rect::from_min_size(pos2(x, y), vec2(w, h));
    (parts.next().is_none() && rect.is_finite() && rect.is_positive()).then_some(rect)
}

fn panel_name(value: &str) -> Option<&'static str> {
    let value = value.trim();
    if value == "선택 마커" {
        return Some("Selected Marker");
    }
    PANELS
        .iter()
        .copied()
        .find(|name| name.eq_ignore_ascii_case(value))
}

/// Peel off the new screenshot options before parsing the existing shot options.
pub fn parse(args: &[String]) -> Result<(Options, Vec<String>), String> {
    let mut options = Options::default();
    let mut rest = Vec::new();
    let mut input = args.iter();
    let mut shot = false;
    let mut used = false;
    let mut scaled = false;
    while let Some(arg) = input.next() {
        let value = |arg: &str, input: &mut std::slice::Iter<'_, String>| {
            input
                .next()
                .filter(|v| !v.starts_with("--") && !v.trim().is_empty())
                .cloned()
                .ok_or_else(|| format!("{arg} 뒤에 값이 필요해요."))
        };
        match arg.as_str() {
            "--screenshot" => {
                shot = true;
                rest.extend([arg.clone(), value(arg, &mut input)?]);
                continue;
            }
            "--size" => options.size = Some(capture::parse_size(&value(arg, &mut input)?)?),
            "--crop" | "--crop-panel" => {
                if options.crop.is_some() {
                    return Err("--crop과 --crop-panel 중 한 가지를 한 번만 써 주세요.".into());
                }
                let v = value(arg, &mut input)?;
                options.crop =
                    Some(if arg == "--crop" {
                        Crop::Points(parse_rect(&v).ok_or(
                            "--crop에는 x,y,w,h를 써 주세요. 폭과 높이는 0보다 커야 해요.",
                        )?)
                    } else {
                        Crop::Panel(
                            panel_name(&v).ok_or_else(|| format!("모르는 화면 영역이에요. {v}"))?,
                        )
                    });
            }
            "--scale" => {
                scaled = true;
                options.scale = value(arg, &mut input)?
                    .parse()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or("--scale에는 1 이상의 정수를 써 주세요.")?;
            }
            "--shot-marker-order" => {
                let v = value(arg, &mut input)?;
                let (pane, names) = match v.split_once(':') {
                    Some(("settings", names)) => (MarkerOrderPane::Settings, names),
                    Some(("menu", names)) => (MarkerOrderPane::Menu, names),
                    Some(("off", names)) => (MarkerOrderPane::Off, names),
                    _ => (MarkerOrderPane::On, v.as_str()),
                };
                let mut names: Vec<_> = names.split(',').map(str::to_owned).collect();
                crate::marker_order::clean(&mut names);
                if names.is_empty() {
                    return Err("--shot-marker-order 뒤에 마커 이름을 하나 이상 써 주세요.".into());
                }
                options.marker_order = Some(MarkerOrderPreview { pane, names });
            }
            "--shot-update" => {
                options.update_preview = Some(crate::update::Preview::parse(&value(arg, &mut input)?)?);
            }
            "--crop-only" => options.crop_only = true,
            "--regions" => options.regions = true,
            _ => {
                rest.push(arg.clone());
                continue;
            }
        }
        used = true;
    }
    if used && !shot {
        return Err("그림 옵션은 --screenshot과 함께 써 주세요.".into());
    }
    if options.crop.is_none() && (scaled || options.crop_only) {
        return Err("--scale과 --crop-only는 --crop 또는 --crop-panel과 함께 써 주세요.".into());
    }
    Ok((options, rest))
}

/// Rectangles are collected in the frame that requests the screenshot.
#[derive(Default)]
pub struct Regions {
    rects: BTreeMap<String, Rect>,
    collecting: bool,
    pixels_per_point: Option<f32>,
}

impl Regions {
    pub fn begin(pixels_per_point: f32) -> Self {
        Self {
            collecting: true,
            pixels_per_point: Some(pixels_per_point),
            ..Self::default()
        }
    }

    pub fn record(&mut self, name: impl Into<String>, rect: Rect) {
        if self.collecting {
            self.rects.insert(name.into(), rect);
        }
    }

    pub fn freeze(&mut self) {
        self.collecting = false;
    }
}

/// Round outwards to cover all painted pixels, then clip against the image.
fn pixel_rect(rect: Rect, ppp: f32, [w, h]: [usize; 2]) -> Option<[usize; 4]> {
    if !rect.is_finite() || !rect.is_positive() {
        return None;
    }
    let left = ((rect.min.x * ppp).floor() as usize).min(w);
    let top = ((rect.min.y * ppp).floor() as usize).min(h);
    let right = ((rect.max.x * ppp).ceil() as usize).min(w);
    let bottom = ((rect.max.y * ppp).ceil() as usize).min(h);
    (right > left && bottom > top).then(|| [left, top, right - left, bottom - top])
}

fn scaled_size(w: usize, h: usize, scale: usize) -> Result<[usize; 2], String> {
    let size = w
        .checked_mul(scale)
        .zip(h.checked_mul(scale))
        .filter(|&(w, h)| w.checked_mul(h).is_some_and(|n| n <= MAX_PIXELS));
    size.map(|(w, h)| [w, h])
        .ok_or_else(|| "확대한 그림이 너무 커요. --scale을 줄여 주세요.".into())
}

fn crop_pixels(
    rgba: &[u8],
    stride: usize,
    [x, y, w, h]: [usize; 4],
    scale: usize,
    [sw, sh]: [usize; 2],
) -> Vec<u8> {
    let mut result = Vec::with_capacity(sw * sh * 4);
    for row in y..y + h {
        let start = (row * stride + x) * 4;
        let pixels = &rgba[start..start + w * 4];
        let dest = result.len();
        if scale == 1 {
            result.extend_from_slice(pixels);
        } else {
            for pixel in pixels.chunks_exact(4) {
                for _ in 0..scale {
                    result.extend_from_slice(pixel);
                }
            }
            for _ in 1..scale {
                result.extend_from_within(dest..dest + sw * 4);
            }
        }
    }
    result
}

/// Reopen the output and decode through IEND. Read rows without allocating a second image.
pub fn verify_png(path: &Path, [w, h]: [usize; 2]) -> io::Result<()> {
    let file = std::fs::File::open(path)?;
    let mut reader = png::Decoder::new(BufReader::new(file)).read_info()?;
    let actual = reader.info().size();
    if actual != (w as u32, h as u32) {
        return Err(io::Error::other(format!(
            "PNG 크기가 {}x{}예요. 필요한 크기는 {w}x{h}예요.",
            actual.0, actual.1
        )));
    }
    while reader.next_row()?.is_some() {}
    reader.finish()?;
    Ok(())
}

#[derive(Serialize)]
struct RegionFile {
    size: [usize; 2],
    pixels_per_point: f32,
    regions: BTreeMap<String, [usize; 4]>,
}

pub fn write(
    shot: &shot::AutoShot,
    image: &eframe::egui::ColorImage,
    ppp: f32,
    regions: Regions,
) -> Result<(), String> {
    let options = &shot.output;
    let [w, h] = image.size;
    let ppp = regions.pixels_per_point.unwrap_or(ppp);
    if let Some(size) = options.size {
        let [expected_w, expected_h] = size.map(|pt| (pt * ppp).round() as usize);
        if [w, h] != [expected_w, expected_h] {
            return Err(format!(
                "화면이 {w}x{h} 픽셀로 찍혔어요. 요청한 크기는 {expected_w}x{expected_h} 픽셀이에요."
            ));
        }
    }
    let rects: BTreeMap<_, _> = regions
        .rects
        .into_iter()
        .filter_map(|(name, rect)| Some((name, pixel_rect(rect, ppp, [w, h])?)))
        .collect();
    let out = capture::absolute(&shot.out);
    let crop = options
        .crop
        .as_ref()
        .map(|crop| {
            let rect = match crop {
                Crop::Points(rect) => pixel_rect(*rect, ppp, [w, h])
                    .ok_or("자를 영역이 그림 밖에 있어요.".to_owned())?,
                Crop::Panel(name) => *rects
                    .get(*name)
                    .ok_or_else(|| format!("그림에 {name} 영역이 보이지 않아요."))?,
            };
            let size = scaled_size(rect[2], rect[3], options.scale)?;
            Ok::<_, String>((out.with_extension("crop.png"), rect, size))
        })
        .transpose()?;
    let sidecar = options.regions.then(|| out.with_extension("regions.json"));
    let check = |path: &Path, result: io::Result<()>| {
        result.map_err(|e| format!("저장한 파일을 확인할 수 없어요. {} ({e})", path.display()))
    };
    // Check every destination before writing any, even when only the sibling crop is requested.
    for path in std::iter::once(out.as_path())
        .chain(crop.iter().map(|(path, _, _)| path.as_path()))
        .chain(sidecar.as_deref())
    {
        check(path, capture::check_output_path(path))?;
    }
    let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
    if !options.crop_only {
        check(
            &out,
            shot::save_png(&out, w, h, &rgba).and_then(|_| verify_png(&out, [w, h])),
        )?;
        print_saved(&out, Some([w, h]));
    }
    if let Some((path, rect, size)) = crop {
        let pixels = crop_pixels(&rgba, w, rect, options.scale, size);
        check(
            &path,
            capture::write_png(&path, size, png::ColorType::Rgba, &pixels)
                .and_then(|_| verify_png(&path, size)),
        )?;
        print_saved(&path, Some(size));
    }
    if let Some(path) = sidecar {
        let json = serde_json::to_vec_pretty(&RegionFile {
            size: [w, h],
            pixels_per_point: ppp,
            regions: rects,
        })
        .map_err(|error| error.to_string())?;
        check(&path, std::fs::write(&path, json))?;
        print_saved(&path, None);
    }
    Ok(())
}

/// Only command runs use this worker. Wake the event loop after posting the result.
pub fn spawn<T: Send + 'static>(
    ctx: &eframe::egui::Context,
    work: impl FnOnce() -> T + Send + 'static,
) -> mpsc::Receiver<T> {
    let (sender, receiver) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = sender.send(work());
        ctx.request_repaint();
    });
    receiver
}

fn print_saved(path: &Path, size: Option<[usize; 2]>) {
    let dims = size.map_or_else(String::new, |[w, h]| format!(" ({w}x{h})"));
    println!("screenshot saved: {}{dims}", path.display());
    if let Some(wsl) = workspace::to_wsl_path(path) {
        println!("screenshot (WSL): {wsl}{dims}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Color32, ColorImage};
    use tak_core::test_support::TempDir;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn size_is_shared_with_capture_and_never_a_file() {
        let (shot, files) = shot::parse_command(&args(&[
            "--size",
            "1400X860",
            "--screenshot",
            "out.png",
            "a.tak",
        ]))
        .unwrap();
        let capture = capture::parse(&args(&["--capture", "a.tak", "--size", "1400X860"]))
            .unwrap()
            .unwrap();
        assert_eq!(shot.unwrap().output.size, Some(capture.size));
        assert_eq!(files, ["a.tak"]);
        for bad in [
            "", "1x2", "8010x900", "NaNx600", "700xinf", "1400,860", "--frame",
        ] {
            assert!(
                shot::parse_command(&args(&["--screenshot", "out.png", "--size", bad, "a.tak"]))
                    .is_err()
            );
        }
        assert!(parse(&args(&["--screenshot", "out.png", "--size"])).is_err());
    }

    #[test]
    fn crop_options_reject_unknown_panels_and_invalid_combinations() {
        for panel in PANELS {
            assert_eq!(panel_name(&panel.to_ascii_lowercase()), Some(*panel));
        }
        assert_eq!(panel_name("선택 마커"), Some("Selected Marker"));
        for (flag, value) in [
            ("--crop-panel", "Not a panel"),
            ("--crop-panel", "3D view 5"),
            ("--crop", "1,2,3,4,5"),
            ("--crop", "0,0,10,0"),
            ("--crop", "0,NaN,10,10"),
            ("--scale", "1.5"),
            ("--scale", "0"),
        ] {
            assert!(parse(&args(&["--screenshot", "out.png", flag, value])).is_err());
        }
        assert!(parse(&args(&["--screenshot", "out.png", "--crop-only"])).is_err());
        assert!(
            parse(&args(&[
                "--screenshot",
                "out.png",
                "--crop-panel",
                "Labeling",
                "--crop",
                "0,0,1,1"
            ]))
            .is_err()
        );
        assert!(parse(&args(&["--crop-panel", "COMMENTS"])).is_err());
    }

    #[test]
    fn crop_rounds_outward_clips_and_rejects_empty_rectangles() {
        let rect = |s| parse_rect(s).unwrap();
        assert_eq!(
            pixel_rect(rect("-2.25,1.25,9,5"), 2.0, [10, 8]),
            Some([0, 2, 10, 6])
        );
        assert_eq!(
            pixel_rect(rect("1.25,2.25,2.5,1.5"), 1.5, [10, 8]),
            Some([1, 3, 5, 3])
        );
        assert_eq!(pixel_rect(rect("10,0,2,5"), 1.0, [10, 8]), None);
        assert_eq!(pixel_rect(rect("-8,1,2,5"), 1.0, [10, 8]), None);
        assert_eq!(pixel_rect(Rect::NOTHING, 1.0, [10, 8]), None);
        assert!(scaled_size(8000, 8000, 2).is_err());
        assert!(scaled_size(2, 2, usize::MAX).is_err());
        let rgba: Vec<u8> = (1..=6).flat_map(|v| [v, v, v, 255]).collect();
        let out = crop_pixels(&rgba, 3, [1, 0, 2, 2], 2, [4, 4]);
        let values: Vec<_> = out.chunks_exact(4).map(|p| p[0]).collect();
        assert_eq!(values, [2, 2, 3, 3, 2, 2, 3, 3, 5, 5, 6, 6, 5, 5, 6, 6]);
    }

    #[test]
    fn resolves_output_and_refuses_take_and_input_paths() {
        let (shot, _) =
            shot::parse_command(&args(&["--screenshot", "/mnt/c/temp/그림.png"])).unwrap();
        let out = shot.unwrap().out;
        #[cfg(windows)]
        {
            assert_eq!(out, Path::new(r"C:\temp\그림.png"));
            assert_eq!(
                workspace::to_wsl_path(&out).as_deref(),
                Some("/mnt/c/temp/그림.png")
            );
        }
        #[cfg(not(windows))]
        assert_eq!(out, Path::new("/mnt/c/temp/그림.png"));
        assert_eq!(
            workspace::to_wsl_path(Path::new(r"C:\가나다라마바.png")).as_deref(),
            Some("/mnt/c/가나다라마바.png"),
        );

        let repo = TempDir::new("shot_output_guards");
        std::fs::create_dir(repo.join("input")).unwrap();
        std::fs::create_dir(repo.join("apps")).unwrap();
        std::fs::write(repo.join("AGENTS.md"), "").unwrap();
        std::fs::write(repo.join("apps/AGENTS.md"), "").unwrap();
        for bad in [
            "test.TAK",
            "test.TaKm",
            "input/test.png",
            "input/next/test.png",
            "apps/../input/test.png",
        ] {
            assert!(
                capture::check_output_path(&repo.join(bad)).is_err(),
                "{bad}"
            );
        }
        assert!(capture::check_output_path(&repo.join("test.png")).is_ok());
        #[cfg(unix)]
        {
            std::fs::create_dir(repo.join("input/deeper")).unwrap();
            std::os::unix::fs::symlink(repo.join("input/deeper"), repo.join("shortcut")).unwrap();
            assert!(capture::check_output_path(&repo.join("shortcut/out.png")).is_err());
        }
    }

    #[test]
    fn saved_png_readback_rejects_missing_empty_wrong_size_and_truncated_files() {
        let temp = TempDir::new("shot_validation");
        let path = temp.join("picture.png");
        assert!(verify_png(&path, [2, 1]).is_err());
        std::fs::write(&path, []).unwrap();
        assert!(verify_png(&path, [2, 1]).is_err());
        let png = shot::encode_png(2, 1, &[255; 8]);
        std::fs::write(&path, &png).unwrap();
        verify_png(&path, [2, 1]).unwrap();
        assert!(verify_png(&path, [1, 2]).is_err());
        std::fs::write(&path, &png[..png.len() - 12]).unwrap();
        assert!(verify_png(&path, [2, 1]).is_err());
    }

    #[test]
    fn panel_crop_uses_frozen_regions_and_crop_only_has_no_full_image() {
        let temp = TempDir::new("shot_panel_output");
        let path = temp.join("picture.png");
        let (shot, _) = shot::parse_command(&args(&[
            "--screenshot",
            path.to_str().unwrap(),
            "--crop-panel",
            "comments",
            "--scale",
            "2",
            "--crop-only",
            "--regions",
        ]))
        .unwrap();
        let shot = shot.unwrap();
        let image = ColorImage::new([10, 15], Color32::WHITE);
        assert!(write(&shot, &image, 1.0, Regions::default()).is_err());
        let mut regions = Regions::begin(1.0);
        regions.record("COMMENTS", parse_rect("2,4,6,3").unwrap());
        regions.freeze();
        regions.record("COMMENTS", parse_rect("0,0,1,1").unwrap());
        write(&shot, &image, 1.0, regions).unwrap();
        assert!(!path.exists());
        verify_png(&temp.join("picture.crop.png"), [12, 6]).unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(temp.join("picture.regions.json")).unwrap())
                .unwrap();
        assert_eq!(json["regions"]["COMMENTS"], serde_json::json!([2, 4, 6, 3]));
    }
}
