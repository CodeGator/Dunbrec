//! Built-in demo firmware, so example circuits work without a toolchain.
//! Each program is written with the [`crate::asm`] assembler for a 16 MHz
//! ATmega328P, using the same pins an Arduino sketch would.

use crate::asm::{Asm, io, mem};

/// (name, description) of every built-in program.
pub const BUILTIN: &[(&str, &str)] = &[
    ("blink", "Toggles D13 every 0.5 s (timer 1 interrupt)"),
    (
        "fade",
        "PWM on D6 fading up and down over about 2 s (timer 0)",
    ),
    (
        "pot-pwm-serial",
        "Reads A0, mirrors it as PWM on D6, prints it at 9600 baud every 100 ms",
    ),
];

/// The flash image of a built-in program.
pub fn builtin(name: &str) -> Option<Vec<u8>> {
    let asm = match name {
        "blink" => blink(),
        "fade" => fade(),
        "pot-pwm-serial" => pot_pwm_serial(),
        _ => return None,
    };
    Some(asm.finish().expect("built-in programs assemble"))
}

fn blink() -> Asm {
    let mut a = Asm::new();
    a.vectors(&[(0, "reset"), (11, "t1_compa")], "unused");
    a.label("unused").reti();
    a.label("reset").init_stack();
    a.sbi(io::DDRB, 5);
    // Timer 1, CTC with TOP = OCR1A = 31249, clk/256: 0.5 s per match.
    a.ldi(16, 0x7A).sts(mem::OCR1AH, 16);
    a.ldi(16, 0x11).sts(mem::OCR1AL, 16);
    a.ldi(16, 0x0C).sts(mem::TCCR1B, 16);
    a.ldi(16, 0x02).sts(mem::TIMSK1, 16);
    a.ldi(16, 0x01).out(io::SMCR, 16); // idle sleep
    a.sei();
    a.label("loop").sleep().rjmp("loop");
    a.label("t1_compa").sbi(io::PINB, 5).reti();
    a
}

fn fade() -> Asm {
    let mut a = Asm::new();
    a.vectors(&[(0, "reset"), (16, "t0_ovf")], "unused");
    a.label("unused").reti();
    a.label("reset").init_stack();
    a.sbi(io::DDRD, 6);
    // Timer 0 fast PWM, non-inverting on OC0A (D6), clk/64: 976.6 Hz.
    a.ldi(16, 0x83).out(io::TCCR0A, 16);
    a.ldi(16, 0x03).out(io::TCCR0B, 16);
    a.ldi(16, 0x01).sts(mem::TIMSK0, 16);
    // r17 duty, r18 step (+1 / -1), r19 overflow count.
    a.clr(17).ldi(18, 1).clr(19);
    a.out(io::OCR0A, 17);
    a.ldi(16, 0x01).out(io::SMCR, 16);
    a.sei();
    a.label("loop").sleep().rjmp("loop");
    // Every 4 overflows (4.1 ms) move the duty one step.
    a.label("t0_ovf").push(16).in_(16, io::SREG).push(16);
    a.inc(19).cpi(19, 4).brne("done");
    a.clr(19).add(17, 18).out(io::OCR0A, 17);
    a.cpi(17, 255).brne("not_top").ldi(18, 0xFF).rjmp("done");
    a.label("not_top").cpi(17, 0).brne("done").ldi(18, 1);
    a.label("done").pop(16).out(io::SREG, 16).pop(16).reti();
    a
}

fn pot_pwm_serial() -> Asm {
    let mut a = Asm::new();
    a.vectors(&[(0, "reset")], "unused");
    a.label("unused").reti();
    a.label("reset").init_stack();
    a.sbi(io::DDRD, 6);
    a.ldi(16, 0x83).out(io::TCCR0A, 16);
    a.ldi(16, 0x03).out(io::TCCR0B, 16);
    // ADC: AVCC reference, left-adjusted, channel 0, enabled, clk/128.
    a.ldi(16, 0x60).sts(mem::ADMUX, 16);
    a.ldi(16, 0x87).sts(mem::ADCSRA, 16);
    // USART: 9600 baud (UBRR = 103), transmitter on, 8N1.
    a.ldi(16, 0).sts(mem::UBRR0H, 16);
    a.ldi(16, 103).sts(mem::UBRR0L, 16);
    a.ldi(16, 0x08).sts(mem::UCSR0B, 16);
    a.ldi(16, 0x06).sts(mem::UCSR0C, 16);
    // Timer 1 CTC, clk/1024, OCR1A = 1562: a flag every 100 ms.
    a.ldi(16, 0x06).sts(mem::OCR1AH, 16);
    a.ldi(16, 0x1A).sts(mem::OCR1AL, 16);
    a.ldi(16, 0x0D).sts(mem::TCCR1B, 16);
    a.ldi_lo(30, "banner").ldi_hi(31, "banner").rcall("print");

    a.label("main");
    a.lds(16, mem::ADCSRA).ori(16, 0x40).sts(mem::ADCSRA, 16);
    a.label("wait_adc")
        .lds(16, mem::ADCSRA)
        .sbrc(16, 6)
        .rjmp("wait_adc");
    a.lds(24, mem::ADCL).lds(25, mem::ADCH);
    a.out(io::OCR0A, 25);
    a.sbis(io::TIFR1, 1).rjmp("main");
    a.sbi(io::TIFR1, 1);
    // r23:r22 = ADCH * 4 + ADCL >> 6 (the 10-bit result).
    a.swap(24).lsr(24).lsr(24).andi(24, 0x03);
    a.mov(22, 25).clr(23);
    a.lsl(22).rol(23).lsl(22).rol(23);
    a.or(22, 24);
    a.ldi_lo(30, "label").ldi_hi(31, "label").rcall("print");
    a.rcall("print_dec");
    a.ldi_lo(30, "crlf").ldi_hi(31, "crlf").rcall("print");
    a.rjmp("main");

    // Print the zero-terminated flash string at Z.
    a.label("print").lpm_zp(16).tst(16).breq("print_done");
    a.rcall("putc").rjmp("print");
    a.label("print_done").ret();

    // Send r16 once the transmit buffer is free.
    a.label("putc")
        .lds(17, mem::UCSR0A)
        .sbrs(17, 5)
        .rjmp("putc");
    a.sts(mem::UDR0, 16).ret();

    // Print r23:r22 (0–9999) as four decimal digits.
    a.label("print_dec");
    a.ldi(20, (1000 & 0xFF) as u8)
        .ldi(21, (1000 >> 8) as u8)
        .rcall("digit");
    a.ldi(20, 100).ldi(21, 0).rcall("digit");
    a.ldi(20, 10).ldi(21, 0).rcall("digit");
    a.mov(16, 22).subi(16, 0u8.wrapping_sub(b'0')).rjmp("putc");
    a.label("digit").ldi(16, b'0');
    a.label("digit_loop")
        .cp(22, 20)
        .cpc(23, 21)
        .brlo("digit_done");
    a.sub(22, 20).sbc(23, 21).inc(16).rjmp("digit_loop");
    a.label("digit_done").rjmp("putc");

    a.label("banner").bytes(b"Dunbrec AVR demo\r\n\0");
    a.label("label").bytes(b"A0 = \0");
    a.label("crlf").bytes(b"\r\n\0");
    a
}
