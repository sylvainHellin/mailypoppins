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
//! The runtime leaving the table, which is what a configuration swap does to
//! an account it removed or changed (`config::reconcile`). The check is once
//! per round rather than a cancellation token, because a round is bounded (an
//! IDLE round expires, a poll sleeps) and a token would need a second lifetime
//! to hang on.
//!
//! A blocked runtime never watches: it holds no engine lock, so a tick it
//! started would refuse itself, and the engine that does hold the lock is
//! watching the same mailbox.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use log::{debug, info, warn};

use crate::config::{AccountConfig, AuthMethod, ImapConfig};
use crate::daemon::server::{tick_and_commit, RuntimeTable};
use crate::daemon::state::{CanonicalState, Change};

use super::account::{off_thread, TickKind};

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

/// Start watching `cfg`'s server, if it has one.
///
/// A local-only account has nothing to watch and gets no task: the whole point
/// of the loop is a server connection.
pub fn spawn(runtimes: Arc<RuntimeTable>, canonical: Arc<CanonicalState>, cfg: AccountConfig) {
    if cfg.is_local_only() {
        debug!("[watcher] {} has no server to watch", cfg.name);
        return;
    }
    tokio::spawn(watch(runtimes, canonical, cfg));
}

/// One account's watch, until its runtime leaves the table.
async fn watch(runtimes: Arc<RuntimeTable>, canonical: Arc<CanonicalState>, cfg: AccountConfig) {
    let graph = cfg.auth_method == AuthMethod::Graph;
    info!(
        "[watcher] watching {} over {}",
        cfg.name,
        if graph { "Graph" } else { "IMAP IDLE" }
    );
    let mut failures: u32 = 0;
    // The Graph poller's memory: the *set* of ids, because one arrival plus one
    // archive inside the same interval leaves the count untouched.
    let mut known: Option<HashSet<String>> = None;
    let mut counts: HashMap<String, (u64, u64)> = HashMap::new();
    let mut client: Option<crate::graph::GraphClient> = None;

    loop {
        if runtimes.get(&cfg.name).is_none() {
            info!("[watcher] {} has no runtime any more; stopping", cfg.name);
            return;
        }
        let round = if graph {
            poll_graph(&cfg, &mut client, &mut known).await
        } else {
            idle_imap(&cfg).await
        };
        match round {
            Ok(changed) => {
                failures = 0;
                if changed {
                    tick(&runtimes, &canonical, &cfg, &mut counts).await;
                }
                if graph {
                    tokio::time::sleep(GRAPH_POLL).await;
                }
            }
            Err(e) => {
                // The Graph token is the likeliest thing to have gone stale and
                // it lives in the client, so the next round builds a new one.
                client = None;
                failures = failures.saturating_add(1);
                warn!(
                    "[watcher] watching {} failed ({failures} in a row): {e:#}",
                    cfg.name
                );
                tokio::time::sleep(backoff(failures)).await;
            }
        }
    }
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

/// Run one quick tick and publish what moved.
async fn tick(
    runtimes: &Arc<RuntimeTable>,
    canonical: &Arc<CanonicalState>,
    cfg: &AccountConfig,
    counts: &mut HashMap<String, (u64, u64)>,
) {
    info!("[watcher] {} changed; ticking", cfg.name);
    // `tick_and_commit` publishes the `sync.completed`, arrivals included; a
    // blocked or joined tick carries no outcome and publishes nothing.
    let outcome = tick_and_commit(runtimes, canonical, &cfg.name, TickKind::Quick).await;
    if outcome.is_none_or(|outcome| outcome.blocked) {
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
}
