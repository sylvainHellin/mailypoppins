//! The send slice, moved onto the daemon (#0123, plan unit P4-U11).
//!
//! Four commands put a message on the wire today and must put it there through
//! the *daemon* tomorrow without a byte moving: `mp send [-y]`,
//! `mp send --invite …`, `mp send-approved [-y] [--all-accounts]` and
//! `mp outbox list|retry|discard`. Six methods carry them, all in the `send.*`
//! family, plus the `operation.*` methods that already exist, plus a rendering
//! that stays in the client - because a preview, a `[y/N]` prompt, a per-row
//! listing and an exit code are the client's business and not the daemon's.
//!
//! This file is a **contract test**. It is written before those methods exist,
//! against the shapes fixed here and in `docs/daemon-protocol.md`, and it
//! fails to compile against today's tree; that failure is the proof the
//! contract has no stub behind it. The implementer (P4-U12) does not edit this
//! file.
//!
//! # The surface under test
//!
//! ```text
//! send.approved        {account}
//!                          -> {operation_id}
//! send.draft           {account, selector}
//!                          -> {operation_id}
//! send.invite          {account, to?, cc?, subject, start,
//!                       end?, duration?, location?, description?}
//!                          -> {operation_id}
//! send.outbox_discard  {account, row_id}
//!                          -> {discarded, row_id, message_id, revision}
//! send.outbox_list     {account}
//!                          -> {account, ever_used, rows: [..], counts: {..}}
//! send.outbox_retry    {account, row_id}
//!                          -> {operation_id}
//! ```
//!
//! Every one of the six is `since: 1` and [`CancelScope::Durable`]. A send is
//! the one thing in this program that cannot be undone by closing a window: a
//! client that disconnected while its message was in the SMTP conversation
//! must not have that conversation abandoned, because an abandoned submission
//! is precisely the ambiguous state `outbox::sweep_pending_sends` has to park
//! for a human. So nothing here is client-scoped, and a client that wants a
//! send stopped calls `operation.cancel`, which is a decision rather than a
//! dropped socket.
//!
//! ## Why `send.outbox_*` and not an `outbox.*` family
//!
//! Plan section 3.0 fixes the method families - `state.*`, `account.*`,
//! `mailbox.*`, `message.*`, `draft.*`, `send.*`, `sync.*`, `contact.*`,
//! `calendar.*`, `signature.*`, `config.*`, `operation.*`, `diagnostic.*`,
//! `daemon.*` - and `outbox` is not among them. `docs/parity-matrix.md`
//! SND-07 already writes the three names down as `send.outbox_list`,
//! `send.outbox_retry` and `send.outbox_discard`. Two committed documents
//! agree, so this file follows them rather than opening a fourteenth family
//! for three methods whose whole subject is what a send left behind.
//!
//! ## Why `send.invite` is its own method
//!
//! `mp send --invite` takes no draft. It builds a `VEVENT` from flags, wraps
//! it in an iMIP MIME tree and submits it through the outbox; there is no
//! selector to name and nothing on disk afterwards. Folding it into
//! `send.draft` as an optional `invite` object would make `selector` optional
//! in a method whose entire subject is a draft, and would put the `ANO-4`
//! Graph refusal - which is about the *invitation*, not about the draft - on a
//! method that has to serve Graph drafts perfectly well. `SND-05` already
//! names `send.invite`.
//!
//! ## Kinds, and the one that is not obvious
//!
//! - `send.outbox_list` is a [`MethodKind::Query`]: it reads rows and changes
//!   nothing.
//! - `send.outbox_discard` is a [`MethodKind::Command`]: one committed
//!   transaction against the local store, no network, so it "changes state at
//!   once and reports the revision it moved to", which is the protocol's own
//!   definition.
//! - `send.outbox_retry` is a [`MethodKind::Operation`], **and this is the
//!   choice this unit had to make**. It looks like a command - it re-arms one
//!   row - but `cmd_outbox` re-arms the row and then calls
//!   `send::resume_outbox`, which submits over SMTP and APPENDs to the Sent
//!   mailbox. That is an unbounded network round trip whose progress a GUI
//!   wants to watch and whose result the CLI prints two lines from, which is
//!   an operation by every criterion the protocol gives. A command that took
//!   thirty seconds and reported one revision would be a lie about what it
//!   did.
//! - `send.draft`, `send.invite` and `send.approved` are operations for the
//!   same reason, and `send.approved` additionally reports one
//!   `operation.progress` per draft so a batch of nine is visible while it
//!   runs.
//!
//! ## `--all-accounts` is a loop in the client
//!
//! Exactly as `mp sync --all-accounts` is (P4-U9), and for the same reason:
//! today's loop walks `global_config.accounts` in **configuration order**,
//! prints a `Summary <account>: …` line per account and keeps going past a
//! failure. `account` is therefore a required string parameter of
//! `send.approved` with no `all_accounts` beside it, and a caller that sends
//! one is told rather than quietly served one account.
//!
//! ## The hold is not here
//!
//! `SND-04`'s undo-send countdown lives inside the TUI process today and
//! `mp send` and `mp send-approved` bypass it (`ANO-7`). Phase 6 moves it into
//! the daemon and *keeps* the CLI bypassing it. So this slice's methods take
//! no `hold` and no `countdown` parameter - a caller that sends one is
//! refused, [`the_cli_send_paths_take_no_hold`] - and a routed `mp send -y`
//! finishes without waiting, which is asserted as elapsed time in
//! [`a_routed_send_delivers_without_waiting_out_a_hold`] rather than left to
//! be discovered when someone's script hangs for twenty seconds.
//!
//! ## Results are path-free
//!
//! Nothing this family answers may name `store.sqlite3`, a blob path, a
//! runtime path or an account directory: a Sent copy lives on a server and a
//! queued message lives in a blob the client has no business opening. The
//! drafts *path* travels only where the oracle prints something derived from
//! it - `mp send-approved`'s listing prints a file **name** - and then it is
//! rendered client-side from a `path` field, exactly as the draft slice does.
//!
//! # The `-y` prompt stays in the client
//!
//! `prompt_confirmation` reads stdin. A daemon has no stdin, and a daemon that
//! asked a question would have to invent a way to be answered. So the client
//! previews, prompts, and only then calls: a run without `-y` whose stdin is
//! not a terminal prints the prompt, reads EOF, prints [`CANCELLED`] and exits
//! **0** without a single `send.*` call. Every fixture run here has
//! `Stdio::null()` for stdin, which is what makes that reproducible, and the
//! parity rows prove the routed binary produces the oracle's bytes for it -
//! including the preview above it, which the client renders from the draft
//! family's existing queries.
//!
//! # Parity, and what proves routing
//!
//! Every parity row is `fixture.mp_routed(args)` against `oracle(args, root)`
//! over the same seeded root: stdout, stderr and exit code, literally. The
//! routed side runs under `MAILYPOPPINS_DAEMON_REQUIRE=1`, so a command that
//! quietly answered in process fails instead of passing for the daemon's work,
//! and [`no_send_command_can_still_answer_without_a_daemon`] covers what that
//! variable cannot: with nothing listening and auto-start off, a command with
//! no in-process path left exits 4.
//!
//! **Two masks, and nothing else.** A send mints a `Message-ID` and an
//! invitation mints a `UID`; neither is reproducible and both reach exactly
//! one line of output. [`mask_uid`] removes the invite preview's `  UID: …`
//! line, in the one row that prints it. No other row masks anything, and no
//! timestamp is masked at all, because [`send_fixture::FIXED_UPDATED`]
//! backdates every seeded row so `mp outbox list`'s time column is a literal.
//!
//! **Mutating rows restore first.** Half of this slice writes. A parity
//! comparison of a writing command cannot run the two binaries one after the
//! other over one root - the second would see the first one's work - so
//! [`Slice::both_mutating`] takes a [`send_fixture::Pristine`] copy of the
//! `accounts/` tree, runs the routed binary, stops the daemon, restores,
//! runs the oracle, compares both the bytes and the resulting state, restores
//! again and starts the daemon back up. The daemon is stopped around the
//! restore deliberately: it holds the store open in WAL mode and replacing
//! `store.sqlite3` under an open connection is undefined.
//!
//! # What this fixture cannot reach, and the hook that fixes it
//!
//! `rg -n 'smtp|TcpListener|MockSmtp' tests/*.rs tests/support/*.rs` finds no
//! server. `tests/outbox_integration.rs` fakes the *Sent mailbox* behind the
//! `SentMailbox` trait and drives `outbox::drain_guarded_at` in process;
//! `tests/imip_integration.rs` never sends at all - it parses RFC822 and
//! ingests. So there is no SMTP fake in this repository to reuse, and
//! `send::build_smtp_transport` is TLS-only on both branches, which a
//! plaintext `TcpListener` cannot serve.
//!
//! Rather than leave every success path unpinned, this slice fixes a
//! daemon-side hook, [`send::FAKE_TRANSPORT_ENV`], in the shape of the
//! `MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME` hook `tests/daemon_sync_outcome.rs`
//! already uses: it serves the SMTP submission and the Sent-mailbox APPEND in
//! process and writes one line per transport event to a log the test reads.
//! That makes a successful routed send, the partly-delivered outcome
//! (`SND-08`), the sent-copy append (`SND-09`) and the racing-drain twin
//! non-vacuous.
//!
//! The hook is the daemon's, so the pre-daemon oracle cannot see it: **every
//! row that uses it is a routed-side assertion, not a parity row**, and each
//! one says so in its own doc comment. That is the same licence
//! `tests/daemon_sync_slice.rs` takes for its engine-lock row.
//!
//! What stays out of reach even with the hook:
//!
//! - A real SMTP conversation, its error strings and its timeouts. Nothing
//!   here asserts on a network error's text.
//! - The Graph *send* path (#0036, blocked on #0035). Only its refusals and
//!   its preview shape are pinned.
//!
//! # The legacy suites, twinned
//!
//! `tests/outbox_integration.rs` and `tests/imip_integration.rs` keep running
//! unchanged. Neither spawns a process and neither has ever gone through the
//! CLI: the first drives `outbox::enqueue`, `record_submission` and
//! `drain_guarded_at` directly, the second drives `parse_rfc822_to_fetched_email`
//! and `ingest_message`. Routing changes nothing about library calls, so
//! "rerun them through the harness" means, for these two, re-running the
//! *scenarios* that have a CLI shape:
//!
//! - `two_racing_drains_append_each_row_exactly_once` is twinned as
//!   [`two_racing_routed_drains_append_each_row_exactly_once`]: two concurrent
//!   `send.outbox_retry` operations against one account, one of them parked
//!   inside its APPEND, and the fake transport's log must hold exactly one
//!   `append` per message-id.
//! - `a_drain_killed_mid_append_leaves_the_row_reclaimable_and_deduped`
//!   **is kept direct, deliberately**. It kills a drain *inside* its APPEND by
//!   dropping the future, which through the daemon would mean killing the
//!   daemon and losing every other row's state with it - the scenario would
//!   stop being about the row. Its reachable half, the dedup that the ambiguity
//!   arms, is twinned instead as
//!   [`a_routed_retry_after_a_swallowed_ack_dedupes_rather_than_duplicating`],
//!   through the hook's `"append": "swallow_ack"` mode.
//! - The rest of `outbox_integration.rs` - the crash windows, the backoff, the
//!   blob accounting, the envelope encoding - is library behaviour every build
//!   ships and is not re-run here.
//! - `imip_integration.rs` is a *receive* suite. The send slice's iMIP half is
//!   `mp send --invite`, whose deterministic surface is its refusals and its
//!   preview, both pinned below; `ANO-4` is
//!   [`mp_send_invite_refuses_a_graph_account_exactly_as_before`].

