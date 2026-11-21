//! The graphical schematic: components placed on a grid, wires and net labels.
//!
//! Coordinates are integer grid units. The simulator never sees this layer
//! directly; it works on the [`Netlist`](crate::netlist::Netlist) produced by
//! [`build_netlist`](crate::netlist::build_netlist).

use std::collections::BTreeMap;
use std::ops::{Add, Sub};

use serde::{Deserialize, Serialize};

use crate::models::ModelLibrary;

/// A point on the schematic grid.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl Add for Point {
    type Output = Point;
    fn add(self, o: Point) -> Point {
        Point::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Point {
    type Output = Point;
    fn sub(self, o: Point) -> Point {
        Point::new(self.x - o.x, self.y - o.y)
    }
}

/// Rotation of a placed component, in 90° steps (counter-clockwise in maths
/// coordinates; the UI decides how y maps to the screen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Rotation {
    #[default]
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    pub fn apply(self, p: Point) -> Point {
        match self {
            Rotation::R0 => p,
            Rotation::R90 => Point::new(-p.y, p.x),
            Rotation::R180 => Point::new(-p.x, -p.y),
            Rotation::R270 => Point::new(p.y, -p.x),
        }
    }

    pub fn rotated_cw(self) -> Self {
        match self {
            Rotation::R0 => Rotation::R270,
            Rotation::R90 => Rotation::R0,
            Rotation::R180 => Rotation::R90,
            Rotation::R270 => Rotation::R180,
        }
    }

    pub fn rotated_ccw(self) -> Self {
        match self {
            Rotation::R0 => Rotation::R90,
            Rotation::R90 => Rotation::R180,
            Rotation::R180 => Rotation::R270,
            Rotation::R270 => Rotation::R0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ComponentId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WireId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LabelId(pub u32);

/// Which simulation domain a pin belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PinDomain {
    Analog,
    /// A logic input: reads its net's voltage against the input thresholds.
    DigitalIn,
    /// A logic output: drives its net to the logic level.
    DigitalOut,
    /// A microcontroller I/O pin: output, input or input with pull-up,
    /// as the firmware decides.
    DigitalIo,
}

/// Static description of one pin of a component kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinDef {
    pub name: &'static str,
    /// Offset from the component origin before rotation/mirroring.
    pub offset: Point,
    pub domain: PinDomain,
}

const fn pin(name: &'static str, x: i32, y: i32) -> PinDef {
    PinDef {
        name,
        offset: Point::new(x, y),
        domain: PinDomain::Analog,
    }
}

const TWO_TERMINAL: &[PinDef] = &[pin("p", -2, 0), pin("n", 2, 0)];
const VOLTAGE_SOURCE: &[PinDef] = &[pin("+", 0, -2), pin("-", 0, 2)];
/// Current flows into the source at `in` and out of it at `out` (arrow points
/// from `in` to `out`). Matches SPICE `I<name> in out <value>`.
const CURRENT_SOURCE: &[PinDef] = &[pin("in", 0, 2), pin("out", 0, -2)];
const DIODE: &[PinDef] = &[pin("A", -2, 0), pin("K", 2, 0)];
const BJT: &[PinDef] = &[pin("C", 2, -2), pin("B", -2, 0), pin("E", 2, 2)];
const MOSFET: &[PinDef] = &[pin("D", 2, -2), pin("G", -2, 0), pin("S", 2, 2)];
const GROUND: &[PinDef] = &[pin("gnd", 0, 0)];
const SPDT: &[PinDef] = &[pin("C", -2, 0), pin("1", 2, -1), pin("2", 2, 1)];
const DPST: &[PinDef] = &[
    pin("1a", -2, -1),
    pin("1b", 2, -1),
    pin("2a", -2, 1),
    pin("2b", 2, 1),
];
const DPDT: &[PinDef] = &[
    pin("C1", -2, -2),
    pin("1A", 2, -3),
    pin("1B", 2, -1),
    pin("C2", -2, 2),
    pin("2A", 2, 1),
    pin("2B", 2, 3),
];
const POT: &[PinDef] = &[pin("A", -2, 0), pin("B", 2, 0), pin("W", 0, -2)];
const fn din(name: &'static str, x: i32, y: i32) -> PinDef {
    PinDef {
        name,
        offset: Point::new(x, y),
        domain: PinDomain::DigitalIn,
    }
}

const fn dout(name: &'static str, x: i32, y: i32) -> PinDef {
    PinDef {
        name,
        offset: Point::new(x, y),
        domain: PinDomain::DigitalOut,
    }
}

const GATE2: &[PinDef] = &[din("A", -3, -1), din("B", -3, 1), dout("Y", 3, 0)];
const INVERTER: &[PinDef] = &[din("A", -3, 0), dout("Y", 3, 0)];
const DFF: &[PinDef] = &[
    din("D", -3, -1),
    din("CLK", -3, 1),
    dout("Q", 3, -1),
    dout("QN", 3, 1),
];
const COUNTER4: &[PinDef] = &[
    din("CLK", -3, 0),
    dout("Q0", 3, -3),
    dout("Q1", 3, -1),
    dout("Q2", 3, 1),
    dout("Q3", 3, 3),
];
const LOGIC_SOURCE: &[PinDef] = &[dout("Y", 3, 0)];

const fn dio(name: &'static str, x: i32, y: i32) -> PinDef {
    PinDef {
        name,
        offset: Point::new(x, y),
        domain: PinDomain::DigitalIo,
    }
}

/// ATmega328P I/O pins in Arduino order: D0–D9 down the left side,
/// D10–D13 and A0–A5 down the right.
const MCU: &[PinDef] = &[
    dio("D0", -6, -9),
    dio("D1", -6, -7),
    dio("D2", -6, -5),
    dio("D3", -6, -3),
    dio("D4", -6, -1),
    dio("D5", -6, 1),
    dio("D6", -6, 3),
    dio("D7", -6, 5),
    dio("D8", -6, 7),
    dio("D9", -6, 9),
    dio("D10", 6, -9),
    dio("D11", 6, -7),
    dio("D12", 6, -5),
    dio("D13", 6, -3),
    dio("A0", 6, -1),
    dio("A1", 6, 1),
    dio("A2", 6, 3),
    dio("A3", 6, 5),
    dio("A4", 6, 7),
    dio("A5", 6, 9),
];
const LOGIC_LED: &[PinDef] = &[din("A", -3, 0)];

/// Op-amp pins in SPICE subcircuit order (in+, in-, V+, V-, out). The
/// inverting input is on top, as in most textbook drawings.
const OPAMP: &[PinDef] = &[
    pin("in+", -3, 1),
    pin("in-", -3, -1),
    pin("V+", 0, -3),
    pin("V-", 0, 3),
    pin("out", 3, 0),
];

/// 555 timer pins in package order (pin 1 to pin 8).
const TIMER555: &[PinDef] = &[
    pin("GND", -1, 6),
    pin("TRIG", -5, 2),
    pin("OUT", 5, 0),
    pin("RESET", 1, -6),
    pin("CTRL", 1, 6),
    pin("THR", -5, 0),
    pin("DIS", -5, -2),
    pin("VCC", -1, -6),
];

/// CD4066B pins in package order (pin 1 to pin 14): the four switches'
/// signal pins `<n>1` and `<n>2` (either way round), their control pins
/// `C<n>`, and the supplies.
pub const CD4066_PINS: &[PinDef] = &[
    pin("A1", -5, -6),
    pin("A2", -5, -4),
    pin("B2", -5, -2),
    pin("B1", -5, 0),
    pin("CB", -5, 2),
    pin("CC", -5, 4),
    pin("VSS", -5, 6),
    pin("C1", 5, 6),
    pin("C2", 5, 4),
    pin("D2", 5, 2),
    pin("D1", 5, 0),
    pin("CD", 5, -2),
    pin("CA", 5, -4),
    pin("VDD", 5, -6),
];

/// The four switches of a CD4066B as `(signal pin, signal pin, control
/// pin)` indices into [`CD4066_PINS`].
pub const CD4066_SWITCHES: [(usize, usize, usize); 4] =
    [(0, 1, 12), (3, 2, 4), (7, 8, 5), (10, 9, 11)];

/// Typical on-resistance of the CD4066B at 25 °C (Ω) for supplies of 5 V,
/// 10 V and 15 V (TI data sheet SCHS051J).
pub const CD4066_R_ON: [(f64, f64); 3] = [(5.0, 470.0), (10.0, 180.0), (15.0, 125.0)];

/// Default on-resistance of a placed CD4066B: the 10 V figure (Ω).
pub const CD4066_DEFAULT_R_ON: f64 = 180.0;

const CONTROLLED: &[PinDef] = &[
    pin("out+", 2, -2),
    pin("out-", 2, 2),
    pin("ctrl+", -2, -2),
    pin("ctrl-", -2, 2),
];

/// The function of a two-input logic gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GateOp {
    And,
    Or,
    Nand,
    Nor,
    Xor,
    Xnor,
}

