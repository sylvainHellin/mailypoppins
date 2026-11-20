//! An SMTP server's refusal in its own words (PERSO-336).
//!
//! lettre keeps a reply the server refused with as its code and its text,
//! and its `Display` puts its own words before them: `permanent error (550):
//! 5.1.1 <bob@example.org>: Recipient address rejected: User unknown`, which
//! is what reached the user. [`smtp_words`] renders the reply as the server
//! wrote it, the code first as on the wire (RFC 5321 4.2): `550 5.1.1
//! <bob@example.org>: Recipient address rejected: User unknown`, the way
//! [`crate::imap_client::words`] renders an IMAP `NO` (PERSO-210).
//!
//! A reply over several lines (`550-...` then `550 ...`, RFC 5321 4.2.1)
//! reads as one sentence: lettre (0.11.23 on) joins its lines with a space
//! and keeps the enhanced status code (RFC 3463) each continuation line
//! repeats once, so its text is used as it comes. A reply that is a bare
//! code, with no text and not even the space after it, parses (0.11.24 on)
//! and reads as its code and its kind.
//!
//! An error that is no reply (a connection lost or closed, a TLS failure, a
//! deadline, lettre refusing on its own) is said in plain words that never
//! read as the server's.
//!
//! These are only words: everything that decides what became of a message
//! reads the typed error (its code, [`is_permanent`], its kind) or the
//! server's own text, never what this module writes, so a row an older build
//! stored with lettre's wording reads and classifies as before.
//!
//! [`is_permanent`]: lettre::transport::smtp::Error::is_permanent

use std::error::Error as _;

use lettre::transport::smtp::Error;

/// What the user reads for `err`: the server's reply as it wrote it, code
/// first, or plain words for an error that is no reply.
pub(crate) fn smtp_words(err: &Error) -> String {
    if let Some(code) = err.status() {
        let text = reply_text(err).unwrap_or_default();
        if text.is_empty() {
            let kind = if err.is_permanent() {
                "a permanent refusal"
            } else {
                "a temporary refusal"
            };
            return format!("{code} (the server gave no reason; {kind})");
        }
        return format!("{code} {text}");
    }
    let source = err
        .source()
        .map(|source| super::first_lowercase(&source.to_string()))
        .unwrap_or_default();
    let with = |lead: &str| {
        if source.is_empty() {
            lead.to_string()
        } else {
            format!("{lead}: {source}")
        }
    };
    if err.is_timeout() {
        "the SMTP server did not answer in time".to_string()
    } else if err.is_response() {
        if source == "incomplete response" {
            "the connection closed before the server answered".to_string()
        } else {
            with("the server's answer could not be read")
        }
    } else if err.is_tls() {
        with("the secure connection to the SMTP server failed")
    } else if err.is_client() {
        with("mp did not send it")
    } else if err.is_transport_shutdown() {
        "the SMTP connection was shut down".to_string()
    } else if super::is_connection_setup_failure(err) {
        with("could not connect to the SMTP server")
    } else {
        with("the connection to the SMTP server failed")
    }
}

/// The text of the reply `err` carries, after its code, its lines joined
/// into one sentence; `None` for an error that is no reply. What a
/// classification by the server's words reads (an enhanced code such as
/// `5.7.8`), never [`smtp_words`]' rendering.
pub(crate) fn reply_text(err: &Error) -> Option<String> {
    err.status()?;
    let text = err.source().map(|s| s.to_string()).unwrap_or_default();
    Some(text.trim().to_string())
}
