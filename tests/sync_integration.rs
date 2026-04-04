use email::parse::attachments_dir_for;
use email::sync::{move_local_email, reconcile_local_files, scan_local_message_ids};
use std::collections::{HashMap, HashSet};
use std::fs;
use tempfile::tempdir;

/// Write a minimal .md file with a message_id in frontmatter.
fn write_email_with_id(
    dir: &std::path::Path,
    filename: &str,
    message_id: &str,
    status: &str,
) -> std::path::PathBuf {
    let path = dir.join(filename);
    let content = format!(
        "---\nfrom: \"sender@example.com\"\nto: \"me@example.com\"\nsubject: \"Test\"\nmessage_id: \"{message_id}\"\nstatus: {status}\n---\n\nBody"
    );
    fs::write(&path, content).unwrap();
    path
}

// -----------------------------------------------------------------------
// scan_local_message_ids
// -----------------------------------------------------------------------

#[test]
fn test_scan_local_message_ids() {
    let tmp = tempdir().unwrap();
    write_email_with_id(tmp.path(), "a.md", "<a@example.com>", "inbox");
    write_email_with_id(tmp.path(), "b.md", "<b@example.com>", "inbox");

    // Write a non-email file that should be ignored
    fs::write(tmp.path().join("notes.txt"), "not an email").unwrap();

    let ids = scan_local_message_ids(tmp.path()).unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains_key("<a@example.com>"));
    assert!(ids.contains_key("<b@example.com>"));
}

#[test]
fn test_scan_local_message_ids_empty_dir() {
    let tmp = tempdir().unwrap();
    let ids = scan_local_message_ids(tmp.path()).unwrap();
    assert!(ids.is_empty());
}

#[test]
fn test_scan_local_message_ids_nonexistent_dir() {
    let ids = scan_local_message_ids(std::path::Path::new("/nonexistent/path")).unwrap();
    assert!(ids.is_empty());
}

// -----------------------------------------------------------------------
// move_local_email
// -----------------------------------------------------------------------

#[test]
fn test_move_local_email_basic() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    let archive = tmp.path().join("archive");
    fs::create_dir_all(&inbox).unwrap();

    let source = write_email_with_id(&inbox, "email.md", "<move@example.com>", "inbox");

    move_local_email(&source, &archive, "inbox", "archived").unwrap();

    // Source should be gone
    assert!(!source.exists());

    // Destination should exist with updated status
    let dest = archive.join("email.md");
    assert!(dest.exists());
    let content = fs::read_to_string(&dest).unwrap();
    assert!(content.contains("status: archived"));
    assert!(!content.contains("status: inbox"));
}

#[test]
fn test_move_local_email_with_html_companion() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    let archive = tmp.path().join("archive");
    fs::create_dir_all(&inbox).unwrap();

    let source = write_email_with_id(&inbox, "email.md", "<html@example.com>", "inbox");
    let html_source = inbox.join("email.html");
    fs::write(&html_source, "<p>HTML body</p>").unwrap();

    move_local_email(&source, &archive, "inbox", "archived").unwrap();

    assert!(!source.exists());
    assert!(!html_source.exists());

    let html_dest = archive.join("email.html");
    assert!(html_dest.exists());
    assert_eq!(fs::read_to_string(&html_dest).unwrap(), "<p>HTML body</p>");
}

#[test]
fn test_move_local_email_with_attachments() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    let archive = tmp.path().join("archive");
    fs::create_dir_all(&inbox).unwrap();

    let source = write_email_with_id(&inbox, "email.md", "<att@example.com>", "inbox");
    let att_dir = attachments_dir_for(&source);
    fs::create_dir_all(&att_dir).unwrap();
    fs::write(att_dir.join("file.pdf"), b"pdf content").unwrap();

    move_local_email(&source, &archive, "inbox", "archived").unwrap();

    assert!(!att_dir.exists());

    let dest = archive.join("email.md");
    let dest_att_dir = attachments_dir_for(&dest);
    assert!(dest_att_dir.exists());
    assert!(dest_att_dir.join("file.pdf").exists());
}

// -----------------------------------------------------------------------
// reconcile_local_files
// -----------------------------------------------------------------------

