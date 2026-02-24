use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use colored::*;
use gray_matter::{engine::YAML, Matter};
use lettre::{
    address::Envelope,
    message::{header::ContentType, Attachment, Mailbox, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use log::{debug, error, info, warn};
use mailparse::{parse_mail, MailHeaderMap};
use pulldown_cmark::{html, Options, Parser as MdParser};
use simplelog::{CombinedLogger, Config as LogConfig, LevelFilter, WriteLogger};
use skim::prelude::*;
use skim::tuikit::prelude::{Attr, Color};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Email configuration from config.toml
#[derive(Debug, Deserialize, Default)]
struct EmailConfig {
    #[serde(default)]
    email: EmailSettings,
    #[serde(default)]
    signatures: SignaturesConfig,
}

#[derive(Debug, Deserialize)]
struct EmailSettings {
    #[serde(default = "default_font_family")]
    font_family: String,
    #[serde(default = "default_font_size")]
    font_size: String,
    #[serde(default = "default_true")]
    include_signature: bool,
}

impl Default for EmailSettings {
    fn default() -> Self {
        Self {
            font_family: default_font_family(),
            font_size: default_font_size(),
            include_signature: true,
        }
    }
}

fn default_font_family() -> String {
    "Helvetica, Arial, sans-serif".to_string()
}

fn default_font_size() -> String {
    "12pt".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Default)]
struct SignaturesConfig {
    #[serde(default)]
    default: Option<String>,
    #[serde(flatten)]
    entries: HashMap<String, SignatureEntry>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
struct SignatureEntry {
    #[serde(default)]
    name: Option<String>,
    path: String,
}

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
    /// Interactive email browser
    Browse {
        /// Starting directory: inbox, drafts, or sent (default: inbox)
        #[arg(default_value = "inbox")]
        view: String,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum EmailStatus {
    Draft,
    Approved,
    Sent,
    Inbox,
    Archived,
}

impl std::fmt::Display for EmailStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmailStatus::Draft => write!(f, "draft"),
            EmailStatus::Approved => write!(f, "approved"),
            EmailStatus::Sent => write!(f, "sent"),
            EmailStatus::Inbox => write!(f, "inbox"),
            EmailStatus::Archived => write!(f, "archived"),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct EmailFrontmatter {
    to: String,
    #[serde(default)]
    cc: Option<String>,
    #[serde(default)]
    bcc: Option<String>,
    subject: String,
    status: EmailStatus,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    reply_to: Option<String>,
    #[serde(default)]
    attachments: Option<Vec<String>>,
    #[serde(default)]
    sent_at: Option<String>,
    #[serde(default)]
    sent_via: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
}

#[derive(Debug)]
struct EmailDraft {
    path: PathBuf,
    frontmatter: EmailFrontmatter,
    body_markdown: String,
}

struct SmtpConfig {
    host: String,
    port: u16,
    username: String,
    password: String,
    default_from: String,
}

#[derive(Clone)]
struct ImapConfig {
    host: String,
    port: u16,
    username: String,
    password: String,
}

impl ImapConfig {
    fn from_env(path_hint: Option<&Path>) -> Result<Self> {
        load_env_from_path(path_hint);

        Ok(Self {
            host: std::env::var("IMAP_HOST").context("IMAP_HOST not set in environment")?,
            port: std::env::var("IMAP_PORT")
                .unwrap_or_else(|_| "993".to_string())
                .parse()
                .context("Invalid IMAP_PORT")?,
            username: std::env::var("IMAP_USERNAME")
                .or_else(|_| std::env::var("SMTP_USERNAME"))
                .context("Neither IMAP_USERNAME nor SMTP_USERNAME set")?,
            password: std::env::var("IMAP_PASSWORD")
                .or_else(|_| std::env::var("SMTP_PASSWORD"))
                .context("Neither IMAP_PASSWORD nor SMTP_PASSWORD set")?,
        })
    }
}

struct FetchCriteria {
    from: Option<String>,
    to: Option<String>,
    cc: Option<String>,
    subject: Option<String>,
    body: Option<String>,
    since: Option<String>,
    before: Option<String>,
}

struct FetchedEmail {
    from: String,
    to: String,
    cc: Option<String>,
    subject: String,
    date: String,
    body_text: String,
    html_body: Option<String>,
    has_attachments: bool,
    message_id: Option<String>,
}

/// Initialize file-based logging to ~/.email-cli/logs/email-cli-YYYY-MM-DD.log.
/// Non-fatal: prints a warning and continues if setup fails.
fn init_logging() {
    let log_dir = dirs_next().join("logs");
    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!(
            "{} Could not create log directory {}: {}",
            "⚠".yellow(),
            log_dir.display(),
            e
        );
        return;
    }

    let filename = format!("email-cli-{}.log", Utc::now().format("%Y-%m-%d"));
    let log_path = log_dir.join(filename);

    let log_file = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "{} Could not open log file {}: {}",
                "⚠".yellow(),
                log_path.display(),
                e
            );
            return;
        }
    };

    if let Err(e) = CombinedLogger::init(vec![WriteLogger::new(
        LevelFilter::Debug,
        LogConfig::default(),
        log_file,
    )]) {
        eprintln!("{} Could not initialize logger: {}", "⚠".yellow(), e);
    }
}

/// Return the base directory for email-cli data: ~/.email-cli
fn dirs_next() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".email-cli")
}

// ---------------------------------------------------------------------------
// Per-recipient sending types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum RecipientRole {
    To,
    Cc,
    Bcc,
}

impl std::fmt::Display for RecipientRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecipientRole::To => write!(f, "To"),
            RecipientRole::Cc => write!(f, "Cc"),
            RecipientRole::Bcc => write!(f, "Bcc"),
        }
    }
}

#[derive(Debug)]
struct RecipientResult {
    address: String,
    role: RecipientRole,
    success: bool,
    error: Option<String>,
}

#[derive(Debug)]
struct SendResult {
    results: Vec<RecipientResult>,
}

impl SendResult {
    fn all_succeeded(&self) -> bool {
        self.results.iter().all(|r| r.success)
    }

    fn any_succeeded(&self) -> bool {
        self.results.iter().any(|r| r.success)
    }

    fn succeeded(&self) -> Vec<&RecipientResult> {
        self.results.iter().filter(|r| r.success).collect()
    }

    fn failed(&self) -> Vec<&RecipientResult> {
        self.results.iter().filter(|r| !r.success).collect()
    }
}

fn build_imap_search_query(criteria: &FetchCriteria) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(ref from) = criteria.from {
        parts.push(format!("FROM \"{}\"", from));
    }
    if let Some(ref to) = criteria.to {
        parts.push(format!("TO \"{}\"", to));
    }
    if let Some(ref cc) = criteria.cc {
        parts.push(format!("CC \"{}\"", cc));
    }
    if let Some(ref subject) = criteria.subject {
        parts.push(format!("SUBJECT \"{}\"", subject));
    }
    if let Some(ref body) = criteria.body {
        parts.push(format!("BODY \"{}\"", body));
    }
    if let Some(ref since) = criteria.since {
        // IMAP expects dates as DD-Mon-YYYY
        if let Some(imap_date) = parse_date_to_imap(since) {
            parts.push(format!("SINCE {}", imap_date));
        }
    }
    if let Some(ref before) = criteria.before {
        if let Some(imap_date) = parse_date_to_imap(before) {
            parts.push(format!("BEFORE {}", imap_date));
        }
    }

    if parts.is_empty() {
        "ALL".to_string()
    } else {
        parts.join(" ")
    }
}

fn parse_date_to_imap(date_str: &str) -> Option<String> {
    // Parse YYYY-MM-DD and convert to DD-Mon-YYYY
    let parts: Vec<&str> = date_str.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let year = parts[0];
    let month = match parts[1] {
        "01" => "Jan",
        "02" => "Feb",
        "03" => "Mar",
        "04" => "Apr",
        "05" => "May",
        "06" => "Jun",
        "07" => "Jul",
        "08" => "Aug",
        "09" => "Sep",
        "10" => "Oct",
        "11" => "Nov",
        "12" => "Dec",
        _ => return None,
    };
    let day: u32 = parts[2].parse().ok()?;
    Some(format!("{}-{}-{}", day, month, year))
}

fn html_to_plain(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 80).unwrap_or_else(|_| html.to_string())
}

/// Recursively collect the first text/plain and text/html parts from a parsed email.
fn extract_body_parts(parsed: &mailparse::ParsedMail) -> (Option<String>, Option<String>) {
    if parsed.ctype.mimetype == "text/plain" {
        let body = parsed.get_body().unwrap_or_default();
        if !body.is_empty() {
            return (Some(body), None);
        }
    }

    if parsed.ctype.mimetype == "text/html" {
        let body = parsed.get_body().unwrap_or_default();
        if !body.is_empty() {
            return (None, Some(body));
        }
    }

    let mut plain = None;
    let mut html = None;

    for sub in &parsed.subparts {
        let (sub_plain, sub_html) = extract_body_parts(sub);
        if plain.is_none() {
            plain = sub_plain;
        }
        if html.is_none() {
            html = sub_html;
        }
        if plain.is_some() && html.is_some() {
            break;
        }
    }

    (plain, html)
}

/// Extract body text from a parsed email.
/// Returns (plain_text, Option<html_body>).
fn extract_body_text(parsed: &mailparse::ParsedMail) -> (String, Option<String>) {
    let (plain, html) = extract_body_parts(parsed);

    if let Some(plain_text) = plain {
        (plain_text, html)
    } else if let Some(ref html_text) = html {
        (html_to_plain(html_text), html)
    } else {
        (String::new(), None)
    }
}

fn has_attachments(parsed: &mailparse::ParsedMail) -> bool {
    for sub in &parsed.subparts {
        let disposition = sub.get_content_disposition();
        if disposition.disposition == mailparse::DispositionType::Attachment {
            return true;
        }
        if has_attachments(sub) {
            return true;
        }
    }
    false
}

/// Compress a sorted list of UIDs into IMAP sequence set format using ranges.
/// e.g., [1,2,3,5,7,8,9] -> "1:3,5,7:9"
fn compress_uid_set(uids: &[u32]) -> String {
    if uids.is_empty() {
        return String::new();
    }
    let mut sorted = uids.to_vec();
    sorted.sort();

    let mut ranges = Vec::new();
    let mut start = sorted[0];
    let mut end = sorted[0];

    for &uid in &sorted[1..] {
        if uid == end + 1 {
            end = uid;
        } else {
            if start == end {
                ranges.push(start.to_string());
            } else {
                ranges.push(format!("{}:{}", start, end));
            }
            start = uid;
            end = uid;
        }
    }
    if start == end {
        ranges.push(start.to_string());
    } else {
        ranges.push(format!("{}:{}", start, end));
    }
    ranges.join(",")
}

