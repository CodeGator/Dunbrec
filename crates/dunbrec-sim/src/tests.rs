//! DC operating point tests against hand-calculated results.

use approx::assert_relative_eq;
use dunbrec_core::{
    ComponentKind as K, ModelLibrary, NetlistBuilder, Point, Schematic, Waveform, build_netlist,
};

use crate::{Circuit, OperatingPoint, SimError, SimWarning, operating_point};

fn r(ohms: f64) -> K {
    K::Resistor { resistance: ohms }
}
fn v(volts: f64) -> K {
    K::VoltageSource {
        waveform: Waveform::Dc(volts),
    }
}
fn i(amps: f64) -> K {
    K::CurrentSource {
        waveform: Waveform::Dc(amps),
    }
}

fn solve(b: NetlistBuilder) -> Result<OperatingPoint, SimError> {
    let circuit = Circuit::from_netlist(&b.build(), &ModelLibrary::new())?;
    operating_point(&circuit)
}

fn volts(op: &OperatingPoint, node: &str) -> f64 {
    op.voltage_by_name(node)
        .unwrap_or_else(|| panic!("no node {node}"))
}

const EPS: f64 = 1e-9;

#[test]
fn voltage_divider() {
    let op = solve(
        NetlistBuilder::new()
            .element("V1", v(10.0), &["in", "0"])
            .element("R1", r(1e3), &["in", "out"])
            .element("R2", r(1e3), &["out", "0"]),
    )
    .unwrap();
    assert_relative_eq!(volts(&op, "in"), 10.0, epsilon = EPS);
    assert_relative_eq!(volts(&op, "out"), 5.0, epsilon = EPS);
    assert_relative_eq!(op.current("R1").unwrap(), 5e-3, epsilon = EPS);
    // Source delivers power: current into + is negative.
    assert_relative_eq!(op.current("V1").unwrap(), -5e-3, epsilon = EPS);
    assert!(op.warnings.is_empty());
}

#[test]
fn current_source_drives_out_pin() {
    // 1 mA flows out of I1's "out" pin into node a, then through 1 kΩ.
    let op = solve(
        NetlistBuilder::new()
            .element("I1", i(1e-3), &["0", "a"])
            .element("R1", r(1e3), &["a", "0"]),
    )
    .unwrap();
    assert_relative_eq!(volts(&op, "a"), 1.0, epsilon = EPS);
    assert_relative_eq!(op.current("I1").unwrap(), 1e-3, epsilon = EPS);
}

#[test]
fn unbalanced_wheatstone_bridge() {
    // Nodal equations: 5Va - 2Vb = 20, -3Va + 7Vb = 10
    // => Va = 160/29, Vb = 110/29.
    let op = solve(
        NetlistBuilder::new()
            .element("V1", v(10.0), &["top", "0"])
            .element("R1", r(1e3), &["top", "a"])
            .element("R2", r(2e3), &["a", "0"])
            .element("R3", r(3e3), &["top", "b"])
            .element("R4", r(1e3), &["b", "0"])
            .element("R5", r(1e3), &["a", "b"]),
    )
    .unwrap();
    assert_relative_eq!(volts(&op, "a"), 160.0 / 29.0, epsilon = EPS);
    assert_relative_eq!(volts(&op, "b"), 110.0 / 29.0, epsilon = EPS);
    assert_relative_eq!(op.current("R5").unwrap(), 50.0 / 29.0 / 1e3, epsilon = EPS);
}

#[test]
fn superposition_of_voltage_and_current_sources() {
    // (Vm - 10)/1k + Vm/1k = 1 mA  =>  Vm = 5.5 V
    let op = solve(
        NetlistBuilder::new()
            .element("V1", v(10.0), &["a", "0"])
            .element("R1", r(1e3), &["a", "m"])
            .element("I1", i(1e-3), &["0", "m"])
            .element("R2", r(1e3), &["m", "0"]),
    )
    .unwrap();
    assert_relative_eq!(volts(&op, "m"), 5.5, epsilon = EPS);
}

#[test]
fn vcvs_amplifier() {
    let op = solve(
        NetlistBuilder::new()
            .element("V1", v(1.0), &["in", "0"])
            .element("RIN", r(1e3), &["in", "0"])
            .element("E1", K::Vcvs { gain: 10.0 }, &["out", "0", "in", "0"])
            .element("RL", r(1e3), &["out", "0"]),
    )
    .unwrap();
    assert_relative_eq!(volts(&op, "out"), 10.0, epsilon = EPS);
    assert_relative_eq!(op.current("E1").unwrap(), -10e-3, epsilon = EPS);
}