#[test]
fn test_reconcile_moves_email_between_mailboxes() {
    let tmp = tempdir().unwrap();
    let inbox_dir = tmp.path().join("inbox");
    let archive_dir = tmp.path().join("archive");
    fs::create_dir_all(&inbox_dir).unwrap();
    fs::create_dir_all(&archive_dir).unwrap();

    // Email is in local inbox but server says it's now in Archive
    write_email_with_id(&inbox_dir, "email.md", "<moved@example.com>", "inbox");

    let mut server_ids: HashMap<String, HashSet<String>> = HashMap::new();
    server_ids.insert("INBOX".to_string(), HashSet::new()); // not in INBOX on server
    server_ids.insert(
        "Archive".to_string(),
        ["<moved@example.com>".to_string()].into_iter().collect(),
    );

    let mut local_dirs: HashMap<String, std::path::PathBuf> = HashMap::new();
    local_dirs.insert("INBOX".to_string(), inbox_dir.clone());
    local_dirs.insert("Archive".to_string(), archive_dir.clone());

    let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs).unwrap();

    assert_eq!(moved, 1);
    assert_eq!(removed, 0);

    // Email should have been moved to archive
    assert!(!inbox_dir.join("email.md").exists());
    assert!(archive_dir.join("email.md").exists());
}

#[test]
fn test_reconcile_removes_deleted_email() {
    let tmp = tempdir().unwrap();
    let inbox_dir = tmp.path().join("inbox");
    let archive_dir = tmp.path().join("archive");
    fs::create_dir_all(&inbox_dir).unwrap();
    fs::create_dir_all(&archive_dir).unwrap();

    // Email in local inbox but not on server anywhere
    write_email_with_id(&inbox_dir, "email.md", "<gone@example.com>", "inbox");

    let mut server_ids: HashMap<String, HashSet<String>> = HashMap::new();
    server_ids.insert("INBOX".to_string(), HashSet::new());
    server_ids.insert("Archive".to_string(), HashSet::new());

    let mut local_dirs: HashMap<String, std::path::PathBuf> = HashMap::new();
    local_dirs.insert("INBOX".to_string(), inbox_dir.clone());
    local_dirs.insert("Archive".to_string(), archive_dir.clone());

    let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs).unwrap();

    assert_eq!(moved, 0);
    assert_eq!(removed, 1);
    assert!(!inbox_dir.join("email.md").exists());
}

#[test]
fn test_reconcile_stale_copy_deleted() {
    let tmp = tempdir().unwrap();
    let inbox_dir = tmp.path().join("inbox");
    let archive_dir = tmp.path().join("archive");
    fs::create_dir_all(&inbox_dir).unwrap();
    fs::create_dir_all(&archive_dir).unwrap();

    // Email exists in both local dirs (stale copy in inbox, fresh copy in archive)
    write_email_with_id(&inbox_dir, "email.md", "<stale@example.com>", "inbox");
    write_email_with_id(&archive_dir, "email.md", "<stale@example.com>", "archived");

    let mut server_ids: HashMap<String, HashSet<String>> = HashMap::new();
    server_ids.insert("INBOX".to_string(), HashSet::new());
    server_ids.insert(
        "Archive".to_string(),
        ["<stale@example.com>".to_string()].into_iter().collect(),
    );

    let mut local_dirs: HashMap<String, std::path::PathBuf> = HashMap::new();
    local_dirs.insert("INBOX".to_string(), inbox_dir.clone());
    local_dirs.insert("Archive".to_string(), archive_dir.clone());

    let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs).unwrap();

    // Stale copy removed from inbox, archive copy still there
    assert_eq!(moved, 1); // counted as a "move" operation
    assert_eq!(removed, 0);
    assert!(!inbox_dir.join("email.md").exists());
    assert!(archive_dir.join("email.md").exists());
}

#[test]
fn test_reconcile_no_changes_needed() {
    let tmp = tempdir().unwrap();
    let inbox_dir = tmp.path().join("inbox");
    let archive_dir = tmp.path().join("archive");
    fs::create_dir_all(&inbox_dir).unwrap();
    fs::create_dir_all(&archive_dir).unwrap();

    write_email_with_id(&inbox_dir, "email.md", "<still@example.com>", "inbox");

    let mut server_ids: HashMap<String, HashSet<String>> = HashMap::new();
    server_ids.insert(
        "INBOX".to_string(),
        ["<still@example.com>".to_string()].into_iter().collect(),
    );
    server_ids.insert("Archive".to_string(), HashSet::new());

    let mut local_dirs: HashMap<String, std::path::PathBuf> = HashMap::new();
    local_dirs.insert("INBOX".to_string(), inbox_dir.clone());
    local_dirs.insert("Archive".to_string(), archive_dir.clone());

    let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs).unwrap();

    assert_eq!(moved, 0);
    assert_eq!(removed, 0);
    assert!(inbox_dir.join("email.md").exists());
}