impl GateOp {
    pub const ALL: [GateOp; 6] = [
        GateOp::And,
        GateOp::Or,
        GateOp::Nand,
        GateOp::Nor,
        GateOp::Xor,
        GateOp::Xnor,
    ];

    pub fn name(self) -> &'static str {
        match self {
            GateOp::And => "AND",
            GateOp::Or => "OR",
            GateOp::Nand => "NAND",
            GateOp::Nor => "NOR",
            GateOp::Xor => "XOR",
            GateOp::Xnor => "XNOR",
        }
    }
}

/// Default propagation delay of logic parts (s).
pub const DEFAULT_LOGIC_DELAY: f64 = 10e-9;

fn default_logic_delay() -> f64 {
    DEFAULT_LOGIC_DELAY
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Polarity {
    /// NPN for BJTs, N-channel for MOSFETs.
    N,
    /// PNP for BJTs, P-channel for MOSFETs.
    P,
}

/// Time-domain behaviour of an independent source. Parameters follow SPICE
/// conventions so import/export is lossless.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Waveform {
    Dc(f64),
    Sine {
        offset: f64,
        amplitude: f64,
        frequency: f64,
        #[serde(default)]
        delay: f64,
        #[serde(default)]
        damping: f64,
        /// Phase in degrees.
        #[serde(default)]
        phase: f64,
    },
    Pulse {
        v1: f64,
        v2: f64,
        delay: f64,
        rise: f64,
        fall: f64,
        width: f64,
        period: f64,
    },
    /// Piecewise linear: (time, value) pairs, times ascending.
    Pwl(Vec<(f64, f64)>),
}

