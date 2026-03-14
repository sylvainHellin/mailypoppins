use anyhow::Result;
use chrono::Utc;
use colored::*;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

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

pub struct AttachmentData {
    pub filename: String,
    pub content: Vec<u8>,
}

pub fn html_to_plain(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 80).unwrap_or_else(|_| html.to_string())
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

fn sanitize_attachment_filename(name: &str) -> String {
    let name = name.replace(['/', '\\', '\0'], "_");
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim().to_string();
    if name.is_empty() {
        "attachment.bin".to_string()
    } else if name.len() > 200 {
        name[..200].to_string()
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
    // Collapse consecutive hyphens
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            prev_hyphen = false;
            result.push(c);
        }
    }
    let result = result.trim_matches('-').to_string();
    if result.len() > 40 {
        // Truncate at word boundary
        let truncated = &result[..40];
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
    // Collapse consecutive hyphens and trim
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            prev_hyphen = false;
            result.push(c);
        }
    }
    result.trim_matches('-').to_string()
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

pub fn save_fetched_emails(emails: &[FetchedEmail], inbox_dir: &Path, status: &str) -> Result<(usize, usize)> {
    fs::create_dir_all(inbox_dir)?;

    // Collect existing message IDs from inbox
    let mut existing_ids: HashSet<String> = HashSet::new();
    for entry in WalkDir::new(inbox_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            if let Ok(content) = fs::read_to_string(path) {
                // Extract message_id from YAML frontmatter
                let mut in_frontmatter = false;
                for line in content.lines() {
                    if line == "---" {
                        if !in_frontmatter {
                            in_frontmatter = true;
                            continue;
                        } else {
                            break; // end of frontmatter
                        }
                    }
                    if in_frontmatter && line.starts_with("message_id:") {
                        let id = line.trim_start_matches("message_id:").trim().trim_matches('"');
                        if !id.is_empty() {
                            existing_ids.insert(id.to_string());
                        }
                        break;
                    }
                }
            }
        }
    }

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

        // Build frontmatter
        let fetched_at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut frontmatter = String::from("---\n");
        frontmatter.push_str(&format!("from: \"{}\"\n", email.from.replace('"', "\\\"")));
        frontmatter.push_str(&format!("to: \"{}\"\n", email.to.replace('"', "\\\"")));
        if let Some(ref cc) = email.cc {
            frontmatter.push_str(&format!("cc: \"{}\"\n", cc.replace('"', "\\\"")));
        }
        frontmatter.push_str(&format!("subject: \"{}\"\n", email.subject.replace('"', "\\\"")));
        frontmatter.push_str(&format!("date: \"{}\"\n", email.date.replace('"', "\\\"")));
        if let Some(ref mid) = email.message_id {
            frontmatter.push_str(&format!("message_id: \"{}\"\n", mid.replace('"', "\\\"")));
        }
        frontmatter.push_str(&format!("status: {}\n", status));
        frontmatter.push_str(&format!("has_attachments: {}\n", email.has_attachments));
        // Save attachment files and record filenames in frontmatter
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
        if !saved_filenames.is_empty() {
            frontmatter.push_str("attachments:\n");
            for name in &saved_filenames {
                frontmatter.push_str(&format!("  - \"{}\"\n", name.replace('"', "\\\"")));
            }
        }
        frontmatter.push_str(&format!("fetched_at: \"{}\"\n", fetched_at));
        frontmatter.push_str("---\n\n");

        let content = format!("{}{}", frontmatter, email.body_text);
        fs::write(&dest, content)?;

        // Save companion HTML file if available
        if let Some(ref html) = email.html_body {
            let html_path = dest.with_extension("html");
            fs::write(&html_path, html)?;
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
        println!("{}: {}", "Date".bold().magenta(), email.date);
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
