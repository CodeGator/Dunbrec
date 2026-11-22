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

use dunbrec_core::units::format_value;
use dunbrec_core::{Netlist, Schematic};
use egui::{Align, Color32, FontId, Layout, RichText, ScrollArea, TextEdit, Ui};

use crate::ai::{self, AiSettings, Chat, ChatMessage, Event, Request, Role};
use crate::edits::{self, DiffKind, Edit};

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
    let mut out = format!("{INSTRUCTIONS}\n\n# Document: {title}\n\n");

    out.push_str("## Circuit\n\n");
    if schematic.components.is_empty() {
        out.push_str("(empty schematic)\n");
    } else {
        match dunbrec_io::spice_export::export(schematic, title, &[]) {
            Ok(spice) => out.push_str(&limited(&spice, CONTEXT_LIMIT / 2)),
            Err(_) => {
                out.push_str("(Could not be exported as SPICE; parts only:)\n");
                for c in &schematic.components {
                    let kind = format!("{:?}", c.kind);
                    let kind: String = kind.chars().take(80).collect();
                    out.push_str(&format!("{} {kind}\n", c.designator));
                }
            }
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
            let can_apply = preview(ui, edit, schematic);
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

/// Show what `edit` would do; true if it can be done and does something.
fn preview(ui: &mut Ui, edit: &Edit, schematic: &Schematic) -> bool {
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
    }

    #[test]
    fn readings_list_values_with_units_and_serial_text() {
        let r = Readings {
            heading: "Simulation running, t = 1 ms".into(),
            voltages: vec![("VOUT".into(), 0.909)],
            currents: vec![("R1".into(), 0.00909)],
            serial: vec![("U1".into(), "hello".into())],
        };
        let t = r.to_text();
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
