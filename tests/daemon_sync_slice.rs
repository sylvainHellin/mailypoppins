//! The sync/watch slice, moved onto the daemon (#0123, plan unit P4-U9).
//!
//! Four commands need a server today and must need the *daemon's* server
//! tomorrow without a byte moving: `mp sync [-n|--mailbox|--dry-run|
//! --all-accounts]`, `mp fetch`, `mp list-mailboxes` and `mp watch
//! [--mailbox|--timeout]`. Five methods carry them - `sync.quick`, `sync.full`
//! and `sync.watch` in a new family, `mailbox.list_server` beside
//! `mailbox.list`, and `message.list_server` beside the read slice's three -
//! plus a rendering that stays in the client, because a summary line, an exit
//! code and a `--timeout` are the client's business and not the daemon's.
//!
//! This file is a **contract test**. It is written before those methods exist,
//! against the shapes fixed here and in `docs/daemon-protocol.md`, and it
//! fails to compile against today's tree; that failure is the proof the
//! contract has no stub behind it. The implementer (P4-U10) does not edit this
//! file.
//!
//! # The surface under test
//!
//! ```text
//! sync.quick          {account, limit?, mailbox?: [..], dry_run?}
//!                         -> {operation_id}
//! sync.full           {account, mailbox?: [..], dry_run?}
//!                         -> {operation_id}
//! sync.watch          {account, mailbox?}          -> {operation_id}
//! mailbox.list_server {account}
//!                         -> {account, source, mailboxes: [{name, delimiter,
//!                             attributes, total, unread}]}
//! message.list_server {account, mailbox, limit, criteria?}
//!                         -> {account, mailbox, messages: [..]}
//! ```
//!
//! ## `sync.quick` and `sync.full` are operations, not commands
//!
//! `docs/daemon-protocol.md` reserves the `{operation_id}` answer to
//! [`MethodKind::Operation`]: a [`MethodKind::Command`] "changes state at once
//! and reports the revision it moved to", which is the opposite of what a sync
//! does. So both are `Operation`, `since: 1`, and both are
//! [`CancelScope::Durable`]: a sync a GUI started must be watchable - and must
//! keep running - from the CLI window beside it, which is the same sentence
//! the protocol uses to justify making an operation daemon-wide. A client that
//! wants it stopped calls `operation.cancel`, which is a decision rather than
//! a dropped socket.
//!
//! `sync.watch` is the exception and is [`CancelScope::ClientScoped`]: a watch
//! exists only to answer one client's question, and a `mp watch` the user
//! interrupted must not leave the daemon watching on its behalf.
//!
//! ## Two kinds, and which one `mp sync` uses
//!
//! [`TickKind::Quick`] is the newest UIDs per mailbox; [`TickKind::Full`] is
//! everything the mailbox lists. `mp sync -n N` is a quick pass with an
//! explicit bound, so **`mp sync` always calls `sync.quick`** and passes `-n`
//! as `limit`; `sync.full` has no CLI form and exists for the TUI's `sS` and
//! the GUI (SYN-01), which is why it takes no `limit`.
//!
//! ## `--all-accounts` is a loop in the client, and `all_accounts` is not on
//! the wire
//!
//! Chosen over one operation carrying per-account results, because parity
//! decides it. Today's `--all-accounts` walks `global_config.accounts` in
//! **configuration order**, prints a `── name ──` header before each one,
//! keeps going past a failure, and names the failures at the end; the headers,
//! the ordering, the "1 of 1 account(s) failed" denominator and the exit code
//! are all client-side rendering of a per-account result. One operation
//! returning a map would make the client re-derive that ordering from a shape
//! that no longer carries it, and would make `operation.cancel` all-or-nothing
//! where today a `^C` stops the account currently running. So: one
//! `sync.quick` per account, issued in configuration order, awaited before the
//! next is issued, and `account` is a required string parameter of both
//! methods with no `all_accounts` beside it.
//!
//! ## An account with nothing to sync is refused, not synced
//!
//! `alpha`, `beta` and `delta` configure no IMAP and no SMTP host, so
//! [`AccountConfig::is_local_only`] holds and `mp sync` prints
//! `- alpha: local-only, skipped` and exits 0. That decision moves into the
//! daemon with the rest of the account knowledge: `sync.quick` on such an
//! account is `-32006` `account_not_ready` with `data` of
//! `{account, state: "local_only"}`, and the client renders the skip line from
//! it. Issuing an operation id for work that will not happen would be a lie
//! the client then has to unpick, and `state` is where the account family
//! already says why an account is not ready.
//!
//! ## `mp fetch`, and why it is `message.list_server`
//!
//! `mp fetch` prints what the server has and writes nothing (#0037), so it is
//! a *query against a server*, which is exactly what `mailbox.list_server`
//! is for mailboxes. Naming it `sync.fetch` would put a method that ingests
//! nothing in the family whose whole subject is ingesting; naming it
//! `message.fetch` would sit one letter from `message.get` and mean something
//! else. It goes in its own array, [`MESSAGE_SERVER_METHOD_SPECS`], rather
//! than growing `MESSAGE_READ_METHOD_SPECS`, because that array is pinned at
//! three by `tests/daemon_read_slice.rs` and an implementer may not edit a T
//! unit's file. `--full` never crosses the socket: it selects which of the
//! fields the answer already carries the client prints.
//!
//! ## `mp watch` is narrowed to INBOX, and says so
//!
//! **The decision this unit had to take** (plan P4-U9), taken the way the plan
//! recommends: the narrowing is recorded, the on-demand client-scoped IDLE
//! connection is not built. The daemon's continuous watcher is INBOX-only, as
//! `imap_watch` is today, and a second connection class with its own teardown
//! semantics is not what a CLI flag with no known user should buy. P4-U10
//! records it in `BACKLOG.md`.
//!
//! Both sides of the socket carry the narrowing. The client rewrites the
//! mailbox to `INBOX` before it calls and prints [`NARROWING_WARNING`], one
//! line on stderr naming the mailbox it dropped and the `BACKLOG.md` entry;
//! the daemon accepts `mailbox` absent or `"INBOX"` and refuses anything else
//! with `-32602`, so a client that skipped the narrowing is told rather than
//! quietly watched the wrong mailbox.
//!
//! That warning is **the only deviation from the oracle in this file**, it is
//! sanctioned by the plan, and it is masked in exactly one parity row
//! ([`mp_watch_of_a_non_inbox_mailbox_warns_and_narrows`]); the rest of that
//! row, the refusal that follows and the exit code, is literal.
//!
//! `--timeout N` never crosses the socket either. The client starts the watch,
//! waits N seconds, calls `operation.cancel`, prints `ℹ Timed out.` and exits
//! **2** - the exit code plan section 3.0 records as "already taken by
//! `mp watch --timeout`". A daemon-side timer would be a second place that
//! knows about a client's patience.
//!
//! ## The drain lines, and the " (after sync)" label
//!
//! `mp sync` drains at both ends (#0114) and labels the tail's report lines so
//! they cannot be read as the head's. After the cutover the drains happen
//! inside the daemon's tick, so the label can only survive if the client
//! learns *which end* a report came from: [`Phase::as_str`] puts the five slot
//! names on the wire (`head_outbox`, `head_mutations`, `body`, `tail_outbox`,
//! `tail_mutations`), `operation.progress` carries one in its `phase`, and the
//! client turns a tail phase into [`TAIL_LABEL`]. The head drain's failure
//! stays non-fatal and stays a warning, [`drain_failed_line`], because a
//! queue that could not be drained must not turn a sync that worked into a
//! failed command.
//!
//! ## One vocabulary for two clients
//!
//! Every line `mp sync` prints about an outcome comes from `mp_client::format`
//! and from nothing else, so P4-U10 can make the TUI adopt the same strings
//! without a second copy of them drifting. `sync_cli_lines` already exists
//! (P3b-U6); this slice adds the three drain wordings and the label beside it,
//! and [`the_client_wordings_are_the_lines_mp_sync_prints`] pins all of them
//! against a synthetic [`SyncCompleted`] rather than against a run.
//!
//! # Parity, and what proves routing
//!
//! Every row is `fixture.mp_routed(args)` against `oracle(args, root)` over
//! the same seeded root: stdout, stderr and exit code, literally, with the two
//! exceptions named below - the narrowing warning, which is masked, and the
//! engine-lock row, which has no oracle. The routed
//! side runs under `MAILYPOPPINS_DAEMON_REQUIRE=1`, so a command that quietly
//! answered in process fails instead of passing for the daemon's work, and
//! [`no_sync_command_can_still_answer_without_a_daemon`] covers what that
//! variable cannot: with nothing listening and auto-start off, a command with
//! no in-process path left exits 4.
//!
//! Nothing in this slice writes anything, so no row needs a pristine restore:
//! `mp sync` over this fixture never gets far enough to ingest, `mp fetch` and
//! `mp list-mailboxes` write nothing by construction, and `mp watch` refuses.
//!
//! The one clap-level row, `--all-accounts` with `-A`, is compared through
//! `mp_routed` too even though it never reaches a socket: clap exits before
//! `main` reaches the require gate, which is what makes the comparison
//! meaningful rather than a tautology.
//!
//! # What this fixture cannot reach
//!
//! `rg -l 'fake_imap|FakeImap|MockImap|imap_server' tests src crates` finds
//! nothing, so **this repository has no fake IMAP server** and this file does
//! not invent one. What that puts out of reach, and where it is pinned
//! instead:
//!
//! - A sync that ingests. The operation is still pinned at the wire: an id is
//!   issued, `operation.status` answers about it by method and scope, and it
//!   settles `failed` carrying the refusal
//!   ([`a_sync_operation_is_issued_watched_and_settles_with_its_refusal`]).
//!   The rendering of a *successful* outcome is pinned as a pure function over
//!   a synthetic payload, and over a real `sync.completed` event forced
//!   through `MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME`
//!   ([`a_sync_completed_event_renders_through_the_shared_wordings`]).
//! - A server mailbox list and a server message list. Both are pinned by their
//!   refusals and by their result being path-free.
//! - `mp watch` reaching `✓ Mailbox changed.`, and `mp watch --timeout N`
//!   reaching exit 2. An IDLE that fires needs a server that speaks IMAP, and
//!   a timeout needs one that stays silent for N seconds; the flag set that
//!   carries both is pinned through the byte-identical `mp watch --help`.
//! - The `↻` drain report lines and the `⚠ mutations: drain failed:` line as
//!   emitted by a run. They are pinned as the client wordings that produce
//!   them, which is where P4-U10 has to put them anyway.
//!
//! A fake IMAP backend would unlock all of it and is recorded as a follow-up.
//!
//! # The legacy suites, twinned
//!
//! `tests/engine_lock_ingest_cli.rs` keeps running unchanged against the
//! in-process path. Its one test is re-run here, but **against the routed
//! binary alone**, and that is the one place in this file where the oracle is
//! not the definition of correctness: the engine lock on the ingest path is
//! #0122, which landed in Phase 3b, *after* the `pre-daemon` tag the oracle is
//! built from. Run against a held lock, the oracle connects anyway and prints
//! `✓ Synced: 0 email(s) ingested`; the tree under test prints the skip line.
//! A byte comparison there would pin the absence of a guard that shipped three
//! phases ago, so [`the_routed_sync_says_it_skipped_when_another_process_holds_the_lock`]
//! asserts the current contract on the routed side and says so in its own
//! doc comment.
//!
//! **What could not be twinned**, and why:
//!
//! - `engine_lock_ingest_cli::mp_sync_exits_zero_and_says_it_skipped_when_another_process_holds_the_lock`,
//!   as a *parity* row, for the reason above. Its assertions are re-run; its
//!   comparison against the oracle cannot be.
//! - `tests/engine_lock_ingest.rs` holds the rest of that phase's tests and
//!   spawns no process: they drive `run_sync_guarded` and `drain_guarded`
//!   directly, which is library behaviour every build ships and which no
//!   amount of routing changes.
//! - The sentinel-port half of the CLI test ("no IMAP session was opened") is
//!   not reproduced: it proves less once the connection would be the daemon's,
//!   and this fixture's account refuses on its credentials long before a
//!   socket, which is asserted directly instead.
//! - `tests/cli_help_snapshot.rs` covers `mp --help`. The four subcommand
//!   helps are compared here instead, because the flag sets they carry
//!   (`--all-accounts`, `--timeout`) are this slice's contract and the
//!   top-level snapshot never shows them.

