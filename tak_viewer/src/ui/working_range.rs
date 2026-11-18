use eframe::egui::{
    self, pos2, vec2, Button, Id, Key, Rect, Response, RichText, Sense, Stroke, TextEdit, Ui,
};
use tak_core::playback::Playback;

use super::theme;

pub const STRIP_H: f32 = 9.0;
pub const FIELDS_H: f32 = 22.0;

pub fn fields_id(slot: usize, file_id: usize) -> Id {
    Id::new(("working_range_fields", slot, file_id))
}

/// egui can drop text focus before the app sees Escape. Leave it for the field draft.
pub fn owns_escape(ctx: &egui::Context, slot: usize, file_id: usize) -> bool {
    if !ctx.input(|input| input.key_pressed(Key::Escape)) {
        return false;
    }
    let id = fields_id(slot, file_id);
    ctx.memory(|memory| {
        ["start", "end"]
            .iter()
            .any(|field| memory.had_focus_last_frame(id.with(*field)))
    })
}

#[derive(Clone, Copy)]
enum Endpoint {
    Start,
    End,
}

impl Endpoint {
    fn split(self, (start, end): (i64, i64)) -> (i64, i64) {
        match self {
            Self::Start => (start, end),
            Self::End => (end, start),
        }
    }

    fn parse(self, text: &str, pb: &Playback) -> Result<(i64, i64), &'static str> {
        let frame = text
            .trim()
            .parse::<i64>()
            .map_err(|_| "Enter a frame number")?;
        if !(pb.first..=pb.last).contains(&frame) {
            return Err("Frame outside take");
        }
        let (_, other) = self.split(pb.working_range());
        match self {
            Self::Start if frame <= other => Ok((frame, other)),
            Self::End if frame >= other => Ok((other, frame)),
            _ => Err("Start must be before or at end"),
        }
    }
}

#[derive(Clone, Copy)]
struct Grab {
    other: i64,
    offset: f32,
}

#[allow(clippy::too_many_arguments)]
pub fn strip(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    pb: &mut Playback,
    now: f64,
    x_of: impl Fn(i64) -> f32,
    frame_at: impl Fn(f32) -> i64,
    enabled: bool,
) {
    let (start, end) = pb.working_range();
    let (x_start, x_end) = (x_of(start), x_of(end));
    let mid = (x_start + x_end) / 2.0;
    for (which, x, left, right) in [
        (
            Endpoint::Start,
            x_start,
            x_start - 6.0,
            (x_start + 6.0).min(mid),
        ),
        (Endpoint::End, x_end, (x_end - 6.0).max(mid), x_end + 6.0),
    ] {
        let (name, tip) = match which {
            Endpoint::Start => ("start", "Drag range start"),
            Endpoint::End => ("end", "Drag range end"),
        };
        let hit = Rect::from_min_max(pos2(left, rect.top()), pos2(right, rect.bottom()));
        let resp = ui
            .interact(
                hit,
                id.with(name),
                if enabled {
                    Sense::drag()
                } else {
                    Sense::hover()
                },
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
            .on_hover_text(tip);
        if enabled && resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(press) = ui.input(|i| i.pointer.press_origin()) {
                let (_, other) = which.split(pb.working_range());
                ui.data_mut(|data| {
                    data.insert_temp(
                        resp.id,
                        Grab {
                            other,
                            offset: press.x - x,
                        },
                    )
                });
            }
        }
        if enabled
            && (resp.dragged_by(egui::PointerButton::Primary)
                || resp.drag_stopped_by(egui::PointerButton::Primary))
        {
            if let (Some(pos), Some(grab)) = (
                resp.interact_pointer_pos(),
                ui.data(|data| data.get_temp::<Grab>(resp.id)),
            ) {
                pb.set_working_range(frame_at(pos.x - grab.offset), grab.other, now);
            }
        }
    }

    let (start, end) = pb.working_range();
    let (left, right) = (x_of(start), x_of(end));
    let p = ui.painter_at(rect);
    let y = rect.center().y;
    p.rect_filled(rect, 0.0, theme::NAV_BG);
    p.line_segment(
        [pos2(left, y), pos2(right, y)],
        Stroke::new(1.5_f32, theme::TIMELINE_GREEN),
    );
    for x in [left, right] {
        p.line_segment(
            [pos2(x, rect.top() + 1.0), pos2(x, rect.bottom() - 1.0)],
            Stroke::new(2.0_f32, theme::TIMELINE_GREEN),
        );
    }
}

