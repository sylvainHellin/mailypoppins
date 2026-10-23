//! The server drafts mirror (#0143): every draft of an account converges with
//! a copy in the account's server Drafts folder, which is where Outlook, Apple
//! Mail, the Proton app and mp on the other machine see it.
//!
//! The draft file stays the truth. A push renders the file into a message and
//! replaces the server copy with it ([`push`]); a pull rewrites the file from a
//! copy that changed on the server. Which copy belongs to which file is the
//! draft's `server_message_id` frontmatter key, minted at the first push and
//! carried by every copy mp writes; Outlook and the Proton app keep it across
//! their own edits (the probe of 2026-10-20). The volatile state, the copy's
//! UID and the hash of the file at the last convergence, lives in the store's
//! [`draft_sync`](crate::store::draft_sync) table, which a store rebuild drops
//! and the reconcile re-derives.
//!
//! A push is owed when a pushable draft's hash differs from its row's, or it
//! has no row: [`push::enqueue_owed`] turns every owed push into a queued
//! [`ServerOp::PushDraft`](crate::ops::ServerOp::PushDraft), which the
//! account's mutation drain runs like a queued archive. The daemon's draft
//! watcher, which opens no store, asks the account's runtime for that drain
//! when a settled save owes one.

pub mod carrier;
pub mod folder;
pub mod holds;
pub mod push;
pub mod reconcile;

use std::path::PathBuf;

use crate::config::{AccountConfig, AuthMethod};
use crate::store::Store;
use crate::types::{EmailDraft, EmailStatus};

/// Whether a draft's approval travels to its server copy and from there to mp
/// on the other machine: yes, as Sylvain decided on 2026-10-23 (#0143).
///
/// The status rides inside the draft file the copy carries
/// ([`carrier`]), never in a header of its own, so it is read only from a
/// copy mp pushed and nobody edited since: the HTML part still ends with
/// mp's mark, the carried file hashes to it and names the copy's own
/// `Message-ID`. A pull or an import takes that status only when the carried
/// source reproduces the pushed file ([`carrier::MpCopy::reproduces_file`]),
/// and every attachment is here or carried by the copy, a file here holding
/// the bytes of the copy's part of the same name; anything else (an edit in
/// Outlook, Apple Mail or the Proton app, a copy whose headers Proton Bridge
/// dropped, an attachment the copy does not carry or carries with other
/// bytes) demotes an approved draft to `status: draft`, since a send must not
/// carry content nobody approved.
/// `false` keeps approval local: every pull and import demotes.
pub const fn approval_travels() -> bool {
    true
}

/// Whether `account`'s drafts are mirrored: `[accounts.drafts] mirror`, on by
/// default, and never on a Graph account, whose half of the mirror is not
/// built.
pub fn mirror_enabled(account: &AccountConfig) -> bool {
    account.drafts.mirror && account.auth_method != AuthMethod::Graph
}

/// The hash a [`draft_sync`](crate::store::draft_sync) row records of a draft
/// file's bytes: SHA-256, in lowercase hex.
///
/// A file whose hash differs from its row's changed locally since the two
/// last converged. The bytes are hashed whole, frontmatter included, so an
/// approval or a recipient edit is a change as much as a body edit is.
pub fn file_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The server folder `account` mirrors its drafts into: the `drafts` role of
/// its mailbox catalogue (#0141), which is the folder the server marks
/// `\Drafts`, the one config names, or the one called `Drafts`; `Drafts`
/// itself for an account whose server was never listed.
pub fn drafts_mailbox(store: &Store, account: &AccountConfig) -> String {
    crate::catalogue::Catalogue::from_store(store, account)
        .role(crate::catalogue::FolderRole::Drafts)
        .map(|folder| folder.server.clone())
        .unwrap_or_else(|| "Drafts".to_string())
}

/// A fresh `Message-ID` for a draft's server copies, under the domain of the
/// account's address, distinct from any a send mints.
pub fn mint_server_message_id(account: &AccountConfig) -> String {
    let domain = account
        .default_from
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim_end_matches('>').trim())
        .filter(|domain| !domain.is_empty())
        .unwrap_or("mailypoppins.invalid");
    let random: [u8; 12] = rand::random();
    let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
    format!("<mp-draft.{hex}@{domain}>")
}

