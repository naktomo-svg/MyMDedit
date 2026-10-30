//! Light/dark theming for a plain Win32 window.
//!
//! Win32 has no single "dark mode" switch. The title bar is a documented DWM attribute;
//! popup menus and scrollbars follow the (undocumented but long-stable, used by Notepad++
//! and others) uxtheme `SetPreferredAppMode`; and the menu bar itself has to be painted
//! by hand through the undocumented `WM_UAHDRAWMENU*` messages, because no theme
//! covers it.

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DT_CENTER, DT_HIDEPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, FillRect,
    GetWindowDC, HDC, HFONT, HGDIOBJ, MapWindowPoints, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, SetWindowTheme};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetClientRect, GetMenuBarInfo, GetMenuItemInfoW, GetWindowRect, HMENU, MENUBARINFO, MENUITEMINFOW,
    MIIM_STRING, OBJID_MENU,
};
use windows::core::{BOOL, PCSTR, PWSTR, w};

use crate::win::rgb;

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub window_bg: COLORREF,
    pub text: COLORREF,
    pub subtle_text: COLORREF,
    pub divider: COLORREF,
    pub editor_bg: COLORREF,
    pub editor_text: COLORREF,
    pub footer_bg: COLORREF,
    pub hover_bg: COLORREF,
    pub pressed_bg: COLORREF,
    pub error_bg: COLORREF,
    pub error_icon: COLORREF,
    pub swatch_border: COLORREF,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                dark,
                window_bg: rgb(0x20, 0x20, 0x20),
                text: rgb(0xFF, 0xFF, 0xFF),
                subtle_text: rgb(0xC8, 0xC8, 0xC8),
                divider: rgb(0x3A, 0x3A, 0x3A),
                editor_bg: rgb(0x1E, 0x1E, 0x1E),
                editor_text: rgb(0xE6, 0xE6, 0xE6),
                footer_bg: rgb(0x2B, 0x2B, 0x2B),
                hover_bg: rgb(0x3A, 0x3A, 0x3A),
                pressed_bg: rgb(0x30, 0x30, 0x30),
                error_bg: rgb(0x44, 0x27, 0x26),
                error_icon: rgb(0xFF, 0x99, 0xA4),
                swatch_border: rgb(0x80, 0x80, 0x80),
            }
        } else {
            Self {
                dark,
                window_bg: rgb(0xF3, 0xF3, 0xF3),
                text: rgb(0x1A, 0x1A, 0x1A),
                subtle_text: rgb(0x5C, 0x5C, 0x5C),
                divider: rgb(0xE0, 0xE0, 0xE0),
                editor_bg: rgb(0xFF, 0xFF, 0xFF),
                editor_text: rgb(0x1A, 0x1A, 0x1A),
                footer_bg: rgb(0xF9, 0xF9, 0xF9),
                hover_bg: rgb(0xE5, 0xE5, 0xE5),
                pressed_bg: rgb(0xD8, 0xD8, 0xD8),
                error_bg: rgb(0xFD, 0xE7, 0xE9),
                error_icon: rgb(0xC4, 0x2B, 0x1C),
                swatch_border: rgb(0x80, 0x80, 0x80),
            }
        }
    }
}

/// Whether Windows is set to dark mode for apps.
pub fn system_prefers_dark() -> bool {
    let mut value: u32 = 1;
    let mut size = size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    r.is_ok() && value == 0
}

/// What uxtheme's undocumented ordinal 135 is on a given build: its meaning changed in
/// 1903, and calling it with the other signature would pass the wrong argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppModeApi {
    /// 1903 (18362) and later: SetPreferredAppMode(AppMode); 2 = ForceDark, 3 = ForceLight.
    PreferredAppMode,
    /// 1809 (17763): AllowDarkModeForApp(BOOL).
    AllowDarkModeForApp,
    /// Older, or unknown: the ordinals are not the functions this code means.
    None,
}

fn app_mode_api(build: u32) -> AppModeApi {
    match build {
        18362.. => AppModeApi::PreferredAppMode,
        17763.. => AppModeApi::AllowDarkModeForApp,
        _ => AppModeApi::None,
    }
}

/// The Windows build number, from the registry (GetVersionEx's answer depends on the
/// manifest, and RtlGetVersion would need ntdll bindings for one number).
fn windows_build() -> Option<u32> {
    let mut buf = [0u16; 32];
    let mut size = size_of_val(&buf) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if let Err(e) = r.ok() {
        crate::log::error("windows_build", &e);
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len]).parse().ok()
}

