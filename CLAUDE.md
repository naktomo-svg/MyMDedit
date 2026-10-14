# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

MyMDedit is a Rust port of the WinUI 3 Markdown editor MyWinApp (`../MyWinApp`): a single-window Win32 app with a RichEdit editor on the left and a WebView2 HTML preview on the right. MyWinApp's `docs/spec.md` is the behavioural spec this port follows; `README.md` lists where the two deliberately differ.

## Commands

`cargo` lives in `%USERPROFILE%\.cargo\bin`, which may not be on `PATH` in a fresh shell — prepend it if `cargo` is not found.

```powershell
cargo build                      # debug build → target\debug\MyMDedit.exe
cargo build --release            # release build → target\release\MyMDedit.exe (no console window)
cargo test                       # unit tests
cargo clippy --all-targets       # lint
.\target\debug\MyMDedit.exe [file.md]
```

**This section is the only place this repo's tooling state is asserted.** Skills under `.claude/skills/` refer to "the commands CLAUDE.md lists" rather than naming them, so adding a tool means editing here and nowhere else.

Verification for every change is: `cargo build` with 0 errors, `cargo test` all passing, and `cargo clippy --all-targets` with no new warnings. Tests cover the pure-logic modules only (`md_ops`, `search`, `document`, `line_endings`, `settings`, `render`, `dialogs`' parsers, `settings_dialog::DialogEdits`). Anything behind a Win32 or WebView2 call needs manual QA in the running app (`winapp ui` can drive it; toolbar buttons carry accessible names, the editor's control id is 300).

Generating the AI tech-debt report (`scripts/Analyze-TechDebt.ps1`) has its own flags, caching/resume behaviour and cost caveats — see the `tech-debt-report` skill.

## Architecture

- `app.rs` — the main window and every command. All state is in `App` (`ui: Cell<Ui>` for handles, `state: RefCell<State>`, `preview: RefCell<Preview>`), reached from window procedures through the thread-local `app()`.
- `editor.rs` — RichEdit (`RICHEDIT50W`) in plain-text mode. Selection offsets are UTF-16 units and line breaks are lone `\r` — the same model WinUI's `TextBox` had, which is why the toolbar logic ported unchanged.
- `md_ops.rs` / `search.rs` — toolbar, list continuation and find/replace as pure functions returning a single `Edit` (one replaced range + the selection after), so each action is one undo step.
- `preview.rs` — WebView2 hosting. Security is three layers: the CSP in `render.rs`, `IsScriptEnabled = false`, and the `NavigationStarting` guard (a **count** of the app's own pending navigations, ANDed with a scheme **denylist**). Never turn the guard into an allowlist on `Uri`'s shape: `NavigateToString`'s URI is `data:text/html;…`, and an allowlist that guessed wrong once blanked the preview in MyWinApp. Nothing is loaded until `ready` is set after all protections are installed. Never add `IsUserInitiated` to the guard: WebView2 documents API-started navigations (NavigateToString included) as user initiated. `ProcessFailed` resets the counter (a dead process never reports its navigations).
- `document.rs` / `line_endings.rs` — reads return the `DocumentFormat` (charset, BOM, line ending) they detected and saves require it back; saves are atomic (sibling temp file + `ReplaceFileW`, falling back to `MoveFileExW`). Non-UTF-8 input is refused, never decoded lossily.
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
- `ReplaceFileW` can fail after moving the original aside. Always give it a backup name so the original's location is known, and never delete the temp file while the target is missing (`document::swap_into_place`).

## Development workflow

- Before acting on a technical-debt, clippy or code-review finding (e.g. from `docs/tech-debt/`), invoke the `verify-then-fix` skill.
- Before fixing a confirmed-real bug or tech-debt finding, invoke the `why5-and-horizontal-check` skill.
