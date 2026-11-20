//! AC analysis and DC sweep tests.

use std::f64::consts::TAU;

use approx::assert_relative_eq;
use dunbrec_core::{
    ComponentKind as K, DeviceModel, ModelKind, ModelLibrary, NetlistBuilder, Polarity, Waveform,
};

use crate::semiconductor::VT;
use crate::{
    Circuit, DcSweeper, SimError, TransientOptions, ac_analysis, dc_sweep, linear_steps,
    log_frequencies, operating_point, transient,
};

fn r(ohms: f64) -> K {
    K::Resistor { resistance: ohms }
}
fn c(farads: f64) -> K {
    K::Capacitor {
        capacitance: farads,
        initial_voltage: None,
    }
}
fn v(volts: f64) -> K {
    K::VoltageSource {
        waveform: Waveform::Dc(volts),
    }
}

fn models() -> ModelLibrary {
    let mut lib = ModelLibrary::new();
    lib.insert(DeviceModel::new("D", ModelKind::Diode).with("IS", 1e-14));
    lib.insert(
        DeviceModel::new("QN", ModelKind::Npn)
            .with("IS", 1e-16)
            .with("BF", 100.0),
    );
    lib.insert(
        DeviceModel::new("QGPC", ModelKind::Npn)
            .with("IS", 6.734e-15)
            .with("BF", 416.4)
            .with("VAF", 74.03)
            .with("IKF", 0.06678)
            .with("CJE", 4.493e-12)
            .with("CJC", 3.638e-12)
            .with("TF", 301.2e-12),
    );
    lib.insert(
        DeviceModel::new("CN", ModelKind::Nmos)
            .with("VTO", 1.0)
            .with("KP", 1e-3)
            .with("LAMBDA", 0.01),
    );
    lib.insert(
        DeviceModel::new("CP", ModelKind::Pmos)
            .with("VTO", -1.0)
            .with("KP", 1e-3)
            .with("LAMBDA", 0.01),
    );
    lib
}

#[test]
fn rc_lowpass_matches_formula() {
    let netlist = NetlistBuilder::new()
        .element("V1", v(0.0), &["in", "0"])
        .element("R1", r(1e3), &["in", "out"])
        .element("C1", c(1e-6), &["out", "0"])
        .build();
    let freqs = log_frequencies(1.0, 1e6, 10);
    let res = ac_analysis(&netlist, &ModelLibrary::new(), "V1", &freqs).unwrap();
    let out = res.response(res.node("out").unwrap());
    for (f, h) in freqs.iter().zip(&out) {
        let wrc = TAU * f * 1e-3;
        assert_relative_eq!(
            h.norm(),
            1.0 / (1.0 + wrc * wrc).sqrt(),
            max_relative = 1e-9
        );
        assert_relative_eq!(h.arg(), -wrc.atan(), epsilon = 1e-9);
    }
    // Source node is exactly the unit excitation.
    let input = res.response(res.node("in").unwrap());
    assert!(
        input
            .iter()
            .all(|h| (h.re - 1.0).abs() < 1e-12 && h.im.abs() < 1e-12)
    );
}

#[test]
fn rlc_bandpass_peaks_at_resonance() {
    // Series L-C into a resistor: |H| = 1 at f0 = 1/(2π√LC).
    let netlist = NetlistBuilder::new()
        .element("V1", v(0.0), &["in", "0"])
        .element(
            "L1",
            K::Inductor {
                inductance: 1e-3,
                initial_current: None,
            },
            &["in", "a"],
        )
        .element("C1", c(1e-6), &["a", "out"])
        .element("R1", r(100.0), &["out", "0"])
        .build();
    let f0 = 1.0 / (TAU * (1e-3f64 * 1e-6).sqrt());
    let freqs = [f0 / 10.0, f0, f0 * 10.0];
    let res = ac_analysis(&netlist, &ModelLibrary::new(), "V1", &freqs).unwrap();
    let out = res.response(res.node("out").unwrap());
    assert_relative_eq!(out[1].norm(), 1.0, max_relative = 1e-9);
    assert!(out[1].arg().abs() < 1e-9);
    // Everywhere: H = R / (R + j(ωL − 1/ωC)).
    for (f, h) in freqs.iter().zip(&out) {
        let w = TAU * f;
        let x = w * 1e-3 - 1.0 / (w * 1e-6);
        let expected =
            num_complex::Complex64::new(100.0, 0.0) / num_complex::Complex64::new(100.0, x);
        assert_relative_eq!(h.re, expected.re, epsilon = 1e-9);
        assert_relative_eq!(h.im, expected.im, epsilon = 1e-9);
    }
}

