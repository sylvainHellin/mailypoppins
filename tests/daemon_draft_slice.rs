//! The draft slice, moved onto the daemon (#0123, plan unit P4-U5).
//!
//! Ten commands answer from the drafts directory and the drafts index today
//! and must answer from the daemon tomorrow without a byte moving: `mp new`,
//! `mp list`, `mp validate`, `mp mark-approved`, `mp mark-draft`, `mp path`,
//! `mp edit`, `mp reply [--all]`, `mp forward`, and the bare-selector dry run
//! (`mp <selector>`). Nine methods carry them, and `docs/parity-matrix.md`
//! rows DFT-01 to DFT-09 name every one of them.
//!
//! This file is a **contract test**. It is written before the methods exist,
//! against the shapes fixed here and in `docs/daemon-protocol.md`, and it fails
//! to compile against today's tree; that failure is the proof the contract has
//! no stub behind it. The implementer (P4-U6) does not edit this file.
//!
//! # The surface under test
//!
//! ```text
//! draft.create    {account, name, no_signature?, signature?}      -> DraftCreated
//! draft.list      {account, status?}                              -> DraftListing
//! draft.validate  {account, id?|selector?}                        -> DraftValidation
//! draft.path      {account, id|selector}                          -> DraftLocation
//! draft.preview   {account, id|selector}                          -> DraftPreview
//! draft.approve   {account, id}                                   -> {account, id, path, status}
//! draft.demote    {account, id}                                   -> {account, id, path, status}
//! draft.reply     {account, source:{id|selector, mailbox?}, all?,
//!                  no_signature?, signature?}                     -> DraftCreated
//! draft.forward   {account, source:{id|selector, mailbox?},
//!                  no_signature?, signature?}                     -> DraftCreated
//! ```
//!
//! The result types are `mp_protocol::draft`, beside `mp_protocol::events`:
//! they are wire shapes, so they live in the crate a client links rather than
//! in the daemon crate a client must never link. Every one of them
//! `Serialize + Deserialize`, which is what lets this file assert on the typed
//! value the CLI renders from instead of on a JSON object that happens to look
//! like it.
//!
//! ```rust,ignore
//! // crates/mp-protocol/src/draft.rs  ->  mp_protocol::draft
//! // every struct: #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
//!
//! pub struct DraftSource { pub id: String, pub selector: String }
//!
//! pub struct DraftCreated {
//!     pub account: String, pub id: String, pub selector: String, pub path: String,
//!     pub source: Option<DraftSource>,   // absent for `draft.create`
//! }
//!
//! pub struct DraftEntry {
//!     pub id: String, pub selector: String, pub path: String, pub status: String,
//!     pub to: Option<String>, pub subject: Option<String>,
//!     pub valid: bool, pub ready: bool,
//! }
//! pub struct DraftSkip { pub path: String, pub error: String }
//! pub struct DraftCollision { pub id: String, pub kept: String, pub shadowed: String }
//! pub struct DraftListing {
//!     pub account: String, pub drafts: Vec<DraftEntry>,
//!     pub skipped: Vec<DraftSkip>, pub collisions: Vec<DraftCollision>,
//! }
//!
//! pub struct DraftReport {
//!     pub id: String, pub selector: String, pub valid: bool,
//!     pub error: Option<String>, pub warnings: Vec<String>,
//! }
//! pub struct DraftValidation { pub account: String, pub reports: Vec<DraftReport> }
//!
//! pub struct DraftLocation {
//!     pub account: String, pub id: String, pub selector: String,
//!     pub path: String, pub status: String,
//! }
//!
//! pub struct DraftPreview {
//!     pub account: String, pub id: String, pub selector: String,
//!     pub from: String, pub to: Option<String>, pub cc: Option<String>,
//!     pub bcc: Option<String>, pub subject: String,
//!     pub body: String, pub body_truncated: bool,
//!     pub status: String, pub valid: bool,
//!     pub error: Option<String>, pub warnings: Vec<String>,
//!     pub font_family: String, pub font_size: String,
//!     pub signature: Option<String>,
//! }
//! ```
//!
//! `draft.approve` and `draft.demote` answer a bare object rather than one of
//! these, because their four keys are frozen (below).
//!
//! ## One resolver, and mutators that take an id
//!
//! **`draft.path` is the family's resolver.** It takes what the user typed
//! (`<id>`, `drafts/<id>`, `mp://<account>/drafts/<id>`) and answers with the
//! canonical selector, the canonical path and the current status. Which
//! *account* a selector names stays a client-side decision, exactly as in the
//! read slice, because `Selector::parse` needs no store.
//!
//! **`draft.approve` and `draft.demote` take an `id`, and their result is the
//! four keys `draft.approve` already has** (`{account, id, path, status}`,
//! pinned by `tests/daemon_draft_watch.rs`, which asserts that key set
//! exactly). That is why the "already approved" line is not a field: `mp
//! mark-approved` resolves through `draft.path` first, which is the call that
//! tells it the *previous* status, and then approves by id. Two calls, one
//! frozen shape, and the `ℹ … is already approved` line still comes out
//! byte-identically.
//!
//! **The resolver must see a draft that was written a moment ago.** The
//! pre-daemon binary rebuilds the drafts index from the directory at the start
//! of every command, so `mp new` then `mp path`, or an agent writing a file
//! and `mp list` reading it, work with no delay
//! (`tests/cli_selector_contract.rs` pins both). A resolution that waited for
//! the watcher's settled inventory would answer "no such draft" for up to a
//! poll plus a debounce, so `draft.approve`'s watcher lookup (P3b-U10) becomes
//! the fast path and not the only one:
//! [`the_mutators_see_a_draft_that_was_created_a_moment_ago`] is the assertion.
//!
//! ## Paths, and the two fields that carry one
//!
//! Every result here is path-free except the fields whose whole point is a
//! path: `path` on `DraftCreated`, `DraftEntry`, `DraftLocation` and the
//! approve/demote results, and `path` on `DraftSkip` and `DraftCollision`.
//! Each of them is **absolute** and **under `<data>/accounts/<account>/drafts/`**,
//! which [`every_path_a_draft_method_returns_is_a_draft_path`] checks for all
//! of them at once. `mp path` is the one selector-to-path edge in the product
//! (DFT-06) and `mp edit` is a client-side editor session on the file it names,
//! so a path-free draft family would delete a documented feature.
//!
//! `DraftSkip` carries a path because a draft that will not parse has no id to
//! name it by: `⚠ 1 draft skipped …` names the file, which is the whole point
//! of #0080, and the client cannot reconstruct it.
//!
//! ## `draft.create`
//!
//! `name` is the user's argument verbatim; the `.md` suffixing rule
//! (`mp new note` -> `note.md`, `mp new note.txt` -> `note.txt`) is the
//! daemon's, because the daemon owns the directory the file lands in. The id
//! is minted by `store::drafts::new_id` and written into the file before the
//! selector is handed out, so a printed selector always resolves
//! ([`a_minted_id_has_the_shape_the_index_mints`] pins the shape rather than a
//! value, since a minted id cannot be compared against the oracle's).
//!
//! A name that is taken is `-32602` carrying `{account, name, path}`, and the
//! client prints the oracle's sentence, `A draft already exists at {path}`.
//!
//! ## `draft.list`
//!
//! The rows are `DraftEntry`, in the index's order (`mtime DESC, id ASC`),
//! which the client prints without re-sorting. `to` and `subject` stay
//! `Option`, which is where this row differs from the snapshot row
//! `mp_protocol::events::DraftChanged`: the snapshot flattens a missing
//! subject to `""`, and `mp list` prints the dimmed subject line only when the
//! file has one, so the two would not render the same.
//!
//! `skipped` and `collisions` are what the refresh reported, in the order the
//! CLI prints them, because a broken file is a warning about the directory
//! rather than a failure of the command: exit code 0, `⚠` lines on stderr.
//!
//! ## `draft.validate`
//!
//! One `DraftReport` per draft, in listing order, or one for the draft a
//! selector names. `error` is the single line `mp validate` prints after the
//! dash and `warnings` are the strings it joins with `", "`; the exit code
//! stays the client's decision (1 when any report is invalid), because the
//! daemon refuses nothing here -- an invalid draft is an answer, not an error.
//!
//! ## `draft.preview`
//!
//! `DraftPreview` is the record the dry run renders, field for field,
//! including the two quirks of `draft::preview_draft` that a client
//! re-implementing them would get wrong: the body is cut at 500 **characters**
//! while `body_truncated` is decided on 500 **bytes**, and `signature` is
//! always `null` for the CLI dry run because the body already carries the
//! signature (#0099). `from` is resolved: the `from:` field if the file has
//! one, otherwise the account's `default_from`.
//!
//! ## `draft.reply` and `draft.forward`
//!
//! The source is a *received* message, so it is addressed the way the read
//! slice addresses one: `{id}` (`"<mailbox>/<uid>"`) or `{selector, mailbox?}`,
//! never both and never neither. `all` is `--all`; `no_signature` and
//! `signature` mirror the two global flags, and the daemon resolves the
//! account's signature from configuration exactly as the client used to.
//! The result is a `DraftCreated` whose `source` names the message the draft
//! answers, because `mp reply` prints that selector before the new one.
//!
//! # Parity, and what proves routing
//!
//! Every command is compared against the `pre-daemon` oracle over the same
//! seeded root: stdout, stderr and exit code, across the flag combinations and
//! the error cases (an unknown draft id, an invalid draft, an already-sent
//! draft, an unknown message to reply to, a filesystem path where a selector
//! belongs, an unconfigured account). The routed side runs under
//! `MAILYPOPPINS_DAEMON_REQUIRE=1` ([`DaemonFixture::mp_routed`]), so a command
//! that quietly answered in process fails instead of passing for the daemon's
//! work, and [`no_draft_command_can_still_answer_without_a_daemon`] is the
//! proof that does not depend on the client noticing.
//!
//! Half of these commands write. Each parity case therefore runs over a
//! pristine fixture: `support::draft_fixture::Stash` copies the drafts
//! directories, restores them between the two binaries and again afterwards,
//! bytes and modification times both, so the two runs see the same directory
//! and the comparison stays literal over one root.
//!
//! Three commands mint an id, which the two binaries cannot agree on. Their
//! comparison masks every minted id
//! ([`mask_minted_ids`]) and is byte-identical after that; the id itself is
//! pinned by shape, and the file it names is asserted to exist.
//!
//! `mp edit` runs `$EDITOR` **in the client** on the path the daemon named.
//! [`mp_edit_hands_the_editor_the_path_mp_path_prints`] proves it with a stub
//! editor that records its argument: the recorded path is `mp path`'s output,
//! it exists, and both binaries record the same one.
//!
//! # The two legacy suites, twinned
//!
//! `tests/draft_integration.rs` and `tests/cli_selector_contract.rs` keep
//! running unchanged against the in-process path. Their CLI-visible assertions
//! are re-run here through [`Slice::for_each_binary`], which runs one assertion
//! body twice, once over the oracle's output and once over the routed one,
//! having first proved the two are byte-identical.
//!
//! **What could not be twinned**, and why:
//!
//! - `draft_integration.rs` is mostly a library test: 30 of its 34 assertions
//!   call `draft::create_reply_draft_from`, `draft::create_forward_draft_from`,
//!   `draft::rewrite_draft_recipients`, `draft::append_draft_attachment`,
//!   `draft::mark_as_approved` and friends directly, over a `SourceMessage`
//!   built by hand, and assert on the bytes of the file they wrote. No CLI
//!   surface shows most of that. What is twinned here is the half a command
//!   can reach: the reply and forward *files* `mp reply` and `mp forward`
//!   write (subject prefixing, the recorded `in-reply-to`, the carried
//!   attachments), and the approve/demote state machine including its two
//!   refusals.
//! - `test_forward_then_archive_source_keeps_attachment_resolvable` needs
//!   `mp archive`, which is the message-mutation slice (P4-U7). It stays where
//!   it is until then.
//! - `cli_selector_contract.rs`'s `an_externally_written_draft_shows_up_within_one_second`
//!   has two halves. The `mp list` half is twinned; the other half polls
//!   `store::drafts::fingerprint` in process, which is the TUI's reader and
//!   not a command.
//! - The same file's `a_cross_account_drafts_selector_deletes_from_its_own_account`
//!   and `a_send_bound_to_another_account_fails_loudly_…` need `mp delete` and
//!   `mp send`, which are P4-U7 and P4-U11. The cross-account *resolution*
//!   they rest on is twinned here through `mp path`.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::draft::{
    DraftCreated, DraftEntry, DraftListing, DraftLocation, DraftPreview, DraftValidation,
};
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::daemon::dispatch::{CancelScope, MethodKind};
use mailypoppins::daemon::methods::draft::DRAFT_METHOD_SPECS;

