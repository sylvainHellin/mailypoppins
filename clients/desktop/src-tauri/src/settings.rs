//! The desktop's own settings, `desktop.json` in the app config directory
//! (#0136).
//!
//! The file is a JSON object of string values, one per [`SettingKey`]: the
//! editor template ([`crate::editor`]), the theme, the reader's mode, and
//! whose colours the embedded editor shows ([`crate::terminal`]).
//! `setting_get` and `setting_set` read and write one key and keep every
//! other key of the file, unknown ones included; a key outside
//! [`SettingKey`] is refused with `not_found`. A missing file, a missing key
//! and a blank value all read as `null`, and writing `null` or a blank value
//! removes the key.
//!
//! None of it reaches the daemon: these are presentation choices of this
//! client, as the editor setting always was.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager};

use crate::error::GuiError;

/// The desktop settings file, in the app config directory.
pub const SETTINGS_FILE: &str = "desktop.json";

/// The values the `theme` key takes.
pub const THEMES: &[&str] = &["dark", "light", "system"];

/// The values the `reader_mode` key takes.
pub const READER_MODES: &[&str] = &["html", "text"];

/// The values the `editor_colors` key takes.
pub const EDITOR_COLORS: &[&str] = &["app", "editor"];

/// A key of `desktop.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(rename_all = "snake_case")]
pub enum SettingKey {
    /// The editor command template (see [`crate::editor`]).
    Editor,
    /// `dark`, `light` or `system`; unset is `dark`.
    Theme,
    /// `html` (the message's own markup in the reader frame) or `text` (the
    /// stored plain text); unset is `html`.
    ReaderMode,
    /// `app` (Neovim and Vim take the app's colorscheme, `mailypoppins`) or
    /// `editor` (the editor's own); unset is `app`.
    EditorColors,
}

impl SettingKey {
    pub const ALL: [SettingKey; 4] = [
        SettingKey::Editor,
        SettingKey::Theme,
        SettingKey::ReaderMode,
        SettingKey::EditorColors,
    ];

    /// The key as the file spells it.
    pub fn name(self) -> &'static str {
        match self {
            SettingKey::Editor => "editor",
            SettingKey::Theme => "theme",
            SettingKey::ReaderMode => "reader_mode",
            SettingKey::EditorColors => "editor_colors",
        }
    }

    /// The key a command names; `not_found` for one outside [`SettingKey`].
    pub fn parse(key: &str) -> Result<SettingKey, GuiError> {
        SettingKey::ALL
            .into_iter()
            .find(|k| k.name() == key)
            .ok_or_else(|| {
                let known: Vec<&str> = SettingKey::ALL.iter().map(|k| k.name()).collect();
                GuiError::not_found(format!(
                    "no desktop setting `{key}`; the settings are {}",
                    known.join(", ")
                ))
            })
    }

    /// Refuse a value this key cannot hold, with a sentence naming why.
    fn check(self, value: &str) -> Result<(), GuiError> {
        match self {
            SettingKey::Editor => crate::editor::split(value)
                .map(|_| ())
                .map_err(|message| GuiError::Setup { message }),
            SettingKey::Theme if !THEMES.contains(&value) => Err(GuiError::Setup {
                message: format!("the theme `{value}` is none of dark, light or system"),
            }),
            SettingKey::ReaderMode if !READER_MODES.contains(&value) => Err(GuiError::Setup {
                message: format!("the reader mode `{value}` is neither html nor text"),
            }),
            SettingKey::EditorColors if !EDITOR_COLORS.contains(&value) => Err(GuiError::Setup {
                message: format!("the editor colours `{value}` are neither app nor editor"),
            }),
            SettingKey::Theme | SettingKey::ReaderMode | SettingKey::EditorColors => Ok(()),
        }
    }
}

/// Whose colours the embedded editor shows: the `editor_colors` key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditorColors {
    /// Neovim and Vim take the `mailypoppins` colorscheme, in the app's
    /// palette; the default.
    #[default]
    App,
    /// The editor keeps its own colorscheme.
    Editor,
}

/// The `editor_colors` key as a spawn reads it: unset, unknown or a file
/// that does not read (logged) is [`EditorColors::App`].
pub fn editor_colors(file: &Path) -> EditorColors {
    match read(file, SettingKey::EditorColors) {
        Ok(Some(v)) if v.trim() == "editor" => EditorColors::Editor,
        Ok(_) => EditorColors::App,
        Err(e) => {
            tracing::warn!("[settings] the editor colours follow the app: {e}");
            EditorColors::App
        }
    }
}

