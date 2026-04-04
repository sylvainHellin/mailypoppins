use anyhow::{Context, Result};
use colored::*;
use std::fs;
use std::io::{self, Write};

use crate::config::{
    all_configured_mailboxes, config_path, get_keyring_password, load_global_config,
    resolve_mailbox_local_path, set_keyring_password, slugify_mailbox_name, AccountConfig,
};
use crate::imap_client::list_mailboxes;

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

/// Store a password in the OS keyring.
pub fn cmd_set_password(which: &str, account_name: &str) -> Result<()> {
    match which {
        "smtp" | "imap" => {},
        _ => {
            eprintln!(
                "{} Unknown password type '{}'. Use 'smtp' or 'imap'.",
                "✗".red(),
                which
            );
            std::process::exit(1);
        }
    };

    let key = format!("{}-password-{}", which, account_name);

    let password = dialoguer::Password::new()
        .with_prompt(format!("Enter {} password for account '{}'", which, account_name))
        .interact()
        .context("Password input cancelled")?;

    set_keyring_password(&key, &password)?;
    println!(
        "{} {} password for '{}' stored in keyring",
        "✓".green(),
        which.to_uppercase(),
        account_name
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Credential validation helpers
// ---------------------------------------------------------------------------

/// Test SMTP connection: connect with TLS and authenticate.
fn test_smtp_connection(host: &str, port: u16, username: &str, password: &str, accept_invalid_certs: bool) -> Result<()> {
    use lettre::transport::smtp::authentication::Credentials;

    let creds = Credentials::new(username.to_string(), password.to_string());

    let mailer = if port == 465 {
        let mut transport = lettre::SmtpTransport::relay(host)?;
        if accept_invalid_certs {
            let tls_params = lettre::transport::smtp::client::TlsParameters::builder(host.to_string())
                .dangerous_accept_invalid_certs(true)
                .build()?;
            transport = transport.tls(lettre::transport::smtp::client::Tls::Wrapper(tls_params));
        }
        transport.port(port).credentials(creds).build()
    } else {
        let mut transport = lettre::SmtpTransport::starttls_relay(host)?;
        if accept_invalid_certs {
            let tls_params = lettre::transport::smtp::client::TlsParameters::builder(host.to_string())
                .dangerous_accept_invalid_certs(true)
                .build()?;
            transport = transport.tls(lettre::transport::smtp::client::Tls::Required(tls_params));
        }
        transport.port(port).credentials(creds).build()
    };

    mailer.test_connection()?;
    Ok(())
}

/// Test IMAP connection: connect with TLS, login, and logout.
fn test_imap_connection(host: &str, port: u16, username: &str, password: &str, accept_invalid_certs: bool) -> Result<()> {
    use crate::imap_client::open_imap_session;

    let imap_config = crate::config::ImapConfig {
        host: host.to_string(),
        port,
        username: username.to_string(),
        password: password.to_string(),
        accept_invalid_certs,
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut session = open_imap_session(&imap_config).await?;
        session.logout().await.ok();
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Interactive config init wizard
// ---------------------------------------------------------------------------

/// Interactive setup wizard for creating the config file.
pub fn cmd_config_init() -> Result<()> {
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

    // ── Provider selection ─────────────────────────────────────────────
    println!("Select email provider:");
    println!("  1. Standard IMAP/SMTP (manual config)");
    println!("  2. Proton Mail (via Proton Bridge)");
    let provider_choice = prompt_input("Choice", "1")?;
    let is_proton = provider_choice.trim() == "2";
    println!();

    if is_proton {
        println!(
            "{} Proton Mail Bridge must be running. Use the bridge password (not your Proton password).",
            "\u{2139}".blue()
        );
        println!();
    }

    // Pre-fill defaults for Proton Bridge
    let default_smtp_host = if is_proton { "127.0.0.1" } else { "smtp.example.com" };
    let default_smtp_port = if is_proton { "1025" } else { "465" };
    let default_imap_host = if is_proton { "127.0.0.1" } else { "" };
    let default_imap_port = if is_proton { "1143" } else { "993" };
    let accept_invalid_certs = is_proton;

    // ── Account name ──────────────────────────────────────────────────
    let default_account_name = if is_proton { "proton" } else { "main" };
    let account_name = prompt_input("Account name (unique slug, e.g. 'tum', 'gmail')", default_account_name)?;
    println!();

    // ── SMTP ──────────────────────────────────────────────────────────
    println!("{}", "SMTP Configuration".bold());
    let smtp_host = prompt_input("SMTP host", default_smtp_host)?;
    let smtp_port: u16 = prompt_input("SMTP port", default_smtp_port)?.parse().unwrap_or(465);
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
        match test_smtp_connection(&smtp_host, smtp_port, &smtp_username, &pw, accept_invalid_certs) {
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
    let smtp_keyring_key = format!("smtp-password-{}", account_name);
    set_keyring_password(&smtp_keyring_key, &smtp_password)?;
    println!("{} SMTP password stored in keyring", "✓".green());

    println!();

    // ── IMAP ──────────────────────────────────────────────────────────
    println!("{}", "IMAP Configuration".bold());
    let imap_host_input = prompt_input("IMAP host (leave empty to use SMTP host)", default_imap_host)?;
    let imap_host = if imap_host_input.is_empty() {
        smtp_host.clone()
    } else {
        imap_host_input.clone()
    };
    let imap_port: u16 = prompt_input("IMAP port", default_imap_port)?.parse().unwrap_or(993);
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
            match test_imap_connection(&imap_host, imap_port, &imap_username, &pw, accept_invalid_certs) {
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
        match test_imap_connection(&imap_host, imap_port, &imap_username, &smtp_password, accept_invalid_certs) {
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
        let imap_keyring_key = format!("imap-password-{}", account_name);
        set_keyring_password(&imap_keyring_key, &imap_password)?;
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
        accept_invalid_certs,
    };

    let rt = tokio::runtime::Runtime::new()?;
    let server_mailboxes = match rt.block_on(list_mailboxes(&imap_config)) {
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

    // Check if existing config.toml has account signature settings to preserve
    let existing_config: Option<crate::config::GlobalConfig> = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|c| toml::from_str(&c).ok())
    } else {
        None
    };
    let existing_sigs: Option<&AccountConfig> = existing_config.as_ref()
        .and_then(|c| c.accounts.iter().find(|a| a.name == account_name));

    // ── Build config TOML ─────────────────────────────────────────────
    let mut toml_content = String::new();
    toml_content.push_str("[email]\n");
    toml_content.push_str("font_family = \"Helvetica, Arial, sans-serif\"\n");
    toml_content.push_str("font_size = \"12pt\"\n");
    toml_content.push_str("include_signature = true\n");

    toml_content.push_str("\n[[accounts]]\n");
    toml_content.push_str(&format!("name = \"{}\"\n", account_name));
    toml_content.push_str(&format!("default_from = \"{}\"\n", default_from));

    toml_content.push_str("\n[accounts.smtp]\n");
    toml_content.push_str(&format!("host = \"{}\"\n", smtp_host));
    toml_content.push_str(&format!("port = {}\n", smtp_port));
    toml_content.push_str(&format!("username = \"{}\"\n", smtp_username));
    if accept_invalid_certs {
        toml_content.push_str("accept_invalid_certs = true\n");
    }

    toml_content.push_str("\n[accounts.imap]\n");
    if !imap_host_input.is_empty() {
        toml_content.push_str(&format!("host = \"{}\"\n", imap_host_input));
    }
    toml_content.push_str(&format!("port = {}\n", imap_port));
    if !imap_username_input.is_empty() {
        toml_content.push_str(&format!("username = \"{}\"\n", imap_username_input));
    }
    if accept_invalid_certs {
        toml_content.push_str("accept_invalid_certs = true\n");
    }

    toml_content.push_str("\n[accounts.directories]\n");
    toml_content.push_str(&format!("root = \"{}\"\n", root_dir));
    toml_content.push_str(&format!("drafts = \"{}\"\n", drafts_dir));

    // Special-role mailboxes
    toml_content.push_str("\n[accounts.mailboxes.inbox]\n");
    toml_content.push_str(&format!("server = \"{}\"\n", inbox_server));
    toml_content.push_str(&format!("local = \"{}\"\n", inbox_local));

    toml_content.push_str("\n[accounts.mailboxes.archive]\n");
    toml_content.push_str(&format!("server = \"{}\"\n", archive_server));
    toml_content.push_str(&format!("local = \"{}\"\n", archive_local));

    toml_content.push_str("\n[accounts.mailboxes.sent]\n");
    toml_content.push_str(&format!("server = \"{}\"\n", sent_server));
    toml_content.push_str(&format!("local = \"{}\"\n", sent_local));

    // Extra mailboxes
    for mb in &extra_mailboxes {
        toml_content.push_str("\n[[accounts.mailboxes.extra]]\n");
        toml_content.push_str(&format!("server = \"{}\"\n", mb));
        toml_content.push_str(&format!("local = \"{}\"\n", slugify_mailbox_name(mb)));
    }

    // Preserve signature settings from existing config
    if let Some(existing_acct) = existing_sigs {
        if !existing_acct.signatures.entries.is_empty() || existing_acct.signatures.default.is_some() {
            toml_content.push_str("\n[accounts.signatures]\n");
            if let Some(ref default) = existing_acct.signatures.default {
                toml_content.push_str(&format!("default = \"{}\"\n", default));
            }
            for (key, entry) in &existing_acct.signatures.entries {
                toml_content.push_str(&format!("\n[accounts.signatures.{}]\n", key));
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

/// Add a new account to an existing config file.
pub fn cmd_config_add_account() -> Result<()> {
    let path = config_path();
    if !path.exists() {
        return Err(anyhow::anyhow!(
            "Config file not found at {}. Run `email config init` first.",
            path.display()
        ));
    }

    // Load existing config to check for name conflicts
    let existing = load_global_config()?;
    let existing_names: Vec<&str> = existing.accounts.iter().map(|a| a.name.as_str()).collect();
    println!(
        "{} Existing accounts: {}",
        "\u{2139}".blue(),
        if existing_names.is_empty() { "(none)".to_string() } else { existing_names.join(", ") }
    );
    println!();

    // ── Provider selection ──────────────────────────────────────────
    println!("Select email provider:");
    println!("  1. Standard IMAP/SMTP (manual config)");
    println!("  2. Proton Mail (via Proton Bridge)");
    let provider_choice = prompt_input("Choice", "1")?;
    let is_proton = provider_choice.trim() == "2";
    println!();

    if is_proton {
        println!(
            "{} Proton Mail Bridge must be running. Use the bridge password (not your Proton password).",
            "\u{2139}".blue()
        );
        println!();
    }

    let default_smtp_host = if is_proton { "127.0.0.1" } else { "smtp.example.com" };
    let default_smtp_port = if is_proton { "1025" } else { "465" };
    let default_imap_host = if is_proton { "127.0.0.1" } else { "" };
    let default_imap_port = if is_proton { "1143" } else { "993" };
    let accept_invalid_certs = is_proton;
    let default_account_name = if is_proton { "proton" } else { "work" };

    // ── Account name ───────────────────────────────────────────────
    let account_name = loop {
        let name = prompt_input("Account name (unique slug)", default_account_name)?;
        if existing_names.contains(&name.as_str()) {
            println!("{} Account '{}' already exists. Choose a different name.", "\u{26a0}".yellow(), name);
        } else {
            break name;
        }
    };
    println!();

    // ── SMTP ───────────────────────────────────────────────────────
    println!("{}", "SMTP Configuration".bold());
    let smtp_host = prompt_input("SMTP host", default_smtp_host)?;
    let smtp_port: u16 = prompt_input("SMTP port", default_smtp_port)?.parse().unwrap_or(465);
    let smtp_username = prompt_input("SMTP username (email)", "")?;
    let default_from = prompt_input("Default from address", &smtp_username)?;

    let smtp_password = loop {
        let pw = dialoguer::Password::new()
            .with_prompt("SMTP password")
            .interact()
            .context("Password input cancelled")?;
        print!("  Testing SMTP connection... ");
        io::stdout().flush()?;
        match test_smtp_connection(&smtp_host, smtp_port, &smtp_username, &pw, accept_invalid_certs) {
            Ok(()) => { println!("{}", "OK".green()); break pw; }
            Err(e) => {
                println!("{}", "FAILED".red());
                eprintln!("  Error: {}", e);
                print!("  Retry? [Y/n] ");
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                if matches!(input.trim().to_lowercase().as_str(), "n" | "no") {
                    return Err(anyhow::anyhow!("SMTP connection test failed."));
                }
            }
        }
    };
    let smtp_key = format!("smtp-password-{}", account_name);
    set_keyring_password(&smtp_key, &smtp_password)?;
    println!("{} SMTP password stored in keyring", "\u{2713}".green());
    println!();

    // ── IMAP ───────────────────────────────────────────────────────
    println!("{}", "IMAP Configuration".bold());
    let imap_host_input = prompt_input("IMAP host (leave empty to use SMTP host)", default_imap_host)?;
    let imap_host = if imap_host_input.is_empty() { smtp_host.clone() } else { imap_host_input.clone() };
    let imap_port: u16 = prompt_input("IMAP port", default_imap_port)?.parse().unwrap_or(993);
    let imap_username_input = prompt_input("IMAP username (leave empty to use SMTP username)", "")?;
    let imap_username = if imap_username_input.is_empty() { smtp_username.clone() } else { imap_username_input.clone() };

    print!("Use same password as SMTP? [Y/n] ");
    io::stdout().flush()?;
    let mut pw_input = String::new();
    io::stdin().read_line(&mut pw_input)?;
    let use_separate = matches!(pw_input.trim().to_lowercase().as_str(), "n" | "no");
    let imap_password = if use_separate {
        loop {
            let pw = dialoguer::Password::new()
                .with_prompt("IMAP password")
                .interact()
                .context("Password input cancelled")?;
            print!("  Testing IMAP connection... ");
            io::stdout().flush()?;
            match test_imap_connection(&imap_host, imap_port, &imap_username, &pw, accept_invalid_certs) {
                Ok(()) => { println!("{}", "OK".green()); break pw; }
                Err(e) => {
                    println!("{}", "FAILED".red());
                    eprintln!("  Error: {}", e);
                    print!("  Retry? [Y/n] ");
                    io::stdout().flush()?;
                    let mut input = String::new();
                    io::stdin().read_line(&mut input)?;
                    if matches!(input.trim().to_lowercase().as_str(), "n" | "no") {
                        return Err(anyhow::anyhow!("IMAP connection test failed."));
                    }
                }
            }
        }
    } else {
        print!("  Testing IMAP connection... ");
        io::stdout().flush()?;
        match test_imap_connection(&imap_host, imap_port, &imap_username, &smtp_password, accept_invalid_certs) {
            Ok(()) => println!("{}", "OK".green()),
            Err(e) => {
                println!("{}", "FAILED".red());
                eprintln!("  {} SMTP password did not work for IMAP.", "\u{26a0}".yellow());
                eprintln!("  Error: {}", e);
            }
        }
        smtp_password.clone()
    };
    if use_separate {
        let imap_key = format!("imap-password-{}", account_name);
        set_keyring_password(&imap_key, &imap_password)?;
        println!("{} IMAP password stored in keyring", "\u{2713}".green());
    }
    println!();

    // ── Mailbox discovery ──────────────────────────────────────────
    println!("{}", "Mailbox Configuration".bold());
    print!("  Fetching mailbox list from server... ");
    io::stdout().flush()?;
    let imap_config = crate::config::ImapConfig {
        host: imap_host.clone(), port: imap_port,
        username: imap_username.clone(), password: imap_password.clone(),
        accept_invalid_certs,
    };
    let rt = tokio::runtime::Runtime::new()?;
    let server_mailboxes = match rt.block_on(list_mailboxes(&imap_config)) {
        Ok(mbs) => { println!("{} ({} mailboxes)", "OK".green(), mbs.len()); mbs }
        Err(e) => { println!("{}", "FAILED".red()); eprintln!("  Error: {}", e); Vec::new() }
    };

    let (inbox_server, archive_server, sent_server) = if !server_mailboxes.is_empty() {
        let mut available = server_mailboxes.clone();
        let inbox = select_mailbox("Which mailbox is your Inbox?", &available, &["INBOX"])?;
        available.retain(|m| m != &inbox);
        let archive = select_mailbox("Which mailbox is your Archive?", &available, &["Archive", "All Mail"])?;
        available.retain(|m| m != &archive);
        let sent = select_mailbox("Which mailbox is your Sent folder?", &available, &["Sent", "Sent Items", "Sent Messages"])?;
        (inbox, archive, sent)
    } else {
        let inbox = prompt_input("Inbox mailbox name", "INBOX")?;
        let archive = prompt_input("Archive mailbox name", "All Mail")?;
        let sent = prompt_input("Sent mailbox name", "Sent")?;
        (inbox, archive, sent)
    };

    let extra_mailboxes: Vec<String> = if !server_mailboxes.is_empty() {
        let remaining: Vec<&str> = server_mailboxes.iter()
            .filter(|m| m.as_str() != inbox_server && m.as_str() != archive_server && m.as_str() != sent_server)
            .map(|s| s.as_str()).collect();
        if !remaining.is_empty() {
            let selection = dialoguer::MultiSelect::new()
                .with_prompt("Select additional mailboxes to sync (optional)")
                .items(&remaining).interact().unwrap_or_default();
            selection.iter().map(|&i| remaining[i].to_string()).collect()
        } else { Vec::new() }
    } else { Vec::new() };
    println!();

    // ── Directories ────────────────────────────────────────────────
    println!("{}", "Directory Configuration".bold());
    let default_root = format!("~/notes/email/{}", account_name);
    let root_dir = prompt_input("Root directory for this account", &default_root)?;
    let drafts_dir = prompt_input("Drafts directory (relative to root)", "drafts")?;

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

    // ── Append to config file ──────────────────────────────────────
    let mut block = String::from("\n");
    block.push_str("[[accounts]]\n");
    block.push_str(&format!("name = \"{}\"\n", account_name));
    block.push_str(&format!("default_from = \"{}\"\n", default_from));

    block.push_str("\n[accounts.smtp]\n");
    block.push_str(&format!("host = \"{}\"\n", smtp_host));
    block.push_str(&format!("port = {}\n", smtp_port));
    block.push_str(&format!("username = \"{}\"\n", smtp_username));
    if accept_invalid_certs { block.push_str("accept_invalid_certs = true\n"); }

    block.push_str("\n[accounts.imap]\n");
    if !imap_host_input.is_empty() { block.push_str(&format!("host = \"{}\"\n", imap_host_input)); }
    block.push_str(&format!("port = {}\n", imap_port));
    if !imap_username_input.is_empty() { block.push_str(&format!("username = \"{}\"\n", imap_username_input)); }
    if accept_invalid_certs { block.push_str("accept_invalid_certs = true\n"); }

    block.push_str("\n[accounts.directories]\n");
    block.push_str(&format!("root = \"{}\"\n", root_dir));
    block.push_str(&format!("drafts = \"{}\"\n", drafts_dir));

    block.push_str("\n[accounts.mailboxes.inbox]\n");
    block.push_str(&format!("server = \"{}\"\nlocal = \"{}\"\n", inbox_server, inbox_local));
    block.push_str("\n[accounts.mailboxes.archive]\n");
    block.push_str(&format!("server = \"{}\"\nlocal = \"{}\"\n", archive_server, archive_local));
    block.push_str("\n[accounts.mailboxes.sent]\n");
    block.push_str(&format!("server = \"{}\"\nlocal = \"{}\"\n", sent_server, sent_local));
    for mb in &extra_mailboxes {
        block.push_str("\n[[accounts.mailboxes.extra]]\n");
        block.push_str(&format!("server = \"{}\"\nlocal = \"{}\"\n", mb, slugify_mailbox_name(mb)));
    }

    // Read existing config and append
    let mut content = fs::read_to_string(&path)?;
    content.push_str(&block);
    fs::write(&path, content)?;

    println!();
    println!(
        "{} Account '{}' added to {}",
        "\u{2713}".green().bold(),
        account_name,
        path.display()
    );
    println!(
        "{} Passwords stored in OS keyring (service: email-cli)",
        "\u{2713}".green().bold()
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
