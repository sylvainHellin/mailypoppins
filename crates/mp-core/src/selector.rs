//! The `mp://` selector contract: one grammar, one parser, one formatter.
//!
//! Paths are gone from every CLI input position (#0050). A message is named by
//!
//! ```text
//! selector := [ "mp://" account "/" ] [ mailbox "/" ] key
//!           | [ "mp://" account "/" ] mailbox "/" key "/" uid
//! ```
//!
//! and the canonical form every command *prints* is the fully qualified
//! `mp://<account>/<mailbox>/<key>`. Elision is positional and deterministic,
//! never sniffed: without the scheme the account comes from `-A/--account` or
//! the default account, and the mailbox comes from `--mailbox` or the
//! command's declared default scope. Segment counts alone decide what was
//! elided, which works because the key is percent-encoded and therefore
//! contains no raw `/`.
//!
//! The trailing `uid` names one copy when a mailbox holds two under one
//! Message-ID (PERSO-239): `mp://<account>/<mailbox>/<key>/<uid>`, or
//! `<mailbox>/<key>/<uid>` without the scheme. It is the store row's UID, the
//! number the daemon's `<mailbox>/<uid>` message id already carries, so it
//! needs the mailbox it is a UID in and is never elided into a shorter form.
//! Only the received namespace has it, and only a row that shares its
//! mailbox and key with another is printed with it, by the ambiguity refusal
//! and by every listing ([`Twins`], [`Selector::for_listed`], PERSO-244): the
//! canonical form stays UID-free, since a UID changes when the message moves.
//!
//! The parser never inspects the string to decide *what kind of thing* it is.
//! The [`Namespace`] is fixed by the command that called it: `mp archive` is
//! always received mail, `mp send` is always a draft. That is the whole reason
//! the two namespaces can share one grammar without a sniffing rule that would
//! silently reinterpret a key the day a Message-ID happens to look like a
//! draft id.
//!
//! Resolution (`selector::resolve_received`, `selector::resolve_draft`, which
//! stayed in the root crate because they read the store) is a single indexed
//! lookup. Zero matches names the namespace searched; more than one match (the
//! cross-mailbox copy) lists every fully qualified candidate and asks for
//! `--mailbox`, and never picks one. Two candidates in one mailbox are listed
//! with their `uid`, so every line of the refusal names a different message.
//!
//! # The effective account is the selector's, resolved before any store opens
//!
//! A selector's own `mp://<account>/…` segment overrides `-A/--account` and the
//! default, the same way its mailbox segment overrides `--mailbox`: naming a
//! thing inside the selector is the more specific statement. The consequence a
//! caller must honour is an ordering one. The account is resolved from the
//! selector *before* any store is opened or any server credential is loaded, so
//! a cross-account selector addresses its own account's index and transport.
//! Opening the default account's store first and resolving against it turns a
//! valid cross-account selector into a wrong-store miss whose error even prints
//! the selector's account as the scope it searched, which is actively
//! misleading (the #0073 follow-up bug: `mp delete mp://tum/drafts/<id>` under a
//! `perso` default). A command whose transport is bound before the selector is
//! parsed (`mp send`, `mp invite`) cannot honour a cross-account selector
//! cheaply; it fails loudly with "selector names account X but this command is
//! bound to Y" rather than operate on the wrong account. No command silently
//! resolves a selector against an account the selector did not name.

use std::collections::{HashMap, HashSet};
use std::fmt;

use anyhow::{anyhow, bail, Result};

/// What [`Selector::for_message`] reads off a received-mail row.
///
/// The row itself is a store type and stayed in the root crate (#0126,
/// P5-U10a), while the canonical selector of a row is a formatting decision
/// and belongs to the grammar. One trait with three accessors is what lets
/// `Selector::for_message` stay the inherent constructor it has always been,
/// called exactly as before, without this crate knowing what a `MessageRow`
/// is.
pub trait MessageRowRef {
    /// The mailbox the row was listed from.
    fn mailbox(&self) -> &str;
    /// The row's Message-ID, with or without its RFC 5322 angle brackets.
    fn message_id(&self) -> &str;
    /// The row's UID in its mailbox, which names its copy (PERSO-239).
    fn uid(&self) -> i64;
}

impl<T: MessageRowRef + ?Sized> MessageRowRef for &T {
    fn mailbox(&self) -> &str {
        (**self).mailbox()
    }
    fn message_id(&self) -> &str {
        (**self).message_id()
    }
    fn uid(&self) -> i64 {
        (**self).uid()
    }
}

