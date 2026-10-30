//! The main window: menu bar, Markdown toolbar, error banner, editor, preview, footer —
//! and every command behind them.
//!
//! State lives in `RefCell`s borrowed only for short, local scopes. The rule that keeps
//! that sound: never hold a borrow across a call that can pump messages or send
//! notifications back into this window (any modal dialog, any message sent to the
//! editor, any WebView2 call), because the handlers it re-enters borrow again.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, DT_CENTER, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DrawTextW, EndPaint, GetDC,
    HFONT, HGDIOBJ, InvalidateRect, PAINTSTRUCT, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy};
use windows::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, FINDMSGSTRINGW, FINDREPLACE_FLAGS, FINDREPLACEW, FR_DIALOGTERM, FR_DOWN, FR_FINDNEXT,
    FR_MATCHCASE, FR_REPLACE, FR_REPLACEALL, FR_WHOLEWORD, FindTextW, ReplaceTextW,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ODS_SELECTED, TOOLTIPS_CLASSW, TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_SETMAXTIPWIDTH,
    TTTOOLINFOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ADD, VK_CONTROL, VK_MENU, VK_NUMPAD0,
    VK_OEM_MINUS, VK_OEM_PLUS, VK_RETURN, VK_SHIFT, VK_SUBTRACT, VK_TAB,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, PWSTR, w};

use crate::dialogs::{self, DiscardChoice};
use crate::document::{self, DiskChange, DocumentFormat, FileStamp, OpenedDocument, ReadError};
use crate::editor::Editor;
use crate::goto_dialog;
use crate::line_endings;
use crate::log;
use crate::md_ops::{self, Edit};
use crate::preview::{self, Preview};
use crate::render;
use crate::search;
use crate::settings::{self, AppSettings, SettingsStore, Theme};
use crate::settings_dialog;
use crate::theme::{self, Palette, fill};
use crate::win::{self, group_thousands, hiword, loword, pcwstr, scale, send, wide};

const SAMPLE_CONTENT: &str = "# Hello, Markdown!\n\nここに Markdown を入力すると、右側にプレビューが表示されます。\n\n- リスト項目 1\n- リスト項目 2\n\n```csharp\nConsole.WriteLine(\"Hello, world!\");\n```";
const UNTITLED: &str = "無題.md";

// ---- command ids ----
const ID_NEW: u16 = 100;
const ID_OPEN: u16 = 101;
const ID_SAVE: u16 = 102;
const ID_SAVE_AS: u16 = 103;
const ID_EXIT: u16 = 104;
const ID_UNDO: u16 = 110;
const ID_REDO: u16 = 111;
const ID_CUT: u16 = 112;
const ID_COPY: u16 = 113;
const ID_PASTE: u16 = 114;
const ID_SELECT_ALL: u16 = 115;
const ID_FIND: u16 = 120;
const ID_REPLACE: u16 = 121;
const ID_GOTO_LINE: u16 = 122;
const ID_OUTLINE: u16 = 123;
const ID_SETTINGS: u16 = 130;
const ID_ZOOM_IN: u16 = 140;
const ID_ZOOM_OUT: u16 = 141;
const ID_ZOOM_RESET: u16 = 142;
const ID_BOLD: u16 = 150;
const ID_ITALIC: u16 = 151;
const ID_HEADING: u16 = 152;
const ID_BULLET: u16 = 153;
const ID_NUMBERED: u16 = 154;
const ID_LINK: u16 = 155;
const ID_CODE: u16 = 156;
const ID_MORE: u16 = 157;
const ID_H1: u16 = 160;
const ID_H2: u16 = 161;
const ID_H3: u16 = 162;
const ID_QUOTE: u16 = 170;
const ID_RULE: u16 = 171;
const ID_IMAGE: u16 = 172;
const ID_TABLE: u16 = 173;
const ID_STRIKE: u16 = 174;
const ID_CODE_BLOCK: u16 = 175;
const ID_TASK: u16 = 176;
const ID_ERROR_CLOSE: u16 = 180;
const ID_SHORTCUTS: u16 = 190;
const ID_HELP_GITHUB: u16 = 191;
const ID_HELP_RELEASES: u16 = 192;
const ID_ABOUT: u16 = 193;
const ID_RECENT_BASE: u16 = 200;
/// One command id per recent-folder slot, derived from the cap so the two can't drift.
const ID_RECENT_END: u16 = ID_RECENT_BASE + settings::limits::RECENT_FOLDER_COUNT as u16;
const ID_EDITOR: usize = 300;

const WM_APP_FOLDER_PROBED: u32 = WM_APP + 1;
const WM_APP_RENDER: u32 = WM_APP + 2;
// WM_APP + 3 is preview::WM_APP_PREVIEW_FAILED.
const WM_APP_CHECK_DISK: u32 = WM_APP + 4;
// WM_APP + 5 is preview::WM_APP_PREVIEW_CLOSE.
const TIMER_SCROLL_SYNC: usize = 1;

const ZOOM_STEP: i32 = 10;
const ZOOM_MIN: i32 = 50;
const ZOOM_MAX: i32 = 200;

/// Window-wide shortcuts: (virtual key, ctrl, shift, command). Also consulted when the
/// keyboard focus is inside the WebView, which the accelerator table never sees.
const ACCELERATORS: &[(u16, bool, bool, u16)] = &[
    (b'N' as u16, true, false, ID_NEW),
    (b'O' as u16, true, false, ID_OPEN),
    (b'S' as u16, true, false, ID_SAVE),
    (b'S' as u16, true, true, ID_SAVE_AS),
    (b'F' as u16, true, false, ID_FIND),
    (b'H' as u16, true, false, ID_REPLACE),
    (b'G' as u16, true, false, ID_GOTO_LINE),
    (b'O' as u16, true, true, ID_OUTLINE),
    (VK_RETURN.0, true, false, ID_TASK),
    (VK_ADD.0, true, false, ID_ZOOM_IN),
    // Main-row "+": on some layouts (Japanese included) it needs Shift, and accelerator
    // modifier matching is exact, so register both.
    (VK_OEM_PLUS.0, true, false, ID_ZOOM_IN),
    (VK_OEM_PLUS.0, true, true, ID_ZOOM_IN),
    (VK_SUBTRACT.0, true, false, ID_ZOOM_OUT),
    (VK_OEM_MINUS.0, true, false, ID_ZOOM_OUT),
    (b'0' as u16, true, false, ID_ZOOM_RESET),
    (VK_NUMPAD0.0, true, false, ID_ZOOM_RESET),
];

pub fn accelerator_command(vk: u16, ctrl: bool, shift: bool) -> Option<u16> {
    ACCELERATORS.iter().find(|a| a.0 == vk && a.1 == ctrl && a.2 == shift).map(|a| a.3)
}

/// ヘルプ → ショートカット一覧, as (the accelerator's command, if it is one; what it
/// does; the keys). Every command in ACCELERATORS has a row (a test checks), so a new
/// shortcut cannot go unlisted.
const SHORTCUT_HELP: &[(Option<u16>, &str, &str)] = &[
    (Some(ID_NEW), "新規", "Ctrl+N"),
    (Some(ID_OPEN), "開く", "Ctrl+O"),
    (Some(ID_SAVE), "保存", "Ctrl+S"),
    (Some(ID_SAVE_AS), "名前を付けて保存", "Ctrl+Shift+S"),
    (Some(ID_FIND), "検索", "Ctrl+F"),
    (Some(ID_REPLACE), "置換", "Ctrl+H"),
    (Some(ID_GOTO_LINE), "行へ移動", "Ctrl+G"),
    (Some(ID_OUTLINE), "見出しへ移動", "Ctrl+Shift+O"),
    (Some(ID_TASK), "タスクの切り替え", "Ctrl+Enter"),
    (Some(ID_ZOOM_IN), "ズームイン", "Ctrl++"),
    (Some(ID_ZOOM_OUT), "ズームアウト", "Ctrl+-"),
    (Some(ID_ZOOM_RESET), "ズームを100%に戻す", "Ctrl+0"),
    (None, "終了", "Alt+F4"),
    (None, "元に戻す / やり直し", "Ctrl+Z / Ctrl+Y"),
    (None, "切り取り / コピー / 貼り付け", "Ctrl+X / Ctrl+C / Ctrl+V"),
    (None, "すべて選択", "Ctrl+A"),
    (None, "ズーム(エディタ上)", "Ctrl+マウスホイール"),
    (None, "リストの自動継続", "Enter"),
    (None, "リストの字下げ / 字下げを戻す", "Tab / Shift+Tab"),
];

fn shortcut_help_text() -> String {
    SHORTCUT_HELP.iter().map(|(_, what, keys)| format!("{what}: {keys}")).collect::<Vec<_>>().join("\n")
}

struct ToolButton {
    id: u16,
    label: &'static str,
    font: ButtonFont,
    /// Draws a chevron and opens a menu.
    dropdown: bool,
    width: i32,
    tip: &'static str,
}

#[derive(Clone, Copy, PartialEq)]
enum ButtonFont {
    Icon,
    Bold,
    Normal,
}

const TOOL_BUTTON_COUNT: usize = 8;

const TOOL_BUTTONS: [ToolButton; TOOL_BUTTON_COUNT] = [
    ToolButton {
        id: ID_BOLD,
        label: "\u{E8DD}",
        font: ButtonFont::Icon,
        dropdown: false,
        width: 32,
        tip: "太字 (**太字**)",
    },
    ToolButton {
        id: ID_ITALIC,
        label: "\u{E8DB}",
        font: ButtonFont::Icon,
        dropdown: false,
        width: 32,
        tip: "斜体 (*斜体*)",
    },
    ToolButton { id: ID_HEADING, label: "H", font: ButtonFont::Bold, dropdown: true, width: 44, tip: "見出し" },
    ToolButton {
        id: ID_BULLET,
        label: "\u{E8FD}",
        font: ButtonFont::Icon,
        dropdown: false,
        width: 32,
        tip: "箇条書きリスト (- 項目)",
    },
    ToolButton {
        id: ID_NUMBERED,
        label: "1.",
        font: ButtonFont::Bold,
        dropdown: false,
        width: 32,
        tip: "番号付きリスト (1. 項目)",
    },
    ToolButton {
        id: ID_LINK,
        label: "\u{E71B}",
        font: ButtonFont::Icon,
        dropdown: false,
        width: 32,
        tip: "リンク ([テキスト](URL))",
    },
    ToolButton {
        id: ID_CODE,
        label: "\u{E943}",
        font: ButtonFont::Icon,
        dropdown: false,
        width: 32,
        tip: "コード (`code`)",
    },
    ToolButton {
        id: ID_MORE,
        label: "その他",
        font: ButtonFont::Normal,
        dropdown: true,
        width: 72,
        tip: "その他の記法",
    },
];

#[derive(Clone, Copy, Default)]
struct Fonts {
    ui: HFONT,
    bold: HFONT,
    icon: HFONT,
    icon_small: HFONT,
    menu: HFONT,
}

