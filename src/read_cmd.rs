//! The CLI read surface over the store: `mp show` and `mp list-messages` (#0062).
//!
//! After `mp sync`, received mail was reachable only through the TUI or through
//! `mp dump-mailbox`, which exists as the parity oracle and emits NDJSON for a
//! diff, not for a person. These two commands close that gap with the queries
//! `src/store/read.rs` already has; nothing here adds a query, and nothing here
//! touches the network. What the store holds is what they print.
//!
//! Both render to a `String` rather than printing, so the layout is testable
//! without a subprocess and `main.rs` keeps one `println!` per command.

use anyhow::Result;
use colored::*;
use serde::{Deserialize, Serialize};

use crate::selector::Selector;
use crate::store::read::{self, MessageRow};
use crate::store::{BlobStore, Store};

/// Width of the rules `mp list` draws, so the two listings line up.
const RULE: usize = 72;

/// What `mp show --json` emits: one object, the message and its body.
///
/// Deliberately not [`crate::dump::EnvelopeRecord`]: the dump is a *parity
/// oracle* whose shape is pinned against the file era and must stay
/// byte-stable, so hanging a body off it would make one contract serve two
/// masters. This record is free to carry what a reader of one message wants.
///
/// It is also the `result` of `message.get` (P4-U4), which is why it
/// deserialises: a routed `mp show` renders either answer from the payload
/// alone, and the JSON answer is that payload re-serialised rather than a
/// second projection that could drift from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShownMessage {
    pub selector: String,
    pub account: String,
    pub mailbox: String,
    pub message_id: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub subject: Option<String>,
    pub date: Option<String>,
    pub flags: Vec<String>,
    pub invite: bool,
    pub attachments: Vec<ShownAttachment>,
    /// The stored plain-text body. `null` when the store has no readable body
    /// for the row: an evicted blob, or a message ingested without one. A
    /// reader can tell that from an empty body, which is a message that was
    /// sent empty.
    ///
    /// `#[serde(default)]` because `message.get` omits the key entirely for a
    /// caller that asked for no body, which is not the same answer as `null`.
    #[serde(default)]
    pub body: Option<String>,
}

/// One attachment of a shown message.
///
/// The list is every stored attachment in stored order, and the position in
/// it is the `part` `message.materialise_attachment` takes. `inline` marks an
/// image the body shows by `cid:` URL (PERSO-165): a reader lists it in its
/// own group after the real attachments, and it stays saveable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShownAttachment {
    pub name: String,
    pub size: u64,
    /// Written only when true, so the record of a message without inline
    /// images stays byte-identical to the one the pre-daemon binary prints
    /// (the read-slice parity); `#[serde(default)]` reads its absence, and an
    /// answer from a daemon older than the field, as a real attachment.
    #[serde(default, skip_serializing_if = "is_false")]
    pub inline: bool,
}

/// `skip_serializing_if` for a flag written only when set.
fn is_false(flag: &bool) -> bool {
    !*flag
}

/// The `part` indexes of `attachments` in the order a reader shows them: the
/// real attachments, then the inline images, each in stored order. `mp save`
/// writes them in this order, so a real attachment that shares its name with
/// an inline image keeps the plain name.
pub fn parts_in_reading_order(attachments: &[ShownAttachment]) -> Vec<usize> {
    let (inline, real): (Vec<usize>, Vec<usize>) =
        (0..attachments.len()).partition(|&part| attachments[part].inline);
    real.into_iter().chain(inline).collect()
}

/// The `part` indexes `mp open` opens: the real attachments, or the inline
/// images when the message has none, so a mail whose one photo is inline (an
/// iPhone picture) still opens it and a signed PDF does not open its logos.
pub fn parts_to_open(attachments: &[ShownAttachment]) -> Vec<usize> {
    let real: Vec<usize> = (0..attachments.len())
        .filter(|&part| !attachments[part].inline)
        .collect();
    if real.is_empty() {
        (0..attachments.len()).collect()
    } else {
        real
    }
}

/// A stored header with the empty string read as absent, the rule ingest's
/// columns need everywhere: an absent header arrives as `""`, not as SQL NULL.
fn present(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.is_empty()).map(str::to_string)
}

/// The flag tokens of a row, in a fixed order so two runs agree.
fn flag_names(row: &MessageRow) -> Vec<String> {
    flag_tokens(row.flags())
}

/// The flag tokens `--json` prints, `mp show`'s spelling and order.
fn flag_tokens(flags: crate::types::MessageFlags) -> Vec<String> {
    let mut out = Vec::new();
    for (on, name) in [
        (flags.seen, "read"),
        (flags.answered, "answered"),
        (flags.forwarded, "forwarded"),
        (flags.flagged, "flagged"),
    ] {
        if on {
            out.push(name.to_string());
        }
    }
    out
}

