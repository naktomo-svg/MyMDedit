//! The Markdown editor: a RichEdit control in plain-text mode.
//!
//! RichEdit rather than the classic EDIT control for multi-level undo/redo, no 32K
//! limit and native zoom. In plain-text mode it stores every line break as a lone `\r`
//! — the same representation WinUI's `TextBox` used — and selection offsets are UTF-16
//! code units into that text, which is what `md_ops` and `search` operate on.

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::Win32::UI::Controls::RichEdit::{
    CFM_COLOR, CFM_FACE, CFM_SIZE, CHARFORMAT2W, CHARRANGE, EM_EXGETSEL, EM_EXLIMITTEXT, EM_EXSETSEL,
    EM_GETTEXTEX, EM_GETTEXTLENGTHEX, EM_REDO, EM_SETBKGNDCOLOR, EM_SETCHARFORMAT, EM_SETEVENTMASK, EM_SETTEXTEX,
    EM_SETTEXTMODE, EM_SETZOOM, ENM_CHANGE, GETTEXTEX, GETTEXTLENGTHEX, GT_DEFAULT, GTL_NUMCHARS, GTL_PRECISE, SCF_ALL,
    SCF_DEFAULT, SETTEXTEX, ST_DEFAULT, TM_MULTILEVELUNDO, TM_PLAINTEXT,
};
use windows::Win32::UI::Controls::{EM_REPLACESEL, EM_SCROLLCARET, EM_SETRECT, EM_SETSEL, EM_UNDO};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, ES_AUTOVSCROLL, ES_MULTILINE, ES_NOHIDESEL, ES_WANTRETURN, GetClientRect, GetScrollInfo, HMENU,
    SB_VERT, SCROLLINFO, SIF_ALL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COPY, WM_CUT, WM_PASTE, WS_CHILD, WS_TABSTOP,
    WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCSTR, w};

use crate::md_ops::Edit;
use crate::win::{send, wide};

const CP_UTF16: u32 = 1200;

/// A RichEdit selection as (start, length), kept inside the text. RichEdit can report
/// a position past the last character (its final paragraph mark), and slicing the text
/// with it would panic inside a window procedure — which aborts the process.
pub fn clamp_selection(a: i32, b: i32, len: usize) -> (usize, usize) {
    let a = (a.max(0) as usize).min(len);
    let b = (b.max(0) as usize).min(len);
    (a.min(b), a.abs_diff(b))
}

#[derive(Clone, Copy)]
pub struct Editor(pub HWND);

impl Editor {
    pub fn create(parent: HWND, id: usize) -> windows::core::Result<Self> {
        unsafe {
            LoadLibraryExW(w!("Msftedit.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)?;
            let style = WS_CHILD
                | WS_VISIBLE
                | WS_VSCROLL
                | WS_TABSTOP
                | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN | ES_NOHIDESEL) as u32);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("RICHEDIT50W"),
                w!(""),
                style,
                0,
                0,
                0,
                0,
                Some(parent),
                Some(HMENU(id as *mut _)),
                None,
                None,
            )?;
            let editor = Editor(hwnd);
            // Must be set while the control is empty.
            send(hwnd, EM_SETTEXTMODE, (TM_PLAINTEXT.0 | TM_MULTILEVELUNDO.0) as usize, 0);
            // The default limit is 32K characters.
            send(hwnd, EM_EXLIMITTEXT, 0, 0x7FFF_FFFE);
            send(hwnd, EM_SETEVENTMASK, 0, ENM_CHANGE as isize);
            Ok(editor)
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.0
    }

    pub fn len(&self) -> usize {
        let gtl = GETTEXTLENGTHEX { flags: GTL_NUMCHARS | GTL_PRECISE, codepage: CP_UTF16 };
        send(self.0, EM_GETTEXTLENGTHEX, &gtl as *const _ as usize, 0).max(0) as usize
    }

    /// The whole text, line breaks as lone `\r`.
    pub fn text(&self) -> Vec<u16> {
        let len = self.len();
        let mut buf = vec![0u16; len + 1];
        let gt = GETTEXTEX {
            cb: (buf.len() * 2) as u32,
            flags: GT_DEFAULT,
            codepage: CP_UTF16,
            lpDefaultChar: PCSTR::null(),
            lpUsedDefChar: std::ptr::null_mut(),
        };
        let n = send(self.0, EM_GETTEXTEX, &gt as *const _ as usize, buf.as_mut_ptr() as isize).max(0) as usize;
        buf.truncate(n.min(len));
        buf
    }

    /// Replaces the whole text and clears the undo history.
    pub fn set_text(&self, text: &str) {
        let buf = wide(text);
        let st = SETTEXTEX { flags: ST_DEFAULT, codepage: CP_UTF16 };
        send(self.0, EM_SETTEXTEX, &st as *const _ as usize, buf.as_ptr() as isize);
        self.set_selection(0, 0);
    }

