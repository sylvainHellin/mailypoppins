//! The file half of a draft: the Markdown-with-frontmatter format, the
//! signature sentinels, the in-place frontmatter rewrites and the reply /
//! forward builders (#0126, P5-U10b).
//!
//! A draft is a file, and nothing here is anything else: it reads and writes
//! `.md` files under the drafts directory and answers about their contents.
//! The half that needs the engine - minting an id through the drafts index,
//! building a [`SourceMessage`] out of a store row, retiring a sent draft
//! against its outbox record, deleting an indexed one - stays in the root
//! crate and re-exports this module whole, so `crate::draft::…` resolves
//! unchanged on both sides of the seam.

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use colored::*;
use gray_matter::{engine::YAML, Matter};
use log::info;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::addresses::{normalize_address_for_smtp, split_addresses};
use crate::config::{EmailSettings, SmtpConfig};
use crate::parse::{
    extract_email_address, slugify_sender, slugify_subject, stable_attachments_dir,
};
use crate::types::{EmailDraft, EmailFrontmatter, EmailStatus};

/// [`new_draft_skeleton`] with the `id:` chosen by the caller, which is what
/// lets `mp new` print the selector of the draft it just wrote without reading
/// the file back.
///
/// `id:` is first so it survives an editor session that reorders nothing and a
/// human eye that reads the top of the file; it is the draft's identity under
/// #0050, and the whole point is that it is preserved rather than regenerated.
pub fn new_draft_skeleton_with_id(
    from: &str,
    date: &str,
    id: &str,
    signature: Option<&str>,
) -> String {
    // The signature (#0099) is appended to the body at creation so it is
    // visible and editable; a blank line separates it from the empty body the
    // user types into. No configured signature leaves the body empty, exactly
    // as before.
    let body = signature_block(signature).unwrap_or_default();
    format!("---\nid: {id}\nto:\ncc:\nbcc:\nsubject: \"\"\nstatus: draft\nfrom: {from}\ndate: {date}\nreply_to:\nattachments:\n---\n\n{body}")
}

/// The signature as it is spliced into a draft body: the trimmed Markdown
/// wrapped in the sentinel comments, or `None` when nothing is configured
/// (#0099, #0106). Callers control the leading blank line.
fn signature_block(signature: Option<&str>) -> Option<String> {
    signature.and_then(wrap_signature_block)
}

/// The sentinel comment that marks the first line of a spliced signature block
/// (#0106). HTML comments are passed through by CommonMark, so both send paths
/// and the TUI preview strip these lines before the recipient or the user ever
/// sees them.
pub const SIG_START: &str = "<!-- mp:sig-start -->";
/// The sentinel comment closing a spliced signature block (#0106).
pub const SIG_END: &str = "<!-- mp:sig-end -->";

/// Wrap a resolved signature Markdown snippet in the sentinel comments so the
/// block can be located and re-spliced when the user picks a different
/// signature (#0106). Returns `None` for empty input, matching the old
/// `signature_block` contract. The result ends in one trailing newline; the
/// caller owns the leading blank line.
pub fn wrap_signature_block(signature: &str) -> Option<String> {
    let sig = signature.trim_end();
    if sig.trim().is_empty() {
        return None;
    }
    Some(format!("{SIG_START}\n{sig}\n{SIG_END}\n"))
}

/// Remove the signature sentinel comment lines (`SIG_START` / `SIG_END`) from
/// `body`, keeping the signature content between them (#0106). Called on both
/// send paths (plain and HTML) and in the TUI preview so the sentinels never
/// reach a recipient or the screen. A line is dropped only when, once trimmed,
/// it equals a sentinel exactly.
pub fn strip_signature_sentinels(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut first = true;
    for line in body.split('\n') {
        let t = line.trim();
        if t == SIG_START || t == SIG_END {
            continue;
        }
        if !first {
            out.push('\n');
        }
        out.push_str(line);
        first = false;
    }
    out
}

/// Replace (or remove) the sentinel-wrapped signature block inside
/// `body_markdown` with `new_sig_markdown`, re-splicing when the user picks a
/// different signature on a draft (#0106).
///
/// - With sentinels present, the inclusive `SIG_START..=SIG_END` region is
///   swapped for the new wrapped block, or dropped entirely when
///   `new_sig_markdown` is `None`/empty.
/// - Without sentinels (a legacy draft, or one whose block was hand-deleted),
///   a new block is inserted just before the `{{SIGNATURE}}` quote marker when
///   there is one, else appended at the end of the body.
///
/// One blank line separates the block from the surrounding text.
pub fn set_signature_block(body_markdown: &str, new_sig_markdown: Option<&str>) -> String {
    let new_block = new_sig_markdown.and_then(wrap_signature_block);

    // An existing sentinel-wrapped block: swap it out in place.
    if let (Some(start), Some(end)) = (body_markdown.find(SIG_START), body_markdown.rfind(SIG_END))
    {
        if end >= start {
            let end = end + SIG_END.len();
            let before = &body_markdown[..start];
            let after = &body_markdown[end..];
            return splice_block(before, new_block.as_deref(), after);
        }
    }

    // No sentinels. Nothing to remove; a `None` leaves the body untouched.
    let Some(block) = new_block else {
        return body_markdown.to_string();
    };

    // Insert before the quote marker when this is a reply/forward draft.
    if let Some(marker) = body_markdown.find("{{SIGNATURE}}") {
        let before = &body_markdown[..marker];
        let after = &body_markdown[marker..];
        return splice_block(before, Some(&block), after);
    }

    // Plain draft: append at the end.
    splice_block(body_markdown, Some(&block), "")
}

/// Join `before`, an optional signature `block`, and `after`, keeping one blank
/// line on either side of the block and collapsing the gap when the block is
/// removed. `block`, when present, is the sentinel-wrapped form ending in a
/// newline.
fn splice_block(before: &str, block: Option<&str>, after: &str) -> String {
    let before = before.trim_end_matches('\n');
    let after = after.trim_start_matches('\n');
    let mut out = String::new();
    out.push_str(before);
    match block {
        Some(block) => {
            let block = block.trim_matches('\n');
            if !before.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(block);
            if !after.is_empty() {
                out.push_str("\n\n");
                out.push_str(after);
            } else {
                out.push('\n');
            }
        }
        None => {
            if !before.is_empty() && !after.is_empty() {
                out.push_str("\n\n");
                out.push_str(after);
            } else if !after.is_empty() {
                out.push_str(after);
            } else if !before.is_empty() {
                out.push('\n');
            }
        }
    }
    out
}

/// Re-splice the signature block of an existing draft in place and record the
/// selected signature name in frontmatter (#0106).
///
/// The body is rewritten via [`set_signature_block`] and the `signature:`
/// frontmatter key is set to `sig_name` (or cleared when `None`, meaning the
/// account default). The `.md`'s other frontmatter fields are preserved
/// byte-for-byte the way [`rewrite_draft_recipients`] preserves them, and the
/// file is only written on the success path.
pub fn rewrite_draft_signature(
    path: &Path,
    new_sig_markdown: Option<&str>,
    sig_name: Option<&str>,
) -> Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;
    let newline = if content.contains("\r\n") { "\r\n" } else { "\n" };

    let after_open = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or_else(|| anyhow!("No frontmatter found (file does not start with '---')"))?;

    // Split the frontmatter (kept verbatim) from the body at the closing fence.
    let mut header_end: Option<usize> = None;
    let mut body = String::new();
    let mut cursor = 0usize;
    while cursor < after_open.len() {
        let rest = &after_open[cursor..];
        let (line, advance) = match rest.find('\n') {
            Some(nl) => (&rest[..nl], nl + 1),
            None => (rest, rest.len()),
        };
        if line.trim_end_matches('\r') == "---" {
            header_end = Some(cursor);
            body = after_open[cursor + advance..].to_string();
            break;
        }
        cursor += advance;
    }
    let header_end =
        header_end.ok_or_else(|| anyhow!("Malformed frontmatter: no closing '---' fence"))?;
    let header = &after_open[..header_end];

    let new_body = set_signature_block(body.trim(), new_sig_markdown);

    // Reassemble the document with the frontmatter intact and one blank line
    // between the fence and the body, the shape the draft builders emit.
    let mut rebuilt = String::new();
    rebuilt.push_str("---");
    rebuilt.push_str(newline);
    rebuilt.push_str(header);
    rebuilt.push_str("---");
    rebuilt.push_str(newline);
    rebuilt.push_str(newline);
    rebuilt.push_str(&new_body);
    if !new_body.ends_with('\n') {
        rebuilt.push_str(newline);
    }

    let updates = match sig_name {
        Some(name) => vec![("signature", FieldWrite::Set(yaml_dq_escape(name)))],
        None => vec![("signature", FieldWrite::ClearIfPresent)],
    };
    let final_content = rewrite_frontmatter_scalars(&rebuilt, &updates)?;

    write_atomic(path, final_content.as_bytes())
        .with_context(|| format!("Failed to write file: {}", path.display()))
}

/// Write an `id:` into an existing draft's frontmatter, in place, preserving
/// the body and every other field byte for byte.
///
/// This is the drafts index's only write to a draft file: a file an agent
/// created without an `id:` is assigned one on the first refresh, so that the
/// selector it is listed under is the selector the file itself carries.
/// The id is written double-quoted (#0083): minted ids start with a letter, so
/// nothing we write today could be misread as a YAML number, but the quoting
/// makes the round-trip shape-stable regardless of what the id contains rather
/// than resting on that property of the minter.
pub fn set_draft_id(path: &Path, id: &str) -> Result<()> {
    rewrite_frontmatter_scalars_at(path, &[("id", FieldWrite::Set(yaml_dq_escape(id)))])
}

/// Which draft a [`SourceMessage`] is turned into.
///
/// It lives beside the source rather than beside the builder that consumes it
/// (`create_draft_from_source`, which needs an index and stayed in the root
/// crate): the compose wizard names the kind before anything is built, and
/// since #0126 (P5-U10f) that wizard is in a crate that links no engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftFromSource {
    Reply { all: bool },
    Forward,
}

