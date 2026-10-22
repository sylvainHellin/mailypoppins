use anyhow::{anyhow, Context, Result};
use lettre::{
    address::Envelope,
    message::{
        header::{ContentTransferEncoding, ContentType},
        Attachment, Body, Mailbox, MultiPart, SinglePart,
    },
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use log::{debug, error, info, warn};
use pulldown_cmark::{html, Event, Options, Parser as MdParser};
use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{AuthMethod, EmailSettings, SmtpConfig};
use crate::types::{EmailDraft, EmailStatus};

pub mod graph_submit;

// The RFC 5322 address-string half of this module, which needs no transport and
// which `draft` reads without wanting one, lives in `mp-core` since #0126
// (P5-U10b). Re-exported so `crate::send::split_addresses` and the two
// formatters resolve here as they always did.
pub use mp_core::addresses::{format_recipient, normalize_address_for_smtp, split_addresses};

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

/// What the transport said about one recipient (#0063).
///
/// SMTP is one conversation per recipient here, so this is a verdict about
/// that recipient and not about the message: the four values are what the
/// outbox needs to decide whether that recipient may be attempted again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipientVerdict {
    /// 250. The recipient has the message and is never attempted again.
    Delivered,
    /// A 5xx, or an address no envelope can be built from. Refused for good.
    Rejected,
    /// A 4xx, no connection, no credentials: nothing was accepted and the next
    /// pass may well succeed.
    Retryable,
    /// No verdict came back, so the recipient may or may not hold the message.
    Unknown,
}

#[derive(Debug)]
pub struct RecipientResult {
    pub address: String,
    pub role: RecipientRole,
    pub success: bool,
    pub error: Option<String>,
    /// What the transport said about this recipient, which is what the outbox
    /// records durably; see [`SendResult::submit_outcome`].
    pub verdict: RecipientVerdict,
}

impl RecipientResult {
    /// True when the failure leaves it unknown whether the server accepted the
    /// message. Drives the outbox's never-auto-re-send rule.
    pub fn ambiguous(&self) -> bool {
        self.verdict == RecipientVerdict::Unknown
    }
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

    /// How the durable outbox must read this result (#0037 item 5, #0063).
    ///
    /// One verdict per recipient, because that is what the per-recipient loop
    /// in [`submit`] actually produced: the same bytes go out in separate
    /// envelopes, so "the message was accepted" is not a fact about the
    /// message but about each recipient in turn. The outbox folds the set into
    /// a row state and, more to the point, remembers who took it, so a retry
    /// never delivers twice.
    pub fn submit_outcome(&self) -> crate::outbox::SubmitOutcome {
        let mut verdicts = crate::outbox::RecipientVerdicts::default();
        for r in &self.results {
            let reason = r
                .error
                .clone()
                .unwrap_or_else(|| "unknown error".to_string());
            match r.verdict {
                RecipientVerdict::Delivered => verdicts.delivered.push(r.address.clone()),
                RecipientVerdict::Rejected => verdicts.rejected.push((r.address.clone(), reason)),
                RecipientVerdict::Retryable => verdicts.retryable.push((r.address.clone(), reason)),
                RecipientVerdict::Unknown => verdicts.ambiguous.push((r.address.clone(), reason)),
            }
        }
        crate::outbox::SubmitOutcome::PerRecipient(verdicts)
    }
}

/// What one recipient's SMTP failure means for that recipient (#0063).
///
/// The question is never "did it work" but "may this recipient be attempted
/// again", and SMTP answers it in three ways:
///
/// - a 5xx is the server refusing in words, and it will refuse again: the
///   recipient is rejected for good and the user has to be told;
/// - a 4xx, a client-side error (no credentials, no usable mechanism) or a TLS
///   failure all happen before any bytes could be accepted, and all of them
///   can be gone by the next attempt: retryable;
/// - anything else is a timeout or a connection that died somewhere in the
///   conversation, where the 250 may simply have been lost on the way back:
///   unknown, and never attempted again automatically.
fn smtp_failure_verdict(err: &lettre::transport::smtp::Error) -> RecipientVerdict {
    if err.is_timeout() {
        return RecipientVerdict::Unknown;
    }
    if err.is_permanent() {
        return RecipientVerdict::Rejected;
    }
    if err.is_transient() || err.is_client() || err.is_tls() {
        return RecipientVerdict::Retryable;
    }
    RecipientVerdict::Unknown
}

pub fn markdown_to_html(
    markdown: &str,
    config: &EmailSettings,
    signature: Option<&str>,
    quoted_html: Option<&str>,
) -> String {
    // The signature is Markdown (#0099, resolved by
    // `config::resolve_signature_markdown`). Draft sends pass `None` because the
    // signature was already spliced into the draft body at reply/forward/compose
    // time; only the send-time paths that have no editable draft (invites,
    // direct sends) pass `Some`. Append it to the body Markdown before rendering
    // so it goes through the same converter as the body and inherits the body
    // font, instead of being injected as pre-styled HTML (which rendered the
    // signature at a different size, and double-injected it on replies).
    // Drop the signature sentinel comment lines (#0106) before rendering. They
    // wrap the spliced block in the draft body so a re-splice can find it;
    // CommonMark would otherwise pass them through as raw HTML comments into the
    // sent message. Stripping the lines keeps the signature content and its
    // surrounding blank lines intact.
    //
    // Rewrite the quote boundary to the one `QUOTE_MARKER` form (PERSO-114),
    // so a legacy `{{SIGNATURE}}` draft splits on the same token as a new one.
    // On its own line the marker is an HTML comment block, which CommonMark
    // passes through verbatim and which interrupts the paragraph above it, so
    // the split below lands between two closed blocks.
    let sanitized =
        crate::draft::normalize_quote_markers(&crate::draft::strip_signature_sentinels(markdown));
    let markdown = sanitized.as_str();

    let owned_body;
    let markdown = match signature {
        Some(sig) if !sig.trim().is_empty() => {
            owned_body = format!("{}\n\n{}", markdown.trim_end(), sig.trim());
            owned_body.as_str()
        }
        _ => markdown,
    };

    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);

    // Below the quote marker the draft holds the quoted original: its plain
    // text and the attribution or forward header built from its headers, all
    // text that was never meant as markup. Raw HTML there is tag-shaped text
    // (a subject `Q&A <draft>`, a display name `"Ops <team>"`) that clients
    // would drop as unknown tags, so it renders escaped (PERSO-169). The
    // `<addr@x>` form is an autolink, not raw HTML, and keeps its link; the
    // marker itself stays raw so the split below finds it.
    let mut in_quote = false;
    let parser = MdParser::new_ext(markdown, options).map(|event| match event {
        Event::Html(raw) if raw.contains(crate::draft::QUOTE_MARKER) => {
            in_quote = true;
            Event::Html(raw)
        }
        Event::Html(raw) if in_quote => Event::Text(raw),
        other => other,
    });
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    // The inline style copy lands in a double-quoted `style="..."` attribute, so
    // any literal double quote in the font stack (e.g. `"Times New Roman", serif`)
    // would prematurely close the attribute. Escape it to `&quot;` for that
    // context; the <style> block is not an attribute and takes the value raw.
    let font_family_attr = config.font_family.replace('"', "&quot;");

    // The quote marker (reply/forward drafts, `draft::QUOTE_MARKER`) is the
    // boundary where the quoted section begins; it carries no signature.
    // Split there and wrap the quoted content in a styled <div> so email clients
    // (Apple Mail, Gmail) do not collapse the reply and signature behind "see
    // more". Replace <blockquote> with styled <div> in the quoted section for
    // the same reason. Regular drafts have no marker and render as-is.
    //
    // Each Markdown-derived fragment is then run through `inline_element_styles`
    // (#0118), which stamps the <style> block's declarations onto the individual
    // elements. The split happens first so the marker still matches the raw
    // comment pulldown-cmark emits, and so the quoted original HTML of a reply
    // passes through with its own styling untouched.
    let marker = crate::draft::QUOTE_MARKER;
    let body = if html_output.contains(marker) {
        let parts: Vec<&str> = html_output.splitn(2, marker).collect();
        let reply_part = inline_element_styles(parts[0], &font_family_attr, &config.font_size);

        if let Some(original_html) = quoted_html {
            // Use original HTML instead of Markdown-converted blockquotes
            format!(
                "{}\n<div style=\"padding-top:1em\">\n{}\n</div>",
                reply_part.trim_end(),
                original_html,
            )
        } else {
            // Fallback: convert Markdown blockquotes to styled divs. The
            // replacement runs before the stamping, or the rewritten
            // `<blockquote style="...">` would no longer match it.
            // A second marker line (a hand-edited draft) must not ride along
            // as a raw comment into the sent HTML.
            let quoted_part = if parts.len() > 1 { parts[1] } else { "" };
            let quoted_styled = quoted_part
                .replace(marker, "")
                .replace(
                    "<blockquote>",
                    "<div style=\"margin:0;padding:0 0 0 1em;border-left:2px solid #ccc\">",
                )
                .replace("</blockquote>", "</div>");
            let quoted_styled =
                inline_element_styles(&quoted_styled, &font_family_attr, &config.font_size);
            format!(
                "{}\n<div style=\"padding-top:1em\">\n{}\n</div>",
                reply_part.trim_end(),
                quoted_styled.trim_start()
            )
        }
    } else {
        inline_element_styles(&html_output, &font_family_attr, &config.font_size)
    };

    // Wrap in basic HTML structure with styling from config. The font is set in
    // three places: the head <style>, an inline style on the content wrapper,
    // and an inline style on every element inside it. Gmail and Outlook strip
    // <style> blocks, and Apple Mail drops the wrapper's inherited font for part
    // of the content (#0118), so only the per-element copy is load-bearing
    // everywhere. The other two stay for the clients that honour them and for
    // anything the stamping does not reach.
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
<div style="font-family: {font_family_attr}; font-size: {font_size}; line-height: 1.6; color: #000;">
{body}
</div>
</body>
</html>"#,
        font_family = config.font_family,
        font_family_attr = font_family_attr,
        font_size = config.font_size,
        body = body,
    )
}

/// Stamp the `<head><style>` declarations onto each element as an inline
/// `style` attribute (#0118).
///
/// A message used to carry its font once, on a wrapper `<div>`, and let it
/// inherit. Apple Mail's reading pane breaks that: in a message whose body mixed
/// paragraphs and a bullet list it kept the wrapper's `font-size` for the `<ul>`
/// and fell back to its own default for every `<p>`, and applied the wrapper's
/// `line-height` nowhere. The list came out a quarter larger than the prose
/// around it. Outlook rendered the same bytes uniformly, and so did WebKit given
/// the message on its own, in quirks mode, or under Mail's own
/// `MUIWebDocument.css` -- the mangling is not reproducible outside Mail, so the
/// fix is to stop depending on inheritance rather than to find the rule that
/// breaks it.
///
/// Applied only to the Markdown-derived fragments. The quoted original HTML of a
/// reply or forward carries the sender's own styling and is passed through
/// untouched.
///
/// Tag scanning is naive: a `>` inside an attribute value ends the tag early.
/// pulldown-cmark escapes its own output, so this only misfires on raw HTML
/// written by hand in a draft body, where the surrounding `<blockquote>` string
/// replacement is already just as naive.
fn inline_element_styles(html: &str, font_family_attr: &str, font_size: &str) -> String {
    let mut out = String::with_capacity(html.len() + html.len() / 4);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after_lt = &rest[lt..];
        let Some(gt) = after_lt.find('>') else {
            out.push_str(after_lt);
            return out;
        };
        out.push_str(&rewrite_start_tag(
            &after_lt[..=gt],
            font_family_attr,
            font_size,
        ));
        rest = &after_lt[gt + 1..];
    }
    out.push_str(rest);
    out
}

/// Rewrite one `<...>` tag, adding or extending its `style` attribute. Returns
/// the tag unchanged when it is a close tag, a comment, a doctype, or an element
/// the style block says nothing about.
fn rewrite_start_tag<'a>(tag: &'a str, font_family_attr: &str, font_size: &str) -> Cow<'a, str> {
    let inner = &tag[1..tag.len() - 1];
    if inner.starts_with('/') || inner.starts_with('!') || inner.starts_with('?') {
        return Cow::Borrowed(tag);
    }
    let name_end = inner
        .find(|c: char| c.is_whitespace() || c == '/')
        .unwrap_or(inner.len());
    let name = inner[..name_end].to_ascii_lowercase();
    let Some(decls) = element_declarations(&name, font_family_attr, font_size) else {
        return Cow::Borrowed(tag);
    };
    let attrs = &inner[name_end..];

    // pulldown-cmark emits `style="text-align: right"` on aligned table cells.
    // Prepend into the existing attribute so those declarations stay and, coming
    // later in the block, still win.
    if let Some(value_start) = find_style_value_start(attrs) {
        let mut merged = String::with_capacity(tag.len() + decls.len() + 2);
        merged.push('<');
        merged.push_str(&inner[..name_end + value_start]);
        merged.push_str(&decls);
        merged.push(' ');
        merged.push_str(&inner[name_end + value_start..]);
        merged.push('>');
        return Cow::Owned(merged);
    }
    Cow::Owned(format!(
        "<{name} style=\"{decls}\"{attrs}>",
        name = &inner[..name_end],
        decls = decls,
        attrs = attrs,
    ))
}

/// Byte offset just past the opening quote of a `style="` attribute, if the tag
/// has one. The attribute must start at a tag boundary so `data-style="..."`
/// does not match.
fn find_style_value_start(attrs: &str) -> Option<usize> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("style=\"") {
        let at = from + rel;
        let preceded_by_boundary = attrs[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace());
        if preceded_by_boundary {
            return Some(at + "style=\"".len());
        }
        from = at + "style=\"".len();
    }
    None
}

/// The declarations the `<head><style>` block applies to one element, as they
/// belong in a `style` attribute. `None` means the block says nothing about it.
fn element_declarations(name: &str, font_family_attr: &str, font_size: &str) -> Option<String> {
    let font = format!(
        "font-family: {font_family_attr}; font-size: {font_size}; line-height: 1.6; color: #000;"
    );
    match name {
        "p" => Some(format!("{font} margin: 0 0 1em 0;")),
        "blockquote" => Some(format!(
            "{font} margin: 0.5em 0; padding: 0 0 0 1em; \
             border-left: 2px solid #ccc; white-space: pre-wrap;"
        )),
        "ul" | "ol" | "li" | "dl" | "dt" | "dd" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
        | "table" | "thead" | "tbody" | "tr" | "th" | "td" => Some(font),
        // Keep the monospace family the client picked; only carry the metrics.
        "pre" | "code" => Some(format!("font-size: {font_size}; line-height: 1.6;")),
        "a" => Some("color: #0066cc;".to_string()),
        _ => None,
    }
}

