//! The main window: menus, palette, canvas, properties and results.

use std::path::PathBuf;

use dunbrec_core::{
    Diagnostic, Element, Netlist, NodeId, Schematic, SourceLanguage, units::format_value,
};
use dunbrec_sim::Sample;
use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText, ViewportCommand};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{AllowedSplits, DockArea, DockState, TabViewer};

use crate::analyses_ui::{Analyses, Inputs};
use crate::canvas::{Annotation, Canvas, Item, Marker, PartDrag, Tool};
use crate::code_ui::CodeEditor;
use crate::debugger_ui::{Action as DebugAction, Debugger};
use crate::dialog::{self, Dialog, Message, Unsaved};
use crate::dock::{self, LAYOUT_KEY, Pane};
use crate::document::Document;
use crate::examples::examples;
use crate::instruments_ui::{Bench, MeterSignal, Signals, Speed};
use crate::library::{PaletteEntry, palette_groups};
use crate::live::{Command, Event, LiveSim};
use crate::open_circuit::{DcResults, DocTab, NewFile, OpenCircuit, source_title};
use crate::project::{self, CircuitCode, Project};
use crate::project_ui::{self, ProjectPanel};
use crate::properties::{DemoRequest, Properties};
use crate::serial_ui::SerialMonitor;
use crate::settings::{STORAGE_KEY, Settings};

/// Keyboard shortcuts shown under Help.
const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+N / Ctrl+O / Ctrl+S", "New circuit, open, save"),
    ("Ctrl+W", "Close the circuit"),
    (
        "Ctrl+Tab / Ctrl+Shift+Tab",
        "The next or previous open circuit",
    ),
    ("Ctrl+Shift+S", "Save as"),
    ("Ctrl+Z / Ctrl+Y", "Undo, redo"),
    ("Ctrl+C / Ctrl+X / Ctrl+V", "Copy, cut, paste"),
    ("Ctrl+D", "Duplicate the selection"),
    ("Ctrl+A", "Select everything"),
    ("Click a part or wire", "Select it"),
    (
        "Drag",
        "Move the selection; from empty space, select with a box",
    ),
    ("Click a pin", "Start a wire from it"),
    ("Drag a wire corner", "Reshape the wire"),
    (
        "Double-click while wiring",
        "Finish the wire (it also ends on a pin or another wire)",
    ),
    ("Right-click while using a tool", "Stop using it"),
    ("Del", "Delete the selection"),
    ("R / X", "Rotate / mirror (selection or part being placed)"),
    (
        "Right-click a part",
        "Rotate it once (the whole selection, if it is selected)",
    ),
    (
        "Drag a part from the palette",
        "Drop it on the canvas to place it, then click to place more",
    ),
    ("Hold Ctrl", "Wire for as long as the key is held"),
    ("Hold Alt", "Select for as long as the key is held"),
    ("S", "Select tool"),
    ("W", "Wire tool"),
    ("L", "Net label tool"),
    (
        "J",
        "Junction: click to place, joining wires that cross there",
    ),
    ("P", "Probe tool"),
    ("Esc", "Stop the current tool / clear the selection"),
    ("Space", "Run, pause or resume the simulation"),
    ("Double-click a switch", "Flip it (also while simulating)"),
    (
        "Hold a push button",
        "Press it for as long as the mouse is down",
    ),
    ("Mouse wheel", "Scroll the schematic up and down"),
    ("Shift + wheel", "Scroll it left and right"),
    ("Ctrl + wheel", "Zoom (so does a pinch on a touchpad)"),
    ("Middle drag", "Move the drawing"),
];

/// Which project to go to when the user changes projects.
#[derive(Debug, Clone, PartialEq)]
enum ProjectTarget {
    /// An existing project file.
    Open(PathBuf),
    /// A new project, at this file.
    Create(PathBuf),
}

/// Something that may discard unsaved changes, waiting for confirmation.
#[derive(Debug, Clone, PartialEq)]
enum Pending {
    Open,
    OpenPath(PathBuf),
    Example(usize),
    ImportSpice,
    ImportKicad,
    /// Close the project, and with it every open circuit.
    CloseProject,
    /// Close one circuit, by its id (the project stays open).
    CloseCircuit(u32),
    /// Close every open circuit.
    CloseAll,
    /// Close every open circuit but the one with this id.
    CloseOthers(u32),
    /// Leave the open project (and its circuits) for another one.
    SwitchProject(ProjectTarget),
    Quit,
}

/// A file or folder the user asked to delete, waiting for their answer.
#[derive(Debug, Clone)]
struct DeleteAsk {
    path: PathBuf,
    /// Open circuits inside it have unsaved changes: the user is asked about
    /// losing those first.
    discard_first: bool,
    /// It is only to leave the project: nothing is deleted from the disk.
    remove_only: bool,
}

/// A circuit waiting to be opened because it names source files that an open
/// circuit has: two circuits cannot share a file.
struct SharedAsk {
    path: PathBuf,
    /// The circuit as read, with its source files.
    schematic: Schematic,
    /// Source files that could not be read.
    problems: Vec<String>,
    /// Each shared file, and the name of another circuit that uses it.
    shared: Vec<(String, String)>,
    /// None of those circuits is open, so it can also be opened as it is.
    may_share: bool,
}

/// A chip of an open circuit was just given a file another circuit uses.
struct SharedEdit {
    /// The circuit that was changed, by its id.
    circuit: u32,
    /// Each shared file, and the name of another circuit that uses it.
    shared: Vec<(String, String)>,
}

/// A source file waiting for the user to say which chip should run it.
struct AssignAsk {
    file: PathBuf,
    /// The project's microcontrollers: circuit file, chip, and how to
    /// describe it.
    chips: Vec<(PathBuf, dunbrec_core::ComponentId, String)>,
    chosen: usize,
}

/// Which instrument windows are on screen, and the speed chosen: these stay
/// as they are when the user moves to another circuit.
#[derive(Clone, Copy)]
struct Shown {
    instruments: [bool; 9],
    speed: Speed,
}

pub struct DunbrecApp {
    /// The open circuits, each a tab. Never empty: with none open it holds a
    /// blank one that is not shown, and `circuit_open` is false.
    circuits: Vec<OpenCircuit>,
    /// Which of `circuits` the panels show and the commands act on.
    active: usize,
    /// The id the next circuit to be opened gets.
    next_id: u32,
    /// Where the circuit tabs were drawn, to notice one being dragged.
    circuit_tab_rects: Vec<(u32, egui::Rect)>,
    /// The mouse button is down on a circuit tab.
    dragging_circuit: bool,
    /// How many circuits Ctrl+Tab has asked to move on by (back, if
    /// negative) since the last frame.
    cycle_by: isize,
    /// Where the active circuit's own tabs were drawn, for tests to click.
    #[cfg(test)]
    file_tab_rects: Vec<(DocTab, egui::Rect)>,
    /// A circuit that shares a source file, waiting for the user's answer.
    shared_ask: Option<SharedAsk>,
    shared_edit: Option<SharedEdit>,
    assign_ask: Option<AssignAsk>,
    live_dc: bool,
    show_voltages: bool,
    pending: Option<Pending>,
    allow_close: bool,
    label_name: String,
    message: Option<Message>,
    title: String,
    pub(crate) show_about: bool,
    pub(crate) about: crate::about::AboutState,
    show_shortcuts: bool,
    settings: Settings,
    settings_dialog: crate::settings_ui::SettingsDialog,
    /// A project file or folder waiting for the user to confirm deleting it.
    confirm_delete: Option<DeleteAsk>,
    /// Asking for the name of a new project file or the new name of one.
    name_prompt: Option<project_ui::NamePrompt>,
    /// The screen shown while no project is open.
    projects_screen: crate::projects_screen::ProjectsScreen,
    /// A circuit is open (opened, made, or imported). With none open the
    /// schematic is not shown and nothing that acts on a circuit can be done.
    circuit_open: bool,
    /// A circuit file the program was started with that is in no project;
    /// the Projects screen offers to make one for it.
    waiting_circuit: Option<PathBuf>,
    assistant: crate::assistant::Assistant,

    // ---- live simulation and instruments ----
    bench: Bench,
    live: Option<LiveSim>,
    /// Run was pressed and sketches are being built first; whether the
    /// simulation is then to start paused.
    run_after_build: Option<bool>,
    /// Simulated seconds per real second the simulation was last told to run.
    live_speed: f64,
    /// Document generation the running simulation reflects.
    live_generation: u64,
    live_time: f64,
    live_keeping_up: f64,
    /// Why the simulation could not start or stopped.
    live_error: Option<String>,
    /// Short hint after a probe click that did not connect.
    probe_hint: Option<String>,
    latest: Option<Sample>,
    /// Probe signals, keyed by (document generation, probe revision).
    signals: ((u64, u64), Signals),
    /// Bode plotter, curve tracer, DC transfer, function generator.
    analyses: Analyses,
    /// Microcontroller serial output.
    serial: SerialMonitor,
    /// Where each panel is: docked, tabbed or floating.
    dock: DockState<Pane>,
    /// Text in the palette's search box.
    palette_search: String,
    /// The microcontroller debugger, and the netlist generation whose
    /// firmware it has loaded.
    debugger: Debugger,
    debug_loaded: Option<u64>,

    // ---- projects ----
    /// The open project folder.
    project: Option<Project>,
    project_panel: ProjectPanel,
    /// Whether the window had the focus last frame: getting it back
    /// rescans the project and picks up edits made in other programs.
    was_focused: bool,
}

/// True if two netlists have the same nodes and connections, so a running
/// simulation can take the new one's values without restarting.
fn same_structure(a: &Netlist, b: &Netlist) -> bool {
    a.nodes.len() == b.nodes.len()
        && a.elements.len() == b.elements.len()
        && a.elements
            .iter()
            .zip(&b.elements)
            .all(|(x, y)| x.component == y.component && x.nodes == y.nodes)
}

/// Make everything drawn with `style` larger by the factor `by`: what the
/// "Menu and toolbar size" setting does to those two.
fn enlarge(style: &mut egui::Style, by: f32) {
    for font in style.text_styles.values_mut() {
        font.size *= by;
    }
    let spacing = &mut style.spacing;
    spacing.interact_size *= by;
    spacing.button_padding *= by;
    spacing.item_spacing.x *= by;
    spacing.icon_width *= by;
    spacing.icon_width_inner *= by;
    spacing.icon_spacing *= by;
    spacing.combo_width *= by;
    spacing.menu_width *= by;
}

/// Whether the circuit has a microcontroller, which sets the "Auto" speed.
fn has_mcu(netlist: &Netlist) -> bool {
    netlist
        .elements
        .iter()
        .any(|e| matches!(e.kind, dunbrec_core::ComponentKind::Mcu { .. }))
}

/// The sketches the microcontrollers of `netlist` run that have to be built
/// first: each one's name and text.
fn unbuilt_sketches(netlist: &Netlist, dir: Option<&std::path::Path>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = netlist
        .elements
        .iter()
        .filter_map(|e| match &e.kind {
            dunbrec_core::ComponentKind::Mcu { firmware, .. } => {
                dunbrec_sim::unbuilt_sketch(firmware, dir, &netlist.sources)
            }
            _ => None,
        })
        .collect();
    // Chips may share a file.
    out.sort();
    out.dedup();
    out
}

/// Feed one simulation sample to the instruments.
fn feed(bench: &mut Bench, signals: &Signals, s: &Sample) {
    let v = |n: Option<NodeId>| {
        n.and_then(|n| s.voltages.get(n.0).copied())
            .unwrap_or(f64::NAN)
    };
    let scope = [0, 1, 2, 3].map(|ch| v(signals.scope[ch]));
    bench.scope.push(s.time, scope);
    let meter = match signals.meter {
        MeterSignal::None => f64::NAN,
        MeterSignal::Voltage(p, n) => v(Some(p)) - v(Some(n)),
        MeterSignal::Current(i) => s.currents.get(i).copied().unwrap_or(f64::NAN),
    };
    bench.meter.push(s.time, meter);
    bench.logic.push(
        s.time,
        [0, 1, 2, 3, 4, 5, 6, 7].map(|ch| v(signals.logic[ch])),
    );
}

