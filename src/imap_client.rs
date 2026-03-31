use anyhow::{anyhow, Context, Result};
use async_imap::extensions::idle::IdleResponse;
use futures::TryStreamExt;
use gray_matter::{engine::YAML, Matter};
use log::{info, warn};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::ImapConfig;
use crate::parse::{
    attachments_dir_for, compress_uid_set, parse_rfc822_to_fetched_email,
    save_fetched_emails_with_known_ids, scan_existing_message_ids, FetchedEmail,
};
use crate::sync::reconcile_local_files;
use crate::types::InboxFrontmatter;

pub type ImapSession = async_imap::Session<async_native_tls::TlsStream<async_std::net::TcpStream>>;

/// A mailbox target for the unified sync operation.
pub struct SyncTarget {
    pub role: String,
    pub server_name: String,
    pub local_dir: PathBuf,
    pub status: String,
}

/// Results from a `sync_mailboxes` operation.
pub struct SyncResult {
    pub saved: usize,
    pub skipped: usize,
    pub moved: usize,
    pub removed: usize,
}

pub async fn open_imap_session(imap_config: &ImapConfig) -> Result<ImapSession> {
    let tls = async_native_tls::TlsConnector::new();
    let addr = format!("{}:{}", imap_config.host, imap_config.port);

    let tcp_stream = async_std::net::TcpStream::connect(&addr)
        .await
        .map_err(|e| anyhow!("Failed to connect to IMAP server: {}", e))?;

    let tls_stream = tls
        .connect(&imap_config.host, tcp_stream)
        .await
        .map_err(|e| anyhow!("TLS handshake failed: {}", e))?;

    let client = async_imap::Client::new(tls_stream);

    let session = client
        .login(&imap_config.username, &imap_config.password)
        .await
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
    pub text: Option<String>,
    /// Routing directive: which mailbox to search. Not an IMAP search criterion.
    pub in_mailbox: Option<String>,
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

    if let Some(ref text) = criteria.text {
        parts.push(format!("TEXT \"{}\"", text));
    }

    if parts.is_empty() {
        "ALL".to_string()
    } else {
        parts.join(" ")
    }
}

/// Parse a user search string into structured FetchCriteria.
///
/// Recognized prefixes: from:, to:, cc:, subject:, body:, since:, before:
/// Quoted values supported: from:"John Doe"
/// Bare text (no prefix) becomes a TEXT search.
pub fn parse_search_query(input: &str) -> FetchCriteria {
    let mut criteria = FetchCriteria {
        from: None,
        to: None,
        cc: None,
        subject: None,
        body: None,
        since: None,
        before: None,
        text: None,
        in_mailbox: None,
    };

    let mut remaining = Vec::new();
    let mut chars = input.chars().peekable();

    while chars.peek().is_some() {
        // skip whitespace
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }

        // Try to match a known prefix
        let rest: String = chars.clone().collect();
        let lower_rest = rest.to_lowercase();

        let mut matched = false;
        for (prefix, setter) in [
            ("from:", 0u8),
            ("to:", 1),
            ("cc:", 2),
            ("subject:", 3),
            ("body:", 4),
            ("since:", 5),
            ("before:", 6),
            ("in:", 7),
        ] {
            if lower_rest.starts_with(prefix) {
                for _ in 0..prefix.len() {
                    chars.next();
                }
                let value = extract_search_value(&mut chars);
                match setter {
                    0 => criteria.from = Some(value),
                    1 => criteria.to = Some(value),
                    2 => criteria.cc = Some(value),
                    3 => criteria.subject = Some(value),
                    4 => criteria.body = Some(value),
                    5 => criteria.since = Some(value),
                    6 => criteria.before = Some(value),
                    7 => criteria.in_mailbox = Some(value),
                    _ => unreachable!(),
                }
                matched = true;
                break;
            }
        }

        if !matched {
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c == ' ' {
                    break;
                }
                word.push(c);
                chars.next();
            }
            if !word.is_empty() {
                remaining.push(word);
            }
        }
    }

    if !remaining.is_empty() {
        criteria.text = Some(remaining.join(" "));
    }

    criteria
}