/// The plain-text body of a draft: the Markdown with the signature sentinel
/// comments (#0106) and the quote marker, new or legacy (#0102, PERSO-114),
/// removed, so a recipient reading the `text/plain` alternative sees neither.
/// The blank lines that padded the marker collapse to one paragraph break.
fn plain_text_body(body_markdown: &str) -> String {
    crate::draft::strip_quote_markers(&crate::draft::strip_signature_sentinels(body_markdown))
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
        let html = markdown_to_html(
            "Hello **world**!\n\nSecond paragraph.",
            &default_settings(),
            None,
            None,
        );
        assert_snapshot!(html);
    }

    /// `mp forward --as-attachment` attaches the source as `<name>.eml`, and
    /// the extension is what labels the part `message/rfc822` (PERSO-174).
    #[test]
    fn an_eml_attachment_is_sent_as_message_rfc822() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("2026-10-21_Re DEAL.eml");
        fs::write(&path, b"Subject: x\r\n\r\nbody\r\n").unwrap();
        let (name, _bytes, content_type) = read_attachment(&path).unwrap();
        assert_eq!(name, "2026-10-21_Re DEAL.eml");
        assert_eq!(content_type, "message/rfc822");
    }

    /// A `message/rfc822` part's headers and body as they go on the wire.
    fn rfc822_part(bytes: &[u8]) -> String {
        let part = attachment_part("m.eml".to_string(), bytes.to_vec(), "message/rfc822");
        String::from_utf8(part.formatted()).unwrap()
    }

    /// RFC 2046 §5.2.1: a `message/rfc822` part is `7bit`, `8bit` or `binary`,
    /// never quoted-printable or base64, even with long 8-bit lines lettre
    /// would have re-encoded, and its bytes go unchanged (PERSO-174).
    #[test]
    fn an_eml_with_long_8bit_lines_goes_8bit_byte_for_byte() {
        let long = format!("Grüße, {}", "ä".repeat(300));
        let raw = format!(
            "Subject: Bericht\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{long}\r\n"
        );
        let wire = rfc822_part(raw.as_bytes());
        assert!(wire.contains("Content-Type: message/rfc822"), "{wire}");
        assert!(wire.contains("Content-Transfer-Encoding: 8bit"), "{wire}");
        assert!(
            wire.contains(&raw),
            "the message is carried unchanged: {wire}"
        );
    }

    #[test]
    fn an_ascii_eml_goes_7bit_and_bare_line_feeds_become_crlf() {
        let wire = rfc822_part(b"Subject: x\n\nbody\n");
        assert!(wire.contains("Content-Type: message/rfc822"), "{wire}");
        assert!(wire.contains("Content-Transfer-Encoding: 7bit"), "{wire}");
        assert!(wire.contains("Subject: x\r\n\r\nbody\r\n"), "{wire}");
    }

    /// A line over 998 octets fits neither `7bit` nor `8bit`: the part goes
    /// as `application/octet-stream` under its `.eml` name, encoded.
    #[test]
    fn an_eml_with_a_line_over_998_octets_goes_as_an_octet_stream() {
        let raw = format!("Subject: x\r\n\r\n{}\r\n", "a".repeat(MAX_LINE_OCTETS + 1));
        let wire = rfc822_part(raw.as_bytes());
        assert!(
            wire.contains("Content-Type: application/octet-stream"),
            "{wire}"
        );
        assert!(!wire.contains("message/rfc822"), "{wire}");
        assert!(wire.contains("filename=\"m.eml\""), "{wire}");
        assert!(
            wire.contains("Content-Transfer-Encoding: base64")
                || wire.contains("Content-Transfer-Encoding: quoted-printable"),
            "{wire}"
        );
        let exact = format!("Subject: x\r\n\r\n{}\r\n", "a".repeat(MAX_LINE_OCTETS));
        assert!(rfc822_part(exact.as_bytes()).contains("Content-Transfer-Encoding: 7bit"));
    }

    #[test]
    fn an_eml_with_a_nul_or_a_bare_cr_goes_as_an_octet_stream() {
        for raw in [
            &b"Subject: x\r\n\r\na\0b\r\n"[..],
            b"Subject: x\r\n\r\na\rb\r\n",
        ] {
            let wire = rfc822_part(raw);
            assert!(
                wire.contains("Content-Type: application/octet-stream"),
                "{wire}"
            );
        }
    }

    /// A sent message carrying `eml` as a `message/rfc822` attachment, built
    /// the way [`build_draft_message`] builds one.
    fn message_with_eml(eml: &[u8]) -> Vec<u8> {
        let mixed = MultiPart::mixed()
            .singlepart(SinglePart::plain("see attached".to_string()))
            .singlepart(attachment_part(
                "Bericht.eml".to_string(),
                eml.to_vec(),
                "message/rfc822",
            ));
        Message::builder()
            .from("me@example.com".parse().unwrap())
            .to("a@example.com".parse().unwrap())
            .subject("Fwd: Bericht")
            .message_id(None)
            .multipart(mixed)
            .unwrap()
            .formatted()
    }

    const UMLAUT_EML: &[u8] =
        "Subject: Bericht\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nGrüße\r\n".as_bytes();

    /// The bytes a server without 8BITMIME gets: the `.eml` as base64
    /// `application/octet-stream` under its name, decoding to the same
    /// message, and the rest of the message untouched (PERSO-174).
    #[test]
    fn the_seven_bit_fallback_carries_the_eml_as_a_base64_octet_stream() {
        let raw = message_with_eml(UMLAUT_EML);
        assert!(!raw.is_ascii());
        let fallback = seven_bit_fallback(&raw).expect("an 8bit message/rfc822 part");
        assert!(
            fallback.is_ascii(),
            "{}",
            String::from_utf8_lossy(&fallback)
        );
        let parsed = mailparse::parse_mail(&fallback).unwrap();
        assert_eq!(message_id_of(&fallback), message_id_of(&raw));
        assert_eq!(parsed.subparts.len(), 2);
        assert_eq!(
            parsed.subparts[0].get_body().unwrap().trim(),
            "see attached"
        );
        let eml = &parsed.subparts[1];
        assert_eq!(eml.ctype.mimetype, "application/octet-stream");
        assert_eq!(
            eml.get_content_disposition()
                .params
                .get("filename")
                .map(String::as_str),
            Some("Bericht.eml")
        );
        assert_eq!(eml.get_body_raw().unwrap(), UMLAUT_EML);
    }

    #[test]
    fn an_ascii_message_has_no_seven_bit_fallback() {
        assert!(seven_bit_fallback(&message_with_eml(b"Subject: x\r\n\r\nbody\r\n")).is_none());
    }

    /// What a fake SMTP server saw: every command line, and every message
    /// body it took after DATA.
    #[derive(Default)]
    struct SmtpLog {
        commands: Vec<String>,
        messages: Vec<Vec<u8>>,
    }

    /// A plain-TCP SMTP server on localhost that offers `8BITMIME` in its
    /// EHLO only when told to, accepts every sender and recipient, and logs.
    async fn fake_smtp(eight_bit_mime: bool) -> (u16, std::sync::Arc<std::sync::Mutex<SmtpLog>>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = std::sync::Arc::new(std::sync::Mutex::new(SmtpLog::default()));
        let shared = log.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let log = shared.clone();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut lines = BufReader::new(read);
                    write.write_all(b"220 fake ESMTP\r\n").await.unwrap();
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let command = line.trim_end().to_string();
                        log.lock().unwrap().commands.push(command.clone());
                        let verb = command.split(' ').next().unwrap_or("").to_ascii_uppercase();
                        let reply: &[u8] = match verb.as_str() {
                            "EHLO" if eight_bit_mime => b"250-fake\r\n250 8BITMIME\r\n",
                            "EHLO" | "HELO" => b"250 fake\r\n",
                            "DATA" => {
                                write.write_all(b"354 go ahead\r\n").await.unwrap();
                                let mut message = Vec::new();
                                loop {
                                    let mut data = Vec::new();
                                    lines.read_until(b'\n', &mut data).await.unwrap();
                                    if data == b".\r\n" || data.is_empty() {
                                        break;
                                    }
                                    message.extend_from_slice(&data);
                                }
                                log.lock().unwrap().messages.push(message);
                                b"250 queued\r\n"
                            }
                            "QUIT" => {
                                let _ = write.write_all(b"221 bye\r\n").await;
                                return;
                            }
                            _ => b"250 OK\r\n",
                        };
                        write.write_all(reply).await.unwrap();
                    }
                });
            }
        });
        (port, log)
    }

    fn built_with_eml(eml: &[u8]) -> BuiltMessage {
        let raw = message_with_eml(eml);
        BuiltMessage {
            message_id: message_id_of(&raw),
            raw,
            recipients: vec![
                ("a@example.com".to_string(), RecipientRole::To),
                ("b@example.com".to_string(), RecipientRole::Cc),
            ],
            from: "me@example.com".to_string(),
            draft_key: None,
        }
    }

    async fn submit_to_fake(eight_bit_mime: bool, built: &BuiltMessage) -> (SendResult, SmtpLog) {
        let (port, log) = fake_smtp(eight_bit_mime).await;
        let mailer = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
            .port(port)
            .build();
        let from: lettre::Address = built.from.parse().unwrap();
        let result = submit_through(built, &from, &mailer).await;
        drop(mailer);
        let log = std::mem::take(&mut *log.lock().unwrap());
        (result, log)
    }

    /// lettre refuses 8bit bytes to a server without 8BITMIME before
    /// `MAIL FROM`: the send goes again as 7bit, and each recipient still
    /// sees exactly one `MAIL FROM` and one message (PERSO-174).
    #[tokio::test]
    async fn a_server_without_8bitmime_gets_the_eml_as_7bit_exactly_once() {
        let built = built_with_eml(UMLAUT_EML);
        let (result, log) = submit_to_fake(false, &built).await;
        assert!(
            result.results.iter().all(|r| r.success),
            "{:?}",
            result.results
        );
        let mails = log
            .commands
            .iter()
            .filter(|c| c.to_ascii_uppercase().starts_with("MAIL FROM"))
            .count();
        assert_eq!(mails, 2, "one MAIL FROM per recipient: {:?}", log.commands);
        assert_eq!(log.messages.len(), 2);
        for message in &log.messages {
            assert!(message.is_ascii());
            let text = String::from_utf8_lossy(message);
            assert!(text.contains("application/octet-stream"), "{text}");
            assert!(text.contains("Bericht.eml"), "{text}");
            assert_eq!(message_id_of(message), built.message_id);
        }
    }

    #[tokio::test]
    async fn a_server_with_8bitmime_gets_the_eml_8bit_unchanged() {
        let built = built_with_eml(UMLAUT_EML);
        let (result, log) = submit_to_fake(true, &built).await;
        assert!(result.results.iter().all(|r| r.success));
        assert_eq!(log.messages.len(), 2);
        for message in &log.messages {
            // lettre ends DATA with CRLF . CRLF, adding the CRLF the bytes
            // may already end with.
            assert_eq!(&message[..built.raw.len()], &built.raw[..]);
            assert!(message[built.raw.len()..]
                .iter()
                .all(|b| b"\r\n".contains(b)));
        }
        assert!(log
            .commands
            .iter()
            .any(|c| c.to_ascii_uppercase().contains("BODY=8BITMIME")));
    }

    #[test]
    fn test_resolve_attachment_paths_expands_folder() {
        let dir = tempfile::tempdir().unwrap();
        // Files land out of order and include a dotfile that must be skipped.
        fs::write(dir.path().join("b.pdf"), b"b").unwrap();
        fs::write(dir.path().join("a.pdf"), b"a").unwrap();
        fs::write(dir.path().join(".hidden"), b"x").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();

        let entries = vec![dir.path().to_string_lossy().to_string()];
        let resolved = resolve_attachment_paths(&entries, Path::new("/nowhere")).unwrap();

        let names: Vec<String> = resolved
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        // Sorted, dotfile and subdirectory dropped.
        assert_eq!(names, vec!["a.pdf".to_string(), "b.pdf".to_string()]);
    }

    #[test]
    fn test_resolve_attachment_paths_passes_files_through() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("one.pdf");
        fs::write(&file, b"one").unwrap();
        // A file entry and a non-existent entry are both returned unchanged;
        // the missing one fails later at read time, not here.
        let entries = vec![
            file.to_string_lossy().to_string(),
            "/no/such/file.pdf".to_string(),
        ];
        let resolved = resolve_attachment_paths(&entries, Path::new("/nowhere")).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0], file);
        assert_eq!(resolved[1], PathBuf::from("/no/such/file.pdf"));
    }

    #[test]
    fn test_resolve_attachment_paths_anchors_relative_entries_to_the_draft_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.pdf");
        fs::write(&file, b"%PDF-1.4").unwrap();

        // A relative entry resolves next to the draft, not next to whichever
        // process happens to be running the send: the daemon's cwd is nobody's
        // choice, so the anchor is passed in rather than read from the
        // environment.
        let resolved = resolve_attachment_paths(&["report.pdf".to_string()], dir.path()).unwrap();
        assert_eq!(resolved, vec![file.clone()]);
        assert!(resolved.iter().all(|p| p.is_absolute()));

        // An absolute entry ignores the anchor entirely.
        let absolute =
            resolve_attachment_paths(&[file.display().to_string()], Path::new("/nowhere")).unwrap();
        assert_eq!(absolute, vec![file]);
    }

    #[test]
    fn test_attachment_anchor_is_the_drafts_own_directory() {
        let draft_at = |path: &str| EmailDraft {
            path: PathBuf::from(path),
            frontmatter: serde_yaml::from_str("subject: x\nstatus: draft\n").unwrap(),
            body_markdown: String::new(),
        };
        assert_eq!(
            attachment_anchor(&draft_at("/home/user/mail/alpha/drafts/note.md")),
            Path::new("/home/user/mail/alpha/drafts")
        );
        // A bare file name has no parent; `.` keeps the degenerate case where
        // it was rather than inventing a root.
        assert_eq!(attachment_anchor(&draft_at("note.md")), Path::new("."));
    }

    #[test]
    fn test_markdown_to_html_with_signature_placeholder() {
        // Reply drafts carry the (Markdown) signature spliced into the body at
        // creation time (#0099); the send-time signature arg is None and the
        // legacy `{{SIGNATURE}}` marker is only the quote boundary. The
        // snapshots keep the legacy marker: drafts on disk still carry it, and
        // `quote_marker_renders_like_the_legacy_one` pins the new marker to
        // the same output.
        let md = "My reply\n\n-- Best, Alice\n\n{{SIGNATURE}}\n\n> Original message";
        let html = markdown_to_html(md, &default_settings(), None, None);
        assert_snapshot!(html);
    }

    #[test]
    fn test_markdown_to_html_signature_with_quoted_html() {
        let md = "My reply\n\n-- Best, Alice\n\n{{SIGNATURE}}\n\n> Quoted text";
        let quoted = "<p>Original HTML content</p>";
        let html = markdown_to_html(md, &default_settings(), None, Some(quoted));
        assert_snapshot!(html);
    }

    #[test]
    fn test_markdown_to_html_signature_without_quoted_html() {
        let md = "My reply\n\n-- Best, Alice\n\n{{SIGNATURE}}\n\n> Quoted text";
        let html = markdown_to_html(md, &default_settings(), None, None);
        assert_snapshot!(html);
    }

    /// PERSO-114: the `<!-- mp:quote -->` boundary renders byte for byte like
    /// the legacy `{{SIGNATURE}}` one, with and without the rich-HTML quote, so
    /// the snapshots above pin both.
    #[test]
    fn quote_marker_renders_like_the_legacy_one() {
        let legacy = "My reply\n\n-- Best, Alice\n\n{{SIGNATURE}}\n\n> Quoted text";
        let new = legacy.replace("{{SIGNATURE}}", crate::draft::QUOTE_MARKER);
        for quoted in [None, Some("<p>Original HTML content</p>")] {
            let a = markdown_to_html(legacy, &default_settings(), None, quoted);
            let b = markdown_to_html(&new, &default_settings(), None, quoted);
            assert_eq!(a, b, "quoted html: {quoted:?}");
            assert!(!b.contains("mp:quote"), "marker in HTML: {b}");
        }
    }

    /// The layout a new reply draft is written in: the signature block's
    /// closing sentinel is directly followed by the quote marker, with no
    /// blank line. The marker must still split cleanly between two closed
    /// blocks, with the signature in the reply part and no raw comment left.
    #[test]
    fn a_signature_block_directly_above_the_quote_marker_renders_cleanly() {
        let md = "My reply\n\n<!-- mp:sig-start -->\nBest,  \nAlice\n<!-- mp:sig-end -->\n<!-- mp:quote -->\n\nOn Mon wrote:\n> Quoted";
        let html = markdown_to_html(md, &default_settings(), None, None);
        for leak in ["mp:quote", "mp:sig-start", "mp:sig-end", "{{SIGNATURE}}"] {
            assert!(!html.contains(leak), "{leak} leaked: {html}");
        }
        let alice = html
            .find("Alice</p>")
            .expect("the signature paragraph closes");
        let quote = html
            .find("<div style=\"padding-top:1em\">")
            .expect("the quoted section is wrapped");
        assert!(alice < quote, "signature below the quote: {html}");
        assert!(html[quote..].contains("Quoted"), "quote missing: {html}");
    }

    #[test]
    fn test_markdown_to_html_tables_and_links() {
        let md =
            "| A | B |\n|---|---|\n| 1 | 2 |\n\n[Link](https://example.com)\n\n~~strikethrough~~";
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
            send_hold_secs: 20,
        };
        let html = markdown_to_html("Hello", &settings, None, None);
        assert!(html.contains("Georgia, serif"));
        assert!(html.contains("14px"));
    }

    #[test]
    fn test_markdown_to_html_quoted_font_name_escapes_attribute() {
        let settings = EmailSettings {
            font_family: "\"Times New Roman\", serif".to_string(),
            font_size: "12px".to_string(),
            include_signature: true,
            send_hold_secs: 20,
        };
        let html = markdown_to_html("Hello", &settings, None, None);
        // The inline wrapper's style attribute must not be broken by the raw
        // double quotes in the font name; they are escaped to &quot;.
        assert!(
            html.contains("font-family: &quot;Times New Roman&quot;, serif;"),
            "attribute not escaped: {html}"
        );
        // The raw form survives in the <style> block (not an attribute context).
        assert!(html.contains("font-family: \"Times New Roman\", serif;"));
    }

    // -----------------------------------------------------------------------
    // SendResult methods
    // -----------------------------------------------------------------------

    fn make_result(successes: &[&str], failures: &[&str]) -> SendResult {
        let mut results = Vec::new();
        for addr in successes {
            results.push(RecipientResult {
                address: addr.to_string(),
                role: RecipientRole::To,
                success: true,
                error: None,
                verdict: RecipientVerdict::Delivered,
            });
        }
        for addr in failures {
            results.push(RecipientResult {
                address: addr.to_string(),
                role: RecipientRole::To,
                success: false,
                error: Some("SMTP error".to_string()),
                verdict: RecipientVerdict::Rejected,
            });
        }
        SendResult { results }
    }

    #[test]
    fn test_send_result_all_succeeded() {
        let r = make_result(&["a@example.com", "b@example.com"], &[]);
        assert!(r.all_succeeded());
        assert!(r.any_succeeded());
        assert_eq!(r.succeeded().len(), 2);
        assert!(r.failed().is_empty());
    }

    #[test]
    fn test_send_result_partial_failure() {
        let r = make_result(&["a@example.com"], &["b@example.com"]);
        assert!(!r.all_succeeded());
        assert!(r.any_succeeded());
        assert_eq!(r.succeeded().len(), 1);
        assert_eq!(r.failed().len(), 1);
    }

    #[test]
    fn test_send_result_all_failed() {
        let r = make_result(&[], &["a@example.com", "b@example.com"]);
        assert!(!r.all_succeeded());
        assert!(!r.any_succeeded());
        assert!(r.succeeded().is_empty());
        assert_eq!(r.failed().len(), 2);
    }

    #[test]
    fn test_send_result_empty() {
        let r = SendResult { results: vec![] };
        assert!(r.all_succeeded()); // vacuously true
        assert!(!r.any_succeeded());
    }

    // -----------------------------------------------------------------------
    // Per-recipient verdicts and the admission gate (#0063)
    // -----------------------------------------------------------------------

    fn recipient(address: &str, verdict: RecipientVerdict) -> RecipientResult {
        RecipientResult {
            address: address.to_string(),
            role: RecipientRole::To,
            success: verdict == RecipientVerdict::Delivered,
            error: (verdict != RecipientVerdict::Delivered).then(|| "why".to_string()),
            verdict,
        }
    }

    /// The outbox is handed one verdict per recipient, in the four buckets it
    /// acts on; nothing collapses a mixed result into "accepted".
    #[test]
    fn submit_outcome_hands_the_outbox_one_verdict_per_recipient() {
        let result = SendResult {
            results: vec![
                recipient("took-it@x.com", RecipientVerdict::Delivered),
                recipient("refused@x.com", RecipientVerdict::Rejected),
                recipient("later@x.com", RecipientVerdict::Retryable),
                recipient("silent@x.com", RecipientVerdict::Unknown),
            ],
        };
        let crate::outbox::SubmitOutcome::PerRecipient(verdicts) = result.submit_outcome() else {
            panic!("an SMTP result is always per recipient");
        };
        assert_eq!(verdicts.delivered, vec!["took-it@x.com".to_string()]);
        assert_eq!(verdicts.rejected[0].0, "refused@x.com");
        assert_eq!(verdicts.retryable[0].0, "later@x.com");
        assert_eq!(verdicts.ambiguous[0].0, "silent@x.com");
        assert!(result.results[3].ambiguous());
    }

    /// The status line of a send that reached some recipients says so, rather
    /// than reporting the outbox row's state as an unqualified success.
    #[test]
    fn a_partial_send_says_so_in_its_status_line() {
        let report = SendReport {
            send_result: SendResult {
                results: vec![
                    recipient("took-it@x.com", RecipientVerdict::Delivered),
                    recipient("refused@x.com", RecipientVerdict::Rejected),
                ],
            },
            state: Some(crate::outbox::OutboxState::Done),
            row_id: Some(1),
        };
        assert_eq!(
            report.status_line(),
            "partly delivered, see `mp outbox list`"
        );
    }

    pub(super) fn draft_with(id: Option<&str>, path: &str) -> EmailDraft {
        EmailDraft {
            path: std::path::PathBuf::from(path),
            frontmatter: crate::types::EmailFrontmatter {
                id: id.map(|s| s.to_string()),
                date: None,
                to: Some("bob@example.com".to_string()),
                cc: None,
                bcc: None,
                subject: "Hello".to_string(),
                status: EmailStatus::Approved,
                from: None,
                reply_to: None,
                attachments: None,
                sent_at: None,
                sent_via: None,
                message_id: None,
                in_reply_to: None,
                forwarded_from: None,
                signature: None,
                event: None,
            },
            body_markdown: String::new(),
        }
    }

    /// The second status axis is written from the draft that caused it: a
    /// reply names its source in `in_reply_to:`, a forward in
    /// `forwarded_from:`, and an ordinary draft names nothing (#TKT-0051).
    #[test]
    fn only_a_reply_or_a_forward_has_a_source_to_flag() {
        let mut reply = draft_with(None, "/drafts/r.md");
        reply.frontmatter.in_reply_to = Some("<src@example.com>".to_string());
        assert_eq!(source_to_flag(&reply), Some(("<src@example.com>", true)));

        let mut forward = draft_with(None, "/drafts/f.md");
        forward.frontmatter.forwarded_from = Some(" <src@example.com> ".to_string());
        assert_eq!(source_to_flag(&forward), Some(("<src@example.com>", false)));

        assert_eq!(source_to_flag(&draft_with(None, "/drafts/plain.md")), None);

        let mut blank = draft_with(None, "/drafts/blank.md");
        blank.frontmatter.in_reply_to = Some("   ".to_string());
        assert_eq!(source_to_flag(&blank), None);
    }

    /// One addressee is a reply, more than one across To and Cc a reply to
    /// all: the verb a Graph account records on the source (PERSO-171).
    #[test]
    fn a_reply_to_several_people_reads_as_a_reply_to_all() {
        let mut reply = draft_with(None, "/drafts/r.md");
        assert!(!answers_all(&reply), "one To, no Cc");
        reply.frontmatter.cc = Some("carol@example.com".to_string());
        assert!(answers_all(&reply), "a Cc beside the To");
        reply.frontmatter.cc = None;
        reply.frontmatter.to = Some("Bob <bob@example.com>, dave@example.com".to_string());
        assert!(answers_all(&reply), "two To");
    }

    /// The `In-Reply-To` and `References` values a built message carries,
    /// unfolded the way a reading client sees them.
    fn threading_of(raw: &[u8]) -> (Option<String>, Option<String>) {
        use mailparse::MailHeaderMap;
        let parsed = mailparse::parse_mail(raw).expect("the built message parses");
        (
            parsed.headers.get_first_value("In-Reply-To"),
            parsed.headers.get_first_value("References"),
        )
    }

    pub(super) fn build_with_ancestry(draft: &EmailDraft, ancestry: Option<&str>) -> BuiltMessage {
        build_draft_message(
            draft,
            "me@example.com",
            &EmailSettings::default(),
            None,
            None,
            ancestry,
        )
        .expect("the draft builds")
    }

    /// A reply names its parent in `In-Reply-To` and appends it to the
    /// parent's own `References`, each id once (PERSO-137, RFC 5322 3.6.4).
    #[test]
    fn a_reply_carries_in_reply_to_and_the_parents_references() {
        let mut reply = draft_with(None, "/drafts/r.md");
        reply.frontmatter.in_reply_to = Some("  <parent@example.com> ".to_string());

        let built = build_with_ancestry(&reply, Some("<root@example.com> <mid@example.com>"));
        assert_eq!(
            threading_of(&built.raw),
            (
                Some("<parent@example.com>".to_string()),
                Some("<root@example.com> <mid@example.com> <parent@example.com>".to_string()),
            )
        );

        // A repeated ancestor, and the parent already listed in its own
        // ancestry, are written once, the parent last.
        let built = build_with_ancestry(
            &reply,
            Some("<root@example.com> <parent@example.com>\r\n <root@example.com>"),
        );
        assert_eq!(
            threading_of(&built.raw).1.as_deref(),
            Some("<root@example.com> <parent@example.com>")
        );

        // A bare id from a hand-edited draft is bracketed.
        reply.frontmatter.in_reply_to = Some("parent@example.com".to_string());
        let built = build_with_ancestry(&reply, None);
        assert_eq!(
            threading_of(&built.raw).0.as_deref(),
            Some("<parent@example.com>")
        );
    }

    /// A parent the store does not hold leaves `References` as the parent
    /// alone, which still threads the reply (PERSO-137).
    #[test]
    fn a_reply_to_an_unknown_parent_references_the_parent_alone() {
        let mut reply = draft_with(None, "/drafts/r.md");
        reply.frontmatter.in_reply_to = Some("<parent@example.com>".to_string());
        let built = build_with_ancestry(&reply, None);
        assert_eq!(
            threading_of(&built.raw),
            (
                Some("<parent@example.com>".to_string()),
                Some("<parent@example.com>".to_string()),
            )
        );
    }

    /// A forward, a blank `in_reply_to:` and a fresh draft carry neither
    /// header, whatever ancestry the caller hands in (PERSO-137).
    #[test]
    fn a_forward_or_a_blank_reply_id_carries_no_threading_headers() {
        let mut forward = draft_with(None, "/drafts/f.md");
        forward.frontmatter.forwarded_from = Some("<src@example.com>".to_string());
        let mut blank = draft_with(None, "/drafts/b.md");
        blank.frontmatter.in_reply_to = Some("   ".to_string());
        let mut blank_brackets = draft_with(None, "/drafts/bb.md");
        blank_brackets.frontmatter.in_reply_to = Some(" <> ".to_string());
        let fresh = draft_with(None, "/drafts/n.md");
        for draft in [&forward, &blank, &blank_brackets, &fresh] {
            let built = build_with_ancestry(draft, Some("<root@example.com>"));
            assert_eq!(
                threading_of(&built.raw),
                (None, None),
                "{}",
                draft.path.display()
            );
        }
    }

    /// The parent's ancestry is its `References`, else a single-id
    /// `In-Reply-To`, else nothing (RFC 5322 3.6.4).
    #[test]
    fn the_parents_ancestry_falls_back_to_a_single_in_reply_to() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(
            parent_ancestry(s("<a@x>"), s("<r@x> <a@x>")),
            s("<r@x> <a@x>")
        );
        assert_eq!(parent_ancestry(s("<a@x>"), None), s("<a@x>"));
        assert_eq!(parent_ancestry(s("<a@x>"), s("   ")), s("<a@x>"));
        assert_eq!(parent_ancestry(s("<a@x> <b@x>"), None), None);
        assert_eq!(parent_ancestry(None, None), None);
    }

    /// End to end through the store: the reply is built from the ancestry the
    /// store holds for its parent, and ingesting the built bytes (what the
    /// outbox APPENDs and files locally as the Sent copy) lands the reply in
    /// the parent's thread instead of a thread of its own (PERSO-137).
    #[test]
    fn an_ingested_reply_joins_its_parents_thread() {
        use crate::ingest::{ingest_message, IngestInput};
        use crate::parse::parse_rfc822_to_fetched_email;
        use crate::store::{BlobStore, Store};

        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(tmp.path().join("blobs"));
        let ingest = |mailbox: &str, uid: i64, raw: &[u8]| {
            let email = parse_rfc822_to_fetched_email(raw).expect("the message parses");
            ingest_message(
                &store,
                &blobs,
                &IngestInput {
                    account: "acct",
                    mailbox,
                    uid,
                    email: &email,
                    raw: Some(raw),
                },
            )
            .unwrap()
        };

        let root = ingest(
            "inbox",
            1,
            b"From: a@example.com\r\nTo: me@example.com\r\nSubject: Plan\r\n\
              Message-ID: <root@example.com>\r\n\r\nroot\r\n",
        );
        let parent = ingest(
            "inbox",
            2,
            b"From: b@example.com\r\nTo: me@example.com\r\nSubject: Re: Plan\r\n\
              Message-ID: <parent@example.com>\r\nIn-Reply-To: <root@example.com>\r\n\
              References: <root@example.com>\r\n\r\nparent\r\n",
        );
        assert_eq!(parent.thread_id, root.message_id);

        let (irt, refs) =
            crate::store::read::threading_headers_of(&store, "acct", "<parent@example.com>")
                .unwrap()
                .expect("the parent is in the store");
        let ancestry = parent_ancestry(irt, refs);
        assert_eq!(ancestry.as_deref(), Some("<root@example.com>"));
        assert_eq!(
            crate::store::read::threading_headers_of(&store, "acct", "<absent@example.com>")
                .unwrap(),
            None
        );

        let mut reply = draft_with(None, "/drafts/r.md");
        reply.frontmatter.subject = "Re: Plan".to_string();
        reply.frontmatter.in_reply_to = Some("<parent@example.com>".to_string());
        let built = build_with_ancestry(&reply, ancestry.as_deref());
        assert_eq!(
            threading_of(&built.raw).1.as_deref(),
            Some("<root@example.com> <parent@example.com>")
        );

        let sent = ingest("sent", 1, &built.raw);
        assert_eq!(sent.message_id, built.message_id);
        assert_eq!(
            sent.thread_id, root.message_id,
            "the Sent copy joins the conversation it answers"
        );
    }

    /// The message a Graph MIME send submits, decoded back from the request
    /// body [`graph_submit::plan`] produced.
    fn graph_request_mime(built: &BuiltMessage) -> Vec<u8> {
        use base64::Engine;
        match graph_submit::plan(built).expect("the route is decided") {
            graph_submit::GraphPlan::Mime(body) => base64::engine::general_purpose::STANDARD
                .decode(body)
                .expect("the request body is standard base64"),
            other => panic!("a small message goes as one MIME sendMail, not {other:?}"),
        }
    }

    /// A Graph reply goes as the same MIME the SMTP path builds: threading
    /// headers, our `Message-ID`, the account's From, the plain/html
    /// alternative, and the Bcc recipients as a header, since Graph has no
    /// envelope to carry them (PERSO-144).
    #[test]
    fn a_graph_reply_is_sent_as_mime_with_threading_and_bcc() {
        use mailparse::MailHeaderMap;
        let mut reply = draft_with(None, "/drafts/r.md");
        reply.frontmatter.subject = "Re: Plan".to_string();
        reply.frontmatter.in_reply_to = Some("<parent@example.com>".to_string());
        reply.frontmatter.cc = Some("carol@example.com".to_string());
        reply.frontmatter.bcc = Some("Dan Hidden <dan@example.com>, eve@example.com".to_string());
        reply.body_markdown = "Sounds **good**.".to_string();
        let built = build_with_ancestry(&reply, Some("<root@example.com>"));

        let mime = graph_request_mime(&built);
        let parsed = mailparse::parse_mail(&mime).expect("the Graph MIME parses");
        let header = |name: &str| parsed.headers.get_first_value(name);
        assert_eq!(header("Message-ID"), Some(built.message_id.clone()));
        assert_eq!(
            header("In-Reply-To").as_deref(),
            Some("<parent@example.com>")
        );
        assert_eq!(
            header("References").as_deref(),
            Some("<root@example.com> <parent@example.com>")
        );
        assert_eq!(header("From").as_deref(), Some("me@example.com"));
        assert_eq!(header("To").as_deref(), Some("bob@example.com"));
        assert_eq!(header("Cc").as_deref(), Some("carol@example.com"));
        assert_eq!(header("Subject").as_deref(), Some("Re: Plan"));
        let bcc = mailparse::addrparse_header(
            parsed
                .headers
                .get_first_header("Bcc")
                .expect("the Graph MIME names the Bcc recipients"),
        )
        .expect("the Bcc header parses");
        assert_eq!(
            bcc.to_string(),
            "\"Dan Hidden\" <dan@example.com>, eve@example.com"
        );
        assert_eq!(
            parsed.headers.get_all_values("Bcc").len(),
            1,
            "one Bcc header"
        );

        assert_eq!(parsed.ctype.mimetype, "multipart/alternative");
        let types: Vec<&str> = parsed
            .subparts
            .iter()
            .map(|p| p.ctype.mimetype.as_str())
            .collect();
        assert_eq!(types, ["text/plain", "text/html"]);
        let html = parsed.subparts[1].get_body().unwrap();
        assert!(html.contains("<strong>good</strong>"), "{html}");
    }

    /// The Bcc header is added for Graph alone: the built bytes, which go to
    /// SMTP, the outbox and the local sent copy, still carry none, and the
    /// Graph MIME is those bytes with the one header in front (PERSO-144).
    #[test]
    fn the_bcc_header_is_added_for_graph_only() {
        use mailparse::MailHeaderMap;
        let mut draft = draft_with(None, "/drafts/b.md");
        draft.frontmatter.bcc = Some("dan@example.com".to_string());
        let built = build_with_ancestry(&draft, None);

        let (smtp_headers, _) = mailparse::parse_headers(&built.raw).unwrap();
        assert!(
            smtp_headers.get_first_value("Bcc").is_none(),
            "lettre keeps Bcc off the SMTP bytes"
        );
        assert!(built
            .recipients
            .contains(&("dan@example.com".to_string(), RecipientRole::Bcc)));

        let mime = graph_mime(&built).unwrap();
        assert_eq!(
            &mime[..mime.len() - built.raw.len()],
            b"Bcc: dan@example.com\r\n"
        );
        assert_eq!(&mime[mime.len() - built.raw.len()..], &built.raw[..]);

        // No Bcc recipient, no header: the Graph MIME is the built bytes.
        let plain = build_with_ancestry(&draft_with(None, "/drafts/p.md"), None);
        assert_eq!(graph_mime(&plain).unwrap(), plain.raw);
    }

    /// Attachments travel inside the Graph MIME exactly as the SMTP builder
    /// lays them out (PERSO-144).
    #[test]
    fn a_graph_mime_send_carries_the_attachments() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.txt"), b"attached text").unwrap();
        let mut draft = draft_with(None, tmp.path().join("d.md").to_str().unwrap());
        draft.frontmatter.attachments = Some(vec!["notes.txt".to_string()]);
        let built = build_with_ancestry(&draft, None);

        let mime = graph_request_mime(&built);
        let parsed = mailparse::parse_mail(&mime).unwrap();
        assert_eq!(parsed.ctype.mimetype, "multipart/mixed");
        assert_eq!(parsed.subparts[0].ctype.mimetype, "multipart/alternative");
        let attachment = &parsed.subparts[1];
        assert_eq!(
            attachment.get_content_disposition().params.get("filename"),
            Some(&"notes.txt".to_string())
        );
        assert_eq!(attachment.get_body().unwrap().trim_end(), "attached text");
    }

    /// A reply too large for one sendMail goes as a draft made from its own
    /// MIME, so it keeps `In-Reply-To`, `References`, our `Message-ID` and the
    /// Bcc header; only the attachment that does not fit is taken out, to be
    /// added to the draft on its own, and a small one stays inside (PERSO-144
    /// follow-up). Before, the whole message fell back to the JSON resource
    /// and landed outside its conversation in Outlook.
    #[test]
    fn a_large_reply_goes_as_a_mime_draft_that_keeps_its_threading() {
        use base64::Engine;
        use mailparse::MailHeaderMap;
        let tmp = tempfile::tempdir().unwrap();
        let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(tmp.path().join("plans.pdf"), &big).unwrap();
        std::fs::write(tmp.path().join("notes.txt"), b"attached text").unwrap();
        let mut reply = draft_with(None, tmp.path().join("r.md").to_str().unwrap());
        reply.frontmatter.subject = "Re: Plan".to_string();
        reply.frontmatter.in_reply_to = Some("<parent@example.com>".to_string());
        reply.frontmatter.bcc = Some("dan@example.com".to_string());
        reply.frontmatter.attachments =
            Some(vec!["notes.txt".to_string(), "plans.pdf".to_string()]);
        let built = build_with_ancestry(&reply, Some("<root@example.com>"));
        assert!(
            crate::graph::mime_send_body(&graph_mime(&built).unwrap()).len()
                > crate::graph::MIME_SEND_MAX_BASE64_BYTES
        );

        let graph_submit::GraphPlan::Draft {
            body: graph_submit::DraftBody::Mime(body),
            attachments,
        } = graph_submit::plan(&built).unwrap()
        else {
            panic!("a large reply goes as a MIME draft");
        };
        assert!(crate::graph::fits_mime_send(body.len()));
        let mime = base64::engine::general_purpose::STANDARD
            .decode(body)
            .unwrap();
        let parsed = mailparse::parse_mail(&mime).expect("the draft MIME parses");
        let header = |name: &str| parsed.headers.get_first_value(name);
        assert_eq!(header("Message-ID"), Some(built.message_id.clone()));
        assert_eq!(
            header("In-Reply-To").as_deref(),
            Some("<parent@example.com>")
        );
        assert_eq!(
            header("References").as_deref(),
            Some("<root@example.com> <parent@example.com>")
        );
        assert_eq!(header("Bcc").as_deref(), Some("dan@example.com"));

        assert_eq!(parsed.ctype.mimetype, "multipart/mixed");
        let kept: Vec<String> = parsed
            .subparts
            .iter()
            .map(|p| p.ctype.mimetype.clone())
            .collect();
        assert_eq!(kept, ["multipart/alternative", "text/plain"]);
        assert_eq!(
            parsed.subparts[1].get_body().unwrap().trim_end(),
            "attached text",
            "the small attachment stays inside the MIME"
        );

        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].name, "plans.pdf");
        assert_eq!(attachments[0].content_type, "application/pdf");
        assert!(
            attachments[0].bytes == big,
            "the moved file is byte-identical"
        );
    }

    /// A body too large for any MIME request still goes, as the JSON resource
    /// staged through a draft: the one case that cannot carry threading.
    #[test]
    fn a_body_too_large_for_mime_goes_as_a_json_draft() {
        let mut big = draft_with(None, "/drafts/big.md");
        big.frontmatter.in_reply_to = Some("<parent@example.com>".to_string());
        big.frontmatter.cc = Some("Carol <carol@example.com>".to_string());
        big.body_markdown = "word ".repeat(600_000);
        let built = build_with_ancestry(&big, None);

        let graph_submit::GraphPlan::Draft {
            body: graph_submit::DraftBody::Json(message),
            attachments,
        } = graph_submit::plan(&built).unwrap()
        else {
            panic!("a body over the MIME limit goes as JSON");
        };
        assert!(attachments.is_empty());
        assert_eq!(message["subject"], "Hello");
        assert_eq!(message["body"]["contentType"], "HTML");
        assert!(message["body"]["content"]
            .as_str()
            .unwrap()
            .contains("word word"));
        assert_eq!(
            message["toRecipients"][0]["emailAddress"]["address"],
            "bob@example.com"
        );
        assert_eq!(message["ccRecipients"][0]["emailAddress"]["name"], "Carol");
    }

    /// The local half of the post-send hook flags *every* copy of the source,
    /// because one message is one row per mailbox and the archived copy is as
    /// likely to be the one on screen (#TKT-0051).
    #[test]
    fn sending_a_reply_flags_every_local_copy_of_its_source() {
        let fx = crate::reconcile::tests::fixture();
        let inbox = fx.ingest_plain("inbox", 7, "Question");
        // The same Message-ID filed in a second mailbox: `ingest_plain` keys
        // its id on `(mailbox, uid)`, so the archive copy is ingested under the
        // inbox row's identity by hand.
        fx.store
            .conn()
            .execute(
                "INSERT INTO messages (account, mailbox, uid, message_id, subject)
                 SELECT account, 'archive', 99, message_id, subject FROM messages WHERE id = ?1",
                [inbox],
            )
            .unwrap();

        let message_id = crate::store::read::find_by_id(&fx.store, inbox)
            .unwrap()
            .unwrap()
            .message_id;
        let rows = crate::store::read::find_by_message_id(&fx.store, "alice", &message_id).unwrap();
        let mailboxes = server_mailboxes_of(&crate::config::AccountConfig::default(), &rows);
        let outcome = crate::pending_ops::apply_post_send_flag(
            &fx.store,
            "alice",
            &message_id,
            true,
            false,
            &mailboxes,
        )
        .unwrap();

        assert_eq!(outcome.rows, 2, "both copies are found by Message-ID");
        for row in crate::store::read::find_by_message_id(&fx.store, "alice", &message_id).unwrap()
        {
            assert!(row.is_answered(), "{} was not flagged", row.mailbox);
            assert!(!row.is_forwarded());
        }
        // Two mailboxes, one queued op: the whole point of #0076 is that the
        // server half is one multi-mailbox op rather than one session each.
        let queued = crate::pending_ops::queued_ops(&fx.store, "alice").unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].kind, "set_answered");
        match &queued[0].op {
            crate::ops::ServerOp::SetAnswered { mailboxes, .. } => {
                assert_eq!(mailboxes.len(), 2, "both server folders ride one op");
            }
            other => panic!("unexpected op {other:?}"),
        }
    }

    /// The distinct server folders are what ride the op: two local rows in the
    /// same server folder are one `SELECT`, not two.
    #[test]
    fn the_server_mailbox_list_is_deduplicated() {
        let fx = crate::reconcile::tests::fixture();
        let a = fx.ingest_plain("inbox", 1, "One");
        let b = fx.ingest_plain("inbox", 2, "Two");
        let rows: Vec<_> = [a, b]
            .into_iter()
            .map(|id| {
                crate::store::read::find_by_id(&fx.store, id)
                    .unwrap()
                    .unwrap()
            })
            .collect();
        let mailboxes = server_mailboxes_of(&crate::config::AccountConfig::default(), &rows);
        assert_eq!(mailboxes.len(), 1, "one server folder, one SELECT");
    }

    /// A forward sets the other bit, and neither write disturbs the read bit
    /// the row already carried.
    #[test]
    fn forwarding_sets_the_forwarded_bit_and_leaves_the_read_bit_alone() {
        let fx = crate::reconcile::tests::fixture();
        let id = fx.ingest_plain("inbox", 3, "Passing this on");
        crate::store::write::set_read(&fx.store, id, true).unwrap();

        let message_id = crate::store::read::find_by_id(&fx.store, id)
            .unwrap()
            .unwrap()
            .message_id;
        crate::pending_ops::apply_post_send_flag(
            &fx.store,
            "alice",
            &message_id,
            false,
            false,
            &[],
        )
        .unwrap();

        let row = crate::store::read::find_by_id(&fx.store, id)
            .unwrap()
            .unwrap();
        assert!(row.is_forwarded());
        assert!(!row.is_answered());
        assert!(row.is_read(), "the read bit survives a history write");
        assert!(
            crate::pending_ops::queued_ops(&fx.store, "alice")
                .unwrap()
                .is_empty(),
            "no server mailboxes (no server config) queues nothing"
        );
    }

    /// A source the store does not hold flags nothing and says so, which is
    /// what keeps the hook silent for a reply to a message that has since been
    /// deleted.
    #[test]
    fn a_source_the_store_does_not_hold_flags_nothing() {
        let fx = crate::reconcile::tests::fixture();
        let outcome = crate::pending_ops::apply_post_send_flag(
            &fx.store,
            "alice",
            "<gone@example.com>",
            true,
            false,
            &["INBOX".to_string()],
        )
        .unwrap();
        assert_eq!(outcome.rows, 0);
        assert_eq!(outcome.op_id, None, "nothing local means nothing owed");
        assert!(crate::pending_ops::queued_ops(&fx.store, "alice")
            .unwrap()
            .is_empty());
    }

    /// The key is the frontmatter id when there is one, because that is what
    /// survives the rename a send performs.
    #[test]
    fn a_draft_is_keyed_by_its_id_and_falls_back_to_its_path() {
        let with_id = draft_with(Some("2026-08-06-note"), "/drafts/note.md");
        assert_eq!(draft_key(&with_id), "id:2026-08-06-note");
        let renamed = draft_with(Some("2026-08-06-note"), "/drafts/renamed.md");
        assert_eq!(draft_key(&renamed), draft_key(&with_id));
        let anonymous = draft_with(None, "/drafts/agent-wrote-this.md");
        assert_eq!(draft_key(&anonymous), "path:/drafts/agent-wrote-this.md");
    }

    /// The cheap half of the admission gate: two threads reaching `send_draft`
    /// for one draft, which is what the TUI does when the cursor draft is also
    /// in the approved batch.
    #[test]
    fn one_draft_admits_one_send_at_a_time() {
        let key = "id:only-once";
        let first = SendAdmission::claim("work", key).expect("the first send is admitted");
        assert!(
            SendAdmission::claim("work", key).is_none(),
            "a second send of the same draft is refused while the first runs"
        );
        assert!(
            SendAdmission::claim("work", "id:another").is_some(),
            "a different draft is not blocked"
        );
        assert!(
            SendAdmission::claim("personal", key).is_some(),
            "the same id on another account is another message, not a duplicate"
        );
        drop(first);
        assert!(
            SendAdmission::claim("work", key).is_some(),
            "the slot is released however the send returned"
        );
    }

    /// The `from:` that reaches SMTP is the one the build validated. An
    /// unquoted comma in the display name parses nowhere, so a draft carrying
    /// one used to enqueue and then fail every submission it would ever get
    /// (#0063 review).
    #[test]
    fn a_from_with_a_comma_in_the_display_name_survives_the_build() {
        let mut draft = draft_with(Some("comma-from"), "/drafts/comma.md");
        draft.frontmatter.from = Some("Doe, Jane <jane@example.com>".to_string());
        let built = build_draft_message(
            &draft,
            "fallback@example.com",
            &EmailSettings::default(),
            None,
            None,
            None,
        )
        .expect("the build accepts a display name it can quote");
        assert_eq!(built.from, "\"Doe, Jane\" <jane@example.com>");
        // What `submit` does with it before it opens a connection.
        let mailbox: Mailbox = built
            .from
            .parse()
            .expect("the stored from address parses on the submission path");
        assert_eq!(mailbox.email.to_string(), "jane@example.com");
    }

    /// The same trap on the RSVP path: an account whose address carries a
    /// display name with a comma builds a reply whose `from:` must still parse
    /// where `submit` derives `MAIL FROM` from it (#0063 review, second half).
    #[test]
    fn an_rsvp_reply_carries_the_from_it_validated() {
        let built = build_reply_message(
            "Doe, Jane <jane@example.com>",
            "organizer@example.com",
            "Accepted: Planning",
            "Accepted.",
            "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n",
        )
        .expect("the build accepts a display name it can quote");
        assert_eq!(built.from, "\"Doe, Jane\" <jane@example.com>");
        let mailbox: Mailbox = built
            .from
            .parse()
            .expect("the stored from address parses on the submission path");
        assert_eq!(mailbox.email.to_string(), "jane@example.com");
    }

    // -----------------------------------------------------------------------
    // plain_text_body: quote marker (#0102, PERSO-114)
    // -----------------------------------------------------------------------

    /// The marker is consumed by the HTML path and dropped from the TUI
    /// preview, but the `text/plain` alternative used to ship it verbatim. It
    /// must leave no trace, new or legacy, and the blank lines it padded
    /// collapse to a single paragraph break rather than a double one.
    #[test]
    fn plain_text_body_drops_the_quote_marker_and_its_padding() {
        for marker in [
            crate::draft::QUOTE_MARKER,
            crate::draft::LEGACY_QUOTE_MARKER,
        ] {
            let body =
                format!("My reply\n\n-- \nBest, Alice\n\n{marker}\n\nOn Mon wrote:\n> Quoted");
            let plain = plain_text_body(&body);
            assert!(!plain.contains(marker), "marker survived: {plain:?}");
            // The signature Markdown (spliced in at draft time, #0099) stays;
            // only the marker and its padding go.
            assert_eq!(
                plain,
                "My reply\n\n-- \nBest, Alice\n\nOn Mon wrote:\n> Quoted"
            );
            assert!(
                !plain.contains("\n\n\n"),
                "double blank line left behind: {plain:?}"
            );
        }
    }

    /// A new draft carries both the sentinel-wrapped signature block and the
    /// quote marker right below it: the plain part keeps the signature text
    /// and drops all three comment lines.
    #[test]
    fn plain_text_body_handles_a_signature_block_and_the_quote_marker() {
        let body = "My reply\n\n\n<!-- mp:sig-start -->\nBest,  \nAlice\n<!-- mp:sig-end -->\n<!-- mp:quote -->\n\nOn Mon wrote:\n> Quoted\n";
        assert_eq!(
            plain_text_body(body),
            "My reply\n\n\nBest,  \nAlice\n\nOn Mon wrote:\n> Quoted\n"
        );
    }

    /// The signature sentinels (#0106) never reach a recipient: the plain part
    /// drops the marker lines and keeps the signature content, and the HTML
    /// part carries no raw `<!-- mp:sig-... -->` comment.
    #[test]
    fn signature_sentinels_are_stripped_from_both_send_parts() {
        let body = "My note\n\n<!-- mp:sig-start -->\nBest,\nAlice\n<!-- mp:sig-end -->\n";
        let plain = plain_text_body(body);
        assert!(
            !plain.contains("mp:sig-start"),
            "sentinel in plain: {plain:?}"
        );
        assert!(
            !plain.contains("mp:sig-end"),
            "sentinel in plain: {plain:?}"
        );
        assert!(
            plain.contains("Best,"),
            "signature dropped from plain: {plain:?}"
        );
        assert!(
            plain.contains("Alice"),
            "signature dropped from plain: {plain:?}"
        );

        let html = markdown_to_html(body, &default_settings(), None, None);
        assert!(!html.contains("mp:sig-start"), "sentinel in html: {html}");
        assert!(!html.contains("mp:sig-end"), "sentinel in html: {html}");
        // The signature content still renders, on its own line (hard break).
        assert!(
            html.contains("Alice"),
            "signature dropped from html: {html}"
        );
    }

    /// A body with no marker is returned untouched, so the non-reply send
    /// paths keep byte-for-byte identical plain text.
    #[test]
    fn plain_text_body_is_a_no_op_without_the_marker() {
        let body = "Just a plain note.\n\nSecond paragraph.\n";
        assert_eq!(plain_text_body(body), body);
    }

    /// The decoded bodies of every `text/plain` and `text/html` leaf part of a
    /// built message, so an assertion sees the text after transfer decoding
    /// (quoted-printable can wrap a marker across a soft line break).
    fn text_parts(raw: &[u8]) -> (Vec<String>, Vec<String>) {
        fn walk(part: &mailparse::ParsedMail, plain: &mut Vec<String>, html: &mut Vec<String>) {
            if part.subparts.is_empty() {
                let body = part.get_body().expect("the part body decodes");
                match part.ctype.mimetype.as_str() {
                    "text/plain" => plain.push(body),
                    "text/html" => html.push(body),
                    _ => {}
                }
            }
            for sub in &part.subparts {
                walk(sub, plain, html);
            }
        }
        let parsed = mailparse::parse_mail(raw).expect("the built message parses");
        let (mut plain, mut html) = (Vec::new(), Vec::new());
        walk(&parsed, &mut plain, &mut html);
        (plain, html)
    }

    /// The reply/forward parts built by `build_draft_message` carry no quote
    /// marker, new (`<!-- mp:quote -->`) or legacy (`{{SIGNATURE}}`), in either
    /// the `text/plain` or the `text/html` part, on the SMTP body and on the
    /// invite path, while the reply text, the signature and the quoted original
    /// still ride along. The draft has the layout `create_reply_draft_from`
    /// writes: a sentinel-wrapped signature block right above the marker.
    #[test]
    fn a_sent_reply_carries_no_quote_marker_in_either_part() {
        for marker in [
            crate::draft::QUOTE_MARKER,
            crate::draft::LEGACY_QUOTE_MARKER,
        ] {
            let mut draft = draft_with(Some("reply-marker"), "/drafts/reply.md");
            draft.body_markdown = format!(
                "My reply\n\n\n<!-- mp:sig-start -->\n-- \nBest, Alice\n<!-- mp:sig-end -->\n{marker}\n\nOn Mon, Alice wrote:\n> Original text\n"
            );
            for invite in [None, Some("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")] {
                let built = build_draft_message(
                    &draft,
                    "fallback@example.com",
                    &EmailSettings::default(),
                    None,
                    invite,
                    None,
                )
                .expect("the reply draft builds");
                let (plain, html) = text_parts(&built.raw);
                assert_eq!(plain.len(), 1, "one plain part (invite: {invite:?})");
                assert_eq!(html.len(), 1, "one html part (invite: {invite:?})");
                for part in plain.iter().chain(&html) {
                    for leak in ["mp:quote", "{{SIGNATURE}}", "mp:sig-start", "mp:sig-end"] {
                        assert!(
                            !part.contains(leak),
                            "{leak} leaked (marker {marker}, invite {invite:?}): {part}"
                        );
                    }
                    for kept in ["My reply", "Best, Alice", "Original text"] {
                        assert!(part.contains(kept), "{kept} missing: {part}");
                    }
                }
                let raw = String::from_utf8_lossy(&built.raw);
                assert!(!raw.contains(marker), "marker in the message bytes");
            }
        }
    }

    /// The sender's address survives the quote header of a sent reply,
    /// reply-all and forward in both parts (PERSO-169): the plain part
    /// carries `Name <addr>` as written, and the HTML part, rendered to text
    /// the way a mail client shows it, still names the address. Checked with
    /// a companion HTML original (the escaped header) and without one (the
    /// Markdown header, where `<addr>` is an email autolink).
    #[test]
    fn a_sent_quote_header_keeps_the_sender_address_in_both_parts() {
        use crate::draft::{create_forward_draft_from, create_reply_draft_from, SourceMessage};
        let senders = [
            ("Sylvain Hellin <s.hellin@evoqs.com>", "s.hellin@evoqs.com"),
            ("Zoë Müller <zoe@example.de>", "zoe@example.de"),
            ("\"AT&T Support\" <help@att.com>", "help@att.com"),
            ("bare@example.com", "bare@example.com"),
        ];
        for (from, addr) in senders {
            for with_html in [false, true] {
                for kind in ["reply", "reply-all", "forward"] {
                    let tmp = tempfile::tempdir().unwrap();
                    let source = SourceMessage {
                        from: from.into(),
                        to: "me@example.com, carol@example.com".into(),
                        subject: "Status".into(),
                        date: Some("Tue, 21 Oct 2026 20:12:00 +0200".into()),
                        body: "Original text".into(),
                        html: with_html.then(|| "<p>Original text</p>".to_string()),
                        ..Default::default()
                    };
                    let path = match kind {
                        "forward" => create_forward_draft_from(
                            &source,
                            "me@example.com",
                            Some(tmp.path()),
                            None,
                        ),
                        _ => create_reply_draft_from(
                            &source,
                            kind == "reply-all",
                            "me@example.com",
                            Some(tmp.path()),
                            None,
                        ),
                    }
                    .unwrap();
                    let mut draft = crate::draft::parse_email_draft(&path).unwrap();
                    draft.frontmatter.status = EmailStatus::Approved;
                    draft.frontmatter.to = Some("dave@example.com".into());
                    let built = build_draft_message(
                        &draft,
                        "me@example.com",
                        &EmailSettings::default(),
                        None,
                        None,
                        None,
                    )
                    .unwrap();
                    let (plain, html) = text_parts(&built.raw);
                    let case = format!("{from} / {kind} / html {with_html}");
                    assert!(plain[0].contains(from), "{case}: plain {}", plain[0]);
                    let shown = crate::parse::html_to_plain(&html[0]);
                    assert!(shown.contains(addr), "{case}: shown {shown}\n{}", html[0]);
                    assert!(shown.contains("Original text"), "{case}: {shown}");
                    if with_html {
                        let escaped = from
                            .replace('&', "&amp;")
                            .replace('<', "&lt;")
                            .replace('>', "&gt;")
                            .replace('"', "&quot;");
                        assert!(html[0].contains(&escaped), "{case}: {}", html[0]);
                    }
                }
            }
        }
    }

    /// Without a companion HTML original the send renders the Markdown quote
    /// header itself (PERSO-169). The HTML part of a forward shows each header
    /// field on its own line, and tag-shaped text in the subject, the
    /// recipients and the sender's display name (`Q&A <draft>`, `Ops <team>`)
    /// shows as text instead of being passed through as an unknown tag that
    /// clients drop; the reply attribution keeps the name the same way. The
    /// addresses still render (as autolinks) and the plain part carries every
    /// value as written, with no Markdown escape in it.
    #[test]
    fn a_markdown_quote_header_keeps_its_lines_and_tag_shaped_text() {
        use crate::draft::{create_forward_draft_from, create_reply_draft_from, SourceMessage};
        let from = "\"Ops <team>\" <ops@example.com>";
        let source = SourceMessage {
            from: from.into(),
            to: "Desk <desk> <me@example.com>, carol@example.com".into(),
            subject: "Q&A <draft>".into(),
            date: Some("Tue, 21 Oct 2026 20:12:00 +0200".into()),
            body: "Original text".into(),
            html: None,
            ..Default::default()
        };
        for kind in ["forward", "reply"] {
            let tmp = tempfile::tempdir().unwrap();
            let path = match kind {
                "forward" => {
                    create_forward_draft_from(&source, "me@example.com", Some(tmp.path()), None)
                }
                _ => create_reply_draft_from(
                    &source,
                    false,
                    "me@example.com",
                    Some(tmp.path()),
                    None,
                ),
            }
            .unwrap();
            let mut draft = crate::draft::parse_email_draft(&path).unwrap();
            draft.frontmatter.status = EmailStatus::Approved;
            draft.frontmatter.to = Some("dave@example.com".into());
            let built = build_draft_message(
                &draft,
                "me@example.com",
                &EmailSettings::default(),
                None,
                None,
                None,
            )
            .unwrap();
            let (plain, html) = text_parts(&built.raw);
            let shown = crate::parse::html_to_plain(&html[0]);
            let lines: Vec<&str> = shown.lines().map(str::trim).collect();
            assert!(
                shown.contains("\"Ops <team>\""),
                "{kind}: {shown}\n{}",
                html[0]
            );
            assert!(shown.contains("ops@example.com"), "{kind}: {shown}");
            assert!(plain[0].contains(from), "{kind}: {}", plain[0]);
            assert!(!plain[0].contains('\\'), "{kind}: {}", plain[0]);
            if kind == "forward" {
                for prefix in ["From: ", "Date: ", "Subject: ", "To: "] {
                    assert!(
                        lines.iter().any(|l| l.starts_with(prefix)),
                        "{prefix} does not start a line: {shown}\n{}",
                        html[0]
                    );
                }
                assert!(
                    lines.contains(&"Subject: Q&A <draft>"),
                    "{shown}\n{}",
                    html[0]
                );
                assert!(shown.contains("Desk <desk>"), "{shown}");
                // The hard break's two trailing spaces survive transfer
                // encoding (the decoded part has CRLF line ends).
                let plain_lf = plain[0].replace("\r\n", "\n");
                assert!(
                    plain_lf.contains("Subject: Q&A <draft>  \n"),
                    "{plain_lf:?}"
                );
            } else {
                assert!(
                    lines.iter().any(|l| l.starts_with("On Tue, 21 Oct 2026")
                        && l.contains("\"Ops <team>\"")
                        && l.ends_with("wrote:")),
                    "{shown}\n{}",
                    html[0]
                );
            }
        }
    }

    /// The unified Markdown signature (spliced into the reply body at draft
    /// creation, #0099) is the single source for both outgoing parts: the
    /// plain part carries it once (as Markdown link syntax) with no marker, and
    /// the HTML part carries it once as a rendered anchor. No send-time HTML
    /// injection, so it can no longer appear twice.
    #[test]
    fn a_reply_carries_the_signature_once_in_each_part() {
        let settings = EmailSettings::default();
        let body =
            "My reply\n\n[Robin](mailto:robin@example.com)\n\n{{SIGNATURE}}\n\nOn Mon wrote:\n> Quoted";

        // Plain part: marker gone, signature exactly once.
        let plain = plain_text_body(body);
        assert!(
            !plain.contains("{{SIGNATURE}}"),
            "marker in plain part: {plain:?}"
        );
        assert_eq!(
            plain.matches("mailto:robin@example.com").count(),
            1,
            "signature not exactly once in plain part: {plain:?}"
        );

        // HTML part: marker gone, no raw spliced `<` from the source, signature
        // exactly once as a rendered anchor.
        let html = markdown_to_html(body, &settings, None, None);
        assert!(!html.contains("{{SIGNATURE}}"), "marker in HTML part");
        assert_eq!(
            html.matches("mailto:robin@example.com").count(),
            1,
            "signature not exactly once in HTML part: {html}"
        );
        assert!(
            html.contains(r#"href="mailto:robin@example.com">Robin</a>"#),
            "signature link not rendered as an anchor: {html}"
        );
    }

    // -----------------------------------------------------------------------
    // RecipientRole display
    // -----------------------------------------------------------------------

    #[test]
    fn test_recipient_role_display() {
        assert_eq!(format!("{}", RecipientRole::To), "To");
        assert_eq!(format!("{}", RecipientRole::Cc), "Cc");
        assert_eq!(format!("{}", RecipientRole::Bcc), "Bcc");
    }

    // -----------------------------------------------------------------------
    // split_addresses
    // -----------------------------------------------------------------------

    #[test]
    fn split_addresses_lettre_parses_quoted_name() {
        use lettre::message::Mailbox;
        let addr = "\"Doe, Jane\" <jane@example.com>";
        let mbox: Mailbox = addr
            .parse()
            .expect("lettre should parse quoted display name");
        assert_eq!(mbox.email.to_string(), "jane@example.com");
    }

    // -----------------------------------------------------------------------
    // normalize_address_for_smtp
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_quotes_brackets_in_display_name() {
        // Real-world TUM mailing list: square brackets are not RFC 5322 atext.
        let raw = "CCBE_Researchers [TUBVCMS] <researchers.ccbe@ed.tum.de>";
        let normalized = normalize_address_for_smtp(raw);
        assert_eq!(
            normalized,
            "\"CCBE_Researchers [TUBVCMS]\" <researchers.ccbe@ed.tum.de>"
        );
        let _: lettre::message::Mailbox = normalized
            .parse()
            .expect("lettre must parse normalized form");
    }

    #[test]
    fn normalize_quotes_unquoted_comma_in_display_name() {
        // "Doe, Jane <j@x>" -- one entry, but the comma inside the
        // display name was not quoted. After splitting (which will treat
        // the whole thing as one because there's no separating comma between
        // entries), normalization must quote it so lettre accepts it.
        let raw = "Doe, Jane <jane@example.com>";
        let normalized = normalize_address_for_smtp(raw);
        assert_eq!(normalized, "\"Doe, Jane\" <jane@example.com>");
        let _: lettre::message::Mailbox = normalized
            .parse()
            .expect("lettre must parse normalized form");
    }

    #[test]
    fn normalize_extracts_email_via_mailbox_for_envelope() {
        // Regression for "Partial: 1/2 succeeded": `submit`'s per-recipient
        // RCPT TO loop parses the address again to extract `mbox.email` for
        // the SMTP envelope. If we forget to normalize there, lettre rejects
        // bracketed display names and that recipient silently fails while
        // the other one goes through.
        let raw = "CCBE_Researchers [TUBVCMS] <researchers.ccbe@ed.tum.de>";
        let mbox: lettre::message::Mailbox = normalize_address_for_smtp(raw)
            .parse()
            .expect("lettre must parse normalized form for envelope");
        assert_eq!(mbox.email.to_string(), "researchers.ccbe@ed.tum.de");
    }

    #[test]
    fn normalize_escapes_inner_quotes_and_backslashes() {
        let raw = "Weird \\ \"name\" <w@x.com>";
        let normalized = normalize_address_for_smtp(raw);
        // backslash and inner double-quotes must be escaped inside the
        // resulting quoted-string.
        assert_eq!(normalized, "\"Weird \\\\ \\\"name\\\"\" <w@x.com>");
        let _: lettre::message::Mailbox =
            normalized.parse().expect("lettre must parse escaped form");
    }

    // -----------------------------------------------------------------------
    // format_recipient (Contacts view seed sites, #0033)
    // -----------------------------------------------------------------------

    #[test]
    fn format_recipient_survives_split_addresses_as_one() {
        // The whole point: a "Last, First" contact name must not be split into
        // two broken recipients by split_addresses (which runs before
        // normalize_address_for_smtp on the send path).
        let seeded = format_recipient("Doe, John", "john@example.com");
        let parts = split_addresses(&seeded);
        assert_eq!(parts, vec!["\"Doe, John\" <john@example.com>"]);
        // And the single recipient parses as a proper lettre Mailbox.
        let mbox: lettre::message::Mailbox = normalize_address_for_smtp(&parts[0])
            .parse()
            .expect("lettre must parse the seeded recipient");
        assert_eq!(mbox.email.to_string(), "john@example.com");
    }

    // -----------------------------------------------------------------------
    // markdown_to_html edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_markdown_to_html_signature_appended_without_placeholder() {
        // Send-time signature (invites, direct sends) is Markdown; it is
        // appended to the body Markdown and rendered through the same converter
        // so it inherits the body font.
        let sig = "-- Best, Alice";
        let html = markdown_to_html("Hello world", &default_settings(), Some(sig), None);
        // Without placeholder, signature is appended after the body
        assert!(html.contains(">Hello world</p>"));
        // The signature is rendered from Markdown (its own <p>), not injected raw.
        assert!(html.contains(">-- Best, Alice</p>"));
    }

    #[test]
    fn test_markdown_to_html_no_signature() {
        let html = markdown_to_html("Hello", &default_settings(), None, None);
        assert!(html.contains(">Hello</p>"));
        // Should still be valid HTML
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("</html>"));
    }

    // -----------------------------------------------------------------------
    // Per-element inline styles (#0118)
    // -----------------------------------------------------------------------

    /// The reported bug: Apple Mail kept the wrapper <div>'s font for the <ul>
    /// and dropped it for every <p>, so the bullets came out a size larger than
    /// the prose. Every block element has to carry the font itself.
    #[test]
    fn a_list_and_the_prose_around_it_carry_the_same_inline_font() {
        let html = markdown_to_html(
            "Intro paragraph:\n\n- first\n- second\n\nClosing paragraph.",
            &default_settings(),
            None,
            None,
        );
        let font = "font-family: Helvetica, Arial, sans-serif; font-size: 16px; line-height: 1.6;";
        for tag in ["<p style=\"", "<ul style=\"", "<li style=\""] {
            assert!(html.contains(tag), "{tag} not stamped: {html}");
        }
        assert_eq!(
            html.matches(font).count(),
            // head <style> body rule + wrapper div + 2 paragraphs + ul + 2 li
            7,
            "font not on every block element: {html}"
        );
    }

    /// pulldown-cmark puts `text-align` on aligned table cells. Stamping must
    /// extend that attribute, not replace it, and the original declaration has to
    /// stay last so it still wins.
    #[test]
    fn stamping_extends_an_existing_style_attribute_instead_of_dropping_it() {
        let md = "| A | B |\n|---:|:--|\n| 1 | 2 |";
        let html = markdown_to_html(md, &default_settings(), None, None);
        assert!(
            html.contains(
                "<th style=\"font-family: Helvetica, Arial, sans-serif; font-size: 16px; \
                 line-height: 1.6; color: #000; text-align: right\">"
            ),
            "alignment lost or reordered: {html}"
        );
    }

    /// A code block must keep the client's monospace family; it takes only the
    /// metrics from the style block.
    #[test]
    fn code_keeps_its_monospace_family() {
        let html = markdown_to_html("```\nfn main() {}\n```", &default_settings(), None, None);
        assert!(
            html.contains("<pre style=\"font-size: 16px; line-height: 1.6;\">"),
            "pre not stamped with metrics only: {html}"
        );
        assert!(
            !html.contains("<pre style=\"font-family"),
            "pre must not inherit the body font family: {html}"
        );
    }

    /// The sender's own styling in a quoted reply is not ours to rewrite.
    #[test]
    fn the_quoted_original_is_passed_through_unstamped() {
        let quoted = "<p style=\"color:#666\">On Mon, someone wrote:</p>\n<p>Their words.</p>";
        let html = markdown_to_html(
            "My reply.\n\n{{SIGNATURE}}",
            &default_settings(),
            None,
            Some(quoted),
        );
        assert!(
            html.contains(quoted),
            "quoted section was rewritten: {html}"
        );
    }

    #[test]
    fn test_markdown_to_html_code_block() {
        let md = "```rust\nfn main() {}\n```";
        let html = markdown_to_html(md, &default_settings(), None, None);
        assert!(html.contains("<code"));
        assert!(html.contains("fn main()"));
    }

    #[test]
    fn test_markdown_to_html_with_quoted_html_replaces_blockquotes() {
        let md = "Reply\n\nsig\n\n{{SIGNATURE}}\n\n> original";
        let quoted = "<p>Original HTML</p>";
        let html = markdown_to_html(md, &default_settings(), None, Some(quoted));
        // When quoted_html is provided, it should be used instead of markdown blockquotes
        assert!(html.contains("Original HTML"));
        assert!(html.contains("sig"));
    }
}

