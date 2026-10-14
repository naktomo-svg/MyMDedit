//! The HTML preview: a WebView2 hosted in the main window.
//!
//! Security model (a third party's `.md` opened by double-click is a real path, and the
//! renderer passes raw HTML through):
//! 1. the CSP in `render.rs`,
//! 2. `IsScriptEnabled = false`,
//! 3. the navigation guard here, which lets through only navigations this app started.
//!
//! The preview is never loaded before all three are in place (`ready`).

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
use windows::core::{BOOL, HSTRING, Interface, PWSTR};

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_COLOR, COREWEBVIEW2_KEY_EVENT_KIND, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
    COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN, COREWEBVIEW2_WEB_ERROR_STATUS, CreateCoreWebView2EnvironmentWithOptions,
    ICoreWebView2, ICoreWebView2Controller, ICoreWebView2Controller2, ICoreWebView2EnvironmentOptions,
};
use webview2_com::{
    AcceleratorKeyPressedEventHandler, CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, ExecuteScriptCompletedHandler, NavigationCompletedEventHandler,
    NavigationStartingEventHandler, take_pwstr,
};

use crate::app::{App, app};
use crate::log;

#[derive(Default)]
pub struct Preview {
    controller: Option<ICoreWebView2Controller>,
    webview: Option<ICoreWebView2>,
    /// Set last in initialization, only once every protection is in place.
    ready: bool,
    /// Navigations this app started that have not reached NavigationStarting yet — the
    /// only signal the guard trusts. A count, not a bool: a render per keystroke can put
    /// two of our own navigations in flight, and a bool would cancel the second.
    pending_self: i32,
    /// Id of the newest navigation we started and the guard let through.
    latest_id: u64,
    update_failure_reported: bool,
    navigation_failure_reported: bool,
    script_busy: bool,
    script_again: bool,
    bounds: RECT,
    background: COREWEBVIEW2_COLOR,
}

/// Schemes NavigateToString can never produce, so a navigation carrying one can only
/// have come from the document itself. Deliberately a denylist ANDed onto the counter,
/// never an allowlist on the URI's shape: NavigateToString's URI is
/// `data:text/html;...`, not `about:blank`, and an allowlist that guessed wrong once
/// cancelled every render and left the preview blank.
const BLOCKED_SCHEMES: [&str; 6] = ["http:", "https:", "file:", "ftp:", "ws:", "wss:"];

fn uses_blocked_scheme(uri: &str) -> bool {
    BLOCKED_SCHEMES.iter().any(|s| uri.len() >= s.len() && uri[..s.len()].eq_ignore_ascii_case(s))
}

fn describe_uri(uri: &str) -> String {
    const LIMIT: usize = 200;
    match uri.char_indices().nth(LIMIT) {
        _ if uri.is_empty() => "(uri なし)".into(),
        Some((i, _)) => format!("{}…(以下省略)", &uri[..i]),
        None => uri.to_string(),
    }
}

const INIT_FAILED: &str =
    "プレビューを初期化できませんでした。WebView2 ランタイムがインストールされているか確認してください。編集は引き続き行えます。";

pub fn start(this: &App, parent: HWND) {
    let data_dir = std::env::var_os("LOCALAPPDATA")
        .map(|d| std::path::PathBuf::from(d).join("MyMDedit").join("WebView2"))
        .unwrap_or_else(|| std::env::temp_dir().join("MyMDedit-WebView2"));

    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |result, env| {
        let env = match (result, env) {
            (Ok(()), Some(env)) => env,
            (r, _) => {
                app().report_error("CreateCoreWebView2Environment", INIT_FAILED, Some(&format!("{r:?}")));
                return Ok(());
            }
        };
        let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |result, controller| {
            match (result, controller) {
                (Ok(()), Some(controller)) => {
                    if let Err(e) = configure(controller) {
                        app().report_error("ConfigureWebView2", INIT_FAILED, Some(&e));
                    }
                }
                (r, _) => app().report_error("CreateCoreWebView2Controller", INIT_FAILED, Some(&format!("{r:?}"))),
            }
            Ok(())
        }));
        if let Err(e) = unsafe { env.CreateCoreWebView2Controller(parent, &handler) } {
            app().report_error("CreateCoreWebView2Controller", INIT_FAILED, Some(&e));
        }
        Ok(())
    }));

    let options: ICoreWebView2EnvironmentOptions = CoreWebView2EnvironmentOptions::default().into();
    let created = unsafe {
        CreateCoreWebView2EnvironmentWithOptions(None, &HSTRING::from(data_dir.as_os_str()), &options, &handler)
    };
    if let Err(e) = created {
        this.report_error("CreateCoreWebView2EnvironmentWithOptions", INIT_FAILED, Some(&e));
    }
}

