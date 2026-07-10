use email::imap_client::sync_local_read_flags;
use email::parse::{attachments_dir_for, save_fetched_emails_with_known_ids, FetchedEmail};
use email::sync::{move_local_email, reconcile_local_files, scan_local_message_ids};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::time::{Duration, SystemTime};
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

fn make_fetched_email(subject: &str, message_id: Option<&str>) -> FetchedEmail {
    FetchedEmail {
        from: "sender@example.com".to_string(),
        to: "me@example.com".to_string(),
        cc: None,
        subject: subject.to_string(),
        date: "Mon, 01 Jan 2024 12:00:00 +0000".to_string(),
        body_text: "Body".to_string(),
        html_body: None,
        has_attachments: false,
        message_id: message_id.map(|s| s.to_string()),
        attachments: Vec::new(),
        is_read: false,
    }
}

// -----------------------------------------------------------------------
// save_fetched_emails_with_known_ids: returned paths (index plumbing)
// -----------------------------------------------------------------------

#[test]
fn test_save_with_known_ids_returns_saved_paths() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");

    let emails = vec![
        make_fetched_email("First", Some("<one@example.com>")),
        make_fetched_email("Second", Some("<two@example.com>")),
    ];

    let mut known = HashSet::new();
    let (saved, skipped, paths) =
        save_fetched_emails_with_known_ids(&emails, &inbox, "inbox", &mut known).unwrap();

    assert_eq!(saved, 2);
    assert_eq!(skipped, 0);
    assert_eq!(paths.len(), 2);

    // Each returned path must exist and carry the matching message_id in
    // its frontmatter (this is what sync uses to update its index without
    // re-scanning the directory).
    let scanned = scan_local_message_ids(&inbox).unwrap();
    for (mid, path) in &paths {
        assert!(path.exists(), "returned path should exist: {}", path.display());
        let mid = mid.as_deref().expect("message_id should be present");
        assert_eq!(scanned.get(mid), Some(path), "returned path must match frontmatter scan");
    }
    assert_eq!(paths[0].0.as_deref(), Some("<one@example.com>"));
    assert_eq!(paths[1].0.as_deref(), Some("<two@example.com>"));
}

#[test]
fn test_save_with_known_ids_no_path_for_duplicates() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");

    let email = make_fetched_email("Dup", Some("<dup@example.com>"));

    let mut known = HashSet::new();
    let (saved1, _, paths1) =
        save_fetched_emails_with_known_ids(&[email.clone()], &inbox, "inbox", &mut known).unwrap();
    assert_eq!(saved1, 1);
    assert_eq!(paths1.len(), 1);

    // Second save of the same message_id is skipped and returns no path
    let (saved2, skipped2, paths2) =
        save_fetched_emails_with_known_ids(&[email], &inbox, "inbox", &mut known).unwrap();
    assert_eq!(saved2, 0);
    assert_eq!(skipped2, 1);
    assert!(paths2.is_empty());
}

#[test]
fn test_save_with_known_ids_path_without_message_id() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");

    // Emails without a Message-ID are always saved; the path is still returned
    let email = make_fetched_email("No id", None);
    let mut known = HashSet::new();
    let (saved, _, paths) =
        save_fetched_emails_with_known_ids(&[email], &inbox, "inbox", &mut known).unwrap();
    assert_eq!(saved, 1);
    assert_eq!(paths.len(), 1);
    assert!(paths[0].0.is_none());
    assert!(paths[0].1.exists());
}

#[test]
fn test_save_with_known_ids_paths_on_filename_collision() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");

    // Same sender/subject/date -> same base filename, different message_ids.
    // The collision counter kicks in; returned paths must be the real ones.
    let emails = vec![
        make_fetched_email("Same subject", Some("<c1@example.com>")),
        make_fetched_email("Same subject", Some("<c2@example.com>")),
    ];
    let mut known = HashSet::new();
    let (saved, _, paths) =
        save_fetched_emails_with_known_ids(&emails, &inbox, "inbox", &mut known).unwrap();
    assert_eq!(saved, 2);
    assert_eq!(paths.len(), 2);
    assert_ne!(paths[0].1, paths[1].1);

    let scanned = scan_local_message_ids(&inbox).unwrap();
    assert_eq!(scanned.get("<c1@example.com>"), Some(&paths[0].1));
    assert_eq!(scanned.get("<c2@example.com>"), Some(&paths[1].1));
}

// -----------------------------------------------------------------------
// Read-status sync (#0004). `sync_local_read_flags` is the single seam
// through which BOTH backends apply server flags to local files: the IMAP
// orchestrator (`sync_mailboxes`) feeds it the \Seen flags collected in
// pass 1, and the Graph orchestrator (`sync_read_status_graph`) feeds it
// the isRead map from `fetch_message_ids`. Covering it here covers both.
// -----------------------------------------------------------------------

