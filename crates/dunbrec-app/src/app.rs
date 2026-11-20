//! The main window: menus, palette, canvas, properties and results.

use std::path::PathBuf;

use dunbrec_core::{
    Diagnostic, Element, Netlist, NodeId, Schematic, build_netlist, units::format_value,
};
use dunbrec_sim::{Circuit, Sample, SimWarning, operating_point};
use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText, ViewportCommand};

use crate::canvas::{
    Annotation, Canvas, Item, NodeMap, TagPlace, Tool, delete_items, node_tag_positions,
    transform_components,
};
use crate::document::Document;
use crate::examples::examples;
use crate::instruments_ui::{Bench, MeterSignal, Signals};
use crate::library::palette;
use crate::live::{Command, Event, LiveSim};
use crate::properties::Properties;

/// Something that would discard unsaved changes, waiting for confirmation.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pending {
    New,
    Open,
    Example(usize),
    ImportSpice,
    ImportKicad,
    Quit,
}

/// DC operating point shown on the canvas and in the results panel.
struct DcResults {
    generation: u64,
    outcome: Result<DcTable, String>,
}

struct DcTable {
    annotations: Vec<Annotation>,
    voltages: Vec<(String, f64)>,
    currents: Vec<(String, f64)>,
    warnings: Vec<String>,
}

pub struct DunbrecApp {
    doc: Document,
    canvas: Canvas,
    properties: Properties,
    netlist: (u64, Netlist),
    dc: Option<DcResults>,
    live_dc: bool,
    show_voltages: bool,
    pending: Option<Pending>,
    allow_close: bool,
    label_name: String,
    message: Option<String>,
    title: String,
    show_about: bool,

    // ---- live simulation and instruments ----
    bench: Bench,
    live: Option<LiveSim>,
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
    /// Node lookup and voltage-tag positions, keyed by generation.
    node_map: (u64, NodeMap, Vec<TagPlace>),
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
}

