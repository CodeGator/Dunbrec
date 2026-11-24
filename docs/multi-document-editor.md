# Multi-document editor (design)

Status: **designed, not started.** Written 2026-11-24 to be picked up the next day.

## Goal

A Visual Studio-style central area: any number of circuits and source files open
at once, each in its own tab, with the user clicking back and forth between them.
Everything belongs to a project (see "Where we are").

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

What is still **single-document** (the work of this design):

- `app.rs` (about 2800 lines) refers to `self.doc` about 80 times and to the
  canvas, live simulation, DC results, netlist and debugger about 90 more.
- One `Document` (circuit + undo/redo + path + generation), one `Canvas`
  (selection, tool state), one `netlist`/`node_map`, one `live` simulation,
  one `Debugger`, one `CodeEditor`.
- Source files live inside the circuit: `Schematic::sources` holds their text;
  the Code panel is one panel with a file picker; an MCU's firmware field holds a
  path relative to the circuit file.
- `Pane` is a `Copy` enum with fixed variants (Schematic, Palette, Properties,
  Checks, Values, Code, Project, Assistant, Workspace, Retired), serialised in the
  saved dock layout (`LAYOUT_KEY`).

## Design

### 1. State per open circuit

Introduce `OpenCircuit` (new module `open_circuit.rs`), one per open circuit file:

- `doc: Document` (circuit, undo/redo, path, dirty state)
- `canvas: Canvas` (selection and tool state) and `properties`
- `netlist`, `node_map`, node tags, `dc` results
- `live: Option<LiveSim>` and its bookkeeping (`latest`, `live_time`, ...)
- `debugger` and `code` editor state (they belong to one circuit)
- a stable `id: u32`

The app holds `circuits: Vec<OpenCircuit>` and `active: Option<usize>`. Code that
uses `self.doc`, `self.canvas`, ... moves to an `active()` / `active_mut()`
accessor. The side panels (Palette, Properties, Checks, Values, instruments,
Assistant) always follow the active circuit. `circuit_open` becomes
`!circuits.is_empty()`.

Make the change mechanically first: move the fields into `OpenCircuit`, keep
exactly one in the list, get all tests green, only then allow several.

### 2. Tabs

`Pane` gains per-document variants, still `Copy`:

- `Pane::Circuit(u32)` (the circuit's id)
- later `Pane::Source(u32 circuit_id, u32 file_id)`

These are **not** saved in the layout (only the arrangement of the permanent
panels is). On load, `dock::sanitize` drops document tabs; the project does not
reopen documents unless we add that later. `Pane::Schematic` is retired in favour
of `Pane::Circuit(id)`; keep it deserialisable as `Retired`-like for old layouts.

`dock::sync_documents` generalises: the set of document tabs in the central
area must equal the open circuits (and open source files); when that set is empty
the `Workspace` placeholder holds the area. Opening adds a tab next to the
existing document tabs (not a new split); focusing a tab sets `active`.

Tab titles: file name, plus a dot when unsaved. Closing one tab asks only about
that circuit. Opening a file that is already open focuses its tab. Document tabs
stay out of floating windows (`Pane::floats()`), as now.

### 3. Source files as tabs

Replace the single Code panel with one tab per source file. Source text still
lives in the owning circuit's `sources` map, so saving, the debugger and the
simulator keep working unchanged. `CodeEditor` per-file state (cursor, typing
session, built output) is keyed by file name inside the `OpenCircuit`.

A source tab needs a circuit that lists it, and closes when that circuit closes
(see decision 2 below). The existing New file box in the Code panel moves to the
tab strip or the project panel (it is already in File > New).

### 4. Simulation

One live simulation at a time, tied to the circuit it was started on. It keeps
running when the user switches tabs, and the instruments and Values panel show
the *running* circuit's data (with a label saying which). Run/Pause/Stop in the
toolbar act on the active circuit: Run on a different circuit than the running
one stops the old one first.

### 5. Closing

- Closing the project: ask once, listing every circuit with unsaved changes,
  with Save all, Discard all, Cancel (extend `dialog::unsaved_changes`).
- Closing the app: same list.
- `Pending::CloseCircuit` carries the circuit id.

### 6. Menus and shortcuts

File > New/Open add a tab instead of replacing the circuit. Add Window (or View)
menu listing open documents, Ctrl+Tab / Ctrl+Shift+Tab to cycle, Ctrl+W closes the
active tab, "Close all", "Close all but this" in the tab context menu
(`TabViewer::context_menu`).

## Stages (each stays runnable)

1. **Refactor to `OpenCircuit`**, one circuit, no visible change. All tests green.
2. **Several circuit tabs**: open adds a tab, tabs switch `active`, per-tab close
   and unsaved prompt, dirty dot, project close with a list, one-simulation
   rule. Replace `Pane::Schematic`.
3. **Source files as tabs**, retire the single Code panel and its picker.
4. **Polish**: Ctrl+Tab, window list, close-all variants, maybe reopen the last
   documents when a project opens.

## Decisions (proposed; confirm with the user)

1. Simulation: one at a time, tied to its circuit, keeps running across tab
   switches. *(proposed)*
2. Source files: open as tabs but belong to a circuit and close with it, rather
   than standing alone as in Visual Studio. *(proposed; revisit later)*
3. Closing the project or app with several unsaved circuits: one question with
   a list (Save all / Discard all / Cancel). *(proposed)*

## Risks and notes

- The refactor in stage 1 is the risk: about 170 references in `app.rs`, plus
  `canvas.rs`, `code_ui.rs`, `properties.rs`, `analyses_ui.rs`, `debugger_ui.rs`
  and the instrument windows that take the document or netlist. Do it in one
  mechanical pass and rely on the compiler.
- `egui_dock` deletes an area when its last tab goes; always keep a tab (the
  Workspace placeholder) in the central area.
- `DockState` has no `PartialEq`; compare layouts through `ron` in tests. A
  layout serialised before the first frame has NaN rectangles; serialise only
  after a frame has run (or use `ron`, as the existing tests do).
- UI wording: say "circuit file" and "project", not file extensions.
- Layout storage key `dunbrec.layout.v1`: adding `Pane` variants is compatible;
  removing `Schematic` needs a migration path (treat the old name as retired).
