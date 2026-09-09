//! Event delivery, coalescing, backpressure and resync (#0121, unit P3a-U5).
//!
//! This file is a **contract test**: it is written before
//! `src/daemon/state/events.rs` exists, against the contract fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.4 (unit P3a-U5), the
//! backpressure prose of the source plan ("coalesce equivalent invalidations by
//! resource and query scope; preserve non-coalescible command outcomes and
//! lifecycle events; when limits are exceeded, discard stale domain events,
//! enqueue `state.resync_required`, and stop sending further domain events
//! until bootstrap succeeds; if the control notification cannot be delivered,
//! close the connection; no slow client can grow daemon memory without bound or
//! delay other clients"), and the event prose in `docs/daemon-protocol.md`. It
//! does not compile under `--features daemon` today, and that failure *is* the
//! proof the contract has no stub behind it. An implementer (P3a-U6) does not
//! edit this file; they make it pass.
//!
//! # The two layers, and why the split
//!
//! **(a) In-process, against `mailypoppins::daemon::state::events`.**
//! Coalescing, the two caps, the discard, the single control message and the
//! poison that survives until a re-bootstrap are properties of one connection's
//! outbound queue. Over a socket none of them is observable as itself: what
//! reaches a client is whatever the kernel buffer and the scheduler let through,
//! so "these five invalidations became one" and "the queue never held more than
//! four mebibytes" can only be asserted where the queue lives. Layer (a) is also
//! where the byte cap is proven, because the daemon's resident memory is not a
//! stable enough number to assert against (see the note on `daemon.status`
//! below).
//!
//! **(b) Over the socket, against a spawned `mp daemon run`.** Whether a
//! stalled reader delays anybody else, whether the daemon survives a client that
//! closed its socket with a control message pending, and whether a client that
//! re-bootstraps is served again are properties of the daemon process and its
//! task layout. Only a real daemon, a real socket and a real client that stops
//! reading can be wrong about them.
//!
//! # Surface under test
//!
//! ```rust,ignore
//! // mailypoppins::daemon::state::events
//! pub const REASON_QUEUE_OVERFLOW: &str = "event_queue_overflow";
//! pub const FAKE_EVENT_BURST_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST";
//!
//! pub enum Event {                       // Clone + Debug + PartialEq
//!     Replace    { kind: &'static str, payload: serde_json::Value },
//!     Invalidate { resource: ResourceId, scope: serde_json::Value },
//!     Remove     { resource: ResourceId },
//!     Lifecycle  { kind: &'static str, payload: serde_json::Value },
//! }
//! impl Event {
//!     pub fn from_change(change: &Change) -> Event;
//!     pub fn resource(&self) -> Option<ResourceId>;
//!     pub fn kind(&self) -> &'static str;
//!     pub fn payload(&self) -> serde_json::Value;
//!     pub fn is_lifecycle(&self) -> bool;
//! }
//!
//! pub struct Subscriber { pub max_events: usize, pub max_bytes: usize }  // Copy + Debug + Default
//! impl Default for Subscriber { /* 512 events, 4 MiB */ }
//!
//! pub enum Push { Queued, Coalesced, Overflowed }   // Copy + Eq + Debug
//! pub enum Outgoing {                               // Clone + Debug + PartialEq
//!     Event(Revision, Event),
//!     ResyncRequired { reason: String },
//! }
//!
//! pub struct Outbound { … }              // Debug + Send
//! impl Outbound {
//!     pub fn new(limits: Subscriber) -> Self;
//!     pub fn push(&mut self, revision: Revision, event: Event) -> Push;
//!     pub fn pop(&mut self) -> Option<Outgoing>;
//!     pub fn len(&self) -> usize;
//!     pub fn is_empty(&self) -> bool;
//!     pub fn bytes(&self) -> usize;
//!     pub fn is_poisoned(&self) -> bool;
//!     pub fn rebootstrap(&mut self);
//! }
//! ```
//!
//! # The push algorithm, pinned
//!
//! `push(revision, event)` runs these steps in this order. Every test below is
//! one of them.
//!
//! 1. **Poisoned and domain.** If the queue is poisoned and the event is a
//!    domain event (`Replace`, `Invalidate`, `Remove`), nothing is queued and
//!    the answer is `Push::Overflowed`. Only [`Outbound::rebootstrap`] ends
//!    this state.
//! 2. **Coalesce.** If the event has a coalescing key and a queued entry shares
//!    it, that entry takes the new payload and the new revision and **moves to
//!    the tail**, so `pop` order stays revision order. The answer is
//!    `Push::Coalesced` and `len()` does not grow. The keys:
//!    `Replace` is keyed by `(kind, resource())`, `Invalidate` by
//!    `(resource, scope)`, and `Remove` and `Lifecycle` have no key at all.
//! 3. **Fit.** If one more event keeps `len()` within `max_events` and
//!    `bytes()` within `max_bytes`, the event is appended: `Push::Queued`.
//! 4. **Overflow.** Otherwise every queued *domain* event is discarded, queued
//!    lifecycle events are kept in their order, and, if the queue was not
//!    poisoned already, it is poisoned and exactly one
//!    `Outgoing::ResyncRequired { reason: REASON_QUEUE_OVERFLOW }` is appended.
//!    A `Lifecycle` event that fits after that discard is then queued
//!    (`Push::Queued`); anything else is dropped (`Push::Overflowed`).
//!
//! The caller pushes in non-decreasing revision order, which the daemon does by
//! construction because a revision is taken under the same lock that fans the
//! change out. `Outbound` does not police it and this file does not test it.
//!
//! # Contract points this file pins beyond the plan text
//!
//! - **`Event` is the wire vocabulary, `Change` is the daemon's.**
//!   [`Event::from_change`] is the one bridge, and it maps: `AccountReady` and
//!   `AccountBlocked` to `Replace { kind: "account.state_changed" }`,
//!   `DraftUpsert` to `Replace { kind: "draft.changed" }`, `MailboxCounts` to
//!   `Invalidate { resource: "mailbox:<account>/<slug>", scope: {"query":
//!   "counts"} }`, `OutboxCounts` to `Invalidate { resource:
//!   "outbox:<account>", scope: {"query": "counts"} }`, and `DraftRemoved` to
//!   `Remove { resource: "draft:<account>/<id>" }`. Counts invalidate rather
//!   than replace because that is what makes them coalescible: the scope is the
//!   identity of the query whose cached answer went stale, so a hundred count
//!   changes for one mailbox are one thing to re-read. Readiness and a draft
//!   are replaced whole because they are small and a client that re-queried
//!   them would learn nothing the payload did not already carry.
//! - **A `Replace` is addressed by `kind` plus the resource its payload names.**
//!   [`Event::resource`] derives it: `account:<account>` for
//!   `account.state_changed`, `draft:<account>/<id>` for `draft.changed`, and
//!   `None` for a kind that names no resource. Two `Replace`s of one kind that
//!   both name no resource coalesce with each other, which is the "the whole of
//!   this kind is replaced" case.
//! - **`Invalidate` and `Remove` travel as their own kinds.** An `Invalidate`
//!   is `kind: "state.invalidate"` with payload `{"resource": str, "scope":
//!   <scope>}`, a `Remove` is `kind: "state.remove"` with payload
//!   `{"resource": str}`, and a `Replace` or `Lifecycle` travels as its own
//!   `kind` and payload. That is what keeps `account.state_changed`, which
//!   `tests/daemon_bootstrap.rs` already pins, arriving unchanged.
//! - **`Remove` never coalesces, in either direction.** Nothing merges into a
//!   queued `Remove` and a `Remove` never merges into anything, including
//!   another `Remove` for the same resource: a removal is a fact about a moment,
//!   and merging two would move the earlier one past whatever came between them.
//! - **`Lifecycle` survives the poison.** A lifecycle event pushed onto a
//!   poisoned queue is queued, and one already queued when the queue overflows
//!   is kept while every domain event around it is discarded. The plan's
//!   sentence is "stop sending further **domain** events until bootstrap
//!   succeeds", and a re-bootstrap re-establishes domain state and nothing else:
//!   a shutdown warning or an operation's last word is not in any snapshot, so
//!   discarding it would lose it for good.
//! - **`bytes()` counts queued events and not the control message.** The
//!   `ResyncRequired` marker is a fixed-size daemon-internal signal rather than
//!   a payload a client made the daemon hold, and counting it would let a full
//!   queue be unable to admit the one message that empties it. `bytes()` is the
//!   encoded size of the queued events, it is `0` for a queue holding only the
//!   marker, and it never exceeds `max_bytes` after any push sequence.
//! - **`len()` counts everything `pop` will return**, the marker included, so a
//!   queue that overflowed while holding lifecycle events can be one longer than
//!   `max_events` for as long as the marker sits in it.
//! - **`rebootstrap()` clears the poison and empties the queue.** Everything
//!   queued at that moment predates the snapshot the client is about to receive,
//!   which is exactly what its watermark would drop. The daemon calls it inside
//!   the serialized section of `CanonicalState::bootstrap`, where no change can
//!   commit between the clear and the attach, so register-first is intact.
//! - **The overflow reason is `event_queue_overflow`**, which is what
//!   `crates/mp-protocol/fixtures/notification.resync_required.json` already
//!   carries; `events::REASON_QUEUE_OVERFLOW` is that string, so the fixture and
//!   the daemon cannot drift.
//! - **The event burst is forced by a test-only environment hook.** Phase 3a
//!   commits no change on its own, so nothing would ever fill a queue and the
//!   plan's slow-client cases would be untestable.
//!   `MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST=<n>` makes the daemon commit `n`
//!   `Change::MailboxCounts` changes **after every `state.bootstrap` it
//!   answers**, against the first configured account, with mailbox slugs
//!   `burst-<i>` taken from a counter that starts at `0` and never restarts for
//!   the life of the process. Distinct slugs are the point: they are distinct
//!   resources, so no two of them coalesce and the queue really fills. The
//!   changes are committed after the bootstrap's own revision is captured, so
//!   every burst revision is above the revision the bootstrap reported, and they
//!   are committed off the bootstrap's own path, so a bootstrap's latency never
//!   includes them. The slugs name mailboxes no account has, which
//!   `CanonicalState::apply` reduces to nothing while still fanning the change
//!   out: that is deliberate, so the burst cannot corrupt the snapshot a second
//!   client takes. Unset or unparseable is no burst at all, exactly as
//!   `MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS` behaves, and no flag exposes it,
//!   so `mp --help` never moves. Its name is
//!   `daemon::state::events::FAKE_EVENT_BURST_ENV` so the test and the daemon
//!   cannot drift apart. With no configured account there is nothing to commit
//!   against and the hook does nothing.
//! - **No `connections[].queued_bytes` on `daemon.status`, and no resident-set
//!   assertion.** `daemon.status` is answered in `src/daemon/server.rs` from an
//!   immutable `DaemonState` that reaches no connection's queue; publishing a
//!   per-connection byte count would mean a shared registry of live outbound
//!   queues, which is a design decision this unit has no business forcing on
//!   P3a-U6. Resident set size is not assertable either: the allocator returns
//!   nothing to the operating system on a schedule a test can pin, so a bound
//!   loose enough not to flake is too loose to catch anything. Bounded memory is
//!   therefore pinned twice by proxy:
//!   [`the_queue_never_exceeds_its_byte_cap_under_any_push_sequence`] proves the
//!   cap in process, and
//!   [`a_stalled_reader_is_told_to_resync_and_sees_no_domain_event_after_it`]
//!   proves the daemon discarded rather than buffered, by counting how few of
//!   the burst's events the stalled client ever receives.
//!
//! # Process hygiene
//!
//! Every daemon this file starts is killed before the test returns, including
//! on panic: the child goes into a [`Proc`] whose `Drop` kills and reaps it, and
//! [`Sandbox`]'s `Drop` kills whatever `daemon.pid` names. Every wait is a
//! bounded poll or a `tokio::time::timeout`; the one deliberate pause is in
//! [`a_client_that_closed_its_socket_is_dropped_and_the_daemon_keeps_serving`]
//! and is bounded and commented. Tests never touch the test process's
//! environment: each passes `HOME`, `MAILYPOPPINS_DATA_DIR` and
//! `MAILYPOPPINS_CONFIG_DIR` to the child through `Command::env`, so they are
//! safe to run in parallel and under `--test-threads=1` alike.
//!
//! The harness is a trimmed copy of `tests/daemon_bootstrap.rs`'s rather than a
//! shared `tests/common/` module: each daemon test file needs a different half
//! of it, and a shared module would have to be built into every explicit
//! `[[test]]` target.

