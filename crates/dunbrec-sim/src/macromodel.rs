//! Integrated circuits built from simpler devices.
//!
//! A [`MacroModel`] is a small netlist, like a SPICE `.subckt`: elements
//! wired to the chip's pins (its *ports*), to ground (`"0"`) and to internal
//! nets. A [`Composite`] device simulates one placed chip by building those
//! elements as ordinary devices. To the rest of the simulator it is a single
//! device with internal nodes and branches, so device indices still match
//! netlist element indices.
//!
//! Chips:
//! * op-amps, by part name ([`opamp`]): the uA741 is the classic Boyle
//!   macromodel (Boyle et al., IEEE JSSC 1974) as published in the PSpice
//!   library — two NPN input transistors, a gain stage with Miller
//!   compensation, output resistance, current limit and supply clamps;
//! * the 555 timer ([`timer555`]): its analog half (the 5 kΩ divider, the
//!   output-high drop, supply current). Its comparators, latch and switches
//!   are wired up by the circuit builder through the digital engine.

use dunbrec_core::{
    ComponentId, ComponentKind, DeviceModel, Element, ModelKind, ModelLibrary, NodeId, Polarity,
    Waveform,
};

use crate::ac::{AcContext, AcStamper};
use crate::circuit::make_device;
use crate::device::{Device, StampContext, Tolerances};
use crate::error::SimError;
use crate::stamp::{BranchId, SolutionView, Stamper};

/// One element of a macromodel. Node names are ports, `"0"` (ground) or
/// internal nets.
#[derive(Debug, Clone)]
pub struct MacroElement {
    pub name: &'static str,
    pub kind: ComponentKind,
    pub nodes: Vec<&'static str>,
}

#[derive(Debug, Clone)]
pub struct MacroModel {
    /// Pin names in [`ComponentKind::pins`] order.
    pub ports: &'static [&'static str],
    pub elements: Vec<MacroElement>,
    pub models: ModelLibrary,
    /// How many of the elements, from the first, are the LEDs of a display,
    /// whose currents are reported one by one.
    pub lamps: usize,
}

impl MacroModel {
    /// Internal nets, in order of first use.
    pub fn nets(&self) -> Vec<&'static str> {
        let mut nets = Vec::new();
        for e in &self.elements {
            for &n in &e.nodes {
                if n != "0" && !self.ports.contains(&n) && !nets.contains(&n) {
                    nets.push(n);
                }
            }
        }
        nets
    }
}

/// Builder shorthand for macromodel elements.
struct Elements(Vec<MacroElement>);

impl Elements {
    fn add(&mut self, name: &'static str, kind: ComponentKind, nodes: &[&'static str]) {
        assert_eq!(nodes.len(), kind.pins().len(), "{name}: wrong node count");
        self.0.push(MacroElement {
            name,
            kind,
            nodes: nodes.to_vec(),
        });
    }
    fn r(&mut self, name: &'static str, a: &'static str, b: &'static str, ohms: f64) {
        self.add(name, ComponentKind::Resistor { resistance: ohms }, &[a, b]);
    }
    fn c(&mut self, name: &'static str, a: &'static str, b: &'static str, farads: f64) {
        let kind = ComponentKind::Capacitor {
            capacitance: farads,
            initial_voltage: None,
        };
        self.add(name, kind, &[a, b]);
    }
    /// DC voltage source: V(p) - V(n) = volts.
    fn v(&mut self, name: &'static str, p: &'static str, n: &'static str, volts: f64) {
        let kind = ComponentKind::VoltageSource {
            waveform: Waveform::Dc(volts),
        };
        self.add(name, kind, &[p, n]);
    }
}

/// Op-amp macromodel for a part name in [`dunbrec_core::OPAMP_PARTS`].
pub fn opamp(part: &str) -> Option<MacroModel> {
    match part.to_ascii_lowercase().as_str() {
        "ua741" | "741" | "lm741" => Some(ua741()),
        _ => None,
    }
}

