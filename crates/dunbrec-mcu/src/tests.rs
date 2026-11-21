//! Emulator tests: instruction semantics, then each peripheral, with
//! programs written in the built-in assembler and timing checked against
//! the datasheet.

use crate::asm::{Asm, io, mem};
use crate::{Atmega328p, PinDrive, Stop};

fn chip(a: &Asm) -> Atmega328p {
    Atmega328p::new(&a.finish().unwrap()).unwrap()
}

/// Run a program that ends in `rjmp end` for a while.
fn run(a: &mut Asm, cycles: u64) -> Atmega328p {
    a.label("end").rjmp("end");
    let mut c = chip(a);
    while c.run(cycles) != Stop::Reached {}
    assert!(c.fault().is_none(), "{:?}", c.fault());
    c
}

/// Drive changes of `pin` up to `until`: (cycle, new drive).
fn edges(c: &mut Atmega328p, pin: usize, until: u64) -> Vec<(u64, PinDrive)> {
    let mut out = Vec::new();
    let mut last = c.pin_drives()[pin];
    loop {
        let stop = c.run(until);
        let now = c.pin_drives()[pin];
        if now != last {
            out.push((c.cycle(), now));
            last = now;
        }
        if stop == Stop::Reached {
            return out;
        }
    }
}

fn highs(e: &[(u64, PinDrive)]) -> Vec<u64> {
    e.iter()
        .filter(|(_, d)| *d == PinDrive::High)
        .map(|e| e.0)
        .collect()
}

fn lows(e: &[(u64, PinDrive)]) -> Vec<u64> {
    e.iter()
        .filter(|(_, d)| *d == PinDrive::Low)
        .map(|e| e.0)
        .collect()
}

const SREG_C: u8 = 1;
const SREG_Z: u8 = 2;
const SREG_N: u8 = 4;
const SREG_V: u8 = 8;
const SREG_S: u8 = 16;
const SREG_H: u8 = 32;

#[test]
fn arithmetic_and_flags() {
    let mut a = Asm::new();
    a.ldi(16, 0x7F).ldi(17, 1).add(16, 17); // 0x80: V, N, H
    a.in_(2, io::SREG);
    a.ldi(18, 0).subi(18, 1); // 0xFF: C, N, H, S
    a.in_(3, io::SREG);
    // 16-bit add 0x00FF + 0x0001 and compare with 0x0100 (Z through CPC).
    a.ldi(24, 0xFF)
        .ldi(25, 0)
        .ldi(26, 1)
        .ldi(27, 0)
        .add(24, 26)
        .adc(25, 27);
    a.ldi(20, 0x00).ldi(21, 0x01).cp(24, 20).cpc(25, 21);
    a.in_(4, io::SREG);
    a.ldi(19, 200).ldi(20, 3).mul(19, 20).movw(6, 0); // 600
    a.ldi(19, 0xFE).muls(19, 20).movw(8, 0); // -2 * 3 = -6
    a.ldi(19, 0x80).ldi(20, 0x80).fmul(19, 20).movw(10, 0); // 1.0 * 1.0 (1.7 format)
    a.ldi(22, 0x80).neg(22); // 0x80, V set
    a.in_(12, io::SREG);
    a.ldi(23, 0x81).asr(23); // 0xC0, C set
    a.ldi(28, 0x01).sec().ror(28); // 0x80, C set
    a.ldi(29, 0x3C).swap(29); // 0xC3
    a.ldi(30, 0x7F).inc(30); // 0x80, V
    a.in_(13, io::SREG);
    a.ldi(31, 0x0F).andi(31, 0x3C).ori(31, 0x40).com(31); // ~0x4C = 0xB3
    let c = run(&mut a, 200);
    assert_eq!(c.reg(16), 0x80);
    assert_eq!(c.reg(2) & 0x3F, SREG_V | SREG_N | SREG_H);
    assert_eq!(c.reg(18), 0xFF);
    assert_eq!(c.reg(3) & 0x3F, SREG_C | SREG_N | SREG_S | SREG_H);
    assert_eq!((c.reg(24), c.reg(25)), (0x00, 0x01));
    assert_eq!(c.reg(4) & SREG_Z, SREG_Z, "CPC keeps Z when equal");
    assert_eq!(u16::from_le_bytes([c.reg(6), c.reg(7)]), 600);
    assert_eq!(u16::from_le_bytes([c.reg(8), c.reg(9)]) as i16, -6);
    assert_eq!(u16::from_le_bytes([c.reg(10), c.reg(11)]), 0x8000);
    assert_eq!(c.reg(22), 0x80);
    assert_eq!(c.reg(12) & SREG_V, SREG_V);
    assert_eq!(c.reg(23), 0xC0);
    assert_eq!(c.reg(28), 0x80);
    assert_eq!(c.reg(29), 0xC3);
    assert_eq!(c.reg(30), 0x80);
    assert_eq!(c.reg(13) & (SREG_V | SREG_N), SREG_V | SREG_N);
    assert_eq!(c.reg(31), 0xB3);
}

