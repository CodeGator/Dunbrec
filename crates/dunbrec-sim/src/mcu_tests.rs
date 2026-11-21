//! Microcontroller co-simulation tests: real firmware driving and reading
//! analog circuits.

use dunbrec_core::{ComponentKind as K, ModelLibrary, NetlistBuilder, NodeId, Waveform};
use dunbrec_mcu::asm::{Asm, io, mem};

use crate::mcu::{MCU_R_OUT, MCU_R_PULLUP};
use crate::transient::run;
use crate::{
    Circuit, HaltReason, McuHalt, SimError, Transient, TransientOptions, TransientResult,
    operating_point, transient,
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
fn dc(v: f64) -> K {
    K::VoltageSource {
        waveform: Waveform::Dc(v),
    }
}
fn mcu(firmware: &str) -> K {
    K::Mcu {
        firmware: firmware.into(),
        clock: 16e6,
    }
}

/// MCU pin nets: "D0".."D13", "A0".."A5".
fn pins() -> Vec<String> {
    dunbrec_mcu::ARDUINO_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn with_mcu(b: NetlistBuilder, firmware: &str) -> NetlistBuilder {
    let names = pins();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    b.element("U1", mcu(firmware), &refs)
}

fn circuit(b: NetlistBuilder) -> Circuit {
    Circuit::from_netlist(&b.build(), &ModelLibrary::new()).unwrap()
}

/// A circuit whose MCU runs an assembled program from a temporary .hex
/// file, found through the firmware folder.
fn circuit_with_program(b: NetlistBuilder, program: &Asm, tag: &str) -> Circuit {
    let dir = std::env::temp_dir().join(format!("dunbrec-mcu-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let image = program.finish().unwrap();
    std::fs::write(dir.join("fw.hex"), dunbrec_mcu::hex::write(&image)).unwrap();
    let mut circuit = circuit(with_mcu(b, "fw.hex"));
    circuit.set_firmware_dir(&dir);
    circuit
}

/// Times at which `node` crosses `level` upwards.
fn rising(res: &TransientResult, node: &str, level: f64) -> Vec<f64> {
    let v = res.voltage(node).unwrap();
    (1..v.len())
        .filter(|&i| v[i - 1] < level && v[i] >= level)
        .map(|i| {
            let (t0, t1) = (res.times[i - 1], res.times[i]);
            t0 + (level - v[i - 1]) / (v[i] - v[i - 1]) * (t1 - t0)
        })
        .collect()
}

#[test]
fn blink_drives_a_load_at_the_right_times() {
    // D13 → 1 kΩ → ground: high level 5 V × 1000 / 1025.
    let b = NetlistBuilder::new().element("R1", r(1e3), &["D13", "0"]);
    let res = transient(
        circuit(with_mcu(b, "builtin:blink")),
        TransientOptions::new(1.6),
    )
    .unwrap();
    let ups = rising(&res, "D13", 2.5);
    assert_eq!(ups.len(), 2, "{ups:?}");
    // Timer 1 matches every 31250 × 256 cycles = 0.5 s.
    assert!((ups[0] - 0.5).abs() < 1e-6, "{}", ups[0]);
    assert!((ups[1] - 1.5).abs() < 1e-6, "{}", ups[1]);
    let v_high = res.at(res.voltage("D13").unwrap(), 0.75);
    assert!(
        (v_high - 5.0 * 1e3 / (1e3 + MCU_R_OUT)).abs() < 1e-3,
        "{v_high}"
    );
    let i = res.at(res.current("R1").unwrap(), 0.75);
    assert!((i - v_high / 1e3).abs() < 1e-6);
    assert!(res.at(res.voltage("D13").unwrap(), 1.25).abs() < 1e-3);
}

#[test]
fn pwm_through_an_rc_filter_and_adc_with_serial() {
    // 2.5 V on A0 → ADC 512 → OCR0A = 128 → duty 129/256 on D6, filtered
    // by 10 kΩ / 10 µF (τ = 0.1 s, ripple ≈ 1 mV).
    let b = NetlistBuilder::new()
        .element("V1", dc(2.5), &["A0", "0"])
        .element("R1", r(10e3), &["D6", "f"])
        .element("C1", c(10e-6), &["f", "0"]);
    let res = transient(
        circuit(with_mcu(b, "builtin:pot-pwm-serial")),
        TransientOptions::new(0.8),
    )
    .unwrap();
    let vf = *res.voltage("f").unwrap().last().unwrap();
    let expect = 5.0 * 129.0 / 256.0;
    assert!((vf - expect).abs() < 0.03, "{vf} vs {expect}");
    let text: String = res.serial.iter().map(|b| b.byte as char).collect();
    assert!(
        text.starts_with("Dunbrec AVR demo\r\nA0 = 0512\r\n"),
        "{text:?}"
    );
    assert!(text.matches("A0 = 0512").count() >= 6);
    // Serial timing: 10 bits at 9600 baud apart, back to back.
    let gap = res.serial[1].time - res.serial[0].time;
    assert!((gap - 10.0 * 1664.0 / 16e6).abs() < 1e-7, "{gap}");
}

#[test]
fn adc_follows_a_changing_voltage() {
    // A0 ramps 0 → 5 V over 1 s; the readings printed every 100 ms rise.
    let b = NetlistBuilder::new().element(
        "V1",
        K::VoltageSource {
            waveform: Waveform::Pwl(vec![(0.0, 0.0), (1.0, 5.0)]),
        },
        &["A0", "0"],
    );
    let res = transient(
        circuit(with_mcu(b, "builtin:pot-pwm-serial")),
        TransientOptions::new(1.0).with_max_step(1e-3),
    )
    .unwrap();
    let text: String = res.serial.iter().map(|b| b.byte as char).collect();
    let readings: Vec<u32> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("A0 = "))
        .map(|v| v.parse().unwrap())
        .collect();
    assert!(readings.len() >= 8, "{text:?}");
    assert!(readings.windows(2).all(|w| w[1] > w[0]), "{readings:?}");
    // The k-th reading is taken just after k × 100 ms: about k × 102.
    for (k, &v) in readings.iter().enumerate() {
        let expect = (k + 1) as f64 * 0.1 / 1.0 * 1024.0;
        assert!(
            (v as f64 - expect).abs() < 6.0,
            "reading {k}: {v} vs {expect}"
        );
    }
}

/// D2 copied to D8 in a tight loop.
fn follower() -> Asm {
    let mut a = Asm::new();
    a.sbi(io::DDRB, 0);
    a.label("loop");
    a.sbic(io::PIND, 2).sbi(io::PORTB, 0);
    a.sbis(io::PIND, 2).cbi(io::PORTB, 0);
    a.rjmp("loop");
    a
}

#[test]
fn inputs_reach_the_firmware_within_a_few_cycles() {
    // A 1 kHz square wave into D2; the firmware copies it to D8.
    let b = NetlistBuilder::new()
        .element(
            "V1",
            K::VoltageSource {
                waveform: Waveform::Pulse {
                    v1: 0.0,
                    v2: 5.0,
                    delay: 0.2e-3,
                    rise: 1e-6,
                    fall: 1e-6,
                    width: 0.5e-3,
                    period: 1e-3,
                },
            },
            &["D2", "0"],
        )
        .element("R1", r(10e3), &["D8", "0"]);
    let res = transient(
        circuit_with_program(b, &follower(), "follow"),
        TransientOptions::new(5e-3),
    )
    .unwrap();
    let ins = rising(&res, "D2", 3.0);
    let outs = rising(&res, "D8", 2.5);
    assert_eq!(ins.len(), 5);
    assert_eq!(outs.len(), 5, "{outs:?}");
    for (i, o) in ins.iter().zip(&outs) {
        // Loop of 8 cycles at most, plus the 5 ns output edge.
        let lag = o - i;
        assert!(lag > 0.0 && lag < 0.8e-6, "lag {lag}");
    }
}

#[test]
fn interrupt_driven_response_and_pull_ups() {
    // INT0 on falling edges toggles D9; D4 has its pull-up enabled into
    // 10 kΩ to ground.
    let mut a = Asm::new();
    a.vectors(&[(0, "reset"), (1, "int0")], "reset");
    a.label("reset").init_stack();
    a.sbi(io::DDRB, 1);
    a.sbi(io::PORTD, 4);
    a.ldi(16, 0x02).sts(mem::EICRA, 16);
    a.ldi(16, 0x01).out(io::EIMSK, 16);
    a.ldi(16, 0x01).out(io::SMCR, 16);
    a.sei();
    a.label("idle").sleep().rjmp("idle");
    a.label("int0").sbi(io::PINB, 1).reti();
    let b = NetlistBuilder::new()
        .element(
            "V1",
            K::VoltageSource {
                waveform: Waveform::Pulse {
                    v1: 5.0,
                    v2: 0.0,
                    delay: 1e-3,
                    rise: 1e-6,
                    fall: 1e-6,
                    width: 1e-3,
                    period: 2e-3,
                },
            },
            &["D2", "0"],
        )
        .element("R1", r(10e3), &["D9", "0"])
        .element("R2", r(10e3), &["D4", "0"]);
    let res = transient(
        circuit_with_program(b, &a, "int0"),
        TransientOptions::new(6.5e-3),
    )
    .unwrap();
    // Falling edges at 1, 3, 5 ms (crossing 1.5 V a little later).
    let toggles = rising(&res, "D9", 2.5);
    assert_eq!(toggles.len(), 2, "{toggles:?}");
    assert!((toggles[0] - 1e-3).abs() < 2e-6, "{}", toggles[0]);
    assert!((toggles[1] - 5e-3).abs() < 2e-6, "{}", toggles[1]);
    let v4 = *res.voltage("D4").unwrap().last().unwrap();
    let expect = 5.0 * 10e3 / (10e3 + MCU_R_PULLUP);
    assert!((v4 - expect).abs() < 1e-3, "{v4} vs {expect}");
}

#[test]
fn operating_point_sees_floating_pins() {
    let b = NetlistBuilder::new()
        .element("V1", dc(5.0), &["vcc", "0"])
        .element("R1", r(10e3), &["vcc", "D3"]);
    let op = operating_point(&circuit(with_mcu(b, "builtin:blink"))).unwrap();
    assert!((op.voltage_by_name("D3").unwrap() - 5.0).abs() < 1e-3);
}

#[test]
fn firmware_problems_are_reported() {
    let load = || NetlistBuilder::new().element("R1", r(1e3), &["D13", "0"]);
    let err = Transient::new(
        circuit(with_mcu(load(), "nowhere/missing.hex")),
        TransientOptions::new(1e-3),
    )
    .err()
    .unwrap();
    assert!(
        matches!(&err, SimError::Firmware { designator, message }
            if designator == "U1" && message.contains("cannot read")),
        "{err}"
    );

    // A program that runs into empty flash stops the simulation.
    let mut a = Asm::new();
    a.nop().nop();
    let sim = Transient::new(
        circuit_with_program(load(), &a, "crash"),
        TransientOptions::new(1e-3),
    )
    .unwrap();
    let err = run(sim).unwrap_err();
    assert!(err.to_string().contains("empty flash"), "{err}");
}

#[test]
fn live_edits_of_the_mcu_need_a_restart() {
    let netlist = with_mcu(
        NetlistBuilder::new().element("R1", r(1e3), &["D13", "0"]),
        "builtin:blink",
    )
    .build();
    let models = ModelLibrary::new();
    let circuit = Circuit::from_netlist(&netlist, &models).unwrap();
    let mut sim = Transient::new(circuit, TransientOptions::free_running(1e-3)).unwrap();
    for _ in 0..50 {
        sim.step().unwrap();
    }
    let u1 = netlist
        .elements
        .iter()
        .position(|e| e.designator == "U1")
        .unwrap();
    assert!(
        sim.update_element(u1, &netlist.elements[u1], &models)
            .unwrap()
    );
    let mut changed = netlist.elements[u1].clone();
    changed.kind = mcu("builtin:fade");
    assert!(!sim.update_element(u1, &changed, &models).unwrap());
    // Ordinary parts still update live alongside an MCU.
    let mut r1 = netlist.elements[0].clone();
    r1.kind = r(2e3);
    assert!(sim.update_element(0, &r1, &models).unwrap());
}

// ---------------------------------------------------------- debugging ---

/// Step the simulation until a microcontroller halts for the debugger.
fn until_halt(sim: &mut Transient) -> (McuHalt, f64) {
    for _ in 0..200_000 {
        sim.step().unwrap().expect("simulation finished");
        if let Some(halt) = sim.take_mcu_halt() {
            return (halt, sim.time());
        }
    }
    panic!("no halt");
}

fn node_index(sim: &Transient, name: &str) -> usize {
    (0..sim.circuit().node_count())
        .find(|&i| sim.circuit().node_name(NodeId(i)) == name)
        .unwrap()
}

/// D13 high for four cycles of an eight-cycle loop.
fn blinker() -> Asm {
    let mut a = Asm::new();
    a.sbi(io::DDRB, 5);
    a.label("loop");
    a.sbi(io::PORTB, 5).nop().nop();
    a.label("late");
    a.cbi(io::PORTB, 5).rjmp("loop");
    a
}

#[test]
fn a_breakpoint_halts_the_whole_simulation_at_its_exact_time() {
    let a = blinker();
    let late = a.label_address("late").unwrap();
    let circuit = circuit_with_program(NetlistBuilder::new(), &a, "bp");
    let mut sim = Transient::new(circuit, TransientOptions::free_running(1e-6)).unwrap();
    sim.set_breakpoints(0, &[late]);

    let (halt, t) = until_halt(&mut sim);
    assert_eq!(
        halt,
        McuHalt {
            part: 0,
            reason: HaltReason::Breakpoint
        }
    );
    let snap = sim.mcu_snapshot(0).unwrap();
    assert_eq!(snap.pc, late, "stopped before the breakpoint's instruction");
    // SBI DDRB (2) + SBI PORTB (2) + two NOPs: cycle 6. The simulated time
    // is that cycle, to within half a cycle.
    assert_eq!(snap.cycle, 6);
    assert!((t - 6.0 / 16e6).abs() < 0.5 / 16e6, "t = {t}");
    // The circuit is at the same moment: D13 has been high for two cycles.
    let d13 = node_index(&sim, "D13");
    assert!(sim.sample().voltages[d13] > 4.9);

    // Resuming runs on to the next pass: one loop of 8 cycles later.
    let (halt, t2) = until_halt(&mut sim);
    assert_eq!(halt.reason, HaltReason::Breakpoint);
    assert!((t2 - t - 8.0 / 16e6).abs() < 1e-9, "{}", t2 - t);
    assert_eq!(sim.mcu_snapshot(0).unwrap().pc, late);
    assert!(sim.sample().voltages[d13] > 4.9);
}

#[test]
fn single_steps_advance_the_chip_and_the_circuit_together() {
    let a = blinker();
    let circuit = circuit_with_program(NetlistBuilder::new(), &a, "step");
    let mut sim = Transient::new(circuit, TransientOptions::free_running(1e-6)).unwrap();
    // Cost of each instruction in turn: SBI 2, SBI 2, NOP 1, NOP 1, CBI 2,
    // RJMP 2, then round again: SBI 2, NOP 1.
    let mut cycle = 0u64;
    for (n, cost) in [2u64, 2, 1, 1, 2, 2, 2, 1].into_iter().enumerate() {
        sim.step_mcu(0);
        let (halt, t) = until_halt(&mut sim);
        assert_eq!(halt.reason, HaltReason::Stepped, "step {n}");
        cycle += cost;
        let snap = sim.mcu_snapshot(0).unwrap();
        assert_eq!(snap.cycle, cycle, "step {n}");
        assert!(
            (t - cycle as f64 / 16e6).abs() < 0.5 / 16e6,
            "step {n}: t = {t}"
        );
    }
}

#[test]
fn an_input_edge_during_a_step_does_not_make_it_run_two_instructions() {
    // Twenty two-cycle instructions (125 ns each) while D2 rises at various
    // moments. Three cases: the edge lands in the first instruction's first
    // half (the step's result is adopted at once), exactly at an instruction
    // boundary (nothing to replay), and in the second half of the second
    // instruction, where the chip must be replayed over the instruction in
    // flight to reach the edge. Every step must still be exactly one
    // instruction.
    for edge in [100e-9, 130e-9, 170e-9] {
        let mut a = Asm::new();
        for _ in 0..20 {
            a.sbiw(24, 1);
        }
        a.label("end").rjmp("end");
        let b = NetlistBuilder::new().element(
            "V1",
            K::VoltageSource {
                waveform: Waveform::Pulse {
                    v1: 0.0,
                    v2: 5.0,
                    delay: edge,
                    rise: 1e-9,
                    fall: 1e-9,
                    width: 1.0,
                    period: 0.0,
                },
            },
            &["D2", "0"],
        );
        let circuit = circuit_with_program(b, &a, "edge");
        let mut sim = Transient::new(circuit, TransientOptions::free_running(1e-6)).unwrap();
        for n in 1..=10u64 {
            sim.step_mcu(0);
            let (halt, _) = until_halt(&mut sim);
            assert_eq!(halt.reason, HaltReason::Stepped);
            let snap = sim.mcu_snapshot(0).unwrap();
            assert_eq!(
                (snap.pc as u64, snap.cycle),
                (n, 2 * n),
                "edge at {edge}, step {n}"
            );
        }
    }
}
