//! `mp config set-password`, as far as the client still owns it: the refusal,
//! the prompt and the line a stored password prints. The write itself is
//! `config.set_password`'s since P4-U14.

use anyhow::{Context, Result};
use colored::*;

/// Refuse a credential name the product does not have, in the words and with
/// the exit code `mp config set-password` has always used.
pub fn check_kind(which: &str) {
    match which {
        "smtp" | "imap" => {}
        _ => {
            eprintln!(
                "{} Unknown password type '{}'. Use 'smtp' or 'imap'.",
                "\u{2717}".red(),
                which
            );
            std::process::exit(1);
        }
    }
}

/// Read one password from this process's terminal.
///
/// Client-side and nowhere else (P4-U14): there is no `MAILYPOPPINS_PASSWORD`
/// environment variable and this slice does not invent one, because a secret in
/// an environment variable is a secret in `/proc`. The value crosses the socket
/// once, as `config.set_password`'s parameter.
pub fn prompt(which: &str, account_name: &str) -> Result<String> {
    dialoguer::Password::new()
        .with_prompt(format!(
            "Enter {} password for account '{}'",
            which, account_name
        ))
        .interact()
        .context("Password input cancelled")
}

/// The line a stored password prints.
pub fn stored_line(which: &str, account_name: &str) -> String {
    format!(
        "{} {} password for '{}' stored",
        "\u{2713}".green(),
        which.to_uppercase(),
        account_name
    )
}
