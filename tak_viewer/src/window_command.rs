//! Interactive launch arguments and startup view options.

use crate::MAX_OPEN_FILES;
use crate::views::{View, MAX_VIEWS};
use crate::workspace;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tak_core::{tak, takm};

pub const USAGE: &str = "tak_viewer --only [--frame N] [--select A,B] [--comments] [--pick-comment K] <file> [<file2> …]
tak_viewer --only [공통 옵션] --tab [--name 이름] [탭 옵션] <file> … [--tab …] (탭마다 최대 4개, 전체에서 서로 다른 파일 최대 30개)
tak_viewer --only --from <tabs.json>
tak_viewer --list-windows [--envelope]
tak_viewer --list-windows --wait <pid> [--timeout 초]";

pub const NO_DESKTOP: &str =
    "지금 실행한 곳은 Windows 0번 세션이에요. 화면이 없어 뷰어 창을 열 수 없어요.";

#[derive(Clone, Debug, Default)]
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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabSpec {
    name: Option<String>,
    files: Vec<PathBuf>,
    frame: Option<i64>,
    select: Option<String>,
    comments: Option<bool>,
    pick_comment: Option<u64>,
}

#[derive(Debug)]
pub struct StartupFile {
    pub path: PathBuf,
    pub identity: PathBuf,
}

#[derive(Debug)]
pub struct StartupTab {
    pub name: String,
    pub files: Vec<StartupFile>,
    pub options: StartupView,
}

pub enum Command {
    Only(Vec<TabSpec>),
    List {
        envelope: bool,
    },
    Wait { pid: u32, timeout: Duration },
}

pub fn parse(args: &[String]) -> Result<Option<Command>, String> {
    let Some((first, rest)) = args.split_first() else {
        return Ok(None);
    };
    match first.as_str() {
        "--list-windows" => parse_list(rest).map(Some),
        "--only" => parse_tabs(rest).map(|tabs| Some(Command::Only(tabs))),
        _ => Ok(None),
    }
}

fn parse_list(rest: &[String]) -> Result<Command, String> {
    match rest {
        [] => return Ok(Command::List { envelope: false }),
        [flag] if flag == "--envelope" => return Ok(Command::List { envelope: true }),
        _ => {}
    }
    let (pid, timeout) = match rest {
        [wait, pid] if wait == "--wait" => (pid, 120),
        [wait, pid, flag, seconds] if wait == "--wait" && flag == "--timeout" => {
            let timeout = seconds
                .parse()
                .map_err(|_| "--timeout 뒤에 초를 정수로 써 주세요.")?;
            (pid, timeout)
        }
        _ => {
            return Err("--list-windows 뒤에는 --envelope 또는 --wait <pid> [--timeout 초]만 쓸 수 있어요.".into())
        }
    };
    let pid = pid
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid != 0)
        .ok_or("--wait 뒤에 0보다 큰 뷰어 PID를 써 주세요.")?;
    Ok(Command::Wait {
        pid,
        timeout: Duration::from_secs(timeout),
    })
}

fn parse_tabs(rest: &[String]) -> Result<Vec<TabSpec>, String> {
    if rest.iter().any(|arg| arg == "--from") {
        return match rest {
            [flag, path] if flag == "--from" && !path.is_empty() && !path.starts_with('-') => {
                read_tabs(&workspace::resolve_path(path))
            }
            _ => Err("--only --from 뒤에는 탭 목록 JSON 파일 하나만 써 주세요.".into()),
        };
    }

    let mut defaults = TabSpec::default();
    let mut groups = Vec::new();
    let mut args = rest.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .filter(|v| !v.is_empty() && !v.starts_with("--"))
                .ok_or_else(|| format!("{arg} 뒤에 값을 써 주세요."))
        };
        if arg == "--tab" {
            groups.push(TabSpec::default());
            continue;
        }
        let named_group = !groups.is_empty();
        let tab = groups.last_mut().unwrap_or(&mut defaults);
        match arg {
            "--name" if named_group && tab.name.is_none() => tab.name = Some(value()?.into()),
            "--frame" => {
                tab.frame = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--frame 뒤에 프레임 번호를 써 주세요.")?,
                );
            }
            "--select" => {
                let names = value()?;
                validate_selection(names)?;
                tab.select = Some(names.into());
            }
            "--comments" => tab.comments = Some(true),
            "--no-comments" => tab.comments = Some(false),
            "--pick-comment" => {
                tab.pick_comment = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--pick-comment 뒤에 Comment 번호를 써 주세요.")?,
                );
            }
            flag if flag.starts_with('-') => {
                return Err(format!("--only에서 쓸 수 없는 옵션이에요. {flag}"));
            }
            file => tab.files.push(PathBuf::from(file)),
        }
    }

    for group in &mut groups {
        group.frame = group.frame.or(defaults.frame);
        group.select = group.select.take().or_else(|| defaults.select.clone());
        group.comments = group.comments.or(defaults.comments);
        group.pick_comment = group.pick_comment.or(defaults.pick_comment);
    }
    if !defaults.files.is_empty() || groups.is_empty() {
        groups.insert(0, defaults);
    }
    validate_tabs(groups)
}

