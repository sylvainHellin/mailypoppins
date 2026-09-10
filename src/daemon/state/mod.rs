//! The canonical state, its revisions, and the bootstrap that hands it over
//! (P3a-U4).
//!
//! One [`CanonicalState`] per daemon process holds everything a client mirrors,
//! and every change to it takes the next [`Revision`]. A connection reaches it
//! twice: [`CanonicalState::subscribe`] creates its event queue, and
//! [`CanonicalState::bootstrap`] attaches that queue to the fan-out and
//! captures a [`Snapshot`] in one serialized operation.
//!
//! # The two rules, from `docs/plans/daemon-bootstrap.md`
//!
//! **Register the subscriber before capturing the snapshot.** Every change
//! committed from the attachment onwards is then already queued, which is
//! exactly what a register-last implementation loses between the capture and the
//! start of queuing.
//!
//! **The client initialises its watermark to the captured revision and silently
//! drops every event at or below it.** That is what makes register-first safe:
//! an event for a change the snapshot already carries arrives, is recognised as
//! redundant, and is dropped rather than applied twice.
//!
//! # The reentrant gate
//!
//! The serialized section cannot be a plain `Mutex`. Anything the state calls
//! while holding it - a race hook in a test, a metric, a log sink that reads
//! state - can commit a change from the *same* thread, and a non-reentrant lock
//! held across that call deadlocks on itself. [`Gate`] therefore records the
//! thread that holds it: a reentrant entry proceeds, another thread blocks.

pub mod events;
pub mod revision;
pub mod snapshot;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::ThreadId;
use std::time::Duration;

use tokio::sync::Notify;

pub use revision::{ConnectionId, InstanceId, Revision};
pub use snapshot::{seeds_from_config, AccountSeed, AccountState, Change, MailboxSeed, Snapshot};

use snapshot::{AccountView, DraftView, MailboxView, OutboxView};

/// Test-only hook: milliseconds after the **first** `state.bootstrap` at which
/// every configured account flips to `ready`, one committed change each, in
/// `config.toml` order.
///
/// Phase 3a starts no account runtimes, so nothing would otherwise ever report
/// readiness and the "converges by event with no second bootstrap" case would be
/// untestable. The countdown starts at the bootstrap rather than at startup, so
/// a client that bootstraps cannot lose the race and no test has to sleep to win
/// it. On the [`FAIL_START_ENV`](crate::daemon::lifecycle::FAIL_START_ENV)
/// precedent: no flag exposes it and `mp --help` never moves.
pub const FAKE_READY_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS";

/// The delay [`FAKE_READY_ENV`] asks for, or `None` when it is unset or is not
/// a number.
pub fn fake_ready_delay() -> Option<Duration> {
    std::env::var(FAKE_READY_ENV)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_millis)
}

/// The four points inside the serialized section of [`CanonicalState::bootstrap`]
/// where another connection's change can land.
///
/// The fifth boundary of the recorded observation table, "after the response
/// frame was written", is outside `bootstrap` by construction and needs no hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// Before this connection's queue is attached to the fan-out.
    BeforeRegister,
    /// Attached, before the snapshot is captured.
    AfterRegister,
    /// Captured. The boundary a register-last implementation loses.
    AfterCapture,
    /// Queuing is running and the response is not assembled yet. Attaching
    /// *is* starting to queue here, so nothing happens between this boundary
    /// and [`Boundary::AfterCapture`]; both are kept because the recorded table
    /// names both, and a later design that buffers elsewhere would separate
    /// them again.
    AfterQueueStart,
}

/// A callback the state invokes at every [`Boundary`], for tests that force a
/// race deterministically instead of racing threads. It may commit changes, on
/// the bootstrapping thread, which is what the reentrant gate exists for.
pub type RaceHook = Arc<dyn Fn(Boundary, &CanonicalState) + Send + Sync>;

// ---------------------------------------------------------------------------
// The subscriber's end of the fan-out
// ---------------------------------------------------------------------------

