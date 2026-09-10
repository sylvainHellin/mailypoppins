//! The message-mutation slice, moved onto the daemon (#0123, plan unit P4-U7).
//!
//! Four commands change something today and must change the same thing from
//! the daemon tomorrow without a byte moving: `mp archive`,
//! `mp delete [--force|--sent]`, `mp open` and `mp save [-o]`. Four methods
//! carry them - `message.archive`, `message.delete`, `draft.discard` and the
//! already-shipped `message.materialise_attachment` - plus a save that stays
//! in the client, because the destination is a directory only the client's
//! process knows the meaning of (ANO-15).
//!
//! This file is a **contract test**. It is written before the methods exist,
//! against the shapes fixed here and in `docs/daemon-protocol.md`, and it
//! fails to compile against today's tree; that failure is the proof the
//! contract has no stub behind it. The implementer (P4-U8) does not edit this
//! file.
//!
//! # The surface under test
//!
//! ```text
//! message.archive   {account, id|selector, mailbox?}
//!                       -> {account, id, selector, mailbox,
//!                           moved_to: {mailbox, selector}}
//! message.delete    {account, id|selector, mailbox?}
//!                       -> {account, id, selector, mailbox}
//! draft.discard     {account, id|selector, force?}
//!                       -> {account, id, selector, status}
//! draft.discard     {account, sent: true}
//!                       -> {account, cleared, kept: [{id, selector, error}]}
//! message.materialise_attachment
//!                   {account, id|selector, mailbox?, part}
//!                       -> {handle, path, name, bytes, expires_at}
//! ```
//!
//! ## `message.archive` and `message.delete`
//!
//! **Two commands, not two flavours of one.** Archiving moves a row and owes
//! the server a `Move`; deleting drops a row and owes the server a `Delete`,
//! and has nothing to roll back if the server refuses. They are declared
//! together in [`MESSAGE_MUTATION_METHOD_SPECS`], in method-name order like
//! every other family, and both are `Command`: each moves the daemon's
//! revision and invalidates `message:<account>/<mailbox>/<uid>`.
//!
//! **Both are `durable`.** The pre-daemon commands commit the row change and
//! the owed server op in one transaction and then drain that op synchronously
//! (`pending_ops::run_and_settle`, #0039). A drain torn down because the
//! calling socket went away would leave the op queued while its caller was
//! told nothing, so the work outlives its client and the answer is the settled
//! outcome rather than an acknowledgement.
//!
//! **A message is addressed by `id` or by `selector`, never by both and never
//! by neither**, exactly as `message.get` addresses one (P4-U3): `id` is
//! `"<mailbox>/<uid>"`, `selector` is the grammar the user types, and
//! `mailbox` narrows a selector the way `--mailbox` does. The refusals are
//! that resolution's own sentences, so a routed command reports what the
//! pre-daemon one reported, ambiguity included.
//!
//! **The backend is resolved before the store is touched.** Over an account
//! with no credentials both methods refuse with the secret-store's own
//! sentence and leave the row exactly where it was, which is what the
//! pre-daemon binary does and what [`fixture::CREDENTIALS_REFUSAL`] pins. The
//! refusal is `-32603`: the account is configured and its store is readable,
//! so neither `-32005` nor `-32006` may be used without contradicting what
//! `account.list` says about the same account, and the caller's parameters
//! were right, so it is not `-32602`. Protocol 1 has no credentials code; if
//! P4-U8 wants one, it takes a protocol-changelog entry and the user-visible
//! bytes do not move.
//!
//! ## `draft.discard`
//!
//! **The tenth method of the `draft.*` family**, which is why
//! [`DRAFT_METHOD_SPECS`] grows from nine to ten and `tests/daemon_draft_slice.rs`
//! and `tests/daemon_draft_watch.rs` count ten. A draft is local: no server op,
//! no backend, just the file and the index row the next scan drops (#0073).
//!
//! **`force` is required for an `approved` draft and for nothing else.** An
//! approved draft is a queued send, and deleting it drops that send; a `sent`
//! draft needs no force, because retiring it is the whole point of the `--sent`
//! sweep. The refusal is `draft::delete_indexed_draft`'s own sentence, verbatim.
//!
//! **The `--sent` sweep is a parameter of the same method, not a method of its
//! own.** `{account, sent: true}` clears every `sent` draft of one account and
//! takes no selector; the family stays at ten methods, and one verb over a set
//! is the same verb. `sent: true` beside `id`, `selector` or `force` is
//! `-32602`: a sweep names no draft, so a caller who named one disagrees with
//! themselves. The result counts what went and lists what stayed, because the
//! sweep keeps going past a file it cannot remove and the client prints one
//! `⚠ keeping <selector>: <error>` line per survivor before its own summary.
//!
//! ## `message.materialise_attachment`, and the client-side save
//!
//! The method shipped in P3b-U12 and this slice **adds one addressing form and
//! one field**, both additive: `{selector, mailbox?}` beside the existing
//! `{id}`, because a client holding a selector cannot build `"<mailbox>/<uid>"`
//! out of a `ShownMessage` (it carries the mailbox and the `Message-ID`, not
//! the uid) and re-listing the mailbox to find the uid would be a second query
//! to answer a question the daemon already answers; and `name`, the sanitised
//! file name, so a client builds its destination without parsing the daemon's
//! path. `mime` is not on the wire: the store keeps no content type for a
//! part, and deriving one from the extension is a guess the client can make
//! for itself. Everything else - `handle`, `path` under
//! `<data_dir>/runtime/handles/<handle>/<name>`, `bytes`, `expires_at`, the
//! ten-minute lifetime, the blob pin - is unchanged, and P4-U8 records the two
//! additions in `docs/daemon-protocol.md`.
//!
//! **The daemon materialises one part; the client names the file.** Two parts
//! sent under one name come back as two handles with the same `name` in two
//! directories, and the `_1` rule that makes them two files is applied where
//! the names become paths, which is `read::materialise_attachments`'s rule and
//! now the client's: dedupe within one call, overwrite what is already on
//! disk. Saving the same message twice therefore writes the same two names
//! twice rather than growing a `_1` copy per run.
//!
//! **`mp save` prints the destination the user spelled.** The client
//! absolutises `-o` (and its default `.`) before the call, because the daemon's
//! working directory means nothing (ANO-15), and prints `dest.join(name)` with
//! `dest` as it was typed: `✓ ./notes.pdf`, `✓ out/notes.pdf`. Today's binary
//! prints the absolutised form (`✓ /tmp/…/./notes.pdf`), which is a parity
//! break P4-U2 introduced and this slice's assertions are the gate on: the
//! absolute path is what crosses the socket, not what reaches the user.
//!
//! **`mp open` launches the opener in the client.** `parse::open_file_with_system`
//! runs the bare command `open`, resolved through `PATH`, and that resolution
//! is the hook this file tests through: [`fixture::opener_env`] puts a
//! recording `open` first on the client's `PATH`. It needs no new environment
//! variable, the pre-daemon binary honours it too, and it is what keeps a test
//! run from launching `xdg-open` on a developer's desktop. P4-U8 must keep the
//! launch in the client and keep it going through that helper.
//!
//! # Parity, and what proves routing
//!
//! Every command is compared against the `pre-daemon` oracle over the same
//! seeded root: stdout, stderr and exit code, plus the state each run left
//! behind - the drafts directory, the store's mailbox contents, the files
//! written into the client's directory. A state-changing row runs the two
//! binaries from the same pristine fixture, restoring between them
//! (`draft_fixture::Stash`, a fresh working directory per binary), so the
//! second never sees the first one's work. The routed side runs under
//! `MAILYPOPPINS_DAEMON_REQUIRE=1` (`DaemonFixture::mp_routed`), so a command
//! that quietly answered in process fails instead of passing for the daemon's
//! work, and [`no_mutation_command_can_still_answer_without_a_daemon`] covers
//! the case that variable cannot: with nothing listening and auto-start off, a
//! command with no in-process path left exits 4.
//!
//! **The one row that is not byte-identical is `mp open`'s success line**, and
//! it cannot be: the pre-daemon binary prints a path under its own
//! `$TMPDIR/mailypoppins-<row>/`, and a daemon-materialised file lives under
//! `<data_dir>/runtime/handles/<handle>/` with a lifetime attached (ATT-01 is
//! "a client-side integration over a daemon-materialized file"). That row is
//! compared with the two directories masked, asserting instead that the file
//! names, their order, stderr and the exit code all agree, and separately that
//! the routed path is under the daemon's runtime directory and holds the
//! stored bytes. Nothing else is masked; every other row is literal.
//!
//! **Two routed runs agree byte for byte** for `mp save` and for every
//! refusal. `mp open` is deliberately excluded: a handle id is minted per
//! call, so two successful runs print two different paths, which is the same
//! fact the masking above records.
//!
//! # What this fixture cannot reach
//!
//! No account here has credentials and nothing listens on a port, so the
//! *successful* archive and delete of a received message - the local commit,
//! the synchronous drain, the rollback when the server refuses - are not
//! exercised. What is exercised is the ordering that protects them: the
//! refusal arrives before the store is touched. The drain itself stays covered
//! by the `pending_ops` unit tests, and a fake IMAP backend that would unlock
//! the rest is recorded as a follow-up rather than invented here.
//!
//! # The legacy suites, twinned
//!
//! `tests/cli_selector_contract.rs` keeps running unchanged against the
//! in-process path. Its assertions about these four commands are re-run here
//! through [`Slice::for_each_binary`], which runs one assertion body twice,
//! once over the oracle's output and once over the routed one, having first
//! proved the two are byte-identical: the ambiguity that lists both selectors
//! and the `--mailbox` that resolves it, the unknown key that names the
//! namespace it searched, the cross-account drafts selector that deletes from
//! its own account, the cross-account received selector that reads from its
//! own store, and the selector naming an unconfigured account.
//!
//! **What could not be twinned**, and why:
//!
//! - `a_path_is_refused_where_a_selector_is_expected` and
//!   `printed_selectors_round_trip_and_mp_path_lands_on_a_real_file` are about
//!   `mp path` and clap-level argument shapes, which the draft slice (P4-U5)
//!   owns and which never reach a mutation method.
//! - `renaming_a_draft_keeps_its_selector_working` and
//!   `an_externally_written_draft_shows_up_within_one_second` are about the
//!   drafts index and the watcher (P3b-U9, P4-U5), not about discarding.
//! - `a_send_bound_to_another_account_fails_loudly_on_a_cross_account_selector`
//!   is `mp send`, which belongs to the send slice (P4-U11).
//! - `tests/draft_integration.rs` asserts nothing about `mp delete`, so it has
//!   nothing to twin here.
//!
//! # `tests/daemon_autostart.rs`
//!
//! Its `#[ignore]`d `mp_save_writes_into_the_clients_cwd_with_the_daemon_started_from_root`
//! cannot pass as written: it runs against a bare temporary root with no
//! configuration and no store, and asks for `mp://alpha/inbox/msg@example.com`,
//! which nothing seeds. P4-U8 makes it pass by seeding
//! `support::mutation_fixture::seed` into that root and asking for
//! `mp://alpha/inbox/bericht@example.com`; the daemon-started-from-`/` half of
//! it needs no change. The same property is asserted here, from the fixture,
//! by [`mp_save_writes_into_the_clients_directory_not_the_daemons`].

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::daemon::dispatch::{CancelScope, MethodKind};
use mailypoppins::daemon::methods::draft::DRAFT_METHOD_SPECS;
use mailypoppins::daemon::methods::message::{
    MESSAGE_HANDLE_METHOD_SPECS, MESSAGE_MUTATION_METHOD_SPECS,
};
use mailypoppins::store::read;

