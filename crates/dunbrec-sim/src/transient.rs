//! Transient (time-domain) analysis and the scheduler that owns simulation
//! time.
//!
//! [`Transient`] advances one accepted time point per call to
//! [`Transient::step`], so a UI can run it incrementally and pace it against
//! wall-clock time. [`transient`] runs it to completion and collects
//! waveforms.
//!
//! Each step:
//! 1. picks a step size: the previous suggestion, capped by the maximum
//!    step and shortened to land exactly on the next breakpoint;
//! 2. solves the circuit at the new time (Backward Euler on the first step
//!    after a breakpoint, trapezoidal otherwise);
//! 3. rejects and retries with a smaller step if any device's truncation
//!    error is too large, or if a watched threshold was crossed too far
//!    inside the step;
//! 4. accepts: devices update their state and crossing events are reported.
//!
//! Watches are the hook mixed-signal simulation will use: a digital input is
//! a watch on an analog node, and the step is cut so the crossing lands
//! accurately.

use dunbrec_core::NodeId;

use crate::circuit::Circuit;
use crate::dc::OperatingPoint;
use crate::device::{Integration, StampContext, Tolerances};
use crate::digital::{CMOS_5V, DigitalSystem, Logic, settle_dc};
use crate::error::SimError;
use crate::newton::{TRANSIENT_MAX_ITERATIONS, Workspace, dc_solve, newton};
use crate::stamp::{SolutionView, node_rows};

/// Default number of steps the maximum step size divides the run into.
pub const DEFAULT_STEPS: f64 = 50.0;
/// Most attempts at one time point before giving up.
const MAX_ATTEMPTS: usize = 64;
/// A step is rejected when the error-based suggestion is below this
/// fraction of the step just taken.
const REJECT_RATIO: f64 = 0.9;
/// Largest growth of the step size from one step to the next.
const MAX_GROWTH: f64 = 2.0;
/// Step size after a breakpoint, as a fraction of the step before it.
const BREAKPOINT_SHRINK: f64 = 0.1;

#[derive(Debug, Clone, PartialEq)]
pub struct TransientOptions {
    /// Simulation end time (s). May be infinite for a free-running
    /// simulation (see [`TransientOptions::free_running`]).
    pub stop_time: f64,
    /// Largest step size (s). Defaults to `stop_time / 50`, further limited
    /// by sources (e.g. 50 steps per sine period).
    pub max_step: Option<f64>,
    pub tolerances: Tolerances,
}

impl TransientOptions {
    pub fn new(stop_time: f64) -> Self {
        Self {
            stop_time,
            max_step: None,
            tolerances: Tolerances::default(),
        }
    }

    pub fn with_max_step(mut self, max_step: f64) -> Self {
        self.max_step = Some(max_step);
        self
    }