/// Build the `multipart/alternative` for an iMIP invite: `text/plain`,
/// `text/html`, and the inline `text/calendar; method=REQUEST; charset=UTF-8`
/// part carrying the `VEVENT`. This inline calendar part is the contract
/// (`docs/plans/calendar-invites.md`, §2) — Outlook and Gmail render it as an
/// actionable Accept / Tentative / Decline invite.
fn build_invite_alternative(plain: &str, html: String, ics: &str) -> MultiPart {
    let calendar_ct: ContentType = "text/calendar; method=REQUEST; charset=UTF-8"
        .parse()
        .expect("static calendar content-type");
    MultiPart::alternative()
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(plain.to_string()),
        )
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(html),
        )
        .singlepart(
            SinglePart::builder()
                .header(calendar_ct)
                .body(ics.to_string()),
        )
}

/// Build the optional `application/ics; name="invite.ics"` attachment carrying
/// the same `.ics` bytes as the inline part. Optional hardening (design §2):
/// ship it in v1, drop it if live testing shows duplicate rendering.
fn build_ics_attachment(ics: &str) -> SinglePart {
    let ct: ContentType = "application/ics; name=\"invite.ics\""
        .parse()
        .expect("static ics attachment content-type");
    Attachment::new("invite.ics".to_string()).body(ics.to_string(), ct)
}

