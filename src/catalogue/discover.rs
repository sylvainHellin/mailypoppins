//! Asking the server which folders it has (#0141), and keeping the answer in
//! the store's listing.
//!
//! IMAP issues a plain `LIST "" "*"`. Neither live server advertises
//! SPECIAL-USE (#0143's probe of 2026-10-20), and both mark the role folders in
//! the plain listing anyway, as RFC 6154 servers do in practice, so the
//! `RETURN (SPECIAL-USE)` form would buy nothing the parser does not already
//! read. Graph walks the folder tree and asks for its well-known folders,
//! whose ids stand in for the attributes an IMAP server sends.
//!
//! Both halves are best-effort for their callers: a tick that cannot list
//! syncs what the stored listing already holds, and a listing that never
//! succeeded leaves the config's mailboxes, which is the pre-#0141 behaviour.

use anyhow::Result;

use crate::config::{AccountConfig, AuthMethod, FolderPolicy, GraphConfig, ImapConfig};
use crate::store::Store;

use super::listing::{self, ListedFolder};
use super::Catalogue;

/// Graph's well-known folders and the attribute each one's folder carries in
/// the listing.
const GRAPH_WELL_KNOWN: [(&str, &str); 6] = [
    ("inbox", "\\Inbox"),
    ("archive", "\\Archive"),
    ("sentitems", "\\Sent"),
    ("drafts", "\\Drafts"),
    ("deleteditems", "\\Trash"),
    ("junkemail", "\\Junk"),
];

/// The attribute of the role folder a config name designates on Graph, for a
/// name Graph resolves to a well-known folder (`Sent Items`, `inbox`), `None`
/// for any other name (#0141).
pub(crate) fn graph_role_attribute(name: &str) -> Option<&'static str> {
    let well_known = crate::graph::well_known_folder(name)?;
    GRAPH_WELL_KNOWN
        .iter()
        .find(|(known, _)| *known == well_known)
        .map(|(_, attribute)| *attribute)
}

/// Every folder an IMAP server lists.
pub async fn imap_listing(imap: &ImapConfig) -> Result<Vec<ListedFolder>> {
    Ok(crate::imap_client::list_mailboxes_detailed(imap)
        .await?
        .into_iter()
        .map(|mailbox| ListedFolder {
            name: mailbox.name,
            delimiter: mailbox.delimiter,
            attributes: mailbox.attributes,
            missing: false,
            total: None,
            unread: None,
            id: None,
        })
        .collect())
}