mod support;

use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::format::{
    drain_failed_line, mutations_drain_line, outbox_drain_line, sync_cli_lines, sync_status_line,
    TAIL_LABEL,
};
use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::events::{Severity, SyncCompleted, KIND_SYNC_COMPLETED};
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::daemon::dispatch::{CancelScope, MethodKind};
use mailypoppins::daemon::methods::mailbox::MAILBOX_METHOD_SPECS;
use mailypoppins::daemon::methods::message::MESSAGE_SERVER_METHOD_SPECS;
use mailypoppins::daemon::methods::sync::SYNC_METHOD_SPECS;
use mailypoppins::daemon::runtime::account::Phase;
use mailypoppins::engine_lock::EngineLock;

use support::parity::{
    assert_byte_identical, mp_command, mp_no_daemon, oracle_command, socket_path, DaemonFixture,
    EXIT_UNAVAILABLE, REQUIRE_ENV,
};
use support::sync_fixture as fixture;

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// Upper bound on waiting for an operation to settle or an event to arrive.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// The JSON-RPC codes this file asserts on by number, because they are the
/// standard pair rather than daemon-range names.
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

/// The exit code clap uses for a usage error, and the code `mp watch` uses for
/// a timeout. Both are 2 and that is not a coincidence worth removing: plan
/// section 3.0 records 2 as taken and routes the daemon's own failures to 3
/// and 4 around it.
const EXIT_USAGE: i32 = 2;

/// The three methods of the new family, in the order their spec array declares
/// them, which is method-name order like every other family.
const SYNC_METHODS: [&str; 3] = ["sync.full", "sync.quick", "sync.watch"];

/// The `mailbox.*` family after this slice: the sidebar hierarchy it already
/// served, plus the server's own list.
const MAILBOX_METHODS: [&str; 2] = ["mailbox.list", "mailbox.list_server"];

/// The one server-side message query, in its own array beside the read
/// slice's three.
const MESSAGE_SERVER_METHODS: [&str; 1] = ["message.list_server"];

/// The five slot names a tick's phases travel under, in the order a tick
/// enters them.
const PHASE_NAMES: [(Phase, &str); 5] = [
    (Phase::HeadOutbox, "head_outbox"),
    (Phase::HeadMutations, "head_mutations"),
    (Phase::Body, "body"),
    (Phase::TailOutbox, "tail_outbox"),
    (Phase::TailMutations, "tail_mutations"),
];

/// Every family declares exactly those names, checked while the tree compiles:
/// an array that grew a method nobody wrote down fails here, naming the
/// constant, before a single test runs.
const _: () = assert!(SYNC_METHOD_SPECS.len() == SYNC_METHODS.len());
const _: () = assert!(MAILBOX_METHOD_SPECS.len() == MAILBOX_METHODS.len());
const _: () = assert!(MESSAGE_SERVER_METHOD_SPECS.len() == MESSAGE_SERVER_METHODS.len());