#[test]
fn branches_and_skips() {
    let mut a = Asm::new();
    // Count r16 down from 5; r17 counts iterations.
    a.ldi(16, 5).clr(17);
    a.label("loop").inc(17).dec(16).brne("loop");
    // CPSE over a two-word instruction (LDS) must skip both words.
    a.ldi(18, 7)
        .ldi(19, 7)
        .cpse(18, 19)
        .lds(20, 0x100)
        .ldi(21, 0xAA);
    // SBRS / SBRC.
    a.ldi(22, 0b100)
        .sbrs(22, 2)
        .ldi(23, 1)
        .sbrc(22, 2)
        .ldi(24, 2);
    // Signed compare: -1 < 1.
    a.ldi(25, 0xFF)
        .cpi(25, 1)
        .brlt("neg")
        .ldi(26, 0)
        .rjmp("after");
    a.label("neg").ldi(26, 1);
    a.label("after");
    // BST / BLD.
    a.ldi(27, 0x80).bst(27, 7).clr(28).bld(28, 3);
    let c = run(&mut a, 200);
    assert_eq!(c.reg(17), 5);
    assert_eq!(c.reg(20), 0, "LDS was skipped");
    assert_eq!(c.reg(21), 0xAA);
    assert_eq!(c.reg(23), 0, "SBRS skipped");
    assert_eq!(c.reg(24), 2, "SBRC did not skip");
    assert_eq!(c.reg(26), 1);
    assert_eq!(c.reg(28), 0x08);
}

#[test]
fn memory_stack_and_calls() {
    let mut a = Asm::new();
    a.init_stack();
    // X post-increment stores, Y pre-decrement loads, Z displacement.
    a.ldi(26, 0x00).ldi(27, 0x01); // X = 0x100
    a.ldi(16, 0x11).st_xp(16).ldi(16, 0x22).st_xp(16);
    a.ldi(28, 0x02).ldi(29, 0x01); // Y = 0x102
    a.ld_my(17).ld_my(18); // r17 = 0x22, r18 = 0x11
    a.ldi(30, 0x00).ldi(31, 0x01).ldd_z(19, 1); // r19 = 0x22
    a.ldi(16, 0x33).std_y(5, 16); // [0x105] = 0x33
    a.adiw(30, 40).sbiw(30, 8); // Z = 0x120
    a.movw(2, 30);
    // PUSH/POP and CALL/RET.
    a.ldi(20, 0x5A).push(20).clr(20).pop(20);
    a.call("sub");
    a.ldi(21, 0x77);
    a.rjmp("end");
    a.label("sub");
    a.in_(4, io::SPL).in_(5, io::SPH);
    a.lds(6, 0x08FF).lds(7, 0x08FE);
    a.ret();
    // LPM from a table.
    a.label("table").bytes(&[0xDE, 0xAD]);
    a.label("end").rjmp("end");
    let image = a.finish().unwrap();
    // The return address is the word after the two-word CALL.
    let call_ret = image
        .chunks(2)
        .position(|p| u16::from_le_bytes([p[0], p[1]]) & 0xFE0E == 0x940E)
        .unwrap()
        + 2;
    let mut c = Atmega328p::new(&image).unwrap();
    while c.run(500) != Stop::Reached {}
    assert_eq!(c.peek(0x100), 0x11);
    assert_eq!(c.peek(0x101), 0x22);
    assert_eq!((c.reg(17), c.reg(18), c.reg(19)), (0x22, 0x11, 0x22));
    assert_eq!(c.peek(0x105), 0x33);
    assert_eq!(u16::from_le_bytes([c.reg(2), c.reg(3)]), 0x120);
    assert_eq!(c.reg(20), 0x5A);
    assert_eq!(c.reg(21), 0x77, "returned from the call");
    assert_eq!(u16::from_le_bytes([c.reg(4), c.reg(5)]), 0x08FD);
    // The return address is pushed low byte first.
    assert_eq!(c.reg(6), call_ret as u8);
    assert_eq!(c.reg(7), (call_ret >> 8) as u8);

    let mut a = Asm::new();
    a.ldi_lo(30, "t").ldi_hi(31, "t").lpm_zp(16).lpm_z(17).lpm();
    a.rjmp("end");
    a.label("t").bytes(&[0xDE, 0xAD]);
    let c = run(&mut a, 50);
    assert_eq!((c.reg(16), c.reg(17), c.reg(0)), (0xDE, 0xAD, 0xAD));
}

