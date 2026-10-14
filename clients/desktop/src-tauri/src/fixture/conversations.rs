//! The fixture's conversations (#0142, PERSO-138): `message.list_conversations`
//! and `message.thread` answered from the fixture's rows, with no stream.
//!
//! The fixture's rows carry no threading headers, so each message is its own
//! conversation, keyed by its `Message-ID` as the daemon keys a row with no
//! `thread_id`, except the one canned thread [`CANNED_THREAD`]: the offsite
//! question in `work`'s inbox, the agenda and the reply in Sent, and the
//! draft `offsite-note`, which is the acceptance case of the ticket (one row
//! with `(3)` and the draft marker, four members on expansion).
//!
//! The fold follows the daemon's rules (`docs/daemon-protocol.md`, "The
//! conversation listing"): the date is the newest member's across every
//! mailbox, drafts excluded; `latest` and `member_ids` are the listed
//! mailbox's copies; `count` and `unread` count `Message-ID`s; the order is
//! `date_sort DESC`, then `latest.id DESC`.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use mp_protocol::listing::{
    ConversationListing, ConversationRow, MessageFlags, MessageListRow, ThreadListing,
    ThreadMessage,
};
use serde_json::Value;

/// The `thread_id` of the fixture's one conversation of several messages:
/// the `Message-ID` of its root, the offsite question in `work`'s inbox.
pub const CANNED_THREAD: &str = "<team-offsite-in-october-1007@fixture.example>";

/// The members of [`CANNED_THREAD`] by `Message-ID`: the question (inbox),
/// the agenda and the reply (Sent).
const CANNED_MEMBERS: &[&str] = &[
    "<team-offsite-in-october-1007@fixture.example>",
    "<draft-agenda-for-the-off-1010@fixture.example>",
    "<re-team-offsite-in-octob-1000@fixture.example>",
];

/// The account and draft id of the draft that answers [`CANNED_THREAD`].
pub const CANNED_DRAFT: (&str, &str) = ("work", "offsite-note");

/// The store's mailbox of local drafts, whose rows no conversation counts.
const DRAFTS: &str = "drafts";

/// The most participants a row carries.
const PARTICIPANTS: usize = 5;

/// A draft that joins a conversation: its id, its frontmatter date and the
/// `thread_id` of the message it answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftMember {
    pub id: String,
    pub date: String,
    pub thread_id: String,
}

/// The conversation a fixture row belongs to.
pub fn thread_id_of(row: &Value) -> String {
    let message_id = row["message_id"].as_str().unwrap_or_default();
    if CANNED_MEMBERS.contains(&message_id) {
        CANNED_THREAD.to_string()
    } else {
        message_id.to_string()
    }
}

/// A fixture row as the listing row it is, normalised as `message.list`
/// sends it ([`super::wire_row`]: an absent or `null` header is `""`). A row
/// that still does not decode is a broken fixture, and says so.
fn list_row(row: &Value) -> MessageListRow {
    serde_json::from_value(super::wire_row(row))
        .unwrap_or_else(|e| panic!("fixture row {} is not a listing row: {e}", row["id"]))
}

/// Every row of the account outside Drafts, with its mailbox.
fn rows_of(boxes: &BTreeMap<String, Vec<Value>>) -> Vec<(&str, &Value)> {
    boxes
        .iter()
        .filter(|(mailbox, _)| mailbox.as_str() != DRAFTS)
        .flat_map(|(mailbox, rows)| rows.iter().map(move |r| (mailbox.as_str(), r)))
        .collect()
}

/// The address of a `From:` header, lowercased, for the participant dedup.
fn address(from: &str) -> String {
    let inner = match (from.rfind('<'), from.rfind('>')) {
        (Some(open), Some(close)) if open < close => &from[open + 1..close],
        _ => from,
    };
    inner.trim().to_lowercase()
}

/// Newest first: `date_sort DESC`, then `id DESC`.
fn newest_first(a: &MessageListRow, b: &MessageListRow) -> std::cmp::Ordering {
    b.date_sort.cmp(&a.date_sort).then(b.id.cmp(&a.id))
}