/// JSON-RPC's own "you asked for something I cannot honour", which is what
/// every addressing mistake in this family is.
const INVALID_PARAMS: i32 = -32602;

use support::draft_fixture as fixture;
use support::draft_fixture::Stash;
use support::parity::{
    assert_byte_identical, mp_command, mp_no_daemon, oracle, oracle_command, socket_path,
    DaemonFixture, SandboxRoot, EXIT_UNAVAILABLE, REQUIRE_ENV,
};

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// The nine methods of the slice, in the order their spec array declares them,
/// which is method-name order like every other family.
const DRAFT_METHODS: [&str; 9] = [
    "draft.approve",
    "draft.create",
    "draft.demote",
    "draft.forward",
    "draft.list",
    "draft.path",
    "draft.preview",
    "draft.reply",
    "draft.validate",
];

/// The family declares exactly those nine, checked while the tree compiles:
/// an array that still holds P3b-U10's single `draft.approve` fails here,
/// naming the constant, before a single test runs.
const _: () = assert!(DRAFT_METHOD_SPECS.len() == DRAFT_METHODS.len());

/// The five methods that change a file, so their answers carry a revision.
const DRAFT_COMMANDS: [&str; 5] = [
    "draft.approve",
    "draft.create",
    "draft.demote",
    "draft.forward",
    "draft.reply",
];

/// Every field of a listed draft row: the whole of what `mp list` and a GUI
/// draft list may say about a draft.
const ENTRY_FIELDS: [&str; 8] = [
    "id", "path", "ready", "selector", "status", "subject", "to", "valid",
];

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A seeded root, a daemon serving it, and the pre-daemon oracle beside it.
///
/// The field order is the drop order: the daemon dies before the sandbox stops
/// whatever else was left listening, which is before the directory both were
/// reading is removed.
struct Slice {
    daemon: DaemonFixture,
    root: SandboxRoot,
}

/// How a parity case compares the two binaries.
#[derive(Clone, Copy)]
enum Compare {
    /// stdout, stderr and exit code, literally.
    Bytes,
    /// The same, with every minted draft id masked, for the three commands
    /// whose whole job is to mint one.
    ModuloMintedIds,
}

/// One side of a twinned assertion: which binary produced it, what it printed
/// and what it left in the drafts directories.
struct Run<'a> {
    route: &'a str,
    out: &'a Output,
    created: &'a [PathBuf],
    root: &'a Path,
}

impl Run<'_> {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.out.stdout).into_owned()
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.out.stderr).into_owned()
    }

    /// The one file this run created, which is what `mp new`, `mp reply` and
    /// `mp forward` each leave behind.
    fn only_created(&self) -> &Path {
        assert_eq!(
            self.created.len(),
            1,
            "{}: exactly one draft was created, got {:?}",
            self.route,
            self.created
        );
        &self.created[0]
    }
}

