use super::*;
use crate::asm::Asm;
use crate::disasm::disassemble_at;
use crate::isa::lookup;
use crate::programs::builtin;

fn ok(source: &str) -> Assembled {
    match assemble(source) {
        Ok(a) => a,
        Err(e) => panic!("{source}\n{e:#?}"),
    }
}

fn words(source: &str) -> Vec<u16> {
    ok(source)
        .image
        .chunks(2)
        .map(|p| p[0] as u16 | (p[1] as u16) << 8)
        .collect()
}

fn errors(source: &str) -> Vec<(usize, usize, String)> {
    assemble(source)
        .expect_err("the source should not assemble")
        .into_iter()
        .map(|e| (e.line, e.column, e.message))
        .collect()
}

/// Every instruction word the chip has: disassemble it, assemble the text
/// again, get the same word back.
#[test]
fn every_instruction_word_survives_a_round_trip_through_the_disassembler() {
    const AT: usize = 0;
    let mut failures = Vec::new();
    let mut checked = 0;
    for word in 0..=0xFFFFu32 {
        let word = word as u16;
        let Some(op) = lookup(word) else { continue };
        // Two-word instructions: try a few second words.
        let seconds: &[u16] = if op.words() == 2 {
            &[0x0000, 0x00C6, 0x1234, 0x2FFF]
        } else {
            &[0]
        };
        for &second in seconds {
            let mut flash = vec![0u16; AT + 2];
            flash[AT] = word;
            flash[AT + 1] = second;
            let line = disassemble_at(&flash, AT);
            // Jumps print byte addresses; the assembler wants word ones.
            let operands = match (line.target, op.form) {
                // Relative jumps wrap around the end of flash: write the
                // distance as `PC+k`, k the short way round.
                (Some(t), crate::isa::Form::Rel12 | crate::isa::Form::Branch { .. }) => {
                    let mut k = t as i64 - AT as i64;
                    if k > 8191 {
                        k -= 16384;
                    }
                    format!("PC{k:+}")
                }
                (Some(t), _) => t.to_string(),
                (None, _) => line.operands.clone(),
            };
            let text = format!("{} {operands}", line.mnemonic);
            let mut expected: Vec<u16> = line.words.clone();
            if op.form == crate::isa::Form::Abs22 {
                // The chip has 16K words, so the upper address bits of JMP
                // and CALL do not exist: the disassembler drops them, and
                // so the assembler writes them as zero.
                expected[0] &= !0x01F1;
            }
            checked += 1;
            match assemble(&text) {
                Ok(a) => {
                    let got: Vec<u16> = a.image[AT * 2..]
                        .chunks(2)
                        .map(|p| p[0] as u16 | (p[1] as u16) << 8)
                        .collect();
                    if got != expected {
                        failures.push(format!("{text:?}: {got:04X?} instead of {expected:04X?}"));
                    }
                }
                Err(e) => failures.push(format!("{text:?}: {}", e[0])),
            }
        }
    }
    assert!(checked > 60_000, "only {checked} cases");
    assert!(
        failures.is_empty(),
        "{} failures; first: {:#?}",
        failures.len(),
        &failures[..failures.len().min(12)]
    );
}

