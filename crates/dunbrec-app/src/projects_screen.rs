//! The Projects screen: everything that is shown while no project is open.
//!
//! Every piece of work in Dunbrec belongs to a project, so with none open
//! this is the whole window: make a project, open one, and look after the list of recent
//! projects (open, rename, remove one, or clear the list).

use std::path::{Path, PathBuf};

use egui::{Color32, RichText, Ui};

use crate::dialog::{self, Dialog};
use crate::project;
use crate::project_ui::select_all;

/// Widest the screen's content gets (points).
const WIDTH: f32 = 640.0;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    NewProject,
    OpenProject,
    OpenRecent(PathBuf),
    /// Take one project out of the recent list (nothing on disk changes).
    Forget(PathBuf),
    ClearRecent,
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    /// Show the project's folder in the system's file browser.
    ShowFolder(PathBuf),
    Settings,
    /// Close the program.
    Quit,
    /// Make a project for the circuit file that was waiting for one.
    ProjectForWaiting,
    DismissWaiting,
}

/// What the screen shows.
pub struct View<'a> {
    pub recent: &'a [PathBuf],
    /// A circuit file the program was started with, which belongs to no
    /// project yet.
    pub waiting: Option<&'a Path>,
}

/// Projects on one page of the list.
const PAGE_SIZE: usize = 10;

/// How the list of projects is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Sort {
    /// The order they were opened in, newest first.
    #[default]
    Recent,
    NameAz,
    NameZa,
    /// By the folder they are in, then by name.
    Folder,
}

impl Sort {
    const ALL: [Sort; 4] = [Sort::Recent, Sort::NameAz, Sort::NameZa, Sort::Folder];

    fn label(self) -> &'static str {
        match self {
            Sort::Recent => "Most recently opened",
            Sort::NameAz => "Name, A to Z",
            Sort::NameZa => "Name, Z to A",
            Sort::Folder => "Folder",
        }
    }
}

/// Which of the projects are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Filter {
    #[default]
    All,
    /// Projects whose file is there.
    Available,
    /// Projects whose file has gone.
    Missing,
}

impl Filter {
    const ALL: [Filter; 3] = [Filter::All, Filter::Available, Filter::Missing];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All projects",
            Filter::Available => "Available",
            Filter::Missing => "File missing",
        }
    }
}

#[derive(Default)]
pub struct ProjectsScreen {
    rename: Option<Rename>,
    query: String,
    sort: Sort,
    filter: Filter,
    /// The page of the list that is shown, from 0.
    page: usize,
}

/// The projects to show, in order: those that match every word of `query`
/// (in the name or the folder, any case) and the `filter`, sorted.
fn arrange(
    recent: &[PathBuf],
    query: &str,
    sort: Sort,
    filter: Filter,
    exists: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let folder_of = |p: &Path| p.parent().unwrap_or(Path::new("")).display().to_string();
    let mut out: Vec<PathBuf> = recent
        .iter()
        .filter(|p| {
            let text = format!("{} {}", stem_of(p), folder_of(p)).to_lowercase();
            words.iter().all(|w| text.contains(w))
        })
        .filter(|p| match filter {
            Filter::All => true,
            Filter::Available => exists(p),
            Filter::Missing => !exists(p),
        })
        .cloned()
        .collect();
    let name = |p: &PathBuf| stem_of(p).to_lowercase();
    match sort {
        Sort::Recent => {}
        Sort::NameAz => out.sort_by_key(|p| (name(p), p.clone())),
        Sort::NameZa => out.sort_by_key(|p| (std::cmp::Reverse(name(p)), p.clone())),
        Sort::Folder => out.sort_by_key(|p| (folder_of(p).to_lowercase(), name(p))),
    }
    out
}

/// How many pages `count` projects make (at least one).
fn pages(count: usize) -> usize {
    count.div_ceil(PAGE_SIZE).max(1)
}

/// The box asking for a project's new name.
struct Rename {
    file: PathBuf,
    name: String,
    fresh: bool,
}

