use std::path::Path;

use anyhow::Result;
use colored::*;

use std::collections::HashMap;

use mp_protocol::account::{AccountCredential, AccountCredentials, TokenState};
use mp_protocol::signature::SignatureFile;

use crate::config::{
    all_configured_mailboxes, blobs_dir, config_path, drafts_dir, mailypoppins_data_dir,
    retention_for, store_path, AuthMethod, GlobalConfig,
};
use crate::store::sweep::human_bytes;

/// A horizon in days, with `0` spelled as the keep-all it means.
fn horizon_label(days: u32) -> String {
    if days == 0 {
        "0 (keep all)".to_string()
    } else {
        format!("{days}")
    }
}

/// The signatures `mp config show` lists, as the daemon answered for them:
/// every file with its content (`signature.read`) and each account's default
/// (`signature.list`).
#[derive(Clone, Debug, Default)]
pub struct Signatures {
    /// Every signature, in name order.
    pub files: Vec<SignatureFile>,
    /// Each account's default, `None` when it has none.
    pub defaults: HashMap<String, Option<String>>,
}

/// The daemon's answer about `account`, `None` when it named no such account.
fn credential_of<'a>(
    credentials: &'a AccountCredentials,
    account: &str,
) -> Option<&'a AccountCredential> {
    credentials.accounts.iter().find(|c| c.account == account)
}

/// The `[smtp]` password column: stored or not, never the value.
fn smtp_password_line(credential: Option<&AccountCredential>) -> String {
    match credential.and_then(|c| c.smtp_password) {
        Some(true) => "****".green().to_string(),
        _ => "(not set)".red().to_string(),
    }
}

/// The `[imap]` password column, which falls back to the SMTP password when
/// the account has no IMAP one of its own.
fn imap_password_line(credential: Option<&AccountCredential>) -> String {
    match credential.map(|c| (c.imap_password, c.smtp_password)) {
        Some((Some(true), _)) => "****".green().to_string(),
        Some((_, Some(true))) => format!("{} (falls back to smtp)", "****".green()),
        _ => "(not set)".red().to_string(),
    }
}

/// The `[oauth2]` token line.
fn token_line(credential: Option<&AccountCredential>) -> String {
    match credential.and_then(|c| c.token) {
        Some(TokenState::Valid) => "valid".green().to_string(),
        Some(TokenState::Expired) => format!("{} (run `mp config oauth2-login`)", "expired".red()),
        Some(TokenState::Invalid) => {
            format!("{} (corrupt or undecryptable cache file)", "invalid".red())
        }
        Some(TokenState::NotCached) | None => {
            format!("{} (run `mp config oauth2-login`)", "not cached".red())
        }
    }
}

/// Print the config file path.
pub fn cmd_config_path() {
    println!("{}", config_path().display());
}

