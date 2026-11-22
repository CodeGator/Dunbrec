//! The microcontroller debugger: a floating window with run controls,
//! registers, a disassembly listing with a breakpoint gutter, and a memory
//! view. It shows the *committed* state of the chip the simulation worker
//! reports when it halts (see `live.rs`), never a live view of a running one.
//!
//! Breakpoints belong to the debugger, keyed by the part's designator, so
//! they survive restarts and can be set before a simulation starts.

use crate::float::{self, Spec};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use dunbrec_core::{ComponentKind, Netlist, SOURCE_PREFIX, SourceLanguage, units::format_value};
use dunbrec_mcu::CpuSnapshot;
use dunbrec_mcu::asm_text::{Assembled, assemble};
use dunbrec_mcu::disasm::{Line, disassemble};
use dunbrec_sim::{HaltReason, McuHalt, McuInfo};
use egui::{Color32, RichText, ScrollArea, Sense, Ui, vec2};

use crate::live::{Command, DebugSetup, LiveSim};

/// What the window asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Start (or restart) the simulation paused at time zero, so the first
    /// instruction can be stepped.
    StartPaused,
}

/// The source text a program was built from, so the listing can show the
/// line each instruction came from.
struct SourceMap {
    name: String,
    /// The source, one entry per line.
    text: Vec<String>,
    /// Source line (from 1) of the statement at each word address.
    built: Assembled,
}

impl SourceMap {
    /// The source line of the instruction at word address `address`: its
    /// number and its text.
    fn at(&self, address: usize) -> Option<(usize, &str)> {
        let line = self.built.line_for_address(address)?;
        Some((line, self.text.get(line - 1)?.as_str()))
    }
}

/// Build the source file a `source:<name>` firmware reference, or a source
/// file's path, names, for the listing. None for any other firmware, or one
/// that does not build.
fn source_map(firmware: &str, netlist: &Netlist) -> Option<SourceMap> {
    let reference = firmware.trim();
    let name = reference
        .strip_prefix(SOURCE_PREFIX)
        .unwrap_or(reference)
        .trim();
    let file = netlist.sources.get(name)?;
    if file.language != SourceLanguage::Asm {
        return None;
    }
    Some(SourceMap {
        name: name.to_string(),
        text: file.text.lines().map(str::to_string).collect(),
        built: assemble(&file.text).ok()?,
    })
}

/// A microcontroller of the circuit and its program.
struct Mcu {
    designator: String,
    clock: f64,
    /// Disassembly of the firmware, by address.
    lines: Vec<Line>,
    /// Where the instructions came from, for firmware built from source.
    source: Option<SourceMap>,
    /// Why the firmware could not be loaded, if it could not.
    error: Option<String>,
}

/// Where a microcontroller stopped last.
struct Stopped {
    snapshot: CpuSnapshot,
    /// The snapshot before it, to show what changed.
    previous: Option<CpuSnapshot>,
    reason: Option<HaltReason>,
}

/// What the debugger knows about a source file, by line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SourceMarks {
    pub breakpoints: BTreeSet<usize>,
    /// The line the program counter is on, while the circuit is paused.
    pub pc_line: Option<usize>,
}

#[derive(Default)]
pub struct Debugger {
    pub show: bool,
    mcus: Vec<Mcu>,
    selected: usize,
    /// Breakpoints (word addresses) by designator.
    breakpoints: BTreeMap<String, BTreeSet<usize>>,
    stopped: Option<Stopped>,
    /// The simulation ran since the last snapshot: ask for a new one when
    /// it pauses.
    stale: bool,
    /// Scroll the listing to the program counter on the next frame.
    scroll_to_pc: bool,
}

// ------------------------------------------------------- pure helpers ---

/// The circuit's microcontrollers in the order the simulation indexes them:
/// (designator, firmware reference, clock).
fn mcu_parts(netlist: &Netlist) -> Vec<(String, String, f64)> {
    netlist
        .elements
        .iter()
        .filter_map(|e| match &e.kind {
            ComponentKind::Mcu { firmware, clock } => {
                Some((e.designator.clone(), firmware.clone(), *clock))
            }
            _ => None,
        })
        .collect()
}

/// Flash words of an image (little endian bytes).
fn words(image: &[u8]) -> Vec<u16> {
    image
        .chunks(2)
        .map(|p| p[0] as u16 | (*p.get(1).unwrap_or(&0xFF) as u16) << 8)
        .collect()
}