/// The mailbox-and-key pairs that more than one row holds: the copies a
/// printed selector has to name by UID (PERSO-244).
///
/// A mailbox can hold two server copies under one Message-ID, one row each,
/// and the canonical selector of either names both, so a listing that printed
/// it would print one selector twice and the line would name neither copy.
/// Every surface that prints a selector per row asks this set whether the
/// row's pair is shared and, through [`Selector::for_listed`], prints the
/// UID-qualified selector when it is and the canonical one otherwise; the
/// ambiguity refusal asks it about its candidates ([`ambiguous`]).
///
/// The set holds the pairs that are shared *in the mailbox*, not in the
/// listing: a page or a deduplicated conversation can show one copy while its
/// twin is left out, and the canonical selector of the copy shown is no less
/// ambiguous for it. [`Twins::among`] answers for the rows it is given, which
/// is the mailbox when they are all of it; a caller holding fewer asks the
/// store, which inserts what it finds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Twins(HashSet<(String, String)>);

impl Twins {
    /// The pairs more than one of `rows` holds.
    pub fn among<'r, R: MessageRowRef + 'r>(rows: impl IntoIterator<Item = &'r R>) -> Twins {
        Twins::of_pairs(
            rows.into_iter()
                .map(|row| (row.mailbox(), row.message_id())),
        )
    }

    /// The pairs more than one of `pairs` (mailbox, Message-ID) holds.
    pub fn of_pairs<'p>(pairs: impl IntoIterator<Item = (&'p str, &'p str)>) -> Twins {
        let mut seen: HashMap<(&str, &str), bool> = HashMap::new();
        for (mailbox, message_id) in pairs {
            seen.entry((mailbox, message_key(message_id)))
                .and_modify(|shared| *shared = true)
                .or_insert(false);
        }
        Twins(
            seen.into_iter()
                .filter(|(_, shared)| *shared)
                .map(|((mailbox, key), _)| (mailbox.to_string(), key.to_string()))
                .collect(),
        )
    }

    /// Record that `mailbox` holds more than one row under `message_id`.
    pub fn insert(&mut self, mailbox: &str, message_id: &str) {
        self.0
            .insert((mailbox.to_string(), message_key(message_id).to_string()));
    }

    /// Whether `mailbox` holds more than one row under `message_id`.
    pub fn holds(&self, mailbox: &str, message_id: &str) -> bool {
        // A borrowed lookup would need a key type of its own; listings are
        // rows by the thousand and twins by the handful, so the empty set,
        // which is nearly every listing, answers without allocating.
        !self.0.is_empty()
            && self
                .0
                .contains(&(mailbox.to_string(), message_key(message_id).to_string()))
    }

    /// Whether no pair is shared.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The reserved mailbox segment of every draft selector. Drafts are local-only
/// files, so this is not a server folder name and can never collide with one:
/// a real IMAP `Drafts` folder syncs into the store as a `messages` row like
/// any other mailbox, in the received namespace.
pub const DRAFTS_MAILBOX: &str = "drafts";

/// The scheme prefix of a fully qualified selector.
pub const SCHEME: &str = "mp://";

/// Which key namespace a command addresses. Fixed by the command, never by the
/// shape of the string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    /// Received mail, resolved in `messages`. The key is the RFC 5322
    /// Message-ID without angle brackets, made total by the
    /// `sha256-<hex16>@local.invalid` synthesis rule ingest applies when the
    /// header is missing.
    Received,
    /// Local drafts, resolved in the `drafts` index. The mailbox segment is
    /// the reserved [`DRAFTS_MAILBOX`] and the key is the draft id.
    Drafts,
}

impl Namespace {
    /// The name used when reporting that nothing matched.
    pub fn label(self) -> &'static str {
        match self {
            Namespace::Received => "received mail",
            Namespace::Drafts => "drafts",
        }
    }
}

/// A canonical, fully qualified selector. This is what every command prints,
/// and it round-trips through [`parse`] unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub account: String,
    pub mailbox: String,
    pub key: String,
    /// The copy within the mailbox, for a key two rows share there
    /// (PERSO-239); `None` in the canonical form.
    pub uid: Option<i64>,
}