#[test]
fn ports_pullups_and_pin_reads() {
    let mut a = Asm::new();
    a.ldi(16, 0b0010_0001).out(io::DDRB, 16); // PB0, PB5 outputs
    a.ldi(16, 0b0010_0010).out(io::PORTB, 16); // PB5 high, PB1 pull-up
    a.sbi(io::PIND, 4); // PORTD4 := 1 (pull-up on D4)
    a.label("read").in_(20, io::PIND).rjmp("read");
    a.label("end").rjmp("end");
    let mut c = chip(&a);
    while c.run(20) != Stop::Reached {}
    let d = c.pin_drives();
    assert_eq!(d[8], PinDrive::Low);
    assert_eq!(d[13], PinDrive::High);
    assert_eq!(d[9], PinDrive::PullUp);
    assert_eq!(d[4], PinDrive::PullUp);
    assert_eq!(d[0], PinDrive::Floating);
    c.set_input(2, true);
    c.set_input(7, true);
    while c.run(c.cycle() + 10) != Stop::Reached {}
    assert_eq!(c.reg(20), 0b1000_0100);

    // PUD disables every pull-up.
    let mut a = Asm::new();
    a.ldi(16, 0xFF)
        .out(io::PORTC, 16)
        .ldi(16, 0x10)
        .out(io::MCUCR, 16);
    let c = run(&mut a, 20);
    assert!(
        c.pin_drives()[14..]
            .iter()
            .all(|d| *d == PinDrive::Floating)
    );
}

#[test]
fn fast_pwm_duty_and_frequency() {
    // Timer 0 fast PWM on OC0A (D6), no prescaler, OCR0A = 63:
    // period 256 cycles, high for 64.
    let mut a = Asm::new();
    a.sbi(io::DDRD, 6);
    a.ldi(16, 63).out(io::OCR0A, 16);
    a.ldi(16, 0x83).out(io::TCCR0A, 16);
    a.ldi(16, 0x01).out(io::TCCR0B, 16);
    a.label("end").rjmp("end");
    let mut c = chip(&a);
    let e = edges(&mut c, 6, 5000);
    let (h, l) = (highs(&e), lows(&e));
    assert!(h.len() > 10);
    for w in h.windows(2) {
        assert!((w[1] - w[0]).abs_diff(256) <= 2, "period {}", w[1] - w[0]);
    }
    let first_low = l.iter().find(|&&t| t > h[2]).unwrap();
    assert!(
        (first_low - h[2]).abs_diff(64) <= 2,
        "high for {}",
        first_low - h[2]
    );
}

#[test]
fn ctc_toggle_on_timer1() {
    // Timer 1 CTC (TOP = OCR1A = 99), clk/8, toggle OC1A (D9): a toggle
    // every 800 cycles.
    let mut a = Asm::new();
    a.sbi(io::DDRB, 1);
    a.ldi(16, 0)
        .sts(mem::OCR1AH, 16)
        .ldi(16, 99)
        .sts(mem::OCR1AL, 16);
    a.ldi(16, 0x40).sts(mem::TCCR1A, 16);
    a.ldi(16, 0x0A).sts(mem::TCCR1B, 16);
    a.label("end").rjmp("end");
    let mut c = chip(&a);
    let e = edges(&mut c, 9, 20_000);
    assert!(e.len() > 20);
    // Skip the first change (the pin becoming an output).
    for w in e[1..].windows(2) {
        assert!((w[1].0 - w[0].0).abs_diff(800) <= 2, "{:?}", w);
    }
}