/// `desktop.json` in `config_dir`.
pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join(SETTINGS_FILE)
}

/// The whole file; a missing or empty one is an empty object.
fn read_all(file: &Path) -> Result<Map<String, Value>, GuiError> {
    match std::fs::read_to_string(file) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) | Err(_) => Err(GuiError::Setup {
                message: format!("{} is not a JSON object", file.display()),
            }),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(GuiError::internal(format!(
            "could not read {}: {e}",
            file.display()
        ))),
    }
}

/// One key of the file, `None` when the file or the key is missing, the
/// value is not a string, or it is blank.
pub fn read(file: &Path, key: SettingKey) -> Result<Option<String>, GuiError> {
    Ok(read_all(file)?
        .get(key.name())
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|v| !v.trim().is_empty()))
}

/// Set one key to `value`, trimmed, or remove it on `None` or a blank value,
/// keeping every other key of the file.
pub fn write(file: &Path, key: SettingKey, value: Option<&str>) -> Result<(), GuiError> {
    let mut settings = read_all(file)?;
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => {
            key.check(value)?;
            settings.insert(key.name().to_string(), json!(value));
        }
        None => {
            settings.remove(key.name());
        }
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| GuiError::internal(format!("could not create {}: {e}", dir.display())))?;
    }
    let text =
        serde_json::to_string_pretty(&Value::Object(settings)).map_err(GuiError::internal)?;
    std::fs::write(file, text + "\n")
        .map_err(|e| GuiError::internal(format!("could not write {}: {e}", file.display())))
}

/// The settings file of this app.
pub(crate) fn settings_file(app: &AppHandle) -> Result<PathBuf, GuiError> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| GuiError::internal(format!("no app config directory: {e}")))?;
    Ok(settings_path(&dir))
}

/// `setting_get` over a file.
pub fn get_on(file: &Path, key: &str) -> Result<Option<String>, GuiError> {
    read(file, SettingKey::parse(key)?)
}

/// `setting_set` over a file: the value the key holds afterwards.
pub fn set_on(file: &Path, key: &str, value: Option<&str>) -> Result<Option<String>, GuiError> {
    let key = SettingKey::parse(key)?;
    write(file, key, value)?;
    read(file, key)
}

/// One key of `desktop.json`, `null` when unset.
#[tauri::command(rename_all = "snake_case")]
pub fn setting_get(app: AppHandle, key: String) -> Result<Option<String>, GuiError> {
    get_on(&settings_file(&app)?, &key)
}

