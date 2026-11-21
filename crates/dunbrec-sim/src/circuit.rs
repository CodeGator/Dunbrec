//! Builds the simulator's view of a circuit from a [`Netlist`].

use std::collections::BTreeSet;

use dunbrec_core::{ComponentKind, Element, ModelLibrary, Netlist, NodeId, Polarity, Severity};

use crate::device::StampContext;
use crate::device::{
    Capacitor, Cccs, Ccvs, ControlledPins, CurrentSource, Device, Inductor, Potentiometer,
    Resistor, Vccs, Vcvs, VoltageSource,
};
use crate::digital::{CMOS_5V, DigitalDriver, NullDevice, PartSpec};
use crate::error::{SimError, SimWarning};
use crate::linalg::{DenseMatrix, LinearSolver};
use crate::semiconductor::{Bjt, BjtParams, Diode, DiodeParams, Mosfet, MosfetParams};
use crate::stamp::{BranchId, Stamper, node_rows};

/// Conductance used to tie DC-floating nodes to ground (1 TΩ).
pub const FLOATING_SHUNT: f64 = 1e-12;

#[derive(Debug)]
pub struct Circuit {
    pub(crate) devices: Vec<Box<dyn Device>>,
    pub(crate) node_names: Vec<String>,
    /// Designator owning each branch, for error messages.
    pub(crate) branch_owners: Vec<String>,
    /// Nodes given a [`FLOATING_SHUNT`] to ground.
    pub(crate) shunted: Vec<NodeId>,
    /// Per device: first branch and first internal node it was given, so
    /// a replacement device can take over the same unknowns.
    slots: Vec<(Option<BranchId>, Option<NodeId>)>,
    /// Digital parts and how they are wired (their drivers follow the
    /// netlist elements in `devices`).
    pub(crate) digital: Vec<PartSpec>,
    pub(crate) warnings: Vec<SimWarning>,
}

impl Circuit {
    /// Prepare a netlist for simulation. Fails if the netlist has errors,
    /// uses unsupported devices, has invalid values, or is topologically
    /// unsolvable (voltage-source loops, current sources with no return path).
    pub fn from_netlist(netlist: &Netlist, models: &ModelLibrary) -> Result<Self, SimError> {
        let errors: Vec<_> = netlist
            .diagnostics
            .iter()
            .filter(|d| d.severity() == Severity::Error)
            .cloned()
            .collect();
        if !errors.is_empty() {
            return Err(SimError::Netlist(errors));
        }

        let mut devices = Vec::with_capacity(netlist.elements.len());
        let mut slots = Vec::with_capacity(netlist.elements.len());
        let mut branch_owners = Vec::new();
        let mut node_names: Vec<String> = netlist.nodes.iter().map(|n| n.name.clone()).collect();
        for element in &netlist.elements {
            let mut device = make_device(element, models)?;
            let count = device.branch_count();
            let mut slot = (None, None);
            if count > 0 {
                slot.0 = Some(BranchId(branch_owners.len()));
                device.assign_branches(BranchId(branch_owners.len()));
                branch_owners.extend(std::iter::repeat_n(element.designator.clone(), count));
            }
            // Internal nodes are named "<designator>#<suffix>", e.g. "Q1#b".
            let internal = device.internal_nodes();
            if !internal.is_empty() {
                slot.1 = Some(NodeId(node_names.len()));
                device.assign_internal_nodes(NodeId(node_names.len()));
                node_names.extend(
                    internal
                        .iter()
                        .map(|suffix| format!("{}#{suffix}", element.designator)),
                );
            }
            devices.push(device);
            slots.push(slot);
        }

        // Digital parts: one analog driver per output, after the elements
        // so device indices still match netlist element indices.
        let mut digital = Vec::new();
        for (index, element) in netlist.elements.iter().enumerate() {
            if !element.kind.is_digital() {
                continue;
            }
            let mut input_nodes = Vec::new();
            let mut drivers = Vec::new();
            let mut output_nodes = Vec::new();
            for (pin, &node) in element.kind.pins().iter().zip(&element.nodes) {
                match pin.domain {
                    dunbrec_core::PinDomain::DigitalIn => input_nodes.push(node),
                    dunbrec_core::PinDomain::DigitalOut => {
                        drivers.push(devices.len());
                        output_nodes.push(node);
                        devices.push(Box::new(DigitalDriver::new(
                            format!("{}:{}", element.designator, pin.name),
                            node,
                            CMOS_5V,
                        )));
                        slots.push((None, None));
                    }
                    dunbrec_core::PinDomain::Analog => {}
                }
            }
            digital.push(PartSpec {
                element: index,
                kind: element.kind.clone(),
                input_nodes,
                drivers,
                output_nodes,
            });
        }

        let mut circuit = Circuit {
            devices,
            node_names,
            branch_owners,
            shunted: Vec::new(),
            warnings: Vec::new(),
            slots,
            digital,
        };
        circuit.check_topology()?;
        Ok(circuit)
    }

