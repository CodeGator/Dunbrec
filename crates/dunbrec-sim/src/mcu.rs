//! Microcontrollers inside a circuit.
//!
//! Each MCU runs on the [`dunbrec_mcu`] emulator. It meets the analog
//! circuit at its pins, each an [`McuPin`] device: an output is the logic
//! level behind the pin's output resistance, an input with pull-up is
//! 5 V behind the pull-up resistor, and a plain input is a tiny leakage.
//! Inputs read their net through thresholds with hysteresis, like logic
//! inputs, and the ADC samples the net voltage.
//!
//! ## Keeping the chip and the circuit in step
//!
//! The chip runs *ahead* of the analog solution on a copy of itself, with
//! its inputs frozen, until its next event: a pin drive change or an ADC
//! sample (or a horizon). The transient then steps the circuit to exactly
//! that time and the copy is adopted. If an input crosses a threshold
//! first, the copy is thrown away, the chip is run from its last committed
//! state to the crossing, and the new input level applied, so input timing
//! is exact to the clock cycle. Nothing the chip did between those points
//! could affect the circuit, because it had no event there.

use std::cell::Cell;
use std::path::Path;

use dunbrec_core::{ComponentKind, NodeId};
use dunbrec_mcu::{Atmega328p, PIN_COUNT, PinDrive, Stop};

use crate::ac::{AcContext, AcStamper};
use crate::device::{Device, Mode, StampContext};
use crate::digital::Ramp;
use crate::error::SimError;
use crate::stamp::{SolutionView, Stamper};

/// Supply voltage of the microcontroller.
pub const MCU_VCC: f64 = 5.0;
/// Output resistance of an I/O pin (about 25 Ω at 5 V).
pub const MCU_R_OUT: f64 = 25.0;
/// Internal pull-up resistance (20–50 kΩ in the datasheet).
pub const MCU_R_PULLUP: f64 = 35e3;
/// Leakage of a floating input.
pub const MCU_R_INPUT: f64 = 1e9;
/// Input thresholds: guaranteed low below 0.3 VCC, high above 0.6 VCC.
pub const MCU_VIL: f64 = 1.5;
pub const MCU_VIH: f64 = 3.0;
/// Output rise and fall time.
const MCU_EDGE: f64 = 5e-9;

fn electrical(drive: PinDrive) -> (f64, f64) {
    match drive {
        PinDrive::Low => (0.0, 1.0 / MCU_R_OUT),
        PinDrive::High => (MCU_VCC, 1.0 / MCU_R_OUT),
        PinDrive::PullUp => (MCU_VCC, 1.0 / MCU_R_PULLUP),
        PinDrive::Floating => (0.0, 1.0 / MCU_R_INPUT),
    }
}

/// One microcontroller pin as seen by the analog circuit (a Norton
/// source, so no extra unknowns).
#[derive(Debug)]
pub struct McuPin {
    pub name: String,
    pub node: NodeId,
    level: Cell<Ramp>,
    conductance: Cell<f64>,
}

impl McuPin {
    pub fn new(name: String, node: NodeId) -> Self {
        let (v, g) = electrical(PinDrive::Floating);
        Self {
            name,
            node,
            level: Cell::new(Ramp {
                t0: 0.0,
                from: v,
                to: v,
                edge: MCU_EDGE,
            }),
            conductance: Cell::new(g),
        }
    }
}

impl Device for McuPin {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext) {
        let ramp = self.level.get();
        let v = match ctx.mode {
            Mode::Dc { .. } => ramp.to,
            Mode::Transient { .. } => ramp.at(ctx.time),
        };
        let g = self.conductance.get();
        s.conductance(self.node, NodeId::GROUND, g);
        s.current(NodeId::GROUND, self.node, g * v);
    }
    fn stamp_ac(&self, s: &mut AcStamper, _ctx: &AcContext) {
        s.conductance(self.node, NodeId::GROUND, self.conductance.get());
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.node, NodeId::GROUND)]
    }
    /// Current flowing from the net into the pin.
    fn current(&self, sol: &SolutionView) -> f64 {
        self.conductance.get() * (sol.voltage(self.node) - self.level.get().to)
    }
    fn next_breakpoint(&self, after: f64) -> Option<f64> {
        let r = self.level.get();
        [r.t0, r.t0 + r.edge]
            .into_iter()
            .find(|&t| t > after && r.from != r.to)
    }
    fn drive_pin(&self, time: f64, drive: PinDrive) {
        let (v, g) = electrical(drive);
        let ramp = self.level.get();
        self.conductance.set(g);
        if time.is_infinite() {
            self.level.set(Ramp {
                t0: 0.0,
                from: v,
                to: v,
                edge: MCU_EDGE,
            });
        } else if v != ramp.to {
            self.level.set(Ramp {
                t0: time,
                from: ramp.at(time),
                to: v,
                edge: MCU_EDGE,
            });
        }
    }
}

