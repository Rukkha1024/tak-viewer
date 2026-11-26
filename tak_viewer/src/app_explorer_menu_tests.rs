// Drive the painted Explorer and its focused key dispatch through real egui input frames.

fn right_click_explorer(ctx: &egui::Context, app: &mut TakViewerApp, at: egui::Pos2) -> egui::FullOutput {
    frame_with(ctx, app, vec![egui::Event::PointerMoved(at)]);
    for pressed in [true, false] {
        frame_with(ctx, app, vec![egui::Event::PointerButton {
            pos: at, button: egui::PointerButton::Secondary, pressed, modifiers: Modifiers::NONE,
        }]);
    }
    frame_with(ctx, app, vec![])
}

fn explorer_point(out: &egui::FullOutput, word: &str) -> egui::Pos2 {
    text_rects(out, word).first().unwrap_or_else(|| panic!("Missing Explorer text {word:?}")).center()
}

fn pick_explorer_menu(ctx: &egui::Context, app: &mut TakViewerApp, menu: &egui::FullOutput, word: &str) -> egui::FullOutput {
    let at = text_rects(menu, word).last().unwrap_or_else(|| panic!("Missing menu command {word:?}")).center();
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    frame_with(ctx, app, vec![egui::Event::PointerMoved(at)]);
    frame_with(ctx, app, vec![button(true)]);
    frame_with(ctx, app, vec![button(false)])
}

fn explorer_key(ctx: &egui::Context, app: &mut TakViewerApp, key: Key, modifiers: Modifiers) -> egui::FullOutput {
    let event = |pressed| egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers };
    frame_with(ctx, app, vec![event(true), event(false)])
}