    pub fn node_count(&self) -> usize {
        self.node_names.len()
    }

    pub fn node_name(&self, n: NodeId) -> &str {
        &self.node_names[n.0]
    }

    pub fn warnings(&self) -> &[SimWarning] {
        &self.warnings
    }

    /// Rebuild device `index` from `element` (same nodes, new parameters).
    /// Returns `Ok(false)` if the new device would need a different number
    /// of branches or internal nodes; nothing is changed in that case.
    pub(crate) fn replace_device(
        &mut self,
        index: usize,
        element: &Element,
        models: &ModelLibrary,
    ) -> Result<bool, SimError> {
        let old = &self.devices[index];
        let mut new = make_device(element, models)?;
        if new.branch_count() != old.branch_count()
            || new.internal_nodes().len() != old.internal_nodes().len()
        {
            return Ok(false);
        }
        let (branch, internal) = self.slots[index];
        if let Some(b) = branch {
            new.assign_branches(b);
        }
        if let Some(n) = internal {
            new.assign_internal_nodes(n);
        }
        self.devices[index] = new;
        Ok(true)
    }

    /// True if any device needs Newton-Raphson iteration.
    pub fn is_nonlinear(&self) -> bool {
        self.devices.iter().any(|d| d.is_nonlinear())
    }

    pub(crate) fn unknown_count(&self) -> usize {
        node_rows(self.node_count()) + self.branch_owners.len()
    }

    /// Build the MNA system for `ctx`, linearised around `iterate`, and
    /// solve it into `x`. All slices and `matrix` must be sized to
    /// [`unknown_count`](Self::unknown_count).
    pub(crate) fn solve_linearised(
        &self,
        ctx: &StampContext,
        iterate: &[f64],
        matrix: &mut DenseMatrix,
        x: &mut [f64],
        solver: &mut dyn LinearSolver,
    ) -> Result<(), SimError> {
        matrix.clear();
        x.fill(0.0);
        {
            let rows = node_rows(self.node_count());
            let mut s = Stamper::new(matrix, x, rows, iterate);
            for d in &self.devices {
                d.stamp(&mut s, ctx);
            }
            for &node in &self.shunted {
                s.conductance(node, NodeId::GROUND, FLOATING_SHUNT);
            }
            if ctx.gmin > 0.0 {
                for n in 1..self.node_count() {
                    s.conductance(NodeId(n), NodeId::GROUND, ctx.gmin);
                }
            }
        }
        solver.solve(matrix, x).map_err(|e| SimError::Singular {
            unknown: self.describe_unknown(e.index),
        })
    }

    /// Human-readable name of an unknown (matrix row/column).
    pub(crate) fn describe_unknown(&self, index: usize) -> String {
        let rows = node_rows(self.node_count());
        if index < rows {
            format!("node {}", self.node_names[index + 1])
        } else {
            format!("the current through {}", self.branch_owners[index - rows])
        }
    }

