#[test]
fn layout_tabs_keep_independent_layouts_and_share_the_same_loaded_file() {
    let ctx = egui::Context::default();
    let mut app = two_views(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    app.views[0].pb.set_frame(3, 0.0);
    app.views[0].cam.yaw = 0.7;
    let second = app.new_layout_tab();
    assert!(app.open_views().is_empty());
    app.open(&ctx, PathBuf::from("a.tak"));
    assert_eq!(app.open_views(), vec![0]);
    let same_data = app.views[0].data.clone().unwrap();
    let same_doc = app.views[0].edit_document.clone().unwrap();
    app.views[0].pb.set_frame(7, 0.0);
    app.switch_layout_tab(first);
    assert_eq!(app.open_views(), vec![0, 1]);
    assert_eq!(app.views[0].pb.frame, 3);
    assert_eq!(app.views[0].cam.yaw, 0.7);
    assert!(std::sync::Arc::ptr_eq(&same_data, app.views[0].data.as_ref().unwrap()));
    assert!(std::sync::Arc::ptr_eq(&same_doc, app.views[0].edit_document.as_ref().unwrap()));
    app.switch_layout_tab(second);
    assert_eq!(app.views[0].pb.frame, 7);
}

#[test]
fn layout_tabs_explorer_open_in_new_tab_is_one_undoable_action() {
    let ctx = egui::Context::default();
    let dir = comment_dir("layout_open_new_tab");
    let files = take_files(&dir, &["new_tab.tak"]);
    let mut app = one_view(&ctx);
    app.hold_loads = true;
    let first = app.layout_tabs.active_id().unwrap();
    let count = app.history.undo.len();
    app.explorer_action(&ctx, WsAction::OpenNewTab(files[0].clone()));
    let second = app.layout_tabs.active_id().unwrap();
    assert_ne!(first, second);
    assert_eq!(app.views[0].target(), Some(files[0].as_path()));
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(app.history.undo.len(), count + 1, "creating and opening the tab are one action");

    app.undo_redo(&ctx, false);
    assert_eq!(app.layout_tabs.active_id(), Some(first));
    assert!(app.layout_tabs.tab(second).is_none());
    app.undo_redo(&ctx, true);
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    assert_eq!(app.views[0].target(), Some(files[0].as_path()));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn layout_tabs_reopening_a_hidden_view_is_one_undoable_action() {
    let ctx = egui::Context::default();
    let mut app = two_views(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    app.views[1].pb.set_frame(5, 0.0);
    let data = app.views[1].data.clone().unwrap();
    let document = app.views[1].edit_document.clone().unwrap();
    app.close_view(1);
    frame_with(&ctx, &mut app, vec![]);
    assert_eq!(app.open_views(), vec![0]);
    let second = app.new_layout_tab();
    let count = app.history.undo.len();

    run_frame(&ctx, &mut app, &[(Modifiers { shift: true, ..ctrl() }, Key::T)]);
    assert_eq!(app.layout_tabs.active_id(), Some(first));
    assert_eq!(app.open_views(), vec![0, 1]);
    assert_eq!(app.history.undo.len(), count + 1, "switch and reopen are one action");
    assert!(std::sync::Arc::ptr_eq(&data, app.views[1].data.as_ref().unwrap()));
    assert!(std::sync::Arc::ptr_eq(&document, app.views[1].edit_document.as_ref().unwrap()));
    assert_eq!(app.views[1].pb.frame, 5);

    app.undo_redo(&ctx, false);
    assert_eq!(app.layout_tabs.active_id(), Some(second), "one undo returns to the previous tab");
    let hidden = app.layout_tabs.tab(first).unwrap().layout.as_ref().unwrap();
    assert!(hidden.views[1].target().is_none(), "the reopened view was closed again");
    app.undo_redo(&ctx, true);
    assert_eq!(app.layout_tabs.active_id(), Some(first));
    assert!(std::sync::Arc::ptr_eq(&data, app.views[1].data.as_ref().unwrap()));
    assert_eq!(app.views[1].pb.frame, 5);
}

#[test]
fn layout_tabs_shortcuts_and_rename_text_focus_take_precedence() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    run_frame(&ctx, &mut app, &[(ctrl(), Key::T)]);
    let second = app.layout_tabs.active_id().unwrap();
    assert_ne!(first, second);
    assert!(app.open_views().is_empty());
    run_frame(&ctx, &mut app, &[(ctrl(), Key::PageUp)]);
    assert_eq!(app.layout_tabs.active_id(), Some(first));
    run_frame(&ctx, &mut app, &[(ctrl(), Key::PageDown)]);
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::F2)]);
    assert!(app.layout_strip.is_renaming());
    let count = app.layout_tabs.tabs().len();
    run_frame(&ctx, &mut app, &[(ctrl(), Key::T)]);
    assert_eq!(app.layout_tabs.tabs().len(), count);
    run_frame(&ctx, &mut app, &[(Modifiers::NONE, Key::Escape)]);
    assert!(!app.layout_strip.is_renaming());
}

