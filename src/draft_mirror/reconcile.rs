//! The reconcile (#0143): after each sync of an account, the server Drafts
//! folder is compared with the drafts directory, and every difference is
//! settled toward convergence.
//!
//! The folder is read directly rather than ingested into the store: one
//! `SELECT` and the headers of every message, a body only for a copy that has
//! to be pulled or imported. The store keeps no rows for it, so no received
//! list, search or conversation ever shows a server draft beside its file.
//!
//! The link from a file to its copy is the copy's `Message-ID`, which is the
//! draft's `server_message_id`. A copy mp wrote ends its HTML part with a mark
//! holding the hash of the file it was written from, and carries the file
//! itself, its Bcc left out, in headers of its own ([`super::carrier`]). The
//! mark survives mp's own APPEND on both servers and no foreign edit: a copy
//! that still has it is exactly a file mp pushed, and pulling it reproduces
//! that file when its headers travelled; a copy without it was edited in
//! Outlook, Apple Mail or the Proton app and is converted from its HTML.
//!
//! For each local draft with a `server_message_id`:
//!
//! - its row's copy is still there and the newest: nothing, or a push when the
//!   file moved since;
//! - its row's copy is gone and a newer copy is there: a pull when the file
//!   has not moved, a conflict copy and a pull when it has;
//! - its row's copy is still there and a newer one too, which is another
//!   machine pushing over it: the newer copy wins, and the local text is kept
//!   as a conflict copy when the file moved since and is not that copy;
//! - no row (a rebuilt store, a crash between the APPEND and the row): the
//!   newest copy is the file's if it was written from it or renders to it,
//!   otherwise a conflict;
//! - no copy at all: it was sent or deleted elsewhere, unless a single new
//!   copy with the draft's subject, recipients and reply target appeared,
//!   which is Apple Mail's edit under a new `Message-ID` and becomes the link;
//!   the file is retired otherwise, once two reconciles in a row have missed
//!   the copy.
//!
//! A pull never lands over a save it did not read: the file is hashed again
//! just before the rename, and a draft that changed is left for the next
//! reconcile, which sees both sides moved.
//!
//! A copy whose draft was sent or discarded is deleted. A copy whose draft
//! was removed by hand, missing from two reconciles in a row, is kept in
//! `drafts/.retired/` and deleted. A draft retired because its copy went is
//! no tombstone: a copy under its `Message-ID` that comes back brings the
//! draft back. A copy no file links to is imported: a draft the other
//! machine's mp pushed, or one started in another client.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use log::{info, warn};

use super::folder::{bare_message_id, header_value, DraftsFolder, ServerHead};
use super::push::Mirror;
use super::{carrier, drafts_mailbox, file_hash, local_drafts, mirror_enabled, LocalDraft};
use crate::store::draft_sync::{self, DraftSyncRow};
use crate::types::EmailStatus;

/// What one reconcile did, for the log and the tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Drafts rewritten from a copy that changed on the server.
    pub pulled: usize,
    /// Conflict copies written.
    pub conflicts: usize,
    /// Drafts retired because their copy is gone.
    pub retired: usize,
    /// Drafts written from a copy no file linked to.
    pub imported: usize,
    /// Copies removed from the server.
    pub deleted: usize,
    /// Rows written for a draft whose copy already was its file.
    pub recovered: usize,
}

/// One message of the folder, by what its header says.
#[derive(Clone, Debug)]
struct Copy {
    uid: u32,
    message_id: Option<String>,
    marker: Option<String>,
    subject: String,
    recipients: BTreeSet<String>,
    in_reply_to: Option<String>,
}

