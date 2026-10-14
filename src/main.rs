//! MyMDedit — a simple Markdown editor for Windows (Win32 + WebView2).
//! Edit Markdown on the left; a live HTML preview renders on the right.

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod dialogs;
mod document;
mod editor;
mod goto_dialog;
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
use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories};
use windows::Win32::UI::Controls::{
    ICC_BAR_CLASSES, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES, ICC_UPDOWN_CLASS, INITCOMMONCONTROLSEX,
    InitCommonControlsEx,
};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{HSTRING, w};

fn main() {
    // Security, and first on purpose: by default a DLL loaded by name is looked for in
    // the exe's own folder before System32, and this exe is typically run straight from
    // Downloads, where a browser may already have saved a dwmapi.dll or Msftedit.dll
    // (all four of the names this app pulls in were shown to load that way in the
    // 2026-10-14 audit, F9). This covers every later load by name, including those the
    // system DLLs make on our behalf; static imports are covered by /DEPENDENTLOADFLAG
    // in build.rs.
    if let Err(e) = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) } {
        log::error("SetDefaultDllDirectories", &e);
    }
    log::install_panic_hook();
    unsafe {
        // WebView2 and the shell dialogs both require an STA thread.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok();
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_BAR_CLASSES | ICC_UPDOWN_CLASS | ICC_TAB_CLASSES,
        });
    }

    // Explorer passes the double-clicked file as the first argument.
    // (DLL search policy: see the top of this function, build.rs and .cargo/config.toml.)
    let launch_file = std::env::args_os().nth(1).map(PathBuf::from);
    if let Err(e) = app::run(launch_file) {
        log::error("startup", &e);
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(format!("MyMDedit を起動できませんでした。\n{e}")),
                w!("MyMDedit"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    /// The link settings that keep DLLs out of the exe's folder (audit F9, F11), read
    /// back from this test binary — built with the same flags as the app.
    struct Pe {
        dependent_load_flags: u16,
        dll_characteristics: u16,
        /// IMAGE_DLLCHARACTERISTICS_EX_* from the debug directory (0 when absent).
        dll_characteristics_ex: u32,
        imports: Vec<String>,
    }

    fn read_pe(bytes: &[u8]) -> Pe {
        let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let pe = u32_at(0x3C) as usize;
        let sections = u16_at(pe + 6) as usize;
        let opt = pe + 24;
        assert_eq!(u16_at(opt), 0x20B, "expected PE32+");
        let data_dirs = opt + 112;
        let section_table = opt + u16_at(pe + 20) as usize;
        let file_offset = |rva: u32| {
            (0..sections)
                .map(|i| section_table + i * 40)
                .find_map(|s| {
                    let (size, va, raw) = (u32_at(s + 8).max(u32_at(s + 16)), u32_at(s + 12), u32_at(s + 20));
                    (va..va + size).contains(&rva).then(|| (rva - va + raw) as usize)
                })
                .unwrap()
        };
        let c_str = |rva: u32| {
            let start = file_offset(rva);
            let end = start + bytes[start..].iter().position(|&b| b == 0).unwrap();
            String::from_utf8_lossy(&bytes[start..end]).into_owned()
        };
        let mut imports = Vec::new();
        let mut entry = file_offset(u32_at(data_dirs + 8));
        while u32_at(entry + 12) != 0 {
            imports.push(c_str(u32_at(entry + 12)));
            entry += 20;
        }
        // IMAGE_LOAD_CONFIG_DIRECTORY64.DependentLoadFlags sits at offset 0x4E,
        // after ProcessHeapFlags (0x48) and CSDVersion (0x4C).
        let dependent_load_flags = u16_at(file_offset(u32_at(data_dirs + 10 * 8)) + 0x4E);
        let dll_characteristics = u16_at(opt + 70);
        // Debug directory entries are 28 bytes: Type at +12, PointerToRawData at +24.
        // Type 20 = IMAGE_DEBUG_TYPE_EX_DLLCHARACTERISTICS, where /CETCOMPAT lands.
        let (debug_rva, debug_size) = (u32_at(data_dirs + 6 * 8), u32_at(data_dirs + 6 * 8 + 4) as usize);
        let debug = if debug_rva == 0 { 0 } else { file_offset(debug_rva) };
        let dll_characteristics_ex = (0..debug_size / 28)
            .map(|i| debug + i * 28)
            .find(|&e| u32_at(e + 12) == 20)
            .map_or(0, |e| u32_at(u32_at(e + 24) as usize));
        Pe { dependent_load_flags, dll_characteristics, dll_characteristics_ex, imports }
    }

    #[test]
    fn exploit_mitigations_are_on() {
        let pe = read_pe(&std::fs::read(std::env::current_exe().unwrap()).unwrap());
        // DYNAMIC_BASE, HIGH_ENTROPY_VA, NX_COMPAT, and GUARD_CF from .cargo/config.toml.
        for (bit, name) in [(0x0040, "ASLR"), (0x0020, "high-entropy ASLR"), (0x0100, "NX"), (0x4000, "CFG")] {
            assert_ne!(pe.dll_characteristics & bit, 0, "{name} is off");
        }
        // IMAGE_DLLCHARACTERISTICS_EX_CET_COMPAT, from build.rs.
        assert_ne!(pe.dll_characteristics_ex & 0x01, 0, "CET shadow stack compatibility is off");
    }

    #[test]
    fn static_imports_are_looked_up_in_system32_only() {
        let pe = read_pe(&std::fs::read(std::env::current_exe().unwrap()).unwrap());
        // LOAD_LIBRARY_SEARCH_SYSTEM32, from build.rs.
        assert_eq!(pe.dependent_load_flags, 0x800);
        // The C runtime is linked in (.cargo/config.toml), so no VC++ runtime DLL is
        // needed — or looked for next to the exe.
        let crt: Vec<_> = pe
            .imports
            .iter()
            .filter(|n| {
                let n = n.to_ascii_lowercase();
                n.starts_with("vcruntime") || n.starts_with("msvcp") || n.starts_with("api-ms-win-crt")
            })
            .collect();
        assert!(crt.is_empty(), "dynamic C runtime imports: {crt:?}");
    }
}
