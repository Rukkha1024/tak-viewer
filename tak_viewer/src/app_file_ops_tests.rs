// EXPLORER file changes in the whole app (backend; no menu yet): background work, one undo step
// each, Ctrl+Z / Ctrl+Y run the inverse, open files are refused. Included in `app.rs` tests.

/// A test folder where a trash exists (Linux home / Windows temp drive), removed after.
struct HomeDir(PathBuf);
impl HomeDir {
    fn new(name: &str) -> Self {
        let p = crate::file_ops::tests::test_base().join("tak_viewer_app_file_ops").join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        HomeDir(p)
    }
}
impl Drop for HomeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn until_files_done(ctx: &egui::Context, app: &mut TakViewerApp) {
    let deadline = Instant::now() + Duration::from_secs(20);
    frame_with(ctx, app, vec![]);
    while app.file_working() {
        assert!(Instant::now() < deadline, "file change did not finish");
        frame_with(ctx, app, vec![]);
    }
}

#[test]
fn file_changes_run_in_the_background_one_undo_step_each_and_ctrl_z_ctrl_y_replay_them() {
    use crate::file_ops::Op;
    let ctx = egui::Context::default();
    let d = HomeDir::new("replay");
    let (a, b) = (d.0.join("a.takm"), d.0.join("b.takm"));
    std::fs::write(&a, b"takm bytes").unwrap();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    frame_with(&ctx, &mut app, vec![]);
    let before = app.history.kinds().len();
    // Rename, then a new folder, then trash: three steps.
    app.file_op(&ctx, Op::Rename { from: a.clone(), name: "b".into() });
    assert!(app.file_working(), "runs on a worker");
    until_files_done(&ctx, &mut app);
    assert!(b.is_file() && !a.exists());
    assert_eq!(takm_note(&app), "Renamed a.takm to b.takm · Ctrl+Z");
    app.file_op(&ctx, Op::NewFolder { parent: d.0.clone(), name: "sub".into() });
    until_files_done(&ctx, &mut app);
    app.file_op(&ctx, Op::Trash { paths: vec![b.clone()] });
    until_files_done(&ctx, &mut app);
    assert!(!b.exists() && d.0.join("sub").is_dir());
    assert_eq!(takm_note(&app), "Moved 1 to trash · Ctrl+Z");
    let kinds = app.history.kinds();
    assert_eq!(kinds[before..], ["Files", "Files", "Files"]);
    // Ctrl+Z three times: back from the trash, folder gone, old name.
    undo_key(&ctx, &mut app);
    until_files_done(&ctx, &mut app);
    assert_eq!((b.is_file(), note(&app).as_str()), (true, "Undo · Added 1 file"));
    undo_key(&ctx, &mut app);
    until_files_done(&ctx, &mut app);
    assert!(!d.0.join("sub").exists());
    undo_key(&ctx, &mut app);
    until_files_done(&ctx, &mut app);
    assert!(a.is_file() && !b.exists());
    assert_eq!(std::fs::read(&a).unwrap(), b"takm bytes");
    assert_eq!(app.history.kinds().len(), before, "all three undone");
    // Ctrl+Y three times: the same again, in order.
    for _ in 0..3 {
        redo_key(&ctx, &mut app);
        until_files_done(&ctx, &mut app);
    }
    assert!(!a.exists() && !b.exists() && d.0.join("sub").is_dir(), "renamed, folder made, trashed again");
    assert_eq!(app.history.kinds()[before..], ["Files", "Files", "Files"]);
    // Clean the home trash entry this test left.
    undo_key(&ctx, &mut app);
    until_files_done(&ctx, &mut app);
    assert!(b.is_file());
}

#[test]
fn open_files_and_tak_files_are_refused_with_the_reason_and_nothing_changes() {
    use crate::file_ops::Op;
    let ctx = egui::Context::default();
    let _dir = comment_dir("file_ops_open");
    let dir = _dir.to_path_buf();
    let takm = dir.join("a.takm");
    make_takm(&takm);
    let tak = dir.join("t.tak");
    std::fs::write(&tak, b"tak").unwrap();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.hold_loads = true;
    frame_with(&ctx, &mut app, vec![]);
    app.open(&ctx, takm.clone());
    frame_with(&ctx, &mut app, vec![]);
    arrive(&ctx, &mut app, 0);
    let before = app.history.kinds();
    app.file_op(&ctx, Op::Rename { from: takm.clone(), name: "b".into() });
    assert!(!app.file_working());
    assert_eq!(takm_note(&app), "열려 있는 파일은 닫은 뒤 해 주세요: a.takm");
    app.file_op(&ctx, Op::Trash { paths: vec![tak.clone()] });
    until_files_done(&ctx, &mut app);
    assert_eq!(takm_note(&app), "t.tak: TAK 파일은 옮기거나 이름을 바꾸거나 지울 수 없어요. 복사는 돼요.");
    assert!(takm.is_file() && tak.is_file());
    assert_eq!(app.history.kinds(), before, "nothing changed: no undo step");
    // Copying an open file only reads it: allowed.
    std::fs::create_dir(dir.join("out")).unwrap();
    app.file_op(&ctx, Op::Paste { sources: vec![takm.clone(), tak.clone()], dest: dir.join("out"), cut: false });
    until_files_done(&ctx, &mut app);
    assert!(dir.join("out").join("a.takm").is_file() && dir.join("out").join("t.tak").is_file());
    assert_eq!(takm_note(&app), "Added 2 files · Ctrl+Z");
}

