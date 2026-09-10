//! The admin slice, moved onto the daemon (#0123, plan unit P4-U13).
//!
//! What is left of the CLI after the read, draft, mutation, sync and send
//! slices is administration: the configuration and the secrets behind it
//! (`mp config …`), the contact index (`mp contacts …`), the calendar
//! (`mp calendar rebuild`, `mp invite …`), the retention sweep
//! (`mp store gc`) and the one migration command (`mp cutover`). Eight
//! methods carry them - three new families' worth and three additions to a
//! family that already ships - plus every prompt, every password read and
//! every browser launch, which stay in the client because a daemon has no
//! stdin, no terminal and no display.
//!
//! This file is a **contract test**. It is written before those methods exist,
//! against the shapes fixed here and in `docs/daemon-protocol.md`, and it
//! fails to compile against today's tree; that failure is the proof the
//! contract has no stub behind it. The implementer (P4-U14) does not edit this
//! file.
//!
//! # The surface under test
//!
//! ```text
//! contact.rebuild        {account}
//!                            -> {operation_id}
//!                            settles {account, contacts, kept, saved, cache_path}
//! contact.search         {account, query, limit}
//!                            -> {account, query, contacts: [ContactRow]}
//! contact.stats          {account}
//!                            -> {account, total, sent_to, sent_cc, received,
//!                                built_at, cache_path, top: [ContactRow]}
//!
//! calendar.rebuild       {account}
//!                            -> {operation_id}
//!                            settles {account, resolved, invites_seen,
//!                                     replies_seen, cancelled}
//! calendar.rsvp          {account, selector, mailbox?, response}
//!                            -> {operation_id}
//!                            settles {account, selector, response, subject,
//!                                     organizer, message_id, delivered}
//!
//! diagnostic.store_gc    {account, dry_run, force}
//!                            -> {operation_id}
//!                            settles {account, dry_run, cap_bytes,
//!                                     before_bytes, after_bytes,
//!                                     evicted_bytes, evicted,
//!                                     decision: {kind, …}}
//!
//! config.cutover         {account, dry_run}
//!                            -> {operation_id}
//!                            settles {account, dry_run, drafts: {…},
//!                                     remnants: [{path, md_files, bytes}]}
//! config.oauth2_login    {account}
//!                            -> {operation_id}
//!                            progress {phase: "device_code",
//!                                      message: "<url> <code>"}
//!                            settles {stored, account, kind, key}
//! config.reset_secrets   {}
//!                            -> {removed: [path]}
//! ```
//!
//! `ContactRow` is `{address, display_name, sent_to, sent_cc, received,
//! score}`: the field names of `mailypoppins::contacts::Contact`, because one
//! spelling per fact is the rule the rest of the protocol follows, and because
//! `mp contacts search --parsable` prints `address\tdisplay_name` and a
//! renamed pair would make the tab-delimited line a translation rather than a
//! projection (`ANO-8`).
//!
//! ## Where each method lives, and why not somewhere else
//!
//! Plan section 3.0 fixes the families - `state.*`, `account.*`, `mailbox.*`,
//! `message.*`, `draft.*`, `send.*`, `sync.*`, `contact.*`, `calendar.*`,
//! `signature.*`, `config.*`, `operation.*`, `diagnostic.*`, `daemon.*` - and
//! there is no `store.*` and no `cutover.*` among them. So, exactly as the
//! send slice put the outbox in `send.outbox_*` rather than opening a
//! fifteenth family for three methods:
//!
//! - **`mp store gc` is `diagnostic.store_gc`.** `docs/parity-matrix.md`
//!   SYN-08 already writes that name down, and the sweep is
//!   diagnostics-and-maintenance by its own classification: it reports what a
//!   cache holds and evicts from it, and the daemon runs the same sweep after
//!   every sync without anybody asking.
//! - **`mp cutover` is `config.cutover`.** `docs/parity-matrix.md` MIG-01
//!   writes down "`config.cutover`, or the command stays client-side over the
//!   filesystem"; client-side is not available, because after the cutover the
//!   daemon owns the data directory and the drafts index the import writes
//!   into, so the named alternative is the one taken.
//!
//! Two committed documents therefore agree about every method here, which is
//! the standard the earlier slices set.
//!
//! ## `account` is required everywhere, and the loops stay in the client
//!
//! Not one method of this slice takes an optional `account`, an
//! `all_accounts` or a "default account" - `mp contacts rebuild` with no flag
//! walks every configured account, `mp store gc --all-accounts` walks them
//! too, and `mp calendar rebuild` and `mp cutover` default to all of them -
//! and every one of those loops is the client's, over `config.get`'s account
//! list, in **configuration order**, printing its per-account block and
//! continuing past a refusal. That is what `mp sync --all-accounts` (P4-U9)
//! and `mp send-approved --all-accounts` (P4-U11) already do, and the reason
//! is the same: the loop is a presentation decision, one account's failure
//! must not end the pass, and a method that quietly served "the first
//! configured account" would make the CLI's default and the GUI's default two
//! rules that can drift. [`no_admin_method_serves_a_default_account`] pins it.
//!
//! ## Kinds, and the two that are not obvious
//!
//! - `contact.search` and `contact.stats` are queries: they read the cache and
//!   change nothing.
//! - `config.reset_secrets` is a [`MethodKind::Command`]: unlinking two files
//!   is one committed change with nothing to watch.
//! - `contact.rebuild` is an [`MethodKind::Operation`], **and this is a choice
//!   this unit had to make**. It looks local, but `build_index_for_account`
//!   walks every row of every mailbox of the account and re-ranks the result;
//!   on a real mailbox that is seconds, it is what the TUI's `r` key runs, and
//!   a GUI wants to watch it. A command that took ten seconds and reported one
//!   revision would be a lie about what it did.
//! - `calendar.rsvp` is an operation because it **sends**: an iMIP reply goes
//!   out over SMTP through the durable outbox, which is an unbounded network
//!   round trip, and the send slice already made every such method an
//!   operation.
//! - `diagnostic.store_gc`, `config.cutover` and `config.oauth2_login` are
//!   operations for the same reason at three different speeds: a sweep walks
//!   and unlinks blobs, a cutover walks a file tree and rewrites drafts, and a
//!   device-code login waits for a human to type a code into a browser.
//!
//! ## Cancellation
//!
//! Everything here is [`CancelScope::Durable`]. A rebuild abandoned half way
//! leaves a cache nobody chose; a sweep abandoned half way leaves blobs
//! unlinked and rows saying otherwise; an RSVP abandoned half way is the
//! ambiguous submission `sweep_pending_sends` exists to park; a secrets reset
//! abandoned half way is the state `mp config reset-secrets` exists to leave.
//! None of that may follow from a window closing.
//!
//! # The prompts stay in the client, and so does the browser
//!
//! Five commands here read stdin today, and not one of them may move:
//!
//! | command | what it reads | with stdin closed |
//! |---|---|---|
//! | `mp config init` | `Overwrite? [y/N]` | prints `Cancelled.`, exits 0 |
//! | `mp config add-account` | the wizard's answers | refuses before the first, no config file |
//! | `mp config set-password` | a `dialoguer::Password` | fails, no secret written |
//! | `mp config reset-secrets` | `Continue? [y/N]` | prints `Cancelled.`, exits 0 |
//! | `mp config oauth2-login` | nothing; it *writes* the code | n/a |
//!
//! Every fixture run below has `Stdio::null()` for stdin, which is what makes
//! those reproducible, and the parity rows prove the routed binary produces
//! the oracle's bytes for each. [`mp_config_reset_secrets_confirmed_matches_the_oracle`]
//! is the one row that answers a prompt, through a piped stdin, so the
//! affirmative branch is pinned too.
//!
//! **`mp config init` and `mp config add-account` call `config.get` before
//! they prompt**, which is how the client learns whether a configuration file
//! exists and where it is - the two facts the first line of each command
//! prints. That is not an optimisation: it is what lets those two commands
//! satisfy `MAILYPOPPINS_DAEMON_REQUIRE` on the branch where the user declines,
//! and it is why `mp config init` is deliberately *not* on the no-daemon list
//! (`src/daemon/client.rs`).
//!
//! **`mp config set-password` reads the password with `dialoguer::Password`,
//! client-side, and nothing else.** There is no `MAILYPOPPINS_PASSWORD`
//! environment variable today and this slice does not invent one: a secret in
//! an environment variable is a secret in `/proc`, and the shipped
//! `config.set_password` (P2) already takes the value as a parameter. So the
//! client prompts and then calls, the value crosses the socket once, and a run
//! whose stdin is not a terminal fails exactly as it fails today
//! ([`mp_config_set_password_reads_the_password_in_the_client`]).
//!
//! **OAuth2's browser is the client's** (`INT-04`). The daemon runs the device
//! flow as an operation and reports the verification URL and the user code as
//! its first `operation.progress`; the client renders them and, on a desktop,
//! opens the browser. The payload is `{phase: "device_code", done: 0,
//! total: null, message: "<verification_uri> <user_code>"}` - the progress
//! shape the protocol already fixes for every operation
//! (`{operation_id, phase, done, total, message}`), with the two values in the
//! one free-text field, separated by the single space neither of them can
//! contain. Inventing a field for this one operation would move the frame
//! shape every other operation shares; a two-token message does not.
//! [`the_device_code_block_renders_from_a_progress_payload`] pins both halves.
//!
//! # OAuth2 has no live provider, so what is pinned is the refusals
//!
//! `mp config oauth2-login` talks to Microsoft. There is no IdP in this
//! repository and this slice does not add an HTTP fake for one, so the
//! success path is pinned only as far as it can be honestly reached:
//!
//! - the three refusals, over the wire and through the CLI: an unknown
//!   account, an account whose `auth_method` is `password`, and an OAuth2
//!   account with no usable `[accounts.oauth2]` section;
//! - the rendering, as a pure function of a synthetic progress payload
//!   through [`oauth2_start_line`], [`oauth2_device_code_lines`] and
//!   [`oauth2_stored_line`], which are the three things `src/oauth2.rs` and
//!   `src/config_cmd/oauth2.rs` print today and the only three a GUI would
//!   have to reproduce.
//!
//! The connection tests `mp config oauth2-login` runs after acquiring a token
//! (IMAP, SMTP, or Graph `/me`) are **not** pinned here: they need a server as
//! much as the flow needs an IdP, and they are the account slice's subject.
//!
//! # Parity, and what proves routing
//!
//! Every parity row is `fixture.mp_routed(args)` against `oracle(args, root)`
//! over the same seeded root: stdout, stderr and exit code, literally. The
//! routed side runs under `MAILYPOPPINS_DAEMON_REQUIRE=1`, so a command that
//! quietly answered in process fails instead of passing for the daemon's work,
//! and [`no_admin_command_can_still_answer_without_a_daemon`] covers what that
//! variable cannot: with nothing listening and auto-start off, a command with
//! no in-process path left exits 4.
//!
//! **`mp config path` is the exception, and it is the exception on purpose.**
//! It computes a path and reads nothing, so it is on the no-daemon list
//! (`src/daemon/client.rs::needs_daemon`) and stays there for good.
//! [`mp_config_path_never_contacts_a_daemon`] runs it through
//! [`mp_no_daemon`] and asserts no socket appeared. This unit also moves
//! `tests/daemon_parity_harness.rs`'s `UNMIGRATED` control row onto it: that
//! list needs one command that a daemon-era binary and the pre-daemon oracle
//! answer identically because it was never migrated, and after this slice
//! `mp config path` is the only such command left in the product - which also
//! means the control row never has to move again.
//!
//! **No masks.** Not one row here masks a byte. The two values that would have
//! forced a mask are removed at the fixture instead: the contact index's
//! `built_at`, by building the cache once before either binary runs
//! ([`admin_fixture`]), and an RSVP's `Message-ID`, by keeping every row that
//! actually sends one on the routed side, where there is nothing to compare
//! it against.
//!
//! **Mutating rows restore first.** Most of this slice writes. A parity
//! comparison of a writing command cannot run the two binaries one after the
//! other over one root - the second would see the first one's work - so
//! [`Slice::both_mutating`] takes a [`admin_fixture::Pristine`] copy of the
//! `accounts/` tree, runs the routed binary, stops the daemon, restores, runs
//! the oracle, compares both the bytes and the resulting state, restores again
//! and starts the daemon back up. The daemon is stopped around the restore
//! deliberately: it holds the store open in WAL mode and replacing
//! `store.sqlite3` under an open connection is undefined.
//!
//! # Results are path-free, with three named exceptions
//!
//! Nothing this slice answers may name `store.sqlite3`, a blob path or a
//! runtime path. Three fields carry a path all the same, each because the
//! command's own output prints it and a client cannot compute it:
//!
//! - `contact.stats.cache_path` and `contact.rebuild`'s settled `cache_path` -
//!   `mp contacts stats` prints `Cache path: …` and `mp contacts rebuild`
//!   prints `… cached at …`;
//! - `config.cutover`'s `remnants[].path` and `drafts.imported[]` - naming the
//!   file-era directories so a human can `rm -rf` them is the entire point of
//!   the command (`MIG-01`);
//! - `config.reset_secrets`'s `removed[]` - `mp config reset-secrets` prints
//!   `✓ Removed <path>` per file, and which files existed is a fact only the
//!   daemon has.
//!
//! `config.get`'s `path` is the fourth, and it already ships.
//! [`every_admin_result_is_path_free_outside_the_named_fields`] enforces the
//! rest.
//!
//! # What this slice does not pin
//!
//! - **The sweep's warn-then-evict decisions (`ANO-5`).** The fixture's blobs
//!   are far under any cap that `RetentionPolicy::resolve` accepts, so every
//!   row here exercises `UnderCap`. The first-breach marker, the eviction and
//!   the half-store guard are library behaviour with unit tests in
//!   `src/store/sweep.rs`, and routing does not touch them; what this slice
//!   owes them is that the decision reaches the client verbatim, which
//!   [`store_gc_reports_the_decision_the_sweep_took`] asserts by name.
//! - **A real SMTP conversation.** As in the send slice, the one available
//!   substitute is the daemon-side [`FAKE_TRANSPORT_ENV`] hook, and every row
//!   that uses it is a routed-side assertion rather than a parity row, because
//!   the pre-daemon oracle cannot see the hook.
//! - **`mp config init`'s and `mp config add-account`'s wizards past their
//!   first prompt.** Both reach for a network connection a few answers in
//!   (`test_imap_connection`), so a run with a closed stdin that answered
//!   every prompt with its default would dial a real host. The rows here stop
//!   at the first branch of each: the overwrite refusal and the "no config
//!   file" refusal.
//!
//! # The legacy suites, twinned
//!
//! `rg -l 'contacts|calendar rebuild|store gc|cutover|set-password|config init|
//! add-account|invite accept' tests/*.rs` finds the daemon suites, plus
//! `tests/draft_integration.rs`, whose mentions are of drafts and not of these
//! commands. The behaviour under these names lives in unit tests inside
//! `src/contacts/`, `src/cutover.rs`, `src/store/sweep.rs`, `src/config_cmd/`
//! and in `tests/imip_integration.rs`, which is a *receive* suite driving
//! `parse_rfc822_to_fetched_email` and `ingest_message` directly. Routing
//! changes nothing about a library call, so what this file re-runs is the
//! scenarios that have a CLI shape: the RSVP's material
//! ([`admin_fixture::INVITATION`], an Outlook-shaped `METHOD:REQUEST`) and the
//! organiser-side fold `mp calendar rebuild` reports (`CAL-04`).