/// Fetch only Message-IDs from a mailbox (lightweight, no body download).
fn fetch_server_message_ids(
    imap_config: &ImapConfig,
    mailbox: &str,
) -> Result<std::collections::HashSet<String>> {
    info!("Fetching Message-IDs from mailbox '{}'", mailbox);
    let tls = native_tls::TlsConnector::builder().build()?;
    let client = imap::connect(
        (imap_config.host.as_str(), imap_config.port),
        &imap_config.host,
        &tls,
    )
    .map_err(|e| anyhow!("Failed to connect to IMAP server: {}", e))?;

    let mut session = client
        .login(&imap_config.username, &imap_config.password)
        .map_err(|e| anyhow!("IMAP login failed: {}", e.0))?;

    session
        .select(mailbox)
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    let uids = session
        .uid_search("ALL")
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        session.logout().ok();
        return Ok(std::collections::HashSet::new());
    }

    let mut uid_list: Vec<u32> = uids.into_iter().collect();
    uid_list.sort();
    let uid_set = compress_uid_set(&uid_list);

    let fetched = session
        .uid_fetch(&uid_set, "BODY.PEEK[HEADER.FIELDS (Message-ID)]")
        .map_err(|e| anyhow!("Failed to fetch Message-IDs: {}", e))?;

    let mut ids = std::collections::HashSet::new();
    for msg in fetched.iter() {
        if let Some(header_bytes) = msg.header() {
            if let Ok(text) = std::str::from_utf8(header_bytes) {
                // Parse "Message-ID: <...>\r\n" from the header field
                for line in text.lines() {
                    let trimmed = line.trim();
                    if let Some(value) = trimmed.strip_prefix("Message-ID:")
                        .or_else(|| trimmed.strip_prefix("Message-Id:"))
                        .or_else(|| trimmed.strip_prefix("message-id:"))
                    {
                        let id = value.trim();
                        if !id.is_empty() {
                            ids.insert(id.to_string());
                        }
                    }
                }
            }
        }
    }

    session.logout().ok();
    Ok(ids)
}

fn fetch_emails(
    imap_config: &ImapConfig,
    criteria: &FetchCriteria,
    mailbox: &str,
    limit: Option<usize>,
) -> Result<Vec<FetchedEmail>> {
    info!("Fetching emails from mailbox '{}' (limit: {:?})", mailbox, limit);
    let tls = native_tls::TlsConnector::builder().build()?;
    let client = imap::connect(
        (imap_config.host.as_str(), imap_config.port),
        &imap_config.host,
        &tls,
    )
    .map_err(|e| anyhow!("Failed to connect to IMAP server: {}", e))?;

    let mut session = client
        .login(&imap_config.username, &imap_config.password)
        .map_err(|e| anyhow!("IMAP login failed: {}", e.0))?;

    session
        .select(mailbox)
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    let query = build_imap_search_query(criteria);
    let uids = session
        .uid_search(&query)
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        session.logout().ok();
        return Ok(Vec::new());
    }

    // Take the last N UIDs (most recent)
    let mut uid_list: Vec<u32> = uids.into_iter().collect();
    uid_list.sort();
    let selected_uids: Vec<u32> = match limit {
        Some(n) => uid_list.into_iter().rev().take(n).collect(),
        None => uid_list,
    };

    let uid_set = compress_uid_set(&selected_uids);

    let fetched = session
        .uid_fetch(&uid_set, "RFC822")
        .map_err(|e| anyhow!("Failed to fetch emails: {}", e))?;

    let mut emails = Vec::new();

    for msg in fetched.iter() {
        let body_raw = msg.body().unwrap_or_default();
        let parsed = match parse_mail(body_raw) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let headers = &parsed.headers;
        let from = headers
            .get_first_value("From")
            .unwrap_or_else(|| "(unknown)".to_string());
        let to = headers
            .get_first_value("To")
            .unwrap_or_else(|| "(unknown)".to_string());
        let cc = headers.get_first_value("Cc");
        let subject = headers
            .get_first_value("Subject")
            .unwrap_or_else(|| "(no subject)".to_string());
        let date = headers
            .get_first_value("Date")
            .unwrap_or_else(|| "(unknown date)".to_string());

        let message_id = headers.get_first_value("Message-ID")
            .or_else(|| headers.get_first_value("Message-Id"));
        let (body_text, html_body) = extract_body_text(&parsed);
        let attachments = has_attachments(&parsed);

        emails.push(FetchedEmail {
            from,
            to,
            cc,
            subject,
            date,
            body_text,
            html_body,
            has_attachments: attachments,
            message_id,
        });
    }

    session.logout().ok();
    Ok(emails)
}

fn list_mailboxes(imap_config: &ImapConfig) -> Result<Vec<String>> {
    let tls = native_tls::TlsConnector::builder().build()?;
    let client = imap::connect(
        (imap_config.host.as_str(), imap_config.port),
        &imap_config.host,
        &tls,
    )
    .map_err(|e| anyhow!("Failed to connect to IMAP server: {}", e))?;

    let mut session = client
        .login(&imap_config.username, &imap_config.password)
        .map_err(|e| anyhow!("IMAP login failed: {}", e.0))?;

    let mailboxes = session
        .list(None, Some("*"))
        .map_err(|e| anyhow!("Failed to list mailboxes: {}", e))?;

    let names: Vec<String> = mailboxes.iter().map(|m| m.name().to_string()).collect();

    session.logout().ok();
    Ok(names)
}

fn append_to_sent_folder(imap_config: &ImapConfig, raw_message: &[u8]) -> Result<()> {
    let sent_mailbox = resolve_sent_mailbox();
    info!("Appending sent email to IMAP folder '{}'", sent_mailbox);

    let tls = native_tls::TlsConnector::builder().build()?;
    let client = imap::connect(
        (imap_config.host.as_str(), imap_config.port),
        &imap_config.host,
        &tls,
    )
    .map_err(|e| anyhow!("Failed to connect to IMAP server: {}", e))?;

    let mut session = client
        .login(&imap_config.username, &imap_config.password)
        .map_err(|e| anyhow!("IMAP login failed: {}", e.0))?;

    session.append_with_flags(&sent_mailbox, raw_message, &[imap::types::Flag::Seen])
        .map_err(|e| anyhow!("Failed to APPEND to '{}': {}", sent_mailbox, e))?;

    session.logout().ok();
    info!("Successfully appended to '{}'", sent_mailbox);
    Ok(())
}

fn watch_mailbox(imap_config: &ImapConfig, mailbox: &str, timeout: Option<u64>) -> Result<i32> {
    use imap::extensions::idle::WaitOutcome;

    let tls = native_tls::TlsConnector::builder().build()?;
    let client = imap::connect(
        (imap_config.host.as_str(), imap_config.port),
        &imap_config.host,
        &tls,
    )
    .map_err(|e| anyhow!("Failed to connect to IMAP server: {}", e))?;

    let mut session = client
        .login(&imap_config.username, &imap_config.password)
        .map_err(|e| anyhow!("IMAP login failed: {}", e.0))?;

    session
        .select(mailbox)
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    info!("Watching mailbox '{}' for changes (IMAP IDLE)", mailbox);
    println!("Watching {} for changes...", mailbox);

    let exit_code = match timeout {
        None => {
            // No timeout: block until mailbox changes, with automatic keepalive
            session.idle()?.wait_keepalive()?;
            0 // mailbox changed
        }
        Some(timeout_secs) => {
            let total_timeout = std::time::Duration::from_secs(timeout_secs);
            let idle_interval = std::time::Duration::from_secs(29.min(timeout_secs));
            let start = std::time::Instant::now();

            loop {
                let remaining = total_timeout.saturating_sub(start.elapsed());
                if remaining.is_zero() {
                    break 2; // timed out
                }

                let wait_duration = idle_interval.min(remaining);
                let outcome = session.idle()?.wait_with_timeout(wait_duration)?;

                match outcome {
                    WaitOutcome::MailboxChanged => break 0,
                    WaitOutcome::TimedOut => continue, // re-idle for keepalive
                }
            }
        }
    };

    session.logout().ok();

    Ok(exit_code)
}

fn slugify_subject(subject: &str) -> String {
    let slug: String = subject
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else if c == ' ' { '-' } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("");
    // Collapse consecutive hyphens
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            prev_hyphen = false;
            result.push(c);
        }
    }
    let result = result.trim_matches('-').to_string();
    if result.len() > 40 {
        // Truncate at word boundary
        let truncated = &result[..40];
        truncated.trim_end_matches('-').to_string()
    } else {
        result
    }
}

fn slugify_sender(from: &str) -> String {
    // Extract display name if present (e.g. "John Doe <john@example.com>" -> "John Doe")
    // Otherwise use the local part of the email address
    let name = if let Some(start) = from.find('<') {
        let display = from[..start].trim().trim_matches('"');
        if display.is_empty() {
            // No display name, use local part of email
            let email = &from[start + 1..from.find('>').unwrap_or(from.len())];
            email.split('@').next().unwrap_or("unknown").to_string()
        } else {
            display.to_string()
        }
    } else if from.contains('@') {
        from.split('@').next().unwrap_or("unknown").to_string()
    } else {
        from.to_string()
    };

    // Slugify the name
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse consecutive hyphens and trim
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            prev_hyphen = false;
            result.push(c);
        }
    }
    result.trim_matches('-').to_string()
}

fn extract_email_address(raw: &str) -> String {
    if let Some(start) = raw.find('<') {
        if let Some(end) = raw.find('>') {
            return raw[start + 1..end].trim().to_string();
        }
    }
    raw.trim().to_string()
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct InboxFrontmatter {
    from: String,
    to: String,
    #[serde(default)]
    cc: Option<String>,
    subject: String,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
}

fn select_inbox_email(inbox_dir: &Path) -> Result<PathBuf> {
    let mut entries: Vec<(PathBuf, InboxFrontmatter)> = Vec::new();

    for entry in WalkDir::new(inbox_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let matter = Matter::<YAML>::new();
            let parsed = matter.parse(&content);
            if let Some(data) = parsed.data {
                if let Ok(fm) = data.deserialize::<InboxFrontmatter>() {
                    entries.push((path.to_path_buf(), fm));
                }
            }
        }
    }

    if entries.is_empty() {
        return Err(anyhow!("No inbox emails found in {}", inbox_dir.display()));
    }

    // Sort by date descending (most recent first)
    entries.sort_by(|a, b| {
        let da = a.1.date.as_deref().unwrap_or("");
        let db = b.1.date.as_deref().unwrap_or("");
        db.cmp(da)
    });

    // Build display strings for the fuzzy selector
    let display_items: Vec<String> = entries
        .iter()
        .map(|(_, fm)| {
            let date_short = fm
                .date
                .as_deref()
                .and_then(|d| chrono::DateTime::parse_from_rfc2822(d).ok())
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let from_short = extract_email_address(&fm.from);
            format!("{} | {} | {}", date_short, from_short, fm.subject)
        })
        .collect();

    let selection = dialoguer::FuzzySelect::new()
        .with_prompt("Select an email to reply to")
        .items(&display_items)
        .default(0)
        .interact()
        .context("Selection cancelled")?;

    Ok(entries[selection].0.clone())
}

