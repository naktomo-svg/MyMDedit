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

/// Records `result`'s error, if any: for calls whose failure the app cannot do anything
/// about, but which must not fail without a trace.
pub fn if_err<T, E: std::fmt::Display>(context: &str, result: Result<T, E>) {
    if let Err(e) = result {
        error(context, e);
    }
}

/// `%LOCALAPPDATA%\MyMDedit\crash.log`. Release builds have no console and are
/// stripped, so a panic would otherwise leave nothing behind.
pub fn crash_log_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("MyMDedit").join("crash.log"))
}

/// crash.log's size limit: past it, the log becomes `crash.old.log` (replacing the one
/// before) and starts afresh, so a panic that recurs every launch cannot grow it forever.
const CRASH_LOG_MAX_BYTES: u64 = 256 * 1024;

pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    append_line_capped(path, line, CRASH_LOG_MAX_BYTES)
}

fn append_line_capped(path: &Path, line: &str, max_bytes: u64) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() >= max_bytes) {
        std::fs::rename(path, path.with_extension("old.log"))?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

/// Records every panic before it unwinds. Most panics here happen inside an
/// `extern "system"` callback, where unwinding aborts the process: the hook runs first,
/// so this is the only trace the user (and the next developer) gets.
pub fn install_panic_hook() {
    // Debug builds have a console: there the default hook still prints the message and,
    // with RUST_BACKTRACE set, the backtrace.
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let line = format!("[{secs}] panic: {info}");
        error("panic", &line);
        if let Some(path) = crash_log_path()
            && let Err(e) = append_line(&path, &line)
        {
            error("crash.log", e);
        }
        if cfg!(debug_assertions) {
            default(info);
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
        // Past the cap the log moves aside and starts again; only one old copy is kept.
        super::append_line_capped(&path, "three", 4).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "three\n");
        assert_eq!(std::fs::read_to_string(path.with_extension("old.log")).unwrap(), "one\ntwo\n");
        super::append_line_capped(&path, "four", 4).unwrap();
        assert_eq!(std::fs::read_to_string(path.with_extension("old.log")).unwrap(), "three\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
