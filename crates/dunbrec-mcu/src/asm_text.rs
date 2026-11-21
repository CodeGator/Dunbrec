//! A text assembler for the ATmega328P, in the Atmel dialect that tutorials,
//! the datasheet and the Arduino world use (`ldi r16, 0xFF`, `sbi PORTB, 5`,
//! `.equ`, `.def`, `.db`, labels with a colon).
//!
//! It reads the same instruction table ([`crate::isa`]) as the
//! disassembler, so the two cannot drift apart: a test disassembles every
//! instruction word the chip has and assembles the text back to the same
//! word.
//!
//! ```
//! use dunbrec_mcu::asm_text::assemble;
//! let program = assemble("
//!         sbi  DDRB, PB5        ; D13 is an output
//! loop:   sbi  PINB, PB5        ; writing a 1 to PIN toggles the pin
//!         rjmp loop
//! ").unwrap();
//! assert_eq!(program.image.len(), 6);
//! ```
//!
//! # What it accepts
//!
//! * **Comments** start with `;` or `//`.
//! * **Labels** end with a colon and may share a line with an instruction.
//!   They are word addresses. Names are case-insensitive, as in Atmel's
//!   assembler.
//! * **Directives:** `.org` (a word address), `.equ NAME = expr`,
//!   `.def NAME = rN`, `.db` and `.dw` (numbers and `"strings"`; `.db`
//!   pads to a whole word), and `.cseg`, `.device`, `.list` and `.nolist`,
//!   which are accepted and ignored. Macros, `.include`, `.dseg` and
//!   `.eseg` are not supported and say so.
//! * **Expressions** use `+ - * / % << >> & | ^ ~`, parentheses, numbers
//!   (`42`, `0x2A`, `$2A`, `0b101010`, `'*'`), names, `PC`, and the
//!   functions `low`, `high`, `byte1` to `byte3`, `lo8`, `hi8` and `abs`.
//! * **Chip names** from Atmel's `m328Pdef.inc`: registers (`DDRB`, `UCSR0A`
//!   ...), bit numbers (`PB5`, `CS01`, `TOIE0`, `ADEN` ...), vector
//!   addresses (`OVF0addr`) and `RAMEND`. A register in I/O space is its I/O
//!   address in `in`, `out`, `sbi`, `cbi`, `sbic` and `sbis`, and its data
//!   address in `lds` and `sts`, so each instruction gets the number it
//!   wants.
//! * **Jumps and branches** take a word address (a label, or `PC+n`), as in
//!   Atmel's assembler.
//! * **Aliases:** `clr`, `tst`, `lsl`, `rol`, `ser`, `sbr`, `cbr`, `sec` and
//!   the other SREG flag names, and `breq`, `brne`, `brlo`, `brsh` and the
//!   rest of the branch names.
//!
//! Errors carry the line and column, and every error in the source is
//! reported, not only the first.

use std::collections::HashMap;

use crate::isa::{
    BRANCH_CLEAR_NAMES, BRANCH_SET_NAMES, CLEAR_NAMES, Form, OPS, Op, REGISTERS, SET_NAMES,
};

/// Words of flash on the ATmega328P.
pub const FLASH_WORDS: usize = 16 * 1024;

/// A problem in the source, with its place (both counted from 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsmError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl std::fmt::Display for AsmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for AsmError {}

/// A successful assembly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assembled {
    /// The flash image, little-endian words. Gaps left by `.org` are erased
    /// flash (0xFF).
    pub image: Vec<u8>,
    /// `(word address, source line)` for every statement that put words in
    /// flash, in address order. Lines count from 1.
    pub lines: Vec<(usize, usize)>,
    /// `(name as written, word address)` of every label, in address order.
    pub labels: Vec<(String, usize)>,
}

impl Assembled {
    /// The source line of the statement at word address `address`.
    pub fn line_for_address(&self, address: usize) -> Option<usize> {
        self.lines
            .binary_search_by_key(&address, |(a, _)| *a)
            .ok()
            .map(|i| self.lines[i].1)
    }

    /// The word address of source line `line`, or of the next line after it
    /// that has code (so a breakpoint on a blank or comment line lands on
    /// the next instruction).
    pub fn address_for_line(&self, line: usize) -> Option<usize> {
        self.lines
            .iter()
            .filter(|(_, l)| *l >= line)
            .min_by_key(|(a, l)| (*l, *a))
            .map(|(a, _)| *a)
    }

    /// Word address of a label (case-insensitive).
    pub fn label_address(&self, name: &str) -> Option<usize> {
        self.labels
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, a)| *a)
    }
}

/// Assemble `source`. On failure, every error found, sorted by place.
pub fn assemble(source: &str) -> Result<Assembled, Vec<AsmError>> {
    let mut asm = Assembler::default();
    asm.pass_one(source);
    asm.pass_two();
    asm.finish(source)
}

// ---------------------------------------------------------------------------
// Symbols and chip names

#[derive(Debug, Clone, Copy)]
enum Sym {
    Const(i64),
    Reg(u8),
}

