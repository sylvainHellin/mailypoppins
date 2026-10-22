//! `mp config init` and `mp config add-account`: the prompts are this
//! process's, every engine step is the daemon's.
//!
//! The wizards ask the user to describe an account, dial its servers to test
//! the answers, offer the server's mailboxes as the next prompt's choices, sign
//! an OAuth2 account in, write `config.toml` and store the passwords. Since the
//! daemon leftovers of 2026-10-22 each of those steps is one call on a
//! [`WizardDaemon`]: `config.probe` for a connection test or a mailbox list,
//! `config.oauth2_login` with the `candidate` block for a sign-in,
//! `config.init` or `config.add_account` for the file, and
//! `config.set_password` for each password. The desktop wizard makes the same
//! calls in the same order, so both clients describe an account with one
//! `[[accounts]]` block, the one [`imap_candidate`] and [`graph_candidate`]
//! build.
//!
//! Nothing is written until the user has answered every prompt, as before:
//! a password the probe accepted is held here and stored once the account it
//! belongs to exists, since `config.set_password` refuses an account the
//! configuration does not name.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use colored::*;
use serde_json::{json, Value};

use crate::config::{
    account_dir, blobs_dir, drafts_dir, mailypoppins_data_dir, store_path, FolderPolicy,
};

/// What a `config.probe` answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    /// Whether the server accepted the session.
    pub ok: bool,
    /// Why it did not, as the server or the transport said it.
    pub error: Option<String>,
    /// The mailbox or folder names a `mailboxes` probe listed.
    pub mailboxes: Vec<String>,
}

impl Probe {
    /// A `config.probe` answer off the wire.
    pub fn from_answer(answer: &Value) -> Probe {
        Probe {
            ok: answer["ok"].as_bool().unwrap_or(false),
            error: answer["error"].as_str().map(str::to_string),
            mailboxes: answer["mailboxes"]
                .as_array()
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|n| n.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// The daemon calls a wizard makes, so a test can run the sequence against a
/// recording double rather than a daemon with a mail server behind it.
#[allow(async_fn_in_trait)]
pub trait WizardDaemon {
    /// `config.probe {account, check, password?}`.
    async fn probe(
        &mut self,
        candidate: &Value,
        check: &str,
        password: Option<&str>,
    ) -> Result<Probe>;
    /// `config.oauth2_login {account, candidate}`, rendering the device code
    /// as it arrives; returns once the token is cached.
    async fn sign_in(&mut self, candidate: &Value) -> Result<()>;
    /// `config.init {account, replace, theme, notifications}`; answers the
    /// file it wrote.
    async fn init(&mut self, candidate: &Value, replace: bool) -> Result<PathBuf>;
    /// `config.add_account {account}`.
    async fn add_account(&mut self, candidate: &Value) -> Result<()>;
    /// `config.set_password {account, kind, value}`.
    async fn set_password(&mut self, account: &str, kind: &str, value: &str) -> Result<()>;
}

/// Which file the wizard ends in.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    /// `mp config init`: a whole file, over an existing one when `replace`.
    Init { replace: bool },
    /// `mp config add-account`: one more block, under a name not taken.
    Add { existing: Vec<String> },
}

/// The four presets both wizards offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Preset {
    Standard,
    Proton,
    Exchange,
    Graph,
}

/// The answers of an IMAP/SMTP account, the shape [`imap_candidate`] writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImapAnswers {
    pub name: String,
    pub default_from: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    /// The IMAP host as typed; empty falls back to the SMTP host.
    pub imap_host: String,
    pub imap_port: u16,
    /// The IMAP username as typed; empty falls back to the SMTP username.
    pub imap_username: String,
    pub accept_invalid_certs: bool,
    /// `Some((client_id, tenant_id))` for an OAuth2 (Exchange) account.
    pub oauth2: Option<(String, String)>,
    pub mailboxes: Mailboxes,
}

/// The mailbox mapping both kinds of account end with.
///
/// No `extra` list: since #0141 the daemon syncs every folder the server
/// lists, so a wizard pins none. `drafts`, `trash` and `junk` name a role
/// folder the server does not mark itself, and `overrides` change the policy
/// the daemon would give a folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mailboxes {
    pub inbox: String,
    pub archive: String,
    pub sent: String,
    pub drafts: Option<String>,
    pub trash: Option<String>,
    pub junk: Option<String>,
    /// `(match, policy)`, first match wins.
    pub overrides: Vec<(String, FolderPolicy)>,
}

