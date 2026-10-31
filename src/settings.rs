//! Persisted settings, and the one owner of their current value.
//!
//! [`SettingsStore`] is the only holder of the current [`AppSettings`]; partial updates
//! are written as `AppSettings { field: v, ..store.current().clone() }` so fields not
//! named are carried over rather than reset. Neither loading nor saving panics or
//! propagates: a failed load degrades to defaults, a failed save returns the logged `Err`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

use crate::document::{self, DocumentFormat};
use crate::line_endings::LineEnding;
use crate::log;

/// Every bound a setting has, defined once.
pub mod limits {
    pub const MIN_FONT_SIZE: f64 = 8.0;
    pub const MAX_FONT_SIZE: f64 = 48.0;
    pub const CUSTOM_COLOR_COUNT: usize = 16;
    pub const RECENT_FOLDER_COUNT: usize = 3;
    /// A real settings file is well under 4KB. The file is writable from outside the
    /// app, and one too big to read would abort every launch, not just this one.
    pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

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
    let Some(hex) = s.strip_prefix('#') else {
        return false;
    };
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

/// Whether two settings would look different on screen. Recent folders and the custom
/// color palette are bookkeeping: nothing has to be redrawn when only they change.
pub fn display_differs(a: &AppSettings, b: &AppSettings) -> bool {
    let strip = |s: &AppSettings| AppSettings { recent_folders: Vec::new(), custom_colors: Vec::new(), ..s.clone() };
    strip(a) != strip(b)
}

/// Puts `folder` first, dropping a case-insensitive duplicate and anything past the cap.
pub fn add_recent_folder(current: &[String], folder: &str) -> Vec<String> {
    std::iter::once(folder.to_string())
        .chain(current.iter().filter(|f| !same_folder(f, folder)).cloned())
        .take(limits::RECENT_FOLDER_COUNT)
        .collect()
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn remove_recent_folder(current: &[String], folder: &str) -> Vec<String> {
    current.iter().filter(|f| !same_folder(f, folder)).cloned().collect()
}

/// The settings file's own on-disk form, decided here rather than borrowed from the
/// document defaults (which are free to change for documents' sake).
const SETTINGS_FILE_FORMAT: DocumentFormat =
    DocumentFormat { charset: document::Charset::Utf8, has_bom: false, line_ending: LineEnding::Lf };

/// What [`parse_settings`] made of a settings file.
#[derive(Debug)]
pub enum Parsed {
    /// Every field read cleanly.
    Clean(AppSettings),
    /// Readable as a whole, but these fields were invalid and fell back to defaults.
    Partial(AppSettings, Vec<String>),
    /// Not a JSON object at all.
    Unreadable(String),
}

/// Reads settings field by field, so one bad value (a misspelt theme, a string where a
/// number belongs) costs only that field instead of every setting.
pub fn parse_settings(text: &str) -> Parsed {
    // Editors that save "UTF-8" with a BOM are common; JSON parsers reject it.
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let object = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(o)) => o,
        Ok(_) => {
            return Parsed::Unreadable("設定ファイルの中身が JSON オブジェクトではありません".into());
        }
        Err(e) => return Parsed::Unreadable(e.to_string()),
    };
    let mut merged = match serde_json::to_value(AppSettings::default()) {
        Ok(serde_json::Value::Object(o)) => o,
        _ => return Parsed::Unreadable("既定の設定を組み立てられませんでした".into()),
    };
    let mut rejected = Vec::new();
    for (key, value) in object {
        if !merged.contains_key(&key) {
            continue; // Unknown keys (e.g. from a newer version) are ignored, not errors.
        }
        let mut candidate = merged.clone();
        candidate.insert(key.clone(), value);
        if serde_json::from_value::<AppSettings>(serde_json::Value::Object(candidate.clone())).is_ok() {
            merged = candidate;
        } else {
            rejected.push(key);
        }
    }
    let raw = serde_json::from_value::<AppSettings>(serde_json::Value::Object(merged)).unwrap_or_default();
    let settings = raw.clone().sanitized();
    // Values the app cannot use ("red" as a color, a size of 200) are corrected, and the
    // correction is reported like a rejected field: the next save drops the original value.
    if let (Ok(serde_json::Value::Object(before)), Ok(serde_json::Value::Object(after))) =
        (serde_json::to_value(&raw), serde_json::to_value(&settings))
    {
        for (key, value) in &before {
            if after.get(key) != Some(value) && !rejected.contains(key) {
                rejected.push(key.clone());
            }
        }
    }
    if rejected.is_empty() { Parsed::Clean(settings) } else { Parsed::Partial(settings, rejected) }
}

