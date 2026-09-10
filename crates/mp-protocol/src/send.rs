//! The results of the `send.*` family (P4-U12).
//!
//! Wire shapes, so they live in the crate a client links rather than in the
//! daemon crate a client must never link, beside [`crate::draft`]. Every one of
//! them is `Serialize + Deserialize`, which is what lets the CLI render from the
//! typed value the daemon sent instead of from a JSON object that happens to
//! look like it.
//!
//! **Nothing here carries a path.** A Sent copy lives on a server and a queued
//! message lives in a blob the client has no business opening, so a send reports
//! who has the message, where the copy went and what the row is now, and never
//! where any of it is on disk. The one file name `mp send-approved` prints comes
//! from `draft.list`, which is the family whose subject *is* the drafts
//! directory.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// A send
// ---------------------------------------------------------------------------

/// One recipient's own verdict.
///
/// SMTP is one conversation per recipient here, so a submission can end with
/// some recipients holding the message and others not (`SND-08`); this is that
/// split, per address, in the order the message was built with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipientOutcome {
    /// The address as the submission attempted it.
    pub address: String,
    /// `To`, `Cc` or `Bcc`, which is what the line prints in brackets.
    pub role: String,
    /// Whether that address's own conversation ended in a 250.
    pub delivered: bool,
    /// The reason it did not, absent when it did.
    pub error: Option<String>,
}

/// Where the local copy of a sent message ended up.
///
/// Not a path and not a UID: the copy is a message in a server mailbox, and the
/// only thing a client renders about it is whether it is there yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SentCopy {
    /// The account files no local copy: the server does it (Gmail, Graph,
    /// Proton) or the user said `save_to_sent = "never"`.
    NotRequested,
    /// SMTP is done and the APPEND is still outstanding.
    Pending,
    /// The copy is in the Sent mailbox (`SND-09`).
    Filed,
}

/// What one send did, end to end: `send.draft`, `send.invite`, and one element
/// of a `send.approved` batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendOutcome {
    /// The account the message went out from.
    pub account: String,
    /// The draft it was built from, absent for an invitation.
    pub selector: Option<String>,
    /// The `Message-ID` the build minted, which is what an operator matches an
    /// outbox row by.
    pub message_id: String,
    /// The one honest line about where the message actually is, which the CLI
    /// prints in brackets (`SendReport::status_line`).
    pub status_line: String,
    /// Every recipient, in build order.
    pub recipients: Vec<RecipientOutcome>,
    /// Where the local copy went.
    pub sent_copy: SentCopy,
    /// The bookkeeping error a submission that *did* go out nevertheless hit
    /// while retiring the draft file. Never a failed send: the message is on the
    /// server either way.
    pub settle_error: Option<String>,
}

/// What one `send.approved` batch did, in the order it walked the drafts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedOutcome {
    /// The account whose approved drafts were sent.
    pub account: String,
    /// One outcome per draft attempted, in listing order.
    pub results: Vec<SendOutcome>,
    /// How many of them reached at least one recipient.
    pub sent: usize,
    /// How many reached none.
    pub failed: usize,
}

// ---------------------------------------------------------------------------
// The outbox
// ---------------------------------------------------------------------------

/// One row of `send.outbox_list`, with every field the listing renders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRow {
    /// The row id, which is what `mp outbox retry|discard` names.
    pub id: i64,
    /// The row's own state, spelled as the store holds it.
    pub state: String,
    /// Whether it is a `done` row that kept a note, which is a message some
    /// recipient never got (`SND-08`) rather than a finished one.
    pub partial: bool,
    /// Whether the transport was provably never entered for it, which is what
    /// the "the next sync sends it" note is about.
    pub never_submitted: bool,
    /// The `Message-ID` the row carries.
    pub message_id: String,
    /// The Sent mailbox the copy is owed to, absent when no copy is filed.
    pub target_mailbox: Option<String>,
    /// When the row last moved, as a unix timestamp; the client renders it in
    /// local time.
    pub updated: i64,
    /// The last error, or the partial-delivery note for a `partial` row.
    pub last_error: Option<String>,
    /// The recipients the server refused for good, with the reason it gave.
    pub rejected: Vec<(String, String)>,
    /// The recipients that have neither taken the message nor been refused it.
    pub outstanding: Vec<String>,
}

/// The three counts under the listing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxCounts {
    /// Rows still working: queued, or waiting for their Sent copy.
    pub open: usize,
    /// Rows parked for a human.
    pub failed: usize,
    /// Rows that went out to some recipients and not others.
    pub partial: usize,
}

/// The `result` of `send.outbox_list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxListing {
    /// The account that was listed.
    pub account: String,
    /// Whether this account has ever had an outbox at all. `false` is a fact
    /// rather than an error: the client prints "nothing has been queued …" and
    /// exits 0 for it.
    pub ever_used: bool,
    /// The unfinished rows, in id order, which the client prints without
    /// re-sorting.
    pub rows: Vec<OutboxRow>,
    /// The counts line under them.
    pub counts: OutboxCounts,
}

/// The `result` of a settled `send.outbox_retry`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRetryOutcome {
    /// The row that was retried.
    pub row_id: i64,
    /// What became of it, `None` when the retry finished it and the row is
    /// gone.
    pub state: Option<String>,
    /// How many Sent copies the drain filed on the way.
    pub completed: usize,
}
