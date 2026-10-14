//! MyMDedit — a simple Markdown editor for Windows (Win32 + WebView2).
//! Edit Markdown on the left; a live HTML preview renders on the right.

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod dialogs;
mod document;
mod editor;
mod line_endings;
mod log;
mod md_ops;
mod preview;
mod render;
mod search;
mod settings;
mod settings_dialog;
mod theme;
mod win;

use std::path::PathBuf;

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::Controls::{
    ICC_BAR_CLASSES, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES, ICC_UPDOWN_CLASS, INITCOMMONCONTROLSEX, InitCommonControlsEx,
};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{HSTRING, w};

fn main() {
    unsafe {
        // WebView2 and the shell dialogs both require an STA thread.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok();
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_BAR_CLASSES | ICC_UPDOWN_CLASS | ICC_TAB_CLASSES,
        });
    }

    // Explorer passes the double-clicked file as the first argument.
    let launch_file = std::env::args_os().nth(1).map(PathBuf::from);
    if let Err(e) = app::run(launch_file) {
        log::error("startup", &e);
        unsafe {
            MessageBoxW(None, &HSTRING::from(format!("MyMDedit を起動できませんでした。\n{e}")), w!("MyMDedit"), MB_OK | MB_ICONERROR);
        }
    }
}