impl Selector {
    pub fn new(
        account: impl Into<String>,
        mailbox: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            account: account.into(),
            mailbox: mailbox.into(),
            key: key.into(),
            uid: None,
        }
    }

    /// The same selector narrowed to the copy at `uid` in its mailbox
    /// (PERSO-239).
    pub fn with_uid(self, uid: i64) -> Self {
        Self {
            uid: Some(uid),
            ..self
        }
    }

    /// The canonical selector of a received message row.
    ///
    /// The key is the Message-ID *without* its angle brackets (scope item 2):
    /// the brackets are RFC 5322 delimiters, not part of the identifier, and
    /// carrying them would make every printed selector end in `%3E` and every
    /// hand-typed one need shell quoting.
    pub fn for_message<R: MessageRowRef>(account: &str, row: &R) -> Self {
        Self::new(account, row.mailbox(), message_key(row.message_id()))
    }

    /// The selector a listing prints for `row` (PERSO-244): the canonical
    /// one, narrowed to the row's UID when its mailbox holds another row
    /// under the same Message-ID ([`Twins`]), so every line names its own
    /// copy.
    pub fn for_listed<R: MessageRowRef>(account: &str, row: &R, twins: &Twins) -> Self {
        let selector = Self::for_message(account, row);
        if twins.holds(row.mailbox(), row.message_id()) {
            selector.with_uid(row.uid())
        } else {
            selector
        }
    }

    /// The canonical selector of a draft.
    pub fn for_draft(account: &str, id: &str) -> Self {
        Self::new(account, DRAFTS_MAILBOX, id)
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{SCHEME}{}/{}/{}",
            encode(&self.account),
            encode(&self.mailbox),
            encode(&self.key)
        )?;
        match self.uid {
            Some(uid) => write!(f, "/{uid}"),
            None => Ok(()),
        }
    }
}

/// What one selector string said, before any default was applied. `None` means
/// the segment was elided, which is a different statement from naming it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorParts {
    pub account: Option<String>,
    pub mailbox: Option<String>,
    pub key: String,
    /// The trailing copy segment, which comes only with a mailbox.
    pub uid: Option<i64>,
}

/// A parsed selector with its defaults applied: the question the resolver
/// asks. `mailbox` stays optional for received mail, where an unqualified
/// selector deliberately searches every mailbox and reports the ambiguity
/// rather than picking one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorQuery {
    pub namespace: Namespace,
    pub account: String,
    pub mailbox: Option<String>,
    pub key: String,
    /// The one copy in `mailbox` the selector names (PERSO-239).
    pub uid: Option<i64>,
}

/// Parse a selector into its segments, applying no defaults.
///
/// The grammar is positional. With the scheme, two segments are
/// `account/key`, three are `account/mailbox/key` and four are
/// `account/mailbox/key/uid`; without it, one segment is `key`, two are
/// `mailbox/key` and three are `mailbox/key/uid`. A `uid` segment that is not
/// an integer, and anything longer, is rejected rather than re-split, because
/// a key that needed a `/` should have been encoded.
pub fn parse(input: &str) -> Result<SelectorParts> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("empty selector: expected {SCHEME}<account>/<mailbox>/<key>");
    }
    let (qualified, rest) = match trimmed.strip_prefix(SCHEME) {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };
    // The path heuristic is a guess, so it only runs where the input is
    // ambiguous. A string carrying the scheme has already said what it is, and
    // sniffing it would make the canonical form of any key ending in `.md`
    // (a Message-ID on a `.md` ccTLD, a draft id ending `.md`) unparseable.
    if !qualified && looks_like_path(trimmed) {
        bail!(
            "{trimmed} looks like a filesystem path; commands take a selector \
             ({SCHEME}<account>/<mailbox>/<key>), and `mp path <selector>` is the only \
             way back to a path"
        );
    }
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.iter().any(|s| s.is_empty()) {
        bail!("selector {trimmed} has an empty segment");
    }

    let uid = |segment: &str| segment.parse::<i64>().ok();
    let parts = match (qualified, segments.len()) {
        (true, 2) => SelectorParts {
            account: Some(decode(segments[0])?),
            mailbox: None,
            key: decode(segments[1])?,
            uid: None,
        },
        (true, 3) => SelectorParts {
            account: Some(decode(segments[0])?),
            mailbox: Some(decode(segments[1])?),
            key: decode(segments[2])?,
            uid: None,
        },
        (true, 4) if uid(segments[3]).is_some() => SelectorParts {
            account: Some(decode(segments[0])?),
            mailbox: Some(decode(segments[1])?),
            key: decode(segments[2])?,
            uid: uid(segments[3]),
        },
        (false, 1) => SelectorParts {
            account: None,
            mailbox: None,
            key: decode(segments[0])?,
            uid: None,
        },
        (false, 2) => SelectorParts {
            account: None,
            mailbox: Some(decode(segments[0])?),
            key: decode(segments[1])?,
            uid: None,
        },
        (false, 3) if uid(segments[2]).is_some() => SelectorParts {
            account: None,
            mailbox: Some(decode(segments[0])?),
            key: decode(segments[1])?,
            uid: uid(segments[2]),
        },
        _ => bail!(
            "selector {trimmed} has {} segments: expected {SCHEME}<account>/<mailbox>/<key> \
             or one of its elided forms (<mailbox>/<key>, <key>), with an optional /<uid> after \
             the key of a form that names its mailbox. A `/` inside a key must be \
             percent-encoded as %2F",
            segments.len()
        ),
    };
    Ok(parts)
}

