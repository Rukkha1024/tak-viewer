#[test]
fn progress_worker_waits_for_explorer_and_foreground_open() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let folder = PathBuf::from("progress_fixture");
    let files = (0..1000)
        .map(|n| workspace::WsFile {
            path: folder.join(format!("{n:04}.takm")),
            name: format!("{n:04}.takm"),
            size: 100,
            modified: Some(std::time::UNIX_EPOCH),
        })
        .collect();
    let listings = [(
        folder.clone(),
        workspace::Listing::Ready {
            dirs: vec![],
            files,
        },
    )]
    .into();
    app.workspace = workspace::Workspace::from_listings(vec![folder.clone()], listings);
    app.explorer.expand(&mut app.workspace, &folder);

    dock::hide(&mut app.dock, Pane::Explorer);
    frame_with(&ctx, &mut app, vec![]);
    assert!(!app.explorer.progress.worker_started());

    app.hold_loads = true;
    app.open_in(&ctx, 0, folder.join("0000.takm"));
    app.show_pane(Pane::Explorer);
    frame_with(&ctx, &mut app, vec![]);
    assert!(app.views[0].loading.is_some());
    assert!(!app.explorer.progress.worker_started());

    app.views[0].loading = None;
    frame_with(&ctx, &mut app, vec![]);
    assert!(app.explorer.progress.worker_started());
}
