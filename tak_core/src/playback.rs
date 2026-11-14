//! Frame cursor and real-time playback (FrameRate based). Pure logic, unit-tested.

#[derive(Clone, Debug)]
pub struct Playback {
    pub frame: i64,
    pub first: i64,
    pub last: i64,
    pub rate: f64,
    pub speed: f64,
    pub looping: bool,
    pub playing: bool,
    /// How many times playback went from the last frame back to the first (a loop in `tick`,
    /// or `play` at the last frame). The viewer closes a held comment range then.
    pub wraps: u64,
    anchor_time: f64,
    anchor_frame: i64,
}

impl Playback {
    pub fn new(first: i64, last: i64, rate: f64) -> Self {
        let rate = if rate.is_finite() && rate > 0.0 { rate } else { 100.0 };
        Playback {
            frame: first,
            first,
            last: last.max(first),
            rate,
            speed: 1.0,
            looping: true,
            playing: false,
            wraps: 0,
            anchor_time: 0.0,
            anchor_frame: first,
        }
    }

    pub fn clamp(&self, f: i64) -> i64 {
        f.clamp(self.first, self.last)
    }

    /// Jump to a frame (clamped). Keeps playing if playing.
    pub fn set_frame(&mut self, f: i64, now: f64) {
        self.frame = self.clamp(f);
        self.anchor_time = now;
        self.anchor_frame = self.frame;
    }

    /// Arrow keys: relative step, clamped at both ends, stops playback.
    pub fn step(&mut self, delta: i64, now: f64) {
        self.playing = false;
        self.set_frame(self.frame.saturating_add(delta), now);
    }

    pub fn play(&mut self, now: f64) {
        if self.frame >= self.last {
            self.frame = self.first;
            self.wraps += 1;
        }
        self.playing = true;
        self.anchor_time = now;
        self.anchor_frame = self.frame;
    }

    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// Space bar.
    pub fn toggle(&mut self, now: f64) {
        if self.playing {
            self.pause();
        } else {
            self.play(now);
        }
    }

    pub fn set_speed(&mut self, speed: f64, now: f64) {
        self.speed = speed;
        self.anchor_time = now;
        self.anchor_frame = self.frame;
    }

    /// Advance by wall-clock time. Returns true when the frame changed.
    pub fn tick(&mut self, now: f64) -> bool {
        if !self.playing {
            return false;
        }
        let elapsed = (now - self.anchor_time).max(0.0);
        let mut target = self.anchor_frame + (elapsed * self.rate * self.speed).floor() as i64;
        if target > self.last {
            if self.looping {
                let span = self.last - self.first + 1;
                target = self.first + (target - self.first).rem_euclid(span);
                self.anchor_time = now;
                self.anchor_frame = target;
                self.wraps += 1;
            } else {
                target = self.last;
                self.playing = false;
            }
        }
        let changed = target != self.frame;
        self.frame = target;
        changed
    }

    /// Seconds from the first frame.
    pub fn time_of(&self, f: i64) -> f64 {
        (f - self.first) as f64 / self.rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_clamp_at_both_ends() {
        let mut p = Playback::new(0, 100, 100.0);
        p.step(-1, 0.0);
        assert_eq!(p.frame, 0);
        p.step(10, 0.0);
        assert_eq!(p.frame, 10);
        p.step(1, 0.0);
        assert_eq!(p.frame, 11);
        p.step(-10, 0.0);
        assert_eq!(p.frame, 1);
        p.step(-10, 0.0);
        assert_eq!(p.frame, 0);
        p.set_frame(95, 0.0);
        p.step(10, 0.0);
        assert_eq!(p.frame, 100);
        p.step(1, 0.0);
        assert_eq!(p.frame, 100);
    }

    #[test]
    fn step_stops_playback() {
        let mut p = Playback::new(0, 100, 100.0);
        p.toggle(0.0);
        assert!(p.playing);
        p.step(1, 0.1);
        assert!(!p.playing);
    }

    #[test]
    fn real_time_tick() {
        let mut p = Playback::new(0, 1000, 100.0);
        p.toggle(10.0);
        assert!(p.tick(10.5));
        assert_eq!(p.frame, 50);
        p.set_speed(2.0, 10.5);
        p.tick(11.0);
        assert_eq!(p.frame, 150);
        p.toggle(11.0);
        assert!(!p.tick(20.0));
        assert_eq!(p.frame, 150);
    }

    #[test]
    fn end_behaviour() {
        let mut p = Playback::new(0, 99, 100.0);
        p.looping = false;
        p.toggle(0.0);
        p.tick(5.0);
        assert_eq!(p.frame, 99);
        assert!(!p.playing);
        // Space at the end restarts from the first frame.
        p.toggle(5.0);
        assert_eq!(p.frame, 0);
        p.looping = true;
        p.tick(6.2); // 120 frames later -> wraps to 20
        assert_eq!(p.frame, 20);
        assert!(p.playing);
    }

    #[test]
    fn going_from_the_last_frame_back_to_the_first_is_counted() {
        let mut p = Playback::new(0, 99, 100.0);
        p.toggle(0.0);
        p.tick(0.5);
        assert_eq!((p.frame, p.wraps), (50, 0));
        p.tick(1.2); // 120 frames from 0 -> wraps to 20
        assert_eq!((p.frame, p.wraps), (20, 1));
        // Steps, Home and seeking back are not wraps.
        p.step(i64::MIN / 2, 1.2);
        p.set_frame(99, 1.2);
        p.set_frame(3, 1.2);
        assert_eq!(p.wraps, 1);
        // Space at the last frame starts again from the first: a wrap too.
        p.set_frame(99, 1.2);
        p.toggle(1.3);
        assert_eq!((p.frame, p.wraps), (0, 2));
        // Space elsewhere is not.
        p.toggle(1.3);
        p.set_frame(40, 1.3);
        p.toggle(1.3);
        assert_eq!(p.wraps, 2);
    }

    #[test]
    fn nonzero_first_frame_time() {
        let p = Playback::new(200, 300, 120.0);
        assert_eq!(p.frame, 200);
        assert!((p.time_of(260) - 0.5).abs() < 1e-12);
    }
}
