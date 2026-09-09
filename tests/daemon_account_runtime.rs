//! The per-account runtime and its tick (#0122, plan unit P3b-U3).
//!
//! This file is a **contract test**: it is written before
//! `src/daemon/runtime/account.rs` and `src/daemon/runtime/pool.rs` exist,
//! against the contract fixed in `.agents/workflow/native-gui-daemon/plan.md`
//! section 3.4 (unit P3b-U3), the tick sequencing already implemented in
//! [`mailypoppins::sync::tick::run_tick_with_drains`] (#0114), the engine lock
//! of [`mailypoppins::engine_lock`] (#0061, #0122) and the pool sizing recorded
//! in `docs/baselines/decisions/read-pool.md` (unit P1a-U5). It does not
//! compile under `--features daemon` today, and that failure *is* the proof the
//! contract has no stub behind it. An implementer (P3b-U4) does not edit this
//! file; they make it pass.
//!
//! # The two layers, and why the split
//!
//! **(a) In-process, against `mailypoppins::daemon::runtime`.** Phase order,
//! the tail on a failing body, outbox-before-mutations, the join of a second
//! tick, the `Blocked` degrade and the read pool are properties of the runtime
//! type. None of them is reachable over the socket in Phase 3b (no `sync.*`
//! method exists yet), and none of them can be forced through a real IMAP
//! server. They are driven through an injected [`TickHooks`] instead, so a body
//! can be made to hang, to fail, or to panic if it is ever called.
//!
//! **(b) Over the socket, against a spawned `mp daemon run`.** Whether a
//! runtime exists at all is a property of the daemon, and the plan states it as
//! an environment opt-in: no `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES`, no runtime
//! and no engine lock. Only a real daemon process can be wrong about that, and
//! the engine lock is cross-process by construction, so the assertion is a
//! `flock` attempt from the test process.
//!
//! # Surface under test
//!
//! ```rust,ignore
//! // mailypoppins::daemon::runtime::account
//! pub struct AccountRuntime;              // Send + Sync + 'static
//! impl AccountRuntime {
//!     pub fn start(cfg: AccountConfig, pool_size: usize) -> anyhow::Result<Self>;
//!     pub fn start_at(dir: &Path, cfg: AccountConfig, pool_size: usize) -> anyhow::Result<Self>;
//!     pub fn start_at_with_hooks(
//!         dir: &Path, cfg: AccountConfig, pool_size: usize, hooks: TickHooks,
//!     ) -> anyhow::Result<Self>;
//!     pub fn account(&self) -> &str;
//!     pub fn readiness(&self) -> Readiness;
//!     pub async fn tick(&self, kind: TickKind) -> TickOutcome;
//!     pub fn read_conn(&self) -> PooledRead;
//! }
//!
//! pub enum Readiness { Opening, Ready, Blocked { reason: String } }  // Clone + Debug + PartialEq
//! pub enum TickKind { Quick, Full }                                  // Copy + Debug + PartialEq
//! pub enum Phase { HeadOutbox, HeadMutations, Body, TailOutbox, TailMutations } // Copy + Debug + Eq
//!
//! pub struct TickOutcome {            // Clone + Debug
//!     pub kind: TickKind,
//!     pub phases: Vec<Phase>,
//!     pub joined: bool,
//!     pub blocked: bool,
//!     pub body_deadline: Option<Duration>,
//!     pub status: String,
//!     pub error: Option<String>,
//! }
//!
//! pub struct TickContext {            // Clone + Debug
//!     pub account: String,
//!     pub kind: TickKind,
//!     pub phase: Phase,
//!     pub body_deadline: Option<Duration>,
//! }
//!
//! pub type DrainHook = Arc<dyn Fn(TickContext) -> BoxFuture<'static, String> + Send + Sync>;
//! pub type BodyHook  = Arc<dyn Fn(TickContext) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
//! pub struct TickHooks { pub outbox: DrainHook, pub mutations: DrainHook, pub body: BodyHook }
//!
//! // mailypoppins::daemon::runtime::pool
//! pub const DEFAULT_READ_POOL_SIZE: usize = 2;
//! pub struct ReadPool;                                  // Send + Sync + 'static
//! impl ReadPool {
//!     pub fn open(store_path: &Path, size: usize) -> anyhow::Result<Self>;
//!     pub fn size(&self) -> usize;
//!     pub fn get(&self) -> PooledRead;                  // blocks until one is free
//!     pub fn try_get(&self) -> Option<PooledRead>;      // None when every connection is out
//! }
//! pub struct PooledRead;                                // Send, Deref<Target = rusqlite::Connection>
//! ```
//!
//! # Contract points this file pins beyond the plan text
//!
//! The plan fixes the four method signatures and the phase ordering and leaves
//! the rest to the unit that pins it. These are the decisions taken here.
//!
//! - **The tick is observable through `TickOutcome.phases`.** The plan requires
//!   tests for the order of the phases and for outbox-before-mutations inside a
//!   drain, and a tick that only returns a status string cannot be asked about
//!   either. `phases` is the ordered record of the five slots the tick actually
//!   entered, and it is the runtime's own claim; every test below checks it
//!   against a second, independent record kept by the injected hooks, so a
//!   runtime that reports an order it did not run fails.
//! - **The five slots are named, not counted.** A drain is two hooks (outbox,
//!   then the mutation queue), which is why [`Phase`] has `HeadOutbox` and
//!   `HeadMutations` rather than one `HeadDrain`: "the outbox goes first and
//!   the mutation queue second" is the invariant, and a single symbol per drain
//!   would leave it unpinned.
//! - **The body is injectable, the drains are too, and `TickHooks` replaces all
//!   three at once.** [`mailypoppins::sync::SyncBackend`] returns
//!   `impl Future` (RPITIT), so it is not object safe and
//!   `Box<dyn SyncBackend>` does not exist as a type. The seam is therefore one
//!   struct of three boxed async callbacks. `start` and `start_at` install the
//!   production hooks (the real outbox drain, the real mutation-queue drain,
//!   [`mailypoppins::sync::engine::run_sync_guarded_at`]);
//!   `start_at_with_hooks` replaces them, and is the only entry point a test
//!   uses.
//! - **The tick body is `run_tick_with_drains(head, body, tail)` verbatim**, so
//!   `TickOutcome.status` is exactly that function's first return value:
//!   `head_suffix` concatenated with `tail_suffix`, where a drain's suffix is
//!   its outbox suffix followed by its mutation suffix. A quiet drain
//!   contributes nothing.
//! - **The body's error is carried, not propagated.** `tick` returns
//!   `TickOutcome`, not `Result<TickOutcome>`: a failed sync is a normal tick
//!   whose `error` is `Some(format!("{:#}", err))`. The caller decides what to
//!   do with it, exactly as `run_tick_with_drains` intends, and the tail drains
//!   have already run by then.
//! - **`start` is synchronous and resolves readiness before it returns**, so
//!   `readiness()` is never [`Readiness::Opening`] on a value `start` handed
//!   back. `Opening` is the daemon-side state of an account whose runtime has
//!   been asked for and has not come back yet, which is what `daemon.status`
//!   reports in that window; it is not a state of a live `AccountRuntime`.
//! - **A contended engine lock is a successful start, not an error.** `start`
//!   returns `Ok` with `readiness() == Blocked { reason }`, the store is opened
//!   anyway and `read_conn()` serves reads: that is the read-only degrade
//!   `src/engine_lock.rs` documents, and it is why a second daemon on the same
//!   account is useful rather than dead. `reason` is free text that names the
//!   account.
//! - **A tick on a `Blocked` runtime is a no-op success.** `blocked: true`,
//!   `phases` empty, `error: None`, `status` empty, and neither drain nor body
//!   is entered. This is `Ok(None)`'s reading from `drain_guarded` and
//!   `run_sync_guarded`: the holder does the work, and refusing costs no login.
//! - **The lock is held for the runtime's lifetime**, not for the tick's.
//!   Dropping the runtime releases it; while it lives, no other file
//!   description can take it.
//! - **A second tick joins and receives the running tick's outcome**, with
//!   `joined: true` and every other field copied from the tick it attached to,
//!   including `kind`. A `Full` tick that arrives while a `Quick` one is
//!   running therefore reports `kind: Quick`: it did not run, and reporting the
//!   kind it asked for would say a full pass happened when none did. The join
//!   lasts exactly as long as the running tick: the next `tick()` after it
//!   returns starts a fresh body.
//! - **`body_deadline` is the tick's budget and it is on the outcome.** The
//!   plan requires a daemon tick to carry `imap_config.body_fetch_deadline()`.
//!   [`mailypoppins::config::ImapConfig::load`] needs credentials out of the
//!   keyring, which an offline test has none of, so the runtime reads the
//!   budget from the [`AccountConfig`] it was started with
//!   (`imap.body_fetch_deadline_secs`, `0` meaning unbounded) and reports the
//!   `Duration` it handed the body. Both kinds of daemon tick carry it; the
//!   explicit recovery sync, which is not a tick and does not go through this
//!   type, stays unbounded.
//! - **`read_conn()` blocks and never fails.** The plan's signature returns
//!   `PooledRead`, not `Result<PooledRead>` and not a future, so exhaustion is
//!   a wait rather than an error. `ReadPool::try_get` is the non-blocking form,
//!   and it exists so a test can prove the pool is bounded without a timer.
//! - **A pooled connection returns to the pool on drop**, and the pool hands
//!   out exactly `size` of them, clamped to at least 1.
//! - **`DEFAULT_READ_POOL_SIZE` is 2**, the number
//!   `docs/baselines/decisions/read-pool.md` chose, and the writer connection
//!   is not one of them.
//! - **The socket-level surface for the environment opt-in is
//!   `mp daemon status --json`**, whose `accounts` array is already documented
//!   in `tests/daemon_lifecycle.rs` as
//!   `[{"name":str,"state":"opening"|"ready"|"blocked"}]`. `state.bootstrap`'s
//!   snapshot would have done as well; `daemon.status` needs no handshake and
//!   no async client, so the daemon-level tests here are plain `#[test]`s.
//!   Without the opt-in this file asserts only what the plan guarantees: no
//!   engine lock is held, and no account has left `opening`. It deliberately
//!   does not assert the array is empty, because `tests/daemon_lifecycle.rs`
//!   leaves the implementer free to list configured-but-not-started accounts.
//!
//! # Process and thread hygiene
//!
//! Every daemon started here is killed before the test returns, including on
//! panic: the child goes into a [`Proc`] whose `Drop` kills and reaps it, and
//! [`Sandbox`]'s `Drop` kills whatever `daemon.pid` names. Every wait is a
//! bounded poll or a `tokio::time::timeout`, and every blocking helper thread
//! is released on every path, including the failing one, so a broken
//! implementation fails an assertion rather than hanging the suite. Tests never
//! touch the test process's own environment: `HOME`, `MAILYPOPPINS_DATA_DIR`
//! and `MAILYPOPPINS_CONFIG_DIR` are passed to the child through
//! `Command::env`, and every in-process test points `start_at` at a tempdir
//! rather than letting `config::account_dir` read the environment
//! (`tests/engine_lock_ingest.rs` takes the same route, for the same reason).