fn explorer_wait(ctx: &egui::Context, app: &mut TakViewerApp,
    predicate: impl Fn(&TakViewerApp, &egui::FullOutput) -> bool) -> egui::FullOutput {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let out = frame_with(ctx, app, vec![]);
        if predicate(app, &out) { return out; }
        assert!(Instant::now() < deadline, "Explorer state did not arrive: {:?}", app.takm_note);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn explorer_test_workspace(ctx: &egui::Context, dir: &Path) -> TakViewerApp {
    let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
    assert!(app.open_workspace(dir.to_path_buf()));
    explorer_wait(ctx, &mut app, |app, _| !app.workspace.is_loading());
    app
}

#[test]
fn explorer_workspace_menu_dispatches_copy_path_name_paste_and_subfolder_add() {
    let dir = comment_dir("explorer_menu_copy");
    let source = dir.join("copy-source.takm");
    make_takm(&source);
    let sub = dir.join("destination");
    std::fs::create_dir(&sub).unwrap();
    let ctx = egui::Context::default();
    let mut app = explorer_test_workspace(&ctx, &dir);
    let out = frame_with(&ctx, &mut app, vec![]);
    let at = explorer_point(&out, "copy-source.takm");
    let menu = right_click_explorer(&ctx, &mut app, at);
    assert!(text_rects(&menu, "Paste").is_empty());
    assert!(text_rects(&menu, "Save as .takm").is_empty());
    assert_eq!(app.explorer.selection.selected, BTreeSet::from([source.clone()]));
    let copied = pick_explorer_menu(&ctx, &mut app, &menu, "Copy Path");
    assert_eq!(copied.platform_output.copied_text, source.display().to_string());
    let menu = right_click_explorer(&ctx, &mut app, at);
    let copied = pick_explorer_menu(&ctx, &mut app, &menu, "Copy Name");
    assert_eq!(copied.platform_output.copied_text, "copy-source.takm");
    let menu = right_click_explorer(&ctx, &mut app, at);
    click_at(&ctx, &mut app, explorer_point(&menu, "Copy"));
    assert_eq!(app.explorer.clipboard.as_ref().unwrap().paths, [source.clone()]);
    let out = frame_with(&ctx, &mut app, vec![]);
    let at = explorer_point(&out, "destination");
    let menu = right_click_explorer(&ctx, &mut app, at);
    click_at(&ctx, &mut app, explorer_point(&menu, "Paste"));
    until_files_done(&ctx, &mut app);
    assert_eq!(std::fs::read(sub.join("copy-source.takm")).unwrap(), std::fs::read(&source).unwrap());
    assert!(app.explorer.clipboard.is_some());
    let out = frame_with(&ctx, &mut app, vec![]);
    let menu = right_click_explorer(&ctx, &mut app, explorer_point(&out, "destination"));
    click_at(&ctx, &mut app, explorer_point(&menu, "Add to WORKSPACE"));
    assert!(app.workspace.roots().contains(&sub));
}

#[test]
fn explorer_workspace_multi_select_keys_match_the_batch_and_keep_3d_keys_separate() {
    let dir = HomeDir::new("workspace_multi");
    let a = dir.0.join("a.tak");
    let b = dir.0.join("b.tak");
    let c = dir.0.join("c.takm");
    for p in [&a, &b] { std::fs::write(p, tak_core::sample_tak::sample_tak()).unwrap(); }
    make_takm(&c);
    let ctx = egui::Context::default();
    let mut app = explorer_test_workspace(&ctx, &dir.0);
    let out = frame_with(&ctx, &mut app, vec![]);
    let ca = explorer_point(&out, "a.tak");
    let cb = explorer_point(&out, "b.tak");
    let cc = explorer_point(&out, "c.takm");
    click_with(&ctx, &mut app, ca, Modifiers::CTRL);
    click_with(&ctx, &mut app, cc, Modifiers::SHIFT);
    let menu = right_click_explorer(&ctx, &mut app, cb);
    for label in ["3 selected", "Save 2 TAK as .takm", "Set Progress", "Copy Paths", "Cut 1 .takm", "Copy 3", "Delete 1 .takm"] {
        assert!(!text_rects(&menu, label).is_empty(), "{label} absent");
    }
    click_at(&ctx, &mut app, explorer_point(&menu, "Cut 1 .takm"));
    assert_eq!(app.explorer.clipboard.as_ref().unwrap().paths, [c.clone()]);
    click_with(&ctx, &mut app, cb, Modifiers::CTRL);
    explorer_key(&ctx, &mut app, Key::A, Modifiers::CTRL);
    assert_eq!(app.explorer.selection.selected.len(), 3);
    explorer_key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
    assert!(app.explorer.selection.selected.is_empty());
    undo_key(&ctx, &mut app);
    assert_eq!(app.explorer.selection.selected.len(), 3);
    redo_key(&ctx, &mut app);
    assert!(app.explorer.selection.selected.is_empty());
    click_with(&ctx, &mut app, cc, Modifiers::CTRL);
    frame_with(&ctx, &mut app, vec![egui::Event::Copy]);
    assert_eq!(app.explorer.clipboard.as_ref().unwrap().paths, [c.clone()]);
    assert!(!app.explorer.clipboard.as_ref().unwrap().cut);
    frame_with(&ctx, &mut app, vec![egui::Event::Cut]);
    assert!(app.explorer.clipboard.as_ref().unwrap().cut);
    let shift_alt = Modifiers::SHIFT | Modifiers::ALT;
    let out = explorer_key(&ctx, &mut app, Key::C, shift_alt);
    assert_eq!(out.platform_output.copied_text, c.display().to_string());
    explorer_key(&ctx, &mut app, Key::R, shift_alt);
    assert_eq!(crate::file_ops::take_shown(), [c.clone()]);
    explorer_key(&ctx, &mut app, Key::Delete, Modifiers::NONE);
    until_files_done(&ctx, &mut app);
    assert!(!c.exists() && a.exists() && b.exists());
    undo_key(&ctx, &mut app);
    until_files_done(&ctx, &mut app);
    assert!(c.exists());
    let out = explorer_wait(&ctx, &mut app, |_, out| !text_rects(out, "c.takm").is_empty());
    click_with(&ctx, &mut app, explorer_point(&out, "a.tak"), Modifiers::CTRL);
    explorer_key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
    click_with(&ctx, &mut app, explorer_point(&out, "a.tak"), Modifiers::CTRL);
    explorer_key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    assert_eq!(app.view().target(), Some(a.as_path()));
    let out = frame_with(&ctx, &mut app, vec![]);
    click_with(&ctx, &mut app, explorer_point(&out, "b.tak"), Modifiers::CTRL);
    explorer_key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
    click_with(&ctx, &mut app, explorer_point(&out, "b.tak"), Modifiers::CTRL);
    explorer_key(&ctx, &mut app, Key::Enter, Modifiers::CTRL);
    assert_eq!(app.open_views().len(), 2);
    assert_eq!(app.view().target(), Some(b.as_path()));
}

#[test]
fn explorer_folder_search_chip_lists_nested_hits_and_clears_scope() {
    let dir = comment_dir("explorer_find_scope");
    let sub = dir.join("scope-child");
    std::fs::create_dir_all(sub.join("deep")).unwrap();
    make_takm(&sub.join("deep").join("inner-match.takm"));
    make_takm(&dir.join("outside.takm"));
    let ctx = egui::Context::default();
    let mut app = explorer_test_workspace(&ctx, &dir);
    let out = frame_with(&ctx, &mut app, vec![]);
    let menu = right_click_explorer(&ctx, &mut app, explorer_point(&out, "scope-child"));
    click_at(&ctx, &mut app, explorer_point(&menu, "Find in Folder…"));
    let out = explorer_wait(&ctx, &mut app, |_, out| !text_rects(out, "inner-match.takm").is_empty());
    assert_eq!(app.explorer.scope.as_deref(), Some(sub.as_path()));
    assert!(text_rects(&out, "outside.takm").is_empty());
    click_at(&ctx, &mut app, explorer_point(&out, "in scope-child ×"));
    assert!(app.explorer.scope.is_none());
    undo_key(&ctx, &mut app);
    assert_eq!(app.explorer.scope.as_deref(), Some(sub.as_path()));
}

#[test]
fn explorer_inline_rename_and_new_folder_validate_commit_cancel_and_keep_duplicate_draft() {
    let dir = comment_dir("explorer_inline_name");
    let source = dir.join("old.takm");
    let taken = dir.join("taken.takm");
    make_takm(&source);
    make_takm(&taken);
    std::fs::create_dir(dir.join("New Folder")).unwrap();
    let ctx = egui::Context::default();
    let mut app = explorer_test_workspace(&ctx, &dir);
    let out = frame_with(&ctx, &mut app, vec![]);
    let row = explorer_point(&out, "old.takm");
    click_with(&ctx, &mut app, row, Modifiers::CTRL);
    assert_eq!(key_focus::target(&ctx), key_focus::Target::Workspace);
    assert_eq!(app.explorer.selection.selected, BTreeSet::from([source.clone()]));
    explorer_key(&ctx, &mut app, Key::F2, Modifiers::NONE);
    assert_eq!(app.explorer.draft(), Some(("old.takm", None, false)),
        "F2 should start a file draft, focus={:?}, tab rename={}, focused folder={:?}",
        key_focus::target(&ctx), app.layout_strip.is_renaming(), app.explorer.focused_folder);
    frame_with(&ctx, &mut app, vec![]);
    assert!(key_focus::text_input_focused(&ctx));
    frame_with(&ctx, &mut app, vec![egui::Event::Text("wrong/name".into())]);
    assert_eq!(app.explorer.draft(), Some(("wrong/name", None, false)));
    explorer_key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    let out = frame_with(&ctx, &mut app, vec![]);
    assert!(!text_rects(&out, "이름에 '/'는 쓸 수 없어요.").is_empty(), "{:?}", app.explorer.draft());
    assert!(source.exists());
    explorer_key(&ctx, &mut app, Key::A, ctrl());
    frame_with(&ctx, &mut app, vec![egui::Event::Text("taken".into())]);
    assert_eq!(app.explorer.draft().map(|draft| draft.0), Some("taken"));
    explorer_key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    until_files_done(&ctx, &mut app);
    let out = frame_with(&ctx, &mut app, vec![]);
    assert!(!text_rects(&out, "같은 이름의 파일이 이미 있어요: taken.takm").is_empty());
    assert!(key_focus::text_input_focused(&ctx));
    explorer_key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
    assert!(source.exists());
    let out = frame_with(&ctx, &mut app, vec![]);
    let menu = right_click_explorer(&ctx, &mut app, explorer_point(&out, "old.takm"));
    click_at(&ctx, &mut app, explorer_point(&menu, "Rename…"));
    frame_with(&ctx, &mut app, vec![egui::Event::Text("renamed".into())]);
    explorer_key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    until_files_done(&ctx, &mut app);
    assert!(!source.exists() && dir.join("renamed.takm").exists());
    let root = workspace::folder_name(&dir).to_uppercase();
    let out = frame_with(&ctx, &mut app, vec![]);
    let menu = right_click_explorer(&ctx, &mut app, explorer_point(&out, &root));
    click_at(&ctx, &mut app, explorer_point(&menu, "New Folder…"));
    explorer_wait(&ctx, &mut app, |_, _| key_focus::text_input_focused(&ctx));
    assert!(!dir.join("New Folder 2").exists());
    explorer_key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
    assert!(!dir.join("New Folder 2").exists());
    let out = frame_with(&ctx, &mut app, vec![]);
    let menu = right_click_explorer(&ctx, &mut app, explorer_point(&out, &root));
    click_at(&ctx, &mut app, explorer_point(&menu, "New Folder…"));
    explorer_wait(&ctx, &mut app, |_, _| key_focus::text_input_focused(&ctx));
    click_at(&ctx, &mut app, egui::pos2(700.0, 90.0));
    until_files_done(&ctx, &mut app);
    assert!(dir.join("New Folder 2").is_dir());
}

#[test]
fn explorer_cut_paste_with_empty_system_clipboard_clears_and_undo_restores_file_clipboard() {
    let dir = comment_dir("explorer_paste_key");
    let source = dir.join("move-me.takm");
    let folder = dir.join("new-home");
    let destination = folder.join("move-me.takm");
    make_takm(&source);
    std::fs::create_dir(&folder).unwrap();
    let ctx = egui::Context::default();
    let mut app = explorer_test_workspace(&ctx, &dir);
    let out = frame_with(&ctx, &mut app, vec![]);
    click_with(&ctx, &mut app, explorer_point(&out, "move-me.takm"), Modifiers::CTRL);
    frame_with(&ctx, &mut app, vec![egui::Event::Cut]);
    click_at(&ctx, &mut app, explorer_point(&out, "new-home"));
    let release = egui::Event::Key { key: Key::V, physical_key: Some(Key::V), pressed: false, repeat: false, modifiers: Modifiers::CTRL };
    frame_with(&ctx, &mut app, vec![release]);
    until_files_done(&ctx, &mut app);
    assert!(destination.exists() && !source.exists());
    assert!(app.explorer.clipboard.is_none());
    undo_key(&ctx, &mut app);
    until_files_done(&ctx, &mut app);
    assert!(source.exists() && !destination.exists());
    assert_eq!(app.explorer.clipboard.as_ref().unwrap().paths, [source.clone()]);
    redo_key(&ctx, &mut app);
    until_files_done(&ctx, &mut app);
    assert!(destination.exists() && !source.exists());
    assert!(app.explorer.clipboard.is_none());
}

#[test]
fn explorer_batch_progress_opens_six_choices_and_changes_only_selected_takm() {
    let dir = comment_dir("explorer_progress_menu");
    let a = dir.join("a.tak");
    let b = dir.join("b.takm");
    let c = dir.join("c.takm");
    let original = tak_core::sample_tak::sample_tak();
    std::fs::write(&a, &original).unwrap();
    make_takm(&b);
    make_takm(&c);
    let ctx = egui::Context::default();
    let mut app = explorer_test_workspace(&ctx, &dir);
    let out = frame_with(&ctx, &mut app, vec![]);
    let a_at = explorer_point(&out, "a.tak");
    let b_at = explorer_point(&out, "b.takm");
    click_with(&ctx, &mut app, a_at, Modifiers::CTRL);
    click_with(&ctx, &mut app, b_at, Modifiers::CTRL);
    assert_eq!(app.explorer.selection.selected.len(), 2);
    let out = frame_with(&ctx, &mut app, vec![]);
    let menu = right_click_explorer(&ctx, &mut app, explorer_point(&out, "a.tak"));
    let at = explorer_point(&menu, "Set Progress");
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
    let choices = explorer_wait(&ctx, &mut app, |_, out| !text_rects(out, "Exported").is_empty());
    for word in ["Empty", "Recorded", "Reviewed", "Cleaned", "Labeled", "Exported"] {
        assert!(!text_rects(&choices, word).is_empty(), "missing Progress choice {word}");
    }
    pick_explorer_menu(&ctx, &mut app, &choices, "Labeled");
    explorer_wait(&ctx, &mut app, |app, _| app.explorer_progress_read.is_none() && app.progress_job.is_none() && app.progress_batch.is_none());
    assert_eq!(tak_core::tak::read_take(&b).unwrap().meta("Progress"), Some("4"));
    assert_ne!(tak_core::tak::read_take(&c).unwrap().meta("Progress"), Some("4"));
    assert_eq!(std::fs::read(a).unwrap(), original);
    undo_key(&ctx, &mut app);
    wait_for_progress(&ctx, &mut app, &b);
    assert_ne!(tak_core::tak::read_take(&b).unwrap().meta("Progress"), Some("4"));
    let out = frame_with(&ctx, &mut app, vec![]);
    let menu = right_click_explorer(&ctx, &mut app, explorer_point(&out, "a.tak"));
    pick_explorer_menu(&ctx, &mut app, &menu, "Save 1 TAK as .takm");
    until_batch_done(&ctx, &mut app);
    assert!(dir.join("a.takm").is_file());
}

#[test]
fn explorer_editor_menus_copy_overlay_close_others_saved_and_batch() {
    let dir = comment_dir("explorer_editor_menu");
    let ctx = egui::Context::default();
    let (mut app, files) = editors_app(&ctx, &dir);
    let at = editor_at(&ctx, &mut app, "b.tak");
    let menu = right_click_explorer(&ctx, &mut app, at);
    assert!(text_rects(&menu, "Save").is_empty());
    assert!(text_rects(&menu, "Show in WORKSPACE").is_empty());
    let copied = pick_explorer_menu(&ctx, &mut app, &menu, "Copy Name");
    assert_eq!(copied.platform_output.copied_text, "b.tak");
    let menu = right_click_explorer(&ctx, &mut app, at);
    pick_explorer_menu(&ctx, &mut app, &menu, "Remove Overlay");
    assert_eq!(app.views[0].overlay_target(), None);
    undo_key(&ctx, &mut app);
    explorer_wait(&ctx, &mut app, |app, _| app.views[0].overlay_target() == Some(files[1].as_path()));
    let at = editor_at(&ctx, &mut app, "c.tak");
    let menu = right_click_explorer(&ctx, &mut app, at);
    pick_explorer_menu(&ctx, &mut app, &menu, "Close Others");
    assert_eq!(app.editor_rows().len(), 1);
    undo_key(&ctx, &mut app);
    explorer_wait(&ctx, &mut app, |app, _| app.editor_rows().len() == 3);
    let at = editor_at(&ctx, &mut app, "c.tak");
    let menu = right_click_explorer(&ctx, &mut app, at);
    pick_explorer_menu(&ctx, &mut app, &menu, "Close Saved");
    assert!(app.editor_rows().is_empty());
    undo_key(&ctx, &mut app);
    explorer_wait(&ctx, &mut app, |app, _| app.editor_rows().len() == 3);
    let at = editor_at(&ctx, &mut app, "c.tak");
    click_with(&ctx, &mut app, at, Modifiers::CTRL);
    explorer_key(&ctx, &mut app, Key::A, Modifiers::CTRL);
    assert_eq!(app.editor_selection.selected.len(), 3);
    let at = editor_at(&ctx, &mut app, "c.tak");
    let menu = right_click_explorer(&ctx, &mut app, at);
    pick_explorer_menu(&ctx, &mut app, &menu, "Close 3");
    assert!(app.editor_rows().is_empty());
}

#[test]
fn explorer_ssh_menu_routes_remote_paths_and_server_conversion() {
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    let ssh = app.ssh.fake_open_list();
    let out = frame_with(&ctx, &mut app, vec![]);
    let at = explorer_point(&out, "gait_001.tak");
    let menu = right_click_explorer(&ctx, &mut app, at);
    assert!(text_rects(&menu, "Show Reason").is_empty());
    let copied = pick_explorer_menu(&ctx, &mut app, &menu, "Copy Path");
    assert_eq!(copied.platform_output.copied_text, "CHO_workstation:~/data/2_gait/gait_001.tak");
    let menu = right_click_explorer(&ctx, &mut app, at);
    let copied = pick_explorer_menu(&ctx, &mut app, &menu, "Copy Name");
    assert_eq!(copied.platform_output.copied_text, "gait_001.tak");
    let menu = right_click_explorer(&ctx, &mut app, at);
    pick_explorer_menu(&ctx, &mut app, &menu, "Save as .takm");
    assert_eq!(ssh.converted().path, "~/data/2_gait/gait_001.tak");
    let menu = right_click_explorer(&ctx, &mut app, at);
    pick_explorer_menu(&ctx, &mut app, &menu, "Open");
    assert_eq!(ssh.opened().1.path, "~/data/2_gait/gait_001.tak");
}

#[test]
fn explorer_saved_root_menu_renames_resets_converts_and_removes() {
    let temp = comment_dir("explorer_root_menu");
    let dir = temp.join("trials");
    std::fs::create_dir(&dir).unwrap();
    let source = dir.join("root-take.tak");
    std::fs::write(&source, tak_core::sample_tak::sample_tak()).unwrap();
    let ctx = egui::Context::default();
    let mut app = explorer_test_workspace(&ctx, &dir);
    let own = workspace::folder_name(&dir);
    let out = frame_with(&ctx, &mut app, vec![]);
    let menu = right_click_explorer(&ctx, &mut app, explorer_point(&out, &own.to_uppercase()));
    assert!(text_rects(&menu, &format!("Reset Name ({own})")).is_empty());
    pick_explorer_menu(&ctx, &mut app, &menu, "Save All TAK as .takm");
    until_batch_done(&ctx, &mut app);
    assert!(dir.join("root-take.takm").is_file());
    let out = frame_with(&ctx, &mut app, vec![]);
    let menu = right_click_explorer(&ctx, &mut app, explorer_point(&out, &own.to_uppercase()));
    click_at(&ctx, &mut app, explorer_point(&menu, "Rename Shown Name…"));
    frame_with(&ctx, &mut app, vec![egui::Event::Text("Trial files".into())]);
    explorer_key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    let out = frame_with(&ctx, &mut app, vec![]);
    assert_eq!(app.workspace.name_of(&dir), Some("Trial files"));
    let menu = right_click_explorer(&ctx, &mut app, explorer_point(&out, "Trial files"));
    pick_explorer_menu(&ctx, &mut app, &menu, &format!("Reset Name ({own})"));
    assert_eq!(app.workspace.name_of(&dir), None);
    let out = frame_with(&ctx, &mut app, vec![]);
    let menu = right_click_explorer(&ctx, &mut app, explorer_point(&out, &own.to_uppercase()));
    pick_explorer_menu(&ctx, &mut app, &menu, "Remove from WORKSPACE");
    assert!(app.workspace.roots().is_empty());
    assert!(source.exists());
}

#[test]
fn explorer_ssh_folder_menu_copies_the_address_and_saves_the_root() {
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.ssh.set_demo("top_linux");
    app.ssh.collapsed = false;
    let out = frame_with(&ctx, &mut app, vec![]);
    let at = explorer_point(&out, "home");
    let menu = right_click_explorer(&ctx, &mut app, at);
    let copied = pick_explorer_menu(&ctx, &mut app, &menu, "Copy Path");
    assert_eq!(copied.platform_output.copied_text, "CHO_workstation:/home");
    let menu = right_click_explorer(&ctx, &mut app, at);
    pick_explorer_menu(&ctx, &mut app, &menu, "Add to WORKSPACE");
    let (root, _) = workspace::remote_address(&app.workspace.roots()[0]).unwrap();
    assert_eq!((root.alias.as_str(), root.path.as_str(), root.platform.as_str()), ("CHO_workstation", "/home", "linux"));
}

#[test]
fn explorer_workspace_open_overlay_tab_and_single_conversion_reach_the_app() {
    let dir = comment_dir("explorer_open_menu");
    for name in ["a.tak", "d.tak"] {
        std::fs::write(dir.join(name), tak_core::sample_tak::sample_tak()).unwrap();
    }
    for name in ["b.takm", "c.takm"] { make_takm(&dir.join(name)); }
    let ctx = egui::Context::default();
    let mut app = explorer_test_workspace(&ctx, &dir);
    let choose = |app: &mut TakViewerApp, file: &str, command: &str| {
        let explorer = place_of(app, Pane::Explorer);
        let out = frame_with(&ctx, app, vec![]);
        let workspace_top = explorer_point(&out, "WORKSPACE (1/10)").y;
        let at = text_rects(&out, file).into_iter().map(|rect| rect.center())
            .find(|p| explorer.contains(*p) && p.y > workspace_top).expect("WORKSPACE file");
        let menu = right_click_explorer(&ctx, app, at);
        pick_explorer_menu(&ctx, app, &menu, command);
    };
    choose(&mut app, "a.tak", "Open Here");
    explorer_wait(&ctx, &mut app, |app, _| app.view().path() == Some(dir.join("a.tak").as_path()));
    choose(&mut app, "b.takm", "Add Overlay");
    assert_eq!(app.views[0].overlay_target(), Some(dir.join("b.takm").as_path()));
    choose(&mut app, "c.takm", "Open Beside");
    assert_eq!(app.open_views().len(), 2);
    let tabs = app.layout_tabs.tabs().len();
    choose(&mut app, "d.tak", "Open in New Tab");
    assert_eq!(app.layout_tabs.tabs().len(), tabs + 1);
    explorer_wait(&ctx, &mut app, |app, _| app.view().path() == Some(dir.join("d.tak").as_path()));
    choose(&mut app, "d.tak", "Save as .takm");
    until_batch_done(&ctx, &mut app);
    assert!(dir.join("d.takm").exists());
}

#[test]
fn explorer_editor_menu_saves_the_addressed_view_and_reveals_its_workspace_row() {
    let tmp = comment_dir("explorer_editor_save");
    let dir = tmp.join("trials");
    std::fs::create_dir(&dir).unwrap();
    let ctx = egui::Context::default();
    let (mut app, files) = editors_app(&ctx, &dir);
    editor_batch_prepare_dirty(&mut app, 0);
    let editor_row = |app: &mut TakViewerApp, prefix: &str| {
        let explorer = place_of(app, Pane::Explorer);
        let out = frame_with(&ctx, app, vec![]);
        editor_texts(&out, explorer).into_iter().find(|(_, label, _)| label.starts_with(prefix))
            .unwrap_or_else(|| panic!("OPEN EDITORS file {prefix:?} missing")).0.center()
    };
    let at = editor_row(&mut app, "a.tak*");
    let menu = right_click_explorer(&ctx, &mut app, at);
    pick_explorer_menu(&ctx, &mut app, &menu, "Save");
    assert_eq!(app.active, 0);
    assert!(app.open_workspace(dir));
    let at = editor_row(&mut app, "c.tak");
    let menu = right_click_explorer(&ctx, &mut app, at);
    pick_explorer_menu(&ctx, &mut app, &menu, "Show in WORKSPACE");
    assert_eq!(app.explorer.reveal.as_ref(), Some(&files[2]));
}
