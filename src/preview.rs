//! The HTML preview: a WebView2 hosted in the main window.
//!
//! Security model (a third party's `.md` opened by double-click is a real path, and the
//! renderer passes raw HTML through):
//! 1. the CSP in `render.rs`,
//! 2. `IsScriptEnabled = false`,
//! 3. the handlers here: the request filter (`WebResourceRequested`), which is what
//!    actually keeps requests from leaving, and the navigation guard, which keeps the
//!    page from being replaced by anything but this app's own renders.
//!
//! Cancelling in NavigationStarting does NOT stop the request: the runtime has already
//! sent it by then (observed: a `<meta refresh>` to http:// reached the server while the
//! guard logged it as blocked). Only the request filter stops traffic; the guard stops
//! the page change. Downloads and frame navigations never reach NavigationStarting at
//! all and have their own handlers.
//!
//! The preview is never loaded before all of these are in place (`ready`).
//!
//! Code blocks' copy buttons are links on `render`'s own scheme; NavigationStarting
//! copies on the app's side and always cancels them (`allow_navigation`).

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{BOOL, HSTRING, Interface, PWSTR, w};

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_COLOR, COREWEBVIEW2_KEY_EVENT_KIND, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
    COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN, COREWEBVIEW2_PROCESS_FAILED_KIND,
    COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED, COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
    COREWEBVIEW2_WEB_ERROR_STATUS, COREWEBVIEW2_WEB_RESOURCE_CONTEXT, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE, COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
    CreateCoreWebView2EnvironmentWithOptions, GetAvailableCoreWebView2BrowserVersionString, ICoreWebView2,
    ICoreWebView2_4, ICoreWebView2_5, ICoreWebView2_18, ICoreWebView2_22, ICoreWebView2AcceleratorKeyPressedEventArgs,
    ICoreWebView2Controller, ICoreWebView2Controller2, ICoreWebView2CreateCoreWebView2ControllerCompletedHandler,
    ICoreWebView2Environment, ICoreWebView2Environment10, ICoreWebView2EnvironmentOptions,
    ICoreWebView2NavigationCompletedEventArgs, ICoreWebView2NavigationStartingEventArgs,
    ICoreWebView2ProcessFailedEventArgs, ICoreWebView2Settings4, ICoreWebView2WebResourceRequestedEventArgs,
};
use webview2_com::{
    AcceleratorKeyPressedEventHandler, ClientCertificateRequestedEventHandler, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    DownloadStartingEventHandler, ExecuteScriptCompletedHandler, LaunchingExternalUriSchemeEventHandler,
    NavigationCompletedEventHandler, NavigationStartingEventHandler, NewWindowRequestedEventHandler,
    ProcessFailedEventHandler, WebResourceRequestedEventHandler, take_pwstr,
};

use crate::app::{App, app};
use crate::log;
use crate::render;

#[derive(Default)]
pub struct Preview {
    controller: Option<ICoreWebView2Controller>,
    webview: Option<ICoreWebView2>,
    /// Set last in initialization, only once every protection is in place.
    ready: bool,
    /// A protection failed to act (`stop_preview`): no document is loaded again for the
    /// rest of the session. Unlike `ready`, nothing ever clears it — not a crash restart,
    /// which would otherwise configure a fresh controller and set `ready` once more.
    stopped: bool,
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
    /// Browser-process restarts so far this session; capped so a crash loop ends.
    restarts: u32,
    /// Renderer crashes since the last successful render; capped so a document that
    /// kills the renderer is not re-rendered forever.
    renderer_crashes: u32,
}

/// Posted to the main window when a WebView2 process dies (wparam: 0 renderer, 1 browser).
pub const WM_APP_PREVIEW_FAILED: u32 = WM_APP + 3;
/// Posted to the main window when a stopped preview could not be hidden and must be
/// closed instead — never from inside the WebView2 handler that found out.
pub const WM_APP_PREVIEW_CLOSE: u32 = WM_APP + 5;
const MAX_CRASH_RETRIES: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrashedProcess {
    Renderer,
    Browser,
    Other,
}

