//! Thin wrappers over OS-native dialogs and lists.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{COLORREF, ERROR_CANCELLED, HWND, LPARAM, S_OK, WPARAM};
use windows::Win32::Graphics::Gdi::{DEFAULT_CHARSET, EnumFontFamiliesExW, GetDC, LOGFONTW, ReleaseDC, TEXTMETRICW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{CC_RGBINIT, CHOOSECOLORW, ChooseColorW};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOGCONFIG_1,
    TD_INFORMATION_ICON, TDCBF_CANCEL_BUTTON, TDCBF_OK_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_ENABLE_HYPERLINKS,
    TDF_POSITION_RELATIVE_TO_WINDOW, TDN_HYPERLINK_CLICKED, TaskDialogIndirect,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOLDERID_Documents, FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, IShellItem,
    KF_FLAG_DEFAULT, SHCreateItemFromParsingName, SHGetKnownFolderItem, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HRESULT, HSTRING, PCWSTR, w};

use crate::log;
use crate::settings::limits;

fn is_cancelled(e: &windows::core::Error) -> bool {
    e.code() == HRESULT::from_win32(ERROR_CANCELLED.0)
}

/// Documents as the folder a file dialog starts in when it has nothing better. A
/// convenience: without it the dialog picks its own, so a failure is only logged.
fn set_default_folder(dlg: &IFileDialog) {
    let set = unsafe {
        SHGetKnownFolderItem::<IShellItem>(&FOLDERID_Documents, KF_FLAG_DEFAULT, None)
            .and_then(|docs| dlg.SetDefaultFolder(&docs))
    };
    if let Err(e) = set {
        log::error("SetDefaultFolder(Documents)", &e);
    }
}

fn item_path(item: &IShellItem) -> windows::core::Result<PathBuf> {
    unsafe {
        let p = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let s = p.to_string();
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(PathBuf::from(s?))
    }
}

/// The OS file-open dialog, filtered to Markdown/text. `folder` is applied with
/// `IFileDialog::SetFolder`, which — unlike `SetDefaultFolder` — is not overridden by
/// the folder the user last picked. `Ok(None)` means the user cancelled.
pub fn open_file(owner: HWND, folder: Option<&Path>) -> windows::core::Result<Option<PathBuf>> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        let filters = [COMDLG_FILTERSPEC {
            pszName: w!("Markdown / テキスト (*.md;*.markdown;*.txt)"),
            pszSpec: w!("*.md;*.markdown;*.txt"),
        }];
        dlg.SetFileTypes(&filters)?;
        set_default_folder(&dlg);
        // Only where the dialog starts: a folder gone since it was checked (or a dropped
        // network drive) is no reason to fail before the user has picked anything.
        if let Some(folder) = folder {
            let set = SHCreateItemFromParsingName(&HSTRING::from(folder.as_os_str()), None)
                .and_then(|item: IShellItem| dlg.SetFolder(&item));
            if let Err(e) = set {
                log::error(&format!("open_file SetFolder({})", folder.display()), &e);
            }
        }
        match dlg.Show(Some(owner)) {
            Err(e) if is_cancelled(&e) => return Ok(None),
            r => r?,
        }
        item_path(&dlg.GetResult()?).map(Some)
    }
}