use std::fs;
use std::future::Future;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use futures::future::FutureExt;
use serde_json::Value;
use tempfile::TempDir;

use mailypoppins::config::AccountConfig;
use mailypoppins::engine_lock::EngineLock;
use mailypoppins::store::Store;

use mailypoppins::daemon::runtime::account::{
    AccountRuntime, BodyHook, DrainHook, Phase, Readiness, TickContext, TickHooks, TickKind,
    TickOutcome,
};
use mailypoppins::daemon::runtime::pool::{PooledRead, ReadPool, DEFAULT_READ_POOL_SIZE};

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Upper bound on any single wait: a daemon's socket appearing, an account
/// leaving `opening`, a pooled read being served. Generous, because it is a
/// ceiling and never a sleep.
const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

/// The environment opt-in the plan fixes in section 3.0. Spelled out rather
/// than imported so the test states the name a user would type.
const ACCOUNT_RUNTIMES_ENV: &str = "MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES";

/// The account every test uses, so a failure names something readable.
const ACCOUNT: &str = "alpha";

// ---------------------------------------------------------------------------
// Compile-time pins for the forms no test drives
// ---------------------------------------------------------------------------

/// Never called: it exists so the *plain* forms are pinned by the compiler too,
/// with the signatures the plan states.
///
/// No test may drive them, because both resolve their paths through
/// [`mailypoppins::config::account_dir`], which reads the process's
/// environment, and that environment is shared by every test in this binary
/// (`src/config.rs` records why a test never writes it).
#[allow(dead_code)]
fn the_plain_forms_take_a_config_and_a_pool_size(
    cfg: AccountConfig,
    dir: &Path,
) -> anyhow::Result<(AccountRuntime, AccountRuntime)> {
    let from_env = AccountRuntime::start(cfg.clone(), DEFAULT_READ_POOL_SIZE)?;
    let from_dir = AccountRuntime::start_at(dir, cfg, DEFAULT_READ_POOL_SIZE)?;
    Ok((from_env, from_dir))
}