/// uxtheme's dark-mode ordinals, resolved once for the process — only on builds where
/// they are known to be the functions they are called as.
struct DarkModeApi {
    app_mode: AppModeApi,
    /// 135: SetPreferredAppMode or AllowDarkModeForApp, as `app_mode` says.
    set_app_mode: Option<unsafe extern "system" fn(i32) -> i32>,
    /// 136: FlushMenuThemes().
    flush_menu_themes: Option<unsafe extern "system" fn()>,
    /// 133: AllowDarkModeForWindow(HWND, BOOL).
    allow_for_window: Option<unsafe extern "system" fn(HWND, BOOL) -> BOOL>,
}

fn dark_mode_api() -> &'static DarkModeApi {
    static API: OnceLock<DarkModeApi> = OnceLock::new();
    API.get_or_init(|| {
        let app_mode = windows_build().map_or(AppModeApi::None, app_mode_api);
        let none = DarkModeApi { app_mode, set_app_mode: None, flush_menu_themes: None, allow_for_window: None };
        if app_mode == AppModeApi::None {
            crate::log::error("dark_mode_api", "unknown Windows build; menus and scrollbars stay light");
            return none;
        }
        // Never freed: the pointers are used for the life of the process.
        let module = match unsafe { LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) } {
            Ok(m) => m,
            Err(e) => {
                crate::log::error("dark_mode_api LoadLibraryExW", &e);
                return none;
            }
        };
        let ordinal = |n: u16| unsafe { GetProcAddress(module, PCSTR(n as usize as *const u8)) };
        // SAFETY: on the builds `app_mode_api` admits, these ordinals have exactly these
        // signatures (both meanings of 135 take one 32-bit argument and return one).
        unsafe {
            DarkModeApi {
                app_mode,
                set_app_mode: ordinal(135).map(|f| std::mem::transmute(f)),
                flush_menu_themes: ordinal(136).map(|f| std::mem::transmute(f)),
                allow_for_window: ordinal(133).map(|f| std::mem::transmute(f)),
            }
        }
    })
}

/// Makes popup menus, context menus and scrollbars of this process follow `dark`.
pub fn set_app_dark_mode(dark: bool) {
    let api = dark_mode_api();
    unsafe {
        if let Some(set) = api.set_app_mode {
            match api.app_mode {
                AppModeApi::PreferredAppMode => set(if dark { 2 } else { 3 }),
                AppModeApi::AllowDarkModeForApp => set(dark.into()),
                AppModeApi::None => 0,
            };
        }
        if let Some(flush) = api.flush_menu_themes {
            flush();
        }
    }
}

pub fn allow_dark_for_window(hwnd: HWND, dark: bool) {
    if let Some(allow) = dark_mode_api().allow_for_window {
        // Returns whether it applied, which the window's own theme call then shows anyway.
        let _ = unsafe { allow(hwnd, dark.into()) };
    }
}

pub fn set_title_bar_dark(hwnd: HWND, dark: bool) {
    let value = BOOL::from(dark);
    let r = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &value as *const BOOL as *const c_void,
            size_of::<BOOL>() as u32,
        )
    };
    crate::log::if_err("DwmSetWindowAttribute", r);
}

/// Explorer-style visual theme for a control, in the dark variant when `dark`.
pub fn set_control_theme(hwnd: HWND, dark: bool, dark_class: &str) {
    let class = crate::win::wide(if dark { dark_class } else { "Explorer" });
    allow_dark_for_window(hwnd, dark);
    unsafe {
        crate::log::if_err("SetWindowTheme", SetWindowTheme(hwnd, crate::win::pcwstr(&class), None));
    }
}

