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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
    /// rendered daemon-side by `mp_core::selector::Selector::for_listed`
    /// (P5-U10c, `RD-07`), with `/<uid>` appended when its mailbox holds
    /// another row under the same Message-ID, so it names this copy
    /// (PERSO-244).
    ///
    /// `""` for a row a daemon older than P5-U10c produced, which is the only
    /// way the field is ever absent: every row this build serves carries one,
    /// because every row has a mailbox and a `Message-ID` key.
    #[serde(default)]
    pub selector: String,
}

/// The whole `message.list` answer in its default `projection: "list"` form.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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

/// The operation that streams one whole mailbox to the calling connection
/// (#0138).
///
/// A twin of `message.list` named after how it differs, as
/// `message.search_server` is: it answers [`MessageListStreamStarted`] at
/// once and then sends the rows as [`MessageRowsChunk`] notifications of method
/// [`crate::METHOD_MESSAGE_ROWS`], so a mailbox past the 16 MiB response cap
/// still lists.
pub const METHOD_MESSAGE_LIST_STREAM: &str = "message.list_stream";

/// The immediate answer of `message.list_stream` (#0138).
///
/// An operation answer with members beside the id, on the precedent of
/// `send.draft`'s `{operation_id, held}`: the daemon reads the whole mailbox
/// before it answers, so `total` is the exact number of rows the stream will
/// carry, and a client checks the stream against it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MessageListStreamStarted {
    /// The operation the rows and the finish belong to.
    pub operation_id: String,
    /// The account that is being listed.
    pub account: String,
    /// The mailbox id the daemon resolved, not the spelling that was sent.
    pub mailbox: String,
    /// How many rows the stream carries, which is the mailbox's row count at
    /// the one read the daemon made.
    #[serde(default)]
    pub total: u64,
}

/// The `params` of one `message.rows` notification: a contiguous run of a
/// streamed listing (#0138).
///
/// `offset` is the position of the first row in the stream, not a request
/// parameter: the first chunk starts at `0`, each next one where the last one
/// ended, and the last one ends at the answer's `total`. A client that sees a
/// gap, or a row past `total`, holds a broken stream and keeps the list it had.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MessageRowsChunk {
    /// The position of `rows[0]` in the stream.
    pub offset: u64,
    /// The `message.list_stream` operation this chunk belongs to.
    pub operation_id: String,
    /// The rows, `message.list` rows byte for byte, newest first.
    #[serde(default)]
    pub rows: Vec<MessageListRow>,
}

/// How every `message.rows` frame a daemon writes begins: the envelope's keys
/// sorted and compact, the bytes `frame::encode` of the same notification
/// produces and the daemon's chunk encoder writes by hand (#0138).
pub const MESSAGE_ROWS_FRAME_PREFIX: &str = r#"{"jsonrpc":"2.0","method":"message.rows","params":"#;

impl MessageRowsChunk {
    /// One frame line decoded straight into the chunk it carries, without the
    /// `serde_json::Value` tree a generic frame decode builds (PERSO-106).
    ///
    /// `None` for a line that is not a `message.rows` frame in the daemon's
    /// own spelling ([`MESSAGE_ROWS_FRAME_PREFIX`]) or that does not decode as
    /// one: the caller then decodes the line generically, so a chunk spelled
    /// differently still arrives, and a malformed one is reported by whoever
    /// reads it rather than lost here.
    pub fn from_frame_line(line: &str) -> Option<MessageRowsChunk> {
        /// The envelope around the params; `jsonrpc` and `method` are what
        /// the prefix already matched.
        #[derive(Deserialize)]
        struct Frame {
            params: MessageRowsChunk,
        }
        if !line.starts_with(MESSAGE_ROWS_FRAME_PREFIX) {
            return None;
        }
        serde_json::from_str::<Frame>(line)
            .ok()
            .map(|frame| frame.params)
    }
}

