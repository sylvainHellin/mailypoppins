//! The `graph_draft_versions` table: the key the drafts mirror knows each
//! version of a Graph draft by (#0143 slice 3, PERSO-182).
//!
//! The mirror's push, reconcile and send guard speak IMAP UIDs: a `u32` that
//! names one copy, that a newer copy outranks, and that changes whenever the
//! copy's content does ([`crate::draft_mirror::folder::DraftsFolder`]). A
//! Graph draft is an immutable id instead, and Outlook edits it in place,
//! keeping the id and changing only its `changeKey`. So each
//! `(immutable id, changeKey)` pair the mirror sees gets a key here, minted
//! in increasing order and never reused (`AUTOINCREMENT`): an edit anywhere
//! is a new, higher key, and a version no listing shows any more is a key
//! gone from the folder, exactly as an expunged UID is.
//!
//! It is a cache table like `draft_sync`, which names its keys: a store
//! rebuild drops both, and the reconcile re-derives every row from the copy
//! the draft's `server_message_id` names.

use anyhow::{anyhow, Context, Result};
use rusqlite::OptionalExtension;

use super::Store;

/// The table's name.
pub const TABLE: &str = "graph_draft_versions";

/// Its `CREATE` statement, run by
/// [`ensure_additive_tables`](super::schema::ensure_additive_tables).
pub fn create_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {TABLE} (
            key        INTEGER PRIMARY KEY AUTOINCREMENT,
            account    TEXT NOT NULL,
            graph_id   TEXT NOT NULL,
            change_key TEXT NOT NULL,
            item_key   INTEGER,
            UNIQUE (account, graph_id, change_key)
        )"
    )
}

/// One version of one Graph draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub key: u32,
    /// The draft's immutable id.
    pub graph_id: String,
    pub change_key: String,
    /// The key of the first version of this draft the mirror saw, which every
    /// later version repeats and a prune keeps: it orders the drafts by when
    /// they appeared, whatever was edited since.
    pub item_key: u32,
}

/// The key of the version `change_key` of the draft `graph_id`, minted when
/// it is new, above every key minted before; a new version of a draft seen
/// before repeats that draft's [`Version::item_key`].
pub fn key_of(store: &Store, account: &str, graph_id: &str, change_key: &str) -> Result<u32> {
    let conn = store.conn();
    let inserted = conn
        .execute(
            &format!(
                "INSERT INTO {TABLE} (account, graph_id, change_key) VALUES (?1, ?2, ?3)
                 ON CONFLICT (account, graph_id, change_key) DO NOTHING"
            ),
            [account, graph_id, change_key],
        )
        .context("recording a Graph draft version")?;
    if inserted > 0 {
        conn.execute(
            &format!(
                "UPDATE {TABLE} SET item_key = COALESCE(
                    (SELECT MIN(item_key) FROM {TABLE}
                     WHERE account = ?1 AND graph_id = ?2 AND item_key IS NOT NULL),
                    key)
                 WHERE key = last_insert_rowid()"
            ),
            [account, graph_id],
        )
        .context("recording a Graph draft's first version")?;
    }
    let key: i64 = conn
        .query_row(
            &format!(
                "SELECT key FROM {TABLE} WHERE account = ?1 AND graph_id = ?2 AND change_key = ?3"
            ),
            [account, graph_id, change_key],
            |row| row.get(0),
        )
        .context("reading a Graph draft version")?;
    u32::try_from(key).map_err(|_| anyhow!("the Graph draft version key {key} is past u32"))
}

/// The version `key` names, `None` when it names none.
pub fn version(store: &Store, account: &str, key: u32) -> Result<Option<Version>> {
    store
        .conn()
        .query_row(
            &format!(
                "SELECT graph_id, change_key, COALESCE(item_key, key) FROM {TABLE}
                 WHERE account = ?1 AND key = ?2"
            ),
            rusqlite::params![account, i64::from(key)],
            |row| {
                Ok(Version {
                    key,
                    graph_id: row.get(0)?,
                    change_key: row.get(1)?,
                    item_key: row.get::<_, i64>(2)? as u32,
                })
            },
        )
        .optional()
        .context("reading a Graph draft version")
}

/// The highest key of the draft `graph_id`: the version the mirror saw last.
pub fn latest_key(store: &Store, account: &str, graph_id: &str) -> Result<Option<u32>> {
    let key: Option<i64> = store
        .conn()
        .query_row(
            &format!("SELECT MAX(key) FROM {TABLE} WHERE account = ?1 AND graph_id = ?2"),
            [account, graph_id],
            |row| row.get(0),
        )
        .context("reading the latest Graph draft version")?;
    Ok(key.and_then(|key| u32::try_from(key).ok()))
}

