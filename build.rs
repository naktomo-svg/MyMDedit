/// Written once: the exe's VERSIONINFO (CompanyName, LegalCopyright) and the app's
/// バージョン情報 both show it. The license comes from Cargo.toml's `license`.
const AUTHOR: &str = "あぽろ";
const COPYRIGHT_YEAR: &str = "2026";

fn copyright() -> String {
    format!("Copyright (c) {COPYRIGHT_YEAR} {AUTHOR}")
}

fn main() {
    check_hardening();
    println!("cargo:rustc-env=MYMDEDIT_COPYRIGHT={}", copyright());
    embed_resource::compile("res/app.rc", embed_resource::NONE).manifest_required().unwrap();
    embed_resource::compile(write_version_rc(), embed_resource::NONE).manifest_optional().unwrap();

    // Security: static imports are resolved by the loader before main() runs, so
    // SetDefaultDllDirectories cannot reach them. 0x800 = LOAD_LIBRARY_SEARCH_SYSTEM32:
    // look for them in System32 only, never in the exe's folder (audit F9). Passed to
    // every target, tests included, so a unit test can check the flag landed.
    println!("cargo:rustc-link-arg=/DEPENDENTLOADFLAG:0x800");
    // Hardware-enforced shadow stack where the CPU supports it (audit F10): a return
    // address overwritten on the stack ends the process instead of redirecting it.
    println!("cargo:rustc-link-arg=/CETCOMPAT");
}

/// Refuses to build without the hardening .cargo/config.toml asks for. Its rustflags
/// are dropped wholesale, without a word, when RUSTFLAGS (or CARGO_ENCODED_RUSTFLAGS)
/// is set — a `RUSTFLAGS=-D warnings` in a workflow would otherwise ship an exe that
/// needs VCRUNTIME140.dll from its own folder and has no CFG. main.rs's tests only see
/// the test binary, so the check lives here, where every build passes through.
fn check_hardening() {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    // The linker arguments below and the rustflags are MSVC's; there is no other target.
    assert_eq!(var("CARGO_CFG_TARGET_ENV"), "msvc", "MyMDedit only builds for an MSVC target");
    let fix = "set the flags in RUSTFLAGS too, or unset RUSTFLAGS (see .cargo/config.toml)";
    assert!(
        var("CARGO_CFG_TARGET_FEATURE").split(',').any(|f| f == "crt-static"),
        "the C runtime is not linked statically: {fix}"
    );
    let rustflags = var("CARGO_ENCODED_RUSTFLAGS");
    let cfg = rustflags.split('\x1f').filter_map(|f| f.split_once("control-flow-guard")).next_back();
    assert!(
        matches!(cfg, Some((_, "" | "=checks" | "=yes" | "=y" | "=on" | "=true"))),
        "Control Flow Guard is off: {fix}"
    );
}

/// The exe's VERSIONINFO (Explorer's Details tab), generated so the version is only
/// ever written in Cargo.toml.
fn write_version_rc() -> std::path::PathBuf {
    let env = |k: &str| std::env::var(k).unwrap();
    let version = env("CARGO_PKG_VERSION");
    let (copyright, license) = (copyright(), env("CARGO_PKG_LICENSE"));
    let numeric = format!(
        "{},{},{},0",
        env("CARGO_PKG_VERSION_MAJOR"),
        env("CARGO_PKG_VERSION_MINOR"),
        env("CARGO_PKG_VERSION_PATCH")
    );
    let rc = format!(
        r#"#pragma code_page(65001)
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "041104B0"
        BEGIN
            VALUE "CompanyName", "{AUTHOR}"
            VALUE "FileDescription", "MyMDedit"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "MyMDedit"
            VALUE "LegalCopyright", "{copyright} ({license} License)"
            VALUE "OriginalFilename", "MyMDedit.exe"
            VALUE "ProductName", "MyMDedit"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x411, 1200
    END
END
"#
    );
    let path = std::path::Path::new(&env("OUT_DIR")).join("version.rc");
    std::fs::write(&path, rc).unwrap();
    path
}
