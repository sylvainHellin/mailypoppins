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
pub async fn refresh(account: &AccountConfig) -> Result<bool> {
    let fresh = server_listing(account).await?;
    let store = Store::open_account(&account.name)?;
    listing::replace(&store, &account.name, &fresh)
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
