//! Microcontroller emulation for Dunbrec.
//!
//! [`Atmega328p`] runs real compiled AVR firmware (the chip on the Arduino
//! Uno and Nano) instruction by instruction, with the peripherals most
//! hobby firmware relies on:
//!
//! * digital I/O ports with internal pull-ups, pin-change and external
//!   interrupts;
//! * timers 0, 1 and 2 in every waveform mode, so `millis()`, `delay()` and
//!   PWM (`analogWrite`) work;
//! * the ADC (`analogRead`);
//! * USART transmit (`Serial.print`), including the waveform on the TX pin.
//!
//! The emulator knows nothing about circuits. A host (the simulator) runs
//! it in slices with [`Atmega328p::run`], which stops whenever the outside
//! world matters: a pin's drive changed, or the ADC needs an analog sample.
//! The host feeds pin levels back with [`Atmega328p::set_input`] and
//! analog voltages with [`Atmega328p::set_analog`].
//!
//! Firmware comes from Intel HEX files ([`hex`]), as produced by the Arduino
//! IDE ("Export Compiled Binary") or `avr-objcopy`, or from the built-in
//! demo programs ([`programs`]), which are written with the small
//! assembler in [`asm`].

pub mod asm;
pub mod asm_text;
mod avr;
pub mod disasm;
pub mod firmware;
pub mod hex;
pub mod isa;
mod periph;
pub mod programs;

pub use avr::{
    ARDUINO_NAMES, Atmega328p, CpuSnapshot, DEFAULT_CLOCK, FLASH_BYTES, PIN_COUNT, PIN_NAMES,
    PinDrive, Stop,
};

#[cfg(test)]
mod tests;
