//! What a server copy carries for mp and for nobody else (#0143).
//!
//! A copy mp writes can be sent unedited from Outlook, Apple Mail or the
//! Proton app, and then everything in its body reaches the recipients. So the
//! body carries no more than a mark, the last comment of the HTML part, which
//! holds two SHA-256 hashes ([`crate::parse::draft_copy_comment`]): the draft
//! file's, so mp tells its own unedited copy from one edited elsewhere, and
//! the carried source's.
//!
//! The source, the draft file with its `bcc:` emptied, rides in
//! `X-Mailypoppins-Draft-Source` headers, base64 in numbered chunks, so mp on
//! the other machine reads the draft back whole: frontmatter, Markdown and
//! quote. The Bcc itself travels in the copy's own `Bcc` header, which every
//! client already treats as blind, and is put back from there.
//!
//! The headers are a best effort with a fallback. Exchange keeps an APPEND's
//! MIME as written until a client edits the copy, and promotes no unknown
//! X-header to a property, so Outlook's edit or send drops it (Microsoft,
//! "Named Properties, X-Headers, and You"); Proton Bridge drops unknown
//! headers at the APPEND (the probe's `X-Mailypoppins-Draft-Id`). A copy
//! without them is read through its HTML like a copy edited elsewhere, with
//! the mark still telling an unedited copy from an edited one.

use base64::Engine;

use super::file_hash;
use super::folder::{bare_message_id, header_value};

/// The header the source rides in, once per chunk: `<i>/<n> <base64>`.
pub const SOURCE_HEADER: &str = "X-Mailypoppins-Draft-Source";

/// Base64 characters per header: a quarter of the 32 KB Exchange Online
/// allows one header.
const CHUNK_CHARS: usize = 8 * 1024;

/// The largest source carried, encoded: a quarter of the 256 KB Exchange
/// allows a header block. A larger draft goes without, and the other machine
/// reads it through its HTML.
const MAX_ENCODED: usize = 64 * 1024;

/// Folded line length inside a header.
const FOLD: usize = 76;

/// What a copy of one draft file carries: the mark its HTML part ends with,
/// and the header lines that go before the message's own.
pub struct Carried {
    pub mark: String,
    pub headers: Vec<u8>,
}

/// The carrier of the draft file `file`.
pub fn carry(file: &[u8]) -> Carried {
    let whole = file_hash(file);
    let redacted = std::str::from_utf8(file)
        .ok()
        .and_then(|text| crate::draft::redact_draft_bcc(text).ok());
    let encoded = redacted
        .as_deref()
        .map(|text| base64::engine::general_purpose::STANDARD.encode(text))
        .filter(|encoded| !encoded.is_empty() && encoded.len() <= MAX_ENCODED);
    match (redacted, encoded) {
        (Some(redacted), Some(encoded)) => Carried {
            mark: crate::parse::draft_copy_comment(&whole, Some(&file_hash(redacted.as_bytes()))),
            headers: header_lines(&encoded),
        },
        _ => Carried {
            mark: crate::parse::draft_copy_comment(&whole, None),
            headers: Vec::new(),
        },
    }
}

fn header_lines(encoded: &str) -> Vec<u8> {
    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(CHUNK_CHARS).collect();
    let mut out = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        out.extend_from_slice(
            format!("{SOURCE_HEADER}: {}/{}", index + 1, chunks.len()).as_bytes(),
        );
        for line in chunk.chunks(FOLD) {
            out.extend_from_slice(b"\r\n ");
            out.extend_from_slice(line);
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// A copy mp wrote and nobody edited since.
#[derive(Clone, Debug)]
pub struct MpCopy {
    /// The hash of the draft file the copy was written from.
    pub file_hash: String,
    /// That file as the copy carries it, its Bcc put back from the copy's
    /// header; `None` when the headers did not travel.
    pub source: Option<Vec<u8>>,
}

impl MpCopy {
    /// Whether the carried source is the file the copy was written from, byte
    /// for byte: the headers travelled and the Bcc put back gives the very
    /// bytes the mark names. Only such a copy hands on the file's approval
    /// ([`super::approval_travels`]); a copy read through its HTML, or one
    /// whose Bcc came back in another spelling, is close to the file and not
    /// the file.
    pub fn reproduces_file(&self) -> bool {
        self.source
            .as_deref()
            .is_some_and(|source| file_hash(source) == self.file_hash)
    }
}

/// What `raw` carries for mp, `None` for a copy edited elsewhere or one whose
/// carrier does not hold together: a source that does not match the mark, or
/// a draft whose `server_message_id` is not the copy's own `Message-ID`, is
/// not mp's and is read as a foreign copy.
pub fn read(raw: &[u8]) -> Option<MpCopy> {
    let email = crate::parse::parse_rfc822_to_fetched_email(raw)?;
    let mark = crate::parse::draft_copy_mark(email.html_body.as_deref()?)?;
    let (headers, _) = mailparse::parse_headers(raw).ok()?;
    let values: Vec<String> = headers
        .iter()
        .filter(|h| h.get_key().eq_ignore_ascii_case(SOURCE_HEADER))
        .map(|h| h.get_value())
        .collect();
    let (Some(expected), false) = (mark.source_hash.as_deref(), values.is_empty()) else {
        return Some(MpCopy {
            file_hash: mark.file_hash,
            source: None,
        });
    };
    let redacted = assemble(&values)?;
    if file_hash(&redacted) != expected {
        return None;
    }
    let text = String::from_utf8(redacted).ok()?;
    let carried =
        crate::draft::parse_email_draft_content(std::path::Path::new("carried.md"), &text).ok()?;
    let own = header_value(raw, "Message-ID")?;
    let named = carried.frontmatter.server_message_id?;
    if bare_message_id(&named) != bare_message_id(&own) {
        return None;
    }
    // The Bcc back from the copy's header: as the file had it when that
    // gives the file's very bytes, quoted otherwise.
    let source = match header_value(raw, "Bcc") {
        Some(bcc) => crate::draft::draft_with_bcc(&text, &bcc, false)
            .ok()
            .filter(|plain| file_hash(plain.as_bytes()) == mark.file_hash)
            .or_else(|| crate::draft::draft_with_bcc(&text, &bcc, true).ok())?,
        None => text,
    };
    Some(MpCopy {
        file_hash: mark.file_hash,
        source: Some(source.into_bytes()),
    })
}

/// The bytes the numbered chunks spell, `None` when one is missing or does
/// not decode.
fn assemble(values: &[String]) -> Option<Vec<u8>> {
    let mut chunks: Vec<(usize, usize, String)> = Vec::new();
    for value in values {
        let mut words = value.split_whitespace();
        let (index, total) = words.next()?.split_once('/')?;
        chunks.push((index.parse().ok()?, total.parse().ok()?, words.collect()));
    }
    chunks.sort_by_key(|(index, _, _)| *index);
    let total = chunks.first()?.1;
    let complete = chunks.len() == total
        && chunks
            .iter()
            .enumerate()
            .all(|(at, (index, n, _))| *index == at + 1 && *n == total);
    if !complete {
        return None;
    }
    let encoded: String = chunks.into_iter().map(|(_, _, chunk)| chunk).collect();
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}
