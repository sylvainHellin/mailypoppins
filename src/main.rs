use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use colored::*;
use gray_matter::{engine::YAML, Matter};
use lettre::{
    message::{header::ContentType, Attachment, Mailbox, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use mailparse::{parse_mail, MailHeaderMap};
use pulldown_cmark::{html, Options, Parser as MdParser};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    },
    /// Send all approved emails in a directory
    SendApproved {
        /// Directory containing email drafts
        #[arg(default_value = ".")]
        dir: PathBuf,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum EmailStatus {
    Draft,
    Approved,
    Sent,
    Inbox,
}

impl std::fmt::Display for EmailStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmailStatus::Draft => write!(f, "draft"),
            EmailStatus::Approved => write!(f, "approved"),
            EmailStatus::Sent => write!(f, "sent"),
            EmailStatus::Inbox => write!(f, "inbox"),
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
    has_attachments: bool,
    message_id: Option<String>,
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

fn extract_body_text(parsed: &mailparse::ParsedMail) -> String {
    // If this part is text/plain, return it
    if parsed.ctype.mimetype == "text/plain" {
        return parsed.get_body().unwrap_or_default();
    }

    // Search subparts for text/plain first, then text/html
    let mut html_body = None;
    for sub in &parsed.subparts {
        let result = extract_body_text(sub);
        if !result.is_empty() {
            if sub.ctype.mimetype == "text/plain"
                || (sub.ctype.mimetype.starts_with("multipart/") && !result.is_empty())
            {
                return result;
            }
            if sub.ctype.mimetype == "text/html" {
                html_body = Some(result);
            }
        }
    }

    // Fall back to HTML body if no plain text found
    if let Some(html) = html_body {
        return strip_html_tags(&html);
    }

    // If this is text/html at top level
    if parsed.ctype.mimetype == "text/html" {
        return strip_html_tags(&parsed.get_body().unwrap_or_default());
    }

    String::new()
}

fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_style = false;
    let mut in_script = false;

    let lower = html.to_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();

    let mut i = 0;
    while i < chars.len() {
        if !in_tag && starts_with_at(&lower_chars, i, "<style") {
            in_style = true;
            in_tag = true;
        } else if !in_tag && starts_with_at(&lower_chars, i, "<script") {
            in_script = true;
            in_tag = true;
        } else if in_style && starts_with_at(&lower_chars, i, "</style") {
            in_style = false;
            in_tag = true;
        } else if in_script && starts_with_at(&lower_chars, i, "</script") {
            in_script = false;
            in_tag = true;
        } else if chars[i] == '<' {
            // Insert newline for block elements
            if starts_with_at(&lower_chars, i, "<br")
                || starts_with_at(&lower_chars, i, "<p")
                || starts_with_at(&lower_chars, i, "<div")
                || starts_with_at(&lower_chars, i, "<tr")
                || starts_with_at(&lower_chars, i, "<li")
            {
                result.push('\n');
            }
            in_tag = true;
        } else if chars[i] == '>' {
            in_tag = false;
        } else if !in_tag && !in_style && !in_script {
            result.push(chars[i]);
        }
        i += 1;
    }

    // Decode common HTML entities
    let result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&#160;", " ");

    // Decode numeric entities like &#43; -> +
    let mut decoded = String::with_capacity(result.len());
    let mut chars_iter = result.chars().peekable();
    while let Some(ch) = chars_iter.next() {
        if ch == '&' && chars_iter.peek() == Some(&'#') {
            let mut entity = String::from("&#");
            chars_iter.next(); // consume '#'
            while let Some(&c) = chars_iter.peek() {
                if c == ';' {
                    chars_iter.next();
                    break;
                }
                entity.push(c);
                chars_iter.next();
            }
            let num_str = &entity[2..];
            if let Ok(code) = num_str.parse::<u32>() {
                if let Some(decoded_char) = char::from_u32(code) {
                    decoded.push(decoded_char);
                    continue;
                }
            }
            decoded.push_str(&entity);
            decoded.push(';');
        } else {
            decoded.push(ch);
        }
    }
    let result = decoded;

    // Collapse excessive whitespace: multiple blank lines -> max 2 newlines
    let mut cleaned = String::with_capacity(result.len());
    let mut consecutive_newlines = 0;
    for ch in result.chars() {
        if ch == '\n' {
            consecutive_newlines += 1;
            if consecutive_newlines <= 2 {
                cleaned.push(ch);
            }
        } else {
            consecutive_newlines = 0;
            cleaned.push(ch);
        }
    }

    cleaned.trim().to_string()
}

fn starts_with_at(chars: &[char], pos: usize, needle: &str) -> bool {
    let needle_chars: Vec<char> = needle.chars().collect();
    if pos + needle_chars.len() > chars.len() {
        return false;
    }
    for (j, nc) in needle_chars.iter().enumerate() {
        if chars[pos + j] != *nc {
            return false;
        }
    }
    true
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

fn fetch_emails(
    imap_config: &ImapConfig,
    criteria: &FetchCriteria,
    mailbox: &str,
    limit: usize,
) -> Result<Vec<FetchedEmail>> {
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
    let selected_uids: Vec<u32> = uid_list.into_iter().rev().take(limit).collect();

    let uid_set = selected_uids
        .iter()
        .map(|u| u.to_string())
        .collect::<Vec<_>>()
        .join(",");

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
        let body_text = extract_body_text(&parsed);
        let attachments = has_attachments(&parsed);

        emails.push(FetchedEmail {
            from,
            to,
            cc,
            subject,
            date,
            body_text,
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
    if result.len() > 60 {
        // Truncate at word boundary
        let truncated = &result[..60];
        truncated.trim_end_matches('-').to_string()
    } else {
        result
    }
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

    // Build quoted body with attribution
    let attribution = format!(
        "On {}, {} wrote:",
        inbox.date.as_deref().unwrap_or("(unknown date)"),
        inbox.from
    );
    let quoted_body: String = original_body
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
    fm.push_str(&format!("bcc: \"{}\"\n", default_from));
    fm.push_str(&format!(
        "subject: \"{}\"\n",
        reply_subject.replace('"', "\\\"")
    ));
    fm.push_str("status: draft\n");
    fm.push_str("---\n");

    // Compose full content
    let full_content = format!("{}\n\n\n{}\n{}\n", fm.trim_end(), attribution, quoted_body);

    // Determine output path
    let output_dir = drafts_dir.unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_dir)?;
    let date_prefix = Utc::now().format("%Y-%m-%d").to_string();
    let subject_slug = slugify_subject(&reply_subject);
    let filename = format!("{}_{}.md", date_prefix, subject_slug);
    let mut dest = output_dir.join(&filename);

    // Avoid overwriting
    if dest.exists() {
        let mut counter = 1;
        loop {
            let name = format!("{}_{}-{}.md", date_prefix, subject_slug, counter);
            dest = output_dir.join(&name);
            if !dest.exists() {
                break;
            }
            counter += 1;
        }
    }

    fs::write(&dest, full_content)?;
    Ok(dest)
}

fn parse_email_date_prefix(date_str: &str) -> String {
    // Try parsing common email date formats to extract YYYY-MM-DD
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(date_str) {
        return dt.format("%Y-%m-%d").to_string();
    }
    // Fallback: use today's date
    Utc::now().format("%Y-%m-%d").to_string()
}

fn save_fetched_emails(emails: &[FetchedEmail], inbox_dir: &Path) -> Result<(usize, usize)> {
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
        let subject_slug = slugify_subject(&email.subject);
        let filename = if subject_slug.is_empty() {
            format!("{}_email.md", date_prefix)
        } else {
            format!("{}_{}.md", date_prefix, subject_slug)
        };

        let mut dest = inbox_dir.join(&filename);
        // Avoid overwriting if filename collides
        if dest.exists() {
            let mut counter = 1;
            loop {
                let name = if subject_slug.is_empty() {
                    format!("{}_email-{}.md", date_prefix, counter)
                } else {
                    format!("{}_{}-{}.md", date_prefix, subject_slug, counter)
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
        frontmatter.push_str("status: inbox\n");
        frontmatter.push_str(&format!("has_attachments: {}\n", email.has_attachments));
        frontmatter.push_str(&format!("fetched_at: \"{}\"\n", fetched_at));
        frontmatter.push_str("---\n\n");

        let content = format!("{}{}", frontmatter, email.body_text);
        fs::write(&dest, content)?;

        // Track the new message_id to prevent duplicates within the same batch
        if let Some(ref mid) = email.message_id {
            existing_ids.insert(mid.clone());
        }

        saved += 1;
    }

    Ok((saved, skipped))
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

fn markdown_to_html(markdown: &str, config: &EmailConfig, signature: Option<&str>) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);

    let parser = MdParser::new_ext(markdown, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    // Add signature if provided
    let signature_html = signature.unwrap_or_default();

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
</style>
</head>
<body>
{body}
{signature}
</body>
</html>"#,
        font_family = config.email.font_family,
        font_size = config.email.font_size,
        body = html_output,
        signature = signature_html
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
) -> Result<()> {
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

    // Build the message
    let mut builder = Message::builder()
        .from(from_mailbox)
        .subject(&draft.frontmatter.subject);

    // Add To recipients
    for to in draft.frontmatter.to.split(',') {
        let to_mailbox: Mailbox = to.trim().parse().context("Invalid 'to' email address")?;
        builder = builder.to(to_mailbox);
    }

    // Add Cc recipients
    if let Some(cc) = &draft.frontmatter.cc {
        for cc_addr in cc.split(',') {
            let cc_mailbox: Mailbox = cc_addr
                .trim()
                .parse()
                .context("Invalid 'cc' email address")?;
            builder = builder.cc(cc_mailbox);
        }
    }

    // Add Bcc recipients
    if let Some(bcc) = &draft.frontmatter.bcc {
        for bcc_addr in bcc.split(',') {
            let bcc_mailbox: Mailbox = bcc_addr
                .trim()
                .parse()
                .context("Invalid 'bcc' email address")?;
            builder = builder.bcc(bcc_mailbox);
        }
    }

    // Add Reply-To
    if let Some(reply_to) = &draft.frontmatter.reply_to {
        let reply_mailbox: Mailbox = reply_to
            .parse()
            .context("Invalid 'reply_to' email address")?;
        builder = builder.reply_to(reply_mailbox);
    }

    // Generate HTML with signature
    let body_html = markdown_to_html(&draft.body_markdown, email_config, signature);

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
            // Wrap body in mixed multipart and add attachments
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

    // Create SMTP transport
    let creds = Credentials::new(smtp_config.username.clone(), smtp_config.password.clone());

    let mailer: AsyncSmtpTransport<Tokio1Executor> =
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp_config.host)?
            .port(smtp_config.port)
            .credentials(creds)
            .build();

    // Send the email
    mailer.send(message).await.context("Failed to send email")?;

    Ok(())
}

fn update_status_to_sent(draft: &EmailDraft, sent_dir: Option<&Path>) -> Result<()> {
    let mut frontmatter = draft.frontmatter.clone();
    frontmatter.status = EmailStatus::Sent;
    frontmatter.sent_at = Some(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    frontmatter.sent_via = Some(format!("email-cli v{}", env!("CARGO_PKG_VERSION")));

    // Serialize the updated frontmatter
    let yaml = serde_yaml::to_string(&frontmatter)?;

    // Reconstruct the file content
    let new_content = format!("---\n{}---\n\n{}", yaml, draft.body_markdown);

    // Determine destination path
    let dest_path = if let Some(sent_dir) = sent_dir {
        fs::create_dir_all(sent_dir)?;
        // Prepend datetime to filename to avoid overwriting and make search easier
        let timestamp = Utc::now().format("%Y-%m-%d-%H-%M-%S").to_string();
        let original_name = draft.path.file_name().unwrap().to_string_lossy();
        let new_name = format!("{}_{}", timestamp, original_name);
        sent_dir.join(new_name)
    } else {
        draft.path.clone()
    };

    // Write the updated content
    fs::write(&dest_path, new_content)?;

    // If we moved the file, remove the original
    if sent_dir.is_some() && dest_path != draft.path {
        fs::remove_file(&draft.path)?;
    }

    Ok(())
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Determine a path hint for finding .env file
    let path_hint: Option<PathBuf> = match &cli.command {
        Some(Commands::Send { file }) => Some(file.clone()),
        Some(Commands::SendApproved { dir }) => Some(dir.clone()),
        Some(Commands::List { dir }) => Some(dir.clone()),
        Some(Commands::Validate { path }) => Some(path.clone()),
        Some(Commands::MarkApproved { file }) => Some(file.clone()),
        Some(Commands::New { name }) => Some(PathBuf::from(name)),
        Some(Commands::Reply { file, .. }) => file.clone(),
        Some(Commands::ListMailboxes) => None,
        Some(Commands::Fetch { .. }) => None,
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
        Some(Commands::Send { file }) => {
            let draft = parse_email_draft(&file)?;
            validate_draft(&draft)?;

            preview_draft(
                &draft,
                &smtp_config,
                &email_config,
                signature_content.as_deref(),
                false,
            )?;

            if !prompt_confirmation("Send this email?") {
                println!("Cancelled.");
                return Ok(());
            }

            println!("Sending email...");
            send_email(
                &draft,
                &smtp_config,
                &email_config,
                signature_content.as_deref(),
            )
            .await?;
            update_status_to_sent(&draft, sent_dir.as_deref())?;

            println!(
                "{} Email sent successfully to {}",
                "✓".green().bold(),
                draft.frontmatter.to
            );
        }

        Some(Commands::SendApproved { dir }) => {
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

            if !prompt_confirmation(&format!("\nSend all {} emails?", drafts.len())) {
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
                    Ok(()) => {
                        if let Err(e) = update_status_to_sent(&draft, sent_dir.as_deref()) {
                            println!("{} (sent but failed to update status: {})", "⚠".yellow(), e);
                        } else {
                            println!("{}", "✓".green());
                        }
                        sent_count += 1;
                    }
                    Err(e) => {
                        println!("{} {}", "✗".red(), e);
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
            let drafts = find_drafts(&dir, None)?;

            if drafts.is_empty() {
                println!("No email drafts found in {}", dir.display());
                return Ok(());
            }

            let mut draft_count = 0;
            let mut approved_count = 0;
            let mut sent_count = 0;
            let mut inbox_count = 0;

            println!("\n{}", "Email Drafts:".bold());
            println!("{}", "─".repeat(60));

            for draft in &drafts {
                let status_colored = match draft.frontmatter.status {
                    EmailStatus::Draft => "draft".yellow(),
                    EmailStatus::Approved => "approved".green(),
                    EmailStatus::Sent => "sent".dimmed(),
                    EmailStatus::Inbox => "inbox".cyan(),
                };

                match draft.frontmatter.status {
                    EmailStatus::Draft => draft_count += 1,
                    EmailStatus::Approved => approved_count += 1,
                    EmailStatus::Sent => sent_count += 1,
                    EmailStatus::Inbox => inbox_count += 1,
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
                "Total: {} | Draft: {} | Approved: {} | Sent: {} | Inbox: {}",
                drafts.len(),
                draft_count.to_string().yellow(),
                approved_count.to_string().green(),
                sent_count.to_string().dimmed(),
                inbox_count.to_string().cyan()
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
            let path = PathBuf::from(&file_name);

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
            let drafts_dir: Option<PathBuf> = std::env::var("DRAFTS_DIR").ok().map(|s| {
                let s = s.trim_matches('"').trim_matches('\'');
                PathBuf::from(shellexpand::tilde(s).into_owned())
            });

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
                fetch_emails(&imap_config, &criteria, &mailbox, limit)
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
                    match save_fetched_emails(&emails, inbox_dir) {
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
