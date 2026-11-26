//! Adding and removing parts on the assistant's suggestion.
//!
//! The assistant lists parts with the net each pin joins, in pin order. A
//! part with many pins may instead name just the pins it uses, and a pin of
//! a part already there may stand for its net (`U1.D13`):
//!
//! ````text
//! ```dunbrec-add
//! R5 resistor 10k VOUT 0
//! Q2 npn 2N3904 C1 B1 0
//! R6 resistor 220 U1.D13 LEDA
//! DS1 7seg-cathode red a=SA b=SB COM=0
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
//! check below is used. Parts in series that lead away from a net the circuit
//! has (a resistor and an LED from a pin to ground, say) are drawn in a row
//! from a pin on it, again with wires. Any other part is laid out in empty
//! space below the drawing. Like an imported netlist, each of its pins gets a short stub
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
    CD4066_DEFAULT_R_ON, CommonPin, ComponentId, ComponentKind as K, DEFAULT_LOGIC_DELAY,
    DEFAULT_MCU_CLOCK, GateOp, LedColour, ModelKind, ModelLibrary, Netlist, NodeId, OPAMP_PARTS,
    Point, Polarity, Rotation, Schematic, SwitchForm, Waveform, build_netlist,
};
use dunbrec_io::layout::{
    self, MANY_PINS, Part, STUB, UNJOINED, ground_rotation, is_ground, pin_direction,
};
use dunbrec_io::tidy;

use crate::canvas::NodeMap;
use crate::library::{
    self, GENERIC_DIODE, GENERIC_NMOS, GENERIC_NPN, GENERIC_PMOS, GENERIC_PNP, GENERIC_ZENER,
    SWITCH_R_OFF, SWITCH_R_ON,
};

/// Fence tag of a block that adds parts.
pub const ADD_TAG: &str = "dunbrec-add";
/// Fence tag of a block that removes parts.
pub const REMOVE_TAG: &str = "dunbrec-remove";

/// Rows between the bottom of the existing drawing and the new parts.
const GAP: i32 = 12;
/// Rows left clear between it and the top of new parts that reach far up.
const CLEARANCE: i32 = 4;

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
        let pins = kind.pins();
        let named = pins.len() > MANY_PINS && words[3..].iter().any(|n| n.contains('='));
        let nets = if named {
            named_nets(designator, &kind, &words[3..])?
        } else {
            words[3..].iter().map(|n| n.to_string()).collect()
        };
        if let Some(bad) = nets.iter().find(|n| n.contains('=')) {
            return Err(format!(
                "{designator}: write the nets in pin order, without “{bad}”."
            ));
        }
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

/// The nets of a part's pins from `PIN=NET` words: pins not named are left
/// unjoined.
fn named_nets(designator: &str, kind: &K, words: &[&str]) -> Result<Vec<String>, String> {
    let pins = kind.pins();
    let mut nets = vec![UNJOINED.to_string(); pins.len()];
    for word in words {
        let Some((name, net)) = word.split_once('=').filter(|(_, net)| !net.is_empty()) else {
            return Err(format!(
                "{designator}: name every pin used as PIN=NET, or none of them (“{word}”)."
            ));
        };
        let at = pins
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                let names: Vec<&str> = pins.iter().map(|p| p.name).collect();
                format!(
                    "{designator} has no pin called {name}; its pins are {}.",
                    names.join(" ")
                )
            })?;
        if nets[at] != UNJOINED {
            return Err(format!("{designator}: pin {name} is given twice."));
        }
        nets[at] = net.to_string();
    }
    Ok(nets)
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
    ("vpulse", "0:5@1k", "pulse source: low:high@hertz"),
    ("vcvs", "10", "voltage gain"),
    ("vccs", "1m", "transconductance in siemens"),
    ("ccvs", "1k", "transresistance in ohms"),
    ("cccs", "10", "current gain"),
    ("spdt", "rest", "rest or thrown"),
    ("dpst", "rest", "rest or thrown"),
    ("dpdt", "rest", "rest or thrown"),
    (
        "button",
        "open",
        "how the push button is when not pressed: open or closed",
    ),
    (
        "diode",
        "generic",
        "generic, zener (5.1 V), or a model such as 1N4148",
    ),
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
    ("cd4066", "default", "the word default"),
    ("led", "red", COLOURS),
    ("ledbar", "red", COLOURS),
    ("7seg-cathode", "red", COLOURS),
    ("7seg-anode", "red", COLOURS),
    ("4digit-cathode", "red", COLOURS),
    ("4digit-anode", "red", COLOURS),
    (
        "atmega328p",
        "none",
        "none, or the name of a program file the circuit already has",
    ),
];

/// What the value of a LED or display is.
const COLOURS: &str = "the colour: red, yellow, green, blue or white";

