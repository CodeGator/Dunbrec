//! The AVR CPU core of the ATmega328P: registers, instruction decoding and
//! execution, the stack, interrupts and sleep. Peripherals live in
//! [`crate::periph`].

use std::sync::Arc;

use crate::periph::{Adc, Timer, Usart, reg};

pub const FLASH_BYTES: usize = 32 * 1024;
const FLASH_WORDS: usize = FLASH_BYTES / 2;
/// Registers, I/O space and 2 KB of SRAM.
pub(crate) const DATA_SIZE: usize = 0x900;
const RAMEND: u16 = 0x08FF;

/// Clock of an Arduino Uno (Hz).
pub const DEFAULT_CLOCK: f64 = 16e6;

/// Number of usable I/O pins: PD0–PD7, PB0–PB5 and PC0–PC5 (the crystal
/// and reset pins are not general I/O).
pub const PIN_COUNT: usize = 20;

/// Pin names, indexed like Arduino pin numbers (D0–D13, then A0–A5).
pub const PIN_NAMES: [&str; PIN_COUNT] = [
    "PD0", "PD1", "PD2", "PD3", "PD4", "PD5", "PD6", "PD7", "PB0", "PB1", "PB2", "PB3", "PB4",
    "PB5", "PC0", "PC1", "PC2", "PC3", "PC4", "PC5",
];

/// Arduino names of the pins, in the same order as [`PIN_NAMES`].
pub const ARDUINO_NAMES: [&str; PIN_COUNT] = [
    "D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7", "D8", "D9", "D10", "D11", "D12", "D13", "A0",
    "A1", "A2", "A3", "A4", "A5",
];

/// What a pin does to its net.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDrive {
    /// Output driven low.
    Low,
    /// Output driven high.
    High,
    /// Input with the internal pull-up resistor enabled.
    PullUp,
    /// High-impedance input.
    Floating,
}

/// Why [`Atmega328p::run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The requested cycle was reached.
    Reached,
    /// At least one pin's [`PinDrive`] changed.
    Pins,
    /// The ADC started converting analog channel `n` (0–5) and samples its
    /// voltage now: call [`Atmega328p::set_analog`] before running on.
    AdcSample(usize),
    /// The next instruction is at a breakpoint (see
    /// [`Atmega328p::set_breakpoints`]) and has **not** run. Calling `run`
    /// again runs it.
    Breakpoint,
    /// The instruction budget of [`Atmega328p::step_instruction`] was used.
    Stepped,
}

/// Everything a debugger shows, copied out of the chip.
#[derive(Debug, Clone, PartialEq)]
pub struct CpuSnapshot {
    /// Program counter (word address).
    pub pc: usize,
    pub sp: u16,
    pub sreg: u8,
    pub regs: [u8; 32],
    pub cycle: u64,
    pub sleeping: bool,
    pub fault: Option<String>,
    /// Data space: registers (0x00 to 0x1F), I/O (0x20 to 0xFF), SRAM.
    pub data: Box<[u8]>,
}

// SREG bits.
const C: u8 = 0;
const Z: u8 = 1;
const N: u8 = 2;
const V: u8 = 3;
const S: u8 = 4;
const H: u8 = 5;
const T: u8 = 6;
const I: u8 = 7;

#[derive(Clone)]
pub struct Atmega328p {
    flash: Arc<[u16]>,
    /// Data space: registers (0x00–0x1F), I/O (0x20–0xFF), SRAM.
    pub(crate) data: Box<[u8]>,
    /// Program counter (word address).
    pc: usize,
    pub(crate) cycle: u64,
    sleeping: bool,
    /// After SEI or RETI one more instruction runs before an interrupt.
    irq_delay: bool,
    fault: Option<String>,
    /// Logic level seen on each pin (bit per pin, Arduino numbering).
    pub(crate) inputs: u32,
    /// Voltage on each analog input A0–A5.
    pub(crate) analog: [f64; 6],
    pub(crate) vcc: f64,
    pub(crate) timers: [Timer; 3],
    pub(crate) adc: Adc,
    pub(crate) usart: Usart,
    /// High-byte buffer for 16-bit timer register access.
    pub(crate) temp: u8,
    /// Set when pin drives may have changed.
    pub(crate) dirty: bool,
    /// Pin drives last reported (packed, see `compute_drive`).
    drive: u64,
    /// Bytes transmitted by the USART: (cycle the stop bit ended, byte).
    pub(crate) serial: Vec<(u64, u8)>,
    /// Word addresses at which `run` stops before executing.
    breakpoints: Vec<usize>,
    /// False right after stopping at a breakpoint, so that resuming runs
    /// that instruction instead of stopping on it again.
    armed: bool,
}

