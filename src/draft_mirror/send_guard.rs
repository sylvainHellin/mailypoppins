//! The guard a send of a mirrored draft passes before it submits (#0143).
//!
//! Since an approval travels, mp on the Mac and mp on the home server can
//! both hold the same approved draft, and `mp send-approved` on each would
//! send it twice. The draft's server copy is the token: a send first takes it
//! out of the Drafts folder, and goes ahead only when this session is the one
//! that removed it.
//!
//! - A copy: the draft's `draft_sync` row is marked claiming, then the copy
//!   is removed with `UID STORE \Deleted` and `UID EXPUNGE` (on Graph, a
//!   `permanentDelete` of its version), and the send goes ahead only when
//!   this session saw the untagged `EXPUNGE` of every copy
//!   ([`DraftsFolder::claim`]); the row is then forgotten.
//! - A claim that fails has an unknown outcome: the copy may be gone by this
//!   session's hand (a connection dropped after the `UID EXPUNGE`, a Graph
//!   answer lost, or a folder that kept renumbering while the claim read the
//!   copy's sequence number) or not at all. The send is refused with
//!   "outcome unknown: check Sent and Drafts, then send again or discard",
//!   and the row stays claiming for the retry.
//! - A claim that answers that another session removed a copy first: the
//!   claim settled, most likely by mp on the other machine sending the
//!   draft. The row is put back as synced and the send refused, so a retry
//!   meets the "sent or discarded elsewhere" refusal below rather than
//!   sending the draft a second time. On IMAP the same answer comes from
//!   another client's plain `EXPUNGE` taking the copy this one had flagged,
//!   one round trip's window: that unsent draft is retired as copy-gone, and
//!   moving it back from `drafts/.retired/` pushes it again.
//! - A copy the row does not name, beside or instead of the one it names,
//!   that this machine did not write ([`own_copies`]): the copy changed since
//!   this machine last synced it (an Outlook save, in place on Graph; another
//!   machine's push), whenever that was since the last reconcile. The send is refused with nothing removed
//!   ([`ChangedElsewhere`]) and the daemon runs the reconcile, which pulls
//!   the change; the user reviews it and sends again.
//! - No copy and a row: the row says this machine last saw the copy and did
//!   not remove it, so the draft was sent or discarded on another machine or
//!   in another client. The send is refused and the file left alone; the
//!   reconcile retires it once the copy has been missing twice. No push puts
//!   it back meanwhile, the approval a send key makes before the send
//!   included ([`super::push::push`]), so the claim never finds a fresh copy
//!   of a draft sent elsewhere.
//! - Copies and no row at all (a rebuilt store, a push that never wrote its
//!   row, a row the reconcile let go): only a copy whose mark carries the
//!   file's own hash is this machine's ([`written_from_file`], PERSO-327);
//!   any other refuses the send as [`ChangedElsewhere`], with nothing
//!   removed, and the reconcile recovers the row before the next send.
//! - No copy and a claiming row: an earlier claim of this machine's took the
//!   copy and its answer was lost. The send goes ahead.
//! - No copy and no row: this machine's own claim took the copy and the
//!   send after it failed, or the push never landed, or a rebuilt store
//!   dropped the row. The send goes ahead, since the reconcile too holds such
//!   a draft for one never pushed.
//! - A server that cannot be reached, or that offers no `UIDPLUS`: the send
//!   is refused, since nothing then says the other machine has not sent it.
//! - A copy edited in place while the claim ran (Graph: its `changeKey`
//!   moved between the lookup and the removal) is left where it is, the row
//!   is put back, and the send is refused as [`ChangedElsewhere`]. The
//!   copies this machine wrote beside the row's go first and the row's copy
//!   last, in a claim of its own that an edit or a shortfall on the way
//!   skips, so the row put back always names a copy still in the folder.
//!
//! The row is not forgotten before the claim but marked claiming
//! (`draft_sync.claiming`, PERSO-182), and forgotten once the claim
//! settles. A claim that fails, loses its answer or never ends (a crash, a
//! restart) leaves it claiming, and a retry then checks every copy against
//! it like any row: a copy it does not name that this machine did not write
//! (an Outlook save in place, another machine's push) refuses the send as
//! [`ChangedElsewhere`], and no copy at all is this machine's own earlier
//! claim, so the send goes on. Forgetting the row first, as the guard did
//! until then, left a retry with no row, which claimed whatever copy the
//! folder held under the draft's `Message-ID`, an Outlook save included.
//!
//! The reconcile never retires a draft whose row is claiming as sent
//! elsewhere: it names the copy as synced again when it is still listed (the
//! claim took nothing of it), and lets the row go once two reconciles in a
//! row miss the copy, which leaves the state a settled claim leaves. A draft
//! without a row is one the owed-push scan pushes again, so a failed send
//! shows in Drafts again and is never presented as sent elsewhere. Should
//! the other machine have sent it after all, its reconcile deletes the copy
//! pushed again as a sent draft's and this one's then retires the file.
//!
//! A draft whose file lost its `server_message_id` while its row remembers it
//! gets it back before the send reads it ([`restore_link`], PERSO-192), so
//! the guard is not skipped as for a draft never pushed; a draft with a row
//! whose `Message-ID` is not known yet is refused until the next reconcile
//! learns it.
//!
//! The daemon runs the guard under the account runtime's turn, so no drain or
//! reconcile of this machine runs between the claim and the submission.
//!
//! What remains open: two sends whose `UID FETCH` both precede either one's
//! `UID EXPUNGE`, a window of one round trip, each see the expunge and both
//! go ahead; plain IMAP has no test-and-set to close it without `CONDSTORE`,
//! which neither server offers. The same window holds between a push and a
//! claim on two machines: a push whose `UID SEARCH` still sees the copy
//! another machine's claim then expunges appends a fresh copy, which a later
//! claim here could take. A store rebuilt after another machine sent
//! the draft leaves it here with no copy and no row, which this machine
//! sends again, as its owed-push scan would push it again anyway.

