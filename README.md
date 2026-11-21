# Dunbrec

A desktop electronics workbench written in Rust: draw or import a schematic, simulate it, and
watch any node live with virtual instruments — oscilloscope, multimeter, function generator,
Bode plotter and curve tracer.

Circuits can mix analog parts (discrete components, sources, diodes, transistors),
integrated circuits (a 741 op-amp and a 555 timer), digital logic (gates, flip-flops, counters,
clocks) and microcontrollers running real firmware (an ATmega328P, as on the Arduino Uno) in
one simulation.

## Installing

Build the Windows installer (needs the WiX Toolset: `dotnet tool install --global wix`):

```powershell
powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1
```

This produces `target\installer\Dunbrec-<version>-x64.msi`, which installs Dunbrec to Program
Files, adds a Start-menu shortcut and opens `.dbr` files with Dunbrec. Or run from source:

```sh
cargo run --release -p dunbrec-app                     # empty schematic
cargo run --release -p dunbrec-app -- --example 2      # an example (1–14, see File → Open example)
cargo run --release -p dunbrec-app -- my.dbr           # open a file
cargo run --release -p dunbrec-app -- amp.cir          # import a SPICE netlist
cargo run --release -p dunbrec-app -- board.kicad_sch  # import a KiCad schematic
```

## Using Dunbrec

**Drawing.** Pick a part from the palette and click to place it (R rotates, X mirrors, Esc
stops). Right-clicking a placed part rotates it a quarter-turn. Or drag a part from the palette onto
the canvas: it is placed where you drop it, stays selected (R and X still turn it) and the
wire tool starts, so you can wire it straight away. W draws wires (click to bend; a wire ends by
itself on a pin or another wire, and clicks inside a part's body are ignored mid-wire; clicking a
part's body instead of a pin switches back to Select, and a drag there moves the part; in Select,
a click on a pin starts a wire from it while a drag from it still moves the part; drag a wire's
corner or free end to reshape it, with guide lines showing where a segment becomes straight), L adds
net labels — labels with the same name are connected. Drag to move (attached wires follow),
drag empty space to box-select, Del deletes, Ctrl+C / X / V / D copy, cut, paste and
duplicate, Ctrl+Z / Y undo and redo. Wheel zooms, middle-drag pans. Values accept engineering
notation (`4k7`, `10u`, `2.2M`). Help → Keyboard shortcuts lists everything.

**Panels.** The schematic, palette, properties, checks and live values are tabs you can drag.
Drop one on the edge of another panel to split the window, on the middle of one to stack it as
a tab, or outside the window area to float it. Drag the dividers to resize. Closing a tab hides
it: bring Palette, Properties, Checks or Values back from View → Panels. The arrangement is
remembered between sessions; View → Reset layout puts everything back. Instruments (the
oscilloscope, multimeter and the rest) are separate floating windows that you can move and
resize freely but that never dock; open them from the toolbar or the Instruments menu.

**Checking.** The Checks panel lists wiring problems in plain English (missing ground,
unconnected pins, missing models), and the DC operating point is recalculated after every edit
with each node's voltage shown on the schematic.

**Simulating.** Press ▶ Run (or Space) for a live time-domain simulation paced to the clock;
the Speed box sets how much simulated time passes per second. Open instruments from the
toolbar or the Instruments menu, click an input's 🔌 Probe button, then click a wire or pin
(or a part, for current). Edits while running take effect at once: double-click a switch to
flip it, drag a potentiometer's wiper, use the function generator, change a value. Changing the
wiring restarts the run.

| Instrument | What it does |
|---|---|
| Oscilloscope | 4 channels, edge trigger (auto/normal/single), AC coupling, Vpp/mean/RMS/frequency |
| Multimeter | DC and true-RMS AC volts and amps |
| Function generator | Drives a voltage source: sine, square (with duty cycle), triangle, DC |
| Bode plotter | AC small-signal gain and phase from a source to a node; peak and −3 dB points |
| DC transfer | Sweeps a source and plots a node's voltage |
| Curve tracer | Characteristic curves of a diode, BJT or MOSFET in the schematic |
| Logic analyser | 8 channels of logic levels over time, with frequency read-outs |
| Serial monitor | Text a microcontroller prints on its serial port (`Serial.print`) |

