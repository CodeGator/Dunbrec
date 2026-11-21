//! Instrument windows (oscilloscope, multimeter) and the "bench" state that
//! connects them to the schematic: which probe is where, and run speed.

// Channels are addressed by number throughout (settings, colours, probes
// and signals are parallel arrays), so index loops read most clearly.
#![allow(clippy::needless_range_loop)]

use dunbrec_core::{ComponentId, Netlist, NodeId, Point, Schematic, units::format_value};
use dunbrec_instruments::scope::{CHANNELS, DIVISIONS_X, DIVISIONS_Y, PRE_TRIGGER_DIVISIONS};
use dunbrec_instruments::{Meter, MeterMode, Scope, ScopeStatus, TriggerEdge, TriggerMode};
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};

use crate::canvas::{Marker, NodeMap};
use crate::logic_ui::{LOGIC_CHANNELS, LOGIC_COLOR, LogicAnalyzer};

/// Trace colours, as on a classic four-channel scope.
pub const CHANNEL_COLORS: [Color32; CHANNELS] = [
    Color32::from_rgb(250, 210, 30),
    Color32::from_rgb(40, 200, 255),
    Color32::from_rgb(255, 90, 200),
    Color32::from_rgb(90, 230, 110),
];
const METER_PLUS: Color32 = Color32::from_rgb(235, 60, 50);
const METER_COM: Color32 = Color32::from_rgb(140, 140, 150);

/// 1-2-5 sequence between `from` and `to`.
fn one_two_five(from: f64, to: f64) -> Vec<f64> {
    let mut out = Vec::new();
    let mut decade = 10f64.powf(from.log10().floor());
    while decade <= to * 1.0001 {
        for m in [1.0, 2.0, 5.0] {
            let v = decade * m;
            if v >= from * 0.9999 && v <= to * 1.0001 {
                out.push(v);
            }
        }
        decade *= 10.0;
    }
    out
}

/// Which instrument input the next probe click connects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeSlot {
    Scope(usize),
    MeterPlus,
    MeterCom,
    /// Output node of the Bode plotter.
    BodeOut,
    /// Output node of the DC transfer plot.
    SweepOut,
    /// A logic analyser channel.
    Logic(usize),
}

/// What a probe is connected to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeTarget {
    /// A point on a net (voltage).
    Node(Point),
    /// A part (current through it).
    Part(ComponentId),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Speed {
    /// About four screen-widths of the scope per second.
    Auto,
    /// Simulated seconds per real second.
    Fixed(f64),
}

impl Speed {
    pub fn value(self, scope: &Scope) -> f64 {
        match self {
            Speed::Auto => 4.0 * scope.window(),
            Speed::Fixed(s) => s,
        }
    }

    fn label(self) -> String {
        match self {
            Speed::Auto => "Auto".into(),
            Speed::Fixed(1.0) => "Real time (1 s/s)".into(),
            Speed::Fixed(s) => format!("{}/s", format_value(s, "s")),
        }
    }
}

/// Resolved signal sources for the current netlist.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Signals {
    pub scope: [Option<NodeId>; CHANNELS],
    pub meter: MeterSignal,
    pub logic: [Option<NodeId>; LOGIC_CHANNELS],
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum MeterSignal {
    #[default]
    None,
    Voltage(NodeId, NodeId),
    /// Current into the first pin of the element at this netlist index.
    Current(usize),
}

pub struct Bench {
    pub scope: Scope,
    pub meter: Meter,
    pub scope_probes: [Option<Point>; CHANNELS],
    pub meter_plus: Option<ProbeTarget>,
    /// `None` means the common lead is on ground.
    pub meter_com: Option<Point>,
    pub bode_out: Option<Point>,
    pub sweep_out: Option<Point>,
    pub logic_probes: [Option<Point>; LOGIC_CHANNELS],
    pub logic: LogicAnalyzer,
    pub show_logic: bool,
    pub active: ProbeSlot,
    pub show_scope: bool,
    pub show_meter: bool,
    pub speed: Speed,
    /// Bumped whenever a probe moves, so signals are re-resolved.
    pub probe_revision: u64,
}

