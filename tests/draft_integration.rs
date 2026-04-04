use email::draft::{
    create_forward_draft, create_reply_draft, find_drafts, mark_as_approved, parse_email_draft,
    update_status_to_sent, validate_draft,
};
use email::types::EmailStatus;
use std::fs;
use tempfile::tempdir;

/// Write a minimal inbox email file and return its path.
fn write_inbox_email(dir: &std::path::Path, filename: &str, from: &str, subject: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(filename);
    let content = format!(
        "---\nfrom: \"{from}\"\nto: \"me@example.com\"\nsubject: \"{subject}\"\ndate: \"Mon, 01 Jan 2024 12:00:00 +0000\"\nmessage_id: \"<{filename}@example.com>\"\nstatus: inbox\nhas_attachments: false\n---\n\n{body}"
    );
    fs::write(&path, content).unwrap();
    path
}

fn write_draft(dir: &std::path::Path, filename: &str, to: &str, subject: &str, body: &str, status: &str) -> std::path::PathBuf {
    let path = dir.join(filename);
    let content = format!(
        "---\nfrom: \"me@example.com\"\nto: \"{to}\"\nsubject: \"{subject}\"\nstatus: {status}\n---\n\n{body}"
    );
    fs::write(&path, content).unwrap();
    path
}

// -----------------------------------------------------------------------
// Reply drafts
// -----------------------------------------------------------------------

#[test]
fn test_create_reply_draft() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    let drafts = tmp.path().join("drafts");
    fs::create_dir_all(&inbox).unwrap();
    fs::create_dir_all(&drafts).unwrap();

    let source = write_inbox_email(&inbox, "email.md", "alice@example.com", "Hello", "Original body");

    let draft_path = create_reply_draft(&source, false, "me@example.com", Some(drafts.as_path())).unwrap();

    assert!(draft_path.exists());
    let content = fs::read_to_string(&draft_path).unwrap();

    // Check frontmatter fields
    assert!(content.contains("to: \"alice@example.com\""));
    assert!(content.contains("subject: \"Re: Hello\""));
    assert!(content.contains("status: draft"));
    assert!(content.contains("from: \"me@example.com\""));

    // Check body structure
    assert!(content.contains("{{SIGNATURE}}"));
    assert!(content.contains("> Original body"));
    assert!(content.contains("alice@example.com wrote:"));
}

#[test]
fn test_create_reply_draft_already_re_prefix() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    let drafts = tmp.path().join("drafts");
    fs::create_dir_all(&inbox).unwrap();
    fs::create_dir_all(&drafts).unwrap();

    let source = write_inbox_email(&inbox, "email.md", "alice@example.com", "Re: Hello", "Body");
    let draft_path = create_reply_draft(&source, false, "me@example.com", Some(drafts.as_path())).unwrap();

    let content = fs::read_to_string(&draft_path).unwrap();
    // Should not double the Re: prefix
    assert!(content.contains("subject: \"Re: Hello\""));
    assert!(!content.contains("Re: Re:"));
}

#[test]
fn test_create_reply_all_draft() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    let drafts = tmp.path().join("drafts");
    fs::create_dir_all(&inbox).unwrap();
    fs::create_dir_all(&drafts).unwrap();

    // Source email with CC
    let path = inbox.join("email.md");
    let content = "---\nfrom: \"alice@example.com\"\nto: \"me@example.com, bob@example.com\"\ncc: \"carol@example.com\"\nsubject: \"Meeting\"\ndate: \"Mon, 01 Jan 2024 12:00:00 +0000\"\nmessage_id: \"<meeting@example.com>\"\nstatus: inbox\nhas_attachments: false\n---\n\nMeeting notes";
    fs::write(&path, content).unwrap();

    let draft_path = create_reply_draft(&path, true, "me@example.com", Some(drafts.as_path())).unwrap();
    let draft_content = fs::read_to_string(&draft_path).unwrap();

    // Reply-all should have CC with bob and carol but not self
    assert!(draft_content.contains("to: \"alice@example.com\""));
    assert!(draft_content.contains("cc:"));
    assert!(draft_content.contains("bob@example.com"));
    assert!(draft_content.contains("carol@example.com"));
    assert!(!draft_content.contains("cc: \"me@example.com"));
}

// -----------------------------------------------------------------------
// Forward drafts
// -----------------------------------------------------------------------

#[test]
fn test_create_forward_draft() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    let drafts = tmp.path().join("drafts");
    fs::create_dir_all(&inbox).unwrap();
    fs::create_dir_all(&drafts).unwrap();

    let source = write_inbox_email(&inbox, "email.md", "alice@example.com", "Hello", "Forward me");

    let draft_path = create_forward_draft(&source, "me@example.com", Some(drafts.as_path())).unwrap();

    let content = fs::read_to_string(&draft_path).unwrap();

    assert!(content.contains("subject: \"Fwd: Hello\""));
    assert!(content.contains("to: \"\""));
    assert!(content.contains("{{SIGNATURE}}"));
    assert!(content.contains("Forwarded message"));
    assert!(content.contains("From: alice@example.com"));
    assert!(content.contains("Forward me"));
}

