//! The parts palette: default component values and the generic device
//! models placed alongside semiconductor parts.
//!
//! The generic models are deliberately *not* named after real part numbers.
//! Real parts (2N3904, 1N4148, ...) come from a model library checked against
//! manufacturer data.
//!
//! Palette groups say what a part *is* (Diodes, Transistors, ...), never where
//! its model came from: a real part sits in its function's group, after the
//! generic one, and is recognisable by its part number.

use dunbrec_core::{
    ComponentKind, DEFAULT_LOGIC_DELAY, DeviceModel, GateOp, ModelKind, Polarity, Schematic,
    SwitchForm, Waveform,
};

pub struct PaletteEntry {
    pub name: &'static str,
    pub group: &'static str,
    pub make: fn() -> ComponentKind,
}

/// Contact resistance of a closed and of an open switch (Ω).
pub const SWITCH_R_ON: f64 = 0.01;
pub const SWITCH_R_OFF: f64 = 1e9;

pub const GENERIC_DIODE: &str = "DGEN";
pub const GENERIC_ZENER: &str = "ZGEN5V1";
pub const GENERIC_NPN: &str = "QNGEN";
pub const GENERIC_PNP: &str = "QPGEN";
pub const GENERIC_NMOS: &str = "MNGEN";
pub const GENERIC_PMOS: &str = "MPGEN";

