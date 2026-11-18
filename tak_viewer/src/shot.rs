
use std::io::Write;
use std::path::{Path, PathBuf};
use crate::ui::comment_list::Field;
use tak_core::view_data::PlateView;

/// `--edit-comment` value: `<id>` (markers) or `<id>:markers|frames|comment`.
pub fn parse_edit(s: &str) -> Option<(u64, Field)> {
    let (id, cell) = s.split_once(':').unwrap_or((s, "markers"));
    let field = match cell {
        "markers" => Field::Markers,
        "frames" => Field::Frames,
        "comment" => Field::Text,
        "type" => Field::Type,
        _ => return None,
    };
    Some((id.trim().parse().ok()?, field))
}

#[derive(Clone, Debug)]
pub struct AutoShot {
    pub out: PathBuf,
    pub frame: Option<i64>,
    pub select: Option<ShotSelection>,
    /// `--plate`: plate name or number (from 0).
    pub plate: Option<String>,
    /// `--range <a>-<b>`: frame range for every view (ordered and clamped when applied).
    pub range: Option<(i64, i64)>,
    pub menu_capture: bool,
    pub edit_tools: bool,
    pub edit_pattern: bool,
    pub edit_layout: u8,
    pub labeling_link: bool,
    /// `--comments`: the COMMENTS pane in front.
    pub comments: bool,
    /// `--pick-comment <id>`: that comment selected in every view.
    pub pick_comment: Option<u64>,
    /// `--edit-comment <id>[:cell]`: that comment selected and the edit of that cell shown.
    pub edit_comment: Option<(u64, Field)>,
    /// `--type-strip <id>` (step 5): the type strip shown for that comment of the active view,
    /// as right after it was made (a command run writes nothing, so it is only looked at).
    pub type_strip: Option<u64>,
    /// `--overlay <b.tak>` (plan step 7, may repeat): the n-th file laid over the n-th view.
    pub overlays: Vec<PathBuf>,
    /// `--hide 1|2` (may repeat): in views with a file laid over them, that file's markers hidden
    /// (1 = the view's own file, 2 = the laid-over one); stored from 0.
    pub hide: Vec<usize>,
    /// `--hover <NAME>` (v0.29.0): the marker graph drawn as if the mouse were on that graphed
    /// marker's curve (`panels::LaneCache::hover_at`).
    pub hover: Option<String>,
    pub stage: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShotSelection {
    /// `--select`: a marker name, or several separated by commas.
    Names(String),
    /// `--select-visible`: the same marker set that 3D Ctrl+A selects.
    Visible,
}

/// `--overlay <file>` pairs taken out of `args` (plan step 7; for `--screenshot` and `--bench`):
/// the files, in order, and what is left.
pub fn take_overlays(args: &[String]) -> (Vec<PathBuf>, Vec<String>) {
    let (mut files, mut rest) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--overlay" && i + 1 < args.len() {
            files.push(PathBuf::from(&args[i + 1]));
            i += 2;
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }
    (files, rest)
}

/// The plate `--plate` asks for: a plate with that name, else the plate with that number
/// (from 0).
pub fn find_plate(plates: &[PlateView], want: &str) -> Option<usize> {
    plates.iter().position(|p| p.name == want).or_else(|| want.parse::<usize>().ok().filter(|&k| k < plates.len()))
}

/// `--range` value `<a>-<b>` (frame numbers from 0, either order). None when it is not that.
pub fn parse_range(s: &str) -> Option<(i64, i64)> {
    let (a, b) = s.trim().split_once('-')?;
    let num = |t: &str| t.trim().parse::<u64>().ok().and_then(|n| i64::try_from(n).ok());
    Some((num(a)?, num(b)?))
}