impl Waveform {
    /// Value at time `t` (seconds).
    pub fn value_at(&self, t: f64) -> f64 {
        match self {
            Waveform::Dc(v) => *v,
            Waveform::Sine {
                offset,
                amplitude,
                frequency,
                delay,
                damping,
                phase,
            } => {
                let ph = phase.to_radians();
                if t < *delay {
                    offset + amplitude * ph.sin()
                } else {
                    let td = t - delay;
                    offset
                        + amplitude
                            * (-td * damping).exp()
                            * (std::f64::consts::TAU * frequency * td + ph).sin()
                }
            }
            Waveform::Pulse {
                v1,
                v2,
                delay,
                rise,
                fall,
                width,
                period,
            } => {
                if t < *delay {
                    return *v1;
                }
                let mut tp = t - delay;
                if *period > 0.0 {
                    tp %= period;
                }
                if tp < *rise {
                    v1 + (v2 - v1) * if *rise > 0.0 { tp / rise } else { 1.0 }
                } else if tp < rise + width {
                    *v2
                } else if tp < rise + width + fall {
                    v2 + (v1 - v2)
                        * if *fall > 0.0 {
                            (tp - rise - width) / fall
                        } else {
                            1.0
                        }
                } else {
                    *v1
                }
            }
            Waveform::Pwl(points) => match points.as_slice() {
                [] => 0.0,
                [first, ..] if t <= first.0 => first.1,
                [.., last] if t >= last.0 => last.1,
                _ => {
                    let i = points.partition_point(|(pt, _)| *pt <= t);
                    let (t0, v0) = points[i - 1];
                    let (t1, v1) = points[i];
                    if t1 > t0 {
                        v0 + (v1 - v0) * (t - t0) / (t1 - t0)
                    } else {
                        v1
                    }
                }
            },
        }
    }

    /// The value used for the DC operating point (t = 0).
    pub fn dc_value(&self) -> f64 {
        self.value_at(0.0)
    }
}

