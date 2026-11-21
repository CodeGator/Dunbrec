//! Event-driven digital logic and its interface to the analog circuit.
//!
//! Digital parts are simulated by events: an input changes, and the part's
//! outputs change one propagation delay later. They meet the analog world
//! only at their pins:
//!
//! * each **output** is a [`DigitalDriver`] in the analog circuit — the
//!   logic level's voltage behind the family's output resistance, ramping
//!   between levels over the family's edge time;
//! * each **input** watches its net's voltage: crossing `VIH` upwards makes
//!   it high, crossing `VIL` downwards makes it low (hysteresis).
//!
//! So logic can drive any analog load, analog signals can clock logic, and
//! instruments see digital nets as ordinary voltages.
//!
//! Mixed-signal chips use the same machinery with two generalisations: an
//! input may compare two voltages (a comparator, see [`InputSense`]), and an
//! output may open and close a resistive path instead of driving a level
//! ([`SwitchDriver`]). The 555 timer is built this way.

use std::cell::Cell;
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use dunbrec_core::{ComponentKind, GateOp, NodeId};

use crate::ac::{AcContext, AcStamper};
use crate::circuit::Circuit;
use crate::device::{Device, Mode, StampContext, Tolerances};
use crate::error::SimError;
use crate::newton::{Workspace, dc_solve};
use crate::stamp::{SolutionView, Stamper, node_rows};

/// Electrical characteristics shared by all logic parts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicFamily {
    pub vol: f64,
    pub voh: f64,
    /// An input reads low below this voltage...
    pub vil: f64,
    /// ...and high above this one.
    pub vih: f64,
    /// Output resistance (Ω).
    pub r_out: f64,
    /// Output rise and fall time (s).
    pub edge: f64,
}

/// 5 V CMOS-style logic.
pub const CMOS_5V: LogicFamily = LogicFamily {
    vol: 0.0,
    voh: 5.0,
    vil: 1.5,
    vih: 3.5,
    r_out: 50.0,
    edge: 5e-9,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Logic {
    Low,
    High,
    /// Unknown (e.g. an output before its inputs are known).
    X,
}

/// What a digital input reads: the voltage `V(p) - V(n)`. Rising through
/// `rise` makes it high, falling through `fall` makes it low.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputSense {
    pub p: NodeId,
    pub n: NodeId,
    pub rise: f64,
    pub fall: f64,
}

impl InputSense {
    /// An ordinary logic input on `node`, with the family's thresholds.
    pub fn logic(node: NodeId, f: &LogicFamily) -> Self {
        Self {
            p: node,
            n: NodeId::GROUND,
            rise: f.vih,
            fall: f.vil,
        }
    }

    /// A comparator: high when `V(p) > V(n)`, with `hysteresis` volts
    /// between its switching points.
    pub fn comparator(p: NodeId, n: NodeId, hysteresis: f64) -> Self {
        Self {
            p,
            n,
            rise: 0.5 * hysteresis,
            fall: -0.5 * hysteresis,
        }
    }

    /// Level of a steady input (no history for hysteresis): beyond a
    /// threshold it is decided, in between the midpoint decides.
    pub fn level(&self, voltage: &impl Fn(NodeId) -> f64) -> Logic {
        let v = voltage(self.p) - voltage(self.n);
        if v >= self.rise {
            Logic::High
        } else if v <= self.fall {
            Logic::Low
        } else {
            Logic::from_bool(v >= 0.5 * (self.rise + self.fall))
        }
    }
}

impl Logic {
    pub fn from_bool(b: bool) -> Self {
        if b { Logic::High } else { Logic::Low }
    }

    pub fn invert(self) -> Self {
        match self {
            Logic::Low => Logic::High,
            Logic::High => Logic::Low,
            Logic::X => Logic::X,
        }
    }

    pub fn voltage(self, f: &LogicFamily) -> f64 {
        match self {
            Logic::Low => f.vol,
            Logic::High => f.voh,
            Logic::X => 0.5 * (f.vol + f.voh),
        }
    }

    /// Level of a steady voltage (used at the operating point, where
    /// there is no history for hysteresis): above the midpoint is high.
    pub fn from_voltage(v: f64, f: &LogicFamily) -> Self {
        if v >= f.vih {
            Logic::High
        } else if v <= f.vil {
            Logic::Low
        } else {
            Logic::from_bool(v >= 0.5 * (f.vil + f.vih))
        }
    }
}