pub fn palette() -> &'static [PaletteEntry] {
    const P: &[PaletteEntry] = &[
        PaletteEntry {
            name: "Resistor",
            group: "Passive",
            make: || ComponentKind::Resistor { resistance: 1e3 },
        },
        PaletteEntry {
            name: "Capacitor",
            group: "Passive",
            make: || ComponentKind::Capacitor {
                capacitance: 1e-6,
                initial_voltage: None,
            },
        },
        PaletteEntry {
            name: "Inductor",
            group: "Passive",
            make: || ComponentKind::Inductor {
                inductance: 1e-3,
                initial_current: None,
            },
        },
        PaletteEntry {
            name: "Potentiometer",
            group: "Passive",
            make: || ComponentKind::Potentiometer {
                resistance: 10e3,
                wiper: 0.5,
            },
        },
        PaletteEntry {
            name: "Switch",
            group: "Switches",
            make: || ComponentKind::Switch {
                closed: false,
                r_on: SWITCH_R_ON,
                r_off: SWITCH_R_OFF,
                momentary: false,
            },
        },
        PaletteEntry {
            name: "SPDT switch",
            group: "Switches",
            make: || ComponentKind::MultiSwitch {
                form: SwitchForm::Spdt,
                thrown: false,
                r_on: SWITCH_R_ON,
                r_off: SWITCH_R_OFF,
            },
        },
        PaletteEntry {
            name: "DPST switch",
            group: "Switches",
            make: || ComponentKind::MultiSwitch {
                form: SwitchForm::Dpst,
                thrown: false,
                r_on: SWITCH_R_ON,
                r_off: SWITCH_R_OFF,
            },
        },
        PaletteEntry {
            name: "DPDT switch",
            group: "Switches",
            make: || ComponentKind::MultiSwitch {
                form: SwitchForm::Dpdt,
                thrown: false,
                r_on: SWITCH_R_ON,
                r_off: SWITCH_R_OFF,
            },
        },
        PaletteEntry {
            name: "Push button (normally open)",
            group: "Switches",
            make: || ComponentKind::Switch {
                closed: false,
                r_on: SWITCH_R_ON,
                r_off: SWITCH_R_OFF,
                momentary: true,
            },
        },
        PaletteEntry {
            name: "Push button (normally closed)",
            group: "Switches",
            make: || ComponentKind::Switch {
                closed: true,
                r_on: SWITCH_R_ON,
                r_off: SWITCH_R_OFF,
                momentary: true,
            },
        },
        PaletteEntry {
            name: "Ground",
            group: "Sources",
            make: || ComponentKind::Ground,
        },
        PaletteEntry {
            name: "Voltage source",
            group: "Sources",
            make: || ComponentKind::VoltageSource {
                waveform: Waveform::Dc(5.0),
            },
        },
        PaletteEntry {
            name: "Sine source",
            group: "Sources",
            make: || ComponentKind::VoltageSource {
                waveform: Waveform::Sine {
                    offset: 0.0,
                    amplitude: 1.0,
                    frequency: 1e3,
                    delay: 0.0,
                    damping: 0.0,
                    phase: 0.0,
                },
            },
        },
        PaletteEntry {
            name: "Pulse source",
            group: "Sources",
            make: || ComponentKind::VoltageSource {
                waveform: Waveform::Pulse {
                    v1: 0.0,
                    v2: 5.0,
                    delay: 0.0,
                    rise: 1e-6,
                    fall: 1e-6,
                    width: 0.5e-3,
                    period: 1e-3,
                },
            },
        },
        PaletteEntry {
            name: "Current source",
            group: "Sources",
            make: || ComponentKind::CurrentSource {
                waveform: Waveform::Dc(1e-3),
            },
        },
        PaletteEntry {
            name: "Diode",
            group: "Diodes",
            make: || ComponentKind::Diode {
                model: GENERIC_DIODE.into(),
            },
        },
        PaletteEntry {
            name: "Zener diode",
            group: "Diodes",
            make: || ComponentKind::Diode {
                model: GENERIC_ZENER.into(),
            },
        },
        PaletteEntry {
            name: "NPN transistor",
            group: "Transistors",
            make: || ComponentKind::Bjt {
                polarity: Polarity::N,
                model: GENERIC_NPN.into(),
            },
        },
        PaletteEntry {
            name: "PNP transistor",
            group: "Transistors",
            make: || ComponentKind::Bjt {
                polarity: Polarity::P,
                model: GENERIC_PNP.into(),
            },
        },
        PaletteEntry {
            name: "N-MOSFET",
            group: "Transistors",
            make: || ComponentKind::Mosfet {
                polarity: Polarity::N,
                model: GENERIC_NMOS.into(),
            },
        },
        PaletteEntry {
            name: "P-MOSFET",
            group: "Transistors",
            make: || ComponentKind::Mosfet {
                polarity: Polarity::P,
                model: GENERIC_PMOS.into(),
            },
        },
        PaletteEntry {
            name: "2N3904 NPN",
            group: "Transistors",
            make: || ComponentKind::Bjt {
                polarity: Polarity::N,
                model: "2N3904".into(),
            },
        },
        PaletteEntry {
            name: "2N3906 PNP",
            group: "Transistors",
            make: || ComponentKind::Bjt {
                polarity: Polarity::P,
                model: "2N3906".into(),
            },
        },
        PaletteEntry {
            name: "1N4148 diode",
            group: "Diodes",
            make: || ComponentKind::Diode {
                model: "1N4148".into(),
            },
        },
        PaletteEntry {
            name: "1N750 4.7 V zener",
            group: "Diodes",
            make: || ComponentKind::Diode {
                model: "1N750".into(),
            },
        },
        PaletteEntry {
            name: "741 op-amp",
            group: "Integrated circuits",
            make: || ComponentKind::OpAmp {
                part: "uA741".into(),
            },
        },
        PaletteEntry {
            name: "555 timer",
            group: "Integrated circuits",
            make: || ComponentKind::Timer555,
        },
        PaletteEntry {
            name: "AND gate",
            group: "Digital",
            make: || ComponentKind::Gate {
                op: GateOp::And,
                delay: DEFAULT_LOGIC_DELAY,
            },
        },
        PaletteEntry {
            name: "OR gate",
            group: "Digital",
            make: || ComponentKind::Gate {
                op: GateOp::Or,
                delay: DEFAULT_LOGIC_DELAY,
            },
        },
        PaletteEntry {
            name: "NAND gate",
            group: "Digital",
            make: || ComponentKind::Gate {
                op: GateOp::Nand,
                delay: DEFAULT_LOGIC_DELAY,
            },
        },
        PaletteEntry {
            name: "NOR gate",
            group: "Digital",
            make: || ComponentKind::Gate {
                op: GateOp::Nor,
                delay: DEFAULT_LOGIC_DELAY,
            },
        },
        PaletteEntry {
            name: "XOR gate",
            group: "Digital",
            make: || ComponentKind::Gate {
                op: GateOp::Xor,
                delay: DEFAULT_LOGIC_DELAY,
            },
        },
        PaletteEntry {
            name: "XNOR gate",
            group: "Digital",
            make: || ComponentKind::Gate {
                op: GateOp::Xnor,
                delay: DEFAULT_LOGIC_DELAY,
            },
        },
        PaletteEntry {
            name: "Inverter (NOT)",
            group: "Digital",
            make: || ComponentKind::Inverter {
                delay: DEFAULT_LOGIC_DELAY,
            },
        },
        PaletteEntry {
            name: "D flip-flop",
            group: "Digital",
            make: || ComponentKind::DFlipFlop {
                delay: DEFAULT_LOGIC_DELAY,
            },
        },
        PaletteEntry {
            name: "4-bit counter",
            group: "Digital",
            make: || ComponentKind::Counter4 {
                delay: DEFAULT_LOGIC_DELAY,
            },
        },
        PaletteEntry {
            name: "Clock",
            group: "Digital",
            make: || ComponentKind::Clock {
                frequency: 1e3,
                duty: 0.5,
            },
        },
        PaletteEntry {
            name: "Logic switch",
            group: "Digital",
            make: || ComponentKind::LogicSwitch { high: false },
        },
        PaletteEntry {
            name: "Logic LED",
            group: "Digital",
            make: || ComponentKind::LogicLed,
        },
        PaletteEntry {
            name: "ATmega328P (Arduino)",
            group: "Microcontrollers",
            make: || ComponentKind::Mcu {
                firmware: "builtin:blink".into(),
                clock: dunbrec_core::DEFAULT_MCU_CLOCK,
            },
        },
        PaletteEntry {
            name: "VCVS (E)",
            group: "Controlled sources",
            make: || ComponentKind::Vcvs { gain: 10.0 },
        },
        PaletteEntry {
            name: "VCCS (G)",
            group: "Controlled sources",
            make: || ComponentKind::Vccs {
                transconductance: 1e-3,
            },
        },
        PaletteEntry {
            name: "CCVS (H)",
            group: "Controlled sources",
            make: || ComponentKind::Ccvs {
                transresistance: 1e3,
            },
        },
        PaletteEntry {
            name: "CCCS (F)",
            group: "Controlled sources",
            make: || ComponentKind::Cccs { gain: 10.0 },
        },
    ];
    P
}

