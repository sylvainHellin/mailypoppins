//! The native application menus (#0129): App, File, Edit, View, Window, Help.
//!
//! An item that runs a GUI action carries one of [`ACTIONS`]' ids and is
//! emitted to the frontend as the [`MENU_EVENT`] event, where
//! `src/app/actions.ts` (`MENU_ACTIONS`) runs the same action its key does.
//! The Edit and Window items are the predefined macOS ones. No item carries
//! an accelerator the webview's keymap also owns: a menu key equivalent is
//! taken before the page sees the key, and a bare `z` or `?` there would stop
//! the user typing it into a field. Cmd+, on "Settings…" is the one
//! accelerator, the macOS convention, which the keymap never sees anyway
//! since it passes every Cmd combination on.
//!
//! "Check for Updates…" under About is the one item outside [`ACTIONS`]: it
//! emits [`UPDATE_CHECK_REQUESTED_EVENT`] (#0139), and the frontend runs the
//! manual check so its notice line shows the answer.
//!
//! The window has no title bar on macOS (PERSO-97, `decorations(false)` in
//! `lib.rs`), and AppKit's predefined Close Window, Minimize, Zoom and Enter
//! Full Screen only press the title bar's buttons (`performClose:`,
//! `performMiniaturize:`, `performZoom:`, `toggleFullScreen:`), which a
//! borderless window refuses with the alert sound. [`WINDOW_ITEMS`] are their
//! replacements: Rust runs each on the window itself, and Close Window goes
//! through `close()`, which asks the webview's close guard as the button did.
//! Their accelerators are the system's, all with Cmd, which the keymap never
//! sees.

use tauri::menu::{AboutMetadata, Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::updates::UPDATE_CHECK_REQUESTED_EVENT;

/// The App menu's "Check for Updates…".
pub const CHECK_FOR_UPDATES: &str = "check_for_updates";

/// The event the frontend listens on (`src/lib/events.ts`).
pub const MENU_EVENT: &str = "menu";

/// Every custom item: its id, its label, the menu it sits in, and its
/// accelerator if it has one.
pub const ACTIONS: &[(&str, &str, &str, Option<&str>)] = &[
    ("settings", "Settings…", "App", Some("CmdOrCtrl+,")),
    ("restart_daemon", "Restart Daemon…", "File", None),
    ("toggle_sidebar", "Toggle Sidebar", "View", None),
    ("widen_list", "Widen List", "View", None),
    ("narrow_list", "Narrow List", "View", None),
    ("zoom_pane", "Zoom Focused Pane", "View", None),
    ("command_palette", "Command Palette…", "View", None),
    ("key_help", "Key Help", "View", None),
    ("keyboard_shortcuts", "Keyboard Shortcuts", "Help", None),
];

/// The window's own items, run in Rust on the `main` window, in the shape of
/// [`ACTIONS`].
pub const WINDOW_ITEMS: &[(&str, &str, &str, Option<&str>)] = &[
    ("close_window", "Close Window", "File", Some("CmdOrCtrl+W")),
    ("minimize_window", "Minimize", "Window", Some("CmdOrCtrl+M")),
    ("zoom_window", "Zoom", "Window", None),
    (
        "toggle_fullscreen",
        "Toggle Full Screen",
        "View",
        Some("Ctrl+Cmd+F"),
    ),
];

fn items_of<'m, R: Runtime>(
    app: &AppHandle<R>,
    sub: SubmenuBuilder<'m, R, AppHandle<R>>,
    menu: &str,
) -> tauri::Result<SubmenuBuilder<'m, R, AppHandle<R>>> {
    items_from(app, sub, menu, ACTIONS)
}

fn items_from<'m, R: Runtime>(
    app: &AppHandle<R>,
    mut sub: SubmenuBuilder<'m, R, AppHandle<R>>,
    menu: &str,
    table: &[(&str, &str, &str, Option<&str>)],
) -> tauri::Result<SubmenuBuilder<'m, R, AppHandle<R>>> {
    for (id, label, of, accelerator) in table {
        if *of != menu {
            continue;
        }
        sub = match accelerator {
            Some(keys) => sub.item(
                &MenuItemBuilder::with_id(*id, *label)
                    .accelerator(keys)
                    .build(app)?,
            ),
            None => sub.text(*id, *label),
        };
    }
    Ok(sub)
}