#[derive(Debug, PartialEq, Eq)]
enum CrashResponse {
    Rerender,
    Restart,
    GiveUp,
    Ignore,
}

/// What to do about a dead WebView2 process, given how many times that has already
/// happened. Bounded both ways: a document that reliably kills the renderer, or a
/// runtime that keeps dying, must not turn into an endless respawn loop.
fn crash_response(process: CrashedProcess, renderer_crashes: u32, browser_restarts: u32) -> CrashResponse {
    match process {
        CrashedProcess::Renderer if renderer_crashes < MAX_CRASH_RETRIES => CrashResponse::Rerender,
        CrashedProcess::Browser if browser_restarts < MAX_CRASH_RETRIES => CrashResponse::Restart,
        CrashedProcess::Renderer | CrashedProcess::Browser => CrashResponse::GiveUp,
        CrashedProcess::Other => CrashResponse::Ignore,
    }
}

/// Recovers from a dead process, on the main window's own message (never inside the
/// WebView2 callback that reported it).
pub fn on_process_failed(app: &App, browser: bool) {
    let process = if browser { CrashedProcess::Browser } else { CrashedProcess::Renderer };
    let response = {
        let mut p = app.preview.borrow_mut();
        if p.stopped {
            // Recovery would bring the preview back; it stays stopped.
            log::error("PreviewProcessFailed", "preview is stopped; not recovering");
            return;
        }
        // Whatever was in flight died with the process.
        p.pending_self = 0;
        p.script_busy = false;
        p.script_again = false;
        let response = crash_response(process, p.renderer_crashes, p.restarts);
        match process {
            CrashedProcess::Renderer => p.renderer_crashes += 1,
            _ => p.restarts += 1,
        }
        response
    };
    match response {
        CrashResponse::Rerender => {
            log::error("PreviewProcessFailed", "render process exited; re-rendering");
            app.refresh_preview();
        }
        CrashResponse::Restart => {
            log::error("PreviewProcessFailed", "browser process exited; restarting");
            close(app);
            start(app, app.main_hwnd());
        }
        CrashResponse::GiveUp if browser => {
            close(app);
            app.report_error(
                "PreviewProcessFailed",
                "プレビューの処理が繰り返し停止したため、再起動をやめました。編集と保存は引き続き行えます。",
                None,
            );
        }
        // Not re-rendered automatically; the next edit tries again, and a successful
        // render resets the count.
        CrashResponse::GiveUp => app.report_error(
            "PreviewProcessFailed",
            "この文書を表示するとプレビューの処理が停止するため、自動での再表示をやめました。編集と保存は引き続き行えます。",
            None,
        ),
        CrashResponse::Ignore => {}
    }
}

/// Schemes NavigateToString can never produce, so a navigation carrying one can only
/// have come from the document itself. Deliberately a denylist ANDed onto the counter,
/// never an allowlist on the URI's shape: NavigateToString's URI is
/// `data:text/html;...`, not `about:blank`, and an allowlist that guessed wrong once
/// cancelled every render and left the preview blank.
const BLOCKED_SCHEMES: [&str; 6] = ["http:", "https:", "file:", "ftp:", "ws:", "wss:"];

fn uses_blocked_scheme(uri: &str) -> bool {
    BLOCKED_SCHEMES.iter().any(|s| render::has_scheme(uri, s))
}

/// The guard, as a pure decision. Both conditions only ever deny; neither can admit a
/// navigation the counter would not.
///
/// Deliberately NOT a condition: IsUserInitiated. WebView2 documents navigations
/// started through its APIs — NavigateToString included — as user initiated, so gating
/// on it would cancel every render as soon as a runtime behaves as documented: the
/// blank-preview failure MyWinApp already had once.
fn navigation_allowed(pending_self: i32, uri: &str) -> bool {
    pending_self > 0 && !uses_blocked_scheme(uri)
}

/// Whether a request the preview makes may go out. The same denylist as the guard
/// (our own document is a `data:` URI and never matches), with one exception: https
/// images, the risk accepted in `render.rs`'s CSP comment. Everything else on a
/// network scheme — navigations, prefetches, frames, CSP-exempt fetches — is answered
/// locally and never sent.
fn request_allowed(context: COREWEBVIEW2_WEB_RESOURCE_CONTEXT, uri: &str) -> bool {
    !uses_blocked_scheme(uri)
        || (context == COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE && render::has_scheme(uri, "https:"))
}

