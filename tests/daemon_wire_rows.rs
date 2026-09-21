//! `RD-07` and the wire rows: the listing row a client builds a list entry
//! from without ever opening a store (#0126, plan unit P5-U10c).
//!
//! This is a **contract test**, written before the daemon serves `selector`
//! and before the snapshot's draft rows carry `cc`, `date` and `error`. It
//! compiles at HEAD and fails at HEAD on the missing fields, which is the proof
//! there is no stub behind it. The implementer does not edit this file.
//!
//! # `RD-07`: the selector goes on the row, not behind a method
//!
//! `src/tui/actions.rs`'s `selected_selector` opens the account's store on
//! every `y` to turn the cursor's row id into an `mp://` string, because no
//! listing carries one. Three shapes could close that:
//!
//! 1. **`message.get`**, which already answers with a `selector`. It is a
//!    whole-message read, body included, to answer a clipboard copy.
//! 2. **A `message.selector` query**, one round trip per keypress over a
//!    string the daemon already had in hand when it built the row.
//! 3. **A `selector` on the listing row**, which is what
//!    `docs/parity-matrix.md` prefers and what this file pins.
//!
//! The cost of 3 is one key per row. P6-U10 measured a warm `message.list` of
//! 5 000 rows at 94 ms with fifteen keys per row; a sixteenth, about forty
//! bytes of ASCII, is within the noise of that measurement and is paid once per
//! listing rather than once per keypress. It is recorded here so the day a
//! listing measurement moves, the reason it has one more key is written down.
//!
//! **The selector is rendered daemon-side.** A client *could* compose it from
//! `message_id`, the result's `mailbox` and its own account name, because
//! `mp_core::selector` is a shared module. It may not: that would be a second
//! implementation of the percent-encoding and of `message_key`'s
//! normalisation, and `tests/cli_selector_contract.rs` pins only the CLI's.
//! The assertion below is that the wire string and
//! `Selector::for_message(account, row)` over the very row the daemon read are
//! the same bytes, so one message has one name in this tree.
//!
//! # The wire rows
//!
//! `src/tui/app/types.rs`'s `entry_from_row` takes a
//! `store::read::MessageRow`, and `src/tui/queries.rs` imports the same type
//! plus `store::drafts::{DraftRow, SkippedDraft}`. Those three imports are the
//! `app/types.rs store` and `queries.rs store` rows of
//! `tests/fixtures/tui-engine-imports.txt`, and they go when the listing row a
//! client holds is [`mp_protocol::listing::MessageListRow`] and the draft row
//! is [`mp_protocol::state::DraftRow`].
//!
//! The proof a test can state without calling a `pub(crate)` function is
//! **field coverage**: every field `entry_from_row` reads off a store row is on
//! the wire row, carrying the store's own value for a seeded message. The
//! golden-frame suite is the broader proof, because it renders the whole list
//! through the client; this file is the narrow one that says the data is there
//! to render from.
//!
//! **A parse-skipped draft is a `DraftRow` with an `error`, not a section of
//! its own.** `store::drafts::SkippedDraft` is `{path, error}` and the TUI
//! renders it as an unopenable row in the same list, counted by the same
//! sidebar badge (#0080). The snapshot already carries such a file as a row
//! with `valid: false` and `status: "invalid"`; what it was missing is the
//! sentence saying why, which is what `error` is. One row type for one list.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::listing::{MessageListRow, MessageListing};
use mp_protocol::draft::DraftListing;

use mailypoppins::selector::Selector;
use mailypoppins::store::read;
use mailypoppins::tui::app::resolve_date;

use support::parity::{socket_path, DaemonFixture};
use support::draft_fixture;
use support::read_fixture as fixture;

const DEADLINE: Duration = Duration::from_secs(20);

/// Every field of a listing row, which is what the fixture and
/// `docs/daemon-protocol.md` document and what a client may read.
const ROW_FIELDS: [&str; 15] = [
    "bcc",
    "cc",
    "date_display",
    "date_sort",
    "flags",
    "from",
    "has_attachments",
    "id",
    "is_invite",
    "message_id",
    "reply_to",
    "selector",
    "subject",
    "to",
    "uid",
];

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

