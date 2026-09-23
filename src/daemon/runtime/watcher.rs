//! The per-account watcher (P5-U8, #0124): what the TUI's two watcher threads
//! did, done by the account runtime.
//!
//! Until Phase 5 every client that wanted to hear about new mail ran its own
//! watcher: `src/tui/helpers.rs` held an IMAP IDLE loop and a 60 s Graph
//! enumeration per account, each on a thread of the TUI's, each holding a
//! server connection of its own. Two clients watched twice, a client that was
//! not running watched not at all, and the daemon - the process that holds the
//! engine lock and does the ingesting - watched nothing.
//!
//! It runs here now, once per account, beside the runtime that would do the
//! ingest anyway. A round that sees the mailbox move runs one
//! [`TickKind::Quick`] tick through [`tick_and_commit`], which is what
//! publishes the `sync.completed` every subscribed client reads, arrivals
//! included ([`mp_protocol::events::Arrival`]). The counts move with it, as one
//! [`Change::MailboxCounts`] per mailbox whose totals actually changed, so a
//! client's sidebar converges without asking.
//!
//! # Why a watcher is not a scheduler
//!
//! This is the watch the TUI had and nothing more: it reacts to a server
//! saying something changed. A periodic tick on an interval - the thing that
//! keeps a store fresh with no client anywhere - is the Phase 6 scheduler's,
//! and `BACKLOG.md` carries it.
//!
//! # What ends a watch
//!
//! The runtime it was spawned for retiring, which is what a configuration swap
//! does to an account it removed or changed (`config::reconcile`), or being
//! dropped, which is what a shutdown does. The watcher holds that runtime
//! weakly and races its retirement signal ([`AccountRuntime::retired`])
//! against every round and every pause, so a changed account never has two
//! watchers and an IDLE connection does not outlive the runtime it served. It
//! never looks its runtime up by name: after a swap the name belongs to the
//! replacement, which has its own watcher.
//!
//! A blocked runtime never watches: it holds no engine lock, so a tick it
//! started would refuse itself, and the engine that does hold the lock is
//! watching the same mailbox.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use log::{debug, info, warn};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::{AccountConfig, AuthMethod, ImapConfig};
use crate::daemon::server::commit_tick;
use crate::daemon::state::{CanonicalState, Change};

use super::account::{off_thread, AccountRuntime, TickKind};

/// The mailbox an IDLE round watches, which is the one the TUI watched.
const WATCHED_MAILBOX: &str = "INBOX";

/// How long one IDLE round lasts before it is renewed, in seconds.
///
/// The TUI's number. A bounded round is what makes a stopped runtime
/// observable at all, and what keeps a server that silently dropped the
/// connection from being waited on for ever.
const IDLE_ROUND_SECS: u64 = 300;

/// [`crate::imap_client::watch_mailbox`]'s answer for a round that expired
/// without the mailbox changing.
const IDLE_TIMED_OUT: i32 = 2;

/// How long the Graph poller waits between enumerations.
///
/// Still an enumeration of the whole folder every minute, exactly as the TUI's
/// poller did it; #0042's delta query is what removes it, and it is a daemon
/// item now.
const GRAPH_POLL: Duration = Duration::from_secs(60);

/// First gap after a failed round, doubling to [`MAX_BACKOFF`].
const BASE_BACKOFF: Duration = Duration::from_secs(30);

/// The longest a watcher waits before trying again.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Start watching `cfg`'s server for `runtime`, if it has one.
///
/// A local-only account has nothing to watch and gets no task: the whole point
/// of the loop is a server connection. The handle is the test's; the daemon
/// lets the task run until the runtime it was spawned for retires.
pub fn spawn(
    runtime: &Arc<AccountRuntime>,
    canonical: Arc<CanonicalState>,
    cfg: AccountConfig,
) -> Option<JoinHandle<()>> {
    if cfg.is_local_only() {
        debug!("[watcher] {} has no server to watch", cfg.name);
        return None;
    }
    let source = if cfg.auth_method == AuthMethod::Graph {
        Source::Graph {
            client: None,
            known: None,
        }
    } else {
        Source::Imap
    };
    Some(tokio::spawn(watch(
        Arc::downgrade(runtime),
        runtime.retired(),
        canonical,
        cfg,
        source,
    )))
}

/// What one round watches.
enum Source {
    /// One IMAP IDLE round on [`WATCHED_MAILBOX`].
    Imap,
    /// One Graph enumeration of the inbox.
    Graph {
        /// Built on the first round, and again after a failed one: the token
        /// is the likeliest thing to have gone stale and it lives here.
        client: Option<crate::graph::GraphClient>,
        /// The *set* of ids, because one arrival plus one archive inside the
        /// same interval leaves the count untouched.
        known: Option<HashSet<String>>,
    },
    /// A round that never ends, which is what an IDLE round looks like from
    /// outside for its first five minutes.
    #[cfg(test)]
    Never,
}

