//! The typed sync outcome: from an engine [`SyncResult`] to the wire's
//! [`SyncCompleted`] (#0122, plan unit P3b-U6).
//!
//! The payload type lives in `mp-protocol`, because the CLI and the GUI both
//! deserialise it and neither may link this crate. The constructor lives here
//! for the mirror-image reason: `mp-protocol` knows nothing about the engine.
//! It is a free function rather than an inherent `impl`, which a foreign type
//! does not take, and rather than a `From`, which would have to carry the
//! account name, the failed-mutation count and the error in a tuple.
//!
//! The constructor is where the tick's facts become a payload: it widens every
//! `usize` to `u64`, canonicalises `non_converging` once so both formatters are
//! a straight walk over it, and decides the severity so no client has to.

use mp_protocol::events::{Arrival, Severity, SyncCompleted};

use crate::sync::SyncResult;

/// Test-only hook: commit one `Change::SyncCompleted` per element of this JSON
/// array after **every** `state.bootstrap`, against the first configured
/// account.
///
/// Phase 3b schedules no tick, so nothing would otherwise ever publish an
/// outcome and every socket assertion about one would be vacuous. A bare object
/// is read as an array of one. Each element's `account` is ignored and replaced
/// by the configured name; every other field travels verbatim, `severity`
/// included, so a test can pin a severity no fake sync could produce.
///
/// Unset, empty or unparseable it does nothing, exactly as the other four
/// hooks behave. No flag exposes it, so `mp --help` never moves; documented in
/// `docs/daemon-operations.md`.
///
/// It had a second gate until P5-U8, the account-runtimes opt-in, because a
/// hook that fired without a runtime would have reported on an engine that was
/// not running. Runtimes are on by default now, so the gate is gone with the
/// variable.
pub const FAKE_SYNC_OUTCOME_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME";

/// The payload for one finished tick.
///
/// `error` is the engine's failure rendered with `{:#}`; `failed_mutations` is
/// what the tick's mutation drains rolled back. The severity rule, in this
/// order: an error outranks everything; otherwise a non-empty `non_converging`
/// or a rolled-back mutation is a warning; otherwise the tick is clean. A
/// deadline stop and a deferred prune raise nothing, because both are progress
/// rather than failure (#0113, #0072).
pub fn from_sync_result(
    account: &str,
    result: &SyncResult,
    failed_mutations: u64,
    error: Option<String>,
) -> SyncCompleted {
    let mut non_converging = result.non_converging.clone();
    non_converging.sort();
    non_converging.dedup();

    let severity = if error.is_some() {
        Severity::Error
    } else if !non_converging.is_empty() || failed_mutations > 0 {
        Severity::Warning
    } else {
        Severity::Ok
    };

    SyncCompleted {
        account: account.to_string(),
        severity,
        saved: result.saved as u64,
        skipped: result.skipped as u64,
        flags_updated: result.flags_updated as u64,
        pruned: result.pruned as u64,
        prunes_deferred: result.prunes_deferred as u64,
        uid_rebound: result.uid_rebound as u64,
        uidvalidity_resets: result.uidvalidity_resets as u64,
        bodies_truncated: result.bodies_truncated as u64,
        non_converging,
        failed_mutations,
        error,
        // The list the engine already keeps, widened onto the wire (P5-U8):
        // the runtime's tick is the only carrier the desktop notification of
        // #0009 has left, because nobody asked for it and nobody reads its
        // answer.
        new_inbox_mail: result
            .new_inbox_mail
            .iter()
            .map(|mail| Arrival {
                from: mail.from.clone(),
                subject: mail.subject.clone(),
            })
            .collect(),
    }
}

/// The outcomes [`FAKE_SYNC_OUTCOME_ENV`] arms, in array order, or an empty
/// vector when the hook is inert.
pub fn fake_sync_outcomes() -> Vec<SyncCompleted> {
    let raw = match std::env::var(FAKE_SYNC_OUTCOME_ENV) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // A bare object is an array of one; anything that is not a payload at all
    // arms nothing, rather than arming the prefix that happened to parse.
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Array(items)) => items
            .into_iter()
            .map(serde_json::from_value::<SyncCompleted>)
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_default(),
        Ok(object @ serde_json::Value::Object(_)) => serde_json::from_value(object)
            .map(|one| vec![one])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unset is inert, and so is every shape that is not a payload.
    #[test]
    fn the_hook_is_inert_until_it_is_armed() {
        assert_eq!(
            FAKE_SYNC_OUTCOME_ENV,
            "MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME"
        );
        assert!(fake_sync_outcomes().is_empty());
    }

    /// The widening is a widening and not a reinterpretation: the counts a
    /// `usize` held arrive unchanged.
    #[test]
    fn every_counter_widens_verbatim() {
        let outcome = from_sync_result(
            "alpha",
            &SyncResult {
                saved: 1,
                skipped: 2,
                flags_updated: 3,
                pruned: 4,
                prunes_deferred: 5,
                uid_rebound: 6,
                bodies_truncated: 7,
                uidvalidity_resets: 8,
                ..SyncResult::default()
            },
            0,
            None,
        );
        assert_eq!(
            (
                outcome.saved,
                outcome.skipped,
                outcome.flags_updated,
                outcome.pruned,
                outcome.prunes_deferred,
                outcome.uid_rebound,
                outcome.bodies_truncated,
                outcome.uidvalidity_resets,
            ),
            (1, 2, 3, 4, 5, 6, 7, 8)
        );
        assert_eq!(outcome.severity, Severity::Ok);
    }
}
