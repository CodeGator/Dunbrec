//! Message boxes and questions, all drawn the same way: one fixed width, a
//! coloured icon and a title, the message wrapped in readable type, technical
//! detail set apart in its own box, and buttons on the right. Enter presses
//! the highlighted button and Escape cancels, as in any other program.

use egui::{
    Align, Align2, Color32, Context, FontId, Frame, Id, Key, Layout, Margin, RichText, ScrollArea,
    Sense, Ui, Vec2,
};

/// Width of every dialog's content (points).
pub const WIDTH: f32 = 420.0;
const BUTTON: Vec2 = Vec2::new(92.0, 28.0);
/// The tallest the detail box grows before it scrolls.
const DETAIL_HEIGHT: f32 = 220.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Info,
    Warning,
    Error,
    Question,
}

impl Kind {
    fn color(self) -> Color32 {
        match self {
            Kind::Info => Color32::from_rgb(70, 140, 220),
            Kind::Warning => Color32::from_rgb(220, 150, 30),
            Kind::Error => Color32::from_rgb(210, 65, 65),
            Kind::Question => Color32::from_rgb(70, 140, 220),
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Kind::Info => "i",
            Kind::Warning | Kind::Error => "!",
            Kind::Question => "?",
        }
    }
}

/// Something to tell the user: a headline, a sentence, and optionally the
/// technical detail (an error from the system, a list of notes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: Kind,
    pub title: String,
    pub text: String,
    pub detail: Option<String>,
}

impl Message {
    fn new(kind: Kind, title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            text: text.into(),
            detail: None,
        }
    }

    pub fn info(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(Kind::Info, title, text)
    }

    pub fn error(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(Kind::Error, title, text)
    }

    /// Add the detail box.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.trim().is_empty()).then_some(detail);
        self
    }

    /// A problem with a file: what was being done, which file, and what the
    /// system said.
    pub fn file_error(title: &str, path: &std::path::Path, reason: impl std::fmt::Display) -> Self {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        Self::error(title, format!("“{name}” could not be used."))
            .with_detail(format!("{}\n\n{reason}", path.display()))
    }
}

/// A modal dialog: build it, then [`show`](Dialog::show) it every frame
/// until a button answers.
pub struct Dialog<'a> {
    pub id: &'a str,
    pub kind: Kind,
    pub title: &'a str,
    /// Button labels, left to right.
    pub buttons: &'a [&'a str],
    /// The highlighted button; Enter presses it.
    pub default: usize,
    /// The button Escape presses, if any.
    pub cancel: Option<usize>,
}

impl Dialog<'_> {
    /// Draw the dialog with `body` between the title and the buttons.
    /// Returns the index of the button pressed this frame.
    pub fn show(&self, ctx: &Context, body: impl FnOnce(&mut Ui)) -> Option<usize> {
        let mut answer = None;
        let frame = Frame::popup(&ctx.style()).inner_margin(Margin::same(20));
        egui::Modal::new(Id::new(self.id))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
                    ui.painter()
                        .circle_filled(rect.center(), 17.0, self.kind.color());
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        self.kind.glyph(),
                        FontId::proportional(22.0),
                        Color32::WHITE,
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new(self.title).size(19.0).strong());
                });
                ui.add_space(12.0);
                body(ui);
                ui.add_space(18.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Right to left, so the last button is added first.
                    for (i, label) in self.buttons.iter().enumerate().rev() {
                        let text = RichText::new(*label).size(14.5);
                        let button = if i == self.default {
                            egui::Button::new(text.strong()).fill(ui.visuals().selection.bg_fill)
                        } else {
                            egui::Button::new(text)
                        };
                        if ui.add(button.min_size(BUTTON)).clicked() {
                            answer = Some(i);
                        }
                    }
                });
            });
        if answer.is_none() {
            if ctx.input(|i| i.key_pressed(Key::Enter)) {
                answer = Some(self.default);
            } else if ctx.input(|i| i.key_pressed(Key::Escape)) {
                answer = self.cancel;
            }
        }
        answer
    }
}

/// What the user chose when asked about unsaved changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsaved {
    Save,
    Discard,
    KeepEditing,
}

/// The one question asked whenever a circuit with unsaved changes is about
/// to be closed or replaced (quitting, opening another, deleting its file).
/// `can_save` adds a Save button where saving first makes sense. The
/// highlighted button, which Enter presses, is the one that loses nothing:
/// Save when there is one, otherwise Keep editing. Escape keeps editing.
pub fn unsaved_changes(ctx: &Context, name: &str, can_save: bool) -> Option<Unsaved> {
    let (buttons, default): (&[&str], usize) = if can_save {
        (&["Save", "Discard changes", "Keep editing"], 0)
    } else {
        (&["Discard changes", "Keep editing"], 1)
    };
    let keep = buttons.len() - 1;
    let answer = Dialog {
        id: "unsaved_changes",
        kind: Kind::Warning,
        title: "Unsaved changes",
        buttons,
        default,
        cancel: Some(keep),
    }
    .show(ctx, |ui| {
        paragraph(
            ui,
            &format!("“{name}” has unsaved changes. Keep editing or discard those changes?"),
        );
    })?;
    Some(match (can_save, answer) {
        (true, 0) => Unsaved::Save,
        (_, i) if i == keep => Unsaved::KeepEditing,
        _ => Unsaved::Discard,
    })
}

