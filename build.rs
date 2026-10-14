fn main() {
    embed_resource::compile("res/app.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();

    // Security: static imports are resolved by the loader before main() runs, so
    // SetDefaultDllDirectories cannot reach them. 0x800 = LOAD_LIBRARY_SEARCH_SYSTEM32:
    // look for them in System32 only, never in the exe's folder (audit F9). Passed to
    // every target, tests included, so a unit test can check the flag landed.
    println!("cargo:rustc-link-arg=/DEPENDENTLOADFLAG:0x800");
    // Hardware-enforced shadow stack where the CPU supports it (audit F10): a return
    // address overwritten on the stack ends the process instead of redirecting it.
    println!("cargo:rustc-link-arg=/CETCOMPAT");
}