/// Never called: `PooledRead` derefs to a `rusqlite::Connection`, so a caller
/// runs its own SQL exactly as it does through [`Store::conn`].
#[allow(dead_code)]
fn a_pooled_read_is_a_connection(read: &PooledRead) -> &rusqlite::Connection {
    read.deref()
}

// ---------------------------------------------------------------------------
// In-process fixtures
// ---------------------------------------------------------------------------

/// A configured account with no server: the runtime opens a store and takes a
/// lock, and every test drives the body through a hook, so no credential and no
/// host is ever needed.
fn account_config(name: &str, body_fetch_deadline_secs: u64) -> AccountConfig {
    let mut cfg = AccountConfig::default();
    cfg.name = name.to_string();
    cfg.default_from = format!("{name}@example.com");
    cfg.imap.body_fetch_deadline_secs = body_fetch_deadline_secs;
    cfg
}

/// An account directory in a tempdir, in the layout `config::account_dir`
/// produces: the runtime finds `store.lock` and `store.sqlite3` below it.
fn account_dir() -> (TempDir, PathBuf) {
    let root = TempDir::new().expect("tempdir");
    let dir = root.path().join("accounts").join(ACCOUNT);
    fs::create_dir_all(&dir).expect("account dir");
    (root, dir)
}

/// `<account_dir>/store.lock`, the engine lock a runtime holds for its
/// lifetime. Pinned here because a test acquires it from outside the runtime.
fn lock_path(dir: &Path) -> PathBuf {
    dir.join("store.lock")
}

/// `<account_dir>/store.sqlite3`, the file the pool reads and the test's own
/// writer writes.
fn store_path(dir: &Path) -> PathBuf {
    dir.join("store.sqlite3")
}

/// What every hook a test installs writes into, in call order.
///
/// This is the test's own record, kept beside `TickOutcome.phases`: the
/// invariant under test is the order the runtime *ran*, and a runtime that
/// reported a phase list it never executed would satisfy an assertion made
/// against its own claim alone.
#[derive(Default)]
struct Log {
    calls: Mutex<Vec<(&'static str, Phase)>>,
}

impl Log {
    fn push(&self, label: &'static str, phase: Phase) {
        self.calls.lock().expect("log").push((label, phase));
    }

    /// The hook labels, in call order.
    fn labels(&self) -> Vec<&'static str> {
        self.calls
            .lock()
            .expect("log")
            .iter()
            .map(|(label, _)| *label)
            .collect()
    }

