//! Thin wrappers over OS-native dialogs and lists.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{COLORREF, ERROR_CANCELLED, HWND, LPARAM};
use windows::Win32::Graphics::Gdi::{DEFAULT_CHARSET, EnumFontFamiliesExW, GetDC, LOGFONTW, ReleaseDC, TEXTMETRICW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Controls::Dialogs::{CC_RGBINIT, CHOOSECOLORW, ChooseColorW};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TDCBF_CANCEL_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION,
    TDF_POSITION_RELATIVE_TO_WINDOW, TaskDialogIndirect,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOLDERID_Documents, FileOpenDialog, FileSaveDialog, IFileOpenDialog, IFileSaveDialog, IShellItem, KF_FLAG_DEFAULT,
    SHCreateItemFromParsingName, SHGetKnownFolderItem, SIGDN_FILESYSPATH,
};
use windows::core::{HRESULT, HSTRING, w};

fn is_cancelled(e: &windows::core::Error) -> bool {
    e.code() == HRESULT::from_win32(ERROR_CANCELLED.0)
}

fn documents_folder() -> Option<IShellItem> {
    unsafe { SHGetKnownFolderItem(&FOLDERID_Documents, KF_FLAG_DEFAULT, None).ok() }
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
        if let Some(docs) = documents_folder() {
            dlg.SetDefaultFolder(&docs)?;
        }
        if let Some(folder) = folder {
            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(folder.as_os_str()), None)?;
            dlg.SetFolder(&item)?;
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
        if let Some(docs) = documents_folder() {
            dlg.SetDefaultFolder(&docs)?;
        }
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

/// The classic Windows 「色の設定」 dialog (basic colors + 16 custom colors).
/// `custom` is updated in place whatever the outcome — the native dialog keeps custom
/// color edits even when cancelled. Returns the chosen color, or `None` on cancel.
pub fn choose_color(owner: HWND, initial: COLORREF, custom: &mut [COLORREF; 16]) -> Option<COLORREF> {
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
    let candidates: Vec<&str> = list.split(',').map(|s| s.trim().trim_matches(['"', '\''])).filter(|s| !s.is_empty()).collect();
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
    fn hex_round_trip() {
        let c = parse_hex_color("#1E2F3A").unwrap();
        assert_eq!(format_hex_color(c), "#1E2F3A");
        assert_eq!(format_hex_color(parse_hex_color("#abc").unwrap()), "#AABBCC");
        assert!(parse_hex_color("red").is_none());
    }

    #[test]
    fn resolves_first_installed_font() {
        let installed = vec!["Consolas".to_string(), "Segoe UI".to_string()];
        assert_eq!(resolve_font("Cascadia Mono, Consolas", &installed, "x"), "Consolas");
        assert_eq!(resolve_font("Nope", &installed, "Segoe UI"), "Segoe UI");
    }
}
