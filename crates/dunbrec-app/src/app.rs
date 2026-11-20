//! The main window: menus, palette, canvas, properties and results.

use std::path::PathBuf;

use dunbrec_core::{Diagnostic, Netlist, NodeId, Schematic, build_netlist, units::format_value};
use dunbrec_sim::{Circuit, SimWarning, operating_point};
use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText, ViewportCommand};

use crate::canvas::{
    Annotation, Canvas, Item, Tool, delete_items, node_tag_positions, transform_components,
};
use crate::document::Document;
use crate::examples::examples;
use crate::library::palette;
use crate::properties::Properties;

/// Something that would discard unsaved changes, waiting for confirmation.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pending {
    New,
    Open,
    Example(usize),
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
        Self {
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
            Pending::Quit => {
                self.allow_close = true;
            }
        }
    }

    fn replace(&mut self, doc: Document) {
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
        if plain(Key::S) {
            self.canvas.cancel();
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
                let annotations: &[Annotation] = match (&self.dc, self.show_voltages) {
                    (
                        Some(DcResults {
                            outcome: Ok(t),
                            generation,
                        }),
                        true,
                    ) if *generation == self.doc.generation() => &t.annotations,
                    _ => &[],
                };
                self.canvas
                    .show(ui, &mut self.doc, &self.netlist.1, annotations);
            });

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
