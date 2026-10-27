//! One account's runtime: the engine lock it holds, the read pool it serves
//! from, and the tick it runs (#0122, plan unit P3b-U4).
//!
//! A runtime is what makes a daemon *the* engine for an account. It takes the
//! account's `store.lock` for its whole lifetime rather than for the length of
//! one operation, opens the store behind a [`ReadPool`], and runs sync ticks
//! that drain the outbox and the mutation queue at both ends.
//!
//! ## Starting is synchronous, and a contended lock is not a failure
//!
//! [`AccountRuntime::start`] resolves [`Readiness`] before it returns, so a
//! value it handed back is never [`Readiness::Opening`]: `Opening` is what the
//! *daemon* reports for an account whose runtime it has asked for and not got
//! back yet, a fact about its table rather than a state a live runtime is in.
//! Starting takes a lock and opens SQLite, so the daemon starts each runtime on
//! `spawn_blocking`.
//!
//! A lock another process holds gives [`Readiness::Blocked`] and still a
//! successful start: the store is opened anyway and reads are served from it,
//! which is the read-only degrade `src/engine_lock.rs` documents and the reason
//! a second daemon on an account is useful rather than dead. A tick on a
//! blocked runtime enters no phase at all, the same reading `Ok(None)` gets
//! from [`crate::outbox::drain_guarded`] and
//! [`crate::sync::engine::run_sync_guarded`]: the holder does the work, and
//! refusing costs no login.
//!
//! ## The tick is [`run_tick_with_drains`] and nothing else
//!
//! Head drain, body, tail drain, the tail running on the `Err` path too
//! (#0114), and within each drain the outbox first and the mutation queue
//! second. [`TickOutcome::status`] is that function's first return value and
//! [`TickOutcome::error`] its second: a failed sync is a normal tick whose
//! error is carried rather than propagated, because the tail has already run by
//! the time it is known. [`TickOutcome::phases`] is the ordered record of the
//! five slots entered, which is the only way "the outbox goes first" can be
//! checked at all.
//!
//! A second [`AccountRuntime::tick`] issued while one is running does not start
//! a second body: it attaches to the running tick and returns its outcome with
//! `joined: true`, including its `kind`. A `Full` tick that joins a `Quick` one
//! reports `Quick`, because reporting the kind it asked for would claim a full
//! pass that never ran. The join lasts exactly as long as the run.
//!
//! ## A drain outside a tick (#0133)
//!
//! [`AccountRuntime::drain`] is the tick's drain without its body: the outbox
//! hook and then the mutation hook, entered as [`Phase::DrainOutbox`] and
//! [`Phase::DrainMutations`]. It is what the debounced drainer
//! ([`super::drainer`]) runs after a `settle: false` mutation, which queued an
//! op and touched no server, and which no tick would otherwise drain until a
//! manual sync or unrelated new mail.
//!
//! A tick's run and a drain take the same **turn**, an async mutex held across
//! the whole of either, so neither ever runs beside the other and the shared
//! [`TickReport`] is never written by two of them at once. A drain that finds a
//! tick running waits for it and then drains, rather than skipping: an op
//! queued after the tick's tail drain read the queue would otherwise wait for
//! the next tick. A drain is not a tick and nothing joins it; a tick that
//! arrives during one waits for its turn and then runs its own body.
//!
//! ## Why the production body calls `run_sync` and not `run_sync_guarded_at`
//!
//! The guard exists to make sure exactly one engine ingests an account, and a
//! ready runtime *is* that engine: it is holding `store.lock` while the body
//! runs. `flock` is per open file description rather than per process
//! (`src/engine_lock.rs`), so a `run_sync_guarded_at` inside the body would
//! open a second description of the same file, contend with the runtime's own
//! lock, and refuse every tick this daemon ever ran. The guarded entry point is
//! the right one for a caller that holds nothing (`mp sync`, the TUI); here the
//! lock is already held for longer than the call, which is strictly stronger.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use futures::future::{BoxFuture, FutureExt};
use log::{debug, info, warn};
use mp_protocol::events::SyncCompleted;
use tokio::sync::watch;

use crate::config::AccountConfig;
use crate::daemon::sync_outcome::from_sync_result;
use crate::engine_lock::EngineLock;
use crate::sync::tick::run_tick_with_drains;
use crate::sync::{FreshObservation, Stop, SyncResult};

use super::pool::{PooledRead, ReadPool};

/// How many of the newest UIDs per mailbox a [`TickKind::Quick`] tick
/// downloads. The TUI's quick sync uses the same number, and so does a
/// `sync.quick` whose caller named no bound (P4-U10).
pub const QUICK_TICK_LIMIT: usize = 100;

/// Whether an account's runtime can serve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// Asked for and not resolved yet. Never observed on a live runtime; it is
    /// what the daemon reports while a `start` is in flight.
    Opening,
    /// Holding the engine lock and serving.
    Ready,
    /// Another engine holds the lock. Reads are still served; nothing writes.
    Blocked { reason: String },
}

/// How much of the account a tick covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickKind {
    /// The newest [`QUICK_TICK_LIMIT`] UIDs per mailbox.
    Quick,
    /// Everything the mailbox lists.
    Full,
}

/// One of the five slots a tick enters, in order, or one of the two a drain
/// outside a tick enters (#0133).
///
/// A drain is two slots, not one, because "the outbox goes first and the
/// mutation queue second" is the invariant and a single symbol per drain would
/// leave it unpinned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The head drain's outbox half.
    HeadOutbox,
    /// The head drain's mutation-queue half.
    HeadMutations,
    /// The sync itself.
    Body,
    /// The tail drain's outbox half.
    TailOutbox,
    /// The tail drain's mutation-queue half.
    TailMutations,
    /// The outbox half of a drain outside a tick ([`AccountRuntime::drain`]).
    DrainOutbox,
    /// The mutation-queue half of a drain outside a tick.
    DrainMutations,
}

impl Phase {
    /// The name this slot travels under in an `operation.progress` report.
    ///
    /// The five names of a tick's slots are wire surface (P4-U10): `mp sync`
    /// labels a drain report line ` (after sync)` when the phase it came from
    /// is one of the tail's, and derives that from this string and nothing
    /// else. The two drain names never travel: a drain outside a tick is no
    /// operation and reports no progress, so they only name the slot in a log.
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::HeadOutbox => "head_outbox",
            Phase::HeadMutations => "head_mutations",
            Phase::Body => "body",
            Phase::TailOutbox => "tail_outbox",
            Phase::TailMutations => "tail_mutations",
            Phase::DrainOutbox => "drain_outbox",
            Phase::DrainMutations => "drain_mutations",
        }
    }

    /// Whether this slot belongs to the drain that runs after the body.
    pub fn is_tail(self) -> bool {
        matches!(self, Phase::TailOutbox | Phase::TailMutations)
    }
}

/// What one tick did.
#[derive(Clone, Debug)]
pub struct TickOutcome {
    /// The kind that actually ran, which for a joiner is the kind of the tick
    /// it attached to rather than the one it asked for.
    pub kind: TickKind,
    /// The slots the tick entered, in order. Empty on a blocked tick.
    pub phases: Vec<Phase>,
    /// Whether this call attached to a tick already in flight.
    pub joined: bool,
    /// Whether the runtime refused because another engine holds the lock.
    pub blocked: bool,
    /// The body-fetch budget this tick carried, `None` for unbounded.
    pub body_deadline: Option<Duration>,
    /// [`run_tick_with_drains`]' status text: the head drain's suffix followed
    /// by the tail's, each of them its outbox suffix then its mutation suffix.
    pub status: String,
    /// The body's error, rendered with `{:#}`. A failing body is reported here,
    /// never propagated: the tail has already run by the time it is known.
    pub error: Option<String>,
    /// The tick's typed outcome, ready to be committed as a
    /// [`Change::SyncCompleted`](crate::daemon::state::Change).
    ///
    /// `None` on a blocked tick, which entered no phase and has nothing to
    /// report, and `None` on a joiner, because the tick it attached to reports
    /// its own outcome once and two commits for one tick would be two facts
    /// where there was one.
    pub sync: Option<SyncCompleted>,
    /// How many contacts the account's index holds once the tick folded in
    /// the correspondents its pass inserted (PERSO-234), `None` when it wrote
    /// no index; the committer announces it as `contact.changed`. `None` on a
    /// joiner too, for the reason `sync` is.
    pub contacts: Option<usize>,
}

/// What one [`AccountRuntime::drain`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrainOutcome {
    /// Whether the drain ran at all: `false` on a blocked or retired runtime,
    /// which entered no slot.
    pub ran: bool,
    /// The two hooks' status suffixes, the outbox's then the mutation queue's.
    pub status: String,
    /// Mutations the drain rolled back. Always zero for a runtime built with
    /// injected hooks, exactly as a tick's count is.
    pub failed_mutations: u64,
    /// Whether the mutation drain found ops owed and was refused the engine
    /// turn, by a guarded pass this runtime's turn does not cover (the
    /// post-send outbox drain, a `mp sync --mailbox`). The ops are still
    /// queued, and the drainer tries again rather than counting the drain as
    /// clean.
    pub busy: bool,
}

