//! CLI handlers for `mp contacts …`, and the rendering both they and the
//! routed client (P4-U14) print through.
//!
//! Since the admin slice the index itself is the daemon's: `contact.search`,
//! `contact.stats` and `contact.rebuild` answer with [`ContactRow`]s and the
//! verdict a rebuild reached, and every literal below is printed once, here,
//! whichever process did the reading.

use crate::config::{account_dir, AccountConfig, GlobalConfig};
use crate::contacts::{
    build_index_for_account, cache_path, load_cache, save_rebuilt_cache, search, CacheSave,
    Contact, ContactIndex,
};
use anyhow::{anyhow, Result};
use colored::*;
use std::path::{Path, PathBuf};

/// One ranked contact, as the listing renders it.
///
/// The field names of [`Contact`], because `contact.search` carries them under
/// those names and `mp contacts search --parsable` prints `address\tname`: a
/// renamed pair would make the tab-delimited line a translation rather than a
/// projection (`ANO-8`).
#[derive(Clone, Debug, Default)]
pub struct ContactRow {
    pub address: String,
    pub display_name: String,
    pub sent_to: u32,
    pub sent_cc: u32,
    pub received: u32,
}

impl From<&Contact> for ContactRow {
    fn from(contact: &Contact) -> ContactRow {
        ContactRow {
            address: contact.address.clone(),
            display_name: contact.display_name.clone(),
            sent_to: contact.sent_to,
            sent_cc: contact.sent_cc,
            received: contact.received,
        }
    }
}

pub fn handle_search(
    config: &GlobalConfig,
    query: Option<String>,
    parsable: bool,
    limit: usize,
    account_name: Option<String>,
) -> Result<()> {
    let account = pick_account(config, account_name.as_deref())?;
    let root = account_root(account)?;
    let index = load_or_build(account, &root)?;

    let q = query.unwrap_or_default();
    let results: Vec<ContactRow> = search(&index, &q, limit)
        .iter()
        .map(|m| ContactRow::from(m.contact))
        .collect();
    print_search(&results, &q, parsable);
    Ok(())
}

/// Every line `mp contacts search` prints, in order.
pub fn print_search(results: &[ContactRow], q: &str, parsable: bool) {
    if parsable {
        // Mutt/aerc/himalaya-vim compatibility format: first line is a header,
        // then `email\tname` lines. Mutt discards the first line by protocol.
        println!("{} results for '{}'", results.len(), q);
        for r in results {
            println!("{}\t{}", r.address, r.display_name);
        }
    } else if results.is_empty() {
        println!("{} No matches", "ℹ".blue());
    } else {
        for (i, r) in results.iter().enumerate() {
            let tier_mark = tier_indicator(r);
            let name = if r.display_name.is_empty() {
                "(no name)".dimmed()
            } else {
                r.display_name.as_str().yellow()
            };
            println!(
                "{:>3}. {} {} {}",
                i + 1,
                tier_mark,
                name,
                format!("<{}>", r.address).cyan(),
            );
        }
    }
}

/// The line a rebuild prints before it starts, which names the account so a
/// batch of five is legible while it runs.
pub fn print_rebuild_header(account: &str) {
    println!(
        "{} Rebuilding contacts for {} …",
        "ℹ".blue(),
        account.yellow()
    );
}

/// The verdict `save_rebuilt_cache` reached, in the words it has always been
/// reported in, including the two refusals that keep a good cache (#0067).
pub fn print_rebuild_outcome(saved: &CacheSave, count: usize, root: &Path) {
    {
        match saved {
            CacheSave::Written => println!(
                "{} {} contacts cached at {}",
                "✓".green(),
                count.to_string().bold(),
                root.display()
            ),
            CacheSave::RefusedEmpty { kept } => println!(
                "{} Rebuild found no contacts; kept the {} already cached at {}",
                "⚠".yellow(),
                kept.to_string().bold(),
                root.display()
            ),
            CacheSave::RefusedShrunk { kept, rebuilt } => println!(
                "{} Rebuild found only {} contacts against {} cached; kept the cache at {}",
                "⚠".yellow(),
                rebuilt.to_string().bold(),
                kept.to_string().bold(),
                root.display()
            ),
        }
    }
}

/// Every line `mp contacts stats` prints, in order.
#[allow(clippy::too_many_arguments)]
pub fn print_stats(
    account: &str,
    total: usize,
    sent_to_total: u64,
    sent_cc_total: u64,
    received_total: u64,
    cache: &str,
    built_at: &str,
    top: &[ContactRow],
) {
    println!("{} Contacts for {}", "ℹ".blue(), account.yellow());
    println!("  Total contacts:  {}", total.to_string().bold());
    println!("  Sent-to hits:    {}", sent_to_total);
    println!("  Sent-cc hits:    {}", sent_cc_total);
    println!("  Received hits:   {}", received_total);
    println!("  Cache path:      {}", cache);
    println!("  Built at:        {}", built_at);
    println!();
    println!("{}", "Top 10:".bold());
    for (i, r) in top.iter().enumerate() {
        let name = if r.display_name.is_empty() {
            "(no name)".to_string()
        } else {
            r.display_name.clone()
        };
        println!(
            "  {:>2}. {} <{}> (sent_to={}, sent_cc={}, received={})",
            i + 1,
            name,
            r.address,
            r.sent_to,
            r.sent_cc,
            r.received,
        );
    }
}

// --- helpers ---

fn pick_account<'a>(config: &'a GlobalConfig, name: Option<&str>) -> Result<&'a AccountConfig> {
    match name {
        Some(n) => config
            .accounts
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(n))
            .ok_or_else(|| anyhow!("no account named '{}'", n)),
        None => config
            .accounts
            .first()
            .ok_or_else(|| anyhow!("no accounts configured")),
    }
}

fn account_root(account: &AccountConfig) -> Result<PathBuf> {
    Ok(account_dir(&account.name))
}

fn load_or_build(account: &AccountConfig, root: &Path) -> Result<ContactIndex> {
    if let Some(idx) = load_cache(root)? {
        return Ok(idx);
    }
    // No cache yet — build on demand.
    let idx = build_index_for_account(account)?;
    match save_rebuilt_cache(root, &idx)? {
        CacheSave::Written => Ok(idx),
        // The guard kept something on disk, so the caller must be shown what
        // the disk holds and not the rebuild that was just refused (#0067).
        refused => {
            eprintln!(
                "{} Contacts rebuild refused ({refused:?}); showing the cache at {}",
                "⚠".yellow(),
                cache_path(root).display()
            );
            Ok(load_cache(root)?.unwrap_or(idx))
        }
    }
}

fn tier_indicator(c: &ContactRow) -> colored::ColoredString {
    // Nerd Font icons (NOT emojis): nf-md-account_check, nf-md-account, nf-md-email.
    if c.sent_to > 0 {
        "󰁞".green()
    } else if c.sent_cc > 0 {
        "".cyan()
    } else {
        "".blue()
    }
}