/// Parse `input` in a fixed namespace and apply the command's defaults.
///
/// `account_default` is `-A/--account` or the first configured account;
/// `mailbox_flag` is the command's `--mailbox`, which the selector's own
/// mailbox segment overrides (naming it in the selector is more specific than
/// naming it beside it). In the drafts namespace the mailbox is always the
/// reserved [`DRAFTS_MAILBOX`], and any other value is rejected instead of
/// being quietly ignored.
pub fn parse_in(
    input: &str,
    namespace: Namespace,
    account_default: &str,
    mailbox_flag: Option<&str>,
) -> Result<SelectorQuery> {
    let parts = parse(input)?;
    let account = parts
        .account
        .or_else(|| non_empty(account_default))
        .ok_or_else(|| {
            anyhow!("no account for {input}: name one with -A/--account or configure a default")
        })?;

    let from_selector = parts.mailbox.is_some();
    let named_mailbox = parts.mailbox.or_else(|| mailbox_flag.map(str::to_string));
    let mailbox = match namespace {
        Namespace::Drafts if parts.uid.is_some() => {
            bail!(
                "{input} ends in a UID, which names one copy of a received message; a draft \
                 selector is {SCHEME}<account>/{DRAFTS_MAILBOX}/<id>"
            );
        }
        Namespace::Drafts => {
            if let Some(mb) = named_mailbox.as_deref() {
                // The likeliest mistake is a received-mail selector handed to
                // a draft command, `mp <selector>` above all, so the refusal
                // names the command that reads one. A mailbox given beside the
                // selector goes into the suggested one, fully qualified, so
                // the suggestion reads the message the command was pointed at.
                if mb != DRAFTS_MAILBOX {
                    if from_selector {
                        bail!(
                            "{input} names the mailbox {mb}, but a draft selector's mailbox \
                             segment is the reserved name `{DRAFTS_MAILBOX}`; to read received \
                             mail, run `mp show {input}`"
                        );
                    }
                    let received = Selector::new(&account, mb, &parts.key);
                    bail!(
                        "--mailbox {mb} names a received-mail mailbox, but a draft's mailbox is \
                         the reserved name `{DRAFTS_MAILBOX}`; to read received mail, run \
                         `mp show {received}`"
                    );
                }
            }
            Some(DRAFTS_MAILBOX.to_string())
        }
        Namespace::Received => {
            if named_mailbox.as_deref() == Some(DRAFTS_MAILBOX) {
                let named_by = if from_selector {
                    input.to_string()
                } else {
                    format!("--mailbox {DRAFTS_MAILBOX}")
                };
                bail!(
                    "{named_by} names the reserved mailbox `{DRAFTS_MAILBOX}`, which holds local \
                     drafts; this command addresses received mail"
                );
            }
            named_mailbox
        }
    };

    Ok(SelectorQuery {
        namespace,
        account,
        mailbox,
        key: parts.key,
        uid: parts.uid,
    })
}

/// The zero-match error of a draft lookup, for a resolver that answers from a
/// directory scan rather than from the index.
///
/// The daemon's `draft.path` is that resolver (P4-U6), and the sentence a user
/// sees may not depend on which process did the looking, so it is minted here
/// rather than spelled again over there.
pub fn draft_not_found(account: &str, key: &str) -> anyhow::Error {
    not_found(&SelectorQuery {
        namespace: Namespace::Drafts,
        account: account.to_string(),
        mailbox: Some(DRAFTS_MAILBOX.to_string()),
        key: key.to_string(),
        uid: None,
    })
}

