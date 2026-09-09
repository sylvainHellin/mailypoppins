//! The typed sync outcome and the client-side formatters (#0122, plan unit
//! P3b-U5).
//!
//! This file is a **contract test**: it is written before
//! `crates/mp-protocol/src/events.rs`, `crates/mp-client/src/format.rs` and
//! `src/daemon/sync_outcome.rs` exist, against the contract fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.4 (unit P3b-U5) and
//! the source plan's event-payload prose ("a sync outcome is one of those small
//! resources, and it is no longer a count plus a colour … the sync-completed
//! event therefore carries a typed outcome with those two lists, the deferred-prune
//! count, the failed-mutation count, and a severity, rather than a formatted
//! string. Clients derive their own presentation from it: the TUI's status
//! suffix and its downgrade to Warning, `mp sync`'s stderr line on a zero exit
//! code, and the GUI's equivalent surface. A client that cannot render one of
//! those states must not present the tick as clean, which is the property the
//! whole detector exists for."). It does not compile under `--features daemon`
//! today, and that failure *is* the proof the contract has no stub behind it.
//! An implementer (P3b-U6) does not edit this file; they make it pass.
//!
//! # The two layers, and why the split
//!
//! **(a) In-process, against the three new surfaces.** The payload's field set,
//! its severity rule, its JSON round trip, the two pure formatters and the way
//! a `Change::SyncCompleted` travels through `Event::from_change` and one
//! connection's `Outbound` are properties of types, not of a process. They are
//! driven directly, which is also the only place "these two outcomes did **not**
//! coalesce" is observable as itself: over a socket, two notifications arriving
//! proves it only because nothing merged them, and what merges them lives in
//! `Outbound`.
//!
//! **(b) Over the socket, against a spawned `mp daemon run`.** Whether the
//! daemon commits the outcome through [`CanonicalState::apply`] at all, whether
//! a client sees it as a `sync.completed` `state.event` notification, and
//! whether every tick gets its own strictly increasing revision are properties
//! of the daemon process. Phase 3b schedules no tick of its own, so the ticks
//! are forced by a test-only environment hook, exactly as
//! `MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST` forces the changes
//! `tests/daemon_events.rs` needs.
//!
//! # Surface under test
//!
//! ```rust,ignore
//! // crates/mp-protocol/src/events.rs  ->  mp_protocol::events
//! pub const KIND_SYNC_COMPLETED: &str = "sync.completed";
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
//! #[serde(rename_all = "lowercase")]
//! pub enum Severity { Ok, Warning, Error }        // "ok" | "warning" | "error"
//!
//! #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
//! pub struct SyncCompleted {
//!     pub account: String,
//!     pub severity: Severity,
//!     pub saved: u64,
//!     pub skipped: u64,
//!     pub flags_updated: u64,
//!     pub pruned: u64,
//!     pub prunes_deferred: u64,
//!     pub uid_rebound: u64,
//!     pub uidvalidity_resets: u64,
//!     pub bodies_truncated: u64,
//!     pub non_converging: Vec<String>,
//!     pub failed_mutations: u64,
//!     pub error: Option<String>,
//! }
//!
//! // crates/mp-client/src/format.rs  ->  mp_client::format
//! pub fn sync_status_line(outcome: &SyncCompleted) -> String;      // the TUI's line
//! pub fn sync_cli_lines(outcome: &SyncCompleted) -> Vec<String>;   // `mp sync`'s lines
//!
//! // src/daemon/sync_outcome.rs  ->  mailypoppins::daemon::sync_outcome
//! pub const FAKE_SYNC_OUTCOME_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME";
//! pub fn from_sync_result(
//!     account: &str,
//!     result: &mailypoppins::sync::SyncResult,
//!     failed_mutations: u64,
//!     error: Option<String>,
//! ) -> SyncCompleted;
//!
//! // src/daemon/state/snapshot.rs  ->  mailypoppins::daemon::state
//! pub enum Change { …, SyncCompleted(SyncCompleted) }
//! ```
//!
//! # Contract points this file pins beyond the plan text
//!
//! - **The payload type lives in `mp-protocol`, not in the root crate.** It is
//!   a wire shape, the GUI and the CLI both deserialise it, and `mp-client`
//!   formats it; `mp-client` must never depend on `mailypoppins`, so the type
//!   they share can only live in the protocol crate. The constructor that reads
//!   a [`SyncResult`] lives in the root crate for the mirror-image reason:
//!   `mp-protocol` knows nothing about the engine.
//! - **The constructor is a free function, not an inherent `impl`.** A foreign
//!   type takes no inherent `impl` from the root crate, and a `From` would have
//!   to carry the account name, the failed-mutation count and the error in a
//!   tuple, which reads worse than four named arguments at every call site.
//! - **`from_sync_result` derives the severity, sorts and deduplicates
//!   `non_converging`, and widens every `usize` to `u64`.** The sort is the
//!   canonicalisation both of today's formatters do at print time
//!   (`src/main.rs`'s `mp sync` arm and `src/tui/helpers.rs::finish_sync` both
//!   `sort()` then `dedup()`); doing it once, in the constructor, is what lets
//!   the pure formatters be a straight walk over the payload and what makes two
//!   payloads for the same tick compare equal.
//! - **The severity rule, in this order:** an `error` that is `Some` is
//!   `Severity::Error` whatever else the tick did; otherwise a non-empty
//!   `non_converging` **or** `failed_mutations > 0` is `Severity::Warning`;
//!   otherwise `Severity::Ok`. That is exactly `tui::bg::drained_sync_level`
//!   read through the payload instead of through a string:
//!   `NON_CONVERGING_MARKER` and `FAILED_OPS_MARKER` are the two substrings it
//!   downgrades on, and nothing else it prints downgrades anything.
//! - **`bodies_truncated` and `prunes_deferred` never raise the severity.** A
//!   deadline stop is progress: the mailbox has more mail on the server and the
//!   next tick resumes from the same cursor (#0113). A deferred prune is a
//!   suspended deletion, not a failure (#0072). Today's TUI prints both and
//!   stays green, and the plan says a deadline stop is `ok` + `bodies_truncated
//!   > 0` in so many words.
//! - **`bodies_truncated` is a count, not a list of mailbox names.**
//!   `SyncResult.bodies_truncated` is a `usize` today (`src/sync/mod.rs`), while
//!   the source plan's prose says "deadline-stopped mailboxes". The payload
//!   carries the count, which is the data that exists; widening it to names is a
//!   behaviour change in the engine and belongs in its own ticket, which
//!   P3b-U6 records in `BACKLOG.md`.
//! - **Severity travels on the wire and is not recomputed on deserialisation.**
//!   A client applies what the daemon decided; a client that recomputed could
//!   disagree with the daemon after a rule change and present a warning tick as
//!   clean, which is the one thing the detector exists to prevent.
//! - **The payload carries no formatted string.**
//!   [`no_field_of_the_payload_carries_a_formatted_status_string`] walks every
//!   value in the JSON object and refuses `Synced`, the two marker texts and the
//!   engine-lock refusal sentence. `error` is the one free-text field, and it is
//!   the engine's error rendered with `{:#}`, never a status line.
//! - **`sync.completed` is a non-coalescing event.**
//!   `Event::from_change(&Change::SyncCompleted(..))` is
//!   `Event::Lifecycle { kind: "sync.completed", payload }`, so no two outcomes
//!   ever merge and a queued outcome survives an overflow discard. The source
//!   plan's backpressure rule is "coalesce equivalent invalidations by resource
//!   and query scope; **preserve non-coalescible command outcomes** and
//!   lifecycle events", and a sync outcome is the outcome of a command: two
//!   ticks are two facts about two moments, and merging a warning into a later
//!   clean tick would present that tick as clean. An `Event::Replace` cannot
//!   express this, because every `Replace` is keyed by `(kind, resource)` and
//!   would coalesce by construction.
//! - **The change reduces to nothing.** `CanonicalState::apply` fans a
//!   `Change::SyncCompleted` out at a fresh revision and leaves the snapshot
//!   untouched, exactly as it does for a `MailboxCounts` naming a mailbox no
//!   account has. Phase 3b's snapshot carries no last-sync section, so nothing
//!   in it could be reduced into; a client that bootstraps between two ticks
//!   learns about neither, which is what a command outcome is.
//! - **The ticks are forced by a test-only environment hook.** Phase 3b
//!   schedules no tick, so nothing would ever emit an outcome and every socket
//!   assertion here would be vacuous. `MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME`
//!   holds a JSON **array** of `sync.completed` payload objects (a bare object
//!   is read as an array of one). With `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES`
//!   also set, the daemon commits one `Change::SyncCompleted` per element, in
//!   array order, through `CanonicalState::apply`, **after every
//!   `state.bootstrap` it answers** and off the bootstrap's own path, against
//!   the first configured account: each element's `account` field is ignored and
//!   replaced by that account's configured name, and every other field travels
//!   verbatim, `severity` included, so a test can pin a severity no fake sync
//!   could produce. Unset, empty, unparseable, or without the account-runtimes
//!   opt-in it does nothing at all, exactly as `fake_event_burst` behaves. No
//!   flag exposes it, so `mp --help` never moves; its name is
//!   `daemon::sync_outcome::FAKE_SYNC_OUTCOME_ENV`, re-exported from
//!   `daemon::lifecycle` beside the other four hooks, so the test and the daemon
//!   cannot drift apart, and P3b-U6 documents it in `docs/daemon-operations.md`.
//! - **The two formatters are pure and live in `mp-client`.** They take the
//!   payload and nothing else: no `App`, no config, no colour. `mp sync` and the
//!   TUI keep their own colouring, since a glyph's colour is a terminal
//!   decision and a GUI has neither.
//!
//! # The wordings, and where each literal comes from
//!
//! Every string this file pins is lifted from today's source, so the daemon-era
//! line a user reads is the line they read now:
//!
//! - [`sync_status_line`] is `src/tui/helpers.rs::finish_sync` clause for
//!   clause, in that function's order, followed by the failed-mutation suffix
//!   `drain_pending_ops` appends (`"; {n} mutation(s) failed and were rolled
//!   back (see the log)"`).
//! - [`sync_cli_lines`] is the `println!` sequence of `src/main.rs`'s
//!   `sync_one_account`, glyph included and uncoloured, in that function's
//!   order, and the error line is the `eprintln!` of the `--all-accounts` arm
//!   (`"✗ {account}: {error}"`).
//! - **Two lines are not extractable, because today's `mp sync` cannot produce
//!   the state.** `mp sync` passes no body-fetch deadline (it is the explicit
//!   recovery path), so its `bodies_truncated` is always `0` and it prints no
//!   line for it; and it reports a failed mutation drain as
//!   `"  ↻ mutations: {completed} completed, {failed} failed"`, a shape the
//!   payload cannot rebuild because it carries no `completed` count. A daemon
//!   tick is deadline-bounded and does drain mutations, so both states reach
//!   `mp sync` in Phase 4. This file pins the TUI's wording for both, at the
//!   glyph their severity contribution earns: `ℹ` for the deadline stop, which
//!   is progress, and `⚠` for the rolled-back mutations, which is a warning.
//!   They are the only two literals below that a reviewer will not find in the
//!   current tree.
//! - The `[dry-run] ` prefix is not derivable and is not pinned: a daemon never
//!   dry-runs, so the payload carries no such flag and `mp sync --dry-run`
//!   keeps its own local formatting path.
//! - The TUI's failure line gains the account name unconditionally
//!   (`"Fetch failed ({account}): {error}"`), where today's
//!   `tui::bg::account_label` adds it only when more than one account is
//!   configured. A pure function over a payload cannot know how many accounts a
//!   client has, and a daemon client is multi-account by construction.
//! - The engine-lock refusal (`SYNC_SKIPPED_MARKER`, "another engine is syncing")
//!   has no field in the plan's payload and is therefore not representable here.
//!   It is out of this unit's scope: a daemon runtime holds the lock for its
//!   lifetime and its ticks are never refused (`docs/lessons-learned.md`, "a
//!   runtime that holds the engine lock must not call the guarded entry point").
//!
//! # Process hygiene
//!
//! Every daemon this file starts is killed before the test returns, including on
//! panic: the child goes into a [`Proc`] whose `Drop` kills and reaps it, and
//! [`Sandbox`]'s `Drop` kills whatever `daemon.pid` names. Every wait is a
//! bounded poll or a `tokio::time::timeout`. Tests never touch the test
//! process's environment: each passes `HOME`, `MAILYPOPPINS_DATA_DIR` and
//! `MAILYPOPPINS_CONFIG_DIR` to the child through `Command::env`, so they are
//! safe to run in parallel and under `--test-threads=1` alike.
//!
//! The harness is a trimmed copy of `tests/daemon_events.rs`'s rather than a
//! shared `tests/common/` module: each daemon test file needs a different half
//! of it, and a shared module would have to be built into every explicit
//! `[[test]]` target.

