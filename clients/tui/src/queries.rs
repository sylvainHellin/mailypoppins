//! The TUI's query layer (P5-U4, #0124): the reads a frame needs, in the TUI's
//! own vocabulary.
//!
//! The reads themselves are `mp_client::queries`, typed over the protocol's
//! rows (`MessageListRow`, `MailboxRow`, `AgendaEvent`, ...) so the desktop
//! client shares them. What is left here is the mapping from those rows into
//! this client's model ([`EmailEntry`], [`MailboxInfo`], [`CalendarEvent`],
//! [`MessageRef`]), the degrade-on-refusal policy (a refused preview is an
//! empty pane and a line in the log), the Drafts branch and its file parse, and
//! the uid index below.
//!
//! # Why the wire row goes through `entry_from_row`
//!
//! A listed row is handed to [`entry_from_row`](crate::app::entry_from_row),
//! the same mapper the store-backed oracle in `src/tui_tests/oracle.rs` feeds.
//! That is what makes the two paths equal by construction rather than by
//! inspection: every derivation (the display name, the `(no subject)`
//! fallback, `resolve_date`'s two strings, the four flag axes) stays in the one
//! function that owns it. The drafts branch does the same through
//! [`entry_from_draft`](crate::app::entry_from_draft) and
//! [`entry_from_skip`](crate::app::entry_from_skip).
//!
//! # The uid index
//!
//! A held list is keyed by `messages.id` (`MessageRef`, #0050) and the daemon
//! removes a row by `message:<account>/<mailbox>/<uid>`. `MessageListRow`
//! carries the uid, so `mp_client::queries::apply_row_delta` needs no index,
//! but an [`EmailEntry`] does not, so the correspondence has to be remembered
//! where both are seen: every listing records `(account, mailbox, uid) -> id`
//! here, and a remove resolves through it. The only other delta is an
//! invalidate, which owes a re-list and needs no index. A listing replaces its
//! mailbox's table whole, so the memory is one entry per row of the mailboxes
//! this session has opened, and a uid the table does not know owes a refetch
//! rather than a guess.
//!
//! The alternative is a `uid` field on `EmailEntry`, which is 31 struct
//! literals across seven files, two of them the frozen golden-frame fixtures.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use serde_json::Value;

use mp_client::queries as wire;
use mp_protocol::listing::{MessageListRow, ThreadListing};

use crate::app::{
    entry_from_conversation, entry_from_draft, entry_from_member, entry_from_row, entry_from_skip,
    fill_draft_member, mailbox_key, status_for_mailbox, CalendarEvent, ConvRole, EmailEntry,
    MailboxInfo, MessageRef,
};
pub use mp_client::queries::{MessageRowDelta, Queries};
use mp_core::selector::DRAFTS_MAILBOX;

// ---------------------------------------------------------------------------
// The mailbox listing
// ---------------------------------------------------------------------------

/// One mailbox of one account, newest first: the whole list, collected before
/// it is handed back.
///
/// A received mailbox is [`Queries::list_stream`], which a session answers
/// with `message.list_stream` (#0138): the rows arrive in chunks of about
/// 1 MiB and are collected on the session thread, so a mailbox past the 16 MiB
/// response cap still lists. The Drafts mailbox is the one branch that is not
/// a message listing: a draft is a local file with no `messages` row, so it is
/// `draft.list` instead, and the two answers become the same kind of row here.
pub fn list_emails(q: &dyn Queries, account: &str, mailbox: &str) -> Result<Vec<EmailEntry>> {
    if mailbox == DRAFTS_MAILBOX {
        let (method, params) = wire::draft_list_request(account);
        return decode_drafts(&q.call(method, params)?);
    }
    let listing = q.list_stream(account, mailbox)?;
    Ok(entries_from_rows(account, mailbox, listing.messages))
}

