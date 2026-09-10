//! The TUI's query layer (P5-U4, #0124): the three reads a frame needs, typed
//! in the TUI's own vocabulary and answered by the daemon.
//!
//! The contract is P5-U3's (`src/tui/app/queries_tests.rs`): a [`Queries`]
//! trait with one blocking `call`, implemented for
//! [`Session`](crate::tui::session::Session), three free functions over
//! `&dyn Queries`, and the [`MessageRowDelta`] half of the whole-list transfer
//! `docs/baselines/decisions/list-transfer.md` chose. A trait rather than three
//! methods on `Session` because it makes the layer testable over an in-process
//! `Dispatcher`, so the answers below are pinned against the daemon's real
//! method bodies and not against a JSON mock.
//!
//! # Why the wire row becomes a [`MessageRow`] first
//!
//! A listed row is decoded into the store row it came from and handed to
//! [`entry_from_row`](crate::tui::app::entry_from_row), the same mapper the
//! store-backed listing uses. That is what makes the two paths equal by
//! construction rather than by inspection: every derivation (the display name,
//! the `(no subject)` fallback, `resolve_date`'s two strings, the four flag
//! axes) stays in the one function that already owns it, and a change to it
//! moves both paths together. The two `MessageRow` fields no listing carries,
//! `body_blob` and `thread_id`, are the two an [`EmailEntry`] does not read.
//!
//! The drafts branch does the same through
//! [`entry_from_draft`](crate::tui::app::entry_from_draft) and
//! [`entry_from_skip`](crate::tui::app::entry_from_skip).
//!
//! # The uid index
//!
//! A held list is keyed by `messages.id` (`MessageRef`, #0050) and the daemon
//! removes a row by `message:<account>/<mailbox>/<uid>`, which is the resource
//! its mutation methods already invalidate. Nothing in an [`EmailEntry`] is a
//! uid, so the correspondence has to be remembered where both are seen: every
//! listing and every row replace records `(account, mailbox, uid) -> id` here,
//! and a remove resolves through it. A listing replaces its mailbox's table
//! whole, so the memory is one entry per row of the mailboxes this session has
//! opened, and a uid the table does not know owes a refetch rather than a
//! guess.
//!
//! The alternative was a `uid` field on `EmailEntry`, which is 31 struct
//! literals across seven files, two of them the frozen golden-frame fixtures
//! of P5-U1. The day the TUI becomes a crate (P5-U10) is the day to revisit
//! it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use serde_json::{json, Value};

use mp_protocol::draft::DraftListing;
use mp_protocol::EventEnvelope;

use crate::selector::DRAFTS_MAILBOX;
use crate::store::drafts::{DraftRow, SkippedDraft};
use crate::store::read::MessageRow;
use crate::tui::app::{
    entry_from_draft, entry_from_row, entry_from_skip, mailbox_key, status_for_mailbox, EmailEntry,
    MailboxInfo, MessageRef,
};
use crate::types::MessageFlags;

/// Something that answers a daemon method call and blocks for the answer.
///
/// One method, with exactly the signature
/// [`Session::call`](crate::tui::session::Session::call) already had, so the
/// implementation for a session is a delegation and no second connect path
/// exists. Object safe on purpose: the functions below take `&dyn Queries` so
/// a test can drive them over a dispatcher in the same process.
pub trait Queries {
    /// Call `method` with `params` and hand back its `result`.
    fn call(&self, method: &str, params: Value) -> Result<Value>;
}

impl Queries for crate::tui::session::Session {
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        crate::tui::session::Session::call(self, method, params)
    }
}

impl Queries for crate::tui::session::QueryHandle {
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        crate::tui::session::QueryHandle::call(self, method, params)
    }
}

// ---------------------------------------------------------------------------
// The mailbox listing
// ---------------------------------------------------------------------------

/// One mailbox of one account, newest first: the whole list, in one call.
///
/// The whole-list half of `docs/baselines/decisions/list-transfer.md`. There is
/// no `offset` and no paging parameter: an offset is the option that was not
/// chosen, and the deltas below are what keep the list current afterwards.
///
/// The Drafts mailbox is the one branch that is not `message.list`: a draft is
/// a local file with no `messages` row, so it is `draft.list` instead, and the
/// two answers become the same kind of row here.
pub fn list_emails(q: &dyn Queries, account: &str, mailbox: &str) -> Result<Vec<EmailEntry>> {
    let (method, params) = list_request(account, mailbox);
    let answer = q.call(method, params)?;
    decode_list(account, mailbox, &answer)
}