use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::UnixStream;

use mp_protocol::events::{Severity, SyncCompleted, KIND_SYNC_COMPLETED};
use mp_protocol::{
    EventEnvelope, JSONRPC_VERSION, METHOD_STATE_EVENT, METHOD_STATE_RESYNC_REQUIRED,
};

use mp_client::format::{sync_cli_lines, sync_status_line};
use mp_client::{
    ClientInfo, ClientKind, Connection, Identity, InitializeResult, Observe, StateTracker,
};

use mailypoppins::daemon::dispatch::ResourceId;
use mailypoppins::daemon::lifecycle::FAKE_SYNC_OUTCOME_ENV as LIFECYCLE_FAKE_SYNC_OUTCOME_ENV;
use mailypoppins::daemon::state::events::{Event, Outbound, Outgoing, Push, Subscriber};
use mailypoppins::daemon::state::{
    AccountSeed, CanonicalState, Change, ConnectionId, InstanceId, MailboxSeed, Revision,
};
use mailypoppins::daemon::sync_outcome::{from_sync_result, FAKE_SYNC_OUTCOME_ENV};
use mailypoppins::sync::SyncResult;

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Upper bound on any single wait: the socket appearing, a frame arriving, a
/// round trip returning. Generous, because it is a ceiling and never a sleep.
const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

/// The account every test uses, so a failure names something readable.
const ACCOUNT: &str = "alpha";

/// The environment opt-in for account runtimes (plan section 3.0). Spelled out
/// rather than imported so the test states the name a user would type.
const ACCOUNT_RUNTIMES_ENV: &str = "MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES";

/// The substring `tui::bg::drained_sync_level` downgrades a green line on when
/// mutations were rolled back (`src/tui/helpers.rs::FAILED_OPS_MARKER`).
/// Spelled out rather than imported: it is `pub(crate)`, and the point of this
/// file is that the derived line still carries it.
const FAILED_OPS_MARKER: &str = "mutation(s) failed and were rolled back";

