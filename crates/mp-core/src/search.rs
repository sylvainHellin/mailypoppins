//! The unified search grammar (#0086a): one parser, one AST, four renderers.
//!
//! Before this module `mp` shipped two query grammars that had drifted apart
//! (the #0043 debt): `imap_client::search`'s prefix scanner for the
//! server path, and `store::search`'s own term splitter for `--local` FTS.
//! Neither could express an `OR` group or an attachment predicate. This module
//! replaces both parsers with one, so a single input string means the same
//! thing whether it is answered by IMAP, Gmail, Microsoft Graph or the local
//! FTS index.
//!
//! The shape is deliberately small:
//!
//! ```text
//! Query   = Vec<Clause>                 // clauses are AND-ed together
//! Clause  = Single(Term) | Or(Vec<Term>)
//! Term    = From | To | Cc | Subject | Body | Text | Filename
//!         | HasAttachment | Before(date) | After(date)
//! ```
//!
//! Two things ride alongside the clauses rather than inside them, because they
//! are not match predicates: `in:MAILBOX` is a scope directive and
//! `message-id:X` is an exact-lookup directive. Both stay top-level and neither
//! may appear inside an `OR` group.
//!
//! Surface grammar (what a user types):
//! - fields `from: to: cc: subject: body: text: filename:`, the flag
//!   `has:attachment`, the dates `before:YYYY-MM-DD` / `after:YYYY-MM-DD`
//!   (with `since:` as an alias for `after:`), and the directives `in:` /
//!   `message-id:`;
//! - quoted phrases: `from:"Ada Lovelace"`, `"quarterly report"`;
//! - `OR` between terms, and parenthesised `(a OR b)` groups;
//! - a bare word is a `Text` term; adjacency is an implicit `AND`.
//!
//! A malformed query is an [`Err`] with a caret pointing at the offending
//! character, never a silent degradation to fewer conditions.

use std::fmt;

use crate::imap_query::parse_date_to_imap;

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

/// One match predicate. The string fields carry the value with its quotes
/// already stripped; the date variants carry a validated `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    From(String),
    To(String),
    Cc(String),
    Subject(String),
    Body(String),
    /// A free-text term (a bare word or an explicit `text:`), matched across
    /// every field a backend searches by default.
    Text(String),
    /// An attachment filename. Native on Gmail and Graph; a case-insensitive
    /// substring match on the stored attachment names on the local index;
    /// unsupported on plain IMAP, which says so rather than guess.
    Filename(String),
    /// `has:attachment`. Server-side on Gmail/Graph; a store post-filter on
    /// plain IMAP; a column predicate on the local index.
    HasAttachment,
    /// `before:YYYY-MM-DD` -> IMAP `BEFORE`.
    Before(String),
    /// `after:YYYY-MM-DD` (alias `since:`) -> IMAP `SINCE`.
    After(String),
}

/// One AND-ed slot of a [`Query`]: either a single term, or an `OR` of terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clause {
    Single(Term),
    Or(Vec<Term>),
}

/// A parsed query: the AND-ed clauses plus the two non-match directives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub clauses: Vec<Clause>,
    /// `in:MAILBOX` scope directive; the caller resolves and applies it.
    pub in_mailbox: Option<String>,
    /// `message-id:X` exact-lookup directive.
    pub message_id: Option<String>,
}

impl Query {
    /// True when nothing was asked: no clauses and no exact-lookup directive.
    /// An empty query is `ALL` on the server but "nothing to search for" on the
    /// local index, so each renderer decides what to do with it.
    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty() && self.message_id.is_none()
    }

    /// True when any clause carries a `has:attachment` term.
    pub fn wants_attachment(&self) -> bool {
        self.clauses.iter().any(|c| match c {
            Clause::Single(t) => matches!(t, Term::HasAttachment),
            Clause::Or(terms) => terms.iter().any(|t| matches!(t, Term::HasAttachment)),
        })
    }
}

// ---------------------------------------------------------------------------
// Parse error with a caret
// ---------------------------------------------------------------------------

/// A parse failure that points at where it went wrong.
///
/// `pos` is a byte offset into the original input; [`fmt::Display`] renders a
/// three-line message with a caret under the offending character so the user
/// sees *where*, not just *that*, the query is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub pos: usize,
    pub input: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The caret is aligned by character count, not byte count, so
        // multi-byte input still points at the right column in a monospace
        // terminal.
        let col = self.input[..self.pos.min(self.input.len())].chars().count();
        writeln!(f, "invalid search query: {}", self.message)?;
        writeln!(f, "  {}", self.input)?;
        write!(f, "  {}^", " ".repeat(col))
    }
}

impl std::error::Error for ParseError {}

