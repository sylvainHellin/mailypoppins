use anyhow::{anyhow, Context, Result};
use gray_matter::{engine::YAML, Matter};
use log::info;
use mailparse::{parse_mail, MailHeaderMap};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::ImapConfig;
use crate::parse::{compress_uid_set, extract_body_text, has_attachments, FetchedEmail};
use crate::types::InboxFrontmatter;

pub type ImapSession = imap::Session<native_tls::TlsStream<std::net::TcpStream>>;

pub fn open_imap_session(imap_config: &ImapConfig) -> Result<ImapSession> {
    let tls = native_tls::TlsConnector::builder().build()?;
    let client = imap::connect(
        (imap_config.host.as_str(), imap_config.port),
        &imap_config.host,
        &tls,
    )
    .map_err(|e| anyhow!("Failed to connect to IMAP server: {}", e))?;

    let session = client
        .login(&imap_config.username, &imap_config.password)
        .map_err(|e| anyhow!("IMAP login failed: {}", e.0))?;

    Ok(session)
}

pub struct FetchCriteria {
    pub from: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub since: Option<String>,
    pub before: Option<String>,
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
pub fn fetch_server_message_ids(
    imap_config: &ImapConfig,
    mailbox: &str,
) -> Result<HashSet<String>> {
    info!("Fetching Message-IDs from mailbox '{}'", mailbox);
    let mut session = open_imap_session(imap_config)?;

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

pub fn fetch_emails(
    imap_config: &ImapConfig,
    criteria: &FetchCriteria,
    mailbox: &str,
    limit: Option<usize>,
) -> Result<Vec<FetchedEmail>> {
    info!("Fetching emails from mailbox '{}' (limit: {:?})", mailbox, limit);
    let mut session = open_imap_session(imap_config)?;

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

pub fn list_mailboxes(imap_config: &ImapConfig) -> Result<Vec<String>> {
    let mut session = open_imap_session(imap_config)?;

    let mailboxes = session
        .list(None, Some("*"))
        .map_err(|e| anyhow!("Failed to list mailboxes: {}", e))?;

    let names: Vec<String> = mailboxes.iter().map(|m| m.name().to_string()).collect();

    session.logout().ok();
    Ok(names)
}

pub fn append_to_sent_folder(imap_config: &ImapConfig, raw_message: &[u8], sent_mailbox: &str) -> Result<()> {
    info!("Appending sent email to IMAP folder '{}'", sent_mailbox);

    let mut session = open_imap_session(imap_config)?;

    session.append_with_flags(sent_mailbox, raw_message, &[imap::types::Flag::Seen])
        .map_err(|e| anyhow!("Failed to APPEND to '{}': {}", sent_mailbox, e))?;

    session.logout().ok();
    info!("Successfully appended to '{}'", sent_mailbox);
    Ok(())
}

pub fn watch_mailbox(imap_config: &ImapConfig, mailbox: &str, timeout: Option<u64>) -> Result<i32> {
    use imap::extensions::idle::WaitOutcome;

    let mut session = open_imap_session(imap_config)?;

    session
        .select(mailbox)
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    info!("Watching mailbox '{}' for changes (IMAP IDLE)", mailbox);

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

pub fn archive_email_on_server(imap_config: &ImapConfig, message_id: &str, archive_mailbox: &str) -> Result<()> {
    info!("Archiving email on server: Message-ID={} -> {}", message_id, archive_mailbox);
    let mut session = open_imap_session(imap_config)?;

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

pub fn archive_email_locally(imap_config: &ImapConfig, archive_dir: &Path, file_path: &Path, archive_mailbox: &str) -> Result<()> {
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

    let message_id = inbox_fm.message_id.as_ref().ok_or_else(|| {
        anyhow!("No message_id in frontmatter -- cannot archive on server")
    })?;

    // --- Local operations (instant) ---
    fs::create_dir_all(archive_dir)?;

    let new_content = content.replacen("status: inbox", "status: archived", 1);

    let file_name = file_path
        .file_name()
        .ok_or_else(|| anyhow!("Invalid file path"))?;
    let dest = archive_dir.join(file_name);

    fs::write(&dest, &new_content)?;
    fs::remove_file(file_path)?;

    let html_companion = file_path.with_extension("html");
    let html_dest = dest.with_extension("html");
    let had_html = html_companion.exists();
    if had_html {
        fs::copy(&html_companion, &html_dest)?;
        fs::remove_file(&html_companion)?;
    }

    // --- IMAP operation (slow) ---
    if let Err(e) = archive_email_on_server(imap_config, message_id, archive_mailbox) {
        // Rollback: restore local files to original state
        info!("IMAP archive failed, rolling back local changes: {}", e);
        fs::write(file_path, &content)?;
        fs::remove_file(&dest)?;
        if had_html {
            fs::copy(&html_dest, &html_companion)?;
            fs::remove_file(&html_dest)?;
        }
        return Err(e.context("IMAP archive failed; local changes have been rolled back"));
    }

    Ok(())
}

pub fn delete_email_on_server(imap_config: &ImapConfig, message_id: &str) -> Result<()> {
    info!("Deleting email on server: Message-ID={}", message_id);
    let mut session = open_imap_session(imap_config)?;

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

pub fn delete_email_locally(imap_config: &ImapConfig, file_path: &Path) -> Result<()> {
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

    // Save companion HTML content for potential rollback
    let html_companion = file_path.with_extension("html");
    let html_content = if html_companion.exists() {
        Some(fs::read_to_string(&html_companion)?)
    } else {
        None
    };

    // --- Local operations (instant) ---
    fs::remove_file(file_path)
        .with_context(|| format!("Failed to remove local file: {}", file_path.display()))?;

    if html_content.is_some() {
        fs::remove_file(&html_companion).ok();
    }

    // --- IMAP operation (slow) ---
    if let Some(ref mid) = message_id {
        if let Err(e) = delete_email_on_server(imap_config, mid) {
            // Rollback: restore local files
            info!("IMAP delete failed, rolling back local changes: {}", e);
            fs::write(file_path, &content)?;
            if let Some(ref html) = html_content {
                fs::write(&html_companion, html)?;
            }
            return Err(e.context("IMAP delete failed; local changes have been rolled back"));
        }
    } else {
        info!(
            "No message_id in frontmatter -- skipping server deletion for {}",
            file_path.display()
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Batch operations (single IMAP connection for N emails)
// ---------------------------------------------------------------------------

/// SEARCH + UID COPY + UID STORE \Deleted on an existing session.
/// Does NOT EXPUNGE -- caller batches a single EXPUNGE at the end.
fn archive_single_on_session(
    session: &mut ImapSession,
    message_id: &str,
    archive_mailbox: &str,
) -> Result<()> {
    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        // Not in INBOX -- already archived/moved on server. Treat as success.
        info!(
            "Message-ID {} not found in INBOX (already moved on server), skipping server-side archive",
            message_id
        );
        return Ok(());
    }

    let uid = *uids.iter().next().unwrap();
    let uid_str = uid.to_string();

    session
        .uid_copy(&uid_str, archive_mailbox)
        .map_err(|e| anyhow!("Failed to copy email to {}: {}", archive_mailbox, e))?;

    session
        .uid_store(&uid_str, "+FLAGS (\\Deleted)")
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?;

    Ok(())
}

/// SEARCH + UID STORE \Deleted on an existing session.
/// Does NOT EXPUNGE -- caller batches a single EXPUNGE at the end.
fn delete_single_on_session(
    session: &mut ImapSession,
    message_id: &str,
) -> Result<()> {
    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        // Not in INBOX -- already deleted/moved on server. Treat as success.
        info!(
            "Message-ID {} not found in INBOX (already removed on server), skipping server-side delete",
            message_id
        );
        return Ok(());
    }

    let uid = *uids.iter().next().unwrap();
    let uid_str = uid.to_string();

    session
        .uid_store(&uid_str, "+FLAGS (\\Deleted)")
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?;

    Ok(())
}

/// Archive multiple emails using a single IMAP connection.
///
/// Phase 1 (local): For each path, parse frontmatter, write archive copy, remove original.
/// Phase 2 (IMAP): Open one session, archive each email, single EXPUNGE at the end.
/// Phase 3 (rollback): Restore local files for any IMAP failures.
pub fn batch_archive_emails_locally(
    imap_config: &ImapConfig,
    archive_dir: &Path,
    paths: &[PathBuf],
    archive_mailbox: &str,
) -> Vec<(PathBuf, Result<()>)> {
    let mut results: Vec<(PathBuf, Result<()>)> = Vec::with_capacity(paths.len());

    if let Err(e) = fs::create_dir_all(archive_dir) {
        return paths
            .iter()
            .map(|p| (p.clone(), Err(anyhow!("Failed to create archive dir: {}", e))))
            .collect();
    }

    let matter = Matter::<YAML>::new();

    struct LocalPrep {
        idx: usize,
        message_id: String,
        original_content: String,
        dest: PathBuf,
        had_html: bool,
        html_companion: PathBuf,
        html_dest: PathBuf,
    }

    let mut prepared: Vec<LocalPrep> = Vec::new();

    // Phase 1: Local operations
    for (i, path) in paths.iter().enumerate() {
        let local_result: Result<LocalPrep> = (|| {
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read file: {}", path.display()))?;

            let parsed = matter.parse(&content);
            let inbox_fm: InboxFrontmatter = parsed
                .data
                .ok_or_else(|| anyhow!("No frontmatter found in {}", path.display()))?
                .deserialize()
                .context("Failed to parse frontmatter")?;

            let message_id = inbox_fm.message_id.ok_or_else(|| {
                anyhow!("No message_id in frontmatter -- cannot archive on server")
            })?;

            let new_content = content.replacen("status: inbox", "status: archived", 1);
            let file_name = path
                .file_name()
                .ok_or_else(|| anyhow!("Invalid file path"))?;
            let dest = archive_dir.join(file_name);

            fs::write(&dest, &new_content)?;
            fs::remove_file(path)?;

            let html_companion = path.with_extension("html");
            let html_dest = dest.with_extension("html");
            let had_html = html_companion.exists();
            if had_html {
                fs::copy(&html_companion, &html_dest)?;
                fs::remove_file(&html_companion)?;
            }

            Ok(LocalPrep {
                idx: i,
                message_id,
                original_content: content,
                dest,
                had_html,
                html_companion,
                html_dest,
            })
        })();

        match local_result {
            Ok(prep) => prepared.push(prep),
            Err(e) => results.push((path.clone(), Err(e))),
        }
    }

    if prepared.is_empty() {
        return results;
    }

    // Phase 2: IMAP operations (single connection)
    let mut imap_results: Vec<Result<()>> = Vec::with_capacity(prepared.len());

    match open_imap_session(imap_config) {
        Ok(mut session) => {
            match session
                .select("INBOX")
                .map_err(|e| anyhow!("Failed to select INBOX: {}", e))
            {
                Ok(_) => {
                    for prep in &prepared {
                        imap_results.push(archive_single_on_session(
                            &mut session,
                            &prep.message_id,
                            archive_mailbox,
                        ));
                    }
                    // Single EXPUNGE at the end (non-fatal)
                    if imap_results.iter().any(Result::is_ok) {
                        if let Err(e) = session.expunge() {
                            info!("Batch EXPUNGE failed (non-fatal): {}", e);
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    for _ in &prepared {
                        imap_results.push(Err(anyhow!("{}", msg)));
                    }
                }
            }
            session.logout().ok();
        }
        Err(e) => {
            let msg = format!("IMAP connection failed: {}", e);
            for _ in &prepared {
                imap_results.push(Err(anyhow!("{}", msg)));
            }
        }
    }

    // Phase 3: Process IMAP results, rollback failures
    for (prep, imap_result) in prepared.into_iter().zip(imap_results) {
        let path = paths[prep.idx].clone();
        match imap_result {
            Ok(()) => results.push((path, Ok(()))),
            Err(e) => {
                info!(
                    "IMAP archive failed for {}, rolling back: {}",
                    path.display(),
                    e
                );
                let _ = fs::write(&path, &prep.original_content);
                let _ = fs::remove_file(&prep.dest);
                if prep.had_html {
                    let _ = fs::copy(&prep.html_dest, &prep.html_companion);
                    let _ = fs::remove_file(&prep.html_dest);
                }
                results.push((
                    path,
                    Err(e.context("IMAP archive failed; local changes rolled back")),
                ));
            }
        }
    }

    results
}

/// Delete multiple emails using a single IMAP connection.
///
/// Phase 1 (local): For each path, read content + HTML companion, delete local files.
/// Phase 2 (IMAP): Open one session, delete each email with a message_id, single EXPUNGE.
/// Phase 3 (rollback): Restore local files for any IMAP failures.
///
/// Emails without `message_id` (e.g. drafts) skip IMAP and succeed after local delete.
pub fn batch_delete_emails_locally(
    imap_config: &ImapConfig,
    paths: &[PathBuf],
) -> Vec<(PathBuf, Result<()>)> {
    let mut results: Vec<(PathBuf, Result<()>)> = Vec::with_capacity(paths.len());

    let matter = Matter::<YAML>::new();

    struct LocalPrep {
        idx: usize,
        message_id: Option<String>,
        original_content: String,
        html_content: Option<String>,
        html_companion: PathBuf,
    }

    let mut prepared: Vec<LocalPrep> = Vec::new();

    // Phase 1: Local operations
    for (i, path) in paths.iter().enumerate() {
        let local_result: Result<LocalPrep> = (|| {
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read file: {}", path.display()))?;

            let parsed = matter.parse(&content);
            let message_id: Option<String> = parsed
                .data
                .and_then(|d| {
                    d.deserialize::<std::collections::HashMap<String, serde_yaml::Value>>()
                        .ok()
                })
                .and_then(|map| {
                    map.get("message_id")
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                });

            let html_companion = path.with_extension("html");
            let html_content = if html_companion.exists() {
                Some(fs::read_to_string(&html_companion)?)
            } else {
                None
            };

            fs::remove_file(path)
                .with_context(|| format!("Failed to remove local file: {}", path.display()))?;

            if html_content.is_some() {
                fs::remove_file(&html_companion).ok();
            }

            Ok(LocalPrep {
                idx: i,
                message_id,
                original_content: content,
                html_content,
                html_companion,
            })
        })();

        match local_result {
            Ok(prep) => prepared.push(prep),
            Err(e) => results.push((path.clone(), Err(e))),
        }
    }

    if prepared.is_empty() {
        return results;
    }

    // Check if any items need IMAP at all
    let has_any_imap = prepared.iter().any(|p| p.message_id.is_some());

    if !has_any_imap {
        for prep in prepared {
            info!(
                "No message_id -- skipping server deletion for {}",
                paths[prep.idx].display()
            );
            results.push((paths[prep.idx].clone(), Ok(())));
        }
        return results;
    }

    // Phase 2: IMAP operations (single connection)
    let mut imap_results: Vec<Result<()>> = Vec::with_capacity(prepared.len());

    match open_imap_session(imap_config) {
        Ok(mut session) => {
            match session
                .select("INBOX")
                .map_err(|e| anyhow!("Failed to select INBOX: {}", e))
            {
                Ok(_) => {
                    for prep in &prepared {
                        if let Some(ref mid) = prep.message_id {
                            imap_results.push(delete_single_on_session(&mut session, mid));
                        } else {
                            imap_results.push(Ok(()));
                        }
                    }
                    if imap_results.iter().any(Result::is_ok) {
                        if let Err(e) = session.expunge() {
                            info!("Batch EXPUNGE failed (non-fatal): {}", e);
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    for prep in &prepared {
                        if prep.message_id.is_some() {
                            imap_results.push(Err(anyhow!("{}", msg)));
                        } else {
                            imap_results.push(Ok(()));
                        }
                    }
                }
            }
            session.logout().ok();
        }
        Err(e) => {
            let msg = format!("IMAP connection failed: {}", e);
            for prep in &prepared {
                if prep.message_id.is_some() {
                    imap_results.push(Err(anyhow!("{}", msg)));
                } else {
                    imap_results.push(Ok(()));
                }
            }
        }
    }

    // Phase 3: Process results, rollback failures
    for (prep, imap_result) in prepared.into_iter().zip(imap_results) {
        let path = paths[prep.idx].clone();
        match imap_result {
            Ok(()) => {
                if prep.message_id.is_none() {
                    info!(
                        "No message_id -- skipping server deletion for {}",
                        path.display()
                    );
                }
                results.push((path, Ok(())));
            }
            Err(e) => {
                info!(
                    "IMAP delete failed for {}, rolling back: {}",
                    path.display(),
                    e
                );
                let _ = fs::write(&path, &prep.original_content);
                if let Some(ref html) = prep.html_content {
                    let _ = fs::write(&prep.html_companion, html);
                }
                results.push((
                    path,
                    Err(e.context("IMAP delete failed; local changes rolled back")),
                ));
            }
        }
    }

    results
}
