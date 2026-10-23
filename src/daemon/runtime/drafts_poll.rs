//! The per-account drafts poll (#0143): the drafts mirror's reconcile once a
//! minute between sync ticks.
//!
//! The reconcile runs after every tick's sync, and a tick comes from the
//! scheduler once an account has gone `imap.sync_interval_secs` without one,
//! or from the watcher when INBOX moves. The watcher hears INBOX only, so on a
//! quiet inbox the server Drafts folder was read every fifteen minutes (the
//! default interval): on 2026-10-23 a draft started in Outlook reached neither
//! machine for seven minutes, and a draft pushed from the Mac and sent five
//! minutes later never reached the home server at all. Outlook and Apple Mail
//! converge with each other in seconds, which is the bar.
//!
//! The poll reconciles alone, without a sync: one `SELECT` of the Drafts
//! folder, a `UID SEARCH` and the linking headers of each copy, on a pooled
//! session. It waits [`DRAFTS_POLL`] from the last tick or the last poll,
//! whichever is later, so an account whose ticks come often enough is not
//! polled at all. It runs under the runtime's turn
//! ([`AccountRuntime::reconcile_drafts`]), so it never runs beside a tick or a
//! drain.
//!
//! # What gets a poll
//!
//! A ready runtime of an account that mirrors its drafts over IMAP and syncs
//! on a schedule slower than the poll: an account with `sync_interval_secs =
//! 0` asked for no background network, and one whose interval is no longer
//! than the poll's is reconciled often enough by its ticks.
//!
//! # Lifetime
//!
//! The scheduler's rules: spawned for one runtime, held weakly, stopped by its
//! retirement or its drop.

use std::sync::{Arc, Weak};
use std::time::Duration;

use futures::future::BoxFuture;
use log::{debug, info};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::AccountConfig;

use super::account::{AccountRuntime, Readiness};

/// How long an account may go without a reconcile of its drafts before the
/// poll runs one.
pub const DRAFTS_POLL: Duration = Duration::from_secs(60);

/// Start the drafts poll for `runtime`, if it gets one (see the module docs).
pub fn spawn(runtime: &Arc<AccountRuntime>, cfg: &AccountConfig) -> Option<JoinHandle<()>> {
    let poll = pollable(runtime, cfg, DRAFTS_POLL)?;
    info!(
        "[drafts] polling the server drafts of {} every {}s between syncs",
        cfg.name,
        poll.as_secs()
    );
    Some(tokio::spawn(run(
        Arc::downgrade(runtime),
        runtime.retired(),
        runtime.completed_ticks(),
        poll,
        Arc::new(|runtime: Arc<AccountRuntime>| {
            Box::pin(async move {
                runtime.reconcile_drafts().await;
            }) as BoxFuture<'static, ()>
        }),
    )))
}

/// The poll `runtime` gets, or `None` (see the module docs).
fn pollable(runtime: &AccountRuntime, cfg: &AccountConfig, poll: Duration) -> Option<Duration> {
    let why_not = if !crate::draft_mirror::mirror_enabled(cfg) {
        "does not mirror its drafts"
    } else if cfg.imap.host.trim().is_empty() {
        "has no IMAP server"
    } else if runtime.readiness() != Readiness::Ready {
        "is blocked"
    } else {
        match cfg.imap.sync_interval() {
            None => "has sync_interval_secs = 0",
            Some(interval) if interval <= poll => "syncs at least as often as the poll",
            Some(_) => return Some(poll),
        }
    };
    debug!(
        "[drafts] not polling the drafts of {}: it {why_not}",
        cfg.name
    );
    None
}

/// What the poll does when it is due: the production one reconciles, a
/// test's counts.
type Fire = Arc<dyn Fn(Arc<AccountRuntime>) -> BoxFuture<'static, ()> + Send + Sync>;

/// Resolve once `retired` goes `true` or its runtime is dropped.
async fn stopped(retired: &mut watch::Receiver<bool>) {
    let _ = retired.wait_for(|retired| *retired).await;
}

