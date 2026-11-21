//! Mixed-signal tests: logic parts driving and reading the analog circuit.

use dunbrec_core::{ComponentKind as K, GateOp, ModelLibrary, NetlistBuilder, NodeId, Waveform};

use crate::digital::CMOS_5V;
use crate::{
    Circuit, Edge, Transient, TransientOptions, TransientResult, Watch, dc_sweep, linear_steps,
    operating_point, transient,
};

const DELAY: f64 = 10e-9;

fn inv() -> K {
    K::Inverter { delay: DELAY }
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
fn clock(f: f64) -> K {
    K::Clock {
        frequency: f,
        duty: 0.5,
    }
}

fn circuit(b: NetlistBuilder) -> Circuit {
    Circuit::from_netlist(&b.build(), &ModelLibrary::new()).unwrap()
}

fn last(res: &TransientResult, node: &str) -> f64 {
    *res.voltage(node).unwrap().last().unwrap()
}

#[test]
fn inverter_switches_at_the_input_threshold_after_its_delay() {
    // RC charging from 0 to 5 V (τ = 1 ms) into an inverter.
    let b = NetlistBuilder::new()
        .element(
            "V1",
            K::VoltageSource {
                waveform: Waveform::Pulse {
                    v1: 0.0,
                    v2: 5.0,
                    delay: 0.0,
                    rise: 1e-9,
                    fall: 1e-9,
                    width: 1.0,
                    period: 0.0,
                },
            },
            &["src", "0"],
        )
        .element("R1", r(1e3), &["src", "in"])
        .element("C1", c(1e-6), &["in", "0"])
        .element("U1", inv(), &["in", "out"])
        .element("RL", r(10e3), &["out", "0"]);
    let netlist = b.build();
    let out = netlist.node_by_name("out").unwrap();
    let mut sim = Transient::new(
        Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap(),
        TransientOptions::new(3e-3),
    )
    .unwrap();
    // Input low: output high, slightly loaded by RL through the 50 Ω output.
    let v0 = sim.initial_point().voltage(out);
    assert!((v0 - 5.0 * 10e3 / (10e3 + 50.0)).abs() < 1e-6, "{v0}");
    let input = netlist.node_by_name("in").unwrap();
    let w_in = sim.add_watch(Watch {
        p: input,
        n: NodeId::GROUND,
        level: 3.5,
        edge: Edge::Rising,
    });
    let w_out = sim.add_watch(Watch {
        p: out,
        n: NodeId::GROUND,
        level: 2.5,
        edge: Edge::Falling,
    });
    let res = crate::transient::run(sim).unwrap();
    let when = |w| res.crossings.iter().find(|c| c.watch == w).unwrap().time;
    // The input crosses VIH = 3.5 V at τ·ln(5/1.5) (to analog accuracy)...
    let t_in = when(w_in);
    assert!((t_in - 1e-3 * (5.0f64 / 1.5).ln()).abs() < 1e-6, "{t_in}");
    // ...and the output is half-way down one delay plus half an edge later.
    let expected = t_in + DELAY + CMOS_5V.edge / 2.0;
    assert!(
        (when(w_out) - expected).abs() < 1e-10,
        "{} vs {expected}",
        when(w_out)
    );
    assert!(last(&res, "out") < 0.01);
}

#[test]
fn clock_drives_a_counter() {
    // 1 kHz clock: rising edges at 0.5 ms, 1.5 ms, ... — 11 by 10.6 ms.
    let res = transient(
        circuit(
            NetlistBuilder::new()
                .element("CLK1", clock(1e3), &["clk"])
                .element(
                    "U1",
                    K::Counter4 { delay: DELAY },
                    &["clk", "q0", "q1", "q2", "q3"],
                ),
        ),
        TransientOptions::new(10.6e-3),
    )
    .unwrap();
    let bits: Vec<bool> = ["q0", "q1", "q2", "q3"]
        .iter()
        .map(|n| last(&res, n) > 2.5)
        .collect();
    assert_eq!(bits, vec![true, true, false, true], "11 = 0b1011");
}

#[test]
fn flip_flop_divides_the_clock_by_two() {
    let res = transient(
        circuit(
            NetlistBuilder::new()
                .element("CLK1", clock(1e3), &["clk"])
                .element(
                    "U1",
                    K::DFlipFlop { delay: DELAY },
                    &["qn", "clk", "q", "qn"],
                ),
        ),
        TransientOptions::new(5e-3),
    )
    .unwrap();
    // Q toggles on each rising edge (0.5, 1.5, 2.5, 3.5, 4.5 ms).
    let q = res.voltage("q").unwrap();
    for (k, t) in [1.0e-3, 2.0e-3, 3.0e-3, 4.0e-3, 4.9e-3].iter().enumerate() {
        let v = res.at(q, *t);
        let expected_high = k % 2 == 0;
        assert_eq!(v > 2.5, expected_high, "Q at {t} = {v}");
    }
}

#[test]
fn ring_oscillator_oscillates_at_the_predicted_period() {
    // Three inverters in a loop have no steady state. Each stage takes its
    // delay plus the time its input needs to ramp through the threshold
    // (70 % of an edge), so a full period is 2 × 3 × (10 ns + 3.5 ns).
    let netlist = NetlistBuilder::new()
        .element("U1", inv(), &["a", "b"])
        .element("U2", inv(), &["b", "c"])
        .element("U3", inv(), &["c", "a"])
        .build();
    let a = netlist.node_by_name("a").unwrap();
    let mut sim = Transient::new(
        Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap(),
        TransientOptions::new(1e-6).with_max_step(2e-9),
    )
    .unwrap();
    sim.add_watch(Watch {
        p: a,
        n: NodeId::GROUND,
        level: 2.5,
        edge: Edge::Rising,
    });
    let res = crate::transient::run(sim).unwrap();
    let rises: Vec<f64> = res.crossings.iter().map(|c| c.time).collect();
    assert!(rises.len() > 8, "{} rising edges", rises.len());
    let period = (rises[rises.len() - 1] - rises[1]) / (rises.len() - 2) as f64;
    let expected = 6.0 * (DELAY + 0.7 * CMOS_5V.edge);
    assert!(
        (period - expected).abs() / expected < 0.05,
        "period {period:e} vs {expected:e}"
    );
}

#[test]
fn logic_drives_an_analog_filter() {
    // 10 kHz square wave into a 1 ms RC: the output settles at the average.
    let res = transient(
        circuit(
            NetlistBuilder::new()
                .element("CLK1", clock(10e3), &["clk"])
                .element("R1", r(10e3), &["clk", "out"])
                .element("C1", c(100e-9), &["out", "0"]),
        ),
        TransientOptions::new(10e-3).with_max_step(5e-6),
    )
    .unwrap();
    let out = res.voltage("out").unwrap();
    let late: Vec<f64> = res
        .times
        .iter()
        .zip(out)
        .filter(|(t, _)| **t > 8e-3)
        .map(|(_, v)| *v)
        .collect();
    let mean = late.iter().sum::<f64>() / late.len() as f64;
    assert!((mean - 2.5).abs() < 0.1, "mean {mean}");
}

#[test]
fn operating_point_settles_logic_and_gates_work() {
    let build = |a: bool, b: bool| {
        NetlistBuilder::new()
            .element("SWA", K::LogicSwitch { high: a }, &["a"])
            .element("SWB", K::LogicSwitch { high: b }, &["b"])
            .element(
                "U1",
                K::Gate {
                    op: GateOp::Nand,
                    delay: DELAY,
                },
                &["a", "b", "nand"],
            )
            .element(
                "U2",
                K::Gate {
                    op: GateOp::Xor,
                    delay: DELAY,
                },
                &["a", "b", "xor"],
            )
            .element("U3", inv(), &["nand", "and"])
            .element("LED1", K::LogicLed, &["and"])
            .element("RL", r(1e3), &["and", "0"])
    };
    for (a, b) in [(false, false), (false, true), (true, false), (true, true)] {
        let op = operating_point(&circuit(build(a, b))).unwrap();
        let level = |n: &str| op.voltage_by_name(n).unwrap() > 2.5;
        assert_eq!(level("nand"), !(a && b), "NAND {a} {b}");
        assert_eq!(level("xor"), a != b, "XOR {a} {b}");
        assert_eq!(level("and"), a && b, "AND via inverter {a} {b}");
    }
}

#[test]
fn flipping_a_logic_switch_while_running() {
    let netlist = NetlistBuilder::new()
        .element("SW1", K::LogicSwitch { high: false }, &["in"])
        .element("U1", inv(), &["in", "out"])
        .build();
    let models = ModelLibrary::new();
    let mut sim = Transient::new(
        Circuit::from_netlist(&netlist, &models).unwrap(),
        TransientOptions::free_running(1e-6),
    )
    .unwrap();
    let out = netlist.node_by_name("out").unwrap().0;
    while sim.time() < 1e-5 {
        sim.step().unwrap();
    }
    assert!(sim.sample().voltages[out] > 4.9);
    let mut flipped = netlist.elements[0].clone();
    flipped.kind = K::LogicSwitch { high: true };
    assert!(sim.update_element(0, &flipped, &models).unwrap());
    let t0 = sim.time();
    while sim.time() < t0 + 1e-6 {
        sim.step().unwrap();
    }
    assert!(
        sim.sample().voltages[out] < 0.1,
        "{}",
        sim.sample().voltages[out]
    );
}

#[test]
fn sweeps_work_with_logic_present() {
    // An analog input swept through an inverter: a clean transfer curve
    // switching at the input threshold midpoint (2.5 V at the operating
    // point, where there is no hysteresis history).
    let netlist = NetlistBuilder::new()
        .element(
            "VIN",
            K::VoltageSource {
                waveform: Waveform::Dc(0.0),
            },
            &["in", "0"],
        )
        .element("U1", inv(), &["in", "out"])
        .build();
    let vin = linear_steps(0.0, 5.0, 11);
    let ops = dc_sweep(&netlist, &ModelLibrary::new(), "VIN", &vin).unwrap();
    let out: Vec<bool> = ops
        .iter()
        .map(|o| o.voltage_by_name("out").unwrap() > 2.5)
        .collect();
    assert_eq!(
        out,
        vec![
            true, true, true, true, true, false, false, false, false, false, false
        ]
    );
}