/// Palette entries grouped for display: groups in order of first appearance,
/// entries in list order within each. `query` (lower case; empty matches
/// everything) keeps an entry if it occurs in the entry's name, its group's
/// name or its part type ("transistor", "operational amplifier"), so typing
/// "diode" lists every diode and "3904" finds one part. Groups with no
/// matching entry are left out.
pub fn palette_groups(query: &str) -> Vec<(&'static str, Vec<&'static PaletteEntry>)> {
    let mut groups: Vec<(&'static str, Vec<&'static PaletteEntry>)> = Vec::new();
    for entry in palette() {
        let matches = query.is_empty()
            || [
                entry.name,
                entry.group,
                crate::properties::kind_name(&(entry.make)()),
            ]
            .iter()
            .any(|text| text.to_lowercase().contains(query));
        if !matches {
            continue;
        }
        match groups.iter_mut().find(|(g, _)| *g == entry.group) {
            Some((_, entries)) => entries.push(entry),
            None => groups.push((entry.group, vec![entry])),
        }
    }
    groups
}

/// The generic model a palette part refers to, if it is not yet in the
/// schematic's library.
fn generic_model(name: &str) -> Option<DeviceModel> {
    let m = match name {
        GENERIC_DIODE => DeviceModel::new(name, ModelKind::Diode)
            .with("IS", 1e-14)
            .with("RS", 1.0)
            .with("CJO", 2e-12)
            .with("TT", 5e-9),
        GENERIC_ZENER => DeviceModel::new(name, ModelKind::Diode)
            .with("IS", 1e-14)
            .with("RS", 1.0)
            .with("BV", 5.1)
            .with("IBV", 1e-3)
            .with("CJO", 50e-12),
        GENERIC_NPN | GENERIC_PNP => {
            let kind = if name == GENERIC_NPN {
                ModelKind::Npn
            } else {
                ModelKind::Pnp
            };
            DeviceModel::new(name, kind)
                .with("IS", 1e-14)
                .with("BF", 200.0)
                .with("VAF", 100.0)
                .with("CJE", 8e-12)
                .with("CJC", 4e-12)
                .with("TF", 0.4e-9)
        }
        GENERIC_NMOS => DeviceModel::new(name, ModelKind::Nmos)
            .with("VTO", 2.0)
            .with("KP", 0.1)
            .with("LAMBDA", 0.01)
            .with("CGSO", 50e-12)
            .with("CGDO", 10e-12)
            .with("CBD", 20e-12),
        GENERIC_PMOS => DeviceModel::new(name, ModelKind::Pmos)
            .with("VTO", -2.0)
            .with("KP", 0.05)
            .with("LAMBDA", 0.01)
            .with("CGSO", 50e-12)
            .with("CGDO", 10e-12)
            .with("CBD", 20e-12),
        _ => return None,
    };
    Some(m)
}

