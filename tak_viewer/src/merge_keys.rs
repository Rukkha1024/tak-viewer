use crate::ui::graph::tracks;
use eframe::egui;
use std::sync::OnceLock;

#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Up,
    Down,
}

impl Direction {
    pub fn source_and_destination<T>(self, [upper, lower]: [T; 2]) -> (T, T) {
        match self {
            Self::Up => (lower, upper),
            Self::Down => (upper, lower),
        }
    }
}

pub(crate) fn toolbar(ui: &mut egui::Ui, blocked: Option<&str>) -> Option<Direction> {
    ui.horizontal(|ui| {
        let mut clicked = None;
        for (label, direction) in [
            ("Merge Keys Up", Direction::Up),
            ("Merge Keys Down", Direction::Down),
        ] {
            let button = ui.add_enabled(blocked.is_none(), egui::Button::new(label));
            if let Some(reason) = blocked {
                button.on_disabled_hover_text(reason);
            } else if button.clicked() {
                clicked = Some(direction);
            }
        }
        clicked
    })
    .inner
}

// Headless-only. Some(None) shows the ready toolbar without changing the document.
pub(crate) static PROBE: OnceLock<Option<Direction>> = OnceLock::new();

pub(crate) fn parse_probe(args: &mut Vec<String>, headless: bool) -> Result<(), &'static str> {
    let Some(index) = args.iter().position(|arg| arg == "--merge-keys") else {
        return Ok(());
    };
    if !headless
        || !args.iter().any(|arg| arg == "--screenshot")
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--bench" | "--bench-load" | "--capture"))
        || !tracks::PROBE
            .get()
            .is_some_and(|probe| probe.layout == tracks::Layout::Tracks)
    {
        return Err("--merge-keys는 --headless --screenshot, --graph-layout tracks와 함께 써요.");
    }
    let direction = match args.get(index + 1).map(String::as_str) {
        Some("preview") => None,
        Some("up") => Some(Direction::Up),
        Some("down") => Some(Direction::Down),
        _ => return Err("--merge-keys 뒤에 preview, up, down 중 하나를 써요."),
    };
    args.drain(index..index + 2);
    let _ = PROBE.set(direction);
    Ok(())
}
