//! The guard a send of a mirrored draft passes before it submits (#0143).
//!
//! Since an approval travels, mp on the Mac and mp on the home server can
//! both hold the same approved draft, and `mp send-approved` on each would
//! send it twice. The draft's server copy is the token: a send first takes it
//! out of the Drafts folder, and goes ahead only when this session is the one
//! that removed it.
//!
//! - A copy: the draft's `draft_sync` row is forgotten, then the copy is
//!   removed with `UID STORE \Deleted` and `UID EXPUNGE`, and the send goes
//!   ahead only when this session saw the untagged `EXPUNGE` of every copy
//!   ([`DraftsFolder::claim`]).
//! - A claim that fails, or that sees another session remove a copy, has an
//!   unknown outcome: the copy may be gone by this session's hand (a
//!   connection dropped after the `UID EXPUNGE`, another client's plain
//!   `EXPUNGE` taking the copy this one had flagged, or a folder that kept
//!   renumbering while the claim read the copy's sequence number) or by
//!   another machine's send. The send is refused with "outcome unknown: check Sent and Drafts,
//!   then send again or discard".
//! - No copy and a row: the row says this machine last saw the copy and did
//!   not remove it, so the draft was sent or discarded on another machine or
//!   in another client. The send is refused and the file left alone; the
//!   reconcile retires it once the copy has been missing twice. No push puts
//!   it back meanwhile, the approval a send key makes before the send
//!   included ([`super::push::push`]), so the claim never finds a fresh copy
//!   of a draft sent elsewhere.
//! - No copy and no row: this machine's own claim took the copy, and the
//!   send after it failed or its outcome was unknown, or the push never
//!   landed, or a rebuilt store dropped the row. The send goes ahead, since
//!   the reconcile too holds such a draft for one never pushed.
//! - A server that cannot be reached, or that offers no `UIDPLUS`: the send
//!   is refused, since nothing then says the other machine has not sent it.
//!
//! Forgetting the row before the claim is the whole of the bookkeeping: a
//! draft without a row is one the reconcile never retires as copy-gone and
//! the owed-push scan pushes again, so whatever happens after the `STORE`,
//! a crash included, leaves a draft this machine sends on a retry and that
//! shows in Drafts again, never one presented as sent elsewhere. Should the
//! other machine have sent it after all, its reconcile deletes the copy
//! pushed again as a sent draft's and this one's then retires the file.
//!
//! The daemon runs the guard under the account runtime's turn, so no drain or
//! reconcile of this machine runs between the claim and the submission.
//!
//! What remains open: two sends whose `UID FETCH` both precede either one's
//! `UID EXPUNGE`, a window of one round trip, each see the expunge and both
//! go ahead; plain IMAP has no test-and-set to close it without `CONDSTORE`,
//! which neither server offers. A store rebuilt after another machine sent
//! the draft leaves it here with no copy and no row, which this machine
//! sends again, as its owed-push scan would push it again anyway.

use anyhow::{anyhow, bail, Result};
use log::{info, warn};

use super::folder::DraftsFolder;
use super::{drafts_mailbox, mirror_enabled};
use crate::config::AccountConfig;
use crate::store::{draft_sync, Store};
use crate::types::EmailDraft;

/// Whether a send of `draft` from `account` passes the guard at all: the
/// account mirrors its drafts and the draft has been pushed, so a copy on the
/// server names it.
pub fn guards(account: &AccountConfig, draft: &EmailDraft) -> bool {
    mirror_enabled(account) && draft.frontmatter.server_message_id.is_some()
}

/// The refusal when the Drafts folder could not be read or written.
fn unreachable(account: &str, e: anyhow::Error) -> anyhow::Error {
    anyhow!(
        "{account}'s server Drafts folder could not be reached to check that this draft was not \
         already sent from another machine, so nothing was sent; send it again once the server \
         answers ({e:#})"
    )
}

/// The refusal when a claim did not show this session removing every copy.
fn outcome_unknown(mailbox: &str, why: &str) -> anyhow::Error {
    anyhow!(
        "{why}; nothing was sent here and whether it went out from another machine is not \
         known, outcome unknown: check Sent and Drafts in {mailbox}, then send again or discard"
    )
}