/// The uA741 Boyle macromodel. Ports: in+, in-, V+, V-, out (SPICE nodes 1
/// to 5). SPICE source:
///
/// ```text
/// .subckt uA741 1 2 3 4 5
/// c1 11 12 8.661E-12      c2 6 7 30.00E-12
/// dc 5 53 dx              de 54 5 dx
/// dlp 90 91 dx            dln 92 90 dx        dp 4 3 dx
/// egnd 99 0 poly(2) (3,0) (4,0) 0 .5 .5
/// fb 7 99 poly(5) vb vc ve vlp vln 0 10.61E6 -10E6 10E6 10E6 -10E6
/// ga 6 0 11 12 188.5E-6   gcm 0 6 10 99 5.961E-9
/// iee 10 4 dc 15.16E-6    hlim 90 0 vlim 1K
/// q1 11 2 13 qx           q2 12 1 14 qx
/// r2 6 9 100.0E3          rc1 3 11 5.305E3    rc2 3 12 5.305E3
/// re1 13 10 1.836E3       re2 14 10 1.836E3   ree 10 99 13.19E6
/// ro1 8 5 50              ro2 7 99 100        rp 3 4 18.16E3
/// vb 9 0 dc 0             vc 3 53 dc 1        ve 54 4 dc 1
/// vlim 7 8 dc 0           vlp 91 0 dc 40      vln 0 92 dc 40
/// .model dx D(Is=800.0E-18 Rs=1)
/// .model qx NPN(Is=800.0E-18 Bf=93.75)
/// .ends
/// ```
///
/// The linear POLY sources become sums of plain controlled sources: `egnd`
/// two voltage-controlled sources in series, and `fb` five current-
/// controlled current sources in parallel, each sensing its source's current
/// with a 0 V sensor in series with it. `hlim` senses `vlim` directly (its
/// sensor *is* the 0 V source `vlim`).
pub fn ua741() -> MacroModel {
    use ComponentKind as K;
    let mut e = Elements(Vec::new());
    // Input stage.
    e.add(
        "Q1",
        K::Bjt {
            polarity: Polarity::N,
            model: "QX".into(),
        },
        &["11", "in-", "13"],
    );
    e.add(
        "Q2",
        K::Bjt {
            polarity: Polarity::N,
            model: "QX".into(),
        },
        &["12", "in+", "14"],
    );
    e.r("RC1", "V+", "11", 5.305e3);
    e.r("RC2", "V+", "12", 5.305e3);
    e.c("C1", "11", "12", 8.661e-12);
    e.r("RE1", "13", "10", 1.836e3);
    e.r("RE2", "14", "10", 1.836e3);
    e.add(
        "IEE",
        K::CurrentSource {
            waveform: Waveform::Dc(15.16e-6),
        },
        &["10", "V-"],
    );
    e.r("REE", "10", "99", 13.19e6);
    e.r("RP", "V+", "V-", 18.16e3);
    // Mid-supply reference: V(99) = 0.5 V(V+) + 0.5 V(V-).
    e.add("EGND1", K::Vcvs { gain: 0.5 }, &["99", "egnd", "V+", "0"]);
    e.add("EGND2", K::Vcvs { gain: 0.5 }, &["egnd", "0", "V-", "0"]);
    // Gain stage with Miller compensation.
    e.add(
        "GA",
        K::Vccs {
            transconductance: 188.5e-6,
        },
        &["6", "0", "11", "12"],
    );
    e.add(
        "GCM",
        K::Vccs {
            transconductance: 5.961e-9,
        },
        &["0", "6", "10", "99"],
    );
    e.r("R2", "6", "9", 100e3);
    e.c("C2", "6", "7", 30e-12);
    // fb = 10.61M I(vb) - 10M I(vc) + 10M I(ve) + 10M I(vlp) - 10M I(vln),
    // from 7 to 99.
    e.add("FB_VB", K::Cccs { gain: 10.61e6 }, &["7", "99", "9", "0"]);
    e.v("VC", "V+", "vcs", 1.0);
    e.add("FB_VC", K::Cccs { gain: -10e6 }, &["7", "99", "vcs", "53"]);
    e.v("VE", "54", "ves", 1.0);
    e.add("FB_VE", K::Cccs { gain: 10e6 }, &["7", "99", "ves", "V-"]);
    e.v("VLP", "91", "vlps", 40.0);
    e.add("FB_VLP", K::Cccs { gain: 10e6 }, &["7", "99", "vlps", "0"]);
    e.v("VLN", "0", "vlns", 40.0);
    e.add(
        "FB_VLN",
        K::Cccs { gain: -10e6 },
        &["7", "99", "vlns", "92"],
    );
    e.r("RO2", "7", "99", 100.0);
    // Output: current limit and clamps to the supplies.
    e.add(
        "HLIM",
        K::Ccvs {
            transresistance: 1e3,
        },
        &["90", "0", "7", "8"],
    );
    e.r("RO1", "8", "out", 50.0);
    let dx = || K::Diode { model: "DX".into() };
    e.add("DC", dx(), &["out", "53"]);
    e.add("DE", dx(), &["54", "out"]);
    e.add("DLP", dx(), &["90", "91"]);
    e.add("DLN", dx(), &["92", "90"]);
    e.add("DP", dx(), &["V-", "V+"]);

    let mut models = ModelLibrary::new();
    models.insert(
        DeviceModel::new("DX", ModelKind::Diode)
            .with("IS", 800e-18)
            .with("RS", 1.0),
    );
    models.insert(
        DeviceModel::new("QX", ModelKind::Npn)
            .with("IS", 800e-18)
            .with("BF", 93.75),
    );
    MacroModel {
        ports: &["in+", "in-", "V+", "V-", "out"],
        elements: e.0,
        models,
        lamps: 0,
    }
}

