//! Transient analysis tests against closed-form solutions.

use std::f64::consts::{E, TAU};

use dunbrec_core::{ComponentKind as K, ModelLibrary, NetlistBuilder, NodeId, Waveform};

use crate::{
    Circuit, Edge, SimError, Transient, TransientOptions, TransientResult, Watch, transient,
};

fn r(ohms: f64) -> K {
    K::Resistor { resistance: ohms }
}
fn c(farads: f64, ic: Option<f64>) -> K {
    K::Capacitor {
        capacitance: farads,
        initial_voltage: ic,
    }
}
fn l(henries: f64, ic: Option<f64>) -> K {
    K::Inductor {
        inductance: henries,
        initial_current: ic,
    }
}
/// A 0 → `v` step at t = 0 with a 1 ns rise, held for the whole run.
fn step(v: f64) -> K {
    K::VoltageSource {
        waveform: Waveform::Pulse {
            v1: 0.0,
            v2: v,
            delay: 0.0,
            rise: 1e-9,
            fall: 1e-9,
            width: 1e3,
            period: 0.0,
        },
    }
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

fn circuit(b: NetlistBuilder) -> Circuit {
    Circuit::from_netlist(&b.build(), &ModelLibrary::new()).unwrap()
}

fn run(b: NetlistBuilder, stop: f64) -> TransientResult {
    transient(circuit(b), TransientOptions::new(stop)).unwrap()
}

/// Largest |simulated - exact| over all samples after `from`.
fn max_error(res: &TransientResult, trace: &[f64], from: f64, exact: impl Fn(f64) -> f64) -> f64 {
    res.times
        .iter()
        .zip(trace)
        .filter(|(t, _)| **t >= from)
        .map(|(t, v)| (v - exact(*t)).abs())
        .fold(0.0, f64::max)
}

const TAU_RC: f64 = 1e-3; // 1 kΩ × 1 µF

fn rc_charge() -> NetlistBuilder {
    NetlistBuilder::new()
        .element("V1", step(1.0), &["in", "0"])
        .element("R1", r(1e3), &["in", "out"])
        .element("C1", c(1e-6, None), &["out", "0"])
}

#[test]
fn rc_charging_matches_exponential() {
    let res = run(rc_charge(), 5.0 * TAU_RC);
    let out = res.voltage("out").unwrap();
    let err = max_error(&res, out, 0.0, |t| 1.0 - (-t / TAU_RC).exp());
    assert!(err < 2e-3, "max error {err}");
    // Final value and capacitor current I = (1 - v)/R.
    let last = *out.last().unwrap();
    assert!(
        (last - (1.0 - (-5.0f64).exp())).abs() < 2e-3,
        "final {last}"
    );
    let ic = res.current("C1").unwrap();
    let i_err = max_error(&res, ic, 1e-5, |t| (-t / TAU_RC).exp() / 1e3);
    assert!(i_err < 2e-6, "current error {i_err}");
    assert_eq!(*res.times.last().unwrap(), 5.0 * TAU_RC);
}

#[test]
fn rc_discharge_from_initial_condition() {
    let res = run(
        NetlistBuilder::new()
            .element("R1", r(1e3), &["out", "0"])
            .element("C1", c(1e-6, Some(5.0)), &["out", "0"]),
        5.0 * TAU_RC,
    );
    let out = res.voltage("out").unwrap();
    assert!((out[0] - 5.0).abs() < 1e-6, "initial {}", out[0]);
    let err = max_error(&res, out, 0.0, |t| 5.0 * (-t / TAU_RC).exp());
    assert!(err < 1e-2, "max error {err}");
}

#[test]
fn rl_current_rise() {
    // τ = L/R = 10 mH / 100 Ω = 100 µs; I_final = 10 mA.
    let tau = 1e-4;
    let res = run(
        NetlistBuilder::new()
            .element("V1", step(1.0), &["in", "0"])
            .element("R1", r(100.0), &["in", "a"])
            .element("L1", l(10e-3, None), &["a", "0"]),
        5.0 * tau,
    );
    let il = res.current("L1").unwrap();
    let err = max_error(&res, il, 0.0, |t| 0.01 * (1.0 - (-t / tau).exp()));
    assert!(err < 2e-5, "max error {err}");
}

#[test]
fn parallel_rlc_rings_and_decays() {
    // C = 1 µF charged to 1 V, L = 1 mH, R = 100 Ω, all in parallel.
    // v(t) = e^{-αt} (cos ωd t - (α/ωd) sin ωd t), α = 1/(2RC).
    let (rr, ll, cc): (f64, f64, f64) = (100.0, 1e-3, 1e-6);
    let alpha = 1.0 / (2.0 * rr * cc);
    let w0 = 1.0 / (ll * cc).sqrt();
    let wd = (w0 * w0 - alpha * alpha).sqrt();
    let exact = |t: f64| (-alpha * t).exp() * ((wd * t).cos() - alpha / wd * (wd * t).sin());

    let stop = 4.0 * TAU / wd;
    let res = run(
        NetlistBuilder::new()
            .element("R1", r(rr), &["v", "0"])
            .element("L1", l(ll, Some(0.0)), &["v", "0"])
            .element("C1", c(cc, Some(1.0)), &["v", "0"]),
        stop,
    );
    let v = res.voltage("v").unwrap();
    let err = max_error(&res, v, 0.0, exact);
    assert!(err < 1e-2, "max error {err}");
}

#[test]
fn lossless_lc_keeps_its_energy() {
    // Trapezoidal integration does not add damping: after 5 periods the
    // amplitude should still be ~1.
    let (ll, cc): (f64, f64) = (1e-3, 1e-6);
    let period = TAU * (ll * cc).sqrt();
    let res = run(
        NetlistBuilder::new()
            .element("L1", l(ll, Some(0.0)), &["v", "0"])
            .element("C1", c(cc, Some(1.0)), &["v", "0"]),
        5.0 * period,
    );
    let v = res.voltage("v").unwrap();
    let last_period_peak = res
        .times
        .iter()
        .zip(v)
        .filter(|(t, _)| **t > 4.0 * period)
        .map(|(_, v)| v.abs())
        .fold(0.0, f64::max);
    assert!(
        (last_period_peak - 1.0).abs() < 2e-2,
        "peak {last_period_peak}"
    );
}

#[test]
fn rc_lowpass_at_corner_frequency() {
    // At f = 1/(2πRC) the output is 1/√2 of the input.
    let f = 1.0 / (TAU * TAU_RC);
    let res = run(
        NetlistBuilder::new()
            .element("V1", sine(1.0, f), &["in", "0"])
            .element("R1", r(1e3), &["in", "out"])
            .element("C1", c(1e-6, None), &["out", "0"]),
        20.0 / f,
    );
    let out = res.voltage("out").unwrap();
    let peak = res
        .times
        .iter()
        .zip(out)
        .filter(|(t, _)| **t > 18.0 / f)
        .map(|(_, v)| v.abs())
        .fold(0.0, f64::max);
    assert!((peak - 0.5f64.sqrt()).abs() < 5e-3, "peak {peak}");
}

#[test]
fn sine_source_is_sampled_finely() {
    let f = 1e3;
    let res = run(
        NetlistBuilder::new()
            .element("V1", sine(2.0, f), &["in", "0"])
            .element("R1", r(1e3), &["in", "0"]),
        2e-3,
    );
    let max_dt = res
        .times
        .windows(2)
        .map(|w| w[1] - w[0])
        .fold(0.0, f64::max);
    assert!(
        max_dt <= 1.0 / (50.0 * f) * (1.0 + 1e-9),
        "max step {max_dt}"
    );
    let v = res.voltage("in").unwrap();
    let err = max_error(&res, v, 0.0, |t| 2.0 * (TAU * f * t).sin());
    assert!(err < 1e-9, "error {err}");
}

#[test]
fn pulse_corners_are_hit_exactly() {
    let wave = Waveform::Pulse {
        v1: 0.0,
        v2: 5.0,
        delay: 1e-4,
        rise: 1e-6,
        fall: 1e-6,
        width: 4e-4,
        period: 1e-3,
    };
    let res = run(
        NetlistBuilder::new()
            .element(
                "V1",
                K::VoltageSource {
                    waveform: wave.clone(),
                },
                &["in", "0"],
            )
            .element("R1", r(1e3), &["in", "out"])
            .element("C1", c(1e-7, None), &["out", "0"]),
        3e-3,
    );
    let corners = [1e-4, 1.01e-4, 5.01e-4, 5.02e-4, 1.1e-3, 1.101e-3, 2.1e-3];
    for corner in corners {
        let hit = res.times.iter().any(|&t| (t - corner).abs() < 1e-15);
        assert!(hit, "no time point at {corner}");
    }
    // Source node follows the waveform exactly at every sample.
    let vin = res.voltage("in").unwrap();
    let err = max_error(&res, vin, 0.0, |t| wave.value_at(t));
    assert!(err < 1e-9, "error {err}");
}

#[test]
fn threshold_crossing_lands_on_time_constant() {
    // v(t) = 1 - e^{-t/τ} crosses 1 - 1/e at t = τ.
    let mut sim =
        Transient::new(circuit(rc_charge()), TransientOptions::new(3.0 * TAU_RC)).unwrap();
    let out = NodeId(2);
    assert_eq!(sim.circuit().node_name(out), "out");
    let rising = sim.add_watch(Watch {
        p: out,
        n: NodeId::GROUND,
        level: 1.0 - 1.0 / E,
        edge: Edge::Rising,
    });
    sim.add_watch(Watch {
        p: out,
        n: NodeId::GROUND,
        level: 0.5,
        edge: Edge::Falling, // never happens
    });
    let res = crate::transient::run(sim).unwrap();
    assert_eq!(res.crossings.len(), 1);
    let x = res.crossings[0];
    assert_eq!(x.watch, rising);
    assert_eq!(x.edge, Edge::Rising);
    assert!((x.time - TAU_RC).abs() < 2e-6, "crossed at {}", x.time);
    // A time point was placed right at the crossing.
    assert!(res.times.iter().any(|&t| (t - x.time).abs() < 1e-8));
}

#[test]
fn stepping_api_advances_monotonically() {
    let stop = 1e-3;
    let mut sim = Transient::new(circuit(rc_charge()), TransientOptions::new(stop)).unwrap();
    assert_eq!(sim.time(), 0.0);
    assert_eq!(sim.initial_point().voltage_by_name("out"), Some(0.0));
    let mut last = 0.0;
    let mut steps = 0;
    while let Some(s) = sim.step().unwrap() {
        assert!(s.sample.time > last);
        last = s.sample.time;
        steps += 1;
    }
    assert_eq!(last, stop);
    assert!(sim.is_finished());
    assert!(sim.step().unwrap().is_none());
    assert!(steps > 10 && steps < 2000, "{steps} steps");
}

#[test]
fn max_step_option_is_respected() {
    let res = transient(
        circuit(rc_charge()),
        TransientOptions::new(1e-3).with_max_step(1e-6),
    )
    .unwrap();
    let max_dt = res
        .times
        .windows(2)
        .map(|w| w[1] - w[0])
        .fold(0.0, f64::max);
    assert!(max_dt <= 1e-6 * (1.0 + 1e-9));
}

#[test]
fn invalid_options_are_rejected() {
    let err = Transient::new(circuit(rc_charge()), TransientOptions::new(0.0)).err();
    assert!(matches!(err, Some(SimError::InvalidOptions(_))));
    let err = Transient::new(
        circuit(rc_charge()),
        TransientOptions::new(1.0).with_max_step(-1.0),
    )
    .err();
    assert!(matches!(err, Some(SimError::InvalidOptions(_))));
}

#[test]
fn interpolation_helper() {
    let res = run(rc_charge(), 5.0 * TAU_RC);
    let out = res.voltage("out").unwrap();
    let v = res.at(out, TAU_RC);
    assert!((v - (1.0 - 1.0 / E)).abs() < 3e-3, "{v}");
}

// ------------------------------------------------------ live simulation ---

/// Step `sim` until its time reaches `t`, returning the last sample.
fn run_until(sim: &mut Transient, t: f64) -> crate::Sample {
    let mut last = sim.sample();
    while sim.time() < t {
        last = sim
            .step()
            .unwrap()
            .expect("free-running never finishes")
            .sample;
    }
    last
}

#[test]
fn free_running_simulation_tracks_rc_charging() {
    let mut sim =
        Transient::new(circuit(rc_charge()), TransientOptions::free_running(1e-5)).unwrap();
    let out = NodeId(2);
    let s = run_until(&mut sim, TAU_RC);
    let expected = 1.0 - (-s.time / TAU_RC).exp();
    assert!((s.voltages[out.0] - expected).abs() < 2e-3);
    run_until(&mut sim, 20.0 * TAU_RC);
    assert!(!sim.is_finished());

    let bad = TransientOptions {
        stop_time: f64::INFINITY,
        max_step: None,
        tolerances: Default::default(),
    };
    assert!(matches!(
        Transient::new(circuit(rc_charge()), bad).err(),
        Some(SimError::InvalidOptions(_))
    ));
}

#[test]
fn changing_a_source_mid_run_continues_from_the_present_state() {
    let netlist = NetlistBuilder::new()
        .element(
            "V1",
            K::VoltageSource {
                waveform: Waveform::Dc(1.0),
            },
            &["in", "0"],
        )
        .element("R1", r(1e3), &["in", "out"])
        .element("C1", c(1e-6, None), &["out", "0"])
        .build();
    let models = ModelLibrary::new();
    let mut sim = Transient::new(
        Circuit::from_netlist(&netlist, &models).unwrap(),
        TransientOptions::free_running(1e-5),
    )
    .unwrap();
    // DC start: the capacitor is already at 1 V.
    let before = run_until(&mut sim, 1e-3);
    assert!((before.voltages[2] - 1.0).abs() < 1e-6);

    // Step the source to 2 V: the output then charges from 1 V towards 2 V.
    let mut v1 = netlist.elements[0].clone();
    v1.kind = K::VoltageSource {
        waveform: Waveform::Dc(2.0),
    };
    assert!(sim.update_element(0, &v1, &models).unwrap());
    let t0 = sim.time();
    let s = run_until(&mut sim, t0 + TAU_RC);
    let expected = 2.0 - (-(s.time - t0) / TAU_RC).exp();
    assert!(
        (s.voltages[2] - expected).abs() < 3e-3,
        "{} vs {expected}",
        s.voltages[2]
    );
    assert!(
        (s.voltages[1] - 2.0).abs() < 1e-9,
        "source node follows at once"
    );
}

#[test]
fn closing_a_switch_mid_run() {
    let switch = |closed| K::Switch {
        closed,
        r_on: 1e-3,
        r_off: 1e12,
        momentary: false,
    };
    let netlist = NetlistBuilder::new()
        .element(
            "V1",
            K::VoltageSource {
                waveform: Waveform::Dc(5.0),
            },
            &["in", "0"],
        )
        .element("S1", switch(false), &["in", "a"])
        .element("R1", r(1e3), &["a", "out"])
        .element("C1", c(1e-6, None), &["out", "0"])
        // An open switch is 1 TΩ, so a bleed resistor holds the output low.
        .element("R2", r(1e6), &["out", "0"])
        .build();
    let models = ModelLibrary::new();
    let mut sim = Transient::new(
        Circuit::from_netlist(&netlist, &models).unwrap(),
        TransientOptions::free_running(1e-5),
    )
    .unwrap();
    let out = netlist.node_by_name("out").unwrap().0;
    assert!(run_until(&mut sim, 1e-3).voltages[out].abs() < 1e-4);

    let mut s1 = netlist.elements[1].clone();
    s1.kind = switch(true);
    assert!(sim.update_element(1, &s1, &models).unwrap());
    let t0 = sim.time();
    // Thevenin view from the capacitor: 5 V·R2/(R1+R2) behind R1 ∥ R2.
    let final_v = 5.0 * 1e6 / (1e6 + 1e3);
    let tau = (1e3 * 1e6 / (1e3 + 1e6)) * 1e-6;
    let s = run_until(&mut sim, t0 + 3.0 * tau);
    let expected = final_v * (1.0 - (-(s.time - t0) / tau).exp());
    assert!(
        (s.voltages[out] - expected).abs() < 1e-2,
        "{} vs {expected}",
        s.voltages[out]
    );
}

#[test]
fn structural_change_is_refused() {
    use dunbrec_core::{DeviceModel, ModelKind};
    let mut models = ModelLibrary::new();
    models.insert(DeviceModel::new("D", ModelKind::Diode));
    models.insert(DeviceModel::new("DRS", ModelKind::Diode).with("RS", 5.0));
    let netlist = NetlistBuilder::new()
        .element(
            "V1",
            K::VoltageSource {
                waveform: Waveform::Dc(5.0),
            },
            &["in", "0"],
        )
        .element("R1", r(1e3), &["in", "a"])
        .element("D1", K::Diode { model: "D".into() }, &["a", "0"])
        .build();
    let mut sim = Transient::new(
        Circuit::from_netlist(&netlist, &models).unwrap(),
        TransientOptions::free_running(1e-5),
    )
    .unwrap();
    run_until(&mut sim, 1e-4);
    // A series resistance needs an extra internal node: restart required.
    let mut d1 = netlist.elements[2].clone();
    d1.kind = K::Diode {
        model: "DRS".into(),
    };
    assert!(!sim.update_element(2, &d1, &models).unwrap());
    // The simulation carries on unchanged.
    run_until(&mut sim, 2e-4);
}

#[test]
fn max_step_can_change_mid_run() {
    let mut sim =
        Transient::new(circuit(rc_charge()), TransientOptions::free_running(1e-4)).unwrap();
    run_until(&mut sim, 1e-3);
    sim.set_max_step(1e-6).unwrap();
    let mut last = sim.time();
    for _ in 0..50 {
        let t = sim.step().unwrap().unwrap().sample.time;
        assert!(t - last <= 1e-6 * (1.0 + 1e-9));
        last = t;
    }
    assert!(sim.set_max_step(0.0).is_err());
}