/// What a component is, plus its electrical parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ComponentKind {
    Ground,
    Resistor {
        resistance: f64,
    },
    Capacitor {
        capacitance: f64,
        #[serde(default)]
        initial_voltage: Option<f64>,
    },
    Inductor {
        inductance: f64,
        #[serde(default)]
        initial_current: Option<f64>,
    },
    /// Holds V(+) - V(-) at the waveform value.
    VoltageSource {
        waveform: Waveform,
    },
    /// Drives the waveform value as a current from `in` through the source to
    /// `out`, i.e. out of the `out` pin into the circuit.
    CurrentSource {
        waveform: Waveform,
    },
    Diode {
        model: String,
    },
    Bjt {
        polarity: Polarity,
        model: String,
    },
    Mosfet {
        polarity: Polarity,
        model: String,
    },
    /// Two-terminal switch. A `momentary` one is a push button: it only
    /// leaves its resting state (`closed`) while held down.
    Switch {
        closed: bool,
        r_on: f64,
        r_off: f64,
        #[serde(default)]
        momentary: bool,
    },
    /// Multi-contact switch (see [`SwitchForm`]); `thrown` is the actuated
    /// position.
    MultiSwitch {
        form: SwitchForm,
        thrown: bool,
        r_on: f64,
        r_off: f64,
    },
    /// Total resistance between A and B; `wiper` is 0.0 (at A) to 1.0 (at B).
    Potentiometer {
        resistance: f64,
        wiper: f64,
    },
    /// Voltage-controlled voltage source: V(out+, out-) = gain * V(ctrl+, ctrl-).
    Vcvs {
        gain: f64,
    },
    /// Voltage-controlled current source: a current gm * V(ctrl+, ctrl-) flows
    /// from out+ through the source to out- (SPICE `G` convention).
    Vccs {
        transconductance: f64,
    },
    /// Current-controlled voltage source: V(out) = r * I(ctrl+ -> ctrl-).
    /// The control port is a zero-volt current sensor.
    Ccvs {
        transresistance: f64,
    },
    /// Current-controlled current source: a current gain * I(ctrl+ -> ctrl-)
    /// flows from out+ through the source to out- (SPICE `F` convention).
    /// The control port is a zero-volt current sensor.
    Cccs {
        gain: f64,
    },

    // ---- Integrated circuits ----
    /// Operational amplifier simulated by a built-in macromodel; `part`
    /// names it (see [`OPAMP_PARTS`]).
    OpAmp {
        part: String,
    },
    /// NE555 timer: threshold and trigger comparators on an internal
    /// divider, a set/reset latch, a discharge switch and an output stage.
    Timer555,
    /// CD4066B quad bilateral switch. Each of its four switches joins its
    /// two signal pins, in either direction, while its control pin is high
    /// (above half the supply). `r_on` is the closed resistance, which on
    /// the real chip depends on the supply (see [`CD4066_R_ON`]).
    Cd4066 {
        r_on: f64,
    },

    // ---- Digital parts (5 V CMOS-style logic) ----
    /// Two-input gate; output Y changes `delay` seconds after an input.
    Gate {
        op: GateOp,
        #[serde(default = "default_logic_delay")]
        delay: f64,
    },
    Inverter {
        #[serde(default = "default_logic_delay")]
        delay: f64,
    },
    /// Rising-edge D flip-flop with complementary outputs; starts with Q = 0.
    DFlipFlop {
        #[serde(default = "default_logic_delay")]
        delay: f64,
    },
    /// 4-bit binary counter advancing on each rising clock edge.
    Counter4 {
        #[serde(default = "default_logic_delay")]
        delay: f64,
    },
    /// Square-wave logic source: low, then high for the `duty` fraction at
    /// the end of each period (first rising edge at (1 − duty)/f).
    Clock {
        frequency: f64,
        duty: f64,
    },
    /// A constant logic level; double-click to flip.
    LogicSwitch {
        high: bool,
    },
    /// Lights up when its input is high.
    LogicLed,

    // ---- Microcontrollers ----
    /// ATmega328P (Arduino Uno/Nano) running firmware from `firmware`: the
    /// path of an Intel HEX file (relative to the schematic's folder, or
    /// absolute) or `builtin:<name>`. Powered at 5 V implicitly.
    Mcu {
        firmware: String,
        /// Clock frequency (Hz); must match what the firmware was built for.
        #[serde(default = "default_mcu_clock")]
        clock: f64,
    },
}

/// Op-amps with a built-in macromodel, by part name.
pub const OPAMP_PARTS: &[&str] = &["uA741"];

/// SPICE subcircuit of a built-in op-amp model: (subcircuit name,
/// definition). Ports: in+, in-, V+, V-, out. The simulator's own copy of
/// each model (`dunbrec_sim::macromodel`) is a translation of this text.
pub fn opamp_subckt(part: &str) -> Option<(&'static str, &'static str)> {
    match part.to_ascii_lowercase().as_str() {
        "ua741" | "741" | "lm741" => Some(("UA741", UA741_SUBCKT)),
        _ => None,
    }
}

