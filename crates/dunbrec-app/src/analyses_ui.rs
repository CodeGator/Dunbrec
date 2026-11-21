//! Analysis instruments: Bode plotter (AC analysis), curve tracer and DC
//! transfer plot (DC sweeps), and a function generator that drives a source
//! in the schematic.

use dunbrec_core::{
    ComponentId, ComponentKind as K, ModelLibrary, Netlist, NetlistBuilder, Polarity, Schematic,
    Waveform, units::format_value,
};
use dunbrec_sim::{DcSweeper, ac_analysis, dc_sweep, linear_steps, log_frequencies};
use egui::{Color32, RichText, Ui};

use crate::canvas::NodeMap;
use crate::document::Document;
use crate::instruments_ui::{Bench, ProbeSlot};
use crate::plot::{Axis, SERIES_COLORS, Series, plot};

/// Everything the analysis windows need from the app.
pub struct Inputs<'a> {
    pub doc: &'a mut Document,
    pub netlist: &'a Netlist,
    pub map: &'a NodeMap,
    pub bench: &'a mut Bench,
    pub probing: bool,
}

#[derive(Default)]
pub struct Analyses {
    pub show_bode: bool,
    pub show_tracer: bool,
    pub show_transfer: bool,
    pub show_fgen: bool,
    bode: Bode,
    tracer: Tracer,
    transfer: Transfer,
    fgen: FunctionGen,
}

impl Analyses {
    /// Show the open windows; returns true if a probe button was clicked.
    pub fn show(&mut self, ctx: &egui::Context, mut inp: Inputs) -> bool {
        let mut probe_clicked = false;
        if self.show_bode {
            let mut open = true;
            egui::Window::new("Bode plotter")
                .open(&mut open)
                .default_size([560.0, 520.0])
                .show(ctx, |ui| probe_clicked |= self.bode.ui(ui, &mut inp));
            self.show_bode = open;
        }
        if self.show_transfer {
            let mut open = true;
            egui::Window::new("DC transfer")
                .open(&mut open)
                .default_size([520.0, 400.0])
                .show(ctx, |ui| probe_clicked |= self.transfer.ui(ui, &mut inp));
            self.show_transfer = open;
        }
        if self.show_tracer {
            let mut open = true;
            egui::Window::new("Curve tracer")
                .open(&mut open)
                .default_size([520.0, 440.0])
                .show(ctx, |ui| self.tracer.ui(ui, &mut inp));
            self.show_tracer = open;
        }
        if self.show_fgen {
            let mut open = true;
            egui::Window::new("Function generator")
                .open(&mut open)
                .default_size([320.0, 240.0])
                .resizable(false)
                .show(ctx, |ui| self.fgen.ui(ui, inp.doc));
            self.show_fgen = open;
        }
        probe_clicked
    }
}

/// Designators of the independent sources in a netlist.
fn sources(netlist: &Netlist) -> Vec<(String, bool)> {
    netlist
        .elements
        .iter()
        .filter_map(|e| match e.kind {
            K::VoltageSource { .. } => Some((e.designator.clone(), true)),
            K::CurrentSource { .. } => Some((e.designator.clone(), false)),
            _ => None,
        })
        .collect()
}

fn source_combo(ui: &mut Ui, id: &str, netlist: &Netlist, chosen: &mut String) {
    let list = sources(netlist);
    if !list.iter().any(|(d, _)| d == chosen) {
        *chosen = list.first().map(|(d, _)| d.clone()).unwrap_or_default();
    }
    egui::ComboBox::from_id_salt(id)
        .selected_text(if chosen.is_empty() {
            "(no sources)"
        } else {
            chosen.as_str()
        })
        .show_ui(ui, |ui| {
            for (d, _) in list {
                ui.selectable_value(chosen, d.clone(), d);
            }
        });
}

fn error_label(ui: &mut Ui, e: &str) {
    ui.label(RichText::new(e).color(Color32::from_rgb(225, 70, 70)));
}

/// Problems that prevent an analysis, in a sentence.
fn netlist_problem(netlist: &Netlist) -> Option<String> {
    netlist
        .has_errors()
        .then(|| "Fix the problems listed under Checks first.".to_string())
}

// ------------------------------------------------------- Bode plotter ---

struct Bode {
    source: String,
    f_start: f64,
    f_stop: f64,
    per_decade: usize,
    result: Option<BodeData>,
    error: Option<String>,
}

