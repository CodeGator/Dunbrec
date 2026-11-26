//! The property inspector for the selected component or label.

use std::collections::HashMap;

use dunbrec_core::{
    ComponentId, ComponentKind, LedColour, ModelKind, Polarity, Schematic, SourceLanguage,
    SwitchForm, Waveform,
    units::{format_value, parse_eng, parse_spice},
};
use egui::{Color32, RichText, Ui};

use dunbrec_core::SOURCE_PREFIX;

use crate::canvas::Item;
use crate::document::Document;

/// Text being typed into value fields, keyed by field id. Values are only
/// applied when the user presses Enter or leaves the field.
#[derive(Default)]
pub struct Properties {
    buffers: HashMap<String, String>,
    errors: HashMap<String, String>,
    new_param: (String, String),
    /// Set while a slider is being dragged: changes apply live, and only
    /// the first frame of the drag records an undo step.
    continuous: bool,
    /// A source file the user asked to edit; the app shows its tab and
    /// clears this.
    pub open_code: Option<String>,
    /// A demo program the user asked to have a copy of; the app makes the
    /// file and clears this.
    pub demo: Option<DemoRequest>,
    /// A microcontroller the user asked to give a new source file, and in
    /// which language; the app makes it and clears this.
    pub new_file: Option<(ComponentId, SourceLanguage)>,
}

/// A microcontroller asking for its own copy of a demo program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemoRequest {
    pub chip: ComponentId,
    /// The demo's name, as in [`dunbrec_mcu::demos`].
    pub demo: &'static str,
    pub language: SourceLanguage,
}

/// A change the panel wants to make; applied with an undo checkpoint.
type Edit = Box<dyn FnOnce(&mut Schematic)>;

impl Properties {
    pub fn show(&mut self, ui: &mut Ui, doc: &mut Document, selection: &[Item]) {
        let mut edit: Option<Edit> = None;
        self.continuous = false;
        match selection {
            [] => {
                ui.label(RichText::new("Nothing selected").weak());
                ui.add_space(6.0);
                ui.label("Pick a part from the palette and click on the canvas to place it.");
            }
            [Item::Component(id)] => {
                if let Some(c) = doc.schematic().component(*id).cloned() {
                    edit = self.component(ui, doc.schematic(), &c);
                }
            }
            [Item::Label(id)] => {
                if let Some(l) = doc.schematic().labels.iter().find(|l| l.id == *id).cloned() {
                    ui.heading("Net label");
                    let key = format!("label{}", id.0);
                    if let Some(name) = self.text_field(ui, &key, "Name", &l.name)
                        && !name.trim().is_empty()
                    {
                        let id = *id;
                        edit = Some(Box::new(move |s| {
                            if let Some(l) = s.labels.iter_mut().find(|l| l.id == id) {
                                l.name = name.trim().to_string();
                            }
                        }));
                    }
                }
            }
            [Item::Wire(_)] => {
                ui.heading("Wire");
            }
            many => {
                ui.label(format!("{} items selected", many.len()));
            }
        }
        if let Some(f) = edit {
            if self.continuous {
                f(doc.schematic_mut());
            } else {
                doc.edit(f);
            }
        }
    }

