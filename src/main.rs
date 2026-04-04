use email::types::*;
use email::config::*;
use email::parse::*;
use email::imap_client::{self, *};
use email::draft::*;
use email::send::*;
use email::sync::*;
use email::config_cmd::*;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use colored::*;
use log::{error, info, warn};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Parser)]
#[command(name = "email-cli")]
#[command(about = "A CLI tool for sending emails from Markdown drafts with YAML frontmatter")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// File to preview (dry-run mode)
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,

    /// Signature to use (overrides config default)
    #[arg(short, long, global = true)]
    signature: Option<String>,

    /// Skip signature entirely
    #[arg(long, global = true)]
    no_signature: bool,

    /// Account to use (default: first in config)
    #[arg(short = 'A', long, global = true)]
    account: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Send a single approved email
    Send {
        /// Path to the email draft file
        file: PathBuf,
        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Send all approved emails in a directory
    SendApproved {
        /// Directory containing email drafts
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// List emails by status
    List {
        /// Directory to scan
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Validate YAML frontmatter in files
    Validate {
        /// File or directory to validate
        path: PathBuf,
    },
    /// Mark an email as approved
    MarkApproved {
        /// File to mark as approved
        file: PathBuf,
    },
    /// Create a new email draft from template
    New {
        /// Name for the new draft file
        name: String,
    },
    /// Create a reply draft from a received email
    Reply {
        /// Path to the email to reply to (interactive selection if omitted)
        file: Option<PathBuf>,
        /// Reply to all recipients
        #[arg(long)]
        all: bool,
    },
    /// Forward an email to new recipients
    Forward {
        /// Path to the email to forward (interactive selection if omitted)
        file: Option<PathBuf>,
    },
    /// List available IMAP mailboxes/folders
    ListMailboxes,
    /// Fetch emails from IMAP server
    Fetch {
        /// Filter by sender address
        #[arg(long)]
        from: Option<String>,
        /// Filter by recipient address
        #[arg(long)]
        to: Option<String>,
        /// Filter by CC address
        #[arg(long)]
        cc: Option<String>,
        /// Subject contains
        #[arg(long)]
        subject: Option<String>,
        /// Body contains
        #[arg(long)]
        body: Option<String>,
        /// Emails since date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// Emails before date (YYYY-MM-DD)
        #[arg(long)]
        before: Option<String>,
        /// Max results (default: 10)
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
        /// Show full body instead of preview
        #[arg(long)]
        full: bool,
        /// Mailbox name (default: INBOX)
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
    },
    /// Sync local mailbox folders with IMAP server
    Sync {
        /// Max messages per mailbox (default: 50)
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,
        /// Mailboxes to sync (default: INBOX, Archive, Sent)
        #[arg(long)]
        mailbox: Option<Vec<String>>,
        /// Reconcile local files against server (detect moves/deletes)
        #[arg(long)]
        reconcile: bool,
    },
    /// Watch a mailbox for changes using IMAP IDLE
    Watch {
        /// Mailbox to watch (default: INBOX)
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        /// Timeout in seconds (exits with code 2 on timeout)
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Archive an inbox email (server + local)
    Archive {
        /// Path to the inbox email file
        file: PathBuf,
    },
    /// Delete an inbox email (server + local)
    Delete {
        /// Path to the inbox email file
        file: PathBuf,
    },
    /// Open an email's attachment in the default application
    Open {
        /// Path to the email file
        file: PathBuf,
    },
    /// Search emails on the IMAP server
    Search {
        /// Search query (supports from:, to:, subject:, body:, since:, before:, in: prefixes)
        query: String,
        /// Mailbox to search (default: all configured mailboxes)
        #[arg(long)]
        mailbox: Option<String>,
        /// Max results (default: 20)
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
        /// Show full body instead of preview
        #[arg(long)]
        full: bool,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Interactive setup wizard
    Init,
    /// Show current configuration
    Show,
    /// Store a password in the OS keyring
    SetPassword {
        /// Which password to set: "smtp" or "imap"
        which: String,
        /// Account name (required if multiple accounts)
        #[arg(long)]
        account: Option<String>,
    },
    /// Add a new account to the existing config
    AddAccount,
    /// Migrate old single-account config to new [[accounts]] format
    Migrate,
    /// Print config file path
    Path,
}

/// Sort fetched emails by date descending (newest first).
fn sort_fetched_by_date(emails: &mut [FetchedEmail]) {
    emails.sort_by(|a, b| {
        let da = chrono::DateTime::parse_from_rfc2822(&a.date).ok();
        let db = chrono::DateTime::parse_from_rfc2822(&b.date).ok();
        db.cmp(&da)
    });
}

fn prompt_confirmation(message: &str) -> bool {
    print!("{} [y/N] ", message);
    io::stdout().flush().unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();

    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let cli = Cli::parse();

    info!("email-cli started: {:?}", std::env::args().collect::<Vec<_>>());

    // Load global config from ~/.config/email/config.toml
    let global_config = load_global_config().unwrap_or_else(|e| {
        eprintln!("{} {}", "⚠".yellow(), e);
        eprintln!("  Some commands may not work without proper configuration.");
        GlobalConfig::default()
    });

    // Resolve which account to use
    let account_config: email::config::AccountConfig = if let Some(ref name) = cli.account {
        global_config.accounts.iter()
            .find(|a| a.name == *name)
            .cloned()
            .unwrap_or_else(|| {
                eprintln!("{} Account '{}' not found in config", "⚠".yellow(), name);
                email::config::AccountConfig::default()
            })
    } else {
        global_config.accounts.first().cloned().unwrap_or_default()
    };

    // Load SMTP config from account config + keyring
    let smtp_config = SmtpConfig::load(&account_config).unwrap_or_else(|e| {
        eprintln!("{} Could not load SMTP config: {}", "⚠".yellow(), e);
        eprintln!("  Some commands may not work without proper configuration.");
        SmtpConfig {
            host: "localhost".to_string(),
            port: 465,
            username: String::new(),
            password: String::new(),
            default_from: "user@example.com".to_string(),
            accept_invalid_certs: false,
        }
    });

    // Determine signature to use
    let signature_content: Option<String> = if cli.no_signature {
        None
    } else if global_config.email.include_signature {
        load_signature(&account_config, cli.signature.as_deref())
    } else {
        // include_signature is false, but user can override with --signature
        cli.signature
            .as_deref()
            .and_then(|s| load_signature(&account_config, Some(s)))
    };

    // Resolve directories from config (mailbox-based)
    let sent_dir: Option<PathBuf> = account_config.mailboxes.sent.as_ref()
        .map(|m| resolve_mailbox_local_path(&account_config, m));
    let drafts_dir: Option<PathBuf> = resolve_drafts_dir_from_config(&account_config);
    let inbox_dir: Option<PathBuf> = account_config.mailboxes.inbox.as_ref()
        .map(|m| resolve_mailbox_local_path(&account_config, m));
    let archive_dir: Option<PathBuf> = account_config.mailboxes.archive.as_ref()
        .map(|m| resolve_mailbox_local_path(&account_config, m));

    match cli.command {
        Some(Commands::Send { file, yes }) => {
            let draft = parse_email_draft(&file)?;
            validate_draft(&draft)?;

            preview_draft(
                &draft,
                &smtp_config,
                &global_config.email,
                signature_content.as_deref(),
                false,
            )?;

            if !yes && !prompt_confirmation("Send this email?") {
                println!("Cancelled.");
                return Ok(());
            }

            println!("Sending email...");
            let (send_result, raw_message, message_id) = send_email(
                &draft,
                &smtp_config,
                &global_config.email,
                signature_content.as_deref(),
            )
            .await?;

            // Display per-recipient results
            for r in &send_result.succeeded() {
                println!(
                    "  {} {} ({})",
                    "✓".green(),
                    r.address,
                    r.role
                );
            }
            for r in &send_result.failed() {
                println!(
                    "  {} {} ({}): {}",
                    "✗".red(),
                    r.address,
                    r.role,
                    r.error.as_deref().unwrap_or("unknown error")
                );
            }

            if send_result.all_succeeded() {
                update_status_to_sent(&draft, sent_dir.as_deref(), message_id.as_deref())?;
                info!("Email marked as sent: {}", draft.path.display());

                // IMAP APPEND to Sent folder (best-effort)
                if let Ok(imap_config) = ImapConfig::load(&account_config) {
                    let sent_mailbox = resolve_sent_mailbox(&account_config);
                    if let Err(e) = append_to_sent_folder(&imap_config, &raw_message, &sent_mailbox).await {
                        warn!("Failed to append to Sent folder: {}", e);
                        println!(
                            "  {} Could not copy to server Sent folder: {}",
                            "⚠".yellow(), e
                        );
                    }
                }

                println!(
                    "{} Email sent successfully to all {} recipient(s)",
                    "✓".green().bold(),
                    send_result.results.len()
                );
            } else if send_result.any_succeeded() {
                update_status_to_sent(&draft, sent_dir.as_deref(), message_id.as_deref())?;
                warn!(
                    "Partial send: {} succeeded, {} failed for {}",
                    send_result.succeeded().len(),
                    send_result.failed().len(),
                    draft.path.display()
                );

                // IMAP APPEND to Sent folder (best-effort)
                if let Ok(imap_config) = ImapConfig::load(&account_config) {
                    let sent_mailbox = resolve_sent_mailbox(&account_config);
                    if let Err(e) = append_to_sent_folder(&imap_config, &raw_message, &sent_mailbox).await {
                        warn!("Failed to append to Sent folder: {}", e);
                        println!(
                            "  {} Could not copy to server Sent folder: {}",
                            "⚠".yellow(), e
                        );
                    }
                }

                println!(
                    "{} Partial send: {} succeeded, {} failed (marked as sent -- see logs for details)",
                    "⚠".yellow().bold(),
                    send_result.succeeded().len().to_string().green(),
                    send_result.failed().len().to_string().red()
                );
            } else {
                error!("All recipients failed for {}", draft.path.display());
                return Err(anyhow!(
                    "Failed to send to all {} recipient(s)",
                    send_result.results.len()
                ));
            }
        }

        Some(Commands::SendApproved { dir, yes }) => {
            let dir = resolve_drafts_dir(&dir, &drafts_dir);
            let drafts = find_drafts(&dir, Some(EmailStatus::Approved))?;

            if drafts.is_empty() {
                println!("No approved emails found in {}", dir.display());
                return Ok(());
            }

            println!(
                "\n{} approved email(s) found:\n",
                drafts.len().to_string().bold()
            );

            for draft in &drafts {
                println!(
                    "  {} → {}",
                    draft.path.file_name().unwrap().to_string_lossy(),
                    draft.frontmatter.to
                );
            }

            if !yes && !prompt_confirmation(&format!("\nSend all {} emails?", drafts.len())) {
                println!("Cancelled.");
                return Ok(());
            }

            let mut sent_count = 0;
            let mut failed_count = 0;

            for draft in drafts {
                print!("Sending to {}... ", draft.frontmatter.to);
                io::stdout().flush()?;

                match send_email(
                    &draft,
                    &smtp_config,
                    &global_config.email,
                    signature_content.as_deref(),
                )
                .await
                {
                    Ok((send_result, raw_message, message_id)) => {
                        if send_result.all_succeeded() {
                            if let Err(e) = update_status_to_sent(&draft, sent_dir.as_deref(), message_id.as_deref()) {
                                println!("{} (sent but failed to update status: {})", "⚠".yellow(), e);
                            } else {
                                // IMAP APPEND to Sent folder (best-effort)
                                if let Ok(imap_config) = ImapConfig::load(&account_config) {
                                    let sent_mailbox = resolve_sent_mailbox(&account_config);
                                    if let Err(e) = append_to_sent_folder(&imap_config, &raw_message, &sent_mailbox).await {
                                        warn!("Failed to append to Sent folder: {}", e);
                                    }
                                }
                                println!("{}", "✓".green());
                            }
                            sent_count += 1;
                        } else if send_result.any_succeeded() {
                            // Partial success -- mark as sent, warn about failures
                            if let Err(e) = update_status_to_sent(&draft, sent_dir.as_deref(), message_id.as_deref()) {
                                println!("{} (partial send, failed to update status: {})", "⚠".yellow(), e);
                            } else {
                                // IMAP APPEND to Sent folder (best-effort)
                                if let Ok(imap_config) = ImapConfig::load(&account_config) {
                                    let sent_mailbox = resolve_sent_mailbox(&account_config);
                                    if let Err(e) = append_to_sent_folder(&imap_config, &raw_message, &sent_mailbox).await {
                                        warn!("Failed to append to Sent folder: {}", e);
                                    }
                                }
                                println!(
                                    "{} (partial: {}/{} recipients)",
                                    "⚠".yellow(),
                                    send_result.succeeded().len(),
                                    send_result.results.len()
                                );
                            }
                            for r in &send_result.failed() {
                                warn!(
                                    "Failed recipient {} ({}) for {}: {}",
                                    r.address,
                                    r.role,
                                    draft.path.display(),
                                    r.error.as_deref().unwrap_or("unknown")
                                );
                            }
                            sent_count += 1;
                        } else {
                            println!("{} all recipients failed", "✗".red());
                            for r in &send_result.failed() {
                                error!(
                                    "Failed recipient {} ({}) for {}: {}",
                                    r.address,
                                    r.role,
                                    draft.path.display(),
                                    r.error.as_deref().unwrap_or("unknown")
                                );
                            }
                            failed_count += 1;
                        }
                    }
                    Err(e) => {
                        println!("{} {}", "✗".red(), e);
                        error!("Fatal send error for {}: {}", draft.path.display(), e);
                        failed_count += 1;
                    }
                }
            }

            println!(
                "\n{}: {} sent, {} failed",
                "Summary".bold(),
                sent_count.to_string().green(),
                failed_count.to_string().red()
            );
        }

        Some(Commands::List { dir }) => {
            let dir = resolve_drafts_dir(&dir, &drafts_dir);
            let drafts = find_drafts(&dir, None)?;

            if drafts.is_empty() {
                println!("No email drafts found in {}", dir.display());
                return Ok(());
            }

            let mut draft_count = 0;
            let mut approved_count = 0;
            let mut sent_count = 0;
            let mut inbox_count = 0;
            let mut archived_count = 0;

            println!("\n{}", "Email Drafts:".bold());
            println!("{}", "─".repeat(60));

            for draft in &drafts {
                let status_colored = match draft.frontmatter.status {
                    EmailStatus::Draft => "draft".yellow(),
                    EmailStatus::Approved => "approved".green(),
                    EmailStatus::Sent => "sent".dimmed(),
                    EmailStatus::Inbox => "inbox".cyan(),
                    EmailStatus::Archived => "archived".blue(),
                };

                match draft.frontmatter.status {
                    EmailStatus::Draft => draft_count += 1,
                    EmailStatus::Approved => approved_count += 1,
                    EmailStatus::Sent => sent_count += 1,
                    EmailStatus::Inbox => inbox_count += 1,
                    EmailStatus::Archived => archived_count += 1,
                }

                println!(
                    "[{}] {} → {}",
                    status_colored,
                    draft.path.file_name().unwrap().to_string_lossy(),
                    draft.frontmatter.to
                );
            }

            println!("{}", "─".repeat(60));
            println!(
                "Total: {} | Draft: {} | Approved: {} | Sent: {} | Inbox: {} | Archived: {}",
                drafts.len(),
                draft_count.to_string().yellow(),
                approved_count.to_string().green(),
                sent_count.to_string().dimmed(),
                inbox_count.to_string().cyan(),
                archived_count.to_string().blue()
            );
        }

        Some(Commands::Validate { path }) => {
            let files: Vec<PathBuf> = if path.is_dir() {
                WalkDir::new(&path)
                    .max_depth(1)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path().is_file() && e.path().extension().is_some_and(|ext| ext == "md")
                    })
                    .map(|e| e.path().to_path_buf())
                    .collect()
            } else {
                vec![path.clone()]
            };

            let mut valid_count = 0;
            let mut invalid_count = 0;

            for file in &files {
                match parse_email_draft(file) {
                    Ok(draft) => match validate_draft(&draft) {
                        Ok(warnings) => {
                            print!("{} {}", "✓".green(), file.display());
                            if !warnings.is_empty() {
                                print!(" ({})", warnings.join(", ").yellow());
                            }
                            println!();
                            valid_count += 1;
                        }
                        Err(e) => {
                            println!("{} {} - {}", "✗".red(), file.display(), e);
                            invalid_count += 1;
                        }
                    },
                    Err(e) => {
                        println!("{} {} - {}", "✗".red(), file.display(), e);
                        invalid_count += 1;
                    }
                }
            }

            println!(
                "\nValidation complete: {} valid, {} invalid",
                valid_count.to_string().green(),
                invalid_count.to_string().red()
            );

            if invalid_count > 0 {
                std::process::exit(1);
            }
        }

        Some(Commands::MarkApproved { file }) => {
            let msg = mark_as_approved(&file)?;
            if msg.starts_with("Already") {
                println!("{} {}", "ℹ".blue(), msg);
            } else {
                println!("{} {}", "✓".green(), msg);
            }
        }

        Some(Commands::New { name }) => {
            // Add .md extension if not present
            let file_name = if Path::new(&name).extension().is_some() {
                name.clone()
            } else {
                format!("{}.md", name)
            };
            let dir = resolve_drafts_dir(Path::new("."), &drafts_dir);
            let path = dir.join(&file_name);

            // Check if file already exists
            if path.exists() {
                return Err(anyhow!("File already exists: {}", path.display()));
            }

            // Create skeleton content with all frontmatter fields
            let now = chrono::Utc::now().to_rfc2822();
            let from = &smtp_config.default_from;
            let skeleton = format!("---\nto:\ncc:\nbcc:\nsubject:\nstatus: draft\nfrom: {from}\ndate: {now}\nreply_to:\nattachments: []\n---\n\n");

            fs::write(&path, skeleton)?;
            println!("{} Created new draft: {}", "✓".green(), path.display());
        }

        Some(Commands::Reply { file, all }) => {
            let resolved = resolve_drafts_dir(Path::new("."), &drafts_dir);
            let reply_drafts_dir: Option<PathBuf> = if resolved != Path::new(".").to_path_buf() {
                Some(resolved)
            } else {
                None
            };

            let source_file = match file {
                Some(f) => f,
                None => {
                    let dir = inbox_dir.as_ref().ok_or_else(|| {
                        anyhow!("Inbox mailbox not configured. Check [mailboxes.inbox] in {}", config_path().display())
                    })?;
                    select_inbox_email(dir, "Select an email to reply to")?
                }
            };

            let draft_path = create_reply_draft(
                &source_file,
                all,
                &smtp_config.default_from,
                reply_drafts_dir.as_deref(),
            )?;
            println!(
                "{} Reply draft created: {}",
                "✓".green(),
                draft_path.display()
            );
        }

        Some(Commands::Forward { file }) => {
            let resolved = resolve_drafts_dir(Path::new("."), &drafts_dir);
            let fwd_drafts_dir: Option<PathBuf> = if resolved != Path::new(".").to_path_buf() {
                Some(resolved)
            } else {
                None
            };

            let source_file = match file {
                Some(f) => f,
                None => {
                    let dir = inbox_dir.as_ref().ok_or_else(|| {
                        anyhow!("Inbox mailbox not configured. Check [mailboxes.inbox] in {}", config_path().display())
                    })?;
                    select_inbox_email(dir, "Select an email to forward")?
                }
            };

            let draft_path = create_forward_draft(
                &source_file,
                &smtp_config.default_from,
                fwd_drafts_dir.as_deref(),
            )?;
            println!(
                "{} Forward draft created: {}",
                "✓".green(),
                draft_path.display()
            );
        }

        Some(Commands::ListMailboxes) => {
            let imap_config = ImapConfig::load(&account_config)?;
            let mailboxes = list_mailboxes(&imap_config).await?;

            println!("{} Available mailboxes:", "ℹ".blue());
            for name in &mailboxes {
                println!("  {}", name);
            }
        }

        Some(Commands::Fetch {
            from,
            to,
            cc,
            subject,
            body,
            since,
            before,
            limit,
            full,
            mailbox,
        }) => {
            let imap_config = ImapConfig::load(&account_config)?;
            let criteria = FetchCriteria {
                from,
                to,
                cc,
                subject,
                body,
                since,
                before,
                text: None,
                in_mailbox: None,
            };

            let mailbox_for_save = mailbox.clone();
            let emails = fetch_emails(&imap_config, &criteria, &mailbox, Some(limit)).await?;

            display_fetched_emails(&emails, full);

            // Determine save directory and status from the mailbox
            let (save_dir, status) = match resolve_mailbox_dir(&account_config, &mailbox_for_save) {
                Ok(dir) => {
                    (Some(dir), mailbox_status(&mailbox_for_save))
                }
                Err(_) => {
                    // Fallback to inbox_dir for unknown mailboxes
                    (inbox_dir.clone(), "inbox")
                }
            };

            if let Some(ref save_dir) = save_dir {
                if !emails.is_empty() {
                    match save_fetched_emails(&emails, save_dir, status) {
                        Ok((saved, skipped)) => {
                            if saved > 0 {
                                println!(
                                    "\n{} Saved {} email(s) to {}",
                                    "✓".green(),
                                    saved,
                                    save_dir.display()
                                );
                            }
                            if skipped > 0 {
                                println!(
                                    "{} Skipped {} duplicate(s)",
                                    "ℹ".blue(),
                                    skipped
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("{} Failed to save emails: {}", "✗".red(), e);
                        }
                    }
                }
            } else if !emails.is_empty() {
                println!(
                    "\n{} No local directory configured for mailbox '{}'. Check [mailboxes] in {}.",
                    "ℹ".blue(),
                    mailbox_for_save,
                    config_path().display()
                );
            }
        }

        Some(Commands::Sync { limit, mailbox, reconcile }) => {
            let imap_config = ImapConfig::load(&account_config)?;

            let targets: Vec<imap_client::SyncTarget> = if let Some(ref user_mailboxes) = mailbox {
                user_mailboxes.iter().map(|mb| {
                    imap_client::SyncTarget {
                        role: mb.clone(),
                        server_name: find_server_name_for_role(&account_config, mb),
                        local_dir: resolve_mailbox_dir(&account_config, mb)
                            .unwrap_or_else(|_| PathBuf::from(mb)),
                        status: mailbox_status(mb).to_string(),
                    }
                }).collect()
            } else {
                all_configured_mailboxes(&account_config).iter().map(|(role, mapping)| {
                    imap_client::SyncTarget {
                        role: role.clone(),
                        server_name: mapping.server.clone(),
                        local_dir: resolve_mailbox_local_path(&account_config, mapping),
                        status: mailbox_status(role).to_string(),
                    }
                }).collect()
            };

            let result = sync_mailboxes(&imap_config, &targets, limit, reconcile).await?;

            if result.skipped > 0 {
                println!(
                    "{} Synced: {} new, {} already present",
                    "✓".green(),
                    result.saved,
                    result.skipped,
                );
            } else {
                println!(
                    "{} Synced: {} email(s) saved",
                    "✓".green(),
                    result.saved,
                );
            }

            if reconcile {
                if result.moved > 0 || result.removed > 0 {
                    println!(
                        "{} Reconciled: {} moved, {} removed",
                        "ℹ".blue(),
                        result.moved,
                        result.removed,
                    );
                } else {
                    println!("{} Reconciled: already in sync", "✓".green());
                }
            }
        }

        Some(Commands::Watch { mailbox, timeout }) => {
            let imap_config = ImapConfig::load(&account_config)?;
            println!("Watching {} for changes...", mailbox);
            let exit_code = watch_mailbox(&imap_config, &mailbox, timeout).await?;

            match exit_code {
                0 => println!("{} Mailbox changed.", "✓".green()),
                2 => println!("{} Timed out.", "ℹ".blue()),
                _ => {}
            }

            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }

        Some(Commands::Archive { file }) => {
            let imap_config = ImapConfig::load(&account_config)?;
            let dir = archive_dir.as_ref().ok_or_else(|| {
                anyhow!("Archive mailbox not configured. Check [mailboxes.archive] in {}", config_path().display())
            })?;
            let archive_server_name = account_config.mailboxes.archive.as_ref()
                .map(|m| m.server.as_str())
                .unwrap_or("Archive");
            match archive_email_locally(&imap_config, dir, &file, archive_server_name).await {
                Ok(()) => {
                    println!(
                        "{} Archived: {}",
                        "✓".green(),
                        file.display()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "{} Failed to archive {}: {}",
                        "✗".red(),
                        file.display(),
                        e
                    );
                    std::process::exit(1);
                }
            }
        }

        Some(Commands::Delete { file }) => {
            let imap_config = ImapConfig::load(&account_config)?;
            match delete_email_locally(&imap_config, &file).await {
                Ok(()) => {
                    println!("{} Deleted: {}", "✓".green(), file.display());
                }
                Err(e) => {
                    eprintln!(
                        "{} Failed to delete {}: {}",
                        "✗".red(),
                        file.display(),
                        e
                    );
                    std::process::exit(1);
                }
            }
        }

        Some(Commands::Open { file }) => {
            let attachments = list_attachments(&file)?;
            if attachments.is_empty() {
                println!("No attachments found for {}", file.display());
                return Ok(());
            }
            let path = if attachments.len() == 1 {
                attachments.into_iter().next().unwrap()
            } else {
                let display_items: Vec<String> = attachments
                    .iter()
                    .map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string())
                    .collect();
                let selection = dialoguer::FuzzySelect::new()
                    .with_prompt("Select attachment to open")
                    .items(&display_items)
                    .default(0)
                    .interact()
                    .map_err(|e| anyhow!("Selection cancelled: {e}"))?;
                attachments[selection].clone()
            };
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            println!("Opening: {name}");
            open_file_with_system(&path)?;
        }

        Some(Commands::Search {
            query,
            mailbox,
            limit,
            full,
        }) => {
            let imap_config = ImapConfig::load(&account_config)?;
            let mut criteria = parse_search_query(&query);

            // Resolve mailbox scope: --mailbox flag > in: prefix > all
            let mailbox_name = mailbox.or_else(|| criteria.in_mailbox.take());
            criteria.in_mailbox = None;

            if let Some(ref mb) = mailbox_name {
                let mut emails =
                    fetch_emails(&imap_config, &criteria, mb, Some(limit)).await?;
                sort_fetched_by_date(&mut emails);
                if emails.is_empty() {
                    println!("{}", "No results found".yellow());
                } else {
                    println!("{} result(s) in {}:\n", emails.len(), mb);
                    display_fetched_emails(&emails, full);
                }
            } else {
                // Search all configured mailboxes
                let configured = all_configured_mailboxes(&account_config);
                let mut session = imap_client::open_imap_session(&imap_config).await?;
                let per_mb = (limit / configured.len().max(1)).max(5);
                let mut total = 0usize;
                let mut all_emails: Vec<FetchedEmail> = Vec::new();

                for (role, mapping) in &configured {
                    if total >= limit {
                        break;
                    }
                    let budget = per_mb.min(limit - total);
                    match imap_client::fetch_emails_on_session(
                        &mut session,
                        &criteria,
                        &mapping.server,
                        Some(budget),
                    )
                    .await
                    {
                        Ok(emails) if !emails.is_empty() => {
                            total += emails.len();
                            all_emails.extend(emails);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!(
                                "{} Search in {} failed: {}",
                                "\u{26a0}".yellow(),
                                role,
                                e
                            );
                        }
                    }
                }
                session.logout().await.ok();

                sort_fetched_by_date(&mut all_emails);
                if all_emails.is_empty() {
                    println!("{}", "No results found".yellow());
                } else {
                    display_fetched_emails(&all_emails, full);
                }
            }
        }

        Some(Commands::Config { action }) => {
            match action {
                ConfigAction::Init => cmd_config_init()?,
                ConfigAction::Show => cmd_config_show()?,
                ConfigAction::SetPassword { which, account } => {
                    let acct_name = account
                        .or_else(|| cli.account.clone())
                        .or_else(|| global_config.accounts.first().map(|a| a.name.clone()))
                        .unwrap_or_else(|| "main".to_string());
                    cmd_set_password(&which, &acct_name)?;
                }
                ConfigAction::AddAccount => cmd_config_add_account()?,
                ConfigAction::Migrate => cmd_config_migrate()?,
                ConfigAction::Path => cmd_config_path(),
            }
        }

        None => {
            if let Some(file) = cli.file {
                // Preview mode (dry run)
                let draft = parse_email_draft(&file)?;
                preview_draft(
                    &draft,
                    &smtp_config,
                    &global_config.email,
                    signature_content.as_deref(),
                    true,
                )?;
            } else {
                // No file, no subcommand -> launch TUI
                email::tui::run()?;
            }
        }
    }

    Ok(())
}