/// Collect one message for `mp show`, body included.
///
/// The body is read through [`read::load_body`], which degrades: a row whose
/// blob is gone answers `Some("")` rather than failing, and a reference to a
/// row that no longer exists answers `None`. Both arrive here as `None` in the
/// record, because from a reader's side they are the same fact -- the store
/// cannot show you this body -- and neither is worth a backtrace.
pub fn shown_message(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    row: &MessageRow,
) -> ShownMessage {
    let attachments = read::attachments_for(store, row.id)
        .unwrap_or_else(|e| {
            log::warn!("[show] attachments of message {}: {e:#}", row.id);
            Vec::new()
        })
        .into_iter()
        .map(|att| ShownAttachment {
            name: att.name,
            size: att.size,
            inline: att.inline,
        })
        .collect();
    ShownMessage {
        selector: Selector::for_message(account, row).to_string(),
        account: account.to_string(),
        mailbox: row.mailbox.clone(),
        message_id: row.message_id.clone(),
        from: present(row.from.as_deref()),
        to: present(row.to.as_deref()),
        cc: present(row.cc.as_deref()),
        subject: present(row.subject.as_deref()),
        date: present(row.date_display.as_deref()),
        flags: flag_names(row),
        invite: row.is_invite,
        attachments,
        body: read::load_body(store, blobs, row.id).filter(|body| !body.is_empty()),
    }
}

/// `mp show <selector>`: one message, headers then body.
///
/// The layout is the headers block `mp fetch --full` prints, with the selector
/// and mailbox added because this command is addressed by selector and the
/// answer should paste into the next one.
///
/// Not `read::render_markdown`, although #0075 built it and it is the obvious
/// candidate: it wraps the headers in a `---` YAML frontmatter and then appends
/// the body verbatim, so a message whose body opens with `---` reads back as
/// though the frontmatter continued (#0062 scope item 5). A human read surface
/// that is not meant to be parsed should not invent a document format it does
/// not escape; `--json` is the parseable answer, and it cannot be ambiguous.
pub fn render_show(message: &ShownMessage) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };

    line(format!(
        "{}: {}",
        "From".bold().green(),
        message.from.as_deref().unwrap_or("(unknown sender)")
    ));
    line(format!(
        "{}: {}",
        "To".bold().blue(),
        message.to.as_deref().unwrap_or("(no recipient)")
    ));
    if let Some(cc) = message.cc.as_deref() {
        line(format!("{}: {}", "Cc".bold().blue(), cc));
    }
    line(format!(
        "{}: {}",
        "Subject".bold().yellow(),
        message.subject.as_deref().unwrap_or("(no subject)")
    ));
    if let Some(date) = message.date.as_deref() {
        line(format!("{}: {}", "Date".bold().magenta(), date));
    }
    line(format!("{}: {}", "Mailbox".bold(), message.mailbox));
    line(format!("{}: {}", "Selector".bold(), message.selector));
    if !message.flags.is_empty() {
        line(format!("{}: {}", "Flags".bold(), message.flags.join(", ")));
    }
    if message.invite {
        line("[calendar invitation]".yellow().to_string());
    }
    // The real attachments first, then the body's inline images in a group
    // of their own (PERSO-165): a signature logo is not a file the sender
    // attached, but a pasted screenshot or a phone photo is sent the same
    // way and `mp save` writes both groups.
    for (inline, label) in [(false, "Attachments"), (true, "Inline images")] {
        let group: Vec<&ShownAttachment> = message
            .attachments
            .iter()
            .filter(|att| att.inline == inline)
            .collect();
        if group.is_empty() {
            continue;
        }
        line(format!("{}:", label.bold()));
        for att in group {
            line(format!("  {} ({})", att.name, human_size(att.size)));
        }
    }

    line("\u{2500}".repeat(RULE));
    match message.body.as_deref() {
        Some(body) => line(body.trim_end().to_string()),
        // The degrade the acceptance criteria ask for: a body the store cannot
        // produce is a sentence, not an error, because the row is still real
        // and everything above this line is still true.
        None => line(
            // Be honest about recovery: a plain `mp sync` skips a UID it already
            // holds a row for, so it does NOT re-download an evicted body.
            // On-demand re-fetch on open is #0085; until it ships, a targeted
            // re-ingest of this message is the recovery.
            "(no stored body: retention evicted it, or it was never ingested; \
             re-ingesting this message restores it -- on-demand re-fetch is #0085)"
                .dimmed()
                .to_string(),
        ),
    }
    out
}

/// `mp list-messages`: one line per message, plus its subject, per mailbox.
///
/// `rows` is one `(mailbox label, messages)` group per mailbox in listing
/// order, already truncated to the limit by the caller, and `total` is how many
/// the store holds for that group before truncation, so the header can say what
/// was left out.
///
/// The shape follows `mp list` (the drafts listing) deliberately: the same
/// rules, the same `[status] selector -> counterpart` line and the same dimmed
/// subject underneath, because a user who has read one has read both.
pub fn render_list(account: &str, groups: &[(String, usize, Vec<MessageRow>)]) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };

    let shown: usize = groups.iter().map(|(_, _, rows)| rows.len()).sum();
    if shown == 0 {
        return format!("No messages in the local store for {account}\n");
    }

    for (label, total, rows) in groups {
        if rows.is_empty() {
            continue;
        }
        line(String::new());
        line(format!("{} ({} of {}):", label.bold(), rows.len(), total));
        line("\u{2500}".repeat(RULE));
        for row in rows {
            let flags = row.flags();
            let status = if flags.seen {
                "read".dimmed()
            } else {
                "unread".yellow()
            };
            line(format!(
                "[{}] {} \u{2192} {}",
                status,
                Selector::for_message(account, row),
                row.from.as_deref().unwrap_or("(unknown sender)")
            ));
            let mut second = String::new();
            if let Some(date) = present(row.date_display.as_deref()) {
                second.push_str(&date);
                second.push_str("  ");
            }
            second.push_str(row.subject.as_deref().unwrap_or("(no subject)"));
            if row.has_attachments {
                second.push_str("  [attachments]");
            }
            line(format!("      {}", second.dimmed()));
        }
    }
    line("\u{2500}".repeat(RULE));
    let total: usize = groups.iter().map(|(_, total, _)| total).sum();
    line(format!("Shown: {shown} | In the store: {total}"));
    out
}