mod support;

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::format::{oauth2_device_code_lines, oauth2_start_line, oauth2_stored_line};
use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::daemon::dispatch::{CancelScope, MethodKind};
use mailypoppins::daemon::methods::calendar::CALENDAR_METHOD_SPECS;
use mailypoppins::daemon::methods::config::CONFIG_METHOD_SPECS;
use mailypoppins::daemon::methods::contact::CONTACT_METHOD_SPECS;
use mailypoppins::daemon::methods::diagnostic::DIAGNOSTIC_METHOD_SPECS;

use support::admin_fixture as fixture;
use support::parity::{
    assert_byte_identical, daemon_is_listening, mp_command, mp_no_daemon, oracle_command,
    socket_path, DaemonFixture, EXIT_UNAVAILABLE, REQUIRE_ENV,
};

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// Upper bound on waiting for an operation to settle.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// The JSON-RPC code every "you asked for something that cannot be done"
/// refusal of this slice carries.
const INVALID_PARAMS: i32 = -32602;

/// The three methods of the contact family, in the order their spec array
/// declares them, which is method-name order like every other family.
const CONTACT_METHODS: [&str; 3] = ["contact.rebuild", "contact.search", "contact.stats"];

/// The two methods of the calendar family.
const CALENDAR_METHODS: [&str; 2] = ["calendar.rebuild", "calendar.rsvp"];

/// The diagnostic family, as far as this slice needs it: one method.
const DIAGNOSTIC_METHODS: [&str; 1] = ["diagnostic.store_gc"];

/// The config family after this slice: the six shipped methods plus the three
/// this one adds, still in method-name order.
const CONFIG_METHODS: [&str; 9] = [
    "config.add_account",
    "config.cutover",
    "config.get",
    "config.init",
    "config.oauth2_login",
    "config.reload",
    "config.reset_secrets",
    "config.set_password",
    "config.validate",
];

/// Every method this slice puts on the wire.
const ADMIN_METHODS: [&str; 8] = [
    "calendar.rebuild",
    "calendar.rsvp",
    "config.cutover",
    "config.oauth2_login",
    "config.reset_secrets",
    "contact.rebuild",
    "contact.search",
    "contact.stats",
];

