//! The code panel: edit the firmware source files stored in the schematic.
//!
//! Each source file belongs to the schematic (`Schematic::sources`) and is
//! picked by a microcontroller's firmware field as `source:<name>`. The
//! panel highlights assembly as it is typed, builds it on every change so
//! mistakes are marked on their line at once, and lists them below the
//! text; clicking an error jumps to it. Running the circuit builds the same
//! text again, so what the panel shows is what runs.
//!
//! Typing edits the document directly. The first change of a typing session
//! records one undo step (not one per key), so Ctrl+Z outside the text undoes
//! the whole stretch of typing at once.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use dunbrec_core::{SourceFile, SourceLanguage};
use dunbrec_mcu::arduino::{self, BuildError, BuildOutput};
use dunbrec_mcu::asm_text::{self, AsmError, Assembled, FLASH_WORDS};
use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Align, Color32, FontId, Id, RichText, Stroke, Ui};

use crate::debugger_ui::Debugger;
use crate::dialog::{self, Dialog};
use crate::document::Document;
use crate::live::LiveSim;

/// What a new project's first source file contains: a program that blinks
/// the LED on D13, about twice a second at 16 MHz.
pub const STARTER: &str = "\
; Blink the LED on D13 (PB5).
        .equ    LED = PB5

        sbi     DDRB, LED       ; PB5 is an output
loop:   sbi     PINB, LED       ; writing a 1 to PIN toggles the pin
        ldi     r18, 0x1E       ; count down 2,000,000 loops of 4 cycles:
        ldi     r19, 0x84       ; half a second at 16 MHz
        ldi     r20, 0x80
delay:  subi    r20, 1
        sbci    r19, 0
        sbci    r18, 0
        brne    delay
        rjmp    loop
";

/// What a new C++ sketch contains.
pub const STARTER_SKETCH: &str = "\
// Blink the LED on D13.
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
}

void loop() {
  digitalWrite(LED_BUILTIN, HIGH);
  delay(500);
  digitalWrite(LED_BUILTIN, LOW);
  delay(500);
}
";

/// A name for a new source *file* that no source of the schematic has:
/// `main.asm`, then `main2.asm`, and so on.
pub fn unused_file_name(
    sources: &BTreeMap<String, SourceFile>,
    base: &str,
    extension: &str,
) -> String {
    let first = format!("{base}.{extension}");
    if !sources.contains_key(&first) {
        return first;
    }
    (2..)
        .map(|n| format!("{base}{n}.{extension}"))
        .find(|name| !sources.contains_key(name))
        .expect("some number is free")
}

/// The name a new source gets for what was typed: the typed name if it
/// already says what kind of file it is, otherwise with the extension of
/// the chosen language added. Returns the name and its language.
pub fn file_key(typed: &str, chosen: SourceLanguage) -> (String, SourceLanguage) {
    let typed = typed.trim();
    match dunbrec_core::source_language_of(typed) {
        Some(language) => (typed.to_string(), language),
        None => {
            let ext = match chosen {
                SourceLanguage::Asm => "asm",
                SourceLanguage::Cpp => "ino",
            };
            (format!("{typed}.{ext}"), chosen)
        }
    }
}

/// The tab to show after `closed` is closed: the one after it, else the one
/// before, else none.
fn after_closing(names: &[String], closed: &str) -> Option<String> {
    let at = names.iter().position(|n| n == closed)?;
    names
        .get(at + 1)
        .or_else(|| at.checked_sub(1).and_then(|before| names.get(before)))
        .cloned()
}

/// The name Arduino's tool should give the sketch built from the source
/// called `name`: the file's own name without its extension.
fn sketch_name(name: &str) -> String {
    std::path::Path::new(name)
        .file_stem()
        .map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned())
}

/// A new Arduino C++ sketch.
pub fn new_sketch() -> SourceFile {
    SourceFile {
        language: SourceLanguage::Cpp,
        text: STARTER_SKETCH.to_string(),
    }
}

/// A new assembly source file: the starter program for the first file of a
/// schematic, an empty one after that.
pub fn new_file(sources: &BTreeMap<String, SourceFile>, name: &str) -> SourceFile {
    SourceFile {
        language: SourceLanguage::Asm,
        text: if sources.is_empty() {
            STARTER.to_string()
        } else {
            format!("; {name}\n")
        },
    }
}

// ---------------------------------------------------------------------------
// Highlighting

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Comment,
    Directive,
    Label,
    Mnemonic,
    Register,
    Constant,
    Number,
    Text,
}

