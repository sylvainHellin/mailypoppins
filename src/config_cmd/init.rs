use anyhow::{Context, Result};
use colored::*;
use std::fs;
use std::io::{self, Write};

use crate::config::{
    config_path, load_global_config, set_keyring_password,
    slugify_mailbox_name, AccountConfig,
};
use crate::imap_client::list_mailboxes;

use super::helpers::{prompt_input, select_mailbox, test_imap_connection, test_smtp_connection};

/// Interactive setup wizard for creating the config file.
pub fn cmd_config_init() -> Result<()> {
    let path = config_path();

    if path.exists() {
        print!(
            "{} Config file already exists at {}. Overwrite? [y/N] ",
            "\u{26a0}".yellow(),
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

    // -- Provider selection
    println!("Select email provider:");
    println!("  1. Standard IMAP/SMTP (manual config)");
    println!("  2. Proton Mail (via Proton Bridge)");
    println!("  3. Microsoft 365 / Exchange Online (OAuth2 IMAP/SMTP)");
    println!("  4. Microsoft 365 / Exchange Online (Graph API)");
    let provider_choice = prompt_input("Choice", "1")?;
    let is_proton = provider_choice.trim() == "2";
    let is_exchange = provider_choice.trim() == "3";
    let is_graph = provider_choice.trim() == "4";
    println!();

    if is_proton {
        println!(
            "{} Proton Mail Bridge must be running. Use the bridge password (not your Proton password).",
            "\u{2139}".blue()
        );
        println!();
    }

    if is_exchange {
        println!(
            "{} Microsoft 365 requires an Azure Entra ID app registration.",
            "\u{2139}".blue()
        );
        println!("  See docs/exchange-setup.md for step-by-step instructions.");
        println!();
    }

    // -- Graph API early-return flow (no IMAP/SMTP needed)
    if is_graph {
        return graph_init_flow(&path);
    }

    // Pre-fill defaults per provider
    let default_smtp_host = if is_proton {
        "127.0.0.1"
    } else if is_exchange {
        "smtp.office365.com"
    } else {
        "smtp.example.com"
    };
    let default_smtp_port = if is_proton {
        "1025"
    } else if is_exchange {
        "587"
    } else {
        "465"
    };
    let default_imap_host = if is_proton {
        "127.0.0.1"
    } else if is_exchange {
        "outlook.office365.com"
    } else {
        ""
    };
    let default_imap_port = if is_proton { "1143" } else { "993" };
    let accept_invalid_certs = is_proton;

    // -- Account name
    let default_account_name = if is_proton {
        "proton"
    } else if is_exchange {
        "exchange"
    } else {
        "main"
    };
    let account_name = prompt_input("Account name (unique slug, e.g. 'tum', 'gmail')", default_account_name)?;
    println!();

    // -- SMTP
    println!("{}", "SMTP Configuration".bold());
    let smtp_host = prompt_input("SMTP host", default_smtp_host)?;
    let smtp_port: u16 = prompt_input("SMTP port", default_smtp_port)?.parse().unwrap_or(465);
    let smtp_username = prompt_input("SMTP username (email)", "")?;
    let default_from = prompt_input("Default from address", &smtp_username)?;

    // -- OAuth2 settings (Exchange only)
    let mut oauth2_client_id = String::new();
    let mut oauth2_tenant_id = String::new();

    // -- Authentication: OAuth2 or password
    let (imap_host_input, imap_host, imap_port, imap_username_input, imap_password);

    if is_exchange {
        // OAuth2 flow: prompt for client_id and tenant_id
        println!("{}", "OAuth2 Configuration".bold());
        oauth2_client_id = prompt_input("Azure App client_id", "")?;
        oauth2_tenant_id = prompt_input("Azure tenant_id", "")?;
        println!();

        // IMAP config (pre-filled for Exchange)
        println!("{}", "IMAP Configuration".bold());
        imap_host_input = prompt_input("IMAP host", default_imap_host)?;
        imap_host = if imap_host_input.is_empty() {
            smtp_host.clone()
        } else {
            imap_host_input.clone()
        };
        imap_port = prompt_input("IMAP port", default_imap_port)?.parse().unwrap_or(993);
        imap_username_input = prompt_input("IMAP username (leave empty to use SMTP username)", "")?;

        // Run device code flow
        println!();
        println!("{} Running OAuth2 device code flow...", "\u{2139}".blue());
        let rt = tokio::runtime::Runtime::new()?;
        let cache = rt.block_on(crate::oauth2::device_code_flow(
            &oauth2_client_id,
            &oauth2_tenant_id,
            &account_name,
            crate::oauth2::IMAP_SMTP_SCOPES,
        ))?;
        println!("{} OAuth2 token acquired and cached", "\u{2713}".green());
        imap_password = cache.access_token;
    } else {
        // Password flow (existing behavior)
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
        println!("{} SMTP password stored in keyring", "\u{2713}".green());

        println!();

        // -- IMAP
        println!("{}", "IMAP Configuration".bold());
        imap_host_input = prompt_input("IMAP host (leave empty to use SMTP host)", default_imap_host)?;
        imap_host = if imap_host_input.is_empty() {
            smtp_host.clone()
        } else {
            imap_host_input.clone()
        };
        imap_port = prompt_input("IMAP port", default_imap_port)?.parse().unwrap_or(993);
        imap_username_input =
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

        imap_password = if use_separate_imap_pw {
            let pw = loop {
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
            };
            let imap_keyring_key = format!("imap-password-{}", account_name);
            set_keyring_password(&imap_keyring_key, &pw)?;
            println!("{} IMAP password stored in keyring", "\u{2713}".green());
            pw
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
                        "\u{26a0}".yellow()
                    );
                }
            }
            smtp_password.clone()
        };
    }

    println!();

    // -- Mailbox discovery
    println!("{}", "Mailbox Configuration".bold());
    print!("  Fetching mailbox list from server... ");
    io::stdout().flush()?;

    let imap_username = if imap_username_input.is_empty() {
        smtp_username.clone()
    } else {
        imap_username_input.clone()
    };

    let imap_config = crate::config::ImapConfig {
        host: imap_host.clone(),
        port: imap_port,
        username: imap_username.clone(),
        password: imap_password.clone(),
        accept_invalid_certs,
        auth_method: if is_exchange {
            crate::config::AuthMethod::OAuth2
        } else {
            crate::config::AuthMethod::Password
        },
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

    // -- Directories
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

    // -- Build config TOML
    let oauth2_cfg = if is_exchange {
        Some((&oauth2_client_id as &str, &oauth2_tenant_id as &str))
    } else {
        None
    };
    let toml_content = build_init_toml(
        &account_name, &default_from,
        &smtp_host, smtp_port, &smtp_username, accept_invalid_certs,
        &imap_host_input, imap_port, &imap_username_input,
        &root_dir, &drafts_dir,
        &inbox_server, &inbox_local,
        &archive_server, &archive_local,
        &sent_server, &sent_local,
        &extra_mailboxes,
        existing_sigs,
        oauth2_cfg,
    );

    // Write config file
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, toml_content)?;

    println!();
    println!(
        "{} Config written to {}",
        "\u{2713}".green().bold(),
        path.display()
    );
    if is_exchange {
        println!(
            "{} OAuth2 token cached at ~/.email-cli/tokens/",
            "\u{2713}".green().bold()
        );
    } else {
        println!(
            "{} Passwords stored in OS keyring (service: email-cli)",
            "\u{2713}".green().bold()
        );
    }

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

    // -- Provider selection
    println!("Select email provider:");
    println!("  1. Standard IMAP/SMTP (manual config)");
    println!("  2. Proton Mail (via Proton Bridge)");
    println!("  3. Microsoft 365 / Exchange Online (OAuth2 IMAP/SMTP)");
    println!("  4. Microsoft 365 / Exchange Online (Graph API)");
    let provider_choice = prompt_input("Choice", "1")?;
    let is_proton = provider_choice.trim() == "2";
    let is_exchange = provider_choice.trim() == "3";
    let is_graph = provider_choice.trim() == "4";
    println!();

    if is_proton {
        println!(
            "{} Proton Mail Bridge must be running. Use the bridge password (not your Proton password).",
            "\u{2139}".blue()
        );
        println!();
    }

    if is_exchange {
        println!(
            "{} Microsoft 365 requires an Azure Entra ID app registration.",
            "\u{2139}".blue()
        );
        println!("  See docs/exchange-setup.md for step-by-step instructions.");
        println!();
    }

    // -- Graph API early-return flow (no IMAP/SMTP needed)
    if is_graph {
        return graph_add_account_flow(&path, &existing_names);
    }

    let default_smtp_host = if is_proton {
        "127.0.0.1"
    } else if is_exchange {
        "smtp.office365.com"
    } else {
        "smtp.example.com"
    };
    let default_smtp_port = if is_proton {
        "1025"
    } else if is_exchange {
        "587"
    } else {
        "465"
    };
    let default_imap_host = if is_proton {
        "127.0.0.1"
    } else if is_exchange {
        "outlook.office365.com"
    } else {
        ""
    };
    let default_imap_port = if is_proton { "1143" } else { "993" };
    let accept_invalid_certs = is_proton;
    let default_account_name = if is_proton {
        "proton"
    } else if is_exchange {
        "exchange"
    } else {
        "work"
    };

    // -- Account name
    let account_name = loop {
        let name = prompt_input("Account name (unique slug)", default_account_name)?;
        if existing_names.contains(&name.as_str()) {
            println!("{} Account '{}' already exists. Choose a different name.", "\u{26a0}".yellow(), name);
        } else {
            break name;
        }
    };
    println!();

    // -- SMTP
    println!("{}", "SMTP Configuration".bold());
    let smtp_host = prompt_input("SMTP host", default_smtp_host)?;
    let smtp_port: u16 = prompt_input("SMTP port", default_smtp_port)?.parse().unwrap_or(465);
    let smtp_username = prompt_input("SMTP username (email)", "")?;
    let default_from = prompt_input("Default from address", &smtp_username)?;

    // -- OAuth2 settings (Exchange only)
    let mut oauth2_client_id = String::new();
    let mut oauth2_tenant_id = String::new();

    // -- Authentication: OAuth2 or password
    let (imap_host_input, imap_host, imap_port, imap_username_input, imap_password);

    if is_exchange {
        println!();
        println!("{}", "OAuth2 Configuration".bold());
        oauth2_client_id = prompt_input("Azure App client_id", "")?;
        oauth2_tenant_id = prompt_input("Azure tenant_id", "")?;
        println!();

        println!("{}", "IMAP Configuration".bold());
        imap_host_input = prompt_input("IMAP host", default_imap_host)?;
        imap_host = if imap_host_input.is_empty() { smtp_host.clone() } else { imap_host_input.clone() };
        imap_port = prompt_input("IMAP port", default_imap_port)?.parse().unwrap_or(993);
        imap_username_input = prompt_input("IMAP username (leave empty to use SMTP username)", "")?;

        println!();
        println!("{} Running OAuth2 device code flow...", "\u{2139}".blue());
        let rt = tokio::runtime::Runtime::new()?;
        let cache = rt.block_on(crate::oauth2::device_code_flow(
            &oauth2_client_id,
            &oauth2_tenant_id,
            &account_name,
            crate::oauth2::IMAP_SMTP_SCOPES,
        ))?;
        println!("{} OAuth2 token acquired and cached", "\u{2713}".green());
        imap_password = cache.access_token;
    } else {
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

        println!("{}", "IMAP Configuration".bold());
        imap_host_input = prompt_input("IMAP host (leave empty to use SMTP host)", default_imap_host)?;
        imap_host = if imap_host_input.is_empty() { smtp_host.clone() } else { imap_host_input.clone() };
        imap_port = prompt_input("IMAP port", default_imap_port)?.parse().unwrap_or(993);
        imap_username_input = prompt_input("IMAP username (leave empty to use SMTP username)", "")?;
        let imap_username = if imap_username_input.is_empty() { smtp_username.clone() } else { imap_username_input.clone() };

        print!("Use same password as SMTP? [Y/n] ");
        io::stdout().flush()?;
        let mut pw_input = String::new();
        io::stdin().read_line(&mut pw_input)?;
        let use_separate = matches!(pw_input.trim().to_lowercase().as_str(), "n" | "no");
        imap_password = if use_separate {
            let pw = loop {
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
            };
            let imap_key = format!("imap-password-{}", account_name);
            set_keyring_password(&imap_key, &pw)?;
            println!("{} IMAP password stored in keyring", "\u{2713}".green());
            pw
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
    }
    println!();

    // -- Mailbox discovery
    println!("{}", "Mailbox Configuration".bold());
    print!("  Fetching mailbox list from server... ");
    io::stdout().flush()?;
    let imap_username = if imap_username_input.is_empty() { smtp_username.clone() } else { imap_username_input.clone() };
    let imap_config = crate::config::ImapConfig {
        host: imap_host.clone(), port: imap_port,
        username: imap_username.clone(), password: imap_password.clone(),
        accept_invalid_certs,
        auth_method: if is_exchange {
            crate::config::AuthMethod::OAuth2
        } else {
            crate::config::AuthMethod::Password
        },
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

    // -- Directories
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

    // -- Append to config file
    let oauth2_cfg = if is_exchange {
        Some((&oauth2_client_id as &str, &oauth2_tenant_id as &str))
    } else {
        None
    };
    let block = build_add_account_toml(
        &account_name, &default_from,
        &smtp_host, smtp_port, &smtp_username, accept_invalid_certs,
        &imap_host_input, imap_port, &imap_username_input,
        &root_dir, &drafts_dir,
        &inbox_server, &inbox_local,
        &archive_server, &archive_local,
        &sent_server, &sent_local,
        &extra_mailboxes,
        oauth2_cfg,
    );

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
    if is_exchange {
        println!(
            "{} OAuth2 token cached at ~/.email-cli/tokens/",
            "\u{2713}".green().bold()
        );
    } else {
        println!(
            "{} Passwords stored in OS keyring (service: email-cli)",
            "\u{2713}".green().bold()
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Graph API wizard flows
// ---------------------------------------------------------------------------

/// Graph API flow for `config init` (creates a new config file).
fn graph_init_flow(path: &std::path::Path) -> Result<()> {
    println!(
        "{} Microsoft 365 (Graph API) -- no IMAP/SMTP needed.",
        "\u{2139}".blue()
    );
    println!("  Requires an Azure Entra ID app with Mail.Read, Mail.ReadWrite, Mail.Send permissions.");
    println!("  See docs/exchange-setup.md for details.");
    println!();

    // -- Account name
    let account_name = prompt_input("Account name (unique slug, e.g. 'exchange', 'hines')", "exchange")?;
    println!();

    // -- Email address
    let default_from = prompt_input("Email address", "")?;
    println!();

    // -- OAuth2 settings
    println!("{}", "OAuth2 Configuration".bold());
    let oauth2_client_id = prompt_input("Azure App client_id", "")?;
    let oauth2_tenant_id = prompt_input("Azure tenant_id", "")?;
    println!();

    // -- Device code flow
    println!("{} Running OAuth2 device code flow (Graph API)...", "\u{2139}".blue());
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::oauth2::device_code_flow(
        &oauth2_client_id,
        &oauth2_tenant_id,
        &account_name,
        crate::oauth2::GRAPH_SCOPES,
    ))?;
    println!("{} OAuth2 token acquired and cached", "\u{2713}".green());
    println!();

    // -- Test Graph API connection
    print!("  Testing Graph API connection... ");
    io::stdout().flush()?;
    let graph_config = crate::config::GraphConfig {
        client_id: oauth2_client_id.clone(),
        tenant_id: oauth2_tenant_id.clone(),
        username: default_from.clone(),
        account_name: account_name.clone(),
    };
    match rt.block_on(crate::graph::GraphClient::new_async(&graph_config)) {
        Ok(_client) => println!("{}", "OK".green()),
        Err(e) => {
            println!("{}", "FAILED".red());
            eprintln!("  {} Graph API test failed: {}", "\u{26a0}".yellow(), e);
            eprintln!("  Continuing with setup (you can re-test later with `email config oauth2-login`).");
        }
    }
    println!();

    // -- Folder discovery via Graph API
    println!("{}", "Mailbox Configuration".bold());
    print!("  Fetching folder list from Graph API... ");
    io::stdout().flush()?;

    let folder_names: Vec<String> = match rt.block_on(async {
        let client = crate::graph::GraphClient::new_async(&graph_config).await?;
        client.list_folders().await
    }) {
        Ok(folders) => {
            println!("{} ({} folders)", "OK".green(), folders.len());
            folders.iter().map(|f| f.display_name.clone()).collect()
        }
        Err(e) => {
            println!("{}", "FAILED".red());
            eprintln!("  Error: {}", e);
            eprintln!("  Continuing with manual mailbox configuration.");
            Vec::new()
        }
    };

    // Select special-role mailboxes
    let (inbox_server, archive_server, sent_server) = if !folder_names.is_empty() {
        let mut available = folder_names.clone();
        let inbox = select_mailbox("Which folder is your Inbox?", &available, &["Inbox"])?;
        available.retain(|m| m != &inbox);
        let archive = select_mailbox("Which folder is your Archive?", &available, &["Archive"])?;
        available.retain(|m| m != &archive);
        let sent = select_mailbox("Which folder is your Sent folder?", &available, &["Sent Items"])?;
        available.retain(|m| m != &sent);
        (inbox, archive, sent)
    } else {
        let inbox = prompt_input("Inbox folder name", "Inbox")?;
        let archive = prompt_input("Archive folder name", "Archive")?;
        let sent = prompt_input("Sent folder name", "Sent Items")?;
        (inbox, archive, sent)
    };

    // Extra mailboxes
    let extra_mailboxes: Vec<String> = if !folder_names.is_empty() {
        let remaining: Vec<&str> = folder_names
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
                .with_prompt("Select additional folders to sync (optional)")
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

    // -- Directories
    println!("{}", "Directory Configuration".bold());
    let root_dir = prompt_input("Root directory for email storage", "~/notes/email")?;
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

    // -- Build config TOML
    let mut toml_content = String::new();
    toml_content.push_str("[email]\n");
    toml_content.push_str("font_family = \"Helvetica, Arial, sans-serif\"\n");
    toml_content.push_str("font_size = \"12pt\"\n");
    toml_content.push_str("include_signature = true\n\n");
    toml_content.push_str(&build_graph_account_toml(
        &account_name, &default_from,
        &oauth2_client_id, &oauth2_tenant_id,
        &root_dir, &drafts_dir,
        &inbox_server, &inbox_local,
        &archive_server, &archive_local,
        &sent_server, &sent_local,
        &extra_mailboxes,
    ));

    // Write config file
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, toml_content)?;

    println!();
    println!(
        "{} Config written to {}",
        "\u{2713}".green().bold(),
        path.display()
    );
    println!(
        "{} OAuth2 token cached at ~/.email-cli/tokens/",
        "\u{2713}".green().bold()
    );

    Ok(())
}

/// Graph API flow for `config add-account` (appends to existing config).
fn graph_add_account_flow(path: &std::path::Path, existing_names: &[&str]) -> Result<()> {
    println!(
        "{} Microsoft 365 (Graph API) -- no IMAP/SMTP needed.",
        "\u{2139}".blue()
    );
    println!("  Requires an Azure Entra ID app with Mail.Read, Mail.ReadWrite, Mail.Send permissions.");
    println!("  See docs/exchange-setup.md for details.");
    println!();

    // -- Account name (unique)
    let account_name = loop {
        let name = prompt_input("Account name (unique slug)", "exchange")?;
        if existing_names.contains(&name.as_str()) {
            println!("{} Account '{}' already exists. Choose a different name.", "\u{26a0}".yellow(), name);
        } else {
            break name;
        }
    };
    println!();

    // -- Email address
    let default_from = prompt_input("Email address", "")?;
    println!();

    // -- OAuth2 settings
    println!("{}", "OAuth2 Configuration".bold());
    let oauth2_client_id = prompt_input("Azure App client_id", "")?;
    let oauth2_tenant_id = prompt_input("Azure tenant_id", "")?;
    println!();

    // -- Device code flow
    println!("{} Running OAuth2 device code flow (Graph API)...", "\u{2139}".blue());
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::oauth2::device_code_flow(
        &oauth2_client_id,
        &oauth2_tenant_id,
        &account_name,
        crate::oauth2::GRAPH_SCOPES,
    ))?;
    println!("{} OAuth2 token acquired and cached", "\u{2713}".green());
    println!();

    // -- Test Graph API connection
    print!("  Testing Graph API connection... ");
    io::stdout().flush()?;
    let graph_config = crate::config::GraphConfig {
        client_id: oauth2_client_id.clone(),
        tenant_id: oauth2_tenant_id.clone(),
        username: default_from.clone(),
        account_name: account_name.clone(),
    };
    match rt.block_on(crate::graph::GraphClient::new_async(&graph_config)) {
        Ok(_client) => println!("{}", "OK".green()),
        Err(e) => {
            println!("{}", "FAILED".red());
            eprintln!("  {} Graph API test failed: {}", "\u{26a0}".yellow(), e);
            eprintln!("  Continuing with setup (you can re-test later with `email config oauth2-login`).");
        }
    }
    println!();

    // -- Folder discovery via Graph API
    println!("{}", "Mailbox Configuration".bold());
    print!("  Fetching folder list from Graph API... ");
    io::stdout().flush()?;

    let folder_names: Vec<String> = match rt.block_on(async {
        let client = crate::graph::GraphClient::new_async(&graph_config).await?;
        client.list_folders().await
    }) {
        Ok(folders) => {
            println!("{} ({} folders)", "OK".green(), folders.len());
            folders.iter().map(|f| f.display_name.clone()).collect()
        }
        Err(e) => {
            println!("{}", "FAILED".red());
            eprintln!("  Error: {}", e);
            eprintln!("  Continuing with manual mailbox configuration.");
            Vec::new()
        }
    };

    let (inbox_server, archive_server, sent_server) = if !folder_names.is_empty() {
        let mut available = folder_names.clone();
        let inbox = select_mailbox("Which folder is your Inbox?", &available, &["Inbox"])?;
        available.retain(|m| m != &inbox);
        let archive = select_mailbox("Which folder is your Archive?", &available, &["Archive"])?;
        available.retain(|m| m != &archive);
        let sent = select_mailbox("Which folder is your Sent folder?", &available, &["Sent Items"])?;
        available.retain(|m| m != &sent);
        (inbox, archive, sent)
    } else {
        let inbox = prompt_input("Inbox folder name", "Inbox")?;
        let archive = prompt_input("Archive folder name", "Archive")?;
        let sent = prompt_input("Sent folder name", "Sent Items")?;
        (inbox, archive, sent)
    };

    let extra_mailboxes: Vec<String> = if !folder_names.is_empty() {
        let remaining: Vec<&str> = folder_names
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
                .with_prompt("Select additional folders to sync (optional)")
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

    // -- Directories
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

    // -- Append to config file
    let block = format!("\n{}", build_graph_account_toml(
        &account_name, &default_from,
        &oauth2_client_id, &oauth2_tenant_id,
        &root_dir, &drafts_dir,
        &inbox_server, &inbox_local,
        &archive_server, &archive_local,
        &sent_server, &sent_local,
        &extra_mailboxes,
    ));

    let mut content = fs::read_to_string(path)?;
    content.push_str(&block);
    fs::write(path, content)?;

    println!();
    println!(
        "{} Account '{}' added to {}",
        "\u{2713}".green().bold(),
        account_name,
        path.display()
    );
    println!(
        "{} OAuth2 token cached at ~/.email-cli/tokens/",
        "\u{2713}".green().bold()
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// TOML builders (pure functions, testable)
// ---------------------------------------------------------------------------

/// Build the full config TOML for `config init`.
/// `oauth2` is Some((client_id, tenant_id)) for OAuth2 accounts.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_init_toml(
    account_name: &str, default_from: &str,
    smtp_host: &str, smtp_port: u16, smtp_username: &str, accept_invalid_certs: bool,
    imap_host_input: &str, imap_port: u16, imap_username_input: &str,
    root_dir: &str, drafts_dir: &str,
    inbox_server: &str, inbox_local: &str,
    archive_server: &str, archive_local: &str,
    sent_server: &str, sent_local: &str,
    extra_mailboxes: &[String],
    existing_sigs: Option<&AccountConfig>,
    oauth2: Option<(&str, &str)>,
) -> String {
    let mut out = String::new();
    out.push_str("[email]\n");
    out.push_str("font_family = \"Helvetica, Arial, sans-serif\"\n");
    out.push_str("font_size = \"12pt\"\n");
    out.push_str("include_signature = true\n");

    out.push_str("\n[[accounts]]\n");
    out.push_str(&format!("name = \"{}\"\n", account_name));
    out.push_str(&format!("default_from = \"{}\"\n", default_from));

    if let Some((client_id, tenant_id)) = oauth2 {
        out.push_str("auth_method = \"oauth2\"\n");
        out.push_str("\n[accounts.oauth2]\n");
        out.push_str(&format!("client_id = \"{}\"\n", client_id));
        out.push_str(&format!("tenant_id = \"{}\"\n", tenant_id));
    }

    out.push_str("\n[accounts.smtp]\n");
    out.push_str(&format!("host = \"{}\"\n", smtp_host));
    out.push_str(&format!("port = {}\n", smtp_port));
    out.push_str(&format!("username = \"{}\"\n", smtp_username));
    if accept_invalid_certs {
        out.push_str("accept_invalid_certs = true\n");
    }

    out.push_str("\n[accounts.imap]\n");
    if !imap_host_input.is_empty() {
        out.push_str(&format!("host = \"{}\"\n", imap_host_input));
    }
    out.push_str(&format!("port = {}\n", imap_port));
    if !imap_username_input.is_empty() {
        out.push_str(&format!("username = \"{}\"\n", imap_username_input));
    }
    if accept_invalid_certs {
        out.push_str("accept_invalid_certs = true\n");
    }

    out.push_str("\n[accounts.directories]\n");
    out.push_str(&format!("root = \"{}\"\n", root_dir));
    out.push_str(&format!("drafts = \"{}\"\n", drafts_dir));

    out.push_str("\n[accounts.mailboxes.inbox]\n");
    out.push_str(&format!("server = \"{}\"\n", inbox_server));
    out.push_str(&format!("local = \"{}\"\n", inbox_local));

    out.push_str("\n[accounts.mailboxes.archive]\n");
    out.push_str(&format!("server = \"{}\"\n", archive_server));
    out.push_str(&format!("local = \"{}\"\n", archive_local));

    out.push_str("\n[accounts.mailboxes.sent]\n");
    out.push_str(&format!("server = \"{}\"\n", sent_server));
    out.push_str(&format!("local = \"{}\"\n", sent_local));

    for mb in extra_mailboxes {
        out.push_str("\n[[accounts.mailboxes.extra]]\n");
        out.push_str(&format!("server = \"{}\"\n", mb));
        out.push_str(&format!("local = \"{}\"\n", slugify_mailbox_name(mb)));
    }

    // Preserve signature settings from existing config
    if let Some(existing_acct) = existing_sigs {
        if !existing_acct.signatures.entries.is_empty() || existing_acct.signatures.default.is_some() {
            out.push_str("\n[accounts.signatures]\n");
            if let Some(ref default) = existing_acct.signatures.default {
                out.push_str(&format!("default = \"{}\"\n", default));
            }
            for (key, entry) in &existing_acct.signatures.entries {
                out.push_str(&format!("\n[accounts.signatures.{}]\n", key));
                if let Some(ref name) = entry.name {
                    out.push_str(&format!("name = \"{}\"\n", name));
                }
                out.push_str(&format!("path = \"{}\"\n", entry.path));
            }
        }
    }

    out
}

/// Build an account TOML block for `config add-account` (appended to existing config).
/// `oauth2` is Some((client_id, tenant_id)) for OAuth2 accounts.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_add_account_toml(
    account_name: &str, default_from: &str,
    smtp_host: &str, smtp_port: u16, smtp_username: &str, accept_invalid_certs: bool,
    imap_host_input: &str, imap_port: u16, imap_username_input: &str,
    root_dir: &str, drafts_dir: &str,
    inbox_server: &str, inbox_local: &str,
    archive_server: &str, archive_local: &str,
    sent_server: &str, sent_local: &str,
    extra_mailboxes: &[String],
    oauth2: Option<(&str, &str)>,
) -> String {
    let mut block = String::from("\n");
    block.push_str("[[accounts]]\n");
    block.push_str(&format!("name = \"{}\"\n", account_name));
    block.push_str(&format!("default_from = \"{}\"\n", default_from));

    if let Some((client_id, tenant_id)) = oauth2 {
        block.push_str("auth_method = \"oauth2\"\n");
        block.push_str("\n[accounts.oauth2]\n");
        block.push_str(&format!("client_id = \"{}\"\n", client_id));
        block.push_str(&format!("tenant_id = \"{}\"\n", tenant_id));
    }

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
    for mb in extra_mailboxes {
        block.push_str("\n[[accounts.mailboxes.extra]]\n");
        block.push_str(&format!("server = \"{}\"\nlocal = \"{}\"\n", mb, slugify_mailbox_name(mb)));
    }

    block
}

/// Build a Graph API account TOML block (no SMTP/IMAP sections).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_graph_account_toml(
    account_name: &str, default_from: &str,
    client_id: &str, tenant_id: &str,
    root_dir: &str, drafts_dir: &str,
    inbox_server: &str, inbox_local: &str,
    archive_server: &str, archive_local: &str,
    sent_server: &str, sent_local: &str,
    extra_mailboxes: &[String],
) -> String {
    let mut out = String::new();
    out.push_str("[[accounts]]\n");
    out.push_str(&format!("name = \"{}\"\n", account_name));
    out.push_str(&format!("default_from = \"{}\"\n", default_from));
    out.push_str("auth_method = \"graph\"\n");

    out.push_str("\n[accounts.oauth2]\n");
    out.push_str(&format!("client_id = \"{}\"\n", client_id));
    out.push_str(&format!("tenant_id = \"{}\"\n", tenant_id));

    out.push_str("\n[accounts.directories]\n");
    out.push_str(&format!("root = \"{}\"\n", root_dir));
    out.push_str(&format!("drafts = \"{}\"\n", drafts_dir));

    out.push_str("\n[accounts.mailboxes.inbox]\n");
    out.push_str(&format!("server = \"{}\"\n", inbox_server));
    out.push_str(&format!("local = \"{}\"\n", inbox_local));

    out.push_str("\n[accounts.mailboxes.archive]\n");
    out.push_str(&format!("server = \"{}\"\n", archive_server));
    out.push_str(&format!("local = \"{}\"\n", archive_local));

    out.push_str("\n[accounts.mailboxes.sent]\n");
    out.push_str(&format!("server = \"{}\"\n", sent_server));
    out.push_str(&format!("local = \"{}\"\n", sent_local));

    for mb in extra_mailboxes {
        out.push_str("\n[[accounts.mailboxes.extra]]\n");
        out.push_str(&format!("server = \"{}\"\n", mb));
        out.push_str(&format!("local = \"{}\"\n", slugify_mailbox_name(mb)));
    }

    out
}