/// The OS save dialog for a `.md` file. `Ok(None)` means the user cancelled.
pub fn save_file(owner: HWND, suggested_name: &str) -> windows::core::Result<Option<PathBuf>> {
    unsafe {
        let dlg: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
        let filters = [COMDLG_FILTERSPEC { pszName: w!("Markdown"), pszSpec: w!("*.md") }];
        dlg.SetFileTypes(&filters)?;
        dlg.SetDefaultExtension(w!("md"))?;
        let stem = Path::new(suggested_name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        dlg.SetFileName(&HSTRING::from(stem))?;
        set_default_folder(&dlg);
        match dlg.Show(Some(owner)) {
            Err(e) if is_cancelled(&e) => return Ok(None),
            r => r?,
        }
        item_path(&dlg.GetResult()?).map(Some)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiscardChoice {
    Save,
    Discard,
    Cancel,
}

const BTN_SAVE: i32 = 100;
const BTN_DISCARD: i32 = 101;

/// 「保存する / 保存しない / キャンセル」.
pub fn confirm_discard(owner: HWND) -> windows::core::Result<DiscardChoice> {
    let buttons = [
        TASKDIALOG_BUTTON { nButtonID: BTN_SAVE, pszButtonText: w!("保存する") },
        TASKDIALOG_BUTTON { nButtonID: BTN_DISCARD, pszButtonText: w!("保存しない") },
    ];
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: w!("MyMDedit"),
        pszMainInstruction: w!("変更を保存しますか?"),
        pszContent: w!("編集内容が保存されていません。"),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: BTN_SAVE,
        ..Default::default()
    };
    let mut pressed = 0;
    unsafe { TaskDialogIndirect(&config, Some(&mut pressed), None, None)? };
    Ok(match pressed {
        BTN_SAVE => DiscardChoice::Save,
        BTN_DISCARD => DiscardChoice::Discard,
        _ => DiscardChoice::Cancel,
    })
}

const BTN_YES: i32 = 102;
const BTN_NO: i32 = 103;

/// A two-way question: `yes`, or `no` (キャンセル when `None`). `Ok(true)` only for
/// `yes`; Esc and the close box count as `no`.
fn ask(owner: HWND, instruction: &str, content: &str, yes: &str, no: Option<&str>) -> windows::core::Result<bool> {
    let (instruction, content, yes) = (HSTRING::from(instruction), HSTRING::from(content), HSTRING::from(yes));
    let no = no.map(HSTRING::from);
    let mut buttons = vec![TASKDIALOG_BUTTON { nButtonID: BTN_YES, pszButtonText: PCWSTR(yes.as_ptr()) }];
    if let Some(no) = &no {
        buttons.push(TASKDIALOG_BUTTON { nButtonID: BTN_NO, pszButtonText: PCWSTR(no.as_ptr()) });
    }
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW,
        dwCommonButtons: if no.is_some() { Default::default() } else { TDCBF_CANCEL_BUTTON },
        pszWindowTitle: w!("MyMDedit"),
        pszMainInstruction: PCWSTR(instruction.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: BTN_YES,
        ..Default::default()
    };
    let mut pressed = 0;
    unsafe { TaskDialogIndirect(&config, Some(&mut pressed), None, None)? };
    Ok(pressed == BTN_YES)
}

/// The open file was changed by another program: 「読み込み直す / このまま」.
pub fn confirm_reload(owner: HWND, file_name: &str, dirty: bool) -> windows::core::Result<bool> {
    let content = if dirty {
        "読み込み直すと、まだ保存していない編集内容は失われます。"
    } else {
        "読み込み直すと、変更後の内容を表示します。"
    };
    ask(
        owner,
        &format!("{file_name} は別のアプリで変更されました。読み込み直しますか?"),
        content,
        "読み込み直す",
        Some("このまま"),
    )
}

/// Saving would overwrite changes another program made: 「上書きする / キャンセル」.
pub fn confirm_overwrite(owner: HWND, file_name: &str) -> windows::core::Result<bool> {
    ask(
        owner,
        &format!("{file_name} は開いたあとに別のアプリで変更されています。上書きしますか?"),
        "上書きすると、別のアプリで加えられた変更は失われます。",
        "上書きする",
        None,
    )
}

/// The project's page: `repository` in Cargo.toml.
const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// `license` in Cargo.toml, as the exe's VERSIONINFO also shows it.
const LICENSE: &str = env!("CARGO_PKG_LICENSE");

/// The only pages the app ever opens in a browser. An enum rather than a URL string,
/// so nothing that came from a document can be handed to the shell through here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectPage {
    Repository,
    /// GitHub redirects it to the newest release.
    LatestRelease,
}

impl ProjectPage {
    pub fn url(self) -> String {
        match self {
            ProjectPage::Repository => REPOSITORY_URL.to_string(),
            ProjectPage::LatestRelease => format!("{REPOSITORY_URL}/releases/latest"),
        }
    }
}

/// Opens `page` in the default browser. The shell's own error UI is off: a failure is
/// returned (with the error ShellExecuteEx sets) for the caller to show on the banner.
pub fn open_project_page(owner: HWND, page: ProjectPage) -> windows::core::Result<()> {
    let url = HSTRING::from(page.url());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        hwnd: owner,
        lpVerb: w!("open"),
        lpFile: PCWSTR(url.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }
}

/// The body of バージョン情報. `webview2` is the installed runtime's version, if any.
pub fn about_text(version: &str, copyright: &str, webview2: Option<&str>) -> String {
    let host = REPOSITORY_URL.trim_start_matches("https://");
    format!(
        "バージョン {version}\nWindows 向けのシンプルな Markdown エディタ\n\n{copyright}\n{LICENSE} License\n\n\
         WebView2 ランタイム: {}\n\n<a href=\"{REPOSITORY_URL}\">{host}</a>",
        webview2.unwrap_or("見つかりません")
    )
}

/// 「バージョン情報」: the app's icon, `content` from [`about_text`], and its link opening
/// the project page. `Ok(Some(e))`: the dialog was shown, but its link could not be
/// opened — for the caller to report, like the menu's own 「GitHub のページを開く」.
pub fn show_about(owner: HWND, content: &str) -> windows::core::Result<Option<windows::core::Error>> {
    let link_error: Cell<Option<windows::core::Error>> = Cell::new(None);
    let content = HSTRING::from(content);
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        hInstance: unsafe { GetModuleHandleW(None)? }.into(),
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW | TDF_ENABLE_HYPERLINKS,
        dwCommonButtons: TDCBF_OK_BUTTON,
        pszWindowTitle: w!("バージョン情報"),
        // MAKEINTRESOURCE(1): the app icon (res/app.rc), not an address.
        Anonymous1: TASKDIALOGCONFIG_0 { pszMainIcon: PCWSTR(std::ptr::without_provenance(1)) },
        pszMainInstruction: w!("MyMDedit"),
        pszContent: PCWSTR(content.as_ptr()),
        Anonymous2: TASKDIALOGCONFIG_1 { pszFooterIcon: TD_INFORMATION_ICON },
        pszFooter: w!("同梱している第三者のソフトウェアのライセンスは THIRD-PARTY-NOTICES.txt にあります。"),
        pfCallback: Some(about_callback),
        // Outlives the dialog: TaskDialogIndirect returns only once it is closed.
        lpCallbackData: &raw const link_error as isize,
        ..Default::default()
    };
    unsafe { TaskDialogIndirect(&config, None, None, None)? };
    Ok(link_error.into_inner())
}