fn until_batch_done(ctx: &egui::Context, app: &mut TakViewerApp) {
    let deadline = Instant::now() + Duration::from_secs(60);
    frame_with(ctx, app, vec![]);
    while app.takm_batch.is_some() || app.takm_job.is_some() {
        assert!(Instant::now() < deadline, "batch did not finish: {:?}", app.takm_working());
        frame_with(ctx, app, vec![]);
    }
}

#[test]
fn many_tak_files_are_saved_as_takm_one_after_another_skipping_ones_that_have_a_takm() {
    let ctx = egui::Context::default();
    let _dir = comment_dir("takm_batch");
    let dir = _dir.to_path_buf();
    for name in ["a.tak", "b.tak", "c.tak"] {
        std::fs::write(dir.join(name), tak_core::sample_tak::sample_tak()).unwrap();
    }
    make_takm(&dir.join("b.takm"));
    let b_before = std::fs::read(dir.join("b.takm")).unwrap();
    make_takm(&dir.join("x.takm"));
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.hold_loads = true;
    frame_with(&ctx, &mut app, vec![]);
    app.save_takm_many(&ctx, vec![dir.join("a.tak"), dir.join("b.tak"), dir.join("c.tak"), dir.join("x.takm")]);
    assert_eq!(app.takm_working().as_deref(), Some("Saving 1/3 as .takm… a.tak"));
    until_batch_done(&ctx, &mut app);
    assert_eq!(takm_note(&app), "Saved 2 · Skipped 2 (already .takm)");
    assert!(dir.join("a.takm").is_file() && dir.join("c.takm").is_file());
    assert_eq!(std::fs::read(dir.join("b.takm")).unwrap(), b_before, "never overwritten");
    assert!(app.views.iter().all(|v| v.target().is_none()), "nothing opened");
    // The folder command: the remaining TAK files directly in it (all have .takm now).
    std::fs::write(dir.join("d.tak"), tak_core::sample_tak::sample_tak()).unwrap();
    app.save_folder_takm(&ctx, dir.clone());
    until_batch_done(&ctx, &mut app);
    assert_eq!(takm_note(&app), "Saved 1 · Skipped 3 (already .takm)");
    assert!(dir.join("d.takm").is_file());
}

#[test]
fn open_editors_close_others_saved_and_all_are_one_undo_step_each_and_show_in_workspace_reveals() {
    let ctx = egui::Context::default();
    let _d = comment_dir("editors_close_backend");
    let dir = _d.to_path_buf();
    let (mut app, f) = editors_app(&ctx, &dir);
    let names = |app: &TakViewerApp| app.editor_rows().into_iter().map(|r| crate::file_ops::name_of(&r.path)).collect::<Vec<_>>();
    assert_eq!(names(&app), ["a.tak", "b.tak", "c.tak"]);
    let before = app.history.kinds().len();
    // Close Others on c.tak's view: a.tak's view (with b.tak laid over it) goes.
    let c = app.editor_rows().into_iter().find(|r| r.path == f[2]).unwrap();
    app.close_other_editors(&ctx, c.tab_id, c.slot);
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(names(&app), ["c.tak"]);
    assert_eq!(app.history.kinds()[before..], ["EditorBatch"]);
    undo_key(&ctx, &mut app);
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(names(&app), ["a.tak", "b.tak", "c.tak"], "one Ctrl+Z brings both back");
    // Nothing unsaved: Close Saved closes all; Close All too.
    app.close_saved_editors(&ctx);
    frame_with(&ctx, &mut app, vec![]);
    assert!(names(&app).is_empty());
    undo_key(&ctx, &mut app);
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(names(&app).len(), 3);
    app.close_all_editors(&ctx);
    frame_with(&ctx, &mut app, vec![]);
    assert!(names(&app).is_empty());
    // Show in WORKSPACE asks the tree to open down to the file.
    app.show_in_workspace(f[0].clone());
    assert_eq!(app.explorer.reveal.as_deref(), Some(f[0].as_path()));
    // Add to WORKSPACE for a folder.
    let sub = dir.join("sub");
    std::fs::create_dir(&sub).unwrap();
    app.add_folder_to_workspace(&ctx, sub.clone());
    assert!(app.workspace.roots().contains(&sub));
}