fn and(a: Logic, b: Logic) -> Logic {
    match (a, b) {
        (Logic::Low, _) | (_, Logic::Low) => Logic::Low,
        (Logic::High, Logic::High) => Logic::High,
        _ => Logic::X,
    }
}

fn or(a: Logic, b: Logic) -> Logic {
    match (a, b) {
        (Logic::High, _) | (_, Logic::High) => Logic::High,
        (Logic::Low, Logic::Low) => Logic::Low,
        _ => Logic::X,
    }
}

fn xor(a: Logic, b: Logic) -> Logic {
    match (a, b) {
        (Logic::X, _) | (_, Logic::X) => Logic::X,
        _ => Logic::from_bool(a != b),
    }
}

pub fn gate(op: GateOp, a: Logic, b: Logic) -> Logic {
    match op {
        GateOp::And => and(a, b),
        GateOp::Or => or(a, b),
        GateOp::Nand => and(a, b).invert(),
        GateOp::Nor => or(a, b).invert(),
        GateOp::Xor => xor(a, b),
        GateOp::Xnor => xor(a, b).invert(),
    }
}

// ------------------------------------------------- analog-side devices ---

/// A linear ramp from `from` (at `t0`) to `to` (at `t0 + edge`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Ramp {
    pub t0: f64,
    pub from: f64,
    pub to: f64,
    pub edge: f64,
}

impl Ramp {
    pub(crate) fn at(&self, t: f64) -> f64 {
        if t <= self.t0 {
            self.from
        } else if t >= self.t0 + self.edge {
            self.to
        } else {
            self.from + (self.to - self.from) * (t - self.t0) / self.edge
        }
    }
}

/// The analog face of a digital output: its level's voltage behind the
/// output resistance (a Norton source, so no extra unknowns).
#[derive(Debug)]
pub struct DigitalDriver {
    pub name: String,
    pub node: NodeId,
    family: LogicFamily,
    ramp: Cell<Ramp>,
}

impl DigitalDriver {
    pub fn new(name: String, node: NodeId, family: LogicFamily) -> Self {
        let mid = Logic::X.voltage(&family);
        Self {
            name,
            node,
            family,
            ramp: Cell::new(Ramp {
                t0: 0.0,
                from: mid,
                to: mid,
                edge: family.edge,
            }),
        }
    }

    fn conductance(&self) -> f64 {
        1.0 / self.family.r_out
    }
}

impl Device for DigitalDriver {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext) {
        let ramp = self.ramp.get();
        let v = match ctx.mode {
            Mode::Dc { .. } => ramp.to,
            Mode::Transient { .. } => ramp.at(ctx.time),
        };
        let g = self.conductance();
        s.conductance(self.node, NodeId::GROUND, g);
        s.current(NodeId::GROUND, self.node, g * v);
    }
    fn stamp_ac(&self, s: &mut AcStamper, _ctx: &AcContext) {
        s.conductance(self.node, NodeId::GROUND, self.conductance());
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.node, NodeId::GROUND)]
    }
    fn current_injections(&self) -> Vec<NodeId> {
        Vec::new()
    }
    /// Current flowing from the net into the output.
    fn current(&self, sol: &SolutionView) -> f64 {
        self.conductance() * (sol.voltage(self.node) - self.ramp.get().to)
    }
    fn next_breakpoint(&self, after: f64) -> Option<f64> {
        let r = self.ramp.get();
        [r.t0, r.t0 + r.edge]
            .into_iter()
            .find(|&t| t > after && r.from != r.to)
    }
    fn drive(&self, time: f64, level: f64) {
        let ramp = self.ramp.get();
        if time.is_infinite() {
            // Operating point: jump straight to the level.
            self.ramp.set(Ramp {
                t0: 0.0,
                from: level,
                to: level,
                edge: self.family.edge,
            });
        } else if level != ramp.to {
            self.ramp.set(Ramp {
                t0: time,
                from: ramp.at(time),
                to: level,
                edge: self.family.edge,
            });
        }
    }
}

