# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

MyMDedit is a Rust port of the WinUI 3 Markdown editor MyWinApp (not public, no longer maintained): a single-window Win32 app with a RichEdit editor on the left and a WebView2 HTML preview on the right. `docs/spec.md` is this app's behavioural spec (rewritten from MyWinApp's for the current implementation) — update it with any user-visible change; `README.md` lists where MyMDedit deliberately differs from MyWinApp.

## Commands

`cargo` lives in `%USERPROFILE%\.cargo\bin`, which may not be on `PATH` in a fresh shell — prepend it if `cargo` is not found.

```powershell
cargo build                      # debug build → target\debug\MyMDedit.exe
cargo build --release            # release build → target\release\MyMDedit.exe (no console window)
cargo test                       # unit tests
cargo clippy --all-targets       # lint
cargo fmt                        # format (rustfmt.toml); run it in fuzz/ too — separate crate
cargo deny check                 # dependency advisories, licenses, sources (deny.toml)
cargo about generate about.hbs -o THIRD-PARTY-NOTICES.txt   # after any dependency change (cargo install cargo-about --features cli)
.\target\debug\MyMDedit.exe [file.md]
.\tests\security\tools\run-all.ps1   # preview security regression: every payload, PASS/FAIL (debug build, ~5 min)
cargo +nightly fuzz run <target> -- -max_total_time=<s>   # fuzz/ ; targets: render_markdown render_settings document_decode md_ops search
```

Formatting is `rustfmt.toml` at the root: `max_width = 120` and `use_small_heuristics = "Max"` (calls, struct literals and the like stay on one line up to the full width, as the code was written before rustfmt was adopted). `fuzz/` has no config of its own and picks up the root one.

Fuzzing needs the nightly toolchain, `cargo install cargo-fuzz`, and the MSVC ASan runtime directory (`…\VC\Tools\MSVC\<ver>\bin\Hostx64\x64`, holding `clang_rt.asan_dynamic-x86_64.dll`) on `PATH`; `-s none` does not link on MSVC. `cd fuzz; cargo +nightly test --lib` checks that the fuzz checkers catch what they guard against.

**This section is the only place this repo's tooling state is asserted.** Skills under `.claude/skills/` refer to "the commands CLAUDE.md lists" rather than naming them, so adding a tool means editing here and nowhere else.

Verification for every change is: `cargo build` with 0 errors, `cargo test` all passing, `cargo clippy --all-targets` with no new warnings, and `cargo fmt --check` with no diff (in `fuzz/` too when it changed). Tests cover the pure-logic modules only (`md_ops`, `search`, `document`, `line_endings`, `settings`, `render`, `dialogs`' parsers, `settings_dialog::DialogEdits`). Anything behind a Win32 or WebView2 call needs manual QA in the running app (`winapp ui` can drive it; toolbar buttons carry accessible names, the editor's control id is 300).

Generating the AI tech-debt report (`scripts/Analyze-TechDebt.ps1`) has its own flags, caching/resume behaviour and cost caveats — see the `tech-debt-report` skill.

## Architecture

