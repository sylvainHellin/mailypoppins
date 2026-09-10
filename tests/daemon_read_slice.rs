//! The read slice, moved onto the daemon (#0123, plan unit P4-U3).
//!
//! Four commands answer from the store today and must answer from the daemon
//! tomorrow without a byte moving: `mp show` (and `mp show --json`),
//! `mp list-messages`, `mp dump-mailbox --json` and `mp search --local`. Three
//! methods carry them: `message.get`, `message.list` and `message.search`.
//!
//! This file is a **contract test**. It is written before the methods exist,
//! against the shapes fixed here and in `docs/daemon-protocol.md`, and it fails
//! to compile against today's tree; that failure is the proof the contract has
//! no stub behind it. The implementer (P4-U4) does not edit this file.
//!
//! # The surface under test
//!
//! ```text
//! message.get    {account, id}                     -> ShownMessage
//!                {account, selector, mailbox?}     -> ShownMessage
//!                (+ body: bool, default true)
//! message.list   {account, mailbox, limit?}        -> {account, mailbox, total, messages:[row]}
//!                {account, projection:"envelope",
//!                 mailbox?: str|[str]|null}        -> {account, records:[EnvelopeRecord]}
//! message.search {account, query, mailbox?, limit?, body?,
//!                 from?, to?, cc?, subject?, body_query?, filename?,
//!                 has_attachment?, after?, before?}
//!                                                  -> {account, query, hits:[row + mailbox + body?]}
//! ```
//!
//! ## `message.get`
//!
//! **The result is the record `mp show --json` prints**, field for field:
//! `mailypoppins::read_cmd::ShownMessage`. That is the whole point of the
//! shape. The text rendering is `read_cmd::render_show` over the same record,
//! so a client that received the payload can print either answer without
//! opening a store, and the JSON answer is that payload re-serialised rather
//! than a second projection that could drift from it. `ShownMessage` and
//! `ShownAttachment` therefore have to `Deserialize`, which is one of the
//! contract items this file pins at compile time.
//!
//! **A message is addressed either by `id` or by `selector`, never by both and
//! never by neither.** `id` is `"<mailbox>/<uid>"`, the addressing
//! `message.materialise_attachment` already uses, taken literally. `selector`
//! is the grammar `mp show` takes from a user (`<message-id>`,
//! `<mailbox>/<message-id>`, `mp://<account>/<mailbox>/<message-id>`), resolved
//! the way `resolve_received_arg` resolves it, with the optional `mailbox`
//! narrowing it exactly as `mp show --mailbox` does. The selector form exists
//! because the resolution needs the store and the client no longer has one;
//! the id form exists because every other `message.*` method addresses a
//! message that way and a GUI holding a listed row has no selector to spell.
//! Which *account* a selector names stays a client-side decision, because
//! `Selector::parse` needs no store.
//!
//! **`body` defaults to `true` and its absence is not its nullity.** With
//! `body: false` the key is absent from the result; with the default it is
//! present and is `null` when the store holds no readable body for the row.
//! `mp show` prints its "no stored body" sentence for exactly that `null`, so
//! collapsing the two would make a bodyless answer indistinguishable from an
//! evicted blob. The asymmetry with `message.search` below (which defaults to
//! `false`) is deliberate: `mp show` always prints a body and `mp search`
//! prints one only under `--full`.
//!
//! ## `message.list`
//!
//! **The single-mailbox shape does not move.** `docs/daemon-protocol.md` and
//! `tests/daemon_read_only_methods.rs` pin it, `mp list-messages` already
//! routes through it, and this slice adds to it rather than reshaping it.
//!
//! **`projection: "envelope"` is how `mp dump-mailbox --json` is served.** The
//! records are `mailypoppins::dump::EnvelopeRecord`, so the client re-serialises
//! them with `dump::to_ndjson` and the NDJSON ordering contract, the field
//! order and the null handling stay in the one place that already owns them.
//! `EnvelopeRecord` and `AttachmentRecord` therefore have to `Deserialize`,
//! the second and third compile-time contract items.
//!
//! **The envelope projection is per account and covers every selected mailbox
//! in one answer**, because the dump's sort key is
//! `(account, mailbox, date_sort, message_id, subject, uid)` and a client that
//! merged per-mailbox answers would be re-implementing it. `mailbox` accepts a
//! name, an array of names (the repeatable `--mailbox`) or `null`/absent for
//! every listable mailbox of the account; a name that is not one of the
//! account's mailboxes selects nothing rather than failing, exactly as
//! `dump::collect_records` treats an unmatched filter. The client's only
//! ordering duty is to call the accounts in ascending name order.
//!
//! **A storeless account is `account_not_ready` here as everywhere else**, and
//! the *client* turns that into "no records" for the dump, because
//! `dump::collect_records` skips an account it cannot open rather than
//! refusing the run. Two answers about one account may not contradict each
//! other, so the method does not invent a fourth, quieter refusal.
//!
//! ## `message.search`
//!
//! **The params mirror `mp search --local`'s flags** and the daemon builds the
//! query with `search::from_cli`, so one parser serves every backend and the
//! client sends what the user typed. `body_query` is the wire name of the
//! `--body` flag, because `body` is already the "send me the bodies" switch
//! (`--full`); one key may not mean two things.
//!
//! **Hits come back in the store's ranking order** (`store::search`), which is
//! what `mp search --local` prints under "best match first", and each hit is a
//! `message.list` row plus the `mailbox` it was found in, which is what
//! `Selector::for_message` needs to render the line.
//!
//! **A query the search layer cannot use is `-32602`**, not `-32603`: the
//! parameter is wrong, not the store.
//!
//! # Parity, and what proves routing
//!
//! Every command is compared against the `pre-daemon` oracle over the same
//! seeded root: stdout, stderr and exit code, across the flag combinations of
//! `tests/support/read_fixture.rs` and the error cases (unknown mailbox,
//! unknown selector, a missing `--json`, an empty result, a storeless
//! account). The routed side runs under `MAILYPOPPINS_DAEMON_REQUIRE=1`
//! (`DaemonFixture::mp_routed`), so a command that quietly answered in process
//! fails instead of passing for the daemon's work.
//!
//! `MAILYPOPPINS_DAEMON_REQUIRE` is checked at the end of `main`, so a command
//! that returns early escapes it (`mp search --local` does today). The proof
//! that survives that is
//! [`no_read_command_can_still_answer_without_a_daemon`]: with nothing
//! listening and auto-start off, a command with no in-process path left cannot
//! answer at all, and exits 4.
//!
//! A refusal therefore has to reach the user in the oracle's exact bytes. That
//! rules out routing an error through `daemon_call`'s `✗ {message}` printer
//! for anything the oracle reports as an `anyhow` error; whether the client
//! resolves it before the call (as `routed_list_messages` already resolves a
//! mailbox name) or maps the RPC error back into one is the implementer's
//! choice, and the assertion is the same either way.
//!
//! # The three legacy suites, twinned
//!
//! `tests/cli_read_surface_integration.rs`, `tests/dump_mailbox_integration.rs`
//! and `tests/store_search_integration.rs` keep running unchanged against the
//! in-process path. Their observable assertions are re-run here through
//! [`Slice::for_each_binary`], which runs one assertion body twice, once over
//! the oracle's output and once over the routed one, having first proved the
//! two are byte-identical.
//!
//! **What could not be twinned**, and why:
//!
//! - `store_search_integration.rs` is a library test: it calls
//!   `mailypoppins::store::search::search` directly and asserts on store row
//!   ids, which never appear on any CLI surface. Its CLI-visible half (subject
//!   ranking over body ranking, the limit taking the best hits, mailbox
//!   scoping, a unicode round trip, punctuation that is not FTS5 syntax, and
//!   an empty result) is twinned through `mp search --local`. Its index-health
//!   half (`index_drift` after a re-ingest, a UIDVALIDITY rebind, a delete, a
//!   prune, a move) has no command that shows it and stays where it is.
//! - `dump_mailbox_integration.rs`'s `EXPECTED` constant belongs to that
//!   file's own fixture. The twin here asserts the same three properties over
//!   this fixture (the record shape, determinism, the selectors) rather than
//!   copying a literal that would then have two owners.

