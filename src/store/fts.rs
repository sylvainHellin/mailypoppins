//! The two full-text indexes over a message's subject, sender and body, and
//! the backfill that fills the second one in a store an older build wrote
//! (PERSO-276).
//!
//! - [`WORD_TABLE`] (`messages_fts`, the default `unicode61` tokenizer) holds
//!   whole words, folded for case and accents: it answers a whole word, a word
//!   prefix and a phrase of words.
//! - [`TRIGRAM_TABLE`] (`messages_fts_tri`, the `trigram` tokenizer with
//!   `case_sensitive 0 remove_diacritics 1`) holds every run of three
//!   characters, so it answers a substring of three characters or more
//!   anywhere in a word, whatever the case: `sanierung` and `DACH` both find
//!   "Dachsanierung". It cannot answer a shorter needle, which is why the word
//!   index stays beside it.
//!
//! Both are contentless with `contentless_delete=1` (see [`super::schema`]),
//! and every writer goes through [`index`] and [`unindex`], so the two never
//! disagree about which rows they hold: ingest indexes a row in the
//! transaction that writes it, and every delete path unindexes it in the
//! transaction that removes it.
//!
//! The trigram table is additive ([`super::schema::ensure_additive_tables`]),
//! so a store an older build wrote gains it empty on its next open, with no
//! version bump and no resync. From then on every write fills it, and
//! [`backfill_trigram`] indexes the rows written before, in batches from
//! `messages` and the body blobs, until the [`META_TRIGRAM_COMPLETE`] marker
//! says it holds every row. A fresh store is stamped complete at creation.
//! Until the marker is there, [`trigram_ready`] is false and the search keeps
//! to the word index, as it answered before the trigram index existed.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use super::blobs::BlobStore;
use super::schema::{clear_meta, get_meta, set_meta};
use super::Store;

/// The word index.
pub const WORD_TABLE: &str = "messages_fts";

/// The trigram index.
pub const TRIGRAM_TABLE: &str = "messages_fts_tri";

/// `meta` key present once [`TRIGRAM_TABLE`] holds every message row: stamped
/// at creation on a fresh store, by the last [`backfill_trigram`] batch on an
/// older one.
pub const META_TRIGRAM_COMPLETE: &str = "fts_trigram_complete";

/// `meta` key holding the highest `messages.id` [`backfill_trigram`] has
/// indexed, so a restarted daemon resumes rather than starts over. Cleared
/// with the completion stamp.
pub const META_TRIGRAM_CURSOR: &str = "fts_trigram_cursor";

/// How many rows one [`backfill_trigram`] batch reads at most.
pub const BACKFILL_ROWS: usize = 200;

/// How many bytes of body text one [`backfill_trigram`] batch indexes before
/// it stops early: the trigram insert is the slow part (about 11 s for 43 MB
/// of text on the TUM store), and the batch holds the write lock while it
/// inserts, so a batch is bounded to a fraction of a second.
pub const BACKFILL_BYTES: usize = 512 * 1024;

/// The `CREATE` statement of [`TRIGRAM_TABLE`], for
/// [`super::schema::ensure_additive_tables`].
pub fn trigram_create_sql() -> String {
    format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {TRIGRAM_TABLE} USING fts5(
            subject,
            from_,
            body_text,
            content='',
            contentless_delete=1,
            tokenize='trigram case_sensitive 0 remove_diacritics 1'
        )"
    )
}

/// Index one row's text in both indexes. The caller has removed any previous
/// entry ([`unindex`]) in the same transaction.
pub fn index(conn: &Connection, rowid: i64, subject: &str, from: &str, body: &str) -> Result<()> {
    for table in [WORD_TABLE, TRIGRAM_TABLE] {
        conn.execute(
            &format!(
                "INSERT INTO {table} (rowid, subject, from_, body_text) VALUES (?1, ?2, ?3, ?4)"
            ),
            rusqlite::params![rowid, subject, from, body],
        )
        .with_context(|| format!("inserting the {table} entry"))?;
    }
    Ok(())
}

/// Remove one row's entries from both indexes. A contentless-delete index
/// undoes an entry from its rowid alone, and a rowid with no entry is a no-op.
pub fn unindex(conn: &Connection, rowid: i64) -> Result<()> {
    for table in [WORD_TABLE, TRIGRAM_TABLE] {
        conn.execute(&format!("DELETE FROM {table} WHERE rowid = ?1"), [rowid])
            .with_context(|| format!("removing the {table} entry"))?;
    }
    Ok(())
}

/// Whether [`TRIGRAM_TABLE`] holds every row, so the search may answer from
/// it. One `meta` lookup by key.
pub fn trigram_ready(conn: &Connection) -> Result<bool> {
    Ok(get_meta(conn, META_TRIGRAM_COMPLETE)?.is_some())
}