/// One connection's queue of committed changes, in revision order.
///
/// Unbounded here and a hand-off rather than a backlog: the connection task
/// drains it eagerly into its [`Outbound`](events::Outbound), which is the one
/// buffer a stalled client can grow and the one the two caps bound.
#[derive(Clone, Debug)]
pub struct EventQueue {
    events: Arc<Mutex<VecDeque<(Revision, Change)>>>,
    lifecycle: Arc<Mutex<VecDeque<(Revision, events::Event)>>>,
    ready: Arc<Notify>,
    /// The RAII half of the subscription: the endpoint stays in the fan-out
    /// until the last handle of this queue drops. Never read, which is the
    /// point of an owner.
    _subscription: Arc<Subscription>,
}

impl EventQueue {
    /// The oldest queued change, or `None` when nothing is queued.
    pub fn try_recv(&mut self) -> Option<(Revision, Change)> {
        lock(&self.events).pop_front()
    }

    /// Every queued change, oldest first, leaving the queue empty.
    pub fn drain(&mut self) -> Vec<(Revision, Change)> {
        lock(&self.events).drain(..).collect()
    }

    /// Every queued lifecycle event, oldest first, leaving that queue empty.
    ///
    /// A second queue rather than a second [`Change`] variant: a lifecycle
    /// event is about the daemon and not about the state, it reduces to
    /// nothing, and no snapshot carries it. [`EventQueue::drain_all`] is what
    /// the connection loop takes; this is the half of it the contract pins.
    pub fn drain_lifecycle(&mut self) -> Vec<(Revision, events::Event)> {
        lock(&self.lifecycle).drain(..).collect()
    }

    /// Everything this connection is owed, as [`events::Event`]s, oldest
    /// first, leaving both queues empty.
    ///
    /// Both locks are taken before either queue is emptied, and that is the
    /// whole point of the method. Draining the two queues under two separate
    /// acquisitions lets a change committed between them (revision N) be
    /// handed over *after* a lifecycle event that was published later (N+1):
    /// the client's `StateTracker` then sees N below its watermark and drops
    /// it as a duplicate, losing a change no snapshot will bring back.
    ///
    /// The lock order is events then lifecycle, and nothing takes them the
    /// other way round: [`CanonicalState::apply`] takes the events lock and
    /// [`CanonicalState::publish`] the lifecycle one, each one at a time and
    /// each behind the gate.
    pub fn drain_all(&mut self) -> Vec<(Revision, events::Event)> {
        let mut events = lock(&self.events);
        let mut lifecycle = lock(&self.lifecycle);
        let mut items: Vec<(Revision, events::Event)> = events
            .drain(..)
            .map(|(revision, change)| (revision, events::Event::from_change(&change)))
            .chain(lifecycle.drain(..))
            .collect();
        items.sort_by_key(|(revision, _)| *revision);
        items
    }

    /// How many changes are queued. The lifecycle queue is counted separately,
    /// because every caller of this reasons about committed changes.
    pub fn len(&self) -> usize {
        lock(&self.events).len()
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Resolve once the queue holds something, immediately if it already does.
    ///
    /// The async companion the socket writer selects on; the in-process
    /// contract drains directly and never needs it. Registered before the
    /// check, so a push racing this call wakes it instead of being missed
    /// between the two.
    pub async fn ready(&self) {
        loop {
            let waiting = self.ready.notified();
            if !self.is_empty() || !lock(&self.lifecycle).is_empty() {
                return;
            }
            waiting.await;
        }
    }
}

/// The state's end of one connection's queue.
///
/// Named for the endpoint rather than for the subscriber, because
/// [`events::Subscriber`] is the *limits* a connection's outbound queue is held
/// to and the two would otherwise read as one thing.
#[derive(Debug)]
struct Endpoint {
    events: Arc<Mutex<VecDeque<(Revision, Change)>>>,
    lifecycle: Arc<Mutex<VecDeque<(Revision, events::Event)>>>,
    ready: Arc<Notify>,
    /// False between `subscribe` and `bootstrap`: the endpoint exists, but the
    /// fan-out does not write to it yet.
    attached: bool,
}

/// One connection's place in the fan-out, owned by its [`EventQueue`].
///
/// Dropping the last handle of that queue removes the endpoint, so a
/// connection that ends any way at all - a clean close, a framing error, a
/// panic in its task - stops accumulating events without anyone remembering to
/// say so. [`Weak`] rather than [`Arc`]: a state that is already gone has no
/// map to clean up, and the queue must not keep the whole state alive.
#[derive(Debug)]
struct Subscription {
    conn: ConnectionId,
    inner: Weak<Mutex<Inner>>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            lock(&inner).subscribers.remove(&self.conn);
        }
    }
}