use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::UnixStream;

use mp_protocol::{
    EventEnvelope, JSONRPC_VERSION, METHOD_STATE_EVENT, METHOD_STATE_RESYNC_REQUIRED,
};

use mp_client::{ClientInfo, ClientKind, Connection, Identity, InitializeResult};

use mailypoppins::daemon::dispatch::ResourceId;
use mailypoppins::daemon::state::events::{
    Event, Outbound, Outgoing, Push, Subscriber, FAKE_EVENT_BURST_ENV, REASON_QUEUE_OVERFLOW,
};
use mailypoppins::daemon::state::{Change, Revision};

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Upper bound on any single wait: the socket appearing, a frame arriving, a
/// round trip returning. Generous, because it is a ceiling and never a sleep.
const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

/// The kind an [`Event::Invalidate`] travels as.
const KIND_INVALIDATE: &str = "state.invalidate";

/// The kind an [`Event::Remove`] travels as.
const KIND_REMOVE: &str = "state.remove";

/// The kind a readiness or blocked change travels as, unchanged from
/// `tests/daemon_bootstrap.rs`.
const KIND_ACCOUNT_STATE_CHANGED: &str = "account.state_changed";

/// The kind a draft change travels as.
const KIND_DRAFT_CHANGED: &str = "draft.changed";

/// How many changes the burst hook commits in the slow-client tests. Far above
/// the 512-event default cap, so the queue overflows within milliseconds of the
/// bootstrap that armed it, and far above anything a socket buffer holds, so a
/// client that receives all of them proves the daemon buffered without bound.
const BURST: u64 = 20_000;

/// How many changes the burst hook commits in the test that reads them all.
/// Well under every cap, so nothing is coalesced, discarded or resynced.
const SMALL_BURST: u64 = 8;

