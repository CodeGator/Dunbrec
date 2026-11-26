//! The Project panel: the open project's circuits and other files as a tree,
//! with each circuit's source files under it (and those no circuit uses
//! under "Not used"), and the commands to make, rename and delete them. The
//! panel only asks; the app does the work (see
//! `DunbrecApp::run_project_command`).

use std::path::{Path, PathBuf};

use dunbrec_core::ComponentId;
use egui::{Color32, RichText, Ui};

use crate::dialog::{self, Dialog};
use crate::project::{
    CircuitCode, Entry, EntryKind, Project, Use, path_eq, relative_to, valid_name,
};

/// What a new item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Create {
    /// A circuit, in a folder of the project.
    Schematic,
    /// A source file for a microcontroller: assembly or an Arduino sketch.
    Assembly,
    Sketch,
}

impl Create {
    fn title(self) -> &'static str {
        match self {
            Create::Schematic => "New circuit",
            Create::Assembly => "New assembly file",
            Create::Sketch => "New Arduino sketch",
        }
    }

    /// A name to start the form with, which nothing in `dir` has yet.
    fn suggest(self, dir: &Path) -> String {
        let (base, ext) = match self {
            Create::Schematic => ("Circuit", "dbr"),
            Create::Assembly => ("main", "asm"),
            Create::Sketch => ("sketch", "ino"),
        };
        let taken = |name: &str| dir.join(format!("{name}.{ext}")).exists();
        // Circuits are numbered from 1 ("Circuit 1"); source files only get a
        // number when the plain name is taken ("main2").
        if self == Create::Schematic {
            return (1..)
                .map(|n| format!("{base} {n}"))
                .find(|n| !taken(n))
                .expect("a free number");
        }
        if !taken(base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base}{n}"))
            .find(|n| !taken(n))
            .expect("a free number")
    }
}

/// Something the panel asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    NewProject,
    OpenProject,
    OpenProjectPath(PathBuf),
    CloseProject,
    OpenSchematic(PathBuf),
    /// Open a circuit and show the tab of one of its source files.
    OpenCircuitSource {
        circuit: PathBuf,
        name: String,
    },
    /// The user picked a source file no circuit uses; the app asks which
    /// microcontroller should run it.
    AskAssign(PathBuf),
    /// The user chose to add a circuit in a folder; the app asks for its name
    /// (see [`NamePrompt`]) and then sends [`Command::CreateCircuit`].
    AskCircuit(PathBuf),
    CreateCircuit {
        dir: PathBuf,
        name: String,
    },
    /// Make a new source file for a microcontroller (asked for in the chip's
    /// properties, and named in a [`NamePrompt`]).
    CreateForChip {
        circuit: PathBuf,
        chip: ComponentId,
        name: String,
        what: Create,
    },
    /// The user chose Rename; the app asks for the new name.
    AskRename(PathBuf),
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    /// Take a file or folder out of the project, leaving it on the disk.
    RemoveFromProject(PathBuf),
    /// The user asked to delete a file or folder from the disk; the app
    /// confirms in a message box before doing it.
    AskDelete(PathBuf),
    /// Pick existing files to add to the project.
    AddExisting,
    Refresh,
    MoveSourcesToFiles,
    /// Take a project off the recent list.
    ForgetProject(PathBuf),
    ClearRecentProjects,
}

/// What the panel shows, borrowed for one frame.
pub struct View<'a> {
    pub project: Option<&'a Project>,
    /// What each of the project's circuits runs, the open ones as they are
    /// now, saved or not.
    pub code: &'a [CircuitCode],
    /// The files of the circuits that are open. Problems with a circuit's
    /// files are pointed out only once it is.
    pub open: &'a [PathBuf],
    pub recent: &'a [PathBuf],
    /// The schematic open in the app.
    pub current: Option<&'a Path>,
    /// How many sources the open schematic still keeps inside itself.
    pub embedded: usize,
}

