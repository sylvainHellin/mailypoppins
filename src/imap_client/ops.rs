//! Per-message server ops: archive, move, delete and the `\Seen` flag.
//!
//! Every one of them names its target by `Message-ID`, and by the UID of the
//! copy the store row stood for when it has one ([`resolve_copy`], PERSO-238),
//! and touches nothing locally: the local half of a mutation is the store row
//! the caller wrote optimistically (`crate::store::write`), and the result here
//! is what tells it whether to keep that write or put it back.

use std::collections::HashSet;

use anyhow::{anyhow, Result};
use futures::TryStreamExt;
use log::{info, warn};

use super::search::message_id_search_term;
use super::{pool, ImapSession};
use crate::config::ImapConfig;
use crate::ops::ServerCopy;

/// Run one op on a session borrowed from the persistent pool (#0041).
///
/// Every public entry point below is this wrapper around an `_on_session`
/// function, which is what stopped each queued mutation from paying its own
/// TCP + TLS + LOGIN. The three rules the wrapper enforces once, so no call
/// site has to remember them:
///
/// - the op always `SELECT`s: the connection arrives selected on whatever the
///   previous borrower was doing, or on nothing at all;
/// - a failed op poisons the session rather than returning it, because a
///   command whose response was not read to the end leaves bytes in the stream
///   that the next borrower would misread as its own answer;
/// - the session is *not* logged out; dropping the guard returns it.
///
/// The typed [`crate::ops::NotFoundOnServer`] travels through as an ordinary
/// error. That does poison a perfectly healthy connection, which costs one
/// reconnect on a path that is already rare (a replayed op whose target has
/// moved), and is the conservative side of the trade.
async fn with_pooled<F, T>(imap_config: &ImapConfig, op: F) -> Result<T>
where
    F: AsyncFnOnce(&mut ImapSession) -> Result<T>,
{
    let mut pooled = pool::checkout(imap_config).await?;
    let out = op(pooled.session()).await;
    pooled.check(out)
}

/// Move an email to a different mailbox on the IMAP server (#0018).
pub async fn move_email_on_server(
    imap_config: &ImapConfig,
    message_id: &str,
    source_mailbox: &str,
    copy: Option<ServerCopy>,
    dest_mailbox: &str,
) -> Result<()> {
    with_pooled(imap_config, async |s: &mut ImapSession| {
        move_email_on_session(s, message_id, source_mailbox, copy, dest_mailbox).await
    })
    .await
}

/// Delete an email on the server: SELECT, find the copy, \Deleted, EXPUNGE.
pub async fn delete_email_on_server(
    imap_config: &ImapConfig,
    message_id: &str,
    source_mailbox: &str,
    copy: Option<ServerCopy>,
) -> Result<()> {
    with_pooled(imap_config, async |s: &mut ImapSession| {
        delete_email_on_session(s, message_id, source_mailbox, copy).await
    })
    .await
}

/// Mark an email as read (`\Seen`) on the IMAP server.
pub async fn mark_read_on_server(
    imap_config: &ImapConfig,
    message_id: &str,
    mailbox: &str,
    copy: Option<ServerCopy>,
) -> Result<()> {
    with_pooled(imap_config, async |s: &mut ImapSession| {
        store_flags_on_session(s, message_id, mailbox, copy, "+FLAGS (\\Seen)").await
    })
    .await
}

/// Mark an email as unread (remove `\Seen`) on the IMAP server.
pub async fn mark_unread_on_server(
    imap_config: &ImapConfig,
    message_id: &str,
    mailbox: &str,
    copy: Option<ServerCopy>,
) -> Result<()> {
    with_pooled(imap_config, async |s: &mut ImapSession| {
        store_flags_on_session(s, message_id, mailbox, copy, "-FLAGS (\\Seen)").await
    })
    .await
}

