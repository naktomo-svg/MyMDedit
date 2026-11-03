//! Small Win32 helpers shared by the windows of this app.

use windows::Win32::Foundation::{COLORREF, GlobalFree, HANDLE, HWND, LPARAM, RECT, SetLastError, WIN32_ERROR, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CreateFontIndirectW, CreateFontW, DEFAULT_CHARSET, DeleteObject, FW_BOLD, FW_NORMAL,
    GetMonitorInfoW, HFONT, HGDIOBJ, LOGFONTW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowTextLengthW, GetWindowTextW, MESSAGEBOX_RESULT, MESSAGEBOX_STYLE, MessageBoxW, NONCLIENTMETRICSW,
    SPI_GETNONCLIENTMETRICS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SendMessageW,
};
use windows::core::PCWSTR;

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

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
    OpenDropped,
}

const fn app_message(m: AppMessage) -> u32 {
    windows::Win32::UI::WindowsAndMessaging::WM_APP + m as u32
}

/// A recent folder's existence check finished (the result waits in a channel).
pub const WM_APP_FOLDER_PROBED: u32 = app_message(AppMessage::FolderProbed);
/// Re-render the preview (coalesces edits).
pub const WM_APP_RENDER: u32 = app_message(AppMessage::Render);
/// A WebView2 process died (wparam: 0 renderer, 1 browser; lparam: the preview
/// generation whose controller reported it).
pub const WM_APP_PREVIEW_FAILED: u32 = app_message(AppMessage::PreviewFailed);
/// The window was activated: look for changes to the open file.
pub const WM_APP_CHECK_DISK: u32 = app_message(AppMessage::CheckDisk);
/// A stopped preview could not be hidden and must be closed instead — never from inside
/// the WebView2 handler that found out.
pub const WM_APP_PREVIEW_CLOSE: u32 = app_message(AppMessage::PreviewClose);
/// The open file's disk check finished (the result waits in a channel).
pub const WM_APP_DISK_CHECKED: u32 = app_message(AppMessage::DiskChecked);
/// A file was dropped on the window (the path waits in `App`): open it — never from
/// inside the drop itself, which the dragging program is still waiting on.
pub const WM_APP_OPEN_DROPPED: u32 = app_message(AppMessage::OpenDropped);

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
/// variable: `let p = pcwstr(&wide(s));` dangles as soon as that statement ends (the
/// temporary buffer is dropped there). Inside the call itself — `Api(pcwstr(&wide(s)))` —
/// the temporary lives to the end of the statement and is safe, but a named buffer keeps
/// the code from turning into the first form in a later edit.
pub fn pcwstr(buf: &[u16]) -> PCWSTR {
    debug_assert_eq!(buf.last(), Some(&0), "pcwstr needs a NUL-terminated buffer");
    PCWSTR(buf.as_ptr())
}

/// A message box titled "MyMDedit". MessageBoxW answers 0 when it cannot show the box
/// (out of resources, owner gone), and the user then never saw `text`: that is recorded.
pub fn message_box(owner: Option<HWND>, text: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let (text, caption) = (wide(text), wide("MyMDedit"));
    let answer = unsafe { MessageBoxW(owner, pcwstr(&text), pcwstr(&caption), style) };
    if answer.0 == 0 {
        crate::log::error("MessageBoxW", windows::core::Error::from_thread());
    }
    answer
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

/// Where a `w`×`h` dialog goes: centered over `owner`, then moved fully onto the work
/// area of the owner's monitor. An owner dragged mostly off screen would otherwise put
/// the dialog's title bar out of reach.
pub fn centered_over(owner: HWND, w: i32, h: i32) -> (i32, i32) {
    let mut owner_rect = RECT::default();
    unsafe {
        crate::log::if_err(
            "GetWindowRect",
            windows::Win32::UI::WindowsAndMessaging::GetWindowRect(owner, &mut owner_rect),
        );
    }
    let x = owner_rect.left + (width(&owner_rect) - w) / 2;
    let y = owner_rect.top + (height(&owner_rect) - h) / 2;
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let monitor = unsafe { MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST) };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        crate::log::error("GetMonitorInfoW", "no work area; the dialog stays centered on its owner");
        return (x, y);
    }
    fit_in(x, y, w, h, &info.rcWork)
}

/// `(x, y)` moved as little as possible to put the `w`×`h` box inside `area`; a box
/// larger than `area` keeps its top-left corner (and so its title bar) inside.
fn fit_in(x: i32, y: i32, w: i32, h: i32, area: &RECT) -> (i32, i32) {
    let x = x.min(area.right - w).max(area.left);
    let y = y.min(area.bottom - h).max(area.top);
    (x, y)
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

/// The file to open out of a drop: the first name, as the dragging program listed them.
/// The others are ignored (one window, one document). `None` when there is no first name
/// or it could not be read (an empty name). `names` is consumed lazily, so only the first
/// name is ever fetched.
pub fn pick_dropped(names: impl IntoIterator<Item = Vec<u16>>) -> Option<PathBuf> {
    let name = names.into_iter().next()?;
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    if len == 0 {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&name[..len])))
}