fn create_reply_draft(
    source_path: &Path,
    reply_all: bool,
    default_from: &str,
    drafts_dir: Option<&Path>,
) -> Result<PathBuf> {
    let content = fs::read_to_string(source_path)?;
    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);
    let inbox: InboxFrontmatter = parsed
        .data
        .ok_or_else(|| anyhow!("No frontmatter found"))?
        .deserialize()?;
    let original_body = parsed.content.trim();

    // Build reply fields
    let reply_to = extract_email_address(&inbox.from);

    let reply_cc = if reply_all {
        let mut all_recipients: Vec<String> = Vec::new();
        for addr in inbox.to.split(',') {
            let email = extract_email_address(addr.trim());
            if email.to_lowercase() != default_from.to_lowercase() {
                all_recipients.push(email);
            }
        }
        if let Some(ref cc) = inbox.cc {
            for addr in cc.split(',') {
                let email = extract_email_address(addr.trim());
                if email.to_lowercase() != default_from.to_lowercase()
                    && !all_recipients
                        .iter()
                        .any(|r| r.to_lowercase() == email.to_lowercase())
                {
                    all_recipients.push(email);
                }
            }
        }
        if all_recipients.is_empty() {
            None
        } else {
            Some(all_recipients.join(", "))
        }
    } else {
        None
    };

    // Build subject with Re: prefix (case-insensitive check)
    let reply_subject = if inbox.subject.to_lowercase().starts_with("re: ") {
        inbox.subject.clone()
    } else {
        format!("Re: {}", inbox.subject)
    };

    // Clean up quoted body: convert HTML remnants to plain text if needed
    let clean_body = if original_body.contains('<') && original_body.contains('>') {
        html_to_plain(original_body)
    } else {
        original_body.to_string()
    };

    // Build quoted body with attribution
    let attribution = format!(
        "On {}, {} wrote:",
        inbox.date.as_deref().unwrap_or("(unknown date)"),
        inbox.from
    );
    let quoted_body: String = clean_body
        .trim()
        .lines()
        .map(|line| format!("> {}", line))
        .collect::<Vec<_>>()
        .join("\n");

    // Build frontmatter
    let mut fm = String::from("---\n");
    fm.push_str(&format!("from: \"{}\"\n", default_from));
    fm.push_str(&format!("to: \"{}\"\n", reply_to));
    if let Some(ref cc) = reply_cc {
        fm.push_str(&format!("cc: \"{}\"\n", cc));
    }
    fm.push_str(&format!(
        "subject: \"{}\"\n",
        reply_subject.replace('"', "\\\"")
    ));
    fm.push_str("status: draft\n");
    fm.push_str("---\n");

    // Compose full content with {{SIGNATURE}} placeholder between reply area and quoted text
    let full_content = format!(
        "{}\n\n\n{{{{SIGNATURE}}}}\n\n{}\n{}\n",
        fm.trim_end(),
        attribution,
        quoted_body
    );

    // Determine output path
    let output_dir = drafts_dir.unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_dir)?;
    let date_prefix = Utc::now().format("%Y-%m-%d-%H%M").to_string();
    let sender_slug = slugify_sender(&inbox.from);
    let subject_slug = slugify_subject(&reply_subject);
    let filename = if subject_slug.is_empty() {
        format!("{}_{}_email.md", date_prefix, sender_slug)
    } else {
        format!("{}_{}_{}.md", date_prefix, sender_slug, subject_slug)
    };
    let mut dest = output_dir.join(&filename);

    // Avoid overwriting
    if dest.exists() {
        let mut counter = 1;
        loop {
            let name = if subject_slug.is_empty() {
                format!("{}_{}_email-{}.md", date_prefix, sender_slug, counter)
            } else {
                format!("{}_{}_{}-{}.md", date_prefix, sender_slug, subject_slug, counter)
            };
            dest = output_dir.join(&name);
            if !dest.exists() {
                break;
            }
            counter += 1;
        }
    }

    fs::write(&dest, full_content)?;

    // Copy and wrap companion HTML for the draft if the original has one
    let source_html = source_path.with_extension("html");
    if source_html.exists() {
        if let Ok(html_content) = fs::read_to_string(&source_html) {
            let wrapped = format!(
                "<p style=\"color:#666\">On {}, {} wrote:</p>\n\
                 <div style=\"margin:0;padding:0 0 0 1em;border-left:2px solid #ccc\">\n\
                 {}\n\
                 </div>",
                inbox.date.as_deref().unwrap_or("(unknown date)"),
                inbox.from,
                html_content,
            );
            let draft_html = dest.with_extension("html");
            fs::write(&draft_html, wrapped)?;
        }
    }

    Ok(dest)
}

fn parse_email_date_prefix(date_str: &str) -> String {
    // Try parsing common email date formats to extract YYYY-MM-DD-HHMM
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(date_str) {
        return dt.format("%Y-%m-%d-%H%M").to_string();
    }
    // Fallback: use current datetime
    Utc::now().format("%Y-%m-%d-%H%M").to_string()
}

fn save_fetched_emails(emails: &[FetchedEmail], inbox_dir: &Path, status: &str) -> Result<(usize, usize)> {
    fs::create_dir_all(inbox_dir)?;

    // Collect existing message IDs from inbox
    let mut existing_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in WalkDir::new(inbox_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            if let Ok(content) = fs::read_to_string(path) {
                // Extract message_id from YAML frontmatter
                let mut in_frontmatter = false;
                for line in content.lines() {
                    if line == "---" {
                        if !in_frontmatter {
                            in_frontmatter = true;
                            continue;
                        } else {
                            break; // end of frontmatter
                        }
                    }
                    if in_frontmatter && line.starts_with("message_id:") {
                        let id = line.trim_start_matches("message_id:").trim().trim_matches('"');
                        if !id.is_empty() {
                            existing_ids.insert(id.to_string());
                        }
                        break;
                    }
                }
            }
        }
    }

    let mut saved = 0;
    let mut skipped = 0;

    for email in emails {
        // Skip duplicates by message_id
        if let Some(ref mid) = email.message_id {
            if existing_ids.contains(mid) {
                skipped += 1;
                continue;
            }
        }

        let date_prefix = parse_email_date_prefix(&email.date);
        let sender_slug = slugify_sender(&email.from);
        let subject_slug = slugify_subject(&email.subject);
        let filename = if subject_slug.is_empty() {
            format!("{}_{}_email.md", date_prefix, sender_slug)
        } else {
            format!("{}_{}_{}.md", date_prefix, sender_slug, subject_slug)
        };

        let mut dest = inbox_dir.join(&filename);
        // Avoid overwriting if filename collides
        if dest.exists() {
            let mut counter = 1;
            loop {
                let name = if subject_slug.is_empty() {
                    format!("{}_{}_email-{}.md", date_prefix, sender_slug, counter)
                } else {
                    format!("{}_{}_{}-{}.md", date_prefix, sender_slug, subject_slug, counter)
                };
                dest = inbox_dir.join(&name);
                if !dest.exists() {
                    break;
                }
                counter += 1;
            }
        }

        // Build frontmatter
        let fetched_at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut frontmatter = String::from("---\n");
        frontmatter.push_str(&format!("from: \"{}\"\n", email.from.replace('"', "\\\"")));
        frontmatter.push_str(&format!("to: \"{}\"\n", email.to.replace('"', "\\\"")));
        if let Some(ref cc) = email.cc {
            frontmatter.push_str(&format!("cc: \"{}\"\n", cc.replace('"', "\\\"")));
        }
        frontmatter.push_str(&format!("subject: \"{}\"\n", email.subject.replace('"', "\\\"")));
        frontmatter.push_str(&format!("date: \"{}\"\n", email.date.replace('"', "\\\"")));
        if let Some(ref mid) = email.message_id {
            frontmatter.push_str(&format!("message_id: \"{}\"\n", mid.replace('"', "\\\"")));
        }
        frontmatter.push_str(&format!("status: {}\n", status));
        frontmatter.push_str(&format!("has_attachments: {}\n", email.has_attachments));
        frontmatter.push_str(&format!("fetched_at: \"{}\"\n", fetched_at));
        frontmatter.push_str("---\n\n");

        let content = format!("{}{}", frontmatter, email.body_text);
        fs::write(&dest, content)?;

        // Save companion HTML file if available
        if let Some(ref html) = email.html_body {
            let html_path = dest.with_extension("html");
            fs::write(&html_path, html)?;
        }

        // Track the new message_id to prevent duplicates within the same batch
        if let Some(ref mid) = email.message_id {
            existing_ids.insert(mid.clone());
        }

        saved += 1;
    }

    Ok((saved, skipped))
}

/// Scan a directory for .md files and extract their Message-IDs from frontmatter.
/// Returns a map from message_id to file path.
fn scan_local_message_ids(dir: &Path) -> Result<std::collections::HashMap<String, PathBuf>> {
    let mut ids: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    if !dir.exists() {
        return Ok(ids);
    }
    for entry in WalkDir::new(dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            if let Ok(content) = fs::read_to_string(path) {
                let mut in_frontmatter = false;
                for line in content.lines() {
                    if line == "---" {
                        if !in_frontmatter {
                            in_frontmatter = true;
                            continue;
                        } else {
                            break;
                        }
                    }
                    if in_frontmatter && line.starts_with("message_id:") {
                        let id = line.trim_start_matches("message_id:").trim().trim_matches('"');
                        if !id.is_empty() {
                            ids.insert(id.to_string(), path.to_path_buf());
                        }
                        break;
                    }
                }
            }
        }
    }
    Ok(ids)
}

/// Move a local email file from one mailbox directory to another, updating its status.
fn move_local_email(
    file_path: &Path,
    target_dir: &Path,
    old_status: &str,
    new_status: &str,
) -> Result<()> {
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read {}", file_path.display()))?;

    let new_content = content.replacen(
        &format!("status: {}", old_status),
        &format!("status: {}", new_status),
        1,
    );

    let file_name = file_path
        .file_name()
        .ok_or_else(|| anyhow!("Invalid file path"))?;
    let dest = target_dir.join(file_name);

    fs::create_dir_all(target_dir)?;
    fs::write(&dest, new_content)?;
    fs::remove_file(file_path)?;

    // Move companion HTML file if it exists
    let html_source = file_path.with_extension("html");
    if html_source.exists() {
        let html_dest = dest.with_extension("html");
        fs::copy(&html_source, &html_dest)?;
        fs::remove_file(&html_source)?;
    }

    Ok(())
}

/// Reconcile local files against server-side Message-ID sets.
/// Moves files that changed mailbox on the server, deletes files no longer on any server mailbox.
/// Only operates on INBOX and Archive (Sent is skipped -- locally-authored files are source of truth).
fn reconcile_local_files(
    server_ids: &std::collections::HashMap<String, std::collections::HashSet<String>>,
    local_dirs: &std::collections::HashMap<String, PathBuf>,
) -> Result<(usize, usize)> {
    let mut moved = 0;
    let mut removed = 0;

    let status_for = |mb: &str| -> &str {
        match mb {
            "INBOX" => "inbox",
            "Archive" => "archived",
            _ => "inbox",
        }
    };

    // Only reconcile INBOX and Archive
    let reconcile_mailboxes = ["INBOX", "Archive"];

    for mb in &reconcile_mailboxes {
        let Some(local_dir) = local_dirs.get(*mb) else { continue };
        let Some(server_set) = server_ids.get(*mb) else { continue };

        let local_ids = scan_local_message_ids(local_dir)?;

        for (mid, file_path) in &local_ids {
            if server_set.contains(mid) {
                continue; // Still on server in this mailbox
            }

            // Message-ID not on server in this mailbox. Check other synced mailboxes.
            let mut found_in: Option<&str> = None;
            for other_mb in &reconcile_mailboxes {
                if *other_mb == *mb {
                    continue;
                }
                if let Some(other_set) = server_ids.get(*other_mb) {
                    if other_set.contains(mid) {
                        found_in = Some(other_mb);
                        break;
                    }
                }
            }

            if let Some(target_mb) = found_in {
                // Email was moved to another mailbox on the server
                let target_dir = &local_dirs[target_mb];

                // Check if a copy already exists in target (from the additive sync)
                let target_ids = scan_local_message_ids(target_dir)?;
                if target_ids.contains_key(mid) {
                    // Already present in target -- just delete the stale copy
                    fs::remove_file(file_path)?;
                    let html_companion = file_path.with_extension("html");
                    if html_companion.exists() {
                        fs::remove_file(&html_companion)?;
                    }
                    info!("Removed stale copy from {} (already in {}): {}", mb, target_mb, file_path.display());
                } else {
                    // Move to target mailbox
                    move_local_email(file_path, target_dir, status_for(mb), status_for(target_mb))?;
                    info!("Moved from {} to {}: {}", mb, target_mb, file_path.display());
                }
                moved += 1;
            } else {
                // Not found in any synced mailbox -- deleted on server
                fs::remove_file(file_path)?;
                let html_companion = file_path.with_extension("html");
                if html_companion.exists() {
                    fs::remove_file(&html_companion)?;
                }
                info!("Removed (no longer on server): {}", file_path.display());
                removed += 1;
            }
        }
    }

    Ok((moved, removed))
}

