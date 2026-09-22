//! The engine half of a draft (#0030, #0126 P5-U10b).
//!
//! Four things a draft file cannot answer on its own: minting an `id:` through
//! the drafts index, building a [`SourceMessage`] out of a stored row, retiring
//! a fully sent draft against its outbox record, and deleting an indexed one.
//! Everything else - the format, the signature sentinels, the frontmatter
//! rewrites, the reply and forward builders, validation, the preview - is
//! [`mp_core::draft`], re-exported whole below, so `crate::draft::…` resolves
//! unchanged.

pub use mp_core::draft::*;

use anyhow::{Context, Result};
use log::info;
use std::fs;
use std::path::PathBuf;

use crate::parse::stable_attachments_dir;
use crate::types::EmailDraft;

/// Frontmatter skeleton for a brand-new empty draft (CLI `mp new` and TUI `n`).
/// `attachments:` is intentionally a bare key: it deserializes to `None` via the
/// serde default, unlike `attachments: []` which yields `Some(vec![])`.
///
/// `subject: ""` is explicit rather than bare, because a file this build writes
/// has to be readable by this build: a bare key is YAML null, and the draft
/// `mp new` had just created was skipped by the index and unreachable through
/// the selector `mp new` printed (#0050). The parser tolerates the bare form
/// too (see [`crate::types::EmailFrontmatter`]), so an agent's draft still
/// indexes; the file we write does not lean on that tolerance.
pub fn new_draft_skeleton(from: &str, date: &str, signature: Option<&str>) -> String {
    new_draft_skeleton_with_id(from, date, &crate::store::drafts::new_id(), signature)
}

/// Build the source of a reply or a forward out of a store row.
///
/// The one assembler both stacks use: `mp reply` / `mp forward` and the TUI's
/// `r` / `R` / `w` all reach a message the same way, so a draft written from
/// the list is the draft the CLI writes for the same selector (#0052).
///
/// `with_attachments` is the forward's extra cost: the attachments are blobs,
/// and a draft's `attachments:` list needs paths, so they are materialised
/// into the stable per-account mirror keyed by Message-ID (#0006) where the
/// draft keeps resolving them after the source row is archived or evicted.
pub fn source_from_row(
    store: &crate::store::Store,
    blobs: &crate::store::BlobStore,
    row: &crate::store::read::MessageRow,
    with_attachments: bool,
) -> Result<SourceMessage> {
    let body = crate::store::read::load_body(store, blobs, row.id).unwrap_or_default();
    let attachments = if with_attachments && row.has_attachments {
        let dest = stable_attachments_dir(
            &crate::config::account_dir(&account_name_of(store)),
            &row.message_id,
        );
        crate::store::read::materialise_attachments(store, blobs, row.id, &dest)?
    } else {
        Vec::new()
    };
    Ok(SourceMessage {
        from: row.from.clone().unwrap_or_default(),
        to: row.to.clone().unwrap_or_default(),
        cc: row.cc.clone(),
        subject: row.subject.clone().unwrap_or_default(),
        message_id: Some(row.message_id.clone()),
        date: row.date_display.clone(),
        body,
        attachments,
        // The quoted HTML companion the file build wrote beside the draft:
        // without it a reply quotes plain text where the sender wrote markup.
        html: crate::store::read::load_html(store, blobs, row.id),
    })
}