/// The coloured pieces of one line of assembly. Gaps are plain.
pub fn tokenize(line: &str) -> Vec<(Range<usize>, Kind)> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // True until the statement's mnemonic or directive has been seen.
    let mut at_start = true;
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b';' || (c == b'/' && b.get(i + 1) == Some(&b'/')) {
            out.push((i..b.len(), Kind::Comment));
            break;
        } else if c == b'"' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push((start..i, Kind::Text));
        } else if c == b'\'' {
            let close = if b.get(i + 1) == Some(&b'\\') {
                i + 3
            } else {
                i + 2
            };
            if b.get(close) == Some(&b'\'') {
                out.push((i..close + 1, Kind::Number));
                i = close + 1;
            } else {
                i += 1;
            }
        } else if c == b'.' && b.get(i + 1).is_some_and(|&n| n.is_ascii_alphabetic()) {
            let start = i;
            i += 1;
            while i < b.len() && ident(b[i]) {
                i += 1;
            }
            out.push((start..i, Kind::Directive));
            at_start = false;
        } else if c.is_ascii_digit()
            || (c == b'$' && b.get(i + 1).is_some_and(u8::is_ascii_hexdigit))
        {
            let start = i;
            i += 1;
            while i < b.len() && ident(b[i]) {
                i += 1;
            }
            out.push((start..i, Kind::Number));
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && ident(b[i]) {
                i += 1;
            }
            let word = &line[start..i];
            if at_start && b.get(i) == Some(&b':') {
                out.push((start..i, Kind::Label));
                i += 1;
            } else if at_start {
                if asm_text::is_mnemonic(word) {
                    out.push((start..i, Kind::Mnemonic));
                }
                at_start = false;
            } else if asm_text::is_register(word) {
                out.push((start..i, Kind::Register));
            } else if asm_text::is_chip_name(word) {
                out.push((start..i, Kind::Constant));
            }
        } else {
            // Punctuation (and any multi-byte character).
            i += line[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

struct Palette {
    plain: Color32,
    comment: Color32,
    directive: Color32,
    label: Color32,
    mnemonic: Color32,
    register: Color32,
    constant: Color32,
    number: Color32,
    text: Color32,
}

impl Palette {
    fn new(dark: bool, plain: Color32) -> Self {
        let c = |r, g, b| Color32::from_rgb(r, g, b);
        if dark {
            Self {
                plain,
                comment: c(106, 153, 85),
                directive: c(197, 134, 192),
                label: c(220, 220, 170),
                mnemonic: c(86, 156, 214),
                register: c(156, 220, 254),
                constant: c(78, 201, 176),
                number: c(181, 206, 168),
                text: c(206, 145, 120),
            }
        } else {
            Self {
                plain,
                comment: c(0, 128, 0),
                directive: c(175, 0, 219),
                label: c(121, 94, 38),
                mnemonic: c(0, 0, 255),
                register: c(0, 16, 128),
                constant: c(38, 127, 153),
                number: c(9, 134, 88),
                text: c(163, 21, 21),
            }
        }
    }

    fn color(&self, kind: Kind) -> Color32 {
        match kind {
            Kind::Plain => self.plain,
            Kind::Comment => self.comment,
            Kind::Directive => self.directive,
            Kind::Label => self.label,
            Kind::Mnemonic => self.mnemonic,
            Kind::Register => self.register,
            Kind::Constant => self.constant,
            Kind::Number => self.number,
            Kind::Text => self.text,
        }
    }
}

const WARNING_AMBER: Color32 = Color32::from_rgb(220, 150, 30);
const ERROR_RED: Color32 = Color32::from_rgb(225, 70, 70);

/// Byte offset in `line` of the 1-based character column `col`.
fn byte_of_column(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col.saturating_sub(1))
        .map_or(line.len(), |(i, _)| i)
}

/// The text as a coloured layout, with the places of `errors` underlined.
fn highlight(
    text: &str,
    asm: bool,
    font: &FontId,
    palette: &Palette,
    errors: &[AsmError],
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.text.reserve(text.len());
    for (index, raw) in text.split_inclusive('\n').enumerate() {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        // The span of the word an error points at.
        let underline: Vec<Range<usize>> = errors
            .iter()
            .filter(|e| e.line == index + 1)
            .map(|e| {
                let start = byte_of_column(line, e.column);
                let len = line[start..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(line.len() - start)
                    .max(1);
                start..(start + len).min(line.len().max(start + 1))
            })
            .collect();

        // Tokens, with the gaps between them filled in and the line ending.
        let mut pieces: Vec<(Range<usize>, Kind)> = Vec::new();
        let mut pos = 0;
        let tokens = if asm { tokenize(line) } else { Vec::new() };
        for (range, kind) in tokens {
            if range.start > pos {
                pieces.push((pos..range.start, Kind::Plain));
            }
            pos = range.end;
            pieces.push((range, kind));
        }
        if pos < raw.len() {
            pieces.push((pos..raw.len(), Kind::Plain));
        }

        for (range, kind) in pieces {
            // Split at the underlined spans.
            let mut cuts = vec![range.start, range.end];
            for u in &underline {
                for edge in [u.start, u.end] {
                    if range.start < edge && edge < range.end {
                        cuts.push(edge);
                    }
                }
            }
            cuts.sort_unstable();
            cuts.dedup();
            for pair in cuts.windows(2) {
                let (a, z) = (pair[0], pair[1]);
                let marked = underline.iter().any(|u| u.start <= a && z <= u.end);
                let mut format = TextFormat::simple(font.clone(), palette.color(kind));
                if marked {
                    format.underline = Stroke::new(1.5_f32, ERROR_RED);
                }
                let offset = job.text.len();
                job.text.push_str(&raw[a..z]);
                job.sections.push(egui::text::LayoutSection {
                    leading_space: 0.0,
                    byte_range: offset..job.text.len(),
                    format,
                });
            }
        }
    }
    job
}

/// The line numbers, with the lines that have a mistake in red.
fn gutter(lines: usize, errors: &[AsmError], font: &FontId, plain: Color32) -> LayoutJob {
    let width = lines.max(1).to_string().len();
    let mut job = LayoutJob::default();
    for n in 1..=lines.max(1) {
        let bad = errors.iter().any(|e| e.line == n);
        let text = if n < lines.max(1) {
            format!("{n:>width$}\n")
        } else {
            format!("{n:>width$}")
        };
        job.append(
            &text,
            LEADING,
            TextFormat::simple(font.clone(), if bad { ERROR_RED } else { plain }),
        );
    }
    job
}

/// Space left of the line numbers for the breakpoint dots.
const LEADING: f32 = 14.0;
const BREAKPOINT: Color32 = Color32::from_rgb(220, 50, 50);
const PC_MARK: Color32 = Color32::from_rgb(255, 190, 60);

/// Character index in `text` of 1-based `line` and `column`, clamped to the
/// line.
fn char_index(text: &str, line: usize, column: usize) -> usize {
    let mut index = 0;
    for (n, l) in text.split('\n').enumerate() {
        let len = l.chars().count();
        if n + 1 == line {
            return index + column.saturating_sub(1).min(len);
        }
        index += len + 1;
    }
    index.saturating_sub(1)
}

// ---------------------------------------------------------------------------
// The panel

struct Built {
    name: String,
    hash: u64,
    result: Result<Assembled, Vec<AsmError>>,
}

/// A sketch build running on a worker thread.
struct Building {
    name: String,
    text: String,
    rx: std::sync::mpsc::Receiver<Result<BuildOutput, BuildError>>,
}

/// How the last build of a sketch ended.
struct CppBuild {
    name: String,
    /// Hash of the text that was built.
    hash: u64,
    /// The size of the image, or why there is none.
    result: Result<usize, BuildError>,
}

#[derive(Default)]
pub struct CodeEditor {
    selected: Option<String>,
    /// The name being typed for a new file, while the box is open.
    new_name: Option<String>,
    /// The name box just opened: select the suggested name.
    new_fresh: bool,
    /// Files made on disk since the app last asked, to list in the project.
    created: Vec<PathBuf>,
    /// Why a file could not be made, until the app has shown it.
    problem: Option<String>,
    /// A typing session has already recorded its undo step.
    typing: bool,
    built: Option<Built>,
    /// New files are C++ sketches (the choice in the New… box).
    new_cpp: bool,
    /// A sketch being compiled on a worker thread.
    building: Option<Building>,
    /// The last finished sketch build.
    cpp: Option<CppBuild>,
    /// A place (line, column) to move the cursor to on the next frame.
    jump: Option<(usize, usize)>,
}

fn hash_of(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

impl CodeEditor {
    /// Collect a finished sketch build.
    fn poll_build(&mut self, ctx: &egui::Context) {
        let Some(b) = &self.building else { return };
        let finished = match b.rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(BuildError::Other("the build stopped unexpectedly".into()))
            }
        };
        let b = self.building.take().expect("checked above");
        let result = finished.map(|out| {
            let bytes = out.image.len();
            arduino::remember(&b.text, out.image);
            bytes
        });
        self.cpp = Some(CppBuild {
            hash: hash_of(&b.text),
            name: b.name,
            result,
        });
    }

    /// Compile sketch `name` on a worker thread.
    fn start_build(&mut self, ctx: &egui::Context, name: &str, text: &str) {
        let Some(cli) = arduino::find_cli(None) else {
            self.cpp = Some(CppBuild {
                name: name.to_string(),
                hash: hash_of(text),
                result: Err(BuildError::NoToolchain(arduino::SETUP_HELP.into())),
            });
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let (sketch, source, ctx) = (name.to_string(), text.to_string(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(arduino::build(&cli, &sketch_name(&sketch), &source));
            ctx.request_repaint();
        });
        self.building = Some(Building {
            name: name.to_string(),
            text: text.to_string(),
            rx,
        });
    }

    /// Show `name` the next time the panel is drawn.
    pub fn select(&mut self, name: &str) {
        self.selected = Some(name.to_string());
    }

    /// The build of `file`, reusing the last one if the text is unchanged.
    fn build(&mut self, name: &str, file: &SourceFile) -> &Result<Assembled, Vec<AsmError>> {
        let hash = hash_of(&file.text);
        let fresh = self
            .built
            .as_ref()
            .is_some_and(|b| b.name == name && b.hash == hash);
        if !fresh {
            self.built = Some(Built {
                name: name.to_string(),
                hash,
                result: asm_text::assemble(&file.text),
            });
        }
        &self.built.as_ref().expect("just built").result
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        doc: &mut Document,
        debugger: &mut Debugger,
        live: &mut Option<LiveSim>,
    ) {
        self.poll_build(ui.ctx());
        let names: Vec<String> = doc.schematic().sources.keys().cloned().collect();
        if self.selected.as_ref().is_none_or(|n| !names.contains(n)) {
            self.selected = names.first().cloned();
        }

        self.new_file_dialog(ui.ctx(), doc, &names);

        egui::TopBottomPanel::top("code_header").show_inside(ui, |ui| {
            ui.add_space(2.0);
            self.tabs(ui, doc, &names);
            self.header(ui, doc);
            self.location_note(ui, doc);
            ui.add_space(2.0);
        });

        let Some(name) = self.selected.clone() else {
            ui.add_space(12.0);
            ui.label("This schematic has no firmware source files yet.");
            ui.label(
                RichText::new(
                    "A source file is AVR assembly (.asm) or an Arduino sketch (.ino) kept as \
                     a file beside the schematic. Dunbrec builds it each time the circuit \
                     runs when a microcontroller's firmware is set to it. Open one from the \
                     Project panel, or create one here.",
                )
                .weak(),
            );
            ui.add_space(6.0);
            if ui.button("Create a source file").clicked() {
                self.create(doc, "main");
            }
            return;
        };
        let file = doc.schematic().sources[&name].clone();
        let is_asm = file.language == SourceLanguage::Asm;
        let errors: Vec<AsmError> = if is_asm {
            self.build(&name, &file)
                .as_ref()
                .err()
                .cloned()
                .unwrap_or_default()
        } else {
            match &self.cpp {
                Some(CppBuild {
                    name: n,
                    hash,
                    result: Err(BuildError::Failed { diagnostics, .. }),
                }) if *n == name && *hash == hash_of(&file.text) => diagnostics
                    .iter()
                    .filter(|d| d.error)
                    .map(|d| AsmError {
                        line: d.line,
                        column: d.column.max(1),
                        message: d.message.clone(),
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };

        if !errors.is_empty() {
            egui::TopBottomPanel::bottom("code_errors")
                .resizable(true)
                .default_height(86.0)
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for e in &errors {
                                let text = RichText::new(format!(
                                    "{}:{}  {}",
                                    e.line, e.column, e.message
                                ))
                                .monospace()
                                .color(ERROR_RED);
                                if ui
                                    .selectable_label(false, text)
                                    .on_hover_text("Go to this line")
                                    .clicked()
                                {
                                    self.jump = Some((e.line, e.column));
                                }
                            }
                        });
                });
        }
        // A build that could not run (no tool, no core...) is explained in
        // full, not just flagged.
        if !is_asm
            && let Some(CppBuild {
                name: n,
                hash,
                result: Err(e @ (BuildError::NoToolchain(_) | BuildError::Other(_))),
            }) = &self.cpp
            && *n == name
            && *hash == hash_of(&file.text)
        {
            let message = e.to_string();
            egui::TopBottomPanel::bottom("code_build_note").show_inside(ui, |ui| {
                ui.label(RichText::new(message).color(ERROR_RED));
            });
        }

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.editor(ui, doc, debugger, live, &name, &file, &errors);
        });
    }

    /// Files this panel has made on disk since the last call.
    pub fn take_created(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.created)
    }

    /// Why a file could not be made, once.
    pub fn take_problem(&mut self) -> Option<String> {
        self.problem.take()
    }

    /// Where the selected file lives, and whether it is on disk yet.
    fn location_note(&self, ui: &mut Ui, doc: &Document) {
        let Some(name) = &self.selected else { return };
        if !crate::project::is_file_source(name) {
            ui.label(
                RichText::new(
                    "Kept inside the circuit file (the older way). The Project panel's \
                     “Move to files” makes it a file of its own.",
                )
                .weak(),
            );
            return;
        }
        let path = doc.path().and_then(|p| p.parent()).map(|d| d.join(name));
        match path {
            Some(path) if path.is_file() => {
                ui.add(
                    egui::Label::new(RichText::new(format!("📄 {}", path.display())).weak())
                        .truncate(),
                )
                .on_hover_text(path.display().to_string());
            }
            Some(path) => {
                ui.colored_label(
                    WARNING_AMBER,
                    format!(
                        "Not on disk yet: it will be written to {} when you save the circuit.",
                        path.display()
                    ),
                );
            }
            None => {
                ui.colored_label(
                    WARNING_AMBER,
                    "Not saved yet: this circuit has no file, so this one will be written next \
                     to it when you first save the circuit (Ctrl+S).",
                );
            }
        }
    }

    /// The message box that asks for the name and kind of a new source file.
    fn new_file_dialog(&mut self, ctx: &egui::Context, doc: &mut Document, names: &[String]) {
        let Some(mut typed) = self.new_name.take() else {
            return;
        };
        let mut cpp = self.new_cpp;
        let mut fresh = std::mem::take(&mut self.new_fresh);
        let (name, chosen) = (
            typed.trim().to_string(),
            if cpp {
                SourceLanguage::Cpp
            } else {
                SourceLanguage::Asm
            },
        );
        let (key, _) = file_key(&name, chosen);
        let taken = names.contains(&key);
        let ok = !name.is_empty() && crate::project::valid_name(&name) && !taken;
        let folder = doc.path().and_then(|p| p.parent()).map(|d| d.to_path_buf());

        let answer = Dialog {
            id: "new_source",
            kind: dialog::Kind::Question,
            title: "New firmware file",
            buttons: &["Create", "Cancel"],
            default: 0,
            cancel: Some(1),
        }
        .show(ctx, |ui| {
            dialog::paragraph(ui, "Name of the new file:");
            ui.add_space(6.0);
            let response = ui.add(
                egui::TextEdit::singleline(&mut typed)
                    .font(egui::TextStyle::Heading)
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut fresh) {
                crate::project_ui::select_all(ui, &response, &typed);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.radio_value(&mut cpp, false, "Assembly");
                ui.radio_value(&mut cpp, true, "Arduino C++");
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(match folder.as_deref() {
                    Some(dir) => format!("It is created now, in {}.", dir.display()),
                    None => "This circuit has not been saved, so the file is written when you \
                             first save it."
                        .to_string(),
                })
                .weak(),
            );
            if taken {
                ui.add_space(4.0);
                ui.colored_label(ERROR_RED, "A file with that name already exists.");
            } else if !name.is_empty() && !crate::project::valid_name(&name) {
                ui.add_space(4.0);
                ui.colored_label(ERROR_RED, "That is not a name a file can have.");
            }
        });
        self.new_cpp = cpp;
        match answer {
            Some(0) if ok => self.create_as(doc, &name, chosen),
            // Not a usable name yet: keep asking.
            Some(0) => self.new_name = Some(typed),
            Some(_) => {}
            None => self.new_name = Some(typed),
        }
        self.new_fresh = fresh;
    }

    /// One tab per source file of the circuit, with a button to add another.
    /// Closing a tab that is a file only stops showing it here (it stays on
    /// disk); closing one kept inside the circuit removes it. Either way
    /// Ctrl+Z brings it back.
    fn tabs(&mut self, ui: &mut Ui, doc: &mut Document, names: &[String]) {
        let mut close = None;
        ui.horizontal(|ui| {
            if ui
                .button("＋ New…")
                .on_hover_text("Add another source file to this schematic")
                .clicked()
            {
                let free = unused_file_name(&doc.schematic().sources, "main", "asm");
                self.new_name = Some(free.trim_end_matches(".asm").to_string());
                self.new_fresh = true;
            }
            ui.separator();
            egui::ScrollArea::horizontal()
                .id_salt("code_tabs")
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for name in names {
                            let selected = self.selected.as_deref() == Some(name.as_str());
                            let is_file = crate::project::is_file_source(name);
                            let tab = ui.selectable_label(selected, name);
                            if tab.clicked() && !selected {
                                self.selected = Some(name.clone());
                                self.typing = false;
                            }
                            if tab.middle_clicked() {
                                close = Some(name.clone());
                            }
                            tab.on_hover_text(if is_file {
                                "A file beside the circuit"
                            } else {
                                "Kept inside the circuit file"
                            });
                            if ui
                                .small_button("×")
                                .on_hover_text(if is_file {
                                    "Close this tab. The file stays on disk; delete it in the \
                                     Project panel. (Ctrl+Z brings the tab back)"
                                } else {
                                    "Remove this file from the circuit (Ctrl+Z brings it back)"
                                })
                                .clicked()
                            {
                                close = Some(name.clone());
                            }
                            ui.add_space(6.0);
                        }
                    });
                });
        });
        if let Some(name) = close {
            let next = after_closing(names, &name);
            doc.edit(|s| s.sources.remove(&name));
            if self.selected.as_deref() == Some(name.as_str()) {
                self.selected = next;
                self.typing = false;
            }
        }
    }

    /// What the selected file builds to: its size, or how many errors.
    fn header(&mut self, ui: &mut Ui, doc: &Document) {
        ui.horizontal(|ui| {
            if let Some(name) = self.selected.clone() {
                let sketch = doc
                    .schematic()
                    .sources
                    .get(&name)
                    .filter(|f| f.language == SourceLanguage::Cpp)
                    .map(|f| f.text.clone());
                if let Some(text) = sketch {
                    self.build_controls(ui, &name, &text);
                    return;
                }
                match self.built.as_ref().filter(|b| b.name == name) {
                    Some(Built { result: Ok(a), .. }) => {
                        ui.label(
                            RichText::new(format!(
                                "{} bytes, {} of {} words of flash",
                                a.image.len(),
                                a.image.len() / 2,
                                FLASH_WORDS
                            ))
                            .weak(),
                        );
                    }
                    Some(Built { result: Err(e), .. }) => {
                        ui.label(
                            RichText::new(format!(
                                "{} error{}",
                                e.len(),
                                if e.len() == 1 { "" } else { "s" }
                            ))
                            .color(ERROR_RED),
                        );
                    }
                    None => {}
                }
            }
        });
    }

    /// The Build button and what it last did, for a C++ sketch.
    fn build_controls(&mut self, ui: &mut Ui, name: &str, text: &str) {
        let building = self.building.is_some();
        if ui
            .add_enabled(!building, egui::Button::new("Build"))
            .on_hover_text(
                "Compile this sketch with arduino-cli. The circuit runs the last build of \
                 exactly this text, so build again after changing it.",
            )
            .clicked()
        {
            let ctx = ui.ctx().clone();
            self.start_build(&ctx, name, text);
        }
        if building {
            ui.spinner();
            ui.label(RichText::new("Building…").weak());
            return;
        }
        if let Some(image) = arduino::cached_image(text) {
            ui.label(RichText::new(format!("Built: {} bytes", image.len())).weak());
            return;
        }
        match self
            .cpp
            .as_ref()
            .filter(|c| c.name == name && c.hash == hash_of(text))
        {
            Some(CppBuild {
                result: Err(BuildError::Failed { diagnostics, .. }),
                ..
            }) => {
                let n = diagnostics.iter().filter(|d| d.error).count();
                ui.label(
                    RichText::new(if n == 0 {
                        "Build failed".to_string()
                    } else {
                        format!("{n} error{}", if n == 1 { "" } else { "s" })
                    })
                    .color(ERROR_RED),
                );
            }
            Some(CppBuild { result: Err(_), .. }) => {
                ui.label(RichText::new("Build could not run").color(ERROR_RED));
            }
            _ => {
                ui.label(RichText::new("Not built since the last change").weak());
            }
        }
    }

    fn create(&mut self, doc: &mut Document, name: &str) {
        self.create_as(doc, name, SourceLanguage::Asm);
    }

    /// Add a source file called `typed` (an extension is added if it has
    /// none). A file of that name already on disk is opened, not replaced.
    fn create_as(&mut self, doc: &mut Document, typed: &str, language: SourceLanguage) {
        let (key, language) = file_key(typed, language);
        let dir = doc.path().and_then(|p| p.parent()).map(|d| d.to_path_buf());
        let on_disk = dir.as_ref().is_some_and(|d| d.join(&key).is_file());
        if on_disk
            && let Some(dir) = &dir
            && crate::project::attach(doc.schematic_mut(), dir, &key).is_ok()
        {
            self.selected = Some(key);
            self.typing = false;
            return;
        }
        // A circuit that has a file makes the new source file on disk now, as
        // the Project panel does, rather than keeping it in memory until the
        // next save.
        if let Some(dir) = &dir {
            let kind = match language {
                SourceLanguage::Asm => crate::project::NewSource::Assembly,
                SourceLanguage::Cpp => crate::project::NewSource::Sketch,
            };
            match crate::project::new_source(dir, &key, kind) {
                Ok(path) => match crate::project::attach(doc.schematic_mut(), dir, &key) {
                    Ok(()) => {
                        self.created.push(path);
                        self.selected = Some(key);
                        self.typing = false;
                        return;
                    }
                    Err(e) => {
                        self.problem = Some(e);
                        return;
                    }
                },
                Err(e) => {
                    self.problem = Some(e);
                    return;
                }
            }
        }
        // An unsaved circuit has no folder yet: the file is written beside it
        // when it is first saved.
        let file = match language {
            SourceLanguage::Asm => new_file(&doc.schematic().sources, &key),
            SourceLanguage::Cpp => new_sketch(),
        };
        doc.edit(|s| s.sources.insert(key.clone(), file));
        self.selected = Some(key);
        self.typing = false;
    }

    #[allow(clippy::too_many_arguments)] // the editor needs the document, the debugger and the file
    fn editor(
        &mut self,
        ui: &mut Ui,
        doc: &mut Document,
        debugger: &mut Debugger,
        live: &mut Option<LiveSim>,
        name: &str,
        file: &SourceFile,
        errors: &[AsmError],
    ) {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let row_height = ui.fonts_mut(|f| f.row_height(&font));
        let rows = ((ui.available_height() / row_height).ceil() as usize).max(8);
        // Each file has its own text box and scroll position, so switching
        // tabs keeps your place in every file.
        let id = Id::new(("code_text", name));
        let plain = ui.visuals().text_color();
        let palette = Palette::new(ui.visuals().dark_mode, plain);
        let weak = ui.visuals().weak_text_color();
        let is_asm = file.language == SourceLanguage::Asm;
        let mut text = file.text.clone();

        let mut jumped = None;
        if let Some((line, column)) = self.jump.take() {
            let index = char_index(&text, line, column);
            let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(index))));
            state.store(ui.ctx(), id);
            ui.memory_mut(|m| m.request_focus(id));
            jumped = Some(index);
        }

        let lines = text.split('\n').count();
        let marks = if is_asm {
            debugger.source_marks(name, &file.text, live)
        } else {
            Default::default()
        };
        let gutter_job = gutter(lines, errors, &font, weak);
        let mut clicked_line = None;
        let mut changed = false;
        let mut has_focus = false;
        egui::ScrollArea::both()
            .id_salt(("code_scroll", name))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let numbers = ui.add(egui::Label::new(gutter_job).selectable(false));
                    if is_asm {
                        let area = numbers.rect.expand2(egui::vec2(2.0, 0.0));
                        let click = ui.interact(area, id.with("gutter"), egui::Sense::click());
                        let top = numbers.rect.top();
                        let dot = |line: usize| {
                            egui::pos2(
                                numbers.rect.left() + 6.0,
                                top + (line as f32 - 0.5) * row_height,
                            )
                        };
                        let painter = ui.painter();
                        for &line in &marks.breakpoints {
                            painter.circle_filled(dot(line), 4.5, BREAKPOINT);
                        }
                        if let Some(line) = marks.pc_line {
                            let at = dot(line);
                            painter.add(egui::Shape::convex_polygon(
                                vec![
                                    at + egui::vec2(-4.0, -5.0),
                                    at + egui::vec2(5.0, 0.0),
                                    at + egui::vec2(-4.0, 5.0),
                                ],
                                PC_MARK,
                                Stroke::NONE,
                            ));
                        }
                        if let Some(pos) = click.hover_pos() {
                            let line = ((pos.y - top) / row_height).floor().max(0.0) as usize + 1;
                            let line = line.min(lines);
                            if !marks.breakpoints.contains(&line) {
                                painter.circle_filled(
                                    dot(line),
                                    4.5,
                                    BREAKPOINT.gamma_multiply(0.3),
                                );
                            }
                            if click.clicked() {
                                clicked_line = Some(line);
                            }
                        }
                        click.on_hover_text("Click to set or clear a breakpoint on this line");
                    }
                    let mut layouter = |ui: &Ui, buffer: &dyn egui::TextBuffer, _wrap: f32| {
                        let job = highlight(buffer.as_str(), is_asm, &font, &palette, errors);
                        let galley: Arc<egui::Galley> = ui.fonts_mut(|f| f.layout_job(job));
                        galley
                    };
                    let output = egui::TextEdit::multiline(&mut text)
                        .id(id)
                        .font(font.clone())
                        .code_editor()
                        .frame(false)
                        .desired_width(f32::INFINITY)
                        .desired_rows(rows)
                        .layouter(&mut layouter)
                        .show(ui);
                    changed = output.response.changed();
                    has_focus = output.response.has_focus();
                    if let Some(index) = jumped {
                        let rect = output
                            .galley
                            .pos_from_cursor(CCursor::new(index))
                            .translate(output.galley_pos.to_vec2());
                        ui.scroll_to_rect(rect, Some(Align::Center));
                    }
                });
            });

        if let Some(line) = clicked_line {
            debugger.toggle_source_line(live, name, &file.text, line);
        }
        if changed {
            if !self.typing {
                doc.checkpoint();
                self.typing = true;
            }
            if let Some(f) = doc.schematic_mut().sources.get_mut(name) {
                f.text = text;
            }
        }
        if !has_focus {
            self.typing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str) -> Vec<(&str, Kind)> {
        tokenize(line)
            .into_iter()
            .map(|(r, k)| (&line[r], k))
            .collect()
    }

    #[test]
    fn instructions_are_coloured_by_what_they_are() {
        assert_eq!(
            kinds("loop: ldi r16, 0xFF   ; set"),
            [
                ("loop", Kind::Label),
                ("ldi", Kind::Mnemonic),
                ("r16", Kind::Register),
                ("0xFF", Kind::Number),
                ("; set", Kind::Comment),
            ]
        );
        assert_eq!(
            kinds("  sbi DDRB, PB5"),
            [
                ("sbi", Kind::Mnemonic),
                ("DDRB", Kind::Constant),
                ("PB5", Kind::Constant),
            ]
        );
        assert_eq!(
            kinds(".db \"a;b\", 'x' // c"),
            [
                (".db", Kind::Directive),
                ("\"a;b\"", Kind::Text),
                ("'x'", Kind::Number),
                ("// c", Kind::Comment),
            ]
        );
        // An unknown word in the mnemonic place gets no colour, and a user
        // name in an operand stays plain.
        assert_eq!(kinds("frob mine"), []);
        assert_eq!(
            kinds("a: b: nop"),
            [
                ("a", Kind::Label),
                ("b", Kind::Label),
                ("nop", Kind::Mnemonic)
            ]
        );
        // Non-ASCII text does not break the scan.
        assert_eq!(kinds("; é ü"), [("; é ü", Kind::Comment)]);
        assert_eq!(kinds("nop é"), [("nop", Kind::Mnemonic)]);
    }

    #[test]
    fn the_starter_program_assembles() {
        let built = asm_text::assemble(STARTER).expect("the starter builds");
        assert!(built.image.len() > 10);
        assert!(built.label_address("loop").is_some());
    }

    #[test]
    fn names_for_new_files_are_unique_and_say_what_they_are() {
        let mut sources = BTreeMap::new();
        assert_eq!(unused_file_name(&sources, "main", "asm"), "main.asm");
        sources.insert("main.asm".to_string(), new_file(&sources, "main.asm"));
        assert_eq!(sources["main.asm"].text, STARTER);
        assert_eq!(unused_file_name(&sources, "main", "asm"), "main2.asm");
        assert_eq!(new_file(&sources, "main2.asm").text, "; main2.asm\n");
        sources.insert("main2.asm".to_string(), new_file(&sources, "main2.asm"));
        assert_eq!(unused_file_name(&sources, "main", "asm"), "main3.asm");
        // What is typed decides the file name and, if it has an extension,
        // the language.
        assert_eq!(
            file_key("blink", SourceLanguage::Asm),
            ("blink.asm".into(), SourceLanguage::Asm)
        );
        assert_eq!(
            file_key(" fade ", SourceLanguage::Cpp),
            ("fade.ino".into(), SourceLanguage::Cpp)
        );
        assert_eq!(
            file_key("x.cpp", SourceLanguage::Asm),
            ("x.cpp".into(), SourceLanguage::Cpp)
        );
        assert_eq!(
            file_key("x.S", SourceLanguage::Cpp),
            ("x.S".into(), SourceLanguage::Asm)
        );
        assert_eq!(sketch_name("fw/blink.ino"), "blink");
    }

    #[test]
    fn closing_a_tab_shows_its_neighbour() {
        let names: Vec<String> = ["a.asm", "b.asm", "c.asm"].map(String::from).to_vec();
        assert_eq!(after_closing(&names, "a.asm").as_deref(), Some("b.asm"));
        assert_eq!(after_closing(&names, "b.asm").as_deref(), Some("c.asm"));
        assert_eq!(after_closing(&names, "c.asm").as_deref(), Some("b.asm"));
        assert_eq!(after_closing(&names[..1], "a.asm"), None);
        assert_eq!(after_closing(&names, "zzz"), None);
    }

    #[test]
    fn a_new_file_never_replaces_one_on_disk() {
        let dir = std::env::temp_dir().join(format!("dunbrec-code-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.asm"), "nop\nnop\n").unwrap();
        let mut doc = Document::default();
        doc.mark_saved(dir.join("x.dbr"));
        let mut editor = CodeEditor::default();
        editor.create(&mut doc, "main");
        // The file that was there is opened as it is.
        assert_eq!(doc.schematic().sources["main.asm"].text, "nop\nnop\n");
        assert!(!doc.can_undo(), "opening a file is not an edit");
        // A name nothing has yet makes a new file on disk straight away, and
        // says so, so the app can list it in the project.
        editor.create(&mut doc, "fresh");
        let on_disk = std::fs::read_to_string(dir.join("fresh.asm")).unwrap();
        assert_eq!(doc.schematic().sources["fresh.asm"].text, on_disk);
        assert_eq!(editor.take_created(), vec![dir.join("fresh.asm")]);
        assert!(editor.take_created().is_empty(), "reported once");
        assert!(!doc.can_undo(), "a file on disk is not an undoable edit");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_file_for_an_unsaved_circuit_waits_in_memory() {
        let mut doc = Document::default();
        let mut editor = CodeEditor::default();
        editor.create(&mut doc, "fresh");
        // The first file of a circuit starts as the blink program.
        assert_eq!(doc.schematic().sources["fresh.asm"].text, STARTER);
        assert!(doc.can_undo(), "it is an edit until the circuit is saved");
        assert!(editor.take_created().is_empty());
        assert_eq!(editor.take_problem(), None);
    }

    #[test]
    fn a_file_that_cannot_be_made_is_reported() {
        let dir = std::env::temp_dir().join(format!("dunbrec-code-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut doc = Document::default();
        // The circuit's folder does not exist, so the file cannot be written.
        doc.mark_saved(dir.join("x.dbr"));
        let mut editor = CodeEditor::default();
        editor.create(&mut doc, "fresh");
        assert!(editor.take_problem().is_some());
        assert!(!doc.schematic().sources.contains_key("fresh.asm"));
        assert!(editor.take_created().is_empty());
    }

    #[test]
    fn error_positions_become_cursor_positions() {
        let text = "nop\nldi r16, é\nx";
        assert_eq!(char_index(text, 1, 1), 0);
        assert_eq!(char_index(text, 2, 1), 4);
        assert_eq!(char_index(text, 2, 10), 4 + 9);
        // Past the end of a line stays on that line.
        assert_eq!(char_index(text, 1, 99), 3);
        assert_eq!(char_index(text, 3, 1), 4 + 10 + 1);
    }

    #[test]
    fn the_highlight_keeps_every_character() {
        let font = FontId::monospace(13.0);
        let palette = Palette::new(true, Color32::WHITE);
        for text in ["", "\n", "nop\n", "ldi r16, é\r\nbad ; c\n\nx:", "\"open"] {
            let errors = [AsmError {
                line: 1,
                column: 2,
                message: String::new(),
            }];
            let job = highlight(text, true, &font, &palette, &errors);
            assert_eq!(job.text, text);
        }
    }

    #[test]
    fn the_panel_draws_and_typing_edits_the_document() {
        let mut doc = Document::default();
        let mut editor = CodeEditor::default();
        let mut debugger = Debugger::default();
        let mut live = None;
        let ctx = egui::Context::default();
        let mut frame = |doc: &mut Document, editor: &mut CodeEditor| {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| editor.show(ui, doc, &mut debugger, &mut live));
            });
        };
        // No files: the empty state draws.
        frame(&mut doc, &mut editor);
        editor.create(&mut doc, "main");
        assert!(doc.schematic().sources.contains_key("main.asm"));
        assert!(doc.can_undo());
        // A file with a mistake draws, with its error list.
        doc.schematic_mut()
            .sources
            .get_mut("main.asm")
            .unwrap()
            .text = "nop\nldi r16, nothing\n".into();
        frame(&mut doc, &mut editor);
        frame(&mut doc, &mut editor);
        let built = editor.built.as_ref().unwrap();
        assert_eq!(built.result.as_ref().unwrap_err()[0].line, 2);
        // Deleting goes back to the empty state, and undo restores the file.
        editor.selected = Some("main.asm".into());
        doc.edit(|s| s.sources.remove("main.asm"));
        frame(&mut doc, &mut editor);
        assert!(doc.undo());
        frame(&mut doc, &mut editor);
        assert_eq!(editor.selected.as_deref(), Some("main.asm"));
    }

    #[test]
    fn a_sketch_builds_from_the_panel_and_the_result_is_kept() {
        let Some(_) = arduino::find_cli(None) else {
            eprintln!("arduino-cli not installed: skipped");
            return;
        };
        let ctx = egui::Context::default();
        let mut editor = CodeEditor::default();
        let mut doc = Document::default();
        editor.create_as(&mut doc, "main", SourceLanguage::Cpp);
        let text = doc.schematic().sources["main.ino"].text.clone();
        assert_eq!(text, STARTER_SKETCH);
        editor.start_build(&ctx, "main.ino", &text);
        assert!(editor.building.is_some());
        let started = std::time::Instant::now();
        while editor.building.is_some() {
            assert!(
                started.elapsed().as_secs() < 120,
                "the build never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
            editor.poll_build(&ctx);
        }
        let done = editor.cpp.as_ref().expect("a result");
        assert!(done.result.is_ok(), "{:?}", done.result);
        assert!(arduino::cached_image(&text).is_some());
        // A sketch with a mistake keeps its diagnostics for the error list.
        let bad = "void setup() {}
void loop() { oops(); }
";
        editor.start_build(&ctx, "main.ino", bad);
        while editor.building.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(50));
            editor.poll_build(&ctx);
        }
        let Some(CppBuild {
            result: Err(BuildError::Failed { diagnostics, .. }),
            ..
        }) = &editor.cpp
        else {
            panic!("expected a failed build");
        };
        assert_eq!(diagnostics.iter().find(|d| d.error).unwrap().line, 2);
    }
}
