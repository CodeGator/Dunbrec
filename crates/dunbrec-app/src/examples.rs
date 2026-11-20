//! Example schematics available from the File menu.

use dunbrec_core::{
    ComponentId, ComponentKind as K, Point, Polarity, Rotation, Schematic, Waveform,
};

use crate::library::{GENERIC_NPN, ensure_model};

pub struct Example {
    pub name: &'static str,
    pub build: fn() -> Schematic,
}

pub fn examples() -> &'static [Example] {
    &[
        Example {
            name: "Voltage divider",
            build: voltage_divider,
        },
        Example {
            name: "Common-emitter amplifier",
            build: common_emitter_amplifier,
        },
    ]
}

fn place(s: &mut Schematic, kind: K, x: i32, y: i32, rotation: Rotation) -> ComponentId {
    ensure_model(s, &kind);
    let id = s.add_component(kind, Point::new(x, y));
    s.component_mut(id).expect("just added").rotation = rotation;
    id
}

/// Add a wire through a sequence of points.
fn wire(s: &mut Schematic, points: &[(i32, i32)]) {
    for w in points.windows(2) {
        s.add_wire(Point::new(w[0].0, w[0].1), Point::new(w[1].0, w[1].1));
    }
}

fn r(ohms: f64) -> K {
    K::Resistor { resistance: ohms }
}

fn c(farads: f64) -> K {
    K::Capacitor {
        capacitance: farads,
        initial_voltage: None,
    }
}

/// 10 V across two 1 kΩ resistors; VOUT = 5 V.
pub fn voltage_divider() -> Schematic {
    let mut s = Schematic::new();
    place(
        &mut s,
        K::VoltageSource {
            waveform: Waveform::Dc(10.0),
        },
        0,
        0,
        Rotation::R0,
    ); // + (0,-2) - (0,2)
    place(&mut s, r(1e3), 4, -2, Rotation::R0); // (2,-2) (6,-2)
    place(&mut s, r(1e3), 8, 0, Rotation::R90); // (8,-2) (8,2)
    place(&mut s, K::Ground, 4, 2, Rotation::R0);
    wire(&mut s, &[(0, -2), (2, -2)]);
    wire(&mut s, &[(6, -2), (8, -2)]);
    wire(&mut s, &[(0, 2), (8, 2)]);
    s.add_label("VOUT", Point::new(7, -2));
    s
}

/// A voltage-divider-biased NPN stage with a bypassed emitter resistor and a
/// 10 mV, 1 kHz input.
pub fn common_emitter_amplifier() -> Schematic {
    let mut s = Schematic::new();
    let npn = K::Bjt {
        polarity: Polarity::N,
        model: GENERIC_NPN.into(),
    };
    place(
        &mut s,
        K::VoltageSource {
            waveform: Waveform::Dc(12.0),
        },
        -10,
        0,
        Rotation::R0,
    ); // (-10,-2) (-10,2)
    place(&mut s, r(47e3), 0, -8, Rotation::R90); // (0,-10) (0,-6)
    place(&mut s, r(10e3), 0, 4, Rotation::R90); // (0,2) (0,6)
    place(&mut s, r(4.7e3), 8, -8, Rotation::R90); // (8,-10) (8,-6)
    place(&mut s, r(1e3), 8, 6, Rotation::R90); // (8,4) (8,8)
    place(&mut s, npn, 6, 0, Rotation::R0); // C (8,-2) B (4,0) E (8,2)
    place(&mut s, c(100e-6), 11, 6, Rotation::R90); // (11,4) (11,8)
    place(&mut s, c(10e-6), -3, 0, Rotation::R0); // (-5,0) (-1,0)
    place(
        &mut s,
        K::VoltageSource {
            waveform: Waveform::Sine {
                offset: 0.0,
                amplitude: 0.01,
                frequency: 1e3,
                delay: 0.0,
                damping: 0.0,
                phase: 0.0,
            },
        },
        -7,
        3,
        Rotation::R0,
    ); // (-7,1) (-7,5)
    place(&mut s, K::Ground, -4, 10, Rotation::R0);

    // Supply rails.
    wire(&mut s, &[(-10, -2), (-10, -10), (8, -10)]);
    wire(&mut s, &[(-10, 2), (-10, 10), (11, 10)]);
    // Bias divider and base.
    wire(&mut s, &[(0, -6), (0, 2)]);
    wire(&mut s, &[(0, 0), (4, 0)]);
    wire(&mut s, &[(0, 6), (0, 10)]);
    // Collector and emitter.
    wire(&mut s, &[(8, -6), (8, -2)]);
    wire(&mut s, &[(8, 2), (8, 4)]);
    wire(&mut s, &[(8, 3), (11, 3), (11, 4)]);
    wire(&mut s, &[(8, 8), (8, 10)]);
    wire(&mut s, &[(11, 8), (11, 10)]);
    // Input.
    wire(&mut s, &[(-1, 0), (0, 0)]);
    wire(&mut s, &[(-5, 0), (-7, 0), (-7, 1)]);
    wire(&mut s, &[(-7, 5), (-7, 10)]);

    s.add_label("VOUT", Point::new(8, -4));
    s.add_label("VIN", Point::new(-6, 0));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::build_netlist;
    use dunbrec_sim::{Circuit, operating_point};

    #[test]
    fn examples_are_clean_and_simulate() {
        for ex in examples() {
            let s = (ex.build)();
            let netlist = build_netlist(&s);
            assert!(
                netlist.diagnostics.is_empty(),
                "{}: {:?}",
                ex.name,
                netlist.diagnostics
            );
            let circuit = Circuit::from_netlist(&netlist, &s.models).unwrap();
            let op = operating_point(&circuit).unwrap();
            let vout = op.voltage_by_name("VOUT").unwrap();
            match ex.name {
                "Voltage divider" => assert!((vout - 5.0).abs() < 1e-9),
                _ => assert!(vout > 3.0 && vout < 10.0, "{}: VOUT = {vout}", ex.name),
            }
        }
    }
}