    fn component(
        &mut self,
        ui: &mut Ui,
        schematic: &Schematic,
        c: &dunbrec_core::Component,
    ) -> Option<Edit> {
        let id = c.id;
        let mut edit: Option<Edit> = None;
        let set_kind = |kind: ComponentKind| -> Option<Edit> {
            Some(Box::new(move |s: &mut Schematic| {
                // A model picked from the library is copied into the schematic.
                crate::library::ensure_model(s, &kind);
                if let Some(c) = s.component_mut(id) {
                    c.kind = kind;
                }
            }))
        };

        ui.heading(kind_name(&c.kind));
        egui::Grid::new("props")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                if !matches!(c.kind, ComponentKind::Ground | ComponentKind::Junction)
                    && let Some(d) =
                        self.text_field(ui, &key(id, "des"), "Designator", &c.designator)
                {
                    let d = d.trim().to_string();
                    if !d.is_empty() {
                        edit = Some(Box::new(move |s| {
                            if let Some(c) = s.component_mut(id) {
                                c.designator = d;
                            }
                        }));
                    }
                }

                let mut k = c.kind.clone();
                let changed = match &mut k {
                    ComponentKind::Ground | ComponentKind::Junction => false,
                    ComponentKind::Resistor { resistance } => {
                        self.value(ui, id, "Resistance", "Ω", resistance)
                    }
                    ComponentKind::Capacitor {
                        capacitance,
                        initial_voltage,
                    } => {
                        self.value(ui, id, "Capacitance", "F", capacitance)
                            | self.optional_value(ui, id, "Initial voltage", "V", initial_voltage)
                    }
                    ComponentKind::Inductor {
                        inductance,
                        initial_current,
                    } => {
                        self.value(ui, id, "Inductance", "H", inductance)
                            | self.optional_value(ui, id, "Initial current", "A", initial_current)
                    }
                    ComponentKind::VoltageSource { waveform } => {
                        self.waveform(ui, id, "V", waveform)
                    }
                    ComponentKind::CurrentSource { waveform } => {
                        self.waveform(ui, id, "A", waveform)
                    }
                    ComponentKind::Diode { model } => {
                        self.model_choice(ui, id, schematic, ModelKind::Diode, model)
                    }
                    ComponentKind::Bjt { polarity, model } => {
                        let mk = if *polarity == Polarity::N {
                            ModelKind::Npn
                        } else {
                            ModelKind::Pnp
                        };
                        self.model_choice(ui, id, schematic, mk, model)
                    }
                    ComponentKind::Mosfet { polarity, model } => {
                        let mk = if *polarity == Polarity::N {
                            ModelKind::Nmos
                        } else {
                            ModelKind::Pmos
                        };
                        self.model_choice(ui, id, schematic, mk, model)
                    }
                    ComponentKind::Switch {
                        closed,
                        r_on,
                        r_off,
                        momentary,
                    } => {
                        ui.label("Closed");
                        let a = ui.checkbox(closed, "").changed();
                        ui.end_row();
                        ui.label("Push button");
                        let b = ui.checkbox(momentary, "").changed();
                        ui.end_row();
                        a | b
                            | self.value(ui, id, "On resistance", "Ω", r_on)
                            | self.value(ui, id, "Off resistance", "Ω", r_off)
                    }
                    ComponentKind::MultiSwitch {
                        thrown,
                        r_on,
                        r_off,
                        ..
                    } => {
                        ui.label("Thrown");
                        let a = ui.checkbox(thrown, "").changed();
                        ui.end_row();
                        a | self.value(ui, id, "On resistance", "Ω", r_on)
                            | self.value(ui, id, "Off resistance", "Ω", r_off)
                    }
                    ComponentKind::Potentiometer { resistance, wiper } => {
                        let a = self.value(ui, id, "Resistance", "Ω", resistance);
                        ui.label("Wiper");
                        let slider = ui.add(egui::Slider::new(wiper, 0.0..=1.0).fixed_decimals(2));
                        ui.end_row();
                        if slider.dragged() && !slider.drag_started() {
                            self.continuous = true;
                        }
                        a | slider.changed()
                    }
                    ComponentKind::Vcvs { gain } => self.value(ui, id, "Gain", "V/V", gain),
                    ComponentKind::Vccs { transconductance } => {
                        self.value(ui, id, "Transconductance", "S", transconductance)
                    }
                    ComponentKind::Ccvs { transresistance } => {
                        self.value(ui, id, "Transresistance", "Ω", transresistance)
                    }
                    ComponentKind::Cccs { gain } => self.value(ui, id, "Gain", "A/A", gain),
                    ComponentKind::Gate { op, delay } => {
                        ui.label("Function");
                        let before = *op;
                        egui::ComboBox::from_id_salt(("gate", id))
                            .selected_text(op.name())
                            .show_ui(ui, |ui| {
                                for o in dunbrec_core::GateOp::ALL {
                                    ui.selectable_value(op, o, o.name());
                                }
                            });
                        ui.end_row();
                        (*op != before) | self.value(ui, id, "Delay", "s", delay)
                    }
                    ComponentKind::Inverter { delay }
                    | ComponentKind::DFlipFlop { delay }
                    | ComponentKind::Counter4 { delay } => self.value(ui, id, "Delay", "s", delay),
                    ComponentKind::Clock { frequency, duty } => {
                        let a = self.value(ui, id, "Frequency", "Hz", frequency);
                        ui.label("Duty cycle");
                        let slider = ui.add(egui::Slider::new(duty, 0.05..=0.95).fixed_decimals(2));
                        ui.end_row();
                        if slider.dragged() && !slider.drag_started() {
                            self.continuous = true;
                        }
                        a | slider.changed()
                    }
                    ComponentKind::LogicSwitch { high } => {
                        ui.label("Output");
                        let r = ui.checkbox(high, "high").changed();
                        ui.end_row();
                        r
                    }
                    ComponentKind::LogicLed => false,
                    ComponentKind::Led { colour } | ComponentKind::LedBar { colour } => {
                        ui.label("Colour");
                        let before = *colour;
                        egui::ComboBox::from_id_salt(("led", id))
                            .selected_text(colour.name())
                            .show_ui(ui, |ui| {
                                for c in LedColour::ALL {
                                    ui.selectable_value(colour, c, c.name());
                                }
                            });
                        ui.end_row();
                        ui.label("");
                        ui.label(
                            egui::RichText::new(format!(
                                "About {} V across it when lit, and it needs a resistor in \
                                 series: from 5 V, about {} Ω.",
                                colour.forward_voltage(),
                                colour.resistor_from_5v()
                            ))
                            .weak(),
                        );
                        ui.end_row();
                        *colour != before
                    }
                    ComponentKind::OpAmp { part } => {
                        ui.label("Part");
                        let before = part.clone();
                        egui::ComboBox::from_id_salt(("opamp", id))
                            .selected_text(part.as_str())
                            .show_ui(ui, |ui| {
                                for p in dunbrec_core::OPAMP_PARTS {
                                    ui.selectable_value(part, p.to_string(), *p);
                                }
                            });
                        ui.end_row();
                        ui.label("");
                        ui.label(
                            egui::RichText::new(
                                "Boyle macromodel: input transistors, 1 MHz gain-bandwidth, \
                                 0.5 V/µs slew rate, output current limit, and an output \
                                 that stops about 1–2 V short of the supplies. Connect V+ \
                                 and V− to a supply (e.g. ±15 V).",
                            )
                            .small()
                            .weak(),
                        );
                        ui.end_row();
                        *part != before
                    }
                    ComponentKind::Cd4066 { r_on } => {
                        let changed = self.value(ui, id, "On resistance", "Ω", r_on);
                        ui.label("");
                        ui.label(
                            egui::RichText::new(
                                "CD4066B quad bilateral switch, 3–18 V. Each switch passes                                  a signal either way between its two pins while its control                                  pin is above half of VDD, and blocks it below. Signals must                                  stay between VSS and VDD. The on resistance depends on the                                  supply: typically 470 Ω at 5 V, 180 Ω at 10 V, 125 Ω at 15 V.                                  Tie unused controls to VSS.",
                            )
                            .small()
                            .weak(),
                        );
                        ui.end_row();
                        changed
                    }
                    ComponentKind::Timer555 => {
                        ui.label("");
                        ui.label(
                            egui::RichText::new(
                                "Bipolar NE555, 4.5–16 V. Output goes high (VCC − 1.4 V) \
                                 when TRIG falls below 1/3 VCC, and low when THR rises \
                                 above CTRL (2/3 VCC); DIS is shorted to GND while the \
                                 output is low. RESET below 0.7 V forces the output low. \
                                 Tie RESET to VCC if unused.",
                            )
                            .small()
                            .weak(),
                        );
                        ui.end_row();
                        false
                    }
                    ComponentKind::Mcu { firmware, clock } => {
                        let mut changed = false;
                        if let Some(f) =
                            self.text_field(ui, &key(id, "firmware"), "Firmware", firmware)
                        {
                            *firmware = f.trim().to_string();
                            changed = true;
                        }
                        // One control to a row, so that none is cut off in
                        // a narrow panel.
                        ui.label("");
                        if ui
                            .button("Browse…")
                            .on_hover_text("Choose an Intel HEX file")
                            .clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .add_filter("Intel HEX", &["hex", "ihx"])
                                .add_filter("All files", &["*"])
                                .pick_file()
                        {
                            *firmware = path.display().to_string();
                            changed = true;
                        }
                        ui.end_row();
                        let builtin = builtin_demo(firmware).map(str::to_string);
                        ui.label("Demo");
                        egui::ComboBox::from_id_salt(("demo", id))
                            .width(CONTROL_WIDTH)
                            // The built-in program the chip runs, if it runs one.
                            .selected_text(builtin.as_deref().unwrap_or("Choose…"))
                            .show_ui(ui, |ui| {
                                for (name, about) in dunbrec_mcu::programs::BUILTIN {
                                    for &(language, label) in DEMO_LANGUAGES {
                                        let hint = format!(
                                            "{about}.\nMakes your own copy, as a file you \
                                             can change."
                                        );
                                        if ui
                                            .selectable_label(false, format!("{name} ({label})"))
                                            .on_hover_text(hint)
                                            .clicked()
                                        {
                                            self.demo = Some(DemoRequest {
                                                chip: id,
                                                demo: name,
                                                language,
                                            });
                                        }
                                    }
                                }
                            });
                        ui.end_row();
                        ui.label("");
                        egui::ComboBox::from_id_salt(("source", id))
                            .width(CONTROL_WIDTH)
                            .selected_text("Source file")
                            .show_ui(ui, |ui| {
                                for name in schematic.sources.keys() {
                                    let reference = source_reference(name);
                                    if ui
                                        .selectable_label(*firmware == reference, name)
                                        .clicked()
                                    {
                                        *firmware = reference;
                                        changed = true;
                                    }
                                }
                                for (language, label) in [
                                    (SourceLanguage::Cpp, "New Arduino sketch…"),
                                    (SourceLanguage::Asm, "New assembly file…"),
                                ] {
                                    if ui.selectable_label(false, label).clicked() {
                                        self.new_file = Some((id, language));
                                    }
                                }
                            });
                        ui.end_row();
                        if let Some(name) = source_name(firmware) {
                            ui.label("");
                            if ui
                                .button("Edit code")
                                .on_hover_text("Show this source file's tab")
                                .clicked()
                            {
                                self.open_code = Some(name);
                            }
                            ui.end_row();
                        } else if let Some(demo) =
                            builtin.as_deref().and_then(dunbrec_mcu::demos::demo)
                        {
                            ui.label("");
                            ui.menu_button("Edit code", |ui| {
                                for &(language, label) in DEMO_LANGUAGES {
                                    if ui.button(format!("Copy as {label}")).clicked() {
                                        self.demo = Some(DemoRequest {
                                            chip: id,
                                            demo: demo.name,
                                            language,
                                        });
                                        ui.close();
                                    }
                                }
                            })
                            .response
                            .on_hover_text(
                                "A built-in demo has no file: make your own copy to change",
                            );
                            ui.end_row();
                        }
                        let clock_changed = self.value(ui, id, "Clock", "Hz", clock);
                        ui.label("");
                        ui.vertical(|ui| {
                            ui.set_max_width(CONTROL_WIDTH);
                            let help = "A source file is a file beside this circuit. Assembly \
                                        (.asm) and sketches (.ino) are built when you press \
                                        Run, sketches only if changed. For a program \
                                        compiled elsewhere: Arduino IDE, Sketch > Export \
                                        Compiled Binary, then pick the .hex without \
                                        \"with_bootloader\".";
                            ui.add(egui::Label::new(RichText::new(help).small().weak()).wrap());
                        });
                        ui.end_row();
                        changed | clock_changed
                    }
                };
                if changed {
                    edit = set_kind(k);
                }
            });

        // Model parameters, edited in place in the schematic's library.
        if let Some(model_name) = c.kind.model_name()
            && let Some(model) = schematic.models.get(model_name)
        {
            ui.add_space(8.0);
            egui::CollapsingHeader::new(format!("Model {} parameters", model.name))
                .default_open(false)
                .show(ui, |ui| {
                    if let Some(e) = self.model_params(ui, model) {
                        edit = Some(e);
                    }
                });
        }

        if !self.errors.is_empty() {
            ui.add_space(6.0);
            for e in self.errors.values() {
                ui.label(RichText::new(e).color(Color32::from_rgb(220, 60, 60)));
            }
        }
        edit
    }

    /// A single-line text field; returns the new text when committed.
    fn text_field(&mut self, ui: &mut Ui, key: &str, label: &str, current: &str) -> Option<String> {
        ui.label(label);
        let buffer = self
            .buffers
            .entry(key.to_string())
            .or_insert_with(|| current.to_string());
        let response = ui.add(egui::TextEdit::singleline(buffer).desired_width(140.0));
        ui.end_row();
        let mut result = None;
        if response.lost_focus() {
            if buffer != current {
                result = Some(buffer.clone());
            }
            self.buffers.remove(key);
        } else if !response.has_focus() {
            *buffer = current.to_string();
        }
        result
    }

    /// An engineering value field ("4k7", "10u"); true if `value` changed.
    fn value(
        &mut self,
        ui: &mut Ui,
        id: ComponentId,
        label: &str,
        unit: &str,
        value: &mut f64,
    ) -> bool {
        let k = key(id, label);
        let shown = format_value(*value, unit);
        let Some(text) = self.text_field(ui, &k, label, &shown) else {
            return false;
        };
        match parse_eng(&text) {
            Ok(v) if v.is_finite() => {
                self.errors.remove(&k);
                *value = v;
                true
            }
            _ => {
                self.errors.insert(
                    k,
                    format!("{label}: '{text}' is not a value (try 4k7, 10u, 2.2M)"),
                );
                false
            }
        }
    }

    fn optional_value(
        &mut self,
        ui: &mut Ui,
        id: ComponentId,
        label: &str,
        unit: &str,
        value: &mut Option<f64>,
    ) -> bool {
        let mut enabled = value.is_some();
        ui.label(label);
        let toggled = ui.checkbox(&mut enabled, "set").changed();
        ui.end_row();
        if toggled {
            *value = enabled.then_some(0.0);
            return true;
        }
        match value {
            Some(v) => self.value(ui, id, &format!("  {label} value"), unit, v),
            None => false,
        }
    }

    fn waveform(&mut self, ui: &mut Ui, id: ComponentId, unit: &str, w: &mut Waveform) -> bool {
        let names = ["DC", "Sine", "Pulse", "PWL"];
        let current = match w {
            Waveform::Dc(_) => 0,
            Waveform::Sine { .. } => 1,
            Waveform::Pulse { .. } => 2,
            Waveform::Pwl(_) => 3,
        };
        let mut chosen = current;
        ui.label("Waveform");
        egui::ComboBox::from_id_salt(("wave", id))
            .selected_text(names[current])
            .show_ui(ui, |ui| {
                for (i, n) in names.iter().enumerate() {
                    ui.selectable_value(&mut chosen, i, *n);
                }
            });
        ui.end_row();
        if chosen != current {
            let level = w.dc_value();
            *w = match chosen {
                0 => Waveform::Dc(level),
                1 => Waveform::Sine {
                    offset: 0.0,
                    amplitude: if level != 0.0 { level } else { 1.0 },
                    frequency: 1e3,
                    delay: 0.0,
                    damping: 0.0,
                    phase: 0.0,
                },
                2 => Waveform::Pulse {
                    v1: 0.0,
                    v2: if level != 0.0 { level } else { 1.0 },
                    delay: 0.0,
                    rise: 1e-6,
                    fall: 1e-6,
                    width: 0.5e-3,
                    period: 1e-3,
                },
                _ => Waveform::Pwl(vec![(0.0, 0.0), (1e-3, level)]),
            };
            return true;
        }
        match w {
            Waveform::Dc(v) => self.value(ui, id, "Value", unit, v),
            Waveform::Sine {
                offset,
                amplitude,
                frequency,
                delay,
                damping,
                phase,
            } => {
                self.value(ui, id, "Offset", unit, offset)
                    | self.value(ui, id, "Amplitude", unit, amplitude)
                    | self.value(ui, id, "Frequency", "Hz", frequency)
                    | self.value(ui, id, "Delay", "s", delay)
                    | self.value(ui, id, "Damping", "1/s", damping)
                    | self.value(ui, id, "Phase", "°", phase)
            }
            Waveform::Pulse {
                v1,
                v2,
                delay,
                rise,
                fall,
                width,
                period,
            } => {
                self.value(ui, id, "Initial", unit, v1)
                    | self.value(ui, id, "Pulsed", unit, v2)
                    | self.value(ui, id, "Delay", "s", delay)
                    | self.value(ui, id, "Rise time", "s", rise)
                    | self.value(ui, id, "Fall time", "s", fall)
                    | self.value(ui, id, "Width", "s", width)
                    | self.value(ui, id, "Period", "s", period)
            }
            Waveform::Pwl(points) => {
                let k = key(id, "pwl");
                let shown = points
                    .iter()
                    .map(|(t, v)| format!("{} {}", format_value(*t, ""), format_value(*v, "")))
                    .collect::<Vec<_>>()
                    .join(", ");
                let Some(text) = self.text_field(ui, &k, "Points (t v, …)", &shown) else {
                    return false;
                };
                match parse_pwl(&text) {
                    Some(p) => {
                        self.errors.remove(&k);
                        *points = p;
                        true
                    }
                    None => {
                        self.errors.insert(
                            k,
                            "PWL points must be 'time value' pairs with rising times".into(),
                        );
                        false
                    }
                }
            }
        }
    }

    fn model_choice(
        &mut self,
        ui: &mut Ui,
        id: ComponentId,
        schematic: &Schematic,
        kind: ModelKind,
        model: &mut String,
    ) -> bool {
        ui.label("Model");
        let before = model.clone();
        egui::ComboBox::from_id_salt(("model", id))
            .selected_text(model.as_str())
            .show_ui(ui, |ui| {
                for m in schematic.models.iter().filter(|m| m.kind == kind) {
                    ui.selectable_value(model, m.name.clone(), &m.name);
                }
                // Real parts not yet used in this schematic.
                let library: Vec<_> = dunbrec_core::parts::standard_parts()
                    .into_iter()
                    .filter(|p| {
                        p.model.kind == kind && schematic.models.get(&p.model.name).is_none()
                    })
                    .collect();
                if !library.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("From the parts library").weak().small());
                    for p in library {
                        ui.selectable_value(model, p.model.name.clone(), &p.model.name)
                            .on_hover_text(p.description);
                    }
                }
            });
        ui.end_row();
        *model != before
    }

    fn model_params(&mut self, ui: &mut Ui, model: &dunbrec_core::DeviceModel) -> Option<Edit> {
        let mut edit: Option<Edit> = None;
        let name = model.name.clone();
        egui::Grid::new(("params", &model.name))
            .num_columns(3)
            .show(ui, |ui| {
                for (param, value) in &model.params {
                    let k = format!("param:{}:{param}", model.name);
                    ui.label(param);
                    let buffer = self
                        .buffers
                        .entry(k.clone())
                        .or_insert_with(|| format_value(*value, ""));
                    let response = ui.add(egui::TextEdit::singleline(buffer).desired_width(90.0));
                    if response.lost_focus() {
                        match parse_spice(buffer) {
                            Ok(v) if v != *value => {
                                let (name, param) = (name.clone(), param.clone());
                                edit = Some(Box::new(move |s| {
                                    if let Some(mut m) = s.models.get(&name).cloned() {
                                        m.set(&param, v);
                                        s.models.insert(m);
                                    }
                                }));
                            }
                            Ok(_) => {}
                            Err(_) => {
                                self.errors.insert(
                                    k.clone(),
                                    format!("{param}: '{buffer}' is not a value"),
                                );
                            }
                        }
                        self.buffers.remove(&k);
                    } else if !response.has_focus() {
                        *buffer = format_value(*value, "");
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Remove (use the default)")
                        .clicked()
                    {
                        let (name, param) = (name.clone(), param.clone());
                        edit = Some(Box::new(move |s| {
                            if let Some(mut m) = s.models.get(&name).cloned() {
                                m.params.remove(&param);
                                s.models.insert(m);
                            }
                        }));
                    }
                    ui.end_row();
                }
            });
        ui.horizontal(|ui| {
            let (p, v) = &mut self.new_param;
            ui.add(
                egui::TextEdit::singleline(p)
                    .hint_text("PARAM")
                    .desired_width(60.0),
            );
            ui.add(
                egui::TextEdit::singleline(v)
                    .hint_text("value")
                    .desired_width(70.0),
            );
            if ui.button("Add").clicked()
                && let (false, Ok(value)) = (p.trim().is_empty(), parse_spice(v))
            {
                let (name, param) = (name.clone(), p.trim().to_ascii_uppercase());
                edit = Some(Box::new(move |s| {
                    if let Some(mut m) = s.models.get(&name).cloned() {
                        m.set(&param, value);
                        s.models.insert(m);
                    }
                }));
                self.new_param = Default::default();
            }
        });
        ui.label(
            RichText::new("Model values use SPICE units: 1M = milli, 1MEG = mega.")
                .weak()
                .small(),
        );
        edit
    }
}

