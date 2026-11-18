use super::theme;
use crate::camera::View;
use eframe::egui::{self, pos2, vec2, Align2, FontId, Stroke};
use tak_core::geom::Vec3;
use tak_core::view_data::ViewData;

impl crate::views::View {
    pub fn measurement(&self, enabled: bool) -> Option<Measurement> {
        if !enabled || !matches!(self.selected.len(), 2 | 3) { return None; }
        let _span = crate::bench_timing::span("scene.measurement_samples");
        Measurement::at(self.data.as_deref()?, self.pb.frame, &self.ordered_selection())
    }
}

// Match the existing selected-marker XYZ display of ViewData positions.
pub const DISPLAY_MM_PER_UNIT: f64 = 1000.0;

#[derive(Clone, Copy)]
pub struct Measurement {
    points: [Option<Vec3>; 3],
    angle: bool,
    value: Option<f64>,
}

impl Measurement {
    pub fn at(data: &ViewData, frame: i64, order: &[usize]) -> Option<Self> {
        if !matches!(order.len(), 2 | 3) { return None; }
        let mut points = [None; 3];
        for (point, &marker) in points.iter_mut().zip(order) {
            *point = data.markers.get(marker).and_then(|m| m.position(frame)).filter(|p| p.is_finite());
        }
        let angle = order.len() == 3;
        let value = match (points, angle) {
            ([Some(a), Some(b), Some(c)], true) => angle_at(a, b, c),
            ([Some(a), Some(b), _], false) => Some(length(delta(a, b)) * DISPLAY_MM_PER_UNIT),
            _ => None,
        };
        Some(Self { points, angle, value })
    }

    pub fn name(self) -> &'static str {
        if self.angle { "각도" } else { "거리" }
    }

    pub fn text(self) -> String {
        match self.value {
            Some(value) if self.angle => format!("{value:.2}°"),
            Some(value) => format!("{value:.2} mm"),
            None => "—".into(),
        }
    }

    pub fn paint(self, painter: &egui::Painter, view: &View) {
        let rect = painter.clip_rect();
        let stroke = Stroke::new(1.5_f32, theme::HUD_TEXT);
        let anchor = match (self.points, self.angle) {
            ([Some(a), Some(b), Some(c)], true) if self.value.is_some() => {
                for end in [a, c] {
                    if let Some((from, to)) = view.project_segment(b, end) {
                        painter.line_segment([from, to], stroke);
                    }
                }
                view.project(b).map(|(p, _)| p - vec2(0.0, 20.0))
            }
            ([Some(a), Some(b), _], false) => view.project_segment(a, b).map(|(from, to)| {
                painter.line_segment([from, to], stroke);
                from.lerp(to, 0.5)
            }),
            _ => self.points.into_iter().flatten().filter_map(|p| view.project(p))
                .map(|(p, _)| p).find(|&p| rect.contains(p)).map(|p| p - vec2(0.0, 20.0))
                .or_else(|| Some(pos2(rect.right() - 18.0, rect.top() + 18.0))),
        };
        if let Some(anchor) = anchor.filter(|&p| rect.contains(p)) {
            let galley = painter.layout_no_wrap(self.text(), FontId::proportional(13.0), theme::HUD_TEXT);
            let label = Align2::CENTER_CENTER.anchor_size(anchor, galley.size());
            painter.rect_filled(label.expand2(vec2(5.0, 2.0)), 3.0, theme::PANEL_BG);
            painter.galley(label.min, galley, theme::HUD_TEXT);
        }
    }
}

fn delta(a: Vec3, b: Vec3) -> [f64; 3] {
    [a.x as f64 - b.x as f64, a.y as f64 - b.y as f64, a.z as f64 - b.z as f64]
}

fn length([x, y, z]: [f64; 3]) -> f64 { (x * x + y * y + z * z).sqrt() }

fn angle_at(a: Vec3, b: Vec3, c: Vec3) -> Option<f64> {
    let u = delta(a, b);
    let v = delta(c, b);
    if u == [0.0; 3] || v == [0.0; 3] { return None; }
    let cross = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    let dot = u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    Some(length(cross).atan2(dot).to_degrees())
}

#[cfg(test)]
#[path = "scene_measurement_tests.rs"]
mod tests;
