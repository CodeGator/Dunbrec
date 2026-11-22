//! The Assistant panel: chat with the language model chosen in Settings
//! about the circuit and firmware that are open.
//!
//! Every message goes out with a fresh description of the document (the
//! circuit as a SPICE netlist, what the Checks panel says, and the firmware
//! source), so the answer is about what is on screen now. The assistant only
//! talks: it never changes the document.

use dunbrec_core::{Netlist, Schematic};
use egui::{Align, Color32, FontId, Layout, RichText, ScrollArea, TextEdit, Ui};

use crate::ai::{self, AiSettings, Chat, ChatMessage, Event, Request, Role};

/// Lines of text the question box shows before it scrolls.
const INPUT_ROWS: usize = 6;

/// Most characters of circuit and firmware text sent along with a question.
const CONTEXT_LIMIT: usize = 24_000;

const INSTRUCTIONS: &str = "\
You are the assistant built into Dunbrec, a circuit simulator and schematic editor \
with an ATmega328P (Arduino Uno) emulator. Help the user troubleshoot circuits, \
design circuits, and write or debug firmware (AVR assembly or Arduino C++). \
Be concrete and concise: name parts by their designators, give component values, \
and explain why. You can only talk; you cannot change the document, so describe \
changes the user should make. The current document follows.";

/// Describe the open document for the model.
pub fn build_system_prompt(title: &str, schematic: &Schematic, netlist: &Netlist) -> String {
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
}

#[derive(Default)]
pub struct Assistant {
    messages: Vec<ChatMessage>,
    input: String,
    chat: Option<Chat>,
    error: Option<String>,
    /// Scroll to the newest text on the next frame.
    stick_to_bottom: bool,
}

/// What the user asked of the app from inside the panel.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    OpenSettings,
}

impl Assistant {
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
            system: build_system_prompt(view.title, view.schematic, view.netlist),
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
                        .hint_text(
                            "Ask about this circuit or firmware…                               (Enter to send, Shift+Enter for a new line)",
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
                for message in &self.messages {
                    show_message(ui, message);
                    ui.add_space(6.0);
                }
            });
        self.stick_to_bottom = false;
        command
    }
}

fn show_message(ui: &mut Ui, message: &ChatMessage) {
    let (who, color) = match message.role {
        Role::User => ("You", Color32::from_rgb(70, 140, 220)),
        Role::Assistant => ("Assistant", Color32::from_rgb(90, 170, 110)),
    };
    ui.label(RichText::new(who).strong().color(color));
    if message.text.is_empty() {
        ui.label(RichText::new("…").weak());
        return;
    }
    for block in split_blocks(&message.text) {
        match block {
            Block::Text(t) => {
                ui.add(egui::Label::new(t).selectable(true).wrap());
            }
            Block::Code(c) => {
                egui::Frame::group(ui.style())
                    .fill(ui.visuals().extreme_bg_color)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ScrollArea::horizontal().show(ui, |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(c).font(FontId::monospace(12.5)))
                                    .selectable(true)
                                    .extend(),
                            );
                        });
                    });
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Block<'a> {
    Text(&'a str),
    Code(&'a str),
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
        // Skip the language tag on the opening line.
        let body_start = after.find('\n').map_or(after.len(), |i| i + 1);
        let body = &after[body_start..];
        match body.find("```") {
            Some(end) => {
                blocks.push(Block::Code(body[..end].trim_end_matches('\n')));
                rest = &body[end + 3..];
            }
            None => {
                blocks.push(Block::Code(body.trim_end_matches('\n')));
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
                Block::Code("int x;"),
                Block::Text("Then run it.")
            ]
        );
    }

    #[test]
    fn an_unfinished_fence_is_code() {
        assert_eq!(
            split_blocks("a\n```asm\nldi r16, 1"),
            [Block::Text("a"), Block::Code("ldi r16, 1")]
        );
        assert_eq!(split_blocks("plain"), [Block::Text("plain")]);
        assert!(split_blocks("").is_empty());
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
        let p = build_system_prompt("Untitled", &s, &n);
        assert!(p.contains("# Document: Untitled"));
        assert!(p.contains("empty schematic"));
    }
}
