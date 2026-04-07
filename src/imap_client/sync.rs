use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use log::{info, warn};

use super::fetch::{fetch_new_emails_on_session, fetch_server_message_ids_on_session};
use super::open_imap_session;
use super::ops::update_read_status_locally;
use crate::config::ImapConfig;
use crate::parse::{save_fetched_emails_with_known_ids, scan_existing_message_ids, scan_mailbox_message_ids};
use crate::sync::reconcile_local_files;

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
    pub read_updated: usize,
}

/// Unified sync orchestrator: fetches all mailboxes using a single IMAP connection,
/// with optional reconciliation pass.
///
/// When `dry_run` is true, connects to the server and checks what *would* change
/// but does not save any files or perform any local mutations.
pub async fn sync_mailboxes(
    imap_config: &ImapConfig,
    targets: &[SyncTarget],
    limit: usize,
    reconcile: bool,
    dry_run: bool,
) -> Result<SyncResult> {
    info!(
        "sync_mailboxes: {} targets, limit={}, reconcile={}, dry_run={}",
        targets.len(),
        limit,
        reconcile,
        dry_run
    );

    let mut session = open_imap_session(imap_config).await?;
    let mut total_saved = 0usize;
    let mut total_skipped = 0usize;
    let mut total_read_updated = 0usize;

    // Build a global known_ids set from ALL local directories.
    // This prevents re-fetching an email to one mailbox if it's already
    // stored in another local mailbox directory (e.g. archived locally but
    // still in server INBOX, or found via search and saved to Archive).
    let mut global_known_ids = std::collections::HashSet::new();
    for target in targets {
        if let Ok(ids) = scan_existing_message_ids(&target.local_dir) {
            global_known_ids.extend(ids);
        }
    }

    // Phase 1: Additive sync with two-pass fetch
    for target in targets {
        // Start from global known_ids so we skip emails already stored anywhere locally
        let mut known_ids = global_known_ids.clone();

        if dry_run {
            // Dry run: only do pass 1 (header check) to count new messages
            match count_new_emails_on_session(
                &mut session,
                &target.server_name,
                Some(limit),
                &known_ids,
            )
            .await
            {
                Ok((new_count, skipped)) => {
                    total_skipped += skipped;
                    total_saved += new_count;
                }
                Err(e) => {
                    warn!(
                        "Failed to check mailbox '{}': {}. Continuing with next.",
                        target.server_name, e
                    );
                }
            }
        } else {
            match fetch_new_emails_on_session(
                &mut session,
                &target.server_name,
                Some(limit),
                &known_ids,
            )
            .await
            {
                Ok((new_emails, skipped, known_read_flags)) => {
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
                    // Sync read status from server for existing local emails
                    if !known_read_flags.is_empty() {
                        total_read_updated += sync_local_read_flags(
                            &target.local_dir,
                            &known_read_flags,
                        );
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
            let mut server_ids: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
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
                if dry_run {
                    let (moved, removed) =
                        crate::sync::dry_run_reconcile(&server_ids, &local_dirs)?;
                    total_moved = moved;
                    total_removed = removed;
                } else {
                    let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs)?;
                    total_moved = moved;
                    total_removed = removed;
                }
            }
        }
    }

    session.logout().await.ok();

    Ok(SyncResult {
        saved: total_saved,
        skipped: total_skipped,
        moved: total_moved,
        removed: total_removed,
        read_updated: total_read_updated,
    })
}

/// Update local emails' `read:` frontmatter to match server \Seen flags.
/// Returns the number of files updated.
fn sync_local_read_flags(
    local_dir: &std::path::Path,
    server_flags: &HashMap<String, bool>,
) -> usize {
    let local_ids = match scan_mailbox_message_ids(local_dir) {
        Ok(ids) => ids,
        Err(_) => return 0,
    };

    let mut updated = 0usize;
    for (message_id, is_seen) in server_flags {
        if let Some(file_path) = local_ids.get(message_id) {
            // Read current local status from frontmatter
            let local_read = read_local_read_status(file_path);
            if local_read != *is_seen {
                if update_read_status_locally(file_path, *is_seen).is_ok() {
                    updated += 1;
                }
            }
        }
    }
    updated
}

/// Read the `read:` field from a frontmatter file. Returns false if missing or unreadable.
fn read_local_read_status(path: &std::path::Path) -> bool {
    if let Ok(content) = std::fs::read_to_string(path) {
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
            if in_frontmatter && line.starts_with("read:") {
                let val = line.trim_start_matches("read:").trim();
                return val == "true";
            }
        }
    }
    false
}

