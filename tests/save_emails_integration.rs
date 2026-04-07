use email::parse::{
    save_fetched_emails, scan_existing_message_ids, AttachmentData,
    FetchedEmail,
};
use email::types::InboxFrontmatter;
use gray_matter::{engine::YAML, Matter};
use std::fs;
use tempfile::tempdir;

/// Helper to parse frontmatter from a saved .md file.
fn parse_frontmatter(content: &str) -> InboxFrontmatter {
    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(content);
    parsed.data.unwrap().deserialize().unwrap()
}

fn make_email(
    from: &str,
    subject: &str,
    date: &str,
    body: &str,
    message_id: Option<&str>,
) -> FetchedEmail {
    FetchedEmail {
        from: from.to_string(),
        to: "me@example.com".to_string(),
        cc: None,
        subject: subject.to_string(),
        date: date.to_string(),
        body_text: body.to_string(),
        html_body: None,
        has_attachments: false,
        message_id: message_id.map(|s| s.to_string()),
        attachments: Vec::new(),
        is_read: false,
    }
}

// -----------------------------------------------------------------------
// save_fetched_emails
// -----------------------------------------------------------------------

#[test]
fn test_save_fetched_emails_basic() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");

    let emails = vec![make_email(
        "alice@example.com",
        "Hello",
        "Mon, 01 Jan 2024 12:00:00 +0000",
        "Email body",
        Some("<msg1@example.com>"),
    )];

    let (saved, skipped) = save_fetched_emails(&emails, &inbox, "inbox").unwrap();
    assert_eq!(saved, 1);
    assert_eq!(skipped, 0);

    // Verify file was created
    let files: Vec<_> = fs::read_dir(&inbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    assert_eq!(files.len(), 1);

    let content = fs::read_to_string(files[0].path()).unwrap();
    let fm = parse_frontmatter(&content);
    assert_eq!(fm.from, "alice@example.com");
    assert_eq!(fm.subject, "Hello");
    assert_eq!(fm.message_id, Some("<msg1@example.com>".to_string()));
    assert!(content.contains("status: inbox"));
    assert!(content.contains("Email body"));
}

// -----------------------------------------------------------------------
// Dedup by message_id
// -----------------------------------------------------------------------

#[test]
fn test_save_fetched_emails_dedup() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");

    let email = make_email(
        "alice@example.com",
        "Hello",
        "Mon, 01 Jan 2024 12:00:00 +0000",
        "Body",
        Some("<dup@example.com>"),
    );

    // Save once
    let (saved1, _) = save_fetched_emails(&[email.clone()], &inbox, "inbox").unwrap();
    assert_eq!(saved1, 1);

    // Save again -- should be skipped
    let (saved2, skipped2) = save_fetched_emails(&[email], &inbox, "inbox").unwrap();
    assert_eq!(saved2, 0);
    assert_eq!(skipped2, 1);
}

// -----------------------------------------------------------------------
// Filename collision
// -----------------------------------------------------------------------

#[test]
fn test_save_fetched_emails_filename_collision() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");

    // Two emails with different message_ids but same subject/date/sender
    let email1 = make_email(
        "alice@example.com",
        "Hello",
        "Mon, 01 Jan 2024 12:00:00 +0000",
        "Body 1",
        Some("<col1@example.com>"),
    );
    let email2 = make_email(
        "alice@example.com",
        "Hello",
        "Mon, 01 Jan 2024 12:00:00 +0000",
        "Body 2",
        Some("<col2@example.com>"),
    );

    let (saved, _) = save_fetched_emails(&[email1, email2], &inbox, "inbox").unwrap();
    assert_eq!(saved, 2);

    // Should have two distinct files
    let files: Vec<_> = fs::read_dir(&inbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    assert_eq!(files.len(), 2);
}

// -----------------------------------------------------------------------
// Attachments
// -----------------------------------------------------------------------