struct Slice {
    /// Held for its `Drop`, which stops the daemon before the directory it was
    /// reading goes away.
    #[allow(dead_code)]
    daemon: DaemonFixture,
    tmp: TempDir,
}

impl Slice {
    /// A read fixture: four accounts, seven messages, no drafts.
    fn read() -> Slice {
        let tmp = TempDir::new().expect("a temporary wire-row root");
        fixture::seed(tmp.path());
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    /// The read fixture plus a seeded drafts directory, plus one draft that
    /// names a Cc.
    ///
    /// The shared fixture's drafts all carry an empty `cc:`, because no draft
    /// assertion needed one before this file: `cc` is one of the two fields
    /// the wire row was missing, and a fixture that never sets it could not
    /// tell "the daemon dropped it" from "the draft had none". The extra file
    /// is written here rather than into `support::draft_fixture`, so no other
    /// suite's draft counts move.
    fn drafts() -> Slice {
        let tmp = TempDir::new().expect("a temporary wire-row root");
        draft_fixture::seed(tmp.path());
        let path = draft_fixture::drafts_dir(tmp.path(), draft_fixture::ACCOUNT)
            .join("mit-kopie.md");
        std::fs::write(
            &path,
            "---\n\
             id: 0123456789abcdef\n\
             to: robin@example.com\n\
             cc: team@example.com\n\
             bcc:\n\
             subject: \"Mit Kopie\"\n\
             status: draft\n\
             from: alpha@example.com\n\
             date: 2026-07-03 11:30\n\
             ---\n\
             \n\
             Body.\n",
        )
        .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    async fn connect(&self) -> Connection {
        let mut conn = within(
            "Connection::connect",
            Connection::connect(&socket_path(self.root())),
        )
        .await
        .expect("connecting to a live daemon socket succeeds");
        within(
            "Connection::initialize",
            conn.initialize(
                ClientInfo {
                    kind: ClientKind::Cli,
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                Identity {
                    data_dir: self.root().to_path_buf(),
                    config_dir: self.root().to_path_buf(),
                },
                &[],
                &[],
            ),
        )
        .await
        .expect("a compatible handshake succeeds");
        conn
    }
}

async fn within<F, T>(what: &str, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(DEADLINE, future).await {
        Ok(value) => value,
        Err(_) => panic!("{what} did not answer within {DEADLINE:?}"),
    }
}

async fn call(conn: &mut Connection, method: &str, params: Value) -> Value {
    within(method, conn.call(method, params))
        .await
        .unwrap_or_else(|e| panic!("{method} failed: {e:?}"))
}

// ---------------------------------------------------------------------------
// 1. RD-07: the selector on the row
// ---------------------------------------------------------------------------

/// Every listed row carries the canonical `mp://` of its message, and it is the
/// string `Selector::for_message` renders over the row the daemon read.
///
/// **Fails at HEAD**: the served row has fourteen keys and `selector` is not
/// one of them, so the decode leaves it `""` and the comparison reports the
/// empty string against `mp://alpha/inbox/bericht@example.com`.
#[tokio::test]
async fn every_listed_row_carries_the_selector_the_cli_would_print() {
    let slice = Slice::read();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "mailbox": "inbox"}),
    )
    .await;

