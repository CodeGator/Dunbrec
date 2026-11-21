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
        Example {
            name: "Logic gates",
            build: logic_gates,
        },
        Example {
            name: "Binary counter",
            build: binary_counter,
        },
        Example {
            name: "Arduino blink",
            build: arduino_blink,
        },
        Example {
            name: "Arduino PWM fade",
            build: arduino_fade,
        },
        Example {
            name: "Arduino pot, PWM and serial",
            build: arduino_pot_serial,
        },
        Example {
            name: "Op-amp inverting amplifier",
            build: opamp_inverting,
        },
        Example {
            name: "555 LED blinker",
            build: timer555_blinker,
        },
        Example {
            name: "555 one-shot",
            build: timer555_one_shot,
        },
    ]
}

/// DC supply `volts` at (x, -6) with its - pin (or + pin, if `negative`)
/// grounded below and the other pin wired up to a `label` at (x, -10).
fn supply(s: &mut Schematic, x: i32, volts: f64, negative: bool, label: &str) {
    let rotation = if negative {
        Rotation::R180
    } else {
        Rotation::R0
    };
    let source = K::VoltageSource {
        waveform: Waveform::Dc(volts),
    };
    place(s, source, x, -6, rotation); // (x,-8) and (x,-4)
    place(s, K::Ground, x, -3, Rotation::R0);
    wire(s, &[(x, -4), (x, -3)]);
    // Longer than the stubs on the op-amp, so the voltage tag shows here.
    wire(s, &[(x, -8), (x, -10)]);
    s.add_label(label, Point::new(x, -10));
}

/// A 741 with gain -10 (10 kΩ in, 100 kΩ feedback) on ±15 V: the 0.5 V,
/// 1 kHz input comes out inverted at 5 V. Scope VIN and VOUT; raise the
/// input past 1.4 V to see the output clip near the supplies.
pub fn opamp_inverting() -> Schematic {
    let mut s = Schematic::new();
    let opamp = K::OpAmp {
        part: "uA741".into(),
    };
    // in+ (-3,1), in- (-3,-1), V+ (0,-3), V- (0,3), out (3,0).
    place(&mut s, opamp, 0, 0, Rotation::R0);
    place(&mut s, sine(0.5, 1e3), -12, 1, Rotation::R0); // (-12,-1) (-12,3)
    place(&mut s, K::Ground, -12, 4, Rotation::R0);
    place(&mut s, r(10e3), -7, -1, Rotation::R0); // (-9,-1) (-5,-1)
    place(&mut s, r(100e3), 0, -6, Rotation::R0); // (-2,-6) (2,-6)
    place(&mut s, K::Ground, -3, 2, Rotation::R0);
    place(&mut s, r(10e3), 6, 3, Rotation::R90); // (6,1) (6,5)
    place(&mut s, K::Ground, 6, 6, Rotation::R0);
    wire(&mut s, &[(-12, -1), (-9, -1)]);
    wire(&mut s, &[(-12, 3), (-12, 4)]);
    wire(&mut s, &[(-5, -1), (-3, -1)]);
    wire(&mut s, &[(-3, 1), (-3, 2)]);
    // Feedback from the output over the top to the inverting input.
    wire(&mut s, &[(-4, -1), (-4, -6), (-2, -6)]);
    wire(&mut s, &[(2, -6), (5, -6), (5, 0)]);
    wire(&mut s, &[(3, 0), (6, 0), (6, 1)]);
    wire(&mut s, &[(6, 5), (6, 6)]);
    // Supplies by name: short stubs on V+ and V-.
    wire(&mut s, &[(0, -3), (0, -4)]);
    wire(&mut s, &[(0, 3), (0, 4)]);
    s.add_label("+15V", Point::new(0, -4));
    s.add_label("-15V", Point::new(0, 4));
    supply(&mut s, 12, 15.0, false, "+15V");
    supply(&mut s, 19, 15.0, true, "-15V");
    s.add_label("VIN", Point::new(-10, -1));
    s.add_label("VOUT", Point::new(4, 0));
    s
}

