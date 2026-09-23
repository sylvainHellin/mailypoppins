//! The results of the `draft.*` family (P4-U6).
//!
//! Wire shapes, so they live in the crate a client links rather than in the
//! daemon crate a client must never link, beside [`crate::events`]. Every one
//! of them is `Serialize + Deserialize`, which is what lets the CLI render from
//! the typed value the daemon sent instead of from a JSON object that happens
//! to look like it, and what makes the field order the struct's rather than a
//! map's.
//!
//! `draft.approve` and `draft.demote` answer a bare `{account, id, path,
//! status}` object instead of one of these: their four keys were frozen by
//! P3b-U10 and are pinned by `tests/daemon_draft_watch.rs`.
//!
//! Every `path` here is absolute and under `<data>/accounts/<account>/drafts/`.
//! The family is otherwise path-free: `mp path` is the one selector-to-path
//! edge in the product (DFT-06) and `mp edit` is a client-side editor session
//! on the file it names, so the fields whose whole point is a path carry one
//! and nothing else does.

use serde::{Deserialize, Serialize};

/// The received message a reply or a forward answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftSource {
    /// `"<mailbox>/<uid>"`, the way the read slice addresses a message.
    pub id: String,
    /// Its canonical selector, which is what `mp reply` prints first.
    pub selector: String,
}

/// A draft that was just written: `draft.create`, `draft.reply`,
/// `draft.forward`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftCreated {
    /// The account whose drafts directory holds it.
    pub account: String,
    /// The minted id, already in the file before the selector was handed out.
    pub id: String,
    /// The canonical selector, which is the form every command prints.
    pub selector: String,
    /// The file it was written to.
    pub path: String,
    /// The message it answers, absent for a draft made from nothing.
    pub source: Option<DraftSource>,
}

/// One row of `draft.list`, which is the index projection `mp list` prints.
///
/// `to` and `subject` stay optional, which is where this row differs from the
/// snapshot row [`crate::events::DraftChanged`]: the snapshot flattens a
/// missing subject to `""`, and `mp list` prints the dimmed subject line only
/// when the file has one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftEntry {
    /// The draft id, the `id:` field of the file.
    pub id: String,
    /// Its canonical selector.
    pub selector: String,
    /// The file it was read from.
    pub path: String,
    /// `draft`, `approved` or `sent`.
    pub status: String,
    /// The `to:` field, absent when the draft is bcc-only.
    pub to: Option<String>,
    /// The `cc:` field, absent when the draft copies nobody (P5-U4).
    ///
    /// Here because a TUI drafts list renders the same row a mailbox listing
    /// renders, and that row prints the Cc line; the CLI listing ignores it.
    pub cc: Option<String>,
    /// The `subject:` field, absent when the file has none.
    pub subject: Option<String>,
    /// The `date:` frontmatter field, absent when the file has none (P5-U4).
    ///
    /// The index's own column, not a mtime: a client sorts and displays a
    /// draft by the same `resolve_date` rule every other row goes through, and
    /// falls back to the `YYYY-MM-DD-…` filename stem in `path` when this is
    /// absent, exactly as the store-backed listing does.
    pub date: Option<String>,
    /// Whether the file parses.
    pub valid: bool,
    /// Whether it would send, which is a different question.
    pub ready: bool,
}

/// A file the refresh could not parse, named by path because a file with no
/// frontmatter has no id to be named by (#0080).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftSkip {
    /// The file that would not parse.
    pub path: String,
    /// The parser's reason, on one line.
    pub error: String,
}

/// Two files claiming one id, which makes one of them unaddressable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftCollision {
    /// The id both files carry.
    pub id: String,
    /// The file the index kept.
    pub kept: String,
    /// The file it shadows.
    pub shadowed: String,
}

/// The `result` of `draft.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftListing {
    /// The account that was listed.
    pub account: String,
    /// The rows, in the index's order (`mtime DESC, id ASC`), which the client
    /// prints without re-sorting.
    pub drafts: Vec<DraftEntry>,
    /// What the refresh skipped, in the order the CLI prints them.
    pub skipped: Vec<DraftSkip>,
    /// The id collisions it found, likewise.
    pub collisions: Vec<DraftCollision>,
}

