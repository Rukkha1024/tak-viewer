use super::*;
use std::time::{Duration, UNIX_EPOCH};

fn stamp() -> Stamp {
    Stamp {
        size: 33_000_000,
        modified: UNIX_EPOCH + Duration::from_secs(1),
    }
}

#[test]
fn saved_progress_mapping_and_menu_tick_agree() {
    for (value, word, mark, tick) in [
        (None, "Empty", "·", Some(0)),
        (Some(0), "Empty", "·", Some(0)),
        (Some(1), "Recorded", "R", Some(1)),
        (Some(2), "Reviewed", "V", Some(2)),
        (Some(3), "Cleaned", "C", Some(3)),
        (Some(4), "Labeled", "L", Some(4)),
        (Some(5), "Exported", "E", Some(5)),
        (Some(7), "Progress 7", "7", None),
    ] {
        let state = State(value);
        assert_eq!(state.label(), word);
        assert_eq!(state.mark().0, mark);
        assert_eq!(state.choice(), tick);
    }
}

#[test]
fn cached_value_and_blank_both_invalidate_on_size_or_mtime_change() {
    let mut cache = Cache::default();
    let file = Path::new("static.tak");
    cache.show();
    cache.remember(file.to_owned(), stamp(), Some(State(Some(1))));
    assert_eq!(
        cache.visible_row(file, stamp()),
        Some((stamp(), State(Some(1))))
    );
    let resized = Stamp {
        size: stamp().size + 1,
        ..stamp()
    };
    assert_eq!(cache.visible_row(file, resized), None);
    assert_eq!(cache.pending, vec![(file.to_owned(), resized)]);
    cache.remember(file.to_owned(), resized, None);
    cache.begin_frame();
    cache.show();
    assert_eq!(cache.visible_row(file, resized), None);
    assert!(cache.pending.is_empty());
    let newer = Stamp {
        modified: resized.modified + Duration::from_secs(1),
        ..resized
    };
    assert_eq!(cache.visible_row(file, newer), None);
    assert_eq!(cache.pending, vec![(file.to_owned(), newer)]);
}

#[test]
fn hidden_opening_and_inflight_frames_keep_work_bounded() {
    let ctx = egui::Context::default();
    let mut cache = Cache::default();
    let path = Path::new("first.tak");
    cache.visible_row(path, stamp());
    cache.finish_frame(&ctx, false);
    assert!(cache.worker.is_none());
    assert!(cache.pending.is_empty());

    cache.show();
    for row in 0..1000 {
        cache.visible_row(Path::new(&format!("{row}.tak")), stamp());
    }
    assert_eq!(cache.pending.len(), BATCH);
    cache.finish_frame(&ctx, true);
    assert!(cache.worker.is_none());

    cache.begin_frame();
    cache.show();
    cache.running = true;
    assert_eq!(cache.visible_row(path, stamp()), None);
    assert!(cache.pending.is_empty());
}

#[test]
fn open_priority_stops_inflight_batch_without_caching_an_interruption() {
    let paused = AtomicBool::new(false);
    let batch: Vec<_> = (0..BATCH)
        .map(|i| (PathBuf::from(format!("{i}.tak")), stamp()))
        .collect();
    let mut read_paths = Vec::new();
    let answers = read_batch(batch, &paused, |path, _, can_read| {
        assert!(can_read());
        read_paths.push(path.to_owned());
        if path == Path::new("1.tak") {
            paused.store(true, Ordering::Relaxed);
            assert!(!can_read());
        }
        Some(State(Some(4)))
    });
    assert_eq!(read_paths, [Path::new("0.tak"), Path::new("1.tak")]);
    assert_eq!(
        answers,
        [((PathBuf::from("0.tak"), stamp()), Some(State(Some(4))))]
    );
}

#[test]
fn changed_stamp_and_unreadable_path_do_not_report_progress() {
    let tmp = tak_core::test_support::TempDir::new("badge_bad_files");
    let path = tmp.join("broken.takm");
    assert_eq!(read_row(&path, stamp(), &|| true), None);
    std::fs::write(&path, b"not a take").unwrap();
    assert_eq!(read_row(&path, stamp(), &|| true), None);
    let actual = Stamp::of(std::fs::metadata(&path).unwrap()).unwrap();
    assert_eq!(read_row(&path, actual, &|| true), None);
}

#[test]
fn fixed_slot_and_menu_use_the_mark_without_stealing_row_space() {
    let ctx = egui::Context::default();
    let menu_id = egui::Id::new("progress-test");
    let state = State(Some(3));
    let row = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(260.0, 22.0));
    let mark = mark_slot(row, 16.0);
    assert_eq!(mark.width(), SLOT);
    assert_eq!(mark.right(), 16.0 + SLOT);

    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
    let draw = |events| {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let pick = row_mark(
                        ui,
                        row,
                        16.0,
                        menu_id,
                        Target::Local("real-looking.tak".into()),
                        Some(state),
                        None,
                        true,
                    );
                    assert!(pick.is_none(), "TAK choice is disabled");
                });
            },
        )
    };
    let pointer = |pressed| egui::Event::PointerButton {
        pos: mark.center(),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    draw(vec![]);
    draw(vec![
        egui::Event::PointerMoved(mark.center()),
        pointer(true),
    ]);
    let _ = draw(vec![pointer(false)]);
    assert!(ctx.memory(|m| m.is_popup_open(menu_id.with("progress-menu"))));
    let output = draw(vec![]);
    let texts: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.text()),
            _ => None,
        })
        .collect();
    for word in WORDS {
        assert!(texts.contains(&word), "{texts:?}");
    }
    let ticks = output
        .shapes
        .iter()
        .filter(|shape| {
            matches!(&shape.shape, egui::Shape::Path(path)
        if path.points.len() == 3 && !path.closed && path.stroke.width == 1.2)
        })
        .count();
    assert_eq!(ticks, 1);
    assert!(texts.contains(&"TAKM에서만 바꿔요."));
}
