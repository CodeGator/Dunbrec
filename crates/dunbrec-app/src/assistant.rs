//! The Assistant panel: chat with the language model chosen in Settings
//! about the circuit and firmware that are open.
//!
//! Every message goes out with a fresh description of the document (the
//! circuit as a SPICE netlist, what the Checks panel says, and the firmware
//! source), so the answer is about what is on screen now.
//!
//! The assistant can suggest changes (see [`crate::edits`]). They appear as
//! cards under its answer; nothing changes until the user presses Apply.

use std::collections::HashMap;
use std::sync::Arc;

use dunbrec_core::units::format_value;
use dunbrec_core::{ComponentKind, Netlist, Schematic};
use egui::{Align, Color32, FontId, Layout, RichText, ScrollArea, TextEdit, Ui};

use crate::ai::{self, AiSettings, Chat, ChatMessage, Event, Request, Role};
use crate::edits::{self, DiffKind, Edit};
use crate::part_edits::{self, NewPart};

/// Lines of text the question box shows before it scrolls.
const INPUT_ROWS: usize = 6;

/// Most characters of circuit and firmware text sent along with a question.
const CONTEXT_LIMIT: usize = 24_000;

const INSTRUCTIONS: &str = "\
You are the assistant built into Dunbrec, a circuit simulator and schematic editor \
with an ATmega328P (Arduino Uno) emulator. Help the user troubleshoot circuits, \
design circuits, and write or debug firmware (AVR assembly or Arduino C++). \
Be concrete and concise: name parts by their designators, give component values, \
and explain why. When live readings are included, they are a snapshot taken when \
the question was sent; compare them with what the circuit should do.

You cannot change the document yourself, but you can suggest changes that the user \
applies with one click. Only use these blocks for a change you actually recommend:

1. To change part values, write a fenced block tagged dunbrec-set with one change per \
line, using the part's designator exactly as in the circuit:
```dunbrec-set
R2 = 1k
V1 = 5
RV1 = 10k
RV1.wiper = 0.5
SW1 = closed
```
Values are plain engineering numbers (4.7k, 100n, 2.2M). This works for resistors, \
capacitors, inductors, DC voltage and current sources, potentiometers and switches. \
For anything else, describe the change in words.

2. To change firmware, write a fenced block tagged dunbrec-file followed by the exact \
file name, containing the complete new text of that file (never a fragment):
```dunbrec-file main.ino
...whole file...
```
The file must already exist in the document (see the Firmware sections below).

3. To add parts, write a fenced block tagged dunbrec-add with one part per line: \
DESIGNATOR kind value, then the net of each pin in pin order. Net 0 is ground. To join \
an existing net, use its name from the circuit above (a label such as VOUT, or an \
automatic name such as N001); any other name is a new net that only the new parts share.
```dunbrec-add
R5 resistor 10k VOUT 0
C3 capacitor 100n VOUT 0
Q2 npn 2N3904 NODEC NODEB 0
V2 vsource 9 VCC 0
```
Except for parts with many pins (below), every part needs one net per pin, with none left out: a source has two (V2 above is + then -, \
so ground is written as 0 for its second pin). For example, a complete common-emitter stage:
```dunbrec-add
V1 vsource 9 VCC 0
R1 resistor 220k VCC BASE
R2 resistor 2.2k VCC COL
R3 resistor 470 EMIT 0
C1 capacitor 10u IN BASE
C2 capacitor 100u EMIT 0
Q1 npn 2N3904 COL BASE EMIT
```
Kinds, what the value means, and the order of the pins (list the nets in that order):
{PARTS}
Choose designators that are not already in use. A two-pin part between nets that already \
exist is drawn beside them with real wires; any other part goes below the drawing, joined \
by net labels.