#[test]
fn phase_correct_pwm_on_timer2() {
    // Timer 2 phase-correct, OC2B (D3) non-inverting, OCR2B = 64, clk/1:
    // period 510 cycles, high for 2 * 64.
    let mut a = Asm::new();
    a.sbi(io::DDRD, 3);
    a.ldi(16, 64).sts(mem::OCR2B, 16);
    a.ldi(16, 0x21).sts(mem::TCCR2A, 16);
    a.ldi(16, 0x01).sts(mem::TCCR2B, 16);
    a.label("end").rjmp("end");
    let mut c = chip(&a);
    let e = edges(&mut c, 3, 10_000);
    let (h, l) = (highs(&e), lows(&e));
    for w in h.windows(2) {
        assert!((w[1] - w[0]).abs_diff(510) <= 2, "period {}", w[1] - w[0]);
    }
    let fall = l.iter().find(|&&t| t > h[2]).unwrap();
    assert!((fall - h[2]).abs_diff(128) <= 2, "high for {}", fall - h[2]);
}

#[test]
fn sixteen_bit_registers_use_temp() {
    let mut a = Asm::new();
    a.ldi(16, 0x12)
        .sts(mem::ICR1H, 16)
        .ldi(16, 0x34)
        .sts(mem::ICR1L, 16);
    a.lds(20, mem::ICR1L).lds(21, mem::ICR1H);
    let c = run(&mut a, 50);
    assert_eq!((c.reg(20), c.reg(21)), (0x34, 0x12));
}

#[test]
fn timer_interrupts_and_sleep() {
    // Timer 0 overflow every 256 cycles increments r20; main sleeps.
    let mut a = Asm::new();
    a.vectors(&[(0, "reset"), (16, "ovf")], "reset");
    a.label("reset").init_stack();
    a.clr(20);
    a.ldi(16, 0x01).out(io::TCCR0B, 16).sts(mem::TIMSK0, 16);
    a.ldi(16, 0x01).out(io::SMCR, 16);
    a.sei();
    a.label("loop").sleep().rjmp("loop");
    a.label("ovf").inc(20).reti();
    let mut c = chip(&a);
    c.run(256 * 50 + 100);
    assert!(c.fault().is_none());
    assert!((c.reg(20) as i32 - 50).abs() <= 1, "{}", c.reg(20));
    assert!(c.is_sleeping());
}

#[test]
fn external_and_pin_change_interrupts() {
    let mut a = Asm::new();
    a.vectors(&[(0, "reset"), (1, "int0"), (3, "pcint0")], "reset");
    a.label("reset").init_stack();
    a.clr(20).clr(21);
    a.ldi(16, 0x02).sts(mem::EICRA, 16); // INT0 on falling edges
    a.ldi(16, 0x01).out(io::EIMSK, 16);
    a.ldi(16, 0x01).sts(mem::PCICR, 16).sts(mem::PCMSK0, 16); // PCINT0 = PB0 (D8)
    a.sei();
    a.label("loop").rjmp("loop");
    a.label("int0").inc(20).reti();
    a.label("pcint0").inc(21).reti();
    let mut c = chip(&a);
    c.run(100);
    c.set_input(2, true); // rising: ignored
    c.run(200);
    c.set_input(2, false); // falling: INT0
    c.run(300);
    c.set_input(8, true); // any change: PCINT0
    c.run(400);
    c.set_input(8, false);
    c.run(500);
    assert_eq!(c.reg(20), 1);
    assert_eq!(c.reg(21), 2);
}