/// The one line this slice prints that the pre-daemon binary does not.
///
/// One line, on stderr, before the call, so it is visible even on a run that
/// then refuses for another reason - which is every run over this fixture. It
/// names the mailbox it dropped, the reason, and where the decision is
/// recorded, because a user who typed `--mailbox Archive` and got INBOX is
/// owed all three.
fn narrowing_warning(mailbox: &str) -> String {
    format!(
        "⚠ watching {inbox} instead of '{mailbox}': the daemon's watcher is {inbox}-only \
         (BACKLOG.md, `mp watch --mailbox` is narrowed to {inbox})",
        inbox = fixture::INBOX,
    )
}

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
    /// Seed the root, then start a daemon over it. In that order: the daemon
    /// loads `config.toml` once, at startup.
    fn start() -> Slice {
        Slice::start_with(&[])
    }

    /// The same, with `SERVER_ACCOUNT`'s engine lock already held by this
    /// process when the daemon starts (P5-U8).
    ///
    /// Before this unit the lock was taken *after* the daemon was up, because
    /// the daemon started no runtime and took no lock, so the only holder that
    /// mattered was this process. Now every account has a runtime and the
    /// daemon *is* an engine, so the lock has to be held before it starts or it
    /// will take it first and the row would be about a daemon syncing rather
    /// than about a daemon refused. Rust opens every file `O_CLOEXEC`, so the
    /// child inherits no copy of the description and the holder here is the
    /// only one.
    fn start_behind_a_held_lock() -> (Slice, EngineLock) {
        let tmp = TempDir::new().expect("a temporary sync-slice root");
        fixture::seed(tmp.path());
        let holder = EngineLock::try_acquire_at(
            &fixture::engine_lock_path(tmp.path(), fixture::SERVER_ACCOUNT),
            fixture::SERVER_ACCOUNT,
        )
        .expect("taking the fixture's engine lock")
        .expect("the test process holds the lock the daemon will be refused");
        let daemon = DaemonFixture::start_with(tmp.path(), None, &[]);
        (Slice { daemon, tmp }, holder)
    }

    /// The same, with hooks put back on top of the sandbox.
    fn start_with(env: &[(&str, &str)]) -> Slice {
        let tmp = TempDir::new().expect("a temporary sync-slice root");
        fixture::seed(tmp.path());
        let daemon = DaemonFixture::start_with(tmp.path(), None, env);
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
        oracle_command(self.root())
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("run oracle `mp {}`: {e}", args.join(" ")))
    }

    /// Assert the two binaries agree byte for byte, and hand the routed output
    /// back for any further assertion.
    fn both(&self, args: &[&str]) -> Output {
        let routed = self.routed(args);
        let direct = self.oracle(args);
        assert_byte_identical(&routed, &direct);
        routed
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

/// The `data` payload an error code fixes, which every code used here does.
fn data_of(error: &RpcError) -> &Value {
    error
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("code {} fixes a data payload", error.code))
}