impl Slice {
    /// Seed the store and the drafts, then start a daemon over them. In that
    /// order: the daemon loads `config.toml` once, at startup.
    fn start() -> Slice {
        let root = SandboxRoot::fresh();
        fixture::seed(root.path());
        let daemon = DaemonFixture::start(root.path());
        Slice { daemon, root }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    /// The client under test, made to prove it reached the daemon.
    fn routed(&self, args: &[&str]) -> Output {
        self.daemon.mp_routed(args)
    }

    /// The pre-daemon binary over the same root: the definition of parity.
    fn oracle(&self, args: &[&str]) -> Output {
        oracle(args, self.root())
    }

    /// Run both binaries over a pristine fixture, compare them, and run
    /// `check` over each side while the state that side produced is still on
    /// disk.
    fn each_route(&self, args: &[&str], compare: Compare, mut check: impl FnMut(&Run)) {
        let stash = Stash::take(self.root());

        let routed = self.routed(args);
        let created = stash.created(self.root());
        check(&Run {
            route: "routed",
            out: &routed,
            created: &created,
            root: self.root(),
        });
        stash.restore();

        let direct = self.oracle(args);
        let created = stash.created(self.root());
        check(&Run {
            route: "pre-daemon",
            out: &direct,
            created: &created,
            root: self.root(),
        });
        stash.restore();

        match compare {
            Compare::Bytes => assert_byte_identical(&routed, &direct),
            Compare::ModuloMintedIds => assert_identical_modulo_ids(&routed, &direct),
        }
    }

    /// The twin runner: a legacy assertion written once runs once against the
    /// pre-daemon path and once against the routed one.
    fn for_each_binary(&self, args: &[&str], check: impl FnMut(&Run)) {
        self.each_route(args, Compare::Bytes, check);
    }

    /// The same, for a command that mints an id.
    fn for_each_binary_modulo_ids(&self, args: &[&str], check: impl FnMut(&Run)) {
        self.each_route(args, Compare::ModuloMintedIds, check);
    }

    /// Prove the two binaries agree on `args`, and nothing else.
    fn parity(&self, args: &[&str]) {
        self.each_route(args, Compare::Bytes, |_| {});
    }

    /// The stdout both binaries produced for `args`, after proving they agree.
    fn agreed_stdout(&self, args: &[&str]) -> String {
        let mut text = None;
        self.each_route(args, Compare::Bytes, |run| {
            if run.route == "routed" {
                text = Some(run.stdout());
            }
        });
        text.expect("the routed run always produces stdout")
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

    /// The drafts directory of one account under this root.
    fn drafts_dir(&self, account: &str) -> PathBuf {
        fixture::drafts_dir(self.root(), account)
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

/// Call a method that must succeed, and deserialise its result.
async fn call_typed<T: serde::de::DeserializeOwned>(
    conn: &mut Connection,
    method: &str,
    params: Value,
) -> T {
    let result = call(conn, method, params).await;
    serde_json::from_value(result.clone())
        .unwrap_or_else(|e| panic!("{method} answered something else than its type: {e}; {result}"))
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

/// The keys of a JSON object, sorted.
fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("{value} is an object"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// Whether `id` has the shape `store::drafts::new_id` mints: sixteen hex
/// characters, the first one a letter, which is the shape that cannot be read
/// back out of YAML as a number (#0077).
fn is_minted_id(id: &str) -> bool {
    id.len() == 16
        && id.starts_with(|c: char| ('a'..='f').contains(&c))
        && id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
}

/// Every minted-looking id in `text`, replaced by a fixed token.
///
/// The mask is deliberately broad: it also masks the fixture's own ids, which
/// are written in the minted shape on purpose. That is why it is used only for
/// the three commands whose output *contains* an id neither binary can
/// predict, and never as a substitute for a byte comparison.
fn mask_minted_ids(text: &str) -> String {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern =
        PATTERN.get_or_init(|| Regex::new(r"\b[a-f][0-9a-f]{15}\b").expect("a valid pattern"));
    pattern.replace_all(text, "<minted-id>").into_owned()
}

/// [`assert_byte_identical`] with every minted id masked on both sides.
fn assert_identical_modulo_ids(left: &Output, right: &Output) {
    assert_eq!(
        left.status.code(),
        right.status.code(),
        "exit codes differ: {:?} vs {:?}",
        left.status.code(),
        right.status.code()
    );
    for (stream, l, r) in [
        ("stdout", &left.stdout, &right.stdout),
        ("stderr", &left.stderr, &right.stderr),
    ] {
        let masked_left = mask_minted_ids(&String::from_utf8_lossy(l));
        let masked_right = mask_minted_ids(&String::from_utf8_lossy(r));
        assert_eq!(
            masked_left, masked_right,
            "{stream} differs once the minted ids are masked"
        );
    }
}

/// Nothing a draft method answers may name a file that is not a draft.
///
/// A `path` field is the exception the family is allowed (`mp path` is the one
/// selector-to-path edge), and this is the shape of that exception: absolute,
/// and under the account's own drafts directory. Everything else - the store,
/// the blobs, the runtime directory - stays invisible.
fn assert_paths_are_draft_paths(what: &str, value: &Value, root: &Path, account: &str) {
    let text = value.to_string();
    for needle in ["store.sqlite3", "/blobs/", "/runtime/"] {
        assert!(
            !text.contains(needle),
            "{what} leaked {needle:?} into its result: {text}"
        );
    }
    let drafts = fixture::drafts_dir(root, account);
    for path in path_fields(value) {
        assert!(
            Path::new(&path).is_absolute(),
            "{what} answered a relative path {path}"
        );
        assert!(
            Path::new(&path).starts_with(&drafts),
            "{what} answered {path}, which is not under {}",
            drafts.display()
        );
    }
}

/// Every string under a `path`, `kept` or `shadowed` key, at any depth.
fn path_fields(value: &Value) -> Vec<String> {
    let mut found = Vec::new();
    collect_path_fields(value, &mut found);
    found
}

fn collect_path_fields(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                match (key.as_str(), child.as_str()) {
                    ("path" | "kept" | "shadowed", Some(text)) => found.push(text.to_string()),
                    _ => collect_path_fields(child, found),
                }
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_path_fields(item, found)),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// 1. The methods themselves
// ---------------------------------------------------------------------------

/// The nine declarations, and the four facts each one fixes: the wire name,
/// the kind, the first protocol version and what a disconnect does to it.
///
/// Four are queries and five are commands, which is the split between "reads
/// the directory" and "writes a file". All nine are durable: a draft written
/// half way because its caller hung up is exactly what the family must never
/// produce, and none of them runs long enough to be worth cancelling.
#[test]
fn the_draft_slice_declares_four_queries_and_five_commands() {
    let names: Vec<&str> = DRAFT_METHOD_SPECS.iter().map(|s| s.name).collect();
    assert_eq!(names, DRAFT_METHODS, "the slice serves exactly these nine");

    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the array is in method-name order");

    for spec in DRAFT_METHOD_SPECS {
        let expected = if DRAFT_COMMANDS.contains(&spec.name) {
            MethodKind::Command
        } else {
            MethodKind::Query
        };
        assert_eq!(spec.kind, expected, "{} is a {expected:?}", spec.name);
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
/// the capability list from the dispatcher, so the nine names appearing there
/// is what says they are registered rather than merely declared.
#[tokio::test]
async fn the_daemon_advertises_every_draft_method() {
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

    for method in DRAFT_METHODS {
        assert!(
            result.capabilities.iter().any(|c| c == method),
            "{method} is served, so it is advertised: {:?}",
            result.capabilities
        );
    }
}

// ---------------------------------------------------------------------------
// 2. `draft.create`
// ---------------------------------------------------------------------------

/// The skeleton lands in the account's drafts directory, under the name the
/// user gave, with the id already in the file.
#[tokio::test]
async fn draft_create_writes_a_skeleton_and_names_it() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "draft.create",
        json!({"account": fixture::ACCOUNT, "name": "quartalsbericht"}),
    )
    .await;
    assert_paths_are_draft_paths("draft.create", &result, slice.root(), fixture::ACCOUNT);
    let created: DraftCreated =
        serde_json::from_value(result.clone()).expect("the result is a DraftCreated");

    assert_eq!(created.account, fixture::ACCOUNT);
    assert!(
        is_minted_id(&created.id),
        "{} is not the shape store::drafts::new_id mints",
        created.id
    );
    assert_eq!(
        created.selector,
        fixture::selector(fixture::ACCOUNT, &created.id),
        "the selector is the canonical one, which is the form every command prints"
    );
    assert_eq!(
        created.path,
        slice
            .drafts_dir(fixture::ACCOUNT)
            .join("quartalsbericht.md")
            .display()
            .to_string(),
        "`mp new note` writes note.md, and the daemon owns that rule because it owns the directory"
    );
    assert!(
        created.source.is_none(),
        "a draft made from nothing names no source"
    );

    let document = fs::read_to_string(&created.path).expect("the file the result names exists");
    assert!(
        document.contains(&format!("id: {}", created.id)),
        "the id is in the file before the selector is handed out, so a printed selector \
         resolves the moment it is printed: {document}"
    );
    assert!(
        document.contains("status: draft"),
        "a new draft starts as a draft: {document}"
    );

    // An explicit extension is taken as given.
    let created: DraftCreated = call_typed(
        &mut conn,
        "draft.create",
        json!({"account": fixture::ACCOUNT, "name": "notiz.markdown"}),
    )
    .await;
    assert!(
        created.path.ends_with("notiz.markdown"),
        "a name with an extension keeps it: {}",
        created.path
    );
}

/// The two refusals: a name that is taken, and an account nothing configures.
#[tokio::test]
async fn draft_create_refuses_a_taken_name_and_an_unknown_account() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let error = call_err(
        &mut conn,
        "draft.create",
        json!({"account": fixture::ACCOUNT, "name": "angebot"}),
    )
    .await;
    assert_eq!(
        error.code, INVALID_PARAMS,
        "a name that is taken is a parameter the daemon cannot honour: {error:?}"
    );
    let data = data_of(&error);
    assert_eq!(data["account"], json!(fixture::ACCOUNT));
    assert_eq!(data["name"], json!("angebot"));
    assert_eq!(
        data["path"],
        json!(slice
            .drafts_dir(fixture::ACCOUNT)
            .join("angebot.md")
            .display()
            .to_string()),
        "the refusal names the file that is in the way, which is the sentence the CLI prints"
    );
    assert_eq!(
        fs::read_to_string(slice.drafts_dir(fixture::ACCOUNT).join("angebot.md"))
            .expect("read it back"),
        fixture::document(
            fixture::VALID,
            "robin@example.com",
            "Angebot",
            "draft",
            "Body.\n"
        ),
        "a refused create writes nothing"
    );

    let error = call_err(
        &mut conn,
        "draft.create",
        json!({"account": fixture::UNKNOWN_ACCOUNT, "name": "egal"}),
    )
    .await;
    assert_eq!(error.code, ErrorCode::AccountUnknown.code());
    assert_eq!(
        data_of(&error),
        &json!({"account": fixture::UNKNOWN_ACCOUNT})
    );
}

/// A minted id is pinned by shape, because no test can pin its value: two
/// binaries minting one for the same draft will never agree, which is exactly
/// why the parity comparison of a minting command masks it.
#[tokio::test]
async fn a_minted_id_has_the_shape_the_index_mints() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let mut seen = Vec::new();
    for name in ["eins", "zwei", "drei"] {
        let created: DraftCreated = call_typed(
            &mut conn,
            "draft.create",
            json!({"account": fixture::ACCOUNT, "name": name}),
        )
        .await;
        assert!(is_minted_id(&created.id), "{} is minted", created.id);
        assert_eq!(
            mask_minted_ids(&created.selector),
            format!("mp://{}/drafts/<minted-id>", fixture::ACCOUNT),
            "the mask the parity comparison uses covers a real minted id"
        );
        seen.push(created.id);
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 3, "three creations mint three ids");
}

// ---------------------------------------------------------------------------
// 3. `draft.list`
// ---------------------------------------------------------------------------

/// Every row `mp list` prints, with the fields it prints them from and no
/// others.
#[tokio::test]
async fn draft_list_carries_every_row_the_listing_prints() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "draft.list",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    assert_paths_are_draft_paths("draft.list", &result, slice.root(), fixture::ACCOUNT);
    assert_eq!(
        sorted_keys(&result),
        vec!["account", "collisions", "drafts", "skipped"],
        "the listing carries its rows and the two things the refresh reported"
    );
    for row in result["drafts"].as_array().expect("drafts is an array") {
        assert_eq!(
            sorted_keys(row),
            ENTRY_FIELDS
                .iter()
                .map(|k| k.to_string())
                .collect::<Vec<_>>(),
            "a listed row has exactly the documented fields"
        );
    }

    let listing: DraftListing =
        serde_json::from_value(result).expect("the result is a DraftListing");
    assert_eq!(listing.account, fixture::ACCOUNT);
    let by_id = |id: &str| -> &DraftEntry {
        listing
            .drafts
            .iter()
            .find(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("{id} is listed"))
    };

    let valid = by_id(fixture::VALID);
    assert_eq!(valid.status, "draft");
    assert_eq!(valid.to.as_deref(), Some("robin@example.com"));
    assert_eq!(valid.subject.as_deref(), Some("Angebot"));
    assert_eq!(
        valid.selector,
        fixture::selector(fixture::ACCOUNT, fixture::VALID)
    );
    assert!(valid.valid, "it parses");
    assert!(valid.ready, "it would send");

    assert_eq!(by_id(fixture::APPROVED).status, "approved");
    assert_eq!(by_id(fixture::SENT).status, "sent");

    let no_subject = by_id(fixture::NO_SUBJECT);
    assert!(
        no_subject.valid,
        "a draft with no subject parses perfectly; `valid` is about the file"
    );
    assert!(
        !no_subject.ready,
        "and `ready` is about whether it would send, which it would not"
    );
    assert_eq!(
        no_subject.subject.as_deref(),
        Some(""),
        "an empty subject stays an empty subject rather than becoming absent"
    );

    assert_eq!(
        listing.drafts.len(),
        4,
        "the file that will not parse is not a row: {:?}",
        listing.drafts
    );
}

/// The file the refresh could not parse is named rather than dropped (#0080),
/// and it is named by path because it has no id to be named by.
#[tokio::test]
async fn draft_list_names_the_file_it_could_not_parse() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let listing: DraftListing = call_typed(
        &mut conn,
        "draft.list",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    assert_eq!(listing.collisions.len(), 0, "nothing here shares an id");
    assert_eq!(listing.skipped.len(), 1, "{:?}", listing.skipped);
    let skipped = &listing.skipped[0];
    assert_eq!(
        skipped.path,
        slice
            .drafts_dir(fixture::ACCOUNT)
            .join(fixture::UNPARSEABLE_FILE)
            .display()
            .to_string()
    );
    assert!(
        !skipped.error.is_empty() && !skipped.error.contains('\n'),
        "the parse failure is one line, because it goes into a list: {:?}",
        skipped.error
    );
}

/// `--status` filters, and an account with no drafts at all is an empty
/// listing rather than a refusal.
#[tokio::test]
async fn draft_list_filters_by_status() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for (status, expected) in [
        ("draft", vec![fixture::NO_SUBJECT, fixture::VALID]),
        ("approved", vec![fixture::APPROVED]),
        ("sent", vec![fixture::SENT]),
    ] {
        let listing: DraftListing = call_typed(
            &mut conn,
            "draft.list",
            json!({"account": fixture::ACCOUNT, "status": status}),
        )
        .await;
        let mut ids: Vec<&str> = listing.drafts.iter().map(|e| e.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, expected, "--status {status}");
    }

    let listing: DraftListing = call_typed(
        &mut conn,
        "draft.list",
        json!({"account": fixture::OTHER_ACCOUNT}),
    )
    .await;
    assert_eq!(
        listing.drafts.len(),
        1,
        "the second account has its own directory and nothing of alpha's"
    );
    assert_eq!(listing.drafts[0].id, fixture::BETA_DRAFT);
}

/// The two account refusals every family shares, and a status nothing spells.
#[tokio::test]
async fn draft_list_refuses_an_unknown_account_and_an_unknown_status() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let error = call_err(
        &mut conn,
        "draft.list",
        json!({"account": fixture::UNKNOWN_ACCOUNT}),
    )
    .await;
    assert_eq!(error.code, ErrorCode::AccountUnknown.code());
    assert_eq!(
        data_of(&error),
        &json!({"account": fixture::UNKNOWN_ACCOUNT})
    );

    let error = call_err(
        &mut conn,
        "draft.list",
        json!({"account": fixture::ACCOUNT, "status": "gesendet"}),
    )
    .await;
    assert_eq!(
        error.code, INVALID_PARAMS,
        "the three statuses are the three the CLI's own enum spells: {error:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. `draft.validate`
// ---------------------------------------------------------------------------

/// One report per draft, or one for the draft a selector names, with the
/// diagnostics `mp validate` prints.
#[tokio::test]
async fn draft_validate_reports_every_draft_or_the_one_asked_for() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let all: DraftValidation = call_typed(
        &mut conn,
        "draft.validate",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    assert_eq!(
        all.reports.len(),
        4,
        "every indexed draft is reported, in listing order: {:?}",
        all.reports
    );

    let one: DraftValidation = call_typed(
        &mut conn,
        "draft.validate",
        json!({"account": fixture::ACCOUNT, "selector": fixture::VALID}),
    )
    .await;
    assert_eq!(one.reports.len(), 1);
    let report = &one.reports[0];
    assert_eq!(report.id, fixture::VALID);
    assert_eq!(
        report.selector,
        fixture::selector(fixture::ACCOUNT, fixture::VALID)
    );
    assert!(report.valid, "the fixture's sendable draft validates");
    assert!(report.error.is_none(), "a valid draft has no error line");
    assert!(
        report.warnings.is_empty(),
        "and nothing to warn about: {:?}",
        report.warnings
    );

    let broken: DraftValidation = call_typed(
        &mut conn,
        "draft.validate",
        json!({"account": fixture::ACCOUNT, "id": fixture::NO_SUBJECT}),
    )
    .await;
    let report = &broken.reports[0];
    assert!(!report.valid);
    assert_eq!(
        report.error.as_deref(),
        Some("Missing 'subject' field"),
        "the error is the one line the CLI prints after the dash"
    );
}

/// An invalid draft is an answer, not a refusal: the exit code is the client's
/// decision and the daemon reports every draft it was asked about.
#[tokio::test]
async fn draft_validate_refuses_only_what_it_cannot_address() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let error = call_err(
        &mut conn,
        "draft.validate",
        json!({"account": fixture::ACCOUNT, "id": fixture::UNKNOWN_ID}),
    )
    .await;
    assert_eq!(error.code, INVALID_PARAMS);
    assert_eq!(
        data_of(&error),
        &json!({"account": fixture::ACCOUNT, "id": fixture::UNKNOWN_ID})
    );

    let error = call_err(
        &mut conn,
        "draft.validate",
        json!({"account": fixture::ACCOUNT, "id": fixture::VALID, "selector": fixture::VALID}),
    )
    .await;
    assert_eq!(
        error.code, INVALID_PARAMS,
        "a draft is addressed by id or by selector, never by both: {error:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. `draft.path`, the resolver
// ---------------------------------------------------------------------------

/// Every spelling a user may type resolves to one draft, and the answer
/// carries the canonical selector, the canonical path and the current status,
/// which are the three things a selector-taking command needs before it acts.
#[tokio::test]
async fn draft_path_resolves_every_spelling_to_the_canonical_answer() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let expected = slice.drafts_dir(fixture::ACCOUNT).join("angebot.md");
    for spelling in [
        fixture::VALID.to_string(),
        format!("drafts/{}", fixture::VALID),
        fixture::selector(fixture::ACCOUNT, fixture::VALID),
    ] {
        let result = call(
            &mut conn,
            "draft.path",
            json!({"account": fixture::ACCOUNT, "selector": spelling}),
        )
        .await;
        assert_paths_are_draft_paths("draft.path", &result, slice.root(), fixture::ACCOUNT);
        let location: DraftLocation =
            serde_json::from_value(result).expect("the result is a DraftLocation");
        assert_eq!(location.account, fixture::ACCOUNT);
        assert_eq!(location.id, fixture::VALID);
        assert_eq!(
            location.selector,
            fixture::selector(fixture::ACCOUNT, fixture::VALID),
            "{spelling} resolves to the canonical selector"
        );
        assert_eq!(
            location.path,
            expected.display().to_string(),
            "{spelling} resolves to the canonical path"
        );
        assert_eq!(
            location.status, "draft",
            "the status is here because `mp mark-approved` needs the previous one"
        );
    }

    // The id form, which is what a GUI holding a listed row has.
    let location: DraftLocation = call_typed(
        &mut conn,
        "draft.path",
        json!({"account": fixture::ACCOUNT, "id": fixture::APPROVED}),
    )
    .await;
    assert_eq!(location.status, "approved");
}

/// What the resolver refuses: an id nothing resolves to, both forms at once,
/// neither form, and an account nothing configures.
#[tokio::test]
async fn draft_path_refuses_what_it_cannot_address() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let error = call_err(
        &mut conn,
        "draft.path",
        json!({"account": fixture::ACCOUNT, "id": fixture::UNKNOWN_ID}),
    )
    .await;
    assert_eq!(error.code, INVALID_PARAMS);
    assert_eq!(
        data_of(&error),
        &json!({"account": fixture::ACCOUNT, "id": fixture::UNKNOWN_ID})
    );

    for params in [
        json!({"account": fixture::ACCOUNT}),
        json!({"account": fixture::ACCOUNT, "id": fixture::VALID, "selector": fixture::VALID}),
    ] {
        let error = call_err(&mut conn, "draft.path", params.clone()).await;
        assert_eq!(error.code, INVALID_PARAMS, "{params} addresses no draft");
    }

    let error = call_err(
        &mut conn,
        "draft.path",
        json!({"account": fixture::UNKNOWN_ACCOUNT, "id": fixture::VALID}),
    )
    .await;
    assert_eq!(error.code, ErrorCode::AccountUnknown.code());
}

// ---------------------------------------------------------------------------
// 6. `draft.approve` and `draft.demote`
// ---------------------------------------------------------------------------

/// The demotion mirrors the approval P3b-U10 already serves: the same four
/// keys, the same surgical rewrite of one line, and the same event afterwards.
#[tokio::test]
async fn draft_demote_mirrors_draft_approve() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let path = slice.drafts_dir(fixture::ACCOUNT).join("freigabe.md");
    let before = fs::read_to_string(&path).expect("the approved fixture draft");

    let result = call(
        &mut conn,
        "draft.demote",
        json!({"account": fixture::ACCOUNT, "id": fixture::APPROVED}),
    )
    .await;
    assert_eq!(
        sorted_keys(&result),
        vec!["account", "id", "path", "status"],
        "the demotion answers exactly what the approval answers"
    );
    assert_eq!(result["status"], json!("draft"));
    assert_eq!(result["path"], json!(path.display().to_string()));
    assert_eq!(
        fs::read_to_string(&path).expect("read it back"),
        before.replace("status: approved", "status: draft"),
        "one line is rewritten and nothing is re-serialised"
    );

    // And back, through the method that already existed.
    let result = call(
        &mut conn,
        "draft.approve",
        json!({"account": fixture::ACCOUNT, "id": fixture::APPROVED}),
    )
    .await;
    assert_eq!(result["status"], json!("approved"));
    assert_eq!(
        fs::read_to_string(&path).expect("read it back"),
        before,
        "the round trip is the identity"
    );
}

/// A sent draft has left the pipeline: neither method may rewrite it, and the
/// refusal names the status so the client can print the oracle's sentence.
#[tokio::test]
async fn approve_and_demote_refuse_a_sent_draft() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let path = slice.drafts_dir(fixture::ACCOUNT).join("verschickt.md");
    let before = fs::read_to_string(&path).expect("the sent fixture draft");