impl Copy {
    /// The copy `head` lists; every header read here is one of
    /// [`super::folder::HEAD_FIELDS`], the only ones a listing carries.
    fn of(head: &ServerHead) -> Copy {
        let header = &head.header;
        let addresses = |name: &str| -> Vec<String> {
            header_value(header, name)
                .map(|value| {
                    mailparse::addrparse(&value)
                        .map(|list| {
                            list.iter()
                                .flat_map(|addr| match addr {
                                    mailparse::MailAddr::Single(info) => vec![info.addr.clone()],
                                    mailparse::MailAddr::Group(group) => {
                                        group.addrs.iter().map(|a| a.addr.clone()).collect()
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default()
        };
        Copy {
            uid: head.uid,
            message_id: header_value(header, "Message-ID")
                .map(|mid| bare_message_id(&mid).to_string()),
            marker: header_value(header, crate::send::DRAFT_ID_HEADER),
            subject: header_value(header, "Subject").unwrap_or_default(),
            recipients: addresses("To")
                .into_iter()
                .chain(addresses("Cc"))
                .map(|a| a.to_lowercase())
                .collect(),
            in_reply_to: header_value(header, "In-Reply-To")
                .map(|mid| bare_message_id(&mid).to_string()),
        }
    }
}

/// The suffix of a file in `drafts/.retired/` that was retired because its
/// server copy went: no tombstone, and brought back when a copy returns.
const COPY_GONE_SUFFIX: &str = ".copy-gone";

/// The file name in `drafts/.retired/` of the `n`th draft named `stem` that
/// was retired because its copy went: `<stem>.copy-gone.md` for the first,
/// `<stem>.copy-gone-<n>.md` from the second, which [`copy_gone_original`]
/// reads back.
fn copy_gone_name(stem: &str, n: u32) -> String {
    if n <= 1 {
        format!("{stem}{COPY_GONE_SUFFIX}.md")
    } else {
        format!("{stem}{COPY_GONE_SUFFIX}-{n}.md")
    }
}

/// The stem the draft had in `drafts/` when `retired_stem` is the stem of a
/// name [`copy_gone_name`] gave it, `None` for a draft retired for another
/// reason, which is a tombstone.
fn copy_gone_original(retired_stem: &str) -> Option<&str> {
    let (base, rest) = retired_stem.rsplit_once(COPY_GONE_SUFFIX)?;
    let counted = rest
        .strip_prefix('-')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    (rest.is_empty() || counted).then_some(base)
}

/// The absence of a draft's server copy, as [`draft_sync::note_absent`]
/// records it.
const COPY_ABSENT: &str = "copy";

/// The absence of a row's file from `drafts/`.
const FILE_ABSENT: &str = "file";

/// A retired draft.
struct Retired {
    path: PathBuf,
    id: Option<String>,
    message_id: Option<String>,
    /// Retired because its copy went, rather than sent, discarded or removed
    /// by hand.
    copy_gone: bool,
}

fn retired(account: &str) -> Vec<Retired> {
    let dir = crate::config::drafts_dir(account).join(crate::draft::RETIRED_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .filter_map(|path| {
            let draft = crate::draft::parse_email_draft(&path).ok()?;
            let copy_gone = path
                .file_stem()
                .is_some_and(|stem| copy_gone_original(&stem.to_string_lossy()).is_some());
            Some(Retired {
                id: draft.frontmatter.id,
                message_id: draft
                    .frontmatter
                    .server_message_id
                    .map(|mid| bare_message_id(&mid).to_string()),
                copy_gone,
                path,
            })
        })
        .collect()
}

/// The `id:` of every file at the top level of `account`'s drafts directory
/// that a line scan can read, whether its frontmatter parses or not.
///
/// A draft whose YAML is half typed is still a draft: it is missing from
/// [`local_drafts`] until it parses again, and must not read as removed by
/// hand, which would delete its server copy.
fn present_ids(account: &str) -> HashSet<String> {
    let dir = crate::config::drafts_dir(account);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
        .filter_map(|path| std::fs::read(&path).ok())
        .filter_map(|bytes| scanned_id(&String::from_utf8_lossy(&bytes)))
        .collect()
}

/// The `id:` of a draft's text by its lines alone: the first top-level `id:`
/// line of the frontmatter, or of the whole text when no fence closes it.
fn scanned_id(text: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return None;
    }
    lines
        .take_while(|line| line.trim_end() != "---")
        .find_map(|line| line.strip_prefix("id:"))
        .map(|value| value.trim().trim_matches(['"', '\'']).trim().to_string())
        .filter(|id| !id.is_empty())
}

/// Reconcile `mirror`'s account against `folder`.
pub async fn reconcile<F: DraftsFolder>(
    mirror: &Mirror<'_>,
    folder: &mut F,
) -> Result<ReconcileReport> {
    let mut report = ReconcileReport::default();
    if !mirror_enabled(mirror.account) {
        return Ok(report);
    }
    let account = mirror.account.name.clone();
    let mailbox = drafts_mailbox(mirror.store, mirror.account);
    let info = folder.select(&mailbox).await?;
    let copies: Vec<Copy> = folder.heads().await?.iter().map(Copy::of).collect();
    let locals = local_drafts(&account);
    let rows: HashMap<String, DraftSyncRow> = draft_sync::load(mirror.store, &account)?
        .into_iter()
        .map(|row| (row.draft_id.clone(), row))
        .collect();
    let gone = retired(&account);
    let present = present_ids(&account);
    let linked_mids: HashSet<String> = locals
        .iter()
        .filter_map(|l| l.draft.frontmatter.server_message_id.as_deref())
        .map(|mid| bare_message_id(mid).to_string())
        .collect();
    let mut claimed: HashSet<u32> = HashSet::new();
    let mut doomed: Vec<u32> = Vec::new();
    // The absences this reconcile saw; any other is forgotten at the end, so
    // only two sightings in a row act.
    let mut absent: HashSet<String> = HashSet::new();
    let at = Place {
        uidvalidity: info.uidvalidity,
        mailbox: &mailbox,
    };

    for local in &locals {
        let Some(mid) = local.draft.frontmatter.server_message_id.clone() else {
            continue;
        };
        let bare = bare_message_id(&mid).to_string();
        let mut mine: Vec<&Copy> = copies
            .iter()
            .filter(|c| c.message_id.as_deref() == Some(bare.as_str()))
            .collect();
        mine.sort_by_key(|c| c.uid);
        claimed.extend(mine.iter().map(|c| c.uid));
        if local.draft.frontmatter.conflict_of.is_some() {
            continue;
        }
        let row = rows
            .get(&local.id)
            .filter(|row| row.mailbox == mailbox && row.uidvalidity == info.uidvalidity);
        if in_flight(mirror, local) {
            continue;
        }

        let Some(newest) = mine.last().copied() else {
            let Some(row) = row else {
                // Never pushed under this folder, or a push that did not
                // land: the owed-push scan queues it.
                continue;
            };
            match apple_mail_edit(&copies, &claimed, &linked_mids, local, row.uid) {
                Some(edit) => {
                    claimed.insert(edit.uid);
                    let new_mid = format!("<{}>", edit.message_id.clone().unwrap_or_default());
                    // The file changed since the copy only if it had before
                    // the relink: the row's hash is the old file's.
                    let moved = local.hash != row.synced_hash;
                    settle_changed(
                        mirror,
                        folder,
                        local,
                        edit.uid,
                        moved,
                        Some(&new_mid),
                        &at,
                        &mut report,
                    )
                    .await?;
                }
                None => {
                    absent.insert(local.id.clone());
                    if draft_sync::note_absent(
                        mirror.store,
                        &account,
                        &local.id,
                        COPY_ABSENT,
                        info.uidvalidity,
                    )? {
                        retire_copy_gone(&local.path)?;
                        draft_sync::delete(mirror.store, &account, &local.id)?;
                        info!("[drafts] {account}/{}: its copy is gone; retired", local.id);
                        report.retired += 1;
                    }
                }
            }
            continue;
        };

        let older: Vec<u32> = mine
            .iter()
            .map(|c| c.uid)
            .filter(|uid| *uid != newest.uid)
            .collect();
        match row {
            Some(row) if row.uid == newest.uid => {
                // The copy the row names is the newest: what is owed, if
                // anything, is a push, which the owed-push scan queues. Older
                // copies are leftovers of a crash between an APPEND and its
                // delete.
                settle_older(folder, local, &older, &mut doomed, &mut report).await?;
            }
            Some(row) if older.contains(&row.uid) => {
                // Another machine pushed over the copy this one wrote. Its
                // text wins; the local text is a conflict only if it moved
                // since the row and is not what the newer copy holds.
                let raw = folder.fetch(newest.uid).await?.unwrap_or_default();
                let same = carrier::read(&raw).is_some_and(|c| c.file_hash == local.hash);
                let moved = local.hash != row.synced_hash;
                if let Some(hash) =
                    pull_into(mirror, local, &raw, moved && !same, None, &mut report)?
                {
                    record(mirror, &local.id, newest.uid, &at, hash)?;
                    report.pulled += 1;
                }
                settle_older(folder, local, &older, &mut doomed, &mut report).await?;
            }
            Some(row) => {
                let moved = local.hash != row.synced_hash;
                settle_changed(
                    mirror,
                    folder,
                    local,
                    newest.uid,
                    moved,
                    None,
                    &at,
                    &mut report,
                )
                .await?;
                settle_older(folder, local, &older, &mut doomed, &mut report).await?;
            }
            None => {
                // No row: a rebuilt store or a crash before the row was
                // written. The newest copy is the file's if it was written
                // from it, or renders to it.
                let raw = folder.fetch(newest.uid).await?.unwrap_or_default();
                if is_the_file(mirror, local, &raw)? {
                    record(mirror, &local.id, newest.uid, &at, local.hash.clone())?;
                    report.recovered += 1;
                } else if let Some(hash) = pull_into(mirror, local, &raw, true, None, &mut report)?
                {
                    record(mirror, &local.id, newest.uid, &at, hash)?;
                    report.pulled += 1;
                }
                settle_older(folder, local, &older, &mut doomed, &mut report).await?;
            }
        }
    }

    // Rows whose draft is no longer in the directory: retired, or removed by
    // hand. A hand-removed draft's copy may be the only one left, so it is
    // kept in `.retired/` before the copy goes, once two reconciles in a row
    // have missed the file.
    for row in rows.values() {
        if present.contains(row.draft_id.as_str()) {
            continue;
        }
        let tombstoned = gone
            .iter()
            .any(|t| t.id.as_deref() == Some(row.draft_id.as_str()));
        if !tombstoned && row.mailbox == mailbox && row.uidvalidity == info.uidvalidity {
            if let Some(copy) = copies.iter().find(|c| c.uid == row.uid) {
                claimed.insert(copy.uid);
                absent.insert(row.draft_id.clone());
                let again = draft_sync::note_absent(
                    mirror.store,
                    &account,
                    &row.draft_id,
                    FILE_ABSENT,
                    info.uidvalidity,
                )?;
                if !again {
                    continue;
                }
                if let Some(raw) = folder.fetch(copy.uid).await? {
                    keep_in_retired(mirror, &row.draft_id, &raw)?;
                }
                doomed.push(copy.uid);
            }
        }
        draft_sync::delete(mirror.store, &account, &row.draft_id)?;
    }

    for copy in &copies {
        if claimed.contains(&copy.uid) {
            continue;
        }
        // Only a draft sent, discarded or removed by hand is a tombstone; one
        // retired because its copy went comes back with the copy.
        let tombstoned = gone.iter().filter(|t| !t.copy_gone).any(|t| {
            (t.message_id.is_some() && t.message_id == copy.message_id)
                || (t.id.is_some() && t.id == copy.marker)
        });
        if tombstoned {
            claimed.insert(copy.uid);
            doomed.push(copy.uid);
            continue;
        }
        let Some(raw) = folder.fetch(copy.uid).await? else {
            continue;
        };
        let returning = gone.iter().find(|t| {
            t.copy_gone
                && t.message_id.is_some()
                && t.message_id == copy.message_id
                && t.id.as_deref().is_some_and(|id| !present.contains(id))
        });
        if let Some(returning) = returning {
            claimed.insert(copy.uid);
            if let Some(hash) = revive(mirror, returning, &raw, &mut report)? {
                let id = returning.id.clone().unwrap_or_default();
                record(mirror, &id, copy.uid, &at, hash)?;
                report.pulled += 1;
            }
            continue;
        }
        if let Some((id, hash)) = imported(mirror, copy, &raw, &present)? {
            claimed.insert(copy.uid);
            record(mirror, &id, copy.uid, &at, hash)?;
            report.imported += 1;
        }
    }

    draft_sync::keep_absences(mirror.store, &account, &absent)?;
    doomed.sort_unstable();
    doomed.dedup();
    if !doomed.is_empty() {
        folder.delete(&doomed).await?;
        report.deleted += doomed.len();
    }
    if report != ReconcileReport::default() {
        info!("[drafts] reconciled {account} against {mailbox}: {report:?}");
    } else {
        log::debug!(
            "[drafts] {account} is in step with {mailbox}: {} copies, {} drafts",
            copies.len(),
            locals.len()
        );
    }
    Ok(report)
}

/// Where the copies of this reconcile live.
struct Place<'a> {
    uidvalidity: u32,
    mailbox: &'a str,
}

/// The copies of a draft older than the one it is linked to.
///
/// A copy mp wrote (it ends with mp's mark) is a leftover of a crash between
/// an APPEND and its delete, or of a push another copy superseded, and goes.
/// A copy edited elsewhere is text nobody here has seen: it is kept as a
/// conflict copy of the draft before it goes, so nothing typed anywhere is
/// lost.
async fn settle_older<F: DraftsFolder>(
    folder: &mut F,
    local: &LocalDraft,
    older: &[u32],
    doomed: &mut Vec<u32>,
    report: &mut ReconcileReport,
) -> Result<()> {
    for &uid in older {
        let Some(raw) = folder.fetch(uid).await? else {
            continue;
        };
        if carrier::read(&raw).is_none() {
            let email = crate::parse::parse_rfc822_to_fetched_email(&raw)
                .context("an older copy does not parse as a message")?;
            let target = conflict_path(local);
            let staging = staging_path(&target);
            std::fs::write(&staging, &local.bytes)?;
            apply_foreign(&staging, &email, &raw, Some(&local.draft.body_markdown))?;
            crate::draft::mark_conflict_copy(&staging, &crate::store::drafts::new_id(), &local.id)?;
            let target = place_new(&staging, &target)?;
            warn!(
                "[drafts] an older copy of {} edited elsewhere is kept in {}",
                local.id,
                target.display()
            );
            report.conflicts += 1;
        }
        doomed.push(uid);
    }
    Ok(())
}

/// Whether a send of `local` is in flight, which no pull may change under it:
/// an open outbox row, or an armed undo-send hold.
fn in_flight(mirror: &Mirror<'_>, local: &LocalDraft) -> bool {
    let approved = local.draft.frontmatter.status == EmailStatus::Approved;
    if super::holds::is_held(&mirror.account.name, &local.id, approved) {
        return true;
    }
    let keys = [
        format!("id:{}", local.id),
        format!("path:{}", local.path.display()),
    ];
    matches!(
        crate::outbox::active_submission_for_draft(mirror.store, &mirror.account.name, &keys),
        Ok(Some(_))
    )
}

/// The copy that changed under a draft whose row named an older one: pulled
/// when the file has not moved since, a conflict when it has. `relink` is the
/// new `Message-ID` of an Apple Mail edit, written with the pull.
#[allow(clippy::too_many_arguments)]
async fn settle_changed<F: DraftsFolder>(
    mirror: &Mirror<'_>,
    folder: &mut F,
    local: &LocalDraft,
    uid: u32,
    moved: bool,
    relink: Option<&str>,
    at: &Place<'_>,
    report: &mut ReconcileReport,
) -> Result<()> {
    let raw = folder.fetch(uid).await?.unwrap_or_default();
    if let Some(hash) = pull_into(mirror, local, &raw, moved, relink, report)? {
        record(mirror, &local.id, uid, at, hash)?;
        report.pulled += 1;
    }
    Ok(())
}

/// Apple Mail's edit of a draft whose copy is gone: the one copy no file
/// links to, newer than the draft's, with its subject, its recipients and its
/// reply target. None, or more than one, is no link.
fn apple_mail_edit<'c>(
    copies: &'c [Copy],
    claimed: &HashSet<u32>,
    linked: &HashSet<String>,
    local: &LocalDraft,
    since: u32,
) -> Option<&'c Copy> {
    let fm = &local.draft.frontmatter;
    let recipients: BTreeSet<String> = [fm.to.as_deref(), fm.cc.as_deref()]
        .into_iter()
        .flatten()
        .flat_map(crate::send::split_addresses)
        .map(|a| {
            mailparse::addrparse(&a)
                .ok()
                .and_then(|list| match list.iter().next() {
                    Some(mailparse::MailAddr::Single(info)) => Some(info.addr.clone()),
                    _ => None,
                })
                .unwrap_or(a)
                .to_lowercase()
        })
        .collect();
    let reply = fm
        .in_reply_to
        .as_deref()
        .map(|mid| bare_message_id(mid).to_string());
    // A forward imported from Outlook, whose copy names its source in
    // `In-Reply-To`, keeps that source in `forwarded_from:`.
    let forwarded = fm
        .forwarded_from
        .as_deref()
        .map(|mid| bare_message_id(mid).to_string());
    let mut candidates = copies.iter().filter(|c| {
        c.uid > since
            && !claimed.contains(&c.uid)
            && c.message_id
                .as_ref()
                .is_none_or(|mid| !linked.contains(mid))
            && c.subject.trim() == fm.subject.trim()
            && c.recipients == recipients
            && (c.in_reply_to == reply
                || (reply.is_none() && forwarded.is_some() && c.in_reply_to == forwarded))
    });
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

/// Whether the copy `raw` is `local`'s file as it is: written from it, for a
/// copy mp wrote, or rendering to it as a pull would, for a copy edited
/// elsewhere (a draft imported from another client renders to its file).
fn is_the_file(mirror: &Mirror<'_>, local: &LocalDraft, raw: &[u8]) -> Result<bool> {
    if let Some(copy) = carrier::read(raw) {
        return Ok(copy.file_hash == local.hash);
    }
    let Some(email) = crate::parse::parse_rfc822_to_fetched_email(raw) else {
        return Ok(false);
    };
    let staging = staging_path(&local.path);
    std::fs::write(&staging, &local.bytes)?;
    let rendered = apply_foreign(&staging, &email, raw, Some(&local.draft.body_markdown))
        .and_then(|()| attach_parts(mirror, &staging, &email, false))
        .and_then(|_| Ok(std::fs::read(&staging)?));
    std::fs::remove_file(&staging).ok();
    Ok(rendered.is_ok_and(|bytes| file_hash(&bytes) == local.hash))
}

/// Record the draft `id` against the copy `uid`, its file hashing to `hash`:
/// the bytes the reconcile wrote or read, never a re-read that could take in
/// a save made since, which would then never be pushed.
fn record(mirror: &Mirror<'_>, id: &str, uid: u32, at: &Place<'_>, hash: String) -> Result<()> {
    draft_sync::upsert(
        mirror.store,
        &DraftSyncRow {
            account: mirror.account.name.clone(),
            draft_id: id.to_string(),
            mailbox: at.mailbox.to_string(),
            uid,
            uidvalidity: at.uidvalidity,
            synced_at: mirror.now,
            synced_hash: hash,
        },
    )
}

/// Keep `bytes`, the local text of `local`, as a conflict copy beside it: a
/// fresh id, `conflict_of` naming the draft, no `server_message_id`.
fn write_conflict_copy(
    local: &LocalDraft,
    bytes: &[u8],
    report: &mut ReconcileReport,
) -> Result<()> {
    let target = conflict_path(local);
    let staging = staging_path(&target);
    std::fs::write(&staging, bytes)
        .with_context(|| format!("copying {} aside", local.path.display()))?;
    crate::draft::mark_conflict_copy(&staging, &crate::store::drafts::new_id(), &local.id)?;
    let target = place_new(&staging, &target)?;
    warn!(
        "[drafts] {} changed here and on the server; the local text is kept in {}",
        local.id,
        target.display()
    );
    report.conflicts += 1;
    Ok(())
}

/// Where the next conflict copy of `local` goes: `<stem>.conflict.md`, then
/// `<stem>.conflict-2.md` and on, never over one that exists.
fn conflict_path(local: &LocalDraft) -> PathBuf {
    let stem = local
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| local.id.clone());
    let dir = local.path.parent().unwrap_or(Path::new("."));
    let mut target = dir.join(format!("{stem}.conflict.md"));
    let mut n = 2;
    while target.exists() {
        target = dir.join(format!("{stem}.conflict-{n}.md"));
        n += 1;
    }
    target
}

/// Put the file at `staging` at `target`, or at the next free name beside it
/// (`-2`, `-3` before the extension), never over a file that is there
/// ([`place_first_free`]). Returns where it went.
fn place_new(staging: &Path, target: &Path) -> Result<PathBuf> {
    let dir = target.parent().unwrap_or(Path::new("."));
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = target
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "md".to_string());
    place_first_free(staging, dir, |n| {
        if n <= 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        }
    })
}

/// Put the file at `staging` in `dir` under the first name `name` gives,
/// counting from 1, that no file there holds: a link fails rather than
/// replace, where a rename would not. Returns where it went.
fn place_first_free(staging: &Path, dir: &Path, name: impl Fn(u32) -> String) -> Result<PathBuf> {
    let mut n = 1;
    let mut candidate = dir.join(name(n));
    loop {
        match std::fs::hard_link(staging, &candidate) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                n += 1;
                candidate = dir.join(name(n));
            }
            Err(e) => {
                return Err(e).with_context(|| format!("placing {}", candidate.display()));
            }
        }
    }
    std::fs::remove_file(staging).ok();
    Ok(candidate)
}

