//! The query layer contract (P5-U3, ticket #0124).
//!
//! This file is a **contract test**: it is written before the TUI can read a
//! mailbox through the daemon at all, against the surface P5-U4 has to supply
//! (`.agents/workflow/native-gui-daemon/plan.md` section 3.7, P5-U3/P5-U4), the
//! whole-list-plus-row-deltas shape P1a-U3 chose
//! (`docs/baselines/decisions/list-transfer.md`) and the `message.*` /
//! `draft.*` / `mailbox.*` sections of `docs/daemon-protocol.md`. It does not
//! compile against today's tree, which has no `crate::tui::queries`; that
//! failure *is* the proof the contract has no stub behind it. The implementer
//! does not edit this file, they make it pass.
//!
//! # Contract
//!
//! ```text
//! mailypoppins::tui::queries                                     // the module
//!
//! trait queries::Queries {                                       // object safe
//!     fn call(&self, method: &str, params: serde_json::Value) -> anyhow::Result<serde_json::Value>;
//! }
//! impl queries::Queries for crate::tui::session::Session         // Session::call, verbatim
//!
//! queries::list_emails(&dyn Queries, account: &str, mailbox: &str)
//!     -> anyhow::Result<Vec<EmailEntry>>
//! queries::mailbox_counts(&dyn Queries, account: &str, mailboxes: &[MailboxInfo])
//!     -> anyhow::Result<Vec<usize>>
//! queries::message_body(&dyn Queries, account: &str, msg: MessageRef)
//!     -> anyhow::Result<Option<String>>
//!
//! queries::MessageRowDelta: Debug                                // variants are P5-U4's
//! queries::MessageRowDelta::decode(&mp_protocol::EventEnvelope) -> Option<MessageRowDelta>
//! queries::apply_row_delta(&mut Vec<EmailEntry>, mailbox: &str, &MessageRowDelta) -> bool
//! ```
//!
//! Eight names, and nothing else. Every other type the file uses is the TUI's
//! own or the daemon's own and exists today.
//!
//! # Why a trait and free functions rather than three methods on `Session`
//!
//! [`Session::call`](crate::tui::session::Session::call) already has exactly
//! the signature `Queries::call` declares, so `impl Queries for Session` is one
//! line and no second connect path is invented. What the trait buys is that the
//! query layer becomes testable without a socket: this file drives it over an
//! in-process [`Dispatcher`](crate::daemon::dispatch::Dispatcher), the same
//! fixture pattern P5-U1 established in `src/tui/ui/golden_frames_daemon.rs`,
//! so the contract is pinned against the daemon's real method bodies and not
//! against a hand-written JSON mock that could agree with nobody.
//!
//! The three functions are typed in the TUI's own vocabulary
//! ([`EmailEntry`](crate::tui::app::EmailEntry),
//! [`MailboxInfo`](crate::tui::app::MailboxInfo),
//! [`MessageRef`](crate::tui::app::MessageRef)) rather than in a wire row,
//! because that is what the six call sites need and because it is what makes
//! the oracle below the strongest one available: the daemon-backed answer is
//! compared field by field against the answer the store-backed path produces
//! today, on the same fixture, in the same process.
//!
//! # The oracle
//!
//! Equality against the current `open_store` path. `load_emails`,
//! `count_all_emails` and `App::load_message_body` are the incumbents; each
//! test runs both halves over one seeded store and compares. `EmailEntry` is
//! not `PartialEq` (adding the derive would be a production edit this unit may
//! not make), so rows are compared by their `Debug` rendering, which covers
//! every field the frame paints and fails loudly naming the row that moved.
//!
//! # What P5-U4 has to decide, which this file deliberately does not
//!
//! Three gaps between what `message.list` / `message.get` carry today and what
//! an `EmailEntry` holds. They are named here so the implementer meets them at
//! the start rather than at the end, and the tests below fail until each is
//! closed, whichever way it is closed:
//!
//! - **The row id.** `entry_from_row` puts `MessageRef(messages.id)` on every
//!   entry, and it is the identity the selection set, the preview memo and
//!   every mutation hold (#0050). `message.list`'s row carries `uid` and
//!   `message_id` and no `messages.id`, so the wire row owes one field or the
//!   client owes a resolution.
//! - **Four columns the wire row does not carry**: `to` (which the Sent and
//!   Drafts lists display instead of `from`), `cc`/`reply_to`/`bcc` (the
//!   headers pane, #0096), `flagged` (the star, #0007, deliberately left off
//!   the wire by P2-U10) and `is_invite` (the badge, #0038). The named-field
//!   encoding the decision keeps makes adding them additive.
//! - **Addressing `message.get`.** It takes `id` as `"<mailbox>/<uid>"` or a
//!   selector; the preview path holds a `MessageRef`. Either address is fine,
//!   the equality is what is pinned.
//!
//! And one on the drafts branch: `mp_protocol::draft::DraftEntry` carries no
//! `date` and no `cc`, which `entry_from_draft` both read, so `draft.list` owes
//! two fields or the client owes a fallback to the filename stem (which is what
//! `resolve_date` already does for a draft with no `date:`).
//!
//! Every one of these was closed one plausible way in a throwaway worktree
//! before this file was committed, and 17 of its 18 rows passed there; the
//! eighteenth is [`the_query_layer_replaced_every_open_store_it_could`], which
//! only a change to the six call sites can satisfy. So P5-U4 is not being
//! handed an assertion no implementation can meet. That implementation is not
//! this commit's and is not proposed as P5-U4's.
//!
//! # What is out of scope, and stays `open_store`
//!
//! Three of the six call sites have no daemon method to move to and P5-U4 may
//! keep them: the calendar agenda, the invite card and the raw ics bytes.
//! [`TUI_APP_STORE_RESIDUE`] is that list with a reason per entry, and
//! [`the_query_layer_replaced_every_open_store_it_could`] is the gate over it.
//!
//! # Determinism
//!
//! Every test owns a per-thread data root
//! ([`crate::config::test_env::TestDataDir`], #0077) held alive for as long as
//! the fixture, so the store, the drafts directory and the daemon's runtime
//! directory all resolve under it and no test can reach the developer's tree.
//! Rows are ingested through the real [`crate::ingest::ingest_message`], so the
//! rows under test are the rows the sync path produces. Dates are frozen
//! literals. Nothing renders, so no theme is pinned and no snapshot is minted.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use mp_protocol::{EventEnvelope, Request, RequestId, JSONRPC_VERSION};

