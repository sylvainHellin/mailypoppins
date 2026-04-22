use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use colored::*;
use gray_matter::{engine::YAML, Matter};
use log::info;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::config::{EmailSettings, SmtpConfig};
use crate::parse::{attachments_dir_for, extract_email_address, slugify_sender, slugify_subject};
use crate::types::{EmailDraft, EmailFrontmatter, EmailStatus, InboxFrontmatter};

pub fn select_inbox_email(inbox_dir: &Path, prompt: &str) -> Result<PathBuf> {
    let mut entries: Vec<(PathBuf, InboxFrontmatter)> = Vec::new();

    for entry in WalkDir::new(inbox_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let matter = Matter::<YAML>::new();
            let parsed = matter.parse(&content);
            if let Some(data) = parsed.data {
                if let Ok(fm) = data.deserialize::<InboxFrontmatter>() {
                    entries.push((path.to_path_buf(), fm));
                }
            }
        }
    }

    if entries.is_empty() {
        return Err(anyhow!("No inbox emails found in {}", inbox_dir.display()));
    }

    // Sort by date descending (most recent first)
    entries.sort_by(|a, b| {
        let da = a.1.date.as_deref().unwrap_or("");
        let db = b.1.date.as_deref().unwrap_or("");
        db.cmp(da)
    });

    // Build display strings for the fuzzy selector
    let display_items: Vec<String> = entries
        .iter()
        .map(|(_, fm)| {
            let date_short = fm
                .date
                .as_deref()
                .and_then(|d| chrono::DateTime::parse_from_rfc2822(d).ok())
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let from_short = extract_email_address(&fm.from);
            format!("{} | {} | {}", date_short, from_short, fm.subject)
        })
        .collect();

    let selection = dialoguer::FuzzySelect::new()
        .with_prompt(prompt)
        .items(&display_items)
        .default(0)
        .interact()
        .context("Selection cancelled")?;

    Ok(entries[selection].0.clone())
}

pub fn create_reply_draft(
    source_path: &Path,
    reply_all: bool,
    default_from: &str,
    drafts_dir: Option<&Path>,
) -> Result<PathBuf> {
    let content = fs::read_to_string(source_path)?;
    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);
    let inbox: InboxFrontmatter = parsed
        .data
        .ok_or_else(|| anyhow!("No frontmatter found"))?
        .deserialize()?;
    let original_body = parsed.content.trim();

    // Build reply fields
    let reply_to = extract_email_address(&inbox.from);

    let reply_cc = if reply_all {
        let mut all_recipients: Vec<String> = Vec::new();
        for addr in crate::send::split_addresses(&inbox.to) {
            let email = extract_email_address(&addr);
            if email.to_lowercase() != default_from.to_lowercase() {
                all_recipients.push(email);
            }
        }
        if let Some(ref cc) = inbox.cc {
            for addr in crate::send::split_addresses(cc) {
                let email = extract_email_address(&addr);
                if email.to_lowercase() != default_from.to_lowercase()
                    && !all_recipients
                        .iter()
                        .any(|r| r.to_lowercase() == email.to_lowercase())
                {
                    all_recipients.push(email);
                }
            }
        }
        if all_recipients.is_empty() {
            None
        } else {
            Some(all_recipients.join(", "))
        }
    } else {
        None
    };

    // Build subject with Re: prefix (case-insensitive check)
    let reply_subject = if inbox.subject.to_lowercase().starts_with("re: ") {
        inbox.subject.clone()
    } else {
        format!("Re: {}", inbox.subject)
    };

    // The body from the .md file is already plain text (either the server's
    // text/plain or the result of html_to_plain() at fetch time). Use as-is.
    let clean_body = original_body.to_string();

    // Build quoted body with attribution
    let attribution = format!(
        "On {}, {} wrote:",
        inbox.date.as_deref().unwrap_or("(unknown date)"),
        inbox.from
    );
    let quoted_body: String = clean_body
        .trim()
        .lines()
        .map(|line| format!("> {}", line))
        .collect::<Vec<_>>()
        .join("\n");

    // Build frontmatter
    let mut fm = String::from("---\n");
    fm.push_str(&format!("from: \"{}\"\n", default_from));
    fm.push_str(&format!("to: \"{}\"\n", reply_to));
    if let Some(ref cc) = reply_cc {
        fm.push_str(&format!("cc: \"{}\"\n", cc));
    }
    fm.push_str(&format!(
        "subject: \"{}\"\n",
        reply_subject.replace('"', "\\\"")
    ));
    fm.push_str("status: draft\n");
    fm.push_str("---\n");

    // Compose full content with {{SIGNATURE}} placeholder between reply area and quoted text
    let full_content = format!(
        "{}\n\n\n{{{{SIGNATURE}}}}\n\n{}\n{}\n",
        fm.trim_end(),
        attribution,
        quoted_body
    );

    // Determine output path
    let output_dir = drafts_dir.unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_dir)?;
    let date_prefix = Utc::now().format("%Y-%m-%d-%H%M").to_string();
    let sender_slug = slugify_sender(&inbox.from);
    let subject_slug = slugify_subject(&reply_subject);
    let filename = if subject_slug.is_empty() {
        format!("{}_{}_email.md", date_prefix, sender_slug)
    } else {
        format!("{}_{}_{}.md", date_prefix, sender_slug, subject_slug)
    };
    let mut dest = output_dir.join(&filename);

    // Avoid overwriting
    if dest.exists() {
        let mut counter = 1;
        loop {
            let name = if subject_slug.is_empty() {
                format!("{}_{}_email-{}.md", date_prefix, sender_slug, counter)
            } else {
                format!(
                    "{}_{}_{}-{}.md",
                    date_prefix, sender_slug, subject_slug, counter
                )
            };
            dest = output_dir.join(&name);
            if !dest.exists() {
                break;
            }
            counter += 1;
        }
    }

    fs::write(&dest, full_content)?;

    // Copy and wrap companion HTML for the draft if the original has one
    let source_html = source_path.with_extension("html");
    if source_html.exists() {
        if let Ok(html_content) = fs::read_to_string(&source_html) {
            let wrapped = format!(
                "<p style=\"color:#666\">On {}, {} wrote:</p>\n\
                 <div style=\"margin:0;padding:0 0 0 1em;border-left:2px solid #ccc\">\n\
                 {}\n\
                 </div>",
                inbox.date.as_deref().unwrap_or("(unknown date)"),
                inbox.from,
                html_content,
            );
            let draft_html = dest.with_extension("html");
            fs::write(&draft_html, wrapped)?;
        }
    }

    Ok(dest)
}

