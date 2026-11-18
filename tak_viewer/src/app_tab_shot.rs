use super::TakViewerApp;
use eframe::egui::{self, Event, Modifiers, MouseWheelUnit, RawInput};

pub(crate) struct TabShot {
    count: usize,
    points: i32,
}

impl TabShot {
    pub(crate) fn take(
        args: &mut Vec<String>,
        headless: bool,
    ) -> Result<Option<Self>, &'static str> {
        const USAGE: &str = "--shot-tabs N[:POINTS]는 --headless --screenshot과 함께 써요. N은 1~30, POINTS는 정수예요.";
        let Some(i) = args.iter().position(|arg| arg == "--shot-tabs") else {
            return Ok(None);
        };
        if !headless || args.iter().filter(|arg| *arg == "--shot-tabs").count() != 1 {
            return Err(USAGE);
        }
        let value = args.get(i + 1).ok_or(USAGE)?;
        let (count, points) = value.split_once(':').unwrap_or((value, "0"));
        let count = count
            .parse()
            .ok()
            .filter(|n| (1..=30).contains(n))
            .ok_or(USAGE)?;
        let points = points.parse().map_err(|_| USAGE)?;
        args.drain(i..i + 2);
        Ok(Some(Self { count, points }))
    }

    pub(crate) fn seed(&self, app: &mut TakViewerApp) {
        let first = app
            .layout_tabs
            .active_id()
            .expect("headless screenshot starts with a tab");
        for _ in 1..self.count {
            app.layout_tabs.add(None);
        }
        app.layout_tabs.activate(first);
    }

    /// Inject once the real tab strip has been drawn and can own the wheel.
    pub(crate) fn inject(&mut self, app: &TakViewerApp, raw: &mut RawInput) {
        if self.points == 0 {
            return;
        }
        let Some(pos) = app.layout_strip.wheel.center() else {
            return;
        };
        raw.events.extend([
            Event::PointerMoved(pos),
            Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -(std::mem::take(&mut self.points) as f32)),
                modifiers: Modifiers::NONE,
            },
        ]);
    }
}