use super::{build_mailboxes, count_all_emails, load_emails, App, EmailEntry, MessageRef};
use crate::config::{AccountConfig, GlobalConfig};
use crate::daemon::config::{ConfigState, ConfigStore};
use crate::daemon::dispatch::{ClientCtx, ClientKind};
use crate::daemon::runtime::InstanceMeta;
use crate::daemon::server::DaemonState;
use crate::parse::FetchedEmail;
use crate::tui::queries::{
    apply_row_delta, list_emails, mailbox_counts, message_body, MessageRowDelta, Queries,
};

/// The one account every fixture configures, and the one every query names.
const ACCOUNT: &str = "alice";

/// The p95 ceiling the daemon-era preview query is held to, in milliseconds
/// over the pre-daemon p95 of the same walk.
///
/// `docs/baselines/pre-daemon/workloads.md` W1: "the daemon-era p95 of
/// `tui_preview_query` may exceed the pre-daemon p95 by at most 5 ms, the same
/// budget the transport decision (P1a-U2) is held to". W1 itself is
/// `NOT TAKEN` in `docs/baselines/pre-daemon/measurements.md` (no account, no
/// terminal on that host), so the number is the budget rather than a recorded
/// figure, which is exactly why the timing row below measures the *delta* on
/// one host in one process instead of asserting an absolute.
const PREVIEW_P95_DELTA_CEILING_MS: f64 = 5.0;

// ---------------------------------------------------------------------------
// The fixture: one seeded store, one in-process daemon over it
// ---------------------------------------------------------------------------

/// A daemon assembled over a fixture data root, reachable as a [`Queries`].
///
/// The data-root override is held here rather than dropped at the end of the
/// builder because every path in sight (the store, the blob store, the drafts
/// directory, the daemon's runtime directory) resolves under it, and a dropped
/// tempdir would point the next resolution at the developer's own tree.
struct Fixture {
    daemon: DaemonState,
    runtime: tokio::runtime::Runtime,
    _data: crate::config::test_env::TestDataDir,
}