// ---------------------------------------------------------------------------
// The state
// ---------------------------------------------------------------------------

/// Everything the daemon holds and every client mirrors.
pub struct CanonicalState {
    instance: InstanceId,
    gate: Gate,
    /// Behind an [`Arc`] so a subscription guard can hold a [`Weak`] to it and
    /// unregister its endpoint when the connection's queue drops.
    inner: Arc<Mutex<Inner>>,
    hook: Mutex<Option<RaceHook>>,
    operations: Mutex<Option<Arc<crate::daemon::operations::OperationRegistry>>>,
}

#[derive(Debug)]
struct Inner {
    revision: u64,
    accounts: Vec<AccountView>,
    mailboxes: BTreeMap<String, Vec<MailboxView>>,
    drafts: BTreeMap<String, Vec<DraftView>>,
    outbox: BTreeMap<String, OutboxView>,
    subscribers: BTreeMap<ConnectionId, Endpoint>,
}

impl CanonicalState {
    /// A state seeded from the configured accounts, at [`Revision`] 1.
    ///
    /// One, not zero: the protocol reserves `0` as the client's pre-bootstrap
    /// sentinel, so no bootstrap may ever report it.
    pub fn new(instance_id: InstanceId, accounts: Vec<AccountSeed>) -> Self {
        let mut inner = Inner {
            revision: 1,
            accounts: Vec::new(),
            mailboxes: BTreeMap::new(),
            drafts: BTreeMap::new(),
            outbox: BTreeMap::new(),
            subscribers: BTreeMap::new(),
        };
        for seed in accounts {
            inner.mailboxes.insert(
                seed.name.clone(),
                seed.mailboxes
                    .into_iter()
                    .map(|seed| MailboxView {
                        seed,
                        total: 0,
                        unread: 0,
                        badge: 0,
                    })
                    .collect(),
            );
            inner.drafts.insert(seed.name.clone(), Vec::new());
            inner
                .outbox
                .insert(seed.name.clone(), OutboxView::default());
            inner.accounts.push(AccountView {
                name: seed.name,
                state: AccountState::Opening,
                health: crate::sync_health::SyncHealth::default(),
            });
        }
        CanonicalState {
            instance: instance_id,
            gate: Gate::default(),
            inner: Arc::new(Mutex::new(inner)),
            hook: Mutex::new(None),
            operations: Mutex::new(None),
        }
    }

    /// Point the snapshot's `operations` array at the daemon's registry.
    ///
    /// Not a constructor parameter: the registry is the daemon's, not the
    /// state's, and every in-process caller of [`CanonicalState::new`] builds a
    /// state that has none.
    pub fn attach_operations(&self, registry: Arc<crate::daemon::operations::OperationRegistry>) {
        *lock(&self.operations) = Some(registry);
    }

    /// The daemon process this state belongs to.
    pub fn instance_id(&self) -> InstanceId {
        self.instance.clone()
    }

    /// The last revision committed.
    pub fn revision(&self) -> Revision {
        Revision(lock(&self.inner).revision)
    }

    /// Every account, in the order they were seeded, which is `config.toml`'s.
    pub fn account_names(&self) -> Vec<String> {
        lock(&self.inner)
            .accounts
            .iter()
            .map(|account| account.name.clone())
            .collect()
    }

    /// Commit one change and return the revision it committed at.
    ///
    /// Serialized against every other commit and against the capture, so a
    /// snapshot never shows half of a change.
    pub fn apply(&self, change: Change) -> Revision {
        let _gate = self.gate.enter();
        let mut inner = lock(&self.inner);
        inner.revision += 1;
        let revision = Revision(inner.revision);
        inner.reduce(&change);
        for subscriber in inner.subscribers.values().filter(|s| s.attached) {
            lock(&subscriber.events).push_back((revision, change.clone()));
            subscriber.ready.notify_waiters();
        }
        revision
    }