use support::draft_fixture::Stash;
use support::mutation_fixture as fixture;
use support::parity::{
    assert_byte_identical, mp_command, mp_no_daemon, oracle_command, socket_path, DaemonFixture,
    EXIT_UNAVAILABLE, REQUIRE_ENV,
};

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// The JSON-RPC codes this file asserts on by number, because they are the
/// standard pair rather than daemon-range names.
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

/// The two methods of the slice, in the order their spec array declares them,
/// which is method-name order like every other family.
const MUTATION_METHODS: [&str; 2] = ["message.archive", "message.delete"];

/// The `draft.*` family after this slice: nine methods plus `draft.discard`,
/// still in method-name order (`demote` < `discard` < `forward`).
const DRAFT_METHODS: [&str; 10] = [
    "draft.approve",
    "draft.create",
    "draft.demote",
    "draft.discard",
    "draft.forward",
    "draft.list",
    "draft.path",
    "draft.preview",
    "draft.reply",
    "draft.validate",
];

/// Both families declare exactly those names, checked while the tree compiles:
/// an array that still holds the draft slice's nine fails here, naming the
/// constant, before a single test runs.
const _: () = assert!(DRAFT_METHOD_SPECS.len() == DRAFT_METHODS.len());
const _: () = assert!(MESSAGE_MUTATION_METHOD_SPECS.len() == MUTATION_METHODS.len());

// ---------------------------------------------------------------------------
// The refusals, verbatim
// ---------------------------------------------------------------------------

/// One message-id in two mailboxes: the resolution refuses and names both
/// candidates plus the flag that settles it.
const AMBIGUOUS_REFUSAL: &str = "shared@example.com matches 2 messages in alpha; \
     name one with --mailbox:\n  mp://alpha/archive/shared@example.com\n  \
     mp://alpha/inbox/shared@example.com";

/// A key the received index does not hold, naming the index it searched.
const RECEIVED_NOT_FOUND: &str =
    "no match for nope@example.com in the received mail index of alpha";

/// A key the drafts index does not hold, naming the index it searched.
const DRAFT_NOT_FOUND: &str = "no match for f0000000000000ff in the drafts index of alpha/drafts";

/// Deleting a queued send needs `--force`, and the sentence says why.
const APPROVED_NEEDS_FORCE: &str = "mp://alpha/drafts/b0000000000000a1 is approved, a queued \
     send; deleting it drops that send. Re-run with --force, or demote it first with \
     `mp mark-draft`.";

/// A message that resolved and has nothing to open or save.
const NO_ATTACHMENTS_REFUSAL: &str = "mp://alpha/inbox/kickoff@example.com has no attachments";

/// A configured account whose store has never been created.
const NO_STORE_YET: &str =
    "delta has no local store yet, so no received mail can be addressed; run `mp sync` first";

