# Dunbrec — handoff

State of the project for whoever picks it up next. User-facing documentation is in
[README.md](README.md); this file covers status, decisions and open work.

*Last updated: 2026-11-21, Phase 12 (integrated circuits).*

## Status

All eleven planned phases are done and committed on `main` (one commit per phase, "Phase N: …"),
plus Phase 12 (integrated circuits) and a fix for probes failing after opening a document.
Version is `0.1.0`; nothing has been released or pushed by this work.

| Phase | Content |
|---|---|
| 1–3 | Workspace, data model, netlister, `.dbr` format; linear DC (MNA); transient + scheduler |
| 4 | Diode/BJT/MOSFET models, Newton-Raphson, junction capacitances, ringing control |
| 5–6 | egui schematic editor; live simulation with oscilloscope and multimeter |
| 7–8 | SPICE/KiCad import, SPICE export; DC sweep, AC, Bode plotter, curve tracer, function generator |
| 9 | Parts library, examples, clipboard, settings, icon, MSI installer |
| 10 | Event-driven digital logic mixed with analog; logic analyser |
| 11 | ATmega328P emulation with `.hex` firmware; serial monitor; Arduino examples |
| 12 | Composite parts; 741 op-amp (Boyle macromodel); 555 timer; examples 12–14 |
| 13 | Dockable panels: every panel is an `egui_dock` tab; layout saved between sessions |

Health at handoff: `cargo test --workspace` 237 passed, 0 failed (1 ignored: an emulator speed
benchmark); `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`
clean. Toolchain: Rust 1.99 (edition 2024, MSRV 1.90), Windows 11.

## Building and checking

```sh
cargo run --release -p dunbrec-app -- --example 12   # examples 1–14, see File → Open example
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo test -p dunbrec-mcu --release speed -- --ignored --nocapture   # emulator speed (~139 MHz equiv.)
powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1   # MSI; needs WiX (dotnet tool)
```

## Architecture in brief

| Crate | Role |
|---|---|
| `dunbrec-core` | Schematic, `ComponentKind`, pins/domains, units, models, netlister + diagnostics |
| `dunbrec-sim` | MNA solver, devices, DC/sweep/AC/transient; `digital.rs` (logic), `mcu.rs` (MCU glue) |
| `dunbrec-mcu` | Standalone ATmega328P emulator, Intel HEX, firmware loading, assembler, demo programs |
| `dunbrec-io` | `.dbr` (RON), SPICE import/export, KiCad import, auto layout |
| `dunbrec-instruments` | Scope/meter signal processing |
| `dunbrec-app` | eframe app; `live.rs` runs the transient on a worker thread |

UI layout (`dock.rs`, `PaneViewer` in `app.rs`):

- The schematic, palette, properties, checks and values are `Pane` tabs in an
  `egui_dock::DockState`. The menu bar, run toolbar and status bar are fixed. `dock.rs` has the
  pane list, `default_layout`, `open` / `close` placement and `sanitize`; `PaneViewer` (in
  `app.rs`) draws each pane from borrowed app state.
