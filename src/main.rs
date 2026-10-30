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
    // Without it the next load by name (Msftedit.dll, for the editor) would look in the
    // exe's folder first: refuse to run rather than run unprotected. It cannot fail on
    // any Windows this app supports, so this is a guard, not a code path users meet.
    if let Err(e) = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) } {
        log::error("SetDefaultDllDirectories", &e);
        fatal(&format!("DLL の読み込み先を System32 に限定できなかったため、安全のため起動を中止しました。\n{e}"));
        return;
    }
    log::install_panic_hook();
    unsafe {
        // WebView2 and the shell dialogs both require an STA thread; without one they fail
        // later with errors that no longer say why, and nothing could be opened or saved.
        // So say why now, and stop. (S_FALSE, "already initialized", counts as success.)
        if let Err(e) = CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() {
            log::error("CoInitializeEx", &e);
            fatal(&format!("COM を初期化できなかったため、起動を中止しました。\n{e}"));
            return;
        }
        if !InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_BAR_CLASSES | ICC_UPDOWN_CLASS | ICC_TAB_CLASSES,
        })
        .as_bool()
        {
            log::error("InitCommonControlsEx", "failed");
        }
    }

    // Explorer passes the double-clicked file as the first argument.
    // (DLL search policy: see the top of this function, build.rs and .cargo/config.toml.)
    let launch_file = std::env::args_os().nth(1).map(PathBuf::from);
    if let Err(e) = app::run(launch_file) {
        log::error("startup", &e);
        fatal(&format!("MyMDedit を起動できませんでした。\n{e}"));
    }
}

/// The app cannot run at all: say so in the one UI there is before a window exists.
fn fatal(message: &str) {
    unsafe {
        MessageBoxW(None, &HSTRING::from(message), w!("MyMDedit"), MB_OK | MB_ICONERROR);
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
        // DYNAMIC_BASE, HIGH_ENTROPY_VA and NX_COMPAT are the MSVC linker's defaults (kept
        // on, never turned off here); GUARD_CF is `-C control-flow-guard` in .cargo/config.toml.
        for (bit, name) in [(0x0040, "ASLR"), (0x0020, "high-entropy ASLR"), (0x0100, "NX"), (0x4000, "CFG")] {
            assert_ne!(pe.dll_characteristics & bit, 0, "{name} is off");
        }
        // IMAGE_DLLCHARACTERISTICS_EX_CET_COMPAT, from build.rs.
        assert_ne!(pe.dll_characteristics_ex & 0x01, 0, "CET shadow stack compatibility is off");
    }

    /// `fuzz/` is its own workspace with its own Cargo.lock, so nothing but this test
    /// stops it from fuzzing a different comrak (or serde_json) than the app ships.
    #[test]
    fn fuzz_resolves_the_same_versions_as_the_app() {
        let packages = |lock: &str| {
            let mut v: Vec<(String, String)> = lock
                .split("[[package]]")
                .filter_map(|p| {
                    let field =
                        |k: &str| p.lines().find_map(|l| l.strip_prefix(k)).map(|s| s.trim_matches('"').to_owned());
                    Some((field("name = ")?, field("version = ")?))
                })
                .collect();
            v.sort();
            v
        };
        let app = packages(include_str!("../Cargo.lock"));
        let fuzz = packages(include_str!("../fuzz/Cargo.lock"));
        let versions = |set: &[(String, String)], name: &str| {
            set.iter().filter(|(n, _)| n == name).map(|(_, v)| v.clone()).collect::<Vec<_>>()
        };
        for (name, _) in &app {
            let in_fuzz = versions(&fuzz, name);
            if !in_fuzz.is_empty() {
                assert_eq!(versions(&app, name), in_fuzz, "{name}: run `cargo update -p {name}` in fuzz/");
            }
        }
    }

    /// The accepted licenses are written twice, once per tool; a license added to one
    /// only would fail the other tool's run with no hint of where the list lives.
    #[test]
    fn license_lists_agree() {
        assert_eq!(
            string_array(include_str!("../about.toml"), "", "accepted"),
            string_array(include_str!("../deny.toml"), "licenses", "allow"),
            "about.toml's `accepted` and deny.toml's `[licenses] allow` differ"
        );
    }

    /// The string array `key` of TOML table `table` ("" for the top level), sorted. Just
    /// enough TOML for these files: comments stripped, the array may span lines, and a key
    /// of the same name in another table (or commented out) is not it.
    fn string_array(toml: &str, table: &str, key: &str) -> Vec<String> {
        let mut current = String::new();
        let mut array: Option<String> = None;
        for raw in toml.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if let Some(a) = &mut array {
                a.push_str(line);
            } else if line.starts_with('[') {
                current = line.trim_matches(['[', ']']).trim().to_string();
            } else if current == table
                && let Some((k, v)) = line.split_once('=')
                && k.trim() == key
            {
                array = Some(v.trim().to_string());
            }
            if array.as_deref().is_some_and(|a| a.contains(']')) {
                break;
            }
        }
        let array = array.unwrap_or_else(|| panic!("no `{key}` in [{table}]"));
        let inner = array.trim().strip_prefix('[').and_then(|a| a.split(']').next()).expect("not an array");
        let mut v: Vec<String> =
            inner.split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect();
        v.sort();
        v
    }

    /// A lone CR or other control character in a source or document reads as text in
    /// some tools and as a line break in others (a PowerShell-written `` `r `` once turned
    /// a comment's `ready` into CR + "eady"). Tabs and CRLF/LF line ends are fine.
    #[test]
    fn sources_hold_no_stray_control_characters() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = vec![root.join("build.rs"), root.join("CLAUDE.md"), root.join("README.md")];
        for dir in ["src", "docs", "fuzz/src", "fuzz/fuzz_targets"] {
            for entry in std::fs::read_dir(root.join(dir)).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|e| e == "rs" || e == "md") {
                    files.push(path);
                }
            }
        }
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap();
            for (n, line) in text.split('\n').enumerate() {
                let line = line.strip_suffix('\r').unwrap_or(line);
                let bad = line.chars().find(|&c| c.is_control() && c != '\t');
                assert!(bad.is_none(), "{}:{}: control character {:?}", path.display(), n + 1, bad.unwrap());
            }
        }
    }

    #[test]
    fn string_array_reads_only_the_named_table() {
        let toml = "# allow = [\"X\"]\n[bans]\nallow = [\"Y\"]\n[licenses]\nallow = [\n  \"MIT\", # a comment with ] in it\n  \"Apache-2.0\",\n]\n";
        assert_eq!(string_array(toml, "licenses", "allow"), ["Apache-2.0", "MIT"]);
        assert_eq!(string_array(toml, "bans", "allow"), ["Y"]);
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
