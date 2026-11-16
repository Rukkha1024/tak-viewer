
use eframe::egui::{CursorIcon, PointerButton, Pos2, Response, Ui};

/// Frames per point of mouse movement.
pub const PER_PT: f32 = 1.0;
/// Frames per point while Shift is held.
pub const PER_PT_SHIFT: f32 = 10.0;
/// Mouse cursor over a number that can be dragged (the arrow points up-right and down-left,
/// the two ways the number grows and shrinks).
pub const CURSOR: CursorIcon = CursorIcon::ResizeNeSw;

/// One drag going on: the value at its start and the movement so far (in frames).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    base: i64,
    acc: f32,
    last: Pos2,
}

impl Drag {
    /// A drag of `base` pressed at `origin`.
    pub fn new(base: i64, origin: Pos2) -> Drag {
        Drag { base, acc: 0.0, last: origin }
    }

    /// The pointer is at `pos` now: the value, inside `lo..=hi`. Each step of the pointer counts
    /// with the Shift of its frame, so pressing Shift halfway does not jump.
    pub fn to(&mut self, pos: Pos2, shift: bool, lo: i64, hi: i64) -> i64 {
        let hi = hi.max(lo);
        let d = pos - self.last;
        self.last = pos;
        self.acc += (d.x - d.y) * if shift { PER_PT_SHIFT } else { PER_PT };
        let free = self.base.saturating_add(self.acc.round() as i64);
        let v = free.clamp(lo, hi);
        if v != free {
            // Stopped at an end: moving back changes the value again at once.
            self.acc = (v - self.base) as f32;
        }
        v
    }
}

/// The primary-button drag on `resp` (a widget that senses clicks and drags) changes `value`:
/// the new value on each frame it is dragged, else None. `value` is read when the drag starts.
pub fn drag(ui: &Ui, resp: &Response, value: i64, lo: i64, hi: i64) -> Option<i64> {
    let key = resp.id.with("scrub");
    if resp.drag_started_by(PointerButton::Primary) {
        let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())?;
        ui.data_mut(|d| d.insert_temp(key, Drag::new(value.clamp(lo, hi.max(lo)), origin)));
    }
    if !resp.dragged_by(PointerButton::Primary) {
        if resp.drag_stopped() {
            ui.data_mut(|d| d.remove::<Drag>(key));
        }
        return None;
    }
    let pos = resp.interact_pointer_pos()?;
    let mut drag: Drag = ui.data(|d| d.get_temp(key))?;
    let shift = ui.input(|i| i.modifiers.shift);
    let v = drag.to(pos, shift, lo, hi);
    ui.data_mut(|d| d.insert_temp(key, drag));
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    #[test]
    fn up_and_right_add_down_and_left_take_shift_is_ten_and_the_ends_hold() {
        let at = |x: f32, y: f32| pos2(100.0 + x, 100.0 + y);
        let mut d = Drag::new(500, at(0.0, 0.0));
        assert_eq!(d.to(at(0.0, -40.0), false, 0, 2999), 540, "40 pt up = +40");
        assert_eq!(d.to(at(5.0, -40.0), false, 0, 2999), 545, "5 pt right = +5");
        assert_eq!(d.to(at(5.0, 0.0), false, 0, 2999), 505, "40 pt down = −40");
        assert_eq!(d.to(at(-5.0, 0.0), false, 0, 2999), 495, "10 pt left = −10");
        assert_eq!(d.to(at(-5.0, -3.0), true, 0, 2999), 525, "Shift: 3 pt up = +30");
        assert_eq!(d.to(at(-5.0, -3.4), false, 0, 2999), 525, "part of a point: kept, not lost");
        assert_eq!(d.to(at(-5.0, -4.0), false, 0, 2999), 526);
        // The ends: the value stops there and comes back at once.
        assert_eq!(d.to(at(-5.0, 4000.0), false, 0, 2999), 0);
        assert_eq!(d.to(at(-5.0, 3990.0), false, 0, 2999), 10, "10 pt back up from the first frame");
        assert_eq!(d.to(at(-5.0, -9000.0), true, 0, 2999), 2999);
        assert_eq!(d.to(at(-5.0, -8999.0), false, 0, 2999), 2998);
    }
}
