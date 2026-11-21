//! A disassembler for flash images, driven by the instruction table in
//! [`crate::isa`]. Output is AVR assembler syntax: registers as `r16`,
//! hexadecimal immediates, I/O registers by name (`sbi PORTB, 5`) and jump
//! targets as absolute *byte* addresses, as in the datasheet's listings.

use crate::isa::{
    BRANCH_CLEAR_NAMES, BRANCH_SET_NAMES, CLEAR_NAMES, Form, SET_NAMES, io_name, lookup,
    register_name,
};

/// Words of flash on the ATmega328P: relative jumps wrap at this size.
const FLASH_WORDS: usize = 16 * 1024;

/// One disassembled instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Word address.
    pub address: usize,
    /// The instruction's word or words.
    pub words: Vec<u16>,
    pub mnemonic: &'static str,
    pub operands: String,
    /// Word address a jump, call or branch goes to, if it is a fixed one.
    pub target: Option<usize>,
}

impl Line {
    /// The instruction as text: `ldi r16, 0xFF`.
    pub fn text(&self) -> String {
        if self.operands.is_empty() {
            self.mnemonic.to_string()
        } else {
            format!("{} {}", self.mnemonic, self.operands)
        }
    }

    /// Words the instruction occupies.
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Byte address of the instruction.
    pub fn byte_address(&self) -> usize {
        self.address * 2
    }
}

fn reg(n: usize) -> String {
    format!("r{n}")
}

fn byte_address(word: usize) -> String {
    format!("0x{:04X}", word * 2)
}

fn io(a: u8) -> String {
    io_name(a).map_or_else(|| format!("0x{a:02X}"), str::to_string)
}

fn data(addr: u16) -> String {
    register_name(addr).map_or_else(|| format!("0x{addr:04X}"), str::to_string)
}

/// Sign-extend the low `bits` bits of `v`.
fn signed(v: u16, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((v as i32) << shift) >> shift
}

fn relative(at: usize, offset: i32) -> usize {
    (at as i32 + 1 + offset).rem_euclid(FLASH_WORDS as i32) as usize
}

