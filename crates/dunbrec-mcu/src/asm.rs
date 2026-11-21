//! A small AVR assembler, enough to write test programs and the built-in
//! demo firmware in Rust without an external toolchain.
//!
//! Instructions are methods named after their mnemonics; branch targets are
//! labels resolved by [`Asm::finish`].
//!
//! ```
//! use dunbrec_mcu::asm::Asm;
//! let mut a = Asm::new();
//! a.sbi(0x04, 5); // DDRB |= 1 << 5
//! a.label("loop");
//! a.sbi(0x03, 5); // toggle PB5
//! a.rjmp("loop");
//! let image = a.finish().unwrap();
//! assert_eq!(image.len(), 6);
//! ```

use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
enum Fix {
    /// 12-bit relative (RJMP, RCALL).
    Rel12,
    /// 7-bit relative (conditional branches).
    Rel7,
    /// 22-bit absolute in the following word (JMP, CALL).
    Abs22,
    /// LDI with the low / high byte of the label's *byte* address (for LPM).
    ByteLo,
    ByteHi,
}

#[derive(Debug, Default, Clone)]
pub struct Asm {
    code: Vec<u16>,
    labels: HashMap<String, usize>,
    fixups: Vec<(usize, String, Fix)>,
}

/// I/O addresses (for IN/OUT/SBI/CBI) of common registers.
pub mod io {
    pub const PINB: u8 = 0x03;
    pub const DDRB: u8 = 0x04;
    pub const PORTB: u8 = 0x05;
    pub const PINC: u8 = 0x06;
    pub const DDRC: u8 = 0x07;
    pub const PORTC: u8 = 0x08;
    pub const PIND: u8 = 0x09;
    pub const DDRD: u8 = 0x0A;
    pub const PORTD: u8 = 0x0B;
    pub const TIFR0: u8 = 0x15;
    pub const TIFR1: u8 = 0x16;
    pub const TIFR2: u8 = 0x17;
    pub const PCIFR: u8 = 0x1B;
    pub const EIFR: u8 = 0x1C;
    pub const EIMSK: u8 = 0x1D;
    pub const GPIOR0: u8 = 0x1E;
    pub const TCCR0A: u8 = 0x24;
    pub const TCCR0B: u8 = 0x25;
    pub const TCNT0: u8 = 0x26;
    pub const OCR0A: u8 = 0x27;
    pub const OCR0B: u8 = 0x28;
    pub const SMCR: u8 = 0x33;
    pub const MCUCR: u8 = 0x35;
    pub const SPL: u8 = 0x3D;
    pub const SPH: u8 = 0x3E;
    pub const SREG: u8 = 0x3F;
}

/// Data-space addresses (for LDS/STS) of extended I/O registers.
pub mod mem {
    pub const PCICR: u16 = 0x68;
    pub const EICRA: u16 = 0x69;
    pub const PCMSK0: u16 = 0x6B;
    pub const PCMSK1: u16 = 0x6C;
    pub const PCMSK2: u16 = 0x6D;
    pub const TIMSK0: u16 = 0x6E;
    pub const TIMSK1: u16 = 0x6F;
    pub const TIMSK2: u16 = 0x70;
    pub const ADCL: u16 = 0x78;
    pub const ADCH: u16 = 0x79;
    pub const ADCSRA: u16 = 0x7A;
    pub const ADCSRB: u16 = 0x7B;
    pub const ADMUX: u16 = 0x7C;
    pub const TCCR1A: u16 = 0x80;
    pub const TCCR1B: u16 = 0x81;
    pub const TCNT1L: u16 = 0x84;
    pub const TCNT1H: u16 = 0x85;
    pub const ICR1L: u16 = 0x86;
    pub const ICR1H: u16 = 0x87;
    pub const OCR1AL: u16 = 0x88;
    pub const OCR1AH: u16 = 0x89;
    pub const OCR1BL: u16 = 0x8A;
    pub const OCR1BH: u16 = 0x8B;
    pub const TCCR2A: u16 = 0xB0;
    pub const TCCR2B: u16 = 0xB1;
    pub const TCNT2: u16 = 0xB2;
    pub const OCR2A: u16 = 0xB3;
    pub const OCR2B: u16 = 0xB4;
    pub const UCSR0A: u16 = 0xC0;
    pub const UCSR0B: u16 = 0xC1;
    pub const UCSR0C: u16 = 0xC2;
    pub const UBRR0L: u16 = 0xC4;
    pub const UBRR0H: u16 = 0xC5;
    pub const UDR0: u16 = 0xC6;
    pub const RAMEND: u16 = 0x08FF;
}

fn rd_rr(base: u16, d: u8, r: u8) -> u16 {
    assert!(d < 32 && r < 32, "register out of range");
    base | ((r as u16 & 0x10) << 5) | ((d as u16) << 4) | (r as u16 & 0xF)
}