/// The ceiling on a healthy client's round trip while another client has
/// stalled. Two orders of magnitude above a local Unix-socket round trip, so it
/// fails on a daemon that serialises clients rather than on a busy machine.
const LATENCY_BOUND: Duration = Duration::from_millis(500);

/// How many round trips the latency test samples. Odd, so the median is a
/// sample rather than a mean of two.
const LATENCY_SAMPLES: usize = 9;

// ---------------------------------------------------------------------------
// Layer (a) helpers: one connection's outbound queue, in process
// ---------------------------------------------------------------------------

/// An outbound queue with the caps a test needs.
fn outbound(max_events: usize, max_bytes: usize) -> Outbound {
    Outbound::new(Subscriber {
        max_events,
        max_bytes,
    })
}

/// An outbound queue whose event cap is what the test is about and whose byte
/// cap is out of the way.
fn events_capped(max_events: usize) -> Outbound {
    outbound(max_events, 1 << 20)
}

/// An `Invalidate` for one mailbox's counts, the shape the burst commits.
fn counts(account: &str, mailbox: &str) -> Event {
    Event::Invalidate {
        resource: ResourceId::new(format!("mailbox:{account}/{mailbox}")),
        scope: json!({"query": "counts"}),
    }
}

/// A `Remove` for one draft.
fn removed(account: &str, id: &str) -> Event {
    Event::Remove {
        resource: ResourceId::new(format!("draft:{account}/{id}")),
    }
}

/// A `Replace` of one account's state, the shape readiness travels as.
fn readiness(account: &str, state: &str) -> Event {
    Event::Replace {
        kind: KIND_ACCOUNT_STATE_CHANGED,
        payload: json!({"account": account, "state": state}),
    }
}

/// A lifecycle event, which no rule coalesces and no overflow discards.
fn lifecycle(payload: Value) -> Event {
    Event::Lifecycle {
        kind: "daemon.shutting_down",
        payload,
    }
}

/// An event whose encoded form is at least `bytes` long.
fn heavy(resource: &str, bytes: usize) -> Event {
    Event::Invalidate {
        resource: ResourceId::new(resource),
        scope: json!({"query": "counts", "blob": "x".repeat(bytes)}),
    }
}

/// Everything the queue will hand over, oldest first, leaving it empty.
fn drain(queue: &mut Outbound) -> Vec<Outgoing> {
    let mut out = Vec::new();
    while let Some(item) = queue.pop() {
        out.push(item);
    }
    out
}

/// The revisions of the queued events, in `pop` order, ignoring any control
/// message.
fn revisions(drained: &[Outgoing]) -> Vec<u64> {
    drained
        .iter()
        .filter_map(|item| match item {
            Outgoing::Event(revision, _) => Some(revision.get()),
            Outgoing::ResyncRequired { .. } => None,
        })
        .collect()
}

/// The one control message the drained items carry, or a failure naming what
/// they carry instead.
fn only_resync(drained: &[Outgoing], label: &str) -> String {
    let reasons: Vec<String> = drained
        .iter()
        .filter_map(|item| match item {
            Outgoing::ResyncRequired { reason } => Some(reason.clone()),
            Outgoing::Event(..) => None,
        })
        .collect();
    assert_eq!(
        reasons.len(),
        1,
        "{label}: an overflow asks for exactly one resync, got {drained:?}"
    );
    reasons.into_iter().next().expect("one reason")
}

/// A deterministic pseudo-random step, so the property loop is reproducible and
/// a failure is reportable as a seed rather than as "it flaked once".
fn step(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *seed >> 33
}

// ---------------------------------------------------------------------------
// Layer (a) - the subscriber's limits
// ---------------------------------------------------------------------------

/// The two numbers the plan fixes, in the type a session builds its queue from.
#[test]
fn the_default_subscriber_is_512_events_and_four_mebibytes() {
    let default = Subscriber::default();
    assert_eq!(
        default.max_events, 512,
        "the plan fixes the event cap at 512"
    );
    assert_eq!(
        default.max_bytes,
        4 * 1024 * 1024,
        "the plan fixes the byte cap at 4 MiB"
    );
}

/// A fresh queue holds nothing, weighs nothing and is not poisoned, so a
/// connection that has just bootstrapped starts from a clean state.
#[test]
fn a_fresh_outbound_is_empty_weighs_nothing_and_is_not_poisoned() {
    let mut queue = Outbound::new(Subscriber::default());
    assert_eq!(queue.len(), 0);
    assert!(queue.is_empty());
    assert_eq!(queue.bytes(), 0);
    assert!(!queue.is_poisoned());
    assert_eq!(queue.pop(), None, "nothing was pushed, so nothing pops");
}

// ---------------------------------------------------------------------------
// Layer (a) - ordering and coalescing
// ---------------------------------------------------------------------------

/// The base guarantee: what went in comes out, oldest revision first.
#[test]
fn events_pop_in_the_order_their_revisions_committed() {
    let mut queue = events_capped(16);
    assert_eq!(
        queue.push(Revision(2), readiness("alpha", "ready")),
        Push::Queued
    );
    assert_eq!(
        queue.push(Revision(3), counts("alpha", "inbox")),
        Push::Queued
    );
    assert_eq!(
        queue.push(Revision(4), removed("alpha", "d1")),
        Push::Queued
    );
    assert_eq!(queue.len(), 3, "three distinct events are three entries");

    let drained = drain(&mut queue);
    assert_eq!(revisions(&drained), vec![2, 3, 4]);
    assert!(
        matches!(drained[0], Outgoing::Event(_, Event::Replace { .. })),
        "got {drained:?}"
    );
    assert!(
        matches!(drained[1], Outgoing::Event(_, Event::Invalidate { .. })),
        "got {drained:?}"
    );
    assert!(
        matches!(drained[2], Outgoing::Event(_, Event::Remove { .. })),
        "got {drained:?}"
    );
    assert!(queue.is_empty(), "a drained queue is empty");
    assert_eq!(queue.bytes(), 0, "and weighs nothing again");
}

/// The plan's coalescing rule: N invalidations of one resource and one scope are
/// one thing to re-read, and the queued entry carries the newest revision so the
/// client's watermark lands where the daemon is.
#[test]
fn repeated_invalidations_of_one_resource_and_scope_collapse_to_the_latest() {
    let mut queue = events_capped(512);
    assert_eq!(
        queue.push(Revision(2), counts("alpha", "inbox")),
        Push::Queued,
        "the first one has nothing to merge into"
    );
    for revision in 3..=200 {
        assert_eq!(
            queue.push(Revision(revision), counts("alpha", "inbox")),
            Push::Coalesced,
            "invalidation {revision} repeats one already queued"
        );
        assert_eq!(queue.len(), 1, "coalescing never grows the queue");
    }

    let drained = drain(&mut queue);
    assert_eq!(
        revisions(&drained),
        vec![200],
        "199 invalidations became one, at the newest revision"
    );
}