impl std::fmt::Debug for Atmega328p {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Atmega328p")
            .field("pc", &self.pc)
            .field("cycle", &self.cycle)
            .field("fault", &self.fault)
            .finish_non_exhaustive()
    }
}

fn two_word(op: u16) -> bool {
    matches!(op & 0xFE0F, 0x9000 | 0x9200) || matches!(op & 0xFE0E, 0x940C | 0x940E)
}

impl Atmega328p {
    /// A chip programmed with `image` (flash contents from address 0),
    /// just out of reset, with all pins floating inputs.
    pub fn new(image: &[u8]) -> Result<Self, String> {
        if image.len() > FLASH_BYTES {
            return Err(format!(
                "the firmware is {} bytes, more than the ATmega328P's 32 KB of flash",
                image.len()
            ));
        }
        let mut flash = vec![0xFFFFu16; FLASH_WORDS];
        for (i, pair) in image.chunks(2).enumerate() {
            flash[i] = pair[0] as u16 | (*pair.get(1).unwrap_or(&0xFF) as u16) << 8;
        }
        let mut chip = Self {
            flash: flash.into(),
            data: vec![0; DATA_SIZE].into_boxed_slice(),
            pc: 0,
            cycle: 0,
            sleeping: false,
            irq_delay: false,
            fault: None,
            inputs: 0,
            analog: [0.0; 6],
            vcc: 5.0,
            timers: Default::default(),
            adc: Adc::default(),
            usart: Usart::default(),
            temp: 0,
            dirty: true,
            drive: 0,
            serial: Vec::new(),
            breakpoints: Vec::new(),
            armed: true,
        };
        chip.reset();
        Ok(chip)
    }

    /// Power-on reset: registers cleared, execution from address 0. Pin
    /// input levels and analog voltages are kept (they belong to the
    /// circuit). Cycle count continues.
    pub fn reset(&mut self) {
        self.data.fill(0);
        self.set_sp(RAMEND);
        self.data[reg::UCSR0C] = 0x06;
        self.pc = 0;
        self.sleeping = false;
        self.irq_delay = false;
        self.armed = true;
        self.fault = None;
        self.timers = Default::default();
        self.adc = Adc::default();
        self.usart = Usart::default();
        self.temp = 0;
        self.dirty = true;
        self.drive = self.compute_drive();
    }

    /// Clock cycles executed since power-on.
    pub fn cycle(&self) -> u64 {
        self.cycle
    }

    pub fn pc(&self) -> usize {
        self.pc
    }

    /// General-purpose register `r` (0–31).
    pub fn reg(&self, r: usize) -> u8 {
        self.data[r]
    }

    /// Byte of data space (registers, I/O, SRAM) without side effects.
    pub fn peek(&self, addr: usize) -> u8 {
        self.data.get(addr).copied().unwrap_or(0)
    }

    pub fn sreg(&self) -> u8 {
        self.data[reg::SREG]
    }

    /// Set when the CPU stopped on an invalid instruction; peripherals
    /// keep running.
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    pub fn is_sleeping(&self) -> bool {
        self.sleeping
    }

    /// Data space (registers, I/O, SRAM), read without side effects.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Flash as words, for disassembly.
    pub fn flash(&self) -> &[u16] {
        &self.flash
    }