impl Fixture {
    /// A daemon over a fresh data root with an empty store for [`ACCOUNT`].
    ///
    /// The store is created before the daemon is assembled because
    /// `account::state_of` probes the store *file*: an account with no file is
    /// `blocked`, and `message.list` refuses a blocked account. Seeding rows
    /// afterwards is fine, the probe runs per call.
    fn new() -> Fixture {
        let data = crate::config::test_env::TestDataDir::new();
        let root = crate::config::mailypoppins_data_dir();
        // Creates the file, which is what makes the account `ready`.
        drop(crate::store::Store::open(crate::config::store_path(ACCOUNT)).expect("a store"));

        let config = GlobalConfig {
            accounts: vec![AccountConfig {
                name: ACCOUNT.to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let store = Arc::new(ConfigStore::new(
            root.join("config.toml"),
            ConfigState::Ok,
            config,
            // No account runtimes: every method under test reads the store for
            // itself, so nothing here depends on a runtime having come up.
            false,
        ));
        let daemon = DaemonState::new(
            InstanceMeta {
                app_version: "0.0.0-p5u3".to_string(),
                protocol_min: mp_protocol::PROTOCOL_MIN,
                protocol_max: mp_protocol::PROTOCOL_MAX,
                instance_id: "query-layer".to_string(),
                pid: 42,
                started_at: "2026-07-28T09:00:00Z".to_string(),
                data_dir: root.clone(),
                config_dir: root,
            },
            store,
        );
        Fixture {
            daemon,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime"),
            _data: data,
        }
    }

    /// The TUI's own connection context: a `tui` client at protocol 1.
    fn ctx() -> ClientCtx {
        ClientCtx {
            connection_id: 1,
            kind: ClientKind::Tui,
            protocol: 1,
            capabilities: vec![
                "state.bootstrap".to_string(),
                "message.list".to_string(),
                "message.get".to_string(),
                "draft.list".to_string(),
                "mailbox.list".to_string(),
            ],
        }
    }

    /// The daemon's runtime directory for materialised handles, whether or not
    /// anything has ever minted one.
    fn handles_dir(&self) -> PathBuf {
        crate::daemon::runtime::runtime_dir().join(crate::daemon::handles::HANDLES_DIR)
    }

    /// How many handle directories the daemon is currently holding open.
    fn live_handles(&self) -> usize {
        match std::fs::read_dir(self.handles_dir()) {
            Ok(entries) => entries.filter_map(Result::ok).count(),
            Err(_) => 0,
        }
    }
}

impl Queries for Fixture {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let request = Request {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(RequestId::Num(1)),
            method: method.to_string(),
            params,
        };
        let outcome = self
            .runtime
            .block_on(self.daemon.dispatcher.dispatch(&Fixture::ctx(), request))
            .map_err(|e| anyhow::anyhow!("{method}: {e}"))?;
        Ok(outcome.result)
    }
}

/// One fixture message, everything about it derived from `subject` so a failure
/// names the row it is about.
fn fixture_email(subject: &str, date: &str, read: bool) -> FetchedEmail {
    FetchedEmail {
        from: format!("Sender {subject} <s@example.com>"),
        to: "me@example.com".into(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: subject.into(),
        date: date.into(),
        body_text: format!("body of {subject}"),
        html_body: None,
        has_attachments: false,
        message_id: Some(format!("<{subject}@example.com>")),
        attachments: Vec::new(),
        flags: crate::types::MessageFlags::seen(read),
        calendar_ics: None,
        event: None,
    }
}

/// Write a fixture message through the real ingest API, so the rows under test
/// are the rows the sync path actually produces.
fn ingest_fixture(mailbox: &str, uid: i64, email: &FetchedEmail) {
    let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).unwrap();
    let blobs = crate::store::BlobStore::for_account(ACCOUNT);
    crate::ingest::ingest_message(
        &store,
        &blobs,
        &crate::ingest::IngestInput {
            account: ACCOUNT,
            mailbox,
            uid,
            email,
            raw: None,
        },
    )
    .unwrap();
}

/// Three inbox messages and one archived one, the shape most rows below want.
fn seed_inbox() {
    ingest_fixture(
        "inbox",
        1,
        &fixture_email("a", "Mon, 01 Jan 2024 09:00:00 +0000", false),
    );
    ingest_fixture(
        "inbox",
        2,
        &fixture_email("b", "Mon, 01 Jan 2024 10:00:00 +0000", true),
    );
    ingest_fixture(
        "inbox",
        3,
        &fixture_email("c", "Mon, 01 Jan 2024 11:00:00 +0000", true),
    );
    ingest_fixture(
        "archive",
        1,
        &fixture_email("old", "Mon, 01 Jan 2023 09:00:00 +0000", true),
    );
}

/// Write a draft `.md` into the account's drafts directory the way an agent or
/// `$EDITOR` does: no index entry, nothing told to the application.
fn write_draft(stem: &str, body: &str) {
    let dir = crate::config::drafts_dir(ACCOUNT);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{stem}.md")), body).unwrap();
}

/// Compare two entry lists field by field, in order, naming the first row that
/// differs rather than printing two whole lists.
///
/// `EmailEntry` is `Debug` and not `PartialEq`, and deriving `PartialEq` on it
/// would be a production edit this unit may not make, so the `Debug` rendering
/// is the comparison: it covers every field of the struct, which is every field
/// the list and the headers pane paint.
fn same_entries(daemon: &[EmailEntry], store: &[EmailEntry], label: &str) {
    for (row, (left, right)) in daemon.iter().zip(store.iter()).enumerate() {
        assert_eq!(
            format!("{left:?}"),
            format!("{right:?}"),
            "{label}: row {row} differs between the daemon-backed load and the store-backed one"
        );
    }
    assert_eq!(
        daemon.len(),
        store.len(),
        "{label}: the daemon-backed load has {} rows against the store-backed {}",
        daemon.len(),
        store.len(),
    );
}

/// An `App` whose active account is [`ACCOUNT`], for the incumbent half of the
/// preview oracle.
///
/// `App::load_message_body` resolves its store from `account_config.name`, so
/// that one field is the whole of what it needs.
fn app_on_fixture() -> App {
    let mut app = App::default_for_tests();
    app.account_config = AccountConfig {
        name: ACCOUNT.to_string(),
        ..Default::default()
    };
    app
}

// ---------------------------------------------------------------------------
// (a) the mailbox listing
// ---------------------------------------------------------------------------

/// The load the frame paints. One mailbox of one account through the daemon,
/// against the same mailbox through `open_store`, row for row and field for
/// field.
///
/// This is the whole-list half of `docs/baselines/decisions/list-transfer.md`:
/// one `message.list` per mailbox open, `limit: null`, newest first, and no
/// paging parameter anywhere.
#[test]
fn a_daemon_backed_mailbox_load_matches_the_store_backed_one() {
    let fixture = Fixture::new();
    seed_inbox();

    let daemon = list_emails(&fixture, ACCOUNT, "inbox").expect("the daemon lists inbox");
    same_entries(&daemon, &load_emails(ACCOUNT, "inbox"), "inbox");
    assert_eq!(daemon.len(), 3, "three inbox rows were ingested");

    let archive = list_emails(&fixture, ACCOUNT, "archive").expect("the daemon lists archive");
    same_entries(&archive, &load_emails(ACCOUNT, "archive"), "archive");
    assert_eq!(archive.len(), 1);
}

/// Newest first, which is the order the list renders and the order every
/// cursor key assumes. Pinned separately from the equality above so a query
/// layer that happened to match a *wrongly* ordered incumbent still fails.
#[test]
fn a_daemon_backed_mailbox_load_is_newest_first() {
    let fixture = Fixture::new();
    seed_inbox();

    let subjects: Vec<String> = list_emails(&fixture, ACCOUNT, "inbox")
        .expect("the daemon lists inbox")
        .iter()
        .map(|entry| entry.subject.clone())
        .collect();
    assert_eq!(subjects, vec!["c", "b", "a"]);
}

/// A mailbox with no rows, and an account with an empty store, are both an
/// empty list rather than an error: that is what a never-synced mailbox looks
/// like today and the sidebar keeps its slot for it.
#[test]
fn an_empty_mailbox_lists_empty_through_the_daemon_too() {
    let fixture = Fixture::new();

    let daemon = list_emails(&fixture, ACCOUNT, "inbox").expect("an empty inbox is not an error");
    same_entries(&daemon, &load_emails(ACCOUNT, "inbox"), "empty inbox");
    assert!(daemon.is_empty());
}

/// The sidebar column. `mailbox.list` answers per-mailbox totals index-aligned
/// with the mailboxes the sidebar holds, including the zero of a
/// configured-but-never-synced one, and including the drafts count that comes
/// from the index rather than from `messages`.
#[test]
fn daemon_backed_mailbox_counts_match_the_store_backed_ones() {
    let fixture = Fixture::new();
    seed_inbox();
    write_draft(
        "2024-01-02-note",
        "---\nid: d1\nto: x@example.com\nsubject: Note\nstatus: draft\n---\n\nbody\n",
    );

    let mailboxes = build_mailboxes(&AccountConfig {
        name: ACCOUNT.to_string(),
        ..Default::default()
    });
    let daemon = mailbox_counts(&fixture, ACCOUNT, &mailboxes).expect("the daemon counts");
    assert_eq!(daemon, count_all_emails(ACCOUNT, &mailboxes));
    assert_eq!(
        daemon.len(),
        mailboxes.len(),
        "index-aligned with the sidebar"
    );
}

// ---------------------------------------------------------------------------
// (b) the preview
// ---------------------------------------------------------------------------

/// The per-cursor-move read. The body of the selected row through the daemon,
/// against the body the preview memo fills from `open_store` today.
///
/// Every row of the mailbox, not one: the preview is walked, and a query layer
/// that resolved the first row and mis-addressed the rest would pass a
/// single-row assertion.
#[test]
fn a_daemon_backed_preview_body_matches_the_store_backed_one() {
    let fixture = Fixture::new();
    seed_inbox();
    let app = app_on_fixture();

    for entry in load_emails(ACCOUNT, "inbox") {
        let msg = entry.msg.expect("an ingested row has a MessageRef");
        assert_eq!(
            message_body(&fixture, ACCOUNT, msg).expect("the daemon reads a body"),
            app.load_message_body(msg),
            "the body of {} ({}) differs",
            msg,
            entry.subject,
        );
    }
}

/// A row that is not in the store previews as an empty body rather than as an
/// error, which is what the incumbent does with a stale `MessageRef`: the pane
/// goes blank and the log says why.
#[test]
fn a_preview_of_a_row_that_is_gone_is_empty_on_both_paths() {
    let fixture = Fixture::new();
    seed_inbox();
    let app = app_on_fixture();
    let gone = MessageRef::new(9_999);

    assert_eq!(
        message_body(&fixture, ACCOUNT, gone).expect("a stale reference is not a transport error"),
        app.load_message_body(gone),
    );
}

// ---------------------------------------------------------------------------
// (c) the drafts list
// ---------------------------------------------------------------------------

/// The Drafts mailbox is the one branch of the list query that is not
/// `message.list`: drafts are local-only `.md` files with no `messages` row, so
/// the incumbent reads the drafts index and the daemon answers `draft.list`.
/// The entries either way are the same rows in the same order.
#[test]
fn a_daemon_backed_drafts_list_matches_the_store_backed_one() {
    let fixture = Fixture::new();
    write_draft(
        "2024-01-02-first",
        "---\nid: d1\nto: a@example.com\nsubject: First\nstatus: draft\n---\n\nfirst\n",
    );
    write_draft(
        "2024-01-03-second",
        "---\nid: d2\nto: b@example.com\nsubject: Second\nstatus: approved\n---\n\nsecond\n",
    );

    let daemon = list_emails(&fixture, ACCOUNT, crate::selector::DRAFTS_MAILBOX)
        .expect("the daemon lists drafts");
    same_entries(
        &daemon,
        &load_emails(ACCOUNT, crate::selector::DRAFTS_MAILBOX),
        "drafts",
    );
    assert_eq!(daemon.len(), 2);
    assert!(
        daemon
            .iter()
            .all(|entry| entry.msg.is_none() && entry.draft_id.is_some()),
        "a draft row is named by its draft id and by no MessageRef",
    );
}

/// A draft the index could not parse still lists, as the error row #0080 put at
/// the top of the list. It is the one entry with neither a `MessageRef` nor a
/// draft id, so a query layer that dropped what it could not decode would lose
/// exactly the row the user is hunting for.
#[test]
fn an_unparseable_draft_still_lists_through_the_daemon() {
    let fixture = Fixture::new();
    write_draft(
        "2024-01-02-good",
        "---\nid: d1\nto: a@example.com\nsubject: Good\nstatus: draft\n---\n\ngood\n",
    );
    write_draft("2024-01-03-broken", "no frontmatter at all\n");

    let daemon = list_emails(&fixture, ACCOUNT, crate::selector::DRAFTS_MAILBOX)
        .expect("the daemon lists drafts");
    same_entries(
        &daemon,
        &load_emails(ACCOUNT, crate::selector::DRAFTS_MAILBOX),
        "drafts with a broken file",
    );
    assert!(
        daemon.iter().any(|entry| entry.skip.is_some()),
        "the unparseable file is listed as an error row",
    );
}

// ---------------------------------------------------------------------------
// (d) row deltas over the held list
// ---------------------------------------------------------------------------

/// The `message.row` replace an updated row travels as, in the shape the
/// decision fixes: `Event::Replace { kind: "message.row", payload: {account,
/// mailbox, message} }`, where `message` is one row of the `message.list`
/// answer.
fn row_replace(mailbox: &str, message: Value) -> EventEnvelope {
    EventEnvelope {
        instance_id: "query-layer".to_string(),
        revision: 7,
        kind: "message.row".to_string(),
        payload: json!({"account": ACCOUNT, "mailbox": mailbox, "message": message}),
    }
}

/// One row of what `message.list` answers for `mailbox`, selected by subject.
fn wire_row(fixture: &Fixture, mailbox: &str, subject: &str) -> Value {
    let answer = fixture
        .call(
            "message.list",
            json!({"account": ACCOUNT, "mailbox": mailbox}),
        )
        .expect("message.list");
    answer["messages"]
        .as_array()
        .expect("messages is an array")
        .iter()
        .find(|row| row["subject"] == json!(subject))
        .unwrap_or_else(|| panic!("{mailbox} has no row subject {subject:?}"))
        .clone()
}

/// A replace of a row the client already holds updates it where it stands: the
/// list keeps its length and its order, and no `message.list` is owed.
///
/// This is the half of the whole-list decision that makes it cheap. A client
/// that refetched on every revision move would pay 13.8 ms per sync event
/// instead of the 2.5 kB a ten-row delta weighs
/// (`docs/baselines/decisions/list-transfer.md`), which is inside W2's ceiling
/// and is not what was chosen.
#[test]
fn a_row_delta_updates_the_held_list_without_a_refetch() {
    let fixture = Fixture::new();
    seed_inbox();

    let mut held = list_emails(&fixture, ACCOUNT, "inbox").expect("the daemon lists inbox");
    let before = held.len();

    // The same row, seen again with `\Seen` set: the flag flip a sync tick
    // produces, which is the commonest delta of all.
    let mut row = wire_row(&fixture, "inbox", "a");
    row["flags"]["seen"] = json!(true);
    let delta = MessageRowDelta::decode(&row_replace("inbox", row))
        .expect("a message.row event decodes to a delta");

    assert!(
        apply_row_delta(&mut held, "inbox", &delta),
        "a row replace is folded into the held list, it does not owe a refetch: {delta:?}",
    );
    assert_eq!(held.len(), before, "a replace keeps the list's length");
    let updated = held
        .iter()
        .find(|entry| entry.subject == "a")
        .expect("the replaced row is still in the list");
    assert!(updated.read, "the replaced row carries the new flag");
    assert_eq!(
        held.iter().map(|e| e.subject.clone()).collect::<Vec<_>>(),
        vec!["c", "b", "a"],
        "a replace keeps the list's order",
    );
}

/// A replace naming a row the client does not hold inserts it where a fresh
/// `message.list` would have put it. The oracle is that list: the delta path
/// and the refetch path have to agree, or a session that stayed open would
/// drift from one that reopened the mailbox.
#[test]
fn a_row_delta_inserts_a_new_row_where_a_refetch_would_have_put_it() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut held = list_emails(&fixture, ACCOUNT, "inbox").expect("the daemon lists inbox");

    // A message that arrives between two list calls, dated between two rows the
    // client holds so the insertion point is neither end.
    ingest_fixture(
        "inbox",
        4,
        &fixture_email("b-and-a-half", "Mon, 01 Jan 2024 10:30:00 +0000", false),
    );
    let delta = MessageRowDelta::decode(&row_replace(
        "inbox",
        wire_row(&fixture, "inbox", "b-and-a-half"),
    ))
    .expect("a message.row event decodes to a delta");

    assert!(apply_row_delta(&mut held, "inbox", &delta), "{delta:?}");
    same_entries(
        &held,
        &list_emails(&fixture, ACCOUNT, "inbox").expect("the refetch"),
        "the delta path against the refetch path",
    );
}

/// A remove drops the row and nothing else: `Event::Remove { resource }`, with
/// the `message:<account>/<mailbox>/<uid>` resource the mutation methods
/// already invalidate.
#[test]
fn a_remove_delta_drops_the_row_from_the_held_list() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut held = list_emails(&fixture, ACCOUNT, "inbox").expect("the daemon lists inbox");
    let uid = wire_row(&fixture, "inbox", "b")["uid"].clone();