mod support;

use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::format::{
    outbox_cli_lines, outbox_discard_line, outbox_retry_lines, send_approved_line,
    send_approved_summary, send_cli_lines, CANCELLED_LINE,
};
use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::send::{
    ApprovedOutcome, OutboxCounts, OutboxListing, OutboxRetryOutcome, OutboxRow, RecipientOutcome,
    SendOutcome, SentCopy,
};
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::daemon::dispatch::{CancelScope, MethodKind};
use mailypoppins::daemon::methods::send::{FAKE_TRANSPORT_ENV, SEND_METHOD_SPECS};
use mailypoppins::outbox::OutboxState;

use support::parity::{
    assert_byte_identical, mp_command, mp_no_daemon, oracle_command, socket_path, DaemonFixture,
    EXIT_UNAVAILABLE, REQUIRE_ENV,
};
use support::send_fixture as fixture;

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// Upper bound on waiting for an operation to settle.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// The JSON-RPC code this file asserts on by number, because it is the
/// standard one rather than a daemon-range name.
///
/// Everything this slice refuses is the caller naming something the daemon
/// cannot act on - a draft that is not there, an account that cannot send an
/// invitation, a row a retry may not re-arm - so `-32602` carries all of it
/// and `-32603` appears nowhere: an internal error here would mean the daemon
/// failed, not that the request was wrong.
const INVALID_PARAMS: i32 = -32602;

/// The default undo-send hold (`email.send_hold_secs`, `SND-04`). No CLI send
/// path may wait it out, so it is the ceiling
/// [`a_routed_send_delivers_without_waiting_out_a_hold`] measures against.
const HOLD_SECS: u64 = 20;

/// The six methods of the family, in the order their spec array declares them,
/// which is method-name order like every other family.
const SEND_METHODS: [&str; 6] = [
    "send.approved",
    "send.draft",
    "send.invite",
    "send.outbox_discard",
    "send.outbox_list",
    "send.outbox_retry",
];

/// The family declares exactly those names, checked while the tree compiles:
/// an array that grew a method nobody wrote down fails here, naming the
/// constant, before a single test runs.
const _: () = assert!(SEND_METHOD_SPECS.len() == SEND_METHODS.len());

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A seeded root, a daemon serving it, and the pre-daemon oracle beside it.
///
/// The field order is the drop order: the daemon dies before the directory it
/// was reading is removed.
struct Slice {
    daemon: Option<DaemonFixture>,
    env: Vec<(String, String)>,
    tmp: TempDir,
}

impl Slice {
    /// Seed the root, then start a daemon over it. In that order: the daemon
    /// loads `config.toml` once, at startup.
    fn start() -> Slice {
        Slice::start_with(&[])
    }

    /// The same, with hooks put back on top of the sandbox.
    ///
    /// `sandbox_env` removes every entry of `DAEMON_ENV_HOOKS`, which is what
    /// keeps a developer's exported variable out of a parity comparison, so a
    /// test that wants the fake transport has to ask for it here.
    fn start_with(env: &[(&str, &str)]) -> Slice {
        let tmp = TempDir::new().expect("a temporary send-slice root");
        fixture::seed(tmp.path());
        let owned: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let daemon = DaemonFixture::start_with(tmp.path(), None, env);
        Slice {
            daemon: Some(daemon),
            env: owned,
            tmp,
        }
    }

    /// A root whose daemon serves SMTP and the Sent-mailbox APPEND in process.
    ///
    /// The hook's value names the ledger file, which lives under the root, so
    /// it can only be built once the root exists: `build` is handed the log
    /// path and returns the hook.
    fn with_transport(build: impl Fn(&Path) -> String) -> Slice {
        let tmp = TempDir::new().expect("a temporary send-slice root");
        fixture::seed(tmp.path());
        let hook = build(&fixture::transport_log(tmp.path()));
        let daemon = DaemonFixture::start_with(tmp.path(), None, &[(FAKE_TRANSPORT_ENV, &hook)]);
        Slice {
            daemon: Some(daemon),
            env: vec![(FAKE_TRANSPORT_ENV.to_string(), hook)],
            tmp,
        }
    }

