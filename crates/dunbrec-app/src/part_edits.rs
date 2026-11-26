//! Adding and removing parts on the assistant's suggestion.
//!
//! The assistant lists parts with the net each pin joins, in pin order:
//!
//! ````text
//! ```dunbrec-add
//! R5 resistor 10k VOUT 0
//! Q2 npn 2N3904 C1 B1 0
//! ```
//!
//! ```dunbrec-remove
//! R3
//! ```
//! ````
//!
//! A part with two pins that join nets the circuit already has is drawn next
//! to them with real wires: positions near the pins it must reach are tried
//! in each rotation, wires run at right angles to a pin already on each net
//! (or to an existing ground symbol), and the tidiest drawing that passes the
//! check below is used. Any other part is laid out in empty space below the
//! drawing. Like an imported netlist, each of its pins gets a short stub
//! ending in a net label (or a ground symbol for net `0`), and same-named
//! labels are connected. A net the circuit never named (`N001`) first gets a
//! label of its own on one of its pins.
//!
//! Either way the netlist is checked afterwards: every new pin must be on the
//! net that was asked for, and no two nets that were separate may have been
//! joined. Otherwise nothing is changed.

use std::collections::{HashMap, HashSet};

use dunbrec_core::units::{format_value, parse_eng};
use dunbrec_core::{
    ComponentId, ComponentKind as K, DEFAULT_LOGIC_DELAY, GateOp, ModelKind, ModelLibrary, Netlist,
    NodeId, OPAMP_PARTS, Point, Polarity, Rotation, Schematic, Waveform, build_netlist,
};
use dunbrec_io::layout::{self, Part, STUB, is_ground, outward};
use dunbrec_io::tidy;

use crate::canvas::NodeMap;
use crate::library::{
    self, GENERIC_DIODE, GENERIC_NMOS, GENERIC_NPN, GENERIC_PMOS, GENERIC_PNP, SWITCH_R_OFF,
    SWITCH_R_ON,
};

/// Fence tag of a block that adds parts.
pub const ADD_TAG: &str = "dunbrec-add";
/// Fence tag of a block that removes parts.
pub const REMOVE_TAG: &str = "dunbrec-remove";

/// Rows between the bottom of the existing drawing and the new parts.
const GAP: i32 = 12;

/// A part to add.
#[derive(Debug, Clone, PartialEq)]
pub struct NewPart {
    pub designator: String,
    pub kind: K,
    /// The net of each pin, in pin order.
    pub nets: Vec<String>,
    /// "resistor 10 kΩ", for the card.
    pub summary: String,
}

fn skip(line: &str) -> bool {
    line.is_empty() || line.starts_with('#') || line.starts_with("//")
}