/// Assemble the full iMIP invite body:
///
/// ```text
/// multipart/mixed
/// ├── multipart/alternative
/// │   ├── text/plain
/// │   ├── text/html
/// │   └── text/calendar; method=REQUEST; charset=UTF-8   (the contract)
/// └── application/ics; name="invite.ics"                 (optional hardening)
/// ```
///
/// Shared by the live send path and the round-trip integration test so both
/// exercise the identical MIME shape. File attachments (if any) are appended by
/// the caller after this base.
pub fn build_invite_mime_body(plain: &str, html: String, ics: &str) -> MultiPart {
    MultiPart::mixed()
        .multipart(build_invite_alternative(plain, html, ics))
        .singlepart(build_ics_attachment(ics))
}

/// Connect to the SMTP server `smtp_config` names, authenticate and hang up,
/// sending nothing: the connection test `config.probe` runs for a wizard
/// before an account is written. The transport is [`build_smtp_transport`]'s,
/// so the test applies the policy a submission would.
pub async fn test_smtp_connection(smtp_config: &SmtpConfig) -> Result<()> {
    let transport = build_smtp_transport(smtp_config)?;
    if transport.test_connection().await? {
        Ok(())
    } else {
        Err(anyhow!(
            "{}:{} did not accept the connection",
            smtp_config.host,
            smtp_config.port
        ))
    }
}

