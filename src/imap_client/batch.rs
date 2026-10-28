//! One flag on one message in several mailboxes, over one IMAP connection.
//!
//! The per-message ops in [`super::ops`] open a session each, which is fine for
//! one copy and wasteful for the post-send bit, which lands on a message in
//! every mailbox the store holds it in (#0076). This opens one session and
//! SELECTs its way down the list.
//!
//! It acts on the server only. The local half of the mutation is the store
//! rows the caller already wrote (`crate::store::write`), so a result here is
//! what tells the queue whether the op is done or owed again.

use anyhow::{anyhow, Result};
use futures::TryStreamExt;
use log::info;

use super::{pool, ImapSession};
use crate::config::ImapConfig;

/// Add one flag to the same message in every one of `mailboxes`, over a single
/// IMAP session (#0076).
///
/// The post-send `\Answered` / `$Forwarded` bookkeeping addresses one source
/// message that the store holds a row for per mailbox (inbox, archive, sent),
/// and the per-message [`super::ops::add_flag_on_server`] opens a fresh
/// connect-TLS-login-SELECT session for each of them. This opens one and
/// SELECTs its way down the list instead.
///
/// Idempotent and not-found tolerant, exactly like the single-mailbox form:
/// `+FLAGS` never clears anything the server already holds, and a mailbox whose
/// SEARCH finds nothing is skipped rather than failed (the copy may have been
/// moved or deleted since). A mailbox that cannot be SELECTed or STOREd is an
/// error for the whole call, so a queued op retries the list; the retry is safe
/// because re-adding a flag that is already set is a no-op on the server.
pub async fn add_flag_in_mailboxes(
    imap_config: &ImapConfig,
    message_id: &str,
    mailboxes: &[String],
    flag: &str,
) -> Result<()> {
    if mailboxes.is_empty() {
        return Ok(());
    }
    let mut pooled = pool::checkout(imap_config)
        .await
        .map_err(|e| anyhow!("IMAP connection failed: {}", e))?;
    let session = pooled.session();

    let mut outcome = Ok(());
    for mailbox in mailboxes {
        if let Err(e) = add_flag_on_session(session, message_id, mailbox, flag).await {
            outcome = Err(e);
            break;
        }
    }
    // A half-finished walk down the mailbox list leaves the session in a state
    // this call cannot vouch for, so it is not handed on (#0041).
    pooled.check(outcome)
}

/// SELECT + SEARCH + UID STORE `+FLAGS` on an existing session, on every copy
/// the search lists.
///
/// The op names a message, not a row, and the bit records that the message
/// was answered or forwarded, which is as true of a duplicate copy as of the
/// first one listed (PERSO-238).
async fn add_flag_on_session(
    session: &mut ImapSession,
    message_id: &str,
    mailbox: &str,
    flag: &str,
) -> Result<()> {
    session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select {}: {}", mailbox, e))?;

    let uids = session
        .uid_search(&crate::imap_client::search::message_id_search_term(
            message_id,
        ))
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;
    if uids.is_empty() {
        info!("Message-ID {message_id} not found in {mailbox}, skipping the {flag} write");
        return Ok(());
    }

    session
        .uid_store(&uid_set(&uids), &format!("+FLAGS ({flag})"))
        .await
        .map_err(|e| anyhow!("Failed to set the {} flag: {}", flag, e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect store response: {}", e))?;
    Ok(())
}

/// A UID sequence set naming every one of `uids`, in ascending order.
fn uid_set(uids: &std::collections::HashSet<u32>) -> String {
    let mut uids: Vec<u32> = uids.iter().copied().collect();
    uids.sort_unstable();
    uids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::uid_set;

    /// The post-send bit lands on every copy the search lists, named in one
    /// sequence set (PERSO-238).
    #[test]
    fn the_post_send_flag_names_every_copy() {
        assert_eq!(
            uid_set(&[289, 285, 288].into_iter().collect()),
            "285,288,289"
        );
        assert_eq!(uid_set(&[7].into_iter().collect()), "7");
    }
}