impl Default for Bench {
    fn default() -> Self {
        let mut scope = Scope::default();
        for (i, ch) in scope.channels.iter_mut().enumerate() {
            ch.enabled = i < 2;
        }
        Self {
            scope,
            meter: Meter::default(),
            scope_probes: [None; CHANNELS],
            meter_plus: None,
            meter_com: None,
            bode_out: None,
            sweep_out: None,
            logic_probes: [None; LOGIC_CHANNELS],
            logic: LogicAnalyzer::default(),
            show_logic: false,
            active: ProbeSlot::Scope(0),
            show_scope: false,
            show_meter: false,
            speed: Speed::Auto,
            probe_revision: 0,
        }
    }
}

impl Bench {
    /// Largest simulation step for the scope to draw smooth traces.
    pub fn max_step(&self) -> f64 {
        self.scope.window() / 250.0
    }

    pub fn clear_readings(&mut self) {
        self.scope.clear();
        self.meter.clear();
        self.logic.clear();
    }

    /// Connect the active probe to what was clicked. Returns a message if
    /// the click did not land on something usable.
    pub fn connect(
        &mut self,
        at: Point,
        part: Option<ComponentId>,
        on_net: bool,
    ) -> Result<(), String> {
        match self.active {
            ProbeSlot::Scope(ch) => {
                if !on_net {
                    return Err("Click on a wire or pin to probe a voltage.".into());
                }
                self.scope_probes[ch] = Some(at);
                self.scope.channels[ch].enabled = true;
            }
            ProbeSlot::MeterPlus if self.meter.mode.measures_current() => {
                let Some(id) = part else {
                    return Err(
                        "In current mode, click on a part to measure the current through it."
                            .into(),
                    );
                };
                self.meter_plus = Some(ProbeTarget::Part(id));
            }
            ProbeSlot::MeterPlus => {
                if !on_net {
                    return Err("Click on a wire or pin to measure its voltage.".into());
                }
                self.meter_plus = Some(ProbeTarget::Node(at));
            }
            ProbeSlot::MeterCom => {
                if !on_net {
                    return Err("Click on a wire or pin for the meter's common lead.".into());
                }
                self.meter_com = Some(at);
            }
            ProbeSlot::Logic(ch) => {
                if !on_net {
                    return Err("Click on a wire or pin for the logic analyser channel.".into());
                }
                self.logic_probes[ch] = Some(at);
            }
            ProbeSlot::BodeOut | ProbeSlot::SweepOut => {
                if !on_net {
                    return Err("Click on a wire or pin for the output to plot.".into());
                }
                if self.active == ProbeSlot::BodeOut {
                    self.bode_out = Some(at);
                } else {
                    self.sweep_out = Some(at);
                }
                self.probe_revision += 1;
                return Ok(());
            }
        }
        self.probe_revision += 1;
        self.clear_readings();
        Ok(())
    }

    /// Resolve probe positions to nodes and elements of `netlist`.
    pub fn resolve(&self, s: &Schematic, netlist: &Netlist, map: &NodeMap) -> Signals {
        let node = |p: &Option<Point>| p.and_then(|p| map.node_at(s, netlist, p));
        let scope = [0, 1, 2, 3].map(|ch| node(&self.scope_probes[ch]));
        let meter = match (self.meter.mode.measures_current(), self.meter_plus) {
            (true, Some(ProbeTarget::Part(id))) => netlist
                .elements
                .iter()
                .position(|e| e.component == id)
                .map_or(MeterSignal::None, MeterSignal::Current),
            (false, Some(ProbeTarget::Node(p))) => match map.node_at(s, netlist, p) {
                Some(plus) => {
                    let com = node(&self.meter_com).unwrap_or(NodeId::GROUND);
                    MeterSignal::Voltage(plus, com)
                }
                None => MeterSignal::None,
            },
            _ => MeterSignal::None,
        };
        let logic = std::array::from_fn(|ch| node(&self.logic_probes[ch]));
        Signals {
            scope,
            meter,
            logic,
        }
    }