/// `mp search --local`: the ranked hits of the FTS5 index (#0043).
///
/// Flat and best-first rather than grouped by mailbox, because the ranking is
/// the answer: a full-tree search that regrouped its hits would hide which of
/// them the index thought was the closest. Each hit prints the two lines
/// `mp list-messages` prints, with the mailbox in front of the sender so the
/// scope of a hit is readable without parsing the selector, plus the body when
/// `bodies` carries one (`--full`).
pub fn render_search(account: &str, query: &str, hits: &[(MessageRow, Option<String>)]) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };

    if hits.is_empty() {
        return format!("No local matches for {query:?} in {account}\n");
    }

    line(String::new());
    line(format!(
        "{} in {} ({} hit{}):",
        format!("{query:?}").bold(),
        account,
        hits.len(),
        if hits.len() == 1 { "" } else { "s" }
    ));
    line("\u{2500}".repeat(RULE));
    for (row, body) in hits {
        let flags = row.flags();
        let status = if flags.seen {
            "read".dimmed()
        } else {
            "unread".yellow()
        };
        line(format!(
            "[{}] {} \u{2192} {}",
            status,
            Selector::for_message(account, row),
            row.from.as_deref().unwrap_or("(unknown sender)")
        ));
        let mut second = String::new();
        if let Some(date) = present(row.date_display.as_deref()) {
            second.push_str(&date);
            second.push_str("  ");
        }
        second.push_str(row.subject.as_deref().unwrap_or("(no subject)"));
        if row.has_attachments {
            second.push_str("  [attachments]");
        }
        line(format!("      {}", second.dimmed()));
        if let Some(body) = body {
            line("\u{2500}".repeat(RULE));
            line(body.trim_end().to_string());
            line("\u{2500}".repeat(RULE));
        }
    }
    line("\u{2500}".repeat(RULE));
    line(format!("Shown: {} (best match first)", hits.len()));
    out
}

/// A byte count in the units a person reads attachment sizes in.
fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    match bytes {
        b if b >= MB => format!("{:.1} MB", b as f64 / MB as f64),
        b if b >= KB => format!("{:.1} KB", b as f64 / KB as f64),
        b => format!("{b} B"),
    }
}

/// Serialize a shown message as pretty JSON, the `--json` answer.
pub fn to_json(message: &ShownMessage) -> Result<String> {
    Ok(serde_json::to_string_pretty(message)?)
}

/// One message of `mp list-messages --json` and `mp search --json`.
///
/// The keys are [`ShownMessage`]'s where the two overlap, so a script that
/// reads a listing reads `mp show --json` too, with two deliberate
/// differences: `date` is the UTC instant in ISO 8601 (`2026-07-02T11:57:30Z`)
/// that a script can compare, the stored `Date:` header travelling beside it
/// as `date_header`; and a listing knows only whether attachments exist, not
/// their names, so it carries `has_attachments` instead of `attachments`.
///
/// `selector` and `mailbox` are `null` only for a server search hit whose
/// mailbox the server did not name (an unscoped Graph search); `message_id`
/// then still addresses it, since `mp show` and friends take a bare key.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ListedMessage {
    pub selector: Option<String>,
    pub account: String,
    pub mailbox: Option<String>,
    pub message_id: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub subject: Option<String>,
    pub date: Option<String>,
    pub date_header: Option<String>,
    pub flags: Vec<String>,
    pub has_attachments: bool,
    pub invite: bool,
    /// The whole plain-text body, present only with `--full`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The first 300 characters of the body, present only on a server search
    /// hit without `--full`, which is what its text layout prints.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// The UTC instant of a message in ISO 8601, from its `Date:` header, else
/// from the wire's `date_sort` (`resolve_date`'s UTC key, without the `Z`).
fn iso_date(header: &str, date_sort: &str) -> Option<String> {
    const ISO: &str = "%Y-%m-%dT%H:%M:%S";
    if let Ok(at) = chrono::DateTime::parse_from_rfc2822(header) {
        return Some(format!("{}Z", at.with_timezone(&chrono::Utc).format(ISO)));
    }
    chrono::NaiveDateTime::parse_from_str(date_sort, ISO)
        .ok()
        .map(|at| format!("{}Z", at.format(ISO)))
}

