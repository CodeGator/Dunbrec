# Multi-document editor (design)

Status: **stages 1, 2 and 4 done** (2026-11-26, both rows of tabs); 3 and 5 to do. Written 2026-11-24;
decisions settled with the user 2026-11-26.

## Goal

A central area with two rows of tabs: any number of circuits open at once in the
top row and, in the second row, the active circuit's schematic and source files.
Code is tied to a microcontroller: a source file belongs to the circuit whose
chip runs it, and to no other circuit. Everything belongs to a project (see
"Where we are").

```
┌───────────┬─────────┐
│ [Blinker] │  Fader  │                  circuits
├───────────┴─────────┴──────────────┐
│  Schematic   [U1: blink.ino]       │   the active circuit's files
├────────────────────────────────────┤
│  void setup() { ... }              │
└────────────────────────────────────┘
```

## Decisions

Agreed with the user:

1. **Simulation** runs on the active circuit only. It stops when the user
   switches to another circuit or closes it. Moving between that circuit's own
   second-row tabs does not stop it. Editing values still updates the running
   simulation, as now.
2. **Code is tied to the chip.** A microcontroller's firmware setting is the one
   link between a circuit and a source file; the file belongs to that circuit and
   closes with it. (This replaced an earlier plan for the circuit file to keep its
   own list of sources, which would have been a second link that could disagree.)
3. **No sharing between circuits;** to reuse code in another circuit, copy the
   file. Chips **in the same circuit may share** a file: there is only one open
   copy to edit.
4. **Two rows of tabs** (circuits above, files below). The user wants to *try*
   this, so judge it in the running app; the fallback is one row with tabs named
   "Blinker › blink.ino".
5. **Closing** the project or app with several unsaved circuits asks once for all
   of them.
6. The **Project panel** shows circuit, then chip, then the chip's file.
7. **"Edit code"** on a chip opens its file, and new source files are created from
   a chip.
8. **Files no chip uses** are listed under "Not used". Deleting a chip leaves its
   file on disk (it becomes unused); undo restores the link.
9. **Existing circuits that already use the same file** both still open, and a
   message box offers to make a copy.

Proposed, not yet confirmed:

- Clicking a "Not used" file asks which chip should use it (a tab needs a circuit
  to live in).
- If programs later span several files, each chip gets its own folder.

Dragging a source tab beside its circuit's schematic should work (shown possible
in the stage 0 trial).

### How other tools do it