fn resolve_mailbox_dir(mailbox: &str) -> Result<PathBuf> {
    let env_var = match mailbox {
        "INBOX" => "INBOX_DIR",
        "Archive" => "ARCHIVE_DIR",
        "Sent" => "SENT_DIR",
        _ => {
            return Err(anyhow!(
                "Unsupported mailbox '{}'. Supported: INBOX, Archive, Sent",
                mailbox
            ))
        }
    };
    let raw = std::env::var(env_var)
        .with_context(|| format!("{} env var not set (needed for mailbox '{}')", env_var, mailbox))?;
    let expanded = shellexpand::tilde(raw.trim_matches('"').trim_matches('\'')).into_owned();
    Ok(PathBuf::from(expanded))
}

fn resolve_sent_mailbox() -> String {
    std::env::var("SENT_MAILBOX")
        .unwrap_or_else(|_| "Sent".to_string())
}

fn display_fetched_emails(emails: &[FetchedEmail], full_body: bool) {
    if emails.is_empty() {
        println!("No emails found matching the criteria.");
        return;
    }

    println!(
        "\n{} ({} result{})\n",
        "Fetched Emails".bold().cyan(),
        emails.len(),
        if emails.len() == 1 { "" } else { "s" }
    );

    for (i, email) in emails.iter().enumerate() {
        println!("{}", "─".repeat(60));
        println!("{}: {}", "From".bold().green(), email.from);
        println!("{}: {}", "To".bold().blue(), email.to);
        if let Some(ref cc) = email.cc {
            println!("{}: {}", "Cc".bold().blue(), cc);
        }
        println!("{}: {}", "Subject".bold().yellow(), email.subject);
        println!("{}: {}", "Date".bold().magenta(), email.date);
        if email.has_attachments {
            println!("{}", "[has attachments]".yellow());
        }

        println!();
        if full_body {
            println!("{}", email.body_text);
        } else {
            let preview: String = email.body_text.chars().take(300).collect();
            println!("{}", preview);
            if email.body_text.len() > 300 {
                println!("{}", "...".dimmed());
            }
        }

        if i < emails.len() - 1 {
            println!();
        }
    }
    println!("{}", "─".repeat(60));
}

/// Load email config from config.toml
fn load_email_config(path_hint: Option<&Path>) -> EmailConfig {
    // Normalize path hint by stripping trailing slashes to ensure parent() works correctly
    // Path::new("foo/bar/").parent() returns "foo/bar", not "foo"
    let normalized_hint: Option<PathBuf> = path_hint.map(|p| {
        let s = p.to_string_lossy();
        let trimmed = s.trim_end_matches('/').trim_end_matches('\\');
        PathBuf::from(trimmed)
    });
    let path_hint = normalized_hint.as_deref();

    // Build list of locations to search
    let mut config_locations: Vec<PathBuf> = Vec::new();

    // 1. Try relative to path hint (file or directory being processed)
    if let Some(hint) = path_hint {
        if let Some(parent) = hint.parent() {
            config_locations.push(parent.join("config.toml"));
            if let Some(grandparent) = parent.parent() {
                config_locations.push(grandparent.join("config.toml"));
                config_locations.push(grandparent.join("email/config.toml"));
                if let Some(great) = grandparent.parent() {
                    config_locations.push(great.join("config.toml"));
                    config_locations.push(great.join("email/config.toml"));
                }
            }
        }
    }

    // 2. Try relative to current working directory (for running from any location)
    if let Ok(cwd) = std::env::current_dir() {
        config_locations.push(cwd.join("config.toml"));
        config_locations.push(cwd.join("email/config.toml"));
        config_locations.push(cwd.join("../config.toml"));
        config_locations.push(cwd.join("../email/config.toml"));
        config_locations.push(cwd.join("../../config.toml"));
        config_locations.push(cwd.join("../../email/config.toml"));
    }

    for location in config_locations {
        if location.exists() {
            if let Ok(content) = fs::read_to_string(&location) {
                if let Ok(mut config) = toml::from_str::<EmailConfig>(&content) {
                    debug!("Loaded config from {}", location.display());
                    // Resolve relative signature paths to absolute
                    let config_dir = location.parent().unwrap_or(Path::new("."));
                    for entry in config.signatures.entries.values_mut() {
                        if !Path::new(&entry.path).is_absolute() {
                            entry.path = config_dir.join(&entry.path).to_string_lossy().to_string();
                        }
                    }
                    return config;
                }
            }
        }
    }

    debug!("No config.toml found, using defaults");
    EmailConfig::default()
}

/// Load signature HTML content
fn load_signature(config: &EmailConfig, signature_name: Option<&str>) -> Option<String> {
    let sig_name = signature_name
        .map(|s| s.to_string())
        .or_else(|| config.signatures.default.clone())?;

    let entry = config.signatures.entries.get(&sig_name)?;
    let path = Path::new(&entry.path);

    if path.exists() {
        fs::read_to_string(path).ok()
    } else {
        eprintln!("{} Signature file not found: {}", "⚠".yellow(), entry.path);
        None
    }
}

/// Try to load .env from multiple locations
fn load_env_from_path(path: Option<&Path>) {
    // First try standard dotenvy (current dir and parents)
    dotenvy::dotenv().ok();

    // Normalize path by stripping trailing slashes
    let normalized: Option<PathBuf> = path.map(|p| {
        let s = p.to_string_lossy();
        let trimmed = s.trim_end_matches('/').trim_end_matches('\\');
        PathBuf::from(trimmed)
    });
    let path = normalized.as_deref();

    // Build list of locations to search for .env
    let mut env_locations: Vec<PathBuf> = Vec::new();

    // 1. Try relative to path hint (file or directory being processed)
    if let Some(p) = path {
        if let Some(parent) = p.parent() {
            env_locations.push(parent.join(".env"));
            if let Some(grandparent) = parent.parent() {
                env_locations.push(grandparent.join(".env"));
                if let Some(great) = grandparent.parent() {
                    env_locations.push(great.join(".env"));
                }
            }
        }
    }

    // 2. Try relative to current working directory (for running from any location)
    if let Ok(cwd) = std::env::current_dir() {
        env_locations.push(cwd.join(".env"));
        env_locations.push(cwd.join("../.env"));
        env_locations.push(cwd.join("../../.env"));
    }

    // Load from all found .env files (later ones override earlier)
    for env_path in env_locations {
        if env_path.exists() {
            dotenvy::from_path(&env_path).ok();
        }
    }
}

impl SmtpConfig {
    fn from_env(path_hint: Option<&Path>) -> Result<Self> {
        load_env_from_path(path_hint);

        Ok(Self {
            host: std::env::var("SMTP_HOST").context("SMTP_HOST not set in environment")?,
            port: std::env::var("SMTP_PORT")
                .unwrap_or_else(|_| "587".to_string())
                .parse()
                .context("Invalid SMTP_PORT")?,
            username: std::env::var("SMTP_USERNAME")
                .context("SMTP_USERNAME not set in environment")?,
            password: std::env::var("SMTP_PASSWORD")
                .context("SMTP_PASSWORD not set in environment")?,
            default_from: std::env::var("DEFAULT_FROM")
                .context("DEFAULT_FROM not set in environment")?,
        })
    }
}