#[derive(Clone, Copy, Default)]
pub struct Ui {
    hwnd: HWND,
    editor: HWND,
    tooltip: HWND,
    error_close: HWND,
    buttons: [HWND; TOOL_BUTTON_COUNT],
    file_menu: HMENU,
    accel: HACCEL,
    find_dialog: HWND,
    dpi: u32,
    fonts: Fonts,
}

#[derive(Clone, Copy, Default)]
struct Layout {
    toolbar: RECT,
    error: RECT,
    divider: RECT,
    preview: RECT,
    footer: RECT,
}

pub struct State {
    file_path: Option<PathBuf>,
    format: DocumentFormat,
    file_name: String,
    dirty: bool,
    zoom: i32,
    settings: SettingsStore,
    /// What the error banner shows, oldest first.
    errors: Vec<String>,
    palette: Palette,
    html: String,
    /// What the preview's copy buttons copy, by index, and the token their links carry
    /// (`render::parse_copy_link`).
    code_blocks: Vec<String>,
    copy_token: u64,
    lines: usize,
    chars: usize,
    scroll_ratio: f64,
    layout: Layout,
    installed_fonts: Vec<String>,
    icon_face: &'static str,
    recent_items: u32,
    find_is_replace: bool,
    /// The folder a recent-folder click is waiting on the background probe for.
    probing_folder: Option<String>,
    /// The open file as last read, saved or acknowledged, to notice another program
    /// changing it. `None` for an untitled document.
    disk_stamp: Option<FileStamp>,
    /// The file's disappearance has been reported; don't repeat it on every activation.
    missing_reported: bool,
    /// The on-disk version the user just declined to overwrite. Closing that prompt
    /// reactivates the window; asking 読み込み直しますか about the very same change right
    /// after would be a second prompt for one decision.
    declined_overwrite: Option<FileStamp>,
}

/// Find/replace dialog data. Leaked once: the modeless common dialog keeps pointers
/// into it for as long as it is open.
struct FindData {
    fr: FINDREPLACEW,
    find: [u16; 512],
    replace: [u16; 512],
}