/// A name in the drafts directory the watcher does not read, for a file
/// written whole before it takes its place.
fn staging_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.mp-staging"))
}

/// Rewrite `local` from the copy `raw`, in one rename, keeping the local text
/// as a conflict copy first when `conflict` says the file moved too.
///
/// A copy whose headers carry its source is that file, with the local
/// draft's id kept; any other keeps the local frontmatter, takes its
/// recipients and subject from the copy's headers, and replaces the body
/// above the quote with the copy's HTML in Markdown, the quote below it
/// staying the local one. The status is the carried file's when the pull
/// writes exactly that file, attachments included, so an approval made on
/// the other machine arrives with it ([`super::approval_travels`]); any
/// other pull demotes an approved draft, since a send must not carry content
/// nobody approved. `relink` is a new `Message-ID` to record with the pull.
///
/// The file is hashed again just before the rename: a save made since the
/// reconcile read it (it waited on the server in between) is never written
/// over. The draft is then left alone, `None`, and the next reconcile sees
/// both sides moved. `Some` is the hash of the bytes written.
fn pull_into(
    mirror: &Mirror<'_>,
    local: &LocalDraft,
    raw: &[u8],
    conflict: bool,
    relink: Option<&str>,
    report: &mut ReconcileReport,
) -> Result<Option<String>> {
    let carried = carrier::read(raw);
    if relink.is_none()
        && carried
            .as_ref()
            .is_some_and(|copy| copy.file_hash == local.hash)
    {
        // The copy was written from this very file: nothing to write.
        return Ok(Some(local.hash.clone()));
    }
    let email = crate::parse::parse_rfc822_to_fetched_email(raw)
        .context("the server copy does not parse as a message")?;
    let staging = staging_path(&local.path);
    let exact = carried
        .as_ref()
        .is_some_and(carrier::MpCopy::reproduces_file);
    let source = carried.and_then(|copy| copy.source);
    match &source {
        Some(bytes) => {
            std::fs::write(&staging, bytes)?;
            crate::draft::set_draft_id(&staging, &local.id)?;
            if let Some(mid) = relink.or(local.draft.frontmatter.server_message_id.as_deref()) {
                crate::draft::set_draft_server_message_id(&staging, mid)?;
            }
        }
        None => {
            std::fs::write(&staging, &local.bytes)?;
            apply_foreign(&staging, &email, raw, Some(&local.draft.body_markdown))?;
            if let Some(mid) = relink {
                crate::draft::set_draft_server_message_id(&staging, mid)?;
            }
        }
    }
    let faithful = attach_parts(mirror, &staging, &email, source.is_some())?;
    settle_status(
        &staging,
        local.draft.frontmatter.status.clone(),
        exact && faithful,
    )?;
    let written = std::fs::read(&staging)?;

    let current = std::fs::read(&local.path).ok();
    if current.as_deref().map(file_hash).as_deref() != Some(local.hash.as_str()) {
        std::fs::remove_file(&staging).ok();
        info!(
            "[drafts] {}/{} changed while the reconcile ran; left for the next one",
            mirror.account.name, local.id
        );
        return Ok(None);
    }
    if conflict {
        write_conflict_copy(local, current.as_deref().unwrap_or_default(), report)?;
    }
    std::fs::rename(&staging, &local.path)
        .with_context(|| format!("replacing {}", local.path.display()))?;
    info!(
        "[drafts] pulled {}/{} from the server",
        mirror.account.name, local.id
    );
    Ok(Some(file_hash(&written)))
}