/// Coalescing is keyed by resource *and* scope: two queries over one resource
/// are two cached answers, and one of them going stale says nothing about the
/// other.
#[test]
fn invalidations_of_different_scopes_or_resources_stay_separate() {
    let mut queue = events_capped(512);
    let inbox = ResourceId::new("mailbox:alpha/inbox");

    assert_eq!(
        queue.push(
            Revision(2),
            Event::Invalidate {
                resource: inbox.clone(),
                scope: json!({"query": "counts"}),
            }
        ),
        Push::Queued
    );
    assert_eq!(
        queue.push(
            Revision(3),
            Event::Invalidate {
                resource: inbox.clone(),
                scope: json!({"query": "messages", "page": 0}),
            }
        ),
        Push::Queued,
        "another scope over the same resource is another cached answer"
    );
    assert_eq!(
        queue.push(Revision(4), counts("alpha", "archive")),
        Push::Queued,
        "another resource is another entry, whatever its scope"
    );
    assert_eq!(
        queue.push(Revision(5), counts("beta", "inbox")),
        Push::Queued,
        "and an account is part of the resource, so two accounts never share one"
    );
    assert_eq!(queue.len(), 4);
    assert_eq!(revisions(&drain(&mut queue)), vec![2, 3, 4, 5]);
}

/// A merged entry takes the newest revision, so it has to take the newest
/// position too: leaving it where it was would hand the client a revision below
/// one it already applied.
#[test]
fn a_coalesced_event_moves_to_the_tail_so_pop_order_stays_revision_order() {
    let mut queue = events_capped(512);
    queue.push(Revision(2), counts("alpha", "inbox"));
    queue.push(Revision(3), counts("alpha", "archive"));
    assert_eq!(
        queue.push(Revision(4), counts("alpha", "inbox")),
        Push::Coalesced
    );

    let drained = drain(&mut queue);
    assert_eq!(
        revisions(&drained),
        vec![3, 4],
        "the merged inbox invalidation moved behind the archive one it now postdates"
    );
    assert_eq!(
        drained[1],
        Outgoing::Event(Revision(4), counts("alpha", "inbox")),
        "and it is the inbox that moved, carrying the newer revision"
    );
}

/// A small resource is replaced whole rather than patched, so a second replace
/// supersedes the first instead of queueing behind it.
#[test]
fn a_replace_supersedes_the_queued_payload_for_its_kind_and_resource() {
    let mut queue = events_capped(512);
    assert_eq!(
        queue.push(Revision(2), readiness("alpha", "opening")),
        Push::Queued
    );
    assert_eq!(
        queue.push(Revision(3), readiness("alpha", "ready")),
        Push::Coalesced,
        "one account has one state, so the newer payload is the whole truth"
    );
    assert_eq!(
        queue.push(Revision(4), readiness("beta", "ready")),
        Push::Queued,
        "another account is another resource"
    );
    assert_eq!(queue.len(), 2);

    let drained = drain(&mut queue);
    assert_eq!(
        drained[0],
        Outgoing::Event(Revision(3), readiness("alpha", "ready")),
        "the queued payload was replaced, not appended to: got {drained:?}"
    );
    assert_eq!(revisions(&drained), vec![3, 4]);
}

/// Two replaces of one kind that name no resource are two versions of the same
/// whole, so they merge; the kind still separates them from everything else.
#[test]
fn two_replaces_of_one_kind_naming_no_resource_replace_each_other() {
    let mut queue = events_capped(512);
    let diagnostics = |n: u64| Event::Replace {
        kind: "diagnostics.changed",
        payload: json!({"count": n}),
    };
    assert_eq!(queue.push(Revision(2), diagnostics(1)), Push::Queued);
    assert_eq!(queue.push(Revision(3), diagnostics(2)), Push::Coalesced);
    assert_eq!(
        queue.push(
            Revision(4),
            Event::Replace {
                kind: "holds.changed",
                payload: json!({"count": 9}),
            }
        ),
        Push::Queued,
        "a different kind is a different whole"
    );

    let drained = drain(&mut queue);
    assert_eq!(drained[0], Outgoing::Event(Revision(3), diagnostics(2)));
    assert_eq!(revisions(&drained), vec![3, 4]);
}

/// The plan's exception: a removal is a fact about a moment, and an invalidation
/// that arrives after it must not swallow it. A client that lost the removal
/// would keep a draft that no longer exists and never learn otherwise.
#[test]
fn a_remove_is_never_coalesced_away_by_a_later_invalidate() {
    let mut queue = events_capped(512);
    let draft = ResourceId::new("draft:alpha/d1");

    assert_eq!(
        queue.push(Revision(2), removed("alpha", "d1")),
        Push::Queued
    );
    assert_eq!(
        queue.push(
            Revision(3),
            Event::Invalidate {
                resource: draft.clone(),
                scope: json!({"query": "body"}),
            }
        ),
        Push::Queued,
        "an invalidation of the same resource is a second entry, not a merge"
    );
    assert_eq!(
        queue.push(
            Revision(4),
            Event::Invalidate {
                resource: draft.clone(),
                scope: json!({"query": "body"}),
            }
        ),
        Push::Coalesced,
        "the two invalidations merge with each other, and only with each other"
    );
    assert_eq!(queue.len(), 2);

    let drained = drain(&mut queue);
    assert_eq!(
        drained[0],
        Outgoing::Event(Revision(2), removed("alpha", "d1")),
        "the removal is still first and still there: got {drained:?}"
    );
    assert_eq!(revisions(&drained), vec![2, 4]);
}

/// Nor does a removal merge into another removal: the resource may have come
/// back between the two, and the queue has no way to know it did not.
#[test]
fn two_removals_of_one_resource_stay_two_entries() {
    let mut queue = events_capped(512);
    assert_eq!(
        queue.push(Revision(2), removed("alpha", "d1")),
        Push::Queued
    );
    assert_eq!(
        queue.push(Revision(3), removed("alpha", "d1")),
        Push::Queued
    );
    assert_eq!(queue.len(), 2);
    assert_eq!(revisions(&drain(&mut queue)), vec![2, 3]);
}

/// Lifecycle events are not state, so nothing about them is redundant: two of
/// one kind are two things that happened.
#[test]
fn a_lifecycle_event_never_coalesces_with_another() {
    let mut queue = events_capped(512);
    assert_eq!(
        queue.push(Revision(2), lifecycle(json!({"in_seconds": 5}))),
        Push::Queued
    );
    assert_eq!(
        queue.push(Revision(3), lifecycle(json!({"in_seconds": 4}))),
        Push::Queued
    );
    assert_eq!(queue.len(), 2, "got {:?}", drain(&mut queue));
}

// ---------------------------------------------------------------------------
// Layer (a) - the two caps, the discard and the poison
// ---------------------------------------------------------------------------