    /// The same, with a transport that accepts every recipient and files the
    /// copy.
    fn with_fake_transport() -> Slice {
        Slice::with_transport(fixture::fake_transport)
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// The fake transport's ledger under this root.
    fn log(&self) -> std::path::PathBuf {
        fixture::transport_log(self.root())
    }

    fn daemon(&self) -> &DaemonFixture {
        self.daemon
            .as_ref()
            .expect("a daemon is running for this call")
    }

    /// The client under test, made to prove it reached the daemon.
    fn routed(&self, args: &[&str]) -> Output {
        self.daemon().mp_routed(args)
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
    ///
    /// For a command that writes nothing: the two runs share one root and the
    /// second must see exactly what the first did.
    fn both(&self, args: &[&str]) -> Output {
        let routed = self.routed(args);
        let direct = self.oracle(args);
        assert_byte_identical(&routed, &direct);
        routed
    }

    /// Assert the two binaries agree byte for byte, and run `check` over each
    /// of their outputs.
    fn for_each_binary(&self, args: &[&str], check: impl Fn(&str, &Output)) {
        let routed = self.routed(args);
        let direct = self.oracle(args);
        assert_byte_identical(&routed, &direct);
        check("pre-daemon", &direct);
        check("routed", &routed);
    }

    /// The same comparison for a command that writes, with the fixture put
    /// back between the two runs and the resulting state compared.
    ///
    /// `state` is read after each binary has run and must agree: bytes alone
    /// would let a routed command print the right sentence about the wrong
    /// row. The daemon is stopped before the restore and started again after
    /// it, because it holds the store open.
    fn both_mutating<S: std::fmt::Debug + PartialEq>(
        &mut self,
        args: &[&str],
        state: impl Fn(&Path) -> S,
    ) -> Output {
        let pristine = fixture::Pristine::take(self.root());

        let routed = self.routed(args);
        let after_routed = state(self.root());

        self.stop_daemon();
        pristine.restore();

        let direct = self.oracle(args);
        let after_direct = state(self.root());

        pristine.restore();
        self.start_daemon();

        assert_byte_identical(&routed, &direct);
        assert_eq!(
            after_routed,
            after_direct,
            "`mp {}` printed the same bytes and left a different fixture behind",
            args.join(" ")
        );
        routed
    }

    /// End the daemon and wait until it is really gone.
    fn stop_daemon(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            daemon.stop();
        }
    }

    /// Start a daemon over the same root, with the same hooks.
    fn start_daemon(&mut self) {
        let env: Vec<(&str, &str)> = self
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        self.daemon = Some(DaemonFixture::start_with(self.tmp.path(), None, &env));
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

/// The `data` payload an error code fixes.
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

/// Start an operation and wait for its terminal status in one step.
async fn run_operation(conn: &mut Connection, method: &str, params: Value) -> Value {
    let started = call(conn, method, params).await;
    assert_eq!(
        started.as_object().map(|o| o.len()),
        Some(1),
        "{method} answers with the id and nothing else: {started}"
    );
    settle(conn, &operation_id(&started)).await
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

/// stdout as text.
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

/// Assert a run stopped at the confirmation prompt without sending anything.
fn assert_cancelled(which: &str, out: &Output) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{which}: declining a prompt is not an error"
    );
    let text = stdout(out);
    assert!(
        text.contains(fixture::CANCELLED),
        "{which} must say it stopped:\n{text}"
    );
}

/// The invite preview's `UID:` line, which `invite::generate_uid` mints per
/// run and which is therefore the only unreproducible line in that row.
fn mask_uid(out: &Output) -> Output {
    let masked: String = stdout(out)
        .lines()
        .filter(|line| !line.trim_start().starts_with("UID:"))
        .map(|line| format!("{line}\n"))
        .collect();
    Output {
        status: out.status,
        stdout: masked.into_bytes(),
        stderr: out.stderr.clone(),
    }
}

/// A synthetic outcome with every clause of the formatter reachable: two
/// recipients, one of whom never got it, a Sent copy that was filed, and a
/// draft that could not be retired.
fn partly_delivered() -> SendOutcome {
    SendOutcome {
        account: fixture::ACCOUNT.to_string(),
        selector: Some(fixture::selector(fixture::ACCOUNT, fixture::APPROVED)),
        message_id: "<synthetic@example.com>".to_string(),
        status_line: "partly delivered, see `mp outbox list`".to_string(),
        recipients: vec![
            RecipientOutcome {
                address: fixture::TO.to_string(),
                role: "To".to_string(),
                delivered: true,
                error: None,
            },
            RecipientOutcome {
                address: fixture::REJECTED.to_string(),
                role: "To".to_string(),
                delivered: false,
                error: Some(fixture::REJECTION.to_string()),
            },
        ],
        sent_copy: SentCopy::Filed,
        settle_error: None,
    }
}

/// The same outcome with everyone delivered, which is what a plain success
/// looks like.
fn all_delivered() -> SendOutcome {
    let mut outcome = partly_delivered();
    outcome.recipients[1].delivered = true;
    outcome.recipients[1].error = None;
    outcome.status_line = "sent + saved".to_string();
    outcome
}

// ---------------------------------------------------------------------------
// 1. The methods themselves
// ---------------------------------------------------------------------------

/// The six declarations, and the four facts each one fixes: the wire name, the
/// kind, the first protocol version and what a disconnect does to it.
#[test]
fn the_send_family_declares_six_methods() {
    let names: Vec<&str> = SEND_METHOD_SPECS.iter().map(|spec| spec.name).collect();
    assert_eq!(names, SEND_METHODS, "the family serves exactly these six");

    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the array is in method-name order");

    for spec in SEND_METHOD_SPECS {
        assert_eq!(spec.since, 1, "{} is served from protocol 1", spec.name);
    }
}

/// A send outlives the client that asked for it, without exception.
///
/// There is no client-scoped method in this family and there may not be one: a
/// submission abandoned because a window closed is exactly the ambiguous state
/// `sweep_pending_sends` has to park for a human, and manufacturing that state
/// on every disconnect would be the opposite of what the durable outbox is
/// for.
#[test]
fn every_send_method_is_durable() {
    for spec in SEND_METHOD_SPECS {
        assert_eq!(
            spec.cancel_scope,
            CancelScope::Durable,
            "{} must not be abandoned when its client goes away",
            spec.name
        );
    }
}

/// Three operations, one query, one command, and the reasoning for each.
#[test]
fn the_kinds_follow_what_each_method_actually_does() {
    let kind = |name: &str| {
        SEND_METHOD_SPECS
            .iter()
            .find(|spec| spec.name == name)
            .unwrap_or_else(|| panic!("{name} is declared"))
            .kind
    };

    for name in ["send.approved", "send.draft", "send.invite"] {
        assert_eq!(
            kind(name),
            MethodKind::Operation,
            "{name} talks to a server for as long as the server takes"
        );
    }
    assert_eq!(
        kind("send.outbox_retry"),
        MethodKind::Operation,
        "a retry re-arms the row and then drains it against SMTP and IMAP"
    );
    assert_eq!(
        kind("send.outbox_list"),
        MethodKind::Query,
        "listing rows changes nothing"
    );
    assert_eq!(
        kind("send.outbox_discard"),
        MethodKind::Command,
        "a discard is one committed transaction against the local store"
    );
}

/// A method may not be served without being advertised: the handshake derives
/// the capability list from the dispatcher, so the names appearing there is
/// what says they are registered rather than merely declared.
#[tokio::test]
async fn the_daemon_advertises_the_send_slice_methods() {
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

    for method in SEND_METHODS {
        assert!(
            result.capabilities.iter().any(|c| c == method),
            "{method} is served, so it is advertised: {:?}",
            result.capabilities
        );
    }
}

/// No send path this slice serves takes a hold, a countdown or a delay.
///
/// `SND-04`'s undo-send lives in the TUI today and Phase 6 moves it into the
/// daemon *without* putting it on the CLI's path (`ANO-7`). Pinning the
/// absence here means Phase 6 cannot quietly add the parameter to the method
/// the CLI calls; it has to add a method of its own.
#[tokio::test]
async fn the_cli_send_paths_take_no_hold() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for method in ["send.draft", "send.approved", "send.invite"] {
        for extra in ["hold", "hold_secs", "countdown"] {
            let refused = call_err(
                &mut conn,
                method,
                json!({
                    "account": fixture::ACCOUNT,
                    "selector": fixture::selector(fixture::ACCOUNT, fixture::APPROVED),
                    "subject": "x",
                    "start": "2026-07-20T14:00",
                    "to": fixture::TO,
                    extra: 5,
                }),
            )
            .await;
            assert_eq!(
                refused.code, INVALID_PARAMS,
                "{method} has no {extra} parameter; the CLI send paths bypass the hold"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 2. `send.draft`
// ---------------------------------------------------------------------------

/// Every way of naming no draft, or the wrong one, is refused before anything
/// is built - and each refusal is the sentence the user reads.
#[tokio::test]
async fn send_draft_refuses_what_it_cannot_send() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let missing = call_err(&mut conn, "send.draft", json!({})).await;
    assert_eq!(missing.code, INVALID_PARAMS);
    assert!(
        missing.message.contains("account"),
        "the refusal names the parameter it wanted: {}",
        missing.message
    );

    let no_selector = call_err(
        &mut conn,
        "send.draft",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    assert_eq!(
        no_selector.code, INVALID_PARAMS,
        "`selector` is required: an invitation is `send.invite`, not a draft-less `send.draft`"
    );

    let unknown_account = call_err(
        &mut conn,
        "send.draft",
        json!({
            "account": fixture::UNKNOWN_ACCOUNT,
            "selector": fixture::selector(fixture::UNKNOWN_ACCOUNT, fixture::VALID),
        }),
    )
    .await;
    assert_eq!(unknown_account.code, ErrorCode::AccountUnknown.code());
    assert_eq!(
        data_of(&unknown_account)["account"],
        fixture::UNKNOWN_ACCOUNT
    );

    let plural = call_err(
        &mut conn,
        "send.draft",
        json!({
            "account": fixture::ACCOUNT,
            "selector": fixture::selector(fixture::ACCOUNT, fixture::VALID),
            "all_accounts": true,
        }),
    )
    .await;
    assert_eq!(
        plural.code, INVALID_PARAMS,
        "there is no all_accounts on a method that sends one draft"
    );
}

/// A selector naming another account is refused rather than sent from the
/// wrong transport.
///
/// The account and the selector are both on the wire and they have to agree:
/// `ensure_selector_account_matches` is the CLI's guard today and it moves
/// into the daemon with the rest of the account knowledge.
#[tokio::test]
async fn send_draft_refuses_a_cross_account_selector() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let crossed = call_err(
        &mut conn,
        "send.draft",
        json!({
            "account": fixture::ACCOUNT,
            "selector": fixture::selector(fixture::OTHER_ACCOUNT, fixture::BETA_DRAFT),
        }),
    )
    .await;
    assert_eq!(crossed.code, INVALID_PARAMS);
    assert!(
        crossed.message.contains(fixture::OTHER_ACCOUNT),
        "the refusal names the account the selector asked for: {}",
        crossed.message
    );
}

/// A draft that does not validate never reaches a transport, and the operation
/// fails rather than the call: the client renders it beside every other
/// per-send failure.
#[tokio::test]
async fn send_draft_fails_the_operation_on_an_invalid_draft() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let settled = run_operation(
        &mut conn,
        "send.draft",
        json!({
            "account": fixture::ACCOUNT,
            "selector": fixture::selector(fixture::ACCOUNT, fixture::NO_SUBJECT),
        }),
    )
    .await;

    assert_eq!(settled["method"], "send.draft");
    assert_eq!(settled["scope"], "durable");
    assert_eq!(settled["state"], "failed", "{settled}");
    assert!(settled["result"].is_null(), "a failure produced nothing");
    assert_path_free("send.draft", &settled);
}

/// An unknown draft is the caller's parameter being wrong, not an internal
/// failure, and it is refused before an operation id exists.
#[tokio::test]
async fn send_draft_refuses_an_unknown_draft_before_it_starts() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let refused = call_err(
        &mut conn,
        "send.draft",
        json!({
            "account": fixture::ACCOUNT,
            "selector": fixture::selector(fixture::ACCOUNT, fixture::UNKNOWN_DRAFT),
        }),
    )
    .await;
    assert_eq!(refused.code, INVALID_PARAMS);
    assert!(
        refused.message.contains(fixture::UNKNOWN_DRAFT),
        "the refusal names what could not be resolved: {}",
        refused.message
    );
    assert_path_free("send.draft", &json!(refused.data));
}

// ---------------------------------------------------------------------------
// 3. `send.invite`
// ---------------------------------------------------------------------------

/// `ANO-4` at the wire: a Graph account is refused for an invitation, in the
/// sentence `src/main.rs` prints today, and *before* anything else about the
/// invitation is examined.
///
/// The ordering is the contract, not an accident. The GUI shows the action as
/// disabled with its reason (`SND-05`), which it can only do if the refusal
/// arrives from a call that carried no valid invitation - so an invite with no
/// subject, no start and no recipients still earns the Graph refusal rather
/// than a complaint about the subject.
#[tokio::test]
async fn send_invite_refuses_a_graph_account_before_it_looks_at_anything_else() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let refused = call_err(
        &mut conn,
        "send.invite",
        json!({"account": fixture::GRAPH_ACCOUNT}),
    )
    .await;
    assert_eq!(
        refused.code, INVALID_PARAMS,
        "the caller named an account that cannot do this, which is a parameter problem"
    );
    assert_eq!(
        refused.message,
        fixture::GRAPH_INVITE_REFUSAL,
        "the sentence is the user's, verbatim"
    );
    assert_eq!(data_of(&refused)["account"], fixture::GRAPH_ACCOUNT);

    // And with a complete, valid invitation: the same refusal, so the reason
    // is about the account and never about the arguments.
    let complete = call_err(
        &mut conn,
        "send.invite",
        json!({
            "account": fixture::GRAPH_ACCOUNT,
            "to": fixture::TO,
            "subject": "Planning",
            "start": "2026-07-20T14:00",
            "duration": "1h",
        }),
    )
    .await;
    assert_eq!(complete.message, fixture::GRAPH_INVITE_REFUSAL);
}

/// The invitation's own parameters, and the refusals each missing one earns,
/// in the order `run_send_invite` makes them.
#[tokio::test]
async fn send_invite_refuses_an_incomplete_invitation() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let account = fixture::SMTP_ACCOUNT;

