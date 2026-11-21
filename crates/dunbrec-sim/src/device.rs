//! The [`Device`] trait and the linear device models.
//!
//! Every device knows how to write its equations into the MNA system for the
//! current analysis ([`StampContext`]) and how to report the current through
//! it. Topology information (`dc_paths`, `voltage_constraints`,
//! `current_injections`) lets the circuit builder catch unsolvable circuits
//! with a clear message before the solver fails.
//!
//! Reactive devices (capacitors, inductors) keep state between time steps.
//! State only changes in [`Device::accept_step`], so a rejected trial step
//! leaves the device untouched.

use dunbrec_core::{NodeId, Waveform};

use crate::ac::{AcContext, AcStamper, C64};
use crate::stamp::{BranchId, SolutionView, Stamper};
use crate::waveform;

/// Numerical integration method for one transient step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integration {
    /// First order, strongly damped. Used for the first step and after
    /// breakpoints, where trapezoidal integration would ring.
    BackwardEuler,
    /// Second order, the normal method.
    Trapezoidal,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// Operating point. With `initial_conditions`, capacitor initial voltages
    /// and inductor initial currents are enforced (start of a transient).
    Dc { initial_conditions: bool },
    /// One transient step of length `h` ending at [`StampContext::time`].
    Transient { h: f64, method: Integration },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StampContext {
    pub mode: Mode,
    /// Time of the solution being computed (0 for DC).
    pub time: f64,
    /// Extra conductance from every node to ground (gmin stepping).
    pub gmin: f64,
    /// Scale factor on all independent sources (source stepping).
    pub source_scale: f64,
}

impl StampContext {
    pub const DC: StampContext = StampContext::dc(false);

    pub const fn dc(initial_conditions: bool) -> Self {
        Self {
            mode: Mode::Dc { initial_conditions },
            time: 0.0,
            gmin: 0.0,
            source_scale: 1.0,
        }
    }

    pub const fn transient(h: f64, method: Integration, time: f64) -> Self {
        Self {
            mode: Mode::Transient { h, method },
            time,
            gmin: 0.0,
            source_scale: 1.0,
        }
    }
}

/// Error tolerances for timestep control (SPICE names and defaults).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tolerances {
    /// Relative tolerance.
    pub reltol: f64,
    /// Absolute voltage tolerance (V).
    pub vntol: f64,
    /// Absolute current tolerance (A).
    pub abstol: f64,
    /// Multiplier on the truncation error tolerance. SPICE defaults to 7;
    /// Dunbrec uses 1, taking about twice as many steps in exchange for
    /// waveforms that stay in phase over many cycles (an RLC ring drifts by
    /// ~2% after four periods with 7, well under 1% with 1).
    pub trtol: f64,
}

impl Default for Tolerances {
    fn default() -> Self {
        Self {
            reltol: 1e-3,
            vntol: 1e-6,
            abstol: 1e-12,
            trtol: 1.0,
        }
    }
}

pub trait Device: Send + std::fmt::Debug {
    fn designator(&self) -> &str;

    /// Number of branch-current unknowns this device needs.
    fn branch_count(&self) -> usize {
        0
    }

    /// Receives the id of the first of this device's branches.
    fn assign_branches(&mut self, _first: BranchId) {}

    /// Names of internal nodes this device needs (e.g. the point between a
    /// diode's junction and its series resistance).
    fn internal_nodes(&self) -> Vec<String> {
        Vec::new()
    }

    /// Receives the id of the first of this device's internal nodes; the
    /// rest follow consecutively.
    fn assign_internal_nodes(&mut self, _first: NodeId) {}

    /// True if the device's equations depend on the solution, requiring
    /// Newton-Raphson iteration.
    fn is_nonlinear(&self) -> bool {
        false
    }

    /// True if the last [`stamp`](Self::stamp) had to limit a junction
    /// voltage, meaning the iteration has not converged yet.
    fn limited(&self) -> bool {
        false
    }

    /// Forget per-iteration state (e.g. before retrying a solve from
    /// scratch).
    fn reset_iteration(&self) {}

    /// Set the level a digital output drives, ramping from `time`
    /// (`f64::INFINITY` means at once, for the operating point). Only
    /// digital output drivers respond.
    fn drive(&self, _time: f64, _level: f64) {}