#[test]
fn test_forward_with_attachments() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    let drafts = tmp.path().join("drafts");
    fs::create_dir_all(&inbox).unwrap();
    fs::create_dir_all(&drafts).unwrap();

    // Create source email with attachments
    let source = inbox.join("email.md");
    let att_dir = inbox.join("email_attachments");
    fs::create_dir_all(&att_dir).unwrap();
    fs::write(att_dir.join("report.pdf"), b"fake pdf").unwrap();

    let content = "---\nfrom: \"alice@example.com\"\nto: \"me@example.com\"\nsubject: \"With attachment\"\ndate: \"Mon, 01 Jan 2024 12:00:00 +0000\"\nmessage_id: \"<att@example.com>\"\nstatus: inbox\nhas_attachments: true\nattachments:\n  - \"report.pdf\"\n---\n\nSee attached";
    fs::write(&source, content).unwrap();

    let draft_path = create_forward_draft(&source, "me@example.com", Some(drafts.as_path())).unwrap();
    let draft_content = fs::read_to_string(&draft_path).unwrap();

    // Forward should reference the attachment
    assert!(draft_content.contains("attachments:"));
    assert!(draft_content.contains("report.pdf"));
}

// -----------------------------------------------------------------------
// Reply with companion HTML
// -----------------------------------------------------------------------

#[test]
fn test_reply_with_companion_html() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    let drafts = tmp.path().join("drafts");
    fs::create_dir_all(&inbox).unwrap();
    fs::create_dir_all(&drafts).unwrap();

    let source = write_inbox_email(&inbox, "email.md", "alice@example.com", "HTML email", "Plain text body");

    // Write companion HTML
    let html_path = inbox.join("email.html");
    fs::write(&html_path, "<p>Rich HTML body</p>").unwrap();

    let draft_path = create_reply_draft(&source, false, "me@example.com", Some(drafts.as_path())).unwrap();

    // Draft should have a companion HTML with quoted content
    let draft_html = draft_path.with_extension("html");
    assert!(draft_html.exists());
    let html = fs::read_to_string(&draft_html).unwrap();
    assert!(html.contains("Rich HTML body"));
    assert!(html.contains("alice@example.com wrote:"));
}

// -----------------------------------------------------------------------
// Parse and validate drafts
// -----------------------------------------------------------------------

#[test]
fn test_parse_and_validate_draft() {
    let tmp = tempdir().unwrap();
    let path = write_draft(tmp.path(), "draft.md", "alice@example.com", "Test", "Body content", "draft");

    let draft = parse_email_draft(&path).unwrap();
    assert_eq!(draft.frontmatter.to, "alice@example.com");
    assert_eq!(draft.frontmatter.subject, "Test");
    assert_eq!(draft.frontmatter.status, EmailStatus::Draft);
    assert_eq!(draft.body_markdown, "Body content");

    let warnings = validate_draft(&draft).unwrap();
    assert!(warnings.is_empty());
}

// -----------------------------------------------------------------------
// Status transitions
// -----------------------------------------------------------------------

#[test]
fn test_mark_as_approved() {
    let tmp = tempdir().unwrap();
    let path = write_draft(tmp.path(), "draft.md", "alice@example.com", "Test", "Body", "draft");

    let msg = mark_as_approved(&path).unwrap();
    assert!(msg.contains("approved"));

    let draft = parse_email_draft(&path).unwrap();
    assert_eq!(draft.frontmatter.status, EmailStatus::Approved);
}

#[test]
fn test_mark_as_approved_already_approved() {
    let tmp = tempdir().unwrap();
    let path = write_draft(tmp.path(), "draft.md", "alice@example.com", "Test", "Body", "approved");

    let msg = mark_as_approved(&path).unwrap();
    assert!(msg.contains("Already approved"));
}

#[test]
fn test_mark_as_approved_sent_fails() {
    let tmp = tempdir().unwrap();
    let path = write_draft(tmp.path(), "draft.md", "alice@example.com", "Test", "Body", "sent");

    let result = mark_as_approved(&path);
    assert!(result.is_err());
}

#[test]
fn test_update_status_to_sent() {
    let tmp = tempdir().unwrap();
    let drafts = tmp.path().join("drafts");
    let sent = tmp.path().join("sent");
    fs::create_dir_all(&drafts).unwrap();

    let path = write_draft(&drafts, "draft.md", "alice@example.com", "Test", "Body", "approved");
    let draft = parse_email_draft(&path).unwrap();

    update_status_to_sent(&draft, Some(sent.as_path()), Some("<msg123@example.com>")).unwrap();

    // Original should be removed
    assert!(!path.exists());

    // Sent file should exist
    let sent_file = sent.join("draft.md");
    assert!(sent_file.exists());

    let sent_draft = parse_email_draft(&sent_file).unwrap();
    assert_eq!(sent_draft.frontmatter.status, EmailStatus::Sent);
    assert!(sent_draft.frontmatter.sent_at.is_some());
    assert_eq!(sent_draft.frontmatter.message_id, Some("<msg123@example.com>".to_string()));
}

// -----------------------------------------------------------------------
// find_drafts with status filter
// -----------------------------------------------------------------------

#[test]
fn test_find_drafts_with_status_filter() {
    let tmp = tempdir().unwrap();
    write_draft(tmp.path(), "a.md", "alice@example.com", "Draft A", "Body", "draft");
    write_draft(tmp.path(), "b.md", "bob@example.com", "Draft B", "Body", "approved");
    write_draft(tmp.path(), "c.md", "carol@example.com", "Draft C", "Body", "draft");

    let all = find_drafts(tmp.path(), None).unwrap();
    assert_eq!(all.len(), 3);

    let drafts_only = find_drafts(tmp.path(), Some(EmailStatus::Draft)).unwrap();
    assert_eq!(drafts_only.len(), 2);

    let approved_only = find_drafts(tmp.path(), Some(EmailStatus::Approved)).unwrap();
    assert_eq!(approved_only.len(), 1);
    assert_eq!(approved_only[0].frontmatter.to, "bob@example.com");
}
