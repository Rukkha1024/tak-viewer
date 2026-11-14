//! Orbit camera (Motive style): right drag orbits around a pivot, middle drag pans,
//! wheel zooms toward the pivot (left button selects markers, see `ui/scene.rs`).
//! Y-up world (observed Motive convention, unverified units).

use eframe::egui::{pos2, Pos2, Rect};
use tak_core::geom::{v3, Vec3};

pub const WORLD_UP: Vec3 = v3(0.0, 1.0, 0.0);
pub const NEAR: f32 = 0.02;
const MIN_DISTANCE: f32 = 0.05;
const MAX_DISTANCE: f32 = 500.0;
const PITCH_LIMIT: f32 = 89.0 * std::f32::consts::PI / 180.0;
pub const ORBIT_SPEED: f32 = 0.006; // radians per pixel
pub const ZOOM_SPEED: f32 = 0.002; // per scroll point

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub pivot: Vec3,
    /// Rotation around world Y; 0 = camera on +Z looking toward -Z.
    pub yaw: f32,
    /// Elevation; positive = camera above the pivot.
    pub pitch: f32,
    pub distance: f32,
    /// Vertical field of view (radians).
    pub fov_y: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            pivot: v3(0.0, 0.9, 0.0),
            yaw: 35f32.to_radians(),
            pitch: 22f32.to_radians(),
            distance: 6.0,
            fov_y: 45f32.to_radians(),
        }
    }
}

/// Camera basis for one frame of drawing.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub eye: Vec3,
    pub forward: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub focal_px: f32,
    pub center: Pos2,
}