/// The operation that streams one mailbox as conversations to the calling
/// connection (#0142, PERSO-138).
///
/// A twin of [`METHOD_MESSAGE_LIST_STREAM`] named after how it differs: it
/// answers [`MessageListStreamStarted`] at once, with `total` the number of
/// conversations, and then sends the rows as [`ConversationRowsChunk`]
/// notifications of method [`crate::METHOD_MESSAGE_CONVERSATIONS`], on every
/// rule of the flat stream.
pub const METHOD_MESSAGE_LIST_CONVERSATIONS: &str = "message.list_conversations";

/// One conversation of a mailbox, as `message.list_conversations` streams it
/// (#0142, PERSO-138).
///
/// The fields are declared in key order on purpose, as the daemon's own wire
/// row is: `serde_json` without `preserve_order` sorts an object's keys, so
/// this order is the one the hand-built frame and `frame::encode` of the same
/// value both put on the wire.
///
/// A conversation is the set of rows ingest gave one `thread_id`, across every
/// mailbox, the rows of the account's `drafts` mailbox excluded; a row with no
/// `thread_id` is a conversation of one keyed by its own `Message-ID`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ConversationRow {
    /// The conversation's messages across every mailbox, one per `Message-ID`.
    #[serde(default)]
    pub count: u64,
    /// The `Date:` of the newest member across every mailbox, which is the
    /// date the row prints: a reply sent today lifts an inbox conversation.
    #[serde(default)]
    pub date_display: String,
    /// The UTC sort key of that newest member, spelled as a listing row's.
    #[serde(default)]
    pub date_sort: String,
    /// Whether a member in the listed mailbox is flagged, the members the flag
    /// action on the row toggles.
    #[serde(default)]
    pub flagged: bool,
    /// Whether any member, in any mailbox, has an attachment.
    #[serde(default)]
    pub has_attachments: bool,
    /// Whether an indexed draft answers or forwards a member.
    #[serde(default)]
    pub has_draft: bool,
    /// The newest copy of a member in the listed mailbox, a `message.list` row
    /// byte for byte; the row a collapsed conversation previews.
    #[serde(default)]
    pub latest: MessageListRow,
    /// The distinct mailboxes the members live in, sorted.
    #[serde(default)]
    pub mailboxes: Vec<String>,
    /// The `messages.id` of every member row in the listed mailbox, newest
    /// first, two copies of one `Message-ID` included: each is a server copy
    /// an action on the row has to reach.
    #[serde(default)]
    pub member_ids: Vec<i64>,
    /// The members' `From:` headers verbatim, one per address compared
    /// case-insensitively, newest first, at most five.
    #[serde(default)]
    pub participants: Vec<String>,
    /// The `thread_id` the members share, which `message.thread` and a
    /// client's selection hold the row by.
    pub thread_id: String,
    /// The `Message-ID`s with an unseen copy in the listed mailbox.
    #[serde(default)]
    pub unread: u64,
}

/// The `params` of one `message.conversations` notification: a contiguous run
/// of a `message.list_conversations` stream, under the rules of
/// [`MessageRowsChunk`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ConversationRowsChunk {
    /// The position of `rows[0]` in the stream.
    pub offset: u64,
    /// The `message.list_conversations` operation this chunk belongs to.
    pub operation_id: String,
    /// The conversations, newest first.
    #[serde(default)]
    pub rows: Vec<ConversationRow>,
}

/// How every `message.conversations` frame a daemon writes begins, the twin of
/// [`MESSAGE_ROWS_FRAME_PREFIX`].
pub const CONVERSATION_ROWS_FRAME_PREFIX: &str =
    r#"{"jsonrpc":"2.0","method":"message.conversations","params":"#;

impl ConversationRowsChunk {
    /// One frame line decoded straight into the chunk it carries, as
    /// [`MessageRowsChunk::from_frame_line`] does for the flat stream.
    ///
    /// `None` for a line that is not a `message.conversations` frame in the
    /// daemon's own spelling or that does not decode as one, which the caller
    /// then decodes generically.
    pub fn from_frame_line(line: &str) -> Option<ConversationRowsChunk> {
        /// The envelope around the params.
        #[derive(Deserialize)]
        struct Frame {
            params: ConversationRowsChunk,
        }
        if !line.starts_with(CONVERSATION_ROWS_FRAME_PREFIX) {
            return None;
        }
        serde_json::from_str::<Frame>(line)
            .ok()
            .map(|frame| frame.params)
    }
}