/// Add one flag to a message on the server (#TKT-0051).
///
/// `+FLAGS` rather than `FLAGS`, so nothing the server already holds is
/// cleared. A server that refuses the keyword (`PERMANENTFLAGS` without `\*`)
/// fails here, which the caller logs and lives with: the next sync overwrites
/// the local bit with what the server believes, so the two never drift
/// silently.
pub async fn add_flag_on_server(
    imap_config: &ImapConfig,
    message_id: &str,
    mailbox: &str,
    copy: Option<ServerCopy>,
    flag: &str,
) -> Result<()> {
    let store = format!("+FLAGS ({flag})");
    with_pooled(imap_config, async |s: &mut ImapSession| {
        store_flags_on_session(s, message_id, mailbox, copy, &store).await
    })
    .await
}

/// Remove one flag from a message on the server (#0007): `-FLAGS`, so only
/// the named flag is touched.
pub async fn remove_flag_on_server(
    imap_config: &ImapConfig,
    message_id: &str,
    mailbox: &str,
    copy: Option<ServerCopy>,
    flag: &str,
) -> Result<()> {
    let store = format!("-FLAGS ({flag})");
    with_pooled(imap_config, async |s: &mut ImapSession| {
        store_flags_on_session(s, message_id, mailbox, copy, &store).await
    })
    .await
}

/// The one IMAP command [`resolve_copy`] issues, behind a trait so its rule is
/// pinned against a fake folder: the repository has no in-process IMAP server.
pub(super) trait UidSearch {
    async fn search_uids(&mut self, query: &str) -> Result<HashSet<u32>>;
}

impl UidSearch for ImapSession {
    async fn search_uids(&mut self, query: &str) -> Result<HashSet<u32>> {
        self.uid_search(query)
            .await
            .map_err(|e| anyhow!("IMAP search failed: {}", e))
    }
}

/// The UID in the selected folder that an op on `message_id` acts on, or
/// `None` when the copy it names is not there (PERSO-238).
///
/// A folder can hold several copies under one `Message-ID`, and the store keeps
/// one row per copy, so the op acts on the copy its row stood for:
///
/// - with a [`ServerCopy`] whose UIDVALIDITY is the selected folder's (or is
///   unknown on either side), the answer is that UID, checked with one
///   `UID <uid> HEADER Message-ID` search. A UID that no longer holds the
///   message is `None`, never a sibling copy: within one UIDVALIDITY a UID is
///   never reused, so its copy was moved or deleted, by this op's own earlier
///   run on a replay above all, and acting on another copy would touch a
///   message the user did not name;
/// - with no copy (the row had no server UID yet, or the op was queued before
///   the field existed), or one from another UIDVALIDITY, which makes its UID
///   meaningless, the answer is the highest UID the `Message-ID` search
///   lists. Such a row is one the store moved or wrote ahead of the server,
///   whose copy in this folder is the one an earlier queued op created, and a
///   `UID COPY` or `APPEND` mints the folder's next UID, which is its highest.
///   `HashSet` order, which picked before, is no rule at all.
pub(super) async fn resolve_copy<S: UidSearch>(
    session: &mut S,
    message_id: &str,
    copy: Option<ServerCopy>,
    selected_uidvalidity: Option<u32>,
) -> Result<Option<u32>> {
    let term = message_id_search_term(message_id);
    match copy {
        Some(copy) if same_uidvalidity(copy.uidvalidity, selected_uidvalidity) => {
            let found = session
                .search_uids(&format!("UID {} {term}", copy.uid))
                .await?;
            Ok(found.contains(&copy.uid).then_some(copy.uid))
        }
        stale => {
            if let Some(copy) = stale {
                warn!(
                    "UID {} of {message_id} was recorded under UIDVALIDITY {:?} and the folder \
                     is now {:?}; finding the message by Message-ID instead",
                    copy.uid, copy.uidvalidity, selected_uidvalidity
                );
            }
            let found = session.search_uids(&term).await?;
            if found.len() > 1 {
                let mut uids: Vec<u32> = found.iter().copied().collect();
                uids.sort_unstable();
                info!("{message_id} is in the folder {} times (UIDs {uids:?}); acting on the newest copy", uids.len());
            }
            Ok(found.into_iter().max())
        }
    }
}

