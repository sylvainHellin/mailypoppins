use anyhow::{Context, Result};
use colored::*;
use std::fs;
use std::io::{self, Write};

use crate::config::{
    all_configured_mailboxes, config_path, get_keyring_password, load_global_config,
    resolve_mailbox_local_path, set_keyring_password, slugify_mailbox_name, GlobalConfig,
};
use crate::imap_client::list_mailboxes;

/// Print the config file path.
pub(crate) fn cmd_config_path() {
    println!("{}", config_path().display());
}

/// Display the resolved config with masked passwords.
pub(crate) fn cmd_config_show() -> Result<()> {
    let config = load_global_config()?;

    println!("{}", "=== Email CLI Configuration ===".bold().cyan());
    println!("{}: {}", "Config file".bold(), config_path().display());

    println!("\n{}", "[email]".bold());
    println!("  font_family       = {}", config.email.font_family);
    println!("  font_size         = {}", config.email.font_size);
    println!("  include_signature = {}", config.email.include_signature);

    println!("\n{}", "[smtp]".bold());
    println!("  host         = {}", config.smtp.host);
    println!("  port         = {}", config.smtp.port);
    println!("  username     = {}", config.smtp.username);
    println!("  default_from = {}", config.smtp.default_from);
    let smtp_pw = if get_keyring_password("smtp-password").is_ok() {
        "****".green().to_string()
    } else {
        "(not set)".red().to_string()
    };
    println!("  password     = {} (keyring)", smtp_pw);

    println!("\n{}", "[imap]".bold());
    println!(
        "  host     = {}",
        if config.imap.host.is_empty() {
            format!("{} (falls back to smtp.host)", config.smtp.host)
                .dimmed()
                .to_string()
        } else {
            config.imap.host.clone()
        }
    );
    println!("  port     = {}", config.imap.port);
    println!(
        "  username = {}",
        if config.imap.username.is_empty() {
            format!("{} (falls back to smtp.username)", config.smtp.username)
                .dimmed()
                .to_string()
        } else {
            config.imap.username.clone()
        }
    );
    let imap_pw = if get_keyring_password("imap-password").is_ok() {
        "****".green().to_string()
    } else if get_keyring_password("smtp-password").is_ok() {
        format!("{} (falls back to smtp)", "****".green())
    } else {
        "(not set)".red().to_string()
    };
    println!("  password = {} (keyring)", imap_pw);

    println!("\n{}", "[directories]".bold());
    println!(
        "  root   = {}",
        config
            .directories
            .root
            .as_deref()
            .unwrap_or("(not set)")
    );
    println!(
        "  drafts = {}",
        config
            .directories
            .drafts
            .as_deref()
            .unwrap_or("(not set)")
    );

    println!("\n{}", "[mailboxes]".bold());
    let all = all_configured_mailboxes(&config);
    if all.is_empty() {
        println!("  (none configured)");
    } else {
        for (role, mapping) in &all {
            let resolved = resolve_mailbox_local_path(&config, mapping);
            println!(
                "  {} {} -> {}",
                format!("[{}]", role).bold(),
                mapping.server,
                resolved.display()
            );
        }
    }

    println!("\n{}", "[signatures]".bold());
    println!(
        "  default = {}",
        config
            .signatures
            .default
            .as_deref()
            .unwrap_or("(none)")
    );
    for (key, entry) in &config.signatures.entries {
        println!("  [signatures.{}]", key);
        if let Some(ref name) = entry.name {
            println!("    name = {}", name);
        }
        println!("    path = {}", entry.path);
    }

    Ok(())
}