/// One whole `message.list_conversations` stream, collected: the twin of
/// [`MessageListing`] a client holds a grouped mailbox in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ConversationListing {
    /// The account that was listed.
    pub account: String,
    /// The mailbox id the daemon resolved, not the spelling that was sent.
    pub mailbox: String,
    /// How many conversations the stream carried.
    #[serde(default)]
    pub total: u64,
    /// The conversations, newest first.
    #[serde(default)]
    pub conversations: Vec<ConversationRow>,
}

/// One message of a conversation, as `message.thread` answers it (P5-U10d,
/// `LST-10`).
///
/// A row of its own rather than a [`MessageListRow`], because a conversation
/// crosses mailboxes: a listing names its mailbox once and every row of it is
/// in that mailbox, while a thread holds the Inbox copy and the archived
/// original side by side and the overlay switches mailbox when it opens one.
/// It carries what the overlay and an expanded conversation row use, which is
/// why there is no `date_sort` here: the daemon orders the conversation oldest
/// first, and a client that shows it newest first reverses that order rather
/// than inventing one of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
    /// Whether the member is the user's own message (PERSO-179), which
    /// is what a client draws the user's own messages apart by.
    ///
    /// Decided daemon-side on the address, or on the Sent mailbox: a copy of a
    /// message the user sent to themselves sits in the Inbox and is still
    /// theirs, a Sent copy is theirs whatever address it went out from, and
    /// the daemon is the one side that holds the configuration naming the
    /// addresses. A draft member is always the account's own.
    /// `false` from a daemon older than the field.
    #[serde(default)]
    pub own: bool,
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
    /// The draft id of a draft member, which `message.thread` answers with
    /// `include_drafts` (#0142); absent for a message, so an answer without
    /// drafts is the one it was before the field existed.
    ///
    /// A draft member carries `id: 0`, `mailbox: "drafts"`, an empty
    /// `message_id`, the account's address as `from` and the frontmatter
    /// `date` as `date_display`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,
    /// The `mp://` selector of this member, the one a listing row carries
    /// for the same copy, a UID-qualified one included (PERSO-244):
    /// `mp://<account>/drafts/<id>` for a draft member (#0142).
    ///
    /// It is what `y` copies on an expanded member; spelled by the daemon so
    /// every client applies the one encoding rule. `""` from a daemon older
    /// than the field.
    #[serde(default)]
    pub selector: String,
    /// The file of a draft member, the path `y` copies on a draft in the
    /// desktop (#0142); absent for a message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// The whole `message.thread` answer: one conversation, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
    /// The selector of `row_id`'s row, the one a listing row carries for it
    /// (PERSO-244), `null` for a server-only hit.
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

/// Whether un-synced mail can be missing from a server search's hits, the
/// `coverage` of [`ServerSearchSettled`] and of `mp search --json` (PERSO-122,
/// PERSO-131).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SearchCoverage {
    /// The server answered every term.
    #[default]
    Server,
    /// The local store answered at least one term, which `local_terms` names,
    /// so a message the account has not synced cannot be among the hits.
    Local,
}

/// One mailbox a server search could not search, which does not fail it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct UnreachableMailbox {
    /// The sidebar label of the mailbox.
    pub mailbox: String,
    /// What the server answered, as one sentence.
    #[serde(default)]
    pub error: String,
}

/// The most results a Microsoft Graph `$search` on messages answers across
/// all its pages: "A `$search` request returns up to 1,000 results" (Microsoft
/// Learn, "Use the $search query parameter"). A `$filter` pages on past it.
/// [`ServerSearchSettled::capped`] says a search ran into it.
pub const GRAPH_SEARCH_CAP: usize = 1000;