    /// The phase each hook was told it was filling, in call order.
    fn phases(&self) -> Vec<Phase> {
        self.calls
            .lock()
            .expect("log")
            .iter()
            .map(|(_, phase)| *phase)
            .collect()
    }
}

/// A drain hook that records its call and returns a numbered status suffix, so
/// `TickOutcome.status` pins *which* drain contributed *which* fragment and in
/// what order.
fn drain_hook(log: Arc<Log>, label: &'static str) -> DrainHook {
    let calls = Arc::new(AtomicUsize::new(0));
    Arc::new(move |ctx: TickContext| {
        let log = Arc::clone(&log);
        let calls = Arc::clone(&calls);
        async move {
            log.push(label, ctx.phase);
            let nth = calls.fetch_add(1, Ordering::SeqCst) + 1;
            format!("; {label}#{nth}")
        }
        .boxed()
    })
}

/// A body hook that records its call and returns `result`.
fn body_hook(log: Arc<Log>, result: Result<(), &'static str>) -> BodyHook {
    Arc::new(move |ctx: TickContext| {
        let log = Arc::clone(&log);
        async move {
            log.push("body", ctx.phase);
            match result {
                Ok(()) => Ok(()),
                Err(message) => Err(anyhow::anyhow!(message)),
            }
        }
        .boxed()
    })
}

/// A hook that fails the test if it is ever entered. Used for the `Blocked`
/// runtime, where "no second engine" means the body is not merely harmless but
/// unreached.
fn panicking_body(what: &'static str) -> BodyHook {
    Arc::new(move |_ctx: TickContext| async move { panic!("{what}") }.boxed())
}

/// The same, for a drain.
fn panicking_drain(what: &'static str) -> DrainHook {
    Arc::new(move |_ctx: TickContext| async move { panic!("{what}") }.boxed())
}

/// Hooks that record into `log` and whose body returns `result`.
fn recording_hooks(log: &Arc<Log>, result: Result<(), &'static str>) -> TickHooks {
    TickHooks {
        outbox: drain_hook(Arc::clone(log), "outbox"),
        mutations: drain_hook(Arc::clone(log), "mutations"),
        body: body_hook(Arc::clone(log), result),
    }
}

/// The five slots a complete tick enters, in the only order #0114 allows.
fn every_phase_in_order() -> Vec<Phase> {
    vec![
        Phase::HeadOutbox,
        Phase::HeadMutations,
        Phase::Body,
        Phase::TailOutbox,
        Phase::TailMutations,
    ]
}

/// The hook labels those five slots produce.
fn every_label_in_order() -> Vec<&'static str> {
    vec!["outbox", "mutations", "body", "outbox", "mutations"]
}

/// Assert the runtime's own phase record agrees with the test's, and that both
/// say what #0114 requires.
fn assert_full_tick_order(outcome: &TickOutcome, log: &Log) {
    assert_eq!(
        outcome.phases,
        every_phase_in_order(),
        "a tick is head drain, body, tail drain, and a drain is the outbox then the mutation queue"
    );
    assert_eq!(
        log.labels(),
        every_label_in_order(),
        "the hooks were entered in that same order"
    );
    assert_eq!(
        log.phases(),
        every_phase_in_order(),
        "and each hook was told which of the five slots it was filling"
    );
}

// ---------------------------------------------------------------------------
// Layer (a) - the tick
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tick_drains_head_first_then_runs_the_body_then_drains_the_tail() {
    let (_root, dir) = account_dir();
    let log = Arc::new(Log::default());
    let runtime = AccountRuntime::start_at_with_hooks(
        &dir,
        account_config(ACCOUNT, 30),
        DEFAULT_READ_POOL_SIZE,
        recording_hooks(&log, Ok(())),
    )
    .expect("a runtime starts against a free lock");

    let outcome = runtime.tick(TickKind::Quick).await;

    assert_full_tick_order(&outcome, &log);
    assert_eq!(outcome.kind, TickKind::Quick);
    assert!(!outcome.joined, "the first tick started the run itself");
    assert!(
        !outcome.blocked,
        "a runtime holding the lock is not blocked"
    );
    assert_eq!(outcome.error, None, "the body succeeded");
    assert_eq!(
        outcome.status, "; outbox#1; mutations#1; outbox#2; mutations#2",
        "the status is run_tick_with_drains' head suffix followed by its tail suffix"
    );
}

#[tokio::test]
async fn the_tail_drain_runs_after_a_body_that_failed_and_the_error_travels_with_the_outcome() {
    let (_root, dir) = account_dir();
    let log = Arc::new(Log::default());
    let runtime = AccountRuntime::start_at_with_hooks(
        &dir,
        account_config(ACCOUNT, 30),
        DEFAULT_READ_POOL_SIZE,
        recording_hooks(&log, Err("login refused")),
    )
    .expect("a runtime starts against a free lock");

    let outcome = runtime.tick(TickKind::Full).await;

    assert_full_tick_order(&outcome, &log);
    assert_eq!(outcome.kind, TickKind::Full);
    assert_eq!(
        outcome.error.as_deref(),
        Some("login refused"),
        "a failing body is reported on the outcome, not propagated past the tail"
    );
    assert_eq!(
        outcome.status, "; outbox#1; mutations#1; outbox#2; mutations#2",
        "a failing tick still carries both drains' status fragments"
    );
    assert!(!outcome.blocked);
    assert!(!outcome.joined);
}

