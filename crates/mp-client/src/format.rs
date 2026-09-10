//! Pure presentation over a typed event payload (P3b-U6).
//!
//! The daemon publishes data; a client decides how to say it. These functions
//! are that decision for the one payload two surfaces already render today, and
//! they take the payload and nothing else: no `App`, no config, no colour. A
//! glyph's colour is a terminal's business and a GUI has neither, so the
//! callers keep their own colouring around these strings.
//!
//! Every literal below is lifted from the current tree, so the daemon-era line
//! is the line a user reads now: [`sync_status_line`] is
//! `src/tui/helpers.rs::finish_sync` clause for clause, with the suffix
//! `drain_pending_ops` appends, and [`sync_cli_lines`] is the `println!`
//! sequence of `src/main.rs`'s `sync_one_account`, glyph included. Two lines
//! have no CLI original because today's `mp sync` cannot reach the state - it
//! passes no body-fetch deadline and it reports a mutation drain with a
//! `completed` count the payload does not carry - and they take the TUI's
//! wording at the glyph their severity contribution earns.

use mp_protocol::events::SyncCompleted;

/// What a drain report line carries when it came from the tail of a tick.
///
/// `mp sync` drains at both ends (#0114) and both ends report in the same
/// words, so without this the four lines of a tick that had work queued at both
/// ends would be two indistinguishable pairs. The client derives it from the
/// phase the daemon reported
/// (`mailypoppins::daemon::runtime::account::Phase::as_str`) and from nothing
/// else.
pub const TAIL_LABEL: &str = " (after sync)";

/// The prefix every line of a `--dry-run` pass carries, after its glyph.
const DRY_RUN: &str = "[dry-run] ";

/// `mp sync`'s report of one outbox drain: what went out, and what is still
/// owed.
///
/// `still_pending` is the rows this drain left behind, whatever the reason;
/// splitting "open" from "awaiting submission" is the outbox's own vocabulary
/// and not a distinction a sync summary makes.
pub fn outbox_drain_line(tail: bool, completed: u64, still_pending: u64) -> String {
    format!(
        "  \u{21bb} outbox{}: {completed} completed, {still_pending} still pending",
        label(tail)
    )
}

/// `mp sync`'s report of one mutation-queue drain.
pub fn mutations_drain_line(tail: bool, completed: u64, failed: u64) -> String {
    format!(
        "  \u{21bb} mutations{}: {completed} completed, {failed} failed",
        label(tail)
    )
}

/// A drain that could not run at all.
///
/// A warning and not a failure: the queue is retried on the next tick, so a
/// queue that could not be drained must not turn a sync that worked into a
/// failed command. It carries no [`TAIL_LABEL`] because only the head drain
/// reports this way; by the tail the outcome is already decided.
pub fn drain_failed_line(error: &str) -> String {
    format!("  \u{26a0} mutations: drain failed: {error}")
}

/// [`TAIL_LABEL`] for a tail phase, nothing for the head's.
fn label(tail: bool) -> &'static str {
    if tail {
        TAIL_LABEL
    } else {
        ""
    }
}

/// The TUI's one-line status message for a finished tick.
///
/// A failed tick reports the failure and nothing else: counts from a pass that
/// did not finish would read as a sync that happened. The account is always
/// named, because a pure function cannot count a client's accounts.
pub fn sync_status_line(outcome: &SyncCompleted) -> String {
    if let Some(error) = &outcome.error {
        return format!("Fetch failed ({}): {error}", outcome.account);
    }

    let mut line = format!(
        "Synced: {} new, {} existing",
        outcome.saved, outcome.skipped
    );
    if outcome.flags_updated > 0 {
        line.push_str(&format!(", {} status updated", outcome.flags_updated));
    }
    if outcome.uid_rebound > 0 {
        line.push_str(&format!(", {} renumbered", outcome.uid_rebound));
    }
    if outcome.pruned > 0 {
        line.push_str(&format!(", {} no longer in this mailbox", outcome.pruned));
    }
    if outcome.prunes_deferred > 0 {
        line.push_str(&format!(
            ", {} removal(s) held back (incomplete pass, run a full sync)",
            outcome.prunes_deferred
        ));
    }
    if outcome.bodies_truncated > 0 {
        line.push_str(&format!(
            ", {} mailbox(es) stopped at the fetch deadline (resuming next sync)",
            outcome.bodies_truncated
        ));
    }
    if !outcome.non_converging.is_empty() {
        line.push_str(&format!(
            ", fetch not converging on {} (see the log)",
            outcome.non_converging.join(", ")
        ));
    }
    if outcome.failed_mutations > 0 {
        line.push_str(&format!(
            "; {} mutation(s) failed and were rolled back (see the log)",
            outcome.failed_mutations
        ));
    }
    line
}

/// `mp sync`'s lines for a finished tick, one per state it reached, in the
/// order `sync_one_account` prints them and uncoloured.
pub fn sync_cli_lines(outcome: &SyncCompleted) -> Vec<String> {
    cli_lines(outcome, false)
}

