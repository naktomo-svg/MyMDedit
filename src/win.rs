//! Small Win32 helpers shared by the windows of this app.

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CreateFontIndirectW, CreateFontW, DEFAULT_CHARSET, DeleteObject, FW_BOLD, FW_NORMAL, HFONT,
    HGDIOBJ, LOGFONTW, OUT_DEFAULT_PRECIS,
};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowTextLengthW, GetWindowTextW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SendMessageW,
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

pub fn dpi_of(hwnd: HWND) -> u32 {
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => 96,
        d => d,
    }
}

/// Device pixels for `dip` at `dpi`.
pub fn scale(dip: i32, dpi: u32) -> i32 {
    (dip as i64 * dpi as i64 / 96) as i32
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

fn nonclient_metrics(dpi: u32) -> NONCLIENTMETRICSW {
    let mut m = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
    unsafe {
        let _ = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            m.cbSize,
            Some(&mut m as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0).0,
            dpi,
        );
    }
    m
}

/// The system message font (what dialogs use) at `dpi`, optionally bold.
pub fn message_font(dpi: u32, bold: bool) -> HFONT {
    let mut lf: LOGFONTW = nonclient_metrics(dpi).lfMessageFont;
    if bold {
        lf.lfWeight = FW_BOLD.0 as i32;
    }
    unsafe { CreateFontIndirectW(&lf) }
}

pub fn menu_font(dpi: u32) -> HFONT {
    let lf = nonclient_metrics(dpi).lfMenuFont;
    unsafe { CreateFontIndirectW(&lf) }
}

/// A font of `face` whose em height is `px` device pixels.
pub fn font(face: &str, px: i32, bold: bool) -> HFONT {
    let face = wide(face);
    unsafe {
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
    }
}

pub fn delete_font(f: HFONT) {
    if !f.is_invalid() {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(f.0));
        }
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
}