/// Every folder of a Graph mailbox, as `Parent/Child` paths with their counts
/// and the attribute of the role a well-known folder holds.
pub async fn graph_listing(client: &crate::graph::GraphClient) -> Result<Vec<ListedFolder>> {
    let tree = client.list_folder_tree().await?;
    let mut roles: Vec<(String, &'static str)> = Vec::new();
    for (well_known, attribute) in GRAPH_WELL_KNOWN {
        match client.well_known_folder_id(well_known).await {
            Ok(Some(id)) => roles.push((id, attribute)),
            Ok(None) => {}
            // A role the lookup could not read falls back to the folder's
            // name; it is no reason to drop the whole listing.
            Err(e) => log::warn!("[catalogue] the Graph {well_known} folder: {e:#}"),
        }
    }
    Ok(tree
        .into_iter()
        .map(|(path, folder)| ListedFolder {
            attributes: roles
                .iter()
                .filter(|(id, _)| *id == folder.id)
                .map(|(_, attribute)| attribute.to_string())
                .collect(),
            name: path,
            delimiter: Some("/".to_string()),
            missing: false,
            total: Some(folder.total_item_count.max(0) as u64),
            unread: Some(folder.unread_item_count.max(0) as u64),
            id: Some(folder.id),
        })
        .collect())
}

/// What the server lists for `account`, over its own transport.
pub async fn server_listing(account: &AccountConfig) -> Result<Vec<ListedFolder>> {
    if account.auth_method == AuthMethod::Graph {
        let config = GraphConfig::load(account)?;
        let client = crate::graph::GraphClient::new_async(&config).await?;
        return graph_listing(&client).await;
    }
    imap_listing(&ImapConfig::load(account)?).await
}

/// List the server's folders and record them in the account's store.
///
/// Answers whether the set of folders changed, which is what tells the daemon
/// to re-seed the sidebar. Creates the store if the account has none, so it
/// is for a caller that is about to sync anyway.
///
/// A folder the server renamed ([`listing::Rename`], Graph only) takes its
/// rows, its sync cursor and its delta token to the key the new name gives
/// it ([`follow_rename`]), so the next pass carries on where the old name
/// left off instead of downloading the folder again beside its stale rows.
pub async fn refresh(account: &AccountConfig) -> Result<bool> {
    let fresh = server_listing(account).await?;
    let store = Store::open_account(&account.name)?;
    let blobs = crate::store::BlobStore::for_account(&account.name);
    record(&store, &blobs, account, &fresh)
}

/// [`refresh`] once the server has answered: record `fresh` and follow its
/// renames.
fn record(
    store: &Store,
    blobs: &crate::store::BlobStore,
    account: &AccountConfig,
    fresh: &[ListedFolder],
) -> Result<bool> {
    let before = Catalogue::from_store(store, account);
    let recorded = listing::replace(store, &account.name, fresh)?;
    if !recorded.renames.is_empty() {
        let after = Catalogue::from_store(store, account);
        for rename in &recorded.renames {
            follow_rename(store, blobs, &account.name, &before, &after, rename)?;
        }
    }
    Ok(recorded.changed)
}

/// Move a renamed folder's rows from the key its old name gave it to the key
/// its new name gives it (#0141).
///
/// Nothing moves when the key does not change (a role folder is keyed by its
/// role), when config pins the old name (the pin still names it, and moving
/// its rows away would leave the pin empty; the log says to edit the config),
/// or when the new name would shadow a role or the local Drafts.
fn follow_rename(
    store: &Store,
    blobs: &crate::store::BlobStore,
    account: &str,
    before: &Catalogue,
    after: &Catalogue,
    rename: &listing::Rename,
) -> Result<()> {
    let listed_as = |catalogue: &'_ Catalogue, name: &str| -> Option<super::Folder> {
        catalogue
            .folders
            .iter()
            .find(|f| f.listed_as.as_deref() == Some(name))
            .cloned()
    };
    let (Some(old), Some(new)) = (
        listed_as(before, &rename.from),
        listed_as(after, &rename.to),
    ) else {
        return Ok(());
    };
    if old.pin.is_some() {
        log::warn!(
            "[catalogue] {account}: the server renamed '{}' to '{}', which config.toml \
             still pins under the old name; its rows stay until the config names the new one",
            rename.from,
            rename.to
        );
        return Ok(());
    }
    if old.key == new.key || (!new.keyed_by_role() && super::shadows_reserved_key(&new.key)) {
        return Ok(());
    }
    let moved = rekey(store, blobs, account, &old.key, &new.key)?;
    log::info!(
        "[catalogue] {account}: '{}' was renamed '{}' on the server; {moved} row(s) follow it",
        rename.from,
        rename.to
    );
    Ok(())
}

/// Re-key every row of mailbox `from` to `to`: the messages, the sync cursor
/// (and with it the delta token, which stays valid since the folder id did not
/// change), the mailbox record and the ingest failures. A message `to`
/// already holds stays there and its old copy is deleted. Answers how many
/// messages moved.
fn rekey(
    store: &Store,
    blobs: &crate::store::BlobStore,
    account: &str,
    from: &str,
    to: &str,
) -> Result<usize> {
    use anyhow::Context;
    let tx = store.immediate_transaction()?;
    let moved = tx
        .execute(
            "UPDATE OR IGNORE messages SET mailbox = ?3 WHERE account = ?1 AND mailbox = ?2",
            rusqlite::params![account, from, to],
        )
        .context("moving the renamed folder's messages")?;
    for (table, column) in [
        ("sync_cursors", "mailbox"),
        ("ingest_failures", "mailbox"),
        ("mailboxes", "name"),
    ] {
        tx.execute(
            &format!(
                "UPDATE OR IGNORE {table} SET {column} = ?3 WHERE account = ?1 AND {column} = ?2"
            ),
            rusqlite::params![account, from, to],
        )
        .with_context(|| format!("moving the renamed folder's {table}"))?;
        tx.execute(
            &format!("DELETE FROM {table} WHERE account = ?1 AND {column} = ?2"),
            rusqlite::params![account, from],
        )
        .with_context(|| format!("dropping the renamed folder's old {table}"))?;
    }
    let leftovers: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT id FROM messages WHERE account = ?1 AND mailbox = ?2")?;
        let ids = stmt
            .query_map(rusqlite::params![account, from], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        ids
    };
    tx.commit().context("committing the rename")?;
    for id in leftovers {
        crate::store::write::delete_row(store, blobs, id)?;
    }
    Ok(moved)
}