/// Bit numbers from `m328Pdef.inc` (the ones tutorials use).
static BITS: &[(&str, i64)] = &[
    ("SREG_C", 0),
    ("SREG_Z", 1),
    ("SREG_N", 2),
    ("SREG_V", 3),
    ("SREG_S", 4),
    ("SREG_H", 5),
    ("SREG_T", 6),
    ("SREG_I", 7),
    ("WGM00", 0),
    ("WGM01", 1),
    ("COM0B0", 4),
    ("COM0B1", 5),
    ("COM0A0", 6),
    ("COM0A1", 7),
    ("CS00", 0),
    ("CS01", 1),
    ("CS02", 2),
    ("WGM02", 3),
    ("FOC0B", 6),
    ("FOC0A", 7),
    ("TOIE0", 0),
    ("OCIE0A", 1),
    ("OCIE0B", 2),
    ("TOV0", 0),
    ("OCF0A", 1),
    ("OCF0B", 2),
    ("WGM10", 0),
    ("WGM11", 1),
    ("COM1B0", 4),
    ("COM1B1", 5),
    ("COM1A0", 6),
    ("COM1A1", 7),
    ("CS10", 0),
    ("CS11", 1),
    ("CS12", 2),
    ("WGM12", 3),
    ("WGM13", 4),
    ("ICES1", 6),
    ("ICNC1", 7),
    ("TOIE1", 0),
    ("OCIE1A", 1),
    ("OCIE1B", 2),
    ("ICIE1", 5),
    ("TOV1", 0),
    ("OCF1A", 1),
    ("OCF1B", 2),
    ("ICF1", 5),
    ("WGM20", 0),
    ("WGM21", 1),
    ("COM2B0", 4),
    ("COM2B1", 5),
    ("COM2A0", 6),
    ("COM2A1", 7),
    ("CS20", 0),
    ("CS21", 1),
    ("CS22", 2),
    ("WGM22", 3),
    ("FOC2B", 6),
    ("FOC2A", 7),
    ("TOIE2", 0),
    ("OCIE2A", 1),
    ("OCIE2B", 2),
    ("TOV2", 0),
    ("OCF2A", 1),
    ("OCF2B", 2),
    ("ADPS0", 0),
    ("ADPS1", 1),
    ("ADPS2", 2),
    ("ADIE", 3),
    ("ADIF", 4),
    ("ADATE", 5),
    ("ADSC", 6),
    ("ADEN", 7),
    ("MUX0", 0),
    ("MUX1", 1),
    ("MUX2", 2),
    ("MUX3", 3),
    ("ADLAR", 5),
    ("REFS0", 6),
    ("REFS1", 7),
    ("ADTS0", 0),
    ("ADTS1", 1),
    ("ADTS2", 2),
    ("ACME", 6),
    ("MPCM0", 0),
    ("U2X0", 1),
    ("UPE0", 2),
    ("DOR0", 3),
    ("FE0", 4),
    ("UDRE0", 5),
    ("TXC0", 6),
    ("RXC0", 7),
    ("TXB80", 0),
    ("RXB80", 1),
    ("UCSZ02", 2),
    ("TXEN0", 3),
    ("RXEN0", 4),
    ("UDRIE0", 5),
    ("TXCIE0", 6),
    ("RXCIE0", 7),
    ("UCPOL0", 0),
    ("UCSZ00", 1),
    ("UCSZ01", 2),
    ("USBS0", 3),
    ("UPM00", 4),
    ("UPM01", 5),
    ("UMSEL00", 6),
    ("UMSEL01", 7),
    ("SE", 0),
    ("SM0", 1),
    ("SM1", 2),
    ("SM2", 3),
    ("INT0", 0),
    ("INT1", 1),
    ("INTF0", 0),
    ("INTF1", 1),
    ("ISC00", 0),
    ("ISC01", 1),
    ("ISC10", 2),
    ("ISC11", 3),
    ("PCIE0", 0),
    ("PCIE1", 1),
    ("PCIE2", 2),
    ("PCIF0", 0),
    ("PCIF1", 1),
    ("PCIF2", 2),
];

/// Interrupt vector addresses (word addresses), `<name>addr` as in
/// `m328Pdef.inc`.
static VECTORS: &[(&str, i64)] = &[
    ("INT0ADDR", 0x02),
    ("INT1ADDR", 0x04),
    ("PCI0ADDR", 0x06),
    ("PCI1ADDR", 0x08),
    ("PCI2ADDR", 0x0A),
    ("WDTADDR", 0x0C),
    ("OC2AADDR", 0x0E),
    ("OC2BADDR", 0x10),
    ("OVF2ADDR", 0x12),
    ("ICP1ADDR", 0x14),
    ("OC1AADDR", 0x16),
    ("OC1BADDR", 0x18),
    ("OVF1ADDR", 0x1A),
    ("OC0AADDR", 0x1C),
    ("OC0BADDR", 0x1E),
    ("OVF0ADDR", 0x20),
    ("SPIADDR", 0x22),
    ("URXCADDR", 0x24),
    ("UDREADDR", 0x26),
    ("UTXCADDR", 0x28),
    ("ADCCADDR", 0x2A),
    ("ERDYADDR", 0x2C),
    ("ACIADDR", 0x2E),
    ("TWIADDR", 0x30),
    ("SPMRADDR", 0x32),
];

/// `PB5`, `DDD3`, `PORTC0`, `PINB2`: the bit number of a port pin.
fn port_bit(name: &str) -> Option<i64> {
    let rest = ["PORT", "PIN", "DD", "P"]
        .iter()
        .find_map(|p| name.strip_prefix(p))?;
    let mut chars = rest.chars();
    let port = chars.next()?;
    let digit = chars.next()?;
    if chars.next().is_some() || !matches!(port, 'B' | 'C' | 'D') || !digit.is_ascii_digit() {
        return None;
    }
    let bit = digit.to_digit(10)? as i64;
    (bit < 8).then_some(bit)
}

/// A chip-defined name (already upper-cased). `data` says whether the
/// caller wants data-space addresses (LDS, STS) or I/O addresses.
fn builtin(name: &str, data: bool) -> Option<i64> {
    if let Some((addr, _)) = REGISTERS.iter().find(|(_, n)| *n == name) {
        let addr = *addr as i64;
        return Some(if !data && (0x20..=0x5F).contains(&addr) {
            addr - 0x20
        } else {
            addr
        });
    }
    if let Some((_, v)) = BITS.iter().find(|(n, _)| *n == name) {
        return Some(*v);
    }
    if let Some((_, v)) = VECTORS.iter().find(|(n, _)| *n == name) {
        return Some(*v);
    }
    if let Some(v) = port_bit(name) {
        return Some(v);
    }
    match name {
        "RAMEND" => Some(0x08FF),
        "SRAM_START" => Some(0x0100),
        "SRAM_SIZE" => Some(2048),
        "FLASHEND" => Some(FLASH_WORDS as i64 - 1),
        _ => None,
    }
}

/// Is `name` an instruction (or alias) the assembler knows? For editors that
/// colour source text.
pub fn is_mnemonic(name: &str) -> bool {
    instruction_words(&name.to_ascii_lowercase()).is_some()
}

/// Is `name` a name the chip defines (a register, a bit, a vector address)?
pub fn is_chip_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper == "PC" || builtin(&upper, false).is_some()
}

/// Is `name` a register: `r0` to `r31` or a pointer half (`XL`, `ZH`...)?
pub fn is_register(name: &str) -> bool {
    register(name, &HashMap::new()).is_ok()
}

/// `r0` to `r31`, the pointer halves, or a `.def` alias.
fn register(text: &str, user: &HashMap<String, Sym>) -> Result<u8, String> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    if let Some(num) = lower.strip_prefix('r')
        && !num.is_empty()
        && num.bytes().all(|b| b.is_ascii_digit())
    {
        return match num.parse::<u8>() {
            Ok(n) if n < 32 => Ok(n),
            _ => Err(format!("there is no register {t}: use r0 to r31")),
        };
    }
    match lower.as_str() {
        "xl" => return Ok(26),
        "xh" => return Ok(27),
        "yl" => return Ok(28),
        "yh" => return Ok(29),
        "zl" => return Ok(30),
        "zh" => return Ok(31),
        _ => {}
    }
    if let Some(Sym::Reg(n)) = user.get(&t.to_ascii_uppercase()) {
        return Ok(*n);
    }
    if t.is_empty() {
        return Err("a register is missing here".into());
    }
    Err(format!("expected a register such as r16, not '{t}'"))
}

