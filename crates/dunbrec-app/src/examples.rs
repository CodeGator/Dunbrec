//! Example schematics available from the File menu.

use dunbrec_core::{
    ComponentId, ComponentKind as K, Point, Polarity, Rotation, Schematic, Waveform,
};

use crate::library::{GENERIC_NMOS, GENERIC_PMOS, ensure_model};

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
        Example {
            name: "RC low-pass filter",
            build: rc_lowpass,
        },
        Example {
            name: "Half-wave rectifier",
            build: half_wave_rectifier,
        },
        Example {
            name: "Zener regulator",
            build: zener_regulator,
        },
        Example {
            name: "CMOS inverter",
            build: cmos_inverter,
        },
    ]
}

fn sine(amplitude: f64, frequency: f64) -> K {
    K::VoltageSource {
        waveform: Waveform::Sine {
            offset: 0.0,
            amplitude,
            frequency,
            delay: 0.0,
            damping: 0.0,
            phase: 0.0,
        },
    }
}

/// Source on the left, a series part, then shunt parts to a ground rail:
/// the shared shape of the filter, rectifier and regulator examples.
/// `series` sits between (2,-2) and (6,-2); `shunts` are vertical at
/// x = 8, 12, ... between the top wire and the ground rail.
fn ladder(source: K, series: (K, Rotation), shunts: Vec<K>, shunt_rotation: Rotation) -> Schematic {
    let mut s = Schematic::new();
    place(&mut s, source, 0, 0, Rotation::R0); // (0,-2) (0,2)
    place(&mut s, series.0, 4, -2, series.1); // (2,-2) (6,-2)
    let right = 8 + 4 * (shunts.len() as i32 - 1);
    for (i, k) in shunts.into_iter().enumerate() {
        place(&mut s, k, 8 + 4 * i as i32, 0, shunt_rotation); // (x,-2) (x,2)
    }
    place(&mut s, K::Ground, 4, 2, Rotation::R0);
    wire(&mut s, &[(0, -2), (2, -2)]);
    wire(&mut s, &[(6, -2), (right, -2)]);
    wire(&mut s, &[(0, 2), (right, 2)]);
    s.add_label("VIN", Point::new(1, -2));
    s.add_label("VOUT", Point::new(right - 1, -2));
    s
}

/// 1.59 kΩ and 100 nF: corner frequency 1 kHz. Try the Bode plotter.
pub fn rc_lowpass() -> Schematic {
    ladder(
        sine(1.0, 1e3),
        (r(1.59e3), Rotation::R0),
        vec![c(100e-9)],
        Rotation::R90,
    )
}

/// 1N4148 into a smoothing capacitor and 1 kΩ load, from 10 V 50 Hz.
pub fn half_wave_rectifier() -> Schematic {
    ladder(
        sine(10.0, 50.0),
        (
            K::Diode {
                model: "1N4148".into(),
            },
            Rotation::R0,
        ),
        vec![c(100e-6), r(1e3)],
        Rotation::R90,
    )
}

/// 12 V through 470 Ω to a 1N750 (4.7 V) zener and a 1 kΩ load.
pub fn zener_regulator() -> Schematic {
    let mut s = ladder(
        K::VoltageSource {
            waveform: Waveform::Dc(12.0),
        },
        (r(470.0), Rotation::R0),
        vec![
            K::Diode {
                model: "1N750".into(),
            },
            r(1e3),
        ],
        // Cathode up: the zener is reverse-biased.
        Rotation::R270,
    );
    // The load resistor reads better upright too.
    if let Some(load) = s.components.iter_mut().find(|c| c.designator == "R2") {
        load.rotation = Rotation::R90;
    }
    s
}

/// A complementary MOSFET pair. Use Instruments → DC transfer on VIN.
pub fn cmos_inverter() -> Schematic {
    let mut s = Schematic::new();
    place(
        &mut s,
        K::VoltageSource {
            waveform: Waveform::Dc(5.0),
        },
        -8,
        0,
        Rotation::R0,
    ); // (-8,-2) (-8,2)
    place(
        &mut s,
        K::VoltageSource {
            waveform: Waveform::Dc(2.5),
        },
        -4,
        1,
        Rotation::R0,
    ); // (-4,-1) (-4,3)
    // P-channel flipped vertically so its source is at the top:
    // S (10,-6), D (10,-2), G (6,-4).
    let mp = place(
        &mut s,
        K::Mosfet {
            polarity: Polarity::P,
            model: GENERIC_PMOS.into(),
        },
        8,
        -4,
        Rotation::R180,
    );
    s.component_mut(mp).expect("just added").mirrored = true;
    // N-channel: D (10,0), S (10,4), G (6,2).
    place(
        &mut s,
        K::Mosfet {
            polarity: Polarity::N,
            model: GENERIC_NMOS.into(),
        },
        8,
        2,
        Rotation::R0,
    );
    place(&mut s, K::Ground, 2, 6, Rotation::R0);
    wire(&mut s, &[(-8, -2), (-8, -8), (10, -8), (10, -6)]);
    wire(&mut s, &[(10, -2), (10, 0)]);
    wire(&mut s, &[(10, -1), (13, -1)]);
    wire(&mut s, &[(6, -4), (6, 2)]);
    wire(&mut s, &[(-4, -1), (6, -1)]);
    wire(&mut s, &[(10, 4), (10, 6)]);
    wire(&mut s, &[(-8, 2), (-8, 6), (10, 6)]);
    wire(&mut s, &[(-4, 3), (-4, 6)]);
    s.add_label("VIN", Point::new(-2, -1));
    s.add_label("VOUT", Point::new(13, -1));
    s
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
        model: "2N3904".into(),
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
                amplitude: 1e-3,
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
                "Common-emitter amplifier" => assert!(vout > 3.0 && vout < 10.0, "VOUT = {vout}"),
                // Sine sources are 0 V at the operating point.
                "RC low-pass filter" | "Half-wave rectifier" => {
                    assert!(vout.abs() < 1e-6, "{vout}")
                }
                "Zener regulator" => assert!(vout > 4.4 && vout < 4.9, "VOUT = {vout}"),
                "CMOS inverter" => assert!(vout > 0.0 && vout < 5.0, "VOUT = {vout}"),
                other => panic!("no expectation for {other}"),
            }
        }
    }
}