pub struct App {
    ui: Cell<Ui>,
    state: RefCell<State>,
    pub preview: RefCell<Preview>,
    /// Set while the app itself replaces the text, so the change is not "dirty".
    suppress_dirty: Cell<bool>,
    close_confirmed: Cell<bool>,
    /// The character (`\r` or `\t`) whose key was consumed on key-down, to drop the
    /// WM_CHAR that follows; 0 for none.
    swallow_char: Cell<u16>,
    /// A disk check is queued (see `WM_ACTIVATE`).
    disk_check_pending: Cell<bool>,
    /// Re-entrancy guard for commands that show a modal dialog.
    busy: Cell<bool>,
    hovered: Cell<HWND>,
    /// A render request is queued (see `request_render`).
    render_pending: Cell<bool>,
    /// Ctrl+wheel movement not yet worth a whole zoom step.
    wheel_accum: Cell<i32>,
    find: Cell<*mut FindData>,
    find_message: Cell<u32>,
    /// Results of the background folder probe. The window message is only a doorbell:
    /// the payload never travels in lparam, so a forged or duplicated message can't hand
    /// the UI thread a pointer to free.
    probe_tx: std::sync::mpsc::Sender<std::io::Result<()>>,
    probe_rx: std::sync::mpsc::Receiver<std::io::Result<()>>,
    /// A probe result waiting for the current modal dialog to close.
    pending_probe: RefCell<Option<std::io::Result<()>>>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

pub fn app() -> Rc<App> {
    APP.with(|a| a.borrow().clone()).expect("app not initialized")
}

fn try_app() -> Option<Rc<App>> {
    APP.with(|a| a.borrow().clone())
}

/// Runs the app until the main window closes. `launch_file` is a path given on the
/// command line (how Explorer passes the double-clicked `.md`).
pub fn run(launch_file: Option<PathBuf>) -> windows::core::Result<()> {
    let settings = SettingsStore::load();
    let installed_fonts = dialogs::installed_fonts();
    let icon_face = if installed_fonts.iter().any(|f| f == "Segoe Fluent Icons") {
        "Segoe Fluent Icons"
    } else {
        "Segoe MDL2 Assets"
    };
    let dark = is_dark(settings.current().theme);
    theme::set_app_dark_mode(dark);

    let (probe_tx, probe_rx) = std::sync::mpsc::channel();
    let app = Rc::new(App {
        ui: Cell::new(Ui::default()),
        state: RefCell::new(State {
            file_path: None,
            format: DocumentFormat::default(),
            file_name: UNTITLED.into(),
            dirty: false,
            zoom: 100,
            settings,
            errors: Vec::new(),
            palette: Palette::new(dark),
            html: String::new(),
            code_blocks: Vec::new(),
            copy_token: 0,
            lines: 1,
            chars: 0,
            scroll_ratio: 0.0,
            layout: Layout::default(),
            installed_fonts,
            icon_face,
            recent_items: 0,
            find_is_replace: false,
            probing_folder: None,
            disk_stamp: None,
            missing_reported: false,
            declined_overwrite: None,
        }),
        preview: RefCell::new(Preview::default()),
        suppress_dirty: Cell::new(false),
        close_confirmed: Cell::new(false),
        swallow_char: Cell::new(0),
        disk_check_pending: Cell::new(false),
        busy: Cell::new(false),
        hovered: Cell::new(HWND::default()),
        wheel_accum: Cell::new(0),
        render_pending: Cell::new(false),
        find: Cell::new(std::ptr::null_mut()),
        find_message: Cell::new(0),
        probe_tx,
        probe_rx,
        pending_probe: RefCell::new(None),
    });
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));

    app.create_window()?;
    let ui = app.ui.get();
    // Settings load before any UI exists; tell the user now that the banner does.
    let warning = app.state.borrow_mut().settings.take_load_warning();
    if let Some(warning) = warning {
        app.report_error("SettingsStore.load", &warning, None);
    }

    app.load_content(SAMPLE_CONTENT);
    if let Some(path) = launch_file {
        // Falls back to the sample document (with the error banner) if unreadable.
        app.open_path(&path);
    }
    app.rebuild_recent_menu();
    preview::start(&app, ui.hwnd);

    unsafe {
        let _ = ShowWindow(ui.hwnd, SW_SHOWDEFAULT);
        let _ = SetFocus(Some(ui.editor));
        SetTimer(Some(ui.hwnd), TIMER_SCROLL_SYNC, 50, None);

        let mut msg = MSG::default();
        loop {
            // BOOL with three values: 0 is WM_QUIT, -1 an error that would repeat forever.
            match GetMessageW(&mut msg, None, 0, 0).0 {
                0 => break,
                -1 => {
                    log::error("GetMessageW", windows::core::Error::from_thread());
                    break;
                }
                _ => {}
            }
            let find = app.ui.get().find_dialog;
            if !find.is_invalid() && IsDialogMessageW(find, &msg).as_bool() {
                continue;
            }
            if TranslateAcceleratorW(ui.hwnd, ui.accel, &msg) != 0 {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    APP.with(|a| *a.borrow_mut() = None);
    Ok(())
}

const BANNER_MAX_MESSAGES: usize = 3;

/// Adds a message to the banner without losing the ones before it: two failures in a
/// row (a removed folder, then the settings write about it) are both worth reading.
fn push_banner_message(messages: &mut Vec<String>, message: &str) {
    if messages.last().is_some_and(|m| m == message) {
        return;
    }
    messages.push(message.to_string());
    if messages.len() > BANNER_MAX_MESSAGES {
        messages.remove(0);
    }
}

fn is_dark(theme: Theme) -> bool {
    match theme {
        Theme::System => theme::system_prefers_dark(),
        Theme::Light => false,
        Theme::Dark => true,
    }
}

fn menu_label(text: &str) -> Vec<u16> {
    wide(text)
}

impl App {
    fn editor(&self) -> Editor {
        Editor(self.ui.get().editor)
    }

    fn hwnd(&self) -> HWND {
        self.ui.get().hwnd
    }

    pub fn main_hwnd(&self) -> HWND {
        self.hwnd()
    }

    pub fn post_command(&self, cmd: u16) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd()), WM_COMMAND, WPARAM(cmd as usize), LPARAM(0));
        }
    }

    // ---- window construction ----------------------------------------------------

    fn create_window(&self) -> windows::core::Result<()> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("MyMDeditMainWindow");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(main_proc),
                hInstance: instance.into(),
                // MAKEINTRESOURCE(1): an integer resource id smuggled in a pointer, not an address.
                hIcon: LoadIconW(Some(instance.into()), PCWSTR(std::ptr::without_provenance(1))).unwrap_or_default(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc);

            let menu = self.build_menu()?;
            let title = wide(UNTITLED);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                pcwstr(&title),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                Some(menu),
                Some(instance.into()),
                None,
            )?;
            // A comfortable default size for a split editor, centered on the primary screen.
            // The window may have been created on another monitor (Explorer passes the one
            // it was launched from); moving it across a DPI boundary sends WM_DPICHANGED
            // before main_proc knows this window, so it is lost. Hence the DPI is read
            // again once the window has arrived, and the size redone if it changed.
            let mut dpi = win::dpi_of(hwnd);
            for _ in 0..2 {
                let (w, h) = (scale(1200, dpi), scale(800, dpi));
                let x = (GetSystemMetrics(SM_CXSCREEN) - w).max(0) / 2;
                let y = (GetSystemMetrics(SM_CYSCREEN) - h).max(0) / 2;
                if let Err(e) = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER) {
                    log::error("SetWindowPos", &e);
                }
                let arrived = win::dpi_of(hwnd);
                if arrived == dpi {
                    break;
                }
                dpi = arrived;
            }

            let editor = Editor::create(hwnd, ID_EDITOR)?;
            let _ = SetWindowSubclass(editor.hwnd(), Some(editor_proc), 1, 0);

            let mut buttons = [HWND::default(); TOOL_BUTTON_COUNT];
            for (slot, b) in buttons.iter_mut().zip(TOOL_BUTTONS.iter()) {
                *slot = self.create_owner_draw_button(hwnd, b.id)?;
                // The window text is never drawn (owner-draw) but is the button's
                // accessible name for screen readers and UI automation.
                let name = wide(b.tip);
                let _ = SetWindowTextW(*slot, pcwstr(&name));
            }
            let error_close = self.create_owner_draw_button(hwnd, ID_ERROR_CLOSE)?;
            let close_name = wide("閉じる");
            let _ = SetWindowTextW(error_close, pcwstr(&close_name));
            let _ = ShowWindow(error_close, SW_HIDE);

            let tooltip = CreateWindowExW(
                WS_EX_TOPMOST,
                TOOLTIPS_CLASSW,
                None,
                WS_POPUP | WINDOW_STYLE(0x01 | 0x02), // TTS_ALWAYSTIP | TTS_NOPREFIX
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                Some(hwnd),
                None,
                Some(instance.into()),
                None,
            )?;
            send(tooltip, TTM_SETMAXTIPWIDTH, 0, scale(400, dpi) as isize);
            for (h, b) in buttons.iter().zip(TOOL_BUTTONS.iter()) {
                let mut text = wide(b.tip);
                let ti = TTTOOLINFOW {
                    cbSize: size_of::<TTTOOLINFOW>() as u32,
                    uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                    hwnd,
                    uId: h.0 as usize,
                    lpszText: PWSTR(text.as_mut_ptr()),
                    ..Default::default()
                };
                send(tooltip, TTM_ADDTOOLW, 0, &ti as *const _ as isize);
            }

            let accels: Vec<ACCEL> = ACCELERATORS
                .iter()
                .map(|&(key, ctrl, shift, cmd)| {
                    let mut f = FVIRTKEY;
                    if ctrl {
                        f |= FCONTROL;
                    }
                    if shift {
                        f |= FSHIFT;
                    }
                    ACCEL { fVirt: f, key, cmd }
                })
                .collect();
            let accel = CreateAcceleratorTableW(&accels)?;

            let find_message = RegisterWindowMessageW(FINDMSGSTRINGW);
            self.find_message.set(find_message);
            // Leaked, and only ever reached through this raw pointer (see show_find).
            let find =
                Box::into_raw(Box::new(FindData { fr: FINDREPLACEW::default(), find: [0; 512], replace: [0; 512] }));
            self.find.set(find);

            self.ui.set(Ui {
                hwnd,
                editor: editor.hwnd(),
                tooltip,
                error_close,
                buttons,
                file_menu: GetSubMenu(menu, 0),
                accel,
                find_dialog: HWND::default(),
                dpi,
                fonts: Fonts::default(),
            });
            self.create_fonts();
            self.apply_theme();
            self.apply_editor_font();
            self.layout();
        }
        Ok(())
    }

    fn create_owner_draw_button(&self, parent: HWND, id: u16) -> windows::core::Result<HWND> {
        unsafe {
            let h = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                None,
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_OWNERDRAW as u32),
                0,
                0,
                0,
                0,
                Some(parent),
                Some(HMENU(id as usize as *mut _)),
                None,
                None,
            )?;
            let _ = SetWindowSubclass(h, Some(button_proc), 1, 0);
            Ok(h)
        }
    }

    fn build_menu(&self) -> windows::core::Result<HMENU> {
        unsafe {
            let bar = CreateMenu()?;
            let add = |menu: HMENU, id: u16, text: &str| {
                let t = menu_label(text);
                let _ = AppendMenuW(menu, MF_STRING, id as usize, pcwstr(&t));
            };
            let sep = |menu: HMENU| {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            };
            let popup = |title: &str, sub: HMENU| {
                let t = menu_label(title);
                let _ = AppendMenuW(bar, MF_POPUP, sub.0 as usize, pcwstr(&t));
            };

            let file = CreatePopupMenu()?;
            add(file, ID_NEW, "新規\tCtrl+N");
            add(file, ID_OPEN, "開く\tCtrl+O");
            sep(file);
            add(file, ID_SAVE, "保存\tCtrl+S");
            add(file, ID_SAVE_AS, "名前を付けて保存\tCtrl+Shift+S");
            sep(file); // Recent folders go just above this (rebuild_recent_menu).
            add(file, ID_EXIT, "終了\tAlt+F4");
            popup("ファイル", file);

            let edit = CreatePopupMenu()?;
            add(edit, ID_UNDO, "元に戻す\tCtrl+Z");
            add(edit, ID_REDO, "やり直し\tCtrl+Y");
            sep(edit);
            add(edit, ID_CUT, "切り取り\tCtrl+X");
            add(edit, ID_COPY, "コピー\tCtrl+C");
            add(edit, ID_PASTE, "貼り付け\tCtrl+V");
            sep(edit);
            add(edit, ID_SELECT_ALL, "すべて選択\tCtrl+A");
            sep(edit);
            add(edit, ID_TASK, "タスクの切り替え\tCtrl+Enter");
            popup("編集", edit);

            let find = CreatePopupMenu()?;
            add(find, ID_FIND, "検索...\tCtrl+F");
            add(find, ID_REPLACE, "置換...\tCtrl+H");
            sep(find);
            add(find, ID_OUTLINE, "見出しへ移動...\tCtrl+Shift+O");
            add(find, ID_GOTO_LINE, "行へ移動...\tCtrl+G");
            popup("検索", find);

            let settings = CreatePopupMenu()?;
            add(settings, ID_SETTINGS, "設定を開く...");
            popup("設定", settings);

            let help = CreatePopupMenu()?;
            add(help, ID_SHORTCUTS, "ショートカット一覧...");
            sep(help);
            add(help, ID_HELP_GITHUB, "GitHub のページを開く");
            add(help, ID_HELP_RELEASES, "最新版を確認");
            sep(help);
            add(help, ID_ABOUT, "バージョン情報...");
            popup("ヘルプ", help);
            Ok(bar)
        }
    }

    fn create_fonts(&self) {
        let mut ui = self.ui.get();
        let old = ui.fonts;
        let icon_face = self.state.borrow().icon_face;
        ui.fonts = Fonts {
            ui: win::message_font(ui.dpi, false),
            bold: win::message_font(ui.dpi, true),
            icon: win::font(icon_face, scale(16, ui.dpi), false),
            icon_small: win::font(icon_face, scale(10, ui.dpi), false),
            menu: win::menu_font(ui.dpi),
        };
        self.ui.set(ui);
        for f in [old.ui, old.bold, old.icon, old.icon_small, old.menu] {
            win::delete_font(f);
        }
    }

    // ---- theme, fonts, layout -----------------------------------------------------

    fn apply_theme(&self) {
        let ui = self.ui.get();
        let dark = is_dark(self.state.borrow().settings.current().theme);
        let palette = Palette::new(dark);
        self.state.borrow_mut().palette = palette;
        theme::set_app_dark_mode(dark);
        theme::allow_dark_for_window(ui.hwnd, dark);
        theme::set_title_bar_dark(ui.hwnd, dark);
        theme::set_control_theme(ui.editor, dark, "DarkMode_Explorer");
        theme::set_control_theme(ui.tooltip, dark, "DarkMode_Explorer");
        self.editor().set_background(palette.editor_bg);
        preview::set_background(self, palette.editor_bg);
        unsafe {
            let _ = DrawMenuBar(ui.hwnd);
            // Repaint the non-client area too: the menu bar is painted there.
            let _ = SetWindowPos(ui.hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
            let _ = InvalidateRect(Some(ui.hwnd), None, true);
            for b in ui.buttons.iter().chain([&ui.error_close]) {
                let _ = InvalidateRect(Some(*b), None, true);
            }
        }
    }

    fn apply_editor_font(&self) {
        let (face, size, color) = {
            let st = self.state.borrow();
            let s = st.settings.current();
            (
                dialogs::resolve_font(&s.editor_font_family, &st.installed_fonts, "Consolas"),
                s.editor_font_size,
                st.palette.editor_text,
            )
        };
        self.suppress_dirty.set(true);
        self.editor().set_font(&face, size, color);
        self.suppress_dirty.set(false);
    }

    fn layout(&self) {
        let ui = self.ui.get();
        let s = |v: i32| scale(v, ui.dpi);
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(ui.hwnd, &mut client);
        }
        let (w, h) = (client.right, client.bottom);

        let toolbar = RECT { left: 0, top: 0, right: w, bottom: s(40) };
        let error_text = {
            let st = self.state.borrow();
            (!st.errors.is_empty()).then(|| st.errors.join("\n"))
        };
        let error_h = match &error_text {
            Some(text) => self.measure_error_height(text, w),
            None => 0,
        };
        let error = RECT { left: 0, top: toolbar.bottom, right: w, bottom: toolbar.bottom + error_h };
        let footer = RECT { left: 0, top: (h - s(28)).max(error.bottom), right: w, bottom: h };
        let body_top = error.bottom;
        let body_bottom = footer.top;
        let half = (w - 1).max(0) / 2;
        let editor = RECT { left: 0, top: body_top, right: half, bottom: body_bottom };
        let divider = RECT { left: half, top: body_top, right: half + 1, bottom: body_bottom };
        let preview_rect = RECT { left: half + 1, top: body_top, right: w, bottom: body_bottom };

        unsafe {
            // Toolbar buttons, after a short vertical separator.
            let mut x = s(8) + s(1) + s(8);
            let bh = s(30);
            let by = (toolbar.bottom - bh) / 2;
            for (hb, b) in ui.buttons.iter().zip(TOOL_BUTTONS.iter()) {
                let bw = s(b.width);
                let _ = MoveWindow(*hb, x, by, bw, bh, true);
                x += bw + s(4);
            }
            let _ = MoveWindow(ui.editor, editor.left, editor.top, win::width(&editor), win::height(&editor), true);
            if error_h > 0 {
                let size = s(32);
                let _ = MoveWindow(ui.error_close, w - size - s(8), error.top + (error_h - size) / 2, size, size, true);
                let _ = ShowWindow(ui.error_close, SW_SHOW);
            } else {
                let _ = ShowWindow(ui.error_close, SW_HIDE);
            }
        }
        self.editor().set_padding(s(16));
        preview::set_bounds(self, preview_rect);
        self.state.borrow_mut().layout = Layout { toolbar, error, divider, preview: preview_rect, footer };
        unsafe {
            let _ = InvalidateRect(Some(ui.hwnd), None, false);
        }
    }

    fn error_text_rect(&self, width: i32) -> RECT {
        let s = |v: i32| scale(v, self.ui.get().dpi);
        RECT { left: s(44), top: 0, right: (width - s(52)).max(s(44) + 1), bottom: 0 }
    }

    fn measure_error_height(&self, text: &str, width: i32) -> i32 {
        let ui = self.ui.get();
        let s = |v: i32| scale(v, ui.dpi);
        let mut rc = self.error_text_rect(width);
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let hdc = GetDC(Some(ui.hwnd));
            let old = SelectObject(hdc, HGDIOBJ(ui.fonts.ui.0));
            DrawTextW(hdc, &mut buf, &mut rc, DT_WORDBREAK | DT_NOPREFIX | windows::Win32::Graphics::Gdi::DT_CALCRECT);
            SelectObject(hdc, old);
            ReleaseDC(Some(ui.hwnd), hdc);
        }
        (win::height(&rc) + s(24)).max(s(48))
    }

    // ---- painting ---------------------------------------------------------------

    fn paint(&self) {
        let ui = self.ui.get();
        let s = |v: i32| scale(v, ui.dpi);
        let (layout, palette, error, footer_texts) = {
            let st = self.state.borrow();
            (
                st.layout,
                st.palette,
                (!st.errors.is_empty()).then(|| st.errors.join("\n")),
                [
                    format!("行: {}", group_thousands(st.lines)),
                    format!("文字: {}", group_thousands(st.chars)),
                    format!("ズーム: {}%", st.zoom),
                ],
            )
        };
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(ui.hwnd, &mut ps);
            SetBkMode(hdc, TRANSPARENT);

            // Toolbar + its bottom border + the separator before the buttons.
            fill(hdc, &layout.toolbar, palette.window_bg);
            let border = RECT { top: layout.toolbar.bottom - 1, ..layout.toolbar };
            fill(hdc, &border, palette.divider);
            let sep_h = s(20);
            let sep_top = (layout.toolbar.bottom - sep_h) / 2;
            fill(hdc, &RECT { left: s(8), top: sep_top, right: s(8) + s(1), bottom: sep_top + sep_h }, palette.divider);

            if let Some(text) = error {
                fill(hdc, &layout.error, palette.error_bg);
                let old = SelectObject(hdc, HGDIOBJ(ui.fonts.icon.0));
                SetTextColor(hdc, palette.error_icon);
                let mut icon = "\u{EA39}".encode_utf16().collect::<Vec<_>>();
                let mut icon_rc =
                    RECT { left: s(12), top: layout.error.top, right: s(40), bottom: layout.error.bottom };
                DrawTextW(hdc, &mut icon, &mut icon_rc, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
                SelectObject(hdc, HGDIOBJ(ui.fonts.ui.0));
                SetTextColor(hdc, palette.text);
                let mut rc = self.error_text_rect(layout.error.right);
                let mut buf: Vec<u16> = text.encode_utf16().collect();
                let mut measure = rc;
                DrawTextW(
                    hdc,
                    &mut buf,
                    &mut measure,
                    DT_WORDBREAK | DT_NOPREFIX | windows::Win32::Graphics::Gdi::DT_CALCRECT,
                );
                let text_h = win::height(&measure);
                rc.top = layout.error.top + (win::height(&layout.error) - text_h) / 2;
                rc.bottom = rc.top + text_h;
                DrawTextW(hdc, &mut buf, &mut rc, DT_WORDBREAK | DT_NOPREFIX);
                SelectObject(hdc, old);
            }

            fill(hdc, &layout.divider, palette.divider);
            // Under the WebView until it exists (and if it never does).
            fill(hdc, &layout.preview, palette.editor_bg);

            fill(hdc, &layout.footer, palette.footer_bg);
            fill(hdc, &RECT { bottom: layout.footer.top + 1, ..layout.footer }, palette.divider);
            let old = SelectObject(hdc, HGDIOBJ(ui.fonts.ui.0));
            SetTextColor(hdc, palette.text);
            let mut x = s(12);
            for t in footer_texts {
                let mut buf: Vec<u16> = t.encode_utf16().collect();
                let mut measure = RECT::default();
                DrawTextW(
                    hdc,
                    &mut buf,
                    &mut measure,
                    DT_SINGLELINE | DT_NOPREFIX | windows::Win32::Graphics::Gdi::DT_CALCRECT,
                );
                let mut rc = RECT {
                    left: x,
                    top: layout.footer.top + 1,
                    right: x + win::width(&measure),
                    bottom: layout.footer.bottom,
                };
                DrawTextW(hdc, &mut buf, &mut rc, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_NOPREFIX);
                x = rc.right + s(16);
            }
            SelectObject(hdc, old);
            let _ = EndPaint(ui.hwnd, &ps);
        }
    }

    fn draw_button(&self, dis: &DRAWITEMSTRUCT) {
        let ui = self.ui.get();
        let palette = self.state.borrow().palette;
        let hovered = self.hovered.get() == dis.hwndItem;
        let pressed = dis.itemState.0 & ODS_SELECTED.0 != 0;
        let is_error_close = dis.CtlID == ID_ERROR_CLOSE as u32;
        let base = if is_error_close { palette.error_bg } else { palette.window_bg };
        let bg = if pressed {
            palette.pressed_bg
        } else if hovered {
            palette.hover_bg
        } else {
            base
        };
        let s = |v: i32| scale(v, ui.dpi);
        fill(dis.hDC, &dis.rcItem, bg);

        let (label, font, dropdown) = if is_error_close {
            ("\u{E711}", ButtonFont::Icon, false)
        } else {
            match TOOL_BUTTONS.iter().find(|b| b.id as u32 == dis.CtlID) {
                Some(b) => (b.label, b.font, b.dropdown),
                None => return,
            }
        };
        unsafe {
            SetBkMode(dis.hDC, TRANSPARENT);
            SetTextColor(dis.hDC, palette.text);
            let font = match font {
                ButtonFont::Icon => ui.fonts.icon,
                ButtonFont::Bold => ui.fonts.bold,
                ButtonFont::Normal => ui.fonts.ui,
            };
            let old = SelectObject(dis.hDC, HGDIOBJ(font.0));
            let mut rc = dis.rcItem;
            if dropdown {
                rc.right -= s(12);
            }
            let mut buf: Vec<u16> = label.encode_utf16().collect();
            DrawTextW(dis.hDC, &mut buf, &mut rc, DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_NOPREFIX);
            if dropdown {
                SelectObject(dis.hDC, HGDIOBJ(ui.fonts.icon_small.0));
                let mut chevron: Vec<u16> = "\u{E70D}".encode_utf16().collect();
                let mut crc = RECT { left: dis.rcItem.right - s(16), right: dis.rcItem.right - s(2), ..dis.rcItem };
                DrawTextW(dis.hDC, &mut chevron, &mut crc, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
            }
            SelectObject(dis.hDC, old);
        }
    }

    fn invalidate_footer(&self) {
        let footer = self.state.borrow().layout.footer;
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd()), Some(&footer), false);
        }
    }

    fn update_title(&self) {
        let title = {
            let st = self.state.borrow();
            if st.dirty { format!("{} *", st.file_name) } else { st.file_name.clone() }
        };
        let t = wide(&title);
        unsafe {
            let _ = SetWindowTextW(self.hwnd(), pcwstr(&t));
        }
    }

    // ---- errors --------------------------------------------------------------------

    /// Records a failure and shows it in the banner under the toolbar — the app's only
    /// user-visible error surface. Stays until the user closes it.
    pub fn report_error(&self, context: &str, message: &str, cause: Option<&dyn std::fmt::Display>) {
        match cause {
            Some(c) => log::error(context, c),
            None => log::error(context, message),
        }
        push_banner_message(&mut self.state.borrow_mut().errors, message);
        self.layout();
    }

    fn report_file_error(&self, context: &str, message: &str, e: &ReadError) {
        match e {
            // Already written for the user.
            ReadError::InvalidEncoding(_) | ReadError::ContainsNul => self.report_error(context, &e.to_string(), None),
            ReadError::Io(io) => self.report_error(context, &format!("{message} ({io})"), Some(io)),
        }
    }

    fn dismiss_error(&self) {
        self.state.borrow_mut().errors.clear();
        self.layout();
    }

    // ---- text & preview ------------------------------------------------------------

    fn on_text_changed(&self) {
        self.request_render();
        if !self.suppress_dirty.get() {
            let was_dirty = std::mem::replace(&mut self.state.borrow_mut().dirty, true);
            if !was_dirty {
                self.update_title();
            }
        }
    }

    /// Renders once the queue is drained of the edits that are already waiting, instead of
    /// once per edit. Nothing is delayed when typing is slower than rendering, which keeps
    /// the spec's "no debounce" behaviour; only bursts (a paste, key repeat) coalesce.
    fn request_render(&self) {
        if self.render_pending.replace(true) {
            return;
        }
        unsafe {
            if PostMessageW(Some(self.hwnd()), WM_APP_RENDER, WPARAM(0), LPARAM(0)).is_err() {
                self.render_pending.set(false);
                self.rerender();
            }
        }
    }

    /// Re-renders the preview from the editor's text without touching the dirty flag.
    fn rerender(&self) {
        let text = self.editor().text();
        let markdown = String::from_utf16_lossy(&text);
        // A fresh token per render: the document cannot learn it, so it cannot forge a
        // copy button, and a click on a page already replaced names nothing.
        let copy_token = document::random_u64();
        let rendered = render::to_html_document(&markdown, self.state.borrow().settings.current(), copy_token);
        {
            let mut st = self.state.borrow_mut();
            st.lines = line_endings::count_lines(&text);
            st.chars = text.len();
            st.html = rendered.html;
            st.code_blocks = rendered.code_blocks;
            st.copy_token = copy_token;
        }
        self.invalidate_footer();
        self.refresh_preview();
    }

    pub fn refresh_preview(&self) {
        let html = self.state.borrow().html.clone();
        preview::navigate(self, &html);
    }

    /// A copy button in the preview was clicked: `uri` is its link.
    pub fn copy_code_block(&self, uri: &str) {
        let text = {
            let st = self.state.borrow();
            render::parse_copy_link(uri, st.copy_token).and_then(|i| st.code_blocks.get(i).cloned())
        };
        let Some(text) = text else {
            log::error("PreviewCopyIgnored", "copy link does not name a code block of the current render");
            return;
        };
        if let Err(e) = win::set_clipboard_text(self.hwnd(), &text) {
            self.report_error("CopyCodeBlock", "コードをクリップボードにコピーできませんでした。", Some(&e));
        }
    }

    pub fn view_state(&self) -> (i32, f64) {
        let st = self.state.borrow();
        (st.zoom, st.scroll_ratio)
    }

    /// Replaces the document's text (after the caller has confirmed discarding).
    fn load_content(&self, content: &str) {
        self.suppress_dirty.set(true);
        self.editor().set_text(content);
        // EN_CHANGE from set_text has already asked for this; it coalesces, so asking again
        // only covers the case where no notification came.
        self.request_render();
        self.suppress_dirty.set(false);
        self.state.borrow_mut().dirty = false;
        self.update_title();
    }

    fn set_zoom(&self, zoom: i32) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        self.state.borrow_mut().zoom = zoom;
        self.editor().set_zoom(zoom);
        self.invalidate_footer();
        preview::sync_view(self);
    }

    fn zoom_by(&self, delta: i32) {
        let z = self.state.borrow().zoom;
        self.set_zoom(z + delta);
    }

    fn poll_scroll(&self) {
        let ratio = self.editor().scroll_ratio();
        let changed = {
            let mut st = self.state.borrow_mut();
            let changed = (st.scroll_ratio - ratio).abs() > 1e-4;
            st.scroll_ratio = ratio;
            changed
        };
        if changed {
            preview::sync_view(self);
        }
    }

    // ---- settings --------------------------------------------------------------------

    fn current_settings(&self) -> AppSettings {
        self.state.borrow().settings.current().clone()
    }

    /// The single place a settings write's failure becomes visible to the user.
    ///
    /// The write re-reads the file, so it can bring in another instance's changes (a
    /// theme switched over there). Whatever now differs on screen is re-applied here, so
    /// the window, the editor and the preview never show a mix of old and new settings.
    fn persist_settings(&self, change: impl FnOnce(&mut AppSettings)) {
        let before = self.current_settings();
        let result = self.state.borrow_mut().settings.update(change);
        if let Err(e) = result {
            self.report_error(
                "SettingsStore.save",
                "設定を保存できませんでした。この変更は次回の起動時には元に戻ります。",
                Some(&e),
            );
        }
        let after = self.current_settings();
        if settings::display_differs(&before, &after) {
            self.apply_theme();
            self.apply_editor_font();
            self.rerender();
        }
        // The recent-folder items are looked up by position in the current list, so the
        // menu is rebuilt whenever that list changes — including when the write brought in
        // another instance's list, whatever this write was about.
        if before.recent_folders != after.recent_folders {
            self.rebuild_recent_menu();
        }
    }

    pub fn update_custom_colors(&self, colors: Vec<String>) {
        self.persist_settings(|s| s.custom_colors = colors);
    }

    fn open_settings(&self) {
        let (current, fonts, palette) = {
            let st = self.state.borrow();
            (st.settings.current().clone(), st.installed_fonts.clone(), st.palette)
        };
        let fonts = if fonts.is_empty() { vec!["Segoe UI".into(), "Consolas".into()] } else { fonts };
        let edits = match settings_dialog::show(
            self.hwnd(),
            &current,
            &fonts,
            palette,
            Box::new(|colors| {
                if let Some(app) = try_app() {
                    app.update_custom_colors(colors);
                }
            }),
        ) {
            Ok(Some(edits)) => edits,
            Ok(None) => return,
            Err(e) => {
                // The user asked for this dialog; its absence needs explaining.
                self.report_error("SettingsDialog", "設定ダイアログを表示できませんでした。", Some(&e));
                return;
            }
        };
        // Applied onto the settings as of now, not as of when the dialog opened.
        // Re-applies the theme, fonts and preview itself when they changed.
        self.persist_settings(|s| *s = edits.apply(s));
    }

    // ---- recent folders ------------------------------------------------------------

    fn record_recent_folder(&self, file: &Path) {
        let Some(folder) = file.parent().map(|p| p.to_string_lossy().into_owned()) else {
            return;
        };
        if folder.is_empty() {
            return;
        }
        let current = self.current_settings().recent_folders;
        if settings::add_recent_folder(&current, &folder) == current {
            return; // Saving the same file again changes nothing worth a settings write.
        }
        // persist_settings rebuilds the menu.
        self.persist_settings(|s| s.recent_folders = settings::add_recent_folder(&s.recent_folders, &folder));
    }

    fn remove_recent_folder(&self, folder: &str) {
        self.persist_settings(|s| s.recent_folders = settings::remove_recent_folder(&s.recent_folders, folder));
    }

    fn rebuild_recent_menu(&self) {
        let menu = self.ui.get().file_menu;
        let folders = self.current_settings().recent_folders;
        let previous = std::mem::take(&mut self.state.borrow_mut().recent_items);
        unsafe {
            // The recent items sit just above the separator over 終了, found by its id
            // rather than by a position that a new File menu item would shift.
            let count = u32::try_from(GetMenuItemCount(Some(menu))).unwrap_or(0);
            let Some(exit) = (0..count).find(|&i| GetMenuItemID(menu, i as i32) == ID_EXIT as u32) else {
                log::error("rebuild_recent_menu", "終了 is not in the File menu");
                return;
            };
            let start = exit.saturating_sub(1 + previous);
            for _ in 0..previous {
                if let Err(e) = RemoveMenu(menu, start, MF_BYPOSITION) {
                    log::error("RemoveMenu", &e);
                }
            }
            if folders.is_empty() {
                return;
            }
            let mut pos = start;
            let _ = InsertMenuW(menu, pos, MF_BYPOSITION | MF_SEPARATOR, 0, None);
            pos += 1;
            for (i, folder) in folders.iter().enumerate() {
                let name =
                    Path::new(folder).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty());
                // Win32 menus have no per-item tooltip; the full path goes in the
                // right-hand column instead.
                let label = match name {
                    Some(n) => format!("{}\t{}", n.replace('&', "&&"), folder.replace('&', "&&")),
                    None => folder.replace('&', "&&"),
                };
                let t = wide(&label);
                let _ =
                    InsertMenuW(menu, pos, MF_BYPOSITION | MF_STRING, (ID_RECENT_BASE + i as u16) as usize, pcwstr(&t));
                pos += 1;
            }
            self.state.borrow_mut().recent_items = pos - start;
        }
    }

    /// Step 1 of opening from a recent folder: confirm the folder can be listed, off the
    /// UI thread — an unreachable network drive blocks for the SMB/DNS timeout.
    fn open_from_recent(&self, index: usize) {
        let Some(folder) = self.current_settings().recent_folders.get(index).cloned() else {
            return;
        };
        let already_probing = self.state.borrow().probing_folder.is_some();
        if already_probing {
            // A doorbell may have been lost; pick up a result that is already there.
            if let Ok(result) = self.probe_rx.try_recv() {
                *self.pending_probe.borrow_mut() = Some(result);
                self.deliver_pending_probe();
            } else {
                // Still checking (an unreachable network drive takes a while).
                unsafe {
                    let _ = MessageBeep(MB_OK);
                }
            }
            return;
        }
        self.state.borrow_mut().probing_folder = Some(folder.clone());
        let hwnd = self.hwnd().0 as isize;
        let tx = self.probe_tx.clone();
        std::thread::spawn(move || {
            // Listing must actually be attempted: one entry is enough. An empty folder
            // is a perfectly good folder; only the error matters.
            let result = std::fs::read_dir(&folder).and_then(|mut it| it.next().transpose().map(|_| ()));
            if tx.send(result).is_err() {
                return; // The app is gone.
            }
            unsafe {
                if let Err(e) = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_FOLDER_PROBED, WPARAM(0), LPARAM(0)) {
                    // The result waits in the channel; the next click on a recent folder collects it.
                    log::error("open_from_recent PostMessage", &e);
                }
            }
        });
    }

    /// Handles a waiting probe result unless a modal dialog is up; then it is retried when
    /// that dialog's command finishes (see `command`). The result opens a discard prompt
    /// and a file picker, which must never stack on another modal.
    fn deliver_pending_probe(&self) {
        // Once closing is decided the window may already be destroyed (終了 runs
        // DestroyWindow inside its command): a prompt owned by it must not appear.
        if self.close_confirmed.get() || self.busy.replace(true) {
            return;
        }
        let result = self.pending_probe.borrow_mut().take();
        if let Some(result) = result {
            self.on_folder_probed(result);
        }
        self.busy.set(false);
    }

    /// Step 2: the probe's verdict. Only "not found" drops the entry; access denied or
    /// a disconnected drive are likely temporary and the entry is the way back.
    fn on_folder_probed(&self, result: std::io::Result<()>) {
        let Some(folder) = self.state.borrow_mut().probing_folder.take() else {
            return;
        };
        match result {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.report_error(
                    "OpenFromFolder",
                    &format!("このフォルダは見つかりません: {folder}（最近使ったフォルダから削除しました）"),
                    Some(&e),
                );
                self.remove_recent_folder(&folder);
                return;
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                self.report_error(
                    "OpenFromFolder",
                    &format!("このフォルダにアクセスする権限がありません: {folder}"),
                    Some(&e),
                );
                return;
            }
            Err(e) => {
                self.report_error(
                    "OpenFromFolder",
                    &format!("このフォルダを読み取れませんでした: {folder}（ネットワークドライブが切断されている可能性があります）"),
                    Some(&e),
                );
                return;
            }
        }
        // The discard prompt comes after the probe, so a stale entry never asks
        // 「保存しますか」 only to abort; and before the picker, so a save it triggers is
        // never followed by loading a pre-save snapshot of that file.
        if !self.confirm_discard(false) {
            return;
        }
        match dialogs::open_file(self.hwnd(), Some(Path::new(&folder))) {
            Ok(Some(path)) => self.read_and_apply(&path, "OpenFromFolder", "ファイルを開けませんでした。"),
            Ok(None) => {}
            Err(e) => self.report_error(
                "OpenFromFolder",
                &format!("ファイルを開けませんでした。 ({})", e.message()),
                Some(&e),
            ),
        }
    }

    // ---- file commands ---------------------------------------------------------------

    /// True when it is safe to replace the document. A save that fails or whose picker
    /// is cancelled leaves the document dirty, which reads as "not safe".
    /// `fail_open` decides what an undisplayable prompt means (true only for closing
    /// the window, so it can never become unclosable).
    fn confirm_discard(&self, fail_open: bool) -> bool {
        if !self.state.borrow().dirty {
            return true;
        }
        match dialogs::confirm_discard(self.hwnd()) {
            Ok(DiscardChoice::Save) => {
                self.save();
                !self.state.borrow().dirty
            }
            Ok(DiscardChoice::Discard) => true,
            Ok(DiscardChoice::Cancel) => false,
            Err(e) => {
                log::error("confirm_discard", &e);
                fail_open
            }
        }
    }

    fn new_document(&self) {
        if !self.confirm_discard(false) {
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            st.file_path = None;
            st.format = DocumentFormat::default();
            st.file_name = UNTITLED.into();
            st.disk_stamp = None;
        }
        self.load_content(SAMPLE_CONTENT);
    }

    fn open(&self) {
        // Gate before the picker: a save the gate triggers must never be followed by
        // loading a pre-save snapshot of the file it just wrote.
        if !self.confirm_discard(false) {
            return;
        }
        match dialogs::open_file(self.hwnd(), None) {
            Ok(Some(path)) => self.read_and_apply(&path, "Open", "ファイルを開けませんでした。"),
            Ok(None) => {}
            Err(e) => self.report_error("Open", &format!("ファイルを開けませんでした。 ({})", e.message()), Some(&e)),
        }
    }

    fn open_path(&self, path: &Path) {
        if !self.confirm_discard(false) {
            return;
        }
        self.read_and_apply(path, "OpenFile", &format!("ファイルを開けませんでした: {}", path.display()));
    }

    /// On failure the current document is deliberately left untouched.
    fn read_and_apply(&self, path: &Path, context: &str, message: &str) {
        match document::read(path) {
            Ok(doc) => self.apply_opened(doc),
            Err(e) => self.report_file_error(context, message, &e),
        }
    }

    /// Every read funnels through here, so the path, the on-disk format and the text
    /// can never describe different files.
    fn apply_opened(&self, doc: OpenedDocument) {
        {
            let mut st = self.state.borrow_mut();
            st.file_name =
                doc.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| UNTITLED.into());
            st.file_path = Some(doc.path.clone());
            st.format = doc.format;
            st.disk_stamp = doc.stamp;
            st.missing_reported = false;
            st.declined_overwrite = None;
        }
        self.load_content(&doc.content);
        self.record_recent_folder(&doc.path);
    }

    fn save(&self) {
        let (path, format, recorded, name) = {
            let st = self.state.borrow();
            (st.file_path.clone(), st.format, st.disk_stamp, st.file_name.clone())
        };
        let Some(path) = path else {
            self.save_as();
            return;
        };
        if let Some(recorded) = recorded
            && let DiskChange::Modified(now) = document::check_disk(&path, recorded)
        {
            // An undisplayable prompt saves anyway: refusing would leave the document
            // with no way to be saved to its own file.
            let overwrite = dialogs::confirm_overwrite(self.hwnd(), &name).unwrap_or_else(|e| {
                log::error("confirm_overwrite", &e);
                true
            });
            if !overwrite {
                self.state.borrow_mut().declined_overwrite = Some(now);
                return; // Still dirty, so a discard gate reads this as "not safe".
            }
        }
        let (text, replaced) = document::text_from_editor(&self.editor().text());
        if let Err(e) = document::save_atomic(&path, &text, &format) {
            // The dirty flag stays: a failed save can never pass for a successful one.
            self.report_error("Save", &format!("保存できませんでした: {} ({e})", path.display()), Some(&e));
            return;
        }
        self.record_saved(&path);
        self.warn_if_replaced(replaced);
        self.update_title();
        self.record_recent_folder(&path);
    }

    fn save_as(&self) {
        let (name, format) = {
            let st = self.state.borrow();
            (st.file_name.clone(), st.format)
        };
        let path = match dialogs::save_file(self.hwnd(), &name) {
            Ok(Some(p)) => p,
            Ok(None) => return,
            Err(e) => {
                self.report_error(
                    "SaveAs picker",
                    &format!("保存先を選択できませんでした。 ({})", e.message()),
                    Some(&e),
                );
                return;
            }
        };
        let (text, replaced) = document::text_from_editor(&self.editor().text());
        if let Err(e) = document::save_atomic(&path, &text, &format) {
            // The document was never written there, so it must not claim that path.
            self.report_error("SaveAs", &format!("保存できませんでした: {} ({e})", path.display()), Some(&e));
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            st.file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            st.file_path = Some(path.clone());
        }
        self.record_saved(&path);
        self.warn_if_replaced(replaced);
        self.update_title();
        self.record_recent_folder(&path);
    }

    /// A save that had to substitute U+FFFD succeeded, but the user must not learn of the
    /// substitution only by noticing it in the file later.
    fn warn_if_replaced(&self, replaced: bool) {
        if replaced {
            self.report_error(
                "Save",
                "保存しましたが、ファイルに書けない文字（壊れたサロゲート）が含まれていたため U+FFFD（�）に置き換えました。",
                None,
            );
        }
    }

    /// After a successful write to `path`: clean, and what is on disk is now ours.
    fn record_saved(&self, path: &Path) {
        let stamp = document::stamp(path).inspect_err(|e| log::error("record_saved stamp", e)).ok();
        let mut st = self.state.borrow_mut();
        st.dirty = false;
        st.disk_stamp = stamp;
        st.missing_reported = false;
        st.declined_overwrite = None;
    }

    /// The window was activated: has another program changed or removed the open file?
    /// Runs from a posted message, never inside WM_ACTIVATE, as it may show a dialog.
    fn check_external_change(&self) {
        // Our own modal is up (its closing reactivates the window and checks again).
        if self.busy.replace(true) {
            return;
        }
        let (path, recorded, name, dirty, missing_reported, declined) = {
            let st = self.state.borrow();
            (
                st.file_path.clone(),
                st.disk_stamp,
                st.file_name.clone(),
                st.dirty,
                st.missing_reported,
                st.declined_overwrite,
            )
        };
        if let (Some(path), Some(recorded)) = (path, recorded) {
            match document::check_disk(&path, recorded) {
                DiskChange::Unchanged => {}
                DiskChange::Missing if !missing_reported => {
                    self.state.borrow_mut().missing_reported = true;
                    self.report_error(
                        "check_external_change",
                        &format!("開いているファイルが見つかりません（別のアプリで削除または移動されました）: {}  保存すると作り直します。", path.display()),
                        None,
                    );
                }
                DiskChange::Missing => {}
                DiskChange::Modified(now) if declined == Some(now) => {}
                DiskChange::Modified(now) => {
                    // Acknowledged either way: a reload that fails (say, the file is no
                    // longer UTF-8) must not ask again on every activation.
                    self.state.borrow_mut().disk_stamp = Some(now);
                    // An undisplayable prompt keeps the editor's text: nothing is lost.
                    let reload = dialogs::confirm_reload(self.hwnd(), &name, dirty).unwrap_or_else(|e| {
                        log::error("confirm_reload", &e);
                        false
                    });
                    if reload {
                        self.read_and_apply(
                            &path,
                            "Reload",
                            &format!("ファイルを読み込み直せませんでした: {}", path.display()),
                        );
                    }
                }
            }
        }
        self.busy.set(false);
        self.deliver_pending_probe();
    }

    fn exit(&self) {
        if !self.confirm_discard(false) {
            return;
        }
        self.close_confirmed.set(true);
        unsafe {
            let _ = DestroyWindow(self.hwnd());
        }
    }

    /// Whether the session may end. With unsaved changes, the shutdown screen is given a
    /// reason (so it explains what is holding it up) and the user gets the same
    /// 保存する / 保存しない / キャンセル prompt as closing the window.
    fn on_query_end_session(&self) -> bool {
        if self.close_confirmed.get() || !self.state.borrow().dirty {
            return true;
        }
        let reason = wide("保存されていない変更があります");
        unsafe {
            let _ = ShutdownBlockReasonCreate(self.hwnd(), pcwstr(&reason))
                .inspect_err(|e| log::error("ShutdownBlockReasonCreate", e));
        }
        // Another modal dialog is already up: don't stack a prompt on it. The block
        // reason stays, so the user sees why and can choose there.
        if self.busy.replace(true) {
            return false;
        }
        // An undisplayable prompt blocks rather than discards: the shutdown screen still
        // lets the user end the session deliberately.
        let ok = self.confirm_discard(false);
        self.busy.set(false);
        if ok {
            self.release_shutdown_block();
        } else {
            // The session continues: hand over anything that waited for this prompt.
            self.deliver_pending_probe();
        }
        ok
    }

    fn release_shutdown_block(&self) {
        unsafe {
            let _ = ShutdownBlockReasonDestroy(self.hwnd());
        }
    }

    fn on_close_request(&self) {
        // A modal dialog of ours is up (its own loop delivered this, e.g. from the taskbar).
        // Closing under it would stack a second prompt; the user can close once it's gone.
        if self.busy.replace(true) {
            return;
        }
        // The prompt below is itself a modal: while it is up, a second WM_CLOSE or a
        // folder-probe result must wait like they do for any other modal.
        let close = self.close_confirmed.get() || self.confirm_discard(true);
        self.busy.set(false);
        if close {
            self.close_confirmed.set(true);
            unsafe {
                let _ = DestroyWindow(self.hwnd());
            }
        } else {
            self.deliver_pending_probe();
        }
    }

    // ---- editing -------------------------------------------------------------------

    fn apply_op(&self, op: impl FnOnce(&[u16], usize, usize) -> Edit) {
        let editor = self.editor();
        let text = editor.text();
        let (start, len) = editor.selection();
        let edit = op(&text, start, len);
        editor.apply(&edit);
        editor.focus();
    }

    /// Enter in the editor: list continuation. Returns true when it consumed the key.
    fn on_editor_enter(&self) -> bool {
        let editor = self.editor();
        let (start, len) = editor.selection();
        let text = editor.text();
        match md_ops::on_enter(&text, start, len) {
            Some(edit) => {
                editor.apply(&edit);
                editor.scroll_caret();
                true
            }
            None => false,
        }
    }

    /// Tab / Shift+Tab in the editor: list indentation. Returns true when it consumed the
    /// key; outside a list the key keeps inserting a tab.
    fn on_editor_tab(&self, outdent: bool) -> bool {
        let editor = self.editor();
        let (start, len) = editor.selection();
        let text = editor.text();
        let Some(edit) = md_ops::indent_lines(&text, start, len, outdent) else {
            return false;
        };
        // Shift+Tab on an unindented item changes nothing: no empty undo step for it.
        if edit.text[..] != text[edit.start..edit.end] {
            editor.apply(&edit);
        }
        editor.scroll_caret();
        true
    }

    /// 見出しへ移動: the document's headings as a popup menu at the editor's top-left.
    fn show_outline(&self) {
        let editor = self.editor();
        let text = editor.text();
        let headings = md_ops::headings(&text);
        let cmd = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            if headings.is_empty() {
                let t = wide("見出しがありません");
                let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, pcwstr(&t));
            }
            for (i, h) in headings.iter().enumerate() {
                let title = if h.title.is_empty() { "(空の見出し)" } else { &h.title };
                // `&` would become a mnemonic and a tab would start the shortcut column.
                let title = title.replace('&', "&&").replace('\t', " ");
                let t = wide(&format!("{}{title}", "\u{3000}".repeat(h.level - 1)));
                let _ = AppendMenuW(menu, MF_STRING, i + 1, pcwstr(&t));
            }
            let mut origin = POINT::default();
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(editor.hwnd(), &mut origin);
            let cmd = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
                origin.x,
                origin.y,
                None,
                self.hwnd(),
                None,
            );
            let _ = DestroyMenu(menu);
            cmd.0 as usize
        };
        if let Some(h) = cmd.checked_sub(1).and_then(|i| headings.get(i)) {
            self.select_and_reveal(h.line_start, 0);
        }
        editor.focus();
    }

    /// 行へ移動: asks for a line number and puts the caret at that line's start.
    fn goto_line(&self) {
        let editor = self.editor();
        let text = editor.text();
        let (start, _) = editor.selection();
        let current = md_ops::line_index_at(&text, start) + 1;
        let palette = self.state.borrow().palette;
        match goto_dialog::show(self.hwnd(), current, line_endings::count_lines(&text), palette) {
            Ok(Some(line)) => self.select_and_reveal(md_ops::line_start_of(&text, line - 1), 0),
            Ok(None) => {}
            Err(e) => self.report_error(
                "GotoLine",
                &format!("行へ移動のダイアログを開けませんでした。 ({})", e.message()),
                Some(&e),
            ),
        }
        editor.focus();
    }

    fn show_dropdown(&self, button_id: u16) {
        let ui = self.ui.get();
        let Some(index) = TOOL_BUTTONS.iter().position(|b| b.id == button_id) else {
            return;
        };
        let button = ui.buttons[index];
        let items: &[(u16, &str)] = if button_id == ID_HEADING {
            &[(ID_H1, "見出し1\t# 見出し"), (ID_H2, "見出し2\t## 見出し"), (ID_H3, "見出し3\t### 見出し")]
        } else {
            &[
                (ID_QUOTE, "引用\t> 引用文"),
                (ID_RULE, "水平線\t---"),
                (ID_IMAGE, "画像\t![代替テキスト](URL)"),
                (ID_TABLE, "テーブル"),
                (ID_STRIKE, "取り消し線\t~~取り消し線~~"),
                (ID_CODE_BLOCK, "コードブロック\t```"),
                (ID_TASK, "タスクリスト\t- [ ] タスク"),
            ]
        };
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            for (id, text) in items {
                let t = wide(text);
                let _ = AppendMenuW(menu, MF_STRING, *id as usize, pcwstr(&t));
            }
            let mut rc = RECT::default();
            let _ = GetWindowRect(button, &mut rc);
            let cmd = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
                rc.left,
                rc.bottom,
                None,
                ui.hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
            if cmd.0 != 0 {
                self.command(cmd.0 as u16);
            } else {
                self.editor().focus();
            }
        }
    }

    // ---- help ------------------------------------------------------------------------

    fn show_shortcuts(&self) {
        if let Err(e) = dialogs::show_shortcuts(self.hwnd(), &shortcut_help_text()) {
            self.report_error("Shortcuts", "ショートカット一覧を表示できませんでした。", Some(&e));
        }
        self.editor().focus();
    }

    fn show_about(&self) {
        let text = dialogs::about_text(
            env!("CARGO_PKG_VERSION"),
            env!("MYMDEDIT_COPYRIGHT"),
            preview::runtime_version().as_deref(),
        );
        if let Err(e) = dialogs::show_about(self.hwnd(), &text) {
            self.report_error("About", "バージョン情報を表示できませんでした。", Some(&e));
        }
        self.editor().focus();
    }

    fn open_project_page(&self, url: &str) {
        if let Err(e) = dialogs::open_url(self.hwnd(), url) {
            self.report_error("OpenUrl", &format!("ブラウザでページを開けませんでした: {url}"), Some(&e));
        }
    }

    // ---- find / replace --------------------------------------------------------------

    fn show_find(&self, replace: bool) {
        let ui = self.ui.get();
        if !ui.find_dialog.is_invalid() {
            if self.state.borrow().find_is_replace == replace {
                unsafe {
                    let _ = SetFocus(Some(ui.find_dialog));
                }
                return;
            }
            unsafe {
                if let Err(e) = DestroyWindow(ui.find_dialog) {
                    log::error("DestroyWindow find dialog", &e);
                }
            }
            // Forget it now: should the new dialog fail, a dead handle left here would be
            // "focused" by every later Ctrl+F instead of a new one being made.
            let mut ui = self.ui.get();
            ui.find_dialog = HWND::default();
            self.ui.set(ui);
        }
        // Never a reference to FindData: the dialog writes through the pointers handed to
        // it below for as long as it is open, and a `&mut` they were derived from would be
        // invalidated (under the aliasing rules) by the next read of the same memory, after
        // which the dialog's writes are undefined behaviour. Raw pointers from the one
        // allocation are all of equal standing.
        let data = self.find.get();
        const BUF: usize = 512;
        // Seed the search box with a short single-line selection.
        let editor = self.editor();
        let (start, len) = editor.selection();
        if len > 0 && len < BUF {
            let text = editor.text();
            let sel = &text[start..start + len];
            if !sel.iter().any(|&c| line_endings::is_break(c)) {
                let mut seeded = [0u16; BUF];
                seeded[..len].copy_from_slice(sel);
                unsafe { (&raw mut (*data).find).write(seeded) };
            }
        }
        let old_flags = unsafe { (*data).fr.Flags };
        let flags = if old_flags.0 == 0 { FR_DOWN } else { old_flags & (FR_DOWN | FR_MATCHCASE | FR_WHOLEWORD) };
        let fr = unsafe { &raw mut (*data).fr };
        unsafe {
            fr.write(FINDREPLACEW {
                lStructSize: size_of::<FINDREPLACEW>() as u32,
                hwndOwner: ui.hwnd,
                Flags: flags,
                lpstrFindWhat: PWSTR((&raw mut (*data).find).cast()),
                lpstrReplaceWith: PWSTR((&raw mut (*data).replace).cast()),
                wFindWhatLen: BUF as u16,
                wReplaceWithLen: BUF as u16,
                ..Default::default()
            });
        }
        let dialog = unsafe { if replace { ReplaceTextW(fr) } else { FindTextW(fr) } };
        if dialog.is_invalid() {
            let code = unsafe { CommDlgExtendedError() };
            // The user asked for this dialog; its absence needs explaining, like the others'.
            self.report_error(
                "FindTextW",
                if replace {
                    "置換ダイアログを表示できませんでした。"
                } else {
                    "検索ダイアログを表示できませんでした。"
                },
                Some(&format!("could not create the dialog: CommDlgExtendedError={}", code.0)),
            );
            return;
        }
        self.state.borrow_mut().find_is_replace = replace;
        let mut ui = self.ui.get();
        ui.find_dialog = dialog;
        self.ui.set(ui);
    }

    fn find_message_box(&self, text: &str) {
        let owner = self.ui.get().find_dialog;
        let owner = if owner.is_invalid() { self.hwnd() } else { owner };
        let t = wide(text);
        unsafe {
            MessageBoxW(Some(owner), pcwstr(&t), w!("MyMDedit"), MB_OK | MB_ICONINFORMATION);
        }
    }

    fn select_and_reveal(&self, start: usize, len: usize) {
        let editor = self.editor();
        editor.set_selection(start, len);
        editor.scroll_caret();
    }

    fn on_find_message(&self) {
        // Copy everything out first: the message boxes below run a modal loop in which the
        // dialog (or show_find) can rewrite FindData. Read through the raw pointer, never
        // a reference (see show_find).
        let (flags, query, replacement): (FINDREPLACE_FLAGS, Vec<u16>, Vec<u16>) = unsafe {
            let data = self.find.get();
            let (find, replace) = ((&raw const (*data).find).read(), (&raw const (*data).replace).read());
            let cut = |b: &[u16]| b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())].to_vec();
            ((*data).fr.Flags, cut(&find), cut(&replace))
        };
        if flags.contains(FR_DIALOGTERM) {
            let mut ui = self.ui.get();
            ui.find_dialog = HWND::default();
            self.ui.set(ui);
            return;
        }
        // The find dialog is modeless and its own top-level window, so a modal of ours
        // does not disable it. Acting now would change the text under that modal (行へ移動
        // has already counted the lines; the reload prompt has already read `dirty`).
        // Its message boxes are modals too, so it takes `busy` like any modal command.
        if self.busy.replace(true) {
            unsafe {
                let _ = MessageBeep(MB_OK);
            }
            return;
        }
        self.find_or_replace(flags, query, replacement);
        self.busy.set(false);
        self.deliver_pending_probe();
    }

    fn find_or_replace(&self, flags: FINDREPLACE_FLAGS, query: Vec<u16>, replacement: Vec<u16>) {
        let case = flags.contains(FR_MATCHCASE);
        let whole = flags.contains(FR_WHOLEWORD);
        let editor = self.editor();
        if query.is_empty() {
            self.find_message_box("検索する文字列を入力してください");
            return;
        }

        if flags.contains(FR_REPLACEALL) {
            let text = editor.text();
            let matches = search::find_all(&text, &query, case, whole);
            let Some((start, end, new)) = search::replace_all(&text, &matches, query.len(), &replacement) else {
                self.find_message_box("一致する文字列が見つかりませんでした");
                return;
            };
            // One replacement over the span covering every match: one undo step, one
            // re-render.
            editor.set_selection(start, end - start);
            editor.replace_selection(&new);
            self.find_message_box(&format!("{}件を置換しました", matches.len()));
            return;
        }

        if flags.contains(FR_REPLACE) {
            let (start, len) = editor.selection();
            if len > 0 {
                let text = editor.text();
                if search::is_match_at(&text, start, len, &query, case, whole) {
                    editor.replace_selection(&replacement);
                    editor.set_selection(start + replacement.len(), 0);
                }
            }
        }

        let text = editor.text();
        let matches = search::find_all(&text, &query, case, whole);
        let (start, len) = editor.selection();
        let found = if flags.contains(FR_FINDNEXT) && !flags.contains(FR_DOWN) {
            search::previous(&matches, start)
        } else {
            search::next(&matches, start + len)
        };
        match found {
            Some(i) => self.select_and_reveal(i, query.len()),
            None => self.find_message_box("見つかりませんでした"),
        }
    }

    // ---- dispatch --------------------------------------------------------------------

    fn command(&self, id: u16) {
        // Commands that open modal UI must not nest (e.g. a shortcut forwarded from the
        // WebView while a dialog is already up). A popup menu (TrackPopupMenu) is a modal
        // loop too: a folder-probe result arriving during it must wait.
        let modal = matches!(
            id,
            ID_NEW
                | ID_OPEN
                | ID_SAVE
                | ID_SAVE_AS
                | ID_EXIT
                | ID_SETTINGS
                | ID_GOTO_LINE
                | ID_OUTLINE
                | ID_HEADING
                | ID_MORE
                | ID_SHORTCUTS
                | ID_ABOUT
        ) || (ID_RECENT_BASE..ID_RECENT_END).contains(&id);
        if modal && self.busy.replace(true) {
            return;
        }
        let editor = self.editor();
        match id {
            ID_NEW => self.new_document(),
            ID_OPEN => self.open(),
            ID_SAVE => self.save(),
            ID_SAVE_AS => self.save_as(),
            ID_EXIT => self.exit(),
            ID_UNDO => editor.undo(),
            ID_REDO => editor.redo(),
            ID_CUT => editor.cut(),
            ID_COPY => editor.copy(),
            ID_PASTE => editor.paste(),
            ID_SELECT_ALL => editor.select_all(),
            ID_FIND => self.show_find(false),
            ID_REPLACE => self.show_find(true),
            ID_SETTINGS => self.open_settings(),
            ID_ZOOM_IN => self.zoom_by(ZOOM_STEP),
            ID_ZOOM_OUT => self.zoom_by(-ZOOM_STEP),
            ID_ZOOM_RESET => self.set_zoom(100),
            ID_ERROR_CLOSE => self.dismiss_error(),
            ID_BOLD => self.apply_op(|t, s, l| md_ops::wrap_selection(t, s, l, "**", "**", "太字")),
            ID_ITALIC => self.apply_op(|t, s, l| md_ops::wrap_selection(t, s, l, "*", "*", "斜体")),
            ID_HEADING | ID_MORE => self.show_dropdown(id),
            ID_H1 | ID_H2 | ID_H3 => {
                let level = (id - ID_H1 + 1) as usize;
                self.apply_op(|t, s, _| md_ops::set_heading(t, s, level));
            }
            ID_BULLET => self.apply_op(|t, s, l| md_ops::prefix_lines(t, s, l, false, |_| "- ".into())),
            ID_NUMBERED => self.apply_op(|t, s, l| md_ops::prefix_lines(t, s, l, false, |i| format!("{}. ", i + 1))),
            ID_LINK => self.apply_op(|t, s, l| md_ops::insert_link(t, s, l, "[", "リンクテキスト", "url")),
            ID_CODE => self.apply_op(|t, s, l| md_ops::wrap_selection(t, s, l, "`", "`", "コード")),
            ID_QUOTE => self.apply_op(|t, s, l| md_ops::prefix_lines(t, s, l, false, |_| "> ".into())),
            ID_RULE => self.apply_op(|t, s, l| md_ops::insert_block(t, s, l, "---", true)),
            ID_IMAGE => self.apply_op(|t, s, l| md_ops::insert_link(t, s, l, "![", "代替テキスト", "url")),
            ID_TABLE => self.apply_op(|t, s, l| {
                md_ops::insert_block(t, s, l, "| 見出し1 | 見出し2 |\r| --- | --- |\r| セル1 | セル2 |", true)
            }),
            ID_STRIKE => self.apply_op(|t, s, l| md_ops::wrap_selection(t, s, l, "~~", "~~", "取り消し線")),
            ID_CODE_BLOCK => self.apply_op(md_ops::code_block),
            ID_TASK => self.apply_op(md_ops::toggle_task),
            ID_GOTO_LINE => self.goto_line(),
            ID_OUTLINE => self.show_outline(),
            ID_SHORTCUTS => self.show_shortcuts(),
            ID_HELP_GITHUB => self.open_project_page(dialogs::REPOSITORY_URL),
            ID_HELP_RELEASES => self.open_project_page(&dialogs::releases_url()),
            ID_ABOUT => self.show_about(),
            id if (ID_RECENT_BASE..ID_RECENT_END).contains(&id) => {
                self.open_from_recent((id - ID_RECENT_BASE) as usize)
            }
            _ => {}
        }
        if matches!(id, ID_UNDO | ID_REDO | ID_CUT | ID_COPY | ID_PASTE | ID_SELECT_ALL | ID_ERROR_CLOSE) {
            editor.focus();
        }
        if modal {
            self.busy.set(false);
            self.deliver_pending_probe();
        }
    }

    fn on_dpi_changed(&self, dpi: u32, suggested: &RECT) {
        let mut ui = self.ui.get();
        ui.dpi = dpi;
        self.ui.set(ui);
        self.create_fonts();
        unsafe {
            let _ = SetWindowPos(
                ui.hwnd,
                None,
                suggested.left,
                suggested.top,
                win::width(suggested),
                win::height(suggested),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.layout();
    }
}

// ---- window procedures ------------------------------------------------------------

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(app) = try_app() else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    if app.ui.get().hwnd != hwnd {
        // Messages during CreateWindowEx, before the handles are recorded.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }

    let palette = app.state.borrow().palette;
    if let Some(r) = theme::handle_menu_bar_message(hwnd, msg, wparam, lparam, &palette, app.ui.get().fonts.menu) {
        return r;
    }
    if msg == app.find_message.get() && msg != 0 {
        // Only our own FINDREPLACEW: another process can post this registered message
        // with any lparam, and it is about to be dereferenced.
        let ours = unsafe { &raw const (*app.find.get()).fr };
        if lparam.0 as *const FINDREPLACEW == ours {
            app.on_find_message();
        }
        return LRESULT(0);
    }

    match msg {
        WM_COMMAND => {
            let id = loword(wparam.0);
            let code = hiword(wparam.0) as u32;
            if lparam.0 == app.ui.get().editor.0 as isize {
                if code == windows::Win32::UI::WindowsAndMessaging::EN_CHANGE {
                    app.on_text_changed();
                }
                return LRESULT(0);
            }
            // 0: menu / button click, 1: accelerator, 5: BN_DOUBLECLICKED (an owner-drawn
            // button reports a fast second click this way).
            if matches!(code, 0 | 1 | 5) {
                app.command(id);
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            app.draw_button(unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) });
            LRESULT(1)
        }
        WM_PAINT => {
            app.paint();
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            app.layout();
            LRESULT(0)
        }
        WM_MOVE | WM_MOVING => {
            preview::notify_moved(&app);
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_GETMINMAXINFO => {
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            let dpi = app.ui.get().dpi.max(96);
            info.ptMinTrackSize = POINT { x: scale(640, dpi), y: scale(400, dpi) };
            LRESULT(0)
        }
        WM_SETFOCUS => {
            app.editor().focus();
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // Coming back from another program is when its changes to the file show up.
            // Posted, not handled here: the check may show a dialog, and activation is
            // the middle of a focus change.
            if loword(wparam.0) as u32 != WA_INACTIVE && !app.disk_check_pending.replace(true) {
                unsafe {
                    if let Err(e) = PostMessageW(Some(hwnd), WM_APP_CHECK_DISK, WPARAM(0), LPARAM(0)) {
                        log::error("WM_ACTIVATE PostMessage", &e);
                        app.disk_check_pending.set(false);
                    }
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_APP_CHECK_DISK => {
            app.disk_check_pending.set(false);
            app.check_external_change();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_SCROLL_SYNC => {
            app.poll_scroll();
            LRESULT(0)
        }
        preview::WM_APP_PREVIEW_FAILED => {
            preview::on_process_failed(&app, wparam.0 == 1);
            LRESULT(0)
        }
        preview::WM_APP_PREVIEW_CLOSE => {
            preview::close(&app);
            LRESULT(0)
        }
        WM_APP_RENDER => {
            app.render_pending.set(false);
            app.rerender();
            LRESULT(0)
        }
        WM_APP_FOLDER_PROBED => {
            if let Ok(result) = app.probe_rx.try_recv() {
                *app.pending_probe.borrow_mut() = Some(result);
                app.deliver_pending_probe();
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            // Follow the OS light/dark switch when the theme is システムの既定.
            let area = if lparam.0 != 0 {
                unsafe { PCWSTR(lparam.0 as *const u16).to_string().unwrap_or_default() }
            } else {
                String::new()
            };
            if area == "ImmersiveColorSet" && app.current_settings().theme == Theme::System {
                app.apply_theme();
                app.apply_editor_font();
                app.rerender();
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DPICHANGED => {
            let rect = unsafe { &*(lparam.0 as *const RECT) };
            app.on_dpi_changed(hiword(wparam.0) as u32, rect);
            LRESULT(0)
        }
        WM_CLOSE => {
            app.on_close_request();
            LRESULT(0)
        }
        // Sign-out, shutdown and Restart Manager (installers, Windows Update) end the app
        // through these, never through WM_CLOSE — so the discard prompt must live here too.
        WM_QUERYENDSESSION => LRESULT(app.on_query_end_session() as isize),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                // The session really is ending; the process is terminated after this.
                app.close_confirmed.set(true);
            } else {
                app.release_shutdown_block();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_SCROLL_SYNC);
            }
            preview::close(&app);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

const MK_CONTROL_BIT: usize = 0x0008;
const WHEEL_DELTA: i32 = 120;

/// Zoom steps for a wheel `delta` on top of the `accum`ulated remainder, and the new
/// remainder. High-resolution touchpads send many deltas well under 120; each used to
/// move the zoom a full 10%. A direction change discards the old remainder.
fn wheel_steps(accum: i32, delta: i32) -> (i32, i32) {
    let accum = if accum.signum() * delta.signum() < 0 { 0 } else { accum };
    let total = accum + delta;
    (total / WHEEL_DELTA, total % WHEEL_DELTA)
}
const WM_MOUSELEAVE: u32 = 0x02A3;

type SubclassProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM, usize, usize) -> LRESULT;

/// SetWindowSubclass's contract: the subclass goes before its window does. WM_NCDESTROY
/// is the last message the window gets; DefSubclassProc still forwards it afterwards.
fn remove_subclass(hwnd: HWND, proc: SubclassProc, id: usize) {
    if !unsafe { RemoveWindowSubclass(hwnd, Some(proc), id) }.as_bool() {
        log::error("RemoveWindowSubclass", "subclass was not installed");
    }
}

unsafe extern "system" fn editor_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_NCDESTROY {
        remove_subclass(hwnd, editor_proc, id);
    }
    if let Some(app) = try_app() {
        match msg {
            // Ctrl+wheel drives the app-wide zoom (editor + preview together) instead of
            // RichEdit's own editor-only zoom.
            WM_MOUSEWHEEL if wparam.0 & MK_CONTROL_BIT != 0 => {
                let delta = hiword(wparam.0) as i16 as i32;
                let (steps, rest) = wheel_steps(app.wheel_accum.get(), delta);
                app.wheel_accum.set(rest);
                if steps != 0 {
                    app.zoom_by(steps * ZOOM_STEP);
                }
                return LRESULT(0);
            }
            // RichEdit inserts the line break on the key-down itself, so list continuation
            // has to act there. While an IME is composing, its confirming Enter arrives
            // as VK_PROCESSKEY, never VK_RETURN, so it is left alone.
            WM_KEYDOWN => {
                // The flag only ever covers the character of the key-down that set it.
                app.swallow_char.set(0);
                // Modified Enter / Tab are not plain line breaks or tabs; leave them to
                // RichEdit. (Ctrl+Enter never gets here: it is the タスク accelerator.)
                let modified = unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 || GetKeyState(VK_MENU.0 as i32) < 0 };
                let consumed = match wparam.0 {
                    _ if modified => None,
                    k if k == VK_RETURN.0 as usize => app.on_editor_enter().then_some(0x0D),
                    k if k == VK_TAB.0 as usize => {
                        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
                        app.on_editor_tab(shift).then_some(0x09)
                    }
                    _ => None,
                };
                if let Some(ch) = consumed {
                    // TranslateMessage has already queued the matching character.
                    app.swallow_char.set(ch);
                    return LRESULT(0);
                }
            }
            WM_CHAR if wparam.0 != 0 && wparam.0 == app.swallow_char.get() as usize => {
                app.swallow_char.set(0);
                return LRESULT(0);
            }
            _ => {}
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn button_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_NCDESTROY {
        remove_subclass(hwnd, button_proc, id);
    }
    if let Some(app) = try_app() {
        match msg {
            WM_MOUSEMOVE if app.hovered.get() != hwnd => {
                let previous = app.hovered.replace(hwnd);
                unsafe {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    if !previous.is_invalid() {
                        let _ = InvalidateRect(Some(previous), None, false);
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            WM_MOUSELEAVE if app.hovered.get() == hwnd => {
                app.hovered.set(HWND::default());
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            _ => {}
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::push_banner_message;

    #[test]
    fn banner_keeps_recent_messages() {
        let mut m = Vec::new();
        push_banner_message(&mut m, "a");
        push_banner_message(&mut m, "a");
        push_banner_message(&mut m, "b");
        push_banner_message(&mut m, "c");
        push_banner_message(&mut m, "d");
        assert_eq!(m, vec!["b", "c", "d"]);
    }

    #[test]
    fn every_shortcut_is_in_the_help_list() {
        for &(_, _, _, cmd) in super::ACCELERATORS {
            assert!(super::SHORTCUT_HELP.iter().any(|(id, _, _)| *id == Some(cmd)), "command {cmd} has no help row");
        }
        let text = super::shortcut_help_text();
        assert!(text.starts_with("新規: Ctrl+N\n開く: Ctrl+O"));
        assert_eq!(text.lines().count(), super::SHORTCUT_HELP.len());
    }

    #[test]
    fn wheel_steps_accumulate_small_deltas() {
        assert_eq!(super::wheel_steps(0, 40), (0, 40));
        assert_eq!(super::wheel_steps(80, 40), (1, 0));
        assert_eq!(super::wheel_steps(0, 240), (2, 0));
        assert_eq!(super::wheel_steps(40, -60), (0, -60)); // direction change drops the remainder
        assert_eq!(super::wheel_steps(-90, -40), (-1, -10));
    }
}