pub struct SettingsStore {
    path: Option<PathBuf>,
    current: AppSettings,
    /// False when the file on disk could not be read: writing would replace settings
    /// we never saw, so this session leaves the file alone.
    writable: bool,
    /// Something the user should be told at startup about how the settings were loaded.
    load_warning: Option<String>,
}

impl SettingsStore {
    /// Loads from `%APPDATA%\MyMDedit\settings.json`. Never fails: anything unreadable
    /// degrades to defaults — but never silently, and never by later overwriting a file
    /// that held more than what was loaded. The one deliberate exception is keys this
    /// version does not know: they are ignored and gone after the next save
    /// (docs/spec.md, 設定の読み込み).
    pub fn load() -> Self {
        Self::open(std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("MyMDedit").join("settings.json")))
    }

    pub fn open(path: Option<PathBuf>) -> Self {
        let mut store =
            Self { path: path.clone(), current: AppSettings::default(), writable: true, load_warning: None };
        if let Some(path) = path {
            store.read(&path);
        }
        store
    }

    fn read(&mut self, path: &Path) {
        let parsed = match read_disk(path) {
            Ok(Some(parsed)) => parsed,
            Ok(None) => return,
            Err(e) => {
                // Probably transient (a lock, a permission glitch). The file may be
                // perfectly good, so it must not be overwritten with defaults.
                log::error("SettingsStore.load", &e);
                self.writable = false;
                self.load_warning = Some(format!(
                    "設定ファイルを読み込めなかったため、既定の設定で起動しました。元のファイルを守るため、この起動中は設定を保存しません。({e})"
                ));
                return;
            }
        };
        let (settings, problem) = match parsed {
            Parsed::Clean(s) => (s, None),
            Parsed::Partial(s, fields) => {
                (s, Some(format!("一部の項目（{}）を読み込めなかったため、既定値を使います。", fields.join(", "))))
            }
            Parsed::Unreadable(reason) => {
                log::error("SettingsStore.load parse", &reason);
                (
                    AppSettings::default(),
                    Some("設定ファイルを読み込めなかったため、既定の設定で起動しました。".to_string()),
                )
            }
        };
        self.current = settings;
        if let Some(problem) = problem {
            // The next save will drop whatever could not be read, so keep the original.
            self.load_warning = Some(match back_up(path) {
                Ok(backup) => format!("{problem}元のファイルは {} に保存してあります。", backup.display()),
                Err(e) => {
                    log::error("SettingsStore.load backup", &e);
                    self.writable = false;
                    format!("{problem}元のファイルを退避できなかったため、この起動中は設定を保存しません。")
                }
            });
        }
    }

    pub fn current(&self) -> &AppSettings {
        &self.current
    }

    /// Takes the startup warning, if any, for the caller to show once a UI exists.
    pub fn take_load_warning(&mut self) -> Option<String> {
        self.load_warning.take()
    }

    /// Applies `change` to the settings as they are on disk *now* and writes the result,
    /// so another running instance's changes to other fields survive. `current` becomes
    /// the changed (and sanitized) value even when nothing is written, so the screen and
    /// `current` agree. A file that is now partly or wholly unreadable is backed up first,
    /// as at startup; one that cannot be read at all, or backed up, is left alone. The
    /// error is the reason nothing was written, already logged.
    pub fn update(&mut self, change: impl FnOnce(&mut AppSettings)) -> std::io::Result<()> {
        let result = self.write_with(change);
        if let Err(e) = &result {
            log::error("SettingsStore.update", e);
        }
        result
    }

    fn write_with(&mut self, change: impl FnOnce(&mut AppSettings)) -> std::io::Result<()> {
        let apply = |mut base: AppSettings| {
            change(&mut base);
            base.sanitized()
        };
        let Some(path) = self.path.clone() else {
            self.current = apply(self.current.clone());
            return Err(std::io::Error::other("APPDATA is not set"));
        };
        if !self.writable {
            self.current = apply(self.current.clone());
            return Err(std::io::Error::other("skipped: the settings file could not be read at startup"));
        }
        let _lock = SettingsLock::acquire();
        let base = match read_disk(&path) {
            Ok(None) => self.current.clone(),
            Ok(Some(Parsed::Clean(s))) => s,
            // What could be read is newer than our view; the rest is about to be dropped.
            Ok(Some(Parsed::Partial(s, _))) => match back_up(&path) {
                Ok(_) => s,
                Err(e) => {
                    self.current = apply(self.current.clone());
                    return Err(e);
                }
            },
            // Our own view is the best information there is, but only once the file that
            // replaced it is safe.
            Ok(Some(Parsed::Unreadable(reason))) => {
                log::error("SettingsStore.update parse", &reason);
                if let Err(e) = back_up(&path) {
                    self.current = apply(self.current.clone());
                    return Err(e);
                }
                self.current.clone()
            }
            // Probably transient; the file may be fine and hold changes we never saw.
            Err(e) => {
                self.current = apply(self.current.clone());
                return Err(e);
            }
        };
        self.current = apply(base);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(&self.current).map_err(std::io::Error::other)?;
        document::save_atomic(&path, &json, &SETTINGS_FILE_FORMAT)
    }
}