fn read_tabs(path: &Path) -> Result<Vec<TabSpec>, String> {
    let read_error = |err: &dyn std::fmt::Display| {
        format!("탭 목록을 읽을 수 없어요. {} ({err})", path.display())
    };
    let bytes = std::fs::read(path).map_err(|err| read_error(&err))?;
    let mut tabs = validate_tabs(serde_json::from_slice(&bytes).map_err(|err| read_error(&err))?)?;
    let folder = path.parent().unwrap_or_else(|| Path::new(""));
    for tab in &mut tabs {
        for file in &mut tab.files {
            *file = workspace::resolve_path(&file.to_string_lossy());
            // On Windows a fully qualified drive/UNC path is left intact by join.
            *file = folder.join(&*file);
        }
    }
    Ok(tabs)
}

fn validate_selection(names: &str) -> Result<(), String> {
    if names.split(',').any(|n| n.trim().is_empty()) {
        return Err("--select 뒤에 마커 이름을 쉼표로 나눠 써 주세요.".into());
    }
    Ok(())
}

fn validate_tabs(mut tabs: Vec<TabSpec>) -> Result<Vec<TabSpec>, String> {
    if tabs.is_empty() {
        return Err("열 탭을 하나 이상 써 주세요.".into());
    }
    let mut names = HashSet::new();
    for tab in &tabs {
        if let Some(name) = &tab.name {
            if name.trim().is_empty() {
                return Err("탭 이름을 써 주세요.".into());
            }
            if !names.insert(name.clone()) {
                return Err(format!("같은 탭 이름을 두 번 쓸 수 없어요. {name}"));
            }
        }
        if !(1..=MAX_VIEWS).contains(&tab.files.len()) {
            return Err(format!(
                "한 탭에는 파일을 1개부터 {MAX_VIEWS}개까지 써 주세요."
            ));
        }
        if let Some(select) = &tab.select {
            validate_selection(select)?;
        }
        for file in &tab.files {
            let file_text = file.to_string_lossy();
            let path = Path::new(file_text.trim().trim_matches(['"', '\'']).trim());
            if !tak::is_take_path(path) && !takm::is_takm_path(path) {
                return Err(format!(
                    ".tak 또는 .takm 파일을 써 주세요. {}",
                    file.display()
                ));
            }
        }
    }
    let mut number = 1;
    for tab in &mut tabs {
        if tab.name.is_none() {
            loop {
                let name = format!("tab{number}");
                number += 1;
                if names.insert(name.clone()) {
                    tab.name = Some(name);
                    break;
                }
            }
        }
    }
    Ok(tabs)
}

