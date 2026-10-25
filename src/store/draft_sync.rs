//! The `draft_sync` table: where each mirrored draft's server copy is, and
//! the bytes of the file it last converged with (#0143).
//!
//! One row per `(account, draft_id)`. The draft file is the truth and the
//! server copy is its mirror; this table only says which copy the mirror is
//! (`mailbox`, `uid`, `uidvalidity`, `message_id`) and what the file looked
//! like when the two last agreed (`synced_hash`, `synced_at`). A file whose
//! hash differs from its row changed locally since; a copy whose UID differs
//! from its row changed on the server since.
//!
//! `message_id` repeats the draft's `server_message_id` so a file rewritten
//! without that key gets it back from here (PERSO-192): without it, the
//! mirror would mint a new one, read the row's copy as gone, and retire the
//! draft as sent elsewhere.
//!
//! It is a cache table like `pending_ops`: a store rebuild drops it, and the
//! reconcile re-derives each row from the server copy the draft's
//! `server_message_id` names and the file ([`crate::draft_mirror`]). Nothing
//! in it is the only copy of anything the user typed.
//!
//! The volatile state lives here rather than in the frontmatter because every
//! push replaces the server copy and changes its UID: written into the file,
//! it would rewrite a draft an editor holds open a few seconds after every
//! save.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::Store;

/// The table's name.
pub const TABLE: &str = "draft_sync";

/// Its `CREATE` statement, run by
/// [`ensure_additive_tables`](super::schema::ensure_additive_tables).
pub fn create_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {TABLE} (
            account     TEXT NOT NULL,
            draft_id    TEXT NOT NULL,
            mailbox     TEXT NOT NULL,
            uid         INTEGER NOT NULL,
            uidvalidity INTEGER NOT NULL,
            synced_at   INTEGER NOT NULL,
            synced_hash TEXT NOT NULL,
            message_id  TEXT,
            PRIMARY KEY (account, draft_id)
        )"
    )
}

/// Add the `message_id` column to a table an earlier build created without
/// it. A read when nothing is missing, as [`create_sql`]'s caller requires;
/// the rows it leaves `NULL` are filled by the next reconcile.
pub fn ensure_columns(conn: &rusqlite::Connection) -> Result<()> {
    let has_message_id: bool = conn
        .query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM pragma_table_info('{TABLE}') WHERE name = 'message_id')"
            ),
            [],
            |row| row.get(0),
        )
        .context("reading the draft_sync columns")?;
    if !has_message_id {
        conn.execute_batch(&format!("ALTER TABLE {TABLE} ADD COLUMN message_id TEXT"))
            .context("adding the draft_sync message_id column")?;
    }
    Ok(())
}

/// One draft's server copy and the convergence it was recorded at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftSyncRow {
    pub account: String,
    /// The draft's `id:`.
    pub draft_id: String,
    /// The server folder the copy lives in.
    pub mailbox: String,
    /// The copy's UID, meaningful only beside `uidvalidity`.
    pub uid: u32,
    /// The folder's UIDVALIDITY when `uid` was recorded.
    pub uidvalidity: u32,
    /// Unix seconds of the push or pull that converged.
    pub synced_at: i64,
    /// [`crate::draft_mirror::file_hash`] of the file's bytes at that moment.
    pub synced_hash: String,
    /// The copy's `Message-ID`, as the draft's `server_message_id` spells it;
    /// `None` on a row an earlier build wrote, until the reconcile fills it.
    pub message_id: Option<String>,
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<DraftSyncRow> {
    Ok(DraftSyncRow {
        account: row.get(0)?,
        draft_id: row.get(1)?,
        mailbox: row.get(2)?,
        uid: row.get::<_, i64>(3)? as u32,
        uidvalidity: row.get::<_, i64>(4)? as u32,
        synced_at: row.get(5)?,
        synced_hash: row.get(6)?,
        message_id: row.get(7)?,
    })
}

const COLUMNS: &str =
    "account, draft_id, mailbox, uid, uidvalidity, synced_at, synced_hash, message_id";

