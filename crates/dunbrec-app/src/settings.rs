//! User settings kept between sessions (via eframe's storage).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const STORAGE_KEY: &str = "dunbrec.settings";
/// Most recent files remembered.
const RECENT_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn preference(self) -> egui::ThemePreference {
        match self {
            Theme::System => egui::ThemePreference::System,
            Theme::Light => egui::ThemePreference::Light,
            Theme::Dark => egui::ThemePreference::Dark,
        }
    }
}

/// How large the whole interface is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TextSize {
    #[default]
    Normal,
    Large,
    ExtraLarge,
    Huge,
}

impl TextSize {
    pub const ALL: [TextSize; 4] = [
        TextSize::Normal,
        TextSize::Large,
        TextSize::ExtraLarge,
        TextSize::Huge,
    ];

    /// What everything is multiplied by.
    pub fn factor(self) -> f32 {
        match self {
            TextSize::Normal => 1.0,
            TextSize::Large => 1.25,
            TextSize::ExtraLarge => 1.5,
            TextSize::Huge => 2.0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TextSize::Normal => "Normal",
            TextSize::Large => "Large",
            TextSize::ExtraLarge => "Extra large",
            TextSize::Huge => "Huge",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub text_size: TextSize,
    /// Most recent first.
    pub recent: Vec<PathBuf>,
    /// Project folders, most recent first.
    pub recent_projects: Vec<PathBuf>,
    /// The project that was open when the app last closed.
    pub project: Option<PathBuf>,
    /// The language model behind the Assistant panel.
    pub ai: crate::ai::AiSettings,
}

impl Settings {
    /// Put `path` at the top of the recent-files list.
    pub fn remember(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(RECENT_LIMIT);
    }

    pub fn forget(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
    }

    /// Put the project folder `path` at the top of the recent projects.
    pub fn remember_project(&mut self, path: &Path) {
        self.recent_projects.retain(|p| p != path);
        self.recent_projects.insert(0, path.to_path_buf());
        self.recent_projects.truncate(RECENT_LIMIT);
        self.project = Some(path.to_path_buf());
    }

    /// Empty the recent-files list.
    pub fn clear_recent(&mut self) {
        self.recent.clear();
    }

    /// Empty the recent-projects list (the open project stays open).
    pub fn clear_recent_projects(&mut self) {
        self.recent_projects.clear();
    }

    pub fn forget_project(&mut self, path: &Path) {
        self.recent_projects.retain(|p| p != path);
        if self.project.as_deref() == Some(path) {
            self.project = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_files_are_unique_and_limited() {
        let mut s = Settings::default();
        for i in 0..12 {
            s.remember(Path::new(&format!("f{i}.dbr")));
        }
        s.remember(Path::new("f5.dbr"));
        assert_eq!(s.recent.len(), RECENT_LIMIT);
        assert_eq!(s.recent[0], PathBuf::from("f5.dbr"));
        assert_eq!(
            s.recent
                .iter()
                .filter(|p| p.as_path() == Path::new("f5.dbr"))
                .count(),
            1
        );
        s.forget(Path::new("f5.dbr"));
        assert!(!s.recent.contains(&PathBuf::from("f5.dbr")));
    }

    #[test]
    fn recent_projects_are_remembered_and_forgotten() {
        let mut s = Settings::default();
        s.remember_project(Path::new("a"));
        s.remember_project(Path::new("b"));
        s.remember_project(Path::new("a"));
        assert_eq!(s.recent_projects, [PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(s.project, Some(PathBuf::from("a")));
        s.forget_project(Path::new("a"));
        assert_eq!(s.recent_projects, [PathBuf::from("b")]);
        assert_eq!(s.project, None);
        s.remember_project(Path::new("c"));
        s.clear_recent_projects();
        assert!(s.recent_projects.is_empty());
        s.remember(Path::new("f.dbr"));
        s.clear_recent();
        assert!(s.recent.is_empty());
    }

    #[test]
    fn old_or_partial_settings_still_load() {
        let s: Settings = ron::from_str("(theme: Dark)").unwrap();
        assert_eq!(s.theme, Theme::Dark);
        assert_eq!(s.text_size, TextSize::Normal);
        assert!(s.recent.is_empty());
    }

    #[test]
    fn text_sizes_grow_in_steps() {
        let factors: Vec<f32> = TextSize::ALL.iter().map(|t| t.factor()).collect();
        assert_eq!(factors[0], 1.0);
        assert!(factors.windows(2).all(|w| w[0] < w[1]));
        let s: Settings = ron::from_str("(text_size: ExtraLarge)").unwrap();
        assert_eq!(s.text_size, TextSize::ExtraLarge);
    }
}