    for method in ["draft.approve", "draft.demote"] {
        let error = call_err(
            &mut conn,
            method,
            json!({"account": fixture::ACCOUNT, "id": fixture::SENT}),
        )
        .await;
        assert_eq!(error.code, INVALID_PARAMS, "{method}");
        assert_eq!(
            data_of(&error),
            &json!({"account": fixture::ACCOUNT, "id": fixture::SENT, "status": "sent"}),
            "{method} names the status that refused it"
        );
    }
    assert_eq!(
        fs::read_to_string(&path).expect("read it back"),
        before,
        "a refused rewrite writes nothing"
    );
}

/// Approving a draft that already is approved is not a refusal: the file is
/// left alone and the answer is the same one. The `ℹ … is already approved`
/// line is the client's, decided from the status `draft.path` gave it before
/// the call.
#[tokio::test]
async fn approving_an_approved_draft_is_a_no_op_with_an_answer() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let path = slice.drafts_dir(fixture::ACCOUNT).join("freigabe.md");
    let before = fs::read_to_string(&path).expect("the approved fixture draft");

    let location: DraftLocation = call_typed(
        &mut conn,
        "draft.path",
        json!({"account": fixture::ACCOUNT, "id": fixture::APPROVED}),
    )
    .await;
    assert_eq!(
        location.status, "approved",
        "this is the call that tells the client which line to print"
    );

    let result = call(
        &mut conn,
        "draft.approve",
        json!({"account": fixture::ACCOUNT, "id": fixture::APPROVED}),
    )
    .await;
    assert_eq!(result["status"], json!("approved"));
    assert_eq!(
        fs::read_to_string(&path).expect("read it back"),
        before,
        "nothing was rewritten"
    );
}