/// The `[[accounts]]` block of an IMAP/SMTP account, as `config.init`,
/// `config.add_account` and `config.probe` take it.
///
/// An empty IMAP host or username is left out, so the account falls back to
/// the SMTP one as it always did, and so does an empty mailbox mapping, which
/// a probe made before the mailboxes were chosen does not have yet.
pub fn imap_candidate(a: &ImapAnswers) -> Value {
    let mut smtp = json!({
        "host": a.smtp_host,
        "port": a.smtp_port,
        "username": a.smtp_username,
    });
    let mut imap = json!({"port": a.imap_port});
    if !a.imap_host.is_empty() {
        imap["host"] = json!(a.imap_host);
    }
    if !a.imap_username.is_empty() {
        imap["username"] = json!(a.imap_username);
    }
    if a.accept_invalid_certs {
        smtp["accept_invalid_certs"] = json!(true);
        imap["accept_invalid_certs"] = json!(true);
    }
    let mut candidate = json!({
        "name": a.name,
        "default_from": a.default_from,
        "smtp": smtp,
        "imap": imap,
    });
    if let Some((client_id, tenant_id)) = &a.oauth2 {
        candidate["auth_method"] = json!("oauth2");
        candidate["oauth2"] = json!({"client_id": client_id, "tenant_id": tenant_id});
    }
    with_mailboxes(candidate, &a.mailboxes)
}

/// The `[[accounts]]` block of a Microsoft Graph account: no SMTP or IMAP
/// table, since Graph is its transport.
pub fn graph_candidate(
    name: &str,
    default_from: &str,
    client_id: &str,
    tenant_id: &str,
    mailboxes: &Mailboxes,
) -> Value {
    with_mailboxes(
        json!({
            "name": name,
            "default_from": default_from,
            "auth_method": "graph",
            "oauth2": {"client_id": client_id, "tenant_id": tenant_id},
        }),
        mailboxes,
    )
}

/// The mailbox mapping on a candidate, once one was chosen.
fn with_mailboxes(mut candidate: Value, m: &Mailboxes) -> Value {
    if !m.inbox.is_empty() {
        let mut mailboxes = json!({
            "inbox": m.inbox,
            "archive": m.archive,
            "sent": m.sent,
        });
        for (role, server) in [
            ("drafts", &m.drafts),
            ("trash", &m.trash),
            ("junk", &m.junk),
        ] {
            if let Some(server) = server {
                mailboxes[role] = json!(server);
            }
        }
        if !m.overrides.is_empty() {
            mailboxes["overrides"] = m
                .overrides
                .iter()
                .map(|(pattern, policy)| json!({"match": pattern, "policy": policy.as_str()}))
                .collect();
        }
        candidate["mailboxes"] = mailboxes;
    }
    candidate
}

/// Print where the account's data lives.
///
/// The daemon creates the account directory when it starts the account's
/// runtime, which `config.init` and `config.add_account` do before they
/// answer; the store and its blobs are made by the first sync and the drafts
/// directory by the first `mp new`, and each line says so.
pub(super) fn print_account_data_paths(account_name: &str) {
    println!("{}", "Local data for this account:".bold());
    println!("  Directory: {}", account_dir(account_name).display());
    println!(
        "  Store:     {} (message store, created by the first sync)",
        store_path(account_name).display()
    );
    println!(
        "  Blobs:     {} (bodies and attachments, created by the first sync)",
        blobs_dir(account_name).display()
    );
    println!(
        "  Drafts:    {} (Markdown files, created by `mp new`)",
        drafts_dir(account_name).display()
    );
}

/// `mp config init`.
///
/// `path` and `exists` come from `config.get`: whether a configuration is
/// there and where it lives are the daemon's facts, and asking before the
/// first prompt is what lets the declined branch satisfy
/// `MAILYPOPPINS_DAEMON_REQUIRE`.
pub async fn cmd_config_init(
    daemon: &mut impl WizardDaemon,
    path: &Path,
    exists: bool,
) -> Result<()> {
    if exists {
        print!(
            "{} Config file already exists at {}. Overwrite? [y/N] ",
            "\u{26a0}".yellow(),
            path.display()
        );
        io::stdout().flush()?;
        if !yes_no(false)? {
            println!("Cancelled.");
            return Ok(());
        }
    }
    println!("{}", "=== mailypoppins setup ===".bold().cyan());
    println!();
    run(daemon, Mode::Init { replace: exists }, path).await
}