/// The operation id an operation method answered with.
fn operation_id(result: &Value) -> String {
    let id = result["operation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("an operation answers with an operation_id: {result}"));
    assert!(!id.is_empty(), "an operation id is never empty");
    id.to_string()
}

/// Poll `operation.status` until the operation leaves `queued`/`running`, and
/// return the terminal status object.
async fn settle(conn: &mut Connection, id: &str) -> Value {
    let start = Instant::now();
    loop {
        let status = call(conn, "operation.status", json!({"operation_id": id})).await;
        let state = status["state"]
            .as_str()
            .unwrap_or_else(|| panic!("operation.status always carries a state: {status}"))
            .to_string();
        if matches!(state.as_str(), "succeeded" | "failed" | "cancelled") {
            return status;
        }
        assert!(
            start.elapsed() < SETTLE_DEADLINE,
            "operation {id} was still {state} after {SETTLE_DEADLINE:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Nothing this slice answers may name a file the client cannot open, or a
/// file it has no business knowing about.
fn assert_path_free(what: &str, value: &Value) {
    let text = value.to_string();
    for needle in ["store.sqlite3", "/blobs/", "/runtime/", "/accounts/"] {
        assert!(
            !text.contains(needle),
            "{what} leaked {needle:?} into its result: {text}"
        );
    }
}

/// stdout as text, which every command of this slice writes.
fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("this slice prints UTF-8")
}

/// stderr as text.
fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("this slice prints UTF-8")
}

/// Assert a failing run carried `sentence` as its `Error:` line.
fn assert_refused(out: &Output, sentence: &str) {
    assert_eq!(out.status.code(), Some(1), "a refusal exits 1: {out:?}");
    assert!(
        stderr(out).contains(&format!("Error: {sentence}")),
        "expected the refusal {sentence:?}, got:\n{}",
        stderr(out)
    );
}

/// A synthetic outcome with every counter at a value a formatter reacts to, so
/// one payload exercises every clause of both formatters.
fn loud_outcome() -> SyncCompleted {
    SyncCompleted {
        account: fixture::SERVER_ACCOUNT.to_string(),
        severity: Severity::Warning,
        saved: 3,
        skipped: 4,
        flags_updated: 5,
        pruned: 6,
        prunes_deferred: 7,
        uid_rebound: 8,
        uidvalidity_resets: 9,
        bodies_truncated: 2,
        non_converging: vec![fixture::INBOX.to_string()],
        failed_mutations: 1,
        error: None,
        // P5-U8: a tick carries what arrived, and this one arrived at nothing.
        new_inbox_mail: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// 1. The methods themselves
// ---------------------------------------------------------------------------

/// The three declarations, and the four facts each one fixes: the wire name,
/// the kind, the first protocol version and what a disconnect does to it.
#[test]
fn the_sync_family_declares_three_operations() {
    let names: Vec<&str> = SYNC_METHOD_SPECS.iter().map(|spec| spec.name).collect();
    assert_eq!(names, SYNC_METHODS, "the family serves exactly these three");

    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the array is in method-name order");

    for spec in SYNC_METHOD_SPECS {
        assert_eq!(
            spec.kind,
            MethodKind::Operation,
            "{} answers with an operation_id and works in the background",
            spec.name
        );
        assert_eq!(spec.since, 1, "{} is served from protocol 1", spec.name);
    }
}

/// A sync outlives the client that asked for it; a watch does not.
///
/// The asymmetry is the whole of the cancellation contract for this slice: a
/// GUI that started a sync and was closed must not leave half an ingest
/// behind, and a `mp watch` the user interrupted must not leave the daemon
/// watching for a process that is gone.
#[test]
fn a_sync_is_durable_and_a_watch_dies_with_its_client() {
    for spec in SYNC_METHOD_SPECS {
        let expected = if spec.name == "sync.watch" {
            CancelScope::ClientScoped
        } else {
            CancelScope::Durable
        };
        assert_eq!(
            spec.cancel_scope, expected,
            "{} declares the wrong cancellation scope",
            spec.name
        );
    }
}

/// `mailbox.list_server` joins the family it belongs to rather than starting
/// one: two names, still in method-name order, and both are reads.
#[test]
fn mailbox_list_server_is_the_second_method_of_the_mailbox_family() {
    let names: Vec<&str> = MAILBOX_METHOD_SPECS.iter().map(|spec| spec.name).collect();
    assert_eq!(names, MAILBOX_METHODS);

    for spec in MAILBOX_METHOD_SPECS {
        assert_eq!(
            spec.kind,
            MethodKind::Query,
            "{} reads and changes nothing",
            spec.name
        );
        assert_eq!(spec.since, 1);
        assert_eq!(
            spec.cancel_scope,
            CancelScope::Durable,
            "{} outlives its caller",
            spec.name
        );
    }
}

/// The one server-side message query, in its own array so the read slice's
/// three stay three.
#[test]
fn message_list_server_is_a_query_of_its_own() {
    let names: Vec<&str> = MESSAGE_SERVER_METHOD_SPECS
        .iter()
        .map(|spec| spec.name)
        .collect();
    assert_eq!(names, MESSAGE_SERVER_METHODS);

    let spec = MESSAGE_SERVER_METHOD_SPECS[0];
    assert_eq!(
        spec.kind,
        MethodKind::Query,
        "`mp fetch` writes nothing, which is what makes it a query"
    );
    assert_eq!(spec.since, 1);
    assert_eq!(spec.cancel_scope, CancelScope::Durable);
}

/// The five slots a tick enters have wire names, because the client's
/// " (after sync)" label is derived from them and from nothing else.
#[test]
fn every_tick_phase_has_a_wire_name() {
    for (phase, name) in PHASE_NAMES {
        assert_eq!(
            phase.as_str(),
            name,
            "{phase:?} travels as {name:?} in operation.progress"
        );
    }

    let names: Vec<&str> = PHASE_NAMES.iter().map(|(_, name)| *name).collect();
    let mut unique = names.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        names.len(),
        "two phases sharing a name would make the tail label unguessable"
    );
}

/// A method may not be served without being advertised: the handshake derives
/// the capability list from the dispatcher, so the names appearing there is
/// what says they are registered rather than merely declared.
#[tokio::test]
async fn the_daemon_advertises_the_sync_slice_methods() {
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

    for method in [
        "sync.quick",
        "sync.full",
        "sync.watch",
        "mailbox.list_server",
        "message.list_server",
    ] {
        assert!(
            result.capabilities.iter().any(|c| c == method),
            "{method} is served, so it is advertised: {:?}",
            result.capabilities
        );
    }
}

// ---------------------------------------------------------------------------
// 2. `sync.quick` and `sync.full`
// ---------------------------------------------------------------------------

/// Both methods name one account, and every way of naming none is the
/// caller's parameter being wrong.
///
/// `all_accounts` is deliberately not a parameter: the loop is the client's,
/// so a caller that sent one is disagreeing with the contract and is told so
/// rather than silently syncing one account.
#[tokio::test]
async fn a_sync_names_exactly_one_account() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for method in ["sync.quick", "sync.full"] {
        let missing = call_err(&mut conn, method, json!({})).await;
        assert_eq!(missing.code, INVALID_PARAMS);
        assert!(
            missing.message.contains("account"),
            "{method} names the parameter it wanted: {}",
            missing.message
        );

        let plural = call_err(
            &mut conn,
            method,
            json!({"account": fixture::SERVER_ACCOUNT, "all_accounts": true}),
        )
        .await;
        assert_eq!(
            plural.code, INVALID_PARAMS,
            "{method} has no all_accounts parameter; the loop is the client's"
        );

        let unknown = call_err(
            &mut conn,
            method,
            json!({"account": fixture::UNKNOWN_ACCOUNT}),
        )
        .await;
        assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
        assert_eq!(data_of(&unknown)["account"], fixture::UNKNOWN_ACCOUNT);
    }
}

/// An account with no server configured has nothing to sync, and that is a
/// refusal rather than an operation that does nothing.
///
/// `state` is `"local_only"`, which is the account family's own field for
/// "why this account is not ready"; the client turns it into
/// `- alpha: local-only, skipped` and keeps the run's exit code at 0.
#[tokio::test]
async fn a_local_only_account_is_refused_rather_than_synced() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for account in fixture::LOCAL_ONLY_ACCOUNTS {
        for method in ["sync.quick", "sync.full"] {
            let refused = call_err(&mut conn, method, json!({"account": account})).await;
            assert_eq!(
                refused.code,
                ErrorCode::AccountNotReady.code(),
                "{method} on {account}: a local-only account is configured but not syncable"
            );
            let data = data_of(&refused);
            assert_eq!(data["account"], account);
            assert_eq!(
                data["state"], "local_only",
                "{method} says why {account} is not ready"
            );
        }
    }
}

/// The operation half of the contract, as far as a fixture with no server
/// reaches: an id is issued at once, `operation.status` answers about it by
/// method and scope, and it settles `failed` carrying the refusal in the
/// command's own words.
///
/// Over this fixture the refusal is the missing credential, which arrives
/// before anything opens a socket. A run that reported a connection error
/// instead would mean the daemon reordered credential resolution behind the
/// connection, which is the ordering this assertion protects.
#[tokio::test]
async fn a_sync_operation_is_issued_watched_and_settles_with_its_refusal() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for method in ["sync.quick", "sync.full"] {
        let started = call(
            &mut conn,
            method,
            json!({"account": fixture::SERVER_ACCOUNT}),
        )
        .await;
        let id = operation_id(&started);
        assert_eq!(
            started.as_object().map(|o| o.len()),
            Some(1),
            "{method} answers with the id and nothing else: {started}"
        );

        let settled = settle(&mut conn, &id).await;
        assert_eq!(settled["method"], method, "the status names its method");
        assert_eq!(
            settled["scope"], "durable",
            "a sync outlives the client that asked for it"
        );
        assert_eq!(
            settled["state"], "failed",
            "no credentials, so the pass could not run: {settled}"
        );
        assert!(settled["result"].is_null(), "a failure produced nothing");
        assert_eq!(
            settled["error"]["code"], INTERNAL_ERROR,
            "a missing secret is neither a bad parameter nor an unknown account"
        );
        assert_eq!(
            settled["error"]["message"],
            fixture::secret_refusal(fixture::SERVER_ACCOUNT),
            "{method} fails in the sentence the user has to act on"
        );
        assert_path_free(method, &settled);
    }
}

/// A `--mailbox` no account configures is refused by name, with the
/// configured mailboxes listed, before anything opens a socket.
///
/// It is the *operation* that fails rather than the call, because the client
/// renders it as `✗ gamma: <sentence>` beside every other per-account failure
/// and a call-level refusal would need a second rendering path for one case.
#[tokio::test]
async fn an_unconfigured_mailbox_fails_the_operation_before_it_connects() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let started = call(
        &mut conn,
        "sync.quick",
        json!({
            "account": fixture::SERVER_ACCOUNT,
            "mailbox": [fixture::UNKNOWN_MAILBOX],
        }),
    )
    .await;
    let settled = settle(&mut conn, &operation_id(&started)).await;

    assert_eq!(settled["state"], "failed");
    assert_eq!(
        settled["error"]["code"], INVALID_PARAMS,
        "the caller named a mailbox that does not exist"
    );
    assert_eq!(
        settled["error"]["message"],
        fixture::unknown_mailbox_refusal(fixture::SERVER_ACCOUNT),
        "the refusal lists what the account does know"
    );
}