To join a pin of a part already in the circuit, whether or not its net has a name, write \
DESIGNATOR.PIN as the net, such as U1.D13. A part with many pins names only the pins it \
uses, as PIN=NET, and leaves the rest unconnected:
```dunbrec-add
R1 resistor 220 U1.D13 LEDA
LED1 led red LEDA 0
DS1 7seg-cathode red a=SEGA b=SEGB COM=0
R2 resistor 220 U1.D2 SEGA
R3 resistor 220 U1.D3 SEGB
```
Every LED, and every segment of a display, needs its own resistor in series (about 220 Ω \
from 5 V, 150 Ω for blue or white). The ATmega328P is powered at 5 V without any wiring. \
A new one has no program unless its value names a file the circuit already has: to give it \
one, ask the user to choose or create a program in the chip's properties first. Controlled \
sources take their two output pins and then their two control pins.

4. To remove parts, write a fenced block tagged dunbrec-remove with one designator per \
line. Wires the user drew to a part stay where they are.

A new part, including an input signal, is always added with dunbrec-add (an AC source is \
vsine with the value amplitude@frequency, such as 0.01@1k); dunbrec-set only changes the \
value of a part already in the circuit, one NAME = value per line. \
Use the kind words exactly as listed (a voltage source is vsource, not voltage), and \
give every part a line; one unknown kind rejects the whole block. To build a circuit, put every part in dunbrec-add blocks (and any value changes in \
dunbrec-set); a netlist or description in an ordinary code fence adds nothing. Never say \
you have created, added or changed anything: nothing happens until the user presses \
Apply on the card, so say you are suggesting it and ask them to press Apply.

Use ordinary code fences, with no dunbrec tag, for examples that are not meant to be \
applied. The current document follows.";

/// What the circuit is doing right now, as the Values panel shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Readings {
    /// What the numbers are: a running simulation, a DC point, or nothing.
    pub heading: String,
    pub voltages: Vec<(String, f64)>,
    pub currents: Vec<(String, f64)>,
    /// The latest serial output of each microcontroller.
    pub serial: Vec<(String, String)>,
    /// What the open instruments show: (heading, lines).
    pub sections: Vec<(String, Vec<String>)>,
}

/// Most values listed, so a big circuit cannot flood the question.
const READINGS_LIMIT: usize = 150;

