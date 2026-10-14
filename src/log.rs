//! The single error sink. Every `Err` this app swallows must be recorded here.
//!
//! Recording is not notifying: failures the user must act on additionally go through
//! `App::report_error`, which raises the error banner under the toolbar.

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