- **Instruments never dock** (the user's decision): each is a floating `egui::Window`
  (`Bench::scope_window` / `meter_window` / `logic_window`, `Analyses::show`,
  `SerialMonitor::window`), switched by the `show_*` flags. They briefly were dock tabs in the
  first version of Phase 13. `Pane::Retired` (`#[serde(other)]`) lets layouts saved then still
  load; those tabs are dropped on load.
- In `egui_dock` splits, `fraction` is the share of the *first* child (left or top), which is
  the new panel for `split_left` despite the crate's wording. `dock.rs` has a test on the real
  proportions.
- The layout is saved under `dunbrec.layout.v1` in eframe storage, separate from `Settings`,
  and restored through `sanitize` (retired tabs dropped, duplicates removed, no schematic means
  default). Bump the key suffix if panes change in a way old layouts cannot express.

Key mechanisms worth knowing before changing the engine:

- **The transient scheduler owns time** (`transient.rs`). It lands steps exactly on
  breakpoints, and cuts steps at watched threshold crossings. Digital inputs and MCU pins are
  just watches on analog nets.
- **Logic parts** (`digital.rs`) are event-driven. Outputs are Norton drivers with a ramp;
  inputs use VIH/VIL hysteresis.
- **Microcontrollers** (`mcu.rs`):
  - Each pin is an `McuPin` Norton device: 25 Ω output, 35 kΩ pull-up, 1 GΩ floating.
  - The chip runs ahead on a clone until its next event (pin change, ADC sample or horizon).
    The analog solver steps exactly there and the clone is adopted.
  - An input crossing before that point discards the clone, replays the committed chip to the
    crossing, then applies the input. Timing is cycle-exact.
  - Sync points that are not pin changes do not reset integration (`kink = false`).
- **Integrated circuits** (`macromodel.rs`):
  - A `MacroModel` is a small netlist, like a SPICE `.subckt`. A `Composite` device builds its
    elements as ordinary devices and presents them as one device, with internal nodes named
    `U1#<net>` (and `U1#<part>.<suffix>` for a part's own internal nodes). Device indices still
    match netlist elements.
  - The 741 is the PSpice-library Boyle macromodel. Its linear POLY sources are sums of plain
    controlled sources. `dunbrec_core::opamp_subckt` holds the SPICE original, which SPICE export
    writes out; keep the two in step. Adding an op-amp means adding its macromodel plus an entry
    in `OPAMP_PARTS` and `opamp_subckt`.
  - The 555 is mixed-signal. Its analog half (divider, output-high drop, supply current) is a
    composite. Its comparators, latch and switches are a digital part (`circuit.rs`,
    `timer555_logic`). Inputs are `InputSense` comparators (`V(p) − V(n)` against thresholds),
    and outputs are `SwitchDriver`s: resistive switches that ramp geometrically over 100 ns.
  - Tests in `ic_tests.rs` check data-sheet behaviour: gain, GBW, slew, swing, 555 timing.
- **Live edits** go through `Transient::update_element`. Returning `Ok(false)` means "restart
  needed", which is what an MCU firmware or clock change does.

## Decisions (agreed with the user)

- GUI egui/eframe; own Rust simulation engine. ngspice is a dev-time reference only and is
  never shipped.
- Publisher CodeGator; **GPL-3.0-only** licence (changed from MIT on 2026-11-21; earlier commits
  were published under MIT); version stays 0.1.0 for now.
- Microcontrollers:
  - Own AVR emulator; simavr was rejected because it is GPL (that reason lapsed when the
    project became GPL-3.0; the in-house emulator stays).
  - ATmega328P first.
  - Firmware is a `.hex` path stored in the schematic (relative to the `.dbr`), reloaded on
    every Run, plus `builtin:<name>` demo programs and `source:<name>` (assembly text kept in
    the schematic, built on every Run).
  - File format: `Schematic.sources` (a map of name to `{ language, text }`) was added with
    `#[serde(default)]`, so older `.dbr` files load unchanged and the version number was not
    bumped. An older Dunbrec opening a file that has sources ignores them.
- The user wants technical options explained in plain terms, with a recommendation, and
  confirmed before they become plans.
- Don't drive the user's mouse or keyboard for UI testing without asking. Window screenshots
  are fine.

## Open items

1. **ngspice golden cross-check** (from phase 4).
   - Needs ngspice installed.
   - Plan: store reference results for fixture circuits and compare within tolerance: about
     0.1 % for DC, bounded RMS error for transients.
2. **Real Arduino firmware tests.**
   - No AVR toolchain is installed, so every MCU test program was assembled in-house
     (`dunbrec_mcu::asm`).
   - Install Arduino IDE or arduino-cli, build a few sketches (blink, `analogWrite`,
     `analogRead`, `Serial.print`, `millis`) and add them as golden tests. This is the most
     valuable remaining check of the emulator.
3. **UI verification of phase 11 in the running app.**
   - Covered so far by worker-thread tests and a screenshot of the schematic.
   - Pressing Run and watching the serial monitor in the window has not been done.
4. **Stale text:** the Simulate menu still says "Transient & instruments: coming in phase 6"
   (`crates/dunbrec-app/src/app.rs`).
5. **More chips.**
   - Op-amps such as the LM358 and TL072, and a comparator such as the LM393, would reuse the
     composite machinery.
   - A CMOS 555 (TLC555) would need different output levels.
   - The ngspice cross-check (item 1) can now also cover the exported 741 subcircuit.
6. **MCU firmware development** (debugger, assembly text, Arduino C++ builds): phases A and B
   built; C (Arduino C++ via arduino-cli) and D (more chips) are not. See
   [docs/mcu-firmware-development.md](docs/mcu-firmware-development.md). None of the new
   windows (debugger, code panel) has been looked at on screen yet.
7. **Not yet supported:**
   - MCU: SPI, I²C, EEPROM, watchdog, serial receive and other chips.
   - Simulation: a fixed temperature of 27 °C, MOSFET level 1 only, and a single logic family.
   - Import: KiCad hierarchical sheets, SPICE `.param` expressions and EXP/SFFM sources.

## Gotchas

- Running the release exe locks `target\release\dunbrec.exe`. Close the app before rebuilding.
- At 2.5 V an ADC input reads 511 or 512 depending on source impedance (pin leakage). Tests
  accept both.
- When scripting file edits from Git Bash, heredocs can turn `\r\n` and `\` line
  continuations into literal characters. Prefer direct file edits for strings with escapes.