/// `mp config add-account`.
///
/// `path`, `exists` and `existing_names` come from `config.get`, for the
/// reason [`cmd_config_init`] states.
pub async fn cmd_config_add_account(
    daemon: &mut impl WizardDaemon,
    path: &Path,
    exists: bool,
    existing_names: &[String],
) -> Result<()> {
    if !exists {
        return Err(anyhow!(
            "Config file not found at {}. Run `mp config init` first.",
            path.display()
        ));
    }
    println!(
        "{} Existing accounts: {}",
        "\u{2139}".blue(),
        if existing_names.is_empty() {
            "(none)".to_string()
        } else {
            existing_names.join(", ")
        }
    );
    println!();
    let mode = Mode::Add {
        existing: existing_names.to_vec(),
    };
    run(daemon, mode, path).await
}

/// The provider prompt, then the flow for it.
async fn run(daemon: &mut impl WizardDaemon, mode: Mode, path: &Path) -> Result<()> {
    println!("Select email provider:");
    println!("  1. Standard IMAP/SMTP (manual config)");
    println!("  2. Proton Mail (via Proton Bridge)");
    println!("  3. Microsoft 365 / Exchange Online (OAuth2 IMAP/SMTP)");
    println!("  4. Microsoft 365 / Exchange Online (Graph API)");
    let preset = match prompt_input("Choice", "1")?.trim() {
        "2" => Preset::Proton,
        "3" => Preset::Exchange,
        "4" => Preset::Graph,
        _ => Preset::Standard,
    };
    println!();
    match preset {
        Preset::Proton => {
            println!(
                "{} Proton Mail Bridge must be running. Use the bridge password (not your Proton password).",
                "\u{2139}".blue()
            );
            println!();
        }
        Preset::Exchange => {
            println!(
                "{} Microsoft 365 requires an Azure Entra ID app registration.",
                "\u{2139}".blue()
            );
            println!("  See docs/exchange-setup.md for step-by-step instructions.");
            println!();
        }
        Preset::Graph => return graph_flow(daemon, &mode, path).await,
        Preset::Standard => {}
    }
    imap_flow(daemon, &mode, preset, path).await
}