fn extract_search_value(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    while chars.peek() == Some(&' ') {
        chars.next();
    }

    if chars.peek() == Some(&'"') {
        chars.next(); // consume opening quote
        let mut value = String::new();
        for c in chars.by_ref() {
            if c == '"' {
                break;
            }
            value.push(c);
        }
        value
    } else {
        let mut value = String::new();
        while let Some(&c) = chars.peek() {
            if c == ' ' {
                break;
            }
            value.push(c);
            chars.next();
        }
        value
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

/// Extract Message-ID from raw header bytes (from BODY.PEEK[HEADER.FIELDS (Message-ID)]).
fn parse_message_id_from_header_bytes(header_bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(header_bytes).ok()?;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed
            .strip_prefix("Message-ID:")
            .or_else(|| trimmed.strip_prefix("Message-Id:"))
            .or_else(|| trimmed.strip_prefix("message-id:"))
        {
            let id = value.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Fetch only Message-IDs from a mailbox on an existing session (lightweight, no body download).
pub async fn fetch_server_message_ids_on_session(
    session: &mut ImapSession,
    mailbox: &str,
) -> Result<HashSet<String>> {
    session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    let uids = session
        .uid_search("ALL")
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        return Ok(HashSet::new());
    }

    let mut uid_list: Vec<u32> = uids.into_iter().collect();
    uid_list.sort();
    let uid_set = compress_uid_set(&uid_list);

    let fetched: Vec<_> = session
        .uid_fetch(&uid_set, "BODY.PEEK[HEADER.FIELDS (Message-ID)]")
        .await
        .map_err(|e| anyhow!("Failed to fetch Message-IDs: {}", e))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect Message-IDs: {}", e))?;

    let mut ids = HashSet::new();
    for msg in fetched.iter() {
        if let Some(header_bytes) = msg.header() {
            if let Some(mid) = parse_message_id_from_header_bytes(header_bytes) {
                ids.insert(mid);
            }
        }
    }

    Ok(ids)
}

/// Fetch only Message-IDs from a mailbox (lightweight, no body download).
/// Opens and closes its own IMAP session.
pub async fn fetch_server_message_ids(
    imap_config: &ImapConfig,
    mailbox: &str,
) -> Result<HashSet<String>> {
    info!("Fetching Message-IDs from mailbox '{}'", mailbox);
    let mut session = open_imap_session(imap_config).await?;
    let ids = fetch_server_message_ids_on_session(&mut session, mailbox).await?;
    session.logout().await.ok();
    Ok(ids)
}

/// Fetch emails on an existing session using search criteria and optional limit.
pub async fn fetch_emails_on_session(
    session: &mut ImapSession,
    criteria: &FetchCriteria,
    mailbox: &str,
    limit: Option<usize>,
) -> Result<Vec<FetchedEmail>> {
    session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    let query = build_imap_search_query(criteria);
    let uids = session
        .uid_search(&query)
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        return Ok(Vec::new());
    }

    let mut uid_list: Vec<u32> = uids.into_iter().collect();
    uid_list.sort();
    let selected_uids: Vec<u32> = match limit {
        Some(n) => uid_list.into_iter().rev().take(n).collect(),
        None => uid_list,
    };

    let uid_set = compress_uid_set(&selected_uids);

    let fetched: Vec<_> = session
        .uid_fetch(&uid_set, "BODY.PEEK[]")
        .await
        .map_err(|e| anyhow!("Failed to fetch emails: {}", e))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect emails: {}", e))?;

    let mut emails = Vec::new();
    for msg in fetched.iter() {
        let body_raw = msg.body().unwrap_or_default();
        if let Some(email) = parse_rfc822_to_fetched_email(body_raw) {
            emails.push(email);
        }
    }

    Ok(emails)
}

/// Fetch emails from an IMAP server. Opens and closes its own session.
pub async fn fetch_emails(
    imap_config: &ImapConfig,
    criteria: &FetchCriteria,
    mailbox: &str,
    limit: Option<usize>,
) -> Result<Vec<FetchedEmail>> {
    info!(
        "Fetching emails from mailbox '{}' (limit: {:?})",
        mailbox, limit
    );
    let mut session = open_imap_session(imap_config).await?;
    let emails = fetch_emails_on_session(&mut session, criteria, mailbox, limit).await?;
    session.logout().await.ok();
    Ok(emails)
}

/// Two-pass fetch: first fetch Message-ID headers (lightweight), then only download
/// full RFC822 for emails not already present locally.
/// Returns (new_emails, num_already_known_in_window).
pub async fn fetch_new_emails_on_session(
    session: &mut ImapSession,
    mailbox: &str,
    limit: Option<usize>,
    known_ids: &HashSet<String>,
) -> Result<(Vec<FetchedEmail>, usize)> {
    session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    let uids = session
        .uid_search("ALL")
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        return Ok((Vec::new(), 0));
    }

    let mut uid_list: Vec<u32> = uids.into_iter().collect();
    uid_list.sort();
    let selected_uids: Vec<u32> = match limit {
        Some(n) => uid_list.into_iter().rev().take(n).collect(),
        None => uid_list,
    };

    if selected_uids.is_empty() {
        return Ok((Vec::new(), 0));
    }

    let checked_count = selected_uids.len();

    // Pass 1: Fetch only Message-ID headers (~50 bytes/msg)
    let uid_set = compress_uid_set(&selected_uids);
    let header_fetched: Vec<_> = session
        .uid_fetch(&uid_set, "(UID BODY.PEEK[HEADER.FIELDS (Message-ID)])")
        .await
        .map_err(|e| anyhow!("Failed to fetch Message-ID headers: {}", e))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect Message-ID headers: {}", e))?;

    // Identify UIDs whose Message-ID is not in known_ids
    let mut new_uids: Vec<u32> = Vec::new();
    for msg in header_fetched.iter() {
        let uid = match msg.uid {
            Some(u) => u,
            None => continue,
        };
        let mid = msg.header().and_then(parse_message_id_from_header_bytes);
        match mid {
            Some(ref id) if known_ids.contains(id) => {
                // Already known locally, skip
            }
            _ => {
                new_uids.push(uid);
            }
        }
    }

    let skipped = checked_count - new_uids.len();

    if new_uids.is_empty() {
        return Ok((Vec::new(), skipped));
    }

    info!(
        "Two-pass fetch for '{}': {} new, {} skipped (out of {} checked)",
        mailbox,
        new_uids.len(),
        skipped,
        checked_count
    );

    // Pass 2: Fetch full message body only for genuinely new messages
    let new_uid_set = compress_uid_set(&new_uids);
    let fetched: Vec<_> = session
        .uid_fetch(&new_uid_set, "BODY.PEEK[]")
        .await
        .map_err(|e| anyhow!("Failed to fetch emails: {}", e))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect emails: {}", e))?;

    let mut emails = Vec::new();
    for msg in fetched.iter() {
        let body_raw = msg.body().unwrap_or_default();
        if let Some(email) = parse_rfc822_to_fetched_email(body_raw) {
            emails.push(email);
        }
    }

    Ok((emails, skipped))
}

/// Unified sync orchestrator: fetches all mailboxes using a single IMAP connection,
/// with optional reconciliation pass.
pub async fn sync_mailboxes(
    imap_config: &ImapConfig,
    targets: &[SyncTarget],
    limit: usize,
    reconcile: bool,
) -> Result<SyncResult> {
    info!(
        "sync_mailboxes: {} targets, limit={}, reconcile={}",
        targets.len(),
        limit,
        reconcile
    );

    let mut session = open_imap_session(imap_config).await?;
    let mut total_saved = 0usize;
    let mut total_skipped = 0usize;

    // Phase 1: Additive sync with two-pass fetch
    for target in targets {
        // Pre-scan local directory for known message IDs
        let mut known_ids = scan_existing_message_ids(&target.local_dir)?;

        match fetch_new_emails_on_session(
            &mut session,
            &target.server_name,
            Some(limit),
            &known_ids,
        )
        .await
        {
            Ok((new_emails, skipped)) => {
                total_skipped += skipped;
                if !new_emails.is_empty() {
                    let (saved, _dup) = save_fetched_emails_with_known_ids(
                        &new_emails,
                        &target.local_dir,
                        &target.status,
                        &mut known_ids,
                    )?;
                    total_saved += saved;
                }
            }
            Err(e) => {
                warn!(
                    "Failed to sync mailbox '{}': {}. Continuing with next.",
                    target.server_name, e
                );
            }
        }
    }

    let mut total_moved = 0usize;
    let mut total_removed = 0usize;

    // Phase 2: Reconciliation (only INBOX and Archive)
    if reconcile {
        let mut reconcile_pairs: Vec<(String, String)> = Vec::new();
        let mut local_dirs: HashMap<String, PathBuf> = HashMap::new();

        for target in targets {
            let role = target.role.to_ascii_lowercase();
            if role == "inbox" || role == "archive" {
                let key = if role == "inbox" {
                    "INBOX".to_string()
                } else {
                    "Archive".to_string()
                };
                reconcile_pairs.push((key.clone(), target.server_name.clone()));
                local_dirs.insert(key, target.local_dir.clone());
            }
        }

        if !reconcile_pairs.is_empty() {
            let mut server_ids: HashMap<String, HashSet<String>> = HashMap::new();
            for (key, imap_mailbox) in &reconcile_pairs {
                match fetch_server_message_ids_on_session(&mut session, imap_mailbox).await {
                    Ok(ids) => {
                        server_ids.insert(key.clone(), ids);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to fetch Message-IDs for reconciliation from '{}': {}",
                            imap_mailbox, e
                        );
                    }
                }
            }

            if !server_ids.is_empty() {
                let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs)?;
                total_moved = moved;
                total_removed = removed;
            }
        }
    }

    session.logout().await.ok();

    Ok(SyncResult {
        saved: total_saved,
        skipped: total_skipped,
        moved: total_moved,
        removed: total_removed,
    })
}

