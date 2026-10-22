//! The per-account scheduler (#0134): a quick tick once an account has gone
//! `imap.sync_interval_secs` without one.
//!
//! The watcher ([`super::watcher`]) hears one mailbox: an IMAP IDLE round on
//! INBOX, or the Graph inbox's id set. Everything else stayed stale until a
//! manual sync or new INBOX mail: another mailbox changed by another client, a
//! queued op or an outbox Sent copy waiting out its backoff, a body fetch the
//! deadline cut, an IDLE connection the server dropped without a word, and a
//! daemon with no client at all. The scheduler is what reaches those: it runs
//! the same [`TickKind::Quick`] tick the watcher runs, through the same
//! [`tick_and_publish`], so its `sync.completed` and its counts read exactly
//! like a watcher's.
//!
//! # The interval is measured from the last tick, whoever asked for it
//!
//! The runtime records when each tick that ran a body finished
//! ([`AccountRuntime::completed_ticks`]), and the scheduler waits until that
//! moment plus the interval. A manual `mp sync`, a watcher tick, a client's
//! startup fetch and the scheduler's own tick all move it, so a busy account is
//! never ticked twice in a row for the schedule's sake. A drain is not a tick
//! and does not move it.
//!
//! # A runtime that comes up syncs once
//!
//! The first scheduled tick is [`STARTUP_TICK`] after the runtime started,
//! not an interval (PERSO-178). Neither watcher syncs on its first look, so a
//! daemon start or restart with no client asking left every account a whole
//! interval stale, a Graph account most visibly. Any tick that finishes
//! before that deadline (a client's startup fetch, a watcher round) moves the
//! record and cancels the startup tick: the next one is an interval after it.
//! A startup tick that finds a client's fetch still running joins it.
//!
//! A scheduled tick that finds a tick running joins it, and one that finds a
//! drain running waits for its turn: both are [`AccountRuntime::tick`]'s own
//! rules. A tick that did not run (refused) does not move the record, so the
//! scheduler also remembers when it last fired and never fires sooner than an
//! interval after that.
//!
//! # A backlog is drained at once
//!
//! A tick that left a Graph folder capped by its window, with more new mail
//! behind it, says so ([`AccountRuntime::backlog_owed`], PERSO-172), and the
//! next tick then comes [`BACKLOG_FOLLOW_UP`] after it instead of an interval.
//! A Graph quick tick downloads by id, so each one takes the next window and a
//! new account's mailbox fills in minutes; at one window per interval it took
//! hours, and offline search missed the mail meanwhile. Every tick stays a
//! bounded quick tick under the Graph ceiling, and the run slot is free
//! between them, so a manual sync, a send or a drain is never queued behind
//! the whole backlog. A tick that failed, or whose batch fell short (a
//! throttle), owes nothing and falls back to the interval, which is the
//! back-off.
//!
//! # What gets a scheduler
//!
//! A ready runtime with a server (IMAP or Graph) and a non-zero interval. A
//! local-only account has no server; a blocked runtime holds no engine lock,
//! and the engine that does is scheduling its own. A Graph account reads the
//! same `imap.sync_interval_secs`: the setting names the tick, not the
//! transport.
//!
//! # Lifetime
//!
//! The watcher's rules: spawned for one runtime, held weakly, stopped by its
//! retirement or its drop, and never looking a runtime up by name. A reload
//! that changes `sync_interval_secs` changes the effective account, which
//! restarts the runtime and with it the scheduler, so the new interval needs
//! no plumbing of its own.

use std::sync::{Arc, Weak};
use std::time::Duration;

use futures::future::BoxFuture;
use log::{debug, info};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::AccountConfig;
use crate::daemon::state::CanonicalState;

use super::account::{AccountRuntime, Readiness, TickKind};
use super::publish::tick_and_publish;

/// How soon after a tick that left a backlog the next one runs (see the module
/// docs): long enough for a client's own request to take the run slot first,
/// short against the seconds a Graph window takes to download.
pub const BACKLOG_FOLLOW_UP: Duration = Duration::from_secs(5);

/// How soon after the runtime comes up its first scheduled tick runs (see the
/// module docs): long enough for a client's startup fetch, which cancels it,
/// to reach the daemon first.
pub const STARTUP_TICK: Duration = Duration::from_secs(10);