pub fn fields(ui: &mut Ui, id: Id, pb: &mut Playback, now: f64) -> Option<&'static str> {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Working Range").color(theme::TIMELINE_GREEN));
        ui.label("Start");
        let start = field(ui, id.with("start"), Endpoint::Start, pb, now);
        ui.label("End");
        let end = field(ui, id.with("end"), Endpoint::End, pb, now);
        if ui
            .button("Reset")
            .on_hover_text("Use full take (Shift+[ / Shift+])")
            .clicked()
        {
            pb.set_working_range(pb.first, pb.last, now);
            return None;
        }
        start.or(end)
    })
    .inner
}

fn field(
    ui: &mut Ui,
    id: Id,
    which: Endpoint,
    pb: &mut Playback,
    now: f64,
) -> Option<&'static str> {
    let (value, _) = which.split(pb.working_range());
    let font = egui::TextStyle::Button.resolve(ui.style());
    let digits = |n: i64| n.unsigned_abs().max(1).ilog10() + 1 + u32::from(n < 0);
    let digits = digits(pb.first).max(digits(pb.last));
    let size = vec2((digits as f32 * 8.0 + 12.0).max(40.0), FIELDS_H);
    let mut status = None;
    let focused = ui.memory(|m| m.has_focus(id));
    // A tab or pane can vanish without giving its text field a blur event.
    let pass = ui.ctx().cumulative_pass_nr();
    let previous = ui
        .data_mut(|d| d.remove_temp::<(u64, String)>(id))
        .filter(|(seen, _)| pass.saturating_sub(*seen) <= 1)
        .map(|(_, draft)| draft);
    if !ui.is_enabled() {
        ui.memory_mut(|m| m.surrender_focus(id));
        ui.add_sized(size, Button::new(value.to_string()));
        return None;
    }
    let cancelled = ui.input(|i| i.key_pressed(Key::Escape) || !i.focused);
    if focused {
        let mut draft = previous.unwrap_or_else(|| value.to_string());
        let resp = ui.add_sized(
            size,
            TextEdit::singleline(&mut draft)
                .id(id)
                .font(font)
                .horizontal_align(egui::Align::Center),
        );
        if cancelled {
            resp.surrender_focus();
        } else if resp.lost_focus() {
            status = commit(which, &draft, pb, now);
        } else {
            ui.data_mut(|d| d.insert_temp(id, (pass, draft)));
        }
    } else {
        if let Some(draft) = previous.filter(|_| !cancelled) {
            status = commit(which, &draft, pb, now);
        }
        let (value, _) = which.split(pb.working_range());
        let resp = ui
            .add_sized(size, Button::new(value.to_string()))
            .on_hover_text("Click to type a frame (Enter)");
        if resp.clicked() {
            let draft = value.to_string();
            select_number(ui, id, &draft, &resp);
        }
    }
    status
}

fn commit(which: Endpoint, draft: &str, pb: &mut Playback, now: f64) -> Option<&'static str> {
    match which.parse(draft, pb) {
        Ok((a, b)) => {
            pb.set_working_range(a, b, now);
            None
        }
        Err(reason) => Some(reason),
    }
}

fn select_number(ui: &Ui, id: Id, draft: &str, button: &Response) {
    let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(draft.chars().count()),
        )));
    state.store(ui.ctx(), id);
    let pass = ui.ctx().cumulative_pass_nr();
    ui.data_mut(|d| d.insert_temp(id, (pass, draft.to_owned())));
    button.surrender_focus();
    ui.memory_mut(|m| m.request_focus(id));
}

#[cfg(test)]
#[path = "working_range_tests.rs"]
mod tests;