    /// Run indefinitely (until the caller stops stepping), never taking a
    /// step longer than `max_step`. Used for live simulation.
    pub fn free_running(max_step: f64) -> Self {
        Self {
            stop_time: f64::INFINITY,
            max_step: Some(max_step),
            tolerances: Tolerances::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Rising,
    Falling,
    Either,
}

/// Report when `V(p) - V(n)` crosses `level`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Watch {
    pub p: NodeId,
    pub n: NodeId,
    pub level: f64,
    pub edge: Edge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WatchId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossing {
    pub watch: WatchId,
    /// Interpolated crossing time (s).
    pub time: f64,
    pub edge: Edge,
}

/// One accepted time point.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub time: f64,
    /// Voltage of every node, indexed by [`NodeId`] (ground is 0).
    pub voltages: Vec<f64>,
    /// Current entering each device's first pin, in device order.
    pub currents: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StepResult {
    pub sample: Sample,
    pub crossings: Vec<Crossing>,
}

pub struct Transient {
    circuit: Circuit,
    options: TransientOptions,
    initial: OperatingPoint,
    time: f64,
    /// Latest accepted solution.
    x: Vec<f64>,
    trial: Vec<f64>,
    workspace: Workspace,
    h_next: f64,
    h_max: f64,
    h_min: f64,
    /// True when the next step starts at a breakpoint.
    restart: bool,
    /// True when a device reported trapezoidal ringing: the next step uses
    /// Backward Euler, whose damping removes the artefact.
    damp: bool,
    watches: Vec<Watch>,
    finished: bool,
    /// Time scale of the run, used for absolute time tolerances: the stop
    /// time, or for a free-running simulation a multiple of the max step.
    span: f64,
    /// The circuit's logic, if it has any.
    digital: Option<DigitalSystem>,
    /// For each watch: the digital input it feeds (part, input, level it
    /// signals), or `None` for a watch added by the caller.
    watch_targets: Vec<Option<(usize, usize, Logic)>>,
}

impl Transient {
    /// Compute the initial operating point (honouring capacitor and inductor
    /// initial conditions) and prepare to step.
    pub fn new(mut circuit: Circuit, options: TransientOptions) -> Result<Self, SimError> {
        if options.stop_time.is_nan() || options.stop_time <= 0.0 {
            return Err(SimError::InvalidOptions(
                "stop time must be greater than zero".into(),
            ));
        }
        if options.stop_time.is_infinite() && options.max_step.is_none() {
            return Err(SimError::InvalidOptions(
                "a free-running simulation needs a maximum step".into(),
            ));
        }
        if let Some(m) = options.max_step
            && !(m.is_finite() && m > 0.0)
        {
            return Err(SimError::InvalidOptions(
                "maximum step must be greater than zero".into(),
            ));
        }

        let size = circuit.unknown_count();
        let mut workspace = Workspace::new(size);
        let mut digital =
            (!circuit.digital.is_empty()).then(|| DigitalSystem::new(&circuit.digital, CMOS_5V));
        let x = match &mut digital {
            Some(sys) => settle_dc(&circuit, sys, &mut workspace, &options.tolerances, true)?,
            None => dc_solve(&circuit, true, &mut workspace, &options.tolerances)?,
        };
        let initial = OperatingPoint::from_solution(&circuit, &x);

        let rows = node_rows(circuit.node_count());
        let sol = SolutionView {
            x: &x,
            node_rows: rows,
        };
        for d in &mut circuit.devices {
            d.begin_transient(&sol, 0.0);
        }

        let stop = options.stop_time;
        let h_max = Self::max_step_for(&circuit, &options);
        let span = if stop.is_finite() {
            stop
        } else {
            h_max * DEFAULT_STEPS
        };
        let h_min = span * 1e-14;

        // Digital inputs watch their nets: up through VIH, down through VIL.
        let mut watches = Vec::new();
        let mut watch_targets = Vec::new();
        if let Some(sys) = &mut digital {
            sys.sync_inputs(|n| sol.voltage(n));
            let f = sys.family;
            for (part, input, node) in sys.inputs() {
                for (level, edge, value) in [
                    (f.vih, Edge::Rising, Logic::High),
                    (f.vil, Edge::Falling, Logic::Low),
                ] {
                    watches.push(Watch {
                        p: node,
                        n: NodeId::GROUND,
                        level,
                        edge,
                    });
                    watch_targets.push(Some((part, input, value)));
                }
            }
            sys.start(0.0);
        }

        Ok(Self {
            circuit,
            initial,
            time: 0.0,
            trial: vec![0.0; size],
            x,
            workspace,
            h_next: h_max * 1e-3,
            h_max,
            h_min,
            restart: true,
            damp: false,
            watches,
            finished: false,
            span,
            digital,
            watch_targets,
            options,
        })
    }

    fn max_step_for(circuit: &Circuit, options: &TransientOptions) -> f64 {
        circuit
            .devices
            .iter()
            .filter_map(|d| d.max_step())
            .chain(options.max_step)
            .fold(options.stop_time / DEFAULT_STEPS, f64::min)
    }

    /// Change the largest allowed step (e.g. when the oscilloscope
    /// timebase changes). Sources may still impose a smaller limit.
    pub fn set_max_step(&mut self, max_step: f64) -> Result<(), SimError> {
        if !(max_step.is_finite() && max_step > 0.0) {
            return Err(SimError::InvalidOptions(
                "maximum step must be greater than zero".into(),
            ));
        }
        self.options.max_step = Some(max_step);
        self.h_max = Self::max_step_for(&self.circuit, &self.options);
        self.h_next = self.h_next.min(self.h_max);
        Ok(())
    }

    /// Replace one element's parameters while the simulation runs (a
    /// potentiometer turned, a switch flipped, a source changed). `index`
    /// is the element's position in the netlist the circuit was built from.
    ///
    /// The device is rebuilt and its state re-initialised from the present
    /// solution, and the next step is treated like a breakpoint. Returns
    /// `Ok(false)` without changing anything if the new parameters change
    /// the circuit's structure (e.g. a model gaining a series resistance);
    /// the caller must then restart the simulation.
    pub fn update_element(
        &mut self,
        index: usize,
        element: &dunbrec_core::Element,
        models: &dunbrec_core::ModelLibrary,
    ) -> Result<bool, SimError> {
        if !self.circuit.replace_device(index, element, models)? {
            return Ok(false);
        }
        let sol = SolutionView {
            x: &self.x,
            node_rows: node_rows(self.circuit.node_count()),
        };
        self.circuit.devices[index].begin_transient(&sol, self.time);
        self.h_max = Self::max_step_for(&self.circuit, &self.options);
        self.h_next = self.h_next.min(self.h_max);
        self.restart = true;
        if let Some(sys) = &mut self.digital
            && let Some(part) = sys.part_of_element(index)
        {
            sys.update_part(part, &element.kind, self.time);
            self.run_digital();
        }
        Ok(true)
    }

    /// The operating point the transient started from.
    pub fn initial_point(&self) -> &OperatingPoint {
        &self.initial
    }

    pub fn circuit(&self) -> &Circuit {
        &self.circuit
    }

    pub fn time(&self) -> f64 {
        self.time
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn add_watch(&mut self, watch: Watch) -> WatchId {
        self.watches.push(watch);
        self.watch_targets.push(None);
        WatchId(self.watches.len() - 1)
    }

    /// Fire due digital events and update the output drivers.
    fn run_digital(&mut self) {
        let Some(sys) = &mut self.digital else { return };
        let changes = sys.advance(self.time);
        let family = sys.family;
        for (driver, level) in &changes {
            self.circuit.devices[*driver].drive(self.time, level.voltage(&family));
        }
        if !changes.is_empty() {
            // An output starts to ramp: a kink, like a breakpoint.
            self.restart = true;
        }
    }

    /// The latest accepted time point.
    pub fn sample(&self) -> Sample {
        let sol = self.view(&self.x);
        Sample {
            time: self.time,
            voltages: (0..self.circuit.node_count())
                .map(|i| sol.voltage(NodeId(i)))
                .collect(),
            currents: self
                .circuit
                .devices
                .iter()
                .map(|d| d.current(&sol))
                .collect(),
        }
    }

    fn view<'a>(&self, x: &'a [f64]) -> SolutionView<'a> {
        SolutionView {
            x,
            node_rows: node_rows(self.circuit.node_count()),
        }
    }

    fn next_breakpoint(&self) -> f64 {
        let digital = self
            .digital
            .as_ref()
            .and_then(|s| s.next_event_time())
            .filter(|&t| t > self.time);
        self.circuit
            .devices
            .iter()
            .filter_map(|d| d.next_breakpoint(self.time))
            .chain(digital)
            .fold(self.options.stop_time, f64::min)
    }

    /// Advance to the next accepted time point. Returns `None` once the stop
    /// time has been reached.
    pub fn step(&mut self) -> Result<Option<StepResult>, SimError> {
        if self.finished {
            return Ok(None);
        }
        let target = self.next_breakpoint();
        if target - self.time <= self.h_min * 10.0 {
            // The target is within rounding of now (e.g. a clock edge that
            // accumulated rounding over many periods): treat it as reached.
            self.time = target;
            self.restart = true;
            self.finished = target >= self.options.stop_time;
            self.run_digital();
            return Ok(Some(StepResult {
                sample: self.sample(),
                crossings: Vec::new(),
            }));
        }
        let mut h = self.h_next.min(self.h_max);
        // Land exactly on the target; stretch slightly rather than leave a
        // sliver of a step before it.
        if self.time + h * 1.25 >= target {
            h = target - self.time;
        }
        let method = if self.restart || self.damp {
            Integration::BackwardEuler
        } else {
            Integration::Trapezoidal
        };
        let crossing_tol = (self.span * 1e-9).max(self.h_min * 10.0);

        for _ in 0..MAX_ATTEMPTS {
            if h < self.h_min {
                break;
            }
            let t_new = self.time + h;
            let ctx = StampContext::transient(h, method, t_new);
            // Start Newton from the last accepted solution.
            self.trial.copy_from_slice(&self.x);
            match newton(
                &self.circuit,
                &ctx,
                &mut self.trial,
                &mut self.workspace,
                TRANSIENT_MAX_ITERATIONS,
                &self.options.tolerances,
            ) {
                Ok(_) => {}
                Err(SimError::NoConvergence { .. }) => {
                    // Too big a jump for the nonlinear devices: retry smaller.
                    h /= 8.0;
                    continue;
                }
                Err(other) => return Err(other),
            }

            // Truncation error. Skipped on the first step after a
            // breakpoint, where history spans the discontinuity.
            let sol = self.view(&self.trial);
            let h_error = if self.restart {
                f64::INFINITY
            } else {
                self.circuit
                    .devices
                    .iter()
                    .filter_map(|d| d.truncation_step(&sol, &ctx, &self.options.tolerances))
                    .fold(f64::INFINITY, f64::min)
            };
            if h_error < REJECT_RATIO * h {
                h = h_error.max(h / 8.0);
                continue;
            }

            // Threshold crossings: if the earliest crossing is well inside
            // the step, shorten the step to end just after it.
            let crossings = self.find_crossings();
            if let Some(first) = crossings.iter().map(|c| c.1).reduce(f64::min) {
                let to_end = h * (1.0 - first);
                if to_end > crossing_tol {
                    h = h * first + crossing_tol * 0.5;
                    continue;
                }
            }

            return Ok(Some(self.accept(h, h_error, ctx, crossings, target)));
        }
        Err(SimError::TimestepTooSmall { time: self.time })
    }

    /// (watch index, fraction of the step at which it crossed).
    fn find_crossings(&self) -> Vec<(usize, f64, Edge)> {
        let (old, new) = (self.view(&self.x), self.view(&self.trial));
        self.watches
            .iter()
            .enumerate()
            .filter_map(|(i, w)| {
                let f0 = old.voltage(w.p) - old.voltage(w.n) - w.level;
                let f1 = new.voltage(w.p) - new.voltage(w.n) - w.level;
                let edge = if f0 < 0.0 && f1 >= 0.0 {
                    Edge::Rising
                } else if f0 > 0.0 && f1 <= 0.0 {
                    Edge::Falling
                } else {
                    return None;
                };
                if w.edge != Edge::Either && w.edge != edge {
                    return None;
                }
                Some((i, f0 / (f0 - f1), edge))
            })
            .collect()
    }

    fn accept(
        &mut self,
        h: f64,
        h_error: f64,
        ctx: StampContext,
        crossings: Vec<(usize, f64, Edge)>,
        target: f64,
    ) -> StepResult {
        let start = self.time;
        {
            let sol = SolutionView {
                x: &self.trial,
                node_rows: node_rows(self.circuit.node_count()),
            };
            for d in &mut self.circuit.devices {
                d.accept_step(&sol, &ctx, self.restart);
            }
        }
        std::mem::swap(&mut self.x, &mut self.trial);
        // Crossings of digital input thresholds become input events; the
        // rest are reported to the caller.
        let mut reported = Vec::new();
        for (i, frac, edge) in crossings {
            let time = start + frac * h;
            match (self.watch_targets[i], &mut self.digital) {
                (Some((part, input, value)), Some(sys)) => {
                    sys.input_event(time, part, input, value)
                }
                _ => reported.push(Crossing {
                    watch: WatchId(i),
                    time,
                    edge,
                }),
            }
        }
        let crossings = reported;

        // An infinite target (free-running, no breakpoint ahead) is never hit.
        let hit_target = target.is_finite()
            && (start + h - target).abs() <= self.h_min.max(target.abs() * 1e-12);
        self.time = if hit_target { target } else { start + h };
        self.h_next = (h * MAX_GROWTH).min(h_error).min(self.h_max);
        self.restart = false;
        self.damp = self.circuit.devices.iter().any(|d| d.ringing());
        if hit_target {
            if target >= self.options.stop_time {
                self.finished = true;
            } else {
                self.restart = true;
                self.h_next = (h * BREAKPOINT_SHRINK).max(self.h_min * 10.0);
            }
        }
        self.run_digital();
        StepResult {
            sample: self.sample(),
            crossings,
        }
    }
}

/// Waveforms from a completed transient run.
#[derive(Debug, Clone, PartialEq)]
pub struct TransientResult {
    pub node_names: Vec<String>,
    pub device_names: Vec<String>,
    pub times: Vec<f64>,
    /// `voltages[node][sample]`, indexed by [`NodeId`].
    pub voltages: Vec<Vec<f64>>,
    /// `currents[device][sample]`, current entering the device's first pin.
    pub currents: Vec<Vec<f64>>,
    pub crossings: Vec<Crossing>,
}

impl TransientResult {
    pub fn voltage(&self, node: &str) -> Option<&[f64]> {
        let i = self.node_names.iter().position(|n| n == node)?;
        Some(&self.voltages[i])
    }