impl DunbrecApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial: Option<(Schematic, Option<PathBuf>)>,
        initial_project: Option<PathBuf>,
    ) -> Self {
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, STORAGE_KEY))
            .unwrap_or_default();
        cc.egui_ctx.set_theme(settings.theme.preference());
        cc.egui_ctx.set_zoom_factor(settings.text_size.factor());
        let mut startup_problems = Vec::new();
        // A circuit file given at start with no project to put it in waits on
        // the Projects screen; nothing of it is opened until it has a project.
        let mut waiting_circuit = None;
        let initial = match initial {
            Some((_, Some(path))) if initial_project.is_none() => {
                waiting_circuit = Some(path);
                None
            }
            other => other,
        };
        let circuit_open = initial.is_some();
        let doc = match initial {
            Some((mut s, path)) => {
                if let Some(dir) = path.as_deref().and_then(|p| p.parent()) {
                    startup_problems = project::hydrate(&mut s, dir);
                }
                let mut d = Document::new(s, None);
                if let Some(p) = path {
                    d.mark_saved(p);
                }
                d
            }
            None => Document::default(),
        };
        let dock = cc
            .storage
            .and_then(|s| eframe::get_value::<DockState<Pane>>(s, LAYOUT_KEY))
            .map_or_else(dock::default_layout, dock::sanitize);
        // No project is open until the user opens one: the last one is not
        // reopened, so an open project is never a surprise.
        let project: Option<Project> = None;
        let mut app = Self {
            project,
            project_panel: ProjectPanel,
            was_focused: true,
            dock,
            palette_search: String::new(),
            debugger: Debugger::default(),
            debug_loaded: None,
            analyses: Analyses::default(),
            serial: SerialMonitor::default(),
            signals: ((u64::MAX, 0), Signals::default()),
            bench: Bench::default(),
            live: None,
            run_after_build: None,
            live_speed: 0.0,
            live_generation: 0,
            live_time: 0.0,
            live_keeping_up: 1.0,
            live_error: None,
            probe_hint: None,
            latest: None,
            circuits: vec![OpenCircuit::new(doc)],
            active: 0,
            next_id: 1,
            circuit_tab_rects: Vec::new(),
            dragging_circuit: false,
            cycle_by: 0,
            #[cfg(test)]
            file_tab_rects: Vec::new(),
            shared_ask: None,
            shared_edit: None,
            assign_ask: None,
            live_dc: true,
            show_voltages: true,
            pending: None,
            allow_close: false,
            label_name: String::new(),
            message: None,
            title: String::new(),
            show_about: false,
            about: Default::default(),
            show_shortcuts: false,
            settings,
            settings_dialog: Default::default(),
            confirm_delete: None,
            name_prompt: None,
            projects_screen: Default::default(),
            circuit_open,
            waiting_circuit,
            assistant: Default::default(),
        };
        if app.project.is_some() {
            dock::open(&mut app.dock, Pane::Project);
        }
        if let Some(file) = initial_project {
            app.open_project(&file);
        }
        if !startup_problems.is_empty() {
            app.message = Some(source_problems(&startup_problems));
        }
        app
    }

    /// The circuit the panels show and the commands act on.
    fn active(&self) -> &OpenCircuit {
        &self.circuits[self.active]
    }

    fn active_mut(&mut self) -> &mut OpenCircuit {
        &mut self.circuits[self.active]
    }

    // ------------------------------------------------------- files ---

    fn index_of(&self, id: u32) -> Option<usize> {
        self.open_ids().iter().position(|&open| open == id)
    }

    /// The ids of the open circuits, in the order they are held.
    fn open_ids(&self) -> Vec<u32> {
        if self.circuit_open {
            self.circuits.iter().map(|c| c.id).collect()
        } else {
            Vec::new()
        }
    }

    /// The open circuits that have unsaved changes.
    fn dirty_circuits(&self) -> Vec<usize> {
        (0..self.open_ids().len())
            .filter(|&i| self.circuits[i].doc.is_dirty())
            .collect()
    }

    /// The circuits whose unsaved changes `action` would throw away. Opening
    /// something adds a tab, so it throws nothing away.
    fn at_risk(&self, action: &Pending) -> Vec<usize> {
        match action {
            Pending::CloseCircuit(id) => self
                .index_of(*id)
                .filter(|&i| self.circuits[i].doc.is_dirty())
                .into_iter()
                .collect(),
            Pending::CloseProject
            | Pending::CloseAll
            | Pending::SwitchProject(_)
            | Pending::Quit => self.dirty_circuits(),
            Pending::CloseOthers(keep) => {
                let mut dirty = self.dirty_circuits();
                dirty.retain(|&i| self.circuits[i].id != *keep);
                dirty
            }
            _ => Vec::new(),
        }
    }

    fn request(&mut self, action: Pending) {
        if self.at_risk(&action).is_empty() {
            self.perform(action);
            return;
        }
        // The circuit asked about is the one to look at.
        if let Pending::CloseCircuit(id) = action
            && let Some(index) = self.index_of(id)
        {
            self.switch_to(index);
        }
        self.pending = Some(action);
    }

    fn perform(&mut self, action: Pending) {
        match action {
            Pending::Open => {
                let picked = rfd::FileDialog::new()
                    .add_filter("Dunbrec schematic", &["dbr"])
                    .pick_file();
                if let Some(path) = picked {
                    self.open_path(path);
                }
            }
            Pending::OpenPath(path) => self.open_path(path),
            Pending::Example(i) => {
                if let Some(ex) = examples().get(i) {
                    let mut circuit = OpenCircuit::new(Document::new((ex.build)(), None));
                    circuit.title = Some(ex.name.to_string());
                    self.add_circuit(circuit);
                }
            }
            Pending::ImportSpice | Pending::ImportKicad => self.import(action),
            Pending::CloseProject => self.close_project(),
            Pending::CloseCircuit(id) => {
                if let Some(index) = self.index_of(id) {
                    self.close_circuit_at(index);
                }
            }
            Pending::CloseAll => self.close_all_circuits(),
            Pending::CloseOthers(keep) => self.close_other_circuits(keep),
            Pending::SwitchProject(to) => {
                self.close_project();
                self.go_to_project(to);
            }
            Pending::Quit => {
                self.allow_close = true;
            }
        }
    }

    /// Import a SPICE netlist or KiCad schematic as a new, unsaved document.
    fn import(&mut self, action: Pending) {
        let (filter, extensions): (&str, &[&str]) = match action {
            Pending::ImportKicad => ("KiCad schematic", &["kicad_sch"]),
            _ => (
                "SPICE netlist",
                &["cir", "sp", "spi", "spice", "net", "ckt", "txt"],
            ),
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter(filter, extensions)
            .pick_file()
        else {
            return;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.message = Some(Message::file_error("Could not read the file", &path, e));
                return;
            }
        };
        let result = match action {
            Pending::ImportKicad => dunbrec_io::kicad::import(&text)
                .map(|imp| (imp.to_schematic(), imp.warnings))
                .map_err(|e| e.to_string()),
            _ => dunbrec_io::spice::import(&text)
                .map(|imp| {
                    let mut notes = imp.warnings.clone();
                    if !imp.directives.is_empty() {
                        notes.push(format!(
                            "Analysis commands in the file (not run automatically): {}",
                            imp.directives.join("; ")
                        ));
                    }
                    (imp.to_schematic(), notes)
                })
                .map_err(|e| e.to_string()),
        };
        match result {
            Ok((schematic, notes)) => {
                let mut doc = Document::new(schematic, None);
                // A fresh import is unsaved work.
                doc.checkpoint();
                let mut circuit = OpenCircuit::new(doc);
                circuit.title = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                self.add_circuit(circuit);
                let name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let mut message = Message::info(
                    "Import complete",
                    format!(
                        "Imported “{name}”. Parts are connected by net labels; rearrange \
                         them as you like."
                    ),
                );
                if !notes.is_empty() {
                    let mut detail = String::new();
                    for n in notes.iter().take(30) {
                        detail.push_str(&format!("• {n}\n"));
                    }
                    if notes.len() > 30 {
                        detail.push_str(&format!("… and {} more\n", notes.len() - 30));
                    }
                    // Caveats from the conversion deserve a second look.
                    message.kind = dialog::Kind::Warning;
                    message = message.with_detail(detail);
                }
                self.message = Some(message);
            }
            Err(e) => {
                self.message = Some(Message::file_error("Could not import the file", &path, e))
            }
        }
    }

    fn export_spice(&mut self) {
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("SPICE netlist", &["cir"])
            .set_file_name(format!(
                "{}.cir",
                self.active().doc.display_name().trim_end_matches(".dbr")
            ))
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("cir");
        }
        let title = self.active().name();
        let result = dunbrec_io::spice_export::export(self.active().doc.schematic(), &title, &[])
            .map_err(|_| "Fix the problems listed under Checks first.".to_string())
            .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
        if let Err(e) = result {
            self.message = Some(Message::file_error("Could not export the file", &path, e));
        }
    }

    /// Open the circuit file at `path` in a tab, or go to its tab if it is
    /// open already.
    fn open_path(&mut self, path: PathBuf) {
        if let Some(index) = self.circuit_with_file(&path) {
            self.switch_to(index);
            return;
        }
        match dunbrec_io::dbr::load(&path) {
            Ok(mut schematic) => {
                self.settings.remember(&path);
                // The firmware source files it uses live beside it.
                let problems = path
                    .parent()
                    .map(|dir| project::hydrate(&mut schematic, dir))
                    .unwrap_or_default();
                let dir = path.parent().unwrap_or(std::path::Path::new(""));
                // Files an open circuit has cannot be shared; those that only
                // a closed one uses can be, though a copy is better.
                let mut shared = self.shared_sources(&schematic, dir, None);
                let may_share = shared.is_empty();
                if may_share {
                    shared = project::code_of(&schematic, &path)
                        .uses
                        .iter()
                        .filter_map(|u| {
                            let other = self.other_user(&u.path, &path)?;
                            Some((u.name.clone(), other))
                        })
                        .collect();
                }
                if shared.is_empty() {
                    let mut doc = Document::new(schematic, None);
                    doc.mark_saved(path);
                    self.add_opened(doc, &problems);
                } else {
                    self.shared_ask = Some(SharedAsk {
                        path,
                        schematic,
                        problems,
                        shared,
                        may_share,
                    });
                }
            }
            Err(e) => {
                self.settings.forget(&path);
                self.message = Some(Message::file_error("Could not open the file", &path, e));
            }
        }
    }

    /// Add a circuit read from its file, saying what could not be read.
    fn add_opened(&mut self, doc: Document, problems: &[String]) {
        self.add_circuit(OpenCircuit::new(doc));
        if !problems.is_empty() {
            self.message = Some(source_problems(problems));
        }
    }

    /// The open circuit whose file is `path`.
    fn circuit_with_file(&self, path: &std::path::Path) -> Option<usize> {
        let wanted = project::normalize(path);
        (0..self.open_ids().len()).find(|&i| {
            self.circuits[i]
                .doc
                .path()
                .is_some_and(|p| project::path_eq(&project::normalize(p), &wanted))
        })
    }

    /// The source files of `schematic`, in the folder `dir`, that an open
    /// circuit (other than `except`) already has, each with that circuit's
    /// name.
    fn shared_sources(
        &self,
        schematic: &Schematic,
        dir: &std::path::Path,
        except: Option<usize>,
    ) -> Vec<(String, String)> {
        let open = self.open_ids().len();
        schematic
            .sources
            .keys()
            .filter(|name| project::is_file_source(name))
            .filter_map(|name| {
                let full = dir.join(name);
                (0..open)
                    .filter(|&i| Some(i) != except)
                    .find(|&i| self.circuits[i].source_at(&full).is_some())
                    .map(|i| (name.clone(), self.circuits[i].name()))
            })
            .collect()
    }

    /// Open a waiting circuit with a copy of its own of each shared file. It
    /// has unsaved changes: saving it keeps the new names.
    fn open_with_copies(&mut self, ask: SharedAsk) {
        let SharedAsk {
            path,
            schematic: mut own,
            problems,
            shared,
            ..
        } = ask;
        let Some(dir) = path.parent().map(|d| d.to_path_buf()) else {
            return;
        };
        let names: Vec<String> = shared.into_iter().map(|(name, _)| name).collect();
        let made = match project::copy_sources(&mut own, &dir, &names) {
            Ok(made) => made,
            Err(e) => {
                self.message = Some(Message::error("Could not copy the source file", e));
                return;
            }
        };
        self.list_in_project(&path, &made);
        let mut doc = Document::new(own, None);
        doc.mark_saved(path);
        // Not an edit to undo (that would share the files again), but the
        // circuit file does not have the new names yet.
        doc.mark_unsaved();
        self.add_opened(doc, &problems);
    }

    // ------------------------------------------ circuits and their code ---

    /// What each of the project's circuits runs: the open ones as they are
    /// now, saved or not, the others as their files say.
    fn project_code(&self) -> Vec<CircuitCode> {
        let Some(project) = &self.project else {
            return Vec::new();
        };
        project
            .code
            .iter()
            .map(|saved| {
                self.circuit_with_file(&saved.circuit)
                    .and_then(|i| self.circuits[i].code())
                    .unwrap_or_else(|| saved.clone())
            })
            .collect()
    }

    /// The name of a circuit other than the one in `circuit` that runs the
    /// source file at `path`.
    fn other_user(&self, path: &std::path::Path, circuit: &std::path::Path) -> Option<String> {
        let elsewhere = |other: &std::path::Path| {
            !project::path_eq(&project::normalize(other), &project::normalize(circuit))
        };
        // Open circuits, listed in the project or not, and then the rest.
        let open = (0..self.open_ids().len())
            .filter_map(|i| self.circuits[i].code())
            .collect::<Vec<_>>();
        open.into_iter()
            .chain(self.project_code())
            .find(|c| elsewhere(&c.circuit) && c.use_of(path).is_some())
            .map(|c| c.name())
    }

    /// List in the project the files its circuits have come to use, so that
    /// its list stays in step with them.
    fn list_used_files(&mut self) {
        let code = self.project_code();
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let new: Vec<PathBuf> = code
            .iter()
            .filter(|c| project.contains(&c.circuit))
            .flat_map(|c| &c.uses)
            .map(|u| u.path.clone())
            .filter(|path| !project.contains(path) && path.is_file())
            .collect();
        if !new.is_empty()
            && let Err(e) = project.add_files(&new)
        {
            self.message = Some(Message::error("Could not update the project file", e));
        }
    }

    /// Look at what the active circuit's chips have come to run: open it, and
    /// ask about any file another circuit uses.
    fn check_new_code(&mut self) {
        if !self.circuit_open {
            return;
        }
        let new = self.active_mut().newly_used();
        if new.is_empty() {
            return;
        }
        let Some(circuit) = self.active().doc.path().map(|p| p.to_path_buf()) else {
            return;
        };
        let shared: Vec<(String, String)> = new
            .iter()
            .filter_map(|u| Some((u.name.clone(), self.other_user(&u.path, &circuit)?)))
            .collect();
        if !shared.is_empty() {
            self.shared_edit = Some(SharedEdit {
                circuit: self.active().id,
                shared,
            });
        }
    }

    /// Give the circuit `edit` is about copies of its own of the shared files.
    fn copy_for_edit(&mut self, edit: SharedEdit) {
        let Some(index) = self.index_of(edit.circuit) else {
            return;
        };
        let circuit = &mut self.circuits[index];
        let (Some(dir), Some(path)) = (
            circuit.firmware_dir(),
            circuit.doc.path().map(|p| p.to_path_buf()),
        ) else {
            return;
        };
        let names: Vec<String> = edit.shared.into_iter().map(|(name, _)| name).collect();
        let mut own = circuit.doc.schematic().clone();
        match project::copy_sources(&mut own, &dir, &names) {
            Ok(made) => {
                circuit.doc.edit(|s| *s = own);
                self.list_in_project(&path, &made);
            }
            Err(e) => self.message = Some(Message::error("Could not copy the source file", e)),
        }
    }

    /// Ask which of the project's microcontrollers should run `file`.
    fn ask_assign(&mut self, file: PathBuf) {
        let active = self
            .circuit_open
            .then(|| self.active().doc.path().map(|p| p.to_path_buf()))
            .flatten();
        let mut chips = Vec::new();
        for code in self.project_code() {
            for chip in &code.chips {
                let runs = if chip.firmware.is_empty() {
                    "nothing".to_string()
                } else {
                    chip.firmware.clone()
                };
                let label = format!("{}: {}  (runs {runs})", code.name(), chip.designator);
                chips.push((code.circuit.clone(), chip.id, label));
            }
        }
        if chips.is_empty() {
            self.message = Some(Message::info(
                "No microcontroller to run it",
                "None of the project's circuits has a microcontroller. Add one from the \
                 palette, then choose this file again under Not used.",
            ));
            return;
        }
        // Start on a chip of the circuit being looked at.
        let chosen = active
            .and_then(|a| chips.iter().position(|c| project::path_eq(&c.0, &a)))
            .unwrap_or(0);
        self.assign_ask = Some(AssignAsk {
            file,
            chips,
            chosen,
        });
    }

    /// Set the chip `chip` of the circuit in `circuit` to run `file`.
    fn assign(
        &mut self,
        file: &std::path::Path,
        circuit: PathBuf,
        chip: dunbrec_core::ComponentId,
    ) {
        self.request(Pending::OpenPath(circuit.clone()));
        let Some(index) = self.circuit_with_file(&circuit) else {
            return;
        };
        self.switch_to(index);
        let Some(dir) = self.firmware_dir() else {
            return;
        };
        let name = project::relative_to(&dir, file);
        // A circuit that no longer uses the file may still have it open, with
        // text not yet saved: the file moves from there.
        let mut held = None;
        for (i, other) in self.circuits.iter_mut().enumerate() {
            if i != index
                && let Some(known) = other.source_at(file)
            {
                held = other.doc.schematic_mut().sources.remove(&known);
            }
        }
        let circuit_now = &mut self.circuits[index];
        let result = match held {
            Some(text) => {
                circuit_now
                    .doc
                    .schematic_mut()
                    .sources
                    .insert(name.clone(), text);
                Ok(())
            }
            None => project::attach(circuit_now.doc.schematic_mut(), &dir, &name),
        };
        if let Err(e) = result {
            self.message = Some(Message::error("Could not open the source file", e));
            return;
        }
        circuit_now.doc.edit(|s| {
            if let Some(c) = s.component_mut(chip)
                && let dunbrec_core::ComponentKind::Mcu { firmware, .. } = &mut c.kind
            {
                firmware.clone_from(&name);
            }
        });
        circuit_now.show_source(&name);
    }

    /// List `files` in the project, if it lists the circuit they belong to.
    fn list_in_project(&mut self, circuit: &std::path::Path, files: &[PathBuf]) {
        if let Some(project) = self.project.as_mut()
            && !files.is_empty()
            && project.contains(circuit)
        {
            if let Err(e) = project.add_files(files) {
                self.message = Some(Message::error("Could not update the project file", e));
            }
            project.refresh();
        }
    }

    // ------------------------------------------------- open circuits ---

    /// Open `circuit` in a tab of its own and make it the active one.
    fn add_circuit(&mut self, mut circuit: OpenCircuit) {
        circuit.id = self.next_id;
        self.next_id += 1;
        if self.circuit_open {
            self.circuits.push(circuit);
            self.switch_to(self.circuits.len() - 1);
            return;
        }
        self.leave_active();
        circuit.canvas.clipboard = self.active().canvas.clipboard.clone();
        self.circuits = vec![circuit];
        self.active = 0;
        self.circuit_open = true;
        self.reset_tools();
        self.show_active_tab();
    }

    /// Make another open circuit the active one. A running simulation stops:
    /// only the active circuit runs.
    fn switch_to(&mut self, index: usize) {
        if index != self.active && index < self.circuits.len() {
            let shown = self.shown_tools();
            self.leave_active();
            // What was copied can be pasted into the other circuit.
            let clipboard = self.active().canvas.clipboard.clone();
            self.swap_tools(self.active);
            self.active = index;
            self.swap_tools(index);
            self.show_tools(shown);
            if clipboard.is_some() {
                let canvas = &mut self.active_mut().canvas;
                canvas.clipboard = clipboard;
                canvas.paste_count = 0;
            }
        }
        self.show_active_tab();
    }

    /// Close an open circuit. Its unsaved changes are lost: ask first (see
    /// [`request`](Self::request)).
    fn close_circuit_at(&mut self, index: usize) {
        let open = self.open_ids().len();
        if index >= open {
            return;
        }
        if open == 1 {
            self.leave_active();
            let mut blank = OpenCircuit::default();
            blank.canvas.clipboard = self.active().canvas.clipboard.clone();
            self.circuits = vec![blank];
            self.active = 0;
            self.circuit_open = false;
            self.reset_tools();
        } else {
            if index == self.active {
                self.switch_to(if index + 1 < open {
                    index + 1
                } else {
                    index - 1
                });
            }
            self.circuits.remove(index);
            if self.active > index {
                self.active -= 1;
            }
        }
        self.show_active_tab();
    }

    fn close_all_circuits(&mut self) {
        while self.circuit_open {
            self.close_circuit_at(self.active);
        }
    }

    /// Close every circuit but `keep`, which becomes the active one.
    fn close_other_circuits(&mut self, keep: u32) {
        let Some(index) = self.index_of(keep) else {
            return;
        };
        self.switch_to(index);
        for id in self.open_ids() {
            if id != keep
                && let Some(index) = self.index_of(id)
            {
                self.close_circuit_at(index);
            }
        }
    }

    /// Make the circuit `step` tabs along from the active one the active
    /// circuit, going round at the ends.
    fn cycle_circuits(&mut self, step: isize) {
        let order = dock::circuit_order(&self.dock);
        let id = self.active().id;
        let Some(at) = order.iter().position(|&o| o == id) else {
            return;
        };
        let to = (at as isize + step).rem_euclid(order.len() as isize) as usize;
        if let Some(index) = self.index_of(order[to]) {
            self.switch_to(index);
        }
    }

    /// Whether a message or question is waiting for the user.
    fn asking(&self) -> bool {
        self.pending.is_some()
            || self.message.is_some()
            || self.confirm_delete.is_some()
            || self.name_prompt.is_some()
            || self.shared_ask.is_some()
            || self.shared_edit.is_some()
            || self.assign_ask.is_some()
            || (self.circuit_open && self.active().canvas.has_label_request())
    }

    /// Put the active circuit's tab in front, adding or removing circuit tabs
    /// as needed.
    fn show_active_tab(&mut self) {
        let ids = self.open_ids();
        dock::sync_documents(&mut self.dock, &ids);
        if self.circuit_open {
            let id = self.active().id;
            dock::bring_to_front(&mut self.dock, Pane::Circuit(id));
        }
    }

    /// Stop what belongs to the active circuit being the active one: the
    /// simulation, what was said about it, and a tool in use.
    fn leave_active(&mut self) {
        self.stop_sim();
        self.live_error = None;
        self.probe_hint = None;
        self.debug_loaded = None;
        self.label_name.clear();
        self.active_mut().canvas.cancel();
    }

    /// Exchange the application's instruments with those a circuit holds.
    fn swap_tools(&mut self, index: usize) {
        let tools = &mut self.circuits[index].tools;
        std::mem::swap(&mut self.bench, &mut tools.bench);
        std::mem::swap(&mut self.analyses, &mut tools.analyses);
        std::mem::swap(&mut self.serial, &mut tools.serial);
        std::mem::swap(&mut self.debugger, &mut tools.debugger);
        std::mem::swap(&mut self.assistant, &mut tools.assistant);
    }

    /// Fresh instruments, for the first circuit to be opened or none.
    fn reset_tools(&mut self) {
        self.bench = Bench::default();
        self.analyses = Analyses::default();
        self.serial = SerialMonitor::default();
        self.debugger = Debugger::default();
        self.assistant = Default::default();
    }

    /// The switches of the instrument windows.
    fn instrument_switches(&mut self) -> [&mut bool; 9] {
        [
            &mut self.bench.show_scope,
            &mut self.bench.show_meter,
            &mut self.bench.show_logic,
            &mut self.analyses.show_fgen,
            &mut self.analyses.show_bode,
            &mut self.analyses.show_transfer,
            &mut self.analyses.show_tracer,
            &mut self.serial.show,
            &mut self.debugger.show,
        ]
    }

    fn shown_tools(&mut self) -> Shown {
        let speed = self.bench.speed;
        Shown {
            instruments: self.instrument_switches().map(|on| *on),
            speed,
        }
    }

    fn show_tools(&mut self, shown: Shown) {
        self.bench.speed = shown.speed;
        for (switch, on) in self
            .instrument_switches()
            .into_iter()
            .zip(shown.instruments)
        {
            *switch = on;
        }
    }

    /// Save the active circuit, asking where if it has no file yet or `ask`.
    fn save(&mut self, ask: bool) -> bool {
        self.save_circuit(self.active, ask)
    }

    fn save_circuit(&mut self, index: usize, ask: bool) -> bool {
        let path = match (ask, self.circuits[index].doc.path()) {
            (false, Some(p)) => Some(p.to_path_buf()),
            _ => {
                // Show which circuit is being named.
                self.switch_to(index);
                rfd::FileDialog::new()
                    .add_filter("Dunbrec schematic", &["dbr"])
                    .set_file_name(format!("{}.dbr", self.circuits[index].name()))
                    .save_file()
            }
        };
        let Some(mut path) = path else { return false };
        if path.extension().is_none() {
            path.set_extension("dbr");
        }
        if self
            .circuit_with_file(&path)
            .is_some_and(|other| other != index)
        {
            self.message = Some(Message::info(
                "That circuit is open",
                "Another tab has that circuit file open. Close it first, or choose another \
                 name.",
            ));
            return false;
        }
        // Its source files are written beside it, which must not write over
        // those of another open circuit.
        if let Some(dir) = path.parent()
            && let Some((name, owner)) = self
                .shared_sources(self.circuits[index].doc.schematic(), dir, Some(index))
                .first()
        {
            self.message = Some(Message::info(
                "That would share a source file",
                format!(
                    "“{name}” in that folder belongs to {owner}, which is open. Save this \
                     circuit in another folder, or close {owner} first."
                ),
            ));
            return false;
        }
        // Firmware source files are saved as files beside the schematic, and
        // the schematic itself keeps only the names.
        let mut written = Vec::new();
        if let Some(dir) = path.parent() {
            match project::write_sources(self.circuits[index].doc.schematic(), dir) {
                Ok(names) => written = names.into_iter().map(|n| dir.join(n)).collect(),
                Err(e) => {
                    self.message = Some(Message::error("Could not save the source files", e));
                    return false;
                }
            }
        }
        match dunbrec_io::dbr::save(
            &project::for_saving(self.circuits[index].doc.schematic()),
            &path,
        ) {
            Ok(()) => {
                self.settings.remember(&path);
                self.circuits[index].doc.mark_saved(path.clone());
                // New source files of a schematic in the project are the project's.
                if let Some(p) = &mut self.project {
                    if p.contains(&path) && !written.is_empty() {
                        let _ = p.add_files(&written);
                    }
                    p.refresh();
                }
                true
            }
            Err(e) => {
                self.message = Some(Message::file_error("Could not save the file", &path, e));
                false
            }
        }
    }

    // ------------------------------------------------------ projects ---

    /// Make the project file `file` the open project and show its panel. No
    /// circuit is opened: the user picks one from the panel.
    fn open_project(&mut self, file: &std::path::Path) {
        match Project::open(file) {
            Ok(p) => self.show_project(p),
            Err(e) => {
                self.settings.forget_project(file);
                self.message = Some(Message::error("Could not open the project", e));
            }
        }
    }

    /// Close the project, and with it every open circuit (the user was asked
    /// about unsaved changes first): with no project open only the Projects
    /// screen is shown.
    fn close_project(&mut self) {
        self.project = None;
        self.name_prompt = None;
        self.shared_ask = None;
        self.shared_edit = None;
        self.assign_ask = None;
        self.close_all_circuits();
    }

    fn show_project(&mut self, project: Project) {
        self.settings.remember_project(&project.file);
        self.project = Some(project);
        dock::open(&mut self.dock, Pane::Project);
        if let Some(at) = self.dock.find_tab(&Pane::Project) {
            self.dock.set_active_tab(at);
        }
    }

    fn pick_and_open_project(&mut self) {
        if let Some(file) = rfd::FileDialog::new()
            .set_title("Open project")
            .add_filter("Dunbrec project", &[project::EXTENSION])
            .pick_file()
        {
            self.switch_project(ProjectTarget::Open(file));
        }
    }

    /// Make a project: the usual Save dialog (which can make a folder, or
    /// pick any existing one) names the project file; a first circuit is made
    /// beside it and both are opened.
    fn new_project(&mut self) {
        let Some(file) = rfd::FileDialog::new()
            .set_title("New project")
            .add_filter("Dunbrec project", &[project::EXTENSION])
            .set_file_name("My project")
            .save_file()
        else {
            return;
        };
        self.switch_project(ProjectTarget::Create(file));
    }

    /// Make a circuit in the open project: ask for its name, then list it and
    /// open it.
    fn new_circuit(&mut self) {
        if let Some(project) = &self.project {
            let dir = project.root.clone();
            self.run_project_command(project_ui::Command::AskCircuit(dir));
        }
    }

    /// Make a project around the circuit file `circuit`: the usual Save
    /// dialog names the project file (beside the circuit file to begin with).
    /// The circuit file stays where it is and is listed by its path.
    fn new_project_for(&mut self, circuit: &std::path::Path) {
        let stem = circuit
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "My project".into());
        let mut dialog = rfd::FileDialog::new()
            .set_title(format!("New project for “{stem}”"))
            .add_filter("Dunbrec project", &[project::EXTENSION])
            .set_file_name(stem);
        if let Some(dir) = circuit.parent() {
            dialog = dialog.set_directory(dir);
        }
        let Some(file) = dialog.save_file() else {
            return;
        };
        match Project::create_for(&file, circuit) {
            Ok(project) => {
                self.waiting_circuit = None;
                self.show_project(project);
                self.request(Pending::OpenPath(circuit.to_path_buf()));
            }
            Err(e) => self.message = Some(Message::error("Could not make the project", e)),
        }
    }

    /// What the user chose on the Projects screen.
    fn run_screen_command(&mut self, command: crate::projects_screen::Command) {
        use crate::projects_screen::Command as S;
        match command {
            S::NewProject => self.new_project(),
            S::OpenProject => self.pick_and_open_project(),
            S::OpenRecent(file) => self.open_project(&file),
            S::Forget(file) => self.settings.forget_project(&file),
            S::ClearRecent => self.settings.clear_recent_projects(),
            S::Rename { from, to } => {
                let name = to
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match project::rename_file(&from, &name) {
                    Ok(new) => self.settings.rename_project(&from, &new),
                    Err(e) => self.message = Some(Message::error("Could not rename it", e)),
                }
            }
            S::ShowFolder(file) => {
                if let Err(e) = crate::projects_screen::show_in_folder(&file) {
                    self.message = Some(Message::error("Could not show the folder", e));
                }
            }
            S::Settings => self.settings_dialog.open(),
            // No project is open, so nothing can be unsaved.
            S::Quit => self.allow_close = true,
            S::ProjectForWaiting => {
                if let Some(circuit) = self.waiting_circuit.clone() {
                    self.new_project_for(&circuit);
                }
            }
            S::DismissWaiting => self.waiting_circuit = None,
        }
    }

    /// Go to another project. The circuits that are open go with the project
    /// that is open now; if any has unsaved changes the user is asked about
    /// them first.
    fn switch_project(&mut self, to: ProjectTarget) {
        let target = match &to {
            ProjectTarget::Open(file) | ProjectTarget::Create(file) => file,
        };
        let leaving = self
            .project
            .as_ref()
            .is_some_and(|p| !project::path_eq(&p.file, target));
        if leaving {
            self.request(Pending::SwitchProject(to));
        } else {
            self.go_to_project(to);
        }
    }

    fn go_to_project(&mut self, to: ProjectTarget) {
        match to {
            ProjectTarget::Open(file) => self.open_project(&file),
            ProjectTarget::Create(file) => match Project::create(&file) {
                // The first circuit is made and listed, but it is not opened:
                // no circuit is shown until the user opens one.
                Ok((project, _first)) => self.show_project(project),
                Err(e) => {
                    self.message = Some(Message::error("Could not create the project", e));
                }
            },
        }
    }

    /// Carry out what the Project panel asked for.
    fn run_project_command(&mut self, command: project_ui::Command) {
        use project_ui::Command as C;
        match command {
            C::NewProject => self.new_project(),
            C::OpenProject => self.pick_and_open_project(),
            C::OpenProjectPath(root) => self.switch_project(ProjectTarget::Open(root)),
            // The open circuit goes with the project, so ask first if it has
            // unsaved changes.
            C::CloseProject => self.request(Pending::CloseProject),
            C::OpenSchematic(path) => self.request(Pending::OpenPath(path)),
            C::OpenCircuitSource { circuit, name } => self.open_circuit_source(circuit, &name),
            C::AskAssign(file) => self.ask_assign(file),
            C::CreateForChip {
                circuit,
                chip,
                name,
                what,
            } => self.create_for_chip(circuit, chip, &name, what),
            C::CreateCircuit { dir, name } => match project::new_schematic(&dir, &name) {
                Ok(path) => {
                    // What is made here belongs to the project.
                    if let Some(p) = &mut self.project
                        && let Err(e) = p.add_file(&path)
                    {
                        self.message = Some(Message::error("Could not update the project file", e));
                    }
                    self.request(Pending::OpenPath(path));
                }
                Err(e) => self.message = Some(Message::error("Could not create it", e)),
            },
            C::AddExisting => self.add_existing_files(),
            C::RemoveFromProject(path) => self.ask_remove(path),
            C::Rename { from, to } => self.rename_in_project(&from, &to),
            C::AskCircuit(dir) => {
                self.name_prompt = Some(project_ui::NamePrompt::circuit(&dir));
            }
            C::AskRename(path) => self.name_prompt = Some(project_ui::NamePrompt::rename(&path)),
            C::AskDelete(path) => {
                let discard_first = self
                    .circuits_in(&path)
                    .iter()
                    .any(|&i| self.circuits[i].doc.is_dirty());
                self.confirm_delete = Some(DeleteAsk {
                    path,
                    discard_first,
                    remove_only: false,
                });
            }
            C::Refresh => self.refresh_project(),
            C::MoveSourcesToFiles => self.move_sources_to_files(),
            C::ForgetProject(path) => self.settings.forget_project(&path),
            C::ClearRecentProjects => self.settings.clear_recent_projects(),
        }
    }

    /// Pick files that exist and list them in the project.
    fn add_existing_files(&mut self) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let Some(paths) = rfd::FileDialog::new()
            .set_title("Add files to the project")
            .set_directory(&project.root)
            .pick_files()
        else {
            return;
        };
        if let Err(e) = project.add_files(&paths) {
            self.message = Some(Message::error("Could not update the project file", e));
        }
    }

    /// Make a new source file beside `circuit` and have its chip run it.
    fn create_for_chip(
        &mut self,
        circuit: PathBuf,
        chip: dunbrec_core::ComponentId,
        name: &str,
        what: project_ui::Create,
    ) {
        let kind = match what {
            project_ui::Create::Sketch => project::NewSource::Sketch,
            _ => project::NewSource::Assembly,
        };
        let dir = circuit.parent().unwrap_or(std::path::Path::new(""));
        match project::new_source(dir, name, kind) {
            Ok(file) => {
                self.list_in_project(&circuit, std::slice::from_ref(&file));
                self.assign(&file, circuit, chip);
            }
            Err(e) => self.message = Some(Message::error("Could not create it", e)),
        }
    }

    /// What every circuit runs: those of the project, and any open one that is
    /// not in it.
    fn all_code(&self) -> Vec<CircuitCode> {
        let mut code = self.project_code();
        for index in 0..self.open_ids().len() {
            if let Some(open) = self.circuits[index].code()
                && !code
                    .iter()
                    .any(|c| project::path_eq(&c.circuit, &open.circuit))
            {
                code.push(open);
            }
        }
        code
    }

    /// The chips outside `going` that would be left without their program if
    /// the files `gone` went.
    fn left_without(
        code: &[CircuitCode],
        gone: &[PathBuf],
        going: &std::path::Path,
    ) -> Vec<String> {
        let going = project::normalize(going);
        let lost = |path: &std::path::Path| {
            gone.iter()
                .any(|g| project::starts_with_ci(path, &project::normalize(g)))
        };
        code.iter()
            .filter(|c| !project::starts_with_ci(&c.circuit, &going))
            .flat_map(|c| {
                c.uses
                    .iter()
                    .filter(|u| lost(&u.path))
                    .map(|u| format!("{} in {} uses {}.", u.chips.join(", "), c.name(), u.name))
            })
            .collect()
    }

    /// Take a file, circuit or folder out of the project, asking first if a
    /// chip would be left without its program.
    fn ask_remove(&mut self, path: PathBuf) {
        let code = self.all_code();
        if Self::left_without(&code, std::slice::from_ref(&path), &path).is_empty() {
            self.remove_from_project(&path);
        } else {
            self.confirm_delete = Some(DeleteAsk {
                path,
                discard_first: false,
                remove_only: true,
            });
        }
    }

    /// Take `path` out of the project, and with a circuit the source files
    /// only it uses. Nothing is deleted from the disk.
    fn remove_from_project(&mut self, path: &std::path::Path) {
        let removal = project::removal(&self.all_code(), path);
        self.forget_in_circuits(path);
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let result = std::iter::once(path)
            .chain(removal.extra.iter().map(PathBuf::as_path))
            .try_for_each(|gone| project.remove(gone).map(|_| ()));
        if let Err(e) = result {
            self.message = Some(Message::error("Could not update the project file", e));
        }
        self.refresh_project();
    }

    /// Open the circuit in `circuit` and show the tab of its source `name`.
    fn open_circuit_source(&mut self, circuit: PathBuf, name: &str) {
        self.request(Pending::OpenPath(circuit.clone()));
        if let Some(index) = self.circuit_with_file(&circuit) {
            self.switch_to(index);
            self.active_mut().show_source(name);
        }
    }

    fn rename_in_project(&mut self, from: &std::path::Path, to: &std::path::Path) {
        let open: Vec<PathBuf> = (0..self.open_ids().len())
            .filter_map(|i| self.circuits[i].doc.path().map(|p| p.to_path_buf()))
            .collect();
        let Some(project) = self.project.as_mut() else {
            return;
        };
        if let Err(e) = project::rename(project, from, to, &open) {
            self.message = Some(Message::error("Could not rename it", e));
            return;
        }
        // The open circuits follow in memory: their own files may have moved,
        // and their references to the renamed file change.
        for index in 0..self.open_ids().len() {
            let Some(old) = self.circuits[index].doc.path().map(|p| p.to_path_buf()) else {
                continue;
            };
            let none = std::path::Path::new("");
            let old_dir = old.parent().unwrap_or(none).to_path_buf();
            let new = match project::strip_prefix_ci(&old, from) {
                Some(rest) if rest.as_os_str().is_empty() => to.to_path_buf(),
                Some(rest) => to.join(rest),
                None => old.clone(),
            };
            let new_dir = new.parent().unwrap_or(none).to_path_buf();
            let doc = &mut self.circuits[index].doc;
            let mut probe = doc.schematic().clone();
            if project::rewrite_refs(&mut probe, &old_dir, &new_dir, from, to) > 0 {
                doc.edit(|s| {
                    project::rewrite_refs(s, &old_dir, &new_dir, from, to);
                });
            }
            if new != old {
                doc.set_path(new.clone());
                self.settings.forget(&old);
                self.settings.remember(&new);
            }
        }
        self.refresh_project();
    }

    /// The open circuits whose file is `path` or inside the folder `path`.
    fn circuits_in(&self, path: &std::path::Path) -> Vec<usize> {
        let folder = project::normalize(path);
        (0..self.open_ids().len())
            .filter(|&i| {
                self.circuits[i]
                    .doc
                    .path()
                    .is_some_and(|p| project::starts_with_ci(p, &folder))
            })
            .collect()
    }

    /// The questions before a file or folder is deleted from the disk: first,
    /// if open circuits in it have unsaved changes, whether to lose those;
    /// then whether to delete.
    fn confirm_delete_dialog(&mut self, ctx: &egui::Context, ask: DeleteAsk) {
        let circuit = ask
            .path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("dbr"));
        let name = if circuit {
            ask.path.file_stem()
        } else {
            ask.path.file_name()
        }
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
        if ask.discard_first {
            let names: Vec<String> = self
                .circuits_in(&ask.path)
                .into_iter()
                .filter(|&i| self.circuits[i].doc.is_dirty())
                .map(|i| self.circuits[i].name())
                .collect();
            let answer = match names.as_slice() {
                [] => Some(Unsaved::Discard),
                [name] => dialog::unsaved_changes(ctx, name, false),
                many => dialog::unsaved_list(ctx, many, false),
            };
            match answer {
                Some(Unsaved::Discard) => {
                    self.confirm_delete = Some(DeleteAsk {
                        discard_first: false,
                        ..ask
                    });
                }
                Some(_) => self.confirm_delete = None,
                None => {}
            }
            return;
        }
        let code = self.all_code();
        let removal = project::removal(&code, &ask.path);
        let root = self.project.as_ref().map(|p| p.root.clone());
        let short = |path: &std::path::Path| match &root {
            Some(root) => project::relative_to(root, path),
            None => path.display().to_string(),
        };
        // The source files that go with a circuit, and those that stay.
        let (mut going, mut kept) = (Vec::new(), Vec::new());
        let mut gone = vec![ask.path.clone()];
        if !ask.remove_only {
            gone.extend(removal.extra.iter().cloned());
            going = removal
                .extra
                .iter()
                .filter(|p| p.exists())
                .map(|p| short(p))
                .collect();
            kept = removal
                .kept
                .iter()
                .map(|(p, others)| format!("{}  ({})", short(p), others.join(", ")))
                .collect();
        }
        let users = Self::left_without(&code, &gone, &ask.path);
        let folder = ask.path.is_dir();
        let what = if ask.remove_only {
            format!("Remove “{name}” from the project?")
        } else if folder {
            format!("Delete the folder “{name}” and everything in it?")
        } else if circuit && !going.is_empty() {
            format!("Delete the circuit “{name}” and its source files?")
        } else if circuit {
            format!("Delete the circuit “{name}”?")
        } else {
            format!("Delete “{name}”?")
        };
        let fate = if ask.remove_only {
            "Nothing is deleted from the disk."
        } else if folder || !going.is_empty() {
            "They go to the Recycle Bin, so you can get them back."
        } else {
            "It goes to the Recycle Bin, so you can get it back."
        };
        let buttons: &[&str] = if ask.remove_only {
            &["Remove", "Cancel"]
        } else {
            &["Delete", "Cancel"]
        };
        // Cancel is the highlighted button: Enter must not delete anything.
        let answer = Dialog {
            id: "confirm_delete",
            kind: dialog::Kind::Warning,
            title: if ask.remove_only {
                "Remove from project"
            } else {
                "Delete"
            },
            buttons,
            default: 1,
            cancel: Some(1),
        }
        .show(ctx, |ui| {
            dialog::paragraph(ui, &what);
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    for file in &going {
                        dialog::paragraph(ui, &format!("•  {file}"));
                    }
                    if !kept.is_empty() {
                        ui.add_space(6.0);
                        dialog::paragraph(ui, "Kept, because another circuit also uses it:");
                        for file in &kept {
                            dialog::paragraph(ui, &format!("•  {file}"));
                        }
                    }
                    for user in &users {
                        ui.add_space(4.0);
                        dialog::paragraph(ui, user);
                    }
                });
            if !users.is_empty() {
                ui.add_space(4.0);
                dialog::paragraph(ui, "A chip that uses it is left with no program.");
            }
            ui.add_space(4.0);
            dialog::paragraph(ui, fate);
        });
        match answer {
            Some(0) => {
                self.confirm_delete = None;
                if ask.remove_only {
                    self.remove_from_project(&ask.path);
                } else {
                    self.delete_in_project(&ask.path);
                }
            }
            Some(_) => self.confirm_delete = None,
            None => {}
        }
    }

    /// Delete `path` (to the Recycle Bin), and with a circuit the source
    /// files only it uses.
    fn delete_in_project(&mut self, path: &std::path::Path) {
        // Worked out while the circuits can still be read.
        let removal = project::removal(&self.all_code(), path);
        let Some(project) = self.project.as_mut() else {
            return;
        };
        if let Err(e) = project::delete(project, path) {
            self.message = Some(Message::error("Could not delete it", e));
            return;
        }
        // A circuit that was deleted cannot be edited or simulated: it is
        // closed.
        let doomed: Vec<u32> = self
            .circuits_in(path)
            .into_iter()
            .map(|i| self.circuits[i].id)
            .collect();
        for id in doomed {
            if let Some(index) = self.index_of(id) {
                self.close_circuit_at(index);
            }
        }
        self.forget_in_circuits(path);
        for file in &removal.extra {
            let Some(project) = self.project.as_mut() else {
                break;
            };
            match project::delete(project, file) {
                Ok(()) => self.forget_in_circuits(file),
                Err(e) => self.message = Some(Message::error("Could not delete it", e)),
            }
        }
        self.refresh_project();
    }

    /// Take the deleted source file (or folder of them) `gone` away from the
    /// circuits: in an open one as an edit, to be saved or undone, and in the
    /// others in their files, so that none is left asking for it.
    fn forget_in_circuits(&mut self, gone: &std::path::Path) {
        let going = self.circuits_in(gone);
        for index in (0..self.open_ids().len()).filter(|i| !going.contains(i)) {
            let Some(dir) = self.circuits[index].firmware_dir() else {
                continue;
            };
            let doc = &mut self.circuits[index].doc;
            let mut changed = doc.schematic().clone();
            let chips = project::forget_sources(&mut changed, &dir, gone);
            if chips > 0 {
                doc.edit(|s| *s = changed);
            } else if changed.sources.len() != doc.schematic().sources.len() {
                // Only a tab to close: nothing of the circuit changes.
                *doc.schematic_mut() = changed;
            }
        }
        let closed: Vec<PathBuf> = self
            .project_code()
            .into_iter()
            .filter(|c| self.circuit_with_file(&c.circuit).is_none())
            .filter(|c| !project::starts_with_ci(&c.circuit, &project::normalize(gone)))
            .filter(|c| {
                c.uses
                    .iter()
                    .any(|u| project::starts_with_ci(&u.path, &project::normalize(gone)))
            })
            .map(|c| c.circuit)
            .collect();
        for path in closed {
            let dir = path.parent().unwrap_or(std::path::Path::new(""));
            let result = dunbrec_io::dbr::load(&path).and_then(|mut s| {
                project::forget_sources(&mut s, dir, gone);
                dunbrec_io::dbr::save(&s, &path)
            });
            if let Err(e) = result {
                self.message = Some(Message::file_error(
                    "Could not change the circuit",
                    &path,
                    e,
                ));
            }
        }
    }

    /// Rescan the project folder, and take up edits made to the open
    /// circuits' source files elsewhere (except in a circuit with unsaved
    /// changes, which win).
    fn refresh_project(&mut self) {
        if let Some(p) = &mut self.project
            && let Err(e) = p.reload()
        {
            // Keep what is open; say why it could not be read again.
            p.refresh();
            self.message = Some(Message::error("Could not read the project file", e));
        }
        for (index, circuit) in self.circuits.iter_mut().enumerate() {
            if !circuit.doc.is_dirty()
                && let Some(dir) = circuit.firmware_dir()
                && !project::sync_from_disk(circuit.doc.schematic_mut(), &dir).is_empty()
                && index == self.active
            {
                // The debugger lists the new text.
                self.debug_loaded = None;
            }
        }
    }

    /// Move the sources an older version kept inside the schematic out into
    /// files beside it.
    fn move_sources_to_files(&mut self) {
        let Some(dir) = self.firmware_dir() else {
            self.message = Some(Message::info(
                "Save the schematic first",
                "The source files are written beside the schematic, so it needs a place on \
                 the disk first. Save it (Ctrl+S) and try again.",
            ));
            return;
        };
        let mut copy = self.active().doc.schematic().clone();
        match project::extract_embedded(&mut copy, &dir) {
            Ok(moved) if moved.is_empty() => {}
            Ok(moved) => {
                self.active_mut().doc.edit(|s| *s = copy);
                if let Some(p) = &mut self.project
                    && self.circuits[self.active]
                        .doc
                        .path()
                        .is_some_and(|d| p.contains(d))
                {
                    let files: Vec<PathBuf> = moved.iter().map(|(_, f)| dir.join(f)).collect();
                    let _ = p.add_files(&files);
                }
                let list: Vec<String> = moved
                    .iter()
                    .map(|(old, file)| format!("• {old} → {file}"))
                    .collect();
                self.message = Some(
                    Message::info(
                        "Sources moved to files",
                        "Each source is now a file beside the schematic. Save the schematic \
                         to keep the new firmware names.",
                    )
                    .with_detail(list.join("\n")),
                );
                self.refresh_project();
            }
            Err(e) => {
                self.message = Some(Message::error("Could not move the sources", e));
            }
        }
    }

    // ------------------------------------------------- simulation ---

    // ---------------------------------------------------- shortcuts ---

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // Ctrl+Tab works while typing too, but not past a question.
        let step = std::mem::take(&mut self.cycle_by);
        if step != 0 && self.circuit_open && !self.asking() {
            self.cycle_circuits(step);
        }
        if ctx.wants_keyboard_input() {
            return;
        }
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let pressed = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));
        if !self.circuit_open {
            if pressed(cmd(Key::O)) {
                self.request(Pending::Open);
            }
            if pressed(cmd(Key::N)) {
                self.new_circuit();
            }
            return;
        }
        if pressed(cmd(Key::W)) {
            let id = self.active().id;
            self.request(Pending::CloseCircuit(id));
        }
        if pressed(cmd_shift(Key::Z)) || pressed(cmd(Key::Y)) {
            self.active_mut().doc.redo();
        }
        if pressed(cmd(Key::Z)) {
            self.active_mut().doc.undo();
        }
        if pressed(cmd_shift(Key::S)) {
            self.save(true);
        }
        if pressed(cmd(Key::S)) {
            self.save(false);
        }
        if pressed(cmd(Key::O)) {
            self.request(Pending::Open);
        }
        if pressed(cmd(Key::N)) {
            self.new_circuit();
        }
        if pressed(cmd(Key::C)) {
            self.active_mut().copy();
        }
        if pressed(cmd(Key::X)) {
            self.active_mut().copy();
            self.active_mut().delete_selection();
        }
        if pressed(cmd(Key::V)) {
            self.active_mut().paste();
        }
        if pressed(cmd(Key::D)) {
            self.active_mut().copy();
            self.active_mut().paste();
        }
        if pressed(cmd(Key::A)) {
            self.active_mut().select_all();
        }
        let plain = |key| ctx.input(|i| i.key_pressed(key) && i.modifiers.is_none());
        if plain(Key::W) {
            self.active_mut().canvas.tool = Tool::Wire;
        }
        if plain(Key::L) {
            self.active_mut().canvas.tool = Tool::Label;
        }
        if plain(Key::P) {
            self.active_mut().canvas.tool = Tool::Probe;
        }
        if plain(Key::J) {
            self.active_mut().canvas.tool = junction_tool();
        }
        if plain(Key::Space) {
            self.toggle_run();
        }
        if plain(Key::S) {
            self.active_mut().canvas.cancel();
        }
    }

    /// Logic LEDs whose input is high: from the running simulation, or
    /// from the DC operating point when stopped.
    fn lit_leds(&self) -> std::collections::HashSet<dunbrec_core::ComponentId> {
        let voltages: Option<&[f64]> = match (&self.live, &self.latest, &self.active().dc) {
            (Some(_), Some(sample), _)
                if self.live_generation == self.active().doc.generation() =>
            {
                Some(&sample.voltages)
            }
            (
                None,
                _,
                Some(DcResults {
                    outcome: Ok(t),
                    generation,
                }),
            ) if *generation == self.active().doc.generation() => Some(&t.node_voltages),
            _ => None,
        };
        let Some(voltages) = voltages else {
            return Default::default();
        };
        self.active()
            .netlist
            .1
            .elements
            .iter()
            .filter(|e| matches!(e.kind, dunbrec_core::ComponentKind::LogicLed))
            .filter(|e| voltages.get(e.nodes[0].0).is_some_and(|v| *v > 2.5))
            .map(|e| e.component)
            .collect()
    }

    // ------------------------------------------------ live simulation ---

    /// Relative firmware paths are relative to the schematic's folder.
    fn firmware_dir(&self) -> Option<PathBuf> {
        self.active().firmware_dir()
    }

    fn start_sim(&mut self) {
        self.start_sim_with(false);
    }

    /// Before the user's own Build or Run: save the active circuit and its
    /// source files, so that what is built is what is on the disk. A circuit
    /// that has never been saved is built as it is, without asking for a name.
    /// False if saving failed, which has been explained.
    fn save_before_build(&mut self) -> bool {
        let doc = &self.active().doc;
        doc.path().is_none() || !doc.is_dirty() || self.save(false)
    }

    /// Run as the user asked (a restart the app makes by itself, after the
    /// circuit is rewired, saves nothing).
    fn run_pressed(&mut self, paused: bool) {
        if self.save_before_build() {
            self.start_sim_with(paused);
        }
    }

    /// The Build button of a sketch.
    fn build_pressed(&mut self, ctx: &egui::Context, name: &str) {
        if !self.save_before_build() {
            return;
        }
        let circuit = self.active_mut();
        if let Some(file) = circuit.doc.schematic().sources.get(name) {
            circuit.code.build_sketch(ctx, name, &file.text);
        }
    }

    /// Start the live simulation, optionally paused at time zero (for the
    /// debugger to step from the first instruction).
    fn start_sim_with(&mut self, paused: bool) {
        self.stop_sim();
        let netlist = self.active().netlist.1.clone();
        if netlist.has_errors() {
            self.live_error = Some("Fix the problems listed under Checks first.".into());
            return;
        }
        // Sketches that have changed are built first; the simulation starts
        // once they all have (see `continue_run`).
        let sketches = unbuilt_sketches(&netlist, self.firmware_dir().as_deref());
        if !sketches.is_empty() {
            for (name, _) in &sketches {
                self.active_mut().code.forget_failure(name);
            }
            self.live_error = None;
            self.run_after_build = Some(paused);
            return;
        }
        self.bench.clear_readings();
        let speed = self.bench.run_speed(has_mcu(&netlist));
        self.live_speed = speed;
        let models = self.active().doc.schematic().models.clone();
        let firmware_dir = self.firmware_dir();
        self.serial.clear();
        // The firmware file is read again for every run, so the debugger
        // lists what this run will execute.
        self.debugger.load(&netlist, firmware_dir.as_deref());
        self.debug_loaded = Some(self.active().netlist.0);
        let debug = self.debugger.setup(&netlist, paused);
        match LiveSim::start_debug(
            netlist,
            models,
            self.bench.max_step(),
            speed,
            firmware_dir,
            debug,
        ) {
            Ok(live) => {
                self.debugger.sim_started();
                self.live = Some(live);
                self.live_generation = self.active().doc.generation();
                self.live_error = None;
                self.live_time = 0.0;
                self.live_keeping_up = 1.0;
            }
            Err(e) => self.live_error = Some(e),
        }
    }

    /// Tell a running simulation when the speed to run at changes: another
    /// choice, another time base, or an instrument opening or closing.
    fn sync_speed(&mut self) {
        let Some(live) = &mut self.live else { return };
        let speed = self.bench.run_speed(has_mcu(&live.netlist));
        if speed != self.live_speed {
            live.send(Command::SetSpeed(speed));
            self.live_speed = speed;
        }
    }

    /// While a run waits for sketches: build the next one, start the
    /// simulation once all are built, or give up on one that fails.
    fn continue_run(&mut self, ctx: &egui::Context) {
        let Some(paused) = self.run_after_build else {
            return;
        };
        let dir = self.firmware_dir();
        let sketches = unbuilt_sketches(&self.active().netlist.1, dir.as_deref());
        let Some((name, text)) = sketches.first() else {
            self.start_sim_with(paused);
            ctx.request_repaint();
            return;
        };
        let code = &mut self.circuits[self.active].code;
        if code.is_building() {
            return;
        }
        let failed = sketches
            .iter()
            .find_map(|(name, text)| Some((name, code.build_failure(name, text)?)));
        match failed {
            Some((name, why)) => {
                self.run_after_build = None;
                self.live_error = Some(format!("Not started: {name} did not build.\n{why}"));
                // Its tab lists the errors.
                self.active_mut().show_source(name);
                self.message = Some(
                    Message::error(
                        "The simulation was not started",
                        format!(
                            "“{name}” did not build. Its tab shows what is wrong: correct \
                             it and press Run again."
                        ),
                    )
                    .with_detail(why),
                );
            }
            None => code.build_sketch(ctx, name, text),
        }
        ctx.request_repaint();
    }

    fn stop_sim(&mut self) {
        self.run_after_build = None;
        self.live = None;
        self.latest = None;
        self.debugger.sim_stopped();
    }

    /// Run, or pause/resume a running simulation.
    fn toggle_run(&mut self) {
        if self.run_after_build.is_some() {
            return;
        }
        match &mut self.live {
            None => self.run_pressed(false),
            Some(live) if live.paused => live.send(Command::Resume),
            Some(live) => live.send(Command::Pause),
        }
    }

    /// Keep probe signals and node lookups in step with the schematic.
    fn refresh_signals(&mut self) {
        let circuit = &mut self.circuits[self.active];
        circuit.refresh_node_map();
        let key = (circuit.doc.generation(), self.bench.probe_revision);
        if self.signals.0 != key {
            let resolved = self.bench.resolve(
                circuit.doc.schematic(),
                &circuit.netlist.1,
                &circuit.node_map.1,
            );
            self.signals = (key, resolved);
        }
    }

    /// Take results from the simulation thread, and pass it any edits.
    fn sync_live(&mut self) {
        let Some(live) = &mut self.live else { return };
        let events = live.poll();
        let mut restart = false;
        let mut error = None;
        for event in events {
            match event {
                Event::Samples(samples) => {
                    for s in samples {
                        feed(&mut self.bench, &self.signals.1, &s);
                        self.latest = Some(s);
                    }
                }
                Event::Status { time, keeping_up } => {
                    self.live_time = time;
                    self.live_keeping_up = keeping_up;
                }
                Event::Serial(bytes) => {
                    for b in bytes {
                        if let Some(e) = live.netlist.elements.get(b.element) {
                            self.serial.push(&e.designator, &[b.byte]);
                        }
                    }
                }
                Event::NeedsRestart => restart = true,
                // A breakpoint or step paused the simulation: the debugger
                // window shows where.
                Event::Halted { halt, snapshot } => {
                    live.paused = true;
                    self.debugger.on_halt(halt, snapshot, &live.mcus);
                }
                Event::McuState { part, snapshot } => {
                    self.debugger.on_state(part, snapshot, &live.mcus);
                }
                Event::Error(e) => error = Some(e),
            }
        }
        self.bench.scope.update();
        if let Some(e) = error {
            self.stop_sim();
            self.live_error = Some(format!("Simulation stopped: {e}"));
            return;
        }

        let circuit = &self.circuits[self.active];
        let generation = circuit.doc.generation();
        if !restart && generation != self.live_generation {
            let new = &circuit.netlist.1;
            if new.has_errors() {
                self.stop_sim();
                self.live_error =
                    Some("Simulation stopped: the circuit now has problems (see Checks).".into());
                return;
            }
            if same_structure(&live.netlist, new) {
                // Same circuit, new values: update it in place.
                let models = &circuit.doc.schematic().models;
                let changed: Vec<(usize, Element)> = new
                    .elements
                    .iter()
                    .enumerate()
                    .filter(|(i, e)| {
                        let old = &live.netlist.elements[*i];
                        old.kind != e.kind
                            || e.kind
                                .model_name()
                                .is_some_and(|m| models.get(m) != live.models.get(m))
                    })
                    .map(|(i, e)| (i, e.clone()))
                    .collect();
                if !changed.is_empty() {
                    live.send(Command::Update(changed, models.clone()));
                }
                live.netlist = new.clone();
                live.models = models.clone();
                self.live_generation = generation;
            } else {
                restart = true;
            }
        }
        if restart {
            self.start_sim();
        }
    }

    /// Fill for the toolbar and status bar that says whether a simulation
    /// is running (green) or paused (amber); `None` when there is none.
    fn run_tint(&self, ctx: &egui::Context) -> Option<Color32> {
        let live = self.live.as_ref()?;
        let dark = ctx.style().visuals.dark_mode;
        Some(match (live.paused, dark) {
            (false, true) => Color32::from_rgb(22, 78, 42),
            (false, false) => Color32::from_rgb(196, 232, 204),
            (true, true) => Color32::from_rgb(88, 66, 18),
            (true, false) => Color32::from_rgb(248, 228, 184),
        })
    }

    /// A panel frame in the run tint, or the usual one when stopped.
    fn run_frame(&self, ctx: &egui::Context) -> egui::Frame {
        let frame = egui::Frame::side_top_panel(&ctx.style());
        match self.run_tint(ctx) {
            Some(fill) => frame.fill(fill),
            None => frame,
        }
    }

    fn run_toolbar(&mut self, ctx: &egui::Context) {
        let frame = self.run_frame(ctx);
        let size = self.settings.bar_size.factor();
        egui::TopBottomPanel::top("run").frame(frame).show(ctx, |ui| {
            enlarge(ui.style_mut(), size);
            ui.add_space(3.0);
            // In a narrow window the rest goes on a second line.
            ui.horizontal_wrapped(|ui| {
                // Whole words and buttons move down; none is broken up.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                if !self.circuit_open {
                    ui.disable();
                }
                match &self.live {
                    None if self.run_after_build.is_some() => {
                        ui.spinner();
                        ui.label("Building…");
                        if ui
                            .button("⏹ Stop")
                            .on_hover_text("Do not start the simulation")
                            .clicked()
                        {
                            self.stop_sim();
                        }
                    }
                    None => {
                        if ui.button("▶ Run").on_hover_text("Save the circuit and start the live simulation (Space)").clicked() {
                            self.run_pressed(false);
                        }
                    }
                    Some(live) => {
                        let label = if live.paused { "▶ Resume" } else { "⏸ Pause" };
                        if ui.button(label).on_hover_text("Space").clicked() {
                            self.toggle_run();
                        }
                        if ui.button("⏹ Stop").clicked() {
                            self.stop_sim();
                        }
                        if ui.button("↺ Restart").clicked() {
                            self.run_pressed(false);
                        }
                    }
                }
                ui.separator();
                ui.label("Speed");
                self.bench.speed_combo(ui);
                if self.live.is_some() {
                    // Fixed-width slots: the text changes every frame, and
                    // a label that grows or shrinks would shove everything
                    // after it sideways.
                    let height = ui.spacing().interact_size.y;
                    ui.allocate_ui_with_layout(
                        egui::vec2(130.0 * size, height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            // The slot shrinks to its contents unless told not to.
                            ui.set_min_size(egui::vec2(130.0 * size, height));
                            ui.label(
                                RichText::new(format!("t = {}", format_value(self.live_time, "s")))
                                    .monospace(),
                            );
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(96.0 * size, height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_size(egui::vec2(96.0 * size, height));
                            if self.live_keeping_up < 0.95 {
                                ui.label(
                                    RichText::new(format!(
                                        "⚠ {:.0}% speed",
                                        self.live_keeping_up * 100.0
                                    ))
                                    .color(Color32::from_rgb(230, 150, 40)),
                                )
                                .on_hover_text(format!(
                                    "Running at {:.0}% of the chosen speed: the circuit takes longer to simulate than the speed allows. Choose a slower speed or a longer time/div.",
                                    self.live_keeping_up * 100.0
                                ));
                            }
                        },
                    );
                }
                ui.separator();
                let probing = self.active().canvas.tool == Tool::Probe;
                if ui.selectable_label(probing, "🔌 Probe   (P)").clicked() {
                    if probing {
                        self.active_mut().canvas.cancel();
                    } else {
                        self.active_mut().canvas.tool = Tool::Probe;
                    }
                }
                ui.toggle_value(&mut self.bench.show_scope, "📈 Oscilloscope");
                ui.toggle_value(&mut self.bench.show_meter, "🔢 Multimeter");
                ui.toggle_value(&mut self.analyses.show_fgen, "〰 Function gen");
                if let Some(e) = self.live_error.clone() {
                    ui.separator();
                    let first = e.lines().next().unwrap_or_default();
                    let short = if first.chars().count() > 70 || e.lines().count() > 1 {
                        format!("{}…", first.chars().take(70).collect::<String>())
                    } else {
                        first.to_string()
                    };
                    let label = ui
                        .add(
                            egui::Label::new(
                                RichText::new(format!("⚠ {short}"))
                                    .color(Color32::from_rgb(225, 70, 70)),
                            )
                            .sense(egui::Sense::click()),
                        )
                        .on_hover_text("Click for the full message")
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if label.clicked() {
                        self.message = Some(Message::error(
                            "The simulation could not run",
                            first.to_string(),
                        )
                        .with_detail(e.lines().skip(1).collect::<Vec<_>>().join("\n")));
                    }
                } else if let Some(h) = &self.probe_hint {
                    ui.separator();
                    ui.label(RichText::new(h).color(Color32::from_rgb(230, 150, 40)));
                }
            });
            ui.add_space(3.0);
        });
    }

    /// The instruments: free-floating windows that never dock.
    fn instrument_windows(&mut self, ctx: &egui::Context) {
        let probing = self.active().canvas.tool == Tool::Probe;
        let circuit = &mut self.circuits[self.active];
        let s = circuit.doc.schematic();
        let netlist = &circuit.netlist.1;
        let map = &circuit.node_map.1;
        let mut probe_clicked = false;
        if self.bench.show_scope {
            let (timebase_changed, clicked) =
                self.bench.scope_window(ctx, s, netlist, map, probing);
            probe_clicked |= clicked;
            if timebase_changed && let Some(live) = &mut self.live {
                live.send(Command::SetMaxStep(self.bench.max_step()));
            }
        }
        if self.bench.show_meter {
            let running = self.live.is_some();
            probe_clicked |= self
                .bench
                .meter_window(ctx, s, netlist, map, probing, running);
        }
        let inputs = Inputs {
            doc: &mut circuit.doc,
            netlist: &circuit.netlist.1,
            map: &circuit.node_map.1,
            bench: &mut self.bench,
            probing,
        };
        probe_clicked |= self.analyses.show(ctx, inputs);
        if self.bench.show_logic {
            let s = circuit.doc.schematic();
            probe_clicked |=
                self.bench
                    .logic_window(ctx, s, &circuit.netlist.1, &circuit.node_map.1, probing);
        }
        self.serial.window(ctx);
        if probe_clicked {
            circuit.canvas.tool = Tool::Probe;
        }

        // The debugger lists the firmware of the circuit as it is now.
        if (self.debugger.show || !circuit.doc.schematic().sources.is_empty())
            && self.debug_loaded != Some(circuit.netlist.0)
        {
            let dir = circuit.firmware_dir();
            self.debugger.load(&circuit.netlist.1, dir.as_deref());
            self.debug_loaded = Some(circuit.netlist.0);
        }
        if self.debugger.window(ctx, &mut self.live) == DebugAction::StartPaused {
            self.run_pressed(true);
        }
    }

    /// What the open instruments show, for the assistant: the oscilloscope's
    /// channels, the multimeter, and the stopped microcontroller.
    fn instrument_sections(&self) -> Vec<(String, Vec<String>)> {
        let mut out = Vec::new();
        let (s, netlist, map) = (
            self.active().doc.schematic(),
            &self.active().netlist.1,
            &self.active().node_map.1,
        );
        if self.bench.show_scope {
            let mut lines = Vec::new();
            for (i, channel) in self.bench.scope.channels.iter().enumerate() {
                let (Some(point), true) = (self.bench.scope_probes[i], channel.enabled) else {
                    continue;
                };
                // The captured screen of the oscilloscope.
                let Some(sweep) = self.bench.scope.sweep() else {
                    continue;
                };
                let Some(stats) = sweep.stats(i) else {
                    continue;
                };
                let net = match map.node_at(s, netlist, point) {
                    Some(n) if n.is_ground() => "0 (ground)".to_string(),
                    Some(n) => netlist.nodes[n.0].name.clone(),
                    None => "(not on a net)".to_string(),
                };
                let frequency = sweep
                    .frequency(i)
                    .map(|f| format!(", frequency {}", format_value(f, "Hz")))
                    .unwrap_or_default();
                lines.push(format!(
                    "Channel {} on net {net}: min {}, max {}, mean {}, rms {}{frequency}",
                    i + 1,
                    format_value(stats.min, "V"),
                    format_value(stats.max, "V"),
                    format_value(stats.mean, "V"),
                    format_value(stats.rms, "V"),
                ));
            }
            if !lines.is_empty() {
                out.push((
                    format!(
                        "Oscilloscope ({} per division, measured over the screen)",
                        format_value(self.bench.scope.time_per_div, "s")
                    ),
                    lines,
                ));
            }
        }
        if self.bench.show_meter
            && let Some(value) = self.bench.meter.reading()
        {
            let mode = self.bench.meter.mode;
            out.push((
                "Multimeter".to_string(),
                vec![format!(
                    "{}: {}",
                    mode.label(),
                    format_value(value, mode.unit())
                )],
            ));
        }
        let paused = self.live.as_ref().is_some_and(|l| l.paused);
        if let Some(lines) = self.debugger.summary(paused) {
            out.push(("Microcontroller (from the debugger)".to_string(), lines));
        }
        out
    }

    /// Draw the dockable panels, with the open circuits in the middle.
    fn dock_area(&mut self, ctx: &egui::Context) {
        // A press on a circuit's tab (as drawn last frame) may become a drag
        // of it.
        let pressed = ctx.input(|i| {
            i.pointer
                .primary_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten()
        });
        if let Some(at) = pressed {
            self.dragging_circuit = self.circuit_tab_rects.iter().any(|(_, r)| r.contains(at));
        }
        self.circuit_tab_rects.clear();
        #[cfg(test)]
        self.file_tab_rects.clear();
        let ids = self.open_ids();
        if dock::sync_documents(&mut self.dock, &ids) {
            self.show_active_tab();
        }
        if self.circuit_open {
            self.active_mut().sync_tabs();
        }
        let project_code = self.project_code();
        let open_files: Vec<PathBuf> = (0..self.open_ids().len())
            .filter_map(|i| self.circuits[i].doc.path().map(|p| p.to_path_buf()))
            .collect();
        let circuit_open = self.circuit_open;
        let names: Vec<(u32, String, bool)> = self
            .circuits
            .iter()
            .map(|c| (c.id, c.name(), c.doc.is_dirty()))
            .collect();
        let active_id = self.active().id;
        let running = self.live.is_some();
        let lit = self.lit_leds();
        let markers = self.bench.markers(self.active().doc.schematic());
        let title = self.active().name();
        let generation = self.active().doc.generation();
        let live_is_current = running && self.live_generation == generation;
        let sections = if dock::is_open(&self.dock, Pane::Assistant) {
            self.instrument_sections()
        } else {
            Vec::new()
        };
        // The panels borrow the parts of the circuit one by one.
        let OpenCircuit {
            doc,
            canvas,
            properties,
            code,
            tabs,
            hidden,
            netlist,
            node_map,
            dc,
            ..
        } = &mut self.circuits[self.active];
        let (netlist, dc) = (&netlist.1, &*dc);
        // Live values while simulating, otherwise the DC point.
        let live_tags: Vec<Annotation>;
        let annotations: &[Annotation] = match (&self.live, &self.latest, dc) {
            _ if !self.show_voltages => &[],
            (Some(_), Some(sample), _) if self.live_generation == generation => {
                live_tags = node_map
                    .2
                    .iter()
                    .filter_map(|place| {
                        sample.voltages.get(place.node.0).map(|v| Annotation {
                            place: *place,
                            text: format_value(*v, "V"),
                        })
                    })
                    .collect();
                &live_tags
            }
            (
                None,
                _,
                Some(DcResults {
                    outcome: Ok(t),
                    generation,
                }),
            ) if *generation == doc.generation() => &t.annotations,
            _ => &[],
        };
        let mut project_command = None;
        let mut closing = Vec::new();
        let mut open_settings = false;
        let readings = || {
            assistant_readings(
                netlist,
                sections.clone(),
                &self.latest,
                live_is_current,
                dc,
                generation,
                &self.serial,
            )
        };
        let mut viewer = PaneViewer {
            project_code: &project_code,
            open_files: &open_files,
            circuit_open,
            circuits: &names,
            active_id,
            tabs,
            hidden,
            closing: &mut closing,
            tab_rects: &mut self.circuit_tab_rects,
            #[cfg(test)]
            file_tab_rects: &mut self.file_tab_rects,
            doc,
            canvas,
            properties,
            code,
            debugger: &mut self.debugger,
            live: &mut self.live,
            palette_search: &mut self.palette_search,
            netlist,
            dc,
            latest: &self.latest,
            readings: &readings,
            running,
            annotations,
            markers: &markers,
            lit: &lit,
            project: self.project.as_ref(),
            recent_projects: &self.settings.recent_projects,
            project_panel: &mut self.project_panel,
            project_command: &mut project_command,
            assistant: &mut self.assistant,
            ai: &self.settings.ai,
            title: &title,
            open_settings: &mut open_settings,
            accent: ctx.style().visuals.selection,
        };
        // There is one active circuit, so circuits are not put side by side.
        let splits = if self.dragging_circuit {
            AllowedSplits::None
        } else {
            AllowedSplits::All
        };
        DockArea::new(&mut self.dock)
            .style(egui_dock::Style::from_egui(ctx.style().as_ref()))
            // Each tab has its own close button. One more that closes a whole
            // group at once only invites doing it by accident.
            .show_leaf_close_all_buttons(false)
            .allowed_splits(splits)
            .show(ctx, &mut viewer);
        if ctx.input(|i| !i.pointer.any_down() && !i.pointer.any_released()) {
            self.dragging_circuit = false;
        }
        // A circuit dragged elsewhere goes back; otherwise the tab the user
        // brought to the front is the active circuit.
        let ids = self.open_ids();
        if dock::sync_documents(&mut self.dock, &ids) {
            self.show_active_tab();
        } else if let Some(index) = dock::front_circuit(&self.dock).and_then(|id| self.index_of(id))
            && index != self.active
        {
            self.switch_to(index);
            ctx.request_repaint();
        }
        if let Some(command) = project_command {
            self.run_project_command(command);
        }
        for action in closing {
            if self.pending.is_none() {
                self.request(action);
            }
        }
        if open_settings {
            self.settings_dialog.open();
        }
        // "Edit code" on a microcontroller's source file shows its tab.
        if let Some(name) = self.active_mut().properties.open_code.take() {
            self.active_mut().show_source(&name);
        }
        if let Some(request) = self.active_mut().properties.demo.take() {
            self.use_demo(ctx, request);
        }
        if let Some(name) = self.active_mut().code.take_build_request() {
            self.build_pressed(ctx, &name);
        }
        if let Some((chip, language)) = self.active_mut().properties.new_file.take() {
            self.new_source_for(chip, language);
        }
    }

    /// Give a chip of the active circuit a new source file: ask for its name,
    /// or, while the circuit has no folder yet, keep it in memory under the
    /// first free one until the circuit is saved.
    fn new_source_for(&mut self, chip: dunbrec_core::ComponentId, language: SourceLanguage) {
        let (what, base, text) = match language {
            SourceLanguage::Cpp => (
                project_ui::Create::Sketch,
                "sketch",
                crate::code_ui::STARTER_SKETCH,
            ),
            SourceLanguage::Asm => (
                project_ui::Create::Assembly,
                "main",
                crate::code_ui::STARTER,
            ),
        };
        let circuit = self.active();
        if let Some(path) = circuit.doc.path() {
            let designator = circuit
                .doc
                .schematic()
                .component(chip)
                .map(|c| c.designator.clone())
                .unwrap_or_default();
            let who = format!("{designator} in {}", circuit.name());
            self.name_prompt = Some(project_ui::NamePrompt::source(path, chip, &who, what));
        } else {
            let made = self.active_mut().add_source(chip, base, language, text);
            self.take_new_file(made);
        }
    }

    /// Make a microcontroller its own copy of a demo program. A sketch is
    /// built straight away, so that it can be run.
    fn use_demo(&mut self, ctx: &egui::Context, request: DemoRequest) {
        let sketch = request.language == SourceLanguage::Cpp;
        // A sketch cannot run until it is built.
        if sketch {
            self.stop_sim();
        }
        let made = self.active_mut().add_demo(request);
        if let Some(file) = self.take_new_file(made)
            && sketch
        {
            self.active_mut()
                .code
                .build_sketch(ctx, &file.name, &file.text);
        }
    }

    /// List a source file just made for the active circuit in the project, or
    /// say why there is none.
    fn take_new_file(&mut self, made: Result<NewFile, String>) -> Option<NewFile> {
        let file = match made {
            Ok(file) => file,
            Err(e) => {
                self.message = Some(Message::error("Could not create the file", e));
                return None;
            }
        };
        if let Some(path) = &file.path
            && let Some(circuit) = self.active().doc.path().map(|p| p.to_path_buf())
        {
            self.list_in_project(&circuit, std::slice::from_ref(path));
        }
        Some(file)
    }

    // ---------------------------------------------------------- UI ---

    fn menu_bar(&mut self, ctx: &egui::Context) {
        let size = self.settings.bar_size.factor();
        // For the bar and, separately, for each menu that drops from it.
        let style = egui::style::StyleModifier::new(move |style| {
            egui::containers::menu::menu_style(style);
            enlarge(style, size);
            // A menu remembers how wide it was, from a smaller size too; its
            // lines must widen it rather than break.
            style.wrap_mode = Some(egui::TextWrapMode::Extend);
        });
        let menus = egui::containers::menu::MenuConfig::new().style(style.clone());
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::MenuBar::new()
                .style(style)
                .config(menus)
                .ui(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        if ui
                            .button("New circuit…   Ctrl+N")
                            .on_hover_text("Make a circuit in the project")
                            .clicked()
                        {
                            self.new_circuit();
                            ui.close();
                        }
                        if ui.button("Open…      Ctrl+O").clicked() {
                            self.request(Pending::Open);
                            ui.close();
                        }
                        ui.add_enabled_ui(!self.settings.recent.is_empty(), |ui| {
                            ui.menu_button("Open recent", |ui| {
                                for path in self.settings.recent.clone() {
                                    let name = path.file_name().map_or_else(
                                        || path.display().to_string(),
                                        |n| n.to_string_lossy().into_owned(),
                                    );
                                    if ui
                                        .button(name)
                                        .on_hover_text(path.display().to_string())
                                        .clicked()
                                    {
                                        self.request(Pending::OpenPath(path));
                                        ui.close();
                                    }
                                }
                                ui.separator();
                                if ui.button("Clear list").clicked() {
                                    self.settings.clear_recent();
                                    ui.close();
                                }
                            });
                        });
                        ui.menu_button("Open example", |ui| {
                            for (i, ex) in examples().iter().enumerate() {
                                if ui.button(ex.name).clicked() {
                                    self.request(Pending::Example(i));
                                    ui.close();
                                }
                            }
                        });
                        ui.separator();
                        if ui
                            .button("Close project")
                            .on_hover_text("Go back to the Projects screen")
                            .clicked()
                        {
                            self.run_project_command(project_ui::Command::CloseProject);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Import SPICE netlist…").clicked() {
                            self.request(Pending::ImportSpice);
                            ui.close();
                        }
                        if ui.button("Import KiCad schematic…").clicked() {
                            self.request(Pending::ImportKicad);
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.circuit_open,
                                egui::Button::new("Export SPICE netlist…"),
                            )
                            .clicked()
                        {
                            self.export_spice();
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                self.circuit_open,
                                egui::Button::new("Close circuit   Ctrl+W"),
                            )
                            .on_hover_text("Close the circuit; the project stays open")
                            .clicked()
                        {
                            let id = self.active().id;
                            self.request(Pending::CloseCircuit(id));
                            ui.close();
                        }
                        if ui
                            .add_enabled(self.circuit_open, egui::Button::new("Save       Ctrl+S"))
                            .clicked()
                        {
                            self.save(false);
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.circuit_open,
                                egui::Button::new("Save as…   Ctrl+Shift+S"),
                            )
                            .clicked()
                        {
                            self.save(true);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Settings…").clicked() {
                            self.settings_dialog.open();
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Quit").clicked() {
                            ctx.send_viewport_cmd(ViewportCommand::Close);
                            ui.close();
                        }
                    });
                    ui.menu_button("Edit", |ui| {
                        if !self.circuit_open {
                            ui.disable();
                        }
                        if ui
                            .add_enabled(
                                self.active().doc.can_undo(),
                                egui::Button::new("Undo   Ctrl+Z"),
                            )
                            .clicked()
                        {
                            self.active_mut().doc.undo();
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.active().doc.can_redo(),
                                egui::Button::new("Redo   Ctrl+Y"),
                            )
                            .clicked()
                        {
                            self.active_mut().doc.redo();
                            ui.close();
                        }
                        ui.separator();
                        let has_selection = !self.active().canvas.selection.is_empty();
                        if ui
                            .add_enabled(has_selection, egui::Button::new("Rotate   R"))
                            .clicked()
                        {
                            self.active_mut().transform_selection(false);
                            ui.close();
                        }
                        if ui
                            .add_enabled(has_selection, egui::Button::new("Mirror   X"))
                            .clicked()
                        {
                            self.active_mut().transform_selection(true);
                            ui.close();
                        }
                        if ui
                            .add_enabled(has_selection, egui::Button::new("Delete   Del"))
                            .clicked()
                        {
                            self.active_mut().delete_selection();
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(has_selection, egui::Button::new("Copy        Ctrl+C"))
                            .clicked()
                        {
                            self.active_mut().copy();
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.active().canvas.clipboard.is_some(),
                                egui::Button::new("Paste       Ctrl+V"),
                            )
                            .clicked()
                        {
                            self.active_mut().paste();
                            ui.close();
                        }
                        if ui
                            .add_enabled(has_selection, egui::Button::new("Duplicate   Ctrl+D"))
                            .clicked()
                        {
                            self.active_mut().copy();
                            self.active_mut().paste();
                            ui.close();
                        }
                    });
                    ui.menu_button("Simulate", |ui| {
                        if !self.circuit_open {
                            ui.disable();
                        }
                        ui.checkbox(&mut self.live_dc, "Live DC operating point");
                        ui.checkbox(&mut self.show_voltages, "Show node voltages on schematic");
                        if ui.button("Recalculate DC now").clicked() {
                            self.active_mut().run_dc();
                            ui.close();
                        }
                    });
                    ui.menu_button("Instruments", |ui| {
                        if !self.circuit_open {
                            ui.disable();
                        }
                        ui.checkbox(&mut self.bench.show_scope, "📈 Oscilloscope");
                        ui.checkbox(&mut self.bench.show_meter, "🔢 Multimeter");
                        ui.checkbox(&mut self.bench.show_logic, "📊 Logic analyser");
                        ui.checkbox(&mut self.analyses.show_fgen, "〰 Function generator");
                        ui.checkbox(&mut self.serial.show, "⌨ Serial monitor");
                        ui.checkbox(&mut self.debugger.show, "🐞 MCU debugger");
                        ui.separator();
                        ui.checkbox(
                            &mut self.analyses.show_bode,
                            "📉 Bode plotter (AC analysis)",
                        );
                        ui.checkbox(&mut self.analyses.show_transfer, "↗ DC transfer (DC sweep)");
                        ui.checkbox(&mut self.analyses.show_tracer, "〽 Curve tracer");
                        ui.separator();
                        if ui
                            .add_enabled(
                                crate::float::any_popped(ctx),
                                egui::Button::new("Dock all instruments"),
                            )
                            .on_hover_text(
                                "Bring every instrument that is in a window of its own back \
                             inside the main window",
                            )
                            .clicked()
                        {
                            crate::float::dock_all(ctx);
                            ui.close();
                        }
                    });
                    ui.menu_button("View", |ui| {
                        ui.checkbox(&mut self.show_voltages, "Node voltages on the schematic");
                        ui.separator();
                        ui.menu_button("Panels", |ui| {
                            for pane in Pane::SIDE_PANELS {
                                let mut open = dock::is_open(&self.dock, pane);
                                if ui.checkbox(&mut open, pane.title()).changed() {
                                    if open {
                                        dock::open(&mut self.dock, pane);
                                    } else {
                                        dock::close(&mut self.dock, pane);
                                    }
                                }
                            }
                        });
                        if ui
                            .button("Reset layout")
                            .on_hover_text("Put every panel back where it started")
                            .clicked()
                        {
                            self.dock = dock::default_layout();
                            ui.close();
                        }
                    });
                    ui.menu_button("Window", |ui| {
                        self.window_menu(ui);
                    });
                    ui.menu_button("Help", |ui| {
                        if ui.button("Keyboard shortcuts").clicked() {
                            self.show_shortcuts = true;
                            ui.close();
                        }
                        if ui.button("About Dunbrec").clicked() {
                            self.show_about = true;
                            ui.close();
                        }
                    });
                });
        });
    }

    /// Moving between the open circuits, closing them, and the list of them.
    fn window_menu(&mut self, ui: &mut egui::Ui) {
        let order = dock::circuit_order(&self.dock);
        let several = order.len() > 1;
        let active = self.active().id;
        for (label, step) in [
            ("Next circuit   Ctrl+Tab", 1),
            ("Previous circuit   Ctrl+Shift+Tab", -1),
        ] {
            if ui.add_enabled(several, egui::Button::new(label)).clicked() {
                self.cycle_circuits(step);
                ui.close();
            }
        }
        ui.separator();
        if ui
            .add_enabled(several, egui::Button::new("Close all but this one"))
            .clicked()
        {
            self.request(Pending::CloseOthers(active));
            ui.close();
        }
        if ui
            .add_enabled(self.circuit_open, egui::Button::new("Close all circuits"))
            .clicked()
        {
            self.request(Pending::CloseAll);
            ui.close();
        }
        ui.separator();
        if !self.circuit_open {
            ui.label(RichText::new("No circuit open").weak());
        }
        for id in order {
            let Some(index) = self.index_of(id) else {
                continue;
            };
            let circuit = &self.circuits[index];
            let dot = if circuit.doc.is_dirty() { " •" } else { "" };
            let name = format!("{}{dot}", circuit.name());
            if ui.selectable_label(index == self.active, name).clicked() {
                self.switch_to(index);
                ui.close();
            }
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(action) = self.pending.clone() {
            let at_risk = self.at_risk(&action);
            let answer = match at_risk.as_slice() {
                // Nothing left to lose.
                [] => Some(Unsaved::Discard),
                [one] => dialog::unsaved_changes(ctx, &self.circuits[*one].name(), true),
                many => {
                    let names: Vec<String> =
                        many.iter().map(|&i| self.circuits[i].name()).collect();
                    dialog::unsaved_list(ctx, &names, true)
                }
            };
            match answer {
                // A save that is cancelled or fails leaves the question up.
                Some(Unsaved::Save) if at_risk.iter().all(|&i| self.save_circuit(i, false)) => {
                    self.pending = None;
                    self.perform(action);
                }
                Some(Unsaved::Discard) => {
                    self.pending = None;
                    self.perform(action);
                }
                Some(Unsaved::KeepEditing) => self.pending = None,
                _ => {}
            }
        }

        if let Some(ask) = &self.shared_ask {
            let name = ask
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let buttons: &[&str] = if ask.may_share {
                &["Make a copy", "Open as it is", "Cancel"]
            } else {
                &["Make a copy", "Cancel"]
            };
            let answer = Dialog {
                id: "shared_source",
                kind: dialog::Kind::Question,
                title: "Shared source file",
                buttons,
                default: 0,
                cancel: Some(buttons.len() - 1),
            }
            .show(ctx, |ui| {
                for (file, owner) in &ask.shared {
                    dialog::paragraph(ui, &format!("“{file}” is also used by {owner}."));
                }
                ui.add_space(6.0);
                dialog::paragraph(
                    ui,
                    &format!(
                        "Circuits should not share a source file. Open {name} with a copy \
                         of its own?"
                    ),
                );
            });
            if let Some(choice) = answer
                && let Some(ask) = self.shared_ask.take()
            {
                match buttons[choice] {
                    "Make a copy" => self.open_with_copies(ask),
                    "Open as it is" => {
                        let mut doc = Document::new(ask.schematic, None);
                        doc.mark_saved(ask.path);
                        self.add_opened(doc, &ask.problems);
                    }
                    _ => {}
                }
            }
        }

        if let Some(edit) = &self.shared_edit {
            let answer = Dialog {
                id: "shared_edit",
                kind: dialog::Kind::Question,
                title: "Shared source file",
                buttons: &["Make a copy", "Undo the change"],
                default: 0,
                cancel: Some(1),
            }
            .show(ctx, |ui| {
                for (file, owner) in &edit.shared {
                    dialog::paragraph(ui, &format!("“{file}” is already used by {owner}."));
                }
                ui.add_space(6.0);
                dialog::paragraph(
                    ui,
                    "Circuits cannot share a source file. Use a copy of it here instead?",
                );
            });
            if let Some(choice) = answer
                && let Some(edit) = self.shared_edit.take()
            {
                if choice == 0 {
                    self.copy_for_edit(edit);
                } else if let Some(index) = self.index_of(edit.circuit) {
                    self.circuits[index].doc.undo();
                }
            }
        }

        if let Some(ask) = &mut self.assign_ask {
            let name = ask
                .file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (chips, chosen) = (&ask.chips, &mut ask.chosen);
            let answer = Dialog {
                id: "assign_source",
                kind: dialog::Kind::Question,
                title: "Use a source file",
                buttons: &["Use it", "Cancel"],
                default: 0,
                cancel: Some(1),
            }
            .show(ctx, |ui| {
                dialog::paragraph(ui, &format!("Which microcontroller should run “{name}”?"));
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        for (i, (_, _, label)) in chips.iter().enumerate() {
                            ui.radio_value(chosen, i, RichText::new(label).size(14.5));
                        }
                    });
            });
            if let Some(choice) = answer
                && let Some(mut ask) = self.assign_ask.take()
                && choice == 0
                && ask.chosen < ask.chips.len()
            {
                let (circuit, chip, _) = ask.chips.swap_remove(ask.chosen);
                self.assign(&ask.file, circuit, chip);
            }
        }

        if self.active().canvas.has_label_request() {
            let answer = Dialog {
                id: "label",
                kind: dialog::Kind::Question,
                title: "Net label",
                buttons: &["OK", "Cancel"],
                default: 0,
                cancel: Some(1),
            }
            .show(ctx, |ui| {
                dialog::paragraph(ui, "Name this net. Nets with the same name are connected.");
                ui.add_space(8.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.label_name)
                        .hint_text("e.g. VOUT")
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
            });
            if let Some(choice) = answer {
                let at = self.active_mut().canvas.take_label_request();
                let name = std::mem::take(&mut self.label_name).trim().to_string();
                if let (0, Some(at), false) = (choice, at, name.is_empty()) {
                    self.active_mut().doc.edit(|s| s.add_label(name, at));
                }
            }
        }

        if let (Some(prompt), Some(project)) = (&mut self.name_prompt, &self.project) {
            match prompt.show(ctx, project) {
                Some(Some(command)) => {
                    self.name_prompt = None;
                    self.run_project_command(command);
                }
                Some(None) => self.name_prompt = None,
                None => {}
            }
        } else {
            // The project was closed while a name was being asked for.
            self.name_prompt = None;
        }

        if let Some(ask) = self.confirm_delete.clone() {
            self.confirm_delete_dialog(ctx, ask);
        }

        if let Some(message) = &self.message
            && dialog::show_message(ctx, message)
        {
            self.message = None;
        }

        self.about_window(ctx);

        if self.show_shortcuts {
            let mut open = true;
            egui::Window::new("Keyboard shortcuts")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    let tallest = ctx.content_rect().height() * 0.75;
                    egui::ScrollArea::vertical()
                        .max_height(tallest)
                        .show(ui, |ui| {
                            egui::Grid::new("shortcuts").striped(true).show(ui, |ui| {
                                for (keys, what) in SHORTCUTS {
                                    ui.monospace(*keys);
                                    ui.label(*what);
                                    ui.end_row();
                                }
                            });
                        });
                });
            self.show_shortcuts = open;
        }
    }

    /// The bar along the bottom: how to use the tool in hand, if it is not
    /// plain selecting, and the zoom of the schematic. Empty with no circuit
    /// open. (It also takes the colour of a running simulation.)
    fn status_bar(&self, ctx: &egui::Context) {
        let frame = self.run_frame(ctx);
        egui::TopBottomPanel::bottom("status")
            .frame(frame)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // As tall empty as full, so that nothing above it moves.
                    ui.set_min_height(ui.spacing().interact_size.y);
                    if !self.circuit_open {
                        return;
                    }
                    let canvas = &self.active().canvas;
                    // The zoom is placed first; the hint gets what is left,
                    // and is cut short rather than run under it. (Pointing at
                    // it then shows the whole of it.)
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format!("{:.0}%", canvas.camera.scale / 14.0 * 100.0));
                        ui.add_space(12.0);
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            if let Some(hint) = tool_hint(&canvas.tool) {
                                ui.add(egui::Label::new(hint).truncate());
                            }
                        });
                    });
                });
            });
    }
}