/// The flag set `mp sync` carries is accepted as parameters, and `limit`
/// belongs to the quick pass alone.
///
/// `sync.full` is "everything the mailbox lists", so a bound on it would be a
/// quick pass under another name; a caller that sent one is told rather than
/// quietly given a different pass.
#[tokio::test]
async fn the_sync_flag_set_crosses_the_socket() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    // `-n 0`, `--dry-run` and two `--mailbox`es: accepted, and the operation
    // gets as far as the credentials like any other.
    let started = call(
        &mut conn,
        "sync.quick",
        json!({
            "account": fixture::SERVER_ACCOUNT,
            "limit": 0,
            "dry_run": true,
            "mailbox": [fixture::INBOX, fixture::EXTRA_MAILBOX],
        }),
    )
    .await;
    let settled = settle(&mut conn, &operation_id(&started)).await;
    assert_eq!(settled["state"], "failed");
    assert_eq!(
        settled["error"]["message"],
        fixture::secret_refusal(fixture::SERVER_ACCOUNT)
    );

    let bounded_full = call_err(
        &mut conn,
        "sync.full",
        json!({"account": fixture::SERVER_ACCOUNT, "limit": 10}),
    )
    .await;
    assert_eq!(
        bounded_full.code, INVALID_PARAMS,
        "a bounded full pass is a quick pass; say so instead of running one"
    );

    let wrong_shape = call_err(
        &mut conn,
        "sync.quick",
        json!({"account": fixture::SERVER_ACCOUNT, "mailbox": fixture::INBOX}),
    )
    .await;
    assert_eq!(
        wrong_shape.code, INVALID_PARAMS,
        "`--mailbox` repeats, so `mailbox` is an array on the wire"
    );
}

/// An operation a client started can be stopped by any client, and the answer
/// is a fact rather than a promise.
#[tokio::test]
async fn a_running_sync_can_be_cancelled_by_another_connection() {
    let slice = Slice::start();
    let mut owner = slice.connect().await;
    let mut other = slice.connect().await;

    let started = call(
        &mut owner,
        "sync.quick",
        json!({"account": fixture::SERVER_ACCOUNT}),
    )
    .await;
    let id = operation_id(&started);

    // The operation may already have failed on the credentials, which is a
    // terminal state and a `-32602` for the cancel. Either answer is correct;
    // what may not happen is a cancel that leaves it running.
    let cancelled = within(
        "operation.cancel",
        other.call("operation.cancel", json!({"operation_id": id})),
    )
    .await;
    match cancelled {
        Ok(result) => assert_eq!(result["state"], "cancelled"),
        Err(ClientError::Rpc(error)) => {
            assert_eq!(error.code, INVALID_PARAMS);
            assert_eq!(data_of(&error)["operation_id"], id);
        }
        Err(other) => panic!("operation.cancel answered neither way: {other:?}"),
    }

    let settled = settle(&mut owner, &id).await;
    assert!(
        matches!(
            settled["state"].as_str(),
            Some("failed") | Some("cancelled")
        ),
        "the operation settled: {settled}"
    );
}

// ---------------------------------------------------------------------------
// 3. `mailbox.list_server` and `message.list_server`
// ---------------------------------------------------------------------------

/// The server mailbox list refuses what it cannot reach, in the words the user
/// reads, and never names a local file.
#[tokio::test]
async fn mailbox_list_server_refuses_what_it_cannot_reach() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let missing = call_err(&mut conn, "mailbox.list_server", json!({})).await;
    assert_eq!(missing.code, INVALID_PARAMS);

    let unknown = call_err(
        &mut conn,
        "mailbox.list_server",
        json!({"account": fixture::UNKNOWN_ACCOUNT}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
    assert_eq!(data_of(&unknown)["account"], fixture::UNKNOWN_ACCOUNT);

    // Both a configured server with no credentials and an account with no
    // server at all refuse with the same sentence, which is what `mp
    // list-mailboxes` prints for either.
    for account in [fixture::ACCOUNT, fixture::SERVER_ACCOUNT] {
        let refused = call_err(
            &mut conn,
            "mailbox.list_server",
            json!({"account": account}),
        )
        .await;
        assert_eq!(refused.code, INTERNAL_ERROR);
        assert_eq!(refused.message, fixture::secret_refusal(account));
        assert_path_free("mailbox.list_server", &json!(refused.data));
    }
}

/// The server message list is `mp fetch`'s method, and it refuses the same
/// way: the account, then the mailbox, then the credentials.
#[tokio::test]
async fn message_list_server_refuses_what_it_cannot_reach() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let missing = call_err(&mut conn, "message.list_server", json!({})).await;
    assert_eq!(missing.code, INVALID_PARAMS);

    let no_mailbox = call_err(
        &mut conn,
        "message.list_server",
        json!({"account": fixture::SERVER_ACCOUNT, "limit": 10}),
    )
    .await;
    assert_eq!(
        no_mailbox.code, INVALID_PARAMS,
        "`mp fetch --mailbox` defaults to INBOX in the client, so the wire always carries one"
    );

    let unknown = call_err(
        &mut conn,
        "message.list_server",
        json!({"account": fixture::UNKNOWN_ACCOUNT, "mailbox": fixture::INBOX, "limit": 10}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());

    let refused = call_err(
        &mut conn,
        "message.list_server",
        json!({
            "account": fixture::SERVER_ACCOUNT,
            "mailbox": fixture::INBOX,
            "limit": 10,
            "criteria": {"from": "someone@example.com", "subject": "Bericht"},
        }),
    )
    .await;
    assert_eq!(refused.code, INTERNAL_ERROR);
    assert_eq!(
        refused.message,
        fixture::secret_refusal(fixture::SERVER_ACCOUNT),
        "the criteria were accepted; what failed is the backend"
    );
}

// ---------------------------------------------------------------------------
// 4. `sync.watch`
// ---------------------------------------------------------------------------

/// A watch is validated before an id is issued, so `mp watch`'s refusal is the
/// call's error and the "Watching … for changes..." line is only ever printed
/// over a watch that started.
#[tokio::test]
async fn a_watch_is_validated_before_it_is_started() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let missing = call_err(&mut conn, "sync.watch", json!({})).await;
    assert_eq!(missing.code, INVALID_PARAMS);

    let unknown = call_err(
        &mut conn,
        "sync.watch",
        json!({"account": fixture::UNKNOWN_ACCOUNT}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());

    let refused = call_err(
        &mut conn,
        "sync.watch",
        json!({"account": fixture::SERVER_ACCOUNT}),
    )
    .await;
    assert_eq!(
        refused.code, INTERNAL_ERROR,
        "no credentials, refused before an operation existed"
    );
    assert_eq!(
        refused.message,
        fixture::secret_refusal(fixture::SERVER_ACCOUNT)
    );
}

/// The daemon watches INBOX and says so: `mailbox` absent or `"INBOX"` is
/// accepted, anything else is `-32602` naming the narrowing.
///
/// The client narrows before it calls, so this refusal is unreachable through
/// `mp watch`. It exists for the client that did not, which is every client
/// this daemon has not shipped yet.
#[tokio::test]
async fn a_watch_accepts_only_inbox() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    // Accepted: the refusal that follows is about the credentials, which means
    // the mailbox was not what stopped it.
    let inbox = call_err(
        &mut conn,
        "sync.watch",
        json!({"account": fixture::SERVER_ACCOUNT, "mailbox": fixture::INBOX}),
    )
    .await;
    assert_eq!(
        inbox.message,
        fixture::secret_refusal(fixture::SERVER_ACCOUNT)
    );

    let elsewhere = call_err(
        &mut conn,
        "sync.watch",
        json!({"account": fixture::SERVER_ACCOUNT, "mailbox": fixture::EXTRA_MAILBOX}),
    )
    .await;
    assert_eq!(elsewhere.code, INVALID_PARAMS);
    assert!(
        elsewhere.message.contains(fixture::INBOX),
        "the refusal names the only mailbox this build watches: {}",
        elsewhere.message
    );
    let data = data_of(&elsewhere);
    assert_eq!(data["account"], fixture::SERVER_ACCOUNT);
    assert_eq!(data["mailbox"], fixture::EXTRA_MAILBOX);
}

// ---------------------------------------------------------------------------
// 5. The shared wordings
// ---------------------------------------------------------------------------