/// The event cap: one push too many discards the queued domain events and asks
/// the client to start again.
#[test]
fn exceeding_the_event_cap_discards_the_queue_and_asks_for_a_resync() {
    let mut queue = events_capped(4);
    for revision in 2..=5 {
        assert_eq!(
            queue.push(
                Revision(revision),
                counts("alpha", &format!("mb{revision}"))
            ),
            Push::Queued,
            "the queue holds four"
        );
    }
    assert_eq!(queue.len(), 4);
    assert!(!queue.is_poisoned(), "four is not five");

    assert_eq!(
        queue.push(Revision(6), counts("alpha", "mb6")),
        Push::Overflowed,
        "the fifth event is one too many"
    );
    assert!(queue.is_poisoned());
    assert_eq!(
        queue.len(),
        1,
        "every queued domain event was discarded, leaving the control message"
    );
    assert_eq!(queue.bytes(), 0, "the control message is not a payload");

    let drained = drain(&mut queue);
    assert_eq!(
        drained,
        vec![Outgoing::ResyncRequired {
            reason: REASON_QUEUE_OVERFLOW.to_string()
        }],
        "an overflowed queue hands over the control message and nothing else"
    );
    assert_eq!(
        REASON_QUEUE_OVERFLOW, "event_queue_overflow",
        "the reason the resync_required fixture carries"
    );
}

/// The byte cap: a queue well under its event cap still overflows when what it
/// holds gets too big, which is the cap that bounds a stalled client's memory.
#[test]
fn exceeding_the_byte_cap_overflows_long_before_the_event_cap() {
    let mut queue = outbound(512, 8 * 1024);
    let mut pushed = 0;
    let mut overflowed = None;
    for revision in 2..=64 {
        let event = heavy(&format!("mailbox:alpha/mb{revision}"), 1024);
        match queue.push(Revision(revision), event) {
            Push::Queued => pushed += 1,
            Push::Overflowed => {
                overflowed = Some(revision);
                break;
            }
            Push::Coalesced => panic!("distinct resources never coalesce"),
        }
        assert!(
            queue.bytes() <= 8 * 1024,
            "the queue passed its byte cap at revision {revision}: {} bytes",
            queue.bytes()
        );
    }

    let at = overflowed.expect("a kilobyte at a time fills eight kilobytes");
    assert!(
        pushed < 512,
        "the byte cap bit first, after {pushed} events at revision {at}"
    );
    assert!(queue.is_poisoned());
    assert_eq!(
        only_resync(&drain(&mut queue), "the byte cap"),
        REASON_QUEUE_OVERFLOW
    );
}

/// A single event bigger than the whole cap can never be queued, so it is
/// refused rather than admitted "just this once".
#[test]
fn an_event_larger_than_the_whole_cap_is_refused_rather_than_queued() {
    let mut queue = outbound(512, 1024);
    assert_eq!(
        queue.push(Revision(2), heavy("mailbox:alpha/inbox", 4096)),
        Push::Overflowed
    );
    assert!(queue.is_poisoned());
    assert_eq!(queue.len(), 1, "only the control message");
    assert_eq!(queue.bytes(), 0);
}

/// After the overflow the daemon stops sending domain events until the client
/// bootstraps again: everything it would have sent is already wrong.
#[test]
fn a_poisoned_queue_drops_every_further_domain_event_until_a_rebootstrap() {
    let mut queue = events_capped(2);
    queue.push(Revision(2), counts("alpha", "inbox"));
    queue.push(Revision(3), counts("alpha", "archive"));
    assert_eq!(
        queue.push(Revision(4), counts("alpha", "sent")),
        Push::Overflowed
    );

    for revision in 5..=40 {
        assert_eq!(
            queue.push(
                Revision(revision),
                counts("alpha", &format!("mb{revision}"))
            ),
            Push::Overflowed,
            "a poisoned queue takes no domain event, revision {revision}"
        );
        assert_eq!(
            queue.push(Revision(revision), removed("alpha", "d1")),
            Push::Overflowed
        );
        assert_eq!(
            queue.push(Revision(revision), readiness("alpha", "ready")),
            Push::Overflowed
        );
    }
    assert_eq!(
        queue.len(),
        1,
        "and queues nothing behind the control message"
    );

    let drained = drain(&mut queue);
    assert_eq!(
        only_resync(&drained, "a queue poisoned once"),
        REASON_QUEUE_OVERFLOW,
        "36 further overflows are still exactly one control message"
    );
    assert!(
        revisions(&drained).is_empty(),
        "and no domain event: {drained:?}"
    );
}

/// The queue is poisoned until the client's fresh snapshot supersedes it, and a
/// re-bootstrap is what clears both the poison and the stale queue.
#[test]
fn a_rebootstrap_clears_the_poison_and_empties_the_queue() {
    let mut queue = events_capped(2);
    queue.push(Revision(2), counts("alpha", "inbox"));
    queue.push(Revision(3), counts("alpha", "archive"));
    queue.push(Revision(4), counts("alpha", "sent"));
    assert!(queue.is_poisoned());

    queue.rebootstrap();
    assert!(!queue.is_poisoned(), "the client bootstrapped again");
    assert_eq!(
        queue.len(),
        0,
        "including the control message it has now acted on"
    );
    assert!(queue.is_empty());
    assert_eq!(queue.bytes(), 0);

    assert_eq!(
        queue.push(Revision(9), counts("alpha", "inbox")),
        Push::Queued,
        "and domain events flow again"
    );
    assert_eq!(revisions(&drain(&mut queue)), vec![9]);
}

/// A re-bootstrap drops queued events even when the queue never overflowed: the
/// snapshot the client is about to apply already carries every one of them.
#[test]
fn a_rebootstrap_drops_a_healthy_queue_too() {
    let mut queue = events_capped(512);
    queue.push(Revision(2), counts("alpha", "inbox"));
    queue.push(Revision(3), readiness("alpha", "ready"));
    assert!(!queue.is_poisoned());

    queue.rebootstrap();
    assert_eq!(queue.len(), 0);
    assert_eq!(queue.bytes(), 0);
}

/// Lifecycle events are not domain state, so a re-bootstrap does not recover
/// them and a poisoned queue must still carry them.
#[test]
fn a_lifecycle_event_is_queued_even_while_the_queue_is_poisoned() {
    let mut queue = events_capped(1);
    queue.push(Revision(2), counts("alpha", "inbox"));
    assert_eq!(
        queue.push(Revision(3), counts("alpha", "archive")),
        Push::Overflowed
    );
    assert!(queue.is_poisoned());

    assert_eq!(
        queue.push(Revision(4), lifecycle(json!({"in_seconds": 5}))),
        Push::Queued,
        "a shutdown warning is in no snapshot, so dropping it would lose it"
    );

    let drained = drain(&mut queue);
    assert_eq!(
        drained,
        vec![
            Outgoing::ResyncRequired {
                reason: REASON_QUEUE_OVERFLOW.to_string()
            },
            Outgoing::Event(Revision(4), lifecycle(json!({"in_seconds": 5}))),
        ],
        "the control message first, then the lifecycle event behind it"
    );
}

/// The same rule looking backwards: the discard takes the domain events and
/// leaves the lifecycle event that was already queued.
#[test]
fn a_lifecycle_event_queued_before_an_overflow_outlives_the_discard() {
    let mut queue = events_capped(4);
    assert_eq!(
        queue.push(Revision(2), lifecycle(json!({"in_seconds": 5}))),
        Push::Queued
    );
    for revision in 3..=5 {
        assert_eq!(
            queue.push(
                Revision(revision),
                counts("alpha", &format!("mb{revision}"))
            ),
            Push::Queued
        );
    }
    assert_eq!(
        queue.len(),
        4,
        "one lifecycle event and three domain events"
    );

    assert_eq!(
        queue.push(Revision(6), counts("alpha", "mb6")),
        Push::Overflowed
    );

    let drained = drain(&mut queue);
    assert_eq!(
        drained,
        vec![
            Outgoing::Event(Revision(2), lifecycle(json!({"in_seconds": 5}))),
            Outgoing::ResyncRequired {
                reason: REASON_QUEUE_OVERFLOW.to_string()
            },
        ],
        "the lifecycle event kept its place and the control message came after it"
    );
}

