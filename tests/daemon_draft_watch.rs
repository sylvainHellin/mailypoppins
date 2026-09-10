//! Draft and signature watching (#0122, plan unit P3b-U9).
//!
//! This file is a **contract test**: it is written before
//! `src/daemon/watch.rs`, `src/daemon/methods/draft.rs` and the four
//! `mp-protocol` payload types exist, against the contract fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.5 (unit P3b-U9) and
//! the source plan's watching prose ("draft changes are watched, validated,
//! indexed, and broadcast by the daemon … a daemon watcher debounces create,
//! write, rename, and delete sequences, then reparses the final file state …
//! the drafts index is refreshed by a one-second fingerprint poll rather than a
//! `notify` watcher, a dependency `src/tui/mod.rs:35` defers deliberately …
//! keeping the fingerprint poll inside the daemon is an acceptable first
//! milestone"). It does not compile under `--features daemon` today, and that
//! failure *is* the proof the contract has no stub behind it. An implementer
//! (P3b-U10) does not edit this file; they make it pass.
//!
//! # The two layers, and why the split
//!
//! **(a) In-process, against [`DraftWatcher`].** Debounce is a statement about
//! time, and a test that proved it by sleeping would either be slow or be
//! flaky: one second of wall clock per poll, multiplied by every burst this
//! file exercises. The watcher therefore takes its clock as a parameter -
//! `poll_once(now)` - so the test drives time forward by hand and the
//! collapsing of a burst into one reparse is a deterministic assertion rather
//! than a race. Everything that is a property of the *algorithm* is here:
//! which files are watched, what a burst collapses to, what a rename produces,
//! what a deletion produces, and the fact that no path in the watcher ever
//! opens a draft for writing.
//!
//! **(b) Over the socket, against a spawned `mp daemon run`.** Whether the poll
//! actually runs inside the daemon, whether its events reach a subscribed
//! client, whether they reduce into the `state.bootstrap` snapshot, and whether
//! `draft.approve` refuses a draft the daemon could not parse are properties of
//! the daemon process and of nothing smaller. Each socket test spawns its own
//! daemon in its own tempdir sandbox, killed on drop, with every wait bounded.
//!
//! # Surface under test
//!
//! ```rust,ignore
//! // crates/mp-protocol/src/events.rs  ->  mp_protocol::events
//! pub const KIND_DRAFT_CHANGED: &str = "draft.changed";
//! pub const KIND_DRAFT_INVALID: &str = "draft.invalid";
//! pub const KIND_SIGNATURE_CHANGED: &str = "signature.changed";
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
//! pub struct Diagnostic { pub line: Option<u32>, pub message: String }
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
//! pub struct DraftChanged {
//!     pub account: String, pub id: String, pub path: String,
//!     pub to: Option<String>, pub subject: String, pub status: String,
//!     pub valid: bool, pub ready: bool,
//! }
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
//! pub struct DraftInvalid {
//!     pub account: String, pub id: String, pub path: String,
//!     pub diagnostics: Vec<Diagnostic>,
//! }
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
//! pub struct SignatureChanged { pub name: String, pub path: String }
//!
//! // crates/mp-protocol/src/error.rs  ->  mp_protocol::ErrorCode
//! ErrorCode::DraftInvalid           // -32010, wire name "draft_invalid"
//!
//! // src/daemon/watch.rs  ->  mailypoppins::daemon::watch
//! pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(1000);
//! pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(300);
//! pub const WATCH_POLL_ENV: &str = "MAILYPOPPINS_DAEMON_WATCH_POLL_MS";
//! pub const WATCH_DEBOUNCE_ENV: &str = "MAILYPOPPINS_DAEMON_WATCH_DEBOUNCE_MS";
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq)]
//! pub struct WatchConfig { pub poll_interval: Duration, pub debounce: Duration }
//! impl Default for WatchConfig;          // the two constants above
//! impl WatchConfig { pub fn from_env() -> WatchConfig; }
//!
//! #[derive(Clone, Debug, Default, PartialEq, Eq)]
//! pub struct WatchRoots {
//!     pub drafts: Vec<(String, PathBuf)>,   // (account, <account_dir>/drafts)
//!     pub signatures: Option<PathBuf>,      // <config_dir>/signatures
//! }
//! impl WatchRoots {
//!     pub fn new() -> WatchRoots;
//!     pub fn with_account(self, account: impl Into<String>, dir: impl Into<PathBuf>) -> WatchRoots;
//!     pub fn with_signatures(self, dir: impl Into<PathBuf>) -> WatchRoots;
//! }
//!
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! pub struct DraftSummary {
//!     pub account: String, pub id: String, pub path: PathBuf,
//!     pub to: Option<String>, pub subject: String, pub status: String,
//!     pub ready: bool,
//! }
//!
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! pub enum WatchEvent {
//!     DraftChanged(DraftSummary),
//!     DraftInvalid { account: String, id: String, path: PathBuf, diagnostics: Vec<Diagnostic> },
//!     DraftRemoved { account: String, id: String },
//!     SignatureChanged { name: String, path: PathBuf },
//! }
//! impl WatchEvent {
//!     pub fn as_change(&self) -> Option<Change>;       // None for a signature
//!     pub fn as_lifecycle(&self) -> Option<Event>;     // Some only for a signature
//! }
//!
//! pub struct DraftWatcher;
//! impl DraftWatcher {
//!     pub fn new(roots: WatchRoots, config: WatchConfig) -> DraftWatcher;
//!     pub fn config(&self) -> WatchConfig;
//!     pub fn poll_once(&mut self, now: Instant) -> Vec<WatchEvent>;
//!     pub fn resolve(&self, account: &str, id: &str) -> Option<PathBuf>;
//! }
//!
//! // src/daemon/state/snapshot.rs  ->  mailypoppins::daemon::state::Change
//! Change::DraftUpsert {
//!     account: String, id: String, path: String, to: Option<String>,
//!     subject: String, status: String, valid: bool, ready: bool,
//! }                                        // gains path, to and ready
//! Change::DraftInvalid {
//!     account: String, id: String, path: String, diagnostics: Vec<Diagnostic>,
//! }                                        // new variant
//!
//! // src/daemon/methods/draft.rs  ->  mailypoppins::daemon::methods::draft
//! pub const DRAFT_METHOD_SPECS: [MethodSpec; 1];   // draft.approve
//! ```
//!
//! # The watcher's semantics, pinned
//!
//! 1. **A root is a directory that need not exist.** A missing root is an empty
//!    root: no events, no error, and the directory is picked up on the poll
//!    after it appears. A daemon that refused to start because an account had
//!    never had a draft would be worse than useless.
//! 2. **A draft is a depth-1 `*.md` file**, which is exactly
//!    `store::drafts::is_draft_file`. A `.tmp`, a `.swp`, a `4913` and a
//!    subdirectory are not drafts, which is what makes an editor's save dance
//!    invisible rather than an event storm.
//! 3. **One observation is `(modified, len)`**, one `stat` per file per poll,
//!    reading no contents - the same cost argument
//!    `store::drafts::fingerprint` makes. It is deliberately *finer* than that
//!    function, which folds mtime to whole seconds: a watcher that reparsed on
//!    the second would miss the second save of a burst, and the debounce below
//!    exists precisely to make bursts one event rather than none.
//! 4. **A poll settles nothing before the debounce has elapsed.** A file whose
//!    observation differs from its settled one becomes *pending* and records
//!    `now`; a pending file whose observation moves again re-records `now`; a
//!    pending file whose observation has not moved and whose `now - dirty_since
//!    >= debounce` **settles** and yields exactly one event carrying the state
//!    the file is in at that moment. Emitting on the first sight of a change
//!    and suppressing the rest would be the other plausible reading of
//!    "debounce", and it is the wrong one here: it publishes the *first* state
//!    of a burst, and the plan asks for "a reparse of the final file state".
//! 5. **A settled present file yields `DraftChanged` when it parses and
//!    `DraftInvalid` when it does not.** Both carry the same id, so the two are
//!    one resource and a client that fixes a broken draft sees a replacement
//!    rather than a second row.
//! 6. **A settled absent file yields `DraftRemoved` only if it had settled as
//!    present before.** A file that appeared and vanished inside one window was
//!    never announced, so announcing its removal would tell a client to drop a
//!    row it never had.
//! 7. **A removal undone inside its window yields `DraftChanged` and no
//!    `DraftRemoved`.** This is `vim`'s `backupcopy=no` save: the original is
//!    renamed away and a new file is created at the same path. Debouncing the
//!    *absence* as well as the presence is what turns that sequence into one
//!    change instead of a remove followed by an add, and a client that had
//!    dropped the row in between would have flickered.
//! 8. **Events from one poll are ordered by root, then by file name**, so a
//!    poll's output is a value a test can compare rather than a set it has to
//!    sort first.
//! 9. **The watcher opens no draft for writing, ever.** Not to mint an `id:`,
//!    not to normalise, not to restore something it saw disappear. That is why
//!    the id below falls back to the file stem instead of calling
//!    `store::drafts`'s minting path: minting inside a watcher would write to a
//!    file an editor is holding open, which is the exact burst the debounce
//!    exists to survive. Id minting stays where it is, on the explicit index
//!    refresh.
//!
//! # The payload fields, pinned
//!
//! - **`id`** is the trimmed, non-empty `id:` frontmatter field, and the file
//!   stem when there is none or the file did not parse. It is a stable
//!   addressable name in both cases, which is what `draft:<account>/<id>` needs
//!   to be a resource.
//! - **`path`** is the absolute path of the file, as a string. A GUI opens a
//!   draft in the user's editor by path, and asking it to reconstruct one from
//!   an account name and an id would make it re-implement `config::drafts_dir`.
//! - **`to`**, **`subject`** and **`status`** are the frontmatter's, with
//!   `status` the word the file spells (`draft`, `approved`, `sent`). `to` is
//!   `null` for a draft with no recipient yet, which is a normal state for a
//!   draft the compose wizard has just created.
//! - **`valid`** is "the file parsed", so it is `true` on every `draft.changed`
//!   and `false` on every `draft.invalid`. It already exists in the snapshot's
//!   draft row and this file does not redefine it.
//! - **`ready`** is `draft::validate_draft(&draft).is_ok()`: recipients present,
//!   subject non-empty, addresses parseable, attachments on disk. It is the
//!   second axis and not the first: a draft with no subject parses perfectly
//!   and is not sendable, and a client that showed the two as one thing would
//!   have to call the subject-less draft broken.
//!
//! # Contract points this file pins beyond the plan text
//!
//! - **`draft.invalid` is an `Event::Replace` keyed `draft:<account>/<id>`, not
//!   a lifecycle event.** A broken draft is a fact about a resource that stays
//!   true until somebody fixes the file, so it belongs in the snapshot: #0080
//!   is the ticket where a draft that would not parse simply vanished from
//!   `mp list` and the TUI, and the snapshot's draft row already carries a
//!   `valid` flag for exactly this. A lifecycle event would be discarded by no
//!   overflow, but it would also be in no snapshot, so a client that connected
//!   after the breakage would never learn about it.
//! - **`draft.invalid`'s payload names its account**, which the plan's sketch
//!   (`{id, path, diagnostics}`) does not. Without it the event cannot key a
//!   resource, since two accounts may hold a draft with the same id, and every
//!   other draft payload names its account. Additive, and the only deviation
//!   from a payload the plan spells out.
//! - **`signature.changed` is an `Event::Lifecycle` and reduces into no
//!   snapshot.** Signatures are global rather than per-account, so no `Change`
//!   variant can answer `Change::account()` for one, and the snapshot has no
//!   signatures section to grow. The event is a "re-read the list" for a client
//!   that caches signature bodies, which is what a lifecycle event is for.
//!   A *deleted* signature publishes nothing: `signature.removed` is not a kind
//!   the plan names, and inventing one for a file nothing in this build reads
//!   at watch time would be a wire kind added on speculation. Recorded as a
//!   follow-up rather than pinned here.
//! - **Drafts stay in the `state.bootstrap` snapshot, and the row grows.**
//!   `docs/daemon-protocol.md` already documents a per-account `drafts` map and
//!   `src/daemon/state/snapshot.rs` already captures one; the source plan's
//!   "draft changes are watched, validated, indexed, and broadcast by the
//!   daemon" is not served by an event-only design, because a client that
//!   connects to a running daemon would show no drafts until somebody touched
//!   one. So the row becomes `{id, path, to, subject, status, valid, ready}`:
//!   the same fields the event carries, so the reducer a client writes is
//!   "replace the row with the payload" rather than a projection it has to keep
//!   in step. An invalid draft reduces to a row with `valid: false`,
//!   `status: "invalid"`, `to: null` and `subject: ""` - `"invalid"` is not an
//!   `EmailStatus`, and that is the point: nothing read a status out of a file
//!   that would not parse.
//! - **`Change::DraftUpsert` gains `path`, `to` and `ready`.** The variant is
//!   already the one a draft change travels as; three additive fields are a
//!   smaller change than a second variant carrying the same draft. Per
//!   `docs/lessons-learned.md` ("Adding a `Change` variant recompiles every
//!   earlier contract test that matched on it"), this unit budgets for the
//!   consequent one-line edits in `tests/daemon_bootstrap.rs` and
//!   `tests/daemon_events.rs`, and makes them itself: an implementer must still
//!   find `git diff --stat -- tests/` empty.
//! - **The refusal is a new domain code, `-32010` `draft_invalid`.** No
//!   existing code fits: `-32007` `config_invalid` is about `config.toml`, and
//!   `-32602` `invalid_params` would say the caller's parameters were wrong
//!   when they were right, and would make "you asked for a draft that does not
//!   exist" and "the draft you asked for will not parse" the same answer on the
//!   same connection - the collision `tests/daemon_operations.rs` refused to
//!   create in the other direction. The `data` is the `draft.invalid` payload,
//!   so a client renders the caller's refusal and the watcher's event with one
//!   piece of code. It widens the daemon's documented range to
//!   `-32010..=-32000`, which is a protocol-changelog entry for P3b-U10 and
//!   stays inside JSON-RPC's implementation-defined `-32099..=-32000`.
//! - **`draft.approve` is the minimal `draft.*` method this unit needs**, and
//!   the only one it pins: `{account, id}` in,
//!   `{account, id, status: "approved", path}` out, `Command`, `since` 1,
//!   `Durable`. It is `draft::mark_as_approved` behind the socket, so it
//!   rewrites the `status:` line and nothing else. An unknown account is
//!   `-32005` `{account}`; an unknown id is `-32602` with `{account, id}`; a
//!   draft that will not parse is `-32010`; a draft already `sent` is `-32602`
//!   with `{account, id, status}` (pinned here, exercised by no test in this
//!   file). The rest of the `draft.*` family arrives with the CLI cutover.
//! - **`draft.approve` resolves an id through the watcher's settled
//!   inventory**, not through the drafts index: the index lives in a store
//!   behind an engine lock, and the whole point of this unit is that watching
//!   drafts costs no lock. [`DraftWatcher::resolve`] is that lookup, and it is
//!   why a socket test waits for a draft's event before approving it.
//! - **The watcher runs whether or not `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES`
//!   is set.** It takes no engine lock, opens no store and starts no runtime;
//!   gating it behind the runtime hook would make Phase 3b's drafts
//!   untestable without also acquiring locks the tests do not want held.
//! - **Two env hooks, both millisecond integers**, mirroring
//!   `MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS`: absent, unparseable or zero
//!   means the default. The poll hook alone would not be enough, because a
//!   300 ms debounce with a 25 ms poll still costs 300 ms per assertion and
//!   this file makes a lot of them.

use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::UnixStream;

use mp_protocol::events::{
    Diagnostic, DraftChanged, DraftInvalid, SignatureChanged, KIND_DRAFT_CHANGED,
    KIND_DRAFT_INVALID, KIND_SIGNATURE_CHANGED,
};
use mp_protocol::{ErrorCode, EventEnvelope, RpcError, METHOD_STATE_EVENT};

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};

use mailypoppins::daemon::dispatch::{CancelScope, MethodKind};
use mailypoppins::daemon::methods::draft::DRAFT_METHOD_SPECS;
use mailypoppins::daemon::state::events::{Event, KIND_REMOVE};
use mailypoppins::daemon::state::Change;
use mailypoppins::daemon::watch::{
    DraftSummary, DraftWatcher, WatchConfig, WatchEvent, WatchRoots, DEFAULT_DEBOUNCE,
    DEFAULT_POLL_INTERVAL, WATCH_DEBOUNCE_ENV, WATCH_POLL_ENV,
};

/// The binary the socket layer spawns.
const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Every bounded wait in the socket layer.
const DEADLINE: Duration = Duration::from_secs(20);

/// How often a socket test re-checks something it is waiting for.
const TICK: Duration = Duration::from_millis(25);

/// The poll interval a sandboxed daemon runs at, in milliseconds.
const FAST_POLL_MS: u64 = 25;

/// The debounce a sandboxed daemon runs with, in milliseconds. Comfortably
/// above the poll so a burst really does span polls, and small enough that a
/// test that waits for one event waits for well under a second.
const FAST_DEBOUNCE_MS: u64 = 100;

/// A debounce wide enough that a handful of `fs::write` calls in a row cannot
/// straddle it, for the one socket test that counts events.
const WIDE_DEBOUNCE_MS: u64 = 600;

/// How long a socket test listens for an event that must not arrive. Several
/// poll-plus-debounce cycles, so "it did not happen yet" is not the reason.
const SILENCE: Duration = Duration::from_millis(900);

/// The two hooks, as the sandbox sets them.
const WATCH_POLL_MS_ENV: &str = "MAILYPOPPINS_DAEMON_WATCH_POLL_MS";
const WATCH_DEBOUNCE_MS_ENV: &str = "MAILYPOPPINS_DAEMON_WATCH_DEBOUNCE_MS";

/// The one method of the `draft.*` family this unit pins.
const DRAFT_METHODS: [&str; 1] = ["draft.approve"];

/// The keys a `draft.changed` payload carries, sorted.
const DRAFT_CHANGED_KEYS: [&str; 8] = [
    "account", "id", "path", "ready", "status", "subject", "to", "valid",
];

/// The keys a `draft.invalid` payload carries, sorted.
const DRAFT_INVALID_KEYS: [&str; 4] = ["account", "diagnostics", "id", "path"];

/// The keys a snapshot draft row carries, sorted.
const DRAFT_ROW_KEYS: [&str; 7] = ["id", "path", "ready", "status", "subject", "to", "valid"];

/// The keys a `signature.changed` payload carries, sorted.
const SIGNATURE_CHANGED_KEYS: [&str; 2] = ["name", "path"];

// ---------------------------------------------------------------------------
// Layer (a) - the vocabulary
// ---------------------------------------------------------------------------

#[test]
fn the_three_event_kinds_are_the_documented_strings() {
    assert_eq!(KIND_DRAFT_CHANGED, "draft.changed");
    assert_eq!(KIND_DRAFT_INVALID, "draft.invalid");
    assert_eq!(KIND_SIGNATURE_CHANGED, "signature.changed");
}

/// `draft.removed` is deliberately absent from that list: a removal travels as
/// the generic `state.remove` of `draft:<account>/<id>`, which
/// `docs/daemon-protocol.md` already says is how `Change::DraftRemoved`
/// reaches a client.
#[test]
fn a_removal_travels_as_the_generic_remove_kind() {
    let event = Event::from_change(&Change::DraftRemoved {
        account: "alpha".to_string(),
        id: "d-one".to_string(),
    });
    assert_eq!(event.kind(), KIND_REMOVE);
    assert_eq!(
        event.payload(),
        json!({"resource": "draft:alpha/d-one"}),
        "the resource a client drops is the one the two draft kinds replace"
    );
}

#[test]
fn the_refusal_code_is_the_new_domain_code() {
    assert_eq!(
        ErrorCode::DraftInvalid.code(),
        -32010,
        "the first free number below the daemon's documented range, which P3b-U10 widens to \
         -32010..=-32000 in docs/daemon-protocol.md and in the protocol changelog"
    );
    assert_eq!(ErrorCode::DraftInvalid.name(), "draft_invalid");
    assert_eq!(
        ErrorCode::from_code(-32010),
        Some(ErrorCode::DraftInvalid),
        "the code round-trips through the table like every other one"
    );
    for other in [
        ErrorCode::ConfigInvalid,
        ErrorCode::AccountUnknown,
        ErrorCode::ShuttingDown,
    ] {
        assert_ne!(
            other.code(),
            ErrorCode::DraftInvalid.code(),
            "{} may not share a number with draft_invalid",
            other.name()
        );
    }
}

#[test]
fn the_two_env_hooks_are_the_documented_names() {
    assert_eq!(WATCH_POLL_ENV, WATCH_POLL_MS_ENV);
    assert_eq!(WATCH_DEBOUNCE_ENV, WATCH_DEBOUNCE_MS_ENV);
}

/// The poll the plan asks for is the one the TUI already runs, moved: one
/// second. The debounce is this unit's number, and 300 ms is chosen to be
/// longer than an editor's save dance and shorter than a person notices.
#[test]
fn the_defaults_are_the_one_second_poll_and_a_three_hundred_millisecond_debounce() {
    assert_eq!(DEFAULT_POLL_INTERVAL, Duration::from_millis(1000));
    assert_eq!(DEFAULT_DEBOUNCE, Duration::from_millis(300));

    let default = WatchConfig::default();
    assert_eq!(default.poll_interval, DEFAULT_POLL_INTERVAL);
    assert_eq!(default.debounce, DEFAULT_DEBOUNCE);
    assert_eq!(
        WatchConfig::from_env(),
        default,
        "an environment with neither hook set is the default configuration; the socket layer \
         sets them per process and this test process sets neither"
    );
    assert!(
        default.debounce < default.poll_interval,
        "a debounce at or above the poll interval would delay every change by a whole extra poll"
    );
}

#[test]
fn a_watcher_reports_the_configuration_it_was_built_with() {
    let dir = TempDir::new().expect("tempdir");
    let config = WatchConfig {
        poll_interval: Duration::from_millis(10),
        debounce: Duration::from_millis(40),
    };
    let watcher = DraftWatcher::new(roots(&dir, "alpha"), config);
    assert_eq!(watcher.config(), config);
}

// ---------------------------------------------------------------------------
// Layer (a) - the bridge from a watch event to the daemon's vocabulary
// ---------------------------------------------------------------------------

#[test]
fn a_draft_change_commits_a_draft_upsert_keyed_by_account_and_id() {
    let summary = DraftSummary {
        account: "alpha".to_string(),
        id: "d-one".to_string(),
        path: PathBuf::from("/tmp/alpha/drafts/d-one.md"),
        to: Some("robin@example.com".to_string()),
        subject: "Angebot".to_string(),
        status: "draft".to_string(),
        ready: true,
    };
    let event = WatchEvent::DraftChanged(summary.clone());

    let change = event
        .as_change()
        .expect("a draft change reduces into the snapshot");
    assert_eq!(
        change,
        Change::DraftUpsert {
            account: "alpha".to_string(),
            id: "d-one".to_string(),
            path: "/tmp/alpha/drafts/d-one.md".to_string(),
            to: Some("robin@example.com".to_string()),
            subject: "Angebot".to_string(),
            status: "draft".to_string(),
            valid: true,
            ready: true,
        },
        "a parsed draft is valid by construction; `ready` is the second axis"
    );
    assert_eq!(change.kind(), KIND_DRAFT_CHANGED);
    assert_keys(&change.payload(), &DRAFT_CHANGED_KEYS, "draft.changed");

    let wire = Event::from_change(&change);
    assert_eq!(wire.kind(), KIND_DRAFT_CHANGED);
    assert_eq!(
        wire.resource().map(|r| r.as_str().to_string()),
        Some("draft:alpha/d-one".to_string()),
        "one draft is one resource, so a second change to it coalesces with the first"
    );
    assert!(
        event.as_lifecycle().is_none(),
        "a draft change is in the snapshot, so it is not a lifecycle event"
    );
    let _ = summary;
}

#[test]
fn an_invalid_draft_commits_a_change_on_the_same_resource() {
    let event = WatchEvent::DraftInvalid {
        account: "alpha".to_string(),
        id: "d-broken".to_string(),
        path: PathBuf::from("/tmp/alpha/drafts/d-broken.md"),
        diagnostics: vec![Diagnostic {
            line: Some(4),
            message: "unexpected end of stream while scanning a quoted scalar".to_string(),
        }],
    };

    let change = event
        .as_change()
        .expect("an invalid draft is a fact about a resource and reduces into the snapshot");
    assert_eq!(change.kind(), KIND_DRAFT_INVALID);
    assert_eq!(change.account(), "alpha");
    assert_keys(&change.payload(), &DRAFT_INVALID_KEYS, "draft.invalid");
    assert_eq!(change.payload()["diagnostics"][0]["line"], json!(4));

    let wire = Event::from_change(&change);
    assert_eq!(wire.kind(), KIND_DRAFT_INVALID);
    assert_eq!(
        wire.resource().map(|r| r.as_str().to_string()),
        Some("draft:alpha/d-broken".to_string()),
        "the same resource `draft.changed` uses, so fixing the file replaces the row rather \
         than adding a second one"
    );
    assert!(matches!(wire, Event::Replace { .. }));
    assert!(event.as_lifecycle().is_none());
}

#[test]
fn a_signature_change_is_a_lifecycle_event_and_reduces_into_nothing() {
    let event = WatchEvent::SignatureChanged {
        name: "work".to_string(),
        path: PathBuf::from("/tmp/config/signatures/work.md"),
    };
    assert!(
        event.as_change().is_none(),
        "signatures are global, so no `Change` can answer `Change::account()` for one"
    );

    let lifecycle = event
        .as_lifecycle()
        .expect("a signature change travels as a lifecycle event");
    assert_eq!(lifecycle.kind(), KIND_SIGNATURE_CHANGED);
    assert!(lifecycle.is_lifecycle());
    assert_eq!(
        lifecycle.payload(),
        json!({"name": "work", "path": "/tmp/config/signatures/work.md"}),
    );
    assert_keys(
        &lifecycle.payload(),
        &SIGNATURE_CHANGED_KEYS,
        "signature.changed",
    );
    assert!(
        lifecycle.resource().is_none(),
        "a lifecycle event addresses no resource, so it coalesces with nothing"
    );
}

#[test]
fn the_three_payload_types_round_trip_through_json() {
    let changed = DraftChanged {
        account: "alpha".to_string(),
        id: "d-one".to_string(),
        path: "/tmp/alpha/drafts/d-one.md".to_string(),
        to: None,
        subject: String::new(),
        status: "draft".to_string(),
        valid: true,
        ready: false,
    };
    let json = serde_json::to_value(&changed).expect("a draft.changed payload serialises");
    assert_keys(&json, &DRAFT_CHANGED_KEYS, "DraftChanged");
    assert_eq!(
        json["to"],
        Value::Null,
        "a draft with no recipient yet reports null rather than an empty string, because an \
         empty `to:` and an absent one are the same thing to the frontmatter"
    );
    assert_eq!(
        serde_json::from_value::<DraftChanged>(json).expect("and parses back"),
        changed
    );

    let invalid = DraftInvalid {
        account: "alpha".to_string(),
        id: "d-broken".to_string(),
        path: "/tmp/alpha/drafts/d-broken.md".to_string(),
        diagnostics: vec![
            Diagnostic {
                line: Some(4),
                message: "mapping values are not allowed in this context".to_string(),
            },
            Diagnostic {
                line: None,
                message: "frontmatter 'id:' is a number, not a string".to_string(),
            },
        ],
    };
    let json = serde_json::to_value(&invalid).expect("a draft.invalid payload serialises");
    assert_keys(&json, &DRAFT_INVALID_KEYS, "DraftInvalid");
    assert_eq!(
        json["diagnostics"][1]["line"],
        Value::Null,
        "a diagnostic with no position reports null rather than inventing a line"
    );
    assert_eq!(
        serde_json::from_value::<DraftInvalid>(json).expect("and parses back"),
        invalid
    );

    let signature = SignatureChanged {
        name: "work".to_string(),
        path: "/tmp/config/signatures/work.md".to_string(),
    };
    let json = serde_json::to_value(&signature).expect("a signature.changed payload serialises");
    assert_keys(&json, &SIGNATURE_CHANGED_KEYS, "SignatureChanged");
    assert_eq!(
        serde_json::from_value::<SignatureChanged>(json).expect("and parses back"),
        signature
    );
}

#[test]
fn the_one_draft_method_declares_its_name_kind_since_and_cancel_scope() {
    let names: Vec<&str> = DRAFT_METHOD_SPECS.iter().map(|spec| spec.name).collect();
    assert_eq!(names, DRAFT_METHODS.to_vec());

    let spec = DRAFT_METHOD_SPECS
        .iter()
        .find(|spec| spec.name == "draft.approve")
        .expect("draft.approve is declared");
    assert_eq!(
        spec.kind,
        MethodKind::Command,
        "approving rewrites the `status:` line and moves the state at once"
    );
    assert_eq!(spec.since, 1);
    assert_eq!(
        spec.cancel_scope,
        CancelScope::Durable,
        "an approval undone because the client that asked for it exited would silently drop a \
         send the user queued"
    );
}

// ---------------------------------------------------------------------------
// Layer (a) - what one poll sees
// ---------------------------------------------------------------------------

/// A watcher over `dir/accounts/<account>/drafts`, at the timings the
/// in-process layer drives by hand.
fn roots(root: &TempDir, account: &str) -> WatchRoots {
    WatchRoots::new().with_account(account, drafts_dir_of(root, account))
}

fn drafts_dir_of(root: &TempDir, account: &str) -> PathBuf {
    let dir = root.path().join("accounts").join(account).join("drafts");
    fs::create_dir_all(&dir).expect("drafts dir");
    dir
}

/// The two timings the in-process layer uses. The numbers are arbitrary: no
/// wall clock ever passes, because [`DraftWatcher::poll_once`] is handed the
/// time.
fn unit_config() -> WatchConfig {
    WatchConfig {
        poll_interval: Duration::from_millis(1000),
        debounce: Duration::from_millis(300),
    }
}

/// A draft as `mp new` writes one, with the fields this file asserts on.
fn draft_document(id: &str, to: &str, subject: &str, status: &str, body: &str) -> String {
    format!(
        "---\n\
         id: {id}\n\
         to: {to}\n\
         cc:\n\
         bcc:\n\
         subject: \"{subject}\"\n\
         status: {status}\n\
         from: alpha@example.com\n\
         date: 2024-05-01 09:00\n\
         ---\n\
         \n\
         {body}\n"
    )
}

/// A frontmatter block whose YAML does not scan: the quote on the `subject:`
/// line is never closed, so the parser fails at a position it can name.
fn unparseable_document(id: &str) -> String {
    format!(
        "---\n\
         id: {id}\n\
         to: robin@example.com\n\
         subject: \"Angebot ohne Ende\n\
         status: draft\n\
         ---\n\
         \n\
         Body.\n"
    )
}

/// A frontmatter block that scans and still refuses to load: `id:` is a YAML
/// number, which `draft::reject_non_string_id` refuses by value rather than by
/// position (#0083). The diagnostic therefore has no line.
fn unpositioned_document() -> String {
    "---\n\
     id: 8808e70039225152\n\
     to: robin@example.com\n\
     subject: \"Angebot\"\n\
     status: draft\n\
     ---\n\
     \n\
     Body.\n"
        .to_string()
}

/// Drive the watcher to the point where everything currently on disk has
/// settled, and hand back everything it emitted on the way.
///
/// Two polls one debounce apart: the first marks every changed file pending,
/// the second settles it. The instant returned is the one the second poll ran
/// at, so a caller can keep driving from there.
fn settle(
    watcher: &mut DraftWatcher,
    at: Instant,
    debounce: Duration,
) -> (Vec<WatchEvent>, Instant) {
    let mut events = watcher.poll_once(at);
    let then = at + debounce;
    events.extend(watcher.poll_once(then));
    (events, then)
}

#[test]
fn a_missing_root_is_an_empty_root_and_the_directory_is_picked_up_when_it_appears() {
    let root = TempDir::new().expect("tempdir");
    let dir = root.path().join("accounts").join("alpha").join("drafts");
    let mut watcher =
        DraftWatcher::new(WatchRoots::new().with_account("alpha", &dir), unit_config());

    let t0 = Instant::now();
    let (events, t1) = settle(&mut watcher, t0, unit_config().debounce);
    assert!(
        events.is_empty(),
        "an account that has never had a draft is not an error: {events:?}"
    );

    fs::create_dir_all(&dir).expect("drafts dir");
    fs::write(
        dir.join("one.md"),
        draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    )
    .expect("write draft");

    let (events, _) = settle(
        &mut watcher,
        t1 + unit_config().debounce,
        unit_config().debounce,
    );
    assert_eq!(
        events.len(),
        1,
        "the directory appearing is one change, not a storm: {events:?}"
    );
    assert_eq!(changed(&events[0]).id, "d-one");
}

#[test]
fn a_direct_write_settles_into_one_draft_changed_carrying_the_frontmatter() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    let path = dir.join("angebot.md");
    fs::write(
        &path,
        draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    )
    .expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let t0 = Instant::now();

    assert!(
        watcher.poll_once(t0).is_empty(),
        "nothing settles inside the debounce window, not even the first inventory"
    );
    let events = watcher.poll_once(t0 + unit_config().debounce);
    assert_eq!(events.len(), 1, "{events:?}");

    let summary = changed(&events[0]);
    assert_eq!(summary.account, "alpha");
    assert_eq!(summary.id, "d-one");
    assert_eq!(summary.path, path);
    assert_eq!(summary.to.as_deref(), Some("robin@example.com"));
    assert_eq!(summary.subject, "Angebot");
    assert_eq!(summary.status, "draft");
    assert!(
        summary.ready,
        "a draft with a recipient, a subject and a body passes `validate_draft`"
    );

    assert!(
        watcher
            .poll_once(t0 + unit_config().debounce * 4)
            .is_empty(),
        "a file nobody touched settles once and then says nothing"
    );

    let after = fs::read(&path).expect("read the draft back");
    assert_eq!(
        after,
        draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body.").into_bytes(),
        "the watcher read the file and wrote nothing back"
    );
}