/// Read the server's counts of every `status-only` folder and record them.
///
/// IMAP asks one `STATUS` per folder; Graph's folder tree carries every
/// folder's counts, so it lists once. Answers whether any count moved.
pub async fn refresh_counts(account: &AccountConfig) -> Result<bool> {
    let catalogue = Catalogue::load(account);
    let counted: Vec<String> = catalogue
        .folders
        .iter()
        .filter(|f| f.policy == FolderPolicy::StatusOnly && !f.missing && f.selectable())
        .map(|f| f.server.clone())
        .collect();
    if counted.is_empty() {
        return Ok(false);
    }
    let counts = if account.auth_method == AuthMethod::Graph {
        let config = GraphConfig::load(account)?;
        let client = crate::graph::GraphClient::new_async(&config).await?;
        client
            .list_folder_tree()
            .await?
            .into_iter()
            .filter(|(path, _)| counted.contains(path))
            .map(|(path, folder)| {
                (
                    path,
                    folder.total_item_count.max(0) as u64,
                    folder.unread_item_count.max(0) as u64,
                )
            })
            .collect()
    } else {
        crate::imap_client::status_counts(&ImapConfig::load(account)?, &counted).await?
    };
    let store = Store::open_account(&account.name)?;
    listing::record_counts(&store, &account.name, &counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::FetchedEmail;

    fn email(id: &str) -> FetchedEmail {
        FetchedEmail {
            from: "a@example.com".to_string(),
            to: "me@example.com".to_string(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: id.to_string(),
            date: "Mon, 01 Jan 2026 09:00:00 +0000".to_string(),
            body_text: "body".to_string(),
            html_body: None,
            has_attachments: false,
            server_has_attachments: None,
            message_id: Some(format!("<{id}@example.com>")),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        }
    }

    fn folder(name: &str, id: &str) -> ListedFolder {
        ListedFolder {
            id: Some(id.to_string()),
            ..ListedFolder::named(name)
        }
    }

    fn mailbox_of(store: &Store, id: &str) -> Vec<String> {
        let mut stmt = store
            .conn()
            .prepare("SELECT mailbox FROM messages WHERE message_id IN (?1, '<' || ?1 || '>') ORDER BY mailbox")
            .unwrap();
        stmt.query_map([format!("{id}@example.com")], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap()
    }

    /// #0141 follow-up: a Graph folder renamed on the server keeps its rows
    /// and its cursor under the new name, and a row the new key already holds
    /// is not duplicated. A pinned folder's rows stay with the pin.
    #[test]
    fn a_renamed_graph_folder_takes_its_rows_and_cursor_along() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let blobs = crate::store::BlobStore::new(dir.path().join("blobs"));
        let account = AccountConfig {
            name: "work".to_string(),
            auth_method: AuthMethod::Graph,
            ..Default::default()
        };
        let ingest = |mailbox: &str, id: &str, uid: i64| {
            crate::ingest::ingest_message(
                &store,
                &blobs,
                &crate::ingest::IngestInput {
                    account: "work",
                    mailbox,
                    uid,
                    email: &email(id),
                    raw: None,
                },
            )
            .unwrap();
        };
        record(
            &store,
            &blobs,
            &account,
            &[folder("Inbox", "AAA"), folder("Projects", "BBB")],
        )
        .unwrap();
        ingest("Projects", "one", 1);
        ingest("Projects", "two", 2);
        ingest("Projekte", "two", 2);
        crate::ingest::record_mailbox_cursor(
            &store,
            "work",
            "Projects",
            &crate::ingest::MailboxCursor {
                deltalink: Some("token".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(record(
            &store,
            &blobs,
            &account,
            &[folder("Inbox", "AAA"), folder("Projekte", "BBB")]
        )
        .unwrap());
        assert_eq!(mailbox_of(&store, "one"), vec!["Projekte"]);
        assert_eq!(mailbox_of(&store, "two"), vec!["Projekte"], "no duplicate");
        let cursor = crate::ingest::load_mailbox_cursor(&store, "work", "Projekte")
            .unwrap()
            .expect("the cursor moved");
        assert_eq!(cursor.deltalink.as_deref(), Some("token"));
        assert!(
            crate::ingest::load_mailbox_cursor(&store, "work", "Projects")
                .unwrap()
                .is_none()
        );

        // A pinned extra keeps its rows under the name config gives it.
        let pinned = AccountConfig {
            mailboxes: crate::config::MailboxesConfig {
                extra: Some(vec![crate::config::MailboxMapping {
                    server: "Projekte".to_string(),
                }]),
                ..Default::default()
            },
            ..account.clone()
        };
        record(
            &store,
            &blobs,
            &pinned,
            &[folder("Inbox", "AAA"), folder("Archiv 2026", "BBB")],
        )
        .unwrap();
        assert_eq!(mailbox_of(&store, "one"), vec!["Projekte"]);
    }
}