/// The other substring it downgrades on (#0115,
/// `src/tui/helpers.rs::NON_CONVERGING_MARKER`).
const NON_CONVERGING_MARKER: &str = "fetch not converging";

/// The engine-lock refusal sentence (`src/tui/helpers.rs::SYNC_SKIPPED_MARKER`),
/// which no payload field may carry.
const SYNC_SKIPPED_MARKER: &str = "Sync skipped: another engine is syncing";

/// The first word of every formatted success line, in both surfaces. A payload
/// that carries it is a payload carrying a rendered string.
const SYNCED_WORD: &str = "Synced";

/// The four substrings no value in the payload may contain.
const FORMATTED_FRAGMENTS: [&str; 4] = [
    SYNCED_WORD,
    FAILED_OPS_MARKER,
    NON_CONVERGING_MARKER,
    SYNC_SKIPPED_MARKER,
];

/// The thirteen keys the plan's payload has, and no others.
const PAYLOAD_KEYS: [&str; 13] = [
    "account",
    "bodies_truncated",
    "error",
    "failed_mutations",
    "flags_updated",
    "non_converging",
    "pruned",
    "prunes_deferred",
    "saved",
    "severity",
    "skipped",
    "uid_rebound",
    "uidvalidity_resets",
];

// ---------------------------------------------------------------------------
// Layer (a) helpers
// ---------------------------------------------------------------------------

/// A clean tick's payload: every count zero, nothing to warn about.
fn clean() -> SyncCompleted {
    SyncCompleted {
        account: ACCOUNT.to_string(),
        severity: Severity::Ok,
        saved: 0,
        skipped: 0,
        flags_updated: 0,
        pruned: 0,
        prunes_deferred: 0,
        uid_rebound: 0,
        uidvalidity_resets: 0,
        bodies_truncated: 0,
        non_converging: Vec::new(),
        failed_mutations: 0,
        error: None,
    }
}

/// A tick that did something in every counter at once, with two distinct
/// non-converging mailboxes and a rolled-back mutation, so one assertion pins
/// every clause and its order.
///
/// `non_converging` is deliberately unsorted and holds a duplicate: the
/// constructor is what canonicalises it.
fn busy_result() -> SyncResult {
    SyncResult {
        saved: 3,
        skipped: 2,
        flags_updated: 4,
        pruned: 5,
        prunes_deferred: 6,
        uid_rebound: 7,
        bodies_truncated: 8,
        uidvalidity_resets: 9,
        non_converging: vec!["Sent".to_string(), "INBOX".to_string(), "INBOX".to_string()],
        ..SyncResult::default()
    }
}

/// [`busy_result`] as a payload, with two mutations rolled back and no error.
fn busy() -> SyncCompleted {
    from_sync_result(ACCOUNT, &busy_result(), 2, None)
}

/// The event a committed outcome travels as, through the daemon's own bridge.
fn sync_event(outcome: &SyncCompleted) -> Event {
    Event::from_change(&Change::SyncCompleted(outcome.clone()))
}

/// A coalescible domain event, used to overflow a queue around a queued
/// outcome.
fn counts(account: &str, mailbox: &str) -> Event {
    Event::Invalidate {
        resource: ResourceId::new(format!("mailbox:{account}/{mailbox}")),
        scope: json!({"query": "counts"}),
    }
}

/// Everything a queue will hand over, oldest first, leaving it empty.
fn drain(queue: &mut Outbound) -> Vec<Outgoing> {
    let mut out = Vec::new();
    while let Some(item) = queue.pop() {
        out.push(item);
    }
    out
}

/// A fresh canonical state with one seeded account and a known instance.
fn fresh_state() -> Arc<CanonicalState> {
    Arc::new(CanonicalState::new(
        InstanceId::new("01J8Z6Q9X4V3N2M1K0H7G5F4D3"),
        vec![AccountSeed {
            name: ACCOUNT.to_string(),
            mailboxes: vec![MailboxSeed {
                role: "inbox".to_string(),
                slug: "inbox".to_string(),
                label: "Inbox".to_string(),
            }],
        }],
    ))
}

/// Every string that appears anywhere in a JSON value, keys excluded.
fn string_values(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => {
            for item in items {
                string_values(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                string_values(item, out);
            }
        }
        _ => {}
    }
}

/// The keys of a JSON object, sorted, or a failure naming what it is instead.
fn sorted_keys(value: &Value) -> Vec<String> {
    let map = value
        .as_object()
        .unwrap_or_else(|| panic!("the payload is a JSON object, got {value}"));
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort();
    keys
}

// ---------------------------------------------------------------------------
// Layer (a) - the wire shape
// ---------------------------------------------------------------------------

/// The event kind the plan fixes, as one constant both sides read.
#[test]
fn the_event_kind_is_sync_completed() {
    assert_eq!(
        KIND_SYNC_COMPLETED, "sync.completed",
        "the plan fixes the kind string"
    );
}

/// The three severities travel as the three lowercase strings and nothing else.
#[test]
fn severity_travels_as_ok_warning_or_error() {
    assert_eq!(serde_json::to_value(Severity::Ok).expect("ok"), json!("ok"));
    assert_eq!(
        serde_json::to_value(Severity::Warning).expect("warning"),
        json!("warning")
    );
    assert_eq!(
        serde_json::to_value(Severity::Error).expect("error"),
        json!("error")
    );

    assert_eq!(
        serde_json::from_value::<Severity>(json!("ok")).expect("ok parses"),
        Severity::Ok
    );
    assert_eq!(
        serde_json::from_value::<Severity>(json!("warning")).expect("warning parses"),
        Severity::Warning
    );
    assert_eq!(
        serde_json::from_value::<Severity>(json!("error")).expect("error parses"),
        Severity::Error
    );
    assert!(
        serde_json::from_value::<Severity>(json!("OK")).is_err(),
        "the strings are lowercase, so nothing else is a severity"
    );
    assert!(
        serde_json::from_value::<Severity>(json!("fatal")).is_err(),
        "a fourth severity is not part of the contract"
    );
}

/// The payload is exactly the plan's object: those thirteen keys, those types,
/// `error` null when there is none, and nothing else.
#[test]
fn the_payload_is_exactly_the_documented_object() {
    let encoded = serde_json::to_value(busy()).expect("a payload serialises");
    assert_eq!(
        encoded,
        json!({
            "account": "alpha",
            "severity": "warning",
            "saved": 3,
            "skipped": 2,
            "flags_updated": 4,
            "pruned": 5,
            "prunes_deferred": 6,
            "uid_rebound": 7,
            "uidvalidity_resets": 9,
            "bodies_truncated": 8,
            "non_converging": ["INBOX", "Sent"],
            "failed_mutations": 2,
            "error": null,
        }),
        "the payload is the plan's object, field for field"
    );
    assert_eq!(
        sorted_keys(&encoded),
        PAYLOAD_KEYS.to_vec(),
        "no field beyond the thirteen the plan lists"
    );
}

/// A clean tick is the same object with zeros, an empty list and a null error,
/// so a client never has to branch on an absent key.
#[test]
fn a_clean_payload_carries_every_key_with_its_empty_value() {
    let encoded = serde_json::to_value(clean()).expect("a payload serialises");
    assert_eq!(
        encoded,
        json!({
            "account": "alpha",
            "severity": "ok",
            "saved": 0,
            "skipped": 0,
            "flags_updated": 0,
            "pruned": 0,
            "prunes_deferred": 0,
            "uid_rebound": 0,
            "uidvalidity_resets": 0,
            "bodies_truncated": 0,
            "non_converging": [],
            "failed_mutations": 0,
            "error": null,
        })
    );
    assert!(
        encoded["error"].is_null(),
        "a tick that did not fail carries a null error, never an empty string"
    );
}