    for (index, row) in result["messages"]
        .as_array()
        .expect("the listing carries rows")
        .iter()
        .enumerate()
    {
        let mut keys: Vec<&str> = row
            .as_object()
            .expect("a row is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        let mut want = ROW_FIELDS.to_vec();
        want.sort_unstable();
        assert_eq!(keys, want, "messages[{index}] carries the documented fields");
    }

    let listing: MessageListing =
        serde_json::from_value(result.clone()).expect("the answer decodes into the typed listing");
    let store = fixture::store(slice.root(), fixture::ACCOUNT);
    let rows = read::list_mailbox(&store, fixture::ACCOUNT, "inbox").expect("the store rows");
    assert_eq!(
        listing.messages.len(),
        rows.len(),
        "the listing and the store agree about how many rows there are"
    );

    for (wire, row) in listing.messages.iter().zip(rows.iter()) {
        assert_eq!(
            wire.selector,
            Selector::for_message(fixture::ACCOUNT, row).to_string(),
            "the row's selector is the daemon's own spelling of `mp://`, not a client's"
        );
        assert!(
            wire.selector.starts_with("mp://"),
            "a selector is an absolute one: a client pastes it into `mp show`"
        );
    }
}

/// A search hit carries one too, and it is the same string the listing gave
/// for the same message.
///
/// The overlay's `y` copies the selector of the hit under the cursor, and a
/// hit already names its own mailbox, so this is the same fact stated where
/// the other half of `RD-07`'s call sites reads it.
///
/// **Fails at HEAD**: the hit has no `selector` key.
#[tokio::test]
async fn a_search_hit_carries_the_same_selector_the_listing_gave() {
    let slice = Slice::read();
    let mut conn = slice.connect().await;

    let listed = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "mailbox": "inbox"}),
    )
    .await;
    let listing: MessageListing = serde_json::from_value(listed).expect("the listing decodes");
    let by_message_id = |id: &str| {
        listing
            .messages
            .iter()
            .find(|row| row.message_id.contains(id))
            .map(|row| row.selector.clone())
    };

    let hits = call(
        &mut conn,
        "message.search",
        json!({"account": fixture::ACCOUNT, "query": "ledger"}),
    )
    .await;
    let hits = hits["hits"].as_array().expect("search answers with hits");
    assert!(!hits.is_empty(), "the fixture's `ledger` query has hits");

    for hit in hits {
        let selector = hit["selector"]
            .as_str()
            .unwrap_or_else(|| panic!("a hit carries its selector, got {hit}"));
        let message_id = hit["message_id"].as_str().unwrap_or_default();
        if let Some(listed) = by_message_id(message_id) {
            assert_eq!(
                selector, listed,
                "one message has one name, whichever method answered"
            );
        }
        assert!(selector.starts_with("mp://"), "got {selector:?}");
    }
}

// ---------------------------------------------------------------------------
// 2. The wire row carries what a list entry is built from
// ---------------------------------------------------------------------------

