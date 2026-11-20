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
//! reads as one sentence. lettre 0.11.19 hands its lines over glued together
//! with nothing between them, so the enhanced status code (RFC 3463) that
//! each continuation line repeats is where one line ended: [`unglue`] puts a
//! space there and drops the repeat, and a reply whose lines carry no
//! enhanced code keeps lettre's joining. A later lettre that separates the
//! lines itself leaves nothing for [`unglue`] to find.
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
/// into one sentence ([`unglue`]); `None` for an error that is no reply.
/// What a classification by the server's words reads (an enhanced code
/// such as `5.7.8`), never [`smtp_words`]' rendering.
pub(crate) fn reply_text(err: &Error) -> Option<String> {
    err.status()?;
    let text = err.source().map(|s| s.to_string()).unwrap_or_default();
    Some(unglue(text.trim()))
}

/// `text` with each glued repeat of its leading enhanced status code (the
/// joint of two lines, `rejected:5.1.1 User`) turned into one space.
///
/// A repeat is glued when the character before it is not white space and
/// white space or the end follows it, so an enhanced code the text quotes
/// in passing (`(5.7.8)`) stays.
fn unglue(text: &str) -> String {
    let Some(code) = text.split_whitespace().next().filter(|w| is_enhanced(w)) else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut first = true;
    while let Some(at) = rest.find(code) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + code.len()..].chars().next();
        let glued = !first
            && before.is_some_and(|c| !c.is_whitespace())
            && after.is_none_or(char::is_whitespace);
        if glued {
            out.push_str(rest[..at].trim_end());
            out.push(' ');
            rest = rest[at + code.len()..].trim_start();
        } else {
            out.push_str(&rest[..at + code.len()]);
            rest = &rest[at + code.len()..];
        }
        first = false;
    }
    out.push_str(rest);
    out
}

/// Whether `word` is an enhanced status code, `class.subject.detail` (RFC
/// 3463 2): a class of 2, 4 or 5, then two numbers of one to three digits.
fn is_enhanced(word: &str) -> bool {
    let mut parts = word.split('.');
    let (Some(class), Some(subject), Some(detail), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    matches!(class, "2" | "4" | "5")
        && [subject, detail]
            .iter()
            .all(|part| (1..=3).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glued_lines_part_at_their_repeated_enhanced_code() {
        assert_eq!(
            unglue("5.1.1 <ada@example.org>: Recipient address rejected:5.1.1 User unknown"),
            "5.1.1 <ada@example.org>: Recipient address rejected: User unknown"
        );
        assert_eq!(
            unglue("5.7.8 Not accepted. Go to5.7.8  https://example.org/?p=Bad"),
            "5.7.8 Not accepted. Go to https://example.org/?p=Bad"
        );
    }

    #[test]
    fn a_quoted_code_and_a_text_without_one_stay_as_they_are() {
        for text in [
            "5.7.1 relaying denied (see 5.7.1)",
            "5.7.1 relaying denied, code 5.7.1x",
            "Greylisted, seehttps://example.org",
            "",
            "5.1.1",
        ] {
            assert_eq!(unglue(text), text);
        }
    }

    #[test]
    fn an_enhanced_code_has_a_class_and_two_short_numbers() {
        for word in ["5.1.1", "4.7.0", "2.0.0", "5.123.456"] {
            assert!(is_enhanced(word), "{word}");
        }
        for word in [
            "550", "3.1.1", "5.1", "5.1.1.1", "5.1234.1", "5.a.1", "5..1",
        ] {
            assert!(!is_enhanced(word), "{word}");
        }
    }
}