    pub fn current(&self, designator: &str) -> Option<&[f64]> {
        let i = self.device_names.iter().position(|n| n == designator)?;
        Some(&self.currents[i])
    }

    /// Linearly interpolated value of `trace` at time `t`.
    pub fn at(&self, trace: &[f64], t: f64) -> f64 {
        let i = self.times.partition_point(|&s| s < t);
        if i == 0 {
            return trace[0];
        }
        if i >= self.times.len() {
            return trace[trace.len() - 1];
        }
        let (t0, t1) = (self.times[i - 1], self.times[i]);
        let a = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
        trace[i - 1] + a * (trace[i] - trace[i - 1])
    }
}

/// Run a transient analysis to completion.
pub fn transient(circuit: Circuit, options: TransientOptions) -> Result<TransientResult, SimError> {
    run(Transient::new(circuit, options)?)
}

/// Run a prepared [`Transient`] (e.g. one with watches added) to completion.
pub fn run(mut sim: Transient) -> Result<TransientResult, SimError> {
    let first = sim.sample();
    let mut result = TransientResult {
        node_names: sim.circuit.node_names.clone(),
        device_names: sim
            .circuit
            .devices
            .iter()
            .map(|d| d.designator().to_string())
            .collect(),
        times: vec![first.time],
        voltages: first.voltages.iter().map(|&v| vec![v]).collect(),
        currents: first.currents.iter().map(|&i| vec![i]).collect(),
        crossings: Vec::new(),
    };
    while let Some(step) = sim.step()? {
        result.times.push(step.sample.time);
        for (trace, v) in result.voltages.iter_mut().zip(step.sample.voltages) {
            trace.push(v);
        }
        for (trace, i) in result.currents.iter_mut().zip(step.sample.currents) {
            trace.push(i);
        }
        result.crossings.extend(step.crossings);
    }
    Ok(result)
}