#[test]
fn a_blink_program_assembles_to_the_known_bytes() {
    let p = ok("
        sbi  DDRB, PB5     ; output
loop:   sbi  PINB, PB5     // toggle
        rjmp loop
");
    // sbi 0x04,5 / sbi 0x03,5 / rjmp -2
    assert_eq!(p.image, [0x25, 0x9A, 0x1D, 0x9A, 0xFE, 0xCF]);
    assert_eq!(p.lines, [(0, 2), (1, 3), (2, 4)]);
    assert_eq!(p.label_address("LOOP"), Some(1));
}

#[test]
fn the_built_in_blink_written_as_text_matches_the_builder_byte_for_byte() {
    let mut vectors = String::new();
    for v in 0..26 {
        let target = match v {
            0 => "reset",
            11 => "t1_compa",
            _ => "unused",
        };
        vectors.push_str(&format!("        jmp {target}\n"));
    }
    let source = format!(
        "{vectors}
unused:  reti
reset:   ldi r16, high(RAMEND)
         out SPH, r16
         ldi r16, low(RAMEND)
         out SPL, r16
         sbi DDRB, 5
         ; Timer 1, CTC, TOP = 31249, clk/256: 0.5 s per match.
         ldi r16, 0x7A
         sts OCR1AH, r16
         ldi r16, 0x11
         sts OCR1AL, r16
         ldi r16, (1<<WGM12)|(1<<CS12)
         sts TCCR1B, r16
         ldi r16, 1<<OCIE1A
         sts TIMSK1, r16
         ldi r16, 1<<SE
         out SMCR, r16
         sei
loop:    sleep
         rjmp loop
t1_compa: sbi PINB, 5
         reti
"
    );
    assert_eq!(ok(&source).image, builtin("blink").unwrap());
}

#[test]
fn the_built_in_fade_written_as_text_matches_the_builder_byte_for_byte() {
    let mut vectors = String::new();
    for v in 0..26 {
        let target = match v {
            0 => "reset",
            16 => "t0_ovf",
            _ => "unused",
        };
        vectors.push_str(&format!("        jmp {target}\n"));
    }
    let source = format!(
        "{vectors}
.def duty = r17
.def step = r18
.def count = r19
unused:  reti
reset:   ldi r16, high(RAMEND)
         out SPH, r16
         ldi r16, low(RAMEND)
         out SPL, r16
         sbi DDRD, PD6
         ldi r16, (1<<COM0A1)|(1<<WGM01)|(1<<WGM00)
         out TCCR0A, r16
         ldi r16, (1<<CS01)|(1<<CS00)
         out TCCR0B, r16
         ldi r16, 1<<TOIE0
         sts TIMSK0, r16
         clr duty
         ldi step, 1
         clr count
         out OCR0A, duty
         ldi r16, 1
         out SMCR, r16
         sei
loop:    sleep
         rjmp loop
t0_ovf:  push r16
         in r16, SREG
         push r16
         inc count
         cpi count, 4
         brne done
         clr count
         add duty, step
         out OCR0A, duty
         cpi duty, 255
         brne not_top
         ldi step, 0xFF
         rjmp done
not_top: cpi duty, 0
         brne done
         ldi step, 1
done:    pop r16
         out SREG, r16
         pop r16
         reti
"
    );
    assert_eq!(ok(&source).image, builtin("fade").unwrap());
}

#[test]
fn aliases_match_the_builder() {
    let mut a = Asm::new();
    a.clr(3).tst(4).lsl(5).rol(6).ldi(20, 0xFF);
    a.ori(21, 0x0C).andi(22, !0x0Cu8);
    a.sec().clc().sei().cli().set().clt();
    a.label("x").breq("x").brne("x").brlo("x");
    let expected = a.finish().unwrap();
    let p = ok("
        clr r3
        tst r4
        lsl r5
        rol r6
        ser r20
        sbr r21, 0x0C
        cbr r22, 0x0C
        sec
        clc
        sei
        cli
        set
        clt
x:      breq x
        brne x
        brlo x
");
    assert_eq!(p.image, expected);
}

#[test]
fn pointer_instructions() {
    let w = words(
        "
        ld r1, X
        ld r2, X+
        ld r3, -X
        ld r4, Y
        ld r5, Y+
        ld r6, -Y
        ld r7, Z
        ld r8, Z+
        ld r9, -Z
        ldd r10, Y+3
        ldd r11, Z+63
        st X, r1
        st Y+, r2
        std Y+5, r3
        std Z+0, r4
        lpm
        lpm r12, Z
        lpm r13, Z+
        movw r25:r24, r27:r26
        movw r2, r4
        adiw r24, 1
        sbiw ZL, 63
        adiw X, 2
    ",
    );
    let text: Vec<String> = {
        let mut out = Vec::new();
        let mut at = 0;
        while at < w.len() {
            let line = disassemble_at(&w, at);
            at += line.len();
            out.push(line.text());
        }
        out
    };
    assert_eq!(
        text,
        [
            "ld r1, X",
            "ld r2, X+",
            "ld r3, -X",
            "ld r4, Y",
            "ld r5, Y+",
            "ld r6, -Y",
            "ld r7, Z",
            "ld r8, Z+",
            "ld r9, -Z",
            "ldd r10, Y+3",
            "ldd r11, Z+63",
            "st X, r1",
            "st Y+, r2",
            "std Y+5, r3",
            "st Z, r4",
            "lpm",
            "lpm r12, Z",
            "lpm r13, Z+",
            "movw r24, r26",
            "movw r2, r4",
            "adiw r24, 1",
            "sbiw r30, 63",
            "adiw r26, 2",
        ]
    );
}

/// The disassembly of what `source` assembles to, one string per instruction.
fn listing(source: &str) -> Vec<String> {
    let w = words(source);
    let mut out = Vec::new();
    let mut at = 0;
    while at < w.len() {
        let line = disassemble_at(&w, at);
        at += line.len();
        out.push(line.text());
    }
    out
}

#[test]
fn io_names_give_the_right_number_for_each_instruction() {
    // DDRB is I/O address 0x04 for OUT and data address 0x24 for LDS.
    assert_eq!(
        listing(
            "
        out DDRB, r16
        lds r16, DDRB
        sts UCSR0B, r16
        in r0, SREG
        lds r1, 0x24
        sbi PORTB, PB5
        "
        ),
        [
            "out DDRB, r16",
            "lds r16, DDRB",
            "sts UCSR0B, r16",
            "in r0, SREG",
            "lds r1, DDRB",
            "sbi PORTB, 5",
        ]
    );
    assert_eq!(words("lds r16, DDRB")[1], 0x24);
    assert_eq!(words("out DDRB, r16")[0] & 0x000F, 0x4);
}

#[test]
fn directives_and_expressions() {
    let source = "
.equ CLOCK = 16000000
.equ BAUD = 9600
.equ UBRR = CLOCK / (16 * BAUD) - 1
        ldi r16, low(UBRR)
        ldi r17, high(UBRR + 256)
        ldi r18, ~0x0F & 0xFF
        ldi r19, -1
        ldi r20, 'A'
        ldi r21, $2A
        ldi r22, 0b1010
        ldi r23, (3 + 4) * 2 << 1
        rjmp PC+1
        nop
        ldi r24, lo8(table * 2)
        ldi r25, hi8(table * 2)
table:  .db 1, 2, 3
        .db \"Hi\", 0
        .dw 0x1234, table
";
    assert_eq!(
        listing(source)[..8],
        [
            "ldi r16, 0x67",
            "ldi r17, 0x01",
            "ldi r18, 0xF0",
            "ldi r19, 0xFF",
            "ldi r20, 0x41",
            "ldi r21, 0x2A",
            "ldi r22, 0x0A",
            "ldi r23, 0x1C",
        ]
    );
    let w = words(source);
    // rjmp PC+1 skips the NOP: offset 0.
    assert_eq!(w[8], 0xC000);
    // table is at word 12 (byte 24): lo8 = 24, hi8 = 0.
    assert_eq!(w[10], 0xE188); // ldi r24, 24
    // .db 1,2,3 pads to 4 bytes; "Hi",0 pads to 4 bytes; .dw
    assert_eq!(&w[12..], [0x0201, 0x0003, 0x6948, 0x0000, 0x1234, 12]);
}

#[test]
fn errors_name_the_line_and_column() {
    let e = errors("nop\n  ldi r16, bogus\n");
    assert_eq!(e, [(2, 12, "'bogus' is not defined".to_string())]);

    let e = errors("ldi r1, 5");
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].0, e[0].1), (1, 5));
    assert!(e[0].2.contains("r16 to r31"), "{e:?}");

    let e = errors("frob r1");
    assert_eq!((e[0].0, e[0].1), (1, 1));
    assert!(e[0].2.contains("not an instruction"));

    let e = errors("ldi r16, 300");
    assert!(e[0].2.contains("between -128 and 255"), "{e:?}");

    let e = errors("ldi r16");
    assert!(
        e[0].2.contains("takes 2 operands, but 1 operand given"),
        "{e:?}"
    );

    let e = errors("x: nop\nx: nop");
    assert_eq!((e[0].0, e[0].1), (2, 1));
    assert!(e[0].2.contains("already defined"));

    let e = errors("rjmp far\n.org 0x1000\nfar: nop");
    assert!(e[0].2.contains("reaches only -2048 to +2047"), "{e:?}");

    let e = errors("ld r1, Y+3");
    assert!(e[0].2.contains("use ldd"), "{e:?}");

    let e = errors(".dseg\n.macro m\n.include \"x\"");
    assert_eq!(e.len(), 3);

    let e = errors("ldi r16, (1 << 2");
    assert!(e[0].2.contains("closing parenthesis"), "{e:?}");

    let e = errors(".org 4\nnop\n.org 2\nnop");
    assert!(e[0].2.contains("goes back"), "{e:?}");
}