    /// Set what a microcontroller pin does, from `time` (`f64::INFINITY`
    /// means at once). Only microcontroller pins respond.
    fn drive_pin(&self, _time: f64, _drive: dunbrec_mcu::PinDrive) {}

    /// Write this device's equations for the given analysis.
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext);

    /// Write the small-signal (AC) equations, linearised at the DC
    /// operating point `ctx.op`.
    fn stamp_ac(&self, s: &mut AcStamper, ctx: &AcContext);

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

    /// Current entering the device at its first pin, given the latest
    /// solution (for reactive devices, the latest *accepted* solution).
    fn current(&self, sol: &SolutionView) -> f64;

    /// Initialise transient state from the initial operating point.
    fn begin_transient(&mut self, _sol: &SolutionView, _time: f64) {}

    /// A trial step was accepted: update internal state. `restart` is true on
    /// the first step after a breakpoint, when history should be discarded.
    fn accept_step(&mut self, _sol: &SolutionView, _ctx: &StampContext, _restart: bool) {}

    /// Largest step this device would accept, based on the local truncation
    /// error of the trial solution `sol`. `None` means no limit.
    fn truncation_step(
        &self,
        _sol: &SolutionView,
        _ctx: &StampContext,
        _tol: &Tolerances,
    ) -> Option<f64> {
        None
    }

    /// Earliest time strictly after `after` at which this device's behaviour
    /// changes abruptly (e.g. a pulse edge). The simulator lands a step
    /// exactly there.
    fn next_breakpoint(&self, _after: f64) -> Option<f64> {
        None
    }

    /// Upper bound on the step size this device needs to be drawn faithfully
    /// (e.g. a fraction of a sine period).
    fn max_step(&self) -> Option<f64> {
        None
    }

    /// True if the last accepted steps show trapezoidal ringing (see
    /// [`RingDetector`]); the scheduler then takes one damped step.
    fn ringing(&self) -> bool {
        false
    }
}

/// Detects trapezoidal "ringing": a reactive current (or inductor voltage)
/// that flips sign at nearly the same magnitude on consecutive steps while
/// the stored quantity barely changes. This is a numerical artefact of the
/// trapezoidal rule after an abrupt change (e.g. a diode snapping off). A
/// genuine oscillation crosses zero once per half cycle, never on
/// consecutive steps, so it is not mistaken for ringing.
#[derive(Debug, Clone, Default)]
pub struct RingDetector {
    last: Option<f64>,
    flips: u8,
}

impl RingDetector {
    /// Consecutive near-symmetric sign flips that count as ringing.
    const FLIPS: u8 = 2;

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Record the latest value; true if it is ringing. Values smaller than
    /// `floor` are ignored as noise.
    pub fn push(&mut self, x: f64, floor: f64) -> bool {
        let flip = self
            .last
            .is_some_and(|p| p * x < 0.0 && x.abs() > floor && (x + p).abs() < 0.1 * (x - p).abs());
        self.flips = if flip {
            self.flips.saturating_add(1)
        } else {
            0
        };
        self.last = Some(x);
        self.flips >= Self::FLIPS
    }
}

fn first_branch(b: Option<BranchId>, name: &str) -> BranchId {
    b.unwrap_or_else(|| panic!("{name}: branches not assigned"))
}

/// The last few accepted values of a reactive quantity, used to estimate
/// local truncation error from divided differences.
#[derive(Debug, Clone, Default)]
pub struct History {
    points: Vec<(f64, f64)>,
}

impl History {
    const DEPTH: usize = 3;

    pub fn clear(&mut self) {
        self.points.clear();
    }

    pub fn push(&mut self, t: f64, x: f64) {
        if self.points.len() == Self::DEPTH {
            self.points.remove(0);
        }
        self.points.push((t, x));
    }

    pub fn last(&self) -> Option<(f64, f64)> {
        self.points.last().copied()
    }