/// The list a mailbox load delivers: [`list_conversation_entries`] when the
/// mailbox lists as conversations, with the threads `expanded` re-expanded,
/// and [`list_emails`] otherwise (#0142).
pub fn list_mailbox(
    q: &dyn Queries,
    account: &str,
    mailbox: &str,
    conversations: Option<&[String]>,
) -> Result<Vec<EmailEntry>> {
    match conversations {
        Some(expanded) => list_conversation_entries(q, account, mailbox, expanded),
        None => list_emails(q, account, mailbox),
    }
}

/// One received mailbox as conversations, newest first, each expanded
/// conversation of `expanded` followed by its members (#0142).
///
/// [`Queries::list_conversations`] for the headers, then one
/// [`conversation_members`] per expanded conversation still listed, so a
/// re-stream keeps open what the user opened. A conversation whose expansion
/// is refused is listed collapsed, with the reason in the log.
pub fn list_conversation_entries(
    q: &dyn Queries,
    account: &str,
    mailbox: &str,
    expanded: &[String],
) -> Result<Vec<EmailEntry>> {
    let listing = q.list_conversations(account, mailbox)?;
    let status = status_for_mailbox(mailbox);
    let mut out = Vec::with_capacity(listing.conversations.len());
    for row in listing.conversations {
        let mut header = entry_from_conversation(row, &status);
        let members = match (&mut header.conv, header.msg) {
            (Some(ConvRole::Header(conv)), Some(latest))
                if conv.expandable() && expanded.contains(&conv.thread_id) =>
            {
                match conversation_members(q, account, mailbox, &conv.thread_id, latest) {
                    Ok(members) => {
                        conv.expanded = true;
                        members
                    }
                    Err(e) => {
                        log::warn!(
                            "[queries] expanding {} of {account}/{mailbox}: {e:#}",
                            conv.thread_id
                        );
                        Vec::new()
                    }
                }
            }
            _ => Vec::new(),
        };
        out.push(header);
        out.extend(members);
    }
    Ok(out)
}

/// The members of one conversation of `mailbox`, newest first, as the rows an
/// expanded header draws under it (#0142).
///
/// [`Queries::thread_expanded`] from the header's `latest`: each message held
/// twice is its copy in the listed mailbox, and the drafts that answer a member
/// join as draft members.
///
/// The daemon answers oldest first, as the thread overlay reads it; the list
/// reverses it so a conversation reads in the order the mailbox around it
/// does, newest on top. A draft member takes its recipients, its sender and
/// its status from its file ([`draft_member_headers`]).
pub fn conversation_members(
    q: &dyn Queries,
    account: &str,
    mailbox: &str,
    thread_id: &str,
    latest: MessageRef,
) -> Result<Vec<EmailEntry>> {
    let thread = q.thread_expanded(account, latest.row_id(), mailbox)?;
    Ok(thread
        .messages
        .iter()
        .rev()
        .map(|member| {
            let mut entry = entry_from_member(member, thread_id, &thread.subject);
            if let Some(id) = member.draft_id.as_deref() {
                if let Some(frontmatter) =
                    draft_member_headers(q, account, id, member.path.as_deref())
                {
                    fill_draft_member(&mut entry, &frontmatter);
                }
            }
            entry
        })
        .collect())
}

/// The frontmatter of a conversation's draft member, read from the file the
/// thread row names, as [`draft_body`] reads its body (PERSO-146).
///
/// A daemon older than the row's `path` is asked through `draft.path`. A
/// file that is gone or does not parse leaves the member as the thread row
/// drew it, with the reason in the log.
fn draft_member_headers(
    q: &dyn Queries,
    account: &str,
    id: &str,
    path: Option<&str>,
) -> Option<mp_core::types::EmailFrontmatter> {
    let path = match path.filter(|p| !p.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => match wire::draft_path(q, account, id) {
            Ok(location) => PathBuf::from(location.path),
            Err(e) => {
                log::warn!("[queries] the draft member {id} of {account} is not indexed: {e:#}");
                return None;
            }
        },
    };
    match mp_core::draft::parse_email_draft(&path) {
        Ok(draft) => Some(draft.frontmatter),
        Err(e) => {
            log::warn!(
                "[queries] reading the draft member {}: {e:#}",
                path.display()
            );
            None
        }
    }
}