/// What the circuit is doing, for the assistant: the running simulation if
/// there is one for this circuit, else the DC point, plus serial output.
fn assistant_readings(
    netlist: &Netlist,
    sections: Vec<(String, Vec<String>)>,
    latest: &Option<Sample>,
    running: bool,
    dc: &Option<DcResults>,
    generation: u64,
    serial: &SerialMonitor,
) -> crate::assistant::Readings {
    let mut r = crate::assistant::Readings {
        heading: "The simulation is not running and no DC point is calculated, so there \
                  are no readings."
            .into(),
        serial: serial.tails(1500),
        sections,
        ..Default::default()
    };
    match (latest, dc) {
        (Some(sample), _) if running => {
            r.heading = format!(
                "The simulation is running; this is a snapshot at t = {}.",
                format_value(sample.time, "s")
            );
            let unused = netlist.unused_io_nets();
            for (i, node) in netlist.nodes.iter().enumerate().skip(1) {
                if let (false, Some(v)) = (unused.contains(&NodeId(i)), sample.voltages.get(i)) {
                    r.voltages.push((node.name.clone(), *v));
                }
            }
            for (e, i) in netlist.elements.iter().zip(&sample.currents) {
                r.currents.push((e.designator.clone(), *i));
            }
        }
        (
            _,
            Some(DcResults {
                outcome: Ok(t),
                generation: g,
            }),
        ) if *g == generation => {
            r.heading = "The simulation is not running; this is the DC operating point.".into();
            r.voltages = t.voltages.clone();
            r.currents = t.currents.clone();
            for w in &t.warnings {
                r.heading.push_str(&format!("\nWarning: {w}"));
            }
        }
        _ => {}
    }
    r
}