// ---------------------------------------------------------------------------
// Expressions

struct Ctx<'a> {
    user: &'a HashMap<String, Sym>,
    /// Word address of the statement being assembled (the value of `PC`).
    pc: i64,
    /// True when a number is a data-space address (LDS, STS).
    data: bool,
}

/// A problem inside an expression: the byte offset in the expression text.
struct ExprErr {
    at: usize,
    msg: String,
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    ctx: &'a Ctx<'a>,
}

impl<'a> Parser<'a> {
    fn err<T>(&self, at: usize, msg: impl Into<String>) -> Result<T, ExprErr> {
        Err(ExprErr {
            at,
            msg: msg.into(),
        })
    }

    fn skip(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.s.get(self.i).copied()
    }

    fn peek2(&mut self) -> Option<(u8, Option<u8>)> {
        let a = self.peek()?;
        Some((a, self.s.get(self.i + 1).copied()))
    }

    fn binary(&mut self, level: usize) -> Result<i64, ExprErr> {
        if level == 6 {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        loop {
            let Some((c, next)) = self.peek2() else {
                return Ok(left);
            };
            let (len, op) = match (level, c, next) {
                (0, b'|', _) => (1, '|'),
                (1, b'^', _) => (1, '^'),
                (2, b'&', _) => (1, '&'),
                (3, b'<', Some(b'<')) => (2, '<'),
                (3, b'>', Some(b'>')) => (2, '>'),
                (4, b'+', _) => (1, '+'),
                (4, b'-', _) => (1, '-'),
                (5, b'*', _) => (1, '*'),
                (5, b'/', _) => (1, '/'),
                (5, b'%', _) => (1, '%'),
                _ => return Ok(left),
            };
            let at = self.i;
            self.i += len;
            let right = self.binary(level + 1)?;
            left = match op {
                '|' => left | right,
                '^' => left ^ right,
                '&' => left & right,
                '<' | '>' => {
                    if !(0..64).contains(&right) {
                        return self.err(at, format!("cannot shift by {right}"));
                    }
                    if op == '<' {
                        left.wrapping_shl(right as u32)
                    } else {
                        left >> right
                    }
                }
                '+' => left.wrapping_add(right),
                '-' => left.wrapping_sub(right),
                '*' => left.wrapping_mul(right),
                _ => {
                    if right == 0 {
                        return self.err(at, "division by zero");
                    }
                    if op == '/' {
                        left.wrapping_div(right)
                    } else {
                        left.wrapping_rem(right)
                    }
                }
            };
        }
    }

    fn unary(&mut self) -> Result<i64, ExprErr> {
        match self.peek() {
            Some(b'-') => {
                self.i += 1;
                Ok(self.unary()?.wrapping_neg())
            }
            Some(b'+') => {
                self.i += 1;
                self.unary()
            }
            Some(b'~') => {
                self.i += 1;
                Ok(!self.unary()?)
            }
            Some(b'(') => {
                self.i += 1;
                let v = self.binary(0)?;
                if self.peek() != Some(b')') {
                    return self.err(self.i, "a closing parenthesis ')' is missing");
                }
                self.i += 1;
                Ok(v)
            }
            Some(b'\'') => self.character(),
            Some(c) if c.is_ascii_digit() || c == b'$' => self.number(),
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => self.name(),
            Some(c) => self.err(self.i, format!("unexpected '{}'", c as char)),
            None => self.err(self.i, "a value is missing here"),
        }
    }

    fn character(&mut self) -> Result<i64, ExprErr> {
        let at = self.i;
        let rest = std::str::from_utf8(&self.s[self.i + 1..]).unwrap_or("");
        let mut chars = rest.chars();
        let c = match chars.next() {
            Some('\\') => match chars.next() {
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('0') => '\0',
                Some(c @ ('\\' | '\'' | '"')) => c,
                _ => return self.err(at, "unknown escape in a character"),
            },
            Some(c) if c != '\'' => c,
            _ => return self.err(at, "a character is missing between the quotes"),
        };
        let used = rest.len() - chars.as_str().len();
        if chars.next() != Some('\'') {
            return self.err(at, "a character needs a closing quote: 'A'");
        }
        self.i += 1 + used + 1;
        Ok(c as i64)
    }

    fn number(&mut self) -> Result<i64, ExprErr> {
        let at = self.i;
        let (radix, mut j) = if self.s[self.i] == b'$' {
            (16, self.i + 1)
        } else if self.s[self.i] == b'0' && matches!(self.s.get(self.i + 1), Some(b'x' | b'X')) {
            (16, self.i + 2)
        } else if self.s[self.i] == b'0' && matches!(self.s.get(self.i + 1), Some(b'b' | b'B')) {
            (2, self.i + 2)
        } else {
            (10, self.i)
        };
        let start = j;
        while j < self.s.len() && self.s[j].is_ascii_alphanumeric() {
            j += 1;
        }
        let digits = std::str::from_utf8(&self.s[start..j]).unwrap_or("");
        match i64::from_str_radix(digits, radix) {
            Ok(v) if !digits.is_empty() => {
                self.i = j;
                Ok(v)
            }
            _ => {
                let text = std::str::from_utf8(&self.s[at..j]).unwrap_or("");
                self.err(at, format!("'{text}' is not a valid number"))
            }
        }
    }

    fn name(&mut self) -> Result<i64, ExprErr> {
        let at = self.i;
        let mut j = at;
        while j < self.s.len() && (self.s[j].is_ascii_alphanumeric() || self.s[j] == b'_') {
            j += 1;
        }
        let name = std::str::from_utf8(&self.s[at..j]).unwrap_or("");
        self.i = j;
        let upper = name.to_ascii_uppercase();
        if self.peek() == Some(b'(') {
            return self.function(&upper, at);
        }
        if let Some(sym) = self.ctx.user.get(&upper) {
            return match sym {
                Sym::Const(v) => Ok(*v),
                Sym::Reg(_) => self.err(at, format!("'{name}' is a register, not a number")),
            };
        }
        if upper == "PC" {
            return Ok(self.ctx.pc);
        }
        if let Some(v) = builtin(&upper, self.ctx.data) {
            return Ok(v);
        }
        self.err(at, format!("'{name}' is not defined"))
    }

    fn function(&mut self, name: &str, at: usize) -> Result<i64, ExprErr> {
        self.i += 1; // the '('
        let v = self.binary(0)?;
        if self.peek() != Some(b')') {
            return self.err(self.i, "a closing parenthesis ')' is missing");
        }
        self.i += 1;
        Ok(match name {
            "LOW" | "LO8" | "BYTE1" => v & 0xFF,
            "HIGH" | "HI8" | "BYTE2" => (v >> 8) & 0xFF,
            "BYTE3" => (v >> 16) & 0xFF,
            "ABS" => v.abs(),
            _ => {
                return self.err(
                    at,
                    format!(
                        "there is no function '{}' (try low, high, lo8, hi8, byte1 to byte3, abs)",
                        name.to_ascii_lowercase()
                    ),
                );
            }
        })
    }
}

fn evaluate(text: &str, ctx: &Ctx) -> Result<i64, ExprErr> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        ctx,
    };
    let v = p.binary(0)?;
    if p.peek().is_some() {
        let rest = &text[p.i..];
        return p.err(p.i, format!("did not expect '{}' here", rest.trim_end()));
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// Source lines

/// A problem placed by byte offset within its source line.
struct E {
    at: usize,
    msg: String,
}

impl E {
    fn new(at: usize, msg: impl Into<String>) -> Self {
        E {
            at,
            msg: msg.into(),
        }
    }
}

#[derive(Debug, Clone)]
struct Operand {
    text: String,
    /// Byte offset of the text in its line.
    at: usize,
}

#[derive(Debug, Clone)]
enum Kind {
    Instr { name: String, ops: Vec<Operand> },
    Bytes(Vec<Operand>),
    Words(Vec<Operand>),
}

#[derive(Debug, Clone)]
struct Stmt {
    /// Index into the source lines (0-based).
    line: usize,
    /// Byte offset of the mnemonic or directive.
    at: usize,
    /// Word address.
    addr: usize,
    kind: Kind,
}

/// Cut a trailing comment off (`;` or `//`), leaving quoted text alone.
fn strip_comment(line: &str) -> &str {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b';' => return &line[..i],
            b'/' if b.get(i + 1) == Some(&b'/') => return &line[..i],
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                // 'x' or '\x'; a lone quote is left for the parser to report.
                let close = if b.get(i + 1) == Some(&b'\\') {
                    i + 3
                } else {
                    i + 2
                };
                if b.get(close) == Some(&b'\'') {
                    i = close;
                }
            }
            _ => {}
        }
        i += 1;
    }
    line
}