/// One mailbox of one account as conversations, the fold
/// `message.list_conversations` streams.
pub fn list(
    account: &str,
    mailbox: &str,
    boxes: &BTreeMap<String, Vec<Value>>,
    drafts: &[DraftMember],
) -> ConversationListing {
    let mut threads: BTreeMap<String, Vec<(&str, MessageListRow)>> = BTreeMap::new();
    for (in_box, row) in rows_of(boxes) {
        threads
            .entry(thread_id_of(row))
            .or_default()
            .push((in_box, list_row(row)));
    }
    let drafted: HashSet<&str> = drafts.iter().map(|d| d.thread_id.as_str()).collect();
    let mut conversations: Vec<ConversationRow> = threads
        .into_iter()
        .filter_map(|(thread_id, mut members)| {
            members.sort_by(|a, b| newest_first(&a.1, &b.1));
            let here: Vec<&MessageListRow> = members
                .iter()
                .filter(|(m, _)| *m == mailbox)
                .map(|(_, r)| r)
                .collect();
            let latest = (*here.first()?).clone();
            let newest = &members[0].1;
            let ids: BTreeSet<&str> = members.iter().map(|(_, r)| r.message_id.as_str()).collect();
            let unread: BTreeSet<&str> = here
                .iter()
                .filter(|r| !r.flags.seen)
                .map(|r| r.message_id.as_str())
                .collect();
            let mut seen = HashSet::new();
            let participants = members
                .iter()
                .filter(|(_, r)| seen.insert(address(&r.from)))
                .take(PARTICIPANTS)
                .map(|(_, r)| r.from.clone())
                .collect();
            let mailboxes: BTreeSet<String> = members.iter().map(|(m, _)| m.to_string()).collect();
            Some(ConversationRow {
                count: ids.len() as u64,
                date_display: newest.date_display.clone(),
                date_sort: newest.date_sort.clone(),
                flagged: here.iter().any(|r| r.flags.flagged),
                has_attachments: members.iter().any(|(_, r)| r.has_attachments),
                has_draft: drafted.contains(thread_id.as_str()),
                member_ids: here.iter().map(|r| r.id).collect(),
                mailboxes: mailboxes.into_iter().collect(),
                participants,
                unread: unread.len() as u64,
                latest,
                thread_id,
            })
        })
        .collect();
    conversations.sort_by(|a, b| {
        b.date_sort
            .cmp(&a.date_sort)
            .then(b.latest.id.cmp(&a.latest.id))
    });
    ConversationListing {
        account: account.to_string(),
        mailbox: mailbox.to_string(),
        total: conversations.len() as u64,
        conversations,
    }
}