#[test]
fn progress_on_many_takm_files_is_one_undo_step_and_tak_files_are_left_out() {
    use crate::take_progress::{Pick, State, Target};
    let ctx = egui::Context::default();
    let _d = comment_dir("progress_many");
    let dir = _d.to_path_buf();
    let files: Vec<PathBuf> = ["a.takm", "b.takm", "c.takm"].iter().map(|n| dir.join(n)).collect();
    for f in &files { make_takm(f); }
    let tak = dir.join("t.tak");
    std::fs::write(&tak, tak_core::sample_tak::sample_tak()).unwrap();
    let progress = |p: &Path| tak_core::tak::read_take(p).unwrap().meta("Progress").map(str::to_owned);
    let start: Vec<_> = files.iter().map(|f| progress(f)).collect();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    frame_with(&ctx, &mut app, vec![]);
    let before = app.history.kinds().len();
    let picks = files.iter().chain([&tak]).map(|p| Pick { target: Target::Local(p.clone()), before: State(None), after: State(Some(4)), stamp: None }).collect();
    app.set_progress_many(&ctx, picks);
    let deadline = Instant::now() + Duration::from_secs(30);
    while app.progress_batch.is_some() || app.progress_job.is_some() {
        assert!(Instant::now() < deadline);
        frame_with(&ctx, &mut app, vec![]);
    }
    assert!(files.iter().all(|f| progress(f).as_deref() == Some("4")), "{:?}", app.takm_note);
    assert_eq!(takm_note(&app), "Labeled · 3 files");
    assert_eq!(app.history.kinds()[before..], ["Progress+Progress+Progress"], "one step");
    let wait = |app: &mut TakViewerApp| {
        let deadline = Instant::now() + Duration::from_secs(30);
        frame_with(&ctx, app, vec![]);
        while app.progress_batch.is_some() || app.progress_job.is_some() {
            assert!(Instant::now() < deadline);
            frame_with(&ctx, app, vec![]);
        }
    };
    undo_key(&ctx, &mut app);
    wait(&mut app);
    assert_eq!(files.iter().map(|f| progress(f)).collect::<Vec<_>>(), start, "one Ctrl+Z: every file back");
    assert_eq!(note(&app), "Undo · Empty · 3 files");
    redo_key(&ctx, &mut app);
    wait(&mut app);
    assert!(files.iter().all(|f| progress(f).as_deref() == Some("4")));
    assert_eq!(app.history.kinds()[before..], ["Progress+Progress+Progress"]);
}

#[test]
fn an_unopened_server_tak_needs_a_connection_and_a_takm_is_not_converted_again() {
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    frame_with(&ctx, &mut app, vec![]);
    let at = |path: &str| crate::ssh::RemoteLocation { host: "lab".into(), path: path.into() };
    app.save_remote_takm(&ctx, at("/data/a.takm"));
    assert_eq!(takm_note(&app), "이미 .takm 파일이에요.");
    app.save_remote_takm(&ctx, at("/data/a.tak"));
    assert_eq!(takm_note(&app), "SSH를 다시 연결한 뒤 .takm으로 저장해 주세요.");
    assert!(app.remote_convert.is_none());
}

#[test]
fn find_in_folder_searches_every_subfolder_in_the_background() {
    let ctx = egui::Context::default();
    let _d = comment_dir("find_in_folder");
    let dir = _d.to_path_buf();
    std::fs::create_dir_all(dir.join("s").join("deep")).unwrap();
    for p in ["walk1.tak", "s/walk2.takm", "s/deep/Walk3.tak", "s/run.tak"] {
        std::fs::write(dir.join(p), b"x").unwrap();
    }
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.find_in_folder(&ctx, dir.clone(), "WALK".into());
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.folder_find_running() {
        assert!(Instant::now() < deadline);
        frame_with(&ctx, &mut app, vec![]);
    }
    let find = app.folder_find.as_ref().unwrap();
    assert_eq!(find.found, vec![dir.join("s/deep/Walk3.tak"), dir.join("s/walk2.takm"), dir.join("walk1.tak")]);
    assert!(!find.cut);
}