pub async fn list_mailboxes(imap_config: &ImapConfig) -> Result<Vec<String>> {
    let mut session = open_imap_session(imap_config).await?;

    let mailboxes: Vec<_> = session
        .list(None, Some("*"))
        .await
        .map_err(|e| anyhow!("Failed to list mailboxes: {}", e))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect mailboxes: {}", e))?;

    let names: Vec<String> = mailboxes.iter().map(|m| m.name().to_string()).collect();

    session.logout().await.ok();
    Ok(names)
}

pub async fn append_to_sent_folder(imap_config: &ImapConfig, raw_message: &[u8], sent_mailbox: &str) -> Result<()> {
    info!("Appending sent email to IMAP folder '{}'", sent_mailbox);

    let mut session = open_imap_session(imap_config).await?;

    session.append(sent_mailbox, Some("(\\Seen)"), None, raw_message)
        .await
        .map_err(|e| anyhow!("Failed to APPEND to '{}': {}", sent_mailbox, e))?;

    session.logout().await.ok();
    info!("Successfully appended to '{}'", sent_mailbox);
    Ok(())
}

pub async fn watch_mailbox(imap_config: &ImapConfig, mailbox: &str, timeout: Option<u64>) -> Result<i32> {
    let mut session = open_imap_session(imap_config).await?;

    session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    info!("Watching mailbox '{}' for changes (IMAP IDLE)", mailbox);

    let exit_code = match timeout {
        None => {
            // No timeout: block until mailbox changes
            let mut handle = session.idle();
            handle.init().await?;
            let (fut, _stop) = handle.wait_with_timeout(std::time::Duration::from_secs(29 * 60));
            let response = fut.await?;
            session = handle.done().await?;

            match response {
                IdleResponse::NewData(_) => 0,
                IdleResponse::Timeout | IdleResponse::ManualInterrupt => {
                    // For no-timeout mode, re-interpret timeout as "no change"
                    // but since we set a very long timeout, treat as connection issue
                    1
                }
            }
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

                let mut handle = session.idle();
                handle.init().await?;
                let (fut, _stop) = handle.wait_with_timeout(wait_duration);
                let response = fut.await?;
                session = handle.done().await?;

                match response {
                    IdleResponse::NewData(_) => break 0,
                    IdleResponse::Timeout => continue, // re-idle for keepalive
                    IdleResponse::ManualInterrupt => continue,
                }
            }
        }
    };

    session.logout().await.ok();

    Ok(exit_code)
}