fn key(id: ComponentId, field: &str) -> String {
    format!("{}:{field}", id.0)
}

fn parse_pwl(text: &str) -> Option<Vec<(f64, f64)>> {
    let mut points = Vec::new();
    for pair in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let mut it = pair.split_whitespace();
        let t = parse_eng(it.next()?).ok()?;
        let v = parse_eng(it.next()?).ok()?;
        if it.next().is_some() {
            return None;
        }
        points.push((t, v));
    }
    let rising = points.windows(2).all(|w| w[1].0 > w[0].0);
    (!points.is_empty() && rising).then_some(points)
}

/// The languages a demo program comes in, and what to call them.
const DEMO_LANGUAGES: &[(SourceLanguage, &str)] = &[
    (SourceLanguage::Cpp, "Arduino"),
    (SourceLanguage::Asm, "assembly"),
];

/// Width of the controls under a microcontroller's firmware field.
const CONTROL_WIDTH: f32 = 140.0;

/// The name of the built-in program a firmware setting selects, if it does.
fn builtin_demo(firmware: &str) -> Option<&str> {
    firmware
        .trim()
        .strip_prefix(dunbrec_mcu::firmware::BUILTIN_PREFIX)
}

/// The firmware reference that selects the source called `name`: its path,
/// for a file, and `source:<name>` for a source kept inside the schematic.
fn source_reference(name: &str) -> String {
    if crate::project::is_file_source(name) {
        name.to_string()
    } else {
        format!("{SOURCE_PREFIX}{name}")
    }
}