/// Explain a netlist problem in a sentence.
pub(crate) fn describe_diagnostic(schematic: &Schematic, d: &Diagnostic) -> String {
    let name = |id| {
        schematic
            .component(id)
            .map(|c| c.designator.clone())
            .unwrap_or_else(|| "?".into())
    };
    match d {
        Diagnostic::NoGround => "No ground symbol: every circuit needs a 0 V reference.".into(),
        Diagnostic::UnconnectedPin { component, pin } => {
            format!("{} pin {pin} is not connected.", name(*component))
        }
        Diagnostic::ConflictingLabels { names } => {
            format!("One net has several labels: {}.", names.join(", "))
        }
        Diagnostic::MissingModel { component, model } => {
            format!(
                "{} uses model {model}, which is not defined.",
                name(*component)
            )
        }
        Diagnostic::WrongModelKind {
            component,
            model,
            expected,
            ..
        } => format!(
            "{} needs a {} model, but {model} is a different kind.",
            name(*component),
            expected.spice_name()
        ),
        Diagnostic::DuplicateDesignator { designator, .. } => {
            format!("More than one part is called {designator}.")
        }
    }
}

/// How to use `tool`, for the bar along the bottom; selecting needs no words.
fn tool_hint(tool: &Tool) -> Option<&'static str> {
    Some(match tool {
        Tool::Select => return None,
        Tool::Place { .. } => "Click to place. R rotates, X mirrors. Esc to stop.",
        Tool::Wire => "Wire: click to start, click to bend. Esc to stop.",
        Tool::Label => "Click a wire or pin to name it. Esc to stop.",
        Tool::Probe => "Click a wire, pin or part to connect. Esc to stop.",
    })
}