/// What the daemon encodes is what a client decodes, including the severity it
/// decided: nothing is recomputed on the way back in.
#[test]
fn the_payload_round_trips_through_json_unchanged() {
    for original in [clean(), busy(), failed()] {
        let encoded = serde_json::to_string(&original).expect("serialise");
        let decoded: SyncCompleted = serde_json::from_str(&encoded).expect("deserialise");
        assert_eq!(decoded, original, "the round trip changed the payload");
    }

    // A severity the rule would not have produced survives the round trip: the
    // wire carries the daemon's decision, and a client applies it.
    let mut lying = clean();
    lying.severity = Severity::Warning;
    let decoded: SyncCompleted =
        serde_json::from_value(serde_json::to_value(&lying).expect("serialise"))
            .expect("deserialise");
    assert_eq!(
        decoded.severity,
        Severity::Warning,
        "a client applies the severity it was sent rather than deriving its own"
    );
}

/// The engine's error is a string on the wire, and it is the only free-text
/// field there is.
#[test]
fn an_error_travels_as_a_string() {
    let encoded = serde_json::to_value(failed()).expect("a payload serialises");
    assert_eq!(
        encoded["error"],
        json!("login refused: AUTHENTICATIONFAILED"),
        "the error is the engine's message, rendered by the daemon"
    );
    assert_eq!(encoded["severity"], json!("error"));
}

/// A failed tick's payload: the engine's error, rendered as the daemon renders
/// it with `{:#}`, and no marker text anywhere.
fn failed() -> SyncCompleted {
    from_sync_result(
        ACCOUNT,
        &SyncResult::default(),
        0,
        Some("login refused: AUTHENTICATIONFAILED".to_string()),
    )
}

/// The property the whole unit exists for: the outcome is data, and the
/// presentation is the client's.
#[test]
fn no_field_of_the_payload_carries_a_formatted_status_string() {
    for outcome in [clean(), busy(), failed()] {
        let encoded = serde_json::to_value(&outcome).expect("a payload serialises");
        let mut strings = Vec::new();
        string_values(&encoded, &mut strings);
        for value in &strings {
            for fragment in FORMATTED_FRAGMENTS {
                assert!(
                    !value.contains(fragment),
                    "the payload carries the formatted fragment {fragment:?} in {value:?}: \
                     a sync outcome is typed data, not a rendered line ({encoded})"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Layer (a) - the constructor and the severity rule
// ---------------------------------------------------------------------------

/// Every counter travels verbatim, widened to `u64`, including
/// `bodies_truncated` as the count the engine actually has.
#[test]
fn every_counter_travels_verbatim_from_the_sync_result() {
    let outcome = busy();
    assert_eq!(outcome.account, ACCOUNT);
    assert_eq!(outcome.saved, 3);
    assert_eq!(outcome.skipped, 2);
    assert_eq!(outcome.flags_updated, 4);
    assert_eq!(outcome.pruned, 5);
    assert_eq!(outcome.prunes_deferred, 6);
    assert_eq!(outcome.uid_rebound, 7);
    assert_eq!(
        outcome.bodies_truncated, 8,
        "the known gap: `SyncResult.bodies_truncated` is a count today, and the \
         payload carries the count rather than inventing a list of names"
    );
    assert_eq!(outcome.uidvalidity_resets, 9);
    assert_eq!(outcome.failed_mutations, 2);
    assert_eq!(outcome.error, None);
}

/// A tick that did nothing wrong is `ok`, which is the baseline every other
/// severity assertion is measured against.
#[test]
fn a_clean_tick_is_ok() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            saved: 4,
            skipped: 9,
            flags_updated: 2,
            pruned: 1,
            uid_rebound: 3,
            uidvalidity_resets: 1,
            ..SyncResult::default()
        },
        0,
        None,
    );
    assert_eq!(outcome.severity, Severity::Ok);
    assert!(outcome.non_converging.is_empty());
    assert_eq!(outcome.failed_mutations, 0);
    assert_eq!(outcome.error, None);
}

/// A fetch that downloaded the same mail again is a warning however green its
/// counts look (#0115).
#[test]
fn a_non_converging_mailbox_makes_the_tick_a_warning() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            saved: 12,
            non_converging: vec!["INBOX".to_string()],
            ..SyncResult::default()
        },
        0,
        None,
    );
    assert_eq!(outcome.severity, Severity::Warning);
    assert_eq!(outcome.non_converging, vec!["INBOX".to_string()]);
}

/// A rolled-back mutation is the other warning (#0039), and it needs no help
/// from the sync itself.
#[test]
fn a_rolled_back_mutation_makes_the_tick_a_warning() {
    let outcome = from_sync_result(ACCOUNT, &SyncResult::default(), 1, None);
    assert_eq!(outcome.severity, Severity::Warning);
    assert_eq!(outcome.failed_mutations, 1);
}

/// A deadline stop is progress, not failure: the mailbox has more mail and the
/// next tick resumes from the same cursor (#0113).
#[test]
fn a_deadline_stop_is_progress_and_stays_ok() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            saved: 40,
            bodies_truncated: 2,
            ..SyncResult::default()
        },
        0,
        None,
    );
    assert_eq!(
        outcome.severity,
        Severity::Ok,
        "a tick cut at its deadline downloaded mail and will download the rest"
    );
    assert_eq!(outcome.bodies_truncated, 2);
}

/// A held-back prune is a suspended deletion, and today's status line stays
/// green through it (`tui::bg::drained_sync_level` reads neither marker in it).
#[test]
fn a_deferred_prune_alone_does_not_downgrade_the_severity() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            prunes_deferred: 3,
            ..SyncResult::default()
        },
        0,
        None,
    );
    assert_eq!(outcome.severity, Severity::Ok);
    assert_eq!(outcome.prunes_deferred, 3);
}

/// An error outranks everything: a tick that failed is not a tick that warned.
#[test]
fn an_error_outranks_every_other_signal() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            non_converging: vec!["INBOX".to_string()],
            bodies_truncated: 5,
            ..SyncResult::default()
        },
        7,
        Some("connection reset".to_string()),
    );
    assert_eq!(outcome.severity, Severity::Error);
    assert_eq!(outcome.error.as_deref(), Some("connection reset"));
    assert_eq!(
        outcome.failed_mutations, 7,
        "the other signals still travel; only the severity is decided by the error"
    );
    assert_eq!(outcome.non_converging, vec!["INBOX".to_string()]);
}

/// The constructor canonicalises the list once, so both formatters are a
/// straight walk and two payloads for one tick compare equal.
#[test]
fn the_non_converging_names_are_sorted_and_deduplicated_once() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            non_converging: vec![
                "Sent".to_string(),
                "INBOX".to_string(),
                "Sent".to_string(),
                "Archive".to_string(),
            ],
            ..SyncResult::default()
        },
        0,
        None,
    );
    assert_eq!(
        outcome.non_converging,
        vec![
            "Archive".to_string(),
            "INBOX".to_string(),
            "Sent".to_string()
        ],
        "the payload carries a canonical list, sorted and deduplicated"
    );
}

