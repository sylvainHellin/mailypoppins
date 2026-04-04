use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use log::{info, warn};

use super::fetch::{fetch_new_emails_on_session, fetch_server_message_ids_on_session};
use super::open_imap_session;
use crate::config::ImapConfig;
use crate::parse::{save_fetched_emails_with_known_ids, scan_existing_message_ids};
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