/// A selector naming an account the configuration does not carry.
const UNCONFIGURED_ACCOUNT: &str =
    "selector names account 'ghost', which is not configured (known: alpha, beta, delta)";

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
        Slice::start_from(None)
    }

    /// The same, with the daemon started from a chosen working directory.
    fn start_from(cwd: Option<&Path>) -> Slice {
        let tmp = TempDir::new().expect("a temporary mutation-slice root");
        fixture::seed(tmp.path());
        let daemon = DaemonFixture::start_in(tmp.path(), cwd);
        Slice { daemon, tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// The client under test, made to prove it reached the daemon.
    fn routed(&self, args: &[&str]) -> Output {
        self.daemon.mp_routed(args)
    }

    /// The client under test, standing in `cwd`.
    ///
    /// The client is the process with a meaningful working directory, so this
    /// is where a relative `-o` and the default `.` are anchored.
    fn routed_in(&self, cwd: &Path, args: &[&str]) -> Output {
        mp_command(self.root())
            .current_dir(cwd)
            .env(REQUIRE_ENV, "1")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("run routed `mp {}`: {e}", args.join(" ")))
    }

    /// The pre-daemon binary over the same root: the definition of parity.
    fn oracle(&self, args: &[&str]) -> Output {
        self.oracle_in(self.root(), args)
    }

    /// The pre-daemon binary, standing in `cwd`.
    fn oracle_in(&self, cwd: &Path, args: &[&str]) -> Output {
        oracle_command(self.root())
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("run oracle `mp {}`: {e}", args.join(" ")))
    }

    /// Run a command that hands files to the opener, with the recording `open`
    /// first on the binary's `PATH`.
    ///
    /// `routed` picks which binary: both resolve `open` through `PATH`, which
    /// is what makes the ATT-01 row comparable at all.
    fn opening(&self, routed: bool, args: &[&str]) -> Output {
        let (path, log) = fixture::opener_env(self.root());
        let mut cmd = if routed {
            let mut cmd = mp_command(self.root());
            cmd.env(REQUIRE_ENV, "1");
            cmd
        } else {
            oracle_command(self.root())
        };
        cmd.env("PATH", path)
            .env("MP_OPEN_LOG", log)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("run `mp {}` with a recording opener: {e}", args.join(" ")))
    }

    /// Assert the two binaries agree byte for byte, and run `check` over each
    /// of their outputs.
    ///
    /// This is the twin runner: a legacy assertion written once runs once
    /// against the pre-daemon path and once against the routed one. Only for
    /// commands that change nothing; a writing one goes through
    /// [`Slice::both_from_pristine`].
    fn for_each_binary(&self, args: &[&str], check: impl Fn(&str, &Output)) {
        let routed = self.routed(args);
        let direct = self.oracle(args);
        assert_byte_identical(&routed, &direct);
        check("pre-daemon", &direct);
        check("routed", &routed);
    }

    /// Run a state-changing command through both binaries from the same
    /// pristine fixture, and assert both the bytes and the state they left.
    ///
    /// The drafts directories are stashed before the first run and restored
    /// between the two, so the oracle sees exactly what the routed client saw.
    /// The store is compared rather than restored: nothing in this fixture can
    /// change it (a received mutation refuses before it commits), and a
    /// comparison that found otherwise is a failure worth reading.
    fn both_from_pristine(&self, args: &[&str]) -> (Output, Output) {
        let stash = Stash::take(self.root());
        let routed = self.routed(args);
        let after_routed = self.state();
        stash.restore();
        let direct = self.oracle(args);
        let after_direct = self.state();
        stash.restore();

        assert_byte_identical(&routed, &direct);
        assert_eq!(
            after_routed,
            after_direct,
            "`mp {}` left two different trees behind",
            args.join(" ")
        );
        (routed, direct)
    }

    /// Run a saving command through both binaries, each standing in its own
    /// empty directory, and assert the bytes and both directories agree.
    fn both_saving(&self, args: &[&str]) -> (Output, TempDir) {
        let routed_cwd = TempDir::new().expect("a temporary client directory");
        let oracle_cwd = TempDir::new().expect("a temporary client directory");
        let routed = self.routed_in(routed_cwd.path(), args);
        let direct = self.oracle_in(oracle_cwd.path(), args);

        assert_byte_identical(&routed, &direct);
        assert_eq!(
            tree(routed_cwd.path()),
            tree(oracle_cwd.path()),
            "`mp {}` wrote two different directories",
            args.join(" ")
        );
        (routed, routed_cwd)
    }

    /// Everything a mutation of this fixture can change: the drafts of both
    /// seeded accounts, keyed by `<account>/<file>` because both accounts name
    /// a file `geteilt.md`, and the mailbox contents of the first account.
    fn state(&self) -> (BTreeMap<PathBuf, Vec<u8>>, Vec<(String, i64, String)>) {
        let mut drafts = BTreeMap::new();
        for account in [fixture::ACCOUNT, fixture::OTHER_ACCOUNT] {
            for (path, bytes) in tree(&fixture::drafts_dir(self.root(), account)) {
                drafts.insert(Path::new(account).join(path), bytes);
            }
        }
        (drafts, self.mailboxes())
    }

    /// `(mailbox, uid, message_id)` for every row of the first account, in
    /// mailbox then uid order.
    fn mailboxes(&self) -> Vec<(String, i64, String)> {
        let store = fixture::store(self.root(), fixture::ACCOUNT);
        let mut rows = Vec::new();
        for mailbox in ["archive", "inbox", "sent", "Team/Reports"] {
            let listed = read::list_mailbox(&store, fixture::ACCOUNT, mailbox)
                .unwrap_or_else(|e| panic!("list {mailbox}: {e:#}"));
            for row in listed {
                rows.push((row.mailbox.clone(), row.uid, row.message_id.clone()));
            }
        }
        rows.sort();
        rows
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

/// Every file under `dir`, keyed by its path relative to `dir`.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    collect(dir, dir, &mut out);
    out
}

fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if let Ok(bytes) = fs::read(&path) {
            let key = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            out.insert(key, bytes);
        }
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