/// The message a reply or a forward is built from, independent of where it
/// came from.
///
/// #0050 is why this type exists: received mail is a store row now, not a
/// `.md` file, so the draft builders cannot start by reading a path. They take
/// this instead: [`source_from_row`] is the one way to build it, off a store
/// row and its blobs, for the CLI and the TUI alike (#0052).
#[derive(Debug, Clone, Default)]
pub struct SourceMessage {
    pub from: String,
    pub to: String,
    pub cc: Option<String>,
    pub subject: String,
    /// The source's `Message-ID`, carried into the draft's `in_reply_to:` or
    /// `forwarded_from:` so the post-send hook can find the row to flag
    /// (#TKT-0051). `None` for a server-search hit that carried no header,
    /// which simply means nothing is flagged when that draft goes out.
    pub message_id: Option<String>,
    /// The `Date:` header verbatim, used in the attribution line.
    pub date: Option<String>,
    /// Plain-text body of the source message.
    pub body: String,
    /// Absolute paths of the attachments a forward should carry.
    pub attachments: Vec<PathBuf>,
    /// Rendered HTML of the source, when one exists, for the draft's companion
    /// `.html` (the quoted block the send path inlines).
    pub html: Option<String>,
    /// The `Reply-To:` header, when the message carried one: the address a
    /// reply goes to instead of `from`.
    pub reply_to: Option<String>,
}

/// Build the source of a reply or a forward out of a message that was fetched
/// from the server but never ingested: the server-search hit that resolved to
/// no local row (#0052).
///
/// The same shape [`source_from_row`] produces, with the bytes coming from the
/// fetch instead of a blob, so a reply to a hit quotes what the overlay just
/// showed. A hit with no Message-ID has no stable key for its attachments, so
/// it forwards its body without them rather than inventing one; the forwarded
/// header block still names the message.
pub fn source_from_fetched(
    account_dir: &Path,
    fetched: &crate::parse::FetchedEmail,
    with_attachments: bool,
) -> Result<SourceMessage> {
    let attachments = match (with_attachments, fetched.message_id.as_deref()) {
        (true, Some(message_id)) if !fetched.attachments.is_empty() => {
            let dest = stable_attachments_dir(account_dir, message_id);
            crate::config::create_private_dir_all(&dest)
                .with_context(|| format!("creating {}", dest.display()))?;
            let mut written = Vec::new();
            for att in &fetched.attachments {
                let out = dest.join(crate::parse::sanitize_attachment_filename(&att.filename));
                fs::write(&out, &att.content)
                    .with_context(|| format!("writing {}", out.display()))?;
                written.push(out);
            }
            written
        }
        _ => Vec::new(),
    };
    Ok(SourceMessage {
        from: fetched.from.clone(),
        to: fetched.to.clone(),
        cc: fetched.cc.clone(),
        subject: fetched.subject.clone(),
        message_id: fetched.message_id.clone(),
        date: Some(fetched.date.clone()),
        body: fetched.body_text.trim().to_string(),
        attachments,
        html: fetched.html_body.clone(),
        reply_to: fetched.reply_to.clone(),
    })
}

/// The `Fwd:`-prefixed subject of a forward, idempotent on a subject that
/// already carries the prefix.
///
/// Shared with the compose wizard, which shows the subject before the draft
/// exists and must show the one the draft will carry.
pub fn fwd_subject(subject: &str) -> String {
    if subject.to_lowercase().starts_with("fwd: ") {
        subject.to_string()
    } else {
        format!("Fwd: {subject}")
    }
}

/// The source `Message-ID` a draft may quote back, YAML-safe.
///
/// An empty header, or one carrying a double quote or a backslash, is dropped
/// rather than escaped: the value exists to be looked up in the
/// `messages_message_id` index after a send, and a Message-ID containing
/// either character is not one any server issued.
///
/// Both characters matter because the value is written into a YAML
/// double-quoted scalar. A backslash is an escape there, so `<a\b@x>` would be
/// read back mangled and `<a\qb@x>` would fail the whole draft's parse, which
/// is the silent skip #0064 called out: the reply disappears from the drafts
/// index with only a log line.
///
/// A control character (a newline smuggled in through an RFC 2047 encoded
/// header) is dropped the same way: no server issues one either.
fn source_message_id(source: &SourceMessage) -> Option<&str> {
    source
        .message_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty() && !id.chars().any(|c| c == '"' || c == '\\' || c.is_control()))
}