const BACKUP_PREFIX: &str = "settings.broken-";

/// Copies a settings file that is about to lose fields to `settings.broken-<time>.json`
/// beside it, unless a backup there already holds exactly this file; the path of the copy
/// that has it either way. Checking the backups on disk (not a memory of the last one)
/// is what keeps a file left broken from gaining a new backup on every launch.
///
/// Streams rather than reads: the file may be far over `limits::MAX_FILE_BYTES` (that is
/// one way to land here), and reading it whole could exhaust memory, which aborts the
/// process with no crash log, on every launch.
fn back_up(path: &Path) -> std::io::Result<PathBuf> {
    use std::io::Seek;
    let mut source = document::open_disk_file(path)?;
    let len = source.metadata()?.len();
    if let Some(at) = existing_backup(path, &mut source, len) {
        return Ok(at);
    }
    source.rewind()?;
    write_backup(path, &mut source)
}

/// A backup beside `path` with the same contents as `source`, if there is one. Errors
/// only cost a duplicate backup, so they are logged and skipped.
fn existing_backup(path: &Path, source: &mut std::fs::File, len: u64) -> Option<PathBuf> {
    use std::io::Seek;
    let entries = std::fs::read_dir(path.parent()?).inspect_err(|e| log::error("settings backup scan", e)).ok()?;
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(BACKUP_PREFIX) || !name.ends_with(".json") {
            continue;
        }
        if !entry.metadata().is_ok_and(|m| m.is_file() && m.len() == len) {
            continue;
        }
        let same = (|| {
            source.rewind()?;
            same_contents(source, &mut document::open_disk_file(&entry.path())?)
        })();
        match same {
            Ok(true) => return Some(entry.path()),
            Ok(false) => {}
            Err(e) => log::error("settings backup compare", &e),
        }
    }
    None
}