/// The body text of a dialog: readable size, wrapped to the dialog's width.
pub fn paragraph(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(14.5));
}

/// Technical detail in a box of its own, scrolling if it is long.
pub fn detail_box(ui: &mut Ui, text: &str) {
    Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(4.0)
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ScrollArea::vertical()
                .max_height(DETAIL_HEIGHT)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.label(RichText::new(text).monospace().size(12.5));
                });
        });
}

/// Show `message` with an OK button. True once it is dismissed.
pub fn show_message(ctx: &Context, message: &Message) -> bool {
    Dialog {
        id: "message",
        kind: message.kind,
        title: &message.title,
        buttons: &["OK"],
        default: 0,
        cancel: Some(0),
    }
    .show(ctx, |ui| {
        paragraph(ui, &message.text);
        if let Some(detail) = &message.detail {
            ui.add_space(10.0);
            detail_box(ui, detail);
        }
    })
    .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key) -> egui::RawInput {
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        input
    }

    fn frame(ctx: &Context, input: egui::RawInput, dialog: &Dialog) -> Option<usize> {
        let mut answer = None;
        let _ = ctx.run(input, |ctx| {
            answer = dialog.show(ctx, |ui| paragraph(ui, "Save changes first?"));
        });
        answer
    }

    #[test]
    fn the_unsaved_changes_question_is_the_same_everywhere() {
        let run = |input: egui::RawInput, can_save: bool| {
            let ctx = Context::default();
            let mut answer = None;
            let _ = ctx.run(input, |ctx| {
                answer = unsaved_changes(ctx, "Circuit 1.dbr", can_save);
            });
            answer
        };
        // With a Save button, Enter saves and Escape keeps editing.
        assert_eq!(run(egui::RawInput::default(), true), None);
        assert_eq!(run(key(Key::Enter), true), Some(Unsaved::Save));
        assert_eq!(run(key(Key::Escape), true), Some(Unsaved::KeepEditing));
        // Without one, neither key can throw the changes away.
        assert_eq!(run(key(Key::Enter), false), Some(Unsaved::KeepEditing));
        assert_eq!(run(key(Key::Escape), false), Some(Unsaved::KeepEditing));
    }

    #[test]
    fn enter_presses_the_default_button_and_escape_the_cancel_button() {
        let dialog = Dialog {
            id: "test",
            kind: Kind::Question,
            title: "Unsaved changes",
            buttons: &["Save", "Discard", "Cancel"],
            default: 0,
            cancel: Some(2),
        };
        let ctx = Context::default();
        assert_eq!(frame(&ctx, egui::RawInput::default(), &dialog), None);
        assert_eq!(frame(&ctx, key(Key::Enter), &dialog), Some(0));
        assert_eq!(frame(&ctx, key(Key::Escape), &dialog), Some(2));
        // With no cancel button, Escape does nothing.
        let no_cancel = Dialog {
            cancel: None,
            ..dialog
        };
        assert_eq!(frame(&ctx, key(Key::Escape), &no_cancel), None);
    }

    #[test]
    fn every_message_draws_whatever_its_size() {
        let ctx = Context::default();
        let long = "word ".repeat(400);
        let messages = [
            Message::info("Done", "All good."),
            Message::error("Could not save", &long).with_detail(&long),
            Message::file_error(
                "Could not open the file",
                std::path::Path::new("C:\\a\\very\\long\\path\\to\\a\\file.dbr"),
                "Access is denied. (os error 5)",
            ),
        ];
        for message in &messages {
            let mut dismissed = None;
            let _ = ctx.run(key(Key::Enter), |ctx| {
                dismissed = Some(show_message(ctx, message));
            });
            assert_eq!(dismissed, Some(true));
        }
    }

    #[test]
    fn file_errors_name_the_file_and_keep_the_reason_in_the_detail() {
        let m = Message::file_error(
            "Could not save the file",
            std::path::Path::new("D:/work/amp.dbr"),
            "disk full",
        );
        assert_eq!(m.kind, Kind::Error);
        assert_eq!(m.text, "“amp.dbr” could not be used.");
        let detail = m.detail.unwrap();
        assert!(detail.contains("D:/work/amp.dbr") && detail.contains("disk full"));
        // An empty detail is no detail.
        assert_eq!(Message::info("a", "b").with_detail("  ").detail, None);
    }
}
