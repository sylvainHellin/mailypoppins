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
    copy: Option<&ServerCopy>,
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
    copy: Option<&ServerCopy>,
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
    copy: Option<&ServerCopy>,
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
    copy: Option<&ServerCopy>,
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
    copy: Option<&ServerCopy>,
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
    copy: Option<&ServerCopy>,
    flag: &str,
) -> Result<()> {
    let store = format!("-FLAGS ({flag})");
    with_pooled(imap_config, async |s: &mut ImapSession| {
        store_flags_on_session(s, message_id, mailbox, copy, &store).await
    })
    .await
}

/// The two IMAP commands [`resolve_copy`] and its callers issue, behind a
/// trait so their rules are pinned against a fake account: the repository has
/// no in-process IMAP server.
pub(super) trait UidSearch {
    /// `UID SEARCH` in the selected folder.
    async fn search_uids(&mut self, query: &str) -> Result<HashSet<u32>>;

    /// Whether `mailbox` holds a copy under `message_id`, read with `EXAMINE`
    /// so nothing in it changes; the selected folder is `mailbox` afterwards.
    async fn holds(&mut self, mailbox: &str, message_id: &str) -> Result<bool>;
}

impl UidSearch for ImapSession {
    async fn search_uids(&mut self, query: &str) -> Result<HashSet<u32>> {
        self.uid_search(query)
            .await
            .map_err(|e| anyhow!("IMAP search failed: {}", e))
    }

    async fn holds(&mut self, mailbox: &str, message_id: &str) -> Result<bool> {
        self.examine(mailbox)
            .await
            .map_err(|e| anyhow!("Failed to examine {}: {}", mailbox, e))?;
        Ok(!self
            .search_uids(&message_id_search_term(message_id))
            .await?
            .is_empty())
    }
}

/// What the selected folder holds of the copy an op names ([`resolve_copy`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Resolved {
    /// The copy to act on, by UID.
    Copy(u32),
    /// The folder holds no copy under the `Message-ID`.
    Gone,
    /// The op's own UID is gone and the folder still holds copies, none of
    /// which can be taken for it: one another store row is bound to, or
    /// several. `unbound` is whether any of them is bound to no row, which
    /// is what a stale UID of the op's own copy would look like.
    Missed { unbound: bool },
}