/// The severity rule as a table, in the order the rule is checked, so a change
/// to any one branch fails here and not only in a wording test.
#[test]
fn the_severity_rule_holds_over_every_combination() {
    let cases: [(bool, u64, bool, Severity); 8] = [
        (false, 0, false, Severity::Ok),
        (true, 0, false, Severity::Warning),
        (false, 1, false, Severity::Warning),
        (true, 1, false, Severity::Warning),
        (false, 0, true, Severity::Error),
        (true, 0, true, Severity::Error),
        (false, 1, true, Severity::Error),
        (true, 1, true, Severity::Error),
    ];
    for (non_converging, failed_mutations, errored, expected) in cases {
        let result = SyncResult {
            non_converging: if non_converging {
                vec!["INBOX".to_string()]
            } else {
                Vec::new()
            },
            ..SyncResult::default()
        };
        let outcome = from_sync_result(
            ACCOUNT,
            &result,
            failed_mutations,
            errored.then(|| "the body failed".to_string()),
        );
        assert_eq!(
            outcome.severity, expected,
            "non_converging={non_converging}, failed_mutations={failed_mutations}, \
             errored={errored}"
        );
    }
}

// ---------------------------------------------------------------------------
// Layer (a) - the TUI's line, derived
// ---------------------------------------------------------------------------

/// The line a clean tick puts in the status bar today, from
/// `src/tui/helpers.rs::finish_sync`.
#[test]
fn the_status_line_of_a_clean_tick_is_todays_wording() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            saved: 3,
            skipped: 12,
            ..SyncResult::default()
        },
        0,
        None,
    );
    assert_eq!(sync_status_line(&outcome), "Synced: 3 new, 12 existing");
}

/// Every optional clause, in `finish_sync`'s order, with the failed-mutation
/// suffix `drain_pending_ops` appends last.
#[test]
fn the_status_line_appends_every_clause_in_todays_order() {
    assert_eq!(
        sync_status_line(&busy()),
        "Synced: 3 new, 2 existing, 4 status updated, 7 renumbered, \
         5 no longer in this mailbox, \
         6 removal(s) held back (incomplete pass, run a full sync), \
         8 mailbox(es) stopped at the fetch deadline (resuming next sync), \
         fetch not converging on INBOX, Sent (see the log)\
         ; 2 mutation(s) failed and were rolled back (see the log)"
    );
}

/// A clause a tick did not earn is absent: the line is built from what
/// happened, not from a template with zeros in it.
#[test]
fn the_status_line_omits_the_clauses_the_tick_did_not_earn() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            saved: 1,
            flags_updated: 2,
            ..SyncResult::default()
        },
        0,
        None,
    );
    let line = sync_status_line(&outcome);
    assert_eq!(line, "Synced: 1 new, 0 existing, 2 status updated");
    assert!(!line.contains("renumbered"));
    assert!(!line.contains("held back"));
    assert!(!line.contains("fetch deadline"));
}

/// The mailboxes are named in the payload's canonical order and joined the way
/// `finish_sync` joins them.
#[test]
fn the_status_line_names_the_non_converging_mailboxes_in_order() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            saved: 6,
            non_converging: vec![
                "Sent".to_string(),
                "Archive".to_string(),
                "Sent".to_string(),
            ],
            ..SyncResult::default()
        },
        0,
        None,
    );
    assert_eq!(
        sync_status_line(&outcome),
        "Synced: 6 new, 0 existing, fetch not converging on Archive, Sent (see the log)"
    );
}

/// A deadline stop reads as progress in the line, exactly as it does in the
/// severity: it says what happened and carries neither marker.
#[test]
fn the_status_line_of_a_deadline_stop_reads_as_progress() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            saved: 40,
            bodies_truncated: 2,
            ..SyncResult::default()
        },
        0,
        None,
    );
    let line = sync_status_line(&outcome);
    assert_eq!(
        line,
        "Synced: 40 new, 0 existing, \
         2 mailbox(es) stopped at the fetch deadline (resuming next sync)"
    );
    assert!(!line.contains(NON_CONVERGING_MARKER));
    assert!(!line.contains(FAILED_OPS_MARKER));
}

/// The failure line, which today's `tui::bg` builds in the `Err` arm. The
/// account is always named: a pure function cannot count a client's accounts.
#[test]
fn the_status_line_of_a_failed_tick_names_the_account_and_the_error() {
    assert_eq!(
        sync_status_line(&failed()),
        "Fetch failed (alpha): login refused: AUTHENTICATIONFAILED"
    );
}

/// A failed tick reports the failure and nothing else: counts from a pass that
/// did not finish would read as a sync that happened.
#[test]
fn the_status_line_of_a_failed_tick_carries_no_counts() {
    let outcome = from_sync_result(
        ACCOUNT,
        &busy_result(),
        2,
        Some("connection reset".to_string()),
    );
    let line = sync_status_line(&outcome);
    assert_eq!(line, "Fetch failed (alpha): connection reset");
    assert!(!line.contains(SYNCED_WORD));
}

/// The invariant that ties the payload to the line: a client that renders the
/// line and re-reads today's two markers agrees with the severity the daemon
/// decided. This is what stops a warning tick from being presented as clean.
#[test]
fn a_warning_is_exactly_a_line_carrying_one_of_the_two_markers() {
    let mut checked = 0;
    for non_converging in [Vec::new(), vec!["INBOX".to_string()]] {
        for failed_mutations in [0_u64, 3] {
            for bodies_truncated in [0_usize, 4] {
                for prunes_deferred in [0_usize, 5] {
                    let result = SyncResult {
                        saved: 1,
                        non_converging: non_converging.clone(),
                        bodies_truncated,
                        prunes_deferred,
                        ..SyncResult::default()
                    };
                    let outcome = from_sync_result(ACCOUNT, &result, failed_mutations, None);
                    let line = sync_status_line(&outcome);
                    let marked =
                        line.contains(NON_CONVERGING_MARKER) || line.contains(FAILED_OPS_MARKER);
                    assert_eq!(
                        marked,
                        outcome.severity == Severity::Warning,
                        "the rendered line and the severity disagree for {outcome:?}: {line}"
                    );
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 16, "the matrix is the whole matrix");
}

// ---------------------------------------------------------------------------
// Layer (a) - `mp sync`'s lines, derived
// ---------------------------------------------------------------------------

/// A pass that ingested everything it saw prints one line, in `mp sync`'s
/// wording for a pass with nothing already present.
#[test]
fn the_cli_lines_of_a_clean_tick_are_one_synced_line() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            saved: 3,
            ..SyncResult::default()
        },
        0,
        None,
    );
    assert_eq!(
        sync_cli_lines(&outcome),
        vec!["✓ Synced: 3 email(s) ingested".to_string()]
    );
}

/// A pass that skipped rows takes `mp sync`'s other summary wording.
#[test]
fn the_cli_lines_say_already_present_when_the_pass_skipped_rows() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            saved: 3,
            skipped: 12,
            ..SyncResult::default()
        },
        0,
        None,
    );
    assert_eq!(
        sync_cli_lines(&outcome),
        vec!["✓ Synced: 3 new, 12 already present".to_string()]
    );
}