    /// Stamp one lifecycle event with the next revision and queue it for every
    /// bootstrapped connection.
    ///
    /// The revision comes from the same counter and the same gate a committed
    /// change takes one from, so an operation event and a state change are
    /// comparable on one client's watermark. Nothing is reduced: a lifecycle
    /// event is about the daemon rather than about the state, and no snapshot
    /// brings it back.
    pub fn publish(&self, event: events::Event) -> Revision {
        let _gate = self.gate.enter();
        let mut inner = lock(&self.inner);
        inner.revision += 1;
        let revision = Revision(inner.revision);
        for subscriber in inner.subscribers.values().filter(|s| s.attached) {
            lock(&subscriber.lifecycle).push_back((revision, event.clone()));
            subscriber.ready.notify_waiters();
        }
        revision
    }

    /// Replace the set of accounts the state holds, keeping what is known
    /// about the ones that stay (P3b-U8).
    ///
    /// A configuration swap adds and removes accounts, and a change naming an
    /// account the state does not have is dropped by [`Inner::reduce`]: an
    /// added account must therefore exist here before its runtime reports
    /// readiness, and a removed one must be gone before the swap is over. It
    /// commits nothing and takes no revision, because it is not a change a
    /// client applies: the `state.remove` of a departed account and the
    /// `account.state_changed` of a new one are published around it, and
    /// `config.changed` closes the swap.
    pub fn reseed(&self, accounts: Vec<AccountSeed>) {
        let _gate = self.gate.enter();
        let mut inner = lock(&self.inner);
        let names: Vec<String> = accounts.iter().map(|seed| seed.name.clone()).collect();
        inner.accounts.retain(|view| names.contains(&view.name));
        inner.mailboxes.retain(|name, _| names.contains(name));
        inner.drafts.retain(|name, _| names.contains(name));
        inner.outbox.retain(|name, _| names.contains(name));
        for seed in accounts {
            // An account that stayed keeps its state and its counts: a swap
            // that touched another account may not blank this one's sidebar.
            let mailboxes = seed
                .mailboxes
                .into_iter()
                .map(|seed| MailboxView {
                    seed,
                    total: 0,
                    unread: 0,
                    badge: 0,
                })
                .collect();
            if inner.accounts.iter().any(|view| view.name == seed.name) {
                continue;
            }
            inner.mailboxes.insert(seed.name.clone(), mailboxes);
            inner.drafts.insert(seed.name.clone(), Vec::new());
            inner
                .outbox
                .insert(seed.name.clone(), OutboxView::default());
            inner.accounts.push(AccountView {
                name: seed.name,
                state: AccountState::Opening,
                health: crate::sync_health::SyncHealth::default(),
            });
        }
        // The order a client reads is the order `config.toml` writes.
        inner
            .accounts
            .sort_by_key(|view| names.iter().position(|name| name == &view.name));
    }

    /// Create this connection's queue endpoint, which nothing writes to until
    /// [`CanonicalState::bootstrap`] attaches it.
    ///
    /// The returned queue *owns* the subscription: dropping it (or its last
    /// clone) removes the endpoint again, so no caller has to pair this with an
    /// unsubscribe and no teardown path can forget to.
    pub fn subscribe(&self, conn: ConnectionId) -> EventQueue {
        let events = Arc::new(Mutex::new(VecDeque::new()));
        let lifecycle = Arc::new(Mutex::new(VecDeque::new()));
        let ready = Arc::new(Notify::new());
        lock(&self.inner).subscribers.insert(
            conn,
            Endpoint {
                events: Arc::clone(&events),
                lifecycle: Arc::clone(&lifecycle),
                ready: Arc::clone(&ready),
                attached: false,
            },
        );
        EventQueue {
            events,
            lifecycle,
            ready,
            _subscription: Arc::new(Subscription {
                conn,
                inner: Arc::downgrade(&self.inner),
            }),
        }
    }

    /// How many connections the fan-out writes to, attached or not.
    ///
    /// The observable half of the RAII subscription: a departed client's
    /// endpoint is gone from here, which is what "its events do not accumulate
    /// forever" means in a test.
    pub fn subscriber_count(&self) -> usize {
        lock(&self.inner).subscribers.len()
    }

