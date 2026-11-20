# Dunbrec

A desktop electronics workbench written in Rust: draw or import a schematic, simulate it, and watch any node live with virtual instruments (multimeter, oscilloscope, …).

v1 targets analog circuits: discrete components, sources, diodes and transistors. The simulation core is designed for later mixed-signal work: digital logic and microcontrollers in the same circuit.

## Workspace

| Crate | Purpose | Status |
|---|---|---|
| `dunbrec-core` | Schematic data model, units, device models, netlister | Phase 1 ✅ |
| `dunbrec-io` | `.dbr` native format; SPICE/KiCad import later | `.dbr` ✅ |
| `dunbrec-sim` | Own MNA simulation engine with a mixed-signal-ready scheduler | DC + transient, diode/BJT/MOSFET ✅ |
| `dunbrec-instruments` | Oscilloscope and multimeter signal processing | ✅ |
| `dunbrec-app` | egui/eframe desktop application | Editor + live simulation ✅ |

## Roadmap

1. Foundations: data model, units, netlister, `.dbr` save/load ✅
2. Linear DC solver (MNA) ✅
3. Scheduler + transient analysis ✅
4. Nonlinear devices: diode, BJT, MOSFET, with junction capacitances and transit times ✅ (ngspice cross-check still to do)
5. Schematic editor ✅ (with live DC operating point)
6. Live simulation + instruments ✅
7. SPICE / KiCad import, SPICE export
8. DC sweep, AC analysis, Bode plotter, curve tracer
9. Polish + v1 release
10. Digital logic (post-v1)
11. Microcontrollers (post-v1)

## Running

```sh
cargo run --release -p dunbrec-app                  # empty schematic
cargo run --release -p dunbrec-app -- --example 2   # common-emitter amplifier
cargo run --release -p dunbrec-app -- my.dbr        # open a file
```

Editing: pick a part from the palette and click to place it (R rotates, X mirrors, Esc stops).
W draws wires (click to bend, ends on a pin or wire), L adds net labels. Drag to move, drag
empty space to box-select, Del deletes, Ctrl+Z / Ctrl+Y undo and redo. Wheel zooms, middle-drag pans.
Values accept engineering notation: `4k7`, `10u`, `2.2M`.

Simulating: press ▶ Run (or Space) to start a live transient simulation paced to the clock;
the Speed box sets how much simulated time passes per second. Open the oscilloscope and
multimeter from the toolbar, click an input's 🔌 Probe button, then click a wire or pin
(or a part, for current). Edits while running take effect immediately: double-click a switch
to flip it, drag a potentiometer's wiper, change a value. Changing the wiring restarts the run.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```
