//! Typed event payloads, for the kinds whose payload is a shape rather than a
//! resource reference (P3b-U6).
//!
//! A `state.event` notification carries `{instance_id, revision, kind,
//! payload}`, and most kinds put a resource identifier in that payload. A sync
//! outcome is different: it is the outcome of a command, it names no resource,
//! and both the CLI and the GUI deserialise the whole of it. The struct
//! therefore lives here, beside the framing, rather than in the daemon crate a
//! client must never link.
//!
//! What it deliberately does not carry is a rendered line.
//! [`mp_client::format`] turns a [`SyncCompleted`] into the TUI's status line
//! and into `mp sync`'s lines; a payload that carried either would make every
//! other client re-parse English.

use serde::{Deserialize, Serialize};

/// The `kind` a completed sync tick travels as.
pub const KIND_SYNC_COMPLETED: &str = "sync.completed";

/// How a client should present one tick.
///
/// Decided by the daemon and carried on the wire rather than recomputed on
/// arrival: a client that derived its own could disagree with the daemon after
/// a rule change and show a warning tick as clean, which is the one thing the
/// non-converging detector (#0115) and the rolled-back-mutation report (#0039)
/// exist to prevent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The tick did what it could and nothing needs attention.
    Ok,
    /// The tick ran, and something in it needs attention.
    Warning,
    /// The tick failed.
    Error,
}

/// What one sync tick did, as the `sync.completed` event carries it.
///
/// Every counter is `u64` on the wire even though the engine counts in `usize`:
/// a wire shape may not change width with the machine that produced it.
/// `non_converging` is canonical - sorted and deduplicated by the constructor -
/// so two payloads describing one tick compare equal and a formatter is a
/// straight walk over it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCompleted {
    /// The account this tick belongs to.
    pub account: String,
    /// How a client presents it.
    pub severity: Severity,
    /// Messages ingested as new rows.
    pub saved: u64,
    /// Messages the store already held.
    pub skipped: u64,
    /// Rows whose flags were updated from the server.
    pub flags_updated: u64,
    /// Rows deleted because the server no longer lists their UID.
    pub pruned: u64,
    /// Rows found vanished and not deleted, because the pass came back short
    /// and the diff cannot be trusted yet (#0072).
    pub prunes_deferred: u64,
    /// Rows rebound to a new UID after a UIDVALIDITY reset.
    pub uid_rebound: u64,
    /// Mailboxes refetched in full after a UIDVALIDITY mismatch.
    pub uidvalidity_resets: u64,
    /// How many mailboxes stopped at the body-fetch deadline with mail still
    /// to download (#0113). A count and not a list of names, because that is
    /// what `SyncResult` holds today; widening it is its own ticket.
    pub bodies_truncated: u64,
    /// Server names of the mailboxes that downloaded the same UIDs again
    /// (#0115), sorted and deduplicated.
    pub non_converging: Vec<String>,
    /// Queued mutations that failed and were rolled back (#0039).
    pub failed_mutations: u64,
    /// The engine's error, rendered with `{:#}`, or `null` on a tick that ran.
    /// The one free-text field there is, and never a status line.
    pub error: Option<String>,
}
