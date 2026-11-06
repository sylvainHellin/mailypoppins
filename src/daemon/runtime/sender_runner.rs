//! The per-account blocked-senders runner (PERSO-261): after every tick that
//! ran a body, move what arrived in the Inbox from a blocked sender to Junk.
//!
//! It waits on the record the scheduler and the hook runner wait on
//! ([`AccountRuntime::completed_ticks`]), so a watcher tick, a scheduled tick,
//! a client's `mp sync` and a startup fetch all wake it, and it scans once
//! when it starts. The scan, the judged mark and the moves are
//! [`crate::daemon::senders`]; this module is the wake-up, and what follows a
//! scan that moved something: a drain request, so the server half runs soon
//! rather than at the next tick, and the mailbox counts, so every client
//! re-reads the Inbox and the Junk folder.
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

use super::account::{AccountRuntime, Readiness};

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
        runtime.completed_ticks(),
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
        let _ = ticks.borrow_and_update();
        if *retired.borrow() {
            break;
        }
        let Some(live) = runtime.upgrade() else {
            break;
        };
        scan_once(&live, &canonical, &cfg).await;
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

/// Scan once, and when something moved ask for a drain and publish the
/// counts that moved.
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
        let entries = senders::load(&senders::list_path())?;
        senders::scan(&store, &scan_cfg, &entries)
    })
    .await;
    let outcome = match scanned {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => return warn!("[senders] scanning {account}: {e:#}"),
        Err(e) => return warn!("[senders] the scan task of {account} {e}"),
    };
    if outcome.moved == 0 {
        debug!("[senders] {account}: nothing to move");
        return;
    }
    runtime.request_drain();
    super::publish::publish_changed_counts(canonical, cfg).await;
}
