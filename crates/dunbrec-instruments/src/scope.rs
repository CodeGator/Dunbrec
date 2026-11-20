//! A four-channel oscilloscope acquisition engine.
//!
//! Samples arrive in time order ([`Scope::push`]). Each call to
//! [`Scope::update`] picks what the screen should show, like a real scope:
//!
//! * **Auto**: the latest triggered sweep; if the signal has not triggered
//!   recently, a free-running ("rolling") view of the latest window.
//! * **Normal**: the latest triggered sweep; without a trigger the last one
//!   stays on screen.
//! * **Single**: the first trigger after [`Scope::arm`] is captured and
//!   frozen until armed again.
//!
//! The screen is 10 divisions wide; the trigger point sits one division
//! from the left edge.

use std::collections::VecDeque;

use crate::measure::{self, Stats};

pub const CHANNELS: usize = 4;
pub const DIVISIONS_X: f64 = 10.0;
pub const DIVISIONS_Y: f64 = 8.0;
/// Divisions of signal shown before the trigger point.
pub const PRE_TRIGGER_DIVISIONS: f64 = 1.0;
/// Samples older than this many screen widths are discarded.
const HISTORY_WINDOWS: f64 = 4.0;
/// Hard cap on stored samples, whatever the timebase.
const MAX_SAMPLES: usize = 400_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub enabled: bool,
    pub volts_per_div: f64,
    /// Vertical position of 0 V, in divisions from the screen centre.
    pub offset_div: f64,
    /// AC coupling: the signal's average is removed for display and
    /// triggering, so a small signal on a large DC level can be examined.
    pub ac: bool,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            enabled: true,
            volts_per_div: 1.0,
            offset_div: 0.0,
            ac: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerEdge {
    Rising,
    Falling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMode {
    Auto,
    Normal,
    Single,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    /// Channel index (0-based) the trigger watches.
    pub source: usize,
    pub edge: TriggerEdge,
    pub level: f64,
    pub mode: TriggerMode,
}

impl Default for Trigger {
    fn default() -> Self {
        Self {
            source: 0,
            edge: TriggerEdge::Rising,
            level: 0.0,
            mode: TriggerMode::Auto,
        }
    }
}

/// One acquisition point: time and the value of each channel (NaN when a
/// channel has no signal).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub t: f64,
    pub v: [f64; CHANNELS],
}

/// What the screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Sweep {
    /// Time at the left edge of the screen.
    pub start: f64,
    /// Time of the trigger, `None` for a free-running (untriggered) view.
    pub trigger_time: Option<f64>,
    pub points: Vec<Point>,
}

impl Sweep {
    /// `(time, value)` of one channel, skipping points without a signal.
    pub fn channel(&self, ch: usize) -> Vec<(f64, f64)> {
        self.points
            .iter()
            .filter(|p| p.v[ch].is_finite())
            .map(|p| (p.t, p.v[ch]))
            .collect()
    }

    pub fn stats(&self, ch: usize) -> Option<Stats> {
        measure::stats(&self.channel(ch))
    }

    pub fn frequency(&self, ch: usize) -> Option<f64> {
        measure::frequency(&self.channel(ch))
    }
}

/// What the scope is doing, for the status display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Showing a triggered sweep.
    Triggered,
    /// Auto mode without a trigger: showing a rolling view.
    Auto,
    /// Normal or armed Single mode, waiting for a trigger.
    Waiting,
    /// Single mode, captured and frozen.
    Stopped,
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub channels: [Channel; CHANNELS],
    /// Seconds per horizontal division.
    pub time_per_div: f64,
    pub trigger: Trigger,
    samples: VecDeque<Point>,
    sweep: Option<Sweep>,
    status: Status,
    /// Single mode: armed at this time, waiting for the first trigger.
    armed_at: Option<f64>,
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            channels: Default::default(),
            time_per_div: 1e-3,
            trigger: Trigger::default(),
            samples: VecDeque::new(),
            sweep: None,
            status: Status::Auto,
            armed_at: Some(f64::NEG_INFINITY),
        }
    }
}