/// The zero-match error: it names the namespace searched, so the reader can
/// tell "no such message" from "you asked the wrong index".
///
/// `pub` only because the two resolvers that raise it stayed in the root
/// crate; it is not part of the grammar's surface.
#[doc(hidden)]
pub fn not_found(query: &SelectorQuery) -> anyhow::Error {
    let scope = match query.mailbox.as_deref() {
        Some(mailbox) => format!("{}/{mailbox}", query.account),
        None => query.account.clone(),
    };
    let copy = match query.uid {
        Some(uid) => format!(" at UID {uid}"),
        None => String::new(),
    };
    anyhow!(
        "no match for {}{copy} in the {} index of {scope}",
        query.key,
        query.namespace.label()
    )
}

/// The multiple-match error: every candidate fully qualified, and the flag
/// that picks one.
///
/// Two candidates in one mailbox are two copies `--mailbox` cannot tell
/// apart, so they are listed with their UID (PERSO-239), and the refusal asks
/// for the selector itself: every line it prints names a different message.
///
/// `pub` for the same reason as [`not_found`].
#[doc(hidden)]
pub fn ambiguous(query: &SelectorQuery, candidates: Vec<(Selector, i64)>) -> anyhow::Error {
    let twins = Twins::of_pairs(
        candidates
            .iter()
            .map(|(s, _)| (s.mailbox.as_str(), s.key.as_str())),
    );
    let copies = !twins.is_empty();
    let list = candidates
        .iter()
        .map(|(s, uid)| {
            let s = if twins.holds(&s.mailbox, &s.key) {
                s.clone().with_uid(*uid)
            } else {
                s.clone()
            };
            format!("\n  {s}")
        })
        .collect::<String>();
    let pick = if copies {
        "name one by its selector"
    } else {
        "name one with --mailbox"
    };
    anyhow!(
        "{} matches {} messages in {}; {pick}:{list}",
        query.key,
        candidates.len(),
        query.account
    )
}

/// The selector key of a Message-ID: the identifier without the RFC 5322
/// angle brackets, whichever form the caller happens to hold.
#[doc(hidden)]
pub fn message_key(message_id: &str) -> &str {
    message_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// True for strings that are obviously filesystem paths, so the decline names
/// the real mistake instead of "no match for ./drafts/foo.md". Only ever
/// applied to unqualified input: see [`parse`].
fn looks_like_path(s: &str) -> bool {
    s.starts_with('/')
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with('~')
        || s.ends_with(".md")
}

// ---------------------------------------------------------------------------
// Percent-encoding
// ---------------------------------------------------------------------------

/// Bytes that survive [`encode`] unescaped: RFC 3986 unreserved plus `@`,
/// which every Message-ID contains and which is unambiguous inside a segment.
/// Everything else, `/` and `%` and whitespace above all, is escaped, which is
/// what makes splitting a selector on `/` total.
fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'@')
}

