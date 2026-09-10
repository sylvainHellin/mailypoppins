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
    /// The `subject:` field, absent when the file has none.
    pub subject: Option<String>,
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
