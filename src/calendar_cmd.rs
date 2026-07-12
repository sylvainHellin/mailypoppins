//! CLI handlers for `mp calendar …` (organizer-side iMIP reconciliation).

use crate::config::{account_dir, AccountConfig, GlobalConfig};
use crate::reconcile::reconcile_account;
use anyhow::{anyhow, Result};
use colored::*;

/// `mp calendar rebuild [--account NAME]`: recompute every locally-stored
/// invite's attendee statuses from the REPLY emails in the mailstore. Safe to
/// run repeatedly (idempotent); rebuilds derived state from IMAP-visible
/// messages alone (design §3, multi-machine consistency).
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
        let root = account_dir(&account.name);
        println!(
            "{} Reconciling calendar replies for {} …",
            "ℹ".blue(),
            account.name.yellow()
        );
        let stats = reconcile_account(&root)?;
        println!(
            "{} {} attendee status(es) updated ({} already current) across {} invite(s) / {} reply(ies)",
            "✓".green(),
            stats.updated.to_string().bold(),
            stats.unchanged,
            stats.invites_seen,
            stats.replies_seen,
        );
    }
    Ok(())
}