/// The request filter's decision for one request; every `Err` becomes a denial.
fn allow_request(args: &ICoreWebView2WebResourceRequestedEventArgs) -> windows::core::Result<bool> {
    let mut uri = PWSTR::null();
    let mut context = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
    unsafe {
        args.Request()?.Uri(&mut uri)?;
        args.ResourceContext(&mut context)?;
    }
    let uri = take_pwstr(uri);
    let allowed = request_allowed(context, &uri);
    if !allowed {
        log::error("PreviewRequestBlocked", format!("blocked request to {}", describe_uri(&uri)));
    }
    Ok(allowed)
}

const STOPPED: &str = "プレビューの安全確認に失敗したため、プレビューを停止しました。編集と保存は引き続き行えます。";

/// The guard could not enforce a denial. Fail closed: no further document is ever
/// loaded, and the preview is hidden so whatever got through cannot be seen or used.
/// Should even hiding fail, the controller is closed, taking the content with it —
/// from the main window's message, as this runs inside a WebView2 handler.
fn stop_preview(context: &str, e: &windows::core::Error) {
    let app = app();
    let controller = {
        let mut p = app.preview.borrow_mut();
        p.ready = false;
        p.stopped = true;
        p.controller.clone()
    };
    if let Some(c) = controller
        && let Err(hide) = unsafe { c.SetIsVisible(false) }
    {
        log::error("stop_preview SetIsVisible", &hide);
        let posted = unsafe { PostMessageW(Some(app.main_hwnd()), WM_APP_PREVIEW_CLOSE, WPARAM(0), LPARAM(0)) };
        log::if_err("stop_preview PostMessage", posted);
    }
    app.report_error(context, STOPPED, Some(e));
}

/// The guard's decision for one NavigationStarting. Every way of failing to decide is
/// an `Err`, which the caller turns into a cancel — the guard must fail closed.
fn allow_navigation(args: &ICoreWebView2NavigationStartingEventArgs) -> windows::core::Result<bool> {
    let mut uri = PWSTR::null();
    let mut id = 0u64;
    unsafe {
        args.Uri(&mut uri)?;
        args.NavigationId(&mut id)?;
    }
    let uri = take_pwstr(uri);
    let app = app();
    // A code block's copy button. Never a navigation, and decided before the counter so
    // a click cannot consume the permission of a render in flight. No borrow is held:
    // copying can put up the error banner.
    if render::is_copy_link(&uri) {
        app.copy_code_block(&uri);
        return Ok(false);
    }
    let mut p = app.preview.borrow_mut();
    if !navigation_allowed(p.pending_self, &uri) {
        log::error("PreviewNavigationBlocked", format!("blocked navigation to {}", describe_uri(&uri)));
        return Ok(false);
    }
    p.pending_self -= 1;
    p.latest_id = id;
    Ok(true)
}

fn describe_uri(uri: &str) -> String {
    const LIMIT: usize = 200;
    match uri.char_indices().nth(LIMIT) {
        _ if uri.is_empty() => "(uri なし)".into(),
        Some((i, _)) => format!("{}…(以下省略)", &uri[..i]),
        None => uri.to_string(),
    }
}