    /// Stop `run` before executing the instruction at any of these word
    /// addresses (the program counter's units). Replaces the previous set.
    pub fn set_breakpoints(&mut self, words: &[usize]) {
        self.breakpoints = words.to_vec();
        self.breakpoints.sort_unstable();
        self.breakpoints.dedup();
    }

    pub fn breakpoints(&self) -> &[usize] {
        &self.breakpoints
    }

    /// A copy of the state a debugger shows.
    pub fn snapshot(&self) -> CpuSnapshot {
        CpuSnapshot {
            pc: self.pc,
            sp: self.sp(),
            sreg: self.sreg(),
            regs: std::array::from_fn(|r| self.data[r]),
            cycle: self.cycle,
            sleeping: self.sleeping,
            fault: self.fault.clone(),
            data: self.data.clone(),
        }
    }

    /// The logic level the circuit presents on `pin`.
    pub fn set_input(&mut self, pin: usize, high: bool) {
        let old = self.inputs >> pin & 1 == 1;
        if old == high {
            return;
        }
        self.inputs ^= 1 << pin;
        self.pin_changed(pin, high);
    }

    pub fn input(&self, pin: usize) -> bool {
        self.inputs >> pin & 1 == 1
    }

    /// Voltage on analog input `channel` (A0–A5), used by ADC samples.
    pub fn set_analog(&mut self, channel: usize, volts: f64) {
        if let Some(a) = self.analog.get_mut(channel) {
            *a = volts;
        }
    }

    /// Present drive of every pin.
    pub fn pin_drives(&self) -> [PinDrive; PIN_COUNT] {
        let packed = self.compute_drive();
        std::array::from_fn(|pin| {
            let out = packed >> pin & 1 == 1;
            let high = packed >> (20 + pin) & 1 == 1;
            let pull = packed >> (40 + pin) & 1 == 1;
            match (out, high, pull) {
                (true, true, _) => PinDrive::High,
                (true, false, _) => PinDrive::Low,
                (false, _, true) => PinDrive::PullUp,
                (false, _, false) => PinDrive::Floating,
            }
        })
    }

    /// Bytes sent by the USART since the last call, each with the cycle at
    /// which its stop bit ended.
    pub fn take_serial(&mut self) -> Vec<(u64, u8)> {
        std::mem::take(&mut self.serial)
    }

    /// Run until cycle `until`, or earlier if the outside world needs to
    /// know (see [`Stop`]). The returned stop happened at [`Self::cycle`].
    pub fn run(&mut self, until: u64) -> Stop {
        self.run_for(until, u64::MAX)
    }

    /// Execute one instruction (or interrupt entry), unless cycle `until`
    /// comes first. The instruction has run when this returns
    /// [`Stop::Stepped`], `Pins` or `AdcSample` (the last two mean the
    /// outside world must hear about it); with [`Stop::Breakpoint`] nothing
    /// ran.
    pub fn step_instruction(&mut self, until: u64) -> Stop {
        self.run_for(until, 1)
    }

    fn run_for(&mut self, until: u64, mut steps: u64) -> Stop {
        if let Some(ch) = self.adc.request.take() {
            self.adc_latch(ch);
        }
        if let Some(stop) = self.check_pins() {
            return stop;
        }
        while self.cycle < until {
            let delayed = std::mem::take(&mut self.irq_delay);
            if self.sleeping || self.fault.is_some() {
                if self.fault.is_none() && self.pending_irq().is_some() {
                    self.sleeping = false;
                    continue;
                }
                // Nothing runs but the peripherals: jump to their next event.
                let next = self.next_peripheral_event().min(until);
                self.tick(next - self.cycle);
            } else if !delayed
                && self.data[reg::SREG] >> I & 1 == 1
                && let Some(vector) = self.pending_irq()
            {
                self.service_irq(vector);
                self.tick(4);
                self.armed = true;
                steps = steps.saturating_sub(1);
            } else {
                if self.armed && !self.breakpoints.is_empty() && self.breakpoints.contains(&self.pc)
                {
                    self.armed = false;
                    // Nothing ran: the instruction after SEI or RETI still
                    // has to run before an interrupt.
                    self.irq_delay = delayed;
                    return Stop::Breakpoint;
                }
                let cycles = self.step();
                self.tick(cycles as u64);
                self.armed = true;
                steps = steps.saturating_sub(1);
            }
            if let Some(ch) = self.adc.request {
                return Stop::AdcSample(ch);
            }
            if let Some(stop) = self.check_pins() {
                return stop;
            }
            if steps == 0 {
                return Stop::Stepped;
            }
        }
        Stop::Reached
    }