/// What a hook is told about the slot it is filling.
#[derive(Clone, Debug)]
pub struct TickContext {
    /// The account this tick belongs to.
    pub account: String,
    /// The kind of tick that is running. A drain outside a tick has no kind
    /// and says [`TickKind::Quick`], which no drain hook reads.
    pub kind: TickKind,
    /// Which of the five slots this call is.
    pub phase: Phase,
    /// The body-fetch budget the tick carries, `None` for unbounded.
    pub body_deadline: Option<Duration>,
    /// What the body asks at every fetch chunk and mailbox boundary
    /// (PERSO-127): the stop of the call that started this tick, `None` for
    /// a tick nothing can stop (the scheduler's, the watcher's) and for every
    /// drain slot.
    pub stop: Option<Stop>,
}

/// One half of a drain: it does its work and returns the status suffix it wants
/// appended to the tick's message, empty when it did nothing.
pub type DrainHook = Arc<dyn Fn(TickContext) -> BoxFuture<'static, String> + Send + Sync>;

/// The sync body. Its error is carried on the outcome rather than propagated.
pub type BodyHook = Arc<dyn Fn(TickContext) -> BoxFuture<'static, Result<()>> + Send + Sync>;

/// The three things a tick runs, injectable as one.
///
/// [`crate::sync::SyncBackend`] returns `impl Future`, so it is not object safe
/// and `Box<dyn SyncBackend>` is not a type; the seam is therefore a struct of
/// boxed async callbacks rather than a trait object.
pub struct TickHooks {
    /// The outbox drain, run at the head and again at the tail.
    pub outbox: DrainHook,
    /// The mutation-queue drain, run after the outbox at each end.
    pub mutations: DrainHook,
    /// The sync itself.
    pub body: BodyHook,
}

/// Where the production hooks leave what only they can know.
///
/// The tick's seam is [`TickHooks`], whose three callbacks return a status
/// string and a bare `Result<()>`: that is the shape
/// `tests/daemon_account_runtime.rs` pins, and widening it would rewrite a
/// contract this unit does not own. The engine's [`SyncResult`] and the
/// mutation drain's failure count therefore arrive here, in a slot the runtime
/// and its production hooks share, and [`AccountRuntime::run_tick`] empties it
/// once per tick.
#[derive(Clone, Debug, Default)]
pub(super) struct TickReport {
    /// What the body's sync returned, `None` for a tick whose body is injected
    /// or failed before the engine ran.
    sync: Arc<Mutex<Option<SyncResult>>>,
    /// Mutations rolled back across both of the tick's drains.
    failed_mutations: Arc<AtomicU64>,
    /// Set by a mutation drain that was refused the engine turn with ops owed
    /// ([`crate::pending_ops::Resume::Busy`]); read by [`AccountRuntime::drain`].
    busy: Arc<AtomicBool>,
}

impl TickReport {
    /// Forget the previous tick's facts, so nothing leaks into this one.
    fn reset(&self) {
        *lock(&self.sync) = None;
        self.failed_mutations.store(0, Ordering::SeqCst);
        self.busy.store(false, Ordering::SeqCst);
    }

    /// Record that a mutation drain was refused its turn with ops owed.
    pub(super) fn mark_busy(&self) {
        self.busy.store(true, Ordering::SeqCst);
    }

    /// Everything this tick recorded, leaving the slot empty.
    fn take(&self) -> (SyncResult, u64) {
        (
            lock(&self.sync).take().unwrap_or_default(),
            self.failed_mutations.swap(0, Ordering::SeqCst),
        )
    }
}

/// One account's engine: the lock, the pool, and the tick.
pub struct AccountRuntime {
    /// The configured account name.
    account: String,
    /// The configuration it started with, which is what its mailbox
    /// catalogue resolves against (#0141): a swap that edits the account
    /// replaces the runtime.
    config: Arc<AccountConfig>,
    /// Resolved by `start` before it returned.
    readiness: Readiness,
    /// Held for the runtime's lifetime, `None` when another engine has it.
    /// Dropping the runtime releases it, and so does [`AccountRuntime::retire`],
    /// which cannot wait for every clone of the runtime to be dropped.
    engine_lock: Mutex<Option<EngineLock>>,
    /// `true` once [`AccountRuntime::retire`] has run: no tick starts after
    /// it, and the account's watcher stops at it. A dropped runtime drops the
    /// sender, which a watcher reads the same way.
    retired: watch::Sender<bool>,
    /// The reads this runtime serves, open whether or not the lock was taken.
    pool: ReadPool,
    /// What the tick runs.
    hooks: TickHooks,
    /// The account's body-fetch budget, `None` when configured as `0`.
    body_deadline: Option<Duration>,
    /// `Some` while a tick is running: its receiver resolves to that tick's
    /// outcome, which is what a joiner returns.
    running: Mutex<Option<watch::Receiver<Option<TickOutcome>>>>,
    /// The engine turn a tick's run and a drain both hold for their whole
    /// length, so neither runs beside the other (#0133).
    turn: tokio::sync::Mutex<()>,
    /// When the last tick this runtime ran finished, or when the runtime
    /// started if none has: what the scheduler measures its interval from
    /// (#0134). Set by the runner of every tick that ran a body, whatever
    /// asked for it; a joiner, a refused tick and a drain leave it alone.
    ticked: watch::Sender<tokio::time::Instant>,
    /// When the runtime started: `ticked`'s first value, kept so the
    /// scheduler can tell "no tick has finished yet" from a tick that
    /// finished before it first looked (PERSO-178).
    started: tokio::time::Instant,
    /// Whether the last tick that ran a body left a Graph folder with a
    /// backlog the next quick tick can drain ([`SyncResult::backlogged`]),
    /// which the scheduler reads to run that tick at once (PERSO-172). Set
    /// before `ticked` moves, so the scheduler woken by it reads this tick's
    /// answer.
    backlog: AtomicBool,
    /// Bumped by [`AccountRuntime::request_drain`], read by the drainer. A
    /// counter rather than a unit so a receiver created after the first
    /// request can still tell it was made.
    drain_requests: watch::Sender<u64>,
    /// Where the production hooks leave the engine's result. Empty for a
    /// runtime built with injected hooks, which report zeros.
    report: TickReport,
    /// The daemon's draft watcher, once the runtime table has handed it over:
    /// where the production hooks pass on the account's drafts mirror state
    /// after a drain or a tick that may have moved it (#0143).
    draft_watch: DraftWatchCell,
    /// The drafts mirror's reconcile on its own, without a sync (#0143), which
    /// [`AccountRuntime::reconcile_drafts`] runs under the turn. `None` for a
    /// runtime built with injected tick hooks and no drafts hook.
    drafts: Option<DraftsHook>,
}

/// The drafts reconcile [`AccountRuntime::reconcile_drafts`] runs: the
/// production one reads the server Drafts folder, a test's counts.
pub type DraftsHook = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// The draft watcher a runtime and its production hooks share (#0143), set
/// once by [`AccountRuntime::attach_draft_watch`].
pub(super) type DraftWatchCell = Arc<std::sync::OnceLock<Arc<crate::daemon::watch::DraftWatch>>>;

/// Which side of the join a [`AccountRuntime::tick`] call is on, decided under
/// the lock and acted on outside it.
enum Slot {
    /// This call starts the run and publishes its outcome.
    Run(watch::Sender<Option<TickOutcome>>),
    /// This call attaches to a run already in flight.
    Join(watch::Receiver<Option<TickOutcome>>),
}

