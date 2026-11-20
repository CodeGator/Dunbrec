//! Builds the simulator's view of a circuit from a [`Netlist`].

use std::collections::BTreeSet;

use dunbrec_core::{ComponentKind, Element, ModelLibrary, Netlist, NodeId, Severity};

use crate::device::{
    Capacitor, Cccs, Ccvs, ControlledPins, CurrentSource, Device, Inductor, Potentiometer,
    Resistor, Vccs, Vcvs, VoltageSource,
};
use crate::error::{SimError, SimWarning};
use crate::stamp::{BranchId, node_rows};

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
    pub(crate) warnings: Vec<SimWarning>,
}

impl Circuit {
    /// Prepare a netlist for simulation. Fails if the netlist has errors,
    /// uses unsupported devices, has invalid values, or is topologically
    /// unsolvable (voltage-source loops, current sources with no return path).
    pub fn from_netlist(netlist: &Netlist, _models: &ModelLibrary) -> Result<Self, SimError> {
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
        let mut branch_owners = Vec::new();
        for element in &netlist.elements {
            let mut device = make_device(element)?;
            let count = device.branch_count();
            if count > 0 {
                device.assign_branches(BranchId(branch_owners.len()));
                branch_owners.extend(std::iter::repeat_n(element.designator.clone(), count));
            }
            devices.push(device);
        }

        let mut circuit = Circuit {
            devices,
            node_names: netlist.nodes.iter().map(|n| n.name.clone()).collect(),
            branch_owners,
            shunted: Vec::new(),
            warnings: Vec::new(),
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

    pub(crate) fn unknown_count(&self) -> usize {
        node_rows(self.node_count()) + self.branch_owners.len()
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

fn make_device(e: &Element) -> Result<Box<dyn Device>, SimError> {
    let name = e.designator.clone();
    let node = |i: usize| e.nodes[i];
    Ok(match &e.kind {
        ComponentKind::Ground => unreachable!("ground symbols do not produce netlist elements"),
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
        ComponentKind::Capacitor { capacitance, .. } => Box::new(Capacitor {
            name,
            p: node(0),
            n: node(1),
            capacitance: check_positive(e, "capacitance", *capacitance)?,
        }),
        ComponentKind::Inductor { inductance, .. } => Box::new(Inductor {
            name,
            p: node(0),
            n: node(1),
            inductance: check_positive(e, "inductance", *inductance)?,
            branch: None,
        }),
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
        ComponentKind::Diode { .. } => return Err(unsupported(e, "diode")),
        ComponentKind::Bjt { .. } => return Err(unsupported(e, "BJT")),
        ComponentKind::Mosfet { .. } => return Err(unsupported(e, "MOSFET")),
    })
}

fn unsupported(e: &Element, kind: &'static str) -> SimError {
    SimError::Unsupported {
        designator: e.designator.clone(),
        kind,
    }
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