impl Readings {
    fn to_text(&self) -> String {
        let mut out = format!("{}\n", self.heading);
        let list = |out: &mut String, label: &str, unit: &str, rows: &[(String, f64)]| {
            for (name, v) in rows.iter().take(READINGS_LIMIT) {
                out.push_str(&format!("{label}({name}) = {}\n", format_value(*v, unit)));
            }
            if rows.len() > READINGS_LIMIT {
                out.push_str(&format!("… {} more\n", rows.len() - READINGS_LIMIT));
            }
        };
        list(&mut out, "V", "V", &self.voltages);
        list(&mut out, "I", "A", &self.currents);
        for (mcu, text) in &self.serial {
            out.push_str(&format!(
                "\nSerial output of {mcu} (most recent):\n{text}\n"
            ));
        }
        for (heading, lines) in &self.sections {
            out.push_str(&format!("\n{heading}:\n"));
            for line in lines {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }
}

/// Describe the open document for the model.
pub fn build_system_prompt(
    title: &str,
    schematic: &Schematic,
    netlist: &Netlist,
    readings: &Readings,
) -> String {
    let mut out = format!(
        "{}\n\n# Document: {title}\n\n",
        INSTRUCTIONS.replace("{PARTS}", &part_edits::help())
    );

    out.push_str("## Circuit\n\n");
    // SPICE has other names for LEDs and displays, and none for logic parts
    // or chips: those circuits are listed part by part.
    let has_leds = netlist
        .elements
        .iter()
        .any(|e| matches!(e.kind, ComponentKind::Led { .. }) || e.kind.led_segments().is_some());
    if schematic.components.is_empty() {
        out.push_str("(empty schematic)\n");
    } else {
        match dunbrec_io::spice_export::export(schematic, title, &[]) {
            Ok(spice) if !has_leds => out.push_str(&limited(&spice, CONTEXT_LIMIT / 2)),
            _ => out.push_str(&limited(&parts_list(netlist), CONTEXT_LIMIT / 2)),
        }
    }

    out.push_str("\n## Checks\n\n");
    if netlist.diagnostics.is_empty() {
        out.push_str("No problems found.\n");
    } else {
        for d in &netlist.diagnostics {
            out.push_str(&format!(
                "- {:?}: {}\n",
                d.severity(),
                crate::app::describe_diagnostic(schematic, d)
            ));
        }
    }

    out.push_str("\n## Live readings\n\n");
    out.push_str(&readings.to_text());

    for (name, file) in &schematic.sources {
        out.push_str(&format!("\n## Firmware: {name} ({:?})\n\n", file.language));
        out.push_str(&limited(&file.text, CONTEXT_LIMIT / 2));
    }
    out
}

/// Each part with what it is, and the net on each of its pins that is joined
/// to anything.
fn parts_list(netlist: &Netlist) -> String {
    let mut pins_on = vec![0; netlist.nodes.len()];
    for node in netlist.elements.iter().flat_map(|e| &e.nodes) {
        pins_on[node.0] += 1;
    }
    let mut out = String::from(
        "Parts, each as designator, kind and value, then PIN=NET for every connected pin \
         (net 0 is ground; pins not listed are unconnected):\n",
    );
    for e in &netlist.elements {
        let kind = crate::properties::kind_name(&e.kind);
        let value = crate::canvas::value_text(&e.kind);
        out.push_str(&format!("{}, {kind}", e.designator));
        if !value.is_empty() {
            out.push_str(&format!(", {value}"));
        }
        out.push(':');
        for (pin, node) in e.kind.pins().iter().zip(&e.nodes) {
            let net = &netlist.nodes[node.0];
            if node.is_ground() || net.labelled || pins_on[node.0] > 1 {
                out.push_str(&format!(" {}={}", pin.name, net.name));
            }
        }
        out.push('\n');
    }
    out
}

/// `text`, cut to at most `max` bytes (on a character boundary) with a note.
fn limited(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let cut = (0..=max)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    format!(
        "{}\n… (cut: {} more characters)\n",
        &text[..cut],
        text.len() - cut
    )
}

/// What the panel needs to know about the document and settings.
pub struct View<'a> {
    pub settings: &'a AiSettings,
    pub title: &'a str,
    pub schematic: &'a Schematic,
    pub netlist: &'a Netlist,
    /// Gathers the current readings; called when a question is sent.
    pub readings: &'a dyn Fn() -> Readings,
}

#[derive(Default)]
pub struct Assistant {
    messages: Vec<ChatMessage>,
    /// What became of each suggested change, by (message, block).
    outcomes: HashMap<Key, Outcome>,
    input: String,
    chat: Option<Chat>,
    error: Option<String>,
    /// Scroll to the newest text on the next frame.
    stick_to_bottom: bool,
}

/// Which suggested change: the message it is in and its block in that message.
pub type Key = (usize, usize);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Applied(String),
    Dismissed,
    Failed(String),
}

/// What the user asked of the app from inside the panel.
#[derive(Debug, PartialEq)]
pub enum Command {
    OpenSettings,
    /// Make a suggested change; report the result with [`Assistant::record`].
    Apply {
        key: Key,
        edit: Edit,
    },
}

impl Assistant {
    /// Note how applying a suggested change went.
    pub fn record(&mut self, key: Key, result: Result<String, String>) {
        let outcome = match result {
            Ok(text) => Outcome::Applied(text),
            Err(e) => Outcome::Failed(e),
        };
        self.outcomes.insert(key, outcome);
    }

