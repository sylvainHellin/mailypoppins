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
//! draft's `server_message_id`. A copy mp wrote carries the draft's file whole
//! in an HTML comment ([`crate::parse::draft_source_comment`]), which survives
//! mp's own APPEND on both servers and no foreign edit: a copy that still has
//! it is exactly a file mp pushed, and pulling it reproduces the file byte for
//! byte; a copy without it was edited in Outlook, Apple Mail or the Proton app
//! and is converted from its HTML.
//!
//! For each local draft with a `server_message_id`:
//!
//! - its row's copy is still there and the newest: nothing, or a push when the
//!   file moved since;
//! - its row's copy is gone and a newer copy is there: a pull when the file
//!   has not moved, a conflict copy and a pull when it has;
//! - its row's copy is still there and a newer one too, which is another
//!   machine pushing over it: the newer copy wins and the local text is kept
//!   as a conflict copy unless the two are the same file;
//! - no row (a rebuilt store, a crash between the APPEND and the row): the
//!   newest copy is the file's if it carries the file, otherwise a conflict;
//! - no copy at all: it was sent or deleted elsewhere, unless a single new
//!   copy with the draft's subject, recipients and reply target appeared,
//!   which is Apple Mail's edit under a new `Message-ID` and becomes the link;
//!   the file is retired otherwise.
//!
//! A copy whose draft was retired is deleted, a copy whose draft was removed
//! by hand is kept in `drafts/.retired/` and deleted, and a copy no file links
//! to is imported: a draft the other machine's mp pushed, or one started in
//! another client.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use log::{info, warn};

use super::folder::{bare_message_id, header_value, DraftsFolder, ServerHead};
use super::push::Mirror;
use super::{drafts_mailbox, file_hash, local_drafts, mirror_enabled, LocalDraft};
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

/// A retired draft, as a tombstone.
struct Tombstone {
    id: Option<String>,
    message_id: Option<String>,
}

