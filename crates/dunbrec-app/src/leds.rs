//! How bright each LED looks.
//!
//! An LED's light follows its current. A program often switches one on and off
//! faster than the eye can see, to dim it or to share wires between digits,
//! so what is shown is the current averaged over a short stretch of simulated
//! time.

use std::collections::{HashMap, HashSet, VecDeque};

use dunbrec_core::{ComponentId, ComponentKind, LedColour, Netlist};
use dunbrec_sim::Sample;
use egui::Color32;

/// The current (A) at which an LED is drawn at full brightness.
pub const FULL_CURRENT: f64 = 0.020;
/// More than this, on average, is too much for an ordinary LED.
pub const MOST_CURRENT: f64 = 0.030;
/// Less than this gives no light worth showing.
const LEAST_CURRENT: f64 = 0.000_05;

/// What the eye averages over, in real seconds.
const EYE: f64 = 0.020;
/// The longest stretch of simulated time ever averaged, at high speeds.
const LONGEST_WINDOW: f64 = 0.040;

/// What lights up on the schematic.
#[derive(Debug, Default)]
pub struct Lights {
    /// Logic LEDs whose input is high.
    pub logic: HashSet<ComponentId>,
    /// The current through each LED part, forwards positive (A): one for a
    /// single LED, one per segment for a display.
    pub leds: HashMap<ComponentId, Vec<f64>>,
}

/// How brightly an LED carrying `current` shines, from 0 to 1. The eye sees
/// small amounts of light well, so brightness rises quickly at first.
pub fn brightness(current: f64) -> f32 {
    if current.is_nan() || current < LEAST_CURRENT {
        return 0.0;
    }
    (current / FULL_CURRENT).min(1.0).sqrt().max(0.15) as f32
}

/// The colour of an LED's light.
pub fn light(colour: LedColour) -> Color32 {
    match colour {
        LedColour::Red => Color32::from_rgb(255, 45, 35),
        LedColour::Yellow => Color32::from_rgb(255, 196, 0),
        LedColour::Green => Color32::from_rgb(30, 205, 70),
        LedColour::Blue => Color32::from_rgb(50, 120, 255),
        // Warm, so that it shows on a white page.
        LedColour::White => Color32::from_rgb(255, 232, 150),
    }
}

/// The recent current of one LED.
#[derive(Debug, Default)]
struct Trace {
    /// (time, charge passed since the start), oldest first.
    past: VecDeque<(f64, f64)>,
    /// The latest sample.
    time: f64,
    charge: f64,
    current: f64,
}

impl Trace {
    fn add(&mut self, time: f64, current: f64, window: f64) {
        if self.past.is_empty() || time < self.time {
            *self = Trace {
                past: VecDeque::from([(time, 0.0)]),
                time,
                charge: 0.0,
                current,
            };
            return;
        }
        self.charge += (self.current + current) / 2.0 * (time - self.time);
        self.time = time;
        self.current = current;
        // A point every hundredth of the window is plenty.
        if self
            .past
            .back()
            .is_none_or(|p| time - p.0 >= window / 100.0)
        {
            self.past.push_back((time, self.charge));
        }
        // Keep one point at or just before the start of the window.
        while self.past.len() > 1 && self.past[1].0 <= time - window {
            self.past.pop_front();
        }
    }

    fn average(&self) -> f64 {
        match self.past.front() {
            Some(&(since, charge)) if self.time > since => {
                (self.charge - charge) / (self.time - since)
            }
            _ => self.current,
        }
    }
}

/// One LED being followed.
#[derive(Debug)]
struct Lamp {
    /// Its part: the netlist element and component.
    element: usize,
    component: ComponentId,
    /// Which LED of a display; `None` for a single LED.
    segment: Option<usize>,
    trace: Trace,
}

/// Follows the current of every LED of a running circuit.
#[derive(Debug, Default)]
pub struct LedMeter {
    /// The LEDs of each part together, in segment order.
    lamps: Vec<Lamp>,
}

impl LedMeter {
    /// Start afresh for a circuit.
    pub fn new(netlist: &Netlist) -> Self {
        let mut lamps = Vec::new();
        for (element, e) in netlist.elements.iter().enumerate() {
            let lamp = |segment| Lamp {
                element,
                component: e.component,
                segment,
                trace: Trace::default(),
            };
            if matches!(e.kind, ComponentKind::Led { .. }) {
                lamps.push(lamp(None));
            }
            if let Some((_, segments)) = e.kind.led_segments() {
                lamps.extend((0..segments.len()).map(|k| lamp(Some(k))));
            }
        }
        Self { lamps }
    }

    /// Take in one time point. `speed` is in simulated seconds per real
    /// second: in slow motion the stretch that is averaged shrinks with it,
    /// so that changes can be watched.
    pub fn add(&mut self, sample: &Sample, speed: f64) {
        let window = (EYE * speed).clamp(0.0, LONGEST_WINDOW);
        for lamp in &mut self.lamps {
            let current = match lamp.segment {
                None => sample.currents.get(lamp.element),
                Some(k) => sample
                    .lamps
                    .iter()
                    .find(|(element, _)| *element == lamp.element)
                    .and_then(|(_, currents)| currents.get(k)),
            };
            if let Some(&current) = current {
                lamp.trace.add(sample.time, current, window);
            }
        }
    }