#[tokio::test]
async fn a_quiet_drain_adds_nothing_to_the_status_text() {
    let (_root, dir) = account_dir();
    let quiet: DrainHook = Arc::new(|_ctx: TickContext| async move { String::new() }.boxed());
    let log = Arc::new(Log::default());
    let runtime = AccountRuntime::start_at_with_hooks(
        &dir,
        account_config(ACCOUNT, 30),
        DEFAULT_READ_POOL_SIZE,
        TickHooks {
            outbox: Arc::clone(&quiet),
            mutations: quiet,
            body: body_hook(Arc::clone(&log), Ok(())),
        },
    )
    .expect("a runtime starts against a free lock");

    let outcome = runtime.tick(TickKind::Quick).await;

    assert_eq!(
        outcome.status, "",
        "four empty suffixes concatenate to none"
    );
    assert_eq!(outcome.phases, every_phase_in_order());
    assert_eq!(outcome.error, None);
}

#[tokio::test]
async fn a_second_tick_joins_the_running_one_and_the_body_runs_once() {
    let (_root, dir) = account_dir();
    let log = Arc::new(Log::default());
    let gate = Arc::new(tokio::sync::Notify::new());
    let bodies = Arc::new(AtomicUsize::new(0));
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    let body: BodyHook = {
        let log = Arc::clone(&log);
        let gate = Arc::clone(&gate);
        let bodies = Arc::clone(&bodies);
        Arc::new(move |ctx: TickContext| {
            let log = Arc::clone(&log);
            let gate = Arc::clone(&gate);
            let bodies = Arc::clone(&bodies);
            let entered = entered_tx.clone();
            async move {
                log.push("body", ctx.phase);
                bodies.fetch_add(1, Ordering::SeqCst);
                let _ = entered.send(());
                // Held open until the test has proved the second tick joined.
                gate.notified().await;
                Ok(())
            }
            .boxed()
        })
    };

    let runtime = AccountRuntime::start_at_with_hooks(
        &dir,
        account_config(ACCOUNT, 30),
        DEFAULT_READ_POOL_SIZE,
        TickHooks {
            outbox: drain_hook(Arc::clone(&log), "outbox"),
            mutations: drain_hook(Arc::clone(&log), "mutations"),
            body,
        },
    )
    .expect("a runtime starts against a free lock");

    let (first, second) = within("two concurrent ticks", async {
        futures::future::join(runtime.tick(TickKind::Quick), async {
            // The first tick is inside its body; only now can a second one join
            // it. No sleep: the body itself says when it got there.
            entered_rx
                .recv()
                .await
                .expect("the body reports it started");

            let joiner = runtime.tick(TickKind::Full);
            futures::pin_mut!(joiner);

            // A joiner cannot finish while the tick it attached to is still in
            // its body. Polled a bounded number of times so an implementation
            // that yields internally before attaching is not misjudged.
            for _ in 0..8 {
                let settled = futures::future::poll_fn(|cx| match joiner.as_mut().poll(cx) {
                    Poll::Ready(outcome) => Poll::Ready(Some(outcome)),
                    Poll::Pending => Poll::Ready(None),
                })
                .await;
                assert!(
                    settled.is_none(),
                    "a joined tick cannot return before the tick it joined: {settled:?}"
                );
                tokio::task::yield_now().await;
            }

            gate.notify_one();
            joiner.await
        })
        .await
    })
    .await;

    assert_eq!(
        bodies.load(Ordering::SeqCst),
        1,
        "two concurrent ticks run one body between them"
    );
    assert_full_tick_order(&first, &log);

    assert!(!first.joined, "the tick that started the run did not join");
    assert!(second.joined, "the tick that arrived second joined it");
    assert_eq!(
        second.kind,
        TickKind::Quick,
        "a joiner reports the kind that actually ran, not the one it asked for"
    );
    assert_eq!(second.phases, first.phases, "a joiner sees the same phases");
    assert_eq!(second.status, first.status, "and the same status text");
    assert_eq!(second.error, first.error, "and the same result");

    // The join is scoped to the run, not cached: the next tick starts a body.
    // The gate needs a fresh permit for it: a `notify_one` delivered to an
    // already registered waiter wakes that waiter and stores nothing, so the
    // call that released the first body left the gate empty. This one has no
    // waiter to wake, so it stores the permit the third body consumes.
    gate.notify_one();
    let third = within("a tick after the join", runtime.tick(TickKind::Quick)).await;
    assert!(!third.joined);
    assert_eq!(
        bodies.load(Ordering::SeqCst),
        2,
        "a tick issued after the running one finished starts its own body"
    );
}