/// The freshness rule the CLI has always had: a draft created a millisecond
/// ago is addressable, without waiting for a watcher poll.
///
/// The pre-daemon binary rebuilds the index at the start of every command, so
/// `mp new … && mp mark-approved …` works. A resolution that only consulted
/// the watcher's settled inventory would answer "no such draft" for up to a
/// poll plus a debounce, and that would be a regression no parity table would
/// catch, because the oracle would fail the same way if it were slow.
#[tokio::test]
async fn the_mutators_see_a_draft_that_was_created_a_moment_ago() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let created: DraftCreated = call_typed(
        &mut conn,
        "draft.create",
        json!({"account": fixture::ACCOUNT, "name": "sofort"}),
    )
    .await;

    let location: DraftLocation = call_typed(
        &mut conn,
        "draft.path",
        json!({"account": fixture::ACCOUNT, "selector": created.selector}),
    )
    .await;
    assert_eq!(location.path, created.path);

    let result = call(
        &mut conn,
        "draft.approve",
        json!({"account": fixture::ACCOUNT, "id": created.id}),
    )
    .await;
    assert_eq!(
        result["status"],
        json!("approved"),
        "approving a draft the watcher has not settled yet still works"
    );

    // The same holds for a file another process wrote, which is the agent
    // workflow `tests/cli_selector_contract.rs` pins.
    let path = slice.drafts_dir(fixture::ACCOUNT).join("von-aussen.md");
    let id = "a00000000000beef";
    fs::write(
        &path,
        fixture::document(id, "robin@example.com", "Von aussen", "draft", "Body.\n"),
    )
    .expect("write the agent's draft");
    let location: DraftLocation = call_typed(
        &mut conn,
        "draft.path",
        json!({"account": fixture::ACCOUNT, "id": id}),
    )
    .await;
    assert_eq!(location.path, path.display().to_string());
}

// ---------------------------------------------------------------------------
// 7. `draft.preview`
// ---------------------------------------------------------------------------

/// The record the bare-selector dry run renders, field for field.
#[tokio::test]
async fn draft_preview_carries_the_record_the_dry_run_prints() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "draft.preview",
        json!({"account": fixture::ACCOUNT, "selector": fixture::VALID}),
    )
    .await;
    assert_paths_are_draft_paths("draft.preview", &result, slice.root(), fixture::ACCOUNT);
    let preview: DraftPreview =
        serde_json::from_value(result).expect("the result is a DraftPreview");

    assert_eq!(preview.account, fixture::ACCOUNT);
    assert_eq!(preview.id, fixture::VALID);
    assert_eq!(
        preview.selector,
        fixture::selector(fixture::ACCOUNT, fixture::VALID)
    );
    assert_eq!(
        preview.from, "alpha@example.com",
        "the `from:` field when the file has one, the account's default_from otherwise"
    );
    assert_eq!(preview.to.as_deref(), Some("robin@example.com"));
    assert_eq!(preview.cc, None, "an absent Cc prints no Cc line");
    assert_eq!(preview.bcc, None);
    assert_eq!(preview.subject, "Angebot");
    assert_eq!(
        preview.body.trim(),
        "Body.",
        "the body as the file spells it, which is what the preview block prints"
    );
    assert!(!preview.body_truncated);
    assert_eq!(preview.status, "draft");
    assert!(preview.valid);
    assert_eq!(preview.error, None);
    assert!(preview.warnings.is_empty());
    assert_eq!(
        preview.font_family, "Helvetica, Arial, sans-serif",
        "the settings block prints the configured font, defaulted here"
    );
    assert_eq!(preview.font_size, "12pt");
    assert_eq!(
        preview.signature, None,
        "the dry run passes no signature, because the body already carries it (#0099)"
    );

    // The invalid draft's diagnostics travel the same way, and the preview is
    // still an answer: a draft that does not validate is previewable.
    let preview: DraftPreview = call_typed(
        &mut conn,
        "draft.preview",
        json!({"account": fixture::ACCOUNT, "id": fixture::NO_SUBJECT}),
    )
    .await;
    assert!(!preview.valid);
    assert_eq!(preview.error.as_deref(), Some("Missing 'subject' field"));
}

/// The two cut-offs of `draft::preview_draft`, which are not the same number:
/// the body is cut at 500 *characters* and the trailing `...` is decided on
/// 500 *bytes*. A client that re-derived either would drift; the daemon
/// decides both.
#[tokio::test]
async fn draft_preview_cuts_the_body_the_way_the_renderer_does() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    // 400 two-byte characters: 400 chars, 800 bytes. Under the character
    // cut-off and over the byte one, which is the case that tells the two
    // rules apart.
    let body = "ä".repeat(400);
    let id = "a0000000000000f2";
    fs::write(
        slice.drafts_dir(fixture::ACCOUNT).join("lang.md"),
        fixture::document(
            id,
            "robin@example.com",
            "Lang",
            "draft",
            &format!("{body}\n"),
        ),
    )
    .expect("write the long draft");

    let preview: DraftPreview = call_typed(
        &mut conn,
        "draft.preview",
        json!({"account": fixture::ACCOUNT, "id": id}),
    )
    .await;
    assert!(
        (400..500).contains(&preview.body.chars().count()),
        "the body is under the 500-character cut, so it arrives whole: {} characters",
        preview.body.chars().count()
    );
    assert!(
        preview.body_truncated,
        "and over the 500-byte one, so the renderer's `...` line is on"
    );
}

// ---------------------------------------------------------------------------
// 8. `draft.reply` and `draft.forward`
// ---------------------------------------------------------------------------