mod support;

use std::path::Path;
use std::process::Output;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::daemon::dispatch::{CancelScope, MethodKind};
use mailypoppins::daemon::methods::message::MESSAGE_READ_METHOD_SPECS;
use mailypoppins::dump::{to_ndjson, EnvelopeRecord};
use mailypoppins::read_cmd::{to_json, ShownMessage};
use mailypoppins::store::read;
use mailypoppins::store::search::search;

use support::parity::{
    assert_byte_identical, mp_no_daemon, oracle, socket_path, DaemonFixture, EXIT_UNAVAILABLE,
};
use support::read_fixture as fixture;

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// The three methods of the slice, in the order their spec array declares
/// them, which is method-name order like every other family.
const READ_METHODS: [&str; 3] = ["message.get", "message.list", "message.search"];

/// Every field of an envelope record, which is every field the dump may carry
/// and, since none of them is a path, the whole of what a dump may say about a
/// message (`docs/dump-allow-list.md`).
const ENVELOPE_FIELDS: [&str; 12] = [
    "account",
    "attachments",
    "cc",
    "date_sort",
    "flags",
    "from",
    "invite",
    "mailbox",
    "message_id",
    "subject",
    "thread",
    "to",
];

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A seeded root, a daemon serving it, and the pre-daemon oracle beside it.
///
/// The field order is the drop order: the daemon dies before the directory it
/// was reading is removed.
struct Slice {
    daemon: DaemonFixture,
    tmp: TempDir,
}

impl Slice {
    /// Seed the store, then start a daemon over it. In that order: the daemon
    /// loads `config.toml` once, at startup.
    fn start() -> Slice {
        let tmp = TempDir::new().expect("a temporary read-slice root");
        fixture::seed(tmp.path());
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// The client under test, made to prove it reached the daemon.
    fn routed(&self, args: &[&str]) -> Output {
        self.daemon.mp_routed(args)
    }

    /// The pre-daemon binary over the same root: the definition of parity.
    fn oracle(&self, args: &[&str]) -> Output {
        oracle(args, self.root())
    }

    /// Assert the two binaries agree byte for byte, and run `check` over each
    /// of their outputs.
    ///
    /// This is the twin runner: a legacy assertion written once runs once
    /// against the pre-daemon path and once against the routed one.
    fn for_each_binary(&self, args: &[&str], check: impl Fn(&str, &Output)) {
        let routed = self.routed(args);
        let direct = self.oracle(args);
        assert_byte_identical(&routed, &direct);
        check("pre-daemon", &direct);
        check("routed", &routed);
    }

    /// The stdout both binaries produced for `args`, after proving they agree.
    fn agreed_stdout(&self, args: &[&str]) -> String {
        let routed = self.routed(args);
        assert_byte_identical(&routed, &self.oracle(args));
        String::from_utf8(routed.stdout).expect("the read surface prints UTF-8")
    }

    /// A connected, initialized client of this daemon.
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

/// Bound every await, so a daemon that stops answering fails the test rather
/// than the session.
async fn within<F, T>(what: &str, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(DEADLINE, future).await {
        Ok(value) => value,
        Err(_) => panic!("{what} did not answer within {DEADLINE:?}"),
    }
}

/// Call a method that must succeed.
async fn call(conn: &mut Connection, method: &str, params: Value) -> Value {
    within(method, conn.call(method, params))
        .await
        .unwrap_or_else(|e| panic!("{method} failed: {e:?}"))
}

/// Call a method that must be refused, and return the typed error.
async fn call_err(conn: &mut Connection, method: &str, params: Value) -> RpcError {
    let error = within(method, conn.call(method, params))
        .await
        .expect_err("this call must be refused");
    match error {
        ClientError::Rpc(error) => error,
        other => panic!("{method}: expected a typed RPC error, got {other:?}"),
    }
}

/// Nothing a read method answers may name a file.
///
/// The dump's contract says so in as many words ("no filesystem path appears
/// in the output"), and the same holds for every other read result: a client
/// that cannot open the store must not be handed a path into it.
fn assert_path_free(what: &str, value: &Value, root: &Path) {
    let text = value.to_string();
    let root = root.display().to_string();
    for needle in [root.as_str(), "store.sqlite3", "/blobs/", "/runtime/"] {
        assert!(
            !text.contains(needle),
            "{what} leaked {needle:?} into its result: {text}"
        );
    }
}

/// The `data` payload an error code fixes, which every code used here does.
fn data_of(error: &RpcError) -> &Value {
    error
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("code {} fixes a data payload", error.code))
}

/// The `"<mailbox>/<uid>"` id of one hit or one listed row.
fn message_id_of(hit: &Value) -> String {
    format!(
        "{}/{}",
        hit["mailbox"].as_str().expect("a hit names its mailbox"),
        hit["uid"].as_i64().expect("a hit carries its uid")
    )
}

// ---------------------------------------------------------------------------
// 1. The methods themselves
// ---------------------------------------------------------------------------

/// The three declarations, and the four facts each one fixes: the wire name,
/// the kind, the first protocol version and what a disconnect does to it.
///
/// All three are queries: they read the store and change nothing, so their
/// answers carry no revision and invalidate no resource. All three are durable,
/// which is what a method that never thought about cancellation means; a read
/// that finishes in milliseconds has no reason to be torn down.
#[test]
fn the_read_slice_declares_three_durable_queries() {
    let names: Vec<&str> = MESSAGE_READ_METHOD_SPECS.iter().map(|s| s.name).collect();
    assert_eq!(names, READ_METHODS, "the slice serves exactly these three");

    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the array is in method-name order");

    for spec in MESSAGE_READ_METHOD_SPECS {
        assert_eq!(spec.kind, MethodKind::Query, "{} is a read", spec.name);
        assert_eq!(spec.since, 1, "{} is served from protocol 1", spec.name);
        assert_eq!(
            spec.cancel_scope,
            CancelScope::Durable,
            "{} outlives its caller",
            spec.name
        );
    }
}

/// A method may not be served without being advertised: the handshake derives
/// the capability list from the dispatcher, so the three names appearing there
/// is what says they are registered rather than merely declared.
#[tokio::test]
async fn the_daemon_advertises_the_three_read_methods() {
    let slice = Slice::start();
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&socket_path(slice.root())),
    )
    .await
    .expect("connect");
    let result = within(
        "Connection::initialize",
        conn.initialize(
            ClientInfo {
                kind: ClientKind::Cli,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Identity {
                data_dir: slice.root().to_path_buf(),
                config_dir: slice.root().to_path_buf(),
            },
            &[],
            &[],
        ),
    )
    .await
    .expect("handshake");

    for method in READ_METHODS {
        assert!(
            result.capabilities.iter().any(|c| c == method),
            "{method} is served, so it is advertised: {:?}",
            result.capabilities
        );
    }
}