/// 555 pins with the chip at the origin: GND (-1,6), TRIG (-5,2),
/// OUT (5,0), RESET (1,-6), CTRL (1,6), THR (-5,0), DIS (-5,-2), VCC (-1,-6).
/// Places the chip, a 9 V supply at x = -16, a VCC rail at y = -8 and a
/// ground rail at y = 10 (both reaching `right`), VCC and RESET tied to the
/// rail, the 10 nF CTRL decoupling capacitor, and a logic LED on OUT.
fn timer555_frame(s: &mut Schematic, right: i32) {
    place(s, K::Timer555, 0, 0, Rotation::R0);
    let source = K::VoltageSource {
        waveform: Waveform::Dc(9.0),
    };
    place(s, source, -16, 1, Rotation::R0); // (-16,-1) (-16,3)
    place(s, K::Ground, -16, 11, Rotation::R0);
    wire(s, &[(-16, -1), (-16, -8), (right, -8)]);
    wire(s, &[(-16, 3), (-16, 10), (right, 10)]);
    wire(s, &[(-16, 10), (-16, 11)]);
    wire(s, &[(-1, -6), (-1, -8)]);
    wire(s, &[(1, -6), (1, -8)]);
    wire(s, &[(-1, 6), (-1, 10)]);
    place(s, c(10e-9), 1, 8, Rotation::R90); // (1,6) (1,10) on the rail
    place(s, K::LogicLed, 10, 0, Rotation::R0); // input (7,0)
    wire(s, &[(5, 0), (7, 0)]);
    s.add_label("OUT", Point::new(6, 0));
}

/// A 555 astable: RA = 1 kΩ, RB = 68 kΩ, C = 10 µF blink the LED at about
/// 1 Hz (f = 1.44 / ((RA + 2 RB) C)), high 50.4 % of the time. Run at real
/// time to watch it; scope TIMING to see the capacitor swing between 3 V
/// and 6 V (1/3 and 2/3 of the supply).
pub fn timer555_blinker() -> Schematic {
    let mut s = Schematic::new();
    timer555_frame(&mut s, 2);
    place(&mut s, r(1e3), -10, -5, Rotation::R90); // RA (-10,-7) (-10,-3)
    place(&mut s, r(68e3), -10, 1, Rotation::R90); // RB (-10,-1) (-10,3)
    let timing = K::Capacitor {
        capacitance: 10e-6,
        initial_voltage: Some(0.0),
    };
    place(&mut s, timing, -6, 8, Rotation::R90); // (-6,6) (-6,10)
    wire(&mut s, &[(-10, -8), (-10, -7)]);
    wire(&mut s, &[(-10, -3), (-10, -2), (-5, -2)]);
    wire(&mut s, &[(-10, -2), (-10, -1)]);
    // THR and TRIG both watch the capacitor.
    wire(&mut s, &[(-10, 3), (-10, 4), (-6, 4)]);
    wire(&mut s, &[(-5, 0), (-6, 0), (-6, 6)]);
    wire(&mut s, &[(-5, 2), (-6, 2)]);
    s.add_label("DIS", Point::new(-8, -2));
    s.add_label("TIMING", Point::new(-6, 5));
    s
}

