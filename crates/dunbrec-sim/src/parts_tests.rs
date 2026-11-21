//! The library part models, simulated under the test conditions of their
//! datasheets and checked against the guaranteed limits.

use dunbrec_core::{
    ComponentKind as K, ModelLibrary, Netlist, NetlistBuilder, Polarity, Waveform, parts,
};

use crate::{DcSweeper, ac_analysis};

fn library() -> ModelLibrary {
    let mut lib = ModelLibrary::new();
    for p in parts::standard_parts() {
        lib.insert(p.model);
    }
    lib
}

fn dc(v: f64) -> Waveform {
    Waveform::Dc(v)
}

fn bjt(model: &str, pnp: bool) -> K {
    K::Bjt {
        polarity: if pnp { Polarity::P } else { Polarity::N },
        model: model.into(),
    }
}

/// Fixed VCE, base current source: for hFE measurements.
fn hfe_fixture(model: &str, pnp: bool) -> Netlist {
    NetlistBuilder::new()
        .element("VCE", K::VoltageSource { waveform: dc(0.0) }, &["c", "0"])
        .element("IB", K::CurrentSource { waveform: dc(0.0) }, &["0", "b"])
        .element("Q", bjt(model, pnp), &["c", "b", "0"])
        .build()
}

/// Base current giving collector current `ic` at `vce` (magnitudes; signs
/// are flipped for PNP), found by bisection in log space.
fn base_current_for(model: &str, pnp: bool, ic: f64, vce: f64) -> (f64, f64) {
    let lib = library();
    let s = if pnp { -1.0 } else { 1.0 };
    let mut sw = DcSweeper::new(&hfe_fixture(model, pnp), &lib).unwrap();
    sw.set_source("VCE", s * vce).unwrap();
    let mut collector = |ib: f64| {
        sw.set_source("IB", s * ib).unwrap();
        s * sw.solve().unwrap().current("Q").unwrap()
    };
    let (mut lo, mut hi) = (1e-9_f64, 10e-3_f64);
    for _ in 0..80 {
        let mid = (lo * hi).sqrt();
        if collector(mid) < ic {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let ib = (lo * hi).sqrt();
    (ib, collector(ib))
}

fn hfe(model: &str, pnp: bool, ic: f64, vce: f64) -> f64 {
    let (ib, got) = base_current_for(model, pnp, ic, vce);
    assert!((got - ic).abs() / ic < 1e-6);
    ic / ib
}

/// (VCE(sat), VBE(sat)) with the base and collector driven by currents.
fn saturation(model: &str, pnp: bool, ic: f64, ib: f64) -> (f64, f64) {
    let s = if pnp { -1.0 } else { 1.0 };
    let netlist = NetlistBuilder::new()
        .element(
            "IC",
            K::CurrentSource {
                waveform: dc(s * ic),
            },
            &["0", "c"],
        )
        .element(
            "IB",
            K::CurrentSource {
                waveform: dc(s * ib),
            },
            &["0", "b"],
        )
        .element("Q", bjt(model, pnp), &["c", "b", "0"])
        .build();
    let op = DcSweeper::new(&netlist, &library())
        .unwrap()
        .solve()
        .unwrap();
    (
        s * op.voltage_by_name("c").unwrap(),
        s * op.voltage_by_name("b").unwrap(),
    )
}

#[test]
fn q2n3904_meets_datasheet() {
    // hFE limits (onsemi 2N3904 datasheet, VCE = 1 V).
    for (ic, min, max) in [
        (0.1e-3, 40.0, f64::INFINITY),
        (1e-3, 70.0, f64::INFINITY),
        (10e-3, 100.0, 300.0),
        (50e-3, 60.0, f64::INFINITY),
        (100e-3, 30.0, f64::INFINITY),
    ] {
        let h = hfe("2N3904", false, ic, 1.0);
        assert!(h >= min && h <= max, "hFE {h:.1} at IC = {ic}");
    }
    let (vce, vbe) = saturation("2N3904", false, 10e-3, 1e-3);
    assert!(vce <= 0.2, "VCE(sat) {vce}");
    assert!((0.65..=0.85).contains(&vbe), "VBE(sat) {vbe}");
    let (vce, vbe) = saturation("2N3904", false, 50e-3, 5e-3);
    assert!(vce <= 0.3, "VCE(sat) {vce} at 50 mA");
    assert!(vbe <= 0.95, "VBE(sat) {vbe} at 50 mA");
}

#[test]
fn q2n3906_meets_datasheet() {
    for (ic, min, max) in [
        (0.1e-3, 60.0, f64::INFINITY),
        (1e-3, 80.0, f64::INFINITY),
        (10e-3, 100.0, 300.0),
        (50e-3, 60.0, f64::INFINITY),
        (100e-3, 30.0, f64::INFINITY),
    ] {
        let h = hfe("2N3906", true, ic, 1.0);
        assert!(h >= min && h <= max, "hFE {h:.1} at IC = -{ic}");
    }
    let (vce, vbe) = saturation("2N3906", true, 10e-3, 1e-3);
    assert!(vce <= 0.25, "VCE(sat) {vce}");
    assert!((0.65..=0.85).contains(&vbe), "VBE(sat) {vbe}");
    let (vce, vbe) = saturation("2N3906", true, 50e-3, 5e-3);
    assert!(vce <= 0.4, "VCE(sat) {vce} at 50 mA");
    assert!(vbe <= 0.95, "VBE(sat) {vbe} at 50 mA");
}

#[test]
fn q2n3904_transition_frequency() {
    // fT ≥ 300 MHz at IC = 10 mA, VCE = 20 V, f = 100 MHz: |β(f)|·f.
    let (ib, _) = base_current_for("2N3904", false, 10e-3, 20.0);
    let netlist = NetlistBuilder::new()
        .element(
            "VCE",
            K::VoltageSource { waveform: dc(20.0) },
            &["supply", "0"],
        )
        .element("RS", K::Resistor { resistance: 1.0 }, &["supply", "c"])
        .element("IB", K::CurrentSource { waveform: dc(ib) }, &["0", "b"])
        .element("Q", bjt("2N3904", false), &["c", "b", "0"])
        .build();
    let f = 100e6;
    let res = ac_analysis(&netlist, &library(), "IB", &[f]).unwrap();
    // Unit AC base current; the collector current shows across the 1 Ω.
    let ic = res.response(res.node("c").unwrap())[0].norm();
    let ft = ic * f;
    assert!(ft >= 300e6, "fT = {:.0} MHz", ft / 1e6);
}

#[test]
fn d1n4148_meets_datasheet() {
    switching_diode_meets_datasheet("1N4148");
}

#[test]
fn d1n914_meets_datasheet() {
    switching_diode_meets_datasheet("1N914");
    let lib = library();
    // IR ≤ 5 µA at VR = 75 V (Vishay 1N914).
    let rev = NetlistBuilder::new()
        .element("VR", K::VoltageSource { waveform: dc(75.0) }, &["k", "0"])
        .element(
            "D",
            K::Diode {
                model: "1N914".into(),
            },
            &["0", "k"],
        )
        .build();
    let ir = DcSweeper::new(&rev, &lib)
        .unwrap()
        .solve()
        .unwrap()
        .current("D")
        .unwrap()
        .abs();
    assert!(ir <= 5e-6, "IR {ir} at 75 V");
    // CD ≤ 4 pF at VR = 0, 1 MHz: a unit AC current into the unbiased diode.
    let cap = NetlistBuilder::new()
        .element("IB", K::CurrentSource { waveform: dc(0.0) }, &["0", "a"])
        .element(
            "D",
            K::Diode {
                model: "1N914".into(),
            },
            &["a", "0"],
        )
        .build();
    let f = 1e6;
    let res = ac_analysis(&cap, &lib, "IB", &[f]).unwrap();
    let z = res.response(res.node("a").unwrap())[0].norm();
    let farads = 1.0 / (2.0 * std::f64::consts::PI * f * z);
    assert!(farads <= 4.05e-12, "CD {:.2} pF", farads * 1e12);
    // trr ≤ 4 ns (VR = 6 V, RL = 100 Ω, IF = 10 mA, iR = 1 mA).
    let trr = reverse_recovery_time("1N914");
    assert!(trr <= 4e-9, "trr {:.2} ns", trr * 1e9);
}

/// VF and leakage limits shared by the 1N914 and 1N4148 datasheets.
fn switching_diode_meets_datasheet(model: &str) {
    let lib = library();
    let diode = K::Diode {
        model: model.into(),
    };
    // VF ≤ 1 V at IF = 10 mA.
    let fwd = NetlistBuilder::new()
        .element(
            "IF",
            K::CurrentSource {
                waveform: dc(10e-3),
            },
            &["0", "a"],
        )
        .element("D", diode.clone(), &["a", "0"])
        .build();
    let vf = DcSweeper::new(&fwd, &lib)
        .unwrap()
        .solve()
        .unwrap()
        .voltage_by_name("a")
        .unwrap();
    assert!((0.6..=1.0).contains(&vf), "VF {vf}");
    // IR ≤ 25 nA at VR = 20 V.
    let rev = NetlistBuilder::new()
        .element("VR", K::VoltageSource { waveform: dc(20.0) }, &["k", "0"])
        .element("D", diode, &["0", "k"])
        .build();
    let ir = DcSweeper::new(&rev, &lib)
        .unwrap()
        .solve()
        .unwrap()
        .current("D")
        .unwrap()
        .abs();
    assert!(ir <= 25e-9, "IR {ir}");
}

#[test]
fn d1n750_zener_voltage() {
    // VZ = 4.7 V ± 5 % at IZT = 20 mA.
    let netlist = NetlistBuilder::new()
        .element(
            "IZ",
            K::CurrentSource {
                waveform: dc(20e-3),
            },
            &["0", "k"],
        )
        .element(
            "D",
            K::Diode {
                model: "1N750".into(),
            },
            &["0", "k"],
        )
        .build();
    let vz = DcSweeper::new(&netlist, &library())
        .unwrap()
        .solve()
        .unwrap()
        .voltage_by_name("k")
        .unwrap();
    assert!((4.465..=4.935).contains(&vz), "VZ {vz}");
}

/// Reverse recovery time under the 1N914 datasheet's test conditions:
/// forward current 10 mA, then switched to VR = 6 V through RL = 100 Ω; the
/// time from the switching edge until the reverse current has fallen to
/// 1 mA.
fn reverse_recovery_time(model: &str) -> f64 {
    use crate::{TransientOptions, transient};
    let edge = 20e-9;
    let netlist = NetlistBuilder::new()
        .element(
            "V1",
            K::VoltageSource {
                waveform: Waveform::Pulse {
                    // Chosen so the forward current settles at 10 mA.
                    v1: 1.72,
                    v2: -6.0,
                    delay: edge,
                    rise: 0.1e-9,
                    fall: 0.1e-9,
                    width: 1e3,
                    period: 0.0,
                },
            },
            &["in", "0"],
        )
        .element("RL", K::Resistor { resistance: 100.0 }, &["in", "a"])
        .element(
            "D",
            K::Diode {
                model: model.into(),
            },
            &["a", "0"],
        )
        .build();
    let circuit = crate::Circuit::from_netlist(&netlist, &library()).unwrap();
    let res = transient(
        circuit,
        TransientOptions::new(edge + 100e-9).with_max_step(0.05e-9),
    )
    .unwrap();
    let i = res.current("D").unwrap();
    let forward = res.at(i, edge * 0.9);
    assert!(
        (forward - 10e-3).abs() < 0.5e-3,
        "forward current {forward} A, not the 10 mA the test calls for"
    );
    let after: Vec<(f64, f64)> = res
        .times
        .iter()
        .zip(i)
        .filter(|(t, _)| **t > edge)
        .map(|(t, i)| (*t, *i))
        .collect();
    let back = after
        .iter()
        .skip_while(|p| p.1 > -1e-3)
        .find(|p| p.1 > -1e-3)
        .expect("the reverse current never recovered");
    back.0 - edge
}
