# Dunbrec

A desktop electronics workbench written in Rust: draw or import a schematic, simulate it, and watch any node live with virtual instruments (multimeter, oscilloscope, …).

v1 targets analog circuits: discrete components, sources, diodes and transistors. The simulation core is designed for later mixed-signal work: digital logic and microcontrollers in the same circuit.

## Workspace

| Crate | Purpose | Status |
|---|---|---|
| `dunbrec-core` | Schematic data model, units, device models, netlister | Phase 1 ✅ |
| `dunbrec-io` | `.dbr` native format; SPICE/KiCad import later | `.dbr` ✅ |
| `dunbrec-sim` | Own MNA simulation engine with a mixed-signal-ready scheduler | DC + transient, diode/BJT/MOSFET ✅ |
| `dunbrec-instruments` | Virtual instruments | Phase 6 |
| `dunbrec-app` | egui/eframe desktop application | Phase 5 |

## Roadmap

1. Foundations: data model, units, netlister, `.dbr` save/load ✅
2. Linear DC solver (MNA) ✅
3. Scheduler + transient analysis ✅
4. Nonlinear devices: diode, BJT, MOSFET ✅ (ngspice cross-check and junction capacitances still to do)
5. Schematic editor
6. Live simulation + instruments
7. SPICE / KiCad import, SPICE export
8. DC sweep, AC analysis, Bode plotter, curve tracer
9. Polish + v1 release
10. Digital logic (post-v1)
11. Microcontrollers (post-v1)

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```