/// What one [`backfill_trigram`] batch did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backfill {
    /// Rows the batch indexed.
    pub indexed: usize,
    /// Whether the trigram index now holds every row.
    pub complete: bool,
}

/// One row as the backfill read it, and the body text its blob holds (empty
/// until read).
struct Pending {
    id: i64,
    subject: Option<String>,
    from: Option<String>,
    body_blob: Option<String>,
    body: String,
}

/// Index one batch of the rows an older build wrote into [`TRIGRAM_TABLE`],
/// in `messages.id` order from the persisted cursor, and stamp the index
/// complete once no row is left.
///
/// The rows and their bodies are read outside the write transaction, so the
/// write lock is held only for the inserts. A row is indexed only if it is
/// still what was read: one that a re-ingest rewrote or a delete removed in
/// between was indexed or unindexed in both tables by that writer, which is
/// also why the rows written after the table was created need no backfill. A
/// row whose body blob is gone (evicted by the retention sweep) or unreadable
/// is indexed with its subject and sender alone; the word index keeps the
/// entry ingest wrote, and the search answers a substring term from either
/// index ([`crate::search::FtsEither`]), so that body stays findable by whole
/// word, not by substring.
///
/// At most [`BACKFILL_ROWS`] rows, and fewer once [`BACKFILL_BYTES`] of body
/// text is read. A store already complete answers at once with nothing done.
pub fn backfill_trigram(store: &Store, blobs: &BlobStore) -> Result<Backfill> {
    backfill_trigram_bounded(store, blobs, BACKFILL_ROWS, BACKFILL_BYTES)
}

/// [`backfill_trigram`] with its bounds, so a test can take small batches.
pub fn backfill_trigram_bounded(
    store: &Store,
    blobs: &BlobStore,
    max_rows: usize,
    max_bytes: usize,
) -> Result<Backfill> {
    let conn = store.conn();
    if trigram_ready(conn)? {
        return Ok(Backfill {
            indexed: 0,
            complete: true,
        });
    }
    let cursor: i64 = get_meta(conn, META_TRIGRAM_CURSOR)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let listed: Vec<Pending> = {
        let mut stmt = conn.prepare(
            "SELECT id, subject, from_, body_blob FROM messages
             WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![cursor, max_rows as i64], |row| {
            Ok(Pending {
                id: row.get(0)?,
                subject: row.get(1)?,
                from: row.get(2)?,
                body_blob: row.get(3)?,
                body: String::new(),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("reading the rows owed a trigram entry")?
    };
    let listed_all = listed.len() < max_rows;
    let listed_count = listed.len();

    let mut batch: Vec<Pending> = Vec::with_capacity(listed_count);
    let mut bytes = 0usize;
    for mut row in listed {
        row.body = row
            .body_blob
            .as_deref()
            .and_then(|hash| super::read::read_blob(blobs, row.id, hash))
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        bytes += row.body.len();
        batch.push(row);
        if bytes >= max_bytes {
            break;
        }
    }
    let complete = listed_all && batch.len() == listed_count;

    let tx = store
        .immediate_transaction()
        .context("opening a trigram backfill transaction")?;
    let mut indexed = 0;
    for row in &batch {
        let unchanged = tx
            .query_row(
                "SELECT 1 FROM messages
                 WHERE id = ?1 AND subject IS ?2 AND from_ IS ?3 AND body_blob IS ?4",
                rusqlite::params![row.id, row.subject, row.from, row.body_blob],
                |_| Ok(()),
            )
            .optional()
            .context("checking a row the backfill read")?
            .is_some();
        if !unchanged {
            continue;
        }
        tx.execute(
            &format!("DELETE FROM {TRIGRAM_TABLE} WHERE rowid = ?1"),
            [row.id],
        )
        .context("removing a previous trigram entry")?;
        tx.execute(
            &format!(
                "INSERT INTO {TRIGRAM_TABLE} (rowid, subject, from_, body_text) \
                 VALUES (?1, ?2, ?3, ?4)"
            ),
            rusqlite::params![
                row.id,
                row.subject.as_deref().unwrap_or_default(),
                row.from.as_deref().unwrap_or_default(),
                row.body
            ],
        )
        .context("inserting a trigram entry")?;
        indexed += 1;
    }
    if complete {
        set_meta(&tx, META_TRIGRAM_COMPLETE, "1")?;
        clear_meta(&tx, META_TRIGRAM_CURSOR)?;
    } else if let Some(last) = batch.last() {
        set_meta(&tx, META_TRIGRAM_CURSOR, &last.id.to_string())?;
    }
    tx.commit()
        .context("committing a trigram backfill transaction")?;
    Ok(Backfill { indexed, complete })
}