#[test]
fn every_error_is_reported_not_only_the_first() {
    let e = errors("ldi r16, a\nnop\nldi r17, b\nbad\n");
    assert_eq!(e.iter().map(|e| e.0).collect::<Vec<_>>(), [1, 3, 4]);
}

#[test]
fn source_lines_map_to_addresses_and_back() {
    let p = ok("
        ldi r16, 1      ; line 2
        ; only a comment
start:
        lds r16, UDR0   ; line 5, two words
        nop             ; line 6
");
    assert_eq!(p.lines, [(0, 2), (1, 5), (3, 6)]);
    assert_eq!(p.line_for_address(1), Some(5));
    assert_eq!(p.line_for_address(2), None); // the second word of LDS
    // A breakpoint on a comment line lands on the next instruction.
    assert_eq!(p.address_for_line(3), Some(1));
    assert_eq!(p.address_for_line(6), Some(3));
    assert_eq!(p.address_for_line(7), None);
    assert_eq!(p.label_address("start"), Some(1));
}

#[test]
fn org_leaves_erased_flash_and_the_image_runs_on_the_chip() {
    let p = ok("
        rjmp main
.org 4
main:   ldi r16, 0xAA
        out PORTB, r16
        sleep
");
    assert_eq!(p.image[2..8], [0xFF; 6]);
    // The result loads and runs.
    let mut chip = crate::Atmega328p::new(&p.image).unwrap();
    chip.step_instruction(u64::MAX);
    chip.step_instruction(u64::MAX);
    assert_eq!(chip.pc(), 5);
}

#[test]
fn comments_do_not_hide_in_strings_and_strings_do_not_hide_comments() {
    let w = words(
        ".db \"a;b//c\", '\"'  ; trailing
.dw ';'",
    );
    // 61 3B 62 2F 2F 63 22, padded with a zero byte; then the .dw.
    assert_eq!(w, [0x3B61, 0x2F62, 0x632F, 0x0022, 0x003B]);
}