/// Parse `--screenshot`, `--frame`, `--select`, `--plate`, `--range`; returns (shot, remaining
/// positional args). A bad `--range` value is warned about and dropped (never taken for a file).
pub fn parse(args: &[String]) -> (Option<AutoShot>, Vec<String>) {
    let (overlays, args) = take_overlays(args);
    let args = args.as_slice();
    let mut hide = Vec::new();
    let mut shot: Option<AutoShot> = None;
    let mut frame = None;
    let mut select = None;
    let mut plate = None;
    let mut range = None;
    let mut menu_capture = false;
    let mut edit_tools = false;
    let mut edit_pattern = false;
    let mut edit_layout = 0;
    let mut labeling_link = false;
    let mut comments = false;
    let mut pick_comment = None;
    let mut edit_comment = None;
    let mut type_strip = None;
    let mut hover = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--screenshot" if i + 1 < args.len() => {
                shot = Some(AutoShot {
                    out: PathBuf::from(&args[i + 1]),
                    frame: None,
                    select: None,
                    plate: None,
                    range: None,
                    menu_capture: false,
                    edit_tools: false,
                    edit_pattern: false,
                    edit_layout: 0,
                    labeling_link: false,
                    comments: false,
                    pick_comment: None,
                    edit_comment: None,
                    type_strip: None,
                    overlays: Vec::new(),
                    hide: Vec::new(),
                    hover: None,
                    stage: 0,
                });
                i += 2;
            }
            "--hide" if i + 1 < args.len() => {
                match args[i + 1].trim() {
                    "1" => hide.push(0),
                    "2" => hide.push(1),
                    other => eprintln!("--hide: '{other}' is not 1 or 2"),
                }
                i += 2;
            }
            "--frame" if i + 1 < args.len() => {
                frame = args[i + 1].parse().ok();
                i += 2;
            }
            "--hover" if i + 1 < args.len() => {
                hover = Some(args[i + 1].trim().to_owned());
                i += 2;
            }
            "--select" if i + 1 < args.len() => {
                select = Some(ShotSelection::Names(args[i + 1].clone()));
                i += 2;
            }
            "--select-visible" => {
                select = Some(ShotSelection::Visible);
                i += 1;
            }
            "--plate" if i + 1 < args.len() => {
                plate = Some(args[i + 1].clone());
                i += 2;
            }
            "--range" if i + 1 < args.len() => {
                range = parse_range(&args[i + 1]);
                if range.is_none() {
                    eprintln!("--range: '{}' is not <a>-<b> (e.g. 300-900); no range", args[i + 1]);
                }
                i += 2;
            }
            "--menu-capture" => {
                menu_capture = true;
                i += 1;
            }
            "--comments" => {
                comments = true;
                i += 1;
            }
            "--edit-tools" => {
                edit_tools = true;
                i += 1;
            }
            "--labeling-link" => {
                labeling_link = true;
                i += 1;
            }
            "--edit-pattern" => {
                edit_tools = true;
                edit_pattern = true;
                i += 1;
            }
            "--edit-split" => {
                edit_layout = 2;
                i += 1;
            }
            "--edit-bottom" => {
                edit_layout = 3;
                i += 1;
            }
            "--pick-comment" if i + 1 < args.len() => {
                pick_comment = args[i + 1].trim().parse().ok();
                if pick_comment.is_none() {
                    eprintln!("--pick-comment: '{}' is not a comment number", args[i + 1]);
                }
                i += 2;
            }
            "--edit-comment" if i + 1 < args.len() => {
                edit_comment = parse_edit(&args[i + 1]);
                if edit_comment.is_none() {
                    eprintln!("--edit-comment: '{}' is not <id>[:markers|frames|comment|type]", args[i + 1]);
                }
                i += 2;
            }
            "--type-strip" if i + 1 < args.len() => {
                type_strip = args[i + 1].trim().parse().ok();
                if type_strip.is_none() {
                    eprintln!("--type-strip: '{}' is not a comment number", args[i + 1]);
                }
                i += 2;
            }
            _ => {
                rest.push(args[i].clone());
                i += 1;
            }
        }
    }
    if let Some(s) = shot.as_mut() {
        s.frame = frame;
        s.select = select;
        s.plate = plate;
        s.range = range;
        s.menu_capture = menu_capture;
        s.edit_tools = edit_tools;
        s.edit_pattern = edit_pattern;
        s.edit_layout = edit_layout;
        s.labeling_link = labeling_link;
        s.comments = comments;
        s.pick_comment = edit_comment.map(|e| e.0).or(pick_comment);
        s.edit_comment = edit_comment;
        s.type_strip = type_strip;
        s.overlays = overlays;
        s.hide = hide;
        s.hover = hover;
    }
    (shot, rest)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut c = Vec::with_capacity(4 + data.len());
    c.extend_from_slice(kind);
    c.extend_from_slice(data);
    out.extend_from_slice(&c);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
}

