use anyhow::Result;
use chrono::Utc;
use colored::*;
use mailparse::{parse_mail, MailHeaderMap};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Ensure an HTML string contains a UTF-8 charset declaration.
/// If a `<meta charset=...>` tag is already present, the HTML is returned as-is.
/// Otherwise, the declaration is injected after `<head>` (or prepended if there
/// is no `<head>` tag) so that browsers interpret the content correctly.
pub(crate) fn ensure_utf8_charset(html: &str) -> String {
    let lower = html.to_lowercase();
    if lower.contains("charset") {
        return html.to_string();
    }
    let meta = r#"<meta charset="UTF-8">"#;
    if let Some(pos) = lower.find("<head>") {
        let insert = pos + "<head>".len();
        format!("{}{}{}", &html[..insert], meta, &html[insert..])
    } else if let Some(pos) = lower.find("<html") {
        // Insert a minimal <head> block right after the opening <html ...> tag.
        let tag_end = html[pos..].find('>').map(|i| pos + i + 1).unwrap_or(pos + 6);
        format!("{}<head>{}</head>{}", &html[..tag_end], meta, &html[tag_end..])
    } else {
        format!("{}{}", meta, html)
    }
}

/// Find the largest byte index <= `max_bytes` that lies on a UTF-8 char boundary.
pub(crate) fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[derive(Debug, Clone)]
pub struct FetchedEmail {
    pub from: String,
    pub to: String,
    pub cc: Option<String>,
    pub subject: String,
    pub date: String,
    pub body_text: String,
    pub html_body: Option<String>,
    pub has_attachments: bool,
    pub message_id: Option<String>,
    pub attachments: Vec<AttachmentData>,
}

#[derive(Debug, Clone)]
pub struct AttachmentData {
    pub filename: String,
    pub content: Vec<u8>,
}

pub fn html_to_plain(html: &str) -> String {
    html2text::config::plain()
        .use_doc_css()
        .no_table_borders()
        .no_link_wrapping()
        .string_from_read(html.as_bytes(), 10_000)
        .unwrap_or_else(|_| html.to_string())
}

/// Recursively collect the first text/plain and text/html parts from a parsed email.
pub fn extract_body_parts(parsed: &mailparse::ParsedMail) -> (Option<String>, Option<String>) {
    if parsed.ctype.mimetype == "text/plain" {
        let body = parsed.get_body().unwrap_or_default();
        if !body.is_empty() {
            return (Some(body), None);
        }
    }

    if parsed.ctype.mimetype == "text/html" {
        let body = parsed.get_body().unwrap_or_default();
        if !body.is_empty() {
            return (None, Some(body));
        }
    }

    let mut plain = None;
    let mut html = None;

    for sub in &parsed.subparts {
        let (sub_plain, sub_html) = extract_body_parts(sub);
        if plain.is_none() {
            plain = sub_plain;
        }
        if html.is_none() {
            html = sub_html;
        }
        if plain.is_some() && html.is_some() {
            break;
        }
    }

    (plain, html)
}

/// Extract body text from a parsed email.
/// Returns (plain_text, Option<html_body>).
pub fn extract_body_text(parsed: &mailparse::ParsedMail) -> (String, Option<String>) {
    let (plain, html) = extract_body_parts(parsed);

    if let Some(plain_text) = plain {
        (plain_text, html)
    } else if let Some(ref html_text) = html {
        (html_to_plain(html_text), html)
    } else {
        (String::new(), None)
    }
}

pub fn has_attachments(parsed: &mailparse::ParsedMail) -> bool {
    for sub in &parsed.subparts {
        let disposition = sub.get_content_disposition();
        if disposition.disposition == mailparse::DispositionType::Attachment {
            return true;
        }
        if has_attachments(sub) {
            return true;
        }
    }
    false
}

/// Extract all attachments from a parsed email, recursing through MIME subparts.
pub fn extract_attachments(parsed: &mailparse::ParsedMail) -> Vec<AttachmentData> {
    let mut attachments = Vec::new();
    let mut counter = 0usize;
    collect_attachments(parsed, &mut attachments, &mut counter);
    attachments
}

fn collect_attachments(
    parsed: &mailparse::ParsedMail,
    attachments: &mut Vec<AttachmentData>,
    counter: &mut usize,
) {
    let disposition = parsed.get_content_disposition();
    if disposition.disposition == mailparse::DispositionType::Attachment {
        let filename = disposition
            .params
            .get("filename")
            .or_else(|| parsed.ctype.params.get("name"))
            .cloned()
            .unwrap_or_else(|| {
                *counter += 1;
                format!("attachment-{}.bin", counter)
            });
        let filename = sanitize_attachment_filename(&filename);
        if let Ok(content) = parsed.get_body_raw() {
            attachments.push(AttachmentData { filename, content });
        }
    }
    for sub in &parsed.subparts {
        collect_attachments(sub, attachments, counter);
    }
}