/// A reply is built from a stored message, addressed the way the read slice
/// addresses one, and the answer names both the new draft and the message it
/// answers.
#[tokio::test]
async fn draft_reply_builds_a_draft_from_a_stored_message() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        "draft.reply",
        json!({
            "account": fixture::ACCOUNT,
            "source": {"selector": support::read_fixture::BERICHT},
        }),
    )
    .await;
    assert_paths_are_draft_paths("draft.reply", &result, slice.root(), fixture::ACCOUNT);
    let created: DraftCreated =
        serde_json::from_value(result).expect("the result is a DraftCreated");
    assert!(is_minted_id(&created.id));
    let source = created
        .source
        .as_ref()
        .expect("a reply names the message it answers");
    assert_eq!(
        source.selector,
        format!(
            "mp://{}/inbox/{}",
            fixture::ACCOUNT,
            support::read_fixture::BERICHT
        ),
        "the canonical selector of the source, which is what `mp reply` prints first"
    );
    assert_eq!(source.id, "inbox/1", "and its <mailbox>/<uid> id");

    let document = fs::read_to_string(&created.path).expect("the reply file exists");
    assert!(
        document.contains("subject: \"Re: Bericht über Anträge\""),
        "the reply prefixes the subject once: {document}"
    );
    assert!(
        document.contains("to: \"ivana@example.com\""),
        "and answers the sender: {document}"
    );
    assert!(
        document.contains(&format!(
            "in_reply_to: \"<{}>\"",
            support::read_fixture::BERICHT
        )),
        "and records the message it answers, which is what the post-send hook reads: {document}"
    );
    assert!(
        !document.contains("petzold@example.com"),
        "a plain reply does not copy the Cc list: {document}"
    );

    // `--all` is the flag, and it is the only difference.
    let created: DraftCreated = call_typed(
        &mut conn,
        "draft.reply",
        json!({
            "account": fixture::ACCOUNT,
            "source": {"id": "inbox/1"},
            "all": true,
        }),
    )
    .await;
    let document = fs::read_to_string(&created.path).expect("the reply-all file exists");
    assert!(
        document.contains("petzold@example.com"),
        "a reply-all keeps the Cc list: {document}"
    );
}

/// A forward carries the original attachments, which is the property #0006
/// exists for and the one a GUI must not drop.
#[tokio::test]
async fn draft_forward_carries_the_attachments() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let created: DraftCreated = call_typed(
        &mut conn,
        "draft.forward",
        json!({
            "account": fixture::ACCOUNT,
            "source": {"selector": support::read_fixture::BERICHT},
        }),
    )
    .await;
    let document = fs::read_to_string(&created.path).expect("the forward file exists");
    assert!(
        document.contains("subject: \"Fwd: Bericht über Anträge\""),
        "{document}"
    );
    for attachment in ["agenda.txt", "notes.pdf"] {
        assert!(
            document.contains(attachment),
            "the forward references {attachment}: {document}"
        );
    }
    assert!(
        created.source.is_some(),
        "a forward names the message it forwards"
    );
}

/// What the two builders refuse: a message nothing resolves to, a source that
/// is addressed twice or not at all, and an ambiguous key with no `mailbox`.
#[tokio::test]
async fn draft_reply_refuses_a_source_it_cannot_resolve() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let error = call_err(
        &mut conn,
        "draft.reply",
        json!({
            "account": fixture::ACCOUNT,
            "source": {"selector": "nothing-here@example.com"},
        }),
    )
    .await;
    assert_eq!(
        error.code, INVALID_PARAMS,
        "a selector that resolves to no message is a parameter the daemon cannot honour: {error:?}"
    );

    for source in [
        json!({}),
        json!({"id": "inbox/1", "selector": "x@example.com"}),
    ] {
        let error = call_err(
            &mut conn,
            "draft.forward",
            json!({"account": fixture::ACCOUNT, "source": source}),
        )
        .await;
        assert_eq!(
            error.code, INVALID_PARAMS,
            "a message is addressed by id or by selector, never by both and never by neither"
        );
    }

    let error = call_err(
        &mut conn,
        "draft.reply",
        json!({
            "account": fixture::STORELESS_ACCOUNT,
            "source": {"id": "inbox/1"},
        }),
    )
    .await;
    assert_eq!(
        error.code,
        ErrorCode::AccountNotReady.code(),
        "an account with no store is not ready here as everywhere else: {error:?}"
    );
}

// ---------------------------------------------------------------------------
// 9. Paths, everywhere at once
// ---------------------------------------------------------------------------

/// One assertion over every method: nothing names the store, the blobs or the
/// runtime directory, and every path that is answered is absolute and under
/// the account's drafts directory.
#[tokio::test]
async fn every_path_a_draft_method_returns_is_a_draft_path() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let calls: Vec<(&str, Value)> = vec![
        (
            "draft.create",
            json!({"account": fixture::ACCOUNT, "name": "pfade"}),
        ),
        ("draft.list", json!({"account": fixture::ACCOUNT})),
        ("draft.validate", json!({"account": fixture::ACCOUNT})),
        (
            "draft.path",
            json!({"account": fixture::ACCOUNT, "id": fixture::VALID}),
        ),
        (
            "draft.preview",
            json!({"account": fixture::ACCOUNT, "id": fixture::VALID}),
        ),
        (
            "draft.approve",
            json!({"account": fixture::ACCOUNT, "id": fixture::VALID}),
        ),
        (
            "draft.demote",
            json!({"account": fixture::ACCOUNT, "id": fixture::VALID}),
        ),
        (
            "draft.reply",
            json!({"account": fixture::ACCOUNT, "source": {"id": "inbox/1"}}),
        ),
        (
            "draft.forward",
            json!({"account": fixture::ACCOUNT, "source": {"id": "inbox/1"}}),
        ),
    ];
    let mut seen: Vec<&str> = Vec::new();
    for (method, params) in calls {
        let result = call(&mut conn, method, params).await;
        assert_paths_are_draft_paths(method, &result, slice.root(), fixture::ACCOUNT);
        assert!(
            !path_fields(&result).is_empty() || matches!(method, "draft.validate"),
            "{method} answers at least one path, or is the one method that answers none"
        );
        seen.push(method);
    }
    assert_eq!(
        seen.len(),
        DRAFT_METHODS.len(),
        "every method of the family is covered here"
    );
}

// ---------------------------------------------------------------------------
// 10. Parity with the pre-daemon binary
// ---------------------------------------------------------------------------

/// `mp list`, filtered and unfiltered, on both accounts and on the one with no
/// store.
const LIST_PARITY: &[&[&str]] = &[
    &["list"],
    &["list", "--status", "draft"],
    &["list", "--status", "approved"],
    &["list", "--status", "sent"],
    &["-A", "beta", "list"],
    &["-A", "delta", "list"],
];

/// `mp validate`, the whole account and one draft of it, valid and not.
const VALIDATE_PARITY: &[&[&str]] = &[
    &["validate"],
    &["validate", fixture::VALID],
    &["validate", fixture::NO_SUBJECT],
    &["validate", fixture::UNKNOWN_ID],
    &["-A", "beta", "validate"],
];

/// `mp path`, every spelling and every refusal.
const PATH_PARITY: &[&[&str]] = &[
    &["path", fixture::VALID],
    &["path", fixture::APPROVED],
    &["path", fixture::UNKNOWN_ID],
    &["path", "mp://beta/drafts/e0000000000000b1"],
    &["path", "mp://zeta/drafts/a0000000000000d1"],
    &["path", "./drafts/angebot.md"],
];

/// The bare-selector dry run, which is a preview and never a path.
const PREVIEW_PARITY: &[&[&str]] = &[
    &[fixture::VALID],
    &[fixture::APPROVED],
    &[fixture::NO_SUBJECT],
    &[fixture::UNKNOWN_ID],
];

/// The state machine, including both no-ops and both refusals.
const MARK_PARITY: &[&[&str]] = &[
    &["mark-approved", fixture::VALID],
    &["mark-approved", fixture::APPROVED],
    &["mark-approved", fixture::SENT],
    &["mark-approved", fixture::UNKNOWN_ID],
    &["mark-draft", fixture::APPROVED],
    &["mark-draft", fixture::VALID],
    &["mark-draft", fixture::SENT],
    &["mark-approved", "./drafts/angebot.md"],
];

/// The three commands that mint an id, compared with the id masked.
const MINTING_PARITY: &[&[&str]] = &[
    &["new", "quartalsbericht"],
    &["reply", support::read_fixture::BERICHT],
    &["reply", "--all", support::read_fixture::BERICHT],
    &[
        "reply",
        "--mailbox",
        "inbox",
        support::read_fixture::KICKOFF,
    ],
    &["forward", support::read_fixture::BERICHT],
];

/// The refusals of the minting commands, which mint nothing and are therefore
/// compared literally.
const MINTING_REFUSAL_PARITY: &[&[&str]] = &[
    &["new", "angebot"],
    &["reply", "nothing-here@example.com"],
    &["forward", "nothing-here@example.com"],
    &["reply", "./inbox/message.md"],
    &["-A", "delta", "reply", support::read_fixture::BERICHT],
];

/// The gate of the whole slice: stdout, stderr and exit code, against the
/// binary the migration may not change the behaviour of.
///
/// One daemon for the whole table; the fixture is restored between the two
/// binaries of every case, so a writing command is compared over the same
/// directory the other one saw.
#[test]
fn every_draft_command_answers_byte_identically_to_the_pre_daemon_binary() {
    let slice = Slice::start();
    for args in LIST_PARITY
        .iter()
        .chain(VALIDATE_PARITY)
        .chain(PATH_PARITY)
        .chain(PREVIEW_PARITY)
        .chain(MARK_PARITY)
        .chain(MINTING_REFUSAL_PARITY)
    {
        slice.parity(args);
    }
}