#[test]
fn layout_tabs_limit_is_twelve_distinct_files_not_twelve_views() {
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.hold_loads = true;
    for tab in 0..3 {
        if tab > 0 { app.new_layout_tab(); }
        for i in 0..4 {
            let path = PathBuf::from(format!("tab_limit_{}.tak", tab * 4 + i));
            if i == 0 { app.open(&ctx, path); } else { app.open_side(&ctx, path); }
        }
        assert_eq!(app.open_views().len(), 4);
    }
    app.new_layout_tab();
    app.open(&ctx, PathBuf::from("tab_limit_12.tak"));
    assert!(app.open_views().is_empty());
    assert!(app.notice.as_deref().is_some_and(|s| s.contains("12")));
    app.notice = None;
    app.open(&ctx, PathBuf::from("tab_limit_0.tak"));
    assert_eq!(app.open_views().len(), 1, "a second view of the same file is not a 13th file");
    assert_eq!(app.views[0].waiting_shared_path(), Some(Path::new("tab_limit_0.tak")));
}

#[test]
fn layout_tabs_limit_counts_displayed_files_during_replacement() {
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.hold_loads = true;
    for tab in 0..3 {
        if tab > 0 { app.new_layout_tab(); }
        for slot in 0..4 {
            let path = PathBuf::from(format!("strict_limit_{}.tak", tab * 4 + slot));
            if slot == 0 { app.open(&ctx, path); } else { app.open_side(&ctx, path); }
        }
    }
    arrive(&ctx, &mut app, 3);
    let displayed = PathBuf::from("strict_limit_11.tak");
    let thirteenth = PathBuf::from("strict_limit_12.tak");
    assert_eq!(app.views[3].path(), Some(displayed.as_path()));
    assert!(!app.allow_open_file(&thirteenth, Some(3)), "a displayed file counts while replacing it");
    assert!(!app.allow_replace_overlay(&thirteenth, 3), "an overlay replacement cannot start a thirteenth file");

    // Replacing by a source already being opened elsewhere is allowed, but its old
    // displayed file still counts until the shared waiter actually attaches.
    app.open_in(&ctx, 3, PathBuf::from("strict_limit_0.tak"));
    assert_eq!(app.views[3].path(), Some(displayed.as_path()));
    assert_eq!(app.views[3].waiting_shared_path(), Some(Path::new("strict_limit_0.tak")));
    assert!(!app.allow_open_file(&thirteenth, None), "a waiting replacement must not hide its displayed source from the cap");
}

#[test]
fn layout_tabs_cap_and_share_checks_reuse_bound_identities() {
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.hold_loads = true;
    for tab in 0..3 {
        if tab > 0 { app.new_layout_tab(); }
        for slot in 0..4 {
            let path = PathBuf::from(format!("identity_bound_{}.tak", tab * 4 + slot));
            if slot == 0 { app.open(&ctx, path); } else { app.open_side(&ctx, path); }
        }
    }
    // Three layouts include hidden pending readers; the last view becomes displayed.
    arrive(&ctx, &mut app, 3);
    let overlay = PathBuf::from("identity_bound_0.tak");
    app.start_overlay(&ctx, 3, overlay.clone(), false);
    assert_eq!(app.views[3].overlay_target(), Some(overlay.as_path()));
    layout_controller::reset_identity_fallbacks();
    assert_eq!(app.open_file_identities(None).len(), 12);
    assert!(app.file_is_loading_elsewhere(&overlay, 3));
    assert!(app.find_shared_file(&overlay, 3).is_none());
    assert_eq!(layout_controller::identity_fallbacks(), 0,
        "loaded, pending, hidden and overlay sources already have canonical identities");

    assert!(!app.allow_open_file(Path::new("identity_bound_12.tak"), None));
    assert_eq!(layout_controller::identity_fallbacks(), 1,
        "only the newly requested candidate may resolve through the filesystem");
}