/// The IMAP/SMTP flow: Standard, Proton Bridge and Exchange over OAuth2.
async fn imap_flow(
    daemon: &mut impl WizardDaemon,
    mode: &Mode,
    preset: Preset,
    path: &Path,
) -> Result<()> {
    let (smtp_host, smtp_port, imap_host, imap_port) = match preset {
        Preset::Proton => ("127.0.0.1", "1025", "127.0.0.1", "1143"),
        Preset::Exchange => ("smtp.office365.com", "587", "outlook.office365.com", "993"),
        _ => ("smtp.example.com", "465", "", "993"),
    };
    let default_name = match (preset, mode) {
        (Preset::Proton, _) => "proton",
        (Preset::Exchange, _) => "exchange",
        (_, Mode::Init { .. }) => "main",
        (_, Mode::Add { .. }) => "work",
    };
    let mut a = ImapAnswers {
        name: account_name(mode, default_name)?,
        accept_invalid_certs: preset == Preset::Proton,
        ..ImapAnswers::default()
    };
    println!();

    println!("{}", "SMTP Configuration".bold());
    a.smtp_host = prompt_input("SMTP host", smtp_host)?;
    a.smtp_port = prompt_input("SMTP port", smtp_port)?.parse().unwrap_or(465);
    a.smtp_username = prompt_input("SMTP username (email)", "")?;
    a.default_from = prompt_input("Default from address", &a.smtp_username)?;

    // Each password the probes accepted, stored once the account exists.
    let mut passwords: Vec<(&str, String)> = Vec::new();
    // What a mailbox probe authenticates with: the IMAP password for a
    // password account, the cached token for an OAuth2 one.
    let imap_password: Option<String>;

    if preset == Preset::Exchange {
        println!();
        println!("{}", "OAuth2 Configuration".bold());
        a.oauth2 = Some((
            prompt_input("Azure App client_id", "")?,
            prompt_input("Azure tenant_id", "")?,
        ));
        println!();
        println!("{}", "IMAP Configuration".bold());
        a.imap_host = prompt_input("IMAP host", imap_host)?;
        a.imap_port = prompt_input("IMAP port", imap_port)?.parse().unwrap_or(993);
        a.imap_username = prompt_input("IMAP username (leave empty to use SMTP username)", "")?;
        println!();
        println!("{} Running OAuth2 device code flow...", "\u{2139}".blue());
        daemon.sign_in(&imap_candidate(&a)).await?;
        println!("{} OAuth2 token acquired and cached", "\u{2713}".green());
        imap_password = None;
    } else {
        let smtp_password = tested_password(daemon, &a, "smtp", "SMTP password").await?;
        passwords.push(("smtp", smtp_password.clone()));
        println!();

        println!("{}", "IMAP Configuration".bold());
        a.imap_host = prompt_input("IMAP host (leave empty to use SMTP host)", imap_host)?;
        a.imap_port = prompt_input("IMAP port", imap_port)?.parse().unwrap_or(993);
        a.imap_username = prompt_input("IMAP username (leave empty to use SMTP username)", "")?;

        print!("Use same password as SMTP? [Y/n] ");
        io::stdout().flush()?;
        if yes_no(true)? {
            print!("  Testing IMAP connection... ");
            io::stdout().flush()?;
            let probe = daemon
                .probe(&imap_candidate(&a), "imap", Some(&smtp_password))
                .await?;
            if probe.ok {
                println!("{}", "OK".green());
            } else {
                println!("{}", "FAILED".red());
                eprintln!(
                    "  Error: {}",
                    probe.error.unwrap_or_else(|| "no reason given".into())
                );
                eprintln!(
                    "  {} SMTP password did not work for IMAP. You may need to set it separately later.",
                    "\u{26a0}".yellow()
                );
            }
            imap_password = Some(smtp_password);
        } else {
            let password = tested_password(daemon, &a, "imap", "IMAP password").await?;
            passwords.push(("imap", password.clone()));
            imap_password = Some(password);
        }
    }
    println!();

    a.mailboxes = choose_mailboxes(
        daemon,
        &imap_candidate(&a),
        imap_password.as_deref(),
        Labels::imap(mode),
    )
    .await?;
    println!();

    let candidate = imap_candidate(&a);
    let path = write(daemon, mode, &candidate, path).await?;
    store_passwords(daemon, &a.name, &passwords).await?;
    println!();
    print_account_data_paths(&a.name);
    println!();
    finished(mode, &a.name, &path);
    if a.oauth2.is_some() {
        println!(
            "{} OAuth2 token cached at {}/<account>.enc",
            "\u{2713}".green().bold(),
            crate::config::tokens_dir().display()
        );
    } else if !passwords.is_empty() {
        println!(
            "{} Passwords stored in the daemon's secrets backend",
            "\u{2713}".green().bold()
        );
    }
    Ok(())
}