impl Camera {
    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        self.pivot + v3(cp * sy, sp, cp * cy) * self.distance
    }

    pub fn view(&self, rect: Rect) -> View {
        let eye = self.eye();
        let forward = (self.pivot - eye).normalized();
        let right = forward.cross(WORLD_UP).normalized();
        let up = right.cross(forward);
        let focal_px = (rect.height() * 0.5) / (self.fov_y * 0.5).tan();
        View { eye, forward, right, up, focal_px, center: rect.center() }
    }

    /// Right drag: orbit around the pivot (pivot stays fixed).
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * ORBIT_SPEED;
        self.pitch = (self.pitch + dy * ORBIT_SPEED).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Middle drag: move pivot and eye together in the screen plane (content follows the mouse).
    pub fn pan(&mut self, dx: f32, dy: f32, rect: Rect) {
        let v = self.view(rect);
        let world_per_px = self.distance / v.focal_px;
        self.pivot = self.pivot + v.right * (-dx * world_per_px) + v.up * (dy * world_per_px);
    }

    /// Wheel: positive scroll (wheel up) zooms in.
    pub fn zoom(&mut self, scroll: f32) {
        self.distance = (self.distance * (-scroll * ZOOM_SPEED).exp()).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Frame a box tightly for a viewport of `aspect` = width / height: the long horizontal
    /// axis runs across the screen, then the smallest distance that keeps all 8 corners
    /// inside 92% of the view is found by bisection.
    pub fn frame_box(&mut self, min: Vec3, max: Vec3, aspect: f32) {
        if !(min.is_finite() && max.is_finite()) || !(aspect > 0.0) {
            return;
        }
        let ext = max - min;
        self.yaw = if ext.x >= ext.z { 20f32.to_radians() } else { 110f32.to_radians() };
        self.pitch = 18f32.to_radians();
        self.fit_box(min, max, aspect);
    }

    /// Like `frame_box` but keeps the current yaw, pitch and field of view (capture presets).
    pub fn fit_box(&mut self, min: Vec3, max: Vec3, aspect: f32) {
        if !(min.is_finite() && max.is_finite()) || !(aspect > 0.0) {
            return;
        }
        self.pivot = (min + max) * 0.5;
        let corners: Vec<Vec3> = (0..8)
            .map(|i| v3(if i & 1 == 0 { min.x } else { max.x }, if i & 2 == 0 { min.y } else { max.y }, if i & 4 == 0 { min.z } else { max.z }))
            .collect();
        let tan_v = (self.fov_y * 0.5).tan();
        let tan_h = tan_v * aspect;
        let fits = |cam: &Camera| {
            let eye = cam.eye();
            let forward = (cam.pivot - eye).normalized();
            let right = forward.cross(WORLD_UP).normalized();
            let up = right.cross(forward);
            corners.iter().all(|&c| {
                let d = c - eye;
                let z = d.dot(forward);
                z > NEAR && (d.dot(right) / (z * tan_h)).abs() <= 0.92 && (d.dot(up) / (z * tan_v)).abs() <= 0.92
            })
        };
        let (mut lo, mut hi) = (MIN_DISTANCE, MAX_DISTANCE);
        for _ in 0..50 {
            let mid = 0.5 * (lo + hi);
            self.distance = mid;
            if fits(self) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        self.distance = hi.max(1.0);
    }
}

impl View {
    /// Camera-space coordinates (x right, y up, z forward depth).
    pub fn to_camera(&self, p: Vec3) -> Vec3 {
        let d = p - self.eye;
        v3(d.dot(self.right), d.dot(self.up), d.dot(self.forward))
    }

    pub fn camera_to_screen(&self, c: Vec3) -> Pos2 {
        pos2(self.center.x + c.x * self.focal_px / c.z, self.center.y - c.y * self.focal_px / c.z)
    }

    /// Screen position and depth; None when behind the near plane.
    pub fn project(&self, p: Vec3) -> Option<(Pos2, f32)> {
        let c = self.to_camera(p);
        if c.z < NEAR || !c.is_finite() {
            return None;
        }
        Some((self.camera_to_screen(c), c.z))
    }

    /// Segment clipped to the near plane; None when fully behind.
    pub fn project_segment(&self, a: Vec3, b: Vec3) -> Option<(Pos2, Pos2)> {
        let mut ca = self.to_camera(a);
        let mut cb = self.to_camera(b);
        if !(ca.is_finite() && cb.is_finite()) || (ca.z < NEAR && cb.z < NEAR) {
            return None;
        }
        if ca.z < NEAR {
            let t = (NEAR - ca.z) / (cb.z - ca.z);
            ca = ca + (cb - ca) * t;
        } else if cb.z < NEAR {
            let t = (NEAR - cb.z) / (ca.z - cb.z);
            cb = cb + (ca - cb) * t;
        }
        Some((self.camera_to_screen(ca), self.camera_to_screen(cb)))
    }

    /// Screen radius of a sphere of world radius `r` at depth `z`.
    pub fn radius_px(&self, r: f32, z: f32) -> f32 {
        r * self.focal_px / z
    }

    /// Direction-only projection for the orientation gizmo (x right, y down).
    pub fn gizmo_dir(&self, d: Vec3) -> (f32, f32, f32) {
        (d.dot(self.right), -d.dot(self.up), d.dot(self.forward))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::vec2;

    fn rect() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))
    }

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn pivot_projects_to_center() {
        let cam = Camera::default();
        let v = cam.view(rect());
        let (p, z) = v.project(cam.pivot).unwrap();
        assert!(close(p.x, 400.0, 1e-3) && close(p.y, 300.0, 1e-3));
        assert!(close(z, cam.distance, 1e-4));
    }

    #[test]
    fn orbit_keeps_pivot_and_distance() {
        let mut cam = Camera::default();
        let pivot = cam.pivot;
        let eye_before = cam.eye();
        cam.orbit(120.0, -45.0);
        assert_eq!(cam.pivot, pivot);
        assert!(close((cam.eye() - pivot).length(), cam.distance, 1e-4));
        assert!((cam.eye() - eye_before).length() > 0.1);
        let (p, _) = cam.view(rect()).project(pivot).unwrap();
        assert!(close(p.x, 400.0, 1e-3) && close(p.y, 300.0, 1e-3));
    }

    #[test]
    fn pitch_is_clamped() {
        let mut cam = Camera::default();
        cam.orbit(0.0, 1.0e6);
        assert!(cam.pitch <= PITCH_LIMIT + 1e-6);
        cam.orbit(0.0, -2.0e6);
        assert!(cam.pitch >= -PITCH_LIMIT - 1e-6);
    }

    #[test]
    fn pan_moves_content_with_mouse() {
        let mut cam = Camera::default();
        let probe = cam.pivot;
        let r = rect();
        let (before, _) = cam.view(r).project(probe).unwrap();
        cam.pan(50.0, 20.0, r);
        let (after, _) = cam.view(r).project(probe).unwrap();
        assert!(close(after.x - before.x, 50.0, 0.05), "dx {}", after.x - before.x);
        assert!(close(after.y - before.y, 20.0, 0.05), "dy {}", after.y - before.y);
        assert!(close(cam.distance, Camera::default().distance, 1e-6));
    }

    #[test]
    fn zoom_in_and_out() {
        let mut cam = Camera::default();
        let d0 = cam.distance;
        cam.zoom(100.0);
        assert!(cam.distance < d0);
        cam.zoom(-100.0);
        assert!(close(cam.distance, d0, 1e-4));
        cam.zoom(1.0e7);
        assert!(cam.distance >= MIN_DISTANCE);
    }

    #[test]
    fn behind_camera_not_projected_and_segment_clipped() {
        let cam = Camera::default();
        let v = cam.view(rect());
        let behind = v.eye - v.forward * 1.0;
        assert!(v.project(behind).is_none());
        let front = cam.pivot;
        assert!(v.project_segment(behind, front).is_some());
        assert!(v.project_segment(behind, behind - v.forward).is_none());
    }

    #[test]
    fn frame_box_is_tight_and_contains_corners() {
        let mut cam = Camera::default();
        let (lo, hi) = (v3(-5.0, 0.0, -1.0), v3(5.0, 1.8, 1.0));
        let r = rect();
        cam.frame_box(lo, hi, r.width() / r.height());
        let v = cam.view(r);
        let mut max_x: f32 = 0.0;
        for i in 0..8 {
            let p = v3(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
            let (s, _) = v.project(p).unwrap();
            assert!(s.x >= 0.0 && s.x <= 800.0 && s.y >= 0.0 && s.y <= 600.0, "{s:?}");
            max_x = max_x.max((s.x - 400.0).abs());
        }
        // Tight: the widest corner uses most of the half-width.
        assert!(max_x > 300.0, "{max_x}");
    }

    #[test]
    fn frame_box_tall_box_fits_vertically() {
        let mut cam = Camera::default();
        let (lo, hi) = (v3(-1.0, 0.0, -2.0), v3(1.0, 2.0, 2.0));
        cam.frame_box(lo, hi, 800.0 / 600.0);
        let v = cam.view(rect());
        for p in [lo, hi, v3(1.0, 0.0, -2.0), v3(-1.0, 2.0, 2.0)] {
            let (s, _) = v.project(p).unwrap();
            assert!(s.y >= 0.0 && s.y <= 600.0 && s.x >= 0.0 && s.x <= 800.0, "{s:?}");
        }
    }
}
