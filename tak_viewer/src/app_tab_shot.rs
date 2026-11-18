use super::TakViewerApp;
use eframe::egui::{self, Event, Modifiers, MouseWheelUnit, RawInput};
use std::path::Path;

const MAX_SCREENSHOT_TABS: usize = 30;
const USAGE: &str = "--shot-tabs [N[:POINTS]]는 --headless --screenshot과 함께 써요. N은 0~30, POINTS는 정수예요.";

pub(crate) struct TabShot {
    count: usize,
    points: i32,
}

impl TabShot {
    pub(crate) fn take(args: &mut Vec<String>, headless: bool) -> Result<Option<Self>, &'static str> {
        let Some(i) = args.iter().position(|arg| arg == "--shot-tabs") else {
            return Ok(None);
        };
        if !headless || args.iter().filter(|arg| *arg == "--shot-tabs").count() != 1 {
            return Err(USAGE);
        }
        let value = args.get(i + 1).filter(|value| {
            !value.starts_with("--") && !crate::desktop_drop::supported(Path::new(value))
        });
        let Some(value) = value else {
            args.remove(i);
            return Ok(Some(Self { count: 0, points: 0 }));
        };
        let (count, points) = value.split_once(':').unwrap_or((value, "0"));
        let count = count.parse().ok().filter(|n| *n <= MAX_SCREENSHOT_TABS).ok_or(USAGE)?;
        let points = points.parse().map_err(|_| USAGE)?;
        args.drain(i..i + 2);
        Ok(Some(Self { count, points }))
    }

    pub(crate) fn seed(&self, app: &mut TakViewerApp) {
        let active = app.layout_tabs.active_id().expect("headless screenshot starts with a tab");
        for _ in app.layout_tabs.tabs().len()..self.count {
            app.layout_tabs.add(None);
        }
        app.layout_tabs.activate(active);
    }

    pub(crate) fn wheel_pending(&self) -> bool { self.points != 0 }

    /// Inject once the real tabs have loaded and the strip can own the wheel.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn tab_shot_keeps_grouped_file_arguments_and_accepts_legacy_padding() {
        let mut full = args(&["--shot-tabs", "a.tak", "b.TAKM"]);
        let shot = TabShot::take(&mut full, true).unwrap().unwrap();
        assert_eq!((shot.count, shot.points), (0, 0));
        assert_eq!(full, args(&["a.tak", "b.TAKM"]));

        let mut padded = args(&["--shot-tabs", "30:1344", "a.tak"]);
        let shot = TabShot::take(&mut padded, true).unwrap().unwrap();
        assert_eq!((shot.count, shot.points), (30, 1344));
        assert_eq!(padded, args(&["a.tak"]));
    }

    #[test]
    fn tab_shot_rejects_duplicates_non_headless_and_bad_wheel_argument() {
        assert!(TabShot::take(&mut args(&["--shot-tabs"]), false).is_err());
        for bad in [
            &["--shot-tabs", "--shot-tabs"][..],
            &["--shot-tabs", "31"],
            &["--shot-tabs", "8:NaN"],
        ] {
            assert!(TabShot::take(&mut args(bad), true).is_err(), "{bad:?}");
        }
        let mut reverse = args(&["--shot-tabs", "0:-400", "a.tak"]);
        let shot = TabShot::take(&mut reverse, true).unwrap().unwrap();
        assert_eq!((shot.count, shot.points), (0, -400));
    }
}
