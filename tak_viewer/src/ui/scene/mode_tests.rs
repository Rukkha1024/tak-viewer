use super::*;
use crate::camera::Preset;

#[test]
fn orthographic_picking_and_box_selection_use_projected_points_and_depth() {
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
    let mut cam = Camera { preset: Preset::Front, ..Camera::default() };
    let positions = [v3(0.0, 1.0, 6.0), v3(0.0, 1.0, -6.0), v3(0.2, 1.2, 0.0)];
    for (preset, nearest) in [(Preset::Front, 0), (Preset::Back, 1)] {
        cam.preset = preset;
        let view = cam.view(rect);
        let projected: Vec<_> = positions.iter().enumerate().map(|(marker, &position)| {
            let (pos, depth) = view.project(position).unwrap();
            Projected { marker, pos, depth, radius: view.radius_px(MARKER_RADIUS_M, depth) }
        }).collect();
        assert_eq!(projected[0].pos, projected[1].pos);
        assert_eq!(pick(&projected, projected[0].pos), Some(nearest));
        let at = projected[2].pos;
        assert_eq!(pick_in_box(&projected, at - vec2(3.0, 3.0), at + vec2(3.0, 3.0)), (vec![2], Some(2)));
    }
}

#[test]
fn orthographic_plate_pick_rejects_edge_on_quads() {
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
    let corners = [v3(-1.0, 0.0, -1.0), v3(1.0, 0.0, -1.0), v3(1.0, 0.0, 1.0), v3(-1.0, 0.0, 1.0)];
    for (preset, hit) in [(Preset::Top, Some(9)), (Preset::Bottom, Some(9)), (Preset::Front, None)] {
        let cam = Camera { preset, ..Camera::default() };
        let view = cam.view(rect);
        let quad = PlateQuad { plate: 9, corners: corners.map(|p| view.project(p).unwrap().0) };
        assert_eq!(pick_plate(&[quad], view.project(v3(0.0, 0.0, 0.0)).unwrap().0), hit);
    }
}

#[test]
fn scene_modes_bound_trails_and_hide_selected_names_unless_hovered() {
    let selected: BTreeSet<_> = (0..100).rev().map(|m| m * 2).collect();
    assert_eq!(trail_markers(&selected).collect::<Vec<_>>(), [0, 2, 4, 6, 8, 10, 12, 14]);
    assert_eq!(TRAIL_MARKER_LIMIT as i64 * HISTORY_FRAMES, 2_000);
    for (selected, labeled) in [(false, false), (false, true), (true, false), (true, true)] {
        assert!(!show_marker_name(false, selected, labeled, false));
        assert!(show_marker_name(false, selected, labeled, true));
        assert_eq!(show_marker_name(true, selected, labeled, false), selected || labeled);
        assert!(show_marker_name(true, selected, labeled, true));
    }
}

#[test]
fn scene_modes_full_trail_geometry_stays_bounded_on_a_long_take() {
    let mut take = crate::views::sample_take("in-memory", &["a", "b", "c", "d", "e", "f", "g", "h", "i"]);
    take.end_frame = 1_000;
    for (m, trajectory) in take.trajectories.iter_mut().enumerate() {
        trajectory.frames = (0..=1_000).collect();
        trajectory.xyz = (0..=1_000).map(|f| [m as f32 / 10.0, 1.0, f as f32 / 1_000.0]).collect();
    }
    let data = ViewData::build(take);
    let ctx = egui::Context::default();
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
    let view = Camera::default().view(rect);
    let selected = (0..9).collect();
    let first_frame = 980;
    let started = std::time::Instant::now();
    for frame in first_frame..=data.end_frame {
        let out = ctx.run(egui::RawInput { screen_rect: Some(rect), ..Default::default() }, |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            for marker in trail_markers(&selected) {
                history(&painter, &view, &data, marker, frame, HISTORY_FRAMES);
            }
        });
        let segments = out.shapes.iter().filter(|s| matches!(&s.shape, Shape::LineSegment { .. })).count();
        assert_eq!(segments, 2_000);
        let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
    }
    let frames = data.end_frame - first_frame + 1;
    println!("full trails: {frames} frames × 2,000 segments, CPU build+tessellate avg {:.3} ms",
        started.elapsed().as_secs_f64() * 1_000.0 / frames as f64);
}