/// RGBA8 -> PNG bytes (zlib stored blocks, no compression).
pub fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width * 4 + 1) * height);
    for y in 0..height {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * width * 4..(y + 1) * width * 4]);
    }
    let mut z = vec![0x78, 0x01];
    let mut pos = 0;
    while pos < raw.len() || raw.is_empty() {
        let n = (raw.len() - pos).min(65535);
        let last = pos + n >= raw.len();
        z.push(last as u8);
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&raw[pos..pos + n]);
        pos += n;
        if raw.is_empty() {
            break;
        }
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    png
}

pub fn save_png(path: &Path, width: usize, height: usize, rgba: &[u8]) -> std::io::Result<()> {
    if tak_core::tak::is_take_path(path) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "refusing to write a .tak path"));
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&encode_png(width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_value() {
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
    }

    #[test]
    fn png_has_signature_and_iend() {
        let png = encode_png(2, 2, &[255u8; 16]);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn parse_args() {
        let a: Vec<String> = ["--screenshot", "o.png", "--frame", "12", "--select", "LASI", "x.tak"].iter().map(|s| s.to_string()).collect();
        let (s, rest) = parse(&a);
        let s = s.unwrap();
        assert_eq!(s.frame, Some(12));
        assert_eq!(s.select, Some(ShotSelection::Names("LASI".into())));
        assert_eq!(s.plate, None);
        assert!(!s.comments);
        assert_eq!(rest, vec!["x.tak".to_string()]);
        let a: Vec<String> = ["--screenshot", "o.png", "--comments", "x.tak"].iter().map(|s| s.to_string()).collect();
        let (s, rest) = parse(&a);
        assert!(s.unwrap().comments, "--comments: the COMMENTS pane in front");
        assert_eq!(rest, vec!["x.tak".to_string()]);
        let a: Vec<String> = ["--screenshot", "o.png", "--edit-pattern", "x.tak"].iter().map(|s| s.to_string()).collect();
        let (s, rest) = parse(&a);
        let s = s.unwrap();
        assert!(s.edit_tools && s.edit_pattern);
        assert_eq!(rest, vec!["x.tak".to_string()]);
        for (flag, expected) in [("--edit-split", 2), ("--edit-bottom", 3)] {
            let a: Vec<String> = ["--screenshot", "o.png", flag, "x.tak"].iter().map(|s| s.to_string()).collect();
            let (s, rest) = parse(&a);
            assert_eq!(s.unwrap().edit_layout, expected);
            assert_eq!(rest, vec!["x.tak".to_string()]);
        }
        // v0.17.1: a selected comment, an edit shown; bad values are never taken for a file.
        let a: Vec<String> = ["--screenshot", "o.png", "--pick-comment", "2", "--edit-comment", "3:frames", "x.tak"].iter().map(|s| s.to_string()).collect();
        let (s, rest) = parse(&a);
        let s = s.unwrap();
        assert_eq!((s.pick_comment, s.edit_comment), (Some(3), Some((3, Field::Frames))), "the edited one is the selected one");
        assert_eq!(rest, vec!["x.tak".to_string()]);
        assert_eq!(parse_edit("4"), Some((4, Field::Markers)));
        assert_eq!(parse_edit("4:comment"), Some((4, Field::Text)));
        assert_eq!(parse_edit("4:type"), Some((4, Field::Type)));
        for bad in ["x", "4:notes", ":frames", ""] {
            assert_eq!(parse_edit(bad), None, "{bad:?}");
        }
        // v0.18.0: the type strip of a comment, for pictures.
        let a: Vec<String> = ["--screenshot", "o.png", "--type-strip", "2", "x.tak"].iter().map(|s| s.to_string()).collect();
        let (s, rest) = parse(&a);
        assert_eq!((s.unwrap().type_strip, rest), (Some(2), vec!["x.tak".to_string()]));
        let a: Vec<String> = ["--screenshot", "o.png", "--type-strip", "x", "x.tak"].iter().map(|s| s.to_string()).collect();
        let (s, rest) = parse(&a);
        assert_eq!((s.unwrap().type_strip, rest), (None, vec!["x.tak".to_string()]));
        let a: Vec<String> = ["--screenshot", "o.png", "--pick-comment", "two", "x.tak"].iter().map(|s| s.to_string()).collect();
        let (s, rest) = parse(&a);
        assert_eq!((s.unwrap().pick_comment, rest), (None, vec!["x.tak".to_string()]));
        // v0.29.0: the hover of a marker's curve, for pictures; the name is never a file.
        let a: Vec<String> = ["--screenshot", "o.png", "--select", "LASI,LKNE", "--hover", "LKNE", "x.tak"].iter().map(|s| s.to_string()).collect();
        let (s, rest) = parse(&a);
        assert_eq!((s.unwrap().hover.as_deref(), rest), (Some("LKNE"), vec!["x.tak".to_string()]));
    }

    #[test]
    fn plate_argument_is_a_name_or_a_number() {
        let a: Vec<String> = ["--screenshot", "o.png", "--plate", "2", "a.tak", "b.tak"].iter().map(|s| s.to_string()).collect();
        let (s, rest) = parse(&a);
        assert_eq!(s.unwrap().plate.as_deref(), Some("2"));
        assert_eq!(rest, vec!["a.tak".to_string(), "b.tak".to_string()]);
        let plate = |name: &str| PlateView { name: name.into(), corners: [tak_core::geom::v3(0.0, 0.0, 0.0); 4], fz: vec![].into(), fz_abs_max: 0.0, fz_signal: Default::default() };
        let plates = [plate("Force Plate 1"), plate("1"), plate("Force Plate 3")];
        assert_eq!(find_plate(&plates, "Force Plate 3"), Some(2), "by name");
        assert_eq!(find_plate(&plates, "1"), Some(1), "a name first");
        assert_eq!(find_plate(&plates, "0"), Some(0), "else by number from 0");
        assert_eq!(find_plate(&plates, "3"), None, "no such plate");
        assert_eq!(find_plate(&plates, "LASI"), None);
    }

    #[test]
    fn range_argument_is_two_frames_and_never_a_file() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let (s, rest) = parse(&args(&["--screenshot", "o.png", "--range", "300-900", "--frame", "5", "a.tak", "b.tak"]));
        let s = s.unwrap();
        assert_eq!(s.range, Some((300, 900)));
        assert_eq!(s.frame, Some(5));
        assert_eq!(rest, args(&["a.tak", "b.tak"]));
        // Either order is kept as given here (ordered and clamped when applied to a view).
        assert_eq!(parse_range("900-300"), Some((900, 300)));
        assert_eq!(parse_range(" 7 - 7 "), Some((7, 7)));
        // Bad values: no range, and the value is not taken for a file.
        for bad in ["300", "a-b", "300-", "-300", "1-2-3", "", "-5-10", "3.5-9"] {
            assert_eq!(parse_range(bad), None, "{bad:?}");
            let (s, rest) = parse(&args(&["--screenshot", "o.png", "--range", bad, "x.tak"]));
            assert_eq!(s.unwrap().range, None, "{bad:?}");
            assert_eq!(rest, args(&["x.tak"]), "{bad:?}");
        }
        // Without --screenshot there is no shot; the value is still not a file.
        let (s, rest) = parse(&args(&["--range", "1-2", "x.tak"]));
        assert!(s.is_none());
        assert_eq!(rest, args(&["x.tak"]));
    }
}
