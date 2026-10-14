//! Persisted settings, and the one owner of their current value.
//!
//! [`SettingsStore`] is the only holder of the current [`AppSettings`]; partial updates
//! are written as `AppSettings { field: v, ..store.current().clone() }` so fields not
//! named are carried over rather than reset. Neither loading nor saving panics or
//! propagates: a failed load degrades to defaults, a failed save returns `false`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::document::{self, DocumentFormat};
use crate::line_endings::LineEnding;
use crate::log;

/// Every bound a setting has, defined once.
pub mod limits {
    pub const MIN_FONT_SIZE: f64 = 8.0;
    pub const MAX_FONT_SIZE: f64 = 48.0;
    pub const CUSTOM_COLOR_COUNT: usize = 16;
    pub const RECENT_FOLDER_COUNT: usize = 3;

    pub fn clamp_font_size(v: f64) -> f64 {
        if v.is_nan() { MIN_FONT_SIZE } else { v.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub theme: Theme,
    pub editor_font_family: String,
    pub editor_font_size: f64,
    pub preview_font_family: String,
    pub preview_font_size: f64,
    /// `#RGB` / `#RRGGBB`, or `None` for the theme's default background.
    pub preview_background_color: Option<String>,
    pub recent_folders: Vec<String>,
    pub custom_colors: Vec<String>,
    pub table_header_gray_background: bool,
    pub table_row_striping: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            editor_font_family: "Cascadia Mono, Consolas".into(),
            editor_font_size: 14.0,
            preview_font_family: "Segoe UI, sans-serif".into(),
            preview_font_size: 15.0,
            preview_background_color: None,
            recent_folders: Vec::new(),
            custom_colors: Vec::new(),
            table_header_gray_background: false,
            table_row_striping: false,
        }
    }
}

/// `#RGB` or `#RRGGBB`. Checked both when loading (the file is writable from outside the
/// app) and right before the value is interpolated into CSS.
pub fn is_hex_color(s: &str) -> bool {
    let Some(hex) = s.strip_prefix('#') else { return false };
    (hex.len() == 3 || hex.len() == 6) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

impl AppSettings {
    /// Brings a value read from disk back inside the bounds the app relies on.
    fn sanitized(mut self) -> Self {
        self.editor_font_size = limits::clamp_font_size(self.editor_font_size);
        self.preview_font_size = limits::clamp_font_size(self.preview_font_size);
        if self.preview_background_color.as_deref().is_some_and(|c| !is_hex_color(c)) {
            self.preview_background_color = None;
        }
        self.custom_colors.retain(|c| is_hex_color(c));
        self.custom_colors.truncate(limits::CUSTOM_COLOR_COUNT);
        self.recent_folders.retain(|f| !f.is_empty());
        self.recent_folders.truncate(limits::RECENT_FOLDER_COUNT);
        self
    }
}

/// Puts `folder` first, dropping a case-insensitive duplicate and anything past the cap.
pub fn add_recent_folder(current: &[String], folder: &str) -> Vec<String> {
    std::iter::once(folder.to_string())
        .chain(current.iter().filter(|f| !f.eq_ignore_ascii_case(folder)).cloned())
        .take(limits::RECENT_FOLDER_COUNT)
        .collect()
}

pub fn remove_recent_folder(current: &[String], folder: &str) -> Vec<String> {
    current.iter().filter(|f| !f.eq_ignore_ascii_case(folder)).cloned().collect()
}

pub struct SettingsStore {
    path: Option<PathBuf>,
    current: AppSettings,
}

impl SettingsStore {
    /// Loads from `%APPDATA%\MyMDedit\settings.json`. Never fails: anything unreadable
    /// degrades to defaults and is recorded.
    pub fn load() -> Self {
        let path = std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("MyMDedit").join("settings.json"));
        let current = path.as_ref().map(|p| Self::read(p)).unwrap_or_default();
        Self { path, current }
    }

    fn read(path: &PathBuf) -> AppSettings {
        match std::fs::read_to_string(path) {
            Ok(json) => match serde_json::from_str::<AppSettings>(&json) {
                Ok(s) => s.sanitized(),
                Err(e) => {
                    log::error("SettingsStore.load parse", e);
                    AppSettings::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => AppSettings::default(),
            Err(e) => {
                log::error("SettingsStore.load", e);
                AppSettings::default()
            }
        }
    }

    pub fn current(&self) -> &AppSettings {
        &self.current
    }

    /// Makes `settings` current and writes it. `current` is replaced before the write,
    /// so what the screen shows and `current` agree even when the write fails.
    pub fn save(&mut self, settings: AppSettings) -> bool {
        self.current = settings.sanitized();
        let Some(path) = &self.path else {
            log::error("SettingsStore.save", "APPDATA is not set");
            return false;
        };
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let json = serde_json::to_string_pretty(&self.current).map_err(std::io::Error::other)?;
            let format = DocumentFormat { line_ending: LineEnding::Lf, ..DocumentFormat::default() };
            document::save_atomic(path, &json, &format)
        })();
        match result {
            Ok(()) => true,
            Err(e) => {
                log::error("SettingsStore.save", e);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors() {
        assert!(is_hex_color("#fff"));
        assert!(is_hex_color("#1E1E1E"));
        assert!(!is_hex_color("#fff; } body { display:none; } x {"));
        assert!(!is_hex_color("fff"));
        assert!(!is_hex_color("#ffff"));
    }

    #[test]
    fn recent_folders_dedupe_and_cap() {
        let cur = vec!["C:\\a".to_string(), "C:\\b".to_string(), "C:\\c".to_string()];
        assert_eq!(add_recent_folder(&cur, "c:\\B"), vec!["c:\\B", "C:\\a", "C:\\c"]);
        assert_eq!(add_recent_folder(&cur, "C:\\d"), vec!["C:\\d", "C:\\a", "C:\\b"]);
        assert_eq!(remove_recent_folder(&cur, "C:\\A"), vec!["C:\\b", "C:\\c"]);
    }

    #[test]
    fn sanitizes_loaded_values() {
        let s: AppSettings =
            serde_json::from_str(r#"{"editor_font_size": 200, "preview_background_color": "red"}"#).unwrap();
        let s = s.sanitized();
        assert_eq!(s.editor_font_size, 48.0);
        assert_eq!(s.preview_background_color, None);
        assert_eq!(s.preview_font_family, "Segoe UI, sans-serif");
    }
}