impl ListedMessage {
    /// A row the daemon listed or found in the local store.
    ///
    /// The selector is the daemon's own spelling; a row from a daemon that
    /// predates it gets the one [`Selector::for_message`] would have rendered.
    pub fn from_wire(
        account: &str,
        mailbox: &str,
        row: &mp_protocol::listing::MessageListRow,
        body: Option<String>,
    ) -> ListedMessage {
        let selector = if row.selector.is_empty() {
            Selector::new(
                account,
                mailbox,
                crate::selector::message_key(&row.message_id),
            )
            .to_string()
        } else {
            row.selector.clone()
        };
        ListedMessage {
            selector: Some(selector),
            account: account.to_string(),
            mailbox: Some(mailbox.to_string()),
            message_id: row.message_id.clone(),
            from: present(Some(&row.from)),
            to: present(Some(&row.to)),
            cc: present(row.cc.as_deref()),
            subject: present(Some(&row.subject)),
            date: iso_date(&row.date_display, &row.date_sort),
            date_header: present(Some(&row.date_display)),
            flags: flag_tokens(crate::types::MessageFlags {
                seen: row.flags.seen,
                answered: row.flags.answered,
                forwarded: row.flags.forwarded,
                flagged: row.flags.flagged,
            }),
            has_attachments: row.has_attachments,
            invite: row.is_invite,
            body,
            preview: None,
        }
    }

    /// A message a server search fetched, found in `mailbox` (the store's id
    /// for it) when the search knows which one.
    pub fn from_fetched(
        account: &str,
        mailbox: Option<&str>,
        email: &crate::parse::FetchedEmail,
        full: bool,
    ) -> ListedMessage {
        let message_id = email.message_id.clone().unwrap_or_default();
        let selector = mailbox.filter(|_| !message_id.is_empty()).map(|mailbox| {
            Selector::new(account, mailbox, crate::selector::message_key(&message_id)).to_string()
        });
        let (body, preview) = if full {
            (Some(email.body_text.clone()), None)
        } else {
            (None, Some(email.body_text.chars().take(300).collect()))
        };
        ListedMessage {
            selector,
            account: account.to_string(),
            mailbox: mailbox.map(str::to_string),
            message_id,
            from: present(Some(&email.from)),
            to: present(Some(&email.to)),
            cc: present(email.cc.as_deref()),
            subject: present(Some(&email.subject)),
            date: iso_date(&email.date, ""),
            date_header: present(Some(&email.date)),
            flags: flag_tokens(email.flags),
            has_attachments: email.has_attachments,
            invite: email.calendar_ics.is_some(),
            body,
            preview,
        }
    }
}

/// One mailbox of `mp list-messages --json`: how many rows it holds and how
/// many of them the limit let through.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ListedMailbox {
    pub mailbox: String,
    pub label: String,
    pub total: usize,
    pub shown: usize,
}

/// `mp list-messages --json`: one object, the mailboxes it covered and every
/// listed message in one flat array, mailbox by mailbox in listing order and
/// newest first within each, as the text layout prints them.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MessageListingJson {
    pub account: String,
    pub mailboxes: Vec<ListedMailbox>,
    pub messages: Vec<ListedMessage>,
}

/// `mp search --json`: one object, the hits best first (`--local`) or newest
/// first (server). `source` is `"local"` or `"server"`.
///
/// `coverage` says whether un-synced mail can be among the hits: `"server"`
/// when the server answered every term, `"local"` when the store answered at
/// least one (a server search on plain IMAP with `has:attachment`) or all of
/// them (`--local`). `local_terms` names the terms a server search had to
/// answer from the store; a `--local` search answers the whole query there and
/// leaves it empty.
///
/// `truncated` says the query matched more than `messages` holds, because
/// `--limit` cut it (PERSO-175): `coverage` alone read as completeness when a
/// mailbox had filled its quota.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResultsJson {
    pub account: String,
    pub query: String,
    pub source: &'static str,
    pub coverage: &'static str,
    pub local_terms: Vec<String>,
    pub truncated: bool,
    pub messages: Vec<ListedMessage>,
}

impl SearchResultsJson {
    /// A `--local` search: the store answered everything.
    pub fn local(
        account: &str,
        query: &str,
        truncated: bool,
        messages: Vec<ListedMessage>,
    ) -> SearchResultsJson {
        SearchResultsJson {
            account: account.to_string(),
            query: query.to_string(),
            source: "local",
            coverage: "local",
            local_terms: Vec::new(),
            truncated,
            messages,
        }
    }

    /// A server search, `local_terms` being what the server had no key for.
    pub fn server(
        account: &str,
        query: &str,
        local_terms: &[String],
        truncated: bool,
        messages: Vec<ListedMessage>,
    ) -> SearchResultsJson {
        SearchResultsJson {
            account: account.to_string(),
            query: query.to_string(),
            source: "server",
            coverage: if local_terms.is_empty() {
                "server"
            } else {
                "local"
            },
            local_terms: local_terms.to_vec(),
            truncated,
            messages,
        }
    }
}

/// The order a server search lists its hits in: newest first, an unparsable
/// date last.
pub fn fetched_newest_first(
    a: &crate::parse::FetchedEmail,
    b: &crate::parse::FetchedEmail,
) -> std::cmp::Ordering {
    let da = chrono::DateTime::parse_from_rfc2822(&a.date).ok();
    let db = chrono::DateTime::parse_from_rfc2822(&b.date).ok();
    db.cmp(&da)
}

/// One mailbox's answer to a server search.
pub struct MailboxHits {
    /// The store's id of the mailbox, which a `--json` hit's selector names.
    pub mailbox: Option<String>,
    pub emails: Vec<crate::parse::FetchedEmail>,
    /// The server matched more than it returned.
    pub more: bool,
}