/// Take a lock whose holder may have panicked. What is behind it is one
/// `Option`, which no panic can leave half-written.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl AccountRuntime {
    /// Start the runtime for `cfg`, against the account directory the
    /// environment resolves.
    pub fn start(cfg: AccountConfig, pool_size: usize) -> Result<Self> {
        let dir = crate::config::account_dir(&cfg.name);
        crate::config::create_private_dir_all(&dir)
            .with_context(|| format!("creating the account directory {}", dir.display()))?;
        Self::start_at(&dir, cfg, pool_size)
    }

    /// The mechanism, split out so a test can point it at a tempdir, exactly as
    /// [`EngineLock::try_acquire_at`] is.
    pub fn start_at(dir: &Path, cfg: AccountConfig, pool_size: usize) -> Result<Self> {
        let report = TickReport::default();
        let draft_watch = DraftWatchCell::default();
        let hooks = production_hooks(
            Arc::new(cfg.clone()),
            report.clone(),
            Arc::clone(&draft_watch),
        );
        let drafts = drafts_hook(Arc::new(cfg.clone()), Arc::clone(&draft_watch));
        let mut runtime = Self::start_at_with_hooks(dir, cfg, pool_size, hooks)?;
        runtime.report = report;
        runtime.draft_watch = draft_watch;
        runtime.drafts = Some(drafts);
        Ok(runtime)
    }

    /// The same, with the tick's three halves replaced.
    ///
    /// The only entry point a contract test uses: a body that hangs, fails or
    /// panics if it is entered is how the phase order, the join and the blocked
    /// degrade are observable without a server.
    pub fn start_at_with_hooks(
        dir: &Path,
        cfg: AccountConfig,
        pool_size: usize,
        hooks: TickHooks,
    ) -> Result<Self> {
        let account = cfg.name.clone();
        // The lock first, so a runtime that will report `Blocked` has already
        // learned it by the time the pool is open and it can say so.
        //
        // `hold_for_runtime` rather than `try_acquire_at` because this lock is
        // held for the runtime's whole lifetime (P5-U8): every guarded call
        // inside this process - the send path's outbox drain, a `mp sync`
        // routed here with a mailbox subset - then takes a turn at the
        // process's own gate instead of opening a second description that
        // contends with this one and refuses itself.
        let held = EngineLock::hold_for_runtime(&dir.join("store.lock"), &account)?;
        let readiness = match &held {
            Some(_) => {
                debug!("[daemon] {account} took the engine lock");
                Readiness::Ready
            }
            None => {
                let reason =
                    format!("another engine holds the lock for '{account}'; serving reads only");
                info!("[daemon] {reason}");
                Readiness::Blocked { reason }
            }
        };

        // Opened on both paths: a blocked runtime still serves reads, which is
        // the whole reason it is worth having.
        let pool = ReadPool::open(&dir.join("store.sqlite3"), pool_size)?;

        let started = tokio::time::Instant::now();
        Ok(AccountRuntime {
            account,
            config: Arc::new(cfg.clone()),
            readiness,
            engine_lock: Mutex::new(held),
            retired: watch::channel(false).0,
            pool,
            hooks,
            body_deadline: (cfg.imap.body_fetch_deadline_secs > 0)
                .then(|| Duration::from_secs(cfg.imap.body_fetch_deadline_secs)),
            running: Mutex::new(None),
            turn: tokio::sync::Mutex::new(()),
            ticked: watch::channel(started).0,
            started,
            backlog: AtomicBool::new(false),
            drain_requests: watch::channel(0).0,
            report: TickReport::default(),
            draft_watch: DraftWatchCell::default(),
            drafts: None,
        })
    }

    /// Install the drafts reconcile [`AccountRuntime::reconcile_drafts`]
    /// runs, as [`AccountRuntime::start_at`] installs the production one.
    pub fn with_drafts_hook(mut self, hook: DraftsHook) -> Self {
        self.drafts = Some(hook);
        self
    }

    /// Reconcile the account's drafts with its server Drafts folder now,
    /// without a sync (#0143): what the drafts poll runs between ticks, so a
    /// draft saved in Outlook or pushed by the other machine arrives within
    /// a minute rather than at the next sync tick.
    ///
    /// Under the turn, as a drain is: it waits for a tick or a drain in
    /// flight and never runs beside one. A blocked or retired runtime, or one
    /// with no drafts hook, refuses; the answer is whether it ran.
    pub async fn reconcile_drafts(&self) -> bool {
        if self.readiness != Readiness::Ready {
            return false;
        }
        let Some(hook) = self.drafts.clone() else {
            return false;
        };
        let _turn = self.turn.lock().await;
        if *self.retired.borrow() {
            return false;
        }
        hook().await;
        true
    }

    /// The turn, for a send of a mirrored draft (#0143): held from the claim
    /// of the draft's server copy to the end of its submission, so no drain
    /// pushes the draft back and no reconcile reads its missing copy in
    /// between. It waits for a tick or a drain in flight, as a drain does.
    /// `None` on a blocked or retired runtime, which runs neither.
    pub async fn send_turn(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        if self.readiness != Readiness::Ready {
            return None;
        }
        let turn = self.turn.lock().await;
        if *self.retired.borrow() {
            return None;
        }
        Some(turn)
    }

    /// The account this runtime serves.
    pub fn account(&self) -> &str {
        &self.account
    }

    /// The configuration this runtime started with.
    pub fn config(&self) -> &AccountConfig {
        &self.config
    }

    /// Whether this runtime can serve, resolved at start and fixed afterwards.
    pub fn readiness(&self) -> Readiness {
        self.readiness.clone()
    }

    /// A receiver that goes `true`, or closes, once this runtime has left the
    /// table: what ends the account's watcher.
    pub fn retired(&self) -> watch::Receiver<bool> {
        self.retired.subscribe()
    }

    /// A receiver holding when the last tick finished (the runtime's start
    /// until one has), which changes as each tick that ran a body finishes and
    /// closes when the runtime is dropped: what the scheduler waits on (#0134).
    pub fn completed_ticks(&self) -> watch::Receiver<tokio::time::Instant> {
        self.ticked.subscribe()
    }

    /// When this runtime started, which [`AccountRuntime::completed_ticks`]
    /// holds until a tick finishes: what the scheduler's startup tick is
    /// measured from (PERSO-178).
    pub fn started(&self) -> tokio::time::Instant {
        self.started
    }

    /// Whether the last tick left a backlog the next quick tick drains, so
    /// the scheduler runs that tick at once instead of an interval later
    /// (PERSO-172).
    pub fn backlog_owed(&self) -> bool {
        self.backlog.load(Ordering::SeqCst)
    }

    /// Record that a pass this runtime did not run, the daemon's first sync of
    /// a new account, finished with a backlog, and wake the scheduler so it
    /// starts draining it now rather than an interval after the runtime
    /// started.
    ///
    /// That pass ran a body for this account, so it moves
    /// [`AccountRuntime::completed_ticks`] exactly as a tick of the runtime's
    /// own would.
    pub fn note_backlog(&self) {
        self.backlog.store(true, Ordering::SeqCst);
        self.ticked.send_replace(tokio::time::Instant::now());
    }

    /// Hand the runtime the daemon's draft watcher (#0143) and pass it the
    /// account's drafts mirror state as the store holds it now, which is what
    /// gives the account's drafts their `server` member. Off the reactor,
    /// since it opens the store; a second call keeps the first watcher.
    pub fn attach_draft_watch(&self, watch: Arc<crate::daemon::watch::DraftWatch>) {
        if self.draft_watch.set(watch).is_err() {
            return;
        }
        let cfg = Arc::clone(&self.config);
        let cell = Arc::clone(&self.draft_watch);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn_blocking(move || publish_mirror_state(&cfg, &cell));
        } else {
            publish_mirror_state(&cfg, &cell);
        }
    }

    /// Ask for a drain of the outbox and the mutation queue soon (#0133).
    ///
    /// What a `settle: false` mutation calls once its row change and owed op
    /// are committed. It only records the request and returns, so the
    /// keystroke that caused it stays network-free; the drainer spawned for
    /// this runtime runs the drain once the requests stop for
    /// [`super::drainer::DRAIN_DEBOUNCE`]. A no-op on a blocked or retired
    /// runtime, which has no engine lock to drain under.
    pub fn request_drain(&self) {
        if self.readiness != Readiness::Ready || *self.retired.borrow() {
            return;
        }
        self.drain_requests
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// A receiver that changes on every [`AccountRuntime::request_drain`], and
    /// closes when the runtime is dropped: what the drainer waits on.
    ///
    /// A request made before this was called is not lost: the receiver is
    /// marked changed when any request has ever been made.
    pub fn drain_requests(&self) -> watch::Receiver<u64> {
        let mut receiver = self.drain_requests.subscribe();
        if *receiver.borrow() != 0 {
            receiver.mark_changed();
        }
        receiver
    }

    /// Take this runtime out of service ahead of its drop: refuse every tick
    /// from here on, wait up to `bound` for the one running to finish, then
    /// release the engine lock.
    ///
    /// A configuration swap that changes an account starts the replacement
    /// the moment this returns, and the replacement takes the same
    /// `store.lock`. Dropping the table's clone is not enough for that: a tick
    /// in flight holds a clone of its own across its whole run, and the lock
    /// would still be held when the replacement asks, leaving it blocked for
    /// good (readiness is fixed at start).
    ///
    /// `false` when the running tick, or a drain in flight, outlived `bound`;
    /// the lock then stays with it and comes free when its last clone drops.
    pub async fn retire(&self, bound: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + bound;
        // Under the tick's own lock, so no `tick()` can take the run slot
        // between the flag and the capture of the run in flight.
        let running = {
            let running = lock(&self.running);
            self.retired.send_replace(true);
            running.clone()
        };
        let overran = || {
            warn!(
                "[daemon] {}'s tick or drain was still running after {}s; its engine lock \
                 is released when it ends",
                self.account,
                bound.as_secs()
            );
            false
        };
        if let Some(mut receiver) = running {
            // A closed channel is a runner that ended without reporting, which
            // is over just the same.
            let finished =
                tokio::time::timeout_at(deadline, receiver.wait_for(Option::is_some)).await;
            if finished.is_err() {
                return overran();
            }
        }
        // A drain holds the turn without the run slot (#0133), so it is waited
        // out here. Anything that takes the turn after this sees the flag and
        // refuses, so the turn is released as soon as it is had.
        if tokio::time::timeout_at(deadline, self.turn.lock())
            .await
            .is_err()
        {
            return overran();
        }
        drop(lock(&self.engine_lock).take());
        true
    }

    /// Check a read connection out of the pool, waiting until one is free.
    pub fn read_conn(&self) -> PooledRead {
        self.pool.get()
    }

    /// Run one tick, or attach to the one already running.
    pub async fn tick(&self, kind: TickKind) -> TickOutcome {
        self.tick_until(kind, None).await
    }

    /// [`AccountRuntime::tick`], whose body asks `stop` at every fetch chunk
    /// and mailbox boundary when this call is the one that runs it
    /// (PERSO-127).
    ///
    /// A call that joins a tick already in flight does not stop it: that tick
    /// is another caller's work, and its `stop` (or its lack of one) is the
    /// one it runs under. A tick that is stopped ends with what it ingested
    /// before the stop, and a caller that joined it is handed that outcome.
    pub async fn tick_until(&self, kind: TickKind, stop: Option<Stop>) -> TickOutcome {
        if self.readiness != Readiness::Ready {
            // No phase, no hook, no login: the holder is doing the work.
            debug!("[daemon] {} refused a {kind:?} tick: blocked", self.account);
            return TickOutcome {
                kind,
                phases: Vec::new(),
                joined: false,
                blocked: true,
                body_deadline: self.body_deadline,
                status: String::new(),
                error: None,
                sync: None,
                contacts: None,
            };
        }

        // Decided under the lock and acted on after it, so no guard is alive
        // across the join's await.
        let slot = {
            let mut running = lock(&self.running);
            if *self.retired.borrow() && running.is_none() {
                // Retired: the lock is gone or about to go, and the runtime
                // replacing this one is the engine now.
                debug!("[daemon] {} refused a {kind:?} tick: retired", self.account);
                return TickOutcome {
                    kind,
                    phases: Vec::new(),
                    joined: false,
                    blocked: true,
                    body_deadline: self.body_deadline,
                    status: String::new(),
                    error: None,
                    sync: None,
                    contacts: None,
                };
            }
            match running.as_ref() {
                Some(receiver) => Slot::Join(receiver.clone()),
                None => {
                    let (sender, receiver) = watch::channel(None);
                    *running = Some(receiver);
                    Slot::Run(sender)
                }
            }
        };

        let sender = match slot {
            Slot::Join(mut receiver) => {
                // The slot is cleared before the outcome is published, so a
                // closed channel here can only mean the runner panicked.
                loop {
                    let published = receiver.borrow_and_update().clone();
                    if let Some(mut outcome) = published {
                        outcome.joined = true;
                        // The runner commits the tick's outcome; a joiner
                        // reports the same tick and must not commit it twice.
                        outcome.sync = None;
                        outcome.contacts = None;
                        return outcome;
                    }
                    if receiver.changed().await.is_err() {
                        warn!(
                            "[daemon] {} joined a tick that never reported",
                            self.account
                        );
                        return TickOutcome {
                            kind,
                            phases: Vec::new(),
                            joined: true,
                            blocked: false,
                            body_deadline: self.body_deadline,
                            status: String::new(),
                            error: Some(
                                "the tick this call joined ended without an outcome".to_string(),
                            ),
                            sync: None,
                            contacts: None,
                        };
                    }
                }
            }
            Slot::Run(sender) => sender,
        };

        let outcome = {
            // A drain in flight holds the turn; this run waits for it (#0133).
            let _turn = self.turn.lock().await;
            if *self.retired.borrow() {
                // Retired while it waited: the engine lock is about to go.
                debug!("[daemon] {} dropped a {kind:?} tick: retired", self.account);
                TickOutcome {
                    kind,
                    phases: Vec::new(),
                    joined: false,
                    blocked: true,
                    body_deadline: self.body_deadline,
                    status: String::new(),
                    error: None,
                    sync: None,
                    contacts: None,
                }
            } else {
                let outcome = self.run_tick(kind, stop).await;
                // Whatever asked for it: a manual sync, the watcher, a
                // client's startup fetch or the scheduler all push the next
                // scheduled tick back (#0134).
                self.ticked.send_replace(tokio::time::Instant::now());
                outcome
            }
        };
        // Cleared first: the next `tick()` after this one returns must start a
        // fresh body rather than join a run that is over.
        *lock(&self.running) = None;
        let _ = sender.send(Some(outcome.clone()));
        outcome
    }

    /// Drain the outbox and then the mutation queue, outside any tick (#0133).
    ///
    /// The tick's drain without its body, through the same two hooks and under
    /// the same turn, so it waits for a tick in flight rather than running
    /// beside it. It runs no sync. A blocked runtime refuses at once, and a
    /// retired one refuses once it has the turn; neither enters a slot, and a
    /// queued op they leave stays queued for the next tick.
    pub async fn drain(&self) -> DrainOutcome {
        if self.readiness != Readiness::Ready {
            debug!("[daemon] {} refused a drain: blocked", self.account);
            return DrainOutcome::default();
        }
        let _turn = self.turn.lock().await;
        if *self.retired.borrow() {
            debug!("[daemon] {} refused a drain: retired", self.account);
            return DrainOutcome::default();
        }
        self.report.reset();
        let context = |phase: Phase| TickContext {
            account: self.account.clone(),
            kind: TickKind::Quick,
            phase,
            body_deadline: self.body_deadline,
            stop: None,
        };
        // The tick's order, for the tick's reason: the outbox first.
        let outbox = (self.hooks.outbox)(context(Phase::DrainOutbox)).await;
        let mutations = (self.hooks.mutations)(context(Phase::DrainMutations)).await;
        let busy = self.report.busy.swap(false, Ordering::SeqCst);
        let (_, failed_mutations) = self.report.take();
        DrainOutcome {
            ran: true,
            status: format!("{outbox}{mutations}"),
            failed_mutations,
            busy,
        }
    }

    /// Install the report the test's hooks write into, as the production
    /// constructor installs the one its hooks share.
    #[cfg(test)]
    pub(super) fn with_report(mut self, report: TickReport) -> Self {
        self.report = report;
        self
    }

    /// The tick itself: [`run_tick_with_drains`] over the installed hooks.
    async fn run_tick(&self, kind: TickKind, stop: Option<Stop>) -> TickOutcome {
        self.report.reset();
        let phases = Mutex::new(Vec::with_capacity(5));
        let context = |phase: Phase| {
            lock(&phases).push(phase);
            TickContext {
                account: self.account.clone(),
                kind,
                phase,
                body_deadline: self.body_deadline,
                // The body's alone: the drains finish work the user already
                // committed to (a send, a queued mutation), which a cancelled
                // sync has no business leaving half done.
                stop: (phase == Phase::Body).then(|| stop.clone()).flatten(),
            }
        };
        // The order inside a drain is the invariant: the outbox first, so a
        // Sent copy that failed to APPEND is retried before the mutation queue
        // touches the same message.
        let head = || async {
            let outbox = (self.hooks.outbox)(context(Phase::HeadOutbox)).await;
            let mutations = (self.hooks.mutations)(context(Phase::HeadMutations)).await;
            format!("{outbox}{mutations}")
        };
        let tail = || async {
            let outbox = (self.hooks.outbox)(context(Phase::TailOutbox)).await;
            let mutations = (self.hooks.mutations)(context(Phase::TailMutations)).await;
            format!("{outbox}{mutations}")
        };

        let (status, result) =
            run_tick_with_drains(head, || (self.hooks.body)(context(Phase::Body)), tail).await;

        let entered = lock(&phases).clone();
        let error = result.err().map(|e| format!("{e:#}"));
        let (mut sync_result, failed_mutations) = self.report.take();
        let contacts = self
            .index_contacts(std::mem::take(&mut sync_result.fresh_observations))
            .await;
        // A failed body owes nothing: whatever failed (the network, the
        // ceiling, a refused token) is what an immediate retry would hit again,
        // so the regular interval paces it.
        self.backlog.store(
            error.is_none() && !sync_result.backlogged.is_empty(),
            Ordering::SeqCst,
        );
        TickOutcome {
            kind,
            phases: entered,
            joined: false,
            blocked: false,
            body_deadline: self.body_deadline,
            status,
            sync: Some(from_sync_result(
                &self.account,
                &sync_result,
                failed_mutations,
                error.clone(),
            )),
            error,
            contacts,
        }
    }

    /// Fold the correspondents a tick's pass inserted into the account's
    /// contact index, through the hook the guarded pass and the first sync
    /// use, and answer how many contacts the index then holds.
    ///
    /// The tick is how a running daemon syncs, and until PERSO-234 it was the
    /// one pass that dropped its observations, so a correspondent first seen
    /// in a scheduled or a watcher tick stayed unsearchable until a rebuild.
    /// The cache read and write are blocking, so they run off the reactor; the
    /// account directory is resolved here first, on the caller's thread.
    async fn index_contacts(&self, observations: Vec<FreshObservation>) -> Option<usize> {
        if observations.is_empty() {
            return None;
        }
        let root = crate::config::account_dir(&self.account);
        let cfg = Arc::clone(&self.config);
        tokio::task::spawn_blocking(move || {
            crate::contacts::hooks::bump_after_sync_at(&root, &cfg, &observations)
        })
        .await
        .unwrap_or_else(|e| {
            warn!("[daemon] the contacts bump of {} {e}", self.account);
            None
        })
    }
}

