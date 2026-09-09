//! Canonical state, revisions and `state.bootstrap` (#0121, unit P3a-U3).
//!
//! This file is a **contract test**: it is written before
//! `src/daemon/state/` and `mp_client::StateTracker` exist, against the
//! contract fixed in `.agents/workflow/native-gui-daemon/plan.md` section 3.3
//! (unit P3a-U3), the two bootstrap rules recorded in
//! `docs/plans/daemon-bootstrap.md`, and the event prose in
//! `docs/daemon-protocol.md`. It does not compile under `--features daemon`
//! today, and that failure *is* the proof the contract has no stub behind it.
//! An implementer (P3a-U4) does not edit this file; they make it pass.
//!
//! # The two layers, and why the split
//!
//! **(a) In-process, against `mailypoppins::daemon::state`.** The five forced
//! race boundaries, the reentrant bootstrap gate, and revision monotonicity
//! under concurrency are properties of the canonical state, not of the wire.
//! Phase 3a has no `Command` method to call over a socket, so a socket test
//! could not even produce two concurrent state changes; and a race forced by
//! sleeping two client processes against each other is a flake, not a test.
//! The boundaries are therefore forced deterministically through a race hook
//! on the same thread, exactly as the P1a-U6 spike did, with no threads, no
//! barriers and no sleeps.
//!
//! **(b) Over the socket, against a spawned `mp daemon run`.** What a client
//! actually receives is the wire shape of `state.bootstrap`, the zeroed counts
//! of an `opening` account, the connection's negotiated capabilities, the
//! `not_initialized` gate, and the readiness event that converges an `opening`
//! account without a second bootstrap. Those are properties of the daemon, and
//! only a real daemon can be wrong about them.
//!
//! # Surface under test
//!
//! ```rust,ignore
//! // mailypoppins::daemon::state
//! pub const FAKE_READY_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS";
//!
//! pub struct Revision(pub u64);          // Copy + Ord + Hash + Debug
//! impl Revision { pub const ZERO: Revision; pub fn get(self) -> u64; }
//! pub struct InstanceId(pub String);     // Clone + Eq + Hash + Debug
//! impl InstanceId { pub fn new(id: impl Into<String>) -> Self; pub fn as_str(&self) -> &str; }
//! pub struct ConnectionId(pub u64);      // Copy + Eq + Hash + Debug
//!
//! pub struct MailboxSeed { pub role: String, pub slug: String, pub label: String }
//! pub struct AccountSeed { pub name: String, pub mailboxes: Vec<MailboxSeed> }
//!
//! pub enum Change {                      // Clone + Debug + PartialEq + Eq
//!     AccountReady   { account: String },
//!     AccountBlocked { account: String, reason: String },
//!     MailboxCounts  { account: String, mailbox: String, total: u64, unread: u64, badge: u64 },
//!     DraftUpsert    { account: String, id: String, subject: String, status: String, valid: bool },
//!     DraftRemoved   { account: String, id: String },
//!     OutboxCounts   { account: String, queued: u64, failed: u64 },
//! }
//!
//! pub struct Snapshot { … }
//! impl Snapshot { pub fn to_json(&self) -> serde_json::Value; }
//!
//! pub enum Boundary { BeforeRegister, AfterRegister, AfterCapture, AfterQueueStart } // Copy + Eq + Debug
//! pub type RaceHook = std::sync::Arc<dyn Fn(Boundary, &CanonicalState) + Send + Sync>;
//!
//! pub struct EventQueue { … }
//! impl EventQueue {
//!     pub fn try_recv(&mut self) -> Option<(Revision, Change)>;
//!     pub fn drain(&mut self) -> Vec<(Revision, Change)>;
//!     pub fn len(&self) -> usize;
//!     pub fn is_empty(&self) -> bool;
//! }
//!
//! pub struct CanonicalState { … }        // Send + Sync
//! impl CanonicalState {
//!     pub fn new(instance_id: InstanceId, accounts: Vec<AccountSeed>) -> Self;
//!     pub fn instance_id(&self) -> InstanceId;
//!     pub fn revision(&self) -> Revision;
//!     pub fn apply(&self, change: Change) -> Revision;
//!     pub fn subscribe(&self, conn: ConnectionId) -> EventQueue;
//!     pub fn bootstrap(&self, conn: ConnectionId) -> (Snapshot, Revision, InstanceId);
//!     pub fn set_race_hook(&self, hook: RaceHook);
//! }
//!
//! // mp_client
//! pub enum Observe { Apply, Duplicate, Gap, InstanceChanged }   // Copy + Eq + Debug
//! pub struct StateTracker { … }
//! impl StateTracker {
//!     pub fn new(bootstrap_revision: u64, instance_id: impl Into<String>) -> Self;
//!     pub fn observe(&mut self, revision: u64, instance_id: &str) -> Observe;
//!     pub fn revision(&self) -> u64;
//!     pub fn instance_id(&self) -> &str;
//!     pub fn needs_bootstrap(&self) -> bool;
//!     pub fn invalidate(&mut self);
//!     pub fn rebootstrap(&mut self, revision: u64, instance_id: impl Into<String>);
//! }
//! impl Connection {
//!     pub async fn next_notification(&mut self) -> Option<mp_protocol::Notification>;
//! }
//!
//! // over the wire
//! state.bootstrap {} -> {"instance_id":str,"revision":u64,"capabilities":[str],"snapshot":{…}}
//! ```
//!
//! # Contract points this file pins beyond the plan text
//!
//! The plan fixes the shapes and leaves the semantics to the unit that pins
//! them. These are the decisions taken here; nothing may change them without a
//! protocol-changelog entry.
//!
//! - **`subscribe` before `bootstrap`, and `bootstrap` registers.** The plan's
//!   step 1 is "register the connection as a subscriber", and
//!   `bootstrap(conn) -> (Snapshot, Revision, InstanceId)` has no room to
//!   return a queue. So a session calls `subscribe(conn)` to *create* the
//!   connection's queue endpoint and `bootstrap(conn)` to *attach* it to the
//!   fan-out, and only changes committed from that attachment onwards are
//!   queued. That is the `BeforeRegister` boundary: a change made between
//!   `subscribe` and `bootstrap` reaches the client through the snapshot alone.
//! - **The queue carries `(Revision, Change)`, not an `Event`.** The wire-level
//!   `Event` enum, its coalescing and its bounds are P3a-U5's to pin, and this
//!   unit must not pre-empt the name. A subscriber here observes the change the
//!   daemon committed and the revision it committed it at, which is the whole
//!   input the ordering rules need.
//! - **A bootstrap without a subscription is a plain capture.** `bootstrap` for
//!   a `ConnectionId` that never called `subscribe` has no queue to attach and
//!   registers nothing, so it is a legitimate way to ask the state what it
//!   currently holds. That is what [`oracle`] uses it for.
//! - **A fresh `CanonicalState` is at `Revision(1)`**, and the first `apply`
//!   returns `Revision(2)`. `docs/daemon-protocol.md` reserves `0` as the
//!   pre-bootstrap sentinel that never appears on the wire, so no bootstrap may
//!   ever report it. `Revision::ZERO` exists for the client's "nothing yet"
//!   state and for nothing else.
//! - **The race hook fires at four boundaries inside the serialized section**
//!   (`BeforeRegister`, `AfterRegister`, `AfterCapture`, `AfterQueueStart`).
//!   The fifth boundary of the P1a-U6 table, "after the response frame is
//!   written", is outside `bootstrap()` by construction, so this file forces it
//!   by mutating after `bootstrap()` returns and before the queue is drained.
//!   All five boundaries are covered; four of them need the hook.
//! - **The bootstrap gate is reentrant on its own thread**, the lesson
//!   `docs/plans/daemon-bootstrap.md` records: the hook calls `apply` on the
//!   bootstrapping thread, and a plain `Mutex` held across it deadlocks on
//!   itself. [`the_bootstrap_gate_survives_a_reentrant_mutation`] is that
//!   lesson as an executable, bounded assertion.
//! - **The watermark drops silently.** A queued event at or below the
//!   bootstrap revision is `Observe::Duplicate` and the client ignores it; the
//!   daemon does not mark it, because a correct daemon queues it in the first
//!   place and a `Duplicate` signal on the wire would be a shape carried for
//!   nothing.
//! - **A gap poisons the stream.** After `Observe::Gap` the tracker's watermark
//!   does not move and every later `observe` returns `Gap` until `rebootstrap`,
//!   which is what makes "bootstrap again" the only exit. `invalidate` is the
//!   same poisoning applied on a `state.resync_required` notification.
//! - **A changed `instance_id` wins over the revision arithmetic.** It is
//!   checked first, it is sticky, and only `rebootstrap` with the new instance
//!   clears it: a revision from a daemon the client has never bootstrapped
//!   against is meaningless, whatever its number.
//! - **Every account is `opening` at a Phase 3a bootstrap.** The daemon starts
//!   no runtimes here, so nothing has reported readiness, and the plan requires
//!   this case to exist. It is deliberately not `account.list`'s `state`, which
//!   is a read-only probe of the store on disk: `account.list` answers "can I
//!   read this account's store", the snapshot answers "has this account's
//!   runtime come up", and Phase 3a has no runtimes.
//! - **`mailboxes`, `drafts` and `outbox` carry one key per account, always**,
//!   so a client indexes by account name without a null check. A daemon with no
//!   configured account has three empty objects and an empty `accounts` array.
//! - **`sync_health` is an object with a `state` of `unknown`, `ok` or
//!   `failed`**, the three variants of `mailypoppins::sync_health::SyncHealth`.
//!   A fresh bootstrap reports `unknown`, which is that type's `Default`.
//! - **Readiness is forced by a test-only environment hook.** Phase 3a starts
//!   no runtimes, so nothing would ever flip an account to `ready` and the
//!   plan's "converges by event with no second bootstrap" case would be
//!   untestable. `MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS=<n>` flips every
//!   configured account to `ready` and commits one change per account, in
//!   `config.toml` order. **The countdown starts at the first
//!   `state.bootstrap`, not at daemon startup**, so a client that bootstraps
//!   cannot lose the race and no test needs a sleep to win it. The variable
//!   follows `MAILYPOPPINS_DAEMON_FAIL_START`'s precedent: no flag exposes it,
//!   `mp --help` never moves, and its name is `daemon::state::FAKE_READY_ENV`
//!   so the test and the daemon cannot drift apart.
//! - **The readiness event is `kind: "account.state_changed"`** with a payload
//!   of `{"account": str, "state": "ready"}`, carried in a `state.event`
//!   notification whose envelope is `mp_protocol::EventEnvelope`.
//! - **`Connection::next_notification` reads until a notification arrives**,
//!   returning a frame already buffered by an earlier `call` if there is one,
//!   and `None` when the daemon closes the connection. A receiver returning a
//!   stream was the alternative; a method keeps `Connection`'s "one borrowed
//!   reader, calls are sequential" design intact, which an owned reader task
//!   would have to undo.
//! - **`state.bootstrap` is behind the handshake gate**, like every other
//!   domain method: called first it is `not_initialized` (`-32000`), and the
//!   connection stays usable.
//!
//! # Process hygiene
//!
//! Every daemon this file starts is killed before the test returns, including
//! on panic: the child goes into a [`Proc`] whose `Drop` kills and reaps it,
//! and [`Sandbox`]'s `Drop` kills whatever `daemon.pid` names. Every wait is a
//! bounded poll or a `tokio::time::timeout`; nothing here sleeps and hopes.
//! Tests never touch the test process's environment: each passes `HOME`,
//! `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` to the child through
//! `Command::env`, so they are safe to run in parallel and under
//! `--test-threads=1` alike.
//!
//! The harness is a trimmed copy of `tests/daemon_handshake.rs`'s rather than a
//! shared `tests/common/` module: each daemon test file needs a different half
//! of it, and a shared module would have to be built into every explicit
//! `[[test]]` target.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::UnixStream;