/// Log a refused send at WARN, naming the account, the draft and the
/// `Message-ID` of its server copy: the error goes back to the client that
/// asked, and without this line the daemon log kept no trace of it.
fn log_refusal(account: &AccountConfig, draft: &EmailDraft, e: &anyhow::Error) {
    let id = draft.frontmatter.id.as_deref().unwrap_or("(no id)");
    let message_id = draft
        .frontmatter
        .server_message_id
        .as_deref()
        .unwrap_or_default();
    warn!(
        "[drafts] refused the send of {}/{id} ({message_id}): {e:#}",
        account.name
    );
}

/// Claim `draft`'s server copy in `folder` before a send, or refuse the send.
/// `Ok` means the send may go ahead: the draft is not mirrored, this session
/// removed its copy, or an earlier claim of this machine's did. Every refusal
/// is logged at WARN.
pub async fn claim_for_send<F: DraftsFolder>(
    store: &Store,
    account: &AccountConfig,
    folder: &mut F,
    draft: &EmailDraft,
) -> Result<()> {
    if !guards(account, draft) {
        return Ok(());
    }
    let claimed = claim(store, account, folder, draft).await;
    if let Err(e) = &claimed {
        log_refusal(account, draft, e);
    }
    claimed
}

/// [`claim_for_send`] for a draft the guard covers, without the log.
async fn claim<F: DraftsFolder>(
    store: &Store,
    account: &AccountConfig,
    folder: &mut F,
    draft: &EmailDraft,
) -> Result<()> {
    let name = &account.name;
    let message_id = draft
        .frontmatter
        .server_message_id
        .as_deref()
        .unwrap_or_default();
    let mailbox = drafts_mailbox(store, account);
    let info = folder
        .select(&mailbox)
        .await
        .map_err(|e| unreachable(name, e))?;
    if !info.uidplus {
        bail!(
            "{name}'s server does not offer UIDPLUS, so this draft's server copy cannot be \
             claimed before the send and another machine could send it too; nothing was sent \
             (set `[accounts.drafts] mirror = false` for {name} to send without the mirror)"
        );
    }
    let uids = folder
        .uids_of(message_id)
        .await
        .map_err(|e| unreachable(name, e))?;
    let id = draft.frontmatter.id.as_deref();
    if uids.is_empty() {
        let own = match id {
            Some(id) => draft_sync::get(store, name, id)?.is_none(),
            None => false,
        };
        if own {
            info!(
                "[drafts] {name}: {message_id} has no server copy and no row: its copy went \
                 with an earlier claim of this machine's or never landed; sending"
            );
            return Ok(());
        }
        bail!(
            "this draft's server copy is no longer in {mailbox}: it was sent or discarded on \
             another machine or in another client, so it was not sent again; the drafts \
             reconcile retires the file"
        );
    }
    if let Some(id) = id {
        draft_sync::delete(store, name, id)?;
    }
    let claimed = folder.claim(&uids).await.map_err(|e| {
        outcome_unknown(
            &mailbox,
            &format!("the claim of this draft's server copy did not complete ({e:#})"),
        )
    })?;
    if claimed.len() != uids.len() {
        return Err(outcome_unknown(
            &mailbox,
            "another session removed this draft's server copy while this send was claiming it, \
             another machine sending it or another client tidying the folder",
        ));
    }
    info!("[drafts] {name}: claimed the server copy of {message_id} (UIDs {uids:?}) for its send");
    Ok(())
}

/// [`claim_for_send`] against the account's IMAP Drafts folder, as the
/// daemon's send runs it. A configuration that does not load is a server the
/// guard cannot reach, and refuses the send the same way.
pub async fn guard_send(account: &AccountConfig, draft: &EmailDraft) -> Result<()> {
    if !guards(account, draft) {
        return Ok(());
    }
    let opened = crate::config::ImapConfig::load(account)
        .map_err(|e| unreachable(&account.name, e))
        .and_then(|imap| Ok((imap, Store::open(crate::config::store_path(&account.name))?)));
    let (imap, store) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            log_refusal(account, draft, &e);
            return Err(e);
        }
    };
    let mut folder = super::folder::ImapDraftsFolder::new(imap);
    claim_for_send(&store, account, &mut folder, draft).await
}