/// The account a store belongs to, read back from its own path
/// (`<data>/<account>/store.sqlite3`).
fn account_name_of(store: &crate::store::Store) -> String {
    store
        .path()
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Write the draft `source` produces into the account's drafts directory,
/// mint its `id:`, refresh the index, and hand back the file and the selector
/// that names it.
///
/// The one sequence behind `mp reply`, `mp forward` and the TUI's `r` / `R` /
/// `w` (#0058): build, optional header rewrite, `set_draft_id`, reindex, name
/// the draft. The index is refreshed before the selector is handed out
/// because that selector has to resolve the moment it is printed or shown
/// (#0050's post-write refresh discipline).
///
/// `headers` is the compose wizard's recipient/subject block, applied to the
/// file before the id is minted so the index holds the final content. `None`
/// is the direct reply/forward, which takes the builder's own headers.
///
/// `signature` is the account's Markdown signature (#0099), appended to the
/// reply area above the quoted text so it is visible and editable; `None`
/// leaves no signature block.
pub fn create_draft_from_source(
    account: &str,
    default_from: &str,
    source: &SourceMessage,
    kind: DraftFromSource,
    headers: Option<&DraftRecipientEdit>,
    signature: Option<&str>,
) -> Result<(PathBuf, crate::selector::Selector)> {
    let dir = crate::config::drafts_dir(account);
    let path = match kind {
        DraftFromSource::Reply { all } => {
            create_reply_draft_from(source, all, default_from, Some(&dir), signature)?
        }
        DraftFromSource::Forward => {
            create_forward_draft_from(source, default_from, Some(&dir), signature)?
        }
    };
    if let Some(edit) = headers {
        rewrite_draft_recipients(&path, edit)?;
    }
    let id = crate::store::drafts::new_id();
    set_draft_id(&path, &id)?;
    crate::store::drafts::refresh_account(account)?;
    Ok((path, crate::selector::Selector::for_draft(account, &id)))
}

/// Settle a draft after a finished send: mark it sent, and retire the file
/// when every recipient took it *and* the send left a durable record.
///
/// A send that reached all of its recipients is over. The copy that matters
/// from then on is the server's, which the durable outbox APPENDs to Sent and
/// ingest reads back into the store, so a file left behind in `drafts/` is a
/// second, staler copy of a message that is no longer a draft: it kept showing
/// up in the TUI's Drafts list and in `mp list` with nothing left to do to it.
/// It goes.
///
/// That argument rests entirely on the outbox row existing. A
/// [`SendReport`](crate::send::SendReport) whose `state` is `None` is a
/// submission the store could not be opened for: nothing will APPEND it to
/// Sent and nothing will ingest it back, so the draft file is the last local
/// copy of a message the recipients now hold. Such a send marks the file and
/// keeps it.
///
/// A *partial* send keeps the marked file too, because it is the only thing
/// that still names the recipients who did not get it, and it stays
/// addressable by its selector. Retrying it is a hand edit rather than a
/// command: [`crate::send::build_draft_message`] only builds an `approved`
/// draft, and both [`mark_as_approved`] and [`mark_as_draft`] refuse a file
/// that says `status: sent`, so the user edits `status:` back to `approved`
/// themselves. The recipient lines want trimming to the addresses that failed
/// first, because a re-send delivers to everyone the file still lists,
/// including the ones who already received it.
///
/// [`mark_draft_sent`] runs first either way, so a file that survives carries
/// `status: sent`, and re-running the whole settle is a no-op: marking
/// tolerates a missing file and so does the removal.
///
/// The drafts *index* is the caller's business, as it already was for the
/// status rewrite: every send path either refreshes it or hands back to a
/// reader that does.
pub fn settle_sent_draft(
    draft: &EmailDraft,
    report: &crate::send::SendReport,
    message_id: Option<&str>,
) -> Result<()> {
    mark_draft_sent(draft, message_id)?;

    // A submission with no outbox row behind it has no second copy anywhere:
    // keep the file, whatever the recipients did with it.
    if report.state.is_none() {
        info!(
            "Kept the sent draft {}: the send has no durable record",
            draft.path.display()
        );
        return Ok(());
    }

    // `all_succeeded` is vacuously true for a result with no recipients at
    // all, which is not a send that happened: such a draft keeps its file.
    let result = &report.send_result;
    if !(result.any_succeeded() && result.all_succeeded()) {
        return Ok(());
    }

    match fs::remove_file(&draft.path) {
        Ok(()) => info!("Retired the fully sent draft: {}", draft.path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::debug!(
                "No draft file at {}; nothing to retire",
                draft.path.display()
            );
        }
        Err(e) => {
            return Err(anyhow::Error::from(e))
                .context(format!("retiring the sent draft {}", draft.path.display()))
        }
    }

    Ok(())
}

/// Delete an indexed draft after the two checks a queued or in-flight send
/// needs (#0073). Deleting a draft is local-only: no server round-trip, and the
/// self-healing index rescan drops the row.
///
/// Refuses a draft an active outbox submission still holds (#0063), on any value
/// of `force`: while the row is in `pending_send`/`sent_pending_append` the file
/// is that send's local anchor, and removing it would not stop the send. Refuses
/// an `approved` draft unless `force`, because approved is the queued-send state
/// and dropping it silently loses a message `mp send-approved` would deliver.
///
/// On success the file and its HTML companion are gone.
pub fn delete_indexed_draft(
    store: &crate::store::Store,
    account: &str,
    row: &crate::store::drafts::DraftRow,
    force: bool,
) -> Result<()> {
    let selector = crate::selector::Selector::for_draft(account, &row.id);
    // Both key forms a send might have enqueued this draft under: the indexed
    // `id:` (the normal case) and the `path:` fallback of a file that had no id
    // when it was sent. See `crate::send::draft_key`.
    let keys = [
        format!("id:{}", row.id),
        format!("path:{}", row.path.display()),
    ];
    if let Some((outbox_id, state)) =
        crate::outbox::active_submission_for_draft(store, account, &keys)?
    {
        anyhow::bail!(
            "{selector} is mid-send: outbox row {outbox_id} ({state}) still holds it. \
             Deleting the file would not stop the send; wait for it to finish, or \
             clear the row with `mp outbox`."
        );
    }
    if row.status == "approved" && !force {
        anyhow::bail!(
            "{selector} is approved, a queued send; deleting it drops that send. \
             Re-run with --force, or demote it first with `mp mark-draft`."
        );
    }
    remove_draft_files(&row.path)
}


// ---------------------------------------------------------------------------
// The drafts index, as the wire spells it (#0126, P5-U10e)
// ---------------------------------------------------------------------------
//
// The Drafts mailbox is listed from the index rather than from `messages`, and
// two readers need that listing: `draft.list` / `mailbox.list` in the daemon,
// and the sessionless oracle the TUI's query tests compare a served answer
// against. Both want the same rows in the same wire shape, so the read lives
// here, in the crate that owns the store, and answers in
// `mp_protocol::draft::{DraftEntry, DraftSkip}`.

use mp_protocol::draft::{DraftEntry, DraftSkip};

/// The indexed drafts of one account: the single answer the Drafts list and
/// the sidebar count both read.
///
/// [`Store::open`] rather than [`open_store`](crate::store::open_store), and
/// the refresh is paid here
/// rather than assumed: drafts are local-only files, so an account that has
/// never synced has no store *file* and still has drafts, and a count that
/// opened differently or skipped the refresh would contradict the list it
/// labels.
pub fn indexed_drafts(account: &str) -> (Vec<DraftEntry>, Vec<DraftSkip>) {
    let store = match crate::store::Store::open(crate::config::store_path(account)) {
        Ok(store) => store,
        Err(e) => {
            log::warn!("[drafts] could not open the store for {account}: {e:#}");
            return (Vec::new(), Vec::new());
        }
    };
    let dir = crate::config::drafts_dir(account);
    // The reporting refresh hands back the files it skipped for a parse
    // failure, so the Drafts list can show them as error rows instead of
    // silently dropping them (#0080).
    let skipped: Vec<DraftSkip> = match crate::store::drafts::refresh_reporting(&store, account, &dir) {
        Ok((_, _, skipped)) => skipped.iter().map(skip_to_wire).collect(),
        Err(e) => {
            log::warn!("[drafts] refreshing the index of {account} failed: {e:#}");
            Vec::new()
        }
    };
    match crate::store::drafts::list(&store, account, None) {
        Ok(rows) => (
            rows.iter().map(|row| draft_to_wire(account, row)).collect(),
            skipped,
        ),
        Err(e) => {
            log::warn!("[drafts] listing the index of {account} failed: {e:#}");
            (Vec::new(), skipped)
        }
    }
}

/// One indexed draft row as `draft.list` would have sent it.
///
/// The oracle's half of the equality `src/tui/app/queries_tests.rs` asserts:
/// the daemon's `entry` (`src/daemon/methods/draft.rs`) builds the same value
/// out of the same row, re-parse included, so the sessionless path and the
/// routed one cannot answer differently about the same file.
fn draft_to_wire(account: &str, row: &crate::store::drafts::DraftRow) -> DraftEntry {
    let draft = parse_email_draft(&row.path);
    DraftEntry {
        id: row.id.clone(),
        selector: crate::selector::Selector::for_draft(account, &row.id).to_string(),
        path: row.path.display().to_string(),
        status: row.status.clone(),
        to: row.to.clone(),
        cc: row.cc.clone(),
        subject: draft
            .as_ref()
            .ok()
            .map(|draft| draft.frontmatter.subject.clone()),
        date: row.date.clone(),
        valid: draft.is_ok(),
        ready: draft
            .as_ref()
            .is_ok_and(|draft| validate_draft(draft).is_ok()),
    }
}

/// One skipped file as `draft.list` would have sent it.
fn skip_to_wire(skip: &crate::store::drafts::SkippedDraft) -> DraftSkip {
    DraftSkip {
        path: skip.path.display().to_string(),
        error: skip.error.clone(),
    }
}

/// How many rows the Drafts mailbox holds, for whoever labels it.
///
/// The count is the length of the list, from the same [`indexed_drafts`] call
/// the mailbox load makes, so the sidebar cannot disagree with the mailbox it
/// labels. It includes the parse-skipped error rows, so the badge matches the
/// list even when some files would not parse (#0080).
///
/// It lives here, beside the four other draft operations that need a store,
/// because the daemon's `mailbox.list` labels the Drafts mailbox with it
/// (#0126, P5-U10e): a daemon method reading its answer out of `src/tui/` was
/// the shape P5-U10c-I2 fixed for the agenda, and the TUI is about to become a
/// crate the daemon may not reach into.
pub fn draft_count(account: &str) -> usize {
    let (rows, skipped) = indexed_drafts(account);
    rows.len() + skipped.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EmailFrontmatter, EmailStatus};
    use gray_matter::{engine::YAML, Matter};
    use std::path::Path;

    /// A draft carrying `date:` plus a field no struct models.
    fn draft_with_unknown_fields(dir: &Path, status: &str) -> PathBuf {
        let path = dir.join("draft-20260701-120000-hello.md");
        fs::write(
            &path,
            format!(
                concat!(
                    "---\n",
                    "to: alice@example.com\n",
                    "subject: Hello\n",
                    "status: {}\n",
                    "from: me@example.com\n",
                    "date: 2026-07-01T12:00:00+02:00\n",
                    "x-ticket: PROJ-42\n",
                    "tags:\n",
                    "  - urgent\n",
                    "  - billing\n",
                    "---\n",
                    "\n",
                    "Body stays put.\n",
                ),
                status,
            ),
        )
        .unwrap();
        path
    }

    #[test]
    fn test_new_draft_skeleton_attachments_none() {
        let skeleton =
            new_draft_skeleton("me@example.com", "Thu, 10 Jul 2026 08:00:00 +0000", None);
        let matter = Matter::<YAML>::new();
        let parsed = matter.parse(&skeleton);
        let fm: EmailFrontmatter = parsed.data.unwrap().deserialize().unwrap();
        assert_eq!(fm.attachments, None);
        assert_eq!(fm.status, EmailStatus::Draft);
        assert_eq!(fm.from.as_deref(), Some("me@example.com"));
        assert_eq!(fm.subject, "");
    }

    /// The skeleton parses as written, with no edit first (#0050). It used to
    /// need `subject:` filled in before it could be read at all, which meant
    /// the drafts index skipped the draft `mp new` had just created and the
    /// selector `mp new` printed resolved to nothing.
    #[test]
    fn the_new_draft_skeleton_parses_without_being_edited_first() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("fresh.md");
        fs::write(
            &path,
            new_draft_skeleton("me@example.com", "Thu, 10 Jul 2026 08:00:00 +0000", None),
        )
        .unwrap();

        let draft = parse_email_draft(&path).expect("a draft we just wrote must parse");
        assert_eq!(draft.frontmatter.subject, "");
        // Validation is still the place that says it is not sendable.
        let err = validate_draft(&draft).unwrap_err().to_string();
        assert!(err.contains("No recipients"), "{err}");
    }

    /// Delete removes the `.md` file and the HTML companion a reply carries
    /// beside it, and the index rescan then drops the row (#0073).
    #[test]
    fn deleting_a_draft_removes_the_file_and_its_html_companion() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("drafts");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("note.md");
        std::fs::write(
            &path,
            "---\nid: aaa\nto: a@example.com\nsubject: Hi\nstatus: draft\n---\n\nBody\n",
        )
        .unwrap();
        let companion = path.with_extension("html");
        std::fs::write(&companion, "<p>quoted</p>").unwrap();

        let store = crate::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        crate::store::drafts::refresh(&store, "work", &dir).unwrap();
        let row = crate::store::drafts::find(&store, "work", "aaa").unwrap().unwrap();

        delete_indexed_draft(&store, "work", &row, false).unwrap();
        assert!(!path.exists(), "the draft file is gone");
        assert!(!companion.exists(), "the html companion is gone");

        crate::store::drafts::refresh(&store, "work", &dir).unwrap();
        assert!(crate::store::drafts::find(&store, "work", "aaa").unwrap().is_none());
    }

    /// An approved draft is a queued send: deleting it silently drops the send,
    /// so it is refused without --force and deleted with it (#0073).
    #[test]
    fn an_approved_draft_is_refused_without_force_and_deleted_with_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("drafts");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("queued.md");
        std::fs::write(
            &path,
            "---\nid: bbb\nto: a@example.com\nsubject: Hi\nstatus: approved\n---\n\nBody\n",
        )
        .unwrap();
        let store = crate::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        crate::store::drafts::refresh(&store, "work", &dir).unwrap();
        let row = crate::store::drafts::find(&store, "work", "bbb").unwrap().unwrap();

        let err = delete_indexed_draft(&store, "work", &row, false).unwrap_err().to_string();
        assert!(err.contains("approved"), "{err}");
        assert!(path.exists(), "the refused draft is still on disk");

        delete_indexed_draft(&store, "work", &row, true).unwrap();
        assert!(!path.exists(), "--force deletes it");
    }

    /// One recipient result, as a send path would have recorded it.
    fn recipient(address: &str, success: bool) -> crate::send::RecipientResult {
        crate::send::RecipientResult {
            address: address.to_string(),
            role: crate::send::RecipientRole::To,
            success,
            error: (!success).then(|| "550 no such mailbox".to_string()),
            verdict: if success {
                crate::send::RecipientVerdict::Delivered
            } else {
                crate::send::RecipientVerdict::Rejected
            },
        }
    }

    fn send_result(outcomes: &[(&str, bool)]) -> crate::send::SendResult {
        crate::send::SendResult {
            results: outcomes
                .iter()
                .map(|(addr, ok)| recipient(addr, *ok))
                .collect(),
        }
    }

    /// A finished send that got an outbox row: the durable path, where the
    /// server's copy is the one that survives the file.
    fn durable_report(outcomes: &[(&str, bool)]) -> crate::send::SendReport {
        crate::send::SendReport {
            send_result: send_result(outcomes),
            state: Some(crate::outbox::OutboxState::Done),
            row_id: Some(1),
        }
    }

    /// The same send with the outbox store unopenable: submitted, but with no
    /// record anywhere (`state: None`).
    fn undurable_report(outcomes: &[(&str, bool)]) -> crate::send::SendReport {
        crate::send::SendReport {
            send_result: send_result(outcomes),
            state: None,
            row_id: None,
        }
    }

    /// A send every recipient took retires the draft: the file leaves
    /// `drafts/`, so it stops showing up in the Drafts list and in `mp list`
    /// with nothing left to do to it. The Sent copy is the server's.
    #[test]
    fn a_fully_sent_draft_is_retired_from_the_drafts_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let path = draft_with_unknown_fields(tmp.path(), "approved");
        let companion = path.with_extension("html");
        fs::write(&companion, "<p>quoted</p>").unwrap();
        let draft = parse_email_draft(&path).unwrap();

        settle_sent_draft(
            &draft,
            &durable_report(&[("alice@example.com", true), ("bob@example.com", true)]),
            Some("<abc@example.com>"),
        )
        .unwrap();

        assert!(!path.exists(), "the fully sent draft is gone");
        assert!(!companion.exists(), "and so is its companion HTML");
    }

    /// A send with no durable record keeps the file even when every recipient
    /// took it: the outbox store could not be opened, so nothing will APPEND
    /// the message to Sent and nothing will ingest it back, and deleting the
    /// draft would leave the recipients holding the only copy.
    #[test]
    fn a_fully_sent_draft_with_no_durable_record_keeps_its_marked_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = draft_with_unknown_fields(tmp.path(), "approved");
        let draft = parse_email_draft(&path).unwrap();

        settle_sent_draft(
            &draft,
            &undurable_report(&[("alice@example.com", true), ("bob@example.com", true)]),
            Some("<abc@example.com>"),
        )
        .unwrap();

        assert!(path.exists(), "a send with no durable record keeps the file");
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("status: sent\n"), "{after}");
        assert!(after.contains("message_id: \"<abc@example.com>\"\n"), "{after}");
    }

    /// A partial send keeps the marked file: it is the only thing that still
    /// names the recipients who did not get it, so it stays addressable by its
    /// selector for the retry.
    #[test]
    fn a_partially_sent_draft_keeps_its_marked_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = draft_with_unknown_fields(tmp.path(), "approved");
        let companion = path.with_extension("html");
        fs::write(&companion, "<p>quoted</p>").unwrap();
        let draft = parse_email_draft(&path).unwrap();

        settle_sent_draft(
            &draft,
            &durable_report(&[("alice@example.com", true), ("bob@example.com", false)]),
            Some("<abc@example.com>"),
        )
        .unwrap();

        assert!(path.exists(), "a partial send leaves the draft addressable");
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("status: sent\n"), "{after}");
        assert!(after.contains("message_id: \"<abc@example.com>\"\n"), "{after}");
        // The companion HTML is dead weight once submitted either way: that
        // behaviour belongs to `mark_draft_sent` and is unchanged.
        assert!(!companion.exists());
    }

    /// A send that reached nobody is not a send: nothing is retired. Neither
    /// is the degenerate result with no recipients at all, for which
    /// `all_succeeded` is vacuously true.
    #[test]
    fn a_send_that_reached_nobody_retires_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let failed = draft_with_unknown_fields(tmp.path(), "approved");
        let draft = parse_email_draft(&failed).unwrap();
        settle_sent_draft(&draft, &durable_report(&[("alice@example.com", false)]), None).unwrap();
        assert!(failed.exists());

        settle_sent_draft(&draft, &durable_report(&[]), None).unwrap();
        assert!(failed.exists(), "no recipients is not a completed send");
    }

    /// Retiring twice is a no-op, which is what makes a retried settle safe:
    /// `mark_draft_sent` already tolerates a missing file and the removal does
    /// too.
    #[test]
    fn settling_an_already_retired_draft_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let path = draft_with_unknown_fields(tmp.path(), "approved");
        let draft = parse_email_draft(&path).unwrap();
        let all_good = durable_report(&[("alice@example.com", true)]);

        settle_sent_draft(&draft, &all_good, None).unwrap();
        settle_sent_draft(&draft, &all_good, None).unwrap();

        assert!(!path.exists());
    }

}