    /// Flags showing where the probes are, for the canvas.
    pub fn markers(&self, s: &Schematic) -> Vec<Marker> {
        let mut out = Vec::new();
        for (ch, p) in self.scope_probes.iter().enumerate() {
            if let Some(at) = p {
                out.push(Marker {
                    at: *at,
                    text: format!("CH{}", ch + 1),
                    color: CHANNEL_COLORS[ch],
                });
            }
        }
        match self.meter_plus {
            Some(ProbeTarget::Node(at)) => out.push(Marker {
                at,
                text: "DMM +".into(),
                color: METER_PLUS,
            }),
            Some(ProbeTarget::Part(id)) => {
                if let Some(c) = s.component(id) {
                    out.push(Marker {
                        at: c.position,
                        text: "DMM A".into(),
                        color: METER_PLUS,
                    });
                }
            }
            None => {}
        }
        if let Some(at) = self.meter_com {
            out.push(Marker {
                at,
                text: "COM".into(),
                color: METER_COM,
            });
        }
        for (ch, p) in self.logic_probes.iter().enumerate() {
            if let Some(at) = p {
                out.push(Marker {
                    at: *at,
                    text: format!("D{ch}"),
                    color: LOGIC_COLOR,
                });
            }
        }
        for (p, text) in [(self.bode_out, "BODE"), (self.sweep_out, "SWEEP")] {
            if let Some(at) = p {
                out.push(Marker {
                    at,
                    text: text.into(),
                    color: Color32::from_rgb(180, 140, 255),
                });
            }
        }
        out
    }

    pub(crate) fn describe(
        &self,
        target: Option<ProbeTarget>,
        s: &Schematic,
        netlist: &Netlist,
        map: &NodeMap,
    ) -> String {
        match target {
            None => "not connected".into(),
            Some(ProbeTarget::Node(p)) => match map.node_at(s, netlist, p) {
                Some(n) if n.is_ground() => "ground".into(),
                Some(n) => netlist.nodes[n.0].name.clone(),
                None => "(nothing here any more)".into(),
            },
            Some(ProbeTarget::Part(id)) => s.component(id).map_or("(part removed)".into(), |c| {
                format!("through {}", c.designator)
            }),
        }
    }

    pub(crate) fn probe_button(
        &mut self,
        ui: &mut Ui,
        slot: ProbeSlot,
        probe_tool_active: bool,
    ) -> bool {
        let selected = self.active == slot && probe_tool_active;
        let clicked = ui
            .selectable_label(selected, "🔌 Probe")
            .on_hover_text("Then click on the schematic to connect this input")
            .clicked();
        if clicked {
            self.active = slot;
        }
        clicked
    }

    // ------------------------------------------------- oscilloscope ---