#[test]
fn a_second_write_settles_again_and_carries_the_new_state() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    let path = dir.join("angebot.md");
    fs::write(
        &path,
        draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    )
    .expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let debounce = unit_config().debounce;
    let (first, t1) = settle(&mut watcher, Instant::now(), debounce);
    assert_eq!(first.len(), 1);

    fs::write(
        &path,
        draft_document(
            "d-one",
            "robin@example.com, sam@example.com",
            "Angebot v2",
            "approved",
            "A longer body, so the size moves as well as the mtime.",
        ),
    )
    .expect("rewrite draft");

    let (second, _) = settle(&mut watcher, t1 + debounce, debounce);
    assert_eq!(second.len(), 1, "{second:?}");
    let summary = changed(&second[0]);
    assert_eq!(summary.id, "d-one", "the id survives an edit");
    assert_eq!(summary.subject, "Angebot v2");
    assert_eq!(summary.status, "approved");
    assert_eq!(
        summary.to.as_deref(),
        Some("robin@example.com, sam@example.com")
    );
}

#[test]
fn an_atomic_save_by_rename_over_the_target_settles_into_one_draft_changed() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    let path = dir.join("angebot.md");
    fs::write(
        &path,
        draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    )
    .expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let debounce = unit_config().debounce;
    let (first, t1) = settle(&mut watcher, Instant::now(), debounce);
    assert_eq!(first.len(), 1);

    // `:w` with the default `backupcopy=auto`: the new contents go to a
    // temporary file beside the target, which is renamed over it. The
    // temporary is not a `.md` file, so it is invisible to the watcher even if
    // a poll lands while it exists.
    let tmp = dir.join("angebot.md.tmp");
    fs::write(
        &tmp,
        draft_document(
            "d-one",
            "robin@example.com",
            "Angebot nach dem Rename",
            "draft",
            "Rewritten wholesale by the editor.",
        ),
    )
    .expect("write the temporary");
    let mid = t1 + debounce;
    assert!(
        watcher.poll_once(mid).is_empty(),
        "a temporary file beside the target is not a draft"
    );
    fs::rename(&tmp, &path).expect("rename over the target");

    let (events, _) = settle(&mut watcher, mid + Duration::from_millis(1), debounce);
    assert_eq!(events.len(), 1, "{events:?}");
    let summary = changed(&events[0]);
    assert_eq!(summary.id, "d-one");
    assert_eq!(summary.path, path, "the id and the path are the target's");
    assert_eq!(summary.subject, "Angebot nach dem Rename");
}

