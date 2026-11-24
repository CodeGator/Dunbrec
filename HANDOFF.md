# Dunbrec — handoff

State of the project for whoever picks it up next. User-facing documentation is in
[README.md](README.md); this file covers status, decisions and open work.

*Last updated: 2026-11-24, version 0.5.0 in progress (0.4.0 released: assistant, settings, message boxes, project behaviour). Next task: the multi-document editor, designed in [docs/multi-document-editor.md](docs/multi-document-editor.md), not started.*

## Status

All eleven planned phases are done and committed on `main` (one commit per phase, "Phase N: …"),
plus Phase 12 (integrated circuits) and a fix for probes failing after opening a document.
Version 0.3.0 was released on 2026-11-21 (tag `v0.3.0`, merged to `main`; 0.1.0 and 0.2.0 were
released the same day). Version 0.4.0 was merged to `main` and pushed to `release` on 2026-11-22;
the workflow drafts the GitHub release `v0.4.0`, which is published by hand. Work branches are
named after the application version and merged into `main`; this one is `0.5.0`. A release is cut by merging the branch into `main` and pushing `main:release`, which
drafts the GitHub release (see `.github/workflows/release.yml`).

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
| 0.2.0 | Switches group: SPST, SPDT, DPST, DPDT (`ComponentKind::MultiSwitch`, `SwitchContacts` device) and hold-to-press push buttons (`Switch { momentary }`, `Canvas::push_buttons`) |
| 0.3.0 | Projects (a `.dbrproj` file listing circuits and firmware files), Junction and hops, J key, CD4066, Linear group |
| 0.5.0 (in progress) | Projects screen (nothing else shown without a project); assistant draws new circuits with real wires and can add sine sources; no circuit open until one is opened; a central area that never collapses; closable schematic tab. See "0.5.0" below |
| 0.4.0 | Settings dialog; Assistant (Ollama or Claude); pop-out instruments; text size; every confirmation a message box; Code panel tabs; project behaviour changes. See "0.4.0" below |

Health at handoff: `cargo test --workspace` 426 passed, 0 failed (1 ignored: an emulator speed
benchmark); `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`
clean. Toolchain: Rust 1.99 (edition 2024, MSRV 1.90), Windows 11.

## Building and checking

```sh
cargo run --release -p dunbrec-app -- --example 12   # examples 1–15, see File → Open example
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
| `dunbrec-app` | eframe app; `live.rs` runs the transient on a worker thread. The 0.4.0 modules are listed under "0.4.0" below |

UI layout (`dock.rs`, `PaneViewer` in `app.rs`):

- The schematic, palette, properties, checks and values are `Pane` tabs in an
  `egui_dock::DockState`. The menu bar, run toolbar and status bar are fixed. `dock.rs` has the
  pane list, `default_layout`, `open` / `close` placement and `sanitize`; `PaneViewer` (in
  `app.rs`) draws each pane from borrowed app state.
- **Instruments never dock** (the user's decision): each is a floating window
  (`Bench::scope_window` / `meter_window` / `logic_window`, `Analyses::show`,
  `SerialMonitor::window`, `Debugger::window`), switched by the `show_*` flags. Since 0.4.0 each
  goes through `float::window`, which draws it as an `egui::Window` inside the main window or,
  after its "Pop out" button, as a real OS window (an immediate viewport) that can leave the main
  window (see "0.4.0"). They briefly were dock tabs in the
  first version of Phase 13. `Pane::Retired` (`#[serde(other)]`) lets layouts saved then still
  load; those tabs are dropped on load.
- In `egui_dock` splits, `fraction` is the share of the *first* child (left or top), which is
  the new panel for `split_left` despite the crate's wording. `dock.rs` has a test on the real
  proportions.
- The layout is saved under `dunbrec.layout.v1` in eframe storage, separate from `Settings`,
  and restored through `sanitize` (retired tabs dropped, duplicates removed; a layout with neither
  the schematic nor the empty central area `Pane::Workspace` means default). Bump the key suffix if panes change in a way old layouts cannot express.

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
  were published under MIT).
