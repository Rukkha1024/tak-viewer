use super::*;
use crate::views::sample_data;
use tak_core::tak::MarkerKind;
use tak_core::view_data::segments;

#[test]
fn tracks_runs_cover_empty_full_alternating_and_single_frame() {
    let extent = ViewRange::full(10, 15);
    for (frames, runs, columns) in [
        (vec![], vec![], vec![]),
        ((10..=15).collect(), vec![(10, 15)], vec![0..6]),
        (
            vec![10, 12, 14],
            vec![(10, 10), (12, 12), (14, 14)],
            vec![0..1, 2..3, 4..5],
        ),
        (vec![12], vec![(12, 12)], vec![2..3]),
    ] {
        assert_eq!(segments(&frames), runs);
        assert_eq!(pixel_runs(&runs, extent, 6), columns);
    }
    assert_eq!(
        pixel_runs(&[(4, 10), (12, 20)], ViewRange::full(10, 12), 3),
        [0..1, 2..3]
    );
    assert_eq!(pixel_runs(&[(11, 11)], extent, 3), [0..1]);
}

#[test]
fn tracks_pixel_work_is_bounded_with_half_a_million_fragments() {
    let alternating = (0..1_000_000)
        .step_by(2)
        .map(|f| (f, f))
        .collect::<Vec<_>>();
    let extent = ViewRange::full(0, 999_999);
    let start = std::time::Instant::now();
    for _ in 0..25 {
        assert_eq!(pixel_runs(&alternating, extent, 1200), [0..1200]);
    }
    eprintln!(
        "25 rows x 1,200 columns x 500,000 fragments: {:?}",
        start.elapsed()
    );
}

#[test]
fn tracks_virtual_rows_and_reveal_work_independently_of_take_size() {
    assert_eq!(visible_rows(0.0, 19.0, 80_000), 0..1);
    assert_eq!(visible_rows(19.5, 38.0, 80_000), 1..4);
    assert_eq!(visible_rows(76.0, 19.0, 4), 4..4);
    assert_eq!(visible_rows(0.0, 19.0, 0), 0..0);
    assert_eq!(reveal_offset(2, 19.0, 38.0), None);
    assert_eq!(reveal_offset(0, 19.0, 38.0), Some(0.0));
    assert_eq!(reveal_offset(5, 19.0, 38.0), Some(76.0));
}

#[test]
fn tracks_named_order_precedes_unlabeled_and_modifier_clicks_share_selection() {
    let mut take = sample_data("rows.tak", &["Untitled", "Late", "Early", "Raw"]).take;
    for (index, slot) in [(1, 9), (2, 2)] {
        let marker = &mut take.trajectories[index];
        marker.kind = MarkerKind::OtherParent;
        marker.parent_uid = Some([13; 16]);
        marker.slot = Some(slot);
    }
    let data = ViewData::build(take);
    assert!(data.marker_order_handle().get().is_none());
    data.prepare_marker_order();
    let mut state = State {
        order: Some(data.marker_order_handle().clone()),
        ..Default::default()
    };
    let order = state.order(&data).unwrap();
    let ctrl = egui::Modifiers {
        ctrl: true,
        ..Default::default()
    };
    let shift = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    assert_eq!(order.indices, [2, 1, 0, 3]);
    let top = hit(order, 9.0, egui::Modifiers::NONE).unwrap();
    let second = hit(order, 27.0, ctrl).unwrap();
    let last = hit(order, 71.0, shift).unwrap();
    assert!(hit(order, 79.0, ctrl).is_none(), "blank body");
    assert!(
        hit(order, 9.0, egui::Modifiers { alt: true, ..ctrl }).is_none(),
        "other modifiers own their gestures"
    );
    let selected = state.select(&data, &[], top).unwrap();
    assert_eq!(selected, [2]);
    let selected = state.select(&data, &selected, second).unwrap();
    assert_eq!(selected, [2, 1]);
    let selected = state.select(&data, &selected, last).unwrap();
    assert_eq!(selected, [1, 0, 3]);
    let stale = ViewData::build(data.take.clone());
    assert!(state.select(&stale, &selected, top).is_none());
}
