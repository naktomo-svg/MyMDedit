//! The 設定 dialog: a hand-built modal window of standard controls.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_WINDOW, CreateSolidBrush, DeleteObject, EndPaint, FrameRect, HBRUSH, HDC, HFONT, HGDIOBJ,
    PAINTSTRUCT, SetBkColor, SetTextColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, UDM_SETBUDDY, UDM_SETPOS32, UDM_SETRANGE32, UDS_ALIGNRIGHT, UDS_ARROWKEYS, UDS_NOTHOUSANDS,
    UDS_SETBUDDYINT,
};
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetActiveWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::dialogs::{
    CustomColors, choose_color, custom_color_slots, custom_colors_to_save, format_hex_color, parse_hex_color,
};
use crate::log;
use crate::settings::{AppSettings, Theme, limits};
use crate::theme::{Palette, fill, set_control_theme};
use crate::win::{message_font, pcwstr, scale, send, wide, window_text};

const SS_OWNERDRAW: u32 = 0x000D;
const IDOK_: i32 = 1;
const IDCANCEL_: i32 = 2;
const IDC_THEME: i32 = 10;
const IDC_EDITOR_FONT: i32 = 11;
const IDC_EDITOR_SIZE: i32 = 12;
const IDC_EDITOR_SIZE_UD: i32 = 13;
const IDC_PREVIEW_FONT: i32 = 14;
const IDC_PREVIEW_SIZE: i32 = 15;
const IDC_PREVIEW_SIZE_UD: i32 = 16;
const IDC_BG_CHECK: i32 = 17;
const IDC_BG_BUTTON: i32 = 18;
const IDC_SWATCH: i32 = 19;
const IDC_TABLE_HEADER: i32 = 20;
const IDC_TABLE_STRIPE: i32 = 21;

/// The theme combo box's items, in order: both directions read this one table.
const THEMES: [(Theme, &str); 3] =
    [(Theme::System, "システムの既定"), (Theme::Light, "ライト"), (Theme::Dark, "ダーク")];

/// What the dialog edits. Applied onto the settings as of the moment 保存 was pressed
/// (see [`DialogEdits::apply`]), not as of when the dialog opened: the color dialog
/// reached from 「色を選択」 persists custom colors while this dialog is still open.
pub struct DialogEdits {
    theme: Theme,
    /// `None` when the field was left empty.
    editor_font: Option<String>,
    editor_size: Option<f64>,
    preview_font: Option<String>,
    preview_size: Option<f64>,
    background: Option<String>,
    table_header: bool,
    table_striping: bool,
}

impl DialogEdits {
    pub fn apply(self, current: &AppSettings) -> AppSettings {
        AppSettings {
            theme: self.theme,
            // A field left empty, or one that no longer holds a number, falls back to that
            // setting's own previous value; out-of-range values are clamped.
            editor_font_family: self.editor_font.unwrap_or_else(|| current.editor_font_family.clone()),
            editor_font_size: self.editor_size.map_or(current.editor_font_size, limits::clamp_font_size),
            preview_font_family: self.preview_font.unwrap_or_else(|| current.preview_font_family.clone()),
            preview_font_size: self.preview_size.map_or(current.preview_font_size, limits::clamp_font_size),
            preview_background_color: self.background,
            table_header_gray_background: self.table_header,
            table_row_striping: self.table_striping,
            ..current.clone()
        }
    }
}

impl Drop for DlgState {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.bg_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.field_brush.0));
        }
    }
}

/// Deletes the dialog's fonts on every exit path, including the early `?` returns.
struct Fonts(HFONT, HFONT);

impl Drop for Fonts {
    fn drop(&mut self) {
        crate::win::delete_font(self.0);
        crate::win::delete_font(self.1);
    }
}

struct DlgState {
    palette: Palette,
    done: Cell<bool>,
    result: RefCell<Option<DialogEdits>>,
    bg_color: Cell<COLORREF>,
    custom: RefCell<CustomColors>,
    on_custom_colors: Box<dyn Fn(Vec<String>)>,
    bg_brush: HBRUSH,
    field_brush: HBRUSH,
}