fn err<T>(message: impl Into<String>, pos: usize, input: &str) -> Result<T, ParseError> {
    Err(ParseError {
        message: message.into(),
        pos,
        input: input.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    LParen(usize),
    RParen(usize),
    /// A word or `field:value`. `text` has quotes stripped; `quoted` is true
    /// when the token *began* with a `"`, which marks it as a literal phrase
    /// that must never be read as a `field:value` pair or an `OR` operator.
    Word {
        text: String,
        quoted: bool,
        pos: usize,
    },
}

/// Split the input into parens and words, honouring `"quoted phrases"`.
///
/// An unterminated quote closes at the end of the input rather than failing:
/// the value the user got half-way through typing is searched, which is the
/// behaviour the FTS overlay has always had. Parens are structural even with
/// no surrounding whitespace, so `(invoice` is two tokens.
fn tokenize(input: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let (pos, c) = chars[i];
        match c {
            c if c.is_whitespace() => {
                i += 1;
            }
            '(' => {
                toks.push(Tok::LParen(pos));
                i += 1;
            }
            ')' => {
                toks.push(Tok::RParen(pos));
                i += 1;
            }
            _ => {
                let start = pos;
                let began_quoted = c == '"';
                let mut text = String::new();
                let mut in_quotes = false;
                while i < chars.len() {
                    let (_, ch) = chars[i];
                    if ch == '"' {
                        in_quotes = !in_quotes;
                        i += 1;
                        continue;
                    }
                    if !in_quotes && (ch.is_whitespace() || ch == '(' || ch == ')') {
                        break;
                    }
                    text.push(ch);
                    i += 1;
                }
                toks.push(Tok::Word {
                    text,
                    quoted: began_quoted,
                    pos: start,
                });
            }
        }
    }
    toks
}

fn is_or(tok: &Tok) -> bool {
    matches!(tok, Tok::Word { text, quoted, .. } if !quoted && text.eq_ignore_ascii_case("or"))
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// What one `field:value` word resolves to.
enum Resolved {
    Term(Term),
    InMailbox(String),
    MessageId(String),
}

/// A strict calendar `YYYY-MM-DD`: `abcd-01-01` or `2024-02-31` is an error
/// here rather than garbage sent to the server.
fn valid_date(s: &str) -> bool {
    s.len() == 10
        && s.as_bytes()[4] == b'-'
        && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
        && parse_date_to_imap(s).is_some()
}

/// Interpret one word token into a [`Term`] or a directive.
fn resolve_word(text: &str, quoted: bool, pos: usize, input: &str) -> Result<Resolved, ParseError> {
    // A token that began with a quote is a literal phrase: never a field, an
    // operator, or a directive. `"from:x"` searches for the text `from:x`.
    if quoted {
        return Ok(Resolved::Term(Term::Text(text.to_string())));
    }
    let Some((field, value)) = text.split_once(':') else {
        return Ok(Resolved::Term(Term::Text(text.to_string())));
    };
    let field_lc = field.to_ascii_lowercase();
    // `has:` is the only field whose value is a keyword rather than free text.
    if field_lc == "has" {
        return if value.eq_ignore_ascii_case("attachment")
            || value.eq_ignore_ascii_case("attachments")
        {
            Ok(Resolved::Term(Term::HasAttachment))
        } else {
            err(
                format!("unknown has: value '{value}' (only has:attachment is supported)"),
                pos,
                input,
            )
        };
    }
    // An unknown field is not a field at all: a colon in a search box is far
    // more often part of a subject line than a field the user meant, so the
    // whole token is searched as the text it is (`re:budget`).
    let known = matches!(
        field_lc.as_str(),
        "from"
            | "to"
            | "cc"
            | "subject"
            | "body"
            | "text"
            | "filename"
            | "before"
            | "after"
            | "since"
            | "in"
            | "message-id"
    );
    if !known {
        return Ok(Resolved::Term(Term::Text(text.to_string())));
    }
    if value.is_empty() {
        return err(format!("{field_lc}: needs a value"), pos, input);
    }
    let term = match field_lc.as_str() {
        "from" => Term::From(value.to_string()),
        "to" => Term::To(value.to_string()),
        "cc" => Term::Cc(value.to_string()),
        "subject" => Term::Subject(value.to_string()),
        "body" => Term::Body(value.to_string()),
        "text" => Term::Text(value.to_string()),
        "filename" => Term::Filename(value.to_string()),
        "before" | "after" | "since" => {
            if !valid_date(value) {
                return err(
                    format!("{field_lc}: expects a date as YYYY-MM-DD, got '{value}'"),
                    pos,
                    input,
                );
            }
            if field_lc == "before" {
                Term::Before(value.to_string())
            } else {
                Term::After(value.to_string())
            }
        }
        "in" => return Ok(Resolved::InMailbox(value.to_string())),
        "message-id" => return Ok(Resolved::MessageId(value.to_string())),
        _ => unreachable!("field was checked as known"),
    };
    Ok(Resolved::Term(term))
}

/// Parse a user query into the shared AST.
///
/// The grammar is a whitespace-separated list of AND-ed *items*. An item is an
/// `OR`-run of factors, where a factor is a single term or a parenthesised
/// group `(a OR b)`. `OR` may join bare terms (`invoice OR receipt`) or appear
/// inside parens; either way the alternatives flatten into one [`Clause::Or`].
/// The directives `in:` and `message-id:` are top-level only.
pub fn parse(input: &str) -> Result<Query, ParseError> {
    let toks = tokenize(input);
    let mut query = Query::default();
    let mut i = 0;
    while i < toks.len() {
        // A leading/standalone `OR` has nothing to bind to.
        if is_or(&toks[i]) {
            let pos = tok_pos(&toks[i]);
            return err("OR must sit between two terms, e.g. (a OR b)", pos, input);
        }
        // Parse one OR-run: factor (OR factor)*.
        let mut or_terms: Vec<Term> = Vec::new();
        let mut directive: Option<Resolved> = None;
        loop {
            let terms = parse_factor(&toks, &mut i, input, &mut directive)?;
            if directive.is_some() {
                // A directive is its own item and cannot be OR-ed.
                if i < toks.len() && is_or(&toks[i]) {
                    let pos = tok_pos(&toks[i]);
                    return err(
                        "in: and message-id: cannot be part of an OR group",
                        pos,
                        input,
                    );
                }
                break;
            }
            or_terms.extend(terms);
            if i < toks.len() && is_or(&toks[i]) {
                i += 1; // consume OR
                if i >= toks.len() {
                    return err("OR must be followed by a term", input.len(), input);
                }
                continue;
            }
            break;
        }
        match directive {
            Some(Resolved::InMailbox(mb)) => query.in_mailbox = Some(mb),
            Some(Resolved::MessageId(mid)) => query.message_id = Some(mid),
            Some(Resolved::Term(_)) => unreachable!(),
            None => {
                if or_terms.len() == 1 {
                    query.clauses.push(Clause::Single(or_terms.pop().unwrap()));
                } else if !or_terms.is_empty() {
                    query.clauses.push(Clause::Or(or_terms));
                }
            }
        }
    }
    Ok(query)
}

/// The `mp search` flags that build the same AST as the positional grammar.
/// Each set field appends an AND-ed clause, so
/// `--from x --has-attachment "invoice OR receipt"` is the identical [`Query`]
/// as `from:x (invoice OR receipt) has:attachment`.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub from: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub filename: Option<String>,
    pub has_attachment: bool,
    pub after: Option<String>,
    pub before: Option<String>,
}

/// Build a [`Query`] from an optional positional query and the CLI flags.
///
/// The field flags are prepended (in a fixed order), the positional clauses
/// come next, and the dates and `has:attachment` are appended last, so a flag
/// invocation lowers to the very same clause vector as its positional twin.
pub fn from_cli(positional: &str, flags: &Flags) -> Result<Query, String> {
    let parsed = parse(positional).map_err(|e| e.to_string())?;
    let mut clauses: Vec<Clause> = Vec::new();
    for (opt, make) in [
        (&flags.from, Term::From as fn(String) -> Term),
        (&flags.to, Term::To),
        (&flags.cc, Term::Cc),
        (&flags.subject, Term::Subject),
        (&flags.body, Term::Body),
        (&flags.filename, Term::Filename),
    ] {
        if let Some(v) = opt {
            clauses.push(Clause::Single(make(v.clone())));
        }
    }
    clauses.extend(parsed.clauses);
    for (opt, is_after) in [(&flags.after, true), (&flags.before, false)] {
        if let Some(v) = opt {
            if !valid_date(v) {
                return Err(format!(
                    "--{} expects a date as YYYY-MM-DD, got '{v}'",
                    if is_after { "after" } else { "before" }
                ));
            }
            clauses.push(Clause::Single(if is_after {
                Term::After(v.clone())
            } else {
                Term::Before(v.clone())
            }));
        }
    }
    if flags.has_attachment {
        clauses.push(Clause::Single(Term::HasAttachment));
    }
    Ok(Query {
        clauses,
        in_mailbox: parsed.in_mailbox,
        message_id: parsed.message_id,
    })
}

// ---------------------------------------------------------------------------
// The search form both clients show
// ---------------------------------------------------------------------------

/// The fields of the search form the TUI's overlay and the desktop's Server
/// Search dialog show, and the desktop's list header fills as `keywords`.
///
/// [`SearchFields::build_query`] is the one lowering both clients call, so a
/// query typed anywhere means the same thing and refuses with the same words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFields {
    pub from: String,
    pub to: String,
    pub subject: String,
    /// Free text in the grammar, so `a OR b`, quoted phrases and `from:x` work.
    pub keywords: String,
    pub after: String,
    pub before: String,
    pub attachment: bool,
    /// A raw query that, when non-blank, replaces every other field.
    pub advanced: String,
}

impl SearchFields {
    /// True when the Advanced line is the active input: it takes precedence
    /// over every structured field once it holds anything non-blank.
    pub fn advanced_active(&self) -> bool {
        !self.advanced.trim().is_empty()
    }

    /// True when nothing was entered anywhere: neither the Advanced line nor
    /// a structured field nor the attachment toggle.
    pub fn is_blank(&self) -> bool {
        !self.advanced_active()
            && self.from.trim().is_empty()
            && self.to.trim().is_empty()
            && self.subject.trim().is_empty()
            && self.keywords.trim().is_empty()
            && self.after.trim().is_empty()
            && self.before.trim().is_empty()
            && !self.attachment
    }

    /// The [`Query`] the form describes, `Ok(None)` for a blank form.
    ///
    /// An active Advanced line is parsed on its own with [`parse`]; otherwise
    /// `keywords` is the positional grammar and the other fields are
    /// [`from_cli`]'s flags. The refusal is the parser's or the date check's
    /// message verbatim, a parse error with its caret line.
    pub fn build_query(&self) -> Result<Option<Query>, String> {
        if self.advanced_active() {
            return parse(self.advanced.trim())
                .map(Some)
                .map_err(|e| e.to_string());
        }
        if self.is_blank() {
            return Ok(None);
        }
        let opt = |s: &str| Some(s.trim().to_string()).filter(|t| !t.is_empty());
        let flags = Flags {
            from: opt(&self.from),
            to: opt(&self.to),
            cc: None,
            subject: opt(&self.subject),
            body: None,
            filename: None,
            has_attachment: self.attachment,
            after: opt(&self.after),
            before: opt(&self.before),
        };
        from_cli(self.keywords.trim(), &flags).map(Some)
    }
}