#[test]
fn vccs_follows_spice_sign() {
    // 2 mA flows from x through G1 to ground, so Vx = -2 V across 1 kΩ.
    let op = solve(
        NetlistBuilder::new()
            .element("V1", v(2.0), &["in", "0"])
            .element(
                "G1",
                K::Vccs {
                    transconductance: 1e-3,
                },
                &["x", "0", "in", "0"],
            )
            .element("RL", r(1e3), &["x", "0"]),
    )
    .unwrap();
    assert_relative_eq!(volts(&op, "x"), -2.0, epsilon = EPS);
}

#[test]
fn cccs_and_ccvs() {
    // Sensor carries 1 mA from "in" to "s".
    let base = || {
        NetlistBuilder::new()
            .element("V1", v(1.0), &["in", "0"])
            .element("R1", r(1e3), &["s", "0"])
            .element("RL", r(1e3), &["x", "0"])
    };
    let op = solve(base().element("F1", K::Cccs { gain: 2.0 }, &["x", "0", "in", "s"])).unwrap();
    assert_relative_eq!(volts(&op, "s"), 1.0, epsilon = EPS);
    assert_relative_eq!(volts(&op, "x"), -2.0, epsilon = EPS);

    let op = solve(base().element(
        "H1",
        K::Ccvs {
            transresistance: 1e3,
        },
        &["x", "0", "in", "s"],
    ))
    .unwrap();
    assert_relative_eq!(volts(&op, "s"), 1.0, epsilon = EPS);
    assert_relative_eq!(volts(&op, "x"), 1.0, epsilon = EPS);
}

#[test]
fn inductor_shorts_and_capacitor_opens_in_dc() {
    let op = solve(
        NetlistBuilder::new()
            .element("V1", v(5.0), &["a", "0"])
            .element(
                "L1",
                K::Inductor {
                    inductance: 1e-3,
                    initial_current: None,
                },
                &["a", "b"],
            )
            .element("R1", r(1e3), &["b", "0"])
            .element(
                "C1",
                K::Capacitor {
                    capacitance: 1e-6,
                    initial_voltage: None,
                },
                &["b", "0"],
            ),
    )
    .unwrap();
    assert_relative_eq!(volts(&op, "b"), 5.0, epsilon = EPS);
    assert_relative_eq!(op.current("L1").unwrap(), 5e-3, epsilon = EPS);
    assert_eq!(op.current("C1"), Some(0.0));
}

#[test]
fn potentiometer_and_switch() {
    let op = solve(
        NetlistBuilder::new()
            .element("V1", v(10.0), &["top", "0"])
            .element(
                "RV1",
                K::Potentiometer {
                    resistance: 10e3,
                    wiper: 0.25,
                },
                &["top", "0", "w"],
            )
            .element(
                "S1",
                K::Switch {
                    closed: true,
                    r_on: 1e-3,
                    r_off: 1e9,
                },
                &["w", "sw"],
            )
            .element(
                "S2",
                K::Switch {
                    closed: false,
                    r_on: 1e-3,
                    r_off: 1e9,
                },
                &["w", "open"],
            )
            .element("RL", r(1e9), &["sw", "0"])
            .element("RL2", r(1e3), &["open", "0"]),
    )
    .unwrap();
    // Wiper 25% of the way from A (10 V) to B (0 V); loads are negligible.
    assert_relative_eq!(volts(&op, "w"), 7.5, epsilon = 1e-4);
    assert_relative_eq!(volts(&op, "sw"), 7.5, epsilon = 1e-4);
    assert!(volts(&op, "open") < 1e-4);
}

#[test]
fn potentiometer_wiper_at_end_is_finite() {
    let op = solve(
        NetlistBuilder::new()
            .element("V1", v(10.0), &["top", "0"])
            .element(
                "RV1",
                K::Potentiometer {
                    resistance: 10e3,
                    wiper: 0.0,
                },
                &["top", "0", "w"],
            ),
    )
    .unwrap();
    assert_relative_eq!(volts(&op, "w"), 10.0, epsilon = 1e-5);
}

#[test]
fn series_capacitors_give_floating_warning() {
    let op = solve(
        NetlistBuilder::new()
            .element("V1", v(5.0), &["a", "0"])
            .element("R1", r(1e3), &["a", "0"])
            .element(
                "C1",
                K::Capacitor {
                    capacitance: 1e-6,
                    initial_voltage: None,
                },
                &["a", "m"],
            )
            .element(
                "C2",
                K::Capacitor {
                    capacitance: 1e-6,
                    initial_voltage: None,
                },
                &["m", "0"],
            ),
    )
    .unwrap();
    assert_eq!(
        op.warnings,
        vec![SimWarning::FloatingNodes {
            nodes: vec!["m".into()]
        }]
    );
    assert!(volts(&op, "m").abs() < 1e-9);
}