fn markdown_to_html(
    markdown: &str,
    config: &EmailConfig,
    signature: Option<&str>,
    quoted_html: Option<&str>,
) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);

    let parser = MdParser::new_ext(markdown, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    // Handle signature placement:
    // If {{SIGNATURE}} placeholder is present (reply drafts), split the HTML at the placeholder,
    // inject the signature, and wrap the quoted content in a styled div.
    // Replace <blockquote> with styled <div> in the quoted section to prevent email clients
    // (Apple Mail, Gmail) from collapsing the signature and conversation behind "see more".
    // Otherwise (regular drafts), append signature at the end.
    let signature_html = signature.unwrap_or_default();
    let body = if html_output.contains("{{SIGNATURE}}") {
        // pulldown-cmark wraps the placeholder in <p> tags; match that form first
        let marker = if html_output.contains("<p>{{SIGNATURE}}</p>") {
            "<p>{{SIGNATURE}}</p>"
        } else {
            "{{SIGNATURE}}"
        };
        let parts: Vec<&str> = html_output.splitn(2, marker).collect();
        let reply_part = parts[0];

        if let Some(original_html) = quoted_html {
            // Use original HTML instead of Markdown-converted blockquotes
            format!(
                "{}\n{}\n<div style=\"padding-top:1em\">\n{}\n</div>",
                reply_part.trim_end(),
                signature_html,
                original_html,
            )
        } else {
            // Fallback: convert Markdown blockquotes to styled divs
            let quoted_part = if parts.len() > 1 { parts[1] } else { "" };
            let quoted_styled = quoted_part
                .replace("<blockquote>", "<div style=\"margin:0;padding:0 0 0 1em;border-left:2px solid #ccc\">")
                .replace("</blockquote>", "</div>");
            format!(
                "{}\n{}\n<div style=\"padding-top:1em\">\n{}\n</div>",
                reply_part.trim_end(),
                signature_html,
                quoted_styled.trim_start()
            )
        }
    } else {
        format!("{}\n{}", html_output, signature_html)
    };

    // Wrap in basic HTML structure with styling from config
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8">
<style>
body {{ font-family: {font_family}; font-size: {font_size}; line-height: 1.6; color: #000; }}
a {{ color: #0066cc; }}
p {{ margin: 0 0 1em 0; }}
blockquote {{ margin: 0.5em 0; padding: 0 0 0 1em; border-left: 2px solid #ccc; white-space: pre-wrap; }}
</style>
</head>
<body>
{body}
</body>
</html>"#,
        font_family = config.email.font_family,
        font_size = config.email.font_size,
        body = body,
    )
}

fn parse_email_draft(path: &Path) -> Result<EmailDraft> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);

    let frontmatter: EmailFrontmatter = parsed
        .data
        .ok_or_else(|| anyhow!("No frontmatter found in file"))?
        .deserialize()
        .context("Failed to parse frontmatter")?;

    let body_markdown = parsed.content.trim().to_string();

    Ok(EmailDraft {
        path: path.to_path_buf(),
        frontmatter,
        body_markdown,
    })
}

fn validate_draft(draft: &EmailDraft) -> Result<Vec<String>> {
    let mut warnings = Vec::new();

    // Check required fields
    if draft.frontmatter.to.is_empty() {
        return Err(anyhow!("Missing 'to' field"));
    }

    if draft.frontmatter.subject.is_empty() {
        return Err(anyhow!("Missing 'subject' field"));
    }

    if draft.body_markdown.is_empty() {
        warnings.push("Email body is empty".to_string());
    }

    // Validate email format (basic check)
    for email in draft.frontmatter.to.split(',') {
        let email = email.trim();
        if !email.contains('@') || !email.contains('.') {
            return Err(anyhow!("Invalid email address: {}", email));
        }
    }

    // Check attachments exist
    if let Some(attachments) = &draft.frontmatter.attachments {
        for attachment in attachments {
            let expanded = shellexpand::tilde(attachment);
            if !Path::new(expanded.as_ref()).exists() {
                warnings.push(format!("Attachment not found: {}", attachment));
            }
        }
    }

    Ok(warnings)
}

fn preview_draft(
    draft: &EmailDraft,
    smtp_config: &SmtpConfig,
    email_config: &EmailConfig,
    signature: Option<&str>,
    is_dry_run: bool,
) -> Result<()> {
    println!("\n{}", "=== Email Draft Preview ===".bold().cyan());
    println!(
        "{}: {}",
        "From".bold(),
        draft
            .frontmatter
            .from
            .as_ref()
            .unwrap_or(&smtp_config.default_from)
    );
    println!("{}: {}", "To".bold(), draft.frontmatter.to);

    if let Some(cc) = &draft.frontmatter.cc {
        println!("{}: {}", "Cc".bold(), cc);
    }

    if let Some(bcc) = &draft.frontmatter.bcc {
        println!("{}: {}", "Bcc".bold(), bcc);
    }

    println!("{}: {}", "Subject".bold(), draft.frontmatter.subject);

    println!("\n{}\n", "--- Body Preview (first 500 chars) ---".dimmed());
    let preview: String = draft.body_markdown.chars().take(500).collect();
    println!("{}", preview);
    if draft.body_markdown.len() > 500 {
        println!("{}", "...".dimmed());
    }

    println!("\n{}", "--- Settings ---".dimmed());
    println!(
        "  Font: {} ({})",
        email_config.email.font_family, email_config.email.font_size
    );
    if let Some(sig) = signature {
        let sig_preview: String = sig.chars().take(50).collect();
        println!("  Signature: {} ...", sig_preview.replace('\n', " "));
    } else {
        println!("  Signature: {}", "none".dimmed());
    }

    println!("\n{}", "--- Status ---".dimmed());

    // Validate and show status
    match validate_draft(draft) {
        Ok(warnings) => {
            println!("{} Valid YAML frontmatter", "✓".green());
            println!(
                "{} Status: {}",
                "✓".green(),
                format!("{}", draft.frontmatter.status).yellow()
            );
            println!("{} All required fields present", "✓".green());

            for warning in warnings {
                println!("{} {}", "⚠".yellow(), warning);
            }
        }
        Err(e) => {
            println!("{} Validation error: {}", "✗".red(), e);
        }
    }

    if is_dry_run {
        println!(
            "\n{}\n",
            "[DRY RUN] Would send email (use 'send' subcommand to actually send)"
                .yellow()
                .bold()
        );
    }

    Ok(())
}

async fn send_email(
    draft: &EmailDraft,
    smtp_config: &SmtpConfig,
    email_config: &EmailConfig,
    signature: Option<&str>,
) -> Result<(SendResult, Vec<u8>, Option<String>)> {
    // Check status
    if draft.frontmatter.status != EmailStatus::Approved {
        return Err(anyhow!(
            "Email not approved for sending. Current status: {}",
            draft.frontmatter.status
        ));
    }

    let from_address = draft
        .frontmatter
        .from
        .as_ref()
        .unwrap_or(&smtp_config.default_from);

    let from_mailbox: Mailbox = from_address
        .parse()
        .context("Invalid 'from' email address")?;

    info!(
        "Sending email: subject=\"{}\", from={}",
        draft.frontmatter.subject, from_address
    );

    // Collect all recipients with roles, deduplicating by address
    let mut seen = HashSet::new();
    let mut recipients: Vec<(String, RecipientRole)> = Vec::new();

    for to in draft.frontmatter.to.split(',') {
        let addr = to.trim().to_string();
        if !addr.is_empty() && seen.insert(addr.to_lowercase()) {
            recipients.push((addr, RecipientRole::To));
        }
    }
    if let Some(cc) = &draft.frontmatter.cc {
        for cc_addr in cc.split(',') {
            let addr = cc_addr.trim().to_string();
            if !addr.is_empty() && seen.insert(addr.to_lowercase()) {
                recipients.push((addr, RecipientRole::Cc));
            }
        }
    }
    if let Some(bcc) = &draft.frontmatter.bcc {
        for bcc_addr in bcc.split(',') {
            let addr = bcc_addr.trim().to_string();
            if !addr.is_empty() && seen.insert(addr.to_lowercase()) {
                recipients.push((addr, RecipientRole::Bcc));
            }
        }
    }

    if recipients.is_empty() {
        return Err(anyhow!("No recipients specified"));
    }

    debug!(
        "Recipients ({}): {:?}",
        recipients.len(),
        recipients
            .iter()
            .map(|(a, r)| format!("{}({})", a, r))
            .collect::<Vec<_>>()
    );

    // Build the message with visible To/Cc headers (Bcc omitted from headers by lettre)
    // message_id(None) triggers auto-generation of a unique Message-ID header
    let mut builder = Message::builder()
        .from(from_mailbox.clone())
        .subject(&draft.frontmatter.subject)
        .message_id(None);

    // Add To recipients to headers
    for (addr, role) in &recipients {
        match role {
            RecipientRole::To => {
                let mbox: Mailbox = addr.parse().context("Invalid 'to' email address")?;
                builder = builder.to(mbox);
            }
            RecipientRole::Cc => {
                let mbox: Mailbox = addr.parse().context("Invalid 'cc' email address")?;
                builder = builder.cc(mbox);
            }
            RecipientRole::Bcc => {
                let mbox: Mailbox = addr.parse().context("Invalid 'bcc' email address")?;
                builder = builder.bcc(mbox);
            }
        }
    }

    // Add Reply-To
    if let Some(reply_to) = &draft.frontmatter.reply_to {
        let reply_mailbox: Mailbox = reply_to
            .parse()
            .context("Invalid 'reply_to' email address")?;
        builder = builder.reply_to(reply_mailbox);
    }

    // Load companion HTML for quoted section if available
    let html_companion_path = draft.path.with_extension("html");
    let quoted_html = if html_companion_path.exists() {
        fs::read_to_string(&html_companion_path).ok()
    } else {
        None
    };

    // Generate HTML with signature (and original HTML for quoted section if available)
    let body_html = markdown_to_html(
        &draft.body_markdown,
        email_config,
        signature,
        quoted_html.as_deref(),
    );

    // Build the text/html alternative part
    let body_multipart = MultiPart::alternative()
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(draft.body_markdown.clone()),
        )
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(body_html),
        );

    // Build message with or without attachments
    let message = if let Some(attachments) = &draft.frontmatter.attachments {
        if !attachments.is_empty() {
            let mut mixed = MultiPart::mixed().multipart(body_multipart);

            for attachment_path in attachments {
                let expanded = shellexpand::tilde(attachment_path);
                let path = Path::new(expanded.as_ref());

                let file_content = fs::read(path)
                    .with_context(|| format!("Failed to read attachment: {}", attachment_path))?;

                let filename = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "attachment".to_string());

                let content_type = mime_guess::from_path(path)
                    .first_or_octet_stream()
                    .to_string();

                let attachment = Attachment::new(filename).body(file_content, content_type.parse().unwrap());
                mixed = mixed.singlepart(attachment);
            }

            builder.multipart(mixed).context("Failed to build email message")?
        } else {
            builder.multipart(body_multipart).context("Failed to build email message")?
        }
    } else {
        builder.multipart(body_multipart).context("Failed to build email message")?
    };

    // Get raw message bytes for send_raw and IMAP APPEND
    let raw_message = message.formatted();

    // Extract Message-ID from the raw message headers
    let message_id = mailparse::parse_headers(&raw_message)
        .ok()
        .and_then(|(headers, _)| {
            headers.iter()
                .find(|h| h.get_key().eq_ignore_ascii_case("Message-ID"))
                .map(|h| h.get_value().trim().to_string())
        });

    // Parse from address for envelope
    let from_addr: lettre::Address = from_address
        .parse::<Mailbox>()
        .context("Invalid 'from' address")?
        .email;

    // Create SMTP transport
    let creds = Credentials::new(smtp_config.username.clone(), smtp_config.password.clone());

    let mailer: AsyncSmtpTransport<Tokio1Executor> =
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp_config.host)?
            .port(smtp_config.port)
            .credentials(creds)
            .build();

    // Send to each recipient individually
    let mut results = Vec::with_capacity(recipients.len());

    for (addr, role) in &recipients {
        let rcpt_addr: lettre::Address = match addr.parse::<Mailbox>() {
            Ok(mbox) => mbox.email,
            Err(e) => {
                let err_msg = format!("Invalid address '{}': {}", addr, e);
                error!("{}", err_msg);
                results.push(RecipientResult {
                    address: addr.clone(),
                    role: *role,
                    success: false,
                    error: Some(err_msg),
                });
                continue;
            }
        };

        let envelope = match Envelope::new(Some(from_addr.clone()), vec![rcpt_addr]) {
            Ok(env) => env,
            Err(e) => {
                let err_msg = format!("Failed to create envelope for '{}': {}", addr, e);
                error!("{}", err_msg);
                results.push(RecipientResult {
                    address: addr.clone(),
                    role: *role,
                    success: false,
                    error: Some(err_msg),
                });
                continue;
            }
        };

        match mailer.send_raw(&envelope, &raw_message).await {
            Ok(_) => {
                info!("Sent to {} ({})", addr, role);
                results.push(RecipientResult {
                    address: addr.clone(),
                    role: *role,
                    success: true,
                    error: None,
                });
            }
            Err(e) => {
                let err_msg = format!("{}", e);
                error!("Failed to send to {} ({}): {}", addr, role, err_msg);
                results.push(RecipientResult {
                    address: addr.clone(),
                    role: *role,
                    success: false,
                    error: Some(err_msg),
                });
            }
        }
    }

    Ok((SendResult { results }, raw_message, message_id))
}

fn update_status_to_sent(draft: &EmailDraft, sent_dir: Option<&Path>, message_id: Option<&str>) -> Result<()> {
    info!("Updating status to sent: {}", draft.path.display());
    let mut frontmatter = draft.frontmatter.clone();
    frontmatter.status = EmailStatus::Sent;
    frontmatter.sent_at = Some(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    frontmatter.sent_via = Some(format!("email-cli v{}", env!("CARGO_PKG_VERSION")));
    frontmatter.message_id = message_id.map(|s| s.to_string());

    // Serialize the updated frontmatter
    let yaml = serde_yaml::to_string(&frontmatter)?;

    // Reconstruct the file content
    let new_content = format!("---\n{}---\n\n{}", yaml, draft.body_markdown);

    // Determine destination path
    let dest_path = if let Some(sent_dir) = sent_dir {
        fs::create_dir_all(sent_dir)?;
        // Keep the original filename (already includes date-time prefix)
        let original_name = draft.path.file_name().unwrap().to_string_lossy();
        sent_dir.join(original_name.as_ref())
    } else {
        draft.path.clone()
    };

    // Write the updated content
    fs::write(&dest_path, new_content)?;

    // If we moved the file, remove the original
    if sent_dir.is_some() && dest_path != draft.path {
        fs::remove_file(&draft.path)?;
    }

    // Clean up companion HTML file (not needed in sent/)
    let html_companion = draft.path.with_extension("html");
    if html_companion.exists() {
        fs::remove_file(&html_companion).ok();
    }

    Ok(())
}

/// Resolve the drafts directory using a fallback chain:
/// 1. If the user passed an explicit (non-default) path, use it as-is.
/// 2. Else if DRAFTS_DIR env var is set and points to an existing directory, use that.
/// 3. Else fall back to "." (current directory).
fn resolve_drafts_dir(cli_dir: &Path) -> PathBuf {
    if cli_dir != Path::new(".") {
        return cli_dir.to_path_buf();
    }
    if let Ok(env_dir) = std::env::var("DRAFTS_DIR") {
        let s = env_dir.trim_matches('"').trim_matches('\'');
        let expanded = PathBuf::from(shellexpand::tilde(s).into_owned());
        if expanded.is_dir() {
            return expanded;
        }
    }
    cli_dir.to_path_buf()
}

fn find_drafts(dir: &Path, status_filter: Option<EmailStatus>) -> Result<Vec<EmailDraft>> {
    let mut drafts = Vec::new();

    for entry in WalkDir::new(dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            match parse_email_draft(path) {
                Ok(draft) => {
                    if let Some(ref filter) = status_filter {
                        if &draft.frontmatter.status == filter {
                            drafts.push(draft);
                        }
                    } else {
                        drafts.push(draft);
                    }
                }
                Err(e) => {
                    eprintln!("{} Skipping {}: {}", "⚠".yellow(), path.display(), e);
                }
            }
        }
    }

    Ok(drafts)
}