/// The three minting commands, compared with the one thing the two binaries
/// cannot agree on masked out, and with the file each of them wrote asserted
/// to exist.
#[test]
fn the_minting_commands_agree_once_the_minted_id_is_masked() {
    let slice = Slice::start();
    for args in MINTING_PARITY {
        slice.for_each_binary_modulo_ids(args, |run| {
            assert!(
                run.out.status.success(),
                "{}: `mp {}` failed: {}",
                run.route,
                args.join(" "),
                run.stderr()
            );
            let created = run.only_created();
            assert!(
                created.is_file(),
                "{}: the created draft {} is on disk",
                run.route,
                created.display()
            );
            let printed = run.stdout();
            let id = printed
                .rsplit("/drafts/")
                .next()
                .map(|tail| tail.trim().to_string())
                .expect("a printed selector ends in the id");
            assert!(
                is_minted_id(&id),
                "{}: `mp {}` printed {id}, which is not a minted id",
                run.route,
                args.join(" ")
            );
            assert!(
                fs::read_to_string(created)
                    .expect("read the draft back")
                    .contains(&id),
                "{}: the printed id is the one in the file, quoted as `set_draft_id` writes it",
                run.route
            );
        });
    }
}

/// `mp edit` asks the daemon for the path and runs `$EDITOR` **in the client**,
/// which is what a stub editor recording its argument proves.
///
/// The recorded path is `mp path`'s output and it exists, on both routes: a
/// client that ran the editor on a path of its own invention, or a daemon that
/// ran an editor of its own, would fail here.
#[test]
fn mp_edit_hands_the_editor_the_path_mp_path_prints() {
    let slice = Slice::start();
    let expected = slice.agreed_stdout(&["path", fixture::VALID]);
    let expected = expected.trim().to_string();
    assert!(
        Path::new(&expected).is_file(),
        "`mp path` names a file that exists: {expected}"
    );

    let editor = stub_editor(slice.root(), 0);
    let log = slice.root().join("editor.log");

    for (route, mut command) in [
        ("routed", {
            let mut cmd = mp_command(slice.root());
            cmd.env(REQUIRE_ENV, "1");
            cmd
        }),
        ("pre-daemon", oracle_command(slice.root())),
    ] {
        let _ = fs::remove_file(&log);
        let out = command
            .env("EDITOR", &editor)
            .env("MP_EDITOR_LOG", &log)
            .args(["edit", fixture::VALID])
            .output()
            .expect("run mp edit");
        assert!(
            out.status.success(),
            "{route}: `mp edit` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let recorded = fs::read_to_string(&log)
            .unwrap_or_else(|e| panic!("{route}: the stub editor recorded nothing: {e}"));
        assert_eq!(
            recorded.trim(),
            expected,
            "{route}: the editor was handed the path `mp path` prints"
        );
        assert!(
            String::from_utf8_lossy(&out.stdout)
                .contains(&fixture::selector(fixture::ACCOUNT, fixture::VALID)),
            "{route}: `mp edit` prints the canonical selector when the editor exits cleanly"
        );
    }
}

/// An editor that fails is an error, and the same error on both routes: the
/// client owns the editor, so it owns the exit code too.
#[test]
fn mp_edit_reports_an_editor_that_failed_identically() {
    let slice = Slice::start();
    let editor = stub_editor(slice.root(), 3);
    let log = slice.root().join("failing-editor.log");

    let mut routed = mp_command(slice.root());
    routed.env(REQUIRE_ENV, "1");
    let routed = routed
        .env("EDITOR", &editor)
        .env("MP_EDITOR_LOG", &log)
        .args(["edit", fixture::VALID])
        .output()
        .expect("run mp edit");
    let direct = oracle_command(slice.root())
        .env("EDITOR", &editor)
        .env("MP_EDITOR_LOG", &log)
        .args(["edit", fixture::VALID])
        .output()
        .expect("run mp edit");
    assert!(!routed.status.success(), "the stub editor exits 3");
    assert_byte_identical(&routed, &direct);
}

/// A `sh` script that appends its argument to `$MP_EDITOR_LOG` and exits with
/// `code`.
fn stub_editor(root: &Path, code: i32) -> PathBuf {
    let path = root.join(format!("stub-editor-{code}.sh"));
    fs::write(
        &path,
        format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$MP_EDITOR_LOG\"\nexit {code}\n"),
    )
    .expect("write the stub editor");
    let mut perms = fs::metadata(&path).expect("stat the stub").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&path, perms).expect("make the stub executable");
    path
}