#[derive(Default)]
pub struct ProjectPanel;

fn same(a: &Path, b: &Path) -> bool {
    path_eq(a, b)
}

/// What a row says: a circuit by its name alone, any other file with its
/// extension (which tells an assembly file from a sketch).
fn shown_name(e: &Entry) -> &str {
    match e.kind {
        EntryKind::Schematic => e.name.strip_suffix(".dbr").unwrap_or(&e.name),
        _ => &e.name,
    }
}

fn icon(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Folder => "📁",
        EntryKind::Schematic => "⚡",
        EntryKind::Source(_) => "📝",
        EntryKind::Header => "📄",
        EntryKind::Hex => "🔢",
        EntryKind::Other => "·",
    }
}

impl ProjectPanel {
    pub fn show(&mut self, ui: &mut Ui, view: &View) -> Option<Command> {
        let Some(project) = view.project else {
            return no_project(ui, view.recent);
        };
        let mut out = None;
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(RichText::new(format!("📁 {}", project.name())).strong())
                    .sense(egui::Sense::click()),
            )
            .on_hover_text(format!(
                "{}\nRight-click to add to the project",
                project.file.display()
            ))
            .context_menu(|ui| {
                if let Some(command) = root_menu(ui, &project.root) {
                    out = Some(command);
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("×")
                    .on_hover_text("Close the project (no file is touched)")
                    .clicked()
                {
                    out = Some(Command::CloseProject);
                }
                if ui
                    .small_button("⟳")
                    .on_hover_text("Read the project file and look at the disk again")
                    .clicked()
                {
                    out = Some(Command::Refresh);
                }
                ui.menu_button("＋ Add", |ui| {
                    if let Some(command) = add_menu(ui, &project.root) {
                        out = Some(command);
                    }
                });
            });
        });
        ui.separator();

        if view.embedded > 0 {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(format!(
                    "This schematic keeps {} firmware source{} inside its own file.",
                    view.embedded,
                    if view.embedded == 1 { "" } else { "s" }
                ));
                if ui
                    .button("Move to files in this folder")
                    .on_hover_text(
                        "Writes each source as a file beside the schematic and points the \
                         microcontrollers at it (Ctrl+Z undoes the change in the schematic)",
                    )
                    .clicked()
                {
                    out = Some(Command::MoveSourcesToFiles);
                }
            });
            ui.add_space(4.0);
        }

        // Right-clicking empty space adds to the project too. This is made
        // first, so the rows drawn over it get their own clicks.
        let area = ui.available_rect_before_wrap();
        ui.interact(
            area,
            ui.id().with("project_background"),
            egui::Sense::click(),
        )
        .context_menu(|ui| {
            if let Some(command) = root_menu(ui, &project.root) {
                out = Some(command);
            }
        });
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if project.entries.is_empty() {
                    ui.label(
                        RichText::new("Nothing here yet. Right-click to add a circuit.").weak(),
                    );
                }
                if let Some(command) = self.tree(ui, project, &project.entries, view) {
                    out = Some(command);
                }
                if let Some(command) = self.not_used(ui, project, view) {
                    out = Some(command);
                }
            });
        out
    }

    /// The folders, circuits and other files. Source files are not shown
    /// here: each is under the circuit that uses it, or under "Not used".
    fn tree(
        &mut self,
        ui: &mut Ui,
        project: &Project,
        entries: &[Entry],
        view: &View,
    ) -> Option<Command> {
        let mut out = None;
        for e in entries {
            if !listed_in_tree(project, e) {
                continue;
            }
            if e.kind == EntryKind::Folder {
                let header = egui::CollapsingHeader::new(format!("{} {}", icon(e.kind), e.name))
                    .id_salt(&e.path)
                    .default_open(true)
                    .show(ui, |ui| self.tree(ui, project, &e.children, view));
                if let Some(Some(command)) = header.body_returned {
                    out = Some(command);
                }
                header.header_response.context_menu(|ui| {
                    if let Some(command) = item_menu(ui, &e.path, None, true) {
                        out = Some(command);
                    }
                });
                continue;
            }
            let selected = view.current.is_some_and(|c| same(c, &e.path));
            if e.missing {
                if let Some(command) = missing_row(ui, e, &e.name) {
                    out = Some(command);
                }
                continue;
            }
            let openable = e.kind == EntryKind::Schematic;
            let mut label = RichText::new(format!("{} {}", icon(e.kind), shown_name(e)));
            if !openable {
                label = label.weak();
            }
            let row = ui.selectable_label(selected, label);
            let row = if openable {
                row.on_hover_text("Double-click to open")
            } else {
                row
            };
            let open = openable.then(|| Command::OpenSchematic(e.path.clone()));
            if row.double_clicked()
                && let Some(command) = open.clone()
            {
                out = Some(command);
            }
            row.context_menu(|ui| {
                if let Some(command) = item_menu(ui, &e.path, open.clone(), true) {
                    out = Some(command);
                }
            });
            // Only a circuit with source files has anything under it.
            if let Some(code) = view.code.iter().find(|c| same(&c.circuit, &e.path))
                && !code.uses.is_empty()
            {
                ui.indent(("code", &e.path), |ui| {
                    for u in by_chip(&code.uses) {
                        if let Some(command) = self.used_row(ui, code, u, view) {
                            out = Some(command);
                        }
                    }
                });
            }
        }
        out
    }

    /// A source file under the circuit that uses it.
    fn used_row(
        &mut self,
        ui: &mut Ui,
        code: &CircuitCode,
        u: &Use,
        view: &View,
    ) -> Option<Command> {
        let mut out = None;
        let title = format!("{}: {}", u.chips.join(", "), u.name);
        // Only circuits from before files belonged to one circuit do this.
        let others: Vec<String> = view
            .code
            .iter()
            .filter(|c| !same(&c.circuit, &code.circuit) && c.use_of(&u.path).is_some())
            .map(CircuitCode::name)
            .collect();
        let exists = u.path.is_file();
        let opened = view.open.iter().any(|f| same(f, &code.circuit));
        let (text, hint) = if opened && !exists {
            (
                RichText::new(format!("⚠ {title}")).color(MISSING),
                format!(
                    "{} is not on the disk. Right-click to remove it, or choose another \
                     file in the chip's properties.",
                    u.path.display()
                ),
            )
        } else if !opened || others.is_empty() {
            (
                RichText::new(format!("📝 {title}")),
                "Double-click to open".to_string(),
            )
        } else {
            (
                RichText::new(format!("⚠ {title}")).color(SHARED),
                format!(
                    "{} also uses this file. Circuits should each have their own: opening \
                     one offers to make a copy.",
                    others.join(", ")
                ),
            )
        };
        let open = Command::OpenCircuitSource {
            circuit: code.circuit.clone(),
            name: u.name.clone(),
        };
        let row = ui.selectable_label(false, text).on_hover_text(hint);
        if row.double_clicked() {
            out = Some(open.clone());
        }
        row.context_menu(|ui| {
            if let Some(command) = item_menu(ui, &u.path, Some(open.clone()), exists) {
                out = Some(command);
            }
        });
        out
    }

    /// The source files of the project that no circuit's chip runs.
    fn not_used(&mut self, ui: &mut Ui, project: &Project, view: &View) -> Option<Command> {
        let unused: Vec<&Entry> = sources(&project.entries)
            .into_iter()
            .filter(|e| view.code.iter().all(|c| c.use_of(&e.path).is_none()))
            .collect();
        if unused.is_empty() {
            return None;
        }
        let mut out = None;
        ui.add_space(4.0);
        egui::CollapsingHeader::new("Not used")
            .id_salt("not used")
            .default_open(true)
            .show(ui, |ui| {
                for e in unused {
                    let name = relative_to(&project.root, &e.path);
                    if e.missing {
                        if let Some(command) = missing_row(ui, e, &name) {
                            out = Some(command);
                        }
                        continue;
                    }
                    let assign = Command::AskAssign(e.path.clone());
                    let row = ui
                        .selectable_label(false, format!("{} {name}", icon(e.kind)))
                        .on_hover_text("Double-click to choose a microcontroller to run it");
                    if row.double_clicked() {
                        out = Some(assign.clone());
                    }
                    row.context_menu(|ui| {
                        if let Some(command) = item_menu(ui, &e.path, Some(assign.clone()), true) {
                            out = Some(command);
                        }
                    });
                }
            });
        out
    }
}