/// Lightweight pass-1-only check: counts how many new emails exist on the server
/// without downloading full message bodies. Used for dry-run mode.
async fn count_new_emails_on_session(
    session: &mut super::ImapSession,
    mailbox: &str,
    limit: Option<usize>,
    known_ids: &std::collections::HashSet<String>,
) -> Result<(usize, usize)> {
    use anyhow::anyhow;
    use crate::parse::compress_uid_set;
    use futures::TryStreamExt;

    session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    let uids = session
        .uid_search("ALL")
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        return Ok((0, 0));
    }

    let mut uid_list: Vec<u32> = uids.into_iter().collect();
    uid_list.sort();
    let selected_uids: Vec<u32> = match limit {
        Some(n) => uid_list.into_iter().rev().take(n).collect(),
        None => uid_list,
    };

    if selected_uids.is_empty() {
        return Ok((0, 0));
    }

    let checked_count = selected_uids.len();
    let uid_set = compress_uid_set(&selected_uids);

    let header_fetched: Vec<_> = session
        .uid_fetch(&uid_set, "(UID BODY.PEEK[HEADER.FIELDS (Message-ID)])")
        .await
        .map_err(|e| anyhow!("Failed to fetch Message-ID headers: {}", e))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect Message-ID headers: {}", e))?;

    let mut new_count = 0usize;
    for msg in header_fetched.iter() {
        let mid = msg
            .header()
            .and_then(super::fetch::parse_message_id_from_header_bytes);
        match mid {
            Some(ref id) if known_ids.contains(id) => {}
            _ => new_count += 1,
        }
    }

    let skipped = checked_count - new_count;
    Ok((new_count, skipped))
}

pub async fn list_mailboxes(imap_config: &ImapConfig) -> Result<Vec<String>> {
    use futures::TryStreamExt;
    use anyhow::anyhow;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_read_local_read_status_true() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(&path, "---\nfrom: a@x.com\nread: true\nstatus: inbox\n---\n\nBody").unwrap();
        assert!(read_local_read_status(&path));
    }

    #[test]
    fn test_read_local_read_status_false() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(&path, "---\nfrom: a@x.com\nread: false\nstatus: inbox\n---\n\nBody").unwrap();
        assert!(!read_local_read_status(&path));
    }

    #[test]
    fn test_read_local_read_status_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(&path, "---\nfrom: a@x.com\nstatus: inbox\n---\n\nBody").unwrap();
        assert!(!read_local_read_status(&path));
    }

    #[test]
    fn test_sync_local_read_flags_updates_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(
            &path,
            "---\nfrom: a@x.com\nmessage_id: \"<msg1@x.com>\"\nread: false\nstatus: inbox\n---\n\nBody",
        ).unwrap();

        let mut flags = HashMap::new();
        flags.insert("<msg1@x.com>".to_string(), true);

        let updated = sync_local_read_flags(dir.path(), &flags);
        assert_eq!(updated, 1);

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("read: true"));
    }

    #[test]
    fn test_sync_local_read_flags_no_change_needed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(
            &path,
            "---\nfrom: a@x.com\nmessage_id: \"<msg1@x.com>\"\nread: true\nstatus: inbox\n---\n\nBody",
        ).unwrap();

        let mut flags = HashMap::new();
        flags.insert("<msg1@x.com>".to_string(), true);

        let updated = sync_local_read_flags(dir.path(), &flags);
        assert_eq!(updated, 0);
    }

    #[test]
    fn test_sync_local_read_flags_marks_unread() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(
            &path,
            "---\nfrom: a@x.com\nmessage_id: \"<msg1@x.com>\"\nread: true\nstatus: inbox\n---\n\nBody",
        ).unwrap();

        let mut flags = HashMap::new();
        flags.insert("<msg1@x.com>".to_string(), false);

        let updated = sync_local_read_flags(dir.path(), &flags);
        assert_eq!(updated, 1);

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("read: false"));
    }
}