/// Every line `mp sync` prints about an outcome is `mp_client::format`'s, so
/// the TUI can adopt the same strings in P4-U10 without a second copy of them
/// drifting.
///
/// The synthetic outcome sets every counter to a value its clause reacts to,
/// so this is the whole of the CLI vocabulary in one list, in the order
/// `sync_one_account` printed it.
#[test]
fn the_client_wordings_are_the_lines_mp_sync_prints() {
    let lines = sync_cli_lines(&loud_outcome());
    assert_eq!(
        lines,
        vec![
            "✓ Synced: 3 new, 4 already present".to_string(),
            "ℹ Status updated on 5 message(s)".to_string(),
            "ℹ Rebound 8 message(s) to new UIDs after a UIDVALIDITY reset".to_string(),
            "ℹ 6 message(s) left their mailbox on the server".to_string(),
            "⚠ 7 removal(s) held back: this pass did not see every message, run a full sync \
             to apply them"
                .to_string(),
            "ℹ 2 mailbox(es) stopped at the fetch deadline (resuming next sync)".to_string(),
            "⚠ 'INBOX' downloaded the same messages again: the fetch is not converging, see \
             the log and docs/tickets/0115-warn-on-a-non-converging-fetch.md"
                .to_string(),
            "⚠ 1 mutation(s) failed and were rolled back (see the log)".to_string(),
        ],
        "the CLI's whole outcome vocabulary"
    );

    // A tick that failed reports the failure and nothing else: counts from a
    // pass that did not finish would read as a sync that happened.
    let mut failed = loud_outcome();
    failed.severity = Severity::Error;
    failed.error = Some("connection reset".to_string());
    assert_eq!(sync_cli_lines(&failed), vec!["✗ gamma: connection reset"]);

    // The TUI's one-line form is the same facts, and it stays in the same
    // module so the two cannot drift apart unnoticed.
    assert!(
        sync_status_line(&loud_outcome()).starts_with("Synced: 3 new, 4 existing"),
        "the status line is built from the same payload"
    );
}

/// A non-convergence warning does not change an exit code.
///
/// The whole point of the detector (#0115) is that nothing failed and the work
/// is repeating anyway, so the run stays a success and the warning is the only
/// thing that says otherwise.
#[test]
fn a_non_convergence_warning_leaves_the_exit_code_at_zero() {
    let mut outcome = loud_outcome();
    outcome.failed_mutations = 0;
    let lines = sync_cli_lines(&outcome);
    assert!(
        lines.iter().any(|line| line.contains("not converging")),
        "the warning is printed: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.starts_with('✗')),
        "nothing failed: {lines:?}"
    );
    let nothing_failed: [String; 0] = [];
    assert_eq!(
        mailypoppins::sync_health::exit_code(&nothing_failed),
        0,
        "an account that warned is not an account that failed"
    );
}

/// The drain report lines, and the label that keeps the tail's apart from the
/// head's.
///
/// The label is the client's, derived from the phase the daemon reported, and
/// it is the reason [`Phase::as_str`] exists.
#[test]
fn the_drain_lines_carry_the_after_sync_label_on_the_tail_alone() {
    assert_eq!(TAIL_LABEL, " (after sync)");

    assert_eq!(
        outbox_drain_line(false, 2, 1),
        "  ↻ outbox: 2 completed, 1 still pending"
    );
    assert_eq!(
        outbox_drain_line(true, 2, 1),
        "  ↻ outbox (after sync): 2 completed, 1 still pending"
    );
    assert_eq!(
        mutations_drain_line(false, 3, 0),
        "  ↻ mutations: 3 completed, 0 failed"
    );
    assert_eq!(
        mutations_drain_line(true, 3, 0),
        "  ↻ mutations (after sync): 3 completed, 0 failed"
    );

    for line in [
        outbox_drain_line(true, 2, 1),
        mutations_drain_line(true, 3, 0),
    ] {
        assert!(
            line.contains(TAIL_LABEL),
            "a tail line carries the label: {line}"
        );
    }
}

/// A drain that could not run is loud and not fatal, and it carries no label:
/// only the head drain reports a failure this way, because the tail's runs
/// after the outcome is already decided.
#[test]
fn a_failed_head_drain_is_a_warning_and_not_a_failure() {
    let line = drain_failed_line("database is locked");
    assert_eq!(line, "  ⚠ mutations: drain failed: database is locked");
    assert!(
        !line.contains(TAIL_LABEL),
        "the head drain's failure is the head's: {line}"
    );
    assert!(
        !line.starts_with('✗'),
        "a queue that will be retried next tick did not fail the command: {line}"
    );
}

/// The outcome the client renders is the one the daemon published: a real
/// `sync.completed` event, deserialised into the payload type and handed to
/// the same formatter the CLI uses.
///
/// Phase 4 schedules no tick against a fixture with no server, so the outcome
/// is forced through `MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME`, exactly as
/// `tests/daemon_sync_outcome.rs` does. What is being proved is the path -
/// event on the wire, payload out of it, lines out of the payload - and not
/// the sync.
#[tokio::test]
async fn a_sync_completed_event_renders_through_the_shared_wordings() {
    let outcomes = json!([{
        "account": fixture::ACCOUNT,
        "severity": "ok",
        "saved": 3,
        "skipped": 4,
        "flags_updated": 0,
        "pruned": 0,
        "prunes_deferred": 0,
        "uid_rebound": 0,
        "uidvalidity_resets": 0,
        "bodies_truncated": 0,
        "non_converging": [],
        "failed_mutations": 0,
        "error": null,
    }]);
    let slice = Slice::start_with(&[(
        "MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME",
        &outcomes.to_string(),
    )]);
    let mut conn = slice.connect().await;
    call(&mut conn, "state.bootstrap", json!({})).await;

    let start = Instant::now();
    let payload = loop {
        let notification = within("state.event", conn.next_notification())
            .await
            .expect("the daemon keeps the stream open");
        if notification.params["kind"] == KIND_SYNC_COMPLETED {
            break notification.params["payload"].clone();
        }
        assert!(
            start.elapsed() < SETTLE_DEADLINE,
            "no sync.completed event arrived within {SETTLE_DEADLINE:?}"
        );
    };

    let outcome: SyncCompleted =
        serde_json::from_value(payload.clone()).expect("the payload is a SyncCompleted");
    assert_eq!(outcome.account, fixture::ACCOUNT);
    assert_eq!(
        sync_cli_lines(&outcome),
        vec!["✓ Synced: 3 new, 4 already present"],
        "the CLI's line comes out of the daemon's payload and nowhere else"
    );
    assert_path_free("sync.completed", &payload);
}

// ---------------------------------------------------------------------------
// 6. Parity: `mp sync`
// ---------------------------------------------------------------------------

/// An account with nothing to sync: one line, exit 0, and no summary claiming
/// a pass that never ran.
#[test]
fn mp_sync_skips_a_local_only_account_exactly_as_before() {
    let slice = Slice::start();
    slice.for_each_binary(&["sync"], |which, out| {
        assert_eq!(out.status.code(), Some(0), "{which}: a skip is a success");
        assert!(
            stdout(out).contains(&fixture::local_only_line(fixture::ACCOUNT)),
            "{which} must say why it did nothing:\n{}",
            stdout(out)
        );
        assert!(
            !stdout(out).contains("Synced:"),
            "{which} reported a pass it never ran:\n{}",
            stdout(out)
        );
    });
}

/// `--dry-run` and `-n` change nothing about a skipped account, which is the
/// cheapest proof that the flags reach the same body.
#[test]
fn mp_sync_flags_over_a_skipped_account_match_byte_for_byte() {
    let slice = Slice::start();
    for args in [
        vec!["sync", "--dry-run"],
        vec!["sync", "-n", "0"],
        vec!["sync", "-n", "1", "--dry-run"],
        vec!["sync", "-A", fixture::STORELESS_ACCOUNT],
    ] {
        slice.both(&args);
    }
}