**Digital logic.** The palette's *Digital* group has AND, OR, NAND, NOR, XOR and XNOR gates,
an inverter, a D flip-flop, a 4-bit counter, a clock, a logic switch (double-click to flip) and
a logic LED that lights on the schematic. Logic is event-driven with a propagation delay per
part, and meets the analog circuit at its pins like real 5 V CMOS: outputs drive their net
through 50 Ω with a 5 ns edge, inputs switch at 3.5 V rising and 1.5 V falling. So a gate can
drive a capacitor, an analog signal can clock a counter, and the scope works on any net. Try
File → Open example → Logic gates or Binary counter (run with Speed at real time).

**Integrated circuits.** The palette's *Integrated circuits* group has two classic chips:

- **741 op-amp** — the standard Boyle macromodel of the uA741 (as in the PSpice library):
  two input transistors, about 200 000 open-loop gain, 1 MHz gain-bandwidth, 0.5 V/µs slew
  rate, an output current limit, and an output that stops 1–2 V short of the supplies. Pins:
  in+, in− (on top), V+, V−, out; power it from a supply such as ±15 V.
- **555 timer** — the bipolar NE555. Its 5 kΩ divider sets the references (CTRL at 2/3 VCC,
  and 1/3 VCC for the trigger); the output goes high (about VCC − 1.4 V) when TRIG falls below
  1/3 VCC and low when THR rises above CTRL, DIS shorts to GND while the output is low, and
  RESET below 0.7 V forces the output low. Astable and monostable circuits time to the usual
  formulas.

Try File → Open example → Op-amp inverting amplifier, 555 LED blinker or 555 one-shot (run the
555 examples at real-time speed). Each chip is a *composite part*: one symbol that simulates as
a small built-in circuit of ordinary devices, so more op-amps and similar chips can be added as
data.

**Microcontrollers.** The palette's *Microcontrollers* group has an ATmega328P with the Arduino
pin names (D0–D13, A0–A5) and an implicit 5 V supply. It runs real compiled firmware cycle by
cycle: digital I/O with pull-ups, pin-change and external interrupts, timers 0/1/2 (so
`millis()`, `delay()` and `analogWrite` PWM work), the ADC (`analogRead`) and serial transmit.
In the Arduino IDE use *Sketch → Export Compiled Binary* and pick the `.hex` file (not the
`with_bootloader` one) in the part's properties; relative paths are relative to the schematic,
and the file is reloaded each time the simulation starts, so rebuild and press Run again. Pins
meet the circuit like the real chip: outputs drive through about 25 Ω, pull-ups are 35 kΩ,
inputs switch at 3.0 V rising and 1.5 V falling, and the ADC samples the net's voltage. Built-in
demo programs (`blink`, `fade`, `pot-pwm-serial`) need no toolchain; try File → Open example →
Arduino blink, Arduino PWM fade, or Arduino pot, PWM and serial.

**Debugging firmware.** Instruments → *MCU debugger* opens a floating window for the
microcontrollers in the circuit. It lists the firmware as assembly (registers and I/O by name,
jump targets as byte addresses); **click a line's gutter to set or clear a breakpoint**, even
before the circuit runs. *Start paused at reset* runs the circuit paused at time zero, so you can
**Step** one instruction at a time from the very first one; *Continue* runs to the next
breakpoint, which pauses the whole circuit at that exact instant and opens the window. While
stopped it shows the program counter, stack pointer, status flags, cycle count and simulated
time, the 32 registers (changed ones highlighted), and a view of data memory. It shows the state
the circuit has seen at the stop, not a live view of a running chip. Breakpoints are kept per
part for the session; *Clear all breakpoints* removes them all.

**Writing firmware in assembly.** View → Panels → *Code* opens an editor tab beside the
schematic for AVR assembly kept inside the schematic file. Create a file with *New…* (or pick
*Source file → New assembly file…* in a microcontroller's properties, which also points the part
at it); the first one starts as a blink program for D13. The dialect is Atmel's: `ldi r16, 0xFF`,
`sbi PORTB, PB5`, labels, `.equ`, `.def`, `.org`, `.db`, `.dw`, expressions with `low()` and
`high()`, and the register, bit and vector names from `m328Pdef.inc`. Text is coloured as you
type and built on every change: mistakes are underlined, listed under the editor (click one to
jump to it), and reported by line when the circuit runs. The part's firmware reads
`source:<name>`. In the debugger the listing shows the source line beside each instruction.
Macros, `.include` and data or EEPROM segments are not supported yet.

