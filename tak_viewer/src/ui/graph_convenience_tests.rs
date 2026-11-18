use super::*;
use std::sync::Arc;

#[test]
fn value_zoom_centers_and_clamps_each_axis_and_reset_restores_auto() {
    let mut zoom = ValueZoom::default();
    zoom.drag(1, -((2.0_f32.ln()) / 0.012));
    let (lo, hi) = zoom.bounds(1, (10.0, 30.0));
    assert!((lo - 15.0).abs() < 1e-6 && (hi - 25.0).abs() < 1e-6);
    assert_eq!(zoom.bounds(0, (10.0, 30.0)), (10.0, 30.0));
    zoom.drag(1, -10_000.0);
    assert_eq!(zoom.bounds(1, (10.0, 30.0)), (20.0 - 10.0 / 64.0, 20.0 + 10.0 / 64.0));
    zoom.drag(1, f32::NAN);
    assert_eq!(zoom.bounds(1, (f64::INFINITY, f64::NEG_INFINITY)), (f64::INFINITY, f64::NEG_INFINITY));
    zoom.drag(1, 10_000.0);
    assert_eq!(zoom.bounds(1, (10.0, 30.0)), (-620.0, 660.0));
    assert!(!zoom.is_auto());
    zoom = ValueZoom::default();
    assert_eq!(zoom.bounds(1, (10.0, 30.0)), (10.0, 30.0));
    assert!(zoom.is_auto());
}

#[test]
fn combining_channels_keeps_exact_samples_colors_and_empty_overlay_slots() {
    let triplet = || [theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z].into_iter().enumerate()
        .map(|(axis, color)| Lane {
            letter: ["X", "Y", "Z"][axis].into(), color, unit: "mm",
            runs: Arc::new(vec![vec![[1.0, axis as f64], [2.0, 5.0]], vec![[4.0, 7.0]]]),
        }).collect::<Vec<_>>();
    let channels = [triplet(), vec![], triplet()];
    let single_lane_sets: Vec<_> = combine(channels.clone()).collect();
    assert_eq!(single_lane_sets.len(), 9);
    for (marker, lanes) in channels.iter().enumerate() {
        for axis in 0..3 {
            let combined = &single_lane_sets[marker * 3 + axis];
            if let Some(channel) = lanes.get(axis) {
                assert_eq!(combined.len(), 1);
                assert!(Arc::ptr_eq(&combined[0].runs, &channel.runs));
                assert_eq!((combined[0].color, combined[0].unit), (channel.color, channel.unit));
            } else {
                assert!(combined.is_empty());
            }
        }
    }
}