/// The copy in the selected folder that an op on `message_id` acts on
/// (PERSO-238).
///
/// A folder can hold several copies under one `Message-ID`, and the store keeps
/// one row per copy, so the op acts on the copy its row stood for:
///
/// - with a [`ServerCopy`] whose UIDVALIDITY is the selected folder's (or is
///   unknown on either side), the answer is that UID, checked with one
///   `UID <uid> HEADER Message-ID` search;
/// - when that UID no longer holds the message, a `Message-ID` search decides.
///   No copy is [`Resolved::Gone`]. A single copy that no other row of the
///   store is bound to ([`ServerCopy::siblings`]) is the op's own under a UID
///   gone stale, because another client moved it out and back since the last
///   sync, and is the answer. A copy another row stands for, or several, is
///   [`Resolved::Missed`] and nothing is acted on: within one UIDVALIDITY a
///   UID is never reused, so the op's copy was moved or deleted, by this op's
///   own earlier run on a replay above all (PERSO-236), and acting on a twin
///   would touch a message the user did not name;
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
    copy: Option<&ServerCopy>,
    selected_uidvalidity: Option<u32>,
) -> Result<Resolved> {
    let term = message_id_search_term(message_id);
    match copy {
        Some(copy) if same_uidvalidity(copy.uidvalidity, selected_uidvalidity) => {
            let found = session
                .search_uids(&format!("UID {} {term}", copy.uid))
                .await?;
            if found.contains(&copy.uid) {
                return Ok(Resolved::Copy(copy.uid));
            }
            let found = session.search_uids(&term).await?;
            let unbound: Vec<u32> = found
                .iter()
                .copied()
                .filter(|uid| !copy.siblings.contains(uid))
                .collect();
            match (found.len(), unbound.as_slice()) {
                (0, _) => Ok(Resolved::Gone),
                (1, [only]) => {
                    warn!(
                        "UID {} of {message_id} holds nothing any more and the folder's one copy \
                         is UID {only}, which no other row stands for; acting on it",
                        copy.uid
                    );
                    Ok(Resolved::Copy(*only))
                }
                _ => {
                    let mut uids: Vec<u32> = found.into_iter().collect();
                    uids.sort_unstable();
                    info!(
                        "UID {} of {message_id} holds nothing any more; the folder's copies \
                         {uids:?} are not taken for it (other rows stand for {:?})",
                        copy.uid, copy.siblings
                    );
                    Ok(Resolved::Missed {
                        unbound: !unbound.is_empty(),
                    })
                }
            }
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
            Ok(found
                .into_iter()
                .max()
                .map_or(Resolved::Gone, Resolved::Copy))
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

/// SELECT `mailbox`, returning its UIDVALIDITY.
async fn select(session: &mut ImapSession, mailbox: &str) -> Result<Option<u32>> {
    let selected = session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select {}: {}", mailbox, e))?;
    Ok(selected.uid_validity)
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

/// A move or delete whose copy is gone while the folder still holds copies
/// that cannot be told apart from it ([`Resolved::Missed`]), when no completed
/// run of the op explains the miss.
///
/// An ordinary error, not [`not_found`]: the drain retries it and then rolls
/// the local change back, so the store shows the message where the server
/// still holds it, and the refusal is visible, instead of retiring the op as a
/// converged replay while the server copy stays and the next sync brings it
/// back.
fn unsettled(message_id: &str, mailbox: &str, copy: Option<&ServerCopy>) -> anyhow::Error {
    let uid = copy.map_or_else(|| "?".to_string(), |copy| copy.uid.to_string());
    anyhow!(
        "the copy of {message_id} the store saw in {mailbox} (UID {uid}) is not there any \
         more, and the copies {mailbox} holds now cannot be told apart from it; nothing was \
         changed on the server"
    )
}

/// The UID a move of `message_id` from the selected `source_mailbox` to
/// `dest_mailbox` acts on, or why it acts on none (PERSO-238).
///
/// A [`Resolved::Missed`] whose remaining copies all stand for other rows,
/// with the message already in the destination, is what a replay of a move
/// that landed leaves (PERSO-236): it converges as not found. Any other miss is
/// [`unsettled`].
pub(super) async fn copy_to_move<S: UidSearch>(
    session: &mut S,
    message_id: &str,
    source_mailbox: &str,
    copy: Option<&ServerCopy>,
    selected_uidvalidity: Option<u32>,
    dest_mailbox: &str,
) -> Result<u32> {
    match resolve_copy(session, message_id, copy, selected_uidvalidity).await? {
        Resolved::Copy(uid) => Ok(uid),
        Resolved::Gone => Err(not_found(message_id, source_mailbox)),
        Resolved::Missed { unbound: false } if session.holds(dest_mailbox, message_id).await? => {
            info!(
                "{message_id} is already in {dest_mailbox} and its copy left {source_mailbox}: \
                 a move that landed"
            );
            Err(not_found(message_id, source_mailbox))
        }
        Resolved::Missed { .. } => Err(unsettled(message_id, source_mailbox, copy)),
    }
}

/// The UID a delete of `message_id` from the selected `source_mailbox` acts
/// on, or why it acts on none (PERSO-238).
///
/// A [`Resolved::Missed`] whose remaining copies all stand for other rows is
/// what a replay of a delete that landed leaves, with no destination to
/// confirm it: it converges as not found, as a folder with no copy at all
/// does. A miss with a copy no row stands for, which may be the op's own
/// under a new UID, is [`unsettled`].
pub(super) async fn copy_to_delete<S: UidSearch>(
    session: &mut S,
    message_id: &str,
    source_mailbox: &str,
    copy: Option<&ServerCopy>,
    selected_uidvalidity: Option<u32>,
) -> Result<u32> {
    match resolve_copy(session, message_id, copy, selected_uidvalidity).await? {
        Resolved::Copy(uid) => Ok(uid),
        Resolved::Gone | Resolved::Missed { unbound: false } => {
            Err(not_found(message_id, source_mailbox))
        }
        Resolved::Missed { unbound: true } => Err(unsettled(message_id, source_mailbox, copy)),
    }
}

/// Move an email to a different mailbox on the IMAP server (#0018).
///
/// SELECTs `source_mailbox`, resolves the copy ([`copy_to_move`]), then
/// UID COPY + \Deleted + EXPUNGE -- the same machinery as archiving
/// (which is a move with a fixed destination). COPY+EXPUNGE is used
/// instead of MOVE so servers without the MOVE extension work too;
/// COPY preserves flags, so read/unread state survives the move.
async fn move_email_on_session(
    session: &mut ImapSession,
    message_id: &str,
    source_mailbox: &str,
    copy: Option<&ServerCopy>,
    dest_mailbox: &str,
) -> Result<()> {
    info!(
        "Moving email on server: Message-ID={} {} -> {}",
        message_id, source_mailbox, dest_mailbox
    );
    let uidvalidity = select(session, source_mailbox).await?;
    let uid = copy_to_move(
        session,
        message_id,
        source_mailbox,
        copy,
        uidvalidity,
        dest_mailbox,
    )
    .await?;
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
    copy: Option<&ServerCopy>,
) -> Result<()> {
    info!("Deleting email on server: Message-ID={}", message_id);
    let uidvalidity = select(session, source_mailbox).await?;
    let uid = copy_to_delete(session, message_id, source_mailbox, copy, uidvalidity).await?;
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
/// A copy the folder no longer holds, or one it cannot tell from a twin
/// ([`Resolved::Missed`]), is not an error and is left alone: the row may have
/// been moved or deleted elsewhere since the op was queued, and the next sync
/// restates whatever the server holds either way.
async fn store_flags_on_session(
    session: &mut ImapSession,
    message_id: &str,
    mailbox: &str,
    copy: Option<&ServerCopy>,
    change: &str,
) -> Result<()> {
    info!("Storing {change} on server: Message-ID={message_id} in {mailbox}");
    let uidvalidity = select(session, mailbox).await?;
    let Resolved::Copy(uid) = resolve_copy(session, message_id, copy, uidvalidity).await? else {
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

    /// A folder that answers the searches [`resolve_copy`] sends, and
    /// records them, beside the other folders [`UidSearch::holds`] reads.
    struct Folder {
        copies: Vec<(u32, &'static str)>,
        elsewhere: Vec<(&'static str, &'static str)>,
        queries: Vec<String>,
    }

    impl Folder {
        fn new(copies: &[(u32, &'static str)]) -> Folder {
            Folder {
                copies: copies.to_vec(),
                elsewhere: Vec::new(),
                queries: Vec::new(),
            }
        }

        /// The same, with `message_id` also in the folder `mailbox`.
        fn with_elsewhere(mut self, mailbox: &'static str, message_id: &'static str) -> Folder {
            self.elsewhere.push((mailbox, message_id));
            self
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

        async fn holds(&mut self, mailbox: &str, message_id: &str) -> Result<bool> {
            self.queries.push(format!("EXAMINE {mailbox}"));
            Ok(self
                .elsewhere
                .iter()
                .any(|(folder, id)| *folder == mailbox && *id == message_id))
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
        Some(ServerCopy {
            uid,
            uidvalidity,
            siblings: Vec::new(),
        })
    }

    /// The copy `uid` of a row whose twins in the folder are bound to
    /// `siblings`.
    fn twin_of(uid: u32, siblings: &[u32]) -> Option<ServerCopy> {
        Some(ServerCopy {
            uid,
            uidvalidity: Some(7),
            siblings: siblings.to_vec(),
        })
    }

    async fn resolve(
        folder: &mut Folder,
        copy: Option<ServerCopy>,
        selected: Option<u32>,
    ) -> Resolved {
        resolve_copy(folder, DUP, copy.as_ref(), selected)
            .await
            .unwrap()
    }

    async fn move_to_archive(folder: &mut Folder, copy: Option<ServerCopy>) -> Result<u32> {
        copy_to_move(folder, DUP, "INBOX", copy.as_ref(), Some(7), "Archive").await
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
                Resolved::Copy(uid)
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
    async fn a_named_copy_that_is_gone_is_not_replaced_by_a_sibling() {
        let mut folder = archive();
        assert_eq!(
            resolve(&mut folder, twin_of(286, &[285, 288]), Some(7)).await,
            Resolved::Missed { unbound: true },
            "several copies, one of them bound to no row"
        );
        assert_eq!(
            resolve(&mut folder, twin_of(286, &[285, 288, 289]), Some(7)).await,
            Resolved::Missed { unbound: false }
        );
        assert_eq!(
            resolve(&mut folder, copy(265, None), Some(7)).await,
            Resolved::Missed { unbound: true },
            "another message's UID"
        );
        let mut empty = Folder::new(&[(265, "<other@example.com>")]);
        assert_eq!(
            resolve(&mut empty, copy(286, Some(7)), Some(7)).await,
            Resolved::Gone
        );
    }

    /// A UID gone stale, because another client moved the message out and
    /// back since the last sync, leaves the folder's one copy under a new UID
    /// no row stands for: the op acts on it, as the `Message-ID` search did
    /// before the op named its UID (PERSO-238).
    #[tokio::test]
    async fn a_stale_uid_acts_on_the_single_copy_no_other_row_stands_for() {
        let mut folder = Folder::new(&[(301, DUP)]);
        assert_eq!(
            move_to_archive(&mut folder, copy(285, Some(7)))
                .await
                .unwrap(),
            301
        );
        let mut folder = Folder::new(&[(301, DUP)]);
        assert_eq!(
            copy_to_delete(
                &mut folder,
                DUP,
                "INBOX",
                copy(285, Some(7)).as_ref(),
                Some(7)
            )
            .await
            .unwrap(),
            301
        );
        let mut folder = Folder::new(&[(301, DUP)]);
        assert_eq!(
            resolve(&mut folder, copy(285, Some(7)), Some(7)).await,
            Resolved::Copy(301),
            "a flag op too"
        );
    }

    /// A replay of a move whose first run landed: the row's copy left the
    /// source, its twin there stands for another row, and the destination
    /// holds the message. It converges as not found and leaves the twin
    /// alone (PERSO-236).
    #[tokio::test]
    async fn a_replayed_move_converges_without_touching_a_twin() {
        let mut folder = Folder::new(&[(288, DUP)]).with_elsewhere("Archive", DUP);
        let err = move_to_archive(&mut folder, twin_of(285, &[288]))
            .await
            .unwrap_err();
        assert!(crate::ops::NotFoundOnServer::is_in(&err), "{err:#}");

        let mut folder = Folder::new(&[(288, DUP)]);
        let err = copy_to_delete(
            &mut folder,
            DUP,
            "INBOX",
            twin_of(285, &[288]).as_ref(),
            Some(7),
        )
        .await
        .unwrap_err();
        assert!(
            crate::ops::NotFoundOnServer::is_in(&err),
            "a replayed delete has no destination and converges: {err:#}"
        );
    }

    /// The row's copy left the source, its twin stands for another row, and
    /// the destination does not hold the message: no run of this move did
    /// it. Nothing is acted on, and the error is not a not-found, so the
    /// drain rolls the local move back rather than retiring it (PERSO-238).
    #[tokio::test]
    async fn a_miss_no_landed_move_explains_is_not_a_not_found() {
        let mut folder = Folder::new(&[(288, DUP)]);
        let err = move_to_archive(&mut folder, twin_of(285, &[288]))
            .await
            .unwrap_err();
        assert!(!crate::ops::NotFoundOnServer::is_in(&err), "{err:#}");
        assert!(err.to_string().contains("cannot be told apart"), "{err:#}");
        assert_eq!(
            folder.queries.last().map(String::as_str),
            Some("EXAMINE Archive")
        );

        let mut folder = Folder::new(&[(288, DUP), (301, DUP)]).with_elsewhere("Archive", DUP);
        let err = move_to_archive(&mut folder, twin_of(285, &[288]))
            .await
            .unwrap_err();
        assert!(
            !crate::ops::NotFoundOnServer::is_in(&err),
            "a copy no row stands for may be the row's own: {err:#}"
        );
        let err = copy_to_delete(
            &mut folder,
            DUP,
            "INBOX",
            twin_of(285, &[288]).as_ref(),
            Some(7),
        )
        .await
        .unwrap_err();
        assert!(!crate::ops::NotFoundOnServer::is_in(&err), "{err:#}");
    }

    /// No copy (a row moved locally, an op queued before the field existed)
    /// and a copy from another UIDVALIDITY both fall back to the Message-ID
    /// search, and take its newest copy whatever order the server lists them in.
    #[tokio::test]
    async fn without_a_usable_uid_the_newest_copy_is_chosen() {
        let mut folder = archive();
        assert_eq!(
            resolve(&mut folder, None, Some(7)).await,
            Resolved::Copy(289)
        );
        assert_eq!(folder.queries, vec![format!("HEADER Message-ID \"{DUP}\"")]);

        let mut folder = archive();
        assert_eq!(
            resolve(&mut folder, copy(285, Some(6)), Some(7)).await,
            Resolved::Copy(289),
            "a reset folder"
        );
        assert_eq!(
            resolve(&mut folder, copy(285, Some(7)), None).await,
            Resolved::Copy(285),
            "an unknown side is not a reset"
        );

        let mut empty = Folder::new(&[]);
        assert_eq!(resolve(&mut empty, None, None).await, Resolved::Gone);
    }

    /// The drain over a fake server whose IMAP arm is the real decision
    /// ([`copy_to_move`]): the three PERSO-238 cases end to end, from the
    /// op the queue builds off the store rows to what the drain does with
    /// the answer.
    mod drain {
        use std::collections::HashMap;

        use super::*;
        use crate::mutations::queue_move;
        use crate::ops::ServerOp;
        use crate::pending_ops::{self, OpExecutor, Runner, MAX_ATTEMPTS};
        use crate::reconcile::tests::{fixture, Fixture};
        use crate::store::read;

        /// A server's folders, the one selected, and every copy it moved.
        struct Server {
            folders: HashMap<&'static str, Vec<(u32, String)>>,
            selected: &'static str,
            next_uid: u32,
            moved: Vec<String>,
        }

        impl Server {
            fn new(folders: &[(&'static str, &[(u32, &str)])]) -> Server {
                Server {
                    folders: folders
                        .iter()
                        .map(|(name, copies)| {
                            let copies = copies.iter().map(|(u, id)| (*u, id.to_string()));
                            (*name, copies.collect())
                        })
                        .collect(),
                    selected: "INBOX",
                    next_uid: 500,
                    moved: Vec::new(),
                }
            }

            fn folder(&self, name: &str) -> Vec<u32> {
                let copies = self.folders.get(name).into_iter().flatten();
                copies.map(|(uid, _)| *uid).collect()
            }
        }

        impl UidSearch for Server {
            async fn search_uids(&mut self, query: &str) -> Result<HashSet<u32>> {
                let copies = self.folders.get(self.selected).into_iter().flatten();
                Ok(copies
                    .filter(|(uid, id)| {
                        let term = message_id_search_term(id);
                        query == term || query == format!("UID {uid} {term}")
                    })
                    .map(|(uid, _)| *uid)
                    .collect())
            }

            async fn holds(&mut self, mailbox: &str, message_id: &str) -> Result<bool> {
                let copies = self.folders.get(mailbox).into_iter().flatten();
                Ok(copies.clone().any(|(_, id)| id == message_id))
            }
        }

        impl OpExecutor for Server {
            async fn execute(&mut self, op: &ServerOp) -> Result<()> {
                let ServerOp::Move {
                    message_id,
                    source_mailbox,
                    dest_mailbox,
                    copy,
                } = op
                else {
                    anyhow::bail!("only moves here");
                };
                self.selected = if source_mailbox == "INBOX" {
                    "INBOX"
                } else {
                    "Archive"
                };
                let uid = copy_to_move(
                    self,
                    message_id,
                    source_mailbox,
                    copy.as_ref(),
                    Some(7),
                    dest_mailbox,
                )
                .await?;
                let source = self.folders.get_mut(self.selected).unwrap();
                let at = source.iter().position(|(u, _)| *u == uid).unwrap();
                let (_, id) = source.remove(at);
                let dest = if dest_mailbox == "INBOX" {
                    "INBOX"
                } else {
                    "Archive"
                };
                self.folders
                    .entry(dest)
                    .or_default()
                    .push((self.next_uid, id));
                self.next_uid += 1;
                self.moved
                    .push(format!("{uid} {source_mailbox} -> {dest_mailbox}"));
                Ok(())
            }
        }

        const MID: &str = "<inbox-1@example.com>";

        /// `inbox/1`, and with `twin` a second row of the same message in
        /// inbox on that UID.
        fn rows(twin: Option<i64>) -> (Fixture, i64) {
            let fx = fixture();
            let id = fx.ingest_plain("inbox", 1, "Receipt");
            if let Some(uid) = twin {
                fx.store
                    .conn()
                    .execute(
                        "INSERT INTO messages (account, mailbox, uid, message_id, subject)
                         SELECT account, mailbox, ?2, message_id, subject
                         FROM messages WHERE id = ?1",
                        rusqlite::params![id, uid],
                    )
                    .unwrap();
            }
            (fx, id)
        }

        /// `mp archive` from the TUI: queued for the drain.
        fn archive(fx: &Fixture, id: i64) {
            let queued = queue_move(
                &fx.store,
                "alice",
                &[id],
                "archive",
                "INBOX",
                "Archive",
                Runner::Drain,
            );
            assert_eq!(queued.len(), 1);
        }

        /// Drain once per attempt the queue allows, each past its backoff.
        async fn drain_all(fx: &Fixture, server: &mut Server) -> pending_ops::DrainResult {
            let mut total = pending_ops::DrainResult::default();
            for pass in 1..=MAX_ATTEMPTS {
                let now = crate::outbox::unix_now() + pass * 100_000;
                let result = pending_ops::drain(&fx.store, &fx.blobs, "alice", server, now)
                    .await
                    .unwrap();
                total.completed += result.completed;
                total.failed += result.failed;
                total.still_open = result.still_open;
            }
            total
        }

        /// A UID gone stale: another client moved the message out of INBOX
        /// and back, so its one copy there is UID 301 and the row still says
        /// 1. The drain moves that copy, where it used to retire the op as a
        /// converged replay and leave the copy for the next sync to bring
        /// back (PERSO-238).
        #[tokio::test]
        async fn a_stale_uid_moves_the_single_copy_no_other_row_stands_for() {
            let (fx, id) = rows(None);
            archive(&fx, id);
            let mut server = Server::new(&[("INBOX", &[(301, MID)])]);

            let drained = drain_all(&fx, &mut server).await;

            assert_eq!((drained.completed, drained.failed), (1, 0));
            assert_eq!(server.moved, ["301 INBOX -> Archive"]);
            assert_eq!(server.folder("INBOX"), Vec::<u32>::new());
            assert_eq!(server.folder("Archive"), [500]);
        }

        /// A replay of a move that landed: the row's copy (UID 1) is in
        /// Archive, and the twin INBOX still holds (UID 2) stands for the
        /// other row. The drain converges and leaves the twin alone
        /// (PERSO-236).
        #[tokio::test]
        async fn a_replayed_move_converges_and_leaves_the_twin() {
            let (fx, id) = rows(Some(2));
            archive(&fx, id);
            let mut server = Server::new(&[("INBOX", &[(2, MID)]), ("Archive", &[(40, MID)])]);

            let drained = drain_all(&fx, &mut server).await;

            assert_eq!((drained.completed, drained.failed), (1, 0));
            assert!(server.moved.is_empty(), "{:?}", server.moved);
            assert_eq!(server.folder("INBOX"), [2]);
            let row = read::find_by_id(&fx.store, id).unwrap().unwrap();
            assert_eq!(row.mailbox, "archive", "the local move stands");
        }

        /// The row's copy left INBOX, the one copy left there stands for the
        /// twin row, and Archive does not hold the message: no run of this
        /// move took it there. Nothing is moved, and the drain rolls the
        /// local move back and parks the op `failed` instead of retiring it
        /// as converged (PERSO-238).
        #[tokio::test]
        async fn a_miss_no_landed_move_explains_is_rolled_back() {
            let (fx, id) = rows(Some(2));
            archive(&fx, id);
            let mut server = Server::new(&[("INBOX", &[(2, MID)])]);

            let drained = drain_all(&fx, &mut server).await;

            assert_eq!((drained.completed, drained.failed), (0, 1));
            assert!(server.moved.is_empty(), "{:?}", server.moved);
            assert_eq!(server.folder("INBOX"), [2]);
            let row = read::find_by_id(&fx.store, id).unwrap().unwrap();
            assert_eq!((row.mailbox.as_str(), row.uid), ("inbox", 1), "rolled back");
            let failed = pending_ops::failed_ops(&fx.store, "alice").unwrap();
            let error = failed[0].last_error.as_deref().unwrap_or_default();
            assert!(error.contains("cannot be told apart"), "{error}");
        }
    }
}