/// The Boyle macromodel of the uA741, as in the PSpice library.
const UA741_SUBCKT: &str = "\
* uA741 operational amplifier (Boyle macromodel)
* connections: in+ in- V+ V- out
.subckt UA741 1 2 3 4 5
c1 11 12 8.661E-12
c2 6 7 30.00E-12
dc 5 53 dx
de 54 5 dx
dlp 90 91 dx
dln 92 90 dx
dp 4 3 dx
egnd 99 0 poly(2) (3,0) (4,0) 0 .5 .5
fb 7 99 poly(5) vb vc ve vlp vln 0 10.61E6 -10E6 10E6 10E6 -10E6
ga 6 0 11 12 188.5E-6
gcm 0 6 10 99 5.961E-9
iee 10 4 dc 15.16E-6
hlim 90 0 vlim 1K
q1 11 2 13 qx
q2 12 1 14 qx
r2 6 9 100.0E3
rc1 3 11 5.305E3
rc2 3 12 5.305E3
re1 13 10 1.836E3
re2 14 10 1.836E3
ree 10 99 13.19E6
ro1 8 5 50
ro2 7 99 100
rp 3 4 18.16E3
vb 9 0 dc 0
vc 3 53 dc 1
ve 54 4 dc 1
vlim 7 8 dc 0
vlp 91 0 dc 40
vln 0 92 dc 40
.model dx D(Is=800.0E-18 Rs=1)
.model qx NPN(Is=800.0E-18 Bf=93.75)
.ends UA741
";

/// Clock of an Arduino Uno (Hz).
pub const DEFAULT_MCU_CLOCK: f64 = 16e6;

fn default_mcu_clock() -> f64 {
    DEFAULT_MCU_CLOCK
}

/// The contact arrangement of a [`ComponentKind::MultiSwitch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SwitchForm {
    /// Single pole, double throw: the common pin goes to throw 1, or to
    /// throw 2 when thrown.
    Spdt,
    /// Double pole, single throw: two contacts that close together.
    Dpst,
    /// Double pole, double throw: two changeover contacts that move together.
    Dpdt,
}

impl SwitchForm {
    pub const ALL: [SwitchForm; 3] = [SwitchForm::Spdt, SwitchForm::Dpst, SwitchForm::Dpdt];

    pub fn name(self) -> &'static str {
        match self {
            SwitchForm::Spdt => "SPDT",
            SwitchForm::Dpst => "DPST",
            SwitchForm::Dpdt => "DPDT",
        }
    }

    /// The contacts as `(pin, pin, closed_when_thrown)`: the contact joins
    /// the two pins when `thrown` equals the third element.
    pub fn contacts(self) -> &'static [(usize, usize, bool)] {
        match self {
            SwitchForm::Spdt => &[(0, 1, false), (0, 2, true)],
            SwitchForm::Dpst => &[(0, 1, true), (2, 3, true)],
            SwitchForm::Dpdt => &[(0, 1, false), (0, 2, true), (3, 4, false), (3, 5, true)],
        }
    }
}