fn tombstones(account: &str) -> Vec<Tombstone> {
    let dir = crate::config::drafts_dir(account).join(crate::draft::RETIRED_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .filter_map(|path| crate::draft::parse_email_draft(&path).ok())
        .map(|draft| Tombstone {
            id: draft.frontmatter.id,
            message_id: draft
                .frontmatter
                .server_message_id
                .map(|mid| bare_message_id(&mid).to_string()),
        })
        .collect()
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
    let tombs = tombstones(&account);
    let local_ids: HashSet<&str> = locals.iter().map(|l| l.id.as_str()).collect();
    let linked_mids: HashSet<String> = locals
        .iter()
        .filter_map(|l| l.draft.frontmatter.server_message_id.as_deref())
        .map(|mid| bare_message_id(mid).to_string())
        .collect();
    let mut claimed: HashSet<u32> = HashSet::new();
    let mut doomed: Vec<u32> = Vec::new();

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
                    crate::draft::set_draft_server_message_id(&local.path, &new_mid)?;
                    let relinked = LocalDraft::read(local.path.clone())
                        .context("re-reading a relinked draft")?;
                    // The file changed since the copy only if it had before
                    // the relink: the row's hash is the old file's.
                    let moved = local.hash != row.synced_hash;
                    settle_changed(
                        mirror,
                        folder,
                        &relinked,
                        edit.uid,
                        moved,
                        info.uidvalidity,
                        &mailbox,
                        &mut report,
                    )
                    .await?;
                }
                None => {
                    crate::draft::retire_draft_files(&local.path)?;
                    draft_sync::delete(mirror.store, &account, &local.id)?;
                    info!("[drafts] {account}/{}: its copy is gone; retired", local.id);
                    report.retired += 1;
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
                // Another machine pushed over the copy this one wrote.
                let raw = folder.fetch(newest.uid).await?.unwrap_or_default();
                let same = source_of(&raw).is_some_and(|src| file_hash(&src) == local.hash);
                if !same {
                    write_conflict_copy(local, &mut report)?;
                }
                pull_into(mirror, local, &raw)?;
                record(mirror, local, newest.uid, info.uidvalidity, &mailbox)?;
                report.pulled += 1;
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
                    info.uidvalidity,
                    &mailbox,
                    &mut report,
                )
                .await?;
                settle_older(folder, local, &older, &mut doomed, &mut report).await?;
            }
            None => {
                // No row: a rebuilt store or a crash before the row was
                // written. The newest copy is the file's if it carries it.
                let raw = folder.fetch(newest.uid).await?.unwrap_or_default();
                if source_of(&raw).is_some_and(|src| file_hash(&src) == local.hash) {
                    record(mirror, local, newest.uid, info.uidvalidity, &mailbox)?;
                    report.recovered += 1;
                } else {
                    write_conflict_copy(local, &mut report)?;
                    pull_into(mirror, local, &raw)?;
                    record(mirror, local, newest.uid, info.uidvalidity, &mailbox)?;
                    report.pulled += 1;
                }
                settle_older(folder, local, &older, &mut doomed, &mut report).await?;
            }
        }
    }

    // Rows whose draft is no longer in the directory: retired, or removed by
    // hand. A hand-removed draft's copy may be the only one left, so it is
    // kept in `.retired/` before the copy goes.
    for row in rows.values() {
        if local_ids.contains(row.draft_id.as_str()) {
            continue;
        }
        let retired = tombs
            .iter()
            .any(|t| t.id.as_deref() == Some(row.draft_id.as_str()));
        if !retired && row.mailbox == mailbox && row.uidvalidity == info.uidvalidity {
            if let Some(copy) = copies.iter().find(|c| c.uid == row.uid) {
                if let Some(raw) = folder.fetch(copy.uid).await? {
                    keep_in_retired(mirror, &row.draft_id, &raw)?;
                }
                claimed.insert(copy.uid);
                doomed.push(copy.uid);
            }
        }
        draft_sync::delete(mirror.store, &account, &row.draft_id)?;
    }

    for copy in &copies {
        if claimed.contains(&copy.uid) {
            continue;
        }
        let tombstoned = tombs.iter().any(|t| {
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
        if let Some(id) = imported(mirror, copy, &raw, &local_ids)? {
            claimed.insert(copy.uid);
            record_path(mirror, &id, copy.uid, info.uidvalidity, &mailbox)?;
            report.imported += 1;
        }
    }

    doomed.sort_unstable();
    doomed.dedup();
    if !doomed.is_empty() {
        folder.delete(&doomed).await?;
        report.deleted += doomed.len();
    }
    if report != ReconcileReport::default() {
        info!("[drafts] reconciled {account} against {mailbox}: {report:?}");
    }
    Ok(report)
}

/// The copies of a draft older than the one it is linked to.
///
/// A copy mp wrote (it carries its source) is a leftover of a crash between
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
        if source_of(&raw).is_none() {
            let email = crate::parse::parse_rfc822_to_fetched_email(&raw)
                .context("an older copy does not parse as a message")?;
            let target = conflict_path(local);
            let staging = staging_path(&target);
            std::fs::copy(&local.path, &staging)?;
            apply_foreign(&staging, &email, &raw, Some(&local.draft.body_markdown))?;
            crate::draft::mark_conflict_copy(&staging, &crate::store::drafts::new_id(), &local.id)?;
            std::fs::rename(&staging, &target)?;
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

/// Whether a send of `local` is in flight, which no pull may change under it.
fn in_flight(mirror: &Mirror<'_>, local: &LocalDraft) -> bool {
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
/// when the file has not moved since, a conflict when it has.
#[allow(clippy::too_many_arguments)]
async fn settle_changed<F: DraftsFolder>(
    mirror: &Mirror<'_>,
    folder: &mut F,
    local: &LocalDraft,
    uid: u32,
    moved: bool,
    uidvalidity: u32,
    mailbox: &str,
    report: &mut ReconcileReport,
) -> Result<()> {
    let raw = folder.fetch(uid).await?.unwrap_or_default();
    if moved {
        write_conflict_copy(local, report)?;
    }
    pull_into(mirror, local, &raw)?;
    record(mirror, local, uid, uidvalidity, mailbox)?;
    report.pulled += 1;
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
    let mut candidates = copies.iter().filter(|c| {
        c.uid > since
            && !claimed.contains(&c.uid)
            && c.message_id
                .as_ref()
                .is_none_or(|mid| !linked.contains(mid))
            && c.subject.trim() == fm.subject.trim()
            && c.recipients == recipients
            && c.in_reply_to == reply
    });
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

/// The source file a copy carries, `None` for one edited elsewhere.
fn source_of(raw: &[u8]) -> Option<Vec<u8>> {
    let email = crate::parse::parse_rfc822_to_fetched_email(raw)?;
    crate::parse::draft_source_of(email.html_body.as_deref()?)
}

/// Record that `local`, as its file is now, is the copy `uid`.
fn record(
    mirror: &Mirror<'_>,
    local: &LocalDraft,
    uid: u32,
    uidvalidity: u32,
    mailbox: &str,
) -> Result<()> {
    record_path(mirror, &local.id, uid, uidvalidity, mailbox)
}

/// Record the draft `id`'s file, as it is now, against the copy `uid`.
fn record_path(
    mirror: &Mirror<'_>,
    id: &str,
    uid: u32,
    uidvalidity: u32,
    mailbox: &str,
) -> Result<()> {
    let local = super::find_local(&mirror.account.name, id)
        .with_context(|| format!("{id} was written and does not read back"))?;
    draft_sync::upsert(
        mirror.store,
        &DraftSyncRow {
            account: mirror.account.name.clone(),
            draft_id: id.to_string(),
            mailbox: mailbox.to_string(),
            uid,
            uidvalidity,
            synced_at: mirror.now,
            synced_hash: local.hash,
        },
    )
}

/// Keep the local text of `local` as a conflict copy beside it: a fresh id,
/// `conflict_of` naming the draft, no `server_message_id`.
fn write_conflict_copy(local: &LocalDraft, report: &mut ReconcileReport) -> Result<()> {
    let target = conflict_path(local);
    let staging = staging_path(&target);
    std::fs::copy(&local.path, &staging)
        .with_context(|| format!("copying {} aside", local.path.display()))?;
    crate::draft::mark_conflict_copy(&staging, &crate::store::drafts::new_id(), &local.id)?;
    std::fs::rename(&staging, &target)?;
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

/// A name in the drafts directory the watcher does not read, for a file
/// written whole before it takes its place.
fn staging_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.mp-staging"))
}

/// Rewrite `local` from the copy `raw`, in one rename.
///
/// A copy that carries its source file is that file, with the local draft's
/// id kept; one edited elsewhere keeps the local frontmatter, takes its
/// recipients and subject from the copy's headers, and replaces the body
/// above the quote with the copy's HTML in Markdown, the quote below it
/// staying the local one. Either way the status follows
/// [`super::approval_travels`]: an approved draft is demoted, since a send
/// must not carry content nobody approved.
fn pull_into(mirror: &Mirror<'_>, local: &LocalDraft, raw: &[u8]) -> Result<()> {
    let email = crate::parse::parse_rfc822_to_fetched_email(raw)
        .context("the server copy does not parse as a message")?;
    let staging = staging_path(&local.path);
    let source = email
        .html_body
        .as_deref()
        .and_then(crate::parse::draft_source_of);
    match &source {
        Some(bytes) => {
            std::fs::write(&staging, bytes)?;
            crate::draft::set_draft_id(&staging, &local.id)?;
            if let Some(mid) = &local.draft.frontmatter.server_message_id {
                crate::draft::set_draft_server_message_id(&staging, mid)?;
            }
        }
        None => {
            std::fs::copy(&local.path, &staging)?;
            apply_foreign(&staging, &email, raw, Some(&local.draft.body_markdown))?;
        }
    }
    settle_status(
        &staging,
        local.draft.frontmatter.status.clone(),
        source.is_some(),
    )?;
    attach_parts(mirror, &staging, &email, source.is_some())?;
    std::fs::rename(&staging, &local.path)
        .with_context(|| format!("replacing {}", local.path.display()))?;
    info!(
        "[drafts] pulled {}/{} from the server",
        mirror.account.name, local.id
    );
    Ok(())
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
            to: email.to.clone(),
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
fn settle_status(path: &Path, local: EmailStatus, from_source: bool) -> Result<()> {
    let pulled = crate::draft::parse_email_draft(path)?.frontmatter.status;
    let wanted = if super::approval_travels() && from_source {
        pulled.clone()
    } else {
        match local {
            EmailStatus::Approved => EmailStatus::Draft,
            other => other,
        }
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
fn attach_parts(
    mirror: &Mirror<'_>,
    path: &Path,
    email: &crate::parse::FetchedEmail,
    from_source: bool,
) -> Result<()> {
    let draft = crate::draft::parse_email_draft(path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let entries = draft.frontmatter.attachments.clone().unwrap_or_default();
    let resolves = |entry: &String| {
        crate::send::resolve_attachment_paths(std::slice::from_ref(entry), dir)
            .is_ok_and(|paths| !paths.is_empty())
    };
    if from_source && entries.iter().all(resolves) {
        return Ok(());
    }
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
                    p.file_name().map(|n| n.to_string_lossy().into_owned())
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
        return Ok(());
    }
    for index in (0..entries.len()).rev() {
        crate::draft::remove_draft_attachment(path, index)?;
    }
    for entry in &wanted {
        crate::draft::append_draft_attachment(path, entry)?;
    }
    Ok(())
}

/// Keep the copy of a draft that was removed by hand in `drafts/.retired/`,
/// since it may be the only one left.
fn keep_in_retired(mirror: &Mirror<'_>, id: &str, raw: &[u8]) -> Result<()> {
    let dir = crate::config::drafts_dir(&mirror.account.name).join(crate::draft::RETIRED_DIR);
    crate::config::create_private_dir_all(&dir)?;
    let path = dir.join(format!("{id}.md"));
    let email = crate::parse::parse_rfc822_to_fetched_email(raw);
    match email
        .as_ref()
        .and_then(|e| crate::parse::draft_source_of(e.html_body.as_deref()?))
    {
        Some(bytes) => std::fs::write(&path, bytes)?,
        None => std::fs::write(&path, raw)?,
    }
    info!(
        "[drafts] {}/{id} was removed by hand; its server copy is kept in {}",
        mirror.account.name,
        path.display()
    );
    Ok(())
}

/// Import a copy no file links to, `Some(id)` with the id it was written
/// under, `None` when it is not one to import.
///
/// A copy carrying its source is a draft mp pushed on the other machine: it
/// is written under its own id, unless a file here already holds that id. A
/// copy that carries none was started or last edited in another client and
/// is imported by [`import_foreign`].
fn imported(
    mirror: &Mirror<'_>,
    copy: &Copy,
    raw: &[u8],
    local_ids: &HashSet<&str>,
) -> Result<Option<String>> {
    let email = crate::parse::parse_rfc822_to_fetched_email(raw);
    let source = email
        .as_ref()
        .and_then(|e| crate::parse::draft_source_of(e.html_body.as_deref()?));
    let Some(bytes) = source else {
        return import_foreign(mirror, copy, raw);
    };
    let dir = crate::config::drafts_dir(&mirror.account.name);
    crate::config::create_private_dir_all(&dir)?;
    let staging = dir.join(format!(".import-{}.mp-staging", copy.uid));
    std::fs::write(&staging, &bytes)?;
    let parsed = crate::draft::parse_email_draft(&staging);
    let id = parsed
        .as_ref()
        .ok()
        .and_then(|draft| draft.frontmatter.id.clone())
        .filter(|id| !local_ids.contains(id.as_str()));
    let Some(id) = id else {
        std::fs::remove_file(&staging).ok();
        return Ok(None);
    };
    let mid = format!("<{}>", copy.message_id.clone().unwrap_or_default());
    crate::draft::set_draft_server_message_id(&staging, &mid)?;
    settle_status(&staging, EmailStatus::Draft, true)?;
    if let Some(email) = &email {
        attach_parts(mirror, &staging, email, true)?;
    }
    let target = unique_draft_path(&dir, &crate::parse::slugify_subject(&copy.subject), &id);
    std::fs::rename(&staging, &target)?;
    info!(
        "[drafts] imported {}/{id} from the server, pushed by mp elsewhere",
        mirror.account.name
    );
    Ok(Some(id))
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
fn import_foreign(mirror: &Mirror<'_>, copy: &Copy, raw: &[u8]) -> Result<Option<String>> {
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
            to: email.to.clone(),
            cc: email.cc.clone().unwrap_or_default(),
            bcc: header_value(raw, "Bcc").unwrap_or_default(),
            subject: email.subject.clone(),
        },
    )?;
    crate::draft::set_draft_server_message_id(&staging, &format!("<{message_id}>"))?;
    if let Some(parent) = &copy.in_reply_to {
        crate::draft::set_draft_in_reply_to(&staging, &format!("<{parent}>"))?;
    }
    attach_parts(mirror, &staging, &email, false)?;
    let target = unique_draft_path(&dir, &crate::parse::slugify_subject(&copy.subject), &id);
    std::fs::rename(&staging, &target)?;
    info!(
        "[drafts] imported {}/{id}, a draft started in another client",
        mirror.account.name
    );
    Ok(Some(id))
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