#[test]
fn test_save_fetched_emails_with_attachments() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");

    let email = FetchedEmail {
        from: "alice@example.com".to_string(),
        to: "me@example.com".to_string(),
        cc: None,
        subject: "With files".to_string(),
        date: "Mon, 01 Jan 2024 12:00:00 +0000".to_string(),
        body_text: "See attached".to_string(),
        html_body: None,
        has_attachments: true,
        message_id: Some("<att@example.com>".to_string()),
        attachments: vec![
            AttachmentData {
                filename: "report.pdf".to_string(),
                content: b"fake pdf content".to_vec(),
            },
            AttachmentData {
                filename: "image.png".to_string(),
                content: b"fake png content".to_vec(),
            },
        ],
        is_read: false,
    };

    let (saved, _) = save_fetched_emails(&[email], &inbox, "inbox").unwrap();
    assert_eq!(saved, 1);

    // Find the saved .md file
    let md_files: Vec<_> = fs::read_dir(&inbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    assert_eq!(md_files.len(), 1);

    let md_path = md_files[0].path();
    let content = fs::read_to_string(&md_path).unwrap();
    assert!(content.contains("has_attachments: true"));
    assert!(content.contains("attachments:"));
    assert!(content.contains("report.pdf"));
    assert!(content.contains("image.png"));

    // Check attachments directory
    let att_dir = email::parse::attachments_dir_for(&md_path);
    assert!(att_dir.exists());
    assert!(att_dir.join("report.pdf").exists());
    assert!(att_dir.join("image.png").exists());
    assert_eq!(
        fs::read(att_dir.join("report.pdf")).unwrap(),
        b"fake pdf content"
    );
}

// -----------------------------------------------------------------------
// Attachment dedup (same filename)
// -----------------------------------------------------------------------

#[test]
fn test_save_fetched_emails_attachment_dedup() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");

    let email = FetchedEmail {
        from: "alice@example.com".to_string(),
        to: "me@example.com".to_string(),
        cc: None,
        subject: "Dup files".to_string(),
        date: "Mon, 01 Jan 2024 12:00:00 +0000".to_string(),
        body_text: "Body".to_string(),
        html_body: None,
        has_attachments: true,
        message_id: Some("<dupatt@example.com>".to_string()),
        attachments: vec![
            AttachmentData {
                filename: "file.pdf".to_string(),
                content: b"content 1".to_vec(),
            },
            AttachmentData {
                filename: "file.pdf".to_string(),
                content: b"content 2".to_vec(),
            },
        ],
        is_read: false,
    };

    let (saved, _) = save_fetched_emails(&[email], &inbox, "inbox").unwrap();
    assert_eq!(saved, 1);

    let md_files: Vec<_> = fs::read_dir(&inbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    let att_dir = email::parse::attachments_dir_for(&md_files[0].path());
    assert!(att_dir.join("file.pdf").exists());
    assert!(att_dir.join("file-1.pdf").exists());
}

// -----------------------------------------------------------------------
// scan_existing_message_ids
// -----------------------------------------------------------------------

#[test]
fn test_scan_existing_message_ids() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();

    // Write files with message_id
    let content1 = "---\nfrom: \"a@x.com\"\nmessage_id: \"<id1@x.com>\"\n---\n\nBody";
    let content2 = "---\nfrom: \"b@x.com\"\nmessage_id: \"<id2@x.com>\"\n---\n\nBody";
    let content_no_id = "---\nfrom: \"c@x.com\"\n---\n\nBody";

    fs::write(dir.join("a.md"), content1).unwrap();
    fs::write(dir.join("b.md"), content2).unwrap();
    fs::write(dir.join("c.md"), content_no_id).unwrap();

    let ids = scan_existing_message_ids(dir).unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains("<id1@x.com>"));
    assert!(ids.contains("<id2@x.com>"));
}

// -----------------------------------------------------------------------
// HTML companion with charset injection
// -----------------------------------------------------------------------

#[test]
fn test_save_with_html_body_has_charset() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("inbox");

    let email = FetchedEmail {
        from: "alice@example.com".to_string(),
        to: "me@example.com".to_string(),
        cc: None,
        subject: "HTML test".to_string(),
        date: "Mon, 01 Jan 2024 12:00:00 +0000".to_string(),
        body_text: "Plain text".to_string(),
        html_body: Some("<html><body><p>Rich content</p></body></html>".to_string()),
        has_attachments: false,
        message_id: Some("<html@example.com>".to_string()),
        attachments: Vec::new(),
        is_read: false,
    };

    let (saved, _) = save_fetched_emails(&[email], &inbox, "inbox").unwrap();
    assert_eq!(saved, 1);

    // Find the .html companion
    let html_files: Vec<_> = fs::read_dir(&inbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "html"))
        .collect();
    assert_eq!(html_files.len(), 1);

    let html_content = fs::read_to_string(html_files[0].path()).unwrap();
    assert!(html_content.contains("charset=\"UTF-8\""));
    assert!(html_content.contains("Rich content"));
}

// ensure_utf8_charset is tested inline in parse.rs unit tests (pub(crate))