#[cfg(unix)]
#[test]
fn layout_tabs_new_open_rechecks_retargeted_symlink() {
    let ctx = egui::Context::default();
    let dir = comment_dir("layout_retargeted_symlink");
    let first = dir.join("first.tak");
    let second = dir.join("second.tak");
    let alias = dir.join("alias.tak");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();
    std::os::unix::fs::symlink(&first, &alias).unwrap();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.hold_loads = true;
    app.open(&ctx, alias.clone());
    arrive(&ctx, &mut app, 0);
    assert_eq!(app.open_file_identities(None).len(), 1);

    app.new_layout_tab();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&second, &alias).unwrap();
    app.open(&ctx, alias.clone());
    assert!(app.views[0].loading.is_some(), "a changed symlink needs its own reader");
    assert!(app.views[0].waiting_shared_path().is_none());
    assert_eq!(app.open_file_identities(None).len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn layout_tabs_dirty_prompt_blocks_mouse_switch_and_creation() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    let second = app.new_layout_tab();
    app.switch_layout_tab(first);
    app.views[0].comments.failed = Some("test write failure".into());
    app.close_layout_tab(first);
    assert!(app.dirty_prompt.is_some());
    app.layout_strip_action(&ctx, layout_tabs::TabAction::Activate(second));
    app.layout_strip_action(&ctx, layout_tabs::TabAction::New);
    assert_eq!(app.layout_tabs.active_id(), Some(first));
    assert_eq!(app.layout_tabs.tabs().len(), 2);
    let out = frame_with(&ctx, &mut app, vec![]);
    let tab2 = text_rects(&out, "tab2");
    if let Some(rect) = tab2.first() { click_at(&ctx, &mut app, rect.center()); }
    assert_eq!(app.layout_tabs.active_id(), Some(first));
    assert!(app.dirty_prompt.is_some());
}

#[test]
fn layout_tabs_reopen_after_fresh_open_reuses_the_live_document() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let old = app.layout_tabs.active_id().unwrap();
    app.views[0].pb.set_frame(4, 0.0);
    app.views[0].cam.yaw = 0.9;
    app.close_layout_tab(old);
    assert!(app.layout_tabs.tabs().is_empty());
    app.hold_loads = true;
    app.open(&ctx, PathBuf::from("a.tak"));
    arrive(&ctx, &mut app, 0);
    let live_data = app.views[0].data.clone().unwrap();
    let live_doc = app.views[0].edit_document.clone().unwrap();
    app.reopen_layout_item();
    assert_eq!(app.layout_tabs.tabs().len(), 2);
    assert!(std::sync::Arc::ptr_eq(&live_data, app.views[0].data.as_ref().unwrap()));
    assert!(std::sync::Arc::ptr_eq(&live_doc, app.views[0].edit_document.as_ref().unwrap()));
    assert_eq!(app.views[0].pb.frame, 4);
    assert_eq!(app.views[0].cam.yaw, 0.9);
}