    let delta = MessageRowDelta::decode(&EventEnvelope {
        instance_id: "query-layer".to_string(),
        revision: 8,
        kind: "state.remove".to_string(),
        payload: json!({"resource": format!("message:{ACCOUNT}/inbox/{uid}")}),
    })
    .expect("a state.remove event over a message decodes to a delta");

    assert!(apply_row_delta(&mut held, "inbox", &delta), "{delta:?}");
    assert_eq!(
        held.iter().map(|e| e.subject.clone()).collect::<Vec<_>>(),
        vec!["c", "a"],
    );
}

/// The coarse fallback. `Event::Invalidate` over the mailbox listing is the
/// one delta a client cannot apply, so it says so and the caller re-issues
/// `message.list`; a queue overflow degrades into the same thing through
/// `state.resync_required`.
#[test]
fn an_invalidate_owes_a_refetch_and_leaves_the_held_list_alone() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut held = list_emails(&fixture, ACCOUNT, "inbox").expect("the daemon lists inbox");
    let before: Vec<String> = held.iter().map(|e| e.subject.clone()).collect();

    let delta = MessageRowDelta::decode(&EventEnvelope {
        instance_id: "query-layer".to_string(),
        revision: 9,
        kind: "state.invalidate".to_string(),
        payload: json!({
            "resource": format!("mailbox:{ACCOUNT}/inbox"),
            "scope": {"query": "list"},
        }),
    })
    .expect("a state.invalidate over a mailbox listing decodes to a delta");

    assert!(
        !apply_row_delta(&mut held, "inbox", &delta),
        "an invalidate owes a refetch: {delta:?}",
    );
    assert_eq!(
        held.iter().map(|e| e.subject.clone()).collect::<Vec<_>>(),
        before,
        "an invalidate does not half-apply anything",
    );
}

