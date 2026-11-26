//! The main window: menus, palette, canvas, properties and results.

use std::path::PathBuf;

use dunbrec_core::{
    Diagnostic, Element, Netlist, NodeId, Schematic, SourceLanguage, units::format_value,
};
use dunbrec_sim::Sample;
use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText, ViewportCommand};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockArea, DockState, TabViewer};

use crate::analyses_ui::{Analyses, Inputs};
use crate::canvas::{Annotation, Canvas, Item, Marker, PartDrag, Tool};
use crate::code_ui::CodeEditor;
use crate::debugger_ui::{Action as DebugAction, Debugger};
use crate::dialog::{self, Dialog, Message, Unsaved};
use crate::dock::{self, LAYOUT_KEY, Pane};
use crate::document::Document;
use crate::examples::examples;
use crate::instruments_ui::{Bench, MeterSignal, Signals};
use crate::library::{PaletteEntry, palette_groups};
use crate::live::{Command, Event, LiveSim};
use crate::open_circuit::{DcResults, OpenCircuit};
use crate::project::{self, Project};
use crate::project_ui::{self, ProjectPanel};
use crate::properties::{DemoRequest, Properties};
use crate::serial_ui::SerialMonitor;
use crate::settings::{STORAGE_KEY, Settings};

/// Keyboard shortcuts shown under Help.
const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+N / Ctrl+O / Ctrl+S", "New circuit, open, save"),
    ("Ctrl+W", "Close the circuit"),
    ("Ctrl+Shift+S", "Save as"),
    ("Ctrl+Z / Ctrl+Y", "Undo, redo"),
    ("Ctrl+C / Ctrl+X / Ctrl+V", "Copy, cut, paste"),
    ("Ctrl+D", "Duplicate the selection"),
    ("Ctrl+A", "Select everything"),
    ("Del", "Delete the selection"),
    ("R / X", "Rotate / mirror (selection or part being placed)"),
    (
        "Right-click a part",
        "Rotate it once (the whole selection, if it is selected)",
    ),
    (
        "Drag a part from the palette",
        "Drop it on the canvas to place it and start wiring",
    ),
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
    ("Mouse wheel / middle drag", "Zoom / pan"),
];

/// Which project to go to when the user changes projects.
#[derive(Debug, Clone, PartialEq)]
enum ProjectTarget {
    /// An existing project file.
    Open(PathBuf),
    /// A new project, at this file.
    Create(PathBuf),
}

/// Something that would discard unsaved changes, waiting for confirmation.
#[derive(Debug, Clone, PartialEq)]
enum Pending {
    Open,
    OpenPath(PathBuf),
    Example(usize),
    ImportSpice,
    ImportKicad,
    /// Close the project, and with it the circuit that belongs to it.
    CloseProject,
    /// Close the circuit (the project stays open).
    CloseCircuit,
    /// Leave the open project (and its circuit) for another one.
    SwitchProject(ProjectTarget),
    Quit,
}

/// A file or folder the user asked to delete, waiting for their answer.
#[derive(Debug, Clone)]
struct DeleteAsk {
    path: PathBuf,
    /// The open circuit is inside it and has unsaved changes: the user is
    /// asked about losing those first.
    discard_first: bool,
}