impl std::fmt::Debug for AccountRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountRuntime")
            .field("account", &self.account)
            .field("readiness", &self.readiness)
            .field("pool", &self.pool)
            .field("body_deadline", &self.body_deadline)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// The production hooks
// ---------------------------------------------------------------------------

/// The real drains and the real sync, which is what [`AccountRuntime::start`]
/// and [`AccountRuntime::start_at`] install.
fn production_hooks(
    cfg: Arc<AccountConfig>,
    report: TickReport,
    watch: DraftWatchCell,
) -> TickHooks {
    TickHooks {
        outbox: outbox_hook(Arc::clone(&cfg)),
        mutations: mutations_hook(Arc::clone(&cfg), report.clone(), Arc::clone(&watch)),
        body: body_hook(cfg, report, watch),
    }
}

/// The account's drafts mirror state as its store holds it (#0143): whether
/// it mirrors, each draft's `draft_sync` row, and the drafts whose push was
/// parked `failed`. An account with no store yet mirrors nothing so far.
pub(crate) fn mirror_state(cfg: &AccountConfig) -> Result<crate::daemon::watch::MirrorState> {
    let mut state = crate::daemon::watch::MirrorState {
        enabled: crate::draft_mirror::mirror_enabled(cfg),
        ..Default::default()
    };
    let path = crate::config::store_path(&cfg.name);
    if !state.enabled || !path.exists() {
        return Ok(state);
    }
    let store = crate::store::Store::open(&path)?;
    state.rows = crate::store::draft_sync::load(&store, &cfg.name)?
        .into_iter()
        .map(|row| (row.draft_id, (row.synced_hash, row.synced_at)))
        .collect();
    state.failed = crate::pending_ops::failed_draft_pushes(&store, &cfg.name)?
        .into_iter()
        .collect();
    Ok(state)
}