/// The memory bound, as a property rather than as one arithmetic example: no
/// sequence of pushes, pops and re-bootstraps puts the queue over its byte cap.
/// This is the in-process proof that stands in for a resident-set assertion the
/// socket layer cannot make.
#[test]
fn the_queue_never_exceeds_its_byte_cap_under_any_push_sequence() {
    const MAX_EVENTS: usize = 64;
    const MAX_BYTES: usize = 64 * 1024;
    let mut queue = outbound(MAX_EVENTS, MAX_BYTES);
    let mut seed = 0x5eed_1234_u64;
    let mut revision = 1_u64;

    for iteration in 0..4_000 {
        revision += 1;
        let size = (step(&mut seed) % 9_000) as usize;
        let resource = format!("mailbox:alpha/mb{}", step(&mut seed) % 24);
        let event = match step(&mut seed) % 8 {
            0..=2 => heavy(&resource, size),
            3 | 4 => Event::Replace {
                kind: KIND_ACCOUNT_STATE_CHANGED,
                payload: json!({"account": "alpha", "state": "ready", "blob": "y".repeat(size)}),
            },
            5 => Event::Remove {
                resource: ResourceId::new(resource),
            },
            _ => lifecycle(json!({"blob": "z".repeat(size)})),
        };
        queue.push(Revision(revision), event);

        assert!(
            queue.bytes() <= MAX_BYTES,
            "iteration {iteration}: {} queued bytes over the {MAX_BYTES} cap",
            queue.bytes()
        );
        assert!(
            queue.len() <= MAX_EVENTS + 1,
            "iteration {iteration}: {} entries, above the {MAX_EVENTS} cap and its control message",
            queue.len()
        );

        if step(&mut seed).is_multiple_of(5) {
            queue.pop();
        }
        if step(&mut seed).is_multiple_of(97) {
            queue.rebootstrap();
            assert_eq!(
                queue.bytes(),
                0,
                "iteration {iteration}: a re-bootstrap empties it"
            );
        }
    }
}

/// A queue holding one big event weighs at least what that event carries, which
/// is what makes the cap a bound on memory rather than on a counter.
#[test]
fn the_byte_count_is_the_size_of_what_is_queued() {
    let mut queue = outbound(512, 1 << 20);
    assert_eq!(queue.bytes(), 0);
    queue.push(Revision(2), heavy("mailbox:alpha/inbox", 32 * 1024));
    assert!(
        queue.bytes() >= 32 * 1024,
        "a 32 KiB payload weighs at least 32 KiB, got {}",
        queue.bytes()
    );
    queue.pop();
    assert_eq!(queue.bytes(), 0, "and the weight leaves with it");
}

// ---------------------------------------------------------------------------
// Layer (a) - the bridge from Change to Event, and the wire form
// ---------------------------------------------------------------------------

/// The one bridge between the daemon's vocabulary and the wire's, variant by
/// variant. A change whose mapping moved would silently change what every
/// client receives, so the whole table is pinned here.
#[test]
fn every_change_maps_onto_the_event_the_contract_names() {
    assert_eq!(
        Event::from_change(&Change::AccountReady {
            account: "alpha".to_string()
        }),
        Event::Replace {
            kind: KIND_ACCOUNT_STATE_CHANGED,
            payload: json!({"account": "alpha", "state": "ready"}),
        },
        "readiness is a small whole resource, so it is replaced"
    );
    assert_eq!(
        Event::from_change(&Change::AccountBlocked {
            account: "alpha".to_string(),
            reason: "no store".to_string(),
        }),
        Event::Replace {
            kind: KIND_ACCOUNT_STATE_CHANGED,
            payload: json!({"account": "alpha", "state": "blocked", "reason": "no store"}),
        }
    );
    assert_eq!(
        Event::from_change(&Change::MailboxCounts {
            account: "alpha".to_string(),
            mailbox: "inbox".to_string(),
            total: 12,
            unread: 3,
            badge: 3,
        }),
        Event::Invalidate {
            resource: ResourceId::new("mailbox:alpha/inbox"),
            scope: json!({"query": "counts"}),
        },
        "counts invalidate a cached query, which is what lets them coalesce"
    );
    assert_eq!(
        Event::from_change(&Change::OutboxCounts {
            account: "alpha".to_string(),
            queued: 2,
            failed: 0,
        }),
        Event::Invalidate {
            resource: ResourceId::new("outbox:alpha"),
            scope: json!({"query": "counts"}),
        }
    );
    assert_eq!(
        Event::from_change(&Change::DraftUpsert {
            account: "alpha".to_string(),
            id: "d1".to_string(),
            subject: "Re: lunch".to_string(),
            status: "draft".to_string(),
            valid: true,
        }),
        Event::Replace {
            kind: KIND_DRAFT_CHANGED,
            payload: json!({
                "account": "alpha", "id": "d1",
                "subject": "Re: lunch", "status": "draft", "valid": true,
            }),
        },
        "a draft is small enough to travel whole"
    );
    assert_eq!(
        Event::from_change(&Change::DraftRemoved {
            account: "alpha".to_string(),
            id: "d1".to_string(),
        }),
        Event::Remove {
            resource: ResourceId::new("draft:alpha/d1"),
        },
        "a removal is the one thing that must never be coalesced away"
    );
}

/// The resource an event addresses, which is half of a `Replace`'s coalescing
/// key and all of an `Invalidate`'s.
#[test]
fn an_event_names_the_resource_it_addresses() {
    assert_eq!(
        readiness("alpha", "ready").resource(),
        Some(ResourceId::new("account:alpha")),
        "a state change addresses its account"
    );
    assert_eq!(
        Event::from_change(&Change::DraftUpsert {
            account: "alpha".to_string(),
            id: "d1".to_string(),
            subject: String::new(),
            status: "draft".to_string(),
            valid: false,
        })
        .resource(),
        Some(ResourceId::new("draft:alpha/d1")),
        "a draft replace addresses the draft"
    );
    assert_eq!(
        counts("alpha", "inbox").resource(),
        Some(ResourceId::new("mailbox:alpha/inbox"))
    );
    assert_eq!(
        removed("alpha", "d1").resource(),
        Some(ResourceId::new("draft:alpha/d1"))
    );
    assert_eq!(
        lifecycle(json!({})).resource(),
        None,
        "a lifecycle event is about the daemon, not about a resource"
    );
    assert_eq!(
        Event::Replace {
            kind: "diagnostics.changed",
            payload: json!({"count": 0}),
        }
        .resource(),
        None,
        "a kind that names no resource replaces the whole of itself"
    );
}

