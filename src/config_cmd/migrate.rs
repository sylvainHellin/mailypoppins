use anyhow::{Context, Result};
use colored::*;
use std::fs;

use crate::config::config_path;

/// Migrate old single-account config to new [[accounts]] format.
pub fn cmd_config_migrate() -> Result<()> {
    let path = config_path();
    if !path.exists() {
        return Err(anyhow::anyhow!(
            "Config file not found at {}. Nothing to migrate.",
            path.display()
        ));
    }

    let content = fs::read_to_string(&path)?;

    // Check if already migrated (has [[accounts]])
    if content.contains("[[accounts]]") {
        println!("{} Config already uses [[accounts]] format. Nothing to do.", "\u{2713}".green());
        return Ok(());
    }

    // Parse as old format using a temporary struct
    #[derive(serde::Deserialize, Default)]
    struct OldConfig {
        #[serde(default)]
        email: crate::config::EmailSettings,
        #[serde(default)]
        smtp: OldSmtp,
        #[serde(default)]
        imap: crate::config::ImapSettings,
        #[serde(default)]
        directories: crate::config::DirectorySettings,
        #[serde(default)]
        mailboxes: crate::config::MailboxesConfig,
        #[serde(default)]
        signatures: crate::config::SignaturesConfig,
    }
    #[derive(serde::Deserialize, Default)]
    struct OldSmtp {
        #[serde(default)]
        host: String,
        #[serde(default)]
        port: u16,
        #[serde(default)]
        username: String,
        #[serde(default)]
        default_from: String,
    }

    let old: OldConfig = toml::from_str(&content)
        .with_context(|| "Failed to parse old config format")?;

    // Derive account name from default_from domain
    let account_name = if !old.smtp.default_from.is_empty() {
        old.smtp.default_from
            .rsplit('@')
            .next()
            .unwrap_or("main")
            .split('.')
            .next()
            .unwrap_or("main")
            .to_string()
    } else {
        "main".to_string()
    };

    // Build new config
    let mut out = String::new();
    out.push_str("[email]\n");
    out.push_str(&format!("font_family = \"{}\"\n", old.email.font_family));
    out.push_str(&format!("font_size = \"{}\"\n", old.email.font_size));
    out.push_str(&format!("include_signature = {}\n", old.email.include_signature));

    out.push_str(&format!("\n[[accounts]]\nname = \"{}\"\n", account_name));
    out.push_str(&format!("default_from = \"{}\"\n", old.smtp.default_from));

    out.push_str("\n[accounts.smtp]\n");
    out.push_str(&format!("host = \"{}\"\n", old.smtp.host));
    out.push_str(&format!("port = {}\n", old.smtp.port));
    out.push_str(&format!("username = \"{}\"\n", old.smtp.username));

    out.push_str("\n[accounts.imap]\n");
    if !old.imap.host.is_empty() {
        out.push_str(&format!("host = \"{}\"\n", old.imap.host));
    }
    out.push_str(&format!("port = {}\n", old.imap.port));
    if !old.imap.username.is_empty() {
        out.push_str(&format!("username = \"{}\"\n", old.imap.username));
    }

    out.push_str("\n[accounts.directories]\n");
    if let Some(ref root) = old.directories.root {
        out.push_str(&format!("root = \"{}\"\n", root));
    }
    if let Some(ref drafts) = old.directories.drafts {
        out.push_str(&format!("drafts = \"{}\"\n", drafts));
    }

    if let Some(ref m) = old.mailboxes.inbox {
        out.push_str("\n[accounts.mailboxes.inbox]\n");
        out.push_str(&format!("server = \"{}\"\nlocal = \"{}\"\n", m.server, m.local));
    }
    if let Some(ref m) = old.mailboxes.archive {
        out.push_str("\n[accounts.mailboxes.archive]\n");
        out.push_str(&format!("server = \"{}\"\nlocal = \"{}\"\n", m.server, m.local));
    }
    if let Some(ref m) = old.mailboxes.sent {
        out.push_str("\n[accounts.mailboxes.sent]\n");
        out.push_str(&format!("server = \"{}\"\nlocal = \"{}\"\n", m.server, m.local));
    }
    if let Some(ref extras) = old.mailboxes.extra {
        for m in extras {
            out.push_str("\n[[accounts.mailboxes.extra]]\n");
            out.push_str(&format!("server = \"{}\"\nlocal = \"{}\"\n", m.server, m.local));
        }
    }

    if !old.signatures.entries.is_empty() || old.signatures.default.is_some() {
        out.push_str("\n[accounts.signatures]\n");
        if let Some(ref default) = old.signatures.default {
            out.push_str(&format!("default = \"{}\"\n", default));
        }
        for (key, entry) in &old.signatures.entries {
            out.push_str(&format!("\n[accounts.signatures.{}]\n", key));
            if let Some(ref name) = entry.name {
                out.push_str(&format!("name = \"{}\"\n", name));
            }
            out.push_str(&format!("path = \"{}\"\n", entry.path));
        }
    }

    // Migrate keyring
    let mut migrated_keys = Vec::new();
    if let Ok(pw) = crate::config::get_keyring_password("smtp-password") {
        let new_key = format!("smtp-password-{}", account_name);
        crate::config::set_keyring_password(&new_key, &pw)?;
        migrated_keys.push(("smtp-password".to_string(), new_key));
    }
    if let Ok(pw) = crate::config::get_keyring_password("imap-password") {
        let new_key = format!("imap-password-{}", account_name);
        crate::config::set_keyring_password(&new_key, &pw)?;
        migrated_keys.push(("imap-password".to_string(), new_key));
    }

    // Backup old config
    let backup = path.with_extension("toml.bak");
    fs::copy(&path, &backup)?;
    println!("{} Backed up old config to {}", "\u{2713}".green(), backup.display());

    // Write new config
    fs::write(&path, out)?;
    println!("{} Config migrated to [[accounts]] format", "\u{2713}".green());
    println!("  Account name: {}", account_name.bold());

    for (old_key, new_key) in &migrated_keys {
        println!("  Keyring: {} -> {}", old_key, new_key);
    }

    Ok(())
}
