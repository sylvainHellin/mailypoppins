//! What `mp calendar …` prints (organizer-side iMIP reconciliation).
//!
//! The fold itself is `calendar.rebuild`'s since P4-U14 and its direct handler
//! went with the rest of the direct engine paths in P4-U15; what is left is the
//! four literals, printed once here from whatever the daemon reports.

use colored::*;

/// The line printed before one account's fold.
pub fn print_header(account: &str) {
    println!(
        "{} Reconciling calendar replies for {} …",
        "ℹ".blue(),
        account.yellow()
    );
}

/// What an account with no store gets: a note, and the walk carries on.
pub fn print_no_store(account: &str) {
    println!("{} no store yet for {}", "•".blue(), account);
}

/// What the fold resolved, and the cancellations it saw.
pub fn print_report(resolved: usize, invites_seen: usize, replies_seen: usize, cancelled: usize) {
    {
        let report = ReportCounts {
            resolved,
            invites_seen,
            replies_seen,
            cancelled,
        };
        println!(
            "{} {} attendee status(es) resolved across {} invite(s) / {} reply(ies)",
            "✓".green(),
            report.resolved.to_string().bold(),
            report.invites_seen,
            report.replies_seen,
        );
        if report.cancelled > 0 {
            println!(
                "{} {} invite(s) cancelled by the organizer (kept, marked cancelled)",
                "•".blue(),
                report.cancelled.to_string().bold(),
            );
        }
    }
}

/// The four counts one fold reports, named so the block above reads as it did.
struct ReportCounts {
    resolved: usize,
    invites_seen: usize,
    replies_seen: usize,
    cancelled: usize,
}
