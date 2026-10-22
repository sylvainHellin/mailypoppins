//! Reading `state.event` notifications beside a daemon that is still finishing
//! its own startup.
//!
//! Self-contained on purpose, as [`super::readiness`] is: a test binary that
//! declares `mod support;` reaches it as `support::events`, and one that does
//! not includes this one file with
//! `#[path = "support/events.rs"] mod events;`.
//!
//! # Why a test cannot expect its own events alone
//!
//! The daemon answers on its socket before its account runtimes have started.
//! Each runtime starts off the startup path, commits the account's
//! `account.state_changed`, and then refreshes the health checks, whose
//! `diagnostic.check_changed` announces whatever that start flipped
//! (`spawn_account_runtimes` in `src/daemon/lifecycle.rs`). On a loaded
//! machine both land after a test's bootstrap, between the events it waits
//! for. A lifecycle event may arrive at any time anyway: the contract keeps it
//! through an overflow and past the poison, because no snapshot carries it.
//!
//! What is domain and what is lifecycle is the protocol's word, read from
//! [`mp_protocol::events::DOMAIN_KINDS`], which the daemon pins its own events
//! to: a domain kind added to the protocol cannot be mistaken here for one a
//! test may skip.

#![allow(dead_code)]

use std::time::Duration;

use mp_client::Connection;
use mp_protocol::events::{
    is_domain_kind, KIND_ACCOUNT_STATE_CHANGED, KIND_DIAGNOSTIC_CHECK_CHANGED,
};
use mp_protocol::{EventEnvelope, JSONRPC_VERSION, METHOD_STATE_EVENT};

/// The kinds the daemon publishes on its own while its account runtimes
/// start, after the socket already answers: each account's readiness and the
/// health checks its start flipped.
pub const STARTUP_KINDS: [&str; 2] = [KIND_ACCOUNT_STATE_CHANGED, KIND_DIAGNOSTIC_CHECK_CHANGED];

/// Whether `event` is a domain event: a change to the state a snapshot
/// carries, which a poisoned queue drops.
pub fn is_domain_event(event: &EventEnvelope) -> bool {
    is_domain_kind(&event.kind)
}

/// Whether `event` is one of the [`STARTUP_KINDS`].
pub fn is_startup_event(event: &EventEnvelope) -> bool {
    STARTUP_KINDS.contains(&event.kind.as_str())
}

/// The next `state.event` on `conn`, failing the test on another
/// notification, on a closed connection, or after `window`.
pub async fn next_event(conn: &mut Connection, window: Duration, label: &str) -> EventEnvelope {
    let notification = tokio::time::timeout(window, conn.next_notification())
        .await
        .unwrap_or_else(|_| panic!("{label}: no notification within {window:?}"))
        .unwrap_or_else(|| panic!("{label}: the daemon closed the connection"));
    assert_eq!(notification.jsonrpc, JSONRPC_VERSION, "{label}");
    assert_eq!(
        notification.method, METHOD_STATE_EVENT,
        "{label}: expected a state.event, got {notification:?}"
    );
    serde_json::from_value(notification.params.clone()).unwrap_or_else(|e| {
        panic!("{label}: the params are an event envelope: {e}; got {notification:?}")
    })
}

/// The next `state.event` that is not one of the daemon's own startup events,
/// for a test that waits for events of its own making.
///
/// `window` bounds each read, and the startup publishes at most two events per
/// configured account, so the whole wait is bounded too.
pub async fn next_event_past_startup(
    conn: &mut Connection,
    window: Duration,
    label: &str,
) -> EventEnvelope {
    loop {
        let event = next_event(conn, window, label).await;
        if !is_startup_event(&event) {
            return event;
        }
    }
}

/// The next domain `state.event`, skipping lifecycle ones, for a test that
/// waits for a change to the state.
pub async fn next_domain_event(
    conn: &mut Connection,
    window: Duration,
    label: &str,
) -> EventEnvelope {
    loop {
        let event = next_event(conn, window, label).await;
        if is_domain_event(&event) {
            return event;
        }
    }
}

/// Assert that no domain event arrives on `conn` for `window`, whatever
/// lifecycle events do.
///
/// Bounded by construction: it waits exactly `window` and never longer.
pub async fn assert_no_domain_event_within(conn: &mut Connection, window: Duration, label: &str) {
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Some(notification)) =
        tokio::time::timeout_at(deadline, conn.next_notification()).await
    {
        if notification.method != METHOD_STATE_EVENT {
            continue;
        }
        let event: EventEnvelope = serde_json::from_value(notification.params.clone())
            .unwrap_or_else(|e| panic!("{label}: the params are an event envelope: {e}"));
        assert!(
            !is_domain_event(&event),
            "{label}: a domain event arrived: {event:?}"
        );
    }
}