- Microcontrollers:
  - Own AVR emulator; simavr was rejected because it is GPL (that reason lapsed when the
    project became GPL-3.0; the in-house emulator stays).
  - ATmega328P first.
  - Firmware is a `.hex` path stored in the schematic (relative to the `.dbr`), reloaded on
    every Run, plus `builtin:<name>` demo programs, a source file's path (`main.asm`,
    `fw/blink.ino`: the file beside the `.dbr`, built on every Run), and the older
    `source:<name>` (text kept inside the schematic).
  - File format: `Schematic.sources` (a map of name to `{ language, text }`) was added with
    `#[serde(default)]`, so older `.dbr` files load unchanged and the version number was not
    bumped. An older Dunbrec opening a file that has sources ignores them.
- The user wants technical options explained in plain terms, with a recommendation, and
  confirmed before they become plans.
- **Readability.** The user has poor eyesight: every confirmation or question is the standard
  message box (`dialog.rs`, large type, one fixed width), never a small inline form. There is
  one text-size setting that scales the whole interface (`Settings.text_size`, applied with
  `set_zoom_factor`).
- **Destructive prompts default to the safe button.** In "Delete from disk" Cancel is
  highlighted, so a stray Enter cannot delete; in the unsaved-changes question Enter presses
  Save (when there is one) or Keep editing, and Escape keeps editing.
- **One unsaved-changes question** everywhere (`dialog::unsaved_changes`): "<name> has unsaved
  changes. Keep editing or discard those changes?" with Save / Discard changes / Keep editing, or
  only the last two where saving first makes no sense (deleting the file).
- **No project is reopened at startup**, so an open project is never a surprise (the old
  `Settings.project` is gone; old settings files still load). A project given on the command
  line still opens.
- Window positions of popped-out instruments are deliberately **not** remembered: a window that
  always reopens at the default place is the escape hatch if one is ever lost. Only the choice of
  "popped out" is kept. Instruments > Dock all instruments is the other way back.
- Don't search the user's personal folders (Desktop, Documents, OneDrive) for files without
  asking; stay in the repo, `projects/` or the scratchpad.
- Don't drive the user's mouse or keyboard for UI testing without asking. Window screenshots
  are fine.

## Open items

1. **ngspice golden cross-check** (from phase 4).
   - Needs ngspice installed.
   - Plan: store reference results for fixture circuits and compare within tolerance: about
     0.1 % for DC, bounded RMS error for transients.
2. **Real Arduino firmware tests.** Done for blink, `analogWrite`, `analogRead`, `Serial.print`
   and `millis` (see `mcu_tests.rs`, "Real Arduino sketches"). They need `arduino-cli` with the
   `arduino:avr` core and skip without it. Not covered: timer interrupts through `attachInterrupt`
   and `Timer1`-style libraries, `tone()`, `pulseIn`, `shiftOut`.
3. **UI verification of phase 11 in the running app.**
   - Covered so far by worker-thread tests and a screenshot of the schematic.
   - Pressing Run and watching the serial monitor in the window has not been done.
4. **Assistant (0.4.0).** Not yet done: adding parts with more than two pins is drawn on labels
   only (no real wires); rewiring (moving a pin to another net) is not a suggestion; controlled
   sources, the CD4066, discrete LEDs and microcontrollers cannot be added; the logic analyser's
   data is not in its context; it has not been run against a real model for long, so the prompt
   may need tuning (small local models are weak at circuit diagnosis). None of the 0.4.0 screens
   was checked by anything but the user looking at them and tests of the logic underneath.
5. **More chips.**
   - Op-amps such as the LM358 and TL072, and a comparator such as the LM393, would reuse the
     composite machinery.
   - A CMOS 555 (TLC555) would need different output levels.
   - The ngspice cross-check (item 1) can now also cover the exported 741 subcircuit.
6. **MCU firmware development** (debugger, assembly text, Arduino C++ builds): phases A and B
   and C (Arduino C++ via arduino-cli) built; D (more chips) is not. See
   [docs/mcu-firmware-development.md](docs/mcu-firmware-development.md).
