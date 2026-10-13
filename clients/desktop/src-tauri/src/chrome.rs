//! The window's chrome on macOS (PERSO-97): no title bar, no traffic lights,
//! and the system's rounded corners, as in Zen or WhatsApp.
//!
//! `lib.rs` builds the window with `TitleBarStyle::Overlay` and a hidden
//! title: the window stays titled, so macOS rounds its corners, resizes it
//! from its edges and keeps its close, minimise and zoom buttons, which the
//! menu's predefined Close Window, Minimize, Zoom and Enter Full Screen
//! press (`menu.rs`); the webview fills the whole frame, and the page's drag
//! regions move it. A window without decorations would lose all of that.
//!
//! The buttons stay in place, hidden: [`hide_traffic_lights`] sets each of
//! [`TRAFFIC_LIGHTS`] hidden through AppKit on the main thread, once the
//! window is built and again after every resize, since AppKit lays the title
//! bar out afresh when the window leaves full screen and may show them.

use tauri::{Runtime, Window};

/// The three title bar buttons the window keeps hidden.
#[cfg(target_os = "macos")]
pub const TRAFFIC_LIGHTS: [objc2_app_kit::NSWindowButton; 3] = [
    objc2_app_kit::NSWindowButton::CloseButton,
    objc2_app_kit::NSWindowButton::MiniaturizeButton,
    objc2_app_kit::NSWindowButton::ZoomButton,
];

/// Hide the window's traffic lights, on the main thread; nothing elsewhere
/// than macOS. A failure is a log line: the window still works with them.
pub fn hide_traffic_lights<R: Runtime>(window: &Window<R>) {
    #[cfg(target_os = "macos")]
    {
        let target = window.clone();
        let scheduled = window.run_on_main_thread(move || match target.ns_window() {
            // SAFETY: `ns_window` is the live `NSWindow` of a window Tauri
            // still holds, and this runs on the main thread AppKit needs.
            Ok(ns_window) => unsafe { hide_buttons(ns_window) },
            Err(e) => tracing::warn!("[chrome] no NSWindow to hide the traffic lights of: {e}"),
        });
        if let Err(e) = scheduled {
            tracing::warn!("[chrome] could not reach the main thread: {e}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

/// Set each of [`TRAFFIC_LIGHTS`] hidden.
///
/// # Safety
///
/// `ns_window` is null or points to a live `NSWindow`, and the caller is on
/// the main thread.
#[cfg(target_os = "macos")]
unsafe fn hide_buttons(ns_window: *mut std::ffi::c_void) {
    let Some(window) = (ns_window as *const objc2_app_kit::NSWindow).as_ref() else {
        return;
    };
    for kind in TRAFFIC_LIGHTS {
        if let Some(button) = window.standardWindowButton(kind) {
            button.setHidden(true);
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use objc2_app_kit::NSWindowButton;

    /// Close, minimise and zoom, the three a title bar shows; the full screen
    /// button is the zoom button's other half since macOS 10.10.
    #[test]
    fn the_three_traffic_lights_are_hidden() {
        assert_eq!(
            TRAFFIC_LIGHTS,
            [
                NSWindowButton::CloseButton,
                NSWindowButton::MiniaturizeButton,
                NSWindowButton::ZoomButton
            ]
        );
    }

    /// A null window pointer is ignored, never dereferenced.
    #[test]
    fn a_null_window_hides_nothing() {
        unsafe { hide_buttons(std::ptr::null_mut()) };
    }
}