/// `:w` with `backupcopy=no`: the original is renamed away and a *new* file is
/// created at the same path. Between the two operations the draft does not
/// exist, and a watcher without a debounce on absence would publish a removal
/// followed by an addition, flickering the row out of every client's list.
#[test]
fn an_atomic_save_that_renames_the_original_away_settles_into_one_change_and_no_removal() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    let path = dir.join("angebot.md");
    fs::write(
        &path,
        draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    )
    .expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let debounce = unit_config().debounce;
    let (first, t1) = settle(&mut watcher, Instant::now(), debounce);
    assert_eq!(first.len(), 1);

    fs::rename(&path, dir.join("angebot.md~")).expect("rename the original away");
    let mid = t1 + debounce;
    assert!(
        watcher.poll_once(mid).is_empty(),
        "the gap between the rename and the new file settles nothing"
    );
    fs::write(
        &path,
        draft_document(
            "d-one",
            "robin@example.com",
            "Angebot ohne Backupcopy",
            "draft",
            "A new inode at the same path.",
        ),
    )
    .expect("create the replacement");

    let (events, _) = settle(&mut watcher, mid + Duration::from_millis(1), debounce);
    assert_eq!(
        events.len(),
        1,
        "one change, not a removal followed by an addition: {events:?}"
    );
    let summary = changed(&events[0]);
    assert_eq!(summary.id, "d-one");
    assert_eq!(summary.subject, "Angebot ohne Backupcopy");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, WatchEvent::DraftRemoved { .. })),
        "the row never left the client's list: {events:?}"
    );
    assert!(
        dir.join("angebot.md~").exists(),
        "the editor's backup file is left alone; it is not a `.md` file and not a draft"
    );
}