/// Placing junctions: what the J key picks.
fn junction_tool() -> Tool {
    Tool::Place {
        kind: dunbrec_core::ComponentKind::Junction,
        rotation: dunbrec_core::Rotation::R0,
        mirrored: false,
    }
}

/// What placing `entry` looks like to the canvas.
fn place_tool(entry: &PaletteEntry) -> Tool {
    Tool::Place {
        kind: (entry.make)(),
        rotation: dunbrec_core::Rotation::R0,
        mirrored: false,
    }
}

/// The tools, a search box, and the parts library by group. With nothing
/// typed, groups fold open and shut; with a search, matching parts are
/// listed under their group and Enter picks the first.
fn palette_ui(ui: &mut egui::Ui, canvas: &mut Canvas, search: &mut String) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("Tools");
        let tool = canvas.tool.clone();
        if ui
            .selectable_label(tool == Tool::Select, "⬉ Select   (S)")
            .clicked()
        {
            canvas.cancel();
        }
        if ui
            .selectable_label(tool == Tool::Wire, "✏ Wire   (W)")
            .clicked()
        {
            canvas.tool = Tool::Wire;
        }
        if ui
            .selectable_label(tool == Tool::Label, "🏷 Net label   (L)")
            .clicked()
        {
            canvas.tool = Tool::Label;
        }
        if ui
            .selectable_label(tool == Tool::Probe, "🔌 Probe   (P)")
            .clicked()
        {
            canvas.tool = Tool::Probe;
        }
        ui.add_space(8.0);

        let groups = palette_groups(&search.trim().to_lowercase());
        let searching = !search.trim().is_empty();
        ui.horizontal(|ui| {
            let clear_room = 26.0;
            let edit = ui.add(
                egui::TextEdit::singleline(search)
                    .hint_text("Search parts")
                    .desired_width(ui.available_width() - clear_room),
            );
            if searching
                && ui
                    .small_button("×")
                    .on_hover_text("Clear the search")
                    .clicked()
            {
                search.clear();
            }
            if edit.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && let Some((_, entries)) = groups.first()
                && let Some(first) = entries.first()
            {
                canvas.tool = place_tool(first);
            }
        });
        if searching && groups.is_empty() {
            ui.label(RichText::new("No parts match").weak());
        }

        let part_list = |ui: &mut egui::Ui, canvas: &mut Canvas, entries: &[&PaletteEntry]| {
            for entry in entries {
                let kind = (entry.make)();
                let active = matches!(&canvas.tool, Tool::Place { kind: k, .. } if *k == kind);
                // Click to pick the part and place copies; drag it onto the
                // canvas to place one there and go on placing.
                let part = ui.add(
                    egui::Button::selectable(active, entry.name)
                        .sense(egui::Sense::click_and_drag()),
                );
                if part.clicked() {
                    canvas.tool = place_tool(entry);
                }
                part.dnd_set_drag_payload(PartDrag { kind });
                if part.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                }
            }
        };
        for (group, entries) in &groups {
            if searching {
                ui.add_space(6.0);
                ui.label(RichText::new(*group).strong());
                part_list(ui, canvas, entries);
            } else {
                egui::CollapsingHeader::new(RichText::new(*group).strong())
                    .id_salt(("palette group", *group))
                    .default_open(true)
                    .show(ui, |ui| part_list(ui, canvas, entries));
            }
        }
    });
}

/// Wiring problems in plain English.
fn checks_ui(ui: &mut egui::Ui, netlist: &Netlist, schematic: &Schematic) {
    egui::ScrollArea::vertical()
        .id_salt("checks")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let diags = &netlist.diagnostics;
            if diags.is_empty() {
                ui.label(
                    RichText::new("✔ No wiring problems found")
                        .color(Color32::from_rgb(80, 170, 90)),
                );
            }
            for d in diags {
                let color = match d.severity() {
                    dunbrec_core::Severity::Error => Color32::from_rgb(225, 70, 70),
                    dunbrec_core::Severity::Warning => Color32::from_rgb(220, 160, 40),
                };
                ui.label(RichText::new(describe_diagnostic(schematic, d)).color(color));
            }
        });
}

/// Live node voltages and currents while simulating, otherwise the DC
/// operating point.
fn values_ui(
    ui: &mut egui::Ui,
    netlist: &Netlist,
    running: bool,
    latest: &Option<Sample>,
    dc: &Option<DcResults>,
) {
    if let (true, Some(sample)) = (running, latest) {
        ui.strong(format!(
            "Live values at t = {}",
            format_value(sample.time, "s")
        ));
        egui::ScrollArea::vertical()
            .id_salt("live")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let unused = netlist.unused_io_nets();
                egui::Grid::new("live_table").striped(true).show(ui, |ui| {
                    for (i, node) in netlist.nodes.iter().enumerate().skip(1) {
                        if unused.contains(&NodeId(i)) {
                            continue;
                        }
                        if let Some(v) = sample.voltages.get(i) {
                            ui.monospace(format!("V({})", node.name));
                            ui.monospace(format_value(*v, "V"));
                            ui.end_row();
                        }
                    }
                    for (e, i) in netlist.elements.iter().zip(&sample.currents) {
                        ui.monospace(format!("I({})", e.designator));
                        ui.monospace(format_value(*i, "A"));
                        ui.end_row();
                    }
                });
            });
        return;
    }
    ui.strong("DC operating point");
    egui::ScrollArea::vertical()
        .id_salt("dc")
        .auto_shrink([false, false])
        .show(ui, |ui| match dc {
            None => {
                ui.label(RichText::new("Simulate → Recalculate DC now").weak());
            }
            // A circuit with nothing in it yet has nothing wrong with it.
            Some(DcResults {
                outcome: Err(e), ..
            }) if netlist.elements.is_empty() => {
                ui.label(RichText::new(e).weak());
            }
            Some(DcResults {
                outcome: Err(e), ..
            }) => {
                ui.label(RichText::new(e).color(Color32::from_rgb(225, 70, 70)));
            }
            Some(DcResults { outcome: Ok(t), .. }) => {
                for w in &t.warnings {
                    ui.label(RichText::new(w).color(Color32::from_rgb(220, 160, 40)));
                }
                egui::Grid::new("dc_table").striped(true).show(ui, |ui| {
                    for (name, v) in &t.voltages {
                        ui.monospace(format!("V({name})"));
                        ui.monospace(format_value(*v, "V"));
                        ui.end_row();
                    }
                    for (name, i) in &t.currents {
                        ui.monospace(format!("I({name})"));
                        ui.monospace(format_value(*i, "A"));
                        ui.end_row();
                    }
                });
            }
        });
}

/// Tell the user which source files could not be opened with a schematic.
fn source_problems(problems: &[String]) -> Message {
    let mut message = Message::info(
        "Some source files could not be opened",
        "A chip is set to run a source file that is missing or cannot be read. The rest \
         of the circuit opened normally. To put it right, right-click the red file in the \
         Project panel and choose Remove from project, or choose another program in the chip's \
         properties.",
    )
    .with_detail(problems.join("\n"));
    message.kind = dialog::Kind::Warning;
    message
}

/// What the dock needs to draw each panel: the parts of the app they show
/// or edit, borrowed for one frame.
struct PaneViewer<'a> {
    /// What each of the project's circuits runs.
    project_code: &'a [CircuitCode],
    open_files: &'a [PathBuf],
    /// With no circuit open, the panels about one say nothing of it.
    circuit_open: bool,
    /// Every open circuit's id, name and whether it has unsaved changes.
    circuits: &'a [(u32, String, bool)],
    /// The active circuit, whose parts follow.
    active_id: u32,
    tabs: &'a mut DockState<DocTab>,
    hidden: &'a mut std::collections::BTreeSet<String>,
    /// Closing the user asked for from the circuit tabs this frame.
    closing: &'a mut Vec<Pending>,
    tab_rects: &'a mut Vec<(u32, egui::Rect)>,
    #[cfg(test)]
    file_tab_rects: &'a mut Vec<(DocTab, egui::Rect)>,
    doc: &'a mut Document,
    canvas: &'a mut Canvas,
    properties: &'a mut Properties,
    code: &'a mut CodeEditor,
    debugger: &'a mut Debugger,
    live: &'a mut Option<LiveSim>,
    palette_search: &'a mut String,
    netlist: &'a Netlist,
    dc: &'a Option<DcResults>,
    latest: &'a Option<Sample>,
    /// What the assistant is told the circuit is doing.
    readings: &'a dyn Fn() -> crate::assistant::Readings,
    running: bool,
    annotations: &'a [Annotation],
    markers: &'a [Marker],
    lit: &'a std::collections::HashSet<dunbrec_core::ComponentId>,
    project: Option<&'a Project>,
    recent_projects: &'a [PathBuf],
    project_panel: &'a mut ProjectPanel,
    /// What the Project panel asked for this frame.
    project_command: &'a mut Option<project_ui::Command>,
    assistant: &'a mut crate::assistant::Assistant,
    ai: &'a crate::ai::AiSettings,
    title: &'a str,
    open_settings: &'a mut bool,
    /// The colours of the active circuit's tab.
    accent: egui::style::Selection,
}