/// Reply to a message that is not a file: the #0050 path, used by
/// `mp reply <selector>` over a store row.
pub fn create_reply_draft_from(
    source: &SourceMessage,
    reply_all: bool,
    default_from: &str,
    drafts_dir: Option<&Path>,
    signature: Option<&str>,
) -> Result<PathBuf> {
    let inbox = source;
    let original_body = inbox.body.trim();

    // Build reply fields. A `Reply-To:` header names where the sender wants
    // answers to go (a list, a team alias behind a `noreply@` From), so it is
    // the primary recipient; `From:` is the fallback. The header may name
    // several mailboxes, and every one of them is a primary recipient.
    let from_addr = extract_email_address(&inbox.from);
    let mut primaries: Vec<String> = Vec::new();
    for addr in inbox.reply_to.as_deref().map(split_addresses).unwrap_or_default() {
        let email = extract_email_address(&addr);
        let lower = email.to_lowercase();
        if !email.is_empty() && !primaries.iter().any(|p| p.to_lowercase() == lower) {
            primaries.push(email);
        }
    }
    if primaries.is_empty() {
        primaries.push(from_addr.clone());
    }
    let reply_to = primaries.join(", ");

    let reply_cc = if reply_all {
        // `default_from` may be a full `"Name" <addr>` mailbox.
        let self_addr = extract_email_address(default_from).to_lowercase();
        let primary: Vec<String> = primaries.iter().map(|p| p.to_lowercase()).collect();
        let mut all_recipients: Vec<String> = Vec::new();
        let push = |email: String, list: &mut Vec<String>| {
            let lower = email.to_lowercase();
            if !email.is_empty()
                && lower != self_addr
                && !primary.contains(&lower)
                && !list.iter().any(|r| r.to_lowercase() == lower)
            {
                list.push(email);
            }
        };
        // With a Reply-To in charge, the original sender is still part of the
        // conversation a reply-all keeps; `push` drops it when it is the
        // Reply-To address itself, so it is never both `to:` and `cc:`.
        push(from_addr.clone(), &mut all_recipients);
        for addr in split_addresses(&inbox.to) {
            push(extract_email_address(&addr), &mut all_recipients);
        }
        if let Some(ref cc) = inbox.cc {
            for addr in split_addresses(cc) {
                push(extract_email_address(&addr), &mut all_recipients);
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
    fm.push_str(&format!("from: {}\n", yaml_dq_escape(default_from)));
    fm.push_str(&format!("to: {}\n", yaml_dq_escape(&reply_to)));
    if let Some(ref cc) = reply_cc {
        fm.push_str(&format!("cc: {}\n", yaml_dq_escape(cc)));
    }
    fm.push_str(&format!("subject: {}\n", yaml_dq_escape(&reply_subject)));
    fm.push_str("status: draft\n");
    // What the post-send hook flags `\Answered` (#TKT-0051). Written here
    // rather than acted on here: a draft that is never sent has answered
    // nothing.
    if let Some(message_id) = source_message_id(inbox) {
        fm.push_str(&format!("in_reply_to: \"{message_id}\"\n"));
    }
    fm.push_str("---\n");

    // Compose full content with the {{SIGNATURE}} placeholder between the
    // reply area and the quoted text. The placeholder is load-bearing: the
    // send path splits on it to splice the companion rich-HTML quote, so it
    // stays even when the account has a signature. The per-account signature
    // (#0099) is appended to the reply area above the placeholder, so it is
    // visible and editable in the draft; the send-time signature injection is
    // then off (empty), which is why there is no double signature.
    let sig_block = signature.and_then(wrap_signature_block);
    let full_content = match sig_block {
        Some(block) => format!(
            "{}\n\n\n{}\n{{{{SIGNATURE}}}}\n\n{}\n{}\n",
            fm.trim_end(),
            block,
            attribution,
            quoted_body
        ),
        None => format!(
            "{}\n\n\n{{{{SIGNATURE}}}}\n\n{}\n{}\n",
            fm.trim_end(),
            attribution,
            quoted_body
        ),
    };

    // Determine output path
    let output_dir = drafts_dir.unwrap_or_else(|| Path::new("."));
    if let Some(dir) = drafts_dir {
        crate::config::create_private_dir_all(dir)?;
    }
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
    {
        if let Some(html_content) = inbox.html.clone() {
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

/// Forward a message that is not a file: the #0050 path, used by
/// `mp forward <selector>` over a store row.
pub fn create_forward_draft_from(
    source: &SourceMessage,
    default_from: &str,
    drafts_dir: Option<&Path>,
    signature: Option<&str>,
) -> Result<PathBuf> {
    let inbox = source;
    let original_body = inbox.body.trim();

    // Build forward subject
    let fwd_subject = fwd_subject(&inbox.subject);

    let attachment_paths: Vec<String> = inbox
        .attachments
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    // Build frontmatter
    let mut fm = String::from("---\n");
    fm.push_str(&format!("from: {}\n", yaml_dq_escape(default_from)));
    fm.push_str("to: \"\"\n");
    fm.push_str(&format!("subject: {}\n", yaml_dq_escape(&fwd_subject)));
    fm.push_str("status: draft\n");
    // The forward half of the same hook: `$Forwarded` on send (#TKT-0051).
    if let Some(message_id) = source_message_id(inbox) {
        fm.push_str(&format!("forwarded_from: \"{message_id}\"\n"));
    }
    if !attachment_paths.is_empty() {
        fm.push_str("attachments:\n");
        for path in &attachment_paths {
            fm.push_str(&format!("  - {}\n", yaml_dq_escape(path)));
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

    // See create_reply_draft_from: the placeholder stays for quote splicing,
    // the per-account signature (#0099) goes above it in the reply area.
    let sig_block = signature.and_then(wrap_signature_block);
    let full_content = match sig_block {
        Some(block) => format!(
            "{}\n\n\n{}\n{{{{SIGNATURE}}}}\n\n{}\n\n{}\n",
            fm.trim_end(),
            block,
            fwd_header,
            clean_body.trim()
        ),
        None => format!(
            "{}\n\n\n{{{{SIGNATURE}}}}\n\n{}\n\n{}\n",
            fm.trim_end(),
            fwd_header,
            clean_body.trim()
        ),
    };

    // Determine output path
    let output_dir = drafts_dir.unwrap_or_else(|| Path::new("."));
    if let Some(dir) = drafts_dir {
        crate::config::create_private_dir_all(dir)?;
    }
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
    {
        if let Some(html_content) = inbox.html.clone() {
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

/// New recipient/subject values for an in-place draft frontmatter rewrite.
/// Empty strings clear the corresponding optional field (to/cc/bcc).
pub struct DraftRecipientEdit {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
}

/// Rewrite ONLY the `to`/`cc`/`bcc`/`subject` frontmatter fields of an
/// existing draft file in place, preserving the body and every other
/// frontmatter field byte-for-byte.
///
/// The file must begin with a `---` fence and contain a closing `---`.
/// Files with missing/malformed frontmatter are rejected with an error so
/// the caller can surface a status message without any data loss (the file
/// is only written on the success path).
///
/// Existing `to:`/`cc:`/`bcc:`/`subject:` lines are replaced where present;
/// missing recipient/subject keys are appended just before the closing
/// fence. Empty recipient values are written as a bare key (e.g. `cc:`),
/// matching the new-draft skeleton convention.
pub fn rewrite_draft_recipients(path: &Path, edit: &DraftRecipientEdit) -> Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    // Detect the dominant line ending so we can re-emit the frontmatter with
    // the same style the file already uses (avoids mixed CRLF/LF endings).
    let newline = if content.contains("\r\n") { "\r\n" } else { "\n" };

    // Split off the opening fence without touching the body bytes.
    let after_open = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or_else(|| anyhow!("No frontmatter found (file does not start with '---')"))?;

    // Collect frontmatter lines up to the closing fence (a line that is
    // exactly `---`). Everything after that newline is the untouched body.
    let mut fm_lines: Vec<String> = Vec::new();
    let mut body = String::new();
    let mut closed = false;
    let mut cursor = 0usize;
    while cursor < after_open.len() {
        let rest = &after_open[cursor..];
        let (line, advance) = match rest.find('\n') {
            Some(nl) => (&rest[..nl], nl + 1),
            None => (rest, rest.len()),
        };
        let trimmed = line.trim_end_matches('\r');
        if trimmed == "---" {
            closed = true;
            body = after_open[cursor + advance..].to_string();
            break;
        }
        fm_lines.push(trimmed.to_string());
        cursor += advance;
    }
    if !closed {
        return Err(anyhow!("Malformed frontmatter: no closing '---' fence"));
    }

    let to_line = if edit.to.trim().is_empty() {
        "to:".to_string()
    } else {
        format!("to: {}", yaml_dq_escape(&edit.to))
    };
    let cc_line = if edit.cc.trim().is_empty() {
        "cc:".to_string()
    } else {
        format!("cc: {}", yaml_dq_escape(&edit.cc))
    };
    let bcc_line = if edit.bcc.trim().is_empty() {
        "bcc:".to_string()
    } else {
        format!("bcc: {}", yaml_dq_escape(&edit.bcc))
    };
    let subject_line = format!("subject: {}", yaml_dq_escape(&edit.subject));

    // Rebuild the frontmatter line list, replacing the four managed keys in
    // place (only when they appear at top level, i.e. zero indentation) and
    // dropping any continuation lines belonging to a replaced multiline
    // scalar. Every other top-level entry — including its own continuation
    // block — is copied verbatim.
    let mut out_lines: Vec<String> = Vec::new();
    let mut wrote_to = false;
    let mut wrote_cc = false;
    let mut wrote_bcc = false;
    let mut wrote_subject = false;
    let mut i = 0usize;
    while i < fm_lines.len() {
        let line = &fm_lines[i];
        // A top-level key line has no leading whitespace. Indented lines are
        // continuation lines of the previous entry (block scalars, nested
        // mappings, flow continuations) and are never matched as managed
        // keys — they are copied verbatim below.
        let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');
        let managed: Option<&str> = if is_top_level {
            if line == "to:" || line.starts_with("to: ") {
                Some("to")
            } else if line == "cc:" || line.starts_with("cc: ") {
                Some("cc")
            } else if line == "bcc:" || line.starts_with("bcc: ") {
                Some("bcc")
            } else if line == "subject:" || line.starts_with("subject: ") {
                Some("subject")
            } else {
                None
            }
        } else {
            None
        };

        match managed {
            Some(key) => {
                match key {
                    "to" => {
                        out_lines.push(to_line.clone());
                        wrote_to = true;
                    }
                    "cc" => {
                        out_lines.push(cc_line.clone());
                        wrote_cc = true;
                    }
                    "bcc" => {
                        out_lines.push(bcc_line.clone());
                        wrote_bcc = true;
                    }
                    _ => {
                        out_lines.push(subject_line.clone());
                        wrote_subject = true;
                    }
                }
                // Skip the replaced key line and every continuation line that
                // follows it (any more-indented line): folded `>`, literal
                // `|`, indent-continued plain scalars, and block
                // sequences/mappings nested under the key.
                //
                // Blank or whitespace-only lines are legal *inside* a block
                // scalar (they separate paragraphs), so they must be consumed
                // too — but only when a further indented line follows them.
                // A trailing blank line that precedes the next top-level key
                // belongs to the document, not the scalar, and must survive.
                i += 1;
                while i < fm_lines.len() {
                    let cont = &fm_lines[i];
                    if cont.starts_with(' ') || cont.starts_with('\t') {
                        i += 1;
                    } else if cont.trim().is_empty() {
                        // Blank line: only consumable if a later indented line
                        // continues the scalar block. Look ahead past any run
                        // of blank lines to find the first non-blank line.
                        let mut j = i + 1;
                        while j < fm_lines.len() && fm_lines[j].trim().is_empty() {
                            j += 1;
                        }
                        let continues = j < fm_lines.len()
                            && (fm_lines[j].starts_with(' ') || fm_lines[j].starts_with('\t'));
                        if continues {
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
            None => {
                out_lines.push(line.clone());
                i += 1;
            }
        }
    }

    if !wrote_to {
        out_lines.push(to_line);
    }
    if !wrote_cc {
        out_lines.push(cc_line);
    }
    if !wrote_bcc {
        out_lines.push(bcc_line);
    }
    if !wrote_subject {
        out_lines.push(subject_line);
    }

    let mut rebuilt = String::new();
    rebuilt.push_str("---");
    rebuilt.push_str(newline);
    for line in out_lines {
        rebuilt.push_str(&line);
        rebuilt.push_str(newline);
    }
    rebuilt.push_str("---");
    rebuilt.push_str(newline);
    rebuilt.push_str(&body);

    write_atomic(path, rebuilt.as_bytes())
        .with_context(|| format!("Failed to write file: {}", path.display()))?;
    Ok(())
}

/// Append `entry` to the `attachments:` list of a draft's frontmatter in
/// place, preserving the body and every other frontmatter field
/// byte-for-byte (#0098).
///
/// A bare or already-populated top-level `attachments:` key gains one more
/// `  - "<entry>"` item after its last existing item; a draft with no
/// `attachments:` key at all gains the key and the item just before the
/// closing fence. `entry` is stored double-quoted and escaped exactly as the
/// forward builder writes attachment paths, so a `~`-relative path survives
/// verbatim to be expanded by [`crate::send`]'s `resolve_attachment_paths` at
/// send time -- the same entry a hand-edit would have produced.
///
/// The file must begin with a `---` fence and contain a closing `---`; a
/// malformed frontmatter is rejected without a write, the same guard
/// [`rewrite_draft_recipients`] applies (the file is only written on success).
pub fn append_draft_attachment(path: &Path, entry: &str) -> Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;
    let newline = if content.contains("\r\n") { "\r\n" } else { "\n" };

    let after_open = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or_else(|| anyhow!("No frontmatter found (file does not start with '---')"))?;

    // Split the frontmatter lines from the untouched body, the same way
    // `rewrite_draft_recipients` does.
    let mut fm_lines: Vec<String> = Vec::new();
    let mut body = String::new();
    let mut closed = false;
    let mut cursor = 0usize;
    while cursor < after_open.len() {
        let rest = &after_open[cursor..];
        let (line, advance) = match rest.find('\n') {
            Some(nl) => (&rest[..nl], nl + 1),
            None => (rest, rest.len()),
        };
        let trimmed = line.trim_end_matches('\r');
        if trimmed == "---" {
            closed = true;
            body = after_open[cursor + advance..].to_string();
            break;
        }
        fm_lines.push(trimmed.to_string());
        cursor += advance;
    }
    if !closed {
        return Err(anyhow!("Malformed frontmatter: no closing '---' fence"));
    }

    let item = format!("  - {}", yaml_dq_escape(entry));

    // The top-level `attachments:` key, if any. An indented line is a
    // continuation (one of the list items) and is never matched as the key.
    let key_idx = fm_lines.iter().position(|line| {
        let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');
        is_top_level && line.starts_with("attachments:")
    });

    match key_idx {
        Some(key_idx) => {
            // A flow-style value (`attachments: []`, `attachments: ["/a"]`)
            // cannot take a block item: inserting `  - "x"` after it would
            // produce YAML `parse_email_draft` rejects. Drafts are hand- and
            // agent-edited files, so the shape is reachable; refuse without a
            // write rather than corrupt the draft.
            let value = fm_lines[key_idx]["attachments:".len()..].trim();
            if !value.is_empty() && !value.starts_with('#') {
                return Err(anyhow!(
                    "attachments uses an inline value ({value}); edit the draft file to the block list form first"
                ));
            }
            // Insert after the key's existing item block: the run of indented
            // continuation lines immediately following the key line.
            let mut insert_at = key_idx + 1;
            while insert_at < fm_lines.len()
                && (fm_lines[insert_at].starts_with(' ')
                    || fm_lines[insert_at].starts_with('\t'))
            {
                insert_at += 1;
            }
            fm_lines.insert(insert_at, item);
        }
        None => {
            fm_lines.push("attachments:".to_string());
            fm_lines.push(item);
        }
    }

    let mut rebuilt = String::new();
    rebuilt.push_str("---");
    rebuilt.push_str(newline);
    for line in fm_lines {
        rebuilt.push_str(&line);
        rebuilt.push_str(newline);
    }
    rebuilt.push_str("---");
    rebuilt.push_str(newline);
    rebuilt.push_str(&body);

    write_atomic(path, rebuilt.as_bytes())
        .with_context(|| format!("Failed to write file: {}", path.display()))?;
    Ok(())
}

/// Atomically overwrite `path` by writing to a `.tmp` sibling then renaming
/// over the destination. Mirrors `secrets::write_secret_file_atomic` minus the
/// fixed 0600 mode — drafts are plain files, so this preserves the permission
/// semantics of an ordinary overwrite.
///
/// A rename replaces the destination inode, so the target's permission bits
/// would otherwise be reset to whatever the umask gives the temp file: a
/// draft the user had chmod'ed to 0600 came back 0644 after every status
/// change. When the target exists, its mode is copied onto the temp file
/// *before* the payload is written, so the content is never briefly readable
/// under wider permissions. A new file keeps default umask behaviour.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;

    #[cfg(unix)]
    let existing_mode = {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o7777)
    };

    // Use a PID-qualified extension so we never collide with (and clobber) a
    // real user file that happens to be named `<draft>.tmp`.
    let tmp = path.with_extension(format!("mp-tmp.{}", std::process::id()));
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(anyhow!(e))
                .with_context(|| format!("Failed to remove stale temp file: {}", tmp.display()))
        }
    }

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .with_context(|| format!("Failed to create temp file: {}", tmp.display()))?;
    #[cfg(unix)]
    if let Some(mode) = existing_mode {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))
            .with_context(|| {
                format!(
                    "Failed to preserve permissions ({mode:o}) on temp file: {}",
                    tmp.display()
                )
            })?;
    }
    file.write_all(data)
        .with_context(|| format!("Failed to write temp file: {}", tmp.display()))?;
    file.sync_all()
        .with_context(|| format!("Failed to flush temp file: {}", tmp.display()))?;
    drop(file);

    fs::rename(&tmp, path)
        .with_context(|| format!("Failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// YAML double-quote escape for a scalar string value.
///
/// Every value it writes stays on its own line: besides `\\` and `"`, the
/// control characters are escaped (`\n`, `\r`, `\t`, `\xNN`), as are DEL, the
/// C1 controls and the Unicode line/paragraph separators YAML would fold.
/// That matters because the values are header-derived: mailparse decodes RFC
/// 2047 encoded-words, so a Subject can carry a real newline, and written raw
/// it would open new top-level keys (`bcc:`, `attachments:`) in the draft.
/// YAML 1.2 `c-printable`: the characters a YAML stream may carry raw.
fn yaml_printable(c: char) -> bool {
    matches!(c,
        '\t' | '\n' | '\r' | '\u{20}'..='\u{7e}' | '\u{85}'
        | '\u{a0}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}

fn yaml_dq_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || ('\u{7f}'..='\u{9f}').contains(&c) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            // Line separators and the BOM are printable but a reader may
            // break or strip on them.
            '\u{2028}' | '\u{2029}' | '\u{feff}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            // Anything else outside YAML 1.2 `c-printable` (U+FFFE, U+FFFF)
            // is rejected by the reader, so a raw one would make the whole
            // draft unreadable.
            c if !yaml_printable(c) => {
                let n = c as u32;
                if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// What to write for one top-level frontmatter key in
/// [`rewrite_frontmatter_scalars`].
#[derive(Debug, Clone)]
enum FieldWrite {
    /// Set the key to this raw YAML scalar (already quoted/escaped by the
    /// caller). Appended just before the closing fence when the key is
    /// absent.
    Set(String),
    /// Reset the key to a bare `key:` (deserializes to `None`) when it is
    /// present. Never appended: a key that was not there stays away, so we
    /// do not sprinkle `cc: null`-style noise into user files.
    ClearIfPresent,
}

/// Rewrite ONLY the listed top-level frontmatter keys of `content`,
/// preserving the body and every other frontmatter byte.
///
/// This is the shared write path behind the status transitions
/// ([`mark_as_approved`] and [`mark_as_draft`]; the `sent` transition went
/// with the local sent `.md` in #0037).
/// They used to parse into [`EmailFrontmatter`] and re-serialize, which
/// silently dropped every field that struct does not model -- including
/// `date:`, the key the TUI sorts on, so an approved draft teleported to
/// the bottom of the list. Line surgery keeps unknown and user-added
/// fields intact.
///
/// Matching is the same as [`rewrite_draft_recipients`]: only zero-indent
/// key lines are managed, and a replaced key's continuation lines (block
/// scalars, nested mappings) are dropped with it.
fn rewrite_frontmatter_scalars(
    content: &str,
    updates: &[(&str, FieldWrite)],
) -> Result<String> {
    let newline = if content.contains("\r\n") { "\r\n" } else { "\n" };

    let after_open = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or_else(|| anyhow!("No frontmatter found (file does not start with '---')"))?;

    // Split frontmatter lines from the untouched body at the closing fence.
    let mut fm_lines: Vec<String> = Vec::new();
    let mut body = String::new();
    let mut closed = false;
    let mut cursor = 0usize;
    while cursor < after_open.len() {
        let rest = &after_open[cursor..];
        let (line, advance) = match rest.find('\n') {
            Some(nl) => (&rest[..nl], nl + 1),
            None => (rest, rest.len()),
        };
        let trimmed = line.trim_end_matches('\r');
        if trimmed == "---" {
            closed = true;
            body = after_open[cursor + advance..].to_string();
            break;
        }
        fm_lines.push(trimmed.to_string());
        cursor += advance;
    }
    if !closed {
        return Err(anyhow!("Malformed frontmatter: no closing '---' fence"));
    }

    let mut written = vec![false; updates.len()];
    let mut out_lines: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < fm_lines.len() {
        let line = &fm_lines[i];
        // Indented lines are continuation lines of the previous entry and
        // are never matched as managed keys.
        let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');
        let managed = if is_top_level {
            updates.iter().position(|(key, _)| {
                line.len() > key.len()
                    && line.starts_with(key)
                    && line.as_bytes()[key.len()] == b':'
                    && line[key.len() + 1..]
                        .chars()
                        .next()
                        .is_none_or(|c| c == ' ')
            })
        } else {
            None
        };

        match managed {
            Some(pos) => {
                let (key, write) = &updates[pos];
                out_lines.push(match write {
                    FieldWrite::Set(value) => format!("{key}: {value}"),
                    FieldWrite::ClearIfPresent => format!("{key}:"),
                });
                written[pos] = true;
                // Skip the replaced key line and its continuation lines
                // (any more-indented line), including blank lines interior
                // to a block scalar but not a trailing one.
                i += 1;
                while i < fm_lines.len() {
                    let cont = &fm_lines[i];
                    if cont.starts_with(' ') || cont.starts_with('\t') {
                        i += 1;
                    } else if cont.trim().is_empty() {
                        let mut j = i + 1;
                        while j < fm_lines.len() && fm_lines[j].trim().is_empty() {
                            j += 1;
                        }
                        let continues = j < fm_lines.len()
                            && (fm_lines[j].starts_with(' ') || fm_lines[j].starts_with('\t'));
                        if continues {
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
            None => {
                out_lines.push(line.clone());
                i += 1;
            }
        }
    }

    for (pos, (key, write)) in updates.iter().enumerate() {
        if written[pos] {
            continue;
        }
        if let FieldWrite::Set(value) = write {
            out_lines.push(format!("{key}: {value}"));
        }
    }

    let mut rebuilt = String::new();
    rebuilt.push_str("---");
    rebuilt.push_str(newline);
    for line in out_lines {
        rebuilt.push_str(&line);
        rebuilt.push_str(newline);
    }
    rebuilt.push_str("---");
    rebuilt.push_str(newline);
    rebuilt.push_str(&body);
    Ok(rebuilt)
}

/// Read `path`, apply [`rewrite_frontmatter_scalars`], write it back
/// atomically. The file is only touched on the success path.
fn rewrite_frontmatter_scalars_at(path: &Path, updates: &[(&str, FieldWrite)]) -> Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;
    let rebuilt = rewrite_frontmatter_scalars(&content, updates)?;
    write_atomic(path, rebuilt.as_bytes())
        .with_context(|| format!("Failed to write file: {}", path.display()))
}

/// Refuse an `id:` that YAML did not read as a string, before the frontmatter
/// is deserialised (#0083).
///
/// [`crate::types::EmailFrontmatter`]'s own `id` deserialiser rejects the same
/// thing, but it cannot see all of it: `gray_matter` coerces its `Pod` through
/// `serde_json` on the way to the struct, and `id: 8808e70039225152` is a YAML
/// float whose value is infinity, which JSON has no representation for and so
/// flattens to `null` -- indistinguishable, by the time serde sees it, from a
/// bare `id:` key. That is exactly the #0077 shape that used to be silently
/// re-minted, so the check that catches it has to run on the `Pod` itself.
fn reject_non_string_id(data: &gray_matter::Pod) -> Result<()> {
    let gray_matter::Pod::Hash(map) = data else { return Ok(()) };
    match map.get("id") {
        None | Some(gray_matter::Pod::Null) | Some(gray_matter::Pod::String(_)) => Ok(()),
        Some(other) => {
            let kind = match other {
                gray_matter::Pod::Integer(_) | gray_matter::Pod::Float(_) => "a number",
                gray_matter::Pod::Boolean(_) => "a boolean",
                gray_matter::Pod::Array(_) => "a list",
                _ => "a mapping",
            };
            Err(anyhow!(
                "frontmatter 'id:' is {kind}, not a string: quote it (id: \"...\") so the draft keeps its identity"
            ))
        }
    }
}

pub fn parse_email_draft(path: &Path) -> Result<EmailDraft> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);

    let data = parsed.data.ok_or_else(|| anyhow!("No frontmatter found in file"))?;
    reject_non_string_id(&data)?;
    let frontmatter: EmailFrontmatter = data
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
    let to_empty = draft.frontmatter.to.as_deref().is_none_or(|s| s.trim().is_empty());
    let cc_empty = draft.frontmatter.cc.as_deref().is_none_or(|s| s.trim().is_empty());
    let bcc_empty = draft.frontmatter.bcc.as_deref().is_none_or(|s| s.trim().is_empty());
    if to_empty && cc_empty && bcc_empty {
        return Err(anyhow!("No recipients (to, cc, and bcc are all empty)"));
    }

    if draft.frontmatter.subject.is_empty() {
        return Err(anyhow!("Missing 'subject' field"));
    }

    if draft.body_markdown.is_empty() {
        warnings.push("Email body is empty".to_string());
    }

    // Validate email format (basic check). Display names that are not raw
    // RFC 5322 atext are auto-quoted before parsing, mirroring what
    // the send path does, so the validator does not flag addresses that we
    // know we will be able to send.
    let validate_field = |field: Option<&String>, name: &str| -> Result<()> {
        if let Some(value) = field {
            for email in split_addresses(value) {
                let normalized = normalize_address_for_smtp(&email);
                if normalized.parse::<lettre::message::Mailbox>().is_err() {
                    return Err(anyhow!(
                        "Invalid email address in '{}': {}",
                        name,
                        email
                    ));
                }
            }
        }
        Ok(())
    };
    validate_field(draft.frontmatter.to.as_ref(), "to")?;
    validate_field(draft.frontmatter.cc.as_ref(), "cc")?;
    validate_field(draft.frontmatter.bcc.as_ref(), "bcc")?;

    // Check attachments exist. A folder entry is valid as long as it holds at
    // least one file, which `send` expands into individual attachments.
    if let Some(attachments) = &draft.frontmatter.attachments {
        for attachment in attachments {
            let expanded = shellexpand::tilde(attachment);
            let path = Path::new(expanded.as_ref());
            if path.is_dir() {
                let has_file = fs::read_dir(path)
                    .ok()
                    .map(|entries| {
                        entries.flatten().any(|e| {
                            e.path().is_file()
                                && !e
                                    .file_name()
                                    .to_str()
                                    .is_some_and(|n| n.starts_with('.'))
                        })
                    })
                    .unwrap_or(false);
                if !has_file {
                    warnings.push(format!("Attachment folder is empty: {}", attachment));
                }
            } else if !path.exists() {
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

/// Mark a draft as sent, in place.
///
/// The local sent `.md` this used to write into `sent/` is gone with the rest
/// of the `.md` tree (#0037): the Sent copy is now the durable outbox's job,
/// which APPENDs it to the server and ingests it into the store. What is left
/// here is the draft's own bookkeeping, rewritten surgically in the draft's
/// bytes so `date:` and any field `EmailFrontmatter` does not model survive the
/// send.
///
/// A draft that was never written to disk (the synthetic one `mp invite`
/// builds) has nothing to mark, and says so in the log rather than
/// materialising a file nobody asked for.
pub fn mark_draft_sent(draft: &EmailDraft, message_id: Option<&str>) -> Result<()> {
    info!("Updating status to sent: {}", draft.path.display());
    let sent_at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let sent_via = format!("mailypoppins v{}", env!("CARGO_PKG_VERSION"));

    let content = match fs::read_to_string(&draft.path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::debug!(
                "No draft file at {}; nothing to mark as sent",
                draft.path.display()
            );
            return Ok(());
        }
        Err(e) => {
            return Err(anyhow::Error::from(e))
                .context(format!("reading the draft {}", draft.path.display()))
        }
    };

    let new_content = rewrite_frontmatter_scalars(
        &content,
        &[
            ("status", FieldWrite::Set(EmailStatus::Sent.to_string())),
            ("sent_at", FieldWrite::Set(sent_at)),
            ("sent_via", FieldWrite::Set(yaml_dq_escape(&sent_via))),
            (
                "message_id",
                match message_id {
                    Some(id) => FieldWrite::Set(yaml_dq_escape(id)),
                    None => FieldWrite::ClearIfPresent,
                },
            ),
        ],
    )?;
    fs::write(&draft.path, new_content)?;

    // The companion HTML only exists to carry the quoted reply into the send;
    // once sent it is dead weight next to the draft.
    let html_companion = draft.path.with_extension("html");
    if html_companion.exists() {
        fs::remove_file(&html_companion).ok();
    }

    Ok(())
}

/// Remove a draft from disk: its `.md` file and the HTML companion a reply
/// carries beside it (the same companion [`settle_sent_draft`] retires on
/// send). The caller reconciles the drafts index afterwards, the rescan
/// `mp list` already runs, so no index bookkeeping lives here.
///
/// Tolerant of a missing file, like the sent-draft retirement: a draft the
/// index still lists but whose file is already gone is a clean delete, not an
/// error.
pub fn remove_draft_files(path: &Path) -> Result<()> {
    let html_companion = path.with_extension("html");
    if html_companion.exists() {
        fs::remove_file(&html_companion).ok();
    }
    match fs::remove_file(path) {
        Ok(()) => {
            info!("Deleted the draft: {}", path.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::Error::from(e))
            .context(format!("deleting the draft {}", path.display())),
    }
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

pub fn mark_as_approved(path: &Path) -> Result<String> {
    let draft = parse_email_draft(path)?;

    if draft.frontmatter.status == EmailStatus::Approved {
        return Ok(format!("Already approved: {}", path.display()));
    }

    if draft.frontmatter.status == EmailStatus::Sent {
        return Err(anyhow!("Cannot approve an already sent email"));
    }

    // Surgical `status:` rewrite: re-serializing `EmailFrontmatter` would
    // drop every field it does not model, including the `date:` line the
    // TUI sorts on (the approved draft then jumped to the bottom of the
    // list) and any user-added key.
    rewrite_frontmatter_scalars_at(
        path,
        &[("status", FieldWrite::Set(EmailStatus::Approved.to_string()))],
    )?;

    Ok(format!("Marked as approved: {}", path.display()))
}

/// Reverse of [`mark_as_approved`] -- demote a draft back to `draft` status.
///
/// Useful when the user pressed `A` by mistake and wants to keep editing.
/// Only `approved` drafts can be demoted: `draft` is a no-op (returns an
/// `Already a draft` message), and a `sent` file is rejected with an error --
/// it has left the draft pipeline and must not be silently rewritten.
pub fn mark_as_draft(path: &Path) -> Result<String> {
    let draft = parse_email_draft(path)?;

    match draft.frontmatter.status {
        EmailStatus::Draft => return Ok(format!("Already a draft: {}", path.display())),
        EmailStatus::Approved => {}
        EmailStatus::Sent => {
            return Err(anyhow!("Cannot revert a sent email back to draft"));
        }
    }

    // Surgical `status:` rewrite, see `mark_as_approved`.
    rewrite_frontmatter_scalars_at(
        path,
        &[("status", FieldWrite::Set(EmailStatus::Draft.to_string()))],
    )?;

    Ok(format!("Marked as draft: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EmailDraft, EmailFrontmatter, EmailStatus};
    use std::path::PathBuf;
    // -----------------------------------------------------------------------
    // Signature sentinels + set_signature_block (#0106)
    // -----------------------------------------------------------------------

    #[test]
    fn wrap_signature_block_wraps_and_rejects_empty() {
        assert_eq!(
            wrap_signature_block("Best,\nAlice").as_deref(),
            Some("<!-- mp:sig-start -->\nBest,\nAlice\n<!-- mp:sig-end -->\n")
        );
        assert_eq!(wrap_signature_block("   \n  ").as_deref(), None);
        assert_eq!(wrap_signature_block("").as_deref(), None);
    }

    #[test]
    fn strip_signature_sentinels_removes_only_the_marker_lines() {
        let body = "Hi\n\n<!-- mp:sig-start -->\nBest,\nAlice\n<!-- mp:sig-end -->\n";
        assert_eq!(strip_signature_sentinels(body), "Hi\n\nBest,\nAlice\n");
        // A body with no sentinels is returned unchanged.
        let plain = "Just a note.\n\nSecond paragraph.";
        assert_eq!(strip_signature_sentinels(plain), plain);
    }

    #[test]
    fn set_signature_block_replaces_an_existing_block() {
        let body = "Hello\n\n<!-- mp:sig-start -->\nOld sig\n<!-- mp:sig-end -->\n";
        let out = set_signature_block(body, Some("New sig line 1\nNew sig line 2"));
        assert_eq!(
            out,
            "Hello\n\n<!-- mp:sig-start -->\nNew sig line 1\nNew sig line 2\n<!-- mp:sig-end -->\n"
        );
        // Round-trips: replacing again keeps a single block.
        assert_eq!(out.matches(SIG_START).count(), 1);
        assert_eq!(out.matches(SIG_END).count(), 1);
    }

    #[test]
    fn set_signature_block_removes_a_block_when_new_is_none() {
        let body = "Hello\n\n<!-- mp:sig-start -->\nOld sig\n<!-- mp:sig-end -->\n\nTrailer";
        let out = set_signature_block(body, None);
        assert_eq!(out, "Hello\n\nTrailer");
        assert!(!out.contains(SIG_START) && !out.contains(SIG_END));
    }

    #[test]
    fn set_signature_block_inserts_before_the_quote_marker_when_no_sentinels() {
        // A legacy reply draft: no sentinels, a `{{SIGNATURE}}` quote marker.
        let body = "My reply\n\n{{SIGNATURE}}\n\nOn Mon wrote:\n> Quoted";
        let out = set_signature_block(body, Some("Best,\nAlice"));
        assert_eq!(
            out,
            "My reply\n\n<!-- mp:sig-start -->\nBest,\nAlice\n<!-- mp:sig-end -->\n\n{{SIGNATURE}}\n\nOn Mon wrote:\n> Quoted"
        );
    }

    #[test]
    fn set_signature_block_appends_when_no_sentinels_and_no_marker() {
        let body = "A plain draft body.";
        let out = set_signature_block(body, Some("Best,\nAlice"));
        assert_eq!(
            out,
            "A plain draft body.\n\n<!-- mp:sig-start -->\nBest,\nAlice\n<!-- mp:sig-end -->\n"
        );
    }

    #[test]
    fn set_signature_block_none_without_sentinels_is_a_noop() {
        let body = "A plain draft body.\n";
        assert_eq!(set_signature_block(body, None), body);
    }

    fn make_draft(to: &str, subject: &str, body: &str, status: EmailStatus) -> EmailDraft {
        let to_opt = if to.is_empty() { None } else { Some(to.to_string()) };
        EmailDraft {
            path: PathBuf::from("test.md"),
            frontmatter: EmailFrontmatter {
                id: None,
                date: None,
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
                in_reply_to: None,
                forwarded_from: None,
                signature: None,
                event: None,
            },
            body_markdown: body.to_string(),
        }
    }

    #[test]
    fn test_rewrite_draft_recipients_preserves_body_and_unknown_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("draft.md");
        let original = concat!(
            "---\n",
            "to: old@example.com\n",
            "cc:\n",
            "bcc:\n",
            "subject: Old subject\n",
            "status: draft\n",
            "from: \"me@example.com\"\n",
            "date: Thu, 10 Jul 2026 08:00:00 +0000\n",
            "reply_to:\n",
            "attachments:\n",
            "custom_field: keep-me\n",
            "---\n",
            "\n",
            "Hello body line 1.\n",
            "Line 2 with --- dashes inside.\n",
        );
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "alice@example.com, bob@example.com".to_string(),
            cc: "carol@example.com".to_string(),
            bcc: String::new(),
            subject: "New subject".to_string(),
        };
        rewrite_draft_recipients(&path, &edit).unwrap();

        let result = fs::read_to_string(&path).unwrap();

        // Recipient/subject fields rewritten.
        assert!(result.contains("to: \"alice@example.com, bob@example.com\""));
        assert!(result.contains("cc: \"carol@example.com\""));
        assert!(
            result.contains("\nbcc:\n"),
            "empty bcc should be a bare key: {result}"
        );
        assert!(result.contains("subject: \"New subject\""));
        assert!(!result.contains("Old subject"));
        assert!(!result.contains("old@example.com"));

        // Unknown / unrelated frontmatter fields preserved verbatim.
        assert!(result.contains("status: draft"));
        assert!(result.contains("from: \"me@example.com\""));
        assert!(result.contains("date: Thu, 10 Jul 2026 08:00:00 +0000"));
        assert!(result.contains("reply_to:"));
        assert!(result.contains("custom_field: keep-me"));

        // Body preserved byte-for-byte (including the interior `---`).
        assert!(
            result.ends_with("\nHello body line 1.\nLine 2 with --- dashes inside.\n"),
            "body not preserved: {result:?}"
        );

        // The rewritten file still parses and reflects the new values.
        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(
            draft.frontmatter.to.as_deref(),
            Some("alice@example.com, bob@example.com")
        );
        assert_eq!(draft.frontmatter.cc.as_deref(), Some("carol@example.com"));
        assert_eq!(draft.frontmatter.bcc, None);
        assert_eq!(draft.frontmatter.subject, "New subject");
    }

    #[test]
    fn test_rewrite_draft_recipients_appends_missing_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("draft.md");
        // No cc/bcc keys present at all.
        let original = "---\nto: old@example.com\nsubject: S\nstatus: draft\n---\n\nBody.\n";
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "new@example.com".to_string(),
            cc: "cc@example.com".to_string(),
            bcc: "bcc@example.com".to_string(),
            subject: "S2".to_string(),
        };
        rewrite_draft_recipients(&path, &edit).unwrap();

        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.to.as_deref(), Some("new@example.com"));
        assert_eq!(draft.frontmatter.cc.as_deref(), Some("cc@example.com"));
        assert_eq!(draft.frontmatter.bcc.as_deref(), Some("bcc@example.com"));
        assert_eq!(draft.frontmatter.subject, "S2");
        assert_eq!(draft.body_markdown, "Body.");
    }

    #[test]
    fn test_rewrite_draft_recipients_folded_subject_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("folded.md");
        // A folded `subject: >` block spanning two indented continuation lines.
        let original = concat!(
            "---\n",
            "to: old@example.com\n",
            "subject: >\n",
            "  This is a long folded\n",
            "  subject line.\n",
            "status: draft\n",
            "custom_field: keep-me\n",
            "---\n",
            "\n",
            "Body stays.\n",
        );
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "new@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "New subject".to_string(),
        };
        rewrite_draft_recipients(&path, &edit).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        // Orphaned continuation lines of the old folded scalar must be gone.
        assert!(
            !result.contains("This is a long folded"),
            "folded continuation not consumed: {result}"
        );
        assert!(!result.contains("subject line."));
        assert!(result.contains("custom_field: keep-me"));

        // Must still parse and yield the new values with body preserved.
        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.to.as_deref(), Some("new@example.com"));
        assert_eq!(draft.frontmatter.subject, "New subject");
        assert_eq!(draft.body_markdown, "Body stays.");
    }

    #[test]
    fn test_rewrite_draft_recipients_literal_block_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("literal.md");
        // A literal `subject: |` block.
        let original = concat!(
            "---\n",
            "to: old@example.com\n",
            "subject: |\n",
            "  line one\n",
            "  line two\n",
            "status: draft\n",
            "---\n",
            "\n",
            "Body.\n",
        );
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "new@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "S2".to_string(),
        };
        rewrite_draft_recipients(&path, &edit).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(!result.contains("line one"), "literal block not consumed: {result}");
        assert!(!result.contains("line two"));

        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.subject, "S2");
        assert_eq!(draft.body_markdown, "Body.");
    }

    #[test]
    fn test_rewrite_draft_recipients_folded_subject_interior_blank_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("folded_blank.md");
        // A folded `subject: >` block with a blank line separating two
        // paragraphs. The blank line is legal YAML and is NOT indented, so a
        // naive "stop at first non-indented line" consumer would orphan the
        // second paragraph and make the frontmatter unparseable.
        let original = concat!(
            "---\n",
            "to: old@example.com\n",
            "subject: >\n",
            "  first paragraph\n",
            "\n",
            "  second paragraph\n",
            "status: draft\n",
            "---\n",
            "\n",
            "Body stays.\n",
        );
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "new@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "New subject".to_string(),
        };
        rewrite_draft_recipients(&path, &edit).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        // Neither paragraph of the old folded scalar may survive.
        assert!(
            !result.contains("first paragraph"),
            "first paragraph not consumed: {result}"
        );
        assert!(
            !result.contains("second paragraph"),
            "orphaned second paragraph after interior blank line: {result}"
        );
        assert!(result.contains("status: draft"));

        // Must still parse cleanly with body preserved.
        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.to.as_deref(), Some("new@example.com"));
        assert_eq!(draft.frontmatter.subject, "New subject");
        assert_eq!(draft.body_markdown, "Body stays.");
    }

    #[test]
    fn test_rewrite_draft_recipients_literal_to_interior_blank_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("literal_blank.md");
        // A literal `to: |` block with an interior blank line between entries.
        let original = concat!(
            "---\n",
            "to: |\n",
            "  first@example.com\n",
            "\n",
            "  second@example.com\n",
            "subject: Old\n",
            "status: draft\n",
            "---\n",
            "\n",
            "Body.\n",
        );
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "new@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "New".to_string(),
        };
        rewrite_draft_recipients(&path, &edit).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(
            !result.contains("first@example.com"),
            "first literal line not consumed: {result}"
        );
        assert!(
            !result.contains("second@example.com"),
            "orphaned literal line after interior blank line: {result}"
        );

        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.to.as_deref(), Some("new@example.com"));
        assert_eq!(draft.frontmatter.subject, "New");
        assert_eq!(draft.body_markdown, "Body.");
    }

    #[test]
    fn test_rewrite_draft_recipients_block_scalar_trailing_blank_before_next_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trailing_blank.md");
        // A managed block scalar followed by a blank line and then a NEXT
        // top-level key. The trailing blank belongs to the document, not the
        // scalar, and the next key must survive verbatim.
        let original = concat!(
            "---\n",
            "subject: >\n",
            "  folded line one\n",
            "  folded line two\n",
            "\n",
            "custom_field: keep-me\n",
            "status: draft\n",
            "---\n",
            "\n",
            "Body.\n",
        );
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "new@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "New".to_string(),
        };
        rewrite_draft_recipients(&path, &edit).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(!result.contains("folded line one"), "scalar not consumed: {result}");
        assert!(!result.contains("folded line two"));
        // The next top-level key survives verbatim.
        assert!(
            result.contains("custom_field: keep-me"),
            "next top-level key was swallowed: {result}"
        );

        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.subject, "New");
        assert_eq!(draft.body_markdown, "Body.");
    }

    #[test]
    fn test_rewrite_draft_recipients_nested_key_not_matched() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested.md");
        // A nested `to:` under `meta:` must NOT be treated as the top-level
        // recipient key, and must not produce duplicate top-level keys.
        let original = concat!(
            "---\n",
            "to: old@example.com\n",
            "subject: Old\n",
            "meta:\n",
            "  to: nested@example.com\n",
            "status: draft\n",
            "---\n",
            "\n",
            "Body.\n",
        );
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "new@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "New".to_string(),
        };
        rewrite_draft_recipients(&path, &edit).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        // The nested key is preserved verbatim under `meta:`.
        assert!(
            result.contains("meta:\n  to: nested@example.com"),
            "nested key mangled: {result}"
        );
        // Exactly one top-level `to:` line (the rewritten one).
        let top_level_to = result
            .lines()
            .filter(|l| l.starts_with("to: ") || *l == "to:")
            .count();
        assert_eq!(top_level_to, 1, "duplicate top-level to keys: {result}");

        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.to.as_deref(), Some("new@example.com"));
        assert_eq!(draft.frontmatter.subject, "New");
    }

    #[test]
    fn test_rewrite_draft_recipients_preserves_crlf() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("crlf.md");
        let original = concat!(
            "---\r\n",
            "to: old@example.com\r\n",
            "subject: Old\r\n",
            "status: draft\r\n",
            "---\r\n",
            "\r\n",
            "Body with CRLF.\r\n",
        );
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "new@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "New".to_string(),
        };
        rewrite_draft_recipients(&path, &edit).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        // Frontmatter lines must use CRLF, not bare LF (no mixed endings).
        assert!(result.contains("to: \"new@example.com\"\r\n"), "frontmatter not CRLF: {result:?}");
        assert!(
            !result.lines().any(|l| l.starts_with("to:") && !result.contains(&format!("{l}\r"))),
            "mixed endings: {result:?}"
        );
        // No lone LF that isn't part of a CRLF pair.
        assert!(!result.replace("\r\n", "").contains('\n'), "stray LF found: {result:?}");

        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.to.as_deref(), Some("new@example.com"));
        assert_eq!(draft.frontmatter.subject, "New");
    }

    #[test]
    fn test_rewrite_draft_recipients_duplicate_key_does_not_multiply() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("dup.md");
        // Two top-level `to:` lines (malformed input). Both are managed keys
        // and get replaced; the result must not multiply the count.
        let original = concat!(
            "---\n",
            "to: a@example.com\n",
            "to: b@example.com\n",
            "subject: Old\n",
            "status: draft\n",
            "---\n",
            "\n",
            "Body.\n",
        );
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "new@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "New".to_string(),
        };
        rewrite_draft_recipients(&path, &edit).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        let to_count = result
            .lines()
            .filter(|l| l.starts_with("to: ") || *l == "to:")
            .count();
        // Both duplicate keys are replaced in place; the count is unchanged
        // (2), never multiplied. (Duplicate top-level keys are malformed YAML
        // to begin with, so this only guards against the rewriter making the
        // situation worse by appending extra copies.)
        assert_eq!(to_count, 2, "to keys multiplied: {result}");
        // Both were rewritten to the new value.
        assert!(!result.contains("a@example.com"), "stale value: {result}");
        assert!(!result.contains("b@example.com"), "stale value: {result}");
        assert_eq!(
            result.matches("to: \"new@example.com\"").count(),
            2,
            "both duplicates should hold the new value: {result}"
        );
    }

    #[test]
    fn test_rewrite_draft_recipients_errors_on_missing_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nofm.md");
        let original = "Just a body, no frontmatter fence.\n";
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "x@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "S".to_string(),
        };
        let result = rewrite_draft_recipients(&path, &edit);
        assert!(result.is_err());
        // File must be left untouched on error (no data loss).
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn test_rewrite_draft_recipients_errors_on_unclosed_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("unclosed.md");
        let original = "---\nto: a@x.com\nsubject: S\nno closing fence here\n";
        fs::write(&path, original).unwrap();

        let edit = DraftRecipientEdit {
            to: "x@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "S".to_string(),
        };
        let result = rewrite_draft_recipients(&path, &edit);
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    // -- append_draft_attachment (#0098) ---------------------------------

    /// A hand-written flow-style value (`attachments: []`) is refused without
    /// a write: a block item inserted after it would produce YAML the parser
    /// rejects, and drafts are hand- and agent-edited files, so the shape is
    /// reachable.
    #[test]
    fn test_append_attachment_refuses_a_flow_style_value() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("draft.md");
        let original =
            "---\nto: a@x.com\nsubject: S\nstatus: draft\nattachments: []\n---\n\nBody.\n";
        fs::write(&path, original).unwrap();

        let err = append_draft_attachment(&path, "/tmp/a.pdf").unwrap_err();
        assert!(format!("{err:#}").contains("inline value"), "{err:#}");
        assert_eq!(fs::read_to_string(&path).unwrap(), original, "no write on refusal");
    }

    /// A bare `attachments:` key (the new-draft skeleton) gains the first item
    /// under it, and the draft then parses with the attachment listed. The
    /// body and every other field survive.
    #[test]
    fn test_append_attachment_to_a_bare_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("draft.md");
        let original =
            "---\nto: a@x.com\nsubject: S\nstatus: draft\nattachments:\n---\n\nBody.\n";
        fs::write(&path, original).unwrap();

        append_draft_attachment(&path, "~/Documents/report.pdf").unwrap();

        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(
            draft.frontmatter.attachments.as_deref(),
            Some(["~/Documents/report.pdf".to_string()].as_slice())
        );
        assert_eq!(draft.body_markdown, "Body.");
        assert_eq!(draft.frontmatter.subject, "S");
    }

    /// A second attach appends after the existing item rather than replacing
    /// it, so both paths reach the send path; the `~` in each is stored
    /// verbatim for `send` to expand later.
    #[test]
    fn test_append_attachment_after_existing_items() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("draft.md");
        let original = concat!(
            "---\n",
            "to: a@x.com\n",
            "subject: S\n",
            "attachments:\n",
            "  - \"/tmp/one.pdf\"\n",
            "status: draft\n",
            "---\n\nBody.\n",
        );
        fs::write(&path, original).unwrap();

        append_draft_attachment(&path, "/tmp/two.pdf").unwrap();

        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(
            draft.frontmatter.attachments.as_deref(),
            Some(["/tmp/one.pdf".to_string(), "/tmp/two.pdf".to_string()].as_slice())
        );
        // The `status:` key that followed the list is untouched.
        assert_eq!(draft.frontmatter.status, EmailStatus::Draft);
        assert_eq!(draft.body_markdown, "Body.");
    }

    /// A draft with no `attachments:` key at all gains the key and the item
    /// just before the closing fence.
    #[test]
    fn test_append_attachment_adds_the_key_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("draft.md");
        let original = "---\nto: a@x.com\nsubject: S\nstatus: draft\n---\n\nBody.\n";
        fs::write(&path, original).unwrap();

        append_draft_attachment(&path, "/tmp/one.pdf").unwrap();

        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(
            draft.frontmatter.attachments.as_deref(),
            Some(["/tmp/one.pdf".to_string()].as_slice())
        );
        assert_eq!(draft.frontmatter.to.as_deref(), Some("a@x.com"));
        assert_eq!(draft.body_markdown, "Body.");
    }

    /// A path with a double-quote is escaped, so the YAML stays parseable and
    /// the entry round-trips byte-for-byte.
    #[test]
    fn test_append_attachment_escapes_quotes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("draft.md");
        let original =
            "---\nto: a@x.com\nsubject: S\nstatus: draft\nattachments:\n---\n\nBody.\n";
        fs::write(&path, original).unwrap();

        append_draft_attachment(&path, "/tmp/wei\"rd.pdf").unwrap();

        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(
            draft.frontmatter.attachments.as_deref(),
            Some(["/tmp/wei\"rd.pdf".to_string()].as_slice())
        );
    }

    /// Malformed frontmatter is rejected without a write (the file is only
    /// written on the success path), the same guard the recipient rewrite has.
    #[test]
    fn test_append_attachment_errors_on_missing_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nofm.md");
        let original = "no frontmatter here\n";
        fs::write(&path, original).unwrap();

        let result = append_draft_attachment(&path, "/tmp/one.pdf");
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
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

    /// A draft the index still lists but whose file is already gone deletes
    /// cleanly rather than erroring (#0073), like the sent-draft retirement.
    #[test]
    fn removing_an_already_gone_draft_is_not_an_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("gone.md");
        remove_draft_files(&missing).unwrap();
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

    // -----------------------------------------------------------------------
    // Status transitions are line surgery, not a frontmatter round-trip
    //
    // These used to parse into `EmailFrontmatter` and re-serialize, which
    // silently dropped every key that struct does not model. Losing `date:`
    // made the TUI fall back to the filename (unparseable for
    // `draft-%Y%m%d-%H%M%S.md`), so `date_sort` went empty and the approved
    // row teleported to the bottom of the list.
    // -----------------------------------------------------------------------

    /// A draft carrying `date:` plus a field no struct models.
    fn draft_with_unknown_fields(dir: &Path, status: &str) -> PathBuf {
        let path = dir.join("draft-20260701-120000-hello.md");
        fs::write(
            &path,
            format!(
                concat!(
                    "---\n",
                    "to: alice@example.com\n",
                    "subject: Hello\n",
                    "status: {}\n",
                    "from: me@example.com\n",
                    "date: 2026-07-01T12:00:00+02:00\n",
                    "x-ticket: PROJ-42\n",
                    "tags:\n",
                    "  - urgent\n",
                    "  - billing\n",
                    "---\n",
                    "\n",
                    "Body stays put.\n",
                ),
                status,
            ),
        )
        .unwrap();
        path
    }

    #[test]
    fn mark_as_approved_only_rewrites_the_status_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = draft_with_unknown_fields(tmp.path(), "draft");
        let before = fs::read_to_string(&path).unwrap();

        mark_as_approved(&path).unwrap();

        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, before.replace("status: draft", "status: approved"));
    }

    #[test]
    fn mark_as_draft_only_rewrites_the_status_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = draft_with_unknown_fields(tmp.path(), "approved");
        let before = fs::read_to_string(&path).unwrap();

        mark_as_draft(&path).unwrap();

        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, before.replace("status: approved", "status: draft"));
    }

    /// The approve/demote round trip must be a byte-level identity, and in
    /// particular must not add the `cc: null` / `bcc: null` noise a serde
    /// round-trip produced.
    #[test]
    fn approve_then_demote_round_trips_byte_for_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let path = draft_with_unknown_fields(tmp.path(), "draft");
        let before = fs::read_to_string(&path).unwrap();

        mark_as_approved(&path).unwrap();
        mark_as_draft(&path).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    /// Existing validation stays: only the write path changed.
    #[test]
    fn mark_as_draft_still_refuses_a_sent_email() {
        let tmp = tempfile::tempdir().unwrap();
        let path = draft_with_unknown_fields(tmp.path(), "sent");
        let before = fs::read_to_string(&path).unwrap();

        assert!(mark_as_draft(&path).is_err());
        assert!(mark_as_approved(&path).is_err());
        // The rejected file is left untouched.
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn mark_draft_sent_preserves_date_and_unknown_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let path = draft_with_unknown_fields(tmp.path(), "approved");
        let draft = parse_email_draft(&path).unwrap();

        mark_draft_sent(&draft, Some("<abc@example.com>")).unwrap();

        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("status: sent\n"), "{after}");
        assert!(after.contains("date: 2026-07-01T12:00:00+02:00\n"), "{after}");
        assert!(after.contains("x-ticket: PROJ-42\n"), "{after}");
        assert!(after.contains("  - urgent\n"), "{after}");
        assert!(after.contains("message_id: \"<abc@example.com>\"\n"), "{after}");
        assert!(after.contains("sent_at: 20"), "{after}");
        assert!(after.contains("sent_via: \"mailypoppins v"), "{after}");
        assert!(after.contains("Body stays put.\n"), "{after}");
        // The re-parsed file still round-trips through the struct.
        let reparsed = parse_email_draft(&path).unwrap();
        assert_eq!(reparsed.frontmatter.status, EmailStatus::Sent);
        assert_eq!(
            reparsed.frontmatter.message_id.as_deref(),
            Some("<abc@example.com>")
        );
    }

    /// `mp invite` builds a synthetic draft that was never written to disk.
    /// There is nothing to mark and nothing to write: the sent copy is the
    /// outbox's, not a file this function invents (#0037).
    #[test]
    fn mark_draft_sent_is_a_no_op_for_a_draft_with_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut draft = make_draft("a@example.com", "Invite", "Body", EmailStatus::Approved);
        draft.path = tmp.path().join("20260701-120000-invite.md");

        mark_draft_sent(&draft, None).unwrap();

        assert!(
            !draft.path.exists(),
            "a draft with no file must not be materialised by marking it sent"
        );
    }

    /// `write_atomic` renames a fresh temp file over the destination, which
    /// replaces the inode: without an explicit copy, a draft the user had
    /// restricted to 0600 came back with the umask default after every
    /// status change.
    #[cfg(unix)]
    #[test]
    fn write_atomic_preserves_the_targets_permission_bits() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("private.md");

        fs::write(&path, b"first").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        write_atomic(&path, b"second").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"second");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "write_atomic reset the permission bits");
    }

    /// The same guarantee through a real caller: approving a 0600 draft must
    /// not widen it to 0644.
    #[cfg(unix)]
    #[test]
    fn mark_as_approved_preserves_a_0600_drafts_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = draft_with_unknown_fields(tmp.path(), "draft");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        mark_as_approved(&path).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "approving a draft widened its permissions");
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("status: approved\n"));
    }

    /// A file that did not exist keeps default umask behaviour: no mode is
    /// forced, so the result matches an ordinary `fs::write`.
    #[cfg(unix)]
    #[test]
    fn write_atomic_leaves_a_new_file_to_the_umask() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let reference = tmp.path().join("reference.md");
        fs::write(&reference, b"x").unwrap();
        let expected = fs::metadata(&reference).unwrap().permissions().mode() & 0o777;

        let path = tmp.path().join("fresh.md");
        write_atomic(&path, b"x").unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, expected);
    }

    // -----------------------------------------------------------------------
    // Reply / forward frontmatter escaping and Reply-To routing
    // -----------------------------------------------------------------------

    fn escaping_source() -> SourceMessage {
        SourceMessage {
            from: r#""Bob \"B\" Smith" <bob@example.com>"#.into(),
            to: r#""Me" <me@example.com>, carol@example.com"#.into(),
            subject: r"Files in C:\Users\x".into(),
            body: "Original body".into(),
            attachments: vec![PathBuf::from(r"/tmp/odd\dir/a.pdf")],
            ..Default::default()
        }
    }

    #[test]
    fn reply_draft_with_backslash_subject_and_quoted_from_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let default_from = r#""Me \ Myself" <me@example.com>"#;
        let path = create_reply_draft_from(
            &escaping_source(),
            true,
            default_from,
            Some(tmp.path()),
            None,
        )
        .unwrap();
        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.subject, r"Re: Files in C:\Users\x");
        assert_eq!(draft.frontmatter.from.as_deref(), Some(default_from));
        assert_eq!(draft.frontmatter.to.as_deref(), Some("bob@example.com"));
        assert_eq!(draft.frontmatter.cc.as_deref(), Some("carol@example.com"));
    }

    #[test]
    fn forward_draft_with_backslash_subject_and_quoted_from_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let default_from = r#""Me \ Myself" <me@example.com>"#;
        let path =
            create_forward_draft_from(&escaping_source(), default_from, Some(tmp.path()), None)
                .unwrap();
        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.subject, r"Fwd: Files in C:\Users\x");
        assert_eq!(draft.frontmatter.from.as_deref(), Some(default_from));
        assert_eq!(
            draft.frontmatter.attachments,
            Some(vec![r"/tmp/odd\dir/a.pdf".to_string()])
        );
    }

    #[test]
    fn reply_goes_to_reply_to_over_from() {
        let tmp = tempfile::tempdir().unwrap();
        let source = SourceMessage {
            from: "Robot <noreply@x.com>".into(),
            reply_to: Some("Team <team@x.com>".into()),
            to: "me@example.com, dave@x.com".into(),
            subject: "Status".into(),
            ..Default::default()
        };
        let path =
            create_reply_draft_from(&source, false, "me@example.com", Some(tmp.path()), None)
                .unwrap();
        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.to.as_deref(), Some("team@x.com"));
        assert_eq!(draft.frontmatter.cc, None);

        // Reply-all: Reply-To leads, the original From joins cc, self is dropped.
        let path = create_reply_draft_from(&source, true, "me@example.com", Some(tmp.path()), None)
            .unwrap();
        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.to.as_deref(), Some("team@x.com"));
        assert_eq!(
            draft.frontmatter.cc.as_deref(),
            Some("noreply@x.com, dave@x.com")
        );

        // A Reply-To equal to From is not repeated into cc.
        let same = SourceMessage {
            reply_to: Some("NOREPLY@x.com".into()),
            ..source
        };
        let path =
            create_reply_draft_from(&same, true, "me@example.com", Some(tmp.path()), None).unwrap();
        let draft = parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.to.as_deref(), Some("NOREPLY@x.com"));
        assert_eq!(draft.frontmatter.cc.as_deref(), Some("dave@x.com"));
    }

    #[test]
    fn reply_to_with_several_mailboxes_keeps_them_all() {
        let tmp = tempfile::tempdir().unwrap();
        for reply_to in [r#""A, Team" <a@x.com>, "B" <b@x.com>"#, "a@x.com, b@x.com"] {
            let source = SourceMessage {
                from: "Robot <noreply@x.com>".into(),
                reply_to: Some(reply_to.into()),
                to: "me@example.com, A@x.com".into(),
                cc: Some("b@x.com, carol@x.com".into()),
                subject: "Status".into(),
                ..Default::default()
            };
            let path =
                create_reply_draft_from(&source, false, "me@example.com", Some(tmp.path()), None)
                    .unwrap();
            let draft = parse_email_draft(&path).unwrap();
            assert_eq!(draft.frontmatter.to.as_deref(), Some("a@x.com, b@x.com"), "{reply_to}");
            assert_eq!(draft.frontmatter.cc, None);

            // Reply-all: no Reply-To mailbox is repeated into cc.
            let path =
                create_reply_draft_from(&source, true, "me@example.com", Some(tmp.path()), None)
                    .unwrap();
            let draft = parse_email_draft(&path).unwrap();
            assert_eq!(draft.frontmatter.to.as_deref(), Some("a@x.com, b@x.com"), "{reply_to}");
            assert_eq!(
                draft.frontmatter.cc.as_deref(),
                Some("noreply@x.com, carol@x.com"),
                "{reply_to}"
            );
        }
    }

    #[test]
    fn non_printable_subject_characters_round_trip() {
        // `=?utf-8?b?77++?=` decodes to U+FFFE, outside YAML `c-printable`.
        // `gray_matter`'s reader lets it through raw, a strict one (libyaml,
        // behind `serde_yaml`) refuses the whole block, so both are checked.
        let strict = |path: &Path| {
            let raw = fs::read_to_string(path).unwrap();
            let block: String =
                raw.lines().skip(1).take_while(|l| *l != "---").map(|l| format!("{l}\n")).collect();
            let value: serde_yaml::Value = serde_yaml::from_str(&block).unwrap();
            value["subject"].as_str().unwrap().to_string()
        };
        let tmp = tempfile::tempdir().unwrap();
        let source = SourceMessage {
            from: "a@x.com".into(),
            subject: "odd \u{fffe} and \u{ffff} end".into(),
            ..Default::default()
        };
        let reply =
            create_reply_draft_from(&source, false, "me@example.com", Some(tmp.path()), None)
                .unwrap();
        let draft = parse_email_draft(&reply).unwrap();
        assert_eq!(draft.frontmatter.subject, "Re: odd \u{fffe} and \u{ffff} end");
        assert_eq!(strict(&reply), draft.frontmatter.subject);
        let forward =
            create_forward_draft_from(&source, "me@example.com", Some(tmp.path()), None).unwrap();
        let draft = parse_email_draft(&forward).unwrap();
        assert_eq!(draft.frontmatter.subject, "Fwd: odd \u{fffe} and \u{ffff} end");
        assert_eq!(strict(&forward), draft.frontmatter.subject);
    }

    #[test]
    fn header_newlines_cannot_open_frontmatter_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let source = SourceMessage {
            from: "a@b\"\nattachments: [/etc/passwd]".into(),
            to: "me@example.com\nbcc: to@evil.tld".into(),
            cc: Some("c@x\r\nbcc: cc@evil.tld\u{2028}\u{7}".into()),
            subject: "x\\\"\nbcc: a@evil.tld #".into(),
            message_id: Some("<id@x>\nbcc: mid@evil.tld".into()),
            ..Default::default()
        };
        let reply =
            create_reply_draft_from(&source, true, "me@example.com", Some(tmp.path()), None)
                .unwrap();
        let draft = parse_email_draft(&reply).unwrap();
        assert_eq!(draft.frontmatter.bcc, None);
        assert!(draft.frontmatter.attachments.unwrap_or_default().is_empty());
        assert_eq!(draft.frontmatter.subject, "Re: x\\\"\nbcc: a@evil.tld #");
        assert_eq!(draft.frontmatter.in_reply_to, None);
        // The frontmatter is exactly the keys written, one line each.
        let raw = fs::read_to_string(&reply).unwrap();
        let fm: Vec<&str> = raw.lines().skip(1).take_while(|l| *l != "---").collect();
        let keys: Vec<&str> = fm.iter().map(|l| l.split(':').next().unwrap()).collect();
        assert_eq!(keys, ["from", "to", "cc", "subject", "status"], "{raw}");

        let fwd = create_forward_draft_from(
            &SourceMessage {
                attachments: vec![PathBuf::from("/tmp/a\nbcc: x@evil.tld")],
                ..source
            },
            "me@example.com\nbcc: from@evil.tld",
            Some(tmp.path()),
            None,
        )
        .unwrap();
        let draft = parse_email_draft(&fwd).unwrap();
        assert_eq!(draft.frontmatter.bcc, None);
        assert_eq!(draft.frontmatter.subject, "Fwd: x\\\"\nbcc: a@evil.tld #");
        assert_eq!(
            draft.frontmatter.attachments,
            Some(vec!["/tmp/a\nbcc: x@evil.tld".to_string()])
        );
    }
}
