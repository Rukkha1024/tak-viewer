//! Opt-in probe for saved Progress in a dedicated test folder.
use serde_json::json;
use tak_core::remote::{CancelToken, RemoteConfig, RemoteSession};

pub(crate) const USAGE: &str = r#"tak_viewer --headless --ssh-test-progress ALIAS FOLDER [1|3] --ssh-platform linux|windows --ssh-python "COMMAND""#;

struct Options<'a> {
    alias: &'a str,
    folder: &'a str,
    platform: &'a str,
    python: &'a str,
    set: Option<i64>,
}

fn test_folder(path: &str, platform: &str) -> bool {
    let name = match platform {
        "windows" => path
            .split_once(":\\")
            .filter(|(drive, _)| matches!(drive.as_bytes(), [c] if c.is_ascii_alphabetic()))
            .map(|(_, name)| name),
        "linux" => path.strip_prefix('/'),
        _ => None,
    };
    name.and_then(|name| name.strip_prefix("tak_viewer_agent_test_"))
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        })
}

fn parse(args: &[String]) -> Result<Options<'_>, String> {
    let [alias, rest @ ..] = args else {
        return Err("SSH 서버 별칭을 넣어 주세요.".into());
    };
    if alias.trim().is_empty() || alias.starts_with('-') {
        return Err("SSH 서버 별칭을 넣어 주세요.".into());
    }
    let [folder, rest @ ..] = rest else {
        return Err("시험 폴더 경로를 넣어 주세요.".into());
    };
    if folder.starts_with("--") {
        return Err("시험 폴더 경로를 넣어 주세요.".into());
    }
    let (set, rest) = match rest {
        [value, rest @ ..] if !value.starts_with("--") => match value.as_str() {
            "1" => (Some(1), rest),
            "3" => (Some(3), rest),
            _ => return Err("진행 상태 값은 1 또는 3을 넣어 주세요.".into()),
        },
        _ => (None, rest),
    };
    let (mut platform, mut python) = (None, None);
    for pair in rest.chunks(2) {
        let [option, value] = pair else {
            return Err(format!("{} 뒤에 값을 넣어 주세요.", pair[0]));
        };
        match option.as_str() {
            "--ssh-platform" if platform.is_none() => platform = Some(value.as_str()),
            "--ssh-python" if python.is_none() => python = Some(value.as_str()),
            _ => return Err(format!("쓸 수 없거나 두 번 나온 옵션이에요. {option}")),
        }
    }
    let platform = platform
        .filter(|platform| matches!(*platform, "linux" | "windows"))
        .ok_or("--ssh-platform 뒤에 linux 또는 windows를 넣어 주세요.")?;
    let python = python
        .filter(|python| !python.trim().is_empty() && !python.starts_with("--"))
        .ok_or("--ssh-python 뒤에 서버에서 쓸 Python 명령을 넣어 주세요.")?;
    tak_core::remote::python_tokens(python, platform)
        .map_err(|_| "--ssh-python 뒤의 Python 명령을 확인해 주세요.")?;
    if !test_folder(folder, platform) {
        return Err(
            "드라이브나 / 바로 아래에 있는 tak_viewer_agent_test_로 시작하는 시험 폴더를 넣어 주세요."
                .into(),
        );
    }
    Ok(Options {
        alias,
        folder,
        platform,
        python,
        set,
    })
}

fn probe(options: Options<'_>) -> Result<(), String> {
    let Options {
        alias,
        folder,
        platform,
        python,
        set,
    } = options;
    let mut config = RemoteConfig {
        directory_limit: 2,
        ..RemoteConfig::default()
    };
    let separator = if platform == "windows" {
        config.windows_python = python.into();
        '\\'
    } else {
        config.linux_python = python.into();
        '/'
    };
    let cancel = CancelToken::default();
    let mut session = RemoteSession::connect_cancellable(alias, platform, config, &cancel)?;
    let listing = session.list(folder, &cancel)?;
    let rows = listing["entries"]
        .as_array()
        .ok_or("서버 파일 목록을 확인할 수 없어요.")?;
    let names = ["sample.tak", "sample.takm"];
    let paths: Vec<_> = names
        .iter()
        .map(|name| format!("{folder}{separator}{name}"))
        .collect();
    if listing["truncated"] != false
        || rows.len() != paths.len()
        || !rows
            .iter()
            .zip(&paths)
            .all(|(row, path)| row["directory"] == false && row["path"] == path.as_str())
    {
        return Err("시험 폴더에는 sample.tak과 sample.takm만 있어야 해요.".into());
    }

    let rows = session.progress_batch(&paths, &cancel)?;
    let saved = rows[1]
        .as_ref()
        .ok_or("TAKM의 진행 상태를 읽을 수 없어요.")?;
    let original = rows[0]
        .as_ref()
        .ok_or("TAK의 진행 상태를 읽을 수 없어요.")?;
    println!(
        "{}",
        json!({"stage":"listed_progress","rows":[
            {"name":names[0],"progress":original.value},
            {"name":names[1],"progress":saved.value}
        ]})
    );
    if let Some(next) = set {
        let previous = if next == 3 { 1 } else { 3 };
        if saved.value != Some(previous) {
            return Err("TAKM의 진행 상태가 예상한 값과 달라요.".into());
        }
        let written = session.write_progress(&saved.source, Some(previous), Some(next), &cancel)?;
        if !written.verified {
            return Err("서버가 저장한 진행 상태를 확인할 수 없어요.".into());
        }
        let reread = session.progress_batch(&paths, &cancel)?;
        if !reread[0]
            .as_ref()
            .is_some_and(|row| row.value == original.value && row.source == original.source)
            || !reread[1]
                .as_ref()
                .is_some_and(|row| row.value == Some(next) && row.source == written.source)
        {
            return Err("다시 읽은 파일의 진행 상태가 예상한 값과 달라요.".into());
        }
        println!(
            "{}",
            json!({"stage":"saved_progress","before":previous,"after":next,"seal_verified":true})
        );
    }
    Ok(())
}

