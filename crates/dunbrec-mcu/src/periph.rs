//! ATmega328P peripherals: I/O ports, external and pin-change interrupts,
//! timers 0/1/2, the ADC and USART transmit. Register behaviour follows the
//! datasheet (DS40002061); simplifications are noted where they apply.

use crate::avr::Atmega328p;

/// Data-space addresses of the I/O registers used here.
pub(crate) mod reg {
    pub const PINB: usize = 0x23;
    pub const PINC: usize = 0x26;
    pub const PIND: usize = 0x29;
    pub const DDRB: usize = 0x24;
    pub const PORTB: usize = 0x25;
    pub const DDRC: usize = 0x27;
    pub const PORTC: usize = 0x28;
    pub const DDRD: usize = 0x2A;
    pub const PORTD: usize = 0x2B;
    pub const TIFR0: usize = 0x35;
    pub const TIFR1: usize = 0x36;
    pub const TIFR2: usize = 0x37;
    pub const PCIFR: usize = 0x3B;
    pub const EIFR: usize = 0x3C;
    pub const EIMSK: usize = 0x3D;
    pub const TCCR0A: usize = 0x44;
    pub const TCCR0B: usize = 0x45;
    pub const TCNT0: usize = 0x46;
    pub const OCR0A: usize = 0x47;
    pub const OCR0B: usize = 0x48;
    pub const SMCR: usize = 0x53;
    pub const MCUCR: usize = 0x55;
    pub const SPL: usize = 0x5D;
    pub const SPH: usize = 0x5E;
    pub const SREG: usize = 0x5F;
    pub const PCICR: usize = 0x68;
    pub const EICRA: usize = 0x69;
    pub const PCMSK0: usize = 0x6B;
    pub const TIMSK0: usize = 0x6E;
    pub const TIMSK1: usize = 0x6F;
    pub const TIMSK2: usize = 0x70;
    pub const ADCL: usize = 0x78;
    pub const ADCH: usize = 0x79;
    pub const ADCSRA: usize = 0x7A;
    pub const ADCSRB: usize = 0x7B;
    pub const ADMUX: usize = 0x7C;
    pub const TCCR1A: usize = 0x80;
    pub const TCCR1B: usize = 0x81;
    pub const TCNT1L: usize = 0x84;
    pub const TCNT1H: usize = 0x85;
    pub const ICR1L: usize = 0x86;
    pub const ICR1H: usize = 0x87;
    pub const OCR1AL: usize = 0x88;
    pub const OCR1AH: usize = 0x89;
    pub const OCR1BL: usize = 0x8A;
    pub const OCR1BH: usize = 0x8B;
    pub const TCCR2A: usize = 0xB0;
    pub const TCCR2B: usize = 0xB1;
    pub const TCNT2: usize = 0xB2;
    pub const OCR2A: usize = 0xB3;
    pub const OCR2B: usize = 0xB4;
    pub const UCSR0A: usize = 0xC0;
    pub const UCSR0B: usize = 0xC1;
    pub const UCSR0C: usize = 0xC2;
    pub const UBRR0L: usize = 0xC4;
    pub const UBRR0H: usize = 0xC5;
    pub const UDR0: usize = 0xC6;
}

use reg::*;

// ------------------------------------------------------------- timers ---

struct TimerDef {
    tccra: usize,
    tccrb: usize,
    tcnt: usize,
    ocr: [usize; 2],
    icr: usize,
    timsk: usize,
    tifr: usize,
    wide: bool,
    prescale: [u64; 8],
    /// (port index, bit) of the OCnA and OCnB pins; port 0 = D, 1 = B.
    pins: [(usize, u8); 2],
    /// Interrupt vector of compare A; compare B and overflow follow.
    vector: usize,
}

const SYNC_PRESCALE: [u64; 8] = [0, 1, 8, 64, 256, 1024, 0, 0];