impl Default for Bode {
    fn default() -> Self {
        Self {
            source: String::new(),
            f_start: 10.0,
            f_stop: 10e6,
            per_decade: 20,
            result: None,
            error: None,
        }
    }
}

struct BodeData {
    label: String,
    gain_db: Vec<(f64, f64)>,
    phase_deg: Vec<(f64, f64)>,
    peak: (f64, f64),
    /// Frequencies where the gain is 3 dB below its peak.
    corners: Vec<f64>,
}

/// Frequencies where `curve` (dB vs log frequency) crosses `level`.
fn crossings(curve: &[(f64, f64)], level: f64) -> Vec<f64> {
    curve
        .windows(2)
        .filter(|w| (w[0].1 - level) * (w[1].1 - level) < 0.0)
        .map(|w| {
            let t = (level - w[0].1) / (w[1].1 - w[0].1);
            10f64.powf(w[0].0.log10() + t * (w[1].0.log10() - w[0].0.log10()))
        })
        .collect()
}

/// Phase in degrees, unwrapped so it does not jump by 360°.
fn unwrap_degrees(raw: &[f64]) -> Vec<f64> {
    let mut out: Vec<f64> = Vec::with_capacity(raw.len());
    let mut offset = 0.0;
    for (i, &p) in raw.iter().enumerate() {
        if i > 0 {
            let prev = raw[i - 1];
            if p - prev > 180.0 {
                offset -= 360.0;
            } else if p - prev < -180.0 {
                offset += 360.0;
            }
        }
        out.push(p + offset);
    }
    out
}

