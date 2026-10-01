//! The mailypoppins desktop client's Rust layer (#0129).
//!
//! A third daemon client beside the CLI and the TUI: it links the client
//! crates only (`mp-client`, `mp-protocol`, `mp-core`), never the root
//! `mailypoppins` crate, and never falls back to the store when the daemon is
//! away. See `clients/desktop/docs/rust-layer.md` for the command surface.
//!
//! - [`paths`]: the data, runtime and log locations, as the binary derives them.
//! - [`connector`]: reaching the daemon, starting it on demand, typed failure.
//! - [`session`]: the one session, the ordered event pump, re-bootstrap.
//! - [`commands`]: the narrow Tauri commands.
//! - [`editor`]: the external editor a draft is opened in.
//! - [`settings`]: `desktop.json`, the desktop's own settings.
//! - [`attachments`]: attachments, a draft's list, the browser rendition.
//! - [`calendar`]: the agenda and an entry's `invite.ics` in the editor.
//! - [`daemon_files`]: `config.toml` and the daemon log in the editor.
//! - [`configuration`]: the Settings view's `config.get`, reload and passwords.
//! - [`reader`]: the `mpmsg` scheme serving `message.html`.
//! - [`navigation`]: the webview's navigation allowlist and intercept log.
//! - [`fixture`]: the daemon stand-in behind `MP_DESKTOP_FIXTURE=1`.
//! - [`menu`]: the native macOS menus, forwarded to the frontend's actions.
//! - [`terminal`]: the embedded terminal editor's PTY sessions.

pub mod attachments;
pub mod calendar;
pub mod commands;
pub mod configuration;
pub mod connector;
pub mod contacts;
pub mod daemon_files;
pub mod editor;
pub mod error;
pub mod fixture;
pub mod logging;
pub mod menu;
pub mod navigation;
pub mod paths;
pub mod reader;
pub mod session;
pub mod settings;
pub mod signatures;
pub mod terminal;

#[cfg(test)]
mod ts_bindings;

use tauri::webview::NewWindowResponse;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::navigation::{intercepted, navigation_allowed, InterceptLog};
use crate::session::{GuiEvent, InterceptSource, SessionHandle};
use crate::terminal::Terminals;

/// `MP_DESKTOP_FIXTURE=1` or `--fixture`: serve the fixtures, no daemon.
pub const FIXTURE_ENV: &str = "MP_DESKTOP_FIXTURE";

/// `MP_DESKTOP_WINDOW_SIZE=WxH`: the initial window size in logical pixels,
/// so a fixture run can open straight into the medium or narrow layout.
pub const WINDOW_SIZE_ENV: &str = "MP_DESKTOP_WINDOW_SIZE";
const DEFAULT_WINDOW_SIZE: (f64, f64) = (1400.0, 900.0);

/// `"950x800"` to `(950.0, 800.0)`; anything else is `None`.
fn parse_window_size(v: &str) -> Option<(f64, f64)> {
    let (w, h) = v.trim().split_once(['x', 'X'])?;
    let (w, h): (f64, f64) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0).then_some((w, h))
}

fn window_size() -> (f64, f64) {
    std::env::var(WINDOW_SIZE_ENV)
        .ok()
        .and_then(|v| parse_window_size(&v))
        .unwrap_or(DEFAULT_WINDOW_SIZE)
}

/// How long the scheme handler waits for a session still connecting.
const READER_CONNECT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

fn fixture_requested() -> bool {
    let env = std::env::var(FIXTURE_ENV)
        .map(|v| !matches!(v.trim(), "" | "0" | "false" | "no"))
        .unwrap_or(false);
    env || std::env::args().any(|a| a == "--fixture")
}

