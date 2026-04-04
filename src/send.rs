use anyhow::{anyhow, Context, Result};
use lettre::{
    address::Envelope,
    message::{header::ContentType, Attachment, Mailbox, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use log::{debug, error, info};
use pulldown_cmark::{html, Options, Parser as MdParser};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::config::{EmailSettings, SmtpConfig};
use crate::types::{EmailDraft, EmailStatus};

// ---------------------------------------------------------------------------
// Per-recipient sending types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecipientRole {
    To,
    Cc,
    Bcc,
}

impl std::fmt::Display for RecipientRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecipientRole::To => write!(f, "To"),
            RecipientRole::Cc => write!(f, "Cc"),
            RecipientRole::Bcc => write!(f, "Bcc"),
        }
    }
}

#[derive(Debug)]
pub struct RecipientResult {
    pub address: String,
    pub role: RecipientRole,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct SendResult {
    pub results: Vec<RecipientResult>,
}

impl SendResult {
    pub fn all_succeeded(&self) -> bool {
        self.results.iter().all(|r| r.success)
    }

    pub fn any_succeeded(&self) -> bool {
        self.results.iter().any(|r| r.success)
    }

    pub fn succeeded(&self) -> Vec<&RecipientResult> {
        self.results.iter().filter(|r| r.success).collect()
    }