/// How a microcontroller is wired into a circuit.
#[derive(Debug, Clone, PartialEq)]
pub struct McuSpec {
    /// Index of the part in the netlist (and in the circuit's devices).
    pub element: usize,
    pub designator: String,
    pub kind: ComponentKind,
    /// Node of each pin, in Arduino order (D0–D13, A0–A5).
    pub nodes: Vec<NodeId>,
    /// Device index of each pin's [`McuPin`].
    pub drivers: Vec<usize>,
}

/// A byte sent on a microcontroller's serial port.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SerialByte {
    /// Netlist index of the microcontroller.
    pub element: usize,
    /// Time its stop bit ended (s).
    pub time: f64,
    pub byte: u8,
}

#[derive(Debug, Clone)]
struct Part {
    spec: McuSpec,
    clock: f64,
    /// State at the last committed point.
    chip: Atmega328p,
    /// Speculative copy run to its next event, and why it stopped.
    ahead: Option<(Atmega328p, Stop)>,
    /// Drive each pin's device currently has.
    applied: [PinDrive; PIN_COUNT],
}

impl Part {
    fn cycle_at(&self, t: f64) -> u64 {
        (t * self.clock).round().max(0.0) as u64
    }

    fn time_of(&self, cycle: u64) -> f64 {
        cycle as f64 / self.clock
    }

    fn handle(&mut self, stop: Stop, voltage: &dyn Fn(NodeId) -> f64) {
        if let Stop::AdcSample(ch) = stop {
            self.chip.set_analog(ch, voltage(self.spec.nodes[14 + ch]));
        }
    }

    /// Run the committed chip to `cycle`, answering ADC samples from the
    /// present solution (used only across sub-cycle gaps).
    fn run_to(&mut self, cycle: u64, voltage: &dyn Fn(NodeId) -> f64) {
        while self.chip.cycle() < cycle {
            let stop = self.chip.run(cycle);
            self.handle(stop, voltage);
        }
    }

    /// Adopt the speculative copy if it has reached `cycle`.
    fn adopt_if_due(&mut self, cycle: u64, voltage: &dyn Fn(NodeId) -> f64) -> bool {
        match &self.ahead {
            Some((a, _)) if a.cycle() <= cycle => {
                let (a, stop) = self.ahead.take().expect("checked");
                self.chip = a;
                self.handle(stop, voltage);
                true
            }
            _ => false,
        }
    }

    fn changes(&mut self) -> Vec<(usize, PinDrive)> {
        let now = self.chip.pin_drives();
        let mut out = Vec::new();
        for (pin, (&drive, applied)) in now.iter().zip(&mut self.applied).enumerate() {
            if drive != *applied {
                *applied = drive;
                out.push((self.spec.drivers[pin], drive));
            }
        }
        out
    }
}

/// All microcontrollers of a circuit.
#[derive(Debug, Clone)]
pub struct McuSystem {
    parts: Vec<Part>,
    serial: Vec<SerialByte>,
}