/// Nothing a mutation answers may name a file the client cannot open.
///
/// A discard answers about a draft, whose path is the one thing a client is
/// allowed to know (the draft slice pins that); everything else here answers
/// with selectors, and a store or a blob directory may never appear.
fn assert_store_free(what: &str, value: &Value) {
    let text = value.to_string();
    for needle in ["store.sqlite3", "/blobs/", "/runtime/"] {
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

// ---------------------------------------------------------------------------
// 1. The methods themselves
// ---------------------------------------------------------------------------

/// The two declarations, and the four facts each one fixes: the wire name, the
/// kind, the first protocol version and what a disconnect does to it.
#[test]
fn the_mutation_slice_declares_two_durable_commands() {
    let names: Vec<&str> = MESSAGE_MUTATION_METHOD_SPECS
        .iter()
        .map(|spec| spec.name)
        .collect();
    assert_eq!(
        names, MUTATION_METHODS,
        "the slice serves exactly these two"
    );

    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the array is in method-name order");

    for spec in MESSAGE_MUTATION_METHOD_SPECS {
        assert_eq!(
            spec.kind,
            MethodKind::Command,
            "{} changes a row at once, so its answer carries a revision",
            spec.name
        );
        assert_eq!(spec.since, 1, "{} is served from protocol 1", spec.name);
        assert_eq!(
            spec.cancel_scope,
            CancelScope::Durable,
            "{} commits a row change and an owed server op together; a disconnect may not \
             tear that down",
            spec.name
        );
    }
}

/// `draft.discard` joins the family it belongs to rather than starting one:
/// ten names, still in method-name order, and the new one is a durable
/// command like the four writers beside it.
#[test]
fn draft_discard_is_the_tenth_method_of_the_draft_family() {
    let names: Vec<&str> = DRAFT_METHOD_SPECS.iter().map(|spec| spec.name).collect();
    assert_eq!(names, DRAFT_METHODS);

    let spec = DRAFT_METHOD_SPECS
        .iter()
        .find(|spec| spec.name == "draft.discard")
        .expect("draft.discard is declared");
    assert_eq!(
        spec.kind,
        MethodKind::Command,
        "a discard removes a file, which is a state change a client caches"
    );
    assert_eq!(spec.since, 1);
    assert_eq!(
        spec.cancel_scope,
        CancelScope::Durable,
        "a half-removed draft is exactly what this family must never produce"
    );
}

/// The materialiser this slice leans on is the one P3b-U12 shipped, unchanged
/// in kind and lifetime: the daemon prepares the file and only the client's
/// own process can open it.
#[test]
fn the_attachment_materialiser_stays_a_durable_client_integration() {
    let spec = MESSAGE_HANDLE_METHOD_SPECS
        .iter()
        .find(|spec| spec.name == "message.materialise_attachment")
        .expect("message.materialise_attachment is declared");
    assert_eq!(spec.kind, MethodKind::ClientIntegration);
    assert_eq!(spec.since, 1);
    assert_eq!(
        spec.cancel_scope,
        CancelScope::Durable,
        "a viewer holding an open file may not lose it because a socket went away"
    );
}

/// A method may not be served without being advertised: the handshake derives
/// the capability list from the dispatcher, so the names appearing there is
/// what says they are registered rather than merely declared.
#[tokio::test]
async fn the_daemon_advertises_the_mutation_methods() {
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

    for method in ["message.archive", "message.delete", "draft.discard"] {
        assert!(
            result.capabilities.iter().any(|c| c == method),
            "{method} is served, so it is advertised: {:?}",
            result.capabilities
        );
    }
}

// ---------------------------------------------------------------------------
// 2. `message.archive` and `message.delete`
// ---------------------------------------------------------------------------

/// Both methods address a message the way `message.get` does, and every way of
/// addressing nothing is the caller's parameter being wrong.
#[tokio::test]
async fn the_received_mutations_are_addressed_like_message_get() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for method in MUTATION_METHODS {
        let both = call_err(
            &mut conn,
            method,
            json!({"account": fixture::ACCOUNT, "id": "inbox/1", "selector": fixture::SHARED}),
        )
        .await;
        assert_eq!(both.code, INVALID_PARAMS);
        assert!(
            both.message.contains("send exactly one"),
            "{method}: {}",
            both.message
        );

        let neither = call_err(&mut conn, method, json!({"account": fixture::ACCOUNT})).await;
        assert_eq!(neither.code, INVALID_PARAMS);
        assert!(
            neither.message.contains("send exactly one"),
            "{method}: {}",
            neither.message
        );

        let malformed = call_err(
            &mut conn,
            method,
            json!({"account": fixture::ACCOUNT, "id": "inbox"}),
        )
        .await;
        assert_eq!(malformed.code, INVALID_PARAMS);

        let missing = call_err(
            &mut conn,
            method,
            json!({"account": fixture::ACCOUNT, "id": "inbox/9999"}),
        )
        .await;
        assert_eq!(missing.code, INVALID_PARAMS);

        let unknown_key = call_err(
            &mut conn,
            method,
            json!({"account": fixture::ACCOUNT, "selector": fixture::UNKNOWN_MESSAGE}),
        )
        .await;
        assert_eq!(unknown_key.code, INVALID_PARAMS);
        assert_eq!(
            unknown_key.message, RECEIVED_NOT_FOUND,
            "{method} refuses in the command's own words"
        );

        let ambiguous = call_err(
            &mut conn,
            method,
            json!({"account": fixture::ACCOUNT, "selector": fixture::SHARED}),
        )
        .await;
        assert_eq!(ambiguous.code, INVALID_PARAMS);
        assert_eq!(
            ambiguous.message, AMBIGUOUS_REFUSAL,
            "{method} lists both candidates rather than guessing"
        );

        let unknown_account = call_err(
            &mut conn,
            method,
            json!({"account": fixture::UNKNOWN_ACCOUNT, "id": "inbox/1"}),
        )
        .await;
        assert_eq!(unknown_account.code, ErrorCode::AccountUnknown.code());
        assert_eq!(
            data_of(&unknown_account)["account"],
            fixture::UNKNOWN_ACCOUNT
        );

        let storeless = call_err(
            &mut conn,
            method,
            json!({"account": fixture::STORELESS_ACCOUNT, "id": "inbox/1"}),
        )
        .await;
        assert_eq!(storeless.code, ErrorCode::AccountNotReady.code());
    }
}

/// `mailbox` narrows a selector exactly as `--mailbox` does, which is what
/// turns the ambiguity above into an answer about one message.
#[tokio::test]
async fn a_mailbox_narrows_an_ambiguous_selector() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    // Narrowed, the resolution succeeds and the refusal that follows is about
    // this account's credentials rather than about the address.
    let refused = call_err(
        &mut conn,
        "message.delete",
        json!({
            "account": fixture::ACCOUNT,
            "selector": fixture::SHARED,
            "mailbox": "inbox",
        }),
    )
    .await;
    assert_eq!(
        refused.message,
        fixture::CREDENTIALS_REFUSAL,
        "the address resolved; what failed is the backend"
    );
}

