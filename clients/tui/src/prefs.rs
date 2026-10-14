//! The TUI's own presentation preferences, `tui.json` beside `config.toml`
//! (#0142).
//!
//! `config.toml` is the user's file, read once at startup and never written by
//! a client; a choice the TUI makes for the user with a key (today only the
//! per-account conversation list, `tc`) lives here instead, as the desktop
//! keeps its own in localStorage. The daemon never sees either: they are
//! presentation, not domain state.
//!
//! The file is small and readable by hand:
//!
//! ```json
//! {"flat_list_accounts": ["work"]}
//! ```
//!
//! An account the list does not name lists as conversations, so a new account
//! starts grouped. A missing file is the default; a file that cannot be read
//! or parsed is the default too, with a warning the caller surfaces, and is
//! never a reason not to start.
//!
//! A save re-reads the file and changes only the toggled account's entry, so
//! two TUIs open at once keep each other's choices, and a key this TUI does
//! not know survives. A file that failed to load is never written in that run,
//! so a hand edit with a typo is not replaced by the defaults.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};

/// The key `tui.json` keeps the flat accounts under.
const FLAT_LIST_ACCOUNTS: &str = "flat_list_accounts";

/// The file name inside `mp_core::config::config_dir()`.
pub const TUI_PREFS_FILE: &str = "tui.json";

/// Where the running TUI reads and writes its preferences.
pub fn tui_prefs_path() -> PathBuf {
    mp_core::config::config_dir().join(TUI_PREFS_FILE)
}

/// What `tui.json` holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TuiPrefs {
    /// The accounts the user turned the conversation list off for.
    pub flat_list_accounts: BTreeSet<String>,
}

impl TuiPrefs {
    /// Read `path`: the defaults when it does not exist, and the defaults plus
    /// a one-line warning when it cannot be read or is not the expected shape.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let (prefs, failure) = Self::load_failure(path);
        (prefs, failure.map(|(_, warning)| warning))
    }

    /// [`TuiPrefs::load`], with the short state a failure leaves the file in
    /// ("could not be read", "is malformed") beside the warning.
    fn load_failure(path: &Path) -> (Self, Option<(&'static str, String)>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                let state = "could not be read";
                return (
                    Self::default(),
                    Some((
                        state,
                        format!("{} {state} ({e}); using the defaults", path.display()),
                    )),
                );
            }
        };
        match Self::parse(&text) {
            Ok(prefs) => (prefs, None),
            Err(why) => {
                let state = "is malformed";
                (
                    Self::default(),
                    Some((
                        state,
                        format!("{} {state} ({why}); using the defaults", path.display()),
                    )),
                )
            }
        }
    }

    /// Parse the file's text. An unknown key is ignored, so a later TUI's file
    /// still loads here; a known key of the wrong shape is an error.
    fn parse(text: &str) -> std::result::Result<Self, String> {
        Self::parse_object(text).map(|(prefs, _)| prefs)
    }

    /// Parse the file's text into the preferences and the whole object, which
    /// a save writes back with only the known key replaced.
    fn parse_object(text: &str) -> std::result::Result<(Self, Map<String, Value>), String> {
        let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let Value::Object(object) = value else {
            return Err("not a JSON object".to_string());
        };
        let mut prefs = Self::default();
        if let Some(list) = object.get(FLAT_LIST_ACCOUNTS) {
            let items = list
                .as_array()
                .ok_or("`flat_list_accounts` is not an array")?;
            for item in items {
                let name = item
                    .as_str()
                    .ok_or("`flat_list_accounts` holds a non-string")?;
                prefs.flat_list_accounts.insert(name.to_string());
            }
        }
        Ok((prefs, object))
    }

    /// Write the preferences to `path` atomically, creating its directory.
    pub fn save(&self, path: &Path) -> Result<()> {
        write_object(path, Map::new(), &self.flat_list_accounts)
    }
}

/// Write `object` to `path` atomically with `flat` as its flat-account list,
/// creating the directory.
fn write_object(
    path: &Path,
    mut object: Map<String, Value>,
    flat: &BTreeSet<String>,
) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    object.insert(FLAT_LIST_ACCOUNTS.to_string(), serde_json::json!(flat));
    let mut text = serde_json::to_string_pretty(&Value::Object(object))?;
    text.push('\n');
    mp_core::draft::write_atomic(path, text.as_bytes())
}