#[test]
fn current_source_into_floating_node_is_an_error() {
    let err = solve(
        NetlistBuilder::new()
            .element("I1", i(1e-3), &["0", "m"])
            .element(
                "C1",
                K::Capacitor {
                    capacitance: 1e-6,
                    initial_voltage: None,
                },
                &["m", "0"],
            ),
    )
    .unwrap_err();
    assert_eq!(
        err,
        SimError::CurrentSourceNoReturn {
            designator: "I1".into(),
            nodes: vec!["m".into()]
        }
    );
}

#[test]
fn parallel_voltage_sources_are_a_loop() {
    let err = solve(
        NetlistBuilder::new()
            .element("V1", v(5.0), &["a", "0"])
            .element("V2", v(3.0), &["a", "0"])
            .element("R1", r(1e3), &["a", "0"]),
    )
    .unwrap_err();
    assert_eq!(
        err,
        SimError::VoltageLoop {
            designator: "V2".into()
        }
    );
}

#[test]
fn invalid_values_and_missing_models() {
    let err = solve(NetlistBuilder::new().element("R1", r(0.0), &["a", "0"])).unwrap_err();
    assert!(matches!(err, SimError::InvalidParameter { designator, .. } if designator == "R1"));

    let err =
        solve(NetlistBuilder::new().element("D1", K::Diode { model: "D".into() }, &["a", "0"]))
            .unwrap_err();
    assert!(matches!(err, SimError::InvalidParameter { designator, .. } if designator == "D1"));
}

#[test]
fn netlist_errors_block_simulation() {
    let mut s = Schematic::new();
    s.add_component(r(1e3), Point::new(0, 0));
    let err = Circuit::from_netlist(&build_netlist(&s), &s.models).unwrap_err();
    assert!(matches!(err, SimError::Netlist(_)));
}

#[test]
fn r2r_ladder_dac() {
    // 8-bit R-2R ladder with only the MSB driven to 10 V: output = 5 V.
    // With all bits driven, output = 10 * 255/256.
    let ladder = |bits: [bool; 8]| {
        let mut b = NetlistBuilder::new().element("VREF", v(10.0), &["ref", "0"]);
        // Node jk is the ladder junction for bit k (0 = LSB). The LSB end is
        // terminated with 2R to ground; the output is j7.
        b.push("RT", r(2e3), &["j0", "0"]);
        for (k, &bit) in bits.iter().enumerate() {
            let j = format!("j{k}");
            let src = if bit { "ref" } else { "0" };
            b.push(&format!("RB{k}"), r(2e3), &[&j, src]);
            if k < 7 {
                b.push(&format!("RS{k}"), r(1e3), &[&j, &format!("j{}", k + 1)]);
            }
        }
        solve(b).map(|op| volts(&op, "j7")).unwrap()
    };
    let mut msb = [false; 8];
    msb[7] = true;
    assert_relative_eq!(ladder(msb), 5.0, epsilon = EPS);
    assert_relative_eq!(ladder([true; 8]), 10.0 * 255.0 / 256.0, epsilon = EPS);
    let mut lsb = [false; 8];
    lsb[0] = true;
    assert_relative_eq!(ladder(lsb), 10.0 / 256.0, epsilon = EPS);
}

#[test]
fn drawn_schematic_end_to_end() {
    // Same divider as the netlister test, drawn on the grid.
    let mut s = Schematic::new();
    s.add_component(v(10.0), Point::new(0, 0));
    let r1 = s.add_component(r(1e3), Point::new(4, -2));
    s.add_component(r(1e3), Point::new(8, -2));
    s.add_component(K::Ground, Point::new(5, 2));
    s.add_wire(Point::new(0, -2), Point::new(2, -2));
    s.add_wire(Point::new(10, -2), Point::new(10, 2));
    s.add_wire(Point::new(0, 2), Point::new(10, 2));
    s.add_label("VOUT", Point::new(6, -2));

    let netlist = build_netlist(&s);
    let circuit = Circuit::from_netlist(&netlist, &s.models).unwrap();
    let op = operating_point(&circuit).unwrap();
    assert_relative_eq!(volts(&op, "VOUT"), 5.0, epsilon = EPS);
    let r1_out = netlist
        .elements
        .iter()
        .find(|e| e.component == r1)
        .unwrap()
        .nodes[1];
    assert_relative_eq!(op.voltage(r1_out), 5.0, epsilon = EPS);
}
