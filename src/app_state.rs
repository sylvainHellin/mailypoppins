//! App-owned state that is not user-edited config (#0107).
//!
//! `config.toml` is the user's file: connection settings, mailbox mappings,
//! preferences they typed. Anything the *app* decides on their behalf, and
//! rewrites without asking, belongs here instead, at
//! `<data_dir>/state.json`. Today that is one key, the per-account default
//! signature, which #0107 moved out of `[accounts.signatures] default`.
//!
//! The file is pretty-printed JSON, load/save modelled on
//! [`crate::contacts::cache`]. A missing file, a missing account and a missing
//! key all mean "nothing recorded"; a corrupt file is logged and treated as
//! empty, never fatal, because the state it holds is a preference and losing
//! it must not stop the app from running.

use anyhow::{Context, Result};
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// `<data_dir>/state.json`.
pub fn state_path() -> PathBuf {
    crate::config::mailypoppins_data_dir().join("state.json")
}

/// The whole app state file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppState {
    /// Per-account state, keyed by the account name from `config.toml`.
    /// `BTreeMap` so the written file has a stable key order and a diff of it
    /// is readable.
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountState>,
}

/// What the app remembers about one account.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountState {
    /// The signature selected as this account's default (#0107), by file stem
    /// under `config_dir()/signatures/`. `None` (or absent) means no default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_signature: Option<String>,
}

impl AppState {
    /// Read `state.json`, or an empty state when it is missing or unreadable.
    ///
    /// Never fails: a corrupt file is warned about and replaced by the empty
    /// state, so the next `save` rewrites it rather than leaving the user with
    /// an app that refuses to start over a preferences file.
    pub fn load() -> Self {
        let path = state_path();
        let data = match fs::read_to_string(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                warn!("[state] cannot read {}: {e}", path.display());
                return Self::default();
            }
        };
        match serde_json::from_str(&data) {
            Ok(state) => state,
            Err(e) => {
                warn!(
                    "[state] unparseable {}, treating it as empty: {e}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Write `state.json`, creating the data directory if needed.
    pub fn save(&self) -> Result<()> {
        let path = state_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating state directory at {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(self)?;
        fs::write(&path, data)
            .with_context(|| format!("writing app state at {}", path.display()))?;
        Ok(())
    }

    /// The account's default signature name, if one is recorded.
    pub fn default_signature(&self, account: &str) -> Option<&str> {
        self.accounts
            .get(account)
            .and_then(|a| a.default_signature.as_deref())
    }

    /// Record (or, with `None`, clear) the account's default signature.
    /// In memory only; the caller decides when to [`AppState::save`].
    pub fn set_default_signature(&mut self, account: &str, name: Option<&str>) {
        let entry = self.accounts.entry(account.to_string()).or_default();
        entry.default_signature = name.map(str::to_string);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_env::TestDataDir;

    #[test]
    fn a_missing_state_file_loads_as_empty() {
        let _data = TestDataDir::new();
        let state = AppState::load();
        assert!(state.accounts.is_empty());
        assert!(state.default_signature("work").is_none());
    }

    #[test]
    fn a_default_round_trips_through_the_file() {
        let _data = TestDataDir::new();
        let mut state = AppState::load();
        state.set_default_signature("work", Some("formal"));
        state.save().unwrap();

        assert!(state_path().exists());
        let loaded = AppState::load();
        assert_eq!(loaded.default_signature("work"), Some("formal"));
        // An account nobody wrote about has no default.
        assert_eq!(loaded.default_signature("personal"), None);
    }

    #[test]
    fn clearing_a_default_removes_it() {
        let _data = TestDataDir::new();
        let mut state = AppState::load();
        state.set_default_signature("work", Some("formal"));
        state.save().unwrap();

        let mut state = AppState::load();
        state.set_default_signature("work", None);
        state.save().unwrap();

        assert_eq!(AppState::load().default_signature("work"), None);
    }

    /// A corrupt file is not fatal: it loads as empty and the next save
    /// repairs it.
    #[test]
    fn a_corrupt_state_file_loads_as_empty() {
        let _data = TestDataDir::new();
        let path = state_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();

        let mut state = AppState::load();
        assert!(state.accounts.is_empty());

        state.set_default_signature("work", Some("casual"));
        state.save().unwrap();
        assert_eq!(AppState::load().default_signature("work"), Some("casual"));
    }

    /// `save` creates the data directory rather than failing on a fresh
    /// install where nothing has written there yet.
    #[test]
    fn save_creates_the_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("does/not/exist/yet");
        let _override = crate::config::test_env::DataDirOverride::set(&nested);

        let mut state = AppState::default();
        state.set_default_signature("a", Some("s"));
        state.save().unwrap();

        assert!(nested.join("state.json").exists());
    }
}