/// Compute the `Fwd: ...` subject that would be used for a forward draft of
/// the given source email, without writing the draft file.
/// Used by the TUI compose wizard to pre-populate the Subject field.
pub fn fwd_subject_from_source(source_path: &Path) -> Result<String> {
    let content = fs::read_to_string(source_path)?;
    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);
    let inbox: InboxFrontmatter = parsed
        .data
        .ok_or_else(|| anyhow!("No frontmatter found"))?
        .deserialize()?;
    if inbox.subject.to_lowercase().starts_with("fwd: ") {
        Ok(inbox.subject)
    } else {
        Ok(format!("Fwd: {}", inbox.subject))
    }
}

pub fn create_forward_draft(
    source_path: &Path,
    default_from: &str,
    drafts_dir: Option<&Path>,
) -> Result<PathBuf> {
    let content = fs::read_to_string(source_path)?;
    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);
    let inbox: InboxFrontmatter = parsed
        .data
        .ok_or_else(|| anyhow!("No frontmatter found"))?
        .deserialize()?;
    let original_body = parsed.content.trim();

    // Build forward subject
    let fwd_subject = if inbox.subject.to_lowercase().starts_with("fwd: ") {
        inbox.subject.clone()
    } else {
        format!("Fwd: {}", inbox.subject)
    };

    // Resolve attachment paths (absolute paths to source attachment files)
    let attachment_paths: Vec<String> = if let Some(ref filenames) = inbox.attachments {
        let att_dir = attachments_dir_for(source_path);
        filenames
            .iter()
            .filter_map(|name| {
                let abs_path = att_dir.join(name);
                if abs_path.exists() {
                    abs_path
                        .canonicalize()
                        .ok()
                        .map(|p| p.to_string_lossy().to_string())
                } else {
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    // Build frontmatter
    let mut fm = String::from("---\n");
    fm.push_str(&format!("from: \"{}\"\n", default_from));
    fm.push_str("to: \"\"\n");
    fm.push_str(&format!(
        "subject: \"{}\"\n",
        fwd_subject.replace('"', "\\\"")
    ));
    fm.push_str("status: draft\n");
    if !attachment_paths.is_empty() {
        fm.push_str("attachments:\n");
        for path in &attachment_paths {
            fm.push_str(&format!("  - \"{}\"\n", path.replace('"', "\\\"")));
        }
    }
    fm.push_str("---\n");

    // Build forwarded message header block
    let fwd_header = format!(
        "---------- Forwarded message ----------\n\
         From: {}\n\
         Date: {}\n\
         Subject: {}\n\
         To: {}",
        inbox.from,
        inbox.date.as_deref().unwrap_or("(unknown date)"),
        inbox.subject,
        inbox.to,
    );

    // The body from the .md file is already plain text (either the server's
    // text/plain or the result of html_to_plain() at fetch time). Use as-is.
    let clean_body = original_body.to_string();

    let full_content = format!(
        "{}\n\n\n{{{{SIGNATURE}}}}\n\n{}\n\n{}\n",
        fm.trim_end(),
        fwd_header,
        clean_body.trim()
    );

    // Determine output path
    let output_dir = drafts_dir.unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_dir)?;
    let date_prefix = Utc::now().format("%Y-%m-%d-%H%M").to_string();
    let sender_slug = slugify_sender(&inbox.from);
    let subject_slug = slugify_subject(&fwd_subject);
    let filename = if subject_slug.is_empty() {
        format!("{}_{}_email.md", date_prefix, sender_slug)
    } else {
        format!("{}_{}_{}.md", date_prefix, sender_slug, subject_slug)
    };
    let mut dest = output_dir.join(&filename);

    // Avoid overwriting
    if dest.exists() {
        let mut counter = 1;
        loop {
            let name = if subject_slug.is_empty() {
                format!("{}_{}_email-{}.md", date_prefix, sender_slug, counter)
            } else {
                format!(
                    "{}_{}_{}-{}.md",
                    date_prefix, sender_slug, subject_slug, counter
                )
            };
            dest = output_dir.join(&name);
            if !dest.exists() {
                break;
            }
            counter += 1;
        }
    }

    fs::write(&dest, full_content)?;

    // Create companion HTML for the forward
    let source_html = source_path.with_extension("html");
    if source_html.exists() {
        if let Ok(html_content) = fs::read_to_string(&source_html) {
            let wrapped = format!(
                "<p style=\"color:#666\">---------- Forwarded message ----------<br/>\n\
                 From: {}<br/>\n\
                 Date: {}<br/>\n\
                 Subject: {}<br/>\n\
                 To: {}</p>\n\
                 <div>\n\
                 {}\n\
                 </div>",
                inbox.from,
                inbox.date.as_deref().unwrap_or("(unknown date)"),
                inbox.subject,
                inbox.to,
                html_content,
            );
            let draft_html = dest.with_extension("html");
            fs::write(&draft_html, wrapped)?;
        }
    }

    Ok(dest)
}

pub fn parse_email_draft(path: &Path) -> Result<EmailDraft> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);

    let frontmatter: EmailFrontmatter = parsed
        .data
        .ok_or_else(|| anyhow!("No frontmatter found in file"))?
        .deserialize()
        .context("Failed to parse frontmatter")?;

    let body_markdown = parsed.content.trim().to_string();

    Ok(EmailDraft {
        path: path.to_path_buf(),
        frontmatter,
        body_markdown,
    })
}

