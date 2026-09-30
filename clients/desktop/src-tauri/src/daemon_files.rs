//! The two files the daemon names, opened in the external editor (INT-01,
//! INT-02, #0131): `config.toml` (the TUI's `sc`) and the daemon's dated log
//! (the TUI's `sf`).
//!
//! The paths are the daemon's, never computed here: `config.get` answers the
//! file it reads with its `path` and its `state`, and `diagnostic.log_path`
//! answers the log it writes today. A daemon with no configuration answers
//! `state: "absent"`, which is refused before any editor starts; an `invalid`
//! file opens, since the editor is where it gets fixed. Both hand the path to
//! [`editor::open_on`], so a fixture journals the editor and runs nothing.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, State};

use crate::commands::{call, with_door};
use crate::editor::{self, EditorLaunch, Lookup};
use crate::error::{Addressing, GuiError};
use crate::session::{Door, SessionHandle};

/// `config.get` and `diagnostic.log_path` read memory only.
const QUERY_BUDGET: Duration = Duration::from_secs(5);

/// What `config_open` says when the daemon has no configuration.
pub const NO_CONFIG: &str = "There is no config.toml yet; add an account first";

/// A string field of an answer, or a protocol error naming the method.
fn answer_str<'a>(method: &str, answer: &'a Value, key: &str) -> Result<&'a str, GuiError> {
    answer[key]
        .as_str()
        .ok_or_else(|| GuiError::protocol(format!("{method} answered without a `{key}`")))
}

fn fixture_of(door: &Door) -> Option<&crate::fixture::Fixture> {
    match door {
        Door::Fixture(fixture) => Some(fixture.as_ref()),
        Door::Daemon(_) => None,
    }
}

/// Open the daemon's `config.toml` in the external editor; `not_found` with
/// [`NO_CONFIG`] when the daemon has none.
pub fn config_open_on(
    door: &Door,
    lookup: &Lookup,
    window: Duration,
) -> Result<EditorLaunch, GuiError> {
    let answer = call(
        door,
        "config.get",
        json!({}),
        QUERY_BUDGET,
        Addressing::Resource,
    )?;
    if answer_str("config.get", &answer, "state")? == "absent" {
        return Err(GuiError::not_found(NO_CONFIG));
    }
    let path = answer_str("config.get", &answer, "path")?;
    editor::open_on(fixture_of(door), lookup, path, window)
}

/// Open the daemon's current log file in the external editor; `not_found`
/// when the daemon has not written it yet.
pub fn log_open_on(
    door: &Door,
    lookup: &Lookup,
    window: Duration,
) -> Result<EditorLaunch, GuiError> {
    let answer = call(
        door,
        "diagnostic.log_path",
        json!({}),
        QUERY_BUDGET,
        Addressing::Resource,
    )?;
    let path = answer_str("diagnostic.log_path", &answer, "path")?;
    if !Path::new(path).is_file() {
        return Err(GuiError::not_found(format!("No log file found at {path}")));
    }
    editor::open_on(fixture_of(door), lookup, path, window)
}

/// The editor lookup with the settings file's `editor`, as `editor_open` reads it.
fn lookup_for(settings: &Path) -> Lookup<'static> {
    let setting = editor::read_setting(settings).unwrap_or_else(|e| {
        tracing::warn!("[editor] ignoring the setting: {e}");
        None
    });
    editor::live_lookup(setting)
}

/// `sc`: `not_found` when there is no `config.toml` yet.
#[tauri::command(rename_all = "snake_case")]
pub async fn config_open(
    app: AppHandle,
    session: State<'_, SessionHandle>,
) -> Result<EditorLaunch, GuiError> {
    let settings = editor::settings_file(&app)?;
    with_door(&session, move |_, door| {
        config_open_on(door, &lookup_for(&settings), editor::EXIT_WINDOW)
    })
    .await
}

/// `sf`: `not_found` when the daemon's log file does not exist yet.
#[tauri::command(rename_all = "snake_case")]
pub async fn log_open(
    app: AppHandle,
    session: State<'_, SessionHandle>,
) -> Result<EditorLaunch, GuiError> {
    let settings = editor::settings_file(&app)?;
    with_door(&session, move |_, door| {
        log_open_on(door, &lookup_for(&settings), editor::EXIT_WINDOW)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{Fixture, FIXTURE_LOG_LINES};
    use std::sync::Arc;

    fn fixture_door() -> (Door, Arc<Fixture>) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        (Door::Fixture(Arc::clone(&fixture)), fixture)
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn any_file(p: &Path) -> bool {
        p.is_file()
    }

    fn lookup() -> Lookup<'static> {
        Lookup {
            env: &no_env,
            setting: Some("zed --wait".to_string()),
            is_file: &any_file,
            macos: true,
        }
    }

    const WINDOW: Duration = Duration::from_millis(100);

    #[test]
    fn config_open_on_journals_the_editor_on_the_daemons_path() {
        let (door, f) = fixture_door();
        let launch = config_open_on(&door, &lookup(), WINDOW).expect("opened");
        assert_eq!(launch.pid, None, "a fixture spawns nothing");
        let path = f.root().join("config.toml");
        let text = std::fs::read_to_string(&path).expect("written at load");
        assert!(text.contains("name = \"work\""), "{text}");
        assert!(text.contains("name = \"home\""), "{text}");
        let opens = f.editor_opens();
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].path, path.to_string_lossy());
        assert_eq!(
            opens[0].command.last().map(String::as_str),
            Some(opens[0].path.as_str())
        );
    }

    #[test]
    fn config_open_on_an_absent_configuration_is_not_found_and_opens_nothing() {
        let (door, f) = fixture_door();
        f.set_config_state("absent");
        let err = config_open_on(&door, &lookup(), WINDOW).expect_err("absent");
        assert_eq!(err, GuiError::not_found(NO_CONFIG));
        assert!(f.editor_opens().is_empty());
    }

    #[test]
    fn config_open_on_an_invalid_configuration_still_opens_it() {
        let (door, f) = fixture_door();
        f.set_config_state("invalid");
        config_open_on(&door, &lookup(), WINDOW).expect("opened");
        assert_eq!(f.editor_opens().len(), 1);
    }

    #[test]
    fn log_open_on_journals_the_editor_on_the_dated_log() {
        let (door, f) = fixture_door();
        let launch = log_open_on(&door, &lookup(), WINDOW).expect("opened");
        assert_eq!(launch.pid, None);
        let path = f.root().join("logs").join("mailypoppins-2026-09-30.log");
        let text = std::fs::read_to_string(&path).expect("written at load");
        assert_eq!(text.lines().count(), FIXTURE_LOG_LINES);
        let opens = f.editor_opens();
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].path, path.to_string_lossy());
        assert_eq!(
            opens[0].command.last().map(String::as_str),
            Some(opens[0].path.as_str())
        );
    }

    #[test]
    fn log_open_on_a_missing_log_is_not_found_and_opens_nothing() {
        let (door, f) = fixture_door();
        let path = f.root().join("logs").join("mailypoppins-2026-09-30.log");
        std::fs::remove_file(&path).expect("removed");
        let err = log_open_on(&door, &lookup(), WINDOW).expect_err("missing");
        assert_eq!(
            err,
            GuiError::not_found(format!("No log file found at {}", path.display()))
        );
        assert!(f.editor_opens().is_empty());
    }
}
