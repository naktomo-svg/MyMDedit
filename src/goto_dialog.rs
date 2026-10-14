//! The 行へ移動 dialog: a hand-built modal window with one number field.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_WINDOW, CreateSolidBrush, DeleteObject, EndPaint, HBRUSH, HDC, HFONT, HGDIOBJ, PAINTSTRUCT,
    SetBkColor, SetTextColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::EM_SETSEL;
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetActiveWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::theme::{Palette, fill, set_control_theme};
use crate::win::{message_font, pcwstr, scale, send, wide, window_text};

const IDOK_: i32 = 1;
const IDCANCEL_: i32 = 2;
const IDC_LINE: i32 = 10;
const EN_CHANGE_: u32 = 0x0300;

/// The 1-based line `input` names, clamped to `1..=line_count`; `None` unless it is a
/// whole number (ASCII or full-width digits, surrounding spaces allowed).
pub fn parse_line_number(input: &str, line_count: usize) -> Option<usize> {
    let digits: String = input
        .trim()
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
            c => c,
        })
        .collect();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Too many digits for usize is still "past the end", not an error.
    let n = digits.parse::<usize>().unwrap_or(usize::MAX);
    Some(n.clamp(1, line_count.max(1)))
}

struct DlgState {
    palette: Palette,
    line_count: usize,
    done: Cell<bool>,
    result: RefCell<Option<usize>>,
    bg_brush: HBRUSH,
    field_brush: HBRUSH,
}

impl Drop for DlgState {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.bg_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.field_brush.0));
        }
    }
}

/// Deletes the dialog's font on every exit path, including the early `?` returns.
struct Font(HFONT);

impl Drop for Font {
    fn drop(&mut self) {
        crate::win::delete_font(self.0);
    }
}

fn item(dlg: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(Some(dlg), id).unwrap_or_default() }
}

