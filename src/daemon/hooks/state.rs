//! The durable half of a hook: which messages it has already considered
//! (#0135).
//!
//! It lives in `<account_dir>/hooks-state.json` and not in the store, because
//! the store is a cache the daemon drops and rebuilds on a schema bump, and a
//! cursor lost with it would either re-fire every message the rebuild
//! re-ingests or need the very backfill rule this file exists to avoid.
//!
//! Per hook it keeps the mailbox and UIDVALIDITY the cursor was taken under,
//! `last_uid` (every UID at or below it has been considered), the UIDs above
//! it already considered out of order, the Message-IDs it fired for, and the
//! last run's outcome for `mp hooks list`. It is written whole, to a temporary
//! file renamed over the old one, before any command runs, so a crash between
//! the claim and the command loses the run rather than repeating it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// How many fired Message-IDs a hook remembers. They catch the one case the
/// UID cursor cannot: a message moved out of the mailbox and back, which the
/// server gives a new, higher UID.
pub const FIRED_MEMORY: usize = 512;

/// `<account_dir>/hooks-state.json`.
pub fn state_path(account: &str) -> PathBuf {
    crate::config::account_dir(account).join("hooks-state.json")
}

/// Every hook of one account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HooksState {
    #[serde(default)]
    pub hooks: BTreeMap<String, HookCursor>,
}

/// One hook's cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookCursor {
    /// The store key of the mailbox the cursor counts in.
    pub mailbox: String,
    /// The UIDVALIDITY the UIDs below were taken under.
    pub uidvalidity: Option<i64>,
    /// Every UID at or below this one has been considered.
    pub last_uid: i64,
    /// UIDs above `last_uid` already considered, held while a lower arrival is
    /// still owed (the store's arrival mark).
    #[serde(default)]
    pub considered: Vec<i64>,
    /// The Message-IDs this hook fired for, newest last.
    #[serde(default)]
    pub fired: Vec<String>,
    /// When the cursor was taken: nothing that arrived before it fires.
    #[serde(default)]
    pub armed_at: String,
    #[serde(default)]
    pub last_run: Option<LastRun>,
}

/// What the most recent run of a hook did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRun {
    pub at: String,
    pub message_id: String,
    pub uid: i64,
    /// `exit 0`, `exit 3`, `timed out after 60s`, `did not start: ...`.
    pub outcome: String,
    pub ok: bool,
    pub duration_ms: u64,
}

impl HookCursor {
    /// Remember a fired Message-ID, forgetting the oldest past the bound.
    pub fn remember_fired(&mut self, message_id: &str) {
        self.fired.retain(|known| known != message_id);
        self.fired.push(message_id.to_string());
        if self.fired.len() > FIRED_MEMORY {
            let excess = self.fired.len() - FIRED_MEMORY;
            self.fired.drain(..excess);
        }
    }

    pub fn has_fired(&self, message_id: &str) -> bool {
        self.fired.iter().any(|known| known == message_id)
    }

    /// Move `last_uid` up over what has been considered, but not past
    /// `ceiling` (the arrival mark: a UID above it may still arrive below a
    /// considered one), and forget the considered UIDs it passed.
    pub fn advance(&mut self, ceiling: Option<i64>) {
        let Some(&top) = self.considered.iter().max() else {
            return;
        };
        let target = ceiling.map_or(top, |ceiling| top.min(ceiling));
        if target > self.last_uid {
            self.last_uid = target;
        }
        let last = self.last_uid;
        self.considered.retain(|uid| *uid > last);
        self.considered.sort_unstable();
        self.considered.dedup();
    }
}

/// Read the state file, an absent one being empty. A file that does not parse
/// is an error rather than an empty state: starting over would re-arm every
/// hook, and the operator should see why.
pub fn load(path: &Path) -> Result<HooksState> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a hooks state file", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HooksState::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write the state file atomically, private to the user.
pub fn save(path: &Path, state: &HooksState) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let parent = path
        .parent()
        .context("the hooks state file has no parent directory")?;
    crate::config::create_private_dir_all(parent)
        .with_context(|| format!("creating {}", parent.display()))?;
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(state).context("serialising the hooks state")?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("opening {}", tmp.display()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_stops_at_the_arrival_mark_and_keeps_what_it_did_not_pass() {
        let mut cursor = HookCursor {
            last_uid: 10,
            considered: vec![12, 11, 15],
            ..HookCursor::default()
        };
        cursor.advance(Some(12));
        assert_eq!(cursor.last_uid, 12);
        assert_eq!(cursor.considered, vec![15]);
        cursor.advance(None);
        assert_eq!(cursor.last_uid, 15);
        assert!(cursor.considered.is_empty());
    }

    #[test]
    fn fired_ids_are_bounded_and_newest_last() {
        let mut cursor = HookCursor::default();
        for n in 0..FIRED_MEMORY + 3 {
            cursor.remember_fired(&format!("<{n}@x>"));
        }
        assert_eq!(cursor.fired.len(), FIRED_MEMORY);
        assert!(!cursor.has_fired("<0@x>"));
        assert!(cursor.has_fired(&format!("<{}@x>", FIRED_MEMORY + 2)));
    }

    #[test]
    fn a_saved_state_loads_back_and_an_absent_one_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("hooks-state.json");
        assert_eq!(load(&path).unwrap(), HooksState::default());
        let mut state = HooksState::default();
        state.hooks.insert(
            "h".to_string(),
            HookCursor {
                mailbox: "inbox".to_string(),
                uidvalidity: Some(7),
                last_uid: 42,
                ..HookCursor::default()
            },
        );
        save(&path, &state).unwrap();
        assert_eq!(load(&path).unwrap(), state);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