    fn check_topology(&mut self) -> Result<(), SimError> {
        let n = self.node_count();

        // Voltage loops: adding a voltage constraint between two nodes that
        // are already linked by voltage constraints closes a loop.
        let mut dsu = Dsu::new(n);
        for d in &self.devices {
            for (a, b) in d.voltage_constraints() {
                if !dsu.union(a.0, b.0) {
                    return Err(SimError::VoltageLoop {
                        designator: d.designator().to_string(),
                    });
                }
            }
        }

        // Nodes with no DC path to ground.
        let mut dsu = Dsu::new(n);
        for d in &self.devices {
            for (a, b) in d.dc_paths() {
                dsu.union(a.0, b.0);
            }
        }
        let ground = dsu.find(0);
        let floating: BTreeSet<usize> = (1..n).filter(|&i| dsu.find(i) != ground).collect();
        if floating.is_empty() {
            return Ok(());
        }

        // A current source feeding a floating node has nowhere to send its
        // current: no DC solution exists.
        for d in &self.devices {
            let stranded: Vec<String> = d
                .current_injections()
                .into_iter()
                .filter(|node| floating.contains(&node.0))
                .map(|node| self.node_names[node.0].clone())
                .collect();
            if !stranded.is_empty() {
                return Err(SimError::CurrentSourceNoReturn {
                    designator: d.designator().to_string(),
                    nodes: stranded,
                });
            }
        }

        // Otherwise (e.g. nodes between series capacitors) tie them to
        // ground with a very large resistor, as SPICE's rshunt option does.
        self.shunted = floating.iter().map(|&i| NodeId(i)).collect();
        self.warnings.push(SimWarning::FloatingNodes {
            nodes: floating
                .iter()
                .map(|&i| self.node_names[i].clone())
                .collect(),
        });
        Ok(())
    }
}

fn invalid(e: &Element, message: impl Into<String>) -> SimError {
    SimError::InvalidParameter {
        designator: e.designator.clone(),
        message: message.into(),
    }
}

fn check_finite(e: &Element, what: &str, v: f64) -> Result<f64, SimError> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(invalid(e, format!("{what} must be a finite number")))
    }
}

fn check_positive(e: &Element, what: &str, v: f64) -> Result<f64, SimError> {
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(invalid(e, format!("{what} must be greater than zero")))
    }
}

fn controlled_pins(e: &Element) -> ControlledPins {
    ControlledPins {
        out_p: e.nodes[0],
        out_n: e.nodes[1],
        ctrl_p: e.nodes[2],
        ctrl_n: e.nodes[3],
    }
}

