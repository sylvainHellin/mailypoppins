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

/// One message a tick ingested into an inbox, as the desktop notification of
/// #0009 reads it (P5-U8).
///
/// Two fields and no more: the notifier prints a sender and a subject, and a
/// payload that carried a row id or a uid would invite a client to treat an
/// arrival as an address into a list it has not refetched yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arrival {
    /// The `From:` header as the message carried it.
    pub from: String,
    /// The subject, empty when the message had none.
    pub subject: String,
}

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
    /// What arrived in an inbox on this tick, for the desktop notification
    /// (#0009, P5-U8).
    ///
    /// On the event rather than only in the answer to whoever asked for the
    /// pass, because a runtime's tick is a pass nobody asked for and its
    /// answer therefore has no reader. `default` so a payload written by a
    /// daemon that predates the field still decodes as a tick that notified
    /// about nothing.
    #[serde(default)]
    pub new_inbox_mail: Vec<Arrival>,
}

/// The `kind` a completed configuration swap travels as.
pub const KIND_CONFIG_CHANGED: &str = "config.changed";

/// The `kind` a rejected configuration candidate travels as.
pub const KIND_CONFIG_INVALID: &str = "config.invalid";

/// What one configuration swap did, as the `config.changed` event carries it
/// and as `config.reload` returns it (P3b-U8).
///
/// The three lists are account names, sorted lexicographically so two daemons
/// reconciling the same edit report it identically. `config_revision` is the
/// counter `config.get` reports, which starts at 0 and moves by one per
/// successful swap: it is not the state revision, which moves on every event
/// from every source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigChanged {
    /// Accounts the swap started a runtime for.
    pub added: Vec<String>,
    /// Accounts whose effective configuration changed.
    pub updated: Vec<String>,
    /// Accounts the swap stopped.
    pub removed: Vec<String>,
    /// The configuration revision the swap moved to.
    pub config_revision: u64,
}

/// The `kind` a watched draft that parsed travels as.
pub const KIND_DRAFT_CHANGED: &str = "draft.changed";

/// The `kind` a watched draft that would not parse travels as.
pub const KIND_DRAFT_INVALID: &str = "draft.invalid";

/// The `kind` a watched signature file travels as.
pub const KIND_SIGNATURE_CHANGED: &str = "signature.changed";

/// One thing wrong with a file, positioned when the parser gave a position.
///
/// `line` is 1-based and relative to the file rather than to the block the
/// parser read, and `null` for a refusal by value rather than by syntax:
/// inventing a position would point the user at an innocent line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The offending line, `null` when the refusal has no position.
    pub line: Option<u32>,
    /// One line a status bar can show.
    pub message: String,
}

/// One draft the daemon parsed, as `draft.changed` carries it (P3b-U10).
///
/// `valid` is "the file parsed", so it is `true` on every one of these;
/// `ready` is `draft::validate_draft`, the second axis, because a draft with no
/// subject parses perfectly and is not sendable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftChanged {
    /// The account whose drafts directory holds the file.
    pub account: String,
    /// The `id:` field, or the file stem when there is none.
    pub id: String,
    /// The absolute path, which is what a GUI opens.
    pub path: String,
    /// The `to:` field, `null` for a draft with no recipient yet.
    pub to: Option<String>,
    /// The `subject:` field, empty when there is none.
    pub subject: String,
    /// The word the file spells: `draft`, `approved` or `sent`.
    pub status: String,
    /// Whether the file parsed, which is always `true` here.
    pub valid: bool,
    /// Whether the draft would send.
    pub ready: bool,
}

/// One draft the daemon could not parse, as `draft.invalid` carries it and as
/// the `-32010` payload spells it.
///
/// It names its account, because two accounts may hold a draft with the same
/// id and the event keys the same `draft:<account>/<id>` resource
/// `draft.changed` does: fixing the file replaces the row rather than adding a
/// second one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftInvalid {
    /// The account whose drafts directory holds the file.
    pub account: String,
    /// The file stem, since the `id:` field is inside what would not parse.
    pub id: String,
    /// The absolute path, which is what the user opens to fix it.
    pub path: String,
    /// Why it was refused, never empty.
    pub diagnostics: Vec<Diagnostic>,
}

/// One signature file that was written, as `signature.changed` carries it.
///
/// A lifecycle event: signatures are global rather than per-account, so no
/// snapshot section carries them and this is a "re-read the list" for a client
/// that caches signature bodies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureChanged {
    /// The file stem, which is the key `signatures::read` takes.
    pub name: String,
    /// The absolute path of the file.
    pub path: String,
}

/// Why a configuration candidate was refused, as the `config.invalid` event
/// carries it and as the `-32007` payload spells it.
///
/// One shape for both, so a client renders a diagnostic the same way whether it
/// asked for the reload or merely watched one. `line` is the 1-based line of
/// the offending token and `null` for a diagnostic with no position: a
/// semantic refusal has no span to point at, and inventing one would send a
/// user to an innocent line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigInvalid {
    /// The file the daemon read, or would have read.
    pub path: String,
    /// The 1-based line of the offending token, `null` when there is none.
    pub line: Option<u32>,
    /// One line a user can act on.
    pub message: String,
}
