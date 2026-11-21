//! The instruction set of the ATmega328P as data: one table that says which
//! 16-bit words are instructions, what they are called and how their
//! operands are packed. The disassembler reads it today; the text assembler
//! (phase B) will too, so the two cannot drift apart. A test runs every
//! possible word through the emulator and checks that the table accepts
//! exactly the words the emulator executes.
//!
//! ELPM is in the table because the emulator runs it (as LPM: the chip has
//! no RAMPZ), although the real ATmega328P does not have it.

/// How an instruction packs its operands into the word(s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// No operands.
    None,
    /// `Rd, Rr`: d in bits 8..4, r in bit 9 and bits 3..0.
    RdRr,
    /// `Rd`: bits 8..4.
    Rd,
    /// `Rd, K` for r16..r31: d = 16 + bits 7..4, K in bits 11..8 and 3..0.
    RdK,
    /// MOVW: register pairs, bits 7..4 and 3..0 (each times two).
    Movw,
    /// MULS: r16..r31, bits 7..4 and 3..0.
    Muls,
    /// MULSU, FMUL, FMULS, FMULSU: r16..r23, bits 6..4 and 2..0.
    Mul8,
    /// ADIW and SBIW: pair r24, r26, r28 or r30 (bits 5..4), K in bits 7..6
    /// and 3..0.
    PairK,
    /// SBI, CBI, SBIC, SBIS: I/O address in bits 7..3, bit in bits 2..0.
    IoBit,
    /// IN `Rd, A`: A in bits 10..9 and 3..0.
    In,
    /// OUT `A, Rr`.
    Out,
    /// RJMP and RCALL: signed 12-bit offset in words.
    Rel12,
    /// BRBS and BRBC: signed 7-bit offset in words (bits 9..3) and the SREG
    /// flag (bits 2..0).
    Branch { set: bool },
    /// JMP and CALL: 22-bit word address (bits 8..4 and 0 of the first word,
    /// then the second word). Two words.
    Abs22,
    /// LDS `Rd, k`: 16-bit data address in the second word. Two words.
    Lds,
    /// STS `k, Rr`. Two words.
    Sts,
    /// LD and LPM through a pointer; the text is the pointer operand.
    Load(&'static str),
    /// ST through a pointer.
    Store(&'static str),
    /// LDD `Rd, Y+q` or `Z+q`: q in bits 13, 11..10 and 2..0.
    DispLoad { y: bool },
    /// STD `Y+q, Rr` or `Z+q, Rr`.
    DispStore { y: bool },
    /// BLD, BST, SBRC, SBRS: `Rd, b` with b in bits 2..0.
    RdBit,
    /// BSET and BCLR: the SREG flag in bits 6..4.
    Sreg { set: bool },
}

/// One instruction: any word where `word & mask == pattern`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Op {
    pub mnemonic: &'static str,
    pub pattern: u16,
    pub mask: u16,
    pub form: Form,
}

impl Op {
    /// Words the instruction occupies in flash (1 or 2).
    pub fn words(&self) -> usize {
        match self.form {
            Form::Abs22 | Form::Lds | Form::Sts => 2,
            _ => 1,
        }
    }

    pub fn matches(&self, word: u16) -> bool {
        word & self.mask == self.pattern
    }
}

const fn op(mnemonic: &'static str, pattern: u16, mask: u16, form: Form) -> Op {
    Op {
        mnemonic,
        pattern,
        mask,
        form,
    }
}

use Form::*;

