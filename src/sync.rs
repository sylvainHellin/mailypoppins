use anyhow::{anyhow, Context, Result};
use log::info;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::parse::{attachments_dir_for, scan_mailbox_message_ids};

/// Scan a directory for .md files and extract their Message-IDs from frontmatter.
/// Returns a map from message_id to file path.
pub fn scan_local_message_ids(dir: &Path) -> Result<HashMap<String, PathBuf>> {
    scan_mailbox_message_ids(dir)
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

/// Dry-run reconciliation: counts what *would* be moved or removed without changing any files.
pub fn dry_run_reconcile(
    server_ids: &HashMap<String, HashSet<String>>,
    local_dirs: &HashMap<String, PathBuf>,
) -> Result<(usize, usize)> {
    let mut would_move = 0;
    let mut would_remove = 0;

    let reconcile_mailboxes = ["INBOX", "Archive"];

    for mb in &reconcile_mailboxes {
        let Some(local_dir) = local_dirs.get(*mb) else { continue };
        let Some(server_set) = server_ids.get(*mb) else { continue };

        let local_ids = scan_local_message_ids(local_dir)?;

        for (mid, file_path) in &local_ids {
            if server_set.contains(mid) {
                continue;
            }

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
                info!(
                    "dry-run: would move from {} to {}: {}",
                    mb, target_mb, file_path.display()
                );
                would_move += 1;
            } else {
                info!(
                    "dry-run: would remove (no longer on server): {}",
                    file_path.display()
                );
                would_remove += 1;
            }
        }
    }

    Ok((would_move, would_remove))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mailbox_status_inbox() {
        assert_eq!(mailbox_status("INBOX"), "inbox");
        assert_eq!(mailbox_status("inbox"), "inbox");
        assert_eq!(mailbox_status("Inbox"), "inbox");
    }

    #[test]
    fn test_mailbox_status_archive() {
        assert_eq!(mailbox_status("Archive"), "archived");
        assert_eq!(mailbox_status("archive"), "archived");
        assert_eq!(mailbox_status("ARCHIVE"), "archived");
    }

    #[test]
    fn test_mailbox_status_sent() {
        assert_eq!(mailbox_status("Sent"), "sent");
        assert_eq!(mailbox_status("sent"), "sent");
    }

    #[test]
    fn test_mailbox_status_unknown() {
        assert_eq!(mailbox_status("Spam"), "inbox");
        assert_eq!(mailbox_status("Trash"), "inbox");
    }

    // -----------------------------------------------------------------------
    // move_local_email (filesystem)
    // -----------------------------------------------------------------------

    #[test]
    fn test_move_local_email_basic() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();

        let content = "---\nstatus: inbox\n---\n\nEmail body";
        let src_file = src_dir.path().join("email.md");
        std::fs::write(&src_file, content).unwrap();

        move_local_email(&src_file, dst_dir.path(), "inbox", "archived").unwrap();

        assert!(!src_file.exists());
        let moved_file = dst_dir.path().join("email.md");
        assert!(moved_file.exists());
        let new_content = std::fs::read_to_string(&moved_file).unwrap();
        assert!(new_content.contains("status: archived"));
        assert!(!new_content.contains("status: inbox"));
    }

    #[test]
    fn test_move_local_email_with_html_companion() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();

        let src_md = src_dir.path().join("email.md");
        let src_html = src_dir.path().join("email.html");
        std::fs::write(&src_md, "---\nstatus: inbox\n---\n\nBody").unwrap();
        std::fs::write(&src_html, "<p>HTML body</p>").unwrap();

        move_local_email(&src_md, dst_dir.path(), "inbox", "archived").unwrap();

        assert!(!src_md.exists());
        assert!(!src_html.exists());
        assert!(dst_dir.path().join("email.md").exists());
        assert!(dst_dir.path().join("email.html").exists());
    }

    #[test]
    fn test_move_local_email_with_attachments_dir() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();

        let src_md = src_dir.path().join("email.md");
        std::fs::write(&src_md, "---\nstatus: inbox\n---\n\nBody").unwrap();
        let att_dir = src_dir.path().join("email_attachments");
        std::fs::create_dir(&att_dir).unwrap();
        std::fs::write(att_dir.join("doc.pdf"), b"pdf").unwrap();

        move_local_email(&src_md, dst_dir.path(), "inbox", "archived").unwrap();

        assert!(!att_dir.exists());
        let moved_att = dst_dir.path().join("email_attachments");
        assert!(moved_att.is_dir());
        assert!(moved_att.join("doc.pdf").exists());
    }

    #[test]
    fn test_move_local_email_creates_target_dir() {
        let src_dir = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let dst_dir = base.path().join("nested").join("archive");

        let src_file = src_dir.path().join("email.md");
        std::fs::write(&src_file, "---\nstatus: inbox\n---\n\nBody").unwrap();

        move_local_email(&src_file, &dst_dir, "inbox", "archived").unwrap();

        assert!(dst_dir.join("email.md").exists());
    }

    // -----------------------------------------------------------------------
    // reconcile_local_files (filesystem)
    // -----------------------------------------------------------------------

    #[test]
    fn test_reconcile_nothing_to_do() {
        let inbox_dir = tempfile::tempdir().unwrap();
        let archive_dir = tempfile::tempdir().unwrap();

        // Email in INBOX, also on server in INBOX
        let content = "---\nmessage_id: \"<a@x.com>\"\nstatus: inbox\n---\n\nBody";
        std::fs::write(inbox_dir.path().join("email.md"), content).unwrap();

        let mut server_ids = HashMap::new();
        let mut inbox_ids = std::collections::HashSet::new();
        inbox_ids.insert("<a@x.com>".to_string());
        server_ids.insert("INBOX".to_string(), inbox_ids);
        server_ids.insert("Archive".to_string(), std::collections::HashSet::new());

        let mut local_dirs = HashMap::new();
        local_dirs.insert("INBOX".to_string(), inbox_dir.path().to_path_buf());
        local_dirs.insert("Archive".to_string(), archive_dir.path().to_path_buf());

        let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs).unwrap();
        assert_eq!(moved, 0);
        assert_eq!(removed, 0);
        assert!(inbox_dir.path().join("email.md").exists());
    }

    #[test]
    fn test_reconcile_removes_deleted_email() {
        let inbox_dir = tempfile::tempdir().unwrap();
        let archive_dir = tempfile::tempdir().unwrap();

        // Email exists locally but NOT on server anywhere
        let content = "---\nmessage_id: \"<gone@x.com>\"\nstatus: inbox\n---\n\nBody";
        std::fs::write(inbox_dir.path().join("email.md"), content).unwrap();

        let mut server_ids = HashMap::new();
        server_ids.insert("INBOX".to_string(), std::collections::HashSet::new());
        server_ids.insert("Archive".to_string(), std::collections::HashSet::new());

        let mut local_dirs = HashMap::new();
        local_dirs.insert("INBOX".to_string(), inbox_dir.path().to_path_buf());
        local_dirs.insert("Archive".to_string(), archive_dir.path().to_path_buf());

        let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs).unwrap();
        assert_eq!(moved, 0);
        assert_eq!(removed, 1);
        assert!(!inbox_dir.path().join("email.md").exists());
    }

    #[test]
    fn test_reconcile_detects_move_to_archive() {
        let inbox_dir = tempfile::tempdir().unwrap();
        let archive_dir = tempfile::tempdir().unwrap();

        // Email in local INBOX, but on server only in Archive
        let content = "---\nmessage_id: \"<moved@x.com>\"\nstatus: inbox\n---\n\nBody";
        std::fs::write(inbox_dir.path().join("email.md"), content).unwrap();

        let mut server_ids = HashMap::new();
        server_ids.insert("INBOX".to_string(), std::collections::HashSet::new());
        let mut archive_ids = std::collections::HashSet::new();
        archive_ids.insert("<moved@x.com>".to_string());
        server_ids.insert("Archive".to_string(), archive_ids);

        let mut local_dirs = HashMap::new();
        local_dirs.insert("INBOX".to_string(), inbox_dir.path().to_path_buf());
        local_dirs.insert("Archive".to_string(), archive_dir.path().to_path_buf());

        let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs).unwrap();
        assert_eq!(moved, 1);
        assert_eq!(removed, 0);
        assert!(!inbox_dir.path().join("email.md").exists());
        assert!(archive_dir.path().join("email.md").exists());
        let new_content = std::fs::read_to_string(archive_dir.path().join("email.md")).unwrap();
        assert!(new_content.contains("status: archived"));
    }

    #[test]
    fn test_reconcile_removes_stale_copy_when_already_in_target() {
        let inbox_dir = tempfile::tempdir().unwrap();
        let archive_dir = tempfile::tempdir().unwrap();

        // Email in both local INBOX and local Archive, server says it's in Archive only
        let content_inbox = "---\nmessage_id: \"<dup@x.com>\"\nstatus: inbox\n---\n\nOld copy";
        std::fs::write(inbox_dir.path().join("email.md"), content_inbox).unwrap();
        let content_archive = "---\nmessage_id: \"<dup@x.com>\"\nstatus: archived\n---\n\nNew copy";
        std::fs::write(archive_dir.path().join("email.md"), content_archive).unwrap();

        let mut server_ids = HashMap::new();
        server_ids.insert("INBOX".to_string(), std::collections::HashSet::new());
        let mut archive_ids = std::collections::HashSet::new();
        archive_ids.insert("<dup@x.com>".to_string());
        server_ids.insert("Archive".to_string(), archive_ids);

        let mut local_dirs = HashMap::new();
        local_dirs.insert("INBOX".to_string(), inbox_dir.path().to_path_buf());
        local_dirs.insert("Archive".to_string(), archive_dir.path().to_path_buf());

        let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs).unwrap();
        assert_eq!(moved, 1); // counted as moved
        assert_eq!(removed, 0);
        // Stale copy in inbox should be gone, archive copy remains
        assert!(!inbox_dir.path().join("email.md").exists());
        assert!(archive_dir.path().join("email.md").exists());
    }

    // -----------------------------------------------------------------------
    // dry_run_reconcile (filesystem)
    // -----------------------------------------------------------------------

    #[test]
    fn test_dry_run_reconcile_counts_moves_and_removes() {
        let inbox_dir = tempfile::tempdir().unwrap();
        let archive_dir = tempfile::tempdir().unwrap();

        // Email 1: in local INBOX, on server only in Archive -> would move
        let c1 = "---\nmessage_id: \"<move@x.com>\"\nstatus: inbox\n---\n\nBody";
        std::fs::write(inbox_dir.path().join("e1.md"), c1).unwrap();

        // Email 2: in local INBOX, not on server anywhere -> would remove
        let c2 = "---\nmessage_id: \"<gone@x.com>\"\nstatus: inbox\n---\n\nBody";
        std::fs::write(inbox_dir.path().join("e2.md"), c2).unwrap();

        let mut server_ids = HashMap::new();
        server_ids.insert("INBOX".to_string(), std::collections::HashSet::new());
        let mut archive_ids = std::collections::HashSet::new();
        archive_ids.insert("<move@x.com>".to_string());
        server_ids.insert("Archive".to_string(), archive_ids);

        let mut local_dirs = HashMap::new();
        local_dirs.insert("INBOX".to_string(), inbox_dir.path().to_path_buf());
        local_dirs.insert("Archive".to_string(), archive_dir.path().to_path_buf());

        let (would_move, would_remove) = dry_run_reconcile(&server_ids, &local_dirs).unwrap();
        assert_eq!(would_move, 1);
        assert_eq!(would_remove, 1);

        // Files should NOT have been modified
        assert!(inbox_dir.path().join("e1.md").exists());
        assert!(inbox_dir.path().join("e2.md").exists());
    }

    #[test]
    fn test_dry_run_reconcile_no_changes_needed() {
        let inbox_dir = tempfile::tempdir().unwrap();
        let archive_dir = tempfile::tempdir().unwrap();

        let c = "---\nmessage_id: \"<ok@x.com>\"\nstatus: inbox\n---\n\nBody";
        std::fs::write(inbox_dir.path().join("e.md"), c).unwrap();

        let mut server_ids = HashMap::new();
        let mut inbox_ids = std::collections::HashSet::new();
        inbox_ids.insert("<ok@x.com>".to_string());
        server_ids.insert("INBOX".to_string(), inbox_ids);
        server_ids.insert("Archive".to_string(), std::collections::HashSet::new());

        let mut local_dirs = HashMap::new();
        local_dirs.insert("INBOX".to_string(), inbox_dir.path().to_path_buf());
        local_dirs.insert("Archive".to_string(), archive_dir.path().to_path_buf());

        let (would_move, would_remove) = dry_run_reconcile(&server_ids, &local_dirs).unwrap();
        assert_eq!(would_move, 0);
        assert_eq!(would_remove, 0);
    }

    #[test]
    fn test_reconcile_deletes_html_companion_and_attachments() {
        let inbox_dir = tempfile::tempdir().unwrap();
        let archive_dir = tempfile::tempdir().unwrap();

        let content = "---\nmessage_id: \"<cleanup@x.com>\"\nstatus: inbox\n---\n\nBody";
        std::fs::write(inbox_dir.path().join("email.md"), content).unwrap();
        std::fs::write(inbox_dir.path().join("email.html"), "<p>HTML</p>").unwrap();
        let att_dir = inbox_dir.path().join("email_attachments");
        std::fs::create_dir(&att_dir).unwrap();
        std::fs::write(att_dir.join("file.pdf"), b"data").unwrap();

        let mut server_ids = HashMap::new();
        server_ids.insert("INBOX".to_string(), std::collections::HashSet::new());
        server_ids.insert("Archive".to_string(), std::collections::HashSet::new());

        let mut local_dirs = HashMap::new();
        local_dirs.insert("INBOX".to_string(), inbox_dir.path().to_path_buf());
        local_dirs.insert("Archive".to_string(), archive_dir.path().to_path_buf());

        reconcile_local_files(&server_ids, &local_dirs).unwrap();

        assert!(!inbox_dir.path().join("email.md").exists());
        assert!(!inbox_dir.path().join("email.html").exists());
        assert!(!att_dir.exists());
    }
}
