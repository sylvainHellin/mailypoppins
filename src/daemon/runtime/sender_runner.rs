//! The per-account blocked-senders runner (PERSO-261): after every tick that
//! ran a body, move what arrived in the Inbox from a blocked sender to Junk.
//!
//! It waits on the record the hook runner and the arrival runner wait on
//! ([`AccountRuntime::judging`]), so a watcher tick, a scheduled tick, a
//! client's `mp sync`, a startup fetch and a `mp sync --mailbox` that brought
//! new inbox mail all wake it (PERSO-186), and it scans once when it starts. The scan, the judged mark and the moves are
//! [`crate::daemon::senders`]; this module is the wake-up, and what follows a
//! scan that moved something: a drain request, so the server half runs soon
//! rather than at the next tick, the removal of each moved row and the Junk
//! listing's invalidate, so every client drops the rows (PERSO-286), and the
//! mailbox counts, so every client re-reads the sidebar.
//!
//! # What gets a runner
//!
//! A ready runtime of an account with a server, IMAP or Graph, whatever the
//! list holds: an empty list still marks what arrives as judged, so a sender
//! blocked later is not applied to mail that came before by the scan (the
//! block call's own move is what reaches that mail). A blocked runtime
//! ingests nothing, and a local-only account has no mail arriving.
//!
//! # Lifetime
//!
//! The scheduler's rules: spawned for one runtime, held weakly, stopped by its
//! retirement or its drop.

use std::sync::{Arc, Weak};

use log::{debug, info, warn};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::AccountConfig;
use crate::daemon::senders;
use crate::daemon::state::CanonicalState;

use super::account::{AccountRuntime, Judge, Readiness};

/// Start the runner for `runtime`, if its account can receive mail.
pub fn spawn(
    runtime: &Arc<AccountRuntime>,
    canonical: Arc<CanonicalState>,
    cfg: &AccountConfig,
) -> Option<JoinHandle<()>> {
    if cfg.is_local_only() || runtime.readiness() != Readiness::Ready {
        return None;
    }
    Some(tokio::spawn(run(
        Arc::downgrade(runtime),
        runtime.retired(),
        runtime.judging(),
        canonical,
        cfg.clone(),
    )))
}

/// Resolve once `retired` goes `true` or its runtime is dropped.
async fn stopped(retired: &mut watch::Receiver<bool>) {
    let _ = retired.wait_for(|retired| *retired).await;
}

/// One account's runner, until the runtime it was spawned for retires.
async fn run(
    runtime: Weak<AccountRuntime>,
    mut retired: watch::Receiver<bool>,
    mut ticks: watch::Receiver<Instant>,
    canonical: Arc<CanonicalState>,
    cfg: AccountConfig,
) {
    loop {
        let tick = *ticks.borrow_and_update();
        if *retired.borrow() {
            break;
        }
        let Some(live) = runtime.upgrade() else {
            break;
        };
        scan_once(&live, &canonical, &cfg).await;
        // What this tick brought in has been judged, whatever the scan did:
        // its new mail may be announced now (PERSO-186).
        live.judged(Judge::Senders, tick);
        drop(live);
        tokio::select! {
            biased;
            () = stopped(&mut retired) => break,
            changed = ticks.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
    info!(
        "[senders] {}'s runtime retired; stopped judging its mail",
        cfg.name
    );
}

/// Scan once, and when something moved ask for a drain and publish the rows
/// that left the Inbox and the counts that moved.
pub async fn scan_once(
    runtime: &AccountRuntime,
    canonical: &Arc<CanonicalState>,
    cfg: &AccountConfig,
) {
    let account = cfg.name.clone();
    let scan_cfg = cfg.clone();
    let scanned = tokio::task::spawn_blocking(move || {
        let Some(store) = crate::store::open_store(&scan_cfg.name) else {
            return anyhow::Ok(senders::Scanned::default());
        };
        senders::scan(&store, &scan_cfg, &senders::list_path())
    })
    .await;
    let outcome = match scanned {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => return warn!("[senders] scanning {account}: {e:#}"),
        Err(e) => return warn!("[senders] the scan task of {account} {e}"),
    };
    if outcome.moved.is_empty() {
        debug!("[senders] {account}: nothing to move");
        return;
    }
    runtime.request_drain();
    super::publish::publish_departures(canonical, &account, &outcome.moved, Some(&outcome.junk));
    super::publish::publish_changed_counts(canonical, cfg).await;
}

/// Move the mail in `cfg`'s Inbox from the senders a `sender.block` owed the
/// account while its store was opening to Junk ([`senders::move_owed`]); the
/// store is open by now, so its integrity check has run. A move that fails
/// stays owed for the account's next start. A move asks `runtime` for a
/// drain and publishes the rows that left the Inbox and the counts, as the
/// block call does for an open account (PERSO-286). The moves owed to an
/// account `configured` no longer names are dropped on the way
/// ([`senders::owed`]).
pub async fn move_owed(
    runtime: Option<&AccountRuntime>,
    canonical: &Arc<CanonicalState>,
    cfg: &AccountConfig,
    configured: &[AccountConfig],
) {
    let account = cfg.name.clone();
    let move_cfg = cfg.clone();
    let configured: Vec<String> = configured.iter().map(|a| a.name.clone()).collect();
    let moved = tokio::task::spawn_blocking(move || {
        let list = senders::list_path();
        let configured: Vec<&str> = configured.iter().map(String::as_str).collect();
        if senders::owed(&list, &move_cfg.name, &configured)?.is_empty() {
            return Ok(senders::Moved::Rows {
                mailbox: String::new(),
                departed: Vec::new(),
            });
        }
        let Some(store) = crate::store::open_store(&move_cfg.name) else {
            anyhow::bail!("its store did not open; the move stays owed");
        };
        senders::move_owed(&store, &move_cfg, &list)
    })
    .await;
    let (junk, departed) = match moved {
        Ok(Ok(senders::Moved::Rows { mailbox, departed })) if !departed.is_empty() => {
            info!("[senders] {account}: moved {} message(s) from senders blocked while its store was opening to Junk", departed.len());
            (mailbox, departed)
        }
        Ok(Ok(senders::Moved::Rows { .. })) => {
            return debug!("[senders] {account}: nothing owed to move");
        }
        Ok(Ok(senders::Moved::NoJunk { matched })) => {
            return warn!("[senders] {account}: {matched} message(s) from a blocked sender stay in the Inbox: {}", senders::no_junk_reason(&account));
        }
        Ok(Err(e)) => return warn!("[senders] moving the mail owed to {account}: {e:#}"),
        Err(e) => return warn!("[senders] the owed move of {account} {e}"),
    };
    if let Some(runtime) = runtime {
        runtime.request_drain();
    }
    super::publish::publish_departures(canonical, &account, &departed, Some(&junk));
    super::publish::publish_changed_counts(canonical, cfg).await;
}