/// Disassemble the instruction at word address `at`. A word that is not an
/// instruction (data, or erased flash) comes out as `.word 0x....`.
pub fn disassemble_at(flash: &[u16], at: usize) -> Line {
    let word = flash.get(at).copied().unwrap_or(0xFFFF);
    let Some(op) = lookup(word) else {
        return Line {
            address: at,
            words: vec![word],
            mnemonic: ".word",
            operands: format!("0x{word:04X}"),
            target: None,
        };
    };
    let second = if op.words() == 2 {
        flash.get(at + 1).copied().unwrap_or(0)
    } else {
        0
    };
    let d5 = ((word >> 4) & 0x1F) as usize;
    let r5 = ((word & 0xF) | ((word >> 5) & 0x10)) as usize;
    let k8 = (((word >> 4) & 0xF0) | (word & 0xF)) as u8;
    let bit = word & 7;
    let mut mnemonic = op.mnemonic;
    let mut target = None;
    let operands = match op.form {
        Form::None => String::new(),
        Form::RdRr => format!("{}, {}", reg(d5), reg(r5)),
        Form::Rd => reg(d5),
        Form::RdK => format!("{}, 0x{k8:02X}", reg(16 + ((word >> 4) & 0xF) as usize)),
        Form::Movw => format!(
            "{}, {}",
            reg(((word >> 4) & 0xF) as usize * 2),
            reg((word & 0xF) as usize * 2)
        ),
        Form::Muls => format!(
            "{}, {}",
            reg(16 + ((word >> 4) & 0xF) as usize),
            reg(16 + (word & 0xF) as usize)
        ),
        Form::Mul8 => format!(
            "{}, {}",
            reg(16 + ((word >> 4) & 7) as usize),
            reg(16 + (word & 7) as usize)
        ),
        Form::PairK => {
            let k = ((word >> 2) & 0x30) | (word & 0xF);
            format!("{}, {k}", reg(24 + ((word >> 4) & 3) as usize * 2))
        }
        Form::IoBit => format!("{}, {bit}", io(((word >> 3) & 0x1F) as u8)),
        Form::In => {
            let a = (((word >> 5) & 0x30) | (word & 0xF)) as u8;
            format!("{}, {}", reg(d5), io(a))
        }
        Form::Out => {
            let a = (((word >> 5) & 0x30) | (word & 0xF)) as u8;
            format!("{}, {}", io(a), reg(d5))
        }
        Form::Rel12 => {
            let t = relative(at, signed(word & 0xFFF, 12));
            target = Some(t);
            byte_address(t)
        }
        Form::Branch { set } => {
            let flag = bit as usize;
            mnemonic = if set {
                BRANCH_SET_NAMES[flag]
            } else {
                BRANCH_CLEAR_NAMES[flag]
            };
            let t = relative(at, signed((word >> 3) & 0x7F, 7));
            target = Some(t);
            byte_address(t)
        }
        Form::Abs22 => {
            let high = (((word >> 3) & 0x3E) | (word & 1)) as usize;
            let t = (high << 16 | second as usize) % FLASH_WORDS;
            target = Some(t);
            byte_address(t)
        }
        Form::Lds => format!("{}, {}", reg(d5), data(second)),
        Form::Sts => format!("{}, {}", data(second), reg(d5)),
        Form::Load(ptr) => format!("{}, {ptr}", reg(d5)),
        Form::Store(ptr) => format!("{ptr}, {}", reg(d5)),
        Form::DispLoad { y } | Form::DispStore { y } => {
            let q = ((word >> 8) & 0x20) | ((word >> 7) & 0x18) | (word & 7);
            let base = if y { 'Y' } else { 'Z' };
            let pointer = if q == 0 {
                // LDD and STD with no displacement are plain LD and ST.
                mnemonic = if matches!(op.form, Form::DispLoad { .. }) {
                    "ld"
                } else {
                    "st"
                };
                base.to_string()
            } else {
                format!("{base}+{q}")
            };
            if matches!(op.form, Form::DispLoad { .. }) {
                format!("{}, {pointer}", reg(d5))
            } else {
                format!("{pointer}, {}", reg(d5))
            }
        }
        Form::RdBit => format!("{}, {bit}", reg(d5)),
        Form::Sreg { set } => {
            let flag = ((word >> 4) & 7) as usize;
            mnemonic = if set {
                SET_NAMES[flag]
            } else {
                CLEAR_NAMES[flag]
            };
            String::new()
        }
    };
    let mut words = vec![word];
    if op.words() == 2 {
        words.push(second);
    }
    Line {
        address: at,
        words,
        mnemonic,
        operands,
        target,
    }
}