/// The account that configures a server and has no credentials: the per-account
/// failure line, the failure summary and exit 1.
#[test]
fn mp_sync_reports_a_missing_credential_exactly_as_before() {
    let slice = Slice::start();
    slice.for_each_binary(&["sync", "-A", fixture::SERVER_ACCOUNT], |which, out| {
        assert_eq!(
            out.status.code(),
            Some(1),
            "{which}: the one account failed"
        );
        let err = stderr(out);
        assert!(
            err.contains(&format!(
                "✗ {}: {}",
                fixture::SERVER_ACCOUNT,
                fixture::secret_refusal(fixture::SERVER_ACCOUNT)
            )),
            "{which} names the account and the sentence:\n{err}"
        );
        assert!(
            err.contains(&fixture::failure_summary(fixture::SERVER_ACCOUNT)),
            "{which} names the failure at the end:\n{err}"
        );
    });
}

/// A `--mailbox` nothing configures is refused by name, with the account's own
/// mailboxes listed, and the run never reaches a server.
#[test]
fn mp_sync_refuses_an_unconfigured_mailbox_exactly_as_before() {
    let slice = Slice::start();
    let args = [
        "sync",
        "-A",
        fixture::SERVER_ACCOUNT,
        "--mailbox",
        fixture::UNKNOWN_MAILBOX,
    ];
    slice.for_each_binary(&args, |which, out| {
        assert_eq!(out.status.code(), Some(1), "{which}");
        assert!(
            stderr(out).contains(&fixture::unknown_mailbox_refusal(fixture::SERVER_ACCOUNT)),
            "{which} lists what the account does know:\n{}",
            stderr(out)
        );
    });
}

/// `--all-accounts`: a header per account in configuration order, the skips,
/// the one failure, and a denominator that counts only what was attempted.
#[test]
fn mp_sync_all_accounts_walks_them_in_configuration_order() {
    let slice = Slice::start();
    slice.for_each_binary(&["sync", "--all-accounts"], |which, out| {
        assert_eq!(out.status.code(), Some(1), "{which}: gamma failed");

        let text = stdout(out);
        let mut at = 0usize;
        for account in fixture::ALL_ACCOUNTS {
            let header = fixture::account_header(account);
            let found = text[at..]
                .find(&header)
                .unwrap_or_else(|| panic!("{which}: no header for {account}:\n{text}"));
            at += found + header.len();
        }

        for account in fixture::LOCAL_ONLY_ACCOUNTS {
            assert!(
                text.contains(&fixture::local_only_line(account)),
                "{which}: {account} was not skipped:\n{text}"
            );
        }
        assert!(
            stderr(out).contains(&fixture::failure_summary(fixture::SERVER_ACCOUNT)),
            "{which}: three skipped accounts are out of the denominator:\n{}",
            stderr(out)
        );
    });
}

/// `-A` naming an account the configuration does not carry leaves nothing to
/// sync, and the command says so rather than syncing the default.
#[test]
fn mp_sync_of_an_unknown_account_refuses_exactly_as_before() {
    let slice = Slice::start();
    slice.for_each_binary(&["sync", "-A", fixture::UNKNOWN_ACCOUNT], |which, out| {
        assert_eq!(out.status.code(), Some(1), "{which}");
        assert!(
            stderr(out).contains(fixture::NO_ACCOUNT_TO_SYNC),
            "{which}:\n{}",
            stderr(out)
        );
    });
}

/// `--all-accounts` and `-A` answer the same question, so clap refuses the
/// pair rather than letting a cron line sync accounts it never named (ANO-9).
///
/// Compared through the routed client although it never reaches a socket:
/// clap exits before `main` reaches the require gate, which is what makes this
/// a comparison of two binaries rather than a tautology.
#[test]
fn all_accounts_and_a_named_account_conflict_with_identical_usage_bytes() {
    let slice = Slice::start();
    let out = slice.both(&["sync", "--all-accounts", "-A", fixture::ACCOUNT]);

    assert_eq!(
        out.status.code(),
        Some(EXIT_USAGE),
        "a usage error is clap's exit 2"
    );
    let err = stderr(&out);
    assert!(
        err.contains("cannot be used with"),
        "clap names the conflict:\n{err}"
    );
    assert!(err.contains("Usage: mp sync"), "and prints a usage:\n{err}");
}

/// The four subcommand helps, byte for byte.
///
/// This is where `--timeout`, `--all-accounts`, `-n` and `--dry-run` are
/// pinned as a flag set: `tests/cli_help_snapshot.rs` covers `mp --help`,
/// which never shows a subcommand's arguments, and three of this slice's
/// commands cannot be run to completion offline.
#[test]
fn the_subcommand_helps_are_byte_identical() {
    let slice = Slice::start();
    for command in ["sync", "fetch", "list-mailboxes", "watch"] {
        let out = slice.both(&[command, "--help"]);
        assert_eq!(out.status.code(), Some(0), "`mp {command} --help`");
    }
}

// ---------------------------------------------------------------------------
// 7. Parity: `mp list-mailboxes`, `mp fetch`, `mp watch`
// ---------------------------------------------------------------------------

/// `mp list-mailboxes` needs a server it cannot reach, and refuses in the
/// secret store's own sentence.
#[test]
fn mp_list_mailboxes_refuses_exactly_as_before() {
    let slice = Slice::start();
    for account in [fixture::ACCOUNT, fixture::SERVER_ACCOUNT] {
        slice.for_each_binary(&["list-mailboxes", "-A", account], |which, out| {
            assert_refused(out, &fixture::secret_refusal(account));
            assert!(
                stdout(out).is_empty(),
                "{which} printed a listing it never got:\n{}",
                stdout(out)
            );
        });
    }
}

/// `mp fetch` refuses the same way, with and without its filters, and writes
/// nothing either way.
#[test]
fn mp_fetch_refuses_exactly_as_before() {
    let slice = Slice::start();
    for args in [
        vec!["fetch", "-A", fixture::SERVER_ACCOUNT],
        vec![
            "fetch",
            "-A",
            fixture::SERVER_ACCOUNT,
            "--from",
            "someone@example.com",
            "--subject",
            "Bericht",
            "-n",
            "3",
            "--full",
        ],
        vec![
            "fetch",
            "-A",
            fixture::SERVER_ACCOUNT,
            "--mailbox",
            fixture::EXTRA_MAILBOX,
        ],
    ] {
        let out = slice.both(&args);
        assert_refused(&out, &fixture::secret_refusal(fixture::SERVER_ACCOUNT));
    }
}

/// `mp watch` over INBOX, explicitly and by default: the same refusal, and
/// nothing on stdout, because the "Watching …" line belongs to a watch that
/// started.
#[test]
fn mp_watch_of_inbox_refuses_exactly_as_before() {
    let slice = Slice::start();
    for args in [
        vec!["watch", "-A", fixture::SERVER_ACCOUNT],
        vec!["watch", "-A", fixture::SERVER_ACCOUNT, "--timeout", "1"],
        vec![
            "watch",
            "-A",
            fixture::SERVER_ACCOUNT,
            "--mailbox",
            fixture::INBOX,
        ],
    ] {
        let out = slice.both(&args);
        assert_refused(&out, &fixture::secret_refusal(fixture::SERVER_ACCOUNT));
        assert!(stdout(&out).is_empty(), "no watch started, so no line");
    }
}