/// A 555 monostable: pressing the push button pulls TRIG low and the output
/// goes high for 1.1 RC = 1.1 s (100 kΩ, 10 µF), then low again. Click the
/// button, or hold it: held down, the output stays high until it is
/// released.
pub fn timer555_one_shot() -> Schematic {
    let mut s = Schematic::new();
    timer555_frame(&mut s, 16);
    // Timing resistor from VCC; DIS and THR on the capacitor.
    place(&mut s, r(100e3), -10, -5, Rotation::R90); // (-10,-7) (-10,-3)
    place(&mut s, c(10e-6), -10, 8, Rotation::R90); // (-10,6) (-10,10)
    wire(&mut s, &[(-10, -8), (-10, -7)]);
    wire(&mut s, &[(-10, -3), (-10, 6)]);
    wire(&mut s, &[(-5, -2), (-10, -2)]);
    wire(&mut s, &[(-5, 0), (-10, 0)]);
    // TRIG: 10 kΩ pull-up and a switch to ground, joined by the TRIG label.
    wire(&mut s, &[(-5, 2), (-7, 2)]);
    place(&mut s, r(10e3), 16, -5, Rotation::R90); // (16,-7) (16,-3)
    let button = K::Switch {
        closed: false,
        r_on: 0.1,
        r_off: 1e9,
        momentary: true,
    };
    place(&mut s, button, 16, 1, Rotation::R90); // (16,-1) (16,3)
    wire(&mut s, &[(16, -8), (16, -7)]);
    wire(&mut s, &[(16, -3), (16, -1)]);
    wire(&mut s, &[(16, 3), (16, 10)]);
    s.add_label("TRIG", Point::new(-6, 2));
    s.add_label("TRIG", Point::new(16, -2));
    s.add_label("TIMING", Point::new(-8, 0));
    s
}

fn mcu(program: &str) -> K {
    K::Mcu {
        firmware: format!("{}{program}", dunbrec_mcu::firmware::BUILTIN_PREFIX),
        clock: dunbrec_core::DEFAULT_MCU_CLOCK,
    }
}

/// D6 → 10 kΩ → 10 µF to ground: averages the PWM into a voltage.
/// Pin D6 of an MCU at the origin is at (-6, 3).
fn pwm_filter(s: &mut Schematic) {
    place(s, r(10e3), -10, 3, Rotation::R0); // (-12,3) (-8,3)
    place(s, c(10e-6), -14, 5, Rotation::R90); // (-14,3) (-14,7)
    place(s, K::Ground, -14, 8, Rotation::R0);
    wire(s, &[(-8, 3), (-6, 3)]);
    wire(s, &[(-12, 3), (-14, 3)]);
    wire(s, &[(-14, 7), (-14, 8)]);
    s.add_label("PWM", Point::new(-7, 3));
    s.add_label("VOUT", Point::new(-13, 3));
}

/// An ATmega328P running the built-in blink program: D13 toggles every
/// 0.5 s and lights the LED. Run at real-time speed to watch it.
pub fn arduino_blink() -> Schematic {
    let mut s = Schematic::new();
    place(&mut s, mcu("blink"), 0, 0, Rotation::R0); // D13 (6,-3)
    place(&mut s, K::LogicLed, 12, -3, Rotation::R0); // input (9,-3)
    wire(&mut s, &[(6, -3), (9, -3)]);
    s.add_label("D13", Point::new(7, -3));
    s
}

/// PWM on D6 fading up and down, smoothed by an RC filter: scope VOUT
/// to see the voltage ramp, PWM to see the duty cycle change.
pub fn arduino_fade() -> Schematic {
    let mut s = Schematic::new();
    place(&mut s, mcu("fade"), 0, 0, Rotation::R0);
    pwm_filter(&mut s);
    s
}

/// A potentiometer into A0; the firmware mirrors it as PWM on D6
/// (filtered to VOUT) and prints the reading on the serial monitor.
/// Drag the wiper while it runs.
pub fn arduino_pot_serial() -> Schematic {
    let mut s = Schematic::new();
    place(&mut s, mcu("pot-pwm-serial"), 0, 0, Rotation::R0); // A0 (6,-1)
    place(
        &mut s,
        K::Potentiometer {
            resistance: 10e3,
            wiper: 0.5,
        },
        12,
        3,
        Rotation::R270,
    ); // A (12,5) B (12,1) W (10,3)
    place(
        &mut s,
        K::VoltageSource {
            waveform: Waveform::Dc(5.0),
        },
        18,
        3,
        Rotation::R0,
    ); // + (18,1) - (18,5)
    place(&mut s, K::Ground, 18, 6, Rotation::R0);
    wire(&mut s, &[(10, 3), (8, 3), (8, -1), (6, -1)]);
    wire(&mut s, &[(12, 1), (18, 1)]);
    wire(&mut s, &[(12, 5), (18, 5)]);
    wire(&mut s, &[(18, 5), (18, 6)]);
    s.add_label("POT", Point::new(8, 1));
    pwm_filter(&mut s);
    s
}