/// Store each password the probes accepted, now that the account exists.
///
/// The file is written by then, so a password the daemon fails to store
/// leaves an account that is configured and cannot log in. Each failure is
/// named with the command that sets it, every password is attempted, and the
/// wizard ends in an error rather than in its success lines.
pub(super) async fn store_passwords(
    daemon: &mut impl WizardDaemon,
    account: &str,
    passwords: &[(&str, String)],
) -> Result<()> {
    let mut missing: Vec<&str> = Vec::new();
    for (kind, value) in passwords {
        match daemon.set_password(account, kind, value).await {
            Ok(()) => println!(
                "{} {} password stored",
                "\u{2713}".green(),
                kind.to_uppercase()
            ),
            Err(e) => {
                eprintln!(
                    "{} The {} password of account '{account}' was not stored: {e:#}",
                    "\u{2717}".red(),
                    kind.to_uppercase()
                );
                missing.push(kind);
            }
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(anyhow!("{}", missing_passwords_message(account, &missing)))
}

/// What the user reads when the account was written and some of its
/// passwords were not: which ones, and the command that sets each.
pub(super) fn missing_passwords_message(account: &str, missing: &[&str]) -> String {
    let kinds = missing
        .iter()
        .map(|kind| kind.to_uppercase())
        .collect::<Vec<_>>()
        .join(" and ");
    let (noun, verb, pronoun) = if missing.len() == 1 {
        ("password", "is", "it")
    } else {
        ("passwords", "are", "them")
    };
    let commands: Vec<String> = missing
        .iter()
        .map(|kind| format!("  mp config set-password {kind} --account {account}"))
        .collect();
    format!(
        "Account '{account}' is configured, but its {kinds} {noun} {verb} not stored, \
         so it cannot log in yet. Set {pronoun} with:\n{}",
        commands.join("\n")
    )
}

/// The Graph flow: no SMTP or IMAP, a sign-in, then the folders.
async fn graph_flow(daemon: &mut impl WizardDaemon, mode: &Mode, path: &Path) -> Result<()> {
    println!(
        "{} Microsoft 365 (Graph API): no IMAP/SMTP needed.",
        "\u{2139}".blue()
    );
    println!(
        "  Signs in through the shared mailypoppins app; no app registration of your own needed."
    );
    println!("  See docs/exchange-setup.md for details.");
    println!();

    let name = account_name(mode, "exchange")?;
    println!();
    let default_from = prompt_input("Email address", "")?;
    println!();

    println!("{}", "OAuth2 Configuration".bold());
    println!("  Press Enter at both prompts to sign in with any work, school or personal Microsoft account.");
    let client_id = crate::oauth2::client_id_or_default(&prompt_input(
        "Client ID [press Enter for the mailypoppins app]",
        "",
    )?);
    let tenant_id = crate::oauth2::tenant_or_default(&prompt_input(
        "Tenant ID or domain",
        crate::oauth2::DEFAULT_TENANT,
    )?);
    println!();

    let unmapped = Mailboxes::default();
    let candidate = graph_candidate(&name, &default_from, &client_id, &tenant_id, &unmapped);
    println!(
        "{} Running OAuth2 device code flow (Graph API)...",
        "\u{2139}".blue()
    );
    daemon.sign_in(&candidate).await?;
    println!("{} OAuth2 token acquired and cached", "\u{2713}".green());
    println!();

    print!("  Testing Graph API connection... ");
    io::stdout().flush()?;
    let probe = daemon.probe(&candidate, "graph", None).await?;
    if probe.ok {
        println!("{}", "OK".green());
    } else {
        println!("{}", "FAILED".red());
        eprintln!(
            "  {} Graph API test failed: {}",
            "\u{26a0}".yellow(),
            probe.error.unwrap_or_else(|| "no reason given".into())
        );
        eprintln!("  Continuing with setup (you can re-test later with `mp config oauth2-login`).");
    }
    println!();

    let mailboxes = choose_mailboxes(daemon, &candidate, None, Labels::graph()).await?;
    println!();
    let candidate = graph_candidate(&name, &default_from, &client_id, &tenant_id, &mailboxes);
    let path = write(daemon, mode, &candidate, path).await?;
    println!();
    print_account_data_paths(&name);
    println!();
    finished(mode, &name, &path);
    println!(
        "{} OAuth2 token cached at {}/",
        "\u{2713}".green().bold(),
        crate::config::tokens_dir().display()
    );
    Ok(())
}

/// The account name: any name for `init`, a name not taken for `add-account`.
fn account_name(mode: &Mode, default: &str) -> Result<String> {
    match mode {
        Mode::Init { .. } => {
            prompt_input("Account name (unique slug, e.g. 'tum', 'gmail')", default)
        }
        Mode::Add { existing } => loop {
            let name = prompt_input("Account name (unique slug)", default)?;
            if existing.contains(&name) {
                println!(
                    "{} Account '{}' already exists. Choose a different name.",
                    "\u{26a0}".yellow(),
                    name
                );
            } else {
                return Ok(name);
            }
        },
    }
}

/// Prompt for a password until the server accepts it, or the user gives up.
async fn tested_password(
    daemon: &mut impl WizardDaemon,
    a: &ImapAnswers,
    check: &str,
    prompt: &str,
) -> Result<String> {
    let label = check.to_uppercase();
    loop {
        let password = read_password(prompt)?;
        print!("  Testing {label} connection... ");
        io::stdout().flush()?;
        let probe = daemon
            .probe(&imap_candidate(a), check, Some(&password))
            .await?;
        if probe.ok {
            println!("{}", "OK".green());
            return Ok(password);
        }
        println!("{}", "FAILED".red());
        eprintln!(
            "  Error: {}",
            probe.error.unwrap_or_else(|| "no reason given".into())
        );
        print!("  Retry? [Y/n] ");
        io::stdout().flush()?;
        if !yes_no(true)? {
            return Err(anyhow!("{label} connection test failed. Aborting setup."));
        }
    }
}

/// The words and the defaults of the mailbox prompts.
struct Labels {
    noun: &'static str,
    inbox: (&'static str, &'static [&'static str]),
    archive: (&'static str, &'static [&'static str]),
    sent: (&'static str, &'static [&'static str]),
}

impl Labels {
    /// An IMAP server's mailboxes; `add-account` defaults the archive to
    /// Gmail's `All Mail`, as it always did.
    fn imap(mode: &Mode) -> Labels {
        let archive = match mode {
            Mode::Init { .. } => "Archive",
            Mode::Add { .. } => "All Mail",
        };
        Labels {
            noun: "mailbox",
            inbox: ("INBOX", &["INBOX"]),
            archive: (archive, &["Archive", "archive", "All Mail"]),
            sent: (
                "Sent",
                &["Sent", "Sent Items", "Sent Messages", "INBOX.Sent"],
            ),
        }
    }

    /// A Graph account's folders.
    fn graph() -> Labels {
        Labels {
            noun: "folder",
            inbox: ("Inbox", &["Inbox"]),
            archive: ("Archive", &["Archive"]),
            sent: ("Sent Items", &["Sent Items"]),
        }
    }
}

/// Offer the server's mailboxes for the three roles, then the roles a server
/// usually marks and the policy overrides (#0141), or ask for the names when
/// the server would not list them.
async fn choose_mailboxes(
    daemon: &mut impl WizardDaemon,
    candidate: &Value,
    password: Option<&str>,
    labels: Labels,
) -> Result<Mailboxes> {
    let noun = labels.noun;
    println!("{}", "Mailbox Configuration".bold());
    print!("  Fetching {noun} list from the server... ");
    io::stdout().flush()?;
    let probe = daemon.probe(candidate, "mailboxes", password).await?;
    let listed = if probe.ok {
        println!("{} ({} {noun}s)", "OK".green(), probe.mailboxes.len());
        probe.mailboxes
    } else {
        println!("{}", "FAILED".red());
        eprintln!(
            "  Error: {}",
            probe.error.unwrap_or_else(|| "no reason given".into())
        );
        eprintln!("  Continuing with manual mailbox configuration.");
        Vec::new()
    };

    if listed.is_empty() {
        let inbox = prompt_input(&format!("Inbox {noun} name"), labels.inbox.0)?;
        let archive = prompt_input(&format!("Archive {noun} name"), labels.archive.0)?;
        let sent = prompt_input(&format!("Sent {noun} name"), labels.sent.0)?;
        let (drafts, trash, junk) = marked_roles(noun, &[])?;
        return Ok(Mailboxes {
            inbox,
            archive,
            sent,
            drafts,
            trash,
            junk,
            overrides: ask_overrides(noun)?,
        });
    }

    let mut available = listed.clone();
    let inbox = select_mailbox(
        &format!("Which {noun} is your Inbox?"),
        &available,
        labels.inbox.1,
    )?;
    available.retain(|m| m != &inbox);
    let archive = select_mailbox(
        &format!("Which {noun} is your Archive?"),
        &available,
        labels.archive.1,
    )?;
    available.retain(|m| m != &archive);
    let sent = select_mailbox(
        &format!("Which {noun} is your Sent folder?"),
        &available,
        labels.sent.1,
    )?;
    available.retain(|m| m != &sent);

    let (drafts, trash, junk) = marked_roles(noun, &available)?;
    Ok(Mailboxes {
        inbox,
        archive,
        sent,
        drafts,
        trash,
        junk,
        overrides: ask_overrides(noun)?,
    })
}

/// The drafts, trash and junk folders, each `None` to let the daemon find the
/// one the server marks (#0141). The daemon syncs every other folder the
/// server lists on its own, so nothing here asks for extra folders.
fn marked_roles(
    noun: &str,
    available: &[String],
) -> Result<(Option<String>, Option<String>, Option<String>)> {
    println!(
        "  The server marks its drafts, trash and junk {noun}s; name one only if it does not."
    );
    let pick = |role: &str| -> Result<Option<String>> {
        if available.is_empty() {
            let typed = prompt_input(
                &format!("{role} {noun} name (Enter: the one the server marks)"),
                "",
            )?;
            return Ok(role_answer(&typed));
        }
        let mut items = vec!["The one the server marks".to_string()];
        items.extend(available.iter().cloned());
        let selection = dialoguer::FuzzySelect::new()
            .with_prompt(format!("Which {noun} is your {role}?"))
            .items(&items)
            .default(0)
            .interact()
            .context("Selection cancelled")?;
        Ok((selection > 0).then(|| items[selection].clone()))
    };
    Ok((pick("Drafts")?, pick("Trash")?, pick("Junk")?))
}

/// A typed role folder name, `None` for an empty answer.
fn role_answer(typed: &str) -> Option<String> {
    let typed = typed.trim();
    (!typed.is_empty()).then(|| typed.to_string())
}

/// The policy overrides, one `<pattern> <policy>` line at a time until an
/// empty one (#0141).
fn ask_overrides(noun: &str) -> Result<Vec<(String, FolderPolicy)>> {
    println!(
        "  Each {noun} is synced, shown with the server's counts, or ignored by default; \
         an override changes that for the {noun}s a pattern matches."
    );
    let mut overrides = Vec::new();
    loop {
        let line = prompt_input(
            "Override as `<pattern> <sync|status-only|ignore>` (Enter: done)",
            "",
        )?;
        if line.trim().is_empty() {
            return Ok(overrides);
        }
        match parse_override(&line) {
            Ok(rule) => overrides.push(rule),
            Err(why) => eprintln!("  {} {why}", "\u{26a0}".yellow()),
        }
    }
}

/// One override line: a glob, then the policy as its last word. A pattern
/// may hold spaces (`Sent Items`), a policy never does.
pub fn parse_override(line: &str) -> Result<(String, FolderPolicy), String> {
    let line = line.trim();
    let Some((pattern, policy)) = line.rsplit_once(char::is_whitespace) else {
        return Err(format!(
            "'{line}' has no policy; end it with sync, status-only or ignore"
        ));
    };
    let pattern = pattern.trim();
    match FolderPolicy::parse(policy) {
        Some(policy) if !pattern.is_empty() => Ok((pattern.to_string(), policy)),
        Some(_) => Err(format!("'{line}' has no pattern")),
        None => Err(format!(
            "'{policy}' is not a policy; a policy is sync, status-only or ignore"
        )),
    }
}

/// Write the account: a whole file for `init`, one block for `add-account`.
/// Answers the file written, which is the daemon's to name.
async fn write(
    daemon: &mut impl WizardDaemon,
    mode: &Mode,
    candidate: &Value,
    path: &Path,
) -> Result<PathBuf> {
    match mode {
        Mode::Init { replace } => daemon.init(candidate, *replace).await,
        Mode::Add { .. } => {
            daemon.add_account(candidate).await?;
            Ok(path.to_path_buf())
        }
    }
}

/// The closing lines both flows share.
fn finished(mode: &Mode, name: &str, path: &Path) {
    match mode {
        Mode::Init { .. } => {
            println!(
                "{} Config written to {}",
                "\u{2713}".green().bold(),
                path.display()
            );
            println!(
                "{} Mail data dir: {}",
                "\u{2713}".green().bold(),
                mailypoppins_data_dir().display()
            );
        }
        Mode::Add { .. } => println!(
            "{} Account '{}' added to {}",
            "\u{2713}".green().bold(),
            name,
            path.display()
        ),
    }
}

/// One line of input; Enter is `default`.
fn prompt_input(prompt: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        print!("{prompt}: ");
    } else {
        print!("{prompt} [{default}]: ");
    }
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    Ok(if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    })
}

/// A password, masked on a terminal; a line of a pipe when stdin is not one,
/// so a script can answer the wizard.
fn read_password(prompt: &str) -> Result<String> {
    use std::io::IsTerminal;
    if io::stdin().is_terminal() {
        return dialoguer::Password::new()
            .with_prompt(prompt)
            .interact()
            .context("Password input cancelled");
    }
    print!("{prompt}: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim_end_matches(['\r', '\n']).to_string())
}

/// A `[Y/n]` or `[y/N]` answer; Enter is `default`.
fn yes_no(default: bool) -> Result<bool> {
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(match input.trim().to_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    })
}

/// One of `options`, the first `preferred` match preselected.
fn select_mailbox(prompt: &str, options: &[String], preferred: &[&str]) -> Result<String> {
    let default = preferred
        .iter()
        .find_map(|pref| options.iter().position(|o| o.eq_ignore_ascii_case(pref)))
        .unwrap_or(0);
    let selection = dialoguer::FuzzySelect::new()
        .with_prompt(prompt)
        .items(options)
        .default(default)
        .interact()
        .context("Selection cancelled")?;
    Ok(options[selection].clone())
}