fn stem_of(path: &Path) -> String {
    path.file_stem().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

impl ProjectsScreen {
    /// Draw the screen; the command the user chose this frame, if any.
    pub fn show(&mut self, ctx: &egui::Context, view: &View) -> Option<Command> {
        let mut out = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let width = ui.available_width().min(WIDTH);
                    let side = ((ui.available_width() - width) / 2.0).max(0.0);
                    ui.horizontal_top(|ui| {
                        ui.add_space(side);
                        ui.vertical(|ui| {
                            ui.set_width(width);
                            if let Some(command) = self.content(ui, view, width) {
                                out = Some(command);
                            }
                        });
                    });
                });
        });
        if let Some(command) = self.rename_box(ctx) {
            out = Some(command);
        }
        out
    }

    fn content(&mut self, ui: &mut Ui, view: &View, width: f32) -> Option<Command> {
        let mut out = None;
        ui.add_space(36.0);
        ui.label(RichText::new("Dunbrec").size(36.0).strong());
        ui.label(
            RichText::new(format!(
                "Circuit simulator and schematic editor  ·  version {}",
                env!("CARGO_PKG_VERSION")
            ))
            .weak(),
        );
        ui.add_space(20.0);

        if let Some(circuit) = view.waiting {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(width - 20.0);
                ui.label(RichText::new(format!("“{}”", stem_of(circuit))).strong());
                ui.label(
                    "is a circuit file that is not in a project yet. Everything in Dunbrec \
                     belongs to a project, so make one for it: the circuit file stays where it is.",
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Make a project for it…").clicked() {
                        out = Some(Command::ProjectForWaiting);
                    }
                    if ui.button("Not now").clicked() {
                        out = Some(Command::DismissWaiting);
                    }
                });
            });
            ui.add_space(14.0);
        }

        ui.horizontal(|ui| {
            let big = |text: &str| egui::Button::new(RichText::new(text).size(15.0));
            let size = egui::vec2(150.0, 34.0);
            if ui.add(big("New project…").min_size(size)).clicked() {
                out = Some(Command::NewProject);
            }
            if ui.add(big("Open project…").min_size(size)).clicked() {
                out = Some(Command::OpenProject);
            }
        });
        ui.add_space(24.0);

        ui.horizontal(|ui| {
            ui.label(RichText::new("Recent projects").size(18.0).strong());
            ui.add_enabled_ui(!view.recent.is_empty(), |ui| {
                if ui
                    .small_button("Clear list")
                    .on_hover_text("Forget every project in the list (no file is touched)")
                    .clicked()
                {
                    out = Some(Command::ClearRecent);
                }
            });
        });
        ui.add_space(6.0);
        if view.recent.is_empty() {
            ui.label(RichText::new("No projects yet. Make one, or open one, to begin.").weak());
        } else {
            self.list(ui, view, width, &mut out);
        }
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            if ui.small_button("Settings…").clicked() {
                out = Some(Command::Settings);
            }
            if ui
                .small_button("Quit")
                .on_hover_text("Close Dunbrec")
                .clicked()
            {
                out = Some(Command::Quit);
            }
        });
        out
    }

    /// Search, sort and filter controls, one page of projects, and the pager.
    fn list(&mut self, ui: &mut Ui, view: &View, width: f32, out: &mut Option<Command>) {
        let before = (self.query.clone(), self.sort, self.filter);
        if view.recent.len() > 1 {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search by name or folder")
                        .desired_width(width * 0.42),
                );
                egui::ComboBox::from_id_salt("project_sort")
                    .selected_text(self.sort.label())
                    .show_ui(ui, |ui| {
                        for s in Sort::ALL {
                            ui.selectable_value(&mut self.sort, s, s.label());
                        }
                    });
                egui::ComboBox::from_id_salt("project_filter")
                    .selected_text(self.filter.label())
                    .show_ui(ui, |ui| {
                        for f in Filter::ALL {
                            ui.selectable_value(&mut self.filter, f, f.label());
                        }
                    });
            });
            ui.add_space(6.0);
        }
        // A new search, sort or filter starts again at the first page.
        if before != (self.query.clone(), self.sort, self.filter) {
            self.page = 0;
        }

        let shown = arrange(view.recent, &self.query, self.sort, self.filter, |p| {
            p.is_file()
        });
        if shown.is_empty() {
            ui.label(RichText::new("No projects match.").weak());
            return;
        }
        // Removing projects can leave the page that was shown without any.
        self.page = self.page.min(pages(shown.len()) - 1);
        let start = self.page * PAGE_SIZE;
        for path in shown.iter().skip(start).take(PAGE_SIZE) {
            if let Some(command) = self.recent_row(ui, path, width) {
                *out = Some(command);
            }
        }

        let count = pages(shown.len());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if count > 1 {
                if ui
                    .add_enabled(self.page > 0, egui::Button::new("‹ Previous"))
                    .clicked()
                {
                    self.page -= 1;
                }
                ui.label(format!("Page {} of {count}", self.page + 1));
                if ui
                    .add_enabled(self.page + 1 < count, egui::Button::new("Next ›"))
                    .clicked()
                {
                    self.page += 1;
                }
                ui.add_space(8.0);
            }
            if shown.len() != view.recent.len() {
                ui.label(
                    RichText::new(format!("{} of {} projects", shown.len(), view.recent.len()))
                        .weak(),
                );
            }
        });
    }

    fn recent_row(&mut self, ui: &mut Ui, path: &Path, width: f32) -> Option<Command> {
        let mut out = None;
        let exists = path.is_file();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(width - 20.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    let name = RichText::new(stem_of(path)).size(17.0).strong();
                    if ui
                        .add_enabled(exists, egui::Button::new(name).frame(false))
                        .on_hover_text(path.display().to_string())
                        .clicked()
                    {
                        out = Some(Command::OpenRecent(path.to_path_buf()));
                    }
                    let folder = path.parent().unwrap_or(Path::new("")).display().to_string();
                    ui.label(RichText::new(folder).weak().small());
                    if !exists {
                        ui.colored_label(
                            Color32::from_rgb(225, 90, 90),
                            "The project file is not there any more.",
                        );
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("×")
                        .on_hover_text("Remove from the list (no file is touched)")
                        .clicked()
                    {
                        out = Some(Command::Forget(path.to_path_buf()));
                    }
                    ui.add_enabled_ui(exists, |ui| {
                        if ui
                            .small_button("Rename…")
                            .on_hover_text(
                                "Rename the project (its circuit files keep their names)",
                            )
                            .clicked()
                        {
                            self.rename = Some(Rename {
                                file: path.to_path_buf(),
                                name: stem_of(path),
                                fresh: true,
                            });
                        }
                        if ui.small_button("Show folder").clicked() {
                            out = Some(Command::ShowFolder(path.to_path_buf()));
                        }
                    });
                });
            });
        });
        out
    }

    /// The box asking for a project's new name.
    fn rename_box(&mut self, ctx: &egui::Context) -> Option<Command> {
        let rename = self.rename.as_mut()?;
        let current = stem_of(&rename.file);
        let (name, fresh) = (&mut rename.name, &mut rename.fresh);
        let answer = Dialog {
            id: "rename_project",
            kind: dialog::Kind::Question,
            title: "Rename project",
            buttons: &["Rename", "Cancel"],
            default: 0,
            cancel: Some(1),
        }
        .show(ctx, |ui| {
            dialog::paragraph(ui, &format!("New name for the project “{current}”:"));
            ui.add_space(6.0);
            let r = ui.add(
                egui::TextEdit::singleline(name)
                    .font(egui::TextStyle::Heading)
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(fresh) {
                select_all(ui, &r, name);
            }
            if !name.trim().is_empty() && !project::valid_name(name) {
                ui.add_space(4.0);
                ui.colored_label(
                    Color32::from_rgb(225, 70, 70),
                    "That is not a name a file can have.",
                );
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new("Only the project is renamed; its circuit files are not touched.")
                    .weak(),
            );
        });
        match answer {
            Some(0) => {
                let wanted = rename.name.trim().to_string();
                if !project::valid_name(&wanted) {
                    return None; // keep the box open until the name is usable
                }
                let from = rename.file.clone();
                self.rename = None;
                (wanted != current).then(|| Command::Rename {
                    to: from.with_file_name(project::with_extension(&wanted, project::EXTENSION)),
                    from,
                })
            }
            Some(_) => {
                self.rename = None;
                None
            }
            None => None,
        }
    }
}