    /// Returns (timebase changed, a probe button was clicked).
    pub fn scope_ui(
        &mut self,
        ui: &mut Ui,
        s: &Schematic,
        netlist: &Netlist,
        map: &NodeMap,
        probe_tool_active: bool,
    ) -> (bool, bool) {
        let mut timebase_changed = false;
        let mut probe_clicked = false;
        {
            {
                let width = ui.available_width().max(300.0);
                let height = (width * 0.6).clamp(180.0, 420.0);
                let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
                draw_screen(ui, rect, &self.scope);

                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label("Time/div");
                    let before = self.scope.time_per_div;
                    egui::ComboBox::from_id_salt("tdiv")
                        .selected_text(format_value(self.scope.time_per_div, "s"))
                        .show_ui(ui, |ui| {
                            for v in one_two_five(1e-6, 1.0) {
                                ui.selectable_value(
                                    &mut self.scope.time_per_div,
                                    v,
                                    format_value(v, "s"),
                                );
                            }
                        });
                    timebase_changed = self.scope.time_per_div != before;
                    ui.separator();
                    ui.label("Trigger");
                    for (mode, name) in [
                        (TriggerMode::Auto, "Auto"),
                        (TriggerMode::Normal, "Normal"),
                        (TriggerMode::Single, "Single"),
                    ] {
                        let current = self.scope.trigger.mode;
                        if ui.selectable_label(current == mode, name).clicked() && current != mode {
                            self.scope.trigger.mode = mode;
                            if mode == TriggerMode::Single {
                                self.scope.arm();
                            }
                        }
                    }
                    if self.scope.trigger.mode == TriggerMode::Single && ui.button("Arm").clicked()
                    {
                        self.scope.arm();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    let t = &mut self.scope.trigger;
                    ui.label("Source");
                    egui::ComboBox::from_id_salt("tsrc")
                        .selected_text(format!("CH{}", t.source + 1))
                        .show_ui(ui, |ui| {
                            for ch in 0..CHANNELS {
                                ui.selectable_value(&mut t.source, ch, format!("CH{}", ch + 1));
                            }
                        });
                    if ui
                        .selectable_label(t.edge == TriggerEdge::Rising, "↗ Rising")
                        .clicked()
                    {
                        t.edge = TriggerEdge::Rising;
                    }
                    if ui
                        .selectable_label(t.edge == TriggerEdge::Falling, "↘ Falling")
                        .clicked()
                    {
                        t.edge = TriggerEdge::Falling;
                    }
                    ui.label("Level");
                    let step = self.scope.channels[t.source].volts_per_div * 0.05;
                    ui.add(
                        egui::DragValue::new(&mut t.level)
                            .speed(step)
                            .suffix(" V")
                            .max_decimals(4),
                    );
                });

                ui.add_space(4.0);
                let sweep = self.scope.sweep().cloned();
                egui::Grid::new("channels")
                    .num_columns(6)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        for ch in 0..CHANNELS {
                            let color = CHANNEL_COLORS[ch];
                            ui.label(RichText::new(format!("CH{}", ch + 1)).color(color).strong());
                            if self.probe_button(ui, ProbeSlot::Scope(ch), probe_tool_active) {
                                probe_clicked = true;
                            }
                            let target = self.scope_probes[ch].map(ProbeTarget::Node);
                            ui.label(self.describe(target, s, netlist, map));
                            let c = &mut self.scope.channels[ch];
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut c.enabled, "On");
                                ui.checkbox(&mut c.ac, "AC").on_hover_text(
                                    "AC coupling: hide the DC level to see a small signal riding on it",
                                );
                            });
                            egui::ComboBox::from_id_salt(("vdiv", ch))
                                .width(80.0)
                                .selected_text(format!(
                                    "{}/div",
                                    format_value(c.volts_per_div, "V")
                                ))
                                .show_ui(ui, |ui| {
                                    for v in one_two_five(1e-3, 50.0) {
                                        ui.selectable_value(
                                            &mut c.volts_per_div,
                                            v,
                                            format!("{}/div", format_value(v, "V")),
                                        );
                                    }
                                });
                            ui.add(
                                egui::DragValue::new(&mut c.offset_div)
                                    .speed(0.05)
                                    .range(-DIVISIONS_Y..=DIVISIONS_Y)
                                    .prefix("pos ")
                                    .suffix(" div")
                                    .max_decimals(2),
                            );
                            ui.end_row();
                            // Measurements on the displayed sweep.
                            if let (true, Some(st)) =
                                (c.enabled, sweep.as_ref().and_then(|w| w.stats(ch)))
                            {
                                ui.label("");
                                let freq = sweep
                                    .as_ref()
                                    .and_then(|w| w.frequency(ch))
                                    .map_or("—".into(), |f| format_value(f, "Hz"));
                                ui.label(
                                    RichText::new(format!(
                                        "Vpp {}   Avg {}   RMS {}   Freq {}",
                                        format_value(st.peak_to_peak(), "V"),
                                        format_value(st.mean, "V"),
                                        format_value(st.rms, "V"),
                                        freq
                                    ))
                                    .monospace()
                                    .small(),
                                );
                                ui.end_row();
                            }
                        }
                    });
            }
        }
        (timebase_changed, probe_clicked)
    }

    // ----------------------------------------------------- multimeter ---

    /// Returns true if a probe button was clicked.
    pub fn meter_ui(
        &mut self,
        ui: &mut Ui,
        s: &Schematic,
        netlist: &Netlist,
        map: &NodeMap,
        probe_tool_active: bool,
        running: bool,
    ) -> bool {
        let mut probe_clicked = false;
        {
            {
                let reading = match (running, self.meter.reading()) {
                    (true, Some(v)) => format_value(v, self.meter.mode.unit()),
                    _ => "– – – –".into(),
                };
                egui::Frame::new()
                    .fill(Color32::from_rgb(20, 30, 25))
                    .corner_radius(6.0)
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.set_min_width(260.0);
                        ui.label(
                            RichText::new(reading)
                                .monospace()
                                .size(34.0)
                                .color(Color32::from_rgb(140, 255, 170)),
                        );
                        ui.label(
                            RichText::new(self.meter.mode.label())
                                .monospace()
                                .color(Color32::from_rgb(90, 170, 110)),
                        );
                    });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let before = self.meter.mode;
                    for mode in [
                        MeterMode::DcVolts,
                        MeterMode::AcVolts,
                        MeterMode::DcAmps,
                        MeterMode::AcAmps,
                    ] {
                        ui.selectable_value(&mut self.meter.mode, mode, mode.label());
                    }
                    if self.meter.mode.measures_current() != before.measures_current() {
                        // Voltage and current need different connections.
                        self.meter_plus = None;
                        self.probe_revision += 1;
                    }
                    if self.meter.mode != before {
                        self.meter.clear();
                    }
                });
                ui.add_space(4.0);
                egui::Grid::new("meter_probes")
                    .num_columns(3)
                    .show(ui, |ui| {
                        ui.label(RichText::new("+").color(METER_PLUS).strong());
                        if self.probe_button(ui, ProbeSlot::MeterPlus, probe_tool_active) {
                            probe_clicked = true;
                        }
                        ui.label(self.describe(self.meter_plus, s, netlist, map));
                        ui.end_row();
                        if !self.meter.mode.measures_current() {
                            ui.label(RichText::new("COM").color(METER_COM).strong());
                            if self.probe_button(ui, ProbeSlot::MeterCom, probe_tool_active) {
                                probe_clicked = true;
                            }
                            match self.meter_com {
                                None => {
                                    ui.label("ground");
                                }
                                Some(p) => {
                                    ui.horizontal(|ui| {
                                        ui.label(self.describe(
                                            Some(ProbeTarget::Node(p)),
                                            s,
                                            netlist,
                                            map,
                                        ));
                                        if ui.small_button("⏚ to ground").clicked() {
                                            self.meter_com = None;
                                            self.probe_revision += 1;
                                            self.meter.clear();
                                        }
                                    });
                                }
                            }
                            ui.end_row();
                        }
                    });
                ui.horizontal(|ui| {
                    ui.label("Integration time");
                    egui::ComboBox::from_id_salt("meter_window")
                        .selected_text(format_value(self.meter.window, "s"))
                        .show_ui(ui, |ui| {
                            for w in [1e-3, 10e-3, 20e-3, 100e-3, 1.0] {
                                ui.selectable_value(
                                    &mut self.meter.window,
                                    w,
                                    format_value(w, "s"),
                                );
                            }
                        });
                });
                if !running {
                    ui.label(RichText::new("Press ▶ Run to measure.").weak());
                }
            }
        }
        probe_clicked
    }

    /// Returns true if a probe button was clicked.
    pub fn logic_ui(
        &mut self,
        ui: &mut Ui,
        s: &Schematic,
        netlist: &Netlist,
        map: &NodeMap,
        probe_tool_active: bool,
    ) -> bool {
        let mut probe_clicked = false;
        {
            {
                let names: [String; LOGIC_CHANNELS] =
                    std::array::from_fn(|ch| match self.logic_probes[ch] {
                        Some(p) => self.describe(Some(ProbeTarget::Node(p)), s, netlist, map),
                        None => String::new(),
                    });
                self.logic.draw(ui, &names);
                ui.horizontal_wrapped(|ui| {
                    ui.label("Time/div");
                    egui::ComboBox::from_id_salt("la_tdiv")
                        .selected_text(format_value(self.logic.time_per_div, "s"))
                        .show_ui(ui, |ui| {
                            for v in one_two_five(1e-9, 1.0) {
                                ui.selectable_value(
                                    &mut self.logic.time_per_div,
                                    v,
                                    format_value(v, "s"),
                                );
                            }
                        });
                });
                ui.horizontal_wrapped(|ui| {
                    for ch in 0..LOGIC_CHANNELS {
                        ui.label(format!("D{ch}"));
                        if self.probe_button(ui, ProbeSlot::Logic(ch), probe_tool_active) {
                            probe_clicked = true;
                        }
                    }
                });
            }
        }
        probe_clicked
    }

    pub fn speed_combo(&mut self, ui: &mut Ui) -> bool {
        let before = self.speed;
        egui::ComboBox::from_id_salt("speed")
            .selected_text(self.speed.label())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.speed, Speed::Auto, Speed::Auto.label())
                    .on_hover_text("About four oscilloscope screens per second");
                for s in [1e-6, 1e-5, 1e-4, 1e-3, 1e-2, 0.1, 1.0] {
                    let sp = Speed::Fixed(s);
                    ui.selectable_value(&mut self.speed, sp, sp.label());
                }
            });
        self.speed != before
    }
}