/// One draft's diagnostics, as `mp validate` prints them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftReport {
    /// The draft id.
    pub id: String,
    /// Its canonical selector, which is what the line names.
    pub selector: String,
    /// Whether it validates.
    pub valid: bool,
    /// The single line printed after the dash, absent when it validates.
    pub error: Option<String>,
    /// The warnings the CLI joins with `", "`.
    pub warnings: Vec<String>,
}

/// The `result` of `draft.validate`: one report per draft, in listing order.
///
/// An invalid draft is an answer rather than a refusal, and the exit code stays
/// the client's decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftValidation {
    /// The account that was validated.
    pub account: String,
    /// The reports, in listing order.
    pub reports: Vec<DraftReport>,
}

/// The `result` of `draft.path`, the family's resolver: what the user typed,
/// answered as the canonical selector, the canonical path and the current
/// status.
///
/// The status is here because `mp mark-approved` needs the *previous* one to
/// decide between `✓ approved …` and `ℹ … is already approved`, and the
/// approval's own four keys are frozen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftLocation {
    /// The account the draft belongs to.
    pub account: String,
    /// The draft id.
    pub id: String,
    /// The canonical selector.
    pub selector: String,
    /// The canonical, absolute path.
    pub path: String,
    /// `draft`, `approved` or `sent`.
    pub status: String,
}

/// The `result` of `draft.preview`: the record the bare-selector dry run
/// renders, field for field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPreview {
    /// The account the draft belongs to.
    pub account: String,
    /// The draft id.
    pub id: String,
    /// The canonical selector.
    pub selector: String,
    /// The file, so a preview names the draft it previewed.
    pub path: String,
    /// The `from:` field if the file has one, the account's `default_from`
    /// otherwise.
    pub from: String,
    /// The `to:` field, absent for a bcc-only draft.
    pub to: Option<String>,
    /// The `cc:` field; an absent Cc prints no Cc line.
    pub cc: Option<String>,
    /// The `bcc:` field, likewise.
    pub bcc: Option<String>,
    /// The subject as the file spells it.
    pub subject: String,
    /// The body, cut at 500 **characters**.
    pub body: String,
    /// Whether the renderer's `...` line is on, which is decided on 500
    /// **bytes**: the two cut-offs of `draft::preview_draft` are not the same
    /// number, and a client re-deriving either would drift.
    pub body_truncated: bool,
    /// `draft`, `approved` or `sent`.
    pub status: String,
    /// Whether the draft validates.
    pub valid: bool,
    /// The validation error, absent when it validates.
    pub error: Option<String>,
    /// The validation warnings.
    pub warnings: Vec<String>,
    /// The configured body font, which the settings block prints.
    pub font_family: String,
    /// The configured body font size.
    pub font_size: String,
    /// Always `null` for the CLI dry run, because the body already carries the
    /// signature (#0099).
    pub signature: Option<String>,
}

/// Which draft `draft.create_from_message` builds (P5-U10d, #0126).
///
/// The wire spelling of `mailypoppins::draft::DraftFromSource`, which is
/// `Reply { all }` and `Forward`: a closed set of three words rather than a
/// verb plus a boolean, because `{"kind": "forward", "all": true}` would be a
/// parameter combination with no meaning and a client that sent it would have
/// to be told so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    /// A reply to the sender.
    Reply,
    /// A reply to the sender and every other recipient.
    ReplyAll,
    /// A forward, which quotes the message and carries no attachments here.
    Forward,
}