/// Every row of `account`, by draft id.
pub fn load(store: &Store, account: &str) -> Result<Vec<DraftSyncRow>> {
    let mut stmt = store.conn().prepare(&format!(
        "SELECT {COLUMNS} FROM {TABLE} WHERE account = ?1 ORDER BY draft_id"
    ))?;
    let rows = stmt
        .query_map([account], row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("reading the draft_sync rows")?;
    Ok(rows)
}

/// The row of one draft, if it has one.
pub fn get(store: &Store, account: &str, draft_id: &str) -> Result<Option<DraftSyncRow>> {
    store
        .conn()
        .query_row(
            &format!("SELECT {COLUMNS} FROM {TABLE} WHERE account = ?1 AND draft_id = ?2"),
            [account, draft_id],
            row_from_sql,
        )
        .optional()
        .context("reading a draft_sync row")
}

/// Write one draft's row, replacing the one it had.
pub fn upsert(store: &Store, row: &DraftSyncRow) -> Result<()> {
    store
        .conn()
        .execute(
            &format!(
                "INSERT INTO {TABLE} ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (account, draft_id) DO UPDATE SET
                    mailbox = excluded.mailbox, uid = excluded.uid,
                    uidvalidity = excluded.uidvalidity, synced_at = excluded.synced_at,
                    synced_hash = excluded.synced_hash, message_id = excluded.message_id"
            ),
            rusqlite::params![
                row.account,
                row.draft_id,
                row.mailbox,
                i64::from(row.uid),
                i64::from(row.uidvalidity),
                row.synced_at,
                row.synced_hash,
                row.message_id,
            ],
        )
        .context("writing a draft_sync row")?;
    Ok(())
}

/// Forget one draft's row: it was retired, or its copy is gone.
pub fn delete(store: &Store, account: &str, draft_id: &str) -> Result<()> {
    store
        .conn()
        .execute(
            &format!("DELETE FROM {TABLE} WHERE account = ?1 AND draft_id = ?2"),
            [account, draft_id],
        )
        .context("deleting a draft_sync row")?;
    Ok(())
}

/// The absences the last reconcile saw (#0143): a draft whose server copy was
/// missing from the listing, or a row whose file was missing from `drafts/`.
/// A draft is retired, or a hand-removed draft's copy deleted, only when two
/// reconciles in a row see the same absence under the same UIDVALIDITY, so
/// one short listing or an editor's save-by-rename retires nothing.
pub const ABSENCE_TABLE: &str = "draft_absence";

/// Its `CREATE` statement, run beside [`create_sql`].
pub fn absence_create_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {ABSENCE_TABLE} (
            account     TEXT NOT NULL,
            draft_id    TEXT NOT NULL,
            kind        TEXT NOT NULL,
            uidvalidity INTEGER NOT NULL,
            PRIMARY KEY (account, draft_id)
        )"
    )
}