// ---------------------------------------------------------------------------
// 2. `message.get`
// ---------------------------------------------------------------------------

/// The result *is* the record `mp show --json` prints: deserialise it into
/// `ShownMessage`, serialise it back through the command's own writer, and the
/// bytes are the command's.
///
/// This is the strongest form of "the client renders from the payload alone":
/// nothing in the comparison consults the store.
#[tokio::test]
async fn message_get_returns_the_record_mp_show_prints() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "message.get",
        json!({"account": fixture::ACCOUNT, "id": "inbox/1"}),
    )
    .await;
    assert_path_free("message.get", &result, slice.root());

    let shown: ShownMessage =
        serde_json::from_value(result.clone()).expect("the result is a ShownMessage");
    assert_eq!(shown.account, fixture::ACCOUNT);
    assert_eq!(shown.mailbox, "inbox");
    assert_eq!(shown.message_id, format!("<{}>", fixture::BERICHT));
    assert_eq!(shown.subject.as_deref(), Some("Bericht über Anträge"));
    assert_eq!(
        shown.cc.as_deref(),
        Some("\"Prof. Petzold\" <petzold@example.com>")
    );
    assert_eq!(
        shown.selector,
        format!("mp://{}/inbox/{}", fixture::ACCOUNT, fixture::BERICHT)
    );
    let flags: Vec<&str> = shown.flags.iter().map(String::as_str).collect();
    assert_eq!(
        flags,
        ["read", "answered", "forwarded", "flagged"],
        "every axis the store holds reaches the reader"
    );
    let names: Vec<&str> = shown.attachments.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        ["notes.pdf", "agenda.txt"],
        "a reader sees the parts in the message's own order; only the dump sorts them"
    );
    assert!(!shown.invite, "this one carries no invitation");

    let printed = slice.agreed_stdout(&["show", "--json", fixture::BERICHT]);
    assert_eq!(
        format!("{}\n", to_json(&shown).expect("serialise")),
        printed,
        "`mp show --json` prints this payload and nothing else"
    );
}

/// A selector is resolved by the daemon, because resolving one needs the store
/// the client no longer has. All four spellings `mp show` accepts land on the
/// same message as the id form.
#[tokio::test]
async fn message_get_resolves_every_selector_spelling_to_one_message() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let by_id = call(
        &mut conn,
        "message.get",
        json!({"account": fixture::ACCOUNT, "id": "inbox/1"}),
    )
    .await;

    for params in [
        json!({"account": fixture::ACCOUNT, "selector": fixture::BERICHT}),
        json!({"account": fixture::ACCOUNT, "selector": format!("inbox/{}", fixture::BERICHT)}),
        json!({"account": fixture::ACCOUNT,
               "selector": format!("mp://{}/inbox/{}", fixture::ACCOUNT, fixture::BERICHT)}),
        json!({"account": fixture::ACCOUNT, "selector": fixture::BERICHT, "mailbox": "inbox"}),
    ] {
        let resolved = call(&mut conn, "message.get", params.clone()).await;
        assert_eq!(
            resolved, by_id,
            "{params} resolves to the id form's message"
        );
    }

    // Exactly one address, always: neither is a caller who forgot, both is a
    // caller who may disagree with themselves.
    for params in [
        json!({"account": fixture::ACCOUNT}),
        json!({"account": fixture::ACCOUNT, "id": "inbox/1", "selector": fixture::BERICHT}),
    ] {
        let refused = call_err(&mut conn, "message.get", params.clone()).await;
        assert_eq!(refused.code, -32602, "{params} is not an address");
    }
}

/// `body` is a switch with three outcomes, and the difference between two of
/// them is what `mp show` prints.
#[tokio::test]
async fn message_get_tells_an_unasked_body_from_an_unreadable_one() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let full = call(
        &mut conn,
        "message.get",
        json!({"account": fixture::ACCOUNT, "id": "inbox/1"}),
    )
    .await;
    assert_eq!(
        full["body"].as_str(),
        Some("der quarterly ledger ist beigefügt\n"),
        "the default carries the body"
    );

    let headers_only = call(
        &mut conn,
        "message.get",
        json!({"account": fixture::ACCOUNT, "id": "inbox/1", "body": false}),
    )
    .await;
    assert!(
        headers_only.get("body").is_none(),
        "body: false omits the key rather than nulling it: {headers_only}"
    );

    // The row with nothing to read: `null`, which is the sentinel `mp show`
    // turns into its "no stored body" sentence.
    let empty = call(
        &mut conn,
        "message.get",
        json!({"account": fixture::ACCOUNT, "id": "inbox/4"}),
    )
    .await;
    assert!(
        empty.get("body").is_some() && empty["body"].is_null(),
        "an unreadable body is present and null: {empty}"
    );

    // HTML-only mail reads as the flattened text, not as markup (RD-03).
    let markup = call(
        &mut conn,
        "message.get",
        json!({"account": fixture::ACCOUNT, "id": "inbox/3"}),
    )
    .await;
    let body = markup["body"]
        .as_str()
        .expect("the HTML message has a body");
    assert!(body.contains("quarterly summary follows"), "{body}");
    assert!(!body.contains("<p>"), "no markup reaches a reader: {body}");
}

