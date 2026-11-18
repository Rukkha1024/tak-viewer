use super::*;
use std::time::{Duration, UNIX_EPOCH};

fn stamp() -> Stamp {
    Stamp {
        size: 33_000_000,
        modified: UNIX_EPOCH + Duration::from_secs(1),
    }
}

#[test]
fn mapping_keeps_only_confirmed_words() {
    assert_eq!(label(1), "Recorded");
    assert_eq!(label(4), "Labeled");
    assert_eq!(label(2), "Progress 2");
    assert_eq!(label(0), "Progress 0");
}

#[test]
fn cached_value_and_blank_both_invalidate_on_size_or_mtime_change() {
    let mut cache = Cache::default();
    let file = Path::new("static.tak");
    cache.show();
    cache.remember(file.to_owned(), stamp(), Some(1));
    assert_eq!(cache.visible_row(file, stamp()), Some(1));
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
        Some(4)
    });
    assert_eq!(read_paths, [Path::new("0.tak"), Path::new("1.tak")]);
    assert_eq!(answers, [((PathBuf::from("0.tak"), stamp()), Some(4))]);
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