/// Display the resolved config with masked passwords.
///
/// The configuration and the path it came from are the caller's, because since
/// P4-U14 they come off the wire: `mp config show` renders `config.get`'s
/// effective configuration, which is the loaded document after serde defaults
/// (`docs/daemon-protocol.md`), rather than re-reading the file the daemon owns.
/// Whether each password and token is there is `account.credentials`'s answer,
/// so this process reads no secret and no token cache of its own.
pub fn cmd_config_show(
    config: &GlobalConfig,
    path: &Path,
    credentials: &AccountCredentials,
    signatures: &Signatures,
) -> Result<()> {
    println!("{}", "=== mailypoppins configuration ===".bold().cyan());
    println!("{}: {}", "Config file".bold(), path.display());
    println!(
        "{}: {}",
        "Data dir".bold(),
        mailypoppins_data_dir().display()
    );

    let theme_name = if config.theme.is_empty() {
        format!("{} (default)", crate::tui::theme::DEFAULT_THEME_NAME)
    } else {
        config.theme.clone()
    };
    println!("{}: {}", "Theme".bold(), theme_name);
    println!("{}: {}", "Notifications".bold(), config.notifications);

    println!("\n{}", "[email]".bold());
    println!("  font_family       = {}", config.email.font_family);
    println!("  font_size         = {}", config.email.font_size);
    println!("  include_signature = {}", config.email.include_signature);

    if config.accounts.is_empty() {
        println!("\n{}", "(no accounts configured)".yellow());
        return Ok(());
    }

    for account in &config.accounts {
        println!(
            "\n{}",
            format!("[[accounts]] name = \"{}\"", account.name)
                .bold()
                .cyan()
        );
        println!("  default_from = {}", account.default_from);

        match account.auth_method {
            AuthMethod::OAuth2 | AuthMethod::Graph => {
                let label = if account.auth_method == AuthMethod::Graph {
                    "graph (Microsoft Graph API)"
                } else {
                    "oauth2"
                };
                println!("  auth_method  = {}", label.yellow());
                if let Some(ref oauth2) = account.oauth2 {
                    println!("\n  {}", "[oauth2]".bold());
                    println!("    client_id = {}", oauth2.client_id);
                    println!("    tenant_id = {}", oauth2.tenant_id);
                    println!(
                        "    token      = {}",
                        token_line(credential_of(credentials, &account.name))
                    );
                }
            }
            AuthMethod::Password => {
                println!("  auth_method  = password");
            }
        }

        if account.auth_method == AuthMethod::Graph {
            // Graph accounts don't use SMTP/IMAP -- show a note instead
            println!("\n  {}", "[transport]".bold());
            println!("    Uses Microsoft Graph API (no SMTP/IMAP)");
        } else {
            println!("\n  {}", "[smtp]".bold());
            println!("    host     = {}", account.smtp.host);
            println!("    port     = {}", account.smtp.port);
            println!("    username = {}", account.smtp.username);
            let credential = credential_of(credentials, &account.name);
            println!("    password = {}", smtp_password_line(credential));
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
            println!("    password = {}", imap_password_line(credential));
            if account.imap.accept_invalid_certs {
                println!("    accept_invalid_certs = {}", "true".yellow());
            }
        }

        println!("\n  {}", "[local paths]".bold());
        println!("    store  = {}", store_path(&account.name).display());
        println!("    blobs  = {}", blobs_dir(&account.name).display());
        println!("    drafts = {}", drafts_dir(&account.name).display());

        // Role to server folder, and nothing else: a mailbox has no local
        // directory since received mail moved into the store above. This block
        // used to print `<account_dir>/inbox` per mailbox, a path nothing has
        // created since the cutover (#0057).
        println!("\n  {}", "[mailboxes]".bold());
        let all = all_configured_mailboxes(account);
        if all.is_empty() {
            println!("    (none configured)");
        } else {
            for (role, mapping) in &all {
                println!("    {} -> {}", format!("[{}]", role).bold(), mapping.server);
            }
        }

        // Retention is enforced as of #0060: the sweep runs after every sync
        // and via `mp store gc`. This block used to (and must no longer) label
        // it unenforced.
        match retention_for(config, account) {
            Ok(policy) => {
                println!("\n  {}", "[retention]".bold());
                println!(
                    "    enforced             = {}",
                    "yes (sweep after sync + `mp store gc`)".green()
                );
                println!(
                    "    max_disk_bytes       = {}",
                    human_bytes(policy.max_disk_bytes)
                );
                println!(
                    "    body_horizon_days    = {}",
                    horizon_label(policy.body_horizon_days)
                );
                println!(
                    "    attachment_horizon_days = {}",
                    horizon_label(policy.attachment_horizon_days)
                );
            }
            Err(e) => {
                println!("\n  {}", "[retention]".bold());
                println!("    {}", format!("invalid: {e:#}").red());
            }
        }

        // Signatures are app-managed files since #0107, not config keys: the
        // daemon's `signature.list` and `signature.read` answer for the
        // directory and the state file.
        println!("\n  {}", "[signatures]".bold());
        println!(
            "    dir     = {}",
            crate::signatures::signatures_dir().display()
        );
        println!(
            "    default = {}",
            signatures
                .defaults
                .get(&account.name)
                .cloned()
                .flatten()
                .unwrap_or_else(|| "(none)".to_string())
        );
        if signatures.files.is_empty() {
            println!("    (none)");
        }
        for file in &signatures.files {
            // First line with content, skipping the RFC 3676 delimiter, which
            // every signature starts with and which identifies none of them.
            let preview: String = file
                .content
                .lines()
                .find(|l| !l.trim().is_empty() && l.trim() != "--")
                .unwrap_or_default()
                .chars()
                .take(60)
                .collect();
            println!("    {}.md: {preview}", file.name);
        }
        if !account.signatures.entries.is_empty() || account.signatures.default.is_some() {
            println!(
                "    {}",
                "[accounts.signatures] in config.toml is legacy and no longer read (#0107)"
                    .yellow()
            );
        }
    }

    Ok(())
}