/// The plan's "rapid saves within one debounce window collapsing to one reparse
/// of the final state", made deterministic by driving the clock.
#[test]
fn a_burst_of_saves_inside_one_window_collapses_to_one_reparse_of_the_last_write() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    let path = dir.join("angebot.md");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let debounce = unit_config().debounce;
    // A poll every 50 ms: faster than the daemon really polls, and the point is
    // that four saves and their polls all fit inside one 300 ms window.
    let step = Duration::from_millis(50);
    let t0 = Instant::now();

    let mut emitted: Vec<WatchEvent> = Vec::new();
    // Four saves, each seen by its own poll, all inside one debounce window
    // because every poll finds the file moving again.
    for (index, subject) in ["one", "two", "three", "four"].iter().enumerate() {
        fs::write(
            &path,
            draft_document(
                "d-one",
                "robin@example.com",
                subject,
                "draft",
                &"body ".repeat(index + 1),
            ),
        )
        .expect("write draft");
        emitted.extend(watcher.poll_once(t0 + step * index as u32));
    }
    assert!(
        emitted.is_empty(),
        "a file that keeps moving never settles, so the burst has published nothing yet: \
         {emitted:?}"
    );

    // The file stops moving. The window opened at the last poll that saw it
    // move, `t0 + step * 3`, so this one is still inside it.
    let quiet = t0 + step * 4;
    assert!(
        watcher.poll_once(quiet).is_empty(),
        "the window runs from the last observed movement, and 200 ms of it have passed"
    );
    let events = watcher.poll_once(quiet + debounce);
    assert_eq!(
        events.len(),
        1,
        "four saves are one reparse, not four: {events:?}"
    );
    assert_eq!(
        changed(&events[0]).subject,
        "four",
        "and the reparse carries the final state, not the first"
    );

    assert!(
        watcher.poll_once(quiet + debounce * 4).is_empty(),
        "nothing is queued behind it"
    );
}