    let no_subject = call_err(&mut conn, "send.invite", json!({"account": account})).await;
    assert_eq!(no_subject.code, INVALID_PARAMS);
    assert_eq!(no_subject.message, fixture::INVITE_NEEDS_A_SUBJECT);

    let no_start = call_err(
        &mut conn,
        "send.invite",
        json!({"account": account, "subject": "Planning"}),
    )
    .await;
    assert_eq!(no_start.message, fixture::INVITE_NEEDS_A_START);

    let no_recipient = call_err(
        &mut conn,
        "send.invite",
        json!({
            "account": account,
            "subject": "Planning",
            "start": "2026-07-20T14:00",
            "duration": "1h",
        }),
    )
    .await;
    assert_eq!(no_recipient.message, fixture::INVITE_NEEDS_A_RECIPIENT);

    // `--end` and `--duration` answer the same question, and `invite::
    // resolve_times` refuses the pair; the refusal crosses as a parameter
    // error because that is what it is.
    let both_ends = call_err(
        &mut conn,
        "send.invite",
        json!({
            "account": account,
            "to": fixture::TO,
            "subject": "Planning",
            "start": "2026-07-20T14:00",
            "end": "2026-07-20T15:00",
            "duration": "1h",
        }),
    )
    .await;
    assert_eq!(both_ends.code, INVALID_PARAMS);
}

// ---------------------------------------------------------------------------
// 4. `send.approved`
// ---------------------------------------------------------------------------

/// One account per call, and every way of naming none or several is the
/// caller's parameter being wrong.
#[tokio::test]
async fn send_approved_names_exactly_one_account() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let missing = call_err(&mut conn, "send.approved", json!({})).await;
    assert_eq!(missing.code, INVALID_PARAMS);

    let plural = call_err(
        &mut conn,
        "send.approved",
        json!({"account": fixture::ACCOUNT, "all_accounts": true}),
    )
    .await;
    assert_eq!(
        plural.code, INVALID_PARAMS,
        "`--all-accounts` is a loop in the client, in configuration order"
    );

    let unknown = call_err(
        &mut conn,
        "send.approved",
        json!({"account": fixture::UNKNOWN_ACCOUNT}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
}

/// An account with nothing approved is a batch of zero, not a refusal.
///
/// `mp send-approved` prints one line and exits 0 over such an account, and it
/// keeps going to the next one under `--all-accounts`; an error would make the
/// loop's exit code depend on which accounts happened to be empty.
#[tokio::test]
async fn send_approved_over_an_empty_batch_succeeds_with_nothing_in_it() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let settled = run_operation(
        &mut conn,
        "send.approved",
        json!({"account": fixture::OTHER_ACCOUNT}),
    )
    .await;
    assert_eq!(settled["state"], "succeeded", "{settled}");

    let outcome: ApprovedOutcome = serde_json::from_value(settled["result"].clone())
        .expect("send.approved answers with an ApprovedOutcome");
    assert_eq!(outcome.account, fixture::OTHER_ACCOUNT);
    assert!(outcome.results.is_empty(), "nothing was approved");
    assert_eq!(outcome.sent, 0);
    assert_eq!(outcome.failed, 0);
    assert_path_free("send.approved", &settled);
}

// ---------------------------------------------------------------------------
// 5. The outbox methods
// ---------------------------------------------------------------------------

/// The listing is a query over one account, and it carries every field
/// `mp outbox list` prints and no path at all.
#[tokio::test]
async fn outbox_list_carries_every_field_the_listing_renders() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "send.outbox_list",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    assert_path_free("send.outbox_list", &result);

    let listing: OutboxListing =
        serde_json::from_value(result).expect("send.outbox_list answers with an OutboxListing");
    assert_eq!(listing.account, fixture::ACCOUNT);
    assert!(
        listing.ever_used,
        "this account has a store, which is what the 'nothing has been queued' line asks"
    );

    let ids: Vec<i64> = listing.rows.iter().map(|row| row.id).collect();
    assert_eq!(
        ids,
        fixture::listed_rows(slice.root(), fixture::ACCOUNT),
        "the listing is the store's own unfinished rows, in id order"
    );

    let queued: &OutboxRow = listing
        .rows
        .iter()
        .find(|row| row.id == fixture::QUEUED_ROW)
        .expect("the never-submitted row is listed");
    assert_eq!(queued.state, OutboxState::PendingSend.as_str());
    assert!(!queued.partial);
    assert!(
        queued.never_submitted,
        "no marker, so the next sync sends it, and the listing says so"
    );
    assert_eq!(queued.message_id, fixture::QUEUED_MID);
    assert_eq!(
        queued.target_mailbox.as_deref(),
        Some(fixture::SENT_MAILBOX)
    );
    assert_eq!(queued.updated, fixture::FIXED_UPDATED);

    let failed: &OutboxRow = listing
        .rows
        .iter()
        .find(|row| row.id == fixture::FAILED_ROW)
        .expect("the parked row is listed");
    assert_eq!(failed.state, OutboxState::Failed.as_str());
    assert_eq!(failed.last_error.as_deref(), Some(fixture::AMBIGUOUS));

    // SND-08: a `done` row that kept a note is `partial`, not `done`, because
    // calling it done would bury the recipient who never got it.
    let partial: &OutboxRow = listing
        .rows
        .iter()
        .find(|row| row.id == fixture::PARTIAL_ROW)
        .expect("the partly delivered row is listed");
    assert_eq!(
        partial.state,
        OutboxState::Done.as_str(),
        "the row's own state is what the store holds"
    );
    assert!(
        partial.partial,
        "and `partial` is the flag the listing colours it by"
    );
    assert_eq!(
        partial.rejected,
        vec![(
            fixture::REJECTED.to_string(),
            fixture::REJECTION.to_string()
        )],
        "the recipient who never got it, and the reason"
    );

    let counts: &OutboxCounts = &listing.counts;
    assert_eq!(counts.open, 2, "the queued row and the appending one");
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.partial, 1);
}

/// An account whose store was never created has never queued anything, and
/// that is a fact in the answer rather than an error.
///
/// `mp outbox list` prints "nothing has been queued for <account> yet" and
/// exits 0 for it, so the daemon has to be able to say it: an `account_unknown`
/// or an internal error would leave the client no way to reproduce the line.
#[tokio::test]
async fn outbox_list_of_a_storeless_account_answers_that_nothing_was_queued() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "send.outbox_list",
        json!({"account": fixture::STORELESS_ACCOUNT}),
    )
    .await;
    let listing: OutboxListing = serde_json::from_value(result).expect("an OutboxListing");
    assert!(!listing.ever_used);
    assert!(listing.rows.is_empty());
    assert_eq!(listing.counts.open, 0);

    let unknown = call_err(
        &mut conn,
        "send.outbox_list",
        json!({"account": fixture::UNKNOWN_ACCOUNT}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
}

/// A discard is a command: one row, one transaction, one revision, and the
/// message-id in the answer so the client can name what it dropped without
/// having listed first.
#[tokio::test]
async fn outbox_discard_is_a_command_that_names_what_it_dropped() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let unknown_row = call_err(
        &mut conn,
        "send.outbox_discard",
        json!({"account": fixture::ACCOUNT, "row_id": fixture::UNKNOWN_ROW}),
    )
    .await;
    assert_eq!(unknown_row.code, INVALID_PARAMS);
    assert_eq!(
        unknown_row.message,
        fixture::no_such_row(fixture::UNKNOWN_ROW),
        "the refusal is the sentence `mp outbox discard` prints"
    );

    let result = call(
        &mut conn,
        "send.outbox_discard",
        json!({"account": fixture::ACCOUNT, "row_id": fixture::FAILED_ROW}),
    )
    .await;
    assert_eq!(result["discarded"], true);
    assert_eq!(result["row_id"], fixture::FAILED_ROW);
    assert_eq!(result["message_id"], fixture::FAILED_MID);
    assert!(
        result["revision"].as_u64().is_some_and(|r| r > 0),
        "a command reports the revision it moved to: {result}"
    );
    assert_path_free("send.outbox_discard", &result);

    assert_eq!(
        fixture::row_state(slice.root(), fixture::ACCOUNT, fixture::FAILED_ROW),
        None,
        "the row is gone, and its bytes with it"
    );
}