/// Read the account's drafts mirror state and pass it to the draft watcher,
/// when the runtime has one. Logged and skipped on failure: the watcher then
/// keeps the state it had, and the next drain tries again.
fn publish_mirror_state(cfg: &AccountConfig, cell: &DraftWatchCell) {
    let Some(watch) = cell.get() else {
        return;
    };
    match mirror_state(cfg) {
        Ok(state) => watch.set_mirror(&cfg.name, state),
        Err(e) => warn!(
            "[drafts] reading the mirror state of {} failed: {e:#}",
            cfg.name
        ),
    }
}

/// The outbox drain (#0037 item 5): a Sent copy that could not be appended when
/// the message was sent is retried on the tick.
///
/// Silent by design, so it adds nothing to the status text: it only finishes
/// work the user already saw succeed. A clean account costs one `COUNT`.
fn outbox_hook(cfg: Arc<AccountConfig>) -> DrainHook {
    Arc::new(move |ctx: TickContext| {
        let cfg = Arc::clone(&cfg);
        async move {
            if let Err(e) = off_thread("the outbox drain", move || async move {
                crate::send::resume_outbox(&cfg).await;
            })
            .await
            {
                warn!(
                    "[outbox] draining {} at {:?} failed: {e:#}",
                    ctx.account, ctx.phase
                );
            }
            String::new()
        }
        .boxed()
    })
}

/// The mutation-queue drain (#0039): archive, delete, move and flag toggles
/// enqueued locally are retired under the engine lock this runtime holds.
///
/// A drained op is silent; a failed one has already been rolled back, so the
/// suffix points at the log rather than repeating the per-op error. The wording
/// is the TUI's, so the same tick reads the same way in both clients.
fn mutations_hook(cfg: Arc<AccountConfig>, report: TickReport, watch: DraftWatchCell) -> DrainHook {
    Arc::new(move |ctx: TickContext| {
        let cfg = Arc::clone(&cfg);
        let report = report.clone();
        let watch = Arc::clone(&watch);
        async move {
            let drained = off_thread("the mutation-queue drain", move || async move {
                let drained = crate::pending_ops::resume_account_or_busy(&cfg).await;
                // A push or a delete of a draft moves the mirror state the
                // watcher shows (#0143).
                if matches!(drained, Ok(crate::pending_ops::Resume::Drained(_))) {
                    publish_mirror_state(&cfg, &watch);
                }
                drained
            })
            .await
            .and_then(|inner| inner);
            match drained {
                Ok(crate::pending_ops::Resume::Busy) => {
                    // Ops are owed and another pass holds the turn. A tick
                    // ignores this (its next run drains them); the debounced
                    // drainer retries (#0133).
                    debug!(
                        "[pending_ops] {} was refused the engine turn at {:?}; ops stay queued",
                        ctx.account, ctx.phase
                    );
                    report.mark_busy();
                    String::new()
                }
                Ok(crate::pending_ops::Resume::Drained(drained)) if drained.failed > 0 => {
                    // Added rather than stored: a tick drains twice, and the
                    // outcome reports what the whole tick rolled back.
                    report
                        .failed_mutations
                        .fetch_add(drained.failed as u64, Ordering::SeqCst);
                    format!(
                        "; {} mutation(s) failed and were rolled back (see the log)",
                        drained.failed
                    )
                }
                Ok(_) => String::new(),
                Err(e) => {
                    warn!(
                        "[pending_ops] draining {} at the {:?} phase failed: {e:#}",
                        ctx.account, ctx.phase
                    );
                    String::new()
                }
            }
        }
        .boxed()
    })
}

/// The sync itself, against the account's real IMAP server or Graph mailbox.
///
/// The backend is built inside the hook rather than at start: it needs
/// [`crate::config::ImapConfig::load`], which reads the keyring, or the Graph
/// token cache, and an account
/// whose credentials are missing must still be a runtime that serves reads and
/// drains queues. A failed load is the body's `Err`, which the tail drain runs
/// after and the outcome carries.
fn body_hook(cfg: Arc<AccountConfig>, report: TickReport, watch: DraftWatchCell) -> BodyHook {
    // Whether this runtime has listed the server's folders yet: the first tick
    // lists, and so does every full one (#0141).
    let listed = Arc::new(AtomicBool::new(false));
    Arc::new(move |ctx: TickContext| {
        let cfg = Arc::clone(&cfg);
        let report = report.clone();
        let listed = Arc::clone(&listed);
        let watch = Arc::clone(&watch);
        async move {
            let result = off_thread("the sync body", move || async move {
                let synced = sync_once(&cfg, &ctx, &listed).await;
                // The drafts mirror's reconcile (#0143), after the sync and
                // whatever it did: the Drafts folder is read on its own, so a
                // failed sync of the mail folders does not hold it back, and
                // a failed reconcile is logged rather than failing the tick.
                if cfg.auth_method != crate::config::AuthMethod::Graph {
                    if let Err(e) = crate::draft_mirror::reconcile_account(&cfg).await {
                        warn!(
                            "[drafts] reconciling the drafts of {} failed: {e:#}",
                            cfg.name
                        );
                    }
                    publish_mirror_state(&cfg, &watch);
                }
                synced
            })
            .await
            .and_then(|inner| inner)?;
            *lock(&report.sync) = Some(result);
            Ok(())
        }
        .boxed()
    })
}

/// The drafts reconcile on its own (#0143): the body hook's reconcile without
/// the sync before it, off the reactor for the same reason.
fn drafts_hook(cfg: Arc<AccountConfig>, watch: DraftWatchCell) -> DraftsHook {
    Arc::new(move || {
        let cfg = Arc::clone(&cfg);
        let watch = Arc::clone(&watch);
        async move {
            let ran = off_thread("the drafts reconcile", move || async move {
                if let Err(e) = crate::draft_mirror::reconcile_account(&cfg).await {
                    warn!(
                        "[drafts] reconciling the drafts of {} failed: {e:#}",
                        cfg.name
                    );
                }
                publish_mirror_state(&cfg, &watch);
            })
            .await;
            if let Err(e) = ran {
                warn!("[drafts] {e:#}");
            }
        }
        .boxed()
    })
}