7. **Not yet supported:**
   - MCU: SPI, I²C, EEPROM, watchdog, serial receive and other chips.
   - Simulation: a fixed temperature of 27 °C, MOSFET level 1 only, and a single logic family.
   - Import: KiCad hierarchical sheets, SPICE `.param` expressions and EXP/SFFM sources.
8. **Projects** (built in 0.3.0; changed in 0.4.0, see "0.4.0"). A project is a *file*,
   `<name>.dbrproj` (JSON, `project::EXTENSION`), listing the project's files relative to its
   own folder (`files`) and any folders to show while empty (`folders`); `version` 1. The
   `.dbr` circuits and the `.asm`/`.ino` sources are ordinary files, and the project view shows
   only what the file lists, so other files in the same folder stay out of it. (The first design
   was a plain folder; the user preferred a file, because the system folder dialog cannot pick a
   folder that does not exist yet and a file lets other files share the folder.)
   - `project.rs` (no UI, tested against a scratch folder): `Project` (`create` writes the
     project file and `Circuit 1.dbr` beside it, or the next free name; `open`, `save`, `reload`,
     `add_files`, `add_folder`, `remove` (list only), `moved`, `refresh` which builds the `Entry`
     tree and marks listed-but-missing files), `rename` (renames on disk, updates the list, then
     rewrites the firmware references in every other listed `.dbr` with `rewrite_refs`; the open
     schematic is updated in memory by the app), `delete` (disk and list), and the source
     plumbing below.
   - **Source files.** A source whose name ends in `.asm`, `.s`, `.ino` or `.cpp`
     (`dunbrec_core::source_language_of`) is a *file*: its key in `Schematic.sources` is its path
     relative to the schematic's folder, with `/`, and a microcontroller's firmware field holds
     the same string. `hydrate` reads the files a schematic refers to when it opens;
     `for_saving` leaves them out of the `.dbr`; `write_sources` writes them on Save (only if the
     text differs; the app then lists new ones in the project); `sync_from_disk` takes up
     outside edits. Sources named without such an extension are the older embedded kind
     (`source:<name>`), still supported; `extract_embedded` moves them into files (the panel's
     *Move to files* button).
   - `dunbrec_sim::load_firmware` accepts `source:<name>`, a path of a source file (from
     `sources` if open, else read from disk beside the schematic) and `.hex`/`builtin:` as before.
   - `project_ui.rs` is the panel (`Pane::Project`, a tab beside the palette): tree and context
     menus on the name, folders, files and empty space. It returns a `Command` that
     `DunbrecApp::run_project_command` carries out; the questions it needs (a name for a new file,
     folder or circuit, a new name, confirming a delete) are message boxes, not inline forms
     (`NamePrompt` in `project_ui.rs`, `DeleteAsk` in `app.rs`). File → New project (Save dialog) /
     Open project / Open recent project / Close project. `dunbrec.exe <file>.dbrproj` opens a
     project, and the installer associates `.dbrproj`. Every change to the project is written to
     the project file at once (there is no project "Save").
     Focus returning to the window re-reads the file and the sources (`refresh_project`).
   - **Not done yet:** multi-file C++ builds (a sketch folder with several `.cpp`/`.h`; the
     Arduino build still takes one file's text) and a per-project build cache with the `.hex`
     output in an ignorable subfolder; a source file shared by two open schematics is
     read separately by each; the firmware picker lists only sources already open in the
     schematic (open a file from the Project panel to add it); no "reveal in Explorer"; no
     project-wide search; the installer's `.dbrproj` association is built but not installed or
     tried.

## 0.4.0

New modules in `crates/dunbrec-app/src`:

| Module | Role |
|---|---|
| `settings_ui.rs` | File > Settings dialog, one page per category (Appearance: theme and text size; Assistant: provider, models, context size, Claude key). Choices apply at once |
| `ai.rs` | Providers: Ollama (`/api/chat`, `/api/tags`) and Claude (`/v1/messages`, `/v1/models`). Requests are built and replies parsed by plain functions (tested without a network); the HTTP runs on a worker thread (`ureq`) and streams `Event`s back. `AiSettings` is in `Settings`; the **Claude API key lives in the Windows Credential Manager** (`keyring`, entry `Dunbrec` / `claude-api-key`), never in the settings file |
| `assistant.rs` | The Assistant panel (`Pane::Assistant`, a tab beside Properties): chat, Enter sends (Shift+Enter newline), replies stream in. Builds the system prompt (instructions, the circuit as SPICE, Checks, firmware, live readings) fresh for each question. Draws suggested changes as cards |
| `edits.rs` | Suggested changes and applying them: `Edit::{Set, File, Add, Remove}`. `dunbrec-set` blocks (`R2 = 1k`, `RV1.wiper = 0.5`, `SW1 = closed`), `dunbrec-file NAME` blocks (whole new file, shown as a diff) |
| `part_edits.rs` | `dunbrec-add` (`R5 resistor 10k VOUT 0`: designator, kind, value, then a net per pin in pin order; `0` is ground) and `dunbrec-remove` blocks. The `KINDS` table drives parsing, the pin-order help given to the model, and a test, so they cannot disagree |
| `float.rs` | `float::window`: an instrument as an inline window or, popped out, a real OS window. `dock_all`, `any_popped`; the popped state is egui persisted data keyed `("popped", title)` |
| `dialog.rs` | Message boxes, and `unsaved_changes` (see Decisions) |

How the assistant works (the rules worth keeping):

- **It only talks, plus cards.** Nothing changes until the user presses Apply on a card. Apply is
  one undo step and all-or-nothing; an edit that cannot be made leaves the document untouched.
  Apply is disabled while the answer is still streaming.
- **Context per question:** SPICE export (or a parts list when the circuit has digital parts or
  errors), Checks, firmware sources (each capped), live readings (running simulation snapshot or
  DC point, ignored if older than the last edit; serial tail; oscilloscope channels from the
  captured sweep; multimeter; the paused MCU's registers when the debugger window is open and not
  stale). Readings are built lazily on send (`assistant_readings`, `instrument_sections` in
  `app.rs`).
- **Adding parts** (`part_edits.rs`): a two-pin part between existing nets is *drawn beside them
  with real right-angle wires*: candidate positions and rotations near the closest points on each
  net (pins, wire ends, points along wires, the existing ground symbol) are scored by wire length
  and crossings, rejected if they run through parts or along wires, then the best few are checked
  electrically. Anything else is laid out below the drawing with stubs and net labels
  (`dunbrec_io::layout`); a net the circuit never named first gets a label on one of its pins.
  **Every addition is verified** against the netlist (each new pin on the requested net; no
  existing net joined or split) or nothing changes. Removal deletes the part and the stubs or
  ground drawn for it; wires the user drew stay.
- **Parts it cannot add:** controlled sources, CD4066, microcontrollers, discrete LEDs (there is
  no such part; only the logic LED).

Other 0.4.0 behaviour:

- **Code panel.** One tab per source file of the open circuit (`CodeEditor::tabs`), each with its
  own text and scroll state (ids keyed by file name). "＋ New…" on a saved circuit creates the
  file **on disk at once** (like the Project panel) and lists it in the project if the circuit is
  in it; an Untitled circuit keeps it in memory until first save. The panel says where the file
  is, or that it is not on disk yet.
- **Projects.** Closing a project, or switching to another, also closes the open circuit if the
  project lists it (and with it its source tabs), asking the unsaved-changes question first;
  a circuit the project does not list stays open (`circuit_belongs_to`, `leaves_circuit`).
  Deleting the open circuit's file asks about unsaved changes, then the delete, then closes it
  (blank Untitled circuit).
- **There is always a document.** *Superseded in 0.5.0: see "0.5.0". A `Document` still always
  exists in memory, but `circuit_open` says whether it is shown.*

## 0.5.0 (in progress)

The branch is `0.5.0`. What it adds, and the decisions behind it (all agreed with the user):

**Assistant builds circuits properly.**

- `dunbrec-io/src/tidy.rs`: a new circuit made only of new nets is drawn like a schematic, not on
  labels: supply rails along the top (nets a voltage source's + pin is on, or named VCC/VDD/...),
  a ground symbol on each ground pin, parts in three rows (rail side, middle, ground side) in
  columns that follow the signal (BFS from an input net). A transistor gets the part on its
  collector above and on its emitter below; a resistor to the rail and one to ground that share
  a net form one column (a divider). Wires come from an orthogonal router (Dijkstra over grid
  cells, bend and crossing costs; never through a body; crosses another net only straight across
  a plain wire, since crossing wires do not connect and an end on a wire does). Every net gets a
  label so it keeps its name (`verify` looks nets up by name).
- `part_edits::place_tidy` tries it on a copy and runs the same `verify` as everything else; on
  any failure it falls back to the old label layout (`place_with_labels`). It is only used when
  every new net is new or ground; parts added to existing nets still use `try_wire` or labels.
- New part kind **`vsine`** (value `amplitude@frequency`, e.g. `10m@1k`; sine, no offset) and
  aliases for kind words models tend to use (`voltage`, `battery`, `ac`, `cap`, ...). The prompt now
  tells the model never to claim it changed anything, to list every pin's net, to add new parts
  (including sources) with `dunbrec-add` only, and shows a full common-emitter stage as an example.

**Projects screen** (`projects_screen.rs`). With no project open it is the whole window (no menu,
toolbar, panels or instrument windows): New project, Open project, Recent projects (open, Rename,
Show folder, remove, Clear list) with search, sort (recent, name, folder), a filter (all /
available / file missing) and paging (10 per page; `RECENT_PROJECT_LIMIT` is 50), Settings, Quit.
A circuit file given on the command line waits there with an offer to make a project for it
(`waiting_circuit`; `Project::create_for` lists it where it is, nothing is copied). Renaming
changes only the project file (`project::rename_file`); `Settings::rename_project` keeps the list.
Closing a project closes its circuit (`close_circuit`). File > New is a submenu (circuit, assembly
file, sketch, folder) that creates in the project, and the Project panel's right-click menus have
the same New submenu. The File menu keeps only Close project for projects. The close icon is a
plain "x" (U+00D7): the dingbat "x" had no glyph in egui's fonts.

**No circuit until one is opened.** `circuit_open` is true only after a circuit is opened, made or
imported (`replace`), false after `close_circuit`. Opening or making a project opens no circuit.
While it is false, Save, Export, Edit, Simulate, Instruments, the run toolbar and all shortcuts
except Ctrl+N and Ctrl+O are disabled, and the title shows the project name.

**Central area that never collapses.** egui_dock deletes an area when its last tab goes, and its
neighbours spread into the space. `Pane::Workspace` is an uncloseable, invisible tab (styled in
`tab_style_override`) that holds the central area while no circuit is open;
`dock::sync_documents(dock, circuit_open)` (called from `dock_area`) swaps it with the schematic
and closes the Code tab (its files belong to the circuit). `Pane::floats()` keeps the schematic and
workspace from being dragged into windows. The schematic tab has a close button; middle-click and
the tab's right-click Close come through `TabViewer::on_close`, which asks to close the circuit
(`Pending::CloseCircuit`, with the unsaved-changes question). File > Close circuit, Ctrl+W.

**Still single-document.** One circuit at a time. The next task is the multi-document editor: see
[docs/multi-document-editor.md](docs/multi-document-editor.md) (an `OpenCircuit` struct per open
circuit, tabs per circuit and per source file, one simulation at a time; three proposed decisions
await the user's confirmation).

## Gotchas

- Running the release exe locks `target\release\dunbrec.exe`. Close the app before rebuilding.
- At 2.5 V an ADC input reads 511 or 512 depending on source impedance (pin leakage). Tests
  accept both.
- When scripting file edits from Git Bash, heredocs can turn `\r\n` and `\` line
  continuations into literal characters. Prefer direct file edits for strings with escapes. In a
  Python script, a single `\` at the end of a line inside a normal string is a continuation and
  silently joins the lines, which is how stray runs of spaces got into a Rust string once.
- A long Python edit script inline in a Git Bash heredoc can fail to parse (the shell choked on
  it once, applying nothing); write the script to a file in the scratchpad and run that.
- Rebuilding while the app is running fails (the exe is locked). Close it normally first, so the
  unsaved-changes prompt still appears; do not kill it.
