//! The [`Device`] trait and the linear device models.
//!
//! Every device knows how to write its equations into the MNA system and how
//! to report the current through it. Topology information (`dc_paths`,
//! `voltage_constraints`, `current_injections`) lets the circuit builder catch
//! unsolvable circuits with a clear message before the solver fails.

use dunbrec_core::{NodeId, Waveform};

use crate::stamp::{BranchId, SolutionView, Stamper};

pub trait Device: Send + std::fmt::Debug {
    fn designator(&self) -> &str;

    /// Number of branch-current unknowns this device needs.
    fn branch_count(&self) -> usize {
        0
    }

    /// Receives the id of the first of this device's branches.
    fn assign_branches(&mut self, _first: BranchId) {}

    /// Write the DC (operating point) equations.
    fn stamp_dc(&self, s: &mut Stamper);

    /// Node pairs joined by a DC-conducting path through this device.
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)>;

    /// Node pairs whose voltage difference this device fixes in DC.
    fn voltage_constraints(&self) -> Vec<(NodeId, NodeId)> {
        Vec::new()
    }

    /// Nodes into which this device forces a current (current sources).
    fn current_injections(&self) -> Vec<NodeId> {
        Vec::new()
    }

    /// Current entering the device at its first pin, given a solution.
    fn current(&self, sol: &SolutionView) -> f64;
}

fn first_branch(b: Option<BranchId>, name: &str) -> BranchId {
    b.unwrap_or_else(|| panic!("{name}: branches not assigned"))
}

#[derive(Debug)]
pub struct Resistor {
    pub name: String,
    pub p: NodeId,
    pub n: NodeId,
    pub resistance: f64,
}

impl Device for Resistor {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp_dc(&self, s: &mut Stamper) {
        s.conductance(self.p, self.n, 1.0 / self.resistance);
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.p, self.n)]
    }
    fn current(&self, sol: &SolutionView) -> f64 {
        (sol.voltage(self.p) - sol.voltage(self.n)) / self.resistance
    }
}

/// Smallest resistance a potentiometer segment is allowed to reach, so a
/// wiper at either end does not create an infinite conductance.
pub const POT_MIN_SEGMENT: f64 = 1e-3;

#[derive(Debug)]
pub struct Potentiometer {
    pub name: String,
    pub a: NodeId,
    pub b: NodeId,
    pub w: NodeId,
    pub resistance: f64,
    /// 0.0 = wiper at A, 1.0 = wiper at B.
    pub wiper: f64,
}

impl Potentiometer {
    fn segments(&self) -> (f64, f64) {
        (
            (self.resistance * self.wiper).max(POT_MIN_SEGMENT),
            (self.resistance * (1.0 - self.wiper)).max(POT_MIN_SEGMENT),
        )
    }
}

impl Device for Potentiometer {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp_dc(&self, s: &mut Stamper) {
        let (r_aw, r_wb) = self.segments();
        s.conductance(self.a, self.w, 1.0 / r_aw);
        s.conductance(self.w, self.b, 1.0 / r_wb);
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.a, self.w), (self.w, self.b)]
    }
    fn current(&self, sol: &SolutionView) -> f64 {
        let (r_aw, _) = self.segments();
        (sol.voltage(self.a) - sol.voltage(self.w)) / r_aw
    }
}

/// In DC a capacitor is an open circuit.
#[derive(Debug)]
pub struct Capacitor {
    pub name: String,
    pub p: NodeId,
    pub n: NodeId,
    pub capacitance: f64,
}

impl Device for Capacitor {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp_dc(&self, _s: &mut Stamper) {}
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        Vec::new()
    }
    fn current(&self, _sol: &SolutionView) -> f64 {
        0.0
    }
}

/// In DC an inductor is a short circuit; its current is a branch unknown.
#[derive(Debug)]
pub struct Inductor {
    pub name: String,
    pub p: NodeId,
    pub n: NodeId,
    pub inductance: f64,
    pub branch: Option<BranchId>,
}

impl Device for Inductor {
    fn designator(&self) -> &str {
        &self.name
    }
    fn branch_count(&self) -> usize {
        1
    }
    fn assign_branches(&mut self, first: BranchId) {
        self.branch = Some(first);
    }
    fn stamp_dc(&self, s: &mut Stamper) {
        s.voltage_source(self.p, self.n, first_branch(self.branch, &self.name), 0.0);
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.p, self.n)]
    }
    fn voltage_constraints(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.p, self.n)]
    }
    fn current(&self, sol: &SolutionView) -> f64 {
        sol.branch_current(first_branch(self.branch, &self.name))
    }
}

#[derive(Debug)]
pub struct VoltageSource {
    pub name: String,
    pub p: NodeId,
    pub n: NodeId,
    pub waveform: Waveform,
    pub branch: Option<BranchId>,
}

impl Device for VoltageSource {
    fn designator(&self) -> &str {
        &self.name
    }
    fn branch_count(&self) -> usize {
        1
    }
    fn assign_branches(&mut self, first: BranchId) {
        self.branch = Some(first);
    }
    fn stamp_dc(&self, s: &mut Stamper) {
        let k = first_branch(self.branch, &self.name);
        s.voltage_source(self.p, self.n, k, self.waveform.dc_value());
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.p, self.n)]
    }
    fn voltage_constraints(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.p, self.n)]
    }
    /// Current entering the + terminal (negative when the source delivers
    /// power), as in SPICE.
    fn current(&self, sol: &SolutionView) -> f64 {
        sol.branch_current(first_branch(self.branch, &self.name))
    }
}