const TIMERS: [TimerDef; 3] = [
    TimerDef {
        tccra: TCCR0A,
        tccrb: TCCR0B,
        tcnt: TCNT0,
        ocr: [OCR0A, OCR0B],
        icr: 0,
        timsk: TIMSK0,
        tifr: TIFR0,
        wide: false,
        prescale: SYNC_PRESCALE,
        pins: [(0, 6), (0, 5)],
        vector: 14,
    },
    TimerDef {
        tccra: TCCR1A,
        tccrb: TCCR1B,
        tcnt: TCNT1L,
        ocr: [OCR1AL, OCR1BL],
        icr: ICR1L,
        timsk: TIMSK1,
        tifr: TIFR1,
        wide: true,
        prescale: SYNC_PRESCALE,
        pins: [(1, 1), (1, 2)],
        vector: 11,
    },
    TimerDef {
        tccra: TCCR2A,
        tccrb: TCCR2B,
        tcnt: TCNT2,
        ocr: [OCR2A, OCR2B],
        icr: 0,
        timsk: TIMSK2,
        tifr: TIFR2,
        wide: false,
        prescale: [0, 1, 8, 32, 64, 128, 256, 1024],
        pins: [(1, 3), (0, 3)],
        vector: 7,
    },
];

const TOV: u8 = 1;
const OCF: [u8; 2] = [2, 4];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Normal,
    Ctc,
    Fast,
    Phase,
    /// Phase and frequency correct (timer 1): compare values update at
    /// BOTTOM instead of TOP.
    PhaseFreq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Top {
    Fixed(u16),
    Ocra,
    Icr,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Timer {
    /// Counting down (phase-correct modes).
    down: bool,
    /// Output compare levels OCnA, OCnB.
    oc: [bool; 2],
    /// Compare values in effect (the registers are double-buffered in PWM
    /// modes).
    ocr: [u16; 2],
}

// ---------------------------------------------------------------- ADC ---

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Adc {
    busy: bool,
    /// Cycle at which the running conversion completes.
    end: u64,
    /// Sampled value of the running conversion (0–1023).
    sample: u16,
    /// Analog channel waiting for the host to supply its voltage.
    pub request: Option<usize>,
    /// The first conversion after enabling takes 25 ADC clocks, later
    /// ones 13.
    warm: bool,
}

const ADC_PRESCALE: [u64; 8] = [2, 2, 4, 8, 16, 32, 64, 128];

// -------------------------------------------------------------- USART ---

#[derive(Debug, Clone, Copy)]
struct Frame {
    /// Line levels, least significant bit first (start bit = 0).
    bits: u16,
    len: u64,
    start: u64,
    bit_cycles: u64,
    byte: u8,
}

impl Frame {
    fn index(&self, cycle: u64) -> u64 {
        cycle.saturating_sub(self.start) / self.bit_cycles
    }
    fn end(&self) -> u64 {
        self.start + self.len * self.bit_cycles
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Usart {
    /// Transmit buffer (UDR0).
    buffer: Option<u8>,
    /// Frame being shifted out.
    shift: Option<Frame>,
    txc: bool,
}

impl Atmega328p {
    // ------------------------------------------------------- timers ---

    fn timer_mode(&self, t: usize) -> (Kind, Top) {
        let def = &TIMERS[t];
        let a = self.data[def.tccra];
        let b = self.data[def.tccrb];
        if def.wide {
            match (a & 3) | ((b >> 1) & 0xC) {
                0 => (Kind::Normal, Top::Fixed(0xFFFF)),
                1 => (Kind::Phase, Top::Fixed(0xFF)),
                2 => (Kind::Phase, Top::Fixed(0x1FF)),
                3 => (Kind::Phase, Top::Fixed(0x3FF)),
                4 => (Kind::Ctc, Top::Ocra),
                5 => (Kind::Fast, Top::Fixed(0xFF)),
                6 => (Kind::Fast, Top::Fixed(0x1FF)),
                7 => (Kind::Fast, Top::Fixed(0x3FF)),
                8 => (Kind::PhaseFreq, Top::Icr),
                9 => (Kind::PhaseFreq, Top::Ocra),
                10 => (Kind::Phase, Top::Icr),
                11 => (Kind::Phase, Top::Ocra),
                12 => (Kind::Ctc, Top::Icr),
                14 => (Kind::Fast, Top::Icr),
                15 => (Kind::Fast, Top::Ocra),
                _ => (Kind::Normal, Top::Fixed(0xFFFF)),
            }
        } else {
            match (a & 3) | ((b >> 1) & 4) {
                1 => (Kind::Phase, Top::Fixed(0xFF)),
                2 => (Kind::Ctc, Top::Ocra),
                3 => (Kind::Fast, Top::Fixed(0xFF)),
                5 => (Kind::Phase, Top::Ocra),
                7 => (Kind::Fast, Top::Ocra),
                _ => (Kind::Normal, Top::Fixed(0xFF)),
            }
        }
    }

    fn reg16(&self, addr: usize, wide: bool) -> u16 {
        if wide {
            u16::from_le_bytes([self.data[addr], self.data[addr + 1]])
        } else {
            self.data[addr] as u16
        }
    }

    fn timer_top(&self, t: usize, top: Top) -> u16 {
        match top {
            Top::Fixed(v) => v,
            Top::Ocra => self.timers[t].ocr[0],
            Top::Icr => self.reg16(TIMERS[t].icr, true),
        }
    }

    /// Compare outputs with COMnx = 1 toggle only in the modes whose TOP
    /// is OCRnA (and, for timer 1, fast PWM with TOP = ICR1).
    fn toggle_allowed(&self, t: usize) -> bool {
        let def = &TIMERS[t];
        let (a, b) = (self.data[def.tccra], self.data[def.tccrb]);
        if def.wide {
            matches!((a & 3) | ((b >> 1) & 0xC), 9 | 11 | 14 | 15)
        } else {
            b & 0x08 != 0
        }
    }

    fn com(&self, t: usize, ch: usize) -> u8 {
        (self.data[TIMERS[t].tccra] >> (6 - 2 * ch)) & 3
    }

    /// True if compare output `ch` of timer `t` overrides its port pin.
    fn oc_connected(&self, t: usize, ch: usize) -> bool {
        match self.com(t, ch) {
            0 => false,
            1 => {
                let (kind, _) = self.timer_mode(t);
                matches!(kind, Kind::Normal | Kind::Ctc) || (ch == 0 && self.toggle_allowed(t))
            }
            _ => true,
        }
    }

    fn set_oc(&mut self, t: usize, ch: usize, level: bool) {
        if self.timers[t].oc[ch] != level {
            self.timers[t].oc[ch] = level;
            self.dirty = true;
        }
    }

    /// Copy the compare registers into effect (immediately in non-PWM
    /// modes; at TOP or BOTTOM in PWM modes).
    fn load_ocr(&mut self, t: usize) {
        let def = &TIMERS[t];
        for ch in 0..2 {
            self.timers[t].ocr[ch] = self.reg16(def.ocr[ch], def.wide);
        }
    }

    fn timer_config_written(&mut self, t: usize) {
        if matches!(self.timer_mode(t).0, Kind::Normal | Kind::Ctc) {
            self.load_ocr(t);
        }
    }

    fn compare_match(&mut self, t: usize, ch: usize, kind: Kind, top: u16) {
        let com = self.com(t, ch);
        let ocr = self.timers[t].ocr[ch];
        let level = self.timers[t].oc[ch];
        match (kind, com) {
            (_, 0) => {}
            (Kind::Normal | Kind::Ctc, 1) => self.set_oc(t, ch, !level),
            (Kind::Normal | Kind::Ctc, c) => self.set_oc(t, ch, c == 3),
            (_, 1) => {
                if ch == 0 && self.toggle_allowed(t) {
                    self.set_oc(t, ch, !level);
                }
            }
            (Kind::Fast, c) => {
                // Compare equal to TOP: the output stays at its BOTTOM level.
                if ocr != top {
                    self.set_oc(t, ch, c == 3);
                }
            }
            (_, c) => {
                // Phase correct: clear on the way up, set on the way down
                // (non-inverting); OCR = BOTTOM or TOP gives a constant level.
                let mut high = self.timers[t].down;
                if ocr == 0 {
                    high = false;
                } else if ocr >= top {
                    high = true;
                }
                self.set_oc(t, ch, high == (c == 2));
            }
        }
    }

    fn timer_tick(&mut self, t: usize) {
        let def = &TIMERS[t];
        let wide = def.wide;
        let max: u16 = if wide { 0xFFFF } else { 0xFF };
        let (kind, top_src) = self.timer_mode(t);
        let top = self.timer_top(t, top_src);
        let cnt = self.reg16(def.tcnt, wide);
        let mut flags = 0u8;
        // Outside the phase-correct modes a match acts on the timer clock
        // after TCNT equals OCR (so fast PWM is high for OCR + 1 clocks and
        // CTC clears as the match fires).
        let phase = matches!(kind, Kind::Phase | Kind::PhaseFreq);
        let matched: [bool; 2] = std::array::from_fn(|ch| !phase && cnt == self.timers[t].ocr[ch]);
        let new = match kind {
            Kind::Normal | Kind::Ctc => {
                let wrap = if kind == Kind::Ctc { top } else { max };
                if cnt == max {
                    flags |= TOV;
                }
                if cnt == wrap {
                    0
                } else {
                    cnt.wrapping_add(1) & max
                }
            }
            Kind::Fast => {
                if cnt == top || cnt == max {
                    flags |= TOV;
                    // BOTTOM: new compare values; outputs set (non-inverting)
                    // or cleared (inverting).
                    self.load_ocr(t);
                    for ch in 0..2 {
                        match self.com(t, ch) {
                            2 => self.set_oc(t, ch, true),
                            3 => self.set_oc(t, ch, false),
                            _ => {}
                        }
                    }
                    0
                } else {
                    cnt + 1
                }
            }
            Kind::Phase | Kind::PhaseFreq => {
                let tm = &mut self.timers[t];
                let next = if !tm.down {
                    if cnt >= top {
                        tm.down = true;
                        cnt.saturating_sub(1).min(top)
                    } else {
                        cnt + 1
                    }
                } else if cnt == 0 {
                    tm.down = false;
                    if top > 0 { 1 } else { 0 }
                } else {
                    cnt - 1
                };
                if next == 0 {
                    flags |= TOV;
                    if kind == Kind::PhaseFreq {
                        self.load_ocr(t);
                    }
                }
                if next == top && !self.timers[t].down && kind == Kind::Phase {
                    self.load_ocr(t);
                }
                next
            }
        };
        if wide {
            let [lo, hi] = new.to_le_bytes();
            self.data[def.tcnt] = lo;
            self.data[def.tcnt + 1] = hi;
        } else {
            self.data[def.tcnt] = new as u8;
        }
        // Compare against the top in effect after any update above.
        let top = self.timer_top(t, top_src);
        for ch in 0..2 {
            if matched[ch] || (phase && new == self.timers[t].ocr[ch]) {
                flags |= OCF[ch];
                self.compare_match(t, ch, kind, top);
            }
        }
        if flags != 0 {
            self.data[def.tifr] |= flags;
        }
    }

    // ---------------------------------------------------------- ADC ---

    fn adc_start(&mut self) {
        let mux = self.data[ADMUX] & 0x0F;
        let div = ADC_PRESCALE[(self.data[ADCSRA] & 7) as usize];
        let clocks = if self.adc.warm { 13 } else { 25 };
        self.adc.warm = true;
        self.adc.busy = true;
        self.adc.end = self.cycle + clocks * div;
        self.data[ADCSRA] |= 0x40;
        match mux {
            0..=5 => self.adc.request = Some(mux as usize),
            // Temperature sensor: about 314 mV at 25 °C.
            8 => self.adc.sample = self.adc_code(0.314),
            // Internal 1.1 V bandgap.
            14 => self.adc.sample = self.adc_code(1.1),
            _ => self.adc.sample = 0,
        }
    }

    fn adc_code(&self, volts: f64) -> u16 {
        let vref = if self.data[ADMUX] >> 6 == 3 {
            1.1
        } else {
            self.vcc
        };
        (volts / vref * 1024.0).floor().clamp(0.0, 1023.0) as u16
    }

    /// Take the sample the host has just supplied for `channel`.
    pub(crate) fn adc_latch(&mut self, channel: usize) {
        self.adc.sample = self.adc_code(self.analog[channel]);
    }

    fn adc_complete(&mut self) {
        let v = self.adc.sample;
        if self.data[ADMUX] & 0x20 != 0 {
            self.data[ADCH] = (v >> 2) as u8;
            self.data[ADCL] = ((v & 3) << 6) as u8;
        } else {
            self.data[ADCL] = v as u8;
            self.data[ADCH] = (v >> 8) as u8;
        }
        self.adc.busy = false;
        self.data[ADCSRA] = (self.data[ADCSRA] & !0x40) | 0x10;
        // Auto trigger in free-running mode starts the next conversion.
        if self.data[ADCSRA] & 0x20 != 0 && self.data[ADCSRB] & 7 == 0 {
            self.adc_start();
        }
    }

    fn adcsra_written(&mut self, v: u8) {
        let old = self.data[ADCSRA];
        let mut new = (v & !0x50) | (old & 0x10);
        if v & 0x10 != 0 {
            new &= !0x10;
        }
        if new & 0x80 == 0 {
            self.adc.busy = false;
            self.adc.request = None;
            self.adc.warm = false;
        }
        if self.adc.busy {
            new |= 0x40;
        }
        self.data[ADCSRA] = new;
        if v & 0x40 != 0 && new & 0x80 != 0 && !self.adc.busy {
            self.adc_start();
        }
    }

    // -------------------------------------------------------- USART ---

    fn tx_enabled(&self) -> bool {
        self.data[UCSR0B] & 0x08 != 0
    }

    fn load_frame(&mut self, start: u64) {
        let Some(byte) = self.usart.buffer.take() else {
            return;
        };
        let ubrr = u16::from_le_bytes([self.data[UBRR0L], self.data[UBRR0H] & 0x0F]) as u64;
        let double = self.data[UCSR0A] & 0x02 != 0;
        let bit_cycles = (ubrr + 1) * if double { 8 } else { 16 };
        let c = self.data[UCSR0C];
        let size = ((c >> 1) & 3) | (self.data[UCSR0B] & 0x04);
        let data_bits: u32 = match size {
            0 => 5,
            1 => 6,
            2 => 7,
            7 => 9,
            _ => 8,
        };
        let mut word = byte as u16;
        if data_bits == 9 {
            word |= ((self.data[UCSR0B] & 1) as u16) << 8;
        }
        word &= (1u16 << data_bits) - 1;
        let mut bits = word << 1;
        let mut len = 1 + data_bits;
        let parity = (c >> 4) & 3;
        if parity >= 2 {
            let odd = word.count_ones() % 2 == 1;
            let p = if parity == 2 { odd } else { !odd };
            bits |= (p as u16) << len;
            len += 1;
        }
        let stops = if c & 0x08 != 0 { 2 } else { 1 };
        for _ in 0..stops {
            bits |= 1 << len;
            len += 1;
        }
        self.usart.shift = Some(Frame {
            bits,
            len: len as u64,
            start,
            bit_cycles,
            byte,
        });
        self.dirty = true;
    }

    fn usart_advance(&mut self, from: u64, to: u64) {
        while let Some(f) = self.usart.shift {
            if f.index(from) != f.index(to) {
                self.dirty = true;
            }
            let end = f.end();
            if end > to {
                break;
            }
            self.serial.push((end, f.byte));
            self.usart.txc = true;
            self.usart.shift = None;
            self.load_frame(end);
        }
    }

    fn tx_level(&self) -> bool {
        match self.usart.shift {
            Some(f) => {
                let k = f.index(self.cycle);
                k >= f.len || f.bits >> k & 1 == 1
            }
            None => true,
        }
    }

    // ------------------------------------------------- shared stepping ---

    /// Timer clock period of timer `def` in CPU cycles (`None`: stopped).
    fn prescale(&self, def: &TimerDef) -> Option<u64> {
        Some(def.prescale[(self.data[def.tccrb] & 7) as usize]).filter(|&p| p != 0)
    }

    pub(crate) fn advance_peripherals(&mut self, from: u64, to: u64) {
        for (t, def) in TIMERS.iter().enumerate() {
            if let Some(p) = self.prescale(def) {
                for _ in 0..(to / p - from / p) {
                    self.timer_tick(t);
                }
            }
        }
        self.usart_advance(from, to);
        if self.adc.busy && to >= self.adc.end {
            self.adc_complete();
        }
    }

    /// Earliest cycle after now at which a peripheral does something.
    pub(crate) fn next_peripheral_event(&self) -> u64 {
        let now = self.cycle;
        let mut next = u64::MAX;
        for def in &TIMERS {
            if let Some(p) = self.prescale(def) {
                next = next.min((now / p + 1) * p);
            }
        }
        if let Some(f) = self.usart.shift {
            next = next.min(f.start + (f.index(now) + 1) * f.bit_cycles);
        }
        if self.adc.busy {
            next = next.min(self.adc.end);
        }
        next.max(now + 1)
    }

    // ---------------------------------------------------------- pins ---

    /// Drive of all pins packed into one word: bits 0–19 output enable,
    /// 20–39 output level, 40–59 pull-up enable (Arduino pin order).
    pub(crate) fn compute_drive(&self) -> u64 {
        let d = &self.data;
        // Port order: D (pins 0–7), B (8–13), C (14–19).
        let mut dir = [d[DDRD], d[DDRB] & 0x3F, d[DDRC] & 0x3F];
        let mut val = [d[PORTD], d[PORTB] & 0x3F, d[PORTC] & 0x3F];
        let pud = d[MCUCR] & 0x10 != 0;
        let pull: [u8; 3] = std::array::from_fn(|p| if pud { 0 } else { !dir[p] & val[p] });
        for (t, def) in TIMERS.iter().enumerate() {
            for ch in 0..2 {
                if self.oc_connected(t, ch) {
                    let (port, bit) = def.pins[ch];
                    val[port] = (val[port] & !(1 << bit)) | ((self.timers[t].oc[ch] as u8) << bit);
                }
            }
        }
        let mut pull = pull;
        if self.tx_enabled() {
            dir[0] |= 2;
            pull[0] &= !2;
            val[0] = (val[0] & !2) | ((self.tx_level() as u8) << 1);
        }
        if d[UCSR0B] & 0x10 != 0 {
            // Receiver enabled: RXD is an input.
            dir[0] &= !1;
        }
        let pack = |b: [u8; 3]| b[0] as u64 | (b[1] as u64) << 8 | (b[2] as u64) << 14;
        let out: [u8; 3] = std::array::from_fn(|p| val[p] & dir[p]);
        pack(dir) | pack(out) << 20 | pack(pull) << 40
    }

    /// An input pin changed level: external and pin-change interrupt flags.
    pub(crate) fn pin_changed(&mut self, pin: usize, high: bool) {
        if pin == 2 || pin == 3 {
            let n = pin - 2;
            let fire = match (self.data[EICRA] >> (2 * n)) & 3 {
                1 => true,
                2 => !high,
                3 => high,
                _ => false,
            };
            if fire {
                self.data[EIFR] |= 1 << n;
            }
        }
        let (group, bit) = match pin {
            0..=7 => (2, pin),
            8..=13 => (0, pin - 8),
            _ => (1, pin - 14),
        };
        if self.data[PCMSK0 + group] >> bit & 1 == 1 {
            self.data[PCIFR] |= 1 << group;
        }
    }

    // ---------------------------------------------------- interrupts ---

    /// Highest-priority enabled interrupt with its flag set.
    pub(crate) fn pending_irq(&self) -> Option<usize> {
        let d = &self.data;
        for n in 0..2 {
            if d[EIMSK] >> n & 1 == 1 {
                let level_low = (d[EICRA] >> (2 * n)) & 3 == 0;
                if (level_low && !self.input(2 + n)) || (!level_low && d[EIFR] >> n & 1 == 1) {
                    return Some(1 + n);
                }
            }
        }
        let pc = d[PCIFR] & d[PCICR];
        if let Some(g) = (0..3).find(|g| pc >> g & 1 == 1) {
            return Some(3 + g);
        }
        // Vector order: timer 2 (7–9), timer 1 (10–13), timer 0 (14–16).
        for t in [2, 1, 0] {
            let def = &TIMERS[t];
            let f = d[def.tifr] & d[def.timsk];
            if t == 1 && f & 0x20 != 0 {
                return Some(10);
            }
            for (i, bit) in [OCF[0], OCF[1], TOV].into_iter().enumerate() {
                if f & bit != 0 {
                    return Some(def.vector + i);
                }
            }
        }
        let b = d[UCSR0B];
        if b & 0x20 != 0 && self.usart.buffer.is_none() {
            return Some(19);
        }
        if b & 0x40 != 0 && self.usart.txc {
            return Some(20);
        }
        if d[ADCSRA] & 0x18 == 0x18 {
            return Some(21);
        }
        None
    }

    /// Clear the flag of an interrupt being serviced (where hardware does).
    pub(crate) fn acknowledge_irq(&mut self, vector: usize) {
        match vector {
            1 | 2 => self.data[EIFR] &= !(1 << (vector - 1)),
            3..=5 => self.data[PCIFR] &= !(1 << (vector - 3)),
            7..=16 => {
                let (t, i) = match vector {
                    7..=9 => (2, vector - 7),
                    10 => (1, 3),
                    11..=13 => (1, vector - 11),
                    _ => (0, vector - 14),
                };
                let bit = [OCF[0], OCF[1], TOV, 0x20][i];
                self.data[TIMERS[t].tifr] &= !bit;
            }
            20 => self.usart.txc = false,
            21 => self.data[ADCSRA] &= !0x10,
            _ => {}
        }
    }

    // ------------------------------------------------- register access ---

    pub(crate) fn io_read(&mut self, addr: usize) -> u8 {
        match addr {
            PIND => self.inputs as u8,
            PINB => (self.inputs >> 8) as u8 & 0x3F,
            PINC => (self.inputs >> 14) as u8 & 0x3F,
            TCNT1L | ICR1L => {
                self.temp = self.data[addr + 1];
                self.data[addr]
            }
            TCNT1H | ICR1H => self.temp,
            UCSR0A => {
                (self.data[UCSR0A] & 0x03)
                    | (self.usart.buffer.is_none() as u8) << 5
                    | (self.usart.txc as u8) << 6
            }
            UDR0 => 0,
            _ => self.data[addr],
        }
    }

    pub(crate) fn io_write(&mut self, addr: usize, v: u8) {
        self.dirty = true;
        match addr {
            // Writing ones to PINx toggles PORTx.
            PINB | PINC | PIND => self.data[addr + 2] ^= v,
            TIFR0 | TIFR1 | TIFR2 | PCIFR | EIFR => self.data[addr] &= !v,
            TCNT1H | OCR1AH | OCR1BH | ICR1H => self.temp = v,
            TCNT1L | ICR1L | OCR1AL | OCR1BL => {
                self.data[addr] = v;
                self.data[addr + 1] = self.temp;
                if matches!(addr, OCR1AL | OCR1BL) {
                    self.timer_config_written(1);
                }
            }
            OCR0A | OCR0B => {
                self.data[addr] = v;
                self.timer_config_written(0);
            }
            OCR2A | OCR2B => {
                self.data[addr] = v;
                self.timer_config_written(2);
            }
            TCCR0A | TCCR0B | TCCR1A | TCCR1B | TCCR2A | TCCR2B => {
                // FOC strobe bits read as zero.
                let v = if matches!(addr, TCCR0B | TCCR2B) {
                    v & 0x3F
                } else {
                    v
                };
                self.data[addr] = v;
                let t = match addr {
                    TCCR0A | TCCR0B => 0,
                    TCCR1A | TCCR1B => 1,
                    _ => 2,
                };
                self.timer_config_written(t);
            }
            ADCSRA => self.adcsra_written(v),
            ADCL | ADCH => {}
            UDR0 => {
                if self.tx_enabled() && self.usart.buffer.is_none() {
                    self.usart.buffer = Some(v);
                    if self.usart.shift.is_none() {
                        self.load_frame(self.cycle);
                    }
                }
            }
            UCSR0A => {
                if v & 0x40 != 0 {
                    self.usart.txc = false;
                }
                self.data[UCSR0A] = v & 0x03;
            }
            UCSR0B => {
                self.data[UCSR0B] = v;
                if v & 0x08 == 0 {
                    self.usart.shift = None;
                    self.usart.buffer = None;
                }
            }
            _ => self.data[addr] = v,
        }
    }

    /// SBI / CBI: on flag and PINx registers only the addressed bit is
    /// written, so other flags are not cleared and other pins not toggled.
    pub(crate) fn write_bit(&mut self, addr: usize, bit: u8, set: bool) {
        match addr {
            PINB | PINC | PIND | TIFR0 | TIFR1 | TIFR2 | PCIFR | EIFR => {
                if set {
                    self.io_write(addr, 1 << bit);
                }
            }
            _ => {
                let v = self.io_read(addr);
                let v = if set { v | 1 << bit } else { v & !(1 << bit) };
                self.io_write(addr, v);
            }
        }
    }
}