/// The hits of a server search over several mailboxes as one answer
/// (PERSO-175).
///
/// Every mailbox was asked for the whole `limit`, so the merge, newest first
/// and cut to `limit`, keeps the newest hits wherever they sit; splitting the
/// limit across the mailboxes instead dropped a busy mailbox's hits for an
/// idle one's older ones. The flag is true when a mailbox held more than it
/// returned or the cut dropped hits, which is the `truncated` of `--json`.
pub fn merge_server_hits(
    groups: Vec<MailboxHits>,
    limit: usize,
) -> (Vec<(Option<String>, crate::parse::FetchedEmail)>, bool) {
    let mut truncated = groups.iter().any(|group| group.more);
    let mut hits: Vec<_> = groups
        .into_iter()
        .flat_map(|group| {
            let mailbox = group.mailbox;
            group
                .emails
                .into_iter()
                .map(move |email| (mailbox.clone(), email))
        })
        .collect();
    hits.sort_by(|(_, a), (_, b)| fetched_newest_first(a, b));
    if hits.len() > limit {
        hits.truncate(limit);
        truncated = true;
    }
    (hits, truncated)
}

/// The stdout line under a search `--limit` cut, the text twin of `--json`'s
/// `truncated`.
pub fn more_results_line(limit: usize) -> String {
    format!("More results than the {limit} shown: raise -n to see them.")
}

/// The coverage caveat of a server search's stdout header, `local store only:
/// has:attachment`, or `None` when the server answered every term.
///
/// The stderr warning says the same at length; this is the copy that survives
/// a caller piping stdout and dropping stderr, where an empty result would
/// otherwise read as "no such mail" rather than "none synced".
pub fn local_store_note(local_terms: &[String]) -> Option<String> {
    (!local_terms.is_empty()).then(|| format!("local store only: {}", local_terms.join(", ")))
}