/// The menu bar, built once at startup.
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let about = AboutMetadata {
        name: Some("mailypoppins".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        ..Default::default()
    };
    let app_menu = items_of(
        app,
        SubmenuBuilder::new(app, "mailypoppins")
            .about(Some(about))
            .text(CHECK_FOR_UPDATES, "Check for Updates…")
            .separator(),
        "App",
    )?
    .separator()
    .services()
    .separator()
    .hide()
    .hide_others()
    .show_all()
    .separator()
    .quit()
    .build()?;
    let file = items_of(app, SubmenuBuilder::new(app, "File"), "File")?.separator();
    let file = items_from(app, file, "File", WINDOW_ITEMS)?.build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let view = items_of(app, SubmenuBuilder::new(app, "View"), "View")?.separator();
    let view = items_from(app, view, "View", WINDOW_ITEMS)?.build()?;
    let window = items_from(
        app,
        SubmenuBuilder::new(app, "Window"),
        "Window",
        WINDOW_ITEMS,
    )?
    .build()?;
    let help = items_of(app, SubmenuBuilder::new(app, "Help"), "Help")?.build()?;
    #[cfg(target_os = "macos")]
    {
        window.set_as_windows_menu_for_nsapp()?;
        help.set_as_help_menu_for_nsapp()?;
    }
    MenuBuilder::new(app)
        .items(&[&app_menu, &file, &edit, &view, &window, &help])
        .build()
}

/// Run one of [`WINDOW_ITEMS`] on the `main` window.
fn window_item<R: Runtime>(app: &AppHandle<R>, id: &str) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    match id {
        "close_window" => window.close(),
        "minimize_window" => window.minimize(),
        "zoom_window" if window.is_maximized()? => window.unmaximize(),
        "zoom_window" => window.maximize(),
        "toggle_fullscreen" => window.set_fullscreen(!window.is_fullscreen()?),
        _ => Ok(()),
    }
}

/// Forward one of our items to the frontend or run it on the window;
/// predefined items act natively.
pub fn on_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if WINDOW_ITEMS.iter().any(|(known, ..)| *known == id) {
        if let Err(e) = window_item(app, id) {
            tracing::warn!("[menu] {id} failed: {e}");
        }
        return;
    }
    if id == CHECK_FOR_UPDATES {
        if let Err(e) = app.emit(UPDATE_CHECK_REQUESTED_EVENT, ()) {
            tracing::warn!("[menu] could not emit {UPDATE_CHECK_REQUESTED_EVENT}: {e}");
        }
        return;
    }
    if ACTIONS.iter().any(|(known, ..)| *known == id) {
        if let Err(e) = app.emit(MENU_EVENT, id) {
            tracing::warn!("[menu] could not emit {id}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frontend maps these ids (`MENU_ACTIONS` in src/app/actions.ts).
    #[test]
    fn every_action_id_is_one_the_frontend_runs() {
        let frontend = include_str!("../../src/app/actions.ts");
        for (id, ..) in ACTIONS {
            assert!(
                frontend.contains(&format!("  {id}: \"")),
                "src/app/actions.ts MENU_ACTIONS lacks {id}"
            );
        }
    }

    /// The update item is not an action, so no action may take its id.
    #[test]
    fn the_update_item_is_no_action() {
        assert!(ACTIONS.iter().all(|(id, ..)| *id != CHECK_FOR_UPDATES));
    }

    /// A window item is run in Rust, never forwarded, so it takes no action's id.
    #[test]
    fn the_window_items_are_no_actions() {
        for (id, ..) in WINDOW_ITEMS {
            assert!(ACTIONS.iter().all(|(action, ..)| action != id), "{id}");
            assert_ne!(*id, CHECK_FOR_UPDATES);
        }
    }

    /// Every window item `window_item` runs, and the system's keys on the
    /// three that have one.
    #[test]
    fn the_window_items_keep_the_system_keys() {
        let keys: Vec<_> = WINDOW_ITEMS.iter().map(|(id, _, _, k)| (*id, *k)).collect();
        assert!(keys.contains(&("close_window", Some("CmdOrCtrl+W"))));
        assert!(keys.contains(&("minimize_window", Some("CmdOrCtrl+M"))));
        assert!(keys.contains(&("toggle_fullscreen", Some("Ctrl+Cmd+F"))));
        let source = include_str!("menu.rs");
        for (id, ..) in WINDOW_ITEMS {
            assert!(
                source.contains(&format!("        \"{id}\" ")),
                "window_item lacks {id}"
            );
        }
    }

    #[test]
    fn every_item_sits_in_a_known_menu() {
        for (_, _, menu, _) in ACTIONS {
            assert!(["App", "File", "View", "Help"].contains(menu), "{menu}");
        }
    }

    /// The keymap passes every Cmd combination on, so an accelerator never
    /// takes a key the page would otherwise read; a bare key here would.
    #[test]
    fn every_accelerator_carries_the_command_modifier() {
        for (id, _, _, accelerator) in ACTIONS {
            if let Some(keys) = accelerator {
                assert!(keys.starts_with("CmdOrCtrl+"), "{id}: {keys}");
            }
        }
    }
}