#[tokio::test]
async fn a_started_runtime_is_ready_and_holds_the_engine_lock_for_its_lifetime() {
    let (_root, dir) = account_dir();
    let log = Arc::new(Log::default());
    let runtime = AccountRuntime::start_at_with_hooks(
        &dir,
        account_config(ACCOUNT, 30),
        DEFAULT_READ_POOL_SIZE,
        recording_hooks(&log, Ok(())),
    )
    .expect("a runtime starts against a free lock");

    assert_eq!(runtime.account(), ACCOUNT);
    assert_eq!(
        runtime.readiness(),
        Readiness::Ready,
        "start resolved readiness before it returned, so nothing is ever Opening here"
    );

    let contender =
        EngineLock::try_acquire_at(&lock_path(&dir), ACCOUNT).expect("the lock file is readable");
    assert!(
        contender.is_none(),
        "a live runtime holds the engine lock, so nobody else can take it"
    );

    drop(runtime);

    let after = EngineLock::try_acquire_at(&lock_path(&dir), ACCOUNT).expect("lock file readable");
    assert!(
        after.is_some(),
        "dropping the runtime releases the lock for the next engine"
    );
}

#[tokio::test]
async fn a_runtime_whose_lock_is_held_elsewhere_is_blocked_and_never_runs_an_engine() {
    let (_root, dir) = account_dir();
    // flock is per open file description, not per process, so a holder in this
    // very test is a genuine second engine as far as the runtime is concerned
    // (`tests/engine_lock_ingest.rs` relies on the same property).
    let holder = EngineLock::try_acquire_at(&lock_path(&dir), ACCOUNT)
        .expect("the lock file is creatable")
        .expect("a free lock is taken");

    let runtime = AccountRuntime::start_at_with_hooks(
        &dir,
        account_config(ACCOUNT, 30),
        DEFAULT_READ_POOL_SIZE,
        TickHooks {
            outbox: panicking_drain("a blocked runtime must not drain the outbox"),
            mutations: panicking_drain("a blocked runtime must not drain the mutation queue"),
            body: panicking_body("a blocked runtime must not run a second engine"),
        },
    )
    .expect("a contended lock is a successful start, not an error");

    match runtime.readiness() {
        Readiness::Blocked { reason } => assert!(
            reason.contains(ACCOUNT),
            "the reason names the account it is blocked on, got {reason:?}"
        ),
        other => panic!("an account whose lock is held elsewhere is Blocked, got {other:?}"),
    }

    let outcome = within("a tick on a blocked runtime", runtime.tick(TickKind::Full)).await;
    assert!(outcome.blocked, "the tick reports the refusal");
    assert!(
        outcome.phases.is_empty(),
        "a blocked tick enters no phase at all, got {:?}",
        outcome.phases
    );
    assert_eq!(outcome.error, None, "a refusal is a success, not an error");
    assert_eq!(outcome.status, "", "and it has nothing to say");
    assert!(!outcome.joined);

    // The read-only degrade: a blocked runtime still serves reads, which is the
    // whole reason it is worth having (`src/engine_lock.rs`).
    let read = runtime.read_conn();
    let one: i64 = read
        .query_row("SELECT 1", [], |row| row.get(0))
        .expect("a blocked runtime still reads its store");
    assert_eq!(one, 1);

    drop(holder);
}

#[tokio::test]
async fn a_daemon_tick_carries_the_configured_body_fetch_deadline() {
    let (_root, dir) = account_dir();
    let log = Arc::new(Log::default());
    let runtime = AccountRuntime::start_at_with_hooks(
        &dir,
        account_config(ACCOUNT, 7),
        DEFAULT_READ_POOL_SIZE,
        recording_hooks(&log, Ok(())),
    )
    .expect("a runtime starts against a free lock");

    for kind in [TickKind::Quick, TickKind::Full] {
        let outcome = within("a deadline-carrying tick", runtime.tick(kind)).await;
        assert_eq!(
            outcome.body_deadline,
            Some(Duration::from_secs(7)),
            "a {kind:?} daemon tick carries the account's body-fetch budget"
        );
    }
}

#[tokio::test]
async fn a_zero_body_fetch_deadline_means_an_unbounded_tick() {
    let (_root, dir) = account_dir();
    let log = Arc::new(Log::default());
    let runtime = AccountRuntime::start_at_with_hooks(
        &dir,
        account_config(ACCOUNT, 0),
        DEFAULT_READ_POOL_SIZE,
        recording_hooks(&log, Ok(())),
    )
    .expect("a runtime starts against a free lock");

    let outcome = within("an unbounded tick", runtime.tick(TickKind::Quick)).await;
    assert_eq!(
        outcome.body_deadline, None,
        "0 seconds is unbounded, exactly as ImapConfig::body_fetch_deadline reads it"
    );
}

// ---------------------------------------------------------------------------
// Layer (a) - the read pool
// ---------------------------------------------------------------------------

#[test]
fn the_default_pool_size_is_the_one_the_measurement_chose() {
    assert_eq!(
        DEFAULT_READ_POOL_SIZE, 2,
        "docs/baselines/decisions/read-pool.md: two read connections per account, \
         the writer being a separate connection outside the pool"
    );
}

#[test]
fn the_pool_hands_out_exactly_its_size_and_takes_a_connection_back_on_drop() {
    let (_root, dir) = account_dir();
    // The pool reads a store; creating it is the runtime's job in production
    // and the test's here, because this exercise has no runtime.
    let store = Store::open(store_path(&dir)).expect("a store is created");
    drop(store);

    let pool = ReadPool::open(&store_path(&dir), 2).expect("a pool opens over an existing store");
    assert_eq!(pool.size(), 2);

    let first = pool.try_get().expect("the first connection is free");
    let second = pool.try_get().expect("the second connection is free");
    assert!(
        pool.try_get().is_none(),
        "a pool of 2 has nothing left to hand out once both are checked out"
    );

    let one: i64 = first
        .query_row("SELECT 1", [], |row| row.get(0))
        .expect("a pooled connection answers a query");
    assert_eq!(one, 1);

    drop(second);
    assert!(
        pool.try_get().is_some(),
        "dropping a pooled read returns it to the pool"
    );
    drop(first);
}

