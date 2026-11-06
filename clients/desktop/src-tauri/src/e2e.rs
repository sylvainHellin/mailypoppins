//! The end-to-end tests' hooks, compiled with the `e2e` feature only
//! (`pnpm test:e2e`, PERSO-271), and only in effect on the fixtures.
//!
//! - The embedded WebDriver server (`tauri-plugin-wdio-webdriver`) the specs
//!   drive the window through, on 127.0.0.1. It has no commands, so the
//!   webview holds no permission for it.
//! - A run stays out of the user's way while they work on the same Mac: the
//!   app is an accessory that is never activated (no keyboard focus, no Dock
//!   icon, no menu bar of its own), and its window opens unfocused, behind
//!   every other window, fully transparent, click-through and left out of
//!   Mission Control.
//!
//! The window stays on screen and ordered in, where hiding it would not do:
//! WebKit marks the page of a hidden, off-screen or occluded window hidden
//! (`document.hidden`, no animation frames, throttled timers), and macOS
//! keeps a titled window from being placed off the screens. With
//! `ActivationPolicy::Prohibited` the page runs hidden too, so the policy is
//! `Accessory`, which only a click on the window could activate.

use tauri::{App, Builder, Manager, Runtime, WebviewWindow, WebviewWindowBuilder};

/// The WebDriver server, and no activation at launch.
pub fn builder<R: Runtime>(builder: Builder<R>, fixture: bool) -> Builder<R> {
    if !fixture {
        return builder;
    }
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    #[cfg(target_os = "macos")]
    let builder = builder.activate_ignoring_other_apps(false);
    builder
}

/// An accessory app: no Dock icon, no menu bar, no activation of its own.
/// Called from `setup`, which runs before the launch applies the policy.
pub fn setup<R: Runtime>(app: &mut App<R>, fixture: bool) {
    #[cfg(target_os = "macos")]
    if fixture {
        app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, fixture);
}

/// The main window, opened without taking the keyboard focus.
pub fn window<'a, R: Runtime, M: Manager<R>>(
    builder: WebviewWindowBuilder<'a, R, M>,
    fixture: bool,
) -> WebviewWindowBuilder<'a, R, M> {
    if !fixture {
        return builder;
    }
    builder.focused(false)
}

/// Put the built window out of sight while its page keeps running.
pub fn out_of_sight<R: Runtime>(window: &WebviewWindow<R>, fixture: bool) {
    if !fixture {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        keep_rendering(window);
        transparent_at_the_back(window);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

/// Behind every other window, fully transparent, click-through, out of
/// Mission Control and the window cycle. WebDriver clicks and keys are
/// synthesized in the page, so they reach it all the same.
#[cfg(target_os = "macos")]
fn transparent_at_the_back<R: Runtime>(window: &WebviewWindow<R>) {
    use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};
    let ns_window = match window.ns_window() {
        Ok(ptr) => ptr,
        Err(e) => {
            tracing::warn!("[e2e] no NSWindow to put out of sight: {e}");
            return;
        }
    };
    // SAFETY: Tauri's `ns_window` is the window's live NSWindow, and `setup`,
    // the only caller, runs on the main thread AppKit requires.
    let ns_window: &NSWindow = unsafe { &*ns_window.cast::<NSWindow>() };
    ns_window.setAlphaValue(0.0);
    ns_window.setIgnoresMouseEvents(true);
    ns_window.setCollectionBehavior(
        NSWindowCollectionBehavior::Transient | NSWindowCollectionBehavior::IgnoresCycle,
    );
    // Out and back in: WebKit reads the page's visibility again when its
    // window comes on screen, now with occlusion detection off.
    ns_window.orderOut(None);
    ns_window.orderBack(None);
    tracing::info!("[e2e] the window is transparent, click-through and at the back");
}

/// Keep the page of the covered, transparent window visible to itself:
/// WKWebView's occlusion detection would mark it hidden. It must be off
/// before the window is ordered in again, which is when WebKit reads the
/// page's visibility. The switch,
/// `_setWindowOcclusionDetectionEnabled:`, is WebKit SPI, the one its own
/// tests use; a WebKit without it only logs, and the page runs hidden.
#[cfg(target_os = "macos")]
fn keep_rendering<R: Runtime>(window: &WebviewWindow<R>) {
    let result = window.with_webview(|webview| {
        use objc2::runtime::{AnyObject, Bool};
        use objc2::{msg_send, sel};
        // SAFETY: wry's inner view on macOS is the WKWebView, alive for the
        // length of the call, and the selector is sent only when the view
        // answers it, with the BOOL argument it takes.
        unsafe {
            let view: &AnyObject = &*webview.inner().cast::<AnyObject>();
            let selector = sel!(_setWindowOcclusionDetectionEnabled:);
            let answers: Bool = msg_send![view, respondsToSelector: selector];
            if answers.as_bool() {
                let _: () = msg_send![view, _setWindowOcclusionDetectionEnabled: Bool::NO];
                tracing::info!("[e2e] occlusion detection off, the covered page stays visible");
            } else {
                tracing::warn!(
                    "[e2e] this WebKit has no occlusion switch; the covered page runs hidden"
                );
            }
        }
    });
    if let Err(e) = result {
        tracing::warn!("[e2e] could not reach the webview: {e}");
    }
}