/// The file's name for a status line, `tui.json` unless a test renamed it.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Which accounts list as conversations, and where the choice is kept (#0142).
///
/// The running TUI builds it with [`ListModes::load`]: every account the
/// stored list does not name is grouped, and `tc` writes the file back. The
/// `Default` is a test `App`'s: nothing is grouped until a test turns an
/// account on, and nothing is written, so a golden frame of the flat list
/// stays flat and no test touches a real config directory.
#[derive(Debug, Clone, Default)]
pub struct ListModes {
    /// What `tui.json` holds.
    pub prefs: TuiPrefs,
    /// Whether an account neither list names is grouped.
    pub grouped_by_default: bool,
    /// The accounts turned on in this run, which only matter while
    /// `grouped_by_default` is false.
    turned_on: BTreeSet<String>,
    /// Where a change is written; `None` writes nothing.
    pub path: Option<PathBuf>,
    /// Why the file is not written in this run: it failed to load, or a save
    /// found it malformed. `None` saves.
    unsaved: Option<String>,
}

impl ListModes {
    /// The running TUI's modes, read from `path`, with the warning a missing
    /// or malformed file earns. A file that failed to load is not written for
    /// the rest of the run.
    pub fn load(path: PathBuf) -> (Self, Option<String>) {
        let (prefs, failure) = TuiPrefs::load_failure(&path);
        let unsaved = failure
            .as_ref()
            .map(|(state, _)| format!("{} {state}", file_name(&path)));
        let warning = failure.map(|(_, warning)| warning);
        let modes = Self {
            prefs,
            grouped_by_default: true,
            turned_on: BTreeSet::new(),
            path: Some(path),
            unsaved,
        };
        (modes, warning)
    }

    /// Whether `account` lists as conversations.
    pub fn grouped(&self, account: &str) -> bool {
        !self.prefs.flat_list_accounts.contains(account)
            && (self.grouped_by_default || self.turned_on.contains(account))
    }

    /// Turn `account`'s conversation list on or off; whether that changed it.
    pub fn set(&mut self, account: &str, on: bool) -> bool {
        let was = self.grouped(account);
        if on {
            self.prefs.flat_list_accounts.remove(account);
            self.turned_on.insert(account.to_string());
        } else {
            self.turned_on.remove(account);
            self.prefs.flat_list_accounts.insert(account.to_string());
        }
        was != on
    }

