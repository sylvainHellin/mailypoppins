//! The push and the delete: the two queued ops of the drafts mirror (#0143),
//! and the scan that says which pushes are owed.
//!
//! A push renders the draft file as it is when the drain runs, APPENDs it to
//! the Drafts folder, reads the new copy's UID back, removes the copy the
//! draft's `draft_sync` row named, and records the new one. APPEND comes before
//! the delete, so a crash between them leaves two copies rather than none.
//! The file is written once in a draft's life, at its first push, to record
//! the `server_message_id` every copy carries; no later push touches it, so an
//! editor holding the file open sees no change.

use anyhow::{anyhow, Context, Result};
use log::{debug, info, warn};

use super::folder::DraftsFolder;
use super::{drafts_mailbox, find_local, local_drafts, mint_server_message_id, mirror_enabled};
use crate::config::{AccountConfig, EmailSettings};
use crate::ops::{NotFoundOnServer, ServerOp};
use crate::store::draft_sync::{self, DraftSyncRow};
use crate::store::Store;

/// What one mirror run works against: the account's store, its
/// configuration, the `[email]` settings the HTML part is rendered with, and
/// the time it records.
pub struct Mirror<'a> {
    pub store: &'a Store,
    pub account: &'a AccountConfig,
    pub email: &'a EmailSettings,
    pub now: i64,
}

/// Run one queued drafts-mirror op against `folder`.
///
/// A [`NotFoundOnServer`] out of a delete is the queue's converged replay. A
/// push of a draft that is gone, does not parse, is not pushable, or already
/// matches its row does nothing and succeeds: the file is the truth, and what
/// it says now is what the next owed push carries. So does a push of a draft
/// whose row names a copy that left the folder with nothing in its place: it
/// was sent or discarded elsewhere, and the reconcile retires it.
pub async fn run<F: DraftsFolder>(
    mirror: &Mirror<'_>,
    folder: &mut F,
    op: &ServerOp,
) -> Result<()> {
    match op {
        ServerOp::PushDraft { draft_id, .. } => push(mirror, folder, draft_id).await,
        ServerOp::DeleteDraft {
            mailbox,
            message_id,
            ..
        } => delete(folder, mailbox, message_id).await,
        other => Err(anyhow!("{} is not a drafts-mirror op", other.kind())),
    }
}

/// Replace the server copy of the draft `draft_id` with the file as it is now.
pub async fn push<F: DraftsFolder>(
    mirror: &Mirror<'_>,
    folder: &mut F,
    draft_id: &str,
) -> Result<()> {
    let account = &mirror.account.name;
    if !mirror_enabled(mirror.account) {
        debug!("[drafts] {account} mirrors no drafts; push of {draft_id} dropped");
        return Ok(());
    }
    let Some(mut local) = find_local(account, draft_id) else {
        debug!("[drafts] {account}/{draft_id} is gone or does not parse; nothing to push");
        return Ok(());
    };
    if !local.pushable() {
        return Ok(());
    }
    let row = draft_sync::get(mirror.store, account, draft_id)?;
    if row
        .as_ref()
        .is_some_and(|row| row.synced_hash == local.hash)
    {
        return Ok(());
    }

    let message_id = match local.draft.frontmatter.server_message_id.clone() {
        Some(message_id) => message_id,
        None => {
            // The draft's one write by the mirror, read back so the hash the
            // row records is the file's as it now is.
            let minted = mint_server_message_id(mirror.account);
            crate::draft::set_draft_server_message_id(&local.path, &minted)
                .with_context(|| format!("recording the server copy of {draft_id}"))?;
            local = super::LocalDraft::read(local.path.clone())
                .ok_or_else(|| anyhow!("{draft_id} did not read back after its first push"))?;
            minted
        }
    };
    crate::pending_ops::note_push_hash(mirror.store, account, draft_id, &local.hash)?;

    let mailbox = drafts_mailbox(mirror.store, mirror.account);
    let info = folder.select(&mailbox).await?;
    // Every copy before this one, which the row names when there is one; the
    // reconcile owns the copies a crash or another machine left beside it.
    let before = folder.uids_of(&message_id).await?;
    let row_current = row
        .as_ref()
        .is_some_and(|row| row.mailbox == mailbox && row.uidvalidity == info.uidvalidity);
    if row_current && before.is_empty() {
        // The copy the row names is gone and nothing replaced it: the draft
        // was sent or discarded on another machine or in another client, or
        // claimed by another machine's send. A push now would put a sent
        // draft back in Drafts, where a send's claim would find it and send
        // it again. The file is left as it is, local edit included, for the
        // reconcile to retire as copy-gone (or to relink to an Apple Mail
        // edit); moved back from `drafts/.retired/` it has no row and is
        // pushed as a new copy.
        info!(
            "[drafts] {account}/{draft_id}: its copy {message_id} is gone from {mailbox}, sent \
             or discarded elsewhere; not pushed again, the reconcile retires it"
        );
        return Ok(());
    }
    let unseen = before
        .iter()
        .any(|uid| !(row_current && row.as_ref().is_some_and(|row| row.uid == *uid)));
    if unseen {
        // A copy the row does not name: an edit in Outlook or a push from the
        // other machine since the last reconcile, or a draft with copies and
        // no row (a rebuilt store, a push that died before its row). A push
        // here would bury it under the local text; the reconcile pulls it or
        // keeps a conflict copy, and a push still owed after it is queued
        // again.
        info!(
            "[drafts] {account}/{draft_id} has a copy this machine has not seen; reconciling \
             instead of pushing"
        );
        super::reconcile::reconcile(mirror, folder).await?;
        return Ok(());
    }
    let raw = crate::send::build_server_draft(
        &local.draft,
        &mirror.account.default_from,
        mirror.email,
        &message_id,
        &local.bytes,
    )?;
    folder.append(&raw).await?;
    let after = folder.uids_of(&message_id).await?;
    let new_uid = after
        .iter()
        .copied()
        .filter(|uid| !before.contains(uid))
        .max()
        .or_else(|| after.last().copied())
        .ok_or_else(|| anyhow!("the copy of {draft_id} was appended and is not in {mailbox}"))?;

    // The copy the row named; any other is the reconcile's.
    let replaced: Vec<u32> = match &row {
        Some(row) if row_current => [row.uid]
            .into_iter()
            .filter(|uid| *uid != new_uid)
            .collect(),
        _ => Vec::new(),
    };
    let mut removed = Vec::new();
    if !replaced.is_empty() {
        match folder.delete(&replaced).await {
            Ok(()) => removed = replaced,
            // The new copy is in and recorded below; the old one is the
            // reconcile's to remove at the next sync.
            Err(e) => {
                warn!("[drafts] could not remove the old copy of {account}/{draft_id}: {e:#}")
            }
        }
    }

    draft_sync::upsert(
        mirror.store,
        &DraftSyncRow {
            account: account.clone(),
            draft_id: draft_id.to_string(),
            mailbox: mailbox.clone(),
            uid: new_uid,
            uidvalidity: info.uidvalidity,
            synced_at: mirror.now,
            synced_hash: local.hash.clone(),
        },
    )?;
    crate::pending_ops::forget_failed_pushes(mirror.store, account, draft_id)?;
    if removed.is_empty() {
        info!("[drafts] pushed {account}/{draft_id} to {mailbox} as UID {new_uid}");
    } else {
        info!(
            "[drafts] pushed {account}/{draft_id} to {mailbox} as UID {new_uid}, replacing UID(s) \
             {removed:?}"
        );
    }
    Ok(())
}