impl TabViewer for PaneViewer<'_> {
    type Tab = Pane;

    fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
        let Pane::Circuit(id) = *tab else {
            return tab.title().into();
        };
        match self.circuits.iter().find(|c| c.0 == id) {
            Some((_, name, dirty)) => {
                let dot = if *dirty { " •" } else { "" };
                RichText::new(format!("{name}{dot}")).strong().into()
            }
            None => "".into(),
        }
    }

    // Not from the title, which changes with unsaved changes.
    fn id(&mut self, tab: &mut Pane) -> egui::Id {
        egui::Id::new(("pane", *tab))
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Pane) {
        match *tab {
            // Another circuit than the active one is in front only for the
            // frame in which the user picks it.
            Pane::Circuit(id) if id != self.active_id => {}
            Pane::Circuit(id) => {
                let mut style = egui_dock::Style::from_egui(ui.style());
                style.main_surface_border_stroke = egui::Stroke::NONE;
                let mut documents = DocViewer {
                    doc: &mut *self.doc,
                    canvas: &mut *self.canvas,
                    code: &mut *self.code,
                    debugger: &mut *self.debugger,
                    live: &mut *self.live,
                    hidden: &mut *self.hidden,
                    netlist: self.netlist,
                    annotations: self.annotations,
                    markers: self.markers,
                    lit: self.lit,
                    #[cfg(test)]
                    tab_rects: &mut *self.file_tab_rects,
                };
                DockArea::new(&mut *self.tabs)
                    .id(egui::Id::new(("circuit-dock", id)))
                    .style(style)
                    .show_leaf_close_all_buttons(false)
                    .show_leaf_collapse_buttons(false)
                    .show_inside(ui, &mut documents);
            }
            Pane::Palette => palette_ui(ui, self.canvas, self.palette_search),
            Pane::Properties | Pane::Checks | Pane::Values if !self.circuit_open => {
                ui.label(RichText::new("No circuit open").weak());
            }
            Pane::Properties => {
                let selection: Vec<Item> = self.canvas.selection.iter().copied().collect();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.properties.show(ui, self.doc, &selection);
                });
            }
            Pane::Checks => checks_ui(ui, self.netlist, self.doc.schematic()),
            Pane::Values => values_ui(ui, self.netlist, self.running, self.latest, self.dc),
            Pane::Project => {
                let embedded = if self.doc.path().is_some() {
                    project::embedded_count(self.doc.schematic())
                } else {
                    0
                };
                let view = project_ui::View {
                    code: self.project_code,
                    open: self.open_files,
                    project: self.project,
                    recent: self.recent_projects,
                    current: self.doc.path(),
                    embedded,
                };
                if let Some(command) = self.project_panel.show(ui, &view) {
                    *self.project_command = Some(command);
                }
            }
            Pane::Assistant => {
                let view = crate::assistant::View {
                    settings: self.ai,
                    title: self.title,
                    schematic: self.doc.schematic(),
                    netlist: self.netlist,
                    readings: self.readings,
                };
                match self.assistant.show(ui, &view) {
                    Some(crate::assistant::Command::OpenSettings) => *self.open_settings = true,
                    Some(crate::assistant::Command::Apply { key, edit }) => {
                        let result = crate::edits::apply(self.doc, &edit);
                        self.assistant.record(key, result);
                    }
                    None => {}
                }
            }
            Pane::Workspace | Pane::Retired => {}
        }
    }

    fn is_closeable(&self, tab: &Pane) -> bool {
        tab.closeable()
    }

    fn allowed_in_windows(&self, tab: &mut Pane) -> bool {
        tab.floats()
    }

    /// Closing a circuit's tab (its close button, a middle click, or the
    /// tab's menu) closes the circuit, after the question about unsaved
    /// changes; the tab itself is taken out by the circuit closing.
    fn on_close(&mut self, tab: &mut Pane) -> OnCloseResponse {
        if let Pane::Circuit(id) = *tab {
            self.closing.push(Pending::CloseCircuit(id));
            OnCloseResponse::Ignore
        } else {
            OnCloseResponse::Close
        }
    }

    /// A circuit tab's menu closes more than the one tab as well.
    fn context_menu(
        &mut self,
        ui: &mut egui::Ui,
        tab: &mut Pane,
        _surface: egui_dock::SurfaceIndex,
        _node: egui_dock::NodeIndex,
    ) {
        let Pane::Circuit(id) = *tab else { return };
        let others = egui::Button::new("Close all but this one");
        if ui.add_enabled(self.circuits.len() > 1, others).clicked() {
            self.closing.push(Pending::CloseOthers(id));
            ui.close();
        }
        if ui.button("Close all").clicked() {
            self.closing.push(Pending::CloseAll);
            ui.close();
        }
    }

    fn on_tab_button(&mut self, tab: &mut Pane, response: &egui::Response) {
        if let Pane::Circuit(id) = *tab {
            self.tab_rects.push((id, response.rect));
        }
    }

    /// Every panel scrolls itself, or draws edge to edge.
    fn scroll_bars(&self, _tab: &Pane) -> [bool; 2] {
        [false, false]
    }

    /// A circuit fills its tab to the edge, and the one in front stands out.
    fn tab_style_override(
        &self,
        tab: &Pane,
        global: &egui_dock::TabStyle,
    ) -> Option<egui_dock::TabStyle> {
        match tab {
            Pane::Circuit(_) => {
                let mut style = global.clone();
                style.tab_body.inner_margin = egui::Margin::ZERO;
                for s in [
                    &mut style.active,
                    &mut style.focused,
                    &mut style.active_with_kb_focus,
                    &mut style.focused_with_kb_focus,
                ] {
                    s.bg_fill = self.accent.bg_fill;
                    s.text_color = self.accent.stroke.color;
                }
                Some(style)
            }
            // The empty central area: its tab is not drawn.
            Pane::Workspace => {
                let mut style = global.clone();
                for s in [
                    &mut style.active,
                    &mut style.inactive,
                    &mut style.focused,
                    &mut style.hovered,
                    &mut style.inactive_with_kb_focus,
                    &mut style.active_with_kb_focus,
                    &mut style.focused_with_kb_focus,
                ] {
                    s.bg_fill = egui::Color32::TRANSPARENT;
                    s.outline_color = egui::Color32::TRANSPARENT;
                    s.text_color = egui::Color32::TRANSPARENT;
                }
                style.minimum_width = Some(0.0);
                Some(style)
            }
            _ => None,
        }
    }
}

/// What a circuit's own row of tabs draws: its schematic and source files.
struct DocViewer<'a> {
    doc: &'a mut Document,
    canvas: &'a mut Canvas,
    code: &'a mut CodeEditor,
    debugger: &'a mut Debugger,
    live: &'a mut Option<LiveSim>,
    /// Source files whose tabs are closed.
    hidden: &'a mut std::collections::BTreeSet<String>,
    netlist: &'a Netlist,
    annotations: &'a [Annotation],
    markers: &'a [Marker],
    lit: &'a std::collections::HashSet<dunbrec_core::ComponentId>,
    #[cfg(test)]
    tab_rects: &'a mut Vec<(DocTab, egui::Rect)>,
}

impl TabViewer for DocViewer<'_> {
    type Tab = DocTab;

    fn title(&mut self, tab: &mut DocTab) -> egui::WidgetText {
        match tab {
            DocTab::Schematic => "Schematic".into(),
            DocTab::Source(name) => source_title(self.doc.schematic(), name).into(),
        }
    }

    // Not from the title, which changes with the chips that run a file.
    fn id(&mut self, tab: &mut DocTab) -> egui::Id {
        egui::Id::new(("document", &*tab))
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut DocTab) {
        match tab {
            DocTab::Schematic => self.canvas.show(
                ui,
                self.doc,
                self.netlist,
                self.annotations,
                self.markers,
                self.lit,
            ),
            DocTab::Source(name) => {
                self.code
                    .show_file(ui, self.doc, self.debugger, self.live, name);
            }
        }
    }

    fn is_closeable(&self, tab: &DocTab) -> bool {
        *tab != DocTab::Schematic
    }

    /// Closing a file's tab only stops showing it: it stays with the circuit,
    /// and is saved with it.
    fn on_close(&mut self, tab: &mut DocTab) -> OnCloseResponse {
        if let DocTab::Source(name) = tab {
            self.hidden.insert(name.clone());
        }
        OnCloseResponse::Close
    }

    #[cfg(test)]
    fn on_tab_button(&mut self, tab: &mut DocTab, response: &egui::Response) {
        self.tab_rects.push((tab.clone(), response.rect));
    }

    fn allowed_in_windows(&self, _tab: &mut DocTab) -> bool {
        false
    }

    fn scroll_bars(&self, _tab: &DocTab) -> [bool; 2] {
        [false, false]
    }

    /// The schematic fills its tab to the edge.
    fn tab_style_override(
        &self,
        tab: &DocTab,
        global: &egui_dock::TabStyle,
    ) -> Option<egui_dock::TabStyle> {
        (*tab == DocTab::Schematic).then(|| {
            let mut style = global.clone();
            style.tab_body.inner_margin = egui::Margin::ZERO;
            style
        })
    }
}