/// What each variant becomes inside a `state.event` envelope. `Replace` keeps
/// carrying `account.state_changed` unchanged, which is what
/// `tests/daemon_bootstrap.rs` already pins over the wire.
#[test]
fn each_variant_travels_as_the_kind_and_payload_the_protocol_documents() {
    let ready = readiness("alpha", "ready");
    assert_eq!(ready.kind(), KIND_ACCOUNT_STATE_CHANGED);
    assert_eq!(
        ready.payload(),
        json!({"account": "alpha", "state": "ready"})
    );
    assert!(!ready.is_lifecycle());

    let invalidate = counts("alpha", "inbox");
    assert_eq!(invalidate.kind(), KIND_INVALIDATE);
    assert_eq!(
        invalidate.payload(),
        json!({"resource": "mailbox:alpha/inbox", "scope": {"query": "counts"}}),
        "an invalidation tells the client what to re-read and which query to re-run"
    );
    assert!(!invalidate.is_lifecycle());

    let remove = removed("alpha", "d1");
    assert_eq!(remove.kind(), KIND_REMOVE);
    assert_eq!(remove.payload(), json!({"resource": "draft:alpha/d1"}));
    assert!(!remove.is_lifecycle());

    let shutdown = lifecycle(json!({"in_seconds": 5}));
    assert_eq!(shutdown.kind(), "daemon.shutting_down");
    assert_eq!(shutdown.payload(), json!({"in_seconds": 5}));
    assert!(
        shutdown.is_lifecycle(),
        "the one predicate the overflow path branches on"
    );
}

// ===========================================================================
// Layer (b) - over the socket, against a spawned daemon
// ===========================================================================

/// A private `HOME`, config directory and data directory.
///
/// Dropping it kills whatever daemon `daemon.pid` names, so nothing outlives the
/// test that started it, even when the test panics half way through.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    /// One configured account, which is what the burst hook commits against.
    fn single_account() -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        let sandbox = Self { root };
        fs::write(
            sandbox.config_dir().join("config.toml"),
            r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"
"#,
        )
        .expect("write config.toml");
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

    fn runtime_dir(&self) -> PathBuf {
        self.data_dir().join("runtime")
    }

    fn socket(&self) -> PathBuf {
        self.runtime_dir().join("daemon.sock")
    }

    fn pid_file(&self) -> PathBuf {
        self.runtime_dir().join("daemon.pid")
    }

    fn identity(&self) -> Identity {
        Identity {
            data_dir: self.data_dir(),
            config_dir: self.config_dir(),
        }
    }

    /// Spawn `mp daemon run` with a burst of `count` changes armed behind every
    /// `state.bootstrap`, killed on drop, and wait until its socket accepts.
    async fn start_daemon_bursting(&self, count: u64) -> Proc {
        let child = Command::new(MP)
            .env("HOME", self.home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env_remove("MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES")
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS")
            .env(FAKE_EVENT_BURST_ENV, count.to_string())
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

    /// The pid `daemon.pid` records.
    fn pid(&self) -> i32 {
        let raw = fs::read_to_string(self.pid_file()).expect("read daemon.pid");
        raw.trim().parse().expect("daemon.pid holds a pid")
    }

    /// Whether that process is still alive. Signal `0` checks for existence and
    /// delivers nothing.
    fn daemon_is_alive(&self) -> bool {
        // Safety: a pid read from a pid file we own, and signal 0 delivers
        // nothing to it.
        unsafe { libc::kill(self.pid(), 0) == 0 }
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
/// burst.
async fn bootstrap_revision(conn: &mut Connection) -> u64 {
    let result = within("state.bootstrap", conn.call("state.bootstrap", json!({})))
        .await
        .expect("state.bootstrap answers an initialized connection");
    result["revision"]
        .as_u64()
        .unwrap_or_else(|| panic!("the bootstrap reports a u64 revision, got {result}"))
}

/// One server-initiated notification, classified.
#[derive(Debug)]
enum Observed {
    Event(EventEnvelope),
    Resync { instance_id: String, reason: String },
}

/// The next notification, or a failure naming what came instead.
async fn next_observed(conn: &mut Connection) -> Observed {
    let notification = within("a notification", conn.next_notification())
        .await
        .expect("the daemon delivers a notification rather than closing");
    assert_eq!(
        notification.jsonrpc, JSONRPC_VERSION,
        "a notification carries the JSON-RPC version"
    );
    match notification.method.as_str() {
        METHOD_STATE_EVENT => Observed::Event(
            serde_json::from_value(notification.params.clone()).unwrap_or_else(|e| {
                panic!("the params are an event envelope: {e}; got {notification:?}")
            }),
        ),
        METHOD_STATE_RESYNC_REQUIRED => {
            let params = &notification.params;
            let field = |name: &str| {
                params[name]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("state.resync_required carries a string {name}, got {params}")
                    })
                    .to_string()
            };
            Observed::Resync {
                instance_id: field("instance_id"),
                reason: field("reason"),
            }
        }
        other => panic!("the daemon sent {other}, which no client subscribed to"),
    }
}

/// Read this connection's events until the daemon asks for a resync, checking
/// revision order on the way, and return how many events arrived first.
async fn drain_until_resync(conn: &mut Connection, bootstrap: u64, instance: &str) -> u64 {
    let mut watermark = bootstrap;
    let mut seen = 0_u64;
    loop {
        match next_observed(conn).await {
            Observed::Event(event) => {
                assert!(
                    event.revision > watermark,
                    "events arrive in revision order: {} after {watermark}",
                    event.revision
                );
                watermark = event.revision;
                seen += 1;
                assert!(
                    seen <= BURST,
                    "the daemon delivered more than the {BURST} events it committed"
                );
            }
            Observed::Resync {
                instance_id,
                reason,
            } => {
                assert_eq!(
                    instance_id, instance,
                    "the control message names the instance that answered the bootstrap"
                );
                assert_eq!(
                    reason, REASON_QUEUE_OVERFLOW,
                    "a queue that overflowed says so"
                );
                return seen;
            }
        }
    }
}

/// Assert that nothing arrives on this connection for `window`. Bounded by
/// construction: it waits exactly that long and never longer.
async fn assert_no_event_within(conn: &mut Connection, window: Duration, label: &str) {
    if let Ok(Some(notification)) = tokio::time::timeout(window, conn.next_notification()).await {
        assert_ne!(
            notification.method, METHOD_STATE_EVENT,
            "{label}: a domain event arrived after the resync: {notification:?}"
        );
    }
}

/// The baseline: a client that reads gets every burst event, in order, distinct,
/// and is never asked to resync. Without this, a passing overflow test could be
/// a daemon that resyncs everybody.
#[tokio::test]
async fn a_reading_client_receives_the_whole_burst_in_revision_order() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon_bursting(SMALL_BURST).await;
    let (mut conn, hello) = connect_initialized(&sandbox).await;

    let bootstrap = bootstrap_revision(&mut conn).await;
    let mut watermark = bootstrap;
    for index in 0..SMALL_BURST {
        match within("a burst event", next_observed(&mut conn)).await {
            Observed::Event(event) => {
                assert_eq!(
                    event.instance_id, hello.instance_id,
                    "the event comes from the instance that answered the bootstrap"
                );
                assert_eq!(
                    event.revision,
                    watermark + 1,
                    "the burst commits one revision per change, with no gap"
                );
                watermark = event.revision;
                assert_eq!(
                    event.kind, KIND_INVALIDATE,
                    "a counts change invalidates a cached query, got {event:?}"
                );
                assert_eq!(
                    event.payload,
                    json!({
                        "resource": format!("mailbox:alpha/burst-{index}"),
                        "scope": {"query": "counts"},
                    }),
                    "the burst names distinct mailboxes so nothing coalesces"
                );
            }
            other => panic!("a client this far under every cap is never resynced: {other:?}"),
        }
    }
    assert!(
        watermark > bootstrap,
        "every burst revision is above the bootstrap's own"
    );
    assert_no_event_within(
        &mut conn,
        Duration::from_millis(200),
        "after the whole burst",
    )
    .await;
}