pub fn validate_draft(draft: &EmailDraft) -> Result<Vec<String>> {
    let mut warnings = Vec::new();

    // Check that at least one recipient exists across to/cc/bcc
    let to_empty = draft.frontmatter.to.as_deref().map_or(true, |s| s.trim().is_empty());
    let cc_empty = draft.frontmatter.cc.as_deref().map_or(true, |s| s.trim().is_empty());
    let bcc_empty = draft.frontmatter.bcc.as_deref().map_or(true, |s| s.trim().is_empty());
    if to_empty && cc_empty && bcc_empty {
        return Err(anyhow!("No recipients (to, cc, and bcc are all empty)"));
    }

    if draft.frontmatter.subject.is_empty() {
        return Err(anyhow!("Missing 'subject' field"));
    }

    if draft.body_markdown.is_empty() {
        warnings.push("Email body is empty".to_string());
    }

    // Validate email format (basic check)
    if let Some(ref to) = draft.frontmatter.to {
        for email in crate::send::split_addresses(to) {
            if email.parse::<lettre::message::Mailbox>().is_err() {
                return Err(anyhow!("Invalid email address: {}", email));
            }
        }
    }

    // Check attachments exist
    if let Some(attachments) = &draft.frontmatter.attachments {
        for attachment in attachments {
            let expanded = shellexpand::tilde(attachment);
            if !Path::new(expanded.as_ref()).exists() {
                warnings.push(format!("Attachment not found: {}", attachment));
            }
        }
    }

    Ok(warnings)
}