fn logic_gate(op: dunbrec_core::GateOp) -> K {
    K::Gate {
        op,
        delay: dunbrec_core::DEFAULT_LOGIC_DELAY,
    }
}

/// Two logic switches into AND, OR and XOR gates, each lighting an LED.
/// Double-click a switch to flip it.
pub fn logic_gates() -> Schematic {
    use dunbrec_core::GateOp;
    let mut s = Schematic::new();
    place(&mut s, K::LogicSwitch { high: true }, -10, -4, Rotation::R0); // Y (-7,-4)
    place(&mut s, K::LogicSwitch { high: false }, -10, 4, Rotation::R0); // Y (-7,4)
    // Gates: A (-3, y-1), B (-3, y+1), Y (3, y) for y = -8, 0, 8.
    for (op, y) in [(GateOp::And, -8), (GateOp::Or, 0), (GateOp::Xor, 8)] {
        place(&mut s, logic_gate(op), 0, y, Rotation::R0);
        place(&mut s, K::LogicLed, 8, y, Rotation::R0); // input (5, y)
        wire(&mut s, &[(3, y), (5, y)]);
    }
    // Input A bus at x = -5, input B bus at x = -6.
    wire(&mut s, &[(-7, -4), (-5, -4)]);
    wire(&mut s, &[(-5, -9), (-5, 7)]);
    for y in [-9, -1, 7] {
        wire(&mut s, &[(-5, y), (-3, y)]);
    }
    wire(&mut s, &[(-7, 4), (-6, 4)]);
    wire(&mut s, &[(-6, -7), (-6, 9)]);
    for y in [-7, 1, 9] {
        wire(&mut s, &[(-6, y), (-3, y)]);
    }
    s.add_label("A", Point::new(-5, -6));
    s.add_label("B", Point::new(-6, 6));
    s.add_label("AND", Point::new(4, -8));
    s.add_label("OR", Point::new(4, 0));
    s.add_label("XOR", Point::new(4, 8));
    s
}