/// The analog face of an output that switches a path rather than driving a
/// level: a conductance between `a` and `b`, closed when the output is high.
/// While switching it moves between its off and on values geometrically
/// over `edge` seconds, like a transistor turning on.
#[derive(Debug)]
pub struct SwitchDriver {
    pub name: String,
    pub a: NodeId,
    pub b: NodeId,
    g_on: f64,
    g_off: f64,
    edge: f64,
    family: LogicFamily,
    /// How closed the switch is, 0 (open) to 1 (closed).
    ramp: Cell<Ramp>,
}

impl SwitchDriver {
    pub fn new(
        name: String,
        (a, b): (NodeId, NodeId),
        (r_on, r_off): (f64, f64),
        edge: f64,
        family: LogicFamily,
    ) -> Self {
        Self {
            name,
            a,
            b,
            g_on: 1.0 / r_on,
            g_off: 1.0 / r_off,
            edge,
            family,
            ramp: Cell::new(Ramp {
                t0: 0.0,
                from: 0.5,
                to: 0.5,
                edge,
            }),
        }
    }

    fn conductance(&self, closed: f64) -> f64 {
        self.g_off * (self.g_on / self.g_off).powf(closed.clamp(0.0, 1.0))
    }

    fn present(&self, ctx: &StampContext) -> f64 {
        let ramp = self.ramp.get();
        let closed = match ctx.mode {
            Mode::Dc { .. } => ramp.to,
            Mode::Transient { .. } => ramp.at(ctx.time),
        };
        self.conductance(closed)
    }
}

impl Device for SwitchDriver {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext) {
        s.conductance(self.a, self.b, self.present(ctx));
    }
    fn stamp_ac(&self, s: &mut AcStamper, _ctx: &AcContext) {
        s.conductance(self.a, self.b, self.conductance(self.ramp.get().to));
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.a, self.b)]
    }
    /// Current from `a` through the switch to `b`.
    fn current(&self, sol: &SolutionView) -> f64 {
        self.conductance(self.ramp.get().to) * (sol.voltage(self.a) - sol.voltage(self.b))
    }
    fn next_breakpoint(&self, after: f64) -> Option<f64> {
        let r = self.ramp.get();
        [r.t0, r.t0 + r.edge]
            .into_iter()
            .find(|&t| t > after && r.from != r.to)
    }
    /// `level` is a logic voltage of the family: high closes the switch.
    fn drive(&self, time: f64, level: f64) {
        let f = &self.family;
        let closed = ((level - f.vol) / (f.voh - f.vol)).clamp(0.0, 1.0);
        let ramp = self.ramp.get();
        if time.is_infinite() {
            self.ramp.set(Ramp {
                t0: 0.0,
                from: closed,
                to: closed,
                edge: self.edge,
            });
        } else if closed != ramp.to {
            self.ramp.set(Ramp {
                t0: time,
                from: ramp.at(time),
                to: closed,
                edge: self.edge,
            });
        }
    }
}

/// Placeholder for a digital part in the analog device list, keeping device
/// indices equal to netlist element indices. Its pins are handled by
/// [`DigitalDriver`]s and input watches.
#[derive(Debug)]
pub struct NullDevice {
    pub name: String,
}

impl Device for NullDevice {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp(&self, _s: &mut Stamper, _ctx: &StampContext) {}
    fn stamp_ac(&self, _s: &mut AcStamper, _ctx: &AcContext) {}
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        Vec::new()
    }
    fn current(&self, _sol: &SolutionView) -> f64 {
        0.0
    }
}

// ---------------------------------------------------- digital parts ---

/// How a digital part is wired into a circuit.
#[derive(Debug, Clone, PartialEq)]
pub struct PartSpec {
    /// Index of the part in the netlist (and in the circuit's devices).
    pub element: usize,
    pub kind: ComponentKind,
    /// What each input reads.
    pub inputs: Vec<InputSense>,
    /// Device index of the driver of each output.
    pub drivers: Vec<usize>,
    /// Net each output sets to its logic level, if it does (a switch
    /// output does not).
    pub output_nodes: Vec<Option<NodeId>>,
}