/// The menu of a circuit, file or folder: the same four for each, with those
/// that cannot be done greyed. `there` is false for one the disk has lost.
fn item_menu(ui: &mut Ui, path: &Path, open: Option<Command>, there: bool) -> Option<Command> {
    let mut out = None;
    if ui
        .add_enabled(there && open.is_some(), egui::Button::new("Open"))
        .clicked()
    {
        out = open;
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(there, egui::Button::new("Rename…"))
        .clicked()
    {
        out = Some(Command::AskRename(path.to_path_buf()));
        ui.close();
    }
    if ui
        .button("Remove from project")
        .on_hover_text("Nothing is deleted from the disk")
        .clicked()
    {
        out = Some(Command::RemoveFromProject(path.to_path_buf()));
        ui.close();
    }
    if ui
        .add_enabled(there, egui::Button::new("Delete from disk…"))
        .on_hover_text("To the Recycle Bin, after asking")
        .clicked()
    {
        out = Some(Command::AskDelete(path.to_path_buf()));
        ui.close();
    }
    out
}

/// What can be added to the project, in the folder `dir`: a circuit, or a file
/// that exists already. (A source file is made from the chip that is to run
/// it.)
fn add_menu(ui: &mut Ui, dir: &Path) -> Option<Command> {
    let mut out = None;
    if ui.button("New circuit…").clicked() {
        out = Some(Command::AskCircuit(dir.to_path_buf()));
        ui.close();
    }
    if ui
        .button("Add existing file…")
        .on_hover_text("List files that are already on the disk as part of the project")
        .clicked()
    {
        out = Some(Command::AddExisting);
        ui.close();
    }
    out
}

/// The menu of the project's name and of empty space.
fn root_menu(ui: &mut Ui, root: &Path) -> Option<Command> {
    let mut out = add_menu(ui, root);
    ui.separator();
    if ui.button("Close project").clicked() {
        out = Some(Command::CloseProject);
        ui.close();
    }
    out
}

/// A circuit's source files in the order of the chips that run them.
fn by_chip(uses: &[Use]) -> Vec<&Use> {
    // U2 before U10.
    let first = |u: &Use| u.chips.iter().map(|d| (d.len(), d.clone())).min();
    let mut out: Vec<&Use> = uses.iter().collect();
    out.sort_by_key(|u| first(u));
    out
}

/// A file the disk no longer has.
const MISSING: Color32 = Color32::from_rgb(225, 70, 70);
/// A file two circuits use.
const SHARED: Color32 = Color32::from_rgb(220, 150, 30);

/// A listed file that is not on the disk: all that can be done is to take it
/// out of the project.
fn missing_row(ui: &mut Ui, e: &Entry, name: &str) -> Option<Command> {
    let mut out = None;
    let row = ui
        .selectable_label(false, RichText::new(format!("⚠ {name}")).color(MISSING))
        .on_hover_text("This file is in the project but not on the disk");
    row.context_menu(|ui| {
        if let Some(command) = item_menu(ui, &e.path, None, false) {
            out = Some(command);
        }
    });
    out
}

/// Whether an entry has a row in the tree of folders: everything but source
/// files, and the folders that hold nothing else unless the project was given
/// them as folders.
fn listed_in_tree(project: &Project, e: &Entry) -> bool {
    match e.kind {
        EntryKind::Source(_) => false,
        EntryKind::Folder => {
            e.children.is_empty()
                || e.children.iter().any(|c| listed_in_tree(project, c))
                || project.lists_folder(&e.path)
        }
        _ => true,
    }
}

/// Every source file under `entries`.
fn sources(entries: &[Entry]) -> Vec<&Entry> {
    entries
        .iter()
        .flat_map(|e| match e.kind {
            EntryKind::Source(_) => vec![e],
            EntryKind::Folder => sources(&e.children),
            _ => Vec::new(),
        })
        .collect()
}

/// A message box asking for the name of something new, or the new name of
/// something that exists.
pub struct NamePrompt {
    kind: PromptKind,
    name: String,
    /// The first frame: focus the box and select the suggested name.
    fresh: bool,
}

enum PromptKind {
    Circuit {
        dir: PathBuf,
    },
    Source {
        circuit: PathBuf,
        chip: ComponentId,
        who: String,
        what: Create,
    },
    Rename {
        path: PathBuf,
    },
}

impl NamePrompt {
    /// A new circuit in the folder `dir`.
    pub fn circuit(dir: &Path) -> Self {
        NamePrompt {
            name: Create::Schematic.suggest(dir),
            kind: PromptKind::Circuit {
                dir: dir.to_path_buf(),
            },
            fresh: true,
        }
    }

    /// A new source file beside `circuit` for the microcontroller `chip`.
    pub fn source(circuit: &Path, chip: ComponentId, who: &str, what: Create) -> Self {
        let dir = circuit.parent().unwrap_or(Path::new(""));
        NamePrompt {
            name: what.suggest(dir),
            kind: PromptKind::Source {
                circuit: circuit.to_path_buf(),
                chip,
                who: who.to_string(),
                what,
            },
            fresh: true,
        }
    }

    pub fn rename(path: &Path) -> Self {
        NamePrompt {
            name: file_name_of(path),
            kind: PromptKind::Rename {
                path: path.to_path_buf(),
            },
            fresh: true,
        }
    }

    /// What the answer would ask the app to do, or `None` if the name is
    /// not acceptable (empty, not a legal file name, or unchanged).
    fn command(&self) -> Option<Command> {
        let name = self.name.trim();
        if !valid_name(name) {
            return None;
        }
        match &self.kind {
            PromptKind::Circuit { dir } => Some(Command::CreateCircuit {
                dir: dir.clone(),
                name: name.to_string(),
            }),
            PromptKind::Source {
                circuit,
                chip,
                what,
                ..
            } => Some(Command::CreateForChip {
                circuit: circuit.clone(),
                chip: *chip,
                name: name.to_string(),
                what: *what,
            }),
            PromptKind::Rename { path } => (name != file_name_of(path)).then(|| Command::Rename {
                from: path.clone(),
                to: path.with_file_name(name),
            }),
        }
    }

    /// Draw the box. `None` while it is open; `Some(None)` when the user
    /// cancelled; `Some(Some(command))` when they accepted a good name.
    pub fn show(&mut self, ctx: &egui::Context, project: &Project) -> Option<Option<Command>> {
        let (title, ok_label) = match &self.kind {
            PromptKind::Circuit { .. } => (Create::Schematic.title(), "Create"),
            PromptKind::Source { what, .. } => (what.title(), "Create"),
            PromptKind::Rename { .. } => ("Rename", "Rename"),
        };
        let heading = match &self.kind {
            PromptKind::Circuit { dir } => {
                let place = relative_to(&project.root, dir);
                let place = if place.is_empty() {
                    project.name()
                } else {
                    format!("{}/{place}", project.name())
                };
                format!("Name of the new circuit, in {place}:")
            }
            PromptKind::Source { who, .. } => format!("Name of the new file for {who}:"),
            PromptKind::Rename { path } => format!("New name for “{}”:", file_name_of(path)),
        };
        let note = match &self.kind {
            PromptKind::Rename { .. } => {
                Some("Schematics that use it are updated to the new name.")
            }
            _ => None,
        };
        let name = &mut self.name;
        let fresh = &mut self.fresh;
        let answer = Dialog {
            id: "project_name",
            kind: dialog::Kind::Question,
            title,
            buttons: &[ok_label, "Cancel"],
            default: 0,
            cancel: Some(1),
        }
        .show(ctx, |ui| {
            dialog::paragraph(ui, &heading);
            ui.add_space(6.0);
            let r = ui.add(
                egui::TextEdit::singleline(name)
                    .font(egui::TextStyle::Heading)
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(fresh) {
                select_all(ui, &r, name);
            }
            if !name.trim().is_empty() && !valid_name(name) {
                ui.add_space(4.0);
                ui.colored_label(
                    Color32::from_rgb(225, 70, 70),
                    "That is not a name a file can have.",
                );
            }
            if let Some(note) = note {
                ui.add_space(4.0);
                ui.label(RichText::new(note).weak());
            }
        });
        match answer {
            Some(0) => self.command().map(Some),
            Some(_) => Some(None),
            None => None,
        }
    }
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Focus a text box and select what is in it, so typing replaces it.
pub fn select_all(ui: &Ui, response: &egui::Response, text: &str) {
    response.request_focus();
    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), response.id) {
        let end = egui::text::CCursor::new(text.chars().count());
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                end,
            )));
        state.store(ui.ctx(), response.id);
    }
}

