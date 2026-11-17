// SSH roots retain the existing folder undo/persistence format without starting SSH.
#[test]
fn remote_workspace_ron_restore_is_collapsed_and_never_starts_network() {
    let ctx = egui::Context::default();
    let root = workspace::RemoteRoot { alias: "lab-server".into(), path: r"E:\take folders\보행".into(), platform: "windows".into() };
    let key = root.key();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    assert_eq!(app.workspace.add_root(key.clone()), AddRoot::Added);
    app.workspace.set_name(&key, "시험 SSH");
    app.sync_settings();
    assert!(app.settings.recent_workspaces.is_empty());
    // Even old/bad settings claiming expansion must not cause startup network work.
    app.settings.workspace_open.push(key.to_string_lossy().into_owned());
    let mut storage = FileLike::default();
    eframe::set_value(&mut storage, eframe::APP_KEY, &app.settings);
    let mut restored = TakViewerApp::build(&ctx, Some(&storage), vec![], None, None);
    assert_eq!(restored.workspace.roots(), &[key.clone()]);
    assert_eq!(restored.workspace.name_of(&key), Some("시험 SSH"));
    assert_eq!(workspace::remote_address(&key).unwrap().0, root);
    assert!(!restored.explorer.folds().0.contains(&key));
    assert!(restored.workspace.listing(&key).is_none());
    restored.workspace.poll_remote(&mut restored.ssh);
    assert!(!restored.ssh.is_busy());
    assert!(!restored.ssh.workers_running());
    assert!(restored.ssh.hosts.is_empty());
    restored.workspace.start_test_watch();
    restored.workspace.watch(&[key.clone()]);
    restored.workspace.round_now();
    assert!(!restored.workspace.poll(), "remote roots never reach disk polling");
    assert!(!restored.workspace.is_missing(&key));
    let out = frame_with(&ctx, &mut restored, vec![]);
    assert!(!text_rects(&out, "not connected").is_empty());
}

#[test]
fn remote_workspace_add_rename_remove_keep_the_local_undo_and_cap() {
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.quiet();
    let location = crate::ssh::RemoteLocation { host: "lab-server".into(), path: r"E:\agent".into() };
    app.add_remote_workspace(&ctx, location.clone(), "windows".into());
    let key = app.workspace.roots()[0].clone();
    assert!(app.workspace.listing(&key).is_none(), "adding is dormant until expand");
    app.undo_redo(&ctx, false);
    assert!(app.workspace.roots().is_empty());
    app.undo_redo(&ctx, true);
    assert_eq!(app.workspace.roots(), &[key.clone()]);
    app.workspace.set_name(&key, "별칭");
    app.observe(true);
    app.undo_redo(&ctx, false);
    assert_eq!(app.workspace.name_of(&key), None);
    app.undo_redo(&ctx, true);
    assert_eq!(app.workspace.name_of(&key), Some("별칭"));
    app.explorer.remove_root(&mut app.workspace, &key);
    app.observe(true);
    app.undo_redo(&ctx, false);
    assert_eq!(app.workspace.roots(), &[key.clone()]);
    assert_eq!(app.workspace.name_of(&key), Some("별칭"));
    for n in 1..MAX_ROOTS { assert_eq!(app.workspace.add_root(PathBuf::from(format!("/local{n}"))), AddRoot::Added); }
    let other = workspace::RemoteRoot { alias: "other".into(), path: "/data".into(), platform: "linux".into() };
    assert_eq!(app.workspace.add_root(other.key()), AddRoot::Full);
    assert_eq!(app.workspace.roots().len(), MAX_ROOTS);
    assert!(app.settings.recent_workspaces.is_empty());
}

#[test]
fn remote_workspace_child_keys_retain_server_address_and_reject_traversal() {
    for (platform, folder, expected) in [("windows", r"E:\data", r"E:\data\sub\a.takm"), ("linux", "/home/a/data", "/home/a/data/sub/a.takm")] {
        let root = workspace::RemoteRoot { alias: "test".into(), path: folder.into(), platform: platform.into() };
        let key = root.key().join("sub").join("a.takm");
        let (_, location) = workspace::remote_address(&key).unwrap();
        assert_eq!(location.path, expected);
        let mut ws = Workspace::from_listings(vec![root.key()], Default::default());
        assert_eq!(ws.key_for_remote(&location), Some(key));
        assert!(workspace::remote_address(&root.key().join("..").join("outside.takm")).is_none());
        ws.request(&root.key());
        assert!(ws.is_loading());
    }
}

#[test]
fn remote_workspace_highlight_prefers_the_deepest_saved_remote_root() {
    let root = workspace::RemoteRoot { alias: "test".into(), path: "/data".into(), platform: "linux".into() };
    let nested = workspace::RemoteRoot { path: "/data/sub".into(), ..root.clone() };
    let ws = Workspace::from_listings(vec![root.key(), nested.key()], Default::default());
    let location = crate::ssh::RemoteLocation { host: "test".into(), path: "/data/sub/a.takm".into() };
    assert_eq!(ws.key_for_remote(&location), Some(nested.key().join("a.takm")));
}
