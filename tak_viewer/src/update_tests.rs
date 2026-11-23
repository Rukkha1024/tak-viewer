use super::*;

const MANIFEST: &str = "[workspace.package]\nversion = \"0.76.0\"";

#[test]
fn only_reads_workspace_package_version() {
    assert_eq!(
        workspace_version(include_str!("../../Cargo.toml")).as_deref(),
        Some(crate::version::VERSION)
    );
    assert_eq!(
        workspace_version("[package]\nversion = \"0.1.0\"\n[workspace.package] # release\n version = '12.4.0' # shared\n[profile.dev]\nversion = '6.0.0'").as_deref(),
        Some("12.4.0")
    );
    for bad in [
        "[package]\nversion = \"1.2.3\"",
        "[workspace.package]\n[profile.dev]\nversion = \"1.2.3\"",
        "[workspace.package]\nversion = \"01.2.3\"",
        "[workspace.package]\nversion = \"1.2\"",
        "[workspace.package]\nversion = \"1.2.3-bad\"",
    ] {
        assert_eq!(workspace_version(bad), None, "{bad}");
    }
}

#[derive(Default)]
struct GitFixture {
    dirty: bool,
    no_upstream: bool,
    no_git: bool,
    no_network: bool,
    counts: Option<&'static str>,
    fetches: usize,
}

impl GitFixture {
    fn run(&mut self, args: &[&str]) -> GitResult {
        if self.no_git {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        let result = match args {
            ["rev-parse", "--show-prefix"] => "",
            ["status", "--porcelain", "--untracked-files=normal"] if self.dirty => "?? notes",
            ["status", "--porcelain", "--untracked-files=normal"] => "",
            ["rev-parse", "--symbolic-full-name", "@{u}"] if self.no_upstream => return Ok(None),
            ["rev-parse", "--symbolic-full-name", "@{u}"] => "refs/remotes/origin/main",
            ["fetch", "--no-recurse-submodules"] => {
                self.fetches += 1;
                if self.no_network {
                    return Ok(None);
                }
                ""
            }
            ["rev-parse", "HEAD", "@{u}"] if self.counts.is_none() => "one\none",
            ["rev-parse", "HEAD", "@{u}"] => "one\ntwo",
            ["rev-list", "--left-right", "--count", "one...two"] => self.counts.unwrap(),
            ["show", "two:Cargo.toml"] => MANIFEST,
            _ => panic!("unexpected Git command {args:?}"),
        };
        Ok(Some(result.to_owned()))
    }
}

#[test]
fn check_reports_latest_and_fast_forward_after_fetch() {
    let mut latest = GitFixture::default();
    assert_eq!(check_git(|args| latest.run(args)), Ok(None));
    assert_eq!(latest.fetches, 1);
    let mut newer = GitFixture {
        counts: Some("0\t3"),
        ..Default::default()
    };
    assert_eq!(
        check_git(|args| newer.run(args)),
        Ok(Some(Available {
            old_head: "one".into(),
            upstream: "two".into(),
            version: "0.76.0".into(),
        }))
    );
    assert_eq!(newer.fetches, 1);
}

#[test]
fn local_changes_missing_upstream_and_missing_git_stop_before_fetch() {
    for (mut fixture, expected) in [
        (
            GitFixture {
                dirty: true,
                ..Default::default()
            },
            DIRTY,
        ),
        (
            GitFixture {
                no_upstream: true,
                ..Default::default()
            },
            NO_UPSTREAM,
        ),
        (
            GitFixture {
                no_git: true,
                ..Default::default()
            },
            GIT_MISSING,
        ),
    ] {
        assert_eq!(check_git(|args| fixture.run(args)), Err(expected));
        assert_eq!(fixture.fetches, 0);
    }
}

#[test]
fn ahead_diverged_and_fetch_failure_are_distinct() {
    for (mut fixture, expected) in [
        (
            GitFixture {
                counts: Some("2\t0"),
                ..Default::default()
            },
            AHEAD,
        ),
        (
            GitFixture {
                counts: Some("2\t3"),
                ..Default::default()
            },
            DIVERGED,
        ),
        (
            GitFixture {
                no_network: true,
                ..Default::default()
            },
            FETCH_FAILED,
        ),
    ] {
        assert_eq!(check_git(|args| fixture.run(args)), Err(expected));
        assert_eq!(fixture.fetches, 1);
    }
}

#[test]
fn every_failure_reason_is_an_independent_korean_message() {
    let reasons = [
        SOURCE_MISSING,
        NOT_A_CHECKOUT,
        GIT_MISSING,
        NO_UPSTREAM,
        DIRTY,
        AHEAD,
        DIVERGED,
        FETCH_FAILED,
        READ_FAILED,
        BAD_VERSION,
        CHANGED,
        MAC_BUNDLE,
        NO_INSTALL_PATH,
        UNSUPPORTED,
        LOG_FAILED,
        BUSY,
        PULL_FAILED,
        BUILD_FAILED,
        VERIFY_FAILED,
        SESSION_FAILED,
        RESTART_FAILED,
        WORKER_FAILED,
    ];
    assert_eq!(
        reasons
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        reasons.len()
    );
    for reason in reasons {
        assert!(
            reason.chars().any(|c| ('가'..='힣').contains(&c)),
            "{reason}"
        );
        assert!(reason.ends_with('.'), "{reason}");
    }
}

#[test]
fn restart_targets_use_installed_paths() {
    let bundle = std::env::temp_dir().join("Applications/TAK Viewer.app");
    let binary = bundle.join("Contents/MacOS/tak_viewer");
    assert_eq!(mac_bundle(&binary), Some(bundle.clone()));
    assert_eq!(mac_bundle(Path::new("/usr/local/bin/tak_viewer")), None);
    assert_eq!(
        mac_bundle(Path::new(
            "/Applications/TAK Viewer.app/Contents/MacOS/another"
        )),
        None
    );
    assert_eq!(
        mac_bundle(Path::new("TAK Viewer.app/Contents/MacOS/tak_viewer")),
        None
    );

    let mac = RestartTarget::Mac(bundle.clone());
    assert_eq!(mac.binary(), binary);
    assert_eq!(mac.command().get_program(), "/usr/bin/open");
    assert_eq!(
        mac.command().get_args().collect::<Vec<_>>(),
        ["-n".as_ref(), bundle.as_os_str()]
    );

    let base = std::env::temp_dir();
    let installed = base.join("tak_viewer/target/release/tak_viewer.exe");
    let windows = RestartTarget::Windows(windows_install_path(Some(&base)).unwrap());
    assert_eq!(windows.binary(), installed);
    assert_eq!(Path::new(windows.command().get_program()), installed);
    assert_eq!(windows_install_path(None), None);
    assert_eq!(windows_install_path(Some(Path::new("relative"))), None);
}

#[test]
fn native_help_click_starts_a_check_on_another_thread() {
    use egui::{pos2, vec2, Event, Modifiers, PointerButton, RawInput, Rect, Shape};

    let ctx = egui::Context::default();
    let (started_tx, started) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let mut check = Some(move || {
        started_tx.send(thread::current().id()).unwrap();
        release_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        Ok(None)
    });
    let mut updater = Updater::default();
    let mut time = 0.0;
    let mut draw = |events| {
        time += 0.1;
        ctx.run(
            RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::TopBottomPanel::top("test-menu").show(ctx, |ui| {
                    egui::menu::bar(ui, |ui| {
                        if help_menu(ui, true, None).check {
                            updater.open_with(ctx, check.take().expect("one check per click"));
                        }
                    });
                });
            },
        )
    };
    let point = |output: &egui::FullOutput, text: &str| {
        output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                Shape::Text(shape) if shape.galley.text() == text => {
                    Some(shape.pos + shape.galley.size() / 2.0)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing drawn text: {text}"))
    };
    let click = |pos: egui::Pos2| {
        vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ]
    };

