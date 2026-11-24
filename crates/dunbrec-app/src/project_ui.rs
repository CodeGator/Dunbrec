//! The Project panel: the open project's folder as a tree of schematics and
//! source files, with the commands to make, rename and delete them. The panel
//! only asks; the app does the work (see `DunbrecApp::run_project_command`).

use std::path::{Path, PathBuf};

use egui::{Color32, RichText, Ui};

use crate::dialog::{self, Dialog};
use crate::project::{Entry, EntryKind, Project, path_eq, relative_to, valid_name};

/// What a new item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Create {
    Schematic,
    Assembly,
    Sketch,
    Folder,
}

impl Create {
    /// Everything the panel can add, in menu order.
    pub const ALL: [Create; 4] = [
        Create::Schematic,
        Create::Assembly,
        Create::Sketch,
        Create::Folder,
    ];

    fn title(self) -> &'static str {
        match self {
            Create::Schematic => "New circuit",
            Create::Assembly => "New assembly file",
            Create::Sketch => "New Arduino sketch",
            Create::Folder => "New folder",
        }
    }

    /// A name to start the form with, which nothing in `dir` has yet.
    fn suggest(self, dir: &Path) -> String {
        let (base, ext) = match self {
            Create::Schematic => ("Circuit", "dbr"),
            Create::Assembly => ("main", "asm"),
            Create::Sketch => ("sketch", "ino"),
            Create::Folder => ("New folder", ""),
        };
        let taken = |name: &str| {
            let file = if ext.is_empty() {
                name.to_string()
            } else {
                format!("{name}.{ext}")
            };
            dir.join(file).exists()
        };
        // Circuits are numbered from 1 ("Circuit 1"); the rest only get a
        // number when the plain name is taken ("main2", "New folder 2").
        let spaced = matches!(self, Create::Schematic | Create::Folder);
        let numbered = |n: usize| {
            if spaced {
                format!("{base} {n}")
            } else {
                format!("{base}{n}")
            }
        };
        if self == Create::Schematic {
            return (1..)
                .map(numbered)
                .find(|n| !taken(n))
                .expect("a free number");
        }
        if !taken(base) {
            return base.to_string();
        }
        (2..)
            .map(numbered)
            .find(|n| !taken(n))
            .expect("a free number")
    }

    /// The menu entry.
    pub fn menu_label(self) -> &'static str {
        match self {
            Create::Schematic => "Circuit…",
            Create::Assembly => "Assembly source file…",
            Create::Sketch => "Arduino sketch…",
            Create::Folder => "Folder…",
        }
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
    OpenSource(PathBuf),
    /// The user chose to add something; the app asks for its name (see
    /// [`NamePrompt`]) and then sends [`Command::Create`].
    AskCreate {
        dir: PathBuf,
        what: Create,
    },
    Create {
        dir: PathBuf,
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
                if let Some(command) = self.root_menu(ui, &project.root) {
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
                    if let Some(command) = self.new_menu(ui, &project.root) {
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
            if let Some(command) = self.root_menu(ui, &project.root) {
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
                if let Some(command) = self.tree(ui, &project.entries, view.current) {
                    out = Some(command);
                }
            });
        out
    }

    fn tree(&mut self, ui: &mut Ui, entries: &[Entry], current: Option<&Path>) -> Option<Command> {
        let mut out = None;
        for e in entries {
            if e.kind == EntryKind::Folder {
                let header = egui::CollapsingHeader::new(format!("{} {}", icon(e.kind), e.name))
                    .id_salt(&e.path)
                    .default_open(true)
                    .show(ui, |ui| self.tree(ui, &e.children, current));
                if let Some(Some(command)) = header.body_returned {
                    out = Some(command);
                }
                header.header_response.context_menu(|ui| {
                    if let Some(command) = self.context_new(ui, &e.path) {
                        out = Some(command);
                    }
                    ui.separator();
                    if let Some(command) = self.rename_delete(ui, e) {
                        out = Some(command);
                    }
                });
                continue;
            }
            let selected = current.is_some_and(|c| same(c, &e.path));
            if e.missing {
                // Listed, but not on the disk: all that can be done is to
                // take it out of the project.
                let row = ui
                    .selectable_label(
                        false,
                        RichText::new(format!("⚠ {}", e.name))
                            .color(Color32::from_rgb(225, 70, 70)),
                    )
                    .on_hover_text("This file is in the project but not on the disk");
                row.context_menu(|ui| {
                    if ui.button("Remove from project").clicked() {
                        out = Some(Command::RemoveFromProject(e.path.clone()));
                        ui.close();
                    }
                });
                continue;
            }
            let openable = matches!(e.kind, EntryKind::Schematic | EntryKind::Source(_));
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
            let open = match e.kind {
                EntryKind::Schematic => Some(Command::OpenSchematic(e.path.clone())),
                EntryKind::Source(_) => Some(Command::OpenSource(e.path.clone())),
                _ => None,
            };
            if row.double_clicked()
                && let Some(command) = open.clone()
            {
                out = Some(command);
            }
            row.context_menu(|ui| {
                if let Some(command) = open.clone()
                    && ui.button("Open").clicked()
                {
                    out = Some(command);
                    ui.close();
                }
                if let Some(command) =
                    self.context_new(ui, e.path.parent().unwrap_or(Path::new("")))
                {
                    out = Some(command);
                }
                ui.separator();
                if let Some(command) = self.rename_delete(ui, e) {
                    out = Some(command);
                }
            });
        }
        out
    }

    /// The choices of something new in the folder `dir`.
    fn creates(&mut self, ui: &mut Ui, dir: &Path) -> Option<Command> {
        let mut out = None;
        for what in Create::ALL {
            if ui.button(what.menu_label()).clicked() {
                out = Some(Command::AskCreate {
                    dir: dir.to_path_buf(),
                    what,
                });
                ui.close();
            }
        }
        out
    }

    fn add_existing(&mut self, ui: &mut Ui) -> Option<Command> {
        let clicked = ui
            .button("Add existing file…")
            .on_hover_text("List files that are already on the disk as part of the project")
            .clicked();
        if clicked {
            ui.close();
        }
        clicked.then_some(Command::AddExisting)
    }

    /// The entries that add something to the project, in the folder `dir`:
    /// a new file or folder, or a file that exists already. For the Add button.
    fn new_menu(&mut self, ui: &mut Ui, dir: &Path) -> Option<Command> {
        let mut out = self.creates(ui, dir);
        ui.separator();
        if let Some(command) = self.add_existing(ui) {
            out = Some(command);
        }
        out
    }

    /// The same for a right-click menu: the choices of something new sit in
    /// a New submenu, as in the File menu.
    fn context_new(&mut self, ui: &mut Ui, dir: &Path) -> Option<Command> {
        let mut out = None;
        ui.menu_button("New", |ui| {
            if let Some(command) = self.creates(ui, dir) {
                out = Some(command);
            }
        });
        if let Some(command) = self.add_existing(ui) {
            out = Some(command);
        }
        out
    }

    /// The menu of the project's name and of empty space.
    fn root_menu(&mut self, ui: &mut Ui, root: &Path) -> Option<Command> {
        let mut out = self.context_new(ui, root);
        ui.separator();
        if ui.button("Close project").clicked() {
            out = Some(Command::CloseProject);
            ui.close();
        }
        out
    }

    /// Rename, take out of the project, and delete from the disk.
    fn rename_delete(&mut self, ui: &mut Ui, e: &Entry) -> Option<Command> {
        let mut out = None;
        if ui.button("Rename…").clicked() {
            out = Some(Command::AskRename(e.path.clone()));
            ui.close();
        }
        if ui
            .button("Remove from project")
            .on_hover_text("The file stays on the disk")
            .clicked()
        {
            out = Some(Command::RemoveFromProject(e.path.clone()));
            ui.close();
        }
        if ui.button("Delete from disk…").clicked() {
            out = Some(Command::AskDelete(e.path.clone()));
            ui.close();
        }
        out
    }
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
    Create { dir: PathBuf, what: Create },
    Rename { path: PathBuf },
}

