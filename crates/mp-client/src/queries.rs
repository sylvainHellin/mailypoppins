//! The blocking door every client's reads and writes go through (P5-U4,
//! #0124), and the typed reads over it, lifted from `clients/tui` so a second
//! client shares both.
//!
//! [`Queries`] is an object-safe trait with one blocking `call`, implemented
//! for [`Session`](crate::session::Session) and
//! [`QueryHandle`](crate::session::QueryHandle), so a query layer is testable
//! over an in-process dispatcher without a socket.
//!
//! Over it sit the reads a client needs, typed in the protocol's own
//! vocabulary ([`mp_protocol`]) rather than in any client's model: a listing
//! is `Vec<MessageListRow>`, the sidebar is `Vec<MailboxRow>`, a conversation
//! is a [`ThreadListing`], the agenda is `Vec<AgendaEvent>`. What a row
//! becomes on screen is each client's own mapping; the TUI's lives in
//! `mp_tui::queries` and `mp_tui::app::entry_from_row`.
//!
//! # Refusals
//!
//! A read here answers the daemon's refusal as an `Err` and decides nothing
//! about it. Whether a refused preview is an empty pane and a line in the log
//! (what the TUI does) or an error banner is presentation, so it is the
//! client's.
//!
//! # Leniency
//!
//! A listed row is decoded with [`MessageListRow`]'s own `serde` derive, one
//! row at a time, so a row a daemon older than the client produced still
//! decodes: every field but the identity defaults, and a client that refused a
//! whole list over one field it would have rendered as empty would turn an
//! additive protocol change into a list that will not paint.
//!
//! # Row deltas
//!
//! [`MessageRowDelta`] is the event half of the whole-list transfer
//! `docs/baselines/decisions/list-transfer.md` chose: `message.row` replaces a
//! row, `state.remove` over `message:<account>/<mailbox>/<uid>` drops one,
//! `state.invalidate` over `mailbox:<account>/<mailbox>` (outside the counts
//! scope) owes a refetch. [`apply_row_delta`] folds one into a held
//! `Vec<MessageListRow>`, which carries the uid a removal names, so this layer
//! needs no uid index; a client whose own row type drops the uid (the TUI's
//! `EmailEntry`) keeps one of its own.

use std::collections::HashMap;

use anyhow::Result;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};

use mp_protocol::calendar::{AgendaEvent, EventFrontmatter};
use mp_protocol::draft::{DraftListing, DraftLocation};
use mp_protocol::listing::{MessageListRow, ThreadListing};
use mp_protocol::rendition::{MessageHtml, MessageHtmlParams, METHOD_MESSAGE_HTML};
use mp_protocol::state::MailboxRow;
use mp_protocol::EventEnvelope;

/// Something that answers a daemon method call and blocks for the answer.
///
/// One method, with exactly the signature
/// [`Session::call`](crate::session::Session::call) already had, so the
/// implementation for a session is a delegation and no second connect path
/// exists. Object safe on purpose: a client's typed reads take `&dyn Queries`
/// so a test can drive them over a dispatcher in the same process.
pub trait Queries {
    /// Call `method` with `params` and hand back its `result`.
    fn call(&self, method: &str, params: Value) -> Result<Value>;
}

impl Queries for crate::session::Session {
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        crate::session::Session::call(self, method, params)
    }
}

impl Queries for crate::session::QueryHandle {
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        crate::session::QueryHandle::call(self, method, params)
    }
}

// ---------------------------------------------------------------------------
// The mailbox listing
// ---------------------------------------------------------------------------

/// The `message.list` call for one whole mailbox, for a caller that dispatches
/// it itself (a background load posts the same request the blocking path
/// sends, rather than a second spelling of it).
///
/// `limit: null` is the whole list: there is no offset and no paging
/// parameter, because an offset is the option the list-transfer decision did
/// not choose and the row deltas keep the list current afterwards.
pub fn message_list_request(account: &str, mailbox: &str) -> (&'static str, Value) {
    (
        "message.list",
        json!({"account": account, "mailbox": mailbox, "limit": null}),
    )
}

/// The `draft.list` call for one account's Drafts mailbox, every status.
///
/// A draft is a local file with no `messages` row, so the Drafts mailbox is
/// this method rather than `message.list`; which mailbox slug means "drafts"
/// is `mp_core::selector::DRAFTS_MAILBOX`, which this crate does not link.
pub fn draft_list_request(account: &str) -> (&'static str, Value) {
    ("draft.list", json!({"account": account, "status": null}))
}