    /// Suggested step size for a trial value `x_new` at `t_new`, reached with
    /// a step of `h` using `method`, so that the truncation error stays
    /// within `tol`. `None` if there is not enough history to estimate.
    pub fn suggest_step(
        &self,
        t_new: f64,
        x_new: f64,
        h: f64,
        method: Integration,
        tol: f64,
    ) -> Option<f64> {
        // Backward Euler error ~ h^2/2 * x'', trapezoidal ~ h^3/12 * x'''.
        let (needed, order, coeff, factorial) = match method {
            Integration::BackwardEuler => (2, 2, 0.5, 2.0),
            Integration::Trapezoidal => (3, 3, 1.0 / 12.0, 6.0),
        };
        if self.points.len() < needed {
            return None;
        }
        let mut pts: Vec<(f64, f64)> = self.points[self.points.len() - needed..].to_vec();
        pts.push((t_new, x_new));
        let derivative = factorial * divided_difference(&pts);
        let error = coeff * h.powi(order) * derivative.abs();
        if error <= 0.0 || !error.is_finite() {
            return None;
        }
        Some(h * (tol / error).powf(1.0 / order as f64))
    }
}

/// Highest-order divided difference of the given points.
fn divided_difference(pts: &[(f64, f64)]) -> f64 {
    let mut values: Vec<f64> = pts.iter().map(|p| p.1).collect();
    let n = pts.len();
    for level in 1..n {
        for i in 0..n - level {
            let dt = pts[i + level].0 - pts[i].0;
            values[i] = (values[i + 1] - values[i]) / dt;
        }
    }
    values[0]
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
    fn stamp_ac(&self, s: &mut AcStamper, _ctx: &AcContext) {
        s.conductance(self.p, self.n, 1.0 / self.resistance);
    }
    fn stamp(&self, s: &mut Stamper, _ctx: &StampContext) {
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
    fn stamp_ac(&self, s: &mut AcStamper, _ctx: &AcContext) {
        let (r_aw, r_wb) = self.segments();
        s.conductance(self.a, self.w, 1.0 / r_aw);
        s.conductance(self.w, self.b, 1.0 / r_wb);
    }
    fn stamp(&self, s: &mut Stamper, _ctx: &StampContext) {
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

/// Conductance used to hold an initial condition (a 1 nΩ Norton source).
const IC_CONDUCTANCE: f64 = 1e9;
/// Ring detection ignores currents / voltages smaller than these.
pub const RING_CURRENT_FLOOR: f64 = 1e-9;
pub const RING_VOLTAGE_FLOOR: f64 = 1e-6;

/// Open in DC. In transient, replaced each step by a companion model: a
/// conductance in parallel with a current source representing the charge
/// already stored.
#[derive(Debug)]
pub struct Capacitor {
    pub name: String,
    pub p: NodeId,
    pub n: NodeId,
    pub capacitance: f64,
    pub initial_voltage: Option<f64>,
    /// Voltage and current at the last accepted time point.
    pub v_prev: f64,
    pub i_prev: f64,
    pub history: History,
    ring: RingDetector,
    ringing: bool,
}

impl Capacitor {
    pub fn new(
        name: String,
        p: NodeId,
        n: NodeId,
        capacitance: f64,
        initial_voltage: Option<f64>,
    ) -> Self {
        Self {
            name,
            p,
            n,
            capacitance,
            initial_voltage,
            v_prev: 0.0,
            i_prev: 0.0,
            history: History::default(),
            ring: RingDetector::default(),
            ringing: false,
        }
    }

    /// Companion model (conductance, constant current p→n) for a step.
    fn companion(&self, h: f64, method: Integration) -> (f64, f64) {
        match method {
            Integration::BackwardEuler => {
                let g = self.capacitance / h;
                (g, -g * self.v_prev)
            }
            Integration::Trapezoidal => {
                let g = 2.0 * self.capacitance / h;
                (g, -g * self.v_prev - self.i_prev)
            }
        }
    }

    fn voltage(&self, sol: &SolutionView) -> f64 {
        sol.voltage(self.p) - sol.voltage(self.n)
    }
}

impl Device for Capacitor {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp_ac(&self, s: &mut AcStamper, ctx: &AcContext) {
        s.capacitance(self.p, self.n, self.capacitance, ctx.omega);
    }
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext) {
        match ctx.mode {
            Mode::Dc { initial_conditions } => {
                if let (true, Some(v0)) = (initial_conditions, self.initial_voltage) {
                    s.conductance(self.p, self.n, IC_CONDUCTANCE);
                    s.current(self.p, self.n, -IC_CONDUCTANCE * v0);
                }
            }
            Mode::Transient { h, method } => {
                let (g, i_eq) = self.companion(h, method);
                s.conductance(self.p, self.n, g);
                s.current(self.p, self.n, i_eq);
            }
        }
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        Vec::new()
    }
    fn current(&self, _sol: &SolutionView) -> f64 {
        self.i_prev
    }
    fn begin_transient(&mut self, sol: &SolutionView, time: f64) {
        self.v_prev = self.voltage(sol);
        self.i_prev = 0.0;
        self.history.clear();
        self.history.push(time, self.v_prev);
        self.ring.reset();
        self.ringing = false;
    }
    fn accept_step(&mut self, sol: &SolutionView, ctx: &StampContext, restart: bool) {
        let Mode::Transient { h, method } = ctx.mode else {
            return;
        };
        let v = self.voltage(sol);
        let (g, i_eq) = self.companion(h, method);
        self.i_prev = g * v + i_eq;
        self.ringing = self.ring.push(self.i_prev, RING_CURRENT_FLOOR);
        if restart {
            self.history.clear();
            self.history.push(ctx.time - h, self.v_prev);
        }
        self.v_prev = v;
        self.history.push(ctx.time, v);
    }
    fn ringing(&self) -> bool {
        self.ringing
    }
    fn truncation_step(
        &self,
        sol: &SolutionView,
        ctx: &StampContext,
        tol: &Tolerances,
    ) -> Option<f64> {
        let Mode::Transient { h, method } = ctx.mode else {
            return None;
        };
        let v = self.voltage(sol);
        let limit = tol.trtol * (tol.reltol * v.abs().max(self.v_prev.abs()) + tol.vntol);
        self.history.suggest_step(ctx.time, v, h, method, limit)
    }
}

/// A short in DC (its current is a branch unknown). In transient, the branch
/// equation becomes the inductor's companion model.
#[derive(Debug)]
pub struct Inductor {
    pub name: String,
    pub p: NodeId,
    pub n: NodeId,
    pub inductance: f64,
    pub initial_current: Option<f64>,
    pub branch: Option<BranchId>,
    pub i_prev: f64,
    pub v_prev: f64,
    pub history: History,
    ring: RingDetector,
    ringing: bool,
}

impl Inductor {
    pub fn new(
        name: String,
        p: NodeId,
        n: NodeId,
        inductance: f64,
        initial_current: Option<f64>,
    ) -> Self {
        Self {
            name,
            p,
            n,
            inductance,
            initial_current,
            branch: None,
            i_prev: 0.0,
            v_prev: 0.0,
            history: History::default(),
            ring: RingDetector::default(),
            ringing: false,
        }
    }

    fn k(&self) -> BranchId {
        first_branch(self.branch, &self.name)
    }
}

impl Device for Inductor {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp_ac(&self, s: &mut AcStamper, ctx: &AcContext) {
        // V(p) - V(n) - jωL·I = 0
        let k = self.k();
        s.branch_incidence(self.p, self.n, k);
        s.branch_current_coeff(k, k, C64::new(0.0, -ctx.omega * self.inductance));
    }
    fn branch_count(&self) -> usize {
        1
    }
    fn assign_branches(&mut self, first: BranchId) {
        self.branch = Some(first);
    }
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext) {
        let k = self.k();
        match ctx.mode {
            Mode::Dc { initial_conditions } => match (initial_conditions, self.initial_current) {
                // Forced current: I(k) = i0.
                (true, Some(i0)) => {
                    s.branch_kcl(self.p, self.n, k);
                    s.branch_current_coeff(k, k, 1.0);
                    s.branch_rhs(k, i0);
                }
                _ => s.voltage_source(self.p, self.n, k, 0.0),
            },
            // v = L di/dt:
            //   BE:   V(p)-V(n) - (L/h) I = -(L/h) I_prev
            //   trap: V(p)-V(n) - (2L/h) I = -(2L/h) I_prev - V_prev
            Mode::Transient { h, method } => {
                s.branch_incidence(self.p, self.n, k);
                let (r, rhs) = match method {
                    Integration::BackwardEuler => {
                        let r = self.inductance / h;
                        (r, -r * self.i_prev)
                    }
                    Integration::Trapezoidal => {
                        let r = 2.0 * self.inductance / h;
                        (r, -r * self.i_prev - self.v_prev)
                    }
                };
                s.branch_current_coeff(k, k, -r);
                s.branch_rhs(k, rhs);
            }
        }
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.p, self.n)]
    }
    fn voltage_constraints(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.p, self.n)]
    }
    fn current(&self, sol: &SolutionView) -> f64 {
        sol.branch_current(self.k())
    }
    fn begin_transient(&mut self, sol: &SolutionView, time: f64) {
        self.i_prev = sol.branch_current(self.k());
        self.v_prev = sol.voltage(self.p) - sol.voltage(self.n);
        self.history.clear();
        self.history.push(time, self.i_prev);
        self.ring.reset();
        self.ringing = false;
    }
    fn accept_step(&mut self, sol: &SolutionView, ctx: &StampContext, restart: bool) {
        let Mode::Transient { h, .. } = ctx.mode else {
            return;
        };
        if restart {
            self.history.clear();
            self.history.push(ctx.time - h, self.i_prev);
        }
        self.i_prev = sol.branch_current(self.k());
        self.v_prev = sol.voltage(self.p) - sol.voltage(self.n);
        self.history.push(ctx.time, self.i_prev);
        self.ringing = self.ring.push(self.v_prev, RING_VOLTAGE_FLOOR);
    }
    fn ringing(&self) -> bool {
        self.ringing
    }
    fn truncation_step(
        &self,
        sol: &SolutionView,
        ctx: &StampContext,
        tol: &Tolerances,
    ) -> Option<f64> {
        let Mode::Transient { h, method } = ctx.mode else {
            return None;
        };
        let i = sol.branch_current(self.k());
        let limit = tol.trtol * (tol.reltol * i.abs().max(self.i_prev.abs()) + tol.abstol);
        self.history.suggest_step(ctx.time, i, h, method, limit)
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
    fn stamp_ac(&self, s: &mut AcStamper, ctx: &AcContext) {
        let k = first_branch(self.branch, &self.name);
        let v = if ctx.excited { 1.0 } else { 0.0 };
        s.voltage_source(self.p, self.n, k, C64::new(v, 0.0));
    }
    fn branch_count(&self) -> usize {
        1
    }
    fn assign_branches(&mut self, first: BranchId) {
        self.branch = Some(first);
    }
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext) {
        let k = first_branch(self.branch, &self.name);
        let v = self.waveform.value_at(ctx.time) * ctx.source_scale;
        s.voltage_source(self.p, self.n, k, v);
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
    fn next_breakpoint(&self, after: f64) -> Option<f64> {
        waveform::next_breakpoint(&self.waveform, after)
    }
    fn max_step(&self) -> Option<f64> {
        waveform::max_step(&self.waveform)
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
    /// Time of the latest solution, so `current` reports the right value.
    pub time: f64,
}

impl Device for CurrentSource {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp_ac(&self, s: &mut AcStamper, ctx: &AcContext) {
        if ctx.excited {
            s.current(self.input, self.output, C64::new(1.0, 0.0));
        }
    }
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext) {
        let i = self.waveform.value_at(ctx.time) * ctx.source_scale;
        s.current(self.input, self.output, i);
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        Vec::new()
    }
    fn current_injections(&self) -> Vec<NodeId> {
        vec![self.input, self.output]
    }
    fn current(&self, _sol: &SolutionView) -> f64 {
        self.waveform.value_at(self.time)
    }
    fn begin_transient(&mut self, _sol: &SolutionView, time: f64) {
        self.time = time;
    }
    fn accept_step(&mut self, _sol: &SolutionView, ctx: &StampContext, _restart: bool) {
        self.time = ctx.time;
    }
    fn next_breakpoint(&self, after: f64) -> Option<f64> {
        waveform::next_breakpoint(&self.waveform, after)
    }
    fn max_step(&self) -> Option<f64> {
        waveform::max_step(&self.waveform)
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
    fn stamp_ac(&self, s: &mut AcStamper, _ctx: &AcContext) {
        let k = first_branch(self.branch, &self.name);
        let p = self.pins;
        s.branch_incidence(p.out_p, p.out_n, k);
        s.branch_voltage_coeff(k, p.ctrl_p, C64::new(-self.gain, 0.0));
        s.branch_voltage_coeff(k, p.ctrl_n, C64::new(self.gain, 0.0));
    }
    fn branch_count(&self) -> usize {
        1
    }
    fn assign_branches(&mut self, first: BranchId) {
        self.branch = Some(first);
    }
    fn stamp(&self, s: &mut Stamper, _ctx: &StampContext) {
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
    fn stamp_ac(&self, s: &mut AcStamper, _ctx: &AcContext) {
        let p = self.pins;
        s.transconductance(p.out_p, p.out_n, p.ctrl_p, p.ctrl_n, self.transconductance);
    }
    fn stamp(&self, s: &mut Stamper, _ctx: &StampContext) {
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
    fn stamp_ac(&self, s: &mut AcStamper, _ctx: &AcContext) {
        let sense = first_branch(self.branch, &self.name);
        let out = BranchId(sense.0 + 1);
        let p = self.pins;
        s.voltage_source(p.ctrl_p, p.ctrl_n, sense, C64::new(0.0, 0.0));
        s.branch_incidence(p.out_p, p.out_n, out);
        s.branch_current_coeff(out, sense, C64::new(-self.transresistance, 0.0));
    }
    fn branch_count(&self) -> usize {
        2
    }
    fn assign_branches(&mut self, first: BranchId) {
        self.branch = Some(first);
    }
    fn stamp(&self, s: &mut Stamper, _ctx: &StampContext) {
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
    fn stamp_ac(&self, s: &mut AcStamper, _ctx: &AcContext) {
        let sense = first_branch(self.branch, &self.name);
        let p = self.pins;
        s.voltage_source(p.ctrl_p, p.ctrl_n, sense, C64::new(0.0, 0.0));
        s.current_controlled_current(p.out_p, p.out_n, sense, self.gain);
    }
    fn branch_count(&self) -> usize {
        1
    }
    fn assign_branches(&mut self, first: BranchId) {
        self.branch = Some(first);
    }
    fn stamp(&self, s: &mut Stamper, _ctx: &StampContext) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn divided_differences_recover_derivatives() {
        // x = t^3: third derivative is 6 everywhere.
        let pts: Vec<(f64, f64)> = [0.0, 0.1, 0.3, 0.6]
            .iter()
            .map(|&t| (t, t * t * t))
            .collect();
        assert_relative_eq!(6.0 * divided_difference(&pts), 6.0, epsilon = 1e-9);
        // x = t^2: second derivative 2.
        let pts: Vec<(f64, f64)> = [1.0, 1.5, 2.5].iter().map(|&t| (t, t * t)).collect();
        assert_relative_eq!(2.0 * divided_difference(&pts), 2.0, epsilon = 1e-9);
    }

    #[test]
    fn ring_detector_flags_alternation_but_not_oscillation() {
        // Trapezoidal ringing: same magnitude, flipping sign every step.
        let mut d = RingDetector::default();
        let flags: Vec<bool> = [5e-3, -5e-3, 5e-3, -5e-3]
            .iter()
            .map(|&x| d.push(x, 1e-9))
            .collect();
        assert_eq!(flags, vec![false, false, true, true]);

        // A sampled sine (50 points per period) crosses zero cleanly.
        let mut d = RingDetector::default();
        let any = (0..500)
            .map(|k| (std::f64::consts::TAU * k as f64 / 50.0 + 0.01).sin())
            .any(|x| d.push(x, 1e-9));
        assert!(!any);

        // Noise below the floor is ignored.
        let mut d = RingDetector::default();
        assert!(
            ![1e-12, -1e-12, 1e-12, -1e-12]
                .iter()
                .any(|&x| d.push(x, 1e-9))
        );
    }

    #[test]
    fn history_suggests_smaller_step_for_larger_error() {
        let mut h = History::default();
        for t in [0.0, 1.0, 2.0] {
            h.push(t, t * t * t);
        }
        let big = h
            .suggest_step(3.0, 27.0, 1.0, Integration::Trapezoidal, 1e-3)
            .unwrap();
        let small = h
            .suggest_step(3.0, 27.0, 1.0, Integration::Trapezoidal, 1e-6)
            .unwrap();
        assert!(small < big);
        // Error = h^3/12 * 6 = 0.5; tol 1e-3 -> h * (1e-3/0.5)^(1/3)
        assert_relative_eq!(big, (1e-3_f64 / 0.5).powf(1.0 / 3.0), epsilon = 1e-9);
        // Linear data: no error, no limit.
        let mut lin = History::default();
        for t in [0.0, 1.0, 2.0] {
            lin.push(t, 2.0 * t);
        }
        assert_eq!(
            lin.suggest_step(3.0, 6.0, 1.0, Integration::Trapezoidal, 1e-3),
            None
        );
    }
}