/// Split `text` (starting at byte `base` of its line) at top-level commas.
fn split_operands(text: &str, base: usize) -> Vec<Operand> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let b = text.as_bytes();
    let mut i = 0;
    let push = |from: usize, to: usize, out: &mut Vec<Operand>| {
        let piece = &text[from..to];
        let lead = piece.len() - piece.trim_start().len();
        out.push(Operand {
            text: piece.trim().to_string(),
            at: base + from + lead,
        });
    };
    while i < b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                let close = if b.get(i + 1) == Some(&b'\\') {
                    i + 3
                } else {
                    i + 2
                };
                if b.get(close) == Some(&b'\'') {
                    i = close;
                }
            }
            b',' if depth == 0 => {
                push(start, i, &mut out);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if !text.trim().is_empty() || !out.is_empty() {
        push(start, text.len(), &mut out);
    }
    out
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The text of a `"string"` operand, or an error message.
fn parse_string(text: &str) -> Result<Vec<u8>, (usize, String)> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().skip(1);
    loop {
        let Some((i, c)) = chars.next() else {
            return Err((text.len(), "the string needs a closing quote".into()));
        };
        match c {
            '"' => {
                let rest = text[i + 1..].trim();
                if !rest.is_empty() {
                    return Err((i + 1, format!("did not expect '{rest}' after the string")));
                }
                return Ok(out);
            }
            '\\' => match chars.next() {
                Some((_, 'n')) => out.push(b'\n'),
                Some((_, 'r')) => out.push(b'\r'),
                Some((_, 't')) => out.push(b'\t'),
                Some((_, '0')) => out.push(0),
                Some((_, c @ ('\\' | '"' | '\''))) => out.push(c as u8),
                _ => {
                    return Err((
                        i,
                        "unknown escape in the string (use \\n \\r \\t \\0 \\\\ \\\")".into(),
                    ));
                }
            },
            c if c.is_ascii() => out.push(c as u8),
            c => {
                return Err((
                    i,
                    format!("'{c}' is not an ASCII character, so it does not fit in a byte"),
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The assembler

#[derive(Default)]
struct Assembler {
    lines: Vec<String>,
    symbols: HashMap<String, Sym>,
    /// Labels as written, for the result.
    labels: Vec<(String, usize)>,
    stmts: Vec<Stmt>,
    /// `(line index, byte offset, message)`.
    errors: Vec<(usize, usize, String)>,
    /// Next word address.
    addr: usize,
    words: Vec<u16>,
    /// `(word address, line)` of every statement that emitted words.
    lines_map: Vec<(usize, usize)>,
}

/// Is this a mnemonic the assembler knows, and how many words is it?
fn instruction_words(name: &str) -> Option<usize> {
    let known = OPS.iter().any(|o| o.mnemonic == name)
        || matches!(
            name,
            "clr"
                | "tst"
                | "lsl"
                | "rol"
                | "ser"
                | "sbr"
                | "cbr"
                | "brlo"
                | "brsh"
                | "brbs"
                | "brbc"
        )
        || SET_NAMES.contains(&name)
        || CLEAR_NAMES.contains(&name)
        || BRANCH_SET_NAMES.contains(&name)
        || BRANCH_CLEAR_NAMES.contains(&name);
    let words = if matches!(name, "jmp" | "call" | "lds" | "sts") {
        2
    } else {
        1
    };
    known.then_some(words)
}

impl Assembler {
    fn error(&mut self, line: usize, at: usize, msg: impl Into<String>) {
        self.errors.push((line, at, msg.into()));
    }

    fn define(&mut self, line: usize, at: usize, name: &str, sym: Sym) {
        let key = name.to_ascii_uppercase();
        match self.symbols.entry(key) {
            std::collections::hash_map::Entry::Occupied(_) => {
                self.error(line, at, format!("'{name}' is already defined"));
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(sym);
            }
        }
    }

    fn ctx(&self, pc: usize) -> Ctx<'_> {
        Ctx {
            user: &self.symbols,
            pc: pc as i64,
            data: false,
        }
    }

    fn eval_at(&self, op: &Operand, pc: usize) -> Result<i64, E> {
        evaluate(&op.text, &self.ctx(pc)).map_err(|e| E::new(op.at + e.at, e.msg))
    }

    /// Read labels and directives, give every statement its address.
    fn pass_one(&mut self, source: &str) {
        self.lines = source.lines().map(str::to_string).collect();
        for li in 0..self.lines.len() {
            let line = self.lines[li].clone();
            let code = strip_comment(&line);
            let mut at = code.len() - code.trim_start().len();

            // Labels: `name:` at the start of the statement.
            loop {
                let b = code.as_bytes();
                if at < b.len() && is_ident_start(b[at]) {
                    let mut j = at;
                    while j < b.len() && is_ident(b[j]) {
                        j += 1;
                    }
                    if b.get(j) == Some(&b':') {
                        let name = &code[at..j];
                        let word = self.addr;
                        self.define(li, at, name, Sym::Const(word as i64));
                        self.labels.push((name.to_string(), word));
                        at = j + 1;
                        while at < b.len() && b[at].is_ascii_whitespace() {
                            at += 1;
                        }
                        continue;
                    }
                }
                break;
            }
            if at >= code.len() {
                continue;
            }

            let b = code.as_bytes();
            let mut j = at;
            if b[j] == b'.' {
                j += 1;
            }
            while j < b.len() && is_ident(b[j]) {
                j += 1;
            }
            let word = code[at..j].to_ascii_lowercase();
            let rest_at = j;
            let rest = &code[rest_at..];
            if word.starts_with('.') {
                self.directive(li, at, &word, rest, rest_at);
            } else if word.is_empty() {
                let c = code[at..].chars().next().unwrap_or(' ');
                self.error(li, at, format!("did not expect '{c}' here"));
            } else {
                let Some(size) = instruction_words(&word) else {
                    self.error(li, at, format!("'{word}' is not an instruction I know"));
                    continue;
                };
                let ops = split_operands(rest, rest_at);
                self.stmts.push(Stmt {
                    line: li,
                    at,
                    addr: self.addr,
                    kind: Kind::Instr { name: word, ops },
                });
                self.addr += size;
            }
        }
    }

    fn directive(&mut self, li: usize, at: usize, name: &str, rest: &str, rest_at: usize) {
        match name {
            ".org" => {
                let ops = split_operands(rest, rest_at);
                let [op] = ops.as_slice() else {
                    return self.error(li, at, ".org takes one operand: the word address");
                };
                match self.eval_at(op, self.addr) {
                    Err(e) => self.error(li, e.at, e.msg),
                    Ok(v) if v < self.addr as i64 => self.error(
                        li,
                        op.at,
                        format!(
                            "this .org goes back to 0x{v:X}, but the code already reaches 0x{:X}",
                            self.addr
                        ),
                    ),
                    Ok(v) if v as usize > FLASH_WORDS => {
                        self.error(li, op.at, "this address is beyond the end of flash")
                    }
                    Ok(v) => self.addr = v as usize,
                }
            }
            ".equ" | ".def" => {
                let Some(eq) = rest.find('=') else {
                    return self.error(li, at, format!("{name} needs NAME = value"));
                };
                let nm = rest[..eq].trim();
                let name_at = rest_at + rest[..eq].len() - rest[..eq].trim_start().len();
                if nm.is_empty() || !nm.bytes().next().is_some_and(is_ident_start) || !nm.bytes().all(is_ident) {
                    return self.error(li, name_at, format!("'{nm}' is not a valid name"));
                }
                let value = Operand {
                    text: rest[eq + 1..].trim().to_string(),
                    at: rest_at + eq + 1 + (rest[eq + 1..].len() - rest[eq + 1..].trim_start().len()),
                };
                if name == ".def" {
                    match register(&value.text, &self.symbols) {
                        Ok(n) => self.define(li, name_at, nm, Sym::Reg(n)),
                        Err(m) => self.error(li, value.at, m),
                    }
                } else {
                    match self.eval_at(&value, self.addr) {
                        Ok(v) => self.define(li, name_at, nm, Sym::Const(v)),
                        Err(e) => self.error(li, e.at, e.msg),
                    }
                }
            }
            ".db" | ".dw" | ".byte" | ".word" => {
                if name == ".byte" || name == ".word" {
                    return self.error(
                        li,
                        at,
                        format!("{name} reserves RAM, which this assembler does not do; use .db or .dw for flash"),
                    );
                }
                let ops = split_operands(rest, rest_at);
                if ops.is_empty() {
                    return self.error(li, at, format!("{name} needs at least one value"));
                }
                let words = if name == ".dw" {
                    ops.len()
                } else {
                    let bytes: usize = ops
                        .iter()
                        .map(|o| {
                            if o.text.starts_with('"') {
                                parse_string(&o.text).map_or(0, |s| s.len())
                            } else {
                                1
                            }
                        })
                        .sum();
                    bytes.div_ceil(2)
                };
                let kind = if name == ".dw" {
                    Kind::Words(ops)
                } else {
                    Kind::Bytes(ops)
                };
                self.stmts.push(Stmt {
                    line: li,
                    at,
                    addr: self.addr,
                    kind,
                });
                self.addr += words;
            }
            ".cseg" | ".device" | ".list" | ".nolist" | ".listmac" => {}
            ".dseg" | ".eseg" => self.error(
                li,
                at,
                format!("{name} is not supported: data and EEPROM segments are not built (use .db or .dw in code)"),
            ),
            ".include" | ".macro" | ".endmacro" | ".endm" | ".if" | ".ifdef" | ".ifndef"
            | ".else" | ".endif" | ".set" | ".undef" | ".define" => self.error(
                li,
                at,
                format!("{name} is not supported by this assembler"),
            ),
            _ => self.error(li, at, format!("unknown directive '{name}'")),
        }
    }

    fn pass_two(&mut self) {
        let stmts = std::mem::take(&mut self.stmts);
        let mut written: Vec<(usize, usize)> = Vec::new();
        for st in &stmts {
            let result = match &st.kind {
                Kind::Instr { name, ops } => self.encode(st, name, ops),
                Kind::Bytes(ops) => self.data_bytes(ops),
                Kind::Words(ops) => self.data_words(ops, st.addr),
            };
            let words = match result {
                Ok(w) => w,
                Err(e) => {
                    self.error(st.line, e.at, e.msg);
                    continue;
                }
            };
            if st.addr + words.len() > FLASH_WORDS {
                self.error(
                    st.line,
                    st.at,
                    "the program does not fit: the flash is 16K words (32 KB)",
                );
                continue;
            }
            if self.words.len() < st.addr + words.len() {
                self.words.resize(st.addr + words.len(), 0xFFFF);
            }
            self.words[st.addr..st.addr + words.len()].copy_from_slice(&words);
            written.push((st.addr, st.line + 1));
        }
        self.stmts = stmts;
        self.lines_map = written;
    }

    fn data_bytes(&self, ops: &[Operand]) -> Result<Vec<u16>, E> {
        let mut bytes: Vec<u8> = Vec::new();
        for op in ops {
            if op.text.starts_with('"') {
                bytes.extend(parse_string(&op.text).map_err(|(o, m)| E::new(op.at + o, m))?);
            } else {
                let v = self.eval_at(op, 0)?;
                if !(-128..=255).contains(&v) {
                    return Err(E::new(
                        op.at,
                        format!("{v} does not fit in a byte (-128 to 255)"),
                    ));
                }
                bytes.push(v as u8);
            }
        }
        if bytes.len() % 2 == 1 {
            bytes.push(0);
        }
        Ok(bytes
            .chunks(2)
            .map(|p| p[0] as u16 | (p[1] as u16) << 8)
            .collect())
    }

    fn data_words(&self, ops: &[Operand], addr: usize) -> Result<Vec<u16>, E> {
        ops.iter()
            .map(|op| {
                let v = self.eval_at(op, addr)?;
                if !(-32768..=65535).contains(&v) {
                    return Err(E::new(
                        op.at,
                        format!("{v} does not fit in a 16-bit word (-32768 to 65535)"),
                    ));
                }
                Ok(v as u16)
            })
            .collect()
    }

    fn finish(self, _source: &str) -> Result<Assembled, Vec<AsmError>> {
        if !self.errors.is_empty() {
            let mut errors: Vec<AsmError> = self
                .errors
                .iter()
                .map(|(line, at, msg)| {
                    let text = self.lines.get(*line).map_or("", String::as_str);
                    let end = (*at).min(text.len());
                    let end = (0..=end)
                        .rev()
                        .find(|i| text.is_char_boundary(*i))
                        .unwrap_or(0);
                    AsmError {
                        line: line + 1,
                        column: text[..end].chars().count() + 1,
                        message: msg.clone(),
                    }
                })
                .collect();
            errors.sort_by_key(|e| (e.line, e.column));
            errors.dedup();
            return Err(errors);
        }
        let mut labels = self.labels;
        labels.sort_by_key(|(_, a)| *a);
        Ok(Assembled {
            image: self.words.iter().flat_map(|w| w.to_le_bytes()).collect(),
            lines: self.lines_map,
            labels,
        })
    }
}

// ---------------------------------------------------------------------------
// Encoding one instruction

fn pattern(mnemonic: &str) -> u16 {
    OPS.iter()
        .find(|o| o.mnemonic == mnemonic)
        .expect("mnemonic is in the table")
        .pattern
}

fn rd_rr(base: u16, d: u8, r: u8) -> u16 {
    base | ((r as u16 & 0x10) << 5) | ((d as u16) << 4) | (r as u16 & 0xF)
}

fn rd_k(base: u16, d: u8, k: u8) -> u16 {
    base | ((k as u16 & 0xF0) << 4) | (((d - 16) as u16) << 4) | (k as u16 & 0xF)
}

/// Pointer operands of LD, ST, LDD, STD, LPM and ELPM.
enum Ptr {
    Plain(char),
    Post(char),
    Pre(char),
    /// Pointer register and the displacement expression's operand.
    Disp(char, Operand),
}

fn parse_pointer(op: &Operand) -> Result<Ptr, E> {
    let t: String = op.text.chars().filter(|c| !c.is_whitespace()).collect();
    let lower = t.to_ascii_lowercase();
    let reg = |c: char| matches!(c, 'x' | 'y' | 'z').then(|| c.to_ascii_uppercase());
    let cs: Vec<char> = lower.chars().collect();
    let bad = || {
        E::new(
            op.at,
            format!(
                "expected a pointer such as X, Y+, -Z or Z+5, not '{}'",
                op.text
            ),
        )
    };
    match cs.as_slice() {
        [c] => reg(*c).map(Ptr::Plain).ok_or_else(bad),
        [c, '+'] => reg(*c).map(Ptr::Post).ok_or_else(bad),
        ['-', c] => reg(*c).map(Ptr::Pre).ok_or_else(bad),
        [c, '+', ..] if matches!(c, 'y' | 'z') => {
            let plus = op.text.find('+').unwrap_or(0);
            let expr = &op.text[plus + 1..];
            let lead = expr.len() - expr.trim_start().len();
            Ok(Ptr::Disp(
                reg(*c).unwrap(),
                Operand {
                    text: expr.trim().to_string(),
                    at: op.at + plus + 1 + lead,
                },
            ))
        }
        _ => Err(bad()),
    }
}

fn plural(n: usize) -> String {
    match n {
        0 => "no operands".into(),
        1 => "1 operand".into(),
        n => format!("{n} operands"),
    }
}

impl Assembler {
    fn reg_at(&self, op: &Operand) -> Result<u8, E> {
        register(&op.text, &self.symbols).map_err(|m| E::new(op.at, m))
    }

    fn high_reg(&self, op: &Operand) -> Result<u8, E> {
        let r = self.reg_at(op)?;
        if r < 16 {
            return Err(E::new(
                op.at,
                format!("this instruction only works on r16 to r31, not r{r}"),
            ));
        }
        Ok(r)
    }

    fn number(&self, op: &Operand, pc: usize, lo: i64, hi: i64, what: &str) -> Result<i64, E> {
        let v = self.eval_at(op, pc)?;
        if !(lo..=hi).contains(&v) {
            return Err(E::new(
                op.at,
                format!("{what} is {v}, but it must be between {lo} and {hi}"),
            ));
        }
        Ok(v)
    }

    fn data_address(&self, op: &Operand, pc: usize) -> Result<u16, E> {
        let ctx = Ctx {
            user: &self.symbols,
            pc: pc as i64,
            data: true,
        };
        let v = evaluate(&op.text, &ctx).map_err(|e| E::new(op.at + e.at, e.msg))?;
        if !(0..=0xFFFF).contains(&v) {
            return Err(E::new(
                op.at,
                format!("the address is {v}, but it must be between 0 and 65535"),
            ));
        }
        Ok(v as u16)
    }

    /// A relative jump or branch: the distance in words from the next
    /// instruction.
    fn relative(&self, op: &Operand, addr: usize, bits: u32, name: &str) -> Result<u16, E> {
        let target = self.eval_at(op, addr)?;
        let rel = target - (addr as i64 + 1);
        let (lo, hi) = (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1);
        if !(lo..=hi).contains(&rel) {
            return Err(E::new(
                op.at,
                format!(
                    "the target is {rel:+} words away, but {name} reaches only {lo} to +{hi} words"
                ),
            ));
        }
        Ok(rel as u16 & ((1u16 << bits) - 1))
    }

    fn pair(&self, op: &Operand) -> Result<u8, E> {
        // r25:r24, a single even register, or X, Y, Z.
        let t = op.text.trim();
        let lower = t.to_ascii_lowercase();
        let single = match lower.as_str() {
            "x" => Some(26),
            "y" => Some(28),
            "z" => Some(30),
            _ => None,
        };
        if let Some(n) = single {
            return Ok(n);
        }
        if let Some((hi, lo)) = t.split_once(':') {
            let h = register(hi, &self.symbols).map_err(|m| E::new(op.at, m))?;
            let l = register(lo, &self.symbols).map_err(|m| E::new(op.at, m))?;
            if l % 2 != 0 || h != l + 1 {
                return Err(E::new(
                    op.at,
                    format!(
                        "r{h}:r{l} is not a register pair (the pair is r(n+1):r(n) with n even)"
                    ),
                ));
            }
            return Ok(l);
        }
        let r = self.reg_at(op)?;
        if r % 2 != 0 {
            return Err(E::new(
                op.at,
                format!("r{r} is not the low register of a pair; pairs start at an even register"),
            ));
        }
        Ok(r)
    }

    fn encode(&self, st: &Stmt, name: &str, ops: &[Operand]) -> Result<Vec<u16>, E> {
        let n = ops.len();
        let want = |k: usize| -> Result<(), E> {
            if n == k {
                Ok(())
            } else {
                Err(E::new(
                    st.at,
                    format!("'{name}' takes {}, but {} given", plural(k), plural(n)),
                ))
            }
        };
        let addr = st.addr;

        // Aliases and the instructions that take pointers.
        match name {
            "clr" | "tst" | "lsl" | "rol" => {
                want(1)?;
                let d = self.reg_at(&ops[0])?;
                let base = match name {
                    "clr" => "eor",
                    "tst" => "and",
                    "lsl" => "add",
                    _ => "adc",
                };
                return Ok(vec![rd_rr(pattern(base), d, d)]);
            }
            "ser" => {
                want(1)?;
                let d = self.high_reg(&ops[0])?;
                return Ok(vec![rd_k(pattern("ldi"), d, 0xFF)]);
            }
            "sbr" | "cbr" => {
                want(2)?;
                let d = self.high_reg(&ops[0])?;
                let k = self.number(&ops[1], addr, 0, 255, "the mask")? as u8;
                return Ok(vec![if name == "sbr" {
                    rd_k(pattern("ori"), d, k)
                } else {
                    rd_k(pattern("andi"), d, !k)
                }]);
            }
            "brlo" | "brsh" => {
                want(1)?;
                let rel = self.relative(&ops[0], addr, 7, name)?;
                let base = pattern(if name == "brlo" { "brbs" } else { "brbc" });
                return Ok(vec![base | rel << 3]);
            }
            "brbs" | "brbc" => {
                want(2)?;
                let flag = self.number(&ops[0], addr, 0, 7, "the SREG bit")? as u16;
                let rel = self.relative(&ops[1], addr, 7, name)?;
                return Ok(vec![pattern(name) | rel << 3 | flag]);
            }
            "bset" | "bclr" => {
                want(1)?;
                let flag = self.number(&ops[0], addr, 0, 7, "the SREG bit")? as u16;
                return Ok(vec![pattern(name) | flag << 4]);
            }
            "ld" | "ldd" | "st" | "std" => return self.pointer_access(st, name, ops),
            "lpm" | "elpm" if n == 0 => {
                let op = OPS
                    .iter()
                    .find(|o| o.mnemonic == name && o.form == Form::None)
                    .expect("table has LPM and ELPM with no operands");
                return Ok(vec![op.pattern]);
            }
            "lpm" | "elpm" => {
                want(2)?;
                let d = self.reg_at(&ops[0])?;
                let ptr = match parse_pointer(&ops[1])? {
                    Ptr::Plain('Z') => "Z",
                    Ptr::Post('Z') => "Z+",
                    _ => {
                        return Err(E::new(ops[1].at, format!("{name} reads through Z or Z+")));
                    }
                };
                let op = OPS
                    .iter()
                    .find(|o| o.mnemonic == name && o.form == Form::Load(ptr))
                    .expect("table has the pointer forms");
                return Ok(vec![op.pattern | (d as u16) << 4]);
            }
            _ => {}
        }
        if let Some(flag) = SET_NAMES.iter().position(|s| *s == name) {
            want(0)?;
            return Ok(vec![pattern("bset") | (flag as u16) << 4]);
        }
        if let Some(flag) = CLEAR_NAMES.iter().position(|s| *s == name) {
            want(0)?;
            return Ok(vec![pattern("bclr") | (flag as u16) << 4]);
        }
        if let Some(flag) = BRANCH_SET_NAMES.iter().position(|s| *s == name) {
            want(1)?;
            let rel = self.relative(&ops[0], addr, 7, name)?;
            return Ok(vec![pattern("brbs") | rel << 3 | flag as u16]);
        }
        if let Some(flag) = BRANCH_CLEAR_NAMES.iter().position(|s| *s == name) {
            want(1)?;
            let rel = self.relative(&ops[0], addr, 7, name)?;
            return Ok(vec![pattern("brbc") | rel << 3 | flag as u16]);
        }

        let op: &Op = OPS
            .iter()
            .find(|o| o.mnemonic == name)
            .expect("the name was checked in pass one");
        self.encode_form(st, op, ops, want)
    }

    fn encode_form(
        &self,
        st: &Stmt,
        op: &Op,
        ops: &[Operand],
        want: impl Fn(usize) -> Result<(), E>,
    ) -> Result<Vec<u16>, E> {
        let addr = st.addr;
        let p = op.pattern;
        let name = op.mnemonic;
        Ok(match op.form {
            Form::None => {
                want(0)?;
                vec![p]
            }
            Form::RdRr => {
                want(2)?;
                vec![rd_rr(p, self.reg_at(&ops[0])?, self.reg_at(&ops[1])?)]
            }
            Form::Rd => {
                want(1)?;
                vec![p | (self.reg_at(&ops[0])? as u16) << 4]
            }
            Form::RdK => {
                want(2)?;
                let d = self.high_reg(&ops[0])?;
                let k = self.number(&ops[1], addr, -128, 255, "the value")?;
                vec![rd_k(p, d, k as u8)]
            }
            Form::Movw => {
                want(2)?;
                let d = self.pair(&ops[0])?;
                let r = self.pair(&ops[1])?;
                vec![p | ((d / 2) as u16) << 4 | (r / 2) as u16]
            }
            Form::Muls => {
                want(2)?;
                let d = self.high_reg(&ops[0])?;
                let r = self.high_reg(&ops[1])?;
                vec![p | ((d - 16) as u16) << 4 | (r - 16) as u16]
            }
            Form::Mul8 => {
                want(2)?;
                let mut regs = [0u8; 2];
                for (i, o) in ops.iter().enumerate() {
                    let r = self.reg_at(o)?;
                    if !(16..24).contains(&r) {
                        return Err(E::new(
                            o.at,
                            format!("{name} works on r16 to r23, not r{r}"),
                        ));
                    }
                    regs[i] = r - 16;
                }
                vec![p | (regs[0] as u16) << 4 | regs[1] as u16]
            }
            Form::PairK => {
                want(2)?;
                let d = self.pair(&ops[0])?;
                if !(24..=30).contains(&d) {
                    return Err(E::new(
                        ops[0].at,
                        format!(
                            "{name} works on the pairs r25:r24, X, Y and Z, not r{}:r{d}",
                            d + 1
                        ),
                    ));
                }
                let k = self.number(&ops[1], addr, 0, 63, "the value")? as u16;
                vec![p | (((d - 24) / 2) as u16) << 4 | (k & 0x30) << 2 | (k & 0xF)]
            }
            Form::IoBit => {
                want(2)?;
                let a = self.number(&ops[0], addr, 0, 31, "the I/O address")? as u16;
                let b = self.number(&ops[1], addr, 0, 7, "the bit number")? as u16;
                vec![p | a << 3 | b]
            }
            Form::In => {
                want(2)?;
                let d = self.reg_at(&ops[0])? as u16;
                let a = self.number(&ops[1], addr, 0, 63, "the I/O address")? as u16;
                vec![p | d << 4 | (a & 0x30) << 5 | (a & 0xF)]
            }
            Form::Out => {
                want(2)?;
                let a = self.number(&ops[0], addr, 0, 63, "the I/O address")? as u16;
                let r = self.reg_at(&ops[1])? as u16;
                vec![p | r << 4 | (a & 0x30) << 5 | (a & 0xF)]
            }
            Form::Rel12 => {
                want(1)?;
                vec![p | self.relative(&ops[0], addr, 12, name)?]
            }
            Form::Branch { .. } => unreachable!("branches are handled by name"),
            Form::Abs22 => {
                want(1)?;
                let k = self.number(&ops[0], addr, 0, FLASH_WORDS as i64 - 1, "the word address")?
                    as u32;
                vec![
                    p | (((k >> 17) & 0x1F) << 4 | ((k >> 16) & 1)) as u16,
                    k as u16,
                ]
            }
            Form::Lds => {
                want(2)?;
                let d = self.reg_at(&ops[0])? as u16;
                vec![p | d << 4, self.data_address(&ops[1], addr)?]
            }
            Form::Sts => {
                want(2)?;
                let k = self.data_address(&ops[0], addr)?;
                let r = self.reg_at(&ops[1])? as u16;
                vec![p | r << 4, k]
            }
            Form::RdBit => {
                want(2)?;
                let d = self.reg_at(&ops[0])? as u16;
                let b = self.number(&ops[1], addr, 0, 7, "the bit number")? as u16;
                vec![p | d << 4 | b]
            }
            Form::Sreg { .. } => unreachable!("handled by name"),
            Form::Load(_) | Form::Store(_) | Form::DispLoad { .. } | Form::DispStore { .. } => {
                unreachable!("pointer instructions are handled by name")
            }
        })
    }

    /// LD, LDD, ST and STD.
    fn pointer_access(&self, st: &Stmt, name: &str, ops: &[Operand]) -> Result<Vec<u16>, E> {
        if ops.len() != 2 {
            return Err(E::new(
                st.at,
                format!("'{name}' takes 2 operands, but {} given", plural(ops.len())),
            ));
        }
        let is_load = name.starts_with("ld");
        let has_d = name.len() == 3; // LDD and STD
        let (reg_op, ptr_op) = if is_load {
            (&ops[0], &ops[1])
        } else {
            (&ops[1], &ops[0])
        };
        let d = self.reg_at(reg_op)? as u16;
        let ptr = parse_pointer(ptr_op)?;
        // Y or Z with no displacement, or with one, is LDD or STD.
        let (q, y) = match &ptr {
            Ptr::Plain(c @ ('Y' | 'Z')) => (Some(0i64), *c == 'Y'),
            Ptr::Disp(c, expr) => {
                if !has_d {
                    return Err(E::new(
                        ptr_op.at,
                        format!("use {name}d for a pointer with a displacement"),
                    ));
                }
                let v = self.number(expr, st.addr, 0, 63, "the displacement")?;
                (Some(v), *c == 'Y')
            }
            _ => (None, false),
        };
        if let Some(q) = q {
            let q = q as u16;
            let mnemonic = if is_load { "ldd" } else { "std" };
            let op = OPS
                .iter()
                .find(|o| {
                    o.mnemonic == mnemonic
                        && match o.form {
                            Form::DispLoad { y: oy } | Form::DispStore { y: oy } => oy == y,
                            _ => false,
                        }
                })
                .expect("table has LDD and STD");
            return Ok(vec![
                op.pattern | (q & 0x20) << 8 | (q & 0x18) << 7 | (q & 7) | d << 4,
            ]);
        }
        if has_d {
            return Err(E::new(
                ptr_op.at,
                format!("{name} needs Y or Z with a displacement, as in Y+5"),
            ));
        }
        let text = match ptr {
            Ptr::Plain(c) => c.to_string(),
            Ptr::Post(c) => format!("{c}+"),
            Ptr::Pre(c) => format!("-{c}"),
            Ptr::Disp(..) => unreachable!(),
        };
        let op = OPS.iter().find(|o| {
            o.mnemonic == name && matches!(o.form, Form::Load(t) | Form::Store(t) if t == text)
        });
        match op {
            Some(op) => Ok(vec![op.pattern | d << 4]),
            None => Err(E::new(
                ptr_op.at,
                format!("{name} cannot use the pointer '{text}'"),
            )),
        }
    }
}

#[cfg(test)]
mod tests;
