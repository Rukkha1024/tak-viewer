/// Count actual scheduler inspections, including dirty-index rebuilds, rather than asserting
/// only that a hidden playback frame happened to stay unchanged.
#[test]
fn hidden_work_idle_hidden_tabs_receive_zero_work_visits_across_600_frames() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let mut hidden = Vec::new();
    for index in 0..MAX_OPEN_FILES / 2 {
        app.views[0].overlay = Some(views::Overlay::from_data(
            format!("idle_overlay_{index}.tak").into(),
            views::sample_data(&format!("idle_overlay_{index}.tak"), &["M"]),
        ));
        if index + 1 == MAX_OPEN_FILES / 2 { break; }
        app.views[0].pb.set_frame(4, 0.0);
        app.views[0].pb.playing = true;
        hidden.push(app.layout_tabs.active_id().unwrap());
        app.new_layout_tab();
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(
            views::sample_data(&format!("idle_host_{}.tak", index + 1), &["M"]),
            rx,
            30 + index,
            1.0,
            0.0,
        );
        app.ensure_open_view(0);
    }
    assert_eq!(app.open_file_identities(None).len(), MAX_OPEN_FILES);
    for _ in 0..8 {
        frame_with(&ctx, &mut app, vec![]);
    }
    let before: Vec<_> = hidden
        .iter()
        .map(|&tab| {
            app.layout_tabs
                .tab(tab)
                .unwrap()
                .layout
                .as_ref()
                .unwrap()
                .views[0]
                .pb
                .frame
        })
        .collect();
    let work_counts = |app: &TakViewerApp| -> Vec<_> {
        hidden
            .iter()
            .map(|&tab| {
                let runtime = app.layout_tabs.tab(tab).unwrap().layout.as_ref().unwrap();
                runtime
                    .views
                    .iter()
                    .zip(&runtime.marker_ui)
                    .map(|(view, ui)| {
                        (
                            view.background_work_checks.get(),
                            view.overlay_owner()
                                .map(|owner| owner.background_work_checks.get()),
                            ui.pending_work_checks.get(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    app.editor_cache.borrow_mut().rebuilds.clear();
    let checks_before = work_counts(&app);
    app.hidden_work.visits.clear();
    for _ in 0..600 {
        frame_with(&ctx, &mut app, vec![]);
    }
    assert!(
        app.hidden_work.visits.is_empty(),
        "idle hidden runtimes were visited: {:?}",
        app.hidden_work.visits
    );
    assert_eq!(
        work_counts(&app),
        checks_before,
        "idle hidden work predicates were still scanned"
    );
    assert!(
        app.editor_cache.borrow().rebuilds.is_empty(),
        "idle hidden OPEN EDITORS groups were rebuilt"
    );
    for (&tab, frame) in hidden.iter().zip(before) {
        assert_eq!(
            app.layout_tabs
                .tab(tab)
                .unwrap()
                .layout
                .as_ref()
                .unwrap()
                .views[0]
                .pb
                .frame,
            frame
        );
    }
    eprintln!("hidden scheduler: {MAX_OPEN_FILES} open files, {} idle tabs, 600 frames, 0 hidden runtime visits", hidden.len());
}

#[test]
fn hidden_work_indexed_hidden_file_comment_and_overlay_work_finishes_then_becomes_idle() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    let tab = app.layout_tabs.active_id().unwrap();
    app.hold_loads = true;
    app.views[0].hold_load("hidden_arrival.takm".into(), None);
    let marker_tx = app.views[0].held.take().unwrap();
    app.views[0].hold_overlay("hidden_overlay.tak".into(), false);
    let overlay_tx = app.views[0]
        .overlay_load
        .as_mut()
        .unwrap()
        .held
        .take()
        .unwrap();
    let visible = app.new_layout_tab();
    marker_tx
        .send(views::LoadMsg::Markers(Ok((
            views::sample_data("hidden_arrival.takm", &["M"]),
            vec![],
        ))))
        .unwrap();
    overlay_tx
        .send(views::LoadMsg::Markers(Ok((
            views::sample_data("hidden_overlay.tak", &["M"]),
            vec![],
        ))))
        .unwrap();
    drop(marker_tx);
    drop(overlay_tx);
    let (comment_tx, comment_rx) = std::sync::mpsc::channel();
    let runtime = app
        .layout_tabs
        .tab_mut(tab)
        .unwrap()
        .layout
        .as_mut()
        .unwrap();
    runtime.views[0].comments_rx = Some(comment_rx);
    comment_tx
        .send(views::CommentsLoad {
            target: Ok("hidden_arrival.takm".into()),
            parsed: None,
            label: Some("hidden Comment arrived".into()),
        })
        .unwrap();
    for _ in 0..12 {
        app.poll_overlays(&ctx);
        app.poll_hidden_file_work(&ctx);
    }
    let runtime = app.layout_tabs.tab(tab).unwrap().layout.as_ref().unwrap();
    assert_eq!(
        runtime.views[0].path(),
        Some(Path::new("hidden_arrival.takm"))
    );
    assert_eq!(
        runtime.views[0].overlay_path(),
        Some(Path::new("hidden_overlay.tak"))
    );
    assert_eq!(
        runtime.views[0].comments.label.as_deref(),
        Some("hidden Comment arrived")
    );
    assert!(runtime.views[0].comments_rx.is_none());
    assert_eq!(app.layout_tabs.active_id(), Some(visible));
    app.hidden_work.visits.clear();
    for _ in 0..600 {
        app.poll_overlays(&ctx);
        app.poll_hidden_file_work(&ctx);
        app.poll_hidden_marker_work(&ctx);
    }
    assert!(
        app.hidden_work.visits.is_empty(),
        "completed work stayed in frame queue"
    );
}

fn hidden_work_write_emg(path: &Path, padding: usize, values: &[f32]) {
    // Include genuine marker/metadata streams so an ordinary asynchronous open can
    // exercise this fixture. Omit the sample's anonymous analog records: this file's
    // one non-marker device is the structurally identified EMG below.
    let sample = tak_core::sample_tak::sample_tak();
    let container = tak_core::tak::container::Container::parse(&sample).unwrap();
    let entries = container.streams().unwrap();
    let stream = |name: &str| {
        container
            .read_stream(entries.iter().find(|(n, _)| n == name).unwrap().1)
            .unwrap()
    };
    let mut nodes = stream("Nodes.dat");
    let marker_channels = stream("Channels.dat");
    let marker_records: Vec<_> = tak_core::tak::channels::iter_channel_inventory(&marker_channels)
        .unwrap()
        .into_iter()
        .filter(|record| record.name == "Translation")
        .collect();
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
    for record in marker_records {
        channels.extend(&marker_channels[record.start..record.end]);
    }
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
    for (index, (name, data)) in [
        ("Nodes.dat", nodes),
        ("Calibration.dat", padding),
        ("Channels.dat", channels),
        ("MetaData.dat", stream("MetaData.dat")),
    ]
    .into_iter()
    .enumerate()
    {
        streams.push(
            serde_json::json!({"name":name,"index":index,"offset":bytes.len(),"size":data.len()}),
        );
        bytes.extend(data);
    }
    let table_at = bytes.len() as u64;
    let table = serde_json::to_vec(&serde_json::json!({
        "streams":streams,"dropped":[],
        "source":{"name":"synthetic.tak","size":0,"sha256":"0".repeat(64)},
        "made_by":"closed reader regression","made_at":"2026-11-16T00:00:00Z"
    }))
    .unwrap();
    bytes.extend(&table);
    let fixed = bytes.len() as u64;
    bytes[..8].copy_from_slice(&tak_core::takm::MAGIC);
    bytes[8..12].copy_from_slice(&tak_core::takm::LAYOUT.to_le_bytes());
    bytes[16..24].copy_from_slice(&table_at.to_le_bytes());
    bytes[24..32].copy_from_slice(&(table.len() as u64).to_le_bytes());
    bytes[32..40].copy_from_slice(&fixed.to_le_bytes());
    let seal = <sha2::Sha256 as sha2::Digest>::digest(&bytes);
    bytes.extend(seal);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn hidden_work_explicit_shared_emg_wakes_only_hidden_owner_including_late_subscription_and_overlay()
{
    for late_subscription in [false, true] {
        for overlay_owner in [false, true] {
            let ctx = egui::Context::default();
            let _dir_guard = comment_dir(&format!(
                "hidden_work_emg_{late_subscription}_{overlay_owner}"
            ));
            let dir = _dir_guard.to_path_buf();
            let path = dir.join("emg.takm");
            hidden_work_write_emg(&path, 0, &[1.0, 2.0, 3.0]);
            let fast = tak_core::tak::fast_read::read_take_fast(&path, false).unwrap();
            let (_, lazy) = fast.analog.unwrap().split();
            let mut data = tak_core::view_data::ViewData::build(fast.take);
            data.emg_pending = true;
            let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
            app.set_command_run();
            app.ensure_open_view(0);
            let (_tx, rx) = std::sync::mpsc::channel();
            app.views[0].show(data, rx, 1, 1.0, 0.0);
            app.views[0].install_deferred_emg_reader(lazy.unwrap());
            let shared = app.views[0].shared_file().unwrap();
            if overlay_owner {
                let mut host = View::default();
                let (_tx, rx) = std::sync::mpsc::channel();
                host.show(
                    views::sample_data("emg_carrier.tak", &["M"]),
                    rx,
                    2,
                    1.0,
                    0.0,
                );
                host.start_shared_overlay(path.clone(), shared.clone(), false);
                host.overlay = Some(host.poll_overlay().unwrap().1.unwrap());
                assert!(app.views[0].transfer_primary_work_to_overlay(&mut host));
                app.views[0] = host;
            }
            let owner_tab = app.layout_tabs.active_id().unwrap();
            let mut follower = View::default();
            follower.show_shared_file(shared.clone(), 8, 1.0, 0.0);
            if late_subscription {
                // Demand exists before the hidden scheduler has subscribed this owner.
                follower.devices.request_emg = true;
                follower.request_emg(&ctx);
            }
            let idle_tab = app.new_layout_tab();
            let (_tx, rx) = std::sync::mpsc::channel();
            app.views[0].show(
                views::sample_data("unrelated_idle.tak", &["M"]),
                rx,
                4,
                1.0,
                0.0,
            );
            app.ensure_open_view(0);
            let visible_tab = app.new_layout_tab();
            app.views[0] = follower;
            app.ensure_open_view(0);
            app.refresh_hidden_work();
            app.hidden_work.visits.clear();
            if !late_subscription {
                app.views[0].devices.request_emg = true;
                app.views[0].request_emg(&ctx);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                app.poll_overlays(&ctx);
                app.poll_hidden_file_work(&ctx);
                app.views[0].sync_shared();
                if !app.views[0].data.as_ref().unwrap().emg_pending {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "hidden owner lost the explicit demand"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(app.layout_tabs.active_id(), Some(visible_tab));
            assert_eq!(
                app.hidden_work.visits.get(&idle_tab).copied().unwrap_or(0),
                0,
                "an unrelated idle tab was visited for another file's demand"
            );
            let runtime = app
                .layout_tabs
                .tab(owner_tab)
                .unwrap()
                .layout
                .as_ref()
                .unwrap();
            let owner = if overlay_owner {
                runtime.views[0].overlay_owner().unwrap()
            } else {
                &runtime.views[0]
            };
            assert_eq!(
                owner
                    .load_steps
                    .iter()
                    .filter(|(stage, _)| *stage == "emg")
                    .count(),
                1
            );
            assert_eq!(
                app.views[0].data.as_ref().unwrap().take.analog[0].channels[0].values,
                vec![1.0, 2.0, 3.0]
            );
            // A worker's final publication may still need one index update; thereafter idle.
            app.poll_overlays(&ctx);
            app.poll_hidden_file_work(&ctx);
            app.hidden_work.visits.clear();
            for _ in 0..100 {
                app.views[0].devices.request_emg = true;
                app.views[0].request_emg(&ctx);
                app.poll_overlays(&ctx);
                app.poll_hidden_file_work(&ctx);
            }
            assert!(
                app.hidden_work.visits.is_empty(),
                "completed EMG kept hidden work alive"
            );
        }
    }
}

#[test]
fn hidden_work_overlay_replacement_arrival_wakes_follower_without_another_layout_event() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.hold_loads = true;
    let source = app.layout_tabs.active_id().unwrap();
    app.views[0].overlay = Some(views::Overlay::from_data(
        "previous_overlay.tak".into(),
        views::sample_data("previous_overlay.tak", &["M"]),
    ));
    app.views[0].hold_overlay("replacement_overlay.tak".into(), false);
    let sender = app.views[0]
        .overlay_load
        .as_mut()
        .unwrap()
        .held
        .take()
        .unwrap();
    let follower = app.new_layout_tab();
    let (_tx, rx) = std::sync::mpsc::channel();
    app.views[0].show(
        views::sample_data("follower_host.tak", &["M"]),
        rx,
        21,
        1.0,
        0.0,
    );
    app.ensure_open_view(0);
    app.views[0].wait_overlay_shared("replacement_overlay.tak".into(), false);
    let visible = app.new_layout_tab();
    // Establish the exact pending topology: C is requested while old B remains visible.
    for _ in 0..3 {
        app.poll_overlays(&ctx);
        app.poll_hidden_file_work(&ctx);
    }
    assert!(app
        .layout_tabs
        .tab(follower)
        .unwrap()
        .layout
        .as_ref()
        .unwrap()
        .views[0]
        .overlay_waiting_shared_path()
        .is_some());
    sender
        .send(views::LoadMsg::Markers(Ok((
            views::sample_data("replacement_overlay.tak", &["M"]),
            vec![],
        ))))
        .unwrap();
    drop(sender);
    for _ in 0..8 {
        app.poll_overlays(&ctx);
        app.poll_hidden_file_work(&ctx);
    }
    let source_view = &app
        .layout_tabs
        .tab(source)
        .unwrap()
        .layout
        .as_ref()
        .unwrap()
        .views[0];
    let follower_view = &app
        .layout_tabs
        .tab(follower)
        .unwrap()
        .layout
        .as_ref()
        .unwrap()
        .views[0];
    assert_eq!(
        source_view.overlay_path(),
        Some(Path::new("replacement_overlay.tak"))
    );
    assert_eq!(
        follower_view.overlay_path(),
        Some(Path::new("replacement_overlay.tak"))
    );
    assert_eq!(
        source_view.overlay_document_id(),
        follower_view.overlay_document_id()
    );
    assert_eq!(app.layout_tabs.active_id(), Some(visible));
    app.hidden_work.visits.clear();
    for _ in 0..600 {
        app.poll_overlays(&ctx);
        app.poll_hidden_file_work(&ctx);
    }
    assert!(app.hidden_work.visits.is_empty());
}

#[test]
fn hidden_work_editor_cache_updates_shared_dirty_save_comment_rename_and_active_labels() {
    use std::sync::Arc;
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    app.set_command_run();
    hidden_work_prepare_document(&mut app);
    let first = app.layout_tabs.active_id().unwrap();
    let shared = app.views[0].shared_file().unwrap();
    let second = app.new_layout_tab();
    app.views[0].show_shared_file(shared, 22, 1.0, 0.0);
    app.ensure_open_view(0);
    let groups = app.editor_groups(&[0]);
    assert_eq!(
        groups
            .iter()
            .filter(|group| group.active)
            .map(|group| group.tab_id)
            .collect::<Vec<_>>(),
        [second]
    );
    assert!(groups.iter().all(|group| !group.entries[0].dirty));
    app.editor_cache.borrow_mut().rebuilds.clear();
    app.views[0].select(0, false);
    let before = app.views[0]
        .edit_document
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .snapshot();
    let patch = tak_core::edit::delete::delete_samples(
        &before,
        &tak_core::edit::delete::DeleteRequest {
            selection: app.views[0].edit_selection_state(),
            range: None,
            current_frame: 0,
        },
    )
    .unwrap()
    .patch
    .unwrap();
    app.views[0].apply_marker_patch(&patch, true).unwrap();
    let groups = app.editor_groups(&[0]);
    assert!(
        groups.iter().all(|group| group.entries[0].dirty),
        "shared edit must update the hidden dirty star immediately"
    );
    assert!(app.view_label(0, &[0]).unwrap().ends_with('*'));
    assert_eq!(app.editor_cache.borrow().rebuilds.get(&first), Some(&1));
    app.layout_tabs.rename(first, "보행 비교").unwrap();
    let groups = app.editor_groups(&[0]);
    assert_eq!(
        groups
            .iter()
            .find(|group| group.tab_id == first)
            .unwrap()
            .tab_name,
        "보행 비교"
    );
    assert!(app.switch_layout_tab_internal(first));
    let groups = app.editor_groups(&[0]);
    assert_eq!(
        groups
            .iter()
            .filter(|group| group.active)
            .map(|group| group.tab_id)
            .collect::<Vec<_>>(),
        [first]
    );
    assert!(groups
        .iter()
        .find(|group| group.tab_id == second)
        .is_some_and(|group| !group.active));
    // Publish a saved revision and path without a hidden runtime sync; cached rows must use
    // the canonical shared path and clear the star in both aliases.
    let saved = app.views[0]
        .edit_document
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .snapshot();
    app.views[0]
        .edit_document
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .mark_saved(&saved, "saved_shared.takm".into())
        .unwrap();
    let mut saved_data = app.views[0].data.as_ref().unwrap().as_ref().clone();
    saved_data.take.path = "saved_shared.takm".into();
    app.views[0].data = Some(Arc::new(saved_data));
    assert!(app.views[0].publish_shared_file());
    let groups = app.editor_groups(&[0]);
    assert!(groups
        .iter()
        .all(|group| !group.entries[0].dirty
            && group.entries[0].path == Path::new("saved_shared.takm")));
    assert!(!app.view_label(0, &[0]).unwrap().ends_with('*'));
    let rebuilds = app
        .editor_cache
        .borrow()
        .rebuilds
        .get(&second)
        .copied()
        .unwrap_or(0);
    app.views[0].comments.pending = 1;
    assert!(app.views[0].publish_shared_file());
    app.editor_groups(&[0]);
    assert_eq!(
        app.editor_cache.borrow().rebuilds.get(&second).copied(),
        Some(rebuilds + 1),
        "Comment publication invalidates its hidden rows"
    );
    assert!(app.layout_tabs.move_tab(second, 0));
    let groups = app.editor_groups(&[0]);
    assert_eq!(groups[0].tab_id, second);
    assert!(!groups[0].active);
}

fn hidden_work_prepare_document(app: &mut TakViewerApp) {
    use std::sync::Arc;
    use tak_core::edit::{DocumentInput, RawSample, RawTrack, RecordBytes};
    let tracks = app.views[0]
        .data
        .as_ref()
        .unwrap()
        .take
        .trajectories
        .iter()
        .map(|track| RawTrack {
            uid: track.uid,
            name: track.name.clone(),
            label: None,
            records: RecordBytes {
                node: Arc::from([1]),
                translation_header: Arc::from([2]),
                serialization_verified: true,
            },
            samples: track
                .frames
                .iter()
                .zip(&track.xyz)
                .map(|(&frame, &xyz)| RawSample::new(frame, xyz))
                .collect::<Vec<_>>()
                .into(),
        })
        .collect();
    {
        let mut document = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
        let revision = document.revision();
        document
            .install_input(
                revision,
                DocumentInput {
                    streams: Vec::new().into(),
                    nodes_prefix: Arc::from([]),
                    channels_prefix: Arc::from([]),
                    tracks,
                    labels: Vec::new(),
                    first_frame: 0,
                    last_frame: 9,
                },
            )
            .unwrap();
    }
}

#[test]
fn hidden_work_editor_cache_discard_and_ctrl_z_clear_hidden_dirty_before_refresh() {
    let ctx = egui::Context::default();
    let mut app = one_view(&ctx);
    hidden_work_prepare_document(&mut app);
    let hidden = app.layout_tabs.active_id().unwrap();
    let shared = app.views[0].shared_file().unwrap();
    app.new_layout_tab();
    app.views[0].show_shared_file(shared, 23, 1.0, 0.0);
    app.ensure_open_view(0);
    app.editor_groups(&[0]); // Subscribe/cache the hidden occurrence before the edit.
    app.views[0].select(0, false);
    app.observe(true);
    let before = app.views[0]
        .edit_document
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .snapshot();
    let patch = tak_core::edit::delete::delete_samples(
        &before,
        &tak_core::edit::delete::DeleteRequest {
            selection: app.views[0].edit_selection_state(),
            range: None,
            current_frame: 0,
        },
    )
    .unwrap()
    .patch
    .unwrap();
    app.commit_marker_edit(0, std::sync::Arc::new(patch))
        .unwrap();
    assert!(app
        .editor_groups(&[0])
        .iter()
        .all(|group| group.entries[0].dirty));
    app.undo_redo(&ctx, false);
    assert!(
        app.editor_groups(&[0])
            .iter()
            .all(|group| !group.entries[0].dirty),
        "Ctrl+Z clears hidden stars immediately"
    );
    app.undo_redo(&ctx, true);
    assert!(app
        .editor_groups(&[0])
        .iter()
        .all(|group| group.entries[0].dirty));
    app.discard_marker_edits(0).unwrap();
    assert!(
        app.views[0].edit_refresh.is_some(),
        "marker refresh has deliberately not completed yet"
    );
    assert!(
        app.editor_groups(&[0])
            .iter()
            .all(|group| !group.entries[0].dirty),
        "Discard clears hidden stars before a refresh can publish"
    );
    assert!(!app.view_label(0, &[0]).unwrap().ends_with('*'));
    let rebuilds = app.editor_cache.borrow().rebuilds.get(&hidden).copied();
    for _ in 0..100 {
        app.editor_groups(&[0]);
    }
    assert_eq!(
        app.editor_cache.borrow().rebuilds.get(&hidden).copied(),
        rebuilds
    );
}
