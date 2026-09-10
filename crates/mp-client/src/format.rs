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