/// The name of the source a firmware reference selects, if it selects one.
fn source_name(firmware: &str) -> Option<String> {
    let reference = firmware.trim();
    match reference.strip_prefix(SOURCE_PREFIX) {
        Some(name) => Some(name.trim().to_string()),
        None => crate::project::is_file_source(reference).then(|| reference.to_string()),
    }
}

pub fn kind_name(kind: &ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Ground => "Ground",
        ComponentKind::Junction => "Junction",
        ComponentKind::Resistor { .. } => "Resistor",
        ComponentKind::Capacitor { .. } => "Capacitor",
        ComponentKind::Inductor { .. } => "Inductor",
        ComponentKind::VoltageSource { .. } => "Voltage source",
        ComponentKind::CurrentSource { .. } => "Current source",
        ComponentKind::Diode { .. } => "Diode",
        ComponentKind::Bjt {
            polarity: Polarity::N,
            ..
        } => "NPN transistor",
        ComponentKind::Bjt { .. } => "PNP transistor",
        ComponentKind::Mosfet {
            polarity: Polarity::N,
            ..
        } => "N-channel MOSFET",
        ComponentKind::Mosfet { .. } => "P-channel MOSFET",
        ComponentKind::Switch {
            momentary: true, ..
        } => "Push button",
        ComponentKind::Switch { .. } => "Switch",
        ComponentKind::MultiSwitch {
            form: SwitchForm::Spdt,
            ..
        } => "SPDT switch",
        ComponentKind::MultiSwitch {
            form: SwitchForm::Dpst,
            ..
        } => "DPST switch",
        ComponentKind::MultiSwitch { .. } => "DPDT switch",
        ComponentKind::Potentiometer { .. } => "Potentiometer",
        ComponentKind::Vcvs { .. } => "Voltage-controlled voltage source",
        ComponentKind::Vccs { .. } => "Voltage-controlled current source",
        ComponentKind::Ccvs { .. } => "Current-controlled voltage source",
        ComponentKind::Cccs { .. } => "Current-controlled current source",
        ComponentKind::Gate { .. } => "Logic gate",
        ComponentKind::Inverter { .. } => "Inverter (NOT)",
        ComponentKind::DFlipFlop { .. } => "D flip-flop",
        ComponentKind::Counter4 { .. } => "4-bit counter",
        ComponentKind::Clock { .. } => "Clock",
        ComponentKind::LogicSwitch { .. } => "Logic switch",
        ComponentKind::LogicLed => "Logic LED",
        ComponentKind::Led { .. } => "LED",
        ComponentKind::LedBar { .. } => "LED bar display",
        ComponentKind::Mcu { .. } => "Microcontroller (ATmega328P)",
        ComponentKind::OpAmp { .. } => "Operational amplifier",
        ComponentKind::Timer555 => "555 timer",
        ComponentKind::Cd4066 { .. } => "CD4066 analog switch",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pwl_parsing() {
        assert_eq!(
            parse_pwl("0 0, 1m 5, 2m 0"),
            Some(vec![(0.0, 0.0), (1e-3, 5.0), (2e-3, 0.0)])
        );
        assert_eq!(parse_pwl("0 0, 0 1"), None, "times must rise");
        assert_eq!(parse_pwl("0"), None);
        assert_eq!(parse_pwl(""), None);
    }
}
