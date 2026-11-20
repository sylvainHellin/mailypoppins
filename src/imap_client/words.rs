//! A server's refusal in its own words.
//!
//! `async-imap` keeps a tagged `NO` or `BAD` as the `Debug` rendering of its
//! parts, `code: Some(TryCreate), info: Some("Mailbox doesn't exist")`, and
//! its `Display` puts `no response: ` before that, which is what reached the
//! user. [`server_words`] renders the refusal as the server wrote it, the
//! response code first in brackets as on the wire (RFC 3501 7.1), after
//! `the server answered NO: `: `the server answered NO: [TRYCREATE] Mailbox
//! doesn't exist`. A code imap-proto does not parse (RFC 5530's
//! `[UNAVAILABLE]`, `[OVERQUOTA]`) is still the start of the text, so the
//! bracketed code leads either way, which is what
//! [`super::sent`]'s `not_about_the_copy` reads.
//!
//! The commands [`super::checked`] sends build the refusal with
//! [`refusal_text`] directly; the ones `async-imap` sends itself (`SELECT`,
//! `EXAMINE`, `COPY`, `STATUS`) carry the `Debug` form, which
//! [`server_words`] takes apart. An answer `async-imap`'s parser cannot read
//! at all reaches mp as an I/O error carrying the parser's input as byte
//! values; [`server_words`] gives the server's text instead (PERSO-323). The LOGIN refusal is left as it is:
//! [`super::is_rejection`] and the health line read its raw form.

use async_imap::error::Error;
use async_imap::imap_proto::ResponseCode;

/// A refusal's text as the server sent it: `[CODE] words`, the code as
/// RFC 3501 and its extensions spell it, or the words alone, or
/// `no reason given` for a bare `NO`.
pub(crate) fn refusal_text(code: Option<&ResponseCode<'_>>, info: Option<&str>) -> String {
    let words = info.map(str::trim).filter(|w| !w.is_empty());
    match (code.map(code_atom), words) {
        (Some(code), Some(words)) => format!("[{code}] {words}"),
        (Some(code), None) => format!("[{code}]"),
        (None, Some(words)) => words.to_string(),
        (None, None) => "no reason given".to_string(),
    }
}

/// A response code as the wire spells it.
fn code_atom(code: &ResponseCode<'_>) -> String {
    match code {
        ResponseCode::BadCharset(Some(charsets)) if !charsets.is_empty() => {
            format!("BADCHARSET ({})", charsets.join(" "))
        }
        other => atom_of(&format!("{other:?}")),
    }
}

