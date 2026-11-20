# Dunbrec

A desktop electronics workbench written in Rust: draw or import a schematic, simulate it, and
watch any node live with virtual instruments — oscilloscope, multimeter, function generator,
Bode plotter and curve tracer.

v1 covers analog circuits: discrete components, sources, diodes and transistors. The simulation
core is designed for later mixed-signal work: digital logic and microcontrollers in the same
circuit.

## Installing

Build the Windows installer (needs the WiX Toolset: `dotnet tool install --global wix`):

```powershell
powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1 -Manufacturer "Your name"
```

This produces `target\installer\Dunbrec-<version>-x64.msi`, which installs Dunbrec to Program
Files, adds a Start-menu shortcut and opens `.dbr` files with Dunbrec. Or run from source:

```sh
cargo run --release -p dunbrec-app                     # empty schematic
cargo run --release -p dunbrec-app -- --example 2      # an example (1–6, see File → Open example)
cargo run --release -p dunbrec-app -- my.dbr           # open a file
cargo run --release -p dunbrec-app -- amp.cir          # import a SPICE netlist
cargo run --release -p dunbrec-app -- board.kicad_sch  # import a KiCad schematic
```

## Using Dunbrec

**Drawing.** Pick a part from the palette and click to place it (R rotates, X mirrors, Esc
stops). W draws wires (click to bend; a wire ends by itself on a pin or another wire), L adds
net labels — labels with the same name are connected. Drag to move (attached wires follow),
drag empty space to box-select, Del deletes, Ctrl+C / X / V / D copy, cut, paste and
duplicate, Ctrl+Z / Y undo and redo. Wheel zooms, middle-drag pans. Values accept engineering
notation (`4k7`, `10u`, `2.2M`). Help → Keyboard shortcuts lists everything.

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

**Parts library.** Besides generic parts, the palette's *Real parts* group has the 2N3904 and
2N3906 transistors and the 1N4148 and 1N750 (4.7 V zener) diodes, using the published
manufacturer SPICE models. Each is checked by the test suite against its datasheet limits
(current gain, saturation voltages, transition frequency, forward voltage, leakage, zener
voltage). Any part's model can be switched to a library model in the properties panel.

**Files.** Schematics are saved as `.dbr` (readable text). File → Import reads SPICE netlists
(R, C, L, V/I with DC/SIN/PULSE/PWL, D, Q, M, E/F/G/H, `.model`, `.subckt`) and KiCad 6+
schematics (top sheet; `Device`, `Simulation_SPICE`, `Switch` and `power` libraries plus
`Sim.*` fields). Imported parts are connected with net labels; anything that could not be
imported is listed. File → Export SPICE netlist writes a netlist ngspice can run.

## Workspace

| Crate | Purpose |
|---|---|
| `dunbrec-core` | Schematic data model, units, device models, parts library, netlister |
| `dunbrec-io` | `.dbr` format, SPICE import/export, KiCad import, automatic layout |
| `dunbrec-sim` | Simulation engine: MNA, Newton-Raphson, DC/sweep/AC/transient, device models |
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
10. Digital logic (post-v1)
11. Microcontrollers (post-v1)

Known limits: fixed temperature (27 °C); MOSFETs are level 1 only; KiCad hierarchical sheets,
SPICE `.param` expressions and EXP/SFFM sources are not supported yet.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
python tools/make_icon.py   # regenerate assets/dunbrec.png and .ico
```