/// The families declare exactly those names, checked while the tree compiles:
/// an array that grew a method nobody wrote down fails here, naming the
/// constant, before a single test runs.
const _: () = assert!(CONTACT_METHOD_SPECS.len() == CONTACT_METHODS.len());
const _: () = assert!(CALENDAR_METHOD_SPECS.len() == CALENDAR_METHODS.len());
const _: () = assert!(DIAGNOSTIC_METHOD_SPECS.len() == DIAGNOSTIC_METHODS.len());
const _: () = assert!(CONFIG_METHOD_SPECS.len() == CONFIG_METHODS.len());

/// The phase the OAuth2 device-code progress report carries.
const DEVICE_CODE_PHASE: &str = "device_code";

/// The synthetic device-code values the rendering is pinned against: a URL and
/// a code in Microsoft's own shapes, neither containing a space.
const VERIFICATION_URI: &str = "https://microsoft.com/devicelogin";
const USER_CODE: &str = "FQXBK7DL";

/// The block `src/oauth2.rs` prints today, verbatim, with a leading blank line
/// and a trailing one.
const DEVICE_CODE_BLOCK: &str = "\n  To sign in, open a browser and go to:\n\n    \
                                 https://microsoft.com/devicelogin\n\n  Enter the code: \
                                 FQXBK7DL\n\n";

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
        let tmp = TempDir::new().expect("a temporary admin-slice root");
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

    /// A root whose daemon serves the RSVP's SMTP submission and its
    /// Sent-mailbox APPEND in process.
    ///
    /// The hook's value names the ledger file, which lives under the root, so
    /// it can only be built once the root exists.
    fn with_fake_transport() -> Slice {
        let tmp = TempDir::new().expect("a temporary admin-slice root");
        fixture::seed(tmp.path());
        let hook = fixture::fake_transport(&fixture::transport_log(tmp.path()));
        let daemon =
            DaemonFixture::start_with(tmp.path(), None, &[(fixture::FAKE_TRANSPORT_ENV, &hook)]);
        Slice {
            daemon: Some(daemon),
            env: vec![(fixture::FAKE_TRANSPORT_ENV.to_string(), hook)],
            tmp,
        }
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

    /// The same comparison for a command that writes, with the fixture put
    /// back between the two runs and the resulting state compared.
    ///
    /// `state` is read after each binary has run and must agree: bytes alone
    /// would let a routed command print the right sentence about the wrong
    /// file. The daemon is stopped before the restore and started again after
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

    /// Run the routed client with `input` on its stdin, which is how the one
    /// row that *answers* a prompt answers it.
    fn routed_with_stdin(&self, args: &[&str], input: &str) -> Output {
        run_with_stdin(mp_command(self.root()).env(REQUIRE_ENV, "1"), args, input)
    }

    /// The same for the oracle.
    fn oracle_with_stdin(&self, args: &[&str], input: &str) -> Output {
        run_with_stdin(&mut oracle_command(self.root()), args, input)
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
            conn.initialize(client_info(), identity(self.root()), &[], &[]),
        )
        .await
        .expect("a compatible handshake succeeds");
        conn
    }
}

/// The client this suite declares itself as.
fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Cli,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// The identity a one-root sandbox produces.
fn identity(root: &Path) -> Identity {
    Identity {
        data_dir: root.to_path_buf(),
        config_dir: root.to_path_buf(),
    }
}

/// Run a command with `input` written to its stdin and the pipe then closed,
/// so a second prompt reads EOF rather than blocking.
fn run_with_stdin(cmd: &mut std::process::Command, args: &[&str], input: &str) -> Output {
    use std::io::Write;

    let mut child = cmd
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn `mp {}`: {e}", args.join(" ")));
    child
        .stdin
        .as_mut()
        .expect("the child's stdin is a pipe")
        .write_all(input.as_bytes())
        .expect("write the answer to the prompt");
    drop(child.stdin.take());
    child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("wait for `mp {}`: {e}", args.join(" ")))
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

/// The `result` of a settled operation that must have succeeded.
fn succeeded(status: &Value) -> &Value {
    assert_eq!(
        status["state"], "succeeded",
        "the operation was expected to succeed: {status}"
    );
    &status["result"]
}

/// The spec of one declared method, whichever family it belongs to.
fn spec_of(name: &str) -> &'static mailypoppins::daemon::dispatch::MethodSpec {
    CONTACT_METHOD_SPECS
        .iter()
        .chain(CALENDAR_METHOD_SPECS.iter())
        .chain(DIAGNOSTIC_METHOD_SPECS.iter())
        .chain(CONFIG_METHOD_SPECS.iter())
        .find(|spec| spec.name == name)
        .unwrap_or_else(|| panic!("{name} is declared"))
}

/// Nothing this slice answers may name a file the client has no business
/// opening. The three fields that carry a path deliberately are removed
/// before the check, and named where they are removed.
fn assert_path_free(what: &str, value: &Value) {
    let mut pruned = value.clone();
    prune_path_fields(&mut pruned);
    let text = pruned.to_string();
    for needle in ["store.sqlite3", "/blobs/", "/runtime/", "daemon.sock"] {
        assert!(
            !text.contains(needle),
            "{what} leaked {needle:?} into its result: {text}"
        );
    }
}

/// Remove the fields the CLI prints a path out of: `cache_path` (`CON-03`,
/// `CON-04`), `path` and `imported` (`MIG-01`), `removed` (`ACC-07`).
fn prune_path_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["cache_path", "path", "imported", "removed", "remnants"] {
                map.remove(key);
            }
            for (_, nested) in map.iter_mut() {
                prune_path_fields(nested);
            }
        }
        Value::Array(rows) => {
            for row in rows.iter_mut() {
                prune_path_fields(row);
            }
        }
        _ => {}
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

// ---------------------------------------------------------------------------
// 1. The methods themselves
// ---------------------------------------------------------------------------

/// The contact family: three methods, their kinds, their protocol version and
/// what a disconnect does to them.
#[test]
fn the_contact_family_declares_three_methods() {
    let names: Vec<&str> = CONTACT_METHOD_SPECS.iter().map(|spec| spec.name).collect();
    assert_eq!(
        names, CONTACT_METHODS,
        "the family serves exactly these three"
    );

    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the array is in method-name order");

    assert_eq!(
        spec_of("contact.rebuild").kind,
        MethodKind::Operation,
        "a rebuild walks every row of every mailbox and re-ranks the result"
    );
    for name in ["contact.search", "contact.stats"] {
        assert_eq!(
            spec_of(name).kind,
            MethodKind::Query,
            "{name} reads the index and changes nothing"
        );
    }
    for spec in CONTACT_METHOD_SPECS {
        assert_eq!(spec.since, 1, "{} is served from protocol 1", spec.name);
        assert_eq!(
            spec.cancel_scope,
            CancelScope::Durable,
            "{} must not be abandoned when its client goes away",
            spec.name
        );
    }
}

/// The calendar family: two methods, one of which sends.
#[test]
fn the_calendar_family_declares_two_methods() {
    let names: Vec<&str> = CALENDAR_METHOD_SPECS.iter().map(|spec| spec.name).collect();
    assert_eq!(
        names, CALENDAR_METHODS,
        "the family serves exactly these two"
    );

    for name in CALENDAR_METHODS {
        assert_eq!(
            spec_of(name).kind,
            MethodKind::Operation,
            "{name} is long-running: a fold over a mailbox, or an SMTP round trip"
        );
        assert_eq!(spec_of(name).since, 1);
        assert_eq!(
            spec_of(name).cancel_scope,
            CancelScope::Durable,
            "an RSVP abandoned mid-submission is the ambiguous state the outbox parks"
        );
    }
}

/// The retention sweep is a `diagnostic.*` method, because plan section 3.0
/// declares no `store.*` family and `docs/parity-matrix.md` SYN-08 names this
/// one.
#[test]
fn the_retention_sweep_is_a_diagnostic_method() {
    let names: Vec<&str> = DIAGNOSTIC_METHOD_SPECS
        .iter()
        .map(|spec| spec.name)
        .collect();
    assert!(
        names.contains(&"diagnostic.store_gc"),
        "the sweep is served as diagnostic.store_gc, not under a store.* family: {names:?}"
    );
    assert_eq!(
        spec_of("diagnostic.store_gc").kind,
        MethodKind::Operation,
        "a sweep walks the blob tree and unlinks from it"
    );
    assert_eq!(spec_of("diagnostic.store_gc").since, 1);
    assert_eq!(
        spec_of("diagnostic.store_gc").cancel_scope,
        CancelScope::Durable,
        "a sweep abandoned half way leaves blobs unlinked and rows saying otherwise"
    );
}