fn rd_k(base: u16, d: u8, k: u8) -> u16 {
    assert!((16..32).contains(&d), "immediate instructions need r16–r31");
    base | ((k as u16 & 0xF0) << 4) | (((d - 16) as u16) << 4) | (k as u16 & 0xF)
}

macro_rules! two_reg {
    ($($name:ident = $base:expr),* $(,)?) => {
        $(pub fn $name(&mut self, d: u8, r: u8) -> &mut Self { self.emit(rd_rr($base, d, r)) })*
    };
}

macro_rules! imm {
    ($($name:ident = $base:expr),* $(,)?) => {
        $(pub fn $name(&mut self, d: u8, k: u8) -> &mut Self { self.emit(rd_k($base, d, k)) })*
    };
}

macro_rules! one_reg {
    ($($name:ident = $base:expr),* $(,)?) => {
        $(pub fn $name(&mut self, d: u8) -> &mut Self {
            assert!(d < 32);
            self.emit($base | (d as u16) << 4)
        })*
    };
}

macro_rules! fixed {
    ($($name:ident = $op:expr),* $(,)?) => {
        $(pub fn $name(&mut self) -> &mut Self { self.emit($op) })*
    };
}

macro_rules! branch {
    ($($name:ident = ($base:expr, $bit:expr)),* $(,)?) => {
        $(pub fn $name(&mut self, label: &str) -> &mut Self {
            self.fix(label, Fix::Rel7);
            self.emit($base | $bit)
        })*
    };
}

impl Asm {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current position (word address).
    pub fn here(&self) -> usize {
        self.code.len()
    }

    /// Word address of a label defined so far.
    pub fn label_address(&self, name: &str) -> Option<usize> {
        self.labels.get(name).copied()
    }

    fn emit(&mut self, w: u16) -> &mut Self {
        self.code.push(w);
        self
    }

    fn fix(&mut self, label: &str, kind: Fix) {
        self.fixups.push((self.code.len(), label.to_string(), kind));
    }

    pub fn label(&mut self, name: &str) -> &mut Self {
        let prev = self.labels.insert(name.to_string(), self.code.len());
        assert!(prev.is_none(), "label '{name}' defined twice");
        self
    }

    /// Pad with erased flash up to word address `addr`.
    pub fn org(&mut self, addr: usize) -> &mut Self {
        assert!(addr >= self.code.len(), "org moves backwards");
        self.code.resize(addr, 0xFFFF);
        self
    }

    /// A raw instruction word.
    pub fn word(&mut self, w: u16) -> &mut Self {
        self.emit(w)
    }

    /// Bytes in flash (padded to a whole word), e.g. strings for LPM.
    pub fn bytes(&mut self, data: &[u8]) -> &mut Self {
        for pair in data.chunks(2) {
            let w = pair[0] as u16 | (*pair.get(1).unwrap_or(&0) as u16) << 8;
            self.code.push(w);
        }
        self
    }

    /// The interrupt vector table: a JMP per vector (26 on the
    /// ATmega328P) to the named handler, or to `default`.
    pub fn vectors(&mut self, handlers: &[(usize, &str)], default: &str) -> &mut Self {
        for v in 0..26 {
            let target = handlers
                .iter()
                .find(|(n, _)| *n == v)
                .map_or(default, |(_, l)| *l);
            self.jmp(target);
        }
        self
    }

    two_reg! {
        cpc = 0x0400, sbc = 0x0800, add = 0x0C00, cpse = 0x1000, cp = 0x1400,
        sub = 0x1800, adc = 0x1C00, and = 0x2000, eor = 0x2400, or = 0x2800,
        mov = 0x2C00, mul = 0x9C00,
    }

    imm! { cpi = 0x3000, sbci = 0x4000, subi = 0x5000, ori = 0x6000, andi = 0x7000, ldi = 0xE000 }

    one_reg! {
        com = 0x9400, neg = 0x9401, swap = 0x9402, inc = 0x9403, asr = 0x9405,
        lsr = 0x9406, ror = 0x9407, dec = 0x940A, pop = 0x900F, push = 0x920F,
        ld_x = 0x900C, ld_xp = 0x900D, ld_mx = 0x900E, ld_yp = 0x9009, ld_my = 0x900A,
        ld_zp = 0x9001, ld_mz = 0x9002, st_x = 0x920C, st_xp = 0x920D, st_mx = 0x920E,
        st_yp = 0x9209, st_my = 0x920A, st_zp = 0x9201, st_mz = 0x9202,
        lpm_z = 0x9004, lpm_zp = 0x9005,
    }