#[test]
fn a_deletion_settles_into_a_removal_under_the_id_the_file_was_announced_with() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    let path = dir.join("angebot.md");
    fs::write(
        &path,
        draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    )
    .expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let debounce = unit_config().debounce;
    let (first, t1) = settle(&mut watcher, Instant::now(), debounce);
    assert_eq!(first.len(), 1);

    fs::remove_file(&path).expect("delete the draft");
    let (events, t2) = settle(&mut watcher, t1 + debounce, debounce);
    assert_eq!(
        events,
        vec![WatchEvent::DraftRemoved {
            account: "alpha".to_string(),
            id: "d-one".to_string(),
        }],
        "the removal names the id the draft was announced under, because that is the resource \
         the client holds"
    );

    assert!(
        watcher.poll_once(t2 + debounce * 4).is_empty(),
        "a draft is removed once"
    );
    assert!(
        !path.exists(),
        "and the watcher does not put a deleted draft back"
    );
}

#[test]
fn a_file_that_appears_and_vanishes_inside_one_window_is_never_announced() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    let path = dir.join("ephemeral.md");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let debounce = unit_config().debounce;
    let t0 = Instant::now();
    assert!(watcher.poll_once(t0).is_empty());

    fs::write(
        &path,
        draft_document("d-gone", "robin@example.com", "Angebot", "draft", "Body."),
    )
    .expect("write draft");
    assert!(watcher.poll_once(t0 + Duration::from_millis(1)).is_empty());
    fs::remove_file(&path).expect("delete it again");

    let (events, _) = settle(&mut watcher, t0 + Duration::from_millis(2), debounce);
    assert!(
        events.is_empty(),
        "a client that was never told about the row must not be told to drop it: {events:?}"
    );
}

#[test]
fn only_depth_one_markdown_files_are_drafts() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    fs::write(dir.join("angebot.md.tmp"), "not a draft").expect("write tmp");
    fs::write(dir.join(".angebot.md.swp"), "not a draft").expect("write swap");
    fs::write(dir.join("4913"), "vim's writability probe").expect("write probe");
    fs::write(dir.join("notes.txt"), "not a draft").expect("write txt");
    fs::create_dir_all(dir.join("attachments")).expect("subdir");
    fs::write(
        dir.join("attachments").join("nested.md"),
        draft_document("d-nested", "robin@example.com", "Nested", "draft", "Body."),
    )
    .expect("write nested");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let (events, _) = settle(&mut watcher, Instant::now(), unit_config().debounce);
    assert!(
        events.is_empty(),
        "the scan is `max_depth(1)` over `*.md`, exactly as `store::drafts` scans: {events:?}"
    );
}

#[test]
fn a_draft_with_no_id_field_is_announced_under_its_file_stem_and_is_not_rewritten() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    let path = dir.join("agent-written.md");
    let document = "---\n\
                    to: robin@example.com\n\
                    subject: \"Von einem Agenten\"\n\
                    status: draft\n\
                    ---\n\
                    \n\
                    Body.\n";
    fs::write(&path, document).expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let (events, t1) = settle(&mut watcher, Instant::now(), unit_config().debounce);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        changed(&events[0]).id,
        "agent-written",
        "the stem is a stable name the file already has; minting one would mean writing to a \
         file an editor may be holding open, which `store::drafts::refresh` does on an explicit \
         refresh and a watcher must not"
    );

    for step in 1..=4 {
        watcher.poll_once(t1 + unit_config().debounce * step);
    }
    assert_eq!(
        fs::read_to_string(&path).expect("read back"),
        document,
        "no `id:` line was written back"
    );
}

#[test]
fn an_id_field_wins_over_the_file_stem() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    fs::write(
        dir.join("renamed-by-the-user.md"),
        draft_document("d-stable", "robin@example.com", "Angebot", "draft", "Body."),
    )
    .expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let (events, _) = settle(&mut watcher, Instant::now(), unit_config().debounce);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        changed(&events[0]).id,
        "d-stable",
        "identity is the `id:` field and survives a rename, which the filename cannot"
    );
}

#[test]
fn a_parseable_draft_that_is_not_sendable_is_valid_and_not_ready() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    fs::write(
        dir.join("halb-fertig.md"),
        draft_document("d-half", "", "", "draft", "Body without a subject."),
    )
    .expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let (events, _) = settle(&mut watcher, Instant::now(), unit_config().debounce);
    assert_eq!(events.len(), 1, "{events:?}");
    let summary = changed(&events[0]);
    assert_eq!(summary.subject, "");
    assert!(
        !summary.ready,
        "`validate_draft` refuses a draft with no recipient and no subject"
    );
    assert_eq!(
        summary.to, None,
        "an empty `to:` is no recipient, not an empty recipient"
    );
    assert!(
        matches!(&events[0], WatchEvent::DraftChanged(_)),
        "and it is still a perfectly valid file: an unfinished draft is not a broken one"
    );
}

#[test]
fn an_unparseable_draft_settles_into_draft_invalid_with_a_positioned_diagnostic() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    let path = dir.join("kaputt.md");
    let document = unparseable_document("d-broken");
    fs::write(&path, &document).expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let (events, t1) = settle(&mut watcher, Instant::now(), unit_config().debounce);
    assert_eq!(events.len(), 1, "{events:?}");

    let WatchEvent::DraftInvalid {
        account,
        id,
        path: reported,
        diagnostics,
    } = &events[0]
    else {
        panic!("a file that will not parse is a `DraftInvalid`, got {events:?}");
    };
    assert_eq!(account, "alpha");
    assert_eq!(
        id, "kaputt",
        "the stem, because the `id:` field is inside the block that would not parse"
    );
    assert_eq!(reported, &path);
    assert!(
        !diagnostics.is_empty(),
        "a refusal with no diagnostic tells the user nothing"
    );
    assert!(
        !diagnostics[0].message.trim().is_empty(),
        "and its message is a line a status bar can show"
    );

    let lines = document.lines().count() as u32;
    let line = diagnostics[0].line.unwrap_or_else(|| {
        panic!(
            "a YAML scan error carries a position, so the diagnostic must too: parse the \
             frontmatter block with `serde_yaml` when `draft::parse_email_draft` fails and take \
             `serde_yaml::Error::location()`, offset by the lines before the block. Got \
             {diagnostics:?}"
        )
    });
    assert!(
        (1..=lines).contains(&line),
        "the line is 1-based and relative to the file, not to the frontmatter block: got {line} \
         for a {lines}-line file"
    );

    for step in 1..=6 {
        watcher.poll_once(t1 + unit_config().debounce * step);
    }
    assert_eq!(
        fs::read_to_string(&path).expect("read back"),
        document,
        "the daemon never rewrites or restores a draft it could not parse"
    );
}

#[test]
fn a_refusal_with_no_position_reports_a_null_line() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    fs::write(dir.join("zahlen-id.md"), unpositioned_document()).expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let (events, _) = settle(&mut watcher, Instant::now(), unit_config().debounce);
    assert_eq!(events.len(), 1, "{events:?}");
    let WatchEvent::DraftInvalid { diagnostics, .. } = &events[0] else {
        panic!("a numeric `id:` is refused by `reject_non_string_id` (#0083), got {events:?}");
    };
    assert_eq!(
        diagnostics[0].line, None,
        "a value the frontmatter refuses by kind rather than by syntax has no position, and \
         inventing one would point the user at an innocent line"
    );
}

#[test]
fn fixing_a_broken_draft_settles_into_a_change_on_the_same_resource() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    let path = dir.join("kaputt.md");
    fs::write(&path, unparseable_document("d-broken")).expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let debounce = unit_config().debounce;
    let (broken, t1) = settle(&mut watcher, Instant::now(), debounce);
    assert!(matches!(&broken[0], WatchEvent::DraftInvalid { .. }));

    fs::write(
        &path,
        draft_document(
            "d-broken",
            "robin@example.com",
            "Repariert",
            "draft",
            "Body.",
        ),
    )
    .expect("fix the draft");
    let (fixed, _) = settle(&mut watcher, t1 + debounce, debounce);
    assert_eq!(fixed.len(), 1, "{fixed:?}");
    let summary = changed(&fixed[0]);
    assert_eq!(summary.subject, "Repariert");
    assert_eq!(
        summary.id, "d-broken",
        "the `id:` field is readable again and it is the same draft; the stem-derived id the \
         broken file was announced under is `kaputt`, so a client keyed on the file's own name \
         would now hold two rows"
    );
}