/// The call [`list_emails`] makes, for a caller that dispatches it itself.
///
/// Split out so the background mailbox load posts the same request the
/// blocking path sends, rather than a second spelling of it.
pub fn list_request(account: &str, mailbox: &str) -> (&'static str, Value) {
    if mailbox == DRAFTS_MAILBOX {
        ("draft.list", json!({"account": account, "status": null}))
    } else {
        (
            "message.list",
            json!({"account": account, "mailbox": mailbox, "limit": null}),
        )
    }
}

/// The rows of a [`list_request`] answer, whichever of the two it was.
pub fn decode_list(account: &str, mailbox: &str, answer: &Value) -> Result<Vec<EmailEntry>> {
    if mailbox == DRAFTS_MAILBOX {
        decode_drafts(answer)
    } else {
        Ok(decode_messages(account, mailbox, answer))
    }
}

/// The `messages` array of a `message.list` answer, as list rows.
fn decode_messages(account: &str, mailbox: &str, answer: &Value) -> Vec<EmailEntry> {
    let rows: Vec<MessageRow> = answer["messages"]
        .as_array()
        .map(|rows| rows.iter().map(|row| row_from_wire(row, mailbox)).collect())
        .unwrap_or_default();
    remember_mailbox(account, mailbox, &rows);
    let status = status_for_mailbox(mailbox);
    rows.into_iter()
        .map(|row| entry_from_row(row, &status))
        .collect()
}

/// A `draft.list` answer as list rows, in the order the Drafts mailbox shows
/// them: the files that would not parse first (#0080), then the index's own
/// order (`mtime DESC, id ASC`).
fn decode_drafts(answer: &Value) -> Result<Vec<EmailEntry>> {
    let listing: DraftListing = serde_json::from_value(answer.clone())?;
    let skipped = listing.skipped.into_iter().map(|skip| {
        entry_from_skip(SkippedDraft {
            path: PathBuf::from(skip.path),
            error: skip.error,
        })
    });
    let rows = listing.drafts.into_iter().map(|row| {
        entry_from_draft(DraftRow {
            id: row.id,
            // The three columns the index keeps for itself and no row reads:
            // the display stem, the file size and the mtime the daemon already
            // sorted by.
            slug: String::new(),
            path: PathBuf::from(row.path),
            mtime: 0,
            size: 0,
            status: row.status,
            to: row.to,
            cc: row.cc,
            subject: row.subject,
            date: row.date,
            snippet: None,
        })
    });
    Ok(skipped.chain(rows).collect())
}