const INIT_FAILED: &str = "プレビューを初期化できませんでした。WebView2 ランタイムがインストールされているか確認してください。編集は引き続き行えます。";

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
        let env_for_configure = env.clone();
        let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |result, controller| {
            match (result, controller) {
                (Ok(()), Some(controller)) => {
                    if let Err(e) = configure(&controller, &env_for_configure) {
                        // Never stored, so nothing else would close it: its child window
                        // would linger over the preview's area.
                        log::if_err("ConfigureWebView2 Close", unsafe { controller.Close() });
                        app().report_error("ConfigureWebView2", INIT_FAILED, Some(&e));
                    }
                }
                (r, _) => app().report_error("CreateCoreWebView2Controller", INIT_FAILED, Some(&format!("{r:?}"))),
            }
            Ok(())
        }));
        if let Err(e) = create_controller(&env, parent, &handler) {
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

/// The preview runs InPrivate: its profile keeps history, downloads and cache in memory
/// only. The page is a `data:` URI holding the whole rendered document, and a normal
/// profile wrote it to disk — every download the preview cancelled still left a
/// History row whose `tab_url` was that URI, long after the document itself was gone.
fn create_controller(
    env: &ICoreWebView2Environment,
    parent: HWND,
    handler: &ICoreWebView2CreateCoreWebView2ControllerCompletedHandler,
) -> windows::core::Result<()> {
    unsafe {
        if let Ok(env10) = env.cast::<ICoreWebView2Environment10>() {
            let options = env10.CreateCoreWebView2ControllerOptions()?;
            options.SetIsInPrivateModeEnabled(true)?;
            return env10.CreateCoreWebView2ControllerWithOptions(parent, &options, handler);
        }
        log::error(
            "CreateCoreWebView2Controller",
            "ICoreWebView2Environment10 unavailable; the preview profile is persistent",
        );
        env.CreateCoreWebView2Controller(parent, handler)
    }
}

#[allow(unused_unsafe)]
fn configure(controller: &ICoreWebView2Controller, env: &ICoreWebView2Environment) -> windows::core::Result<()> {
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
        // Everything the preview does not use is off rather than left at the runtime's
        // default. DevTools above all: F12 or "Inspect" opened a console on the
        // document, undoing the no-script premise with one keystroke.
        settings.SetAreDevToolsEnabled(false)?;
        settings.SetIsWebMessageEnabled(false)?;
        settings.SetAreHostObjectsAllowed(false)?;
        if let Ok(s4) = settings.cast::<ICoreWebView2Settings4>() {
            // A hostile document can contain a form; nothing of the user's may land in it.
            s4.SetIsGeneralAutofillEnabled(false)?;
            s4.SetIsPasswordAutosaveEnabled(false)?;
        } else {
            // Every protection that is skipped on an older runtime says so: a silent
            // skip looks the same as one that is in place.
            log::error(
                "ConfigureWebView2",
                "ICoreWebView2Settings4 unavailable; autofill stays at the runtime default",
            );
        }

        let mut token = 0i64;
        webview.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                let allow = allow_navigation(&args).unwrap_or_else(|e| {
                    // WebView2 ignores a handler's error and lets the navigation proceed,
                    // so an early `?` here would fail open.
                    log::error("PreviewNavigationBlocked", format!("could not inspect navigation, cancelling: {e}"));
                    false
                });
                if !allow && let Err(e) = unsafe { args.SetCancel(true) } {
                    stop_preview("PreviewNavigationGuard", &e);
                }
                Ok(())
            })),
            &mut token,
        )?;

        // The only layer that stops traffic (see the module comment). Every request on a
        // network scheme is filtered, from every source, and anything not allowed gets a
        // local 403 instead of going out.
        if let Ok(wv22) = webview.cast::<ICoreWebView2_22>() {
            wv22.AddWebResourceRequestedFilterWithRequestSourceKinds(
                w!("*"),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
            )?;
        } else {
            log::error(
                "ConfigureWebView2",
                "ICoreWebView2_22 unavailable; request filter covers document requests only",
            );
            webview.AddWebResourceRequestedFilter(w!("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
        }
        let env = env.clone();
        webview.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let allow = allow_request(&args).unwrap_or_else(|e| {
                    log::error("PreviewRequestBlocked", format!("could not inspect request, blocking: {e}"));
                    false
                });
                if !allow {
                    let blocked = unsafe {
                        env.CreateWebResourceResponse(None, 403, w!("Blocked"), w!(""))
                            .and_then(|response| args.SetResponse(&response))
                    };
                    if let Err(e) = blocked {
                        stop_preview("PreviewRequestFilter", &e);
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;

        // Frame navigations never reach NavigationStarting. Nothing this app renders
        // needs a frame, so none may navigate.
        webview.add_FrameNavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                if let Err(e) = unsafe { args.SetCancel(true) } {
                    stop_preview("PreviewFrameNavigationGuard", &e);
                }
                let mut uri = PWSTR::null();
                let uri = match unsafe { args.Uri(&mut uri) } {
                    Ok(()) => take_pwstr(uri),
                    Err(_) => String::new(),
                };
                log::error(
                    "PreviewFrameNavigationBlocked",
                    format!("blocked frame navigation to {}", describe_uri(&uri)),
                );
                Ok(())
            })),
            &mut token,
        )?;

        // `<a href="data:…" download>` starts a download without any NavigationStarting
        // and, for file types the runtime does not warn about, saves it to the user's
        // Downloads folder unasked. Required, not best-effort: nothing else sees this
        // path, so a runtime without the event must not show the preview at all.
        webview.cast::<ICoreWebView2_4>()?.add_DownloadStarting(
            &DownloadStartingEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                let cancelled = unsafe { args.SetCancel(true).and_then(|()| args.SetHandled(true)) };
                if let Err(e) = cancelled {
                    stop_preview("PreviewDownloadGuard", &e);
                }
                log::error("PreviewDownloadBlocked", "blocked a download");
                Ok(())
            })),
            &mut token,
        )?;

        // An https image is the one request allowed out, and its server can ask for a
        // client certificate; unhandled, WebView2 shows the certificate picker, and the
        // one the user picks (a corporate certificate names its owner) goes to the
        // document's author. Required for the same reason as DownloadStarting.
        webview.cast::<ICoreWebView2_5>()?.add_ClientCertificateRequested(
            &ClientCertificateRequestedEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                if let Err(e) = unsafe { args.SetCancel(true) } {
                    stop_preview("PreviewClientCertificateGuard", &e);
                }
                log::error("PreviewClientCertificateBlocked", "refused a client certificate request");
                Ok(())
            })),
            &mut token,
        )?;

        // Ctrl/Shift/middle-click, target="_blank" and the context menu's "open in new
        // window" bypass NavigationStarting entirely: WebView2 opens a popup that inherits
        // neither IsScriptEnabled=false nor the CSP. Never let one be created.
        webview.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                if let Err(e) = unsafe { args.SetHandled(true) } {
                    stop_preview("PreviewNewWindowGuard", &e);
                }
                let mut uri = PWSTR::null();
                let uri = match unsafe { args.Uri(&mut uri) } {
                    Ok(()) => take_pwstr(uri),
                    Err(_) => String::new(),
                };
                log::error("PreviewNewWindowBlocked", format!("blocked new window for {}", describe_uri(&uri)));
                Ok(())
            })),
            &mut token,
        )?;

        // mailto:, ms-settings: and other OS protocols leave through this event, which a
        // navigation that slipped into the counter's window would otherwise reach.
        if let Ok(wv18) = webview.cast::<ICoreWebView2_18>() {
            wv18.add_LaunchingExternalUriScheme(
                &LaunchingExternalUriSchemeEventHandler::create(Box::new(|_, args| {
                    let Some(args) = args else { return Ok(()) };
                    if let Err(e) = unsafe { args.SetCancel(true) } {
                        stop_preview("PreviewExternalSchemeGuard", &e);
                    }
                    log::error("PreviewExternalSchemeBlocked", "blocked an external URI scheme launch");
                    Ok(())
                })),
                &mut token,
            )?;
        } else {
            log::error(
                "ConfigureWebView2",
                "ICoreWebView2_18 unavailable; external URI schemes are stopped by the navigation guard alone",
            );
        }

        webview.add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                // WebView2 drops a handler's Err without a word; a failure here would stop
                // crash recovery silently, so it is logged instead.
                log::if_err("PreviewProcessFailed", on_process_failed_event(&args));
                Ok(())
            })),
            &mut token,
        )?;

        webview.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                log::if_err("PreviewNavigationCompleted", on_navigation_completed(&args));
                Ok(())
            })),
            &mut token,
        )?;

        // Keyboard focus inside the WebView never reaches the main window's accelerator
        // table, so forward the app's shortcuts from here.
        controller.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                log::if_err("PreviewAcceleratorKeyPressed", on_accelerator_key(&args));
                Ok(())
            })),
            &mut token,
        )?;
    }

    let app = app();
    let (bounds, background, stopped) = {
        let p = app.preview.borrow();
        (p.bounds, p.background, p.stopped)
    };
    if stopped {
        // Only a restart gets here after a stop, and on_process_failed refuses those;
        // should another path appear, it still never brings the preview back.
        return Err(windows::core::Error::new(windows::Win32::Foundation::E_ABORT, "the preview was stopped"));
    }
    unsafe {
        log::if_err("ConfigureWebView2 SetBounds", controller.SetBounds(bounds));
        match controller.cast::<ICoreWebView2Controller2>() {
            Ok(c2) => log::if_err("ConfigureWebView2 background", c2.SetDefaultBackgroundColor(background)),
            Err(e) => log::error("ConfigureWebView2 ICoreWebView2Controller2", e),
        }
        controller.SetIsVisible(true)?;
    }
    {
        let mut p = app.preview.borrow_mut();
        p.controller = Some(controller.clone());
        p.webview = Some(webview);
        p.ready = true;
    }
    app.refresh_preview();
    Ok(())
}

