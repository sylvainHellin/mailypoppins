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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!(
                        "{} could not be read ({e}); using the defaults",
                        path.display()
                    )),
                )
            }
        };
        match Self::parse(&text) {
            Ok(prefs) => (prefs, None),
            Err(why) => (
                Self::default(),
                Some(format!(
                    "{} is malformed ({why}); using the defaults",
                    path.display()
                )),
            ),
        }
    }

    /// Parse the file's text. An unknown key is ignored, so a later TUI's file
    /// still loads here; a known key of the wrong shape is an error.
    fn parse(text: &str) -> std::result::Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let object = value.as_object().ok_or("not a JSON object")?;
        let mut prefs = Self::default();
        if let Some(list) = object.get("flat_list_accounts") {
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
        Ok(prefs)
    }

    /// Write the preferences to `path` atomically, creating its directory.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
        }
        let body = serde_json::json!({ "flat_list_accounts": self.flat_list_accounts });
        let mut text = serde_json::to_string_pretty(&body)?;
        text.push('\n');
        mp_core::draft::write_atomic(path, text.as_bytes())
    }
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
}

impl ListModes {
    /// The running TUI's modes, read from `path`, with the warning a missing
    /// or malformed file earns.
    pub fn load(path: PathBuf) -> (Self, Option<String>) {
        let (prefs, warning) = TuiPrefs::load(&path);
        let modes = Self {
            prefs,
            grouped_by_default: true,
            turned_on: BTreeSet::new(),
            path: Some(path),
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

    /// Write the preferences where they came from, if anywhere.
    pub fn persist(&self) -> Result<()> {
        match &self.path {
            Some(path) => self.prefs.save(path),
            None => Ok(()),
        }
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
        modes.persist().unwrap();
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
        modes.persist().unwrap();
        assert_eq!(modes.path, None);
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