/// Every instruction of the chip.
pub static OPS: &[Op] = &[
    op("nop", 0x0000, 0xFFFF, None),
    op("movw", 0x0100, 0xFF00, Movw),
    op("muls", 0x0200, 0xFF00, Muls),
    op("mulsu", 0x0300, 0xFF88, Mul8),
    op("fmul", 0x0308, 0xFF88, Mul8),
    op("fmuls", 0x0380, 0xFF88, Mul8),
    op("fmulsu", 0x0388, 0xFF88, Mul8),
    op("cpc", 0x0400, 0xFC00, RdRr),
    op("sbc", 0x0800, 0xFC00, RdRr),
    op("add", 0x0C00, 0xFC00, RdRr),
    op("cpse", 0x1000, 0xFC00, RdRr),
    op("cp", 0x1400, 0xFC00, RdRr),
    op("sub", 0x1800, 0xFC00, RdRr),
    op("adc", 0x1C00, 0xFC00, RdRr),
    op("and", 0x2000, 0xFC00, RdRr),
    op("eor", 0x2400, 0xFC00, RdRr),
    op("or", 0x2800, 0xFC00, RdRr),
    op("mov", 0x2C00, 0xFC00, RdRr),
    op("cpi", 0x3000, 0xF000, RdK),
    op("sbci", 0x4000, 0xF000, RdK),
    op("subi", 0x5000, 0xF000, RdK),
    op("ori", 0x6000, 0xF000, RdK),
    op("andi", 0x7000, 0xF000, RdK),
    op("ldd", 0x8000, 0xD208, DispLoad { y: false }),
    op("ldd", 0x8008, 0xD208, DispLoad { y: true }),
    op("std", 0x8200, 0xD208, DispStore { y: false }),
    op("std", 0x8208, 0xD208, DispStore { y: true }),
    op("lds", 0x9000, 0xFE0F, Lds),
    op("ld", 0x9001, 0xFE0F, Load("Z+")),
    op("ld", 0x9002, 0xFE0F, Load("-Z")),
    op("lpm", 0x9004, 0xFE0F, Load("Z")),
    op("lpm", 0x9005, 0xFE0F, Load("Z+")),
    op("elpm", 0x9006, 0xFE0F, Load("Z")),
    op("elpm", 0x9007, 0xFE0F, Load("Z+")),
    op("ld", 0x9009, 0xFE0F, Load("Y+")),
    op("ld", 0x900A, 0xFE0F, Load("-Y")),
    op("ld", 0x900C, 0xFE0F, Load("X")),
    op("ld", 0x900D, 0xFE0F, Load("X+")),
    op("ld", 0x900E, 0xFE0F, Load("-X")),
    op("pop", 0x900F, 0xFE0F, Rd),
    op("sts", 0x9200, 0xFE0F, Sts),
    op("st", 0x9201, 0xFE0F, Store("Z+")),
    op("st", 0x9202, 0xFE0F, Store("-Z")),
    op("st", 0x9209, 0xFE0F, Store("Y+")),
    op("st", 0x920A, 0xFE0F, Store("-Y")),
    op("st", 0x920C, 0xFE0F, Store("X")),
    op("st", 0x920D, 0xFE0F, Store("X+")),
    op("st", 0x920E, 0xFE0F, Store("-X")),
    op("push", 0x920F, 0xFE0F, Rd),
    op("com", 0x9400, 0xFE0F, Rd),
    op("neg", 0x9401, 0xFE0F, Rd),
    op("swap", 0x9402, 0xFE0F, Rd),
    op("inc", 0x9403, 0xFE0F, Rd),
    op("asr", 0x9405, 0xFE0F, Rd),
    op("lsr", 0x9406, 0xFE0F, Rd),
    op("ror", 0x9407, 0xFE0F, Rd),
    op("bset", 0x9408, 0xFF8F, Sreg { set: true }),
    op("ijmp", 0x9409, 0xFFFF, None),
    op("dec", 0x940A, 0xFE0F, Rd),
    op("jmp", 0x940C, 0xFE0E, Abs22),
    op("call", 0x940E, 0xFE0E, Abs22),
    op("bclr", 0x9488, 0xFF8F, Sreg { set: false }),
    op("ret", 0x9508, 0xFFFF, None),
    op("icall", 0x9509, 0xFFFF, None),
    op("reti", 0x9518, 0xFFFF, None),
    op("sleep", 0x9588, 0xFFFF, None),
    op("break", 0x9598, 0xFFFF, None),
    op("wdr", 0x95A8, 0xFFFF, None),
    op("lpm", 0x95C8, 0xFFFF, None),
    op("elpm", 0x95D8, 0xFFFF, None),
    op("spm", 0x95E8, 0xFFFF, None),
    op("adiw", 0x9600, 0xFF00, PairK),
    op("sbiw", 0x9700, 0xFF00, PairK),
    op("cbi", 0x9800, 0xFF00, IoBit),
    op("sbic", 0x9900, 0xFF00, IoBit),
    op("sbi", 0x9A00, 0xFF00, IoBit),
    op("sbis", 0x9B00, 0xFF00, IoBit),
    op("mul", 0x9C00, 0xFC00, RdRr),
    op("in", 0xB000, 0xF800, In),
    op("out", 0xB800, 0xF800, Out),
    op("rjmp", 0xC000, 0xF000, Rel12),
    op("rcall", 0xD000, 0xF000, Rel12),
    op("ldi", 0xE000, 0xF000, RdK),
    op("brbs", 0xF000, 0xFC00, Branch { set: true }),
    op("brbc", 0xF400, 0xFC00, Branch { set: false }),
    op("bld", 0xF800, 0xFE08, RdBit),
    op("bst", 0xFA00, 0xFE08, RdBit),
    op("sbrc", 0xFC00, 0xFE08, RdBit),
    op("sbrs", 0xFE00, 0xFE08, RdBit),
];