use mp_protocol::{
    EventEnvelope, Notification, JSONRPC_VERSION, METHOD_STATE_EVENT, METHOD_STATE_RESYNC_REQUIRED,
};

use mp_client::{
    ClientError, ClientInfo, ClientKind, Connection, Identity, InitializeResult, Observe,
    StateTracker,
};

use mailypoppins::daemon::state::{
    AccountSeed, Boundary, CanonicalState, Change, ConnectionId, EventQueue, InstanceId,
    MailboxSeed, RaceHook, Revision, Snapshot, FAKE_READY_ENV,
};

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Upper bound on any single wait: the socket appearing, a frame arriving, a
/// bootstrap returning. Generous, because it is a ceiling and never a sleep.
const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

/// `not_initialized`, the gate every domain method sits behind.
const NOT_INITIALIZED: i32 = -32000;

/// How long the fake-readiness hook waits after the first `state.bootstrap`
/// before flipping the accounts. Small, because the countdown starts at the
/// bootstrap and no test can be late for it.
const FAKE_READY_MS: u64 = 50;

/// The event kind an account's readiness travels as.
const KIND_ACCOUNT_STATE_CHANGED: &str = "account.state_changed";

// ---------------------------------------------------------------------------
// Layer (a) helpers: an in-process canonical state and a client-side reducer
// ---------------------------------------------------------------------------

/// The seed every in-process test starts from: two accounts, one with two
/// mailboxes and one with one, so a per-account map cannot be faked with a
/// single list.
fn seed_accounts() -> Vec<AccountSeed> {
    vec![
        AccountSeed {
            name: "alpha".to_string(),
            mailboxes: vec![
                MailboxSeed {
                    role: "inbox".to_string(),
                    slug: "inbox".to_string(),
                    label: "Inbox".to_string(),
                },
                MailboxSeed {
                    role: "sent".to_string(),
                    slug: "sent".to_string(),
                    label: "Sent".to_string(),
                },
            ],
        },
        AccountSeed {
            name: "beta".to_string(),
            mailboxes: vec![MailboxSeed {
                role: "inbox".to_string(),
                slug: "inbox".to_string(),
                label: "Inbox".to_string(),
            }],
        },
    ]
}

/// A fresh canonical state with [`seed_accounts`] and a known instance.
fn fresh_state() -> Arc<CanonicalState> {
    Arc::new(CanonicalState::new(
        InstanceId::new("01J8Z6Q9X4V3N2M1K0H7G5F4D3"),
        seed_accounts(),
    ))
}

/// The change every boundary test forces, chosen because it touches three
/// different parts of the snapshot at once through its three sibling changes.
fn forced_change() -> Change {
    Change::MailboxCounts {
        account: "alpha".to_string(),
        mailbox: "inbox".to_string(),
        total: 42,
        unread: 7,
        badge: 7,
    }
}