/// The headers and the body of a copy edited elsewhere, onto the file at
/// `path`: recipients and subject from the headers, the body above the quote
/// from the HTML part (or the plain one), and the local quote kept below.
fn apply_foreign(
    path: &Path,
    email: &crate::parse::FetchedEmail,
    raw: &[u8],
    local_body: Option<&str>,
) -> Result<()> {
    crate::draft::rewrite_draft_recipients(
        path,
        &crate::draft::DraftRecipientEdit {
            to: copy_to(email, raw),
            cc: email.cc.clone().unwrap_or_default(),
            bcc: header_value(raw, "Bcc").unwrap_or_default(),
            subject: email.subject.clone(),
        },
    )?;
    let own = match email.html_body.as_deref() {
        Some(html) => crate::parse::draft_html_to_markdown(html),
        None => email.body_text.trim().to_string(),
    };
    let body = match local_body.and_then(crate::draft::quoted_tail) {
        Some(tail) => format!("{own}\n\n{}", tail.trim_end()),
        None => own,
    };
    crate::draft::replace_draft_body(path, &body)
}

/// The status a pulled or imported draft ends with.
///
/// `as_approved` says the file at `path` is, byte for byte and attachments
/// included, the file mp pushed elsewhere: then, while
/// [`super::approval_travels`], it keeps the status that file had, approval
/// included. Otherwise it keeps `local`, an approval demoted to a draft: the
/// content is not what was approved, or not known to be.
fn settle_status(path: &Path, local: EmailStatus, as_approved: bool) -> Result<()> {
    let pulled = crate::draft::parse_email_draft(path)?.frontmatter.status;
    let wanted = match (
        super::approval_travels() && as_approved,
        pulled.clone(),
        local,
    ) {
        // A sent draft is never pushed, so no copy should say so; one that
        // does is read as an unfinished draft.
        (true, EmailStatus::Sent, _) => EmailStatus::Draft,
        (true, carried, _) => carried,
        (false, _, EmailStatus::Approved) => EmailStatus::Draft,
        (false, _, other) => other,
    };
    if pulled != wanted {
        match wanted {
            EmailStatus::Draft => {
                crate::draft::mark_as_draft(path)?;
            }
            EmailStatus::Approved => {
                crate::draft::mark_as_approved(path)?;
            }
            EmailStatus::Sent => {}
        }
    }
    Ok(())
}

