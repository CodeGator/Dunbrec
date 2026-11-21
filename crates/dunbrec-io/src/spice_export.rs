//! SPICE netlist export, readable by ngspice and other SPICE simulators.
//!
//! Dunbrec parts with no direct SPICE element are converted:
//! * a potentiometer becomes two resistors meeting at the wiper;
//! * a switch becomes a resistor of its present on/off resistance;
//! * a current-controlled source (F/H) gets a 0 V sensing source for its
//!   control port, which SPICE needs to measure the current.
//!
//! MOSFET `W`/`L` stored in a Dunbrec model are written on the instance
//! line, where SPICE expects them.

use std::fmt::Write;

use dunbrec_core::{
    ComponentKind as K, ModelKind, Netlist, NodeId, Schematic, Severity, Waveform, build_netlist,
};
use thiserror::Error;

/// Smallest resistance of a potentiometer segment (matches the simulator).
const POT_MIN_SEGMENT: f64 = 1e-3;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum ExportError {
    #[error("the schematic has problems that must be fixed before exporting: {0:?}")]
    Netlist(Vec<String>),
    #[error(
        "logic parts, microcontrollers and 555 timers cannot be written as a plain SPICE \
         netlist: {}",
        .0.join(", ")
    )]
    Digital(Vec<String>),
}

/// A SPICE-safe identifier.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Element name starting with the SPICE type letter.
fn element_name(letter: char, designator: &str) -> String {
    let d = sanitize(designator);
    if d.to_ascii_uppercase().starts_with(letter) {
        d
    } else {
        format!("{letter}{d}")
    }
}

/// Shortest exact representation, which SPICE reads back unchanged.
fn num(v: f64) -> String {
    format!("{v:e}")
}

fn waveform(w: &Waveform) -> String {
    match w {
        Waveform::Dc(v) => format!("DC {}", num(*v)),
        Waveform::Sine {
            offset,
            amplitude,
            frequency,
            delay,
            damping,
            phase,
        } => format!(
            "SIN({} {} {} {} {} {})",
            num(*offset),
            num(*amplitude),
            num(*frequency),
            num(*delay),
            num(*damping),
            num(*phase)
        ),
        Waveform::Pulse {
            v1,
            v2,
            delay,
            rise,
            fall,
            width,
            period,
        } => format!(
            "PULSE({} {} {} {} {} {} {})",
            num(*v1),
            num(*v2),
            num(*delay),
            num(*rise),
            num(*fall),
            num(*width),
            num(*period)
        ),
        Waveform::Pwl(points) => {
            let pts: Vec<String> = points
                .iter()
                .map(|(t, v)| format!("{} {}", num(*t), num(*v)))
                .collect();
            format!("PWL({})", pts.join(" "))
        }
    }
}

/// Export `schematic` as a SPICE netlist. `directives` (e.g. `.tran 1u 1m`)
/// are added before `.end`.
pub fn export(
    schematic: &Schematic,
    title: &str,
    directives: &[String],
) -> Result<String, ExportError> {
    let netlist = build_netlist(schematic);
    let errors: Vec<String> = netlist
        .diagnostics
        .iter()
        .filter(|d| d.severity() == Severity::Error)
        .map(|d| format!("{d:?}"))
        .collect();
    if !errors.is_empty() {
        return Err(ExportError::Netlist(errors));
    }
    let digital: Vec<String> = netlist
        .elements
        .iter()
        .filter(|e| e.kind.is_digital() || matches!(e.kind, K::Timer555))
        .map(|e| e.designator.clone())
        .collect();
    if !digital.is_empty() {
        return Err(ExportError::Digital(digital));
    }
    Ok(write_netlist(&netlist, schematic, title, directives))
}