    fixed! {
        nop = 0x0000, sei = 0x9478, cli = 0x94F8, sec = 0x9408, clc = 0x9488,
        set = 0x9468, clt = 0x94E8, ret = 0x9508, reti = 0x9518, sleep = 0x9588,
        brk = 0x9598, wdr = 0x95A8, lpm = 0x95C8, ijmp = 0x9409, icall = 0x9509,
    }

    branch! {
        brcs = (0xF000, 0), brlo = (0xF000, 0), breq = (0xF000, 1), brmi = (0xF000, 2),
        brvs = (0xF000, 3), brlt = (0xF000, 4), brhs = (0xF000, 5), brts = (0xF000, 6),
        brie = (0xF000, 7), brcc = (0xF400, 0), brsh = (0xF400, 0), brne = (0xF400, 1),
        brpl = (0xF400, 2), brvc = (0xF400, 3), brge = (0xF400, 4), brhc = (0xF400, 5),
        brtc = (0xF400, 6), brid = (0xF400, 7),
    }

    pub fn clr(&mut self, d: u8) -> &mut Self {
        self.eor(d, d)
    }
    pub fn tst(&mut self, d: u8) -> &mut Self {
        self.and(d, d)
    }
    pub fn lsl(&mut self, d: u8) -> &mut Self {
        self.add(d, d)
    }
    pub fn rol(&mut self, d: u8) -> &mut Self {
        self.adc(d, d)
    }
    pub fn ser(&mut self, d: u8) -> &mut Self {
        self.ldi(d, 0xFF)
    }

    pub fn movw(&mut self, d: u8, r: u8) -> &mut Self {
        assert!(d.is_multiple_of(2) && r.is_multiple_of(2));
        self.emit(0x0100 | ((d as u16 / 2) << 4) | (r as u16 / 2))
    }

    pub fn muls(&mut self, d: u8, r: u8) -> &mut Self {
        assert!((16..32).contains(&d) && (16..32).contains(&r));
        self.emit(0x0200 | (((d - 16) as u16) << 4) | (r - 16) as u16)
    }

    fn mul_low(&mut self, base: u16, d: u8, r: u8) -> &mut Self {
        assert!((16..24).contains(&d) && (16..24).contains(&r));
        self.emit(base | (((d - 16) as u16) << 4) | (r - 16) as u16)
    }
    pub fn mulsu(&mut self, d: u8, r: u8) -> &mut Self {
        self.mul_low(0x0300, d, r)
    }
    pub fn fmul(&mut self, d: u8, r: u8) -> &mut Self {
        self.mul_low(0x0308, d, r)
    }
    pub fn fmuls(&mut self, d: u8, r: u8) -> &mut Self {
        self.mul_low(0x0380, d, r)
    }
    pub fn fmulsu(&mut self, d: u8, r: u8) -> &mut Self {
        self.mul_low(0x0388, d, r)
    }

    fn word_imm(&mut self, base: u16, d: u8, k: u8) -> &mut Self {
        assert!(matches!(d, 24 | 26 | 28 | 30) && k < 64);
        self.emit(base | ((k as u16 & 0x30) << 2) | (((d - 24) as u16 / 2) << 4) | (k as u16 & 0xF))
    }
    pub fn adiw(&mut self, d: u8, k: u8) -> &mut Self {
        self.word_imm(0x9600, d, k)
    }
    pub fn sbiw(&mut self, d: u8, k: u8) -> &mut Self {
        self.word_imm(0x9700, d, k)
    }

    fn io_bit(&mut self, base: u16, a: u8, b: u8) -> &mut Self {
        assert!(a < 32 && b < 8);
        self.emit(base | (a as u16) << 3 | b as u16)
    }
    pub fn cbi(&mut self, a: u8, b: u8) -> &mut Self {
        self.io_bit(0x9800, a, b)
    }
    pub fn sbic(&mut self, a: u8, b: u8) -> &mut Self {
        self.io_bit(0x9900, a, b)
    }
    pub fn sbi(&mut self, a: u8, b: u8) -> &mut Self {
        self.io_bit(0x9A00, a, b)
    }
    pub fn sbis(&mut self, a: u8, b: u8) -> &mut Self {
        self.io_bit(0x9B00, a, b)
    }

    fn reg_bit(&mut self, base: u16, d: u8, b: u8) -> &mut Self {
        assert!(d < 32 && b < 8);
        self.emit(base | (d as u16) << 4 | b as u16)
    }
    pub fn bld(&mut self, d: u8, b: u8) -> &mut Self {
        self.reg_bit(0xF800, d, b)
    }
    pub fn bst(&mut self, d: u8, b: u8) -> &mut Self {
        self.reg_bit(0xFA00, d, b)
    }
    pub fn sbrc(&mut self, d: u8, b: u8) -> &mut Self {
        self.reg_bit(0xFC00, d, b)
    }
    pub fn sbrs(&mut self, d: u8, b: u8) -> &mut Self {
        self.reg_bit(0xFE00, d, b)
    }