#[test]
fn diode_small_signal_resistance() {
    let netlist = NetlistBuilder::new()
        .element("V1", v(5.0), &["in", "0"])
        .element("R1", r(1e3), &["in", "a"])
        .element("D1", K::Diode { model: "D".into() }, &["a", "0"])
        .build();
    let lib = models();
    let op = operating_point(&Circuit::from_netlist(&netlist, &lib).unwrap()).unwrap();
    let id = op.current("D1").unwrap();
    let rd = VT / (id + 1e-14); // dI/dV = (Id + IS)/Vt
    let res = ac_analysis(&netlist, &lib, "V1", &[1e3]).unwrap();
    let h = res.response(res.node("a").unwrap())[0];
    assert_relative_eq!(h.norm(), rd / (1e3 + rd), max_relative = 1e-6);
}

fn ce_amplifier(model: &str, input: Waveform) -> dunbrec_core::Netlist {
    NetlistBuilder::new()
        .element("VCC", v(12.0), &["vcc", "0"])
        .element("VIN", K::VoltageSource { waveform: input }, &["sig", "0"])
        .element("CIN", c(10e-6), &["sig", "b"])
        .element("R1", r(47e3), &["vcc", "b"])
        .element("R2", r(10e3), &["b", "0"])
        .element("RC", r(4.7e3), &["vcc", "c"])
        .element("RE", r(1e3), &["e", "0"])
        .element("CE", c(1e-3), &["e", "0"])
        .element(
            "Q1",
            K::Bjt {
                polarity: Polarity::N,
                model: model.into(),
            },
            &["c", "b", "e"],
        )
        .build()
}

#[test]
fn common_emitter_gain_is_gm_times_rc() {
    let lib = models();
    let netlist = ce_amplifier("QN", Waveform::Dc(0.0));
    let op = operating_point(&Circuit::from_netlist(&netlist, &lib).unwrap()).unwrap();
    let ic = op.current("Q1").unwrap();
    // Ideal exponential, no Early effect: gm = (Ic + IS)/Vt; the emitter is
    // bypassed and the input driven directly.
    let gm = (ic + 1e-16) / VT;
    let res = ac_analysis(&netlist, &lib, "VIN", &[1e3]).unwrap();
    let h = res.response(res.node("c").unwrap())[0];
    // CE ≈ 1 mF still leaves a tiny emitter impedance at 1 kHz.
    assert_relative_eq!(h.norm(), gm * 4.7e3, max_relative = 2e-3);
    assert!(
        (h.arg().abs() - std::f64::consts::PI).abs() < 0.02,
        "inverting: {}",
        h.arg()
    );
}

#[test]
fn ac_gain_agrees_with_small_signal_transient() {
    // The same 2N3904-like stage: AC gain at 1 kHz vs the gain measured in
    // a transient run with a 100 µV input (small enough to stay linear).
    let lib = models();
    let ac_net = ce_amplifier("QGPC", Waveform::Dc(0.0));
    let ac = ac_analysis(&ac_net, &lib, "VIN", &[1e3]).unwrap();
    let ac_gain = ac.response(ac.node("c").unwrap())[0].norm();

    let tran_net = ce_amplifier(
        "QGPC",
        Waveform::Sine {
            offset: 0.0,
            amplitude: 100e-6,
            frequency: 1e3,
            delay: 0.0,
            damping: 0.0,
            phase: 0.0,
        },
    );
    let res = transient(
        Circuit::from_netlist(&tran_net, &lib).unwrap(),
        TransientOptions::new(10e-3),
    )
    .unwrap();
    let c_trace = res.voltage("c").unwrap();
    let late: Vec<f64> = res
        .times
        .iter()
        .zip(c_trace)
        .filter(|(t, _)| **t > 8e-3)
        .map(|(_, v)| *v)
        .collect();
    let swing = late.iter().copied().fold(f64::MIN, f64::max)
        - late.iter().copied().fold(f64::MAX, f64::min);
    let tran_gain = swing / 2.0 / 100e-6;
    assert_relative_eq!(tran_gain, ac_gain, max_relative = 0.02);
}

