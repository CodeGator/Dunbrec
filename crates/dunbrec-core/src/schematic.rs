//! The graphical schematic: components placed on a grid, wires and net labels.
//!
//! Coordinates are integer grid units. The simulator never sees this layer
//! directly; it works on the [`Netlist`](crate::netlist::Netlist) produced by
//! [`build_netlist`](crate::netlist::build_netlist).

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

/// Which simulation domain a pin belongs to. Only analog exists today; digital
/// pins arrive with the mixed-signal phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PinDomain {
    Analog,
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
const POT: &[PinDef] = &[pin("A", -2, 0), pin("B", 2, 0), pin("W", 0, -2)];
const CONTROLLED: &[PinDef] = &[
    pin("out+", 2, -2),
    pin("out-", 2, 2),
    pin("ctrl+", -2, -2),
    pin("ctrl-", -2, 2),
];

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
    Switch {
        closed: bool,
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
            ComponentKind::Potentiometer { .. } => POT,
            ComponentKind::Vcvs { .. }
            | ComponentKind::Vccs { .. }
            | ComponentKind::Ccvs { .. }
            | ComponentKind::Cccs { .. } => CONTROLLED,
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
            ComponentKind::Switch { .. } => "S",
            ComponentKind::Potentiometer { .. } => "RV",
            ComponentKind::Vcvs { .. } => "E",
            ComponentKind::Vccs { .. } => "G",
            ComponentKind::Ccvs { .. } => "H",
            ComponentKind::Cccs { .. } => "F",
        }
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
