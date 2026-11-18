use super::*;
use tak_core::view_data::ViewData;
use tak_core::edit::{DocumentInput, RawSample, RawTrack, RecordBytes};
use tak_core::edit::delete::{delete_samples, DeleteRequest};

fn shown(app: &mut TakViewerApp, slot: usize, path: &Path) {
    if !app.open_views().contains(&slot) { dock::add_view(&mut app.dock, slot as u8, dock::NOMINAL_AREA); }
    let (_, rx) = channel();
    app.next_file_id += 1;
    app.views[slot].show(crate::views::sample_data(&path.to_string_lossy(), &["A", "B"]), rx, app.next_file_id, 1.0, 0.0);
    app.views[slot].fresh = false;
    app.views[slot].fit_pending = false;
}

fn app(ctx: &egui::Context, source: &Path) -> TakViewerApp {
    let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
    shown(&mut app, 0, source);
    app.observe(false);
    app
}

fn finish(ctx: &egui::Context, app: &mut TakViewerApp) {
    for _ in 0..2000 {
        app.poll_takm(ctx);
        if app.takm_job.is_none() { return; }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("TAKM stage did not complete");
}

fn dirtify(view: &mut View) {
    let tracks = view.data.as_ref().unwrap().take.trajectories.iter().map(|track| RawTrack {
        uid: track.uid, name: track.name.clone(), label: None,
        records: RecordBytes { node: Arc::from([1]), translation_header: Arc::from([2]), serialization_verified: true },
        samples: track.frames.iter().zip(&track.xyz).map(|(&frame, &xyz)| RawSample::new(frame, xyz)).collect::<Vec<_>>().into(),
    }).collect();
    let mut document = view.edit_document.as_ref().unwrap().lock().unwrap();
    if !document.snapshot().is_loaded() {
        let revision = document.revision();
        document.install_input(revision, DocumentInput {
            streams: Vec::new().into(), nodes_prefix: Arc::from([]), channels_prefix: Arc::from([]),
            tracks, labels: vec![], first_frame: 0, last_frame: 9,
        }).unwrap();
    }
    let snapshot = document.snapshot();
    let selection = tak_core::edit::EditSelection {
        selected: [[1; 16]].into_iter().collect(), order: vec![[1; 16]], primary: Some([1; 16]), armed_label: None,
    };
    let patch = delete_samples(&snapshot, &DeleteRequest { selection, range: None, current_frame: 0 }).unwrap().patch.unwrap();
    document.apply(&patch, true).unwrap();
}

fn fixture(name: &str) -> tak_core::test_support::TempDir {
    tak_core::test_support::TempDir::new(&format!("takm_batch_{name}"))
}

#[test]
fn all_primary_occurrences_switch_once_preserving_hidden_state_and_real_overlay_loads() {
    let ctx = egui::Context::default();
    let source = Path::new("batch_source.tak");
    let output = Path::new("batch_source.takm");
    let mut app = app(&ctx, source);
    let first = app.layout_tabs.active_id();
    let shared = app.views[0].shared_file().unwrap();
    dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
    app.views[1].show_shared_file(shared, 12, 1.0, 0.0);
    for slot in 0..2 {
        app.views[slot].pb.set_frame(2 + slot as i64, 0.0);
        app.views[slot].cam.yaw = 0.4 + slot as f32;
        app.views[slot].range = Some((slot as i64, 6));
        app.views[slot].select(slot, false);
        app.views[slot].fresh = false;
        app.views[slot].fit_pending = false;
    }
    let overlay_path = PathBuf::from("overlay_dirty.takm");
    app.views[0].overlay = Some(crate::views::Overlay::from_data(overlay_path.clone(), crate::views::sample_data(&overlay_path.to_string_lossy(), &["A", "B"])));
    dirtify(app.views[0].overlay_owner_mut().unwrap());
    let overlay_document = app.views[0].overlay_document_id();
    let overlay_data = app.views[0].overlay.as_ref().unwrap().data.clone();
    app.views[0].hold_overlay("incoming_overlay.tak".into(), false);
    let overlay_sender = app.views[0].overlay_load.as_ref().unwrap().held.as_ref().unwrap().clone();
    app.views[0].shown = [false, true];
    app.views[0].pb.playing = true;
    let second = app.new_layout_tab();
    app.open(&ctx, source.into());
    app.views[0].pb.set_frame(8, 0.0);
    app.views[0].cam.yaw = 2.3;
    app.views[0].range = Some((5, 8));
    app.views[0].select(1, false);
    app.views[0].fresh = false;
    app.views[0].fit_pending = false;
    shown(&mut app, 1, output);
    let destination = app.views[1].edit_document.clone().unwrap();
    app.observe(false);
    let before = app.history.undo.len();
    assert!(app.switch_to_takm(&ctx, source, output));
    finish(&ctx, &mut app);
    assert!(app.takm_job.is_none());
    assert_eq!(app.history.undo.len(), before + 1);
    assert!(matches!(app.history.undo.last().unwrap().as_slice(), [undo::Part::TakmSwitch { .. }]));
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    assert!(Arc::ptr_eq(app.views[0].edit_document.as_ref().unwrap(), &destination));
    let hidden = app.layout_tabs.tab(first.unwrap()).unwrap().layout.as_ref().unwrap();
    for slot in 0..2 {
        assert_eq!(hidden.views[slot].path(), Some(output));
        assert!(Arc::ptr_eq(hidden.views[slot].edit_document.as_ref().unwrap(), &destination));
        assert_eq!(hidden.views[slot].pb.frame, 2 + slot as i64);
        assert_eq!(hidden.views[slot].cam.yaw, 0.4 + slot as f32);
        assert_eq!(hidden.views[slot].primary, Some(slot));
        assert_eq!(hidden.views[slot].range, Some((slot as i64, 6)));
    }
    assert!(hidden.views[0].pb.playing);
    assert_eq!(hidden.views[0].overlay_document_id(), overlay_document);
    assert!(Arc::ptr_eq(&hidden.views[0].overlay.as_ref().unwrap().data, &overlay_data));
    assert!(hidden.views[0].overlay_owner().unwrap().marker_dirty());
    assert_eq!(hidden.views[0].shown, [false, true]);
    assert!(hidden.views[0].overlay_load.is_some());
    app.undo_redo(&ctx, false);
    assert!(app.dirty_prompt.is_none(), "unmodified overlay is never removed");
    assert_eq!(app.views[0].path(), Some(source));
    let hidden = app.layout_tabs.tab(first.unwrap()).unwrap().layout.as_ref().unwrap();
    assert_eq!(hidden.views[0].path(), Some(source));
    assert_eq!(hidden.views[1].path(), Some(source));
    assert_eq!(hidden.views[0].overlay_document_id(), overlay_document);
    assert!(Arc::ptr_eq(&hidden.views[0].overlay.as_ref().unwrap().data, &overlay_data));
    app.undo_redo(&ctx, true);
    assert_eq!(app.views[0].path(), Some(output));
    let hidden = app.layout_tabs.tab(first.unwrap()).unwrap().layout.as_ref().unwrap();
    assert_eq!(hidden.views[1].path(), Some(output));
    assert_eq!(hidden.views[0].shown, [false, true]);
    overlay_sender.send(crate::views::LoadMsg::Markers(Err("still attached".into()))).unwrap();
}

#[test]
fn destination_already_overlaid_refuses_the_whole_batch_and_preserves_the_dirty_overlay() {
    let ctx = egui::Context::default();
    let source = Path::new("overlaid_source.tak");
    let output = Path::new("overlaid_source.takm");
    let mut app = app(&ctx, source);
    app.views[0].overlay = Some(crate::views::Overlay::from_data(output.into(), crate::views::sample_data(&output.to_string_lossy(), &["A", "B"])));
    dirtify(app.views[0].overlay_owner_mut().unwrap());
    let document = app.views[0].overlay_owner().unwrap().edit_document.clone().unwrap();
    let data = app.views[0].overlay.as_ref().unwrap().data.clone();
    app.views[0].shown = [false, true];
    app.observe(false);
    let count = app.history.undo.len();
    app.switch_to_takm(&ctx, source, output);
    finish(&ctx, &mut app);
    assert_eq!(app.views[0].path(), Some(source));
    assert!(app.takm_job.is_none());
    assert_eq!(app.history.undo.len(), count);
    assert!(app.notice.as_ref().unwrap().contains("이미 이 화면에 얹어"));
    assert!(Arc::ptr_eq(&app.views[0].overlay.as_ref().unwrap().data, &data));
    assert_eq!(app.views[0].overlay_document_id(), document.lock().ok().map(|doc| doc.id()));
    assert!(app.views[0].overlay_owner().unwrap().marker_dirty());
    assert_eq!(app.views[0].shown, [false, true]);
}

#[test]
fn dirty_existing_hidden_destination_is_joined_and_undo_keeps_its_surviving_reference() {
    let ctx = egui::Context::default();
    let source = Path::new("hidden_dest.tak");
    let output = Path::new("hidden_dest.takm");
    let mut app = app(&ctx, output);
    dirtify(&mut app.views[0]);
    let hidden = app.layout_tabs.active_id().unwrap();
    let destination = app.views[0].edit_document.clone().unwrap();
    app.new_layout_tab();
    shown(&mut app, 0, source);
    app.observe(false);
    app.switch_to_takm(&ctx, source, output);
    finish(&ctx, &mut app);
    assert!(Arc::ptr_eq(app.views[0].edit_document.as_ref().unwrap(), &destination));
    assert!(app.views[0].marker_dirty());
    app.undo_redo(&ctx, false);
    assert!(app.dirty_prompt.is_none());
    assert_eq!(app.views[0].path(), Some(source));
    assert!(app.layout_tabs.tab(hidden).unwrap().layout.as_ref().unwrap().views[0].marker_dirty());
}

#[test]
fn undo_last_dirty_destination_prompts_once_cancel_keeps_batch_and_discard_allows_one_undo() {
    let ctx = egui::Context::default();
    let source = Path::new("dirty_last.tak");
    let output = Path::new("dirty_last.takm");
    let mut app = app(&ctx, source);
    shown(&mut app, 1, output);
    app.observe(false);
    app.switch_to_takm(&ctx, source, output);
    finish(&ctx, &mut app);
    // Remove the pre-existing destination occurrence without recording a separate user action.
    drop_later(Some(std::mem::take(&mut app.views[1])));
    dock::remove_view(&mut app.dock, 1);
    dirtify(&mut app.views[0]);
    app.observe(false);
    let count = app.history.undo.len();
    app.undo_redo(&ctx, false);
    assert!(app.dirty_prompt.is_some());
    assert_eq!(app.history.undo.len(), count);
    assert_eq!(app.views[0].path(), Some(output));
    app.cancel_dirty_transition(&ctx);
    assert!(app.dirty_prompt.is_none());
    assert_eq!(app.views[0].path(), Some(output));
    app.undo_redo(&ctx, false);
    assert!(app.dirty_prompt.is_some());
    app.discard_pending_transition(&ctx);
    assert!(app.dirty_prompt.is_none());
    assert_eq!(app.views[0].path(), Some(source));
    app.undo_redo(&ctx, true);
    assert_eq!(app.views[0].path(), Some(output));
    assert!(!app.views[0].marker_dirty(), "discarded changes never return");
}

#[test]
fn actual_file_dirty_source_is_refused_before_creating_any_raw_sibling() {
    let ctx = egui::Context::default();
    let _dir_guard = fixture("dirty_source");
    let dir = _dir_guard.to_path_buf();
    let source = dir.join("source.tak");
    let bytes = tak_core::sample_tak::sample_tak();
    std::fs::write(&source, &bytes).unwrap();
    let mut app = app(&ctx, &source);
    let (prepared, _) = tak_core::edit::PreparedInput::read_from_path(&source).unwrap();
    {
        let mut document = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
        let revision = document.revision();
        document.install_prepared(revision, prepared).unwrap();
    }
    dirtify(&mut app.views[0]);
    let document = app.views[0].document_id();
    app.save_takm(&ctx, source.clone());
    assert!(app.takm_job.is_none());
    assert!(!dir.join("source.takm").exists());
    assert!(app.dirty_prompt.is_none());
    assert_eq!(app.views[0].document_id(), document);
    assert!(app.views[0].marker_dirty());
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    assert!(app.takm_note.as_ref().unwrap().0.contains("Ctrl+S"));
}

#[test]
fn actual_file_conversion_stages_then_switches_all_occurrences_and_undo_redo_once() {
    let ctx = egui::Context::default();
    let _dir_guard = fixture("convert");
    let dir = _dir_guard.to_path_buf();
    let source = dir.join("source.tak");
    let bytes = tak_core::sample_tak::sample_tak();
    std::fs::write(&source, &bytes).unwrap();
    let mut app = app(&ctx, &source);
    let first = app.layout_tabs.active_id().unwrap();
    app.new_layout_tab();
    app.open(&ctx, source.clone());
    app.views[0].fresh = false;
    app.views[0].fit_pending = false;
    app.observe(false);
    let count = app.history.undo.len();
    app.save_takm(&ctx, source.clone());
    assert_eq!(app.views[0].path(), Some(source.as_path()), "old views survive the pending conversion");
    finish(&ctx, &mut app);
    let output = dir.join("source.takm");
    assert_eq!(app.views[0].path(), Some(output.as_path()), "{:?}", app.takm_note);
    let hidden = app.layout_tabs.tab(first).unwrap().layout.as_ref().unwrap();
    assert_eq!(hidden.views[0].path(), Some(output.as_path()));
    assert!(Arc::ptr_eq(app.views[0].data.as_ref().unwrap(), hidden.views[0].data.as_ref().unwrap()));
    assert_eq!(app.history.undo.len(), count + 1);
    app.undo_redo(&ctx, false);
    assert_eq!(app.views[0].path(), Some(source.as_path()));
    assert_eq!(app.layout_tabs.tab(first).unwrap().layout.as_ref().unwrap().views[0].path(), Some(source.as_path()));
    app.undo_redo(&ctx, true);
    assert_eq!(app.views[0].path(), Some(output.as_path()));
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    assert!(output.exists(), "undo never deletes created files");
}

#[test]
fn failed_destination_read_keeps_every_primary_overlay_and_history_unchanged() {
    let ctx = egui::Context::default();
    let _dir_guard = fixture("failed");
    let dir = _dir_guard.to_path_buf();
    let source = dir.join("source.tak");
    let output = dir.join("source.takm");
    std::fs::write(&output, b"invalid TAKM").unwrap();
    let mut app = app(&ctx, &source);
    let document = app.views[0].document_id();
    app.views[0].hold_overlay("pending_overlay.tak".into(), false);
    let sender = app.views[0].overlay_load.as_ref().unwrap().held.as_ref().unwrap().clone();
    app.observe(false);
    let count = app.history.undo.len();
    app.switch_to_takm(&ctx, &source, &output);
    finish(&ctx, &mut app);
    assert_eq!(app.views[0].path(), Some(source.as_path()));
    finish(&ctx, &mut app);
    assert_eq!(app.views[0].document_id(), document);
    assert_eq!(app.history.undo.len(), count);
    assert!(app.views[0].overlay_load.is_some());
    sender.send(crate::views::LoadMsg::Markers(Err("preserved".into()))).unwrap();
    assert!(app.takm_note.as_ref().unwrap().0.contains("화면은 그대로"));
}

#[test]
fn cap_uses_final_distinct_set_and_refuses_without_partial_switch() {
    let ctx = egui::Context::default();
    let source = Path::new("cap_source.tak");
    let output = Path::new("cap_source.takm");
    let mut app = app(&ctx, source);
    let first = app.layout_tabs.active_id().unwrap();
    // Keep the original as an unchanged overlay, so conversion would add a distinct source.
    let shared = app.views[0].shared_file().unwrap();
    app.views[0].start_shared_overlay(source.into(), shared, false);
    let (_, overlay, _) = app.views[0].poll_overlay().unwrap();
    app.views[0].overlay = Some(overlay.unwrap());
    for n in 0..MAX_OPEN_FILES - 1 {
        if n % 4 == 0 { app.new_layout_tab(); }
        shown(&mut app, n % 4, &PathBuf::from(format!("cap_other_{n}.tak")));
    }
    app.switch_layout_tab_internal(first);
    app.observe(false);
    let count = app.history.undo.len();
    app.switch_to_takm(&ctx, source, output);
    finish(&ctx, &mut app);
    assert_eq!(app.views[0].path(), Some(source));
    assert!(app.takm_job.is_none());
    assert_eq!(app.history.undo.len(), count);
    assert!(app.notice.as_ref().unwrap().contains("30개까지"));
    // An already-open destination changes no distinct-file count.
    let last = app.layout_tabs.tabs().last().unwrap().id;
    app.switch_layout_tab_internal(last);
    shown(&mut app, 0, output);
    let destination = app.views[0].document_id();
    app.switch_layout_tab_internal(first);
    app.observe(false);
    app.notice = None;
    app.switch_to_takm(&ctx, source, output);
    finish(&ctx, &mut app);
    assert_eq!(app.views[0].document_id(), destination);
    assert_eq!(app.views[0].path(), Some(output));
    assert_eq!(app.views[0].overlay_path(), Some(source));
    assert!(app.notice.is_none());
}

#[test]
fn selected_plate_waits_for_pending_destination_then_clamps_a_known_missing_plate() {
    let ctx = egui::Context::default();
    let source = Path::new("plate_source.tak");
    let output = Path::new("plate_source.takm");
    let mut app = app(&ctx, source);
    let (_, rx) = channel();
    app.views[0].show(crate::views::sample_with_plates(&source.to_string_lossy(), &["A", "B"]), rx, 100, 1.0, 0.0);
    app.views[0].plate = Some(1);
    app.views[0].fresh = false;
    app.views[0].fit_pending = false;
    let (tx, rx) = channel();
    let mut pending = crate::views::sample_data(&output.to_string_lossy(), &["A", "B"]);
    pending.analog_pending = true;
    dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
    app.views[1].show(pending, rx, 101, 1.0, 0.0);
    app.views[1].fresh = false;
    app.views[1].fit_pending = false;
    app.observe(false);
    app.switch_to_takm(&ctx, source, output);
    finish(&ctx, &mut app);
    assert_eq!(app.views[0].plate, Some(1), "the destination has not resolved its plates yet");
    drop(tx);
    for _ in 0..1000 {
        app.views[1].poll_analog();
        app.views[0].sync_shared();
        if !app.views[0].data.as_ref().unwrap().analog_pending { break; }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!app.views[0].data.as_ref().unwrap().analog_pending);
    assert_eq!(app.views[0].plate, None, "a known missing destination plate is no longer selected");
    app.undo_redo(&ctx, false);
    assert_eq!(app.views[0].plate, Some(1), "the original plate selection remains in undo");
}

#[test]
fn deferred_emg_owner_survives_conversion_undo_redo_in_an_unchanged_original_overlay() {
    let recording = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../input/static/000000_S01_static.tak");
    if !recording.exists() { return; }
    let before = std::fs::metadata(&recording).unwrap();
    let mut fast = tak_core::tak::fast_read::read_take_fast(&recording, false).unwrap();
    let (_, lazy) = fast.analog.take().unwrap().split();
    let source = Path::new("emg_conversion_source.tak");
    let output = Path::new("emg_conversion_source.takm");
    fast.take.path = source.into();
    let mut data = ViewData::build(fast.take);
    data.emg_pending = true;
    let ctx = egui::Context::default();
    let mut app = app(&ctx, source);
    let (_, rx) = channel();
    app.views[0].show(data, rx, 150, 1.0, 0.0);
    app.views[0].install_deferred_emg_reader(lazy.expect("static recording has deferred EMG"));
    app.views[0].fresh = false;
    app.views[0].fit_pending = false;
    let source_document = app.views[0].document_id();
    let shared = app.views[0].shared_file().unwrap();
    shown(&mut app, 1, Path::new("emg_overlay_host.tak"));
    app.views[1].start_shared_overlay(source.into(), shared, false);
    app.views[1].overlay = Some(app.views[1].poll_overlay().unwrap().1.unwrap());
    shown(&mut app, 2, output);
    app.observe(false);
    app.switch_to_takm(&ctx, source, output);
    finish(&ctx, &mut app);
    assert_eq!(app.views[0].path(), Some(output));
    assert!(app.views[1].overlay_owner().unwrap().owns_shared_file_work(), "history must not own the only live reader");
    app.undo_redo(&ctx, false);
    assert_eq!(app.views[0].document_id(), source_document);
    assert!(app.views[1].overlay_owner().unwrap().owns_shared_file_work());
    app.undo_redo(&ctx, true);
    assert_eq!(app.views[0].path(), Some(output));
    let owner = app.views[1].overlay_owner_mut().unwrap();
    owner.devices.request_emg = true;
    owner.request_emg(&ctx);
    assert!(owner.emg_requested);
    for _ in 0..5000 {
        owner.poll_analog();
        if !owner.data.as_ref().unwrap().emg_pending { break; }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!owner.data.as_ref().unwrap().emg_pending);
    assert!(owner.data.as_ref().unwrap().devices.iter().enumerate().any(|(index, device)| device.plate.is_none()
        && owner.data.as_ref().unwrap().take.analog[index].channels.iter().any(|channel| !channel.values.is_empty())));
    let after = std::fs::metadata(&recording).unwrap();
    assert_eq!((before.len(), before.modified().unwrap()), (after.len(), after.modified().unwrap()));
}

#[test]
fn closing_the_source_while_conversion_waits_never_reopens_an_existing_destination() {
    let ctx = egui::Context::default();
    let _dir_guard = fixture("cancel_closed");
    let dir = _dir_guard.to_path_buf();
    let source = dir.join("source.tak");
    std::fs::write(&source, tak_core::sample_tak::sample_tak()).unwrap();
    let converted = takm::convert(&source, None).unwrap();
    let before = std::fs::read(&converted.out).unwrap();
    let mut app = app(&ctx, &source);
    let (go, gate) = channel();
    app.takm_gate = Some(gate);
    app.save_takm(&ctx, source);
    assert!(app.takm_job.is_some());
    app.close_view(0);
    assert!(app.open_views().is_empty());
    let count = app.history.undo.len();
    go.send(()).unwrap();
    finish(&ctx, &mut app);
    assert!(app.open_views().is_empty());
    assert_eq!(app.history.undo.len(), count);
    assert_eq!(std::fs::read(&converted.out).unwrap(), before);
}

#[test]
fn destination_already_loading_in_a_hidden_tab_is_joined_after_its_one_reader_arrives() {
    let ctx = egui::Context::default();
    let source = Path::new("async_source.tak");
    let output = Path::new("async_source.takm");
    let mut app = app(&ctx, source);
    let first = app.layout_tabs.active_id().unwrap();
    let second = app.new_layout_tab();
    app.hold_loads = true;
    app.open(&ctx, output.into());
    let sender = app.views[0].held.take().unwrap();
    app.switch_layout_tab_internal(first);
    app.observe(false);
    app.switch_to_takm(&ctx, source, output);
    assert!(app.takm_job.is_some());
    assert_eq!(app.views[0].path(), Some(source));
    assert!(matches!(app.takm_job.as_ref().unwrap().phase, Phase::Read { view: None, .. }), "no duplicate destination reader");
    sender.send(crate::views::LoadMsg::Markers(Ok((crate::views::sample_data(&output.to_string_lossy(), &["A", "B"]), vec![])))).unwrap();
    app.poll_hidden_file_work(&ctx);
    finish(&ctx, &mut app);
    assert!(app.takm_job.is_none());
    let destination = app.layout_tabs.tab(second).unwrap().layout.as_ref().unwrap().views[0].edit_document.clone().unwrap();
    assert!(Arc::ptr_eq(app.views[0].edit_document.as_ref().unwrap(), &destination));
    assert_eq!(app.views[0].path(), Some(output));
}

#[test]
fn strict_cap_refuses_new_takm_while_thirty_sources_are_still_displayed() {
    let ctx = egui::Context::default();
    let _dir_guard = fixture("strict_cap");
    let dir = _dir_guard.to_path_buf();
    let source = dir.join("source.tak");
    let bytes = tak_core::sample_tak::sample_tak();
    std::fs::write(&source, &bytes).unwrap();
    let mut app = app(&ctx, &source);
    let first = app.layout_tabs.active_id().unwrap();
    for n in 0..MAX_OPEN_FILES - 1 {
        if n % MAX_VIEWS == 0 { app.new_layout_tab(); }
        shown(&mut app, n % MAX_VIEWS, &PathBuf::from(format!("strict_cap_other_{n}.tak")));
    }
    app.switch_layout_tab_internal(first);
    app.observe(false);
    let count = app.history.undo.len();
    app.save_takm(&ctx, source.clone());
    finish(&ctx, &mut app);
    assert!(app.takm_job.is_none(), "source 31 is refused before any worker writes");
    assert!(!dir.join("source.takm").exists());
    assert!(app.notice.take().is_some_and(|s| s.contains("30개까지")));
    assert_eq!(app.history.undo.len(), count);
    assert_eq!(app.views[0].path(), Some(source.as_path()));
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
}

#[test]
fn canceling_hidden_batch_dirty_prompt_restores_the_original_active_tab() {
    let ctx = egui::Context::default();
    let source = Path::new("hidden_cancel.tak");
    let output = Path::new("hidden_cancel.takm");
    let mut app = app(&ctx, source);
    let first = app.layout_tabs.active_id().unwrap();
    shown(&mut app, 1, output);
    let second = app.new_layout_tab();
    app.switch_layout_tab_internal(first);
    app.observe(false);
    app.switch_to_takm(&ctx, source, output);
    finish(&ctx, &mut app);
    drop_later(Some(std::mem::take(&mut app.views[1])));
    dock::remove_view(&mut app.dock, 1);
    dirtify(&mut app.views[0]);
    app.switch_layout_tab_internal(second);
    app.observe(false);
    let count = app.history.undo.len();
    app.undo_redo(&ctx, false);
    assert!(app.dirty_prompt.is_some());
    assert_eq!(app.layout_tabs.active_id(), Some(first));
    app.cancel_dirty_transition(&ctx);
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    assert_eq!(app.history.undo.len(), count);
    app.undo_redo(&ctx, false);
    app.discard_pending_transition(&ctx);
    assert_eq!(app.layout_tabs.active_id(), Some(second));
    assert_eq!(app.layout_tabs.tab(first).unwrap().layout.as_ref().unwrap().views[0].path(), Some(source));
}

#[cfg(unix)]
#[test]
fn canonical_source_alias_is_checked_for_dirty_edits_before_conversion_worker_writes() {
    let ctx = egui::Context::default();
    let _dir_guard = fixture("alias_dirty");
    let dir = _dir_guard.to_path_buf();
    let source = dir.join("source.tak");
    let alias = dir.join("alias.tak");
    std::fs::write(&source, tak_core::sample_tak::sample_tak()).unwrap();
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    let mut app = app(&ctx, &source);
    let metadata = std::fs::metadata(&source).unwrap();
    app.views[0].source_stamp = Some(Arc::new(Mutex::new(Some((std::fs::canonicalize(&source).unwrap(), metadata.len(), metadata.modified().ok())))));
    dirtify(&mut app.views[0]);
    app.save_takm(&ctx, alias.clone());
    finish(&ctx, &mut app);
    assert!(!alias.with_extension("takm").exists());
    assert!(app.views[0].marker_dirty());
    assert!(app.takm_note.as_ref().unwrap().0.contains("Ctrl+S"));
}

#[cfg(unix)]
#[test]
fn existing_destination_alias_at_the_file_cap_reuses_the_open_document() {
    let ctx = egui::Context::default();
    let _dir_guard = fixture("alias_at_cap");
    let dir = _dir_guard.to_path_buf();
    let actual = dir.join("actual");
    let alias = dir.join("alias");
    std::fs::create_dir_all(&actual).unwrap();
    std::os::unix::fs::symlink(&actual, &alias).unwrap();
    let source = actual.join("source.tak");
    let bytes = tak_core::sample_tak::sample_tak();
    std::fs::write(&source, &bytes).unwrap();
    let output = takm::convert(&source, None).unwrap().out;
    let original_output = std::fs::read(&output).unwrap();
    let mut app = app(&ctx, &source);
    shown(&mut app, 1, &output);
    let first = app.layout_tabs.active_id().unwrap();
    let destination = app.views[1].edit_document.clone().unwrap();
    for n in 0..MAX_OPEN_FILES - 2 {
        if n % 4 == 0 { app.new_layout_tab(); }
        shown(&mut app, n % 4, &PathBuf::from(format!("alias_cap_other_{n}.tak")));
    }
    app.switch_layout_tab_internal(first);
    app.observe(false);
    let count = app.history.undo.len();
    app.save_takm(&ctx, alias.join("source.tak"));
    finish(&ctx, &mut app);
    assert!(app.notice.is_none(), "an alias to the existing destination counts once");
    assert!(Arc::ptr_eq(app.views[0].edit_document.as_ref().unwrap(), &destination));
    assert_eq!(app.views[0].path(), Some(output.as_path()));
    assert_eq!(app.history.undo.len(), count + 1);
    assert_eq!(std::fs::read(&output).unwrap(), original_output);
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
}
