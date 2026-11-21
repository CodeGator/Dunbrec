//! The Project panel: the open project's folder as a tree of schematics and
//! source files, with the commands to make, rename and delete them. The panel
//! only asks; the app does the work (see `DunbrecApp::run_project_command`).

use std::path::{Path, PathBuf};

use egui::{Color32, RichText, Ui};

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
    const ALL: [Create; 4] = [
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
    fn menu_label(self) -> &'static str {
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
    Create {
        dir: PathBuf,
        name: String,
        what: Create,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    /// Take a file or folder out of the project, leaving it on the disk.
    RemoveFromProject(PathBuf),
    /// Delete a file or folder from the disk (and so from the project).
    Delete(PathBuf),
    /// Pick existing files to add to the project.
    AddExisting,
    Refresh,
    MoveSourcesToFiles,
    /// Take a project off the recent list.
    ForgetProject(PathBuf),
    ClearRecentProjects,
}

enum Form {
    Create {
        dir: PathBuf,
        what: Create,
        name: String,
        fresh: bool,
    },
    Rename {
        path: PathBuf,
        name: String,
        fresh: bool,
    },
    Delete {
        path: PathBuf,
    },
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
pub struct ProjectPanel {
    form: Option<Form>,
}

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
            self.form = None;
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
                    .small_button("✕")
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

        if let Some(command) = self.form_ui(ui, project) {
            out = Some(command);
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
                    if let Some(command) = self.new_menu(ui, &e.path) {
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
                if let Some(command) = self.rename_delete(ui, e) {
                    out = Some(command);
                }
            });
        }
        out
    }

    /// The entries that add something to the project, in the folder `dir`:
    /// a new file or folder, or a file that exists already.
    fn new_menu(&mut self, ui: &mut Ui, dir: &Path) -> Option<Command> {
        let mut out = None;
        for what in Create::ALL {
            if ui.button(what.menu_label()).clicked() {
                self.form = Some(Form::Create {
                    dir: dir.to_path_buf(),
                    what,
                    name: what.suggest(dir),
                    fresh: true,
                });
                ui.close();
            }
        }
        ui.separator();
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
    fn root_menu(&mut self, ui: &mut Ui, root: &Path) -> Option<Command> {
        let mut out = self.new_menu(ui, root);
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
            self.form = Some(Form::Rename {
                path: e.path.clone(),
                name: e.name.clone(),
                fresh: true,
            });
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
            self.form = Some(Form::Delete {
                path: e.path.clone(),
            });
            ui.close();
        }
        out
    }

    /// The small form for a name or a confirmation, above the tree.
    fn form_ui(&mut self, ui: &mut Ui, project: &Project) -> Option<Command> {
        let mut form = self.form.take()?;
        let mut out = None;
        let mut keep = true;
        egui::Frame::group(ui.style()).show(ui, |ui| match &mut form {
            Form::Create {
                dir,
                what,
                name,
                fresh,
            } => {
                let place = relative_to(&project.root, dir);
                let place = if place.is_empty() {
                    project.name()
                } else {
                    format!("{}/{place}", project.name())
                };
                ui.label(RichText::new(format!("{} in {place}", what.title())).strong());
                let ok = valid_name(name);
                let r = ui.add(
                    egui::TextEdit::singleline(name)
                        .hint_text(match what {
                            Create::Folder => "folder name",
                            _ => "file name",
                        })
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(fresh) {
                    select_all(ui, &r, name);
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if (ui.add_enabled(ok, egui::Button::new("Create")).clicked() || (enter && ok))
                        && ok
                    {
                        out = Some(Command::Create {
                            dir: dir.clone(),
                            name: name.trim().to_string(),
                            what: *what,
                        });
                        keep = false;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            }
            Form::Rename { path, name, fresh } => {
                let was = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.label(RichText::new(format!("Rename {was}")).strong());
                let ok = valid_name(name) && name.trim() != was;
                let r = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
                if std::mem::take(fresh) {
                    select_all(ui, &r, name);
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.label(
                    RichText::new("Schematics that use it are updated to the new name.")
                        .small()
                        .weak(),
                );
                ui.horizontal(|ui| {
                    if (ui.add_enabled(ok, egui::Button::new("Rename")).clicked() || (enter && ok))
                        && ok
                    {
                        out = Some(Command::Rename {
                            from: path.clone(),
                            to: path.with_file_name(name.trim()),
                        });
                        keep = false;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            }
            Form::Delete { path } => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let what = if path.is_dir() {
                    format!("Delete the folder “{name}” and everything in it?")
                } else {
                    format!("Delete “{name}”?")
                };
                ui.label(RichText::new(what).strong());
                ui.label(
                    RichText::new("This removes it from the disk and cannot be undone.")
                        .small()
                        .color(Color32::from_rgb(220, 150, 30)),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("Delete").color(Color32::from_rgb(225, 70, 70)))
                        .clicked()
                    {
                        out = Some(Command::Delete(path.clone()));
                        keep = false;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            }
        });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            keep = false;
        }
        if keep {
            self.form = Some(form);
        }
        ui.add_space(4.0);
        out
    }
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
                    .small_button("✕")
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
        let mut panel = ProjectPanel::default();
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

        // Each kind of form draws, and an invalid or unchanged name does
        // not ask for anything.
        for form in [
            Form::Create {
                dir: dir.clone(),
                what: Create::Assembly,
                name: "bad/name".into(),
                fresh: true,
            },
            Form::Rename {
                path: dir.join("a.dbr"),
                name: "a.dbr".into(),
                fresh: true,
            },
            Form::Delete {
                path: dir.join("fw"),
            },
        ] {
            panel.form = Some(form);
            assert_eq!(draw(&mut panel, &view), None);
            assert!(panel.form.is_some(), "the form stays open until answered");
        }
        std::fs::remove_dir_all(&dir).unwrap();
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