impl McuSystem {
    /// Load each part's firmware (relative paths against `base_dir`).
    pub fn new(specs: &[McuSpec], base_dir: Option<&Path>) -> Result<Self, SimError> {
        let parts = specs
            .iter()
            .map(|spec| {
                let ComponentKind::Mcu { firmware, clock } = &spec.kind else {
                    unreachable!("MCU specs are built for MCU parts");
                };
                let err = |message: String| SimError::Firmware {
                    designator: spec.designator.clone(),
                    message,
                };
                let image = dunbrec_mcu::firmware::load(firmware, base_dir).map_err(err)?;
                let chip = Atmega328p::new(&image).map_err(err)?;
                Ok(Part {
                    spec: spec.clone(),
                    clock: *clock,
                    applied: chip.pin_drives(),
                    chip,
                    ahead: None,
                })
            })
            .collect::<Result<Vec<_>, SimError>>()?;
        Ok(Self {
            parts,
            serial: Vec::new(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn part_of_element(&self, element: usize) -> Option<usize> {
        self.parts.iter().position(|p| p.spec.element == element)
    }

    pub fn spec(&self, part: usize) -> &McuSpec {
        &self.parts[part].spec
    }

    /// (part, pin, node) of every pin, for input threshold watches.
    pub fn inputs(&self) -> Vec<(usize, usize, NodeId)> {
        self.parts
            .iter()
            .enumerate()
            .flat_map(|(p, part)| {
                part.spec
                    .nodes
                    .iter()
                    .enumerate()
                    .map(move |(pin, &n)| (p, pin, n))
            })
            .collect()
    }

    /// Set every pin's input level from steady voltages (no edges).
    pub fn sync_inputs(&mut self, voltage: impl Fn(NodeId) -> f64) {
        for part in &mut self.parts {
            for pin in 0..PIN_COUNT {
                let v = voltage(part.spec.nodes[pin]);
                part.chip.set_input(pin, v >= 0.5 * (MCU_VIL + MCU_VIH));
                if pin >= 14 {
                    part.chip.set_analog(pin - 14, v);
                }
            }
        }
    }

    /// Make sure every chip has run ahead to its next event, looking no
    /// further than `horizon` (s).
    pub fn run_ahead(&mut self, horizon: f64) {
        for part in &mut self.parts {
            if part.ahead.is_none() {
                let mut a = part.chip.clone();
                let target = part.cycle_at(horizon).max(a.cycle() + 1);
                let stop = a.run(target);
                part.ahead = Some((a, stop));
            }
        }
    }

    /// Time of the earliest speculative stop, and whether it changes the
    /// circuit abruptly (a pin drive change) or is only a sync point.
    pub fn next_event(&self) -> Option<(f64, bool)> {
        self.parts
            .iter()
            .filter_map(|p| {
                let (a, stop) = p.ahead.as_ref()?;
                Some((p.time_of(a.cycle()), *stop == Stop::Pins))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
    }

    /// Commit every chip whose speculative run ends by `now`; returns the
    /// pin devices to re-drive.
    pub fn commit_due(
        &mut self,
        now: f64,
        voltage: &dyn Fn(NodeId) -> f64,
    ) -> Vec<(usize, PinDrive)> {
        let mut changes = Vec::new();
        for part in &mut self.parts {
            let cycle = part.cycle_at(now);
            if part.adopt_if_due(cycle, voltage) {
                changes.extend(part.changes());
            }
        }
        self.collect_serial();
        changes
    }

    /// An input pin crossed a threshold at `time`.
    pub fn input_event(
        &mut self,
        time: f64,
        part: usize,
        pin: usize,
        high: bool,
        voltage: &dyn Fn(NodeId) -> f64,
    ) -> Vec<(usize, PinDrive)> {
        let p = &mut self.parts[part];
        let cycle = p.cycle_at(time).max(p.chip.cycle());
        if !p.adopt_if_due(cycle, voltage) {
            p.ahead = None;
        }
        p.run_to(cycle, voltage);
        p.chip.set_input(pin, high);
        p.ahead = None;
        let changes = p.changes();
        self.collect_serial();
        changes
    }

    fn collect_serial(&mut self) {
        for part in &mut self.parts {
            for (cycle, byte) in part.chip.take_serial() {
                self.serial.push(SerialByte {
                    element: part.spec.element,
                    time: part.time_of(cycle),
                    byte,
                });
            }
        }
    }

    pub fn take_serial(&mut self) -> Vec<SerialByte> {
        std::mem::take(&mut self.serial)
    }

    /// The first chip whose CPU has stopped on a firmware error.
    pub fn fault(&self) -> Option<SimError> {
        self.parts.iter().find_map(|p| {
            p.chip.fault().map(|m| SimError::Firmware {
                designator: p.spec.designator.clone(),
                message: m.to_string(),
            })
        })
    }
}