/// Let a held key repeat in the webview (#0137): macOS's press-and-hold
/// accent popup (`ApplePressAndHoldEnabled`, on by default) swallows key
/// repeat for every key in a WKWebView, so a held `j` moved once. The value
/// goes into the registration domain, the last place `NSUserDefaults` looks,
/// so a user's own `defaults write dev.mailypoppins.desktop
/// ApplePressAndHoldEnabled -bool true` (or the global domain's value) still
/// wins. Before the window exists, so its first key reads it.
#[cfg(target_os = "macos")]
fn key_repeat() {
    use objc2_foundation::{ns_string, NSDictionary, NSNumber, NSObject, NSUserDefaults};
    let off = NSNumber::new_bool(false);
    let value: &NSObject = &off;
    let defaults =
        NSDictionary::from_slices(&[ns_string!("ApplePressAndHoldEnabled")], &[&**value]);
    // SAFETY: the dictionary maps an NSString to an NSNumber, a property-list
    // value, which is what `registerDefaults:` takes.
    unsafe { NSUserDefaults::standardUserDefaults().registerDefaults(&defaults) };
}

#[cfg(not(target_os = "macos"))]
fn key_repeat() {}

/// A refused URL: log it, and tell the frontend.
fn refuse(app: &tauri::AppHandle, url: &tauri::Url, source: InterceptSource) {
    let entry = intercepted(&app.state::<InterceptLog>(), url, source);
    app.state::<SessionHandle>()
        .emit(GuiEvent::LinkIntercepted { url: entry });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let paths = paths::Paths::resolve();
    logging::init(&paths);
    let fixture = fixture_requested();
    tracing::info!(
        "[app] mp-desktop {} starting{}; data dir {}",
        env!("CARGO_PKG_VERSION"),
        if fixture { " in fixture mode" } else { "" },
        paths.data_dir.display()
    );
    key_repeat();
    let session = SessionHandle::new(fixture);
    let setup_session = session.clone();

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(session)
        .menu(menu::build)
        .on_menu_event(|app, event| menu::on_event(app, event.id().as_ref()))
        .manage(InterceptLog::default())
        .manage(Terminals::default())
        // Destroyed, not CloseRequested: a close the webview listens for is
        // prevented and decided there, and the user may choose to stay.
        .on_window_event(|window, event| {
            if let WindowEvent::Destroyed = event {
                window.state::<Terminals>().kill_all();
            }
        })
        .register_asynchronous_uri_scheme_protocol("mpmsg", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            let method = request.method().as_str().to_string();
            let path = request.uri().path().to_string();
            tauri::async_runtime::spawn_blocking(move || {
                let door = app.state::<SessionHandle>().door(READER_CONNECT_WAIT);
                responder.respond(reader::respond(&method, &path, door));
            });
        })
        .setup(move |app| {
            setup_session.start();
            let dev_origin = if cfg!(debug_assertions) {
                app.config().build.dev_url.clone()
            } else {
                None
            };
            let (width, height) = window_size();
            let nav_app = app.handle().clone();
            let win_app = app.handle().clone();
            WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("mailypoppins")
                .inner_size(width, height)
                .min_inner_size(480.0, 400.0)
                .on_navigation(move |url| {
                    let allowed = navigation_allowed(url, dev_origin.as_ref());
                    if !allowed {
                        refuse(&nav_app, url, InterceptSource::Navigation);
                    }
                    allowed
                })
                .on_new_window(move |url, _features| {
                    refuse(&win_app, &url, InterceptSource::NewWindow);
                    NewWindowResponse::Deny
                })
                .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::subscribe_events,
            commands::connection_status,
            commands::retry_connect,
            commands::bootstrap,
            commands::list_accounts,
            commands::list_mailboxes,
            commands::list_messages,
            commands::message_text,
            commands::message_html_meta,
            commands::search_local,
            commands::search_server_start,
            commands::search_server_cancel,
            commands::operation_cancel,
            commands::message_archive,
            commands::message_delete,
            commands::message_move,
            commands::message_set_flag,
            commands::message_set_read,
            commands::draft_discard,
            commands::draft_create,
            commands::draft_reply,
            commands::draft_forward,
            commands::draft_from_message,
            commands::draft_path,
            commands::draft_approve,
            commands::draft_demote,
            commands::draft_validate,
            commands::draft_preview,
            commands::draft_set_recipients,
            commands::signature_list,
            signatures::signature_read,
            signatures::signature_create,
            signatures::signature_rename,
            signatures::signature_delete,
            signatures::signature_set_default,
            editor::editor_open,
            editor::editor_setting_get,
            editor::editor_setting_set,
            settings::setting_get,
            settings::setting_set,
            commands::send_hold_status,
            commands::send_cancel_hold,
            commands::send_draft,
            commands::send_approved,
            commands::outbox_list,
            commands::outbox_retry,
            commands::outbox_discard,
            commands::message_fetch,
            attachments::attachment_open,
            attachments::attachment_save,
            attachments::html_open,
            attachments::hit_html_open,
            attachments::draft_attachments,
            attachments::draft_attach,
            attachments::draft_attachment_remove,
            attachments::draft_attachment_open,
            calendar::calendar_events,
            calendar::invite_source_open,
            calendar::invite_get,
            calendar::calendar_rsvp,
            calendar::invite_refusal,
            calendar::send_invite,
            contacts::contact_search,
            contacts::contact_rebuild,
            contacts::contact_vcard_draft,
            daemon_files::config_open,
            daemon_files::log_open,
            configuration::config_get,
            configuration::config_reload,
            configuration::config_set_password,
            configuration::config_init,
            configuration::config_add_account,
            configuration::config_oauth2_login,
            commands::sync_trigger,
            commands::restart_daemon,
            commands::intercepted_urls,
            commands::open_external,
            commands::version_info,
            commands::fixture_simulate,
            terminal::terminal_spawn,
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_kill,
        ])
        .build(tauri::generate_context!());
    match result {
        Ok(app) => app.run(|app, event| {
            if let RunEvent::Exit = event {
                app.state::<Terminals>().kill_all();
            }
        }),
        Err(e) => {
            tracing::error!("[app] the application failed: {e}");
            eprintln!("mp-desktop: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_window_size;

    /// The registration domain is this process's alone and never stored, so
    /// the test changes nothing outside it; it reads that domain, since a
    /// user's own value elsewhere rightly wins over it.
    #[cfg(target_os = "macos")]
    #[test]
    fn press_and_hold_is_registered_off() {
        use objc2_foundation::{ns_string, NSNumber, NSRegistrationDomain, NSUserDefaults};
        super::key_repeat();
        // SAFETY: an extern static Foundation defines.
        let domain = unsafe { NSRegistrationDomain };
        let registered = NSUserDefaults::standardUserDefaults().volatileDomainForName(domain);
        let value = registered
            .objectForKey(ns_string!("ApplePressAndHoldEnabled"))
            .expect("registered")
            .downcast::<NSNumber>()
            .expect("a number");
        assert!(!value.as_bool());
    }

    #[test]
    fn a_window_size_parses_as_width_x_height() {
        assert_eq!(parse_window_size("950x800"), Some((950.0, 800.0)));
        assert_eq!(parse_window_size(" 600 X 820 "), Some((600.0, 820.0)));
        assert_eq!(parse_window_size("600"), None);
        assert_eq!(parse_window_size("0x800"), None);
        assert_eq!(parse_window_size("wide"), None);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex, MutexGuard};

    static ENV: Mutex<()> = Mutex::new(());
    static SCRATCH: AtomicU32 = AtomicU32::new(0);

    /// Serialises the tests that touch the process environment.
    pub fn env_lock() -> MutexGuard<'static, ()> {
        match ENV.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// A fresh directory under the temp dir, unique to this process and call.
    pub fn scratch_dir(tag: &str) -> PathBuf {
        let n = SCRATCH.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("mp-desktop-test-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }
}
