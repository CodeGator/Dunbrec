//! Simulation-side properties of source waveforms: where their breakpoints
//! (sharp corners) are, and how finely they must be sampled.

use dunbrec_core::Waveform;

/// Number of steps per sine period the simulator takes at most.
pub const STEPS_PER_PERIOD: f64 = 50.0;

/// Earliest corner of the waveform strictly after `after`.
pub fn next_breakpoint(w: &Waveform, after: f64) -> Option<f64> {
    // Ignore corners that are only rounding noise away from `after`.
    let eps = 1e-12 * after.abs().max(1e-9);
    let later = |t: f64| t > after + eps;
    match w {
        Waveform::Dc(_) => None,
        Waveform::Sine { delay, .. } => later(*delay).then_some(*delay),
        Waveform::Pulse {
            delay,
            rise,
            fall,
            width,
            period,
            ..
        } => {
            let corners = [0.0, *rise, rise + width, rise + width + fall];
            let first_cycle = if *period > 0.0 && after > *delay {
                ((after - delay) / period).floor().max(0.0)
            } else {
                0.0
            };
            let cycles = if *period > 0.0 { 2 } else { 1 };
            (0..cycles)
                .flat_map(|c| {
                    let base = delay + (first_cycle + c as f64) * period.max(0.0);
                    corners.into_iter().map(move |o| base + o)
                })
                .filter(|&t| later(t))
                .reduce(f64::min)
        }
        Waveform::Pwl(points) => points.iter().map(|p| p.0).find(|&t| later(t)),
    }
}

/// Largest step that still draws the waveform faithfully.
pub fn max_step(w: &Waveform) -> Option<f64> {
    match w {
        Waveform::Sine { frequency, .. } if *frequency > 0.0 => {
            Some(1.0 / (frequency * STEPS_PER_PERIOD))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_breakpoints() {
        let w = Waveform::Pulse {
            v1: 0.0,
            v2: 1.0,
            delay: 1.0,
            rise: 0.1,
            fall: 0.2,
            width: 0.5,
            period: 2.0,
        };
        let mut t = 0.0;
        let mut got = Vec::new();
        while let Some(bp) = next_breakpoint(&w, t) {
            if bp > 5.5 {
                break;
            }
            got.push((bp * 1e6).round() / 1e6);
            t = bp;
        }
        assert_eq!(got, vec![1.0, 1.1, 1.6, 1.8, 3.0, 3.1, 3.6, 3.8, 5.0, 5.1]);
    }

    #[test]
    fn single_pulse_ends() {
        let w = Waveform::Pulse {
            v1: 0.0,
            v2: 1.0,
            delay: 0.0,
            rise: 1e-9,
            fall: 1e-9,
            width: 1e-3,
            period: 0.0,
        };
        assert_eq!(next_breakpoint(&w, 0.0), Some(1e-9));
        assert_eq!(next_breakpoint(&w, 1.0), None);
    }

    #[test]
    fn pwl_and_sine() {
        let w = Waveform::Pwl(vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)]);
        assert_eq!(next_breakpoint(&w, 0.0), Some(1.0));
        assert_eq!(next_breakpoint(&w, 1.0), Some(2.0));
        assert_eq!(next_breakpoint(&w, 2.0), None);
        let s = Waveform::Sine {
            offset: 0.0,
            amplitude: 1.0,
            frequency: 1e3,
            delay: 0.0,
            damping: 0.0,
            phase: 0.0,
        };
        assert_eq!(next_breakpoint(&s, 0.0), None);
        assert_eq!(max_step(&s), Some(1.0 / 50e3));
    }
}