unsafe extern "system" fn about_callback(
    hwnd: HWND,
    msg: TASKDIALOG_NOTIFICATIONS,
    _: WPARAM,
    lparam: LPARAM,
    data: isize,
) -> HRESULT {
    if msg == TDN_HYPERLINK_CLICKED {
        // The href as written in about_text; anything else is not followed.
        let href = unsafe { PCWSTR(lparam.0 as *const u16).to_string() }.unwrap_or_default();
        if href == ProjectPage::Repository.url()
            && let Err(e) = open_project_page(hwnd, ProjectPage::Repository)
        {
            log::error("about open_project_page", &e);
            // show_about's `link_error`, alive for the whole dialog.
            let link_error = unsafe { &*(data as *const Cell<Option<windows::core::Error>>) };
            link_error.set(Some(e));
        }
    }
    S_OK
}

/// 「ショートカット一覧」: `content` is the list, one line per shortcut.
pub fn show_shortcuts(owner: HWND, content: &str) -> windows::core::Result<()> {
    let content = HSTRING::from(content);
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW,
        dwCommonButtons: TDCBF_OK_BUTTON,
        pszWindowTitle: w!("ショートカット一覧"),
        pszMainInstruction: w!("キーボードショートカット"),
        pszContent: PCWSTR(content.as_ptr()),
        ..Default::default()
    };
    unsafe { TaskDialogIndirect(&config, None, None, None) }
}