/// An account with no credentials cannot reach its server, and the refusal
/// arrives **before** the store is touched: the row is still where it was.
///
/// That ordering is the whole safety property of this slice over a fixture
/// with no server. A method that committed the local half first would leave a
/// message archived locally and unarchived on the server, with the owed op
/// queued behind a credential the user has not entered yet.
#[tokio::test]
async fn a_received_mutation_refuses_before_it_touches_the_store() {
    let slice = Slice::start();
    let before = slice.mailboxes();
    let mut conn = slice.connect().await;

    for method in MUTATION_METHODS {
        let refused = call_err(
            &mut conn,
            method,
            json!({"account": fixture::ACCOUNT, "id": "inbox/1"}),
        )
        .await;
        assert_eq!(
            refused.code, INTERNAL_ERROR,
            "{method}: a missing secret is neither a bad parameter nor an unknown account"
        );
        assert_eq!(
            refused.message,
            fixture::CREDENTIALS_REFUSAL,
            "{method} reports the sentence the user has to act on"
        );
        assert_eq!(
            data_of(&refused)["account"],
            fixture::ACCOUNT,
            "{method} says which account could not be reached"
        );
        assert_eq!(
            slice.mailboxes(),
            before,
            "{method} changed the store before it found out it could not finish"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. `draft.discard`
// ---------------------------------------------------------------------------

/// One draft, addressed by id or by selector, gone from the directory, and the
/// answer says which one and what state it was in.
#[tokio::test]
async fn draft_discard_removes_one_draft_and_reports_it() {
    let slice = Slice::start();
    let stash = Stash::take(slice.root());
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "draft.discard",
        json!({"account": fixture::ACCOUNT, "id": fixture::VALID_DRAFT}),
    )
    .await;
    assert_store_free("draft.discard", &result);
    assert_eq!(result["account"], fixture::ACCOUNT);
    assert_eq!(result["id"], fixture::VALID_DRAFT);
    assert_eq!(
        result["selector"],
        fixture::draft_selector(fixture::ACCOUNT, fixture::VALID_DRAFT)
    );
    assert_eq!(
        result["status"], "draft",
        "the answer says what was discarded, which is the state it was in"
    );
    assert!(
        !fixture::drafts_dir(slice.root(), fixture::ACCOUNT)
            .join("angebot.md")
            .exists(),
        "the file is gone"
    );

    // Twice is a refusal, not a silent success: the second call is addressing
    // something that is not there.
    let again = call_err(
        &mut conn,
        "draft.discard",
        json!({"account": fixture::ACCOUNT, "id": fixture::VALID_DRAFT}),
    )
    .await;
    assert_eq!(again.code, INVALID_PARAMS);

    stash.restore();

    // The selector form addresses the same draft.
    let result = call(
        &mut conn,
        "draft.discard",
        json!({
            "account": fixture::ACCOUNT,
            "selector": fixture::draft_selector(fixture::ACCOUNT, fixture::VALID_DRAFT),
        }),
    )
    .await;
    assert_eq!(result["id"], fixture::VALID_DRAFT);
    stash.restore();
}

/// An approved draft is a queued send: it needs `force`, the refusal is the
/// command's own sentence, and the file survives the refusal.
#[tokio::test]
async fn draft_discard_refuses_an_approved_draft_without_force() {
    let slice = Slice::start();
    let stash = Stash::take(slice.root());
    let mut conn = slice.connect().await;
    let path = fixture::drafts_dir(slice.root(), fixture::ACCOUNT).join("freigabe.md");

    let refused = call_err(
        &mut conn,
        "draft.discard",
        json!({"account": fixture::ACCOUNT, "id": fixture::APPROVED_DRAFT}),
    )
    .await;
    assert_eq!(refused.code, INVALID_PARAMS);
    assert_eq!(refused.message, APPROVED_NEEDS_FORCE);
    assert!(path.exists(), "a refused discard removes nothing");

    let result = call(
        &mut conn,
        "draft.discard",
        json!({"account": fixture::ACCOUNT, "id": fixture::APPROVED_DRAFT, "force": true}),
    )
    .await;
    assert_eq!(result["status"], "approved");
    assert!(!path.exists(), "forced, it goes");
    stash.restore();
}

/// A sent draft needs no force: retiring one is what the sweep is for, and a
/// send that already happened cannot be dropped by deleting its record.
#[tokio::test]
async fn draft_discard_takes_a_sent_draft_without_force() {
    let slice = Slice::start();
    let stash = Stash::take(slice.root());
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "draft.discard",
        json!({"account": fixture::ACCOUNT, "id": fixture::SENT_DRAFT}),
    )
    .await;
    assert_eq!(result["status"], "sent");
    assert!(!fixture::drafts_dir(slice.root(), fixture::ACCOUNT)
        .join("verschickt.md")
        .exists());
    stash.restore();
}

/// An id nothing resolves to is the command's own not-found sentence, and an
/// unknown account is the family's `-32005`.
#[tokio::test]
async fn draft_discard_refuses_what_it_cannot_find() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let missing = call_err(
        &mut conn,
        "draft.discard",
        json!({"account": fixture::ACCOUNT, "id": fixture::UNKNOWN_DRAFT}),
    )
    .await;
    assert_eq!(missing.code, INVALID_PARAMS);
    assert_eq!(missing.message, DRAFT_NOT_FOUND);
    assert_eq!(data_of(&missing)["id"], fixture::UNKNOWN_DRAFT);

    let unknown = call_err(
        &mut conn,
        "draft.discard",
        json!({"account": fixture::UNKNOWN_ACCOUNT, "id": fixture::VALID_DRAFT}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());

    let unaddressed = call_err(
        &mut conn,
        "draft.discard",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    assert_eq!(unaddressed.code, INVALID_PARAMS);
}

/// The `--sent` sweep: one parameter of the same method, no selector, a count
/// of what went and a list of what stayed.
#[tokio::test]
async fn draft_discard_sweeps_every_sent_draft_of_one_account() {
    let slice = Slice::start();
    let stash = Stash::take(slice.root());
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "draft.discard",
        json!({"account": fixture::ACCOUNT, "sent": true}),
    )
    .await;
    assert_eq!(result["account"], fixture::ACCOUNT);
    assert_eq!(result["cleared"], 1, "the fixture holds one sent draft");
    assert_eq!(
        result["kept"],
        json!([]),
        "nothing refused to go, so nothing is listed"
    );
    let left = tree(&fixture::drafts_dir(slice.root(), fixture::ACCOUNT));
    assert!(
        !left.contains_key(Path::new("verschickt.md")),
        "the sent draft is gone: {left:?}"
    );
    assert!(
        left.contains_key(Path::new("angebot.md")),
        "a draft that was not sent is untouched: {left:?}"
    );
    stash.restore();

    // An account with no sent draft sweeps nothing and says so with a number,
    // not with a refusal.
    let empty = call(
        &mut conn,
        "draft.discard",
        json!({"account": fixture::OTHER_ACCOUNT, "sent": true}),
    )
    .await;
    assert_eq!(empty["cleared"], 0);

    // A sweep names no draft; a caller who named one disagrees with themselves.
    for extra in [
        json!({"account": fixture::ACCOUNT, "sent": true, "id": fixture::SENT_DRAFT}),
        json!({"account": fixture::ACCOUNT, "sent": true, "force": true}),
    ] {
        let refused = call_err(&mut conn, "draft.discard", extra).await;
        assert_eq!(refused.code, INVALID_PARAMS);
    }
}

// ---------------------------------------------------------------------------
// 4. `message.materialise_attachment`
// ---------------------------------------------------------------------------

/// The additive half of the contract: a selector addresses a part, the answer
/// carries the file's own name, and the file is where the protocol says.
#[tokio::test]
async fn an_attachment_materialises_from_a_selector_under_the_runtime_directory() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for (part, (name, bytes)) in fixture::ATTACHMENT_FILES.iter().enumerate() {
        let result = call(
            &mut conn,
            "message.materialise_attachment",
            json!({
                "account": fixture::ACCOUNT,
                "selector": fixture::WITH_ATTACHMENTS,
                "part": part,
            }),
        )
        .await;
        assert_eq!(result["name"], *name, "the answer names the file it wrote");
        assert_eq!(result["bytes"], bytes.len() as u64);
        assert!(
            result["expires_at"]
                .as_str()
                .is_some_and(|at| at.contains('T')),
            "a handle carries the RFC 3339 instant it dies at: {result}"
        );

        let path = PathBuf::from(result["path"].as_str().expect("a handle names its file"));
        assert!(path.is_absolute(), "{} is absolute", path.display());
        assert!(
            path.starts_with(slice.root().join("runtime").join("handles")),
            "{} is under the daemon's runtime directory",
            path.display()
        );
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(*name));
        assert_eq!(
            fs::read(&path).expect("the materialised file is readable"),
            bytes.to_vec(),
            "the file holds the stored blob"
        );

        let released = call(
            &mut conn,
            "message.release_handle",
            json!({"handle": result["handle"]}),
        )
        .await;
        assert_eq!(released, json!({}));
    }
}

/// The id form still addresses a part: this slice adds a form, it does not
/// replace one, and a P3b client keeps working.
#[tokio::test]
async fn an_attachment_still_materialises_from_a_mailbox_and_uid() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "message.materialise_attachment",
        json!({"account": fixture::ACCOUNT, "id": "inbox/1", "part": 0}),
    )
    .await;
    assert_eq!(result["name"], fixture::ATTACHMENT_FILES[0].0);
}

/// Two parts sent under one name come back as two handles carrying that one
/// name: the daemon writes each into its own directory and never renames, and
/// the `_1` rule that turns them into two files belongs where the names become
/// paths, which is the client.
#[tokio::test]
async fn two_parts_under_one_name_are_two_handles_with_that_name() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let mut paths = Vec::new();
    for (part, expected) in [(0usize, "first part"), (1usize, "second part")] {
        let result = call(
            &mut conn,
            "message.materialise_attachment",
            json!({
                "account": fixture::ACCOUNT,
                "selector": fixture::TWO_PARTS,
                "part": part,
            }),
        )
        .await;
        assert_eq!(
            result["name"], "report.pdf",
            "the daemon hands back the name the sender chose"
        );
        let path = PathBuf::from(result["path"].as_str().expect("a path"));
        assert_eq!(fs::read_to_string(&path).expect("readable"), expected);
        paths.push(path);
    }
    assert_ne!(
        paths[0], paths[1],
        "one directory per handle is what keeps two same-named parts apart"
    );
}