/// One mailbox of one account, newest first, as the daemon's wire rows.
pub fn list_messages(q: &dyn Queries, account: &str, mailbox: &str) -> Result<Vec<MessageListRow>> {
    let (method, params) = message_list_request(account, mailbox);
    Ok(decode_message_rows(&q.call(method, params)?))
}

/// One account's drafts, through `draft.list`.
pub fn list_drafts(q: &dyn Queries, account: &str) -> Result<DraftListing> {
    let (method, params) = draft_list_request(account);
    decode_draft_listing(&q.call(method, params)?)
}

/// The `messages` array of a `message.list` answer, each row decoded on its
/// own (see the module docs on leniency). An answer with no array is no rows.
pub fn decode_message_rows(answer: &Value) -> Vec<MessageListRow> {
    answer["messages"]
        .as_array()
        .map(|rows| rows.iter().map(row_from_wire).collect())
        .unwrap_or_default()
}

/// A `draft.list` answer.
pub fn decode_draft_listing(answer: &Value) -> Result<DraftListing> {
    Ok(<DraftListing as serde::Deserialize>::deserialize(answer)?)
}

/// One listed row as the typed wire row it is.
///
/// A row that does not decode at all (no `id`, or a field of the wrong type)
/// is logged and becomes [`MessageListRow::default`], which is what a list
/// that keeps painting needs: one blank row is visible, a list that will not
/// load is not.
pub fn row_from_wire(row: &Value) -> MessageListRow {
    // `&Value` is a `Deserializer`: no per-row clone of the whole object.
    <MessageListRow as serde::Deserialize>::deserialize(row).unwrap_or_else(|e| {
        log::warn!("[queries] a listed row did not decode: {e}");
        MessageListRow::default()
    })
}

// ---------------------------------------------------------------------------
// The local search pass
// ---------------------------------------------------------------------------

/// One `message.search` hit: the listing row, the mailbox it was found in, and
/// its body when the call asked for one (`body: true`).
///
/// The protocol documents a hit as "the same row plus the `mailbox`" and has
/// no struct for it, so this is the client's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchHit {
    /// The mailbox the hit lives in, which a listing row does not carry.
    pub mailbox: String,
    /// The envelope.
    pub row: MessageListRow,
    /// The stored plain-text body, `None` when the call did not ask for it.
    pub body: Option<String>,
}