    /// Attach `conn`'s queue to the fan-out and capture the state, as one
    /// serialized operation.
    ///
    /// A `conn` that never subscribed has no queue to attach and registers
    /// nothing, which makes this a plain "what does the state hold right now".
    pub fn bootstrap(&self, conn: ConnectionId) -> (Snapshot, Revision, InstanceId) {
        let _gate = self.gate.enter();

        self.fire(Boundary::BeforeRegister);
        if let Some(subscriber) = lock(&self.inner).subscribers.get_mut(&conn) {
            subscriber.attached = true;
        }
        self.fire(Boundary::AfterRegister);

        let (mut snapshot, revision) = {
            let inner = lock(&self.inner);
            (inner.capture(), Revision(inner.revision))
        };
        // The registry is not part of the state a client mirrors, so its
        // projection is taken here rather than reduced into `Inner`.
        snapshot.operations = lock(&self.operations)
            .as_ref()
            .map(|registry| {
                registry
                    .live()
                    .iter()
                    .map(crate::daemon::operations::OperationStatus::to_json)
                    .collect()
            })
            .unwrap_or_default();
        self.fire(Boundary::AfterCapture);
        self.fire(Boundary::AfterQueueStart);

        (snapshot, revision, self.instance.clone())
    }

    /// Install the callback every [`Boundary`] invokes. Replaces any earlier
    /// one; `None` is not expressible, because only a test installs one at all.
    pub fn set_race_hook(&self, hook: RaceHook) {
        *lock(&self.hook) = Some(hook);
    }

    /// Invoke the race hook, holding no lock but the gate: the hook may commit
    /// changes, and it does so on this thread.
    fn fire(&self, at: Boundary) {
        let hook = lock(&self.hook).clone();
        if let Some(hook) = hook {
            hook(at, self);
        }
    }
}

impl Inner {
    /// Apply one change to the state itself.
    ///
    /// A change naming an account or a mailbox this daemon does not have is
    /// dropped: the fan-out is the daemon's own, so that is a daemon bug, and
    /// inventing the account would hide it behind a snapshot a client cannot
    /// reconcile with `account.list`.
    fn reduce(&mut self, change: &Change) {
        let account = change.account().to_string();
        match change {
            Change::AccountReady { .. } => self.set_state(&account, AccountState::Ready),
            Change::AccountBlocked { .. } => self.set_state(&account, AccountState::Blocked),
            Change::MailboxCounts {
                mailbox,
                total,
                unread,
                badge,
                ..
            } => {
                if let Some(view) = self
                    .mailboxes
                    .get_mut(&account)
                    .and_then(|list| list.iter_mut().find(|view| &view.seed.slug == mailbox))
                {
                    view.total = *total;
                    view.unread = *unread;
                    view.badge = *badge;
                }
            }
            Change::DraftUpsert {
                id,
                path,
                to,
                subject,
                status,
                valid,
                ready,
                ..
            } => self.upsert_draft(
                &account,
                DraftView {
                    id: id.clone(),
                    path: path.clone(),
                    to: to.clone(),
                    subject: subject.clone(),
                    status: status.clone(),
                    valid: *valid,
                    ready: *ready,
                },
            ),
            // A file that would not parse is still a row, with nothing read
            // out of it: `"invalid"` is not an `EmailStatus`, and that is the
            // point.
            Change::DraftInvalid { id, path, .. } => self.upsert_draft(
                &account,
                DraftView {
                    id: id.clone(),
                    path: path.clone(),
                    to: None,
                    subject: String::new(),
                    status: "invalid".to_string(),
                    valid: false,
                    ready: false,
                },
            ),
            Change::DraftRemoved { id, .. } => {
                if let Some(list) = self.drafts.get_mut(&account) {
                    list.retain(|entry| &entry.id != id);
                }
            }
            Change::OutboxCounts { queued, failed, .. } => {
                if let Some(outbox) = self.outbox.get_mut(&account) {
                    outbox.queued = *queued;
                    outbox.failed = *failed;
                }
            }
            // A command outcome reduces to nothing: no snapshot carries a last
            // sync, so a client that bootstraps between two ticks learns about
            // neither, which is what a command outcome is. It still takes a
            // revision and still fans out.
            Change::SyncCompleted(_) => {}
        }
    }