/// The same lines for a `--dry-run` pass: every one of them prefixed after its
/// glyph, and the ingest count phrased as what *would* be downloaded.
pub fn sync_cli_lines_dry_run(outcome: &SyncCompleted) -> Vec<String> {
    cli_lines(outcome, true)
}

fn cli_lines(outcome: &SyncCompleted, dry_run: bool) -> Vec<String> {
    let at = if dry_run { DRY_RUN } else { "" };
    if let Some(error) = &outcome.error {
        return vec![format!("✗ {}: {error}", outcome.account)];
    }

    let mut lines = Vec::new();
    if outcome.skipped > 0 {
        lines.push(format!(
            "✓ {at}Synced: {} new, {} already present",
            outcome.saved, outcome.skipped
        ));
    } else {
        lines.push(format!(
            "✓ {at}Synced: {} email(s) {}",
            outcome.saved,
            if dry_run { "to download" } else { "ingested" }
        ));
    }
    if outcome.flags_updated > 0 {
        lines.push(format!(
            "ℹ {at}Status updated on {} message(s)",
            outcome.flags_updated
        ));
    }
    if outcome.uid_rebound > 0 {
        lines.push(format!(
            "ℹ {at}Rebound {} message(s) to new UIDs after a UIDVALIDITY reset",
            outcome.uid_rebound
        ));
    }
    if outcome.pruned > 0 {
        lines.push(format!(
            "ℹ {at}{} message(s) left their mailbox on the server",
            outcome.pruned
        ));
    }
    if outcome.prunes_deferred > 0 {
        lines.push(format!(
            "⚠ {at}{} removal(s) held back: this pass did not see every message, \
             run a full sync to apply them",
            outcome.prunes_deferred
        ));
    }
    // No CLI original: `mp sync` is the explicit recovery path and carries no
    // body-fetch deadline, so it never sees this. The TUI's wording at `ℹ`,
    // because a deadline stop is progress (#0113).
    if outcome.bodies_truncated > 0 {
        lines.push(format!(
            "ℹ {at}{} mailbox(es) stopped at the fetch deadline (resuming next sync)",
            outcome.bodies_truncated
        ));
    }
    for name in &outcome.non_converging {
        lines.push(format!(
            "⚠ {at}'{name}' downloaded the same messages again: the fetch is not converging, \
             see the log and docs/tickets/0115-warn-on-a-non-converging-fetch.md"
        ));
    }
    // No CLI original either: today's `mp sync` prints a `completed`/`failed`
    // pair the payload cannot rebuild. The TUI's wording at `⚠`, which is the
    // severity a rolled-back mutation earns (#0039).
    if outcome.failed_mutations > 0 {
        lines.push(format!(
            "⚠ {at}{} mutation(s) failed and were rolled back (see the log)",
            outcome.failed_mutations
        ));
    }
    lines
}

// ---------------------------------------------------------------------------
// The send slice (P4-U12)
// ---------------------------------------------------------------------------

use mp_protocol::send::{
    ApprovedOutcome, OutboxListing, OutboxRetryOutcome, OutboxRow, SendOutcome,
};

/// What either confirmation prompt prints when the answer is not yes.
///
/// One constant because two commands print it and the parity rows compare it
/// literally; a second copy in the CLI would be a second thing to get wrong.
pub const CANCELLED_LINE: &str = "Cancelled.";

/// Every line `mp outbox list` prints for one account, in order, glyphs
/// included and colourless.
///
/// Three shapes, and the client picks between them from the payload rather than
/// from a store: an account that never queued anything, an account whose outbox
/// is clear, and the rows with their annotations and the counts line under them.
pub fn outbox_cli_lines(listing: &OutboxListing) -> Vec<String> {
    if !listing.ever_used {
        return vec![format!(
            "  \u{b7} nothing has been queued for {} yet",
            listing.account
        )];
    }
    if listing.rows.is_empty() {
        return vec![format!(
            "  \u{2713} the outbox for {} is clear",
            listing.account
        )];
    }
    let mut lines = Vec::new();
    for row in &listing.rows {
        lines.extend(outbox_row_lines(row));
    }
    lines.push(format!(
        "  \u{21bb} {} working, {} failed, {} partly delivered",
        listing.counts.open, listing.counts.failed, listing.counts.partial
    ));
    lines
}

/// One row and everything indented under it.
fn outbox_row_lines(row: &OutboxRow) -> Vec<String> {
    // The state column is padded to a fixed width, so a listing reads as a
    // table; the caller may colour the word inside it, which is what costs the
    // padding on a terminal, exactly as it did before the daemon.
    let state = if row.partial { "partial" } else { &row.state };
    let mut lines = vec![format!(
        "  {:>4}  {:<20} {}  {}",
        row.id,
        state,
        local_minute(row.updated),
        row.message_id
    )];
    if let Some(target) = row.target_mailbox.as_deref() {
        lines.push(format!("        sent copy -> {target}"));
    }
    if row.never_submitted {
        lines.push("        never submitted; the next sync sends it".to_string());
    }
    for (address, reason) in &row.rejected {
        lines.push(format!("        never delivered to: {address} ({reason})"));
    }
    // A `done` row owes nobody anything more, whatever its envelope still lists:
    // the submission is over and the note above says who missed out.
    if !row.outstanding.is_empty() && row.state != "done" {
        lines.push(format!(
            "        still to deliver to: {}",
            row.outstanding.join(", ")
        ));
    }
    if let Some(error) = row.last_error.as_deref() {
        let label = if row.partial {
            "outcome:"
        } else {
            "last error:"
        };
        lines.push(format!("        {label} {error}"));
    }
    lines
}