/// Two routed runs over an unchanged fixture are byte-identical, which is what
/// makes a diff against another build mean something.
///
/// Only the reading commands are here: a command that mints an id is not
/// deterministic by design, and its id is pinned by shape above.
#[test]
fn two_routed_runs_agree_with_each_other() {
    let slice = Slice::start();
    for args in [
        ["list"].as_slice(),
        ["validate", fixture::VALID].as_slice(),
        ["path", fixture::VALID].as_slice(),
        [fixture::VALID].as_slice(),
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

/// With no daemon and no way to start one, every command of the slice exits 4.
///
/// This is the routing proof that does not depend on the client noticing:
/// `MAILYPOPPINS_DAEMON_REQUIRE` is checked at the end of `main`, and a command
/// that returns early escapes it. A command with no in-process path left
/// cannot answer at all when nothing is listening, which is a fact about the
/// command rather than about a guard it might have skipped.
#[test]
fn no_draft_command_can_still_answer_without_a_daemon() {
    let root = SandboxRoot::fresh();
    fixture::seed(root.path());

    for args in [
        ["new", "irgendwas"].as_slice(),
        ["list"].as_slice(),
        ["validate"].as_slice(),
        ["mark-approved", fixture::VALID].as_slice(),
        ["mark-draft", fixture::APPROVED].as_slice(),
        ["path", fixture::VALID].as_slice(),
        ["edit", fixture::VALID].as_slice(),
        ["reply", support::read_fixture::BERICHT].as_slice(),
        ["forward", support::read_fixture::BERICHT].as_slice(),
        [fixture::VALID].as_slice(),
    ] {
        let out = mp_no_daemon(args, root.path());
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
fn the_draft_commands_are_answered_by_the_daemon() {
    let slice = Slice::start();
    let stash = Stash::take(slice.root());
    for args in [
        ["new", "geroutet"].as_slice(),
        ["list"].as_slice(),
        ["validate", fixture::VALID].as_slice(),
        ["mark-approved", fixture::VALID].as_slice(),
        ["mark-draft", fixture::VALID].as_slice(),
        ["path", fixture::VALID].as_slice(),
        ["reply", support::read_fixture::BERICHT].as_slice(),
        ["forward", support::read_fixture::BERICHT].as_slice(),
        [fixture::VALID].as_slice(),
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
        stash.restore();
    }
}

// ---------------------------------------------------------------------------
// 11. The legacy suites, twinned
// ---------------------------------------------------------------------------

/// `tests/cli_selector_contract.rs`, run once in process and once routed: the
/// path refusal, the round trip, the rename, the freshness rule and the two
/// account refusals.
#[test]
fn the_selector_contract_holds_on_both_routes() {
    let slice = Slice::start();

    // a_path_is_refused_where_a_selector_is_expected, for the commands this
    // slice owns.
    let path = slice
        .drafts_dir(fixture::ACCOUNT)
        .join("angebot.md")
        .display()
        .to_string();
    for command in [
        "mark-approved",
        "mark-draft",
        "validate",
        "path",
        "edit",
        "reply",
        "forward",
    ] {
        slice.for_each_binary(&[command, path.as_str()], |run| {
            assert!(
                !run.out.status.success(),
                "{}: `mp {command} <path>` must fail",
                run.route
            );
            assert!(
                run.stderr().contains("looks like a filesystem path"),
                "{}: `mp {command} <path>` must name the mistake: {}",
                run.route,
                run.stderr()
            );
            assert!(
                !run.stderr().contains("No such file or directory"),
                "{}: and must not surface a raw I/O error: {}",
                run.route,
                run.stderr()
            );
        });
    }

    // printed_selectors_round_trip_and_mp_path_lands_on_a_real_file: the
    // printed selector resolves, and `mp path` lands on the file that was
    // written. The id is minted, so the round trip is asserted rather than the
    // bytes.
    slice.for_each_binary_modulo_ids(&["new", "quartalsbericht"], |run| {
        let printed = run.stdout();
        let selector = printed
            .split_whitespace()
            .find(|word| word.starts_with("mp://"))
            .unwrap_or_else(|| panic!("{}: `mp new` prints one selector: {printed}", run.route))
            .to_string();
        let created = run.only_created().to_path_buf();

        for spelling in [
            selector.clone(),
            selector.rsplit('/').next().expect("an id").to_string(),
            format!("drafts/{}", selector.rsplit('/').next().expect("an id")),
        ] {
            let out = resolve_with(run, &["path", &spelling]);
            assert_eq!(
                out.trim(),
                created.display().to_string(),
                "{}: {spelling} resolves to the file `mp new` wrote",
                run.route
            );
        }
    });

    // renaming_a_draft_keeps_its_selector_working: identity is the `id:`
    // field, not the filename, which is what makes it safe for an agent to
    // reorganise the directory.
    slice.for_each_binary(&["path", fixture::VALID], |run| {
        let before = PathBuf::from(run.stdout().trim());
        let after = before.with_file_name("ganz-anders.md");
        fs::rename(&before, &after).expect("rename the draft");
        let out = resolve_with(run, &["path", fixture::VALID]);
        assert_eq!(
            out.trim(),
            after.display().to_string(),
            "{}: the selector follows the file",
            run.route
        );
        fs::rename(&after, &before).expect("rename it back");
    });

    // an_externally_written_draft_shows_up_within_one_second, the `mp list`
    // half: a fresh process refreshes the index at startup, so the draft an
    // agent wrote is listed by the next command.
    let external = slice.drafts_dir(fixture::ACCOUNT).join("von-aussen.md");
    fs::write(
        &external,
        fixture::document(
            "a00000000000beef",
            "robin@example.com",
            "Von aussen",
            "draft",
            "Body.\n",
        ),
    )
    .expect("write the agent's draft");
    slice.for_each_binary(&["list"], |run| {
        assert!(
            run.stdout().contains("Von aussen"),
            "{}: the draft another process wrote is listed: {}",
            run.route,
            run.stdout()
        );
    });
    fs::remove_file(&external).expect("remove the agent's draft");

    // an_unknown_key_names_the_namespace_it_searched.
    slice.for_each_binary(&["path", fixture::UNKNOWN_ID], |run| {
        assert!(
            !run.out.status.success(),
            "{}: an unknown id fails",
            run.route
        );
        assert!(
            run.stderr().contains("drafts"),
            "{}: and names the namespace it searched: {}",
            run.route,
            run.stderr()
        );
    });

    // a_selector_naming_an_unconfigured_account_fails_with_that_account_named.
    slice.for_each_binary(
        &[
            "path",
            &format!(
                "mp://{}/drafts/{}",
                fixture::UNKNOWN_ACCOUNT,
                fixture::VALID
            ),
        ],
        |run| {
            assert!(!run.out.status.success(), "{}", run.route);
            assert!(
                run.stderr().contains(fixture::UNKNOWN_ACCOUNT),
                "{}: the refusal names the account: {}",
                run.route,
                run.stderr()
            );
        },
    );

    // The cross-account rule the `mp delete` test rests on: a selector naming
    // the second account resolves in that account, without `-A`.
    slice.for_each_binary(
        &[
            "path",
            &fixture::selector(fixture::OTHER_ACCOUNT, fixture::BETA_DRAFT),
        ],
        |run| {
            assert!(
                run.stdout().contains("/accounts/beta/drafts/"),
                "{}: the account comes from the selector: {}",
                run.route,
                run.stdout()
            );
        },
    );
}

/// Run a follow-up command on the same route as `run`, so a two-step
/// assertion does not silently mix the two binaries.
fn resolve_with(run: &Run, args: &[&str]) -> String {
    let out = match run.route {
        "routed" => {
            let mut cmd = mp_command(run.root);
            cmd.env(REQUIRE_ENV, "1");
            cmd.args(args).output().expect("run mp")
        }
        _ => oracle_command(run.root)
            .args(args)
            .output()
            .expect("run mp"),
    };
    assert!(
        out.status.success(),
        "{}: `mp {}` failed: {}",
        run.route,
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `tests/draft_integration.rs`'s approve/demote half, through the commands:
/// the transitions, the two no-ops and the two refusals, with the file
/// asserted after each one.
#[test]
fn the_draft_lifecycle_contract_holds_on_both_routes() {
    let slice = Slice::start();
    let angebot = slice.drafts_dir(fixture::ACCOUNT).join("angebot.md");
    let verschickt = slice.drafts_dir(fixture::ACCOUNT).join("verschickt.md");

    // test_mark_as_approved.
    slice.for_each_binary(&["mark-approved", fixture::VALID], |run| {
        assert!(run.out.status.success(), "{}: {}", run.route, run.stderr());
        assert!(
            fs::read_to_string(&angebot)
                .expect("read it back")
                .contains("status: approved"),
            "{}: the status line is rewritten",
            run.route
        );
    });

    // test_mark_as_approved_already_approved: an information line, exit 0.
    slice.for_each_binary(&["mark-approved", fixture::APPROVED], |run| {
        assert!(run.out.status.success(), "{}: {}", run.route, run.stderr());
        assert!(
            run.stdout().contains("already approved"),
            "{}: {}",
            run.route,
            run.stdout()
        );
    });

    // test_mark_as_approved_sent_fails, and its demotion twin.
    for args in [
        ["mark-approved", fixture::SENT],
        ["mark-draft", fixture::SENT],
    ] {
        slice.for_each_binary(&args, |run| {
            assert!(
                !run.out.status.success(),
                "{}: a sent draft has left the pipeline",
                run.route
            );
            assert!(
                fs::read_to_string(&verschickt)
                    .expect("read it back")
                    .contains("status: sent"),
                "{}: and is not rewritten",
                run.route
            );
        });
    }

    // test_mark_as_draft_demotes_approved and test_mark_as_draft_already_draft.
    slice.for_each_binary(&["mark-draft", fixture::APPROVED], |run| {
        assert!(run.out.status.success(), "{}: {}", run.route, run.stderr());
        assert!(
            fs::read_to_string(slice.drafts_dir(fixture::ACCOUNT).join("freigabe.md"))
                .expect("read it back")
                .contains("status: draft"),
            "{}: the approved draft is demoted",
            run.route
        );
    });
    slice.for_each_binary(&["mark-draft", fixture::VALID], |run| {
        assert!(run.out.status.success(), "{}: {}", run.route, run.stderr());
        assert!(
            run.stdout().contains("already a draft"),
            "{}: {}",
            run.route,
            run.stdout()
        );
    });

    // test_parse_and_validate_draft, through the command that shows it: the
    // invalid draft is reported and the exit code says so.
    slice.for_each_binary(&["validate"], |run| {
        assert_eq!(
            run.out.status.code(),
            Some(1),
            "{}: one invalid draft makes the run fail",
            run.route
        );
        assert!(
            run.stdout().contains("Missing 'subject' field"),
            "{}: and names why: {}",
            run.route,
            run.stdout()
        );
    });
}

/// `tests/draft_integration.rs`'s reply and forward half, through the commands
/// that write those files.
#[test]
fn the_reply_and_forward_contract_holds_on_both_routes() {
    let slice = Slice::start();

    // test_create_reply_draft, a_reply_draft_records_the_message_it_answers.
    slice.for_each_binary_modulo_ids(&["reply", support::read_fixture::BERICHT], |run| {
        let document = fs::read_to_string(run.only_created()).expect("the reply file");
        assert!(
            document.contains("subject: \"Re: Bericht über Anträge\""),
            "{}: {document}",
            run.route
        );
        assert!(
            document.contains("to: \"ivana@example.com\""),
            "{}: {document}",
            run.route
        );
        assert!(
            document.contains(&format!(
                "in_reply_to: \"<{}>\"",
                support::read_fixture::BERICHT
            )),
            "{}: {document}",
            run.route
        );
        assert!(
            document.contains("status: draft"),
            "{}: a reply starts as a draft: {document}",
            run.route
        );
        assert!(
            run.stdout().contains("reply to"),
            "{}: the source is named first: {}",
            run.route,
            run.stdout()
        );
    });

    // test_create_reply_all_draft.
    slice.for_each_binary_modulo_ids(&["reply", "--all", support::read_fixture::BERICHT], |run| {
        let document = fs::read_to_string(run.only_created()).expect("the reply-all file");
        assert!(
            document.contains("petzold@example.com"),
            "{}: reply-all keeps the Cc list: {document}",
            run.route
        );
    });

    // test_create_forward_draft, test_forward_with_attachments.
    slice.for_each_binary_modulo_ids(&["forward", support::read_fixture::BERICHT], |run| {
        let document = fs::read_to_string(run.only_created()).expect("the forward file");
        assert!(
            document.contains("subject: \"Fwd: Bericht über Anträge\""),
            "{}: {document}",
            run.route
        );
        for attachment in ["agenda.txt", "notes.pdf"] {
            assert!(
                document.contains(attachment),
                "{}: the forward carries {attachment}: {document}",
                run.route
            );
        }
        assert!(
            run.stdout().contains("forward of"),
            "{}: {}",
            run.route,
            run.stdout()
        );
    });

    // A reply to a message the account does not hold fails the same way on
    // both routes, which the parity table also covers; here it is asserted as
    // a property rather than as bytes.
    slice.for_each_binary(&["reply", "nothing-here@example.com"], |run| {
        assert!(!run.out.status.success(), "{}", run.route);
        assert_eq!(
            run.created.len(),
            0,
            "{}: a refused reply writes no draft",
            run.route
        );
    });
}

/// The dry run is a preview and never a send: it prints the draft, says what
/// it would do, and leaves the file exactly as it was.
#[test]
fn the_bare_selector_dry_run_holds_on_both_routes() {
    let slice = Slice::start();
    let angebot = slice.drafts_dir(fixture::ACCOUNT).join("angebot.md");
    let before = fs::read_to_string(&angebot).expect("the fixture draft");

    slice.for_each_binary(&[fixture::VALID], |run| {
        assert!(run.out.status.success(), "{}: {}", run.route, run.stderr());
        let stdout = run.stdout();
        assert!(
            stdout.contains("Email Draft Preview"),
            "{}: {stdout}",
            run.route
        );
        assert!(
            stdout.contains("[DRY RUN]"),
            "{}: the dry run says it is one: {stdout}",
            run.route
        );
        assert!(
            stdout.contains("robin@example.com") && stdout.contains("Angebot"),
            "{}: it prints the draft it previewed: {stdout}",
            run.route
        );
        assert_eq!(
            fs::read_to_string(&angebot).expect("read it back"),
            before,
            "{}: a dry run writes nothing",
            run.route
        );
    });
}
