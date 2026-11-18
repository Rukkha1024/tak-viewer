use super::*;
use tak_core::geom::v3;

#[test]
fn distance_and_angle_use_three_dimensional_positions_in_selection_order() {
    let mut take = crate::views::sample_take("measurement.tak", &["A", "B", "C"]);
    for (trajectory, point) in take.trajectories.iter_mut().zip([[0.0, 0.0, 3.0], [0.0; 3], [4.0, 0.0, 0.0]]) {
        trajectory.xyz = vec![point; 10].into();
    }
    let data = ViewData::build(take);
    let distance = Measurement::at(&data, 0, &[0, 2]).unwrap();
    assert_eq!(distance.text(), "5000.00 mm");
    let right = Measurement::at(&data, 0, &[0, 1, 2]).unwrap();
    assert_eq!(right.text(), "90.00°");
    let acute = Measurement::at(&data, 0, &[1, 0, 2]).unwrap();
    assert_eq!(acute.text(), "53.13°");
    assert_eq!(Measurement::at(&data, 0, &[0, 0]).unwrap().text(), "0.00 mm");
    assert_eq!(Measurement::at(&data, 0, &[0, 0, 2]).unwrap().text(), "—");
    assert_eq!(Measurement::at(&data, 10, &[0, 1, 2]).unwrap().text(), "—");
    assert_eq!(Measurement::at(&data, 0, &[0, 99]).unwrap().text(), "—");
    for order in [&[0][..], &[0, 1, 2, 3][..]] {
        assert!(Measurement::at(&data, 0, order).is_none());
    }
}

#[test]
fn angle_handles_opposite_rays_and_coincident_endpoints() {
    let center = v3(0.0, 0.0, 0.0);
    let left = v3(-1.0, 0.0, 0.0);
    let right = v3(1.0, 0.0, 0.0);
    assert_eq!(angle_at(left, center, right), Some(180.0));
    assert_eq!(angle_at(right, center, right), Some(0.0));
    assert_eq!(angle_at(left, center, center), None);
}