    /// The current each LED looks to be carrying, by part.
    pub fn currents(&self) -> HashMap<ComponentId, Vec<f64>> {
        let mut parts: HashMap<ComponentId, Vec<f64>> = HashMap::new();
        for lamp in self.lamps.iter().filter(|l| !l.trace.past.is_empty()) {
            let seen = lamp.trace.average();
            parts.entry(lamp.component).or_default().push(seen);
        }
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::{ComponentKind as K, NetlistBuilder};

    fn one_led() -> (LedMeter, ComponentId) {
        let netlist = NetlistBuilder::new()
            .element("R1", K::Resistor { resistance: 1.0 }, &["a", "0"])
            .element(
                "LED1",
                K::Led {
                    colour: LedColour::Red,
                },
                &["a", "0"],
            )
            .build();
        let id = netlist.elements[1].component;
        (LedMeter::new(&netlist), id)
    }

    /// A time point with these currents, by netlist element.
    fn at(time: f64, currents: &[f64]) -> Sample {
        Sample {
            time,
            voltages: Vec::new(),
            currents: currents.to_vec(),
            lamps: Vec::new(),
        }
    }

    /// Switch the LED between 20 mA and nothing, `on` of each `period`, for a
    /// tenth of a second, and return the greatest and least it looked once
    /// settled.
    fn switched(period: f64, on: f64, speed: f64) -> (f64, f64) {
        let (mut meter, id) = one_led();
        let (mut low, mut high) = (f64::MAX, f64::MIN);
        let step = period / 200.0;
        for n in 0..(0.1 / step) as usize {
            let t = n as f64 * step;
            let current = if t % period < on * period { 0.020 } else { 0.0 };
            meter.add(&at(t, &[9.0, current]), speed);
            if t > 0.06 {
                let seen = meter.currents()[&id][0];
                (low, high) = (low.min(seen), high.max(seen));
            }
        }
        (low, high)
    }

    #[test]
    fn rapid_switching_looks_like_its_average() {
        // Arduino's 490 Hz PWM at a quarter, watched at real speed.
        let (low, high) = switched(1.0 / 490.0, 0.25, 1.0);
        assert!(low > 0.0044 && high < 0.0056, "{low} {high}");
        // Four digits taking turns, 5 ms each.
        let (low, high) = switched(0.020, 0.25, 1.0);
        assert!(low > 0.0045 && high < 0.0055, "{low} {high}");
    }

    #[test]
    fn in_slow_motion_the_switching_is_seen() {
        // At a thousandth of real speed each 2 ms pulse lasts seconds.
        let (low, high) = switched(0.002, 0.5, 0.001);
        assert!(low < 0.001 && high > 0.019, "{low} {high}");
    }

    #[test]
    fn a_steady_current_is_shown_at_once_and_a_restart_forgets() {
        let (mut meter, id) = one_led();
        assert!(meter.currents().is_empty());
        meter.add(&at(5.0, &[0.0, 0.012]), 1.0);
        assert_eq!(meter.currents()[&id], [0.012]);
        meter.add(&at(5.001, &[0.0, 0.012]), 1.0);
        assert!((meter.currents()[&id][0] - 0.012).abs() < 1e-12);
        // Time going back means the circuit started again.
        meter.add(&at(0.0, &[0.0, 0.0]), 1.0);
        assert_eq!(meter.currents()[&id], [0.0]);
    }

    #[test]
    fn each_led_of_a_display_is_followed_by_itself() {
        let nets: Vec<String> = (0..20).map(|i| format!("n{i}")).collect();
        let nets: Vec<&str> = nets.iter().map(String::as_str).collect();
        let colour = LedColour::Green;
        let netlist = NetlistBuilder::new()
            .element("DS1", K::LedBar { colour }, &nets)
            .build();
        let id = netlist.elements[0].component;
        let mut meter = LedMeter::new(&netlist);
        let currents: Vec<f64> = (0..10).map(|k| f64::from(k) * 0.001).collect();
        let mut sample = at(0.0, &[0.0]);
        sample.lamps = vec![(0, currents.clone())];
        meter.add(&sample, 1.0);
        assert_eq!(meter.currents()[&id], currents);
    }

    #[test]
    fn brightness_follows_the_current() {
        assert_eq!(brightness(0.0), 0.0);
        assert_eq!(brightness(-0.02), 0.0);
        assert_eq!(brightness(f64::NAN), 0.0);
        let steps: Vec<f32> = [0.0001, 0.001, 0.005, 0.010, 0.020]
            .into_iter()
            .map(brightness)
            .collect();
        assert!(steps.windows(2).all(|w| w[0] < w[1]), "{steps:?}");
        assert_eq!(brightness(0.5), 1.0);
        // Even a faint one can be told from off.
        assert!(brightness(0.0001) >= 0.15);
    }
}