/// A retry is an operation over one row, and the row has to be one a retry can
/// do anything with.
///
/// `outbox::retry` refuses any state but `failed`: a row that is mid-flight
/// cannot be re-armed under the send path's feet, and the refusal crosses as a
/// parameter error because the caller named the wrong row.
#[tokio::test]
async fn outbox_retry_refuses_a_row_it_may_not_rearm() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let missing = call_err(
        &mut conn,
        "send.outbox_retry",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    assert_eq!(missing.code, INVALID_PARAMS);
    assert!(
        missing.message.contains("row_id"),
        "a retry names one row: {}",
        missing.message
    );

    let unknown_row = call_err(
        &mut conn,
        "send.outbox_retry",
        json!({"account": fixture::ACCOUNT, "row_id": fixture::UNKNOWN_ROW}),
    )
    .await;
    assert_eq!(unknown_row.code, INVALID_PARAMS);

    let mid_flight = call_err(
        &mut conn,
        "send.outbox_retry",
        json!({"account": fixture::ACCOUNT, "row_id": fixture::APPENDING_ROW}),
    )
    .await;
    assert_eq!(
        mid_flight.code, INVALID_PARAMS,
        "only a parked row may be re-armed: {}",
        mid_flight.message
    );

    // `mp outbox retry` has no `--all`, so neither has the method.
    let all = call_err(
        &mut conn,
        "send.outbox_retry",
        json!({"account": fixture::ACCOUNT, "all": true}),
    )
    .await;
    assert_eq!(all.code, INVALID_PARAMS);
}

// ---------------------------------------------------------------------------
// 6. The shared wordings
// ---------------------------------------------------------------------------

/// Every line `mp outbox list` prints is `mp_client::format`'s, so P4-U12 can
/// make the TUI adopt the same strings without a second copy of them drifting.
///
/// This is asserted against the *command's own stdout* rather than against a
/// hand-written expectation, because two of those lines pad a coloured column
/// and a hand-derived width would pin the harness's guess at `colored`'s
/// `Display` rather than the wording. What makes it a real assertion is that
/// the same stdout is proven byte-identical to the oracle's in
/// [`mp_outbox_list_renders_every_row_shape_exactly_as_before`].
#[tokio::test]
async fn the_client_wordings_are_the_lines_mp_outbox_list_prints() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let listing: OutboxListing = serde_json::from_value(
        call(
            &mut conn,
            "send.outbox_list",
            json!({"account": fixture::ACCOUNT}),
        )
        .await,
    )
    .expect("an OutboxListing");

    let rendered = outbox_cli_lines(&listing)
        .into_iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    assert_eq!(
        rendered,
        stdout(&slice.routed(&["outbox", "list"])),
        "the CLI prints the client wordings and nothing else"
    );

    // The annotations, which carry no padded column and can be named.
    assert!(
        rendered.contains("        never submitted; the next sync sends it"),
        "the never-submitted note:\n{rendered}"
    );
    assert!(
        rendered.contains(&format!("        sent copy -> {}", fixture::SENT_MAILBOX)),
        "the target mailbox:\n{rendered}"
    );
    assert!(
        rendered.contains(&format!(
            "        never delivered to: {} ({})",
            fixture::REJECTED,
            fixture::REJECTION
        )),
        "SND-08's recipient line:\n{rendered}"
    );
    assert!(
        rendered.contains("  ↻ 2 working, 1 failed, 1 partly delivered"),
        "the counts line:\n{rendered}"
    );
}

/// An account that never queued anything, and one whose outbox is clear, are
/// two different sentences and neither of them is a row.
#[tokio::test]
async fn the_empty_outbox_wordings_are_two_different_sentences() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let storeless: OutboxListing = serde_json::from_value(
        call(
            &mut conn,
            "send.outbox_list",
            json!({"account": fixture::STORELESS_ACCOUNT}),
        )
        .await,
    )
    .expect("an OutboxListing");
    assert_eq!(
        outbox_cli_lines(&storeless),
        vec![fixture::never_queued_line(fixture::STORELESS_ACCOUNT)]
    );

    let clear = OutboxListing {
        account: fixture::OTHER_ACCOUNT.to_string(),
        ever_used: true,
        rows: Vec::new(),
        counts: OutboxCounts::default(),
    };
    assert_eq!(
        outbox_cli_lines(&clear),
        vec![fixture::outbox_clear_line(fixture::OTHER_ACCOUNT)],
        "a store that exists and holds nothing is clear, not unused"
    );
}

/// The two lines a retry prints, and the third it prints only when a copy was
/// filed.
#[test]
fn the_retry_wordings_report_the_row_and_the_copies() {
    let landed = OutboxRetryOutcome {
        row_id: fixture::FAILED_ROW,
        state: Some(OutboxState::Done.as_str().to_string()),
        completed: 1,
    };
    assert_eq!(
        outbox_retry_lines(&landed),
        vec![
            format!(
                "  ↻ row {} is queued again; sending it now",
                fixture::FAILED_ROW
            ),
            format!("  ✓ row {} is now done", fixture::FAILED_ROW),
            "  ✓ 1 sent copy/copies filed".to_string(),
        ]
    );

    // A row the retry finished and discarded is gone rather than in a state,
    // and no copies means no third line.
    let gone = OutboxRetryOutcome {
        row_id: fixture::FAILED_ROW,
        state: None,
        completed: 0,
    };
    assert_eq!(
        outbox_retry_lines(&gone),
        vec![
            format!(
                "  ↻ row {} is queued again; sending it now",
                fixture::FAILED_ROW
            ),
            format!("  ✓ row {} is gone", fixture::FAILED_ROW),
        ]
    );
}

/// The discard line names the row and the message, because an operator who
/// just dropped a message is owed both.
#[test]
fn the_discard_wording_names_the_row_and_the_message() {
    assert_eq!(
        outbox_discard_line(fixture::FAILED_ROW, fixture::FAILED_MID),
        format!(
            "  ✓ discarded row {} ({}); its bytes are released",
            fixture::FAILED_ROW,
            fixture::FAILED_MID
        )
    );
}

/// Every line `mp send` prints about an outcome, over a payload that reaches
/// every clause: two recipients, one refused, and the partly-delivered tail.
#[test]
fn the_client_wordings_are_the_lines_mp_send_prints() {
    let lines = send_cli_lines(&partly_delivered());
    assert_eq!(
        lines,
        vec![
            format!("  ✓ {} (To)", fixture::TO),
            format!("  ✗ {} (To): {}", fixture::REJECTED, fixture::REJECTION),
            "⚠ Partial send: 1 succeeded, 1 failed [partly delivered, see `mp outbox list`] \
             (marked as sent -- see logs for details)"
                .to_string(),
        ],
        "SND-08 is a warning about a message that went out, not a failure"
    );

    // Everyone got it: the same recipient lines, a different tail.
    assert_eq!(
        send_cli_lines(&all_delivered()).last().map(String::as_str),
        Some("✓ Email sent successfully to all 2 recipient(s) [sent + saved]")
    );

    // The message is out and the bookkeeping failed: a warning above the
    // success line, never a failed send, because the draft's status is not the
    // message.
    let mut settled = all_delivered();
    settled.settle_error = Some("permission denied".to_string());
    let lines = send_cli_lines(&settled);
    assert_eq!(
        lines[lines.len() - 2],
        "⚠ (sent but failed to retire draft: permission denied)",
        "{lines:?}"
    );
    assert!(
        lines
            .last()
            .is_some_and(|line| line.starts_with("✓ Email sent successfully")),
        "a draft that could not be retired is still a message that went out: {lines:?}"
    );
}

/// The batch's per-draft line and its summary, which are the only two things
/// `mp send-approved` prints that `mp send` does not.
#[test]
fn the_batch_wordings_are_the_lines_mp_send_approved_prints() {
    assert_eq!(send_approved_line(&all_delivered()), "✓ [sent + saved]");

    assert_eq!(
        send_approved_line(&partly_delivered()),
        "⚠ (partial: 1/2 recipients) [partly delivered, see `mp outbox list`]"
    );

    // The batch's bookkeeping warning is its own sentence, not `mp send`'s.
    let mut settled = all_delivered();
    settled.settle_error = Some("permission denied".to_string());
    assert_eq!(
        send_approved_line(&settled),
        "⚠ (sent but failed to update status: permission denied)"
    );

    let outcome = ApprovedOutcome {
        account: fixture::ACCOUNT.to_string(),
        results: vec![all_delivered(), partly_delivered()],
        sent: 2,
        failed: 0,
    };
    assert_eq!(
        send_approved_summary(&outcome),
        fixture::approved_summary(fixture::ACCOUNT, 2, 0)
    );
}