use std::path::Path;

use anyhow::{anyhow, bail, Result};
use log::{info, warn};

use super::folder::DraftsFolder;
use super::{drafts_mailbox, mirror_enabled};
use crate::config::AccountConfig;
use crate::store::{draft_sync, Store};
use crate::types::EmailDraft;

/// Put back the `server_message_id` the file at `path` lost, from the draft's
/// row in `store`, before a send reads the draft (PERSO-192).
///
/// A draft without the key and without a row was never pushed and is left
/// alone. One with a row whose `Message-ID` an earlier build did not record
/// is refused: its copy is on the server, and sending without claiming it
/// would let the other machine send it too.
pub fn restore_link(store: &Store, account: &AccountConfig, path: &Path) -> Result<()> {
    if !mirror_enabled(account) {
        return Ok(());
    }
    let Some(local) = super::LocalDraft::read(path.to_path_buf()) else {
        return Ok(());
    };
    if local.draft.frontmatter.server_message_id.is_some() {
        return Ok(());
    }
    let Some(row) = draft_sync::get(store, &account.name, &local.id)? else {
        return Ok(());
    };
    if row.message_id.is_none() {
        bail!(
            "this draft has a copy in {}'s server Drafts folder but its file lost the \
             server_message_id that names it, so the copy cannot be claimed before the send; \
             nothing was sent, send it again after the next sync",
            account.name
        );
    }
    super::restore_server_message_id(&account.name, local, Some(&row))?;
    Ok(())
}

/// [`restore_link`] against the account's own store, as the daemon's send
/// runs it; a draft that has its key opens no store.
pub fn restore_link_for_send(account: &AccountConfig, path: &Path) -> Result<()> {
    if !mirror_enabled(account)
        || crate::draft::parse_email_draft(path)
            .map_or(true, |draft| draft.frontmatter.server_message_id.is_some())
    {
        return Ok(());
    }
    let store = Store::open(crate::config::store_path(&account.name))?;
    restore_link(&store, account, path)
}

/// Whether a send of `draft` from `account` passes the guard at all: the
/// account mirrors its drafts and the draft has a `server_message_id`, which
/// a draft the daemon created carries before its first push (PERSO-231); with
/// no copy and no row, the claim then lets such a draft go.
pub fn guards(account: &AccountConfig, draft: &EmailDraft) -> bool {
    mirror_enabled(account) && draft.frontmatter.server_message_id.is_some()
}

/// The refusal when the draft's server copy is not the version this machine
/// last synced (PERSO-182): Outlook saved an edit over it, in place on Graph
/// or as a new copy on IMAP, or mp on the other machine pushed over it, at
/// any time since the last reconcile. Claiming it would remove that edit and
/// send the older local text, so nothing is removed and nothing sent; the
/// reconcile pulls the edit, which the daemon asks for at once
/// ([`changed_elsewhere`]).
#[derive(Debug)]
pub struct ChangedElsewhere {
    mailbox: String,
}

impl std::fmt::Display for ChangedElsewhere {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this draft was changed in Outlook or on another machine since this machine last \
             synced it, so nothing was sent; the change is being pulled from {} into the draft: \
             review it, then send again",
            self.mailbox
        )
    }
}

impl std::error::Error for ChangedElsewhere {}

