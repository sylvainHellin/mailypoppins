use anyhow::{anyhow, Context, Result};
use log::info;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::parse::attachments_dir_for;

/// Scan a directory for .md files and extract their Message-IDs from frontmatter.
/// Returns a map from message_id to file path.
pub fn scan_local_message_ids(dir: &Path) -> Result<HashMap<String, PathBuf>> {
    let mut ids: HashMap<String, PathBuf> = HashMap::new();
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
pub fn move_local_email(
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

    // Move attachment directory if it exists
    let att_source = attachments_dir_for(file_path);
    if att_source.is_dir() {
        let att_dest = attachments_dir_for(&dest);
        fs::rename(&att_source, &att_dest)?;
    }

    Ok(())
}

/// Reconcile local files against server-side Message-ID sets.
/// Moves files that changed mailbox on the server, deletes files no longer on any server mailbox.
/// Only operates on INBOX and Archive (Sent is skipped -- locally-authored files are source of truth).
pub fn reconcile_local_files(
    server_ids: &HashMap<String, HashSet<String>>,
    local_dirs: &HashMap<String, PathBuf>,
) -> Result<(usize, usize)> {
    let mut moved = 0;
    let mut removed = 0;

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
                    let att_dir = attachments_dir_for(file_path);
                    if att_dir.is_dir() {
                        fs::remove_dir_all(&att_dir)?;
                    }
                    info!("Removed stale copy from {} (already in {}): {}", mb, target_mb, file_path.display());
                } else {
                    // Move to target mailbox
                    move_local_email(file_path, target_dir, mailbox_status(mb), mailbox_status(target_mb))?;
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
                let att_dir = attachments_dir_for(file_path);
                if att_dir.is_dir() {
                    fs::remove_dir_all(&att_dir)?;
                }
                info!("Removed (no longer on server): {}", file_path.display());
                removed += 1;
            }
        }
    }

    Ok((moved, removed))
}

/// Map a mailbox name to the corresponding email status string (case-insensitive).
pub fn mailbox_status(mailbox: &str) -> &'static str {
    if mailbox.eq_ignore_ascii_case("inbox") {
        "inbox"
    } else if mailbox.eq_ignore_ascii_case("archive") {
        "archived"
    } else if mailbox.eq_ignore_ascii_case("sent") {
        "sent"
    } else {
        "inbox"
    }
}