/// Whether a recorded UIDVALIDITY still holds: an unknown side cannot say it
/// changed, and the `Message-ID` in the UID search still guards the copy.
fn same_uidvalidity(recorded: Option<u32>, selected: Option<u32>) -> bool {
    match (recorded, selected) {
        (Some(recorded), Some(selected)) => recorded == selected,
        _ => true,
    }
}

/// SELECT `mailbox` and resolve the copy an op acts on ([`resolve_copy`]).
async fn select_copy(
    session: &mut ImapSession,
    message_id: &str,
    mailbox: &str,
    copy: Option<ServerCopy>,
) -> Result<Option<u32>> {
    let selected = session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select {}: {}", mailbox, e))?;
    resolve_copy(session, message_id, copy, selected.uid_validity).await
}

/// The typed not-found of a move or delete whose copy is gone, so the durable
/// queue's drain can converge a replay whose op already landed (#0039 review);
/// a direct CLI/TUI caller still sees the same message through `Display`.
fn not_found(message_id: &str, mailbox: &str) -> anyhow::Error {
    crate::ops::NotFoundOnServer {
        message_id: message_id.to_string(),
        mailbox: Some(mailbox.to_string()),
    }
    .into()
}

/// Move an email to a different mailbox on the IMAP server (#0018).
///
/// SELECTs `source_mailbox`, resolves the copy ([`resolve_copy`]), then
/// UID COPY + \Deleted + EXPUNGE -- the same machinery as archiving
/// (which is a move with a fixed destination). COPY+EXPUNGE is used
/// instead of MOVE so servers without the MOVE extension work too;
/// COPY preserves flags, so read/unread state survives the move.
async fn move_email_on_session(
    session: &mut ImapSession,
    message_id: &str,
    source_mailbox: &str,
    copy: Option<ServerCopy>,
    dest_mailbox: &str,
) -> Result<()> {
    info!(
        "Moving email on server: Message-ID={} {} -> {}",
        message_id, source_mailbox, dest_mailbox
    );
    let Some(uid) = select_copy(session, message_id, source_mailbox, copy).await? else {
        return Err(not_found(message_id, source_mailbox));
    };
    let uid_str = uid.to_string();

    session
        .uid_copy(&uid_str, dest_mailbox)
        .await
        .map_err(|e| anyhow!("Failed to copy email to {}: {}", dest_mailbox, e))?;

    mark_deleted_and_expunge(session, &uid_str).await
}

/// Delete an email on the server: SELECT, resolve the copy, \Deleted, EXPUNGE.
///
/// `source_mailbox` is the server-side folder the message is in. It used to be
/// hardcoded to `INBOX`, which was only ever right because the file build could
/// not tell where a message lived; the store row knows its mailbox, so the
/// caller passes the folder it maps to.
async fn delete_email_on_session(
    session: &mut ImapSession,
    message_id: &str,
    source_mailbox: &str,
    copy: Option<ServerCopy>,
) -> Result<()> {
    info!("Deleting email on server: Message-ID={}", message_id);
    let Some(uid) = select_copy(session, message_id, source_mailbox, copy).await? else {
        return Err(not_found(message_id, source_mailbox));
    };
    mark_deleted_and_expunge(session, &uid.to_string()).await
}

/// The tail a move and a delete share: `\Deleted` on one UID, then EXPUNGE.
async fn mark_deleted_and_expunge(session: &mut ImapSession, uid: &str) -> Result<()> {
    session
        .uid_store(uid, "+FLAGS (\\Deleted)")
        .await
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect store response: {}", e))?;

    session
        .expunge()
        .await
        .map_err(|e| anyhow!("Failed to expunge: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect expunge response: {}", e))?;

    Ok(())
}