/// The rows of one message listing, as list rows.
fn entries_from_rows(account: &str, mailbox: &str, rows: Vec<MessageListRow>) -> Vec<EmailEntry> {
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
    let listing = wire::decode_draft_listing(answer)?;
    let skipped = listing.skipped.into_iter().map(entry_from_skip);
    let rows = listing.drafts.into_iter().map(entry_from_draft);
    Ok(skipped.chain(rows).collect())
}

// ---------------------------------------------------------------------------
// The local search pass
// ---------------------------------------------------------------------------

/// The `hits` array of a `message.search` answer, as the search overlay's rows
/// (#0105, P5-U6).
///
/// A hit carries its own `mailbox`, which the overlay's `source_label` is, and
/// its body (`body: true`), because the overlay renders a hit out of the
/// [`mp_core::parse::FetchedEmail`] it holds rather than out of a second read.
/// The row goes through `entry_from_row`, the same construction a listing
/// uses, so the overlay row and the list row cannot drift.
pub fn decode_search_hits(answer: &Value) -> Vec<crate::app::SearchResultEntry> {
    wire::decode_search_hits(answer)
        .into_iter()
        .map(search_hit)
        .collect()
}

/// One `message.search` hit as an overlay row.
fn search_hit(hit: wire::SearchHit) -> crate::app::SearchResultEntry {
    let wire::SearchHit { mailbox, row, body } = hit;
    let fetched = mp_core::parse::FetchedEmail {
        from: row.from.clone(),
        to: row.to.clone(),
        cc: row.cc.clone(),
        reply_to: row.reply_to.clone(),
        bcc: row.bcc.clone(),
        subject: row.subject.clone(),
        date: row.date_display.clone(),
        body_text: body.unwrap_or_default(),
        html_body: None,
        has_attachments: row.has_attachments,
        server_has_attachments: None,
        message_id: Some(row.message_id.clone()),
        attachments: Vec::new(),
        flags: mp_core::types::MessageFlags {
            seen: row.flags.seen,
            answered: row.flags.answered,
            forwarded: row.flags.forwarded,
            flagged: row.flags.flagged,
        },
        calendar_ics: None,
        event: None,
    };
    let status = status_for_mailbox(&mailbox);
    crate::app::SearchResultEntry {
        entry: entry_from_row(row, &status),
        fetched,
        source_label: mailbox,
    }
}

// ---------------------------------------------------------------------------
// The sidebar counts
// ---------------------------------------------------------------------------

/// The per-mailbox totals the sidebar prints, index-aligned with `mailboxes`.
///
/// One `mailbox.list`: a mailbox the daemon does not name counts zero and
/// keeps its slot rather than shifting every count after it. The Drafts total
/// comes from the draft index on the daemon's side.
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
    wire::mailbox_list_params(account)
}

/// A `mailbox.list` answer as the sidebar's count column.
pub fn decode_counts(mailboxes: &[MailboxInfo], answer: &Value) -> Vec<usize> {
    let rows = wire::decode_mailbox_rows(answer);
    let totals = wire::totals_by_slug(&rows);
    mailboxes
        .iter()
        .map(|mb| totals.get(mailbox_key(mb).as_str()).copied().unwrap_or(0) as usize)
        .collect()
}

// ---------------------------------------------------------------------------
// The preview body
// ---------------------------------------------------------------------------

/// The stored body of one row, for the preview memo.
///
/// A refusal is an empty preview and a line in the log, which is what the
/// store-backed path did with a stale reference: the pane goes blank and the
/// log says why. The `Err` arm is therefore unreachable today and is kept
/// because a transport failure rather than a missing row is a distinction a
/// later unit may want to make.
pub fn message_body(q: &dyn Queries, account: &str, msg: MessageRef) -> Result<Option<String>> {
    Ok(
        wire::message_body(q, account, msg.row_id()).unwrap_or_else(|e| {
            log::warn!("[queries] {msg} of {account} has no body to preview: {e:#}");
            None
        }),
    )
}