/// Everything a caller can get wrong about one message, and the code it gets.
#[tokio::test]
async fn message_get_refuses_what_it_cannot_address() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for params in [
        json!({"account": fixture::ACCOUNT, "id": "inbox/9999"}),
        json!({"account": fixture::ACCOUNT, "id": "inbox"}),
        json!({"account": fixture::ACCOUNT, "id": "inbox/not-a-number"}),
        json!({"account": fixture::ACCOUNT, "id": "/1"}),
        json!({"account": fixture::ACCOUNT, "id": "nope/1"}),
        json!({"account": fixture::ACCOUNT, "selector": "nothing-here@example.com"}),
        json!({"account": fixture::ACCOUNT, "selector": fixture::BERICHT, "mailbox": "nope"}),
        json!({"account": fixture::ACCOUNT,
               "selector": fixture::ONLY_IN_BETA}),
    ] {
        let refused = call_err(&mut conn, "message.get", params.clone()).await;
        assert_eq!(refused.code, -32602, "{params} must be -32602");
    }

    let unknown = call_err(
        &mut conn,
        "message.get",
        json!({"account": fixture::UNKNOWN_ACCOUNT, "id": "inbox/1"}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
    assert_eq!(data_of(&unknown)["account"], fixture::UNKNOWN_ACCOUNT);

    let storeless = call_err(
        &mut conn,
        "message.get",
        json!({"account": fixture::STORELESS_ACCOUNT, "id": "inbox/1"}),
    )
    .await;
    assert_eq!(storeless.code, ErrorCode::AccountNotReady.code());
    assert_eq!(data_of(&storeless)["account"], fixture::STORELESS_ACCOUNT);
}

// ---------------------------------------------------------------------------
// 3. `message.list`
// ---------------------------------------------------------------------------

/// The shape P2-U10 pinned is still exactly the shape: an extension that
/// reshaped the answer would break `mp list-messages`, which already routes.
#[tokio::test]
async fn message_list_keeps_its_single_mailbox_shape() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "mailbox": "inbox", "limit": 2}),
    )
    .await;
    assert_path_free("message.list", &result, slice.root());
    assert_eq!(result["account"], fixture::ACCOUNT);
    assert_eq!(result["mailbox"], "inbox");
    assert_eq!(result["total"], 4, "total ignores the limit");
    assert_eq!(result["messages"].as_array().expect("messages").len(), 2);
    assert!(
        result.get("records").is_none(),
        "the list projection carries no envelope records: {result}"
    );

    let explicit = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "mailbox": "inbox", "limit": 2,
               "projection": "list"}),
    )
    .await;
    assert_eq!(explicit, result, "`list` is the default projection");

    let refused = call_err(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "mailbox": "inbox", "projection": "sideways"}),
    )
    .await;
    assert_eq!(refused.code, -32602, "an unknown projection is a bad param");
}

/// The envelope projection is `mp dump-mailbox --json` on the wire: the
/// records the client re-serialises are the command's own output, line for
/// line, in the dump's order.
#[tokio::test]
async fn message_list_envelope_projection_reproduces_the_dump() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "projection": "envelope"}),
    )
    .await;
    assert_path_free("message.list envelope", &result, slice.root());
    assert_eq!(result["account"], fixture::ACCOUNT);

    let records: Vec<EnvelopeRecord> =
        serde_json::from_value(result["records"].clone()).expect("records are EnvelopeRecords");
    let printed = slice.agreed_stdout(&["-A", fixture::ACCOUNT, "dump-mailbox", "--json"]);
    assert_eq!(
        to_ndjson(&records),
        printed,
        "the wire records serialise to the dump the command prints, in its order"
    );

    // Which is also the whole ordering contract: mailbox, then date, and the
    // mailbox ids of this account do not sort the way the sidebar lists them.
    let mailboxes: Vec<&str> = records.iter().map(|r| r.mailbox.as_str()).collect();
    let mut sorted = mailboxes.clone();
    sorted.sort_unstable();
    assert_eq!(mailboxes, sorted, "records are grouped in mailbox order");
    assert!(
        mailboxes.starts_with(&[fixture::MAILBOXES[0]]),
        "`Team/Reports` sorts before `inbox`: {mailboxes:?}"
    );
}

/// A record says what the dump says and nothing more: twelve fields, none of
/// them a path.
#[tokio::test]
async fn every_envelope_record_carries_the_allow_listed_fields_only() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "projection": "envelope"}),
    )
    .await;
    let records = result["records"].as_array().expect("records");
    assert_eq!(records.len(), 6, "every alpha message is dumped");

    for record in records {
        let object = record.as_object().expect("a record is an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys, ENVELOPE_FIELDS,
            "the record shape is the dump's, exactly: {record}"
        );
        assert_path_free("an envelope record", record, slice.root());
    }
}

/// `mailbox` selects, in the three spellings the command offers, and an
/// unmatched name selects nothing rather than failing.
#[tokio::test]
async fn the_envelope_projection_takes_a_name_a_list_or_nothing() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let count = |result: &Value| result["records"].as_array().expect("records").len();

    let all = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "projection": "envelope", "mailbox": null}),
    )
    .await;
    assert_eq!(count(&all), 6, "null is every listable mailbox");

    let inbox = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "projection": "envelope", "mailbox": "INBOX"}),
    )
    .await;
    assert_eq!(count(&inbox), 4, "a name resolves case-insensitively");

    let pair = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "projection": "envelope",
               "mailbox": ["sent", "drafts"]}),
    )
    .await;
    assert_eq!(count(&pair), 1, "drafts contribute no envelope records");

    let named = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "projection": "envelope",
               "mailbox": "Team/Reports"}),
    )
    .await;
    assert_eq!(count(&named), 1);

    let unmatched = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "projection": "envelope", "mailbox": "nope"}),
    )
    .await;
    assert_eq!(
        count(&unmatched),
        0,
        "an unmatched dump filter selects nothing, as `dump::collect_records` does"
    );
}