/// What a `message.search_server` settles with (P5-U10c, `LST-08`).
///
/// The hits are not here: each streamed as a `message.server_hit` event while
/// the search ran. What settles is the count, the hits dropped because the
/// local pass was already showing them, the mailboxes that refused, the
/// coverage (PERSO-131): on a plain IMAP server `has:attachment` has no search
/// key, so the daemon keeps only the hits the local store marks as carrying an
/// attachment, and an un-synced message cannot be among them; the two fields
/// mean what `mp search --json`'s do (PERSO-122). Last, whether the limit cut
/// the answer and whether Graph's cap did (PERSO-175).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ServerSearchSettled {
    /// The account the search ran against.
    pub account: String,
    /// The query as the caller sent it.
    #[serde(default)]
    pub query: String,
    /// The hits published, one `message.server_hit` each.
    #[serde(default)]
    pub hits: u64,
    /// The hits dropped because `exclude_message_ids` named them.
    #[serde(default)]
    pub deduplicated: u64,
    /// The mailboxes the server refused, in search order.
    #[serde(default)]
    pub unreachable: Vec<UnreachableMailbox>,
    /// `local` when the store answered a term, `server` otherwise. Absent from
    /// a daemon that predates PERSO-131, which reads as `server`.
    #[serde(default)]
    pub coverage: SearchCoverage,
    /// The terms the store answered, in the grammar's own spelling
    /// (`has:attachment`), and `[]` when the server answered every term.
    #[serde(default)]
    pub local_terms: Vec<String>,
    /// More matched than the hits hold: a mailbox matched more than the
    /// limit, or the merge of every mailbox's newest hits cut some
    /// (PERSO-175). Absent from an older daemon, which reads as `false`.
    #[serde(default)]
    pub truncated: bool,
    /// Microsoft Graph's [`GRAPH_SEARCH_CAP`] stopped a mailbox's search short
    /// of the limit, which no larger limit gets past: only a narrower query
    /// reaches the rest.
    #[serde(default)]
    pub capped: bool,
}