impl DunbrecApp {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        initial: Option<(Schematic, Option<PathBuf>)>,
    ) -> Self {
        let doc = match initial {
            Some((s, path)) => {
                let mut d = Document::new(s, None);
                if let Some(p) = path {
                    d.mark_saved(p);
                }
                d
            }
            None => Document::default(),
        };
        let netlist = build_netlist(doc.schematic());
        let node_map = NodeMap::new(doc.schematic(), &netlist);
        let tags = node_tag_positions(doc.schematic(), &netlist);
        Self {
            node_map: (doc.generation(), node_map, tags),
            signals: ((u64::MAX, 0), Signals::default()),
            bench: Bench::default(),
            live: None,
            live_generation: 0,
            live_time: 0.0,
            live_keeping_up: 1.0,
            live_error: None,
            probe_hint: None,
            latest: None,
            netlist: (doc.generation(), netlist),
            doc,
            canvas: Canvas::default(),
            properties: Properties::default(),
            dc: None,
            live_dc: true,
            show_voltages: true,
            pending: None,
            allow_close: false,
            label_name: String::new(),
            message: None,
            title: String::new(),
            show_about: false,
        }
    }

    // ------------------------------------------------------- files ---

    fn request(&mut self, action: Pending) {
        if self.doc.is_dirty() {
            self.pending = Some(action);
        } else {
            self.perform(action);
        }
    }

    fn perform(&mut self, action: Pending) {
        match action {
            Pending::New => self.replace(Document::default()),
            Pending::Open => {
                let picked = rfd::FileDialog::new()
                    .add_filter("Dunbrec schematic", &["dbr"])
                    .pick_file();
                if let Some(path) = picked {
                    match dunbrec_io::dbr::load(&path) {
                        Ok(s) => {
                            let mut d = Document::new(s, None);
                            d.mark_saved(path);
                            self.replace(d);
                        }
                        Err(e) => {
                            self.message = Some(format!("Could not open {}:\n{e}", path.display()))
                        }
                    }
                }
            }
            Pending::Example(i) => {
                if let Some(ex) = examples().get(i) {
                    self.replace(Document::new((ex.build)(), None));
                }
            }
            Pending::ImportSpice | Pending::ImportKicad => self.import(action),
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
                self.message = Some(format!("Could not read {}:\n{e}", path.display()));
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
                let mut msg = format!(
                    "Imported {name}. Parts are connected by net labels; rearrange them as you like."
                );
                if !notes.is_empty() {
                    msg.push_str("\n\nNotes:\n");
                    for n in notes.iter().take(30) {
                        msg.push_str(&format!("• {n}\n"));
                    }
                    if notes.len() > 30 {
                        msg.push_str(&format!("… and {} more\n", notes.len() - 30));
                    }
                }
                self.message = Some(msg);
            }
            Err(e) => self.message = Some(format!("Could not import {}:\n{e}", path.display())),
        }
    }

    fn export_spice(&mut self) {
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("SPICE netlist", &["cir"])
            .set_file_name(format!(
                "{}.cir",
                self.doc.display_name().trim_end_matches(".dbr")
            ))
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("cir");
        }
        let title = self.doc.display_name();
        let result = dunbrec_io::spice_export::export(self.doc.schematic(), &title, &[])
            .map_err(|_| "Fix the problems listed under Checks first.".to_string())
            .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
        if let Err(e) = result {
            self.message = Some(format!("Could not export {}:\n{e}", path.display()));
        }
    }

    fn replace(&mut self, doc: Document) {
        self.stop_sim();
        self.bench = Bench::default();
        self.doc = doc;
        self.canvas.selection.clear();
        self.canvas.cancel();
        self.dc = None;
    }

    fn save(&mut self, ask: bool) -> bool {
        let path = match (ask, self.doc.path()) {
            (false, Some(p)) => Some(p.to_path_buf()),
            _ => rfd::FileDialog::new()
                .add_filter("Dunbrec schematic", &["dbr"])
                .set_file_name(format!(
                    "{}.dbr",
                    self.doc.display_name().trim_end_matches(".dbr")
                ))
                .save_file(),
        };
        let Some(mut path) = path else { return false };
        if path.extension().is_none() {
            path.set_extension("dbr");
        }
        match dunbrec_io::dbr::save(self.doc.schematic(), &path) {
            Ok(()) => {
                self.doc.mark_saved(path);
                true
            }
            Err(e) => {
                self.message = Some(format!("Could not save {}:\n{e}", path.display()));
                false
            }
        }
    }

    // ------------------------------------------------- simulation ---

    fn refresh_netlist(&mut self) {
        if self.netlist.0 != self.doc.generation() {
            self.netlist = (self.doc.generation(), build_netlist(self.doc.schematic()));
        }
        let stale = self
            .dc
            .as_ref()
            .is_some_and(|d| d.generation != self.doc.generation());
        if self.live_dc && (self.dc.is_none() || stale) {
            self.run_dc();
        } else if stale {
            self.dc = None;
        }
    }

    fn run_dc(&mut self) {
        let s = self.doc.schematic();
        let netlist = &self.netlist.1;
        let outcome = if s.components.is_empty() {
            Err("Place some parts to see the DC operating point.".to_string())
        } else {
            Circuit::from_netlist(netlist, &s.models)
                .and_then(|c| operating_point(&c).map(|op| (c, op)))
                .map(|(circuit, op)| {
                    let annotations = node_tag_positions(s, netlist)
                        .into_iter()
                        .map(|place| Annotation {
                            text: format_value(op.voltage(place.node), "V"),
                            place,
                        })
                        .collect();
                    let voltages = (1..circuit.node_count())
                        .map(NodeId)
                        .map(|n| (circuit.node_name(n).to_string(), op.voltage(n)))
                        .filter(|(name, _)| !name.contains('#'))
                        .collect();
                    let currents = op.currents().map(|(d, i)| (d.to_string(), i)).collect();
                    let warnings = op
                        .warnings
                        .iter()
                        .map(|w| match w {
                            SimWarning::FloatingNodes { nodes } => format!(
                                "No DC path to ground at {} (tied down with 1 TΩ).",
                                nodes.join(", ")
                            ),
                        })
                        .collect();
                    DcTable {
                        annotations,
                        voltages,
                        currents,
                        warnings,
                    }
                })
                .map_err(|e| e.to_string())
        };
        self.dc = Some(DcResults {
            generation: self.doc.generation(),
            outcome,
        });
    }

    fn describe(&self, d: &Diagnostic) -> String {
        let name = |id| {
            self.doc
                .schematic()
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

    // ---------------------------------------------------- shortcuts ---

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let pressed = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));
        if pressed(cmd_shift(Key::Z)) || pressed(cmd(Key::Y)) {
            self.doc.redo();
        }
        if pressed(cmd(Key::Z)) {
            self.doc.undo();
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
            self.request(Pending::New);
        }
        if pressed(cmd(Key::A)) {
            let s = self.doc.schematic();
            self.canvas.selection = s
                .components
                .iter()
                .map(|c| Item::Component(c.id))
                .chain(s.wires.iter().map(|w| Item::Wire(w.id)))
                .chain(s.labels.iter().map(|l| Item::Label(l.id)))
                .collect();
        }
        let plain = |key| ctx.input(|i| i.key_pressed(key) && i.modifiers.is_none());
        if plain(Key::W) {
            self.canvas.tool = Tool::Wire;
        }
        if plain(Key::L) {
            self.canvas.tool = Tool::Label;
        }
        if plain(Key::P) {
            self.canvas.tool = Tool::Probe;
        }
        if plain(Key::Space) {
            self.toggle_run();
        }
        if plain(Key::S) {
            self.canvas.cancel();
        }
    }

    // ------------------------------------------------ live simulation ---

    fn start_sim(&mut self) {
        self.stop_sim();
        let netlist = self.netlist.1.clone();
        if netlist.has_errors() {
            self.live_error = Some("Fix the problems listed under Checks first.".into());
            return;
        }
        self.bench.clear_readings();
        let speed = self.bench.speed.value(&self.bench.scope);
        let models = self.doc.schematic().models.clone();
        match LiveSim::start(netlist, models, self.bench.max_step(), speed) {
            Ok(live) => {
                self.live = Some(live);
                self.live_generation = self.doc.generation();
                self.live_error = None;
                self.live_time = 0.0;
                self.live_keeping_up = 1.0;
            }
            Err(e) => self.live_error = Some(e),
        }
    }

    fn stop_sim(&mut self) {
        self.live = None;
        self.latest = None;
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
        let generation = self.doc.generation();
        if self.node_map.0 != generation {
            let s = self.doc.schematic();
            self.node_map = (
                generation,
                NodeMap::new(s, &self.netlist.1),
                node_tag_positions(s, &self.netlist.1),
            );
        }
        let key = (generation, self.bench.probe_revision);
        if self.signals.0 != key {
            let resolved =
                self.bench
                    .resolve(self.doc.schematic(), &self.netlist.1, &self.node_map.1);
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
                Event::NeedsRestart => restart = true,
                Event::Error(e) => error = Some(e),
            }
        }
        self.bench.scope.update();
        if let Some(e) = error {
            self.stop_sim();
            self.live_error = Some(format!("Simulation stopped: {e}"));
            return;
        }

        let generation = self.doc.generation();
        if !restart && generation != self.live_generation {
            let new = &self.netlist.1;
            if new.has_errors() {
                self.stop_sim();
                self.live_error =
                    Some("Simulation stopped: the circuit now has problems (see Checks).".into());
                return;
            }
            if same_structure(&live.netlist, new) {
                // Same circuit, new values: update it in place.
                let models = &self.doc.schematic().models;
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

    fn run_toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("run").show(ctx, |ui| {
            ui.horizontal(|ui| {
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
                if self.bench.speed_combo(ui) {
                    let speed = self.bench.speed.value(&self.bench.scope);
                    if let Some(live) = &mut self.live {
                        live.send(Command::SetSpeed(speed));
                    }
                }
                if self.live.is_some() {
                    ui.label(RichText::new(format!("t = {}", format_value(self.live_time, "s"))).monospace());
                    if self.live_keeping_up < 0.95 {
                        ui.label(
                            RichText::new(format!(
                                "running at {:.0}% of the chosen speed",
                                self.live_keeping_up * 100.0
                            ))
                            .color(Color32::from_rgb(230, 150, 40)),
                        )
                        .on_hover_text("The circuit takes longer to simulate than the speed allows. Choose a slower speed or a longer time/div.");
                    }
                }
                ui.separator();
                let probing = self.canvas.tool == Tool::Probe;
                if ui.selectable_label(probing, "🔌 Probe   (P)").clicked() {
                    if probing {
                        self.canvas.cancel();
                    } else {
                        self.canvas.tool = Tool::Probe;
                    }
                }
                ui.toggle_value(&mut self.bench.show_scope, "📈 Oscilloscope");
                ui.toggle_value(&mut self.bench.show_meter, "🔢 Multimeter");
                if let Some(e) = &self.live_error {
                    ui.separator();
                    ui.label(RichText::new(e).color(Color32::from_rgb(225, 70, 70)));
                } else if let Some(h) = &self.probe_hint {
                    ui.separator();
                    ui.label(RichText::new(h).color(Color32::from_rgb(230, 150, 40)));
                }
            });
        });
    }

    fn instrument_windows(&mut self, ctx: &egui::Context) {
        let probing = self.canvas.tool == Tool::Probe;
        let s = self.doc.schematic();
        let netlist = &self.netlist.1;
        let map = &self.node_map.1;
        let mut probe_clicked = false;
        if self.bench.show_scope {
            let (timebase_changed, clicked) =
                self.bench.scope_window(ctx, s, netlist, map, probing);
            probe_clicked |= clicked;
            if timebase_changed && let Some(live) = &mut self.live {
                live.send(Command::SetMaxStep(self.bench.max_step()));
                live.send(Command::SetSpeed(self.bench.speed.value(&self.bench.scope)));
            }
        }
        if self.bench.show_meter {
            let running = self.live.is_some();
            probe_clicked |= self
                .bench
                .meter_window(ctx, s, netlist, map, probing, running);
        }
        if probe_clicked {
            self.canvas.tool = Tool::Probe;
        }
    }

    // ---------------------------------------------------------- UI ---

    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("New        Ctrl+N").clicked() {
                        self.request(Pending::New);
                        ui.close();
                    }
                    if ui.button("Open…      Ctrl+O").clicked() {
                        self.request(Pending::Open);
                        ui.close();
                    }
                    ui.menu_button("Open example", |ui| {
                        for (i, ex) in examples().iter().enumerate() {
                            if ui.button(ex.name).clicked() {
                                self.request(Pending::Example(i));
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    if ui.button("Import SPICE netlist…").clicked() {
                        self.request(Pending::ImportSpice);
                        ui.close();
                    }
                    if ui.button("Import KiCad schematic…").clicked() {
                        self.request(Pending::ImportKicad);
                        ui.close();
                    }
                    if ui.button("Export SPICE netlist…").clicked() {
                        self.export_spice();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Save       Ctrl+S").clicked() {
                        self.save(false);
                        ui.close();
                    }
                    if ui.button("Save as…   Ctrl+Shift+S").clicked() {
                        self.save(true);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                        ui.close();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui
                        .add_enabled(self.doc.can_undo(), egui::Button::new("Undo   Ctrl+Z"))
                        .clicked()
                    {
                        self.doc.undo();
                        ui.close();
                    }
                    if ui
                        .add_enabled(self.doc.can_redo(), egui::Button::new("Redo   Ctrl+Y"))
                        .clicked()
                    {
                        self.doc.redo();
                        ui.close();
                    }
                    ui.separator();
                    let has_selection = !self.canvas.selection.is_empty();
                    if ui
                        .add_enabled(has_selection, egui::Button::new("Rotate   R"))
                        .clicked()
                    {
                        let items = self.canvas.selection.clone();
                        self.doc.edit(|s| transform_components(s, &items, false));
                        ui.close();
                    }
                    if ui
                        .add_enabled(has_selection, egui::Button::new("Mirror   X"))
                        .clicked()
                    {
                        let items = self.canvas.selection.clone();
                        self.doc.edit(|s| transform_components(s, &items, true));
                        ui.close();
                    }
                    if ui
                        .add_enabled(has_selection, egui::Button::new("Delete   Del"))
                        .clicked()
                    {
                        let items = std::mem::take(&mut self.canvas.selection);
                        self.doc.edit(|s| delete_items(s, &items));
                        ui.close();
                    }
                });
                ui.menu_button("Simulate", |ui| {
                    ui.checkbox(&mut self.live_dc, "Live DC operating point");
                    ui.checkbox(&mut self.show_voltages, "Show node voltages on schematic");
                    if ui.button("Recalculate DC now").clicked() {
                        self.run_dc();
                        ui.close();
                    }
                    ui.separator();
                    ui.label(RichText::new("Transient & instruments: coming in phase 6").weak());
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About Dunbrec").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });
            });
        });
    }

    fn palette_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("palette")
            .resizable(true)
            .default_width(170.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading("Tools");
                    let tool = self.canvas.tool.clone();
                    if ui
                        .selectable_label(tool == Tool::Select, "⬉ Select   (S)")
                        .clicked()
                    {
                        self.canvas.cancel();
                    }
                    if ui
                        .selectable_label(tool == Tool::Wire, "✏ Wire   (W)")
                        .clicked()
                    {
                        self.canvas.tool = Tool::Wire;
                    }
                    if ui
                        .selectable_label(tool == Tool::Label, "🏷 Net label   (L)")
                        .clicked()
                    {
                        self.canvas.tool = Tool::Label;
                    }
                    if ui
                        .selectable_label(tool == Tool::Probe, "🔌 Probe   (P)")
                        .clicked()
                    {
                        self.canvas.tool = Tool::Probe;
                    }
                    ui.add_space(8.0);
                    let mut group = "";
                    for entry in palette() {
                        if entry.group != group {
                            group = entry.group;
                            ui.add_space(6.0);
                            ui.label(RichText::new(group).strong());
                        }
                        let kind = (entry.make)();
                        let active = matches!(&tool, Tool::Place { kind: k, .. } if *k == kind);
                        if ui.selectable_label(active, entry.name).clicked() {
                            self.canvas.tool = Tool::Place {
                                kind,
                                rotation: dunbrec_core::Rotation::R0,
                                mirrored: false,
                            };
                        }
                    }
                });
            });
    }

    fn results_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("results")
            .resizable(true)
            .default_height(150.0)
            .show(ctx, |ui| {
                ui.columns(2, |cols| {
                    cols[0].strong("Checks");
                    egui::ScrollArea::vertical()
                        .id_salt("checks")
                        .show(&mut cols[0], |ui| {
                            let diags = &self.netlist.1.diagnostics;
                            if diags.is_empty() {
                                ui.label(
                                    RichText::new("✔ No wiring problems found")
                                        .color(Color32::from_rgb(80, 170, 90)),
                                );
                            }
                            for d in diags {
                                let color = match d.severity() {
                                    dunbrec_core::Severity::Error => Color32::from_rgb(225, 70, 70),
                                    dunbrec_core::Severity::Warning => {
                                        Color32::from_rgb(220, 160, 40)
                                    }
                                };
                                ui.label(RichText::new(self.describe(d)).color(color));
                            }
                        });

                    if let (Some(_), Some(sample)) = (&self.live, &self.latest) {
                        cols[1].strong(format!(
                            "Live values at t = {}",
                            format_value(sample.time, "s")
                        ));
                        egui::ScrollArea::vertical()
                            .id_salt("live")
                            .show(&mut cols[1], |ui| {
                                egui::Grid::new("live_table").striped(true).show(ui, |ui| {
                                    for (i, node) in self.netlist.1.nodes.iter().enumerate().skip(1)
                                    {
                                        if let Some(v) = sample.voltages.get(i) {
                                            ui.monospace(format!("V({})", node.name));
                                            ui.monospace(format_value(*v, "V"));
                                            ui.end_row();
                                        }
                                    }
                                    for (e, i) in
                                        self.netlist.1.elements.iter().zip(&sample.currents)
                                    {
                                        ui.monospace(format!("I({})", e.designator));
                                        ui.monospace(format_value(*i, "A"));
                                        ui.end_row();
                                    }
                                });
                            });
                        return;
                    }
                    cols[1].strong("DC operating point");
                    egui::ScrollArea::vertical()
                        .id_salt("dc")
                        .show(&mut cols[1], |ui| match &self.dc {
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
                                    ui.label(
                                        RichText::new(w).color(Color32::from_rgb(220, 160, 40)),
                                    );
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
                });
            });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(action) = self.pending {
            egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
                ui.heading("Unsaved changes");
                ui.label(format!(
                    "Save changes to {} first?",
                    self.doc.display_name()
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() && self.save(false) {
                        self.pending = None;
                        self.perform(action);
                    }
                    if ui.button("Discard").clicked() {
                        self.pending = None;
                        self.perform(action);
                    }
                    if ui.button("Cancel").clicked() {
                        self.pending = None;
                    }
                });
            });
        }

        if self.canvas.has_label_request() {
            let mut done = None;
            egui::Modal::new(egui::Id::new("label")).show(ctx, |ui| {
                ui.heading("Net label");
                let r =
                    ui.add(egui::TextEdit::singleline(&mut self.label_name).hint_text("e.g. VOUT"));
                r.request_focus();
                ui.horizontal(|ui| {
                    let enter = ui.input(|i| i.key_pressed(Key::Enter));
                    if ui.button("OK").clicked() || enter {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        done = Some(false);
                    }
                });
            });
            if let Some(ok) = done {
                let at = self.canvas.take_label_request();
                let name = std::mem::take(&mut self.label_name).trim().to_string();
                if let (true, Some(at), false) = (ok, at, name.is_empty()) {
                    self.doc.edit(|s| s.add_label(name, at));
                }
            }
        }

        if let Some(msg) = self.message.clone() {
            egui::Modal::new(egui::Id::new("message")).show(ctx, |ui| {
                ui.label(msg);
                if ui.button("OK").clicked() {
                    self.message = None;
                }
            });
        }

        if self.show_about {
            egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
                ui.heading("Dunbrec");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label("Schematic capture and circuit simulation.");
                if ui.button("Close").clicked() {
                    self.show_about = false;
                }
            });
        }
    }

    fn status_bar(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let hint = match &self.canvas.tool {
                    Tool::Select => "Click to select · drag to move · drag empty space to box-select · R rotate · X mirror · Del delete",
                    Tool::Place { .. } => "Click to place · R rotate · X mirror · Esc or right-click to stop",
                    Tool::Wire => "Click to start, click to bend · ends on a pin or wire · double-click, Esc or right-click to stop",
                    Tool::Label => "Click on a wire or pin to name that net",
                    Tool::Probe => "Click a wire or pin to connect the chosen instrument input (a part, for current) · Esc to stop",
                };
                ui.label(RichText::new(hint).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("{:.0}%", self.canvas.camera.scale / 14.0 * 100.0));
                    ui.label(RichText::new("Wheel: zoom · middle-drag: pan").weak());
                });
            });
        });
    }
}