fn item(dlg: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(Some(dlg), id).unwrap_or_default() }
}

fn checked(dlg: HWND, id: i32) -> bool {
    send(item(dlg, id), BM_GETCHECK, 0, 0) == 1
}

fn parse_size(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn format_size(v: f64) -> String {
    if v.fract() == 0.0 { format!("{}", v as i64) } else { format!("{v}") }
}

pub fn show(
    owner: HWND,
    current: &AppSettings,
    fonts: &[String],
    palette: Palette,
    on_custom_colors: Box<dyn Fn(Vec<String>)>,
) -> windows::core::Result<Option<DialogEdits>> {
    let custom = custom_color_slots(&current.custom_colors);
    let state = Box::new(DlgState {
        palette,
        done: Cell::new(false),
        result: RefCell::new(None),
        bg_color: Cell::new(
            current
                .preview_background_color
                .as_deref()
                .and_then(parse_hex_color)
                .unwrap_or(crate::win::rgb(255, 255, 255)),
        ),
        custom: RefCell::new(custom),
        on_custom_colors,
        bg_brush: unsafe { CreateSolidBrush(palette.window_bg) },
        field_brush: unsafe { CreateSolidBrush(palette.editor_bg) },
    });

    let dpi = crate::win::dpi_of(owner);
    let s = |v: i32| scale(v, dpi);
    let ui_font = message_font(dpi, false);
    let bold_font = message_font(dpi, true);
    let _fonts = Fonts(ui_font, bold_font);

    let hwnd = unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("MyMDeditSettingsDialog"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as usize as *mut _),
            ..Default::default()
        };
        RegisterClassW(&wc); // A second registration fails harmlessly.

        let client = RECT { left: 0, top: 0, right: s(420), bottom: s(560) };
        let mut outer = client;
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT;
        crate::log::if_err("AdjustWindowRectExForDpi", AdjustWindowRectExForDpi(&mut outer, style, false, ex, dpi));
        let (w, h) = (outer.right - outer.left, outer.bottom - outer.top);
        let mut owner_rect = RECT::default();
        crate::log::if_err("GetWindowRect", GetWindowRect(owner, &mut owner_rect));
        let x = owner_rect.left + ((owner_rect.right - owner_rect.left) - w) / 2;
        let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top) - h) / 2;

        CreateWindowExW(
            ex,
            w!("MyMDeditSettingsDialog"),
            w!("設定"),
            style,
            x,
            y,
            w,
            h,
            Some(owner),
            None,
            Some(instance.into()),
            None,
        )?
    };
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*state as *const DlgState as isize) };
    crate::theme::set_title_bar_dark(hwnd, palette.dark);

    // ---- controls ----
    // The first control that could not be created. A missing control reads back as an
    // empty font name, システムの既定 and unchecked boxes, which 保存 would then store; so
    // a dialog missing any is not shown at all.
    let failed: RefCell<Option<windows::core::Error>> = RefCell::new(None);
    let make =
        |class: PCWSTR, text: &str, style: u32, ex: u32, id: i32, x: i32, y: i32, cx: i32, cy: i32, f: HFONT| -> HWND {
            let text = wide(text);
            unsafe {
                let created = CreateWindowExW(
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
                );
                let h = created.unwrap_or_else(|e| {
                    failed.borrow_mut().get_or_insert(e);
                    HWND::default()
                });
                send(h, WM_SETFONT, f.0 as usize, 1);
                h
            }
        };
    let label = |text: &str, x: i32, y: i32, cx: i32, bold: bool| {
        make(w!("STATIC"), text, 0, 0, -1, x, y, cx, 20, if bold { bold_font } else { ui_font })
    };
    let tab = WS_TABSTOP.0;
    let left = 20;
    let full = 380;
    let mut y = 16;

    label("テーマ", left, y, full, true);
    y += 22;
    let theme = make(
        w!("COMBOBOX"),
        "",
        tab | CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0,
        0,
        IDC_THEME,
        left,
        y,
        full,
        200,
        ui_font,
    );
    for (_, t) in THEMES {
        let t = wide(t);
        send(theme, CB_ADDSTRING, 0, t.as_ptr() as isize);
    }
    send(theme, CB_SETCURSEL, THEMES.iter().position(|(t, _)| *t == current.theme).unwrap_or(0), 0);
    y += 30 + 14;

    let font_section =
        |title: &str, (font_id, size_id, updown_id): (i32, i32, i32), family: &str, size: f64, y: &mut i32| {
            label(title, left, *y, full, true);
            *y += 22;
            let combo = make(
                w!("COMBOBOX"),
                "",
                tab | (CBS_DROPDOWN | CBS_AUTOHSCROLL) as u32 | WS_VSCROLL.0,
                0,
                font_id,
                left,
                *y,
                full,
                300,
                ui_font,
            );
            for f in fonts {
                let f = wide(f);
                send(combo, CB_ADDSTRING, 0, f.as_ptr() as isize);
            }
            let family_w = wide(family);
            send(combo, WM_SETTEXT, 0, family_w.as_ptr() as isize);
            *y += 32;
            label("サイズ", left, *y + 3, 50, false);
            let edit = make(
                w!("EDIT"),
                &format_size(size),
                tab | ES_AUTOHSCROLL as u32,
                WS_EX_CLIENTEDGE.0,
                size_id,
                left + 56,
                *y,
                80,
                24,
                ui_font,
            );
            let updown = make(
                w!("msctls_updown32"),
                "",
                UDS_SETBUDDYINT | UDS_ALIGNRIGHT | UDS_ARROWKEYS | UDS_NOTHOUSANDS,
                0,
                updown_id,
                0,
                0,
                0,
                0,
                ui_font,
            );
            send(updown, UDM_SETBUDDY, edit.0 as usize, 0);
            send(updown, UDM_SETRANGE32, limits::MIN_FONT_SIZE as usize, limits::MAX_FONT_SIZE as isize);
            send(updown, UDM_SETPOS32, 0, size.round() as isize);
            // Set after attaching: UDS_SETBUDDYINT rewrites the buddy as an integer on
            // attach, which would round a fractional size loaded from settings.
            let text = wide(&format_size(size));
            send(edit, WM_SETTEXT, 0, text.as_ptr() as isize);
            *y += 24 + 18;
            (combo, edit, updown)
        };
    let (efont, esize, eud) = font_section(
        "エディタのフォント",
        (IDC_EDITOR_FONT, IDC_EDITOR_SIZE, IDC_EDITOR_SIZE_UD),
        &current.editor_font_family,
        current.editor_font_size,
        &mut y,
    );
    let (pfont, psize, pud) = font_section(
        "プレビューのフォント",
        (IDC_PREVIEW_FONT, IDC_PREVIEW_SIZE, IDC_PREVIEW_SIZE_UD),
        &current.preview_font_family,
        current.preview_font_size,
        &mut y,
    );

    label("プレビュー背景色", left, y, full, true);
    y += 22;
    let bg_check = make(
        w!("BUTTON"),
        "背景色をカスタマイズする",
        tab | BS_AUTOCHECKBOX as u32,
        0,
        IDC_BG_CHECK,
        left,
        y,
        full,
        22,
        ui_font,
    );
    let use_bg = current.preview_background_color.is_some();
    send(bg_check, BM_SETCHECK, use_bg as usize, 0);
    y += 26;
    let bg_button =
        make(w!("BUTTON"), "色を選択", tab | BS_PUSHBUTTON as u32, 0, IDC_BG_BUTTON, left, y, 100, 28, ui_font);
    unsafe {
        let _ = EnableWindow(bg_button, use_bg);
    }
    make(w!("STATIC"), "", SS_OWNERDRAW, 0, IDC_SWATCH, left + 110, y, 28, 28, ui_font);
    y += 28 + 18;

    label("表(テーブル)", left, y, full, true);
    y += 22;
    let header = make(
        w!("BUTTON"),
        "ヘッダー行をグレー背景にする",
        tab | BS_AUTOCHECKBOX as u32,
        0,
        IDC_TABLE_HEADER,
        left,
        y,
        full,
        22,
        ui_font,
    );
    send(header, BM_SETCHECK, current.table_header_gray_background as usize, 0);
    y += 26;
    let stripe = make(
        w!("BUTTON"),
        "データ行を1行おきにグレー背景にする(縞模様)",
        tab | BS_AUTOCHECKBOX as u32,
        0,
        IDC_TABLE_STRIPE,
        left,
        y,
        full,
        22,
        ui_font,
    );
    send(stripe, BM_SETCHECK, current.table_row_striping as usize, 0);
    y += 26 + 22;

    let ok = make(w!("BUTTON"), "保存", tab | BS_DEFPUSHBUTTON as u32, 0, IDOK_, left + full - 200, y, 96, 30, ui_font);
    let cancel = make(
        w!("BUTTON"),
        "キャンセル",
        tab | BS_PUSHBUTTON as u32,
        0,
        IDCANCEL_,
        left + full - 96,
        y,
        96,
        30,
        ui_font,
    );
    y += 30 + 18;

    if let Some(e) = failed.take() {
        unsafe {
            log::if_err("SettingsDialog DestroyWindow", DestroyWindow(hwnd));
        }
        return Err(e);
    }

    // Theme the controls.
    for h in [theme, efont, pfont] {
        set_control_theme(h, palette.dark, "DarkMode_CFD");
    }
    for h in [bg_button, ok, cancel, eud, pud] {
        set_control_theme(h, palette.dark, "DarkMode_Explorer");
    }
    for h in [esize, psize] {
        set_control_theme(h, palette.dark, "DarkMode_CFD");
    }
    if palette.dark {
        // A visual-styled checkbox draws its label in the theme's (black) text color and
        // ignores WM_CTLCOLORBTN; unthemed, it honours the color we hand it.
        for h in [bg_check, header, stripe] {
            unsafe {
                crate::log::if_err("SetWindowTheme", windows::Win32::UI::Controls::SetWindowTheme(h, w!(""), w!("")));
            }
        }
    }

    // Fit the window to the content.
    unsafe {
        let mut rc = RECT { left: 0, top: 0, right: s(420), bottom: s(y) };
        crate::log::if_err(
            "AdjustWindowRectExForDpi",
            AdjustWindowRectExForDpi(
                &mut rc,
                WS_POPUP | WS_CAPTION | WS_SYSMENU,
                false,
                WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT,
                dpi,
            ),
        );
        crate::log::if_err(
            "SetWindowPos",
            SetWindowPos(hwnd, None, 0, 0, rc.right - rc.left, rc.bottom - rc.top, SWP_NOMOVE | SWP_NOZORDER),
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetFocus(Some(theme));
    }

    // ---- modal loop ----
    unsafe {
        let _ = EnableWindow(owner, false);
        let mut msg = MSG::default();
        // Set when the loop could not go on: told to the caller rather than passed off as
        // キャンセル.
        let mut loop_error = None;
        while !state.done.get() {
            match GetMessageW(&mut msg, None, 0, 0).0 {
                0 => {
                    // Re-post WM_QUIT so the main loop sees it too.
                    PostQuitMessage(msg.wParam.0 as i32);
                    break;
                }
                -1 => {
                    loop_error = Some(windows::core::Error::from_thread());
                    break;
                }
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
        crate::log::if_err("DestroyWindow", DestroyWindow(hwnd));
        if let Some(e) = loop_error {
            return Err(e);
        }
    }
    Ok(state.result.borrow_mut().take())
}

fn collect(dlg: HWND, state: &DlgState) -> DialogEdits {
    let index = usize::try_from(send(item(dlg, IDC_THEME), CB_GETCURSEL, 0, 0)).ok();
    let theme = index.and_then(|i| THEMES.get(i)).map_or(Theme::System, |(t, _)| *t);
    let font = |id| Some(window_text(item(dlg, id)).trim().to_string()).filter(|f| !f.is_empty());
    DialogEdits {
        theme,
        editor_font: font(IDC_EDITOR_FONT),
        editor_size: parse_size(&window_text(item(dlg, IDC_EDITOR_SIZE))),
        preview_font: font(IDC_PREVIEW_FONT),
        preview_size: parse_size(&window_text(item(dlg, IDC_PREVIEW_SIZE))),
        background: checked(dlg, IDC_BG_CHECK).then(|| format_hex_color(state.bg_color.get())),
        table_header: checked(dlg, IDC_TABLE_HEADER),
        table_striping: checked(dlg, IDC_TABLE_STRIPE),
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const DlgState;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &*ptr };
    let p = &state.palette;
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            match id {
                IDOK_ => {
                    *state.result.borrow_mut() = Some(collect(hwnd, state));
                    state.done.set(true);
                }
                IDCANCEL_ => state.done.set(true),
                IDC_BG_CHECK => unsafe {
                    let _ = EnableWindow(item(hwnd, IDC_BG_BUTTON), checked(hwnd, IDC_BG_CHECK));
                },
                IDC_BG_BUTTON => {
                    let mut custom = *state.custom.borrow();
                    let chosen = choose_color(hwnd, state.bg_color.get(), &mut custom);
                    *state.custom.borrow_mut() = custom;
                    // Persisted the moment the color dialog closes, whatever the outcome
                    // of this dialog — matching the native dialog keeping its palette.
                    (state.on_custom_colors)(custom_colors_to_save(&custom));
                    let chosen = match chosen {
                        Ok(c) => c,
                        Err(e) => {
                            log::error("choose_color", &e);
                            let text = wide("色の選択画面を開けませんでした。");
                            let caption = wide("MyMDedit");
                            unsafe {
                                MessageBoxW(Some(hwnd), pcwstr(&text), pcwstr(&caption), MB_OK | MB_ICONERROR);
                            }
                            None
                        }
                    };
                    if let Some(c) = chosen {
                        state.bg_color.set(c);
                        unsafe {
                            let _ =
                                windows::Win32::Graphics::Gdi::InvalidateRect(Some(item(hwnd, IDC_SWATCH)), None, true);
                        }
                    }
                }
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
                crate::log::if_err("GetClientRect", GetClientRect(hwnd, &mut rc));
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
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => unsafe {
            let hdc = HDC(wparam.0 as *mut _);
            SetTextColor(hdc, p.editor_text);
            SetBkColor(hdc, p.editor_bg);
            LRESULT(state.field_brush.0 as isize)
        },
        WM_DRAWITEM => {
            let dis = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if dis.CtlID == IDC_SWATCH as u32 {
                fill(dis.hDC, &dis.rcItem, state.bg_color.get());
                unsafe {
                    let border = CreateSolidBrush(p.swatch_border);
                    FrameRect(dis.hDC, &dis.rcItem, border);
                    let _ = DeleteObject(HGDIOBJ(border.0));
                }
            }
            LRESULT(1)
        }
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
    fn empty_size_falls_back_to_previous_value_and_clamps() {
        let current = AppSettings::default();
        let edits = DialogEdits {
            theme: Theme::Dark,
            editor_font: Some("Consolas".into()),
            editor_size: None,
            preview_font: Some("Meiryo".into()),
            preview_size: Some(100.0),
            background: Some("#112233".into()),
            table_header: true,
            table_striping: false,
        };
        let s = edits.apply(&AppSettings { recent_folders: vec!["C:\\x".into()], ..current.clone() });
        assert_eq!(s.editor_font_size, current.editor_font_size);
        assert_eq!(s.preview_font_size, 48.0);
        assert_eq!(s.recent_folders, vec!["C:\\x"]);
        assert_eq!(s.theme, Theme::Dark);
        assert_eq!(s.editor_font_family, "Consolas");
    }

    #[test]
    fn empty_font_falls_back_to_previous_value() {
        let current = AppSettings { editor_font_family: "Consolas".into(), ..AppSettings::default() };
        let edits = DialogEdits {
            theme: Theme::System,
            editor_font: None,
            editor_size: None,
            preview_font: None,
            preview_size: None,
            background: None,
            table_header: false,
            table_striping: false,
        };
        let s = edits.apply(&current);
        assert_eq!(s.editor_font_family, "Consolas");
        assert_eq!(s.preview_font_family, current.preview_font_family);
    }
}
