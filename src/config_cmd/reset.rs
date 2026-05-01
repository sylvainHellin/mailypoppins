use anyhow::{Context, Result};
use colored::*;
use std::fs;
use std::io::{self, Write};

use crate::config::{load_global_config, set_secret, AuthMethod};
use crate::secrets::secrets_path;

/// Wipe the encrypted secrets file and OAuth2 token caches, then walk
/// configured accounts and re-prompt for credentials.
///
/// This is the recovery path when the secrets file cannot be decrypted on the
/// current machine (e.g. after a Time Machine restore to a new Mac, or when
/// `machine-uid` returns a new value for any reason).
pub fn cmd_reset_secrets() -> Result<()> {
    let secrets_file = secrets_path();
    let token_dir = std::env::var("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".mailypoppins").join("tokens"))
        .unwrap_or_else(|_| std::path::PathBuf::from(".mailypoppins/tokens"));

    println!("{}", "=== Reset Secrets ===".bold().cyan());
    println!();
    println!("This will:");
    println!("  - Delete {}", secrets_file.display());
    println!("  - Delete {}/*.enc", token_dir.display());
    println!("  - Prompt you to re-enter SMTP/IMAP passwords for each account");
    println!("  - For OAuth2/Graph accounts, you will need to re-run `email config oauth2-login`");
    println!();
    print!("Continue? [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Cancelled.");
        return Ok(());
    }

    // 1. Wipe files.
    if secrets_file.exists() {
        fs::remove_file(&secrets_file)
            .with_context(|| format!("Failed to delete {}", secrets_file.display()))?;
        println!("{} Removed {}", "\u{2713}".green(), secrets_file.display());
    }
    if token_dir.exists() {
        for entry in fs::read_dir(&token_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("enc") {
                fs::remove_file(&path)?;
                println!("{} Removed {}", "\u{2713}".green(), path.display());
            }
        }
    }

    // 2. Re-init the secrets backend so subsequent `set_secret` calls open
    //    a fresh empty file with the current machine key.
    let config = load_global_config().context("Failed to load config")?;
    let _ = crate::config::init_secrets_backend(&config);

    // 3. Walk accounts and prompt.
    if config.accounts.is_empty() {
        println!();
        println!(
            "{} No accounts configured. Run `email config init` to create one.",
            "\u{26a0}".yellow()
        );
        return Ok(());
    }

    println!();
    for account in &config.accounts {
        match account.auth_method {
            AuthMethod::Password => {
                println!(
                    "{} Account '{}' (Password auth)",
                    "\u{25b6}".cyan(),
                    account.name.bold()
                );
                let smtp_pw = dialoguer::Password::new()
                    .with_prompt(format!("  SMTP password for '{}'", account.name))
                    .interact()
                    .context("SMTP password input cancelled")?;
                set_secret(&format!("smtp-password-{}", account.name), &smtp_pw)?;
                println!("    {} SMTP password stored", "\u{2713}".green());

                print!("  Use a separate IMAP password? [y/N] ");
                io::stdout().flush()?;
                let mut sep = String::new();
                io::stdin().read_line(&mut sep)?;
                if matches!(sep.trim().to_lowercase().as_str(), "y" | "yes") {
                    let imap_pw = dialoguer::Password::new()
                        .with_prompt(format!("  IMAP password for '{}'", account.name))
                        .interact()
                        .context("IMAP password input cancelled")?;
                    set_secret(&format!("imap-password-{}", account.name), &imap_pw)?;
                    println!("    {} IMAP password stored", "\u{2713}".green());
                }
            }
            AuthMethod::OAuth2 | AuthMethod::Graph => {
                println!(
                    "{} Account '{}' ({:?} auth) -- run `email config oauth2-login --account {}` to re-acquire token",
                    "\u{2139}".blue(),
                    account.name.bold(),
                    account.auth_method,
                    account.name
                );
            }
        }
    }

    println!();
    println!("{} Secrets reset complete.", "\u{2713}".green().bold());
    Ok(())
}
