//! `mp config set-password`, as far as the client still owns it: the refusal,
//! the prompt or the piped line, and the line a stored password prints. The
//! write itself is `config.set_password`'s since P4-U14.

use std::io::{BufRead, IsTerminal};

use anyhow::{bail, Context, Result};
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

/// Read one password from this process's terminal: the `which` password,
/// or with none the one password of both logins (PERSO-297).
///
/// Client-side and nowhere else (P4-U14): there is no `MAILYPOPPINS_PASSWORD`
/// environment variable and this slice does not invent one, because a secret in
/// an environment variable is a secret in `/proc`. The value crosses the socket
/// once, as `config.set_password`'s parameter.
pub fn prompt(which: Option<&str>, account_name: &str) -> Result<String> {
    dialoguer::Password::new()
        .with_prompt(prompt_text(which, account_name))
        .interact()
        .context("Password input cancelled")
}

/// What the password prompt asks.
pub fn prompt_text(which: Option<&str>, account_name: &str) -> String {
    match which {
        Some(which) => format!("Enter {which} password for account '{account_name}'"),
        None => format!("Enter the password for account '{account_name}' (IMAP and SMTP)"),
    }
}

/// Read one password from this process's standard input, for `--stdin`
/// (PERSO-281): a script or a password manager pipes it in.
pub fn from_stdin() -> Result<String> {
    let stdin = std::io::stdin();
    read_piped(stdin.is_terminal(), stdin.lock())
}

/// The password a pipe carries: its first line, the line ending removed and
/// nothing else trimmed, so a password with a leading or trailing space
/// survives.
///
/// A terminal on stdin is refused before anything is read, since a password
/// typed there would echo; the prompt is the way to type one. An empty line,
/// or no line at all, is refused rather than stored. No error carries the
/// value.
pub fn read_piped(stdin_is_terminal: bool, mut input: impl BufRead) -> Result<String> {
    if stdin_is_terminal {
        bail!(
            "--stdin reads the password from a pipe, and standard input is a terminal; \
             leave out --stdin to be prompted"
        );
    }
    let mut line = String::new();
    input
        .read_line(&mut line)
        .context("Could not read the password from standard input")?;
    let password = line
        .strip_suffix('\n')
        .map(|rest| rest.strip_suffix('\r').unwrap_or(rest))
        .unwrap_or(&line);
    if password.is_empty() {
        bail!("The password on standard input is empty; nothing was stored");
    }
    Ok(password.to_string())
}

/// The line a stored password prints: the `which` password, or with none
/// the one password stored as both.
pub fn stored_line(which: Option<&str>, account_name: &str) -> String {
    let which = match which {
        Some(which) => which.to_uppercase(),
        None => "IMAP and SMTP".to_string(),
    };
    format!(
        "{} {which} password for '{account_name}' stored",
        "\u{2713}".green(),
    )
}