#[test]
fn adc_conversion() {
    // AVCC reference, right-adjusted, channel 1, clk/128.
    let mut a = Asm::new();
    a.ldi(16, 0x41).sts(mem::ADMUX, 16);
    a.ldi(16, 0xC7).sts(mem::ADCSRA, 16); // enable + start
    a.label("wait")
        .lds(16, mem::ADCSRA)
        .sbrc(16, 6)
        .rjmp("wait");
    a.lds(24, mem::ADCL).lds(25, mem::ADCH);
    a.label("end").rjmp("end");
    let mut c = chip(&a);
    assert_eq!(c.run(100_000), Stop::AdcSample(1));
    let start = c.cycle();
    c.set_analog(1, 2.5);
    assert_eq!(c.run(100_000), Stop::Reached);
    assert_eq!(u16::from_le_bytes([c.reg(24), c.reg(25)]), 512);
    // The first conversion takes 25 ADC clocks of 128 cycles.
    let mut probe = chip(&a);
    probe.run(100_000);
    probe.set_analog(1, 2.5);
    let mut done = start;
    while probe.peek(0x7A) & 0x40 != 0 {
        probe.run(probe.cycle() + 1);
        done = probe.cycle();
    }
    assert!((done - start).abs_diff(25 * 128) <= 4, "{}", done - start);
}

#[test]
fn usart_transmits_bytes_and_waveform() {
    // 9600 baud at 16 MHz: 1664 cycles per bit, 10 bits per byte.
    let mut a = Asm::new();
    a.ldi(16, 103).sts(mem::UBRR0L, 16);
    a.ldi(16, 0x08).sts(mem::UCSR0B, 16);
    for &b in b"Hi" {
        a.label(&format!("w{b}"))
            .lds(17, mem::UCSR0A)
            .sbrs(17, 5)
            .rjmp(&format!("w{b}"));
        a.ldi(16, b).sts(mem::UDR0, 16);
    }
    a.label("end").rjmp("end");
    let mut c = chip(&a);
    let e = edges(&mut c, 1, 40_000);
    let bytes = c.take_serial();
    assert_eq!(bytes.iter().map(|b| b.1).collect::<Vec<_>>(), b"Hi");
    // 'H' = 0x48: start bit 0, then 0,0,0,1,0,0,1,0, stop 1.
    let start = lows(&e)[0];
    let expect_end = start + 10 * 1664;
    assert!(bytes[0].0.abs_diff(expect_end) <= 4);
    // Second byte follows back to back.
    assert!((bytes[1].0 - bytes[0].0).abs_diff(10 * 1664) <= 4);
    // First rising edge: after start + three zero data bits, at bit 4.
    let rise = highs(&e).into_iter().find(|&t| t > start).unwrap();
    assert!((rise - start).abs_diff(4 * 1664) <= 4, "{}", rise - start);
}

#[test]
fn faults_are_reported() {
    let mut a = Asm::new();
    a.nop();
    let mut c = chip(&a);
    c.run(100);
    assert!(c.fault().unwrap().contains("empty flash"));
    let mut a = Asm::new();
    a.word(0x9204); // XCH: not on this chip
    let mut c = chip(&a);
    c.run(100);
    assert!(
        c.fault()
            .unwrap()
            .contains("unsupported instruction 0x9204")
    );
    assert!(Atmega328p::new(&vec![0; 40_000]).is_err());
}

#[test]
fn builtin_blink_and_fade() {
    let mut c = Atmega328p::new(&crate::programs::builtin("blink").unwrap()).unwrap();
    let e = edges(&mut c, 13, 34_000_000);
    let changes: Vec<u64> = e.iter().map(|e| e.0).skip(1).collect();
    assert_eq!(changes.len(), 4, "{e:?}");
    for (k, t) in changes.iter().enumerate() {
        let expect = (k as u64 + 1) * 8_000_000;
        assert!(t.abs_diff(expect) < 100, "toggle {k} at {t}");
    }

    let mut c = Atmega328p::new(&crate::programs::builtin("fade").unwrap()).unwrap();
    while c.run(1_600_000) != Stop::Reached {} // 0.1 s: 24 overflow groups → duty ~24
    let duty = c.peek(0x47);
    assert!((20..=28).contains(&duty), "{duty}");
}

#[test]
fn builtin_pot_pwm_serial() {
    let mut c = Atmega328p::new(&crate::programs::builtin("pot-pwm-serial").unwrap()).unwrap();
    let end = 16_000_000 / 4;
    loop {
        match c.run(end) {
            Stop::AdcSample(0) => c.set_analog(0, 2.5),
            Stop::Reached => break,
            _ => {}
        }
    }
    let text: String = c.take_serial().iter().map(|b| b.1 as char).collect();
    assert!(
        text.starts_with("Dunbrec AVR demo\r\nA0 = 0512\r\nA0 = 0512\r\n"),
        "{text:?}"
    );
    // PWM follows the top 8 bits of the reading.
    assert_eq!(c.peek(0x47), 128);
}