impl ComponentKind {
    pub fn pins(&self) -> &'static [PinDef] {
        match self {
            ComponentKind::Ground => GROUND,
            ComponentKind::Resistor { .. }
            | ComponentKind::Capacitor { .. }
            | ComponentKind::Inductor { .. }
            | ComponentKind::Switch { .. } => TWO_TERMINAL,
            ComponentKind::VoltageSource { .. } => VOLTAGE_SOURCE,
            ComponentKind::CurrentSource { .. } => CURRENT_SOURCE,
            ComponentKind::Diode { .. } => DIODE,
            ComponentKind::Bjt { .. } => BJT,
            ComponentKind::Mosfet { .. } => MOSFET,
            ComponentKind::MultiSwitch { form, .. } => match form {
                SwitchForm::Spdt => SPDT,
                SwitchForm::Dpst => DPST,
                SwitchForm::Dpdt => DPDT,
            },
            ComponentKind::Potentiometer { .. } => POT,
            ComponentKind::Vcvs { .. }
            | ComponentKind::Vccs { .. }
            | ComponentKind::Ccvs { .. }
            | ComponentKind::Cccs { .. } => CONTROLLED,
            ComponentKind::Gate { .. } => GATE2,
            ComponentKind::Inverter { .. } => INVERTER,
            ComponentKind::DFlipFlop { .. } => DFF,
            ComponentKind::Counter4 { .. } => COUNTER4,
            ComponentKind::Clock { .. } | ComponentKind::LogicSwitch { .. } => LOGIC_SOURCE,
            ComponentKind::LogicLed => LOGIC_LED,
            ComponentKind::Mcu { .. } => MCU,
            ComponentKind::OpAmp { .. } => OPAMP,
            ComponentKind::Timer555 => TIMER555,
            ComponentKind::Cd4066 { .. } => CD4066_PINS,
        }
    }

    /// Conventional designator prefix ("R", "C", "Q", ...). Ground has none.
    pub fn designator_prefix(&self) -> &'static str {
        match self {
            ComponentKind::Ground => "",
            ComponentKind::Resistor { .. } => "R",
            ComponentKind::Capacitor { .. } => "C",
            ComponentKind::Inductor { .. } => "L",
            ComponentKind::VoltageSource { .. } => "V",
            ComponentKind::CurrentSource { .. } => "I",
            ComponentKind::Diode { .. } => "D",
            ComponentKind::Bjt { .. } => "Q",
            ComponentKind::Mosfet { .. } => "M",
            ComponentKind::Switch { .. } | ComponentKind::MultiSwitch { .. } => "S",
            ComponentKind::Potentiometer { .. } => "RV",
            ComponentKind::Vcvs { .. } => "E",
            ComponentKind::Vccs { .. } => "G",
            ComponentKind::Ccvs { .. } => "H",
            ComponentKind::Cccs { .. } => "F",
            ComponentKind::Gate { .. }
            | ComponentKind::Inverter { .. }
            | ComponentKind::DFlipFlop { .. }
            | ComponentKind::Counter4 { .. }
            | ComponentKind::Mcu { .. }
            | ComponentKind::OpAmp { .. }
            | ComponentKind::Timer555
            | ComponentKind::Cd4066 { .. } => "U",
            ComponentKind::Clock { .. } => "CLK",
            ComponentKind::LogicSwitch { .. } => "SW",
            ComponentKind::LogicLed => "LED",
        }
    }

    /// True for logic parts and microcontrollers, which are simulated by
    /// event-driven engines and meet the analog circuit at their pins.
    pub fn is_digital(&self) -> bool {
        matches!(
            self,
            ComponentKind::Gate { .. }
                | ComponentKind::Inverter { .. }
                | ComponentKind::DFlipFlop { .. }
                | ComponentKind::Counter4 { .. }
                | ComponentKind::Clock { .. }
                | ComponentKind::LogicSwitch { .. }
                | ComponentKind::LogicLed
                | ComponentKind::Mcu { .. }
        )
    }

    /// Name of the device model this component refers to, if any.
    pub fn model_name(&self) -> Option<&str> {
        match self {
            ComponentKind::Diode { model }
            | ComponentKind::Bjt { model, .. }
            | ComponentKind::Mosfet { model, .. } => Some(model),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Component {
    pub id: ComponentId,
    /// Reference designator, e.g. "R1". Empty for ground symbols.
    pub designator: String,
    pub kind: ComponentKind,
    pub position: Point,
    #[serde(default)]
    pub rotation: Rotation,
    /// Mirror across the vertical axis, applied before rotation.
    #[serde(default)]
    pub mirrored: bool,
}

impl Component {
    /// Transform a local offset into schematic coordinates.
    pub fn transform(&self, offset: Point) -> Point {
        let m = if self.mirrored {
            Point::new(-offset.x, offset.y)
        } else {
            offset
        };
        self.position + self.rotation.apply(m)
    }

    /// Absolute positions of all pins, in [`ComponentKind::pins`] order.
    pub fn pin_positions(&self) -> impl Iterator<Item = (&'static PinDef, Point)> + '_ {
        self.kind
            .pins()
            .iter()
            .map(move |p| (p, self.transform(p.offset)))
    }
}

/// A straight wire segment. Wires connect at their endpoints; an endpoint (or
/// pin, or label) touching the middle of a wire forms a T-junction. Two wires
/// that merely cross are *not* connected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wire {
    pub id: WireId,
    pub a: Point,
    pub b: Point,
}

impl Wire {
    /// True if `p` lies strictly between the endpoints of this wire.
    pub fn contains_interior(&self, p: Point) -> bool {
        if p == self.a || p == self.b || self.a == self.b {
            return false;
        }
        let (d, e) = (self.b - self.a, p - self.a);
        let cross = d.x as i64 * e.y as i64 - d.y as i64 * e.x as i64;
        if cross != 0 {
            return false;
        }
        let dot = d.x as i64 * e.x as i64 + d.y as i64 * e.y as i64;
        let len2 = d.x as i64 * d.x as i64 + d.y as i64 * d.y as i64;
        dot > 0 && dot < len2
    }
}

/// A named net label. All labels with the same name are connected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetLabel {
    pub id: LabelId,
    pub name: String,
    pub position: Point,
}

/// The language of a [`SourceFile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceLanguage {
    /// AVR assembly, built by the assembler inside Dunbrec.
    Asm,
    /// An Arduino C++ sketch, built by an external toolchain.
    Cpp,
}

