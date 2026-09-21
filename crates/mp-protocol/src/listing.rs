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
}