    /// Drain what the worker has sent so far.
    fn poll(&mut self) {
        let Some(chat) = &self.chat else { return };
        let mut finished = false;
        while let Ok(event) = chat.events.try_recv() {
            match event {
                Event::Text(t) => {
                    if let Some(last) = self.messages.last_mut() {
                        last.text.push_str(&t);
                    }
                    self.stick_to_bottom = true;
                }
                Event::Done => finished = true,
                Event::Failed(e) => {
                    self.error = Some(e);
                    finished = true;
                }
            }
        }
        if finished {
            self.chat = None;
            // An answer that never started is not worth keeping.
            if self
                .messages
                .last()
                .is_some_and(|m| m.role == Role::Assistant && m.text.is_empty())
            {
                self.messages.pop();
            }
        }
    }

    fn send(&mut self, view: &View, ctx: &egui::Context) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.chat.is_some() {
            return;
        }
        self.input.clear();
        self.error = None;
        self.messages.push(ChatMessage {
            role: Role::User,
            text,
        });
        let request = Request {
            settings: view.settings.clone(),
            system: build_system_prompt(
                view.title,
                view.schematic,
                view.netlist,
                &(view.readings)(),
            ),
            messages: self.messages.clone(),
        };
        self.messages.push(ChatMessage {
            role: Role::Assistant,
            text: String::new(),
        });
        let ctx = ctx.clone();
        self.chat = Some(ai::start(request, move || ctx.request_repaint()));
        self.stick_to_bottom = true;
    }

    pub fn show(&mut self, ui: &mut Ui, view: &View) -> Option<Command> {
        self.poll();
        let mut command = None;
        let busy = self.chat.is_some();

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} · {}",
                    view.settings.provider.name(),
                    match view.settings.model() {
                        "" => "no model chosen",
                        m => m,
                    }
                ))
                .weak(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("⚙ Settings").clicked() {
                    command = Some(Command::OpenSettings);
                }
                if ui
                    .add_enabled(
                        !busy && !self.messages.is_empty(),
                        egui::Button::new("Clear").small(),
                    )
                    .clicked()
                {
                    self.messages.clear();
                    self.outcomes.clear();
                    self.error = None;
                }
            });
        });
        ui.separator();

        let problem = view.settings.problem();
        // The input box and its buttons sit at the bottom.
        egui::TopBottomPanel::bottom("assistant_input")
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                if let Some(error) = &self.error {
                    ui.colored_label(Color32::from_rgb(210, 65, 65), error);
                }
                if let Some(problem) = problem {
                    ui.colored_label(Color32::from_rgb(220, 150, 30), problem);
                }
                // Enter sends; Shift+Enter starts a new line. The key is taken
                // before the box sees it, so no stray line break is typed.
                let input_id = ui.make_persistent_id("assistant_input_box");
                let enter = ui.memory(|m| m.has_focus(input_id))
                    && ui.input_mut(|i| {
                        i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                            || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)
                    });
                ui.add(
                    TextEdit::multiline(&mut self.input)
                        .id(input_id)
                        .desired_rows(INPUT_ROWS)
                        .desired_width(f32::INFINITY)
                        .hint_text("Ask about this circuit or firmware…  (Enter to send, Shift+Enter for a new line)"
                        ),
                );
                ui.horizontal(|ui| {
                    if busy {
                        if ui.button("■ Stop").clicked()
                            && let Some(chat) = &self.chat
                        {
                            chat.cancel();
                        }
                        ui.spinner();
                    } else {
                        let can = problem.is_none() && !self.input.trim().is_empty();
                        if ui.add_enabled(can, egui::Button::new("Send")).clicked()
                            || (enter && can)
                        {
                            self.send(view, ui.ctx());
                        }
                    }
                });
            });

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(self.stick_to_bottom)
            .show(ui, |ui| {
                if self.messages.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Ask for help finding a fault, choosing parts, or writing \
                             firmware. The open circuit, the Checks and the firmware are \
                             sent with each question.",
                        )
                        .weak(),
                    );
                }
                let last = self.messages.len().saturating_sub(1);
                for (i, message) in self.messages.iter().enumerate() {
                    let writing = busy && i == last;
                    if let Some(apply) =
                        show_message(ui, i, message, writing, view.schematic, &mut self.outcomes)
                    {
                        command = Some(apply);
                    }
                    ui.add_space(6.0);
                }
            });
        self.stick_to_bottom = false;
        command
    }
}