/// The instruction `word` encodes, if it encodes one.
pub fn lookup(word: u16) -> Option<&'static Op> {
    OPS.iter().find(|o| o.matches(word))
}

/// Names of the SREG flags, bit 0 to 7.
pub const SREG_FLAGS: [&str; 8] = ["C", "Z", "N", "V", "S", "H", "T", "I"];

/// BSET and BCLR by flag: SEC, SEZ, ... and CLC, CLZ, ...
pub const SET_NAMES: [&str; 8] = ["sec", "sez", "sen", "sev", "ses", "seh", "set", "sei"];
pub const CLEAR_NAMES: [&str; 8] = ["clc", "clz", "cln", "clv", "cls", "clh", "clt", "cli"];

/// BRBS and BRBC by flag: BRCS, BREQ, ... and BRCC, BRNE, ...
pub const BRANCH_SET_NAMES: [&str; 8] = [
    "brcs", "breq", "brmi", "brvs", "brlt", "brhs", "brts", "brie",
];
pub const BRANCH_CLEAR_NAMES: [&str; 8] = [
    "brcc", "brne", "brpl", "brvc", "brge", "brhc", "brtc", "brid",
];

/// Registers of the ATmega328P by data-space address (I/O registers sit at
/// their I/O address plus 0x20).
pub static REGISTERS: &[(u16, &str)] = &[
    (0x23, "PINB"),
    (0x24, "DDRB"),
    (0x25, "PORTB"),
    (0x26, "PINC"),
    (0x27, "DDRC"),
    (0x28, "PORTC"),
    (0x29, "PIND"),
    (0x2A, "DDRD"),
    (0x2B, "PORTD"),
    (0x35, "TIFR0"),
    (0x36, "TIFR1"),
    (0x37, "TIFR2"),
    (0x3B, "PCIFR"),
    (0x3C, "EIFR"),
    (0x3D, "EIMSK"),
    (0x3E, "GPIOR0"),
    (0x3F, "EECR"),
    (0x40, "EEDR"),
    (0x41, "EEARL"),
    (0x42, "EEARH"),
    (0x43, "GTCCR"),
    (0x44, "TCCR0A"),
    (0x45, "TCCR0B"),
    (0x46, "TCNT0"),
    (0x47, "OCR0A"),
    (0x48, "OCR0B"),
    (0x4A, "GPIOR1"),
    (0x4B, "GPIOR2"),
    (0x4C, "SPCR"),
    (0x4D, "SPSR"),
    (0x4E, "SPDR"),
    (0x53, "SMCR"),
    (0x55, "MCUCR"),
    (0x5D, "SPL"),
    (0x5E, "SPH"),
    (0x5F, "SREG"),
    (0x68, "PCICR"),
    (0x69, "EICRA"),
    (0x6B, "PCMSK0"),
    (0x6C, "PCMSK1"),
    (0x6D, "PCMSK2"),
    (0x6E, "TIMSK0"),
    (0x6F, "TIMSK1"),
    (0x70, "TIMSK2"),
    (0x78, "ADCL"),
    (0x79, "ADCH"),
    (0x7A, "ADCSRA"),
    (0x7B, "ADCSRB"),
    (0x7C, "ADMUX"),
    (0x80, "TCCR1A"),
    (0x81, "TCCR1B"),
    (0x82, "TCCR1C"),
    (0x84, "TCNT1L"),
    (0x85, "TCNT1H"),
    (0x86, "ICR1L"),
    (0x87, "ICR1H"),
    (0x88, "OCR1AL"),
    (0x89, "OCR1AH"),
    (0x8A, "OCR1BL"),
    (0x8B, "OCR1BH"),
    (0xB0, "TCCR2A"),
    (0xB1, "TCCR2B"),
    (0xB2, "TCNT2"),
    (0xB3, "OCR2A"),
    (0xB4, "OCR2B"),
    (0xC0, "UCSR0A"),
    (0xC1, "UCSR0B"),
    (0xC2, "UCSR0C"),
    (0xC4, "UBRR0L"),
    (0xC5, "UBRR0H"),
    (0xC6, "UDR0"),
];