/// One listed row as the store row it was read from.
///
/// `from`, `to`, `subject` and `date_display` are flattened to `""` on the
/// wire and become `None` again here; `cc`, `reply_to` and `bcc` travel
/// nullable, because the header pane prints each of them only when the message
/// carried one and an empty header is not an absent one.
fn row_from_wire(row: &Value, mailbox: &str) -> MessageRow {
    let flattened = |key: &str| {
        row[key]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let nullable = |key: &str| row[key].as_str().map(str::to_string);
    let flag = |key: &str| row["flags"][key].as_bool().unwrap_or(false);
    MessageRow {
        id: row["id"].as_i64().unwrap_or_default(),
        mailbox: mailbox.to_string(),
        uid: row["uid"].as_i64().unwrap_or_default(),
        message_id: row["message_id"].as_str().unwrap_or_default().to_string(),
        from: flattened("from"),
        to: flattened("to"),
        cc: nullable("cc"),
        reply_to: nullable("reply_to"),
        bcc: nullable("bcc"),
        subject: flattened("subject"),
        date_display: flattened("date_display"),
        flags: Some(
            MessageFlags {
                seen: flag("seen"),
                answered: flag("answered"),
                forwarded: flag("forwarded"),
                flagged: flag("flagged"),
            }
            .to_flag_string(),
        ),
        has_attachments: row["has_attachments"].as_bool().unwrap_or(false),
        // Neither is carried by a listing and neither is read by a list row:
        // the body is the preview's one blob, and the thread is the
        // conversation overlay's own query.
        body_blob: None,
        thread_id: None,
        is_invite: row["is_invite"].as_bool().unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// The sidebar counts
// ---------------------------------------------------------------------------

/// The per-mailbox totals the sidebar prints, index-aligned with `mailboxes`.
///
/// One `mailbox.list`, whose hierarchy is derived from `build_mailboxes` and is
/// therefore the sidebar's own: a mailbox the daemon does not name counts zero
/// and keeps its slot rather than shifting every count after it. The Drafts
/// total comes from the draft index on the daemon's side, the same exception
/// the store-backed count makes.
pub fn mailbox_counts(
    q: &dyn Queries,
    account: &str,
    mailboxes: &[MailboxInfo],
) -> Result<Vec<usize>> {
    let answer = q.call("mailbox.list", counts_params(account))?;
    Ok(decode_counts(mailboxes, &answer))
}

/// The params [`mailbox_counts`] sends.
pub fn counts_params(account: &str) -> Value {
    json!({"account": account})
}

/// A `mailbox.list` answer as the sidebar's count column.
pub fn decode_counts(mailboxes: &[MailboxInfo], answer: &Value) -> Vec<usize> {
    let mut totals: HashMap<&str, usize> = HashMap::new();
    if let Some(rows) = answer["mailboxes"].as_array() {
        for row in rows {
            if let Some(slug) = row["slug"].as_str() {
                totals.insert(slug, row["total"].as_u64().unwrap_or(0) as usize);
            }
        }
    }
    mailboxes
        .iter()
        .map(|mb| totals.get(mailbox_key(mb).as_str()).copied().unwrap_or(0))
        .collect()
}

// ---------------------------------------------------------------------------
// The preview body
// ---------------------------------------------------------------------------

/// The stored body of one row, for the preview memo.
///
/// Addressed by `row_id`, which is the `id` the listing carried: the preview
/// holds a [`MessageRef`] and nothing else (#0050), and re-deriving a
/// `"<mailbox>/<uid>"` on every cursor move would make the client carry a
/// second identity for the same row.
///
/// A refusal is an empty preview and a line in the log, which is what the
/// store-backed path does with a stale reference and with a store it could not
/// open: the pane goes blank and the log says why. The `Err` arm is therefore
/// unreachable today and is kept because a body that is a transport failure
/// rather than a missing row is a distinction a later unit may want to make.
pub fn message_body(q: &dyn Queries, account: &str, msg: MessageRef) -> Result<Option<String>> {
    let params = json!({"account": account, "row_id": msg.row_id(), "body": true});
    match q.call("message.get", params) {
        Ok(answer) => Ok(answer["body"].as_str().map(str::to_string)),
        Err(e) => {
            log::warn!("[queries] {msg} of {account} has no body to preview: {e:#}");
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Row deltas
// ---------------------------------------------------------------------------

/// One change to a held mailbox list, decoded from one event.
///
/// The three shapes the daemon publishes, in its own vocabulary rather than the
/// decision's prose: `message.row` for an inserted or updated row, and
/// `state.remove` / `state.invalidate` over the `message:` and `mailbox:`
/// resources the mutation methods and the count changes already name.
#[derive(Debug, Clone)]
pub enum MessageRowDelta {
    /// One row, as a fresh listing would carry it.
    Replace {
        /// The account it belongs to.
        account: String,
        /// The mailbox it belongs to.
        mailbox: String,
        /// The row itself, boxed because it is much the largest variant.
        entry: Box<EmailEntry>,
    },
    /// One row is gone, named the way the daemon names it.
    Remove {
        /// The account it belonged to.
        account: String,
        /// The mailbox it belonged to.
        mailbox: String,
        /// Its uid, which the uid index turns back into a row id.
        uid: i64,
    },
    /// A whole listing went stale and owes a `message.list`.
    Invalidate {
        /// The account whose listing it is.
        account: String,
        /// The mailbox whose listing it is.
        mailbox: String,
    },
}

impl MessageRowDelta {
    /// Decode one event, or `None` for an event that is not about a row.
    ///
    /// An unrelated kind is not a delta and may not become one: letting a sync
    /// tick or a draft change decode into something applicable would let it
    /// silently rewrite a message list.
    pub fn decode(event: &EventEnvelope) -> Option<MessageRowDelta> {
        match event.kind.as_str() {
            "message.row" => {
                let account = event.payload["account"].as_str()?.to_string();
                let mailbox = event.payload["mailbox"].as_str()?.to_string();
                let row = row_from_wire(&event.payload["message"], &mailbox);
                remember_row(&account, &mailbox, &row);
                let entry = entry_from_row(row, &status_for_mailbox(&mailbox));
                Some(MessageRowDelta::Replace {
                    account,
                    mailbox,
                    entry: Box::new(entry),
                })
            }
            "state.remove" => {
                let (account, mailbox, uid) = message_resource(&event.payload)?;
                Some(MessageRowDelta::Remove {
                    account,
                    mailbox,
                    uid,
                })
            }
            "state.invalidate" => {
                // The counts scope is the sidebar's, not the list's: a hundred
                // count changes for one mailbox must not each refetch the open
                // list.
                if event.payload["scope"]["query"].as_str() == Some("counts") {
                    return None;
                }
                let (account, mailbox) = mailbox_resource(&event.payload)?;
                Some(MessageRowDelta::Invalidate { account, mailbox })
            }
            _ => None,
        }
    }

    /// The mailbox this delta is about.
    fn mailbox(&self) -> &str {
        match self {
            MessageRowDelta::Replace { mailbox, .. }
            | MessageRowDelta::Remove { mailbox, .. }
            | MessageRowDelta::Invalidate { mailbox, .. } => mailbox,
        }
    }
}

/// Fold one delta into a held list, answering whether nothing is owed.
///
/// `true` means the list is current again, `false` means the caller must
/// re-issue `message.list`. A delta about another mailbox folds as a no-op and
/// answers `true`: the sidebar's other mailboxes move constantly, and
/// refetching the open list on each of them would put back exactly the
/// per-event whole-list transfer the deltas exist to avoid.
pub fn apply_row_delta(held: &mut Vec<EmailEntry>, mailbox: &str, delta: &MessageRowDelta) -> bool {
    if delta.mailbox() != mailbox {
        return true;
    }
    match delta {
        MessageRowDelta::Replace { entry, .. } => {
            match entry
                .msg
                .and_then(|msg| held.iter().position(|row| row.msg == Some(msg)))
            {
                // The row is one the client already holds: replace it where it
                // stands, so the list keeps its length and its order.
                Some(at) => held[at] = (**entry).clone(),
                // A row the client has not seen goes where a fresh listing
                // would have put it, which is the store's order,
                // `date_sort DESC, id DESC`.
                None => {
                    let at = held
                        .iter()
                        .position(|row| sort_key(row) < sort_key(entry))
                        .unwrap_or(held.len());
                    held.insert(at, (**entry).clone());
                }
            }
            true
        }
        MessageRowDelta::Remove {
            account,
            mailbox,
            uid,
        } => match row_id_for(account, mailbox, *uid) {
            Some(id) => {
                let gone = MessageRef::new(id);
                held.retain(|row| row.msg != Some(gone));
                true
            }
            // A uid this session never listed cannot be turned into the row id
            // the list is keyed by, so the honest answer is a refetch rather
            // than a guess at which row was meant.
            None => false,
        },
        MessageRowDelta::Invalidate { .. } => false,
    }
}

/// A row's place in the store's order: the sort date first, the row id as the
/// tiebreak, both descending.
fn sort_key(entry: &EmailEntry) -> (&str, i64) {
    (
        entry.date_sort.as_str(),
        entry.msg.map(MessageRef::row_id).unwrap_or_default(),
    )
}

/// The `(account, mailbox, uid)` of a `message:<account>/<mailbox>/<uid>`
/// resource, or `None` for a resource about anything else.
fn message_resource(payload: &Value) -> Option<(String, String, i64)> {
    let resource = payload["resource"].as_str()?;
    let path = resource.strip_prefix("message:")?;
    let (account, rest) = path.split_once('/')?;
    let (mailbox, uid) = rest.rsplit_once('/')?;
    Some((account.to_string(), mailbox.to_string(), uid.parse().ok()?))
}

/// The `(account, mailbox)` of a `mailbox:<account>/<mailbox>` resource.
fn mailbox_resource(payload: &Value) -> Option<(String, String)> {
    let resource = payload["resource"].as_str()?;
    let (account, mailbox) = resource.strip_prefix("mailbox:")?.split_once('/')?;
    Some((account.to_string(), mailbox.to_string()))
}

// ---------------------------------------------------------------------------
// The uid index
// ---------------------------------------------------------------------------

/// One mailbox's `uid -> messages.id` table, keyed by `(account, mailbox)`.
type RowIdIndex = HashMap<(String, String), HashMap<i64, i64>>;

/// `(account, mailbox) -> (uid -> messages.id)`, the correspondence a remove
/// event needs and a held list cannot carry. See the module docs.
static ROW_IDS: OnceLock<Mutex<RowIdIndex>> = OnceLock::new();

/// The index, created on first use.
fn row_ids() -> &'static Mutex<RowIdIndex> {
    ROW_IDS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record a whole listing, replacing whatever was known about that mailbox: a
/// listing is the current truth about it, and keeping the previous one would
/// grow without bound across a session.
fn remember_mailbox(account: &str, mailbox: &str, rows: &[MessageRow]) {
    let table = rows.iter().map(|row| (row.uid, row.id)).collect();
    if let Ok(mut index) = row_ids().lock() {
        index.insert((account.to_string(), mailbox.to_string()), table);
    }
}

/// Record one row, which is what a replace delta carries.
fn remember_row(account: &str, mailbox: &str, row: &MessageRow) {
    if let Ok(mut index) = row_ids().lock() {
        index
            .entry((account.to_string(), mailbox.to_string()))
            .or_default()
            .insert(row.uid, row.id);
    }
}

/// The `messages.id` of a uid this session has listed, if it has.
fn row_id_for(account: &str, mailbox: &str, uid: i64) -> Option<i64> {
    let index = row_ids().lock().ok()?;
    index
        .get(&(account.to_string(), mailbox.to_string()))?
        .get(&uid)
        .copied()
}