pub(crate) fn sanitize_attachment_filename(name: &str) -> String {
    let name = name.replace(['/', '\\', '\0'], "_");
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim().to_string();
    if name.is_empty() {
        "attachment.bin".to_string()
    } else if name.len() > 200 {
        name[..floor_char_boundary(&name, 200)].to_string()
    } else {
        name
    }
}

/// Return the attachments directory path for a given .md email file.
/// Convention: `{parent}/{stem}_attachments/`
pub fn attachments_dir_for(md_path: &Path) -> PathBuf {
    let parent = md_path.parent().unwrap_or(Path::new("."));
    let stem = md_path.file_stem().unwrap_or_default().to_string_lossy();
    parent.join(format!("{}_attachments", stem))
}

/// List all attachment files for a given email .md file.
/// Returns an empty Vec if the attachments directory doesn't exist or is empty.
pub fn list_attachments(email_path: &Path) -> Result<Vec<PathBuf>> {
    let dir = attachments_dir_for(email_path);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    Ok(files)
}

/// Open a file with the system default application (macOS `open`).
pub fn open_file_with_system(path: &Path) -> Result<()> {
    let status = std::process::Command::new("open")
        .arg(path)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run 'open': {e}"))?;
    if !status.success() {
        anyhow::bail!("'open' exited with status {}", status);
    }
    Ok(())
}

/// Parse raw RFC822 bytes into a FetchedEmail struct.
pub fn parse_rfc822_to_fetched_email(rfc822_body: &[u8]) -> Option<FetchedEmail> {
    let parsed = parse_mail(rfc822_body).ok()?;
    let headers = &parsed.headers;
    let from = headers
        .get_first_value("From")
        .unwrap_or_else(|| "(unknown)".to_string());
    let to = headers
        .get_first_value("To")
        .unwrap_or_else(|| "(unknown)".to_string());
    let cc = headers.get_first_value("Cc");
    let subject = headers
        .get_first_value("Subject")
        .unwrap_or_else(|| "(no subject)".to_string());
    let date = headers
        .get_first_value("Date")
        .unwrap_or_else(|| "(unknown date)".to_string());
    let message_id = headers
        .get_first_value("Message-ID")
        .or_else(|| headers.get_first_value("Message-Id"));
    let (body_text, html_body) = extract_body_text(&parsed);
    let has_att = has_attachments(&parsed);
    let att_data = extract_attachments(&parsed);

    Some(FetchedEmail {
        from,
        to,
        cc,
        subject,
        date,
        body_text,
        html_body,
        has_attachments: has_att,
        message_id,
        attachments: att_data,
    })
}

/// Compress a sorted list of UIDs into IMAP sequence set format using ranges.
/// e.g., [1,2,3,5,7,8,9] -> "1:3,5,7:9"
pub fn compress_uid_set(uids: &[u32]) -> String {
    if uids.is_empty() {
        return String::new();
    }
    let mut sorted = uids.to_vec();
    sorted.sort();

    let mut ranges = Vec::new();
    let mut start = sorted[0];
    let mut end = sorted[0];

    for &uid in &sorted[1..] {
        if uid == end + 1 {
            end = uid;
        } else {
            if start == end {
                ranges.push(start.to_string());
            } else {
                ranges.push(format!("{}:{}", start, end));
            }
            start = uid;
            end = uid;
        }
    }
    if start == end {
        ranges.push(start.to_string());
    } else {
        ranges.push(format!("{}:{}", start, end));
    }
    ranges.join(",")
}

pub fn slugify_subject(subject: &str) -> String {
    let slug: String = subject
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else if c == ' ' { '-' } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("");
    let result = crate::types::collapse_hyphens(&slug);
    if result.len() > 40 {
        // Truncate at nearest char boundary <= 40 bytes
        let end = floor_char_boundary(&result, 40);
        let truncated = &result[..end];
        truncated.trim_end_matches('-').to_string()
    } else {
        result
    }
}

pub fn slugify_sender(from: &str) -> String {
    // Extract display name if present (e.g. "John Doe <john@example.com>" -> "John Doe")
    // Otherwise use the local part of the email address
    let name = if let Some(start) = from.find('<') {
        let display = from[..start].trim().trim_matches('"');
        if display.is_empty() {
            // No display name, use local part of email
            let email = &from[start + 1..from.find('>').unwrap_or(from.len())];
            email.split('@').next().unwrap_or("unknown").to_string()
        } else {
            display.to_string()
        }
    } else if from.contains('@') {
        from.split('@').next().unwrap_or("unknown").to_string()
    } else {
        from.to_string()
    };

    // Slugify the name
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    crate::types::collapse_hyphens(&slug)
}

pub fn extract_email_address(raw: &str) -> String {
    if let Some(start) = raw.find('<') {
        if let Some(end) = raw.find('>') {
            return raw[start + 1..end].trim().to_string();
        }
    }
    raw.trim().to_string()
}