/// A delta about another mailbox is folded as a no-op rather than as a
/// refetch: the sidebar's other three mailboxes move all the time, and a
/// client that refetched the open list on each of them would put back exactly
/// the per-event whole-list transfer the deltas exist to avoid.
#[test]
fn a_delta_about_another_mailbox_costs_the_open_list_nothing() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut held = list_emails(&fixture, ACCOUNT, "inbox").expect("the daemon lists inbox");
    let before: Vec<String> = held.iter().map(|e| e.subject.clone()).collect();

    let delta = MessageRowDelta::decode(&row_replace(
        "archive",
        wire_row(&fixture, "archive", "old"),
    ))
    .expect("a message.row event decodes to a delta");

    assert!(
        apply_row_delta(&mut held, "inbox", &delta),
        "another mailbox's row owes the open list no refetch: {delta:?}",
    );
    assert_eq!(
        held.iter().map(|e| e.subject.clone()).collect::<Vec<_>>(),
        before,
    );
}

/// An event of a kind the query layer does not handle is not a delta. It is
/// P5-U7/U8's business, and decoding it into something applicable would let a
/// sync tick or a draft change silently rewrite a message list.
#[test]
fn an_unrelated_event_is_not_a_row_delta() {
    for kind in ["sync.completed", "draft.changed", "mailbox.counts_changed"] {
        let event = EventEnvelope {
            instance_id: "query-layer".to_string(),
            revision: 10,
            kind: kind.to_string(),
            payload: json!({"account": ACCOUNT}),
        };
        assert!(
            MessageRowDelta::decode(&event).is_none(),
            "{kind} decoded into a message row delta",
        );
    }
}