/// Everything a caller can get wrong about a part is `-32602`, and an
/// ambiguous selector is refused here in the same words as everywhere else.
#[tokio::test]
async fn materialising_refuses_a_part_that_is_not_there() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let past_the_end = call_err(
        &mut conn,
        "message.materialise_attachment",
        json!({
            "account": fixture::ACCOUNT,
            "selector": fixture::WITH_ATTACHMENTS,
            "part": 2,
        }),
    )
    .await;
    assert_eq!(past_the_end.code, INVALID_PARAMS);

    let none_at_all = call_err(
        &mut conn,
        "message.materialise_attachment",
        json!({
            "account": fixture::ACCOUNT,
            "selector": fixture::NO_ATTACHMENTS,
            "part": 0,
        }),
    )
    .await;
    assert_eq!(none_at_all.code, INVALID_PARAMS);

    let ambiguous = call_err(
        &mut conn,
        "message.materialise_attachment",
        json!({"account": fixture::ACCOUNT, "selector": fixture::SHARED, "part": 0}),
    )
    .await;
    assert_eq!(ambiguous.code, INVALID_PARAMS);
    assert_eq!(ambiguous.message, AMBIGUOUS_REFUSAL);
}

// ---------------------------------------------------------------------------
// 5. Parity: `mp archive` and `mp delete`
// ---------------------------------------------------------------------------

/// Every way `mp archive` can refuse over this fixture, byte for byte, with
/// the store untouched on both sides.
#[test]
fn mp_archive_refuses_exactly_as_the_pre_daemon_binary_did() {
    let slice = Slice::start();
    let before = slice.mailboxes();

    let (routed, _) = slice.both_from_pristine(&["archive", fixture::WITH_ATTACHMENTS]);
    assert_refused(&routed, fixture::CREDENTIALS_REFUSAL);
    assert_eq!(
        slice.mailboxes(),
        before,
        "a credential-less archive moves nothing"
    );

    let (routed, _) = slice.both_from_pristine(&["archive", fixture::UNKNOWN_MESSAGE]);
    assert_refused(&routed, RECEIVED_NOT_FOUND);

    let (routed, _) = slice.both_from_pristine(&["archive", fixture::SHARED]);
    assert_refused(&routed, AMBIGUOUS_REFUSAL);

    let (routed, _) = slice.both_from_pristine(&["archive", "--mailbox", "inbox", fixture::SHARED]);
    assert_refused(&routed, fixture::CREDENTIALS_REFUSAL);

    let (routed, _) = slice.both_from_pristine(&[
        "-A",
        fixture::STORELESS_ACCOUNT,
        "archive",
        fixture::UNKNOWN_MESSAGE,
    ]);
    assert_refused(&routed, NO_STORE_YET);

    assert_eq!(slice.mailboxes(), before, "no row moved anywhere");
}

/// `mp delete` of a received message: the same refusals, and the same
/// untouched store.
#[test]
fn mp_delete_of_received_mail_refuses_exactly_as_before() {
    let slice = Slice::start();
    let before = slice.mailboxes();

    let (routed, _) = slice.both_from_pristine(&["delete", fixture::WITH_ATTACHMENTS]);
    assert_refused(&routed, fixture::CREDENTIALS_REFUSAL);

    let (routed, _) = slice.both_from_pristine(&["delete", fixture::UNKNOWN_MESSAGE]);
    assert_refused(&routed, RECEIVED_NOT_FOUND);

    let (routed, _) = slice.both_from_pristine(&["delete", fixture::SHARED]);
    assert_refused(&routed, AMBIGUOUS_REFUSAL);

    assert_eq!(
        slice.mailboxes(),
        before,
        "a credential-less delete drops nothing"
    );
}

/// `mp delete` of a draft: the successes and the refusal, with the drafts
/// directory both binaries left behind compared file by file.
#[test]
fn mp_delete_of_a_draft_matches_byte_for_byte_and_file_for_file() {
    let slice = Slice::start();

    let id = fixture::VALID_DRAFT;
    let (routed, _) = slice.both_from_pristine(&["delete", &format!("drafts/{id}")]);
    assert_eq!(routed.status.code(), Some(0));
    assert_eq!(
        stdout(&routed),
        format!(
            "\u{2713} deleted {}\n",
            fixture::draft_selector(fixture::ACCOUNT, id)
        )
    );

    let approved = fixture::APPROVED_DRAFT;
    let (routed, _) = slice.both_from_pristine(&["delete", &format!("drafts/{approved}")]);
    assert_refused(&routed, APPROVED_NEEDS_FORCE);

    let (routed, _) =
        slice.both_from_pristine(&["delete", "--force", &format!("drafts/{approved}")]);
    assert_eq!(routed.status.code(), Some(0));

    let sent = fixture::SENT_DRAFT;
    let (routed, _) = slice.both_from_pristine(&["delete", &format!("drafts/{sent}")]);
    assert_eq!(
        routed.status.code(),
        Some(0),
        "a sent draft goes without --force"
    );

    let unknown = format!("drafts/{}", fixture::UNKNOWN_DRAFT);
    let (routed, _) = slice.both_from_pristine(&["delete", &unknown]);
    assert_refused(&routed, DRAFT_NOT_FOUND);
}

/// The `--sent` sweep, on an account that has one and on an account that has
/// none.
#[test]
fn mp_delete_sent_sweeps_identically() {
    let slice = Slice::start();

    let (routed, _) = slice.both_from_pristine(&["delete", "--sent"]);
    assert_eq!(
        stdout(&routed),
        format!("\u{2713} cleared 1 sent draft on {}\n", fixture::ACCOUNT)
    );

    let (routed, _) = slice.both_from_pristine(&["-A", fixture::OTHER_ACCOUNT, "delete", "--sent"]);
    assert_eq!(
        stdout(&routed),
        format!("No sent drafts to clear on {}\n", fixture::OTHER_ACCOUNT)
    );
}

// ---------------------------------------------------------------------------
// 6. Parity: `mp save`
// ---------------------------------------------------------------------------

/// The default destination is the directory the client is standing in, and the
/// line printed is the spelling the user gave, not the absolute form that
/// crossed the socket.
#[test]
fn mp_save_writes_the_clients_directory_and_prints_the_users_spelling() {
    let slice = Slice::start();

    let (routed, cwd) = slice.both_saving(&["save", fixture::WITH_ATTACHMENTS]);
    assert_eq!(routed.status.code(), Some(0));
    assert_eq!(
        stdout(&routed),
        "\u{2713} ./notes.pdf\n\u{2713} ./agenda.txt\n",
        "the default destination prints as the user's `.`"
    );
    for (name, bytes) in fixture::ATTACHMENT_FILES {
        assert_eq!(
            fs::read(cwd.path().join(name)).expect("the saved file is there"),
            bytes.to_vec(),
            "{name} holds the stored blob"
        );
    }
}

/// A relative `-o` is anchored to the client's directory and printed as it was
/// typed.
#[test]
fn mp_save_into_a_relative_output_directory() {
    let slice = Slice::start();

    let (routed, cwd) = slice.both_saving(&["save", "-o", "out", fixture::WITH_ATTACHMENTS]);
    assert_eq!(
        stdout(&routed),
        "\u{2713} out/notes.pdf\n\u{2713} out/agenda.txt\n"
    );
    assert!(
        cwd.path().join("out").join("notes.pdf").is_file(),
        "the directory was created under the client's cwd"
    );
}