pub fn preview_draft(
    draft: &EmailDraft,
    smtp_config: &SmtpConfig,
    email_config: &EmailSettings,
    signature: Option<&str>,
    is_dry_run: bool,
) -> Result<()> {
    println!("\n{}", "=== Email Draft Preview ===".bold().cyan());
    println!(
        "{}: {}",
        "From".bold(),
        draft
            .frontmatter
            .from
            .as_ref()
            .unwrap_or(&smtp_config.default_from)
    );
    println!("{}: {}", "To".bold(), draft.frontmatter.to.as_deref().unwrap_or("(bcc only)"));

    if let Some(cc) = &draft.frontmatter.cc {
        println!("{}: {}", "Cc".bold(), cc);
    }

    if let Some(bcc) = &draft.frontmatter.bcc {
        println!("{}: {}", "Bcc".bold(), bcc);
    }

    println!("{}: {}", "Subject".bold(), draft.frontmatter.subject);

    println!("\n{}\n", "--- Body Preview (first 500 chars) ---".dimmed());
    let preview: String = draft.body_markdown.chars().take(500).collect();
    println!("{}", preview);
    if draft.body_markdown.len() > 500 {
        println!("{}", "...".dimmed());
    }

    println!("\n{}", "--- Settings ---".dimmed());
    println!(
        "  Font: {} ({})",
        email_config.font_family, email_config.font_size
    );
    if let Some(sig) = signature {
        let sig_preview: String = sig.chars().take(50).collect();
        println!("  Signature: {} ...", sig_preview.replace('\n', " "));
    } else {
        println!("  Signature: {}", "none".dimmed());
    }

    println!("\n{}", "--- Status ---".dimmed());

    // Validate and show status
    match validate_draft(draft) {
        Ok(warnings) => {
            println!("{} Valid YAML frontmatter", "✓".green());
            println!(
                "{} Status: {}",
                "✓".green(),
                format!("{}", draft.frontmatter.status).yellow()
            );
            println!("{} All required fields present", "✓".green());

            for warning in warnings {
                println!("{} {}", "⚠".yellow(), warning);
            }
        }
        Err(e) => {
            println!("{} Validation error: {}", "✗".red(), e);
        }
    }

    if is_dry_run {
        println!(
            "\n{}\n",
            "[DRY RUN] Would send email (use 'send' subcommand to actually send)"
                .yellow()
                .bold()
        );
    }

    Ok(())
}