/// Build the SMTP transport (implicit TLS on :465, STARTTLS otherwise),
/// honouring the account's OAuth2/password auth and `accept_invalid_certs`
/// opt-in. The single transport builder behind [`submit`], so every SMTP
/// submission applies identical transport policy.
fn build_smtp_transport(smtp_config: &SmtpConfig) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
    if smtp_config.accept_invalid_certs {
        crate::config::ensure_invalid_certs_allowed(&smtp_config.host)?;
    }
    let creds = Credentials::new(smtp_config.username.clone(), smtp_config.password.clone());

    let mailer: AsyncSmtpTransport<Tokio1Executor> = if smtp_config.port == 465 {
        // Implicit TLS (SMTPS)
        let mut transport = AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp_config.host)?;
        if smtp_config.accept_invalid_certs {
            let tls_params =
                lettre::transport::smtp::client::TlsParameters::builder(smtp_config.host.clone())
                    .dangerous_accept_invalid_certs(true)
                    .build()?;
            transport = transport.tls(lettre::transport::smtp::client::Tls::Wrapper(tls_params));
        }
        let transport = transport.port(smtp_config.port).credentials(creds);
        if smtp_config.auth_method == AuthMethod::OAuth2 {
            transport
                .authentication(vec![
                    lettre::transport::smtp::authentication::Mechanism::Xoauth2,
                ])
                .build()
        } else {
            transport.build()
        }
    } else {
        // STARTTLS
        let mut transport =
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp_config.host)?;
        if smtp_config.accept_invalid_certs {
            let tls_params =
                lettre::transport::smtp::client::TlsParameters::builder(smtp_config.host.clone())
                    .dangerous_accept_invalid_certs(true)
                    .build()?;
            transport = transport.tls(lettre::transport::smtp::client::Tls::Required(tls_params));
        }
        let transport = transport.port(smtp_config.port).credentials(creds);
        if smtp_config.auth_method == AuthMethod::OAuth2 {
            transport
                .authentication(vec![
                    lettre::transport::smtp::authentication::Mechanism::Xoauth2,
                ])
                .build()
        } else {
            transport.build()
        }
    };
    Ok(mailer)
}

/// Build the `multipart/alternative` body of an iMIP RSVP `REPLY` (#0029):
/// a minimal `text/plain` human note plus the inline
/// `text/calendar; method=REPLY; charset=UTF-8` part carrying the responding
/// `ATTENDEE`'s `PARTSTAT`. No `text/html` and no `.ics` attachment: a REPLY
/// is machine-consumed by the organizer's client, so the alternative stays
/// lean (mirrors what Gmail/Outlook emit).
pub fn build_reply_mime_body(plain: &str, ics: &str) -> MultiPart {
    let calendar_ct: ContentType = "text/calendar; method=REPLY; charset=UTF-8"
        .parse()
        .expect("static calendar content-type");
    MultiPart::alternative()
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(plain.to_string()),
        )
        .singlepart(
            SinglePart::builder()
                .header(calendar_ct)
                .body(ics.to_string()),
        )
}

/// Send an iMIP RSVP `REPLY` to a received invitation's `ORGANIZER`.
///
/// `from` is the responding account's primary address (also the REPLY
/// `ATTENDEE`); `organizer` is the single recipient. `reply_ics` is the
/// `METHOD:REPLY` payload from [`crate::invite::build_reply_ics`]. `subject`
/// follows the Outlook convention (`Accepted:`/`Tentative:`/`Declined: <summary>`).
/// The reply is built here and submitted through the durable outbox by
/// [`send_rsvp`], like any other outgoing message.
pub fn build_reply_message(
    from: &str,
    organizer: &str,
    subject: &str,
    plain_body: &str,
    reply_ics: &str,
) -> Result<BuiltMessage> {
    // Normalised once and carried on the built message, the twin of what
    // `build_draft_message` does: `submit` parses `BuiltMessage::from` to
    // derive `MAIL FROM`, so a raw `Doe, Jane <j@x.com>` stored here would
    // pass the build and then fail every submission it would ever get
    // (#0063 review).
    let normalized_from = normalize_address_for_smtp(from);
    let from_mailbox: Mailbox = normalized_from
        .parse()
        .context("Invalid 'from' address for RSVP reply")?;
    let organizer_mailbox: Mailbox = normalize_address_for_smtp(organizer)
        .parse()
        .context("Invalid ORGANIZER address for RSVP reply")?;

    info!(
        "Building RSVP reply: subject=\"{}\", from={}, to={}",
        subject, from, organizer
    );

    let message = Message::builder()
        .from(from_mailbox.clone())
        .to(organizer_mailbox.clone())
        .subject(subject)
        .message_id(None)
        .multipart(build_reply_mime_body(plain_body, reply_ics))
        .context("Failed to build RSVP reply message")?;

    let raw_message = message.formatted();
    Ok(BuiltMessage {
        message_id: message_id_of(&raw_message),
        raw: raw_message,
        recipients: vec![(organizer.to_string(), RecipientRole::To)],
        from: normalized_from,
        // An RSVP is built from an invitation, not from a draft file, and
        // there is no second copy of it to press send on.
        draft_key: None,
    })
}

/// Outcome of an RSVP send, carried back to the caller so it can surface a
/// status line. The Sent copy is the outbox's business, not the caller's.
pub struct RsvpOutcome {
    /// Where the reply's outbox row ended up, or `None` when the store could
    /// not be opened and the reply was submitted without a durable record.
    pub outbox_state: Option<crate::outbox::OutboxState>,
    pub send_result: SendResult,
    pub raw_message: Vec<u8>,
    pub message_id: Option<String>,
    /// The `ORGANIZER` the reply was sent to.
    pub organizer: String,
    /// The Outlook-convention subject used (`Accepted: <summary>` etc.).
    pub subject: String,
}

/// End-to-end attendee RSVP to a received invite (#0029), shared by the CLI
/// and the TUI so both run identical logic (no subprocess).
///
/// Steps: build a `METHOD:REPLY` from the invitation's own iMIP payload (the
/// source of truth for `UID`/`SEQUENCE`) carrying the account's `PARTSTAT`,
/// and email it to the `ORGANIZER`. The payload itself is never rewritten.
///
/// Nothing local is flipped afterwards, and there is nothing to flip: the
/// reply goes out through the durable outbox, which appends it to the server
/// Sent mailbox and ingests that copy into the store during the send, so our
/// own `PARTSTAT` is derived from that row the next time an invite is
/// rendered (#0038 scope item 6, `crate::reconcile::own_rsvp`).
///
/// `ics` is the invitation's `invite.ics` bytes, read from the message's blob
/// by the caller; `account_address` is the responding account's primary
/// address (the REPLY `ATTENDEE`).
pub async fn send_rsvp(
    ics: &[u8],
    account_config: &crate::config::AccountConfig,
    account_address: &str,
    rsvp: crate::invite::Rsvp,
    smtp_config: &SmtpConfig,
) -> Result<RsvpOutcome> {
    prepare_rsvp(ics, account_address, rsvp)?
        .submit(account_config, smtp_config)
        .await
}

/// An RSVP reply built and not yet submitted: the half of [`send_rsvp`] that
/// touches no transport, split off so `calendar.rsvp` can observe its cancel
/// token between the two. Dropping it sends nothing.
pub struct PreparedRsvp {
    built: BuiltMessage,
    organizer: String,
    subject: String,
}

/// Build the `METHOD:REPLY` and its message, exactly as [`send_rsvp`] does
/// before it submits.
pub fn prepare_rsvp(
    ics: &[u8],
    account_address: &str,
    rsvp: crate::invite::Rsvp,
) -> Result<PreparedRsvp> {
    let account_address = crate::parse::extract_email_address(account_address);
    if account_address.is_empty() {
        return Err(anyhow!("Account has no usable address to RSVP as"));
    }

    let ctx = crate::invite::reply_context_from_ics(ics)?;
    let reply_ics = crate::invite::build_reply_ics(&ctx, &account_address, rsvp)?;

    let summary = ctx.summary.as_deref().unwrap_or("(no subject)");
    let subject = format!("{}: {}", rsvp.subject_verb(), summary);
    let plain_body = format!("{} the invitation: {}", rsvp.subject_verb(), summary);

    let built = build_reply_message(
        &account_address,
        &ctx.organizer,
        &subject,
        &plain_body,
        &reply_ics,
    )?;

    Ok(PreparedRsvp {
        built,
        organizer: ctx.organizer,
        subject,
    })
}

impl PreparedRsvp {
    /// The durable submission: the commit point of an RSVP.
    pub async fn submit(
        self,
        account_config: &crate::config::AccountConfig,
        smtp_config: &SmtpConfig,
    ) -> Result<RsvpOutcome> {
        let PreparedRsvp {
            built,
            organizer,
            subject,
        } = self;
        let report = send_durably(&built, account_config, smtp_config).await?;
        Ok(RsvpOutcome {
            outbox_state: report.state,
            send_result: report.send_result,
            raw_message: built.raw,
            message_id: Some(built.message_id),
            organizer,
            subject,
        })
    }
}

// ---------------------------------------------------------------------------
// The durable send path (#0037 item 5)
// ---------------------------------------------------------------------------

/// What one durable send did, end to end.
pub struct SendReport {
    /// The SMTP (or Graph) result, per recipient.
    pub send_result: SendResult,
    /// Where the outbox row ended up. `None` when the store could not be
    /// opened at all, in which case the message was still submitted but has no
    /// durable record.
    pub state: Option<crate::outbox::OutboxState>,
    /// The outbox row id, for a status line or a later retry.
    pub row_id: Option<i64>,
}

impl SendReport {
    /// One honest line about where the message actually is, for `mp send` and
    /// the TUI status bar.
    pub fn status_line(&self) -> String {
        use crate::outbox::OutboxState;
        // A message some recipients never got is the one thing worth saying
        // before where the copy is (#0063): the outbox row keeps the detail.
        if !self.send_result.failed().is_empty() && self.send_result.any_succeeded() {
            return "partly delivered, see `mp outbox list`".to_string();
        }
        // A Graph send whose delivering request lost its answer stays queued
        // until the outbox has asked the mailbox whether it went out (#0063).
        if self.state == Some(OutboxState::PendingSend)
            && self
                .send_result
                .results
                .iter()
                .any(|r| r.verdict == RecipientVerdict::Unknown)
        {
            return "submitted, not yet confirmed (see `mp outbox list`)".to_string();
        }
        match self.state {
            Some(OutboxState::Done) => "sent + saved".to_string(),
            Some(OutboxState::SentPendingAppend) => "sent + append pending".to_string(),
            Some(OutboxState::Failed) => "failed (see the outbox)".to_string(),
            Some(OutboxState::PendingSend) => "queued, not sent".to_string(),
            None => {
                if self.send_result.any_succeeded() {
                    "sent (no local record)".to_string()
                } else {
                    "not sent".to_string()
                }
            }
        }
    }
}

/// A submission in flight: the outbox row exists, SMTP has not run yet.
///
/// Held by the caller across the submission so the Graph path and the SMTP
/// path share one state machine. See [`crate::outbox`] for the invariants.
pub struct DurableSend {
    store: crate::store::Store,
    blobs: crate::store::BlobStore,
    account: crate::config::AccountConfig,
    row_id: i64,
}

impl DurableSend {
    /// Commit the raw bytes and the `pending_send` row. Must be called before
    /// the message is submitted.
    pub fn begin(account: &crate::config::AccountConfig, built: &BuiltMessage) -> Result<Self> {
        let store = crate::store::Store::open_account(&account.name)?;
        let blobs = crate::store::BlobStore::for_account(&account.name);
        // `None` when the server files its own copy (Gmail, Graph, Proton) or
        // the user said `save_to_sent = "never"`: the row then goes straight
        // from `pending_send` to `done` on a 250, with no APPEND ever.
        let target = crate::config::appends_to_sent(account)
            .then(|| crate::config::resolve_sent_mailbox(account));
        let row_id = crate::outbox::enqueue(
            &store,
            &blobs,
            &account.name,
            target.as_deref(),
            &built.message_id,
            &built.raw,
            &crate::outbox::Envelope {
                from: built.from.clone(),
                recipients: built.recipients.clone(),
                draft_key: built.draft_key.clone(),
                ..Default::default()
            },
        )?;
        Ok(Self {
            store,
            blobs,
            account: account.clone(),
            row_id,
        })
    }

    pub fn row_id(&self) -> i64 {
        self.row_id
    }

    /// Commit "the transport is about to be entered" for this row.
    ///
    /// Called immediately before the submission, and never batched with
    /// anything else: the marker is what tells a later resume whether a
    /// `pending_send` row it finds was ever attempted (see
    /// [`crate::outbox::sweep_pending_sends`]). An error means the submission
    /// must not run (#0063 review): a message sent without its marker looks
    /// to the resume like one never sent, and a crash before its verdict is
    /// recorded would have the resume send it a second time.
    pub fn mark_started(&self) -> Result<()> {
        crate::outbox::mark_submission_started(&self.store, self.row_id).map_err(|e| {
            error!(
                "[outbox] could not mark row {} as entering submission: {e:#}",
                self.row_id
            );
            e
        })
    }

    /// Record what the submission did, committing the transition immediately.
    pub fn record(
        &self,
        outcome: &crate::outbox::SubmitOutcome,
    ) -> Result<crate::outbox::OutboxState> {
        crate::outbox::record_submission(&self.store, &self.blobs, self.row_id, outcome)
    }

    /// Drive the outstanding APPENDs for this account, this row included, and
    /// report where this row ended up.
    ///
    /// A row that cannot be appended now stays `sent_pending_append` and is
    /// picked up by the next startup or sync tick; the message is already on
    /// the server either way.
    pub async fn settle(&self) -> Result<crate::outbox::OutboxState> {
        drain_account(&self.store, &self.blobs, &self.account).await;
        Ok(crate::outbox::load(&self.store, self.row_id)?
            .map(|row| row.state)
            .unwrap_or(crate::outbox::OutboxState::Done))
    }
}

/// Build, commit, submit over SMTP, record, append: the whole durable path for
/// an already-built message.
pub async fn send_durably(
    built: &BuiltMessage,
    account: &crate::config::AccountConfig,
    smtp_config: &SmtpConfig,
) -> Result<SendReport> {
    let durable = begin_durably(account, built)?;
    submit_durably(durable, built, smtp_config, &RealSmtp).await
}

/// The SMTP submission of one built message, behind a trait so the durable
/// path and the resume are tested against a counting fake rather than a
/// server or the env-armed fake transport, which would leak into every test
/// running beside them.
pub(crate) trait SmtpSubmit {
    fn submit(
        &self,
        built: &BuiltMessage,
        smtp_config: &SmtpConfig,
    ) -> impl std::future::Future<Output = Result<SendResult>>;
}

/// The real SMTP submission, [`submit`].
pub(crate) struct RealSmtp;

impl SmtpSubmit for RealSmtp {
    fn submit(
        &self,
        built: &BuiltMessage,
        smtp_config: &SmtpConfig,
    ) -> impl std::future::Future<Output = Result<SendResult>> {
        submit(built, smtp_config)
    }
}

/// Commit the row a send is about to make, or say why the send goes ahead
/// without one (`Ok(None)`) or must not go at all (`Err`).
fn begin_durably(
    account: &crate::config::AccountConfig,
    built: &BuiltMessage,
) -> Result<Option<DurableSend>> {
    match DurableSend::begin(account, built) {
        Ok(d) => Ok(Some(d)),
        // The admission gate is the one queue refusal that must stop the send:
        // sending anyway is precisely the double delivery it exists to prevent.
        Err(e) if crate::outbox::is_already_in_flight(&e) => Err(e),
        // A busy store is the gate not having been consulted yet, not a gate
        // that cannot exist: another process is writing this very outbox, and
        // sending anyway would walk straight past its admission gate. Told to
        // the user as the retryable condition it is (#0063 review).
        Err(e) if crate::outbox::is_store_busy(&e) => {
            Err(e.context("the outbox is busy with another send; try again"))
        }
        Err(e) => {
            // A store that will not open must not stop the user sending mail;
            // it only costs the durability of this one submission.
            error!("[outbox] could not queue the message durably: {e:#}");
            Ok(None)
        }
    }
}

/// Submit over SMTP with the row committed, record what happened, append.
async fn submit_durably<S: SmtpSubmit>(
    durable: Option<DurableSend>,
    built: &BuiltMessage,
    smtp_config: &SmtpConfig,
    smtp: &S,
) -> Result<SendReport> {
    if let Some(durable) = durable.as_ref() {
        if let Err(e) = durable.mark_started() {
            // Nothing entered the transport, so the failure is a clean one:
            // the row stays submittable, under backoff, for the resume.
            let clean = crate::outbox::SubmitOutcome::CleanPreSubmission(format!(
                "the send start could not be recorded: {e:#}"
            ));
            if let Err(e) = durable.record(&clean) {
                error!("[outbox] could not record a pre-submission failure: {e:#}");
            }
            return Err(e.context(
                "nothing was sent: the outbox could not record that the send was starting",
            ));
        }
    }
    let send_result = match smtp.submit(built, smtp_config).await {
        Ok(result) => result,
        Err(e) => {
            // The marker is already committed and nothing entered the
            // transport: everything `submit` can fail on (the envelope sender,
            // the transport itself) happens before the first connection. Left
            // as it is, the next resume would read the marker as "died inside
            // the SMTP session" and park a message that was never sent, so the
            // failure is recorded as the clean one it is, which puts the
            // marker back to NULL (#0063).
            if let Some(durable) = durable.as_ref() {
                let clean = crate::outbox::SubmitOutcome::CleanPreSubmission(format!("{e:#}"));
                if let Err(e) = durable.record(&clean) {
                    error!("[outbox] could not record a pre-submission failure: {e:#}");
                }
            }
            return Err(e);
        }
    };

    let Some(durable) = durable else {
        return Ok(SendReport {
            send_result,
            state: None,
            row_id: None,
        });
    };

    let mut state = durable.record(&send_result.submit_outcome())?;
    if state == crate::outbox::OutboxState::SentPendingAppend {
        state = durable.settle().await?;
    }
    Ok(SendReport {
        send_result,
        state: Some(state),
        row_id: Some(durable.row_id()),
    })
}