/// The two account refusals, identical in both projections: one account, one
/// answer.
#[tokio::test]
async fn message_list_refuses_an_unknown_and_a_storeless_account() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for projection in [json!({}), json!({"projection": "envelope"})] {
        let mut unknown_params = json!({"account": fixture::UNKNOWN_ACCOUNT, "mailbox": "inbox"});
        let mut storeless_params =
            json!({"account": fixture::STORELESS_ACCOUNT, "mailbox": "inbox"});
        for (key, value) in projection.as_object().expect("an object") {
            unknown_params[key] = value.clone();
            storeless_params[key] = value.clone();
        }

        let unknown = call_err(&mut conn, "message.list", unknown_params).await;
        assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
        assert_eq!(data_of(&unknown)["account"], fixture::UNKNOWN_ACCOUNT);

        let storeless = call_err(&mut conn, "message.list", storeless_params).await;
        assert_eq!(storeless.code, ErrorCode::AccountNotReady.code());
        assert_eq!(data_of(&storeless)["account"], fixture::STORELESS_ACCOUNT);
    }
}

// ---------------------------------------------------------------------------
// 4. `message.search`
// ---------------------------------------------------------------------------

/// The order on the wire is the store's ranked order, asserted against the
/// store rather than against a transcription of it.
#[tokio::test]
async fn message_search_answers_in_the_stores_ranking_order() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "message.search",
        json!({"account": fixture::ACCOUNT, "query": "ledger", "limit": 20}),
    )
    .await;
    assert_path_free("message.search", &result, slice.root());
    assert_eq!(result["account"], fixture::ACCOUNT);
    assert_eq!(result["query"], "ledger");

    let hits = result["hits"].as_array().expect("hits");
    let wire: Vec<String> = hits.iter().map(message_id_of).collect();

    let store = fixture::store(slice.root(), fixture::ACCOUNT);
    let expected: Vec<String> = search(&store, fixture::ACCOUNT, "ledger", None, 20)
        .expect("the store answers the same query")
        .into_iter()
        .map(|hit| format!("{}/{}", hit.row.mailbox, hit.row.uid))
        .collect();
    assert_eq!(wire, expected, "the wire order is the store's ranking");
    assert_eq!(
        wire,
        ["inbox/3", "sent/1", "inbox/1"],
        "the subject hit outranks the two body hits"
    );

    // A hit is a listing row plus the mailbox it was found in, which is what
    // renders the selector; the bodies are `--full`'s business and absent.
    let first = &hits[0];
    for key in [
        "uid",
        "mailbox",
        "message_id",
        "from",
        "subject",
        "date_sort",
        "date_display",
        "flags",
        "has_attachments",
    ] {
        assert!(first.get(key).is_some(), "a hit carries {key}: {first}");
    }
    assert!(
        first.get("body").is_none(),
        "no body without asking for one: {first}"
    );
}

/// The flags of `mp search --local`, on the wire: the scope, the cap, the
/// bodies, the field filters and a query that matches nothing.
#[tokio::test]
async fn message_search_mirrors_the_local_search_flags() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let hits = |result: &Value| {
        result["hits"]
            .as_array()
            .expect("hits")
            .iter()
            .map(message_id_of)
            .collect::<Vec<_>>()
    };

    let scoped = call(
        &mut conn,
        "message.search",
        json!({"account": fixture::ACCOUNT, "query": "ledger", "mailbox": "sent"}),
    )
    .await;
    assert_eq!(hits(&scoped), vec!["sent/1".to_string()]);

    let by_role = call(
        &mut conn,
        "message.search",
        json!({"account": fixture::ACCOUNT, "query": "ledger", "mailbox": "SENT"}),
    )
    .await;
    assert_eq!(hits(&by_role), hits(&scoped), "a mailbox name is a name");

    let capped = call(
        &mut conn,
        "message.search",
        json!({"account": fixture::ACCOUNT, "query": "ledger", "limit": 1}),
    )
    .await;
    assert_eq!(
        hits(&capped),
        vec!["inbox/3".to_string()],
        "the limit takes the best hit, not an arbitrary one"
    );

    let full = call(
        &mut conn,
        "message.search",
        json!({"account": fixture::ACCOUNT, "query": "ledger", "body": true}),
    )
    .await;
    for hit in full["hits"].as_array().expect("hits") {
        assert!(
            hit.get("body").is_some(),
            "body: true carries every body it can read: {hit}"
        );
    }

    let by_subject = call(
        &mut conn,
        "message.search",
        json!({"account": fixture::ACCOUNT, "query": "", "subject": "Ledger"}),
    )
    .await;
    assert_eq!(
        hits(&by_subject),
        vec!["inbox/3".to_string()],
        "the flags build the same query the grammar does"
    );

    let with_attachment = call(
        &mut conn,
        "message.search",
        json!({"account": fixture::ACCOUNT, "query": "ledger", "has_attachment": true}),
    )
    .await;
    let mut found = hits(&with_attachment);
    found.sort();
    assert_eq!(found, vec!["inbox/1".to_string(), "sent/1".to_string()]);

    let unicode = call(
        &mut conn,
        "message.search",
        json!({"account": fixture::ACCOUNT, "query": "Anträge"}),
    )
    .await;
    let mut accented = hits(&unicode);
    accented.sort();
    assert_eq!(
        accented,
        ["inbox/1".to_string(), "sent/1".to_string()],
        "both subjects carry the word, accents and all"
    );

    let nothing = call(
        &mut conn,
        "message.search",
        json!({"account": fixture::ACCOUNT, "query": "aardvark"}),
    )
    .await;
    assert!(
        hits(&nothing).is_empty(),
        "no match is an empty answer, not an error"
    );
}

/// A query with nothing to search for, an unknown mailbox and the two account
/// refusals.
#[tokio::test]
async fn message_search_refuses_a_query_it_cannot_run() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for params in [
        json!({"account": fixture::ACCOUNT, "query": "  ??  "}),
        json!({"account": fixture::ACCOUNT, "query": "\""}),
        json!({"account": fixture::ACCOUNT, "query": "ledger", "mailbox": "nope"}),
    ] {
        let refused = call_err(&mut conn, "message.search", params.clone()).await;
        assert_eq!(
            refused.code, -32602,
            "{params} is the caller's mistake, not the store's"
        );
    }

    let unknown = call_err(
        &mut conn,
        "message.search",
        json!({"account": fixture::UNKNOWN_ACCOUNT, "query": "ledger"}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());

    let storeless = call_err(
        &mut conn,
        "message.search",
        json!({"account": fixture::STORELESS_ACCOUNT, "query": "ledger"}),
    )
    .await;
    assert_eq!(storeless.code, ErrorCode::AccountNotReady.code());
}

