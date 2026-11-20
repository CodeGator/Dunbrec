//! Waveform measurements on `(time, value)` samples. Samples from the
//! simulator are unevenly spaced, so averages are time-weighted
//! (trapezoidal integration) rather than simple means of the samples.

/// Summary statistics of a waveform segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub min: f64,
    pub max: f64,
    /// Time-weighted mean (the DC value).
    pub mean: f64,
    /// True RMS, including the DC component.
    pub rms: f64,
    /// RMS of the AC component only (what an AC-coupled meter shows).
    pub ac_rms: f64,
}

impl Stats {
    pub fn peak_to_peak(&self) -> f64 {
        self.max - self.min
    }
}

/// Statistics of `samples` (time-ordered). `None` if there are no finite
/// samples. A single sample (or zero duration) gives that value as the mean.
pub fn stats(samples: &[(f64, f64)]) -> Option<Stats> {
    let finite: Vec<(f64, f64)> = samples
        .iter()
        .copied()
        .filter(|(t, v)| t.is_finite() && v.is_finite())
        .collect();
    let first = *finite.first()?;
    let (mut min, mut max) = (first.1, first.1);
    let (mut area, mut area_sq, mut duration) = (0.0, 0.0, 0.0);
    for w in finite.windows(2) {
        let ((t0, v0), (t1, v1)) = (w[0], w[1]);
        let dt = t1 - t0;
        if dt <= 0.0 {
            continue;
        }
        area += 0.5 * (v0 + v1) * dt;
        // Exact integral of the square of a linear segment.
        area_sq += (v0 * v0 + v0 * v1 + v1 * v1) / 3.0 * dt;
        duration += dt;
    }
    for &(_, v) in &finite {
        min = min.min(v);
        max = max.max(v);
    }
    let last = finite[finite.len() - 1].1;
    let (mean, mean_sq) = if duration > 0.0 {
        (area / duration, area_sq / duration)
    } else {
        (last, last * last)
    };
    Some(Stats {
        min,
        max,
        mean,
        rms: mean_sq.max(0.0).sqrt(),
        ac_rms: (mean_sq - mean * mean).max(0.0).sqrt(),
    })
}

/// Times at which `samples` cross `level` going upwards, interpolated
/// between samples. Hysteresis of `hysteresis` (absolute) stops noise near
/// the level counting as many crossings: the signal must first fall below
/// `level - hysteresis`.
pub fn rising_crossings(samples: &[(f64, f64)], level: f64, hysteresis: f64) -> Vec<f64> {
    let mut out = Vec::new();
    let mut armed = false;
    for w in samples.windows(2) {
        let ((t0, v0), (t1, v1)) = (w[0], w[1]);
        if !(v0.is_finite() && v1.is_finite()) {
            continue;
        }
        if v0 < level - hysteresis {
            armed = true;
        }
        if armed && v0 < level && v1 >= level {
            let frac = if v1 != v0 {
                (level - v0) / (v1 - v0)
            } else {
                0.0
            };
            out.push(t0 + frac * (t1 - t0));
            armed = false;
        }
    }
    out
}

/// Frequency from the spacing of rising crossings of the waveform's mean,
/// with 10 % peak-to-peak hysteresis. Needs at least one full cycle.
pub fn frequency(samples: &[(f64, f64)]) -> Option<f64> {
    let s = stats(samples)?;
    let pp = s.peak_to_peak();
    if pp <= 0.0 {
        return None;
    }
    let crossings = rising_crossings(samples, s.mean, 0.1 * pp);
    if crossings.len() < 2 {
        return None;
    }
    let span = crossings[crossings.len() - 1] - crossings[0];
    (span > 0.0).then(|| (crossings.len() - 1) as f64 / span)
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use std::f64::consts::{SQRT_2, TAU};

    /// Unevenly sampled sine (as a simulator would produce).
    pub fn sine(offset: f64, amplitude: f64, freq: f64, cycles: f64) -> Vec<(f64, f64)> {
        let mut t = 0.0;
        let mut out = Vec::new();
        let end = cycles / freq;
        let mut k = 0usize;
        while t <= end {
            out.push((t, offset + amplitude * (TAU * freq * t).sin()));
            // Steps alternate between two sizes.
            t += if k.is_multiple_of(2) { 0.7 } else { 1.3 } / (100.0 * freq);
            k += 1;
        }
        out
    }

    #[test]
    fn sine_statistics() {
        let s = stats(&sine(2.0, 3.0, 1e3, 10.0)).unwrap();
        assert_relative_eq!(s.mean, 2.0, epsilon = 2e-2);
        assert_relative_eq!(s.ac_rms, 3.0 / SQRT_2, max_relative = 1e-2);
        assert_relative_eq!(s.rms, (4.0f64 + 4.5).sqrt(), max_relative = 1e-2);
        assert_relative_eq!(s.peak_to_peak(), 6.0, max_relative = 1e-2);
    }

    #[test]
    fn dc_and_degenerate_inputs() {
        let s = stats(&[(0.0, 5.0)]).unwrap();
        assert_eq!((s.mean, s.ac_rms, s.rms), (5.0, 0.0, 5.0));
        let s = stats(&[(0.0, 1.0), (1.0, 1.0), (3.0, 1.0)]).unwrap();
        assert_relative_eq!(s.mean, 1.0);
        assert_relative_eq!(s.ac_rms, 0.0, epsilon = 1e-12);
        assert!(stats(&[]).is_none());
        assert!(stats(&[(0.0, f64::NAN)]).is_none());
    }

    #[test]
    fn time_weighting() {
        // 0 V for 9 s then 10 V for 1 s (with sharp edges): mean ≈ 1 V,
        // not the 5 V a sample average would give.
        let s = stats(&[(0.0, 0.0), (9.0, 0.0), (9.0, 10.0), (10.0, 10.0)]).unwrap();
        assert_relative_eq!(s.mean, 1.0, epsilon = 1e-12);
    }

    #[test]
    fn frequency_of_a_sine() {
        let f = frequency(&sine(0.5, 1.0, 1e3, 12.0)).unwrap();
        assert_relative_eq!(f, 1e3, max_relative = 1e-3);
        assert!(
            frequency(&sine(0.0, 1.0, 1e3, 0.8)).is_none(),
            "under one cycle"
        );
        assert!(frequency(&[(0.0, 1.0), (1.0, 1.0)]).is_none(), "flat line");
    }

    #[test]
    fn hysteresis_rejects_noise() {
        // Wiggles around the level without first dropping below it by the
        // hysteresis count only once.
        let samples = [
            (0.0, -1.0),
            (1.0, 0.1),
            (2.0, -0.05),
            (3.0, 0.1),
            (4.0, 0.2),
        ];
        assert_eq!(rising_crossings(&samples, 0.0, 0.5).len(), 1);
        assert_eq!(rising_crossings(&samples, 0.0, 0.0).len(), 2);
    }
}