pub fn fill(hdc: HDC, rect: &RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(hdc, rect, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

// ---- Menu bar painting (dark mode only) -------------------------------------------

const WM_UAHDRAWMENU: u32 = 0x0091;
const WM_UAHDRAWMENUITEM: u32 = 0x0092;
const ODS_SELECTED: u32 = 0x0001;
const ODS_GRAYED: u32 = 0x0002;
const ODS_DISABLED: u32 = 0x0004;
const ODS_HOTLIGHT: u32 = 0x0040;
const ODS_INACTIVE: u32 = 0x0080;
const ODS_NOACCEL: u32 = 0x0100;

#[repr(C)]
struct UahMenu {
    hmenu: HMENU,
    hdc: HDC,
    flags: u32,
}

#[repr(C)]
struct UahMenuItem {
    position: i32,
    metrics: [u32; 8],
    popup_metrics: [u32; 5],
}

#[repr(C)]
struct UahDrawMenuItem {
    dis: DRAWITEMSTRUCT,
    um: UahMenu,
    umi: UahMenuItem,
}

fn menu_bar_rect_in_window(hwnd: HWND) -> Option<RECT> {
    unsafe {
        let mut mbi = MENUBARINFO { cbSize: size_of::<MENUBARINFO>() as u32, ..Default::default() };
        GetMenuBarInfo(hwnd, OBJID_MENU, 0, &mut mbi).ok()?;
        let mut win = RECT::default();
        GetWindowRect(hwnd, &mut win).ok()?;
        let mut r = mbi.rcBar;
        r.left -= win.left;
        r.right -= win.left;
        r.top -= win.top;
        r.bottom -= win.top;
        Some(r)
    }
}

/// Paints the menu bar in `palette` when it is dark. Returns `Some` when the message
/// was fully handled.
pub fn handle_menu_bar_message(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    palette: &Palette,
    font: HFONT,
) -> Option<LRESULT> {
    if !palette.dark {
        return None;
    }
    unsafe {
        match msg {
            WM_UAHDRAWMENU => {
                let um = &*(lparam.0 as *const UahMenu);
                if let Some(r) = menu_bar_rect_in_window(hwnd) {
                    fill(um.hdc, &r, palette.window_bg);
                }
                Some(LRESULT(0))
            }
            WM_UAHDRAWMENUITEM => {
                let item = &*(lparam.0 as *const UahDrawMenuItem);
                let mut buf = [0u16; 256];
                let mut mii = MENUITEMINFOW {
                    cbSize: size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_STRING,
                    dwTypeData: PWSTR(buf.as_mut_ptr()),
                    cch: (buf.len() - 1) as u32,
                    ..Default::default()
                };
                crate::log::if_err("GetMenuItemInfoW", GetMenuItemInfoW(item.um.hmenu, item.umi.position as u32, true, &mut mii));
                let text_len = buf.iter().position(|&c| c == 0).unwrap_or(0);

                let state = item.dis.itemState.0;
                let bg = if state & (ODS_HOTLIGHT | ODS_SELECTED) != 0 { palette.hover_bg } else { palette.window_bg };
                let fg = if state & (ODS_INACTIVE | ODS_GRAYED | ODS_DISABLED) != 0 { palette.subtle_text } else { palette.text };
                let mut rc = item.dis.rcItem;
                fill(item.um.hdc, &rc, bg);
                let old = SelectObject(item.um.hdc, HGDIOBJ(font.0));
                SetBkMode(item.um.hdc, TRANSPARENT);
                SetTextColor(item.um.hdc, fg);
                let mut flags = DT_CENTER | DT_SINGLELINE | DT_VCENTER;
                if state & ODS_NOACCEL != 0 {
                    flags |= DT_HIDEPREFIX;
                }
                DrawTextW(item.um.hdc, &mut buf[..text_len], &mut rc, flags);
                SelectObject(item.um.hdc, old);
                Some(LRESULT(0))
            }
            // The non-client painter draws a 1px light line under the menu bar that no
            // UAH message covers; paint over it after the default painting.
            0x0085 /* WM_NCPAINT */ | 0x0086 /* WM_NCACTIVATE */ => {
                let result = DefWindowProcW(hwnd, msg, wparam, lparam);
                let mut client = RECT::default();
                crate::log::if_err("GetClientRect", GetClientRect(hwnd, &mut client));
                let mut pts = [windows::Win32::Foundation::POINT { x: client.left, y: client.top }];
                MapWindowPoints(Some(hwnd), None, &mut pts);
                let mut win = RECT::default();
                crate::log::if_err("GetWindowRect", GetWindowRect(hwnd, &mut win));
                let top = pts[0].y - win.top;
                let line = RECT { left: pts[0].x - win.left, top: top - 1, right: pts[0].x - win.left + client.right, bottom: top };
                let hdc = GetWindowDC(Some(hwnd));
                fill(hdc, &line, palette.window_bg);
                ReleaseDC(Some(hwnd), hdc);
                Some(result)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_mode_ordinals_are_only_trusted_on_known_builds() {
        assert_eq!(app_mode_api(17762), AppModeApi::None);
        assert_eq!(app_mode_api(17763), AppModeApi::AllowDarkModeForApp); // 1809
        assert_eq!(app_mode_api(18361), AppModeApi::AllowDarkModeForApp);
        assert_eq!(app_mode_api(18362), AppModeApi::PreferredAppMode); // 1903
        assert_eq!(app_mode_api(26100), AppModeApi::PreferredAppMode);
    }
}