/// An absolute `-o` is printed as given, which is the one spelling both
/// binaries can share a directory for.
#[test]
fn mp_save_into_an_absolute_output_directory() {
    let slice = Slice::start();
    let dest = TempDir::new().expect("a destination");
    let arg = dest.path().display().to_string();

    let routed = slice.routed(&["save", "-o", &arg, fixture::WITH_ATTACHMENTS]);
    let after_routed = tree(dest.path());
    let direct = slice.oracle(&["save", "-o", &arg, fixture::WITH_ATTACHMENTS]);
    assert_byte_identical(&routed, &direct);
    assert_eq!(
        after_routed,
        tree(dest.path()),
        "the second run rewrote the same two files"
    );
    assert_eq!(
        stdout(&routed),
        format!("\u{2713} {arg}/notes.pdf\n\u{2713} {arg}/agenda.txt\n")
    );
}

/// Two parts sent under one name become two files, and the `_1` is the
/// client's, applied within one call rather than against what is on disk: a
/// second save writes the same two names again.
#[test]
fn mp_save_disambiguates_two_parts_that_share_a_name() {
    let slice = Slice::start();

    let (routed, cwd) = slice.both_saving(&["save", fixture::TWO_PARTS]);
    assert_eq!(
        stdout(&routed),
        "\u{2713} ./report.pdf\n\u{2713} ./report_1.pdf\n"
    );
    for (name, bytes) in fixture::TWO_PARTS_FILES {
        assert_eq!(
            fs::read(cwd.path().join(name)).expect("the saved file is there"),
            bytes.to_vec()
        );
    }

    // Again, into a directory that already holds both: the same two names, not
    // `report_2.pdf`.
    let second = slice.routed_in(cwd.path(), &["save", fixture::TWO_PARTS]);
    assert_eq!(stdout(&second), stdout(&routed));
    assert_eq!(
        tree(cwd.path()).keys().cloned().collect::<Vec<_>>(),
        vec![PathBuf::from("report.pdf"), PathBuf::from("report_1.pdf")],
        "saving twice overwrites rather than accumulating"
    );
}

/// The refusals of `mp save`, which are the resolution's own and the
/// message's.
#[test]
fn mp_save_refuses_exactly_as_before() {
    let slice = Slice::start();

    let (routed, _) = slice.both_saving(&["save", fixture::NO_ATTACHMENTS]);
    assert_refused(&routed, NO_ATTACHMENTS_REFUSAL);

    let (routed, _) = slice.both_saving(&["save", fixture::UNKNOWN_MESSAGE]);
    assert_refused(&routed, RECEIVED_NOT_FOUND);

    let (routed, _) = slice.both_saving(&["save", fixture::SHARED]);
    assert_refused(&routed, AMBIGUOUS_REFUSAL);
}

/// The daemon's own working directory carries no meaning: started from `/`, it
/// still saves into the directory the client was standing in.
///
/// This is the property `tests/daemon_autostart.rs` reaches for in its
/// `#[ignore]`d twin, asserted here over a fixture that actually holds a
/// message with an attachment.
#[test]
fn mp_save_writes_into_the_clients_directory_not_the_daemons() {
    let slice = Slice::start_from(Some(Path::new("/")));
    let standing_in = TempDir::new().expect("a client directory");

    let out = slice.routed_in(standing_in.path(), &["save", fixture::WITH_ATTACHMENTS]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        tree(standing_in.path()).keys().cloned().collect::<Vec<_>>(),
        vec![PathBuf::from("agenda.txt"), PathBuf::from("notes.pdf")],
        "the attachments landed where the client was standing"
    );
}

// ---------------------------------------------------------------------------
// 7. Parity: `mp open`
// ---------------------------------------------------------------------------

/// The refusals of `mp open` never reach an opener, so they are literal.
#[test]
fn mp_open_refuses_exactly_as_before() {
    let slice = Slice::start();

    for (args, sentence) in [
        (
            vec!["open", fixture::NO_ATTACHMENTS],
            NO_ATTACHMENTS_REFUSAL,
        ),
        (vec!["open", fixture::UNKNOWN_MESSAGE], RECEIVED_NOT_FOUND),
        (vec!["open", fixture::SHARED], AMBIGUOUS_REFUSAL),
    ] {
        let routed = slice.opening(true, &args);
        let direct = slice.opening(false, &args);
        assert_byte_identical(&routed, &direct);
        assert_refused(&routed, sentence);
        assert!(
            fixture::opened_paths(slice.root()).is_empty(),
            "a refusal hands nothing to the opener"
        );
    }
}

/// The one masked row: both binaries open the same files in the same order and
/// print one line each, and only the directory the files were materialised
/// into differs - the pre-daemon binary's private temp directory against the
/// daemon's runtime handle directory.
///
/// The bytes handed over are the stored blob on both sides, which is the fact
/// the mask cannot hide.
#[test]
fn mp_open_hands_the_same_files_to_the_client_side_opener() {
    let slice = Slice::start();
    let args = ["open", fixture::WITH_ATTACHMENTS];

    fixture::clear_opened(slice.root());
    let routed = slice.opening(true, &args);
    let routed_opened = fixture::opened_paths(slice.root());

    fixture::clear_opened(slice.root());
    let direct = slice.opening(false, &args);
    let direct_opened = fixture::opened_paths(slice.root());

    assert_eq!(routed.status.code(), Some(0));
    assert_eq!(
        routed.status.code(),
        direct.status.code(),
        "the exit codes agree"
    );
    assert_eq!(stderr(&routed), stderr(&direct), "the stderr agrees");
    assert_eq!(
        mask_paths(&stdout(&routed)),
        mask_paths(&stdout(&direct)),
        "one `opened` line per attachment, in the message's own order, and only the \
         directory differs"
    );

    let names: Vec<String> = routed_opened
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        fixture::ATTACHMENT_FILES
            .iter()
            .map(|(name, _)| name.to_string())
            .collect::<Vec<_>>(),
        "the client opened both parts, in the message's order"
    );
    assert_eq!(
        names.len(),
        direct_opened.len(),
        "both binaries opened the same number of files"
    );

    for (path, (name, bytes)) in routed_opened.iter().zip(fixture::ATTACHMENT_FILES) {
        assert!(
            path.starts_with(slice.root().join("runtime")),
            "{} is a daemon-materialised file",
            path.display()
        );
        assert_eq!(
            fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
            bytes.to_vec(),
            "{name} was handed over with the stored bytes"
        );
    }
}

