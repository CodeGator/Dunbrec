//! The demo programs as source text, for the user to copy into a project and
//! change. Each comes in AVR assembly and as an Arduino sketch, with the
//! settings worth changing at the top.
//!
//! They behave like the compiled-in programs of the same name
//! ([`crate::programs`]), which example circuits and older circuit files
//! still refer to.

/// One demo program in both languages.
pub struct Demo {
    /// The name it shares with its compiled-in program.
    pub name: &'static str,
    /// AVR assembly, for Dunbrec's own assembler.
    pub assembly: &'static str,
    /// An Arduino sketch, built with `arduino-cli`.
    pub sketch: &'static str,
}

macro_rules! demo {
    ($name:literal) => {
        Demo {
            name: $name,
            assembly: include_str!(concat!("../demos/", $name, ".asm")),
            sketch: include_str!(concat!("../demos/", $name, ".ino")),
        }
    };
}

pub const DEMOS: &[Demo] = &[demo!("blink"), demo!("fade"), demo!("pot-pwm-serial")];

/// The demo called `name`.
pub fn demo(name: &str) -> Option<&'static Demo> {
    DEMOS.iter().find(|d| d.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Atmega328p, Stop, arduino, asm_text};

    /// Pin numbers as [`Atmega328p`] counts them: D0 to D13.
    const D4: usize = 4;
    const D5: usize = 5;
    const D6: usize = 6;
    const D13: usize = 13;
    const SECOND: u64 = 16_000_000;

    /// What a run of a program did.
    struct Seen {
        /// Times (in seconds) at which the watched pin's drive changed.
        changes: Vec<f64>,
        serial: String,
        chip: Atmega328p,
    }

    /// Run `image` for `seconds` with 2.5 V on every analog input, watching
    /// `pin`.
    fn run(image: &[u8], pin: usize, seconds: f64) -> Seen {
        let mut chip = Atmega328p::new(image).expect("fits the chip");
        let end = (seconds * SECOND as f64) as u64;
        let mut changes = Vec::new();
        let mut last = chip.pin_drives()[pin];
        while chip.cycle() < end {
            match chip.run(end) {
                Stop::Pins => {
                    let now = chip.pin_drives()[pin];
                    if now != last {
                        changes.push(chip.cycle() as f64 / SECOND as f64);
                        last = now;
                    }
                }
                Stop::AdcSample(channel) => chip.set_analog(channel, 2.5),
                Stop::Reached => break,
                other => panic!("unexpected stop {other:?}"),
            }
        }
        let serial = chip.take_serial().iter().map(|b| b.1 as char).collect();
        Seen {
            changes,
            serial,
            chip,
        }
    }

    fn assembled(source: &str) -> Vec<u8> {
        asm_text::assemble(source)
            .unwrap_or_else(|e| panic!("{e:?}"))
            .image
    }

    /// `source` with one setting changed.
    fn changed(source: &str, from: &str, to: &str) -> String {
        assert!(source.contains(from), "no setting {from:?}");
        source.replacen(from, to, 1)
    }

    /// The gaps between changes, leaving out the first (the program setting
    /// itself up).
    fn gaps(changes: &[f64]) -> Vec<f64> {
        changes.windows(2).skip(1).map(|w| w[1] - w[0]).collect()
    }

    #[test]
    fn every_compiled_in_program_has_both_sources() {
        for (name, _) in crate::programs::BUILTIN {
            let d = demo(name).expect("a source for it");
            assert!(!d.assembly.is_empty() && !d.sketch.is_empty());
        }
        assert_eq!(DEMOS.len(), crate::programs::BUILTIN.len());
        assert!(demo("nope").is_none());
    }

    #[test]
    fn assembly_blink_blinks_and_its_settings_work() {
        let source = demo("blink").unwrap().assembly;
        let seen = run(&assembled(source), D13, 2.2);
        for gap in gaps(&seen.changes) {
            assert!((gap - 0.5).abs() < 0.002, "{gap}");
        }
        assert!(seen.changes.len() >= 4, "{:?}", seen.changes);

        // Another pin on the other port, and other times.
        let other = changed(source, "LED_DDR  = DDRB", "LED_DDR  = DDRD");
        let other = changed(&other, "LED_PINS = PINB", "LED_PINS = PIND");
        let other = changed(&other, "LED_BIT  = 5 ", "LED_BIT  = 4 ");
        let other = changed(&other, "ON_MS  = 500", "ON_MS  = 100");
        let other = changed(&other, "OFF_MS = 500", "OFF_MS = 300");
        let seen = run(&assembled(&other), D4, 1.5);
        let g = gaps(&seen.changes);
        assert!(g.len() >= 4, "{g:?}");
        // On and off alternate; the first gap kept is an on time.
        assert!(
            (g[0] - 0.1).abs() < 0.002 && (g[1] - 0.3).abs() < 0.002,
            "{g:?}"
        );
    }

    #[test]
    fn assembly_fade_changes_the_duty_on_either_pin() {
        let source = demo("fade").unwrap().assembly;
        let image = assembled(source);
        // The compare register climbs one step every 4 ms.
        let early = run(&image, D6, 0.1).chip.peek(0x47);
        let later = run(&image, D6, 0.5).chip.peek(0x47);
        assert!((20..=30).contains(&early), "{early}");
        assert!((115..=130).contains(&later), "{later}");
        assert!(run(&image, D6, 0.1).changes.len() > 50, "pulses on D6");

        let other = changed(source, "PWM_BIT  = 6", "PWM_BIT  = 5");
        let other = changed(&other, "PWM_OCR  = OCR0A", "PWM_OCR  = OCR0B");
        let other = changed(&other, ".equ    PWM_MODE = 0x83", ".equ    PWM_MODE = 0x23");
        let seen = run(&assembled(&other), D5, 0.5);
        assert!(seen.changes.len() > 50, "pulses on D5");
        assert!((115..=130).contains(&seen.chip.peek(0x48)));
    }

    #[test]
    fn assembly_pot_reads_prints_and_drives_the_pwm() {
        let source = demo("pot-pwm-serial").unwrap().assembly;
        let seen = run(&assembled(source), D6, 0.35);
        assert!(
            seen.serial
                .starts_with("Dunbrec AVR demo\r\nA0 = 0512\r\nA0 = 0512\r\n"),
            "{:?}",
            seen.serial
        );
        // A reading, about 11 ms of sending it, then a 100 ms wait.
        assert_eq!(seen.serial.matches("A0 = ").count(), 3);
        assert_eq!(seen.chip.peek(0x47), 128);

        let other = changed(source, "INPUT = 0", "INPUT = 3");
        let seen = run(&assembled(&other), D6, 0.05);
        assert!(seen.serial.contains("A3 = 0512"), "{:?}", seen.serial);
    }

    /// Runs the real tool, and does nothing on a machine without it.
    #[test]
    fn the_sketches_build_and_behave() {
        let Some(cli) = arduino::find_cli(None) else {
            eprintln!("arduino-cli not installed: skipped");
            return;
        };
        let built = |name: &str| {
            arduino::build(&cli, name, demo(name).unwrap().sketch)
                .unwrap_or_else(|e| panic!("{name}: {e}"))
                .image
        };
        let seen = run(&built("blink"), D13, 1.8);
        let g = gaps(&seen.changes);
        assert!(
            g.iter().all(|g| (g - 0.5).abs() < 0.01) && !g.is_empty(),
            "{g:?}"
        );

        let seen = run(&built("fade"), D6, 0.5);
        assert!(seen.changes.len() > 50, "pulses on D6");

        let seen = run(&built("pot-pwm-serial"), D6, 0.35);
        assert!(seen.serial.contains("Reading = 512"), "{:?}", seen.serial);
    }
}
