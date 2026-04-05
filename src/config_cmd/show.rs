use anyhow::Result;
use colored::*;

use crate::config::{
    all_configured_mailboxes, config_path, get_keyring_password, load_global_config,
    resolve_mailbox_local_path,
};

/// Print the config file path.
pub fn cmd_config_path() {
    println!("{}", config_path().display());
}

/// Display the resolved config with masked passwords.
pub fn cmd_config_show() -> Result<()> {
    let config = load_global_config()?;

    println!("{}", "=== Email CLI Configuration ===".bold().cyan());
    println!("{}: {}", "Config file".bold(), config_path().display());

    println!("\n{}", "[email]".bold());
    println!("  font_family       = {}", config.email.font_family);
    println!("  font_size         = {}", config.email.font_size);
    println!("  include_signature = {}", config.email.include_signature);

    if config.accounts.is_empty() {
        println!("\n{}", "(no accounts configured)".yellow());
        return Ok(());
    }

    for account in &config.accounts {
        println!("\n{}", format!("[[accounts]] name = \"{}\"", account.name).bold().cyan());
        println!("  default_from = {}", account.default_from);

        println!("\n  {}", "[smtp]".bold());
        println!("    host     = {}", account.smtp.host);
        println!("    port     = {}", account.smtp.port);
        println!("    username = {}", account.smtp.username);
        let smtp_key = format!("smtp-password-{}", account.name);
        let smtp_pw = if get_keyring_password(&smtp_key).is_ok() {
            "****".green().to_string()
        } else {
            "(not set)".red().to_string()
        };
        println!("    password = {} (keyring)", smtp_pw);
        if account.smtp.accept_invalid_certs {
            println!("    accept_invalid_certs = {}", "true".yellow());
        }

        println!("\n  {}", "[imap]".bold());
        println!(
            "    host     = {}",
            if account.imap.host.is_empty() {
                format!("{} (falls back to smtp.host)", account.smtp.host)
                    .dimmed()
                    .to_string()
            } else {
                account.imap.host.clone()
            }
        );
        println!("    port     = {}", account.imap.port);
        println!(
            "    username = {}",
            if account.imap.username.is_empty() {
                format!("{} (falls back to smtp.username)", account.smtp.username)
                    .dimmed()
                    .to_string()
            } else {
                account.imap.username.clone()
            }
        );
        let imap_key = format!("imap-password-{}", account.name);
        let imap_pw = if get_keyring_password(&imap_key).is_ok() {
            "****".green().to_string()
        } else if get_keyring_password(&smtp_key).is_ok() {
            format!("{} (falls back to smtp)", "****".green())
        } else {
            "(not set)".red().to_string()
        };
        println!("    password = {} (keyring)", imap_pw);
        if account.imap.accept_invalid_certs {
            println!("    accept_invalid_certs = {}", "true".yellow());
        }

        println!("\n  {}", "[directories]".bold());
        println!(
            "    root   = {}",
            account.directories.root.as_deref().unwrap_or("(not set)")
        );
        println!(
            "    drafts = {}",
            account.directories.drafts.as_deref().unwrap_or("(not set)")
        );

        println!("\n  {}", "[mailboxes]".bold());
        let all = all_configured_mailboxes(account);
        if all.is_empty() {
            println!("    (none configured)");
        } else {
            for (role, mapping) in &all {
                let resolved = resolve_mailbox_local_path(account, mapping);
                println!(
                    "    {} {} -> {}",
                    format!("[{}]", role).bold(),
                    mapping.server,
                    resolved.display()
                );
            }
        }

        println!("\n  {}", "[signatures]".bold());
        println!(
            "    default = {}",
            account.signatures.default.as_deref().unwrap_or("(none)")
        );
        for (key, entry) in &account.signatures.entries {
            println!("    [signatures.{}]", key);
            if let Some(ref name) = entry.name {
                println!("      name = {}", name);
            }
            println!("      path = {}", entry.path);
        }
    }

    Ok(())
}