/// The line a server search with no hits prints, carrying the coverage caveat
/// when the store answered a term.
pub fn no_server_results(local_terms: &[String]) -> String {
    match local_store_note(local_terms) {
        Some(note) => format!("No results found ({note})"),
        None => "No results found".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{ingest_message, IngestInput};
    use crate::parse::FetchedEmail;
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        store: Store,
        blobs: BlobStore,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        Fixture {
            _dir: dir,
            store,
            blobs,
        }
    }

    fn email(subject: &str, body: &str) -> FetchedEmail {
        FetchedEmail {
            from: "Ada Lovelace <ada@example.com>".into(),
            to: "b@example.com".into(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: subject.into(),
            date: "Thu, 7 Aug 2026 10:00:00 +0000".into(),
            body_text: body.into(),
            html_body: None,
            has_attachments: false,
            server_has_attachments: None,
            message_id: Some(format!("<{subject}@example.com>")),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        }
    }

    fn ingest(fx: &Fixture, mailbox: &str, uid: i64, e: &FetchedEmail) -> MessageRow {
        let outcome = ingest_message(
            &fx.store,
            &fx.blobs,
            &IngestInput {
                account: "acct",
                mailbox,
                uid,
                email: e,
                raw: None,
            },
        )
        .unwrap();
        read::find_by_id(&fx.store, outcome.row_id)
            .unwrap()
            .unwrap()
    }

    /// The WZT shape (PERSO-165): a real PDF and an Outlook signature logo.
    /// `mp show` lists the PDF under "Attachments" and the logo under
    /// "Inline images" after it; the JSON keeps the stored order with the
    /// flag, which is the `part` order.
    #[test]
    fn show_lists_the_inline_images_in_their_own_group() {
        colored::control::set_override(false);
        let fx = fixture();
        let mut e = email("signed", "See attached.\n");
        e.html_body = Some("<p>See attached.</p><img src=\"cid:image001.png@01DA\">".into());
        e.has_attachments = true;
        e.attachments = vec![
            crate::parse::AttachmentData {
                filename: "image001.png".into(),
                content: b"\x89PNG\r\n\x1a\nlogo".to_vec(),
                content_id: Some("image001.png@01DA".into()),
            },
            crate::parse::AttachmentData {
                filename: "Rechnung.pdf".into(),
                content: b"%PDF-1.7".to_vec(),
                content_id: None,
            },
        ];
        let row = ingest(&fx, "inbox", 1, &e);
        let shown = shown_message(&fx.store, &fx.blobs, "acct", &row);

        assert_eq!(
            shown
                .attachments
                .iter()
                .map(|a| (a.name.as_str(), a.inline))
                .collect::<Vec<_>>(),
            vec![("image001.png", true), ("Rechnung.pdf", false)]
        );
        let text = render_show(&shown);
        assert!(
            text.contains(
                "Attachments:\n  Rechnung.pdf (8 B)\nInline images:\n  image001.png (12 B)\n"
            ),
            "{text}"
        );
        assert_eq!(parts_in_reading_order(&shown.attachments), vec![1, 0]);
        assert_eq!(parts_to_open(&shown.attachments), vec![1]);
    }

    /// A mail whose one photo is inline (an iPhone picture) lists it under
    /// "Inline images" alone, and `mp open` falls back to it.
    #[test]
    fn an_inline_only_message_opens_its_inline_images() {
        let only = |inline: bool| ShownAttachment {
            name: "IMG_1234.jpeg".into(),
            size: 3,
            inline,
        };
        assert_eq!(parts_to_open(&[only(true), only(true)]), vec![0, 1]);
        assert_eq!(parts_to_open(&[only(true), only(false)]), vec![1]);
        assert!(parts_to_open(&[]).is_empty());
        let shown = ShownMessage {
            selector: "mp://acct/inbox/x".into(),
            account: "acct".into(),
            mailbox: "inbox".into(),
            message_id: "<x>".into(),
            from: None,
            to: None,
            cc: None,
            subject: None,
            date: None,
            flags: Vec::new(),
            invite: false,
            attachments: vec![only(true)],
            body: None,
        };
        colored::control::set_override(false);
        let text = render_show(&shown);
        assert!(!text.contains("Attachments:"), "{text}");
        assert!(
            text.contains("Inline images:\n  IMG_1234.jpeg (3 B)\n"),
            "{text}"
        );
    }

    /// The acceptance criterion: `mp show` prints the body the TUI shows, which
    /// is the same `read::load_body` the preview pane reads.
    #[test]
    fn show_prints_the_stored_body_and_the_canonical_selector() {
        colored::control::set_override(false);
        let fx = fixture();
        let row = ingest(&fx, "inbox", 4, &email("hello", "the body\nsecond line"));

        let shown = shown_message(&fx.store, &fx.blobs, "acct", &row);
        assert_eq!(shown.body.as_deref(), Some("the body\nsecond line"));
        assert_eq!(shown.selector, "mp://acct/inbox/hello@example.com");

        let text = render_show(&shown);
        assert!(text.contains("From: Ada Lovelace <ada@example.com>"));
        assert!(text.contains("Subject: hello"));
        assert!(text.contains("Selector: mp://acct/inbox/hello@example.com"));
        assert!(text.ends_with("the body\nsecond line\n"));
    }

    /// A body whose first line is `---` is why this is not
    /// `read::render_markdown`: nothing downstream can mistake it for a
    /// frontmatter fence, because there is no frontmatter.
    #[test]
    fn a_body_that_opens_with_a_yaml_fence_is_not_ambiguous() {
        colored::control::set_override(false);
        let fx = fixture();
        let row = ingest(
            &fx,
            "inbox",
            5,
            &email("fenced", "---\nfrom: forged@example.com\n---"),
        );

        let text = render_show(&shown_message(&fx.store, &fx.blobs, "acct", &row));
        let rendered = read::render_markdown(&fx.store, &fx.blobs, &row);
        assert_eq!(
            rendered.matches("\n---\n").count(),
            3,
            "the markdown rendition really is ambiguous, which is what this avoids"
        );
        assert!(
            !text.starts_with("---"),
            "the show layout opens with headers, not a fence"
        );
        assert!(
            text.contains("from: forged@example.com"),
            "the body is still printed in full"
        );
    }

    /// An evicted body degrades to a sentence rather than an error, and says so
    /// in JSON with `null` instead of an empty string.
    #[test]
    fn an_evicted_body_degrades_to_a_message() {
        colored::control::set_override(false);
        let fx = fixture();
        let row = ingest(&fx, "inbox", 6, &email("evicted", "gone soon"));
        // Evict the blob the way retention would: the row survives, the bytes
        // do not.
        std::fs::remove_dir_all(fx.blobs.root()).unwrap();

        let shown = shown_message(&fx.store, &fx.blobs, "acct", &row);
        assert_eq!(shown.body, None);
        let text = render_show(&shown);
        assert!(
            text.contains("Subject: evicted"),
            "the envelope is still readable"
        );
        assert!(text.contains("no stored body"));
    }

    /// The listing follows the store's own order (newest first), carries the
    /// selector that addresses each row, and reports what the limit left out.
    #[test]
    fn the_listing_names_every_row_by_selector_and_reports_the_truncation() {
        colored::control::set_override(false);
        let fx = fixture();
        for (uid, subject) in [(1, "oldest"), (2, "middle"), (3, "newest")] {
            let mut e = email(subject, "body");
            e.date = format!("Thu, {} Aug 2026 10:00:00 +0000", 5 + uid);
            ingest(&fx, "inbox", uid, &e);
        }
        let all = read::list_mailbox(&fx.store, "acct", "inbox").unwrap();
        assert_eq!(all.len(), 3);

        // The limit is applied by the caller over the store's own order, which
        // is the order the TUI list shows, so the listing is the first N of it
        // and the N+1th is absent.
        let text = render_list("acct", &[("Inbox".into(), all.len(), all[..2].to_vec())]);
        assert!(text.contains("Inbox (2 of 3):"));
        for row in &all[..2] {
            assert!(text.contains(&Selector::for_message("acct", row).to_string()));
        }
        assert!(
            !text.contains(&Selector::for_message("acct", &all[2]).to_string()),
            "the limit cuts from the end of the store's order, not from the middle"
        );
        assert!(text.contains("[unread]"));
        assert!(text.contains("Shown: 2 | In the store: 3"));
    }

    /// `mp search --local` prints its hits best-first, each addressable by the
    /// selector the next command takes, and shows the body only with `--full`.
    #[test]
    fn the_search_listing_is_ranked_selectors_and_optional_bodies() {
        colored::control::set_override(false);
        let fx = fixture();
        let first = ingest(&fx, "inbox", 1, &email("ledger", "the quarterly ledger"));
        let second = ingest(&fx, "archive", 2, &email("lunch", "ledger of pizzas"));

        let text = render_search(
            "acct",
            "ledger",
            &[(first.clone(), None), (second.clone(), None)],
        );
        assert!(text.contains("\"ledger\" in acct (2 hits):"));
        let at_first = text.find(&Selector::for_message("acct", &first).to_string());
        let at_second = text.find(&Selector::for_message("acct", &second).to_string());
        assert!(
            at_first < at_second,
            "hits print in the order they are ranked"
        );
        assert!(text.contains("Shown: 2 (best match first)"));
        assert!(!text.contains("quarterly ledger"), "no body without --full");

        let full = render_search(
            "acct",
            "ledger",
            &[(first, Some("the quarterly ledger".into()))],
        );
        assert!(full.contains("the quarterly ledger"));
        assert!(full.contains("(1 hit):"));
    }

    /// A query nothing matches says so rather than printing an empty frame.
    #[test]
    fn an_empty_search_says_so() {
        colored::control::set_override(false);
        let text = render_search("acct", "nothing", &[]);
        assert_eq!(text, "No local matches for \"nothing\" in acct\n");
    }

    /// A `--json` listing entry carries the selector the next command takes,
    /// the date as a UTC ISO 8601 instant, and `mp show`'s flag tokens.
    #[test]
    fn a_listed_message_carries_its_selector_and_an_iso_date() {
        let row = mp_protocol::listing::MessageListRow {
            id: 7,
            uid: 3,
            message_id: "<abc@example.com>".into(),
            from: "Ada <ada@example.com>".into(),
            to: "b@example.com".into(),
            subject: "".into(),
            date_display: "Thu, 2 Jul 2026 13:57:30 +0200".into(),
            flags: mp_protocol::listing::MessageFlags {
                seen: true,
                flagged: true,
                ..Default::default()
            },
            has_attachments: true,
            ..Default::default()
        };
        let listed = ListedMessage::from_wire("acct", "inbox", &row, None);
        assert_eq!(
            listed.selector.as_deref(),
            Some("mp://acct/inbox/abc@example.com"),
            "a row without the daemon's selector gets the canonical one"
        );
        assert_eq!(listed.date.as_deref(), Some("2026-07-02T11:57:30Z"));
        assert_eq!(
            listed.date_header.as_deref(),
            Some("Thu, 2 Jul 2026 13:57:30 +0200")
        );
        assert_eq!(listed.flags, vec!["read", "flagged"]);
        assert_eq!(listed.subject, None, "an empty subject is absent");
        assert!(listed.has_attachments);

        let value = serde_json::to_value(&listed).unwrap();
        assert!(value.get("body").is_none(), "no body key without --full");
        assert!(value.get("preview").is_none());
        assert_eq!(value["mailbox"], "inbox");
    }

    /// An unparsable `Date:` header falls back to the wire's UTC sort key.
    #[test]
    fn the_iso_date_falls_back_to_the_sort_key() {
        assert_eq!(
            iso_date("not a date", "2026-07-02T11:57:30").as_deref(),
            Some("2026-07-02T11:57:30Z")
        );
        assert_eq!(iso_date("not a date", ""), None);
    }

    /// A server hit carries the preview its text layout prints, or the whole
    /// body with `--full`, and no selector when the mailbox is unknown.
    #[test]
    fn a_fetched_hit_carries_a_preview_or_the_body() {
        let mut e = email("hit", &"x".repeat(400));
        // The fixture's weekday is wrong for the date, which RFC 2822 parsing
        // refuses, and a server hit has no sort key to fall back to.
        e.date = "Fri, 7 Aug 2026 10:00:00 +0000".into();
        e.flags = crate::types::MessageFlags::seen(true);

        let short = ListedMessage::from_fetched("acct", Some("archive"), &e, false);
        assert_eq!(
            short.selector.as_deref(),
            Some("mp://acct/archive/hit@example.com")
        );
        assert_eq!(short.preview.as_ref().map(String::len), Some(300));
        assert_eq!(short.body, None);
        assert_eq!(short.date.as_deref(), Some("2026-08-07T10:00:00Z"));
        assert_eq!(short.flags, vec!["read"]);

        let full = ListedMessage::from_fetched("acct", None, &e, true);
        assert_eq!(full.selector, None);
        assert_eq!(full.mailbox, None);
        assert_eq!(full.message_id, "<hit@example.com>");
        assert_eq!(full.body.as_ref().map(String::len), Some(400));
        assert_eq!(full.preview, None);
    }

    /// An account with rows in no listed mailbox says so instead of printing an
    /// empty frame.
    #[test]
    fn an_empty_listing_says_so() {
        colored::control::set_override(false);
        let text = render_list("acct", &[("Inbox".into(), 0, Vec::new())]);
        assert_eq!(text, "No messages in the local store for acct\n");
    }

    /// A server search whose `has:attachment` the local store answered says so
    /// on stdout, in the header and in the empty line, because an agent piping
    /// stdout never sees the stderr warning (PERSO-122).
    #[test]
    fn a_locally_answered_term_is_named_on_stdout() {
        let local = vec!["has:attachment".to_string()];
        let note = local_store_note(&local);
        assert_eq!(note.as_deref(), Some("local store only: has:attachment"));
        assert_eq!(
            crate::parse::fetched_emails_count(0, note.as_deref()),
            "(0 results, local store only: has:attachment)"
        );
        assert_eq!(
            crate::parse::fetched_emails_count(1, note.as_deref()),
            "(1 result, local store only: has:attachment)"
        );
        assert_eq!(
            no_server_results(&local),
            "No results found (local store only: has:attachment)"
        );

        // A search the server answered in full keeps the old header.
        assert_eq!(local_store_note(&[]), None);
        assert_eq!(crate::parse::fetched_emails_count(20, None), "(20 results)");
        assert_eq!(no_server_results(&[]), "No results found");
    }

    /// `mp search --json` carries the coverage a script tests before trusting
    /// an empty or short answer (PERSO-122).
    #[test]
    fn the_search_json_carries_its_coverage() {
        let render = crate::search::to_imap(&crate::search::parse("has:attachment").unwrap())
            .expect("plain IMAP renders has:attachment as a post-filter");
        let fallback = SearchResultsJson::server(
            "tum",
            "has:attachment",
            &render.local_terms(),
            false,
            Vec::new(),
        );
        let json = serde_json::to_value(&fallback).unwrap();
        assert_eq!(json["source"], "server");
        assert_eq!(json["coverage"], "local");
        assert_eq!(json["local_terms"], serde_json::json!(["has:attachment"]));
        assert_eq!(json["truncated"], false);
        assert_eq!(json["messages"], serde_json::json!([]));

        let full = SearchResultsJson::server("tum", "from:borrmann", &[], true, Vec::new());
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(json["coverage"], "server");
        assert_eq!(json["local_terms"], serde_json::json!([]));
        assert_eq!(json["truncated"], true);

        let offline = SearchResultsJson::local("tum", "ledger", false, Vec::new());
        let json = serde_json::to_value(&offline).unwrap();
        assert_eq!(json["source"], "local");
        assert_eq!(json["coverage"], "local");
        assert_eq!(json["local_terms"], serde_json::json!([]));
        assert_eq!(json["truncated"], false);
    }

    fn dated(subject: &str, date: &str) -> FetchedEmail {
        FetchedEmail {
            date: date.into(),
            ..email(subject, "")
        }
    }

    fn subjects(hits: &[(Option<String>, FetchedEmail)]) -> Vec<String> {
        hits.iter().map(|(_, e)| e.subject.clone()).collect()
    }

    /// The PERSO-175 case: an archive holding most of the hits and an inbox
    /// holding two old ones. Each mailbox answered the whole limit, so the
    /// newest hits win wherever they sit, and the cut says so.
    #[test]
    fn the_newest_hits_win_whichever_mailbox_holds_them() {
        let archive: Vec<_> = (10..=26)
            .map(|day| {
                dated(
                    &format!("archive-{day}"),
                    &format!("{day} Feb 2026 10:00:00 +0000"),
                )
            })
            .rev()
            .collect();
        let inbox = vec![
            dated("inbox-2", "2 Feb 2026 10:00:00 +0000"),
            dated("inbox-1", "1 Feb 2026 10:00:00 +0000"),
        ];
        let groups = vec![
            MailboxHits {
                mailbox: Some("inbox".into()),
                emails: inbox,
                more: false,
            },
            MailboxHits {
                mailbox: Some("archive".into()),
                emails: archive,
                more: false,
            },
        ];
        let (hits, truncated) = merge_server_hits(groups, 7);
        assert_eq!(
            subjects(&hits),
            [
                "archive-26",
                "archive-25",
                "archive-24",
                "archive-23",
                "archive-22",
                "archive-21",
                "archive-20"
            ]
        );
        assert!(hits.iter().all(|(mb, _)| mb.as_deref() == Some("archive")));
        assert!(truncated, "twelve hits were cut");
    }

    /// Everything fits: no cut, every mailbox keeps its id, newest first.
    #[test]
    fn a_search_that_fits_its_limit_is_not_truncated() {
        let groups = vec![
            MailboxHits {
                mailbox: Some("inbox".into()),
                emails: vec![dated("old", "1 Feb 2026 10:00:00 +0000")],
                more: false,
            },
            MailboxHits {
                mailbox: None,
                emails: vec![dated("new", "2 Feb 2026 10:00:00 +0000")],
                more: false,
            },
        ];
        let (hits, truncated) = merge_server_hits(groups, 2);
        assert_eq!(subjects(&hits), ["new", "old"]);
        assert_eq!(hits[0].0, None);
        assert_eq!(hits[1].0.as_deref(), Some("inbox"));
        assert!(!truncated);
    }

    /// A mailbox that filled its quota and held more truncates the answer
    /// even when the merge itself drops nothing.
    #[test]
    fn a_mailbox_holding_more_than_it_returned_truncates_the_answer() {
        let groups = vec![MailboxHits {
            mailbox: Some("inbox".into()),
            emails: vec![dated("only", "1 Feb 2026 10:00:00 +0000")],
            more: true,
        }];
        let (hits, truncated) = merge_server_hits(groups, 1);
        assert_eq!(hits.len(), 1);
        assert!(truncated);
        assert_eq!(
            more_results_line(1),
            "More results than the 1 shown: raise -n to see them."
        );
    }
}