/// The stored body of one row and its recipients, for the preview memo of a
/// conversation member, whose thread row carries none (PERSO-146).
///
/// The same `message.get` [`message_body`] sends, read further, with the same
/// degrade: a refusal is an empty preview, no recipients and a line in the
/// log.
pub fn message_preview(
    q: &dyn Queries,
    account: &str,
    msg: MessageRef,
) -> Result<Option<wire::MessagePreview>> {
    Ok(wire::message_preview(q, account, msg.row_id())
        .map(Some)
        .unwrap_or_else(|e| {
            log::warn!("[queries] {msg} of {account} has no body to preview: {e:#}");
            None
        }))
}

/// The body of one draft, through `draft.path` plus a client-side parse.
///
/// The daemon answers where the file is, not what is in it: a draft is a local
/// Markdown file, both ends of the socket read it with `mp_core::draft`'s
/// parser, and shipping the body through the wire would be a second place for
/// the signature sentinels to be stripped.
///
/// `None` degrades to an empty pane, which is what a stale index has always
/// looked like.
pub fn draft_body(q: &dyn Queries, account: &str, id: &str) -> Result<Option<String>> {
    let location = match wire::draft_path(q, account, id) {
        Ok(location) => location,
        Err(e) => {
            log::warn!("[queries] {id} of {account} is no longer indexed: {e:#}");
            return Ok(None);
        }
    };
    match mp_core::draft::parse_email_draft(&PathBuf::from(&location.path)) {
        Ok(draft) => Ok(Some(draft.body_markdown)),
        Err(e) => {
            log::warn!("[queries] reading {}: {e:#}", location.path);
            Ok(None)
        }
    }
}

/// The conversation one message belongs to, through `message.thread`
/// (`LST-10`, P5-U10d).
pub fn thread(q: &dyn Queries, account: &str, msg: MessageRef) -> Result<ThreadListing> {
    wire::thread(q, account, msg.row_id())
}

// ---------------------------------------------------------------------------
// Invitations and the agenda (P5-U10)
// ---------------------------------------------------------------------------

/// The active account's agenda, through `calendar.events`.
///
/// An empty agenda on a refusal, which is what an account with no store, no
/// invites or no readable blob always looked like; a malformed answer
/// degrades the same way.
pub fn calendar_events(q: &dyn Queries, account: &str) -> Result<Vec<CalendarEvent>> {
    match wire::calendar_events(q, account) {
        Ok(rows) => Ok(rows.into_iter().map(CalendarEvent::from_wire).collect()),
        Err(e) => {
            log::warn!("[queries] the agenda of {account}: {e:#}");
            Ok(Vec::new())
        }
    }
}

/// One message's invitation card, through `message.invite`; `None` on a
/// refusal, which renders as no card.
pub fn message_invite(
    q: &dyn Queries,
    account: &str,
    msg: MessageRef,
) -> Result<Option<mp_core::types::EventFrontmatter>> {
    match wire::message_invite(q, account, msg.row_id()) {
        Ok(event) => Ok(event),
        Err(e) => {
            log::warn!("[queries] the invitation on {msg} of {account}: {e:#}");
            Ok(None)
        }
    }
}