**Parts library.** Besides generic parts, the palette has real ones: the 2N3904 and 2N3906
transistors and the 1N4148 and 1N750 (4.7 V zener) diodes, using the published manufacturer
SPICE models. They sit in the group for what they are (Transistors, Diodes), after the generic
parts, and are recognisable by part number. Palette groups fold open and shut, and the search
box finds parts by name, group or type ("3904", "diode", "operational"); Enter picks the first
match. Each is checked by the test suite against its datasheet limits
(current gain, saturation voltages, transition frequency, forward voltage, leakage, zener
voltage). Any part's model can be switched to a library model in the properties panel.

**Files.** Schematics are saved as `.dbr` (readable text). File → Import reads SPICE netlists
(R, C, L, V/I with DC/SIN/PULSE/PWL, D, Q, M, E/F/G/H, `.model`, `.subckt`) and KiCad 6+
schematics (top sheet; `Device`, `Simulation_SPICE`, `Switch` and `power` libraries plus
`Sim.*` fields). Imported parts are connected with net labels; anything that could not be
imported is listed. File → Export SPICE netlist writes a netlist ngspice can run; a 741 is
written as a subcircuit call with its `.subckt` definition included.

## Workspace

| Crate | Purpose |
|---|---|
| `dunbrec-core` | Schematic data model, units, device models, parts library, netlister |
| `dunbrec-io` | `.dbr` format, SPICE import/export, KiCad import, automatic layout |
| `dunbrec-sim` | Simulation engine: MNA, Newton-Raphson, DC/sweep/AC/transient, device models |
| `dunbrec-mcu` | ATmega328P emulator (AVR core and peripherals), Intel HEX, a small assembler |
| `dunbrec-instruments` | Oscilloscope and multimeter signal processing |
| `dunbrec-app` | egui/eframe desktop application |

## Roadmap

1. Foundations: data model, units, netlister, `.dbr` save/load ✅
2. Linear DC solver (MNA) ✅
3. Scheduler + transient analysis ✅
4. Nonlinear devices with junction capacitances and transit times ✅ (ngspice cross-check still to do)
5. Schematic editor ✅
6. Live simulation + instruments ✅
7. SPICE / KiCad import, SPICE export ✅
8. DC sweep, AC analysis, Bode plotter, curve tracer, function generator ✅
9. Polish: parts library, examples, copy/paste, settings, icon, installer ✅
10. Digital logic: gates, flip-flops, counters, clocks, logic analyser ✅
11. Microcontrollers: ATmega328P emulation, `.hex` firmware, GPIO/timers/PWM/ADC/serial ✅
12. Integrated circuits: composite parts, 741 op-amp (Boyle macromodel), 555 timer ✅
13. Dockable, draggable panels (egui_dock) with a remembered layout ✅

Known limits: fixed temperature (27 °C); MOSFETs are level 1 only; one logic family (5 V
CMOS-style); logic parts, microcontrollers and 555 timers are not exported to SPICE; the 555
has no input bias currents and switches its outputs rather than modelling its transistors; the ATmega328P has no
SPI, I²C, EEPROM, watchdog, serial receive or sleep-mode power differences yet, and only the
ATmega328P is available; KiCad hierarchical sheets, SPICE `.param`
expressions and EXP/SFFM sources are not supported yet.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
python tools/make_icon.py   # regenerate assets/dunbrec.png and .ico
```

## License

Copyright © 2026 CodeGator. Dunbrec is free software: you can redistribute it and/or modify it
under the terms of the GNU General Public License, version 3 only, as published by the Free
Software Foundation. It is distributed in the hope that it will be useful, but without any
warranty. See [LICENSE](LICENSE) for the full text.

The project was published under the MIT licence until 2026-11-21; copies obtained under that
licence stay under it, and everything from this change on is GPL-3.0.