#[derive(Debug)]
pub struct CurrentSource {
    pub name: String,
    /// Current enters the source here...
    pub input: NodeId,
    /// ...and leaves it here, into the circuit.
    pub output: NodeId,
    pub waveform: Waveform,
}

impl Device for CurrentSource {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp_dc(&self, s: &mut Stamper) {
        s.current(self.input, self.output, self.waveform.dc_value());
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        Vec::new()
    }
    fn current_injections(&self) -> Vec<NodeId> {
        vec![self.input, self.output]
    }
    fn current(&self, _sol: &SolutionView) -> f64 {
        self.waveform.dc_value()
    }
}

/// Pins shared by the four controlled sources.
#[derive(Debug, Clone, Copy)]
pub struct ControlledPins {
    pub out_p: NodeId,
    pub out_n: NodeId,
    pub ctrl_p: NodeId,
    pub ctrl_n: NodeId,
}

/// V(out+, out-) = gain * V(ctrl+, ctrl-).
#[derive(Debug)]
pub struct Vcvs {
    pub name: String,
    pub pins: ControlledPins,
    pub gain: f64,
    pub branch: Option<BranchId>,
}

impl Device for Vcvs {
    fn designator(&self) -> &str {
        &self.name
    }
    fn branch_count(&self) -> usize {
        1
    }
    fn assign_branches(&mut self, first: BranchId) {
        self.branch = Some(first);
    }
    fn stamp_dc(&self, s: &mut Stamper) {
        let k = first_branch(self.branch, &self.name);
        let p = self.pins;
        s.branch_incidence(p.out_p, p.out_n, k);
        s.branch_voltage_coeff(k, p.ctrl_p, -self.gain);
        s.branch_voltage_coeff(k, p.ctrl_n, self.gain);
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.pins.out_p, self.pins.out_n)]
    }
    fn voltage_constraints(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.pins.out_p, self.pins.out_n)]
    }
    fn current(&self, sol: &SolutionView) -> f64 {
        sol.branch_current(first_branch(self.branch, &self.name))
    }
}

/// Current gm * V(ctrl+, ctrl-) from out+ through the source to out-.
#[derive(Debug)]
pub struct Vccs {
    pub name: String,
    pub pins: ControlledPins,
    pub transconductance: f64,
}

impl Device for Vccs {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp_dc(&self, s: &mut Stamper) {
        let p = self.pins;
        s.transconductance(p.out_p, p.out_n, p.ctrl_p, p.ctrl_n, self.transconductance);
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        Vec::new()
    }
    fn current_injections(&self) -> Vec<NodeId> {
        vec![self.pins.out_p, self.pins.out_n]
    }
    fn current(&self, sol: &SolutionView) -> f64 {
        let p = self.pins;
        self.transconductance * (sol.voltage(p.ctrl_p) - sol.voltage(p.ctrl_n))
    }
}

/// V(out+, out-) = r * I(sensor), where the sensor is a 0 V source from
/// ctrl+ to ctrl-. Branches: [sensor, output].
#[derive(Debug)]
pub struct Ccvs {
    pub name: String,
    pub pins: ControlledPins,
    pub transresistance: f64,
    pub branch: Option<BranchId>,
}

impl Device for Ccvs {
    fn designator(&self) -> &str {
        &self.name
    }
    fn branch_count(&self) -> usize {
        2
    }
    fn assign_branches(&mut self, first: BranchId) {
        self.branch = Some(first);
    }
    fn stamp_dc(&self, s: &mut Stamper) {
        let sense = first_branch(self.branch, &self.name);
        let out = BranchId(sense.0 + 1);
        let p = self.pins;
        s.voltage_source(p.ctrl_p, p.ctrl_n, sense, 0.0);
        s.branch_incidence(p.out_p, p.out_n, out);
        s.branch_current_coeff(out, sense, -self.transresistance);
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![
            (self.pins.ctrl_p, self.pins.ctrl_n),
            (self.pins.out_p, self.pins.out_n),
        ]
    }
    fn voltage_constraints(&self) -> Vec<(NodeId, NodeId)> {
        self.dc_paths()
    }
    /// Output branch current (entering out+).
    fn current(&self, sol: &SolutionView) -> f64 {
        let sense = first_branch(self.branch, &self.name);
        sol.branch_current(BranchId(sense.0 + 1))
    }
}

/// Current gain * I(sensor) from out+ through the source to out-, where the
/// sensor is a 0 V source from ctrl+ to ctrl-.
#[derive(Debug)]
pub struct Cccs {
    pub name: String,
    pub pins: ControlledPins,
    pub gain: f64,
    pub branch: Option<BranchId>,
}

impl Device for Cccs {
    fn designator(&self) -> &str {
        &self.name
    }
    fn branch_count(&self) -> usize {
        1
    }
    fn assign_branches(&mut self, first: BranchId) {
        self.branch = Some(first);
    }
    fn stamp_dc(&self, s: &mut Stamper) {
        let sense = first_branch(self.branch, &self.name);
        let p = self.pins;
        s.voltage_source(p.ctrl_p, p.ctrl_n, sense, 0.0);
        s.current_controlled_current(p.out_p, p.out_n, sense, self.gain);
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.pins.ctrl_p, self.pins.ctrl_n)]
    }
    fn voltage_constraints(&self) -> Vec<(NodeId, NodeId)> {
        self.dc_paths()
    }
    fn current_injections(&self) -> Vec<NodeId> {
        vec![self.pins.out_p, self.pins.out_n]
    }
    /// Output current (from out+ through the source to out-).
    fn current(&self, sol: &SolutionView) -> f64 {
        let sense = first_branch(self.branch, &self.name);
        self.gain * sol.branch_current(sense)
    }
}