/// One account's drafts poll, until the runtime it was spawned for retires.
async fn run(
    runtime: Weak<AccountRuntime>,
    mut retired: watch::Receiver<bool>,
    mut ticks: watch::Receiver<Instant>,
    poll: Duration,
    fire: Fire,
) {
    let account = runtime
        .upgrade()
        .map(|runtime| runtime.account().to_string())
        .unwrap_or_default();
    let mut polled: Option<Instant> = None;
    loop {
        let last = *ticks.borrow_and_update();
        let due = polled.map_or(last, |polled| polled.max(last)) + poll;
        tokio::select! {
            biased;
            () = stopped(&mut retired) => break,
            // A tick reconciled after its sync: the poll moves with it.
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
        polled = Some(Instant::now());
        debug!(
            "[drafts] {account} has gone {}s without a drafts reconcile; polling",
            poll.as_secs()
        );
        fire(runtime).await;
    }
    debug!("[drafts] {account}'s runtime retired; stopped polling its drafts");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::future::FutureExt;

    use super::super::account::{DraftsHook, DrainHook, TickHooks, TickKind};

    /// A short poll, so the tests run in real time.
    const POLL: Duration = Duration::from_millis(200);

    fn config(name: &str) -> AccountConfig {
        let mut cfg = AccountConfig {
            name: name.to_string(),
            ..Default::default()
        };
        cfg.imap.host = "imap.example.com".to_string();
        cfg
    }

    /// A runtime whose drafts hook counts the reconciles it ran.
    fn started(dir: &std::path::Path, reconciles: &Arc<AtomicUsize>) -> Arc<AccountRuntime> {
        let quiet: DrainHook = Arc::new(|_ctx| async { String::new() }.boxed());
        let hooks = TickHooks {
            outbox: Arc::clone(&quiet),
            mutations: quiet,
            body: Arc::new(|_ctx| async { Ok(()) }.boxed()),
        };
        let counted = Arc::clone(reconciles);
        let drafts: DraftsHook = Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            async {}.boxed()
        });
        Arc::new(
            AccountRuntime::start_at_with_hooks(dir, config("tum"), 1, hooks)
                .unwrap()
                .with_drafts_hook(drafts),
        )
    }

    fn spawned(runtime: &Arc<AccountRuntime>) -> JoinHandle<()> {
        tokio::spawn(run(
            Arc::downgrade(runtime),
            runtime.retired(),
            runtime.completed_ticks(),
            POLL,
            Arc::new(|runtime: Arc<AccountRuntime>| {
                Box::pin(async move {
                    runtime.reconcile_drafts().await;
                }) as BoxFuture<'static, ()>
            }),
        ))
    }

    /// With no tick in between, the drafts are reconciled once per poll: on
    /// 2026-10-23 they were read only at a sync tick, fifteen minutes apart.
    #[tokio::test]
    async fn the_drafts_are_reconciled_between_ticks() {
        let dir = tempfile::tempdir().unwrap();
        let reconciles = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &reconciles);
        let poll = spawned(&runtime);
        tokio::time::sleep(POLL * 5 / 2).await;
        assert!(
            reconciles.load(Ordering::SeqCst) <= 2,
            "never more than one poll a period"
        );
        eventually(&reconciles, 2).await;
        poll.abort();
    }

    /// Wait, a few seconds at most, until `count` reaches `want`: a loaded
    /// test host may run a poll late, never early.
    async fn eventually(count: &AtomicUsize, want: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while count.load(Ordering::SeqCst) < want {
            assert!(Instant::now() < deadline, "{want} reconciles never came");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A tick reconciles after its sync, so it pushes the next poll back by a
    /// whole period.
    #[tokio::test]
    async fn a_tick_pushes_the_poll_back() {
        let dir = tempfile::tempdir().unwrap();
        let reconciles = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &reconciles);
        let poll = spawned(&runtime);
        tokio::time::sleep(POLL * 3 / 4).await;
        runtime.tick(TickKind::Quick).await;
        tokio::time::sleep(POLL / 2).await;
        assert_eq!(
            reconciles.load(Ordering::SeqCst),
            0,
            "the tick at 0.75 moved the poll to 1.75"
        );
        eventually(&reconciles, 1).await;
        poll.abort();
    }

    /// The poll stops with its runtime.
    #[tokio::test]
    async fn the_poll_stops_when_its_runtime_retires() {
        let dir = tempfile::tempdir().unwrap();
        let reconciles = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &reconciles);
        let poll = spawned(&runtime);
        assert!(runtime.retire(Duration::from_secs(1)).await);
        tokio::time::timeout(Duration::from_secs(2), poll)
            .await
            .expect("the poll ended")
            .unwrap();
        assert_eq!(reconciles.load(Ordering::SeqCst), 0);
    }

    /// Only an account that mirrors over IMAP, on a schedule slower than the
    /// poll, is polled.
    #[tokio::test]
    async fn only_a_mirrored_imap_account_on_a_slow_schedule_is_polled() {
        let dir = tempfile::tempdir().unwrap();
        let reconciles = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &reconciles);
        let mut cfg = config("tum");
        cfg.imap.sync_interval_secs = 900;
        assert_eq!(pollable(&runtime, &cfg, DRAFTS_POLL), Some(DRAFTS_POLL));

        let mut off = cfg.clone();
        off.drafts.mirror = false;
        assert_eq!(pollable(&runtime, &off, DRAFTS_POLL), None);

        let mut no_imap = cfg.clone();
        no_imap.imap.host = String::new();
        assert_eq!(pollable(&runtime, &no_imap, DRAFTS_POLL), None);

        let mut fast = cfg.clone();
        fast.imap.sync_interval_secs = 60;
        assert_eq!(pollable(&runtime, &fast, DRAFTS_POLL), None);

        let mut never = cfg;
        never.imap.sync_interval_secs = 0;
        assert_eq!(pollable(&runtime, &never, DRAFTS_POLL), None);
    }
}