/// Pin names of the 555, in package order.
pub const TIMER555_PORTS: &[&str] = &["GND", "TRIG", "OUT", "RESET", "CTRL", "THR", "DIS", "VCC"];

/// Output high sits this far below VCC with no load (V)...
pub const TIMER555_HIGH_DROP: f64 = 1.4;
/// ...behind these on-resistances (Ω): output high side, output low side,
/// discharge. Fitted to NE555 data sheet output levels.
pub const TIMER555_R_ON: [f64; 3] = [5.0, 10.0, 15.0];
/// Off-state resistance of the output and discharge switches (Ω).
pub const TIMER555_R_OFF: f64 = 1e9;
/// Output rise and fall time (s).
pub const TIMER555_EDGE: f64 = 100e-9;

/// Analog half of the NE555: the three 5 kΩ divider resistors that set the
/// comparator references (CTRL at 2/3 VCC, net `mid` at 1/3 VCC), the
/// output-high supply drop (net `high`), and the chip's own supply current
/// (about 3 mA at 5 V, 9 mA at 15 V).
pub fn timer555() -> MacroModel {
    let mut e = Elements(Vec::new());
    e.r("R1", "VCC", "CTRL", 5e3);
    e.r("R2", "CTRL", "mid", 5e3);
    e.r("R3", "mid", "GND", 5e3);
    e.r("RQ", "VCC", "GND", 1.8e3);
    e.v("VHIGH", "VCC", "high", TIMER555_HIGH_DROP);
    MacroModel {
        ports: TIMER555_PORTS,
        elements: e.0,
        models: ModelLibrary::new(),
        lamps: 0,
    }
}

/// Output rise and fall time of a CD4066B switch (s): its on/off change
/// follows the control pin within the data sheet's tens of nanoseconds.
pub const CD4066_EDGE: f64 = 10e-9;
/// Off-state resistance of a CD4066B switch (Ω): the data sheet's 10 pA
/// leakage at 10 V is about 1 TΩ; 1 GΩ keeps the matrices well conditioned.
pub const CD4066_R_OFF: f64 = 1e9;
/// Resistance of each half of the divider that sets the control threshold
/// at half the supply (Ω): the chip's own supply current stays below 1 µA.
const CD4066_DIVIDER: f64 = 10e6;