/// Read the lines of a `dunbrec-add` block.
pub fn parse_add(body: &str) -> Result<Vec<NewPart>, String> {
    let mut parts = Vec::new();
    for raw in body.lines() {
        let line = raw.trim();
        if skip(line) {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.len() < 4 {
            return Err(format!(
                "“{line}” should read: DESIGNATOR kind value net net …"
            ));
        }
        let designator = words[0];
        if !designator
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
            || !designator.starts_with(|c: char| c.is_ascii_alphabetic())
        {
            return Err(format!("“{designator}” is not a usable designator."));
        }
        let (kind, summary) = build_kind(&words[1].to_ascii_lowercase(), words[2])
            .map_err(|e| format!("{designator}: {e}"))?;
        let nets: Vec<String> = words[3..].iter().map(|n| n.to_string()).collect();
        if let Some(bad) = nets.iter().find(|n| n.contains('=')) {
            return Err(format!(
                "{designator}: write the nets in pin order, without “{bad}”."
            ));
        }
        let pins = kind.pins();
        if nets.len() != pins.len() {
            let names: Vec<&str> = pins.iter().map(|p| p.name).collect();
            return Err(format!(
                "{designator}: a {} has {} pins ({}) but {} nets were given.",
                words[1],
                pins.len(),
                names.join(" "),
                nets.len()
            ));
        }
        parts.push(NewPart {
            designator: designator.to_string(),
            kind,
            nets,
            summary,
        });
    }
    if parts.is_empty() {
        return Err("The block has no parts in it.".into());
    }
    Ok(parts)
}

/// Read the lines of a `dunbrec-remove` block: one designator per line.
pub fn parse_remove(body: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for raw in body.lines() {
        let line = raw.trim();
        if skip(line) {
            continue;
        }
        if line.split_whitespace().count() != 1 {
            return Err(format!("“{line}”: give one designator per line."));
        }
        names.push(line.to_string());
    }
    if names.is_empty() {
        return Err("The block has no parts in it.".into());
    }
    Ok(names)
}

/// The parts that can be added: the word that names each, a value that is
/// valid for it, and what the value means. The assistant is told this list,
/// with the pin order read from the real part, so the two cannot disagree.
const KINDS: &[(&str, &str, &str)] = &[
    ("resistor", "1k", "ohms"),
    ("capacitor", "100n", "farads"),
    ("inductor", "1m", "henries"),
    ("potentiometer", "10k", "total ohms"),
    ("switch", "open", "open or closed"),
    ("vsource", "5", "DC volts"),
    ("vsine", "0.1@1k", "sine source: peak volts@hertz"),
    ("isource", "1m", "DC amps"),
    ("diode", "generic", "generic, or a model such as 1N4148"),
    ("npn", "generic", "generic, or 2N3904"),
    ("pnp", "generic", "generic, or 2N3906"),
    ("nmos", "generic", "generic"),
    ("pmos", "generic", "generic"),
    ("opamp", "uA741", "the part (uA741)"),
    ("timer555", "default", "the word default"),
    ("and", "default", "default, or a delay such as 10n"),
    ("or", "default", "default, or a delay such as 10n"),
    ("nand", "default", "default, or a delay such as 10n"),
    ("nor", "default", "default, or a delay such as 10n"),
    ("xor", "default", "default, or a delay such as 10n"),
    ("xnor", "default", "default, or a delay such as 10n"),
    ("not", "default", "default, or a delay such as 10n"),
    ("dff", "default", "default, or a delay such as 10n"),
    ("counter4", "default", "default, or a delay such as 10n"),
    ("clock", "1k", "frequency in hertz"),
    ("logicswitch", "low", "low or high"),
    ("logicled", "default", "the word default"),
];

/// The kinds, values and pin orders, one per line, for the assistant.
pub fn help() -> String {
    KINDS
        .iter()
        .filter_map(|(word, example, meaning)| {
            let (kind, _) = build_kind(word, example).ok()?;
            let pins: Vec<&str> = kind.pins().iter().map(|p| p.name).collect();
            Some(format!(
                "- {word}: value is {meaning}; pins in order: {}",
                pins.join(" ")
            ))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_kind(word: &str, value: &str) -> Result<(K, String), String> {
    let number = || parse_eng(value).map_err(|e| e.to_string());
    let positive = || {
        number().and_then(|v| {
            if v > 0.0 && v.is_finite() {
                Ok(v)
            } else {
                Err("the value must be above zero.".to_string())
            }
        })
    };
    // Words a model often uses for the kinds listed in KINDS.
    let word = match word {
        "voltage" | "battery" | "dc" | "supply" | "vdc" | "voltagesource" => "vsource",
        "current" | "currentsource" => "isource",
        "sine" | "ac" | "vac" | "vsin" | "sinesource" | "signal" => "vsine",
        "cap" => "capacitor",
        "res" => "resistor",
        "ind" => "inductor",
        "pot" => "potentiometer",
        "bjt" | "npn_bjt" => "npn",
        other => other,
    };
    Ok(match word {
        "resistor" => {
            let v = positive()?;
            (
                K::Resistor { resistance: v },
                format!("resistor {}", format_value(v, "Ω")),
            )
        }
        "capacitor" => {
            let v = positive()?;
            (
                K::Capacitor {
                    capacitance: v,
                    initial_voltage: None,
                },
                format!("capacitor {}", format_value(v, "F")),
            )
        }
        "inductor" => {
            let v = positive()?;
            (
                K::Inductor {
                    inductance: v,
                    initial_current: None,
                },
                format!("inductor {}", format_value(v, "H")),
            )
        }
        "potentiometer" => {
            let v = positive()?;
            (
                K::Potentiometer {
                    resistance: v,
                    wiper: 0.5,
                },
                format!("potentiometer {}", format_value(v, "Ω")),
            )
        }
        "switch" => {
            let closed = match value.to_ascii_lowercase().as_str() {
                "closed" => true,
                "open" => false,
                _ => return Err("a switch is either open or closed.".into()),
            };
            (
                K::Switch {
                    closed,
                    r_on: SWITCH_R_ON,
                    r_off: SWITCH_R_OFF,
                    momentary: false,
                },
                format!("switch, {}", value.to_ascii_lowercase()),
            )
        }
        "vsource" => {
            let v = number()?;
            (
                K::VoltageSource {
                    waveform: Waveform::Dc(v),
                },
                format!("DC voltage source {}", format_value(v, "V")),
            )
        }
        "vsine" => {
            let (amp, freq) = value
                .split_once('@')
                .ok_or("write the value as amplitude@frequency, such as 0.1@1k.")?;
            let amplitude = parse_eng(amp).map_err(|e| e.to_string())?;
            let frequency = parse_eng(freq).map_err(|e| e.to_string())?;
            if !(amplitude.is_finite() && frequency > 0.0 && frequency.is_finite()) {
                return Err("the amplitude must be a number and the frequency above zero.".into());
            }
            (
                K::VoltageSource {
                    waveform: Waveform::Sine {
                        offset: 0.0,
                        amplitude,
                        frequency,
                        delay: 0.0,
                        damping: 0.0,
                        phase: 0.0,
                    },
                },
                format!(
                    "sine source {} peak at {}",
                    format_value(amplitude, "V"),
                    format_value(frequency, "Hz")
                ),
            )
        }
        "isource" => {
            let v = number()?;
            (
                K::CurrentSource {
                    waveform: Waveform::Dc(v),
                },
                format!("DC current source {}", format_value(v, "A")),
            )
        }
        "diode" => {
            let model = model_name(value, ModelKind::Diode, GENERIC_DIODE, |m| K::Diode {
                model: m,
            })?;
            (
                K::Diode {
                    model: model.clone(),
                },
                format!("diode {model}"),
            )
        }
        "npn" | "pnp" => {
            let (polarity, expected, generic) = if word == "npn" {
                (Polarity::N, ModelKind::Npn, GENERIC_NPN)
            } else {
                (Polarity::P, ModelKind::Pnp, GENERIC_PNP)
            };
            let model = model_name(value, expected, generic, |m| K::Bjt { polarity, model: m })?;
            (
                K::Bjt {
                    polarity,
                    model: model.clone(),
                },
                format!("{word} transistor {model}"),
            )
        }
        "nmos" | "pmos" => {
            let (polarity, expected, generic) = if word == "nmos" {
                (Polarity::N, ModelKind::Nmos, GENERIC_NMOS)
            } else {
                (Polarity::P, ModelKind::Pmos, GENERIC_PMOS)
            };
            let model = model_name(value, expected, generic, |m| K::Mosfet {
                polarity,
                model: m,
            })?;
            (
                K::Mosfet {
                    polarity,
                    model: model.clone(),
                },
                format!("{word} {model}"),
            )
        }
        "opamp" => {
            let canonical = dunbrec_core::schematic::opamp_subckt(value).map(|(name, _)| name);
            let part = OPAMP_PARTS
                .iter()
                .find(|p| {
                    canonical.is_some()
                        && dunbrec_core::schematic::opamp_subckt(p).map(|(name, _)| name)
                            == canonical
                })
                .ok_or_else(|| format!("there is no op-amp called {value}; use uA741."))?;
            (
                K::OpAmp {
                    part: part.to_string(),
                },
                format!("op-amp {part}"),
            )
        }
        "timer555" => (K::Timer555, "555 timer".to_string()),
        "and" | "or" | "nand" | "nor" | "xor" | "xnor" => {
            let op = match word {
                "and" => GateOp::And,
                "or" => GateOp::Or,
                "nand" => GateOp::Nand,
                "nor" => GateOp::Nor,
                "xor" => GateOp::Xor,
                _ => GateOp::Xnor,
            };
            (
                K::Gate {
                    op,
                    delay: logic_delay(value)?,
                },
                format!("{} gate", word.to_ascii_uppercase()),
            )
        }
        "not" => (
            K::Inverter {
                delay: logic_delay(value)?,
            },
            "inverter".to_string(),
        ),
        "dff" => (
            K::DFlipFlop {
                delay: logic_delay(value)?,
            },
            "D flip-flop".to_string(),
        ),
        "counter4" => (
            K::Counter4 {
                delay: logic_delay(value)?,
            },
            "4-bit counter".to_string(),
        ),
        "clock" => {
            let f = positive()?;
            (
                K::Clock {
                    frequency: f,
                    duty: 0.5,
                },
                format!("clock {}", format_value(f, "Hz")),
            )
        }
        "logicswitch" => {
            let high = match value.to_ascii_lowercase().as_str() {
                "high" => true,
                "low" => false,
                _ => return Err("a logic switch is either low or high.".into()),
            };
            (
                K::LogicSwitch { high },
                format!("logic switch, {}", value.to_ascii_lowercase()),
            )
        }
        "logicled" => (K::LogicLed, "logic LED".to_string()),
        other => {
            let words: Vec<&str> = KINDS.iter().map(|k| k.0).collect();
            return Err(format!(
                "“{other}” is not a part I can add. Use: {}.",
                words.join(", ")
            ));
        }
    })
}

/// The delay of a logic part: the word `default`, or seconds.
fn logic_delay(value: &str) -> Result<f64, String> {
    if value.eq_ignore_ascii_case("default") {
        return Ok(DEFAULT_LOGIC_DELAY);
    }
    match parse_eng(value) {
        Ok(v) if v > 0.0 && v.is_finite() => Ok(v),
        Ok(_) => Err("the delay must be above zero.".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// The library's spelling of the model `value` (or the generic model for
/// the word `generic`), if it exists and is the `expected` kind of model.
fn model_name(
    value: &str,
    expected: ModelKind,
    generic: &str,
    make: impl Fn(String) -> K,
) -> Result<String, String> {
    let name = if value.eq_ignore_ascii_case("generic") {
        generic.to_string()
    } else {
        value.to_string()
    };
    let mut probe = Schematic::new();
    library::ensure_model(&mut probe, &make(name.clone()));
    match probe.models.get(&name) {
        None => Err(format!(
            "there is no model called {value}; use generic or a library part number."
        )),
        Some(m) if m.kind != expected => Err(format!("{value} is not the right kind of model.")),
        Some(m) => Ok(m.name.clone()),
    }
}

/// How a net a new part asks for relates to the circuit.
#[derive(Debug, Clone)]
enum Net {
    /// A net the circuit already has.
    Existing {
        old: NodeId,
        name: String,
        labelled: bool,
    },
    /// A net only the new parts share.
    New,
}

fn classify(before: &Netlist, net: &str) -> Net {
    match before
        .nodes
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, n)| n.name.eq_ignore_ascii_case(net))
    {
        Some((i, n)) => Net::Existing {
            old: NodeId(i),
            name: n.name.clone(),
            labelled: n.labelled,
        },
        None => Net::New,
    }
}

/// Add `parts` to `s`. A part with two pins that join nets the circuit
/// already has is drawn next to them with real wires; any other part goes in
/// empty space below the drawing with net labels. Returns notes saying what
/// was done. If anything is wrong, `s` may have been changed: callers use a
/// copy first (see [`check_add`]).
pub fn add_parts(s: &mut Schematic, parts: &[NewPart]) -> Result<Vec<String>, String> {
    let mut taken: HashSet<String> = s
        .components
        .iter()
        .map(|c| c.designator.to_ascii_uppercase())
        .collect();
    for p in parts {
        if !taken.insert(p.designator.to_ascii_uppercase()) {
            return Err(format!("There is already a part called {}.", p.designator));
        }
    }

    let before = build_netlist(s);
    let mut nets: HashMap<String, Net> = HashMap::new();
    for net in parts.iter().flat_map(|p| &p.nets) {
        if !is_ground(net) {
            nets.entry(net.clone())
                .or_insert_with(|| classify(&before, net));
        }
    }

    let mut notes = Vec::new();
    let mut leftover: Vec<&NewPart> = Vec::new();
    let map = NodeMap::new(s, &before);
    for p in parts {
        match try_wire(s, &before, &map, &nets, p) {
            Some(note) => notes.push(note),
            None => leftover.push(p),
        }
    }
    if !leftover.is_empty() && !place_tidy(s, &before, &nets, &leftover, &mut notes) {
        place_with_labels(s, &before, &nets, &leftover, &mut notes);
    }
    for p in parts {
        library::ensure_model(s, &p.kind);
    }
    verify(s, &before, parts, &nets)?;
    Ok(notes)
}

/// Draw the parts in free space below the drawing, each pin on a stub that
/// ends in a net label (or a ground symbol).
fn place_with_labels(
    s: &mut Schematic,
    before: &Netlist,
    nets: &HashMap<String, Net>,
    parts: &[&NewPart],
    notes: &mut Vec<String>,
) {
    struct Anchor {
        label: String,
        component: ComponentId,
        pin: usize,
    }
    let mut anchors: Vec<Anchor> = Vec::new();
    let mut label_of: HashMap<String, String> = HashMap::new();
    let mut used: HashSet<String> = s.labels.iter().map(|l| l.name.clone()).collect();
    for net in parts.iter().flat_map(|p| &p.nets) {
        if is_ground(net) || label_of.contains_key(net) {
            continue;
        }
        let label = match nets.get(net) {
            Some(Net::Existing {
                name,
                labelled: true,
                ..
            }) => name.clone(),
            // A net the circuit never named gets a label on one of its pins.
            Some(Net::Existing { old, name, .. }) => {
                let spot = before.elements.iter().find_map(|e| {
                    e.nodes
                        .iter()
                        .position(|id| id == old)
                        .map(|pin| (e.component, pin))
                });
                match spot {
                    Some((component, pin)) => {
                        let mut label = format!("X_{name}");
                        while !used.insert(label.clone()) {
                            label.push('x');
                        }
                        anchors.push(Anchor {
                            label: label.clone(),
                            component,
                            pin,
                        });
                        label
                    }
                    None => name.clone(),
                }
            }
            _ => net.clone(),
        };
        label_of.insert(net.clone(), label);
    }

    let layout_parts: Vec<Part> = parts
        .iter()
        .map(|p| Part {
            designator: p.designator.clone(),
            kind: p.kind.clone(),
            nets: p
                .nets
                .iter()
                .map(|n| label_of.get(n).cloned().unwrap_or_else(|| n.clone()))
                .collect(),
            position: None,
            rotation: Rotation::R0,
        })
        .collect();
    paste(s, &layout::layout(&layout_parts, ModelLibrary::new()));

    for a in &anchors {
        let Some(c) = s.component(a.component) else {
            continue;
        };
        let Some((_, at)) = c.pin_positions().nth(a.pin) else {
            continue;
        };
        let away = outward(Point::new(at.x - c.position.x, at.y - c.position.y));
        let end = Point::new(at.x + away.x * STUB, at.y + away.y * STUB);
        s.add_wire(at, end);
        s.add_label(a.label.clone(), end);
        notes.push(format!(
            "Named a net {} so the new parts can join it.",
            a.label
        ));
    }
    let names: Vec<&str> = parts.iter().map(|p| p.designator.as_str()).collect();
    notes.push(format!(
        "{} placed below the drawing and joined by net labels{}.",
        names.join(", "),
        if parts.len() == 1 {
            ""
        } else {
            " (move them if you want it tidier)"
        }
    ));
}

/// Copy a drawing made on its own into `s`, in the free space below what is
/// already drawn.
fn paste(s: &mut Schematic, placed: &Schematic) {
    let (dx, dy) = free_space_below(s);
    let shift = |p: Point| Point::new(p.x + dx, p.y + dy);
    for c in &placed.components {
        let id = s.add_component_named(c.kind.clone(), c.designator.clone(), shift(c.position));
        if let Some(new) = s.component_mut(id) {
            new.rotation = c.rotation;
            new.mirrored = c.mirrored;
        }
    }
    for w in &placed.wires {
        s.add_wire(shift(w.a), shift(w.b));
    }
    for l in &placed.labels {
        s.add_label(l.name.clone(), shift(l.position));
    }
}

/// Draw parts that only join each other (and ground) the way a person would:
/// rails, columns and real wires. Only used when the result passes the same
/// check as everything else; otherwise `s` is left alone and false returned.
fn place_tidy(
    s: &mut Schematic,
    before: &Netlist,
    nets: &HashMap<String, Net>,
    parts: &[&NewPart],
    notes: &mut Vec<String>,
) -> bool {
    let only_new = parts
        .iter()
        .flat_map(|p| &p.nets)
        .all(|n| is_ground(n) || matches!(nets.get(n), Some(Net::New)));
    if !only_new {
        return false;
    }
    let layout_parts: Vec<Part> = parts
        .iter()
        .map(|p| Part {
            designator: p.designator.clone(),
            kind: p.kind.clone(),
            nets: p.nets.clone(),
            position: None,
            rotation: Rotation::R0,
        })
        .collect();
    let Some(drawing) = tidy::tidy(&layout_parts, ModelLibrary::new()) else {
        return false;
    };
    let mut trial = s.clone();
    paste(&mut trial, &drawing);
    for p in parts {
        library::ensure_model(&mut trial, &p.kind);
    }
    let owned: Vec<NewPart> = parts.iter().map(|p| (*p).clone()).collect();
    if verify(&trial, before, &owned, nets).is_err() {
        return false;
    }
    *s = trial;
    let names: Vec<&str> = parts.iter().map(|p| p.designator.as_str()).collect();
    notes.push(format!(
        "{} drawn below the drawing with wires, a supply rail and ground symbols.",
        names.join(", ")
    ));
    true
}

/// Where the labelled parts start: under everything that is drawn, at its left.
fn free_space_below(s: &Schematic) -> (i32, i32) {
    let mut points: Vec<Point> = s.components.iter().map(|c| c.position).collect();
    points.extend(s.wires.iter().flat_map(|w| [w.a, w.b]));
    points.extend(s.labels.iter().map(|l| l.position));
    match (
        points.iter().map(|p| p.x).min(),
        points.iter().map(|p| p.y).max(),
    ) {
        (Some(x), Some(y)) => (x, y + GAP),
        _ => (0, 0),
    }
}

// ---- drawing a part with real wires ----

/// An axis-aligned wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seg {
    a: Point,
    b: Point,
}

impl Seg {
    fn horizontal(&self) -> bool {
        self.a.y == self.b.y
    }
    fn vertical(&self) -> bool {
        self.a.x == self.b.x
    }
    fn x_range(&self) -> (i32, i32) {
        (self.a.x.min(self.b.x), self.a.x.max(self.b.x))
    }
    fn y_range(&self) -> (i32, i32) {
        (self.a.y.min(self.b.y), self.a.y.max(self.b.y))
    }
    fn length(&self) -> i32 {
        (self.a.x - self.b.x).abs() + (self.a.y - self.b.y).abs()
    }
}

/// An open rectangle (its edges are not inside it).
#[derive(Debug, Clone, Copy)]
struct Area {
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
}

impl Area {
    fn crossed_by(&self, s: &Seg) -> bool {
        let (sx0, sx1) = s.x_range();
        let (sy0, sy1) = s.y_range();
        if s.horizontal() {
            self.y0 < sy0 && sy0 < self.y1 && sx1 > self.x0 && sx0 < self.x1
        } else if s.vertical() {
            self.x0 < sx0 && sx0 < self.x1 && sy1 > self.y0 && sy0 < self.y1
        } else {
            true
        }
    }

    fn overlaps(&self, o: &Area) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }
}

/// The space a drawn part takes, between and around its pins.
fn body_of(pins: &[Point], ground: bool) -> Area {
    let x0 = pins.iter().map(|p| p.x).min().unwrap_or(0);
    let x1 = pins.iter().map(|p| p.x).max().unwrap_or(0);
    let y0 = pins.iter().map(|p| p.y).min().unwrap_or(0);
    let y1 = pins.iter().map(|p| p.y).max().unwrap_or(0);
    if ground {
        return Area {
            x0: x0 - 1,
            x1: x0 + 1,
            y0,
            y1: y0 + 3,
        };
    }
    let pad = |lo: i32, hi: i32| if lo == hi { (lo - 1, hi + 1) } else { (lo, hi) };
    let (ax0, ax1) = pad(x0, x1);
    let (ay0, ay1) = pad(y0, y1);
    Area {
        x0: ax0,
        x1: ax1,
        y0: ay0,
        y1: ay1,
    }
}

fn collinear_overlap(a: &Seg, b: &Seg) -> bool {
    if a.horizontal() && b.horizontal() && a.a.y == b.a.y {
        let ((a0, a1), (b0, b1)) = (a.x_range(), b.x_range());
        return a0.max(b0) < a1.min(b1);
    }
    if a.vertical() && b.vertical() && a.a.x == b.a.x {
        let ((a0, a1), (b0, b1)) = (a.y_range(), b.y_range());
        return a0.max(b0) < a1.min(b1);
    }
    false
}

fn crosses(a: &Seg, b: &Seg) -> bool {
    let (h, v) = if a.horizontal() && b.vertical() {
        (a, b)
    } else if a.vertical() && b.horizontal() {
        (b, a)
    } else {
        return false;
    };
    let (hx0, hx1) = h.x_range();
    let (vy0, vy1) = v.y_range();
    hx0 < v.a.x && v.a.x < hx1 && vy0 < h.a.y && h.a.y < vy1
}

/// The ways to run a wire from pin `p` to target `t` with right angles.
fn routes(p: Point, t: Point) -> Vec<Vec<Seg>> {
    if p == t {
        return vec![Vec::new()];
    }
    if p.x == t.x || p.y == t.y {
        return vec![vec![Seg { a: p, b: t }]];
    }
    let across = Point::new(t.x, p.y);
    let up = Point::new(p.x, t.y);
    vec![
        vec![Seg { a: p, b: across }, Seg { a: across, b: t }],
        vec![Seg { a: p, b: up }, Seg { a: up, b: t }],
    ]
}

/// Points of the pins of existing parts on the net `old`.
fn pin_points(s: &Schematic, before: &Netlist, old: NodeId) -> Vec<Point> {
    let mut out = Vec::new();
    for e in &before.elements {
        for (i, &id) in e.nodes.iter().enumerate() {
            if id == old
                && let Some(c) = s.component(e.component)
                && let Some((_, at)) = c.pin_positions().nth(i)
                && !out.contains(&at)
            {
                out.push(at);
            }
        }
    }
    out
}

fn ground_points(s: &Schematic) -> Vec<Point> {
    s.components
        .iter()
        .filter(|c| matches!(c.kind, K::Ground))
        .map(|c| c.position)
        .collect()
}

/// Every place a new wire could join the net `node`: its pins, and the ends
/// of and points along the wires already on it. (Whether a point really is on
/// the net is settled by the check after the part is drawn.)
fn net_points(s: &Schematic, before: &Netlist, map: &NodeMap, node: NodeId) -> Vec<Point> {
    let mut out = if node.is_ground() {
        ground_points(s)
    } else {
        pin_points(s, before, node)
    };
    // `map` knows the wires the circuit had before any part was added; they
    // are the first ones in the list.
    for (w, n) in s.wires.iter().zip(&map.wires) {
        if *n != Some(node) || !(w.a.x == w.b.x || w.a.y == w.b.y) {
            continue;
        }
        let (x0, x1) = (w.a.x.min(w.b.x), w.a.x.max(w.b.x));
        let (y0, y1) = (w.a.y.min(w.b.y), w.a.y.max(w.b.y));
        for x in x0..=x1 {
            for y in y0..=y1 {
                let p = Point::new(x, y);
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// The `TARGETS` points of `mine` that are nearest to any of `other`.
fn nearest(mine: Vec<Point>, other: &[Point]) -> Vec<Point> {
    let gap = |p: &Point| {
        other
            .iter()
            .map(|q| (p.x - q.x).abs() + (p.y - q.y).abs())
            .min()
            .unwrap_or(0)
    };
    let mut mine = mine;
    mine.sort_by_key(gap);
    mine.truncate(TARGETS);
    mine
}

/// Most places a pin may be wired to, per net (those closest to the other
/// net), and most drawings checked electrically before giving up.
const TARGETS: usize = 4;
const TRIES: usize = 60;
/// How far from the targets a part may be put.
const REACH: i32 = 10;

/// Try to draw a two-pin part next to the nets it joins, with real wires.
/// Returns a note if it was drawn; `None` leaves the part for the labels.
fn try_wire(
    s: &mut Schematic,
    before: &Netlist,
    map: &NodeMap,
    nets: &HashMap<String, Net>,
    part: &NewPart,
) -> Option<String> {
    if part.kind.pins().len() != 2 || part.nets[0].eq_ignore_ascii_case(&part.nets[1]) {
        return None;
    }
    let mut candidates_of: Vec<Vec<Point>> = Vec::new();
    for net in &part.nets {
        let node = if is_ground(net) {
            NodeId::GROUND
        } else {
            match nets.get(net)? {
                Net::Existing { old, .. } => *old,
                Net::New => return None,
            }
        };
        let points = net_points(s, before, map, node);
        if points.is_empty() {
            return None;
        }
        candidates_of.push(points);
    }
    let targets: Vec<Vec<Point>> = vec![
        nearest(candidates_of[0].clone(), &candidates_of[1]),
        nearest(candidates_of[1].clone(), &candidates_of[0]),
    ];

    // What the new drawing must keep clear of.
    let boxes: Vec<Area> = s
        .components
        .iter()
        .map(|c| {
            let pins: Vec<Point> = c.pin_positions().map(|(_, at)| at).collect();
            body_of(
                &if pins.is_empty() {
                    vec![c.position]
                } else {
                    pins
                },
                matches!(c.kind, K::Ground),
            )
        })
        .collect();
    let wires: Vec<Seg> = s
        .wires
        .iter()
        .map(|w| Seg { a: w.a, b: w.b })
        .filter(|w| w.horizontal() || w.vertical())
        .collect();

    let all: Vec<Point> = targets.iter().flatten().copied().collect();
    let (min_x, max_x) = (
        all.iter().map(|p| p.x).min()? - REACH,
        all.iter().map(|p| p.x).max()? + REACH,
    );
    let (min_y, max_y) = (
        all.iter().map(|p| p.y).min()? - REACH,
        all.iter().map(|p| p.y).max()? + REACH,
    );
    let pins = part.kind.pins();

    struct Candidate {
        cost: i32,
        position: Point,
        rotation: Rotation,
        wires: Vec<Seg>,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    for rotation in [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270] {
        let o0 = rotation.apply(pins[0].offset);
        let o1 = rotation.apply(pins[1].offset);
        for px in min_x..=max_x {
            for py in min_y..=max_y {
                let p0 = Point::new(px + o0.x, py + o0.y);
                let p1 = Point::new(px + o1.x, py + o1.y);
                let body = body_of(&[p0, p1], false);
                if boxes.iter().any(|b| b.overlaps(&body))
                    || wires.iter().any(|w| body.crossed_by(w))
                {
                    continue;
                }
                let body_seg = Seg { a: p0, b: p1 };
                for t0 in &targets[0] {
                    for t1 in &targets[1] {
                        for r0 in routes(p0, *t0) {
                            for r1 in routes(p1, *t1) {
                                let new: Vec<Seg> = r0.iter().chain(&r1).copied().collect();
                                if let Some(cost) = judge(&new, &body_seg, &boxes, &wires) {
                                    candidates.push(Candidate {
                                        cost,
                                        position: Point::new(px, py),
                                        rotation,
                                        wires: new,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    candidates.sort_by_key(|c| c.cost);

    for cand in candidates.into_iter().take(TRIES) {
        let mut trial = s.clone();
        let id =
            trial.add_component_named(part.kind.clone(), part.designator.clone(), cand.position);
        if let Some(c) = trial.component_mut(id) {
            c.rotation = cand.rotation;
        }
        for w in &cand.wires {
            trial.add_wire(w.a, w.b);
        }
        library::ensure_model(&mut trial, &part.kind);
        if verify(&trial, before, std::slice::from_ref(part), nets).is_ok() {
            *s = trial;
            return Some(format!(
                "{} is drawn with wires next to the circuit.",
                part.designator
            ));
        }
    }
    None
}

/// The cost of a candidate set of wires, or `None` if it would run through
/// a part, lie along another wire, or cross itself.
fn judge(new: &[Seg], body: &Seg, boxes: &[Area], wires: &[Seg]) -> Option<i32> {
    let mut cost = 0;
    for w in new {
        cost += w.length() + 2;
        if boxes.iter().any(|b| b.crossed_by(w)) {
            return None;
        }
        if wires.iter().any(|e| collinear_overlap(w, e)) {
            return None;
        }
        cost += 6 * wires.iter().filter(|e| crosses(w, e)).count() as i32;
    }
    for (i, a) in new.iter().enumerate() {
        if collinear_overlap(a, body) || crosses(a, body) {
            return None;
        }
        for b in &new[i + 1..] {
            if collinear_overlap(a, b) || crosses(a, b) {
                return None;
            }
        }
    }
    Some(cost)
}

/// Check the result: each new pin is on the net asked for, and no nets that
/// were separate have been joined (or one split) by the new drawing.
fn verify(
    s: &Schematic,
    before: &Netlist,
    parts: &[NewPart],
    nets: &HashMap<String, Net>,
) -> Result<(), String> {
    let after = build_netlist(s);
    // Where each of the circuit's old nets is now.
    let mut forward: HashMap<NodeId, NodeId> = HashMap::new();
    let mut backward: HashMap<NodeId, NodeId> = HashMap::new();
    for old in &before.elements {
        let Some(new) = after.element(&old.designator) else {
            continue;
        };
        for (&a, &b) in old.nodes.iter().zip(&new.nodes) {
            if forward.insert(a, b).is_some_and(|prev| prev != b)
                || backward.insert(b, a).is_some_and(|prev| prev != a)
            {
                return Err(
                    "Placing the new parts would have joined or split nets of the \
                            existing circuit; nothing was changed."
                        .into(),
                );
            }
        }
    }
    for p in parts {
        let Some(e) = after.element(&p.designator) else {
            return Err(format!("{} did not appear in the circuit.", p.designator));
        };
        for (net, &node) in p.nets.iter().zip(&e.nodes) {
            let wanted = if is_ground(net) {
                Some(NodeId::GROUND)
            } else {
                match nets.get(net) {
                    Some(Net::Existing { old, .. }) => forward.get(old).copied(),
                    _ => after.node_by_name(net),
                }
            };
            if wanted != Some(node) {
                return Err(format!(
                    "{} would not end up on net {net}; nothing was changed.",
                    p.designator
                ));
            }
        }
    }
    Ok(())
}

/// Try adding `parts` to a copy of `s`; the notes if it would work.
pub fn check_add(s: &Schematic, parts: &[NewPart]) -> Result<Vec<String>, String> {
    add_parts(&mut s.clone(), parts)
}

/// What a part is, for the card.
pub fn describe(s: &Schematic, designator: &str) -> Option<String> {
    let c = find(s, designator)?;
    let what = match &c.kind {
        K::Resistor { resistance } => format!("resistor {}", format_value(*resistance, "Ω")),
        K::Capacitor { capacitance, .. } => {
            format!("capacitor {}", format_value(*capacitance, "F"))
        }
        K::Inductor { inductance, .. } => format!("inductor {}", format_value(*inductance, "H")),
        K::Diode { model } => format!("diode {model}"),
        K::Bjt { model, .. } => format!("transistor {model}"),
        K::Mosfet { model, .. } => format!("MOSFET {model}"),
        K::VoltageSource { .. } => "voltage source".to_string(),
        K::CurrentSource { .. } => "current source".to_string(),
        K::Switch { .. } => "switch".to_string(),
        K::Potentiometer { resistance, .. } => {
            format!("potentiometer {}", format_value(*resistance, "Ω"))
        }
        _ => "part".to_string(),
    };
    Some(format!("{} ({what})", c.designator))
}

fn find<'a>(s: &'a Schematic, designator: &str) -> Option<&'a dunbrec_core::Component> {
    s.components
        .iter()
        .find(|c| !c.designator.is_empty() && c.designator.eq_ignore_ascii_case(designator))
}

/// Remove the part `designator` and the stubs (a short wire ending in a
/// label or ground symbol that nothing else touches) that were drawn for it.
/// Wires drawn by hand are left, for the user to tidy.
pub fn remove_part(s: &mut Schematic, designator: &str) -> Result<String, String> {
    let c = find(s, designator).ok_or_else(|| format!("There is no part called {designator}."))?;
    if matches!(c.kind, K::Ground | K::Junction) {
        return Err(format!("{designator} cannot be removed this way."));
    }
    let (id, name) = (c.id, c.designator.clone());
    let pins: Vec<Point> = c.pin_positions().map(|(_, at)| at).collect();

    let mut doomed_wires = Vec::new();
    let mut doomed_labels = Vec::new();
    let mut doomed_grounds = Vec::new();
    for at in pins {
        for w in s.wires.iter().filter(|w| w.a == at || w.b == at) {
            let far = if w.a == at { w.b } else { w.a };
            // Anything else at the far end makes this more than a stub.
            let others = s
                .wires
                .iter()
                .filter(|o| o.id != w.id && (o.a == far || o.b == far))
                .count()
                + s.components
                    .iter()
                    .filter(|o| o.id != id && !matches!(o.kind, K::Ground))
                    .flat_map(|o| o.pin_positions().map(|(_, p)| p).collect::<Vec<_>>())
                    .filter(|p| *p == far)
                    .count();
            let labels: Vec<_> = s.labels.iter().filter(|l| l.position == far).collect();
            let grounds: Vec<_> = s
                .components
                .iter()
                .filter(|g| matches!(g.kind, K::Ground) && g.position == far)
                .collect();
            if others == 0 && (!labels.is_empty() || !grounds.is_empty()) {
                doomed_wires.push(w.id);
                doomed_labels.extend(labels.iter().map(|l| l.id));
                doomed_grounds.extend(grounds.iter().map(|g| g.id));
            }
        }
    }
    s.remove_component(id);
    for w in &doomed_wires {
        s.remove_wire(*w);
    }
    for l in &doomed_labels {
        s.remove_label(*l);
    }
    for g in &doomed_grounds {
        s.remove_component(*g);
    }
    Ok(name)
}

/// Try removing `names` from a copy of `s`.
pub fn check_remove(s: &Schematic, names: &[String]) -> Result<(), String> {
    let mut trial = s.clone();
    for n in names {
        remove_part(&mut trial, n)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 V source, two 1 k resistors; the middle net is called VOUT.
    fn divider() -> Schematic {
        crate::examples::voltage_divider()
    }

    fn node_of(s: &Schematic, designator: &str, pin: usize) -> String {
        let n = build_netlist(s);
        let e = n.element(designator).unwrap();
        n.nodes[e.nodes[pin].0].name.clone()
    }

    #[test]
    fn add_lines_are_read() {
        let p =
            parse_add("R5 resistor 10k VOUT 0\n# note\nQ2 npn 2N3904 c b 0\nD1 diode generic a k")
                .unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].designator, "R5");
        assert_eq!(p[0].nets, ["VOUT", "0"]);
        assert!(p[0].summary.contains("10 kΩ"), "{}", p[0].summary);
        assert!(matches!(&p[1].kind, K::Bjt { model, .. } if model == "2N3904"));
        assert!(matches!(&p[2].kind, K::Diode { model } if model == GENERIC_DIODE));
    }

    #[test]
    fn bad_add_lines_are_explained() {
        for (text, needle) in [
            ("R5 resistor 10k VOUT", "2 pins"),
            ("R5 resistor 10k A=VOUT B=0", "pin order"),
            ("R5 resistor banana A B", "R5"),
            ("R5 resistor -1 A B", "above zero"),
            ("R5 toaster 1 A B", "not a part"),
            ("5R resistor 1k A B", "designator"),
            ("D1 diode NOSUCH a k", "no model"),
            ("D1 diode 2N3904 a k", "right kind"),
            ("S1 switch maybe a b", "open or closed"),
            ("# only a comment", "no parts"),
            ("R5 resistor 1k", "should read"),
        ] {
            let e = parse_add(text).unwrap_err();
            assert!(e.contains(needle), "{text}: {e}");
        }
    }

    #[test]
    fn every_listed_kind_builds_and_the_help_names_its_pins() {
        let mut words: Vec<&str> = KINDS.iter().map(|k| k.0).collect();
        for (word, example, _) in KINDS {
            assert!(build_kind(word, example).is_ok(), "{word} {example}");
        }
        words.sort();
        words.dedup();
        assert_eq!(words.len(), KINDS.len(), "each word once");
        let help = help();
        assert_eq!(help.lines().count(), KINDS.len());
        assert!(
            help.contains("- opamp: value is the part (uA741); pins in order: in+ in- V+ V- out")
        );
        assert!(help.contains("- npn:") && help.contains("C B E"), "{help}");
        assert!(
            help.contains("- dff:") && help.contains("D CLK Q QN"),
            "{help}"
        );
    }

    #[test]
    fn logic_and_ic_lines_are_read() {
        let p = parse_add(
            "U1 opamp 741 INP INN VCC VEE OUT\nU2 timer555 default 0 TR OUT RS CT TH DIS VCC\n\
             G1 nand 10n A B Y\nFF1 dff default D CLK Q QN\nCK1 clock 1k CLK\nSW9 logicswitch high D",
        )
        .unwrap();
        assert_eq!(p.len(), 6);
        assert!(matches!(&p[0].kind, K::OpAmp { part } if part == "uA741"));
        assert!(
            matches!(p[2].kind, K::Gate { op: GateOp::Nand, delay } if (delay - 10e-9).abs() < 1e-15)
        );
        assert_eq!(p[3].nets.len(), 4);
        assert!(parse_add("U1 opamp 9999 A B C D E").is_err());
        assert!(parse_add("G1 nand -1 A B Y").is_err());
        assert!(parse_add("S1 logicswitch maybe A").is_err());
    }

    #[test]
    fn an_op_amp_is_added_on_labels_and_joins_its_nets() {
        let mut s = divider();
        let parts = parse_add("U1 opamp uA741 VOUT FB VCC VEE OUTX").unwrap();
        add_parts(&mut s, &parts).unwrap();
        assert_eq!(node_of(&s, "U1", 0), "VOUT");
        assert_eq!(node_of(&s, "U1", 4), "OUTX");
        assert_eq!(node_of(&s, "R1", 1), "VOUT");
    }

    /// Draw the schematic as text: wires `-` `|`, pins and parts by the first
    /// letter of their designator, ground `G`, labels in lower case.
    fn ascii(s: &Schematic) -> String {
        let mut cells: std::collections::BTreeMap<(i32, i32), char> = Default::default();
        for w in &s.wires {
            let (dx, dy) = ((w.b.x - w.a.x).signum(), (w.b.y - w.a.y).signum());
            let mut p = w.a;
            loop {
                let ch = if dy == 0 { '-' } else { '|' };
                let e = cells.entry((p.x, p.y)).or_insert(ch);
                if *e != ch && *e != '+' {
                    *e = '+';
                }
                if p == w.b {
                    break;
                }
                p = Point::new(p.x + dx, p.y + dy);
            }
        }
        for c in &s.components {
            let ch = if matches!(c.kind, K::Ground) {
                'G'
            } else {
                c.designator.chars().next().unwrap_or('?')
            };
            for (_, at) in c.pin_positions() {
                cells.insert((at.x, at.y), 'o');
            }
            cells.insert((c.position.x, c.position.y), ch);
        }
        for l in &s.labels {
            for (i, ch) in l.name.chars().enumerate() {
                cells.insert(
                    (l.position.x + 1 + i as i32, l.position.y),
                    ch.to_ascii_lowercase(),
                );
            }
        }
        let (x1, y1) = (
            cells.keys().map(|k| k.0).max().unwrap_or(0),
            cells.keys().map(|k| k.1).max().unwrap_or(0),
        );
        let (x0, y0) = (
            cells.keys().map(|k| k.0).min().unwrap_or(0),
            cells.keys().map(|k| k.1).min().unwrap_or(0),
        );
        let mut out = String::new();
        for y in y0..=y1 {
            for x in x0..=x1 {
                out.push(*cells.get(&(x, y)).unwrap_or(&' '));
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn a_new_amplifier_is_drawn_with_wires_and_the_right_nets() {
        let mut s = Schematic::new();
        let parts = parse_add(
            "V1 vsource 9 VCC 0
R1 resistor 220k VCC BASE
R2 resistor 22k BASE 0
             Rc resistor 2.2k VCC COL
Re resistor 470 EMIT 0
C1 capacitor 10u INPUT BASE
             C2 capacitor 100u EMIT 0
Q1 npn 2N3904 COL BASE EMIT",
        )
        .unwrap();
        let notes = add_parts(&mut s, &parts).unwrap();
        println!(
            "{}
{notes:?}",
            ascii(&s)
        );
        assert!(notes.iter().any(|n| n.contains("with wires")), "{notes:?}");
        for (d, pin, net) in [
            ("V1", 0, "VCC"),
            ("R1", 1, "BASE"),
            ("R2", 0, "BASE"),
            ("Rc", 1, "COL"),
            ("Re", 0, "EMIT"),
            ("C1", 0, "INPUT"),
            ("C2", 0, "EMIT"),
            ("Q1", 0, "COL"),
            ("Q1", 1, "BASE"),
            ("Q1", 2, "EMIT"),
        ] {
            assert_eq!(node_of(&s, d, pin), net, "{d} pin {pin}");
        }
        assert_eq!(node_of(&s, "Re", 1), "0");
    }

    #[test]
    fn other_new_circuits_are_drawn_and_stay_correct() {
        for (name, text, checks) in [
            (
                "pnp",
                "V1 vsource 9 VCC 0
Re resistor 1k VCC E
Q1 pnp 2N3906 C B E
                 Rb resistor 100k B 0
Rc resistor 2.2k C 0",
                vec![("Q1", 2, "E"), ("Q1", 0, "C"), ("Rb", 0, "B")],
            ),
            (
                "opamp",
                "V1 vsource 12 VCC 0
V2 vsource 5 IN 0
U1 opamp uA741 0 INV VCC 0 OUT
                 R1 resistor 10k IN INV
R2 resistor 100k INV OUT",
                vec![("U1", 1, "INV"), ("R2", 1, "OUT")],
            ),
            (
                "555",
                "V1 vsource 9 VCC 0
U1 timer555 default 0 TRIG OUT VCC CTL THR DIS VCC
                 R1 resistor 1k VCC DIS
R2 resistor 10k DIS THR
C1 capacitor 10u THR 0
                 D1 diode generic OUT LED",
                vec![("R2", 1, "THR"), ("C1", 0, "THR")],
            ),
        ] {
            let mut s = Schematic::new();
            let parts = parse_add(text).unwrap();
            let notes = add_parts(&mut s, &parts).unwrap_or_else(|e| panic!("{name}: {e}"));
            println!(
                "== {name}: {notes:?}
{}",
                ascii(&s)
            );
            for (d, pin, net) in checks {
                assert_eq!(node_of(&s, d, pin), net, "{name}: {d} pin {pin}");
            }
        }
    }

    #[test]
    fn a_sine_source_is_added_to_an_existing_input() {
        let mut s = Schematic::new();
        let amp = parse_add(
            "V1 vsource 9 VCC 0
R1 resistor 220k VCC BASE
R2 resistor 22k BASE 0
             Rc resistor 2.2k VCC COL
Re resistor 470 EMIT 0
C1 capacitor 10u INPUT BASE
             Q1 npn 2N3904 COL BASE EMIT",
        )
        .unwrap();
        add_parts(&mut s, &amp).unwrap();
        let src = parse_add("Vin ac 10m@1k INPUT 0").unwrap();
        assert!(matches!(
            src[0].kind,
            K::VoltageSource { waveform: Waveform::Sine { amplitude, frequency, .. } }
                if (amplitude - 0.01).abs() < 1e-12 && (frequency - 1000.0).abs() < 1e-9
        ));
        let notes = add_parts(&mut s, &src).unwrap();
        println!(
            "{}
{notes:?}",
            ascii(&s)
        );
        assert_eq!(node_of(&s, "Vin", 0), "INPUT");
        assert_eq!(node_of(&s, "Vin", 1), "0");
        assert!(parse_add("Vin vsine 5 INPUT 0").is_err());
    }

    #[test]
    fn remove_lines_are_read() {
        assert_eq!(parse_remove("R1\n\n# x\nC2").unwrap(), ["R1", "C2"]);
        assert!(parse_remove("R1 R2").is_err());
        assert!(parse_remove("").is_err());
    }

    #[test]
    fn a_new_part_joins_a_named_net_and_ground() {
        let mut s = divider();
        let parts = parse_add("R9 resistor 4k7 VOUT 0").unwrap();
        let notes = add_parts(&mut s, &parts).unwrap();
        assert!(
            notes[0].contains("R9") && notes[0].contains("wires"),
            "{notes:?}"
        );
        assert_eq!(node_of(&s, "R9", 0), "VOUT");
        assert_eq!(node_of(&s, "R9", 1), "0");
        // The original circuit is unchanged electrically.
        assert_eq!(node_of(&s, "R1", 1), "VOUT");
        assert_eq!(node_of(&s, "R2", 0), "VOUT");
        assert!(build_netlist(&s).diagnostics.is_empty());
    }

    #[test]
    fn new_parts_go_below_the_drawing_without_overlap() {
        let mut s = divider();
        let lowest = s.components.iter().map(|c| c.position.y).max().unwrap();
        let existing = s.components.len();
        add_parts(
            &mut s,
            &parse_add("R9 resistor 1k A B\nR8 resistor 1k B C").unwrap(),
        )
        .unwrap();
        for c in &s.components[existing..] {
            if !matches!(c.kind, K::Ground) {
                assert!(c.position.y > lowest, "{:?}", c.position);
            }
        }
    }

    #[test]
    fn an_unnamed_net_is_named_so_new_parts_can_join_it() {
        let mut s = divider();
        s.labels.clear();
        let n = build_netlist(&s);
        let mid = n.element("R1").unwrap().nodes[1];
        let mid_name = n.nodes[mid.0].name.clone();
        assert!(mid_name.starts_with('N'), "{mid_name}");
        // A transistor has three pins, so it goes below the drawing on labels.
        let parts = parse_add(&format!("Q9 npn generic {mid_name} B 0")).unwrap();
        let notes = add_parts(&mut s, &parts).unwrap();
        assert!(notes.iter().any(|n| n.contains("Named a net")), "{notes:?}");
        let after = build_netlist(&s);
        assert_eq!(
            after.element("Q9").unwrap().nodes[0],
            after.element("R1").unwrap().nodes[1]
        );
        assert_eq!(
            after.element("R2").unwrap().nodes[0],
            after.element("R1").unwrap().nodes[1]
        );
    }

    #[test]
    fn a_part_between_existing_nets_is_drawn_with_wires() {
        let mut s = divider();
        let (grounds, labels, wires) = (
            s.components
                .iter()
                .filter(|c| matches!(c.kind, K::Ground))
                .count(),
            s.labels.len(),
            s.wires.len(),
        );
        let before = build_netlist(&s);
        let notes = add_parts(&mut s, &parse_add("C9 capacitor 100n VOUT 0").unwrap()).unwrap();
        assert!(notes[0].contains("wires"), "{notes:?}");
        // No second ground symbol and no labels: it is wired to the existing ones.
        assert_eq!(
            s.components
                .iter()
                .filter(|c| matches!(c.kind, K::Ground))
                .count(),
            grounds
        );
        assert_eq!(s.labels.len(), labels);
        assert!(s.wires.len() > wires);
        assert_eq!(node_of(&s, "C9", 0), "VOUT");
        assert_eq!(node_of(&s, "C9", 1), "0");
        // The circuit it was added to is electrically what it was.
        let after = build_netlist(&s);
        for e in &before.elements {
            let new = after.element(&e.designator).unwrap();
            for (a, b) in e.nodes.iter().zip(&new.nodes) {
                assert_eq!(before.nodes[a.0].name, after.nodes[b.0].name);
            }
        }
        assert!(after.diagnostics.is_empty(), "{:?}", after.diagnostics);
    }

    #[test]
    fn a_part_on_an_unnamed_net_is_wired_straight_to_a_pin() {
        let mut s = divider();
        s.labels.clear();
        let n = build_netlist(&s);
        let mid = n.nodes[n.element("R1").unwrap().nodes[1].0].name.clone();
        let notes = add_parts(
            &mut s,
            &parse_add(&format!("R9 resistor 1k {mid} 0")).unwrap(),
        )
        .unwrap();
        assert!(notes[0].contains("wires"), "{notes:?}");
        assert!(s.labels.is_empty(), "no label was needed");
        let after = build_netlist(&s);
        assert_eq!(
            after.element("R9").unwrap().nodes[0],
            after.element("R1").unwrap().nodes[1]
        );
    }

    #[test]
    fn several_wired_parts_do_not_disturb_each_other() {
        let mut s = divider();
        let parts =
            parse_add("C1 capacitor 100n VOUT 0\nR8 resistor 10k VOUT 0\nR7 resistor 1k VOUT 0")
                .unwrap();
        add_parts(&mut s, &parts).unwrap();
        for d in ["C1", "R8", "R7"] {
            assert_eq!(node_of(&s, d, 0), "VOUT", "{d}");
            assert_eq!(node_of(&s, d, 1), "0", "{d}");
        }
        assert_eq!(node_of(&s, "R1", 1), "VOUT");
    }

    #[test]
    fn a_brand_new_net_joins_only_the_new_parts() {
        let mut s = divider();
        let parts = parse_add("R7 resistor 1k MID 0\nC7 capacitor 1u MID 0").unwrap();
        add_parts(&mut s, &parts).unwrap();
        assert_eq!(node_of(&s, "R7", 0), "MID");
        assert_eq!(node_of(&s, "C7", 0), "MID");
    }

    #[test]
    fn a_taken_designator_is_refused() {
        let mut s = divider();
        let e = add_parts(&mut s, &parse_add("R1 resistor 1k A B").unwrap()).unwrap_err();
        assert!(e.contains("already"), "{e}");
        let dup = parse_add("R9 resistor 1k A B\nr9 resistor 1k A B").unwrap();
        assert!(check_add(&divider(), &dup).is_err());
    }

    #[test]
    fn semiconductors_bring_their_models() {
        let mut s = divider();
        let parts = parse_add("Q5 npn 2N3904 VOUT B 0\nD5 diode generic B 0").unwrap();
        add_parts(&mut s, &parts).unwrap();
        assert!(s.models.get("2N3904").is_some());
        assert!(s.models.get(GENERIC_DIODE).is_some());
        assert!(
            build_netlist(&s)
                .diagnostics
                .iter()
                .all(|d| !matches!(d, dunbrec_core::Diagnostic::MissingModel { .. }))
        );
    }

    #[test]
    fn removing_a_part_takes_its_stubs_with_it() {
        let mut s = divider();
        let (c, w, l) = (s.components.len(), s.wires.len(), s.labels.len());
        // New nets, so it goes below the drawing on stubs and labels.
        add_parts(&mut s, &parse_add("R9 resistor 4k7 MID 0").unwrap()).unwrap();
        assert!(s.components.len() > c);
        remove_part(&mut s, "r9").unwrap();
        assert_eq!(
            (s.components.len(), s.wires.len(), s.labels.len()),
            (c, w, l)
        );
    }

    #[test]
    fn removing_a_hand_wired_part_keeps_the_wires_and_unknowns_fail() {
        let mut s = divider();
        let wires = s.wires.len();
        remove_part(&mut s, "R1").unwrap();
        assert!(s.components.iter().all(|c| c.designator != "R1"));
        assert_eq!(s.wires.len(), wires, "hand-drawn wires stay");
        assert!(remove_part(&mut s, "R99").is_err());
        assert!(check_remove(&divider(), &["R1".into(), "R99".into()]).is_err());
    }

    #[test]
    fn describe_names_the_part() {
        let s = divider();
        assert!(describe(&s, "r1").unwrap().contains("1 kΩ"));
        assert_eq!(describe(&s, "R99"), None);
    }
}