impl eframe::App for DunbrecApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Closing with unsaved changes asks first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.doc.is_dirty()
        {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
        }
        if self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }

        self.shortcuts(ctx);
        self.refresh_netlist();
        self.refresh_signals();
        self.sync_live();
        if self.live.as_ref().is_some_and(|l| !l.paused) {
            ctx.request_repaint();
        }

        let title = format!(
            "{}{} — Dunbrec",
            self.doc.display_name(),
            if self.doc.is_dirty() { " •" } else { "" }
        );
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        self.menu_bar(ctx);
        self.run_toolbar(ctx);
        self.status_bar(ctx);
        self.results_panel(ctx);
        self.palette_panel(ctx);
        egui::SidePanel::right("properties")
            .resizable(true)
            .default_width(260.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let selection: Vec<Item> = self.canvas.selection.iter().copied().collect();
                    self.properties.show(ui, &mut self.doc, &selection);
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                // Live values while simulating, otherwise the DC point.
                let live_tags: Vec<Annotation>;
                let annotations: &[Annotation] = match (&self.live, &self.latest, &self.dc) {
                    _ if !self.show_voltages => &[],
                    (Some(_), Some(sample), _) if self.live_generation == self.doc.generation() => {
                        live_tags = self
                            .node_map
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
                    ) if *generation == self.doc.generation() => &t.annotations,
                    _ => &[],
                };
                let markers = self.bench.markers(self.doc.schematic());
                self.canvas
                    .show(ui, &mut self.doc, &self.netlist.1, annotations, &markers);
            });

        if let Some((at, part)) = self.canvas.take_probe_request() {
            let s = self.doc.schematic();
            let on_net = self.node_map.1.node_at(s, &self.netlist.1, at).is_some();
            self.probe_hint = self.bench.connect(at, part, on_net).err();
        }
        self.instrument_windows(ctx);

        // Drop selections of items that no longer exist (after undo etc.).
        let s = self.doc.schematic();
        self.canvas.selection.retain(|item| match item {
            Item::Component(id) => s.component(*id).is_some(),
            Item::Wire(id) => s.wires.iter().any(|w| w.id == *id),
            Item::Label(id) => s.labels.iter().any(|l| l.id == *id),
        });

        self.dialogs(ctx);
    }
}