/// Every line `sync_one_account` prints, in its order, one per state the tick
/// reached, glyph included and uncoloured.
#[test]
fn the_cli_lines_are_todays_lines_in_todays_order() {
    assert_eq!(
        sync_cli_lines(&busy()),
        vec![
            "✓ Synced: 3 new, 2 already present".to_string(),
            "ℹ Status updated on 4 message(s)".to_string(),
            "ℹ Rebound 7 message(s) to new UIDs after a UIDVALIDITY reset".to_string(),
            "ℹ 5 message(s) left their mailbox on the server".to_string(),
            "⚠ 6 removal(s) held back: this pass did not see every message, \
             run a full sync to apply them"
                .to_string(),
            "ℹ 8 mailbox(es) stopped at the fetch deadline (resuming next sync)".to_string(),
            "⚠ 'INBOX' downloaded the same messages again: the fetch is not converging, \
             see the log and docs/tickets/0115-warn-on-a-non-converging-fetch.md"
                .to_string(),
            "⚠ 'Sent' downloaded the same messages again: the fetch is not converging, \
             see the log and docs/tickets/0115-warn-on-a-non-converging-fetch.md"
                .to_string(),
            "⚠ 2 mutation(s) failed and were rolled back (see the log)".to_string(),
        ]
    );
}

/// One line per non-converging mailbox, each named once, in the payload's
/// canonical order (#0115).
#[test]
fn the_cli_lines_name_each_non_converging_mailbox_once() {
    let outcome = from_sync_result(
        ACCOUNT,
        &SyncResult {
            saved: 1,
            non_converging: vec![
                "Sent".to_string(),
                "Archive".to_string(),
                "Sent".to_string(),
            ],
            ..SyncResult::default()
        },
        0,
        None,
    );
    let lines = sync_cli_lines(&outcome);
    assert_eq!(lines.len(), 3, "one summary and two mailboxes: {lines:?}");
    assert!(lines[1].contains("'Archive'"));
    assert!(lines[2].contains("'Sent'"));
    assert_eq!(
        lines.iter().filter(|line| line.contains("'Sent'")).count(),
        1,
        "a mailbox that repeated twice is still one line: {lines:?}"
    );
}

/// A failed tick is the one line `mp sync`'s account loop prints on stderr, and
/// no summary of a pass that never finished.
#[test]
fn the_cli_lines_of_a_failed_tick_are_one_failure_line() {
    assert_eq!(
        sync_cli_lines(&failed()),
        vec!["✗ alpha: login refused: AUTHENTICATIONFAILED".to_string()]
    );
}

// ---------------------------------------------------------------------------
// Layer (a) - the change, the event, and the queue
// ---------------------------------------------------------------------------

/// The change is addressed to its account, travels as the plan's kind, and
/// carries the payload verbatim.
#[test]
fn the_change_carries_the_account_the_kind_and_the_payload() {
    let outcome = busy();
    let change = Change::SyncCompleted(outcome.clone());
    assert_eq!(change.account(), ACCOUNT);
    assert_eq!(change.kind(), KIND_SYNC_COMPLETED);
    assert_eq!(
        change.payload(),
        serde_json::to_value(&outcome).expect("a payload serialises"),
        "the event's payload is the typed outcome and nothing else"
    );
}

/// The bridge from the daemon's vocabulary to the wire's: an outcome is a
/// command outcome, so it names no resource and merges with nothing.
#[test]
fn the_outcome_travels_as_a_non_coalescing_event_of_its_own_kind() {
    let outcome = busy();
    let event = sync_event(&outcome);
    assert_eq!(event.kind(), KIND_SYNC_COMPLETED);
    assert_eq!(
        event.payload(),
        serde_json::to_value(&outcome).expect("a payload serialises")
    );
    assert_eq!(
        event.resource(),
        None,
        "an outcome addresses no resource: it is a fact about a tick, not a resource's state"
    );
    assert!(
        event.is_lifecycle(),
        "a command outcome is preserved rather than coalesced or discarded"
    );
}

/// Two ticks are two events, even for one account and even back to back. This
/// is the coalescing question the plan leaves open, decided here: merging the
/// earlier outcome away would present a warning tick as clean.
#[test]
fn two_outcomes_for_one_account_stay_two_events() {
    let warning = busy();
    let ok = clean();
    let mut queue = Outbound::new(Subscriber::default());
    assert_eq!(queue.push(Revision(2), sync_event(&warning)), Push::Queued);
    assert_eq!(
        queue.push(Revision(3), sync_event(&ok)),
        Push::Queued,
        "a second outcome is queued, never coalesced into the first"
    );
    assert_eq!(queue.len(), 2);

    let drained = drain(&mut queue);
    let payloads: Vec<Value> = drained
        .iter()
        .map(|item| match item {
            Outgoing::Event(_, event) => event.payload(),
            Outgoing::ResyncRequired { .. } => panic!("nothing overflowed: {drained:?}"),
        })
        .collect();
    assert_eq!(
        payloads,
        vec![
            serde_json::to_value(&warning).expect("serialise"),
            serde_json::to_value(&ok).expect("serialise"),
        ],
        "the warning arrives before the clean tick and is not replaced by it"
    );
}

/// A queued outcome survives the discard an overflow performs, because a
/// re-bootstrap brings no snapshot that carries it back.
#[test]
fn a_queued_outcome_outlives_a_queue_overflow() {
    let outcome = busy();
    let mut queue = Outbound::new(Subscriber {
        max_events: 1,
        max_bytes: 1 << 20,
    });
    assert_eq!(queue.push(Revision(2), sync_event(&outcome)), Push::Queued);
    assert_eq!(
        queue.push(Revision(3), counts(ACCOUNT, "inbox")),
        Push::Overflowed,
        "the second domain event does not fit and overflows the queue"
    );
    assert!(queue.is_poisoned());

    let drained = drain(&mut queue);
    match drained.first() {
        Some(Outgoing::Event(revision, event)) => {
            assert_eq!(revision.get(), 2);
            assert_eq!(event.kind(), KIND_SYNC_COMPLETED);
            assert_eq!(
                event.payload(),
                serde_json::to_value(&outcome).expect("serialise")
            );
        }
        other => panic!("the outcome must survive the discard, got {other:?}"),
    }
    assert!(
        matches!(drained.get(1), Some(Outgoing::ResyncRequired { .. })),
        "the overflow still asks for a resync: {drained:?}"
    );
    assert_eq!(drained.len(), 2);
}

/// The daemon's commit path: `apply` stamps a fresh revision, queues the event
/// for an attached connection, and reduces nothing into the snapshot.
#[test]
fn applying_an_outcome_queues_one_event_and_leaves_the_snapshot_alone() {
    let state = fresh_state();
    let conn = ConnectionId(1);
    let mut queue = state.subscribe(conn);
    let (snapshot, bootstrap, instance) = state.bootstrap(conn);
    let before = snapshot.to_json();

    let outcome = busy();
    let revision = state.apply(Change::SyncCompleted(outcome.clone()));
    assert!(
        revision.get() > bootstrap.get(),
        "a committed outcome is above the revision the bootstrap reported"
    );

    let drained = queue.drain_all();
    assert_eq!(drained.len(), 1, "one commit is one event: {drained:?}");
    let (queued, event) = &drained[0];
    assert_eq!(queued.get(), revision.get());
    assert_eq!(event.kind(), KIND_SYNC_COMPLETED);
    assert_eq!(
        event.payload(),
        serde_json::to_value(&outcome).expect("serialise")
    );

    let mut tracker = StateTracker::new(bootstrap.get(), instance.as_str());
    assert_eq!(
        tracker.observe(queued.get(), instance.as_str()),
        Observe::Apply,
        "the outcome lands in order, so no client bootstraps again for it"
    );

    // A capture on a connection that never subscribed: it registers nothing and
    // only reads the daemon's own truth.
    let (after, _, _) = state.bootstrap(ConnectionId(99));
    assert_eq!(
        after.to_json(),
        before,
        "an outcome is a command outcome: no snapshot carries it, so none changed"
    );
}