/// The wire atom of a response code's `Debug` name (`TryCreate`,
/// `ReadOnly`, `BadCharset(None)`), which is its variant name: imap-proto
/// names each after the atom, hyphens dropped.
fn atom_of(debug: &str) -> String {
    let name = debug.split(['(', ' ', '{']).next().unwrap_or_default();
    match name {
        "ReadOnly" => "READ-ONLY".to_string(),
        "ReadWrite" => "READ-WRITE".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

/// `err` for a user: a `NO` or `BAD` as the server's own words after
/// `the server answered NO: ` (or `BAD`), an answer the parser could not
/// read as the server's text ([`unread_answer`]), anything else as
/// `async-imap` says it.
pub(crate) fn server_words(err: &Error) -> String {
    match err {
        Error::No(answer) => format!("the server answered NO: {}", answer_text(answer)),
        Error::Bad(answer) => format!("the server answered BAD: {}", answer_text(answer)),
        Error::Io(io) => match unread_answer(&io.to_string()) {
            Some(text) => format!("the server sent an answer mp cannot read: {text:?}"),
            None => err.to_string(),
        },
        other => other.to_string(),
    }
}

/// `err` as an error in [`server_words`]' words, for the `?` on a read of the
/// server's answer that would otherwise carry `async-imap`'s own rendering:
/// an [`Error`], or the bare I/O error `read_response` answers with.
pub(crate) fn in_server_words(err: impl Into<Error>) -> anyhow::Error {
    anyhow::anyhow!(server_words(&err.into()))
}

/// The server's text in the I/O error `async-imap` makes of an answer its
/// parser could not read, its lines joined by ` / `.
///
/// `async-imap` renders that error as `{nom_error:?} during parsing of
/// {text:?}` (`src/imap_stream.rs`), the nom error carrying its input as a
/// list of byte values, so the text is the `Debug` string after the last
/// ` during parsing of ` (PERSO-323). `None` for any other I/O error.
fn unread_answer(io: &str) -> Option<String> {
    let (_, quoted) = io.rsplit_once(" during parsing of ")?;
    let text = unescape(quoted.strip_prefix('"')?.strip_suffix('"')?);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    Some(lines.join(" / "))
}

/// The text of a refusal `async-imap` kept: [`refusal_text`]'s form already
/// for a checked command's, taken apart from `code: {code:?}, info:
/// {info:?}` for one `async-imap` sent itself.
fn answer_text(answer: &str) -> String {
    let Some(rest) = answer.strip_prefix("code: ") else {
        return answer.to_string();
    };
    let Some((code, info)) = rest.split_once(", info: ") else {
        return answer.to_string();
    };
    let code = code
        .strip_prefix("Some(")
        .and_then(|c| c.strip_suffix(')'))
        .map(atom_of);
    let words = info
        .strip_prefix("Some(\"")
        .and_then(|i| i.strip_suffix("\")"))
        .map(unescape)
        .filter(|w| !w.trim().is_empty());
    match (code, words) {
        (Some(code), Some(words)) => format!("[{code}] {}", words.trim()),
        (Some(code), None) => format!("[{code}]"),
        (None, Some(words)) => words.trim().to_string(),
        (None, None) => "no reason given".to_string(),
    }
}

/// A `Debug`-escaped string's text: `\"` and `\\` and the control escapes a
/// server's words can carry.
fn unescape(escaped: &str) -> String {
    let mut out = String::with_capacity(escaped.len());
    let mut chars = escaped.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `async-imap` keeps of its own commands' refusals, rendered as the
    /// server wrote them, the code first.
    #[test]
    fn an_async_imap_refusal_reads_as_the_server_wrote_it() {
        let cases = [
            (
                Error::No(
                    "code: Some(TryCreate), info: Some(\"Mailbox doesn't exist: Nowhere\")".into(),
                ),
                "the server answered NO: [TRYCREATE] Mailbox doesn't exist: Nowhere",
            ),
            (
                Error::No("code: None, info: Some(\"[OVERQUOTA] Quota exceeded\")".into()),
                "the server answered NO: [OVERQUOTA] Quota exceeded",
            ),
            (
                Error::No("code: Some(ReadOnly), info: None".into()),
                "the server answered NO: [READ-ONLY]",
            ),
            (
                Error::Bad("code: None, info: Some(\"Unknown \\\"command\\\"\")".into()),
                "the server answered BAD: Unknown \"command\"",
            ),
            (
                Error::No("code: None, info: None".into()),
                "the server answered NO: no reason given",
            ),
            (
                Error::No("[CANNOT] Mailbox is locked".into()),
                "the server answered NO: [CANNOT] Mailbox is locked",
            ),
        ];
        for (err, words) in cases {
            assert_eq!(server_words(&err), words, "{err:?}");
        }
        assert_eq!(
            server_words(&Error::ConnectionLost),
            Error::ConnectionLost.to_string()
        );
    }

    /// An answer async-imap's parser could not read is the server's text, its
    /// lines joined, rather than the parser's error with its input as byte
    /// values (PERSO-323): what xmail.mwn.de sent while it was down.
    #[test]
    fn an_answer_the_parser_cannot_read_is_the_servers_text() {
        let input = "Server Unavailable. 15\r\n* BYE Connection closed.\r\n";
        let bytes: Vec<u8> = input.bytes().collect();
        // async-imap's own rendering, `src/imap_stream.rs`.
        let unread = Error::Io(std::io::Error::other(format!(
            "Error(Error {{ input: {bytes:?}, code: Tag }}) during parsing of {input:?}"
        )));
        assert_eq!(
            server_words(&unread),
            "the server sent an answer mp cannot read: \
             \"Server Unavailable. 15 / * BYE Connection closed.\""
        );
        let other = Error::Io(std::io::Error::other("connection reset"));
        assert_eq!(server_words(&other), "io: connection reset");
    }

    /// A parsed code is spelled as on the wire.
    #[test]
    fn a_parsed_code_is_spelled_as_on_the_wire() {
        use std::borrow::Cow;
        assert_eq!(
            refusal_text(Some(&ResponseCode::TryCreate), Some("no such folder")),
            "[TRYCREATE] no such folder"
        );
        assert_eq!(
            refusal_text(
                Some(&ResponseCode::BadCharset(Some(vec![Cow::Borrowed(
                    "US-ASCII"
                )]))),
                Some("not supported")
            ),
            "[BADCHARSET (US-ASCII)] not supported"
        );
        assert_eq!(refusal_text(None, Some("  ")), "no reason given");
    }
}