/// Disassemble up to `count` instructions starting at word address `start`
/// (stopping at the end of `flash`).
pub fn disassemble(flash: &[u16], start: usize, count: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut at = start;
    while lines.len() < count && at < flash.len() {
        let line = disassemble_at(flash, at);
        at += line.len();
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::{Asm, io, mem};

    /// Assemble with the builder, then disassemble: the text of each line.
    fn listing(a: &Asm) -> Vec<String> {
        let image = a.finish().unwrap();
        let flash: Vec<u16> = image
            .chunks(2)
            .map(|p| p[0] as u16 | (*p.get(1).unwrap_or(&0) as u16) << 8)
            .collect();
        disassemble(&flash, 0, flash.len())
            .iter()
            .map(Line::text)
            .collect()
    }

    #[test]
    fn common_instructions_read_like_assembly() {
        let mut a = Asm::new();
        a.ldi(16, 0xFF).out(io::DDRB, 16);
        a.sbi(io::PORTB, 5).cbi(io::PORTB, 5);
        a.mov(1, 2).add(3, 4).inc(20).clr(21);
        a.lds(24, mem::UDR0).sts(mem::UCSR0B, 24);
        a.sei();
        a.sleep().reti();
        assert_eq!(
            listing(&a),
            [
                "ldi r16, 0xFF",
                "out DDRB, r16",
                "sbi PORTB, 5",
                "cbi PORTB, 5",
                "mov r1, r2",
                "add r3, r4",
                "inc r20",
                "eor r21, r21",
                "lds r24, UDR0",
                "sts UCSR0B, r24",
                "sei",
                "sleep",
                "reti",
            ]
        );
    }

    #[test]
    fn jumps_and_branches_show_absolute_byte_addresses() {
        let mut a = Asm::new();
        a.label("top").nop();
        a.rjmp("top"); // word 1 -> word 0
        a.brne("top"); // word 2 -> word 0
        a.label("end").jmp("end"); // words 3, 4 -> word 3
        let image = a.finish().unwrap();
        let flash: Vec<u16> = image
            .chunks(2)
            .map(|p| p[0] as u16 | (*p.get(1).unwrap_or(&0) as u16) << 8)
            .collect();
        let lines = disassemble(&flash, 0, 10);
        assert_eq!(lines.len(), 4, "the JMP is one line of two words");
        assert_eq!(lines[1].text(), "rjmp 0x0000");
        assert_eq!(lines[1].target, Some(0));
        assert_eq!(lines[2].text(), "brne 0x0000");
        assert_eq!(lines[3].text(), "jmp 0x0006");
        assert_eq!(
            (lines[3].address, lines[3].len(), lines[3].target),
            (3, 2, Some(3))
        );
        assert_eq!(lines[3].byte_address(), 6);
    }

    #[test]
    fn flag_and_branch_aliases_and_pointer_forms() {
        let at = |w: u16| disassemble_at(&[w, 0], 0).text();
        assert_eq!(at(0x9408), "sec");
        assert_eq!(at(0x9478), "sei");
        assert_eq!(at(0x94F8), "cli");
        assert_eq!(at(0xF409), "brne 0x0004", "branch +1 word from word 0");
        assert_eq!(at(0x9001), "ld r0, Z+");
        assert_eq!(at(0x920E), "st -X, r0");
        assert_eq!(at(0x8008), "ld r0, Y", "LDD with q = 0 is LD");
        assert_eq!(at(0x800A), "ldd r0, Y+2");
        assert_eq!(
            at(0xA827),
            "ldd r2, Z+55",
            "q uses bits 13, 11..10 and 2..0"
        );
        assert_eq!(at(0x8E2F), "std Y+31, r2", "bit 9 makes it a store");
        assert_eq!(at(0x95C8), "lpm");
        assert_eq!(at(0x9004), "lpm r0, Z");
        assert_eq!(at(0x9622), "adiw r28, 2");
        assert_eq!(at(0x01A4), "movw r20, r8");
        assert_eq!(at(0x0148), "movw r8, r16");
    }

    #[test]
    fn data_and_erased_flash_become_dot_word() {
        let lines = disassemble(&[0x0000, 0xFFFF, 0x1234], 0, 10);
        assert_eq!(lines[0].text(), "nop");
        assert_eq!(lines[1].text(), ".word 0xFFFF");
        assert_eq!(lines.len(), 3);
        // Beyond the image it is erased flash.
        assert_eq!(disassemble_at(&[], 5).text(), ".word 0xFFFF");
    }

    #[test]
    fn a_two_word_instruction_cut_off_by_the_end_still_reads() {
        let line = disassemble_at(&[0x9100], 0);
        assert_eq!(line.mnemonic, "lds");
        assert_eq!(line.len(), 2);
    }

    #[test]
    fn the_builtin_demo_programs_disassemble_without_unknown_words() {
        for (name, _) in crate::programs::BUILTIN {
            let image = crate::programs::builtin(name).unwrap();
            let flash: Vec<u16> = image
                .chunks(2)
                .map(|p| p[0] as u16 | (*p.get(1).unwrap_or(&0) as u16) << 8)
                .collect();
            let lines = disassemble(&flash, 0, flash.len());
            // Strings the programs keep in flash (for LPM) are data; the
            // rest must all be real instructions.
            let unknown = lines.iter().filter(|l| l.mnemonic == ".word").count();
            assert!(
                unknown * 4 <= lines.len(),
                "{name}: {unknown} of {} words are unknown",
                lines.len()
            );
            assert!(
                lines
                    .iter()
                    .any(|l| l.mnemonic == "reti" || l.mnemonic == "rjmp")
            );
        }
    }
}