/// Asks for a line number; `Some` holds the chosen 1-based line, already clamped.
pub fn show(owner: HWND, current_line: usize, line_count: usize, palette: Palette) -> windows::core::Result<Option<usize>> {
    let state = Box::new(DlgState {
        palette,
        line_count,
        done: Cell::new(false),
        result: RefCell::new(None),
        bg_brush: unsafe { CreateSolidBrush(palette.window_bg) },
        field_brush: unsafe { CreateSolidBrush(palette.editor_bg) },
    });

    let dpi = crate::win::dpi_of(owner);
    let s = |v: i32| scale(v, dpi);
    let font = Font(message_font(dpi, false));
    let (client_w, client_h) = (300, 118);
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let ex = WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT;

    let hwnd = unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("MyMDeditGotoDialog"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as usize as *mut _),
            ..Default::default()
        };
        RegisterClassW(&wc); // A second registration fails harmlessly.

        let mut outer = RECT { left: 0, top: 0, right: s(client_w), bottom: s(client_h) };
        let _ = AdjustWindowRectExForDpi(&mut outer, style, false, ex, dpi);
        let (w, h) = (outer.right - outer.left, outer.bottom - outer.top);
        let mut owner_rect = RECT::default();
        let _ = GetWindowRect(owner, &mut owner_rect);
        let x = owner_rect.left + ((owner_rect.right - owner_rect.left) - w) / 2;
        let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top) - h) / 2;

        CreateWindowExW(ex, w!("MyMDeditGotoDialog"), w!("行へ移動"), style, x, y, w, h, Some(owner), None, Some(instance.into()), None)?
    };
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*state as *const DlgState as isize) };
    crate::theme::set_title_bar_dark(hwnd, palette.dark);

    let make = |class: PCWSTR, text: &str, style: u32, ex: u32, id: i32, x: i32, y: i32, cx: i32, cy: i32| -> HWND {
        let text = wide(text);
        unsafe {
            let h = CreateWindowExW(
                WINDOW_EX_STYLE(ex),
                class,
                pcwstr(&text),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                s(x),
                s(y),
                s(cx),
                s(cy),
                Some(hwnd),
                Some(HMENU(id as isize as *mut _)),
                None,
                None,
            )
            .unwrap_or_default();
            send(h, WM_SETFONT, font.0.0 as usize, 1);
            h
        }
    };
    let tab = WS_TABSTOP.0;
    make(w!("STATIC"), &format!("行番号 (1 - {line_count})"), 0, 0, -1, 16, 14, 268, 20);
    let edit = make(w!("EDIT"), &current_line.to_string(), tab | ES_AUTOHSCROLL as u32, WS_EX_CLIENTEDGE.0, IDC_LINE, 16, 38, 268, 24);
    let ok = make(w!("BUTTON"), "移動", tab | BS_DEFPUSHBUTTON as u32, 0, IDOK_, 84, 76, 96, 30);
    let cancel = make(w!("BUTTON"), "キャンセル", tab | BS_PUSHBUTTON as u32, 0, IDCANCEL_, 188, 76, 96, 30);
    set_control_theme(edit, palette.dark, "DarkMode_CFD");
    for h in [ok, cancel] {
        set_control_theme(h, palette.dark, "DarkMode_Explorer");
    }

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetFocus(Some(edit));
        send(edit, EM_SETSEL, 0, -1);
    }

    // ---- modal loop ----
    unsafe {
        let _ = EnableWindow(owner, false);
        let mut msg = MSG::default();
        while !state.done.get() {
            match GetMessageW(&mut msg, None, 0, 0).0 {
                0 => {
                    // Re-post WM_QUIT so the main loop sees it too.
                    PostQuitMessage(msg.wParam.0 as i32);
                    break;
                }
                -1 => break,
                _ => {
                    if !IsDialogMessageW(hwnd, &msg).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        }
        let _ = EnableWindow(owner, true);
        let _ = SetActiveWindow(owner);
        let _ = DestroyWindow(hwnd);
    }
    Ok(state.result.borrow_mut().take())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const DlgState;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &*ptr };
    let p = &state.palette;
    let parsed = || parse_line_number(&window_text(item(hwnd, IDC_LINE)), state.line_count);
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            match id {
                // IsDialogMessage sends IDOK for Enter even while the button is disabled.
                IDOK_ => {
                    if let Some(line) = parsed() {
                        *state.result.borrow_mut() = Some(line);
                        state.done.set(true);
                    }
                }
                IDCANCEL_ => state.done.set(true),
                IDC_LINE if code == EN_CHANGE_ => unsafe {
                    let _ = EnableWindow(item(hwnd, IDOK_), parsed().is_some());
                },
                _ => {}
            }
            LRESULT(0)
        }
        DM_GETDEFID => LRESULT(((DC_HASDEFID as isize) << 16) | IDOK_ as isize),
        WM_CLOSE => {
            state.done.set(true);
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            let mut rc = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut rc);
            }
            fill(HDC(wparam.0 as *mut _), &rc, p.window_bg);
            LRESULT(1)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => unsafe {
            let hdc = HDC(wparam.0 as *mut _);
            SetTextColor(hdc, p.text);
            SetBkColor(hdc, p.window_bg);
            LRESULT(state.bg_brush.0 as isize)
        },
        WM_CTLCOLOREDIT => unsafe {
            let hdc = HDC(wparam.0 as *mut _);
            SetTextColor(hdc, p.editor_text);
            SetBkColor(hdc, p.editor_bg);
            LRESULT(state.field_brush.0 as isize)
        },
        WM_PAINT => unsafe {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        },
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        },
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_number_is_parsed_and_clamped() {
        assert_eq!(parse_line_number(" 12 ", 50), Some(12));
        // Full-width digits, as an IME left on types them.
        assert_eq!(parse_line_number("１２", 50), Some(12));
        assert_eq!(parse_line_number("0", 50), Some(1));
        assert_eq!(parse_line_number("51", 50), Some(50));
        assert_eq!(parse_line_number("99999999999999999999999", 50), Some(50));
    }

    #[test]
    fn line_number_rejects_non_numbers() {
        for input in ["", "  ", "abc", "-3", "1.5", "1 2"] {
            assert_eq!(parse_line_number(input, 50), None, "{input:?}");
        }
    }
}
