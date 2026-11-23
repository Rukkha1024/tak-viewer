// Regression coverage for ordinary opens versus retained close-history restoration.

mod freshness_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tak_core::edit::{delete::{delete_samples, DeleteRequest}, save::{FileVersion, SaveOperation, SaveRequest, TakmSave}, EditDocument, EditPatch, EditSelection, PreparedInput};

    fn wait(ctx: &egui::Context, app: &mut TakViewerApp, ready: impl Fn(&TakViewerApp) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready(app) {
            frame_with(ctx, app, vec![]);
            assert!(app.error.is_none(), "{:?}", app.error);
            assert!(Instant::now() < deadline, "fresh-open reader did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn opened(ctx: &egui::Context, path: &Path) -> TakViewerApp {
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        app.set_command_run();
        app.open(ctx, path.to_path_buf());
        wait(ctx, &mut app, |app| app.views[0].data.is_some() && !app.views[0].busy());
        app
    }

    fn add_disk_comment(path: &Path, id: u64, text: &str) {
        let fixed = tak_core::takm::fixed_len(path).unwrap();
        tak_core::takm::append_events(path, &[comments::Event {
            time: "2026-11-16T00:00:00+09:00".into(), kind: comments::Kind::Add, id,
            markers: vec![], start: 0, end: 0, comment_type: String::new(), text: text.into(),
            shown_a: "1".into(), shown_b: String::new(),
            file_a: comments::file_label(path, fixed), file_b: String::new(),
            operation: None, expected: None,
        }]).unwrap();
    }

    fn delete_patch(document: &mut EditDocument, path: &Path) -> EditPatch {
        let (prepared, _) = PreparedInput::read_from_path(path).unwrap();
        document.install_prepared(document.revision(), prepared).unwrap();
        let snapshot = document.snapshot();
        let uid = snapshot.track_order()[0];
        delete_samples(&snapshot, &DeleteRequest {
            selection: EditSelection { selected: [uid].into_iter().collect(), ..Default::default() },
            range: None, current_frame: 0,
        }).unwrap().patch.unwrap()
    }

    fn save_document(document: &mut EditDocument, path: &Path) {
        let version = FileVersion::capture(path).unwrap();
        let snapshot = document.snapshot();
        TakmSave.save(SaveRequest {
            snapshot: snapshot.clone(), source: path.to_path_buf(), destination: path.to_path_buf(),
            expected_source: version.clone(), expected_destination: version,
        }).unwrap();
        document.mark_saved(&snapshot, path.to_path_buf()).unwrap();
    }

    fn save_deleted_marker(view: &mut View, path: &Path) -> (views::EditSession, EditPatch) {
        let session = view.edit_document.clone().unwrap();
        let patch = delete_patch(&mut session.lock().unwrap(), path);
        view.apply_marker_patch(&patch, true).unwrap();
        let edited = view.data.as_ref().unwrap().rebuild_markers(&patch.after_snapshot()).unwrap();
        view.publish_edited_view(edited).unwrap();
        save_document(&mut session.lock().unwrap(), path);
        let meta = std::fs::metadata(path).unwrap();
        *view.source_stamp.as_ref().unwrap().lock().unwrap() = Some((tak_core::takm::canonical_target(path).unwrap(), meta.len(), Some(meta.modified().unwrap())));
        assert!(!view.marker_dirty());
        (session, patch)
    }

    #[test]
    fn fresh_open_reads_external_marker_and_comment_changes_then_reconciles_closed_aliases() {
        let _dir_guard = comment_dir("fresh_external_changes");
        let dir = _dir_guard.to_path_buf();
        let path = dir.join("changed.takm");
        make_takm(&path);
        add_disk_comment(&path, 1, "before");
        let ctx = egui::Context::default();
        let mut app = opened(&ctx, &path);
        let before_id = app.views[0].document_id();
        let before = app.views[0].data.clone().unwrap();
        let uid = before.markers[0].uid;
        assert!(before.markers[0].present(0));
        let old_tab = app.layout_tabs.active_id().unwrap();
        app.views[0].pb.set_frame(17, 0.0);
        app.close_layout_tab(old_tab);

        let mut external = EditDocument::unloaded(path.clone());
        let patch = delete_patch(&mut external, &path);
        external.apply(&patch, true).unwrap();
        save_document(&mut external, &path);
        add_disk_comment(&path, 2, "after external edit");

        app.open(&ctx, path.clone());
        assert!(app.views[0].loading.is_some(), "closed history must not satisfy a fresh open synchronously");
        wait(&ctx, &mut app, |app| !app.views[0].busy());
        let fresh_id = app.views[0].document_id();
        assert_ne!(fresh_id, before_id);
        assert!(!app.views[0].data.as_ref().unwrap().markers.iter().find(|m| m.uid == uid).unwrap().present(0));
        assert_eq!(app.views[0].comments.list.len(), 2);
        assert_eq!(app.views[0].comments.list.get(2).unwrap().text, "after external edit");
        app.reopen_layout_item();
        assert_eq!(app.layout_tabs.active_id(), Some(old_tab));
        assert_eq!(app.views[0].document_id(), fresh_id);
        assert_eq!(app.views[0].pb.frame, 17);
        assert_eq!(app.views[0].comments.list.len(), 2);
        assert!(app.views[0].loading.is_none(), "retained reopen stays instant");
        drop(app);
    }

    #[test]
    fn unchanged_fresh_open_preserves_saved_edit_identity_and_undo_patch() {
        let _dir_guard = comment_dir("fresh_unchanged_undo");
        let dir = _dir_guard.to_path_buf();
        let path = dir.join("unchanged.takm");
        make_takm(&path);
        let ctx = egui::Context::default();
        let mut app = opened(&ctx, &path);
        let (session, patch) = save_deleted_marker(&mut app.views[0], &path);
        app.close_layout_tab(app.layout_tabs.active_id().unwrap());
        app.open(&ctx, path.clone());
        assert!(app.views[0].loading.is_some());
        wait(&ctx, &mut app, |app| !app.views[0].busy());
        assert!(Arc::ptr_eq(&session, app.views[0].edit_document.as_ref().unwrap()));
        app.views[0].apply_marker_patch(&patch, false).unwrap();
        let restored = app.views[0].data.as_ref().unwrap().rebuild_markers(&patch.before_snapshot()).unwrap();
        app.views[0].publish_edited_view(restored).unwrap();
        assert!(app.views[0].data.as_ref().unwrap().markers[0].present(0));
        assert!(app.views[0].marker_dirty(), "undoing a saved edit must mark the same document dirty");
        drop(app);
    }

    #[test]
    fn in_app_comment_append_preserves_marker_document_and_saved_edit_undo() {
        let _dir_guard = comment_dir("fresh_own_comment_undo");
        let dir = _dir_guard.to_path_buf();
        let path = dir.join("own_comment.takm");
        make_takm(&path);
        let ctx = egui::Context::default();
        let mut app = opened(&ctx, &path);
        let (session, patch) = save_deleted_marker(&mut app.views[0], &path);
        app.no_persist = false;
        wait(&ctx, &mut app, |app| app.views[0].comments.id_pool.as_ref().is_some_and(|ids| !ids.is_empty()));
        app.add_comment(&ctx, 0, vec![], 3, 5);
        app.add_comment(&ctx, 0, vec![], 8, 9);
        assert_eq!(app.views[0].comments.list.len(), 2);
        wait(&ctx, &mut app, |app| app.views[0].comments.pending == 0);
        assert!(app.views[0].comments.failed.is_none());
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(app.views[0].source_stamp.as_ref().unwrap().lock().unwrap().as_ref(), Some(&(tak_core::takm::canonical_target(&path).unwrap(), meta.len(), Some(meta.modified().unwrap()))));
        app.close_layout_tab(app.layout_tabs.active_id().unwrap());
        app.open(&ctx, path.clone());
        wait(&ctx, &mut app, |app| !app.views[0].busy());
        assert!(Arc::ptr_eq(&session, app.views[0].edit_document.as_ref().unwrap()));
        assert_eq!(app.views[0].comments.list.len(), 2);
        app.views[0].apply_marker_patch(&patch, false).unwrap();
        assert!(app.views[0].marker_dirty());
        drop(app);
    }

    #[test]
    fn external_change_before_own_comment_cannot_certify_the_old_marker_document() {
        let _dir_guard = comment_dir("fresh_external_before_own_comment");
        let dir = _dir_guard.to_path_buf();
        let path = dir.join("external_then_own.takm");
        make_takm(&path);
        let ctx = egui::Context::default();
        let mut app = opened(&ctx, &path);
        let old = app.views[0].document_id();
        let stamp = app.views[0].source_stamp.as_ref().unwrap().lock().unwrap().clone();
        app.no_persist = false;
        wait(&ctx, &mut app, |app| app.views[0].comments.id_pool.as_ref().is_some_and(|ids| !ids.is_empty()));
        add_disk_comment(&path, 5000, "external before own append");
        app.add_comment(&ctx, 0, vec![], 3, 5);
        wait(&ctx, &mut app, |app| app.views[0].comments.pending == 0);
        assert!(app.views[0].comments.failed.is_none());
        assert_eq!(*app.views[0].source_stamp.as_ref().unwrap().lock().unwrap(), stamp,
            "own append cannot acknowledge an intervening unknown write");
        app.close_layout_tab(app.layout_tabs.active_id().unwrap());
        app.open(&ctx, path.clone());
        wait(&ctx, &mut app, |app| !app.views[0].busy());
        assert_ne!(app.views[0].document_id(), old);
        assert_eq!(app.views[0].comments.list.len(), 2);
        drop(app);
    }

    #[cfg(unix)]
    #[test]
    fn alias_paths_share_worker_stamps_and_keep_document_after_own_comments() {
        for relative in [false, true] {
            let _dir_guard = comment_dir(&format!("fresh_alias_{relative}"));
            let dir = _dir_guard.to_path_buf();
            let path = dir.join("source.takm");
            let alias = dir.join("alias.takm");
            make_takm(&path);
            std::os::unix::fs::symlink(&path, &alias).unwrap();
            let initial = if relative {
                let cwd = std::env::current_dir().unwrap();
                let mut relative = PathBuf::new();
                for _ in cwd.components().filter(|part| matches!(part, std::path::Component::Normal(_))) { relative.push(".."); }
                relative.push(alias.strip_prefix("/").unwrap());
                relative
            } else { alias };
            let ctx = egui::Context::default();
            let mut app = opened(&ctx, &initial);
            assert_eq!(app.views[0].path(), Some(initial.as_path()), "display keeps the chosen alias");
            let session = app.views[0].edit_document.clone().unwrap();
            let patch = delete_patch(&mut session.lock().unwrap(), &initial);
            app.no_persist = false;
            wait(&ctx, &mut app, |app| app.views[0].comments.id_pool.as_ref().is_some_and(|ids| !ids.is_empty()));
            app.add_comment(&ctx, 0, vec![], 3, 5);
            wait(&ctx, &mut app, |app| app.views[0].comments.pending == 0);
            assert!(app.views[0].comments.failed.is_none());
            let meta = std::fs::metadata(&path).unwrap();
            assert_eq!(*app.views[0].source_stamp.as_ref().unwrap().lock().unwrap(), Some((tak_core::takm::canonical_target(&path).unwrap(), meta.len(), Some(meta.modified().unwrap()))));
            app.close_layout_tab(app.layout_tabs.active_id().unwrap());
            app.open(&ctx, path.clone());
            wait(&ctx, &mut app, |app| !app.views[0].busy());
            assert!(Arc::ptr_eq(&session, app.views[0].edit_document.as_ref().unwrap()), "relative={relative}");
            app.views[0].apply_marker_patch(&patch, true).unwrap();
            let edited = app.views[0].data.as_ref().unwrap().rebuild_markers(&patch.after_snapshot()).unwrap();
            app.views[0].publish_edited_view(edited).unwrap();
            assert!(!app.views[0].data.as_ref().unwrap().markers[0].present(0));
            drop(app);
        }
    }

    #[test]
    fn portable_parent_path_alias_keeps_source_stamp_after_own_comment() {
        let _dir_guard = comment_dir("fresh_portable_alias");
        let dir = _dir_guard.to_path_buf();
        let path = dir.join("source.takm");
        let component = dir.join("spelling");
        std::fs::create_dir(&component).unwrap();
        let alias = component.join("..").join("source.takm");
        make_takm(&path);
        let ctx = egui::Context::default();
        let mut app = opened(&ctx, &alias);
        let session = app.views[0].edit_document.clone().unwrap();
        let patch = delete_patch(&mut session.lock().unwrap(), &alias);
        app.no_persist = false;
        wait(&ctx, &mut app, |app| app.views[0].comments.id_pool.as_ref().is_some_and(|ids| !ids.is_empty()));
        app.add_comment(&ctx, 0, vec![], 3, 5);
        wait(&ctx, &mut app, |app| app.views[0].comments.pending == 0);
        assert!(app.views[0].comments.failed.is_none());
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(*app.views[0].source_stamp.as_ref().unwrap().lock().unwrap(), Some((tak_core::takm::canonical_target(&path).unwrap(), meta.len(), Some(meta.modified().unwrap()))));
        app.close_layout_tab(app.layout_tabs.active_id().unwrap());
        app.open(&ctx, path.clone());
        wait(&ctx, &mut app, |app| !app.views[0].busy());
        assert!(Arc::ptr_eq(&session, app.views[0].edit_document.as_ref().unwrap()));
        app.views[0].apply_marker_patch(&patch, true).unwrap();
        let edited = app.views[0].data.as_ref().unwrap().rebuild_markers(&patch.after_snapshot()).unwrap();
        app.views[0].publish_edited_view(edited).unwrap();
        assert!(!app.views[0].data.as_ref().unwrap().markers[0].present(0));
        drop(app);
    }

    #[test]
    fn evicted_history_fallback_verifies_source_before_restoring_edit_document() {
        for mode in ["unchanged", "changed", "invalid"] {
            let _dir_guard = comment_dir(&format!("fresh_evicted_{mode}"));
            let dir = _dir_guard.to_path_buf();
            let path = dir.join("evicted.takm");
            make_takm(&path);
            let ctx = egui::Context::default();
            let mut app = opened(&ctx, &path);
            let (session, patch) = save_deleted_marker(&mut app.views[0], &path);
            let first = app.layout_tabs.active_id().unwrap();
            app.views[0].pb.set_frame(17, 0.0);
            let snapshot = app.tab_undo_snapshot(first).unwrap();
            let anchor = app.new_layout_tab();
            app.close_layout_tab(first);
            app.history.push(vec![undo::Part::TabLifecycle {
                id: first, before_open: true, after_open: false,
                before_active: Some(first), after_active: Some(anchor), snapshot,
            }]);
            for _ in 0..=layout_tabs::MAX_CLOSED {
                let filler = app.new_layout_tab();
                app.close_layout_tab(filler);
            }
            assert!(!app.layout_tabs.has_closed_tab(first));
            if mode == "changed" { add_disk_comment(&path, 77, "external after eviction"); }
            if mode == "invalid" { std::fs::write(&path, b"invalid TAKM file").unwrap(); }
            app.undo_redo(&ctx, false);
            assert_eq!(app.layout_tabs.active_id(), Some(first));
            assert!(app.views[0].loading.is_some(), "evicted runtime needs a fresh background read");
            if mode == "invalid" {
                let deadline = Instant::now() + Duration::from_secs(10);
                while app.views[0].loading.is_some() {
                    app.poll_loads(&ctx);
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(app.error.is_some());
                assert!(app.views[0].fresh_open_document().is_none());
                assert!(app.views[0].restore_edit_document.is_none());
            } else {
                wait(&ctx, &mut app, |app| !app.views[0].busy());
                assert_eq!(app.views[0].pb.frame, 17);
                if mode == "unchanged" {
                    assert!(Arc::ptr_eq(app.views[0].edit_document.as_ref().unwrap(), &session));
                    app.views[0].apply_marker_patch(&patch, false).unwrap();
                } else {
                    assert!(!Arc::ptr_eq(app.views[0].edit_document.as_ref().unwrap(), &session));
                    assert_eq!(app.views[0].comments.list.get(77).unwrap().text, "external after eviction");
                    assert!(app.views[0].apply_marker_patch(&patch, false).is_err());
                }
            }
            drop(app);
        }
    }

    #[test]
    fn fresh_open_in_hidden_tab_reads_new_comments_and_duplicate_views_share_it() {
        let _dir_guard = comment_dir("fresh_hidden");
        let dir = _dir_guard.to_path_buf();
        let path = dir.join("hidden.takm");
        make_takm(&path);
        let ctx = egui::Context::default();
        let mut app = opened(&ctx, &path);
        let old_id = app.views[0].document_id();
        app.close_layout_tab(app.layout_tabs.active_id().unwrap());
        add_disk_comment(&path, 5, "new while closed");
        app.open(&ctx, path.clone());
        let loading_tab = app.layout_tabs.active_id().unwrap();
        app.new_layout_tab();
        app.open(&ctx, path.clone());
        assert_eq!(app.views[0].waiting_shared_path(), Some(path.as_path()));
        // A follower owns no receivers. Await the hidden owner's whole startup first:
        // a later forceplate publication legitimately replaces the shared data Arc.
        wait(&ctx, &mut app, |app| app.views[0].data.is_some() && !app.views[0].busy()
            && app.views[0].comments.list.get(5).is_some()
            && app.layout_tabs.tab(loading_tab).and_then(|tab| tab.layout.as_ref())
                .is_some_and(|runtime| !runtime.views[0].file_work_pending()));
        // Consume the owner's final publication before comparing the stable shared Arc.
        app.poll_loads(&ctx);
        assert_ne!(app.views[0].document_id(), old_id);
        let data = app.views[0].data.clone().unwrap();
        let session = app.views[0].edit_document.clone().unwrap();
        app.switch_layout_tab(loading_tab);
        assert!(Arc::ptr_eq(&data, app.views[0].data.as_ref().unwrap()));
        assert!(Arc::ptr_eq(&session, app.views[0].edit_document.as_ref().unwrap()));
        assert_eq!(app.views[0].comments.list.get(5).unwrap().text, "new while closed");
        drop(app);
    }

    #[test]
    fn fresh_overlay_reads_external_change_and_restored_history_joins_it() {
        let _dir_guard = comment_dir("fresh_overlay");
        let dir = _dir_guard.to_path_buf();
        let path = dir.join("overlay.takm");
        let host = dir.join("host.takm");
        make_takm(&path);
        make_takm(&host);
        let ctx = egui::Context::default();
        let mut app = opened(&ctx, &path);
        let old = app.views[0].document_id();
        app.close_layout_tab(app.layout_tabs.active_id().unwrap());
        add_disk_comment(&path, 8, "overlay changed");
        app.open(&ctx, host.clone());
        wait(&ctx, &mut app, |app| !app.views[0].busy());
        app.overlay_on(&ctx, path.clone());
        assert_eq!(app.views[0].overlay_reading_path(), Some(path.as_path()));
        wait(&ctx, &mut app, |app| app.views[0].overlay.is_some() && !app.views[0].busy());
        let fresh = app.views[0].overlay_document_id();
        assert_ne!(fresh, old);
        assert_eq!(app.views[0].overlay_owner().unwrap().comments.list.len(), 1);
        app.reopen_layout_item();
        assert_eq!(app.views[0].document_id(), fresh);
        assert_eq!(app.views[0].comments.list.get(8).unwrap().text, "overlay changed");
        assert!(app.views[0].loading.is_none());
        drop(app);
    }

    #[test]
    fn unknown_stamp_cancelled_load_and_failed_load_never_restore_a_candidate() {
        for mode in ["unknown", "cancelled", "failed"] {
            let mut old = View::default();
            let (_tx, rx) = std::sync::mpsc::channel();
            old.show(views::sample_data("fresh_candidate.takm", &["M"]), rx, 1, 1.0, 0.0);
            let modified = (mode != "unknown").then_some(std::time::UNIX_EPOCH + Duration::from_secs(42));
            old.source_stamp = Some(Arc::new(Mutex::new(Some(("fresh_candidate.takm".into(), 42, modified)))));
            let candidate = old.retained_document();
            let mut target = View::default();
            target.hold_load("fresh_candidate.takm".into(), None);
            target.source_stamp = old.source_stamp.clone();
            target.set_fresh_open_candidate(candidate);
            if mode == "cancelled" { target.hold_load("fresh_candidate.takm".into(), None); }
            if mode == "failed" {
                target.held.take().unwrap().send(views::LoadMsg::Markers(Err("failed".into()))).unwrap();
                assert!(target.poll_markers().unwrap().result.is_err());
            }
            let (_tx, rx) = std::sync::mpsc::channel();
            target.show(views::sample_data("fresh_candidate.takm", &["M"]), rx, 2, 1.0, 0.0);
            assert_ne!(target.document_id(), old.document_id(), "{mode}");
        }
    }
}