impl Source {
    fn label(&self) -> &'static str {
        match self {
            Source::Imap => "IMAP IDLE",
            Source::Graph { .. } => "Graph",
            #[cfg(test)]
            Source::Never => "nothing",
        }
    }

    /// `true` when the mailbox moved during the round.
    async fn round(&mut self, cfg: &AccountConfig) -> anyhow::Result<bool> {
        match self {
            Source::Imap => idle_imap(cfg).await,
            Source::Graph { client, known } => poll_graph(cfg, client, known).await,
            #[cfg(test)]
            Source::Never => std::future::pending().await,
        }
    }

    /// Forget what a failed round may have left stale.
    fn failed(&mut self) {
        if let Source::Graph { client, .. } = self {
            *client = None;
        }
    }

    /// The pause after a round that succeeded: a poll waits, an IDLE round
    /// already did.
    fn gap(&self) -> Option<Duration> {
        matches!(self, Source::Graph { .. }).then_some(GRAPH_POLL)
    }
}

/// Resolve once `retired` goes `true` or its runtime is dropped.
async fn stopped(retired: &mut watch::Receiver<bool>) {
    let _ = retired.wait_for(|retired| *retired).await;
}

/// One account's watch, until the runtime it was spawned for retires.
///
/// Held weakly and stopped by the runtime's own signal, never by a lookup by
/// name: a swap that changes the account puts a *new* runtime under the same
/// name, and a watcher that only checked for the name would carry on with the
/// old configuration beside the new runtime's watcher. The signal is raced
/// against every round and every pause, so an IDLE connection does not outlive
/// its runtime by a round.
async fn watch(
    runtime: Weak<AccountRuntime>,
    mut retired: watch::Receiver<bool>,
    canonical: Arc<CanonicalState>,
    cfg: AccountConfig,
    mut source: Source,
) {
    info!("[watcher] watching {} over {}", cfg.name, source.label());
    let mut failures: u32 = 0;
    let mut counts: HashMap<String, (u64, u64)> = HashMap::new();

    loop {
        let round = tokio::select! {
            () = stopped(&mut retired) => break,
            round = source.round(&cfg) => round,
        };
        let pause = match round {
            Ok(changed) => {
                failures = 0;
                if changed {
                    let Some(runtime) = runtime.upgrade() else {
                        break;
                    };
                    tick(&runtime, &canonical, &cfg, &mut counts).await;
                }
                source.gap()
            }
            Err(e) => {
                source.failed();
                failures = failures.saturating_add(1);
                warn!(
                    "[watcher] watching {} failed ({failures} in a row): {e:#}",
                    cfg.name
                );
                Some(backoff(failures))
            }
        };
        if let Some(pause) = pause {
            tokio::select! {
                () = stopped(&mut retired) => break,
                () = tokio::time::sleep(pause) => {}
            }
        }
    }
    info!("[watcher] {}'s runtime retired; stopped watching", cfg.name);
}

/// The gap after `failures` consecutive failures: the TUI's curve, capped.
fn backoff(failures: u32) -> Duration {
    let doubled = BASE_BACKOFF.saturating_mul(1u32 << failures.saturating_sub(1).min(8));
    doubled.min(MAX_BACKOFF)
}

/// One IDLE round: `true` when the mailbox moved, `false` when the round
/// expired.
async fn idle_imap(cfg: &AccountConfig) -> anyhow::Result<bool> {
    let cfg = Arc::new(cfg.clone());
    off_thread("the IDLE watch", move || async move {
        let imap = ImapConfig::load(&cfg)?;
        crate::imap_client::watch_mailbox(&imap, WATCHED_MAILBOX, Some(IDLE_ROUND_SECS)).await
    })
    .await
    .and_then(|inner| inner)
    .map(|code| code != IDLE_TIMED_OUT)
}

/// One Graph enumeration: `true` when the inbox's id set differs from the
/// previous round's.
///
/// The first round only records, because a watcher that reported a change on
/// its first look would sync every account once per daemon start for no reason.
async fn poll_graph(
    cfg: &AccountConfig,
    client: &mut Option<crate::graph::GraphClient>,
    known: &mut Option<HashSet<String>>,
) -> anyhow::Result<bool> {
    match client.as_mut() {
        Some(existing) => {
            let graph = crate::config::GraphConfig::load(cfg)?;
            existing.refresh_token(&graph).await?;
        }
        None => {
            let graph = crate::config::GraphConfig::load(cfg)?;
            *client = Some(crate::graph::GraphClient::new_async(&graph).await?);
        }
    }
    let folder = client
        .as_ref()
        .expect("the client was built above")
        .enumerate_folder("inbox")
        .await?;
    let ids: HashSet<String> = folder.entries.into_keys().collect();
    let changed = known.as_ref().is_some_and(|previous| *previous != ids);
    *known = Some(ids);
    Ok(changed)
}