#[test]
fn junction_capacitance_gives_high_frequency_rolloff() {
    let lib = models();
    let netlist = ce_amplifier("QGPC", Waveform::Dc(0.0));
    let res = ac_analysis(&netlist, &lib, "VIN", &[1e3, 1e9]).unwrap();
    let h = res.response(res.node("c").unwrap());
    assert!(
        h[1].norm() < 0.1 * h[0].norm(),
        "{} vs {}",
        h[1].norm(),
        h[0].norm()
    );
}

#[test]
fn ac_rejects_bad_requests() {
    let netlist = NetlistBuilder::new()
        .element("V1", v(1.0), &["a", "0"])
        .element("R1", r(1.0), &["a", "0"])
        .build();
    let lib = ModelLibrary::new();
    assert!(matches!(
        ac_analysis(&netlist, &lib, "R1", &[1.0]),
        Err(SimError::InvalidOptions(_))
    ));
    assert!(matches!(
        ac_analysis(&netlist, &lib, "V1", &[0.0]),
        Err(SimError::InvalidOptions(_))
    ));
    let f = log_frequencies(10.0, 1e5, 5);
    assert_relative_eq!(f[0], 10.0);
    assert_relative_eq!(*f.last().unwrap(), 1e5, max_relative = 1e-12);
    assert_eq!(f.len(), 21);
}

#[test]
fn diode_iv_sweep() {
    let netlist = NetlistBuilder::new()
        .element("V1", v(0.0), &["a", "0"])
        .element("D1", K::Diode { model: "D".into() }, &["a", "0"])
        .build();
    let volts = linear_steps(-1.0, 0.8, 37);
    let ops = dc_sweep(&netlist, &models(), "V1", &volts).unwrap();
    let currents: Vec<f64> = ops.iter().map(|op| op.current("D1").unwrap()).collect();
    assert!(currents.windows(2).all(|w| w[1] >= w[0]), "monotonic");
    let at_07 = ops[volts.iter().position(|&x| (x - 0.7).abs() < 1e-9).unwrap()]
        .current("D1")
        .unwrap();
    assert_relative_eq!(
        at_07,
        1e-14 * ((0.7 / VT).exp() - 1.0) + 1e-12 * 0.7,
        max_relative = 1e-9
    );
}

#[test]
fn cmos_inverter_transfer_by_sweep() {
    let netlist = NetlistBuilder::new()
        .element("VDD", v(5.0), &["vdd", "0"])
        .element("VIN", v(0.0), &["in", "0"])
        .element(
            "MP",
            K::Mosfet {
                polarity: Polarity::P,
                model: "CP".into(),
            },
            &["out", "in", "vdd"],
        )
        .element(
            "MN",
            K::Mosfet {
                polarity: Polarity::N,
                model: "CN".into(),
            },
            &["out", "in", "0"],
        )
        .build();
    let vin = linear_steps(0.0, 5.0, 51);
    let ops = dc_sweep(&netlist, &models(), "VIN", &vin).unwrap();
    let out: Vec<f64> = ops
        .iter()
        .map(|o| o.voltage_by_name("out").unwrap())
        .collect();
    assert!(out[0] > 4.999 && out[50] < 0.001);
    assert_relative_eq!(out[25], 2.5, epsilon = 1e-6);
    assert!(out.windows(2).all(|w| w[1] <= w[0] + 1e-6));
}

#[test]
fn bjt_output_characteristics() {
    // Curve tracer: base current steps, collector voltage swept.
    let netlist = NetlistBuilder::new()
        .element("VCE", v(0.0), &["c", "0"])
        .element(
            "IB",
            K::CurrentSource {
                waveform: Waveform::Dc(0.0),
            },
            &["0", "b"],
        )
        .element(
            "Q1",
            K::Bjt {
                polarity: Polarity::N,
                model: "QN".into(),
            },
            &["c", "b", "0"],
        )
        .build();
    let mut sweeper = DcSweeper::new(&netlist, &models()).unwrap();
    for ib in [10e-6, 20e-6, 50e-6] {
        sweeper.set_source("IB", ib).unwrap();
        for vce in linear_steps(0.0, 5.0, 26) {
            sweeper.set_source("VCE", vce).unwrap();
            let ic = sweeper.solve().unwrap().current("Q1").unwrap();
            if vce >= 1.0 {
                // Forward active: Ic = BF·Ib (no Early effect in this model).
                assert_relative_eq!(ic, 100.0 * ib, max_relative = 1e-3);
            }
            if vce == 0.0 {
                assert!(ic <= 0.0, "saturated at Vce = 0: {ic}");
            }
        }
    }
}