/// The client's own reducer: apply one committed change to the snapshot JSON a
/// bootstrap handed over.
///
/// This is deliberately the test's code and not the daemon's. The invariant
/// under test is that snapshot-plus-accepted-events reconstructs the daemon's
/// own state, and a reducer borrowed from the daemon could reconstruct it by
/// agreeing with a bug.
fn reduce(snapshot: &mut Value, change: &Change) {
    match change {
        Change::AccountReady { account } => set_account_state(snapshot, account, "ready"),
        Change::AccountBlocked { account, .. } => set_account_state(snapshot, account, "blocked"),
        Change::MailboxCounts {
            account,
            mailbox,
            total,
            unread,
            badge,
        } => {
            let list = snapshot["mailboxes"][account]
                .as_array_mut()
                .unwrap_or_else(|| panic!("mailboxes has an array for {account}"));
            let entry = list
                .iter_mut()
                .find(|entry| entry["slug"] == json!(mailbox))
                .unwrap_or_else(|| panic!("{account} has a mailbox {mailbox}"));
            entry["total"] = json!(total);
            entry["unread"] = json!(unread);
            entry["badge"] = json!(badge);
        }
        Change::DraftUpsert {
            account,
            id,
            subject,
            status,
            valid,
        } => {
            let draft = json!({"id": id, "subject": subject, "status": status, "valid": valid});
            let list = snapshot["drafts"][account]
                .as_array_mut()
                .unwrap_or_else(|| panic!("drafts has an array for {account}"));
            match list.iter().position(|entry| entry["id"] == json!(id)) {
                Some(at) => list[at] = draft,
                None => list.push(draft),
            }
        }
        Change::DraftRemoved { account, id } => {
            let list = snapshot["drafts"][account]
                .as_array_mut()
                .unwrap_or_else(|| panic!("drafts has an array for {account}"));
            list.retain(|entry| entry["id"] != json!(id));
        }
        Change::OutboxCounts {
            account,
            queued,
            failed,
        } => {
            snapshot["outbox"][account] = json!({"queued": queued, "failed": failed});
        }
        // A sync outcome reduces nothing into the snapshot, matching the
        // daemon's own reducer in `src/daemon/state/mod.rs`.
        Change::SyncCompleted(_) => {}
    }
}

/// Set one account's `state` in a snapshot, or fail naming the account.
fn set_account_state(snapshot: &mut Value, account: &str, state: &str) {
    let accounts = snapshot["accounts"]
        .as_array_mut()
        .expect("the snapshot has an accounts array");
    let entry = accounts
        .iter_mut()
        .find(|entry| entry["name"] == json!(account))
        .unwrap_or_else(|| panic!("the snapshot lists an account {account}"));
    entry["state"] = json!(state);
}

/// Install a hook that commits `change` exactly once, at `boundary`.
///
/// The `Once` flag matters: `bootstrap` may legitimately call the hook at every
/// boundary, and a hook that fired twice at the same one would be testing the
/// hook rather than the algorithm.
fn hook_at(boundary: Boundary, change: Change) -> RaceHook {
    let fired = AtomicBool::new(false);
    let change = Mutex::new(Some(change));
    Arc::new(move |at: Boundary, state: &CanonicalState| {
        if at != boundary || fired.swap(true, Ordering::SeqCst) {
            return;
        }
        let change = change.lock().expect("hook change").take();
        if let Some(change) = change {
            state.apply(change);
        }
    })
}

/// The five boundaries of the P1a-U6 sequence, as this file drives them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Forced {
    /// Between `subscribe` and `bootstrap`: no subscriber exists yet.
    BeforeRegister,
    /// Inside the serialized section, after the subscriber is attached and
    /// before the snapshot is captured.
    AfterRegister,
    /// After the capture, before queuing starts. The boundary a naive
    /// implementation loses.
    AfterCapture,
    /// After queuing starts, before the response is assembled.
    AfterQueueStart,
    /// After `bootstrap` returned, which stands for "after the response frame
    /// was written" on the wire.
    AfterResponseWrite,
}

impl Forced {
    /// The hook boundary this one is forced at, or `None` when it is driven
    /// from outside `bootstrap` entirely.
    fn hook_boundary(self) -> Option<Boundary> {
        match self {
            Forced::BeforeRegister => Some(Boundary::BeforeRegister),
            Forced::AfterRegister => Some(Boundary::AfterRegister),
            Forced::AfterCapture => Some(Boundary::AfterCapture),
            Forced::AfterQueueStart => Some(Boundary::AfterQueueStart),
            Forced::AfterResponseWrite => None,
        }
    }

    /// How many events the client accepts, from the table in
    /// `docs/plans/daemon-bootstrap.md`.
    fn expected_deliveries(self) -> usize {
        match self {
            Forced::BeforeRegister | Forced::AfterRegister => 0,
            Forced::AfterCapture | Forced::AfterQueueStart | Forced::AfterResponseWrite => 1,
        }
    }

    fn all() -> [Forced; 5] {
        [
            Forced::BeforeRegister,
            Forced::AfterRegister,
            Forced::AfterCapture,
            Forced::AfterQueueStart,
            Forced::AfterResponseWrite,
        ]
    }
}

/// What one forced-race run produced.
struct Run {
    /// The snapshot the bootstrap returned, with every accepted event applied.
    reconstructed: Value,
    /// Every event the tracker said to apply, in arrival order.
    accepted: Vec<(u64, Change)>,
    /// Every event the watermark dropped, in arrival order.
    dropped: Vec<(u64, Change)>,
}

/// Run one bootstrap with `change` forced at `boundary`, and reduce what the
/// client received.
fn run_boundary(boundary: Forced, change: Change) -> Run {
    let state = fresh_state();
    let conn = ConnectionId(1);
    let mut queue: EventQueue = state.subscribe(conn);

    if let Some(hook) = boundary.hook_boundary() {
        state.set_race_hook(hook_at(hook, change.clone()));
    }

    let (snapshot, revision, instance) = state.bootstrap(conn);

    if boundary == Forced::AfterResponseWrite {
        state.apply(change);
    }

    let mut tracker = StateTracker::new(revision.get(), instance.as_str());
    let mut reconstructed = snapshot.to_json();
    let mut accepted = Vec::new();
    let mut dropped = Vec::new();

    for (revision, change) in queue.drain() {
        match tracker.observe(revision.get(), instance.as_str()) {
            Observe::Apply => {
                reduce(&mut reconstructed, &change);
                accepted.push((revision.get(), change));
            }
            Observe::Duplicate => dropped.push((revision.get(), change)),
            other => panic!("a queued event of a correct daemon is never {other:?}"),
        }
    }

    Run {
        reconstructed,
        accepted,
        dropped,
    }
}

/// The daemon's own truth, as JSON: a bootstrap on a connection that never
/// subscribed, so it registers nothing and only captures. Legitimate as an
/// oracle because a snapshot is by definition the whole state.
fn oracle(state: &CanonicalState) -> Value {
    state.bootstrap(ConnectionId(u64::MAX)).0.to_json()
}

// ---------------------------------------------------------------------------
// Shared shape assertions, used by both layers
// ---------------------------------------------------------------------------

/// The keys of an object, sorted, or a failure naming what was there instead.
fn keys(value: &Value, label: &str) -> Vec<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{label} is an object, got {value}"))
        .keys()
        .cloned()
        .collect()
}

/// Assert an object has exactly `expected` keys: a missing one is a hole in the
/// contract and an extra one is an unpinned field a client would come to rely
/// on.
fn assert_keys(value: &Value, expected: &[&str], label: &str) {
    let found: BTreeSet<String> = keys(value, label).into_iter().collect();
    let want: BTreeSet<String> = expected.iter().map(|key| key.to_string()).collect();
    assert_eq!(
        found, want,
        "{label} carries exactly {expected:?}, got {value}"
    );
}