Looked up 2026-11-26. Proteus (a firmware project per chip, "Edit Firmware" on
the part, a tree with one branch per chip) and Multisim (a code workspace folder
per chip, its path stored in the circuit file; adding an existing file copies it)
tie code to the chip, from their makers' manuals. Thinner sources suggest the same
of Tinkercad (a Code button for the selected board) and SimulIDE (each chip holds
its program's path).

## Where we are (all in branch 0.5.0)

Done and working, so this design builds on it:

- **Projects screen**: the only thing shown while no project is open (new, open,
  recent list with search/sort/filter/paging, rename, clear list, settings, quit).
- **No auto-open**: opening a project opens no circuit.
- **Central area that never collapses**: `Pane::Workspace` is an invisible,
  uncloseable placeholder tab that holds the central area while no circuit is
  open. `dock::sync_documents(dock, circuit_open)` swaps it with the schematic
  (and closes the Code tab) as a circuit opens or closes. Removing the last tab
  of an area makes egui_dock delete the area and spread its neighbours, which is
  why a permanent holder is needed.
- **Closing**: the schematic tab has a close button, middle-click and right-click
  Close all routed through `TabViewer::on_close`; File > Close circuit, Ctrl+W.
  Closing asks about unsaved changes (`Pending::CloseCircuit`).
- `Pane::floats()` stops the schematic and the workspace being dragged into a
  window (that would empty the central area).
- File > New is a submenu (circuit, assembly, sketch, folder) creating files in
  the project; the Project panel's context menus match.

What was **single-document** when this was written (stage 1 has since moved the
per-circuit state into `OpenCircuit`):

- `app.rs` (about 2800 lines) refers to `self.doc` about 80 times and to the
  canvas, live simulation, DC results, netlist and debugger about 90 more.
- One `Document` (circuit + undo/redo + path + generation), one `Canvas`
  (selection, tool state), one `netlist`/`node_map`, one `live` simulation,
  one `Debugger`, one `CodeEditor`.
- Source files live inside the circuit: `Schematic::sources` holds their text;
  the Code panel is one panel with a file picker; an MCU's firmware field holds a
  path relative to the circuit file. A file source is only remembered if a
  microcontroller refers to it (`project::referenced_files`; `for_saving` drops
  the texts), and nothing stops two circuits referring to one file.
- `Pane` is a `Copy` enum with fixed variants (Schematic, Palette, Properties,
  Checks, Values, Code, Project, Assistant, Workspace, Retired), serialised in the
  saved dock layout (`LAYOUT_KEY`).

## Design

### 1. State per open circuit

Introduce `OpenCircuit` (new module `open_circuit.rs`), one per open circuit file:

- `doc: Document` (circuit, undo/redo, path, dirty state)
- `canvas: Canvas` (selection and tool state) and `properties`
- `netlist`, `node_map`, node tags, `dc` results
- `code` editor state and the second-row tab arrangement
- a stable `id: u32`

The app holds `circuits: Vec<OpenCircuit>` and `active: Option<usize>`. Code that
uses `self.doc`, `self.canvas`, ... moves to an `active()` / `active_mut()`
accessor. The side panels (Palette, Properties, Checks, Values, instruments,
Assistant) always follow the active circuit. `circuit_open` becomes
`!circuits.is_empty()`.

The live simulation and its bookkeeping (`live`, `latest`, `live_time`, ...) and
the debugger stay on the app, since only the active circuit can run (decision 1).
Changing `active` stops them.

Make the change mechanically first: move the fields into `OpenCircuit`, keep
exactly one in the list, get all tests green, only then allow several.

**Done in stage 1.** `open_circuit.rs` holds `doc`, `canvas`, `properties`,
`code`, `netlist`, `node_map` and `dc`, with the netlist, DC and editing methods
that used to be on the app. The app has `circuits` and `active: usize` (always one
circuit; `circuit_open` still says whether it is shown).

Left for stage 2, found while doing it:

- `active` has to become optional (or `circuit_open` be replaced by
  `!circuits.is_empty()`); code such as `refresh_netlist` runs even with no
  circuit shown.
- `OpenCircuit` has no `id` yet; add it with the first thing that uses it.
- The clipboard lives in `Canvas`, so it would become per circuit. Move it to the
  app so parts can be pasted from one circuit into another.
- `bench` (probe positions), `analyses` (results), `serial` and `probe_hint` are
  on the app but refer to one circuit: reset them, or keep them per circuit, when
  `active` changes. `replace` resets `bench` today.
- Caches keyed on `Document::generation` are safe across circuits, as generations
  are unique.

### 2. The two rows of tabs

Each circuit is an outer dock tab (`Pane::Circuit(id)`, still `Copy`) whose
contents are a nested `DockArea` with its own `DockState`.

**Trial result (stage 0, 2026-11-26, egui_dock 0.18).** This works. A throwaway
program, driven by simulated pointer events in a headless egui context, showed:
file tabs split beside the schematic and rejoin; each circuit remembers its
arrangement; neither dock's drags leak into the other; typed text survives
switching; it holds at 175 % zoom. What the real build must do:

- Give each nested `DockArea` its own id (`("circuit-dock", id)`), and override
  `TabViewer::id` in both docks so an unsaved dot in a title does not reset the
  tab.
- Turn off the nested dock's close-all and collapse buttons, return `false` from
  its `allowed_in_windows`, and zero the circuit pane's body margin.
- Keep the invisible `Pane::Workspace` tab **permanently** in the central leaf
  (no more swapping). It marks the central area.
- egui_dock cannot restrict one tab's drop targets. So: if the pointer went down
  on a circuit tab (`response.dragged()` does not report it; compare the press
  position with the tab rects from the last frame), pass
  `AllowedSplits::None`; and after drawing, move any circuit tab found outside
  the central leaf back into it. Circuit tabs also return `false` from
  `allowed_in_windows`. Reordering within the row still works.
- There is one active circuit: the one in front in the central leaf.

Cosmetic, to deal with in stage 2: the two rows look alike (make the circuit row
stand out; the user's eyesight is poor), and the invisible tab leaves a small gap
before the first circuit tab.

Not tried: the real schematic canvas and code editor inside the nested dock.

The second row has `Schematic` (cannot be closed) and one tab per open source
file. Closing a source tab only hides it, so it asks nothing: the text stays in
the circuit and is saved with it. Second-row tabs cannot be floated.

Neither row is saved in the layout (only the arrangement of the permanent panels
is). On load, `dock::sanitize` drops document tabs; the project does not reopen
documents unless we add that later. `Pane::Schematic` and `Pane::Code` are
retired; old layouts that name them must still load.

When no circuit is open the central area is empty but keeps its place (the job
`Pane::Workspace` does now).

Tab titles: the circuit name; for a source file, the chips that run it and its
name ("U1: blink.ino", "U1, U2: blink.ino"). A dot marks unsaved changes. Closing a circuit
tab asks only about that circuit. Opening a circuit that is already open focuses
its tab; opening a source file focuses its circuit, then its tab.

### 3. Source files and chips

- No new field in the circuit file: a circuit's files are those its chips name
  (`project::referenced_files`, already read on open by `hydrate`). Text still
  lives in `Schematic::sources`, so saving, the debugger and the simulator keep
  working. Older sources kept inside the circuit belong to it by being there.
- Today opening a file from the Project panel attaches it to the open circuit
  whether or not a chip uses it (`open_source`); that goes.
- A file used by one circuit's chip cannot be chosen for a chip in another. The
  firmware picker offers this circuit's files and unused ones. (`.hex` files and
  `builtin:` programs are not source files, so the rule does not cover them.)
- Changing a chip's file, or deleting the chip, must not lose unsaved text in the
  old file.
- Circuits saved before this may already share a file; they must still open, with
  a message box offering to make a copy (decision 9).
- Knowing which files are used needs every circuit in the project, including
  those not open, so the Project panel reads them from disk and refreshes with the
  project.
- A file belongs only once a chip uses it, so helper or header files have no home
  until multi-file programs are supported. Little is lost today: a chip builds one
  file.
- The Code panel's per-file state (cursor, typing session, built output) is keyed
  by file name inside the `OpenCircuit`. Its New file box, firmware controls and
  *Move to files* button need new homes in the source tab, the chip's properties
  or the Project panel.

### 4. Closing

- Closing the project: ask once, listing every circuit with unsaved changes,
  with Save all, Discard all, Cancel (extend `dialog::unsaved_changes`).
- Closing the app: same list.
- `Pending::CloseCircuit` carries the circuit id.

### 5. Menus and shortcuts

File > New/Open add a tab instead of replacing the circuit. Add Window (or View)
menu listing open circuits, Ctrl+Tab / Ctrl+Shift+Tab to cycle, Ctrl+W closes the
active tab, "Close all", "Close all but this" in the tab context menu. Cycling to
another circuit stops a running simulation like a click does.

## Stages (each stays runnable)

0. **Try the two rows** in egui_dock (section 2). *Done: works.*
1. **Refactor to `OpenCircuit`**, one circuit, no visible change. *Done.*
2. *Done.* **Several circuit tabs**: open adds a tab, tabs switch `active`, per-tab close
   and unsaved prompt, dirty dot, project close with a list, stop the simulation
   on switching. Replace `Pane::Schematic`.
3. **Code and chips**: "Edit code" and new files on the chip, the no-sharing
   rule, the circuit/chip/file tree and "Not used" in the Project panel.
4. *Done, before stage 3 (agreed: names for circuits without names for files
   would only half answer the confusion).* **Second row**: source files as tabs, retire the single Code panel and its
   picker.
5. **Polish**: Ctrl+Tab, window list, close-all variants, maybe reopen the last
   documents when a project opens.

Built differently from section 1: rather than `active: Option<usize>`, the app
keeps a blank, unshown circuit when none is open. The clipboard is carried over on
switching, and instruments, debugger, serial monitor and assistant are kept per
circuit. The no-sharing rule is enforced between *open* circuits (on opening and
on saving elsewhere); the rest of section 3 is still to do.

## Risks and notes

- The refactor in stage 1 is the risk: about 170 references in `app.rs`, plus
  `canvas.rs`, `code_ui.rs`, `properties.rs`, `analyses_ui.rs`, `debugger_ui.rs`
  and the instrument windows that take the document or netlist. Do it in one
  mechanical pass and rely on the compiler.
- Two rows cost height, more so at large text sizes; check at the user's size.
- `egui_dock` deletes an area when its last tab goes; always keep a tab in the
  central area.
- `DockState` has no `PartialEq`; compare layouts through `ron` in tests. A
  layout serialised before the first frame has NaN rectangles; serialise only
  after a frame has run (or use `ron`, as the existing tests do).
- UI wording: say "circuit file" and "project", not file extensions.
- Layout storage key `dunbrec.layout.v1`: adding `Pane` variants is compatible;
  removing `Schematic` or `Code` needs a migration path (treat the old names as
  retired).