impl NamePrompt {
    pub fn create(dir: &Path, what: Create) -> Self {
        NamePrompt {
            name: what.suggest(dir),
            kind: PromptKind::Create {
                dir: dir.to_path_buf(),
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
            PromptKind::Create { dir, what } => Some(Command::Create {
                dir: dir.clone(),
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
            PromptKind::Create { what, .. } => (what.title(), "Create"),
            PromptKind::Rename { .. } => ("Rename", "Rename"),
        };
        let heading = match &self.kind {
            PromptKind::Create { dir, what } => {
                let place = relative_to(&project.root, dir);
                let place = if place.is_empty() {
                    project.name()
                } else {
                    format!("{}/{place}", project.name())
                };
                let noun = match what {
                    Create::Folder => "folder",
                    _ => "file",
                };
                format!("Name of the new {noun}, in {place}:")
            }
            PromptKind::Rename { path } => format!("New name for “{}”:", file_name_of(path)),
        };
        let note = match &self.kind {
            PromptKind::Rename { .. } => {
                Some("Schematics that use it are updated to the new name.")
            }
            PromptKind::Create { .. } => None,
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
            recent: &recent,
            current: Some(&dir.join("a.dbr")),
            embedded: 2,
        };
        assert_eq!(draw(&mut panel, &view), None);

        // The name boxes draw, and a bad or unchanged name asks for nothing.
        for mut prompt in [
            NamePrompt::create(&dir, Create::Assembly),
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

    #[test]
    fn a_name_prompt_accepts_only_good_names() {
        let dir = PathBuf::from("proj");
        let mut p = NamePrompt::create(&dir, Create::Assembly);
        assert!(matches!(
            p.command(),
            Some(Command::Create {
                what: Create::Assembly,
                ..
            })
        ));
        p.name = "  led.asm  ".into();
        assert!(matches!(p.command(), Some(Command::Create { name, .. }) if name == "led.asm"));
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
        assert_eq!(Create::Folder.suggest(&dir), "New folder");
        for f in ["Circuit 1.dbr", "Circuit 2.dbr", "main.asm", "sketch.ino"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        std::fs::create_dir(dir.join("New folder")).unwrap();
        assert_eq!(Create::Schematic.suggest(&dir), "Circuit 3");
        assert_eq!(Create::Assembly.suggest(&dir), "main2");
        assert_eq!(Create::Sketch.suggest(&dir), "sketch2");
        assert_eq!(Create::Folder.suggest(&dir), "New folder 2");
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