/// Assert one snapshot has the documented shape, whatever produced it.
///
/// Structure only: the counts and the account states are each test's own
/// business, because a snapshot of a converged daemon has the same shape as a
/// snapshot of an opening one.
fn assert_snapshot_shape(snapshot: &Value, label: &str) {
    assert_keys(
        snapshot,
        &[
            "accounts",
            "mailboxes",
            "drafts",
            "outbox",
            "holds",
            "operations",
            "diagnostics",
        ],
        label,
    );

    let accounts = snapshot["accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("{label}: accounts is an array, got {snapshot}"));
    let mut names = Vec::new();
    for account in accounts {
        assert_keys(
            account,
            &["name", "state", "sync_health"],
            "a snapshot account",
        );
        let name = account["name"]
            .as_str()
            .unwrap_or_else(|| panic!("{label}: an account name is a string, got {account}"));
        let state = account["state"]
            .as_str()
            .unwrap_or_else(|| panic!("{label}: an account state is a string, got {account}"));
        assert!(
            matches!(state, "opening" | "ready" | "blocked"),
            "{label}: {name} is one of opening/ready/blocked, got {state:?}"
        );
        let health = &account["sync_health"];
        let health_state = health["state"].as_str().unwrap_or_else(|| {
            panic!("{label}: {name} sync_health carries a string state, got {health}")
        });
        assert!(
            matches!(health_state, "unknown" | "ok" | "failed"),
            "{label}: {name} sync_health.state is one of the three SyncHealth variants, \
             got {health_state:?}"
        );
        names.push(name.to_string());
    }

    for family in ["mailboxes", "drafts", "outbox"] {
        let map = &snapshot[family];
        let found: BTreeSet<String> = keys(map, &format!("{label}: {family}"))
            .into_iter()
            .collect();
        let want: BTreeSet<String> = names.iter().cloned().collect();
        assert_eq!(
            found, want,
            "{label}: {family} carries one key per listed account, so a client indexes it \
             without a null check; got {map}"
        );
    }

    for name in &names {
        for mailbox in snapshot["mailboxes"][name]
            .as_array()
            .unwrap_or_else(|| panic!("{label}: mailboxes[{name}] is an array"))
        {
            assert_keys(
                mailbox,
                &["role", "slug", "label", "total", "unread", "badge"],
                &format!("{label}: a mailbox of {name}"),
            );
        }
        for draft in snapshot["drafts"][name]
            .as_array()
            .unwrap_or_else(|| panic!("{label}: drafts[{name}] is an array"))
        {
            assert_keys(
                draft,
                &["id", "subject", "status", "valid"],
                &format!("{label}: a draft of {name}"),
            );
        }
        assert_keys(
            &snapshot["outbox"][name],
            &["queued", "failed"],
            &format!("{label}: the outbox of {name}"),
        );
    }

    for family in ["holds", "operations", "diagnostics"] {
        assert!(
            snapshot[family].is_array(),
            "{label}: {family} is an array, got {}",
            snapshot[family]
        );
    }
}

/// Assert every count of every listed account is zero and every draft list is
/// empty: what `App::new` presents for an account it has not opened yet.
fn assert_opening_and_zeroed(snapshot: &Value, label: &str) {
    for account in snapshot["accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("{label}: accounts is an array"))
    {
        let name = account["name"].as_str().expect("an account name");
        assert_eq!(
            account["state"],
            json!("opening"),
            "{label}: {name} is opening before any runtime reports readiness"
        );
        for mailbox in snapshot["mailboxes"][name]
            .as_array()
            .unwrap_or_else(|| panic!("{label}: mailboxes[{name}] is an array"))
        {
            for count in ["total", "unread", "badge"] {
                assert_eq!(
                    mailbox[count],
                    json!(0),
                    "{label}: {name}/{} has a zeroed {count} while opening, got {mailbox}",
                    mailbox["slug"]
                );
            }
        }
        assert_eq!(
            snapshot["drafts"][name],
            json!([]),
            "{label}: {name} has no drafts while opening"
        );
        assert_eq!(
            snapshot["outbox"][name],
            json!({"queued": 0, "failed": 0}),
            "{label}: {name} has a zeroed outbox while opening"
        );
    }
    for family in ["holds", "operations", "diagnostics"] {
        assert_eq!(
            snapshot[family],
            json!([]),
            "{label}: {family} is empty before anything has happened"
        );
    }
}

// ===========================================================================
// Layer (a) - the canonical state, in process
// ===========================================================================

#[test]
fn a_fresh_bootstrap_has_the_documented_shape_with_opening_accounts() {
    let state = fresh_state();
    let queue = state.subscribe(ConnectionId(1));
    let (snapshot, revision, instance) = state.bootstrap(ConnectionId(1));

    let json = snapshot.to_json();
    assert_snapshot_shape(&json, "an in-process bootstrap snapshot");
    assert_opening_and_zeroed(&json, "an in-process bootstrap snapshot");

    assert_eq!(
        instance,
        state.instance_id(),
        "the bootstrap echoes the state's own instance"
    );
    assert_eq!(
        revision,
        state.revision(),
        "the bootstrap reports the revision it captured at"
    );
    assert!(
        revision.get() > Revision::ZERO.get(),
        "revision 0 is the client's pre-bootstrap sentinel and never travels, got {revision:?}"
    );
    assert!(
        queue.is_empty(),
        "nothing has changed, so the subscriber's queue is empty"
    );

    let names: Vec<&str> = json["accounts"]
        .as_array()
        .expect("accounts")
        .iter()
        .map(|account| account["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(
        names,
        vec!["alpha", "beta"],
        "the snapshot lists the seeded accounts in the seeding order"
    );
}

#[test]
fn the_five_boundaries_reproduce_the_recorded_observation_table() {
    for boundary in Forced::all() {
        let run = run_boundary(boundary, forced_change());
        assert_eq!(
            run.accepted.len(),
            boundary.expected_deliveries(),
            "{boundary:?}: docs/plans/daemon-bootstrap.md records {} delivered event(s); \
             accepted {:?}, dropped {:?}",
            boundary.expected_deliveries(),
            run.accepted,
            run.dropped
        );
    }
}

#[test]
fn every_boundary_reaches_the_client_exactly_once() {
    for boundary in Forced::all() {
        let run = run_boundary(boundary, forced_change());
        let inbox = run.reconstructed["mailboxes"]["alpha"]
            .as_array()
            .expect("alpha has mailboxes")
            .iter()
            .find(|mailbox| mailbox["slug"] == json!("inbox"))
            .cloned()
            .expect("alpha has an inbox");
        assert_eq!(
            inbox["total"],
            json!(42),
            "{boundary:?}: the change reached the client, by snapshot or by event"
        );
        assert_eq!(
            inbox["unread"],
            json!(7),
            "{boundary:?}: every field of the change reached the client"
        );
        assert!(
            run.accepted.len() <= 1,
            "{boundary:?}: the change is applied once, not {} times: {:?}",
            run.accepted.len(),
            run.accepted
        );
    }
}

#[test]
fn the_after_register_boundary_is_a_snapshot_hit_and_a_watermarked_drop() {
    // The boundary that looks like a duplicate and is not: the change is in the
    // snapshot *and* an event for it exists, and the watermark collapses the
    // two into one application.
    let run = run_boundary(Forced::AfterRegister, forced_change());
    assert!(
        run.accepted.is_empty(),
        "the event is redundant with the snapshot, so nothing is applied: {:?}",
        run.accepted
    );
    assert_eq!(
        run.dropped.len(),
        1,
        "the event was queued and then dropped by the watermark, not never queued: \
         dropped {:?}",
        run.dropped
    );
}

#[test]
fn the_after_capture_boundary_is_delivered_rather_than_lost() {
    // The boundary a register-last implementation loses: the revision is above
    // R and the queue is not open yet.
    let run = run_boundary(Forced::AfterCapture, forced_change());
    assert_eq!(
        run.accepted.len(),
        1,
        "a change committed after the capture is delivered, not lost: accepted {:?}, \
         dropped {:?}",
        run.accepted,
        run.dropped
    );
}

#[test]
fn a_reconstructed_client_state_equals_a_snapshot_taken_now() {
    // The whole invariant in one assertion: snapshot plus accepted events is
    // the daemon's own state, at every boundary.
    for boundary in Forced::all() {
        let state = fresh_state();
        let conn = ConnectionId(7);
        let mut queue = state.subscribe(conn);
        if let Some(hook) = boundary.hook_boundary() {
            state.set_race_hook(hook_at(hook, forced_change()));
        }
        let (snapshot, revision, instance) = state.bootstrap(conn);
        if boundary == Forced::AfterResponseWrite {
            state.apply(forced_change());
        }
        // A second, unrelated change after the bootstrap, so the reconstruction
        // is not satisfied by a state that never moved again.
        state.apply(Change::DraftUpsert {
            account: "beta".to_string(),
            id: "d-1".to_string(),
            subject: "Angebot".to_string(),
            status: "parsed".to_string(),
            valid: true,
        });

        let mut tracker = StateTracker::new(revision.get(), instance.as_str());
        let mut reconstructed = snapshot.to_json();
        for (revision, change) in queue.drain() {
            if tracker.observe(revision.get(), instance.as_str()) == Observe::Apply {
                reduce(&mut reconstructed, &change);
            }
        }

        assert_eq!(
            reconstructed,
            oracle(&state),
            "{boundary:?}: the client's reconstruction differs from the daemon's own state"
        );
    }
}

#[test]
fn a_change_before_the_subscription_reaches_the_client_by_snapshot_alone() {
    let state = fresh_state();
    let conn = ConnectionId(3);
    // Committed before anyone subscribes: no queue can hold it.
    state.apply(forced_change());
    let mut queue = state.subscribe(conn);
    let (snapshot, _, _) = state.bootstrap(conn);

    assert!(
        queue.drain().is_empty(),
        "a change older than the subscription is not replayed"
    );
    let json = snapshot.to_json();
    let inbox = json["mailboxes"]["alpha"]
        .as_array()
        .expect("mailboxes")
        .iter()
        .find(|mailbox| mailbox["slug"] == json!("inbox"))
        .cloned()
        .expect("an inbox");
    assert_eq!(
        inbox["total"],
        json!(42),
        "the snapshot carries every change committed before it"
    );
}

#[test]
fn two_connections_bootstrapped_at_different_revisions_each_see_their_own_tail() {
    let state = fresh_state();
    let first = ConnectionId(1);
    let second = ConnectionId(2);

    let mut first_queue = state.subscribe(first);
    let (first_snapshot, first_revision, instance) = state.bootstrap(first);

    let committed = state.apply(forced_change());
    assert!(
        committed > first_revision,
        "a change after a bootstrap commits above its revision, {committed:?} vs {first_revision:?}"
    );

    let mut second_queue = state.subscribe(second);
    let (second_snapshot, second_revision, _) = state.bootstrap(second);
    assert_eq!(
        second_revision, committed,
        "the later bootstrap captures the change's own revision"
    );

    assert_eq!(
        first_queue.len(),
        1,
        "the earlier connection has exactly the one change queued"
    );
    let head = first_queue
        .try_recv()
        .expect("the earlier connection is told about the change");
    assert_eq!(head.0, committed);
    assert!(first_queue.try_recv().is_none(), "and about nothing else");
    let first_events = vec![head];
    assert!(
        second_queue.drain().is_empty(),
        "the later connection has the change in its snapshot and needs no event"
    );

    let mut tracker = StateTracker::new(first_revision.get(), instance.as_str());
    let mut reconstructed = first_snapshot.to_json();
    for (revision, change) in first_events {
        if tracker.observe(revision.get(), instance.as_str()) == Observe::Apply {
            reduce(&mut reconstructed, &change);
        }
    }
    assert_eq!(
        reconstructed,
        second_snapshot.to_json(),
        "both connections converge on the same state"
    );
}

#[test]
fn the_bootstrap_gate_survives_a_reentrant_mutation() {
    // The P1a-U6 lesson: the hook calls `apply` on the bootstrapping thread, so
    // a non-reentrant lock held across the serialized section deadlocks on
    // itself. Bounded, because the failure mode under test is a hang.
    let state = fresh_state();
    state.set_race_hook(Arc::new(|at: Boundary, state: &CanonicalState| {
        if at == Boundary::AfterRegister {
            state.apply(Change::OutboxCounts {
                account: "beta".to_string(),
                queued: 3,
                failed: 1,
            });
        }
    }));

    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = Arc::clone(&state);
    let handle = std::thread::spawn(move || {
        let conn = ConnectionId(11);
        let _queue = worker.subscribe(conn);
        let (snapshot, revision, _) = worker.bootstrap(conn);
        let _ = sender.send((snapshot.to_json(), revision));
    });

    let (snapshot, _) = receiver
        .recv_timeout(DEADLINE)
        .expect("bootstrap returns: a reentrant mutate from its own hook must not deadlock");
    handle.join().expect("the bootstrap thread finished");

    assert_eq!(
        snapshot["outbox"]["beta"],
        json!({"queued": 3, "failed": 1}),
        "a mutation committed before the capture is in the snapshot"
    );
}

#[tokio::test]
async fn revisions_strictly_increase_across_concurrent_commands_from_two_connections() {
    // Phase 3a registers no `Command` method, so the concurrency this asserts
    // cannot be produced over a socket. Two tasks against one shared state are
    // the same contention the dispatcher will put on it in Phase 4.
    const PER_TASK: u64 = 200;

    let state = fresh_state();
    let first = Arc::clone(&state);
    let second = Arc::clone(&state);

    let left = tokio::spawn(async move {
        let mut seen = Vec::new();
        for n in 0..PER_TASK {
            seen.push(
                first
                    .apply(Change::OutboxCounts {
                        account: "alpha".to_string(),
                        queued: n,
                        failed: 0,
                    })
                    .get(),
            );
            tokio::task::yield_now().await;
        }
        seen
    });
    let right = tokio::spawn(async move {
        let mut seen = Vec::new();
        for n in 0..PER_TASK {
            seen.push(
                second
                    .apply(Change::OutboxCounts {
                        account: "beta".to_string(),
                        queued: n,
                        failed: 0,
                    })
                    .get(),
            );
            tokio::task::yield_now().await;
        }
        seen
    });

    let (left, right) = within("two concurrent committers", async {
        (
            left.await.expect("left task"),
            right.await.expect("right task"),
        )
    })
    .await;

    for (label, seen) in [("the first connection", &left), ("the second", &right)] {
        assert!(
            seen.windows(2).all(|pair| pair[0] < pair[1]),
            "{label} sees its own commits in strictly increasing order: {seen:?}"
        );
    }

    let mut all: Vec<u64> = left.into_iter().chain(right).collect();
    all.sort_unstable();
    let unique: BTreeSet<u64> = all.iter().copied().collect();
    assert_eq!(
        unique.len(),
        all.len(),
        "no revision is handed to two commits"
    );
    assert_eq!(
        all.first().copied(),
        Some(2),
        "a fresh state is at revision 1, so the first commit is 2: got {:?}",
        all.first()
    );
    assert_eq!(
        all.last().copied(),
        Some(2 * PER_TASK + 1),
        "revisions are dense: {} commits from revision 2 upwards",
        2 * PER_TASK
    );
    assert_eq!(
        state.revision().get(),
        2 * PER_TASK + 1,
        "the state's revision is the last one it committed"
    );
}

#[test]
fn an_opening_account_converges_by_event_without_a_second_bootstrap() {
    let state = fresh_state();
    let conn = ConnectionId(5);
    let mut queue = state.subscribe(conn);
    let (snapshot, revision, instance) = state.bootstrap(conn);

    let mut json = snapshot.to_json();
    assert_opening_and_zeroed(&json, "the bootstrap taken before readiness");

    state.apply(Change::AccountReady {
        account: "alpha".to_string(),
    });
    state.apply(Change::MailboxCounts {
        account: "alpha".to_string(),
        mailbox: "inbox".to_string(),
        total: 12,
        unread: 3,
        badge: 3,
    });

    let mut tracker = StateTracker::new(revision.get(), instance.as_str());
    for (revision, change) in queue.drain() {
        assert_eq!(
            tracker.observe(revision.get(), instance.as_str()),
            Observe::Apply,
            "readiness arrives as an ordinary in-order event"
        );
        reduce(&mut json, &change);
    }

    assert_eq!(
        json,
        oracle(&state),
        "the client converged on the daemon's state without bootstrapping again"
    );
    assert_eq!(
        json["accounts"][0]["state"],
        json!("ready"),
        "alpha is ready in the converged view"
    );
    assert_eq!(
        json["accounts"][1]["state"],
        json!("opening"),
        "beta, which never reported, is still opening"
    );
}

#[test]
fn a_canonical_state_is_shareable_across_connection_tasks() {
    // Every connection task holds the same state, so the type has to be
    // `Send + Sync`; a compile-time assertion, kept as a test so the reason it
    // exists is written down next to it.
    fn assert_shareable<T: Send + Sync + 'static>() {}
    assert_shareable::<CanonicalState>();
    assert_shareable::<Snapshot>();
}

// ===========================================================================
// Layer (a) - the client-side tracker
// ===========================================================================

/// The instance every tracker test starts against.
const INSTANCE: &str = "01J8Z6Q9X4V3N2M1K0H7G5F4D3";

/// Another daemon process's instance.
const OTHER_INSTANCE: &str = "01K0000000000000000000000A";

#[test]
fn an_event_one_above_the_watermark_is_applied_and_advances_it() {
    let mut tracker = StateTracker::new(10, INSTANCE);
    assert_eq!(tracker.revision(), 10);
    assert_eq!(tracker.instance_id(), INSTANCE);
    assert!(!tracker.needs_bootstrap());

    assert_eq!(tracker.observe(11, INSTANCE), Observe::Apply);
    assert_eq!(tracker.revision(), 11, "the watermark follows the event");
    assert_eq!(tracker.observe(12, INSTANCE), Observe::Apply);
    assert_eq!(tracker.revision(), 12);
    assert!(!tracker.needs_bootstrap());
}

#[test]
fn a_duplicate_or_older_revision_is_ignored_and_leaves_the_watermark() {
    let mut tracker = StateTracker::new(10, INSTANCE);
    assert_eq!(
        tracker.observe(10, INSTANCE),
        Observe::Duplicate,
        "the bootstrap revision itself is already applied"
    );
    assert_eq!(
        tracker.observe(4, INSTANCE),
        Observe::Duplicate,
        "an older revision is a change the snapshot already carries"
    );
    assert_eq!(
        tracker.revision(),
        10,
        "a duplicate never moves the watermark"
    );
    assert!(
        !tracker.needs_bootstrap(),
        "a duplicate is normal at the AfterRegister boundary, not a fault"
    );

    assert_eq!(
        tracker.observe(11, INSTANCE),
        Observe::Apply,
        "the stream continues after a duplicate"
    );
}

#[test]
fn a_revision_gap_requires_a_resync_and_poisons_the_stream() {
    let mut tracker = StateTracker::new(10, INSTANCE);
    assert_eq!(
        tracker.observe(12, INSTANCE),
        Observe::Gap,
        "11 was missed, so 12 cannot be applied"
    );
    assert_eq!(tracker.revision(), 10, "a gap does not move the watermark");
    assert!(
        tracker.needs_bootstrap(),
        "the only exit from a gap is a fresh bootstrap"
    );
    assert_eq!(
        tracker.observe(11, INSTANCE),
        Observe::Gap,
        "the stream stays poisoned: even the missing revision is refused"
    );
    assert_eq!(
        tracker.observe(13, INSTANCE),
        Observe::Gap,
        "and so is every later one"
    );
    assert_eq!(tracker.revision(), 10);
}

#[test]
fn a_fresh_bootstrap_clears_a_gap_and_resumes_the_stream() {
    let mut tracker = StateTracker::new(10, INSTANCE);
    assert_eq!(tracker.observe(12, INSTANCE), Observe::Gap);

    tracker.rebootstrap(40, INSTANCE);
    assert_eq!(tracker.revision(), 40);
    assert!(!tracker.needs_bootstrap(), "the resync cleared the poison");
    assert_eq!(tracker.observe(41, INSTANCE), Observe::Apply);
    assert_eq!(
        tracker.observe(39, INSTANCE),
        Observe::Duplicate,
        "the new snapshot already carries everything below its revision"
    );
}

#[test]
fn a_changed_instance_id_forces_a_fresh_bootstrap() {
    let mut tracker = StateTracker::new(10, INSTANCE);
    assert_eq!(
        tracker.observe(11, OTHER_INSTANCE),
        Observe::InstanceChanged,
        "an in-order revision from another daemon is still meaningless"
    );
    assert!(tracker.needs_bootstrap());
    assert_eq!(
        tracker.revision(),
        10,
        "the watermark belongs to the old instance and does not move"
    );
    assert_eq!(
        tracker.instance_id(),
        INSTANCE,
        "the tracker keeps the instance it bootstrapped against until it re-bootstraps"
    );
    assert_eq!(
        tracker.observe(11, INSTANCE),
        Observe::InstanceChanged,
        "the instance change is sticky: only a bootstrap clears it"
    );

    tracker.rebootstrap(1, OTHER_INSTANCE);
    assert_eq!(tracker.instance_id(), OTHER_INSTANCE);
    assert!(!tracker.needs_bootstrap());
    assert_eq!(
        tracker.observe(2, OTHER_INSTANCE),
        Observe::Apply,
        "the new instance's stream starts from its own bootstrap revision"
    );
}

#[test]
fn a_resync_required_notification_invalidates_the_tracker() {
    let raw = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("crates/mp-protocol/fixtures/notification.resync_required.json"),
    )
    .expect("the committed resync fixture");
    let notification: Notification =
        serde_json::from_str(&raw).expect("the fixture parses as a notification");
    assert_eq!(notification.jsonrpc, JSONRPC_VERSION);
    assert_eq!(notification.method, METHOD_STATE_RESYNC_REQUIRED);
    let instance = notification.params["instance_id"]
        .as_str()
        .expect("the notification names its instance");

    let mut tracker = StateTracker::new(10, instance);
    assert_eq!(tracker.observe(11, instance), Observe::Apply);

    tracker.invalidate();
    assert!(
        tracker.needs_bootstrap(),
        "a resync_required discards the client's state"
    );
    assert_eq!(
        tracker.observe(12, instance),
        Observe::Gap,
        "no further domain event is applied until the bootstrap succeeds"
    );

    tracker.rebootstrap(90, instance);
    assert!(!tracker.needs_bootstrap());
    assert_eq!(tracker.observe(91, instance), Observe::Apply);
}

// ===========================================================================
// Layer (b) - over the socket, against a spawned daemon
// ===========================================================================

/// A private `HOME`, config directory and data directory.
///
/// Dropping it kills whatever daemon `daemon.pid` names, so nothing outlives
/// the test that started it, even when the test panics half way through.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    /// Two configured accounts, in `config.toml` order, neither with a store:
    /// Phase 3a starts no runtimes, so every account is `opening` whatever is
    /// on disk.
    fn new() -> Self {
        let sandbox = Self::bare();
        sandbox.write_config(
            r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"

[[accounts]]
name = "beta"
default_from = "beta@example.com"
"#,
        );
        sandbox
    }

    /// One configured account, for the tests whose watermark arithmetic must be
    /// exact: one account means exactly one readiness event.
    fn single_account() -> Self {
        let sandbox = Self::bare();
        sandbox.write_config(
            r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"
"#,
        );
        sandbox
    }

    /// No `config.toml` at all: a daemon serves in that state too.
    fn bare() -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        Self { root }
    }

    fn write_config(&self, body: &str) {
        fs::write(self.config_dir().join("config.toml"), body).expect("write config.toml");
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

    fn runtime_dir(&self) -> PathBuf {
        self.data_dir().join("runtime")
    }

    fn socket(&self) -> PathBuf {
        self.runtime_dir().join("daemon.sock")
    }

    fn pid_file(&self) -> PathBuf {
        self.runtime_dir().join("daemon.pid")
    }

    fn instance_file(&self) -> PathBuf {
        self.runtime_dir().join("daemon.json")
    }

    fn identity(&self) -> Identity {
        Identity {
            data_dir: self.data_dir(),
            config_dir: self.config_dir(),
        }
    }

    /// Spawn `mp daemon run`, killed on drop, and wait until its socket accepts
    /// a connection.
    async fn start_daemon(&self) -> Proc {
        self.start_daemon_with(&[]).await
    }

    /// Spawn `mp daemon run` with extra environment, killed on drop.
    async fn start_daemon_with(&self, extra: &[(&str, String)]) -> Proc {
        let mut command = Command::new(MP);
        command
            .env("HOME", self.home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env_remove("MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES")
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START")
            .env_remove(FAKE_READY_ENV);
        for (key, value) in extra {
            command.env(key, value);
        }
        let child = command
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

    /// `instance_id` out of `daemon.json`, which the bootstrap must echo.
    fn recorded_instance_id(&self) -> String {
        let raw = fs::read_to_string(self.instance_file()).expect("read daemon.json");
        let meta: Value = serde_json::from_str(&raw).expect("daemon.json is JSON");
        meta["instance_id"]
            .as_str()
            .unwrap_or_else(|| panic!("daemon.json has a string instance_id, got {raw}"))
            .to_string()
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
    connect_requiring(sandbox, &[]).await
}

/// Connect and handshake, requiring `required`.
async fn connect_requiring(sandbox: &Sandbox, required: &[&str]) -> (Connection, InitializeResult) {
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&sandbox.socket()),
    )
    .await
    .expect("connecting to a live daemon socket succeeds");
    let hello = within(
        "Connection::initialize",
        conn.initialize(client_info(), sandbox.identity(), required, &[]),
    )
    .await
    .expect("a compatible handshake succeeds");
    (conn, hello)
}

/// Call `state.bootstrap` with the documented empty params.
async fn bootstrap(conn: &mut Connection) -> Value {
    within("state.bootstrap", conn.call("state.bootstrap", json!({})))
        .await
        .expect("state.bootstrap answers an initialized connection")
}

/// The next `state.event` envelope, or a failure naming what came instead.
async fn next_event(conn: &mut Connection) -> EventEnvelope {
    let notification = within("state.event", conn.next_notification())
        .await
        .expect("the daemon delivers a notification rather than closing");
    assert_eq!(
        notification.jsonrpc, JSONRPC_VERSION,
        "a notification carries the JSON-RPC version"
    );
    assert_eq!(
        notification.method, METHOD_STATE_EVENT,
        "expected a state.event, got {notification:?}"
    );
    serde_json::from_value(notification.params.clone())
        .unwrap_or_else(|e| panic!("the params are an event envelope: {e}; got {notification:?}"))
}

#[tokio::test]
async fn bootstrap_returns_the_documented_shape() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;
    let (mut conn, hello) = connect_initialized(&sandbox).await;

    let result = bootstrap(&mut conn).await;
    assert_keys(
        &result,
        &["instance_id", "revision", "capabilities", "snapshot"],
        "the state.bootstrap result",
    );

    assert_eq!(
        result["instance_id"].as_str(),
        Some(hello.instance_id.as_str()),
        "the bootstrap names the instance the handshake named"
    );
    assert_eq!(
        result["instance_id"].as_str(),
        Some(sandbox.recorded_instance_id().as_str()),
        "and the one daemon.json records"
    );

    let revision = result["revision"]
        .as_u64()
        .unwrap_or_else(|| panic!("revision is a u64, got {result}"));
    assert!(
        revision > 0,
        "revision 0 is the client's pre-bootstrap sentinel and never travels, got {revision}"
    );

    for capability in result["capabilities"]
        .as_array()
        .unwrap_or_else(|| panic!("capabilities is an array, got {result}"))
    {
        assert!(
            capability.is_string(),
            "a capability identifier is a string, got {capability}"
        );
    }

    assert_snapshot_shape(&result["snapshot"], "the state.bootstrap snapshot");
}

#[tokio::test]
async fn an_opening_account_has_zero_counts_and_no_drafts() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;
    let (mut conn, _) = connect_initialized(&sandbox).await;

    let result = bootstrap(&mut conn).await;
    let snapshot = &result["snapshot"];
    assert_opening_and_zeroed(snapshot, "a Phase 3a bootstrap");

    let names: Vec<&str> = snapshot["accounts"]
        .as_array()
        .expect("accounts")
        .iter()
        .map(|account| account["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(
        names,
        vec!["alpha", "beta"],
        "the snapshot lists every configured account in config.toml's order, got {result}"
    );
}

#[tokio::test]
async fn a_daemon_with_no_configuration_bootstraps_with_empty_collections() {
    let sandbox = Sandbox::bare();
    let _daemon = sandbox.start_daemon().await;
    let (mut conn, _) = connect_initialized(&sandbox).await;

    let result = bootstrap(&mut conn).await;
    let snapshot = &result["snapshot"];
    assert_snapshot_shape(snapshot, "a zero-account bootstrap");
    assert_eq!(snapshot["accounts"], json!([]), "no account is configured");
    for family in ["mailboxes", "drafts", "outbox"] {
        assert_eq!(
            snapshot[family],
            json!({}),
            "{family} has no account key to carry"
        );
    }
}

#[tokio::test]
async fn bootstrap_carries_the_connections_negotiated_capabilities() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    let (_probe, hello) = connect_initialized(&sandbox).await;
    let offered = hello
        .capabilities
        .first()
        .cloned()
        .expect("the daemon offers at least one capability");

    let (mut conn, _) = connect_requiring(&sandbox, &[offered.as_str()]).await;
    let result = bootstrap(&mut conn).await;
    assert_eq!(
        result["capabilities"],
        json!([offered]),
        "the bootstrap reports what this connection agreed on, not the daemon's whole list"
    );
}

