//! The integrated-circuit macromodels, checked against their data sheets.

use dunbrec_core::{ComponentKind as K, ModelLibrary, NetlistBuilder, Waveform};

use crate::{Circuit, TransientOptions, ac_analysis, operating_point, transient};

fn dc(v: f64) -> Waveform {
    Waveform::Dc(v)
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

fn ua741() -> K {
    K::OpAmp {
        part: "uA741".into(),
    }
}

/// ±15 V supplies on nets `vp` and `vn`.
fn supplies() -> NetlistBuilder {
    NetlistBuilder::new()
        .element("VP", K::VoltageSource { waveform: dc(15.0) }, &["vp", "0"])
        .element("VN", K::VoltageSource { waveform: dc(15.0) }, &["0", "vn"])
}

/// Times at which `trace` crosses `level` going up.
fn rising_crossings(times: &[f64], trace: &[f64], level: f64) -> Vec<f64> {
    (1..trace.len())
        .filter(|&i| trace[i - 1] < level && trace[i] >= level)
        .map(|i| {
            let f = (level - trace[i - 1]) / (trace[i] - trace[i - 1]);
            times[i - 1] + f * (times[i] - times[i - 1])
        })
        .collect()
}

fn falling_crossings(times: &[f64], trace: &[f64], level: f64) -> Vec<f64> {
    let flipped: Vec<f64> = trace.iter().map(|v| -v).collect();
    rising_crossings(times, &flipped, -level)
}

#[test]
fn ua741_inverting_amplifier() {
    // Gain -10: 1 kΩ in, 10 kΩ feedback.
    let netlist = supplies()
        .element("VIN", K::VoltageSource { waveform: dc(0.5) }, &["in", "0"])
        .element("RI", r(1e3), &["in", "inv"])
        .element("RF", r(10e3), &["inv", "out"])
        .element("U1", ua741(), &["0", "inv", "vp", "vn", "out"])
        .build();
    let circuit = Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap();
    let op = operating_point(&circuit).unwrap();
    let out = op.voltage_by_name("out").unwrap();
    assert!((out + 5.0).abs() < 5e-3, "out = {out}");
    // The inverting input is a virtual ground.
    assert!(op.voltage_by_name("inv").unwrap().abs() < 1e-3);
    // Internal nets are named after the chip, and every one of them
    // (including the diodes' and transistors' own) has a DC path.
    assert!(circuit.node_names.iter().any(|n| n == "U1#99"));
    assert!(circuit.node_names.iter().any(|n| n == "U1#DC.a"));
    assert!(circuit.warnings().is_empty(), "{:?}", circuit.warnings());
}

#[test]
fn series_resistances_are_dc_paths() {
    // Internal nodes behind RS (diode), RB/RC/RE (BJT) and RD/RS (MOSFET)
    // are not floating.
    use dunbrec_core::{DeviceModel, ModelKind, Polarity};
    let mut models = ModelLibrary::new();
    models.insert(DeviceModel::new("D", ModelKind::Diode).with("RS", 1.0));
    models.insert(
        DeviceModel::new("Q", ModelKind::Npn)
            .with("RB", 10.0)
            .with("RC", 1.0)
            .with("RE", 0.5),
    );
    models.insert(
        DeviceModel::new("M", ModelKind::Nmos)
            .with("RD", 1.0)
            .with("RS", 1.0),
    );
    let netlist = NetlistBuilder::new()
        .element("V1", K::VoltageSource { waveform: dc(5.0) }, &["a", "0"])
        .element("R1", r(1e3), &["a", "k"])
        .element("D1", K::Diode { model: "D".into() }, &["k", "0"])
        .element(
            "Q1",
            K::Bjt {
                polarity: Polarity::N,
                model: "Q".into(),
            },
            &["a", "k", "0"],
        )
        .element(
            "M1",
            K::Mosfet {
                polarity: Polarity::N,
                model: "M".into(),
            },
            &["a", "a", "0"],
        )
        .build();
    let circuit = Circuit::from_netlist(&netlist, &models).unwrap();
    assert!(circuit.node_names.len() > 3, "internal nodes exist");
    assert!(circuit.warnings().is_empty(), "{:?}", circuit.warnings());
}

#[test]
fn ua741_output_swing() {
    // Open loop, overdriven both ways: the output stops short of the rails
    // (data sheet: ±14 V typical into 10 kΩ at ±15 V).
    for (vin, sign) in [(1.0, 1.0), (-1.0, -1.0)] {
        let netlist = supplies()
            .element("VIN", K::VoltageSource { waveform: dc(vin) }, &["in", "0"])
            .element("U1", ua741(), &["in", "0", "vp", "vn", "out"])
            .element("RL", r(10e3), &["out", "0"])
            .build();
        let circuit = Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap();
        let out = operating_point(&circuit)
            .unwrap()
            .voltage_by_name("out")
            .unwrap();
        assert!(
            (12.5..14.8).contains(&(sign * out)),
            "vin {vin}: out = {out}"
        );
    }
}

#[test]
fn ua741_open_loop_gain_and_bandwidth() {
    // DC feedback through 1 MΩ keeps the output near 0 V (the input bias
    // current, ~80 nA, only offsets it ~80 mV); a huge
    // capacitor makes the loop open for AC above a fraction of a hertz.
    let netlist = supplies()
        .element("VIN", K::VoltageSource { waveform: dc(0.0) }, &["in", "0"])
        .element("U1", ua741(), &["in", "inv", "vp", "vn", "out"])
        .element("RF", r(1e6), &["out", "inv"])
        .element("CF", c(1.0), &["inv", "0"])
        .build();
    let freqs = [10.0, 100.0, 1e3, 1e4, 1e5, 3e5, 1e6, 3e6];
    let res = ac_analysis(&netlist, &ModelLibrary::new(), "VIN", &freqs).unwrap();
    let out = res.node("out").unwrap();
    let gain: Vec<f64> = (0..freqs.len())
        .map(|i| res.voltages[i][out.0].norm())
        .collect();
    // Data sheet: 200 000 (106 dB) typical; the pole is near 5 Hz, so at
    // 10 Hz the gain is already about half.
    assert!(
        (50e3..200e3).contains(&gain[0]),
        "gain at 10 Hz: {}",
        gain[0]
    );
    // -20 dB/decade above the pole: gain x frequency stays near the
    // unity-gain bandwidth, 1 MHz typical.
    for (f, g) in freqs.iter().zip(&gain).skip(1).take(4) {
        let gbw = f * g;
        assert!((0.7e6..1.5e6).contains(&gbw), "GBW at {f} Hz: {gbw}");
    }
    // Unity gain between 300 kHz and 3 MHz.
    assert!(gain[5] > 1.0 && gain[7] < 1.0, "{gain:?}");
}

#[test]
fn ua741_slew_rate() {
    // Voltage follower driven by a fast 10 V step: the output ramps at the
    // slew rate, 0.5 V/µs typical.
    let step = Waveform::Pulse {
        v1: -5.0,
        v2: 5.0,
        delay: 5e-6,
        rise: 1e-9,
        fall: 1e-9,
        width: 1.0,
        period: 0.0,
    };
    let netlist = supplies()
        .element("VIN", K::VoltageSource { waveform: step }, &["in", "0"])
        .element("U1", ua741(), &["in", "out", "vp", "vn", "out"])
        .element("RL", r(10e3), &["out", "0"])
        .build();
    let circuit = Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap();
    let res = transient(circuit, TransientOptions::new(60e-6).with_max_step(0.2e-6)).unwrap();
    let out = res.voltage("out").unwrap();
    let t10 = rising_crossings(&res.times, out, -3.0)[0];
    let t90 = rising_crossings(&res.times, out, 3.0)[0];
    let slew = 6.0 / (t90 - t10);
    assert!((0.35e6..0.7e6).contains(&slew), "slew rate {slew} V/s");
    // And it settles at the input.
    assert!((out[out.len() - 1] - 5.0).abs() < 0.01);
}

#[test]
fn unknown_opamp_part_is_reported() {
    let netlist = supplies()
        .element(
            "U1",
            K::OpAmp {
                part: "XYZ123".into(),
            },
            &["0", "0", "vp", "vn", "out"],
        )
        .build();
    let err = Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap_err();
    assert!(err.to_string().contains("XYZ123"), "{err}");
}

/// Add a 555 whose pins, in package order (GND, TRIG, OUT, RESET, CTRL, THR,
/// DIS, VCC), connect to `nodes`.
fn add_555(b: NetlistBuilder, nodes: [&str; 8]) -> NetlistBuilder {
    b.element("U1", K::Timer555, &nodes)
}

#[test]
fn ne555_astable_frequency_and_duty() {
    // RA = 1 kΩ, RB = 10 kΩ, C = 100 nF at 9 V:
    // f = 1 / (ln 2 (RA + 2 RB) C) = 687 Hz, high for (RA + RB)/(RA + 2 RB).
    let (ra, rb, cap) = (1e3, 10e3, 100e-9);
    let b = NetlistBuilder::new()
        .element("VCC", K::VoltageSource { waveform: dc(9.0) }, &["vcc", "0"])
        .element("RA", r(ra), &["vcc", "dis"])
        .element("RB", r(rb), &["dis", "timing"])
        .element(
            "CT",
            K::Capacitor {
                capacitance: cap,
                initial_voltage: Some(0.0),
            },
            &["timing", "0"],
        )
        .element("CC", c(10e-9), &["ctrl", "0"])
        .element("RL", r(10e3), &["out", "0"]);
    let netlist = add_555(
        b,
        ["0", "timing", "out", "vcc", "ctrl", "timing", "dis", "vcc"],
    )
    .build();
    let circuit = Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap();
    let res = transient(circuit, TransientOptions::new(10e-3).with_max_step(20e-6)).unwrap();
    let out = res.voltage("out").unwrap();
    let rises = rising_crossings(&res.times, out, 4.5);
    let falls = falling_crossings(&res.times, out, 4.5);
    assert!(rises.len() >= 5, "rises {rises:?}");

    // Skip the first, longer cycle (the capacitor starts from 0 V).
    let periods: Vec<f64> = rises.windows(2).skip(1).map(|w| w[1] - w[0]).collect();
    let expected = std::f64::consts::LN_2 * (ra + 2.0 * rb) * cap;
    for p in &periods {
        assert!(
            (p / expected - 1.0).abs() < 0.02,
            "period {p} vs {expected}"
        );
    }
    let rise = rises[2];
    let fall = *falls.iter().find(|&&t| t > rise).unwrap();
    let duty = (fall - rise) / periods[1];
    let expected_duty = (ra + rb) / (ra + 2.0 * rb);
    assert!((duty - expected_duty).abs() < 0.02, "duty {duty}");

    // Output levels: about VCC - 1.4 V high, near 0 V low.
    let high = out.iter().cloned().fold(f64::MIN, f64::max);
    let low = out
        .iter()
        .zip(&res.times)
        .filter(|(_, t)| **t > 2e-3)
        .map(|(v, _)| *v)
        .fold(f64::MAX, f64::min);
    assert!((7.3..7.8).contains(&high), "high {high}");
    assert!(low.abs() < 0.1, "low {low}");

    // The timing capacitor swings between 1/3 and 2/3 of VCC.
    let timing = res.voltage("timing").unwrap();
    let late: Vec<f64> = timing
        .iter()
        .zip(&res.times)
        .filter(|(_, t)| **t > 3e-3)
        .map(|(v, _)| *v)
        .collect();
    let (lo, hi) = late
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    assert!(
        (lo - 3.0).abs() < 0.05 && (hi - 6.0).abs() < 0.05,
        "{lo}..{hi}"
    );
}

#[test]
fn ne555_monostable_pulse_width() {
    // R = 10 kΩ, C = 100 nF: one 1.1 RC = 1.1 ms pulse per trigger.
    let trigger = Waveform::Pulse {
        v1: 5.0,
        v2: 0.0,
        delay: 1e-3,
        rise: 1e-6,
        fall: 1e-6,
        width: 20e-6,
        period: 0.0,
    };
    let b = NetlistBuilder::new()
        .element("VCC", K::VoltageSource { waveform: dc(5.0) }, &["vcc", "0"])
        .element("VT", K::VoltageSource { waveform: trigger }, &["trig", "0"])
        .element("R", r(10e3), &["vcc", "timing"])
        .element("CT", c(100e-9), &["timing", "0"])
        .element("CC", c(10e-9), &["ctrl", "0"])
        .element("RL", r(10e3), &["out", "0"]);
    let netlist = add_555(
        b,
        ["0", "trig", "out", "vcc", "ctrl", "timing", "timing", "vcc"],
    )
    .build();
    let circuit = Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap();
    let res = transient(circuit, TransientOptions::new(4e-3).with_max_step(10e-6)).unwrap();
    let out = res.voltage("out").unwrap();
    // Quiet until triggered.
    let before = out
        .iter()
        .zip(&res.times)
        .filter(|(_, t)| **t < 0.9e-3)
        .all(|(v, _)| v.abs() < 0.1);
    assert!(before, "output high before the trigger");
    let rises = rising_crossings(&res.times, out, 2.0);
    let falls = falling_crossings(&res.times, out, 2.0);
    assert_eq!(rises.len(), 1, "{rises:?}");
    assert_eq!(falls.len(), 1, "{falls:?}");
    let width = falls[0] - rises[0];
    let expected = 1.1 * 10e3 * 100e-9;
    assert!((width / expected - 1.0).abs() < 0.03, "width {width}");
    assert!((rises[0] - 1e-3).abs() < 5e-6, "starts at {}", rises[0]);
}

#[test]
fn ne555_reset_holds_output_low() {
    let b = NetlistBuilder::new()
        .element("VCC", K::VoltageSource { waveform: dc(9.0) }, &["vcc", "0"])
        .element("RA", r(1e3), &["vcc", "dis"])
        .element("RB", r(10e3), &["dis", "timing"])
        .element("CT", c(100e-9), &["timing", "0"])
        .element("RL", r(10e3), &["out", "0"]);
    let netlist = add_555(
        b,
        ["0", "timing", "out", "0", "ctrl", "timing", "dis", "vcc"],
    )
    .build();
    let circuit = Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap();
    let res = transient(circuit, TransientOptions::new(5e-3)).unwrap();
    let out = res.voltage("out").unwrap();
    assert!(
        out.iter().all(|v| v.abs() < 0.1),
        "output moved while reset"
    );
}

// ---------------------------------------------------------- CD4066B ---

/// A CD4066B on a `vdd` supply (VSS at ground), with 5 V
/// applied to one signal pin of switch A through 1 Ω, a 10 kΩ load on
/// the other, and control A at `ctrl_a`. Switch B is fed 5 V and loaded
/// the same way, with control B at `ctrl_b`. Returns the settled voltages
/// at the loads of A and B.
fn cd4066_run(vdd: f64, ctrl_a: f64, ctrl_b: f64, r_on: f64, reversed: bool) -> (f64, f64) {
    // Pins: A1 A2 B2 B1 CB CC VSS C1 C2 D2 D1 CD CA VDD.
    let (a_in, a_out) = if reversed { ("a2", "a1") } else { ("a1", "a2") };
    let netlist = NetlistBuilder::new()
        .element("VDD", K::VoltageSource { waveform: dc(vdd) }, &["vdd", "0"])
        .element("VIN", K::VoltageSource { waveform: dc(5.0) }, &["sig", "0"])
        .element(
            "VCA",
            K::VoltageSource {
                waveform: dc(ctrl_a),
            },
            &["ca", "0"],
        )
        .element(
            "VCB",
            K::VoltageSource {
                waveform: dc(ctrl_b),
            },
            &["cb", "0"],
        )
        .element("RA", r(1.0), &["sig", a_in])
        .element("RB", r(1.0), &["sig", "b1"])
        .element("RLA", r(10e3), &[a_out, "0"])
        .element("RLB", r(10e3), &["b2", "0"])
        .element(
            "U1",
            K::Cd4066 { r_on },
            &[
                "a1", "a2", "b2", "b1", "cb", "0", "0", "c1", "c2", "d2", "d1", "0", "ca", "vdd",
            ],
        )
        .element("RC1", r(1e6), &["c1", "0"])
        .element("RC2", r(1e6), &["c2", "0"])
        .element("RD1", r(1e6), &["d1", "0"])
        .element("RD2", r(1e6), &["d2", "0"])
        .build();
    let circuit = Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap();
    let res = transient(circuit, TransientOptions::new(500e-9)).unwrap();
    let last = |node: &str| *res.voltage(node).unwrap().last().unwrap();
    (last(a_out), last("b2"))
}

#[test]
fn cd4066_passes_the_signal_when_its_control_is_high() {
    let (a_on, _) = cd4066_run(10.0, 10.0, 0.0, 180.0, false);
    // 5 V through 180 Ω + 1 Ω into 10 kΩ.
    let expected = 5.0 * 10e3 / (10e3 + 181.0);
    assert!(
        (a_on - expected).abs() < 5e-3,
        "on: {a_on} V, wanted {expected}"
    );
    let (a_off, _) = cd4066_run(10.0, 0.0, 0.0, 180.0, false);
    assert!(a_off.abs() < 1e-3, "off: {a_off} V");
}

#[test]
fn cd4066_switches_work_both_ways_and_independently() {
    // Signal in at A2 and out at A1 passes as well.
    let (a, _) = cd4066_run(10.0, 10.0, 0.0, 180.0, true);
    assert!(a > 4.8, "reversed: {a} V");
    // Switch A on, B off, and the other way round.
    let (a, b) = cd4066_run(10.0, 10.0, 0.0, 180.0, false);
    assert!(a > 4.8 && b.abs() < 1e-3, "A on, B off: {a} V, {b} V");
    let (a, b) = cd4066_run(10.0, 0.0, 10.0, 180.0, false);
    assert!(a.abs() < 1e-3 && b > 4.8, "A off, B on: {a} V, {b} V");
}

#[test]
fn cd4066_control_threshold_is_half_the_supply() {
    for (vdd, below, above) in [(5.0, 2.0, 3.0), (10.0, 4.0, 6.0), (15.0, 7.0, 8.0)] {
        let (low, _) = cd4066_run(vdd, below, 0.0, 180.0, false);
        let (high, _) = cd4066_run(vdd, above, 0.0, 180.0, false);
        assert!(
            low < 0.1,
            "VDD {vdd}: control at {below} V should be off, got {low} V"
        );
        assert!(
            high > 4.8,
            "VDD {vdd}: control at {above} V should be on, got {high} V"
        );
    }
}

#[test]
fn cd4066_on_resistance_is_a_property() {
    // 5 V into the 10 kΩ load through 1 kΩ of switch.
    let (v, _) = cd4066_run(10.0, 10.0, 0.0, 1e3, false);
    let expected = 5.0 * 10e3 / (10e3 + 1e3 + 1.0);
    assert!((v - expected).abs() < 5e-3, "{v} V, wanted {expected}");
}

#[test]
fn cd4066_follows_a_control_signal_while_running() {
    // A 1 kHz square wave on control A gates a DC level into the load.
    let netlist = NetlistBuilder::new()
        .element(
            "VDD",
            K::VoltageSource { waveform: dc(10.0) },
            &["vdd", "0"],
        )
        .element("VIN", K::VoltageSource { waveform: dc(5.0) }, &["a1", "0"])
        .element(
            "VCA",
            K::VoltageSource {
                waveform: Waveform::Pulse {
                    v1: 0.0,
                    v2: 10.0,
                    delay: 0.0,
                    rise: 1e-7,
                    fall: 1e-7,
                    width: 0.5e-3,
                    period: 1e-3,
                },
            },
            &["ca", "0"],
        )
        .element("RL", r(10e3), &["a2", "0"])
        .element(
            "U1",
            K::Cd4066 { r_on: 180.0 },
            &[
                "a1", "a2", "b2", "b1", "0", "0", "0", "c1", "c2", "d2", "d1", "0", "ca", "vdd",
            ],
        )
        .element("RB1", r(1e6), &["b1", "0"])
        .element("RB2", r(1e6), &["b2", "0"])
        .element("RC1", r(1e6), &["c1", "0"])
        .element("RC2", r(1e6), &["c2", "0"])
        .element("RD1", r(1e6), &["d1", "0"])
        .element("RD2", r(1e6), &["d2", "0"])
        .build();
    let circuit = Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap();
    let res = transient(circuit, TransientOptions::new(3e-3)).unwrap();
    let out = res.voltage("a2").unwrap();
    // High for the first half-millisecond of each period, low for the rest.
    for k in 0..3 {
        let t = k as f64 * 1e-3;
        let on = res.at(out, t + 0.25e-3);
        let off = res.at(out, t + 0.75e-3);
        assert!(on > 4.8, "period {k}: on at {on} V");
        assert!(off < 0.2, "period {k}: off at {off} V");
    }
}