/// Firmware source text saved with the schematic. A microcontroller refers
/// to one by name with the firmware reference `source:<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    pub language: SourceLanguage,
    pub text: String,
}

/// Prefix of a firmware reference that names a [`SourceFile`] of the
/// schematic.
pub const SOURCE_PREFIX: &str = "source:";

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Schematic {
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub wires: Vec<Wire>,
    #[serde(default)]
    pub labels: Vec<NetLabel>,
    #[serde(default)]
    pub models: ModelLibrary,
    /// Firmware source files by name. Files saved before this field existed
    /// load with none.
    #[serde(default)]
    pub sources: BTreeMap<String, SourceFile>,
    /// Next id to hand out; shared by components, wires and labels.
    #[serde(default)]
    next_id: u32,
}

impl Schematic {
    pub fn new() -> Self {
        Self::default()
    }

    fn alloc_id(&mut self) -> u32 {
        let floor = self
            .components
            .iter()
            .map(|c| c.id.0)
            .chain(self.wires.iter().map(|w| w.id.0))
            .chain(self.labels.iter().map(|l| l.id.0))
            .max()
            .map_or(0, |m| m + 1);
        let id = self.next_id.max(floor);
        self.next_id = id + 1;
        id
    }

    /// Add a component, assigning the next free designator ("R1", "R2", ...).
    pub fn add_component(&mut self, kind: ComponentKind, position: Point) -> ComponentId {
        let designator = self.next_designator(kind.designator_prefix());
        self.add_component_named(kind, designator, position)
    }

    /// Add a component with an explicit designator (used by importers).
    pub fn add_component_named(
        &mut self,
        kind: ComponentKind,
        designator: impl Into<String>,
        position: Point,
    ) -> ComponentId {
        let id = ComponentId(self.alloc_id());
        self.components.push(Component {
            id,
            designator: designator.into(),
            kind,
            position,
            rotation: Rotation::R0,
            mirrored: false,
        });
        id
    }

    pub fn add_wire(&mut self, a: Point, b: Point) -> WireId {
        let id = WireId(self.alloc_id());
        self.wires.push(Wire { id, a, b });
        id
    }

    pub fn add_label(&mut self, name: impl Into<String>, position: Point) -> LabelId {
        let id = LabelId(self.alloc_id());
        self.labels.push(NetLabel {
            id,
            name: name.into(),
            position,
        });
        id
    }

    pub fn component(&self, id: ComponentId) -> Option<&Component> {
        self.components.iter().find(|c| c.id == id)
    }

    pub fn component_mut(&mut self, id: ComponentId) -> Option<&mut Component> {
        self.components.iter_mut().find(|c| c.id == id)
    }

    pub fn remove_component(&mut self, id: ComponentId) -> Option<Component> {
        let i = self.components.iter().position(|c| c.id == id)?;
        Some(self.components.remove(i))
    }

    pub fn remove_wire(&mut self, id: WireId) -> Option<Wire> {
        let i = self.wires.iter().position(|w| w.id == id)?;
        Some(self.wires.remove(i))
    }

    pub fn remove_label(&mut self, id: LabelId) -> Option<NetLabel> {
        let i = self.labels.iter().position(|l| l.id == id)?;
        Some(self.labels.remove(i))
    }