/// The durable path for a Microsoft Graph submission (PERSO-144, #0063).
///
/// The row is committed before the first request, as for SMTP, and
/// [`graph_submit::submit`] writes its progress to it: the draft it created,
/// and the marker immediately before the request that delivers. Graph files
/// its own copy in Sent Items, so with `save_to_sent = "auto"` the row never
/// carries a target mailbox and goes straight from `pending_send` to `done`.
///
/// A delivering request that lost its answer commits nothing: the row stays
/// `pending_send` with its marker, and the next resume asks the mailbox
/// ([`graph_submit::resume_rows`]).
pub async fn send_durably_graph<A: graph_submit::GraphSendApi>(
    built: &BuiltMessage,
    account: &crate::config::AccountConfig,
    plan: graph_submit::GraphPlan,
    api: &A,
) -> Result<SendReport> {
    let durable = match DurableSend::begin(account, built) {
        Ok(d) => Some(d),
        Err(e) if crate::outbox::is_already_in_flight(&e) => return Err(e),
        // As in `send_durably`: busy is retryable, not a bypass.
        Err(e) if crate::outbox::is_store_busy(&e) => {
            return Err(e.context("the outbox is busy with another send; try again"));
        }
        Err(e) => {
            error!("[outbox] could not queue the message durably: {e:#}");
            None
        }
    };

    let outcome =
        graph_submit::submit(durable.as_ref().map(|d| (&d.store, d.row_id)), plan, api).await;
    let verdict = match &outcome {
        graph_submit::GraphOutcome::Delivered => RecipientVerdict::Delivered,
        graph_submit::GraphOutcome::Clean(_) => RecipientVerdict::Retryable,
        graph_submit::GraphOutcome::Unconfirmed(_) => RecipientVerdict::Unknown,
    };
    let send_result = SendResult {
        results: built
            .recipients
            .iter()
            .map(|(addr, role)| RecipientResult {
                address: addr.clone(),
                role: *role,
                success: outcome == graph_submit::GraphOutcome::Delivered,
                error: outcome.error().map(str::to_string),
                verdict,
            })
            .collect(),
    };

    let Some(durable) = durable else {
        return Ok(SendReport {
            send_result,
            state: None,
            row_id: None,
        });
    };
    let mut state = match outcome.submit_outcome() {
        Some(transition) => durable.record(&transition)?,
        None => crate::outbox::OutboxState::PendingSend,
    };
    if state == crate::outbox::OutboxState::SentPendingAppend {
        state = durable.settle().await?;
    }
    Ok(SendReport {
        send_result,
        state: Some(state),
        row_id: Some(durable.row_id()),
    })
}

// ---------------------------------------------------------------------------
// One draft, sent (#0058)
// ---------------------------------------------------------------------------

/// Everything one durable send needs that does not come out of the draft.
///
/// `graph` being `Some` is what picks the Graph submission over SMTP, the same
/// test `mp send` makes on `AuthMethod::Graph`; `smtp` is what the SMTP path
/// submits through and where its `from` fallback comes from.
pub struct SendContext {
    pub graph: Option<crate::config::GraphConfig>,
    pub smtp: Option<SmtpConfig>,
    pub account: crate::config::AccountConfig,
    pub email_settings: EmailSettings,
    pub signature: Option<String>,
}

impl SendContext {
    /// The address a draft that names no `from:` of its own is sent from.
    ///
    /// The Graph path has no SMTP config to take a fallback from, so it takes
    /// the account's; identical bytes either way (see
    /// [`build_draft_message`]).
    fn default_from(&self) -> Result<&str> {
        match (self.graph.as_ref(), self.smtp.as_ref()) {
            (Some(_), _) => Ok(&self.account.default_from),
            (None, Some(smtp)) => Ok(&smtp.default_from),
            (None, None) => Err(anyhow!("SMTP not configured")),
        }
    }
}

/// The key one draft is admitted under (#0063).
///
/// The frontmatter `id` when there is one, because it survives the rename a
/// send performs and is the same key the drafts index and every
/// `mp://<account>/drafts/<key>` selector use. A file that has none (an
/// agent-written draft the index has not seen yet) falls back to its path,
/// which is the only other thing two sends of the same draft share.
pub fn draft_key(draft: &EmailDraft) -> String {
    match draft.frontmatter.id.as_deref() {
        Some(id) if !id.trim().is_empty() => format!("id:{}", id.trim()),
        _ => format!("path:{}", draft.path.display()),
    }
}

/// The drafts a send is running for in this process.
///
/// The cheap half of the admission gate (#0063): the TUI reaches
/// [`send_draft`] on a background thread per send, and an approved draft under
/// the cursor is by definition also in the approved batch, so the same draft
/// can be sent twice with nothing durable committed yet by either. This is a
/// plain set because the whole question is "is one already running", and the
/// answer has to be given without touching the disk.
///
/// Keyed by account as well as draft, like the durable half
/// ([`crate::outbox::enqueue`] scopes its query to one account): two accounts
/// each holding a hand-written draft with the same frontmatter `id:` are two
/// messages, and refusing the second would be a false positive.
static SENDING: std::sync::LazyLock<std::sync::Mutex<HashSet<(String, String)>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

/// One draft's slot in [`SENDING`], released when the send returns however it
/// returns.
pub(crate) struct SendAdmission((String, String));

impl SendAdmission {
    /// `None` when a send for this draft is already running on this account.
    pub(crate) fn claim(account: &str, key: &str) -> Option<Self> {
        let slot = (account.to_string(), key.to_string());
        let mut sending = SENDING.lock().unwrap_or_else(|e| e.into_inner());
        sending.insert(slot.clone()).then(|| SendAdmission(slot))
    }

    /// The slot a resume of one outbox row takes: the draft's own, so a
    /// resume and a send of the same draft never run together, and a slot of
    /// the row's own when it came from no draft.
    pub(crate) fn claim_row(account: &str, row: &crate::outbox::OutboxRow) -> Option<Self> {
        let key = row
            .envelope
            .as_ref()
            .and_then(|e| e.draft_key.clone())
            .unwrap_or_else(|| format!("row:{}", row.id));
        Self::claim(account, &key)
    }
}

/// Retire the draft file an outbox row was built from, once a resume has
/// recorded its message as sent (#0063 review).
///
/// The live send retires the file through [`crate::draft::settle_sent_draft`]
/// when its own submission succeeds. A submission the resume completes (an
/// interrupted send, a clean failure resent later, an unconfirmed Graph send
/// found in Sent Items) never reaches that code, and a file left `approved` is
/// one `mp send-approved` sends again under a fresh `Message-ID`. The same
/// rule applies here: the file is marked sent once any recipient has the
/// message, and removed once the row is through with every recipient taking
/// it.
///
/// Best effort and logged: the message is out either way, and the admission
/// gate's check of finished rows ([`crate::outbox::refuse_a_finished_draft`])
/// still stands between a file that could not be retired and a second send.
/// Called with the row's send slot held.
pub(crate) fn retire_draft_of_row(
    drafts: &std::path::Path,
    store: &crate::store::Store,
    row_id: i64,
) {
    use crate::outbox::OutboxState;
    let row = match crate::outbox::load(store, row_id) {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(e) => {
            warn!("[outbox] could not reload row {row_id} to retire its draft: {e:#}");
            return;
        }
    };
    let Some(envelope) = row.envelope.as_ref() else {
        return;
    };
    let Some(key) = envelope.draft_key.as_deref() else {
        return;
    };
    let finished = matches!(
        row.state,
        OutboxState::Done | OutboxState::SentPendingAppend
    );
    if !finished && envelope.delivered.is_empty() {
        return;
    }
    let draft = match find_draft_by_key(drafts, key) {
        Ok(Some(draft)) => draft,
        Ok(None) => {
            debug!("[outbox] row {row_id}: no draft file under {key} to retire");
            return;
        }
        Err(e) => {
            warn!("[outbox] row {row_id}: could not find its draft {key}: {e:#}");
            return;
        }
    };
    if let Err(e) = crate::draft::mark_draft_sent(&draft, Some(&row.message_id)) {
        warn!(
            "[outbox] row {row_id} was sent but {} could not be marked sent: {e:#}",
            draft.path.display()
        );
        return;
    }
    if !(finished && envelope.rejected.is_empty()) {
        return;
    }
    match std::fs::remove_file(&draft.path) {
        Ok(()) => info!(
            "[outbox] row {row_id} was sent on resume; retired its draft {}",
            draft.path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(
            "[outbox] row {row_id} was sent but {} could not be removed: {e:#}",
            draft.path.display()
        ),
    }
}

/// The draft file in `drafts` a send was admitted under `key`
/// ([`draft_key`]): the file at the path of a `path:` key, else the one file
/// whose frontmatter `id:` matches. `None` when there is none, or when two
/// files claim the id and nothing can say which one was sent.
fn find_draft_by_key(drafts: &std::path::Path, key: &str) -> Result<Option<EmailDraft>> {
    if let Some(path) = key.strip_prefix("path:") {
        let path = std::path::Path::new(path);
        if !path.is_file() {
            return Ok(None);
        }
        return crate::draft::parse_email_draft(path).map(Some);
    }
    let entries = match std::fs::read_dir(drafts) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow::Error::from(e))
                .with_context(|| format!("reading {}", drafts.display()))
        }
    };
    let mut found: Vec<EmailDraft> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !(path.is_file() && path.extension().is_some_and(|ext| ext == "md")) {
            continue;
        }
        // A file that will not parse is not the one a send was built from.
        if let Ok(draft) = crate::draft::parse_email_draft(&path) {
            if draft_key(&draft) == key {
                found.push(draft);
            }
        }
    }
    if found.len() > 1 {
        warn!(
            "[outbox] {} draft files share {key}; none is retired automatically",
            found.len()
        );
        return Ok(None);
    }
    Ok(found.pop())
}

impl Drop for SendAdmission {
    fn drop(&mut self) {
        let mut sending = SENDING.lock().unwrap_or_else(|e| e.into_inner());
        sending.remove(&self.0);
    }
}

/// What [`send_draft`] did: the durable send, and what became of the file.
pub struct SentDraft {
    /// Where the submission got to, per recipient and in the outbox.
    pub report: SendReport,
    /// The `Message-ID` the build minted, the one the outbox row and the
    /// retired draft record (#0131).
    pub message_id: String,
    /// The bookkeeping error a submission that did go out nevertheless hit
    /// while retiring the draft file. The message is on the server either
    /// way, so this is a line for the caller to word, never a failed send.
    pub settle_error: Option<anyhow::Error>,
}

/// Parse an address string like `"Name <addr>"` or `"addr"` into
/// `(name, address)`.
pub(crate) fn parse_name_address(s: &str) -> (String, String) {
    let s = s.trim();
    if let Some(lt) = s.find('<') {
        if let Some(gt) = s.find('>') {
            let name = s[..lt].trim().trim_matches('"').trim().to_string();
            let addr = s[lt + 1..gt].trim().to_string();
            return (name, addr);
        }
    }
    // Plain address
    (String::new(), s.to_string())
}

/// Parse a frontmatter address field (comma-separated) into `(name, address)`
/// pairs, which is the shape the Graph API takes its recipients in.
fn parse_graph_recipients(field: Option<&str>) -> Vec<(String, String)> {
    match field {
        Some(s) if !s.trim().is_empty() => split_addresses(s)
            .into_iter()
            .map(|a| parse_name_address(&a))
            .collect(),
        _ => Vec::new(),
    }
}

/// The directory a draft's relative `attachments:` entries are anchored to:
/// the draft file's own directory.
///
/// A bare file name has no directory component - `Path::parent` answers
/// `Some("")` for it, not `None` - and only a hand-built `EmailDraft` has one,
/// since every draft the daemon reads comes off a full path. That degenerate
/// case falls back to `.`, which leaves the entry relative exactly as the
/// pre-daemon binary left it.
fn attachment_anchor(draft: &EmailDraft) -> &Path {
    match draft.path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// Expand a draft's `attachments:` frontmatter entries into concrete files.
///
/// Each entry is tilde-expanded and then made absolute against `anchor`. A
/// directory entry contributes every regular file directly inside it
/// (non-recursive, sorted by file name, dotfiles skipped); any other entry
/// contributes itself. This lets a draft name one folder instead of listing
/// every file in it. A path that is neither a readable directory nor an
/// existing file is passed through, so the later `fs::read` reports the
/// missing path the same way it always has.
///
/// `anchor` is the draft's own directory ([`attachment_anchor`]), not the
/// process's working directory. Every send runs inside the daemon now, and the
/// daemon is started from wherever the machine happened to start it
/// (`spawn_detached` sets no `current_dir`), so a `cwd`-anchored entry would
/// resolve somewhere nobody chose. No attachment path crosses the wire - the
/// client sends a selector - so the client cannot rewrite the entry either
/// without editing the draft file. The draft's directory is the one location
/// both processes agree on. This is an accepted divergence from the pre-daemon
/// binary, which anchored to the sender's cwd; see
/// `docs/tickets/0123-cli-cutover.md`.
///
/// The other visible change is in a failure: a missing `report.pdf` is now
/// reported by its full path.
pub fn resolve_attachment_paths(entries: &[String], anchor: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in entries {
        let expanded = shellexpand::tilde(entry);
        let path = crate::daemon::client::absolutise_in(Path::new(expanded.as_ref()), anchor);
        if path.is_dir() {
            let mut dir_files = Vec::new();
            for dent in fs::read_dir(&path)
                .with_context(|| format!("Failed to read attachment folder: {}", entry))?
            {
                let dent =
                    dent.with_context(|| format!("Failed to read attachment folder: {}", entry))?;
                let file = dent.path();
                if !file.is_file() {
                    continue;
                }
                let is_dotfile = file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'));
                if is_dotfile {
                    continue;
                }
                dir_files.push(file);
            }
            // Deterministic order so the same folder always attaches the same
            // sequence; `read_dir` yields entries in arbitrary order.
            dir_files.sort();
            paths.extend(dir_files);
        } else {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Read one attachment file into its `(filename, bytes, content-type)` triple.
fn read_attachment(path: &Path) -> Result<(String, Vec<u8>, String)> {
    let content =
        fs::read(path).with_context(|| format!("Failed to read attachment: {}", path.display()))?;
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "attachment".to_string());
    let content_type = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    Ok((filename, content, content_type))
}

/// The longest line RFC 5322 allows, CRLF excluded; `7bit` and `8bit` carry
/// no longer one.
const MAX_LINE_OCTETS: usize = 998;

/// One file attachment as a MIME part, `content_type` as [`read_attachment`]
/// guessed it.
///
/// Every type but `message/rfc822` takes lettre's own transfer encoding.
/// RFC 2046 §5.2.1 allows a `message/rfc822` part only `7bit`, `8bit` or
/// `binary`, where lettre would pick quoted-printable or base64 for any line
/// over 76 octets, so that part is labelled by [`rfc822_body`] instead
/// (PERSO-174). A message neither can carry, a line over 998 octets or a NUL,
/// goes as `application/octet-stream` under the same `.eml` name, which a
/// mail client still opens as a message, and the reason is logged. An SMTP
/// server without `8BITMIME` gets an `8bit` part in that same form, swapped in
/// by [`seven_bit_fallback`] when lettre refuses the bytes.
fn attachment_part(filename: String, bytes: Vec<u8>, content_type: &str) -> SinglePart {
    let octet_stream = || {
        "application/octet-stream"
            .parse::<ContentType>()
            .expect("static MIME type")
    };
    if content_type.eq_ignore_ascii_case("message/rfc822") {
        match rfc822_body(bytes) {
            Ok(body) => {
                let message = "message/rfc822"
                    .parse::<ContentType>()
                    .expect("static MIME type");
                return Attachment::new(filename).body(body, message);
            }
            Err((bytes, why)) => {
                warn!(
                    "[send] {filename} goes as application/octet-stream, not message/rfc822: {why}"
                );
                return Attachment::new(filename).body(bytes, octet_stream());
            }
        }
    }
    let parsed = content_type
        .parse::<ContentType>()
        .unwrap_or_else(|_| octet_stream());
    Attachment::new(filename).body(bytes, parsed)
}

/// The body of a `message/rfc822` part: its lines ended by CRLF (a bare LF
/// becomes one), labelled `7bit` when every octet is ASCII and `8bit`
/// otherwise, and never re-encoded, so the forwarded message arrives byte for
/// byte, signatures intact.
///
/// `Err` hands the bytes back with the reason neither label is true of them:
/// a NUL, a bare CR, or a line over [`MAX_LINE_OCTETS`]. `binary` would carry
/// those, but only to a server that offers `BINARYMIME`, which the send path
/// does not ask for.
fn rfc822_body(bytes: Vec<u8>) -> std::result::Result<Body, (Vec<u8>, String)> {
    let mut canonical = Vec::with_capacity(bytes.len() + bytes.len() / 64);
    let mut line = 0usize;
    let mut longest = 0usize;
    for (at, &byte) in bytes.iter().enumerate() {
        match byte {
            b'\0' => return Err((bytes, "it holds a NUL octet".to_string())),
            // The LF that follows writes the CRLF.
            b'\r' if bytes.get(at + 1) == Some(&b'\n') => {}
            b'\r' => return Err((bytes, "it holds a CR outside a line break".to_string())),
            b'\n' => {
                canonical.extend_from_slice(b"\r\n");
                longest = longest.max(line);
                line = 0;
            }
            _ => {
                canonical.push(byte);
                line += 1;
            }
        }
    }
    longest = longest.max(line);
    if longest > MAX_LINE_OCTETS {
        return Err((
            bytes,
            format!("a line is {longest} octets, over the {MAX_LINE_OCTETS} 7bit and 8bit allow"),
        ));
    }
    let encoding = if canonical.is_ascii() {
        ContentTransferEncoding::SevenBit
    } else {
        ContentTransferEncoding::EightBit
    };
    Ok(Body::dangerous_pre_encoded(canonical, encoding))
}

/// `raw` as a server without `8BITMIME` can take it: every `8bit`
/// `message/rfc822` part becomes a base64 `application/octet-stream` part
/// under the same name, the fallback [`attachment_part`] already takes for a
/// message no line limit holds, and every other byte stays where it was, the
/// `Message-ID` included. `None` when `raw` has no such part, which is the
/// only 8-bit content the builder writes.
///
/// It works on the bytes rather than on the draft because a resumed outbox
/// row has only its committed bytes to submit ([`resubmit_row`]).
fn seven_bit_fallback(raw: &[u8]) -> Option<Vec<u8>> {
    fn eight_bit_messages<'a>(
        part: &'a mailparse::ParsedMail<'a>,
        out: &mut Vec<&'a mailparse::ParsedMail<'a>>,
    ) {
        let eight_bit = part
            .headers
            .iter()
            .find(|h| {
                h.get_key()
                    .eq_ignore_ascii_case("Content-Transfer-Encoding")
            })
            .is_some_and(|h| h.get_value().trim().eq_ignore_ascii_case("8bit"));
        if part.ctype.mimetype.eq_ignore_ascii_case("message/rfc822") {
            if eight_bit {
                out.push(part);
            }
            // What the forwarded message holds is its own business.
            return;
        }
        for sub in &part.subparts {
            eight_bit_messages(sub, out);
        }
    }

    let parsed = mailparse::parse_mail(raw).ok()?;
    let mut parts = Vec::new();
    for sub in &parsed.subparts {
        eight_bit_messages(sub, &mut parts);
    }
    if parts.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 3);
    let mut at = 0usize;
    for part in parts {
        let start = part.raw_bytes.as_ptr() as usize - raw.as_ptr() as usize;
        let filename = part
            .get_content_disposition()
            .params
            .get("filename")
            .or_else(|| part.ctype.params.get("name"))
            .cloned()
            .unwrap_or_else(|| "message.eml".to_string());
        // mailparse ends a part with the CRLF that opens the next boundary
        // line: that CRLF is the delimiter's (RFC 2046 5.1.1), not the
        // message's, and lettre writes it again after the new part.
        let delimiter = part.raw_bytes.ends_with(b"\r\n");
        let mut body = part.get_body_raw().ok()?;
        if delimiter && body.ends_with(b"\r\n") {
            body.truncate(body.len() - 2);
        }
        let octet_stream = "application/octet-stream"
            .parse::<ContentType>()
            .expect("static MIME type");
        let mut formatted = Attachment::new(filename)
            .body(body, octet_stream)
            .formatted();
        if !delimiter && formatted.ends_with(b"\r\n") {
            formatted.truncate(formatted.len() - 2);
        }
        out.extend_from_slice(&raw[at..start]);
        out.extend_from_slice(&formatted);
        at = start + part.raw_bytes.len();
    }
    out.extend_from_slice(&raw[at..]);
    Some(out)
}

/// Send one draft: the single orchestration behind `mp send`,
/// `mp send-approved` and the TUI's send key (#0058).
///
/// The order is the durable one (#0037 item 5): the bytes are built first
/// (which is where the approved-status requirement is enforced, by
/// [`build_draft_message`]), committed to the outbox, submitted over whichever
/// transport `ctx` names, and only then is the draft file touched. A
/// submission that reached at least one recipient rewrites the draft's
/// `status:` to `sent`; one that reached *every* recipient and got an outbox
/// row additionally takes the file out of `drafts/`, see
/// [`crate::draft::settle_sent_draft`]. The sent copy is the outbox's
/// business, not this function's.
///
/// What is left to the caller is what differs between the three: the
/// confirmation prompt, the wording of the status line, the exit code, and the
/// drafts-index refresh (a send-approved run pays for one refresh at the end
/// rather than one per draft).
pub async fn send_draft(draft: &EmailDraft, ctx: &SendContext) -> Result<SentDraft> {
    let key = draft_key(draft);
    let _admission = SendAdmission::claim(&ctx.account.name, &key).ok_or_else(|| {
        anyhow::Error::new(crate::outbox::AlreadyInFlight(
            "this draft is already being sent; it is sent once, not twice".to_string(),
        ))
    })?;
    refuse_a_finished_draft(draft, &ctx.account.name, &key)?;
    let ancestry = reply_ancestry(draft, &ctx.account.name);
    let built = build_draft_message(
        draft,
        ctx.default_from()?,
        &ctx.email_settings,
        ctx.signature.as_deref(),
        None,
        ancestry.as_deref(),
    )?;

    let report = match ctx.graph.as_ref() {
        // Graph files its own copy in Sent Items, so the outbox row never
        // carries a target mailbox; what it buys here is exactly-once
        // durability of the submission. The plan is decided before the row is
        // committed, so a message Graph can never take is refused, not queued.
        Some(graph_config) => {
            let plan = graph_submit::plan(&built)?;
            let client = crate::graph::GraphClient::new_async(graph_config).await?;
            send_durably_graph(&built, &ctx.account, plan, &client).await?
        }
        None => {
            let smtp = ctx
                .smtp
                .as_ref()
                .ok_or_else(|| anyhow!("SMTP not configured"))?;
            send_durably(&built, &ctx.account, smtp).await?
        }
    };

    let mut settle_error = None;
    if report.send_result.any_succeeded() {
        if let Err(e) = crate::draft::settle_sent_draft(draft, &report, Some(&built.message_id)) {
            warn!("Sent but failed to retire {}: {e:#}", draft.path.display());
            settle_error = Some(e);
        }
        // Best-effort, no-op without a contacts cache; the message went out,
        // so the correspondent is worth remembering whether or not the file
        // could be retired.
        crate::contacts::hooks::bump_after_send(&ctx.account, draft);
        mark_source_after_send(draft, ctx).await;
    }
    Ok(SentDraft {
        report,
        message_id: built.message_id.clone(),
        settle_error,
    })
}

/// Refuse a draft whose last submission already went out (#0063 review).
///
/// The second guard behind [`retire_draft_of_row`]: a file that a resumed send
/// could not retire is still `approved`, and the outbox's duplicate check at
/// enqueue sees only rows in flight. A store that will not open costs the
/// guard and nothing else, as it costs [`DurableSend::begin`] its row.
fn refuse_a_finished_draft(draft: &EmailDraft, account: &str, key: &str) -> Result<()> {
    let store = match crate::store::Store::open_account(account) {
        Ok(store) => store,
        Err(e) => {
            debug!("[outbox] no store to check {key} against: {e:#}");
            return Ok(());
        }
    };
    crate::outbox::refuse_a_finished_draft(
        &store,
        account,
        key,
        draft.frontmatter.message_id.as_deref(),
    )
}

/// The `Message-ID` a reply draft answers, read off its `in_reply_to:`
/// (PERSO-137).
///
/// Trimmed and `None` when blank, the same reading [`source_to_flag`] gives
/// the key; a bare id from a hand-edited draft gets the angle brackets the
/// header grammar requires. A forward's `forwarded_from:` is not read: a
/// forward starts a conversation of its own rather than joining its source's.
fn reply_parent_id(draft: &EmailDraft) -> Option<String> {
    let id = draft.frontmatter.in_reply_to.as_deref()?.trim();
    let bare = id.trim_start_matches('<').trim_end_matches('>').trim();
    (!bare.is_empty()).then(|| format!("<{bare}>"))
}

/// The `References` value of a reply: the parent's own ancestry, then the
/// parent, each id once (RFC 5322 section 3.6.4).
fn references_for_reply(parent: &str, parent_references: Option<&str>) -> String {
    let mut seen: HashSet<String> = HashSet::new();
    let mut ids: Vec<String> = parent_references
        .map(crate::ingest::split_message_ids)
        .unwrap_or_default()
        .into_iter()
        .filter(|id| id != parent && seen.insert(id.clone()))
        .collect();
    ids.push(parent.to_string());
    ids.join(" ")
}

/// The ancestry a reply's parent carried, from the store's copy of it
/// (PERSO-137): its `References`, or, when it has none, an `In-Reply-To` that
/// names a single id, the fallback RFC 5322 section 3.6.4 gives.
fn parent_ancestry(in_reply_to: Option<String>, references: Option<String>) -> Option<String> {
    references
        .filter(|refs| !crate::ingest::split_message_ids(refs).is_empty())
        .or_else(|| in_reply_to.filter(|irt| crate::ingest::split_message_ids(irt).len() == 1))
}

/// What [`build_draft_message`] takes as `parent_references` for one draft,
/// looked up in the account's store; `None` for anything but a reply.
///
/// Best effort: a store that cannot be read costs the reply its grandparents
/// in `References`, never the send, and `In-Reply-To` still threads it.
pub fn reply_ancestry(draft: &EmailDraft, account: &str) -> Option<String> {
    let parent = reply_parent_id(draft)?;
    let store = match crate::store::Store::open_account(account) {
        Ok(store) => store,
        Err(e) => {
            warn!("[send] could not open the store to thread the reply to {parent}: {e:#}");
            return None;
        }
    };
    match crate::store::read::threading_headers_of(&store, account, &parent) {
        Ok(found) => found.and_then(|(irt, refs)| parent_ancestry(irt, refs)),
        Err(e) => {
            warn!("[send] could not read the threading headers of {parent}: {e:#}");
            None
        }
    }
}

/// The source a sent draft has something to say about, and which bit it sets:
/// `true` for `\Answered`, `false` for `$Forwarded` (#TKT-0051).
///
/// A draft carries at most one of the two keys, because it was built as a
/// reply or as a forward and never as both; a reply wins if a hand-edited
/// draft names both, since answering is the stronger statement.
fn source_to_flag(draft: &EmailDraft) -> Option<(&str, bool)> {
    let (message_id, answered) = match (
        draft.frontmatter.in_reply_to.as_deref(),
        draft.frontmatter.forwarded_from.as_deref(),
    ) {
        (Some(id), _) => (id.trim(), true),
        (None, Some(id)) => (id.trim(), false),
        (None, None) => return None,
    };
    (!message_id.is_empty()).then_some((message_id, answered))
}

/// Whether a reply draft answers more than one person, which Exchange records
/// as a reply to all rather than a reply (PERSO-171).
///
/// The draft does not remember which command built it, so this reads its
/// recipients: more than one address across To and Cc is a reply to all. A
/// Cc added by hand to a plain reply reads as one too; both verbs show the
/// same replied icon in Outlook, so the cost of a wrong guess is the tooltip.
fn answers_all(draft: &EmailDraft) -> bool {
    let to = parse_graph_recipients(draft.frontmatter.to.as_deref());
    let cc = parse_graph_recipients(draft.frontmatter.cc.as_deref());
    to.len() + cc.len() > 1
}

/// The distinct server mailboxes the local copies of a source live in, in a
/// stable order (#0076).
///
/// One store row per mailbox is one server folder to flag; the same folder
/// twice is one `SELECT`, so the list is deduplicated before it rides the op.
fn server_mailboxes_of(
    account: &crate::config::AccountConfig,
    rows: &[crate::store::read::MessageRow],
) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let server_mailbox = crate::config::find_server_name_for_role(account, &row.mailbox);
        if seen.insert(server_mailbox.clone()) {
            out.push(server_mailbox);
        }
    }
    out
}