/// Start the scheduler for `runtime`, if it has anything to schedule.
///
/// The handle is the test's; the daemon lets the task run until the runtime
/// it was spawned for retires.
pub fn spawn(
    runtime: &Arc<AccountRuntime>,
    canonical: Arc<CanonicalState>,
    cfg: &AccountConfig,
) -> Option<JoinHandle<()>> {
    let interval = schedulable(runtime, cfg)?;
    info!(
        "[scheduler] scheduling {}: a first sync in {}s, then one after {}s without a sync",
        cfg.name,
        STARTUP_TICK.min(interval).as_secs(),
        interval.as_secs()
    );
    Some(tokio::spawn(run(
        Arc::downgrade(runtime),
        runtime.retired(),
        runtime.completed_ticks(),
        Cadence {
            startup: STARTUP_TICK,
            interval,
            follow_up: BACKLOG_FOLLOW_UP,
        },
        Arc::new(move |runtime: Arc<AccountRuntime>| {
            let canonical = Arc::clone(&canonical);
            Box::pin(async move {
                tick_and_publish(&runtime, &canonical, TickKind::Quick).await;
            }) as BoxFuture<'static, ()>
        }),
    )))
}

/// The interval `runtime` is scheduled at, or `None` when it gets no
/// scheduler (see the module docs).
fn schedulable(runtime: &AccountRuntime, cfg: &AccountConfig) -> Option<Duration> {
    let why_not = if cfg.is_local_only() {
        "has no server"
    } else if runtime.readiness() != Readiness::Ready {
        "is blocked"
    } else if cfg.imap.sync_interval().is_none() {
        "has sync_interval_secs = 0"
    } else {
        return cfg.imap.sync_interval();
    };
    debug!("[scheduler] not scheduling {}: it {why_not}", cfg.name);
    None
}

/// What the scheduler does when the interval is up: the production one ticks
/// and publishes, a test's counts.
type Fire = Arc<dyn Fn(Arc<AccountRuntime>) -> BoxFuture<'static, ()> + Send + Sync>;

/// Resolve once `retired` goes `true` or its runtime is dropped.
async fn stopped(retired: &mut watch::Receiver<bool>) {
    let _ = retired.wait_for(|retired| *retired).await;
}

/// The wait before the next scheduled tick: the interval, or the backlog
/// follow-up when the last tick left one (never longer than the interval).
fn gap(interval: Duration, follow_up: Duration, backlog_owed: bool) -> Duration {
    if backlog_owed {
        follow_up.min(interval)
    } else {
        interval
    }
}

/// The three waits a scheduler chooses between (see the module docs).
#[derive(Debug, Clone, Copy)]
struct Cadence {
    /// After the runtime started, before its first scheduled tick.
    startup: Duration,
    /// After the last tick, in the steady state.
    interval: Duration,
    /// After a tick that left a backlog.
    follow_up: Duration,
}

/// Why the scheduler is about to tick, for the log line.
fn reason(startup: bool, owed: bool) -> &'static str {
    if startup {
        "has just come up"
    } else if owed {
        "still has new mail to download"
    } else {
        "has gone its interval without a sync"
    }
}

