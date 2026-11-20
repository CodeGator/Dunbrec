//! Diode, BJT and MOSFET tests. Expected values come from independent
//! calculations (bisection on the device equation, closed-form MOSFET
//! solutions) or from physical relationships the solution must satisfy.

use approx::assert_relative_eq;
use dunbrec_core::{
    ComponentKind as K, DeviceModel, ModelKind, ModelLibrary, NetlistBuilder, Polarity, Waveform,
};

use crate::semiconductor::{GMIN, VT};
use crate::{
    Circuit, OperatingPoint, SimError, TransientOptions, TransientResult, operating_point,
    transient,
};

fn r(ohms: f64) -> K {
    K::Resistor { resistance: ohms }
}
fn v(volts: f64) -> K {
    K::VoltageSource {
        waveform: Waveform::Dc(volts),
    }
}
fn diode(model: &str) -> K {
    K::Diode {
        model: model.into(),
    }
}
fn npn(model: &str) -> K {
    K::Bjt {
        polarity: Polarity::N,
        model: model.into(),
    }
}
fn pnp(model: &str) -> K {
    K::Bjt {
        polarity: Polarity::P,
        model: model.into(),
    }
}
fn nmos(model: &str) -> K {
    K::Mosfet {
        polarity: Polarity::N,
        model: model.into(),
    }
}
fn pmos(model: &str) -> K {
    K::Mosfet {
        polarity: Polarity::P,
        model: model.into(),
    }
}

