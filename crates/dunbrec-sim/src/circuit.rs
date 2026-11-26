//! Builds the simulator's view of a circuit from a [`Netlist`].

use std::collections::{BTreeSet, HashSet};

use dunbrec_core::{ComponentKind, Element, ModelLibrary, Netlist, NodeId, Polarity, Severity};

use crate::device::StampContext;
use crate::device::{
    Capacitor, Cccs, Ccvs, ControlledPins, CurrentSource, Device, Inductor, Potentiometer,
    Resistor, SwitchContacts, Vccs, Vcvs, VoltageSource,
};
use crate::digital::{CMOS_5V, DigitalDriver, InputSense, NullDevice, PartSpec, SwitchDriver};
use crate::error::{SimError, SimWarning};
use crate::linalg::{DenseMatrix, LinearSolver};
use crate::macromodel::{
    CD4066_EDGE, CD4066_R_OFF, Composite, TIMER555_EDGE, TIMER555_PORTS, TIMER555_R_OFF,
    TIMER555_R_ON, cd4066, led_display, opamp, timer555,
};
use crate::mcu::{McuPin, McuSpec};
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
    /// What each display is, by device index, to name its LEDs.
    pub(crate) displays: std::collections::HashMap<usize, ComponentKind>,
    /// Digital parts and how they are wired (their drivers follow the
    /// netlist elements in `devices`).
    pub(crate) digital: Vec<PartSpec>,
    /// Microcontrollers and how they are wired (their pin devices follow
    /// the netlist elements in `devices`).
    pub(crate) mcus: Vec<McuSpec>,
    /// Folder relative firmware paths are resolved against.
    pub(crate) firmware_dir: Option<std::path::PathBuf>,
    /// Firmware source files of the schematic (for `source:` firmware).
    pub(crate) sources: std::collections::BTreeMap<String, dunbrec_core::SourceFile>,
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
        let mut mcus = Vec::new();
        for (index, element) in netlist.elements.iter().enumerate() {
            if let ComponentKind::Timer555 = element.kind {
                let (spec, switches) = timer555_logic(
                    index,
                    element,
                    devices[index].as_ref(),
                    slots[index],
                    devices.len(),
                );
                for switch in switches {
                    devices.push(Box::new(switch));
                    slots.push((None, None));
                }
                digital.push(spec);
                continue;
            }
            if let ComponentKind::Cd4066 { r_on } = element.kind {
                let (spec, switches) = cd4066_logic(
                    index,
                    element,
                    r_on,
                    devices[index].as_ref(),
                    slots[index],
                    devices.len(),
                )?;
                for switch in switches {
                    devices.push(Box::new(switch));
                    slots.push((None, None));
                }
                digital.push(spec);
                continue;
            }
            if !element.kind.is_digital() {
                continue;
            }
            if matches!(element.kind, ComponentKind::Mcu { .. }) {
                let mut drivers = Vec::new();
                for (pin, &node) in element.kind.pins().iter().zip(&element.nodes) {
                    drivers.push(devices.len());
                    devices.push(Box::new(McuPin::new(
                        format!("{}:{}", element.designator, pin.name),
                        node,
                    )));
                    slots.push((None, None));
                }
                mcus.push(McuSpec {
                    element: index,
                    designator: element.designator.clone(),
                    kind: element.kind.clone(),
                    nodes: element.nodes.clone(),
                    drivers,
                });
                continue;
            }
            let mut inputs = Vec::new();
            let mut drivers = Vec::new();
            let mut output_nodes = Vec::new();
            for (pin, &node) in element.kind.pins().iter().zip(&element.nodes) {
                match pin.domain {
                    dunbrec_core::PinDomain::DigitalIn => {
                        inputs.push(InputSense::logic(node, &CMOS_5V))
                    }
                    dunbrec_core::PinDomain::DigitalOut => {
                        drivers.push(devices.len());
                        output_nodes.push(Some(node));
                        devices.push(Box::new(DigitalDriver::new(
                            format!("{}:{}", element.designator, pin.name),
                            node,
                            CMOS_5V,
                        )));
                        slots.push((None, None));
                    }
                    dunbrec_core::PinDomain::Analog | dunbrec_core::PinDomain::DigitalIo => {}
                }
            }
            digital.push(PartSpec {
                element: index,
                kind: element.kind.clone(),
                inputs,
                drivers,
                output_nodes,
            });
        }

        let displays = netlist
            .elements
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind.led_segments().is_some())
            .map(|(index, e)| (index, e.kind.clone()))
            .collect();
        let mut circuit = Circuit {
            displays,
            devices,
            node_names,
            branch_owners,
            shunted: Vec::new(),
            warnings: Vec::new(),
            slots,
            digital,
            mcus,
            firmware_dir: None,
            sources: netlist.sources.clone(),
        };
        circuit.check_topology(&netlist.unused_nets())?;
        Ok(circuit)
    }

    pub fn node_count(&self) -> usize {
        self.node_names.len()
    }

    pub fn node_name(&self, n: NodeId) -> &str {
        &self.node_names[n.0]
    }

    /// Folder against which relative microcontroller firmware paths are
    /// resolved (normally the schematic's folder).
    pub fn set_firmware_dir(&mut self, dir: impl Into<std::path::PathBuf>) {
        self.firmware_dir = Some(dir.into());
    }

    /// True if the circuit contains microcontrollers.
    pub fn has_mcus(&self) -> bool {
        !self.mcus.is_empty()
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

    /// Fail on circuits with no solution, and tie down nodes with no DC path
    /// to ground, saying so unless nothing was meant to be joined to them
    /// (`unused`).
    fn check_topology(&mut self, unused: &HashSet<NodeId>) -> Result<(), SimError> {
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
        // Nothing is said of a group of nodes that holds only unused pins,
        // and the internal nodes between them.
        let (mut quiet, mut used) = (HashSet::new(), HashSet::new());
        for &i in &floating {
            if unused.contains(&NodeId(i)) {
                quiet.insert(dsu.find(i));
            } else if !self.node_names[i].contains('#') {
                used.insert(dsu.find(i));
            }
        }
        let nodes: Vec<String> = floating
            .iter()
            .filter(|&&i| {
                let group = dsu.find(i);
                used.contains(&group) || !quiet.contains(&group)
            })
            .map(|&i| self.node_names[i].clone())
            .collect();
        if !nodes.is_empty() {
            self.warnings.push(SimWarning::FloatingNodes { nodes });
        }
        Ok(())
    }
}