#[test]
fn a_pool_size_below_one_is_clamped_to_one() {
    let (_root, dir) = account_dir();
    Store::open(store_path(&dir)).expect("a store is created");

    let pool = ReadPool::open(&store_path(&dir), 0).expect("a pool opens");
    assert_eq!(
        pool.size(),
        1,
        "a pool of zero connections could never serve a read"
    );
    assert!(pool.try_get().is_some());
}

#[tokio::test]
async fn read_conn_waits_for_a_busy_pool_and_is_served_as_soon_as_one_returns() {
    let (_root, dir) = account_dir();
    let log = Arc::new(Log::default());
    let runtime = Arc::new(
        AccountRuntime::start_at_with_hooks(
            &dir,
            account_config(ACCOUNT, 30),
            1,
            recording_hooks(&log, Ok(())),
        )
        .expect("a runtime starts against a free lock"),
    );

    let held = runtime.read_conn();

    let waiter = {
        let runtime = Arc::clone(&runtime);
        tokio::task::spawn_blocking(move || {
            let read = runtime.read_conn();
            read.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .expect("the pooled connection answers once it is free")
        })
    };
    let mut waiter = waiter;

    // Deterministic: the only connection is held, so a correct pool cannot
    // serve this one however long the test waits.
    let early = tokio::time::timeout(Duration::from_millis(200), &mut waiter).await;
    assert!(
        early.is_err(),
        "read_conn cannot return a connection that is checked out"
    );

    drop(held);

    let served = within("the waiting read", waiter)
        .await
        .expect("the blocking read task did not panic");
    assert_eq!(served, 1, "the waiter was served the returned connection");
}

