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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// Most recent first.
    pub recent: Vec<PathBuf>,
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
    fn old_or_partial_settings_still_load() {
        let s: Settings = ron::from_str("(theme: Dark)").unwrap();
        assert_eq!(s.theme, Theme::Dark);
        assert!(s.recent.is_empty());
    }
}