- `app.rs` — the main window and every command. All state is in `App` (`ui: Cell<Ui>` for handles, `state: RefCell<State>`, `preview: RefCell<Preview>`), reached from window procedures through the thread-local `app()`.
- `editor.rs` — RichEdit (`RICHEDIT50W`) in plain-text mode. Selection offsets are UTF-16 units and line breaks are lone `\r` — the same model WinUI's `TextBox` had, which is why the toolbar logic ported unchanged.
- `md_ops.rs` / `search.rs` — toolbar, list continuation and find/replace as pure functions returning a single `Edit` (one replaced range + the selection after), so each action is one undo step.
- `preview.rs` — WebView2 hosting. Security is three layers: the CSP in `render.rs`, `IsScriptEnabled = false`, and the handlers in `preview.rs`. Of those handlers, only the `WebResourceRequested` filter (`request_allowed`: the guard's denylist, except https images) stops traffic; the `NavigationStarting` guard only stops the page change — by the time it cancels, the request has been sent. Downloads (`DownloadStarting`), frame navigations (`FrameNavigationStarting`) and client-certificate requests (`ClientCertificateRequested`) never reach the guard and are cancelled outright. The guard itself is a **count** of the app's own pending navigations, ANDed with a scheme **denylist**. Never turn the guard into an allowlist on `Uri`'s shape: `NavigateToString`'s URI is `data:text/html;…`, and an allowlist that guessed wrong once blanked the preview in MyWinApp. Nothing is loaded until `ready` is set after all protections are installed. Never add `IsUserInitiated` to the guard: WebView2 documents API-started navigations (NavigateToString included) as user initiated. `ProcessFailed` resets the counter (a dead process never reports its navigations).
- `document.rs` / `line_endings.rs` — reads return the `DocumentFormat` (charset, BOM, line ending) they detected and saves require it back; saves are atomic (sibling temp file + `ReplaceFileW`, falling back to `MoveFileExW`). Non-UTF-8 input is refused, never decoded lossily. Every read (documents and settings.json) goes through `read_at_most`, which refuses anything that is not a disk file: `CON` or a named pipe would block the UI thread until its writer goes away.
- `settings.rs` — `SettingsStore` is the only owner of the current `AppSettings` (`%APPDATA%\MyMDedit\settings.json`). Writes go through `SettingsStore::update(|s| …)`, which re-reads the file under a named mutex and applies only the change, so concurrent instances don't overwrite each other; `App::persist_settings` is the single place a failed write becomes the error banner. Bounds live only in `settings::limits`.
- `log.rs` — every swallowed error is recorded with `log::error` (OutputDebugString). Errors the user must act on also go through `App::report_error` (the banner under the toolbar).

### Rules that keep the code sound

- **Never hold a `RefCell` borrow across a call that can re-enter a window procedure**: any modal dialog (file pickers, TaskDialog, MessageBox, the settings dialog), any `SendMessage` to the editor (it sends `EN_CHANGE` back synchronously), and any WebView2 call. Copy what you need out, drop the borrow, then call. A violation is a `BorrowMutError` panic at runtime, not a compile error.
- Buffers handed to Win32/COM must outlive the call that reads them, and longer when the API keeps the pointer: the modeless find dialog keeps pointers into `FindData`, which is therefore leaked once for the app's lifetime.

### Win32 / WebView2 gotchas hit in this codebase

- RichEdit inserts the line break for Enter on **`WM_KEYDOWN`**, not `WM_CHAR`. List continuation intercepts `VK_RETURN` there and then swallows the `'\r'` `WM_CHAR` that `TranslateMessage` already queued (`swallow_enter_char`). An IME's confirming Enter arrives as `VK_PROCESSKEY`, so it is untouched.
- RichEdit's default text limit is 32K characters (`EM_EXLIMITTEXT` raises it), and `EM_SETTEXTMODE` only works while the control is empty.
- `EM_SETTEXTEX` and `EM_SETCHARFORMAT` can raise `EN_CHANGE`; code that changes text or formatting on the app's own behalf sets `suppress_dirty` so it doesn't mark the document modified.
- Keyboard focus inside the WebView never reaches the main accelerator table; `AcceleratorKeyPressed` forwards the app's shortcuts (`app::ACCELERATORS` is the single list both paths use).
- The menu bar has no dark theme; it is painted through the undocumented `WM_UAHDRAWMENU`/`WM_UAHDRAWMENUITEM` messages (`theme.rs`), and popup menus follow uxtheme ordinal 135 (`SetPreferredAppMode`).
- Owner-drawn buttons report a fast second click as `BN_DOUBLECLICKED` (code 5), which `WM_COMMAND` handling treats as a click.
- Sign-out, shutdown and Restart Manager end the app through `WM_QUERYENDSESSION`/`WM_ENDSESSION`, never `WM_CLOSE`; `DefWindowProc` answers "yes" and unsaved work is gone. Any "before the app goes away" logic must be wired to both paths (`on_close_request` and `on_query_end_session`).
- WebView2 ignores an event handler's `Err` and carries on with the default action. A security handler that bails out with `?` before deciding therefore **fails open** — compute the decision in a function returning `Result` and treat `Err` as "deny" (`preview::allow_navigation`).
- `NavigationStarting` does not see popups: Ctrl/Shift/middle-click, `target="_blank"` and "open in new window" go through `NewWindowRequested`, and the popup inherits neither `IsScriptEnabled = false` nor the CSP. Any new way content can leave the preview needs its own handler installed before `ready`.
- Cancelling in `NavigationStarting` does not stop the network request (a `<meta refresh>` to http:// reached a local listener while the guard logged it blocked). Anything that must not go out is stopped in `WebResourceRequested`. Not even that stops `dns-prefetch`/`preconnect` or a frame's speculative connect: those resolve and connect without a request, so `render::FETCHING_TAGS` keeps such elements out of the content itself. A clicked link still opens a TCP/TLS connection before its (blocked) request — accepted, as it needs the user's click. Re-check with `tests/security/` (payloads + listener/netlog tools) after touching any preview handler.
- DLLs are only ever loaded from System32. A planted `dwmapi.dll`/`Msftedit.dll` next to the exe (e.g. in Downloads) used to load; three settings now prevent it: `SetDefaultDllDirectories` first thing in `main` (loads by name, including those system DLLs make for us), `/DEPENDENTLOADFLAG:0x800` in `build.rs` (static imports), and the static CRT in `.cargo/config.toml` (no VCRUNTIME140.dll at all). `main.rs`'s test checks the last two in the built binary; keep `SetDefaultDllDirectories` ahead of anything that can load a DLL.
- The preview is a `data:` URI holding the whole rendered document, and a persistent WebView2 profile writes page URLs to disk (a cancelled download still left a History row with the document in `tab_url`). The controller is therefore InPrivate (`preview::create_controller`); never create one without it. `run-all.ps1` fails if `data:text/html` shows up anywhere in the profile.
- `ReplaceFileW` can fail after moving the original aside. Always give it a backup name so the original's location is known, and never delete the temp file while the target is missing (`document::swap_into_place`).

## Development workflow

- Before acting on a technical-debt, clippy or code-review finding (e.g. from `docs/tech-debt/`), invoke the `verify-then-fix` skill.
- Before fixing a confirmed-real bug or tech-debt finding, invoke the `why5-and-horizontal-check` skill.
- Security re-audit triggers (record results in the commit message or PR description): bumping `webview2-com` or `comrak`; any change to a handler or setting in `preview.rs`; any new way content or a path enters the app (a new file source, drag-and-drop, clipboard format, IPC); a major WebView2 Runtime update. At minimum, run `run-all.ps1`, `cargo deny check` and the fuzz targets that cover the changed module.