/// Forget every version of `account` but those `keep` names and those a
/// `draft_sync` row names, after a complete listing of the folder: a version
/// the listing did not show is gone from it, and its key is never minted
/// again.
pub fn prune(store: &Store, account: &str, keep: &[u32]) -> Result<usize> {
    let conn = store.conn();
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS graph_draft_keep (key INTEGER PRIMARY KEY)",
    )
    .context("preparing the Graph draft version prune")?;
    conn.execute("DELETE FROM temp.graph_draft_keep", [])?;
    for key in keep {
        conn.execute(
            "INSERT OR IGNORE INTO temp.graph_draft_keep (key) VALUES (?1)",
            [i64::from(*key)],
        )?;
    }
    let pruned = conn
        .execute(
            &format!(
                "DELETE FROM {TABLE} WHERE account = ?1
                 AND key NOT IN (SELECT key FROM temp.graph_draft_keep)
                 AND key NOT IN (SELECT uid FROM {sync} WHERE account = ?1)",
                sync = super::draft_sync::TABLE
            ),
            [account],
        )
        .context("pruning the Graph draft versions")?;
    conn.execute("DELETE FROM temp.graph_draft_keep", [])?;
    Ok(pruned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::draft_sync::{self, DraftSyncRow};

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        (dir, store)
    }

    /// A version keeps its key; an edit is a new, higher key; a key names
    /// its version back, and the latest key of a draft is its last version.
    #[test]
    fn each_version_gets_one_key_and_an_edit_a_higher_one() {
        let (_dir, store) = store();
        let first = key_of(&store, "evoqs", "AAMk-1", "ck1").unwrap();
        assert_eq!(key_of(&store, "evoqs", "AAMk-1", "ck1").unwrap(), first);
        let other = key_of(&store, "evoqs", "AAMk-2", "ck1").unwrap();
        let edited = key_of(&store, "evoqs", "AAMk-1", "ck2").unwrap();
        assert!(first < other && other < edited);
        assert_eq!(
            version(&store, "evoqs", edited).unwrap(),
            Some(Version {
                key: edited,
                graph_id: "AAMk-1".to_string(),
                change_key: "ck2".to_string(),
                item_key: first,
            })
        );
        assert_eq!(version(&store, "tum", edited).unwrap(), None);
        assert_eq!(latest_key(&store, "evoqs", "AAMk-1").unwrap(), Some(edited));
        assert_eq!(latest_key(&store, "evoqs", "AAMk-9").unwrap(), None);
    }

    /// A prune keeps what the listing showed and what a row names, and a
    /// pruned version that comes back gets a key it never had.
    #[test]
    fn a_prune_keeps_the_listed_and_the_named_and_never_reuses_a_key() {
        let (_dir, store) = store();
        let named = key_of(&store, "evoqs", "AAMk-1", "ck1").unwrap();
        let listed = key_of(&store, "evoqs", "AAMk-2", "ck1").unwrap();
        let gone = key_of(&store, "evoqs", "AAMk-3", "ck1").unwrap();
        let elsewhere = key_of(&store, "tum", "AAMk-3", "ck1").unwrap();
        draft_sync::upsert(
            &store,
            &DraftSyncRow {
                account: "evoqs".to_string(),
                draft_id: "d1".to_string(),
                mailbox: "drafts".to_string(),
                uid: named,
                uidvalidity: 1,
                synced_at: 0,
                synced_hash: String::new(),
                message_id: None,
            },
        )
        .unwrap();
        assert_eq!(prune(&store, "evoqs", &[listed]).unwrap(), 1);
        assert!(version(&store, "evoqs", named).unwrap().is_some());
        assert!(version(&store, "evoqs", listed).unwrap().is_some());
        assert!(version(&store, "evoqs", gone).unwrap().is_none());
        assert!(version(&store, "tum", elsewhere).unwrap().is_some());
        let back = key_of(&store, "evoqs", "AAMk-3", "ck1").unwrap();
        assert!(back > elsewhere, "{back} is a fresh key");

        // A version of a draft whose first version was pruned keeps the
        // draft's first key.
        let edited = key_of(&store, "evoqs", "AAMk-2", "ck2").unwrap();
        assert_eq!(prune(&store, "evoqs", &[edited]).unwrap(), 2);
        let again = key_of(&store, "evoqs", "AAMk-2", "ck3").unwrap();
        assert_eq!(
            version(&store, "evoqs", again).unwrap().unwrap().item_key,
            listed
        );
    }
}