    pub fn failed(&self) -> Vec<&RecipientResult> {
        self.results.iter().filter(|r| !r.success).collect()
    }
}

pub fn markdown_to_html(
    markdown: &str,
    config: &EmailSettings,
    signature: Option<&str>,
    quoted_html: Option<&str>,
) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);

    let parser = MdParser::new_ext(markdown, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    // Handle signature placement:
    // If {{SIGNATURE}} placeholder is present (reply drafts), split the HTML at the placeholder,
    // inject the signature, and wrap the quoted content in a styled div.
    // Replace <blockquote> with styled <div> in the quoted section to prevent email clients
    // (Apple Mail, Gmail) from collapsing the signature and conversation behind "see more".
    // Otherwise (regular drafts), append signature at the end.
    let signature_html = signature.unwrap_or_default();
    let body = if html_output.contains("{{SIGNATURE}}") {
        // pulldown-cmark wraps the placeholder in <p> tags; match that form first
        let marker = if html_output.contains("<p>{{SIGNATURE}}</p>") {
            "<p>{{SIGNATURE}}</p>"
        } else {
            "{{SIGNATURE}}"
        };
        let parts: Vec<&str> = html_output.splitn(2, marker).collect();
        let reply_part = parts[0];

        if let Some(original_html) = quoted_html {
            // Use original HTML instead of Markdown-converted blockquotes
            format!(
                "{}\n{}\n<div style=\"padding-top:1em\">\n{}\n</div>",
                reply_part.trim_end(),
                signature_html,
                original_html,
            )
        } else {
            // Fallback: convert Markdown blockquotes to styled divs
            let quoted_part = if parts.len() > 1 { parts[1] } else { "" };
            let quoted_styled = quoted_part
                .replace("<blockquote>", "<div style=\"margin:0;padding:0 0 0 1em;border-left:2px solid #ccc\">")
                .replace("</blockquote>", "</div>");
            format!(
                "{}\n{}\n<div style=\"padding-top:1em\">\n{}\n</div>",
                reply_part.trim_end(),
                signature_html,
                quoted_styled.trim_start()
            )
        }
    } else {
        format!("{}\n{}", html_output, signature_html)
    };

    // Wrap in basic HTML structure with styling from config
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8">
<style>
body {{ font-family: {font_family}; font-size: {font_size}; line-height: 1.6; color: #000; }}
a {{ color: #0066cc; }}
p {{ margin: 0 0 1em 0; }}
blockquote {{ margin: 0.5em 0; padding: 0 0 0 1em; border-left: 2px solid #ccc; white-space: pre-wrap; }}
</style>
</head>
<body>
{body}
</body>
</html>"#,
        font_family = config.font_family,
        font_size = config.font_size,
        body = body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EmailSettings;
    use insta::assert_snapshot;

    fn default_settings() -> EmailSettings {
        EmailSettings::default()
    }

    #[test]
    fn test_markdown_to_html_basic_paragraph() {
        let html = markdown_to_html("Hello **world**!\n\nSecond paragraph.", &default_settings(), None, None);
        assert_snapshot!(html);
    }

    #[test]
    fn test_markdown_to_html_with_signature_placeholder() {
        let md = "My reply\n\n{{SIGNATURE}}\n\n> Original message";
        let sig = "<p>-- Best, Alice</p>";
        let html = markdown_to_html(md, &default_settings(), Some(sig), None);
        assert_snapshot!(html);
    }

    #[test]
    fn test_markdown_to_html_signature_with_quoted_html() {
        let md = "My reply\n\n{{SIGNATURE}}\n\n> Quoted text";
        let sig = "<p>-- Best, Alice</p>";
        let quoted = "<p>Original HTML content</p>";
        let html = markdown_to_html(md, &default_settings(), Some(sig), Some(quoted));
        assert_snapshot!(html);
    }

    #[test]
    fn test_markdown_to_html_signature_without_quoted_html() {
        let md = "My reply\n\n{{SIGNATURE}}\n\n> Quoted text";
        let sig = "<p>-- Best, Alice</p>";
        let html = markdown_to_html(md, &default_settings(), Some(sig), None);
        assert_snapshot!(html);
    }

    #[test]
    fn test_markdown_to_html_tables_and_links() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n\n[Link](https://example.com)\n\n~~strikethrough~~";
        let html = markdown_to_html(md, &default_settings(), None, None);
        assert_snapshot!(html);
    }

    #[test]
    fn test_markdown_to_html_empty_body() {
        let html = markdown_to_html("", &default_settings(), None, None);
        assert_snapshot!(html);
    }

    #[test]
    fn test_markdown_to_html_custom_font() {
        let settings = EmailSettings {
            font_family: "Georgia, serif".to_string(),
            font_size: "14px".to_string(),
            include_signature: true,
        };
        let html = markdown_to_html("Hello", &settings, None, None);
        assert!(html.contains("Georgia, serif"));
        assert!(html.contains("14px"));
    }
}

pub async fn send_email(
    draft: &EmailDraft,
    smtp_config: &SmtpConfig,
    email_config: &EmailSettings,
    signature: Option<&str>,
) -> Result<(SendResult, Vec<u8>, Option<String>)> {
    // Check status
    if draft.frontmatter.status != EmailStatus::Approved {
        return Err(anyhow!(
            "Email not approved for sending. Current status: {}",
            draft.frontmatter.status
        ));
    }

    let from_address = draft
        .frontmatter
        .from
        .as_ref()
        .unwrap_or(&smtp_config.default_from);

    let from_mailbox: Mailbox = from_address
        .parse()
        .context("Invalid 'from' email address")?;

    info!(
        "Sending email: subject=\"{}\", from={}",
        draft.frontmatter.subject, from_address
    );

    // Collect all recipients with roles, deduplicating by address
    let mut seen = HashSet::new();
    let mut recipients: Vec<(String, RecipientRole)> = Vec::new();

    for to in draft.frontmatter.to.split(',') {
        let addr = to.trim().to_string();
        if !addr.is_empty() && seen.insert(addr.to_lowercase()) {
            recipients.push((addr, RecipientRole::To));
        }
    }
    if let Some(cc) = &draft.frontmatter.cc {
        for cc_addr in cc.split(',') {
            let addr = cc_addr.trim().to_string();
            if !addr.is_empty() && seen.insert(addr.to_lowercase()) {
                recipients.push((addr, RecipientRole::Cc));
            }
        }
    }
    if let Some(bcc) = &draft.frontmatter.bcc {
        for bcc_addr in bcc.split(',') {
            let addr = bcc_addr.trim().to_string();
            if !addr.is_empty() && seen.insert(addr.to_lowercase()) {
                recipients.push((addr, RecipientRole::Bcc));
            }
        }
    }

    if recipients.is_empty() {
        return Err(anyhow!("No recipients specified"));
    }

    debug!(
        "Recipients ({}): {:?}",
        recipients.len(),
        recipients
            .iter()
            .map(|(a, r)| format!("{}({})", a, r))
            .collect::<Vec<_>>()
    );

    // Build the message with visible To/Cc headers (Bcc omitted from headers by lettre)
    // message_id(None) triggers auto-generation of a unique Message-ID header
    let mut builder = Message::builder()
        .from(from_mailbox.clone())
        .subject(&draft.frontmatter.subject)
        .message_id(None);

    // Add To recipients to headers
    for (addr, role) in &recipients {
        match role {
            RecipientRole::To => {
                let mbox: Mailbox = addr.parse().context("Invalid 'to' email address")?;
                builder = builder.to(mbox);
            }
            RecipientRole::Cc => {
                let mbox: Mailbox = addr.parse().context("Invalid 'cc' email address")?;
                builder = builder.cc(mbox);
            }
            RecipientRole::Bcc => {
                let mbox: Mailbox = addr.parse().context("Invalid 'bcc' email address")?;
                builder = builder.bcc(mbox);
            }
        }
    }

    // Add Reply-To
    if let Some(reply_to) = &draft.frontmatter.reply_to {
        let reply_mailbox: Mailbox = reply_to
            .parse()
            .context("Invalid 'reply_to' email address")?;
        builder = builder.reply_to(reply_mailbox);
    }

    // Load companion HTML for quoted section if available
    let html_companion_path = draft.path.with_extension("html");
    let quoted_html = if html_companion_path.exists() {
        fs::read_to_string(&html_companion_path).ok()
    } else {
        None
    };

    // Generate HTML with signature (and original HTML for quoted section if available)
    let body_html = markdown_to_html(
        &draft.body_markdown,
        email_config,
        signature,
        quoted_html.as_deref(),
    );

    // Build the text/html alternative part
    let body_multipart = MultiPart::alternative()
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(draft.body_markdown.clone()),
        )
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(body_html),
        );

    // Build message with or without attachments
    let message = if let Some(attachments) = &draft.frontmatter.attachments {
        if !attachments.is_empty() {
            let mut mixed = MultiPart::mixed().multipart(body_multipart);

            for attachment_path in attachments {
                let expanded = shellexpand::tilde(attachment_path);
                let path = Path::new(expanded.as_ref());

                let file_content = fs::read(path)
                    .with_context(|| format!("Failed to read attachment: {}", attachment_path))?;

                let filename = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "attachment".to_string());

                let content_type = mime_guess::from_path(path)
                    .first_or_octet_stream()
                    .to_string();

                let attachment = Attachment::new(filename).body(file_content, content_type.parse().unwrap());
                mixed = mixed.singlepart(attachment);
            }

            builder.multipart(mixed).context("Failed to build email message")?
        } else {
            builder.multipart(body_multipart).context("Failed to build email message")?
        }
    } else {
        builder.multipart(body_multipart).context("Failed to build email message")?
    };

    // Get raw message bytes for send_raw and IMAP APPEND
    let raw_message = message.formatted();

    // Extract Message-ID from the raw message headers
    let message_id = mailparse::parse_headers(&raw_message)
        .ok()
        .and_then(|(headers, _)| {
            headers.iter()
                .find(|h| h.get_key().eq_ignore_ascii_case("Message-ID"))
                .map(|h| h.get_value().trim().to_string())
        });

    // Parse from address for envelope
    let from_addr: lettre::Address = from_address
        .parse::<Mailbox>()
        .context("Invalid 'from' address")?
        .email;

    // Create SMTP transport
    let creds = Credentials::new(smtp_config.username.clone(), smtp_config.password.clone());

    let mailer: AsyncSmtpTransport<Tokio1Executor> = if smtp_config.port == 465 {
        // Implicit TLS (SMTPS)
        let mut transport = AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp_config.host)?;
        if smtp_config.accept_invalid_certs {
            let tls_params = lettre::transport::smtp::client::TlsParameters::builder(smtp_config.host.clone())
                .dangerous_accept_invalid_certs(true)
                .build()?;
            transport = transport.tls(lettre::transport::smtp::client::Tls::Wrapper(tls_params));
        }
        transport.port(smtp_config.port).credentials(creds).build()
    } else {
        // STARTTLS
        let mut transport = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp_config.host)?;
        if smtp_config.accept_invalid_certs {
            let tls_params = lettre::transport::smtp::client::TlsParameters::builder(smtp_config.host.clone())
                .dangerous_accept_invalid_certs(true)
                .build()?;
            transport = transport.tls(lettre::transport::smtp::client::Tls::Required(tls_params));
        }
        transport.port(smtp_config.port).credentials(creds).build()
    };

    // Send to each recipient individually
    let mut results = Vec::with_capacity(recipients.len());

    for (addr, role) in &recipients {
        let rcpt_addr: lettre::Address = match addr.parse::<Mailbox>() {
            Ok(mbox) => mbox.email,
            Err(e) => {
                let err_msg = format!("Invalid address '{}': {}", addr, e);
                error!("{}", err_msg);
                results.push(RecipientResult {
                    address: addr.clone(),
                    role: *role,
                    success: false,
                    error: Some(err_msg),
                });
                continue;
            }
        };

        let envelope = match Envelope::new(Some(from_addr.clone()), vec![rcpt_addr]) {
            Ok(env) => env,
            Err(e) => {
                let err_msg = format!("Failed to create envelope for '{}': {}", addr, e);
                error!("{}", err_msg);
                results.push(RecipientResult {
                    address: addr.clone(),
                    role: *role,
                    success: false,
                    error: Some(err_msg),
                });
                continue;
            }
        };

        match mailer.send_raw(&envelope, &raw_message).await {
            Ok(_) => {
                info!("Sent to {} ({})", addr, role);
                results.push(RecipientResult {
                    address: addr.clone(),
                    role: *role,
                    success: true,
                    error: None,
                });
            }
            Err(e) => {
                let err_msg = format!("{}", e);
                error!("Failed to send to {} ({}): {}", addr, role, err_msg);
                results.push(RecipientResult {
                    address: addr.clone(),
                    role: *role,
                    success: false,
                    error: Some(err_msg),
                });
            }
        }
    }

    Ok((SendResult { results }, raw_message, message_id))
}
