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
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use futures::future::{BoxFuture, FutureExt};
use log::{debug, info, warn};
use tokio::sync::watch;

use crate::config::AccountConfig;
use crate::engine_lock::EngineLock;
use crate::sync::tick::run_tick_with_drains;

use super::pool::{PooledRead, ReadPool};

/// How many of the newest UIDs per mailbox a [`TickKind::Quick`] tick
/// downloads. The TUI's quick sync uses the same number.
const QUICK_TICK_LIMIT: usize = 100;

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

/// One of the five slots a tick enters, in order.
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
}

/// What a hook is told about the slot it is filling.
#[derive(Clone, Debug)]
pub struct TickContext {
    /// The account this tick belongs to.
    pub account: String,
    /// The kind of tick that is running.
    pub kind: TickKind,
    /// Which of the five slots this call is.
    pub phase: Phase,
    /// The body-fetch budget the tick carries, `None` for unbounded.
    pub body_deadline: Option<Duration>,
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

/// One account's engine: the lock, the pool, and the tick.
pub struct AccountRuntime {
    /// The configured account name.
    account: String,
    /// Resolved by `start` before it returned.
    readiness: Readiness,
    /// Held for the runtime's lifetime, `None` when another engine has it.
    /// Dropping the runtime releases it.
    _lock: Option<EngineLock>,
    /// The reads this runtime serves, open whether or not the lock was taken.
    pool: ReadPool,
    /// What the tick runs.
    hooks: TickHooks,
    /// The account's body-fetch budget, `None` when configured as `0`.
    body_deadline: Option<Duration>,
    /// `Some` while a tick is running: its receiver resolves to that tick's
    /// outcome, which is what a joiner returns.
    running: Mutex<Option<watch::Receiver<Option<TickOutcome>>>>,
}

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
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating the account directory {}", dir.display()))?;
        Self::start_at(&dir, cfg, pool_size)
    }

    /// The mechanism, split out so a test can point it at a tempdir, exactly as
    /// [`EngineLock::try_acquire_at`] is.
    pub fn start_at(dir: &Path, cfg: AccountConfig, pool_size: usize) -> Result<Self> {
        let hooks = production_hooks(Arc::new(cfg.clone()));
        Self::start_at_with_hooks(dir, cfg, pool_size, hooks)
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
        let held = EngineLock::try_acquire_at(&dir.join("store.lock"), &account)?;
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

        Ok(AccountRuntime {
            account,
            readiness,
            _lock: held,
            pool,
            hooks,
            body_deadline: (cfg.imap.body_fetch_deadline_secs > 0)
                .then(|| Duration::from_secs(cfg.imap.body_fetch_deadline_secs)),
            running: Mutex::new(None),
        })
    }

    /// The account this runtime serves.
    pub fn account(&self) -> &str {
        &self.account
    }

    /// Whether this runtime can serve, resolved at start and fixed afterwards.
    pub fn readiness(&self) -> Readiness {
        self.readiness.clone()
    }

    /// Check a read connection out of the pool, waiting until one is free.
    pub fn read_conn(&self) -> PooledRead {
        self.pool.get()
    }

    /// Run one tick, or attach to the one already running.
    pub async fn tick(&self, kind: TickKind) -> TickOutcome {
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
            };
        }

        // Decided under the lock and acted on after it, so no guard is alive
        // across the join's await.
        let slot = {
            let mut running = lock(&self.running);
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
                        };
                    }
                }
            }
            Slot::Run(sender) => sender,
        };

        let outcome = self.run_tick(kind).await;
        // Cleared first: the next `tick()` after this one returns must start a
        // fresh body rather than join a run that is over.
        *lock(&self.running) = None;
        let _ = sender.send(Some(outcome.clone()));
        outcome
    }

    /// The tick itself: [`run_tick_with_drains`] over the installed hooks.
    async fn run_tick(&self, kind: TickKind) -> TickOutcome {
        let phases = Mutex::new(Vec::with_capacity(5));
        let context = |phase: Phase| {
            lock(&phases).push(phase);
            TickContext {
                account: self.account.clone(),
                kind,
                phase,
                body_deadline: self.body_deadline,
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
        TickOutcome {
            kind,
            phases: entered,
            joined: false,
            blocked: false,
            body_deadline: self.body_deadline,
            status,
            error: result.err().map(|e| format!("{e:#}")),
        }
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
fn production_hooks(cfg: Arc<AccountConfig>) -> TickHooks {
    TickHooks {
        outbox: outbox_hook(Arc::clone(&cfg)),
        mutations: mutations_hook(Arc::clone(&cfg)),
        body: body_hook(cfg),
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
fn mutations_hook(cfg: Arc<AccountConfig>) -> DrainHook {
    Arc::new(move |ctx: TickContext| {
        let cfg = Arc::clone(&cfg);
        async move {
            let drained = off_thread("the mutation-queue drain", move || async move {
                crate::pending_ops::resume_account(&cfg).await
            })
            .await
            .and_then(|inner| inner);
            match drained {
                Ok(Some(drained)) if drained.failed > 0 => format!(
                    "; {} mutation(s) failed and were rolled back (see the log)",
                    drained.failed
                ),
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

/// The sync itself, against the account's real IMAP server.
///
/// The backend is built inside the hook rather than at start: it needs
/// [`crate::config::ImapConfig::load`], which reads the keyring, and an account
/// whose credentials are missing must still be a runtime that serves reads and
/// drains queues. A failed load is the body's `Err`, which the tail drain runs
/// after and the outcome carries.
fn body_hook(cfg: Arc<AccountConfig>) -> BodyHook {
    Arc::new(move |ctx: TickContext| {
        let cfg = Arc::clone(&cfg);
        async move {
            off_thread("the sync body", move || async move {
                sync_once(&cfg, &ctx).await
            })
            .await
            .and_then(|inner| inner)
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
async fn off_thread<T, Fut>(
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

/// One sync pass over every configured mailbox of `cfg`.
async fn sync_once(cfg: &AccountConfig, ctx: &TickContext) -> Result<()> {
    use crate::config::ImapConfig;
    use crate::store::{BlobStore, Store};
    use crate::sync::engine::{run_sync, SyncRun};
    use crate::sync::SyncTarget;

    anyhow::ensure!(
        cfg.auth_method != crate::config::AuthMethod::Graph,
        "the daemon tick has no Graph backend yet; '{}' syncs through `mp sync`",
        cfg.name
    );

    let imap_config = ImapConfig::load(cfg)?;
    let targets: Vec<SyncTarget> = crate::config::all_configured_mailboxes(cfg)
        .iter()
        .map(|(role, mapping)| SyncTarget {
            role: role.clone(),
            server_name: mapping.server.clone(),
        })
        .collect();

    // The writer connection, and it is not one of the pool's: a sync
    // transaction must never occupy a read slot
    // (docs/baselines/decisions/read-pool.md).
    let store = Store::open(crate::config::store_path(&cfg.name))?;
    let blobs = BlobStore::for_account(&cfg.name);
    let mut backend =
        crate::imap_client::ImapBackend::new(&imap_config).with_body_budget(ctx.body_deadline);
    let mut span = crate::timing::TimingSpan::with_context("daemon_tick", cfg.name.clone());

    // Unguarded on purpose: this runtime is holding `store.lock` already. See
    // the module docs.
    run_sync(
        &mut backend,
        &SyncRun {
            store: &store,
            blobs: &blobs,
            account: &cfg.name,
            targets: &targets,
            limit: match ctx.kind {
                TickKind::Quick => QUICK_TICK_LIMIT,
                TickKind::Full => usize::MAX,
            },
            dry_run: false,
        },
        &mut span,
    )
    .await?;
    Ok(())
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
        drop(runtime);
        assert!(EngineLock::try_acquire_at(&path, "alpha")
            .expect("readable")
            .is_some());
    }
}
