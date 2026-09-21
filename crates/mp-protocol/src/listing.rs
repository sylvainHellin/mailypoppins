//! The message listing row, typed, plus the two shapes P5-U10c adds around it
//! (#0126).
//!
//! `message.list` has answered with a row since P2, and every client has read
//! that row by indexing a `serde_json::Value` with string literals. The TUI
//! cannot: `src/tui/app/types.rs`'s `entry_from_row` takes a
//! `store::read::MessageRow`, which is the store's own type and the last
//! reason `src/tui/` imports `crate::store`. This module is the wire type that
//! replaces it, so a client builds a list entry from what the daemon sent and
//! never opens a store to do it.
//!
//! Three rules run through the file:
//!
//! - **Every field but the identity defaults.** The fixture committed at P2
//!   predates `selector`, and a client that refused a row over a field it
//!   would have rendered as empty turns an additive protocol change into a
//!   list that will not paint.
//! - **`cc`, `reply_to` and `bcc` are `Option<String>` and `from`, `to`,
//!   `subject` and `date_display` are `String`.** That is the split
//!   `docs/daemon-protocol.md` documents: an absent Cc is a different fact
//!   from an empty one, and an absent `From:` is not.
//! - **`selector` is the daemon's spelling, not a client's.** Rendering
//!   `mp://<account>/<mailbox>/<key>` client-side would be a second
//!   implementation of `mp_core::selector`'s percent-encoding and of the
//!   `message_key` normalisation, and `tests/cli_selector_contract.rs` pins
//!   only the CLI's. One string on the wire, one grammar in the tree.

use serde::{Deserialize, Serialize};

/// The four flag axes the store keeps, as a listing row carries them.
///
/// An object of four booleans rather than a set of strings, because a renderer
/// branches on each axis independently and a missing axis is `false` rather
/// than unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFlags {
    /// The server has flagged it `\Seen`.
    #[serde(default)]
    pub seen: bool,
    /// `\Answered`.
    #[serde(default)]
    pub answered: bool,
    /// `$Forwarded`.
    #[serde(default)]
    pub forwarded: bool,
    /// `\Flagged`, the star, which is a marker of its own rather than a state
    /// of the read/answered/forwarded axis (P5-U4).
    #[serde(default)]
    pub flagged: bool,
}

/// One row of a `message.list` answer, and the envelope half of a
/// `message.search` hit.
///
/// The field set is the one `docs/daemon-protocol.md` documents and
/// `crates/mp-protocol/fixtures/message.list.response.json` pins, plus
/// `selector` (P5-U10c).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageListRow {
    /// `messages.id`, the synthetic row key a client holds a listed row by and
    /// addresses `message.get` with as `row_id`.
    ///
    /// Per store and per session: it survives no store rebuild, and a client
    /// that persisted one would be naming a row that may since have become
    /// another message.
    pub id: i64,
    /// The server's uid within the mailbox, which with the mailbox makes the
    /// `"<mailbox>/<uid>"` the handle methods take.
    #[serde(default)]
    pub uid: i64,
    /// The `Message-ID:` header verbatim as ingest stored it, angle brackets
    /// included.
    #[serde(default)]
    pub message_id: String,
    /// The `From:` header, `""` when the message carried none.
    #[serde(default)]
    pub from: String,
    /// The `To:` header, `""` when the message carried none.
    #[serde(default)]
    pub to: String,
    /// The `Cc:` header, `null` when the message carried none.
    #[serde(default)]
    pub cc: Option<String>,
    /// The `Reply-To:` header, `null` when the message carried none.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// The `Bcc:` header, `null` when the message carried none.
    #[serde(default)]
    pub bcc: Option<String>,
    /// The `Subject:` header, `""` when the message carried none. A client
    /// renders its own placeholder; the wire does not invent one.
    #[serde(default)]
    pub subject: String,
    /// The UTC sort key every stack orders by, `tui::app::resolve_date`'s.
    #[serde(default)]
    pub date_sort: String,
    /// The `Date:` header as the store holds it, which is the column a listing
    /// prints.
    #[serde(default)]
    pub date_display: String,
    /// The four axes.
    #[serde(default)]
    pub flags: MessageFlags,
    /// Whether the message has user-facing attachments, so a badge costs no
    /// blob read.
    #[serde(default)]
    pub has_attachments: bool,
    /// Whether the row carries an iMIP payload, likewise.
    #[serde(default)]
    pub is_invite: bool,
    /// The canonical `mp://<account>/<mailbox>/<key>` of this message,
    /// rendered daemon-side by `mp_core::selector::Selector::for_message`
    /// (P5-U10c, `RD-07`).
    ///
    /// `""` for a row a daemon older than P5-U10c produced, which is the only
    /// way the field is ever absent: every row this build serves carries one,
    /// because every row has a mailbox and a `Message-ID` key.
    #[serde(default)]
    pub selector: String,
}