// ---------------------------------------------------------------------------
// 5. Parity with the pre-daemon binary
// ---------------------------------------------------------------------------

/// `mp show`, every spelling and every refusal.
const SHOW_PARITY: &[&[&str]] = &[
    &["show", fixture::BERICHT],
    &["show", "--json", fixture::BERICHT],
    &["show", "--mailbox", "inbox", fixture::KICKOFF],
    &["show", fixture::MARKUP],
    &["show", "--json", fixture::MARKUP],
    &["show", "mp://beta/inbox/only-in-beta@example.com"],
    &["show", "--json", "mp://beta/inbox/only-in-beta@example.com"],
    &["show", "nothing-here@example.com"],
    &["show", "--mailbox", "nope", fixture::BERICHT],
    &["-A", "delta", "show", fixture::BERICHT],
];

/// `mp list-messages`, grouped and scoped, plus the empty and refused cases.
const LIST_PARITY: &[&[&str]] = &[
    &["list-messages"],
    &["list-messages", "-n", "1"],
    &["list-messages", "-n", "0"],
    &["list-messages", "--mailbox", "inbox"],
    &["list-messages", "--mailbox", "Team/Reports"],
    &["-A", "beta", "list-messages"],
    &["-A", "beta", "list-messages", "--mailbox", "sent"],
    &["list-messages", "--mailbox", "nope"],
    &["-A", "delta", "list-messages"],
];

/// `mp dump-mailbox`, including the missing `--json` and the account with no
/// store.
const DUMP_PARITY: &[&[&str]] = &[
    &["dump-mailbox", "--json"],
    &["-A", "alpha", "dump-mailbox", "--json"],
    &[
        "-A",
        "alpha",
        "dump-mailbox",
        "--json",
        "--mailbox",
        "INBOX",
    ],
    &[
        "-A",
        "alpha",
        "dump-mailbox",
        "--json",
        "--mailbox",
        "Team/Reports",
    ],
    &[
        "-A",
        "alpha",
        "dump-mailbox",
        "--json",
        "--mailbox",
        "sent",
        "--mailbox",
        "drafts",
    ],
    &["-A", "alpha", "dump-mailbox", "--json", "--mailbox", "nope"],
    &["-A", "beta", "dump-mailbox", "--json"],
    &["-A", "delta", "dump-mailbox", "--json"],
    &["dump-mailbox"],
];

/// `mp search --local`, the grammar, the flags and the refusals.
const SEARCH_PARITY: &[&[&str]] = &[
    &["search", "--local", "ledger"],
    &["search", "--local", "ledger", "--full"],
    &["search", "--local", "ledger", "-n", "1"],
    &["search", "--local", "ledger", "--mailbox", "sent"],
    &["search", "--local", "\"quarterly summary\""],
    &["search", "--local", "quarter*"],
    &["search", "--local", "Anträge"],
    &["search", "--local", "aardvark"],
    &["search", "--local", "--subject", "Ledger"],
    &["search", "--local", "--has-attachment", "ledger"],
    &["search", "--local", "  ??  "],
    &["search", "--local", "ledger", "--mailbox", "nope"],
    &["-A", "delta", "search", "--local", "ledger"],
];

/// The gate of the whole slice: stdout, stderr and exit code, against the
/// binary the migration may not change the behaviour of.
///
/// One daemon for the whole table, because starting one per invocation would
/// cost more than it proves: the fixture is read-only and no command here
/// leaves a mark on it.
#[test]
fn every_read_command_answers_byte_identically_to_the_pre_daemon_binary() {
    let slice = Slice::start();
    for args in SHOW_PARITY
        .iter()
        .chain(LIST_PARITY)
        .chain(DUMP_PARITY)
        .chain(SEARCH_PARITY)
    {
        let routed = slice.routed(args);
        let direct = slice.oracle(args);
        assert_byte_identical(&routed, &direct);
    }
}

/// The message with no `Message-ID:` header is addressed by the synthetic id
/// ingest gave it, which the fixture cannot spell but the store can.
///
/// It is the one row whose body the store cannot produce, so it is the one
/// invocation that exercises `mp show`'s degrade over the wire.
#[test]
fn the_bodyless_message_shows_identically_through_the_daemon() {
    let slice = Slice::start();
    let store = fixture::store(slice.root(), fixture::ACCOUNT);
    let row = read::list_mailbox(&store, fixture::ACCOUNT, "inbox")
        .expect("list the inbox")
        .into_iter()
        .find(|row| row.uid == 4)
        .expect("the fixture ingested the undated message");
    drop(store);
    let selector = format!(
        "mp://{}/inbox/{}",
        fixture::ACCOUNT,
        row.message_id.trim_matches(['<', '>'])
    );

    for args in [
        vec!["show", selector.as_str()],
        vec!["show", "--json", selector.as_str()],
    ] {
        assert_byte_identical(&slice.routed(&args), &slice.oracle(&args));
    }
}

/// Two routed runs over an unchanged store are byte-identical, which is the
/// dump's own contract and the property that makes a diff against another
/// build mean something.
#[test]
fn two_routed_runs_agree_with_each_other() {
    let slice = Slice::start();
    for args in [
        ["dump-mailbox", "--json"].as_slice(),
        ["list-messages"].as_slice(),
        ["show", "--json", fixture::BERICHT].as_slice(),
        ["search", "--local", "ledger"].as_slice(),
    ] {
        let first = slice.routed(args);
        let second = slice.routed(args);
        assert!(
            first.status.success(),
            "`mp {}` must succeed before its determinism means anything: {}",
            args.join(" "),
            String::from_utf8_lossy(&first.stderr)
        );
        assert_byte_identical(&first, &second);
    }
}