#[tokio::test]
async fn a_preview_read_is_served_while_a_list_load_and_a_write_are_in_flight() {
    let (_root, dir) = account_dir();
    let log = Arc::new(Log::default());
    let runtime = Arc::new(
        AccountRuntime::start_at_with_hooks(
            &dir,
            account_config(ACCOUNT, 30),
            DEFAULT_READ_POOL_SIZE,
            recording_hooks(&log, Ok(())),
        )
        .expect("a runtime starts against a free lock"),
    );

    // The long read: one of the pool's connections, held for the whole
    // measurement, standing for a 5 k-row list load
    // (docs/baselines/decisions/read-pool.md).
    let list_load = runtime.read_conn();
    let rows: i64 = list_load
        .query_row("SELECT 1", [], |row| row.get(0))
        .expect("the list load reads");
    assert_eq!(rows, 1);

    // The write in flight: a separate connection on its own thread, which is
    // what the daemon's writer is. It commits continuously until stopped, and
    // announces its first commit so the preview read below starts against a
    // writer that is genuinely in flight rather than one still opening.
    let stop = Arc::new(AtomicBool::new(false));
    let (first_commit_tx, first_commit_rx) = tokio::sync::oneshot::channel::<()>();
    let writer = {
        let stop = Arc::clone(&stop);
        let path = store_path(&dir);
        tokio::task::spawn_blocking(move || {
            let store = Store::open(&path).expect("the writer opens the store");
            store
                .conn()
                .execute_batch("CREATE TABLE IF NOT EXISTS _pool_probe (id INTEGER PRIMARY KEY)")
                .expect("the writer creates its scratch table");
            let mut written = 0u64;
            let mut announce = Some(first_commit_tx);
            while !stop.load(Ordering::SeqCst) {
                store
                    .conn()
                    .execute("INSERT INTO _pool_probe DEFAULT VALUES", [])
                    .expect("the writer commits");
                written += 1;
                if let Some(tx) = announce.take() {
                    let _ = tx.send(());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            written
        })
    };

    within("the writer's first commit", first_commit_rx)
        .await
        .expect("the writer announces its first commit");

    // The preview read: the short one the pool exists to keep out of the queue.
    let preview = {
        let runtime = Arc::clone(&runtime);
        tokio::task::spawn_blocking(move || {
            let started = Instant::now();
            let read = runtime.read_conn();
            let one: i64 = read
                .query_row("SELECT 1", [], |row| row.get(0))
                .expect("the preview read answers");
            assert_eq!(one, 1);
            started.elapsed()
        })
    };

    let served = tokio::time::timeout(DEADLINE, preview).await;

    // Released on every path, including the failing one, so a pool that cannot
    // serve the read leaves no thread parked behind a panicking assertion.
    drop(list_load);
    stop.store(true, Ordering::SeqCst);

    let elapsed = served
        .expect("the preview read is served while a list load and a write are in flight")
        .expect("the preview task did not panic");
    assert!(
        elapsed < DEADLINE,
        "the preview read took {elapsed:?}, which is not 'served'"
    );

    let written = within("the writer", writer)
        .await
        .expect("the writer task did not panic");
    assert!(
        written > 0,
        "the write really was in flight during the preview read"
    );
}

// ---------------------------------------------------------------------------
// Layer (b) - the environment opt-in, over a spawned daemon
// ---------------------------------------------------------------------------

/// A private `HOME`, config directory and data directory, with one configured
/// account. Dropping it kills whatever daemon `daemon.pid` names.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
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
        // The account directory exists before any daemon runs, so a test can
        // reach the lock file whether or not a runtime was ever created.
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

    /// An `mp` invocation pointed at this sandbox, with the opt-in explicitly
    /// cleared so an inherited variable cannot change the outcome.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(MP);
        cmd.env("HOME", self.home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env_remove(ACCOUNT_RUNTIMES_ENV)
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START");
        cmd
    }

    /// Spawn `mp daemon run`, killed on drop, and wait until its socket
    /// accepts a connection.
    fn start_daemon(&self, account_runtimes: bool) -> Proc {
        let mut cmd = self.cmd();
        if account_runtimes {
            cmd.env(ACCOUNT_RUNTIMES_ENV, "1");
        }
        let child = cmd
            .args(["daemon", "run"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mp daemon run");
        let proc = Proc(Some(child));
        let start = Instant::now();
        while std::os::unix::net::UnixStream::connect(self.socket()).is_err() {
            assert!(
                start.elapsed() < DEADLINE,
                "the daemon socket {} never accepted a connection within {DEADLINE:?}",
                self.socket().display()
            );
            std::thread::sleep(TICK);
        }
        proc
    }

    /// `mp daemon status --json`, parsed.
    fn status_json(&self) -> Value {
        let out = self
            .cmd()
            .args(["daemon", "status", "--json"])
            .output()
            .expect("run mp daemon status --json");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("`mp daemon status --json` did not print one JSON object: {e}\nstdout: {stdout}")
        })
    }

    /// The `state` of the configured account, or `None` when the daemon does
    /// not list it at all.
    fn account_state(&self) -> Option<String> {
        let status = self.status_json();
        let accounts = status["accounts"]
            .as_array()
            .unwrap_or_else(|| panic!("accounts is an array, got {status}"))
            .clone();
        accounts
            .iter()
            .find(|entry| entry["name"] == Value::from(ACCOUNT))
            .map(|entry| {
                entry["state"]
                    .as_str()
                    .unwrap_or_else(|| panic!("an account state is a string, got {entry}"))
                    .to_string()
            })
    }

    /// Poll until the account has left `opening`, and report where it landed.
    fn wait_settled_state(&self) -> String {
        let start = Instant::now();
        loop {
            if let Some(state) = self.account_state() {
                if state != "opening" {
                    return state;
                }
            }
            assert!(
                start.elapsed() < DEADLINE,
                "the account never left `opening` within {DEADLINE:?}"
            );
            std::thread::sleep(TICK);
        }
    }

    /// Whether the engine lock for the configured account is free right now.
    fn engine_lock_is_free(&self) -> bool {
        EngineLock::try_acquire_at(&self.account_dir().join("store.lock"), ACCOUNT)
            .expect("the lock file is creatable")
            .is_some()
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
/// patience if it never finishes.
async fn within<T>(label: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(DEADLINE, fut).await {
        Ok(value) => value,
        Err(_) => panic!("{label} did not finish within {DEADLINE:?}"),
    }
}

#[test]
fn without_the_environment_opt_in_the_daemon_starts_no_runtime_and_takes_no_engine_lock() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon(false);

    let status = sandbox.status_json();
    assert_eq!(status["running"], Value::from(true));
    // The array may list configured accounts (tests/daemon_lifecycle.rs leaves
    // that free), but none of them may have come up.
    for entry in status["accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("accounts is an array, got {status}"))
    {
        assert_eq!(
            entry["state"],
            Value::from("opening"),
            "no runtime exists without {ACCOUNT_RUNTIMES_ENV}, so nothing is ready or blocked: \
             {entry}"
        );
    }

    assert!(
        sandbox.engine_lock_is_free(),
        "without {ACCOUNT_RUNTIMES_ENV} the daemon acquires no engine lock"
    );
}

#[test]
fn with_the_environment_opt_in_the_account_becomes_ready_and_the_daemon_holds_the_engine_lock() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon(true);

    assert_eq!(
        sandbox.wait_settled_state(),
        "ready",
        "an account whose lock is free comes up ready"
    );
    assert!(
        !sandbox.engine_lock_is_free(),
        "a live runtime holds its account's engine lock"
    );
    assert!(
        sandbox.account_dir().join("store.sqlite3").exists(),
        "a runtime that came up ready opened (and created) its store"
    );
}

#[test]
fn an_account_whose_lock_is_held_elsewhere_comes_up_blocked_over_the_socket() {
    let sandbox = Sandbox::new();
    let holder = EngineLock::try_acquire_at(&sandbox.account_dir().join("store.lock"), ACCOUNT)
        .expect("the lock file is creatable")
        .expect("a free lock is taken");

    let _daemon = sandbox.start_daemon(true);

    assert_eq!(
        sandbox.wait_settled_state(),
        "blocked",
        "a daemon that cannot take the engine lock reports blocked rather than running a second \
         engine"
    );

    drop(holder);
}