pub struct DunbrecApp {
    /// The open circuits. For now there is always exactly one; whether it is
    /// shown is `circuit_open`.
    circuits: Vec<OpenCircuit>,
    /// Which of `circuits` the panels show and the commands act on.
    active: usize,
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

/// Whether the circuit has a microcontroller, which sets the "Auto" speed.
fn has_mcu(netlist: &Netlist) -> bool {
    netlist
        .elements
        .iter()
        .any(|e| matches!(e.kind, dunbrec_core::ComponentKind::Mcu { .. }))
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
            live_speed: 0.0,
            live_generation: 0,
            live_time: 0.0,
            live_keeping_up: 1.0,
            live_error: None,
            probe_hint: None,
            latest: None,
            circuits: vec![OpenCircuit::new(doc)],
            active: 0,
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

    fn request(&mut self, action: Pending) {
        if self.active().doc.is_dirty() {
            self.pending = Some(action);
        } else {
            self.perform(action);
        }
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
                    self.replace(Document::new((ex.build)(), None));
                }
            }
            Pending::ImportSpice | Pending::ImportKicad => self.import(action),
            Pending::CloseProject => self.close_project(),
            Pending::CloseCircuit => self.close_circuit(),
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
                self.replace(doc);
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
        let title = self.active().doc.display_name();
        let result = dunbrec_io::spice_export::export(self.active().doc.schematic(), &title, &[])
            .map_err(|_| "Fix the problems listed under Checks first.".to_string())
            .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
        if let Err(e) = result {
            self.message = Some(Message::file_error("Could not export the file", &path, e));
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        match dunbrec_io::dbr::load(&path) {
            Ok(mut s) => {
                self.settings.remember(&path);
                // The firmware source files it uses live beside it.
                let problems = path
                    .parent()
                    .map(|dir| project::hydrate(&mut s, dir))
                    .unwrap_or_default();
                let mut d = Document::new(s, None);
                d.mark_saved(path);
                self.replace(d);
                if !problems.is_empty() {
                    self.message = Some(source_problems(&problems));
                }
            }
            Err(e) => {
                self.settings.forget(&path);
                self.message = Some(Message::file_error("Could not open the file", &path, e));
            }
        }
    }

    fn replace(&mut self, doc: Document) {
        self.circuit_open = true;
        self.stop_sim();
        self.bench = Bench::default();
        self.active_mut().replace_document(doc);
        // A circuit that is opened is shown, not the Code tab beside it.
        dock::bring_to_front(&mut self.dock, Pane::Schematic);
    }

    /// Close the circuit: nothing is open, and the schematic is not shown.
    fn close_circuit(&mut self) {
        self.replace(Document::default());
        self.circuit_open = false;
    }