/// Set one key of `desktop.json`, or remove it with `null`; answers the value
/// it holds afterwards.
#[tauri::command(rename_all = "snake_case")]
pub fn setting_set(
    app: AppHandle,
    key: String,
    value: Option<String>,
) -> Result<Option<String>, GuiError> {
    set_on(&settings_file(&app)?, &key, value.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_json(file: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(file).expect("file")).expect("json")
    }

    #[test]
    fn a_missing_file_reads_every_key_as_null() {
        let dir = crate::test_support::scratch_dir("settings-missing");
        let file = settings_path(&dir.join("nested"));
        for key in SettingKey::ALL {
            assert_eq!(get_on(&file, key.name()).expect("read"), None);
        }
    }

    #[test]
    fn a_set_keeps_the_other_keys_and_null_removes_one() {
        let dir = crate::test_support::scratch_dir("settings-keep");
        let file = settings_path(&dir.join("nested"));
        std::fs::create_dir_all(file.parent().unwrap()).expect("dir");
        std::fs::write(&file, r#"{"editor": "zed -w {path}", "later": 3}"#).expect("write");
        assert_eq!(
            set_on(&file, "theme", Some(" light ")).expect("set"),
            Some("light".to_string())
        );
        assert_eq!(
            set_on(&file, "reader_mode", Some("text")).expect("set"),
            Some("text".to_string())
        );
        assert_eq!(
            file_json(&file),
            json!({"editor": "zed -w {path}", "later": 3, "theme": "light", "reader_mode": "text"})
        );
        assert_eq!(
            get_on(&file, "theme").expect("get").as_deref(),
            Some("light")
        );
        assert_eq!(set_on(&file, "theme", None).expect("clear"), None);
        assert_eq!(
            set_on(&file, "reader_mode", Some("  ")).expect("blank"),
            None
        );
        assert_eq!(
            file_json(&file),
            json!({"editor": "zed -w {path}", "later": 3})
        );
        assert_eq!(
            crate::editor::read_setting(&file)
                .expect("editor")
                .as_deref(),
            Some("zed -w {path}")
        );
    }

    #[test]
    fn an_unknown_key_is_refused_and_writes_nothing() {
        let dir = crate::test_support::scratch_dir("settings-unknown");
        let file = settings_path(&dir);
        for key in ["colour", "", "Theme", "later"] {
            match get_on(&file, key) {
                Err(GuiError::NotFound { message, .. }) => {
                    assert!(
                        message.contains("editor, theme, reader_mode, editor_colors"),
                        "{message}"
                    )
                }
                other => panic!("{key}: {other:?}"),
            }
            assert!(matches!(
                set_on(&file, key, Some("x")),
                Err(GuiError::NotFound { .. })
            ));
        }
        assert!(!file.exists());
    }

    #[test]
    fn a_value_the_key_cannot_hold_is_refused() {
        let dir = crate::test_support::scratch_dir("settings-check");
        let file = settings_path(&dir);
        for theme in THEMES {
            assert_eq!(
                set_on(&file, "theme", Some(theme))
                    .expect("theme")
                    .as_deref(),
                Some(*theme)
            );
        }
        assert!(matches!(
            set_on(&file, "theme", Some("sepia")),
            Err(GuiError::Setup { .. })
        ));
        assert!(matches!(
            set_on(&file, "editor", Some("'unclosed")),
            Err(GuiError::Setup { .. })
        ));
        for mode in READER_MODES {
            assert_eq!(
                set_on(&file, "reader_mode", Some(mode))
                    .expect("mode")
                    .as_deref(),
                Some(*mode)
            );
        }
        for bad in ["markdown", "HTML", "plain"] {
            match set_on(&file, "reader_mode", Some(bad)) {
                Err(GuiError::Setup { message }) => {
                    assert!(message.contains("neither html nor text"), "{message}")
                }
                other => panic!("{bad}: {other:?}"),
            }
        }
        assert_eq!(
            file_json(&file),
            json!({"theme": "system", "reader_mode": "text"})
        );
    }

    #[test]
    fn the_editor_colours_round_trip_and_default_to_the_app() {
        let dir = crate::test_support::scratch_dir("settings-colors");
        let file = settings_path(&dir);
        assert_eq!(editor_colors(&file), EditorColors::App, "no file");
        std::fs::write(&file, r#"{"editor": "nvim"}"#).expect("write");
        assert_eq!(get_on(&file, "editor_colors").expect("get"), None);
        assert_eq!(editor_colors(&file), EditorColors::App, "unset");
        assert_eq!(
            set_on(&file, "editor_colors", Some("editor"))
                .expect("set")
                .as_deref(),
            Some("editor")
        );
        assert_eq!(editor_colors(&file), EditorColors::Editor);
        assert_eq!(
            file_json(&file),
            json!({"editor": "nvim", "editor_colors": "editor"})
        );
        assert_eq!(
            set_on(&file, "editor_colors", Some("app"))
                .expect("set")
                .as_deref(),
            Some("app")
        );
        assert_eq!(editor_colors(&file), EditorColors::App);
        match set_on(&file, "editor_colors", Some("theme")) {
            Err(GuiError::Setup { message }) => {
                assert!(message.contains("neither app nor editor"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(set_on(&file, "editor_colors", None).expect("clear"), None);
        assert_eq!(file_json(&file), json!({"editor": "nvim"}));
        std::fs::write(&file, "[1]").expect("write");
        assert_eq!(editor_colors(&file), EditorColors::App, "a broken file");
    }

    #[test]
    fn a_file_that_is_no_object_is_a_setup_error() {
        let dir = crate::test_support::scratch_dir("settings-bad");
        let file = settings_path(&dir);
        std::fs::write(&file, "[1]").expect("write");
        assert!(matches!(
            get_on(&file, "theme"),
            Err(GuiError::Setup { .. })
        ));
        assert!(matches!(
            set_on(&file, "theme", Some("dark")),
            Err(GuiError::Setup { .. })
        ));
        std::fs::write(&file, r#"{"theme": 3}"#).expect("write");
        assert_eq!(get_on(&file, "theme").expect("not a string"), None);
    }
}
