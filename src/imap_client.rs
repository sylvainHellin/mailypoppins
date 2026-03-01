use anyhow::{anyhow, Context, Result};
use gray_matter::{engine::YAML, Matter};
use log::info;
use mailparse::{parse_mail, MailHeaderMap};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::config::ImapConfig;
use crate::parse::{compress_uid_set, extract_body_text, has_attachments, FetchedEmail};
use crate::types::InboxFrontmatter;

pub(crate) struct FetchCriteria {
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
    pub(crate) cc: Option<String>,
    pub(crate) subject: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) since: Option<String>,
    pub(crate) before: Option<String>,
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

/// Fetch only Message-IDs from a mailbox (lightweight, no body download).
pub(crate) fn fetch_server_message_ids(
    imap_config: &ImapConfig,
    mailbox: &str,
) -> Result<HashSet<String>> {
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
        return Ok(HashSet::new());
    }

    let mut uid_list: Vec<u32> = uids.into_iter().collect();
    uid_list.sort();
    let uid_set = compress_uid_set(&uid_list);

    let fetched = session
        .uid_fetch(&uid_set, "BODY.PEEK[HEADER.FIELDS (Message-ID)]")
        .map_err(|e| anyhow!("Failed to fetch Message-IDs: {}", e))?;

    let mut ids = HashSet::new();
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

pub(crate) fn fetch_emails(
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

pub(crate) fn list_mailboxes(imap_config: &ImapConfig) -> Result<Vec<String>> {
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

pub(crate) fn append_to_sent_folder(imap_config: &ImapConfig, raw_message: &[u8], sent_mailbox: &str) -> Result<()> {
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

    session.append_with_flags(sent_mailbox, raw_message, &[imap::types::Flag::Seen])
        .map_err(|e| anyhow!("Failed to APPEND to '{}': {}", sent_mailbox, e))?;

    session.logout().ok();
    info!("Successfully appended to '{}'", sent_mailbox);
    Ok(())
}

pub(crate) fn watch_mailbox(imap_config: &ImapConfig, mailbox: &str, timeout: Option<u64>) -> Result<i32> {
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

pub(crate) fn archive_email_on_server(imap_config: &ImapConfig, message_id: &str, archive_mailbox: &str) -> Result<()> {
    info!("Archiving email on server: Message-ID={} -> {}", message_id, archive_mailbox);
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
        .uid_copy(&uid_str, archive_mailbox)
        .map_err(|e| anyhow!("Failed to copy email to {}: {}", archive_mailbox, e))?;

    session
        .uid_store(&uid_str, "+FLAGS (\\Deleted)")
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?;

    session
        .expunge()
        .map_err(|e| anyhow!("Failed to expunge: {}", e))?;

    session.logout().ok();
    Ok(())
}

pub(crate) fn archive_email_locally(imap_config: &ImapConfig, archive_dir: &Path, file_path: &Path, archive_mailbox: &str) -> Result<()> {
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
        archive_email_on_server(imap_config, mid, archive_mailbox)?;
    } else {
        return Err(anyhow!(
            "No message_id in frontmatter -- cannot archive on server"
        ));
    }

    fs::create_dir_all(archive_dir)?;

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

pub(crate) fn delete_email_on_server(imap_config: &ImapConfig, message_id: &str) -> Result<()> {
    info!("Deleting email on server: Message-ID={}", message_id);
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

pub(crate) fn delete_email_locally(imap_config: &ImapConfig, file_path: &Path) -> Result<()> {
    info!("Deleting email locally: {}", file_path.display());
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    // Extract just the message_id from frontmatter without requiring a specific struct.
    // This allows deletion of any email type (inbox, draft, sent, etc.).
    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);
    let message_id: Option<String> = parsed
        .data
        .and_then(|d| d.deserialize::<std::collections::HashMap<String, serde_yaml::Value>>().ok())
        .and_then(|map| map.get("message_id").and_then(|v| v.as_str().map(|s| s.to_string())));

    if let Some(ref mid) = message_id {
        delete_email_on_server(imap_config, mid)?;
    } else {
        info!(
            "No message_id in frontmatter -- skipping server deletion for {}",
            file_path.display()
        );
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
