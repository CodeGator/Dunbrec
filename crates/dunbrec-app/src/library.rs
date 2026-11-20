//! The parts palette: default component values and the generic device
//! models placed alongside semiconductor parts.
//!
//! The generic models are deliberately *not* named after real part numbers.
//! Real parts (2N3904, 1N4148, ...) belong in a model library checked against
//! manufacturer data, which comes later.

use dunbrec_core::{ComponentKind, DeviceModel, ModelKind, Polarity, Schematic, Waveform};

pub struct PaletteEntry {
    pub name: &'static str,
    pub group: &'static str,
    pub make: fn() -> ComponentKind,
}

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
            group: "Passive",
            make: || ComponentKind::Switch {
                closed: false,
                r_on: 0.01,
                r_off: 1e9,
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
            group: "Semiconductors",
            make: || ComponentKind::Diode {
                model: GENERIC_DIODE.into(),
            },
        },
        PaletteEntry {
            name: "Zener diode",
            group: "Semiconductors",
            make: || ComponentKind::Diode {
                model: GENERIC_ZENER.into(),
            },
        },
        PaletteEntry {
            name: "NPN transistor",
            group: "Semiconductors",
            make: || ComponentKind::Bjt {
                polarity: Polarity::N,
                model: GENERIC_NPN.into(),
            },
        },
        PaletteEntry {
            name: "PNP transistor",
            group: "Semiconductors",
            make: || ComponentKind::Bjt {
                polarity: Polarity::P,
                model: GENERIC_PNP.into(),
            },
        },
        PaletteEntry {
            name: "N-MOSFET",
            group: "Semiconductors",
            make: || ComponentKind::Mosfet {
                polarity: Polarity::N,
                model: GENERIC_NMOS.into(),
            },
        },
        PaletteEntry {
            name: "P-MOSFET",
            group: "Semiconductors",
            make: || ComponentKind::Mosfet {
                polarity: Polarity::P,
                model: GENERIC_PMOS.into(),
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
pub fn ensure_model(schematic: &mut Schematic, kind: &ComponentKind) {
    if let Some(name) = kind.model_name()
        && schematic.models.get(name).is_none()
        && let Some(model) = generic_model(name)
    {
        schematic.models.insert(model);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::{Point, build_netlist};

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