/// Analog half of the CD4066B: a divider that puts net `mid` halfway
/// between VDD and VSS, which the four control pins are compared with.
/// The switches themselves are added by the circuit builder.
pub fn cd4066() -> MacroModel {
    let mut e = Elements(Vec::new());
    e.r("RH", "VDD", "mid", CD4066_DIVIDER);
    e.r("RL", "mid", "VSS", CD4066_DIVIDER);
    MacroModel {
        ports: CD4066_PORTS,
        elements: e.0,
        models: ModelLibrary::new(),
        lamps: 0,
    }
}

/// Names of a display's LEDs within it, by segment.
const LAMP_NAMES: [&str; 16] = [
    "S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8", "S9", "S10", "S11", "S12", "S13", "S14", "S15",
    "S16",
];

/// A display of several LEDs: one for each of the part's segments, between
/// its two pins.
pub fn led_display(kind: &ComponentKind) -> Option<MacroModel> {
    let (colour, segments) = kind.led_segments()?;
    let pins = kind.pins();
    let mut e = Elements(Vec::new());
    for (&(a, k), name) in segments.iter().zip(LAMP_NAMES) {
        e.add(
            name,
            ComponentKind::Led { colour },
            &[pins[a].name, pins[k].name],
        );
    }
    Some(MacroModel {
        ports: LED_BAR_PORTS,
        elements: e.0,
        models: ModelLibrary::new(),
        lamps: segments.len(),
    })
}

/// Pin names of the LED bar display, in pin order.
const LED_BAR_PORTS: &[&str] = &[
    "A1", "A2", "A3", "A4", "A5", "A6", "A7", "A8", "A9", "A10", "K1", "K2", "K3", "K4", "K5",
    "K6", "K7", "K8", "K9", "K10",
];

/// Pin names of the CD4066B, in package order.
pub const CD4066_PORTS: &[&str] = &[
    "A1", "A2", "B2", "B1", "CB", "CC", "VSS", "C1", "C2", "D2", "D1", "CD", "CA", "VDD",
];

/// One placed chip, simulated as the devices of its macromodel.
#[derive(Debug)]
pub struct Composite {
    name: String,
    model: MacroModel,
    pins: Vec<NodeId>,
    nets: Vec<&'static str>,
    parts: Vec<Box<dyn Device>>,
    first_branch: Option<BranchId>,
    first_internal: Option<NodeId>,
}

impl Composite {
    pub fn new(name: String, model: MacroModel, pins: &[NodeId]) -> Result<Self, SimError> {
        assert_eq!(pins.len(), model.ports.len(), "{name}: wrong pin count");
        let nets = model.nets();
        let mut c = Self {
            name,
            model,
            pins: pins.to_vec(),
            nets,
            parts: Vec::new(),
            first_branch: None,
            first_internal: None,
        };
        // Built at once so counts are known; internal nets get their real
        // nodes when they are assigned.
        c.parts = c.build()?;
        Ok(c)
    }

    /// Node of a port, ground or internal net (ground until internal nodes
    /// are assigned).
    pub fn node(&self, net: &str) -> NodeId {
        if net == "0" {
            return NodeId::GROUND;
        }
        if let Some(i) = self.model.ports.iter().position(|&p| p == net) {
            return self.pins[i];
        }
        match (
            self.nets.iter().position(|&n| n == net),
            self.first_internal,
        ) {
            (Some(k), Some(first)) => NodeId(first.0 + k),
            _ => NodeId::GROUND,
        }
    }

    fn build(&self) -> Result<Vec<Box<dyn Device>>, SimError> {
        self.model
            .elements
            .iter()
            .map(|m| {
                let element = Element {
                    component: ComponentId(u32::MAX),
                    designator: format!("{}.{}", self.name, m.name),
                    kind: m.kind.clone(),
                    nodes: m.nodes.iter().map(|n| self.node(n)).collect(),
                };
                make_device(&element, &self.model.models)
            })
            .collect()
    }

    fn assign_part_branches(&mut self) {
        let Some(mut next) = self.first_branch else {
            return;
        };
        for d in &mut self.parts {
            let count = d.branch_count();
            if count > 0 {
                d.assign_branches(next);
                next = BranchId(next.0 + count);
            }
        }
    }
}