    /// Lowest unused designator with the given prefix, e.g. "R3".
    pub fn next_designator(&self, prefix: &str) -> String {
        if prefix.is_empty() {
            return String::new();
        }
        let used: std::collections::HashSet<u32> = self
            .components
            .iter()
            .filter(|c| c.kind.designator_prefix() == prefix)
            .filter_map(|c| c.designator.strip_prefix(prefix)?.parse().ok())
            .collect();
        let n = (1..).find(|n| !used.contains(n)).unwrap_or(1);
        format!("{prefix}{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn rotation_and_mirror() {
        let mut s = Schematic::new();
        let id = s.add_component(
            ComponentKind::Resistor { resistance: 1e3 },
            Point::new(10, 10),
        );
        let pins: Vec<Point> = s
            .component(id)
            .unwrap()
            .pin_positions()
            .map(|(_, p)| p)
            .collect();
        assert_eq!(pins, vec![Point::new(8, 10), Point::new(12, 10)]);

        let c = s.component_mut(id).unwrap();
        c.rotation = Rotation::R90;
        let pins: Vec<Point> = c.pin_positions().map(|(_, p)| p).collect();
        assert_eq!(pins, vec![Point::new(10, 8), Point::new(10, 12)]);

        c.rotation = Rotation::R0;
        c.mirrored = true;
        let pins: Vec<Point> = c.pin_positions().map(|(_, p)| p).collect();
        assert_eq!(pins, vec![Point::new(12, 10), Point::new(8, 10)]);
    }

    #[test]
    fn rotation_cycles() {
        let mut r = Rotation::R0;
        for _ in 0..4 {
            r = r.rotated_cw();
        }
        assert_eq!(r, Rotation::R0);
        assert_eq!(Rotation::R0.rotated_ccw(), Rotation::R90);
    }

    #[test]
    fn designators_fill_gaps() {
        let mut s = Schematic::new();
        let r1 = s.add_component(
            ComponentKind::Resistor { resistance: 1.0 },
            Point::default(),
        );
        s.add_component(
            ComponentKind::Resistor { resistance: 1.0 },
            Point::default(),
        );
        s.add_component(
            ComponentKind::Capacitor {
                capacitance: 1e-6,
                initial_voltage: None,
            },
            Point::default(),
        );
        s.remove_component(r1);
        assert_eq!(s.next_designator("R"), "R1");
        assert_eq!(s.next_designator("C"), "C2");
        let g = s.add_component(ComponentKind::Ground, Point::default());
        assert_eq!(s.component(g).unwrap().designator, "");
    }

    #[test]
    fn wire_interior() {
        let w = Wire {
            id: WireId(0),
            a: Point::new(0, 0),
            b: Point::new(10, 0),
        };
        assert!(w.contains_interior(Point::new(5, 0)));
        assert!(!w.contains_interior(Point::new(0, 0)));
        assert!(!w.contains_interior(Point::new(10, 0)));
        assert!(!w.contains_interior(Point::new(5, 1)));
        assert!(!w.contains_interior(Point::new(11, 0)));
    }

    #[test]
    fn waveforms() {
        let pulse = Waveform::Pulse {
            v1: 0.0,
            v2: 5.0,
            delay: 1.0,
            rise: 1.0,
            fall: 1.0,
            width: 2.0,
            period: 10.0,
        };
        assert_relative_eq!(pulse.value_at(0.5), 0.0);
        assert_relative_eq!(pulse.value_at(1.5), 2.5);
        assert_relative_eq!(pulse.value_at(3.0), 5.0);
        assert_relative_eq!(pulse.value_at(4.5), 2.5);
        assert_relative_eq!(pulse.value_at(6.0), 0.0);
        assert_relative_eq!(pulse.value_at(13.0), 5.0);

        let sine = Waveform::Sine {
            offset: 1.0,
            amplitude: 2.0,
            frequency: 1.0,
            delay: 0.0,
            damping: 0.0,
            phase: 0.0,
        };
        assert_relative_eq!(sine.value_at(0.25), 3.0, epsilon = 1e-12);
        assert_relative_eq!(sine.dc_value(), 1.0);

        let pwl = Waveform::Pwl(vec![(0.0, 0.0), (1.0, 10.0), (2.0, 10.0)]);
        assert_relative_eq!(pwl.value_at(-1.0), 0.0);
        assert_relative_eq!(pwl.value_at(0.5), 5.0);
        assert_relative_eq!(pwl.value_at(1.5), 10.0);
        assert_relative_eq!(pwl.value_at(9.0), 10.0);
    }
}