    /// (start, length) in UTF-16 units.
    pub fn selection(&self) -> (usize, usize) {
        let mut cr = CHARRANGE::default();
        send(self.0, EM_EXGETSEL, 0, &mut cr as *mut _ as isize);
        clamp_selection(cr.cpMin, cr.cpMax, self.len())
    }

    pub fn set_selection(&self, start: usize, len: usize) {
        let cr = CHARRANGE { cpMin: start as i32, cpMax: (start + len) as i32 };
        send(self.0, EM_EXSETSEL, 0, &cr as *const _ as isize);
    }

    /// Replaces the selection as one undoable step.
    pub fn replace_selection(&self, text: &[u16]) {
        let mut buf = text.to_vec();
        buf.push(0);
        send(self.0, EM_REPLACESEL, 1, buf.as_ptr() as isize);
    }

    pub fn apply(&self, edit: &Edit) {
        self.set_selection(edit.start, edit.end - edit.start);
        self.replace_selection(&edit.text);
        self.set_selection(edit.sel_start, edit.sel_len);
    }

    pub fn scroll_caret(&self) {
        send(self.0, EM_SCROLLCARET, 0, 0);
    }

    pub fn focus(&self) {
        unsafe {
            let _ = SetFocus(Some(self.0));
        }
    }

    pub fn undo(&self) {
        send(self.0, EM_UNDO, 0, 0);
    }

    pub fn redo(&self) {
        send(self.0, EM_REDO, 0, 0);
    }

    pub fn cut(&self) {
        send(self.0, WM_CUT, 0, 0);
    }

    pub fn copy(&self) {
        send(self.0, WM_COPY, 0, 0);
    }

    pub fn paste(&self) {
        send(self.0, WM_PASTE, 0, 0);
    }

    pub fn select_all(&self) {
        send(self.0, EM_SETSEL, 0, -1);
    }

    /// `size_px` is in device-independent pixels at 96 DPI (what the settings store);
    /// RichEdit takes twips and scales them to the monitor itself.
    pub fn set_font(&self, face: &str, size_px: f64, color: COLORREF) {
        let mut cf = CHARFORMAT2W::default();
        cf.Base.cbSize = size_of::<CHARFORMAT2W>() as u32;
        cf.Base.dwMask = CFM_FACE | CFM_SIZE | CFM_COLOR;
        cf.Base.yHeight = (size_px * 15.0).round() as i32;
        cf.Base.crTextColor = color;
        let face: Vec<u16> = face.encode_utf16().take(31).collect();
        cf.Base.szFaceName[..face.len()].copy_from_slice(&face);
        send(self.0, EM_SETCHARFORMAT, SCF_DEFAULT as usize, &cf as *const _ as isize);
        send(self.0, EM_SETCHARFORMAT, SCF_ALL as usize, &cf as *const _ as isize);
    }

    pub fn set_background(&self, color: COLORREF) {
        send(self.0, EM_SETBKGNDCOLOR, 0, color.0 as isize);
    }

    pub fn set_zoom(&self, percent: i32) {
        if percent == 100 {
            send(self.0, EM_SETZOOM, 0, 0);
        } else {
            send(self.0, EM_SETZOOM, percent as usize, 100);
        }
    }

    /// Inset the text from the control's edges by `padding` device pixels.
    pub fn set_padding(&self, padding: i32) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.0, &mut rc);
        }
        let inner = RECT {
            left: rc.left + padding,
            top: rc.top + padding,
            right: (rc.right - padding).max(rc.left + padding + 1),
            bottom: (rc.bottom - padding).max(rc.top + padding + 1),
        };
        send(self.0, EM_SETRECT, 0, &inner as *const _ as isize);
    }

    /// Vertical scroll position as 0 (top) ..= 1 (bottom).
    pub fn scroll_ratio(&self) -> f64 {
        let mut si = SCROLLINFO { cbSize: size_of::<SCROLLINFO>() as u32, fMask: SIF_ALL, ..Default::default() };
        unsafe {
            if GetScrollInfo(self.0, SB_VERT, &mut si).is_err() {
                return 0.0;
            }
        }
        // nPos rather than EM_GETSCROLLPOS: the latter is limited to 16 bits, so on a long
        // document the preview stopped following past ~65535 pixels.
        let scrollable = si.nMax - si.nMin + 1 - si.nPage as i32;
        if scrollable <= 0 { 0.0 } else { ((si.nPos - si.nMin) as f64 / scrollable as f64).clamp(0.0, 1.0) }
    }
}

#[cfg(test)]
mod tests {
    use super::clamp_selection;

    #[test]
    fn selection_never_exceeds_text() {
        assert_eq!(clamp_selection(2, 5, 10), (2, 3));
        assert_eq!(clamp_selection(5, 2, 10), (2, 3));
        assert_eq!(clamp_selection(0, 11, 10), (0, 10)); // RichEdit's final EOP
        assert_eq!(clamp_selection(-1, 3, 10), (0, 3));
    }
}