/// A 2 Hz clock driving a 4-bit counter with an LED on each output. Run it
/// with Speed set to real time to watch it count.
pub fn binary_counter() -> Schematic {
    let mut s = Schematic::new();
    place(
        &mut s,
        K::Clock {
            frequency: 2.0,
            duty: 0.5,
        },
        -8,
        0,
        Rotation::R0,
    ); // Y (-5,0)
    place(
        &mut s,
        K::Counter4 {
            delay: dunbrec_core::DEFAULT_LOGIC_DELAY,
        },
        0,
        0,
        Rotation::R0,
    ); // CLK (-3,0)
    wire(&mut s, &[(-5, 0), (-3, 0)]);
    // Fan the outputs out to LEDs spaced well apart.
    for (i, (y, led_y)) in [(-3, -6), (-1, -2), (1, 2), (3, 6)].into_iter().enumerate() {
        place(&mut s, K::LogicLed, 12, led_y, Rotation::R0); // input (9, led_y)
        wire(&mut s, &[(3, y), (6, y), (6, led_y), (9, led_y)]);
        s.add_label(format!("Q{i}"), Point::new(8, led_y));
    }
    s.add_label("CLK", Point::new(-4, 0));
    s
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
            let high = |n: &str| op.voltage_by_name(n).unwrap() > 2.5;
            if ex.name.starts_with("Arduino") {
                // Pins float until the firmware runs; see `arduino_examples_run`.
                continue;
            }
            match ex.name {
                // A = 1, B = 0.
                "Logic gates" => {
                    assert!(!high("AND") && high("OR") && high("XOR"));
                    continue;
                }
                // Astable: no steady state to check; see `timer555_examples_run`.
                "555 LED blinker" => continue,
                "555 one-shot" => {
                    assert!(!high("OUT"), "waits for a trigger");
                    continue;
                }
                "Binary counter" => {
                    assert!(
                        ["Q0", "Q1", "Q2", "Q3"].iter().all(|q| !high(q)),
                        "starts at 0"
                    );
                    continue;
                }
                _ => {}
            }
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
                // Sine input at 0 V: only the input offset shows.
                "Op-amp inverting amplifier" => assert!(vout.abs() < 0.05, "VOUT = {vout}"),
                other => panic!("no expectation for {other}"),
            }
        }
    }

    /// Times at which `v` crosses `level` upwards.
    fn rising(times: &[f64], v: &[f64], level: f64) -> Vec<f64> {
        (1..v.len())
            .filter(|&i| v[i - 1] < level && v[i] >= level)
            .map(|i| times[i])
            .collect()
    }

    #[test]
    fn timer555_examples_run() {
        use dunbrec_sim::{TransientOptions, transient};
        let s = timer555_blinker();
        let netlist = build_netlist(&s);
        let circuit = Circuit::from_netlist(&netlist, &s.models).unwrap();
        let res = transient(circuit, TransientOptions::new(3.5).with_max_step(2e-3)).unwrap();
        let out = res.voltage("OUT").unwrap();
        let rises = rising(&res.times, out, 4.5);
        assert!(rises.len() >= 3, "{rises:?}");
        // Steady cycles (after the first, which starts from 0 V).
        let period = rises[2] - rises[1];
        let expected = std::f64::consts::LN_2 * (1e3 + 2.0 * 68e3) * 10e-6;
        assert!((period / expected - 1.0).abs() < 0.02, "period {period}");

        // The op-amp example amplifies by -10: 5 V peak out. With 1 MHz
        // gain-bandwidth the closed-loop bandwidth is ~90 kHz, so at 1 kHz
        // the output lags by ~0.6°, up to ~60 mV from an exact -10 x VIN.
        let s = opamp_inverting();
        let netlist = build_netlist(&s);
        let circuit = Circuit::from_netlist(&netlist, &s.models).unwrap();
        let res = transient(circuit, TransientOptions::new(3e-3).with_max_step(10e-6)).unwrap();
        let (vin, vout) = (res.voltage("VIN").unwrap(), res.voltage("VOUT").unwrap());
        let peak = vout.iter().cloned().fold(f64::MIN, f64::max);
        assert!((peak - 5.0).abs() < 0.05, "peak {peak}");
        for (i, t) in res.times.iter().enumerate() {
            if *t > 1e-3 {
                assert!((vout[i] + 10.0 * vin[i]).abs() < 0.1, "t = {t}");
            }
        }
    }

    #[test]
    fn arduino_examples_run() {
        use dunbrec_sim::{TransientOptions, transient};
        let run = |s: &Schematic, t: f64| {
            let netlist = build_netlist(s);
            let circuit = Circuit::from_netlist(&netlist, &s.models).unwrap();
            transient(circuit, TransientOptions::new(t).with_max_step(1e-3)).unwrap()
        };
        let res = run(&arduino_blink(), 0.6);
        assert!(*res.voltage("D13").unwrap().last().unwrap() > 4.5);
        let res = run(&arduino_fade(), 0.6);
        let v = *res.voltage("VOUT").unwrap().last().unwrap();
        assert!(v > 0.5 && v < 4.5, "VOUT = {v}");
        let res = run(&arduino_pot_serial(), 0.6);
        let v = *res.voltage("VOUT").unwrap().last().unwrap();
        assert!((v - 2.5).abs() < 0.1, "VOUT = {v}");
        let text: String = res.serial.iter().map(|b| b.byte as char).collect();
        // 2.5 V sits on the 511/512 code boundary.
        assert!(text.contains("A0 = 051"), "{text:?}");
    }
}