/// A unix timestamp as local `YYYY-MM-DD HH:MM`, or `-` when it is unset.
///
/// Local, because the operator reading the listing is in one place and the row
/// is about something that happened to them.
fn local_minute(ts: i64) -> String {
    if ts <= 0 {
        return "-".to_string();
    }
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "-".to_string())
}

/// The line `mp outbox discard` prints: the row and the message, because an
/// operator who just dropped a message is owed both.
pub fn outbox_discard_line(row_id: i64, message_id: &str) -> String {
    format!("  \u{2713} discarded row {row_id} ({message_id}); its bytes are released")
}

/// The two lines a retry prints, and the third it prints only when a copy was
/// filed.
pub fn outbox_retry_lines(outcome: &OutboxRetryOutcome) -> Vec<String> {
    let mut lines = vec![format!(
        "  \u{21bb} row {} is queued again; sending it now",
        outcome.row_id
    )];
    lines.push(match outcome.state.as_deref() {
        Some(state) => format!("  \u{2713} row {} is now {state}", outcome.row_id),
        None => format!("  \u{2713} row {} is gone", outcome.row_id),
    });
    if outcome.completed > 0 {
        lines.push(format!(
            "  \u{2713} {} sent copy/copies filed",
            outcome.completed
        ));
    }
    lines
}

/// Every line `mp send` prints about an outcome, in order.
///
/// A send nobody received produces the recipient lines and nothing else: the
/// sentence that names it a failure is the command's exit code talking, and
/// that stays the client's decision.
pub fn send_cli_lines(outcome: &SendOutcome) -> Vec<String> {
    let mut lines: Vec<String> = outcome.recipients.iter().map(recipient_line).collect();
    let delivered = outcome.recipients.iter().filter(|r| r.delivered).count();
    if delivered == 0 {
        return lines;
    }
    if let Some(error) = outcome.settle_error.as_deref() {
        lines.push(format!(
            "\u{26a0} (sent but failed to retire draft: {error})"
        ));
    }
    let failed = outcome.recipients.len() - delivered;
    if failed == 0 {
        lines.push(format!(
            "\u{2713} Email sent successfully to all {} recipient(s) [{}]",
            outcome.recipients.len(),
            outcome.status_line
        ));
    } else {
        lines.push(format!(
            "\u{26a0} Partial send: {delivered} succeeded, {failed} failed [{}] \
             (marked as sent -- see logs for details)",
            outcome.status_line
        ));
    }
    lines
}

/// One recipient's own line.
fn recipient_line(recipient: &mp_protocol::send::RecipientOutcome) -> String {
    if recipient.delivered {
        format!("  \u{2713} {} ({})", recipient.address, recipient.role)
    } else {
        format!(
            "  \u{2717} {} ({}): {}",
            recipient.address,
            recipient.role,
            recipient.error.as_deref().unwrap_or("unknown error")
        )
    }
}

/// The one line `mp send-approved` prints per draft, after the
/// `Sending to …` prefix it is appended to.
///
/// A send that never reached the transport at all carries no recipients, and
/// then the line is the reason it did not.
pub fn send_approved_line(outcome: &SendOutcome) -> String {
    if outcome.recipients.is_empty() {
        return format!("\u{2717} {}", outcome.status_line);
    }
    let delivered = outcome.recipients.iter().filter(|r| r.delivered).count();
    if delivered == 0 {
        return match outcome.recipients.iter().find_map(|r| r.error.as_deref()) {
            Some(reason) => format!(
                "\u{2717} all recipients failed: {reason} [{}]",
                outcome.status_line
            ),
            None => format!("\u{2717} all recipients failed [{}]", outcome.status_line),
        };
    }
    if let Some(error) = outcome.settle_error.as_deref() {
        return format!("\u{26a0} (sent but failed to update status: {error})");
    }
    if delivered == outcome.recipients.len() {
        format!("\u{2713} [{}]", outcome.status_line)
    } else {
        format!(
            "\u{26a0} (partial: {delivered}/{} recipients) [{}]",
            outcome.recipients.len(),
            outcome.status_line
        )
    }
}

/// The summary line the batch ends each account with.
pub fn send_approved_summary(outcome: &ApprovedOutcome) -> String {
    format!(
        "Summary {}: {} sent, {} failed",
        outcome.account, outcome.sent, outcome.failed
    )
}