#[tokio::test]
async fn bootstrap_before_initialize_is_refused_and_leaves_the_connection_usable() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&sandbox.socket()),
    )
    .await
    .expect("connecting succeeds");

    let error = within(
        "state.bootstrap before initialize",
        conn.call("state.bootstrap", json!({})),
    )
    .await
    .expect_err("an uninitialized connection may not bootstrap");
    match error {
        ClientError::Rpc(error) => assert_eq!(
            error.code, NOT_INITIALIZED,
            "state.bootstrap is behind the handshake gate, got {error:?}"
        ),
        other => panic!("expected a typed RPC error, got {other:?}"),
    }

    within(
        "the initialize that should have come first",
        conn.initialize(client_info(), sandbox.identity(), &[], &[]),
    )
    .await
    .expect("the refusal is per request, so the connection stays usable");
    let result = bootstrap(&mut conn).await;
    assert!(result["snapshot"].is_object());
}

#[tokio::test]
async fn an_opening_account_converges_by_event_with_no_second_bootstrap() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox
        .start_daemon_with(&[(FAKE_READY_ENV, FAKE_READY_MS.to_string())])
        .await;
    let (mut conn, _) = connect_initialized(&sandbox).await;

    // The one and only bootstrap of this test. The readiness countdown starts
    // here, so nothing below is a race.
    let result = bootstrap(&mut conn).await;
    let instance = result["instance_id"]
        .as_str()
        .expect("an instance")
        .to_string();
    let revision = result["revision"].as_u64().expect("a revision");
    let mut snapshot = result["snapshot"].clone();
    assert_opening_and_zeroed(&snapshot, "the bootstrap taken before readiness");

    let mut tracker = StateTracker::new(revision, instance.clone());
    let event = next_event(&mut conn).await;

    assert_eq!(
        event.instance_id, instance,
        "the event comes from the instance that answered the bootstrap"
    );
    assert_eq!(
        tracker.observe(event.revision, &event.instance_id),
        Observe::Apply,
        "readiness is an ordinary in-order event, at revision {} above {revision}",
        event.revision
    );
    assert_eq!(
        event.revision,
        revision + 1,
        "the first event after a bootstrap is the next revision"
    );
    assert_eq!(
        event.kind, KIND_ACCOUNT_STATE_CHANGED,
        "readiness travels as an account state change, got {event:?}"
    );
    assert_eq!(
        event.payload["account"],
        json!("alpha"),
        "the payload names the account, got {event:?}"
    );
    assert_eq!(
        event.payload["state"],
        json!("ready"),
        "the payload names the state it moved to, got {event:?}"
    );

    set_account_state(&mut snapshot, "alpha", "ready");
    assert_eq!(
        snapshot["accounts"][0]["state"],
        json!("ready"),
        "the client converged with one bootstrap and one event"
    );
    assert!(
        !tracker.needs_bootstrap(),
        "nothing in that sequence asked the client to bootstrap again"
    );
}