/// One account's scheduler, until the runtime it was spawned for retires.
async fn run(
    runtime: Weak<AccountRuntime>,
    mut retired: watch::Receiver<bool>,
    mut ticks: watch::Receiver<Instant>,
    cadence: Cadence,
    fire: Fire,
) {
    let Cadence {
        startup,
        interval,
        follow_up,
    } = cadence;
    let Some((account, started)) = runtime
        .upgrade()
        .map(|runtime| (runtime.account().to_string(), runtime.started()))
    else {
        return;
    };
    // A floor under the next deadline that the scheduler's own firing sets, so
    // a tick that was refused and moved nothing is not fired again at once.
    let mut fired: Option<Instant> = None;
    loop {
        let last = *ticks.borrow_and_update();
        let owed = runtime
            .upgrade()
            .is_some_and(|runtime| runtime.backlog_owed());
        // Owed until the scheduler fires once or a tick of another origin
        // finishes, whichever comes first.
        let at_startup = fired.is_none() && last == started;
        let wait = if at_startup {
            startup.min(interval)
        } else {
            gap(interval, follow_up, owed)
        };
        if owed {
            info!(
                "[scheduler] {account} still has new mail to download; ticking again in {}s",
                wait.as_secs()
            );
        }
        let due = fired.map_or(last, |fired| fired.max(last)) + wait;
        tokio::select! {
            // Checked first, so a retired runtime never fires a deadline that
            // expired at the same moment.
            biased;
            () = stopped(&mut retired) => break,
            // Another tick finished: the deadline moves with it.
            changed = ticks.changed() => {
                if changed.is_err() {
                    break;
                }
                continue;
            }
            () = tokio::time::sleep_until(due) => {}
        }
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        fired = Some(Instant::now());
        if runtime.readiness() != Readiness::Ready {
            // The tick would be refused: say so, and log no "ticking" for it.
            debug!("[scheduler] not ticking {account}: its runtime is blocked");
            continue;
        }
        info!(
            "[scheduler] {account} {}; ticking",
            reason(at_startup, owed)
        );
        fire(runtime).await;
    }
    info!("[scheduler] {account}'s runtime retired; stopped scheduling");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::future::FutureExt;

    use super::super::account::{DrainHook, TickHooks};
    use crate::config::AuthMethod;
    use crate::engine_lock::EngineLock;

    /// A short interval, so the tests run in real time without a paused clock:
    /// the production value is the configured one, and only its length
    /// differs.
    const INTERVAL: Duration = Duration::from_millis(300);

    /// Hooks whose body counts the ticks that ran one.
    fn counting_hooks(bodies: &Arc<AtomicUsize>) -> TickHooks {
        let quiet: DrainHook = Arc::new(|_ctx| async { String::new() }.boxed());
        let bodies = Arc::clone(bodies);
        TickHooks {
            outbox: Arc::clone(&quiet),
            mutations: quiet,
            body: Arc::new(move |_ctx| {
                bodies.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }.boxed()
            }),
        }
    }

    fn config() -> AccountConfig {
        let mut cfg = AccountConfig {
            name: "alpha".to_string(),
            ..Default::default()
        };
        cfg.imap.host = "imap.example.com".to_string();
        cfg
    }

    fn started(dir: &std::path::Path, bodies: &Arc<AtomicUsize>) -> Arc<AccountRuntime> {
        Arc::new(
            AccountRuntime::start_at_with_hooks(dir, config(), 1, counting_hooks(bodies)).unwrap(),
        )
    }

    /// The scheduler over `runtime`, firing a real quick tick and counting the
    /// times it fired.
    ///
    /// Its startup tick waits the whole interval, so the tests that use it
    /// read the steady-state interval alone; the startup tick has its own.
    fn scheduler(runtime: &Arc<AccountRuntime>, fired: &Arc<AtomicUsize>) -> JoinHandle<()> {
        scheduler_with(
            runtime,
            fired,
            Cadence {
                startup: INTERVAL,
                interval: INTERVAL,
                follow_up: INTERVAL / 6,
            },
        )
    }

    /// [`scheduler`] with its cadence chosen.
    fn scheduler_with(
        runtime: &Arc<AccountRuntime>,
        fired: &Arc<AtomicUsize>,
        cadence: Cadence,
    ) -> JoinHandle<()> {
        let fired = Arc::clone(fired);
        tokio::spawn(run(
            Arc::downgrade(runtime),
            runtime.retired(),
            runtime.completed_ticks(),
            cadence,
            Arc::new(move |runtime: Arc<AccountRuntime>| {
                fired.fetch_add(1, Ordering::SeqCst);
                async move {
                    runtime.tick(TickKind::Quick).await;
                }
                .boxed()
            }),
        ))
    }

    fn canonical() -> Arc<CanonicalState> {
        Arc::new(CanonicalState::new(
            crate::daemon::state::InstanceId::new("test"),
            Vec::new(),
        ))
    }

    /// Nothing before the interval, one tick after it, and the next one an
    /// interval after that one finished.
    #[tokio::test]
    async fn a_tick_fires_after_the_interval_and_again_an_interval_later() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        let fired = Arc::new(AtomicUsize::new(0));
        let task = scheduler(&runtime, &fired);

        tokio::time::sleep(INTERVAL / 2).await;
        assert_eq!(fired.load(Ordering::SeqCst), 0, "not before the interval");
        tokio::time::sleep(INTERVAL).await;
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(
            bodies.load(Ordering::SeqCst),
            1,
            "the scheduled tick ran a body"
        );
        tokio::time::sleep(INTERVAL).await;
        assert_eq!(fired.load(Ordering::SeqCst), 2);

        drop(runtime);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("a dropped runtime stops its scheduler")
            .unwrap();
    }

    /// PERSO-178: a runtime that comes up is ticked once soon after, not an
    /// interval later, and the next tick then waits the interval.
    #[tokio::test]
    async fn a_runtime_that_comes_up_ticks_soon_and_then_on_the_interval() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        let fired = Arc::new(AtomicUsize::new(0));
        let start = Instant::now();
        let cadence = Cadence {
            startup: Duration::from_millis(50),
            interval: Duration::from_millis(1500),
            follow_up: Duration::from_millis(50),
        };
        let _task = scheduler_with(&runtime, &fired, cadence);

        tokio::time::sleep_until(start + Duration::from_millis(500)).await;
        assert_eq!(fired.load(Ordering::SeqCst), 1, "the startup tick fired");
        assert_eq!(bodies.load(Ordering::SeqCst), 1, "and ran a body");
        tokio::time::sleep_until(start + Duration::from_millis(1200)).await;
        assert_eq!(fired.load(Ordering::SeqCst), 1, "then the interval");
        tokio::time::sleep_until(start + Duration::from_millis(2300)).await;
        assert_eq!(fired.load(Ordering::SeqCst), 2);
    }

    /// A tick of another origin (a client's startup fetch) that finishes
    /// before the startup tick cancels it: the next one is an interval later.
    #[tokio::test]
    async fn a_tick_before_the_startup_one_cancels_it() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        let fired = Arc::new(AtomicUsize::new(0));
        let cadence = Cadence {
            startup: Duration::from_millis(400),
            interval: Duration::from_millis(1500),
            follow_up: Duration::from_millis(50),
        };
        let _task = scheduler_with(&runtime, &fired, cadence);

        let fetched = Instant::now();
        runtime.tick(TickKind::Quick).await;
        tokio::time::sleep_until(fetched + Duration::from_millis(900)).await;
        assert_eq!(fired.load(Ordering::SeqCst), 0, "the fetch cancelled it");
        tokio::time::sleep_until(fetched + Duration::from_millis(2200)).await;
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    /// A tick of another origin (a manual sync, the watcher) pushes the
    /// scheduled one back a whole interval from when it finished.
    #[tokio::test]
    async fn a_tick_of_another_origin_pushes_the_scheduled_one_back() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        let fired = Arc::new(AtomicUsize::new(0));
        let _task = scheduler(&runtime, &fired);

        tokio::time::sleep(INTERVAL * 2 / 3).await;
        let manual = Instant::now();
        runtime.tick(TickKind::Full).await;
        // Past the deadline the runtime's start set, short of the one the
        // manual tick set.
        tokio::time::sleep_until(manual + INTERVAL * 2 / 3).await;
        assert_eq!(fired.load(Ordering::SeqCst), 0, "the manual tick reset it");
        tokio::time::sleep_until(manual + INTERVAL * 3 / 2).await;
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    /// PERSO-172: a backlog brings the next tick forward to the follow-up,
    /// and a tick that owes nothing more puts the schedule back on the
    /// interval.
    #[tokio::test]
    async fn a_backlog_brings_the_next_tick_forward_until_it_is_drained() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        let fired = Arc::new(AtomicUsize::new(0));
        let interval = Duration::from_secs(3);
        let follow_up = Duration::from_millis(50);
        let cadence = Cadence {
            startup: interval,
            interval,
            follow_up,
        };
        let _task = scheduler_with(&runtime, &fired, cadence);

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(fired.load(Ordering::SeqCst), 0, "nothing owed yet");

        // The first sync's backlog, noted on the runtime it started.
        runtime.note_backlog();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "the backlog ticked at the follow-up, not the interval"
        );
        assert_eq!(bodies.load(Ordering::SeqCst), 1);
        // That tick's body reported no backlog, so the next one waits.
        assert!(!runtime.backlog_owed());
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(fired.load(Ordering::SeqCst), 1, "back on the interval");
    }

    /// The follow-up never stretches a shorter interval.
    #[test]
    fn the_gap_is_the_interval_unless_a_backlog_is_owed() {
        let (interval, follow_up) = (Duration::from_secs(900), BACKLOG_FOLLOW_UP);
        assert_eq!(gap(interval, follow_up, false), interval);
        assert_eq!(gap(interval, follow_up, true), follow_up);
        let short = Duration::from_secs(2);
        assert_eq!(gap(short, follow_up, true), short);
    }

    /// A drain is not a tick: it does not move the deadline.
    #[tokio::test]
    async fn a_drain_does_not_push_the_scheduled_tick_back() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        let before = *runtime.completed_ticks().borrow();
        assert!(runtime.drain().await.ran);
        assert_eq!(*runtime.completed_ticks().borrow(), before);
        runtime.tick(TickKind::Quick).await;
        assert!(*runtime.completed_ticks().borrow() > before, "a tick does");
    }

    /// `sync_interval_secs = 0` schedules nothing, and so does a local-only
    /// account; a Graph account is scheduled like an IMAP one, and its `0`
    /// turns it off the same way.
    #[tokio::test]
    async fn a_zero_interval_schedules_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        assert!(
            schedulable(&runtime, &config()).is_some(),
            "the default schedules"
        );

        let mut off = config();
        off.imap.sync_interval_secs = 0;
        assert!(spawn(&runtime, canonical(), &off).is_none());

        let local = AccountConfig {
            name: "alpha".to_string(),
            ..Default::default()
        };
        assert!(local.is_local_only());
        assert!(spawn(&runtime, canonical(), &local).is_none());

        let graph = AccountConfig {
            name: "alpha".to_string(),
            auth_method: AuthMethod::Graph,
            ..Default::default()
        };
        assert_eq!(
            schedulable(&runtime, &graph),
            graph.imap.sync_interval(),
            "a Graph account with no [accounts.imap] gets the default interval"
        );
        assert!(schedulable(&runtime, &graph).is_some());
        let mut graph_off = graph;
        graph_off.imap.sync_interval_secs = 0;
        assert!(spawn(&runtime, canonical(), &graph_off).is_none());
    }

    /// A retired runtime fires nothing, even with its deadline long past, and
    /// its scheduler stops.
    #[tokio::test]
    async fn a_retired_runtime_schedules_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        let fired = Arc::new(AtomicUsize::new(0));
        let task = scheduler(&runtime, &fired);

        assert!(runtime.retire(Duration::from_secs(1)).await);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("a retired runtime stops its scheduler")
            .unwrap();
        tokio::time::sleep(INTERVAL * 2).await;
        assert_eq!(fired.load(Ordering::SeqCst), 0);
        assert_eq!(bodies.load(Ordering::SeqCst), 0);
    }

    /// A blocked runtime gets no scheduler; one forced onto it never fires,
    /// startup tick included, since the tick would be refused, and it does
    /// not loop on a deadline that nothing moves.
    #[tokio::test]
    async fn a_blocked_runtime_schedules_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let holder = EngineLock::try_acquire_at(&dir.path().join("store.lock"), "alpha")
            .unwrap()
            .expect("a free lock");
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        assert!(matches!(runtime.readiness(), Readiness::Blocked { .. }));
        assert!(spawn(&runtime, canonical(), &config()).is_none());

        let fired = Arc::new(AtomicUsize::new(0));
        let cadence = Cadence {
            startup: INTERVAL / 10,
            interval: INTERVAL,
            follow_up: INTERVAL / 6,
        };
        let _task = scheduler_with(&runtime, &fired, cadence);
        tokio::time::sleep(INTERVAL * 5 / 2).await;
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "a tick that would be refused is not fired"
        );
        assert_eq!(bodies.load(Ordering::SeqCst), 0);
        drop(holder);
    }
}