/// Every value `entry_from_row` reads off a store row is on the wire row,
/// carrying the store's own value.
///
/// `src/tui/app/types.rs`'s `entry_from_row(row, status)` reads exactly
/// `row.id`, `row.from`, `row.to`, `row.cc`, `row.reply_to`, `row.bcc`,
/// `row.subject`, `row.date_display` (through `resolve_date`), `row.flags()`,
/// `row.has_attachments` and `row.is_invite`. `status` is derived from the
/// mailbox the row was listed from, which the *answer* carries, and the three
/// remaining columns of a store row - `mailbox`, `body_blob`, `thread_id` -
/// it never touches. So this list is the whole of what the wire row owes it,
/// and a row that carries all of it is a row the client can build an entry
/// from without `crate::store`.
///
/// **Fails at HEAD**: on `selector`, which the served row does not carry. The
/// other fourteen already agree, and asserting them here is what makes the
/// signature change safe rather than plausible.
#[tokio::test]
async fn the_wire_row_carries_every_field_a_list_entry_is_built_from() {
    let slice = Slice::read();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "mailbox": "inbox"}),
    )
    .await;
    let listing: MessageListing = serde_json::from_value(result).expect("the listing decodes");
    let store = fixture::store(slice.root(), fixture::ACCOUNT);
    let rows = read::list_mailbox(&store, fixture::ACCOUNT, "inbox").expect("the store rows");

    for (wire, row) in listing.messages.iter().zip(rows.iter()) {
        let flags = row.flags();
        let (_display, date_sort) = resolve_date(&row.date_display, &None, Path::new(""));
        let expected = MessageListRow {
            id: row.id,
            uid: row.uid,
            message_id: row.message_id.clone(),
            from: row.from.clone().unwrap_or_default(),
            to: row.to.clone().unwrap_or_default(),
            cc: row.cc.clone(),
            reply_to: row.reply_to.clone(),
            bcc: row.bcc.clone(),
            subject: row.subject.clone().unwrap_or_default(),
            date_sort,
            date_display: row.date_display.clone().unwrap_or_default(),
            flags: mp_protocol::listing::MessageFlags {
                seen: flags.seen,
                answered: flags.answered,
                forwarded: flags.forwarded,
                flagged: flags.flagged,
            },
            has_attachments: row.has_attachments,
            is_invite: row.is_invite,
            selector: Selector::for_message(fixture::ACCOUNT, row).to_string(),
        };
        assert_eq!(
            wire, &expected,
            "the wire row is the store row `entry_from_row` would have read, field for field"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. The draft rows
// ---------------------------------------------------------------------------

/// `draft.list` already carries every field the TUI's Drafts list renders, so
/// the draft half of the wire rows is a routing change and not a protocol one.
///
/// `src/tui/app/types.rs`'s `indexed_drafts` reads
/// `store::drafts::{DraftRow, SkippedDraft}`; `entry_from_draft` then reads
/// `id`, `to`, `cc`, `subject`, `status` and `date` off the row with `path` as
/// `resolve_date`'s fallback, and `entry_from_skip` reads `path` and `error`
/// off the skip. `mp_protocol::draft::DraftEntry` has all six plus the
/// selector, and `DraftSkip` is `{path, error}`: P5-U4 added `cc` and `date`
/// for exactly this client.
///
/// **Passes at HEAD**, deliberately. It is the row that says the implementer
/// has nothing to add to the protocol for the Drafts list, only a call site to
/// move, and it fails the day somebody narrows that shape.
#[tokio::test]
async fn the_draft_listing_carries_every_field_the_drafts_list_renders() {
    let slice = Slice::drafts();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "draft.list",
        json!({"account": draft_fixture::ACCOUNT}),
    )
    .await;
    let listing: DraftListing =
        serde_json::from_value(result).expect("draft.list answers with a DraftListing");

    assert!(
        !listing.drafts.is_empty(),
        "the draft fixture seeds drafts for {}",
        draft_fixture::ACCOUNT
    );
    assert!(
        listing.drafts.iter().any(|row| row.cc.is_some()),
        "the Cc column of a drafts list needs a Cc on the wire: {:?}",
        listing.drafts
    );
    assert!(
        listing.drafts.iter().any(|row| row.date.is_some()),
        "a draft sorts by its own `date:` before it falls back to the stem: {:?}",
        listing.drafts
    );
    for row in &listing.drafts {
        assert!(
            !row.path.is_empty(),
            "`path` is `resolve_date`'s fallback, so every row needs one: {row:?}"
        );
        assert!(
            row.selector.starts_with("mp://"),
            "a draft is named by its selector, which is what `y` copies: {row:?}"
        );
    }
}

/// A parse-skipped draft travels with the sentence the error row shows
/// (#0080), which is what `entry_from_skip` renders.
///
/// **Passes at HEAD**, for the same reason as the row above.
#[tokio::test]
async fn a_parse_skipped_draft_travels_with_its_reason() {
    let slice = Slice::drafts();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "draft.list",
        json!({"account": draft_fixture::ACCOUNT}),
    )
    .await;
    let listing: DraftListing = serde_json::from_value(result).expect("the listing decodes");

    assert_eq!(
        listing.skipped.len(),
        1,
        "the fixture seeds one file that will not parse: {:?}",
        listing.skipped
    );
    let skip = &listing.skipped[0];
    assert!(
        skip.path.ends_with(draft_fixture::UNPARSEABLE_FILE),
        "a skipped file is named by its path, which is all the identity it has: {skip:?}"
    );
    assert!(
        !skip.error.is_empty(),
        "the skip carries the parser's own one-line reason: {skip:?}"
    );
}
