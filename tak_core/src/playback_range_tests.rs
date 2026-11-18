use super::*;

#[test]
fn playback_working_range_loops_forward_and_reverse_at_the_same_speed() {
    let mut pb = Playback::new(100, 200, 8.0);
    pb.set_working_range(110, 114, 0.0);
    pb.set_speed(2.0, 0.0);
    pb.play(0.0);
    assert_eq!(pb.frame, 110);
    pb.tick(0.25);
    assert_eq!((pb.frame, pb.wraps), (114, 0));
    pb.tick(0.375);
    assert_eq!((pb.frame, pb.wraps), (111, 1));

    pb.set_reverse(true, 0.375);
    pb.tick(0.4375);
    assert_eq!((pb.frame, pb.wraps), (110, 1));
    pb.tick(0.625);
    assert_eq!((pb.frame, pb.wraps), (112, 2));
    pb.pause();
    pb.set_frame(110, 0.625);
    pb.play(0.625);
    assert_eq!((pb.frame, pb.wraps), (114, 3));

    pb.pause();
    pb.set_frame(200, 1.0);
    pb.play(1.0);
    assert_eq!(
        (pb.frame, pb.wraps),
        (110, 3),
        "start from outside in either direction"
    );
}

#[test]
fn playback_working_range_clamps_sorts_and_keeps_one_frame_in_both_directions() {
    let mut pb = Playback::new(100, 200, 8.0);
    pb.set_working_range(150, -10, 0.0);
    assert_eq!(pb.working_range(), (100, 150));
    pb.set_working_range(220, 250, 0.0);
    assert_eq!(pb.working_range(), (200, 200));
    for reverse in [false, true] {
        pb.pause();
        pb.set_reverse(reverse, 0.0);
        pb.set_frame(200, 0.0);
        pb.step(-50, 0.0);
        assert_eq!(pb.frame, 150);
        pb.play(0.0);
        assert_eq!(pb.frame, 200);
        for t in [0.125, 0.25, 3600.0] {
            assert!(!pb.tick(t));
            assert!(pb.playing);
            assert_eq!(pb.frame, 200);
        }
    }
}

#[test]
fn playback_working_range_seek_and_stepping_keep_full_bounds() {
    let mut pb = Playback::new(100, 200, 8.0);
    pb.set_working_range(110, 120, 0.0);
    pb.set_frame(180, 0.0);
    pb.step(5, 0.0);
    assert_eq!(pb.frame, 185);
    pb.play(0.0);
    pb.set_frame(200, 0.125);
    assert_eq!(pb.frame, 200);
    assert!(pb.tick(0.25));
    assert_eq!(pb.frame, 110);
    pb.set_working_range(150, 160, 0.25);
    pb.tick(0.375);
    assert_eq!(pb.frame, 150);
}

#[test]
fn playback_working_range_handles_a_large_skip_with_fixed_work() {
    let mut pb = Playback::new(0, i64::MAX, 1.0);
    pb.set_working_range(1, i64::MAX, 0.0);
    pb.set_frame(i64::MAX - 1, 0.0);
    pb.play(0.0);
    assert!(pb.tick(4.0));
    assert_eq!((pb.frame, pb.wraps), (3, 1));
    pb.tick(1.0e25);
    assert_eq!((pb.frame, pb.wraps), (3, 2));
}

#[test]
fn playback_working_range_clock_cost_by_span() {
    use std::hint::black_box;
    use std::time::Instant;

    const TICKS: u32 = 100_000;
    for length in [10, 500_000_000] {
        let mut pb = Playback::new(0, length + 200, 100.0);
        pb.set_working_range(100, length + 99, 0.0);
        pb.play(0.0);
        let start = Instant::now();
        for tick in 1..=TICKS {
            black_box(&mut pb).tick(f64::from(tick) * 6_000_001.0);
        }
        assert_eq!(pb.wraps, u64::from(TICKS));
        println!(
            "working range {length:>9} frames: {:.1} ns/tick ({} wraps)",
            start.elapsed().as_secs_f64() * 1e9 / f64::from(TICKS),
            pb.wraps
        );
    }
}