#[test]
fn a_watcher_resolves_a_settled_draft_id_to_its_path() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    let path = dir.join("angebot.md");
    fs::write(
        &path,
        draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    )
    .expect("write draft");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    assert_eq!(
        watcher.resolve("alpha", "d-one"),
        None,
        "nothing resolves before the file has settled: the inventory is what the daemon has \
         announced, not what a poll happened to glimpse"
    );

    let debounce = unit_config().debounce;
    let (events, t1) = settle(&mut watcher, Instant::now(), debounce);
    assert_eq!(events.len(), 1);
    assert_eq!(
        watcher.resolve("alpha", "d-one"),
        Some(path.clone()),
        "this is the lookup `draft.approve` uses, so approving costs no engine lock"
    );
    assert_eq!(watcher.resolve("beta", "d-one"), None, "per account");
    assert_eq!(watcher.resolve("alpha", "d-two"), None);

    fs::remove_file(&path).expect("delete the draft");
    let (_, _) = settle(&mut watcher, t1 + debounce, debounce);
    assert_eq!(
        watcher.resolve("alpha", "d-one"),
        None,
        "a removed draft leaves the inventory with its event"
    );
}

#[test]
fn two_accounts_are_two_inventories() {
    let root = TempDir::new().expect("tempdir");
    let alpha = drafts_dir_of(&root, "alpha");
    let beta = drafts_dir_of(&root, "beta");
    fs::write(
        alpha.join("a.md"),
        draft_document(
            "d-shared",
            "robin@example.com",
            "Von Alpha",
            "draft",
            "Body.",
        ),
    )
    .expect("write alpha draft");
    fs::write(
        beta.join("b.md"),
        draft_document("d-shared", "sam@example.com", "Von Beta", "draft", "Body."),
    )
    .expect("write beta draft");

    let mut watcher = DraftWatcher::new(
        WatchRoots::new()
            .with_account("alpha", &alpha)
            .with_account("beta", &beta),
        unit_config(),
    );
    let (events, _) = settle(&mut watcher, Instant::now(), unit_config().debounce);
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        events
            .iter()
            .map(|event| changed(event).account.clone())
            .collect::<Vec<_>>(),
        vec!["alpha".to_string(), "beta".to_string()],
        "roots are visited in the order they were declared, which is `config.toml`'s"
    );
    assert_eq!(changed(&events[0]).subject, "Von Alpha");
    assert_eq!(changed(&events[1]).subject, "Von Beta");
    assert_eq!(
        Event::from_change(&events[0].as_change().expect("a change")).resource(),
        Event::from_change(&Change::DraftUpsert {
            account: "alpha".to_string(),
            id: "d-shared".to_string(),
            path: alpha.join("a.md").display().to_string(),
            to: Some("robin@example.com".to_string()),
            subject: "Von Alpha".to_string(),
            status: "draft".to_string(),
            valid: true,
            ready: true,
        })
        .resource(),
        "the same id under two accounts is two resources"
    );
}

#[test]
fn one_polls_events_are_ordered_by_root_then_by_file_name() {
    let root = TempDir::new().expect("tempdir");
    let dir = drafts_dir_of(&root, "alpha");
    for name in ["gamma", "alpha", "beta"] {
        fs::write(
            dir.join(format!("{name}.md")),
            draft_document(
                &format!("d-{name}"),
                "robin@example.com",
                name,
                "draft",
                "Body.",
            ),
        )
        .expect("write draft");
    }

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let (events, _) = settle(&mut watcher, Instant::now(), unit_config().debounce);
    assert_eq!(
        events
            .iter()
            .map(|event| changed(event).id.clone())
            .collect::<Vec<_>>(),
        vec![
            "d-alpha".to_string(),
            "d-beta".to_string(),
            "d-gamma".to_string()
        ],
        "a poll's output is a value a test can compare, not a set it has to sort"
    );
}

#[test]
fn a_signature_write_settles_into_signature_changed_named_by_its_stem() {
    let root = TempDir::new().expect("tempdir");
    let signatures = root.path().join("config").join("signatures");
    fs::create_dir_all(&signatures).expect("signatures dir");
    let path = signatures.join("work.md");
    fs::write(&path, "Mit freundlichen Gruessen\nSylvain\n").expect("write signature");

    let mut watcher = DraftWatcher::new(
        WatchRoots::new()
            .with_account("alpha", drafts_dir_of(&root, "alpha"))
            .with_signatures(&signatures),
        unit_config(),
    );
    let debounce = unit_config().debounce;
    let (events, t1) = settle(&mut watcher, Instant::now(), debounce);
    assert_eq!(
        events,
        vec![WatchEvent::SignatureChanged {
            name: "work".to_string(),
            path: path.clone(),
        }],
        "the name is the stem, which is the key `signatures::read` already takes"
    );

    fs::write(&path, "Viele Gruesse\nSylvain\n").expect("rewrite signature");
    let (again, t2) = settle(&mut watcher, t1 + debounce, debounce);
    assert_eq!(again.len(), 1, "an edit is one more event: {again:?}");

    assert!(
        watcher.poll_once(t2 + debounce * 4).is_empty(),
        "a signature nobody touched says nothing"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read back"),
        "Viele Gruesse\nSylvain\n",
        "and the watcher writes nothing back here either"
    );
}

#[test]
fn a_watcher_with_no_signature_root_watches_no_signatures() {
    let root = TempDir::new().expect("tempdir");
    let signatures = root.path().join("config").join("signatures");
    fs::create_dir_all(&signatures).expect("signatures dir");
    fs::write(signatures.join("work.md"), "Gruesse\n").expect("write signature");

    let mut watcher = DraftWatcher::new(roots(&root, "alpha"), unit_config());
    let (events, _) = settle(&mut watcher, Instant::now(), unit_config().debounce);
    assert!(
        events.is_empty(),
        "the signature root is an `Option`, and `None` is a watcher that never looks: {events:?}"
    );
}

// ---------------------------------------------------------------------------
// Layer (a) - helpers
// ---------------------------------------------------------------------------

/// The summary inside a [`WatchEvent::DraftChanged`], or a failure naming what
/// arrived instead.
fn changed(event: &WatchEvent) -> &DraftSummary {
    match event {
        WatchEvent::DraftChanged(summary) => summary,
        other => panic!("expected a draft change, got {other:?}"),
    }
}

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("expected a JSON object, got {value}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn assert_keys(value: &Value, expected: &[&str], what: &str) {
    assert_eq!(
        sorted_keys(value),
        expected.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
        "{what} has exactly the documented keys, got {value}"
    );
}

// ---------------------------------------------------------------------------
// Layer (b) - the sandbox
// ---------------------------------------------------------------------------

/// A private `HOME`, config directory and data directory, with a daemon that
/// polls fast enough for a test to wait on it.
///
/// Dropping it kills whatever daemon `daemon.pid` names, so nothing outlives
/// the test that started it, even when the test panics half way through.
struct Sandbox {
    root: TempDir,
    debounce_ms: u64,
}

impl Sandbox {
    /// A sandbox whose `config.toml` declares `accounts`, each with its drafts
    /// directory already present, at the fast timings.
    fn with_accounts(accounts: &[&str]) -> Self {
        Sandbox::with_accounts_at(accounts, FAST_DEBOUNCE_MS)
    }

    fn with_accounts_at(accounts: &[&str], debounce_ms: u64) -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        let sandbox = Sandbox { root, debounce_ms };
        let document: String = accounts.iter().map(|name| account_toml(name)).collect();
        fs::write(sandbox.config_path(), document).expect("write config.toml");
        for name in accounts {
            fs::create_dir_all(sandbox.drafts_dir(name)).expect("drafts dir");
        }
        fs::create_dir_all(sandbox.signatures_dir()).expect("signatures dir");
        sandbox
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn config_dir(&self) -> PathBuf {
        self.root.path().join("config")
    }

    fn data_dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn config_path(&self) -> PathBuf {
        self.config_dir().join("config.toml")
    }

    fn signatures_dir(&self) -> PathBuf {
        self.config_dir().join("signatures")
    }

    fn drafts_dir(&self, account: &str) -> PathBuf {
        self.data_dir()
            .join("accounts")
            .join(account)
            .join("drafts")
    }

    fn draft_path(&self, account: &str, stem: &str) -> PathBuf {
        self.drafts_dir(account).join(format!("{stem}.md"))
    }

    fn socket(&self) -> PathBuf {
        self.data_dir().join("runtime").join("daemon.sock")
    }

    fn pid_file(&self) -> PathBuf {
        self.data_dir().join("runtime").join("daemon.pid")
    }

    fn identity(&self) -> Identity {
        Identity {
            data_dir: self.data_dir(),
            config_dir: self.config_dir(),
        }
    }

    /// Write a draft and return its path.
    fn write_draft(&self, account: &str, stem: &str, document: &str) -> PathBuf {
        let path = self.draft_path(account, stem);
        fs::write(&path, document).expect("write draft");
        path
    }