impl Bode {
    fn run(&mut self, inp: &Inputs) {
        self.error = None;
        if let Some(p) = netlist_problem(inp.netlist) {
            self.error = Some(p);
            return;
        }
        let s = inp.doc.schematic();
        let Some(out) = inp
            .bench
            .bode_out
            .and_then(|p| inp.map.node_at(s, inp.netlist, p))
        else {
            self.error = Some("Connect the output probe: click 🔌 Probe, then a wire.".into());
            return;
        };
        if self.f_stop <= self.f_start {
            self.error = Some("The stop frequency must be above the start.".into());
            return;
        }
        let freqs = log_frequencies(self.f_start, self.f_stop, self.per_decade);
        match ac_analysis(inp.netlist, &s.models, &self.source, &freqs) {
            Ok(res) => {
                let h = res.response(out);
                let gain_db: Vec<(f64, f64)> = freqs
                    .iter()
                    .zip(&h)
                    .map(|(f, h)| (*f, 20.0 * h.norm().max(1e-300).log10()))
                    .collect();
                let phase =
                    unwrap_degrees(&h.iter().map(|h| h.arg().to_degrees()).collect::<Vec<_>>());
                let peak = gain_db
                    .iter()
                    .copied()
                    .fold(
                        (0.0, f64::NEG_INFINITY),
                        |a, b| if b.1 > a.1 { b } else { a },
                    );
                let corners = crossings(&gain_db, peak.1 - 3.0);
                self.result = Some(BodeData {
                    label: format!("{} → {}", self.source, inp.netlist.nodes[out.0].name),
                    phase_deg: freqs.iter().copied().zip(phase).collect(),
                    gain_db,
                    peak,
                    corners,
                });
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn ui(&mut self, ui: &mut Ui, inp: &mut Inputs) -> bool {
        let mut probe_clicked = false;
        let decades = [1.0, 10.0, 100.0, 1e3, 10e3, 100e3, 1e6, 10e6, 100e6, 1e9];
        ui.horizontal_wrapped(|ui| {
            ui.label("Input");
            source_combo(ui, "bode_src", inp.netlist, &mut self.source);
            ui.separator();
            ui.label("Output");
            probe_clicked = inp.bench.probe_button(ui, ProbeSlot::BodeOut, inp.probing);
            let target = inp
                .bench
                .bode_out
                .map(crate::instruments_ui::ProbeTarget::Node);
            ui.label(
                inp.bench
                    .describe(target, inp.doc.schematic(), inp.netlist, inp.map),
            );
        });
        ui.horizontal_wrapped(|ui| {
            for (label, value, id) in [
                ("From", &mut self.f_start, "f0"),
                ("to", &mut self.f_stop, "f1"),
            ] {
                ui.label(label);
                egui::ComboBox::from_id_salt(id)
                    .selected_text(format_value(*value, "Hz"))
                    .show_ui(ui, |ui| {
                        for f in decades {
                            ui.selectable_value(value, f, format_value(f, "Hz"));
                        }
                    });
            }
            ui.label("Points/decade");
            for n in [10, 20, 50] {
                ui.selectable_value(&mut self.per_decade, n, n.to_string());
            }
            if ui.button("▶ Run").clicked() {
                self.run(inp);
            }
        });
        if let Some(e) = &self.error {
            error_label(ui, e);
        }
        if let Some(r) = &self.result {
            let freq_axis = Axis {
                label: "Frequency",
                unit: "Hz",
                log: true,
            };
            ui.label(RichText::new(&r.label).strong());
            plot(
                ui,
                200.0,
                &freq_axis,
                &Axis {
                    label: "Gain (dB)",
                    unit: "",
                    log: false,
                },
                &[Series {
                    label: String::new(),
                    color: SERIES_COLORS[0],
                    points: r.gain_db.clone(),
                }],
            );
            plot(
                ui,
                150.0,
                &freq_axis,
                &Axis {
                    label: "Phase (°)",
                    unit: "",
                    log: false,
                },
                &[Series {
                    label: String::new(),
                    color: SERIES_COLORS[1],
                    points: r.phase_deg.clone(),
                }],
            );
            let corners: Vec<String> = r.corners.iter().map(|f| format_value(*f, "Hz")).collect();
            ui.label(format!(
                "Peak gain {:.2} dB at {}{}",
                r.peak.1,
                format_value(r.peak.0, "Hz"),
                if corners.is_empty() {
                    String::new()
                } else {
                    format!(" · −3 dB at {}", corners.join(", "))
                }
            ));
            ui.label(
                RichText::new(
                    "Small-signal analysis: the circuit is linearised at its DC operating point.",
                )
                .weak()
                .small(),
            );
        }
        probe_clicked
    }
}

// ------------------------------------------------------- DC transfer ---

struct Transfer {
    source: String,
    start: f64,
    stop: f64,
    points: usize,
    result: Option<TransferResult>,
    error: Option<String>,
}

/// A transfer curve: its title, the points, and whether the swept source
/// is a voltage (else a current).
struct TransferResult {
    label: String,
    points: Vec<(f64, f64)>,
    voltage_source: bool,
}

impl Default for Transfer {
    fn default() -> Self {
        Self {
            source: String::new(),
            start: 0.0,
            stop: 5.0,
            points: 101,
            result: None,
            error: None,
        }
    }
}

impl Transfer {
    fn run(&mut self, inp: &Inputs) {
        self.error = None;
        if let Some(p) = netlist_problem(inp.netlist) {
            self.error = Some(p);
            return;
        }
        let s = inp.doc.schematic();
        let Some(out) = inp
            .bench
            .sweep_out
            .and_then(|p| inp.map.node_at(s, inp.netlist, p))
        else {
            self.error = Some("Connect the output probe: click 🔌 Probe, then a wire.".into());
            return;
        };
        let values = linear_steps(self.start, self.stop, self.points.max(2));
        let is_voltage = sources(inp.netlist)
            .iter()
            .any(|(d, v)| *d == self.source && *v);
        match dc_sweep(inp.netlist, &s.models, &self.source, &values) {
            Ok(ops) => {
                let pts = values
                    .iter()
                    .zip(&ops)
                    .map(|(x, op)| (*x, op.voltage(out)))
                    .collect();
                let label = format!("V({}) vs {}", inp.netlist.nodes[out.0].name, self.source);
                self.result = Some(TransferResult {
                    label,
                    points: pts,
                    voltage_source: is_voltage,
                });
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn ui(&mut self, ui: &mut Ui, inp: &mut Inputs) -> bool {
        let mut probe_clicked = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Sweep");
            source_combo(ui, "transfer_src", inp.netlist, &mut self.source);
            ui.add(
                egui::DragValue::new(&mut self.start)
                    .speed(0.05)
                    .prefix("from "),
            );
            ui.add(
                egui::DragValue::new(&mut self.stop)
                    .speed(0.05)
                    .prefix("to "),
            );
            ui.add(
                egui::DragValue::new(&mut self.points)
                    .range(2..=2001)
                    .suffix(" points"),
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Output");
            probe_clicked = inp.bench.probe_button(ui, ProbeSlot::SweepOut, inp.probing);
            let target = inp
                .bench
                .sweep_out
                .map(crate::instruments_ui::ProbeTarget::Node);
            ui.label(
                inp.bench
                    .describe(target, inp.doc.schematic(), inp.netlist, inp.map),
            );
            if ui.button("▶ Run").clicked() {
                self.run(inp);
            }
        });
        if let Some(e) = &self.error {
            error_label(ui, e);
        }
        if let Some(r) = &self.result {
            ui.label(RichText::new(&r.label).strong());
            let x_unit = if r.voltage_source { "V" } else { "A" };
            plot(
                ui,
                280.0,
                &Axis {
                    label: "Source",
                    unit: x_unit,
                    log: false,
                },
                &Axis {
                    label: "Output",
                    unit: "V",
                    log: false,
                },
                &[Series {
                    label: String::new(),
                    color: SERIES_COLORS[0],
                    points: r.points.clone(),
                }],
            );
        }
        probe_clicked
    }
}

// ------------------------------------------------------- curve tracer ---

#[derive(Default)]
struct Tracer {
    part: Option<ComponentId>,
    v_max: f64,
    steps: usize,
    step: f64,
    result: Option<(Vec<Series>, Axis, Axis)>,
    error: Option<String>,
}

/// The test fixture for a device: (netlist, swept source, stepped source).
fn fixture(kind: &K) -> Option<(Netlist, &'static str, Option<&'static str>)> {
    let b = NetlistBuilder::new();
    Some(match kind {
        K::Diode { .. } => (
            b.element(
                "VA",
                K::VoltageSource {
                    waveform: Waveform::Dc(0.0),
                },
                &["a", "0"],
            )
            .element("DUT", kind.clone(), &["a", "0"])
            .build(),
            "VA",
            None,
        ),
        K::Bjt { .. } => (
            b.element(
                "VCE",
                K::VoltageSource {
                    waveform: Waveform::Dc(0.0),
                },
                &["c", "0"],
            )
            .element(
                "IB",
                K::CurrentSource {
                    waveform: Waveform::Dc(0.0),
                },
                &["0", "b"],
            )
            .element("DUT", kind.clone(), &["c", "b", "0"])
            .build(),
            "VCE",
            Some("IB"),
        ),
        K::Mosfet { .. } => (
            b.element(
                "VDS",
                K::VoltageSource {
                    waveform: Waveform::Dc(0.0),
                },
                &["d", "0"],
            )
            .element(
                "VGS",
                K::VoltageSource {
                    waveform: Waveform::Dc(0.0),
                },
                &["g", "0"],
            )
            .element("DUT", kind.clone(), &["d", "g", "0"])
            .build(),
            "VDS",
            Some("VGS"),
        ),
        _ => return None,
    })
}

impl Tracer {
    fn defaults_for(&mut self, kind: &K) {
        (self.v_max, self.steps, self.step) = match kind {
            K::Diode { .. } => (1.0, 1, 0.0),
            K::Bjt { .. } => (10.0, 5, 10e-6),
            _ => (10.0, 5, 1.0),
        };
        self.result = None;
        self.error = None;
    }

    fn run(&mut self, kind: &K, models: &ModelLibrary) -> Result<(), String> {
        let (netlist, swept, stepped) = fixture(kind).ok_or("not a semiconductor")?;
        // PNP and P-channel parts work with negative voltages and currents.
        let sign = match kind {
            K::Bjt {
                polarity: Polarity::P,
                ..
            }
            | K::Mosfet {
                polarity: Polarity::P,
                ..
            } => -1.0,
            _ => 1.0,
        };
        let mut sweeper = DcSweeper::new(&netlist, models).map_err(|e| e.to_string())?;
        let volts = linear_steps(0.0, self.v_max, 101);
        let levels: Vec<f64> = match stepped {
            Some(_) => (1..=self.steps.max(1))
                .map(|k| k as f64 * self.step)
                .collect(),
            None => vec![0.0],
        };
        let mut series = Vec::new();
        for (i, level) in levels.iter().enumerate() {
            if let Some(name) = stepped {
                sweeper
                    .set_source(name, sign * level)
                    .map_err(|e| e.to_string())?;
            }
            let mut pts = Vec::with_capacity(volts.len());
            for &v in &volts {
                sweeper
                    .set_source(swept, sign * v)
                    .map_err(|e| e.to_string())?;
                let op = sweeper.solve().map_err(|e| e.to_string())?;
                pts.push((v, sign * op.current("DUT").unwrap_or(f64::NAN)));
            }
            let label = match (stepped, kind) {
                (Some(_), K::Bjt { .. }) => format!("IB = {}", format_value(*level, "A")),
                (Some(_), _) => format!("VGS = {}", format_value(sign * level, "V")),
                _ => String::new(),
            };
            series.push(Series {
                label,
                color: SERIES_COLORS[i % SERIES_COLORS.len()],
                points: pts,
            });
        }
        let (x, y) = match kind {
            K::Diode { .. } => (
                Axis {
                    label: "Voltage",
                    unit: "V",
                    log: false,
                },
                Axis {
                    label: "Current",
                    unit: "A",
                    log: false,
                },
            ),
            K::Bjt { .. } => (
                Axis {
                    label: "|VCE|",
                    unit: "V",
                    log: false,
                },
                Axis {
                    label: "|IC|",
                    unit: "A",
                    log: false,
                },
            ),
            _ => (
                Axis {
                    label: "|VDS|",
                    unit: "V",
                    log: false,
                },
                Axis {
                    label: "|ID|",
                    unit: "A",
                    log: false,
                },
            ),
        };
        self.result = Some((series, x, y));
        Ok(())
    }

    fn ui(&mut self, ui: &mut Ui, inp: &mut Inputs) {
        let s = inp.doc.schematic();
        let parts: Vec<(ComponentId, String, K)> = s
            .components
            .iter()
            .filter(|c| matches!(c.kind, K::Diode { .. } | K::Bjt { .. } | K::Mosfet { .. }))
            .map(|c| (c.id, c.designator.clone(), c.kind.clone()))
            .collect();
        if parts.is_empty() {
            ui.label("Place a diode, transistor or MOSFET to see its characteristic curves.");
            return;
        }
        if !self
            .part
            .is_some_and(|p| parts.iter().any(|(id, _, _)| *id == p))
        {
            self.part = Some(parts[0].0);
            self.defaults_for(&parts[0].2);
        }
        let Some((_, name, kind)) = parts
            .iter()
            .find(|(id, _, _)| Some(*id) == self.part)
            .cloned()
        else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label("Device");
            egui::ComboBox::from_id_salt("tracer_part")
                .selected_text(format!("{name} ({})", kind.model_name().unwrap_or("")))
                .show_ui(ui, |ui| {
                    for (id, d, k) in &parts {
                        if ui.selectable_label(self.part == Some(*id), d).clicked() {
                            self.part = Some(*id);
                            self.defaults_for(k);
                        }
                    }
                });
            ui.add(
                egui::DragValue::new(&mut self.v_max)
                    .speed(0.1)
                    .range(0.1..=1000.0)
                    .prefix("up to ")
                    .suffix(" V"),
            );
            match kind {
                K::Bjt { .. } => {
                    ui.add(
                        egui::DragValue::new(&mut self.steps)
                            .range(1..=10)
                            .suffix(" steps"),
                    );
                    let mut ua = self.step * 1e6;
                    if ui
                        .add(
                            egui::DragValue::new(&mut ua)
                                .speed(1.0)
                                .range(0.1..=10_000.0)
                                .prefix("of ")
                                .suffix(" µA"),
                        )
                        .changed()
                    {
                        self.step = ua * 1e-6;
                    }
                }
                K::Mosfet { .. } => {
                    ui.add(
                        egui::DragValue::new(&mut self.steps)
                            .range(1..=10)
                            .suffix(" steps"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut self.step)
                            .speed(0.05)
                            .range(0.01..=100.0)
                            .prefix("of ")
                            .suffix(" V"),
                    );
                }
                _ => {}
            }
            if ui.button("▶ Run").clicked() {
                self.error = self.run(&kind, &s.models).err();
            }
        });
        if let Some(e) = &self.error {
            error_label(ui, e);
        }
        if let Some((series, x, y)) = &self.result {
            plot(ui, 300.0, x, y, series);
            ui.label(RichText::new("The part alone, tested like on a bench curve tracer; the rest of the circuit is not involved.").weak().small());
        }
    }
}

// --------------------------------------------------- function generator ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Sine,
    Square,
    Triangle,
    Dc,
}

struct FunctionGen {
    target: Option<ComponentId>,
    shape: Shape,
    frequency: f64,
    amplitude: f64,
    offset: f64,
    duty: f64,
}

impl Default for FunctionGen {
    fn default() -> Self {
        Self {
            target: None,
            shape: Shape::Sine,
            frequency: 1e3,
            amplitude: 1.0,
            offset: 0.0,
            duty: 0.5,
        }
    }
}

impl FunctionGen {
    fn waveform(&self) -> Waveform {
        let period = 1.0 / self.frequency.max(1e-12);
        let (lo, hi) = (self.offset - self.amplitude, self.offset + self.amplitude);
        match self.shape {
            Shape::Sine => Waveform::Sine {
                offset: self.offset,
                amplitude: self.amplitude,
                frequency: self.frequency,
                delay: 0.0,
                damping: 0.0,
                phase: 0.0,
            },
            Shape::Square => {
                let edge = period * 1e-3;
                Waveform::Pulse {
                    v1: lo,
                    v2: hi,
                    delay: 0.0,
                    rise: edge,
                    fall: edge,
                    width: (period * self.duty - edge).max(0.0),
                    period,
                }
            }
            Shape::Triangle => Waveform::Pulse {
                v1: lo,
                v2: hi,
                delay: 0.0,
                rise: period / 2.0,
                fall: period / 2.0,
                width: 0.0,
                period,
            },
            Shape::Dc => Waveform::Dc(self.offset),
        }
    }

    /// Take the settings from a source's present waveform.
    fn load(&mut self, w: &Waveform) {
        match w {
            Waveform::Sine {
                offset,
                amplitude,
                frequency,
                ..
            } => {
                (self.shape, self.offset, self.amplitude, self.frequency) =
                    (Shape::Sine, *offset, *amplitude, *frequency);
            }
            Waveform::Pulse {
                v1,
                v2,
                rise,
                width,
                period,
                ..
            } if *period > 0.0 => {
                self.shape = if *width == 0.0 {
                    Shape::Triangle
                } else {
                    Shape::Square
                };
                self.offset = (v1 + v2) / 2.0;
                self.amplitude = (v2 - v1) / 2.0;
                self.frequency = 1.0 / period;
                self.duty = ((width + rise) / period).clamp(0.01, 0.99);
            }
            Waveform::Dc(v) => (self.shape, self.offset) = (Shape::Dc, *v),
            _ => {}
        }
    }

    fn ui(&mut self, ui: &mut Ui, doc: &mut Document) {
        let s: &Schematic = doc.schematic();
        let targets: Vec<(ComponentId, String, Waveform)> = s
            .components
            .iter()
            .filter_map(|c| match &c.kind {
                K::VoltageSource { waveform } => {
                    Some((c.id, c.designator.clone(), waveform.clone()))
                }
                _ => None,
            })
            .collect();
        if targets.is_empty() {
            ui.label("Place a voltage source: the generator drives it.");
            return;
        }
        if !self
            .target
            .is_some_and(|t| targets.iter().any(|(id, _, _)| *id == t))
        {
            self.target = Some(targets[0].0);
            self.load(&targets[0].2);
        }
        let mut changed = false;
        let mut dragging = false;
        egui::Grid::new("fgen")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Output");
                let current = targets
                    .iter()
                    .find(|(id, _, _)| Some(*id) == self.target)
                    .map_or("", |t| t.1.as_str());
                egui::ComboBox::from_id_salt("fgen_target")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (id, d, w) in &targets {
                            if ui.selectable_label(self.target == Some(*id), d).clicked() {
                                self.target = Some(*id);
                                self.load(w);
                            }
                        }
                    });
                ui.end_row();
                ui.label("Waveform");
                ui.horizontal(|ui| {
                    for (shape, label) in [
                        (Shape::Sine, "〰 Sine"),
                        (Shape::Square, "⊓ Square"),
                        (Shape::Triangle, "△ Triangle"),
                        (Shape::Dc, "— DC"),
                    ] {
                        changed |= ui.selectable_value(&mut self.shape, shape, label).changed();
                    }
                });
                ui.end_row();
                let mut drag = |ui: &mut Ui,
                                label: &str,
                                value: &mut f64,
                                speed: f64,
                                range: std::ops::RangeInclusive<f64>,
                                suffix: &str| {
                    ui.label(label);
                    let r = ui.add(
                        egui::DragValue::new(value)
                            .speed(speed)
                            .range(range)
                            .suffix(suffix)
                            .max_decimals(4),
                    );
                    ui.end_row();
                    changed |= r.changed();
                    dragging |= r.dragged() && !r.drag_started();
                };
                if self.shape != Shape::Dc {
                    let speed = self.frequency * 0.01;
                    drag(
                        ui,
                        "Frequency",
                        &mut self.frequency,
                        speed,
                        0.001..=1e9,
                        " Hz",
                    );
                    drag(ui, "Amplitude", &mut self.amplitude, 0.01, 0.0..=1e4, " V");
                }
                drag(
                    ui,
                    if self.shape == Shape::Dc {
                        "Level"
                    } else {
                        "Offset"
                    },
                    &mut self.offset,
                    0.01,
                    -1e4..=1e4,
                    " V",
                );
                if self.shape == Shape::Square {
                    drag(ui, "Duty cycle", &mut self.duty, 0.005, 0.01..=0.99, "");
                }
            });
        ui.label(
            RichText::new(format!(
                "Period {}",
                format_value(1.0 / self.frequency.max(1e-12), "s")
            ))
            .weak(),
        );
        ui.label(
            RichText::new("Changes apply at once, also while the simulation runs.")
                .weak()
                .small(),
        );

        if changed && let Some(id) = self.target {
            let w = self.waveform();
            let apply = move |s: &mut Schematic| {
                if let Some(c) = s.component_mut(id) {
                    c.kind = K::VoltageSource { waveform: w };
                }
            };
            // One undo step per drag, not one per frame.
            if dragging {
                apply(doc.schematic_mut());
            } else {
                doc.edit(apply);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::interpolate;

    #[test]
    fn corner_frequencies_and_phase_unwrap() {
        // First-order low-pass, fc = 1 kHz, sampled on a log grid.
        let curve: Vec<(f64, f64)> = log_frequencies(10.0, 1e5, 50)
            .into_iter()
            .map(|f| (f, -10.0 * (1.0 + (f / 1e3).powi(2)).log10()))
            .collect();
        let c = crossings(&curve, -3.0);
        assert_eq!(c.len(), 1);
        assert!((c[0] - 1e3).abs() / 1e3 < 0.01, "{c:?}");
        assert_eq!(
            unwrap_degrees(&[170.0, -175.0, -160.0]),
            vec![170.0, 185.0, 200.0]
        );
    }

    #[test]
    fn generator_waveforms() {
        let mut g = FunctionGen {
            shape: Shape::Square,
            frequency: 1e3,
            amplitude: 2.0,
            offset: 1.0,
            ..Default::default()
        };
        let w = g.waveform();
        assert_eq!(w.value_at(0.25e-3), 3.0);
        assert_eq!(w.value_at(0.75e-3), -1.0);
        g.shape = Shape::Triangle;
        let w = g.waveform();
        assert!((w.value_at(0.25e-3) - 1.0).abs() < 1e-9);
        assert!((w.value_at(0.5e-3) - 3.0).abs() < 1e-9);
        // Settings round-trip through a source's waveform.
        let mut h = FunctionGen::default();
        h.load(&w);
        assert_eq!(h.shape, Shape::Triangle);
        assert!((h.frequency - 1e3).abs() < 1e-9 && (h.amplitude - 2.0).abs() < 1e-12);
    }

    #[test]
    fn curve_tracer_bjt_family() {
        let mut lib = ModelLibrary::new();
        lib.insert(
            dunbrec_core::DeviceModel::new("QP", dunbrec_core::ModelKind::Pnp).with("BF", 50.0),
        );
        let kind = K::Bjt {
            polarity: Polarity::P,
            model: "QP".into(),
        };
        let mut t = Tracer::default();
        t.defaults_for(&kind);
        t.run(&kind, &lib).unwrap();
        let (series, _, _) = t.result.unwrap();
        assert_eq!(series.len(), 5);
        // Forward active, well clear of saturation: |IC| = BF·|IB|.
        let ic = interpolate(&series[1].points, 5.0).unwrap();
        assert!((ic - 50.0 * 20e-6).abs() / 1e-3 < 1e-3, "{ic}");
    }
}