/// Remove every copy carrying `message_id` from `mailbox`; none is a
/// [`NotFoundOnServer`], which the drain treats as converged.
pub async fn delete<F: DraftsFolder>(
    folder: &mut F,
    mailbox: &str,
    message_id: &str,
) -> Result<()> {
    folder.select(mailbox).await?;
    let uids = folder.uids_of(message_id).await?;
    if uids.is_empty() {
        return Err(NotFoundOnServer {
            message_id: message_id.to_string(),
            mailbox: Some(mailbox.to_string()),
        }
        .into());
    }
    folder.delete(&uids).await?;
    info!("[drafts] removed {message_id} from {mailbox} (UIDs {uids:?})");
    Ok(())
}

/// Queue a push for every draft of `account` that owes one: pushable, and its
/// file's hash differs from its row's or it has no row. A draft that already
/// has a push queued is not queued twice, and one whose push for these very
/// bytes was parked `failed` is not queued again until
/// [`crate::pending_ops::PUSH_RETRY_COOL_DOWN_SECS`] have passed; the next
/// edit queues it at once. Returns how many were queued.
///
/// It also prunes the account's retired drafts past their 30 days, mirror or
/// not, since every drain point passes here.
pub fn enqueue_owed(store: &Store, account: &AccountConfig) -> Result<usize> {
    super::prune_retired(&account.name, std::time::SystemTime::now());
    if !mirror_enabled(account) {
        return Ok(0);
    }
    let rows = draft_sync::load(store, &account.name)?;
    let now = crate::outbox::unix_now();
    let mut queued = 0;
    for local in local_drafts(&account.name) {
        if !local.pushable() {
            continue;
        }
        let synced = rows
            .iter()
            .find(|row| row.draft_id == local.id)
            .is_some_and(|row| row.synced_hash == local.hash);
        if synced
            || crate::pending_ops::push_failed_for(
                store,
                &account.name,
                &local.id,
                &local.hash,
                now,
            )?
        {
            continue;
        }
        let op = ServerOp::PushDraft {
            draft_id: local.id.clone(),
            hash: Some(local.hash.clone()),
        };
        if crate::pending_ops::enqueue_draft_op(store, &account.name, &op)?.is_some() {
            queued += 1;
        }
    }
    Ok(queued)
}

/// After a draft left the drafts directory by a discard or a sweep (#0143):
/// forget its `draft_sync` row and, on an account that mirrors, queue the
/// delete of its server copies. `server_message_id` is the draft's, read
/// before it was retired. Returns whether a delete was queued, which is when
/// the caller asks for a drain.
pub fn forget_retired(
    store: &Store,
    account: &AccountConfig,
    draft_id: &str,
    server_message_id: Option<&str>,
) -> Result<bool> {
    let row = draft_sync::get(store, &account.name, draft_id)?;
    draft_sync::delete(store, &account.name, draft_id)?;
    let Some(message_id) = server_message_id.filter(|_| mirror_enabled(account)) else {
        return Ok(false);
    };
    let op = ServerOp::DeleteDraft {
        mailbox: row
            .as_ref()
            .map(|row| row.mailbox.clone())
            .unwrap_or_else(|| drafts_mailbox(store, account)),
        message_id: message_id.to_string(),
        uid: row.as_ref().map(|row| row.uid),
        uidvalidity: row.as_ref().map(|row| row.uidvalidity),
    };
    Ok(crate::pending_ops::enqueue_draft_op(store, &account.name, &op)?.is_some())
}