fn show_message(
    ui: &mut Ui,
    index: usize,
    message: &ChatMessage,
    writing: bool,
    schematic: &Schematic,
    outcomes: &mut HashMap<Key, Outcome>,
) -> Option<Command> {
    let (who, color) = match message.role {
        Role::User => ("You", Color32::from_rgb(70, 140, 220)),
        Role::Assistant => ("Assistant", Color32::from_rgb(90, 170, 110)),
    };
    ui.label(RichText::new(who).strong().color(color));
    if message.text.is_empty() {
        ui.label(RichText::new("…").weak());
        return None;
    }
    let mut command = None;
    for (n, block) in split_blocks(&message.text).into_iter().enumerate() {
        match block {
            Block::Text(t) => {
                ui.add(egui::Label::new(t).selectable(true).wrap());
            }
            Block::Code { lang, text } => match proposal(lang, text) {
                Some(edit) if message.role == Role::Assistant => {
                    let key = (index, n);
                    if let Some(edit) = card(ui, key, edit, schematic, outcomes, writing) {
                        command = Some(Command::Apply { key, edit });
                    }
                }
                _ => code_box(ui, text),
            },
        }
    }
    command
}

fn code_box(ui: &mut Ui, text: &str) {
    egui::Frame::group(ui.style())
        .fill(ui.visuals().extreme_bg_color)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ScrollArea::horizontal().show(ui, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(text).font(FontId::monospace(12.5)))
                        .selectable(true)
                        .extend(),
                );
            });
        });
}

/// The suggested change a fenced block stands for, if it is one. A block
/// that is meant as a change but cannot be read is an error to show.
fn proposal(lang: &str, text: &str) -> Option<Result<Edit, String>> {
    if lang == edits::SET_TAG {
        return Some(edits::parse_set(text).map(Edit::Set));
    }
    if lang == part_edits::ADD_TAG {
        return Some(part_edits::parse_add(text).map(Edit::Add));
    }
    if lang == part_edits::REMOVE_TAG {
        return Some(part_edits::parse_remove(text).map(Edit::Remove));
    }
    let name = lang.strip_prefix(edits::FILE_TAG)?;
    // "dunbrec-filefoo" is not ours; the tag is followed by a space.
    if !name.is_empty() && !name.starts_with(char::is_whitespace) {
        return None;
    }
    let name = name.trim();
    Some(if name.is_empty() {
        Err("The firmware block does not say which file it is for.".into())
    } else {
        Ok(Edit::File {
            name: name.to_string(),
            text: edits::file_text(text),
        })
    })
}

const GOOD: Color32 = Color32::from_rgb(90, 170, 110);
const BAD: Color32 = Color32::from_rgb(210, 65, 65);