    /// An `mp` invocation pointed at this sandbox, with the two watch hooks set
    /// and every other test-only hook explicitly cleared so an inherited
    /// variable cannot change an outcome.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(MP);
        cmd.env("HOME", self.home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env(WATCH_POLL_MS_ENV, FAST_POLL_MS.to_string())
            .env(WATCH_DEBOUNCE_MS_ENV, self.debounce_ms.to_string())
            .env_remove("MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES")
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_OPERATIONS")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME");
        cmd
    }

    /// Spawn `mp daemon run`, killed on drop, and wait until its socket
    /// accepts a connection. Its stdio goes nowhere.
    ///
    /// No `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES`: watching drafts takes no
    /// engine lock and opens no store, so the watcher must run without it.
    async fn start_daemon(&self) -> Proc {
        let child = self
            .cmd()
            .args(["daemon", "run"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mp daemon run");
        let proc = Proc(Some(child));
        self.wait_socket_live().await;
        proc
    }

    async fn wait_socket_live(&self) {
        let start = Instant::now();
        loop {
            if UnixStream::connect(self.socket()).await.is_ok() {
                return;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "the daemon socket {} never accepted a connection within {DEADLINE:?}",
                self.socket().display()
            );
            tokio::time::sleep(TICK).await;
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Ok(raw) = fs::read_to_string(self.pid_file()) {
            if let Ok(pid) = raw.trim().parse::<i32>() {
                if pid > 1 {
                    // Safety: a pid read from a pid file we own.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
    }
}

/// A spawned `mp` process, killed and reaped on drop, so a panicking assertion
/// never leaves a daemon behind.
struct Proc(Option<Child>);

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// Layer (b) - helpers
// ---------------------------------------------------------------------------

/// Await `fut` under [`DEADLINE`], failing the test rather than the suite's
/// patience if the daemon never answers.
async fn within<T>(label: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(DEADLINE, fut).await {
        Ok(value) => value,
        Err(_) => panic!("{label} did not finish within {DEADLINE:?}"),
    }
}

fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Gui,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Connect, handshake, and subscribe to the fan-out.
async fn subscribed(sandbox: &Sandbox) -> Connection {
    let mut conn = connected(sandbox).await;
    call_ok(&mut conn, "state.bootstrap", json!({})).await;
    conn
}

/// Connect and handshake, without subscribing.
async fn connected(sandbox: &Sandbox) -> Connection {
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&sandbox.socket()),
    )
    .await
    .expect("connecting to a live daemon socket succeeds");
    within(
        "Connection::initialize",
        conn.initialize(client_info(), sandbox.identity(), &[], &[]),
    )
    .await
    .expect("a compatible handshake succeeds");
    conn
}

async fn call_ok(conn: &mut Connection, method: &str, params: Value) -> Value {
    within(method, conn.call(method, params))
        .await
        .unwrap_or_else(|e| panic!("{method} was expected to succeed, got {e:?}"))
}

async fn call_err(conn: &mut Connection, method: &str, params: Value) -> RpcError {
    let error = within(method, conn.call(method, params))
        .await
        .expect_err("the call was expected to fail");
    match error {
        ClientError::Rpc(error) => error,
        other => {
            panic!("{method} failed for a transport reason rather than a domain one: {other:?}")
        }
    }
}

/// Read `state.event` notifications until one of `kind` arrives, and return it.
async fn next_event_of_kind(conn: &mut Connection, kind: &str) -> EventEnvelope {
    loop {
        let notification = within("a notification", conn.next_notification())
            .await
            .expect("the daemon delivers a notification rather than closing");
        if notification.method != METHOD_STATE_EVENT {
            continue;
        }
        let envelope: EventEnvelope = serde_json::from_value(notification.params.clone())
            .unwrap_or_else(|e| {
                panic!("the params are an event envelope: {e}; got {notification:?}")
            });
        if envelope.kind == kind {
            return envelope;
        }
    }
}

/// Every `state.event` that arrives inside `window`, which is how a test asserts
/// that something did *not* happen.
async fn events_within(conn: &mut Connection, window: Duration) -> Vec<EventEnvelope> {
    let deadline = Instant::now() + window;
    let mut seen = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return seen;
        }
        match tokio::time::timeout(left, conn.next_notification()).await {
            Err(_) => return seen,
            Ok(None) => return seen,
            Ok(Some(notification)) => {
                if notification.method != METHOD_STATE_EVENT {
                    continue;
                }
                if let Ok(envelope) =
                    serde_json::from_value::<EventEnvelope>(notification.params.clone())
                {
                    seen.push(envelope);
                }
            }
        }
    }
}

/// One account's `[[accounts]]` block, as a user would write it. No IMAP or
/// SMTP host: nothing in this file connects to anything.
fn account_toml(name: &str) -> String {
    format!(
        "[[accounts]]\n\
         name = \"{name}\"\n\
         default_from = \"{name}@example.com\"\n\
         \n\
         [accounts.mailboxes.inbox]\n\
         server = \"INBOX\"\n\
         \n"
    )
}

/// The draft row for `id` in a `state.bootstrap` snapshot, or a failure naming
/// what the snapshot did hold.
fn snapshot_draft<'a>(bootstrap: &'a Value, account: &str, id: &str) -> &'a Value {
    let list = bootstrap["snapshot"]["drafts"][account]
        .as_array()
        .unwrap_or_else(|| {
            panic!("the snapshot carries one drafts array per account, got {bootstrap}")
        });
    list.iter()
        .find(|row| row["id"] == Value::from(id))
        .unwrap_or_else(|| panic!("{account} lists a draft {id}, got {list:?}"))
}

// ---------------------------------------------------------------------------
// Layer (b) - the poll runs inside the daemon
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_direct_write_reaches_a_subscribed_client_as_draft_changed() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = subscribed(&sandbox).await;

    let path = sandbox.write_draft(
        "alpha",
        "angebot",
        &draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    );

    let event = next_event_of_kind(&mut conn, KIND_DRAFT_CHANGED).await;
    assert_keys(&event.payload, &DRAFT_CHANGED_KEYS, "draft.changed");
    assert_eq!(event.payload["account"], json!("alpha"));
    assert_eq!(event.payload["id"], json!("d-one"));
    assert_eq!(event.payload["path"], json!(path.display().to_string()));
    assert_eq!(event.payload["to"], json!("robin@example.com"));
    assert_eq!(event.payload["subject"], json!("Angebot"));
    assert_eq!(event.payload["status"], json!("draft"));
    assert_eq!(event.payload["valid"], json!(true));
    assert_eq!(event.payload["ready"], json!(true));

    let typed: DraftChanged = serde_json::from_value(event.payload.clone())
        .unwrap_or_else(|e| panic!("the payload is a DraftChanged: {e}; got {event:?}"));
    assert_eq!(typed.id, "d-one");
    assert!(
        event.revision > 0,
        "every event travels at the revision it committed at"
    );
}

#[tokio::test]
async fn an_atomic_save_reaches_a_subscribed_client_as_one_draft_changed() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = subscribed(&sandbox).await;

    let path = sandbox.write_draft(
        "alpha",
        "angebot",
        &draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    );
    let first = next_event_of_kind(&mut conn, KIND_DRAFT_CHANGED).await;
    assert_eq!(first.payload["subject"], json!("Angebot"));

    // Write beside the target and rename over it, which is what an editor,
    // `write_atomic` and every careful writer in this codebase do.
    let tmp = sandbox.drafts_dir("alpha").join("angebot.md.tmp");
    fs::write(
        &tmp,
        draft_document(
            "d-one",
            "robin@example.com",
            "Angebot nach dem Rename",
            "draft",
            "Rewritten wholesale.",
        ),
    )
    .expect("write the temporary");
    fs::rename(&tmp, &path).expect("rename over the target");

    let second = next_event_of_kind(&mut conn, KIND_DRAFT_CHANGED).await;
    assert_eq!(second.payload["subject"], json!("Angebot nach dem Rename"));
    assert_eq!(
        second.payload["path"],
        json!(path.display().to_string()),
        "the temporary never had an event of its own"
    );
}

/// The socket half of the burst assertion. The debounce is wide here so that a
/// handful of `fs::write` calls in a row cannot straddle it on any machine;
/// the deterministic version of this property is in layer (a).
#[tokio::test]
async fn rapid_saves_reach_a_client_as_one_event_carrying_the_last_write() {
    let sandbox = Sandbox::with_accounts_at(&["alpha"], WIDE_DEBOUNCE_MS);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = subscribed(&sandbox).await;

    for (index, subject) in ["eins", "zwei", "drei", "vier"].iter().enumerate() {
        sandbox.write_draft(
            "alpha",
            "angebot",
            &draft_document(
                "d-one",
                "robin@example.com",
                subject,
                "draft",
                &format!("Body for {subject}.{}", ".".repeat(index)),
            ),
        );
    }

    let event = next_event_of_kind(&mut conn, KIND_DRAFT_CHANGED).await;
    assert_eq!(
        event.payload["subject"],
        json!("vier"),
        "the daemon reparsed the final file state, not the first"
    );

    let after = events_within(&mut conn, Duration::from_millis(WIDE_DEBOUNCE_MS * 2)).await;
    let more: Vec<&EventEnvelope> = after
        .iter()
        .filter(|event| event.kind == KIND_DRAFT_CHANGED)
        .collect();
    assert!(
        more.is_empty(),
        "four saves inside one debounce window are one event, not four: {more:?}"
    );
}

#[tokio::test]
async fn deleting_a_draft_reaches_a_client_as_a_remove_of_its_resource() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = subscribed(&sandbox).await;

    let path = sandbox.write_draft(
        "alpha",
        "angebot",
        &draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    );
    next_event_of_kind(&mut conn, KIND_DRAFT_CHANGED).await;

    fs::remove_file(&path).expect("delete the draft");
    let event = next_event_of_kind(&mut conn, KIND_REMOVE).await;
    assert_eq!(
        event.payload,
        json!({"resource": "draft:alpha/d-one"}),
        "the resource the two draft kinds replace is the one a removal drops"
    );

    tokio::time::sleep(SILENCE).await;
    assert!(
        !path.exists(),
        "the watcher does not restore a draft the user deleted"
    );
}

#[tokio::test]
async fn a_signature_write_reaches_a_client_as_signature_changed() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = subscribed(&sandbox).await;

    let path = sandbox.signatures_dir().join("work.md");
    fs::write(&path, "Mit freundlichen Gruessen\nSylvain\n").expect("write signature");

    let event = next_event_of_kind(&mut conn, KIND_SIGNATURE_CHANGED).await;
    assert_keys(&event.payload, &SIGNATURE_CHANGED_KEYS, "signature.changed");
    assert_eq!(event.payload["name"], json!("work"));
    assert_eq!(event.payload["path"], json!(path.display().to_string()));
    let typed: SignatureChanged = serde_json::from_value(event.payload.clone())
        .unwrap_or_else(|e| panic!("the payload is a SignatureChanged: {e}; got {event:?}"));
    assert_eq!(typed.name, "work");
}