/// One draft file the mirror may act on, read once.
#[derive(Debug)]
pub struct LocalDraft {
    /// The `id:` field; a file without one is not mirrored until the drafts
    /// index mints it one.
    pub id: String,
    pub path: PathBuf,
    /// [`file_hash`] of the bytes the parse read.
    pub hash: String,
    /// The file's bytes, the ones `hash` and `draft` were read from.
    pub bytes: Vec<u8>,
    pub draft: EmailDraft,
}

impl LocalDraft {
    /// Read and parse the file at `path`, `None` when it is gone, does not
    /// parse, or has no `id:`.
    pub fn read(path: PathBuf) -> Option<LocalDraft> {
        let bytes = std::fs::read(&path).ok()?;
        let text = std::str::from_utf8(&bytes).ok()?;
        let draft = crate::draft::parse_email_draft_content(&path, text).ok()?;
        let id = draft
            .frontmatter
            .id
            .clone()
            .filter(|id| !id.trim().is_empty())?;
        Some(LocalDraft {
            id,
            hash: file_hash(&bytes),
            bytes,
            path,
            draft,
        })
    }

    /// Whether the mirror pushes this draft at all: not a conflict copy, which
    /// is never pushed, and not a sent draft, which has left.
    pub fn pushable(&self) -> bool {
        self.draft.frontmatter.conflict_of.is_none()
            && self.draft.frontmatter.status != EmailStatus::Sent
    }
}

/// Every draft at the top level of `account`'s drafts directory that the
/// mirror may act on, in file-name order. A file in `drafts/.retired/` is not
/// one of them.
pub fn local_drafts(account: &str) -> Vec<LocalDraft> {
    let dir = crate::config::drafts_dir(account);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    paths.sort();
    paths.into_iter().filter_map(LocalDraft::read).collect()
}

/// Delete what `account`'s `drafts/.retired/` has held longer than
/// [`RETIRED_KEEP_SECS`](crate::draft::RETIRED_KEEP_SECS), counted from the
/// retirement (#0143, Sylvain's 30 days). Returns how many files went.
pub fn prune_retired(account: &str, now: std::time::SystemTime) -> usize {
    let dir = crate::config::drafts_dir(account).join(crate::draft::RETIRED_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let keep = std::time::Duration::from_secs(crate::draft::RETIRED_KEEP_SECS);
    let mut pruned = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let expired = entry
            .metadata()
            .ok()
            .filter(|meta| meta.is_file())
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > keep);
        if expired && std::fs::remove_file(&path).is_ok() {
            pruned += 1;
        }
    }
    if pruned > 0 {
        log::info!("[drafts] pruned {pruned} retired draft file(s) of {account}");
    }
    pruned
}

/// The draft `id` of `account`, if a file holds it.
pub fn find_local(account: &str, id: &str) -> Option<LocalDraft> {
    local_drafts(account)
        .into_iter()
        .find(|draft| draft.id == id)
}

/// Reconcile `account`'s drafts with its server Drafts folder over IMAP, the
/// pass the daemon runs after every sync (#0143). Nothing on an account that
/// does not mirror; the configuration's credentials are what the sync used.
pub async fn reconcile_account(
    account: &AccountConfig,
) -> anyhow::Result<reconcile::ReconcileReport> {
    if !mirror_enabled(account) {
        return Ok(reconcile::ReconcileReport::default());
    }
    let imap = crate::config::ImapConfig::load(account)?;
    let store = Store::open(crate::config::store_path(&account.name))?;
    let email = crate::config::load_global_config()
        .map(|config| config.email)
        .unwrap_or_default();
    let mirror = push::Mirror {
        store: &store,
        account,
        email: &email,
        now: crate::outbox::unix_now(),
    };
    let mut folder = folder::ImapDraftsFolder::new(imap);
    reconcile::reconcile(&mirror, &mut folder).await
}

#[cfg(test)]
mod tests;