/// Emulation speed in release builds (run with `--release --ignored`).
#[test]
#[ignore]
fn speed() {
    let mut a = Asm::new();
    a.label("l").inc(16).add(17, 16).sbiw(24, 1).rjmp("l");
    let mut c = chip(&a);
    let t = std::time::Instant::now();
    c.run(160_000_000);
    let s = t.elapsed().as_secs_f64();
    eprintln!("{:.0} MHz equivalent", 160.0 / s);
}

// ---------------------------------------------------------- debugging ---

#[test]
fn a_breakpoint_stops_before_its_instruction_and_resuming_runs_it() {
    let mut a = Asm::new();
    a.clr(20);
    a.label("loop").inc(20);
    a.label("mark").nop();
    a.rjmp("loop");
    let mark = a.label_address("mark").unwrap();
    let mut c = chip(&a);
    c.set_breakpoints(&[mark]);

    // Stops after INC ran once, before the NOP.
    assert_eq!(c.run(1000), Stop::Breakpoint);
    assert_eq!((c.pc(), c.reg(20)), (mark, 1));
    let first = c.cycle();
    // Resuming runs the NOP and a whole pass (INC 1 + NOP 1 + RJMP 2 = 4
    // cycles), then stops again at the same place.
    assert_eq!(c.run(1000), Stop::Breakpoint);
    assert_eq!((c.pc(), c.reg(20)), (mark, 2));
    assert_eq!(c.cycle() - first, 4);

    // With the breakpoints cleared it runs on to the requested cycle.
    c.set_breakpoints(&[]);
    assert_eq!(c.run(first + 100), Stop::Reached);
}

#[test]
fn a_breakpoint_on_the_first_instruction_stops_before_anything_runs() {
    let mut a = Asm::new();
    a.ldi(16, 5).label("end").rjmp("end");
    let mut c = chip(&a);
    c.set_breakpoints(&[0]);
    assert_eq!(c.run(100), Stop::Breakpoint);
    assert_eq!((c.pc(), c.cycle(), c.reg(16)), (0, 0, 0));
    // Asking again without moving on does not run it either way: the
    // second call runs the instruction, as promised.
    assert_eq!(c.run(100), Stop::Reached);
    assert_eq!(c.reg(16), 5);
    // A reset arms the breakpoint again.
    c.reset();
    assert_eq!(c.run(c.cycle() + 100), Stop::Breakpoint);
}

#[test]
fn a_breakpoint_in_an_interrupt_handler_stops_on_every_entry() {
    // Timer 0 overflows every 256 cycles; the handler counts in r20.
    let mut a = Asm::new();
    a.vectors(&[(0, "reset"), (16, "ovf")], "reset");
    a.label("reset").init_stack();
    a.clr(20);
    a.ldi(16, 0x01).out(io::TCCR0B, 16).sts(mem::TIMSK0, 16);
    a.sei();
    a.label("loop").rjmp("loop");
    a.label("ovf").inc(20).reti();
    let ovf = a.label_address("ovf").unwrap();
    let mut c = chip(&a);
    c.set_breakpoints(&[ovf]);

    assert_eq!(c.run(10_000), Stop::Breakpoint);
    assert_eq!(
        (c.pc(), c.reg(20)),
        (ovf, 0),
        "stopped before the handler's INC"
    );
    let first = c.cycle();
    assert_eq!(c.run(10_000), Stop::Breakpoint);
    assert_eq!((c.pc(), c.reg(20)), (ovf, 1));
    assert_eq!(c.cycle() - first, 256, "one timer period apart");
    assert!(c.fault().is_none());
}