/// Store a password in the OS keyring.
pub(crate) fn cmd_set_password(which: &str) -> Result<()> {
    let key = match which {
        "smtp" => "smtp-password",
        "imap" => "imap-password",
        _ => {
            eprintln!(
                "{} Unknown password type '{}'. Use 'smtp' or 'imap'.",
                "✗".red(),
                which
            );
            std::process::exit(1);
        }
    };

    let password = dialoguer::Password::new()
        .with_prompt(format!("Enter {} password", which))
        .interact()
        .context("Password input cancelled")?;

    set_keyring_password(key, &password)?;
    println!(
        "{} {} password stored in keyring",
        "✓".green(),
        which.to_uppercase()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Credential validation helpers
// ---------------------------------------------------------------------------

/// Test SMTP connection: connect with TLS and authenticate.
fn test_smtp_connection(host: &str, port: u16, username: &str, password: &str) -> Result<()> {
    use lettre::transport::smtp::authentication::Credentials;

    let creds = Credentials::new(username.to_string(), password.to_string());

    let mailer = if port == 465 {
        lettre::SmtpTransport::relay(host)?
            .port(port)
            .credentials(creds)
            .build()
    } else {
        lettre::SmtpTransport::starttls_relay(host)?
            .port(port)
            .credentials(creds)
            .build()
    };

    mailer.test_connection()?;
    Ok(())
}

/// Test IMAP connection: connect with TLS, login, and logout.
fn test_imap_connection(host: &str, port: u16, username: &str, password: &str) -> Result<()> {
    let tls = native_tls::TlsConnector::builder().build()?;
    let client = imap::connect((host, port), host, &tls)
        .map_err(|e| anyhow::anyhow!("Failed to connect to IMAP server: {}", e))?;

    let mut session = client
        .login(username, password)
        .map_err(|e| anyhow::anyhow!("IMAP login failed: {}", e.0))?;

    session.logout().ok();
    Ok(())
}

// ---------------------------------------------------------------------------
// Interactive config init wizard
// ---------------------------------------------------------------------------

/// Interactive setup wizard for creating the config file.
pub(crate) fn cmd_config_init() -> Result<()> {
    let path = config_path();

    if path.exists() {
        print!(
            "{} Config file already exists at {}. Overwrite? [y/N] ",
            "⚠".yellow(),
            path.display()
        );
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    println!("{}", "=== Email CLI Setup ===".bold().cyan());
    println!();

    // ── SMTP ──────────────────────────────────────────────────────────
    println!("{}", "SMTP Configuration".bold());
    let smtp_host = prompt_input("SMTP host", "smtp.example.com")?;
    let smtp_port: u16 = prompt_input("SMTP port", "465")?.parse().unwrap_or(465);
    let smtp_username = prompt_input("SMTP username (email)", "")?;
    let default_from = prompt_input("Default from address", &smtp_username)?;

    // SMTP password with connection test + retry
    let smtp_password = loop {
        let pw = dialoguer::Password::new()
            .with_prompt("SMTP password")
            .interact()
            .context("Password input cancelled")?;

        print!("  Testing SMTP connection... ");
        io::stdout().flush()?;
        match test_smtp_connection(&smtp_host, smtp_port, &smtp_username, &pw) {
            Ok(()) => {
                println!("{}", "OK".green());
                break pw;
            }
            Err(e) => {
                println!("{}", "FAILED".red());
                eprintln!("  Error: {}", e);
                print!("  Retry? [Y/n] ");
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                if matches!(input.trim().to_lowercase().as_str(), "n" | "no") {
                    return Err(anyhow::anyhow!("SMTP connection test failed. Aborting setup."));
                }
            }
        }
    };
    set_keyring_password("smtp-password", &smtp_password)?;
    println!("{} SMTP password stored in keyring", "✓".green());

    println!();

    // ── IMAP ──────────────────────────────────────────────────────────
    println!("{}", "IMAP Configuration".bold());
    let imap_host_input = prompt_input("IMAP host (leave empty to use SMTP host)", "")?;
    let imap_host = if imap_host_input.is_empty() {
        smtp_host.clone()
    } else {
        imap_host_input.clone()
    };
    let imap_port: u16 = prompt_input("IMAP port", "993")?.parse().unwrap_or(993);
    let imap_username_input =
        prompt_input("IMAP username (leave empty to use SMTP username)", "")?;
    let imap_username = if imap_username_input.is_empty() {
        smtp_username.clone()
    } else {
        imap_username_input.clone()
    };

    // IMAP password with connection test + retry
    print!("Use same password as SMTP? [Y/n] ");
    io::stdout().flush()?;
    let mut pw_input = String::new();
    io::stdin().read_line(&mut pw_input)?;
    let use_separate_imap_pw = matches!(pw_input.trim().to_lowercase().as_str(), "n" | "no");

    let imap_password = if use_separate_imap_pw {
        loop {
            let pw = dialoguer::Password::new()
                .with_prompt("IMAP password")
                .interact()
                .context("Password input cancelled")?;

            print!("  Testing IMAP connection... ");
            io::stdout().flush()?;
            match test_imap_connection(&imap_host, imap_port, &imap_username, &pw) {
                Ok(()) => {
                    println!("{}", "OK".green());
                    break pw;
                }
                Err(e) => {
                    println!("{}", "FAILED".red());
                    eprintln!("  Error: {}", e);
                    print!("  Retry? [Y/n] ");
                    io::stdout().flush()?;
                    let mut input = String::new();
                    io::stdin().read_line(&mut input)?;
                    if matches!(input.trim().to_lowercase().as_str(), "n" | "no") {
                        return Err(anyhow::anyhow!(
                            "IMAP connection test failed. Aborting setup."
                        ));
                    }
                }
            }
        }
    } else {
        // Test with SMTP password
        print!("  Testing IMAP connection... ");
        io::stdout().flush()?;
        match test_imap_connection(&imap_host, imap_port, &imap_username, &smtp_password) {
            Ok(()) => {
                println!("{}", "OK".green());
            }
            Err(e) => {
                println!("{}", "FAILED".red());
                eprintln!("  Error: {}", e);
                eprintln!(
                    "  {} SMTP password did not work for IMAP. You may need to set it separately later.",
                    "⚠".yellow()
                );
            }
        }
        smtp_password.clone()
    };

    if use_separate_imap_pw {
        set_keyring_password("imap-password", &imap_password)?;
        println!("{} IMAP password stored in keyring", "✓".green());
    }

    println!();

    // ── Mailbox discovery ─────────────────────────────────────────────
    println!("{}", "Mailbox Configuration".bold());
    print!("  Fetching mailbox list from server... ");
    io::stdout().flush()?;

    let imap_config = crate::config::ImapConfig {
        host: imap_host.clone(),
        port: imap_port,
        username: imap_username.clone(),
        password: imap_password.clone(),
    };

    let server_mailboxes = match list_mailboxes(&imap_config) {
        Ok(mbs) => {
            println!("{} ({} mailboxes)", "OK".green(), mbs.len());
            mbs
        }
        Err(e) => {
            println!("{}", "FAILED".red());
            eprintln!("  Error: {}", e);
            eprintln!("  Continuing with manual mailbox configuration.");
            Vec::new()
        }
    };

    // Select special-role mailboxes
    let (inbox_server, archive_server, sent_server) = if !server_mailboxes.is_empty() {
        let mut available = server_mailboxes.clone();

        let inbox = select_mailbox(
            "Which mailbox is your Inbox?",
            &available,
            &["INBOX"],
        )?;
        available.retain(|m| m != &inbox);

        let archive = select_mailbox(
            "Which mailbox is your Archive?",
            &available,
            &["Archive", "archive"],
        )?;
        available.retain(|m| m != &archive);

        let sent = select_mailbox(
            "Which mailbox is your Sent folder?",
            &available,
            &["Sent", "Sent Items", "INBOX.Sent"],
        )?;
        available.retain(|m| m != &sent);

        (inbox, archive, sent)
    } else {
        let inbox = prompt_input("Inbox mailbox name", "INBOX")?;
        let archive = prompt_input("Archive mailbox name", "Archive")?;
        let sent = prompt_input("Sent mailbox name", "Sent")?;
        (inbox, archive, sent)
    };

    // Select extra mailboxes to sync
    let extra_mailboxes: Vec<String> = if !server_mailboxes.is_empty() {
        let remaining: Vec<&str> = server_mailboxes
            .iter()
            .filter(|m| {
                m.as_str() != inbox_server
                    && m.as_str() != archive_server
                    && m.as_str() != sent_server
            })
            .map(|s| s.as_str())
            .collect();

        if !remaining.is_empty() {
            let selection = dialoguer::MultiSelect::new()
                .with_prompt("Select additional mailboxes to sync (optional)")
                .items(&remaining)
                .interact()
                .unwrap_or_default();

            selection.iter().map(|&i| remaining[i].to_string()).collect()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    println!();

    // ── Directories ───────────────────────────────────────────────────
    println!("{}", "Directory Configuration".bold());
    let root_dir = prompt_input("Root directory for email storage", "~/notes/email")?;
    let drafts_dir = prompt_input("Drafts directory (relative to root)", "drafts")?;

    // Derive local directory names from mailbox server names
    let inbox_local = slugify_mailbox_name(&inbox_server);
    let archive_local = slugify_mailbox_name(&archive_server);
    let sent_local = slugify_mailbox_name(&sent_server);

    println!();
    println!("{}", "Derived local paths:".bold());
    println!("  Inbox:   {}/{}", root_dir, inbox_local);
    println!("  Archive: {}/{}", root_dir, archive_local);
    println!("  Sent:    {}/{}", root_dir, sent_local);
    for mb in &extra_mailboxes {
        println!("  {}:   {}/{}", mb, root_dir, slugify_mailbox_name(mb));
    }
    println!("  Drafts:  {}/{}", root_dir, drafts_dir);

    // Check if existing config.toml has signature settings to preserve
    let existing_config: Option<GlobalConfig> = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|c| toml::from_str(&c).ok())
    } else {
        None
    };

    // ── Build config TOML ─────────────────────────────────────────────
    let mut toml_content = String::new();
    toml_content.push_str("[email]\n");
    toml_content.push_str("font_family = \"Helvetica, Arial, sans-serif\"\n");
    toml_content.push_str("font_size = \"12pt\"\n");
    toml_content.push_str("include_signature = true\n");

    toml_content.push_str("\n[smtp]\n");
    toml_content.push_str(&format!("host = \"{}\"\n", smtp_host));
    toml_content.push_str(&format!("port = {}\n", smtp_port));
    toml_content.push_str(&format!("username = \"{}\"\n", smtp_username));
    toml_content.push_str(&format!("default_from = \"{}\"\n", default_from));

    toml_content.push_str("\n[imap]\n");
    if !imap_host_input.is_empty() {
        toml_content.push_str(&format!("host = \"{}\"\n", imap_host_input));
    }
    toml_content.push_str(&format!("port = {}\n", imap_port));
    if !imap_username_input.is_empty() {
        toml_content.push_str(&format!("username = \"{}\"\n", imap_username_input));
    }

    toml_content.push_str("\n[directories]\n");
    toml_content.push_str(&format!("root = \"{}\"\n", root_dir));
    toml_content.push_str(&format!("drafts = \"{}\"\n", drafts_dir));

    // Special-role mailboxes
    toml_content.push_str("\n[mailboxes.inbox]\n");
    toml_content.push_str(&format!("server = \"{}\"\n", inbox_server));
    toml_content.push_str(&format!("local = \"{}\"\n", inbox_local));

    toml_content.push_str("\n[mailboxes.archive]\n");
    toml_content.push_str(&format!("server = \"{}\"\n", archive_server));
    toml_content.push_str(&format!("local = \"{}\"\n", archive_local));

    toml_content.push_str("\n[mailboxes.sent]\n");
    toml_content.push_str(&format!("server = \"{}\"\n", sent_server));
    toml_content.push_str(&format!("local = \"{}\"\n", sent_local));

    // Extra mailboxes
    for mb in &extra_mailboxes {
        toml_content.push_str(&format!("\n[[mailboxes.extra]]\n"));
        toml_content.push_str(&format!("server = \"{}\"\n", mb));
        toml_content.push_str(&format!("local = \"{}\"\n", slugify_mailbox_name(mb)));
    }

    // Preserve signature settings from existing config
    if let Some(ref existing) = existing_config {
        if !existing.signatures.entries.is_empty() || existing.signatures.default.is_some() {
            toml_content.push_str("\n[signatures]\n");
            if let Some(ref default) = existing.signatures.default {
                toml_content.push_str(&format!("default = \"{}\"\n", default));
            }
            for (key, entry) in &existing.signatures.entries {
                toml_content.push_str(&format!("\n[signatures.{}]\n", key));
                if let Some(ref name) = entry.name {
                    toml_content.push_str(&format!("name = \"{}\"\n", name));
                }
                toml_content.push_str(&format!("path = \"{}\"\n", entry.path));
            }
        }
    }

    // Write config file
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, toml_content)?;

    println!();
    println!(
        "{} Config written to {}",
        "✓".green().bold(),
        path.display()
    );
    println!(
        "{} Passwords stored in OS keyring (service: email-cli)",
        "✓".green().bold()
    );

    Ok(())
}

/// Prompt user to select a mailbox from a list using FuzzySelect.
/// Pre-selects the first matching candidate from `preferred`.
fn select_mailbox(prompt: &str, options: &[String], preferred: &[&str]) -> Result<String> {
    let default_idx = preferred
        .iter()
        .find_map(|pref| {
            options
                .iter()
                .position(|o| o.eq_ignore_ascii_case(pref))
        })
        .unwrap_or(0);

    let selection = dialoguer::FuzzySelect::new()
        .with_prompt(prompt)
        .items(options)
        .default(default_idx)
        .interact()
        .context("Selection cancelled")?;

    Ok(options[selection].clone())
}

fn prompt_input(prompt: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        print!("{}: ", prompt);
    } else {
        print!("{} [{}]: ", prompt, default);
    }
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();

    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}