fn make_device(e: &Element, models: &ModelLibrary) -> Result<Box<dyn Device>, SimError> {
    let name = e.designator.clone();
    let node = |i: usize| e.nodes[i];
    Ok(match &e.kind {
        ComponentKind::Ground => unreachable!("ground symbols do not produce netlist elements"),
        kind @ (ComponentKind::Gate { .. }
        | ComponentKind::Inverter { .. }
        | ComponentKind::DFlipFlop { .. }
        | ComponentKind::Counter4 { .. }
        | ComponentKind::Clock { .. }
        | ComponentKind::LogicSwitch { .. }
        | ComponentKind::LogicLed) => {
            if let ComponentKind::Clock { frequency, duty } = kind {
                check_positive(e, "frequency", *frequency)?;
                if !(0.0 < *duty && *duty < 1.0) {
                    return Err(invalid(e, "duty cycle must be between 0 and 1"));
                }
            }
            Box::new(NullDevice { name })
        }
        ComponentKind::Resistor { resistance } => {
            let r = check_finite(e, "resistance", *resistance)?;
            if r == 0.0 {
                return Err(invalid(e, "resistance must not be zero"));
            }
            Box::new(Resistor {
                name,
                p: node(0),
                n: node(1),
                resistance: r,
            })
        }
        ComponentKind::Switch {
            closed,
            r_on,
            r_off,
        } => {
            let r_on = check_positive(e, "on resistance", *r_on)?;
            let r_off = check_positive(e, "off resistance", *r_off)?;
            Box::new(Resistor {
                name,
                p: node(0),
                n: node(1),
                resistance: if *closed { r_on } else { r_off },
            })
        }
        ComponentKind::Potentiometer { resistance, wiper } => {
            let resistance = check_positive(e, "resistance", *resistance)?;
            if !(0.0..=1.0).contains(wiper) {
                return Err(invalid(e, "wiper position must be between 0 and 1"));
            }
            Box::new(Potentiometer {
                name,
                a: node(0),
                b: node(1),
                w: node(2),
                resistance,
                wiper: *wiper,
            })
        }
        ComponentKind::Capacitor {
            capacitance,
            initial_voltage,
        } => {
            let ic = initial_voltage
                .map(|v| check_finite(e, "initial voltage", v))
                .transpose()?;
            Box::new(Capacitor::new(
                name,
                node(0),
                node(1),
                check_positive(e, "capacitance", *capacitance)?,
                ic,
            ))
        }
        ComponentKind::Inductor {
            inductance,
            initial_current,
        } => {
            let ic = initial_current
                .map(|i| check_finite(e, "initial current", i))
                .transpose()?;
            Box::new(Inductor::new(
                name,
                node(0),
                node(1),
                check_positive(e, "inductance", *inductance)?,
                ic,
            ))
        }
        ComponentKind::VoltageSource { waveform } => {
            check_finite(e, "source value", waveform.dc_value())?;
            Box::new(VoltageSource {
                name,
                p: node(0),
                n: node(1),
                waveform: waveform.clone(),
                branch: None,
            })
        }
        ComponentKind::CurrentSource { waveform } => {
            check_finite(e, "source value", waveform.dc_value())?;
            Box::new(CurrentSource {
                name,
                input: node(0),
                output: node(1),
                waveform: waveform.clone(),
                time: 0.0,
            })
        }
        ComponentKind::Vcvs { gain } => Box::new(Vcvs {
            name,
            pins: controlled_pins(e),
            gain: check_finite(e, "gain", *gain)?,
            branch: None,
        }),
        ComponentKind::Vccs { transconductance } => Box::new(Vccs {
            name,
            pins: controlled_pins(e),
            transconductance: check_finite(e, "transconductance", *transconductance)?,
        }),
        ComponentKind::Ccvs { transresistance } => Box::new(Ccvs {
            name,
            pins: controlled_pins(e),
            transresistance: check_finite(e, "transresistance", *transresistance)?,
            branch: None,
        }),
        ComponentKind::Cccs { gain } => Box::new(Cccs {
            name,
            pins: controlled_pins(e),
            gain: check_finite(e, "gain", *gain)?,
            branch: None,
        }),
        ComponentKind::Diode { model } => {
            let p = DiodeParams::from_model(lookup(e, models, model)?);
            if !(p.is > 0.0 && p.n > 0.0 && p.rs >= 0.0) {
                return Err(invalid(e, "diode model needs IS > 0, N > 0 and RS >= 0"));
            }
            Box::new(Diode::new(name, node(0), node(1), p))
        }
        ComponentKind::Bjt { polarity, model } => {
            let p = BjtParams::from_model(lookup(e, models, model)?);
            let positive = [p.is, p.bf, p.br, p.nf, p.nr, p.ne, p.nc];
            let not_positive = |v: &f64| v.is_nan() || *v <= 0.0;
            let negative = |r: &f64| r.is_nan() || *r < 0.0;
            if positive.iter().any(not_positive) || [p.rb, p.rc, p.re].iter().any(negative) {
                return Err(invalid(
                    e,
                    "BJT model needs IS, BF, BR, NF, NR, NE, NC > 0 and RB, RC, RE >= 0",
                ));
            }
            Box::new(Bjt::new(
                name,
                [node(0), node(1), node(2)],
                *polarity == Polarity::N,
                p,
            ))
        }
        ComponentKind::Mosfet { polarity, model } => {
            let p = MosfetParams::from_model(lookup(e, models, model)?);
            if !(p.kp > 0.0 && p.w_over_l > 0.0 && p.rd >= 0.0 && p.rs >= 0.0) {
                return Err(invalid(
                    e,
                    "MOSFET model needs KP > 0, W/L > 0 and RD, RS >= 0",
                ));
            }
            Box::new(Mosfet::new(
                name,
                [node(0), node(1), node(2)],
                *polarity == Polarity::N,
                p,
            ))
        }
    })
}

fn lookup<'m>(
    e: &Element,
    models: &'m ModelLibrary,
    name: &str,
) -> Result<&'m dunbrec_core::DeviceModel, SimError> {
    models
        .get(name)
        .ok_or_else(|| invalid(e, format!("model '{name}' not found")))
}

struct Dsu(Vec<usize>);

impl Dsu {
    fn new(n: usize) -> Self {
        Self((0..n).collect())
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }
    /// Returns false if `a` and `b` were already in the same set.
    fn union(&mut self, a: usize, b: usize) -> bool {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return false;
        }
        self.0[rb] = ra;
        true
    }
}