/// `message.thread` of one row: its conversation oldest first, one member
/// per `Message-ID` (the copy in `prefer_mailbox` when there is one, else
/// the oldest), and with `drafts` the drafts that join it, by their date
/// among the messages. `None` for a row the account does not hold.
pub fn thread(
    account: &str,
    boxes: &BTreeMap<String, Vec<Value>>,
    row_id: i64,
    prefer_mailbox: Option<&str>,
    drafts: &[DraftMember],
    from: &str,
) -> Option<ThreadListing> {
    let rows = rows_of(boxes);
    let (_, addressed) = rows
        .iter()
        .find(|(_, r)| r["id"].as_i64() == Some(row_id))?;
    let thread_id = thread_id_of(addressed);
    let current = list_row(addressed);
    let mut members: Vec<(&str, MessageListRow)> = rows
        .iter()
        .filter(|(_, r)| thread_id_of(r) == thread_id)
        .map(|(m, r)| (*m, list_row(r)))
        .collect();
    members.sort_by(|a, b| a.1.date_sort.cmp(&b.1.date_sort).then(a.1.id.cmp(&b.1.id)));
    let mut kept: Vec<(&str, MessageListRow)> = Vec::new();
    for (mailbox, row) in members {
        match kept
            .iter_mut()
            .find(|(_, k)| k.message_id == row.message_id)
        {
            Some(slot) => {
                if prefer_mailbox == Some(mailbox) && Some(slot.0) != prefer_mailbox {
                    *slot = (mailbox, row);
                }
            }
            None => kept.push((mailbox, row)),
        }
    }
    let mut messages: Vec<(String, ThreadMessage)> = kept
        .into_iter()
        .map(|(mailbox, row)| {
            let member = ThreadMessage {
                id: row.id,
                mailbox: mailbox.to_string(),
                current: row.message_id == current.message_id,
                message_id: row.message_id,
                from: row.from,
                date_display: row.date_display,
                flags: row.flags,
                draft_id: None,
            };
            (row.date_sort, member)
        })
        .collect();
    for draft in drafts.iter().filter(|d| d.thread_id == thread_id) {
        let member = ThreadMessage {
            id: 0,
            mailbox: DRAFTS.to_string(),
            message_id: String::new(),
            from: from.to_string(),
            date_display: draft.date.clone(),
            flags: MessageFlags::default(),
            current: false,
            draft_id: Some(draft.id.clone()),
        };
        let at = messages
            .iter()
            .position(|(date, _)| *date > draft.date)
            .unwrap_or(messages.len());
        messages.insert(at, (draft.date.clone(), member));
    }
    Some(ThreadListing {
        account: account.to_string(),
        thread_id,
        subject: current.subject,
        messages: messages.into_iter().map(|(_, m)| m).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: i64, message_id: &str, from: &str, date: &str, seen: bool) -> Value {
        json!({
            "id": id, "uid": id, "message_id": message_id, "from": from,
            "subject": format!("subject {id}"), "date_sort": date,
            "date_display": format!("display {date}"),
            "flags": {"seen": seen, "answered": false, "forwarded": false, "flagged": false},
            "has_attachments": id == 1010, "is_invite": false,
            "body": "fixture only", "attachments": []
        })
    }

    /// The offsite thread across inbox and Sent, plus one lone inbox message.
    fn boxes() -> BTreeMap<String, Vec<Value>> {
        let mut b = BTreeMap::new();
        b.insert(
            "inbox".to_string(),
            vec![
                row(
                    1002,
                    "<lone@fixture.example>",
                    "Robin <robin@example.com>",
                    "2026-09-29T07:40:00",
                    false,
                ),
                row(
                    1007,
                    CANNED_MEMBERS[0],
                    "Marta <marta@example.com>",
                    "2026-09-26T13:15:00",
                    false,
                ),
            ],
        );
        b.insert(
            "sent".to_string(),
            vec![
                row(
                    1000,
                    CANNED_MEMBERS[2],
                    "Me <me@example.com>",
                    "2026-09-27T09:20:00",
                    true,
                ),
                row(
                    1010,
                    CANNED_MEMBERS[1],
                    "Me <ME@example.com>",
                    "2026-09-26T15:02:00",
                    true,
                ),
            ],
        );
        b
    }

    fn draft() -> Vec<DraftMember> {
        vec![DraftMember {
            id: "offsite-note".to_string(),
            date: "2026-09-27T18:40:00".to_string(),
            thread_id: CANNED_THREAD.to_string(),
        }]
    }

    /// The inbox lists the canned thread as one row of three with the draft
    /// marker and the later sent reply's date, beside the lone message.
    #[test]
    fn the_inbox_folds_the_canned_thread() {
        let listing = list("work", "inbox", &boxes(), &draft());
        assert_eq!(listing.total, 2);
        let lone = &listing.conversations[0];
        assert_eq!(lone.thread_id, "<lone@fixture.example>");
        assert_eq!((lone.count, lone.unread, lone.has_draft), (1, 1, false));
        let offsite = &listing.conversations[1];
        assert_eq!(offsite.thread_id, CANNED_THREAD);
        assert_eq!(offsite.count, 3);
        assert!(offsite.has_draft);
        assert!(offsite.has_attachments, "an attachment in Sent counts");
        assert_eq!(offsite.date_sort, "2026-09-27T09:20:00");
        assert_eq!(offsite.latest.id, 1007);
        assert_eq!(offsite.member_ids, [1007]);
        assert_eq!(offsite.unread, 1);
        assert_eq!(offsite.mailboxes, ["inbox", "sent"]);
        assert_eq!(
            offsite.participants,
            ["Me <me@example.com>", "Marta <marta@example.com>"],
            "one per address, compared case-insensitively, newest first"
        );
    }

    /// Sent lists the same conversation, held by its two sent copies.
    #[test]
    fn sent_lists_the_same_conversation() {
        let listing = list("work", "sent", &boxes(), &draft());
        assert_eq!(listing.total, 1);
        let offsite = &listing.conversations[0];
        assert_eq!(offsite.member_ids, [1000, 1010]);
        assert_eq!(offsite.latest.id, 1000);
        assert_eq!((offsite.count, offsite.unread), (3, 0));
    }

    /// The expansion is oldest first, the draft placed by its date, and a
    /// thread without the draft list carries none.
    #[test]
    fn the_thread_lists_four_members_oldest_first() {
        let t = thread(
            "work",
            &boxes(),
            1007,
            Some("inbox"),
            &draft(),
            "Me <me@example.com>",
        )
        .expect("thread");
        let labels: Vec<(&str, i64)> = t
            .messages
            .iter()
            .map(|m| (m.mailbox.as_str(), m.id))
            .collect();
        assert_eq!(
            labels,
            [
                ("inbox", 1007),
                ("sent", 1010),
                ("sent", 1000),
                ("drafts", 0)
            ]
        );
        assert_eq!(t.messages[3].draft_id.as_deref(), Some("offsite-note"));
        assert!(t.messages[0].current);
        assert_eq!(t.thread_id, CANNED_THREAD);
        let bare = thread("work", &boxes(), 1007, None, &[], "x").expect("thread");
        assert_eq!(bare.messages.len(), 3);
        assert!(thread("work", &boxes(), 99, None, &[], "x").is_none());
    }

    /// Every mailbox of the shipped fixtures folds into rows that name real
    /// messages: a row with a `null` header (`home`'s newsletter 1021) is
    /// normalised, not decoded into a default row of id 0.
    #[test]
    fn every_fixture_conversation_names_real_rows() {
        let all: BTreeMap<String, BTreeMap<String, Vec<Value>>> =
            serde_json::from_str(super::super::MESSAGES).expect("messages.json");
        let mut listed = 0;
        for (account, boxes) in &all {
            for mailbox in boxes.keys().filter(|m| m.as_str() != DRAFTS) {
                let listing = list(account, mailbox, boxes, &[]);
                assert!(listing.total > 0, "{account}/{mailbox} lists nothing");
                for c in &listing.conversations {
                    let at = format!("{account}/{mailbox} {}", c.thread_id);
                    assert_ne!(c.latest.id, 0, "{at}: latest.id");
                    assert!(!c.member_ids.is_empty(), "{at}: no members");
                    assert!(c.member_ids.iter().all(|&id| id != 0), "{at}: member_ids");
                    let t = thread(account, boxes, c.latest.id, Some(mailbox), &[], "x")
                        .expect("the latest row has a thread");
                    assert!(t.messages.iter().all(|m| m.id != 0), "{at}: thread ids");
                    listed += 1;
                }
            }
        }
        assert!(listed > 0);
        let newsletters = list("home", "newsletters", &all["home"], &[]);
        let row = newsletters
            .conversations
            .iter()
            .find(|c| c.latest.id == 1021)
            .expect("the newsletter with no subject is listed");
        assert_eq!(row.latest.subject, "");
    }

    /// One `Message-ID` held twice counts once, and the expansion names the
    /// copy in the preferred mailbox.
    #[test]
    fn a_message_held_twice_counts_once() {
        let mut b = boxes();
        b.insert(
            "archive".to_string(),
            vec![row(
                1050,
                CANNED_MEMBERS[0],
                "Marta <marta@example.com>",
                "2026-09-26T13:15:00",
                true,
            )],
        );
        let offsite = list("work", "inbox", &b, &[])
            .conversations
            .into_iter()
            .find(|c| c.thread_id == CANNED_THREAD)
            .expect("row");
        assert_eq!(offsite.count, 3);
        let inbox = thread("work", &b, 1050, Some("inbox"), &[], "x").expect("thread");
        assert_eq!(inbox.messages.len(), 3);
        assert_eq!(inbox.messages[0].id, 1007);
        let archive = thread("work", &b, 1007, Some("archive"), &[], "x").expect("thread");
        assert_eq!(archive.messages[0].id, 1050);
    }
}
