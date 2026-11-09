//! Smooth scrolling through the scrollback: a position in lines that moves continuously (with
//! a trackpad's fingers, coasting after they lift, or easing to a mouse wheel's notch), of which
//! the whole lines move the terminal's viewport and the rest offsets the drawn grid.
//!
//! Lines count back into the scrollback: positive is up.

/// No trackpad delta for this long: the fingers have lifted (Linux reports no phases).
const LIFT: f64 = 0.07;
/// Coasting slows by this factor per second (e^-k).
const FRICTION: f64 = 3.0;
/// Slower than this when the fingers lift, in lines a second: no coasting.
const MIN_FLING: f64 = 6.0;
/// Coasting stops below this speed.
const STOP: f64 = 1.5;
const MAX_SPEED: f64 = 800.0;
/// How quickly a wheel notch (or settling onto a line) eases in, per second.
const EASE: f64 = 22.0;

#[derive(Debug, Default)]
pub struct Scroller {
    /// Where the grid is drawn, and where it's heading.
    shown: f64,
    target: f64,
    /// Coasting speed, lines a second.
    speed: f64,
    /// The whole lines already given to the terminal.
    applied: i64,
    touching: bool,
    last_input: Option<f64>,
    /// Where the fingers took it lately, (time, position): their speed when they lift.
    recent: std::collections::VecDeque<(f64, f64)>,
    last_step: Option<f64>,
}

impl Scroller {
    /// A trackpad's delta, at time `t` (seconds).
    pub fn touch(&mut self, t: f64, lines: f64) {
        if !self.touching {
            // Fingers down: catch whatever was coasting.
            self.speed = 0.0;
            self.recent.clear();
            self.recent.push_back((t, self.shown));
        }
        self.touching = true;
        self.last_input = Some(t);
        self.shown += lines;
        self.target = self.shown;
        self.recent.push_back((t, self.shown));
        while self.recent.front().is_some_and(|(at, _)| t - at > 0.1) {
            self.recent.pop_front();
        }
    }

    /// The fingers lifted: coast on if they were moving.
    pub fn lift(&mut self) {
        if !self.touching {
            return;
        }
        self.touching = false;
        // Their speed over the last moments they moved.
        self.speed = match (self.recent.front(), self.recent.back()) {
            (Some(&(t0, p0)), Some(&(t1, p1))) if t1 - t0 > 0.01 => (p1 - p0) / (t1 - t0),
            _ => 0.0,
        };
        self.recent.clear();
        self.speed = if self.speed.abs() < MIN_FLING { 0.0 } else { self.speed.clamp(-MAX_SPEED, MAX_SPEED) };
    }

    /// A mouse wheel's notch (or several): eased in.
    pub fn notch(&mut self, lines: f64) {
        self.touching = false;
        self.speed = 0.0;
        self.target += lines;
    }

    /// Moves on to time `t`; returns the whole lines to move the viewport (positive is back).
    pub fn step(&mut self, t: f64) -> i64 {
        let dt = self.last_step.map_or(0.0, |l| (t - l).clamp(0.0, 0.05));
        self.last_step = Some(t);
        if self.touching && self.last_input.is_some_and(|l| t - l > LIFT) {
            self.lift();
        }
        if !self.touching {
            if self.speed != 0.0 {
                self.shown += self.speed * dt;
                self.target = self.shown;
                self.speed *= (-FRICTION * dt).exp();
                if self.speed.abs() < STOP {
                    self.speed = 0.0;
                    self.target = self.shown.round();
                }
            } else if self.shown != self.target {
                let d = self.target - self.shown;
                self.shown = if d.abs() < 0.002 { self.target } else { self.shown + d * (1.0 - (-EASE * dt).exp()) };
            } else if self.shown.fract() != 0.0 {
                // At rest between two lines: settle onto the nearer.
                self.target = self.shown.round();
            }
        }
        let want = self.shown.floor() as i64;
        let n = want - self.applied;
        self.applied = want;
        n
    }

    /// The viewport moved `short` lines less than `step` asked (an end of the scrollback): stop
    /// there.
    pub fn stop_short(&mut self, short: i64) {
        self.applied -= short;
        self.halt();
    }

    /// Nothing more above (the top of the scrollback): no part of a line to show there.
    pub fn at_top(&mut self) {
        if self.frac() > 0.0 {
            self.halt();
        }
    }

    fn halt(&mut self) {
        self.shown = self.applied as f64;
        self.target = self.shown;
        self.speed = 0.0;
    }

    /// The part of a line the grid is drawn further back than the viewport: 0 to 1.
    pub fn frac(&self) -> f32 {
        (self.shown - self.applied as f64).clamp(0.0, 1.0) as f32
    }

    /// Still moving (or not yet settled on a line): keep frames coming.
    pub fn active(&self) -> bool {
        self.touching || self.speed != 0.0 || self.shown != self.target || self.frac() != 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steps at 120 Hz from `t` for `secs`, returning the lines given to the viewport.
    fn run(s: &mut Scroller, t: &mut f64, secs: f64) -> i64 {
        let mut total = 0;
        for _ in 0..(secs * 120.0) as usize {
            *t += 1.0 / 120.0;
            total += s.step(*t);
        }
        total
    }

    #[test]
    fn small_trackpad_deltas_add_up_and_settle_on_a_line() {
        let (mut s, mut t) = (Scroller::default(), 0.0);
        for _ in 0..10 {
            t += 0.05;
            s.touch(t, 0.13);
            s.step(t);
        }
        // Slow fingers: no coasting, and it settles on a whole line (1.3 -> 1).
        let moved = s.applied;
        run(&mut s, &mut t, 1.0);
        assert_eq!(s.applied, 1, "moved {moved}");
        assert_eq!(s.frac(), 0.0);
        assert!(!s.active());
    }

    #[test]
    fn a_flick_coasts_on_after_the_fingers_lift() {
        let (mut s, mut t) = (Scroller::default(), 0.0);
        for _ in 0..6 {
            t += 1.0 / 120.0;
            s.touch(t, 0.5); // 60 lines a second
            s.step(t);
        }
        let at_lift = s.applied;
        run(&mut s, &mut t, 3.0);
        assert!(s.applied > at_lift + 10, "coasted from {at_lift} to {}", s.applied);
        assert!(!s.active());
        assert_eq!(s.frac(), 0.0);
    }

    #[test]
    fn a_wheel_notch_eases_in_to_its_lines() {
        let (mut s, mut t) = (Scroller::default(), 0.0);
        s.notch(3.0);
        let first = run(&mut s, &mut t, 1.0 / 60.0);
        assert!(first < 3, "eases, not jumps");
        run(&mut s, &mut t, 1.0);
        assert_eq!(s.applied, 3);
        assert!(!s.active());
    }

    #[test]
    fn an_end_of_the_scrollback_stops_it() {
        let (mut s, mut t) = (Scroller::default(), 0.0);
        s.notch(-3.0);
        let n = run(&mut s, &mut t, 1.0 / 30.0);
        s.stop_short(n); // at the bottom already: nothing moved
        run(&mut s, &mut t, 1.0);
        assert_eq!(s.applied, 0);
        assert!(!s.active());
    }
}