#[test]
fn layout_tabs_restored_view_and_tab_reclaim_a_closed_owners_refreshed_emg_reader() {
    use sha2::{Digest, Sha256};

    // A tiny sealed TAKM with one structurally identified EMG device. Moving its channel
    // stream models a marker save while keeping this regression independent of input/.
    fn write_emg(path: &Path, padding: usize, values: &[f32]) {
        // Include genuine marker/metadata streams so an ordinary asynchronous open can
        // exercise this fixture. Omit the sample's anonymous analog records: this file's
        // one non-marker device is the structurally identified EMG below.
        let sample = tak_core::sample_tak::sample_tak();
        let container = tak_core::tak::container::Container::parse(&sample).unwrap();
        let entries = container.streams().unwrap();
        let stream = |name: &str| container.read_stream(entries.iter().find(|(n, _)| n == name).unwrap().1).unwrap();
        let mut nodes = stream("Nodes.dat");
        let marker_channels = stream("Channels.dat");
        let marker_records: Vec<_> = tak_core::tak::channels::iter_channel_inventory(&marker_channels).unwrap().into_iter()
            .filter(|record| record.name == "Translation").collect();
        let text = "{\n    \"properties\": [{\"name\":\"NodeName\",\"value\":\"Synthetic EMG\"},{\"name\":\"DeviceNode_DeviceType\",\"value\":\"1\"}]\n}";
        let kind = "class MoCapCore::cDeviceNode<float,16>";
        nodes.extend((kind.len() as u16).to_le_bytes());
        nodes.extend(kind.as_bytes());
        nodes.extend((text.encode_utf16().count() as u16).to_le_bytes());
        nodes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        nodes.extend([0; 19]);
        nodes.extend([9; 16]);
        nodes.extend([0; 33]);
        let mut channels = tak_core::tak::channels::CHANNELS_HEADER.to_vec();
        channels.extend(((marker_records.len() + 1) as u32).to_le_bytes());
        for record in marker_records { channels.extend(&marker_channels[record.start..record.end]); }
        channels.extend(12u16.to_le_bytes());
        channels.extend(b"FloatChannel");
        channels.push(4);
        let name = "AnalogChannel0";
        channels.extend((name.len() as u16).to_le_bytes());
        channels.extend(name.encode_utf16().flat_map(u16::to_le_bytes));
        channels.extend([9; 16]);
        channels.extend([0; 4]);
        channels.extend((values.len() as u32).to_le_bytes());
        for (frame, value) in values.iter().enumerate() {
            channels.extend((frame as u32).to_le_bytes());
            channels.extend(value.to_le_bytes());
        }
        let padding = vec![0x55; padding];
        let mut bytes = vec![0; tak_core::takm::HEADER_LEN as usize];
        let mut streams = Vec::new();
        for (index, (name, data)) in [("Nodes.dat", nodes), ("Calibration.dat", padding), ("Channels.dat", channels), ("MetaData.dat", stream("MetaData.dat"))].into_iter().enumerate() {
            streams.push(serde_json::json!({"name":name,"index":index,"offset":bytes.len(),"size":data.len()}));
            bytes.extend(data);
        }
        let table_at = bytes.len() as u64;
        let table = serde_json::to_vec(&serde_json::json!({
            "streams":streams,"dropped":[],
            "source":{"name":"synthetic.tak","size":0,"sha256":"0".repeat(64)},
            "made_by":"closed reader regression","made_at":"2026-11-16T00:00:00Z"
        })).unwrap();
        bytes.extend(&table);
        let fixed = bytes.len() as u64;
        bytes[..8].copy_from_slice(&tak_core::takm::MAGIC);
        bytes[8..12].copy_from_slice(&tak_core::takm::LAYOUT.to_le_bytes());
        bytes[16..24].copy_from_slice(&table_at.to_le_bytes());
        bytes[24..32].copy_from_slice(&(table.len() as u64).to_le_bytes());
        bytes[32..40].copy_from_slice(&fixed.to_le_bytes());
        let seal = Sha256::digest(&bytes);
        bytes.extend(seal);
        std::fs::write(path, bytes).unwrap();
    }

    fn put_in_overlay(view: &mut View, path: &Path, file_id: usize) {
        let shared = view.shared_file().unwrap();
        let mut host = View::default();
        let (_tx, rx) = std::sync::mpsc::channel();
        host.show(views::sample_data("reader_carrier.tak", &["M"]), rx, file_id, 1.0, 0.0);
        host.start_shared_overlay(path.to_path_buf(), shared, false);
        host.overlay = Some(host.poll_overlay().unwrap().1.unwrap());
        assert!(view.transfer_primary_work_to_overlay(&mut host));
        drop_later(view.clear());
        *view = host;
    }

    for tab in [false, true] {
        for target_overlay in [false, true] {
            for source_overlay in [false, true] {
                let ctx = egui::Context::default();
                let dir = comment_dir(&format!("closed_emg_{tab}_{target_overlay}_{source_overlay}"));
                let path = dir.join("deferred.takm");
                write_emg(&path, 0, &[1.0, 2.0, 3.0]);
                let old_offset = tak_core::takm::read_index_path(&path).unwrap().unique("Channels.dat").unwrap().offset;
                let fast = tak_core::tak::fast_read::read_take_fast(&path, false).unwrap();
                let (_, lazy) = fast.analog.unwrap().split();
                let mut data = tak_core::view_data::ViewData::build(fast.take);
                data.emg_pending = true;
                let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
                let (_tx, rx) = std::sync::mpsc::channel();
                app.views[0].show(data, rx, 10, 1.0, 0.0);
                let meta = std::fs::metadata(&path).unwrap();
                app.views[0].source_stamp = Some(std::sync::Arc::new(std::sync::Mutex::new(Some((tak_core::takm::canonical_target(&path).unwrap(), meta.len(), Some(meta.modified().unwrap()))))));
                app.views[0].install_deferred_emg_reader(lazy.unwrap());
                let document = app.views[0].document_id().unwrap();
                if target_overlay { put_in_overlay(&mut app.views[0], &path, 11); }
                let target_path = app.views[0].path().unwrap().to_path_buf();
                let target_file_id = app.views[0].file_id;
                let target_tab = app.layout_tabs.active_id().unwrap();
                if tab {
                    app.close_layout_tab(target_tab);
                } else {
                    app.ensure_open_view(1);
                    dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
                    let (_tx, rx) = std::sync::mpsc::channel();
                    app.views[1].show(views::sample_data("other.tak", &["M"]), rx, 12, 1.0, 0.0);
                    app.close_view(0);
                }

                // A later open adopts the closed document and becomes the only reader owner.
                let source_tab = app.new_layout_tab();
                app.open_in(&ctx, 0, path.clone());
                close_wait_until(&ctx, &mut app, |app| app.views[0].loading.is_none() && app.views[0].data.is_some() && !app.views[0].busy());
                assert_eq!(app.views[0].document_id(), Some(document));
                assert!(app.views[0].owns_shared_file_work());
                if source_overlay { put_in_overlay(&mut app.views[0], &path, 13); }
                write_emg(&path, 4096, &[41.25, -7.5, 99.0]);
                assert_ne!(tak_core::takm::read_index_path(&path).unwrap().unique("Channels.dat").unwrap().offset, old_offset);
                let fresh = tak_core::tak::fast_read::deferred_job_after_save(&path).unwrap().unwrap();
                let source = if source_overlay { app.views[0].overlay_owner_mut().unwrap() } else { &mut app.views[0] };
                source.install_deferred_emg_reader(fresh);
                assert!(source.data.as_ref().unwrap().take.analog[0].channels[0].values.is_empty());
                app.close_layout_tab(source_tab);

                // Restore the older item, leaving the newer sole reader owner closed.
                if tab {
                    assert!(app.restore_closed_tab_internal(target_tab));
                } else {
                    assert_eq!(app.layout_tabs.active_id(), Some(target_tab));
                    assert!(app.restore_closed_view_for_history(target_tab, 0, &target_path, target_file_id));
                    dock::add_view(&mut app.dock, 0, dock::NOMINAL_AREA);
                    app.activate(0);
                }
                let restored = if target_overlay { app.views[0].overlay_owner_mut().unwrap() } else { &mut app.views[0] };
                assert_eq!(restored.document_id(), Some(document));
                assert!(restored.owns_shared_file_work(), "tab={tab}, target_overlay={target_overlay}, source_overlay={source_overlay}");
                restored.devices.request_emg = true;
                restored.request_emg(&ctx);
                close_wait_until(&ctx, &mut app, |app| {
                    let view = if target_overlay { app.views[0].overlay_owner().unwrap() } else { &app.views[0] };
                    !view.data.as_ref().unwrap().emg_pending || view.emg_failure.is_some()
                });
                let restored = if target_overlay { app.views[0].overlay_owner().unwrap() } else { &app.views[0] };
                assert!(restored.emg_failure.is_none(), "{:?}", restored.emg_failure);
                let channel = &restored.data.as_ref().unwrap().take.analog[0].channels[0];
                assert_eq!(channel.frames, [0, 1, 2]);
                assert_eq!(channel.values, [41.25, -7.5, 99.0]);
                assert_eq!(restored.load_steps.iter().filter(|(stage, _)| *stage == "emg").count(), 1);
                let closed_source = app.layout_tabs.closed_items().find_map(|item| match item {
                    layout_tabs::ClosedItem::Tab(closed) if closed.tab.id == source_tab => Some(&closed.tab.layout.as_ref().unwrap().views[0]),
                    _ => None,
                }).unwrap();
                let source = if source_overlay { closed_source.overlay_owner().unwrap() } else { closed_source };
                assert!(!source.owns_shared_file_work(), "the closed owner must surrender its only reader");
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }
}

#[test]
fn layout_tabs_open_from_zero_tabs_undoes_in_one_step() {
    let ctx = egui::Context::default();
    let dir = comment_dir("layout_open_after_empty");
    let files = take_files(&dir, &["new_after_empty.tak"]);
    let mut app = one_view(&ctx);
    let first = app.layout_tabs.active_id().unwrap();
    app.close_layout_tab(first);
    assert!(app.layout_tabs.tabs().is_empty());
    app.hold_loads = true;
    app.open(&ctx, files[0].clone());
    assert_eq!(app.layout_tabs.tabs().len(), 1);
    assert_eq!(app.views[0].target(), Some(files[0].as_path()));
    app.undo_redo(&ctx, false);
    assert!(app.layout_tabs.tabs().is_empty());
    assert!(app.open_views().is_empty());
    app.undo_redo(&ctx, true);
    assert_eq!(app.layout_tabs.tabs().len(), 1);
    assert_eq!(app.views[0].target(), Some(files[0].as_path()));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn layout_tabs_failed_shared_open_releases_every_waiter() {
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.hold_loads = true;
    app.open(&ctx, PathBuf::from("broken_shared.tak"));
    let owner = app.layout_tabs.active_id().unwrap();
    let follower = app.new_layout_tab();
    app.open(&ctx, PathBuf::from("broken_shared.tak"));
    assert!(app.views[0].waiting_shared_path().is_some());
    app.switch_layout_tab(owner);
    let tx = app.views[0].held.take().unwrap();
    tx.send(views::LoadMsg::Markers(Err("invalid file".into()))).unwrap();
    frame_with(&ctx, &mut app, vec![]);
    app.switch_layout_tab(follower);
    assert!(app.views[0].waiting_shared_path().is_none());
    assert!(app.error.as_deref().is_some_and(|e| e.contains("invalid file")));
}

#[test]
fn layout_tabs_saved_names_layouts_and_independent_spots_return() {
    let ctx = egui::Context::default();
    let dir = comment_dir("layout_tabs_persist");
    let paths = take_files(&dir, &["same.takm", "other.takm"]);
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    let (_tx, rx) = std::sync::mpsc::channel();
    app.views[0].show(views::sample_data(paths[0].to_str().unwrap(), &["M"]), rx, 10, 1.0, 0.0);
    app.views[0].pb.set_frame(3, 0.0);
    let first = app.layout_tabs.active_id().unwrap();
    app.layout_tabs.rename(first, "Walk").unwrap();
    let second = app.new_layout_tab();
    app.open(&ctx, paths[0].clone());
    app.views[0].pb.set_frame(7, 0.0);
    app.open_side(&ctx, paths[1].clone());
    app.layout_tabs.rename(second, "Comparison").unwrap();
    let mut storage = FileLike::default();
    app.save(&mut storage);
    let mut restored = TakViewerApp::build(&ctx, Some(&storage), vec![], None, None);
    assert_eq!(restored.layout_tabs.tabs().iter().map(|tab| tab.name.as_str()).collect::<Vec<_>>(), vec!["Walk", "Comparison"]);
    assert_eq!(restored.layout_tabs.active().unwrap().name, "Comparison");
    assert_eq!(restored.open_views(), vec![0, 1]);
    assert_eq!(restored.views[0].target(), Some(paths[0].as_path()));
    assert_eq!(restored.views[0].restore.as_ref().map(|spot| spot.frame), Some(7));
    let first_id = restored.layout_tabs.tabs()[0].id;
    restored.switch_layout_tab_internal(first_id);
    assert_eq!(restored.open_views(), vec![0]);
    assert_eq!(restored.views[0].target(), Some(paths[0].as_path()));
    assert_eq!(restored.views[0].restore.as_ref().map(|spot| spot.frame), Some(3));
    let _ = std::fs::remove_dir_all(dir);
}

/// Optional real-file, headless captures used to compare the three approved mockups.
#[test]
fn layout_tabs_make_review_shots_when_requested() {
    let (Some(dir), Some(file)) = (std::env::var_os("TAK_TABS_SHOTS_DIR"), std::env::var_os("TAK_TABS_SHOTS_FILE")) else { return };
    let dir = PathBuf::from(dir);
    let file = PathBuf::from(file);
    assert!(file.is_file(), "missing screenshot input {}", file.display());
    let source_before = std::fs::metadata(&file).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = egui::Context::default();
    let mut textures = crate::soft_render::Textures::default();
    fn review_frame(ctx: &egui::Context, app: &mut TakViewerApp, textures: &mut crate::soft_render::Textures) -> egui::FullOutput {
        let out = frame_with(ctx, app, vec![]);
        textures.set(&out.textures_delta);
        out
    }
    let mut app = TakViewerApp::build(&ctx, None, vec![file.clone(), file.clone()], None, None);
    app.set_command_run();
    if let Some(parent) = file.parent() { app.open_workspace(parent.to_path_buf()); }
    let started = std::time::Instant::now();
    while app.views.iter().filter(|view| view.data.as_ref().is_some_and(|data| !data.analog_pending)).count() < 2 {
        assert!(started.elapsed() < Duration::from_secs(30), "two views did not finish: {:?}", app.error);
        review_frame(&ctx, &mut app, &mut textures);
        std::thread::sleep(Duration::from_millis(5));
    }
    app.set_graph_float(false);
    for _ in 0..3 { review_frame(&ctx, &mut app, &mut textures); }
    // The approved 1600×960 mockups put GRAPH at y=612. Set only its parent
    // split; this is fixture state, not a change to the app's default layout.
    let (_, graph, _) = app.dock.find_tab(&Pane::Graph).unwrap();
    let parent = egui_dock::NodeIndex((graph.0 - 1) / 2);
    if let egui_dock::Node::Vertical { rect, fraction } = &mut app.dock.main_surface_mut()[parent] {
        *fraction = ((612.0 - rect.top()) / rect.height()).clamp(0.05, 0.95);
    } else { panic!("GRAPH must be below the editor in the review fixture"); }
    for _ in 0..3 { review_frame(&ctx, &mut app, &mut textures); }

    fn prepare_view(view: &mut View, preferred: &str) {
        let data = view.data.as_ref().expect("screenshot view is loaded");
        let selected = data.markers.iter().position(|marker| marker.name == preferred)
            .or_else(|| (!data.markers.is_empty()).then_some(0));
        view.pb.set_frame(600, 0.0);
        // Frame the real points at the shown frame. The mockup's points/curves are
        // illustrative, so the fixture never replaces them with made-up samples.
        if let Some((lo, hi)) = capture::fit_bounds(data, view.pb.frame, capture::Fit::Frame, &[]) {
            let find = |name| data.markers.iter().find(|marker| marker.name == name)
                .and_then(|marker| marker.position(view.pb.frame));
            if let (Some(lasi), Some(rasi), Some(lpsi), Some(rpsi)) =
                (find("LASI"), find("RASI"), find("LPSI"), find("RPSI")) {
                let facing = (lasi + rasi) - (lpsi + rpsi);
                view.cam.yaw = facing.x.atan2(facing.z);
            }
            view.cam.pitch = 6f32.to_radians();
            view.cam.fit_box(lo, hi, view.scene_rect.width() / view.scene_rect.height().max(1.0));
            view.cam.distance *= 1.15;
        }
        if let Some(marker) = selected {
            println!("review view: {} · frame {} · {}", data.take.path.display(), view.pb.frame, data.markers[marker].name);
            view.select(marker, false);
        }
    }
    let first = app.layout_tabs.active_id().unwrap();
    for slot in app.open_views() {
        prepare_view(&mut app.views[slot], if slot == 0 { "LASI" } else { "RASI" });
    }
    app.activate(0);
    let second = app.new_layout_tab();
    app.open(&ctx, file.clone());
    for _ in 0..3 { review_frame(&ctx, &mut app, &mut textures); }
    prepare_view(&mut app.views[0], "RASI");
    app.activate(0);
    assert!(app.layout_strip.begin_rename(&app.layout_tabs, second));
    fn capture(ctx: &egui::Context, app: &mut TakViewerApp, textures: &mut crate::soft_render::Textures, path: &Path) {
        // Keep app-cached text in its original context and preserve all font patches.
        for _ in 0..4 { review_frame(ctx, app, textures); }
        let out = review_frame(ctx, app, textures);
        let ppp = out.pixels_per_point;
        let size = [(1600.0 * ppp).round() as usize, (960.0 * ppp).round() as usize];
        let primitives = ctx.tessellate(out.shapes, ppp);
        // Upload the complete live atlas as well: cached dock titles may refer to
        // glyphs created before the final frame's partial texture delta.
        textures.set(&egui::epaint::textures::TexturesDelta {
            set: vec![(egui::TextureId::Managed(0), egui::epaint::ImageDelta::full(
                ctx.fonts(|fonts| fonts.image()), egui::TextureOptions::LINEAR,
            ))],
            ..Default::default()
        });
        let picture = crate::soft_render::paint(&primitives, &textures, size, ppp, egui::Color32::from_rgb(25, 25, 25));
        let rgba: Vec<u8> = picture.pixels.iter().flat_map(|pixel| pixel.to_array()).collect();
        crate::shot::save_png(path, size[0], size[1], &rgba).unwrap();
    }
    capture(&ctx, &mut app, &mut textures, &dir.join("tab2_one_view.png"));
    assert!(app.layout_strip.is_renaming(), "the second mockup shows the inline rename field");
    app.layout_strip.cancel_rename();
    app.switch_layout_tab(first);
    assert_eq!(app.active, 0, "GRAPH must follow View 1 in the first mockup");
    capture(&ctx, &mut app, &mut textures, &dir.join("tab1_two_views.png"));
    app.close_layout_tab(first);
    app.close_layout_tab(second);
    capture(&ctx, &mut app, &mut textures, &dir.join("empty.png"));
    let source_after = std::fs::metadata(&file).unwrap();
    assert_eq!(source_before.len(), source_after.len(), "screenshot input size changed");
    assert_eq!(source_before.modified().unwrap(), source_after.modified().unwrap(), "screenshot input was modified");
}

/// Optional real-file 12-source/3-tab timing. Inputs are read only; no screenshots or TAKs
/// are written. Timings cover the UI and tessellation on the CPU, not GPU presentation.
#[test]
fn layout_tabs_measure_twelve_files_when_requested() {
    let Some(folder) = std::env::var_os("TAK_TABS_BENCH_DIR") else { return };
    let folder = PathBuf::from(folder);
    let ctx = egui::Context::default();
    let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
    app.set_command_run();
    let start_memory = crate::bench_load::ProcessMemory::sample();
    let mut tabs = Vec::new();
    let mut timing = Vec::new();
    let mut memory = Vec::new();
    for n in 1..=12 {
        if (n - 1) % 4 == 0 {
            if n > 1 { app.new_layout_tab(); }
            tabs.push(app.layout_tabs.active_id().unwrap());
        }
        let file = folder.join(format!("gait_{n}.tak"));
        assert!(file.is_file(), "missing benchmark input {}", file.display());
        let began = Instant::now();
        if (n - 1) % 4 == 0 { app.open(&ctx, file.clone()); } else { app.open_side(&ctx, file.clone()); }
        let slot = app.active;
        loop {
            frame_with(&ctx, &mut app, vec![]);
            if app.views[slot].path() == Some(file.as_path()) { break; }
            assert!(began.elapsed() < Duration::from_secs(60), "file {n} did not open: {:?}", app.error);
            std::thread::sleep(Duration::from_millis(1));
        }
        let visible_ms = began.elapsed().as_secs_f64() * 1000.0;
        timing.push((n, app.views[slot].markers_ms.unwrap_or(visible_ms), visible_ms));
        if [1, 4, 8, 12].contains(&n) {
            // Sample after forceplate reads have settled in all tabs. EMG remains lazy.
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let busy = |view: &View| view.analog_work_pending()
                    || view.data.as_ref().is_some_and(|data| data.analog_pending);
                let pending = app.views.iter().any(busy)
                    || app.layout_tabs.tabs().iter().filter_map(|tab| tab.layout.as_ref())
                        .any(|runtime| runtime.views.iter().any(busy));
                if !pending { break; }
                assert!(Instant::now() < deadline, "forceplate reads did not settle after file {n}");
                frame_with(&ctx, &mut app, vec![]);
                std::thread::sleep(Duration::from_millis(1));
            }
            memory.push((n, crate::bench_load::ProcessMemory::sample()));
        }
    }
    for &id in &tabs {
        app.switch_layout_tab_internal(id);
        for slot in app.open_views() {
            let view = &mut app.views[slot];
            if view.data.as_ref().is_some_and(|data| !data.markers.is_empty()) { view.select(0, false); }
            view.pb.play(app.layout_time);
        }
    }
    let mut times = Vec::with_capacity(600);
    let mut switch_times = Vec::with_capacity(200);
    for frame in 0..600 {
        let began = Instant::now();
        if frame % 3 == 0 {
            let switch_started = Instant::now();
            app.switch_layout_tab_internal(tabs[(frame / 3) % tabs.len()]);
            switch_times.push(switch_started.elapsed().as_secs_f64() * 1000.0);
        }
        let out = frame_with(&ctx, &mut app, vec![]);
        let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
        times.push(began.elapsed().as_secs_f64() * 1000.0);
    }
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    let max = times.iter().copied().fold(0.0_f64, f64::max);
    println!("initial memory: {}", crate::bench_load::ProcessMemory::describe(start_memory));
    for (n, markers_ms, visible_ms) in timing { println!("file {n}: markers {markers_ms:.2} ms, first UI frame {visible_ms:.2} ms"); }
    for (n, mem) in memory { println!("after {n} files, forceplates settled: {}", crate::bench_load::ProcessMemory::describe(mem)); }
    println!("3 tabs, 12 files, 600 frames, switch every 3 frames: average {avg:.2} ms, max {max:.2} ms");
    let switch_avg = switch_times.iter().sum::<f64>() / switch_times.len() as f64;
    let switch_max = switch_times.iter().copied().fold(0.0_f64, f64::max);
    let over_budget = times.iter().filter(|&&ms| ms > 16.0).count();
    println!("200 tab switches: average {switch_avg:.3} ms, max {switch_max:.3} ms; frames over 16 ms: {over_budget}/600");
}