impl Scope {
    pub fn window(&self) -> f64 {
        self.time_per_div * DIVISIONS_X
    }

    fn pre_trigger(&self) -> f64 {
        self.time_per_div * PRE_TRIGGER_DIVISIONS
    }

    pub fn sweep(&self) -> Option<&Sweep> {
        self.sweep.as_ref()
    }

    pub fn status(&self) -> Status {
        self.status
    }

    /// Forget all samples and the displayed sweep (e.g. on restart).
    pub fn clear(&mut self) {
        self.samples.clear();
        self.sweep = None;
        self.status = Status::Auto;
        if self.trigger.mode == TriggerMode::Single {
            self.armed_at = Some(f64::NEG_INFINITY);
        }
    }

    /// Arm Single mode: capture the next trigger after the latest sample.
    pub fn arm(&mut self) {
        let now = self.samples.back().map_or(f64::NEG_INFINITY, |p| p.t);
        self.armed_at = Some(now);
        self.status = Status::Waiting;
    }

    pub fn push(&mut self, t: f64, v: [f64; CHANNELS]) {
        if self.samples.back().is_some_and(|p| t < p.t) {
            // Time went backwards: a new run. Start afresh.
            self.clear();
        }
        self.samples.push_back(Point { t, v });
        let keep_after = t - HISTORY_WINDOWS * self.window();
        while self.samples.len() > MAX_SAMPLES
            || self.samples.front().is_some_and(|p| p.t < keep_after)
        {
            self.samples.pop_front();
        }
    }

    /// Average of channel `ch` over the stored samples (time-weighted).
    fn buffer_mean(&self, ch: usize) -> Option<f64> {
        let samples: Vec<(f64, f64)> = self.samples.iter().map(|p| (p.t, p.v[ch])).collect();
        measure::stats(&samples).map(|s| s.mean)
    }

    /// The trigger level as an absolute voltage: for an AC-coupled source
    /// it is relative to the signal's average.
    pub fn trigger_level_absolute(&self) -> f64 {
        let ch = self.trigger.source.min(CHANNELS - 1);
        let base = if self.channels[ch].ac {
            self.buffer_mean(ch).unwrap_or(0.0)
        } else {
            0.0
        };
        self.trigger.level + base
    }

    /// Value subtracted from channel `ch` for display (its average when AC
    /// coupled).
    pub fn display_offset(&self, ch: usize) -> f64 {
        if !self.channels[ch].ac {
            return 0.0;
        }
        self.sweep
            .as_ref()
            .and_then(|s| s.stats(ch))
            .map_or(0.0, |s| s.mean)
    }

    /// Times of trigger crossings in `[from, to]`.
    fn crossings(&self, from: f64, to: f64) -> Vec<f64> {
        let ch = self.trigger.source.min(CHANNELS - 1);
        let level = self.trigger_level_absolute();
        let mut out = Vec::new();
        let mut prev: Option<Point> = None;
        for p in &self.samples {
            if let Some(q) = prev {
                let (a, b) = (q.v[ch], p.v[ch]);
                let hit = match self.trigger.edge {
                    TriggerEdge::Rising => a < level && b >= level,
                    TriggerEdge::Falling => a > level && b <= level,
                };
                if hit && a.is_finite() && b.is_finite() {
                    let tc = q.t + (level - a) / (b - a) * (p.t - q.t);
                    if tc >= from && tc <= to {
                        out.push(tc);
                    }
                }
            }
            prev = Some(*p);
        }
        out
    }