/// The second status axis, written after the message actually went out
/// (#TKT-0051, moved onto the durable queue by #0076).
///
/// A reply draft carries `in_reply_to:` and a forward carries
/// `forwarded_from:`, both the source's `Message-ID`; this is the one reader
/// of either. The local flag write and the server op's queue row commit in one
/// transaction ([`crate::pending_ops::apply_post_send_flag`]), and that commit
/// is the whole cost on the send path: **no IMAP session is opened here**. The
/// server half is one multi-mailbox op that
/// [`crate::pending_ops::resume_account`] drains over a single session on the
/// next tick or at the next startup.
///
/// Best effort throughout, and that is a durability statement, not a shrug:
/// nothing here may fail, delay or retry a send that already succeeded, so
/// every error is logged and swallowed, and the function returns `()`. The
/// enqueue happens strictly after the delivery and touches no `outbox` row, so
/// the exactly-once submission marker (#0063) is untouched by this path: a
/// message can never be re-sent because its bookkeeping failed. A queue row
/// that can never be written is still self-healed by the sync, which restates
/// every flag the server holds over the whole window.
///
/// A Graph account queues the same op (PERSO-171): its drain writes Outlook's
/// MAPI history properties on the server's copies, and its sync merge
/// (`ingest::apply_graph_flags`) only ever adds a history bit, so a pass can
/// never erase what is written here.
async fn mark_source_after_send(draft: &EmailDraft, ctx: &SendContext) {
    let Some((message_id, answered)) = source_to_flag(draft) else {
        return;
    };

    let store = match crate::store::Store::open_account(&ctx.account.name) {
        Ok(store) => store,
        Err(e) => {
            warn!("[send] could not open the store to flag {message_id}: {e:#}");
            return;
        }
    };
    let rows = match crate::store::read::find_by_message_id(&store, &ctx.account.name, message_id) {
        Ok(rows) => rows,
        Err(e) => {
            warn!("[send] could not look {message_id} up to flag it: {e:#}");
            return;
        }
    };
    if rows.is_empty() {
        debug!("[send] nothing local to flag for {message_id}");
        return;
    }
    // An account with no usable IMAP config has no server half to owe: write
    // the local bit and queue nothing. A Graph account owes one; its op writes
    // every server copy and only carries the folders for the record.
    let server_mailboxes = if ctx.graph.is_some() {
        server_mailboxes_of(&ctx.account, &rows)
    } else if let Err(e) = crate::config::ImapConfig::load(&ctx.account) {
        debug!("[send] no IMAP config to flag {message_id} on the server: {e:#}");
        Vec::new()
    } else {
        server_mailboxes_of(&ctx.account, &rows)
    };

    match crate::pending_ops::apply_post_send_flag(
        &store,
        &ctx.account.name,
        message_id,
        answered,
        answered && answers_all(draft),
        &server_mailboxes,
    ) {
        Ok(outcome) => debug!(
            "[send] flagged {} local row(s) for {message_id}, server op {:?}",
            outcome.rows, outcome.op_id
        ),
        Err(e) => warn!("[send] could not flag {message_id}: {e:#}"),
    }
}

/// Run every outstanding APPEND for one account, best effort.
///
/// Shared by the post-send settle, the startup resume and the sync tick, which
/// is why it drains under the engine lock: three callers on one account is a
/// normal Tuesday, and unguarded drains APPEND the same rows once each
/// (#0116). A drain that is refused the lock reports nothing done, and the
/// holder files its rows: it sweeps again against a clock it re-reads each
/// time, which is what lets it see a row committed after this timestamp was
/// taken.
///
/// Never returns an error: a Sent copy that has to wait for the next tick is
/// not a reason to fail whatever the caller was doing.
pub async fn drain_account(
    store: &crate::store::Store,
    blobs: &crate::store::BlobStore,
    account: &crate::config::AccountConfig,
) -> crate::outbox::DrainResult {
    drain_account_at(store, blobs, account, crate::outbox::unix_now()).await
}

/// [`drain_account`] against a clock of the caller's choosing.
///
/// The clock is what decides whether a row is still in its backoff, so a caller
/// that hands this a future timestamp is saying "now, whatever the backoff
/// says". `mp outbox retry` is that caller and nothing else is: an operator who
/// names one row means now, and the periodic drivers must keep waiting.
pub async fn drain_account_at(
    store: &crate::store::Store,
    blobs: &crate::store::BlobStore,
    account: &crate::config::AccountConfig,
    now: i64,
) -> crate::outbox::DrainResult {
    let counts = match crate::outbox::counts(store, &account.name) {
        Ok(c) => c,
        Err(e) => {
            log::warn!(
                "[outbox] could not read the outbox for {}: {e:#}",
                account.name
            );
            return crate::outbox::DrainResult::default();
        }
    };
    if counts.open == 0 {
        return crate::outbox::DrainResult::default();
    }

    // The fake Sent mailbox stands in for the IMAP session, credentials and
    // all (P4-U12): it is armed on the daemon and nowhere else.
    if let Some(fake) = crate::daemon::fake_transport::fake_transport() {
        let mut mailbox = crate::daemon::fake_transport::FakeSentMailbox::new(fake);
        return match crate::outbox::drain_guarded(store, blobs, &account.name, &mut mailbox, now)
            .await
        {
            Ok(Some(result)) => result,
            Ok(None) => crate::outbox::DrainResult::default(),
            Err(e) => {
                log::warn!("[outbox] draining {} failed: {e:#}", account.name);
                crate::outbox::DrainResult::default()
            }
        };
    }

    let imap_config = match crate::config::ImapConfig::load(account) {
        Ok(c) => c,
        Err(e) => {
            log::warn!(
                "[outbox] {} has {} row(s) waiting for an APPEND but no usable IMAP config: {e:#}",
                account.name,
                counts.open
            );
            return crate::outbox::DrainResult::default();
        }
    };

    // The session is opened on first use, so a drain that is refused the lock
    // costs no server traffic.
    let mut mailbox = crate::imap_client::ImapSentMailbox::new(imap_config);
    // Captured before the lock, so it is already stale by the time the first
    // APPEND goes out; the guarded drain re-reads the clock per sweep and only
    // treats this as a floor.
    let result = crate::outbox::drain_guarded(store, blobs, &account.name, &mut mailbox, now).await;
    mailbox.close().await;
    match result {
        Ok(Some(result)) => result,
        Ok(None) => crate::outbox::DrainResult::default(),
        Err(e) => {
            log::warn!("[outbox] draining {} failed: {e:#}", account.name);
            crate::outbox::DrainResult::default()
        }
    }
}

/// Resume the outbox for one account: the startup and sync-tick entry point.
///
/// Opens the account's store only when there is one, so a fresh account costs
/// nothing. Three things happen, in this order and for this reason:
///
/// 1. [`crate::outbox::sweep_pending_sends`] reads the exactly-once marker on
///    every `pending_send` row. A row that died inside its SMTP session is
///    parked in `failed` for a human and never re-sent.
/// 2. The rows that provably never reached the transport are submitted here,
///    which is the half of the crash story the driver cannot do: it needs the
///    credentials and the envelope, and both live on this side.
/// 3. [`drain_account`] finishes the outstanding APPENDs, this pass's included.
pub async fn resume_outbox(account: &crate::config::AccountConfig) -> crate::outbox::DrainResult {
    resume_outbox_at(account, crate::outbox::unix_now()).await
}

/// [`resume_outbox`] against a clock of the caller's choosing, which is what
/// makes `mp outbox retry` immediate; see [`drain_account_at`].
pub async fn resume_outbox_at(
    account: &crate::config::AccountConfig,
    now: i64,
) -> crate::outbox::DrainResult {
    let path = crate::config::store_path(&account.name);
    if !path.exists() {
        return crate::outbox::DrainResult::default();
    }
    let store = match crate::store::Store::open(&path) {
        Ok(store) => store,
        Err(e) => {
            log::warn!(
                "[outbox] could not open the store for {}: {e:#}",
                account.name
            );
            return crate::outbox::DrainResult::default();
        }
    };
    let blobs = crate::store::BlobStore::for_account(&account.name);
    resubmit_pending(&store, &blobs, account, now).await;
    drain_account_at(&store, &blobs, account, now).await
}

/// Send the `pending_send` rows that a crash left behind, exactly once each.
///
/// Never returns an error: this runs on startup and on the sync tick, where a
/// server that is still down must not fail the caller. A row that cannot be
/// submitted now stays `pending_send` and is tried again next time, under the
/// same backoff the APPEND retries use.
async fn resubmit_pending(
    store: &crate::store::Store,
    blobs: &crate::store::BlobStore,
    account: &crate::config::AccountConfig,
    now: i64,
) {
    // Before the sweep: a Graph row with a marker is settled by asking the
    // mailbox, not parked unseen (#0063 item 3).
    if account.auth_method == crate::config::AuthMethod::Graph {
        resubmit_pending_graph(store, blobs, account, now).await;
        return;
    }

    let sweep = match crate::outbox::sweep_pending_sends(store, &account.name) {
        Ok(sweep) => sweep,
        Err(e) => {
            log::warn!(
                "[outbox] could not classify the pending sends for {}: {e:#}",
                account.name
            );
            return;
        }
    };
    if !sweep.stranded.is_empty() {
        log::warn!(
            "[outbox] {} submission(s) for {} died mid-SMTP and are parked as failed; \
             inspect them with `mp outbox list`",
            sweep.stranded.len(),
            account.name
        );
    }
    if sweep.resubmittable.is_empty() {
        return;
    }

    // A fake transport needs no credentials: it is the transport (P4-U12).
    let smtp_config = match SmtpConfig::load(account) {
        Ok(c) => c,
        Err(_) if crate::daemon::fake_transport::armed() => SmtpConfig {
            host: "fake.invalid".to_string(),
            port: 465,
            username: String::new(),
            password: String::new(),
            default_from: account.default_from.clone(),
            accept_invalid_certs: false,
            auth_method: crate::config::AuthMethod::Password,
        },
        Err(e) => {
            log::warn!(
                "[outbox] {} has {} queued message(s) but no usable SMTP config: {e:#}",
                account.name,
                sweep.resubmittable.len()
            );
            return;
        }
    };

    let drafts = crate::config::drafts_dir(&account.name);
    resubmit_rows(
        store,
        blobs,
        &account.name,
        &drafts,
        sweep.resubmittable,
        now,
        &smtp_config,
        &RealSmtp,
    )
    .await;
}

/// Submit the rows the sweep found never attempted, each under its send slot,
/// and retire the draft file of every row that went out.
#[allow(clippy::too_many_arguments)]
async fn resubmit_rows<S: SmtpSubmit>(
    store: &crate::store::Store,
    blobs: &crate::store::BlobStore,
    account: &str,
    drafts: &std::path::Path,
    rows: Vec<crate::outbox::OutboxRow>,
    now: i64,
    smtp_config: &SmtpConfig,
    smtp: &S,
) {
    for row in rows {
        if row.updated + crate::outbox::backoff_secs(row.attempts) > now {
            // A previous clean failure (no transport, a rejected address) bumped
            // the counter; wait it out rather than hammer the server.
            continue;
        }
        // The slot `send_draft` takes for the same draft, so a send of it
        // started in this process meanwhile and this resubmission cannot both
        // run, and the file is retired before that send could read it again.
        let Some(_claim) = SendAdmission::claim_row(account, &row) else {
            log::debug!(
                "[outbox] row {} is being sent right now; leaving it",
                row.id
            );
            continue;
        };
        if let Err(e) = resubmit_row(store, blobs, &row, smtp_config, smtp).await {
            log::warn!("[outbox] could not resubmit row {}: {e:#}", row.id);
        }
        retire_draft_of_row(drafts, store, row.id);
    }
}

/// The Graph half of [`resubmit_pending`]: every `pending_send` row of the
/// account, settled or resubmitted by [`graph_submit::resume_rows`].
///
/// The client is built only when there is a row or a server cleanup
/// `mp outbox discard` left, so a clean account costs no token refresh on its
/// tick. Without a client the rows and the cleanups stay as they are, for the
/// next pass.
async fn resubmit_pending_graph(
    store: &crate::store::Store,
    blobs: &crate::store::BlobStore,
    account: &crate::config::AccountConfig,
    now: i64,
) {
    match graph_resume_owed(store, &account.name) {
        Ok(false) => return,
        Ok(true) => {}
        Err(e) => {
            log::warn!(
                "[outbox] could not read the pending sends for {}: {e:#}",
                account.name
            );
            return;
        }
    }
    let client = match crate::config::GraphConfig::load(account) {
        Ok(config) => crate::graph::GraphClient::new_async(&config).await,
        Err(e) => Err(e),
    };
    match client {
        Ok(client) => {
            let drafts = crate::config::drafts_dir(&account.name);
            graph_submit::resume_rows(store, blobs, &account.name, &drafts, now, &client).await
        }
        Err(e) => log::warn!(
            "[outbox] {} has queued Graph sends but no usable Graph client: {e:#}",
            account.name
        ),
    }
}

/// Whether a Graph account has anything for the resume: a `pending_send` row,
/// or a server cleanup `mp outbox discard` left.
fn graph_resume_owed(store: &crate::store::Store, account: &str) -> Result<bool> {
    Ok(
        !crate::outbox::pending_send_rows(store, account)?.is_empty()
            || !crate::outbox::server_cleanups(store, account)?.is_empty(),
    )
}

/// Submit one never-attempted row, marker first.
async fn resubmit_row<S: SmtpSubmit>(
    store: &crate::store::Store,
    blobs: &crate::store::BlobStore,
    row: &crate::outbox::OutboxRow,
    smtp_config: &SmtpConfig,
    smtp: &S,
) -> Result<crate::outbox::OutboxState> {
    let Some(envelope) = row.envelope.clone().filter(|e| e.is_submittable()) else {
        // Without an envelope there is no honest way to address the message,
        // and guessing from the headers would drop the blind recipients.
        crate::outbox::record_submission(
            store,
            blobs,
            row.id,
            &crate::outbox::SubmitOutcome::Ambiguous(
                "the queued submission has no usable envelope; it cannot be resent \
                 automatically"
                    .to_string(),
            ),
        )?;
        return Ok(crate::outbox::OutboxState::Failed);
    };
    // Only the recipients that have neither taken the message nor been refused
    // it: a recipient that answered 250 on an earlier pass must not see the
    // message a second time (#0063).
    let outstanding = envelope.outstanding();
    if outstanding.is_empty() {
        // Every recipient is settled, so there is nothing left to submit and
        // the row only needs the transition it did not get.
        return crate::outbox::record_submission(
            store,
            blobs,
            row.id,
            &crate::outbox::SubmitOutcome::PerRecipient(crate::outbox::RecipientVerdicts::default()),
        );
    }
    let raw = blobs
        .read(&row.raw_blob)
        .with_context(|| format!("reading the queued message of outbox row {}", row.id))?;
    let built = BuiltMessage {
        raw,
        message_id: row.message_id.clone(),
        recipients: outstanding,
        from: envelope.from,
        draft_key: envelope.draft_key,
    };

    info!(
        "[outbox] resubmitting row {} ({}) to {} recipient(s)",
        row.id,
        row.message_id,
        built.recipients.len()
    );
    crate::outbox::mark_submission_started(store, row.id)?;
    let result = match smtp.submit(&built, smtp_config).await {
        Ok(result) => result,
        Err(e) => {
            // Provably before the transport, as in `send_durably`: record it
            // as clean so the marker goes back to NULL and the row stays
            // submittable instead of being swept into `failed`.
            crate::outbox::record_submission(
                store,
                blobs,
                row.id,
                &crate::outbox::SubmitOutcome::CleanPreSubmission(format!("{e:#}")),
            )?;
            return Err(e);
        }
    };
    crate::outbox::record_submission(store, blobs, row.id, &result.submit_outcome())
}

