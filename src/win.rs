//! Small Win32 helpers shared by the windows of this app.

use windows::Win32::Foundation::{COLORREF, GlobalFree, HANDLE, HWND, LPARAM, RECT, SetLastError, WIN32_ERROR, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CreateFontIndirectW, CreateFontW, DEFAULT_CHARSET, DeleteObject, FW_BOLD, FW_NORMAL, HFONT,
    HGDIOBJ, LOGFONTW, OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowTextLengthW, GetWindowTextW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SendMessageW,
};
use windows::core::PCWSTR;

/// The app's own window messages, all posted to the main window. Numbered by one enum so
/// that two cannot share a value: the compiler rejects duplicate discriminants, where two
/// hand-written `WM_APP + n` in different modules would silently run each other's
/// handler. `main.rs`'s tests fail on a `WM_APP +` written anywhere else.
#[repr(u32)]
enum AppMessage {
    FolderProbed = 1,
    Render,
    PreviewFailed,
    CheckDisk,
    PreviewClose,
    DiskChecked,
}

const fn app_message(m: AppMessage) -> u32 {
    windows::Win32::UI::WindowsAndMessaging::WM_APP + m as u32
}

/// A recent folder's existence check finished (the result waits in a channel).
pub const WM_APP_FOLDER_PROBED: u32 = app_message(AppMessage::FolderProbed);
/// Re-render the preview (coalesces edits).
pub const WM_APP_RENDER: u32 = app_message(AppMessage::Render);
/// A WebView2 process died (wparam: 0 renderer, 1 browser).
pub const WM_APP_PREVIEW_FAILED: u32 = app_message(AppMessage::PreviewFailed);
/// The window was activated: look for changes to the open file.
pub const WM_APP_CHECK_DISK: u32 = app_message(AppMessage::CheckDisk);
/// A stopped preview could not be hidden and must be closed instead — never from inside
/// the WebView2 handler that found out.
pub const WM_APP_PREVIEW_CLOSE: u32 = app_message(AppMessage::PreviewClose);
/// The open file's disk check finished (the result waits in a channel).
pub const WM_APP_DISK_CHECKED: u32 = app_message(AppMessage::DiskChecked);

/// 64 bits from the system's cryptographic RNG, for values that must not be guessed.
pub fn secure_random_u64() -> windows::core::Result<u64> {
    use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    let mut bytes = [0u8; 8];
    unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }.ok()?;
    Ok(u64::from_ne_bytes(bytes))
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A pointer to `buf` for a Win32 call: `buf` must end in a NUL (as `wide` makes it) and
/// outlive the call. The pointer carries no lifetime, so keep the buffer in a named
/// variable — `pcwstr(&wide(s))` would dangle by the time the call reads it.
pub fn pcwstr(buf: &[u16]) -> PCWSTR {
    debug_assert_eq!(buf.last(), Some(&0), "pcwstr needs a NUL-terminated buffer");
    PCWSTR(buf.as_ptr())
}

pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

pub fn send(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(wparam)), Some(LPARAM(lparam))).0 }
}

/// The window's DPI; 96 (and a log line) for an invalid window, the only way it fails.
pub fn dpi_of(hwnd: HWND) -> u32 {
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => {
            crate::log::error("GetDpiForWindow", "invalid window; assuming 96 DPI");
            96
        }
        d => d,
    }
}

/// Device pixels for `dip` at `dpi`, rounded half away from zero as `MulDiv` (and so
/// the system's own metrics) rounds.
pub fn scale(dip: i32, dpi: u32) -> i32 {
    let n = dip as i64 * dpi as i64;
    ((n + n.signum() * 48) / 96) as i32
}

/// A control's text. Empty when it cannot be read — a failure told apart from an empty
/// control and logged. Lossy: the callers read numbers and font names from their own
/// dialog's edit boxes, where a lone surrogate is as invalid as any other wrong input.
pub fn window_text(hwnd: HWND) -> String {
    unsafe {
        // Both calls answer 0 for an empty control too; only the last error tells.
        SetLastError(WIN32_ERROR(0));
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len.max(0) as usize + 1];
        let n = if len > 0 { GetWindowTextW(hwnd, &mut buf) } else { 0 };
        if n == 0 {
            let e = windows::core::Error::from_thread();
            if e.code().is_err() {
                crate::log::error("window_text", &e);
            }
        }
        String::from_utf16_lossy(&buf[..(n.max(0) as usize).min(buf.len())])
    }
}

pub fn width(r: &RECT) -> i32 {
    r.right - r.left
}

pub fn height(r: &RECT) -> i32 {
    r.bottom - r.top
}

/// The system's message and menu fonts at `dpi`. Should the query fail, both fall back
/// to Windows' documented UI font at its default 9pt (12px at 96 DPI) — logged, since a
/// zeroed LOGFONTW would quietly become GDI's System font instead.
fn nonclient_fonts(dpi: u32) -> (LOGFONTW, LOGFONTW) {
    let mut m = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
    let result = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            m.cbSize,
            Some(&mut m as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0).0,
            dpi,
        )
    };
    match result {
        Ok(()) => (m.lfMessageFont, m.lfMenuFont),
        Err(e) => {
            crate::log::error("SystemParametersInfoForDpi", &e);
            // The same character set and quality `font` asks for; zero would be
            // ANSI_CHARSET, leaving Japanese to font linking on this path alone.
            let mut lf = LOGFONTW {
                lfHeight: -scale(12, dpi),
                lfWeight: FW_NORMAL.0 as i32,
                lfCharSet: DEFAULT_CHARSET,
                lfQuality: CLEARTYPE_QUALITY,
                ..Default::default()
            };
            let face: Vec<u16> = "Segoe UI".encode_utf16().collect();
            lf.lfFaceName[..face.len()].copy_from_slice(&face);
            (lf, lf)
        }
    }
}