/// Show `path`'s folder in the system's file browser.
pub fn show_in_folder(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let result = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .spawn();
    result.map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn stems(list: &[PathBuf]) -> Vec<String> {
        list.iter().map(|p| stem_of(p)).collect()
    }

    #[test]
    fn the_list_is_searched_by_every_word_in_name_or_folder() {
        let all = paths(&[
            "C:/Work/Amp.dbrproj",
            "C:/Home/Radio.dbrproj",
            "C:/Work/Radio.dbrproj",
        ]);
        let find = |q: &str| arrange(&all, q, Sort::Recent, Filter::All, |_| true);
        assert_eq!(find("").len(), 3);
        assert_eq!(find("radio").len(), 2);
        assert_eq!(find("RADIO work"), paths(&["C:/Work/Radio.dbrproj"]));
        assert_eq!(find("amp").len(), 1);
        assert!(find("nothing").is_empty());
    }

    #[test]
    fn the_list_is_sorted_by_name_or_folder_or_left_as_opened() {
        let all = paths(&[
            "C:/B/zeta.dbrproj",
            "C:/A/Mid.dbrproj",
            "C:/C/alpha.dbrproj",
        ]);
        let sorted = |s| stems(&arrange(&all, "", s, Filter::All, |_| true));
        assert_eq!(sorted(Sort::Recent), ["zeta", "Mid", "alpha"]);
        assert_eq!(sorted(Sort::NameAz), ["alpha", "Mid", "zeta"]);
        assert_eq!(sorted(Sort::NameZa), ["zeta", "Mid", "alpha"]);
        assert_eq!(sorted(Sort::Folder), ["Mid", "zeta", "alpha"]);
    }

    #[test]
    fn the_list_is_filtered_by_whether_the_file_is_there() {
        let all = paths(&["a.dbrproj", "b.dbrproj", "c.dbrproj"]);
        let there = |p: &Path| p != Path::new("b.dbrproj");
        let show = |f| stems(&arrange(&all, "", Sort::Recent, f, there));
        assert_eq!(show(Filter::All), ["a", "b", "c"]);
        assert_eq!(show(Filter::Available), ["a", "c"]);
        assert_eq!(show(Filter::Missing), ["b"]);
    }

    #[test]
    fn projects_are_paged_ten_to_a_page() {
        assert_eq!(pages(0), 1);
        assert_eq!(pages(10), 1);
        assert_eq!(pages(11), 2);
        assert_eq!(pages(50), 5);
    }
}