pub fn parse_hex_color(hex: &str) -> Option<COLORREF> {
    let h = hex.strip_prefix('#')?;
    let full: String = match h.len() {
        3 => h.chars().flat_map(|c| [c, c]).collect(),
        6 => h.to_string(),
        _ => return None,
    };
    let v = u32::from_str_radix(&full, 16).ok()?;
    Some(crate::win::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn format_hex_color(c: COLORREF) -> String {
    format!("#{:02X}{:02X}{:02X}", c.0 & 0xFF, (c.0 >> 8) & 0xFF, (c.0 >> 16) & 0xFF)
}

/// What the color dialog shows in a custom-color slot nobody has filled.
const EMPTY_CUSTOM_COLOR: COLORREF = COLORREF(0x00FF_FFFF);

/// The color dialog's custom-color slots. ChooseColorW reads exactly 16 through a bare
/// pointer, and the settings keep as many: the one bound, checked against the other.
pub type CustomColors = [COLORREF; limits::CUSTOM_COLOR_COUNT];
const _: () = assert!(limits::CUSTOM_COLOR_COUNT == 16, "ChooseColorW's lpCustColors holds 16 colors");

/// The color dialog's custom-color slots from the saved list; the rest are empty.
pub fn custom_color_slots(saved: &[String]) -> CustomColors {
    let mut slots = [EMPTY_CUSTOM_COLOR; limits::CUSTOM_COLOR_COUNT];
    for (slot, hex) in slots.iter_mut().zip(saved) {
        if let Some(c) = parse_hex_color(hex) {
            *slot = c;
        }
    }
    slots
}

/// The slots as they are saved: trailing empty slots are dropped, since
/// `custom_color_slots` fills them back in — so settings.json lists only what was picked.
pub fn custom_colors_to_save(slots: &CustomColors) -> Vec<String> {
    let used = slots.iter().rposition(|&c| c != EMPTY_CUSTOM_COLOR).map_or(0, |i| i + 1);
    slots[..used].iter().map(|&c| format_hex_color(c)).collect()
}

/// The classic Windows 「色の設定」 dialog (basic colors + 16 custom colors).
/// `custom` is updated in place whatever the outcome — the native dialog keeps custom
/// color edits even when cancelled. Returns the chosen color, or `None` on cancel.
pub fn choose_color(owner: HWND, initial: COLORREF, custom: &mut CustomColors) -> Option<COLORREF> {
    let mut cc = CHOOSECOLORW {
        lStructSize: size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: initial,
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT,
        ..Default::default()
    };
    unsafe { ChooseColorW(&mut cc).as_bool().then_some(cc.rgbResult) }
}

unsafe extern "system" fn font_proc(lf: *const LOGFONTW, _tm: *const TEXTMETRICW, _type: u32, lparam: LPARAM) -> i32 {
    unsafe {
        let names = &mut *(lparam.0 as *mut Vec<String>);
        let face = &(*lf).lfFaceName;
        let len = face.iter().position(|&c| c == 0).unwrap_or(face.len());
        let name = String::from_utf16_lossy(&face[..len]);
        // '@'-prefixed families are the vertical-writing variants of CJK fonts.
        if !name.is_empty() && !name.starts_with('@') {
            names.push(name);
        }
    }
    1
}

/// Every installed font family, sorted and deduplicated. Empty on failure.
pub fn installed_fonts() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    unsafe {
        let hdc = GetDC(None);
        let lf = LOGFONTW { lfCharSet: DEFAULT_CHARSET, ..Default::default() };
        EnumFontFamiliesExW(hdc, &lf, Some(font_proc), LPARAM(&mut names as *mut _ as isize), 0);
        ReleaseDC(None, hdc);
    }
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

/// The first family of a CSS-style list (`"Cascadia Mono, Consolas"`) that is installed,
/// for controls that take a single face name.
pub fn resolve_font<'a>(list: &'a str, installed: &[String], fallback: &'a str) -> String {
    let candidates: Vec<&str> =
        list.split(',').map(|s| s.trim().trim_matches(['"', '\''])).filter(|s| !s.is_empty()).collect();
    candidates
        .iter()
        .find(|c| installed.iter().any(|f| f.eq_ignore_ascii_case(c)))
        .or(candidates.first().filter(|_| installed.is_empty()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_lists_version_runtime_and_a_link_to_the_project() {
        let t = about_text("1.2.3", "Copyright (c) 2026 X", Some("141.0.1"));
        assert!(t.starts_with("バージョン 1.2.3\n"));
        assert!(t.contains("Copyright (c) 2026 X\nMIT License"));
        assert!(t.contains("WebView2 ランタイム: 141.0.1"));
        assert!(t.ends_with(&format!("<a href=\"{REPOSITORY_URL}\">github.com/naktomo-svg/MyMDedit</a>")));
        assert!(about_text("1", "c", None).contains("WebView2 ランタイム: 見つかりません"));
        assert_eq!(ProjectPage::Repository.url(), "https://github.com/naktomo-svg/MyMDedit");
        assert_eq!(ProjectPage::LatestRelease.url(), "https://github.com/naktomo-svg/MyMDedit/releases/latest");
    }

    #[test]
    fn hex_round_trip() {
        let c = parse_hex_color("#1E2F3A").unwrap();
        assert_eq!(format_hex_color(c), "#1E2F3A");
        assert_eq!(format_hex_color(parse_hex_color("#abc").unwrap()), "#AABBCC");
        assert!(parse_hex_color("red").is_none());
    }

    #[test]
    fn custom_colors_save_only_the_used_slots() {
        // Never opened the palette, or emptied it: nothing to save.
        assert!(custom_colors_to_save(&custom_color_slots(&[])).is_empty());
        // A white in the middle is a real pick; only the empty tail goes.
        let saved: Vec<String> = ["#112233", "#FFFFFF", "#445566"].map(String::from).to_vec();
        let slots = custom_color_slots(&saved);
        assert_eq!(custom_colors_to_save(&slots), saved);
        // Saving and loading again gives the dialog the same 16 slots.
        assert_eq!(custom_color_slots(&custom_colors_to_save(&slots)), slots);
    }

    #[test]
    fn resolves_first_installed_font() {
        let installed = vec!["Consolas".to_string(), "Segoe UI".to_string()];
        assert_eq!(resolve_font("Cascadia Mono, Consolas", &installed, "x"), "Consolas");
        assert_eq!(resolve_font("Nope", &installed, "Segoe UI"), "Segoe UI");
    }
}