/// What the panel shows with no project open.
fn no_project(ui: &mut Ui, recent: &[PathBuf]) -> Option<Command> {
    let mut out = None;
    ui.add_space(8.0);
    ui.label(RichText::new("No project open").strong());
    ui.label(
        RichText::new(
            "A project is a file that lists a design's schematics and firmware source \
             files, so they are kept together. The files can be anywhere you like.",
        )
        .weak(),
    );
    ui.add_space(6.0);
    if ui.button("New project…").clicked() {
        out = Some(Command::NewProject);
    }
    if ui.button("Open project…").clicked() {
        out = Some(Command::OpenProject);
    }
    if !recent.is_empty() {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Recent projects").strong());
            if ui
                .small_button("Clear list")
                .on_hover_text("Forget all of them (the folders are not touched)")
                .clicked()
            {
                out = Some(Command::ClearRecentProjects);
            }
        });
        for path in recent {
            let name = path.file_stem().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            ui.horizontal(|ui| {
                let open = ui
                    .button(format!("📁 {name}"))
                    .on_hover_text(path.display().to_string());
                if open.clicked() {
                    out = Some(Command::OpenProjectPath(path.clone()));
                }
                if ui
                    .small_button("×")
                    .on_hover_text("Remove from the list (the folder is not touched)")
                    .clicked()
                {
                    out = Some(Command::ForgetProject(path.clone()));
                }
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dunbrec-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("fw")).unwrap();
        for f in ["a.dbr", "fw/main.asm", "fw/blink.ino", "notes.txt"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        dir
    }

    /// Draw the panel for a few frames and return what it asked for.
    fn draw(panel: &mut ProjectPanel, view: &View) -> Option<Command> {
        let ctx = egui::Context::default();
        let mut asked = None;
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if let Some(c) = panel.show(ui, view) {
                        asked = Some(c);
                    }
                });
            });
        }
        asked
    }

    #[test]
    fn the_panel_draws_with_and_without_a_project() {
        let mut panel = ProjectPanel;
        let recent = vec![PathBuf::from("a"), PathBuf::from("b")];
        let none = View {
            project: None,
            code: &[],
            open: &[],
            recent: &recent,
            current: None,
            embedded: 0,
        };
        assert_eq!(draw(&mut panel, &none), None);

        let dir = scratch();
        let (mut project, _) = Project::create(&dir.join("P")).unwrap();
        project
            .add_files(&[
                dir.join("fw").join("main.asm"),
                dir.join("fw").join("blink.ino"),
                dir.join("a.dbr"),
                dir.join("gone.asm"),
            ])
            .unwrap();
        let view = View {
            project: Some(&project),
            code: &project.code,
            open: &[],
            recent: &recent,
            current: Some(&dir.join("a.dbr")),
            embedded: 2,
        };
        assert_eq!(draw(&mut panel, &view), None);

        // The name boxes draw, and a bad or unchanged name asks for nothing.
        for mut prompt in [
            NamePrompt::circuit(&dir),
            NamePrompt::source(
                &dir.join("a.dbr"),
                ComponentId(1),
                "U1 in a",
                Create::Sketch,
            ),
            NamePrompt::rename(&dir.join("a.dbr")),
        ] {
            let ctx = egui::Context::default();
            let mut answer = None;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                answer = prompt.show(ctx, &project);
            });
            assert_eq!(answer, None, "the box stays open until answered");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The labels the panel shows for `project`, top to bottom, with the
    /// circuits in `open` open.
    fn labels(project: &Project, open: &[PathBuf]) -> Vec<String> {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut found: Vec<(f64, String)> = Vec::new();
        for _ in 0..4 {
            let view = View {
                project: Some(project),
                code: &project.code,
                open,
                recent: &[],
                current: None,
                embedded: 0,
            };
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(500.0, 900.0),
                )),
                ..Default::default()
            };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| ProjectPanel.show(ui, &view));
            });
            if let Some(update) = out.platform_output.accesskit_update {
                found = update
                    .nodes
                    .iter()
                    .filter_map(|(_, n)| {
                        let text = n.label().or_else(|| n.value())?.to_string();
                        Some((n.bounds()?.y0, text))
                    })
                    .collect();
            }
        }
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        found.into_iter().map(|(_, text)| text).collect()
    }

    #[test]
    fn source_files_sit_under_the_circuit_that_uses_them() {
        let dir = std::env::temp_dir().join(format!("dunbrec-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("fw")).unwrap();
        let circuit = |name: &str, firmware: &[&str]| {
            let mut s = dunbrec_core::Schematic::new();
            for firmware in firmware {
                s.add_component(
                    dunbrec_core::ComponentKind::Mcu {
                        firmware: (*firmware).into(),
                        clock: 16e6,
                    },
                    Default::default(),
                );
            }
            let path = dir.join(name);
            dunbrec_io::dbr::save(&s, &path).unwrap();
            path
        };
        for file in ["fw/blink.ino", "old.asm", "shared.asm", "notes.txt"] {
            std::fs::write(dir.join(file), "").unwrap();
        }
        let files = [
            circuit("Blink.dbr", &["fw/blink.ino", "gone.asm"]),
            circuit("Fader.dbr", &["shared.asm", "shared.asm"]),
            circuit("Other.dbr", &["shared.asm"]),
            circuit("Plain.dbr", &["builtin:blink", "", "fw.hex"]),
            dir.join("fw").join("blink.ino"),
            dir.join("old.asm"),
            dir.join("shared.asm"),
            dir.join("notes.txt"),
        ];
        let (mut project, _) = Project::create(&dir.join("P")).unwrap();
        project.add_files(&files).unwrap();
        // Nothing is pointed out in circuits that have not been opened.
        let quiet = labels(&project, &[]);
        assert!(!quiet.iter().any(|t| t.starts_with('⚠')), "{quiet:?}");
        assert!(quiet.iter().any(|t| t == "📝 U2: gone.asm"));
        let shown = labels(&project, &files[..4]);
        let rows: Vec<&str> = shown
            .iter()
            .map(String::as_str)
            .skip_while(|t| !t.ends_with("Blink"))
            .collect();
        assert_eq!(
            rows,
            [
                "⚡ Blink",
                "📝 U1: fw/blink.ino",
                "⚠ U2: gone.asm",
                "⚡ Circuit 1",
                "⚡ Fader",
                "⚠ U1, U2: shared.asm",
                "· notes.txt",
                "⚡ Other",
                "⚠ U1: shared.asm",
                // Chips with no source file add nothing to see.
                "⚡ Plain",
                "Not used",
                "📝 old.asm",
            ],
            "{shown:?}"
        );
        // The folder that held only a source file has no row.
        assert!(!shown.iter().any(|t| t.ends_with(" fw")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_name_prompt_accepts_only_good_names() {
        let dir = PathBuf::from("proj");
        let mut p = NamePrompt::source(
            &dir.join("a.dbr"),
            ComponentId(1),
            "U1 in a",
            Create::Assembly,
        );
        assert!(matches!(
            p.command(),
            Some(Command::CreateForChip {
                what: Create::Assembly,
                ..
            })
        ));
        p.name = "  led.asm  ".into();
        assert!(matches!(
            p.command(),
            Some(Command::CreateForChip { name, .. }) if name == "led.asm"
        ));
        assert!(matches!(
            NamePrompt::circuit(&dir).command(),
            Some(Command::CreateCircuit { .. })
        ));
        for bad in ["", "   ", "bad/name", "a:b", ".."] {
            p.name = bad.into();
            assert_eq!(p.command(), None, "{bad:?}");
        }

        let mut r = NamePrompt::rename(&dir.join("a.dbr"));
        assert_eq!(r.name, "a.dbr");
        assert_eq!(r.command(), None, "an unchanged name asks for nothing");
        r.name = "b.dbr".into();
        assert_eq!(
            r.command(),
            Some(Command::Rename {
                from: dir.join("a.dbr"),
                to: dir.join("b.dbr"),
            })
        );
    }

    #[test]
    fn circuits_are_listed_by_name_without_the_extension() {
        let entry = |name: &str| Entry {
            name: name.to_string(),
            path: PathBuf::from(name),
            kind: crate::project::kind_of(name),
            children: Vec::new(),
            missing: false,
        };
        assert_eq!(shown_name(&entry("Blinker.dbr")), "Blinker");
        assert_eq!(shown_name(&entry("main.asm")), "main.asm");
        assert_eq!(shown_name(&entry("fade.ino")), "fade.ino");
        assert_eq!(shown_name(&entry("notes.txt")), "notes.txt");
    }

    #[test]
    fn new_items_start_with_a_free_name() {
        let dir = std::env::temp_dir().join(format!("dunbrec-suggest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(Create::Schematic.suggest(&dir), "Circuit 1");
        assert_eq!(Create::Assembly.suggest(&dir), "main");
        assert_eq!(Create::Sketch.suggest(&dir), "sketch");
        for f in ["Circuit 1.dbr", "Circuit 2.dbr", "main.asm", "sketch.ino"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        assert_eq!(Create::Schematic.suggest(&dir), "Circuit 3");
        assert_eq!(Create::Assembly.suggest(&dir), "main2");
        assert_eq!(Create::Sketch.suggest(&dir), "sketch2");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn icons_say_what_each_entry_is() {
        let kinds = [
            EntryKind::Folder,
            EntryKind::Schematic,
            EntryKind::Source(dunbrec_core::SourceLanguage::Asm),
            EntryKind::Header,
            EntryKind::Hex,
            EntryKind::Other,
        ];
        let icons: std::collections::HashSet<_> = kinds.iter().map(|k| icon(*k)).collect();
        assert_eq!(icons.len(), kinds.len());
    }
}
