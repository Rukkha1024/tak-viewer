#[test]
fn reopen_tab_key_obeys_cap_and_keeps_refused_tab_in_history() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.hold_loads = true;
    let original = app.layout_tabs.active_id().unwrap();
    let document = app.views[0].document_id().unwrap();
    app.close_layout_tab(original);

    let files: Vec<_> = (0..MAX_OPEN_FILES - 1)
        .map(|index| PathBuf::from(format!("reopen_limit_{index}.tak"))).collect();
    app.open_shot_tabs(&ctx, &files);
    let active = app.layout_tabs.active_id();
    let reopen = [(Modifiers { shift: true, ..ctrl() }, Key::T)];
    run_frame(&ctx, &mut app, &reopen);
    assert_eq!(app.open_file_identities(None).len(), MAX_OPEN_FILES);
    assert_eq!(app.layout_tabs.active_id(), Some(original));
    assert_eq!(app.views[0].document_id(), Some(document));

    app.close_layout_tab(original);
    app.switch_layout_tab_internal(active.unwrap());
    assert_eq!(app.layout_tabs.active_id(), active);
    app.open_side(&ctx, "reopen_limit_last.tak".into());
    assert_eq!(app.open_file_identities(None).len(), MAX_OPEN_FILES);
    let file_id = app.next_file_id;
    let live = app.open_file_identities(None);
    let pushed = app.history.pushed;
    run_frame(&ctx, &mut app, &reopen);
    assert!(app.layout_tabs.tab(original).is_none());
    assert_eq!(app.layout_tabs.active_id(), active);
    assert_eq!(app.open_file_identities(None), live);
    assert_eq!(app.next_file_id, file_id);
    assert_eq!(app.history.pushed, pushed);
    assert!(app.notice.as_ref().is_some_and(|s| s.contains("30개까지")));
    assert!(matches!(app.layout_tabs.last_closed(), Some(layout_tabs::ClosedItem::Tab(tab)) if tab.tab.id == original));
}

#[test]
fn settings_restore_caps_distinct_sources_before_starting_file_loads() {
    let ctx = egui::Context::default();
    let dir = comment_dir("settings_file_cap");
    let names: Vec<_> = (0..=MAX_OPEN_FILES).map(|index| format!("saved_{index}.tak")).collect();
    let names: Vec<_> = names.iter().map(String::as_str).collect();
    let files = take_files(&dir, &names);
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.hold_loads = true;
    app.settings.layout_tabs = Some(files.chunks(MAX_VIEWS).map(|group| {
        let slots: Vec<_> = (0..group.len() as u8).collect();
        SavedLayoutTab {
            layout: dock::editor_layout(&dock::default_layout_for(&slots)),
            views: group.iter().enumerate().map(|(slot, path)| SavedLayoutView {
                slot: slot as u8, path: path.to_string_lossy().into_owned(), spot: None,
            }).collect(),
            ..Default::default()
        }
    }).collect());
    app.restore_layout_tabs(&ctx);
    let files_open = app.open_file_identities(None);
    assert_eq!(files_open.len(), MAX_OPEN_FILES);
    let readers = app.views.iter().chain(app.layout_tabs.tabs().iter()
        .filter_map(|tab| tab.layout.as_ref()).flat_map(|runtime| runtime.views.iter()));
    assert_eq!(readers.filter(|view| view.loading.is_some()).count(), MAX_OPEN_FILES);
    assert!(!files_open.contains(&app.known_identity(&files[MAX_OPEN_FILES])));
    assert!(app.notice.as_ref().is_some_and(|s| s.contains("30개까지")));
}

#[test]
fn reopening_a_tab_with_a_pending_replacement_counts_its_displayed_file() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.hold_loads = true;
    let original = app.layout_tabs.active_id().unwrap();
    let files: Vec<_> = (0..MAX_OPEN_FILES).map(|index| PathBuf::from(format!("pending_restore_{index}.tak"))).collect();
    app.views[0].wait_for_shared(files[0].clone(), None);
    assert!(app.close_layout_tab_internal(original));
    app.open_shot_tabs(&ctx, &files);
    let live = app.open_file_identities(None);
    assert_eq!(live.len(), MAX_OPEN_FILES);
    run_frame(&ctx, &mut app, &[(Modifiers { shift: true, ..ctrl() }, Key::T)]);
    assert!(app.layout_tabs.tab(original).is_none());
    assert_eq!(app.open_file_identities(None), live);
    assert!(app.notice.as_ref().is_some_and(|s| s.contains("30개까지")));
}