/// The message a draft is built from when the store holds no row for it
/// (P5-U10d, `DFT-08`, `DFT-09`).
///
/// The subset of [`crate::listing::ServerSearchHit`] that
/// `mp_core::draft::source_from_fetched` reads, under the hit's own field
/// names, so a client that holds a hit fills this in without renaming
/// anything. The fields a `SourceMessage` has no use for - `row_id`,
/// `selector`, `mailbox`, `reply_to`, `bcc`, the flags, `has_attachments`,
/// `is_invite` - are absent: a draft is built from the headers it quotes back
/// and the body it quotes.
///
/// **It carries no attachments.** A forward built from a stored row
/// materialises the original parts into the account's stable attachment
/// mirror; a server-only hit has no parts on the client's side to send, so a
/// forward built this way quotes the message and attaches nothing. That is
/// what the client does with such a hit today.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftMessage {
    /// The `From:` header, which the reply addresses.
    #[serde(default)]
    pub from: String,
    /// The `To:` header, which a reply-all keeps.
    #[serde(default)]
    pub to: String,
    /// The `Cc:` header, `null` when the message carried none.
    #[serde(default)]
    pub cc: Option<String>,
    /// The `Reply-To:` header, which a reply addresses instead of `From:`;
    /// `null` or absent when the message carried none, so a client that
    /// predates the field still builds a reply to `From:`.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// The `Subject:` header, which the builder prefixes with `Re:` or `Fwd:`.
    #[serde(default)]
    pub subject: String,
    /// The `Message-ID:` header, `null` for a server that returned none.
    ///
    /// Nullable and not required: the draft records it as `in_reply_to` when
    /// it is there and quotes fine without it, which is what makes this the
    /// one reply flow a message with no `Message-ID` can still have.
    #[serde(default)]
    pub message_id: Option<String>,
    /// The `Date:` header verbatim, which the quote header prints.
    #[serde(default)]
    pub date_display: String,
    /// The flattened text the quote is built from, `""` when the server sent
    /// none.
    #[serde(default)]
    pub body_text: String,
    /// The sender's markup, `null` when there is none, which is what the
    /// draft's HTML companion quotes.
    #[serde(default)]
    pub html_body: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The committed request fixture decodes into the typed payload, and the
    /// kind is one of the three words.
    #[test]
    fn the_committed_create_from_message_fixture_decodes_whole() {
        let raw = include_str!("../fixtures/draft.create_from_message.request.json");
        let request: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let params = &request["params"];

        let kind: DraftKind =
            serde_json::from_value(params["kind"].clone()).expect("the kind decodes");
        assert_eq!(kind, DraftKind::ReplyAll);

        let message: DraftMessage =
            serde_json::from_value(params["message"].clone()).expect("the message decodes");
        assert_eq!(message.from, "Ivana <ivana@example.com>");
        assert_eq!(message.cc.as_deref(), Some("team@example.com"));
        assert_eq!(
            message.message_id.as_deref(),
            Some("<Angebot@example.com>"),
            "the hit's own Message-ID, which the draft records as in_reply_to"
        );
        assert_eq!(message.body_text, "hier ist das Angebot\n");
        assert_eq!(message.html_body, None);
    }

    /// The committed response fixture is a `DraftCreated` whose `source` is
    /// `null`: the draft answers a message the store holds no row for, so
    /// there is nothing to name it by.
    #[test]
    fn a_draft_built_from_a_message_names_no_stored_source() {
        let raw = include_str!("../fixtures/draft.create_from_message.response.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let created: DraftCreated =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");

        assert_eq!(created.account, "work");
        assert_eq!(created.id, "0123456789abcdef");
        assert!(created.selector.starts_with("mp://work/drafts/"));
        assert!(created.path.ends_with(".md"));
        assert_eq!(
            created.source, None,
            "a message the store does not hold has no id and no selector to answer with"
        );
    }

    /// A message with no `Message-ID` and no Cc decodes, which is the hit a
    /// server returned neither for: the one reply flow that still works.
    #[test]
    fn a_message_without_an_id_still_decodes() {
        let message: DraftMessage = serde_json::from_value(json!({
            "from": "someone@example.com",
            "subject": "Kein Message-ID",
            "body_text": "text",
        }))
        .expect("every field defaults");
        assert_eq!(message.message_id, None);
        assert_eq!(message.cc, None);
        assert_eq!(message.to, "");
        assert_eq!(message.date_display, "");
    }

    /// The three kinds are three words on the wire, and a fourth is a decode
    /// error rather than a silent reply.
    #[test]
    fn the_draft_kinds_are_a_closed_set_of_three_words() {
        assert_eq!(
            serde_json::to_value(DraftKind::Forward).expect("it serialises"),
            json!("forward")
        );
        assert_eq!(
            serde_json::to_value(DraftKind::ReplyAll).expect("it serialises"),
            json!("reply_all")
        );
        let refused: Result<DraftKind, _> = serde_json::from_value(json!("bounce"));
        assert!(refused.is_err(), "the kind set is closed");
    }
}