#[derive(Debug, Clone)]
struct Part {
    spec: PartSpec,
    inputs: Vec<Logic>,
    outputs: Vec<Logic>,
    /// Last value scheduled for each output (avoids duplicate events).
    scheduled: Vec<Logic>,
    /// Flip-flop Q or counter value.
    state: u32,
    flop: Logic,
    clock_high: bool,
    /// Invalidates clock toggles scheduled before a frequency change.
    clock_generation: u32,
}

impl Part {
    fn delay(&self) -> f64 {
        match self.spec.kind {
            ComponentKind::Gate { delay, .. }
            | ComponentKind::Inverter { delay }
            | ComponentKind::DFlipFlop { delay }
            | ComponentKind::Counter4 { delay } => delay.max(0.0),
            ComponentKind::Timer555 => TIMER555_DELAY,
            ComponentKind::Cd4066 { .. } => CD4066_DELAY,
            _ => 0.0,
        }
    }

    /// What the outputs should be, given the present inputs and state.
    fn evaluate(&self) -> Vec<Logic> {
        match self.spec.kind {
            ComponentKind::Gate { op, .. } => vec![gate(op, self.inputs[0], self.inputs[1])],
            ComponentKind::Inverter { .. } => vec![self.inputs[0].invert()],
            ComponentKind::DFlipFlop { .. } => vec![self.flop, self.flop.invert()],
            ComponentKind::Counter4 { .. } => (0..4)
                .map(|bit| Logic::from_bool(self.state >> bit & 1 == 1))
                .collect(),
            ComponentKind::Clock { .. } => vec![Logic::from_bool(self.clock_high)],
            ComponentKind::LogicSwitch { high } => vec![Logic::from_bool(high)],
            // Output high side, output low side, discharge.
            ComponentKind::Timer555 => vec![self.flop, self.flop.invert(), self.flop.invert()],
            // Each switch follows its control input.
            ComponentKind::Cd4066 { .. } => self.inputs.clone(),
            _ => Vec::new(),
        }
    }

    /// Apply an input change; returns true if it was a clock edge or level
    /// change that may change outputs.
    fn set_input(&mut self, k: usize, value: Logic) -> bool {
        let old = self.inputs[k];
        if old == value {
            return false;
        }
        self.inputs[k] = value;
        let rising = old == Logic::Low && value == Logic::High;
        match self.spec.kind {
            ComponentKind::DFlipFlop { .. } if k == 1 && rising => self.flop = self.inputs[0],
            ComponentKind::Counter4 { .. } if k == 0 && rising => {
                self.state = (self.state + 1) % 16
            }
            _ => {}
        }
        self.update_latch();
        true
    }

    /// Level-sensitive state: the 555's set/reset latch. Its inputs are
    /// [trigger, threshold, reset]: trigger active (high) sets it and wins
    /// over threshold, which resets it; the RESET pin low clears it.
    fn update_latch(&mut self) {
        if let ComponentKind::Timer555 = self.spec.kind {
            let [trigger, threshold, reset] = [self.inputs[0], self.inputs[1], self.inputs[2]];
            if reset == Logic::Low {
                self.flop = Logic::Low;
            } else if trigger == Logic::High {
                self.flop = Logic::High;
            } else if threshold == Logic::High {
                self.flop = Logic::Low;
            }
        }
    }
}

/// Delay from a CD4066B control pin to its switch (s): of the order of the
/// data sheet's 7–40 ns signal propagation time.
pub const CD4066_DELAY: f64 = 20e-9;

/// Propagation delay of the 555 from a comparator to its outputs (s).
pub const TIMER555_DELAY: f64 = 100e-9;