/// The runtime the production hooks `block_on`, built on first use and kept for
/// the process lifetime.
///
/// One shared multi-thread runtime rather than one per call, for the reason
/// `src/tui/runtime.rs` records (#0095): building and tearing a runtime down
/// per background action costs a worker thread per core every time.
static HOOK_RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> =
    std::sync::LazyLock::new(|| {
        tokio::runtime::Runtime::new().expect("building the daemon tick's runtime")
    });

/// Drive a future that is not `Send` on a thread of its own, and await its
/// result here.
///
/// Every production hook needs this. The store paths hold a `&Store` across
/// their awaits and `rusqlite::Connection` is not `Sync`, so the futures
/// [`crate::send::resume_outbox`], [`crate::pending_ops::resume_account`] and
/// [`crate::sync::engine::run_sync`] return are not `Send` and cannot be a
/// [`BoxFuture`]. `block_on` is called from a plain thread, never from a tokio
/// worker, so it cannot nest one runtime inside another.
pub(crate) async fn off_thread<T, Fut>(
    label: &'static str,
    make: impl FnOnce() -> Fut + Send + 'static,
) -> Result<T>
where
    Fut: std::future::Future<Output = T>,
    T: Send + 'static,
{
    let (done, wait) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = done.send(HOOK_RUNTIME.block_on(make()));
    });
    wait.await
        .with_context(|| format!("{label} ended without reporting an outcome"))
}

/// How many new messages per mailbox a tick of `kind` downloads, on either
/// transport.
fn tick_limit(kind: TickKind) -> usize {
    match kind {
        TickKind::Quick => QUICK_TICK_LIMIT,
        TickKind::Full => usize::MAX,
    }
}

/// Bring the account's folder list up to date before a tick picks its
/// targets (#0141): the server is listed on the runtime's first tick and on
/// every full one, so a folder created since appears within a full-sync
/// interval, and a quick tick costs no `LIST`.
///
/// A failed listing is logged and the tick syncs what the stored listing
/// holds: the folders the account had a tick ago, or its configured
/// mailboxes when it was never listed. It is never the tick's error, since
/// the mail it can reach is still worth fetching.
async fn refresh_catalogue(cfg: &AccountConfig, kind: TickKind, listed: &AtomicBool) {
    if kind == TickKind::Quick && listed.load(Ordering::SeqCst) {
        return;
    }
    match crate::catalogue::discover::refresh(cfg).await {
        Ok(_) => listed.store(true, Ordering::SeqCst),
        Err(e) => warn!(
            "[catalogue] listing the folders of {} failed; this tick syncs the ones it knew: {e:#}",
            cfg.name
        ),
    }
}

/// Read the server's counts of the `status-only` folders after a tick, which
/// is what their sidebar rows show. Logged and skipped on failure, like the
/// listing.
async fn refresh_status_counts(cfg: &AccountConfig) {
    if let Err(e) = crate::catalogue::discover::refresh_counts(cfg).await {
        warn!(
            "[catalogue] counting the folders of {} failed: {e:#}",
            cfg.name
        );
    }
}

/// One sync pass over every folder of the catalogue a tick syncs.
async fn sync_once(
    cfg: &AccountConfig,
    ctx: &TickContext,
    listed: &AtomicBool,
) -> Result<SyncResult> {
    refresh_catalogue(cfg, ctx.kind, listed).await;
    let targets = crate::catalogue::Catalogue::load(cfg).targets();
    let result = if cfg.auth_method == crate::config::AuthMethod::Graph {
        sync_once_graph(cfg, ctx, &targets).await
    } else {
        sync_once_imap(cfg, ctx, &targets).await
    };
    refresh_status_counts(cfg).await;
    result
}

/// The IMAP body of a tick: the engine over the account's targets.
async fn sync_once_imap(
    cfg: &AccountConfig,
    ctx: &TickContext,
    targets: &[crate::sync::SyncTarget],
) -> Result<SyncResult> {
    use crate::config::ImapConfig;
    use crate::store::{BlobStore, Store};
    use crate::sync::engine::{run_sync, SyncRun};

    let imap_config = ImapConfig::load(cfg)?;

    // The writer connection, and it is not one of the pool's: a sync
    // transaction must never occupy a read slot
    // (docs/baselines/decisions/read-pool.md).
    let store = Store::open(crate::config::store_path(&cfg.name))?;
    let blobs = BlobStore::for_account(&cfg.name);
    let mut backend = crate::imap_client::ImapBackend::new(&imap_config)
        .with_body_budget(ctx.body_deadline)
        .with_stop(ctx.stop.clone());
    let mut span = crate::timing::TimingSpan::with_context("daemon_tick", cfg.name.clone());

    // Unguarded on purpose: this runtime is holding `store.lock` already. See
    // the module docs.
    let result = run_sync(
        &mut backend,
        &SyncRun {
            store: &store,
            blobs: &blobs,
            account: &cfg.name,
            targets,
            limit: tick_limit(ctx.kind),
            dry_run: false,
            stop: ctx.stop.as_ref(),
        },
        &mut span,
    )
    .await?;
    Ok(result)
}

/// The Graph body of a tick: the same orchestrator `mp sync --mailbox` runs
/// (`graph::sync_mailboxes_graph`), over every configured mailbox.
///
/// A quick tick is bounded at [`QUICK_TICK_LIMIT`] new messages per mailbox,
/// which is also what lets it walk `/messages/delta` from the stored token
/// (#0042); a full tick relists every folder and re-mints the token. Throttling
/// is the orchestrator's: a 429 or 503 sub-response's `Retry-After` paces the
/// batch download, and a pass the throttle runs out of budget defers its prune
/// to the next tick.
///
/// The body-fetch deadline (#0113) is not a budget the Graph pass can spend
/// the way IMAP does, since a Graph target fetches and ingests in one step and
/// the stop is honoured only at each mailbox boundary. It sizes a quick
/// pass's ceiling instead ([`graph_tick_ceiling`]): a quick pass that
/// outlives it is cut off and reported as the tick's error, so a request that
/// never returns cannot hold the run slot and starve every later tick.
/// Cutting it off is safe at any await point, because every store write in
/// the pass (an ingest, a flag update, a repair, a cursor, a prune) is a
/// synchronous transaction between two awaits; the next tick picks up from
/// what the store holds.
///
/// A full tick has no ceiling. It downloads every new message of a folder
/// before ingesting any of them, so a cut-off full pass would discard the
/// whole download and the next full sync would restart from scratch and be
/// cut off again. The Graph client's connect and idle-read timeouts already
/// bound each request, which is what frees the slot on a dead connection.
///
/// The orchestrator opens its own writer connection, unguarded, like the IMAP
/// engine above: this runtime already holds the account's engine lock.
async fn sync_once_graph(
    cfg: &AccountConfig,
    ctx: &TickContext,
    targets: &[crate::sync::SyncTarget],
) -> Result<SyncResult> {
    let graph = crate::config::GraphConfig::load(cfg)?;
    within_ceiling(
        graph_tick_ceiling(ctx.kind, ctx.body_deadline),
        crate::graph::sync_mailboxes_graph(
            &graph,
            &cfg.name,
            targets,
            tick_limit(ctx.kind),
            false,
            ctx.stop.as_ref(),
        ),
    )
    .await
}

/// How many body-fetch deadlines a quick Graph tick may last: ten minutes at
/// the default thirty seconds, far more than its [`QUICK_TICK_LIMIT`] messages
/// per mailbox need, while a pass stuck on a dead connection still frees the
/// run slot.
const GRAPH_TICK_CEILING_FACTOR: u32 = 20;

/// The longest a Graph tick may run: `None` for a full tick, which must be
/// allowed to finish however large the mailbox, and when the body-fetch
/// deadline is configured unbounded (`0`). The HTTP client's own connect and
/// idle-read timeouts still bound each request then.
fn graph_tick_ceiling(kind: TickKind, body_deadline: Option<Duration>) -> Option<Duration> {
    match kind {
        TickKind::Quick => body_deadline.map(|deadline| deadline * GRAPH_TICK_CEILING_FACTOR),
        TickKind::Full => None,
    }
}