fn write_email_with_read(
    dir: &std::path::Path,
    filename: &str,
    message_id: &str,
    read: bool,
    body: &str,
) -> std::path::PathBuf {
    let path = dir.join(filename);
    let content = format!(
        "---\nfrom: \"sender@example.com\"\nto: \"me@example.com\"\nsubject: \"Test\"\nmessage_id: \"{message_id}\"\nstatus: inbox\nread: {read}\n---\n\n{body}"
    );
    fs::write(&path, content).unwrap();
    path
}

/// Backdate a file's mtime so it clearly predates a "sync started" cutoff
/// (well beyond the 1s mtime-granularity slack in the guard).
fn backdate(path: &std::path::Path, secs: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

/// Server-side read changes (webmail, another client) propagate to the
/// local frontmatter on fetch -- both directions.
#[test]
fn test_read_flags_server_change_propagates_to_local_file() {
    let tmp = tempdir().unwrap();
    let read_path = write_email_with_read(tmp.path(), "a.md", "<a@example.com>", false, "Body");
    let unread_path = write_email_with_read(tmp.path(), "b.md", "<b@example.com>", true, "Body");
    backdate(&read_path, 3600);
    backdate(&unread_path, 3600);

    let mut server_flags = HashMap::new();
    server_flags.insert("<a@example.com>".to_string(), true); // read in webmail
    server_flags.insert("<b@example.com>".to_string(), false); // marked unread in webmail

    let updated = sync_local_read_flags(tmp.path(), &server_flags, Some(SystemTime::now()));
    assert_eq!(updated, 2);
    assert!(fs::read_to_string(&read_path).unwrap().contains("read: true"));
    assert!(fs::read_to_string(&unread_path).unwrap().contains("read: false"));
}

/// A fetch must NOT clobber a local read mark made while the sync was in
/// flight: the file's mtime is at/after the snapshot cutoff, so the (older)
/// server flags are ignored for it. Regression test for the "read status
/// resets after each fetch" symptom of #0004.
#[test]
fn test_read_flags_fetch_does_not_clobber_local_mark_made_during_sync() {
    let tmp = tempdir().unwrap();

    // Cutoff captured when the (simulated) sync started reading the server.
    let cutoff = SystemTime::now() - Duration::from_secs(30);

    // AFTER the cutoff, the user marks the email read locally (fresh mtime).
    let path = write_email_with_read(tmp.path(), "a.md", "<a@example.com>", true, "Body");

    // The sync's server snapshot still says unread.
    let mut server_flags = HashMap::new();
    server_flags.insert("<a@example.com>".to_string(), false);

    let updated = sync_local_read_flags(tmp.path(), &server_flags, Some(cutoff));
    assert_eq!(updated, 0, "stale server snapshot must not revert the local mark");
    assert!(
        fs::read_to_string(&path).unwrap().contains("read: true"),
        "local read mark must survive the fetch"
    );
}

/// The local read state must be parsed from frontmatter, not substring-
/// matched against the whole file. Regression test for the Graph-path bug
/// where a body containing "read: true" masked the frontmatter's
/// `read: false` and blocked unread->read syncing forever.
#[test]
fn test_read_flags_body_text_does_not_mask_frontmatter() {
    let tmp = tempdir().unwrap();
    let path = write_email_with_read(
        tmp.path(),
        "a.md",
        "<a@example.com>",
        false,
        "Quoting docs: set read: true in the frontmatter to mark an email read.",
    );
    backdate(&path, 3600);

    let mut server_flags = HashMap::new();
    server_flags.insert("<a@example.com>".to_string(), true);

    let updated = sync_local_read_flags(tmp.path(), &server_flags, Some(SystemTime::now()));
    assert_eq!(updated, 1, "body text must not be mistaken for the read: frontmatter field");
    // The frontmatter flips; the body stays untouched.
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("\nread: true\n"));
    assert!(content.contains("set read: true in the frontmatter"));
}

/// Flags for messages with no matching local file are ignored (no panic,
/// no spurious writes), and agreement is a no-op.
#[test]
fn test_read_flags_unknown_ids_and_agreement_are_noops() {
    let tmp = tempdir().unwrap();
    let path = write_email_with_read(tmp.path(), "a.md", "<a@example.com>", true, "Body");
    backdate(&path, 3600);
    let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();

    let mut server_flags = HashMap::new();
    server_flags.insert("<a@example.com>".to_string(), true); // agrees
    server_flags.insert("<ghost@example.com>".to_string(), false); // no local file

    let updated = sync_local_read_flags(tmp.path(), &server_flags, Some(SystemTime::now()));
    assert_eq!(updated, 0);
    // Agreement must not rewrite the file (would defeat the mtime guard).
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), mtime_before);
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