/// The attachment parts of a copy, as entries of the draft at `path`.
///
/// A copy carrying its source keeps the source's entries when every one of
/// them resolves here; otherwise, and for a copy edited elsewhere, each part
/// is an entry: a local entry with the part's file name that resolves stays,
/// and any other part is written under the account's stable attachments
/// directory for the copy's `Message-ID`, where a forward's parts already go.
/// An entry whose part the copy no longer carries is dropped, and its file
/// left in place.
///
/// The answer says, for a copy carrying its source, whether the draft's
/// attachments are still the source's: every entry that resolves here is a
/// file whose bytes are those of the copy's part under the same name, every
/// other entry is a part the copy carries under its file name, and the copy
/// carries no part beyond those. A file here that differs from the part, or
/// an entry the copy does not carry (a file that was missing when the other
/// machine pushed), means the draft is not what was approved there.
fn attach_parts(
    mirror: &Mirror<'_>,
    path: &Path,
    email: &crate::parse::FetchedEmail,
    from_source: bool,
) -> Result<bool> {
    let draft = crate::draft::parse_email_draft(path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let entries = draft.frontmatter.attachments.clone().unwrap_or_default();
    let html = email.html_body.as_deref().unwrap_or_default();
    let parts: Vec<&crate::parse::AttachmentData> = email
        .attachments
        .iter()
        .filter(|part| {
            part.content_id
                .as_deref()
                .is_none_or(|cid| !html.contains(&format!("cid:{}", cid.trim_matches(['<', '>']))))
        })
        .collect();
    // An entry resolves when it names files that are here: a path joined to
    // the directory is not yet a file, and the other machine's paths are
    // mostly not. `resolve_attachment_paths` does not check that.
    let resolved = |entry: &String| {
        crate::send::resolve_attachment_paths(std::slice::from_ref(entry), dir)
            .ok()
            .filter(|paths| !paths.is_empty() && paths.iter().all(|p| p.is_file()))
    };
    let faithful = from_source && attachments_match(&entries, &parts, resolved);
    if faithful && entries.iter().all(|entry| resolved(entry).is_some()) {
        return Ok(true);
    }
    let mid = draft
        .frontmatter
        .server_message_id
        .clone()
        .or_else(|| email.message_id.clone())
        .unwrap_or_else(|| draft.frontmatter.id.clone().unwrap_or_default());
    let stable = crate::parse::stable_attachments_dir(
        &crate::config::account_dir(&mirror.account.name),
        &mid,
    );
    let mut wanted = Vec::new();
    for part in parts {
        let kept = entries.iter().find(|entry| {
            crate::send::resolve_attachment_paths(std::slice::from_ref(*entry), dir)
                .ok()
                .and_then(|paths| paths.into_iter().next())
                .is_some_and(|p| {
                    p.is_file()
                        && p.file_name().map(|n| n.to_string_lossy().into_owned())
                            == Some(part.filename.clone())
                })
        });
        match kept {
            Some(entry) => wanted.push(entry.clone()),
            None => {
                crate::config::create_private_dir_all(&stable)?;
                let file = stable.join(&part.filename);
                std::fs::write(&file, &part.content)?;
                wanted.push(file.display().to_string());
            }
        }
    }
    if wanted == entries {
        return Ok(faithful);
    }
    for index in (0..entries.len()).rev() {
        crate::draft::remove_draft_attachment(path, index)?;
    }
    for entry in &wanted {
        crate::draft::append_draft_attachment(path, entry)?;
    }
    Ok(faithful)
}

/// Whether the attachment `entries` of a carried draft are exactly the
/// `parts` its copy carries: each file an entry resolves to here (`resolved`)
/// matches a part of the same file name byte for byte, each entry that
/// resolves to nothing here matches a part of its file name, and no part is
/// left over.
fn attachments_match(
    entries: &[String],
    parts: &[&crate::parse::AttachmentData],
    resolved: impl Fn(&String) -> Option<Vec<std::path::PathBuf>>,
) -> bool {
    let name_of = |path: &Path| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    // What each entry stands for: a file name, and the bytes when the file
    // is here. Byte-bound items are matched first, so a bare name never
    // takes the part a file here needed.
    let mut wanted: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    for entry in entries {
        match resolved(entry) {
            Some(paths) => {
                for path in paths {
                    let Ok(bytes) = std::fs::read(&path) else {
                        return false;
                    };
                    wanted.push((name_of(&path), Some(bytes)));
                }
            }
            None => wanted.push((name_of(Path::new(entry)), None)),
        }
    }
    if wanted.len() != parts.len() {
        return false;
    }
    wanted.sort_by_key(|(_, bytes)| bytes.is_none());
    let mut used = vec![false; parts.len()];
    wanted.iter().all(|(name, bytes)| {
        let found = parts.iter().enumerate().position(|(at, part)| {
            !used[at]
                && part.filename == *name
                && bytes
                    .as_ref()
                    .is_none_or(|bytes| same_attachment_bytes(bytes, &part.content))
        });
        found.map(|at| used[at] = true).is_some()
    })
}

/// Whether a file here and the part a copy carries hold the same attachment.
///
/// Byte for byte, except for what the transfer does to a part sent as text:
/// lettre sends an ASCII file as `7bit` or quoted-printable, which turns its
/// line ends into CRLF and ends it with one, so a `.txt` or a `.csv` comes
/// back with those line ends. Line ends are no content anyone approves;
/// every other byte counts.
fn same_attachment_bytes(local: &[u8], part: &[u8]) -> bool {
    if local == part {
        return true;
    }
    let normal = |bytes: &[u8]| {
        let mut out = Vec::with_capacity(bytes.len());
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == b'\r' && bytes.get(at + 1) == Some(&b'\n') {
                at += 1;
                continue;
            }
            out.push(bytes[at]);
            at += 1;
        }
        while out.last() == Some(&b'\n') {
            out.pop();
        }
        out
    };
    normal(local) == normal(part)
}