fn models() -> ModelLibrary {
    let mut lib = ModelLibrary::new();
    lib.insert(DeviceModel::new("D", ModelKind::Diode).with("IS", 1e-14));
    lib.insert(
        DeviceModel::new("DRS", ModelKind::Diode)
            .with("IS", 1e-14)
            .with("N", 1.5)
            .with("RS", 10.0),
    );
    lib.insert(
        DeviceModel::new("Z5V1", ModelKind::Diode)
            .with("IS", 1e-14)
            .with("BV", 5.1)
            .with("IBV", 1e-3),
    );
    lib.insert(
        DeviceModel::new("QN", ModelKind::Npn)
            .with("IS", 1e-16)
            .with("BF", 100.0),
    );
    lib.insert(
        DeviceModel::new("QP", ModelKind::Pnp)
            .with("IS", 1e-16)
            .with("BF", 100.0),
    );
    lib.insert(
        DeviceModel::new("QGP", ModelKind::Npn)
            .with("IS", 6.734e-15)
            .with("BF", 416.4)
            .with("BR", 0.7371)
            .with("VAF", 74.03)
            .with("IKF", 0.06678)
            .with("ISE", 6.734e-15)
            .with("NE", 1.259)
            .with("RB", 10.0)
            .with("RC", 1.0),
    );
    lib.insert(
        DeviceModel::new("MN", ModelKind::Nmos)
            .with("VTO", 2.0)
            .with("KP", 1e-3)
            .with("LAMBDA", 0.02),
    );
    lib.insert(
        DeviceModel::new("MN0", ModelKind::Nmos)
            .with("VTO", 2.0)
            .with("KP", 1e-3),
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

fn circuit(b: NetlistBuilder) -> Result<Circuit, SimError> {
    Circuit::from_netlist(&b.build(), &models())
}

fn op(b: NetlistBuilder) -> OperatingPoint {
    operating_point(&circuit(b).unwrap()).unwrap()
}

fn tran(b: NetlistBuilder, opts: TransientOptions) -> TransientResult {
    transient(circuit(b).unwrap(), opts).unwrap()
}

fn volts(op: &OperatingPoint, node: &str) -> f64 {
    op.voltage_by_name(node).unwrap()
}

/// Root of a monotonic function on [lo, hi] by bisection.
fn bisect(f: impl Fn(f64) -> f64, mut lo: f64, mut hi: f64) -> f64 {
    let rising = f(hi) > f(lo);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if (f(mid) > 0.0) == rising {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    0.5 * (lo + hi)
}

// ------------------------------------------------------------- diodes ---

#[test]
fn diode_forward_bias_matches_bisection() {
    let o = op(NetlistBuilder::new()
        .element("V1", v(5.0), &["in", "0"])
        .element("R1", r(1e3), &["in", "a"])
        .element("D1", diode("D"), &["a", "0"]));
    let is = 1e-14;
    let expected = bisect(
        |vd| (5.0 - vd) / 1e3 - is * ((vd / VT).exp() - 1.0) - GMIN * vd,
        0.0,
        1.0,
    );
    assert_relative_eq!(volts(&o, "a"), expected, epsilon = 1e-6);
    assert_relative_eq!(
        o.current("D1").unwrap(),
        (5.0 - expected) / 1e3,
        epsilon = 1e-9
    );
}

#[test]
fn diode_series_resistance_and_emission_coefficient() {
    let o = op(NetlistBuilder::new()
        .element("V1", v(5.0), &["in", "0"])
        .element("R1", r(100.0), &["in", "a"])
        .element("D1", diode("DRS"), &["a", "0"]));
    // Current I satisfies 5 = 100 I + 10 I + 1.5 Vt ln(I/IS + 1).
    let nvt = 1.5 * VT;
    let i = bisect(
        |i| 110.0 * i + nvt * (i / 1e-14 + 1.0).ln() - 5.0,
        1e-6,
        0.05,
    );
    assert_relative_eq!(o.current("D1").unwrap(), i, max_relative = 1e-5);
    assert_relative_eq!(volts(&o, "a"), 5.0 - 100.0 * i, epsilon = 1e-5);
    // The internal junction node exists and sits RS * I below the anode.
    let junction = volts(&o, "D1#a");
    assert_relative_eq!(volts(&o, "a") - junction, 10.0 * i, epsilon = 1e-5);
}

#[test]
fn reverse_biased_diode_leaks_only_is() {
    let o = op(NetlistBuilder::new()
        .element("V1", v(5.0), &["in", "0"])
        .element("R1", r(1e3), &["in", "k"])
        .element("D1", diode("D"), &["0", "k"]));
    assert!((volts(&o, "k") - 5.0).abs() < 1e-6);
    let i = o.current("D1").unwrap();
    assert!(i < 0.0 && i > -1e-11, "{i}");
}

#[test]
fn zener_regulates_near_breakdown_voltage() {
    let o = op(NetlistBuilder::new()
        .element("V1", v(12.0), &["in", "0"])
        .element("R1", r(1e3), &["in", "out"])
        .element("Z1", diode("Z5V1"), &["0", "out"])
        .element("RL", r(10e3), &["out", "0"]));
    let vout = volts(&o, "out");
    assert!(vout > 5.1 && vout < 5.3, "{vout}");
}

// --------------------------------------------------------------- BJTs ---

fn common_emitter(transistor: K, supply: f64) -> NetlistBuilder {
    NetlistBuilder::new()
        .element("VCC", v(supply), &["vcc", "0"])
        .element("RB", r(1e6), &["vcc", "b"])
        .element("RC", r(1e3), &["vcc", "c"])
        .element("Q1", transistor, &["c", "b", "0"])
}

#[test]
fn npn_forward_active_bias() {
    let o = op(common_emitter(npn("QN"), 10.0));
    let (vb, vc) = (volts(&o, "b"), volts(&o, "c"));
    let ic = o.current("Q1").unwrap();
    let ib = (10.0 - vb) / 1e6;
    // Forward active: Ic ≈ BF·Ib, Vbe ≈ Vt ln(Ic/IS), KCL at the collector.
    assert_relative_eq!(ic / ib, 100.0, max_relative = 1e-3);
    assert_relative_eq!(vb, VT * (ic / 1e-16).ln(), epsilon = 1e-3);
    assert_relative_eq!(vc, 10.0 - 1e3 * ic, epsilon = 1e-6);
    assert!(vc > vb, "should not be saturated");
}

#[test]
fn pnp_mirrors_npn() {
    let n = op(common_emitter(npn("QN"), 10.0));
    let p = op(common_emitter(pnp("QP"), -10.0));
    for node in ["b", "c"] {
        assert_relative_eq!(volts(&p, node), -volts(&n, node), epsilon = 1e-9);
    }
    assert_relative_eq!(
        p.current("Q1").unwrap(),
        -n.current("Q1").unwrap(),
        epsilon = 1e-12
    );
}

#[test]
fn saturated_bjt() {
    let o = op(NetlistBuilder::new()
        .element("VCC", v(5.0), &["vcc", "0"])
        .element("RB", r(1e3), &["vcc", "b"])
        .element("RC", r(1e3), &["vcc", "c"])
        .element("Q1", npn("QN"), &["c", "b", "0"]));
    let vce = volts(&o, "c");
    assert!(vce > 0.0 && vce < 0.2, "Vce(sat) = {vce}");
}

#[test]
fn gummel_poon_model_with_parasitics_converges() {
    // Voltage-divider-biased amplifier with a realistic 2N3904-like model.
    let o = op(NetlistBuilder::new()
        .element("VCC", v(12.0), &["vcc", "0"])
        .element("R1", r(47e3), &["vcc", "b"])
        .element("R2", r(10e3), &["b", "0"])
        .element("RC", r(4.7e3), &["vcc", "c"])
        .element("RE", r(1e3), &["e", "0"])
        .element("Q1", npn("QGP"), &["c", "b", "e"]));
    let (vb, ve, vc) = (volts(&o, "b"), volts(&o, "e"), volts(&o, "c"));
    // Classic hand analysis: Vb ≈ 12·10/57 ≈ 2.1 V, Ve ≈ Vb - 0.65 V.
    assert!(vb > 1.9 && vb < 2.15, "Vb {vb}");
    assert!((vb - ve) > 0.55 && (vb - ve) < 0.75, "Vbe {}", vb - ve);
    let ic = o.current("Q1").unwrap();
    assert!(ic > 1.2e-3 && ic < 1.6e-3, "Ic {ic}");
    // KCL at the collector, through the RC parasitic.
    assert_relative_eq!(vc, 12.0 - 4.7e3 * ic, epsilon = 1e-6);
    assert!(o.voltage_by_name("Q1#b").is_some() && o.voltage_by_name("Q1#c").is_some());
}

#[test]
fn differential_pair_splits_tail_current() {
    let o = op(NetlistBuilder::new()
        .element("VCC", v(10.0), &["vcc", "0"])
        .element("VEE", v(-10.0), &["vee", "0"])
        .element("RC1", r(5e3), &["vcc", "c1"])
        .element("RC2", r(5e3), &["vcc", "c2"])
        .element("Q1", npn("QN"), &["c1", "0", "tail"])
        .element("Q2", npn("QN"), &["c2", "0", "tail"])
        .element("RT", r(9.3e3), &["tail", "vee"]));
    let (i1, i2) = (o.current("Q1").unwrap(), o.current("Q2").unwrap());
    assert_relative_eq!(i1, i2, max_relative = 1e-9);
    let tail = (volts(&o, "tail") + 10.0) / 9.3e3;
    // Each collector carries α/2 of the tail current.
    assert_relative_eq!(i1 + i2, tail * 100.0 / 101.0, max_relative = 1e-3);
}

#[test]
fn darlington_converges() {
    let o = op(NetlistBuilder::new()
        .element("VCC", v(12.0), &["vcc", "0"])
        .element("RB", r(1e6), &["vcc", "b"])
        .element("RL", r(100.0), &["vcc", "c"])
        .element("Q1", npn("QN"), &["c", "b", "m"])
        .element("Q2", npn("QN"), &["c", "m", "0"]));
    let ic2 = o.current("Q2").unwrap();
    assert!(ic2 > 1e-2, "Darlington should conduct strongly: {ic2}");
}

// ------------------------------------------------------------ MOSFETs ---

#[test]
fn nmos_saturation_with_channel_length_modulation() {
    let o = op(NetlistBuilder::new()
        .element("VDD", v(10.0), &["vdd", "0"])
        .element("VG", v(3.0), &["g", "0"])
        .element("RD", r(1e3), &["vdd", "d"])
        .element("M1", nmos("MN"), &["d", "g", "0"]));
    // id = KP/2 (Vgs - VTO)² (1 + λ Vds), Vds = 10 - 1k·id
    // => id = 0.5e-3 (1.2 - 20 id) => id = 0.6e-3 / 1.01
    let id = 0.6e-3 / 1.01;
    assert_relative_eq!(o.current("M1").unwrap(), id, max_relative = 1e-6);
    assert_relative_eq!(volts(&o, "d"), 10.0 - 1e3 * id, epsilon = 1e-6);
}

#[test]
fn nmos_triode_region() {
    let o = op(NetlistBuilder::new()
        .element("VDD", v(10.0), &["vdd", "0"])
        .element("RD", r(10e3), &["vdd", "d"])
        .element("M1", nmos("MN0"), &["d", "vdd", "0"]));
    // 1e-3 (8 v - v²/2) = (10 - v)/1e4  =>  5v² - 81v + 10 = 0
    let vds = (81.0 - (81.0f64 * 81.0 - 200.0).sqrt()) / 10.0;
    assert_relative_eq!(volts(&o, "d"), vds, epsilon = 1e-6);
}

#[test]
fn cmos_inverter_transfer() {
    let inverter = |vin: f64| {
        let o = op(NetlistBuilder::new()
            .element("VDD", v(5.0), &["vdd", "0"])
            .element("VIN", v(vin), &["in", "0"])
            .element("MP", pmos("CP"), &["out", "in", "vdd"])
            .element("MN", nmos("CN"), &["out", "in", "0"]));
        volts(&o, "out")
    };
    assert!(inverter(0.0) > 4.999);
    assert!(inverter(5.0) < 0.001);
    // Symmetric devices switch at exactly half the supply.
    assert_relative_eq!(inverter(2.5), 2.5, epsilon = 1e-6);
    // Monotonic falling transfer curve.
    let curve: Vec<f64> = (0..=10).map(|k| inverter(k as f64 * 0.5)).collect();
    // (Leakage conductances move the rails by nanovolts, hence the slack.)
    assert!(curve.windows(2).all(|w| w[1] <= w[0] + 1e-6), "{curve:?}");
}

// ---------------------------------------------------------- transient ---

#[test]
fn half_wave_rectifier_with_smoothing() {
    let res = tran(
        NetlistBuilder::new()
            .element(
                "V1",
                K::VoltageSource {
                    waveform: Waveform::Sine {
                        offset: 0.0,
                        amplitude: 10.0,
                        frequency: 50.0,
                        delay: 0.0,
                        damping: 0.0,
                        phase: 0.0,
                    },
                },
                &["in", "0"],
            )
            .element("D1", diode("D"), &["in", "out"])
            .element(
                "C1",
                K::Capacitor {
                    capacitance: 100e-6,
                    initial_voltage: None,
                },
                &["out", "0"],
            )
            .element("RL", r(1e3), &["out", "0"]),
        TransientOptions::new(0.1),
    );
    let out = res.voltage("out").unwrap();
    let late: Vec<f64> = res
        .times
        .iter()
        .zip(out)
        .filter(|(t, _)| **t > 0.06)
        .map(|(_, v)| *v)
        .collect();
    let peak = late.iter().copied().fold(f64::MIN, f64::max);
    let trough = late.iter().copied().fold(f64::MAX, f64::min);
    // Peak is one diode drop below 10 V; ripple ≈ Vpeak / (f·R·C) ≈ 1.9 V.
    assert!(peak > 9.2 && peak < 9.5, "peak {peak}");
    let ripple = peak - trough;
    assert!(ripple > 1.4 && ripple < 2.0, "ripple {ripple}");
    // The diode never conducts backwards by more than leakage.
    let id = res.current("D1").unwrap();
    assert!(id.iter().all(|&i| i > -1e-9));
}

#[test]
fn common_emitter_amplifier_inverts_and_amplifies() {
    let coupling = |c: f64| K::Capacitor {
        capacitance: c,
        initial_voltage: None,
    };
    let res = tran(
        NetlistBuilder::new()
            .element("VCC", v(12.0), &["vcc", "0"])
            .element(
                "VIN",
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
                &["sig", "0"],
            )
            .element("CIN", coupling(10e-6), &["sig", "b"])
            .element("R1", r(47e3), &["vcc", "b"])
            .element("R2", r(10e3), &["b", "0"])
            .element("RC", r(4.7e3), &["vcc", "c"])
            .element("RE", r(1e3), &["e", "0"])
            .element("CE", coupling(100e-6), &["e", "0"])
            .element("Q1", npn("QGP"), &["c", "b", "e"]),
        TransientOptions::new(5e-3),
    );
    let (sig, c) = (res.voltage("sig").unwrap(), res.voltage("c").unwrap());
    // Use the last millisecond, after the coupling capacitors settle.
    let window: Vec<(f64, f64)> = res
        .times
        .iter()
        .zip(sig.iter().zip(c))
        .filter(|(t, _)| **t > 4e-3)
        .map(|(_, (s, c))| (*s, *c))
        .collect();
    let mean_c = window.iter().map(|w| w.1).sum::<f64>() / window.len() as f64;
    let swing = |f: &dyn Fn(&(f64, f64)) -> f64| {
        let max = window.iter().map(f).fold(f64::MIN, f64::max);
        let min = window.iter().map(f).fold(f64::MAX, f64::min);
        max - min
    };
    let gain = swing(&|w| w.1) / swing(&|w| w.0);
    // Bypassed emitter: gain ≈ gm·RC ≈ (1.4 mA / 26 mV)·4.7k ≈ 250, reduced
    // somewhat by the transistor's base resistance and Early effect.
    assert!(gain > 120.0 && gain < 300.0, "gain {gain}");
    // Output is inverted: covariance with the input is negative.
    let cov: f64 = window.iter().map(|(s, c)| s * (c - mean_c)).sum();
    assert!(cov < 0.0);
}

#[test]
fn missing_model_is_reported() {
    let err = circuit(
        NetlistBuilder::new()
            .element("V1", v(1.0), &["a", "0"])
            .element("D1", diode("NOPE"), &["a", "0"]),
    )
    .unwrap_err();
    assert!(matches!(err, SimError::InvalidParameter { designator, .. } if designator == "D1"));
}

fn darlington_with_degeneration() -> NetlistBuilder {
    NetlistBuilder::new()
        .element("VCC", v(12.0), &["vcc", "0"])
        .element("RB", r(1e6), &["vcc", "b"])
        .element("RL", r(100.0), &["vcc", "c"])
        .element("Q1", npn("QGP"), &["c", "b", "m"])
        .element("Q2", npn("QGP"), &["c", "m", "e"])
        .element("RE", r(10.0), &["e", "0"])
}

#[test]
fn convergence_aids_reach_the_same_answer() {
    use crate::device::Tolerances;
    use crate::newton::{DcStrategy, Workspace, dc_solve_with};

    let reference = op(darlington_with_degeneration());

    // Starve Newton of iterations so plain Newton fails and a fallback is
    // needed; the answer must not change.
    let mut used = Vec::new();
    for limit in [4, 6, 8] {
        let c = circuit(darlington_with_degeneration()).unwrap();
        let mut ws = Workspace::new(c.unknown_count());
        let (x, strategy) = dc_solve_with(&c, false, &mut ws, &Tolerances::default(), limit)
            .unwrap_or_else(|e| panic!("limit {limit}: {e}"));
        used.push(strategy);
        let o = OperatingPoint::from_solution(&c, &x);
        for node in ["b", "m", "c", "e"] {
            assert_relative_eq!(volts(&o, node), volts(&reference, node), epsilon = 1e-5);
        }
    }
    assert!(used.iter().any(|s| *s != DcStrategy::Newton), "{used:?}");
}
