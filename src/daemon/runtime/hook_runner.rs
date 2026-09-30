//! The per-account hook runner (#0135): after every tick that ran a sync body,
//! scan the account's hooks and run the ones a new message matched.
//!
//! It waits on the same record the scheduler does
//! ([`AccountRuntime::completed_ticks`]), so a watcher tick, a scheduled tick,
//! a client's `mp sync` and a startup fetch all wake it, and it scans once when
//! it starts, which is what picks up a claim-free arrival a crashed daemon
//! ingested and never scanned. The scan and the cursor are
//! [`crate::daemon::hooks`]; this module is only the wake-up and the order:
//! claim everything first, then run the commands one at a time, in UID order.
//!
//! # What gets a runner
//!
//! A ready runtime of an IMAP account with at least one hook. A blocked
//! runtime ingests nothing, a local-only account has no mail arriving, and a
//! Graph row has no raw message to authenticate. An account whose hooks were
//! all removed gets no runner, but its stale cursors are dropped at once, so a
//! hook added back later arms afresh.
//!
//! # Lifetime
//!
//! The scheduler's rules: spawned for one runtime, held weakly, stopped by its
//! retirement or its drop. A reload that edits a hook changes the effective
//! account, which restarts the runtime and with it this runner.

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use log::{debug, info, warn};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::{AccountConfig, AuthMethod};
use crate::daemon::hooks::{self, state};

use super::account::{AccountRuntime, Readiness};

/// Start the hook runner for `runtime`, if its account has hooks to run.
pub fn spawn(runtime: &Arc<AccountRuntime>, cfg: &AccountConfig) -> Option<JoinHandle<()>> {
    let state_path = state::state_path(&cfg.name);
    if cfg.hooks.is_empty() {
        if state_path.exists() {
            let cfg = cfg.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) = hooks::forget_removed(&cfg, &state_path) {
                    warn!("[hooks] {}: dropping stale cursors: {e:#}", cfg.name);
                }
            });
        }
        return None;
    }
    let why_not = if cfg.is_local_only() {
        Some("has no server")
    } else if runtime.readiness() != Readiness::Ready {
        Some("is blocked")
    } else if cfg.auth_method == AuthMethod::Graph {
        Some("is a Graph account, whose rows carry no raw message to authenticate")
    } else {
        None
    };
    if let Some(why) = why_not {
        warn!(
            "[hooks] {} has hooks but it {why}; none of them run",
            cfg.name
        );
        return None;
    }
    info!(
        "[hooks] running {} hook(s) for {}",
        cfg.hooks.len(),
        cfg.name
    );
    Some(tokio::spawn(run(
        Arc::downgrade(runtime),
        runtime.retired(),
        runtime.completed_ticks(),
        cfg.clone(),
        state_path,
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
    cfg: AccountConfig,
    state_path: PathBuf,
) {
    loop {
        let _ = ticks.borrow_and_update();
        if runtime.upgrade().is_none() || *retired.borrow() {
            break;
        }
        scan_and_fire(&cfg, &state_path).await;
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
        "[hooks] {}'s runtime retired; stopped running hooks",
        cfg.name
    );
}

/// Scan once and run whatever was claimed.
pub async fn scan_and_fire(cfg: &AccountConfig, state_path: &std::path::Path) {
    let account = cfg.name.clone();
    let scan_cfg = cfg.clone();
    let path = state_path.to_path_buf();
    let scanned = tokio::task::spawn_blocking(move || {
        let store = crate::store::Store::open(crate::config::store_path(&scan_cfg.name))?;
        let blobs = crate::store::blobs::BlobStore::for_account(&scan_cfg.name);
        let claimed = hooks::scan(&store, &blobs, &scan_cfg, &path)?;
        let prepared: Vec<_> = claimed
            .into_iter()
            .map(|claim| {
                let hook = scan_cfg
                    .hooks
                    .iter()
                    .find(|hook| hook.name == claim.hook)
                    .cloned()
                    .expect("a claim names a configured hook");
                let prepared =
                    hooks::prepare(&store, &blobs, &scan_cfg.name, &hook, claim.row_id, false);
                (hook, claim, prepared)
            })
            .collect();
        anyhow::Ok(prepared)
    })
    .await;
    let prepared = match scanned {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(e)) => return warn!("[hooks] scanning {account}: {e:#}"),
        Err(e) => return warn!("[hooks] the scan task of {account} {e}"),
    };
    if prepared.is_empty() {
        debug!("[hooks] {account}: nothing new to run");
    }
    for (hook, claim, prepared) in prepared {
        match prepared {
            Ok(prepared) => {
                hooks::fire(&account, &hook, prepared, state_path, false).await;
            }
            Err(e) => warn!(
                "[hooks] {account}/{}: {} (uid {}) was claimed but could not be prepared, and does not run: {e:#}",
                hook.name, claim.message_id, claim.uid
            ),
        }
    }
}