/// A created font, or the failure logged (a null HFONT makes the control fall back to
/// the System font, which is all that can be done about it).
fn checked(f: HFONT, what: &str) -> HFONT {
    // CreateFont(Indirect) does not set the thread's last error, so there is no reason
    // to give beyond which font it was.
    if f.is_invalid() {
        crate::log::error("CreateFont", format!("{what}: could not be created"));
    }
    f
}

/// The system message font (what dialogs use) at `dpi`, optionally bold.
pub fn message_font(dpi: u32, bold: bool) -> HFONT {
    let mut lf = nonclient_fonts(dpi).0;
    if bold {
        lf.lfWeight = FW_BOLD.0 as i32;
    }
    checked(unsafe { CreateFontIndirectW(&lf) }, "message font")
}

pub fn menu_font(dpi: u32) -> HFONT {
    let lf = nonclient_fonts(dpi).1;
    checked(unsafe { CreateFontIndirectW(&lf) }, "menu font")
}

/// A font of `face` whose em height is `px` device pixels.
pub fn font(face: &str, px: i32, bold: bool) -> HFONT {
    let what = format!("{face} {px}px");
    let face = wide(face);
    let f = unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            if bold { FW_BOLD.0 as i32 } else { FW_NORMAL.0 as i32 },
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            Default::default(),
            CLEARTYPE_QUALITY,
            0,
            pcwstr(&face),
        )
    };
    checked(f, &what)
}

pub fn delete_font(f: HFONT) {
    // Fails for a font still selected into a DC (a leak) or a handle that is no longer a
    // font (deleted twice): either is worth a trace, and the log cannot tell which.
    if !f.is_invalid() && !unsafe { DeleteObject(HGDIOBJ(f.0)) }.as_bool() {
        crate::log::error("DeleteObject", "font still selected into a DC (leaked) or already deleted");
    }
}

/// `1234567` → `1,234,567` (what the original's `N0` format produced in ja-JP).
pub fn group_thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Puts `text` on the clipboard as CF_UNICODETEXT, owned by `hwnd`.
pub fn set_clipboard_text(hwnd: HWND, text: &str) -> windows::core::Result<()> {
    let units = wide(text);
    unsafe {
        // Everything that can fail for lack of memory happens before the clipboard is
        // opened and emptied, so such a failure leaves what the user had there; and the
        // clipboard is held only as long as the hand-over takes. The one exception is
        // SetClipboardData failing after EmptyClipboard: taking ownership requires the
        // emptying, so the clipboard is then left empty (the error says the copy failed).
        let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * size_of::<u16>())?;
        let free = |e: windows::core::Error| {
            if let Err(free_error) = GlobalFree(Some(memory)) {
                crate::log::error("GlobalFree", free_error);
            }
            e
        };
        let target = GlobalLock(memory).cast::<u16>();
        if target.is_null() {
            return Err(free(windows::core::Error::from_thread()));
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), target, units.len());
        // FALSE with no error is the normal "no longer locked" answer.
        let _ = GlobalUnlock(memory);
        open_clipboard(hwnd).map_err(free)?;
        // On success the clipboard owns the memory; on failure it is still ours.
        let result = EmptyClipboard()
            .and_then(|()| SetClipboardData(CF_UNICODETEXT.0.into(), Some(HANDLE(memory.0))).map(|_| ()))
            .map_err(free);
        if let Err(e) = CloseClipboard() {
            crate::log::error("CloseClipboard", &e);
        }
        result
    }
}

/// OpenClipboard fails while any other program has the clipboard open, which clipboard
/// managers and remote-desktop clients do for a moment after every change. A few short
/// retries ride that out; the wait is bounded (about 100 ms) since this is the UI thread.
fn open_clipboard(hwnd: HWND) -> windows::core::Result<()> {
    let mut last = None;
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        match unsafe { OpenClipboard(Some(hwnd)) } {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(windows::core::Error::from_thread))
}

pub fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

pub fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

#[cfg(test)]
mod tests {
    #[test]
    #[should_panic(expected = "NUL-terminated")]
    fn pcwstr_refuses_a_buffer_without_its_nul() {
        let units: Vec<u16> = "abc".encode_utf16().collect();
        super::pcwstr(&units);
    }

    #[test]
    fn window_text_of_a_window_that_is_gone_is_empty() {
        // Not a window at all: GetWindowText fails, which reads as empty (and is logged).
        let gone = windows::Win32::Foundation::HWND(0x7FFF_0000 as *mut _);
        assert_eq!(super::window_text(gone), "");
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(super::group_thousands(0), "0");
        assert_eq!(super::group_thousands(999), "999");
        assert_eq!(super::group_thousands(1234567), "1,234,567");
    }

    #[test]
    fn scale_rounds_like_muldiv() {
        assert_eq!(super::scale(1, 144), 2);
        assert_eq!(super::scale(-1, 144), -2);
        assert_eq!(super::scale(10, 120), 13);
        assert_eq!(super::scale(16, 96), 16);
        assert_eq!(super::scale(0, 144), 0);
    }
}
