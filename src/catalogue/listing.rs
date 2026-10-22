//! What the server last listed, per account, in the store's `mailbox_listing`
//! table (#0141).
//!
//! Raw facts only: the name `SELECT` takes, the hierarchy delimiter, the
//! `LIST` attributes, the counts a `STATUS` or Graph's folder list reported,
//! and whether the last listing still had the folder. Role and policy are
//! derived at read time by [`super::Catalogue::resolve`], so a config edit
//! takes effect without asking the server again.
//!
//! A folder the server stops listing is not deleted: it is marked `missing`,
//! its rows stay in the store, and it is neither synced nor shown until the
//! server lists it again (pitfall 9). The table is additive, a cache like the
//! rest of the store: a rebuild loses it and the next tick lists again.

use anyhow::{Context, Result};
use rusqlite::params;

use crate::store::Store;

/// The table, created by [`crate::store::schema::ensure_additive_tables`].
pub const TABLE: &str = "mailbox_listing";

/// Its `CREATE` statement.
pub fn create_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {TABLE} (
            account    TEXT NOT NULL,
            name       TEXT NOT NULL,
            delimiter  TEXT,
            attributes TEXT NOT NULL DEFAULT '[]',
            position   INTEGER NOT NULL,
            missing    INTEGER NOT NULL DEFAULT 0,
            total      INTEGER,
            unread     INTEGER,
            PRIMARY KEY (account, name)
        )"
    )
}

/// One folder as the server listed it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListedFolder {
    /// The name as the server spells it, which is what `SELECT` takes.
    pub name: String,
    /// The hierarchy delimiter, `None` for a flat namespace.
    pub delimiter: Option<String>,
    /// The `LIST` attributes as `\Name` tokens; Graph's well-known folders
    /// carry the RFC 6154 attribute of their role.
    pub attributes: Vec<String>,
    /// Listed once and not by the last listing.
    pub missing: bool,
    /// The server's message count, when one was asked for.
    pub total: Option<u64>,
    /// The server's unread count, when one was asked for.
    pub unread: Option<u64>,
}

impl ListedFolder {
    /// A folder with a name and nothing else, which is what a test or a pin
    /// starts from.
    pub fn named(name: &str) -> Self {
        ListedFolder {
            name: name.to_string(),
            delimiter: Some("/".to_string()),
            ..Default::default()
        }
    }

    /// The same folder with `attributes`.
    pub fn with_attributes(mut self, attributes: &[&str]) -> Self {
        self.attributes = attributes.iter().map(|a| a.to_string()).collect();
        self
    }
}