/// Every tick gets its own revision, strictly increasing, and a client applies
/// all of them without a gap.
#[test]
fn every_tick_commits_its_own_strictly_increasing_revision() {
    let state = fresh_state();
    let conn = ConnectionId(1);
    let mut queue = state.subscribe(conn);
    let (_, bootstrap, instance) = state.bootstrap(conn);

    let outcomes = [busy(), clean(), failed()];
    let mut committed = Vec::new();
    for outcome in &outcomes {
        committed.push(state.apply(Change::SyncCompleted(outcome.clone())).get());
    }
    assert!(
        committed.windows(2).all(|pair| pair[1] > pair[0]),
        "revisions are strictly increasing: {committed:?}"
    );

    let drained = queue.drain_all();
    assert_eq!(drained.len(), outcomes.len());
    let mut tracker = StateTracker::new(bootstrap.get(), instance.as_str());
    for ((revision, event), outcome) in drained.iter().zip(outcomes.iter()) {
        assert_eq!(
            tracker.observe(revision.get(), instance.as_str()),
            Observe::Apply,
            "consecutive commits leave no gap"
        );
        assert_eq!(event.kind(), KIND_SYNC_COMPLETED);
        assert_eq!(
            event.payload(),
            serde_json::to_value(outcome).expect("serialise")
        );
    }
    assert!(!tracker.needs_bootstrap());
}

// ---------------------------------------------------------------------------
// Layer (b) helpers: over the socket, against a spawned daemon
// ---------------------------------------------------------------------------

/// The hook's name, and the re-export the other four hooks are named beside.
#[test]
fn the_fake_outcome_hook_is_one_name_in_two_places() {
    assert_eq!(
        FAKE_SYNC_OUTCOME_ENV,
        "MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME"
    );
    assert_eq!(
        FAKE_SYNC_OUTCOME_ENV, LIFECYCLE_FAKE_SYNC_OUTCOME_ENV,
        "`daemon::lifecycle` re-exports the hook it does not declare"
    );
}

/// A private `HOME`, config directory and data directory.
///
/// Dropping it kills whatever daemon `daemon.pid` names, so nothing outlives the
/// test that started it, even when the test panics half way through.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    /// One configured account, whose directory exists before any daemon runs so
    /// a runtime can take its engine lock.
    fn single_account() -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        let sandbox = Self { root };
        fs::write(
            sandbox.config_dir().join("config.toml"),
            format!(
                r#"
[[accounts]]
name = "{ACCOUNT}"
default_from = "{ACCOUNT}@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"
"#
            ),
        )
        .expect("write config.toml");
        fs::create_dir_all(sandbox.account_dir()).expect("account dir");
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

    fn account_dir(&self) -> PathBuf {
        self.data_dir().join("accounts").join(ACCOUNT)
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

    /// Spawn `mp daemon run` with the outcomes armed behind every
    /// `state.bootstrap`, killed on drop, and wait until its socket accepts.
    ///
    /// `account_runtimes` is a parameter rather than a constant because the
    /// hook is defined to do nothing without it: without a runtime there is no
    /// tick, and without a tick there is no outcome.
    async fn start_daemon(&self, outcomes: Option<&Value>, account_runtimes: bool) -> Proc {
        let mut cmd = Command::new(MP);
        cmd.env("HOME", self.home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env_remove(ACCOUNT_RUNTIMES_ENV)
            .env_remove(FAKE_SYNC_OUTCOME_ENV)
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST");
        if account_runtimes {
            cmd.env(ACCOUNT_RUNTIMES_ENV, "1");
        }
        if let Some(outcomes) = outcomes {
            cmd.env(FAKE_SYNC_OUTCOME_ENV, outcomes.to_string());
        }
        let child = cmd
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

    /// Block until the socket accepts a connection, or fail the test.
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

/// Await `fut` under [`DEADLINE`], failing the test rather than the suite's
/// patience if the daemon never answers.
async fn within<T>(label: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(DEADLINE, fut).await {
        Ok(value) => value,
        Err(_) => panic!("{label} did not finish within {DEADLINE:?}"),
    }
}

/// The client identification every test sends.
fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Gui,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Connect and complete a well-formed handshake, requiring nothing.
async fn connect_initialized(sandbox: &Sandbox) -> (Connection, InitializeResult) {
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&sandbox.socket()),
    )
    .await
    .expect("connecting to a live daemon socket succeeds");
    let hello = within(
        "Connection::initialize",
        conn.initialize(client_info(), sandbox.identity(), &[], &[]),
    )
    .await
    .expect("a compatible handshake succeeds");
    (conn, hello)
}

/// Call `state.bootstrap` and return the revision it captured at. Arms one
/// round of the fake outcomes.
async fn bootstrap_revision(conn: &mut Connection) -> u64 {
    let result = within("state.bootstrap", conn.call("state.bootstrap", json!({})))
        .await
        .expect("state.bootstrap answers an initialized connection");
    result["revision"]
        .as_u64()
        .unwrap_or_else(|| panic!("the bootstrap reports a u64 revision, got {result}"))
}

/// Read notifications until `want` `sync.completed` events have arrived,
/// checking every one of them through a client's own [`StateTracker`].
///
/// Other kinds may interleave (an account's readiness lands whenever its
/// runtime comes up) and are counted only as revisions. A readiness change
/// committed between this connection's registration and its snapshot is a
/// legitimate `Duplicate`; an outcome never is, because the hook commits it
/// after the bootstrap it answers.
async fn collect_outcomes(
    conn: &mut Connection,
    bootstrap: u64,
    instance: &str,
    want: usize,
) -> Vec<EventEnvelope> {
    let mut tracker = StateTracker::new(bootstrap, instance);
    let mut outcomes: Vec<EventEnvelope> = Vec::new();
    while outcomes.len() < want {
        let notification = within("a notification", conn.next_notification())
            .await
            .expect("the daemon delivers a notification rather than closing");
        assert_eq!(notification.jsonrpc, JSONRPC_VERSION);
        assert_ne!(
            notification.method, METHOD_STATE_RESYNC_REQUIRED,
            "nothing here fills a queue, so no client is asked to bootstrap again"
        );
        assert_eq!(
            notification.method, METHOD_STATE_EVENT,
            "the daemon sent a method no client subscribed to: {notification:?}"
        );
        let envelope: EventEnvelope = serde_json::from_value(notification.params.clone())
            .unwrap_or_else(|e| {
                panic!("the params are an event envelope: {e}; got {notification:?}")
            });
        assert_eq!(
            envelope.instance_id, instance,
            "an event names the instance that answered the bootstrap"
        );
        let observed = tracker.observe(envelope.revision, &envelope.instance_id);
        if envelope.kind == KIND_SYNC_COMPLETED {
            assert_eq!(
                observed,
                Observe::Apply,
                "an outcome committed after the bootstrap arrives in order: {envelope:?}"
            );
            outcomes.push(envelope);
        } else {
            assert!(
                matches!(observed, Observe::Apply | Observe::Duplicate),
                "an interleaved {} event is neither a gap nor another instance's: {observed:?}",
                envelope.kind
            );
        }
    }
    outcomes
}

/// Assert that no `sync.completed` arrives for `window`. Bounded by
/// construction: it waits exactly that long and never longer.
async fn assert_no_outcome_within(conn: &mut Connection, window: Duration, label: &str) {
    let deadline = Instant::now() + window;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match tokio::time::timeout(remaining, conn.next_notification()).await {
            Ok(Some(notification)) => {
                if notification.method != METHOD_STATE_EVENT {
                    continue;
                }
                let kind = notification.params["kind"].as_str().unwrap_or_default();
                assert_ne!(
                    kind, KIND_SYNC_COMPLETED,
                    "{label}: an outcome arrived: {notification:?}"
                );
            }
            // The connection closed, or the window ran out: either way nothing
            // more can arrive within it.
            Ok(None) | Err(_) => return,
        }
    }
}