/// Keep the copy of a draft that was removed by hand in `drafts/.retired/`,
/// since it may be the only one left. It is a tombstone, as a discard is.
fn keep_in_retired(mirror: &Mirror<'_>, id: &str, raw: &[u8]) -> Result<()> {
    let dir = crate::config::drafts_dir(&mirror.account.name).join(crate::draft::RETIRED_DIR);
    crate::config::create_private_dir_all(&dir)?;
    let staging = dir.join(format!(".{id}.mp-staging"));
    match carrier::read(raw).and_then(|copy| copy.source) {
        Some(bytes) => std::fs::write(&staging, bytes)?,
        None => std::fs::write(&staging, raw)?,
    }
    let path = place_new(&staging, &dir.join(format!("{id}.md")))?;
    info!(
        "[drafts] {}/{id} was removed by hand; its server copy is kept in {}",
        mirror.account.name,
        path.display()
    );
    Ok(())
}

/// Retire the draft at `path` because its server copy went: into
/// `drafts/.retired/` like any retirement, under a name that says why, so it
/// is no tombstone and a copy that comes back brings it back.
fn retire_copy_gone(path: &Path) -> Result<()> {
    let Some(target) = crate::draft::retire_draft_files(path)? else {
        return Ok(());
    };
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = target.parent().unwrap_or(Path::new("."));
    let renamed = place_first_free(&target, dir, |n| copy_gone_name(&stem, n))?;
    let companion = target.with_extension("html");
    if companion.exists() {
        std::fs::rename(&companion, renamed.with_extension("html")).ok();
    }
    Ok(())
}