#[allow(unused_unsafe)]
fn configure(controller: ICoreWebView2Controller) -> windows::core::Result<()> {
    let webview = unsafe { controller.CoreWebView2()? };
    unsafe {
        let settings = webview.Settings()?;
        // Ctrl+wheel over the preview does nothing; zoom is driven from the editor and
        // the window-wide shortcuts only.
        settings.SetIsZoomControlEnabled(false)?;
        // Scripts in the document never run. ExecuteScript (zoom/scroll below) is
        // documented to be unaffected by this.
        settings.SetIsScriptEnabled(false)?;
        settings.SetIsStatusBarEnabled(false)?;

        let mut token = 0i64;
        webview.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut uri = PWSTR::null();
                unsafe { args.Uri(&mut uri)? };
                let uri = take_pwstr(uri);
                let app = app();
                let mut p = app.preview.borrow_mut();
                let cancel = p.pending_self == 0 || uses_blocked_scheme(&uri);
                if cancel {
                    unsafe { args.SetCancel(true)? };
                    log::error("PreviewNavigationBlocked", format!("blocked navigation to {}", describe_uri(&uri)));
                    return Ok(());
                }
                p.pending_self -= 1;
                let mut id = 0u64;
                unsafe { args.NavigationId(&mut id)? };
                p.latest_id = id;
                Ok(())
            })),
            &mut token,
        )?;

        webview.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut ok = BOOL::default();
                let mut id = 0u64;
                let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                unsafe {
                    args.IsSuccess(&mut ok)?;
                    args.NavigationId(&mut id)?;
                    args.WebErrorStatus(&mut status)?;
                }
                let app = app();
                if !ok.as_bool() {
                    // A render superseded by a newer one, and a hostile navigation the
                    // guard cancelled, both land here as routine failures — only the
                    // newest render we started is worth telling the user about.
                    let report = {
                        let mut p = app.preview.borrow_mut();
                        if p.pending_self > 0 || id != p.latest_id {
                            log::error("PreviewNavigationCompleted", format!("superseded or blocked navigation {id}: {}", status.0));
                            false
                        } else if p.navigation_failure_reported {
                            log::error("PreviewNavigationCompleted", format!("navigation failed: {}", status.0));
                            false
                        } else {
                            p.navigation_failure_reported = true;
                            true
                        }
                    };
                    if report {
                        app.report_error(
                            "PreviewNavigationCompleted",
                            &format!("プレビューを表示できませんでした。({}) 編集は引き続き行えます。", status.0),
                            None,
                        );
                    }
                    return Ok(());
                }
                app.preview.borrow_mut().navigation_failure_reported = false;
                sync_view(&app);
                Ok(())
            })),
            &mut token,
        )?;

        // Keyboard focus inside the WebView never reaches the main window's accelerator
        // table, so forward the app's shortcuts from here.
        controller.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                let mut vk = 0u32;
                unsafe {
                    args.KeyEventKind(&mut kind)?;
                    args.VirtualKey(&mut vk)?;
                }
                if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                    return Ok(());
                }
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                if let Some(cmd) = crate::app::accelerator_command(vk as u16, ctrl, shift) {
                    unsafe { args.SetHandled(true)? };
                    app().post_command(cmd);
                }
                Ok(())
            })),
            &mut token,
        )?;
    }

    let app = app();
    {
        let mut p = app.preview.borrow_mut();
        unsafe {
            let _ = controller.SetBounds(p.bounds);
            if let Ok(c2) = controller.cast::<ICoreWebView2Controller2>() {
                let _ = c2.SetDefaultBackgroundColor(p.background);
            }
            controller.SetIsVisible(true)?;
        }
        p.controller = Some(controller);
        p.webview = Some(webview);
        p.ready = true;
    }
    app.refresh_preview();
    Ok(())
}