    fn tick(&mut self, cycles: u64) {
        let from = self.cycle;
        self.cycle += cycles;
        self.advance_peripherals(from, self.cycle);
    }

    fn check_pins(&mut self) -> Option<Stop> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        let d = self.compute_drive();
        (d != self.drive).then(|| {
            self.drive = d;
            Stop::Pins
        })
    }

    // ------------------------------------------------------- memory ---

    pub(crate) fn read(&mut self, addr: usize) -> u8 {
        if addr < 0x20 {
            self.data[addr]
        } else if addr < 0x100 {
            self.io_read(addr)
        } else {
            self.data.get(addr).copied().unwrap_or(0)
        }
    }

    pub(crate) fn write(&mut self, addr: usize, v: u8) {
        if addr < 0x20 {
            self.data[addr] = v;
        } else if addr < 0x100 {
            self.io_write(addr, v);
        } else if addr < DATA_SIZE {
            self.data[addr] = v;
        }
    }

    fn flash_byte(&self, addr: usize) -> u8 {
        let w = self.flash[(addr >> 1) % FLASH_WORDS];
        if addr & 1 == 0 {
            w as u8
        } else {
            (w >> 8) as u8
        }
    }

    fn fetch(&self, pc: usize) -> u16 {
        self.flash[pc % FLASH_WORDS]
    }

    /// Stack pointer.
    pub fn sp(&self) -> u16 {
        u16::from_le_bytes([self.data[reg::SPL], self.data[reg::SPH]])
    }

    fn set_sp(&mut self, sp: u16) {
        let [lo, hi] = sp.to_le_bytes();
        self.data[reg::SPL] = lo;
        self.data[reg::SPH] = hi;
    }

    fn push(&mut self, v: u8) {
        let sp = self.sp();
        self.write(sp as usize, v);
        self.set_sp(sp.wrapping_sub(1));
    }

    fn pop(&mut self) -> u8 {
        let sp = self.sp().wrapping_add(1);
        self.set_sp(sp);
        self.read(sp as usize)
    }

    /// Return addresses go on the stack low byte first.
    fn push_pc(&mut self, pc: usize) {
        self.push(pc as u8);
        self.push((pc >> 8) as u8);
    }

    fn pop_pc(&mut self) -> usize {
        let hi = self.pop() as usize;
        let lo = self.pop() as usize;
        (hi << 8 | lo) % FLASH_WORDS
    }

    fn pair(&self, r: usize) -> u16 {
        u16::from_le_bytes([self.data[r], self.data[r + 1]])
    }

    fn set_pair(&mut self, r: usize, v: u16) {
        let [lo, hi] = v.to_le_bytes();
        self.data[r] = lo;
        self.data[r + 1] = hi;
    }

    // ----------------------------------------------------- interrupts ---

    fn service_irq(&mut self, vector: usize) {
        self.acknowledge_irq(vector);
        self.push_pc(self.pc);
        self.data[reg::SREG] &= !(1 << I);
        self.pc = vector * 2;
        self.sleeping = false;
    }

    // -------------------------------------------------------- flags ---

    fn flag(&self, bit: u8) -> bool {
        self.data[reg::SREG] >> bit & 1 == 1
    }

    /// Replace the flags not in `keep` with `bits`.
    fn set_flags(&mut self, keep: u8, bits: u8) {
        let s = &mut self.data[reg::SREG];
        *s = (*s & keep) | (bits & !keep);
    }

    fn znvs(r: u8, v: bool) -> u8 {
        let n = r & 0x80 != 0;
        ((r == 0) as u8) << Z | (n as u8) << N | (v as u8) << V | ((n ^ v) as u8) << S
    }

    fn add8(&mut self, a: u8, b: u8, carry: bool) -> u8 {
        let r = a.wrapping_add(b).wrapping_add(carry as u8);
        let cv = (a & b) | (b & !r) | (!r & a);
        let v = (a & b & !r) | (!a & !b & r);
        let bits = Self::znvs(r, v & 0x80 != 0) | ((cv >> 3 & 1) << H) | ((cv >> 7) << C);
        self.set_flags(1 << T | 1 << I, bits);
        r
    }

    /// `a - b - carry`. With `chain` (SBC, SBCI, CPC), Z can only stay set.
    fn sub8(&mut self, a: u8, b: u8, carry: bool, chain: bool) -> u8 {
        let r = a.wrapping_sub(b).wrapping_sub(carry as u8);
        let bv = (!a & b) | (b & r) | (r & !a);
        let v = (a & !b & !r) | (!a & b & r);
        let mut bits = Self::znvs(r, v & 0x80 != 0) | ((bv >> 3 & 1) << H) | ((bv >> 7) << C);
        if chain && !self.flag(Z) {
            bits &= !(1 << Z);
        }
        self.set_flags(1 << T | 1 << I, bits);
        r
    }

    fn logic(&mut self, r: u8) -> u8 {
        self.set_flags(1 << C | 1 << H | 1 << T | 1 << I, Self::znvs(r, false));
        r
    }

    /// Flags after a right shift: C is the bit shifted out.
    fn shift_flags(&mut self, r: u8, c: bool) {
        let n = r & 0x80 != 0;
        let v = n ^ c;
        let bits = ((r == 0) as u8) << Z
            | (n as u8) << N
            | (v as u8) << V
            | ((n ^ v) as u8) << S
            | (c as u8) << C;
        self.set_flags(1 << H | 1 << T | 1 << I, bits);
    }

    fn mul_flags(&mut self, r: u16, c: bool) {
        self.set_flags(!(1 << C | 1 << Z), ((r == 0) as u8) << Z | (c as u8) << C);
    }

    // --------------------------------------------------- execution ---

    /// Skip the next instruction; returns the extra cycles.
    fn skip(&mut self) -> u32 {
        if two_word(self.fetch(self.pc)) {
            self.pc = (self.pc + 2) % FLASH_WORDS;
            2
        } else {
            self.pc = (self.pc + 1) % FLASH_WORDS;
            1
        }
    }

    fn invalid(&mut self, op: u16, at: usize) -> u32 {
        self.fault = Some(if op == 0xFFFF {
            format!(
                "the program ran into empty flash at byte address 0x{:04X}",
                at * 2
            )
        } else {
            format!(
                "unsupported instruction 0x{op:04X} at byte address 0x{:04X}",
                at * 2
            )
        });
        self.pc = at;
        1
    }

    /// Execute one instruction; returns the cycles it took.
    fn step(&mut self) -> u32 {
        let at = self.pc;
        let op = self.fetch(at);
        self.pc = (at + 1) % FLASH_WORDS;
        let d = ((op >> 4) & 0x1F) as usize;
        let r = ((op & 0x0F) | ((op >> 5) & 0x10)) as usize;
        let dh = 16 + ((op >> 4) & 0x0F) as usize;
        let k8 = (((op >> 4) & 0xF0) | (op & 0x0F)) as u8;
        match op >> 12 {
            0x0 => match (op >> 10) & 3 {
                0 => match op >> 8 {
                    0x00 if op == 0 => 1,
                    0x01 => {
                        let (d2, r2) = ((op >> 4 & 0xF) as usize * 2, (op & 0xF) as usize * 2);
                        self.data[d2] = self.data[r2];
                        self.data[d2 + 1] = self.data[r2 + 1];
                        1
                    }
                    0x02 => {
                        let (a, b) = (
                            self.data[dh] as i8,
                            self.data[16 + (op & 0xF) as usize] as i8,
                        );
                        let p = (a as i16 * b as i16) as u16;
                        self.set_pair(0, p);
                        self.mul_flags(p, p & 0x8000 != 0);
                        2
                    }
                    0x03 => {
                        let a = self.data[16 + (op >> 4 & 7) as usize];
                        let b = self.data[16 + (op & 7) as usize];
                        let (p, fractional) = match op & 0x88 {
                            0x00 => ((a as i8 as i16 * b as i16) as u16, false),
                            0x08 => ((a as u16) * (b as u16), true),
                            0x80 => ((a as i8 as i16 * b as i8 as i16) as u16, true),
                            _ => ((a as i8 as i16 * b as i16) as u16, true),
                        };
                        let c = p & 0x8000 != 0;
                        let p = if fractional { p << 1 } else { p };
                        self.set_pair(0, p);
                        self.mul_flags(p, c);
                        2
                    }
                    _ => self.invalid(op, at),
                },
                1 => {
                    let c = self.flag(C);
                    self.sub8(self.data[d], self.data[r], c, true);
                    1
                }
                2 => {
                    let c = self.flag(C);
                    self.data[d] = self.sub8(self.data[d], self.data[r], c, true);
                    1
                }
                _ => {
                    self.data[d] = self.add8(self.data[d], self.data[r], false);
                    1
                }
            },
            0x1 => match (op >> 10) & 3 {
                0 => {
                    if self.data[d] == self.data[r] {
                        1 + self.skip()
                    } else {
                        1
                    }
                }
                1 => {
                    self.sub8(self.data[d], self.data[r], false, false);
                    1
                }
                2 => {
                    self.data[d] = self.sub8(self.data[d], self.data[r], false, false);
                    1
                }
                _ => {
                    let c = self.flag(C);
                    self.data[d] = self.add8(self.data[d], self.data[r], c);
                    1
                }
            },
            0x2 => {
                match (op >> 10) & 3 {
                    0 => self.data[d] = self.logic(self.data[d] & self.data[r]),
                    1 => self.data[d] = self.logic(self.data[d] ^ self.data[r]),
                    2 => self.data[d] = self.logic(self.data[d] | self.data[r]),
                    _ => self.data[d] = self.data[r],
                }
                1
            }
            0x3 => {
                self.sub8(self.data[dh], k8, false, false);
                1
            }
            0x4 => {
                let c = self.flag(C);
                self.data[dh] = self.sub8(self.data[dh], k8, c, true);
                1
            }
            0x5 => {
                self.data[dh] = self.sub8(self.data[dh], k8, false, false);
                1
            }
            0x6 => {
                self.data[dh] = self.logic(self.data[dh] | k8);
                1
            }
            0x7 => {
                self.data[dh] = self.logic(self.data[dh] & k8);
                1
            }
            0x8 | 0xA => {
                // LDD / STD with displacement (LD/ST Y and Z when q = 0).
                let q = ((op >> 8) & 0x20) | ((op >> 7) & 0x18) | (op & 7);
                let base = if op & 8 != 0 {
                    self.pair(28)
                } else {
                    self.pair(30)
                };
                let addr = base.wrapping_add(q) as usize;
                if op & 0x200 != 0 {
                    self.write(addr, self.data[d]);
                } else {
                    self.data[d] = self.read(addr);
                }
                2
            }
            0x9 => self.exec_9(op, at, d, r),
            0xB => {
                let a = 0x20 + (((op >> 5) & 0x30) | (op & 0xF)) as usize;
                if op & 0x800 != 0 {
                    self.write(a, self.data[d]);
                } else {
                    self.data[d] = self.read(a);
                }
                1
            }
            0xC | 0xD => {
                let k = ((op & 0xFFF) as i16) << 4 >> 4;
                if op >> 12 == 0xD {
                    self.push_pc(self.pc);
                }
                self.pc = (self.pc as isize + k as isize).rem_euclid(FLASH_WORDS as isize) as usize;
                if op >> 12 == 0xD { 3 } else { 2 }
            }
            0xE => {
                self.data[dh] = k8;
                1
            }
            _ => {
                let bit = (op & 7) as u8;
                match (op >> 9) & 7 {
                    0..=3 => {
                        // BRBS / BRBC
                        let set = self.flag(bit);
                        let want = op & 0x400 == 0;
                        if set == want {
                            let k = ((op >> 3) & 0x7F) as i8;
                            let k = (k << 1) >> 1;
                            self.pc = (self.pc as isize + k as isize)
                                .rem_euclid(FLASH_WORDS as isize)
                                as usize;
                            2
                        } else {
                            1
                        }
                    }
                    _ if op & 8 != 0 => self.invalid(op, at),
                    4 => {
                        let t = self.flag(T);
                        self.data[d] = (self.data[d] & !(1 << bit)) | ((t as u8) << bit);
                        1
                    }
                    5 => {
                        let t = self.data[d] >> bit & 1;
                        self.set_flags(!(1 << T), t << T);
                        1
                    }
                    6 | 7 => {
                        let set = self.data[d] >> bit & 1 == 1;
                        if set == ((op >> 9) & 7 == 7) {
                            1 + self.skip()
                        } else {
                            1
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    fn exec_9(&mut self, op: u16, at: usize, d: usize, r: usize) -> u32 {
        match op & 0xFE00 {
            0x9000 | 0x9200 => {
                let store = op & 0x200 != 0;
                let mode = op & 0xF;
                if mode == 0 {
                    let addr = self.fetch(self.pc) as usize;
                    self.pc = (self.pc + 1) % FLASH_WORDS;
                    if store {
                        self.write(addr, self.data[d]);
                    } else {
                        self.data[d] = self.read(addr);
                    }
                    return 2;
                }
                if mode == 0xF {
                    if store {
                        self.push(self.data[d]);
                    } else {
                        self.data[d] = self.pop();
                    }
                    return 2;
                }
                if !store && matches!(mode, 4..=7) {
                    // LPM / ELPM (no RAMPZ on this chip, so ELPM = LPM).
                    let z = self.pair(30);
                    self.data[d] = self.flash_byte(z as usize);
                    if mode & 1 == 1 {
                        self.set_pair(30, z.wrapping_add(1));
                    }
                    return 3;
                }
                let ptr = match mode {
                    1 | 2 => 30,
                    9 | 10 => 28,
                    12..=14 => 26,
                    _ => return self.invalid(op, at),
                };
                let mut p = self.pair(ptr);
                let pre_dec = matches!(mode, 2 | 10 | 14);
                let post_inc = matches!(mode, 1 | 9 | 13);
                if pre_dec {
                    p = p.wrapping_sub(1);
                }
                if store {
                    self.write(p as usize, self.data[d]);
                } else {
                    self.data[d] = self.read(p as usize);
                }
                if post_inc {
                    p = p.wrapping_add(1);
                }
                if pre_dec || post_inc {
                    self.set_pair(ptr, p);
                }
                2
            }
            0x9400 => self.exec_94(op, at, d),
            0x9600 => {
                let rd = 24 + ((op >> 4) & 3) as usize * 2;
                let k = ((op >> 2) & 0x30) | (op & 0xF);
                let a = self.pair(rd);
                let sub = op & 0x100 != 0;
                let res = if sub {
                    a.wrapping_sub(k)
                } else {
                    a.wrapping_add(k)
                };
                let (a15, r15) = (a & 0x8000 != 0, res & 0x8000 != 0);
                let (v, c) = if sub {
                    (a15 && !r15, r15 && !a15)
                } else {
                    (!a15 && r15, !r15 && a15)
                };
                let bits = ((res == 0) as u8) << Z
                    | (r15 as u8) << N
                    | (v as u8) << V
                    | ((r15 ^ v) as u8) << S
                    | (c as u8) << C;
                self.set_flags(1 << H | 1 << T | 1 << I, bits);
                self.set_pair(rd, res);
                2
            }
            0x9800 | 0x9A00 => {
                let a = 0x20 + ((op >> 3) & 0x1F) as usize;
                let bit = (op & 7) as u8;
                match (op >> 8) & 3 {
                    0 => {
                        self.write_bit(a, bit, false);
                        2
                    }
                    2 => {
                        self.write_bit(a, bit, true);
                        2
                    }
                    k => {
                        let set = self.read(a) >> bit & 1 == 1;
                        if set == (k == 3) { 1 + self.skip() } else { 1 }
                    }
                }
            }
            _ => {
                // MUL
                let p = self.data[d] as u16 * self.data[r] as u16;
                self.set_pair(0, p);
                self.mul_flags(p, p & 0x8000 != 0);
                2
            }
        }
    }

    fn exec_94(&mut self, op: u16, at: usize, d: usize) -> u32 {
        let v = self.data[d];
        match op & 0xF {
            0x0 => {
                let r = !v;
                self.set_flags(1 << H | 1 << T | 1 << I, Self::znvs(r, false) | 1 << C);
                self.data[d] = r;
                1
            }
            0x1 => {
                let r = 0u8.wrapping_sub(v);
                let h = (r | v) >> 3 & 1;
                let bits = Self::znvs(r, r == 0x80) | h << H | ((r != 0) as u8) << C;
                self.set_flags(1 << T | 1 << I, bits);
                self.data[d] = r;
                1
            }
            0x2 => {
                self.data[d] = v.rotate_left(4);
                1
            }
            0x3 => {
                let r = v.wrapping_add(1);
                self.set_flags(1 << C | 1 << H | 1 << T | 1 << I, Self::znvs(r, r == 0x80));
                self.data[d] = r;
                1
            }
            0xA => {
                let r = v.wrapping_sub(1);
                self.set_flags(1 << C | 1 << H | 1 << T | 1 << I, Self::znvs(r, r == 0x7F));
                self.data[d] = r;
                1
            }
            0x5..=0x7 => {
                let top = match op & 0xF {
                    0x5 => v & 0x80,
                    0x6 => 0,
                    _ => (self.flag(C) as u8) << 7,
                };
                let r = (v >> 1) | top;
                self.shift_flags(r, v & 1 == 1);
                self.data[d] = r;
                1
            }
            0x8 => {
                if op & 0x100 == 0 {
                    // BSET / BCLR
                    let bit = ((op >> 4) & 7) as u8;
                    let set = op & 0x80 == 0;
                    self.set_flags(!(1 << bit), (set as u8) << bit);
                    if set && bit == I {
                        self.irq_delay = true;
                    }
                    return 1;
                }
                match op {
                    0x9508 => {
                        self.pc = self.pop_pc();
                        4
                    }
                    0x9518 => {
                        self.pc = self.pop_pc();
                        self.data[reg::SREG] |= 1 << I;
                        self.irq_delay = true;
                        4
                    }
                    0x9588 => {
                        if self.data[reg::SMCR] & 1 == 1 {
                            self.sleeping = true;
                        }
                        1
                    }
                    // BREAK and WDR: no debugger or watchdog; carry on.
                    0x9598 | 0x95A8 => 1,
                    0x95C8 | 0x95D8 => {
                        let z = self.pair(30);
                        self.data[0] = self.flash_byte(z as usize);
                        3
                    }
                    // SPM: self-programming is not emulated.
                    0x95E8 => 1,
                    _ => self.invalid(op, at),
                }
            }
            0x9 => match op {
                0x9409 => {
                    self.pc = self.pair(30) as usize % FLASH_WORDS;
                    2
                }
                0x9509 => {
                    self.push_pc(self.pc);
                    self.pc = self.pair(30) as usize % FLASH_WORDS;
                    3
                }
                _ => self.invalid(op, at),
            },
            0xC..=0xF => {
                let hi = (((op >> 3) & 0x3E) | (op & 1)) as usize;
                let k = (hi << 16 | self.fetch(self.pc) as usize) % FLASH_WORDS;
                let ret = (self.pc + 1) % FLASH_WORDS;
                if op & 2 != 0 {
                    self.push_pc(ret);
                    self.pc = k;
                    4
                } else {
                    self.pc = k;
                    3
                }
            }
            _ => self.invalid(op, at),
        }
    }
}
