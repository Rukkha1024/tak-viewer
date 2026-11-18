use super::*;
use eframe::egui::{vec2, Pos2};

fn viewport() -> Rect {
    Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))
}

#[test]
fn orthographic_projection_round_trips_signed_depth_for_all_axes() {
    let rect = viewport();
    let mut cam = Camera::default();
    for preset in Preset::ALL.into_iter().skip(1) {
        cam.preset = preset;
        let view = cam.view(rect);
        let (center, depth) = view.project(cam.pivot).unwrap();
        assert_eq!((center, depth), (rect.center(), 0.0), "{preset:?}");
        for depth in [-10.0, 0.0, 10.0] {
            let point = cam.pivot + view.right * 0.5 + view.up * 0.25 + view.forward * depth;
            let (screen, projected_depth) = view.project(point).unwrap();
            let world_per_pixel = cam.distance / view.focal_px;
            let offset = (screen - view.center) * world_per_pixel;
            let unprojected = view.eye + view.right * offset.x - view.up * offset.y
                + view.forward * projected_depth;
            assert!((unprojected - point).length() < 1e-5, "{preset:?}");
            assert!((screen - (center + vec2(0.5, -0.25) / world_per_pixel)).length() < 1e-4);
            assert_eq!(view.radius_px(0.01, depth), view.radius_px(0.01, 0.0));
        }
    }
}

#[test]
fn orthographic_pan_zoom_fit_and_orbit_use_the_expected_axes() {
    let rect = viewport();
    let mut cam = Camera { preset: Preset::Top, ..Camera::default() };
    let (lo, hi) = (v3(-3.0, 0.0, -1.0), v3(3.0, 3.0, 1.0));
    cam.frame_box(lo, hi, rect.width() / rect.height());
    assert_eq!(cam.preset, Preset::Top);
    let view = cam.view(rect);
    assert!(rect.contains(view.project(lo).unwrap().0));
    assert!(rect.contains(view.project(hi).unwrap().0));
    let before = cam.pivot;
    cam.pan(50.0, 20.0, rect);
    let (screen, _) = cam.view(rect).project(before).unwrap();
    assert!((screen - rect.center() - vec2(50.0, 20.0)).length() < 0.01);
    let distance = cam.distance;
    cam.zoom(100.0);
    assert!(cam.distance < distance);
    let orientation = (cam.yaw, cam.pitch);
    cam.orbit(0.0, 0.0);
    assert_eq!(cam.preset, Preset::Perspective);
    assert_eq!((cam.yaw, cam.pitch), orientation);
}