pub async fn archive_email_on_server(imap_config: &ImapConfig, message_id: &str, archive_mailbox: &str) -> Result<()> {
    info!("Archiving email on server: Message-ID={} -> {}", message_id, archive_mailbox);
    let mut session = open_imap_session(imap_config).await?;

    session
        .select("INBOX")
        .await
        .map_err(|e| anyhow!("Failed to select INBOX: {}", e))?;

    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        session.logout().await.ok();
        return Err(anyhow!(
            "Email with Message-ID {} not found in INBOX on server",
            message_id
        ));
    }

    let uid = *uids.iter().next().unwrap();
    let uid_str = uid.to_string();

    session
        .uid_copy(&uid_str, archive_mailbox)
        .await
        .map_err(|e| anyhow!("Failed to copy email to {}: {}", archive_mailbox, e))?;

    session
        .uid_store(&uid_str, "+FLAGS (\\Deleted)")
        .await
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect store response: {}", e))?;

    session
        .expunge()
        .await
        .map_err(|e| anyhow!("Failed to expunge: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect expunge response: {}", e))?;

    session.logout().await.ok();
    Ok(())
}

pub async fn archive_email_locally(imap_config: &ImapConfig, archive_dir: &Path, file_path: &Path, archive_mailbox: &str) -> Result<()> {
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

    let att_source = attachments_dir_for(file_path);
    let att_dest = attachments_dir_for(&dest);
    let had_att_dir = att_source.is_dir();
    if had_att_dir {
        fs::rename(&att_source, &att_dest)?;
    }

    // --- IMAP operation (slow) ---
    if let Err(e) = archive_email_on_server(imap_config, message_id, archive_mailbox).await {
        // Rollback: restore local files to original state
        info!("IMAP archive failed, rolling back local changes: {}", e);
        fs::write(file_path, &content)?;
        fs::remove_file(&dest)?;
        if had_html {
            fs::copy(&html_dest, &html_companion)?;
            fs::remove_file(&html_dest)?;
        }
        if had_att_dir {
            fs::rename(&att_dest, &att_source).ok();
        }
        return Err(e.context("IMAP archive failed; local changes have been rolled back"));
    }

    Ok(())
}