/// With no daemon and no way to start one, all four commands exit 4.
///
/// This is the routing proof that does not depend on the client noticing:
/// `MAILYPOPPINS_DAEMON_REQUIRE` is checked at the end of `main`, and a
/// command that returns early (`mp search --local` does, at `src/main.rs`'s
/// `if local` branch) never reaches the check. A command with no in-process
/// path left cannot answer at all when nothing is listening, which is a fact
/// about the command rather than about a guard it might have skipped.
///
/// It is also the CLI half of P4-U15's deletion gate, one slice early.
#[test]
fn no_read_command_can_still_answer_without_a_daemon() {
    let tmp = TempDir::new().expect("a temporary read-slice root");
    fixture::seed(tmp.path());

    for args in [
        ["show", fixture::BERICHT].as_slice(),
        ["list-messages"].as_slice(),
        ["dump-mailbox", "--json"].as_slice(),
        ["search", "--local", "ledger"].as_slice(),
    ] {
        let out = mp_no_daemon(args, tmp.path());
        assert_eq!(
            out.status.code(),
            Some(EXIT_UNAVAILABLE),
            "`mp {}` answered without a daemon:\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The routing proof itself: under `MAILYPOPPINS_DAEMON_REQUIRE=1` a command
/// that answered in process fails loudly, so every parity comparison above is
/// a comparison of the daemon's work.
#[test]
fn the_read_commands_are_answered_by_the_daemon() {
    let slice = Slice::start();
    for args in [
        ["show", fixture::BERICHT].as_slice(),
        ["list-messages"].as_slice(),
        ["dump-mailbox", "--json"].as_slice(),
        ["search", "--local", "ledger"].as_slice(),
    ] {
        let out = slice.routed(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`mp {}` must succeed when routed: {stderr}",
            args.join(" ")
        );
        assert!(
            !stderr.contains("answered without opening a daemon session"),
            "`mp {}` did not route: {stderr}",
            args.join(" ")
        );
    }
}

// ---------------------------------------------------------------------------
// 6. The legacy suites, twinned
// ---------------------------------------------------------------------------

/// `tests/cli_read_surface_integration.rs`, run once in process and once
/// routed: the selector grammar, the cross-account rule, the grouping, the
/// per-mailbox limit and the two refusals.
#[test]
fn the_cli_read_surface_contract_holds_on_both_routes() {
    let slice = Slice::start();

    let bericht_selector = format!("mp://alpha/inbox/{}", fixture::BERICHT);
    let scoped = format!("inbox/{}", fixture::BERICHT);
    let beta_selector = format!("mp://beta/inbox/{}", fixture::ONLY_IN_BETA);

    // show_prints_one_message_from_a_bare_key
    slice.for_each_binary(&["show", fixture::BERICHT], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{route}: {out:?}");
        assert!(
            stdout.contains("From: Ivana Hečimović <ivana@example.com>"),
            "{route}: {stdout}"
        );
        assert!(
            stdout.contains("Cc: \"Prof. Petzold\" <petzold@example.com>"),
            "{route}: {stdout}"
        );
        assert!(
            stdout.contains("Subject: Bericht über Anträge"),
            "{route}: {stdout}"
        );
        assert!(
            stdout.contains(&format!("Selector: {bericht_selector}")),
            "{route}: {stdout}"
        );
        assert!(
            stdout.contains("Flags: read, answered, forwarded, flagged"),
            "{route}: {stdout}"
        );
        assert!(stdout.contains("notes.pdf (14 B)"), "{route}: {stdout}");
        assert!(stdout.contains("agenda.txt (3 B)"), "{route}: {stdout}");
        assert!(
            stdout.trim_end().ends_with("beigefügt"),
            "{route}: {stdout}"
        );
    });

    // show_json_carries_the_envelope_and_the_body
    slice.for_each_binary(&["show", "--json", scoped.as_str()], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let value: Value = serde_json::from_str(&stdout).expect("valid JSON");
        assert_eq!(value["selector"], bericht_selector.as_str(), "{route}");
        assert_eq!(value["account"], "alpha", "{route}");
        assert_eq!(value["mailbox"], "inbox", "{route}");
        assert_eq!(value["subject"], "Bericht über Anträge", "{route}");
        assert_eq!(value["flags"][0], "read", "{route}");
        assert_eq!(value["attachments"][0]["name"], "notes.pdf", "{route}");
        assert_eq!(
            value["body"], "der quarterly ledger ist beigefügt\n",
            "{route}"
        );
    });

    // a_selector_naming_another_account_reads_that_accounts_store
    slice.for_each_binary(&["show", beta_selector.as_str()], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{route}: {out:?}");
        assert!(
            stdout.contains(&format!("Selector: {beta_selector}")),
            "{route}: {stdout}"
        );
        assert!(
            stdout.trim_end().ends_with("beta body"),
            "{route}: {stdout}"
        );
    });

    // a_missing_message_is_a_clear_error
    slice.for_each_binary(&["show", "nothing-here@example.com"], |route, out| {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{route}: a miss must exit non-zero");
        assert!(
            stderr.contains("nothing-here@example.com"),
            "{route}: {stderr}"
        );
        assert!(!stderr.contains("panicked"), "{route}: {stderr}");
    });

    // list_messages_lists_one_mailbox_of_one_account, order included
    let store = fixture::store(slice.root(), fixture::ACCOUNT);
    let rows = read::list_mailbox(&store, fixture::ACCOUNT, "inbox").expect("rows");
    let expected: Vec<String> = rows
        .iter()
        .map(|row| row.message_id.trim_matches(['<', '>']).to_string())
        .collect();
    drop(store);
    slice.for_each_binary(
        &["list-messages", "-A", "alpha", "--mailbox", "inbox"],
        |route, out| {
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "{route}: {out:?}");
            assert!(stdout.contains("Inbox (4 of 4):"), "{route}: {stdout}");
            assert!(
                !stdout.contains("Re: Bericht"),
                "{route}: the sent mailbox is not listed"
            );
            assert!(
                !stdout.contains("Only-In-Beta"),
                "{route}: another account is never listed"
            );
            assert!(
                stdout.contains("Shown: 4 | In the store: 4"),
                "{route}: {stdout}"
            );
            let printed: Vec<&str> = stdout
                .lines()
                .filter_map(|line| line.split("mp://alpha/inbox/").nth(1))
                .filter_map(|rest| rest.split(' ').next())
                .collect();
            assert_eq!(
                printed, expected,
                "{route}: the listing follows the store's order"
            );
        },
    );

    // list_messages_groups_every_mailbox_and_limits_per_group
    slice.for_each_binary(&["list-messages", "-n", "1"], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("Inbox (1 of 4):"), "{route}: {stdout}");
        assert!(
            stdout.contains("Sent (1 of 1):"),
            "{route}: the limit is per mailbox: {stdout}"
        );
        assert!(
            stdout.contains("Shown: 3 | In the store: 6"),
            "{route}: {stdout}"
        );
    });

    // an_unknown_mailbox_is_an_error_that_names_the_known_ones
    slice.for_each_binary(&["list-messages", "--mailbox", "nope"], |route, out| {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{route}");
        assert!(
            stderr.contains("not a mailbox of alpha"),
            "{route}: {stderr}"
        );
        assert!(stderr.contains("inbox"), "{route}: {stderr}");
    });

    // an_empty_mailbox_is_not_an_error
    slice.for_each_binary(
        &["list-messages", "-A", "beta", "--mailbox", "sent"],
        |route, out| {
            assert!(
                out.status.success(),
                "{route}: an empty mailbox is not an error: {out:?}"
            );
        },
    );
}