fn prompt_confirmation(message: &str) -> bool {
    print!("{} [y/N] ", message);
    io::stdout().flush().unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();

    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

fn mark_as_approved(path: &Path) -> Result<()> {
    let draft = parse_email_draft(path)?;

    if draft.frontmatter.status == EmailStatus::Approved {
        println!("{} Already approved: {}", "ℹ".blue(), path.display());
        return Ok(());
    }

    if draft.frontmatter.status == EmailStatus::Sent {
        return Err(anyhow!("Cannot approve an already sent email"));
    }

    let mut frontmatter = draft.frontmatter.clone();
    frontmatter.status = EmailStatus::Approved;

    let yaml = serde_yaml::to_string(&frontmatter)?;
    let new_content = format!("---\n{}---\n\n{}", yaml, draft.body_markdown);

    fs::write(path, new_content)?;

    println!("{} Marked as approved: {}", "✓".green(), path.display());

    Ok(())
}

// --- Browse command support ---

#[derive(Debug, Clone, Copy, PartialEq)]
enum DirType {
    Inbox,
    Drafts,
    Sent,
    Archive,
}

impl std::fmt::Display for DirType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirType::Inbox => write!(f, "INBOX"),
            DirType::Drafts => write!(f, "DRAFTS"),
            DirType::Sent => write!(f, "SENT"),
            DirType::Archive => write!(f, "ARCHIVE"),
        }
    }
}

struct EmailSkimItem {
    display_text: String,
    file_path: PathBuf,
    preview_text: String,
}

impl SkimItem for EmailSkimItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.display_text)
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Owned(self.file_path.to_string_lossy().into_owned())
    }

    fn preview(&self, _context: PreviewContext) -> ItemPreview {
        ItemPreview::AnsiText(self.preview_text.clone())
    }
}

struct ColoredLine {
    text: String,
    fragments: Vec<(Attr, (u32, u32))>,
}

impl ColoredLine {
    fn new() -> Self {
        Self {
            text: String::new(),
            fragments: Vec::new(),
        }
    }

    fn push(&mut self, s: &str, attr: Attr) -> &mut Self {
        let start = self.text.chars().count() as u32;
        self.text.push_str(s);
        let end = self.text.chars().count() as u32;
        self.fragments.push((attr, (start, end)));
        self
    }

    fn into_ansi_string(self) -> AnsiString<'static> {
        AnsiString::new_string(self.text, self.fragments)
    }
}

struct HeaderSkimItem {
    stripped: String,
    ansi: AnsiString<'static>,
}

impl SkimItem for HeaderSkimItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.stripped)
    }

    fn display<'a>(&'a self, _context: DisplayContext<'a>) -> AnsiString<'a> {
        self.ansi.clone()
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn preview(&self, _context: PreviewContext) -> ItemPreview {
        ItemPreview::Text(String::new())
    }
}

fn build_browse_items(dir: &Path, dir_type: DirType) -> Vec<Arc<dyn SkimItem>> {
    let mut items: Vec<(String, Arc<dyn SkimItem>)> = Vec::new();

    for entry in WalkDir::new(dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() || path.extension().map_or(true, |ext| ext != "md") {
            continue;
        }

        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let matter = Matter::<YAML>::new();
        let parsed = matter.parse(&content);
        let data = match parsed.data {
            Some(d) => d,
            None => continue,
        };

        let yaml: serde_yaml::Value = match data.deserialize() {
            Ok(v) => v,
            Err(_) => continue,
        };

        let get_str = |key: &str| -> String {
            yaml.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        let subject = get_str("subject");
        let status = get_str("status");

        let contact = match dir_type {
            DirType::Inbox | DirType::Archive => {
                let from = get_str("from");
                extract_email_address(&from)
            }
            DirType::Drafts | DirType::Sent => {
                let to = get_str("to");
                extract_email_address(&to)
            }
        };

        let date_display = if dir_type == DirType::Inbox || dir_type == DirType::Archive {
            let date_str = get_str("date");
            chrono::DateTime::parse_from_rfc2822(&date_str)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|_| "unknown".to_string())
        } else {
            let sent_at = get_str("sent_at");
            if !sent_at.is_empty() {
                chrono::DateTime::parse_from_rfc3339(&sent_at)
                    .map(|dt| dt.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|_| "unknown".to_string())
            } else {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.get(..10))
                    .unwrap_or("unknown")
                    .to_string()
            }
        };

        let display_text = format!(
            "{}  {:<30}  {:<40}  [{}]",
            date_display,
            if contact.len() > 30 { let mut i = 30; while !contact.is_char_boundary(i) { i -= 1; } &contact[..i] } else { &contact },
            if subject.len() > 40 { let mut i = 40; while !subject.is_char_boundary(i) { i -= 1; } &subject[..i] } else { &subject },
            status
        );

        // Catppuccin Mocha ANSI colors for preview header fields
        let green = "\x1b[1;38;2;166;227;161m";   // #a6e3a1 — From
        let blue = "\x1b[1;38;2;137;180;250m";    // #89b4fa — To/Cc
        let yellow = "\x1b[1;38;2;249;226;175m";  // #f9e2af — Subject
        let magenta = "\x1b[1;38;2;203;166;247m"; // #cba6f7 — Date
        let teal = "\x1b[1;38;2;148;226;213m";    // #94e2d5 — Status
        let dim = "\x1b[38;2;108;112;134m";        // #6c7086 — separator
        let reset = "\x1b[0m";

        let mut preview_lines = Vec::new();
        if dir_type == DirType::Inbox || dir_type == DirType::Archive {
            preview_lines.push(format!("{green}From:{reset} {}", get_str("from")));
            preview_lines.push(format!("{blue}To:{reset} {}", get_str("to")));
            let cc = get_str("cc");
            if !cc.is_empty() {
                preview_lines.push(format!("{blue}Cc:{reset} {}", cc));
            }
        } else {
            preview_lines.push(format!("{blue}To:{reset} {}", get_str("to")));
            let cc = get_str("cc");
            if !cc.is_empty() {
                preview_lines.push(format!("{blue}Cc:{reset} {}", cc));
            }
            let from = get_str("from");
            if !from.is_empty() {
                preview_lines.push(format!("{green}From:{reset} {}", from));
            }
        }
        preview_lines.push(format!("{yellow}Subject:{reset} {}", subject));
        if dir_type == DirType::Inbox || dir_type == DirType::Archive {
            preview_lines.push(format!("{magenta}Date:{reset} {}", get_str("date")));
        }
        preview_lines.push(format!("{teal}Status:{reset} {}", status));
        preview_lines.push(String::new());
        preview_lines.push(format!("{dim}{}{reset}", "─".repeat(60)));
        preview_lines.push(String::new());

        let body = parsed.content.trim().to_string();
        preview_lines.push(body);

        let preview_text = preview_lines.join("\n");
        let sort_key = date_display.clone();

        items.push((sort_key, Arc::new(EmailSkimItem {
            display_text,
            file_path: path.to_path_buf(),
            preview_text,
        })));
    }

    items.sort_by(|a, b| b.0.cmp(&a.0));
    items.into_iter().map(|(_, item)| item).collect()
}

fn resolve_browse_dir(dir_type: DirType) -> Result<PathBuf> {
    let dir = match dir_type {
        DirType::Inbox => std::env::var("INBOX_DIR")
            .map(|s| {
                let s = s.trim_matches('"').trim_matches('\'');
                PathBuf::from(shellexpand::tilde(s).into_owned())
            })
            .unwrap_or_else(|_| PathBuf::from(".")),
        DirType::Drafts => resolve_drafts_dir(Path::new(".")),
        DirType::Sent => std::env::var("SENT_DIR")
            .map(|s| {
                let s = s.trim_matches('"').trim_matches('\'');
                PathBuf::from(shellexpand::tilde(s).into_owned())
            })
            .unwrap_or_else(|_| PathBuf::from(".")),
        DirType::Archive => std::env::var("ARCHIVE_DIR")
            .map(|s| {
                let s = s.trim_matches('"').trim_matches('\'');
                PathBuf::from(shellexpand::tilde(s).into_owned())
            })
            .unwrap_or_else(|_| PathBuf::from(".")),
    };
    Ok(dir)
}

fn next_dir_type(current: DirType) -> DirType {
    match current {
        DirType::Inbox => DirType::Drafts,
        DirType::Drafts => DirType::Sent,
        DirType::Sent => DirType::Archive,
        DirType::Archive => DirType::Inbox,
    }
}

fn build_browse_header(dir_type: DirType, count: usize) -> Vec<Arc<dyn SkimItem>> {
    // Catppuccin Mocha palette
    let mauve = Attr { fg: Color::Rgb(203, 166, 247), ..Attr::default() };   // #cba6f7 — mailbox title
    let peach = Attr { fg: Color::Rgb(250, 179, 135), ..Attr::default() };    // #fab387 — mailbox-specific keys
    let teal = Attr { fg: Color::Rgb(148, 226, 213), ..Attr::default() };     // #94e2d5 — global keys
    let subtext = Attr { fg: Color::Rgb(166, 173, 200), ..Attr::default() };  // #a6adc8 — labels
    let overlay = Attr { fg: Color::Rgb(108, 112, 134), ..Attr::default() };  // #6c7086 — separators

    let mut l1 = ColoredLine::new();
    l1.push(&format!("[{}]", dir_type), mauve);
    l1.push(&format!(" {} emails", count), subtext);

    match dir_type {
        DirType::Inbox => {
            l1.push(" | ", overlay);
            l1.push("^y", peach).push(":reply  ", subtext);
            l1.push("^o", peach).push(":reply-all  ", subtext);
            l1.push("^s", peach).push(":archive", subtext);
        }
        DirType::Drafts => {
            l1.push(" | ", overlay);
            l1.push("^g", peach).push(":approve  ", subtext);
            l1.push("^x", peach).push(":send", subtext);
        }
        _ => {}
    }

    let mut l2 = ColoredLine::new();
    l2.push("⏎", teal).push(":open  ", subtext);
    l2.push("tab", teal).push(":next  ", subtext);
    l2.push("^r", teal).push(":refresh  ", subtext);
    l2.push("^d", teal).push(":delete  ", subtext);
    l2.push("^p", teal).push(":copy-path  ", subtext);
    l2.push("esc", teal).push(":quit", subtext);

    vec![
        Arc::new(HeaderSkimItem {
            stripped: l1.text.clone(),
            ansi: l1.into_ansi_string(),
        }),
        Arc::new(HeaderSkimItem {
            stripped: l2.text.clone(),
            ansi: l2.into_ansi_string(),
        }),
    ]
}