pub fn update_status_to_sent(
    draft: &EmailDraft,
    sent_dir: Option<&Path>,
    message_id: Option<&str>,
) -> Result<()> {
    info!("Updating status to sent: {}", draft.path.display());
    let mut frontmatter = draft.frontmatter.clone();
    frontmatter.status = EmailStatus::Sent;
    frontmatter.sent_at = Some(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    frontmatter.sent_via = Some(format!("email-cli v{}", env!("CARGO_PKG_VERSION")));
    frontmatter.message_id = message_id.map(|s| s.to_string());

    // Serialize the updated frontmatter
    let yaml = serde_yaml::to_string(&frontmatter)?;

    // Reconstruct the file content
    let new_content = format!("---\n{}---\n\n{}", yaml, draft.body_markdown);

    // Determine destination path
    let dest_path = if let Some(sent_dir) = sent_dir {
        fs::create_dir_all(sent_dir)?;
        // Keep the original filename (already includes date-time prefix)
        let original_name = draft
            .path
            .file_name()
            .ok_or_else(|| anyhow!("Draft path has no filename: {}", draft.path.display()))?
            .to_string_lossy();
        sent_dir.join(original_name.as_ref())
    } else {
        draft.path.clone()
    };

    // Write the updated content
    fs::write(&dest_path, new_content)?;

    // If we moved the file, remove the original
    if sent_dir.is_some() && dest_path != draft.path {
        fs::remove_file(&draft.path)?;
    }

    // Clean up companion HTML file (not needed in sent/)
    let html_companion = draft.path.with_extension("html");
    if html_companion.exists() {
        fs::remove_file(&html_companion).ok();
    }

    Ok(())
}

/// Resolve the drafts directory using a fallback chain:
/// 1. If the user passed an explicit (non-default) path, use it as-is.
/// 2. Else if config_drafts_dir is set and points to an existing directory, use that.
/// 3. Else fall back to "." (current directory).
pub fn resolve_drafts_dir(cli_dir: &Path, config_drafts_dir: &Option<PathBuf>) -> PathBuf {
    if cli_dir != Path::new(".") {
        return cli_dir.to_path_buf();
    }
    if let Some(ref dir) = config_drafts_dir {
        if dir.is_dir() {
            return dir.clone();
        }
    }
    cli_dir.to_path_buf()
}

pub fn find_drafts(dir: &Path, status_filter: Option<EmailStatus>) -> Result<Vec<EmailDraft>> {
    let mut drafts = Vec::new();

    for entry in WalkDir::new(dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            match parse_email_draft(path) {
                Ok(draft) => {
                    if let Some(ref filter) = status_filter {
                        if &draft.frontmatter.status == filter {
                            drafts.push(draft);
                        }
                    } else {
                        drafts.push(draft);
                    }
                }
                Err(e) => {
                    eprintln!("{} Skipping {}: {}", "⚠".yellow(), path.display(), e);
                }
            }
        }
    }

    Ok(drafts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EmailDraft, EmailFrontmatter, EmailStatus};
    use std::path::PathBuf;

    fn make_draft(to: &str, subject: &str, body: &str, status: EmailStatus) -> EmailDraft {
        let to_opt = if to.is_empty() { None } else { Some(to.to_string()) };
        EmailDraft {
            path: PathBuf::from("test.md"),
            frontmatter: EmailFrontmatter {
                to: to_opt,
                cc: None,
                bcc: None,
                subject: subject.to_string(),
                status,
                from: Some("me@example.com".to_string()),
                reply_to: None,
                attachments: None,
                sent_at: None,
                sent_via: None,
                message_id: None,
            },
            body_markdown: body.to_string(),
        }
    }

    #[test]
    fn test_validate_draft_valid() {
        let draft = make_draft(
            "alice@example.com",
            "Hello",
            "Body text",
            EmailStatus::Draft,
        );
        let result = validate_draft(&draft);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_validate_draft_missing_to() {
        let draft = make_draft("", "Hello", "Body text", EmailStatus::Draft);
        let result = validate_draft(&draft);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No recipients"));
    }

    #[test]
    fn test_validate_draft_bcc_only() {
        let mut draft = make_draft("", "Hello", "Body text", EmailStatus::Draft);
        draft.frontmatter.bcc = Some("secret@example.com".to_string());
        let result = validate_draft(&draft);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_draft_missing_subject() {
        let draft = make_draft("alice@example.com", "", "Body text", EmailStatus::Draft);
        let result = validate_draft(&draft);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("subject"));
    }

    #[test]
    fn test_validate_draft_empty_body_warning() {
        let draft = make_draft("alice@example.com", "Hello", "", EmailStatus::Draft);
        let result = validate_draft(&draft);
        assert!(result.is_ok());
        let warnings = result.unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("empty"));
    }

    #[test]
    fn test_validate_draft_invalid_email() {
        let draft = make_draft("not-an-email", "Hello", "Body", EmailStatus::Draft);
        let result = validate_draft(&draft);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid email"));
    }

    #[test]
    fn test_validate_draft_multiple_recipients_one_invalid() {
        let draft = make_draft(
            "alice@example.com, badaddr",
            "Hello",
            "Body",
            EmailStatus::Draft,
        );
        let result = validate_draft(&draft);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_drafts_dir_explicit_path() {
        let result = resolve_drafts_dir(Path::new("/explicit/path"), &None);
        assert_eq!(result, PathBuf::from("/explicit/path"));
    }

    #[test]
    fn test_resolve_drafts_dir_config_fallback() {
        // When cli_dir is ".", use config dir if it exists
        // We can't easily test a real dir here, so test the default fallback
        let result = resolve_drafts_dir(Path::new("."), &Some(PathBuf::from("/nonexistent")));
        // /nonexistent doesn't exist as a dir, so should fall back to "."
        assert_eq!(result, PathBuf::from("."));
    }

    #[test]
    fn test_resolve_drafts_dir_default_fallback() {
        let result = resolve_drafts_dir(Path::new("."), &None);
        assert_eq!(result, PathBuf::from("."));
    }
}

pub fn mark_as_approved(path: &Path) -> Result<String> {
    let draft = parse_email_draft(path)?;

    if draft.frontmatter.status == EmailStatus::Approved {
        return Ok(format!("Already approved: {}", path.display()));
    }

    if draft.frontmatter.status == EmailStatus::Sent {
        return Err(anyhow!("Cannot approve an already sent email"));
    }

    let mut frontmatter = draft.frontmatter.clone();
    frontmatter.status = EmailStatus::Approved;

    let yaml = serde_yaml::to_string(&frontmatter)?;
    let new_content = format!("---\n{}---\n\n{}", yaml, draft.body_markdown);

    fs::write(path, new_content)?;

    Ok(format!("Marked as approved: {}", path.display()))
}