// ---------------------------------------------------------------------------
// Renderer: back to the grammar `parse` reads
// ---------------------------------------------------------------------------

/// A [`Query`] as a string [`parse`] reads back into the same query.
///
/// The inverse of [`parse`], and the reason it exists: `message.search` takes
/// what the user typed and builds the AST daemon-side ([`from_cli`]), while the
/// TUI's search overlay has already built one from its form fields. Rendering
/// the AST back into the grammar is what lets the overlay's local pass go
/// through the method instead of through a store of its own, without putting an
/// engine enum on the wire.
///
/// Every value is quoted, which is always safe: a token that begins with a
/// quote is a literal phrase, and `field:"value"` is the field form the
/// tokenizer already reads. The two shapes it cannot carry are the two [`parse`]
/// cannot produce either, so they are dropped rather than rendered wrong: a
/// field term with an empty value (which `parse` refuses) and a value
/// containing a `"` (which the tokenizer consumes as a delimiter). A
/// [`Clause::Or`] of one term renders as a group and parses back as a
/// [`Clause::Single`], which is the same query.
pub fn to_query_string(q: &Query) -> String {
    let mut parts: Vec<String> = Vec::new();
    for clause in &q.clauses {
        match clause {
            Clause::Single(term) => parts.extend(render_term(term)),
            Clause::Or(terms) => {
                let rendered: Vec<String> = terms.iter().filter_map(render_term).collect();
                if !rendered.is_empty() {
                    parts.push(format!("({})", rendered.join(" OR ")));
                }
            }
        }
    }
    if let Some(mailbox) = &q.in_mailbox {
        parts.push(format!("in:{}", quote_value(mailbox)));
    }
    if let Some(message_id) = &q.message_id {
        parts.push(format!("message-id:{}", quote_value(message_id)));
    }
    parts.join(" ")
}

/// One term as its grammar token, or `None` for a term the grammar cannot
/// carry back (see [`to_query_string`]).
fn render_term(term: &Term) -> Option<String> {
    let field = |name: &str, value: &String| {
        (!value.is_empty() && !value.contains('"'))
            .then(|| format!("{name}:{}", quote_value(value)))
    };
    match term {
        Term::From(v) => field("from", v),
        Term::To(v) => field("to", v),
        Term::Cc(v) => field("cc", v),
        Term::Subject(v) => field("subject", v),
        Term::Body(v) => field("body", v),
        Term::Filename(v) => field("filename", v),
        Term::Before(v) => field("before", v),
        Term::After(v) => field("after", v),
        // A free-text term is the one shape whose value may be empty: `""`
        // tokenizes as a quoted word and resolves to `Text("")`.
        Term::Text(v) => (!v.contains('"')).then(|| quote_value(v)),
        Term::HasAttachment => Some("has:attachment".to_string()),
    }
}

/// A value in the quotes the tokenizer strips again.
fn quote_value(value: &str) -> String {
    format!("\"{value}\"")
}

fn tok_pos(tok: &Tok) -> usize {
    match tok {
        Tok::LParen(p) | Tok::RParen(p) => *p,
        Tok::Word { pos, .. } => *pos,
    }
}