    fn capture(&self, start: f64, trigger_time: Option<f64>) -> Sweep {
        let end = start + self.window();
        // Include one point either side so traces reach the screen edges.
        let first = self
            .samples
            .partition_point(|p| p.t < start)
            .saturating_sub(1);
        let last = (self.samples.partition_point(|p| p.t <= end) + 1).min(self.samples.len());
        Sweep {
            start,
            trigger_time,
            points: self.samples.range(first..last).copied().collect(),
        }
    }

    /// Decide what to show, based on the samples received so far.
    pub fn update(&mut self) {
        let (Some(oldest), Some(newest)) = (self.samples.front(), self.samples.back()) else {
            return;
        };
        let (oldest, newest) = (oldest.t, newest.t);
        let pre = self.pre_trigger();
        let post = self.window() - pre;
        // A trigger can be shown once a full screen after it has arrived.
        let latest_usable = newest - post;

        match self.trigger.mode {
            TriggerMode::Single => {
                let Some(armed) = self.armed_at else {
                    self.status = Status::Stopped;
                    return;
                };
                let first = self
                    .crossings(armed.max(oldest + pre), latest_usable)
                    .into_iter()
                    .next();
                match first {
                    Some(tc) => {
                        self.sweep = Some(self.capture(tc - pre, Some(tc)));
                        self.armed_at = None;
                        self.status = Status::Stopped;
                    }
                    None => self.status = Status::Waiting,
                }
            }
            mode => {
                let latest = self.crossings(oldest + pre, latest_usable).pop();
                match latest {
                    Some(tc) => {
                        self.sweep = Some(self.capture(tc - pre, Some(tc)));
                        self.status = Status::Triggered;
                    }
                    None if mode == TriggerMode::Auto => {
                        self.sweep = Some(self.capture(newest - self.window(), None));
                        self.status = Status::Auto;
                    }
                    None => self.status = Status::Waiting,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use std::f64::consts::TAU;

    fn feed_sine(scope: &mut Scope, from: f64, to: f64, freq: f64, amplitude: f64) {
        let dt = 1.0 / (freq * 200.0);
        let mut t = from;
        while t <= to {
            let v = amplitude * (TAU * freq * t).sin();
            scope.push(t, [v, -v, f64::NAN, f64::NAN]);
            t += dt;
        }
    }

    #[test]
    fn triggers_on_rising_edge_one_division_in() {
        let mut s = Scope {
            time_per_div: 0.2e-3,
            ..Default::default()
        };
        feed_sine(&mut s, 0.0, 10e-3, 1e3, 1.0);
        s.update();
        assert_eq!(s.status(), Status::Triggered);
        let sweep = s.sweep().unwrap();
        let tc = sweep.trigger_time.unwrap();
        // Rising zero crossings of a 1 kHz sine are at whole milliseconds.
        assert_relative_eq!(tc * 1e3, (tc * 1e3).round(), epsilon = 1e-6);
        assert_relative_eq!(sweep.start, tc - 0.2e-3, epsilon = 1e-12);
        // A full screen after the trigger is available.
        assert!(sweep.points.last().unwrap().t >= sweep.start + s.window());
        // Measurements on the displayed sweep: two full cycles at 1 kHz.
        let st = sweep.stats(0).unwrap();
        assert_relative_eq!(st.peak_to_peak(), 2.0, max_relative = 1e-3);
        assert_relative_eq!(sweep.frequency(0).unwrap(), 1e3, max_relative = 1e-2);
        // Channel 3 has no signal.
        assert!(sweep.stats(2).is_none());
    }

    #[test]
    fn falling_edge_and_level() {
        let mut s = Scope {
            time_per_div: 0.2e-3,
            trigger: Trigger {
                edge: TriggerEdge::Falling,
                level: 0.5,
                ..Default::default()
            },
            ..Default::default()
        };
        feed_sine(&mut s, 0.0, 5e-3, 1e3, 1.0);
        s.update();
        let tc = s.sweep().unwrap().trigger_time.unwrap();
        // sin(2π f t) = 0.5 falling at phase 150°.
        let phase = (tc * 1e3).fract() * 360.0;
        assert_relative_eq!(phase, 150.0, epsilon = 0.5);
    }

    #[test]
    fn auto_rolls_and_normal_waits_without_a_trigger() {
        let dc = |s: &mut Scope| {
            for k in 0..=1000 {
                s.push(k as f64 * 1e-5, [3.0, f64::NAN, f64::NAN, f64::NAN]);
            }
        };
        let mut auto = Scope::default();
        dc(&mut auto);
        auto.update();
        assert_eq!(auto.status(), Status::Auto);
        let sweep = auto.sweep().unwrap();
        assert!(sweep.trigger_time.is_none());
        assert_relative_eq!(sweep.start, 1e-2 - auto.window(), epsilon = 1e-9);

        let mut normal = Scope {
            trigger: Trigger {
                mode: TriggerMode::Normal,
                ..Default::default()
            },
            ..Default::default()
        };
        dc(&mut normal);
        normal.update();
        assert_eq!(normal.status(), Status::Waiting);
        assert!(normal.sweep().is_none());
    }

    #[test]
    fn single_shot_freezes_until_rearmed() {
        let mut s = Scope {
            time_per_div: 0.2e-3,
            trigger: Trigger {
                mode: TriggerMode::Single,
                ..Default::default()
            },
            ..Default::default()
        };
        feed_sine(&mut s, 0.0, 5e-3, 1e3, 1.0);
        s.update();
        assert_eq!(s.status(), Status::Stopped);
        let first = s.sweep().unwrap().trigger_time.unwrap();
        // The first rising crossing after the pre-trigger time.
        assert_relative_eq!(first, 1e-3, epsilon = 1e-6);

        feed_sine(&mut s, 5e-3 + 5e-6, 10e-3, 1e3, 1.0);
        s.update();
        assert_eq!(s.sweep().unwrap().trigger_time.unwrap(), first, "frozen");

        s.arm();
        assert_eq!(s.status(), Status::Waiting);
        feed_sine(&mut s, 10e-3 + 5e-6, 15e-3, 1e3, 1.0);
        s.update();
        let second = s.sweep().unwrap().trigger_time.unwrap();
        assert!(second > 10e-3, "{second}");
    }

    #[test]
    fn ac_coupling_triggers_on_a_signal_riding_on_dc() {
        let mut s = Scope {
            time_per_div: 0.2e-3,
            ..Default::default()
        };
        // 10 mV sine on 5 V: a 0 V DC trigger level never crosses.
        let dt = 1.0 / (1e3 * 200.0);
        let mut t = 0.0;
        while t <= 10e-3 {
            s.push(
                t,
                [
                    5.0 + 0.01 * (TAU * 1e3 * t).sin(),
                    f64::NAN,
                    f64::NAN,
                    f64::NAN,
                ],
            );
            t += dt;
        }
        s.update();
        assert_eq!(s.status(), Status::Auto);
        s.channels[0].ac = true;
        s.update();
        assert_eq!(s.status(), Status::Triggered);
        assert_relative_eq!(s.trigger_level_absolute(), 5.0, epsilon = 1e-3);
        assert_relative_eq!(s.display_offset(0), 5.0, epsilon = 1e-3);
        assert_eq!(s.display_offset(1), 0.0);
    }

    #[test]
    fn old_samples_are_discarded_and_restart_clears() {
        let mut s = Scope {
            time_per_div: 1e-4,
            ..Default::default()
        };
        feed_sine(&mut s, 0.0, 0.1, 1e3, 1.0);
        let span = s.samples.back().unwrap().t - s.samples.front().unwrap().t;
        assert!(span <= HISTORY_WINDOWS * s.window() + 1e-9);
        // Time jumping backwards means a new run.
        s.update();
        s.push(0.0, [0.0; CHANNELS]);
        assert_eq!(s.samples.len(), 1);
        assert!(s.sweep().is_none());
    }
}