/// The plan's "no slow client delays another": one client bootstraps and never
/// reads while the daemon commits twenty thousand changes at it, and a second
/// client's round trips have to stay local-socket fast throughout.
#[tokio::test]
async fn a_stalled_reader_does_not_delay_another_clients_round_trip() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon_bursting(BURST).await;

    // The stalled client: it bootstraps, which arms the burst, and then never
    // touches its socket again. Held to the end of the test so its connection
    // stays open and its queue stays the daemon's problem.
    let (mut stalled, _) = connect_initialized(&sandbox).await;
    let _stalled_revision = bootstrap_revision(&mut stalled).await;

    // The healthy client never bootstraps, so it subscribes to nothing: what is
    // measured is the daemon's willingness to answer at all, with no event
    // traffic of its own in the way.
    let (mut healthy, _) = connect_initialized(&sandbox).await;
    let mut samples = Vec::new();
    for _ in 0..LATENCY_SAMPLES {
        let started = Instant::now();
        within(
            "daemon.status under a stalled reader",
            healthy.call("daemon.status", json!({})),
        )
        .await
        .expect("the daemon answers a healthy client");
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let median = samples[LATENCY_SAMPLES / 2];
    assert!(
        median < LATENCY_BOUND,
        "a stalled reader delayed a healthy client: median {median:?} over {LATENCY_SAMPLES} \
         samples, worst {:?}, bound {LATENCY_BOUND:?}",
        samples[LATENCY_SAMPLES - 1]
    );

    // And a bootstrap, the heaviest query the daemon serves, is just as
    // unaffected.
    let started = Instant::now();
    let _ = bootstrap_revision(&mut healthy).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < LATENCY_BOUND,
        "a state.bootstrap took {elapsed:?} while another client stalled, bound {LATENCY_BOUND:?}"
    );

    drop(stalled);
}

/// The plan's overflow path end to end: the stalled client eventually finds a
/// resync waiting for it, it received far fewer events than the daemon
/// committed (so they were discarded rather than buffered), and nothing domain
/// follows the control message.
#[tokio::test]
async fn a_stalled_reader_is_told_to_resync_and_sees_no_domain_event_after_it() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon_bursting(BURST).await;
    let (mut conn, hello) = connect_initialized(&sandbox).await;

    let bootstrap = bootstrap_revision(&mut conn).await;
    let seen = within(
        "the stalled client draining to its resync",
        drain_until_resync(&mut conn, bootstrap, &hello.instance_id),
    )
    .await;

    assert!(
        seen < BURST / 2,
        "the daemon delivered {seen} of {BURST} events before resyncing, which is not a \
         discard: a bounded queue plus a socket buffer cannot hold that many"
    );
    assert_no_event_within(
        &mut conn,
        Duration::from_millis(500),
        "a client that has been asked to resync",
    )
    .await;
}

/// And the exit: a fresh bootstrap ends the poison, and the client is served
/// domain events again, above the revision its new snapshot was captured at.
#[tokio::test]
async fn a_client_that_re_bootstraps_after_a_resync_receives_events_again() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon_bursting(BURST).await;
    let (mut conn, hello) = connect_initialized(&sandbox).await;

    let first = bootstrap_revision(&mut conn).await;
    let discarded = within(
        "the first drain to a resync",
        drain_until_resync(&mut conn, first, &hello.instance_id),
    )
    .await;
    assert!(
        discarded < BURST,
        "the queue overflowed rather than kept up"
    );

    // The recovery: a fresh bootstrap clears the poison and arms a new burst,
    // whose revisions are all above the revision it just reported.
    let mut watermark = bootstrap_revision(&mut conn).await;
    assert!(
        watermark >= first,
        "revisions never go backwards within an instance"
    );

    // A client that is resynced again bootstraps again: that is the protocol's
    // whole recovery loop, and what this asserts is that it converges on being
    // served rather than starving. Bounded by the attempt count and by the
    // deadline around the whole loop.
    let mut resyncs = 0;
    let delivered = within("an event after the re-bootstrap", async {
        loop {
            match next_observed(&mut conn).await {
                // A leftover from before the bootstrap, at or below the
                // revision the new snapshot already carries: the client's
                // watermark drops it, and so does this test.
                Observed::Event(event) if event.revision <= watermark => continue,
                Observed::Event(event) => {
                    assert_eq!(event.instance_id, hello.instance_id);
                    assert_eq!(event.kind, KIND_INVALIDATE);
                    return event.revision;
                }
                Observed::Resync { .. } => {
                    resyncs += 1;
                    assert!(
                        resyncs <= 3,
                        "a re-bootstrapped client was resynced {resyncs} times without ever \
                         being sent an event"
                    );
                    watermark = bootstrap_revision(&mut conn).await;
                }
            }
        }
    })
    .await;

    assert!(
        delivered > watermark,
        "the event that resumed the stream postdates the bootstrap that asked for it: \
         {delivered} after {watermark}"
    );
}

/// The last recovery path: when the control message cannot be written the daemon
/// drops that connection and keeps serving everybody else.
#[tokio::test]
async fn a_client_that_closed_its_socket_is_dropped_and_the_daemon_keeps_serving() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon_bursting(BURST).await;

    {
        let (mut doomed, hello) = connect_initialized(&sandbox).await;
        let bootstrap = bootstrap_revision(&mut doomed).await;
        assert!(!hello.instance_id.is_empty());

        // Read three events, which proves the burst is flowing. By the time
        // three frames have crossed the socket the daemon has committed
        // thousands, so this connection's queue has overflowed and its control
        // message is pending. Closing here is therefore a write failure the
        // daemon has to survive, whether the frame it loses is that control
        // message or the event before it.
        let mut watermark = bootstrap;
        for _ in 0..3 {
            match within("an event before the close", next_observed(&mut doomed)).await {
                Observed::Event(event) => {
                    assert!(event.revision > watermark);
                    watermark = event.revision;
                }
                Observed::Resync { .. } => break,
            }
        }
    } // The socket closes here.

    // A survivor is served normally, which is the whole assertion: the daemon
    // dropped one connection rather than itself.
    let (mut survivor, _) = connect_initialized(&sandbox).await;
    within(
        "daemon.status after a client vanished",
        survivor.call("daemon.status", json!({})),
    )
    .await
    .expect("the daemon still answers");
    let revision = bootstrap_revision(&mut survivor).await;
    assert!(revision > 0, "and still bootstraps");
    assert!(
        sandbox.daemon_is_alive(),
        "a client that closed its socket must not take the daemon with it"
    );
}