#[tokio::test]
async fn two_connections_bootstrap_the_same_instance_and_a_consistent_revision() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    let (mut first, _) = connect_initialized(&sandbox).await;
    let (mut second, _) = connect_initialized(&sandbox).await;

    let earlier = bootstrap(&mut first).await;
    let later = bootstrap(&mut second).await;

    assert_eq!(
        earlier["instance_id"], later["instance_id"],
        "one daemon process is one instance"
    );
    let earlier_revision = earlier["revision"].as_u64().expect("a revision");
    let later_revision = later["revision"].as_u64().expect("a revision");
    assert!(
        later_revision >= earlier_revision,
        "revisions never go backwards within an instance: {later_revision} after \
         {earlier_revision}"
    );
    assert_eq!(
        earlier["snapshot"], later["snapshot"],
        "nothing changed between the two, so both see the same state"
    );
}

#[tokio::test]
async fn a_restarted_daemon_carries_a_new_instance_and_forces_a_fresh_bootstrap() {
    let sandbox = Sandbox::new();

    let first_instance = {
        let _daemon = sandbox.start_daemon().await;
        let (mut conn, _) = connect_initialized(&sandbox).await;
        let result = bootstrap(&mut conn).await;
        result["instance_id"]
            .as_str()
            .expect("an instance")
            .to_string()
    };
    // The daemon is killed here, by `Proc::drop`.

    let _daemon = sandbox.start_daemon().await;
    let (mut conn, _) = connect_initialized(&sandbox).await;
    let result = bootstrap(&mut conn).await;
    let second_instance = result["instance_id"].as_str().expect("an instance");
    let second_revision = result["revision"].as_u64().expect("a revision");

    assert_ne!(
        first_instance, second_instance,
        "a new daemon process is a new instance, so a client cannot reuse its watermark"
    );

    // A client still holding the first instance's watermark refuses everything
    // the second instance says until it bootstraps against it.
    let mut stale = StateTracker::new(second_revision, first_instance.as_str());
    assert_eq!(
        stale.observe(second_revision + 1, second_instance),
        Observe::InstanceChanged,
        "an event from an unfamiliar instance is not applied, whatever its revision"
    );
    assert!(stale.needs_bootstrap());
    stale.rebootstrap(second_revision, second_instance);
    assert_eq!(
        stale.observe(second_revision + 1, second_instance),
        Observe::Apply,
        "and the fresh bootstrap is what lets it resume"
    );
}