/// A message that is fully built and not yet submitted.
///
/// The split between building and submitting is what the durable outbox is
/// made of: these bytes are committed to the store *before* the SMTP
/// conversation opens, so no crash window can lose a message that the server
/// might already have accepted.
#[derive(Debug, Clone)]
pub struct BuiltMessage {
    /// The RFC822 bytes, exactly as they go to SMTP and into the blob store.
    pub raw: Vec<u8>,
    /// The `Message-ID` header, synthesised when the builder produced none.
    /// The outbox's dedup search keys on it, so it is never optional.
    pub message_id: String,
    /// Every recipient with its role, deduplicated, in header order.
    pub recipients: Vec<(String, RecipientRole)>,
    /// The envelope sender.
    pub from: String,
    /// The draft these bytes were built from, when they were built from one.
    /// The outbox admits one submission per draft at a time (#0063).
    pub draft_key: Option<String>,
}

/// The `Message-ID` of a built message.
///
/// lettre generates one for every message it builds, so the fallback only
/// fires for bytes that came from somewhere else; it reuses the ingest path's
/// synthesis so the same message gets the same id on both sides.
pub fn message_id_of(raw: &[u8]) -> String {
    let header = mailparse::parse_headers(raw).ok().and_then(|(headers, _)| {
        headers
            .iter()
            .find(|h| h.get_key().eq_ignore_ascii_case("Message-ID"))
            .map(|h| h.get_value().trim().to_string())
            .filter(|v| !v.is_empty())
    });
    match header {
        Some(id) => id,
        None => match crate::parse::parse_rfc822_to_fetched_email(raw) {
            Some(email) => crate::ingest::synthesize_message_id(&email, Some(raw)),
            None => {
                use sha2::{Digest, Sha256};
                let digest = Sha256::digest(raw);
                let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
                format!("<sha256-{hex}@local.invalid>")
            }
        },
    }
}

/// The `text/html` part a draft is sent with: its Markdown body rendered by
/// [`markdown_to_html`], with the quoted original taken from the companion
/// `.html` the reply and forward builders write beside the draft.
///
/// The one place a draft's HTML is built, shared by the SMTP bytes
/// ([`build_draft_message`]), the Graph body ([`send_draft`]) and the
/// daemon's `draft.html` preview (PERSO-136), so what a user previews is what
/// a recipient gets. `signature` is the send-time one: `None` for every draft
/// send, whose signature was spliced into the body when the draft was written
/// (#0099), and `Some` only on the invite path, which has no editable draft.
///
/// A companion that is missing or unreadable renders the Markdown quote
/// instead, which is what the send has always done with one.
pub fn draft_html_body(
    draft: &EmailDraft,
    email_config: &EmailSettings,
    signature: Option<&str>,
) -> String {
    let companion = draft.path.with_extension("html");
    let quoted_html = if companion.exists() {
        fs::read_to_string(&companion).ok()
    } else {
        None
    };
    markdown_to_html(
        &draft.body_markdown,
        email_config,
        signature,
        quoted_html.as_deref(),
    )
}

/// Build the message a draft describes, without touching the network.
///
/// `default_from` is the account's address, used when the draft names no
/// `from:` of its own. It is passed in rather than read off the SMTP config so
/// the Graph path, which has no SMTP config, builds identical bytes.
///
/// A reply draft (`in_reply_to:`) gets `In-Reply-To` and `References` headers
/// (PERSO-137). `parent_references` is the ancestry the parent itself carried,
/// which the caller reads off the store ([`reply_ancestry`]); `None` when the
/// parent is not held locally, in which case `References` names the parent
/// alone. A forward and a fresh draft get neither header.
pub fn build_draft_message(
    draft: &EmailDraft,
    default_from: &str,
    email_config: &EmailSettings,
    signature: Option<&str>,
    invite_ics: Option<&str>,
    parent_references: Option<&str>,
) -> Result<BuiltMessage> {
    // Check status
    if draft.frontmatter.status != EmailStatus::Approved {
        return Err(anyhow!(
            "Email not approved for sending. Current status: {}",
            draft.frontmatter.status
        ));
    }

    let from_address = draft.frontmatter.from.as_deref().unwrap_or(default_from);

    // Normalised once and carried on the built message: what is validated
    // here is what `submit` parses to derive `MAIL FROM`, so a `from:` like
    // `Doe, Jane <j@x.com>` cannot pass the build and then fail every
    // submission forever (#0063 review).
    let normalized_from = normalize_address_for_smtp(from_address);
    let from_mailbox: Mailbox = normalized_from
        .parse()
        .context("Invalid 'from' email address")?;

    info!(
        "Sending email: subject=\"{}\", from={}",
        draft.frontmatter.subject, from_address
    );

    // Collect all recipients with roles, deduplicating by address
    let mut seen = HashSet::new();
    let mut recipients: Vec<(String, RecipientRole)> = Vec::new();

    if let Some(ref to) = draft.frontmatter.to {
        for addr in split_addresses(to) {
            if seen.insert(addr.to_lowercase()) {
                recipients.push((addr, RecipientRole::To));
            }
        }
    }
    if let Some(cc) = &draft.frontmatter.cc {
        for addr in split_addresses(cc) {
            if seen.insert(addr.to_lowercase()) {
                recipients.push((addr, RecipientRole::Cc));
            }
        }
    }
    if let Some(bcc) = &draft.frontmatter.bcc {
        for addr in split_addresses(bcc) {
            if seen.insert(addr.to_lowercase()) {
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

    // Threading (RFC 5322 section 3.6.4): what the recipient's client and
    // mp's own ingest (`resolve_thread_id`) group the reply on.
    if let Some(parent) = reply_parent_id(draft) {
        let references = references_for_reply(&parent, parent_references);
        builder = builder.in_reply_to(parent).references(references);
    }

    // Add To recipients to headers
    for (addr, role) in &recipients {
        match role {
            RecipientRole::To => {
                let mbox: Mailbox = normalize_address_for_smtp(addr)
                    .parse()
                    .context("Invalid 'to' email address")?;
                builder = builder.to(mbox);
            }
            RecipientRole::Cc => {
                let mbox: Mailbox = normalize_address_for_smtp(addr)
                    .parse()
                    .context("Invalid 'cc' email address")?;
                builder = builder.cc(mbox);
            }
            RecipientRole::Bcc => {
                let mbox: Mailbox = normalize_address_for_smtp(addr)
                    .parse()
                    .context("Invalid 'bcc' email address")?;
                builder = builder.bcc(mbox);
            }
        }
    }

    // Add Reply-To
    if let Some(reply_to) = &draft.frontmatter.reply_to {
        let reply_mailbox: Mailbox = normalize_address_for_smtp(reply_to)
            .parse()
            .context("Invalid 'reply_to' email address")?;
        builder = builder.reply_to(reply_mailbox);
    }

    let body_html = draft_html_body(draft, email_config, signature);

    // Build the plain/html alternative part (non-invite path).
    let body_multipart = MultiPart::alternative()
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(plain_text_body(&draft.body_markdown)),
        )
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(body_html.clone()),
        );

    // Build message with or without attachments
    let message = if let Some(ics) = invite_ics {
        // Invite path: multipart/mixed [ alternative(plain, html, calendar),
        // application/ics ]; regular file attachments (if any) follow.
        let mut mixed =
            build_invite_mime_body(&plain_text_body(&draft.body_markdown), body_html, ics);
        if let Some(attachments) = &draft.frontmatter.attachments {
            for path in resolve_attachment_paths(attachments, attachment_anchor(draft))? {
                let (filename, file_content, content_type) = read_attachment(&path)?;
                mixed = mixed.singlepart(attachment_part(filename, file_content, &content_type));
            }
        }
        builder
            .multipart(mixed)
            .context("Failed to build invite message")?
    } else if let Some(attachments) = &draft.frontmatter.attachments {
        let files = resolve_attachment_paths(attachments, attachment_anchor(draft))?;
        if !files.is_empty() {
            let mut mixed = MultiPart::mixed().multipart(body_multipart);

            for path in files {
                let (filename, file_content, content_type) = read_attachment(&path)?;
                mixed = mixed.singlepart(attachment_part(filename, file_content, &content_type));
            }

            builder
                .multipart(mixed)
                .context("Failed to build email message")?
        } else {
            builder
                .multipart(body_multipart)
                .context("Failed to build email message")?
        }
    } else {
        builder
            .multipart(body_multipart)
            .context("Failed to build email message")?
    };

    // Get raw message bytes for send_raw and IMAP APPEND
    let raw_message = message.formatted();

    Ok(BuiltMessage {
        message_id: message_id_of(&raw_message),
        raw: raw_message,
        recipients,
        from: normalized_from,
        draft_key: Some(draft_key(draft)),
    })
}

/// The bytes a Graph MIME send submits: the built message with its Bcc
/// recipients restored as a `Bcc:` header (PERSO-144).
///
/// lettre drops `Bcc:` from the formatted bytes because SMTP carries those
/// recipients in the envelope; Graph's MIME sendMail has no envelope and
/// reads every recipient off the headers, and Exchange strips `Bcc:` on
/// delivery. Everything else, `Message-ID` and the threading headers
/// included, is `built.raw` unchanged, so the outbox row, the local sent copy
/// and an SMTP send keep the bytes they always had.
pub fn graph_mime(built: &BuiltMessage) -> Result<Vec<u8>> {
    let mut bcc = lettre::message::Mailboxes::new();
    for (addr, role) in &built.recipients {
        if *role == RecipientRole::Bcc {
            let mbox: Mailbox = normalize_address_for_smtp(addr)
                .parse()
                .context("Invalid 'bcc' email address")?;
            bcc.push(mbox);
        }
    }
    if bcc.iter().next().is_none() {
        return Ok(built.raw.clone());
    }
    let mut headers = lettre::message::header::Headers::new();
    headers.set(lettre::message::header::Bcc::from(bcc));
    let mut raw = headers.to_string().into_bytes();
    raw.extend_from_slice(&built.raw);
    Ok(raw)
}

/// Submit an already-built message over SMTP, one envelope per recipient.
///
/// Never called before the message is committed to the outbox: see
/// [`crate::outbox`] for why the ordering is the whole design.
pub async fn submit(built: &BuiltMessage, smtp_config: &SmtpConfig) -> Result<SendResult> {
    // The daemon-side transport fake (P4-U12), which serves the submission in
    // process and records it. Unset outside a test, and never set on a client.
    if let Some(fake) = crate::daemon::fake_transport::fake_transport() {
        return Ok(fake.submit(&built.message_id, &built.recipients));
    }
    // Parse from address for envelope
    let from_addr: lettre::Address = built
        .from
        .parse::<Mailbox>()
        .context("Invalid 'from' address")?
        .email;

    // Create SMTP transport (branching on auth method / TLS mode).
    let mailer = build_smtp_transport(smtp_config)?;
    Ok(submit_through(built, &from_addr, &mailer).await)
}

/// [`submit`]'s per-recipient loop over a transport already built, split out
/// so a test can hand it a plain-TCP transport to a fake server.
///
/// A server whose EHLO offers no `8BITMIME` gets the 7bit bytes of
/// [`seven_bit_fallback`] once lettre refuses the 8bit ones. lettre refuses
/// before `MAIL FROM`, so nothing of the message reached the server and the
/// retry is that recipient's first submission, not a second one; every later
/// recipient takes the 7bit bytes straight away.
async fn submit_through(
    built: &BuiltMessage,
    from_addr: &lettre::Address,
    mailer: &AsyncSmtpTransport<Tokio1Executor>,
) -> SendResult {
    let mut seven_bit: Option<Vec<u8>> = None;
    let mut results = Vec::with_capacity(built.recipients.len());

    for (addr, role) in &built.recipients {
        let rcpt_addr: lettre::Address = match normalize_address_for_smtp(addr).parse::<Mailbox>() {
            Ok(mbox) => mbox.email,
            Err(e) => {
                let err_msg = format!("Invalid address '{}': {}", addr, e);
                error!("{}", err_msg);
                results.push(RecipientResult {
                    address: addr.clone(),
                    role: *role,
                    success: false,
                    error: Some(err_msg),
                    // Nothing about waiting turns an unparseable address into
                    // a deliverable one.
                    verdict: RecipientVerdict::Rejected,
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
                    verdict: RecipientVerdict::Rejected,
                });
                continue;
            }
        };

        let wire = seven_bit.as_deref().unwrap_or(&built.raw);
        let mut sent = mailer.send_raw(&envelope, wire).await;
        if seven_bit.is_none() && sent.as_ref().is_err_and(is_8bitmime_refusal) {
            if let Some(fallback) = seven_bit_fallback(&built.raw) {
                warn!(
                    "[send] {} holds an 8bit message/rfc822 part and the SMTP server offers no \
                     8BITMIME; sending it as base64 application/octet-stream instead",
                    built.message_id
                );
                sent = mailer.send_raw(&envelope, seven_bit.insert(fallback)).await;
            }
        }
        match sent {
            Ok(_) => {
                info!("Sent to {} ({})", addr, role);
                results.push(RecipientResult {
                    address: addr.clone(),
                    role: *role,
                    success: true,
                    error: None,
                    verdict: RecipientVerdict::Delivered,
                });
            }
            Err(e) => {
                let err_msg = format!("{}", e);
                error!("Failed to send to {} ({}): {}", addr, role, err_msg);
                results.push(RecipientResult {
                    address: addr.clone(),
                    role: *role,
                    success: false,
                    verdict: smtp_failure_verdict(&e),
                    error: Some(err_msg),
                });
            }
        }
    }

    SendResult { results }
}

/// lettre's refusal of a message with 8-bit octets to a server whose EHLO
/// offered no `8BITMIME`, raised before `MAIL FROM` is written
/// (`AsyncSmtpConnection::send`), so it is a client error and not a reply.
fn is_8bitmime_refusal(err: &lettre::transport::smtp::Error) -> bool {
    err.is_client() && err.to_string().contains("8BITMIME")
}

/// The SMTP resume and the durable send against a counting fake transport
/// (#0063 review of b1952fc4): no server, and no env-armed fake transport
/// leaking into the tests beside them.
#[cfg(test)]
mod smtp_durability_tests {
    use super::*;
    use crate::outbox::{self, OutboxState};
    use crate::store::{BlobStore, Store};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ACCOUNT: &str = "smtp-acct";

    /// Takes every message: one 250 per recipient.
    #[derive(Default)]
    struct CountingSmtp(AtomicUsize);

    impl SmtpSubmit for CountingSmtp {
        fn submit(
            &self,
            built: &BuiltMessage,
            _smtp_config: &SmtpConfig,
        ) -> impl std::future::Future<Output = Result<SendResult>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let results = built
                .recipients
                .iter()
                .map(|(address, role)| RecipientResult {
                    address: address.clone(),
                    role: *role,
                    success: true,
                    error: None,
                    verdict: RecipientVerdict::Delivered,
                })
                .collect();
            std::future::ready(Ok(SendResult { results }))
        }
    }

    impl CountingSmtp {
        fn calls(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn smtp_config() -> SmtpConfig {
        SmtpConfig {
            host: "smtp.invalid".to_string(),
            port: 465,
            username: String::new(),
            password: String::new(),
            default_from: "me@example.com".to_string(),
            accept_invalid_certs: false,
            auth_method: AuthMethod::Password,
        }
    }

    struct Outbox {
        dir: tempfile::TempDir,
        store: Store,
        blobs: BlobStore,
    }

    impl Outbox {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
            let blobs = BlobStore::new(dir.path().join("blobs"));
            Self { dir, store, blobs }
        }

        fn built(message_id: &str, draft_key: Option<&str>) -> BuiltMessage {
            BuiltMessage {
                raw: format!("Message-ID: {message_id}\r\nSubject: x\r\n\r\nbody\r\n").into_bytes(),
                message_id: message_id.to_string(),
                recipients: vec![("bob@example.com".to_string(), RecipientRole::To)],
                from: "me@example.com".to_string(),
                draft_key: draft_key.map(str::to_string),
            }
        }

        fn enqueue(&self, built: &BuiltMessage) -> i64 {
            outbox::enqueue(
                &self.store,
                &self.blobs,
                ACCOUNT,
                None,
                &built.message_id,
                &built.raw,
                &outbox::Envelope {
                    from: built.from.clone(),
                    recipients: built.recipients.clone(),
                    draft_key: built.draft_key.clone(),
                    ..Default::default()
                },
            )
            .unwrap()
        }

        fn row(&self, id: i64) -> outbox::OutboxRow {
            outbox::load(&self.store, id).unwrap().unwrap()
        }

        async fn resume(&self, smtp: &CountingSmtp) {
            let rows = outbox::pending_send_rows(&self.store, ACCOUNT).unwrap();
            resubmit_rows(
                &self.store,
                &self.blobs,
                ACCOUNT,
                &self.dir.path().join("drafts"),
                rows,
                outbox::unix_now(),
                &smtp_config(),
                smtp,
            )
            .await;
        }
    }

    /// The SMTP resume takes the slot `send_draft` holds for the same draft,
    /// so a row whose draft a send in this process is still working on is
    /// left alone, untouched, and submitted once the slot is free.
    #[tokio::test]
    async fn the_smtp_resume_leaves_a_row_whose_draft_is_being_sent() {
        let outbox = Outbox::new();
        let built = Outbox::built("<held@example.com>", Some("id:x"));
        let id = outbox.enqueue(&built);
        let before = outbox.row(id);
        let smtp = CountingSmtp::default();

        let held = SendAdmission::claim(ACCOUNT, "id:x").expect("free");
        outbox.resume(&smtp).await;
        assert_eq!(smtp.calls(), 0, "a draft being sent is not resubmitted");
        let row = outbox.row(id);
        assert_eq!(row.state, OutboxState::PendingSend);
        assert_eq!(row.attempts, before.attempts);
        assert_eq!(row.updated, before.updated);
        assert_eq!(row.submission_started_at, None);

        drop(held);
        outbox.resume(&smtp).await;
        assert_eq!(smtp.calls(), 1);
        assert_eq!(outbox.row(id).state, OutboxState::Done);
    }

    /// A Graph account with nothing queued but a discarded row's cleanup still
    /// builds its client on the tick, so the cleanup is carried out.
    #[test]
    fn a_graph_cleanup_alone_is_work_for_the_resume() {
        let outbox = Outbox::new();
        assert!(!graph_resume_owed(&outbox.store, ACCOUNT).unwrap());
        let id = outbox.enqueue(&Outbox::built("<cleanup@example.com>", None));
        assert!(graph_resume_owed(&outbox.store, ACCOUNT).unwrap());
        outbox::discard_graph(&outbox.store, &outbox.blobs, id).unwrap();
        assert!(
            graph_resume_owed(&outbox.store, ACCOUNT).unwrap(),
            "the cleanup is owed"
        );
    }

    /// A row built from no draft is claimed under a slot of its own, `row:<id>`.
    #[tokio::test]
    async fn the_smtp_resume_claims_a_draftless_row_by_its_id() {
        let outbox = Outbox::new();
        let built = Outbox::built("<draftless@example.com>", None);
        let id = outbox.enqueue(&built);
        let smtp = CountingSmtp::default();

        let held = SendAdmission::claim(ACCOUNT, &format!("row:{id}")).expect("free");
        outbox.resume(&smtp).await;
        assert_eq!(smtp.calls(), 0);

        drop(held);
        outbox.resume(&smtp).await;
        assert_eq!(smtp.calls(), 1);
    }

    /// A live SMTP send whose marker cannot be committed does not go out: the
    /// failure is a clean one, and the row stays submittable with no marker.
    #[tokio::test]
    async fn an_smtp_send_whose_marker_did_not_commit_sends_nothing() {
        let outbox = Outbox::new();
        let built = Outbox::built("<unmarked-smtp@example.com>", Some("id:y"));
        let row_id = outbox.enqueue(&built);
        outbox
            .store
            .conn()
            .execute_batch(
                "CREATE TRIGGER no_marker BEFORE UPDATE OF submission_started_at ON outbox
                 WHEN NEW.submission_started_at IS NOT NULL
                 BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;",
            )
            .unwrap();
        let durable = DurableSend {
            store: Store::open(outbox.dir.path().join("store.sqlite3")).unwrap(),
            blobs: BlobStore::new(outbox.dir.path().join("blobs")),
            account: crate::config::AccountConfig {
                name: ACCOUNT.to_string(),
                ..Default::default()
            },
            row_id,
        };
        let smtp = CountingSmtp::default();

        let refused = submit_durably(Some(durable), &built, &smtp_config(), &smtp)
            .await
            .err()
            .expect("the send is held back");

        assert!(
            refused.to_string().contains("nothing was sent"),
            "{refused:#}"
        );
        assert_eq!(smtp.calls(), 0);
        let row = outbox.row(row_id);
        assert_eq!(row.state, OutboxState::PendingSend);
        assert_eq!(row.submission_started_at, None);
        assert_eq!(row.attempts, 1, "a clean failure, under backoff");
    }
}