fn on_process_failed_event(args: &ICoreWebView2ProcessFailedEventArgs) -> windows::core::Result<()> {
    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
    unsafe { args.ProcessFailedKind(&mut kind)? };
    let process = if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED {
        CrashedProcess::Renderer
    } else if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED {
        CrashedProcess::Browser
    } else {
        CrashedProcess::Other
    };
    if process == CrashedProcess::Other {
        log::error("PreviewProcessFailed", format!("ignored process failure kind {}", kind.0));
        return Ok(());
    }
    // Handled after this callback returns: recovery closes the controller that is
    // raising this very event, which must not happen from inside it.
    let wparam = WPARAM(if process == CrashedProcess::Browser { 1 } else { 0 });
    unsafe { PostMessageW(Some(app().main_hwnd()), WM_APP_PREVIEW_FAILED, wparam, LPARAM(0)) }
}

fn on_navigation_completed(args: &ICoreWebView2NavigationCompletedEventArgs) -> windows::core::Result<()> {
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
        // A render superseded by a newer one, and a hostile navigation the guard
        // cancelled, both land here as routine failures — only the newest render we
        // started is worth telling the user about.
        let report = {
            let mut p = app.preview.borrow_mut();
            if p.pending_self > 0 || id != p.latest_id {
                log::error(
                    "PreviewNavigationCompleted",
                    format!("superseded or blocked navigation {id}: {}", status.0),
                );
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
    {
        let mut p = app.preview.borrow_mut();
        p.navigation_failure_reported = false;
        p.renderer_crashes = 0;
    }
    sync_view(&app);
    Ok(())
}

fn on_accelerator_key(args: &ICoreWebView2AcceleratorKeyPressedEventArgs) -> windows::core::Result<()> {
    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
    let mut vk = 0u32;
    unsafe {
        args.KeyEventKind(&mut kind)?;
        args.VirtualKey(&mut vk)?;
    }
    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
        return Ok(());
    }
    // The accelerator table never matches with Alt held (no entry has FALT), so neither
    // does this: AltGr is Ctrl+Alt, and AltGr+S must not save.
    if unsafe { GetKeyState(VK_MENU.0 as i32) } < 0 {
        return Ok(());
    }
    let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    if let Some(cmd) = crate::app::accelerator_command(vk as u16, ctrl, shift) {
        unsafe { args.SetHandled(true)? };
        app().post_command(cmd);
    }
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
            let message = if e.code() == windows::Win32::Foundation::E_INVALIDARG {
                "プレビューを更新できませんでした。文書が大きすぎる可能性があります（プレビューの上限は約2MBです）。編集内容は保持されています。".to_string()
            } else {
                format!("プレビューを更新できませんでした。({}) 編集内容は保持されています。", e.message())
            };
            app.report_error("NavigateToString", &message, Some(&e));
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
    let Some(webview) = webview else {
        // Not reachable while eady implies a webview, but a stuck flag would end zoom
        // and scroll sync for good.
        this.preview.borrow_mut().script_busy = false;
        return;
    };
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

/// The installed WebView2 Runtime's version (バージョン情報), or `None` without one.
pub fn runtime_version() -> Option<String> {
    let mut version = PWSTR::null();
    match unsafe { GetAvailableCoreWebView2BrowserVersionString(None, &mut version) } {
        Ok(()) if !version.is_null() => Some(take_pwstr(version)),
        Ok(()) => None,
        Err(e) => {
            log::error("GetAvailableCoreWebView2BrowserVersionString", &e);
            None
        }
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
            log::if_err("Preview SetBounds", c.SetBounds(bounds));
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
            log::if_err("Preview SetDefaultBackgroundColor", controller.SetDefaultBackgroundColor(c));
        }
    }
}

pub fn notify_moved(app: &App) {
    let controller = app.preview.borrow().controller.clone();
    if let Some(c) = controller {
        unsafe {
            log::if_err("Preview NotifyParentWindowPositionChanged", c.NotifyParentWindowPositionChanged());
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
            log::if_err("Preview Close", c.Close());
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
    fn scheme_check_does_not_panic_on_non_ascii() {
        assert!(!uses_blocked_scheme("日本語"));
        assert!(!uses_blocked_scheme("ab日"));
    }

    #[test]
    fn guard_denies_unexpected_navigations() {
        assert!(navigation_allowed(1, "data:text/html;base64,AA"));
        assert!(!navigation_allowed(0, "data:text/html;base64,AA")); // not ours
        assert!(!navigation_allowed(1, "https://evil/")); // scheme we never produce
    }

    #[test]
    fn crashes_are_retried_a_bounded_number_of_times() {
        assert_eq!(crash_response(CrashedProcess::Renderer, 0, 0), CrashResponse::Rerender);
        assert_eq!(crash_response(CrashedProcess::Renderer, 2, 0), CrashResponse::Rerender);
        // A document that kills the renderer must not loop render → crash → render.
        assert_eq!(crash_response(CrashedProcess::Renderer, 3, 0), CrashResponse::GiveUp);
        assert_eq!(crash_response(CrashedProcess::Browser, 0, 2), CrashResponse::Restart);
        assert_eq!(crash_response(CrashedProcess::Browser, 0, 3), CrashResponse::GiveUp);
        // GPU/utility exits and a hung renderer leave our navigations alive: touch nothing.
        assert_eq!(crash_response(CrashedProcess::Other, 0, 0), CrashResponse::Ignore);
    }

    #[test]
    fn requests_leave_only_as_https_images() {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_OTHER,
        };
        let image = COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE;
        let document = COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT;
        // The accepted risk (render.rs's CSP comment), and nothing wider.
        assert!(request_allowed(image, "https://example.com/a.png"));
        assert!(request_allowed(image, "HTTPS://example.com/a.png"));
        assert!(!request_allowed(image, "http://192.168.0.1/a.png"));
        // A <meta refresh> or a clicked link: the guard cancels the page change, but only
        // this filter keeps the request itself from being sent.
        assert!(!request_allowed(document, "http://192.168.0.1/reboot"));
        assert!(!request_allowed(document, "https://evil/"));
        assert!(!request_allowed(COREWEBVIEW2_WEB_RESOURCE_CONTEXT_OTHER, "https://evil/prefetch"));
        assert!(!request_allowed(image, "file:///C:/x.png"));
        // Our own render is a data: URI and must never be filtered out.
        assert!(request_allowed(document, "data:text/html;charset=utf-8;base64,AAA"));
        // Byte-wise prefix check: a multi-byte character at the cut must not panic (a
        // panic here aborts the process). Not a network scheme, so the denylist passes it.
        assert!(request_allowed(image, "ht日"));
    }

    #[test]
    fn own_render_is_allowed_even_if_reported_user_initiated() {
        // WebView2 documents API-initiated navigations as user initiated; gating on that
        // flag would cancel every NavigateToString the moment a runtime follows its docs.
        assert!(navigation_allowed(1, "data:text/html;base64,AA"));
    }

    #[test]
    fn truncates_long_uris() {
        assert_eq!(describe_uri(""), "(uri なし)");
        assert!(describe_uri(&"a".repeat(500)).ends_with("…(以下省略)"));
    }
}
