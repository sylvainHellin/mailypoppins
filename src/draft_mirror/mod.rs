//! The server drafts mirror (#0143): every draft of an account converges with
//! a copy in the account's server Drafts folder, which is where Outlook, Apple
//! Mail, the Proton app and mp on the other machine see it.
//!
//! The draft file stays the truth. A push renders the file into a message and
//! replaces the server copy with it ([`push`]); a pull rewrites the file from a
//! copy that changed on the server. Which copy belongs to which file is the
//! draft's `server_message_id` frontmatter key, minted when the daemon creates
//! the draft ([`link_new_draft`], PERSO-231), or at the first push for a file
//! written outside mp, and carried by every copy mp writes; Outlook and the
//! Proton app keep it across their own edits (the probe of 2026-10-20). The
//! `Message-ID` also names the draft's id ([`mint_server_message_id`]), so mp
//! on the other machine imports a copy whose server dropped mp's headers
//! (Proton Bridge) under the draft's own id (PERSO-183). The volatile state,
//! the copy's UID and the hash of the file at the last convergence, lives in
//! the store's [`draft_sync`](crate::store::draft_sync) table, which a store
//! rebuild drops and the reconcile re-derives. The row repeats the `Message-ID`, so a file
//! rewritten without the key gets it back before the push, the reconcile or
//! a send reads it ([`restore_server_message_id`]).
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
pub mod send_guard;

use std::path::{Path, PathBuf};

use anyhow::anyhow;

use crate::config::{AccountConfig, AuthMethod};
use crate::store::draft_sync::DraftSyncRow;
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

/// The prefix of the local part of every `Message-ID` the mirror mints.
const MINTED_PREFIX: &str = "mp-draft.";

/// A fresh `Message-ID` for the server copies of the draft `draft_id`, under
/// the domain of the account's address, distinct from any a send mints:
/// `<mp-draft.<24 hex>.<draft id>@<domain>>`.
///
/// The `Message-ID` is the one thing every server keeps of a copy mp wrote:
/// Proton Bridge drops the headers that carry the draft and its id at the
/// APPEND (the probe of 2026-10-20). Naming the id in it lets mp on the other
/// machine import such a copy under the draft's own id ([`draft_id_named`],
/// PERSO-183) rather than as a foreign draft under a fresh one. An id that is
/// not a plain token (letters, digits, `-`, `_`) is left out, and the
/// `Message-ID` then names no draft, as one an earlier build minted.
pub fn mint_server_message_id(account: &AccountConfig, draft_id: &str) -> String {
    let domain = account
        .default_from
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim_end_matches('>').trim())
        .filter(|domain| !domain.is_empty())
        .unwrap_or("mailypoppins.invalid");
    let random: [u8; 12] = rand::random();
    let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
    if is_plain_id(draft_id) {
        format!("<{MINTED_PREFIX}{hex}.{draft_id}@{domain}>")
    } else {
        format!("<{MINTED_PREFIX}{hex}@{domain}>")
    }
}

/// The draft id a `Message-ID` [`mint_server_message_id`] minted names, with
/// or without its angle brackets; `None` for any other `Message-ID`, one an
/// earlier build minted included.
pub fn draft_id_named(message_id: &str) -> Option<&str> {
    let bare = folder::bare_message_id(message_id);
    let (local, _domain) = bare.rsplit_once('@')?;
    let (hex, id) = local.strip_prefix(MINTED_PREFIX)?.split_once('.')?;
    let minted = hex.len() == 24 && hex.bytes().all(|b| b.is_ascii_hexdigit());
    (minted && is_plain_id(id)).then_some(id)
}

/// Whether `id` can stand in a `Message-ID` as it is and be read back whole.
fn is_plain_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Give the draft the daemon just wrote at `path` the `server_message_id` its
/// server copies will carry, when `account` mirrors its drafts (PERSO-231).
///
/// It runs before the path is handed to a client, which opens it in an editor
/// at once. Minted at the first push instead, the key was written under the
/// editor's open buffer seconds later: saving the buffer dropped the key (and
/// had the row restore it under the buffer again), not saving dropped the
/// edits. With the key in the file from creation on, no push writes a draft
/// mp created.
pub fn link_new_draft(account: &AccountConfig, path: &Path) -> anyhow::Result<()> {
    if !mirror_enabled(account) {
        return Ok(());
    }
    let id = crate::draft::parse_email_draft(path)
        .ok()
        .and_then(|draft| draft.frontmatter.id)
        .unwrap_or_default();
    crate::draft::set_draft_server_message_id(path, &mint_server_message_id(account, &id))
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

/// `local` with the `server_message_id` its `row` remembers written back, when
/// the file has none (PERSO-192); `local` itself otherwise.
///
/// A draft rewritten whole without the key (an agent writing a fresh file over
/// the scaffold, a draft from before PERSO-231 or a hand-written file saved
/// from a buffer older than its first push) loses it. Read as a draft never
/// pushed, it would get a fresh `Message-ID` that no copy carries, so the
/// row's copy would read as sent elsewhere: the push and the send refused,
/// the file retired, and the copy imported back as a draft of its own. The row's copy is still the
/// draft's, so the key is put back instead, and the push replaces that copy
/// as it would after any edit.
pub fn restore_server_message_id(
    account: &str,
    local: LocalDraft,
    row: Option<&DraftSyncRow>,
) -> anyhow::Result<LocalDraft> {
    if local.draft.frontmatter.server_message_id.is_some() {
        return Ok(local);
    }
    let Some(message_id) = row.and_then(|row| row.message_id.as_deref()) else {
        return Ok(local);
    };
    crate::draft::set_draft_server_message_id(&local.path, message_id)?;
    log::warn!(
        "[drafts] {account}/{}: its file had lost server_message_id; restored {message_id} \
         from its row",
        local.id
    );
    LocalDraft::read(local.path.clone())
        .ok_or_else(|| anyhow!("{} did not read back after its restore", local.id))
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