/// Resolve all paths and enforce the app-wide cap before creating a window.
pub fn resolve_tabs(tabs: Vec<TabSpec>) -> Result<Vec<StartupTab>, (i32, String)> {
    let mut distinct = HashSet::new();
    tabs.into_iter()
        .map(|tab| {
            let files = tab
                .files
                .iter()
                .map(|path| {
                    let path = workspace::resolve_path(&path.to_string_lossy());
                    let path = std::path::absolute(&path).map_err(|err| {
                        (
                            1,
                            format!("파일 경로를 확인할 수 없어요. {} ({err})", path.display()),
                        )
                    })?;
                    if !path.is_file() {
                        return Err((1, format!("파일을 찾을 수 없어요. {}", path.display())));
                    }
                    let identity = takm::canonical_target(&path).map_err(|err| {
                        (
                            1,
                            format!("파일 경로를 확인할 수 없어요. {} ({err})", path.display()),
                        )
                    })?;
                    distinct.insert(identity.clone());
                    if distinct.len() > MAX_OPEN_FILES {
                        return Err((
                            2,
                            format!(
                                "서로 다른 파일은 전체에서 {MAX_OPEN_FILES}개까지 열 수 있어요."
                            ),
                        ));
                    }
                    Ok(StartupFile { path, identity })
                })
                .collect::<Result<_, _>>()?;
            Ok(StartupTab {
                name: tab.name.expect("validated tab has a name"),
                files,
                options: StartupView {
                    frame: tab.frame,
                    select: tab.select,
                    comments: tab.comments.unwrap_or_default(),
                    pick_comment: tab.pick_comment,
                },
            })
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
        let Some(Command::Only(tabs)) = parse(&args(&values)).unwrap() else {
            panic!("only")
        };
        assert_eq!(
            tabs[0].files,
            ["가 나.tak", "b.TAKM", "c.tak", "d.tak"].map(PathBuf::from)
        );
        assert_eq!(tabs[0].frame, Some(-7));
        assert_eq!(tabs[0].select.as_deref(), Some("A,B"));
        assert_eq!(tabs[0].comments, Some(true));
        assert_eq!(tabs[0].pick_comment, Some(42));
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

    fn only(values: &[&str]) -> Vec<TabSpec> {
        let Some(Command::Only(tabs)) = parse(&args(values)).unwrap() else {
            panic!("only")
        };
        tabs
    }

    #[test]
    fn tabs_inherit_defaults_and_reserve_explicit_names() {
        let tabs = only(&[
            "--only",
            "--frame",
            "21",
            "--select",
            "A,B",
            "--pick-comment",
            "3",
            "first.tak",
            "--tab",
            "--name",
            "tab1",
            "--frame",
            "42",
            "--comments",
            "--select",
            "C",
            "second.tak",
            "third.takm",
            "--tab",
            "--pick-comment",
            "9",
            "fourth.tak",
        ]);
        assert_eq!(
            tabs.iter()
                .map(|t| t.name.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["tab2", "tab1", "tab3"]
        );
        assert_eq!(tabs[0].files, [PathBuf::from("first.tak")]);
        assert_eq!(
            tabs[1].files,
            ["second.tak", "third.takm"].map(PathBuf::from)
        );
        assert_eq!(tabs[2].files, [PathBuf::from("fourth.tak")]);
        assert_eq!(
            tabs.iter().map(|t| t.frame).collect::<Vec<_>>(),
            [Some(21), Some(42), Some(21)]
        );
        assert_eq!(
            tabs.iter().map(|t| t.select.as_deref()).collect::<Vec<_>>(),
            [Some("A,B"), Some("C"), Some("A,B")]
        );
        assert_eq!(
            tabs.iter().map(|t| t.comments).collect::<Vec<_>>(),
            [None, Some(true), None]
        );
        assert_eq!(
            tabs.iter().map(|t| t.pick_comment).collect::<Vec<_>>(),
            [Some(3), Some(3), Some(9)]
        );
        let comments = only(&[
            "--only",
            "--comments",
            "--tab",
            "a.tak",
            "--tab",
            "--no-comments",
            "b.tak",
        ]);
        assert_eq!(comments[0].comments, Some(true));
        assert_eq!(comments[1].comments, Some(false));
    }

    #[test]
    fn reads_manifest_relative_to_its_folder_and_checks_schema() {
        let dir = tak_core::test_support::TempDir::new("tak_viewer_manifest");
        let manifest = dir.join("탭 목록.json");
        let first = dir.join("한 글.tak");
        std::fs::write(&first, b"").unwrap();
        std::fs::write(
            &manifest,
            r#"[
            {"name":"비교", "files":["한 글.tak"], "frame":-7,"select":"A,B",
             "comments":true,"pick_comment":3},
            {"files":["./한 글.tak"]}
        ]"#,
        )
        .unwrap();
        let name = manifest.to_str().unwrap();
        let tabs = resolve_tabs(only(&["--only", "--from", name])).unwrap();
        assert_eq!(tabs[0].name, "비교");
        assert_eq!(tabs[0].files[0].path, first);
        assert_eq!(tabs[0].files[0].identity, tabs[1].files[0].identity);
        assert_eq!(tabs[0].options.frame, Some(-7));
        assert_eq!(tabs[0].options.select.as_deref(), Some("A,B"));
        assert!(tabs[0].options.comments);
        assert_eq!(tabs[0].options.pick_comment, Some(3));
        for invalid in [
            "{}",
            "[]",
            r#"[{"files":[]}]"#,
            r#"[{"file":["a.tak"]}]"#,
            r#"[{"files":["a.tak"],"frames":2}]"#,
            r#"[{"files":["a.tak"],"select":"A,,B"}]"#,
            r#"[{"name":"A","files":["a.tak"]},{"name":"A","files":["b.tak"]}]"#,
        ] {
            std::fs::write(&manifest, invalid).unwrap();
            assert!(
                parse(&args(&["--only", "--from", name])).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn rejects_mixed_groups_and_wait_errors() {
        for bad in [
            vec!["--only", "--from"],
            vec!["--only", "--from", "a.json", "--tab", "a.tak"],
            vec!["--only", "a.tak", "--from", "a.json"],
            vec!["--only", "--tab", "--tab", "b.tak"],
            vec!["--only", "--name", "A", "a.tak"],
            vec!["--only", "--tab", "--name", " ", "a.tak"],
            vec!["--only", "--tab", "a.tak", "--select", "--tab", "b.tak"],
            vec!["--only", "--tab", "--list-windows"],
            vec!["--list-windows", "--wait"],
            vec!["--list-windows", "--wait", "0"],
            vec!["--list-windows", "--wait", "-4"],
            vec!["--list-windows", "--wait", "4", "--timeout", "1.5"],
            vec!["--list-windows", "--timeout", "4"],
            vec!["--list-windows", "--wait", "5", "a.tak"],
        ] {
            assert!(parse(&args(&bad)).is_err(), "{bad:?}");
        }
        for (extra, seconds) in [(vec![], 120), (vec!["--timeout", "0"], 0)] {
            let values = [vec!["--list-windows", "--wait", "31"], extra].concat();
            let Some(Command::Wait { pid, timeout }) = parse(&args(&values)).unwrap() else {
                panic!("wait")
            };
            assert_eq!(pid, 31);
            assert_eq!(timeout, Duration::from_secs(seconds));
        }
    }

    #[test]
    fn resolves_files_and_caps_distinct_sources_across_tab_groups() {
        let dir = tak_core::test_support::TempDir::new("tak_viewer_only_args");
        let files: Vec<_> = (0..=MAX_OPEN_FILES)
            .map(|n| {
                let path = dir.join(format!("{n}.takm"));
                std::fs::write(&path, b"").unwrap();
                path
            })
            .collect();
        let groups = |paths: &[PathBuf]| {
            validate_tabs(
                paths
                    .chunks(MAX_VIEWS)
                    .map(|files| TabSpec {
                        files: files.to_vec(),
                        ..Default::default()
                    })
                    .collect(),
            )
            .unwrap()
        };
        assert_eq!(resolve_tabs(groups(&files[..MAX_OPEN_FILES])).unwrap().len(), MAX_OPEN_FILES.div_ceil(MAX_VIEWS));
        assert_eq!(resolve_tabs(groups(&files)).unwrap_err().0, 2);
        let alias = dir.join(".").join("0.takm");
        let duplicate = [&files[..MAX_OPEN_FILES], &[alias]].concat();
        assert_eq!(resolve_tabs(groups(&duplicate)).unwrap().len(), (MAX_OPEN_FILES + 1).div_ceil(MAX_VIEWS));
        assert_eq!(
            resolve_tabs(groups(&[dir.join("missing.tak")]))
                .unwrap_err()
                .0,
            1
        );
        let folder = dir.join("folder.tak");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(resolve_tabs(groups(&[folder])).unwrap_err().0, 1);
    }
}