    /// Save `account`'s mode where the preferences came from, if anywhere.
    ///
    /// The file is read again first and only `account`'s entry changes, so a
    /// choice another TUI saved since this one started, and any key this TUI
    /// does not know, survive. A file that failed to load, or that a save
    /// finds malformed, is left as it is and the error says why.
    pub fn persist(&mut self, account: &str) -> Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        if let Some(why) = &self.unsaved {
            return Err(anyhow!("{why}"));
        }
        let (mut flat, object) = match std::fs::read_to_string(&path) {
            Ok(text) => match TuiPrefs::parse_object(&text) {
                Ok((prefs, object)) => (prefs.flat_list_accounts, object),
                Err(_) => {
                    let why = format!("{} is malformed", file_name(&path));
                    self.unsaved = Some(why.clone());
                    return Err(anyhow!("{why}"));
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (BTreeSet::new(), Map::new()),
            Err(e) => {
                return Err(e).with_context(|| format!("{} could not be read", file_name(&path)))
            }
        };
        if self.prefs.flat_list_accounts.contains(account) {
            flat.insert(account.to_string());
        } else {
            flat.remove(account);
        }
        write_object(&path, object, &flat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loaded_mode_groups_every_account_the_file_does_not_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TUI_PREFS_FILE);
        std::fs::write(&path, r#"{"flat_list_accounts": ["work"]}"#).unwrap();
        let (mut modes, warning) = ListModes::load(path.clone());
        assert_eq!(warning, None);
        assert!(!modes.grouped("work"));
        assert!(modes.grouped("home"));

        assert!(modes.set("work", true));
        assert!(modes.set("home", false));
        assert!(!modes.set("home", false), "a second off changes nothing");
        modes.persist("work").unwrap();
        modes.persist("home").unwrap();
        let (back, _) = ListModes::load(path);
        assert!(back.grouped("work"));
        assert!(!back.grouped("home"));
    }

    #[test]
    fn a_test_mode_is_flat_until_turned_on_and_writes_nothing() {
        let mut modes = ListModes::default();
        assert!(!modes.grouped("work"));
        assert!(modes.set("work", true));
        assert!(modes.grouped("work"));
        assert!(!modes.grouped("home"));
        modes.persist("work").unwrap();
        assert_eq!(modes.path, None);
    }

    #[test]
    fn a_save_keeps_what_another_tui_saved_since_this_one_started() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TUI_PREFS_FILE);
        std::fs::write(&path, r#"{"flat_list_accounts": []}"#).unwrap();
        let (mut first, _) = ListModes::load(path.clone());
        let (mut second, _) = ListModes::load(path.clone());

        first.set("work", false);
        first.persist("work").unwrap();
        second.set("home", false);
        second.persist("home").unwrap();
        let (back, _) = ListModes::load(path.clone());
        assert!(!back.grouped("work"), "the first TUI's choice survives");
        assert!(!back.grouped("home"));

        // Turning one account back on removes only that one.
        first.set("work", true);
        first.persist("work").unwrap();
        let (back, _) = ListModes::load(path);
        assert!(back.grouped("work"));
        assert!(!back.grouped("home"), "the second TUI's choice survives");
    }

    #[test]
    fn a_save_keeps_a_key_this_tui_does_not_know() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TUI_PREFS_FILE);
        std::fs::write(&path, r#"{"later": {"x": 1}, "flat_list_accounts": []}"#).unwrap();
        let (mut modes, _) = ListModes::load(path.clone());
        modes.set("work", false);
        modes.persist("work").unwrap();
        let stored: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            stored,
            serde_json::json!({ "later": { "x": 1 }, "flat_list_accounts": ["work"] })
        );
    }

    #[test]
    fn a_file_that_failed_to_load_is_never_written_in_that_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TUI_PREFS_FILE);
        let typo = r#"{"flat_list_accounts": ["work", "home",]}"#;
        std::fs::write(&path, typo).unwrap();
        let (mut modes, warning) = ListModes::load(path.clone());
        assert!(warning.is_some_and(|w| w.contains("malformed")));

        // Even once the user fixes the file, this run leaves it alone.
        for text in [typo, r#"{"flat_list_accounts": ["work"]}"#] {
            std::fs::write(&path, text).unwrap();
            modes.set("other", false);
            let err = modes.persist("other").unwrap_err().to_string();
            assert_eq!(err, "tui.json is malformed");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }

    #[test]
    fn a_file_that_turned_malformed_since_start_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TUI_PREFS_FILE);
        let (mut modes, warning) = ListModes::load(path.clone());
        assert_eq!(warning, None, "a missing file loads cleanly");
        let typo = r#"{"flat_list_accounts": ["work",]}"#;
        std::fs::write(&path, typo).unwrap();
        modes.set("home", false);
        let err = modes.persist("home").unwrap_err().to_string();
        assert_eq!(err, "tui.json is malformed");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), typo);
    }

    #[test]
    fn an_unreadable_file_is_never_written_in_that_run() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be reads as an error, not as
        // missing, on every platform the TUI runs on.
        let path = dir.path().join(TUI_PREFS_FILE);
        std::fs::create_dir(&path).unwrap();
        let (mut modes, warning) = ListModes::load(path.clone());
        assert!(warning.is_some_and(|w| w.contains("could not be read")));
        modes.set("work", false);
        let err = modes.persist("work").unwrap_err().to_string();
        assert_eq!(err, "tui.json could not be read");
        assert!(path.is_dir());
    }

    #[test]
    fn a_missing_file_is_the_default_without_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let (prefs, warning) = TuiPrefs::load(&dir.path().join(TUI_PREFS_FILE));
        assert_eq!(prefs, TuiPrefs::default());
        assert_eq!(warning, None);
    }

    #[test]
    fn a_saved_file_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(TUI_PREFS_FILE);
        let prefs = TuiPrefs {
            flat_list_accounts: ["work".to_string(), "home".to_string()].into(),
        };
        prefs.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"flat_list_accounts\""), "{text}");
        assert_eq!(TuiPrefs::load(&path), (prefs, None));
    }

    #[test]
    fn a_malformed_file_is_the_default_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TUI_PREFS_FILE);
        for text in [
            "{not json",
            "[]",
            r#"{"flat_list_accounts": "work"}"#,
            r#"{"flat_list_accounts": [1]}"#,
        ] {
            std::fs::write(&path, text).unwrap();
            let (prefs, warning) = TuiPrefs::load(&path);
            assert_eq!(prefs, TuiPrefs::default(), "{text}");
            let warning = warning.unwrap_or_else(|| panic!("no warning for {text}"));
            assert!(warning.contains("malformed"), "{warning}");
        }
    }

    #[test]
    fn an_unknown_key_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TUI_PREFS_FILE);
        std::fs::write(&path, r#"{"later": true, "flat_list_accounts": ["a"]}"#).unwrap();
        let (prefs, warning) = TuiPrefs::load(&path);
        assert_eq!(warning, None);
        assert!(prefs.flat_list_accounts.contains("a"));
    }
}
