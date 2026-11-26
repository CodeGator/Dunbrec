# Developing Dunbrec

## Building and running

Build the Windows installer (needs the WiX Toolset: `dotnet tool install --global wix`):

```powershell
powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1
```

This produces `target\installer\Dunbrec-<version>-x64.msi`, which installs Dunbrec to Program
Files, adds a Start-menu shortcut and opens `.dbr` and `.dbrproj` files with Dunbrec. Or run from source:

```sh
cargo run --release -p dunbrec-app                     # empty schematic
cargo run --release -p dunbrec-app -- --example 2      # an example (1–15, see File → Open example)
cargo run --release -p dunbrec-app -- my.dbr           # open a file
cargo run --release -p dunbrec-app -- amp.cir          # import a SPICE netlist
cargo run --release -p dunbrec-app -- board.kicad_sch  # import a KiCad schematic
```

## Workspace

| Crate | Purpose |
|---|---|
| `dunbrec-core` | Schematic data model, units, device models, parts library, netlister |
| `dunbrec-io` | `.dbr` format, SPICE import/export, KiCad import, automatic layout |
| `dunbrec-sim` | Simulation engine: MNA, Newton-Raphson, DC/sweep/AC/transient, device models |
| `dunbrec-mcu` | ATmega328P emulator (AVR core and peripherals), Intel HEX, a small assembler |
| `dunbrec-instruments` | Oscilloscope and multimeter signal processing |
| `dunbrec-app` | egui/eframe desktop application |

## Checks

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
python tools/make_icon.py   # regenerate assets/dunbrec.png and .ico
```

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
14. 0.4.0: Settings, the assistant (Ollama or Claude) with approved edits, pop-out instruments,
    text size, message boxes for every question, Code panel tabs, tidier project behaviour ✅
15. 0.5.0: a Projects screen, several circuits open at once with their source files, build and
    run that save first, easier schematic editing, larger menus and toolbar, coloured sketches ✅