pub async fn delete_email_on_server(imap_config: &ImapConfig, message_id: &str) -> Result<()> {
    info!("Deleting email on server: Message-ID={}", message_id);
    let mut session = open_imap_session(imap_config).await?;

    session
        .select("INBOX")
        .await
        .map_err(|e| anyhow!("Failed to select INBOX: {}", e))?;

    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        session.logout().await.ok();
        return Err(anyhow!(
            "Email with Message-ID {} not found in INBOX on server",
            message_id
        ));
    }

    let uid = *uids.iter().next().unwrap();
    let uid_str = uid.to_string();

    session
        .uid_store(&uid_str, "+FLAGS (\\Deleted)")
        .await
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect store response: {}", e))?;

    session
        .expunge()
        .await
        .map_err(|e| anyhow!("Failed to expunge: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect expunge response: {}", e))?;

    session.logout().await.ok();
    Ok(())
}

pub async fn delete_email_locally(imap_config: &ImapConfig, file_path: &Path) -> Result<()> {
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

    // Move attachment dir to temp location for potential rollback
    let att_dir = attachments_dir_for(file_path);
    let att_backup = att_dir.with_extension("deleting");
    let had_att_dir = att_dir.is_dir();
    if had_att_dir {
        fs::rename(&att_dir, &att_backup)?;
    }

    // --- Local operations (instant) ---
    fs::remove_file(file_path)
        .with_context(|| format!("Failed to remove local file: {}", file_path.display()))?;

    if html_content.is_some() {
        fs::remove_file(&html_companion).ok();
    }

    // --- IMAP operation (slow) ---
    if let Some(ref mid) = message_id {
        if let Err(e) = delete_email_on_server(imap_config, mid).await {
            // Rollback: restore local files
            info!("IMAP delete failed, rolling back local changes: {}", e);
            fs::write(file_path, &content)?;
            if let Some(ref html) = html_content {
                fs::write(&html_companion, html)?;
            }
            if had_att_dir {
                fs::rename(&att_backup, &att_dir).ok();
            }
            return Err(e.context("IMAP delete failed; local changes have been rolled back"));
        }
    } else {
        info!(
            "No message_id in frontmatter -- skipping server deletion for {}",
            file_path.display()
        );
    }

    // Clean up attachment backup on success
    if had_att_dir && att_backup.exists() {
        fs::remove_dir_all(&att_backup).ok();
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Batch operations (single IMAP connection for N emails)
// ---------------------------------------------------------------------------

/// SEARCH + UID COPY + UID STORE \Deleted on an existing session.
/// Does NOT EXPUNGE -- caller batches a single EXPUNGE at the end.
async fn archive_single_on_session(
    session: &mut ImapSession,
    message_id: &str,
    archive_mailbox: &str,
) -> Result<()> {
    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .await
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
        .await
        .map_err(|e| anyhow!("Failed to copy email to {}: {}", archive_mailbox, e))?;

    session
        .uid_store(&uid_str, "+FLAGS (\\Deleted)")
        .await
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect store response: {}", e))?;

    Ok(())
}

/// SEARCH + UID STORE \Deleted on an existing session.
/// Does NOT EXPUNGE -- caller batches a single EXPUNGE at the end.
async fn delete_single_on_session(
    session: &mut ImapSession,
    message_id: &str,
) -> Result<()> {
    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .await
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
        .await
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect store response: {}", e))?;

    Ok(())
}

/// Archive multiple emails using a single IMAP connection.
///
/// Phase 1 (local): For each path, parse frontmatter, write archive copy, remove original.
/// Phase 2 (IMAP): Open one session, archive each email, single EXPUNGE at the end.
/// Phase 3 (rollback): Restore local files for any IMAP failures.
pub async fn batch_archive_emails_locally(
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
        had_att_dir: bool,
        att_source: PathBuf,
        att_dest: PathBuf,
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

            let att_source = attachments_dir_for(path);
            let att_dest = attachments_dir_for(&dest);
            let had_att_dir = att_source.is_dir();
            if had_att_dir {
                fs::rename(&att_source, &att_dest)?;
            }

            Ok(LocalPrep {
                idx: i,
                message_id,
                original_content: content,
                dest,
                had_html,
                html_companion,
                html_dest,
                had_att_dir,
                att_source,
                att_dest,
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

    match open_imap_session(imap_config).await {
        Ok(mut session) => {
            match session
                .select("INBOX")
                .await
                .map_err(|e| anyhow!("Failed to select INBOX: {}", e))
            {
                Ok(_) => {
                    for prep in &prepared {
                        imap_results.push(archive_single_on_session(
                            &mut session,
                            &prep.message_id,
                            archive_mailbox,
                        ).await);
                    }
                    // Single EXPUNGE at the end (non-fatal)
                    if imap_results.iter().any(Result::is_ok) {
                        match session.expunge().await {
                            Ok(stream) => {
                                if let Err(e) = stream.try_collect::<Vec<_>>().await {
                                    info!("Batch EXPUNGE collect failed (non-fatal): {}", e);
                                }
                            }
                            Err(e) => {
                                info!("Batch EXPUNGE failed (non-fatal): {}", e);
                            }
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
            session.logout().await.ok();
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
                if prep.had_att_dir {
                    let _ = fs::rename(&prep.att_dest, &prep.att_source);
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
pub async fn batch_delete_emails_locally(
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
        had_att_dir: bool,
        att_dir: PathBuf,
        att_backup: PathBuf,
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

            let att_dir = attachments_dir_for(path);
            let att_backup = att_dir.with_extension("deleting");
            let had_att_dir = att_dir.is_dir();
            if had_att_dir {
                fs::rename(&att_dir, &att_backup)?;
            }

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
                had_att_dir,
                att_dir,
                att_backup,
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

    match open_imap_session(imap_config).await {
        Ok(mut session) => {
            match session
                .select("INBOX")
                .await
                .map_err(|e| anyhow!("Failed to select INBOX: {}", e))
            {
                Ok(_) => {
                    for prep in &prepared {
                        if let Some(ref mid) = prep.message_id {
                            imap_results.push(delete_single_on_session(&mut session, mid).await);
                        } else {
                            imap_results.push(Ok(()));
                        }
                    }
                    if imap_results.iter().any(Result::is_ok) {
                        match session.expunge().await {
                            Ok(stream) => {
                                if let Err(e) = stream.try_collect::<Vec<_>>().await {
                                    info!("Batch EXPUNGE collect failed (non-fatal): {}", e);
                                }
                            }
                            Err(e) => {
                                info!("Batch EXPUNGE failed (non-fatal): {}", e);
                            }
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
            session.logout().await.ok();
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
                // Clean up attachment backup on success
                if prep.had_att_dir && prep.att_backup.exists() {
                    let _ = fs::remove_dir_all(&prep.att_backup);
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
                if prep.had_att_dir {
                    let _ = fs::rename(&prep.att_backup, &prep.att_dir);
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