impl Device for Composite {
    fn designator(&self) -> &str {
        &self.name
    }
    fn branch_count(&self) -> usize {
        self.parts.iter().map(|d| d.branch_count()).sum()
    }
    fn assign_branches(&mut self, first: BranchId) {
        self.first_branch = Some(first);
        self.assign_part_branches();
    }
    /// The model's internal nets, then each part's own internal nodes
    /// (named after the part, e.g. `Q1.b`).
    fn internal_nodes(&self) -> Vec<String> {
        let mut names: Vec<String> = self.nets.iter().map(|n| n.to_string()).collect();
        for (m, d) in self.model.elements.iter().zip(&self.parts) {
            names.extend(
                d.internal_nodes()
                    .into_iter()
                    .map(|n| format!("{}.{n}", m.name)),
            );
        }
        names
    }
    fn assign_internal_nodes(&mut self, first: NodeId) {
        self.first_internal = Some(first);
        // Rebuild now that internal nets have nodes. The parts were built
        // from the same elements before, so this cannot fail.
        self.parts = self
            .build()
            .expect("macromodel elements were already validated");
        self.assign_part_branches();
        let mut next = first.0 + self.nets.len();
        for d in &mut self.parts {
            let count = d.internal_nodes().len();
            if count > 0 {
                d.assign_internal_nodes(NodeId(next));
                next += count;
            }
        }
    }
    fn is_nonlinear(&self) -> bool {
        self.parts.iter().any(|d| d.is_nonlinear())
    }
    fn limited(&self) -> bool {
        self.parts.iter().any(|d| d.limited())
    }
    fn reset_iteration(&self) {
        self.parts.iter().for_each(|d| d.reset_iteration());
    }
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext) {
        self.parts.iter().for_each(|d| d.stamp(s, ctx));
    }
    fn stamp_ac(&self, s: &mut AcStamper, ctx: &AcContext) {
        self.parts.iter().for_each(|d| d.stamp_ac(s, ctx));
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        self.parts.iter().flat_map(|d| d.dc_paths()).collect()
    }
    fn voltage_constraints(&self) -> Vec<(NodeId, NodeId)> {
        self.parts
            .iter()
            .flat_map(|d| d.voltage_constraints())
            .collect()
    }
    fn current_injections(&self) -> Vec<NodeId> {
        self.parts
            .iter()
            .flat_map(|d| d.current_injections())
            .collect()
    }
    /// A chip has no single current "through" it; reported as zero.
    fn current(&self, _sol: &SolutionView) -> f64 {
        0.0
    }
    fn lamp_currents(&self, sol: &SolutionView) -> Vec<f64> {
        self.parts[..self.model.lamps]
            .iter()
            .map(|d| d.current(sol))
            .collect()
    }
    fn begin_transient(&mut self, sol: &SolutionView, time: f64) {
        self.parts
            .iter_mut()
            .for_each(|d| d.begin_transient(sol, time));
    }
    fn accept_step(&mut self, sol: &SolutionView, ctx: &StampContext, restart: bool) {
        self.parts
            .iter_mut()
            .for_each(|d| d.accept_step(sol, ctx, restart));
    }
    fn truncation_step(
        &self,
        sol: &SolutionView,
        ctx: &StampContext,
        tol: &Tolerances,
    ) -> Option<f64> {
        self.parts
            .iter()
            .filter_map(|d| d.truncation_step(sol, ctx, tol))
            .reduce(f64::min)
    }
    fn next_breakpoint(&self, after: f64) -> Option<f64> {
        self.parts
            .iter()
            .filter_map(|d| d.next_breakpoint(after))
            .reduce(f64::min)
    }
    fn max_step(&self) -> Option<f64> {
        self.parts
            .iter()
            .filter_map(|d| d.max_step())
            .reduce(f64::min)
    }
    fn ringing(&self) -> bool {
        self.parts.iter().any(|d| d.ringing())
    }
}