/// Bring back the draft `returning`, retired because its copy went, from the
/// copy `raw` that came back under its `Message-ID`: the file moves back into
/// `drafts/` under its old name and takes the copy's text, as a pull would.
/// `Some` is the hash of the bytes written, `None` when the file did not land.
fn revive(
    mirror: &Mirror<'_>,
    returning: &Retired,
    raw: &[u8],
    report: &mut ReconcileReport,
) -> Result<Option<String>> {
    let dir = crate::config::drafts_dir(&mirror.account.name);
    let stem = returning
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = copy_gone_original(&stem).unwrap_or(&stem);
    let back = place_new(&returning.path, &dir.join(format!("{name}.md")))?;
    let companion = returning.path.with_extension("html");
    if companion.exists() {
        std::fs::rename(&companion, back.with_extension("html")).ok();
    }
    let Some(local) = LocalDraft::read(back.clone()) else {
        return Ok(None);
    };
    info!(
        "[drafts] {}/{}: its copy came back; the draft is back in {}",
        mirror.account.name,
        local.id,
        back.display()
    );
    pull_into(mirror, &local, raw, false, None, report)
}

/// Import a copy no file links to, `Some((id, hash))` with the id it was
/// written under and the hash of the bytes written, `None` when it is not one
/// to import.
///
/// A copy whose headers carry its source is a draft mp pushed on the other
/// machine: it is written under its own id, unless a file here already holds
/// that id. Any other copy was started or last edited in another client, or
/// lost its headers on the way (Proton Bridge drops them), and is imported by
/// [`import_foreign`].
fn imported(
    mirror: &Mirror<'_>,
    copy: &Copy,
    raw: &[u8],
    present: &HashSet<String>,
) -> Result<Option<(String, String)>> {
    let carried = carrier::read(raw);
    let exact = carried
        .as_ref()
        .is_some_and(carrier::MpCopy::reproduces_file);
    let Some(bytes) = carried.and_then(|c| c.source) else {
        return import_foreign(mirror, copy, raw);
    };
    let email = crate::parse::parse_rfc822_to_fetched_email(raw);
    let dir = crate::config::drafts_dir(&mirror.account.name);
    crate::config::create_private_dir_all(&dir)?;
    let staging = dir.join(format!(".import-{}.mp-staging", copy.uid));
    std::fs::write(&staging, &bytes)?;
    let parsed = crate::draft::parse_email_draft(&staging);
    let id = parsed
        .as_ref()
        .ok()
        .and_then(|draft| draft.frontmatter.id.clone())
        .filter(|id| !present.contains(id.as_str()));
    let Some(id) = id else {
        std::fs::remove_file(&staging).ok();
        return Ok(None);
    };
    let mid = format!("<{}>", copy.message_id.clone().unwrap_or_default());
    crate::draft::set_draft_server_message_id(&staging, &mid)?;
    let faithful = match &email {
        Some(email) => attach_parts(mirror, &staging, email, true)?,
        None => false,
    };
    settle_status(&staging, EmailStatus::Draft, exact && faithful)?;
    let hash = file_hash(&std::fs::read(&staging)?);
    let target = unique_draft_path(&dir, &crate::parse::slugify_subject(&copy.subject), &id);
    place_new(&staging, &target)?;
    info!(
        "[drafts] imported {}/{id} from the server, pushed by mp elsewhere",
        mirror.account.name
    );
    Ok(Some((id, hash)))
}