    fn save(&mut self, ask: bool) -> bool {
        let path = match (ask, self.active().doc.path()) {
            (false, Some(p)) => Some(p.to_path_buf()),
            _ => rfd::FileDialog::new()
                .add_filter("Dunbrec schematic", &["dbr"])
                .set_file_name(format!(
                    "{}.dbr",
                    self.active().doc.display_name().trim_end_matches(".dbr")
                ))
                .save_file(),
        };
        let Some(mut path) = path else { return false };
        if path.extension().is_none() {
            path.set_extension("dbr");
        }
        // Firmware source files are saved as files beside the schematic, and
        // the schematic itself keeps only the names.
        let mut written = Vec::new();
        if let Some(dir) = path.parent() {
            match project::write_sources(self.active().doc.schematic(), dir) {
                Ok(names) => written = names.into_iter().map(|n| dir.join(n)).collect(),
                Err(e) => {
                    self.message = Some(Message::error("Could not save the source files", e));
                    return false;
                }
            }
        }
        match dunbrec_io::dbr::save(&project::for_saving(self.active().doc.schematic()), &path) {
            Ok(()) => {
                self.settings.remember(&path);
                self.active_mut().doc.mark_saved(path.clone());
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

    /// Close the project. The circuit that is open goes too if the project
    /// lists it (and with it the source files shown in the Code panel), so
    /// nothing stays open that belonged to the project.
    fn close_project(&mut self) {
        self.project = None;
        self.name_prompt = None;
        // With no project open only the Projects screen is shown, so the
        // circuit goes too (the user was asked about unsaved changes first).
        self.close_circuit();
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

    /// Make something new in the open project (a circuit, a source file or
    /// a folder): ask for its name, then list it in the project and open it.
    fn new_in_project(&mut self, what: project_ui::Create) {
        if let Some(project) = &self.project {
            let dir = project.root.clone();
            self.run_project_command(project_ui::Command::AskCreate { dir, what });
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

    /// Go to another project. The circuit of the project that is open now
    /// goes too, so nothing of the old project stays open; if it has
    /// unsaved changes the user is asked about them first.
    fn switch_project(&mut self, to: ProjectTarget) {
        let target = match &to {
            ProjectTarget::Open(file) | ProjectTarget::Create(file) => file,
        };
        if leaves_circuit(self.project.as_ref(), self.active().doc.path(), target) {
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
        use project_ui::{Command as C, Create};
        match command {
            C::NewProject => self.new_project(),
            C::OpenProject => self.pick_and_open_project(),
            C::OpenProjectPath(root) => self.switch_project(ProjectTarget::Open(root)),
            // The open circuit goes with the project, so ask first if it has
            // unsaved changes.
            C::CloseProject => self.request(Pending::CloseProject),
            C::OpenSchematic(path) => self.request(Pending::OpenPath(path)),
            C::OpenSource(path) => self.open_source(&path),
            C::Create { dir, name, what } => {
                let made = match what {
                    Create::Schematic => project::new_schematic(&dir, &name),
                    Create::Assembly => {
                        project::new_source(&dir, &name, project::NewSource::Assembly)
                    }
                    Create::Sketch => project::new_source(&dir, &name, project::NewSource::Sketch),
                    Create::Folder => project::new_folder(&dir, &name),
                };
                match made {
                    Ok(path) => {
                        // What is made here belongs to the project.
                        if let Some(p) = &mut self.project {
                            let listed = match what {
                                Create::Folder => p.add_folder(&path),
                                _ => p.add_file(&path).map(|_| ()),
                            };
                            if let Err(e) = listed {
                                self.message =
                                    Some(Message::error("Could not update the project file", e));
                            }
                        }
                        match what {
                            Create::Schematic => self.request(Pending::OpenPath(path)),
                            Create::Assembly | Create::Sketch => self.open_source(&path),
                            Create::Folder => {}
                        }
                    }
                    Err(e) => self.message = Some(Message::error("Could not create it", e)),
                }
            }
            C::AddExisting => self.add_existing_files(),
            C::RemoveFromProject(path) => {
                if let Some(p) = &mut self.project
                    && let Err(e) = p.remove(&path)
                {
                    self.message = Some(Message::error("Could not update the project file", e));
                }
            }
            C::Rename { from, to } => self.rename_in_project(&from, &to),
            C::AskCreate { dir, what } => {
                self.name_prompt = Some(project_ui::NamePrompt::create(&dir, what));
            }
            C::AskRename(path) => self.name_prompt = Some(project_ui::NamePrompt::rename(&path)),
            C::AskDelete(path) => {
                let discard_first = self.active().doc.is_dirty() && self.open_circuit_is_in(&path);
                self.confirm_delete = Some(DeleteAsk {
                    path,
                    discard_first,
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

    /// Show the source file at `path` in the Code panel, opening it in the
    /// current schematic if it is not open already.
    fn open_source(&mut self, path: &std::path::Path) {
        let Some(dir) = self.firmware_dir() else {
            self.message = Some(Message::info(
                "Save the schematic first",
                "A source file belongs to a schematic and is found by its place relative to \
                 it. Save this schematic (Ctrl+S), preferably in the project folder, and \
                 then open the file again.",
            ));
            return;
        };
        let name = project::relative_to(&dir, path);
        if let Err(e) = project::attach(self.active_mut().doc.schematic_mut(), &dir, &name) {
            self.message = Some(Message::error("Could not open the source file", e));
            return;
        }
        dock::open(&mut self.dock, Pane::Code);
        if let Some(at) = self.dock.find_tab(&Pane::Code) {
            self.dock.set_active_tab(at);
        }
        self.active_mut().code.select(&name);
    }

    fn rename_in_project(&mut self, from: &std::path::Path, to: &std::path::Path) {
        let open = self.active().doc.path().map(|p| p.to_path_buf());
        let Some(project) = self.project.as_mut() else {
            return;
        };
        if let Err(e) = project::rename(project, from, to, open.as_deref()) {
            self.message = Some(Message::error("Could not rename it", e));
            return;
        }
        // The open schematic follows in memory: its own file may have moved,
        // and its references to the renamed file change.
        if let Some(open) = open {
            let none = std::path::Path::new("");
            let old_dir = open.parent().unwrap_or(none).to_path_buf();
            let new_open = match project::strip_prefix_ci(&open, from) {
                Some(rest) if rest.as_os_str().is_empty() => to.to_path_buf(),
                Some(rest) => to.join(rest),
                None => open.clone(),
            };
            let new_dir = new_open.parent().unwrap_or(none).to_path_buf();
            let mut probe = self.active().doc.schematic().clone();
            if project::rewrite_refs(&mut probe, &old_dir, &new_dir, from, to) > 0 {
                self.active_mut().doc.edit(|s| {
                    project::rewrite_refs(s, &old_dir, &new_dir, from, to);
                });
            }
            if new_open != open {
                self.settings.forget(&open);
                self.settings.remember(&new_open);
                self.active_mut().doc.set_path(new_open);
            }
        }
        self.refresh_project();
    }

    /// Whether the open circuit's file is `path` or inside the folder `path`.
    fn open_circuit_is_in(&self, path: &std::path::Path) -> bool {
        let gone = project::normalize(path);
        self.active()
            .doc
            .path()
            .is_some_and(|p| project::starts_with_ci(p, &gone))
    }

    /// The questions before a file or folder is deleted from the disk: first,
    /// if the open circuit is in it and has unsaved changes, whether to lose
    /// those; then whether to delete.
    fn confirm_delete_dialog(&mut self, ctx: &egui::Context, ask: DeleteAsk) {
        let name = ask
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if ask.discard_first {
            match dialog::unsaved_changes(ctx, &self.active().doc.display_name(), false) {
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
        let what = if ask.path.is_dir() {
            format!("Delete the folder “{name}” and everything in it?")
        } else {
            format!("Delete “{name}”?")
        };
        // Cancel is the highlighted button: Enter must not delete a file.
        let answer = Dialog {
            id: "confirm_delete",
            kind: dialog::Kind::Warning,
            title: "Delete from disk",
            buttons: &["Delete", "Cancel"],
            default: 1,
            cancel: Some(1),
        }
        .show(ctx, |ui| {
            dialog::paragraph(ui, &what);
            ui.add_space(4.0);
            dialog::paragraph(ui, "This removes it from the disk and cannot be undone.");
        });
        match answer {
            Some(0) => {
                self.confirm_delete = None;
                self.delete_in_project(&ask.path);
            }
            Some(_) => self.confirm_delete = None,
            None => {}
        }
    }

    fn delete_in_project(&mut self, path: &std::path::Path) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        if let Err(e) = project::delete(project, path) {
            self.message = Some(Message::error("Could not delete it", e));
            return;
        }
        // A circuit that was deleted cannot be edited or simulated: it is closed.
        if self.open_circuit_is_in(path) {
            self.close_circuit();
        } else if let Some(dir) = self.firmware_dir() {
            // Drop open source files that were deleted with it.
            let gone = project::normalize(path);
            let dead: Vec<String> = self
                .active()
                .doc
                .schematic()
                .sources
                .keys()
                .filter(|n| project::is_file_source(n))
                .filter(|n| project::starts_with_ci(&dir.join(n), &gone))
                .cloned()
                .collect();
            for name in dead {
                self.active_mut().doc.schematic_mut().sources.remove(&name);
            }
        }
        self.refresh_project();
    }

    /// Rescan the project folder, and take up edits made to the open
    /// schematic's source files elsewhere (unless it has unsaved changes,
    /// which win).
    fn refresh_project(&mut self) {
        if let Some(p) = &mut self.project
            && let Err(e) = p.reload()
        {
            // Keep what is open; say why it could not be read again.
            p.refresh();
            self.message = Some(Message::error("Could not read the project file", e));
        }
        if !self.active().doc.is_dirty()
            && let Some(dir) = self.firmware_dir()
        {
            let changed = project::sync_from_disk(self.active_mut().doc.schematic_mut(), &dir);
            if !changed.is_empty() {
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
                self.new_in_project(project_ui::Create::Schematic);
            }
            return;
        }
        if pressed(cmd(Key::W)) {
            self.request(Pending::CloseCircuit);
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
            self.new_in_project(project_ui::Create::Schematic);
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

    /// Start the live simulation, optionally paused at time zero (for the
    /// debugger to step from the first instruction).
    fn start_sim_with(&mut self, paused: bool) {
        self.stop_sim();
        let netlist = self.active().netlist.1.clone();
        if netlist.has_errors() {
            self.live_error = Some("Fix the problems listed under Checks first.".into());
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

    fn stop_sim(&mut self) {
        self.live = None;
        self.latest = None;
        self.debugger.sim_stopped();
    }

    /// Run, or pause/resume a running simulation.
    fn toggle_run(&mut self) {
        match &mut self.live {
            None => self.start_sim(),
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
        egui::TopBottomPanel::top("run").frame(frame).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if !self.circuit_open {
                    ui.disable();
                }
                match &self.live {
                    None => {
                        if ui.button("▶ Run").on_hover_text("Start the live simulation (Space)").clicked() {
                            self.start_sim();
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
                            self.start_sim();
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
                        egui::vec2(130.0, height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            // The slot shrinks to its contents unless told not to.
                            ui.set_min_size(egui::vec2(130.0, height));
                            ui.label(
                                RichText::new(format!("t = {}", format_value(self.live_time, "s")))
                                    .monospace(),
                            );
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(96.0, height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_size(egui::vec2(96.0, height));
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
        if (self.debugger.show || dock::is_open(&self.dock, Pane::Code))
            && self.debug_loaded != Some(circuit.netlist.0)
        {
            let dir = circuit.firmware_dir();
            self.debugger.load(&circuit.netlist.1, dir.as_deref());
            self.debug_loaded = Some(circuit.netlist.0);
        }
        if self.debugger.window(ctx, &mut self.live) == DebugAction::StartPaused {
            self.start_sim_with(true);
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

    /// Draw the dockable panels: schematic, palette, properties, checks and
    /// values.
    fn dock_area(&mut self, ctx: &egui::Context) {
        // The central area holds the schematic while a circuit is open, and
        // nothing (but stays) when none is.
        dock::sync_documents(&mut self.dock, self.circuit_open);
        let running = self.live.is_some();
        let lit = self.lit_leds();
        let markers = self.bench.markers(self.active().doc.schematic());
        let title = self.active().doc.display_name();
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
            netlist,
            node_map,
            dc,
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
        let mut close_circuit = false;
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
            close_circuit: &mut close_circuit,
        };
        DockArea::new(&mut self.dock)
            .style(egui_dock::Style::from_egui(ctx.style().as_ref()))
            .show(ctx, &mut viewer);
        if let Some(command) = project_command {
            self.run_project_command(command);
        }
        if close_circuit {
            self.request(Pending::CloseCircuit);
        }
        // Source files the Code panel made on disk belong to the project the
        // circuit is in.
        let created = self.active_mut().code.take_created();
        if !created.is_empty()
            && let (Some(circuit), Some(project)) =
                (self.circuits[self.active].doc.path(), self.project.as_mut())
            && project.contains(circuit)
        {
            if let Err(e) = project.add_files(&created) {
                self.message = Some(Message::error("Could not update the project file", e));
            }
            project.refresh();
        }
        if let Some(problem) = self.active_mut().code.take_problem() {
            self.message = Some(Message::error("Could not create the file", problem));
        }
        if open_settings {
            self.settings_dialog.open();
        }
        // "Edit code" on a microcontroller's source file opens it in the
        // editor.
        if let Some(name) = self.active_mut().properties.open_code.take() {
            self.show_code(&name);
        }
        if let Some(request) = self.active_mut().properties.demo.take() {
            self.use_demo(ctx, request);
        }
    }

    /// Bring the Code panel to the front on the source `name`.
    fn show_code(&mut self, name: &str) {
        dock::open(&mut self.dock, Pane::Code);
        dock::bring_to_front(&mut self.dock, Pane::Code);
        self.active_mut().code.select(name);
    }

    /// Make a microcontroller its own copy of a demo program, list the file
    /// in the project, and show it. A sketch is built straight away, so that
    /// it can be run.
    fn use_demo(&mut self, ctx: &egui::Context, request: DemoRequest) {
        let sketch = request.language == SourceLanguage::Cpp;
        // A sketch cannot run until it is built.
        if sketch {
            self.stop_sim();
        }
        let file = match self.active_mut().add_demo(request) {
            Ok(file) => file,
            Err(e) => {
                self.message = Some(Message::error("Could not create the file", e));
                return;
            }
        };
        if let Some(path) = &file.path
            && let Some(project) = self.project.as_mut()
            && self.circuits[self.active]
                .doc
                .path()
                .is_some_and(|c| project.contains(c))
        {
            if let Err(e) = project.add_files(std::slice::from_ref(path)) {
                self.message = Some(Message::error("Could not update the project file", e));
            }
            project.refresh();
        }
        self.show_code(&file.name);
        if sketch {
            self.active_mut()
                .code
                .build_sketch(ctx, &file.name, &file.text);
        }
    }

    // ---------------------------------------------------------- UI ---

    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    ui.menu_button("New", |ui| {
                        for what in project_ui::Create::ALL {
                            let label = if what == project_ui::Create::Schematic {
                                format!("{}   Ctrl+N", what.menu_label())
                            } else {
                                what.menu_label().to_string()
                            };
                            if ui.button(label).clicked() {
                                self.new_in_project(what);
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text("Make something new in the project");
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
                        self.request(Pending::CloseCircuit);
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

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(action) = self.pending.clone() {
            match dialog::unsaved_changes(ctx, &self.active().doc.display_name(), true) {
                Some(Unsaved::Save) if self.save(false) => {
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
                    egui::Grid::new("shortcuts").striped(true).show(ui, |ui| {
                        for (keys, what) in SHORTCUTS {
                            ui.monospace(*keys);
                            ui.label(*what);
                            ui.end_row();
                        }
                    });
                });
            self.show_shortcuts = open;
        }
    }

    fn status_bar(&self, ctx: &egui::Context) {
        let frame = self.run_frame(ctx);
        egui::TopBottomPanel::bottom("status").frame(frame).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let hint = match &self.active().canvas.tool {
                    Tool::Select => "Click to select · click a pin to start a wire · drag a wire corner to reshape it · drag to move · drag empty space to box-select · R rotate · X mirror · Del delete",
                    Tool::Place { .. } => "Click to place · R rotate · X mirror · Esc or right-click to stop",
                    Tool::Wire => "Click to start a wire (a part's body selects it instead), click to bend · ends on a pin or wire, not inside a part · double-click, Esc or right-click to stop",
                    Tool::Label => "Click on a wire or pin to name that net",
                    Tool::Probe => "Click a wire or pin to connect the chosen instrument input (a part, for current) · Esc to stop",
                };
                ui.label(RichText::new(hint).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("{:.0}%", self.active().canvas.camera.scale / 14.0 * 100.0));
                    ui.label(RichText::new("Wheel: zoom · middle-drag: pan").weak());
                });
            });
        });
    }
}

/// Whether `circuit`, the file of the open circuit, is one the project lists.
fn circuit_belongs_to(project: Option<&Project>, circuit: Option<&std::path::Path>) -> bool {
    matches!((project, circuit), (Some(p), Some(c)) if p.contains(c))
}

/// Whether going to the project file `target` leaves the open circuit
/// behind: it belongs to the project that is open, which is a different one.
fn leaves_circuit(
    open: Option<&Project>,
    circuit: Option<&std::path::Path>,
    target: &std::path::Path,
) -> bool {
    circuit_belongs_to(open, circuit) && open.is_some_and(|p| !project::path_eq(&p.file, target))
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
                // canvas to place one and start wiring it.
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
        "The schematic refers to firmware source files that are missing or unreadable. \
         The rest of it opened normally.",
    )
    .with_detail(problems.join("\n"));
    message.kind = dialog::Kind::Warning;
    message
}

/// What the dock needs to draw each panel: the parts of the app they show
/// or edit, borrowed for one frame.
struct PaneViewer<'a> {
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
    /// The user closed the schematic's tab: the circuit is to be closed.
    close_circuit: &'a mut bool,
}

impl TabViewer for PaneViewer<'_> {
    type Tab = Pane;

    fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Pane) {
        match *tab {
            Pane::Schematic => self.canvas.show(
                ui,
                self.doc,
                self.netlist,
                self.annotations,
                self.markers,
                self.lit,
            ),
            Pane::Palette => palette_ui(ui, self.canvas, self.palette_search),
            Pane::Properties => {
                let selection: Vec<Item> = self.canvas.selection.iter().copied().collect();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.properties.show(ui, self.doc, &selection);
                });
            }
            Pane::Checks => checks_ui(ui, self.netlist, self.doc.schematic()),
            Pane::Values => values_ui(ui, self.netlist, self.running, self.latest, self.dc),
            Pane::Code => self.code.show(ui, self.doc, self.debugger, self.live),
            Pane::Project => {
                let embedded = if self.doc.path().is_some() {
                    project::embedded_count(self.doc.schematic())
                } else {
                    0
                };
                let view = project_ui::View {
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

    /// Closing the schematic's tab (its close button, a middle click, or the
    /// tab's menu) closes the circuit, after the question about unsaved
    /// changes; the tab itself is taken out by the circuit closing.
    fn on_close(&mut self, tab: &mut Pane) -> OnCloseResponse {
        if *tab == Pane::Schematic {
            *self.close_circuit = true;
            OnCloseResponse::Ignore
        } else {
            OnCloseResponse::Close
        }
    }

    /// Every panel scrolls itself, or draws edge to edge.
    fn scroll_bars(&self, _tab: &Pane) -> [bool; 2] {
        [false, false]
    }

    /// The schematic fills its tab to the edge.
    fn tab_style_override(
        &self,
        tab: &Pane,
        global: &egui_dock::TabStyle,
    ) -> Option<egui_dock::TabStyle> {
        match tab {
            Pane::Schematic => {
                let mut style = global.clone();
                style.tab_body.inner_margin = egui::Margin::ZERO;
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

impl eframe::App for DunbrecApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, &self.settings);
        eframe::set_value(storage, LAYOUT_KEY, &self.dock);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Closing with unsaved changes asks first.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.allow_close
            && self.active().doc.is_dirty()
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

        self.shortcuts(ctx);
        let live_dc = self.live_dc;
        self.active_mut().refresh_netlist(live_dc);
        self.refresh_signals();
        self.sync_live();
        if self.live.as_ref().is_some_and(|l| !l.paused) {
            ctx.request_repaint();
        }

        let title = if self.circuit_open {
            format!(
                "{}{} — Dunbrec",
                self.active().doc.display_name(),
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

    #[test]
    fn only_a_circuit_the_project_lists_goes_with_it() {
        let dir = std::env::temp_dir().join(format!("dunbrec-close-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (project, circuit) = Project::create(&dir.join("P")).unwrap();
        assert!(circuit_belongs_to(Some(&project), Some(&circuit)));
        // A circuit somewhere else, an unsaved one, or no project at all.
        assert!(!circuit_belongs_to(
            Some(&project),
            Some(&dir.join("other.dbr"))
        ));
        assert!(!circuit_belongs_to(Some(&project), None));
        assert!(!circuit_belongs_to(None, Some(&circuit)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn changing_project_leaves_the_circuit_only_for_a_different_project() {
        let dir = std::env::temp_dir().join(format!("dunbrec-switch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (project, circuit) = Project::create(&dir.join("A")).unwrap();
        let other = dir.join("B.dbrproj");
        // Another project: the open circuit of A is left behind.
        assert!(leaves_circuit(Some(&project), Some(&circuit), &other));
        // The same project again keeps it.
        assert!(!leaves_circuit(
            Some(&project),
            Some(&circuit),
            &project.file
        ));
        // A circuit that is not A's, an unsaved one, or no project: nothing to leave.
        assert!(!leaves_circuit(
            Some(&project),
            Some(&dir.join("x.dbr")),
            &other
        ));
        assert!(!leaves_circuit(Some(&project), None, &other));
        assert!(!leaves_circuit(None, Some(&circuit), &other));
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
            let input = egui::RawInput {
                screen_rect: Some(screen),
                time: Some(self.time),
                events,
                ..Default::default()
            };
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
            self.frame(vec![egui::Event::PointerMoved(at)]);
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                }]);
            }
            self.settle();
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
        assert!(dock::is_open(&app.dock, Pane::Code), "shown for editing");
        assert!(
            app.active()
                .doc
                .schematic()
                .sources
                .contains_key("blink.asm")
        );

        // On "Auto" it runs as a board would: the light changes every half
        // second of real time.
        rig.click(" Run");
        assert!(rig.app.live.is_some(), "{:?}", rig.app.live_error);
        assert_eq!(rig.app.live_speed, 1.0);
        let mut states = Vec::new();
        for _ in 0..60 {
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

    #[test]
    fn the_j_key_is_listed_in_the_shortcuts() {
        assert!(SHORTCUTS.iter().any(|(key, _)| *key == "J"));
    }
}