#[tokio::test]
async fn a_bootstrapped_connection_keeps_answering_calls_while_events_arrive() {
    // The reader must not swallow a reply while it waits for a notification,
    // nor a notification while it waits for a reply: one socket carries both.
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox
        .start_daemon_with(&[(FAKE_READY_ENV, FAKE_READY_MS.to_string())])
        .await;
    let (mut conn, _) = connect_initialized(&sandbox).await;

    let first = bootstrap(&mut conn).await;
    let event = next_event(&mut conn).await;
    assert_eq!(event.kind, KIND_ACCOUNT_STATE_CHANGED);

    let second = bootstrap(&mut conn).await;
    assert_eq!(
        second["instance_id"], first["instance_id"],
        "still the same daemon"
    );
    let earlier = first["revision"].as_u64().expect("a revision");
    let later = second["revision"].as_u64().expect("a revision");
    assert!(
        later >= event.revision && later > earlier,
        "a bootstrap taken after an event captures at least that event's revision: \
         {later} vs event {} and earlier bootstrap {earlier}",
        event.revision
    );
    assert_eq!(
        second["snapshot"]["accounts"][0]["state"],
        json!("ready"),
        "the later snapshot carries what the event announced"
    );
}

#[tokio::test]
async fn the_snapshot_of_a_converged_daemon_still_has_the_documented_shape() {
    // Readiness may not change the shape a client parses, only the values in
    // it: the same reducer has to keep working after convergence.
    let sandbox = Sandbox::new();
    let _daemon = sandbox
        .start_daemon_with(&[(FAKE_READY_ENV, FAKE_READY_MS.to_string())])
        .await;
    let (mut conn, _) = connect_initialized(&sandbox).await;

    let _first = bootstrap(&mut conn).await;
    let mut ready = BTreeMap::new();
    for _ in 0..2 {
        let event = next_event(&mut conn).await;
        assert_eq!(event.kind, KIND_ACCOUNT_STATE_CHANGED);
        let account = event.payload["account"]
            .as_str()
            .unwrap_or_else(|| panic!("the payload names an account, got {event:?}"))
            .to_string();
        ready.insert(account, event.revision);
    }
    assert_eq!(
        ready.keys().cloned().collect::<Vec<_>>(),
        vec!["alpha".to_string(), "beta".to_string()],
        "every configured account reports, once each"
    );
    let revisions: BTreeSet<u64> = ready.values().copied().collect();
    assert_eq!(
        revisions.len(),
        2,
        "two commits are two revisions, got {ready:?}"
    );

    let converged = bootstrap(&mut conn).await;
    assert_snapshot_shape(&converged["snapshot"], "a converged snapshot");
    for account in converged["snapshot"]["accounts"]
        .as_array()
        .expect("accounts")
    {
        assert_eq!(
            account["state"],
            json!("ready"),
            "every account converged, got {account}"
        );
    }
}
