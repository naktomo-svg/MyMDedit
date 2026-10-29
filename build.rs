fn main() {
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

/// The exe's VERSIONINFO (Explorer's Details tab), generated so the version is only
/// ever written in Cargo.toml.
fn write_version_rc() -> std::path::PathBuf {
    let env = |k: &str| std::env::var(k).unwrap();
    let version = env("CARGO_PKG_VERSION");
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
            VALUE "CompanyName", "あぽろ"
            VALUE "FileDescription", "MyMDedit"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "MyMDedit"
            VALUE "LegalCopyright", "Copyright (c) 2026 あぽろ (MIT License)"
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