/// Pushes `html` into the preview. The single place that calls NavigateToString.
pub fn navigate(app: &App, html: &str) {
    let webview = {
        let p = app.preview.borrow();
        // Security: gate on `ready`, not merely on the WebView existing — a half-
        // configured WebView must never load a hostile document.
        if !p.ready {
            return;
        }
        p.webview.clone()
    };
    let Some(webview) = webview else { return };

    // Counted immediately before the call; the guard consumes it.
    app.preview.borrow_mut().pending_self += 1;
    let result = unsafe { webview.NavigateToString(&HSTRING::from(html)) };
    let mut p = app.preview.borrow_mut();
    match result {
        Ok(()) => p.update_failure_reported = false,
        Err(e) => {
            // No navigation will start, so nothing would consume the count — leaving it
            // raised would hand the permission to whatever navigates next.
            p.pending_self -= 1;
            // NavigateToString rejects strings over ~2MB. Report once per failing
            // streak: this runs per keystroke.
            if p.update_failure_reported {
                log::error("NavigateToString", &e);
                return;
            }
            p.update_failure_reported = true;
            drop(p);
            app.report_error(
                "NavigateToString",
                "プレビューを更新できませんでした。文書が大きすぎる可能性があります（プレビューの上限は約2MBです）。編集内容は保持されています。",
                Some(&e),
            );
        }
    }
}

/// Applies the zoom and then the editor's scroll ratio to the preview, in one script
/// so the order (zoom first — it changes the document height) can never invert.
///
/// While a script is in flight, further requests collapse into one re-run after it
/// completes, which paces the updates to whatever the WebView can absorb.
pub fn sync_view(this: &App) {
    let webview = {
        let mut p = this.preview.borrow_mut();
        if !p.ready {
            return;
        }
        if p.script_busy {
            p.script_again = true;
            return;
        }
        p.script_busy = true;
        p.webview.clone()
    };
    let Some(webview) = webview else { return };
    let (zoom, ratio) = this.view_state();
    let script = format!(
        "document.body.style.zoom='{zoom}%';(function(){{var e=document.scrollingElement||document.documentElement;\
         var m=e.scrollHeight-e.clientHeight;e.scrollTop=m>0?m*{ratio}:0;}})()"
    );
    let handler = ExecuteScriptCompletedHandler::create(Box::new(|result, _| {
        if let Err(e) = result {
            log::error("ExecuteScript", e);
        }
        let app = app();
        let again = {
            let mut p = app.preview.borrow_mut();
            p.script_busy = false;
            std::mem::take(&mut p.script_again)
        };
        if again {
            sync_view(&app);
        }
        Ok(())
    }));
    // Recorded, not shown: a preview that fails to zoom/scroll degrades visibly on its
    // own, and a banner per scroll tick would be noise.
    if let Err(e) = unsafe { webview.ExecuteScript(&HSTRING::from(script), &handler) } {
        log::error("ExecuteScript", e);
        this.preview.borrow_mut().script_busy = false;
    }
}

pub fn set_bounds(app: &App, bounds: RECT) {
    let controller = {
        let mut p = app.preview.borrow_mut();
        p.bounds = bounds;
        p.controller.clone()
    };
    if let Some(c) = controller {
        unsafe {
            let _ = c.SetBounds(bounds);
        }
    }
}

pub fn set_background(app: &App, color: COLORREF) {
    let c = COREWEBVIEW2_COLOR {
        A: 255,
        R: (color.0 & 0xFF) as u8,
        G: ((color.0 >> 8) & 0xFF) as u8,
        B: ((color.0 >> 16) & 0xFF) as u8,
    };
    let controller = {
        let mut p = app.preview.borrow_mut();
        p.background = c;
        p.controller.clone()
    };
    if let Some(controller) = controller.and_then(|c| c.cast::<ICoreWebView2Controller2>().ok()) {
        unsafe {
            let _ = controller.SetDefaultBackgroundColor(c);
        }
    }
}

pub fn notify_moved(app: &App) {
    if let Some(c) = app.preview.borrow().controller.clone() {
        unsafe {
            let _ = c.NotifyParentWindowPositionChanged();
        }
    }
}

pub fn close(app: &App) {
    let controller = {
        let mut p = app.preview.borrow_mut();
        p.ready = false;
        p.webview = None;
        p.controller.take()
    };
    if let Some(c) = controller {
        unsafe {
            let _ = c.Close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_remote_and_file_schemes_only() {
        assert!(uses_blocked_scheme("HTTPS://evil/"));
        assert!(uses_blocked_scheme("file:///C:/x"));
        assert!(!uses_blocked_scheme("data:text/html;charset=utf-8;base64,AAA"));
        assert!(!uses_blocked_scheme("about:blank"));
    }

    #[test]
    fn truncates_long_uris() {
        assert_eq!(describe_uri(""), "(uri なし)");
        assert!(describe_uri(&"a".repeat(500)).ends_with("…(以下省略)"));
    }
}
