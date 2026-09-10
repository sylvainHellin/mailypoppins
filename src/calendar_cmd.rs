//! CLI handlers for `mp calendar …` (organizer-side iMIP reconciliation).

use crate::config::{store_path, AccountConfig, GlobalConfig};
use crate::reconcile::reconcile_account;
use crate::store::{BlobStore, Store};
use anyhow::{anyhow, Result};
use colored::*;

/// `mp calendar rebuild [--account NAME]`: report what the stored REPLY
/// messages resolve on the stored invitations.
///
/// It writes nothing. Attendee statuses are derived where they are displayed,
/// from the `invite.ics` blobs of the account's rows (#0038 scope item 6), so
/// there is no cached copy left to rebuild and the command exists to show what
/// the fold sees. Safe to run repeatedly by construction.
// Unused from P4-U14, when `mp calendar rebuild` started answering from
// `calendar.rebuild`. Deleted with the rest of the direct engine paths by
// P4-U15; the rendering below is shared with the routed client.
#[allow(dead_code)]
pub fn handle_rebuild(config: &GlobalConfig, account_name: Option<String>) -> Result<()> {
    let accounts: Vec<&AccountConfig> = match account_name {
        Some(name) => vec![config
            .accounts
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(&name))
            .ok_or_else(|| anyhow!("no account named '{}'", name))?],
        None => {
            if config.accounts.is_empty() {
                return Err(anyhow!("no accounts configured"));
            }
            config.accounts.iter().collect()
        }
    };

    for account in accounts {
        print_header(&account.name);
        let path = store_path(&account.name);
        if !path.exists() {
            print_no_store(&account.name);
            continue;
        }
        let store = Store::open(&path)?;
        let blobs = BlobStore::for_account(&account.name);
        let report = reconcile_account(&store, &blobs, &account.name);
        print_report(
            report.resolved,
            report.invites_seen,
            report.replies_seen,
            report.cancelled,
        );
    }
    Ok(())
}

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