/// The first file named in a drop (`pick_dropped`). The caller still owns `hdrop`.
pub fn dropped_file(hdrop: HDROP) -> Option<PathBuf> {
    let names = (0..unsafe { DragQueryFileW(hdrop, u32::MAX, None) }).map(|i| {
        // The length without the NUL; 0 is also what a failure answers.
        let len = unsafe { DragQueryFileW(hdrop, i, None) } as usize;
        let mut buf = vec![0u16; len + 1];
        let copied = if len > 0 { unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) as usize } } else { 0 };
        buf.truncate(copied.min(len));
        buf
    });
    pick_dropped(names)
}

// ---- lparam as a pointer ------------------------------------------------------------
//
// Every window procedure that reads a message's lparam as a pointer goes through these,
// so a null lparam falls back to the message's default handling instead of crashing:
// another process of the same user can SendMessage any message with lparam 0. Only null
// is caught — a non-null address that points nowhere cannot be told from a real one, and
// for messages Windows does not marshal across processes there is no way to check it.

/// The `T` a message's lparam points at; `None` for a null lparam.
///
/// # Safety
/// A non-null `lparam` must point at a valid `T` for `'a` (in practice, the window
/// procedure call that received it).
pub unsafe fn lparam_ref<'a, T>(lparam: LPARAM) -> Option<&'a T> {
    unsafe { (lparam.0 as *const T).as_ref() }
}

/// `lparam_ref` for a struct the message expects to be filled in.
///
/// # Safety
/// As `lparam_ref`, and nothing else may refer to the `T` during `'a`.
pub unsafe fn lparam_mut<'a, T>(lparam: LPARAM) -> Option<&'a mut T> {
    unsafe { (lparam.0 as *mut T).as_mut() }
}

/// The NUL-terminated UTF-16 string a message's lparam points at; empty for a null
/// lparam or text that is not valid UTF-16.
///
/// # Safety
/// A non-null `lparam` must point at a NUL-terminated UTF-16 string.
pub unsafe fn lparam_string(lparam: LPARAM) -> String {
    if lparam.0 == 0 {
        return String::new();
    }
    unsafe { PCWSTR(lparam.0 as *const u16).to_string() }.unwrap_or_default()
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
    fn a_dialog_is_moved_onto_the_work_area() {
        use super::fit_in;
        use windows::Win32::Foundation::RECT;
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1040 };
        assert_eq!(fit_in(100, 100, 400, 300, &work), (100, 100), "already inside: unchanged");
        assert_eq!(fit_in(1800, -50, 400, 300, &work), (1520, 0));
        assert_eq!(fit_in(-300, 900, 400, 300, &work), (0, 740));
        // Taller than the screen: the top (title bar) stays reachable.
        assert_eq!(fit_in(10, 500, 400, 2000, &work), (10, 0));
        let second = RECT { left: 1920, top: 0, right: 3840, bottom: 1040 };
        assert_eq!(fit_in(1700, 100, 400, 300, &second), (1920, 100), "a monitor to the right");
    }

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
    fn a_drop_opens_its_first_file_only() {
        use super::pick_dropped;
        use std::path::PathBuf;
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(pick_dropped(Vec::new()), None, "nothing dropped");
        assert_eq!(pick_dropped(vec![w(r"C:\d\a.md"), w(r"C:\d\b.md")]), Some(PathBuf::from(r"C:\d\a.md")));
        assert_eq!(pick_dropped(vec![w("C:\\d\\a.md\0junk")]), Some(PathBuf::from(r"C:\d\a.md")), "cut at the NUL");
        // An unreadable first name is not replaced by the second: the user dropped the first.
        assert_eq!(pick_dropped(vec![Vec::new(), w(r"C:\d\b.md")]), None);
        assert_eq!(pick_dropped(vec![vec![0u16, 0x41]]), None, "empty before the NUL");
        // Only the first name is fetched.
        let mut fetched = 0;
        let names = (0..3).map(|i| {
            fetched += 1;
            w(&format!(r"C:\d\{i}.md"))
        });
        assert_eq!(pick_dropped(names), Some(PathBuf::from(r"C:\d\0.md")));
        assert_eq!(fetched, 1);
    }

    #[test]
    fn a_null_lparam_is_none() {
        use super::{lparam_mut, lparam_ref, lparam_string};
        use windows::Win32::Foundation::{LPARAM, RECT};
        let null = LPARAM(0);
        assert!(unsafe { lparam_ref::<RECT>(null) }.is_none());
        assert!(unsafe { lparam_mut::<RECT>(null) }.is_none());
        assert_eq!(unsafe { lparam_string(null) }, "", "null reads as no text");

        let mut rect = RECT { left: 1, top: 2, right: 3, bottom: 4 };
        let lparam = LPARAM(&raw mut rect as isize);
        assert_eq!(unsafe { lparam_ref::<RECT>(lparam) }.map(|r| r.right), Some(3));
        if let Some(r) = unsafe { lparam_mut::<RECT>(lparam) } {
            r.bottom = 40;
        }
        assert_eq!(rect.bottom, 40);

        let text = super::wide("ImmersiveColorSet");
        assert_eq!(unsafe { lparam_string(LPARAM(text.as_ptr() as isize)) }, "ImmersiveColorSet");
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