    /// Replace one draft row of `account`, appending it when the account has
    /// none under that id.
    fn upsert_draft(&mut self, account: &str, draft: DraftView) {
        if let Some(list) = self.drafts.get_mut(account) {
            match list.iter().position(|entry| entry.id == draft.id) {
                Some(at) => list[at] = draft,
                None => list.push(draft),
            }
        }
    }

    fn set_state(&mut self, account: &str, state: AccountState) {
        if let Some(view) = self.accounts.iter_mut().find(|view| view.name == account) {
            view.state = state;
        }
    }

    /// A whole-state capture. Cloned rather than shared, because the snapshot
    /// outlives the lock and must not move afterwards.
    fn capture(&self) -> Snapshot {
        Snapshot {
            accounts: self.accounts.clone(),
            mailboxes: self.mailboxes.clone(),
            drafts: self.drafts.clone(),
            outbox: self.outbox.clone(),
            operations: Vec::new(),
        }
    }
}

impl std::fmt::Debug for CanonicalState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanonicalState")
            .field("instance", &self.instance)
            .field("revision", &lock(&self.inner).revision)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// The reentrant gate
// ---------------------------------------------------------------------------

/// A mutual exclusion that a thread already holding may enter again.
#[derive(Debug, Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct GateState {
    owner: Option<ThreadId>,
    depth: usize,
}

impl Gate {
    fn enter(&self) -> GateGuard<'_> {
        let me = std::thread::current().id();
        let mut state = lock(&self.state);
        loop {
            match state.owner {
                None => {
                    state.owner = Some(me);
                    state.depth = 1;
                    break;
                }
                Some(owner) if owner == me => {
                    state.depth += 1;
                    break;
                }
                Some(_) => {
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner)
                }
            }
        }
        GateGuard { gate: self }
    }
}

/// Leaves the gate when it drops, releasing it only at the outermost entry.
struct GateGuard<'a> {
    gate: &'a Gate,
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.gate.state);
        state.depth = state.depth.saturating_sub(1);
        if state.depth == 0 {
            state.owner = None;
            self.gate.changed.notify_all();
        }
    }
}