/// The whole `message.list` answer in its default `projection: "list"` form.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageListing {
    /// The account that was listed.
    pub account: String,
    /// The mailbox id the daemon resolved, not the spelling that was sent.
    pub mailbox: String,
    /// How many messages the mailbox holds, ignoring `limit`.
    #[serde(default)]
    pub total: u64,
    /// The rows, in `date_sort DESC, id DESC`.
    #[serde(default)]
    pub messages: Vec<MessageListRow>,
}

/// One message of a conversation, as `message.thread` answers it (P5-U10d,
/// `LST-10`).
///
/// A row of its own rather than a [`MessageListRow`], because a conversation
/// crosses mailboxes: a listing names its mailbox once and every row of it is
/// in that mailbox, while a thread holds the Inbox copy and the archived
/// original side by side and the overlay switches mailbox when it opens one.
/// It carries what the overlay renders and nothing else, which is why there is
/// no `date_sort` here: the daemon orders the conversation oldest first, and a
/// client that re-sorted it would be inventing an order the overlay does not
/// have.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadMessage {
    /// `messages.id`, the same synthetic row key [`MessageListRow`] carries
    /// under the same name, which is what the overlay opens the message with.
    pub id: i64,
    /// The mailbox this copy lives in, as the store holds it.
    ///
    /// The one field a listing row has no use for: the overlay prints it and
    /// its `Enter` switches mailbox when the highlighted message is in another
    /// one.
    #[serde(default)]
    pub mailbox: String,
    /// The `Message-ID:` header verbatim, which is the identity the
    /// conversation is deduped over: one logical message is one row even when
    /// the store holds it twice.
    #[serde(default)]
    pub message_id: String,
    /// The `From:` header, `""` when the message carried none. The display
    /// name is extracted client-side, as it is for a listing row.
    #[serde(default)]
    pub from: String,
    /// The `Date:` header as the store holds it, which is the column the
    /// overlay prints.
    #[serde(default)]
    pub date_display: String,
    /// The four flag axes, which draw the same markers a list row draws.
    #[serde(default)]
    pub flags: MessageFlags,
    /// Whether this is the message the conversation was opened from.
    ///
    /// Decided daemon-side on the `Message-ID`, not on the row id: the store
    /// may hold two copies of the addressed message and the dedup keeps the
    /// first one ingest saw, so the row marked `current` can carry an `id`
    /// other than the one the call addressed.
    #[serde(default)]
    pub current: bool,
}

/// The whole `message.thread` answer: one conversation, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadListing {
    /// The account the conversation was read from.
    pub account: String,
    /// The `thread_id` ingest assigned, which is the `Message-ID` of the root
    /// the `In-Reply-To` / `References` chain resolved to, and the addressed
    /// message's own `Message-ID` when ingest assigned it none.
    #[serde(default)]
    pub thread_id: String,
    /// The `Subject:` of the message the conversation was opened from, `""`
    /// when it carried none: the overlay's title renders its own placeholder,
    /// and the wire does not invent one.
    #[serde(default)]
    pub subject: String,
    /// The conversation, oldest first (`date_sort ASC, id ASC`), one row per
    /// `Message-ID`, and always including the addressed message.
    ///
    /// A message with no relatives is therefore a one-row answer rather than
    /// an empty one: "this message is its own conversation" is a fact the
    /// client renders its own sentence for, and an empty list would make it
    /// indistinguishable from a thread whose rows were all evicted.
    #[serde(default)]
    pub messages: Vec<ThreadMessage>,
}