/// The `hits` array of a `message.search` answer.
pub fn decode_search_hits(answer: &Value) -> Vec<SearchHit> {
    answer["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .map(|hit| SearchHit {
                    mailbox: hit["mailbox"].as_str().unwrap_or_default().to_string(),
                    row: row_from_wire(hit),
                    body: hit["body"].as_str().map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The sidebar
// ---------------------------------------------------------------------------

/// The params `mailbox.list` takes.
pub fn mailbox_list_params(account: &str) -> Value {
    json!({"account": account})
}

/// One account's mailboxes with their counts, through `mailbox.list`.
pub fn mailbox_rows(q: &dyn Queries, account: &str) -> Result<Vec<MailboxRow>> {
    Ok(decode_mailbox_rows(
        &q.call("mailbox.list", mailbox_list_params(account))?,
    ))
}

/// The `mailboxes` array of a `mailbox.list` answer. A row that does not
/// decode is logged and skipped, so one malformed row costs its own count and
/// not the sidebar's.
pub fn decode_mailbox_rows(answer: &Value) -> Vec<MailboxRow> {
    answer["mailboxes"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    <MailboxRow as serde::Deserialize>::deserialize(row)
                        .map_err(|e| log::warn!("[queries] a mailbox row did not decode: {e}"))
                        .ok()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `slug -> total` over a `mailbox.list` answer, the shape a sidebar that
/// already knows its own mailbox order looks counts up in.
pub fn totals_by_slug(rows: &[MailboxRow]) -> HashMap<&str, u64> {
    rows.iter()
        .map(|row| (row.slug.as_str(), row.total))
        .collect()
}

// ---------------------------------------------------------------------------
// One message
// ---------------------------------------------------------------------------

/// The stored plain-text body of one row, through `message.get`.
///
/// Addressed by `row_id`, the `id` the listing carried, so a client holding a
/// listed row needs no second identity for it. `None` when the row has no
/// stored body.
pub fn message_body(q: &dyn Queries, account: &str, row_id: i64) -> Result<Option<String>> {
    let params = json!({"account": account, "row_id": row_id, "body": true});
    let answer = q.call("message.get", params)?;
    Ok(answer["body"].as_str().map(str::to_string))
}

/// The browser rendition of one row, inline, through `message.html`: the same
/// bytes `message.materialise_html` writes to a file, with no handle to
/// release.
///
/// A rendition over [`mp_protocol::rendition::MAX_INLINE_HTML_BYTES`] is a
/// `frame_too_large` refusal whose `data` is an
/// [`InlineHtmlRefusal`](mp_protocol::rendition::InlineHtmlRefusal) naming
/// `message.materialise_html`, and a row with no markup is `-32602`; both come
/// back as the `Err` they are, for the client to fall back or say so.
pub fn message_html(q: &dyn Queries, account: &str, row_id: i64) -> Result<MessageHtml> {
    let params = MessageHtmlParams {
        account: account.to_string(),
        row_id: Some(row_id),
        ..MessageHtmlParams::default()
    };
    let answer = q.call(METHOD_MESSAGE_HTML, serde_json::to_value(params)?)?;
    Ok(serde_json::from_value(answer)?)
}

/// The conversation one message belongs to, through `message.thread`
/// (`LST-10`): oldest first, deduped by `Message-ID`, with the addressed
/// message marked `current`.
pub fn thread(q: &dyn Queries, account: &str, row_id: i64) -> Result<ThreadListing> {
    let params = json!({"account": account, "row_id": row_id});
    Ok(serde_json::from_value(q.call("message.thread", params)?)?)
}

/// One message's invitation card, through `message.invite`.
///
/// `None` when the row carries no iMIP payload or the payload does not parse.
pub fn message_invite(
    q: &dyn Queries,
    account: &str,
    row_id: i64,
) -> Result<Option<EventFrontmatter>> {
    let params = json!({"account": account, "row_id": row_id});
    let answer = q.call("message.invite", params)?;
    match answer.get("event") {
        None | Some(Value::Null) => Ok(None),
        Some(event) => Ok(Some(serde_json::from_value(event.clone())?)),
    }
}

/// One message's raw `invite.ics` bytes, through `message.ics`, decoded from
/// the base64 the wire carries them in (a blob is bytes, a JSON string is
/// not). `None` when the row has no ics.
pub fn message_ics(q: &dyn Queries, account: &str, row_id: i64) -> Result<Option<Vec<u8>>> {
    let params = json!({"account": account, "row_id": row_id});
    let answer = q.call("message.ics", params)?;
    let Some(encoded) = answer.get("ics").and_then(Value::as_str) else {
        return Ok(None);
    };
    Ok(Some(BASE64.decode(encoded)?))
}

/// Where one draft's file is, through `draft.path`.
///
/// The daemon answers where the file is and not what is in it: a draft is a
/// local Markdown file whose format is `mp_core::draft`'s, and a client parses
/// it with that parser (`mp_tui::queries::draft_body` does).
pub fn draft_path(q: &dyn Queries, account: &str, id: &str) -> Result<DraftLocation> {
    let params = json!({"account": account, "id": id});
    Ok(serde_json::from_value(q.call("draft.path", params)?)?)
}

// ---------------------------------------------------------------------------
// The agenda
// ---------------------------------------------------------------------------

/// One account's agenda, through `calendar.events`: deduped, reply-folded and
/// sorted daemon-side, because every derived column is a fold over the
/// account's other rows.
pub fn calendar_events(q: &dyn Queries, account: &str) -> Result<Vec<AgendaEvent>> {
    let answer = q.call("calendar.events", json!({"account": account}))?;
    Ok(serde_json::from_value(answer["events"].clone())?)
}

// ---------------------------------------------------------------------------
// Row deltas
// ---------------------------------------------------------------------------

/// One change to a held mailbox list, decoded from one event. See the module
/// docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRowDelta {
    /// One row, as a fresh listing would carry it.
    Replace {
        /// The account it belongs to.
        account: String,
        /// The mailbox it belongs to.
        mailbox: String,
        /// The row itself, boxed because it is much the largest variant.
        row: Box<MessageListRow>,
    },
    /// One row is gone, named the way the daemon names it.
    Remove {
        /// The account it belonged to.
        account: String,
        /// The mailbox it belonged to.
        mailbox: String,
        /// Its uid within the mailbox.
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
    /// silently rewrite a message list. The counts scope of an invalidate is
    /// the sidebar's, not the list's, so it is not a delta either: a hundred
    /// count changes for one mailbox must not each refetch the open list.
    pub fn decode(event: &EventEnvelope) -> Option<MessageRowDelta> {
        match event.kind.as_str() {
            "message.row" => Some(MessageRowDelta::Replace {
                account: event.payload["account"].as_str()?.to_string(),
                mailbox: event.payload["mailbox"].as_str()?.to_string(),
                row: Box::new(row_from_wire(&event.payload["message"])),
            }),
            "state.remove" => {
                let (account, mailbox, uid) =
                    message_resource(event.payload["resource"].as_str()?)?;
                Some(MessageRowDelta::Remove {
                    account,
                    mailbox,
                    uid,
                })
            }
            "state.invalidate" => {
                if event.payload["scope"]["query"].as_str() == Some("counts") {
                    return None;
                }
                let (account, mailbox) = mailbox_resource(event.payload["resource"].as_str()?)?;
                Some(MessageRowDelta::Invalidate { account, mailbox })
            }
            _ => None,
        }
    }

    /// The account this delta is about.
    pub fn account(&self) -> &str {
        match self {
            MessageRowDelta::Replace { account, .. }
            | MessageRowDelta::Remove { account, .. }
            | MessageRowDelta::Invalidate { account, .. } => account,
        }
    }

    /// The mailbox this delta is about.
    pub fn mailbox(&self) -> &str {
        match self {
            MessageRowDelta::Replace { mailbox, .. }
            | MessageRowDelta::Remove { mailbox, .. }
            | MessageRowDelta::Invalidate { mailbox, .. } => mailbox,
        }
    }
}

/// Fold one delta into a held list of `mailbox`, answering whether nothing is
/// owed.
///
/// `true` means the list is current again, `false` means the caller must
/// re-issue `message.list`. A delta about another mailbox folds as a no-op and
/// answers `true`: the other mailboxes move constantly, and refetching the
/// open list on each of them would put back exactly the per-event whole-list
/// transfer the deltas exist to avoid.
///
/// A replace of a held row (same `id`) lands where the row stands; a new row
/// goes where a fresh listing would put it, the store's
/// `date_sort DESC, id DESC`. A removal drops the row with that uid.
pub fn apply_row_delta(
    held: &mut Vec<MessageListRow>,
    mailbox: &str,
    delta: &MessageRowDelta,
) -> bool {
    if delta.mailbox() != mailbox {
        return true;
    }
    match delta {
        MessageRowDelta::Replace { row, .. } => {
            match held.iter().position(|held| held.id == row.id) {
                Some(at) => held[at] = (**row).clone(),
                None => {
                    let key = (row.date_sort.as_str(), row.id);
                    let at = held
                        .iter()
                        .position(|held| (held.date_sort.as_str(), held.id) < key)
                        .unwrap_or(held.len());
                    held.insert(at, (**row).clone());
                }
            }
            true
        }
        MessageRowDelta::Remove { uid, .. } => {
            held.retain(|row| row.uid != *uid);
            true
        }
        MessageRowDelta::Invalidate { .. } => false,
    }
}

/// The `(account, mailbox, uid)` of a `message:<account>/<mailbox>/<uid>`
/// resource, or `None` for a resource about anything else. The mailbox may
/// itself contain `/` (a server folder path); the uid is the last segment.
pub fn message_resource(resource: &str) -> Option<(String, String, i64)> {
    let path = resource.strip_prefix("message:")?;
    let (account, rest) = path.split_once('/')?;
    let (mailbox, uid) = rest.rsplit_once('/')?;
    Some((account.to_string(), mailbox.to_string(), uid.parse().ok()?))
}

/// The `(account, mailbox)` of a `mailbox:<account>/<mailbox>` resource.
pub fn mailbox_resource(resource: &str) -> Option<(String, String)> {
    let (account, mailbox) = resource.strip_prefix("mailbox:")?.split_once('/')?;
    Some((account.to_string(), mailbox.to_string()))
}

/// The account of a `draft:<account>/<id>` (or `draft:<account>`) resource.
pub fn draft_resource_account(resource: &str) -> Option<&str> {
    resource.strip_prefix("draft:")?.split('/').next()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// The `result` of a committed response fixture.
    fn fixture_result(raw: &str) -> Value {
        serde_json::from_str::<Value>(raw).expect("fixture is JSON")["result"].clone()
    }

    /// The `params` of a committed notification fixture, as an envelope.
    fn fixture_event(raw: &str) -> EventEnvelope {
        let value: Value = serde_json::from_str(raw).expect("fixture is JSON");
        serde_json::from_value(value["params"].clone()).expect("an event envelope")
    }

    /// A [`Queries`] that answers one canned result and records the call.
    struct Canned {
        answer: Value,
        seen: RefCell<Vec<(String, Value)>>,
    }

    impl Canned {
        fn new(answer: Value) -> Self {
            Canned {
                answer,
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl Queries for Canned {
        fn call(&self, method: &str, params: Value) -> Result<Value> {
            self.seen.borrow_mut().push((method.to_string(), params));
            Ok(self.answer.clone())
        }
    }

    #[test]
    fn the_message_list_fixture_decodes_into_typed_rows() {
        let answer = fixture_result(include_str!(
            "../../mp-protocol/fixtures/message.list.response.json"
        ));
        let rows = decode_message_rows(&answer);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, 3141);
        assert_eq!(rows[0].uid, 1846);
        assert_eq!(rows[0].cc.as_deref(), Some("team@example.com"));
        assert!(rows[0].flags.answered && rows[0].flags.seen);
        assert!(rows[1].flags.flagged && rows[1].has_attachments);
        assert_eq!(
            rows[1].selector,
            "mp://work/inbox/Invoice-2025-114@example.com"
        );
    }

    /// `message_html` addresses the row by `row_id` alone and decodes the
    /// committed answer.
    #[test]
    fn message_html_asks_by_row_id_and_decodes_the_rendition() {
        let door = Canned::new(fixture_result(include_str!(
            "../../mp-protocol/fixtures/message.html.response.json"
        )));
        let answer = message_html(&door, "work", 3141).expect("a rendition");
        assert_eq!(answer.row_id, 3141);
        assert_eq!(answer.bytes, answer.html.len() as u64);
        let seen = door.seen.borrow();
        assert_eq!(seen[0].0, "message.html");
        assert_eq!(seen[0].1, json!({"account": "work", "row_id": 3141}));
    }

    #[test]
    fn list_messages_asks_for_the_whole_mailbox() {
        let door = Canned::new(fixture_result(include_str!(
            "../../mp-protocol/fixtures/message.list.response.json"
        )));
        let rows = list_messages(&door, "work", "inbox").expect("rows");
        assert_eq!(rows.len(), 2);
        let seen = door.seen.borrow();
        assert_eq!(seen[0].0, "message.list");
        assert_eq!(
            seen[0].1,
            json!({"account": "work", "mailbox": "inbox", "limit": null})
        );
    }

    /// A row missing everything but its identity still decodes, and a row
    /// that cannot decode at all costs itself and not the list.
    #[test]
    fn a_sparse_or_broken_row_does_not_stop_the_list() {
        let answer = json!({"messages": [{"id": 7}, {"id": "not a number"}]});
        let rows = decode_message_rows(&answer);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, 7);
        assert_eq!(rows[0].subject, "");
        assert_eq!(rows[1], MessageListRow::default());
    }

    #[test]
    fn the_mailbox_list_fixture_decodes_into_sidebar_rows() {
        let answer = fixture_result(include_str!(
            "../../mp-protocol/fixtures/mailbox.list.response.json"
        ));
        let rows = decode_mailbox_rows(&answer);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[4].slug, "Newsletters");
        assert_eq!(rows[4].unread, 12);
        let totals = totals_by_slug(&rows);
        assert_eq!(totals.get("archive"), Some(&1408));
        assert_eq!(totals.get("drafts"), Some(&2));
    }

    #[test]
    fn the_thread_fixture_decodes_through_the_typed_read() {
        let door = Canned::new(fixture_result(include_str!(
            "../../mp-protocol/fixtures/message.thread.response.json"
        )));
        let listing = thread(&door, "work", 3141).expect("a thread");
        assert_eq!(listing.account, "work");
        assert!(!listing.messages.is_empty());
        assert_eq!(listing.messages[0].id, 3141);
        assert_eq!(
            door.seen.borrow()[0].1,
            json!({"account": "work", "row_id": 3141})
        );
    }

    #[test]
    fn a_search_hit_carries_its_mailbox_and_its_body() {
        let answer = json!({"hits": [
            {"id": 1, "uid": 9, "mailbox": "archive", "subject": "s", "body": "hello"},
            {"id": 2, "uid": 3, "mailbox": "inbox"},
        ]});
        let hits = decode_search_hits(&answer);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].mailbox, "archive");
        assert_eq!(hits[0].row.uid, 9);
        assert_eq!(hits[0].body.as_deref(), Some("hello"));
        assert_eq!(hits[1].body, None);
    }

    #[test]
    fn the_ics_read_decodes_the_base64_the_wire_carries() {
        let door = Canned::new(json!({"ics": BASE64.encode(b"BEGIN:VCALENDAR")}));
        assert_eq!(
            message_ics(&door, "work", 1).expect("bytes"),
            Some(b"BEGIN:VCALENDAR".to_vec())
        );
        let none = Canned::new(json!({"ics": null}));
        assert_eq!(message_ics(&none, "work", 1).expect("no bytes"), None);
    }

    #[test]
    fn an_invite_read_answers_none_for_a_null_event() {
        let door = Canned::new(json!({"event": null}));
        assert!(message_invite(&door, "work", 1).expect("answer").is_none());
    }

    /// The two committed notification fixtures are the two shapes that must
    /// not become a row delta: a removal of a draft, and a counts-scoped
    /// invalidate.
    #[test]
    fn the_committed_remove_and_invalidate_fixtures_are_not_row_deltas() {
        let remove = fixture_event(include_str!(
            "../../mp-protocol/fixtures/notification.state_remove.json"
        ));
        assert_eq!(MessageRowDelta::decode(&remove), None);
        assert_eq!(
            draft_resource_account(remove.payload["resource"].as_str().unwrap()),
            Some("tum")
        );
        let counts = fixture_event(include_str!(
            "../../mp-protocol/fixtures/notification.state_invalidate.json"
        ));
        assert_eq!(MessageRowDelta::decode(&counts), None);
    }

    fn envelope(kind: &str, payload: Value) -> EventEnvelope {
        EventEnvelope {
            instance_id: "i".to_string(),
            revision: 1,
            kind: kind.to_string(),
            payload,
        }
    }

    fn row(id: i64, uid: i64, date_sort: &str) -> MessageListRow {
        MessageListRow {
            id,
            uid,
            date_sort: date_sort.to_string(),
            ..MessageListRow::default()
        }
    }

    #[test]
    fn deltas_fold_into_a_held_list_in_store_order() {
        let mut held = vec![row(3, 30, "2024-01-03"), row(1, 10, "2024-01-01")];

        // A new row between the two.
        let insert = MessageRowDelta::decode(&envelope(
            "message.row",
            json!({"account": "a", "mailbox": "inbox",
                   "message": {"id": 2, "uid": 20, "date_sort": "2024-01-02"}}),
        ))
        .expect("a replace");
        assert!(apply_row_delta(&mut held, "inbox", &insert));
        assert_eq!(held.iter().map(|r| r.id).collect::<Vec<_>>(), vec![3, 2, 1]);

        // A replace of a held row keeps its place.
        let flip = MessageRowDelta::decode(&envelope(
            "message.row",
            json!({"account": "a", "mailbox": "inbox",
                   "message": {"id": 1, "uid": 10, "date_sort": "2024-01-01",
                               "flags": {"seen": true}}}),
        ))
        .expect("a replace");
        assert!(apply_row_delta(&mut held, "inbox", &flip));
        assert!(held[2].flags.seen);

        // A remove by uid, which the typed row carries.
        let remove = MessageRowDelta::decode(&envelope(
            "state.remove",
            json!({"resource": "message:a/inbox/30"}),
        ))
        .expect("a remove");
        assert_eq!(remove.account(), "a");
        assert!(apply_row_delta(&mut held, "inbox", &remove));
        assert_eq!(held.iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 1]);

        // Another mailbox is a no-op; an invalidate owes a refetch.
        assert!(apply_row_delta(&mut held, "archive", &remove));
        let stale = MessageRowDelta::decode(&envelope(
            "state.invalidate",
            json!({"resource": "mailbox:a/inbox", "scope": {"query": "list"}}),
        ))
        .expect("an invalidate");
        assert!(!apply_row_delta(&mut held, "inbox", &stale));
        assert_eq!(held.len(), 2);
    }

    #[test]
    fn a_nested_mailbox_keeps_its_slashes_in_a_message_resource() {
        assert_eq!(
            message_resource("message:a/Projects/2024/17"),
            Some(("a".to_string(), "Projects/2024".to_string(), 17))
        );
        assert_eq!(message_resource("draft:a/x"), None);
        assert_eq!(
            mailbox_resource("mailbox:a/Projects/2024"),
            Some(("a".to_string(), "Projects/2024".to_string()))
        );
    }
}