/// The one word a declined prompt prints, in one place, because both commands
/// print it and the parity rows compare it literally.
#[test]
fn the_cancelled_wording_is_one_constant() {
    assert_eq!(CANCELLED_LINE, fixture::CANCELLED);
}

// ---------------------------------------------------------------------------
// 7. Parity: `mp send`
// ---------------------------------------------------------------------------

/// `mp send` with neither a selector nor `--invite` has nothing to send, and
/// says which of the two the caller forgot.
#[test]
fn mp_send_without_a_selector_refuses_exactly_as_before() {
    let slice = Slice::start();
    slice.for_each_binary(&["send"], |which, out| {
        assert_refused(out, fixture::SEND_NEEDS_A_SELECTOR);
        assert!(
            stdout(out).is_empty(),
            "{which} previewed something it could not resolve:\n{}",
            stdout(out)
        );
    });
}

/// A draft nothing resolves to, an account nothing configures, and a selector
/// naming the other account: three refusals, three sets of bytes.
#[test]
fn mp_send_refuses_what_it_cannot_resolve_exactly_as_before() {
    let slice = Slice::start();
    for args in [
        vec!["send", fixture::UNKNOWN_DRAFT],
        vec!["send", "-A", fixture::UNKNOWN_ACCOUNT, fixture::VALID],
        vec![
            "send",
            &fixture::selector(fixture::OTHER_ACCOUNT, fixture::BETA_DRAFT),
        ],
    ] {
        let out = slice.both(&args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "`mp {}` must refuse:\n{}",
            args.join(" "),
            stderr(&out)
        );
    }
}

/// A draft that does not validate is refused after its selector is echoed and
/// before anything is built.
#[test]
fn mp_send_refuses_an_invalid_draft_exactly_as_before() {
    let slice = Slice::start();
    let args = ["send", fixture::NO_SUBJECT, "-y"];
    slice.for_each_binary(&args, |which, out| {
        assert_eq!(out.status.code(), Some(1), "{which}");
        assert!(
            stdout(out).contains(&fixture::selector(fixture::ACCOUNT, fixture::NO_SUBJECT)),
            "{which} echoes what it resolved before it refuses:\n{}",
            stdout(out)
        );
        assert!(
            !stdout(out).contains("Sending email..."),
            "{which} started a send it should have refused:\n{}",
            stdout(out)
        );
    });
}

/// The prompt, and what a closed stdin answers.
///
/// This is the whole of the `-y` contract: without it, the client previews,
/// asks, reads EOF, prints `Cancelled.` and exits 0 - and nothing crosses the
/// socket, which the unchanged outbox afterwards is the proof of.
#[test]
fn mp_send_without_yes_previews_and_cancels_exactly_as_before() {
    let slice = Slice::start();
    let selector = fixture::selector(fixture::ACCOUNT, fixture::VALID);
    let before = fixture::listed_rows(slice.root(), fixture::ACCOUNT);

    slice.for_each_binary(&["send", &selector], |which, out| {
        assert_cancelled(which, out);
        let text = stdout(out);
        assert!(
            text.contains("=== Email Draft Preview ==="),
            "{which} must preview before it asks:\n{text}"
        );
        assert!(
            text.contains(fixture::SEND_PROMPT),
            "{which} must ask:\n{text}"
        );
        assert!(
            !text.contains("Sending email..."),
            "{which} sent a message nobody confirmed:\n{text}"
        );
    });

    assert_eq!(
        fixture::listed_rows(slice.root(), fixture::ACCOUNT),
        before,
        "a declined prompt queues nothing"
    );
    assert_eq!(
        fixture::draft_status(slice.root(), fixture::ACCOUNT, "angebot.md"),
        "draft",
        "and retires nothing"
    );
}

/// The Graph transport's preview is the simplified block, not the draft
/// preview, and it is still followed by the same prompt and the same
/// cancellation.
#[test]
fn mp_send_over_graph_previews_the_simplified_block_exactly_as_before() {
    let slice = Slice::start();
    let args = [
        "send",
        "-A",
        fixture::GRAPH_ACCOUNT,
        &fixture::selector(fixture::GRAPH_ACCOUNT, fixture::GRAPH_DRAFT),
    ];
    slice.for_each_binary(&args, |which, out| {
        assert_cancelled(which, out);
        let text = stdout(out);
        assert!(
            text.contains("--- Email Preview ---"),
            "{which} prints the Graph preview:\n{text}"
        );
        assert!(
            !text.contains("=== Email Draft Preview ==="),
            "{which} printed the SMTP preview for a Graph account:\n{text}"
        );
    });
}

/// `mp send --invite` on a Graph account: `ANO-4`, byte for byte, with nothing
/// previewed and nothing sent.
#[test]
fn mp_send_invite_refuses_a_graph_account_exactly_as_before() {
    let slice = Slice::start();
    for args in [
        vec!["send", "--invite", "-A", fixture::GRAPH_ACCOUNT],
        vec![
            "send",
            "--invite",
            "-A",
            fixture::GRAPH_ACCOUNT,
            "--to",
            fixture::TO,
            "--subject",
            "Planning",
            "--start",
            "2026-07-20T14:00",
            "--duration",
            "1h",
            "-y",
        ],
    ] {
        slice.for_each_binary(&args, |which, out| {
            assert_refused(out, fixture::GRAPH_INVITE_REFUSAL);
            assert!(
                stdout(out).is_empty(),
                "{which} previewed an invitation it refuses to send:\n{}",
                stdout(out)
            );
        });
    }
}

/// The invitation's own refusals: no subject, no start, no recipient, and both
/// ends at once.
#[test]
fn mp_send_invite_refuses_an_incomplete_invitation_exactly_as_before() {
    let slice = Slice::start();
    let account = fixture::SMTP_ACCOUNT;
    for args in [
        vec!["send", "--invite", "-A", account],
        vec!["send", "--invite", "-A", account, "--subject", "Planning"],
        vec![
            "send",
            "--invite",
            "-A",
            account,
            "--subject",
            "Planning",
            "--start",
            "2026-07-20T14:00",
            "--duration",
            "1h",
        ],
        vec![
            "send",
            "--invite",
            "-A",
            account,
            "--to",
            fixture::TO,
            "--subject",
            "Planning",
            "--start",
            "2026-07-20T14:00",
            "--end",
            "2026-07-20T15:00",
            "--duration",
            "1h",
        ],
    ] {
        let out = slice.both(&args);
        assert_eq!(out.status.code(), Some(1), "`mp {}`", args.join(" "));
    }
}

