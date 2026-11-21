//! The Project panel: the open project's folder as a tree of schematics and
//! source files, with the commands to make, rename and delete them. The panel
//! only asks; the app does the work (see `DunbrecApp::run_project_command`).

use std::path::{Path, PathBuf};

use egui::{Color32, RichText, Ui};

use crate::project::{Entry, EntryKind, Project, normalize, relative_to, valid_name};

/// What a new item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Create {
    Schematic,
    Assembly,
    Sketch,
    Folder,
}

impl Create {
    fn title(self) -> &'static str {
        match self {
            Create::Schematic => "New schematic",
            Create::Assembly => "New assembly file",
            Create::Sketch => "New Arduino sketch",
            Create::Folder => "New folder",
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
    Delete(PathBuf),
    Refresh,
    MoveSourcesToFiles,
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
    normalize(a) == normalize(b)
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
            ui.label(RichText::new(format!("📁 {}", project.name())).strong())
                .on_hover_text(project.root.display().to_string());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("✕")
                    .on_hover_text("Close the project (its files stay where they are)")
                    .clicked()
                {
                    out = Some(Command::CloseProject);
                }
                if ui
                    .small_button("⟳")
                    .on_hover_text("Look at the folder again")
                    .clicked()
                {
                    out = Some(Command::Refresh);
                }
                ui.menu_button("＋ New", |ui| {
                    for what in [
                        Create::Schematic,
                        Create::Assembly,
                        Create::Sketch,
                        Create::Folder,
                    ] {
                        let label = match what {
                            Create::Schematic => "Schematic…",
                            Create::Assembly => "Assembly source file…",
                            Create::Sketch => "Arduino sketch…",
                            Create::Folder => "Folder…",
                        };
                        if ui.button(label).clicked() {
                            self.form = Some(Form::Create {
                                dir: project.root.clone(),
                                what,
                                name: String::new(),
                                fresh: true,
                            });
                            ui.close();
                        }
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

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if project.entries.is_empty() {
                    ui.label(RichText::new("This folder is empty.").weak());
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
                    for what in [
                        Create::Schematic,
                        Create::Assembly,
                        Create::Sketch,
                        Create::Folder,
                    ] {
                        if ui.button(format!("{} here…", what.title())).clicked() {
                            self.form = Some(Form::Create {
                                dir: e.path.clone(),
                                what,
                                name: String::new(),
                                fresh: true,
                            });
                            ui.close();
                        }
                    }
                    ui.separator();
                    self.rename_delete(ui, e);
                });
                continue;
            }
            let selected = current.is_some_and(|c| same(c, &e.path));
            let openable = matches!(e.kind, EntryKind::Schematic | EntryKind::Source(_));
            let mut label = RichText::new(format!("{} {}", icon(e.kind), e.name));
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
                self.rename_delete(ui, e);
            });
        }
        out
    }

    fn rename_delete(&mut self, ui: &mut Ui, e: &Entry) {
        if ui.button("Rename…").clicked() {
            self.form = Some(Form::Rename {
                path: e.path.clone(),
                name: e.name.clone(),
                fresh: true,
            });
            ui.close();
        }
        if ui.button("Delete…").clicked() {
            self.form = Some(Form::Delete {
                path: e.path.clone(),
            });
            ui.close();
        }
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
                    r.request_focus();
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
                    r.request_focus();
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

/// What the panel shows with no project open.
fn no_project(ui: &mut Ui, recent: &[PathBuf]) -> Option<Command> {
    let mut out = None;
    ui.add_space(8.0);
    ui.label(RichText::new("No project open").strong());
    ui.label(
        RichText::new(
            "A project is a folder that keeps a design's schematics and firmware source \
             files together.",
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
        ui.label(RichText::new("Recent projects").strong());
        for path in recent {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            if ui
                .button(format!("📁 {name}"))
                .on_hover_text(path.display().to_string())
                .clicked()
            {
                out = Some(Command::OpenProjectPath(path.clone()));
            }
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
        let project = Project::open(&dir).unwrap();
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