// ---------------------------------------------------------------------------
// (f) handle and lifetime hygiene
// ---------------------------------------------------------------------------

/// A preview walk mints no materialised handle, so none is owed a release when
/// the cursor moves off.
///
/// `message.get` answers inline: `docs/baselines/decisions/large-payloads.md`
/// puts the handle threshold at 1 MiB and the three handle methods
/// (`message.materialise_html`, `message.materialise_attachment`,
/// `message.release_handle`) are the only minters in the daemon. This test is
/// the record of that, and it is the release contract the day P5-U4 or a later
/// unit routes a preview through a minter instead: the number after the walk is
/// zero either way, whether because nothing was minted or because everything
/// was released.
#[test]
fn a_preview_walk_leaves_no_handle_behind() {
    let fixture = Fixture::new();
    seed_inbox();
    assert_eq!(fixture.live_handles(), 0, "a fresh daemon holds no handle");

    let rows = list_emails(&fixture, ACCOUNT, "inbox").expect("the daemon lists inbox");
    // Down the list and back up, which is the walk that evicts a one-slot memo
    // on every move (`docs/plans/preview-latency.md`).
    for entry in rows.iter().chain(rows.iter().rev()) {
        let msg = entry.msg.expect("an ingested row has a MessageRef");
        message_body(&fixture, ACCOUNT, msg).expect("the daemon reads a body");
    }

    assert_eq!(
        fixture.live_handles(),
        0,
        "the preview walk left handles in {}",
        fixture.handles_dir().display(),
    );
}

// ---------------------------------------------------------------------------
// (e) the gate over the call sites
// ---------------------------------------------------------------------------

/// The `open_store` calls P5-U4 may keep in `src/tui/app/`, and why.
///
/// `(file, function, reason)`, sorted by file then function. Read it as the
/// answer to "why is P5-U3's gate not at literal zero": three of the six sites
/// read data no `message.*` method answers, and contracting a method for them
/// is neither this unit's nor P5-U4's brief.
///
/// The table is a record as much as a gate: a residue that goes away must be
/// struck from it in the same commit, which is why
/// [`the_query_layer_replaced_every_open_store_it_could`] fails in both
/// directions. It follows `tests/architecture_boundaries.rs`'s
/// `CLI_ENGINE_RESIDUE` in shape and in intent; it lives here rather than
/// there because it only becomes true when P5-U4 lands, and the plan requires
/// `cargo test --workspace` to stay green on a T unit's commit. This module is
/// the target that does not compile, so a gate inside it costs the rest of the
/// tree nothing.
const TUI_APP_STORE_RESIDUE: [(&str, &str, &str); 3] = [
    (
        "src/tui/app/mod.rs",
        "load_calendar_events",
        "the agenda: calendar.rebuild is an operation over the store, not a query that answers \
         the invite rows the Calendar view lists",
    ),
    (
        "src/tui/app/mod.rs",
        "load_message_ics",
        "the raw invite.ics bytes the RSVP reply builder signs; no message.* method hands out an \
         attachment blob inline",
    ),
    (
        "src/tui/app/mod.rs",
        "load_message_invite",
        "the invite card: it folds the account's REPLY rows over one payload (reconcile::*), \
         which is a whole-account read no method answers",
    ),
];