#[test]
fn stopping_at_a_breakpoint_does_not_lose_the_instruction_after_sei() {
    // The instruction after SEI always runs before an interrupt is taken.
    // A breakpoint on it must not let the interrupt in first.
    let mut a = Asm::new();
    a.vectors(&[(0, "reset"), (1, "int0")], "reset");
    a.label("reset").init_stack();
    a.clr(20).clr(21);
    a.ldi(16, 0x02).sts(mem::EICRA, 16);
    a.ldi(16, 0x01).out(io::EIMSK, 16);
    a.sei();
    a.label("after").inc(21);
    a.label("loop").rjmp("loop");
    a.label("int0").inc(20).reti();
    let after = a.label_address("after").unwrap();
    let mut c = chip(&a);
    c.set_input(2, true);
    c.set_breakpoints(&[after]);
    assert_eq!(c.run(1000), Stop::Breakpoint);
    c.set_input(2, false); // falling edge: INT0 becomes pending
    // Resume: the very next step must be INC r21, not the interrupt entry.
    c.set_breakpoints(&[]);
    assert_eq!(c.step_instruction(u64::MAX), Stop::Stepped);
    assert_eq!(
        (c.reg(21), c.reg(20)),
        (1, 0),
        "INC ran before the interrupt"
    );
    // And then the interrupt is taken.
    c.run(c.cycle() + 100);
    assert_eq!((c.reg(21), c.reg(20)), (1, 1));
}

#[test]
fn stepping_runs_exactly_one_instruction() {
    let mut a = Asm::new();
    a.ldi(16, 7).ldi(17, 9).add(16, 17).label("end").rjmp("end");
    let mut c = chip(&a);
    let step = |c: &mut Atmega328p| c.step_instruction(u64::MAX);

    assert_eq!(step(&mut c), Stop::Stepped);
    assert_eq!((c.pc(), c.reg(16), c.cycle()), (1, 7, 1));
    assert_eq!(step(&mut c), Stop::Stepped);
    assert_eq!((c.pc(), c.reg(17), c.cycle()), (2, 9, 2));
    assert_eq!(step(&mut c), Stop::Stepped);
    assert_eq!((c.pc(), c.reg(16), c.cycle()), (3, 16, 3));
    // RJMP to itself takes two cycles and leaves the program counter put.
    assert_eq!(step(&mut c), Stop::Stepped);
    assert_eq!((c.pc(), c.cycle()), (3, 5));

    // A step runs the instruction at the program counter even on a
    // breakpoint (stepping off one must not stall)...
    c.set_breakpoints(&[3]);
    assert_eq!(step(&mut c), Stop::Stepped);
    assert_eq!(c.cycle(), 7);
    // ...but arriving at one by running stops before it.
    assert_eq!(c.run(100), Stop::Breakpoint);
    assert_eq!(c.cycle(), 7);
}

#[test]
fn a_step_lands_in_the_interrupt_handler() {
    let mut a = Asm::new();
    a.vectors(&[(0, "reset"), (16, "ovf")], "reset");
    a.label("reset").init_stack();
    a.ldi(16, 0x01).out(io::TCCR0B, 16).sts(mem::TIMSK0, 16);
    a.sei();
    a.label("loop").rjmp("loop");
    a.label("ovf").reti();
    let ovf = a.label_address("ovf").unwrap();
    let mut c = chip(&a);
    // Step until the program counter reaches the handler: the interrupt
    // entry counts as one step.
    let mut steps = 0;
    while c.pc() != ovf {
        assert_eq!(c.step_instruction(u64::MAX), Stop::Stepped);
        steps += 1;
        assert!(steps < 1000, "never entered the handler");
    }
    assert!(c.cycle() >= 256);
    // Returning lands back in the main loop.
    c.step_instruction(u64::MAX);
    assert_ne!(c.pc(), ovf);
}

#[test]
fn a_snapshot_matches_the_chip() {
    let mut a = Asm::new();
    a.init_stack();
    a.ldi(16, 0x5A).ldi(31, 0x01).label("end").rjmp("end");
    let mut c = chip(&a);
    c.run(50);
    let s = c.snapshot();
    assert_eq!(
        (s.pc, s.cycle, s.sreg, s.sp),
        (c.pc(), c.cycle(), c.sreg(), c.sp())
    );
    assert_eq!(s.regs[16], 0x5A);
    assert_eq!(s.regs[31], 0x01);
    assert_eq!(s.data.len(), c.data().len());
    assert_eq!(s.sp, 0x08FF, "init_stack puts the stack at the top of SRAM");
    assert!(!s.sleeping && s.fault.is_none());
    // The snapshot is a copy: running on does not change it.
    c.run(200);
    assert_ne!(c.cycle(), s.cycle);
}