/// One message's raw `invite.ics` bytes, through `message.ics`; `None` on a
/// refusal.
pub fn message_ics(q: &dyn Queries, account: &str, msg: MessageRef) -> Result<Option<Vec<u8>>> {
    match wire::message_ics(q, account, msg.row_id()) {
        Ok(bytes) => Ok(bytes),
        Err(e) => {
            log::warn!("[queries] the ics of {msg} of {account}: {e:#}");
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Credentials and contacts (the daemon leftovers' TUI follow-up)
// ---------------------------------------------------------------------------

/// Whether `account` can reach its server, through `account.credentials`:
/// the presence of its passwords and token, never their value.
pub fn account_credentials(q: &dyn Queries, account: &str) -> Result<crate::app::Credentials> {
    let answer = q.call(
        "account.credentials",
        serde_json::json!({ "account": account }),
    )?;
    let answer: mp_protocol::account::AccountCredentials = serde_json::from_value(answer)
        .map_err(|e| anyhow::anyhow!("reading account.credentials: {e}"))?;
    answer
        .accounts
        .iter()
        .find(|entry| entry.account == account)
        .map(crate::app::Credentials::from_wire)
        .ok_or_else(|| anyhow::anyhow!("account.credentials did not name {account}"))
}

/// How many rows one `contact.search` asks for when the TUI lists an
/// account's whole index: the Contacts view shows every contact, and the
/// compose wizard's completion ranks over all of them.
pub const CONTACT_INDEX_LIMIT: u64 = u32::MAX as u64;

/// An account's contact index as the daemon ranks it, and the build the cache
/// guard refused when the search had to build one.
#[derive(Clone, Debug)]
pub struct ContactsAnswer {
    /// Every contact, keyed by address; `None` when the index holds none, which
    /// the Contacts view renders as "press r to build the index".
    pub index: Option<mp_core::contacts::ContactIndex>,
    /// The implicit build `contact.search` ran and the guard refused.
    pub refused: Option<mp_protocol::contact::RefusedRebuild>,
}

/// `account`'s whole contact index, through `contact.search` with an empty
/// query.
///
/// The rows carry what the ranking reads (the three tier counts and the two
/// timestamps), so the index rebuilt here ranks a typed query exactly as the
/// daemon would, without a round trip per keystroke. The cache file is the
/// daemon's; nothing here reads it.
pub fn contact_index(q: &dyn Queries, account: &str) -> Result<ContactsAnswer> {
    let answer = q.call(
        "contact.search",
        serde_json::json!({ "account": account, "query": "", "limit": CONTACT_INDEX_LIMIT }),
    )?;
    Ok(decode_contacts(account, &answer))
}

/// [`contact_index`]'s answer, decoded.
pub fn decode_contacts(account: &str, answer: &Value) -> ContactsAnswer {
    let refused = serde_json::from_value(answer["rebuild"].clone()).ok();
    let text = |row: &Value, key: &str| row[key].as_str().unwrap_or_default().to_string();
    let count = |row: &Value, key: &str| row[key].as_u64().unwrap_or(0) as u32;
    let contacts: HashMap<String, mp_core::contacts::Contact> = answer["contacts"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let address = row["address"].as_str()?.to_string();
                    Some((
                        address.clone(),
                        mp_core::contacts::Contact {
                            address,
                            display_name: text(row, "display_name"),
                            sent_to: count(row, "sent_to"),
                            sent_cc: count(row, "sent_cc"),
                            received: count(row, "received"),
                            first_seen: text(row, "first_seen"),
                            last_seen: text(row, "last_seen"),
                            source: mp_core::contacts::ContactSource::Local,
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let index = (!contacts.is_empty()).then(|| mp_core::contacts::ContactIndex {
        account: account.to_string(),
        contacts,
        built_at: String::new(),
    });
    ContactsAnswer { index, refused }
}

// ---------------------------------------------------------------------------
// Row deltas
// ---------------------------------------------------------------------------

/// Fold one delta into a held list of [`EmailEntry`], answering whether
/// nothing is owed; the TUI's twin of `mp_client::queries::apply_row_delta`.
///
/// `true` means the list is current again, `false` means the caller must
/// re-list the mailbox through [`list_emails`]. A delta about another mailbox folds as a no-op and
/// answers `true`.
pub fn apply_row_delta(held: &mut Vec<EmailEntry>, mailbox: &str, delta: &MessageRowDelta) -> bool {
    if delta.mailbox() != mailbox {
        return true;
    }
    match delta {
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
fn remember_mailbox(account: &str, mailbox: &str, rows: &[MessageListRow]) {
    let table = rows.iter().map(|row| (row.uid, row.id)).collect();
    if let Ok(mut index) = row_ids().lock() {
        index.insert((account.to_string(), mailbox.to_string()), table);
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