impl eframe::App for DunbrecApp {
    /// Ctrl+Tab is taken out before egui sees it: it would move the keyboard
    /// focus, or type a tab into a source file.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        let mut step = 0;
        input.events.retain(|event| match event {
            egui::Event::Key {
                key: Key::Tab,
                pressed,
                modifiers,
                ..
            } if modifiers.ctrl => {
                if *pressed {
                    step += if modifiers.shift { -1 } else { 1 };
                }
                false
            }
            _ => true,
        });
        self.cycle_by += step;
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, &self.settings);
        eframe::set_value(storage, LAYOUT_KEY, &self.dock);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Closing with unsaved changes asks first.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.allow_close
            && !self.dirty_circuits().is_empty()
        {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
        }
        if self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }

        // Coming back from another program: its edits to the project's
        // files show up.
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.was_focused {
            self.refresh_project();
        }
        self.was_focused = focused;

        // With no project open, the Projects screen is all there is: no
        // menu, toolbar, panels or instrument windows.
        if self.project.is_none() {
            if self.title != "Dunbrec" {
                ctx.send_viewport_cmd(ViewportCommand::Title("Dunbrec".into()));
                self.title = "Dunbrec".into();
            }
            let view = crate::projects_screen::View {
                recent: &self.settings.recent_projects,
                waiting: self.waiting_circuit.as_deref(),
            };
            if let Some(command) = self.projects_screen.show(ctx, &view) {
                self.run_screen_command(command);
            }
            self.dialogs(ctx);
            self.settings_dialog.show(ctx, &mut self.settings);
            return;
        }

        // A sketch is ready to run once built, whether or not it is in view.
        for circuit in &mut self.circuits {
            circuit.code.poll_build(ctx);
        }
        self.shortcuts(ctx);
        // A chip given a new source file gets its tab, the file is listed in
        // the project, and one another circuit uses is asked about.
        self.check_new_code();
        self.list_used_files();
        let live_dc = self.live_dc;
        self.active_mut().refresh_netlist(live_dc);
        self.continue_run(ctx);
        self.refresh_signals();
        self.sync_live();
        if self.live.as_ref().is_some_and(|l| !l.paused) {
            ctx.request_repaint();
        }

        let title = if self.circuit_open {
            format!(
                "{}{} — Dunbrec",
                self.active().name(),
                if self.active().doc.is_dirty() {
                    " •"
                } else {
                    ""
                }
            )
        } else {
            let name = self.project.as_ref().map(|p| p.name());
            format!("{} — Dunbrec", name.unwrap_or_default())
        };
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        self.menu_bar(ctx);
        self.run_toolbar(ctx);
        self.status_bar(ctx);
        self.dock_area(ctx);

        if let Some((at, part)) = self.active_mut().canvas.take_probe_request() {
            let s = self.active().doc.schematic();
            let on_net = self
                .active()
                .node_map
                .1
                .node_at(s, &self.active().netlist.1, at)
                .is_some();
            self.probe_hint = self.bench.connect(at, part, on_net).err();
        }
        self.instrument_windows(ctx);
        self.sync_speed();

        // Drop selections of items that no longer exist (after undo etc.).
        self.active_mut().prune_selection();

        self.dialogs(ctx);
        self.settings_dialog.show(ctx, &mut self.settings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::palette;
    use crate::settings::BarSize;

    #[test]
    fn the_j_key_picks_the_same_tool_as_the_palette_junction() {
        let entry = palette()
            .iter()
            .find(|e| matches!((e.make)(), dunbrec_core::ComponentKind::Junction))
            .expect("a junction in the palette");
        assert_eq!(junction_tool(), place_tool(entry));
    }

    /// The whole app in a headless context, clicked by the labels it shows.
    struct Rig {
        ctx: egui::Context,
        app: DunbrecApp,
        time: f64,
        seen: Vec<(String, egui::Rect)>,
    }

    impl Rig {
        fn frame(&mut self, events: Vec<egui::Event>) {
            use eframe::App as _;
            self.time += 0.1;
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
            let mut input = egui::RawInput {
                screen_rect: Some(screen),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            self.app.raw_input_hook(&self.ctx, &mut input);
            let app = &mut self.app;
            let mut frame = eframe::Frame::_new_kittest();
            let out = self.ctx.run(input, |ctx| app.update(ctx, &mut frame));
            if let Some(update) = out.platform_output.accesskit_update {
                self.seen = update
                    .nodes
                    .iter()
                    .filter_map(|(_, n)| {
                        let text = n.label().or_else(|| n.value())?.to_string();
                        let b = n.bounds()?;
                        let min = egui::pos2(b.x0 as f32, b.y0 as f32);
                        let max = egui::pos2(b.x1 as f32, b.y1 as f32);
                        Some((text, egui::Rect::from_min_max(min, max)))
                    })
                    .collect();
            }
        }

        fn settle(&mut self) {
            for _ in 0..4 {
                self.frame(vec![]);
            }
        }

        /// Click the thing labelled `label`, or failing that one whose label
        /// ends with it.
        fn click(&mut self, label: &str) {
            let at = self
                .seen
                .iter()
                .find(|(text, _)| text == label)
                .or_else(|| self.seen.iter().find(|(text, _)| text.ends_with(label)))
                .unwrap_or_else(|| panic!("nothing labelled {label:?} is shown"))
                .1
                .center();
            self.click_at(at, egui::PointerButton::Primary);
        }

        fn button(at: egui::Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
            egui::Event::PointerButton {
                pos: at,
                button,
                pressed,
                modifiers: Modifiers::NONE,
            }
        }

        fn click_at(&mut self, at: egui::Pos2, button: egui::PointerButton) {
            self.frame(vec![egui::Event::PointerMoved(at)]);
            for pressed in [true, false] {
                self.frame(vec![Self::button(at, button, pressed)]);
            }
            self.settle();
        }

        fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
            let primary = egui::PointerButton::Primary;
            self.frame(vec![egui::Event::PointerMoved(from)]);
            self.frame(vec![Self::button(from, primary, true)]);
            for i in 1..=20 {
                let at = from + (to - from) * (i as f32 / 20.0);
                self.frame(vec![egui::Event::PointerMoved(at)]);
            }
            for _ in 0..8 {
                self.frame(vec![egui::Event::PointerMoved(to)]);
            }
            self.frame(vec![Self::button(to, primary, false)]);
            self.settle();
        }

        /// The middle of the tab of the open circuit called `name`.
        fn circuit_tab(&self, name: &str) -> egui::Pos2 {
            let circuit = self
                .app
                .circuits
                .iter()
                .find(|c| c.name() == name)
                .unwrap_or_else(|| panic!("{name} is not open"));
            self.app
                .circuit_tab_rects
                .iter()
                .find(|(id, _)| *id == circuit.id)
                .unwrap_or_else(|| panic!("{name} has no tab"))
                .1
                .center()
        }

        fn click_circuit(&mut self, name: &str) {
            self.click_at(self.circuit_tab(name), egui::PointerButton::Primary);
        }

        /// The middle of a tab in the active circuit's own row.
        fn file_tab(&self, tab: &DocTab) -> egui::Pos2 {
            self.app
                .file_tab_rects
                .iter()
                .find(|(t, _)| t == tab)
                .unwrap_or_else(|| panic!("no tab {tab:?}"))
                .1
                .center()
        }

        fn active_name(&self) -> String {
            self.app.active().name()
        }

        /// The names of the circuits that have tabs.
        fn open_names(&self) -> Vec<String> {
            self.app
                .open_ids()
                .iter()
                .map(|&id| {
                    let i = self.app.index_of(id).unwrap();
                    assert!(dock::is_open(&self.app.dock, Pane::Circuit(id)));
                    self.app.circuits[i].name()
                })
                .collect()
        }
    }

    #[test]
    fn choosing_a_demo_adds_a_file_to_the_project_and_it_runs_in_real_time() {
        let dir = std::env::temp_dir().join(format!("dunbrec-usedemo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (project, circuit) = Project::create(&dir.join("P")).unwrap();
        let blink = examples()
            .iter()
            .find(|e| e.name == "Arduino blink")
            .expect("the blink example");
        let schematic = (blink.build)();
        let chip = schematic
            .components
            .iter()
            .find(|c| matches!(c.kind, dunbrec_core::ComponentKind::Mcu { .. }))
            .unwrap()
            .id;
        dunbrec_io::dbr::save(&schematic, &circuit).unwrap();

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DunbrecApp::new(&cc, None, Some(project.file.clone()));
        app.open_path(circuit);
        app.active_mut().canvas.selection = [Item::Component(chip)].into_iter().collect();
        let mut rig = Rig {
            ctx,
            app,
            time: 0.0,
            seen: Vec::new(),
        };
        rig.settle();

        // The box shows which built-in program the chip runs.
        rig.click("blink");
        rig.click("blink (assembly)");
        let root = project.root.clone();
        assert!(
            root.join("blink.asm").is_file(),
            "written beside the circuit"
        );
        let app = &rig.app;
        assert!(
            app.project
                .as_ref()
                .unwrap()
                .contains(&root.join("blink.asm"))
        );
        assert!(
            app.active()
                .is_in_front(&DocTab::Source("blink.asm".into())),
            "shown for editing"
        );
        assert!(
            app.active()
                .doc
                .schematic()
                .sources
                .contains_key("blink.asm")
        );

        // On "Auto" it runs as a board would: the light changes every half
        // second of real time (later, on a busy machine).
        rig.click(" Run");
        assert!(rig.app.live.is_some(), "{:?}", rig.app.live_error);
        assert_eq!(rig.app.live_speed, 1.0);
        let mut states = Vec::new();
        let started = std::time::Instant::now();
        // Far sooner than the 12 s a change would take at four scope screens
        // a second, unless the machine is very busy.
        while states.len() < 3 && started.elapsed().as_secs() < 60 {
            std::thread::sleep(std::time::Duration::from_millis(30));
            rig.frame(vec![]);
            let lit = rig.app.lit_leds().len();
            if states.last() != Some(&lit) {
                states.push(lit);
            }
        }
        assert!(states.len() >= 3, "the light went on and off: {states:?}");
        // Opening the oscilloscope slows it, and closing it speeds it up.
        rig.app.bench.show_scope = true;
        rig.settle();
        assert!(rig.app.live_speed < 0.1);
        rig.app.bench.show_scope = false;
        rig.settle();
        assert_eq!(rig.app.live_speed, 1.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A project in a scratch folder with "Blink" (the Arduino blink example)
    /// and "Divider" (the first example), and the app with it open.
    struct Fixture {
        rig: Rig,
        root: PathBuf,
        blink: PathBuf,
        divider: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn example(name: &str) -> Schematic {
        (examples()
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no example {name}"))
            .build)()
    }

    fn fixture(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("dunbrec-tabs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let (mut project, _) = Project::create(&root.join("P")).unwrap();
        let blink = root.join("Blink.dbr");
        let divider = root.join("Divider.dbr");
        dunbrec_io::dbr::save(&example("Arduino blink"), &blink).unwrap();
        dunbrec_io::dbr::save(&(examples()[0].build)(), &divider).unwrap();
        project
            .add_files(&[blink.clone(), divider.clone()])
            .unwrap();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let app = DunbrecApp::new(&cc, None, Some(project.file.clone()));
        let mut rig = Rig {
            ctx,
            app,
            time: 0.0,
            seen: Vec::new(),
        };
        rig.settle();
        Fixture {
            rig,
            root,
            blink,
            divider,
        }
    }

    impl Fixture {
        fn open(&mut self, path: &std::path::Path) {
            self.rig.app.request(Pending::OpenPath(path.to_path_buf()));
            self.rig.settle();
        }

        fn open_both(&mut self) {
            self.open(&self.blink.clone());
            self.open(&self.divider.clone());
        }

        fn add_a_resistor(&mut self) {
            self.rig.app.active_mut().doc.edit(|s| {
                s.add_component(
                    dunbrec_core::ComponentKind::Resistor { resistance: 1e3 },
                    dunbrec_core::Point::new(60, 60),
                )
            });
            self.rig.settle();
        }

        fn parts(path: &std::path::Path) -> usize {
            dunbrec_io::dbr::load(path).unwrap().components.len()
        }

        fn chip(&self) -> dunbrec_core::ComponentId {
            self.rig
                .app
                .active()
                .doc
                .schematic()
                .components
                .iter()
                .find(|c| matches!(c.kind, dunbrec_core::ComponentKind::Mcu { .. }))
                .expect("a microcontroller")
                .id
        }

        /// Give the active circuit's chip its own assembly blink.
        fn give_the_chip_a_file(&mut self) {
            let request = DemoRequest {
                chip: self.chip(),
                demo: "blink",
                language: SourceLanguage::Asm,
            };
            let ctx = self.rig.ctx.clone();
            self.rig.app.use_demo(&ctx, request);
            self.rig.settle();
        }
    }

    #[test]
    fn each_open_circuit_has_a_tab_and_its_own_schematic() {
        let mut f = fixture("two");
        assert!(f.rig.open_names().is_empty());
        f.open_both();
        assert_eq!(f.rig.open_names(), ["Blink", "Divider"]);
        assert_eq!(f.rig.active_name(), "Divider", "the one just opened");
        let divider = f.rig.app.active().doc.schematic().clone();
        f.rig.click_circuit("Blink");
        assert_eq!(f.rig.active_name(), "Blink");
        assert_ne!(f.rig.app.active().doc.schematic(), &divider);
        assert_eq!(f.rig.app.title, "Blink — Dunbrec");
        assert!(!f.rig.app.file_tab_rects.is_empty(), "its second row");
        f.rig.click_circuit("Divider");
        assert_eq!(f.rig.app.active().doc.schematic(), &divider);
        // Opening one that is open goes to its tab.
        f.open(&f.blink.clone());
        assert_eq!(f.rig.open_names(), ["Blink", "Divider"]);
        assert_eq!(f.rig.active_name(), "Blink");
        // An example has no file, and is named after what it is.
        f.rig.app.request(Pending::Example(0));
        f.rig.settle();
        assert_eq!(f.rig.active_name(), examples()[0].name);
        assert_eq!(f.rig.open_names().len(), 3);
    }

    #[test]
    fn a_circuit_tab_cannot_be_dragged_out_of_the_row() {
        let mut f = fixture("drag");
        f.open_both();
        let before = format!("{:?}", f.rig.app.dock.iter_all_tabs().count());
        for to in [
            egui::pos2(800.0, 450.0),
            egui::pos2(1150.0, 450.0),
            egui::pos2(60.0, 60.0),
        ] {
            let from = f.rig.circuit_tab("Blink");
            f.rig.drag(from, to);
            let central = f.rig.app.dock.find_tab(&Pane::Workspace).unwrap();
            for id in f.rig.app.open_ids() {
                let at = f.rig.app.dock.find_tab(&Pane::Circuit(id)).unwrap();
                assert_eq!((at.0, at.1), (central.0, central.1), "dropped at {to:?}");
            }
            assert_eq!(
                format!("{:?}", f.rig.app.dock.iter_all_tabs().count()),
                before
            );
            assert_eq!(f.rig.app.dock.surfaces_count(), 1);
        }
    }

    #[test]
    fn only_the_active_circuit_runs() {
        let mut f = fixture("run");
        f.open_both();
        f.rig.click_circuit("Blink");
        f.rig.click(" Run");
        assert!(f.rig.app.live.is_some(), "{:?}", f.rig.app.live_error);
        // Its own tabs do not stop it.
        let at = f.rig.file_tab(&DocTab::Schematic);
        f.rig.click_at(at, egui::PointerButton::Primary);
        assert!(f.rig.app.live.is_some());
        f.rig.click_circuit("Divider");
        assert!(f.rig.app.live.is_none(), "stopped on leaving");
        f.rig.click_circuit("Blink");
        assert!(f.rig.app.live.is_none(), "and stays stopped");
    }

    #[test]
    fn instruments_follow_the_circuit_and_copies_cross_over() {
        let mut f = fixture("tools");
        f.open_both();
        f.rig.click_circuit("Blink");
        f.rig.app.bench.show_scope = true;
        f.rig.app.bench.scope_probes[0] = Some(dunbrec_core::Point::new(1, 2));
        f.rig.app.active_mut().select_all();
        f.rig.app.active_mut().copy();
        let copied = f.rig.app.active().doc.schematic().components.len();
        f.rig.settle();
        f.rig.click_circuit("Divider");
        assert!(f.rig.app.bench.show_scope, "the window stays up");
        assert_eq!(f.rig.app.bench.scope_probes[0], None, "other probes");
        let before = f.rig.app.active().doc.schematic().components.len();
        f.rig.app.active_mut().paste();
        assert_eq!(
            f.rig.app.active().doc.schematic().components.len(),
            before + copied
        );
        f.rig.app.bench.show_scope = false;
        f.rig.settle();
        f.rig.click_circuit("Blink");
        assert!(!f.rig.app.bench.show_scope);
        assert_eq!(
            f.rig.app.bench.scope_probes[0],
            Some(dunbrec_core::Point::new(1, 2))
        );
    }

    #[test]
    fn closing_a_tab_asks_only_about_that_circuit() {
        let mut f = fixture("close");
        f.open_both();
        // A saved circuit that is not in front just goes, and nothing else
        // changes.
        f.rig.click_circuit("Blink");
        f.rig.click(" Run");
        let middle = egui::PointerButton::Middle;
        f.rig.click_at(f.rig.circuit_tab("Divider"), middle);
        assert_eq!(f.rig.open_names(), ["Blink"]);
        assert!(f.rig.app.pending.is_none() && f.rig.app.live.is_some());
        // An unsaved one is brought to the front to be asked about.
        f.open(&f.divider.clone());
        f.add_a_resistor();
        f.rig.click_circuit("Blink");
        f.rig.click_at(f.rig.circuit_tab("Divider"), middle);
        assert_eq!(f.rig.active_name(), "Divider");
        assert!(f.rig.app.pending.is_some());
        f.rig.click("Keep editing");
        assert_eq!(f.rig.open_names(), ["Blink", "Divider"]);
        f.rig.click_at(f.rig.circuit_tab("Divider"), middle);
        f.rig.click("Discard changes");
        assert_eq!(f.rig.open_names(), ["Blink"]);
        // The last one leaves the central area empty.
        f.rig.click_at(f.rig.circuit_tab("Blink"), middle);
        assert!(f.rig.open_names().is_empty() && !f.rig.app.circuit_open);
        assert!(dock::is_open(&f.rig.app.dock, Pane::Workspace));
        assert_eq!(f.rig.app.title, "P — Dunbrec");
    }

    #[test]
    fn leaving_with_several_unsaved_circuits_asks_once_and_saves_them_all() {
        let mut f = fixture("quit");
        f.open_both();
        let (blink, divider) = (Fixture::parts(&f.blink), Fixture::parts(&f.divider));
        f.add_a_resistor();
        f.rig.click_circuit("Blink");
        f.add_a_resistor();
        f.rig.app.request(Pending::Quit);
        f.rig.settle();
        assert!(!f.rig.app.allow_close);
        f.rig.click("Cancel");
        assert!(f.rig.app.pending.is_none() && !f.rig.app.allow_close);
        f.rig.app.request(Pending::Quit);
        f.rig.settle();
        f.rig.click("Save all");
        assert!(f.rig.app.allow_close);
        assert_eq!(Fixture::parts(&f.blink), blink + 1);
        assert_eq!(Fixture::parts(&f.divider), divider + 1);
    }

    #[test]
    fn closing_the_project_closes_every_circuit() {
        let mut f = fixture("project");
        f.open_both();
        f.add_a_resistor();
        let parts = Fixture::parts(&f.divider);
        f.rig.app.request(Pending::CloseProject);
        f.rig.settle();
        // One unsaved circuit, so it is named.
        f.rig.click("Discard changes");
        assert!(f.rig.app.project.is_none() && !f.rig.app.circuit_open);
        assert_eq!(f.rig.app.circuits.len(), 1);
        assert_eq!(Fixture::parts(&f.divider), parts);
    }

    #[test]
    fn deleting_a_circuit_file_closes_its_tab() {
        let mut f = fixture("delete");
        f.open_both();
        f.rig.app.delete_in_project(&f.divider.clone());
        f.rig.settle();
        assert_eq!(f.rig.open_names(), ["Blink"]);
        assert!(!f.divider.exists());
    }

    #[test]
    fn a_source_file_has_a_named_tab_beside_its_schematic() {
        let mut f = fixture("files");
        f.open_both();
        f.rig.click_circuit("Blink");
        f.give_the_chip_a_file();
        let file = DocTab::Source("blink.asm".into());
        assert!(f.rig.app.active().is_in_front(&file));
        assert_eq!(
            source_title(f.rig.app.active().doc.schematic(), "blink.asm"),
            "U1: blink.asm"
        );
        let at = f.rig.file_tab(&DocTab::Schematic);
        f.rig.click_at(at, egui::PointerButton::Primary);
        assert!(f.rig.app.active().is_in_front(&DocTab::Schematic));
        let at = f.rig.file_tab(&file);
        f.rig.click_at(at, egui::PointerButton::Primary);
        assert!(f.rig.app.active().is_in_front(&file));
        // Closing the file's tab keeps the file with the circuit.
        f.rig
            .click_at(f.rig.file_tab(&file), egui::PointerButton::Middle);
        assert!(f.rig.app.active().tabs.find_tab(&file).is_none());
        assert!(
            f.rig
                .app
                .active()
                .doc
                .schematic()
                .sources
                .contains_key("blink.asm")
        );
        // The other circuit has no such tab; picking the file in the project
        // goes to the circuit that has it.
        f.rig.click_circuit("Divider");
        assert!(f.rig.app.file_tab_rects.iter().all(|(t, _)| *t != file));
        f.rig
            .app
            .run_project_command(project_ui::Command::OpenCircuitSource {
                circuit: f.blink.clone(),
                name: "blink.asm".into(),
            });
        f.rig.settle();
        assert_eq!(f.rig.active_name(), "Blink");
        assert!(f.rig.app.active().is_in_front(&file));
    }

    #[test]
    fn two_circuits_never_share_a_source_file() {
        let mut f = fixture("shared");
        f.open(&f.blink.clone());
        f.give_the_chip_a_file();
        assert!(f.rig.app.save(false));
        // A second circuit whose chip names the same file.
        let copy = f.root.join("Copy.dbr");
        std::fs::copy(&f.blink, &copy).unwrap();
        f.rig.app.project.as_mut().unwrap().add_file(&copy).unwrap();
        f.open(&copy);
        assert_eq!(f.rig.open_names(), ["Blink"], "asked first");
        f.rig.click("Cancel");
        assert!(f.rig.app.shared_ask.is_none());
        assert_eq!(f.rig.open_names(), ["Blink"]);

        f.open(&copy);
        f.rig.click("Make a copy");
        assert_eq!(f.rig.open_names(), ["Blink", "Copy"]);
        let circuit = f.rig.app.active();
        assert!(circuit.doc.is_dirty(), "the new name is not saved yet");
        assert!(!circuit.doc.can_undo(), "undo cannot share the file again");
        let sources: Vec<&String> = circuit.doc.schematic().sources.keys().collect();
        assert_eq!(sources, ["blink2.asm"]);
        assert_eq!(
            source_title(circuit.doc.schematic(), "blink2.asm"),
            "U1: blink2.asm"
        );
        let new = f.root.join("blink2.asm");
        assert!(new.is_file());
        assert!(f.rig.app.project.as_ref().unwrap().contains(&new));
        // Once saved, the circuit opens again without a question.
        assert!(f.rig.app.save(false));
        assert!(!f.rig.app.active().doc.is_dirty());
        let id = f.rig.app.active().id;
        f.rig.app.request(Pending::CloseCircuit(id));
        f.open(&copy);
        assert!(f.rig.app.shared_ask.is_none());
        assert_eq!(f.rig.open_names(), ["Blink", "Copy"]);
    }

    /// Draw frames until the run that is waiting has started or given up.
    fn wait_for_the_build(rig: &mut Rig) {
        let started = std::time::Instant::now();
        while rig.app.run_after_build.is_some() {
            assert!(started.elapsed().as_secs() < 180, "the build never ended");
            std::thread::sleep(std::time::Duration::from_millis(50));
            rig.frame(vec![]);
        }
        rig.settle();
    }

    /// Runs the real Arduino tool, and does nothing on a machine without it.
    #[test]
    fn run_builds_a_changed_sketch_before_it_starts() {
        if dunbrec_mcu::arduino::find_cli(None).is_none() {
            eprintln!("arduino-cli not installed: skipped");
            return;
        }
        let mut f = fixture("autobuild");
        f.open(&f.blink.clone());
        let chip = f.chip();
        let ctx = f.rig.ctx.clone();
        f.rig.app.use_demo(
            &ctx,
            DemoRequest {
                chip,
                demo: "blink",
                language: SourceLanguage::Cpp,
            },
        );
        f.rig.settle();
        let set = |f: &mut Fixture, from: &str, to: &str| {
            f.rig.app.active_mut().doc.edit(|s| {
                let file = s.sources.get_mut("blink.ino").unwrap();
                assert!(file.text.contains(from), "{from}");
                file.text = file.text.replacen(from, to, 1);
            });
            f.rig.settle();
        };
        // A change, then straight to Run (a time no other test builds).
        set(&mut f, "ON_MS = 500", "ON_MS = 137");
        f.rig.click(" Run");
        assert!(f.rig.app.live.is_none(), "not before it is built");
        assert!(f.rig.app.run_after_build.is_some());
        wait_for_the_build(&mut f.rig);
        assert!(f.rig.app.live.is_some(), "{:?}", f.rig.app.live_error);

        // A mistake: nothing starts, and the file with it is shown.
        f.rig.app.stop_sim();
        set(&mut f, "digitalWrite(LED_PIN, HIGH);", "oops();");
        let at = f.rig.file_tab(&DocTab::Schematic);
        f.rig.click_at(at, egui::PointerButton::Primary);
        f.rig.click(" Run");
        wait_for_the_build(&mut f.rig);
        assert!(f.rig.app.live.is_none());
        let why = f.rig.app.live_error.clone().expect("said");
        assert!(
            why.contains("blink.ino did not build") && why.contains("oops"),
            "{why}"
        );
        assert!(
            f.rig
                .app
                .active()
                .is_in_front(&DocTab::Source("blink.ino".into()))
        );
        assert!(f.rig.app.message.is_some());
        f.rig.click("OK");

        // Corrected, and Run again; then a run that is called off.
        set(&mut f, "oops();", "digitalWrite(LED_PIN, HIGH);");
        set(&mut f, "ON_MS = 137", "ON_MS = 139");
        f.rig.click(" Run");
        assert!(f.rig.app.run_after_build.is_some());
        f.rig.click(" Stop");
        assert!(f.rig.app.run_after_build.is_none() && f.rig.app.live.is_none());
        f.rig.click(" Run");
        wait_for_the_build(&mut f.rig);
        assert!(f.rig.app.live.is_some(), "{:?}", f.rig.app.live_error);
    }

    /// Whether some label on screen is exactly `label`.
    fn shows(f: &Fixture, label: &str) -> bool {
        f.rig.seen.iter().any(|(text, _)| text == label)
    }

    fn double_click(rig: &mut Rig, label: &str) {
        let at = rig
            .seen
            .iter()
            .find(|(text, _)| text == label)
            .unwrap_or_else(|| panic!("nothing labelled {label:?} is shown"))
            .1
            .center();
        let primary = egui::PointerButton::Primary;
        rig.frame(vec![egui::Event::PointerMoved(at)]);
        for pressed in [true, false, true, false] {
            rig.frame(vec![Rig::button(at, primary, pressed)]);
        }
        rig.settle();
    }

    fn firmware_of(f: &Fixture) -> String {
        let code = f.rig.app.active().code().expect("a circuit file");
        code.chips[0].firmware.clone()
    }

    fn set_firmware(f: &mut Fixture, to: &str) {
        let chip = f.chip();
        f.rig.app.active_mut().doc.edit(|s| {
            if let dunbrec_core::ComponentKind::Mcu { firmware, .. } =
                &mut s.component_mut(chip).unwrap().kind
            {
                *firmware = to.into();
            }
        });
        f.rig.settle();
    }

    #[test]
    fn the_project_panel_follows_what_the_chips_run() {
        let mut f = fixture("panel");
        f.open(&f.blink.clone());
        f.give_the_chip_a_file();
        // Under its circuit at once, before anything is saved.
        assert!(shows(&f, "📝 U1: blink.asm"));
        assert!(!shows(&f, "Not used"));
        // Without its chip, the file is no circuit's.
        let chip = f.chip();
        f.rig.app.active_mut().canvas.selection = [Item::Component(chip)].into_iter().collect();
        f.rig.app.active_mut().delete_selection();
        f.rig.settle();
        assert!(shows(&f, "Not used") && shows(&f, "📝 blink.asm"));
        assert!(!shows(&f, "📝 U1: blink.asm"));
        f.rig.app.active_mut().doc.undo();
        f.rig.settle();
        assert!(shows(&f, "📝 U1: blink.asm") && !shows(&f, "Not used"));
        // A chip pointed at a file nothing lists: listed, and given a tab.
        std::fs::write(f.root.join("typed.asm"), "nop\n").unwrap();
        set_firmware(&mut f, "typed.asm");
        let project = f.rig.app.project.as_ref().unwrap();
        assert!(project.contains(&f.root.join("typed.asm")));
        assert!(
            f.rig
                .app
                .active()
                .doc
                .schematic()
                .sources
                .contains_key("typed.asm")
        );
        assert!(shows(&f, "📝 U1: typed.asm"));
        assert!(shows(&f, "📝 blink.asm"), "now under Not used");
        assert!(f.rig.app.shared_edit.is_none());
        // Double-clicking a file shows its tab, from another circuit too.
        f.open(&f.divider.clone());
        double_click(&mut f.rig, "📝 U1: typed.asm");
        assert_eq!(f.rig.active_name(), "Blink");
        assert!(
            f.rig
                .app
                .active()
                .is_in_front(&DocTab::Source("typed.asm".into()))
        );
        // A file the disk has lost shows as such.
        std::fs::remove_file(f.root.join("typed.asm")).unwrap();
        f.rig.settle();
        assert!(shows(&f, "⚠ U1: typed.asm"));
    }

    #[test]
    fn an_unused_file_is_given_to_the_chip_the_user_picks() {
        let mut f = fixture("assign");
        std::fs::write(f.root.join("spare.asm"), "nop\n").unwrap();
        let spare = f.root.join("spare.asm");
        f.rig
            .app
            .project
            .as_mut()
            .unwrap()
            .add_file(&spare)
            .unwrap();
        f.rig.settle();
        // No circuit is open: choosing one opens it.
        double_click(&mut f.rig, "📝 spare.asm");
        assert!(f.rig.app.assign_ask.is_some());
        f.rig.click("Cancel");
        assert!(f.rig.app.assign_ask.is_none() && !f.rig.app.circuit_open);
        double_click(&mut f.rig, "📝 spare.asm");
        f.rig.click("Use it");
        assert_eq!(f.rig.open_names(), ["Blink"]);
        assert_eq!(firmware_of(&f), "spare.asm");
        assert!(f.rig.app.active().doc.is_dirty());
        assert!(
            f.rig
                .app
                .active()
                .is_in_front(&DocTab::Source("spare.asm".into()))
        );
        assert!(shows(&f, "📝 U1: spare.asm") && !shows(&f, "Not used"));
        assert!(f.rig.app.shared_edit.is_none());
    }

    #[test]
    fn unsaved_text_moves_with_a_file_to_another_circuit() {
        let mut f = fixture("move");
        f.open(&f.blink.clone());
        f.give_the_chip_a_file();
        assert!(f.rig.app.save(false));
        // Edit it, then stop using it, without saving.
        f.rig
            .app
            .active_mut()
            .doc
            .edit(|s| s.sources.get_mut("blink.asm").unwrap().text = "; edited\nnop\n".into());
        set_firmware(&mut f, "builtin:blink");
        let other = f.root.join("Other.dbr");
        dunbrec_io::dbr::save(&example("Arduino blink"), &other).unwrap();
        f.rig
            .app
            .project
            .as_mut()
            .unwrap()
            .add_file(&other)
            .unwrap();
        let chip = f.chip();
        f.rig.app.assign(&f.root.join("blink.asm"), other, chip);
        f.rig.settle();
        assert_eq!(f.rig.active_name(), "Other");
        assert_eq!(
            f.rig.app.active().doc.schematic().sources["blink.asm"].text,
            "; edited\nnop\n"
        );
        let first = &f.rig.app.circuits[0];
        assert!(!first.doc.schematic().sources.contains_key("blink.asm"));
    }

    #[test]
    fn a_chip_given_another_circuits_file_is_asked_about() {
        let mut f = fixture("steal");
        f.open(&f.blink.clone());
        f.give_the_chip_a_file();
        assert!(f.rig.app.save(false));
        let id = f.rig.app.active().id;
        f.rig.app.request(Pending::CloseCircuit(id));
        // The owner is closed: its file says it uses blink.asm.
        let other = f.root.join("Other.dbr");
        dunbrec_io::dbr::save(&example("Arduino blink"), &other).unwrap();
        f.rig
            .app
            .project
            .as_mut()
            .unwrap()
            .add_file(&other)
            .unwrap();
        f.open(&other);
        set_firmware(&mut f, "blink.asm");
        assert!(f.rig.app.shared_edit.is_some());
        f.rig.click("Undo the change");
        assert_eq!(firmware_of(&f), "builtin:blink");
        assert!(f.rig.app.shared_edit.is_none());
        set_firmware(&mut f, "blink.asm");
        f.rig.click("Make a copy");
        assert_eq!(firmware_of(&f), "blink2.asm");
        assert!(f.root.join("blink2.asm").is_file());
        assert!(f.rig.app.shared_edit.is_none());
        assert!(shows(&f, "📝 U1: blink2.asm") && shows(&f, "📝 U1: blink.asm"));
    }

    #[test]
    fn circuits_that_already_share_a_file_can_still_open() {
        let mut f = fixture("old-share");
        std::fs::write(f.root.join("both.asm"), "nop\n").unwrap();
        let mut s = example("Arduino blink");
        for c in &mut s.components {
            if let dunbrec_core::ComponentKind::Mcu { firmware, .. } = &mut c.kind {
                *firmware = "both.asm".into();
            }
        }
        let other = f.root.join("Other.dbr");
        dunbrec_io::dbr::save(&s, &f.blink).unwrap();
        dunbrec_io::dbr::save(&s, &other).unwrap();
        let project = f.rig.app.project.as_mut().unwrap();
        project.add_file(&other).unwrap();
        f.rig.app.refresh_project();
        f.rig.settle();
        assert!(shows(&f, "📝 U1: both.asm") && !shows(&f, "⚠ U1: both.asm"));
        f.open(&f.blink.clone());
        assert!(f.rig.app.shared_ask.is_some());
        f.rig.click("Open as it is");
        assert_eq!(f.rig.open_names(), ["Blink"]);
        assert!(!f.rig.app.active().doc.is_dirty());
        assert!(shows(&f, "⚠ U1: both.asm"), "marked once open");
        // The other cannot also be open without its own copy.
        f.open(&other);
        assert!(!shows(&f, "Open as it is"));
        f.rig.click("Make a copy");
        assert_eq!(f.rig.open_names(), ["Blink", "Other"]);
        assert_eq!(firmware_of(&f), "both2.asm");
        assert!(f.rig.app.shared_edit.is_none());
    }

    #[test]
    fn deleting_a_used_file_names_the_chips_that_use_it() {
        let mut f = fixture("warn");
        f.open(&f.blink.clone());
        f.give_the_chip_a_file();
        f.rig
            .app
            .run_project_command(project_ui::Command::AskDelete(f.root.join("blink.asm")));
        f.rig.settle();
        assert!(shows(&f, "U1 in Blink uses blink.asm."), "{:?}", f.rig.seen);
        f.rig.click("Cancel");
        assert!(f.root.join("blink.asm").is_file());
    }

    #[test]
    fn nothing_is_reported_about_circuits_that_are_not_open() {
        let mut f = fixture("quiet");
        // A circuit whose chip names a file that is not there.
        let mut s = example("Arduino blink");
        for c in &mut s.components {
            if let dunbrec_core::ComponentKind::Mcu { firmware, .. } = &mut c.kind {
                *firmware = "lost.asm".into();
            }
        }
        dunbrec_io::dbr::save(&s, &f.blink).unwrap();
        f.rig.app.refresh_project();
        f.rig.settle();
        assert!(shows(&f, "📝 U1: lost.asm") && !shows(&f, "⚠ U1: lost.asm"));
        // The default layout shows Checks and Values: they have no circuit.
        assert!(shows(&f, "No circuit open"));
        let complains = |f: &Fixture| {
            f.rig.seen.iter().any(|(text, _)| {
                text.contains("Place some parts") || text.contains("wiring problems")
            })
        };
        assert!(!complains(&f), "{:?}", f.rig.seen);
        // Opening it says what is wrong.
        f.open(&f.blink.clone());
        assert!(f.rig.app.message.is_some());
        f.rig.click("OK");
        assert!(shows(&f, "⚠ U1: lost.asm"));
        assert!(!shows(&f, "No circuit open"));
    }

    #[test]
    fn a_missing_file_can_be_removed_from_the_panel() {
        let mut f = fixture("lost");
        f.open(&f.blink.clone());
        f.give_the_chip_a_file();
        assert!(f.rig.app.save(false));
        let id = f.rig.app.active().id;
        f.rig.app.request(Pending::CloseCircuit(id));
        std::fs::remove_file(f.root.join("blink.asm")).unwrap();
        f.open(&f.blink.clone());
        f.rig.click("OK");
        let secondary = egui::PointerButton::Secondary;
        let row = |f: &Fixture| {
            f.rig
                .seen
                .iter()
                .find(|(text, _)| text == "⚠ U1: blink.asm")
                .expect("the missing file is shown")
                .1
                .center()
        };
        f.rig.click_at(row(&f), secondary);
        f.rig.click("Remove from project");
        assert!(shows(&f, "U1 in Blink uses blink.asm."), "{:?}", f.rig.seen);
        assert!(shows(&f, "Nothing is deleted from the disk."));
        f.rig.click("Remove");
        assert!(f.rig.app.message.is_none(), "{:?}", f.rig.app.message);
        assert_eq!(firmware_of(&f), "");
        assert!(!shows(&f, "⚠ U1: blink.asm") && !shows(&f, "Not used"));
        assert!(
            !f.rig
                .app
                .project
                .as_ref()
                .unwrap()
                .contains(&f.root.join("blink.asm"))
        );
        assert!(f.rig.app.active().doc.is_dirty());
        f.rig.app.active_mut().doc.undo();
        f.rig.settle();
        assert_eq!(firmware_of(&f), "blink.asm");
    }

    #[test]
    fn deleting_a_file_leaves_no_closed_circuit_asking_for_it() {
        let mut f = fixture("forget");
        f.open(&f.blink.clone());
        f.give_the_chip_a_file();
        assert!(f.rig.app.save(false));
        let id = f.rig.app.active().id;
        f.rig.app.request(Pending::CloseCircuit(id));
        f.rig.app.delete_in_project(&f.root.join("blink.asm"));
        f.rig.settle();
        assert!(!f.root.join("blink.asm").exists());
        let saved = dunbrec_io::dbr::load(&f.blink).unwrap();
        let code = project::code_of(&saved, &f.blink);
        assert_eq!(code.chips[0].firmware, "");
        f.open(&f.blink.clone());
        assert!(f.rig.app.message.is_none());
    }

    /// Right-click the row labelled `label`.
    fn menu_of(f: &mut Fixture, label: &str) {
        let at = f
            .rig
            .seen
            .iter()
            .find(|(text, _)| text == label)
            .unwrap_or_else(|| panic!("no row {label:?}: {:?}", f.rig.seen))
            .1
            .center();
        f.rig.click_at(at, egui::PointerButton::Secondary);
    }

    /// "Blink" with its own assembly file, saved and closed.
    fn blink_with_a_file(tag: &str) -> Fixture {
        let mut f = fixture(tag);
        f.open(&f.blink.clone());
        f.give_the_chip_a_file();
        assert!(f.rig.app.save(false));
        let id = f.rig.app.active().id;
        f.rig.app.request(Pending::CloseCircuit(id));
        f.rig.settle();
        f
    }

    #[test]
    fn circuits_and_source_files_have_the_same_menu() {
        let mut f = blink_with_a_file("menus");
        for row in ["⚡ Blink", "📝 U1: blink.asm"] {
            menu_of(&mut f, row);
            for item in [
                "Open",
                "Rename…",
                "Remove from project",
                "Delete from disk…",
            ] {
                assert!(shows(&f, item), "{row}: {item}");
            }
            for gone in ["New", "Delete", "Folder…", "Add existing file…"] {
                assert!(!shows(&f, gone), "{row}: {gone}");
            }
            f.rig
                .click_at(egui::pos2(900.0, 500.0), egui::PointerButton::Primary);
        }
    }

    #[test]
    fn deleting_a_circuit_deletes_its_source_files() {
        let mut f = blink_with_a_file("delete-all");
        // One file another circuit also uses, and one no circuit uses.
        std::fs::write(f.root.join("spare.asm"), "nop\n").unwrap();
        let spare = f.root.join("spare.asm");
        f.rig
            .app
            .project
            .as_mut()
            .unwrap()
            .add_file(&spare)
            .unwrap();
        f.rig.settle();
        menu_of(&mut f, "⚡ Blink");
        f.rig.click("Delete from disk…");
        assert!(shows(
            &f,
            "Delete the circuit “Blink” and its source files?"
        ));
        assert!(shows(&f, "•  blink.asm"), "{:?}", f.rig.seen);
        assert!(shows(
            &f,
            "They go to the Recycle Bin, so you can get them back."
        ));
        f.rig.click("Cancel");
        assert!(f.blink.is_file() && f.root.join("blink.asm").is_file());
        menu_of(&mut f, "⚡ Blink");
        f.rig.click("Delete from disk…");
        f.rig.click("Delete");
        assert!(!f.blink.exists() && !f.root.join("blink.asm").exists());
        assert!(spare.is_file(), "not the circuit's");
        let project = f.rig.app.project.as_ref().unwrap();
        assert!(!project.contains(&f.root.join("blink.asm")) && project.contains(&spare));
        assert!(f.rig.app.message.is_none(), "{:?}", f.rig.app.message);
    }

    #[test]
    fn a_shared_file_outlives_one_of_its_circuits() {
        let mut f = blink_with_a_file("delete-shared");
        let twin = f.root.join("Twin.dbr");
        std::fs::copy(&f.blink, &twin).unwrap();
        f.rig.app.project.as_mut().unwrap().add_file(&twin).unwrap();
        f.rig.settle();
        f.rig
            .app
            .run_project_command(project_ui::Command::AskDelete(f.blink.clone()));
        f.rig.settle();
        assert!(shows(&f, "Delete the circuit “Blink”?"), "{:?}", f.rig.seen);
        assert!(shows(&f, "•  blink.asm  (Twin)"));
        f.rig.click("Delete");
        assert!(!f.blink.exists() && f.root.join("blink.asm").is_file());
    }

    #[test]
    fn removing_a_circuit_takes_its_files_out_and_leaves_the_disk_alone() {
        let mut f = blink_with_a_file("remove-circuit");
        menu_of(&mut f, "⚡ Blink");
        f.rig.click("Remove from project");
        assert!(f.rig.app.confirm_delete.is_none(), "nothing to ask");
        let source = f.root.join("blink.asm");
        assert!(f.blink.is_file() && source.is_file());
        let project = f.rig.app.project.as_ref().unwrap();
        assert!(!project.contains(&f.blink) && !project.contains(&source));
        assert!(!shows(&f, "Not used"));
    }

    #[test]
    fn a_new_file_is_made_from_the_chips_properties() {
        let mut f = fixture("new-for-chip");
        // A chip without a file is nothing to show in the project.
        let under_blink = |f: &Fixture| {
            f.rig
                .seen
                .iter()
                .any(|(text, _)| text.contains("U1:") || text.contains("no program"))
        };
        assert!(!under_blink(&f), "{:?}", f.rig.seen);
        f.open(&f.blink.clone());
        let chip = f.chip();
        f.rig.app.active_mut().canvas.selection = [Item::Component(chip)].into_iter().collect();
        f.rig.settle();
        for (item, file) in [
            ("New assembly file…", "main.asm"),
            ("New Arduino sketch…", "sketch.ino"),
        ] {
            f.rig.click("Source file");
            f.rig.click(item);
            assert!(shows(&f, "Name of the new file for U1 in Blink:"));
            f.rig.click("Create");
            assert_eq!(firmware_of(&f), file);
            assert!(f.root.join(file).is_file());
            assert!(
                f.rig
                    .app
                    .project
                    .as_ref()
                    .unwrap()
                    .contains(&f.root.join(file))
            );
            assert!(f.rig.app.active().is_in_front(&DocTab::Source(file.into())));
            assert!(shows(&f, &format!("📝 U1: {file}")));
            assert!(f.rig.app.shared_edit.is_none() && f.rig.app.message.is_none());
        }
        // Removing the file leaves just the circuit.
        menu_of(&mut f, "📝 U1: sketch.ino");
        f.rig.click("Remove from project");
        f.rig.click("Remove");
        assert!(!shows(&f, "📝 U1: sketch.ino") && !under_blink(&f) || shows(&f, "Not used"));
        assert_eq!(firmware_of(&f), "");
        assert!(f.root.join("sketch.ino").is_file());
        // A circuit with no file yet keeps the new one until it is saved.
        f.rig.app.request(Pending::Example(8));
        f.rig.settle();
        let chip = f.chip();
        f.rig.app.new_source_for(chip, SourceLanguage::Asm);
        assert!(f.rig.app.name_prompt.is_none());
        assert!(
            f.rig
                .app
                .active()
                .doc
                .schematic()
                .sources
                .contains_key("main.asm")
        );
    }

    fn ctrl_tab(rig: &mut Rig, shift: bool) {
        let modifiers = Modifiers {
            ctrl: true,
            command: true,
            shift,
            ..Default::default()
        };
        for pressed in [true, false] {
            rig.frame(vec![egui::Event::Key {
                key: Key::Tab,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            }]);
        }
        rig.settle();
    }

    /// "Blink", "Divider" and an example, in that order, the last in front.
    fn three_open(tag: &str) -> (Fixture, String) {
        let mut f = fixture(tag);
        f.open_both();
        f.rig.app.request(Pending::Example(1));
        f.rig.settle();
        let third = f.rig.active_name();
        (f, third)
    }

    #[test]
    fn ctrl_tab_goes_round_the_circuits_in_tab_order() {
        let (mut f, third) = three_open("cycle");
        ctrl_tab(&mut f.rig, false);
        assert_eq!(f.rig.active_name(), "Blink", "round from the last");
        ctrl_tab(&mut f.rig, false);
        assert_eq!(f.rig.active_name(), "Divider");
        ctrl_tab(&mut f.rig, true);
        ctrl_tab(&mut f.rig, true);
        assert_eq!(f.rig.active_name(), third, "and back round");
        // The order is the tabs', wherever they have been dragged.
        let from = f.rig.circuit_tab(&third);
        let to = f.rig.circuit_tab("Blink") - egui::vec2(20.0, 0.0);
        f.rig.drag(from, to);
        assert_eq!(
            dock::circuit_order(&f.rig.app.dock)[0],
            f.rig.app.active().id
        );
        ctrl_tab(&mut f.rig, false);
        assert_eq!(f.rig.active_name(), "Blink");
    }

    #[test]
    fn ctrl_tab_leaves_the_text_alone_stops_the_run_and_waits_for_questions() {
        let mut f = fixture("cycle-typing");
        f.open_both();
        f.rig.click_circuit("Blink");
        f.give_the_chip_a_file();
        let text = f.rig.app.active().doc.schematic().sources["blink.asm"]
            .text
            .clone();
        // Put the cursor in the source file.
        f.rig
            .click_at(egui::pos2(700.0, 400.0), egui::PointerButton::Primary);
        assert!(f.rig.ctx.wants_keyboard_input(), "typing");
        ctrl_tab(&mut f.rig, false);
        assert_eq!(f.rig.active_name(), "Divider");
        ctrl_tab(&mut f.rig, false);
        assert_eq!(
            f.rig.app.active().doc.schematic().sources["blink.asm"].text,
            text
        );
        f.rig.click(" Run");
        assert!(f.rig.app.live.is_some(), "{:?}", f.rig.app.live_error);
        ctrl_tab(&mut f.rig, false);
        assert!(f.rig.app.live.is_none());
        // Not while a question is up.
        f.add_a_resistor();
        f.rig.app.request(Pending::Quit);
        f.rig.settle();
        let before = f.rig.active_name();
        ctrl_tab(&mut f.rig, false);
        assert_eq!(f.rig.active_name(), before);
    }

    #[test]
    fn the_window_menu_lists_the_circuits_and_goes_to_them() {
        let (mut f, third) = three_open("window");
        f.rig.click("Window");
        for item in [
            "Next circuit   Ctrl+Tab",
            "Previous circuit   Ctrl+Shift+Tab",
            "Close all but this one",
            "Close all circuits",
            "Blink",
            "Divider",
        ] {
            assert!(shows(&f, item), "{item}: {:?}", f.rig.seen);
        }
        assert!(shows(&f, &third));
        f.rig.click("Divider");
        assert_eq!(f.rig.active_name(), "Divider");
        f.rig.click("Window");
        f.rig.click("Close all but this one");
        assert_eq!(f.rig.open_names(), ["Divider"]);
        f.rig.click("Window");
        f.rig.click("Close all circuits");
        assert!(f.rig.open_names().is_empty());
        f.rig.click("Window");
        assert!(shows(&f, "No circuit open"));
    }

    #[test]
    fn a_tab_menu_closes_the_others_or_all_asking_once() {
        let (mut f, third) = three_open("tab-menu");
        let secondary = egui::PointerButton::Secondary;
        // Two unsaved circuits; keeping one of them asks only about the other.
        f.rig.click_circuit("Blink");
        f.add_a_resistor();
        f.rig.click_circuit("Divider");
        f.add_a_resistor();
        let at = f.rig.circuit_tab("Divider");
        f.rig.click_at(at, secondary);
        f.rig.click("Close all but this one");
        assert!(shows(
            &f,
            "“Blink” has unsaved changes. Keep editing or discard those changes?"
        ));
        f.rig.click("Keep editing");
        assert_eq!(f.rig.open_names().len(), 3);
        // Closing all asks about both together.
        let at = f.rig.circuit_tab(&third);
        f.rig.click_at(at, secondary);
        f.rig.click("Close all");
        assert!(shows(&f, "These circuits have unsaved changes:"));
        f.rig.click("Cancel");
        assert_eq!(f.rig.open_names().len(), 3);
        let blink = Fixture::parts(&f.blink);
        let at = f.rig.circuit_tab(&third);
        f.rig.click_at(at, secondary);
        f.rig.click("Close all");
        f.rig.click("Save all");
        assert!(f.rig.open_names().is_empty() && !f.rig.app.circuit_open);
        assert_eq!(Fixture::parts(&f.blink), blink + 1);
    }

    /// What the bar along the bottom shows in a window `width` wide: each
    /// text and where it starts and ends.
    fn bottom_bar(app: &DunbrecApp, width: f32) -> Vec<(String, f64, f64)> {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut shown = Vec::new();
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 600.0),
                )),
                ..Default::default()
            };
            let out = ctx.run(input, |ctx| app.status_bar(ctx));
            if let Some(update) = out.platform_output.accesskit_update {
                shown = update
                    .nodes
                    .iter()
                    // Not the runs of text inside each label, which are
                    // placed as if every line began at its left edge.
                    .filter(|(_, n)| n.role() != egui::accesskit::Role::TextRun)
                    .filter_map(|(_, n)| {
                        let text = n.label().or_else(|| n.value())?.to_string();
                        let b = n.bounds()?;
                        Some((text, b.x0, b.x1))
                    })
                    .collect();
            }
        }
        shown
    }

    #[test]
    fn the_bottom_bar_says_only_what_helps() {
        let mut f = fixture("status");
        assert!(bottom_bar(&f.rig.app, 1000.0).is_empty(), "no circuit");
        f.open(&f.blink.clone());
        let selecting = bottom_bar(&f.rig.app, 1000.0);
        assert_eq!(selecting.len(), 1, "{selecting:?}");
        assert_eq!(selecting[0].0, "100%");
        f.rig.app.active_mut().canvas.tool = Tool::Wire;
        for width in [300.0, 1000.0] {
            let shown = bottom_bar(&f.rig.app, width);
            assert_eq!(shown.len(), 2, "{shown:?}");
            let of = |start: &str| {
                shown
                    .iter()
                    .find(|(text, ..)| text.starts_with(start))
                    .unwrap_or_else(|| panic!("no {start:?} at {width}: {shown:?}"))
            };
            let (hint, zoom) = (of("Wire: click to start"), of("100%"));
            assert!(hint.2 <= zoom.1, "{width}: {shown:?}");
            assert!(zoom.2 <= f64::from(width), "{width}: {shown:?}");
        }
    }

    #[test]
    fn the_toolbar_is_large_and_all_of_it_fits_a_narrow_window() {
        // At the middle size, which is what a new user gets.
        let mut f = fixture("toolbar");
        f.open(&f.blink.clone());
        for width in [640.0_f32, 1600.0] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut shown: Vec<(String, egui::accesskit::Rect)> = Vec::new();
            for _ in 0..4 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 600.0),
                    )),
                    ..Default::default()
                };
                let app = &mut f.rig.app;
                let out = ctx.run(input, |ctx| app.run_toolbar(ctx));
                if let Some(update) = out.platform_output.accesskit_update {
                    shown = update
                        .nodes
                        .iter()
                        .filter(|(_, n)| n.role() != egui::accesskit::Role::TextRun)
                        .filter_map(|(_, n)| {
                            let text = n.label().or_else(|| n.value())?.to_string();
                            Some((text, n.bounds()?))
                        })
                        .collect();
                }
            }
            let of = |end: &str| {
                shown
                    .iter()
                    .find(|(text, _)| text.ends_with(end))
                    .unwrap_or_else(|| panic!("no {end:?} at {width}: {shown:?}"))
                    .1
            };
            let run = of(" Run");
            assert!(run.y1 - run.y0 >= 24.0, "{run:?}");
            for end in [
                " Run",
                "Speed",
                "Oscilloscope",
                "Multimeter",
                "Function gen",
            ] {
                let r = of(end);
                assert!(
                    r.x0 >= 0.0 && r.x1 <= f64::from(width),
                    "{width}: {end} {r:?}"
                );
            }
            // One line when there is room, more when there is not.
            let wrapped = of("Function gen").y0 > run.y1;
            assert_eq!(wrapped, width < 1000.0, "{width}: {shown:?}");
        }
    }

    /// How tall the thing labelled exactly or lastly `label` is drawn.
    fn height_of(rig: &Rig, label: &str) -> f32 {
        rig.seen
            .iter()
            .find(|(text, _)| text == label)
            .or_else(|| rig.seen.iter().find(|(text, _)| text.starts_with(label)))
            .unwrap_or_else(|| panic!("no {label:?}: {:?}", rig.seen))
            .1
            .height()
    }

    #[test]
    fn the_chosen_size_changes_the_menus_and_the_toolbar() {
        let mut f = fixture("bar-size");
        f.open(&f.blink.clone());
        // The menu bar, an open menu, a menu within it, and the toolbar.
        let heights: Vec<[f32; 4]> = BarSize::ALL
            .into_iter()
            .map(|size| {
                f.rig.app.settings.bar_size = size;
                f.rig.settle();
                let bar = height_of(&f.rig, "File");
                let run = f
                    .rig
                    .seen
                    .iter()
                    .find(|(text, _)| text.ends_with(" Run"))
                    .unwrap()
                    .1
                    .height();
                f.rig.click("File");
                let item = height_of(&f.rig, "Open…");
                // Its label ends with the arrow that marks a menu.
                let opener = f
                    .rig
                    .seen
                    .iter()
                    .find(|(text, _)| text.starts_with("Open recent"))
                    .unwrap_or_else(|| panic!("no recent menu: {:?}", f.rig.seen))
                    .1
                    .center();
                f.rig.click_at(opener, egui::PointerButton::Primary);
                let within = height_of(&f.rig, "Clear");
                f.rig.frame(vec![egui::Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }]);
                f.rig.settle();
                [bar, item, within, run]
            })
            .collect();
        for part in 0..4 {
            let [small, medium, large] = [heights[0][part], heights[1][part], heights[2][part]];
            assert!(small < medium && medium < large, "{part}: {heights:?}");
        }
        // A menu within a menu is enlarged once, like the one it is in.
        for [_, item, within, _] in &heights {
            assert!((item - within).abs() < 1.0, "{heights:?}");
        }
    }

    #[test]
    fn the_size_is_chosen_in_the_settings() {
        let mut f = fixture("bar-setting");
        f.rig.app.settings_dialog.open();
        f.rig.settle();
        assert!(shows(&f, "Menu and toolbar size"));
        f.rig.click("Medium");
        f.rig.click("Large");
        assert_eq!(f.rig.app.settings.bar_size, BarSize::Large);
    }

    #[test]
    fn every_tool_but_selecting_has_a_hint() {
        assert_eq!(tool_hint(&Tool::Select), None);
        for tool in [Tool::Wire, Tool::Label, Tool::Probe, junction_tool()] {
            assert!(tool_hint(&tool).is_some_and(|h| h.ends_with("Esc to stop.")));
        }
    }

    /// Change the active circuit's source file `name` as typing would.
    fn type_into(f: &mut Fixture, name: &str, line: &str) {
        f.rig.app.active_mut().doc.edit(|s| {
            s.sources.get_mut(name).unwrap().text.push_str(line);
        });
        f.rig.settle();
    }

    #[test]
    fn run_saves_the_circuit_and_its_files_first() {
        let mut f = fixture("save-run");
        f.open(&f.blink.clone());
        f.give_the_chip_a_file();
        type_into(&mut f, "blink.asm", "; typed\n");
        assert!(f.rig.app.active().doc.is_dirty());
        f.rig.click(" Run");
        assert!(f.rig.app.live.is_some(), "{:?}", f.rig.app.live_error);
        assert!(!f.rig.app.active().doc.is_dirty());
        let file = f.root.join("blink.asm");
        let on_disk = || std::fs::read_to_string(&file).unwrap();
        assert!(on_disk().ends_with("; typed\n"));
        let saved = dunbrec_io::dbr::load(&f.blink).unwrap();
        assert_eq!(
            project::code_of(&saved, &f.blink).chips[0].firmware,
            "blink.asm"
        );
        // A restart the app makes by itself, for new wiring, saves nothing.
        let parts = Fixture::parts(&f.blink);
        f.add_a_resistor();
        f.rig.settle();
        assert!(f.rig.app.active().doc.is_dirty());
        assert_eq!(Fixture::parts(&f.blink), parts);
        // Restart is the user's doing.
        f.rig.app.run_pressed(false);
        f.rig.settle();
        assert_eq!(Fixture::parts(&f.blink), parts + 1);
    }

    #[test]
    fn build_saves_first_even_if_the_build_then_fails() {
        let mut f = fixture("save-build");
        f.open(&f.blink.clone());
        let chip = f.chip();
        let ctx = f.rig.ctx.clone();
        f.rig.app.use_demo(
            &ctx,
            DemoRequest {
                chip,
                demo: "blink",
                language: SourceLanguage::Cpp,
            },
        );
        // The build the demo starts has to be over for the button to work.
        let started = std::time::Instant::now();
        while f.rig.app.active().code.is_building() {
            assert!(started.elapsed().as_secs() < 180, "the build never ended");
            std::thread::sleep(std::time::Duration::from_millis(50));
            f.rig.frame(vec![]);
        }
        type_into(&mut f, "blink.ino", "this is not C++\n");
        assert!(f.rig.app.active().doc.is_dirty());
        f.rig.click("Build");
        assert!(!f.rig.app.active().doc.is_dirty());
        let on_disk = std::fs::read_to_string(f.root.join("blink.ino")).unwrap();
        assert!(on_disk.ends_with("this is not C++\n"));
    }

    #[test]
    fn nothing_runs_if_the_circuit_cannot_be_saved() {
        let mut f = fixture("save-fails");
        f.open(&f.blink.clone());
        f.add_a_resistor();
        let nowhere = f.root.join("no such folder").join("x.dbr");
        f.rig.app.active_mut().doc.set_path(nowhere);
        f.rig.app.run_pressed(false);
        assert!(f.rig.app.live.is_none());
        assert!(f.rig.app.message.is_some());
    }

    #[test]
    fn a_circuit_never_saved_runs_without_asking_for_a_name() {
        let mut f = fixture("save-untitled");
        f.rig.app.request(Pending::Example(0));
        f.rig.settle();
        f.add_a_resistor();
        // A Save dialog here would hang the test.
        assert!(f.rig.app.save_before_build());
        assert!(f.rig.app.active().doc.path().is_none());
    }

    #[test]
    fn the_j_key_is_listed_in_the_shortcuts() {
        assert!(SHORTCUTS.iter().any(|(key, _)| *key == "J"));
    }
}