    let help = point(&draw(vec![]), "Help");
    draw(click(help));
    let menu = draw(vec![]);
    point(&menu, "Controls");
    point(&menu, &format!("TAK Viewer v{}", crate::version::VERSION));
    let check_pos = point(&menu, MENU_ITEM);
    assert!(matches!(started.try_recv(), Err(TryRecvError::Empty)));
    draw(click(check_pos));
    let worker_id = started
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("click starts check");
    assert_ne!(worker_id, thread::current().id());
    assert!(matches!(updater.state, State::Checking));
    updater.open_with(&ctx, || panic!("second open must reuse the running check"));
    release.send(()).unwrap();
    let event = updater
        .worker
        .as_ref()
        .unwrap()
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(matches!(event, super::Event::Checked(Ok(None))));
}

#[cfg(windows)]
#[test]
fn windows_build_runs_the_quoted_batch_with_nopause() {
    let temp = tak_core::test_support::TempDir::new("update_batch");
    let source = temp.join("한글 소스 & checkout");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("빌드.cmd"), b"@echo off\r\nif /I not \"%~1\"==\"/nopause\" exit /b 7\r\necho batch received nopause\r\n").unwrap();
    let exe = windows_install_path(Some(temp.as_ref())).unwrap();
    let output = windows_build(&source, &exe).output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("batch received nopause"));
}