fn write_netlist(
    netlist: &Netlist,
    schematic: &Schematic,
    title: &str,
    directives: &[String],
) -> String {
    let node = |n: NodeId| -> String {
        if n.is_ground() {
            "0".into()
        } else {
            sanitize(&netlist.nodes[n.0].name)
        }
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}",
        if title.is_empty() {
            "Dunbrec export"
        } else {
            title
        }
    );
    let _ = writeln!(out, "* Exported by Dunbrec {}", env!("CARGO_PKG_VERSION"));
    // Subcircuits the netlist uses, written once each after the elements.
    let mut subckts: Vec<&'static str> = Vec::new();

    for e in &netlist.elements {
        let n: Vec<String> = e.nodes.iter().map(|&id| node(id)).collect();
        let d = &e.designator;
        let line = match &e.kind {
            K::Ground => continue,
            // Rejected by `export` before writing.
            K::Gate { .. }
            | K::Inverter { .. }
            | K::DFlipFlop { .. }
            | K::Counter4 { .. }
            | K::Clock { .. }
            | K::LogicSwitch { .. }
            | K::LogicLed
            | K::Mcu { .. }
            | K::Timer555 => continue,
            K::OpAmp { part } => match dunbrec_core::schematic::opamp_subckt(part) {
                Some((name, text)) => {
                    if !subckts.contains(&text) {
                        subckts.push(text);
                    }
                    format!("{} {} {name}", element_name('X', d), n.join(" "))
                }
                None => format!("* {d}: no SPICE model for op-amp '{part}'"),
            },
            K::Resistor { resistance } => {
                format!(
                    "{} {} {} {}",
                    element_name('R', d),
                    n[0],
                    n[1],
                    num(*resistance)
                )
            }
            K::Capacitor {
                capacitance,
                initial_voltage,
            } => {
                let ic = initial_voltage.map_or(String::new(), |v| format!(" IC={}", num(v)));
                format!(
                    "{} {} {} {}{ic}",
                    element_name('C', d),
                    n[0],
                    n[1],
                    num(*capacitance)
                )
            }
            K::Inductor {
                inductance,
                initial_current,
            } => {
                let ic = initial_current.map_or(String::new(), |i| format!(" IC={}", num(i)));
                format!(
                    "{} {} {} {}{ic}",
                    element_name('L', d),
                    n[0],
                    n[1],
                    num(*inductance)
                )
            }
            K::VoltageSource { waveform: w } => {
                format!("{} {} {} {}", element_name('V', d), n[0], n[1], waveform(w))
            }
            K::CurrentSource { waveform: w } => {
                format!("{} {} {} {}", element_name('I', d), n[0], n[1], waveform(w))
            }
            K::Diode { model } => format!(
                "{} {} {} {}",
                element_name('D', d),
                n[0],
                n[1],
                sanitize(model)
            ),
            K::Bjt { model, .. } => {
                format!(
                    "{} {} {} {} {}",
                    element_name('Q', d),
                    n[0],
                    n[1],
                    n[2],
                    sanitize(model)
                )
            }
            K::Mosfet { model, .. } => {
                let mut size = String::new();
                if let Some(m) = schematic.models.get(model) {
                    for p in ["W", "L"] {
                        if let Some(v) = m.params.get(p) {
                            let _ = write!(size, " {p}={}", num(*v));
                        }
                    }
                }
                // Body tied to source.
                format!(
                    "{} {} {} {} {} {}{size}",
                    element_name('M', d),
                    n[0],
                    n[1],
                    n[2],
                    n[2],
                    sanitize(model)
                )
            }
            K::MultiSwitch {
                form,
                thrown,
                r_on,
                r_off,
            } => {
                let name = element_name('R', d);
                let mut text = format!(
                    "* {} switch {d} ({})",
                    form.name(),
                    if *thrown { "thrown" } else { "rest" }
                );
                for (i, &(a, b, when)) in form.contacts().iter().enumerate() {
                    let r = if *thrown == when { r_on } else { r_off };
                    let _ = write!(
                        text,
                        "
{name}_{} {} {} {}",
                        i + 1,
                        n[a],
                        n[b],
                        num(*r)
                    );
                }
                text
            }
            K::Switch {
                closed,
                r_on,
                r_off,
                ..
            } => {
                let (r, state) = if *closed {
                    (r_on, "closed")
                } else {
                    (r_off, "open")
                };
                format!(
                    "* switch {d} ({state})\n{} {} {} {}",
                    element_name('R', d),
                    n[0],
                    n[1],
                    num(*r)
                )
            }
            K::Potentiometer { resistance, wiper } => {
                let r_aw = (resistance * wiper).max(POT_MIN_SEGMENT);
                let r_wb = (resistance * (1.0 - wiper)).max(POT_MIN_SEGMENT);
                let name = element_name('R', d);
                format!(
                    "* potentiometer {d} at {:.0}%\n{name}_A {} {} {}\n{name}_B {} {} {}",
                    wiper * 100.0,
                    n[0],
                    n[2],
                    num(r_aw),
                    n[2],
                    n[1],
                    num(r_wb)
                )
            }
            K::Vcvs { gain } => format!(
                "{} {} {} {} {} {}",
                element_name('E', d),
                n[0],
                n[1],
                n[2],
                n[3],
                num(*gain)
            ),
            K::Vccs { transconductance } => format!(
                "{} {} {} {} {} {}",
                element_name('G', d),
                n[0],
                n[1],
                n[2],
                n[3],
                num(*transconductance)
            ),
            K::Cccs { gain }
            | K::Ccvs {
                transresistance: gain,
            } => {
                let letter = if matches!(e.kind, K::Cccs { .. }) {
                    'F'
                } else {
                    'H'
                };
                let name = element_name(letter, d);
                let sense = format!("V{}_SENSE", sanitize(d));
                format!(
                    "{sense} {} {} DC 0\n{name} {} {} {sense} {}",
                    n[2],
                    n[3],
                    n[0],
                    n[1],
                    num(*gain)
                )
            }
        };
        let _ = writeln!(out, "{line}");
    }

    for text in subckts {
        out.push_str(text);
    }

    for m in schematic.models.iter() {
        let params: Vec<String> = m
            .params
            .iter()
            .filter(|(k, _)| {
                !(matches!(m.kind, ModelKind::Nmos | ModelKind::Pmos) && (*k == "W" || *k == "L"))
            })
            .map(|(k, v)| format!("{k}={}", num(*v)))
            .collect();
        let _ = writeln!(
            out,
            ".model {} {}({})",
            sanitize(&m.name),
            m.kind.spice_name(),
            params.join(" ")
        );
    }
    for d in directives {
        let _ = writeln!(out, "{d}");
    }
    out.push_str(".end\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spice::import;
    use dunbrec_core::Point;

    /// A part with every pin on its own labelled net, plus a ground.
    fn lone_part(kind: K, names: &[&str]) -> Schematic {
        let mut s = Schematic::new();
        let id = s.add_component(kind, Point::new(0, 0));
        let pins: Vec<Point> = s
            .component(id)
            .unwrap()
            .pin_positions()
            .map(|(_, p)| p)
            .collect();
        for (name, p) in names.iter().zip(pins) {
            s.add_label(*name, p);
        }
        s.add_component(K::Ground, Point::new(20, 20));
        s.add_label(names[0], Point::new(20, 20));
        s
    }

    #[test]
    fn opamps_export_as_subcircuits() {
        let mut s = lone_part(
            K::OpAmp {
                part: "uA741".into(),
            },
            &["gnd", "inv", "vp", "vn", "out"],
        );
        // A second 741 shares the one definition.
        let id = s.add_component(
            K::OpAmp {
                part: "uA741".into(),
            },
            Point::new(0, 10),
        );
        let pins: Vec<Point> = s
            .component(id)
            .unwrap()
            .pin_positions()
            .map(|(_, p)| p)
            .collect();
        for (name, p) in ["gnd", "inv", "vp", "vn", "out2"].iter().zip(pins) {
            s.add_label(*name, p);
        }
        let text = export(&s, "op-amps", &[]).unwrap();
        assert!(
            text.contains(
                "XU1 0 inv vp vn out UA741
"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "XU2 0 inv vp vn out2 UA741
"
            ),
            "{text}"
        );
        assert_eq!(text.matches(".subckt UA741 1 2 3 4 5").count(), 1, "{text}");
        assert!(text.contains(".ends UA741"));
    }

    #[test]
    fn timers_are_not_exported() {
        let s = lone_part(
            K::Timer555,
            &["gnd", "trig", "out", "reset", "ctrl", "thr", "dis", "vcc"],
        );
        match export(&s, "", &[]) {
            Err(ExportError::Digital(parts)) => assert_eq!(parts, vec!["U1".to_string()]),
            other => panic!("{other:?}"),
        }
    }

    const DECK: &str = "round trip\n\
        V1 vcc 0 DC 12\n\
        VIN sig 0 SIN(0 10m 1k)\n\
        R1 vcc b 47k\n\
        R2 b 0 10k\n\
        RC vcc c 4.7k\n\
        RE e 0 1k\n\
        C1 sig b 10u\n\
        C2 e 0 100u IC=1.2\n\
        Q1 c b e QN\n\
        D1 c vcc DX\n\
        E1 x 0 c 0 2\n\
        RX x 0 1k\n\
        .model QN NPN(IS=1e-14 BF=200 VAF=100)\n\
        .model DX D(IS=2.52e-9)\n\
        .tran 10u 5m\n";

    /// Import → schematic → export → import gives the same parts.
    #[test]
    fn round_trip_through_a_schematic() {
        let first = import(DECK).unwrap();
        let schematic = first.to_schematic();
        let text = export(&schematic, &first.title, &first.directives).unwrap();
        let second = import(&text).unwrap();
        assert!(second.warnings.is_empty(), "{:?}\n{text}", second.warnings);
        assert_eq!(second.directives, vec![".tran 10u 5m"]);
        for p in &first.parts {
            let q = second
                .parts
                .iter()
                .find(|q| q.designator == p.designator)
                .unwrap_or_else(|| panic!("{} missing in:\n{text}", p.designator));
            assert_eq!(q.kind, p.kind, "{}", p.designator);
            assert_eq!(q.nets, p.nets, "{}", p.designator);
        }
        assert_eq!(second.models, first.models);
    }

    #[test]
    fn dunbrec_only_parts_are_converted() {
        use crate::layout::{Part, layout};
        use dunbrec_core::{ModelLibrary, Rotation};
        let part = |d: &str, kind, nets: &[&str]| Part {
            designator: d.into(),
            kind,
            nets: nets.iter().map(|s| s.to_string()).collect(),
            position: None,
            rotation: Rotation::R0,
        };
        let s = layout(
            &[
                part(
                    "V1",
                    K::VoltageSource {
                        waveform: Waveform::Dc(10.0),
                    },
                    &["top", "0"],
                ),
                part(
                    "RV1",
                    K::Potentiometer {
                        resistance: 10e3,
                        wiper: 0.25,
                    },
                    &["top", "0", "w"],
                ),
                part(
                    "S1",
                    K::Switch {
                        closed: true,
                        r_on: 0.01,
                        r_off: 1e9,
                        momentary: false,
                    },
                    &["w", "out"],
                ),
                part("RL", K::Resistor { resistance: 1e3 }, &["out", "0"]),
                part("F1", K::Cccs { gain: 3.0 }, &["y", "0", "out", "z"]),
                part("RZ", K::Resistor { resistance: 1.0 }, &["z", "0"]),
                part("RY", K::Resistor { resistance: 1.0 }, &["y", "0"]),
            ],
            ModelLibrary::new(),
        );
        let text = export(&s, "conversions", &[]).unwrap();
        assert!(text.contains("RV1_A top w 2.5e3"), "{text}");
        assert!(text.contains("RV1_B w 0 7.5e3"), "{text}");
        assert!(text.contains("RS1 w out 1e-2"), "{text}");
        assert!(text.contains("VF1_SENSE out z DC 0"), "{text}");
        assert!(text.contains("F1 y 0 VF1_SENSE 3e0"), "{text}");
        // And SPICE (our importer) reads it back.
        let back = import(&text).unwrap();
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    }

    #[test]
    fn schematic_errors_block_export() {
        let mut s = Schematic::new();
        s.add_component(
            K::Resistor { resistance: 1.0 },
            dunbrec_core::Point::default(),
        );
        assert!(matches!(export(&s, "x", &[]), Err(ExportError::Netlist(_))));
    }
}