/// The three outcomes the socket tests arm, in the order they must arrive: a
/// warning, a clean tick behind it, and a deadline stop that is still clean.
fn armed_outcomes() -> Value {
    json!([
        {
            "account": "ignored-by-the-hook",
            "severity": "warning",
            "saved": 3, "skipped": 2, "flags_updated": 0, "pruned": 0,
            "prunes_deferred": 0, "uid_rebound": 0, "uidvalidity_resets": 0,
            "bodies_truncated": 0, "non_converging": ["INBOX"],
            "failed_mutations": 1, "error": null
        },
        {
            "account": "ignored-by-the-hook",
            "severity": "ok",
            "saved": 0, "skipped": 7, "flags_updated": 1, "pruned": 0,
            "prunes_deferred": 0, "uid_rebound": 0, "uidvalidity_resets": 0,
            "bodies_truncated": 0, "non_converging": [],
            "failed_mutations": 0, "error": null
        },
        {
            "account": "ignored-by-the-hook",
            "severity": "ok",
            "saved": 40, "skipped": 0, "flags_updated": 0, "pruned": 0,
            "prunes_deferred": 0, "uid_rebound": 0, "uidvalidity_resets": 0,
            "bodies_truncated": 2, "non_converging": [],
            "failed_mutations": 0, "error": null
        }
    ])
}

// ---------------------------------------------------------------------------
// Layer (b) - the daemon emits it
// ---------------------------------------------------------------------------

/// The end-to-end shape: three ticks, three `sync.completed` notifications, in
/// order, each at its own revision, each carrying the typed payload.
#[tokio::test]
async fn a_client_receives_one_sync_completed_notification_per_tick() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(Some(&armed_outcomes()), true).await;
    let (mut conn, hello) = connect_initialized(&sandbox).await;
    let bootstrap = bootstrap_revision(&mut conn).await;

    let outcomes = collect_outcomes(&mut conn, bootstrap, &hello.instance_id, 3).await;

    let revisions: Vec<u64> = outcomes.iter().map(|e| e.revision).collect();
    assert!(
        revisions.windows(2).all(|pair| pair[1] > pair[0]),
        "each tick carries its own strictly increasing revision: {revisions:?}"
    );
    assert!(
        revisions[0] > bootstrap,
        "the outcomes are committed after the bootstrap that armed them"
    );

    let payloads: Vec<SyncCompleted> = outcomes
        .iter()
        .map(|e| {
            serde_json::from_value(e.payload.clone())
                .unwrap_or_else(|err| panic!("the payload is a typed outcome: {err}; got {e:?}"))
        })
        .collect();
    for payload in &payloads {
        assert_eq!(
            payload.account, ACCOUNT,
            "the hook names the configured account, not the one the JSON carried"
        );
    }
    assert_eq!(payloads[0].severity, Severity::Warning);
    assert_eq!(payloads[0].non_converging, vec!["INBOX".to_string()]);
    assert_eq!(payloads[0].failed_mutations, 1);
    assert_eq!(payloads[1].severity, Severity::Ok);
    assert_eq!(payloads[1].skipped, 7);
    assert_eq!(payloads[2].severity, Severity::Ok);
    assert_eq!(
        payloads[2].bodies_truncated, 2,
        "a deadline stop reaches the client as progress"
    );
}

/// The warning is not merged into the clean tick behind it, which is the whole
/// reason a sync outcome is not coalescible: a client that saw only the second
/// would present a non-converging fetch as a clean sync.
#[tokio::test]
async fn a_warning_outcome_is_never_merged_into_a_later_clean_one() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(Some(&armed_outcomes()), true).await;
    let (mut conn, hello) = connect_initialized(&sandbox).await;
    let bootstrap = bootstrap_revision(&mut conn).await;

    let outcomes = collect_outcomes(&mut conn, bootstrap, &hello.instance_id, 2).await;
    let severities: Vec<Value> = outcomes
        .iter()
        .map(|e| e.payload["severity"].clone())
        .collect();
    assert_eq!(
        severities,
        vec![json!("warning"), json!("ok")],
        "both ticks arrive, oldest first: {outcomes:?}"
    );
}

/// What reaches the client is data. A daemon that put a rendered line in the
/// payload would pass every other socket assertion here.
#[tokio::test]
async fn the_notification_payload_is_typed_data_and_not_a_rendered_line() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(Some(&armed_outcomes()), true).await;
    let (mut conn, hello) = connect_initialized(&sandbox).await;
    let bootstrap = bootstrap_revision(&mut conn).await;

    let outcomes = collect_outcomes(&mut conn, bootstrap, &hello.instance_id, 1).await;
    let payload = &outcomes[0].payload;
    assert_eq!(outcomes[0].kind, KIND_SYNC_COMPLETED);
    assert_eq!(
        sorted_keys(payload),
        PAYLOAD_KEYS.to_vec(),
        "the wire payload is the plan's thirteen fields"
    );

    let mut strings = Vec::new();
    string_values(payload, &mut strings);
    for value in &strings {
        for fragment in FORMATTED_FRAGMENTS {
            assert!(
                !value.contains(fragment),
                "the wire payload carries the formatted fragment {fragment:?}: {payload}"
            );
        }
    }

    // And the two surfaces are derivable from exactly what arrived.
    let typed: SyncCompleted =
        serde_json::from_value(payload.clone()).expect("the payload is a typed outcome");
    assert_eq!(
        sync_status_line(&typed),
        "Synced: 3 new, 2 existing, fetch not converging on INBOX (see the log)\
         ; 1 mutation(s) failed and were rolled back (see the log)"
    );
    assert_eq!(
        sync_cli_lines(&typed),
        vec![
            "✓ Synced: 3 new, 2 already present".to_string(),
            "⚠ 'INBOX' downloaded the same messages again: the fetch is not converging, \
             see the log and docs/tickets/0115-warn-on-a-non-converging-fetch.md"
                .to_string(),
            "⚠ 1 mutation(s) failed and were rolled back (see the log)".to_string(),
        ]
    );
}

/// No runtime, no tick, no outcome: the hook is armed but the account-runtimes
/// opt-in is absent, so the daemon has nothing to report on.
#[tokio::test]
async fn no_outcome_is_emitted_without_the_account_runtimes_opt_in() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(Some(&armed_outcomes()), false).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;
    bootstrap_revision(&mut conn).await;

    assert_no_outcome_within(
        &mut conn,
        Duration::from_millis(1_500),
        "without the account-runtimes opt-in nothing ticks",
    )
    .await;
}

/// The hook is opt-in on both sides: runtimes without armed outcomes emit
/// nothing either, so no test elsewhere in the suite starts seeing events it
/// never asked for.
#[tokio::test]
async fn no_outcome_is_emitted_without_the_hook() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(None, true).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;
    bootstrap_revision(&mut conn).await;

    assert_no_outcome_within(
        &mut conn,
        Duration::from_millis(1_500),
        "an unset hook commits nothing",
    )
    .await;
}