/// The config family grows by three, keeps its order, and keeps every one of
/// the six it already served.
#[test]
fn the_config_family_grows_by_three() {
    let names: Vec<&str> = CONFIG_METHOD_SPECS.iter().map(|spec| spec.name).collect();
    assert_eq!(
        names, CONFIG_METHODS,
        "the family serves exactly these nine"
    );

    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the array is in method-name order");

    assert_eq!(
        spec_of("config.reset_secrets").kind,
        MethodKind::Command,
        "unlinking the secret files is one committed change with nothing to watch"
    );
    for name in ["config.cutover", "config.oauth2_login"] {
        assert_eq!(
            spec_of(name).kind,
            MethodKind::Operation,
            "{name} runs for as long as a file tree, or a human, takes"
        );
    }
    for name in [
        "config.cutover",
        "config.oauth2_login",
        "config.reset_secrets",
    ] {
        assert_eq!(spec_of(name).since, 1);
        assert_eq!(
            spec_of(name).cancel_scope,
            CancelScope::Durable,
            "a configuration change undone by a disconnect leaves a state nobody chose"
        );
    }
}

/// A method may not be served without being advertised: the handshake derives
/// the capability list from the dispatcher, so the names appearing there is
/// what says they are registered rather than merely declared.
#[tokio::test]
async fn the_daemon_advertises_the_admin_slice_methods() {
    let slice = Slice::start();
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&socket_path(slice.root())),
    )
    .await
    .expect("connect");
    let result = within(
        "Connection::initialize",
        conn.initialize(client_info(), identity(slice.root()), &[], &[]),
    )
    .await
    .expect("handshake");

    for method in ADMIN_METHODS.iter().chain(["diagnostic.store_gc"].iter()) {
        assert!(
            result.capabilities.iter().any(|c| c == method),
            "{method} is served, so it is advertised: {:?}",
            result.capabilities
        );
    }
}

/// Not one method of this slice serves "the account you probably meant".
///
/// Every all-accounts loop and every "first configured account" default is the
/// client's, over the account list it already reads, in configuration order.
/// A call with no `account` is a parameter error rather than a guess.
#[tokio::test]
async fn no_admin_method_serves_a_default_account() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for method in [
        "contact.search",
        "contact.stats",
        "contact.rebuild",
        "calendar.rebuild",
        "calendar.rsvp",
        "diagnostic.store_gc",
        "config.cutover",
        "config.oauth2_login",
    ] {
        let refused = call_err(&mut conn, method, json!({})).await;
        assert_eq!(
            refused.code, INVALID_PARAMS,
            "{method} takes a named account and never picks one"
        );
    }

    // And the negative: no method of this slice accepts an `all_accounts`
    // flag, so a loop cannot quietly move into the daemon later.
    for method in ["contact.rebuild", "diagnostic.store_gc", "config.cutover"] {
        let refused = call_err(
            &mut conn,
            method,
            json!({"account": fixture::ACCOUNT, "all_accounts": true}),
        )
        .await;
        assert_eq!(
            refused.code, INVALID_PARAMS,
            "{method} has no all_accounts parameter; the loop is the client's"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. `contact.*`
// ---------------------------------------------------------------------------

/// The rows `mp contacts search` and its `--parsable` projection are built
/// from, and the fields each row fixes.
///
/// The fixture's messages are real ingested mail, so the index has real rows:
/// the addresses that wrote to and were written to by `alpha`.
#[tokio::test]
async fn contact_search_answers_the_rows_the_parsable_projection_is_built_from() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "contact.search",
        json!({"account": fixture::ACCOUNT, "query": "", "limit": 20}),
    )
    .await;
    assert_eq!(result["account"], fixture::ACCOUNT);
    assert_eq!(result["query"], "");

    let contacts = result["contacts"]
        .as_array()
        .unwrap_or_else(|| panic!("contact.search answers a `contacts` array: {result}"));
    assert!(
        !contacts.is_empty(),
        "the fixture's ingested mail yields real contacts: {result}"
    );
    assert!(
        contacts.len() <= 20,
        "the limit is the caller's and is honoured: {result}"
    );
    for row in contacts {
        for field in [
            "address",
            "display_name",
            "sent_to",
            "sent_cc",
            "received",
            "score",
        ] {
            assert!(
                row.get(field).is_some(),
                "a contact row carries {field}: {row}"
            );
        }
        assert!(
            row["address"].as_str().is_some_and(|a| a.contains('@')),
            "a contact row's address is an address: {row}"
        );
    }

    // The tier the CLI's glyph is chosen from is on the row, not derived from
    // a second call: `sent_to > 0`, else `sent_cc > 0`, else received-only.
    assert!(
        contacts
            .iter()
            .any(|row| row["sent_to"].as_u64().unwrap_or(0) > 0),
        "the fixture's sent mailbox makes at least one row a sent-to contact: {result}"
    );

    // A query narrows, and a query nothing matches answers an empty array
    // rather than a refusal: "no matches" is an answer.
    let narrowed = call(
        &mut conn,
        "contact.search",
        json!({"account": fixture::ACCOUNT, "query": "aardvark", "limit": 20}),
    )
    .await;
    assert_eq!(
        narrowed["contacts"].as_array().map(Vec::len),
        Some(0),
        "a query with no hits is an empty list: {narrowed}"
    );

    let limited = call(
        &mut conn,
        "contact.search",
        json!({"account": fixture::ACCOUNT, "query": "", "limit": 1}),
    )
    .await;
    assert_eq!(
        limited["contacts"].as_array().map(Vec::len),
        Some(1),
        "`-n 1` is one row: {limited}"
    );

    assert_path_free("contact.search", &result);
}

/// The two account refusals every method of this slice that opens a store
/// makes: an account no configuration carries, and a configured account with
/// no store on disk.
#[tokio::test]
async fn contact_search_refuses_an_unknown_account() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let unknown = call_err(
        &mut conn,
        "contact.search",
        json!({"account": fixture::UNKNOWN_ACCOUNT, "query": "", "limit": 20}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
    assert_eq!(data_of(&unknown)["account"], fixture::UNKNOWN_ACCOUNT);

    let storeless = call_err(
        &mut conn,
        "contact.search",
        json!({"account": fixture::STORELESS_ACCOUNT, "query": "", "limit": 20}),
    )
    .await;
    assert_eq!(storeless.code, ErrorCode::AccountNotReady.code());
    assert_eq!(data_of(&storeless)["account"], fixture::STORELESS_ACCOUNT);
}

/// Everything `mp contacts stats` prints, in one answer: the totals, the cache
/// it read, when it was built, and the top rows.
#[tokio::test]
async fn contact_stats_reports_the_index_and_its_top_rows() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "contact.stats",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;

    assert_eq!(result["account"], fixture::ACCOUNT);
    let total = result["total"]
        .as_u64()
        .unwrap_or_else(|| panic!("contact.stats carries a total: {result}"));
    assert!(total > 0, "the fixture's index is not empty: {result}");
    for field in ["sent_to", "sent_cc", "received"] {
        assert!(
            result[field].as_u64().is_some(),
            "contact.stats carries {field}: {result}"
        );
    }
    assert!(
        result["built_at"].as_str().is_some(),
        "contact.stats reports when the index was built: {result}"
    );
    assert_eq!(
        result["cache_path"].as_str(),
        Some(
            fixture::contacts_cache(slice.root(), fixture::ACCOUNT)
                .to_string_lossy()
                .as_ref()
        ),
        "the cache path is the one the CLI prints: {result}"
    );

    let top = result["top"]
        .as_array()
        .unwrap_or_else(|| panic!("contact.stats carries the top rows: {result}"));
    assert!(!top.is_empty() && top.len() <= 10, "at most ten: {result}");
    for row in top {
        for field in ["address", "display_name", "sent_to", "sent_cc", "received"] {
            assert!(row.get(field).is_some(), "a top row carries {field}: {row}");
        }
    }

    assert_path_free("contact.stats", &result);
}