/// One hit of the `message.search_server` operation, as the `message.server_hit`
/// event carries it (P5-U10c, `LST-08`).
///
/// It is a server envelope rather than a store row: the message may never have
/// been ingested, so there is no `messages.id` for it and no selector that
/// would name it for the CLI. What the overlay renders today is exactly this
/// set, plus the two body renditions it previews and hands to a browser.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSearchHit {
    /// The account the search ran against.
    pub account: String,
    /// The sidebar label of the mailbox the hit came from, which is the
    /// overlay's `source_label` column.
    #[serde(default)]
    pub mailbox: String,
    /// The `Message-ID:` header, `null` for a server that returned none. It is
    /// the dedup key and the address `message.fetch` takes, so a hit without
    /// one can be read and not fetched.
    #[serde(default)]
    pub message_id: Option<String>,
    /// The row this hit resolved to in the account's local store, `null` for a
    /// server-only hit.
    ///
    /// The resolution is the daemon's, by `Message-ID`, and it is what makes
    /// the local pass and the server leg one list: a hit the local pass
    /// already showed arrives with the same `row_id` and is dropped.
    #[serde(default)]
    pub row_id: Option<i64>,
    /// The canonical selector of `row_id`'s row, `null` for a server-only hit.
    #[serde(default)]
    pub selector: Option<String>,
    /// The `From:` header.
    #[serde(default)]
    pub from: String,
    /// The `To:` header.
    #[serde(default)]
    pub to: String,
    /// The `Cc:` header, `null` when absent.
    #[serde(default)]
    pub cc: Option<String>,
    /// The `Reply-To:` header, `null` when absent.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// The `Bcc:` header, `null` when absent.
    #[serde(default)]
    pub bcc: Option<String>,
    /// The `Subject:` header.
    #[serde(default)]
    pub subject: String,
    /// The `Date:` header verbatim, which the overlay renders and sorts by
    /// after the daemon has derived `date_sort` from it.
    #[serde(default)]
    pub date_display: String,
    /// The UTC sort key, derived the same way a listing row's is.
    #[serde(default)]
    pub date_sort: String,
    /// The flags the server reported.
    #[serde(default)]
    pub flags: MessageFlags,
    /// Whether the message has attachments.
    #[serde(default)]
    pub has_attachments: bool,
    /// Whether it carries an iMIP payload.
    #[serde(default)]
    pub is_invite: bool,
    /// The flattened text the preview pane shows, `""` when the server sent
    /// none.
    #[serde(default)]
    pub body_text: String,
    /// The sender's markup, `null` when there is none. The overlay's `b` key
    /// renders it for a browser, and for a server-only hit this is the only
    /// copy of it anywhere.
    #[serde(default)]
    pub html_body: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The committed fixture decodes into the typed row, `selector` included.
    #[test]
    fn the_committed_listing_fixture_decodes_whole() {
        let raw = include_str!("../fixtures/message.list.response.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let listing: MessageListing =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");

        assert_eq!(listing.account, "work");
        assert_eq!(listing.mailbox, "inbox");
        assert_eq!(listing.messages.len(), 2);

        let first = &listing.messages[0];
        assert_eq!(first.id, 3141);
        assert_eq!(first.uid, 1846);
        assert_eq!(first.message_id, "<Bericht@example.com>");
        assert_eq!(first.cc.as_deref(), Some("team@example.com"));
        assert_eq!(first.reply_to, None);
        assert!(first.flags.seen && first.flags.answered);
        assert!(!first.flags.flagged);
        assert_eq!(
            first.selector, "mp://work/inbox/Bericht@example.com",
            "every row carries the daemon's own spelling of its selector"
        );
    }

    /// A row from a daemon that predates `selector` still decodes: a field a
    /// client would have rendered as empty may not stop a list from painting.
    #[test]
    fn a_row_without_a_selector_still_decodes() {
        let row: MessageListRow = serde_json::from_value(json!({"id": 7}))
            .expect("the identity is the only required field");
        assert_eq!(row.id, 7);
        assert_eq!(row.selector, "");
        assert_eq!(row.cc, None);
        assert_eq!(row.subject, "");
        assert_eq!(row.flags, MessageFlags::default());
    }

    /// Round trip, which is what lets a test build a row by hand and trust the
    /// wire.
    #[test]
    fn a_listing_round_trips() {
        let listing = MessageListing {
            account: "work".to_string(),
            mailbox: "inbox".to_string(),
            total: 1,
            messages: vec![MessageListRow {
                id: 12,
                uid: 41,
                message_id: "<a@example.com>".to_string(),
                from: "Ivana <ivana@example.com>".to_string(),
                to: "alice@example.com".to_string(),
                cc: Some("team@example.com".to_string()),
                reply_to: None,
                bcc: None,
                subject: "Bericht".to_string(),
                date_sort: "2026-07-02T11:57:30".to_string(),
                date_display: "Thu, 2 Jul 2026 13:57:30 +0200".to_string(),
                flags: MessageFlags {
                    seen: true,
                    answered: false,
                    forwarded: false,
                    flagged: true,
                },
                has_attachments: true,
                is_invite: false,
                selector: "mp://work/inbox/a@example.com".to_string(),
            }],
        };
        let encoded = serde_json::to_value(&listing).expect("it serialises");
        assert_eq!(encoded["messages"][0]["flags"]["flagged"], json!(true));
        assert_eq!(
            encoded["messages"][0]["selector"],
            json!("mp://work/inbox/a@example.com")
        );
        let decoded: MessageListing = serde_json::from_value(encoded).expect("it decodes");
        assert_eq!(decoded, listing);
    }

    /// A server-only hit has no row and no selector, and says so with `null`
    /// rather than with an empty string: "not in the store" is a fact a client
    /// branches on, and `""` would make it indistinguishable from a row whose
    /// selector the daemon failed to render.
    #[test]
    fn a_server_only_hit_carries_no_row_and_no_selector() {
        let hit: ServerSearchHit = serde_json::from_value(json!({
            "account": "work",
            "mailbox": "Archive",
            "message_id": "<only-on-the-server@example.com>",
            "subject": "Angebot",
            "body_text": "hier ist das Angebot\n",
        }))
        .expect("a hit decodes");
        assert_eq!(hit.row_id, None);
        assert_eq!(hit.selector, None);
        assert_eq!(hit.html_body, None);
        assert_eq!(hit.body_text, "hier ist das Angebot\n");
    }

    /// The committed conversation fixture decodes whole: the rows are oldest
    /// first, exactly one of them is `current`, and each carries the mailbox
    /// its copy lives in.
    #[test]
    fn the_committed_thread_fixture_decodes_whole() {
        let raw = include_str!("../fixtures/message.thread.response.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let thread: ThreadListing =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");

        assert_eq!(thread.account, "work");
        assert_eq!(thread.thread_id, "<Bericht@example.com>");
        assert_eq!(thread.subject, "Re: Bericht");
        assert_eq!(thread.messages.len(), 3);
        assert_eq!(
            thread.messages[0].mailbox, "inbox",
            "a conversation crosses mailboxes and each row says where it is"
        );
        assert_eq!(thread.messages[1].mailbox, "sent");
        assert_eq!(
            thread
                .messages
                .iter()
                .filter(|message| message.current)
                .count(),
            1,
            "exactly one row is the message the conversation was opened from"
        );
        assert!(thread.messages[2].current);
        assert!(thread.messages[0].flags.seen && thread.messages[0].flags.answered);
    }

    /// A thread row from a daemon that predates a later field still decodes,
    /// and a lone message is a one-row conversation rather than an empty one.
    #[test]
    fn a_lone_message_is_a_one_row_conversation() {
        let thread: ThreadListing = serde_json::from_value(json!({
            "account": "work",
            "thread_id": "<lonely@example.com>",
            "subject": "",
            "messages": [{"id": 7, "current": true}],
        }))
        .expect("the account is the only required field");
        assert_eq!(thread.messages.len(), 1);
        assert_eq!(thread.messages[0].id, 7);
        assert_eq!(thread.messages[0].mailbox, "");
        assert_eq!(thread.messages[0].flags, MessageFlags::default());
        assert!(thread.messages[0].current);
    }

    /// Round trip, which is what lets a test build a conversation by hand and
    /// trust the wire.
    #[test]
    fn a_thread_round_trips() {
        let thread = ThreadListing {
            account: "work".to_string(),
            thread_id: "<root@example.com>".to_string(),
            subject: "Bericht".to_string(),
            messages: vec![ThreadMessage {
                id: 12,
                mailbox: "Team/Reports".to_string(),
                message_id: "<root@example.com>".to_string(),
                from: "Ivana <ivana@example.com>".to_string(),
                date_display: "Thu, 2 Jul 2026 13:57:30 +0200".to_string(),
                flags: MessageFlags {
                    seen: true,
                    answered: false,
                    forwarded: false,
                    flagged: true,
                },
                current: false,
            }],
        };
        let encoded = serde_json::to_value(&thread).expect("it serialises");
        assert_eq!(encoded["messages"][0]["mailbox"], json!("Team/Reports"));
        assert_eq!(encoded["messages"][0]["flags"]["flagged"], json!(true));
        let decoded: ThreadListing = serde_json::from_value(encoded).expect("it decodes");
        assert_eq!(decoded, thread);
    }
}