fn same_contents(a: &mut impl std::io::Read, b: &mut impl std::io::Read) -> std::io::Result<bool> {
    let (mut x, mut y) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let (n, m) = (fill(a, &mut x)?, fill(b, &mut y)?);
        if x[..n] != y[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Reads until `buf` is full or the input ends; how much was read.
fn fill(r: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// A name already taken (two backups within one second) gets a counter instead of
/// overwriting the earlier backup. A copy that fails midway is removed: a truncated
/// backup would claim to be the original.
fn write_backup(path: &Path, source: &mut std::fs::File) -> std::io::Result<PathBuf> {
    let secs = unix_seconds();
    for n in 1..100 {
        let name =
            if n == 1 { format!("{BACKUP_PREFIX}{secs}.json") } else { format!("{BACKUP_PREFIX}{secs}-{n}.json") };
        let backup = path.with_file_name(name);
        match std::fs::File::create_new(&backup) {
            Ok(mut f) => {
                if let Err(e) = std::io::copy(source, &mut f) {
                    drop(f);
                    if let Err(e) = std::fs::remove_file(&backup) {
                        log::error("settings backup cleanup", &e);
                    }
                    return Err(e);
                }
                return Ok(backup);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free name for the settings backup"))
}

/// What is on disk right now. `Ok(None)` = no file yet.
fn read_disk(path: &Path) -> std::io::Result<Option<Parsed>> {
    match document::read_at_most(path, limits::MAX_FILE_BYTES) {
        Ok(bytes) => Ok(Some(match String::from_utf8(bytes) {
            Ok(text) => parse_settings(&text),
            // Lossy decoding would parse fine and write U+FFFD back on the next save.
            Err(_) => Parsed::Unreadable("設定ファイルが UTF-8 ではありません".into()),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        // Handled like any other unreadable file: set aside, defaults used.
        Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
            Ok(Some(Parsed::Unreadable("設定ファイルが大きすぎます".into())))
        }
        Err(e) => Err(e),
    }
}

/// Serializes read-modify-write across running instances. Waiting is bounded: a
/// wedged instance must not freeze this one, so after the timeout the write proceeds
/// unserialized (still atomic per file; at worst the old last-writer-wins race).
struct SettingsLock {
    handle: Option<HANDLE>,
    /// Whether the wait gave us the mutex: only then may it be released.
    owned: bool,
}

impl SettingsLock {
    fn acquire() -> Self {
        unsafe {
            match CreateMutexW(None, false, w!("Local\\MyMDedit.Settings")) {
                Ok(h) => {
                    let wait = WaitForSingleObject(h, 2000);
                    // An abandoned mutex is ours all the same; its last owner died mid-write,
                    // which the atomic save already makes harmless.
                    let owned = wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED;
                    if !owned {
                        let reason = if wait == WAIT_TIMEOUT {
                            "timed out; writing unserialized".to_string()
                        } else {
                            windows::core::Error::from_thread().to_string()
                        };
                        log::error("SettingsLock wait", reason);
                    }
                    Self { handle: Some(h), owned }
                }
                Err(e) => {
                    log::error("SettingsLock", &e);
                    Self { handle: None, owned: false }
                }
            }
        }
    }
}

impl Drop for SettingsLock {
    fn drop(&mut self) {
        if let Some(h) = self.handle {
            unsafe {
                if self.owned
                    && let Err(e) = ReleaseMutex(h)
                {
                    log::error("SettingsLock release", &e);
                }
                if let Err(e) = CloseHandle(h) {
                    log::error("SettingsLock close", &e);
                }
            }
        }
    }
}

/// Windows paths compare case-insensitively across all of Unicode, and `C:\a\` names
/// the same folder as `C:\a`.
pub fn same_folder(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> Vec<u16> { s.replace('/', "\\").trim_end_matches('\\').encode_utf16().collect() };
    let (a, b) = (norm(a), norm(b));
    unsafe { CompareStringOrdinal(&a, &b, true) == CSTR_EQUAL }
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
    fn one_bad_field_keeps_the_others() {
        match parse_settings(r#"{"theme": "dark", "editor_font_size": 20, "table_row_striping": true}"#) {
            Parsed::Partial(s, rejected) => {
                assert_eq!(rejected, vec!["theme"]);
                assert_eq!(s.theme, Theme::System);
                assert_eq!(s.editor_font_size, 20.0);
                assert!(s.table_row_striping);
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[test]
    fn bom_is_accepted_and_unknown_keys_ignored() {
        assert!(
            matches!(parse_settings("\u{FEFF}{\"theme\": \"Dark\", \"future\": 1}"), Parsed::Clean(s) if s.theme == Theme::Dark)
        );
    }

    #[test]
    fn syntax_error_is_unreadable() {
        assert!(matches!(parse_settings("{\"theme\": \"Dark\",}"), Parsed::Unreadable(_)));
        assert!(matches!(parse_settings("[]"), Parsed::Unreadable(_)));
    }

    fn test_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mymdedit-settings-{tag}-{}-{}", std::process::id(), unix_seconds()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn broken_file_is_backed_up_before_it_can_be_overwritten() {
        let dir = test_dir("broken");
        let path = dir.join("settings.json");
        std::fs::write(&path, "{\"theme\": \"Dark\",}").unwrap();
        let mut store = SettingsStore::open(Some(path.clone()));
        assert!(store.take_load_warning().is_some_and(|w| w.contains("settings.broken-")));
        assert!(store.update(|_| {}).is_ok());
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.broken-"))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read_to_string(backups[0].path()).unwrap(), "{\"theme\": \"Dark\",}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn oversized_file_is_set_aside_not_read() {
        let dir = test_dir("oversized");
        let path = dir.join("settings.json");
        std::fs::write(&path, vec![b' '; limits::MAX_FILE_BYTES as usize + 1]).unwrap();
        let mut store = SettingsStore::open(Some(path.clone()));
        assert!(store.take_load_warning().is_some_and(|w| w.contains("settings.broken-")));
        assert_eq!(store.current(), &AppSettings::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sanitizes_loaded_values() {
        match parse_settings(r#"{"editor_font_size": 200, "preview_background_color": "red"}"#) {
            Parsed::Partial(s, fields) => {
                assert_eq!(s.editor_font_size, 48.0);
                assert_eq!(s.preview_background_color, None);
                assert_eq!(s.preview_font_family, "Segoe UI, sans-serif");
                assert!(fields.contains(&"editor_font_size".to_string()));
                assert!(fields.contains(&"preview_background_color".to_string()));
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[test]
    fn invalid_utf8_is_unreadable_not_lossy() {
        let dir = test_dir("utf8");
        let path = dir.join("settings.json");
        std::fs::write(&path, b"{\"editor_font_family\": \"\xFF\"}").unwrap();
        let mut store = SettingsStore::open(Some(path.clone()));
        assert!(store.take_load_warning().is_some());
        assert_eq!(store.current().editor_font_family, AppSettings::default().editor_font_family);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sanitized_fields_are_reported() {
        match parse_settings(r#"{"preview_background_color": "red", "editor_font_size": 20}"#) {
            Parsed::Partial(s, fields) => {
                assert_eq!(fields, vec!["preview_background_color"]);
                assert_eq!(s.editor_font_size, 20.0);
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[test]
    fn two_instances_keep_each_others_changes() {
        let dir = test_dir("rmw");
        let path = dir.join("settings.json");
        let mut a = SettingsStore::open(Some(path.clone()));
        let mut b = SettingsStore::open(Some(path.clone()));
        assert!(a.update(|s| s.theme = Theme::Dark).is_ok());
        assert!(b.update(|s| s.recent_folders = vec!["C:\\x".into()]).is_ok());
        let reloaded = SettingsStore::open(Some(path));
        assert_eq!(reloaded.current().theme, Theme::Dark);
        assert_eq!(reloaded.current().recent_folders, vec!["C:\\x"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn backups(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("settings.broken-"))
            .collect()
    }

    #[test]
    fn update_keeps_what_another_writer_left_in_a_partly_bad_file() {
        let dir = test_dir("partial-update");
        let path = dir.join("settings.json");
        let mut store = SettingsStore::open(Some(path.clone()));
        // Hand-edited (or written by another version) after startup.
        std::fs::write(&path, r#"{"theme": "dark", "editor_font_size": 20}"#).unwrap();
        assert!(store.update(|s| s.recent_folders = vec!["C:\\x".into()]).is_ok());
        let reloaded = SettingsStore::open(Some(path.clone()));
        assert_eq!(reloaded.current().editor_font_size, 20.0);
        assert_eq!(reloaded.current().recent_folders, vec!["C:\\x"]);
        assert_eq!(backups(&dir).len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_backs_up_a_file_broken_after_startup() {
        let dir = test_dir("broken-update");
        let path = dir.join("settings.json");
        let mut store = SettingsStore::open(Some(path.clone()));
        std::fs::write(&path, "{\"theme\": \"Dark\",}").unwrap();
        assert!(store.update(|s| s.theme = Theme::Light).is_ok());
        let names = backups(&dir);
        assert_eq!(names.len(), 1);
        assert_eq!(std::fs::read_to_string(dir.join(&names[0])).unwrap(), "{\"theme\": \"Dark\",}");
        assert_eq!(SettingsStore::open(Some(path)).current().theme, Theme::Light);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn backups_within_one_second_do_not_overwrite_each_other() {
        let dir = test_dir("backup-names");
        let path = dir.join("settings.json");
        std::fs::write(&path, "first").unwrap();
        let a = back_up(&path).unwrap();
        // The same broken content again is not copied twice...
        assert_eq!(back_up(&path).unwrap(), a);
        // ...but different content gets its own backup, even within the same second
        // (and even at the same length).
        std::fs::write(&path, "fifth").unwrap();
        let c = back_up(&path).unwrap();
        assert_ne!(a, c);
        assert_eq!(std::fs::read_to_string(&c).unwrap(), "fifth");
        std::fs::write(&path, "second").unwrap();
        let b = back_up(&path).unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "second");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_left_broken_is_not_backed_up_again_on_every_launch() {
        let dir = test_dir("relaunch");
        let path = dir.join("settings.json");
        std::fs::write(&path, "{\"theme\": \"Dark\",}").unwrap();
        let first = SettingsStore::open(Some(path.clone())).take_load_warning().unwrap();
        // The next launch (a new store, nothing remembered in memory) finds the same file.
        let second = SettingsStore::open(Some(path.clone())).take_load_warning().unwrap();
        assert_eq!(backups(&dir).len(), 1);
        assert_eq!(first, second, "the warning points at the backup that already exists");
        // A file broken differently still gets its own backup.
        std::fs::write(&path, "{\"theme\": \"Light\",}").unwrap();
        SettingsStore::open(Some(path.clone()));
        assert_eq!(backups(&dir).len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_oversized_file_is_backed_up_whole() {
        let dir = test_dir("oversized-backup");
        let path = dir.join("settings.json");
        let mut big = vec![b' '; limits::MAX_FILE_BYTES as usize * 3];
        big.extend_from_slice(b"tail");
        std::fs::write(&path, &big).unwrap();
        SettingsStore::open(Some(path.clone()));
        SettingsStore::open(Some(path.clone()));
        let names = backups(&dir);
        assert_eq!(names.len(), 1);
        assert!(std::fs::read(dir.join(&names[0])).unwrap() == big);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_sanitizes_even_when_nothing_is_written() {
        let mut store = SettingsStore::open(None);
        assert!(store.update(|s| s.editor_font_size = 999.0).is_err());
        assert_eq!(store.current().editor_font_size, limits::MAX_FONT_SIZE);
    }

    #[test]
    fn display_change_detection_ignores_bookkeeping_fields() {
        let a = AppSettings::default();
        let bookkeeping =
            AppSettings { recent_folders: vec!["C:\\x".into()], custom_colors: vec!["#fff".into()], ..a.clone() };
        assert!(!display_differs(&a, &bookkeeping));
        assert!(display_differs(&a, &AppSettings { theme: Theme::Dark, ..a.clone() }));
        assert!(display_differs(&a, &AppSettings { editor_font_size: 20.0, ..a.clone() }));
    }

    #[test]
    fn folders_compare_unicode_case_insensitively_ignoring_trailing_separator() {
        assert!(same_folder("C:\\Ä\\Docs\\", "c:\\ä\\docs"));
        assert!(!same_folder("C:\\a", "C:\\ab"));
        assert_eq!(add_recent_folder(&["C:\\Ä".into()], "c:\\ä"), vec!["c:\\ä"]);
    }
}