/// The program as a listing: every word up to the last one that is not
/// erased flash.
fn program_lines(flash: &[u16]) -> Vec<Line> {
    let end = flash
        .iter()
        .rposition(|&w| w != 0xFFFF)
        .map_or(0, |i| i + 1);
    disassemble(&flash[..end], 0, end)
}

/// Index of the listing row at word address `pc`.
fn row_of(lines: &[Line], pc: usize) -> Option<usize> {
    let i = lines.partition_point(|l| l.address < pc);
    (lines.get(i)?.address == pc).then_some(i)
}

/// Which general registers differ between two snapshots.
fn changed_registers(before: Option<&CpuSnapshot>, now: &CpuSnapshot) -> [bool; 32] {
    std::array::from_fn(|r| before.is_some_and(|b| b.regs[r] != now.regs[r]))
}

/// One row of the memory view: the bytes at `row * 16` as hex and ASCII.
fn memory_row(data: &[u8], row: usize) -> (String, String) {
    let bytes = data.get(row * 16..).unwrap_or(&[]);
    let bytes = &bytes[..bytes.len().min(16)];
    let hex = bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    let ascii = bytes
        .iter()
        .map(|&b| {
            if (0x20..0x7F).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    (hex, ascii)
}

const CHANGED: Color32 = Color32::from_rgb(255, 190, 60);
const BREAKPOINT: Color32 = Color32::from_rgb(220, 50, 50);

impl Debugger {
    /// Read each microcontroller's firmware (relative paths against
    /// `firmware_dir`) and disassemble it. Call when the circuit changes
    /// and when a simulation starts, since the file is reloaded then.
    pub fn load(&mut self, netlist: &Netlist, firmware_dir: Option<&Path>) {
        let parts = mcu_parts(netlist);
        self.mcus = parts
            .into_iter()
            .map(|(designator, firmware, clock)| {
                match dunbrec_sim::load_firmware(&firmware, firmware_dir, &netlist.sources) {
                    Ok(image) => Mcu {
                        source: source_map(&firmware, netlist),
                        designator,
                        clock,
                        lines: program_lines(&words(&image)),
                        error: None,
                    },
                    Err(e) => Mcu {
                        designator,
                        clock,
                        lines: Vec::new(),
                        source: None,
                        error: Some(e),
                    },
                }
            })
            .collect();
        self.selected = self.selected.min(self.mcus.len().saturating_sub(1));
    }

    /// Breakpoints (and whether to start paused) for a new simulation of
    /// `netlist`.
    pub fn setup(&self, netlist: &Netlist, start_paused: bool) -> DebugSetup {
        let breakpoints = mcu_parts(netlist)
            .iter()
            .enumerate()
            .filter_map(|(part, (designator, _, _))| {
                let set = self.breakpoints.get(designator)?;
                (!set.is_empty()).then(|| (part, set.iter().copied().collect()))
            })
            .collect();
        DebugSetup {
            breakpoints,
            start_paused,
        }
    }

    pub fn sim_started(&mut self) {
        self.stopped = None;
        self.stale = true;
        self.scroll_to_pc = true;
    }

    pub fn sim_stopped(&mut self) {
        self.stopped = None;
    }

    /// A breakpoint or step halted the simulation. A breakpoint opens the
    /// window, so the pause is not a mystery.
    ///
    /// `sim_mcus` is the running simulation's list: its part numbers are
    /// only meaningful against it.
    pub fn on_halt(&mut self, halt: McuHalt, snapshot: CpuSnapshot, sim_mcus: &[McuInfo]) {
        if halt.reason == HaltReason::Breakpoint {
            self.show = true;
        }
        if let Some(i) = self.index_of(halt.part, sim_mcus) {
            self.selected = i;
        }
        self.store(snapshot, Some(halt.reason));
    }

    /// The state asked for with a snapshot request.
    pub fn on_state(&mut self, part: usize, snapshot: CpuSnapshot, sim_mcus: &[McuInfo]) {
        if self.index_of(part, sim_mcus) == Some(self.selected) {
            self.store(snapshot, None);
        }
    }

    /// Our index of the microcontroller the simulation calls `part`.
    fn index_of(&self, part: usize, sim_mcus: &[McuInfo]) -> Option<usize> {
        let designator = &sim_mcus.get(part)?.designator;
        self.mcus.iter().position(|m| &m.designator == designator)
    }

    /// The selected microcontroller's number in the running simulation, or
    /// in the one that would start now if none runs. `None` if the running
    /// simulation does not have it (the circuit changed since it started).
    fn part(&self, live: &Option<LiveSim>) -> Option<usize> {
        let designator = &self.mcus.get(self.selected)?.designator;
        match live {
            Some(l) => l
                .mcus
                .iter()
                .find(|m| &m.designator == designator)
                .map(|m| m.part),
            None => Some(self.selected),
        }
    }

    fn store(&mut self, snapshot: CpuSnapshot, reason: Option<HaltReason>) {
        let previous = self.stopped.take().map(|s| s.snapshot);
        self.stopped = Some(Stopped {
            snapshot,
            previous,
            reason,
        });
        self.stale = false;
        self.scroll_to_pc = true;
    }

    /// Breakpoints set on all microcontrollers.
    fn breakpoint_count(&self) -> usize {
        self.breakpoints.values().map(BTreeSet::len).sum()
    }

    /// Clear every breakpoint, on every microcontroller, including in a
    /// running simulation.
    fn clear_breakpoints(&mut self, live: &mut Option<LiveSim>) {
        let cleared: Vec<String> = self
            .breakpoints
            .iter()
            .filter(|(_, set)| !set.is_empty())
            .map(|(designator, _)| designator.clone())
            .collect();
        self.breakpoints.clear();
        if let Some(live) = live {
            for designator in cleared {
                let part = live
                    .mcus
                    .iter()
                    .find(|m| m.designator == designator)
                    .map(|m| m.part);
                if let Some(part) = part {
                    live.send(Command::SetBreakpoints {
                        part,
                        words: Vec::new(),
                    });
                }
            }
        }
    }

    fn toggle_breakpoint(&mut self, live: &mut Option<LiveSim>, word: usize) {
        let Some(mcu) = self.mcus.get(self.selected) else {
            return;
        };
        let set = self.breakpoints.entry(mcu.designator.clone()).or_default();
        if !set.remove(&word) {
            set.insert(word);
        }
        let words: Vec<usize> = set.iter().copied().collect();
        if let (Some(part), Some(live)) = (self.part(live), live.as_mut()) {
            live.send(Command::SetBreakpoints { part, words });
        }
    }

    // ----------------------------------------------- source file marks ---

    /// The microcontroller whose program was built from exactly this text of
    /// source file `name`. (After an edit the listing is out of date until
    /// the app reloads it; nothing matches meanwhile.)
    fn source_mcu(&self, name: &str, text: &str) -> Option<usize> {
        self.mcus.iter().position(|m| {
            m.source.as_ref().is_some_and(|s| {
                s.name == name && s.text.iter().map(String::as_str).eq(text.lines())
            })
        })
    }

    /// Breakpoints and the current instruction of source file `name`, as
    /// line numbers, for the code panel's gutter.
    pub fn source_marks(&self, name: &str, text: &str, live: &Option<LiveSim>) -> SourceMarks {
        let mut marks = SourceMarks::default();
        let Some(index) = self.source_mcu(name, text) else {
            return marks;
        };
        let mcu = &self.mcus[index];
        let map = mcu.source.as_ref().expect("matched on its source");
        if let Some(set) = self.breakpoints.get(&mcu.designator) {
            marks.breakpoints = set
                .iter()
                .filter_map(|&word| map.built.line_for_address(word))
                .collect();
        }
        if index == self.selected
            && live.as_ref().is_some_and(|l| l.paused)
            && let Some(stopped) = &self.stopped
        {
            marks.pc_line = map.built.line_for_address(stopped.snapshot.pc);
        }
        marks
    }

    /// Set or clear the breakpoint on `line` of source file `name`. False
    /// if the line has no instruction (or the listing is out of date).
    pub fn toggle_source_line(
        &mut self,
        live: &mut Option<LiveSim>,
        name: &str,
        text: &str,
        line: usize,
    ) -> bool {
        let Some(index) = self.source_mcu(name, text) else {
            return false;
        };
        let map = self.mcus[index]
            .source
            .as_ref()
            .expect("matched on its source");
        let Some(&(word, _)) = map.built.lines.iter().find(|(_, l)| *l == line) else {
            return false;
        };
        self.selected = index;
        self.toggle_breakpoint(live, word);
        true
    }

    /// What the assistant is told about the microcontroller the debugger is
    /// showing: only while the simulation is paused and the window has
    /// fetched its state since (otherwise the registers would be old).
    pub fn summary(&self, paused: bool) -> Option<Vec<String>> {
        if !paused || !self.show || self.stale {
            return None;
        }
        let mcu = self.mcus.get(self.selected)?;
        let snap = &self.stopped.as_ref()?.snapshot;
        let flags: String = "CZNVSHTI"
            .chars()
            .enumerate()
            .filter(|(bit, _)| snap.sreg & (1 << bit) != 0)
            .map(|(_, c)| c)
            .collect();
        let mut lines = vec![format!(
            "{} is paused: PC = 0x{:04X} (word address), SP = 0x{:04X}, SREG = 0x{:02X} \
                 (flags set: {}), cycle {}, sleeping: {}.",
            mcu.designator,
            snap.pc,
            snap.sp,
            snap.sreg,
            if flags.is_empty() { "none" } else { &flags },
            snap.cycle,
            if snap.sleeping { "yes" } else { "no" },
        )];
        for (row, chunk) in snap.regs.chunks(8).enumerate() {
            let values: Vec<String> = chunk.iter().map(|r| format!("{r:02X}")).collect();
            lines.push(format!(
                "r{}..r{}: {}",
                row * 8,
                row * 8 + 7,
                values.join(" ")
            ));
        }
        if let Some(fault) = &snap.fault {
            lines.push(format!("Fault: {fault}"));
        }
        Some(lines)
    }

    // ------------------------------------------------------------ window ---

    /// Draw the window if it is open.
    pub fn window(&mut self, ctx: &egui::Context, live: &mut Option<LiveSim>) -> Action {
        if !self.show {
            return Action::None;
        }
        let mut action = Action::None;
        let mut open = true;
        let spec = Spec {
            title: float::MCU_DEBUGGER,
            size: [640.0, 620.0],
            resizable: true,
            pos: None,
        };
        float::window(ctx, spec, &mut open, |ui| {
            action = self.contents(ui, live);
        });
        self.show = open;
        action
    }

    fn contents(&mut self, ui: &mut Ui, live: &mut Option<LiveSim>) -> Action {
        let mut action = Action::None;
        if self.mcus.is_empty() {
            ui.label("This circuit has no microcontroller.");
            return action;
        }

        // The running simulation's number for this microcontroller.
        let part = self.part(live);

        // A paused simulation shows its state: ask for it once.
        match (live.as_mut(), part) {
            (Some(l), Some(part)) if l.paused && self.stale => {
                l.send(Command::Snapshot { part });
                self.stale = false;
            }
            (Some(l), _) if !l.paused => self.stale = true,
            _ => {}
        }
        ui.horizontal(|ui| {
            if self.mcus.len() > 1 {
                let before = self.selected;
                egui::ComboBox::from_id_salt("debug_mcu")
                    .selected_text(&self.mcus[self.selected].designator)
                    .show_ui(ui, |ui| {
                        for (i, m) in self.mcus.iter().enumerate() {
                            ui.selectable_value(&mut self.selected, i, &m.designator);
                        }
                    });
                if self.selected != before {
                    self.stopped = None;
                    self.stale = true;
                    self.scroll_to_pc = true;
                }
            } else {
                ui.label(RichText::new(&self.mcus[self.selected].designator).strong());
            }
            match live {
                None => {
                    if ui
                        .button("⏯ Start paused at reset")
                        .on_hover_text("Run the circuit paused at time zero, to step from the first instruction")
                        .clicked()
                    {
                        action = Action::StartPaused;
                    }
                }
                Some(l) => {
                    let paused = l.paused && part.is_some();
                    if ui
                        .add_enabled(paused, egui::Button::new("▶ Continue"))
                        .clicked()
                    {
                        l.send(Command::Resume);
                    }
                    if ui
                        .add_enabled(!paused, egui::Button::new("⏸ Pause"))
                        .clicked()
                    {
                        l.send(Command::Pause);
                    }
                    if ui
                        .add_enabled(paused, egui::Button::new("⏭ Step"))
                        .on_hover_text("Run one instruction")
                        .clicked()
                        && let Some(part) = part
                    {
                        l.send(Command::Step { part });
                    }
                    if ui
                        .button("↺ Restart paused")
                        .on_hover_text("Restart the circuit paused at time zero")
                        .clicked()
                    {
                        action = Action::StartPaused;
                    }
                }
            }
        });

        let running = live.as_ref().is_some_and(|l| !l.paused);
        let status = match (&live, &self.stopped) {
            (None, _) => "Not running. Click a line's gutter to set a breakpoint, then press Run."
                .to_string(),
            (Some(_), _) if running => "Running. Pause, or wait for a breakpoint.".to_string(),
            (Some(_), Some(s)) => match s.reason {
                Some(HaltReason::Breakpoint) => "Stopped at a breakpoint.".to_string(),
                Some(HaltReason::Stepped) => "Stepped one instruction.".to_string(),
                None => "Paused.".to_string(),
            },
            (Some(_), None) => "Paused.".to_string(),
        };
        ui.label(RichText::new(status).weak());
        if live.is_some() && part.is_none() {
            ui.label(
                RichText::new(
                    "The circuit changed since this simulation started. Restart it to debug this part.",
                )
                .color(CHANGED),
            );
        }
        if let Some(e) = &self.mcus[self.selected].error {
            ui.label(RichText::new(e).color(Color32::from_rgb(225, 70, 70)));
        }
        ui.separator();

        if !running && let Some(s) = &self.stopped {
            self.registers(ui, s);
            ui.separator();
        }
        let count = self.breakpoint_count();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Program").strong());
            if ui
                .add_enabled(
                    count > 0,
                    egui::Button::new(format!("✖ Clear all breakpoints ({count})")),
                )
                .on_hover_text("Remove the breakpoints on every microcontroller")
                .clicked()
            {
                self.clear_breakpoints(live);
            }
        });
        self.listing(ui, live);
        if !running && let Some(s) = &self.stopped {
            ui.separator();
            egui::CollapsingHeader::new("Memory (data space)")
                .default_open(false)
                .show(ui, |ui| Self::memory(ui, s));
        }
        action
    }

    fn registers(&self, ui: &mut Ui, s: &Stopped) {
        let snap = &s.snapshot;
        let clock = self.mcus[self.selected].clock;
        ui.horizontal_wrapped(|ui| {
            ui.monospace(format!("PC 0x{:04X}", snap.pc * 2));
            ui.monospace(format!("SP 0x{:04X}", snap.sp));
            ui.monospace(format!("cycle {}", snap.cycle));
            ui.monospace(format!(
                "t = {}",
                format_value(snap.cycle as f64 / clock, "s")
            ));
            if snap.sleeping {
                ui.label(RichText::new("sleeping").color(CHANGED));
            }
        });
        if let Some(fault) = &snap.fault {
            ui.label(RichText::new(fault).color(Color32::from_rgb(225, 70, 70)));
        }
        ui.horizontal(|ui| {
            ui.monospace(format!("SREG 0x{:02X}", snap.sreg));
            for (bit, name) in dunbrec_mcu::isa::SREG_FLAGS.iter().enumerate().rev() {
                let set = snap.sreg >> bit & 1 == 1;
                let text = RichText::new(*name).monospace();
                ui.label(if set {
                    text.strong().color(CHANGED)
                } else {
                    text.weak()
                });
            }
        });
        let changed = changed_registers(s.previous.as_ref(), snap);
        egui::Grid::new("debug_regs").num_columns(9).show(ui, |ui| {
            for row in 0..4 {
                ui.monospace(format!("r{}–r{}", row * 8, row * 8 + 7));
                for (&value, &was_changed) in snap.regs.iter().zip(&changed).skip(row * 8).take(8) {
                    let text = RichText::new(format!("{value:02X}")).monospace();
                    ui.label(if was_changed {
                        text.strong().color(CHANGED)
                    } else {
                        text
                    });
                }
                ui.end_row();
            }
        });
    }

    fn listing(&mut self, ui: &mut Ui, live: &mut Option<LiveSim>) {
        let pc = self
            .stopped
            .as_ref()
            .filter(|_| live.as_ref().is_some_and(|l| l.paused))
            .map(|s| s.snapshot.pc);
        let mcu = &self.mcus[self.selected];
        if mcu.lines.is_empty() {
            ui.label(RichText::new("No program to show.").weak());
            return;
        }
        let breakpoints = self
            .breakpoints
            .get(&mcu.designator)
            .cloned()
            .unwrap_or_default();
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
        let mut area = ScrollArea::vertical()
            .id_salt("debug_listing")
            .max_height(260.0)
            .auto_shrink([false, false]);
        if self.scroll_to_pc {
            self.scroll_to_pc = false;
            if let Some(row) = pc.and_then(|pc| row_of(&mcu.lines, pc)) {
                area = area.vertical_scroll_offset((row as f32 - 4.0).max(0.0) * row_height);
            }
        }
        let mut toggled = None;
        area.show_rows(ui, row_height, mcu.lines.len(), |ui, range| {
            for line in &mcu.lines[range] {
                let here = Some(line.address) == pc;
                ui.horizontal(|ui| {
                    // The gutter: click to set or clear a breakpoint.
                    let (gutter, response) =
                        ui.allocate_exact_size(vec2(18.0, row_height), Sense::click());
                    let center = gutter.center();
                    if breakpoints.contains(&line.address) {
                        ui.painter().circle_filled(center, 5.0, BREAKPOINT);
                    } else if response.hovered() {
                        ui.painter()
                            .circle_filled(center, 5.0, BREAKPOINT.gamma_multiply(0.35));
                    }
                    if response
                        .on_hover_text("Click to set or clear a breakpoint")
                        .clicked()
                    {
                        toggled = Some(line.address);
                    }
                    let hex: Vec<String> = line.words.iter().map(|w| format!("{w:04X}")).collect();
                    let mut text = format!(
                        "{} 0x{:04X}  {:<9} {}",
                        if here { "▶" } else { " " },
                        line.byte_address(),
                        hex.join(" "),
                        line.text()
                    );
                    // Firmware built from source: say which line this is.
                    if let Some(map) = &mcu.source
                        && let Some((n, source)) = map.at(line.address)
                    {
                        text = format!("{text:<44} ; {}:{n}  {}", map.name, source.trim());
                    }
                    let text = RichText::new(text).monospace();
                    ui.label(if here {
                        text.strong().background_color(CHANGED.gamma_multiply(0.45))
                    } else {
                        text
                    });
                });
            }
        });
        if let Some(word) = toggled {
            self.toggle_breakpoint(live, word);
        }
    }

    fn memory(ui: &mut Ui, s: &Stopped) {
        let data = &s.snapshot.data;
        let before = s.previous.as_ref().map(|p| &p.data);
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
        ScrollArea::vertical()
            .id_salt("debug_memory")
            .max_height(200.0)
            .auto_shrink([false, false])
            .show_rows(ui, row_height, data.len().div_ceil(16), |ui, range| {
                for row in range {
                    let (hex, ascii) = memory_row(data, row);
                    let changed = before.is_some_and(|b| {
                        b.get(row * 16..(row * 16 + 16).min(b.len()))
                            != data.get(row * 16..(row * 16 + 16).min(data.len()))
                    });
                    let text =
                        RichText::new(format!("{:04X}  {hex:<47}  {ascii}", row * 16)).monospace();
                    ui.label(if changed { text.color(CHANGED) } else { text });
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::{ComponentKind as K, NetlistBuilder};
    use dunbrec_mcu::asm::{Asm, io};

    fn flash_of(a: &Asm) -> Vec<u16> {
        words(&a.finish().unwrap())
    }

    fn snapshot(pc: usize, regs: [u8; 32]) -> CpuSnapshot {
        CpuSnapshot {
            pc,
            sp: 0x08FF,
            sreg: 0,
            regs,
            cycle: 10,
            sleeping: false,
            fault: None,
            data: vec![0; 0x900].into_boxed_slice(),
        }
    }

    fn netlist(firmware: &[&str]) -> Netlist {
        let pins: Vec<&str> = dunbrec_mcu::ARDUINO_NAMES.to_vec();
        let mut b = NetlistBuilder::new();
        for (i, f) in firmware.iter().enumerate() {
            b = b.element(
                &format!("U{}", i + 1),
                K::Mcu {
                    firmware: f.to_string(),
                    clock: 16e6,
                },
                &pins,
            );
        }
        b.build()
    }

    #[test]
    fn firmware_built_from_source_shows_its_lines() {
        let mut nl = netlist(&["source:main", "builtin:blink", "source:gone"]);
        nl.sources.insert(
            "main".into(),
            dunbrec_core::SourceFile {
                language: SourceLanguage::Asm,
                text: "; start
  sbi DDRB, 5
loop: rjmp loop
"
                .into(),
            },
        );
        let map = source_map("source:main", &nl).expect("builds");
        assert_eq!(map.at(0), Some((2, "  sbi DDRB, 5")));
        assert_eq!(map.at(1), Some((3, "loop: rjmp loop")));
        assert_eq!(map.at(2), None);
        assert!(source_map("builtin:blink", &nl).is_none());
        assert!(source_map("source:gone", &nl).is_none());

        // The debugger lists the program and remembers where it came from.
        let mut dbg = Debugger::default();
        dbg.load(&nl, None);
        assert_eq!(dbg.mcus[0].lines.len(), 2);
        assert!(dbg.mcus[0].source.is_some());
        assert!(dbg.mcus[1].source.is_none());
        assert!(dbg.mcus[2].error.as_deref().unwrap().contains("gone"));
    }

    #[test]
    fn breakpoints_can_be_set_and_shown_by_source_line() {
        let text = "; start\n  sbi DDRB, 5\nloop: rjmp loop\n";
        let mut nl = netlist(&["source:main"]);
        nl.sources.insert(
            "main".into(),
            dunbrec_core::SourceFile {
                language: SourceLanguage::Asm,
                text: text.into(),
            },
        );
        let mut dbg = Debugger::default();
        dbg.load(&nl, None);
        let mut live = None;
        assert_eq!(
            dbg.source_marks("main", text, &live),
            SourceMarks::default()
        );
        // Line 3 is `rjmp`, at word 1; a comment line has no instruction.
        assert!(dbg.toggle_source_line(&mut live, "main", text, 3));
        assert!(!dbg.toggle_source_line(&mut live, "main", text, 1));
        assert_eq!(dbg.breakpoints["U1"], BTreeSet::from([1]));
        assert_eq!(
            dbg.source_marks("main", text, &live).breakpoints,
            BTreeSet::from([3])
        );
        // The listing's own gutter and the source gutter share breakpoints.
        dbg.toggle_breakpoint(&mut live, 0);
        assert_eq!(
            dbg.source_marks("main", text, &live).breakpoints,
            BTreeSet::from([2, 3])
        );
        assert!(dbg.toggle_source_line(&mut live, "main", text, 3));
        // After an edit the old listing no longer matches the text.
        let edited = format!("{text}nop\n");
        assert!(!dbg.toggle_source_line(&mut live, "main", &edited, 2));
        assert_eq!(
            dbg.source_marks("main", &edited, &live),
            SourceMarks::default()
        );
    }

    #[test]
    fn the_listing_ends_at_the_last_programmed_word() {
        let mut a = Asm::new();
        a.sbi(io::DDRB, 5).label("loop").rjmp("loop");
        let mut flash = flash_of(&a);
        flash.resize(1000, 0xFFFF);
        let lines = program_lines(&flash);
        assert_eq!(
            lines.iter().map(Line::text).collect::<Vec<_>>(),
            ["sbi DDRB, 5", "rjmp 0x0002"]
        );
        assert!(
            program_lines(&[0xFFFF; 8]).is_empty(),
            "erased flash has no program"
        );
    }

    #[test]
    fn rows_are_found_by_word_address_across_two_word_instructions() {
        let mut a = Asm::new();
        a.nop().label("x").jmp("x").nop();
        let lines = program_lines(&flash_of(&a));
        // NOP at word 0, JMP at words 1-2, NOP at word 3.
        assert_eq!(row_of(&lines, 0), Some(0));
        assert_eq!(row_of(&lines, 1), Some(1));
        assert_eq!(row_of(&lines, 3), Some(2));
        assert_eq!(row_of(&lines, 2), None, "the middle of an instruction");
        assert_eq!(row_of(&lines, 99), None);
    }

    #[test]
    fn changed_registers_compare_with_the_previous_stop() {
        let mut regs = [0u8; 32];
        let before = snapshot(0, regs);
        regs[16] = 7;
        regs[31] = 1;
        let now = snapshot(1, regs);
        let changed = changed_registers(Some(&before), &now);
        assert_eq!(
            changed
                .iter()
                .enumerate()
                .filter(|(_, c)| **c)
                .map(|(i, _)| i)
                .collect::<Vec<_>>(),
            [16, 31]
        );
        assert_eq!(changed_registers(None, &now), [false; 32]);
    }

    #[test]
    fn memory_rows_show_hex_and_ascii() {
        let mut data = vec![0u8; 0x900];
        data[0x10..0x15].copy_from_slice(b"Hi\x00!A");
        let (hex, ascii) = memory_row(&data, 1);
        assert!(hex.starts_with("48 69 00 21 41 00"), "{hex}");
        assert!(ascii.starts_with("Hi.!A"), "{ascii}");
        assert_eq!(memory_row(&data, 1000), (String::new(), String::new()));
    }

    #[test]
    fn breakpoints_are_kept_by_designator_and_handed_to_new_simulations() {
        let n = netlist(&["builtin:blink", "builtin:fade"]);
        let mut d = Debugger::default();
        d.load(&n, None);
        assert_eq!(d.mcus.len(), 2);
        // Set a breakpoint on the second microcontroller, with no
        // simulation running.
        d.selected = 1;
        d.toggle_breakpoint(&mut None, 5);
        d.toggle_breakpoint(&mut None, 9);
        d.toggle_breakpoint(&mut None, 5); // cleared again
        let setup = d.setup(&n, true);
        assert!(setup.start_paused);
        assert_eq!(setup.breakpoints, vec![(1, vec![9])]);
        // The first has none.
        assert!(
            d.setup(&n, false)
                .breakpoints
                .iter()
                .all(|(part, _)| *part == 1)
        );
    }

    #[test]
    fn clearing_removes_every_breakpoint_on_every_microcontroller() {
        let n = netlist(&["builtin:blink", "builtin:fade"]);
        let mut d = Debugger::default();
        d.load(&n, None);
        d.toggle_breakpoint(&mut None, 3);
        d.selected = 1;
        d.toggle_breakpoint(&mut None, 8);
        d.toggle_breakpoint(&mut None, 9);
        assert_eq!(d.breakpoint_count(), 3);
        d.clear_breakpoints(&mut None);
        assert_eq!(d.breakpoint_count(), 0);
        assert!(d.setup(&n, false).breakpoints.is_empty());
        // Clearing with nothing set is harmless.
        d.clear_breakpoints(&mut None);
        assert_eq!(d.breakpoint_count(), 0);
    }

    #[test]
    fn a_breakpoint_halt_opens_the_window_and_keeps_the_previous_state() {
        let n = netlist(&["builtin:blink"]);
        let mut d = Debugger::default();
        d.load(&n, None);
        assert!(!d.show);
        let stepped = McuHalt {
            part: 0,
            reason: HaltReason::Stepped,
        };
        let sim = [McuInfo {
            part: 0,
            element: 0,
            designator: "U1".into(),
        }];
        d.on_halt(stepped, snapshot(1, [0; 32]), &sim);
        assert!(!d.show, "a step does not open the window");
        let hit = McuHalt {
            part: 0,
            reason: HaltReason::Breakpoint,
        };
        d.on_halt(hit, snapshot(5, [0; 32]), &sim);
        assert!(d.show);
        let s = d.stopped.as_ref().unwrap();
        assert_eq!(s.snapshot.pc, 5);
        assert_eq!(s.previous.as_ref().unwrap().pc, 1);
        assert_eq!(s.reason, Some(HaltReason::Breakpoint));
        d.sim_stopped();
        assert!(d.stopped.is_none());
    }

    #[test]
    fn firmware_that_cannot_be_read_is_reported_not_fatal() {
        let n = netlist(&["no-such-file.hex"]);
        let mut d = Debugger::default();
        d.load(&n, None);
        assert!(d.mcus[0].error.is_some());
        assert!(d.mcus[0].lines.is_empty());
    }

    #[test]
    fn the_window_draws_in_every_state_without_panicking() {
        let n = netlist(&["builtin:blink"]);
        let mut d = Debugger::default();
        d.load(&n, None);
        d.show = true;
        let ctx = egui::Context::default();
        let frame = |d: &mut Debugger| {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                d.window(ctx, &mut None);
            });
        };
        // Not running, with a breakpoint set and one halt on record.
        d.toggle_breakpoint(&mut None, 0);
        frame(&mut d);
        d.on_halt(
            McuHalt {
                part: 0,
                reason: HaltReason::Breakpoint,
            },
            snapshot(0, [1; 32]),
            &[McuInfo {
                part: 0,
                element: 0,
                designator: "U1".into(),
            }],
        );
        frame(&mut d);
        // No microcontroller at all.
        d.load(&netlist(&[]), None);
        frame(&mut d);
    }
}
