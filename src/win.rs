//! Small Win32 helpers shared by the windows of this app.

use windows::Win32::Foundation::{COLORREF, GlobalFree, HANDLE, HWND, LPARAM, RECT, WPARAM};
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

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn pcwstr(buf: &[u16]) -> PCWSTR {
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

pub fn window_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n as usize])
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
            let mut lf = LOGFONTW { lfHeight: -scale(12, dpi), lfWeight: FW_NORMAL.0 as i32, ..Default::default() };
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
    // Fails only for a font still selected into a DC: a leak worth a trace.
    if !f.is_invalid() && !unsafe { DeleteObject(HGDIOBJ(f.0)) }.as_bool() {
        crate::log::error("DeleteObject", "font still in use; leaked");
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
        // opened and emptied: a failed copy must leave what the user had there, and the
        // clipboard is held only as long as the hand-over takes.
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