/// The rebuild is an operation, it reports progress, and it settles with the
/// verdict `save_rebuilt_cache` reached - which is the whole of what the CLI
/// prints, including the two refusals that keep a good cache in place (#0067).
#[tokio::test]
async fn contact_rebuild_reports_progress_and_settles_with_the_save_verdict() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    call(&mut conn, "state.bootstrap", json!({})).await;

    let started = call(
        &mut conn,
        "contact.rebuild",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    let id = operation_id(&started);

    // The progress report a GUI draws its spinner from: the phase is the
    // family's own word and the message names the account, so a batch of five
    // accounts is legible while it runs.
    let start = Instant::now();
    let mut seen_progress = false;
    while start.elapsed() < SETTLE_DEADLINE {
        let Some(notification) = within("state.event", conn.next_notification()).await else {
            break;
        };
        if notification.params["kind"] != "operation.progress" {
            continue;
        }
        let payload = &notification.params["payload"];
        if payload["operation_id"] != Value::String(id.clone()) {
            continue;
        }
        assert_eq!(
            payload["phase"], "contacts",
            "a contact rebuild reports under its own phase: {payload}"
        );
        assert_eq!(
            payload["message"],
            fixture::ACCOUNT,
            "the report names the account being rebuilt: {payload}"
        );
        seen_progress = true;
        break;
    }
    assert!(
        seen_progress,
        "contact.rebuild is an operation with progress, and reported none"
    );

    let status = settle(&mut conn, &id).await;
    let result = succeeded(&status);
    assert_eq!(result["account"], fixture::ACCOUNT);
    assert_eq!(
        result["saved"], "written",
        "a rebuild that found rows writes them: {result}"
    );
    let contacts = result["contacts"]
        .as_u64()
        .unwrap_or_else(|| panic!("the settled result carries the count: {result}"));
    assert_eq!(
        contacts as usize,
        fixture::cached_contacts(slice.root(), fixture::ACCOUNT),
        "the count it reported is the count it wrote"
    );
    assert_eq!(
        result["cache_path"].as_str(),
        Some(
            fixture::contacts_cache(slice.root(), fixture::ACCOUNT)
                .to_string_lossy()
                .as_ref()
        ),
        "the CLI prints `… cached at <path>` from this field: {result}"
    );
    assert!(
        result.get("kept").is_some(),
        "the refusal branches report what they kept, so the field is always there: {result}"
    );

    assert_path_free("contact.rebuild", result);
}

// ---------------------------------------------------------------------------
// 3. `calendar.*`
// ---------------------------------------------------------------------------

/// `CAL-04`: what the stored replies resolve on the stored invitations, and
/// nothing written.
#[tokio::test]
async fn calendar_rebuild_reports_what_the_replies_resolve() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let status = run_operation(
        &mut conn,
        "calendar.rebuild",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    let result = succeeded(&status);

    assert_eq!(result["account"], fixture::ACCOUNT);
    assert_eq!(
        result["replies_seen"], 1,
        "the fixture holds exactly one METHOD:REPLY: {result}"
    );
    assert_eq!(
        result["resolved"], 1,
        "that reply resolves against the invitation this account organised: {result}"
    );
    assert!(
        result["invites_seen"].as_u64().unwrap_or(0) >= 2,
        "the fixture holds at least the two REQUESTs: {result}"
    );
    assert_eq!(
        result["cancelled"], 0,
        "nothing in the fixture is cancelled, and the field is reported all the same: {result}"
    );

    assert_path_free("calendar.rebuild", result);
}

/// A configured account with no store is `-32006`, which is the refusal the
/// client renders as today's `• no store yet for <account>` line before
/// carrying on to the next account.
#[tokio::test]
async fn calendar_rebuild_refuses_a_storeless_account() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let refused = call_err(
        &mut conn,
        "calendar.rebuild",
        json!({"account": fixture::STORELESS_ACCOUNT}),
    )
    .await;
    assert_eq!(refused.code, ErrorCode::AccountNotReady.code());
    assert_eq!(data_of(&refused)["account"], fixture::STORELESS_ACCOUNT);
}

/// `ANO-4` at the wire, for the RSVP: a Graph account is refused in the
/// sentence `src/main.rs` prints today, and *before* anything else about the
/// selector is examined.
///
/// The ordering is the contract, not an accident: the GUI shows the RSVP
/// buttons as disabled with their reason, which it can only do if the refusal
/// arrives from a call that named no resolvable message.
#[tokio::test]
async fn calendar_rsvp_refuses_a_graph_account_before_it_looks_at_anything_else() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let refused = call_err(
        &mut conn,
        "calendar.rsvp",
        json!({
            "account": fixture::GRAPH_ACCOUNT,
            "selector": fixture::UNKNOWN_SELECTOR,
            "response": "accept",
        }),
    )
    .await;
    assert_eq!(refused.code, INVALID_PARAMS);
    assert_eq!(
        refused.message,
        fixture::GRAPH_RSVP_REFUSAL,
        "the sentence is the user's, verbatim"
    );
    assert_eq!(data_of(&refused)["account"], fixture::GRAPH_ACCOUNT);
}

/// The three ways an RSVP can name something it cannot answer, and the one
/// spelling each earns.
#[tokio::test]
async fn calendar_rsvp_refuses_what_it_cannot_answer() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let account = fixture::ACCOUNT;

    // A selector that resolves to no row.
    let unknown = call_err(
        &mut conn,
        "calendar.rsvp",
        json!({"account": account, "selector": fixture::UNKNOWN_SELECTOR, "response": "accept"}),
    )
    .await;
    assert_eq!(unknown.code, INVALID_PARAMS);

    // A row with no `invite.ics` blob: there is nothing to reply to.
    let no_invite = call_err(
        &mut conn,
        "calendar.rsvp",
        json!({
            "account": account,
            "selector": fixture::selector(account, fixture::NOT_AN_INVITATION),
            "response": "accept",
        }),
    )
    .await;
    assert_eq!(no_invite.code, INVALID_PARAMS);
    assert!(
        no_invite.message.contains("carries no invitation"),
        "the sentence is the user's, verbatim: {}",
        no_invite.message
    );

    // A response word the protocol does not carry. Three spellings and no
    // synonyms, as everywhere else on the wire.
    let bad_response = call_err(
        &mut conn,
        "calendar.rsvp",
        json!({
            "account": account,
            "selector": fixture::selector(account, fixture::INVITATION),
            "response": "maybe",
        }),
    )
    .await;
    assert_eq!(bad_response.code, INVALID_PARAMS);

    // A selector naming another account: the reply goes out over *this*
    // account's transport, so a cross-account selector fails loudly rather
    // than replying from the wrong address.
    let cross = call_err(
        &mut conn,
        "calendar.rsvp",
        json!({
            "account": account,
            "selector": fixture::selector(fixture::OTHER_ACCOUNT, fixture::INVITATION),
            "response": "accept",
        }),
    )
    .await;
    assert_eq!(cross.code, INVALID_PARAMS);
}

/// The success path, through the daemon-side fake transport: an RSVP that
/// really goes out, in all three responses.
///
/// **Routed-side only.** The hook is the daemon's and the pre-daemon oracle
/// cannot see it, so this is an assertion about the daemon rather than a
/// parity row.
#[tokio::test]
async fn a_routed_rsvp_submits_the_reply_to_the_organizer() {
    let slice = Slice::with_fake_transport();
    let mut conn = slice.connect().await;

    let status = run_operation(
        &mut conn,
        "calendar.rsvp",
        json!({
            "account": fixture::ACCOUNT,
            "selector": fixture::selector(fixture::ACCOUNT, fixture::INVITATION),
            "mailbox": fixture::MAILBOX,
            "response": "accept",
        }),
    )
    .await;
    let result = succeeded(&status);

    assert_eq!(result["account"], fixture::ACCOUNT);
    assert_eq!(result["response"], "accept");
    assert_eq!(
        result["organizer"],
        fixture::ORGANIZER,
        "the organizer is who the reply went to, and what the CLI's line names: {result}"
    );
    assert_eq!(
        result["subject"],
        format!("Accepted: {}", fixture::INVITATION_SUMMARY),
        "the subject is `<verb>: <summary>`, which the CLI prints: {result}"
    );
    assert_eq!(
        result["delivered"], true,
        "a submission every recipient took is delivered: {result}"
    );
    assert!(
        result["message_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "the reply carries the Message-ID it was submitted under: {result}"
    );
    assert_path_free("calendar.rsvp", result);

    // And the transport really saw it: one submission, to the organizer.
    let events = fixture::transport_events(&slice.log());
    assert!(
        events.iter().any(|event| matches!(
            event,
            fixture::TransportEvent::Submit { address, accepted, .. }
                if address.contains(fixture::ORGANIZER) && *accepted
        )),
        "the organizer was the recipient of the submission: {events:?}"
    );
}

/// The other two responses, and the verb each puts in the subject.
#[tokio::test]
async fn a_routed_rsvp_carries_the_verb_of_each_response() {
    let slice = Slice::with_fake_transport();
    let mut conn = slice.connect().await;

    for (response, verb) in [("tentative", "Tentative"), ("decline", "Declined")] {
        let status = run_operation(
            &mut conn,
            "calendar.rsvp",
            json!({
                "account": fixture::ACCOUNT,
                "selector": fixture::selector(fixture::ACCOUNT, fixture::INVITATION),
                "response": response,
            }),
        )
        .await;
        let result = succeeded(&status);
        assert_eq!(result["response"], response);
        assert_eq!(
            result["subject"],
            format!("{verb}: {}", fixture::INVITATION_SUMMARY),
            "`mp invite {response}` says {verb}: {result}"
        );
    }
}

// ---------------------------------------------------------------------------
// 4. `diagnostic.store_gc`
// ---------------------------------------------------------------------------

/// The sweep's whole report, which is what `report_sweep_outcome` renders and
/// therefore what has to cross: the cap, the size either side of the pass, the
/// decision and what it evicted.
#[tokio::test]
async fn store_gc_reports_the_decision_the_sweep_took() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let status = run_operation(
        &mut conn,
        "diagnostic.store_gc",
        json!({"account": fixture::ACCOUNT, "dry_run": false, "force": false}),
    )
    .await;
    let result = succeeded(&status);

    assert_eq!(result["account"], fixture::ACCOUNT);
    assert_eq!(
        result["dry_run"], false,
        "the pass echoes what it was asked"
    );
    for field in ["cap_bytes", "before_bytes", "after_bytes", "evicted_bytes"] {
        assert!(
            result[field].as_u64().is_some(),
            "the sweep report carries {field}: {result}"
        );
    }
    assert!(
        result["evicted"].as_array().is_some(),
        "the evicted blobs are a list, even when it is empty: {result}"
    );
    assert_eq!(
        result["decision"]["kind"], "under_cap",
        "the fixture is far under any cap, so this is the branch it takes: {result}"
    );
    assert!(
        result["decision"]["cleared_marker"].as_bool().is_some(),
        "the under-cap branch reports whether it cleared a stale marker: {result}"
    );

    assert_path_free("diagnostic.store_gc", result);
}