fn refresh_inbox() -> Result<()> {
    println!("{} Fetching new emails...", "ℹ".blue());

    let imap_config = ImapConfig::from_env(None)?;
    let criteria = FetchCriteria {
        from: None,
        to: None,
        cc: None,
        subject: None,
        body: None,
        since: None,
        before: None,
    };

    let emails = fetch_emails(&imap_config, &criteria, "INBOX", Some(20))?;

    let inbox_dir = std::env::var("INBOX_DIR")
        .map(|s| {
            let s = s.trim_matches('"').trim_matches('\'');
            PathBuf::from(shellexpand::tilde(s).into_owned())
        })
        .unwrap_or_else(|_| PathBuf::from("."));

    let (saved, _skipped) = save_fetched_emails(&emails, &inbox_dir, "inbox")?;
    println!("{} Fetched {} new emails", "✓".green(), saved);

    Ok(())
}

fn archive_email_on_server(message_id: &str) -> Result<()> {
    info!("Archiving email on server: Message-ID={}", message_id);
    let imap_config = ImapConfig::from_env(None)?;
    let tls = native_tls::TlsConnector::builder().build()?;
    let client = imap::connect(
        (imap_config.host.as_str(), imap_config.port),
        &imap_config.host,
        &tls,
    )
    .map_err(|e| anyhow!("Failed to connect to IMAP server: {}", e))?;

    let mut session = client
        .login(&imap_config.username, &imap_config.password)
        .map_err(|e| anyhow!("IMAP login failed: {}", e.0))?;

    session
        .select("INBOX")
        .map_err(|e| anyhow!("Failed to select INBOX: {}", e))?;

    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        session.logout().ok();
        return Err(anyhow!(
            "Email with Message-ID {} not found in INBOX on server",
            message_id
        ));
    }

    let uid = *uids.iter().next().unwrap();
    let uid_str = uid.to_string();

    session
        .uid_copy(&uid_str, "Archive")
        .map_err(|e| anyhow!("Failed to copy email to Archive: {}", e))?;

    session
        .uid_store(&uid_str, "+FLAGS (\\Deleted)")
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?;

    session
        .expunge()
        .map_err(|e| anyhow!("Failed to expunge: {}", e))?;

    session.logout().ok();
    Ok(())
}

fn archive_email_locally(file_path: &Path) -> Result<()> {
    info!("Archiving email locally: {}", file_path.display());
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);
    let inbox_fm: InboxFrontmatter = parsed
        .data
        .ok_or_else(|| anyhow!("No frontmatter found in {}", file_path.display()))?
        .deserialize()
        .context("Failed to parse frontmatter")?;

    // Archive on server using message_id
    if let Some(ref mid) = inbox_fm.message_id {
        archive_email_on_server(mid)?;
    } else {
        return Err(anyhow!(
            "No message_id in frontmatter — cannot archive on server"
        ));
    }

    // Determine archive directory
    let archive_dir = std::env::var("ARCHIVE_DIR")
        .map(|s| {
            let s = s.trim_matches('"').trim_matches('\'');
            PathBuf::from(shellexpand::tilde(s).into_owned())
        })
        .context("ARCHIVE_DIR not set in .env")?;

    fs::create_dir_all(&archive_dir)?;

    // Update status from inbox to archived in the content
    let new_content = content.replacen("status: inbox", "status: archived", 1);

    // Move the file to archive dir
    let file_name = file_path
        .file_name()
        .ok_or_else(|| anyhow!("Invalid file path"))?;
    let dest = archive_dir.join(file_name);

    fs::write(&dest, new_content)?;
    fs::remove_file(file_path)?;

    // Move companion HTML file if it exists
    let html_companion = file_path.with_extension("html");
    if html_companion.exists() {
        let html_dest = dest.with_extension("html");
        fs::copy(&html_companion, &html_dest)?;
        fs::remove_file(&html_companion)?;
    }

    Ok(())
}

fn delete_email_on_server(message_id: &str) -> Result<()> {
    info!("Deleting email on server: Message-ID={}", message_id);
    let imap_config = ImapConfig::from_env(None)?;
    let tls = native_tls::TlsConnector::builder().build()?;
    let client = imap::connect(
        (imap_config.host.as_str(), imap_config.port),
        &imap_config.host,
        &tls,
    )
    .map_err(|e| anyhow!("Failed to connect to IMAP server: {}", e))?;

    let mut session = client
        .login(&imap_config.username, &imap_config.password)
        .map_err(|e| anyhow!("IMAP login failed: {}", e.0))?;

    session
        .select("INBOX")
        .map_err(|e| anyhow!("Failed to select INBOX: {}", e))?;

    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        session.logout().ok();
        return Err(anyhow!(
            "Email with Message-ID {} not found in INBOX on server",
            message_id
        ));
    }

    let uid = *uids.iter().next().unwrap();
    let uid_str = uid.to_string();

    session
        .uid_store(&uid_str, "+FLAGS (\\Deleted)")
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?;

    session
        .expunge()
        .map_err(|e| anyhow!("Failed to expunge: {}", e))?;

    session.logout().ok();
    Ok(())
}

fn delete_email_locally(file_path: &Path) -> Result<()> {
    info!("Deleting email locally: {}", file_path.display());
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);
    let inbox_fm: InboxFrontmatter = parsed
        .data
        .ok_or_else(|| anyhow!("No frontmatter found in {}", file_path.display()))?
        .deserialize()
        .context("Failed to parse frontmatter")?;

    if let Some(ref mid) = inbox_fm.message_id {
        delete_email_on_server(mid)?;
    } else {
        return Err(anyhow!(
            "No message_id in frontmatter — cannot delete on server"
        ));
    }

    fs::remove_file(file_path)
        .with_context(|| format!("Failed to remove local file: {}", file_path.display()))?;

    // Remove companion HTML file if it exists
    let html_companion = file_path.with_extension("html");
    if html_companion.exists() {
        fs::remove_file(&html_companion).ok();
    }

    Ok(())
}