/// The two files this unit is accountable for.
const QUERY_LAYER_SOURCES: [&str; 2] = ["src/tui/app/mod.rs", "src/tui/app/types.rs"];

/// P5-U3's gate: the three call sites a daemon method can answer are gone from
/// the TUI's app module, and the three that remain are the three
/// [`TUI_APP_STORE_RESIDUE`] names.
///
/// Fails on the tree as committed (six sites against three), which is the
/// point: it is P5-U4's gate, and P5-U4 passes it by routing the mailbox load,
/// the sidebar counts and the preview body through
/// [`crate::tui::queries`](crate::tui::queries).
#[test]
fn the_query_layer_replaced_every_open_store_it_could() {
    let actual = store_opens_in_app();
    let expected: BTreeSet<(String, String)> = TUI_APP_STORE_RESIDUE
        .iter()
        .map(|(file, function, _)| (file.to_string(), function.to_string()))
        .collect();

    let added: Vec<_> = actual.difference(&expected).collect();
    let removed: Vec<_> = expected.difference(&actual).collect();
    if added.is_empty() && removed.is_empty() {
        return;
    }

    let mut report = String::new();
    for (file, function) in &added {
        report.push_str(&format!("  still opens a store: {file}::{function}\n"));
    }
    for (file, function) in &removed {
        let reason = TUI_APP_STORE_RESIDUE
            .iter()
            .find(|(f, fun, _)| f == file && fun == function)
            .map(|(_, _, reason)| *reason)
            .unwrap_or("");
        report.push_str(&format!(
            "  no longer opens a store: {file}::{function} ({reason})\n"
        ));
    }
    panic!(
        "the TUI app module's store opens moved ({} in the tree, {} in TUI_APP_STORE_RESIDUE):\n\
         {report}\n\
         Phase 5's gate is that the mailbox load, the sidebar counts and the preview body read \
         through crate::tui::queries rather than through crate::store::open_store. A site that \
         is still there belongs behind a query; a site that went away belongs struck from \
         TUI_APP_STORE_RESIDUE in the same commit.",
        actual.len(),
        expected.len(),
    );
}

/// Every residue entry names a function that exists, in a scanned file, with a
/// usable reason, and the table is sorted and deduplicated.
///
/// A row with an empty reason is a row nobody thought about, which is the thing
/// this table exists to prevent.
#[test]
fn every_residue_entry_names_a_real_function_and_a_reason() {
    let mut previous: Option<(&str, &str)> = None;
    for (file, function, reason) in TUI_APP_STORE_RESIDUE {
        assert!(
            QUERY_LAYER_SOURCES.contains(&file),
            "TUI_APP_STORE_RESIDUE names {file}, which this unit does not scan"
        );
        let source = read_source(file);
        assert!(
            source.contains(&format!("fn {function}(")),
            "TUI_APP_STORE_RESIDUE names {file}::{function}, which is not a function of that file"
        );
        assert!(
            reason.len() > 20,
            "TUI_APP_STORE_RESIDUE's entry for {file}::{function} has no usable reason: {reason:?}"
        );
        if let Some(previous) = previous {
            assert!(
                previous < (file, function),
                "TUI_APP_STORE_RESIDUE is not sorted: {previous:?} precedes {:?}",
                (file, function)
            );
        }
        previous = Some((file, function));
    }
}

/// The scanner finds a call in production code, attributes it to its enclosing
/// function, and ignores one inside a `#[cfg(test)]` module and one in a doc
/// comment.
///
/// A test for the test, because a source scan that silently found nothing would
/// pass the gate above for the wrong reason.
#[test]
fn the_scanner_attributes_a_call_and_ignores_tests_and_comments() {
    let source = "\
fn real() {\n    let s = open_store(\"a\");\n}\n\
/// See open_store(\"x\") for the reason.\n\
pub(crate) fn documented() {\n    let _ = 1;\n}\n\
#[cfg(test)]\nmod tests {\n    fn t() { open_store(\"a\"); }\n}\n";
    assert_eq!(
        store_opens_in(source),
        vec!["real".to_string()],
        "the scanner found something other than the one production call"
    );
}

/// Every `open_store(` call in the production code of [`QUERY_LAYER_SOURCES`],
/// as `(path relative to the repo root, enclosing function)` pairs.
fn store_opens_in_app() -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for file in QUERY_LAYER_SOURCES {
        for function in store_opens_in(&read_source(file)) {
            found.insert((file.to_string(), function));
        }
    }
    found
}

/// One source file of the crate, read from the repo root the build ran in.
fn read_source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The functions of `source` that call `open_store(`, with `#[cfg(test)]`
/// modules and line comments removed first.
///
/// A unit test may open a store: it is the module's own test, and it is not a
/// path the frame takes. A doc comment may name `open_store` too, and several
/// of them do, explaining why a drafts path uses `Store::open` instead.
fn store_opens_in(source: &str) -> Vec<String> {
    let source = strip_test_modules(&strip_line_comments(source));
    let mut current = String::new();
    let mut found = Vec::new();
    for line in source.lines() {
        if let Some(name) = function_name(line) {
            current = name;
        }
        if line.contains("open_store(") && !found.contains(&current) {
            found.push(current.clone());
        }
    }
    found
}