/// `--dry-run` decides and reports and unlinks nothing.
#[tokio::test]
async fn store_gc_dry_run_changes_no_blob() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let before = fixture::blob_files(slice.root(), fixture::ACCOUNT);
    assert!(!before.is_empty(), "the fixture has blobs to sweep");

    let status = run_operation(
        &mut conn,
        "diagnostic.store_gc",
        json!({"account": fixture::ACCOUNT, "dry_run": true, "force": false}),
    )
    .await;
    let result = succeeded(&status);
    assert_eq!(result["dry_run"], true);

    assert_eq!(
        fixture::blob_files(slice.root(), fixture::ACCOUNT),
        before,
        "a dry run leaves every blob where it was"
    );
}

/// The account refusals, which are the same two every store-opening method of
/// this slice makes.
#[tokio::test]
async fn store_gc_refuses_an_account_it_cannot_sweep() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let unknown = call_err(
        &mut conn,
        "diagnostic.store_gc",
        json!({"account": fixture::UNKNOWN_ACCOUNT, "dry_run": false, "force": false}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());

    let storeless = call_err(
        &mut conn,
        "diagnostic.store_gc",
        json!({"account": fixture::STORELESS_ACCOUNT, "dry_run": false, "force": false}),
    )
    .await;
    assert_eq!(storeless.code, ErrorCode::AccountNotReady.code());
    assert_eq!(data_of(&storeless)["account"], fixture::STORELESS_ACCOUNT);
}

// ---------------------------------------------------------------------------
// 5. `config.cutover`, `config.reset_secrets`, `config.oauth2_login`
// ---------------------------------------------------------------------------

/// `MIG-01`: the two halves of a cutover report, and the `--dry-run` that
/// writes not even the `id:` field.
#[tokio::test]
async fn config_cutover_reports_both_halves_and_writes_nothing_on_a_dry_run() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let before = fixture::legacy_draft_bytes(slice.root(), fixture::ACCOUNT);
    assert!(
        !before.is_empty() && !String::from_utf8_lossy(&before).contains("id:"),
        "the fixture's legacy draft carries no id: field"
    );

    let status = run_operation(
        &mut conn,
        "config.cutover",
        json!({"account": fixture::ACCOUNT, "dry_run": true}),
    )
    .await;
    let result = succeeded(&status);

    assert_eq!(result["account"], fixture::ACCOUNT);
    assert_eq!(result["dry_run"], true);

    let imported = result["drafts"]["imported"]
        .as_array()
        .unwrap_or_else(|| panic!("the drafts half lists what would get an id: {result}"));
    assert_eq!(
        imported.len(),
        1,
        "exactly the one draft with no id: field: {result}"
    );
    assert!(
        result["drafts"]["already_indexed"].as_u64().is_some(),
        "and how many already carried one: {result}"
    );
    for field in ["skipped", "collisions"] {
        assert!(
            result["drafts"][field].as_array().is_some(),
            "the drafts half reports {field}, empty or not: {result}"
        );
    }

    let remnants = result["remnants"]
        .as_array()
        .unwrap_or_else(|| panic!("the file-era half lists what is still on disk: {result}"));
    assert_eq!(
        remnants.len(),
        1,
        "one file-era mailbox directory: {result}"
    );
    assert_eq!(
        remnants[0]["path"].as_str(),
        Some(
            fixture::legacy_dir(slice.root(), fixture::ACCOUNT)
                .to_string_lossy()
                .as_ref()
        ),
        "the path is what the printed `rm -rf` line names: {result}"
    );
    assert_eq!(remnants[0]["md_files"], 1);
    assert!(
        remnants[0]["bytes"].as_u64().unwrap_or(0) > 0,
        "and its size, which the report renders as a human figure: {result}"
    );

    assert_eq!(
        fixture::legacy_draft_bytes(slice.root(), fixture::ACCOUNT),
        before,
        "a dry run writes not even the id: field"
    );
    assert!(
        fixture::legacy_tree_present(slice.root(), fixture::ACCOUNT),
        "a cutover deletes nothing, ever"
    );
}

/// The pass that does write: the `id:` field lands, and the file-era tree is
/// still there afterwards, because `mp cutover` deletes nothing by design.
#[tokio::test]
async fn config_cutover_writes_the_id_field_and_deletes_nothing() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let status = run_operation(
        &mut conn,
        "config.cutover",
        json!({"account": fixture::ACCOUNT, "dry_run": false}),
    )
    .await;
    succeeded(&status);

    let after =
        String::from_utf8_lossy(&fixture::legacy_draft_bytes(slice.root(), fixture::ACCOUNT))
            .into_owned();
    assert!(
        after.contains("id:"),
        "the import gave the draft an id: field, which is what makes it addressable:\n{after}"
    );
    assert!(
        fixture::legacy_tree_present(slice.root(), fixture::ACCOUNT),
        "a cutover deletes nothing, ever"
    );
}

/// `ACC-07`: the secret file and the token caches go, and the answer names
/// what went, in the order the CLI prints it.
#[tokio::test]
async fn config_reset_secrets_removes_the_secret_file_and_the_token_caches() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    // The secrets file is made the only way anything makes one: by storing a
    // password through the shipped `config.set_password`, whose ciphertext is
    // keyed to this machine. A file written by hand would not decrypt, and
    // every command that opens the backend would refuse before reaching the
    // reset.
    call(
        &mut conn,
        "config.set_password",
        json!({"account": fixture::ACCOUNT, "kind": "smtp", "value": "hunter2"}),
    )
    .await;
    assert_eq!(
        fixture::secrets_present(slice.root()),
        (true, true),
        "a stored password and a seeded token cache: two files to remove"
    );

    let result = call(&mut conn, "config.reset_secrets", json!({})).await;
    let removed: Vec<&str> = result["removed"]
        .as_array()
        .unwrap_or_else(|| panic!("config.reset_secrets answers what it removed: {result}"))
        .iter()
        .map(|value| value.as_str().expect("a removed entry is a path"))
        .collect();

    assert_eq!(
        removed,
        vec![
            fixture::secrets_file(slice.root())
                .to_string_lossy()
                .as_ref(),
            fixture::token_cache(slice.root(), fixture::GRAPH_ACCOUNT)
                .to_string_lossy()
                .as_ref(),
        ],
        "the secrets file first, then the token caches: the order the CLI prints"
    );
    assert_eq!(
        fixture::secrets_present(slice.root()),
        (false, false),
        "both files are gone"
    );

    // Idempotent: a second reset removes nothing and says so, rather than
    // failing because the first one worked.
    let again = call(&mut conn, "config.reset_secrets", json!({})).await;
    assert_eq!(
        again["removed"].as_array().map(Vec::len),
        Some(0),
        "nothing left to remove: {again}"
    );
}

