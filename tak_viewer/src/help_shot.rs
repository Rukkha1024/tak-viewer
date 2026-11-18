use std::path::PathBuf;

pub(crate) const USAGE: &str =
    "tak_viewer --headless --screenshot out.png --shot-help [--shot-help-size WIDTHxHEIGHT[@SCALE]] [--shot-help-bottom]";

pub(crate) struct Request {
    pub out: PathBuf,
    pub pixels: [usize; 2],
    pub scale: f32,
    pub bottom: bool,
}

pub(crate) fn parse(args: &[String]) -> Result<Option<Request>, &'static str> {
    if !args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--shot-help" | "--shot-help-size" | "--shot-help-bottom"))
    {
        return Ok(None);
    }
    let mut headless = false;
    let mut help = false;
    let mut bottom = false;
    let mut out = None;
    let mut size = None;
    let mut args = args.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        match arg {
            "--headless" if !headless => headless = true,
            "--shot-help" if !help => help = true,
            "--shot-help-bottom" if !bottom => bottom = true,
            "--screenshot" if out.is_none() => {
                out = args
                    .next()
                    .filter(|path| !path.is_empty() && !path.starts_with("--"));
                if out.is_none() {
                    return Err(USAGE);
                }
            }
            "--shot-help-size" if size.is_none() => {
                size = Some(args.next().and_then(parse_size).ok_or(
                    "그림 크기는 320x240~4096x4096, 배율은 1~3으로 써 주세요. 예: 1920x1080@1.5",
                )?);
            }
            _ => return Err(USAGE),
        }
    }
    let Some(out) = out.filter(|_| headless && help) else {
        return Err(USAGE);
    };
    let out = crate::workspace::resolve_path(out);
    if tak_core::tak::is_take_path(&out) || tak_core::takm::is_takm_path(&out) {
        return Err("그림 경로에는 .tak 또는 .takm 파일을 쓸 수 없어요.");
    }
    let (pixels, scale) = size.unwrap_or(([1600, 960], 1.0));
    Ok(Some(Request { out, pixels, scale, bottom }))
}

fn parse_size(size: &str) -> Option<([usize; 2], f32)> {
    let (size, scale) = size.split_once('@').unwrap_or((size, "1"));
    let (width, height) = size.split_once('x')?;
    let (width, height, scale) = (
        width.parse().ok()?,
        height.parse().ok()?,
        scale.parse().ok()?,
    );
    ((320..=4096).contains(&width)
        && (240..=4096).contains(&height)
        && (1.0..=3.0).contains(&scale))
    .then_some(([width, height], scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn help_shot_requires_headless_and_protects_take_paths() {
        assert!(parse(&args(&["--headless", "--screenshot", "out.png"]))
            .unwrap()
            .is_none());
        for bad in [
            &["--screenshot", "out.png", "--shot-help"][..],
            &["--headless", "--shot-help"],
            &["--headless", "--screenshot", "sample.TAK", "--shot-help"],
            &["--headless", "--screenshot", "sample.TAKM", "--shot-help"],
            &[
                "--headless",
                "--screenshot",
                "out.png",
                "--shot-help",
                "input.tak",
            ],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn help_shot_parses_size_and_scale() {
        let request = parse(&args(&[
            "--headless",
            "--screenshot",
            "out.png",
            "--shot-help",
            "--shot-help-size",
            "1920x1080@1.5",
            "--shot-help-bottom",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(request.pixels, [1920, 1080]);
        assert_eq!(request.scale, 1.5);
        assert!(request.bottom);
        assert_eq!(parse_size("1280x720"), Some(([1280, 720], 1.0)));
        for invalid in [
            "1920",
            "1920x0",
            "5000x1080",
            "1920x1080@NaN",
            "1920x1080@0",
            "1920x1080@4",
        ] {
            assert_eq!(parse_size(invalid), None);
        }
    }
}