    pub fn in_(&mut self, d: u8, a: u8) -> &mut Self {
        assert!(d < 32 && a < 64);
        self.emit(0xB000 | ((a as u16 & 0x30) << 5) | (d as u16) << 4 | (a as u16 & 0xF))
    }
    pub fn out(&mut self, a: u8, r: u8) -> &mut Self {
        assert!(r < 32 && a < 64);
        self.emit(0xB800 | ((a as u16 & 0x30) << 5) | (r as u16) << 4 | (a as u16 & 0xF))
    }

    pub fn lds(&mut self, d: u8, addr: u16) -> &mut Self {
        self.emit(0x9000 | (d as u16) << 4).emit(addr)
    }
    pub fn sts(&mut self, addr: u16, r: u8) -> &mut Self {
        self.emit(0x9200 | (r as u16) << 4).emit(addr)
    }

    fn disp(&mut self, y: bool, store: bool, d: u8, q: u8) -> &mut Self {
        assert!(d < 32 && q < 64);
        let q = q as u16;
        self.emit(
            0x8000
                | ((q & 0x20) << 8)
                | ((q & 0x18) << 7)
                | (q & 7)
                | (d as u16) << 4
                | if y { 8 } else { 0 }
                | if store { 0x200 } else { 0 },
        )
    }
    pub fn ldd_y(&mut self, d: u8, q: u8) -> &mut Self {
        self.disp(true, false, d, q)
    }
    pub fn ldd_z(&mut self, d: u8, q: u8) -> &mut Self {
        self.disp(false, false, d, q)
    }
    pub fn std_y(&mut self, q: u8, r: u8) -> &mut Self {
        self.disp(true, true, r, q)
    }
    pub fn std_z(&mut self, q: u8, r: u8) -> &mut Self {
        self.disp(false, true, r, q)
    }

    pub fn rjmp(&mut self, label: &str) -> &mut Self {
        self.fix(label, Fix::Rel12);
        self.emit(0xC000)
    }
    pub fn rcall(&mut self, label: &str) -> &mut Self {
        self.fix(label, Fix::Rel12);
        self.emit(0xD000)
    }
    pub fn jmp(&mut self, label: &str) -> &mut Self {
        self.fix(label, Fix::Abs22);
        self.emit(0x940C).emit(0)
    }
    pub fn call(&mut self, label: &str) -> &mut Self {
        self.fix(label, Fix::Abs22);
        self.emit(0x940E).emit(0)
    }

    /// LDI `d` with the low byte of `label`'s byte address.
    pub fn ldi_lo(&mut self, d: u8, label: &str) -> &mut Self {
        self.fix(label, Fix::ByteLo);
        self.ldi(d, 0)
    }
    /// LDI `d` with the high byte of `label`'s byte address.
    pub fn ldi_hi(&mut self, d: u8, label: &str) -> &mut Self {
        self.fix(label, Fix::ByteHi);
        self.ldi(d, 0)
    }

    /// Set up the stack pointer at the top of SRAM (uses r16).
    pub fn init_stack(&mut self) -> &mut Self {
        self.ldi(16, (mem::RAMEND >> 8) as u8)
            .out(io::SPH, 16)
            .ldi(16, mem::RAMEND as u8)
            .out(io::SPL, 16)
    }

    /// Resolve labels and return the flash image (little-endian words).
    pub fn finish(&self) -> Result<Vec<u8>, String> {
        let mut code = self.code.clone();
        for (at, label, kind) in &self.fixups {
            let target = *self
                .labels
                .get(label)
                .ok_or_else(|| format!("undefined label '{label}'"))?;
            let rel = target as isize - (*at as isize + 1);
            match kind {
                Fix::Rel12 => {
                    if !(-2048..2048).contains(&rel) {
                        return Err(format!("'{label}' is out of RJMP/RCALL range"));
                    }
                    code[*at] |= (rel as u16) & 0xFFF;
                }
                Fix::Rel7 => {
                    if !(-64..64).contains(&rel) {
                        return Err(format!("'{label}' is out of branch range"));
                    }
                    code[*at] |= ((rel as u16) & 0x7F) << 3;
                }
                Fix::Abs22 => {
                    let k = target as u32;
                    code[*at] |= (((k >> 17) & 0x1F) << 4 | ((k >> 16) & 1)) as u16;
                    code[*at + 1] = k as u16;
                }
                Fix::ByteLo | Fix::ByteHi => {
                    let addr = target * 2;
                    let k = if matches!(kind, Fix::ByteLo) {
                        addr
                    } else {
                        addr >> 8
                    } as u16;
                    code[*at] |= ((k & 0xF0) << 4) | (k & 0xF);
                }
            }
        }
        Ok(code.iter().flat_map(|w| w.to_le_bytes()).collect())
    }
}