pub(crate) fn run(args: &[String]) -> i32 {
    run_with(args, probe)
}

fn run_with(args: &[String], probe: impl FnOnce(Options<'_>) -> Result<(), String>) -> i32 {
    let options = match parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return 2;
        }
    };
    match probe(options) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("SSH 진행 상태 시험을 마치지 못했어요. {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_a_top_level_test_folder() {
        assert!(test_folder(r"X:\tak_viewer_agent_test_demo", "windows"));
        assert!(test_folder("/tak_viewer_agent_test_demo", "linux"));
        for path in [
            r"X:\tak_viewer_agent_test_",
            r"\\test-host\share\tak_viewer_agent_test_demo",
            r"X:\data\tak_viewer_agent_test_demo",
            r"X:\tak_viewer_agent_test_demo\..\data",
            r"X:\tak_viewer_agent_test_demo\sample.takm",
            r"X:\tak_viewer_agent_test_demo:stream",
            r"X:tak_viewer_agent_test_demo",
            r"XY:\tak_viewer_agent_test_demo",
            "/tak_viewer_agent_test_demo",
        ] {
            assert!(!test_folder(path, "windows"), "{path}");
        }
        for path in [
            "/tak_viewer_agent_test_",
            "/data/tak_viewer_agent_test_demo",
            "/tak_viewer_agent_test_demo/../data",
            "/tak_viewer_agent_test_demo/sample.takm",
            "/tak_viewer_agent_test_demo:stream",
            "//tak_viewer_agent_test_demo",
            "tak_viewer_agent_test_demo",
            r"X:\tak_viewer_agent_test_demo",
        ] {
            assert!(!test_folder(path, "linux"), "{path}");
        }
    }

    #[test]
    fn missing_options_and_unsafe_folders_never_start_the_probe() {
        for args in [
            &[][..],
            &["--ssh-platform", "linux", "--ssh-python", "python3"],
            &[
                "test-host",
                "/tak_viewer_agent_test_demo",
                "--ssh-platform",
                "linux",
            ],
            &[
                "test-host",
                "/tak_viewer_agent_test_demo",
                "--ssh-python",
                "python3",
            ],
            &[
                "test-host",
                "/other/tak_viewer_agent_test_demo",
                "--ssh-platform",
                "linux",
                "--ssh-python",
                "python3",
            ],
            &[
                r"test-host",
                r"X:\tak_viewer_agent_test_demo:stream",
                "--ssh-platform",
                "windows",
                "--ssh-python",
                "py -3",
            ],
            &[
                "test-host",
                "/tak_viewer_agent_test_demo",
                "2",
                "--ssh-platform",
                "linux",
                "--ssh-python",
                "python3",
            ],
        ] {
            let args = args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>();
            assert_eq!(run_with(&args, |_| panic!("must stop before SSH")), 2);
        }
    }

    #[test]
    fn probe_receives_the_explicit_connection_options() {
        let args = [
            "test-host",
            r"X:\tak_viewer_agent_test_demo",
            "3",
            "--ssh-python",
            "py -3",
            "--ssh-platform",
            "windows",
        ]
        .map(String::from);
        assert_eq!(
            run_with(&args, |options| {
                assert_eq!(options.alias, "test-host");
                assert_eq!(options.folder, r"X:\tak_viewer_agent_test_demo");
                assert_eq!(options.platform, "windows");
                assert_eq!(options.python, "py -3");
                assert_eq!(options.set, Some(3));
                Ok(())
            }),
            0
        );
    }
}