/// Comparator hysteresis of the 555's threshold and trigger inputs (V).
const TIMER555_HYSTERESIS: f64 = 1e-3;
/// The 555's RESET pin clears it below about 0.7 V (rising and falling
/// switching points, V).
const TIMER555_RESET: (f64, f64) = (0.8, 0.6);

/// The logic half of a 555 placed as element `index`, whose analog half is
/// `device` (a [`Composite`] of [`timer555`]): the trigger comparator (high
/// while TRIG is below the 1/3 VCC tap), the threshold comparator (high while
/// THR is above CTRL, 2/3 VCC), the RESET input, and three switches — output
/// to the high-side supply, output to GND, and DIS to GND. The switches are
/// returned for the caller to add to the circuit, in that order, starting at
/// device index `first_driver` (the present device count).
fn timer555_logic(
    index: usize,
    element: &Element,
    device: &dyn Device,
    (_, first_internal): (Option<BranchId>, Option<NodeId>),
    first_driver: usize,
) -> (PartSpec, Vec<SwitchDriver>) {
    let pin = |name: &str| {
        let i = TIMER555_PORTS
            .iter()
            .position(|&p| p == name)
            .expect("555 pin");
        element.nodes[i]
    };
    let internal = |name: &str| {
        let k = device
            .internal_nodes()
            .iter()
            .position(|n| n == name)
            .expect("555 internal net");
        NodeId(first_internal.expect("555 has internal nets").0 + k)
    };
    let (gnd, out) = (pin("GND"), pin("OUT"));
    let inputs = vec![
        InputSense::comparator(internal("mid"), pin("TRIG"), TIMER555_HYSTERESIS),
        InputSense::comparator(pin("THR"), pin("CTRL"), TIMER555_HYSTERESIS),
        InputSense {
            p: pin("RESET"),
            n: gnd,
            rise: TIMER555_RESET.0,
            fall: TIMER555_RESET.1,
        },
    ];
    let paths = [
        ("out-high", internal("high"), out),
        ("out-low", out, gnd),
        ("discharge", pin("DIS"), gnd),
    ];
    let switches: Vec<SwitchDriver> = paths
        .iter()
        .zip(TIMER555_R_ON)
        .map(|(&(what, a, b), r_on)| {
            SwitchDriver::new(
                format!("{}:{what}", element.designator),
                (a, b),
                (r_on, TIMER555_R_OFF),
                TIMER555_EDGE,
                CMOS_5V,
            )
        })
        .collect();
    let spec = PartSpec {
        element: index,
        kind: element.kind.clone(),
        inputs,
        drivers: (first_driver..first_driver + switches.len()).collect(),
        // Only the high side reads as the output's logic level.
        output_nodes: vec![Some(out), None, None],
    };
    (spec, switches)
}