/// One `UID STORE` of `change` (`+FLAGS (...)` or `-FLAGS (...)`) on the copy
/// an op names, for the read toggle, the star and the post-send bits.
///
/// A copy the folder no longer holds is not an error: the row may have been
/// moved or deleted elsewhere since the op was queued, and the next sync
/// restates whatever the server holds either way.
async fn store_flags_on_session(
    session: &mut ImapSession,
    message_id: &str,
    mailbox: &str,
    copy: Option<ServerCopy>,
    change: &str,
) -> Result<()> {
    info!("Storing {change} on server: Message-ID={message_id} in {mailbox}");
    let Some(uid) = select_copy(session, message_id, mailbox, copy).await? else {
        return Ok(());
    };
    session
        .uid_store(&uid.to_string(), change)
        .await
        .map_err(|e| anyhow!("Failed to store {}: {}", change, e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect store response: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder that answers the two searches [`resolve_copy`] sends, and
    /// records them.
    struct Folder {
        copies: Vec<(u32, &'static str)>,
        queries: Vec<String>,
    }

    impl Folder {
        fn new(copies: &[(u32, &'static str)]) -> Folder {
            Folder {
                copies: copies.to_vec(),
                queries: Vec::new(),
            }
        }
    }

    impl UidSearch for Folder {
        async fn search_uids(&mut self, query: &str) -> Result<HashSet<u32>> {
            self.queries.push(query.to_string());
            Ok(self
                .copies
                .iter()
                .filter(|(uid, id)| {
                    let term = message_id_search_term(id);
                    query == term || query == format!("UID {uid} {term}")
                })
                .map(|(uid, _)| *uid)
                .collect())
        }
    }

    const DUP: &str = "<diu6d8@example.com>";

    /// The three copies PERSO-236 left in Archive, plus an unrelated message.
    fn archive() -> Folder {
        Folder::new(&[
            (265, "<other@example.com>"),
            (285, DUP),
            (288, DUP),
            (289, DUP),
        ])
    }

    fn copy(uid: u32, uidvalidity: Option<u32>) -> Option<ServerCopy> {
        Some(ServerCopy { uid, uidvalidity })
    }

    async fn resolve(
        folder: &mut Folder,
        copy: Option<ServerCopy>,
        selected: Option<u32>,
    ) -> Option<u32> {
        resolve_copy(folder, DUP, copy, selected).await.unwrap()
    }

    /// PERSO-238: each of three copies under one Message-ID is the one acted
    /// on when its row names it, which the first-listed copy of a search
    /// cannot be for more than one of them.
    #[tokio::test]
    async fn an_op_acts_on_the_copy_its_row_names_among_duplicates() {
        for uid in [285, 288, 289] {
            let mut folder = archive();
            assert_eq!(
                resolve(&mut folder, copy(uid, Some(7)), Some(7)).await,
                Some(uid)
            );
            assert_eq!(
                folder.queries,
                vec![format!("UID {uid} HEADER Message-ID \"{DUP}\"")],
                "one search, scoped to the row's UID"
            );
        }
    }

    /// A UID that no longer holds the message is gone, not a reason to act on
    /// a sibling copy: a replayed move whose first run landed must converge
    /// rather than move the next duplicate (PERSO-236).
    #[tokio::test]
    async fn a_named_copy_that_is_gone_is_not_found_rather_than_a_sibling() {
        let mut folder = archive();
        assert_eq!(
            resolve(&mut folder, copy(286, Some(7)), Some(7)).await,
            None
        );
        assert_eq!(
            resolve(&mut folder, copy(265, None), Some(7)).await,
            None,
            "another message's UID"
        );
    }

    /// No copy (a row moved locally, an op queued before the field existed)
    /// and a copy from another UIDVALIDITY both fall back to the Message-ID
    /// search, and take its newest copy whatever order the server lists them in.
    #[tokio::test]
    async fn without_a_usable_uid_the_newest_copy_is_chosen() {
        let mut folder = archive();
        assert_eq!(resolve(&mut folder, None, Some(7)).await, Some(289));
        assert_eq!(folder.queries, vec![format!("HEADER Message-ID \"{DUP}\"")]);

        let mut folder = archive();
        assert_eq!(
            resolve(&mut folder, copy(285, Some(6)), Some(7)).await,
            Some(289),
            "a reset folder"
        );
        assert_eq!(
            resolve(&mut folder, copy(285, Some(7)), None).await,
            Some(285),
            "an unknown side is not a reset"
        );

        let mut empty = Folder::new(&[]);
        assert_eq!(resolve(&mut empty, None, None).await, None);
    }
}