/// `tests/dump_mailbox_integration.rs`, run once in process and once routed:
/// the record shape, the determinism, the selectors, the flag axes and the
/// required `--json`.
#[test]
fn the_dump_contract_holds_on_both_routes() {
    let slice = Slice::start();

    // dump_mailbox_emits_the_recorded_envelope_shape: the record shape and the
    // absence of any filesystem path, over this fixture rather than the other
    // file's literal.
    let root = slice.root().display().to_string();
    slice.for_each_binary(&["dump-mailbox", "--json"], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout.lines().count(), 7, "{route}: every fixture message");
        assert!(
            !stdout.contains(&root),
            "{route}: a dump carries no filesystem path"
        );
        for line in stdout.lines() {
            let record: Value = serde_json::from_str(line).expect("a record is one JSON object");
            let mut keys: Vec<&str> = record
                .as_object()
                .expect("an object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, ENVELOPE_FIELDS, "{route}: {line}");
        }
        // The accounts come out in name order and the mailboxes with them.
        let accounts: Vec<String> = stdout
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("json")["account"].to_string())
            .collect();
        let mut sorted = accounts.clone();
        sorted.sort();
        assert_eq!(accounts, sorted, "{route}: records are grouped by account");
    });

    // dump_mailbox_is_deterministic_across_runs
    let first = slice.agreed_stdout(&["dump-mailbox", "--json"]);
    let second = slice.agreed_stdout(&["dump-mailbox", "--json"]);
    assert_eq!(first, second, "two runs over an unchanged store agree");

    // dump_mailbox_honours_account_and_mailbox_selectors
    slice.for_each_binary(&["-A", "beta", "dump-mailbox", "--json"], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout.lines().count(), 1, "{route}: {stdout}");
        assert!(stdout.contains(r#""account":"beta""#), "{route}: {stdout}");
    });
    slice.for_each_binary(
        &[
            "-A",
            "alpha",
            "dump-mailbox",
            "--json",
            "--mailbox",
            "INBOX",
        ],
        |route, out| {
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert_eq!(stdout.lines().count(), 4, "{route}: {stdout}");
            assert!(
                stdout.lines().all(|l| l.contains(r#""mailbox":"inbox""#)),
                "{route}: {stdout}"
            );
        },
    );
    slice.for_each_binary(
        &[
            "-A",
            "alpha",
            "dump-mailbox",
            "--json",
            "--mailbox",
            "sent",
            "--mailbox",
            "drafts",
        ],
        |route, out| {
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert_eq!(stdout.lines().count(), 1, "{route}: drafts have no rows");
        },
    );

    // dump_mailbox_reports_the_answered_and_forwarded_flags
    slice.for_each_binary(&["-A", "alpha", "dump-mailbox", "--json"], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout
            .lines()
            .find(|l| l.contains(fixture::BERICHT))
            .unwrap_or_else(|| panic!("{route}: the flagged message is dumped"));
        assert!(
            line.contains(r#""flags":["answered","flagged","forwarded","seen"]"#),
            "{route}: the dump carries all four axes, sorted: {line}"
        );
    });

    // dump_mailbox_requires_the_json_flag
    slice.for_each_binary(&["dump-mailbox"], |route, out| {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{route}");
        assert!(stderr.contains("--json"), "{route}: {stderr}");
    });
}

/// The CLI-visible half of `tests/store_search_integration.rs`, run once in
/// process and once routed. The library half stays in that file; the header of
/// this one says which assertions have no command to carry them.
#[test]
fn the_local_search_contract_holds_on_both_routes() {
    let slice = Slice::start();
    let markup_id = format!("inbox/{}", fixture::MARKUP);

    // a_subject_hit_outranks_a_body_hit, and the flat "best match first" order
    slice.for_each_binary(&["search", "--local", "ledger"], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{route}: {out:?}");
        let selectors: Vec<&str> = stdout
            .lines()
            .filter_map(|line| line.split("mp://alpha/").nth(1))
            .filter_map(|rest| rest.split(' ').next())
            .collect();
        assert_eq!(
            selectors.first().copied(),
            Some(markup_id.as_str()),
            "{route}: the subject hit comes first: {stdout}"
        );
        assert!(
            stdout.contains("Shown: 3 (best match first)"),
            "{route}: {stdout}"
        );
    });

    // the_limit_caps_the_ranked_hits
    slice.for_each_binary(&["search", "--local", "ledger", "-n", "1"], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("Shown: 1 (best match first)"),
            "{route}: {stdout}"
        );
        assert!(
            stdout.contains(fixture::MARKUP),
            "{route}: the best hit, not the first row"
        );
    });

    // search_spans_mailboxes_and_can_be_scoped_to_one
    slice.for_each_binary(
        &["search", "--local", "ledger", "--mailbox", "sent"],
        |route, out| {
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(stdout.contains(fixture::SENT), "{route}: {stdout}");
            assert!(
                !stdout.contains(fixture::MARKUP),
                "{route}: scoped out: {stdout}"
            );
        },
    );

    // unicode_terms_round_trip
    slice.for_each_binary(&["search", "--local", "anträge"], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(fixture::BERICHT),
            "{route}: case folding is the tokenizer's: {stdout}"
        );
    });

    // a_phrase_matches_adjacent_words_only, and a_trailing_star_matches_a_prefix
    slice.for_each_binary(
        &["search", "--local", "\"quarterly summary\""],
        |route, out| {
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(stdout.contains(fixture::MARKUP), "{route}: {stdout}");
        },
    );
    slice.for_each_binary(&["search", "--local", "quarter*"], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(fixture::MARKUP),
            "{route}: a prefix query matches: {stdout}"
        );
    });

    // an empty result is a sentence, not an error
    slice.for_each_binary(&["search", "--local", "aardvark"], |route, out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{route}: {out:?}");
        assert!(stdout.contains("No local matches for"), "{route}: {stdout}");
    });

    // punctuation_in_a_query_is_not_a_syntax_error: the refusal, which the
    // parity table also compares byte for byte.
    slice.for_each_binary(&["search", "--local", "  ??  "], |route, out| {
        assert!(
            !out.status.success(),
            "{route}: a query with nothing in it is refused"
        );
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("panicked"),
            "{route}: refused, not crashed"
        );
    });
}