impl ServerSearchSettled {
    /// The caveat a client appends to its hit count, `local store only:
    /// has:attachment`, or `None` when the server answered every term: the
    /// words `mp search` prints in its result header.
    pub fn local_store_note(&self) -> Option<String> {
        (self.coverage == SearchCoverage::Local && !self.local_terms.is_empty())
            .then(|| format!("local store only: {}", self.local_terms.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A settle that names a locally answered term round-trips and carries
    /// the CLI's caveat; one from a daemon that predates the two fields reads
    /// as a search the server answered whole (PERSO-131).
    #[test]
    fn a_server_search_settle_carries_its_coverage() {
        let settled = ServerSearchSettled {
            account: "tum".to_string(),
            query: "has:attachment".to_string(),
            hits: 3,
            deduplicated: 1,
            unreachable: vec![UnreachableMailbox {
                mailbox: "Archive".to_string(),
                error: "timeout".to_string(),
            }],
            coverage: SearchCoverage::Local,
            local_terms: vec!["has:attachment".to_string()],
            truncated: true,
            capped: false,
        };
        let encoded = serde_json::to_value(&settled).expect("it serialises");
        assert_eq!(encoded["truncated"], json!(true));
        assert_eq!(encoded["capped"], json!(false));
        assert_eq!(encoded["coverage"], json!("local"));
        assert_eq!(encoded["local_terms"], json!(["has:attachment"]));
        assert_eq!(
            encoded["unreachable"],
            json!([{"mailbox": "Archive", "error": "timeout"}])
        );
        let decoded: ServerSearchSettled = serde_json::from_value(encoded).expect("it decodes");
        assert_eq!(decoded, settled);
        assert_eq!(
            settled.local_store_note().as_deref(),
            Some("local store only: has:attachment")
        );

        let older: ServerSearchSettled = serde_json::from_value(json!({
            "account": "work", "query": "handover", "hits": 2,
            "deduplicated": 0, "unreachable": [],
        }))
        .expect("a settle without the two fields decodes");
        assert_eq!(older.coverage, SearchCoverage::Server);
        assert!(older.local_terms.is_empty());
        assert!(!older.truncated && !older.capped);
        assert_eq!(older.local_store_note(), None);
    }

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

    /// The two committed `message.list_stream` fixtures decode into their typed
    /// shapes, and a chunk's rows are the listing fixture's rows exactly.
    #[test]
    fn the_committed_stream_fixtures_decode_whole() {
        let raw = include_str!("../fixtures/message.list_stream.response.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let started: MessageListStreamStarted =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");
        assert_eq!(started.account, "work");
        assert_eq!(started.mailbox, "inbox");
        assert_eq!(started.total, 2);
        assert!(!started.operation_id.is_empty());

        let raw = include_str!("../fixtures/notification.message_rows.json");
        let notification: crate::Notification =
            serde_json::from_str(raw).expect("the fixture is a notification");
        assert_eq!(notification.method, crate::METHOD_MESSAGE_ROWS);
        let chunk: MessageRowsChunk =
            serde_json::from_value(notification.params).expect("the params decode");
        assert_eq!(chunk.offset, 0);
        assert_eq!(chunk.operation_id, started.operation_id);

        let raw = include_str!("../fixtures/message.list.response.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let listing: MessageListing =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");
        assert_eq!(
            chunk.rows, listing.messages,
            "a streamed row is a message.list row, field for field"
        );
    }

    /// A chunk's frame, encoded the generic way, starts with the prefix and
    /// decodes straight into the chunk; a frame of another method, one spelled
    /// otherwise and a malformed chunk are left to the generic decode.
    #[test]
    fn a_rows_frame_decodes_straight_into_its_chunk() {
        let raw = include_str!("../fixtures/notification.message_rows.json");
        let notification: crate::Notification =
            serde_json::from_str(raw).expect("the fixture is a notification");
        let frame = crate::frame::encode(&notification).expect("encodes");
        let line = std::str::from_utf8(&frame).expect("UTF-8").trim_end();
        assert!(line.starts_with(MESSAGE_ROWS_FRAME_PREFIX), "{line}");
        let typed = MessageRowsChunk::from_frame_line(line).expect("the fast path");
        let generic: MessageRowsChunk =
            serde_json::from_value(notification.params).expect("the params decode");
        assert_eq!(typed, generic);

        let state_event = r#"{"jsonrpc":"2.0","method":"state.event","params":{}}"#;
        assert_eq!(MessageRowsChunk::from_frame_line(state_event), None);
        let spaced = r#"{"jsonrpc": "2.0", "method": "message.rows", "params": {"offset": 0, "operation_id": "x", "rows": []}}"#;
        assert_eq!(MessageRowsChunk::from_frame_line(spaced), None);
        let malformed = format!(
            r#"{MESSAGE_ROWS_FRAME_PREFIX}{{"operation_id":"x","rows":[{{"id":"seven"}}]}}}}"#
        );
        assert_eq!(MessageRowsChunk::from_frame_line(&malformed), None);
    }

    /// The committed conversation fixtures decode into their typed shapes, a
    /// conversation's `latest` is the listing fixture's row exactly, and a
    /// chunk's frame decodes straight into the chunk by its own prefix.
    #[test]
    fn the_committed_conversation_fixtures_decode_whole() {
        let raw = include_str!("../fixtures/message.list_conversations.response.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let started: MessageListStreamStarted =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");
        assert_eq!(started.total, 2);

        let raw = include_str!("../fixtures/notification.message_conversations.json");
        let notification: crate::Notification =
            serde_json::from_str(raw).expect("the fixture is a notification");
        assert_eq!(notification.method, crate::METHOD_MESSAGE_CONVERSATIONS);
        let chunk: ConversationRowsChunk =
            serde_json::from_value(notification.params.clone()).expect("the params decode");
        assert_eq!(chunk.operation_id, started.operation_id);
        assert_eq!(chunk.rows.len() as u64, started.total);
        assert_eq!(chunk.rows[0].count, 2);
        assert!(chunk.rows[0].has_draft);
        assert_eq!(chunk.rows[0].mailboxes, ["inbox", "sent"]);

        let raw = include_str!("../fixtures/message.list.response.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let listing: MessageListing =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");
        let latest: Vec<MessageListRow> = chunk.rows.iter().map(|r| r.latest.clone()).collect();
        assert_eq!(latest, listing.messages, "latest is a message.list row");

        let frame = crate::frame::encode(&notification).expect("encodes");
        let line = std::str::from_utf8(&frame).expect("UTF-8").trim_end();
        assert!(line.starts_with(CONVERSATION_ROWS_FRAME_PREFIX), "{line}");
        assert_eq!(ConversationRowsChunk::from_frame_line(line), Some(chunk));
        assert_eq!(
            MessageRowsChunk::from_frame_line(line),
            None,
            "the flat fast path leaves a conversation chunk alone"
        );
        let rows = include_str!("../fixtures/notification.message_rows.json");
        let rows: crate::Notification = serde_json::from_str(rows).expect("a notification");
        let frame = crate::frame::encode(&rows).expect("encodes");
        let line = std::str::from_utf8(&frame).expect("UTF-8").trim_end();
        assert_eq!(ConversationRowsChunk::from_frame_line(line), None);
    }

    /// A conversation row serialises its keys in sorted order, the bytes a
    /// `Value` of it would encode to.
    #[test]
    fn a_conversation_row_serialises_in_key_order() {
        let raw = include_str!("../fixtures/notification.message_conversations.json");
        let notification: crate::Notification = serde_json::from_str(raw).expect("a notification");
        let chunk: ConversationRowsChunk =
            serde_json::from_value(notification.params).expect("the params decode");
        for row in &chunk.rows {
            let typed = serde_json::to_string(row).expect("encodes");
            let value = serde_json::to_value(row).expect("encodes");
            let top: Vec<&str> = value
                .as_object()
                .expect("an object")
                .keys()
                .map(String::as_str)
                .collect();
            let mut sorted = top.clone();
            sorted.sort_unstable();
            assert_eq!(top, sorted);
            let positions: Vec<usize> = top
                .iter()
                .map(|key| typed.find(&format!("\"{key}\":")).expect("the key"))
                .collect();
            assert!(
                positions.windows(2).all(|w| w[0] < w[1]),
                "the struct writes its keys in sorted order: {typed}"
            );
        }
    }

    /// `message.thread` with `include_drafts` carries a draft member with its
    /// id; a message member and an answer without drafts carry no `draft_id`
    /// key at all, so the answer without the new parameters is unchanged.
    #[test]
    fn a_draft_member_carries_its_id_and_a_message_none() {
        let raw = include_str!("../fixtures/message.thread.drafts.response.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let thread: ThreadListing =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");
        let draft = thread.messages.last().expect("a draft member");
        assert_eq!(draft.draft_id.as_deref(), Some("c3f1a2b4d5e6f708"));
        assert_eq!((draft.id, draft.mailbox.as_str()), (0, "drafts"));
        assert_eq!(draft.message_id, "");
        assert!(draft.own, "a draft member is the account's own");
        let own: Vec<bool> = thread.messages.iter().map(|m| m.own).collect();
        assert_eq!(own, [false, true, true], "the reply from alice is hers");
        assert!(thread.messages[..2].iter().all(|m| m.draft_id.is_none()));
        assert!(response["result"]["messages"][0].get("draft_id").is_none());

        let raw = include_str!("../fixtures/message.thread.response.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let thread: ThreadListing =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");
        assert_eq!(
            serde_json::to_value(&thread).expect("encodes"),
            response["result"],
            "an answer without drafts re-encodes to the bytes it had"
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
        assert!(
            !thread.messages[0].own,
            "a daemon older than `own` claims no member as the user's"
        );
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
                own: true,
                date_display: "Thu, 2 Jul 2026 13:57:30 +0200".to_string(),
                flags: MessageFlags {
                    seen: true,
                    answered: false,
                    forwarded: false,
                    flagged: true,
                },
                current: false,
                draft_id: None,
                selector: "mp://work/Team%2FReports/root@example.com".to_string(),
                path: None,
            }],
        };
        let encoded = serde_json::to_value(&thread).expect("it serialises");
        assert_eq!(encoded["messages"][0]["mailbox"], json!("Team/Reports"));
        assert_eq!(encoded["messages"][0]["flags"]["flagged"], json!(true));
        assert_eq!(encoded["messages"][0]["own"], json!(true));
        let decoded: ThreadListing = serde_json::from_value(encoded).expect("it decodes");
        assert_eq!(decoded, thread);
    }
}