fn browse_emails(starting_view: &str) -> Result<()> {
    let _ = dotenvy::dotenv();
    load_env_from_path(None);

    let mut current_dir_type = match starting_view {
        "drafts" => DirType::Drafts,
        "sent" => DirType::Sent,
        "archive" => DirType::Archive,
        _ => DirType::Inbox,
    };

    loop {
        let dir = resolve_browse_dir(current_dir_type)?;
        let items = build_browse_items(&dir, current_dir_type);

        let count = items.len();
        let header_items = build_browse_header(current_dir_type, count);
        let num_header_lines = header_items.len();

        let mut bindings: Vec<String> = vec![
            "esc:abort".to_string(),
            "tab:accept".to_string(),
            "ctrl-r:accept".to_string(),
            "ctrl-d:accept".to_string(),
            "ctrl-p:accept".to_string(),
        ];

        match current_dir_type {
            DirType::Inbox => {
                bindings.push("enter:execute(hx {})+accept".to_string());
                bindings.push("ctrl-y:accept".to_string());
                bindings.push("ctrl-o:accept".to_string());
                bindings.push("ctrl-s:accept".to_string());
            }
            DirType::Drafts => {
                bindings.push("enter:execute(hx {})".to_string());
                bindings.push("ctrl-g:accept".to_string());
                bindings.push("ctrl-x:execute(email send {})".to_string());
            }
            DirType::Sent => {
                bindings.push("enter:execute(hx {})".to_string());
            }
            DirType::Archive => {
                bindings.push("enter:execute(hx {})".to_string());
            }
        }

        let options = SkimOptionsBuilder::default()
            .ansi(true)
            .height(String::from("100%"))
            .multi(false)
            .preview(Some(String::new()))
            .preview_window(String::from("right:50%"))
            .header_lines(num_header_lines)
            .bind(bindings)
            .color(Some(String::from(
                "fg:#cdd6f4,bg:#1e1e2e,hl:#cba6f7,fg+:#a6e3a1,bg+:#313244,hl+:#fab387,\
                 query:#cdd6f4,prompt:#cba6f7,pointer:#f5e0dc,marker:#f5e0dc,\
                 header:#a6adc8,border:#585b70,info:#bac2de,spinner:#cba6f7"
            )))
            .build()
            .map_err(|e| anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for item in header_items {
            let _ = tx.send(item);
        }
        for item in items {
            let _ = tx.send(item);
        }
        drop(tx);

        let output = Skim::run_with(&options, Some(rx));

        match output {
            None => break,
            Some(out) if out.is_abort => break,
            Some(out) => {
                let key = out.final_key;

                match key {
                    Key::Tab => {
                        current_dir_type = next_dir_type(current_dir_type);
                        continue;
                    }
                    Key::Ctrl('r') => {
                        if current_dir_type == DirType::Inbox {
                            if let Err(e) = refresh_inbox() {
                                eprintln!("{} Refresh failed: {}", "✗".red(), e);
                                std::thread::sleep(std::time::Duration::from_secs(2));
                            }
                        }
                        continue;
                    }
                    Key::Ctrl('g') => {
                        // Mark-approved in drafts
                        if current_dir_type == DirType::Drafts {
                            if let Some(selected) = out.selected_items.first() {
                                let file_path = selected.output().to_string();
                                let path = Path::new(&file_path);
                                if let Err(e) = mark_as_approved(path) {
                                    eprintln!("{} {}", "✗".red(), e);
                                    std::thread::sleep(std::time::Duration::from_secs(2));
                                }
                            }
                        }
                        continue;
                    }
                    Key::Ctrl('p') => {
                        if let Some(selected) = out.selected_items.first() {
                            let file_path = selected.output().to_string();
                            let _ = arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&file_path));
                        }
                        continue;
                    }
                    Key::Ctrl('d') => {
                        if let Some(selected) = out.selected_items.first() {
                            let file_path = selected.output().to_string();
                            let path = Path::new(&file_path);
                            // Remove companion HTML file if it exists
                            let html_companion = path.with_extension("html");
                            if html_companion.exists() {
                                std::fs::remove_file(&html_companion).ok();
                            }
                            match std::fs::remove_file(path) {
                                Ok(()) => {
                                    println!("{} Deleted: {}", "✓".green(), file_path);
                                    std::thread::sleep(std::time::Duration::from_millis(500));
                                }
                                Err(e) => {
                                    eprintln!("{} Failed to delete: {}", "✗".red(), e);
                                    std::thread::sleep(std::time::Duration::from_secs(2));
                                }
                            }
                        }
                        continue;
                    }
                    Key::Ctrl('s') => {
                        // Archive email from inbox
                        if current_dir_type == DirType::Inbox {
                            if let Some(selected) = out.selected_items.first() {
                                let file_path = selected.output().to_string();
                                let path = Path::new(&file_path);
                                match archive_email_locally(path) {
                                    Ok(()) => {
                                        println!(
                                            "{} Archived: {}",
                                            "✓".green(),
                                            path.file_name()
                                                .map(|n| n.to_string_lossy().to_string())
                                                .unwrap_or_default()
                                        );
                                        std::thread::sleep(std::time::Duration::from_millis(500));
                                    }
                                    Err(e) => {
                                        eprintln!("{} Archive failed: {}", "✗".red(), e);
                                        std::thread::sleep(std::time::Duration::from_secs(2));
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    Key::Ctrl('y') | Key::Ctrl('o') => {
                        // Reply / reply-all in inbox
                        if current_dir_type == DirType::Inbox {
                            if let Some(selected) = out.selected_items.first() {
                                let file_path = selected.output().to_string();
                                let reply_all = key == Key::Ctrl('o');
                                match SmtpConfig::from_env(Some(Path::new(&file_path))) {
                                    Ok(cfg) => {
                                        let drafts_dir = resolve_drafts_dir(Path::new("."));
                                        let drafts_opt = if drafts_dir != PathBuf::from(".") {
                                            Some(drafts_dir)
                                        } else {
                                            None
                                        };
                                        match create_reply_draft(
                                            Path::new(&file_path),
                                            reply_all,
                                            &cfg.default_from,
                                            drafts_opt.as_deref(),
                                        ) {
                                            Ok(draft_path) => {
                                                let _ = std::process::Command::new("hx")
                                                    .arg(&draft_path)
                                                    .status();
                                            }
                                            Err(e) => {
                                                eprintln!("{} {}", "✗".red(), e);
                                                std::thread::sleep(
                                                    std::time::Duration::from_secs(2),
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("{} Could not load config: {}", "✗".red(), e);
                                        std::thread::sleep(std::time::Duration::from_secs(2));
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    _ => {
                        continue;
                    }
                }
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let cli = Cli::parse();

    info!("email-cli started: {:?}", std::env::args().collect::<Vec<_>>());

    // Determine a path hint for finding .env file
    let path_hint: Option<PathBuf> = match &cli.command {
        Some(Commands::Send { file, .. }) => Some(file.clone()),
        Some(Commands::SendApproved { dir, .. }) => Some(dir.clone()),
        Some(Commands::List { dir }) => Some(dir.clone()),
        Some(Commands::Validate { path }) => Some(path.clone()),
        Some(Commands::MarkApproved { file }) => Some(file.clone()),
        Some(Commands::New { name }) => Some(PathBuf::from(name)),
        Some(Commands::Reply { file, .. }) => file.clone(),
        Some(Commands::ListMailboxes) => None,
        Some(Commands::Fetch { .. }) => None,
        Some(Commands::Sync { .. }) => None,
        Some(Commands::Browse { .. }) => None,
        Some(Commands::Watch { .. }) => None,
        Some(Commands::Archive { file }) => Some(file.clone()),
        Some(Commands::Delete { file }) => Some(file.clone()),
        None => cli.file.clone(),
    };

    // Load SMTP config with path hint for finding .env
    let smtp_config = SmtpConfig::from_env(path_hint.as_deref()).unwrap_or_else(|e| {
        eprintln!("{} Could not load SMTP config: {}", "⚠".yellow(), e);
        eprintln!("  Some commands may not work without proper configuration.");
        SmtpConfig {
            host: "localhost".to_string(),
            port: 587,
            username: String::new(),
            password: String::new(),
            default_from: "user@example.com".to_string(),
        }
    });

    // Load email config (fonts, signatures)
    let email_config = load_email_config(path_hint.as_deref());

    // Determine signature to use
    let signature_content: Option<String> = if cli.no_signature {
        None
    } else if email_config.email.include_signature {
        load_signature(&email_config, cli.signature.as_deref())
    } else {
        // include_signature is false, but user can override with --signature
        cli.signature
            .as_deref()
            .and_then(|s| load_signature(&email_config, Some(s)))
    };

    // Get sent directory from env (now loaded from .env via path hint)
    // Strip quotes if present (some .env parsers keep them)
    let sent_dir: Option<PathBuf> = std::env::var("SENT_DIR").ok().map(|s| {
        let s = s.trim_matches('"').trim_matches('\'');
        PathBuf::from(shellexpand::tilde(s).into_owned())
    });

    match cli.command {
        Some(Commands::Send { file, yes }) => {
            let draft = parse_email_draft(&file)?;
            validate_draft(&draft)?;

            preview_draft(
                &draft,
                &smtp_config,
                &email_config,
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
                &email_config,
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
                if let Ok(imap_config) = ImapConfig::from_env(path_hint.as_deref()) {
                    if let Err(e) = append_to_sent_folder(&imap_config, &raw_message) {
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
                if let Ok(imap_config) = ImapConfig::from_env(path_hint.as_deref()) {
                    if let Err(e) = append_to_sent_folder(&imap_config, &raw_message) {
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
            let dir = resolve_drafts_dir(&dir);
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
                    &email_config,
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
                                if let Ok(imap_config) = ImapConfig::from_env(path_hint.as_deref()) {
                                    if let Err(e) = append_to_sent_folder(&imap_config, &raw_message) {
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
                                if let Ok(imap_config) = ImapConfig::from_env(path_hint.as_deref()) {
                                    if let Err(e) = append_to_sent_folder(&imap_config, &raw_message) {
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
            let dir = resolve_drafts_dir(&dir);
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
            mark_as_approved(&file)?;
        }

        Some(Commands::New { name }) => {
            // Add .md extension if not present
            let file_name = if Path::new(&name).extension().is_some() {
                name.clone()
            } else {
                format!("{}.md", name)
            };
            let drafts_dir = resolve_drafts_dir(Path::new("."));
            let path = drafts_dir.join(&file_name);

            // Check if file already exists
            if path.exists() {
                return Err(anyhow!("File already exists: {}", path.display()));
            }

            // Create skeleton content with all frontmatter fields
            let skeleton = r#"---
to:
cc:
bcc:
subject:
status: draft
from:
reply_to:
attachments: []
---

"#;

            fs::write(&path, skeleton)?;
            println!("{} Created new draft: {}", "✓".green(), path.display());
        }

        Some(Commands::Reply { file, all }) => {
            let resolved = resolve_drafts_dir(Path::new("."));
            let drafts_dir: Option<PathBuf> = if resolved != Path::new(".").to_path_buf() {
                Some(resolved)
            } else {
                None
            };

            let source_file = match file {
                Some(f) => f,
                None => {
                    let inbox_dir: PathBuf = std::env::var("INBOX_DIR")
                        .map(|s| {
                            let s = s.trim_matches('"').trim_matches('\'');
                            PathBuf::from(shellexpand::tilde(s).into_owned())
                        })
                        .context(
                            "INBOX_DIR not set in .env (required for interactive reply selection)",
                        )?;
                    select_inbox_email(&inbox_dir)?
                }
            };

            let draft_path = create_reply_draft(
                &source_file,
                all,
                &smtp_config.default_from,
                drafts_dir.as_deref(),
            )?;
            println!(
                "{} Reply draft created: {}",
                "✓".green(),
                draft_path.display()
            );
        }

        Some(Commands::ListMailboxes) => {
            let imap_config = ImapConfig::from_env(path_hint.as_deref())?;
            let mailboxes = tokio::task::spawn_blocking(move || {
                list_mailboxes(&imap_config)
            })
            .await??;

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
            let imap_config = ImapConfig::from_env(path_hint.as_deref())?;
            let criteria = FetchCriteria {
                from,
                to,
                cc,
                subject,
                body,
                since,
                before,
            };

            let emails = tokio::task::spawn_blocking(move || {
                fetch_emails(&imap_config, &criteria, &mailbox, Some(limit))
            })
            .await??;

            display_fetched_emails(&emails, full);

            // Save to inbox if INBOX_DIR is configured
            let inbox_dir: Option<PathBuf> = std::env::var("INBOX_DIR").ok().map(|s| {
                let s = s.trim_matches('"').trim_matches('\'');
                PathBuf::from(shellexpand::tilde(s).into_owned())
            });

            if let Some(ref inbox_dir) = inbox_dir {
                if !emails.is_empty() {
                    match save_fetched_emails(&emails, inbox_dir, "inbox") {
                        Ok((saved, skipped)) => {
                            if saved > 0 {
                                println!(
                                    "\n{} Saved {} email(s) to {}",
                                    "✓".green(),
                                    saved,
                                    inbox_dir.display()
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
                    "\n{} Set INBOX_DIR in .env to automatically save fetched emails.",
                    "ℹ".blue()
                );
            }
        }

        Some(Commands::Sync { limit, mailbox, reconcile }) => {
            let mailboxes = mailbox.unwrap_or_else(|| vec!["INBOX".to_string(), "Archive".to_string(), "Sent".to_string()]);
            let imap_config = ImapConfig::from_env(path_hint.as_deref())?;

            // Pass 1: Additive sync (fetch full emails, save new ones)
            for mb in &mailboxes {
                let imap_mailbox = if mb == "Sent" {
                    resolve_sent_mailbox()
                } else {
                    mb.clone()
                };

                let local_dir = resolve_mailbox_dir(mb)?;
                let status_str = match mb.as_str() {
                    "INBOX" => "inbox",
                    "Archive" => "archived",
                    "Sent" => "sent",
                    _ => "inbox",
                };

                let imap_cfg = imap_config.clone();
                let fetch_limit = Some(limit);
                let emails = tokio::task::spawn_blocking(move || {
                    let criteria = FetchCriteria {
                        from: None,
                        to: None,
                        cc: None,
                        subject: None,
                        body: None,
                        since: None,
                        before: None,
                    };
                    fetch_emails(&imap_cfg, &criteria, &imap_mailbox, fetch_limit)
                })
                .await??;

                let (saved, skipped) = save_fetched_emails(&emails, &local_dir, status_str)?;

                if skipped > 0 {
                    println!(
                        "{} Synced {}: {} new, {} already present in {}",
                        "✓".green(),
                        mb,
                        saved,
                        skipped,
                        local_dir.display()
                    );
                } else {
                    println!(
                        "{} Synced {}: {} email(s) saved to {}",
                        "✓".green(),
                        mb,
                        saved,
                        local_dir.display()
                    );
                }
            }

            // Pass 2: Reconciliation (only with --reconcile flag)
            if reconcile {
                let reconcile_mailboxes = ["INBOX", "Archive"];
                let mut server_ids: std::collections::HashMap<String, std::collections::HashSet<String>> = std::collections::HashMap::new();
                let mut local_dirs: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();

                for mb in &reconcile_mailboxes {
                    let imap_mailbox = mb.to_string();
                    let local_dir = resolve_mailbox_dir(mb)?;
                    local_dirs.insert(mb.to_string(), local_dir);

                    let imap_cfg = imap_config.clone();
                    let ids = tokio::task::spawn_blocking(move || {
                        fetch_server_message_ids(&imap_cfg, &imap_mailbox)
                    })
                    .await??;
                    server_ids.insert(mb.to_string(), ids);
                }

                let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs)?;
                if moved > 0 || removed > 0 {
                    println!(
                        "{} Reconciled: {} moved, {} removed",
                        "ℹ".blue(),
                        moved,
                        removed,
                    );
                } else {
                    println!("{} Reconciled: already in sync", "✓".green());
                }
            }
        }

        Some(Commands::Browse { view }) => {
            browse_emails(&view)?;
        }

        Some(Commands::Watch { mailbox, timeout }) => {
            let imap_config = ImapConfig::from_env(path_hint.as_deref())?;
            let exit_code = tokio::task::spawn_blocking(move || {
                watch_mailbox(&imap_config, &mailbox, timeout)
            })
            .await??;

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
            match archive_email_locally(&file) {
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
            match delete_email_locally(&file) {
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

        None => {
            // Default: preview mode (dry run)
            if let Some(file) = cli.file {
                let draft = parse_email_draft(&file)?;
                preview_draft(
                    &draft,
                    &smtp_config,
                    &email_config,
                    signature_content.as_deref(),
                    true,
                )?;
            } else {
                // No file provided, show help
                println!("email-cli - A CLI tool for sending emails from Markdown drafts\n");
                println!("Usage:");
                println!("  email-cli <file>              Preview a draft (dry-run)");
                println!("  email-cli send <file>         Send a single approved email");
                println!("  email-cli send-approved <dir> Send all approved emails");
                println!("  email-cli list <dir>          List emails by status");
                println!("  email-cli validate <path>     Validate YAML frontmatter");
                println!("  email-cli mark-approved <file> Mark as approved");
                println!("  email-cli new <name>          Create a new draft from template");
                println!("\nOptions:");
                println!("  -s, --signature <name>  Use a specific signature");
                println!("  --no-signature          Skip signature entirely");
                println!("\nRun 'email-cli --help' for more information.");
            }
        }
    }

    Ok(())
}