/// Run `pass`, giving up on it once `ceiling` has passed.
async fn within_ceiling<F>(ceiling: Option<Duration>, pass: F) -> Result<SyncResult>
where
    F: std::future::Future<Output = Result<SyncResult>>,
{
    let Some(ceiling) = ceiling else {
        return pass.await;
    };
    match tokio::time::timeout(ceiling, pass).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "the Graph sync pass ran past its {}s ceiling and was cut off; \
             the next tick resumes from what it stored",
            ceiling.as_secs()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runtime built with hooks that record nothing, for the cases that only
    /// need a started runtime.
    fn quiet_hooks() -> TickHooks {
        let quiet: DrainHook = Arc::new(|_ctx| async { String::new() }.boxed());
        TickHooks {
            outbox: Arc::clone(&quiet),
            mutations: quiet,
            body: Arc::new(|_ctx| async { Ok(()) }.boxed()),
        }
    }

    fn config(name: &str, deadline_secs: u64) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            imap: crate::config::ImapSettings {
                body_fetch_deadline_secs: deadline_secs,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn tick_context(kind: TickKind) -> TickContext {
        TickContext {
            account: "evoqs".to_string(),
            kind,
            phase: Phase::Body,
            body_deadline: Some(Duration::from_secs(30)),
            stop: None,
        }
    }

    fn graph_config() -> AccountConfig {
        let mut cfg = AccountConfig {
            name: "evoqs".to_string(),
            auth_method: crate::config::AuthMethod::Graph,
            ..Default::default()
        };
        let mapping = |server: &str| crate::config::MailboxMapping {
            server: server.to_string(),
        };
        cfg.mailboxes.inbox = Some(mapping("Inbox"));
        cfg.mailboxes.sent = Some(mapping("Sent Items"));
        cfg.mailboxes.extra = Some(vec![mapping("Junk Email")]);
        cfg
    }

    /// A Graph account's tick body takes the Graph backend rather than
    /// refusing (the gap #0134 left): with no `[accounts.oauth2]` it fails on
    /// the Graph configuration, before any IMAP load and before any network.
    #[tokio::test]
    async fn a_graph_tick_body_runs_the_graph_backend() {
        for kind in [TickKind::Quick, TickKind::Full] {
            let err = sync_once(&graph_config(), &tick_context(kind), &AtomicBool::new(true))
                .await
                .expect_err("no oauth2 settings, so the Graph config cannot load");
            let text = format!("{err:#}");
            assert!(
                text.contains("Graph auth_method requires [accounts.oauth2]"),
                "{text}"
            );
            assert!(!text.contains("no Graph backend"), "{text}");
        }
    }

    /// A quick Graph pass that never returns (a request on a dead
    /// connection) is cut off at its ceiling, twenty body-fetch deadlines,
    /// and reported as an error; a pass that finishes in time is returned
    /// untouched, and an unbounded deadline imposes no ceiling.
    #[tokio::test]
    async fn a_graph_pass_that_hangs_is_cut_off_at_its_ceiling() {
        assert_eq!(
            graph_tick_ceiling(TickKind::Quick, Some(Duration::from_secs(30))),
            Some(Duration::from_secs(600))
        );
        assert_eq!(graph_tick_ceiling(TickKind::Quick, None), None);

        let started = tokio::time::Instant::now();
        let err = within_ceiling(
            Some(Duration::from_millis(50)),
            std::future::pending::<Result<SyncResult>>(),
        )
        .await
        .expect_err("a hung pass is cut off");
        assert!(format!("{err:#}").contains("cut off"), "{err:#}");
        assert!(started.elapsed() >= Duration::from_millis(50));

        let done = within_ceiling(Some(Duration::from_secs(600)), async {
            Ok(SyncResult {
                saved: 3,
                ..Default::default()
            })
        })
        .await
        .expect("a pass in time is returned");
        assert_eq!(done.saved, 3);

        let unbounded = within_ceiling(None, async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(SyncResult::default())
        })
        .await;
        assert!(unbounded.is_ok(), "no ceiling, no cut-off");
    }

    /// A full Graph tick has no ceiling: a pass that outlasts twenty
    /// body-fetch deadlines still runs to completion, because cutting it off
    /// would discard every message it downloaded before ingesting.
    #[tokio::test]
    async fn a_full_graph_pass_is_not_cut_off() {
        let deadline = Some(Duration::from_millis(1));
        assert_eq!(graph_tick_ceiling(TickKind::Full, deadline), None);
        assert_eq!(graph_tick_ceiling(TickKind::Full, None), None);

        let done = within_ceiling(graph_tick_ceiling(TickKind::Full, deadline), async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(SyncResult {
                saved: 7,
                ..Default::default()
            })
        })
        .await
        .expect("a full pass runs to completion");
        assert_eq!(done.saved, 7);
    }

    /// A tick whose body hit the ceiling frees the run slot: the next tick
    /// runs its body rather than joining a pass that never ends.
    #[tokio::test]
    async fn a_tick_cut_off_at_its_ceiling_lets_the_next_tick_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entered = Arc::new(AtomicU64::new(0));
        let mut hooks = quiet_hooks();
        hooks.body = {
            let entered = Arc::clone(&entered);
            Arc::new(move |_ctx: TickContext| {
                entered.fetch_add(1, Ordering::SeqCst);
                async move {
                    within_ceiling(
                        Some(Duration::from_millis(50)),
                        std::future::pending::<Result<SyncResult>>(),
                    )
                    .await
                    .map(|_| ())
                }
                .boxed()
            })
        };
        let runtime = AccountRuntime::start_at_with_hooks(dir.path(), config("alpha", 1), 1, hooks)
            .expect("a free lock");

        for expected in 1..=2 {
            let outcome = runtime.tick(TickKind::Quick).await;
            assert!(!outcome.joined, "tick {expected} ran its own body");
            assert!(
                outcome
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("cut off"),
                "{:?}",
                outcome.error
            );
            assert_eq!(outcome.phases.len(), 5, "the tail still ran");
            assert_eq!(entered.load(Ordering::SeqCst), expected);
        }
        assert_eq!(runtime.readiness(), Readiness::Ready);
    }

    /// Both transports tick over every configured mailbox when the server was
    /// never listed, with the quick bound on a quick tick and none on a full
    /// one.
    #[test]
    fn a_tick_covers_every_configured_mailbox_with_its_kinds_bound() {
        let targets = crate::catalogue::Catalogue::resolve(&graph_config(), &[]).targets();
        let servers: Vec<&str> = targets.iter().map(|t| t.server_name.as_str()).collect();
        assert_eq!(servers, vec!["Inbox", "Sent Items", "Junk Email"]);
        assert_eq!(tick_limit(TickKind::Quick), QUICK_TICK_LIMIT);
        assert_eq!(tick_limit(TickKind::Full), usize::MAX);
    }

    /// The five slots, in the only order #0114 allows, and the status text is
    /// the four suffixes concatenated.
    #[tokio::test]
    async fn a_tick_enters_the_five_slots_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime =
            AccountRuntime::start_at_with_hooks(dir.path(), config("alpha", 9), 1, quiet_hooks())
                .expect("a free lock");

        assert_eq!(runtime.readiness(), Readiness::Ready);
        let outcome = runtime.tick(TickKind::Quick).await;
        assert_eq!(
            outcome.phases,
            vec![
                Phase::HeadOutbox,
                Phase::HeadMutations,
                Phase::Body,
                Phase::TailOutbox,
                Phase::TailMutations,
            ]
        );
        assert_eq!(outcome.body_deadline, Some(Duration::from_secs(9)));
        assert_eq!(outcome.status, "");
        assert_eq!(outcome.error, None);
        assert!(!outcome.blocked && !outcome.joined);
    }

    /// A body that fails is reported on the outcome, and the tail still ran.
    #[tokio::test]
    async fn a_failing_body_is_carried_and_the_tail_still_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut hooks = quiet_hooks();
        hooks.body = Arc::new(|_ctx| async { Err(anyhow::anyhow!("login refused")) }.boxed());
        let runtime = AccountRuntime::start_at_with_hooks(dir.path(), config("alpha", 0), 1, hooks)
            .expect("a free lock");

        let outcome = runtime.tick(TickKind::Full).await;
        assert_eq!(outcome.error.as_deref(), Some("login refused"));
        assert_eq!(outcome.phases.len(), 5);
        assert_eq!(outcome.body_deadline, None, "0 seconds is unbounded");
    }

    /// PERSO-172: a tick whose pass left a backlog says so until a tick drains
    /// it, a failed body owes nothing whatever it reported, and the first
    /// sync's backlog is noted from outside and moves the tick record.
    #[tokio::test]
    async fn a_tick_that_left_a_backlog_owes_the_next_one_until_it_is_drained() {
        let dir = tempfile::tempdir().expect("tempdir");
        let report = TickReport::default();
        // What each tick's pass reports: a backlog or none, and whether the
        // body then failed.
        let script = Arc::new(Mutex::new(vec![
            (true, false),
            (true, false),
            (false, false),
            (true, true),
        ]));
        let mut hooks = quiet_hooks();
        hooks.body = {
            let report = report.clone();
            let script = Arc::clone(&script);
            Arc::new(move |_ctx: TickContext| {
                let (backlog, fails) = lock(&script).remove(0);
                *lock(&report.sync) = Some(SyncResult {
                    backlogged: if backlog {
                        vec!["Inbox".to_string()]
                    } else {
                        Vec::new()
                    },
                    ..Default::default()
                });
                async move {
                    if fails {
                        Err(anyhow::anyhow!("throttled"))
                    } else {
                        Ok(())
                    }
                }
                .boxed()
            })
        };
        let runtime = AccountRuntime::start_at_with_hooks(dir.path(), config("alpha", 0), 1, hooks)
            .expect("a free lock")
            .with_report(report);

        assert!(!runtime.backlog_owed(), "a fresh runtime owes nothing");
        for owed in [true, true, false, false] {
            runtime.tick(TickKind::Quick).await;
            assert_eq!(runtime.backlog_owed(), owed);
        }

        let before = *runtime.completed_ticks().borrow();
        runtime.note_backlog();
        assert!(runtime.backlog_owed());
        assert!(
            *runtime.completed_ticks().borrow() > before,
            "the first sync ran a body, so the scheduler is woken by it"
        );
    }

    /// A runtime whose lock is held elsewhere refuses the tick and still reads.
    #[tokio::test]
    async fn a_blocked_runtime_refuses_the_tick_and_serves_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let holder = EngineLock::try_acquire_at(&dir.path().join("store.lock"), "alpha")
            .expect("the lock file is creatable")
            .expect("a free lock");

        let mut hooks = quiet_hooks();
        hooks.body = Arc::new(|_ctx| async { panic!("a blocked runtime runs no engine") }.boxed());
        let runtime =
            AccountRuntime::start_at_with_hooks(dir.path(), config("alpha", 30), 1, hooks)
                .expect("a contended lock is a successful start");

        assert!(matches!(runtime.readiness(), Readiness::Blocked { .. }));
        let outcome = runtime.tick(TickKind::Quick).await;
        assert!(outcome.blocked);
        assert!(outcome.phases.is_empty());
        assert_eq!(outcome.error, None);

        let read = runtime.read_conn();
        let one: i64 = read
            .query_row("SELECT 1", [], |row| row.get(0))
            .expect("a blocked runtime still reads");
        assert_eq!(one, 1);
        drop(holder);
    }

    /// A drain outside a tick enters the two drain slots, outbox first, runs no
    /// body, and reports what its mutation drain rolled back.
    #[tokio::test]
    async fn a_drain_enters_its_two_slots_and_reports_its_rollbacks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let report = TickReport::default();
        let entered = Arc::new(Mutex::new(Vec::new()));
        let record = |entered: &Arc<Mutex<Vec<Phase>>>| -> DrainHook {
            let entered = Arc::clone(entered);
            Arc::new(move |ctx| {
                lock(&entered).push(ctx.phase);
                async { String::new() }.boxed()
            })
        };
        let rolled_back = {
            let entered = Arc::clone(&entered);
            let report = report.clone();
            Arc::new(move |ctx: TickContext| {
                lock(&entered).push(ctx.phase);
                // What the production mutation hook does with a rollback.
                report.failed_mutations.fetch_add(2, Ordering::SeqCst);
                async { "; 2 mutation(s) failed".to_string() }.boxed()
            }) as DrainHook
        };
        let hooks = TickHooks {
            outbox: record(&entered),
            mutations: rolled_back,
            body: Arc::new(|_ctx| async { panic!("a drain runs no sync") }.boxed()),
        };
        let mut runtime =
            AccountRuntime::start_at_with_hooks(dir.path(), config("alpha", 0), 1, hooks)
                .expect("a free lock");
        runtime.report = report;

        let outcome = runtime.drain().await;
        assert_eq!(
            outcome,
            DrainOutcome {
                ran: true,
                status: "; 2 mutation(s) failed".to_string(),
                failed_mutations: 2,
                busy: false,
            }
        );
        assert_eq!(
            *lock(&entered),
            vec![Phase::DrainOutbox, Phase::DrainMutations]
        );
        // The count is the drain's alone: the next one starts from zero.
        assert_eq!(runtime.drain().await.failed_mutations, 2);
    }

    /// Dropping the runtime frees the lock for the next engine.
    #[test]
    fn dropping_a_runtime_releases_its_engine_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime =
            AccountRuntime::start_at_with_hooks(dir.path(), config("alpha", 30), 1, quiet_hooks())
                .expect("a free lock");
        let path = dir.path().join("store.lock");
        assert!(EngineLock::try_acquire_at(&path, "alpha")
            .expect("readable")
            .is_none());
        // This process is the account's engine while the runtime lives, so a
        // guarded *pass* here takes a turn at the process's own gate rather
        // than a second description of the same file (P5-U8), and one at a
        // time.
        let turn = EngineLock::take_turn_at(&path, "alpha")
            .expect("readable")
            .expect("the engine's own pass gets a turn");
        assert!(EngineLock::take_turn_at(&path, "alpha")
            .expect("readable")
            .is_none());
        drop(turn);
        drop(runtime);
        assert!(EngineLock::try_acquire_at(&path, "alpha")
            .expect("readable")
            .is_some());
    }

    /// PERSO-234: the correspondents a tick's pass inserted are in the contact
    /// index once the tick returns, without a rebuild, and the index is the one
    /// a rebuild of the store would produce.
    ///
    /// The tick is how a running daemon syncs (the scheduler, the watcher, a
    /// `mp sync` routed to the runtime), and it used to drop the pass's
    /// observations: only the guarded pass and the first sync merged them, so
    /// a meeting accept from someone the user had never written to stayed
    /// unsearchable until `mp contacts rebuild`. The body ingests through the
    /// real ingest API and records what the engine records, an observation
    /// for every row it inserted; the second tick ingests the same two
    /// messages again, inserts nothing, and must not count them twice.
    #[tokio::test]
    async fn a_tick_indexes_the_correspondents_its_pass_inserted() {
        use crate::contacts::hooks::fixture::{email, ingest, meeting_reply_and_own_forward};
        use crate::contacts::{build_index_for_account, load_cache, save_cache, search};
        use crate::sync::FreshObservation;
        use crate::types::MailboxRole;

        let _data = crate::config::test_env::TestDataDir::new();
        let mut cfg = config("alpha", 0);
        cfg.default_from = "Sylvain Hellin <me@example.com>".into();
        let dir = crate::config::account_dir(&cfg.name);
        crate::config::create_private_dir_all(&dir).expect("the account directory");
        let zoe = email(
            "Zoe <zoe@example.com>",
            &cfg.default_from,
            None,
            "Seed",
            "Fri, 02 Jan 2026 09:00:00 +0000",
            "<seed@example.com>",
        );
        ingest(&cfg, "inbox", 1, &zoe);
        save_cache(&dir, &build_index_for_account(&cfg).expect("rebuilt")).expect("cached");

        let report = TickReport::default();
        let mut hooks = quiet_hooks();
        hooks.body = {
            let report = report.clone();
            let cfg = cfg.clone();
            Arc::new(move |_ctx: TickContext| {
                let mut result = SyncResult::default();
                for (uid, (mailbox, message)) in (10..).zip(meeting_reply_and_own_forward(&cfg)) {
                    if ingest(&cfg, mailbox, uid, &message).inserted {
                        result
                            .fresh_observations
                            .push(FreshObservation::of(&MailboxRole::from(mailbox), &message));
                    }
                }
                *lock(&report.sync) = Some(result);
                async { Ok(()) }.boxed()
            })
        };
        let runtime = AccountRuntime::start_at_with_hooks(&dir, cfg.clone(), 1, hooks)
            .expect("a free lock")
            .with_report(report);

        let facts = |index: &crate::contacts::ContactIndex| {
            let mut facts: Vec<_> = index
                .contacts
                .values()
                .map(|c| {
                    let seen = (c.first_seen.clone(), c.last_seen.clone());
                    (
                        c.address.clone(),
                        c.tier_tuple(),
                        c.display_name.clone(),
                        seen,
                    )
                })
                .collect();
            facts.sort();
            facts
        };
        for tick in 1..=2 {
            let outcome = runtime.tick(TickKind::Quick).await;
            let cached = load_cache(&dir).expect("readable").expect("cached");
            assert_eq!(
                outcome.contacts,
                (tick == 1).then_some(cached.contacts.len()),
                "tick {tick}: only a tick that wrote the index announces it"
            );
            let hits = search(&cached, "hochmuth", 20);
            assert_eq!(hits.len(), 1, "tick {tick}: searchable without a rebuild");
            assert_eq!(
                hits[0].contact.address,
                "markus.hochmuth@obermeyer-group.com"
            );
            assert_eq!(hits[0].contact.tier_tuple(), (1, 0, 1), "tick {tick}");
            let rebuilt = build_index_for_account(&cfg).expect("rebuilt");
            assert_eq!(facts(&cached), facts(&rebuilt), "tick {tick}");
        }
    }
}