/// The kinds, values and pin orders, one per line, for the assistant.
pub fn help() -> String {
    KINDS
        .iter()
        .filter_map(|(word, example, meaning)| {
            let (kind, _) = build_kind(word, example).ok()?;
            let pins: Vec<&str> = kind.pins().iter().map(|p| p.name).collect();
            let how = if pins.len() > MANY_PINS {
                "name the pins used, as PIN=NET, from"
            } else {
                "pins in order:"
            };
            Some(format!(
                "- {word}: value is {meaning}; {how} {}",
                pins.join(" ")
            ))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_kind(word: &str, value: &str) -> Result<(K, String), String> {
    // "zener default" means the same as "diode zener".
    let value = if word == "zener" && value.eq_ignore_ascii_case("default") {
        "zener"
    } else {
        value
    };
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
        "pulse" | "square" => "vpulse",
        "pushbutton" => "button",
        "zener" => "diode",
        "arduino" | "mcu" | "microcontroller" => "atmega328p",
        "7seg_cathode" | "7segment-cathode" | "7seg-cc" => "7seg-cathode",
        "7seg_anode" | "7segment-anode" | "7seg-ca" => "7seg-anode",
        "4digit_cathode" | "4digit-cc" => "4digit-cathode",
        "4digit_anode" | "4digit-ca" => "4digit-anode",
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
            let value = if value.eq_ignore_ascii_case("zener") {
                GENERIC_ZENER
            } else {
                value
            };
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
        "vpulse" => pulse_source(value)?,
        "vcvs" => {
            let gain = number()?;
            (
                K::Vcvs { gain },
                format!("voltage-controlled voltage source ×{gain}"),
            )
        }
        "vccs" => {
            let g = number()?;
            (
                K::Vccs {
                    transconductance: g,
                },
                format!("voltage-controlled current source {}", format_value(g, "S")),
            )
        }
        "ccvs" => {
            let r = number()?;
            (
                K::Ccvs { transresistance: r },
                format!("current-controlled voltage source {}", format_value(r, "Ω")),
            )
        }
        "cccs" => {
            let gain = number()?;
            (
                K::Cccs { gain },
                format!("current-controlled current source ×{gain}"),
            )
        }
        "spdt" | "dpst" | "dpdt" => {
            let form = match word {
                "spdt" => SwitchForm::Spdt,
                "dpst" => SwitchForm::Dpst,
                _ => SwitchForm::Dpdt,
            };
            let thrown = match value.to_ascii_lowercase().as_str() {
                "rest" => false,
                "thrown" => true,
                _ => return Err("the switch is either rest or thrown.".into()),
            };
            (
                K::MultiSwitch {
                    form,
                    thrown,
                    r_on: SWITCH_R_ON,
                    r_off: SWITCH_R_OFF,
                },
                format!("{} switch", form.name()),
            )
        }
        "button" => {
            let closed = match value.to_ascii_lowercase().as_str() {
                "closed" => true,
                "open" => false,
                _ => return Err("a push button rests either open or closed.".into()),
            };
            (
                K::Switch {
                    closed,
                    r_on: SWITCH_R_ON,
                    r_off: SWITCH_R_OFF,
                    momentary: true,
                },
                format!(
                    "push button, normally {}",
                    if closed { "closed" } else { "open" }
                ),
            )
        }
        "cd4066" => (
            K::Cd4066 {
                r_on: CD4066_DEFAULT_R_ON,
            },
            "CD4066 analog switch".to_string(),
        ),
        "led" => {
            let colour = led_colour(value)?;
            (K::Led { colour }, format!("{} LED", colour.name()))
        }
        "ledbar" => {
            let colour = led_colour(value)?;
            (
                K::LedBar { colour },
                format!("{} 10-segment LED bar", colour.name()),
            )
        }
        "7seg-cathode" | "7seg-anode" | "4digit-cathode" | "4digit-anode" => {
            let colour = led_colour(value)?;
            let common = if word.ends_with("cathode") {
                CommonPin::Cathode
            } else {
                CommonPin::Anode
            };
            let (kind, what) = if word.starts_with('7') {
                (K::SevenSegment { colour, common }, "7-segment display")
            } else {
                (K::FourDigit { colour, common }, "4-digit display")
            };
            (kind, format!("{} {what}, {}", colour.name(), common.name()))
        }
        "atmega328p" => {
            let firmware = if value.eq_ignore_ascii_case("none") {
                String::new()
            } else {
                value.to_string()
            };
            let program = if firmware.is_empty() {
                "no program yet"
            } else {
                value
            };
            let summary = format!("ATmega328P microcontroller, {program}");
            (
                K::Mcu {
                    firmware,
                    clock: DEFAULT_MCU_CLOCK,
                },
                summary,
            )
        }
        other => {
            let words: Vec<&str> = KINDS.iter().map(|k| k.0).collect();
            return Err(format!(
                "“{other}” is not a part I can add. Use: {}.",
                words.join(", ")
            ));
        }
    })
}

/// The colour of a LED: its name, or the word `default` for red.
fn led_colour(value: &str) -> Result<LedColour, String> {
    if value.eq_ignore_ascii_case("default") {
        return Ok(LedColour::Red);
    }
    LedColour::ALL
        .into_iter()
        .find(|c| c.name().eq_ignore_ascii_case(value))
        .ok_or_else(|| "the colour is red, yellow, green, blue or white.".into())
}

/// A square-wave voltage source from `low:high@frequency`.
fn pulse_source(value: &str) -> Result<(K, String), String> {
    let form = "write the value as low:high@frequency, such as 0:5@1k.";
    let (levels, frequency) = value.split_once('@').ok_or(form)?;
    let (low, high) = levels.split_once(':').ok_or(form)?;
    let number = |text: &str| parse_eng(text).map_err(|e| e.to_string());
    let (low, high, frequency) = (number(low)?, number(high)?, number(frequency)?);
    if !(low.is_finite() && high.is_finite() && frequency > 0.0 && frequency.is_finite()) {
        return Err("the levels must be numbers and the frequency above zero.".into());
    }
    let period = 1.0 / frequency;
    // Edges of a microsecond, shorter for a very fast one.
    let edge = (period / 100.0).min(1e-6);
    Ok((
        K::VoltageSource {
            waveform: Waveform::Pulse {
                v1: low,
                v2: high,
                delay: 0.0,
                rise: edge,
                fall: edge,
                width: period / 2.0 - edge,
                period,
            },
        },
        format!(
            "pulse source {} to {} at {}",
            format_value(low, "V"),
            format_value(high, "V"),
            format_value(frequency, "Hz")
        ),
    ))
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

    for p in parts {
        if let K::Mcu { firmware, .. } = &p.kind
            && !firmware.is_empty()
            && !s.sources.contains_key(firmware)
        {
            let have: Vec<&str> = s.sources.keys().map(String::as_str).collect();
            let have = if have.is_empty() {
                "none".to_string()
            } else {
                format!("none, or one of {}", have.join(", "))
            };
            return Err(format!(
                "{}: there is no program file called {firmware}; use {have}.",
                p.designator
            ));
        }
    }

    let before = build_netlist(s);
    // A pin of a part already there stands for the net it is on, and a pin
    // of another new part for the net that one is given.
    let mut parts = parts.to_vec();
    join_new_pins(&mut parts);
    for net in parts.iter_mut().flat_map(|p| &mut p.nets) {
        if let Some(name) = net_of_pin(&before, net) {
            *net = name;
        }
    }
    let parts = &parts[..];
    let mut nets: HashMap<String, Net> = HashMap::new();
    for net in parts.iter().flat_map(|p| &p.nets) {
        if !is_ground(net) && net != UNJOINED {
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
    for chain in chains(&nets, parts, &leftover) {
        if let Some(note) = draw_chain(s, &before, &map, &nets, &chain) {
            notes.push(note);
            leftover.retain(|p| chain.links.iter().all(|l| l.part != *p));
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

/// The part and pin that `net` refers to, if it reads `DESIGNATOR.PIN` for
/// one of the new parts.
fn new_pin(parts: &[NewPart], net: &str) -> Option<(usize, usize)> {
    let (designator, pin) = net.split_once('.')?;
    let part = parts
        .iter()
        .position(|p| p.designator.eq_ignore_ascii_case(designator))?;
    let pins = parts[part].kind.pins();
    let pin = pins.iter().position(|p| p.name.eq_ignore_ascii_case(pin))?;
    Some((part, pin))
}

/// Where a new part's net names a pin of another new part, put both on one
/// net: the one that pin was given, or a new one if it was given none.
fn join_new_pins(parts: &mut [NewPart]) {
    for i in 0..parts.len() {
        for j in 0..parts[i].nets.len() {
            let Some((part, pin)) = new_pin(parts, &parts[i].nets[j]) else {
                continue;
            };
            let given = &parts[part].nets[pin];
            if given == UNJOINED || new_pin(parts, given).is_some() {
                let p = &parts[part];
                parts[part].nets[pin] = format!("{}_{}", p.designator, p.kind.pins()[pin].name);
            }
            parts[i].nets[j] = parts[part].nets[pin].clone();
        }
    }
}

/// The name of the net that pin `PIN` of the part `DESIGNATOR` is on, if
/// `net` reads `DESIGNATOR.PIN` and is not itself the name of a net.
fn net_of_pin(before: &Netlist, net: &str) -> Option<String> {
    if before
        .nodes
        .iter()
        .any(|n| n.name.eq_ignore_ascii_case(net))
    {
        return None;
    }
    let (designator, pin) = net.split_once('.')?;
    let element = before
        .elements
        .iter()
        .find(|e| e.designator.eq_ignore_ascii_case(designator))?;
    let at = element
        .kind
        .pins()
        .iter()
        .position(|p| p.name.eq_ignore_ascii_case(pin))?;
    Some(before.nodes[element.nodes[at].0].name.clone())
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
        if is_ground(net) || net == UNJOINED || label_of.contains_key(net) {
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
        let Some((def, at)) = c.pin_positions().nth(a.pin) else {
            continue;
        };
        let away = pin_direction(c, def.offset);
        let stub = Seg {
            a: at,
            b: step(at, away, STUB),
        };
        // A stub that would run into something would join it: then the name
        // goes on the pin itself.
        let blocked = points_of(s).any(|p| p != at && on(&stub, p))
            || s.wires.iter().any(|w| on(&Seg { a: w.a, b: w.b }, stub.b));
        let end = if blocked { at } else { stub.b };
        if end != at {
            s.add_wire(at, end);
        }
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
    let (dx, mut dy) = free_space_below(s);
    // A large part reaches well above and beside where it is placed.
    if let (Some(floor), Some(ceiling)) = (
        points_of(s).map(|p| p.y).max(),
        points_of(placed).map(|p| p.y).min(),
    ) {
        dy = dy.max(floor + CLEARANCE - ceiling);
    }
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

/// Every point a drawing reaches: its pins, wire ends and labels.
fn points_of(s: &Schematic) -> impl Iterator<Item = Point> + '_ {
    let pins = s
        .components
        .iter()
        .flat_map(|c| c.pin_positions().map(|(_, at)| at).collect::<Vec<_>>());
    pins.chain(s.wires.iter().flat_map(|w| [w.a, w.b]))
        .chain(s.labels.iter().map(|l| l.position))
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
    if part.kind.pins().len() != 2
        || part.nets[0].eq_ignore_ascii_case(&part.nets[1])
        || part.nets.iter().any(|n| n == UNJOINED)
    {
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

// ---- drawing parts in series ----

/// One part of a chain, and the pin of it nearer the start.
struct Link<'a> {
    part: &'a NewPart,
    entry: usize,
}

/// Where a chain stops.
enum End {
    Ground,
    /// At a net the circuit already has.
    Existing(NodeId),
    /// At a net other new parts join, or nothing yet: it is labelled.
    Open(String),
}

/// Two-pin parts in series, leading away from a net the circuit has.
struct Chain<'a> {
    start: NodeId,
    links: Vec<Link<'a>>,
    end: End,
}

/// The chains among the `left` parts. A net joins two links when nothing but
/// those two of all the new `parts` is on it.
fn chains<'a>(
    nets: &HashMap<String, Net>,
    parts: &'a [NewPart],
    left: &[&'a NewPart],
) -> Vec<Chain<'a>> {
    let fits = |p: &NewPart| {
        p.kind.pins().len() == 2 && p.nets[0] != p.nets[1] && p.nets.iter().all(|n| n != UNJOINED)
    };
    let mut used: Vec<&str> = Vec::new();
    let mut found = Vec::new();
    for &first in left {
        if !fits(first) || used.contains(&first.designator.as_str()) {
            continue;
        }
        let from = first
            .nets
            .iter()
            .enumerate()
            .find_map(|(pin, net)| match nets.get(net) {
                Some(Net::Existing { old, .. }) if !old.is_ground() => Some((pin, *old)),
                _ => None,
            });
        let Some((entry, start)) = from else { continue };
        used.push(&first.designator);
        let mut links = vec![Link { part: first, entry }];
        let end = loop {
            let last = &links[links.len() - 1];
            let net = &last.part.nets[1 - last.entry];
            if is_ground(net) {
                break End::Ground;
            }
            if let Some(Net::Existing { old, .. }) = nets.get(net) {
                break End::Existing(*old);
            }
            let on: Vec<&NewPart> = parts.iter().filter(|p| p.nets.contains(net)).collect();
            let next = left.iter().copied().find(|p| {
                fits(p) && !used.contains(&p.designator.as_str()) && p.nets.contains(net)
            });
            match next {
                Some(next) if on.len() == 2 => {
                    used.push(&next.designator);
                    let entry = usize::from(next.nets[0] != *net);
                    links.push(Link { part: next, entry });
                }
                _ => break End::Open(net.clone()),
            }
        };
        found.push(Chain { start, links, end });
    }
    found
}

/// A chain laid out in one particular way.
struct Run {
    cost: i32,
    parts: Vec<(Point, Rotation)>,
    wires: Vec<Seg>,
    ground: Option<(Point, Rotation)>,
    label: Option<Point>,
}

/// The space the text of a label takes.
fn label_area(at: Point, name: &str) -> Area {
    Area {
        x0: at.x,
        x1: at.x + 1 + name.len() as i32 / 2,
        y0: at.y - 2,
        y1: at.y,
    }
}

fn step(from: Point, along: Point, far: i32) -> Point {
    Point::new(from.x + along.x * far, from.y + along.y * far)
}

/// The room a ground symbol takes, hanging whichever way it is turned.
fn ground_area(at: Point, rotation: Rotation) -> Area {
    let way = rotation.apply(Point::new(0, 3));
    let side = rotation.apply(Point::new(1, 0));
    let corners = [
        Point::new(at.x + side.x, at.y + side.y),
        Point::new(at.x - side.x + way.x, at.y - side.y + way.y),
    ];
    Area {
        x0: corners[0].x.min(corners[1].x),
        x1: corners[0].x.max(corners[1].x),
        y0: corners[0].y.min(corners[1].y),
        y1: corners[0].y.max(corners[1].y),
    }
}

/// Lay `chain` out in a straight line from the pin at `anchor`, in the
/// direction `along`, for `lead` before its first part. If `aside` is not
/// nothing, the wire first runs `turn` that way, then steps that far to one
/// side. Gives each way of finishing it, if there is any.
fn lay_out(
    chain: &Chain,
    anchor: Point,
    along: Point,
    (turn, aside, lead): (i32, i32, i32),
    targets: &[Point],
) -> Vec<Run> {
    let mut wires = Vec::new();
    let mut at = anchor;
    if aside != 0 {
        let across = Point::new(-along.y, along.x);
        let corner = step(at, along, turn);
        let turned = step(corner, across, aside);
        wires.push(Seg { a: at, b: corner });
        wires.push(Seg {
            a: corner,
            b: turned,
        });
        at = turned;
    }
    let mut cursor = step(at, along, lead);
    wires.push(Seg { a: at, b: cursor });

    let mut parts = Vec::new();
    for (i, link) in chain.links.iter().enumerate() {
        if i > 0 {
            let next = step(cursor, along, STUB);
            wires.push(Seg { a: cursor, b: next });
            cursor = next;
        }
        let pins = link.part.kind.pins();
        let (inward, outer) = (pins[link.entry].offset, pins[1 - link.entry].offset);
        let turned = [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270]
            .into_iter()
            .find(|r| {
                let span = r.apply(Point::new(outer.x - inward.x, outer.y - inward.y));
                (span.x.signum(), span.y.signum()) == (along.x, along.y)
            });
        let Some(rotation) = turned else {
            return Vec::new();
        };
        let offset = rotation.apply(inward);
        let position = Point::new(cursor.x - offset.x, cursor.y - offset.y);
        let leaving = rotation.apply(outer);
        cursor = Point::new(position.x + leaving.x, position.y + leaving.y);
        parts.push((position, rotation));
    }

    let run = |wires: Vec<Seg>, ground, label| Run {
        cost: 0,
        parts: parts.clone(),
        wires,
        ground,
        label,
    };
    let beyond = step(cursor, along, STUB);
    wires.push(Seg {
        a: cursor,
        b: beyond,
    });
    match &chain.end {
        End::Ground => {
            // Straight on, the symbol turned to suit; or, from a level
            // chain, turned down to a symbol hanging the usual way.
            let straight = (Some((beyond, ground_rotation(along))), wires.clone());
            if along.y != 0 {
                vec![run(straight.1, straight.0, None)]
            } else {
                let below = Point::new(beyond.x, beyond.y + STUB);
                wires.push(Seg {
                    a: beyond,
                    b: below,
                });
                let mut sideways = run(straight.1, straight.0, None);
                sideways.cost = 6;
                vec![run(wires, Some((below, Rotation::R0)), None), sideways]
            }
        }
        End::Open(name) => {
            // A name reads to the right of its point: going left, the wire
            // is longer so that the name ends before the part.
            let at = if along.x < 0 {
                let at = step(beyond, along, (name.len() as i32 + 1) / 2);
                wires.push(Seg { a: beyond, b: at });
                at
            } else {
                beyond
            };
            vec![run(wires, None, Some(at))]
        }
        End::Existing(_) => targets
            .iter()
            .flat_map(|&t| routes(beyond, t))
            .map(|route| {
                let mut all = wires.clone();
                all.extend(route);
                run(all, None, None)
            })
            .collect(),
    }
}

/// True if `p` is on the wire, its ends included.
fn on(w: &Seg, p: Point) -> bool {
    let ((x0, x1), (y0, y1)) = (w.x_range(), w.y_range());
    (w.horizontal() || w.vertical()) && x0 <= p.x && p.x <= x1 && y0 <= p.y && p.y <= y1
}

/// Draw `chain` with wires beside a pin on the net it starts from: the
/// shortest, straightest way that keeps clear of what is drawn and passes
/// the check. `None` leaves its parts for the labels.
fn draw_chain(
    s: &mut Schematic,
    before: &Netlist,
    map: &NodeMap,
    nets: &HashMap<String, Net>,
    chain: &Chain,
) -> Option<String> {
    // The pins it could start from, and which way each one points.
    let anchors: Vec<(Point, Point)> = before
        .elements
        .iter()
        .flat_map(|e| {
            let c = s.component(e.component);
            let pins = c.into_iter().flat_map(|c| {
                c.pin_positions()
                    .map(|(def, at)| (at, pin_direction(c, def.offset)))
                    .collect::<Vec<_>>()
            });
            pins.zip(&e.nodes)
                .filter(|(_, node)| **node == chain.start)
                .map(|(pin, _)| pin)
                .collect::<Vec<_>>()
        })
        .take(TARGETS)
        .collect();
    let targets = match chain.end {
        End::Existing(node) => {
            let from: Vec<Point> = anchors.iter().map(|a| a.0).collect();
            nearest(net_points(s, before, map, node), &from)
        }
        _ => Vec::new(),
    };

    // What the new drawing must keep clear of, labels included.
    let mut boxes: Vec<Area> = s
        .components
        .iter()
        .map(|c| {
            let pins: Vec<Point> = c.pin_positions().map(|(_, at)| at).collect();
            if matches!(c.kind, K::Ground) {
                ground_area(c.position, c.rotation)
            } else {
                body_of(&pins, false)
            }
        })
        .collect();
    boxes.extend(s.labels.iter().map(|l| label_area(l.position, &l.name)));
    let wires: Vec<Seg> = s
        .wires
        .iter()
        .map(|w| Seg { a: w.a, b: w.b })
        .filter(|w| w.horizontal() || w.vertical())
        .collect();

    // The way out from each pin, best left free for what may join it later.
    let ways_out: Vec<(Point, Seg)> = s
        .components
        .iter()
        .flat_map(|c| {
            c.pin_positions()
                .map(|(def, at)| {
                    let away = pin_direction(c, def.offset);
                    let way = Seg {
                        a: step(at, away, 1),
                        b: step(at, away, 3 * STUB),
                    };
                    (at, way)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    // Anything a new wire passes over or ends on joins it.
    let joints: Vec<Point> = points_of(s).collect();

    let [right, down] = [Point::new(1, 0), Point::new(0, 1)];
    let [left, up] = [Point::new(-1, 0), Point::new(0, -1)];
    let mut runs = Vec::new();
    for &(anchor, outward) in &anchors {
        for along in [outward, right, down, left, up] {
            // How far out the wire turns, how far it steps aside, and how
            // far it runs on to the first part.
            let ways = (-24..=24).step_by(2).flat_map(|aside| {
                let turns = if aside == 0 { STUB } else { 6 * STUB };
                (1..=turns).map(move |turn| (turn, aside))
            });
            for (turn, aside) in ways {
                for lead in (STUB..=24).step_by(2) {
                    'run: for mut run in
                        lay_out(chain, anchor, along, (turn, aside, lead), &targets)
                    {
                        // Room for each part and the text beside it.
                        let mut bodies: Vec<Area> = Vec::new();
                        for (link, &(position, rotation)) in chain.links.iter().zip(&run.parts) {
                            let ends: Vec<Point> = link
                                .part
                                .kind
                                .pins()
                                .iter()
                                .map(|p| {
                                    let o = rotation.apply(p.offset);
                                    Point::new(position.x + o.x, position.y + o.y)
                                })
                                .collect();
                            let body = body_of(&ends, false);
                            let roomy = Area {
                                y0: body.y0 - 1,
                                y1: body.y1 + 1,
                                ..body
                            };
                            if boxes.iter().any(|b| b.overlaps(&roomy))
                                || wires.iter().any(|w| roomy.crossed_by(w))
                            {
                                continue 'run;
                            }
                            bodies.push(body);
                        }
                        if let Some((at, rotation)) = run.ground {
                            let area = ground_area(at, rotation);
                            if boxes.iter().any(|b| b.overlaps(&area))
                                || wires.iter().any(|w| area.crossed_by(w))
                            {
                                continue;
                            }
                        }
                        if let (Some(at), End::Open(name)) = (run.label, &chain.end) {
                            let area = label_area(at, name);
                            if boxes.iter().chain(&bodies).any(|b| b.overlaps(&area)) {
                                continue;
                            }
                        }
                        run.wires.retain(|w| w.a != w.b);
                        // It may only touch the circuit where it starts, and
                        // where it ends if it ends there.
                        let meant = |p: Point| p == anchor || targets.contains(&p);
                        let new_ends = run.wires.iter().flat_map(|w| [w.a, w.b]);
                        let ground = run.ground.map(|g| g.0);
                        if joints
                            .iter()
                            .any(|&p| !meant(p) && run.wires.iter().any(|w| on(w, p)))
                            || new_ends
                                .chain(ground)
                                .any(|p| !meant(p) && wires.iter().any(|w| on(w, p)))
                        {
                            continue;
                        }
                        for w in &run.wires {
                            if boxes.iter().chain(&bodies).any(|b| b.crossed_by(w))
                                || wires.iter().any(|e| collinear_overlap(w, e))
                            {
                                continue 'run;
                            }
                            let crossings = wires.iter().filter(|e| crosses(w, e)).count();
                            run.cost += w.length() + 2 + 6 * crossings as i32;
                        }
                        let blocked = ways_out
                            .iter()
                            .filter(|(pin, way)| {
                                *pin != anchor
                                    && run.wires.iter().any(|w| collinear_overlap(w, way))
                            })
                            .count();
                        run.cost += 20 * blocked as i32;
                        // The way the pin points is the natural one, and
                        // upwards the least.
                        if along != outward {
                            run.cost += if along == up { 30 } else { 16 };
                        }
                        run.cost += aside.abs() + turn;
                        runs.push(run);
                    }
                }
            }
        }
    }
    runs.sort_by_key(|r| r.cost);

    let drawn: Vec<NewPart> = chain.links.iter().map(|l| l.part.clone()).collect();
    for run in runs.into_iter().take(TRIES) {
        let mut trial = s.clone();
        for (part, &(position, rotation)) in drawn.iter().zip(&run.parts) {
            let id =
                trial.add_component_named(part.kind.clone(), part.designator.clone(), position);
            if let Some(c) = trial.component_mut(id) {
                c.rotation = rotation;
            }
            library::ensure_model(&mut trial, &part.kind);
        }
        for w in &run.wires {
            trial.add_wire(w.a, w.b);
        }
        if let Some((at, rotation)) = run.ground {
            let id = trial.add_component(K::Ground, at);
            if let Some(c) = trial.component_mut(id) {
                c.rotation = rotation;
            }
        }
        if let (Some(at), End::Open(name)) = (run.label, &chain.end) {
            trial.add_label(name.clone(), at);
        }
        if verify(&trial, before, &drawn, nets).is_ok() {
            *s = trial;
            let names: Vec<&str> = drawn.iter().map(|p| p.designator.as_str()).collect();
            return Some(format!(
                "{} drawn with wires next to the circuit.",
                names.join(", ")
            ));
        }
    }
    None
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
            // Ground counts as a net of the circuit even before anything
            // in it is grounded.
            if a.is_ground() != b.is_ground()
                || forward.insert(a, b).is_some_and(|prev| prev != b)
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
    // Where each net only the new parts have has been found.
    let mut new_nodes: HashMap<&str, NodeId> = HashMap::new();
    for p in parts {
        let Some(e) = after.element(&p.designator) else {
            return Err(format!("{} did not appear in the circuit.", p.designator));
        };
        for (net, &node) in p.nets.iter().zip(&e.nodes) {
            if net == UNJOINED {
                let pins = after.elements.iter().flat_map(|e| &e.nodes);
                let alone = !node.is_ground()
                    && !after.nodes[node.0].labelled
                    && pins.filter(|&&n| n == node).count() == 1;
                if !alone {
                    return Err(format!(
                        "An unused pin of {} would be joined to something; nothing was \
                         changed.",
                        p.designator
                    ));
                }
                continue;
            }
            let wanted = if is_ground(net) {
                Some(NodeId::GROUND)
            } else {
                match nets.get(net) {
                    Some(Net::Existing { old, .. }) => forward.get(old).copied(),
                    // By its label, or if it is only wires, wherever its
                    // first pin is; either way a net of its own.
                    _ => {
                        let at = after.node_by_name(net).unwrap_or_else(|| {
                            new_nodes.get(net.as_str()).copied().unwrap_or(node)
                        });
                        let shared = at.is_ground()
                            || backward.contains_key(&at)
                            || new_nodes.iter().any(|(&n, &v)| v == at && n != net);
                        new_nodes.insert(net, at);
                        (!shared).then_some(at)
                    }
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

/// What each pin of a new part joins, for the card.
pub fn connections(part: &NewPart) -> String {
    if part.nets.iter().all(|n| n != UNJOINED) {
        return part.nets.join(", ");
    }
    let used: Vec<String> = part
        .kind
        .pins()
        .iter()
        .zip(&part.nets)
        .filter(|(_, net)| *net != UNJOINED)
        .map(|(pin, net)| format!("{} {net}", pin.name))
        .collect();
    used.join(", ")
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
        other => crate::properties::kind_name(other).to_string(),
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
    fn every_part_in_the_palette_can_be_added() {
        use std::mem::discriminant;
        // The same sort of part, whatever its values.
        let same = |a: &K, b: &K| {
            discriminant(a) == discriminant(b)
                && match (a, b) {
                    (K::Bjt { polarity: a, .. }, K::Bjt { polarity: b, .. }) => a == b,
                    (K::Mosfet { polarity: a, .. }, K::Mosfet { polarity: b, .. }) => a == b,
                    (K::MultiSwitch { form: a, .. }, K::MultiSwitch { form: b, .. }) => a == b,
                    (K::Switch { momentary: a, .. }, K::Switch { momentary: b, .. }) => a == b,
                    (K::Gate { op: a, .. }, K::Gate { op: b, .. }) => a == b,
                    (K::SevenSegment { common: a, .. }, K::SevenSegment { common: b, .. })
                    | (K::FourDigit { common: a, .. }, K::FourDigit { common: b, .. }) => a == b,
                    (K::VoltageSource { waveform: a }, K::VoltageSource { waveform: b }) => {
                        discriminant(a) == discriminant(b)
                    }
                    _ => true,
                }
        };
        let can: Vec<K> = KINDS
            .iter()
            .map(|(word, example, _)| build_kind(word, example).unwrap().0)
            .collect();
        for entry in library::palette() {
            let kind = (entry.make)();
            // Ground is net 0, and a junction only joins wires.
            if matches!(kind, K::Ground | K::Junction) {
                continue;
            }
            assert!(
                can.iter().any(|k| same(k, &kind)),
                "the assistant cannot add {}",
                entry.name
            );
        }
    }

    #[test]
    fn the_values_of_the_new_kinds_are_checked() {
        let p = parse_add(
            "V2 pulse 0:5@2k a 0\nD2 zener default a 0\nD3 diode zener b 0\n\
             LED1 led Green a 0\nS2 button closed a b\nS3 dpdt thrown a b c d e f",
        )
        .unwrap();
        assert!(matches!(
            p[0].kind,
            K::VoltageSource { waveform: Waveform::Pulse { v2, period, width, .. } }
                if v2 == 5.0 && (period - 500e-6).abs() < 1e-12 && width < period / 2.0
        ));
        assert!(
            p[1..3]
                .iter()
                .all(|d| matches!(&d.kind, K::Diode { model } if model == GENERIC_ZENER))
        );
        assert!(matches!(
            p[3].kind,
            K::Led {
                colour: LedColour::Green
            }
        ));
        for bad in [
            "V2 vpulse 5@1k a 0",
            "V2 vpulse 0:5@0 a 0",
            "LED1 led purple a 0",
            "S3 spdt open a b c",
            "S2 button maybe a b",
        ] {
            assert!(parse_add(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_part_with_many_pins_may_name_only_those_it_uses() {
        let p = parse_add("DS1 7seg-cathode red b=SB c=SC com=0").unwrap();
        assert_eq!(p[0].nets, ["-", "SB", "SC", "-", "-", "-", "-", "-", "0"]);
        assert_eq!(connections(&p[0]), "b SB, c SC, COM 0");
        assert!(p[0].summary.contains("common cathode"), "{}", p[0].summary);
        for (bad, needle) in [
            ("DS1 7seg-cathode red b=SB q=SC", "no pin called q"),
            ("DS1 7seg-cathode red b=SB SC", "every pin"),
            ("DS1 7seg-cathode red b=SB B=SC", "twice"),
            ("DS1 7seg-cathode red b= c=SC", "every pin"),
            // Parts with few pins name them all, in order.
            ("R1 resistor 1k p=A n=B", "pin order"),
        ] {
            let e = parse_add(bad).unwrap_err();
            assert!(e.contains(needle), "{bad}: {e}");
        }
    }

    #[test]
    fn an_led_is_wired_to_a_pin_of_a_chip_that_is_already_there() {
        let mut s = Schematic::new();
        s.add_component(
            K::Mcu {
                firmware: String::new(),
                clock: DEFAULT_MCU_CLOCK,
            },
            Point::new(0, 0),
        );
        // D1 sits in a row of pins, with others above and below it.
        let parts = parse_add(
            "R1 resistor 220 U1.D1 LA\nLED1 led red LA 0\nR2 resistor 220 u1.d13 LB\n\
             LED2 led green LB 0",
        )
        .unwrap();
        add_parts(&mut s, &parts).unwrap();
        let n = build_netlist(&s);
        let chip = n.element("U1").unwrap();
        let pin = |name: &str| {
            let at = chip.kind.pins().iter().position(|p| p.name == name);
            chip.nodes[at.unwrap()]
        };
        assert_eq!(n.element("R1").unwrap().nodes[0], pin("D1"));
        assert_eq!(n.element("R2").unwrap().nodes[0], pin("D13"));
        assert_ne!(pin("D1"), pin("D13"));
        assert!(n.diagnostics.is_empty(), "{:?}", n.diagnostics);
    }

    #[test]
    fn a_chip_and_a_four_digit_display_are_added_to_a_circuit() {
        let mut s = divider();
        let before = build_netlist(&s);
        let segments: String = ["a", "b", "c", "d", "e", "f", "g"]
            .iter()
            .enumerate()
            .map(|(k, seg)| format!("R{} resistor 220 P{k} {seg}_net\n", k + 20))
            .collect();
        let chip_pins: String = (0..7).map(|k| format!(" D{k}=P{k}")).collect();
        let display_pins: String = ["a", "b", "c", "d", "e", "f", "g"]
            .iter()
            .map(|seg| format!(" {seg}={seg}_net"))
            .collect();
        let text = format!(
            "U1 atmega328p none{chip_pins} D8=0\n{segments}\
             DS1 4digit-cathode red{display_pins} D1=0 D2=0 D3=0 D4=0"
        );
        let parts = parse_add(&text).unwrap();
        add_parts(&mut s, &parts).unwrap();
        let n = build_netlist(&s);
        let (chip, display) = (n.element("U1").unwrap(), n.element("DS1").unwrap());
        for k in 0..7 {
            let r = n.element(&format!("R{}", k + 20)).unwrap();
            assert_eq!(r.nodes, [chip.nodes[k], display.nodes[k]], "segment {k}");
        }
        // Nothing is drawn over anything else.
        let areas: Vec<(&str, egui::Rect)> = s
            .components
            .iter()
            .filter(|c| !matches!(c.kind, K::Ground))
            .map(|c| (c.designator.as_str(), crate::symbols::bounds(c).shrink(0.1)))
            .collect();
        for (i, (a, area)) in areas.iter().enumerate() {
            for (b, other) in &areas[i + 1..] {
                assert!(!area.intersects(*other), "{a} lies over {b}");
            }
        }
        // The unused pins stayed alone, so nothing is said of them, and the
        // circuit that was there is untouched.
        assert!(n.diagnostics.is_empty(), "{:?}", n.diagnostics);
        for e in &before.elements {
            let now = n.element(&e.designator).unwrap();
            for (a, b) in e.nodes.iter().zip(&now.nodes) {
                assert_eq!(before.nodes[a.0].name, n.nodes[b.0].name);
            }
        }
    }

    /// A circuit holding one microcontroller and nothing else.
    fn lone_chip() -> Schematic {
        let mut s = Schematic::new();
        s.add_component(
            K::Mcu {
                firmware: String::new(),
                clock: DEFAULT_MCU_CLOCK,
            },
            Point::new(0, 0),
        );
        s
    }

    /// Fail if any part is drawn over another, or the netlist has anything to
    /// say.
    fn assert_clear(s: &Schematic) {
        assert_apart(s);
        let n = build_netlist(s);
        assert!(
            n.diagnostics.is_empty(),
            "{:?}\n{}",
            n.diagnostics,
            ascii(s)
        );
    }

    /// Fail if any part is drawn over another.
    fn assert_apart(s: &Schematic) {
        let areas: Vec<(&str, egui::Rect)> = s
            .components
            .iter()
            .map(|c| (c.designator.as_str(), crate::symbols::bounds(c).shrink(0.2)))
            .collect();
        for (i, (a, area)) in areas.iter().enumerate() {
            for (b, other) in &areas[i + 1..] {
                assert!(!area.intersects(*other), "{a} lies over {b}\n{}", ascii(s));
            }
        }
    }

    #[test]
    fn a_resistor_and_led_are_drawn_with_wires_from_a_chip_pin() {
        for pin in ["D13", "D2", "A0"] {
            let mut s = lone_chip();
            let text = format!("R1 resistor 220 U1.{pin} LA\nLED1 led red LA 0");
            let notes = add_parts(&mut s, &parse_add(&text).unwrap()).unwrap();
            println!("{pin}: {notes:?}\n{}", ascii(&s));
            assert!(notes.iter().any(|n| n.contains("with wires")), "{notes:?}");
            // No labels at all: it is one drawing.
            assert!(s.labels.is_empty(), "{:?}", s.labels);
            assert_clear(&s);
            // Beside the chip, level with the pin it comes from.
            let chip = s.components[0].clone();
            let (_, at) = chip.pin_positions().find(|(d, _)| d.name == pin).unwrap();
            for name in ["R1", "LED1"] {
                let c = find(&s, name).unwrap();
                assert_eq!(c.position.y, at.y, "{pin} {name}");
                assert!((c.position.x - at.x).abs() <= 16, "{pin} {name}");
            }
            let n = build_netlist(&s);
            let (r, led) = (n.element("R1").unwrap(), n.element("LED1").unwrap());
            let index = chip.kind.pins().iter().position(|p| p.name == pin).unwrap();
            assert_eq!(r.nodes[0], n.element("U1").unwrap().nodes[index]);
            assert_eq!(r.nodes[1], led.nodes[0]);
            assert!(led.nodes[1].is_ground() && !led.nodes[0].is_ground());
        }
    }

    #[test]
    fn leds_on_neighbouring_pins_are_all_wired_without_overlapping() {
        let mut s = lone_chip();
        let text: String = (2..5)
            .map(|k| format!("R{k} resistor 220 U1.D{k} N{k}x\nLED{k} led red N{k}x 0\n"))
            .collect();
        let notes = add_parts(&mut s, &parse_add(&text).unwrap()).unwrap();
        println!("{notes:?}\n{}", ascii(&s));
        assert!(s.labels.is_empty(), "{}", ascii(&s));
        assert_clear(&s);
        // One at a time works too, as when the user asks again.
        add_parts(
            &mut s,
            &parse_add("R9 resistor 220 U1.D5 Q\nLED9 led green Q 0").unwrap(),
        )
        .unwrap();
        assert!(s.labels.is_empty(), "{}", ascii(&s));
        assert_clear(&s);
    }

    #[test]
    fn a_chain_leaves_an_ordinary_circuit_by_wire() {
        let mut s = divider();
        let labels = s.labels.len();
        let text = "R9 resistor 1k VOUT X\nD9 led yellow X Y\nR8 resistor 100 Y 0";
        let notes = add_parts(&mut s, &parse_add(text).unwrap()).unwrap();
        println!("{notes:?}\n{}", ascii(&s));
        assert_eq!(s.labels.len(), labels, "{}", ascii(&s));
        assert_clear(&s);
        assert_eq!(node_of(&s, "R9", 0), "VOUT");
        assert_eq!(node_of(&s, "R9", 1), node_of(&s, "D9", 0));
        assert_eq!(node_of(&s, "D9", 1), node_of(&s, "R8", 0));
        assert_eq!(node_of(&s, "R8", 1), "0");
    }

    #[test]
    fn resistors_for_a_new_display_are_wired_to_the_chip() {
        let mut s = lone_chip();
        let text = "R1 resistor 220 U1.D2 SA\nR2 resistor 220 U1.D5 SB\n\
                    DS1 7seg-cathode red a=SA b=SB COM=0";
        let notes = add_parts(&mut s, &parse_add(text).unwrap()).unwrap();
        println!("{notes:?}\n{}", ascii(&s));
        assert!(notes[0].contains("R1 drawn with wires"), "{notes:?}");
        // Only the display's side of each net needs its name.
        let mut names: Vec<&str> = s.labels.iter().map(|l| l.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["SA", "SA", "SB", "SB"]);
        // (Its other segments are rightly reported as unconnected.)
        assert_apart(&s);
        assert_eq!(node_of(&s, "DS1", 0), "SA");
        assert_eq!(node_of(&s, "R2", 1), "SB");
    }

    #[test]
    fn nothing_may_ground_a_pin_that_was_not_meant_to_be() {
        // No ground symbol yet: a wire from D3 onto one being added with the
        // part must still be refused.
        let s = lone_chip();
        let before = build_netlist(&s);
        let mut bad = s.clone();
        let chip = bad.components[0].clone();
        let (_, at) = chip.pin_positions().find(|(d, _)| d.name == "D3").unwrap();
        let ground = bad.add_component(K::Ground, Point::new(at.x - 4, at.y));
        let ground = bad.component(ground).unwrap().position;
        bad.add_wire(at, ground);
        let e = verify(&bad, &before, &[], &HashMap::new()).unwrap_err();
        assert!(e.contains("joined"), "{e}");
    }

    #[test]
    fn a_pin_of_another_new_part_stands_for_its_net() {
        let mut s = Schematic::new();
        // D13 is named only from the resistor's side, D2 from both sides, and
        // the chip's D3 is given a net that the other resistor refers to.
        let parts = parse_add(
            "U1 atmega328p none D2=U1.D2 D3=SIG\nR1 resistor 220 U1.D13 0\n\
             R2 resistor 220 u1.d2 0\nR3 resistor 220 U1.D3 0",
        )
        .unwrap();
        add_parts(&mut s, &parts).unwrap();
        let n = build_netlist(&s);
        let chip = n.element("U1").unwrap();
        for (resistor, pin) in [("R1", "D13"), ("R2", "D2"), ("R3", "D3")] {
            let at = chip.kind.pins().iter().position(|p| p.name == pin);
            let node = chip.nodes[at.unwrap()];
            assert_eq!(n.element(resistor).unwrap().nodes[0], node, "{resistor}");
        }
        assert!(n.diagnostics.is_empty(), "{:?}", n.diagnostics);
    }

    #[test]
    fn a_chip_can_only_run_a_program_the_circuit_has() {
        let parts = parse_add("U1 atmega328p main.ino D13=a").unwrap();
        let mut s = Schematic::new();
        let e = add_parts(&mut s.clone(), &parts).unwrap_err();
        assert!(e.contains("no program file called main.ino"), "{e}");
        s.sources.insert(
            "main.ino".into(),
            dunbrec_core::SourceFile {
                language: dunbrec_core::SourceLanguage::Cpp,
                text: "void setup() {}\n".into(),
            },
        );
        add_parts(&mut s, &parts).unwrap();
        assert!(
            s.components
                .iter()
                .any(|c| matches!(&c.kind, K::Mcu { firmware, .. } if firmware == "main.ino"))
        );
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