/// A suggested change with its preview and Apply / Dismiss buttons. Returns
/// the change when Apply is pressed.
fn card(
    ui: &mut Ui,
    key: Key,
    proposal: Result<Edit, String>,
    schematic: &Schematic,
    outcomes: &mut HashMap<Key, Outcome>,
    writing: bool,
) -> Option<Edit> {
    let mut apply = None;
    egui::Frame::group(ui.style())
        .fill(ui.visuals().faint_bg_color)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let title = match &proposal {
                Ok(Edit::Set(_)) => "Suggested change: values".to_string(),
                Ok(Edit::Add(_)) => "Suggested change: add parts".to_string(),
                Ok(Edit::Remove(_)) => "Suggested change: remove parts".to_string(),
                Ok(Edit::File { name, .. }) => format!("Suggested change: {name}"),
                Err(_) => "Suggested change".to_string(),
            };
            ui.label(RichText::new(title).strong());

            // Once decided, a card is just a note; its preview would now be
            // measured against the changed document.
            match outcomes.get(&key) {
                Some(Outcome::Applied(text)) => {
                    ui.colored_label(GOOD, format!("✔ {text}  (Ctrl+Z undoes it)"));
                    return;
                }
                Some(Outcome::Dismissed) => {
                    ui.label(RichText::new("Dismissed").weak());
                    return;
                }
                _ => {}
            }

            let edit = match &proposal {
                Ok(edit) => edit,
                Err(e) => {
                    ui.colored_label(BAD, e);
                    return;
                }
            };
            let can_apply = preview(ui, key, edit, schematic);
            if let Some(Outcome::Failed(e)) = outcomes.get(&key) {
                ui.colored_label(BAD, e);
            }
            ui.horizontal(|ui| {
                let ready = can_apply && !writing;
                if ui
                    .add_enabled(ready, egui::Button::new("Apply"))
                    .on_disabled_hover_text(if writing {
                        "Wait for the answer to finish."
                    } else {
                        "This change cannot be made as written."
                    })
                    .clicked()
                {
                    apply = Some(edit.clone());
                }
                if ui.button("Dismiss").clicked() {
                    outcomes.insert(key, Outcome::Dismissed);
                }
                if writing {
                    ui.label(RichText::new("still writing…").weak());
                }
            });
        });
    apply
}

/// What [`part_edits::check_add`] says of `parts`. Trying them means drawing
/// them, too much work for every frame, so the answer is kept until the
/// circuit or the parts differ.
fn tried_add(
    ui: &Ui,
    key: Key,
    schematic: &Schematic,
    parts: &[NewPart],
) -> Result<Vec<String>, String> {
    type Tried = (Schematic, Vec<NewPart>, Result<Vec<String>, String>);
    let id = egui::Id::new(("assistant tried", key));
    if let Some(tried) = ui.data(|d| d.get_temp::<Arc<Tried>>(id))
        && tried.0 == *schematic
        && tried.1 == parts
    {
        return tried.2.clone();
    }
    let result = part_edits::check_add(schematic, parts);
    let tried: Arc<Tried> = Arc::new((schematic.clone(), parts.to_vec(), result.clone()));
    ui.data_mut(|d| d.insert_temp(id, tried));
    result
}

/// Show what `edit` would do; true if it can be done and does something.
fn preview(ui: &mut Ui, key: Key, edit: &Edit, schematic: &Schematic) -> bool {
    match edit {
        Edit::Set(lines) => {
            let mut all_fine = true;
            for line in lines {
                match edits::plan_set(schematic, line) {
                    Ok(p) => {
                        ui.colored_label(GOOD, format!("✓ {}", p.summary));
                    }
                    Err(e) => {
                        all_fine = false;
                        ui.colored_label(BAD, format!("✗ {e}"));
                    }
                }
            }
            all_fine
        }
        Edit::Add(parts) => {
            for p in parts {
                ui.colored_label(
                    GOOD,
                    format!(
                        "+ {}: {}  ({})",
                        p.designator,
                        p.summary,
                        part_edits::connections(p)
                    ),
                );
            }
            match tried_add(ui, key, schematic, parts) {
                Ok(notes) => {
                    for n in &notes {
                        ui.label(RichText::new(n).weak());
                    }
                    true
                }
                Err(e) => {
                    ui.colored_label(BAD, format!("✗ {e}"));
                    false
                }
            }
        }
        Edit::Remove(names) => {
            for n in names {
                match part_edits::describe(schematic, n) {
                    Some(d) => {
                        ui.colored_label(BAD, format!("− {d}"));
                    }
                    None => {
                        ui.colored_label(BAD, format!("✗ There is no part called {n}."));
                    }
                }
            }
            let ok = part_edits::check_remove(schematic, names).is_ok();
            if ok {
                ui.label(RichText::new("Wires you drew to these parts are left in place.").weak());
            }
            ok
        }
        Edit::File { name, text } => {
            if let Err(e) = edits::check_file(schematic, name) {
                ui.colored_label(BAD, e);
                return false;
            }
            let old = &schematic.sources[name].text;
            if old == text {
                ui.label(RichText::new("The file already has this text.").weak());
                return false;
            }
            match edits::diff(old, text) {
                Some(lines) => diff_view(ui, &lines),
                None => {
                    ui.label("The file is large; Apply replaces all of it.");
                }
            }
            true
        }
    }
}