/// The one sanctioned deviation: `--mailbox` naming anything but INBOX is
/// narrowed, and the narrowing is announced before the call rather than
/// discovered afterwards.
///
/// The warning is the only difference from the oracle, so it is the only thing
/// masked: with that one line removed the two runs are byte-identical again,
/// refusal and exit code included.
#[test]
fn mp_watch_of_a_non_inbox_mailbox_warns_and_narrows() {
    let slice = Slice::start();
    let args = [
        "watch",
        "-A",
        fixture::SERVER_ACCOUNT,
        "--mailbox",
        fixture::EXTRA_MAILBOX,
    ];
    let routed = slice.routed(&args);
    let direct = slice.oracle(&args);

    let warning = narrowing_warning(fixture::EXTRA_MAILBOX);
    let err = stderr(&routed);
    assert!(
        err.lines().any(|line| line == warning),
        "the narrowing is one line, exactly:\n{err}"
    );

    let masked: Vec<u8> = err
        .lines()
        .filter(|line| *line != warning)
        .map(|line| format!("{line}\n"))
        .collect::<String>()
        .into_bytes();
    let unmasked = Output {
        status: routed.status,
        stdout: routed.stdout.clone(),
        stderr: masked,
    };
    let trimmed = Output {
        status: direct.status,
        stdout: direct.stdout.clone(),
        stderr: stderr(&direct)
            .lines()
            .map(|line| format!("{line}\n"))
            .collect::<String>()
            .into_bytes(),
    };
    assert_byte_identical(&unmasked, &trimmed);
    assert_refused(&routed, &fixture::secret_refusal(fixture::SERVER_ACCOUNT));
}

// ---------------------------------------------------------------------------
// 8. Determinism, and what proves routing
// ---------------------------------------------------------------------------

/// Two routed runs of the same command produce the same bytes.
///
/// Nothing in this slice mints an id or a path that reaches the user, so every
/// row belongs here, the narrowing warning included.
#[test]
fn two_routed_runs_agree() {
    let slice = Slice::start();
    for args in [
        vec!["sync"],
        vec!["sync", "--all-accounts"],
        vec!["sync", "-A", fixture::SERVER_ACCOUNT],
        vec!["list-mailboxes", "-A", fixture::SERVER_ACCOUNT],
        vec!["fetch", "-A", fixture::SERVER_ACCOUNT],
        vec!["watch", "-A", fixture::SERVER_ACCOUNT, "--timeout", "1"],
        vec![
            "watch",
            "-A",
            fixture::SERVER_ACCOUNT,
            "--mailbox",
            fixture::EXTRA_MAILBOX,
        ],
    ] {
        let first = slice.routed(&args);
        let second = slice.routed(&args);
        assert_byte_identical(&first, &second);
    }
}

/// With nothing listening and auto-start off, no command of this slice can
/// answer at all: it exits 4, the code plan section 3.0 fixes for "daemon
/// unavailable".
///
/// This is the proof `MAILYPOPPINS_DAEMON_REQUIRE` cannot give: that variable
/// is checked at the end of `main`, so a command that returned early would
/// escape it, while a command with no in-process path left cannot produce an
/// answer without a socket.
#[test]
fn no_sync_command_can_still_answer_without_a_daemon() {
    let tmp = TempDir::new().expect("a temporary root");
    fixture::seed(tmp.path());

    for args in [
        vec!["sync"],
        vec!["sync", "--all-accounts"],
        vec!["fetch"],
        vec!["list-mailboxes"],
        vec!["watch", "--timeout", "1"],
    ] {
        let out = mp_no_daemon(&args, tmp.path());
        assert_eq!(
            out.status.code(),
            Some(EXIT_UNAVAILABLE),
            "`mp {}` answered without a daemon:\nstdout: {}\nstderr: {}",
            args.join(" "),
            stdout(&out),
            stderr(&out)
        );
    }
}

// ---------------------------------------------------------------------------
// 9. The legacy suite, twinned
// ---------------------------------------------------------------------------

/// `engine_lock_ingest_cli::mp_sync_exits_zero_and_says_it_skipped_when_another_process_holds_the_lock`,
/// re-run against the routed binary: a `mp sync` whose account is already
/// somebody else's engine exits 0, says why it did nothing, and reports no
/// pass.
///
/// **Routed side only, deliberately.** The guard is #0122 and the oracle is
/// `pre-daemon`, which predates it: given the same held lock the oracle
/// connects anyway and prints `✓ Synced: 0 email(s) ingested`. Comparing the
/// two would pin the absence of a guard that has shipped since, so this row
/// asserts the contract instead of the diff. It is the only row of this file
/// that does.
///
/// This is also the one *success* path of `mp sync` that needs no server, and
/// the only row that needs credentials: without them the run would refuse
/// before it ever looked at the lock. A host whose machine id cannot be read
/// has no encrypted secrets file and skips, exactly as
/// `tests/secrets_integration.rs` does.
///
/// The lock is taken *before* the daemon is started (P5-U8): the daemon starts
/// a runtime per account now and would otherwise be the holder itself. See
/// [`Slice::start_behind_a_held_lock`].
#[test]
fn the_routed_sync_says_it_skipped_when_another_process_holds_the_lock() {
    let (slice, _holder) = Slice::start_behind_a_held_lock();
    if !fixture::seed_secrets(slice.root()) {
        return;
    }

    let out = slice.routed(&["sync", "-A", fixture::SERVER_ACCOUNT]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a refused sync is a success, not a failure\nstdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stdout(&out).contains(&fixture::engine_busy_line(fixture::SERVER_ACCOUNT)),
        "the run must say why it did nothing:\n{}",
        stdout(&out)
    );
    assert!(
        !stdout(&out).contains("Synced:"),
        "a skipped sync must not report a pass it never ran:\n{}",
        stdout(&out)
    );
}

/// The same refusal seen from the wire: the operation succeeds, because the
/// holder is doing the work, and its result says the runtime was blocked so
/// the client can print the skip line instead of a summary.
#[tokio::test]
async fn a_blocked_account_settles_as_a_success_that_did_nothing() {
    let (slice, _holder) = Slice::start_behind_a_held_lock();
    if !fixture::seed_secrets(slice.root()) {
        return;
    }

    let mut conn = slice.connect().await;
    let started = call(
        &mut conn,
        "sync.quick",
        json!({"account": fixture::SERVER_ACCOUNT}),
    )
    .await;
    let settled = settle(&mut conn, &operation_id(&started)).await;

    assert_eq!(
        settled["state"], "succeeded",
        "the holder is doing the work, which is not this operation's failure: {settled}"
    );
    assert_eq!(
        settled["result"]["blocked"], true,
        "the result says nothing ran, so the client prints the skip line"
    );
    assert!(
        settled["result"]["outcome"].is_null(),
        "a pass that never ran produced no outcome: {settled}"
    );
    assert_path_free("sync.quick", &settled);
}

/// A run under the require gate that finds no daemon is not a run that
/// silently answered: the gate and the unavailable path agree.
///
/// Kept beside the twin because it is the same question from the other side -
/// the twin proves the routed binary produces the pre-daemon bytes, this
/// proves it could not have produced them from its own process.
#[test]
fn the_require_gate_and_the_unavailable_exit_agree() {
    let tmp = TempDir::new().expect("a temporary root");
    fixture::seed(tmp.path());

    let out = mp_command(tmp.path())
        .env(REQUIRE_ENV, "1")
        .env("MAILYPOPPINS_DAEMON_AUTOSTART", "0")
        .args(["sync"])
        .output()
        .expect("run `mp sync` with no daemon and the require gate on");
    assert_eq!(
        out.status.code(),
        Some(EXIT_UNAVAILABLE),
        "stdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
}