/// Run one quick tick on this watcher's own runtime and publish what moved.
async fn tick(
    runtime: &AccountRuntime,
    canonical: &Arc<CanonicalState>,
    cfg: &AccountConfig,
    counts: &mut HashMap<String, (u64, u64)>,
) {
    info!("[watcher] {} changed; ticking", cfg.name);
    // `commit_tick` publishes the `sync.completed`, arrivals included; a
    // blocked or joined tick carries no outcome and publishes nothing.
    let outcome = commit_tick(runtime, canonical, TickKind::Quick).await;
    if outcome.blocked {
        return;
    }
    publish_counts(canonical, &cfg.name, counts).await;
}

/// Commit one [`Change::MailboxCounts`] per mailbox whose totals moved.
///
/// Per mailbox and only on a change, because the event coalesces by resource
/// and a client answers each one with a `mailbox.list`: a tick that ingested
/// into the inbox may not cost every other mailbox a round trip.
async fn publish_counts(
    canonical: &Arc<CanonicalState>,
    account: &str,
    previous: &mut HashMap<String, (u64, u64)>,
) {
    let name = account.to_string();
    let read = tokio::task::spawn_blocking(move || {
        let store = crate::store::Store::open(crate::config::store_path(&name))?;
        crate::store::read::mailbox_read_counts(&store, &name)
    })
    .await;
    let fresh = match read {
        Ok(Ok(fresh)) => fresh,
        Ok(Err(e)) => return warn!("[watcher] counting the mailboxes of {account}: {e:#}"),
        Err(e) => return warn!("[watcher] the count task for {account} {e}"),
    };
    for (mailbox, counts) in fresh {
        let pair = (counts.total as u64, counts.unread as u64);
        if previous.get(&mailbox) == Some(&pair) {
            continue;
        }
        previous.insert(mailbox.clone(), pair);
        canonical.apply(Change::MailboxCounts {
            account: account.to_string(),
            mailbox,
            total: pair.0,
            unread: pair.1,
            // What the sidebar prints beside the label, which is the total
            // (`src/daemon/methods/mailbox.rs`).
            badge: pair.0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The curve widens and stops widening, and it never dips below the first
    /// gap: backing *off* may not retry sooner.
    #[test]
    fn the_backoff_widens_to_the_cap_and_never_dips_below_the_base() {
        assert_eq!(backoff(0), BASE_BACKOFF);
        assert_eq!(backoff(1), BASE_BACKOFF);
        assert_eq!(backoff(2), BASE_BACKOFF * 2);
        let widening: Vec<Duration> = (1..=10).map(backoff).collect();
        for pair in widening.windows(2) {
            assert!(pair[1] >= pair[0], "the curve narrowed: {widening:?}");
        }
        assert_eq!(backoff(10), MAX_BACKOFF);
    }

    fn runtime(dir: &std::path::Path) -> Arc<AccountRuntime> {
        use super::super::account::{DrainHook, TickHooks};
        use futures::future::FutureExt;
        let quiet: DrainHook = Arc::new(|_ctx| async { String::new() }.boxed());
        let hooks = TickHooks {
            outbox: Arc::clone(&quiet),
            mutations: quiet,
            body: Arc::new(|_ctx| async { Ok(()) }.boxed()),
        };
        let cfg = AccountConfig {
            name: "alpha".to_string(),
            ..Default::default()
        };
        Arc::new(AccountRuntime::start_at_with_hooks(dir, cfg, 1, hooks).expect("a start"))
    }

    /// A swap that changes an account retires the old runtime and starts a new
    /// one under the same name: the old watcher stops mid-round, and exactly
    /// one watcher, the replacement's, is left.
    #[tokio::test]
    async fn a_replaced_runtime_leaves_exactly_one_watcher() {
        let canonical = Arc::new(CanonicalState::new(
            crate::daemon::state::InstanceId::new("test"),
            Vec::new(),
        ));
        let cfg = AccountConfig {
            name: "alpha".to_string(),
            ..Default::default()
        };
        let (first_dir, second_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

        let old = runtime(first_dir.path());
        let old_watch = tokio::spawn(watch(
            Arc::downgrade(&old),
            old.retired(),
            Arc::clone(&canonical),
            cfg.clone(),
            Source::Never,
        ));
        // The swap: retire and drop the old one, start its replacement.
        let new = runtime(second_dir.path());
        let new_watch = tokio::spawn(watch(
            Arc::downgrade(&new),
            new.retired(),
            Arc::clone(&canonical),
            cfg,
            Source::Never,
        ));
        assert!(old.retire(Duration::from_secs(1)).await);

        tokio::time::timeout(Duration::from_secs(5), old_watch)
            .await
            .expect("the old watcher stopped inside its round")
            .expect("the old watcher's task");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!new_watch.is_finished(), "the replacement's watcher is still watching");

        // A runtime dropped without a retire (the shutdown) stops its watcher too.
        drop(new);
        tokio::time::timeout(Duration::from_secs(5), new_watch)
            .await
            .expect("a dropped runtime stops its watcher")
            .expect("the new watcher's task");
    }
}