/// Make sure the model a component refers to exists, adding the generic
/// model if it is one of ours and missing.
/// Make sure the model a component refers to exists, adding it from the
/// generic models or the library of real parts if it is missing.
pub fn ensure_model(schematic: &mut Schematic, kind: &ComponentKind) {
    if let Some(name) = kind.model_name()
        && schematic.models.get(name).is_none()
        && let Some(model) =
            generic_model(name).or_else(|| dunbrec_core::parts::standard_model(name))
    {
        schematic.models.insert(model);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::{Point, build_netlist};

    fn names(groups: &[(&str, Vec<&PaletteEntry>)]) -> Vec<&'static str> {
        groups
            .iter()
            .flat_map(|(_, entries)| entries.iter().map(|e| e.name))
            .collect()
    }

    #[test]
    fn groups_describe_what_a_part_is() {
        let all = palette_groups("");
        // Every entry appears exactly once, and none sits in a provenance group.
        let listed = names(&all);
        assert_eq!(listed.len(), palette().len());
        assert!(all.iter().all(|(g, _)| *g != "Real parts"));
        let group_of = |part: &str| {
            all.iter()
                .find(|(_, entries)| entries.iter().any(|e| e.name == part))
                .map(|(g, _)| *g)
        };
        // A real part sits with its generic siblings, after them.
        assert_eq!(group_of("2N3904 NPN"), Some("Transistors"));
        assert_eq!(group_of("NPN transistor"), Some("Transistors"));
        assert_eq!(group_of("1N4148 diode"), group_of("Diode"));
        let transistors = &all.iter().find(|(g, _)| *g == "Transistors").unwrap().1;
        let position = |n: &str| transistors.iter().position(|e| e.name == n).unwrap();
        assert!(position("NPN transistor") < position("2N3904 NPN"));
    }

    #[test]
    fn switches_have_their_own_group() {
        let all = palette_groups("");
        let switches = &all.iter().find(|(g, _)| *g == "Switches").unwrap().1;
        let names: Vec<_> = switches.iter().map(|e| e.name).collect();
        assert_eq!(
            names,
            [
                "Switch",
                "SPDT switch",
                "DPST switch",
                "DPDT switch",
                "Push button (normally open)",
                "Push button (normally closed)"
            ]
        );
        // Searching by part type finds them.
        assert!(
            super::palette_groups("push")
                .iter()
                .any(|(g, _)| *g == "Switches")
        );
        // The plain switch is no longer a passive part.
        let passive = &all.iter().find(|(g, _)| *g == "Passive").unwrap().1;
        assert!(passive.iter().all(|e| e.name != "Switch"));
    }

    #[test]
    fn searching_matches_name_group_and_part_type() {
        // By part number.
        assert_eq!(names(&palette_groups("3904")), vec!["2N3904 NPN"]);
        // By group: every diode, generic and real.
        let diodes = names(&palette_groups("diode"));
        for d in ["Diode", "Zener diode", "1N4148 diode"] {
            assert!(diodes.contains(&d), "{diodes:?}");
        }
        // By part type, which the name does not contain.
        assert!(names(&palette_groups("operational")).contains(&"741 op-amp"));
        assert!(names(&palette_groups("transistor")).contains(&"2N3906 PNP"));
        // Nothing matches nonsense; groups without matches are left out.
        assert!(palette_groups("zzzz").is_empty());
        assert!(
            palette_groups("3904")
                .iter()
                .all(|(_, entries)| !entries.is_empty())
        );
    }

    #[test]
    fn every_palette_part_has_its_model() {
        let mut s = Schematic::new();
        for entry in palette() {
            let kind = (entry.make)();
            ensure_model(&mut s, &kind);
            if let Some(name) = kind.model_name() {
                assert!(s.models.get(name).is_some(), "{} lacks {name}", entry.name);
            }
            s.add_component(kind, Point::default());
        }
        let netlist = build_netlist(&s);
        assert!(
            !netlist.diagnostics.iter().any(|d| matches!(
                d,
                dunbrec_core::Diagnostic::MissingModel { .. }
                    | dunbrec_core::Diagnostic::WrongModelKind { .. }
            )),
            "{:?}",
            netlist.diagnostics
        );
    }
}
