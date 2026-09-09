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
    if let Some(error) = &outcome.error {
        return vec![format!("✗ {}: {error}", outcome.account)];
    }

    let mut lines = Vec::new();
    if outcome.skipped > 0 {
        lines.push(format!(
            "✓ Synced: {} new, {} already present",
            outcome.saved, outcome.skipped
        ));
    } else {
        lines.push(format!("✓ Synced: {} email(s) ingested", outcome.saved));
    }
    if outcome.flags_updated > 0 {
        lines.push(format!(
            "ℹ Status updated on {} message(s)",
            outcome.flags_updated
        ));
    }
    if outcome.uid_rebound > 0 {
        lines.push(format!(
            "ℹ Rebound {} message(s) to new UIDs after a UIDVALIDITY reset",
            outcome.uid_rebound
        ));
    }
    if outcome.pruned > 0 {
        lines.push(format!(
            "ℹ {} message(s) left their mailbox on the server",
            outcome.pruned
        ));
    }
    if outcome.prunes_deferred > 0 {
        lines.push(format!(
            "⚠ {} removal(s) held back: this pass did not see every message, \
             run a full sync to apply them",
            outcome.prunes_deferred
        ));
    }
    // No CLI original: `mp sync` is the explicit recovery path and carries no
    // body-fetch deadline, so it never sees this. The TUI's wording at `ℹ`,
    // because a deadline stop is progress (#0113).
    if outcome.bodies_truncated > 0 {
        lines.push(format!(
            "ℹ {} mailbox(es) stopped at the fetch deadline (resuming next sync)",
            outcome.bodies_truncated
        ));
    }
    for name in &outcome.non_converging {
        lines.push(format!(
            "⚠ '{name}' downloaded the same messages again: the fetch is not converging, \
             see the log and docs/tickets/0115-warn-on-a-non-converging-fetch.md"
        ));
    }
    // No CLI original either: today's `mp sync` prints a `completed`/`failed`
    // pair the payload cannot rebuild. The TUI's wording at `⚠`, which is the
    // severity a rolled-back mutation earns (#0039).
    if outcome.failed_mutations > 0 {
        lines.push(format!(
            "⚠ {} mutation(s) failed and were rolled back (see the log)",
            outcome.failed_mutations
        ));
    }
    lines
}