/// Whether `error` is a send refused as [`ChangedElsewhere`], after which the
/// daemon runs the account's drafts reconcile so the edit is pulled.
pub fn changed_elsewhere(error: &anyhow::Error) -> bool {
    error.chain().any(|e| e.is::<ChangedElsewhere>())
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

/// The refusal when another session removed the draft's server copy while
/// this send was claiming it: most likely mp on the other machine sending it.
/// A retry is refused as sent elsewhere, so this one does not say to send
/// again.
fn taken_elsewhere(mailbox: &str) -> anyhow::Error {
    anyhow!(
        "another session removed this draft's server copy while this send was claiming it, \
         another machine sending it or another client tidying the folder; nothing was sent here \
         and the draft is treated as sent or discarded elsewhere: check Sent and Drafts in \
         {mailbox}; the drafts reconcile retires the file, and a draft moved back from \
         drafts/.retired/ is pushed again as a new copy"
    )
}

/// Put `row` back as it was before the claim marked it claiming, once a
/// claim has settled without this machine taking the copy: an edit stopped
/// it, or another session removed the copy first.
fn put_back(store: &Store, row: Option<&draft_sync::DraftSyncRow>) -> Result<()> {
    if let Some(row) = row {
        draft_sync::upsert(
            store,
            &draft_sync::DraftSyncRow {
                claiming: None,
                ..row.clone()
            },
        )?;
    }
    Ok(())
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
    let row = match id {
        Some(id) => draft_sync::get(store, name, id)?,
        None => None,
    };
    let ours = match &row {
        Some(row) => own_copies(folder, row, &uids, &mailbox, info.uidvalidity, draft).await,
        None => written_from_file(folder, &uids, draft).await,
    }
    .map_err(|e| unreachable(name, e))?;
    if !ours {
        return Err(ChangedElsewhere {
            mailbox: mailbox.clone(),
        }
        .into());
    }
    let claiming = row.as_ref().is_some_and(|row| row.claiming.is_some());
    if uids.is_empty() {
        if claiming {
            // A claim of this machine's that did not settle took the copy:
            // its removal went through and its answer was lost.
            if let Some(id) = id {
                draft_sync::delete(store, name, id)?;
            }
            info!(
                "[drafts] {name}: {message_id} has no server copy and its row is claiming it: \
                 an earlier claim of this machine's took it; sending"
            );
            return Ok(());
        }
        let own = id.is_some() && row.is_none();
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
    // The row is marked claiming rather than forgotten until the claim
    // settles: a claim that fails, loses its answer or never ends leaves it
    // so, and a retry still checks every copy against it.
    if let Some(row) = &row {
        draft_sync::upsert(
            store,
            &draft_sync::DraftSyncRow {
                claiming: Some(crate::outbox::unix_now()),
                ..row.clone()
            },
        )?;
    }
    // The copies this machine wrote beside the one the row names go first,
    // and the row's copy last, in a claim of its own: an edit found on the
    // way stops the claim with the row's copy still in the folder, so the row
    // put back names a copy that exists and the reconcile reads the edit as
    // one of an older copy (`DraftsFolder::concurrent_edit`) and keeps both
    // texts.
    let rows_copy = row
        .as_ref()
        .filter(|row| row.mailbox == mailbox && row.uidvalidity == info.uidvalidity)
        .map(|row| row.uid)
        .filter(|uid| uids.contains(uid));
    let extras: Vec<u32> = uids
        .iter()
        .copied()
        .filter(|uid| Some(*uid) != rows_copy)
        .collect();
    for batch in [extras, rows_copy.into_iter().collect::<Vec<u32>>()] {
        if batch.is_empty() {
            continue;
        }
        let claim = folder.claim(&batch).await.map_err(|e| {
            outcome_unknown(
                &mailbox,
                &format!("the claim of this draft's server copy did not complete ({e:#})"),
            )
        })?;
        if !claim.edited.is_empty() {
            // An edit saved in place while this send claimed the copy
            // (Graph): it stays, so the draft is not sent, and its row comes
            // back so a retry before the pull is refused the same way rather
            // than read as this machine's own earlier claim, and so the
            // reconcile pulls the edit as a change of the copy the row names.
            put_back(store, row.as_ref())?;
            return Err(ChangedElsewhere {
                mailbox: mailbox.clone(),
            }
            .into());
        }
        if claim.removed.len() != batch.len() {
            // The claim answered that another session removed a copy first:
            // the claim settled, and not by this machine's hand. The row comes
            // back as synced, so a retry finds no copy and a row that names
            // one and is refused as sent elsewhere, rather than read as this
            // machine's own earlier claim and sent a second time; the
            // reconcile then retires the file.
            put_back(store, row.as_ref())?;
            return Err(taken_elsewhere(&mailbox));
        }
    }
    // Settled: the row is forgotten, so a send that fails from here leaves a
    // draft the owed-push scan pushes again and the reconcile never retires.
    if let Some(id) = id {
        draft_sync::delete(store, name, id)?;
    }
    info!("[drafts] {name}: claimed the server copy of {message_id} (UIDs {uids:?}) for its send");
    Ok(())
}

/// Whether every copy `uids` names beside the one `row` names is this
/// machine's own, so the claim may take them all: a copy written from the
/// file as it is now (a push that died between creating its copy and writing
/// its row), or one mp wrote that is older than the row's copy (a push whose
/// removal of the old copy failed after the row moved on), which the
/// reconcile would delete as a leftover. Any other copy is a change made
/// elsewhere ([`ChangedElsewhere`]).
async fn own_copies<F: DraftsFolder>(
    folder: &mut F,
    row: &draft_sync::DraftSyncRow,
    uids: &[u32],
    mailbox: &str,
    uidvalidity: u32,
    draft: &EmailDraft,
) -> Result<bool> {
    let current = row.mailbox == mailbox && row.uidvalidity == uidvalidity;
    let others: Vec<u32> = uids
        .iter()
        .copied()
        .filter(|uid| !(current && *uid == row.uid))
        .collect();
    if others.is_empty() {
        return Ok(true);
    }
    let file_hash = super::LocalDraft::read(draft.path.clone()).map(|local| local.hash);
    for uid in others {
        let Some(raw) = folder.fetch(uid).await? else {
            // Gone since the lookup: the claim's count tells.
            continue;
        };
        let ours = super::carrier::read(&raw).is_some_and(|copy| {
            Some(&copy.file_hash) == file_hash.as_ref() || (current && uid < row.uid)
        });
        if !ours {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether every copy `uids` names was written by mp from the draft's file as
/// it is now (its mark carries the file's hash): the test a draft with no
/// `draft_sync` row passes before its copies are claimed (PERSO-327).
///
/// With no row (a rebuilt store, a push that never wrote one, a row the
/// reconcile let go) nothing says which copy this machine last saw, so only a
/// copy that is this very text is this machine's to take. Any other, an
/// Outlook save or another machine's push, refuses the send as
/// [`ChangedElsewhere`]: the daemon then runs the reconcile, which recovers
/// the row from the copy, keeping the local text as a conflict copy when
/// the two differ, and the next send passes through the row.
async fn written_from_file<F: DraftsFolder>(
    folder: &mut F,
    uids: &[u32],
    draft: &EmailDraft,
) -> Result<bool> {
    if uids.is_empty() {
        return Ok(true);
    }
    let Some(file_hash) = super::LocalDraft::read(draft.path.clone()).map(|local| local.hash)
    else {
        return Ok(false);
    };
    for &uid in uids {
        let Some(raw) = folder.fetch(uid).await? else {
            // Gone since the lookup: the claim's count tells.
            continue;
        };
        let written = super::carrier::read(&raw).is_some_and(|copy| copy.file_hash == file_hash);
        if !written {
            return Ok(false);
        }
    }
    Ok(true)
}

/// [`claim_for_send`] against the account's Drafts folder, over IMAP or
/// Graph, as the daemon's send runs it. A configuration that does not load is
/// a server the guard cannot reach, and refuses the send the same way.
///
/// On Graph a claim is a `permanentDelete` of exactly the version the lookup
/// found ([`super::graph_folder`]), which this request either removed or
/// did not, so two sends never both claim one copy. An Outlook save in place
/// since the last reconcile is a version the row does not name, and refuses
/// the send ([`ChangedElsewhere`]). One that lands between the lookup and the
/// removal changes the draft's `changeKey`, which the removal reads first and
/// names in `If-Match`: the copy is left, the row the claim forgot is put
/// back, and the send is refused as [`ChangedElsewhere`] too, so the daemon
/// pulls the edit and a retry before that pull is refused the same way. A
/// token the login gate holds as refused is a folder out of reach.
pub async fn guard_send(account: &AccountConfig, draft: &EmailDraft) -> Result<()> {
    if !guards(account, draft) {
        return Ok(());
    }
    let opened = crate::ops::Backend::resolve(account)
        .map_err(|e| unreachable(&account.name, e))
        .and_then(|backend| {
            Ok((
                backend,
                Store::open(crate::config::store_path(&account.name))?,
            ))
        });
    let (backend, store) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            log_refusal(account, draft, &e);
            return Err(e);
        }
    };
    match backend {
        crate::ops::Backend::Imap(imap) => {
            let mut folder = super::folder::ImapDraftsFolder::new(*imap);
            claim_for_send(&store, account, &mut folder, draft).await
        }
        crate::ops::Backend::Graph(graph) => {
            let api = super::graph_folder::LiveGraphDrafts::new(*graph);
            let mut folder =
                super::graph_folder::GraphDraftsFolder::new(&store, &account.name, api);
            claim_for_send(&store, account, &mut folder, draft).await
        }
    }
}