// ---------------------------------------------------------------------------
// Layer (b) - an invalid draft: the event, the snapshot, the refusal, the file
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unparseable_draft_reaches_a_client_as_draft_invalid_with_a_line() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = subscribed(&sandbox).await;

    let document = unparseable_document("d-broken");
    let path = sandbox.write_draft("alpha", "kaputt", &document);

    let event = next_event_of_kind(&mut conn, KIND_DRAFT_INVALID).await;
    assert_keys(&event.payload, &DRAFT_INVALID_KEYS, "draft.invalid");
    assert_eq!(event.payload["account"], json!("alpha"));
    assert_eq!(event.payload["id"], json!("kaputt"));
    assert_eq!(event.payload["path"], json!(path.display().to_string()));

    let typed: DraftInvalid = serde_json::from_value(event.payload.clone())
        .unwrap_or_else(|e| panic!("the payload is a DraftInvalid: {e}; got {event:?}"));
    assert!(!typed.diagnostics.is_empty());
    let line = typed.diagnostics[0]
        .line
        .expect("a YAML scan error carries a position");
    assert!(
        (1..=document.lines().count() as u32).contains(&line),
        "the line is 1-based and file-relative, got {line}"
    );
}

/// The plan's "a file the daemon cannot parse is left byte-identical on disk",
/// measured across many polls rather than one.
#[tokio::test]
async fn an_unparseable_draft_is_left_byte_identical_across_many_polls() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = subscribed(&sandbox).await;

    let document = unparseable_document("d-broken");
    let path = sandbox.write_draft("alpha", "kaputt", &document);
    let before = fs::read(&path).expect("read the broken draft");
    let before_mtime = fs::metadata(&path)
        .expect("stat")
        .modified()
        .expect("mtime");

    next_event_of_kind(&mut conn, KIND_DRAFT_INVALID).await;
    // Many more polls than the one that published the diagnostic.
    tokio::time::sleep(Duration::from_millis(FAST_POLL_MS * 40)).await;

    assert_eq!(
        fs::read(&path).expect("read it back"),
        before,
        "the daemon never rewrites a draft it could not parse, not even to mint an `id:`"
    );
    assert_eq!(
        fs::metadata(&path)
            .expect("stat")
            .modified()
            .expect("mtime"),
        before_mtime,
        "and it did not touch the file at all: an mtime that moved would restart the debounce \
         on every poll and publish the same diagnostic forever"
    );
    assert_eq!(
        String::from_utf8_lossy(&before),
        document,
        "the bytes on disk are the user's, unchanged"
    );
}

#[tokio::test]
async fn approving_an_unparseable_draft_is_refused_and_leaves_the_file_untouched() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = subscribed(&sandbox).await;

    let document = unparseable_document("d-broken");
    let path = sandbox.write_draft("alpha", "kaputt", &document);
    next_event_of_kind(&mut conn, KIND_DRAFT_INVALID).await;

    let error = call_err(
        &mut conn,
        "draft.approve",
        json!({"account": "alpha", "id": "kaputt"}),
    )
    .await;
    assert_eq!(
        error.code,
        ErrorCode::DraftInvalid.code(),
        "a draft that will not parse is refused by its own domain code, not by invalid_params: \
         {error:?}"
    );
    let data = error
        .data
        .clone()
        .expect("the refusal carries the diagnostics the client already renders");
    assert_keys(&data, &DRAFT_INVALID_KEYS, "the refusal payload");
    let typed: DraftInvalid = serde_json::from_value(data)
        .unwrap_or_else(|e| panic!("the refusal data is a DraftInvalid: {e}; got {error:?}"));
    assert_eq!(typed.id, "kaputt");
    assert!(!typed.diagnostics.is_empty());

    assert_eq!(
        fs::read_to_string(&path).expect("read it back"),
        document,
        "a refused approve writes nothing: the file is still the user's to fix"
    );
}

#[tokio::test]
async fn approving_a_draft_rewrites_only_its_status_line_and_publishes_the_change() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = subscribed(&sandbox).await;

    let document = draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body.");
    let path = sandbox.write_draft("alpha", "angebot", &document);
    next_event_of_kind(&mut conn, KIND_DRAFT_CHANGED).await;

    let result = call_ok(
        &mut conn,
        "draft.approve",
        json!({"account": "alpha", "id": "d-one"}),
    )
    .await;
    assert_keys(
        &result,
        &["account", "id", "path", "status"],
        "the draft.approve result",
    );
    assert_eq!(result["account"], json!("alpha"));
    assert_eq!(result["id"], json!("d-one"));
    assert_eq!(result["status"], json!("approved"));
    assert_eq!(result["path"], json!(path.display().to_string()));

    assert_eq!(
        fs::read_to_string(&path).expect("read it back"),
        document.replace("status: draft", "status: approved"),
        "`draft::mark_as_approved` rewrites the one line and re-serialises nothing"
    );

    let event = next_event_of_kind(&mut conn, KIND_DRAFT_CHANGED).await;
    assert_eq!(
        event.payload["status"],
        json!("approved"),
        "the watcher notices the daemon's own write like any other, so a client learns the new \
         state from the same event it would have got from `$EDITOR`"
    );
}

#[tokio::test]
async fn approving_refuses_an_unknown_account_and_an_unknown_draft() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let error = call_err(
        &mut conn,
        "draft.approve",
        json!({"account": "gamma", "id": "d-one"}),
    )
    .await;
    assert_eq!(error.code, ErrorCode::AccountUnknown.code());
    assert_eq!(error.data, Some(json!({"account": "gamma"})));

    let error = call_err(
        &mut conn,
        "draft.approve",
        json!({"account": "alpha", "id": "d-nothing"}),
    )
    .await;
    assert_eq!(
        error.code, -32602,
        "a draft id nothing resolves to is a parameter the daemon cannot honour, which is what \
         `tests/daemon_operations.rs` already decided for an unknown operation id"
    );
    assert_eq!(
        error.data,
        Some(json!({"account": "alpha", "id": "d-nothing"}))
    );

    let error = call_err(&mut conn, "draft.approve", json!({"account": "alpha"})).await;
    assert_eq!(error.code, -32602, "the id is a required parameter");
}

// ---------------------------------------------------------------------------
// Layer (b) - the snapshot
// ---------------------------------------------------------------------------

/// Watched drafts reduce into the canonical state, so a client that connects to
/// a daemon that has been running for a week sees the drafts on disk without
/// anybody having to touch one.
#[tokio::test]
async fn a_watched_draft_is_in_the_bootstrap_snapshot_of_a_later_client() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut first = subscribed(&sandbox).await;

    let path = sandbox.write_draft(
        "alpha",
        "angebot",
        &draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    );
    next_event_of_kind(&mut first, KIND_DRAFT_CHANGED).await;

    let mut second = connected(&sandbox).await;
    let bootstrap = call_ok(&mut second, "state.bootstrap", json!({})).await;
    let row = snapshot_draft(&bootstrap, "alpha", "d-one");
    assert_keys(row, &DRAFT_ROW_KEYS, "a snapshot draft row");
    assert_eq!(row["subject"], json!("Angebot"));
    assert_eq!(row["status"], json!("draft"));
    assert_eq!(row["to"], json!("robin@example.com"));
    assert_eq!(row["path"], json!(path.display().to_string()));
    assert_eq!(row["valid"], json!(true));
    assert_eq!(row["ready"], json!(true));
}

#[tokio::test]
async fn an_unparseable_draft_stays_visible_in_the_snapshot_as_an_invalid_row() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut first = subscribed(&sandbox).await;

    let path = sandbox.write_draft("alpha", "kaputt", &unparseable_document("d-broken"));
    next_event_of_kind(&mut first, KIND_DRAFT_INVALID).await;

    let mut second = connected(&sandbox).await;
    let bootstrap = call_ok(&mut second, "state.bootstrap", json!({})).await;
    let row = snapshot_draft(&bootstrap, "alpha", "kaputt");
    assert_keys(row, &DRAFT_ROW_KEYS, "a snapshot draft row");
    assert_eq!(
        row["valid"],
        json!(false),
        "#0080: a draft that will not parse must stay in front of the user, unopenable but named"
    );
    assert_eq!(
        row["status"],
        json!("invalid"),
        "not an `EmailStatus`, and deliberately: nothing read a status out of this file"
    );
    assert_eq!(row["subject"], json!(""));
    assert_eq!(row["to"], Value::Null);
    assert_eq!(row["ready"], json!(false));
    assert_eq!(row["path"], json!(path.display().to_string()));
}

#[tokio::test]
async fn a_deleted_draft_leaves_the_snapshot() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon().await;
    let mut first = subscribed(&sandbox).await;

    let path = sandbox.write_draft(
        "alpha",
        "angebot",
        &draft_document("d-one", "robin@example.com", "Angebot", "draft", "Body."),
    );
    next_event_of_kind(&mut first, KIND_DRAFT_CHANGED).await;
    fs::remove_file(&path).expect("delete the draft");
    next_event_of_kind(&mut first, KIND_REMOVE).await;

    let mut second = connected(&sandbox).await;
    let bootstrap = call_ok(&mut second, "state.bootstrap", json!({})).await;
    let list = bootstrap["snapshot"]["drafts"]["alpha"]
        .as_array()
        .unwrap_or_else(|| panic!("the snapshot carries a drafts array per account: {bootstrap}"))
        .clone();
    assert!(
        list.iter().all(|row| row["id"] != json!("d-one")),
        "the removal reduced into the snapshot, so a client that bootstraps now never sees the \
         row at all: {list:?}"
    );
}