#[derive(Debug, Clone, Copy, PartialEq)]
enum EventKind {
    Output {
        part: usize,
        out: usize,
        value: Logic,
    },
    ClockToggle {
        part: usize,
        generation: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Event {
    time: f64,
    seq: u64,
    kind: EventKind,
}

impl Eq for Event {}
impl Ord for Event {
    fn cmp(&self, other: &Self) -> Ordering {
        self.time
            .total_cmp(&other.time)
            .then(self.seq.cmp(&other.seq))
    }
}
impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// All digital parts of a circuit and their pending events.
#[derive(Debug, Clone)]
pub struct DigitalSystem {
    parts: Vec<Part>,
    queue: BinaryHeap<Reverse<Event>>,
    seq: u64,
    pub family: LogicFamily,
}

impl DigitalSystem {
    pub fn new(specs: &[PartSpec], family: LogicFamily) -> Self {
        let parts = specs
            .iter()
            .map(|spec| {
                let n_out = spec.drivers.len();
                let mut p = Part {
                    spec: spec.clone(),
                    inputs: vec![Logic::X; spec.inputs.len()],
                    outputs: vec![Logic::X; n_out],
                    scheduled: vec![Logic::X; n_out],
                    state: 0,
                    flop: Logic::Low,
                    clock_high: false,
                    clock_generation: 0,
                };
                p.outputs = p.evaluate();
                p.scheduled = p.outputs.clone();
                p
            })
            .collect();
        Self {
            parts,
            queue: BinaryHeap::new(),
            seq: 0,
            family,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// (part, input) of every digital input, with what it reads.
    pub fn inputs(&self) -> Vec<(usize, usize, InputSense)> {
        self.parts
            .iter()
            .enumerate()
            .flat_map(|(p, part)| {
                part.spec
                    .inputs
                    .iter()
                    .enumerate()
                    .map(move |(k, &sense)| (p, k, sense))
            })
            .collect()
    }

    pub fn part_of_element(&self, element: usize) -> Option<usize> {
        self.parts.iter().position(|p| p.spec.element == element)
    }

    fn push(&mut self, time: f64, kind: EventKind) {
        self.seq += 1;
        self.queue.push(Reverse(Event {
            time,
            seq: self.seq,
            kind,
        }));
    }

    /// Schedule output changes for part `p` evaluated at `time`.
    fn schedule_outputs(&mut self, p: usize, time: f64) {
        let desired = self.parts[p].evaluate();
        let at = time + self.parts[p].delay();
        for (out, value) in desired.into_iter().enumerate() {
            if self.parts[p].scheduled[out] != value {
                self.parts[p].scheduled[out] = value;
                self.push(
                    at,
                    EventKind::Output {
                        part: p,
                        out,
                        value,
                    },
                );
            }
        }
    }

    fn schedule_clock(&mut self, p: usize, time: f64) {
        if let ComponentKind::Clock { frequency, duty } = self.parts[p].spec.kind {
            if frequency.is_nan() || frequency <= 0.0 {
                return;
            }
            let period = 1.0 / frequency;
            let duty = duty.clamp(0.01, 0.99);
            let wait = if self.parts[p].clock_high {
                duty * period
            } else {
                (1.0 - duty) * period
            };
            let generation = self.parts[p].clock_generation;
            self.push(
                time + wait,
                EventKind::ClockToggle {
                    part: p,
                    generation,
                },
            );
        }
    }

    /// Set every input's level from steady node voltages, without
    /// treating the change as a clock edge.
    pub fn sync_inputs(&mut self, voltage: impl Fn(NodeId) -> f64) {
        for part in &mut self.parts {
            for k in 0..part.inputs.len() {
                part.inputs[k] = part.spec.inputs[k].level(&voltage);
            }
        }
    }

    /// Begin event-driven simulation at `time`: clocks start ticking, and
    /// any output not yet consistent with its inputs (e.g. in a ring
    /// oscillator, which has no steady state) is scheduled to change.
    pub fn start(&mut self, time: f64) {
        for p in 0..self.parts.len() {
            self.schedule_outputs(p, time);
            self.schedule_clock(p, time);
        }
    }

    /// An input changed at `time`.
    pub fn input_event(&mut self, time: f64, part: usize, input: usize, value: Logic) {
        if self.parts[part].set_input(input, value) {
            self.schedule_outputs(part, time);
        }
    }

    pub fn next_event_time(&self) -> Option<f64> {
        self.queue.peek().map(|Reverse(e)| e.time)
    }

    /// Process all events up to `now`; returns (driver device, level)
    /// for every output that changed.
    pub fn advance(&mut self, now: f64) -> Vec<(usize, Logic)> {
        let mut changes = Vec::new();
        let eps = 1e-15 + now.abs() * 1e-12;
        while let Some(Reverse(e)) = self.queue.peek().copied() {
            if e.time > now + eps {
                break;
            }
            self.queue.pop();
            match e.kind {
                EventKind::Output { part, out, value } => {
                    let pt = &mut self.parts[part];
                    if pt.outputs[out] != value {
                        pt.outputs[out] = value;
                        changes.push((pt.spec.drivers[out], value));
                    }
                }
                EventKind::ClockToggle { part, generation } => {
                    if self.parts[part].clock_generation == generation {
                        self.parts[part].clock_high = !self.parts[part].clock_high;
                        self.schedule_outputs(part, e.time);
                        self.schedule_clock(part, e.time);
                    }
                }
            }
        }
        changes
    }

    /// Settle without delays for the operating point. Parts are evaluated
    /// one at a time, each seeing the outputs of parts already evaluated
    /// (a net driven by a logic output takes that output's level; other
    /// nets are read from their steady voltage). Updating one part at a
    /// time, rather than all at once, breaks the artificial symmetry of
    /// loops such as ring oscillators, as a real power-up does. Returns
    /// true if any output changed.
    fn settle_static(&mut self, voltage: impl Fn(NodeId) -> f64) -> bool {
        let mut changed = false;
        for p in 0..self.parts.len() {
            for k in 0..self.parts[p].inputs.len() {
                let sense = self.parts[p].spec.inputs[k];
                // A ground-referenced input on a net a logic output drives
                // reads that output's level.
                let driven = sense
                    .n
                    .is_ground()
                    .then(|| {
                        self.parts.iter().find_map(|q| {
                            let i = q
                                .spec
                                .output_nodes
                                .iter()
                                .position(|&n| n == Some(sense.p))?;
                            Some(q.outputs[i]).filter(|l| *l != Logic::X)
                        })
                    })
                    .flatten();
                // No clock edges at the operating point: set levels only.
                self.parts[p].inputs[k] = driven.unwrap_or_else(|| sense.level(&voltage));
            }
            self.parts[p].update_latch();
            let outs = self.parts[p].evaluate();
            if outs != self.parts[p].outputs {
                changed = true;
                self.parts[p].outputs = outs.clone();
                self.parts[p].scheduled = outs;
            }
        }
        changed
    }

    /// Every output's driver and present level.
    fn driver_levels(&self) -> Vec<(usize, Logic)> {
        self.parts
            .iter()
            .flat_map(|p| {
                p.spec
                    .drivers
                    .iter()
                    .copied()
                    .zip(p.outputs.iter().copied())
            })
            .collect()
    }

    /// New parameters for a part while running (e.g. a logic switch
    /// flipped, a clock retuned). Effects are scheduled from `time`.
    pub fn update_part(&mut self, part: usize, kind: &ComponentKind, time: f64) {
        let was_clock = matches!(self.parts[part].spec.kind, ComponentKind::Clock { .. });
        self.parts[part].spec.kind = kind.clone();
        if was_clock {
            self.parts[part].clock_generation += 1;
            self.schedule_clock(part, time);
        }
        self.schedule_outputs(part, time);
    }

    /// The kind of the part for netlist element `element`, as last set.
    pub fn part_kind(&self, element: usize) -> Option<&ComponentKind> {
        self.part_of_element(element)
            .map(|p| &self.parts[p].spec.kind)
    }

    /// Present output levels of the part for netlist element `element`.
    pub fn output_levels(&self, element: usize) -> Option<Vec<Logic>> {
        self.part_of_element(element)
            .map(|p| self.parts[p].outputs.clone())
    }
}

/// Apply every output's level to its analog driver at the operating point.
fn apply_static(circuit: &Circuit, sys: &DigitalSystem) {
    for (driver, level) in sys.driver_levels() {
        circuit.devices[driver].drive(f64::INFINITY, level.voltage(&sys.family));
    }
}

/// Operating point of a circuit with logic: analog solve and logic
/// evaluation alternate until they agree (or a limit is reached, e.g. for
/// a ring oscillator, which has no steady state).
pub(crate) fn settle_dc(
    circuit: &Circuit,
    sys: &mut DigitalSystem,
    ws: &mut Workspace,
    tol: &Tolerances,
    initial_conditions: bool,
) -> Result<Vec<f64>, SimError> {
    let rows = node_rows(circuit.node_count());
    apply_static(circuit, sys);
    let mut x = dc_solve(circuit, initial_conditions, ws, tol)?;
    for _ in 0..20 {
        let view = SolutionView {
            x: &x,
            node_rows: rows,
        };
        if !sys.settle_static(|n| view.voltage(n)) {
            break;
        }
        apply_static(circuit, sys);
        x = dc_solve(circuit, initial_conditions, ws, tol)?;
    }
    Ok(x)
}

/// DC solution of any circuit: plain for analog, settled with its logic
/// for mixed circuits.
pub(crate) fn operating_solution(
    circuit: &Circuit,
    ws: &mut Workspace,
    tol: &Tolerances,
) -> Result<Vec<f64>, SimError> {
    if circuit.digital.is_empty() {
        dc_solve(circuit, false, ws, tol)
    } else {
        let mut sys = DigitalSystem::new(&circuit.digital, CMOS_5V);
        settle_dc(circuit, &mut sys, ws, tol, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Logic::*;

    #[test]
    fn truth_tables_with_unknowns() {
        assert_eq!(gate(GateOp::And, High, High), High);
        assert_eq!(gate(GateOp::And, Low, X), Low);
        assert_eq!(gate(GateOp::And, High, X), X);
        assert_eq!(gate(GateOp::Or, X, High), High);
        assert_eq!(gate(GateOp::Nor, Low, Low), High);
        assert_eq!(gate(GateOp::Nand, High, High), Low);
        assert_eq!(gate(GateOp::Xor, High, Low), High);
        assert_eq!(gate(GateOp::Xor, X, Low), X);
        assert_eq!(gate(GateOp::Xnor, High, High), High);
        assert_eq!(Logic::from_voltage(2.4, &CMOS_5V), Low);
        assert_eq!(Logic::from_voltage(2.6, &CMOS_5V), High);
        assert_eq!(X.voltage(&CMOS_5V), 2.5);
    }

    fn spec(kind: ComponentKind, inputs: usize, outputs: usize) -> PartSpec {
        PartSpec {
            element: 0,
            kind,
            inputs: vec![InputSense::logic(NodeId(1), &CMOS_5V); inputs],
            drivers: (0..outputs).collect(),
            output_nodes: vec![Some(NodeId(2)); outputs],
        }
    }

    #[test]
    fn flip_flop_and_counter_act_on_rising_edges() {
        let mut sys = DigitalSystem::new(
            &[
                spec(ComponentKind::DFlipFlop { delay: 1e-9 }, 2, 2),
                spec(ComponentKind::Counter4 { delay: 1e-9 }, 1, 4),
            ],
            CMOS_5V,
        );
        // D = 1, then a clock edge: Q goes high 1 ns later.
        sys.input_event(0.0, 0, 0, High);
        sys.input_event(0.0, 0, 1, Low);
        sys.input_event(1e-6, 0, 1, High);
        assert!(sys.advance(1e-6).is_empty(), "delay not yet elapsed");
        let ch = sys.advance(1e-6 + 1e-9);
        assert_eq!(ch, vec![(0, High), (1, Low)]);
        // A falling edge does nothing.
        sys.input_event(2e-6, 0, 1, Low);
        assert!(sys.advance(3e-6).is_empty());

        // Counter: five rising edges -> 0101.
        for k in 0..5 {
            let t = 10e-6 + k as f64 * 1e-6;
            sys.input_event(t, 1, 0, Low);
            sys.input_event(t + 0.5e-6, 1, 0, High);
            sys.advance(t + 0.9e-6);
        }
        assert_eq!(sys.output_levels(0).unwrap().len(), 2);
        assert_eq!(sys.parts[1].outputs, vec![High, Low, High, Low]);
    }

    #[test]
    fn clock_timing() {
        let mut sys = DigitalSystem::new(
            &[spec(
                ComponentKind::Clock {
                    frequency: 1e3,
                    duty: 0.25,
                },
                0,
                1,
            )],
            CMOS_5V,
        );
        assert_eq!(sys.parts[0].outputs, vec![Low]);
        sys.start(0.0);
        // Low for 0.75 ms, high for 0.25 ms.
        assert_eq!(sys.next_event_time(), Some(0.75e-3));
        assert_eq!(sys.advance(0.75e-3), vec![(0, High)]);
        assert!((sys.next_event_time().unwrap() - 1e-3).abs() < 1e-15);
        assert_eq!(sys.advance(1e-3), vec![(0, Low)]);
    }
}
