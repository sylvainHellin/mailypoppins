//! The guard a send of a mirrored draft passes before it submits (#0143).
//!
//! Since an approval travels, mp on the Mac and mp on the home server can
//! both hold the same approved draft, and `mp send-approved` on each would
//! send it twice. The draft's server copy is the token: a send first takes it
//! out of the Drafts folder, and goes ahead only when this session is the one
//! that removed it.
//!
//! - No copy: the draft was sent or discarded on another machine or in
//!   another client. The send is refused and the file left alone; the
//!   reconcile retires it once the copy has been missing twice.
//! - A copy: it is removed with `UID STORE \Deleted` and `UID EXPUNGE`, and
//!   the send goes ahead only when this session saw the untagged `EXPUNGE`
//!   of every copy ([`DraftsFolder::claim`]). The draft's `draft_sync` row is
//!   then forgotten, so a send that fails afterwards leaves a draft the
//!   owed-push scan pushes again, and the reconcile does not take its missing
//!   copy for a send elsewhere.
//! - A server that cannot be reached, or that offers no `UIDPLUS`: the send
//!   is refused, since nothing then says the other machine has not sent it.
//!
//! The daemon runs the guard under the account runtime's turn, so no drain or
//! reconcile of this machine runs between the claim and the submission.
//!
//! What remains open: two sends whose `UID FETCH` both precede either one's
//! `UID EXPUNGE`, a window of one round trip, each see the expunge and both
//! go ahead; plain IMAP has no test-and-set to close it without `CONDSTORE`,
//! which neither server offers.

use anyhow::{anyhow, bail, Result};
use log::info;

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

/// Claim `draft`'s server copy in `folder` before a send, or refuse the send.
/// `Ok` means the send may go ahead: the draft is not mirrored, or this
/// session removed its copy.
pub async fn claim_for_send<F: DraftsFolder>(
    store: &Store,
    account: &AccountConfig,
    folder: &mut F,
    draft: &EmailDraft,
) -> Result<()> {
    if !guards(account, draft) {
        return Ok(());
    }
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
    if uids.is_empty() {
        bail!(
            "this draft's server copy is no longer in {mailbox}: it was sent or discarded on \
             another machine or in another client, so it was not sent again; the drafts \
             reconcile retires the file"
        );
    }
    let claimed = folder
        .claim(&uids)
        .await
        .map_err(|e| unreachable(name, e))?;
    if claimed.len() != uids.len() {
        bail!(
            "another machine removed this draft's server copy while this send was claiming it: \
             it is being sent or discarded there, so it was not sent here"
        );
    }
    if let Some(id) = draft.frontmatter.id.as_deref() {
        draft_sync::delete(store, name, id)?;
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
    let imap =
        crate::config::ImapConfig::load(account).map_err(|e| unreachable(&account.name, e))?;
    let store = Store::open(crate::config::store_path(&account.name))?;
    let mut folder = super::folder::ImapDraftsFolder::new(imap);
    claim_for_send(&store, account, &mut folder, draft).await
}