/// Every folder of `account` the store holds, in the order the server last
/// listed them, the missing ones after.
pub fn load(store: &Store, account: &str) -> Result<Vec<ListedFolder>> {
    let mut stmt = store
        .conn()
        .prepare(&format!(
            "SELECT name, delimiter, attributes, missing, total, unread FROM {TABLE}
             WHERE account = ?1 ORDER BY missing, position"
        ))
        .context("reading the folder list")?;
    let rows = stmt.query_map([account], |row| {
        let attributes: String = row.get(2)?;
        Ok(ListedFolder {
            name: row.get(0)?,
            delimiter: row.get(1)?,
            attributes: serde_json::from_str(&attributes).unwrap_or_default(),
            missing: row.get::<_, i64>(3)? != 0,
            total: row.get::<_, Option<i64>>(4)?.map(|n| n.max(0) as u64),
            unread: row.get::<_, Option<i64>>(5)?.map(|n| n.max(0) as u64),
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("reading the folder list")
}

/// Record a fresh listing, keeping the counts a folder had unless the listing
/// carries its own, and marking every folder it no longer has as missing.
///
/// Answers whether the set of present folders changed, which is what tells
/// the daemon the sidebar hierarchy has to be re-seeded.
pub fn replace(store: &Store, account: &str, fresh: &[ListedFolder]) -> Result<bool> {
    let previous = load(store, account)?;
    let tx = store.immediate_transaction()?;
    for (position, folder) in fresh.iter().enumerate() {
        let before = previous.iter().find(|p| p.name == folder.name);
        tx.execute(
            &format!(
                "INSERT INTO {TABLE}
                     (account, name, delimiter, attributes, position, missing, total, unread)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7)
                 ON CONFLICT (account, name) DO UPDATE SET
                     delimiter = excluded.delimiter, attributes = excluded.attributes,
                     position = excluded.position, missing = 0,
                     total = excluded.total, unread = excluded.unread"
            ),
            params![
                account,
                folder.name,
                folder.delimiter,
                serde_json::to_string(&folder.attributes)?,
                position as i64,
                folder
                    .total
                    .or(before.and_then(|b| b.total))
                    .map(|n| n as i64),
                folder
                    .unread
                    .or(before.and_then(|b| b.unread))
                    .map(|n| n as i64),
            ],
        )
        .with_context(|| format!("recording the folder {}", folder.name))?;
    }
    let mut gone = 0usize;
    for folder in &previous {
        if fresh.iter().any(|f| f.name == folder.name) {
            continue;
        }
        tx.execute(
            &format!(
                "UPDATE {TABLE} SET missing = 1, position = ?3 WHERE account = ?1 AND name = ?2"
            ),
            params![account, folder.name, (fresh.len() + gone) as i64],
        )
        .with_context(|| format!("marking the folder {} missing", folder.name))?;
        gone += 1;
    }
    tx.commit().context("committing the folder list")?;

    let present = |list: &[ListedFolder]| -> Vec<(String, Vec<String>)> {
        let mut names: Vec<(String, Vec<String>)> = list
            .iter()
            .filter(|f| !f.missing)
            .map(|f| (f.name.clone(), f.attributes.clone()))
            .collect();
        names.sort();
        names
    };
    Ok(present(&previous) != present(fresh))
}

/// Record the server's counts for the folders a `STATUS` (or a Graph folder
/// list) answered, leaving every other folder's alone. Answers whether any
/// count moved.
pub fn record_counts(store: &Store, account: &str, counts: &[(String, u64, u64)]) -> Result<bool> {
    let tx = store.immediate_transaction()?;
    let mut moved = 0;
    for (name, total, unread) in counts {
        moved += tx
            .execute(
                &format!(
                    "UPDATE {TABLE} SET total = ?3, unread = ?4
                     WHERE account = ?1 AND name = ?2
                       AND (total IS NOT ?3 OR unread IS NOT ?4)"
                ),
                params![account, name, *total as i64, *unread as i64],
            )
            .with_context(|| format!("recording the counts of {name}"))?;
    }
    tx.commit().context("committing the folder counts")?;
    Ok(moved > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        (dir, store)
    }

    #[test]
    fn a_listing_round_trips_and_a_vanished_folder_is_kept_as_missing() {
        let (_dir, store) = store();
        let first = vec![
            ListedFolder::named("INBOX"),
            ListedFolder::named("Spesen"),
            ListedFolder::named("Junk Email").with_attributes(&["\\Junk"]),
        ];
        assert!(
            replace(&store, "tum", &first).unwrap(),
            "a first listing is a change"
        );
        assert_eq!(load(&store, "tum").unwrap(), first);
        assert!(
            !replace(&store, "tum", &first).unwrap(),
            "the same listing is not"
        );

        assert!(record_counts(&store, "tum", &[("Junk Email".into(), 12, 3)]).unwrap());
        assert!(!record_counts(&store, "tum", &[("Junk Email".into(), 12, 3)]).unwrap());

        let second = vec![
            ListedFolder::named("INBOX"),
            ListedFolder::named("Junk Email").with_attributes(&["\\Junk"]),
        ];
        assert!(replace(&store, "tum", &second).unwrap());
        let listed = load(&store, "tum").unwrap();
        assert_eq!(listed.len(), 3, "nothing is deleted");
        let spesen = listed.iter().find(|f| f.name == "Spesen").unwrap();
        assert!(spesen.missing);
        let junk = listed.iter().find(|f| f.name == "Junk Email").unwrap();
        assert_eq!(
            (junk.total, junk.unread),
            (Some(12), Some(3)),
            "counts survive a relist"
        );

        assert!(
            replace(&store, "tum", &first).unwrap(),
            "a folder that came back"
        );
        assert!(!load(&store, "tum").unwrap().iter().any(|f| f.missing));
    }
}
