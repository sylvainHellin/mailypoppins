//! The native application menus (#0129): App, File, Edit, View, Window, Help.
//!
//! An item that runs a GUI action carries one of [`ACTIONS`]' ids and is
//! emitted to the frontend as the [`MENU_EVENT`] event, where
//! `src/app/actions.ts` (`MENU_ACTIONS`) runs the same action its key does.
//! The Edit and Window items are the predefined macOS ones. No item carries
//! an accelerator the webview's keymap also owns: a menu key equivalent is
//! taken before the page sees the key, and a bare `z` or `?` there would stop
//! the user typing it into a field.

use tauri::menu::{AboutMetadata, Menu, MenuBuilder, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Runtime};

/// The event the frontend listens on (`src/lib/events.ts`).
pub const MENU_EVENT: &str = "menu";

/// Every custom item: its id, its label, and the menu it sits in.
pub const ACTIONS: &[(&str, &str, &str)] = &[
    ("restart_daemon", "Restart Daemon…", "File"),
    ("toggle_sidebar", "Toggle Sidebar", "View"),
    ("widen_list", "Widen List", "View"),
    ("narrow_list", "Narrow List", "View"),
    ("zoom_pane", "Zoom Focused Pane", "View"),
    ("command_palette", "Command Palette…", "View"),
    ("key_help", "Key Help", "View"),
    ("keyboard_shortcuts", "Keyboard Shortcuts", "Help"),
];

fn items_of<'m, R: Runtime>(
    mut sub: SubmenuBuilder<'m, R, AppHandle<R>>,
    menu: &str,
) -> SubmenuBuilder<'m, R, AppHandle<R>> {
    for (id, label, of) in ACTIONS {
        if *of == menu {
            sub = sub.text(*id, *label);
        }
    }
    sub
}

/// The menu bar, built once at startup.
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let about = AboutMetadata {
        name: Some("mailypoppins".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        ..Default::default()
    };
    let app_menu = SubmenuBuilder::new(app, "mailypoppins")
        .about(Some(about))
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;
    let file = items_of(SubmenuBuilder::new(app, "File"), "File")
        .separator()
        .close_window()
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let view = items_of(SubmenuBuilder::new(app, "View"), "View")
        .separator()
        .fullscreen()
        .build()?;
    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .separator()
        .close_window()
        .build()?;
    let help = items_of(SubmenuBuilder::new(app, "Help"), "Help").build()?;
    #[cfg(target_os = "macos")]
    {
        window.set_as_windows_menu_for_nsapp()?;
        help.set_as_help_menu_for_nsapp()?;
    }
    MenuBuilder::new(app)
        .items(&[&app_menu, &file, &edit, &view, &window, &help])
        .build()
}

/// Forward one of our items to the frontend; predefined items act natively.
pub fn on_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if ACTIONS.iter().any(|(known, _, _)| *known == id) {
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
        for (id, _, _) in ACTIONS {
            assert!(
                frontend.contains(&format!("  {id}: \"")),
                "src/app/actions.ts MENU_ACTIONS lacks {id}"
            );
        }
    }

    #[test]
    fn every_item_sits_in_a_known_menu() {
        for (_, _, menu) in ACTIONS {
            assert!(["File", "View", "Help"].contains(menu), "{menu}");
        }
    }
}