/// Import a draft started in Outlook, Apple Mail or the Proton app (slice 2):
/// a new file under a fresh id, linked to the copy by its own `Message-ID`.
///
/// The recipients, the subject and the reply target come from the copy's
/// headers, the body from its HTML in Markdown, with a quoted thread below a
/// quote marker so a later pull keeps it, and the attachments as files. The
/// row records the file as written, so the draft is not pushed until it is
/// edited here, and a draft only read in mp keeps the HTML its client wrote.
/// A copy with no `Message-ID` has nothing to link by and is left alone.
fn import_foreign(
    mirror: &Mirror<'_>,
    copy: &Copy,
    raw: &[u8],
) -> Result<Option<(String, String)>> {
    let Some(message_id) = copy.message_id.clone() else {
        return Ok(None);
    };
    let Some(email) = crate::parse::parse_rfc822_to_fetched_email(raw) else {
        return Ok(None);
    };
    let body = match email.html_body.as_deref() {
        Some(html) => {
            let own = crate::parse::draft_html_to_markdown(html);
            match crate::parse::draft_reply_boundary(html) {
                Some(at) => format!(
                    "{own}\n\n{}\n\n{}",
                    crate::draft::QUOTE_MARKER,
                    crate::parse::html_fragment_to_markdown(&html[at..])
                ),
                None => own,
            }
        }
        None => email.body_text.trim().to_string(),
    };
    let dir = crate::config::drafts_dir(&mirror.account.name);
    crate::config::create_private_dir_all(&dir)?;
    let id = crate::store::drafts::new_id();
    let staging = dir.join(format!(".import-{}.mp-staging", copy.uid));
    std::fs::write(
        &staging,
        crate::draft::new_draft_with_body(
            &mirror.account.default_from,
            &chrono::Utc::now().to_rfc2822(),
            &id,
            &body,
            None,
        ),
    )?;
    crate::draft::rewrite_draft_recipients(
        &staging,
        &crate::draft::DraftRecipientEdit {
            to: copy_to(&email, raw),
            cc: email.cc.clone().unwrap_or_default(),
            bcc: header_value(raw, "Bcc").unwrap_or_default(),
            subject: email.subject.clone(),
        },
    )?;
    crate::draft::set_draft_server_message_id(&staging, &format!("<{message_id}>"))?;
    if let Some(parent) = &copy.in_reply_to {
        // Outlook names the forwarded message in `In-Reply-To` too; the
        // subject is what tells a forward from a reply, and a forward flags
        // its source `$Forwarded` and threads into nothing.
        if is_forward_subject(&copy.subject) {
            crate::draft::set_draft_forwarded_from(&staging, &format!("<{parent}>"))?;
        } else {
            crate::draft::set_draft_in_reply_to(&staging, &format!("<{parent}>"))?;
        }
    }
    attach_parts(mirror, &staging, &email, false)?;
    let hash = file_hash(&std::fs::read(&staging)?);
    let target = unique_draft_path(&dir, &crate::parse::slugify_subject(&copy.subject), &id);
    place_new(&staging, &target)?;
    info!(
        "[drafts] imported {}/{id}, a draft started in another client",
        mirror.account.name
    );
    Ok(Some((id, hash)))
}

/// The `To` of a copy: empty when it has no `To` header, as a draft another
/// client saved before a recipient was chosen has, where the message parser
/// would answer a placeholder.
fn copy_to(email: &crate::parse::FetchedEmail, raw: &[u8]) -> String {
    if header_value(raw, "To").is_some() {
        email.to.clone()
    } else {
        String::new()
    }
}

/// Whether a subject is a forward's: it opens with `FW:`, `Fwd:` or `WG:`
/// (Outlook's German), in any case.
fn is_forward_subject(subject: &str) -> bool {
    let lower = subject.trim_start().to_ascii_lowercase();
    ["fw:", "fwd:", "wg:"]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

/// A draft file name the directory does not hold yet.
fn unique_draft_path(dir: &Path, slug: &str, id: &str) -> PathBuf {
    let date = chrono::Local::now().format("%Y-%m-%d");
    let base = if slug.is_empty() {
        format!("{date}-{id}")
    } else {
        format!("{date}-{slug}")
    };
    let mut path = dir.join(format!("{base}.md"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{base}-{n}.md"));
        n += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::{copy_gone_name, copy_gone_original, scanned_id};

    /// Every name a copy-gone retirement is given reads back as one, to the
    /// stem the draft had, and no other retired name does.
    #[test]
    fn a_copy_gone_name_reads_back_whatever_its_count() {
        for n in 1..=12 {
            let name = copy_gone_name("angebot", n);
            let stem = name.strip_suffix(".md").unwrap();
            assert_eq!(copy_gone_original(stem), Some("angebot"), "{name}");
        }
        assert_eq!(copy_gone_original("angebot"), None);
        assert_eq!(copy_gone_original("angebot-2"), None);
        assert_eq!(copy_gone_original("angebot.copy-gone-x"), None);
        assert_eq!(copy_gone_original("angebot.copy-gone-"), None);
    }

    /// A half-typed frontmatter still names its draft to a line scan.
    #[test]
    fn a_line_scan_reads_the_id_of_a_half_typed_draft() {
        assert_eq!(
            scanned_id("---\nid: \"d1\"\nto: [robin\n---\n\nx\n").as_deref(),
            Some("d1")
        );
        assert_eq!(
            scanned_id("---\nsubject: x\nid: d2\nto: [").as_deref(),
            Some("d2")
        );
        assert_eq!(scanned_id("---\nsubject: x\n---\nid: body\n"), None);
        assert_eq!(scanned_id("no frontmatter\nid: x\n"), None);
    }
}
