//! A digital multimeter: averages a signal over a recent window of
//! simulated time, like a real meter's integration time.

use std::collections::VecDeque;

use crate::measure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterMode {
    DcVolts,
    /// AC-coupled true RMS volts.
    AcVolts,
    DcAmps,
    /// AC-coupled true RMS amps.
    AcAmps,
}

impl MeterMode {
    pub fn unit(self) -> &'static str {
        match self {
            MeterMode::DcVolts | MeterMode::AcVolts => "V",
            MeterMode::DcAmps | MeterMode::AcAmps => "A",
        }
    }

    pub fn measures_current(self) -> bool {
        matches!(self, MeterMode::DcAmps | MeterMode::AcAmps)
    }

    pub fn is_ac(self) -> bool {
        matches!(self, MeterMode::AcVolts | MeterMode::AcAmps)
    }

    pub fn label(self) -> &'static str {
        match self {
            MeterMode::DcVolts => "V DC",
            MeterMode::AcVolts => "V AC",
            MeterMode::DcAmps => "A DC",
            MeterMode::AcAmps => "A AC",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Meter {
    pub mode: MeterMode,
    /// Integration time in simulated seconds.
    pub window: f64,
    samples: VecDeque<(f64, f64)>,
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            mode: MeterMode::DcVolts,
            window: 20e-3,
            samples: VecDeque::new(),
        }
    }
}

impl Meter {
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    pub fn push(&mut self, t: f64, value: f64) {
        if self.samples.back().is_some_and(|&(last, _)| t < last) {
            self.clear();
        }
        if !value.is_finite() {
            return;
        }
        self.samples.push_back((t, value));
        // Keep one sample before the window so it is fully covered.
        while self.samples.len() > 2 && self.samples[1].0 < t - self.window {
            self.samples.pop_front();
        }
    }

    /// The meter reading, or `None` with nothing measured yet.
    pub fn reading(&self) -> Option<f64> {
        let samples: Vec<(f64, f64)> = self.samples.iter().copied().collect();
        let s = measure::stats(&samples)?;
        Some(if self.mode.is_ac() { s.ac_rms } else { s.mean })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use std::f64::consts::{SQRT_2, TAU};

    fn feed(m: &mut Meter, f: impl Fn(f64) -> f64, until: f64) {
        let mut t = 0.0;
        while t <= until {
            m.push(t, f(t));
            t += 1e-5;
        }
    }

    #[test]
    fn dc_and_ac_readings() {
        let signal = |t: f64| 2.0 + 3.0 * (TAU * 1e3 * t).sin();
        let mut m = Meter::default();
        feed(&mut m, signal, 0.1);
        assert_relative_eq!(m.reading().unwrap(), 2.0, epsilon = 1e-3);
        m.mode = MeterMode::AcVolts;
        assert_relative_eq!(m.reading().unwrap(), 3.0 / SQRT_2, max_relative = 1e-3);
    }

    #[test]
    fn only_recent_samples_count() {
        let mut m = Meter::default();
        // 5 V for a long time, then 1 V for longer than the window.
        feed(&mut m, |t| if t < 0.5 { 5.0 } else { 1.0 }, 0.6);
        assert_relative_eq!(m.reading().unwrap(), 1.0, epsilon = 1e-9);
    }

    #[test]
    fn single_sample_and_empty() {
        let mut m = Meter::default();
        assert!(m.reading().is_none());
        m.push(0.0, 4.2);
        assert_eq!(m.reading(), Some(4.2));
        m.push(1.0, f64::NAN);
        assert_eq!(m.reading(), Some(4.2), "missing values are skipped");
        assert_eq!(MeterMode::AcAmps.unit(), "A");
        assert!(MeterMode::DcAmps.measures_current());
    }
}