/// `mp config oauth2-login` on an account that cannot do it: three refusals,
/// each in the sentence the CLI prints today.
#[tokio::test]
async fn config_oauth2_login_refuses_an_account_it_cannot_authenticate() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let unknown = call_err(
        &mut conn,
        "config.oauth2_login",
        json!({"account": fixture::UNKNOWN_ACCOUNT}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
    assert_eq!(data_of(&unknown)["account"], fixture::UNKNOWN_ACCOUNT);

    // A password account: there is no token to acquire, and the sentence says
    // which key to change.
    let password_account = call_err(
        &mut conn,
        "config.oauth2_login",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    assert_eq!(password_account.code, INVALID_PARAMS);
    assert!(
        password_account.message.contains("auth_method"),
        "the refusal names the key to change: {}",
        password_account.message
    );

    // An OAuth2-flavoured account with no usable client_id / tenant_id is the
    // other half of the same refusal, and the fixture's SMTP account is
    // exactly that shape.
    let refused = call_err(
        &mut conn,
        "config.oauth2_login",
        json!({"account": fixture::SMTP_ACCOUNT}),
    )
    .await;
    assert_eq!(refused.code, INVALID_PARAMS);
    assert_path_free("config.oauth2_login", &json!(refused.data));
}

/// The device-code block is a pure function of one progress payload.
///
/// There is no IdP in this repository, so the flow itself cannot run here. Its
/// *rendering* can, and it is the half a GUI has to reproduce: the daemon
/// reports `{phase, message}`, the client splits the message at its single
/// space and prints the block `src/oauth2.rs` prints today, byte for byte.
#[test]
fn the_device_code_block_renders_from_a_progress_payload() {
    let payload = json!({
        "operation_id": "op-1",
        "phase": DEVICE_CODE_PHASE,
        "done": 0,
        "total": Value::Null,
        "message": format!("{VERIFICATION_URI} {USER_CODE}"),
    });

    assert_eq!(payload["phase"], DEVICE_CODE_PHASE);
    assert!(
        payload["total"].is_null(),
        "a device-code wait has no total, and says so rather than omitting the field"
    );

    let message = payload["message"].as_str().expect("a message");
    let (uri, code) = message
        .split_once(' ')
        .expect("the two values are separated by the one space neither can contain");
    assert_eq!(uri, VERIFICATION_URI);
    assert_eq!(code, USER_CODE);

    assert_eq!(
        oauth2_device_code_lines(uri, code),
        DEVICE_CODE_BLOCK,
        "the block is `src/oauth2.rs`'s, verbatim, blank lines included"
    );
}

/// The two other lines the OAuth2 command prints, which are the client's too.
#[test]
fn the_oauth2_login_lines_are_the_ones_the_cli_prints_today() {
    assert_eq!(
        oauth2_start_line("tum", false),
        "\u{2139} Starting OAuth2 device code flow for account 'tum' (IMAP/SMTP)"
    );
    assert_eq!(
        oauth2_start_line("tum", true),
        "\u{2139} Starting OAuth2 device code flow for account 'tum' (Graph API)"
    );
    assert_eq!(
        oauth2_stored_line("tum"),
        "\u{2713} OAuth2 token acquired and cached for account 'tum'"
    );
}

/// Every result of this slice is path-free outside the four fields the CLI
/// prints a path out of.
#[tokio::test]
async fn every_admin_result_is_path_free_outside_the_named_fields() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let search = call(
        &mut conn,
        "contact.search",
        json!({"account": fixture::ACCOUNT, "query": "", "limit": 5}),
    )
    .await;
    let stats = call(
        &mut conn,
        "contact.stats",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    let calendar = run_operation(
        &mut conn,
        "calendar.rebuild",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    let gc = run_operation(
        &mut conn,
        "diagnostic.store_gc",
        json!({"account": fixture::ACCOUNT, "dry_run": true, "force": false}),
    )
    .await;
    let cutover = run_operation(
        &mut conn,
        "config.cutover",
        json!({"account": fixture::ACCOUNT, "dry_run": true}),
    )
    .await;

    for (what, value) in [
        ("contact.search", &search),
        ("contact.stats", &stats),
        ("calendar.rebuild", succeeded(&calendar)),
        ("diagnostic.store_gc", succeeded(&gc)),
        ("config.cutover", succeeded(&cutover)),
    ] {
        assert_path_free(what, value);
    }
}

// ---------------------------------------------------------------------------
// 6. Parity: the contacts, calendar, store and cutover commands
// ---------------------------------------------------------------------------

/// `CON-01` and `CON-02`: the human listing and the tab-delimited one that
/// mutt, aerc and vim already consume (`ANO-8`), for four shapes of query.
#[test]
fn mp_contacts_search_matches_the_oracle() {
    let slice = Slice::start();
    for args in [
        vec!["contacts", "search"],
        vec!["contacts", "search", "ivana"],
        vec!["contacts", "search", "aardvark"],
        vec!["contacts", "search", "-n", "2"],
        vec!["contacts", "search", "--account", fixture::OTHER_ACCOUNT],
        vec!["contacts", "search", "--parsable"],
        vec!["contacts", "search", "--parsable", "ivana"],
        vec!["contacts", "search", "--parsable", "aardvark"],
    ] {
        slice.both(&args);
    }
}

/// And the tab-delimited shape itself, asserted rather than merely compared: a
/// header line other tools discard, then one `address\tname` line per row.
#[test]
fn mp_contacts_search_parsable_is_tab_delimited() {
    let slice = Slice::start();
    let out = slice.both(&["contacts", "search", "--parsable"]);
    let text = stdout(&out);
    let mut lines = text.lines();

    let header = lines.next().expect("the header mutt discards");
    assert!(
        header.starts_with(char::is_numeric) && header.contains("results for"),
        "the first line is the count line: {header:?}"
    );
    let rows: Vec<&str> = lines.collect();
    assert!(!rows.is_empty(), "the fixture yields rows:\n{text}");
    for row in rows {
        let (address, _name) = row
            .split_once('\t')
            .unwrap_or_else(|| panic!("a parsable row is `address\\tname`: {row:?}"));
        assert!(
            address.contains('@'),
            "the first field is the address: {row:?}"
        );
    }
}

/// `CON-04`: the statistics block, for the default account and a named one.
#[test]
fn mp_contacts_stats_matches_the_oracle() {
    let slice = Slice::start();
    slice.both(&["contacts", "stats"]);
    slice.both(&["contacts", "stats", "--account", fixture::OTHER_ACCOUNT]);
    slice.both(&["contacts", "stats", "--account", fixture::UNKNOWN_ACCOUNT]);
}

/// `CON-03`: the rebuild, which writes, for one account and for the
/// all-accounts default.
#[test]
fn mp_contacts_rebuild_matches_the_oracle() {
    let mut slice = Slice::start();
    slice.both_mutating(
        &["contacts", "rebuild", "--account", fixture::ACCOUNT],
        |root| fixture::cached_addresses(root, fixture::ACCOUNT),
    );
    slice.both_mutating(&["contacts", "rebuild"], |root| {
        (
            fixture::cached_addresses(root, fixture::ACCOUNT),
            fixture::cached_addresses(root, fixture::OTHER_ACCOUNT),
        )
    });
}

/// `CAL-04`: the organizer-side fold, which writes nothing.
#[test]
fn mp_calendar_rebuild_matches_the_oracle() {
    let slice = Slice::start();
    slice.both(&["calendar", "rebuild"]);
    slice.both(&["calendar", "rebuild", "--account", fixture::ACCOUNT]);
    slice.both(&[
        "calendar",
        "rebuild",
        "--account",
        fixture::STORELESS_ACCOUNT,
    ]);
    slice.both(&["calendar", "rebuild", "--account", fixture::UNKNOWN_ACCOUNT]);
}

/// `SYN-08`: the retention sweep, in the three shapes the CLI offers, plus the
/// account with no store at all.
#[test]
fn mp_store_gc_matches_the_oracle() {
    let mut slice = Slice::start();
    slice.both(&["store", "gc", "--dry-run"]);
    slice.both(&["store", "gc", "-A", fixture::STORELESS_ACCOUNT]);
    slice.both_mutating(&["store", "gc"], |root| {
        fixture::blob_files(root, fixture::ACCOUNT)
    });
    slice.both_mutating(&["store", "gc", "--all-accounts"], |root| {
        (
            fixture::blob_files(root, fixture::ACCOUNT),
            fixture::blob_files(root, fixture::OTHER_ACCOUNT),
        )
    });
}

/// `MIG-01`: the report, and then the pass that writes the `id:` field.
#[test]
fn mp_cutover_matches_the_oracle() {
    let mut slice = Slice::start();
    slice.both(&["cutover", "--dry-run"]);
    slice.both(&["cutover", "--dry-run", "--account", fixture::ACCOUNT]);
    slice.both_mutating(&["cutover"], |root| {
        fixture::legacy_draft_shape(root, fixture::ACCOUNT)
    });
}

/// `CAL-01` through the CLI: every refusal, byte for byte, including `ANO-4`.
#[test]
fn mp_invite_refusals_match_the_oracle() {
    let slice = Slice::start();

    let unknown = slice.both(&[
        "invite",
        "accept",
        &fixture::selector(fixture::ACCOUNT, fixture::UNKNOWN_SELECTOR),
    ]);
    assert_eq!(unknown.status.code(), Some(1), "an unknown selector fails");

    let no_invite = slice.both(&[
        "invite",
        "tentative",
        &fixture::selector(fixture::ACCOUNT, fixture::NOT_AN_INVITATION),
    ]);
    assert_refused(
        &no_invite,
        &format!(
            "mp://{}/{}/{} carries no invitation to reply to",
            fixture::ACCOUNT,
            fixture::MAILBOX,
            fixture::NOT_AN_INVITATION
        ),
    );

    let graph = slice.both(&[
        "-A",
        fixture::GRAPH_ACCOUNT,
        "invite",
        "decline",
        &fixture::selector(fixture::GRAPH_ACCOUNT, fixture::INVITATION),
    ]);
    assert_refused(&graph, fixture::GRAPH_RSVP_REFUSAL);
}

// ---------------------------------------------------------------------------
// 7. Parity: the config commands and their prompts
// ---------------------------------------------------------------------------

/// `ACC-03`: the effective configuration, rendered from the shipped
/// `config.get`, byte for byte.
///
/// The fixture's configuration names every SMTP and IMAP port, and that is not
/// decoration: this tree defaults `smtp.port` to 465 where the `pre-daemon`
/// binary defaulted it to 0, so a port-less configuration makes the two
/// binaries print two different numbers for a reason that has nothing to do
/// with routing. `docs/daemon-protocol.md` chose that default deliberately
/// ("effective means after serde defaults"), so it is not the admin slice's to
/// undo; [`admin_fixture`]'s header records it.
#[test]
fn mp_config_show_matches_the_oracle() {
    let slice = Slice::start();
    slice.both(&["config", "show"]);
}

/// `ACC-04`: the one command of this slice that never needs a daemon, and
/// never starts one.
///
/// It is also the `UNMIGRATED` control row of `tests/daemon_parity_harness.rs`
/// from this unit on, because after the admin slice it is the only command in
/// the product that a daemon-era binary answers in process.
#[test]
fn mp_config_path_never_contacts_a_daemon() {
    let tmp = TempDir::new().expect("a temporary root");
    let ours = mp_no_daemon(&["config", "path"], tmp.path());
    let theirs = oracle_command(tmp.path())
        .args(["config", "path"])
        .output()
        .expect("run the oracle");

    assert_byte_identical(&ours, &theirs);
    assert_eq!(ours.status.code(), Some(0), "printing a path cannot fail");
    assert!(
        !daemon_is_listening(tmp.path()),
        "`mp config path` started a daemon, which is exactly what the no-daemon list forbids"
    );
    assert!(
        !socket_path(tmp.path()).exists(),
        "and left no socket behind"
    );
}

/// `ACC-01`: the wizard's first question is asked by the client, and a run
/// that cannot answer it stops there.
///
/// The routed side runs under `MAILYPOPPINS_DAEMON_REQUIRE`, which is what
/// pins the second half of the contract: the client learns that a
/// configuration file exists, and where, from `config.get` *before* it prompts,
/// so even the declined branch went through the daemon.
#[test]
fn mp_config_init_prompts_in_the_client() {
    let slice = Slice::start();
    let out = slice.both(&["config", "init"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "declining to overwrite is not an error"
    );
    let text = stdout(&out);
    assert!(
        text.contains("Overwrite? [y/N]") && text.contains("Cancelled."),
        "the prompt and the answer are both the client's:\n{text}"
    );
    assert!(
        !text.contains("=== mailypoppins setup ==="),
        "and the wizard never started:\n{text}"
    );
}

/// `ACC-02`: the same, for the command that refuses when there is no
/// configuration to add to.
///
/// A bare root, because the seeded one has a `config.toml` and the wizard
/// past this branch dials a mail server.
#[test]
fn mp_config_add_account_refuses_without_a_configuration() {
    let tmp = TempDir::new().expect("a temporary root");
    let daemon = DaemonFixture::start(tmp.path());

    let ours = daemon.mp_routed(&["config", "add-account"]);
    let theirs = oracle_command(tmp.path())
        .args(["config", "add-account"])
        .output()
        .expect("run the oracle");

    assert_byte_identical(&ours, &theirs);
    assert_eq!(ours.status.code(), Some(1));
    assert!(
        stderr(&ours).contains("Run `mp config init` first"),
        "the refusal names the command that fixes it:\n{}",
        stderr(&ours)
    );
    daemon.stop();
}

/// `ACC-05`: the password is read by the client, and a run that cannot read
/// one writes no secret.
///
/// There is no environment-variable path into this command and this slice does
/// not add one: `dialoguer::Password` on the client's terminal, then the value
/// crosses the socket once as `config.set_password`'s parameter.
#[test]
fn mp_config_set_password_reads_the_password_in_the_client() {
    let slice = Slice::start();
    assert!(
        !fixture::secrets_file(slice.root()).exists(),
        "nothing has stored a secret in this root yet"
    );

    let out = slice.both(&["config", "set-password", "smtp"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a password prompt with no terminal fails, on both binaries"
    );
    assert!(
        !fixture::secrets_file(slice.root()).exists(),
        "a run that never read a password never wrote one"
    );
}

/// `ACC-07` through the CLI, declined: the confirmation is the client's, and a
/// declined run removes nothing.
#[test]
fn mp_config_reset_secrets_declined_matches_the_oracle() {
    let slice = Slice::start();
    let out = slice.both(&["config", "reset-secrets"]);

    assert_eq!(out.status.code(), Some(0), "declining is not an error");
    let text = stdout(&out);
    assert!(
        text.contains("Continue? [y/N]") && text.contains("Cancelled."),
        "the prompt and the answer are both the client's:\n{text}"
    );
    assert_eq!(
        fixture::secrets_present(slice.root()),
        (false, true),
        "a declined reset removes nothing, the token cache included"
    );
}

/// And confirmed, through a piped stdin: the files go, in the order the CLI
/// prints them, and the re-prompt for each account is still the client's.
///
/// The run ends unsuccessfully on both binaries - the account walk asks for a
/// password and there is no terminal to ask on - which is precisely the
/// boundary this row pins: everything up to the prompt is the daemon's work
/// and the prompt itself is not.
#[test]
fn mp_config_reset_secrets_confirmed_matches_the_oracle() {
    let mut slice = Slice::start();

    let routed = slice.routed_with_stdin(&["config", "reset-secrets"], "y\n");
    let after_routed = fixture::secrets_present(slice.root());

    // Put the token cache back for the oracle's run, exactly as
    // `both_mutating` puts the `accounts/` tree back; the daemon holds no
    // handle on it, so no restart is needed.
    slice.stop_daemon();
    std::fs::write(
        fixture::token_cache(slice.root(), fixture::GRAPH_ACCOUNT),
        b"opaque-token",
    )
    .expect("restore");

    let direct = slice.oracle_with_stdin(&["config", "reset-secrets"], "y\n");
    let after_direct = fixture::secrets_present(slice.root());
    slice.start_daemon();

    assert_byte_identical(&routed, &direct);
    assert_eq!(
        after_routed, after_direct,
        "the same bytes and the same files left behind"
    );
    assert_eq!(
        after_routed,
        (false, false),
        "a confirmed reset removes the token cache it found"
    );
    assert!(
        stdout(&routed).contains("Removed"),
        "and names each one as it goes:\n{}",
        stdout(&routed)
    );
}

/// `ACC-06` through the CLI: the refusals, which are the whole of what can be
/// pinned without a provider.
#[test]
fn mp_config_oauth2_login_refusals_match_the_oracle() {
    let slice = Slice::start();
    slice.both(&[
        "config",
        "oauth2-login",
        "--account",
        fixture::UNKNOWN_ACCOUNT,
    ]);
    slice.both(&["config", "oauth2-login", "--account", fixture::ACCOUNT]);
    slice.both(&["config", "oauth2-login", "--account", fixture::SMTP_ACCOUNT]);
}

// ---------------------------------------------------------------------------
// 8. What routing itself has to hold
// ---------------------------------------------------------------------------

/// Two routed runs of a reading command agree with each other, not just with
/// the oracle: an answer that drifted between two identical calls would pass a
/// one-shot parity comparison and fail a user.
#[test]
fn two_routed_runs_agree() {
    let slice = Slice::start();
    for args in [
        vec!["contacts", "search"],
        vec!["contacts", "search", "--parsable"],
        vec!["contacts", "stats"],
        vec!["calendar", "rebuild"],
        vec!["store", "gc", "--dry-run"],
        vec!["cutover", "--dry-run"],
        vec!["config", "show"],
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
/// answer at all.
///
/// `mp config path` is deliberately absent from the list: it is the no-daemon
/// row, and [`mp_config_path_never_contacts_a_daemon`] holds it to the
/// opposite standard.
#[test]
fn no_admin_command_can_still_answer_without_a_daemon() {
    let tmp = TempDir::new().expect("a temporary root");
    fixture::seed(tmp.path());

    for args in [
        vec!["contacts", "search"],
        vec!["contacts", "stats"],
        vec!["contacts", "rebuild"],
        vec!["calendar", "rebuild"],
        vec!["store", "gc", "--dry-run"],
        vec!["cutover", "--dry-run"],
        vec!["config", "show"],
        vec!["config", "reset-secrets"],
        vec![
            "invite",
            "accept",
            &fixture::selector(fixture::ACCOUNT, fixture::INVITATION),
        ],
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
}