/// Name of the register at data-space address `addr`.
pub fn register_name(addr: u16) -> Option<&'static str> {
    REGISTERS.iter().find(|(a, _)| *a == addr).map(|(_, n)| *n)
}

/// Name of the register at I/O address `io` (0 to 63, as IN and OUT use).
pub fn io_name(io: u8) -> Option<&'static str> {
    register_name(0x20 + io as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Atmega328p;
    use crate::asm::{io, mem};

    #[test]
    fn the_table_accepts_exactly_the_words_the_emulator_runs() {
        let mut disagree = Vec::new();
        for word in 0..=0xFFFFu32 {
            let word = word as u16;
            let in_table = lookup(word).is_some();
            // The word at address 0, then a NOP; pointers and registers
            // start at zero so nothing here depends on state.
            let image = [word as u8, (word >> 8) as u8, 0, 0];
            let mut chip = Atmega328p::new(&image).unwrap();
            // A word the emulator accepts may still trip over the empty
            // machine around it (say, RET from an empty stack): that is
            // not a refusal to decode it.
            let accepted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                chip.step_instruction(u64::MAX);
                chip.fault().is_none()
            }))
            .unwrap_or(true);
            if accepted != in_table {
                disagree.push((word, in_table));
            }
        }
        assert!(
            disagree.is_empty(),
            "{} words differ (word, table accepts it); first: {:04X?}",
            disagree.len(),
            &disagree[..disagree.len().min(16)]
        );
    }

    #[test]
    fn two_word_instructions_advance_the_program_counter_by_two() {
        // For instructions that do not jump, the emulator's pc move agrees
        // with the table's size.
        for word in 0..=0xFFFFu32 {
            let word = word as u16;
            let Some(op) = lookup(word) else { continue };
            if matches!(op.form, Rel12 | Branch { .. } | Abs22)
                || matches!(op.mnemonic, "ijmp" | "icall" | "ret" | "reti")
                || matches!(op.mnemonic, "sbic" | "sbis" | "cpse" | "sbrc" | "sbrs")
            {
                continue;
            }
            let image = [word as u8, (word >> 8) as u8, 0, 0];
            let mut chip = Atmega328p::new(&image).unwrap();
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                chip.step_instruction(u64::MAX);
                chip.pc()
            }));
            if let Ok(pc) = ran {
                assert_eq!(pc, op.words(), "{} ({word:04X})", op.mnemonic);
            }
        }
    }

    #[test]
    fn patterns_do_not_overlap_and_fit_their_masks() {
        for (i, a) in OPS.iter().enumerate() {
            assert_eq!(
                a.pattern & !a.mask,
                0,
                "{} pattern outside its mask",
                a.mnemonic
            );
            for b in &OPS[i + 1..] {
                // Two entries overlap if some word matches both.
                let common = a.mask & b.mask;
                assert!(
                    (a.pattern & common) != (b.pattern & common),
                    "{} and {} overlap",
                    a.mnemonic,
                    b.mnemonic
                );
            }
        }
    }

    #[test]
    fn register_names_agree_with_the_assemblers_constants() {
        let io_names = [
            (io::PINB, "PINB"),
            (io::DDRB, "DDRB"),
            (io::PORTB, "PORTB"),
            (io::PIND, "PIND"),
            (io::DDRD, "DDRD"),
            (io::PORTD, "PORTD"),
            (io::TCCR0B, "TCCR0B"),
            (io::OCR0A, "OCR0A"),
            (io::SMCR, "SMCR"),
            (io::SPL, "SPL"),
            (io::SPH, "SPH"),
            (io::SREG, "SREG"),
        ];
        for (addr, name) in io_names {
            assert_eq!(io_name(addr), Some(name));
        }
        let mem_names = [
            (mem::TIMSK0, "TIMSK0"),
            (mem::ADMUX, "ADMUX"),
            (mem::ADCSRA, "ADCSRA"),
            (mem::TCCR1B, "TCCR1B"),
            (mem::OCR1AL, "OCR1AL"),
            (mem::UCSR0B, "UCSR0B"),
            (mem::UDR0, "UDR0"),
        ];
        for (addr, name) in mem_names {
            assert_eq!(register_name(addr), Some(name));
        }
    }
}