/// Replace every absolute path in an `opened` line with its file name, so two
/// materialisation directories compare equal and nothing else does.
fn mask_paths(text: &str) -> String {
    text.lines()
        .map(|line| match line.rsplit_once('/') {
            Some((head, name)) if head.contains(" /") => {
                let prefix = head.split_once(" /").expect("a rooted path").0;
                format!("{prefix} <materialised>/{name}")
            }
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// 8. Determinism, and what proves routing
// ---------------------------------------------------------------------------

/// Two routed runs of the same command produce the same bytes.
///
/// `mp open` is not here: a handle id is minted per call, so two successful
/// runs print two different paths, which is what
/// [`mp_open_hands_the_same_files_to_the_client_side_opener`] records instead.
#[test]
fn two_routed_runs_agree() {
    let slice = Slice::start();
    let cwd = TempDir::new().expect("a client directory");
    let unknown_draft = format!("drafts/{}", fixture::UNKNOWN_DRAFT);

    for args in [
        vec!["save", fixture::WITH_ATTACHMENTS],
        vec!["save", "-o", "out", fixture::TWO_PARTS],
        vec!["archive", fixture::WITH_ATTACHMENTS],
        vec!["delete", fixture::UNKNOWN_MESSAGE],
        vec!["delete", &unknown_draft],
        vec!["-A", fixture::OTHER_ACCOUNT, "delete", "--sent"],
    ] {
        let first = slice.routed_in(cwd.path(), &args);
        let second = slice.routed_in(cwd.path(), &args);
        assert_byte_identical(&second, &first);
    }
}

/// With nothing listening and auto-start off, no command of this slice can
/// answer at all: it exits 4, the code plan section 3.0 fixes for "daemon
/// unavailable".
///
/// This is the proof `MAILYPOPPINS_DAEMON_REQUIRE` cannot give: that variable
/// is checked at the end of `main`, so a command that returns early would
/// escape it, while a command with no in-process path left cannot produce an
/// answer without a socket.
#[test]
fn no_mutation_command_can_still_answer_without_a_daemon() {
    let tmp = TempDir::new().expect("a temporary root");
    fixture::seed(tmp.path());
    let valid_draft = format!("drafts/{}", fixture::VALID_DRAFT);

    for args in [
        vec!["archive", fixture::WITH_ATTACHMENTS],
        vec!["delete", fixture::WITH_ATTACHMENTS],
        vec!["delete", "--sent"],
        vec!["delete", &valid_draft],
        vec!["open", fixture::WITH_ATTACHMENTS],
        vec!["save", fixture::WITH_ATTACHMENTS],
    ] {
        let out = mp_no_daemon(&args, tmp.path());
        assert_eq!(
            out.status.code(),
            Some(EXIT_UNAVAILABLE),
            "`mp {}` answered without a daemon:\n{}\n{}",
            args.join(" "),
            stdout(&out),
            stderr(&out)
        );
    }
    assert!(
        fixture::drafts_dir(tmp.path(), fixture::ACCOUNT)
            .join("angebot.md")
            .exists(),
        "a command that could not reach a daemon changed nothing"
    );
}

// ---------------------------------------------------------------------------
// 9. The legacy suite, twinned
// ---------------------------------------------------------------------------

/// `cli_selector_contract::an_ambiguous_key_lists_both_selectors_and_mailbox_resolves_it`,
/// run against both binaries: the ambiguity names the flag that settles it and
/// lists both fully qualified selectors, and either `--mailbox` or the
/// selector's own mailbox segment resolves it.
#[test]
fn twin_an_ambiguous_key_lists_both_selectors_and_mailbox_resolves_it() {
    let slice = Slice::start();

    slice.for_each_binary(&["save", fixture::SHARED], |which, out| {
        let text = stderr(out);
        assert!(
            text.contains("--mailbox"),
            "{which}: the ambiguity names the flag that resolves it: {text}"
        );
        for mailbox in ["inbox", "archive"] {
            let candidate = fixture::message_selector(fixture::ACCOUNT, mailbox, fixture::SHARED);
            assert!(
                text.contains(&candidate),
                "{which}: the ambiguity must list {candidate}: {text}"
            );
        }
    });

    slice.for_each_binary(
        &["save", "--mailbox", "inbox", fixture::SHARED],
        |which, out| {
            let text = stderr(out);
            assert!(
                text.contains("has no attachments"),
                "{which}: --mailbox must resolve the ambiguity: {text}"
            );
            assert!(
                text.contains(&fixture::message_selector(
                    fixture::ACCOUNT,
                    "inbox",
                    fixture::SHARED
                )),
                "{which}: the resolved selector is reported in full: {text}"
            );
        },
    );

    let qualified = format!("archive/{}", fixture::SHARED);
    slice.for_each_binary(&["save", &qualified], |which, out| {
        let text = stderr(out);
        assert!(text.contains("has no attachments"), "{which}: {text}");
        assert!(
            text.contains(&fixture::message_selector(
                fixture::ACCOUNT,
                "archive",
                fixture::SHARED
            )),
            "{which}: {text}"
        );
    });
}

/// `cli_selector_contract::an_unknown_key_names_the_namespace_it_searched`,
/// for the received half: "no such message" stays distinguishable from "you
/// asked the wrong index".
#[test]
fn twin_an_unknown_key_names_the_namespace_it_searched() {
    let slice = Slice::start();

    slice.for_each_binary(&["save", fixture::UNKNOWN_MESSAGE], |which, out| {
        assert!(
            stderr(out).contains("received mail"),
            "{which}: {}",
            stderr(out)
        );
    });
    let unknown_draft = format!("drafts/{}", fixture::UNKNOWN_DRAFT);
    slice.for_each_binary(&["delete", &unknown_draft], |which, out| {
        assert!(stderr(out).contains("drafts"), "{which}: {}", stderr(out));
    });
}

/// `cli_selector_contract::a_cross_account_drafts_selector_deletes_from_its_own_account`:
/// with no `-A`, the selector's account is the only thing naming `beta`, and
/// the identically-keyed `alpha` draft must survive.
#[test]
fn twin_a_cross_account_drafts_selector_deletes_from_its_own_account() {
    let slice = Slice::start();
    let selector = fixture::draft_selector(fixture::OTHER_ACCOUNT, fixture::SHARED_DRAFT);

    let stash = Stash::take(slice.root());
    let routed = slice.routed(&["delete", &selector]);
    let after_routed = slice.state();
    stash.restore();
    let direct = slice.oracle(&["delete", &selector]);
    let after_direct = slice.state();
    stash.restore();

    assert_byte_identical(&routed, &direct);
    assert_eq!(after_routed, after_direct);
    assert!(stdout(&routed).contains(&selector), "{}", stdout(&routed));

    // The state each binary left: `beta`'s file gone, `alpha`'s identically
    // keyed one untouched. The map is keyed by account precisely because both
    // files are called `geteilt.md`.
    let (drafts, _) = after_routed;
    assert!(
        !drafts.contains_key(&Path::new(fixture::OTHER_ACCOUNT).join("geteilt.md")),
        "the beta draft the selector named is gone: {:?}",
        drafts.keys().collect::<Vec<_>>()
    );
    assert!(
        drafts.contains_key(&Path::new(fixture::ACCOUNT).join("geteilt.md")),
        "the alpha draft with the same id was never a candidate: {:?}",
        drafts.keys().collect::<Vec<_>>()
    );
}

/// `cli_selector_contract::a_cross_account_received_selector_reads_from_its_own_account`:
/// the read resolves in `beta`'s store and fails only because that message has
/// no attachments, naming the `beta` selector it settled on.
#[test]
fn twin_a_cross_account_received_selector_reads_from_its_own_account() {
    let slice = Slice::start();
    let selector =
        fixture::message_selector(fixture::OTHER_ACCOUNT, "inbox", fixture::ONLY_IN_BETA);

    slice.for_each_binary(&["save", &selector], |which, out| {
        let text = stderr(out);
        assert!(text.contains("has no attachments"), "{which}: {text}");
        assert!(text.contains(&selector), "{which}: {text}");
    });
}

/// `cli_selector_contract::a_selector_naming_an_unconfigured_account_fails_with_that_account_named`:
/// the failure happens where the account is resolved, with the account named,
/// instead of surfacing as a phantom miss against the default account.
#[test]
fn twin_a_selector_naming_an_unconfigured_account_fails_with_that_account_named() {
    let slice = Slice::start();

    slice.for_each_binary(&["delete", "mp://ghost/drafts/whatever"], |which, out| {
        let text = stderr(out);
        assert!(text.contains("ghost"), "{which}: {text}");
        assert!(text.contains("not configured"), "{which}: {text}");
        assert!(
            text.contains(&format!("Error: {UNCONFIGURED_ACCOUNT}")),
            "{which}: {text}"
        );
    });
}