fn diff_view(ui: &mut Ui, lines: &[(DiffKind, String)]) {
    let font = FontId::monospace(12.0);
    ScrollArea::both()
        .id_salt("diff")
        .max_height(220.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for entry in edits::hunks(lines, 2) {
                let (text, color) = match entry {
                    None => ("  …".to_string(), ui.visuals().weak_text_color()),
                    Some((DiffKind::Same, t)) => (format!("  {t}"), ui.visuals().text_color()),
                    Some((DiffKind::Added, t)) => (format!("+ {t}"), GOOD),
                    Some((DiffKind::Removed, t)) => (format!("- {t}"), BAD),
                };
                ui.add(
                    egui::Label::new(RichText::new(text).font(font.clone()).color(color)).extend(),
                );
            }
        });
}

#[derive(Debug, PartialEq, Eq)]
enum Block<'a> {
    Text(&'a str),
    /// Fenced code; `lang` is what follows the opening fence on its line.
    Code {
        lang: &'a str,
        text: &'a str,
    },
}

/// Cut `text` into prose and fenced (```) code. An unfinished fence, as in
/// an answer still arriving, counts as code to the end.
fn split_blocks(text: &str) -> Vec<Block<'_>> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let before = rest[..start].trim_matches('\n');
        if !before.is_empty() {
            blocks.push(Block::Text(before));
        }
        let after = &rest[start + 3..];
        let line_end = after.find('\n').map_or(after.len(), |i| i + 1);
        let lang = after[..line_end].trim();
        let body = &after[line_end..];
        match body.find("```") {
            Some(end) => {
                blocks.push(Block::Code {
                    lang,
                    text: body[..end].trim_end_matches('\n'),
                });
                rest = &body[end + 3..];
            }
            None => {
                blocks.push(Block::Code {
                    lang,
                    text: body.trim_end_matches('\n'),
                });
                rest = "";
            }
        }
    }
    let tail = rest.trim_matches('\n');
    if !tail.is_empty() {
        blocks.push(Block::Text(tail));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_examples_given_to_the_model_can_be_read() {
        let opening = format!(
            "```{}
",
            part_edits::ADD_TAG
        );
        let blocks: Vec<&str> = INSTRUCTIONS
            .split(&opening)
            .skip(1)
            .map(|rest| rest.split("```").next().unwrap())
            .collect();
        assert_eq!(blocks.len(), 3);
        for block in blocks {
            part_edits::parse_add(block).unwrap_or_else(|e| {
                panic!(
                    "{e}
{block}"
                )
            });
        }
    }

    #[test]
    fn circuits_with_leds_or_chips_are_listed_with_their_connections() {
        let mut s = Schematic::new();
        let parts = part_edits::parse_add(
            "U1 atmega328p none D13=LA\nR1 resistor 220 LA LB\nLED1 led red LB 0",
        )
        .unwrap();
        part_edits::add_parts(&mut s, &parts).unwrap();
        let netlist = dunbrec_core::build_netlist(&s);
        let prompt = build_system_prompt("t", &s, &netlist, &Readings::default());
        let circuit = prompt.split("## Circuit").nth(1).unwrap();
        // The designators are those suggestions must use, not SPICE's.
        for line in [
            "U1, Microcontroller (ATmega328P), no firmware",
            "D13=LA",
            "R1, Resistor, 220 Ω: p=LA n=LB",
            "LED1, LED, red: A=LB K=0",
        ] {
            assert!(circuit.contains(line), "{line}: {circuit}");
        }
        assert!(
            !circuit.contains("DLED1") && !circuit.contains("D12="),
            "{circuit}"
        );
    }

    #[test]
    fn code_fences_are_split_from_prose() {
        let b = split_blocks("Try this:\n```cpp\nint x;\n```\nThen run it.");
        assert_eq!(
            b,
            [
                Block::Text("Try this:"),
                Block::Code {
                    lang: "cpp",
                    text: "int x;"
                },
                Block::Text("Then run it.")
            ]
        );
    }

    #[test]
    fn an_unfinished_fence_is_code() {
        assert_eq!(
            split_blocks("a\n```asm\nldi r16, 1"),
            [
                Block::Text("a"),
                Block::Code {
                    lang: "asm",
                    text: "ldi r16, 1"
                }
            ]
        );
        assert_eq!(split_blocks("plain"), [Block::Text("plain")]);
        assert!(split_blocks("").is_empty());
    }

    #[test]
    fn suggested_changes_are_told_from_plain_code() {
        assert!(proposal("cpp", "int x;").is_none());
        assert!(proposal("", "x").is_none());
        let set = proposal(edits::SET_TAG, "R2 = 1k").unwrap().unwrap();
        assert!(matches!(set, Edit::Set(ref l) if l.len() == 1));
        assert!(proposal(edits::SET_TAG, "nonsense").unwrap().is_err());
        let file = proposal("dunbrec-file main.ino", "void loop() {}\n\n")
            .unwrap()
            .unwrap();
        assert_eq!(
            file,
            Edit::File {
                name: "main.ino".into(),
                text: "void loop() {}\n".into()
            }
        );
        assert!(proposal("dunbrec-file", "x").unwrap().is_err());
        assert!(proposal("dunbrec-filesystem", "x").is_none());
    }

    #[test]
    fn long_context_is_cut_on_a_character_boundary() {
        let text = "é".repeat(100);
        let cut = limited(&text, 51);
        assert!(cut.contains("cut:"));
        assert!(limited("short", 100) == "short");
    }

    #[test]
    fn the_prompt_describes_an_empty_document() {
        let s = Schematic::default();
        let n = dunbrec_core::build_netlist(&s);
        let p = build_system_prompt("Untitled", &s, &n, &Readings::default());
        assert!(p.contains("# Document: Untitled"));
        assert!(p.contains("empty schematic"));
        assert!(p.contains("## Live readings"));
        // The list of parts the assistant may add, with real pin orders, is in it.
        assert!(
            p.contains("- opamp:") && p.contains("in+ in- V+ V- out"),
            "{p}"
        );
        assert!(!p.contains("{PARTS}"));
    }

    #[test]
    fn readings_list_values_with_units_and_serial_text() {
        let r = Readings {
            heading: "Simulation running, t = 1 ms".into(),
            voltages: vec![("VOUT".into(), 0.909)],
            currents: vec![("R1".into(), 0.00909)],
            serial: vec![("U1".into(), "hello".into())],
            sections: vec![("Oscilloscope".into(), vec!["Channel 1: rms 5 V".into()])],
        };
        let t = r.to_text();
        assert!(t.contains("Oscilloscope:\nChannel 1: rms 5 V"), "{t}");
        assert!(t.starts_with("Simulation running"));
        assert!(t.contains("V(VOUT) = 909"), "{t}");
        assert!(t.contains("I(R1) = 9.09"), "{t}");
        assert!(t.contains("Serial output of U1") && t.contains("hello"));
    }

    #[test]
    fn a_long_list_of_readings_is_cut() {
        let r = Readings {
            heading: "x".into(),
            voltages: (0..READINGS_LIMIT + 5)
                .map(|i| (format!("N{i}"), 1.0))
                .collect(),
            ..Readings::default()
        };
        assert!(r.to_text().contains("… 5 more"));
    }
}
