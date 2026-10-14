//! The single error sink. Every `Err` this app swallows must be recorded here.
//!
//! Recording is not notifying: failures the user must act on additionally go through
//! `App::report_error`, which raises the error banner under the toolbar.

use std::io::Write;
use std::path::{Path, PathBuf};

use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows::core::HSTRING;

pub fn error(context: &str, message: impl std::fmt::Display) {
    let line = format!("[MyMDedit] ERROR {context}: {message}\n");
    // OutputDebugString rather than a debug-only channel, so release builds still leave
    // a trail that DebugView / a debugger can pick up.
    unsafe { OutputDebugStringW(&HSTRING::from(line.as_str())) };
    #[cfg(debug_assertions)]
    eprint!("{line}");
}

/// `%LOCALAPPDATA%\MyMDedit\crash.log`. Release builds have no console and are
/// stripped, so a panic would otherwise leave nothing behind.
pub fn crash_log_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("MyMDedit").join("crash.log"))
}

pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{line}")
}

/// Records every panic before it unwinds. Most panics here happen inside an
/// `extern "system"` callback, where unwinding aborts the process: the hook runs first,
/// so this is the only trace the user (and the next developer) gets.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!("[{secs}] panic: {info}");
        error("panic", &line);
        if let Some(path) = crash_log_path() {
            let _ = append_line(&path, &line);
        }
    }));
}

#[cfg(test)]
mod tests {
    #[test]
    fn append_line_creates_and_appends() {
        let dir = std::env::temp_dir().join(format!("mymdedit-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("crash.log");
        super::append_line(&path, "one").unwrap();
        super::append_line(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