/// Comparator hysteresis of the CD4066B's control pins (V).
const CD4066_HYSTERESIS: f64 = 0.1;

/// The logic half of a CD4066B placed as element `index`: each control pin
/// is compared with the half-supply net, and drives a switch between its
/// signal pins.
fn cd4066_logic(
    index: usize,
    element: &Element,
    r_on: f64,
    device: &dyn Device,
    (_, first_internal): (Option<BranchId>, Option<NodeId>),
    first_driver: usize,
) -> Result<(PartSpec, Vec<SwitchDriver>), SimError> {
    let r_on = check_positive(element, "on resistance", r_on)?;
    let mid = {
        let k = device
            .internal_nodes()
            .iter()
            .position(|n| n == "mid")
            .expect("CD4066 internal net");
        NodeId(first_internal.expect("CD4066 has an internal net").0 + k)
    };
    let mut inputs = Vec::new();
    let mut switches = Vec::new();
    for (k, &(a, b, control)) in dunbrec_core::CD4066_SWITCHES.iter().enumerate() {
        inputs.push(InputSense::comparator(
            element.nodes[control],
            mid,
            CD4066_HYSTERESIS,
        ));
        switches.push(SwitchDriver::new(
            format!("{}:{}", element.designator, (b'A' + k as u8) as char),
            (element.nodes[a], element.nodes[b]),
            (r_on, CD4066_R_OFF),
            CD4066_EDGE,
            CMOS_5V,
        ));
    }
    let spec = PartSpec {
        element: index,
        kind: element.kind.clone(),
        inputs,
        drivers: (first_driver..first_driver + switches.len()).collect(),
        output_nodes: vec![None; switches.len()],
    };
    Ok((spec, switches))
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

pub(crate) fn make_device(e: &Element, models: &ModelLibrary) -> Result<Box<dyn Device>, SimError> {
    let name = e.designator.clone();
    let node = |i: usize| e.nodes[i];
    Ok(match &e.kind {
        ComponentKind::Ground | ComponentKind::Junction => {
            unreachable!("ground symbols and junctions do not produce netlist elements")
        }
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
        ComponentKind::Mcu { clock, .. } => {
            check_positive(e, "clock frequency", *clock)?;
            Box::new(NullDevice { name })
        }
        ComponentKind::OpAmp { part } => {
            let model = opamp(part).ok_or_else(|| {
                invalid(
                    e,
                    format!(
                        "no built-in op-amp model '{part}' (available: {})",
                        dunbrec_core::OPAMP_PARTS.join(", ")
                    ),
                )
            })?;
            Box::new(Composite::new(name, model, &e.nodes)?)
        }
        ComponentKind::Timer555 => Box::new(Composite::new(name, timer555(), &e.nodes)?),
        ComponentKind::Cd4066 { r_on } => {
            check_positive(e, "on resistance", *r_on)?;
            Box::new(Composite::new(name, cd4066(), &e.nodes)?)
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
        ComponentKind::MultiSwitch {
            form,
            thrown,
            r_on,
            r_off,
        } => {
            let r_on = check_positive(e, "on resistance", *r_on)?;
            let r_off = check_positive(e, "off resistance", *r_off)?;
            Box::new(SwitchContacts {
                name,
                contacts: form
                    .contacts()
                    .iter()
                    .map(|&(a, b, when)| {
                        (node(a), node(b), if *thrown == when { r_on } else { r_off })
                    })
                    .collect(),
            })
        }
        ComponentKind::Switch {
            closed,
            r_on,
            r_off,
            ..
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
        ComponentKind::Led { colour } => {
            let p = DiodeParams::from_model(&colour.model());
            Box::new(Diode::new(name, node(0), node(1), p).folded())
        }
        kind @ (ComponentKind::LedBar { .. }
        | ComponentKind::SevenSegment { .. }
        | ComponentKind::FourDigit { .. }) => {
            let model = led_display(kind).expect("a display has LEDs");
            Box::new(Composite::new(name, model, &e.nodes)?)
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
