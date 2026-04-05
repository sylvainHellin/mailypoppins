use anyhow::{Context, Result};
use colored::*;

use crate::config::set_keyring_password;

/// Store a password in the OS keyring.
pub fn cmd_set_password(which: &str, account_name: &str) -> Result<()> {
    match which {
        "smtp" | "imap" => {},
        _ => {
            eprintln!(
                "{} Unknown password type '{}'. Use 'smtp' or 'imap'.",
                "\u{2717}".red(),
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
        "\u{2713}".green(),
        which.to_uppercase(),
        account_name
    );
    Ok(())
}