pub fn parse_email_date_prefix(date_str: &str) -> String {
    // Try parsing common email date formats to extract YYYY-MM-DD-HHMM
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(date_str) {
        return dt.format("%Y-%m-%d-%H%M").to_string();
    }
    // Fallback: use current datetime
    Utc::now().format("%Y-%m-%d-%H%M").to_string()
}

/// Low-level scanner: walks a mailbox directory and extracts message_id from frontmatter.
/// Returns {message_id -> file_path}. Used as the canonical base for all scanning.
pub(crate) fn scan_mailbox_message_ids(dir: &Path) -> Result<std::collections::HashMap<String, PathBuf>> {
    let mut ids = std::collections::HashMap::new();
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

/// Scan a directory for .md files and collect their message_ids into a HashSet.
/// Delegates to scan_mailbox_message_ids to deduplicate the low-level scanning logic.
pub fn scan_existing_message_ids(dir: &Path) -> Result<HashSet<String>> {
    Ok(scan_mailbox_message_ids(dir)?.into_keys().collect())
}

pub fn save_fetched_emails(emails: &[FetchedEmail], inbox_dir: &Path, status: &str) -> Result<(usize, usize)> {
    let mut existing_ids = scan_existing_message_ids(inbox_dir)?;
    save_fetched_emails_with_known_ids(emails, inbox_dir, status, &mut existing_ids)
}

pub fn save_fetched_emails_with_known_ids(
    emails: &[FetchedEmail],
    inbox_dir: &Path,
    status: &str,
    existing_ids: &mut HashSet<String>,
) -> Result<(usize, usize)> {
    fs::create_dir_all(inbox_dir)?;

    let mut saved = 0;
    let mut skipped = 0;

    for email in emails {
        // Skip duplicates by message_id
        if let Some(ref mid) = email.message_id {
            if existing_ids.contains(mid) {
                skipped += 1;
                continue;
            }
        }

        let date_prefix = parse_email_date_prefix(&email.date);
        let sender_slug = slugify_sender(&email.from);
        let subject_slug = slugify_subject(&email.subject);
        let filename = if subject_slug.is_empty() {
            format!("{}_{}_email.md", date_prefix, sender_slug)
        } else {
            format!("{}_{}_{}.md", date_prefix, sender_slug, subject_slug)
        };

        let mut dest = inbox_dir.join(&filename);
        // Avoid overwriting if filename collides
        if dest.exists() {
            let mut counter = 1;
            loop {
                let name = if subject_slug.is_empty() {
                    format!("{}_{}_email-{}.md", date_prefix, sender_slug, counter)
                } else {
                    format!("{}_{}_{}-{}.md", date_prefix, sender_slug, subject_slug, counter)
                };
                dest = inbox_dir.join(&name);
                if !dest.exists() {
                    break;
                }
                counter += 1;
            }
        }

        // Build frontmatter via serde for correct quoting
        let fetched_at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let fm = crate::types::SaveFrontmatter {
            from: email.from.clone(),
            to: email.to.clone(),
            cc: email.cc.clone(),
            subject: email.subject.clone(),
            date: email.date.clone(),
            message_id: email.message_id.clone(),
            status: status.to_string(),
            has_attachments: email.has_attachments,
            attachments: None, // filled below after saving attachment files
            fetched_at: fetched_at.clone(),
        };

        // Save attachment files and record filenames
        let mut saved_filenames: Vec<String> = Vec::new();
        if !email.attachments.is_empty() {
            let att_dir = attachments_dir_for(&dest);
            fs::create_dir_all(&att_dir)?;

            let mut used_names: HashSet<String> = HashSet::new();
            for att in &email.attachments {
                let mut name = att.filename.clone();
                if used_names.contains(&name) {
                    let stem = Path::new(&name)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let ext = Path::new(&name)
                        .extension()
                        .map(|e| format!(".{}", e.to_string_lossy()))
                        .unwrap_or_default();
                    let mut n = 1;
                    loop {
                        name = format!("{}-{}{}", stem, n, ext);
                        if !used_names.contains(&name) {
                            break;
                        }
                        n += 1;
                    }
                }
                used_names.insert(name.clone());
                fs::write(att_dir.join(&name), &att.content)?;
                saved_filenames.push(name);
            }
        }

        // Build frontmatter with attachment info
        let fm = crate::types::SaveFrontmatter {
            attachments: if saved_filenames.is_empty() { None } else { Some(saved_filenames) },
            ..fm
        };
        let mut frontmatter = serde_yaml::to_string(&fm)?;
        // serde_yaml 0.9 does not add document markers.
        // Wrap in "---\n...\n---\n\n" for valid frontmatter.
        frontmatter = format!("---\n{}---\n\n", frontmatter);

        let content = format!("{}{}", frontmatter, email.body_text);
        fs::write(&dest, content)?;

        // Save companion HTML file if available
        if let Some(ref html) = email.html_body {
            let html_path = dest.with_extension("html");
            fs::write(&html_path, ensure_utf8_charset(html))?;
        }

        // Track the new message_id to prevent duplicates within the same batch
        if let Some(ref mid) = email.message_id {
            existing_ids.insert(mid.clone());
        }

        saved += 1;
    }

    Ok((saved, skipped))
}

pub fn display_fetched_emails(emails: &[FetchedEmail], full_body: bool) {
    if emails.is_empty() {
        println!("No emails found matching the criteria.");
        return;
    }

    println!(
        "\n{} ({} result{})\n",
        "Fetched Emails".bold().cyan(),
        emails.len(),
        if emails.len() == 1 { "" } else { "s" }
    );

    for (i, email) in emails.iter().enumerate() {
        println!("{}", "─".repeat(60));
        println!("{}: {}", "From".bold().green(), email.from);
        println!("{}: {}", "To".bold().blue(), email.to);
        if let Some(ref cc) = email.cc {
            println!("{}: {}", "Cc".bold().blue(), cc);
        }
        println!("{}: {}", "Subject".bold().yellow(), email.subject);
        let date_display = if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(&email.date) {
            dt.format("%Y-%m-%d").to_string()
        } else {
            email.date.clone()
        };
        println!("{}: {}", "Date".bold().magenta(), date_display);
        if email.has_attachments {
            println!("{}", "[has attachments]".yellow());
        }

        println!();
        if full_body {
            println!("{}", email.body_text);
        } else {
            let preview: String = email.body_text.chars().take(300).collect();
            println!("{}", preview);
            if email.body_text.len() > 300 {
                println!("{}", "...".dimmed());
            }
        }

        if i < emails.len() - 1 {
            println!();
        }
    }
    println!("{}", "─".repeat(60));
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // ensure_utf8_charset
    // -----------------------------------------------------------------------

    #[test]
    fn test_ensure_utf8_charset_already_has_charset() {
        let html = r#"<html><head><meta charset="UTF-8"></head><body>hi</body></html>"#;
        assert_eq!(ensure_utf8_charset(html), html);
    }

    #[test]
    fn test_ensure_utf8_charset_injects_after_head() {
        let html = "<html><head><title>Test</title></head><body>hi</body></html>";
        let result = ensure_utf8_charset(html);
        assert!(result.contains("<meta charset=\"UTF-8\">"));
        // Must appear right after <head>
        assert!(result.contains("<head><meta charset=\"UTF-8\"><title>"));
    }

    #[test]
    fn test_ensure_utf8_charset_html_without_head() {
        let html = "<html><body>hi</body></html>";
        let result = ensure_utf8_charset(html);
        assert!(result.contains(r#"<meta charset="UTF-8">"#));
        assert!(result.contains("<head>"));
    }

    #[test]
    fn test_ensure_utf8_charset_bare_html() {
        let html = "<div>hello</div>";
        let result = ensure_utf8_charset(html);
        assert!(result.starts_with(r#"<meta charset="UTF-8">"#));
    }

    // -----------------------------------------------------------------------
    // compress_uid_set
    // -----------------------------------------------------------------------

    #[test]
    fn test_compress_uid_set_ranges() {
        assert_eq!(compress_uid_set(&[1, 2, 3, 5, 7, 8, 9]), "1:3,5,7:9");
    }

    #[test]
    fn test_compress_uid_set_empty() {
        assert_eq!(compress_uid_set(&[]), "");
    }

    #[test]
    fn test_compress_uid_set_single() {
        assert_eq!(compress_uid_set(&[42]), "42");
    }

    #[test]
    fn test_compress_uid_set_unsorted() {
        assert_eq!(compress_uid_set(&[5, 3, 1, 2, 4]), "1:5");
    }

    #[test]
    fn test_compress_uid_set_non_contiguous() {
        assert_eq!(compress_uid_set(&[1, 3, 5]), "1,3,5");
    }

    // -----------------------------------------------------------------------
    // slugify_subject
    // -----------------------------------------------------------------------

    #[test]
    fn test_slugify_subject_normal() {
        assert_eq!(slugify_subject("Hello World"), "hello-world");
    }

    #[test]
    fn test_slugify_subject_unicode() {
        let result = slugify_subject("Prufung Ergebnis");
        assert!(result.contains("prufung"));
    }

    #[test]
    fn test_slugify_subject_empty() {
        assert_eq!(slugify_subject(""), "");
    }

    #[test]
    fn test_slugify_subject_long() {
        let long = "a ".repeat(30);
        let result = slugify_subject(&long);
        assert!(result.len() <= 40);
    }

    #[test]
    fn test_slugify_subject_special_chars() {
        assert_eq!(slugify_subject("Re: Hello! @#$ World?"), "re-hello-world");
    }

    #[test]
    fn test_slugify_subject_consecutive_hyphens() {
        assert_eq!(slugify_subject("hello   world"), "hello-world");
    }

    // -----------------------------------------------------------------------
    // slugify_sender
    // -----------------------------------------------------------------------

    #[test]
    fn test_slugify_sender_display_name() {
        assert_eq!(slugify_sender("John Doe <john@example.com>"), "john-doe");
    }

    #[test]
    fn test_slugify_sender_bare_email() {
        assert_eq!(slugify_sender("john@example.com"), "john");
    }

    #[test]
    fn test_slugify_sender_no_display_name() {
        assert_eq!(slugify_sender("<john@example.com>"), "john");
    }

    #[test]
    fn test_slugify_sender_quoted_display_name() {
        assert_eq!(slugify_sender("\"John Doe\" <john@example.com>"), "john-doe");
    }

    // -----------------------------------------------------------------------
    // extract_email_address
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_email_address_angle_brackets() {
        assert_eq!(extract_email_address("John Doe <john@x.com>"), "john@x.com");
    }

    #[test]
    fn test_extract_email_address_bare() {
        assert_eq!(extract_email_address("john@x.com"), "john@x.com");
    }

    #[test]
    fn test_extract_email_address_whitespace() {
        assert_eq!(extract_email_address("  john@x.com  "), "john@x.com");
    }

    // -----------------------------------------------------------------------
    // parse_email_date_prefix
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_email_date_prefix_valid_rfc2822() {
        let result = parse_email_date_prefix("Mon, 01 Jan 2024 12:00:00 +0000");
        assert_eq!(result, "2024-01-01-1200");
    }

    #[test]
    fn test_parse_email_date_prefix_invalid_fallback() {
        let result = parse_email_date_prefix("not a date");
        // Should fall back to current date - just verify it has the right format
        assert!(result.len() >= 15); // YYYY-MM-DD-HHMM
        assert_eq!(&result[4..5], "-");
    }

    // -----------------------------------------------------------------------
    // sanitize_attachment_filename
    // -----------------------------------------------------------------------

    #[test]
    fn test_sanitize_attachment_filename_normal() {
        assert_eq!(sanitize_attachment_filename("report.pdf"), "report.pdf");
    }

    #[test]
    fn test_sanitize_attachment_filename_slashes() {
        assert_eq!(sanitize_attachment_filename("path/to/file.pdf"), "path_to_file.pdf");
    }

    #[test]
    fn test_sanitize_attachment_filename_control_chars() {
        assert_eq!(sanitize_attachment_filename("file\x00name.pdf"), "file_name.pdf");
    }

    #[test]
    fn test_sanitize_attachment_filename_empty() {
        assert_eq!(sanitize_attachment_filename(""), "attachment.bin");
    }

    #[test]
    fn test_sanitize_attachment_filename_long() {
        let long = "a".repeat(250);
        let result = sanitize_attachment_filename(&long);
        assert!(result.len() <= 200);
    }

    // -----------------------------------------------------------------------
    // attachments_dir_for
    // -----------------------------------------------------------------------

    #[test]
    fn test_attachments_dir_for_basic() {
        let path = Path::new("/mail/inbox/email.md");
        assert_eq!(attachments_dir_for(path), PathBuf::from("/mail/inbox/email_attachments"));
    }

    #[test]
    fn test_attachments_dir_for_nested() {
        let path = Path::new("a/b/c/test.md");
        assert_eq!(attachments_dir_for(path), PathBuf::from("a/b/c/test_attachments"));
    }

    // -----------------------------------------------------------------------
    // floor_char_boundary
    // -----------------------------------------------------------------------

    #[test]
    fn test_floor_char_boundary_ascii() {
        assert_eq!(floor_char_boundary("hello", 3), 3);
    }

    #[test]
    fn test_floor_char_boundary_multibyte() {
        // "ae" is U+00E4, 2 bytes in UTF-8
        let s = "\u{00E4}bc";
        // Byte 1 is in the middle of the 2-byte char -> should clamp to 0
        assert_eq!(floor_char_boundary(s, 1), 0);
        // Byte 2 is the start of 'b'
        assert_eq!(floor_char_boundary(s, 2), 2);
    }

    #[test]
    fn test_floor_char_boundary_exact() {
        assert_eq!(floor_char_boundary("abc", 10), 3);
    }

    // -----------------------------------------------------------------------
    // parse_rfc822_to_fetched_email
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_rfc822_minimal() {
        let raw = b"From: alice@example.com\r\nTo: bob@example.com\r\nSubject: Test\r\nDate: Mon, 01 Jan 2024 12:00:00 +0000\r\nMessage-ID: <test123@example.com>\r\n\r\nHello world";
        let email = parse_rfc822_to_fetched_email(raw).expect("should parse");
        assert_eq!(email.from, "alice@example.com");
        assert_eq!(email.to, "bob@example.com");
        assert_eq!(email.subject, "Test");
        assert!(email.body_text.contains("Hello world"));
        assert_eq!(email.message_id, Some("<test123@example.com>".to_string()));
        assert!(!email.has_attachments);
    }

    #[test]
    fn test_parse_rfc822_missing_fields() {
        let raw = b"\r\nBody only";
        let email = parse_rfc822_to_fetched_email(raw).expect("should parse");
        assert_eq!(email.from, "(unknown)");
        assert_eq!(email.subject, "(no subject)");
    }

    // -----------------------------------------------------------------------
    // html_to_plain (existing tests below)
    // -----------------------------------------------------------------------

    #[test]
    fn test_html_to_plain_preserves_paragraph_breaks() {
        let html = "<p>First paragraph</p><p>Second paragraph</p>";
        let result = html_to_plain(html);
        let non_empty: Vec<&str> = result.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(non_empty.len(), 2, "Expected 2 non-empty lines, got: {:?}", non_empty);
        assert!(non_empty[0].contains("First paragraph"));
        assert!(non_empty[1].contains("Second paragraph"));
    }

    #[test]
    fn test_html_to_plain_no_hard_wrap() {
        // A single paragraph with a 200-char word -- must not be hard-wrapped at 80.
        let long_word = "a".repeat(200);
        let html = format!("<p>{}</p>", long_word);
        let result = html_to_plain(&html);
        let non_empty: Vec<&str> = result.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(non_empty.len(), 1, "Expected 1 non-empty line (no hard wrap), got: {:?}", non_empty);
        assert!(non_empty[0].contains(&long_word));
    }

    #[test]
    fn test_html_to_plain_blockquote_markers() {
        let html = "<blockquote>quoted text</blockquote>";
        let result = html_to_plain(html);
        let non_empty: Vec<&str> = result.lines().filter(|l| !l.trim().is_empty()).collect();
        assert!(!non_empty.is_empty(), "Expected at least one non-empty line");
        for line in &non_empty {
            assert!(line.starts_with("> "), "Expected line to start with '> ', got: {:?}", line);
        }
    }

    #[test]
    fn test_html_to_plain_nested_blockquote() {
        let html = "<blockquote><blockquote>deep quote</blockquote></blockquote>";
        let result = html_to_plain(html);
        let non_empty: Vec<&str> = result.lines().filter(|l| !l.trim().is_empty()).collect();
        assert!(!non_empty.is_empty(), "Expected at least one non-empty line");
        for line in &non_empty {
            assert!(
                line.starts_with("> > "),
                "Expected line to start with '> > ', got: {:?}",
                line
            );
        }
    }

    #[test]
    fn test_html_to_plain_no_table_borders() {
        let html = "<table><tr><td>cell</td></tr></table>";
        let result = html_to_plain(html);
        assert!(!result.contains('+'), "Output should not contain '+' table border chars: {:?}", result);
        assert!(!result.contains("---"), "Output should not contain '---' table border chars: {:?}", result);
        assert!(result.contains("cell"), "Output should contain the cell text");
    }

    #[test]
    fn test_html_to_plain_fallback_on_error() {
        // Empty string should not panic
        let result = html_to_plain("");
        // Just verify it returns without panicking
        let _ = result;
    }

    // -----------------------------------------------------------------------
    // compress_uid_set -- additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_compress_uid_set_duplicates() {
        // compress_uid_set does not deduplicate; duplicates break ranges
        assert_eq!(compress_uid_set(&[1, 1, 2, 2, 3]), "1,1:2,2:3");
    }

    #[test]
    fn test_compress_uid_set_two_elements_contiguous() {
        assert_eq!(compress_uid_set(&[10, 11]), "10:11");
    }

    #[test]
    fn test_compress_uid_set_large_gap() {
        assert_eq!(compress_uid_set(&[1, 1000]), "1,1000");
    }

    // -----------------------------------------------------------------------
    // slugify_sender -- additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_slugify_sender_plain_name() {
        assert_eq!(slugify_sender("Alice"), "alice");
    }

    #[test]
    fn test_slugify_sender_empty() {
        assert_eq!(slugify_sender(""), "");
    }

    #[test]
    fn test_slugify_sender_email_only_angle_brackets_no_local() {
        // Edge: angle brackets with empty local part
        assert_eq!(slugify_sender("<@example.com>"), "");
    }

    // -----------------------------------------------------------------------
    // slugify_subject -- additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_slugify_subject_only_special_chars() {
        assert_eq!(slugify_subject("!@#$%^&*()"), "");
    }

    #[test]
    fn test_slugify_subject_leading_trailing_spaces() {
        assert_eq!(slugify_subject("  hello world  "), "hello-world");
    }

    // -----------------------------------------------------------------------
    // extract_email_address -- additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_email_address_malformed_angle_brackets() {
        // Only opening bracket, no closing
        assert_eq!(extract_email_address("John <john@x.com"), "John <john@x.com");
    }

    #[test]
    fn test_extract_email_address_empty() {
        assert_eq!(extract_email_address(""), "");
    }

    // -----------------------------------------------------------------------
    // floor_char_boundary -- additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_floor_char_boundary_empty_string() {
        assert_eq!(floor_char_boundary("", 5), 0);
    }

    #[test]
    fn test_floor_char_boundary_zero() {
        assert_eq!(floor_char_boundary("hello", 0), 0);
    }

    #[test]
    fn test_floor_char_boundary_emoji() {
        // Emoji is 4 bytes in UTF-8
        let s = "\u{1F600}abc"; // grinning face + "abc"
        assert_eq!(floor_char_boundary(s, 1), 0); // mid emoji
        assert_eq!(floor_char_boundary(s, 4), 4); // exactly after emoji
        assert_eq!(floor_char_boundary(s, 5), 5); // after 'a'
    }

    // -----------------------------------------------------------------------
    // parse_rfc822_to_fetched_email -- additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_rfc822_with_cc() {
        let raw = b"From: a@x.com\r\nTo: b@x.com\r\nCc: c@x.com, d@x.com\r\nSubject: Test\r\nDate: Mon, 01 Jan 2024 12:00:00 +0000\r\n\r\nBody";
        let email = parse_rfc822_to_fetched_email(raw).expect("should parse");
        assert_eq!(email.cc, Some("c@x.com, d@x.com".to_string()));
    }

    #[test]
    fn test_parse_rfc822_html_only() {
        let raw = b"From: a@x.com\r\nTo: b@x.com\r\nSubject: HTML\r\nDate: Mon, 01 Jan 2024 12:00:00 +0000\r\nContent-Type: text/html\r\n\r\n<p>Hello</p>";
        let email = parse_rfc822_to_fetched_email(raw).expect("should parse");
        assert!(email.html_body.is_some());
        assert!(email.body_text.contains("Hello"));
    }

    #[test]
    fn test_parse_rfc822_empty_body() {
        let raw = b"From: a@x.com\r\nTo: b@x.com\r\nSubject: Empty\r\nDate: Mon, 01 Jan 2024 12:00:00 +0000\r\n\r\n";
        let email = parse_rfc822_to_fetched_email(raw).expect("should parse");
        assert!(email.body_text.is_empty() || email.body_text.trim().is_empty());
    }

    // -----------------------------------------------------------------------
    // scan_existing_message_ids (filesystem)
    // -----------------------------------------------------------------------

    #[test]
    fn test_scan_existing_message_ids_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ids = scan_existing_message_ids(dir.path()).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn test_scan_existing_message_ids_nonexistent_dir() {
        let ids = scan_existing_message_ids(Path::new("/nonexistent/path/12345")).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn test_scan_existing_message_ids_finds_ids() {
        let dir = tempfile::tempdir().unwrap();
        let content = "---\nmessage_id: \"<abc@example.com>\"\nstatus: inbox\n---\n\nBody";
        std::fs::write(dir.path().join("email1.md"), content).unwrap();
        let content2 = "---\nmessage_id: \"<def@example.com>\"\nstatus: inbox\n---\n\nBody";
        std::fs::write(dir.path().join("email2.md"), content2).unwrap();
        // Non-md file should be ignored
        std::fs::write(dir.path().join("notes.txt"), "---\nmessage_id: \"<skip>\"\n---\n").unwrap();

        let ids = scan_existing_message_ids(dir.path()).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("<abc@example.com>"));
        assert!(ids.contains("<def@example.com>"));
    }

    #[test]
    fn test_scan_existing_message_ids_no_message_id() {
        let dir = tempfile::tempdir().unwrap();
        let content = "---\nstatus: inbox\n---\n\nBody without message_id";
        std::fs::write(dir.path().join("email.md"), content).unwrap();

        let ids = scan_existing_message_ids(dir.path()).unwrap();
        assert!(ids.is_empty());
    }

    // -----------------------------------------------------------------------
    // list_attachments (filesystem)
    // -----------------------------------------------------------------------

    #[test]
    fn test_list_attachments_no_dir() {
        let dir = tempfile::tempdir().unwrap();
        let md_path = dir.path().join("email.md");
        let files = list_attachments(&md_path).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_list_attachments_with_files() {
        let dir = tempfile::tempdir().unwrap();
        let md_path = dir.path().join("email.md");
        let att_dir = dir.path().join("email_attachments");
        std::fs::create_dir(&att_dir).unwrap();
        std::fs::write(att_dir.join("doc.pdf"), b"pdf data").unwrap();
        std::fs::write(att_dir.join("img.png"), b"png data").unwrap();

        let files = list_attachments(&md_path).unwrap();
        assert_eq!(files.len(), 2);
        // Should be sorted by filename
        assert!(files[0].file_name().unwrap().to_str().unwrap() == "doc.pdf");
        assert!(files[1].file_name().unwrap().to_str().unwrap() == "img.png");
    }

    // -----------------------------------------------------------------------
    // save_fetched_emails (filesystem)
    // -----------------------------------------------------------------------

    #[test]
    fn test_save_fetched_emails_basic() {
        let dir = tempfile::tempdir().unwrap();
        let emails = vec![FetchedEmail {
            from: "alice@example.com".to_string(),
            to: "bob@example.com".to_string(),
            cc: None,
            subject: "Test Subject".to_string(),
            date: "Mon, 01 Jan 2024 12:00:00 +0000".to_string(),
            body_text: "Hello world".to_string(),
            html_body: None,
            has_attachments: false,
            message_id: Some("<test1@example.com>".to_string()),
            attachments: vec![],
        }];

        let (saved, skipped) = save_fetched_emails(&emails, dir.path(), "inbox").unwrap();
        assert_eq!(saved, 1);
        assert_eq!(skipped, 0);

        // Verify file was created
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        assert_eq!(files.len(), 1);

        let content = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(content.contains("message_id:"));
        assert!(content.contains("Hello world"));
    }

    #[test]
    fn test_save_fetched_emails_dedup_by_message_id() {
        let dir = tempfile::tempdir().unwrap();
        let email = FetchedEmail {
            from: "alice@example.com".to_string(),
            to: "bob@example.com".to_string(),
            cc: None,
            subject: "Dup Test".to_string(),
            date: "Mon, 01 Jan 2024 12:00:00 +0000".to_string(),
            body_text: "Body".to_string(),
            html_body: None,
            has_attachments: false,
            message_id: Some("<dup@example.com>".to_string()),
            attachments: vec![],
        };

        // Save once
        let (saved1, _) = save_fetched_emails(&[email.clone()], dir.path(), "inbox").unwrap();
        assert_eq!(saved1, 1);

        // Save again -- should be skipped
        let (saved2, skipped2) = save_fetched_emails(&[email], dir.path(), "inbox").unwrap();
        assert_eq!(saved2, 0);
        assert_eq!(skipped2, 1);
    }

    #[test]
    fn test_save_fetched_emails_with_html_companion() {
        let dir = tempfile::tempdir().unwrap();
        let emails = vec![FetchedEmail {
            from: "alice@example.com".to_string(),
            to: "bob@example.com".to_string(),
            cc: None,
            subject: "HTML Email".to_string(),
            date: "Mon, 01 Jan 2024 12:00:00 +0000".to_string(),
            body_text: "Plain text".to_string(),
            html_body: Some("<p>Rich text</p>".to_string()),
            has_attachments: false,
            message_id: Some("<html@example.com>".to_string()),
            attachments: vec![],
        }];

        save_fetched_emails(&emails, dir.path(), "inbox").unwrap();

        let html_files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "html"))
            .collect();
        assert_eq!(html_files.len(), 1);
        let html_content = std::fs::read_to_string(html_files[0].path()).unwrap();
        assert!(html_content.contains("Rich text"));
    }

    #[test]
    fn test_save_fetched_emails_with_attachments() {
        let dir = tempfile::tempdir().unwrap();
        let emails = vec![FetchedEmail {
            from: "alice@example.com".to_string(),
            to: "bob@example.com".to_string(),
            cc: None,
            subject: "Attachment Email".to_string(),
            date: "Mon, 01 Jan 2024 12:00:00 +0000".to_string(),
            body_text: "See attached".to_string(),
            html_body: None,
            has_attachments: true,
            message_id: Some("<att@example.com>".to_string()),
            attachments: vec![
                AttachmentData {
                    filename: "report.pdf".to_string(),
                    content: b"pdf content".to_vec(),
                },
                AttachmentData {
                    filename: "image.png".to_string(),
                    content: b"png content".to_vec(),
                },
            ],
        }];

        save_fetched_emails(&emails, dir.path(), "inbox").unwrap();

        // Find the md file and check its attachments dir
        let md_files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        assert_eq!(md_files.len(), 1);

        let att_dir = attachments_dir_for(&md_files[0].path());
        assert!(att_dir.is_dir());
        assert!(att_dir.join("report.pdf").exists());
        assert!(att_dir.join("image.png").exists());
        assert_eq!(
            std::fs::read(att_dir.join("report.pdf")).unwrap(),
            b"pdf content"
        );
    }

    // -----------------------------------------------------------------------
    // ensure_utf8_charset -- additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_ensure_utf8_charset_case_insensitive_check() {
        let html = r#"<html><head><meta CHARSET="utf-8"></head><body>hi</body></html>"#;
        assert_eq!(ensure_utf8_charset(html), html);
    }

    #[test]
    fn test_ensure_utf8_charset_html_with_attributes() {
        let html = r#"<html lang="en"><body>hi</body></html>"#;
        let result = ensure_utf8_charset(html);
        assert!(result.contains("<head>"));
        assert!(result.contains(r#"<meta charset="UTF-8">"#));
    }
}
