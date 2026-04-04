use std::collections::HashSet;

use anyhow::{anyhow, Result};
use futures::TryStreamExt;
use log::info;

use super::{ImapSession, search::{FetchCriteria, build_imap_search_query}, open_imap_session};
use crate::config::ImapConfig;
use crate::parse::{compress_uid_set, parse_rfc822_to_fetched_email, FetchedEmail};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_message_id_from_header_bytes_standard() {
        let header = b"Message-ID: <abc123@mail.example.com>";
        assert_eq!(
            parse_message_id_from_header_bytes(header),
            Some("<abc123@mail.example.com>".to_string())
        );
    }

    #[test]
    fn test_parse_message_id_from_header_bytes_case_insensitive() {
        let header = b"Message-Id: <def456@mail.example.com>\nOther: header";
        assert_eq!(
            parse_message_id_from_header_bytes(header),
            Some("<def456@mail.example.com>".to_string())
        );

        let header = b"message-id: <xyz@mail.example.com>";
        assert_eq!(
            parse_message_id_from_header_bytes(header),
            Some("<xyz@mail.example.com>".to_string())
        );
    }

    #[test]
    fn test_parse_message_id_from_header_bytes_with_whitespace() {
        let header = b"Message-ID:   <whitespace@mail.example.com>  ";
        assert_eq!(
            parse_message_id_from_header_bytes(header),
            Some("<whitespace@mail.example.com>".to_string())
        );
    }

    #[test]
    fn test_parse_message_id_from_header_bytes_not_found() {
        let header = b"From: alice@example.com\nSubject: Test\nDate: Mon, 1 Jan 2024";
        assert_eq!(parse_message_id_from_header_bytes(header), None);
    }

    #[test]
    fn test_parse_message_id_from_header_bytes_empty_value() {
        let header = b"Message-ID: ";
        assert_eq!(parse_message_id_from_header_bytes(header), None);

        let header = b"Message-ID:";
        assert_eq!(parse_message_id_from_header_bytes(header), None);
    }

    #[test]
    fn test_parse_message_id_from_header_bytes_utf8() {
        let header = b"Message-ID: <utf8-test-\xc2\xb5@mail.example.com>";
        assert!(parse_message_id_from_header_bytes(header).is_some());
    }
}