/// Lock, recovering from a poisoned mutex.
///
/// A panic in a race hook or in a connection task must not wedge the daemon's
/// whole state: the data behind these locks is always left consistent, because
/// every section between a lock and its release is a handful of infallible
/// field writes.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> CanonicalState {
        CanonicalState::new(
            InstanceId::new("test"),
            vec![AccountSeed {
                name: "alpha".to_string(),
                mailboxes: vec![MailboxSeed {
                    role: "inbox".to_string(),
                    slug: "inbox".to_string(),
                    label: "Inbox".to_string(),
                }],
            }],
        )
    }

    /// A fresh state is at 1, so the protocol's `0` sentinel never travels.
    #[test]
    fn a_fresh_state_starts_above_the_sentinel() {
        let state = state();
        assert_eq!(state.revision(), Revision(1));
        assert_eq!(
            state.apply(Change::AccountReady {
                account: "alpha".to_string()
            }),
            Revision(2)
        );
    }

    /// A change naming an account this daemon does not have is dropped rather
    /// than inventing one the rest of the daemon has never heard of.
    #[test]
    fn a_change_for_an_unknown_account_is_dropped() {
        let state = state();
        state.apply(Change::AccountReady {
            account: "nobody".to_string(),
        });
        let json = state.bootstrap(ConnectionId(1)).0.to_json();
        assert_eq!(json["accounts"].as_array().expect("accounts").len(), 1);
        assert_eq!(json["accounts"][0]["state"], serde_json::json!("opening"));
    }

    /// The gate lets the same thread back in, which is what a hook that
    /// commits a change from inside `bootstrap` needs.
    #[test]
    fn the_gate_is_reentrant_on_one_thread() {
        let gate = Gate::default();
        let outer = gate.enter();
        let inner = gate.enter();
        drop(inner);
        drop(outer);
    }

    /// The merged drain is revision-ordered *across* calls, which is what one
    /// lock per queue could not promise: a change committed while the domain
    /// queue had already been emptied used to be handed over behind a
    /// lifecycle event published after it, and the client dropped it as a
    /// duplicate.
    #[test]
    fn the_merged_drain_is_revision_ordered_across_calls() {
        const ROUNDS: u64 = 200;

        let state = Arc::new(state());
        let conn = ConnectionId(1);
        let mut queue = state.subscribe(conn);
        state.bootstrap(conn);

        let producer = Arc::clone(&state);
        let handle = std::thread::spawn(move || {
            for round in 0..ROUNDS {
                producer.apply(Change::MailboxCounts {
                    account: "alpha".to_string(),
                    mailbox: "inbox".to_string(),
                    total: round,
                    unread: 0,
                    badge: round,
                });
                producer.publish(events::Event::Lifecycle {
                    kind: "operation.progress",
                    payload: serde_json::json!({ "round": round }),
                });
            }
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut seen: Vec<Revision> = Vec::new();
        while seen.len() < (2 * ROUNDS) as usize {
            for (revision, _) in queue.drain_all() {
                if let Some(last) = seen.last() {
                    assert!(
                        revision > *last,
                        "revision {revision:?} was handed over after {last:?}"
                    );
                }
                seen.push(revision);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "only {} of {} items were drained",
                seen.len(),
                2 * ROUNDS
            );
            std::thread::yield_now();
        }
        handle.join().expect("the producer thread finished");
        assert!(
            queue.drain_all().is_empty(),
            "the drain leaves both queues empty"
        );
    }

    /// One drain of two queues filled in a known interleaving, without a
    /// thread: the merge is by revision and not by queue.
    #[test]
    fn one_merged_drain_interleaves_the_two_queues() {
        let state = state();
        let conn = ConnectionId(1);
        let mut queue = state.subscribe(conn);
        state.bootstrap(conn);

        state.apply(Change::AccountReady {
            account: "alpha".to_string(),
        });
        state.publish(events::Event::Lifecycle {
            kind: "daemon.shutting_down",
            payload: serde_json::json!({"in_seconds": 5}),
        });
        state.apply(Change::MailboxCounts {
            account: "alpha".to_string(),
            mailbox: "inbox".to_string(),
            total: 3,
            unread: 1,
            badge: 3,
        });

        let drained = queue.drain_all();
        let revisions: Vec<u64> = drained.iter().map(|(rev, _)| rev.get()).collect();
        assert_eq!(revisions, vec![2, 3, 4]);
        assert!(
            !drained[0].1.is_lifecycle()
                && drained[1].1.is_lifecycle()
                && !drained[2].1.is_lifecycle(),
            "the lifecycle event sits between the two changes it was published between"
        );
    }

    /// The subscription is the queue's: dropping it unregisters the endpoint,
    /// so no teardown path has to remember to.
    #[test]
    fn dropping_the_queue_unsubscribes_the_connection() {
        let state = state();
        assert_eq!(state.subscriber_count(), 0);

        let queue = state.subscribe(ConnectionId(1));
        state.bootstrap(ConnectionId(1));
        assert_eq!(state.subscriber_count(), 1);

        // A clone keeps it alive: the endpoint goes when the *last* handle does.
        let clone = queue.clone();
        drop(queue);
        assert_eq!(state.subscriber_count(), 1, "a clone still holds it");
        drop(clone);
        assert_eq!(state.subscriber_count(), 0);

        // And the fan-out no longer writes to it, which is what the removal is
        // for: a departed client's events must not accumulate.
        state.apply(Change::AccountReady {
            account: "alpha".to_string(),
        });
        assert_eq!(state.subscriber_count(), 0);
    }

    /// An unset variable is no hook at all, and a value that is not a number
    /// is not a hook either rather than a zero-millisecond one.
    #[test]
    fn the_fake_ready_delay_is_absent_unless_it_is_a_number() {
        assert_eq!(FAKE_READY_ENV, "MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS");
        assert!(fake_ready_delay().is_none());
    }
}