/// Percent-encode one selector segment.
pub fn encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for &b in segment.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-decode one selector segment. A truncated or non-hex escape is an
/// error rather than a literal `%`, so a mistyped selector cannot resolve to
/// something the user did not write.
pub fn decode(segment: &str) -> Result<String> {
    let bytes = segment.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3).ok_or_else(|| {
                anyhow!("selector segment {segment} ends in a truncated % escape")
            })?;
            let hi = (hex[0] as char)
                .to_digit(16)
                .ok_or_else(|| anyhow!("selector segment {segment} has a non-hex % escape"))?;
            let lo = (hex[1] as char)
                .to_digit(16)
                .ok_or_else(|| anyhow!("selector segment {segment} has a non-hex % escape"))?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out)
        .map_err(|_| anyhow!("selector segment {segment} decodes to invalid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys chosen to break a naive split-on-slash or a naive encoder: the
    /// characters the grammar promises never appear raw, plus unicode and the
    /// scheme itself as a payload.
    fn hostile_keys() -> Vec<String> {
        vec![
            "plain@example.com".to_string(),
            "has/slash@example.com".to_string(),
            "has%25percent@example.com".to_string(),
            "has percent % and space".to_string(),
            "has\ttab\nnewline".to_string(),
            "üñíçø∂é@例え.jp".to_string(),
            "mp://looks/like/a/selector".to_string(),
            "../../etc/passwd".to_string(),
            "sha256-0123456789abcdef@local.invalid".to_string(),
            "<already-bracketed@example.com>".to_string(),
            "trailing-dot.".to_string(),
            // A key that ends in `.md` is not a path: Message-IDs live on
            // `.md` ccTLD domains and draft ids can end that way too.
            "2026-07-31-note.md".to_string(),
            "newsletter@digital.md".to_string(),
            "%%%".to_string(),
            "a".repeat(300),
            "emoji \u{1F4E7} key".to_string(),
        ]
    }

    #[test]
    fn encode_decode_round_trips_over_hostile_keys() {
        for key in hostile_keys() {
            let encoded = encode(&key);
            assert!(!encoded.contains('/'), "{encoded} must not contain a raw /");
            assert!(
                !encoded.chars().any(char::is_whitespace),
                "{encoded} must not contain raw whitespace"
            );
            assert_eq!(decode(&encoded).unwrap(), key, "round trip of {key:?}");
        }
    }

    #[test]
    fn canonical_selectors_round_trip_through_the_parser() {
        for key in hostile_keys() {
            for mailbox in ["inbox", "archive", DRAFTS_MAILBOX, "Some Folder/Sub"] {
                let selector = Selector::new("work account", mailbox, key.clone());
                let printed = selector.to_string();
                let parts = parse(&printed).expect("canonical form parses");
                assert_eq!(parts.account.as_deref(), Some("work account"));
                assert_eq!(parts.mailbox.as_deref(), Some(mailbox));
                assert_eq!(parts.key, key);
            }
        }
    }

    #[test]
    fn elided_forms_take_the_defaults_positionally() {
        let q = parse_in("key@example.com", Namespace::Received, "work", None).unwrap();
        assert_eq!(q.account, "work");
        assert_eq!(q.mailbox, None);
        assert_eq!(q.key, "key@example.com");

        let q = parse_in("archive/key@example.com", Namespace::Received, "work", None).unwrap();
        assert_eq!(q.account, "work");
        assert_eq!(q.mailbox.as_deref(), Some("archive"));

        // The selector's own mailbox wins over the flag beside it.
        let q = parse_in(
            "archive/key@example.com",
            Namespace::Received,
            "work",
            Some("inbox"),
        )
        .unwrap();
        assert_eq!(q.mailbox.as_deref(), Some("archive"));

        let q = parse_in(
            "key@example.com",
            Namespace::Received,
            "work",
            Some("inbox"),
        )
        .unwrap();
        assert_eq!(q.mailbox.as_deref(), Some("inbox"));

        // Two segments after the scheme is account + key, not mailbox + key.
        let q = parse_in(
            "mp://home/key@example.com",
            Namespace::Received,
            "work",
            None,
        )
        .unwrap();
        assert_eq!(q.account, "home");
        assert_eq!(q.mailbox, None);
        assert_eq!(q.key, "key@example.com");
    }

    #[test]
    fn the_namespace_is_fixed_by_the_caller_not_the_string() {
        // The same string resolves in either namespace; nothing about it is
        // sniffed. Only the mailbox segment is namespace-checked.
        let received = parse_in("2026-07-31-note", Namespace::Received, "work", None).unwrap();
        let draft = parse_in("2026-07-31-note", Namespace::Drafts, "work", None).unwrap();
        assert_eq!(received.key, draft.key);
        assert_eq!(received.namespace, Namespace::Received);
        assert_eq!(draft.mailbox.as_deref(), Some(DRAFTS_MAILBOX));
    }

    #[test]
    fn a_draft_selector_rejects_a_non_drafts_mailbox() {
        let err = parse_in("inbox/some-id", Namespace::Drafts, "work", None).unwrap_err();
        assert!(err.to_string().contains("reserved name"), "{err}");
        assert!(
            err.to_string().contains("run `mp show inbox/some-id`"),
            "the refusal names the command that reads received mail: {err}"
        );
    }

    /// A mailbox given beside the selector rather than in it is carried into
    /// the suggested `mp show`, with the account, as one qualified selector.
    #[test]
    fn a_draft_selector_refusal_carries_a_mailbox_given_beside_it() {
        let err = parse_in("some-id", Namespace::Drafts, "work", Some("Sent Items")).unwrap_err();
        let err = err.to_string();
        assert!(err.contains("--mailbox Sent Items"), "{err}");
        assert!(
            err.contains("run `mp show mp://work/Sent%20Items/some-id`"),
            "the suggestion reads the message in the mailbox it was given: {err}"
        );

        let err = parse_in(
            "mp://home/some-id",
            Namespace::Drafts,
            "work",
            Some("inbox"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("run `mp show mp://home/inbox/some-id`"),
            "the selector's own account wins over the default: {err}"
        );
        let suggested = err.split('`').rev().nth(1).unwrap();
        let query = parse_in(
            suggested.strip_prefix("mp show ").unwrap(),
            Namespace::Received,
            "work",
            None,
        )
        .unwrap();
        assert_eq!(query.account, "home");
        assert_eq!(query.mailbox.as_deref(), Some("inbox"));
        assert_eq!(query.key, "some-id");
    }

    #[test]
    fn a_received_selector_rejects_the_reserved_drafts_mailbox() {
        let err = parse_in("drafts/some-id", Namespace::Received, "work", None).unwrap_err();
        assert!(err.to_string().contains("reserved mailbox"), "{err}");
        let err = parse_in("some-id", Namespace::Received, "work", Some("drafts")).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("--mailbox drafts names the reserved mailbox"),
            "the refusal names the flag that said it: {err}"
        );
    }

    #[test]
    fn paths_are_refused_with_the_reason_rather_than_searched() {
        for path in [
            "./drafts/2026-07-31.md",
            "/home/user/mail/inbox/message.md",
            "../other.md",
            "~/mail/x.md",
        ] {
            let err = parse(path).unwrap_err();
            assert!(
                err.to_string().contains("looks like a filesystem path"),
                "{path}: {err}"
            );
        }
    }

    /// The heuristic is for unqualified input only. Once the scheme is there
    /// the string has declared itself, and a key ending in `.md` (a `.md`
    /// ccTLD Message-ID, a draft id) must survive its own canonical form.
    #[test]
    fn the_path_heuristic_does_not_run_on_qualified_selectors() {
        let parts =
            parse("mp://work/inbox/newsletter@digital.md").expect("qualified .md key parses");
        assert_eq!(parts.key, "newsletter@digital.md");

        let parts =
            parse("mp://work/drafts/2026-07-31-note.md").expect("qualified .md draft id parses");
        assert_eq!(parts.mailbox.as_deref(), Some(DRAFTS_MAILBOX));
        assert_eq!(parts.key, "2026-07-31-note.md");
    }

    #[test]
    fn over_long_and_empty_selectors_are_rejected() {
        assert!(parse("").is_err());
        assert!(parse("a//b").is_err());
        assert!(
            parse("a/b/c").is_err(),
            "three unqualified segments are not a form"
        );
        assert!(parse("mp://a/b/c/d").is_err());
        assert!(parse("mp://a").is_err(), "the scheme needs at least a key");
    }

    /// The key is the bare Message-ID, so a selector can be typed without
    /// shell-quoting and printed without a trailing `%3E`. Both forms are
    /// accepted on input, because a user pasting from a mail header holds the
    /// bracketed one.
    #[test]
    fn angle_brackets_are_not_part_of_the_key() {
        assert_eq!(message_key("<a@example.com>"), "a@example.com");
        assert_eq!(message_key("a@example.com"), "a@example.com");
        assert_eq!(
            message_key("<sha256-0123456789abcdef@local.invalid>"),
            "sha256-0123456789abcdef@local.invalid"
        );
    }

    #[test]
    fn a_truncated_escape_is_an_error_not_a_literal_percent() {
        assert!(decode("%4").is_err());
        assert!(decode("%zz").is_err());
        assert!(decode("ok%20fine").is_ok());
    }

    /// A selector that names one copy round-trips through the parser with its
    /// UID, in the qualified and in the elided form; a draft has no copies
    /// (PERSO-239).
    #[test]
    fn a_copy_selector_round_trips_with_its_uid() {
        for key in hostile_keys() {
            for uid in [1, 285, 4294967295, -17] {
                let selector = Selector::new("work", "Some Folder/Sub", key.clone()).with_uid(uid);
                let printed = selector.to_string();
                assert!(printed.ends_with(&format!("/{uid}")), "{printed}");
                let parts = parse(&printed).expect("a copy selector parses");
                assert_eq!(parts.account.as_deref(), Some("work"));
                assert_eq!(parts.mailbox.as_deref(), Some("Some Folder/Sub"));
                assert_eq!(parts.key, key);
                assert_eq!(parts.uid, Some(uid));
            }
        }
        assert_eq!(
            Selector::new("work", "archive", "a@x").to_string(),
            "mp://work/archive/a@x",
            "the canonical form carries no UID"
        );

        let q = parse_in("archive/a@x/285", Namespace::Received, "work", None).unwrap();
        assert_eq!(
            (q.mailbox.as_deref(), q.key.as_str(), q.uid),
            (Some("archive"), "a@x", Some(285))
        );
        let q = parse_in("mp://home/archive/a@x", Namespace::Received, "work", None).unwrap();
        assert_eq!(q.uid, None);

        // A UID needs the mailbox it is a UID in, and is a number: anything
        // else is the old segment-count refusal, which names the %2F escape.
        for bad in [
            "mp://work/archive/a@x/285/1",
            "archive/a/b@x",
            "mp://w/archive/a/b@x",
        ] {
            let err = parse(bad).unwrap_err().to_string();
            assert!(err.contains("%2F"), "{bad}: {err}");
        }
        let err = parse_in("mp://work/drafts/d1/3", Namespace::Drafts, "work", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ends in a UID"), "{err}");
    }

    /// Two candidates in one mailbox are listed with their UID, so the refusal
    /// names two different selectors and asks for one of them; candidates in
    /// different mailboxes keep the canonical form and the --mailbox hint
    /// (PERSO-239).
    #[test]
    fn an_ambiguity_lists_a_distinct_selector_per_candidate() {
        let query = parse_in("a@x", Namespace::Received, "work", None).unwrap();
        let err = ambiguous(
            &query,
            vec![
                (Selector::new("work", "archive", "a@x"), 285),
                (Selector::new("work", "archive", "a@x"), 289),
                (Selector::new("work", "inbox", "a@x"), 3),
            ],
        )
        .to_string();
        assert_eq!(
            err,
            "a@x matches 3 messages in work; name one by its selector:\n  \
             mp://work/archive/a@x/285\n  mp://work/archive/a@x/289\n  mp://work/inbox/a@x"
        );

        let err = ambiguous(
            &query,
            vec![
                (Selector::new("work", "archive", "a@x"), 285),
                (Selector::new("work", "inbox", "a@x"), 3),
            ],
        )
        .to_string();
        assert_eq!(
            err,
            "a@x matches 2 messages in work; name one with --mailbox:\n  \
             mp://work/archive/a@x\n  mp://work/inbox/a@x"
        );
    }

    /// A row as a listing holds it, for [`Twins`] and [`Selector::for_listed`].
    struct Row(&'static str, &'static str, i64);

    impl MessageRowRef for Row {
        fn mailbox(&self) -> &str {
            self.0
        }
        fn message_id(&self) -> &str {
            self.1
        }
        fn uid(&self) -> i64 {
            self.2
        }
    }

    /// Two rows of one mailbox under one Message-ID each print the selector
    /// of their own copy, which parses back to it; a row alone in its mailbox
    /// prints the canonical form, the same Message-ID in another mailbox
    /// included, and the brackets do not make two keys (PERSO-244).
    #[test]
    fn a_listing_names_each_twin_by_its_uid_and_a_lone_row_by_its_key() {
        let rows = [
            Row("archive", "<a@x>", 285),
            Row("archive", "a@x", 289),
            Row("inbox", "<a@x>", 3),
            Row("archive", "<b@x>", 290),
        ];
        let twins = Twins::among(&rows);
        let printed: Vec<String> = rows
            .iter()
            .map(|row| Selector::for_listed("work", row, &twins).to_string())
            .collect();
        assert_eq!(
            printed,
            [
                "mp://work/archive/a@x/285",
                "mp://work/archive/a@x/289",
                "mp://work/inbox/a@x",
                "mp://work/archive/b@x",
            ]
        );
        let parts = parse(&printed[1]).unwrap();
        assert_eq!(
            (parts.mailbox.as_deref(), parts.key.as_str(), parts.uid),
            (Some("archive"), "a@x", Some(289))
        );

        // A set filled from elsewhere (the store, for a page that shows one
        // copy) narrows the one copy shown.
        let lone = Row("inbox", "<c@x>", 7);
        let mut twins = Twins::among([&lone]);
        assert!(twins.is_empty());
        assert_eq!(
            Selector::for_listed("work", &lone, &twins).to_string(),
            "mp://work/inbox/c@x"
        );
        twins.insert("inbox", "c@x");
        assert_eq!(
            Selector::for_listed("work", &lone, &twins).to_string(),
            "mp://work/inbox/c@x/7"
        );
    }
}