/// Record that this reconcile saw the absence `kind` of `draft_id` under
/// `uidvalidity`. `true` when the previous reconcile saw the same one, which
/// is when the absence is acted on.
pub fn note_absent(
    store: &Store,
    account: &str,
    draft_id: &str,
    kind: &str,
    uidvalidity: u32,
) -> Result<bool> {
    let seen: Option<(String, i64)> = store
        .conn()
        .query_row(
            &format!(
                "SELECT kind, uidvalidity FROM {ABSENCE_TABLE}
                 WHERE account = ?1 AND draft_id = ?2"
            ),
            [account, draft_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("reading a draft absence")?;
    if seen.is_some_and(|(k, v)| k == kind && v == i64::from(uidvalidity)) {
        return Ok(true);
    }
    store
        .conn()
        .execute(
            &format!(
                "INSERT INTO {ABSENCE_TABLE} (account, draft_id, kind, uidvalidity)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (account, draft_id) DO UPDATE SET
                    kind = excluded.kind, uidvalidity = excluded.uidvalidity"
            ),
            rusqlite::params![account, draft_id, kind, i64::from(uidvalidity)],
        )
        .context("recording a draft absence")?;
    Ok(false)
}

/// Forget every absence of `account` but the ones this reconcile saw, which
/// is what makes two sightings consecutive.
pub fn keep_absences(
    store: &Store,
    account: &str,
    seen: &std::collections::HashSet<String>,
) -> Result<()> {
    let ids: Vec<String> = {
        let mut stmt = store.conn().prepare(&format!(
            "SELECT draft_id FROM {ABSENCE_TABLE} WHERE account = ?1"
        ))?;
        let rows = stmt
            .query_map([account], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        rows
    };
    for id in ids.iter().filter(|id| !seen.contains(*id)) {
        store
            .conn()
            .execute(
                &format!("DELETE FROM {ABSENCE_TABLE} WHERE account = ?1 AND draft_id = ?2"),
                [account, id.as_str()],
            )
            .context("forgetting a draft absence")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        (dir, store)
    }

    fn row(id: &str, uid: u32) -> DraftSyncRow {
        DraftSyncRow {
            account: "tum".to_string(),
            draft_id: id.to_string(),
            mailbox: "Drafts".to_string(),
            uid,
            uidvalidity: 7,
            synced_at: 1_700_000_000,
            synced_hash: format!("hash-{uid}"),
            message_id: Some(format!("<mp-draft.{uid}@tum.de>")),
        }
    }

    /// A fresh store has the table, a row round-trips, a second write
    /// replaces it, and a delete forgets it, per account.
    #[test]
    fn a_row_round_trips_is_replaced_and_is_deleted() {
        let (_dir, store) = store();
        assert!(load(&store, "tum").unwrap().is_empty());
        upsert(&store, &row("a", 10)).unwrap();
        upsert(&store, &row("b", 11)).unwrap();
        upsert(&store, &row("a", 12)).unwrap();
        let mut other = row("a", 1);
        other.account = "proton".to_string();
        upsert(&store, &other).unwrap();

        assert_eq!(
            load(&store, "tum").unwrap(),
            vec![row("a", 12), row("b", 11)]
        );
        assert_eq!(get(&store, "tum", "a").unwrap(), Some(row("a", 12)));
        delete(&store, "tum", "a").unwrap();
        assert_eq!(get(&store, "tum", "a").unwrap(), None);
        assert_eq!(load(&store, "proton").unwrap(), vec![other]);
    }

    /// A UID or UIDVALIDITY above `i32::MAX` survives the round trip, since
    /// both are unsigned 32-bit on the wire.
    #[test]
    fn a_large_uid_round_trips() {
        let (_dir, store) = store();
        let mut big = row("a", u32::MAX);
        big.uidvalidity = u32::MAX - 1;
        upsert(&store, &big).unwrap();
        assert_eq!(get(&store, "tum", "a").unwrap(), Some(big));
    }

    /// A table an earlier build created without `message_id` gains the
    /// column at the next open, its rows reading `None` until a write fills
    /// it (PERSO-192).
    #[test]
    fn a_table_without_the_message_id_gains_it_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite3");
        {
            let store = Store::open(&path).unwrap();
            store
                .conn()
                .execute_batch(&format!(
                    "DROP TABLE {TABLE};
                     CREATE TABLE {TABLE} (
                        account TEXT NOT NULL, draft_id TEXT NOT NULL,
                        mailbox TEXT NOT NULL, uid INTEGER NOT NULL,
                        uidvalidity INTEGER NOT NULL, synced_at INTEGER NOT NULL,
                        synced_hash TEXT NOT NULL, PRIMARY KEY (account, draft_id));
                     INSERT INTO {TABLE} VALUES ('tum', 'a', 'Drafts', 10, 7, 1, 'h');"
                ))
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let old = get(&store, "tum", "a").unwrap().expect("the old row");
        assert_eq!(old.message_id, None);
        upsert(&store, &row("a", 10)).unwrap();
        assert_eq!(get(&store, "tum", "a").unwrap(), Some(row("a", 10)));
    }
}