fn draw_screen(ui: &Ui, rect: Rect, scope: &Scope) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_rgb(8, 14, 12));
    let grid = Color32::from_rgb(45, 70, 60);
    let axis = Color32::from_rgb(80, 115, 100);
    let dx = rect.width() / DIVISIONS_X as f32;
    let dy = rect.height() / DIVISIONS_Y as f32;
    for i in 0..=DIVISIONS_X as i32 {
        let x = rect.left() + i as f32 * dx;
        let color = if i == DIVISIONS_X as i32 / 2 {
            axis
        } else {
            grid
        };
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0_f32, color),
        );
    }
    for j in 0..=DIVISIONS_Y as i32 {
        let y = rect.top() + j as f32 * dy;
        let color = if j == DIVISIONS_Y as i32 / 2 {
            axis
        } else {
            grid
        };
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0_f32, color),
        );
    }
    // Minor ticks on the centre lines.
    let cx = rect.center().x;
    let cy = rect.center().y;
    for k in 0..(DIVISIONS_X as i32 * 5) {
        let x = rect.left() + k as f32 * dx / 5.0;
        painter.line_segment(
            [pos2(x, cy - 3.0), pos2(x, cy + 3.0)],
            Stroke::new(1.0_f32, axis),
        );
    }
    for k in 0..(DIVISIONS_Y as i32 * 5) {
        let y = rect.top() + k as f32 * dy / 5.0;
        painter.line_segment(
            [pos2(cx - 3.0, y), pos2(cx + 3.0, y)],
            Stroke::new(1.0_f32, axis),
        );
    }

    let window = scope.window();
    let shift: [f64; CHANNELS] = [0, 1, 2, 3].map(|ch| scope.display_offset(ch));
    let y_of = |ch: usize, v: f64| {
        let c = &scope.channels[ch];
        let divs = (v - shift[ch]) / c.volts_per_div + c.offset_div;
        cy - (divs as f32) * dy
    };

    if let Some(sweep) = scope.sweep() {
        for ch in 0..CHANNELS {
            if !scope.channels[ch].enabled {
                continue;
            }
            let points: Vec<_> = sweep
                .channel(ch)
                .into_iter()
                .map(|(t, v)| {
                    let x = rect.left() + ((t - sweep.start) / window) as f32 * rect.width();
                    pos2(x, y_of(ch, v).clamp(rect.top() - 2.0, rect.bottom() + 2.0))
                })
                .collect();
            if points.len() >= 2 {
                painter.add(Shape::line(
                    points,
                    Stroke::new(1.6_f32, CHANNEL_COLORS[ch]),
                ));
            }
        }
    }

    // Trigger position (top) and level (right edge).
    let tx = rect.left() + dx * PRE_TRIGGER_DIVISIONS as f32;
    let trig_color = CHANNEL_COLORS[scope.trigger.source];
    painter.add(Shape::convex_polygon(
        vec![
            pos2(tx - 5.0, rect.top()),
            pos2(tx + 5.0, rect.top()),
            pos2(tx, rect.top() + 7.0),
        ],
        trig_color,
        Stroke::NONE,
    ));
    let ly = y_of(scope.trigger.source, scope.trigger_level_absolute());
    if ly >= rect.top() && ly <= rect.bottom() {
        painter.add(Shape::convex_polygon(
            vec![
                pos2(rect.right(), ly - 5.0),
                pos2(rect.right(), ly + 5.0),
                pos2(rect.right() - 7.0, ly),
            ],
            trig_color,
            Stroke::NONE,
        ));
    }
    // Ground markers on the left edge.
    for ch in 0..CHANNELS {
        if scope.channels[ch].enabled {
            let gy = y_of(ch, 0.0);
            if gy >= rect.top() && gy <= rect.bottom() {
                painter.text(
                    pos2(rect.left() + 2.0, gy),
                    Align2::LEFT_CENTER,
                    format!("{}▸", ch + 1),
                    FontId::monospace(10.0),
                    CHANNEL_COLORS[ch],
                );
            }
        }
    }

    let (status, color) = match scope.status() {
        ScopeStatus::Triggered => ("Trig'd", Color32::from_rgb(90, 230, 110)),
        ScopeStatus::Auto => ("Auto", Color32::from_rgb(250, 210, 30)),
        ScopeStatus::Waiting => ("Wait", Color32::from_rgb(255, 140, 60)),
        ScopeStatus::Stopped => ("Stop", Color32::from_rgb(255, 80, 80)),
    };
    painter.text(
        rect.left_top() + vec2(18.0, 6.0),
        Align2::LEFT_TOP,
        status,
        FontId::monospace(12.0),
        color,
    );
    painter.text(
        rect.right_top() + vec2(-8.0, 6.0),
        Align2::RIGHT_TOP,
        format!("{}/div", format_value(scope.time_per_div, "s")),
        FontId::monospace(12.0),
        Color32::from_rgb(200, 220, 210),
    );
    let mut label_x = rect.left() + 8.0;
    for ch in 0..CHANNELS {
        if scope.channels[ch].enabled {
            let r = painter.text(
                pos2(label_x, rect.bottom() - 6.0),
                Align2::LEFT_BOTTOM,
                format!(
                    "CH{} {}/div",
                    ch + 1,
                    format_value(scope.channels[ch].volts_per_div, "V")
                ),
                FontId::monospace(11.0),
                CHANNEL_COLORS[ch],
            );
            label_x = r.right() + 14.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_two_five_sequence() {
        let v = one_two_five(1e-3, 50.0);
        assert_eq!(v.first().copied(), Some(1e-3));
        assert_eq!(v.last().copied(), Some(50.0));
        assert_eq!(v.len(), 15);
        assert!(v.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn probes_resolve_against_the_netlist() {
        let s = crate::examples::voltage_divider();
        let netlist = dunbrec_core::build_netlist(&s);
        let map = NodeMap::new(&s, &netlist);
        let mut bench = Bench {
            active: ProbeSlot::Scope(1),
            ..Default::default()
        };
        bench.connect(Point::new(7, -2), None, true).unwrap();
        bench.active = ProbeSlot::MeterPlus;
        bench.connect(Point::new(1, -2), None, true).unwrap();
        let sig = bench.resolve(&s, &netlist, &map);
        let vout = netlist.node_by_name("VOUT").unwrap();
        assert_eq!(sig.scope[1], Some(vout));
        assert_eq!(sig.scope[0], None);
        assert!(matches!(sig.meter, MeterSignal::Voltage(_, NodeId::GROUND)));

        // Current mode needs a part.
        bench.meter.mode = MeterMode::DcAmps;
        assert!(bench.connect(Point::new(1, -2), None, true).is_err());
        let r1 = s.components[1].id;
        bench.connect(Point::new(4, -2), Some(r1), false).unwrap();
        let sig = bench.resolve(&s, &netlist, &map);
        assert_eq!(sig.meter, MeterSignal::Current(1));
        assert_eq!(bench.markers(&s).len(), 2);
    }
}