/// The name a line declares, for a line that declares a function.
fn function_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = ["pub(crate) ", "pub(super) ", "pub(self) ", "pub "]
        .iter()
        .find_map(|prefix| trimmed.strip_prefix(prefix))
        .unwrap_or(trimmed);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Drop everything from `//` to the end of the line, string literals included:
/// a scan over source text is not a parser, and a doc comment naming
/// `open_store` is not a call to it.
fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drop every `#[cfg(test)] mod … { … }` block, braces matched.
///
/// The same helper `tests/architecture_boundaries.rs` uses, for the same
/// reason and with the same limitation: braces inside string literals inside a
/// test module would confuse it, and none of the scanned files has one.
fn strip_test_modules(source: &str) -> String {
    let mut out = source.to_string();
    loop {
        let Some(at) = out.find("#[cfg(test)]") else {
            return out;
        };
        let after = &out[at + "#[cfg(test)]".len()..];
        let trimmed = after.trim_start();
        if !trimmed.starts_with("mod ") && !trimmed.starts_with("pub mod ") {
            out.replace_range(at..at + 1, " ");
            continue;
        }
        let Some(open) = out[at..].find('{').map(|i| at + i) else {
            return out;
        };
        let mut depth = 0usize;
        let mut end = out.len();
        for (i, ch) in out[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.replace_range(at..end, "");
    }
}

// ---------------------------------------------------------------------------
// (g) the preview cost
// ---------------------------------------------------------------------------

/// W1's budget over the query layer: the daemon-backed preview walk's p95 may
/// exceed the store-backed one's by at most
/// [`PREVIEW_P95_DELTA_CEILING_MS`].
///
/// `#[ignore]`, for two reasons that are both worth stating rather than
/// working around:
///
/// - **It needs a fixture the rest of this file does not.** 200 rows walked
///   twice is 400 store opens and 400 dispatches, a few seconds; the other
///   rows here run in milliseconds on three. A wall-clock row in the default
///   suite is a flake waiting for a loaded CI box.
/// - **It is a lower bound on W1, not W1.** It measures the dispatcher in
///   this process, so it prices the store read and the row conversion and
///   *not* the socket, the framing or the session thread's hop. W1 itself is
///   `NOT TAKEN` in `docs/baselines/pre-daemon/measurements.md` (no account,
///   no terminal on that host), so there is no recorded p95 to compare an
///   absolute against; what can be asserted here is that the query layer's own
///   work does not eat the 5 ms budget before the transport gets any of it.
///
/// Run it with `cargo test --offline --lib queries_tests -- --ignored`.
/// P5-U11 owns the real W1 rerun, against a terminal and an account.
#[test]
#[ignore = "wall-clock, needs a 200-row fixture; a lower bound on W1, not W1 itself"]
fn the_preview_query_stays_inside_the_p95_delta_ceiling() {
    const ROWS: i64 = 200;

    let fixture = Fixture::new();
    for uid in 1..=ROWS {
        ingest_fixture(
            "inbox",
            uid,
            &fixture_email(
                &format!("row {uid}"),
                "Mon, 01 Jan 2024 09:00:00 +0000",
                true,
            ),
        );
    }
    let app = app_on_fixture();
    let refs: Vec<MessageRef> = load_emails(ACCOUNT, "inbox")
        .iter()
        .filter_map(|entry| entry.msg)
        .collect();
    assert_eq!(refs.len(), ROWS as usize);

    // One untimed pass each, so neither side pays the page-cache warm-up the
    // other has already paid.
    for msg in &refs {
        let _ = app.load_message_body(*msg);
        let _ = message_body(&fixture, ACCOUNT, *msg);
    }

    let store = p95(&refs, |msg| {
        let _ = app.load_message_body(msg);
    });
    let daemon = p95(&refs, |msg| {
        let _ = message_body(&fixture, ACCOUNT, msg);
    });

    assert!(
        daemon - store <= PREVIEW_P95_DELTA_CEILING_MS,
        "the daemon-backed preview p95 is {daemon:.3} ms against the store-backed {store:.3} ms, \
         a delta of {:.3} ms over the {PREVIEW_P95_DELTA_CEILING_MS} ms W1 budget",
        daemon - store,
    );
}

/// The p95 of one cursor move, in milliseconds, over a walk down `refs` and
/// back up: the twenty-rows-each-way shape W1 names, at whatever length the
/// caller seeded.
fn p95(refs: &[MessageRef], mut read: impl FnMut(MessageRef)) -> f64 {
    let mut samples: Vec<f64> = Vec::with_capacity(refs.len() * 2);
    for msg in refs.iter().chain(refs.iter().rev()) {
        let started = std::time::Instant::now();
        read(*msg);
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in a duration"));
    samples[(samples.len() as f64 * 0.95) as usize % samples.len()]
}

/// The one compile-time assertion in this file: [`Session`] is a
/// [`Queries`] source, so the query layer the tests above drive over an
/// in-process dispatcher is the same code the TUI drives over the socket.
///
/// Never called. If it were removed the suite would still pass over the
/// fixture and the TUI would have no query layer at all, which is the failure
/// this line exists to make impossible.
#[allow(dead_code)]
fn a_session_is_a_query_source(session: &crate::tui::session::Session) -> &dyn Queries {
    session
}