/// A complete invitation over an SMTP account: the preview, the prompt, the
/// cancellation - and the `UID:` line masked, because `invite::generate_uid`
/// mints it per run.
///
/// That mask is one of the two in this file and it removes one line. What is
/// left - the summary, the organizer, the attendee list, the resolved times,
/// the prompt, the exit code - is compared literally.
#[test]
fn mp_send_invite_previews_and_cancels_with_the_uid_masked() {
    let slice = Slice::start();
    let args = [
        "send",
        "--invite",
        "-A",
        fixture::SMTP_ACCOUNT,
        "--to",
        fixture::TO,
        "--subject",
        "Planning",
        "--start",
        "2026-07-20T14:00",
        "--duration",
        "1h30m",
        "--location",
        "Room 1",
    ];
    let routed = slice.routed(&args);
    let direct = slice.oracle(&args);

    assert_byte_identical(&mask_uid(&routed), &mask_uid(&direct));
    assert_cancelled("routed", &routed);

    let text = stdout(&routed);
    assert!(
        text.contains("--- Invite Preview ---"),
        "the invitation is previewed:\n{text}"
    );
    assert!(
        text.contains(fixture::INVITE_PROMPT),
        "and then asked about:\n{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("UID:")),
        "the masked line was really there:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// 8. Parity: `mp send-approved`
// ---------------------------------------------------------------------------

/// An account with nothing approved: one line, exit 0, no summary claiming a
/// batch that never ran.
#[test]
fn mp_send_approved_over_an_empty_batch_matches_exactly() {
    let slice = Slice::start();
    slice.for_each_binary(
        &["send-approved", "-A", fixture::OTHER_ACCOUNT],
        |which, out| {
            assert_eq!(out.status.code(), Some(0), "{which}: nothing to do is fine");
            assert!(
                stdout(out).contains(&fixture::no_approved_drafts(fixture::OTHER_ACCOUNT)),
                "{which} says why it did nothing:\n{}",
                stdout(out)
            );
            assert!(
                !stdout(out).contains("Summary"),
                "{which} summarised a batch it never ran:\n{}",
                stdout(out)
            );
        },
    );
}

/// The batch listing, the prompt and the cancellation, and the drafts still
/// approved afterwards.
#[test]
fn mp_send_approved_without_yes_lists_and_cancels_exactly_as_before() {
    let slice = Slice::start();
    slice.for_each_binary(&["send-approved"], |which, out| {
        assert_cancelled(which, out);
        let text = stdout(out);
        assert!(
            text.contains("approved email(s) found:"),
            "{which} lists the batch first:\n{text}"
        );
        assert!(
            text.contains("freigabe.md -> ivana@example.com"),
            "{which} names the file and the recipient, and nothing else about the path:\n{text}"
        );
    });

    assert_eq!(
        fixture::draft_status(slice.root(), fixture::ACCOUNT, "freigabe.md"),
        "approved",
        "a declined prompt retires nothing"
    );
}

/// `--all-accounts` walks the configured accounts in configuration order and
/// keeps going past the ones with nothing to send.
#[test]
fn mp_send_approved_all_accounts_walks_them_in_configuration_order() {
    let slice = Slice::start();
    slice.for_each_binary(&["send-approved", "--all-accounts"], |which, out| {
        let text = stdout(out);
        let mut at = 0usize;
        for account in [fixture::OTHER_ACCOUNT, fixture::STORELESS_ACCOUNT] {
            let line = fixture::no_approved_drafts(account);
            let found = text[at..].find(&line).unwrap_or_else(|| {
                panic!("{which}: {account} was not walked in configuration order:\n{text}")
            });
            at += found + line.len();
        }
    });
}

/// The three subcommand helps, byte for byte.
///
/// This is where `-y`, `--invite`, `--all-accounts` and the invitation's eight
/// flags are pinned as a flag set: `tests/cli_help_snapshot.rs` covers
/// `mp --help`, which never shows a subcommand's arguments.
#[test]
fn the_subcommand_helps_are_byte_identical() {
    let slice = Slice::start();
    for args in [
        vec!["send", "--help"],
        vec!["send-approved", "--help"],
        vec!["outbox", "--help"],
        vec!["outbox", "retry", "--help"],
    ] {
        let out = slice.both(&args);
        assert_eq!(out.status.code(), Some(0), "`mp {}`", args.join(" "));
    }
}

// ---------------------------------------------------------------------------
// 9. Parity: `mp outbox`
// ---------------------------------------------------------------------------

/// Every row shape in one listing: never-submitted, parked, partly delivered
/// and appending, plus the counts line under them.
#[test]
fn mp_outbox_list_renders_every_row_shape_exactly_as_before() {
    let slice = Slice::start();
    slice.for_each_binary(&["outbox", "list"], |which, out| {
        assert_eq!(out.status.code(), Some(0), "{which}");
        let text = stdout(out);
        for needle in [
            fixture::QUEUED_MID,
            fixture::FAILED_MID,
            fixture::PARTIAL_MID,
            fixture::APPENDING_MID,
            "never submitted; the next sync sends it",
            "never delivered to:",
            "partly delivered",
        ] {
            assert!(
                text.contains(needle),
                "{which} is missing {needle:?}:\n{text}"
            );
        }
    });
}

/// An account with no store, and one whose outbox has nothing to say: two
/// sentences, both exit 0.
#[test]
fn mp_outbox_list_of_an_empty_account_matches_exactly() {
    let slice = Slice::start();
    slice.for_each_binary(
        &["outbox", "list", "-A", fixture::STORELESS_ACCOUNT],
        |which, out| {
            assert_eq!(out.status.code(), Some(0), "{which}");
            assert!(
                stdout(out).contains(&fixture::never_queued_line(fixture::STORELESS_ACCOUNT)),
                "{which}:\n{}",
                stdout(out)
            );
        },
    );

    slice.for_each_binary(
        &["outbox", "list", "-A", fixture::OTHER_ACCOUNT],
        |which, out| {
            assert_eq!(out.status.code(), Some(0), "{which}");
            assert!(
                stdout(out).contains(&fixture::outbox_clear_line(fixture::OTHER_ACCOUNT)),
                "{which}:\n{}",
                stdout(out)
            );
        },
    );
}

/// A row id nothing holds: the refusal, and no row touched either way.
#[test]
fn mp_outbox_of_an_unknown_row_refuses_exactly_as_before() {
    let mut slice = Slice::start();
    let id = fixture::UNKNOWN_ROW.to_string();
    let out = slice.both_mutating(&["outbox", "discard", &id], |root| {
        fixture::listed_rows(root, fixture::ACCOUNT)
    });
    assert_refused(&out, &fixture::no_such_row(fixture::UNKNOWN_ROW));
}

/// `mp outbox discard`: the sentence, the row gone, and the same row gone on
/// both sides.
///
/// The one mutating parity row of this file. The fixture is restored between
/// the two binaries and the resulting row list is compared, because bytes
/// alone would let a routed discard print the right sentence about the wrong
/// row.
#[test]
fn mp_outbox_discard_drops_the_same_row_in_both_binaries() {
    let mut slice = Slice::start();
    let id = fixture::FAILED_ROW.to_string();

    let out = slice.both_mutating(&["outbox", "discard", &id], |root| {
        fixture::listed_rows(root, fixture::ACCOUNT)
    });

    assert_eq!(out.status.code(), Some(0));
    assert!(
        stdout(&out).contains(&outbox_discard_line(
            fixture::FAILED_ROW,
            fixture::FAILED_MID
        )),
        "the discard names the row and the message:\n{}",
        stdout(&out)
    );
    // `both_mutating` puts the fixture back after the second binary has run,
    // so what is asserted here is the *harness*: a comparison that left the
    // root half-discarded would silently poison every row after it.
    assert!(
        fixture::listed_rows(slice.root(), fixture::ACCOUNT).contains(&fixture::FAILED_ROW),
        "the fixture is pristine again once both binaries have had their turn"
    );
}

// ---------------------------------------------------------------------------
// 10. Determinism, and what proves routing
// ---------------------------------------------------------------------------

/// Two routed runs of the same read-only command produce the same bytes.
///
/// Only the rows that write nothing belong here; the mutating ones are
/// compared through [`Slice::both_mutating`], which is where their determinism
/// is asserted.
#[test]
fn two_routed_runs_agree() {
    let slice = Slice::start();
    for args in [
        vec!["outbox", "list"],
        vec!["outbox", "list", "-A", fixture::STORELESS_ACCOUNT],
        vec!["send"],
        vec!["send", fixture::UNKNOWN_DRAFT],
        vec!["send-approved"],
        vec!["send-approved", "-A", fixture::OTHER_ACCOUNT],
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
fn no_send_command_can_still_answer_without_a_daemon() {
    let tmp = TempDir::new().expect("a temporary root");
    fixture::seed(tmp.path());

    for args in [
        vec!["send", fixture::VALID, "-y"],
        vec!["send-approved", "-y"],
        vec!["outbox", "list"],
        vec!["outbox", "retry", "2"],
        vec!["outbox", "discard", "2"],
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

/// A run under the require gate that finds no daemon is not a run that
/// silently answered: the gate and the unavailable path agree.
#[test]
fn the_require_gate_and_the_unavailable_exit_agree() {
    let tmp = TempDir::new().expect("a temporary root");
    fixture::seed(tmp.path());

    let out = mp_command(tmp.path())
        .env(REQUIRE_ENV, "1")
        .env("MAILYPOPPINS_DAEMON_AUTOSTART", "0")
        .args(["outbox", "list"])
        .output()
        .expect("run `mp outbox list` with no daemon and the require gate on");
    assert_eq!(
        out.status.code(),
        Some(EXIT_UNAVAILABLE),
        "stdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
}

// ---------------------------------------------------------------------------
// 11. The successful paths, through the fake transport
// ---------------------------------------------------------------------------
//
// Every test below sets `FAKE_TRANSPORT_ENV` on the daemon, which the
// pre-daemon oracle knows nothing about. They are **routed-side assertions,
// not parity rows**, and they exist because without them the slice's whole
// success half - a message that goes out, a draft that is retired, a Sent copy
// that is filed - would be unpinned.

/// A routed `mp send -y` delivers, retires the draft, files the Sent copy and
/// prints the success line.
///
/// Routed side only (fake transport). What it proves is the shape of a
/// successful send end to end: the transport saw the recipients, the outbox
/// row reached `done`, the draft file is gone, and the line the user reads
/// came out of the daemon's own outcome.
///
/// "Retired" is the file being *removed*, not its `status:` line being
/// rewritten. `draft::settle_sent_draft` marks the file sent and then deletes
/// it whenever every recipient took the message and an outbox row is behind
/// it, because from that moment the copy that matters is the server's: the
/// durable row APPENDs it to Sent and ingest reads it back, so a file left in
/// `drafts/` would be a second, staler copy showing up in `mp list` and the
/// TUI with nothing left to do to it. So the assertion is the pair the
/// deletion rests on - no file, and a row in the outbox.
#[test]
fn a_routed_send_delivers_retires_the_draft_and_files_the_copy() {
    let slice = Slice::with_fake_transport();
    let selector = fixture::selector(fixture::ACCOUNT, fixture::APPROVED);

    let out = slice.routed(&["send", &selector, "-y"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );

    let text = stdout(&out);
    assert!(
        text.contains("✓ Email sent successfully to all"),
        "the success line:\n{text}"
    );
    let path = fixture::drafts_dir(slice.root(), fixture::ACCOUNT).join(fixture::APPROVED_FILE);
    assert!(
        !path.exists(),
        "a fully delivered draft is retired, file and all: {} is still there",
        path.display()
    );
    let minted: Vec<i64> = fixture::all_rows(slice.root(), fixture::ACCOUNT)
        .into_iter()
        .filter(|id| !fixture::SEEDED_ROWS.contains(id))
        .collect();
    assert_eq!(
        minted.len(),
        1,
        "the send left exactly one new outbox row, which is what the retirement rests on: {minted:?}"
    );
    assert_eq!(
        fixture::row_state(slice.root(), fixture::ACCOUNT, minted[0]),
        Some(OutboxState::Done),
        "and that row is finished: SMTP took it and the Sent copy is filed"
    );

    let events = fixture::transport_events(&slice.log());
    assert!(
        !events.is_empty(),
        "the fake transport served the send rather than the send being skipped"
    );
    // SND-09 is about *this* message's copy, counted by its Message-ID rather
    // than by the ledger's length: the send drains the account's outbox on its
    // way out, so the seeded row that was waiting on its APPEND
    // (`APPENDING_ROW`) files its copy in the same run and a total of two is
    // the drain doing its job.
    let mid = events
        .iter()
        .find_map(|event| match event {
            fixture::TransportEvent::Submit { message_id, .. } => Some(message_id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the send submitted nothing: {events:?}"));
    assert_eq!(
        fixture::appends_of(&slice.log(), &mid),
        1,
        "SND-09: exactly one Sent copy, and it was filed by this send: {events:?}"
    );
    assert!(
        !text.contains(".sqlite3") && !text.contains("/blobs/"),
        "the outcome named a file the user cannot open:\n{text}"
    );
}

/// The CLI send path does not wait out the undo-send hold.
///
/// Routed side only (fake transport). `SND-04`'s default is twenty seconds and
/// `ANO-7` records that `mp send` bypasses it; Phase 6 moves the hold into the
/// daemon and must keep that true, so the bound is asserted rather than
/// assumed.
#[test]
fn a_routed_send_delivers_without_waiting_out_a_hold() {
    let slice = Slice::with_fake_transport();
    let selector = fixture::selector(fixture::ACCOUNT, fixture::APPROVED);

    let start = Instant::now();
    let out = slice.routed(&["send", &selector, "-y"]);
    let elapsed = start.elapsed();

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        elapsed < Duration::from_secs(HOLD_SECS / 2),
        "`mp send -y` took {elapsed:?}, which is long enough to have waited out a hold"
    );
}

/// `SND-08` end to end: a server that refuses one recipient produces a partly
/// delivered row, a warning rather than a failure, and a listing an operator
/// can act on.
///
/// Routed side only (fake transport). The refused address is
/// [`fixture::REJECTED`], which the fixture puts on the approved draft's `cc:`
/// line for exactly this row: a draft with one recipient is delivered whole or
/// refused whole, and "partly" needs two.
#[test]
fn a_routed_send_whose_recipient_is_refused_is_partly_delivered() {
    let slice = Slice::with_transport(|log| {
        fixture::fake_transport_rejecting(log, &[(fixture::REJECTED, fixture::REJECTION)])
    });

    let selector = fixture::selector(fixture::ACCOUNT, fixture::APPROVED);
    let out = slice.routed(&["send", &selector, "-y"]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "a message that went out to somebody is not a failed command:\n{}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("⚠ Partial send:"),
        "SND-08 is a warning:\n{}",
        stdout(&out)
    );

    let listing = slice.routed(&["outbox", "list"]);
    assert!(
        stdout(&listing).contains(&format!("never delivered to: {}", fixture::REJECTED)),
        "the operator is told who never got it:\n{}",
        stdout(&listing)
    );
}

/// A routed `mp outbox retry` re-arms the parked row, drains it and reports
/// both the new state and the copies it filed.
///
/// Routed side only (fake transport).
#[test]
fn a_routed_outbox_retry_rearms_and_drains_the_parked_row() {
    let slice = Slice::with_fake_transport();
    let id = fixture::FAILED_ROW.to_string();

    let out = slice.routed(&["outbox", "retry", &id]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    let text = stdout(&out);
    assert!(
        text.contains(&format!(
            "  ↻ row {} is queued again; sending it now",
            fixture::FAILED_ROW
        )),
        "the retry says what it did:\n{text}"
    );
    assert!(
        text.contains(&format!("  ✓ row {} is now ", fixture::FAILED_ROW))
            || text.contains(&format!("  ✓ row {} is gone", fixture::FAILED_ROW)),
        "and what became of the row:\n{text}"
    );
    assert_eq!(
        fixture::appends_of(&slice.log(), fixture::FAILED_MID),
        1,
        "exactly one copy in Sent for the row that was retried"
    );
}

/// `tests/outbox_integration.rs::two_racing_drains_append_each_row_exactly_once`,
/// twinned through the daemon.
///
/// Routed side only (fake transport). Two `send.outbox_retry` operations are
/// started from two connections against one account, with the first parked
/// inside its APPEND so the second lands in exactly the window the legacy test
/// opens with its `Hold::Until`. Whatever the drains decide between them - one
/// refused by the engine lock, or both serialised behind it - the ledger has
/// to hold exactly one `append` per message-id, which is the invariant #0116
/// is about.
#[tokio::test]
async fn two_racing_routed_drains_append_each_row_exactly_once() {
    // The first drain parks for 300 ms inside its APPEND, which is the window
    // the legacy test opens with its `Hold::Until`.
    let slice = Slice::with_transport(|log| fixture::fake_transport_appending(log, "ok", 300));
    let log = slice.log();

    let mut first = slice.connect().await;
    let mut second = slice.connect().await;

    // Two rows: the parked one, re-armed, and the one whose SMTP is already
    // done and whose copy is outstanding. One drain each, at the same time.
    let started_first = call(
        &mut first,
        "send.outbox_retry",
        json!({"account": fixture::ACCOUNT, "row_id": fixture::FAILED_ROW}),
    )
    .await;
    let started_second = call(
        &mut second,
        "send.outbox_retry",
        json!({"account": fixture::ACCOUNT, "row_id": fixture::FAILED_ROW}),
    )
    .await;

    let first_id = operation_id(&started_first);
    let second_id = operation_id(&started_second);
    let (one, two) = tokio::join!(
        settle(&mut first, &first_id),
        settle(&mut second, &second_id),
    );
    for settled in [&one, &two] {
        assert!(
            matches!(
                settled["state"].as_str(),
                Some("succeeded") | Some("failed")
            ),
            "both drains settled: {settled}"
        );
    }

    assert_eq!(
        fixture::appends_of(&log, fixture::FAILED_MID),
        1,
        "the row two drains raced for must not be appended twice: {:?}",
        fixture::transport_events(&log)
    );
    assert_eq!(
        fixture::row_state(slice.root(), fixture::ACCOUNT, fixture::FAILED_ROW),
        Some(OutboxState::Done),
        "and it finished rather than being left mid-flight by the loser"
    );
}

/// The reachable half of
/// `tests/outbox_integration.rs::a_drain_killed_mid_append_leaves_the_row_reclaimable_and_deduped`.
///
/// Routed side only (fake transport). The killing itself is **kept direct** in
/// the legacy suite: it drops a drain future while it is inside its APPEND,
/// which through the daemon would mean killing the daemon and losing every
/// other row with it, and the scenario would stop being about the row. What is
/// reachable here is what the ambiguity arms: a copy the server filed while
/// the acknowledgement was lost must be found by the dedup search rather than
/// appended a second time.
#[test]
fn a_routed_retry_after_a_swallowed_ack_dedupes_rather_than_duplicating() {
    let slice =
        Slice::with_transport(|log| fixture::fake_transport_appending(log, "swallow_ack", 0));
    let log = slice.log();
    let id = fixture::FAILED_ROW.to_string();

    // The first drain files the copy and loses the acknowledgement.
    let first = slice.routed(&["outbox", "retry", &id]);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert_eq!(
        fixture::appends_of(&log, fixture::FAILED_MID),
        1,
        "the server filed it once"
    );

    // The row's attempt counter arms the dedup search, so the reclaim looks
    // before it appends and the copy count stays at one.
    let second = slice.routed(&["outbox", "retry", &id]);
    assert!(
        second.status.code() == Some(0) || second.status.code() == Some(1),
        "the second retry ran:\n{}",
        stderr(&second)
    );
    assert!(
        fixture::searches(&log) >= 1,
        "the reclaim must look before it appends: {:?}",
        fixture::transport_events(&log)
    );
    assert_eq!(
        fixture::appends_of(&log, fixture::FAILED_MID),
        1,
        "exactly one copy in Sent: {:?}",
        fixture::transport_events(&log)
    );
}

/// The outcome the client renders is the one the daemon published: a real
/// `send.draft` result, deserialised into the payload type and handed to the
/// same formatter the CLI uses.
///
/// Routed side only (fake transport). What is being proved is the path -
/// operation result on the wire, payload out of it, lines out of the payload -
/// and not the send.
#[tokio::test]
async fn a_send_result_renders_through_the_shared_wordings() {
    let slice = Slice::with_fake_transport();
    let mut conn = slice.connect().await;

    let settled = run_operation(
        &mut conn,
        "send.draft",
        json!({
            "account": fixture::ACCOUNT,
            "selector": fixture::selector(fixture::ACCOUNT, fixture::APPROVED),
        }),
    )
    .await;
    assert_eq!(settled["state"], "succeeded", "{settled}");
    assert_path_free("send.draft", &settled);

    let outcome: SendOutcome = serde_json::from_value(settled["result"].clone())
        .expect("send.draft answers with a SendOutcome");
    assert_eq!(outcome.account, fixture::ACCOUNT);
    assert_eq!(
        outcome.selector.as_deref(),
        Some(fixture::selector(fixture::ACCOUNT, fixture::APPROVED).as_str())
    );
    assert!(
        outcome.recipients.iter().all(|r| r.delivered),
        "the fake transport accepted everyone: {:?}",
        outcome.recipients
    );
    assert_eq!(
        outcome.sent_copy,
        SentCopy::Filed,
        "SND-09: the copy is filed, and the status says so without naming a path"
    );

    let lines = send_cli_lines(&outcome);
    assert!(
        lines
            .last()
            .is_some_and(|line| line.starts_with("✓ Email sent successfully to all")),
        "the CLI's line comes out of the daemon's payload and nowhere else: {lines:?}"
    );
}
