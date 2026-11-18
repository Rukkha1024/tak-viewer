//! Interactive launch arguments and startup view options.

use crate::views::{View, MAX_VIEWS};
use crate::workspace;
use std::path::{Path, PathBuf};
use tak_core::{tak, takm};

pub const USAGE: &str = "tak_viewer --only [--frame N] [--select A,B] [--comments] [--pick-comment K] <file> [<file2> …] (최대 4개)
tak_viewer --list-windows [--envelope]";

pub const NO_DESKTOP: &str =
    "지금 실행한 곳은 Windows 0번 세션이에요. 화면이 없어 뷰어 창을 열 수 없어요.";

#[derive(Debug, Default)]
pub struct StartupView {
    pub frame: Option<i64>,
    pub select: Option<String>,
    pub comments: bool,
    pub pick_comment: Option<u64>,
}

impl StartupView {
    pub fn apply(&self, view: &mut View, now: f64) {
        let picked = self
            .pick_comment
            .filter(|id| view.comments.list.get(*id).is_some());
        if let Some(id) = picked {
            view.goto_comment(id, now);
        }
        if let Some(frame) = self.frame {
            view.pb.set_frame(frame, now);
        }
        select_markers(view, self.select.as_deref());
        // Explicit markers override the Comment's markers and keep its row selected.
        if let Some(id) = picked {
            view.picked = Some(id);
            view.comment_selection.single(id);
        }
    }
}

/// Use the same multi-selection order for window launches and screenshots.
pub fn select_markers(view: &mut View, names: Option<&str>) {
    let Some(names) = names else { return };
    let mut extend = false;
    for name in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        if let Some(index) = view
            .data
            .as_ref()
            .and_then(|d| d.markers.iter().position(|m| m.name == name))
        {
            view.select(index, extend);
            extend = true;
        }
    }
}

pub enum Command {
    Only {
        paths: Vec<PathBuf>,
        startup: StartupView,
    },
    List {
        envelope: bool,
    },
}

pub fn parse(args: &[String]) -> Result<Option<Command>, String> {
    let Some((first, rest)) = args.split_first() else {
        return Ok(None);
    };
    if first == "--list-windows" {
        if matches!(rest, [flag] if flag == "--envelope") {
            return Ok(Some(Command::List { envelope: true }));
        }
        return if rest.is_empty() {
            Ok(Some(Command::List { envelope: false }))
        } else {
            Err("--list-windows 뒤에는 다른 옵션이나 파일을 쓸 수 없어요.".into())
        };
    }
    if first != "--only" {
        return Ok(None);
    }

    let mut startup = StartupView::default();
    let mut paths = Vec::new();
    let mut args = rest.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .filter(|v| !v.is_empty() && !v.starts_with("--"))
                .ok_or_else(|| format!("{arg} 뒤에 값을 써 주세요."))
        };
        match arg {
            "--frame" => {
                startup.frame = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--frame 뒤에 프레임 번호를 써 주세요.")?,
                );
            }
            "--select" => {
                let names = value()?;
                if names.split(',').any(|n| n.trim().is_empty()) {
                    return Err("--select 뒤에 마커 이름을 쉼표로 나눠 써 주세요.".into());
                }
                startup.select = Some(names.into());
            }
            "--comments" => startup.comments = true,
            "--pick-comment" => {
                startup.pick_comment = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--pick-comment 뒤에 Comment 번호를 써 주세요.")?,
                );
            }
            flag if flag.starts_with('-') => {
                return Err(format!("--only에서 쓸 수 없는 옵션이에요. {flag}"));
            }
            file => {
                let path = Path::new(file.trim().trim_matches(['"', '\'']).trim());
                if !tak::is_take_path(path) && !takm::is_takm_path(path) {
                    return Err(format!(".tak 또는 .takm 파일을 써 주세요. {file}"));
                }
                paths.push(PathBuf::from(file));
            }
        }
    }
    if !(1..=MAX_VIEWS).contains(&paths.len()) {
        return Err(format!(
            "--only 뒤에 파일을 1개부터 {MAX_VIEWS}개까지 써 주세요."
        ));
    }
    Ok(Some(Command::Only { paths, startup }))
}

/// Resolve and check all inputs before creating the window. File contents load as usual on workers.
pub fn resolve_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    paths
        .iter()
        .map(|path| {
            let path = workspace::resolve_path(&path.to_string_lossy());
            let path = std::path::absolute(&path).map_err(|err| {
                format!("파일 경로를 확인할 수 없어요. {} ({err})", path.display())
            })?;
            if !path.is_file() {
                return Err(format!("파일을 찾을 수 없어요. {}", path.display()));
            }
            Ok(path)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_only_options_and_four_files_in_order() {
        let values = [
            "--only",
            "--frame",
            "-7",
            "가 나.tak",
            "--select",
            "A,B",
            "--comments",
            "--pick-comment",
            "42",
            "b.TAKM",
            "c.tak",
            "d.tak",
        ];
        let Some(Command::Only { paths, startup }) = parse(&args(&values)).unwrap() else {
            panic!("only")
        };
        assert_eq!(
            paths,
            ["가 나.tak", "b.TAKM", "c.tak", "d.tak"].map(PathBuf::from)
        );
        assert_eq!(startup.frame, Some(-7));
        assert_eq!(startup.select.as_deref(), Some("A,B"));
        assert!(startup.comments);
        assert_eq!(startup.pick_comment, Some(42));
        assert!(matches!(
            parse(&args(&["--list-windows"])),
            Ok(Some(Command::List { envelope: false }))
        ));
        assert!(matches!(
            parse(&args(&["--list-windows", "--envelope"])),
            Ok(Some(Command::List { envelope: true }))
        ));
        assert!(matches!(parse(&args(&["a.tak"])), Ok(None)));
    }

    #[test]
    fn rejects_bad_window_arguments() {
        for values in [
            vec!["--only"],
            vec!["--only", "a.tak", "b.tak", "c.tak", "d.tak", "e.tak"],
            vec!["--only", "folder"],
            vec!["--only", "a.tak", "--headless"],
            vec!["--only", "--frame", "a.tak"],
            vec!["--only", "a.tak", "--frame"],
            vec!["--only", "a.tak", "--select", "--comments"],
            vec!["--only", "a.tak", "--select", "A,,B"],
            vec!["--only", "a.tak", "--pick-comment", "-1"],
            vec!["--list-windows", "a.tak"],
        ] {
            assert!(parse(&args(&values)).is_err(), "{values:?}");
        }
    }

    #[test]
    fn resolves_existing_files_and_rejects_a_missing_file_or_directory() {
        let dir = tak_core::test_support::TempDir::new("tak_viewer_only_args");
        let file = dir.join("a.takm");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(resolve_files(&[file.clone()]).unwrap(), [file.clone()]);
        assert!(resolve_files(&[file, dir.join("missing.tak")]).is_err());
        let folder = dir.join("folder.tak");
        std::fs::create_dir(&folder).unwrap();
        assert!(resolve_files(&[folder]).is_err());
    }
}