/// Parse one factor at `*i`: a group `( ... )` returns its OR-ed terms; a word
/// returns a single term, or sets `directive` and returns nothing.
fn parse_factor(
    toks: &[Tok],
    i: &mut usize,
    input: &str,
    directive: &mut Option<Resolved>,
) -> Result<Vec<Term>, ParseError> {
    match &toks[*i] {
        Tok::LParen(pos) => {
            let open = *pos;
            *i += 1;
            let mut terms: Vec<Term> = Vec::new();
            let mut expect_term = true;
            loop {
                if *i >= toks.len() {
                    return err("unclosed '(' in query", open, input);
                }
                match &toks[*i] {
                    Tok::RParen(rpos) => {
                        if expect_term {
                            return if terms.is_empty() {
                                err("empty group '()'", open, input)
                            } else {
                                err("expected a term after OR", *rpos, input)
                            };
                        }
                        *i += 1;
                        break;
                    }
                    Tok::LParen(lpos) => {
                        return err("nested '(' is not supported", *lpos, input);
                    }
                    tok if is_or(tok) => {
                        if expect_term {
                            return err("unexpected OR in group", tok_pos(tok), input);
                        }
                        expect_term = true;
                        *i += 1;
                    }
                    Tok::Word { text, quoted, pos } => {
                        if !expect_term {
                            return err("expected OR or ')' in group", *pos, input);
                        }
                        match resolve_word(text, *quoted, *pos, input)? {
                            Resolved::Term(t) => terms.push(t),
                            Resolved::InMailbox(_) | Resolved::MessageId(_) => {
                                return err(
                                    "in: and message-id: cannot appear inside a group",
                                    *pos,
                                    input,
                                );
                            }
                        }
                        expect_term = false;
                        *i += 1;
                    }
                }
            }
            Ok(terms)
        }
        Tok::RParen(pos) => err("unmatched ')'", *pos, input),
        Tok::Word { text, quoted, pos } => {
            let resolved = resolve_word(text, *quoted, *pos, input)?;
            *i += 1;
            match resolved {
                Resolved::Term(t) => Ok(vec![t]),
                other => {
                    *directive = Some(other);
                    Ok(Vec::new())
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Render error (for backends that cannot express a term)
// ---------------------------------------------------------------------------

/// A term the target backend cannot honestly express (e.g. `filename:` on
/// plain IMAP). Returned rather than dropped, so the query never silently
/// searches for less than the user asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderError(pub String);

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RenderError {}

fn imap_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `s` as the body of an IMAP quoted string, refused when it carries a CR, an
/// LF or a NUL: RFC 3501 allows none of them in a quoted string, and a CRLF
/// would end the `SEARCH` line and start a command of the value's choosing.
fn imap_quote_checked(s: &str) -> Result<String, RenderError> {
    if s.contains(['\r', '\n', '\0']) {
        return Err(RenderError(format!(
            "a search value cannot contain a line break or a NUL on IMAP: {:?}",
            s
        )));
    }
    Ok(imap_quote(s))
}

// ---------------------------------------------------------------------------
// Renderer: plain IMAP (RFC 3501)
// ---------------------------------------------------------------------------

/// The result of lowering a [`Query`] to an RFC 3501 `SEARCH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImapRender {
    /// The `SEARCH` criteria string; `ALL` when nothing narrows it.
    pub search: String,
    /// True when the query asked for `has:attachment`, which plain IMAP has no
    /// key for. The caller runs `search` server-side, then keeps only the
    /// results the local store marks as carrying an attachment, and warns that
    /// un-synced mail is not covered.
    pub attachment_postfilter: bool,
}

impl ImapRender {
    /// The terms this render leaves to the local store, in the grammar's own
    /// spelling: what `mp search` names in its warning, its result header and
    /// its `--json` `local_terms`, so a reader knows un-synced mail is not
    /// covered. Empty when the server answers every term.
    pub fn local_terms(&self) -> Vec<String> {
        if self.attachment_postfilter {
            vec!["has:attachment".to_string()]
        } else {
            Vec::new()
        }
    }
}

/// One term as an RFC 3501 search key. `HasAttachment` never reaches here (it
/// is stripped before rendering); it is an error inside an `OR` group because
/// a post-filter cannot resolve half of an alternation.
fn imap_term(term: &Term) -> Result<String, RenderError> {
    Ok(match term {
        Term::From(s) => format!("FROM \"{}\"", imap_quote_checked(s)?),
        Term::To(s) => format!("TO \"{}\"", imap_quote_checked(s)?),
        Term::Cc(s) => format!("CC \"{}\"", imap_quote_checked(s)?),
        Term::Subject(s) => format!("SUBJECT \"{}\"", imap_quote_checked(s)?),
        Term::Body(s) => format!("BODY \"{}\"", imap_quote_checked(s)?),
        Term::Text(s) => format!("TEXT \"{}\"", imap_quote_checked(s)?),
        Term::Before(d) => format!(
            "BEFORE {}",
            parse_date_to_imap(d).ok_or_else(|| RenderError(format!("bad date {d}")))?
        ),
        Term::After(d) => format!(
            "SINCE {}",
            parse_date_to_imap(d).ok_or_else(|| RenderError(format!("bad date {d}")))?
        ),
        Term::Filename(_) => {
            return Err(RenderError(
                "filename: search is not supported on plain IMAP; use a Gmail or Exchange account, or --local".into(),
            ))
        }
        Term::HasAttachment => {
            return Err(RenderError(
                "has:attachment cannot be combined with OR on plain IMAP".into(),
            ))
        }
    })
}

/// Nest binary `OR`s over an alternation: `[a,b,c]` -> `OR a OR b c`.
fn imap_or(terms: &[String]) -> String {
    match terms {
        [] => String::new(),
        [one] => one.clone(),
        [head, tail @ ..] => format!("OR {} {}", head, imap_or(tail)),
    }
}

/// Lower a [`Query`] to an RFC 3501 `SEARCH` string, stripping `has:attachment`
/// (which plain IMAP cannot express) and flagging it for a store post-filter.
pub fn to_imap(q: &Query) -> Result<ImapRender, RenderError> {
    let mut parts: Vec<String> = Vec::new();
    let mut attachment_postfilter = false;

    if let Some(ref mid) = q.message_id {
        parts.push(format!(
            "HEADER \"Message-ID\" \"{}\"",
            imap_quote_checked(&crate::imap_query::bracketed_message_id(mid))?
        ));
    }

    for clause in &q.clauses {
        match clause {
            Clause::Single(Term::HasAttachment) => {
                attachment_postfilter = true;
            }
            Clause::Single(t) => parts.push(imap_term(t)?),
            Clause::Or(terms) => {
                let rendered: Result<Vec<String>, RenderError> =
                    terms.iter().map(imap_term).collect();
                parts.push(imap_or(&rendered?));
            }
        }
    }

    let search = if parts.is_empty() {
        "ALL".to_string()
    } else {
        parts.join(" ")
    };
    Ok(ImapRender {
        search,
        attachment_postfilter,
    })
}

// ---------------------------------------------------------------------------
// Renderer: Gmail X-GM-RAW
// ---------------------------------------------------------------------------

fn gmail_value(s: &str) -> String {
    if s.chars().any(char::is_whitespace) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

fn gmail_date(d: &str) -> String {
    d.replace('-', "/")
}

fn gmail_term(term: &Term) -> String {
    match term {
        Term::From(s) => format!("from:{}", gmail_value(s)),
        Term::To(s) => format!("to:{}", gmail_value(s)),
        Term::Cc(s) => format!("cc:{}", gmail_value(s)),
        Term::Subject(s) => format!("subject:{}", gmail_value(s)),
        // Gmail has no body: operator; a bare word already searches the body.
        Term::Body(s) | Term::Text(s) => gmail_value(s),
        Term::Filename(s) => format!("filename:{}", gmail_value(s)),
        Term::HasAttachment => "has:attachment".to_string(),
        Term::Before(d) => format!("before:{}", gmail_date(d)),
        Term::After(d) => format!("after:{}", gmail_date(d)),
    }
}

/// Lower a [`Query`] to a Gmail search string for `X-GM-RAW`. Everything,
/// including `has:attachment`, runs server-side; the caller wraps the result as
/// `X-GM-RAW "<escaped>"`.
pub fn to_gmail(q: &Query) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ref mid) = q.message_id {
        parts.push(format!(
            "rfc822msgid:{}",
            crate::imap_query::normalize_message_id(mid)
        ));
    }
    for clause in &q.clauses {
        match clause {
            Clause::Single(t) => parts.push(gmail_term(t)),
            Clause::Or(terms) => {
                let inner: Vec<String> = terms.iter().map(gmail_term).collect();
                parts.push(format!("({})", inner.join(" OR ")));
            }
        }
    }
    parts.join(" ")
}

/// The full `X-GM-RAW "..."` IMAP search command for a Gmail query, refused
/// when a value carries a line break or a NUL (the same rule as `to_imap`).
pub fn to_gmail_search_command(q: &Query) -> Result<String, RenderError> {
    Ok(format!(
        "X-GM-RAW \"{}\"",
        imap_quote_checked(&to_gmail(q))?
    ))
}

// ---------------------------------------------------------------------------
// Renderer: Microsoft Graph
// ---------------------------------------------------------------------------

/// What one Graph message request asks the server for (PERSO-173).
///
/// Graph refuses `$search` beside `$filter` (`SearchWithFilter`) or beside
/// `$orderby` (`SearchWithOrderBy`) on a message collection, and refuses a
/// `$filter` beside `$orderby=receivedDateTime` unless the filter starts with
/// `receivedDateTime` (`InefficientFilter`). So a query takes exactly one
/// path: every match term goes into one KQL `$search`, and only the exact
/// Message-ID lookup, which KQL cannot express, goes into a `$filter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphQuery {
    /// No term at all: the folder's newest messages, sorted server-side.
    All,
    /// A KQL expression for `$search`, without the surrounding double quotes
    /// the URL builder adds.
    Search(String),
    /// An OData `$filter` expression, sent without `$orderby`.
    Filter(String),
}

/// One term as a KQL restriction.
///
/// `from:`, `to:` and `cc:` match a display name or any token of the address
/// by prefix (`from:amaz` finds `order-update@amazon.de`), which is as close
/// to the IMAP substring match as Graph offers; an OData `eq` would match only
/// the full address.
fn kql_term(term: &Term) -> String {
    match term {
        Term::From(s) => format!("from:{}", kql_value(s)),
        Term::To(s) => format!("to:{}", kql_value(s)),
        Term::Cc(s) => format!("cc:{}", kql_value(s)),
        Term::Subject(s) => format!("subject:{}", kql_value(s)),
        Term::Body(s) => format!("body:{}", kql_value(s)),
        Term::Filename(s) => format!("attachment:{}", kql_value(s)),
        Term::Text(s) => kql_value(s),
        Term::HasAttachment => "hasAttachments:true".to_string(),
        Term::After(d) => format!("received>={d}"),
        Term::Before(d) => format!("received<{d}"),
    }
}

/// A value as KQL reads it inside the double-quoted `$search` string.
///
/// A plain token stays bare, which keeps Graph's prefix match. Anything KQL
/// would split or read as syntax (whitespace, a parenthesis, a colon, a
/// comparison, a leading sign, an operator word) becomes a phrase in escaped
/// double quotes. A phrase cannot carry a double quote, so one is replaced by
/// a space, and a backslash is escaped for the outer string.
fn kql_value(s: &str) -> String {
    let needs_phrase = s.is_empty()
        || s.starts_with(['-', '+'])
        || matches!(s, "AND" | "OR" | "NOT" | "NEAR" | "ONEAR")
        || s.chars().any(|c| {
            c.is_whitespace() || matches!(c, '"' | '\\' | '(' | ')' | ':' | '<' | '>' | '=')
        });
    if !needs_phrase {
        return s.to_string();
    }
    let inner = s.replace('\\', "\\\\").replace('"', " ");
    format!("\\\"{inner}\\\"")
}

/// Lower a [`Query`] to the one Graph request that answers it.
///
/// Clauses are joined with an explicit `AND`: KQL ORs two adjacent
/// restrictions on the same property, so `received>=A received<B` would match
/// everything after `A` (seen live on PERSO-173). A `message-id:` lookup is a
/// `$filter` of its own and is refused beside any other term, since Graph
/// cannot combine the two halves in one request.
pub fn to_graph(q: &Query) -> Result<GraphQuery, RenderError> {
    if let Some(ref mid) = q.message_id {
        if !q.clauses.is_empty() {
            return Err(RenderError(
                "message-id: cannot be combined with other terms on Microsoft Graph; search by the Message-ID alone".into(),
            ));
        }
        return Ok(GraphQuery::Filter(format!(
            "internetMessageId eq '{}'",
            crate::imap_query::bracketed_message_id(mid).replace('\'', "''")
        )));
    }
    if q.clauses.is_empty() {
        return Ok(GraphQuery::All);
    }
    let parts: Vec<String> = q
        .clauses
        .iter()
        .map(|clause| match clause {
            Clause::Single(t) => kql_term(t),
            Clause::Or(terms) => format!(
                "({})",
                terms.iter().map(kql_term).collect::<Vec<_>>().join(" OR ")
            ),
        })
        .collect();
    Ok(GraphQuery::Search(parts.join(" AND ")))
}

// ---------------------------------------------------------------------------
// Renderer: local FTS
// ---------------------------------------------------------------------------

/// The result of lowering a [`Query`] to the local FTS index. `match_expr` is
/// the FTS5 `MATCH` string (absent when the query has only attachment,
/// filename or date predicates); the rest are SQL predicates the index cannot
/// express.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FtsRender {
    pub match_expr: Option<String>,
    pub has_attachment: bool,
    /// `filename:` terms, answered from the stored attachment names rather
    /// than the index. Each inner list is one OR group of case-insensitive
    /// substrings (a lone `filename:x` is a group of one); the groups are
    /// AND-ed. A trailing `*` is already stripped, since a substring match
    /// subsumes a prefix one.
    pub filenames: Vec<Vec<String>>,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// True when the term carries something an FTS5 tokenizer would index.
fn indexable(term: &str) -> bool {
    term.chars().any(char::is_alphanumeric)
}

/// One text term as an FTS5 column filter. A trailing `*` is a prefix query;
/// the value is double-quoted so arbitrary punctuation is safe. Returns `None`
/// when nothing indexable survives, so the caller can drop it.
fn fts_text_term(column: Option<&str>, value: &str) -> Option<String> {
    let mut v = value.to_string();
    let prefix = v.ends_with('*');
    if prefix {
        v.pop();
    }
    if !indexable(&v) {
        return None;
    }
    let escaped = v.replace('"', "\"\"");
    let star = if prefix { "*" } else { "" };
    Some(match column {
        Some(col) => format!("{col}:\"{escaped}\"{star}"),
        None => format!("\"{escaped}\"{star}"),
    })
}

/// A `filename:` value as the substring the store matches: a trailing `*` is
/// a prefix query elsewhere, and a substring match already covers it.
fn filename_needle(value: &str) -> String {
    value.strip_suffix('*').unwrap_or(value).to_string()
}

/// Map a text term to its FTS column, or report the fields the index cannot
/// answer (`to`/`cc` are not indexed).
fn fts_term(term: &Term) -> Result<Option<String>, RenderError> {
    match term {
        Term::From(s) => Ok(fts_text_term(Some("from_"), s)),
        Term::Subject(s) => Ok(fts_text_term(Some("subject"), s)),
        Term::Body(s) => Ok(fts_text_term(Some("body_text"), s)),
        Term::Text(s) => Ok(fts_text_term(None, s)),
        Term::To(_) | Term::Cc(_) => Err(RenderError(
            "to: and cc: are not indexed by --local; drop --local to search them server-side"
                .into(),
        )),
        Term::Filename(_) | Term::HasAttachment | Term::Before(_) | Term::After(_) => {
            unreachable!("attachment/filename/date handled by the clause walker")
        }
    }
}

/// Lower a [`Query`] to an FTS `MATCH` expression plus the SQL predicates
/// (attachment, attachment name, date range) the contentless index cannot
/// carry. This is the renderer that closes the #0043 two-grammar debt:
/// `--local` now parses the same input as the server path.
pub fn to_fts(q: &Query) -> Result<FtsRender, RenderError> {
    if q.message_id.is_some() {
        return Err(RenderError(
            "message-id: search is not available on --local".into(),
        ));
    }
    let mut render = FtsRender::default();
    let mut match_parts: Vec<String> = Vec::new();

    for clause in &q.clauses {
        match clause {
            Clause::Single(Term::HasAttachment) => render.has_attachment = true,
            Clause::Single(Term::Filename(f)) => render.filenames.push(vec![filename_needle(f)]),
            // An OR group of names alone is one attachment-name predicate. A
            // group mixing a name with an indexed term would need the SQL
            // predicate and the FTS `MATCH` OR-ed together, which this split
            // cannot express, so it is refused like the date/attachment mix.
            Clause::Or(terms) if terms.iter().all(|t| matches!(t, Term::Filename(_))) => {
                render.filenames.push(
                    terms
                        .iter()
                        .filter_map(|t| match t {
                            Term::Filename(f) => Some(filename_needle(f)),
                            _ => None,
                        })
                        .collect(),
                );
            }
            // Every date term narrows, as it does in `to_imap`: the latest
            // `after` and the earliest `before` win. A date is a strict
            // `YYYY-MM-DD` by the time it is a term, so the strings order as
            // the dates do.
            Clause::Single(Term::Before(d)) => {
                if render.before.as_ref().is_none_or(|kept| d < kept) {
                    render.before = Some(d.clone());
                }
            }
            Clause::Single(Term::After(d)) => {
                if render.after.as_ref().is_none_or(|kept| d > kept) {
                    render.after = Some(d.clone());
                }
            }
            Clause::Single(t) => {
                if let Some(part) = fts_term(t)? {
                    match_parts.push(part);
                }
            }
            Clause::Or(terms) => {
                let mut inner: Vec<String> = Vec::new();
                for t in terms {
                    match t {
                        Term::HasAttachment | Term::Before(_) | Term::After(_) => {
                            return Err(RenderError(
                                "an OR group with attachment or date terms is not supported on --local".into(),
                            ))
                        }
                        Term::Filename(_) => {
                            return Err(RenderError(
                                "an OR group mixing filename: with other terms is not supported on --local".into(),
                            ))
                        }
                        _ => {
                            if let Some(part) = fts_term(t)? {
                                inner.push(part);
                            }
                        }
                    }
                }
                if inner.len() == 1 {
                    match_parts.push(inner.pop().unwrap());
                } else if !inner.is_empty() {
                    match_parts.push(format!("({})", inner.join(" OR ")));
                }
            }
        }
    }

    render.match_expr = (!match_parts.is_empty()).then(|| match_parts.join(" "));
    Ok(render)
}

/// Back-compat shim (#0043): the old `fts_expression` name, now a *renderer* of
/// the shared AST rather than a second parser. Returns the FTS `MATCH` string
/// and errors when the query has nothing searchable, which is what the local
/// search command and its tests expect.
pub fn fts_expression(query: &str) -> anyhow::Result<String> {
    let parsed = parse(query).map_err(|e| anyhow::anyhow!("{e}"))?;
    let render = to_fts(&parsed).map_err(|e| anyhow::anyhow!("{e}"))?;
    match render.match_expr {
        Some(expr) => Ok(expr),
        None => anyhow::bail!("nothing to search for: the query has no searchable words"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(input: &str) -> Query {
        parse(input).expect("should parse")
    }

    // -- the search form ----------------------------------------------------

    fn keywords(text: &str) -> SearchFields {
        SearchFields {
            keywords: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_blank_form_builds_no_query() {
        assert_eq!(SearchFields::default().build_query(), Ok(None));
        assert_eq!(keywords("   ").build_query(), Ok(None));
    }

    #[test]
    fn the_form_lowers_through_from_cli_and_the_advanced_line_takes_over() {
        let form = SearchFields {
            from: " ada ".into(),
            keywords: "report".into(),
            after: "2026-01-01".into(),
            attachment: true,
            ..Default::default()
        };
        let flags = Flags {
            from: Some("ada".into()),
            has_attachment: true,
            after: Some("2026-01-01".into()),
            ..Default::default()
        };
        assert_eq!(
            form.build_query(),
            Ok(Some(from_cli("report", &flags).expect("cli")))
        );
        let advanced = SearchFields {
            advanced: "to:bob".into(),
            ..form
        };
        assert_eq!(advanced.build_query(), Ok(Some(q("to:bob"))));
    }

    /// `from: borrmann`, a space after the colon, is a field with no value:
    /// both clients refuse it with these three lines, the caret under the
    /// field (PERSO-115).
    #[test]
    fn a_field_with_a_space_before_its_value_is_refused_with_the_caret() {
        assert_eq!(
            keywords("from: borrmann").build_query(),
            Err("invalid search query: from: needs a value\n  from: borrmann\n  ^".to_string())
        );
        assert_eq!(
            keywords("from:borrmann").build_query(),
            Ok(Some(q("from:borrmann")))
        );
    }

    // -- parser -------------------------------------------------------------

    #[test]
    fn bare_words_become_and_ed_text_terms() {
        assert_eq!(
            q("urgent meeting").clauses,
            vec![
                Clause::Single(Term::Text("urgent".into())),
                Clause::Single(Term::Text("meeting".into())),
            ]
        );
    }

    #[test]
    fn fields_parse_to_their_terms() {
        assert_eq!(
            q("from:ada to:bob cc:x subject:s body:b text:t").clauses,
            vec![
                Clause::Single(Term::From("ada".into())),
                Clause::Single(Term::To("bob".into())),
                Clause::Single(Term::Cc("x".into())),
                Clause::Single(Term::Subject("s".into())),
                Clause::Single(Term::Body("b".into())),
                Clause::Single(Term::Text("t".into())),
            ]
        );
    }

    #[test]
    fn quoted_phrase_is_one_term() {
        assert_eq!(
            q("\"quarterly report\"").clauses,
            vec![Clause::Single(Term::Text("quarterly report".into()))]
        );
        assert_eq!(
            q("from:\"Ada Lovelace\"").clauses,
            vec![Clause::Single(Term::From("Ada Lovelace".into()))]
        );
    }

    #[test]
    fn a_quoted_token_is_never_a_field_or_operator() {
        assert_eq!(
            q("\"from:x\"").clauses,
            vec![Clause::Single(Term::Text("from:x".into()))]
        );
        assert_eq!(
            q("a \"OR\" b").clauses,
            vec![
                Clause::Single(Term::Text("a".into())),
                Clause::Single(Term::Text("OR".into())),
                Clause::Single(Term::Text("b".into())),
            ]
        );
    }

    #[test]
    fn unknown_field_is_searched_as_text() {
        assert_eq!(
            q("re:budget").clauses,
            vec![Clause::Single(Term::Text("re:budget".into()))]
        );
    }

    #[test]
    fn has_attachment_parses() {
        assert_eq!(
            q("has:attachment").clauses,
            vec![Clause::Single(Term::HasAttachment)]
        );
        assert!(parse("has:document").is_err());
    }

    #[test]
    fn dates_and_since_alias() {
        assert_eq!(
            q("after:2026-01-01 before:2026-07-01").clauses,
            vec![
                Clause::Single(Term::After("2026-01-01".into())),
                Clause::Single(Term::Before("2026-07-01".into())),
            ]
        );
        assert_eq!(
            q("since:2026-01-01").clauses,
            vec![Clause::Single(Term::After("2026-01-01".into()))]
        );
    }

    #[test]
    fn bad_date_errors_with_position() {
        let e = parse("before:nope").unwrap_err();
        assert!(e.message.contains("YYYY-MM-DD"), "{}", e.message);
        assert_eq!(e.pos, 0);
        let e2 = parse("from:x before:2026-13-01").unwrap_err();
        assert_eq!(e2.pos, 7);
    }

    #[test]
    fn non_calendar_dates_are_rejected() {
        assert!(parse("after:abcd-01-01").is_err());
        assert!(parse("before:2024-02-31").is_err());
        assert!(parse("before:2023-02-29").is_err());
        assert!(parse("after:2024-02-29").is_ok());
    }

    #[test]
    fn or_group_with_parens() {
        assert_eq!(
            q("(invoice OR receipt)").clauses,
            vec![Clause::Or(vec![
                Term::Text("invoice".into()),
                Term::Text("receipt".into()),
            ])]
        );
    }

    #[test]
    fn bare_or_without_parens() {
        assert_eq!(
            q("invoice OR receipt").clauses,
            vec![Clause::Or(vec![
                Term::Text("invoice".into()),
                Term::Text("receipt".into()),
            ])]
        );
    }

    #[test]
    fn sylvains_example_parses() {
        assert_eq!(
            q("from:boss@corp.com (invoice OR receipt) has:attachment").clauses,
            vec![
                Clause::Single(Term::From("boss@corp.com".into())),
                Clause::Or(vec![
                    Term::Text("invoice".into()),
                    Term::Text("receipt".into()),
                ]),
                Clause::Single(Term::HasAttachment),
            ]
        );
    }

    #[test]
    fn or_flattens_across_a_group_for_three_way_nesting() {
        assert_eq!(
            q("(a OR b OR c)").clauses,
            vec![Clause::Or(vec![
                Term::Text("a".into()),
                Term::Text("b".into()),
                Term::Text("c".into()),
            ])]
        );
    }

    #[test]
    fn directives_are_top_level() {
        let query = q("in:Archive message-id:<a@b> from:alice");
        assert_eq!(query.in_mailbox, Some("Archive".into()));
        assert_eq!(query.message_id, Some("<a@b>".into()));
        assert_eq!(
            query.clauses,
            vec![Clause::Single(Term::From("alice".into()))]
        );
    }

    #[test]
    fn malformed_parens_error_with_caret() {
        let e = parse("from:x (invoice OR").unwrap_err();
        assert_eq!(e.pos, 7); // the '('
        assert!(e.message.contains("unclosed"), "{}", e.message);

        let e2 = parse("a )").unwrap_err();
        assert_eq!(e2.pos, 2);
        assert!(e2.message.contains("unmatched"), "{}", e2.message);

        let e3 = parse("()").unwrap_err();
        assert!(e3.message.contains("empty group"), "{}", e3.message);

        let e4 = parse("(a b)").unwrap_err();
        assert!(e4.message.contains("expected OR"), "{}", e4.message);

        let e5 = parse("(in:X OR a)").unwrap_err();
        assert!(
            e5.message.contains("cannot appear inside"),
            "{}",
            e5.message
        );
    }

    #[test]
    fn caret_display_points_at_the_offender() {
        let e = parse("from:x )").unwrap_err();
        let shown = format!("{e}");
        // Caret line is the third line, with the '^' under the ')'.
        let caret_line = shown.lines().nth(2).unwrap();
        let caret_col = caret_line.find('^').unwrap();
        // "  " prefix (2) + 7 chars before ')' = column 9.
        assert_eq!(caret_col, 2 + 7);
    }

    // -- to_imap ------------------------------------------------------------

    #[test]
    fn imap_renders_and_of_keys() {
        let r = to_imap(&q("from:alice subject:invoice")).unwrap();
        assert_eq!(r.search, "FROM \"alice\" SUBJECT \"invoice\"");
        assert!(!r.attachment_postfilter);
        assert!(r.local_terms().is_empty());
    }

    #[test]
    fn imap_empty_is_all() {
        assert_eq!(to_imap(&q("")).unwrap().search, "ALL");
    }

    #[test]
    fn imap_nests_or() {
        let r = to_imap(&q("(a OR b)")).unwrap();
        assert_eq!(r.search, "OR TEXT \"a\" TEXT \"b\"");
        let r3 = to_imap(&q("(a OR b OR c)")).unwrap();
        assert_eq!(r3.search, "OR TEXT \"a\" OR TEXT \"b\" TEXT \"c\"");
    }

    #[test]
    fn imap_strips_has_attachment_and_flags_postfilter() {
        let r = to_imap(&q("from:boss@corp.com (invoice OR receipt) has:attachment")).unwrap();
        assert_eq!(
            r.search,
            "FROM \"boss@corp.com\" OR TEXT \"invoice\" TEXT \"receipt\""
        );
        assert!(r.attachment_postfilter);
        assert_eq!(r.local_terms(), vec!["has:attachment".to_string()]);
    }

    #[test]
    fn imap_dates_and_message_id() {
        let r = to_imap(&q("after:2026-01-01 before:2026-07-01")).unwrap();
        assert_eq!(r.search, "SINCE 1-Jan-2026 BEFORE 1-Jul-2026");
        let r2 = to_imap(&q("message-id:abc@x")).unwrap();
        assert_eq!(r2.search, "HEADER \"Message-ID\" \"<abc@x>\"");
    }

    #[test]
    fn imap_filename_and_or_attachment_error() {
        assert!(to_imap(&q("filename:pdf")).is_err());
        assert!(to_imap(&q("(has:attachment OR a)")).is_err());
    }

    // -- to_gmail -----------------------------------------------------------

    #[test]
    fn gmail_renders_sylvains_example_server_side() {
        let s = to_gmail(&q("from:boss@corp.com (invoice OR receipt) has:attachment"));
        assert_eq!(s, "from:boss@corp.com (invoice OR receipt) has:attachment");
    }

    #[test]
    fn gmail_quotes_spaces_and_converts_dates() {
        assert_eq!(
            to_gmail(&q("from:\"Ada Lovelace\"")),
            "from:\"Ada Lovelace\""
        );
        assert_eq!(to_gmail(&q("after:2026-01-01")), "after:2026/01/01");
        assert_eq!(to_gmail(&q("filename:pdf")), "filename:pdf");
    }

    #[test]
    fn gmail_search_command_is_wrapped_and_escaped() {
        assert_eq!(
            to_gmail_search_command(&q("has:attachment")).unwrap(),
            "X-GM-RAW \"has:attachment\""
        );
    }

    /// A CRLF in a value would end the `SEARCH` line and smuggle in a
    /// command of its own, so the render refuses it, and a NUL with it.
    #[test]
    fn imap_refuses_line_breaks_and_nul_in_values() {
        let with = |value: &str| Query {
            in_mailbox: None,
            message_id: None,
            clauses: vec![Clause::Single(Term::Subject(value.to_string()))],
        };
        assert!(to_imap(&with("x\r\nA1 DELETE Trash")).is_err());
        assert!(to_imap(&with("x\nY")).is_err());
        assert!(to_imap(&with("x\0y")).is_err());
        assert!(to_imap(&with("plain")).is_ok());
        assert!(to_gmail_search_command(&with("x\r\nA1 DELETE Trash")).is_err());
        assert!(to_gmail_search_command(&with("plain")).is_ok());
    }

    /// Repeated date terms all narrow, as `to_imap` sends them all: the
    /// latest `after` and the earliest `before` survive, whatever the order.
    #[test]
    fn fts_keeps_the_narrowest_of_repeated_date_terms() {
        let render = to_fts(&q("after:2026-03-01 after:2025-01-01")).unwrap();
        assert_eq!(render.after.as_deref(), Some("2026-03-01"));
        let render = to_fts(&q("before:2025-01-01 before:2026-03-01")).unwrap();
        assert_eq!(render.before.as_deref(), Some("2025-01-01"));
    }

    // -- to_graph -----------------------------------------------------------

    /// A double quote cannot close the quoted `$search` string early: a value
    /// that needs a phrase is wrapped in escaped quotes, an inner quote
    /// becomes a space, and a backslash is escaped.
    #[test]
    fn graph_search_values_escape_quotes_and_backslashes() {
        assert_eq!(kql_value(r#"say "hi" \ now"#), r#"\"say  hi  \\ now\""#);
        assert_eq!(kql_value("amazon"), "amazon");
        assert_eq!(kql_value("a+b@x.com"), "a+b@x.com");
        assert_eq!(kql_value("PERSO-144"), "PERSO-144");
        assert_eq!(kql_value("-draft"), r#"\"-draft\""#);
        assert_eq!(kql_value("OR"), r#"\"OR\""#);
        assert_eq!(kql_value("a:b"), r#"\"a:b\""#);
    }

    /// Every match term lands in one KQL `$search`, joined by an explicit
    /// `AND`: Graph refuses `$search` beside `$filter`, and KQL ORs two
    /// adjacent restrictions on one property.
    #[test]
    fn graph_renders_every_term_as_kql() {
        assert_eq!(
            to_graph(&q("from:alice subject:report has:attachment")).unwrap(),
            GraphQuery::Search("from:alice AND subject:report AND hasAttachments:true".into())
        );
        assert_eq!(
            to_graph(&q("to:bob cc:carol body:x filename:pdf")).unwrap(),
            GraphQuery::Search("to:bob AND cc:carol AND body:x AND attachment:pdf".into())
        );
        assert_eq!(
            to_graph(&q("from:amazon")).unwrap(),
            GraphQuery::Search("from:amazon".into())
        );
        assert_eq!(
            to_graph(&q("from:ovh mailypoppins after:2026-10-01")).unwrap(),
            GraphQuery::Search("from:ovh AND mailypoppins AND received>=2026-10-01".into())
        );
    }

    #[test]
    fn graph_phrases_are_quoted_inside_the_search_string() {
        assert_eq!(
            to_graph(&q(r#"subject:"quarterly report""#)).unwrap(),
            GraphQuery::Search(r#"subject:\"quarterly report\""#.into())
        );
    }

    #[test]
    fn graph_or_groups_may_mix_fields() {
        assert_eq!(
            to_graph(&q("(invoice OR receipt)")).unwrap(),
            GraphQuery::Search("(invoice OR receipt)".into())
        );
        assert_eq!(
            to_graph(&q("(from:a OR invoice)")).unwrap(),
            GraphQuery::Search("(from:a OR invoice)".into())
        );
    }

    #[test]
    fn graph_dates_are_an_and_ed_range() {
        assert_eq!(
            to_graph(&q("after:2026-01-01 before:2026-07-01")).unwrap(),
            GraphQuery::Search("received>=2026-01-01 AND received<2026-07-01".into())
        );
    }

    #[test]
    fn graph_message_id_is_a_filter_of_its_own() {
        assert_eq!(
            to_graph(&q("message-id:<a'b@x>")).unwrap(),
            GraphQuery::Filter("internetMessageId eq '<a''b@x>'".into())
        );
        let err = to_graph(&q("message-id:<a@x> from:bob")).unwrap_err();
        assert!(err.to_string().contains("message-id:"), "{err}");
    }

    #[test]
    fn graph_empty_query_lists_everything() {
        assert_eq!(to_graph(&Query::default()).unwrap(), GraphQuery::All);
        // `in:` is a scope, not a term.
        assert_eq!(to_graph(&q("in:sent")).unwrap(), GraphQuery::All);
    }

    // -- to_fts -------------------------------------------------------------

    #[test]
    fn fts_and_ed_quoted_terms() {
        let r = to_fts(&q("hello world")).unwrap();
        assert_eq!(r.match_expr.as_deref(), Some("\"hello\" \"world\""));
    }

    #[test]
    fn fts_columns_and_prefix() {
        assert_eq!(
            to_fts(&q("subject:invoice")).unwrap().match_expr.as_deref(),
            Some("subject:\"invoice\"")
        );
        assert_eq!(
            to_fts(&q("from:ada")).unwrap().match_expr.as_deref(),
            Some("from_:\"ada\"")
        );
        assert_eq!(
            to_fts(&q("invoi*")).unwrap().match_expr.as_deref(),
            Some("\"invoi\"*")
        );
    }

    #[test]
    fn fts_or_group() {
        assert_eq!(
            to_fts(&q("(invoice OR receipt)"))
                .unwrap()
                .match_expr
                .as_deref(),
            Some("(\"invoice\" OR \"receipt\")")
        );
    }

    #[test]
    fn fts_attachment_and_dates_are_predicates_not_match() {
        let r = to_fts(&q("has:attachment after:2026-01-01")).unwrap();
        assert!(r.match_expr.is_none());
        assert!(r.has_attachment);
        assert_eq!(r.after.as_deref(), Some("2026-01-01"));
    }

    #[test]
    fn fts_refuses_to_and_cc() {
        assert!(to_fts(&q("to:bob")).is_err());
        assert!(to_fts(&q("cc:x")).is_err());
    }

    #[test]
    fn fts_filename_is_an_attachment_name_predicate() {
        let r = to_fts(&q("filename:invoice.pdf")).unwrap();
        assert!(r.match_expr.is_none());
        assert_eq!(r.filenames, vec![vec!["invoice.pdf".to_string()]]);

        // A trailing star is dropped: the substring match covers the prefix.
        let r = to_fts(&q("filename:invoi* ledger")).unwrap();
        assert_eq!(r.match_expr.as_deref(), Some("\"ledger\""));
        assert_eq!(r.filenames, vec![vec!["invoi".to_string()]]);

        // Separate terms AND; an all-filename OR group is one predicate.
        let r = to_fts(&q("filename:pdf (filename:a OR filename:b)")).unwrap();
        assert_eq!(
            r.filenames,
            vec![
                vec!["pdf".to_string()],
                vec!["a".to_string(), "b".to_string()]
            ]
        );
    }

    #[test]
    fn fts_refuses_an_or_group_mixing_filename_with_other_terms() {
        let err = to_fts(&q("(filename:pdf OR invoice)")).unwrap_err();
        assert!(err.to_string().contains("filename:"), "{err}");
    }

    #[test]
    fn fts_sylvains_example_drops_attachment_into_a_predicate() {
        let r = to_fts(&q("from:boss@corp.com (invoice OR receipt) has:attachment")).unwrap();
        assert_eq!(
            r.match_expr.as_deref(),
            Some("from_:\"boss@corp.com\" (\"invoice\" OR \"receipt\")")
        );
        assert!(r.has_attachment);
    }

    // -- cross-renderer equivalence (closes the #0043 two-grammar debt) -----

    // -- CLI flags build the same AST as the positional grammar -------------

    #[test]
    fn cli_flags_equal_the_positional_grammar() {
        let flags = Flags {
            from: Some("boss@corp.com".into()),
            has_attachment: true,
            ..Default::default()
        };
        let built = from_cli("(invoice OR receipt)", &flags).unwrap();
        let positional = parse("from:boss@corp.com (invoice OR receipt) has:attachment").unwrap();
        assert_eq!(built, positional);
    }

    #[test]
    fn cli_date_flags_validate_and_order() {
        let flags = Flags {
            subject: Some("quarterly report".into()),
            after: Some("2026-01-01".into()),
            before: Some("2026-07-01".into()),
            ..Default::default()
        };
        let built = from_cli("", &flags).unwrap();
        let positional =
            parse("subject:\"quarterly report\" after:2026-01-01 before:2026-07-01").unwrap();
        assert_eq!(built, positional);
        assert!(from_cli(
            "",
            &Flags {
                after: Some("nope".into()),
                ..Default::default()
            }
        )
        .is_err());
    }

    #[test]
    fn one_input_renders_consistently_across_backends() {
        let query = q("from:boss@corp.com (invoice OR receipt) has:attachment");
        let imap = to_imap(&query).unwrap();
        let gmail = to_gmail(&query);
        let graph = to_graph(&query).unwrap();
        let fts = to_fts(&query).unwrap();

        // Every backend sees the sender, the OR group, and the attachment.
        assert!(imap.search.contains("FROM \"boss@corp.com\""));
        assert!(imap.search.contains("OR TEXT \"invoice\" TEXT \"receipt\""));
        assert!(imap.attachment_postfilter); // plain IMAP resolves it locally

        assert!(gmail.contains("from:boss@corp.com"));
        assert!(gmail.contains("(invoice OR receipt)"));
        assert!(gmail.contains("has:attachment")); // Gmail runs it server-side

        assert_eq!(
            graph,
            GraphQuery::Search(
                "from:boss@corp.com AND (invoice OR receipt) AND hasAttachments:true".into()
            )
        ); // Graph server-side, one KQL search

        assert!(fts
            .match_expr
            .unwrap()
            .contains("(\"invoice\" OR \"receipt\")"));
        assert!(fts.has_attachment); // local index column
    }
    // -- to_query_string: the inverse of the parser -------------------------

    /// Every shape the grammar can express survives a render/parse round trip,
    /// which is the property the TUI's local search pass rides on: the AST the
    /// overlay built and the AST `message.search` rebuilds from the rendered
    /// string are the same value.
    #[test]
    fn every_grammar_shape_round_trips_through_to_query_string() {
        for input in [
            "",
            "urgent",
            "urgent meeting",
            "\"quarterly report\"",
            "from:ada to:bob cc:x subject:s body:b text:t",
            "filename:invoice.pdf",
            "has:attachment",
            "before:2024-01-01 after:2023-12-01",
            "(invoice OR receipt)",
            "invoice OR receipt",
            "from:ada (invoice OR receipt) has:attachment",
            "in:Sent",
            "message-id:<abc@example.com>",
            "from:\"Ada Lovelace\" in:\"Sent Items\"",
            "re:budget",
        ] {
            let parsed = q(input);
            let rendered = to_query_string(&parsed);
            assert_eq!(
                parse(&rendered).expect("the render parses"),
                parsed,
                "{input:?} rendered as {rendered:?} and came back a different query"
            );
        }
    }

    /// A flag-built query round trips too, which is the other way the overlay
    /// builds one (`SearchForm::build_query` with no advanced line).
    #[test]
    fn a_flag_built_query_round_trips() {
        let flags = Flags {
            from: Some("ada@example.com".into()),
            subject: Some("quarterly report".into()),
            has_attachment: true,
            after: Some("2024-01-01".into()),
            ..Default::default()
        };
        let built = from_cli("invoice", &flags).expect("the flags build a query");
        assert_eq!(parse(&to_query_string(&built)).unwrap(), built);
    }

    /// The two shapes the grammar cannot carry back are dropped rather than
    /// rendered into something that parses as a different query: a field term
    /// with an empty value, which `parse` refuses outright, and a value
    /// carrying a `"`, which the tokenizer consumes as a delimiter. Neither is
    /// reachable from `parse`; both are constructible by hand.
    #[test]
    fn a_term_the_grammar_cannot_carry_is_dropped_not_mangled() {
        let query = Query {
            clauses: vec![
                Clause::Single(Term::From(String::new())),
                Clause::Single(Term::Subject("a\"b".into())),
                Clause::Single(Term::Text("kept".into())),
            ],
            in_mailbox: None,
            message_id: None,
        };
        assert_eq!(to_query_string(&query), "\"kept\"");
        assert_eq!(
            parse(&to_query_string(&query)).unwrap().clauses,
            vec![Clause::Single(Term::Text("kept".into()))]
        );
    }
}
