//! Full-text search over the store, end to end (#0043).
//!
//! Offline, against a store and a blob directory in a tempdir. What is asserted
//! here is the pair the ticket asks for: that a query returns the right rows in
//! a useful order, and that the index behind it never drifts from the rows it
//! describes -- across ingest, re-ingest of the same UID, a UIDVALIDITY rebind,
//! a delete and a prune.

use mailypoppins::ingest::{
    ingest_message, ingest_message_with_policy, prune_vanished, IngestInput, RebindPolicy,
};
use mailypoppins::parse::{AttachmentData, FetchedEmail};
use mailypoppins::store::search::{fts_expression, index_drift, search, SearchOrder};
use mailypoppins::store::{BlobStore, Store};
use tempfile::TempDir;

struct Fixture {
    _tmp: TempDir,
    store: Store,
    blobs: BlobStore,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(tmp.path().join("blobs"));
        Self {
            _tmp: tmp,
            store,
            blobs,
        }
    }

    fn ingest(&self, mailbox: &str, uid: i64, email: &FetchedEmail) -> i64 {
        ingest_message(
            &self.store,
            &self.blobs,
            &IngestInput {
                account: "acct",
                mailbox,
                uid,
                email,
                raw: None,
            },
        )
        .unwrap()
        .row_id
    }

    /// [`Fixture::ingest`] under the #0112 rebind gate: `listed` is what the
    /// server holds for this mailbox on this pass.
    fn ingest_listed(&self, mailbox: &str, uid: i64, email: &FetchedEmail, listed: &[i64]) -> i64 {
        let listed: std::collections::HashSet<i64> = listed.iter().copied().collect();
        ingest_message_with_policy(
            &self.store,
            &self.blobs,
            &IngestInput {
                account: "acct",
                mailbox,
                uid,
                email,
                raw: None,
            },
            &RebindPolicy::UnlessListed(&listed),
        )
        .unwrap()
        .row_id
    }

    fn hits(&self, query: &str) -> Vec<i64> {
        search(&self.store, "acct", query, None, 20, SearchOrder::Relevance)
            .unwrap()
            .into_iter()
            .map(|h| h.row.id)
            .collect()
    }

    fn hits_in(&self, query: &str, mailbox: &str) -> Vec<i64> {
        search(
            &self.store,
            "acct",
            query,
            Some(mailbox),
            20,
            SearchOrder::Relevance,
        )
        .unwrap()
        .into_iter()
        .map(|h| h.row.id)
        .collect()
    }
}

fn email(message_id: &str, from: &str, subject: &str, body: &str) -> FetchedEmail {
    FetchedEmail {
        from: from.into(),
        to: "me@example.com".into(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: subject.into(),
        date: "Thu, 7 Aug 2026 10:00:00 +0000".into(),
        body_text: body.into(),
        html_body: None,
        has_attachments: false,
        server_has_attachments: None,
        message_id: Some(format!("<{message_id}@example.com>")),
        attachments: Vec::new(),
        flags: Default::default(),
        calendar_ics: None,
        event: None,
    }
}

/// [`email`] carrying attachments under these names.
fn with_attachments(mut message: FetchedEmail, names: &[&str]) -> FetchedEmail {
    message.has_attachments = !names.is_empty();
    message.attachments = names
        .iter()
        .map(|name| AttachmentData {
            filename: (*name).to_string(),
            content: format!("bytes of {name}").into_bytes(),
            content_id: None,
        })
        .collect();
    message
}

/// `filename:` (PERSO-130) matches the attachment names ingest recorded, as a
/// case-insensitive substring, and narrows like any other term.
#[test]
fn filename_matches_a_stored_attachment_name() {
    let f = Fixture::new();
    let invoice = f.ingest(
        "inbox",
        1,
        &with_attachments(
            email("a", "a@example.com", "Billing", "see attached"),
            &["Invoice-2026_03.PDF", "logo.png"],
        ),
    );
    let notes = f.ingest(
        "archive",
        2,
        &with_attachments(
            email("b", "b@example.com", "Minutes", "see attached"),
            &["notes.txt"],
        ),
    );
    // A body that says the name is not an attachment that carries it.
    f.ingest(
        "inbox",
        3,
        &email("c", "c@example.com", "Plain", "invoice.pdf is coming"),
    );

    assert_eq!(f.hits("filename:invoice-2026_03.pdf"), vec![invoice]);
    assert_eq!(
        f.hits("filename:pdf"),
        vec![invoice],
        "an extension matches"
    );
    assert_eq!(f.hits("filename:INVOI*"), vec![invoice], "a prefix matches");
    assert_eq!(f.hits("filename:notes"), vec![notes]);
    assert!(f.hits("filename:budget").is_empty());

    // AND with indexed terms and the mailbox scope.
    assert_eq!(f.hits("filename:pdf billing"), vec![invoice]);
    assert!(f.hits("filename:pdf minutes").is_empty());
    assert!(f.hits_in("filename:notes", "inbox").is_empty());

    // Separate terms AND; an all-filename OR group ORs.
    assert_eq!(f.hits("filename:pdf filename:logo"), vec![invoice]);
    assert!(f.hits("filename:pdf filename:notes").is_empty());
    let mut either = f.hits("(filename:logo OR filename:notes)");
    either.sort_unstable();
    assert_eq!(either, vec![invoice, notes].tap_sorted());
}

/// `%` and `_` in a `filename:` value are literal characters, not `LIKE`
/// wildcards: `filename:a_b` must not match `axb`.
#[test]
fn filename_wildcard_characters_match_literally() {
    let f = Fixture::new();
    let underscored = f.ingest(
        "inbox",
        1,
        &with_attachments(email("a", "a@example.com", "One", "x"), &["a_b 100%.txt"]),
    );
    f.ingest(
        "inbox",
        2,
        &with_attachments(email("b", "b@example.com", "Two", "x"), &["axb 1000.txt"]),
    );
    assert_eq!(f.hits("filename:a_b"), vec![underscored]);
    assert_eq!(f.hits("filename:\"100%\""), vec![underscored]);
}

/// `filename:` folds case beyond ASCII (PERSO-134): an umlaut matches across
/// case both ways, and `ẞ` folds to `ß`. `ß` against `SS` is not folded, so
/// `filename:STRASSE` misses `straße.pdf`; the test pins that as the known
/// limit of a per-character fold rather than leaving it unstated.
#[test]
fn filename_folds_case_beyond_ascii() {
    let f = Fixture::new();
    let overview = f.ingest(
        "inbox",
        1,
        &with_attachments(email("a", "a@example.com", "One", "x"), &["Übersicht.pdf"]),
    );
    let street = f.ingest(
        "inbox",
        2,
        &with_attachments(email("b", "b@example.com", "Two", "x"), &["straße.pdf"]),
    );
    let greek = f.ingest(
        "inbox",
        3,
        &with_attachments(email("c", "c@example.com", "Three", "x"), &["ΟΔΟΣ.pdf"]),
    );

    assert_eq!(f.hits("filename:übersicht"), vec![overview]);
    assert_eq!(f.hits("filename:ÜBERSICHT"), vec![overview]);
    assert_eq!(f.hits("filename:STRAßE"), vec![street]);
    assert_eq!(f.hits("filename:STRAẞE"), vec![street], "capital sharp s");
    assert!(
        f.hits("filename:STRASSE").is_empty(),
        "ß and SS are not folded together"
    );
    assert_eq!(f.hits("filename:οδος"), vec![greek], "final sigma");
}

/// The iMIP sidecar ingest stores as an attachment blob is not a user-facing
/// attachment, so `filename:` does not see its name.
#[test]
fn filename_does_not_match_the_invite_sidecar() {
    let f = Fixture::new();
    let mut invite = email("a", "a@example.com", "Kickoff", "join us");
    invite.calendar_ics = Some(b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".to_vec());
    f.ingest("inbox", 1, &invite);
    assert!(f.hits("filename:invite").is_empty());
    assert!(f.hits("filename:ics").is_empty());
}

/// An OR group mixing a name with an indexed term cannot be split into the
/// FTS `MATCH` and the SQL predicate, so it is refused rather than narrowed.
#[test]
fn an_or_group_mixing_filename_and_text_is_refused() {
    let f = Fixture::new();
    let err = search(
        &f.store,
        "acct",
        "(filename:pdf OR invoice)",
        None,
        20,
        SearchOrder::Relevance,
    )
    .unwrap_err();
    assert!(err.to_string().contains("filename:"), "{err}");
}

/// The base assertion of the whole feature: a word in a body that was never
/// written to a file is found by a `SELECT`.
#[test]
fn a_word_in_the_body_is_found() {
    let f = Fixture::new();
    let row = f.ingest(
        "inbox",
        1,
        &email(
            "a",
            "Ada <ada@example.com>",
            "Hello",
            "the quarterly ledger is attached",
        ),
    );
    f.ingest(
        "inbox",
        2,
        &email("b", "Bob <bob@example.com>", "Lunch", "pizza?"),
    );
    assert_eq!(f.hits("ledger"), vec![row]);
    assert!(f.hits("aardvark").is_empty());
}

/// Subject and sender are indexed columns too, and the field filters address
/// them individually.
#[test]
fn subject_and_sender_are_searchable_and_addressable() {
    let f = Fixture::new();
    let row = f.ingest(
        "inbox",
        1,
        &email(
            "a",
            "Ada Lovelace <ada@example.com>",
            "Quarterly report",
            "nothing here",
        ),
    );
    // A decoy whose *body* says the word the subject filter must not match.
    let decoy = f.ingest(
        "inbox",
        2,
        &email(
            "b",
            "Bob <bob@example.com>",
            "Lunch",
            "quarterly pizza budget",
        ),
    );

    assert_eq!(f.hits("subject:quarterly"), vec![row]);
    assert_eq!(f.hits("body:quarterly"), vec![decoy]);
    assert_eq!(f.hits("from:lovelace"), vec![row]);
    // Unfiltered, both match; ranking is asserted separately.
    assert_eq!(f.hits("quarterly").len(), 2);
}

/// Terms are AND-ed, which is what makes a second word narrow a search rather
/// than widen it.
#[test]
fn several_terms_all_have_to_match() {
    let f = Fixture::new();
    let both = f.ingest(
        "inbox",
        1,
        &email("a", "a@example.com", "Trip", "berlin in october"),
    );
    f.ingest(
        "inbox",
        2,
        &email("b", "b@example.com", "Trip", "berlin in march"),
    );
    assert_eq!(f.hits("berlin october"), vec![both]);
}

/// A quoted phrase matches adjacency, not the same words anywhere.
#[test]
fn a_phrase_matches_adjacent_words_only() {
    let f = Fixture::new();
    let phrase = f.ingest(
        "inbox",
        1,
        &email(
            "a",
            "a@example.com",
            "One",
            "please sign the lease agreement today",
        ),
    );
    f.ingest(
        "inbox",
        2,
        &email("b", "b@example.com", "Two", "the agreement is not a lease"),
    );
    assert_eq!(f.hits("\"lease agreement\""), vec![phrase]);
    // Unquoted, both documents carry both words.
    assert_eq!(f.hits("lease agreement").len(), 2);
}

/// A trailing `*` still reads as a prefix, which the substring match a term
/// of three characters or more makes already covers (PERSO-276).
#[test]
fn a_trailing_star_matches_a_prefix() {
    let f = Fixture::new();
    let row = f.ingest(
        "inbox",
        1,
        &email("a", "a@example.com", "Bill", "the invoicing run finished"),
    );
    assert_eq!(f.hits("invoic*"), vec![row]);
    assert_eq!(f.hits("invoic"), vec![row]);
    assert_eq!(f.hits("in*"), vec![row]);
}

/// The substring match of PERSO-276: a term of three characters or more
/// matches any part of a word in the subject, the sender or the body, whatever
/// its case or the case of the text.
#[test]
fn a_term_matches_any_part_of_a_word_whatever_the_case() {
    let f = Fixture::new();
    let roof = f.ingest(
        "inbox",
        1,
        &email(
            "a",
            "Hausverwaltung Mueller <info@hv-mueller.de>",
            "Angebot Dachsanierung",
            "Anbei das Angebot fuer die Fassadenreinigung.",
        ),
    );
    f.ingest(
        "inbox",
        2,
        &email("b", "Bob <bob@example.com>", "Lunch", "pizza at noon"),
    );

    // The subject, from the middle and the end of a word, in any case.
    assert_eq!(f.hits("sanierung"), vec![roof]);
    assert_eq!(f.hits("DACH"), vec![roof]);
    assert_eq!(f.hits("chSaNi"), vec![roof]);
    assert_eq!(f.hits("subject:SANIERUNG"), vec![roof]);
    // The sender's name and address.
    assert_eq!(f.hits("verwaltung"), vec![roof]);
    assert_eq!(f.hits("from:MUELLER.DE"), vec![roof]);
    assert!(f.hits("subject:verwaltung").is_empty(), "the column holds");
    // The body.
    assert_eq!(f.hits("REINIGUNG"), vec![roof]);
    assert_eq!(f.hits("body:ssadenr"), vec![roof]);
    assert!(f.hits("body:sanierung").is_empty(), "the column holds");
    // Accents fold as they did in the word index.
    assert_eq!(f.hits("fassadenréinigung"), vec![roof]);
    assert!(f.hits("sanierungen").is_empty());
}

/// A term with punctuation inside still finds what the word index found for
/// it, the words in order across any punctuation or space, beside the literal
/// substring: a typographic apostrophe, as Outlook and Apple Mail write it,
/// and a hyphen typed as a space (PERSO-276).
#[test]
fn a_punctuated_term_finds_what_the_word_index_found() {
    let f = Fixture::new();
    let straight = f.ingest(
        "inbox",
        1,
        &email("a", "a@example.com", "Note", "please don't reply"),
    );
    let curly = f.ingest(
        "inbox",
        2,
        &email("b", "b@example.com", "Note", "please don\u{2019}t reply"),
    );
    let hyphen = f.ingest(
        "inbox",
        3,
        &email("c", "c@example.com", "Re: follow-up", "see below"),
    );
    let spaced = f.ingest(
        "inbox",
        4,
        &email("d", "d@example.com", "A follow up", "see below"),
    );
    let obrien = f.ingest(
        "inbox",
        5,
        &email("e", "Pat O'Brien <pat@example.com>", "Hello", "hi"),
    );
    let obrien_curly = f.ingest(
        "inbox",
        6,
        &email("f", "a@example.com", "Call O\u{2019}Brien", "hi"),
    );

    assert_eq!(f.hits("don't").tap_sorted(), vec![straight, curly]);
    assert_eq!(f.hits("follow-up").tap_sorted(), vec![hyphen, spaced]);
    assert_eq!(f.hits("O'Brien").tap_sorted(), vec![obrien, obrien_curly]);
    // The substring side still widens it.
    assert_eq!(f.hits("llow-u"), vec![hyphen]);
    assert_eq!(f.hits("brie").tap_sorted(), vec![obrien, obrien_curly]);
}

/// A term of one or two characters has no trigram, so it matches the
/// beginning of a word through the word index rather than nothing.
#[test]
fn a_short_term_matches_the_beginning_of_a_word() {
    let f = Fixture::new();
    let roof = f.ingest(
        "inbox",
        1,
        &email("a", "a@example.com", "Dachsanierung", "Termin am Montag"),
    );
    assert_eq!(f.hits("da"), vec![roof]);
    assert_eq!(f.hits("DA"), vec![roof]);
    assert_eq!(f.hits("subject:d"), vec![roof]);
    assert!(
        f.hits("ch").is_empty(),
        "two letters match a word's beginning, not its middle"
    );
    assert!(f.hits("body:da").is_empty(), "the column holds");
}

/// A query mixing short and long terms runs as one statement over both
/// indexes, AND-ed, with OR groups inside one index or split across both.
#[test]
fn a_query_mixes_substring_and_word_beginning_terms() {
    let f = Fixture::new();
    let roof = f.ingest(
        "inbox",
        1,
        &dated(
            email("a", "a@example.com", "Dachsanierung", "Termin am Montag"),
            "Mon, 5 Jan 2026 09:00:00 +0000",
        ),
    );
    let facade = f.ingest(
        "inbox",
        2,
        &dated(
            email("b", "b@example.com", "Fassade", "Termin am Dienstag"),
            "Tue, 6 Jan 2026 09:00:00 +0000",
        ),
    );

    assert_eq!(f.hits("am sanierung"), vec![roof]);
    assert_eq!(f.hits("fa termin"), vec![facade]);
    assert!(f.hits("fa sanierung").is_empty(), "the terms are AND-ed");
    // An OR group split across both indexes.
    let mut either = f.hits("(fa OR sanierung) termin");
    either.sort_unstable();
    assert_eq!(either, vec![roof, facade].tap_sorted());
    assert_eq!(f.hits("(fa OR sanierung) montag"), vec![roof]);
    // A phrase of several words stays a phrase of whole words.
    assert_eq!(f.hits("\"am montag\" dach"), vec![roof]);
    // The SQL predicates and the orders still apply.
    assert_eq!(f.hits("termin after:2026-01-06"), vec![facade]);
    let newest: Vec<i64> = search(&f.store, "acct", "min am", None, 20, SearchOrder::Newest)
        .unwrap()
        .into_iter()
        .map(|h| h.row.id)
        .collect();
    assert_eq!(newest, vec![facade, roof]);
}

/// A pasted paragraph is a query too: hundreds of substring terms beside a
/// short word and a phrase still run, past SQLite's 64-table cap on a join
/// and its 500-term cap on a compound `SELECT`, and every term still has to
/// match (PERSO-276).
#[test]
fn a_pasted_paragraph_of_many_terms_is_answered() {
    let f = Fixture::new();
    // A fresh store has nothing to backfill, so the query takes the
    // substring path, the one that unions an arm per index and term and
    // groups them by row.
    assert!(mailypoppins::store::fts::trigram_ready(f.store.conn()).unwrap());
    let words: Vec<String> = (0..300).map(|i| format!("term{i:03}")).collect();
    let body = format!("{} we meet at noon", words.join(" "));
    let all = f.ingest("inbox", 1, &email("a", "a@example.com", "Notes", &body));
    let short_of_one = f.ingest(
        "inbox",
        2,
        &email(
            "b",
            "b@example.com",
            "Notes",
            &format!("{} we meet at noon", words[..299].join(" ")),
        ),
    );

    let first_eighty = format!("{} at \"meet at noon\"", words[..80].join(" "));
    assert_eq!(f.hits(&first_eighty).tap_sorted(), vec![all, short_of_one]);
    let query = format!("{} at \"meet at noon\"", words.join(" "));
    assert_eq!(f.hits(&query), vec![all]);
}

/// A word typed again, in any ASCII case, is the same AND clause: the query
/// finds and ranks what the query without the repeats does, and a repeat
/// costs no work and no bound parameter (PERSO-276).
#[test]
fn a_repeated_word_finds_and_ranks_as_once() {
    let f = Fixture::new();
    f.ingest(
        "inbox",
        1,
        &email(
            "a",
            "a@example.com",
            "Rechnung Dach",
            "die Rechnung am Montag",
        ),
    );
    f.ingest(
        "inbox",
        2,
        &email("b", "b@example.com", "Termin", "Dachrechnung am Dienstag"),
    );
    f.ingest(
        "inbox",
        3,
        &email("c", "c@example.com", "Lunch", "am Mittag"),
    );
    let ranked = |query: &str| -> Vec<(i64, f64)> {
        search(&f.store, "acct", query, None, 20, SearchOrder::Relevance)
            .unwrap()
            .into_iter()
            .map(|h| (h.row.id, h.rank))
            .collect()
    };
    let once = ranked("rechnung dach am");
    assert_eq!(once.len(), 2);
    // Past the parameter limit a query of distinct words would hit.
    let repeated = ["Rechnung", "dach", "am", "RECHNUNG", "Dach", "AM"]
        .repeat(3_000)
        .join(" ");
    assert_eq!(ranked(&repeated), once);
}

/// A query of more distinct words than SQLite binds parameters for is refused
/// in plain words, not with the generated SQL (PERSO-276).
#[test]
fn too_many_distinct_words_are_refused_in_plain_words() {
    let f = Fixture::new();
    f.ingest("inbox", 1, &email("a", "a@example.com", "Notes", "w00001"));
    let query = (0..17_000)
        .map(|i| format!("w{i:05}"))
        .collect::<Vec<_>>()
        .join(" ");
    let err = search(&f.store, "acct", &query, None, 20, SearchOrder::Relevance)
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("the search has too many distinct words (17000); the limit is "),
        "{err}"
    );
    assert!(!err.contains("SELECT") && !err.contains("WITH"), "{err}");
    assert!(err.len() < 100, "{err}");
}

/// Non-ASCII text survives the round trip through the tokenizer, and so does a
/// query typed with the same accents.
#[test]
fn unicode_terms_round_trip() {
    let f = Fixture::new();
    let row = f.ingest(
        "inbox",
        1,
        &email(
            "a",
            "Émile <emile@example.com>",
            "Réunion d'équipe",
            "on se réunit à Zürich",
        ),
    );
    assert_eq!(f.hits("Zürich"), vec![row]);
    assert_eq!(f.hits("réunit"), vec![row]);
    assert_eq!(f.hits("subject:équipe"), vec![row]);
    // Case folding is the tokenizer's, so an uppercase query is the same query.
    assert_eq!(f.hits("ZÜRICH"), vec![row]);
}

/// Punctuation a user types is text, not FTS5 syntax. Every one of these is a
/// syntax error when handed to `MATCH` unquoted.
#[test]
fn punctuation_in_a_query_is_not_a_syntax_error() {
    let f = Fixture::new();
    f.ingest(
        "inbox",
        1,
        &email("a", "a@example.com", "Build", "we ship c++ and rust"),
    );
    for query in ["c++", "(c++)", "rust -", "ship AND", "ship\" OR rust"] {
        assert!(
            search(&f.store, "acct", query, None, 20, SearchOrder::Relevance).is_ok(),
            "query {query:?} must not fail"
        );
    }
    // A query with nothing indexable in it is refused rather than run.
    assert!(search(&f.store, "acct", "  ??  ", None, 20, SearchOrder::Relevance).is_err());
    assert!(search(&f.store, "acct", "\"", None, 20, SearchOrder::Relevance).is_err());
    assert!(fts_expression("??").is_err());
}

/// Rank puts the subject hit above the body hit, which is the whole reason the
/// bm25 weights are not uniform.
#[test]
fn a_subject_hit_outranks_a_body_hit() {
    let f = Fixture::new();
    let buried = f.ingest(
        "inbox",
        1,
        &email(
            "a",
            "a@example.com",
            "Lunch",
            "somewhere in the thread: invoice",
        ),
    );
    let titled = f.ingest(
        "inbox",
        2,
        &email("b", "b@example.com", "Invoice 42", "attached"),
    );
    assert_eq!(f.hits("invoice"), vec![titled, buried]);
}

/// [`email`] sent on this `Date:`.
fn dated(mut message: FetchedEmail, date: &str) -> FetchedEmail {
    message.date = date.into();
    message
}

/// The three orders (PERSO-273): newest and oldest by `Date:` whatever the
/// rank, a dateless row last in both, relevance by bm25 and then newest
/// first; and the limit keeps the first rows of the order, not of the rank.
#[test]
fn hits_come_back_newest_oldest_or_best_first() {
    let f = Fixture::new();
    // A subject hit in the middle of the dates, and body hits around it.
    let old = f.ingest(
        "inbox",
        1,
        &dated(
            email("old", "a@example.com", "Lunch", "the invoice"),
            "Mon, 5 Jan 2026 09:00:00 +0000",
        ),
    );
    let titled = f.ingest(
        "archive",
        2,
        &dated(
            email("mid", "b@example.com", "Invoice 42", "attached"),
            "Tue, 3 Mar 2026 09:00:00 +0000",
        ),
    );
    let new = f.ingest(
        "inbox",
        3,
        &dated(
            email("new", "c@example.com", "Lunch again", "one more invoice"),
            "Wed, 6 May 2026 09:00:00 +0000",
        ),
    );
    let dateless = f.ingest(
        "inbox",
        4,
        &dated(
            email("nodate", "d@example.com", "Lunch", "an invoice"),
            "not a date",
        ),
    );
    let ordered = |order: SearchOrder, limit: usize| -> Vec<i64> {
        search(&f.store, "acct", "invoice", None, limit, order)
            .unwrap()
            .into_iter()
            .map(|h| h.row.id)
            .collect()
    };

    assert_eq!(
        ordered(SearchOrder::Newest, 20),
        vec![new, titled, old, dateless]
    );
    assert_eq!(
        ordered(SearchOrder::Oldest, 20),
        vec![old, titled, new, dateless]
    );
    let best = ordered(SearchOrder::Relevance, 20);
    assert_eq!(best[0], titled, "the subject hit ranks first: {best:?}");
    assert_eq!(best.len(), 4);

    // The limit cuts the order the SQL ran, so it keeps the right rows.
    assert_eq!(ordered(SearchOrder::Newest, 1), vec![new]);
    assert_eq!(ordered(SearchOrder::Oldest, 1), vec![old]);
    assert_eq!(ordered(SearchOrder::Relevance, 1), vec![titled]);

    // A query with no text term has no rank: relevance is newest first.
    let undated = |order: SearchOrder| -> Vec<i64> {
        search(&f.store, "acct", "after:2026-01-01", None, 20, order)
            .unwrap()
            .into_iter()
            .map(|h| h.row.id)
            .collect()
    };
    assert_eq!(undated(SearchOrder::Relevance), vec![new, titled, old]);
    assert_eq!(undated(SearchOrder::Oldest), vec![old, titled, new]);
}

/// `date_sort` is a Unix timestamp, so a `Date:` before 1970 is negative: it
/// is the oldest of the dated rows, never lumped with the undated one (0),
/// which stays last in every order (PERSO-273).
#[test]
fn a_date_before_1970_is_a_date_not_an_undated_row() {
    let f = Fixture::new();
    let modern = f.ingest(
        "inbox",
        1,
        &dated(
            email("modern", "a@example.com", "Lunch", "the invoice"),
            "Mon, 5 Jan 2026 09:00:00 +0000",
        ),
    );
    // The epoch east of UTC: -3600.
    let epoch_east = f.ingest(
        "inbox",
        2,
        &dated(
            email("epoch", "b@example.com", "Lunch", "an invoice"),
            "Thu, 1 Jan 1970 00:00:00 +0100",
        ),
    );
    let dateless = f.ingest(
        "inbox",
        3,
        &dated(
            email("nodate", "c@example.com", "Lunch", "one invoice"),
            "not a date",
        ),
    );
    let date_sort: i64 = f
        .store
        .conn()
        .query_row(
            "SELECT date_sort FROM messages WHERE id = ?1",
            [epoch_east],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(date_sort, -3600, "the fixture stores a negative date_sort");

    let ordered = |query: &str, order: SearchOrder| -> Vec<i64> {
        search(&f.store, "acct", query, None, 20, order)
            .unwrap()
            .into_iter()
            .map(|h| h.row.id)
            .collect()
    };
    assert_eq!(
        ordered("invoice", SearchOrder::Newest),
        vec![modern, epoch_east, dateless]
    );
    assert_eq!(
        ordered("invoice", SearchOrder::Oldest),
        vec![epoch_east, modern, dateless]
    );
    // Without a text term every rank ties, so relevance is newest first.
    assert_eq!(
        ordered("before:2030-01-01", SearchOrder::Relevance),
        vec![modern, epoch_east, dateless]
    );
}

/// The search covers every mailbox at once, and `--mailbox` narrows it.
#[test]
fn search_spans_mailboxes_and_can_be_scoped_to_one() {
    let f = Fixture::new();
    let inbox = f.ingest(
        "inbox",
        1,
        &email("a", "a@example.com", "Contract", "signed"),
    );
    let archive = f.ingest(
        "archive",
        1,
        &email("b", "b@example.com", "Contract", "signed"),
    );
    let mut all = f.hits("contract");
    all.sort_unstable();
    assert_eq!(all, vec![inbox, archive].tap_sorted());
    assert_eq!(f.hits_in("contract", "archive"), vec![archive]);
    assert_eq!(f.hits_in("contract", "inbox"), vec![inbox]);
}

trait TapSorted {
    fn tap_sorted(self) -> Vec<i64>;
}
impl TapSorted for Vec<i64> {
    fn tap_sorted(mut self) -> Vec<i64> {
        self.sort_unstable();
        self
    }
}

/// Another account's rows are invisible, even though one file holds one
/// account today: the query is account-scoped like every other store query.
#[test]
fn hits_are_scoped_to_the_account() {
    let f = Fixture::new();
    let mine = f.ingest("inbox", 1, &email("a", "a@example.com", "Ledger", "mine"));
    ingest_message(
        &f.store,
        &f.blobs,
        &IngestInput {
            account: "other",
            mailbox: "inbox",
            uid: 1,
            email: &email("b", "b@example.com", "Ledger", "theirs"),
            raw: None,
        },
    )
    .unwrap();
    assert_eq!(f.hits("ledger"), vec![mine]);
}

/// Re-ingesting the same UID replaces the indexed text instead of adding a
/// second entry for the row (the #0037 double-index bug, guarded from the
/// query side this time).
#[test]
fn reingesting_a_uid_replaces_what_is_indexed() {
    let f = Fixture::new();
    let row = f.ingest(
        "inbox",
        1,
        &email("a", "a@example.com", "First", "original wording"),
    );
    assert_eq!(f.hits("original"), vec![row]);

    let same = f.ingest(
        "inbox",
        1,
        &email("a", "a@example.com", "Second", "corrected wording"),
    );
    assert_eq!(same, row, "the UPSERT must keep the row id");
    assert!(f.hits("original").is_empty(), "stale text still matches");
    assert_eq!(f.hits("corrected"), vec![row]);
    assert_eq!(f.hits("wording"), vec![row], "one entry, not two");
    assert_eq!(index_drift(&f.store).unwrap(), (0, 0));
}

/// A UIDVALIDITY reset rebinds the row to a new UID in place; the index goes
/// with it and does not fork into two entries.
#[test]
fn a_uidvalidity_rebind_keeps_one_indexed_entry() {
    let f = Fixture::new();
    let row = f.ingest(
        "inbox",
        7,
        &email("a", "a@example.com", "Renumbered", "same content"),
    );
    let rebound = f.ingest(
        "inbox",
        9001,
        &email("a", "a@example.com", "Renumbered", "same content"),
    );
    assert_eq!(rebound, row);
    assert_eq!(f.hits("renumbered"), vec![row]);
    assert_eq!(index_drift(&f.store).unwrap(), (0, 0));
}

/// The insert branch's twin (#0112): a second server-side copy of one message
/// is a second row, so it is a second entry in the index and both are found.
/// The rebind test above pins the other branch, and the pair is what says the
/// index follows the rows rather than the `Message-ID`.
#[test]
fn a_second_copy_of_one_message_is_indexed_as_its_own_entry() {
    let f = Fixture::new();
    let copy = || email("a", "a@example.com", "Duplicated", "same content");
    let first = f.ingest_listed("sent", 6540, &copy(), &[6540, 6542]);
    let second = f.ingest_listed("sent", 6542, &copy(), &[6540, 6542]);

    assert_ne!(second, first, "a listed copy owes a row of its own");
    let mut hits = f.hits("duplicated");
    hits.sort_unstable();
    assert_eq!(hits, vec![first, second], "and an index entry of its own");
    assert_eq!(index_drift(&f.store).unwrap(), (0, 0));
}

/// A pruned message stops being findable, in the same transaction that removed
/// its row.
#[test]
fn a_pruned_message_leaves_the_index() {
    let f = Fixture::new();
    let stays = f.ingest(
        "inbox",
        1,
        &email("a", "a@example.com", "Keep", "shared word"),
    );
    let goes = f.ingest(
        "inbox",
        2,
        &email("b", "b@example.com", "Drop", "shared word"),
    );
    assert_eq!(f.hits("shared").len(), 2);

    let pruned = prune_vanished(&f.store, &f.blobs, "acct", "inbox", &[2i64]);
    assert_eq!(pruned, 1);
    assert_eq!(f.hits("shared"), vec![stays]);
    assert!(f.hits("drop").is_empty());
    assert_ne!(stays, goes);
    assert_eq!(index_drift(&f.store).unwrap(), (0, 0));
}

/// The same for an explicit delete (`mp delete`, the TUI's `d`).
#[test]
fn a_deleted_row_leaves_the_index() {
    let f = Fixture::new();
    let row = f.ingest(
        "inbox",
        1,
        &email("a", "a@example.com", "Doomed", "delete me"),
    );
    mailypoppins::store::write::delete_row(&f.store, &f.blobs, row).unwrap();
    assert!(f.hits("doomed").is_empty());
    assert_eq!(index_drift(&f.store).unwrap(), (0, 0));
}

/// A move changes the mailbox of a row without touching its text, so the hit
/// follows the message to its new scope rather than disappearing.
#[test]
fn a_moved_row_is_found_under_its_new_mailbox() {
    let f = Fixture::new();
    let row = f.ingest(
        "inbox",
        1,
        &email("a", "a@example.com", "Filed", "keep this one"),
    );
    mailypoppins::store::write::move_row(&f.store, row, "archive").unwrap();
    assert!(f.hits_in("filed", "inbox").is_empty());
    assert_eq!(f.hits_in("filed", "archive"), vec![row]);
    assert_eq!(index_drift(&f.store).unwrap(), (0, 0));
}

/// A store an older build wrote has no trigram index: its next open creates
/// it empty, the search keeps matching whole words until the backfill has
/// indexed every row, and then matches substrings (PERSO-276). Rows written,
/// rewritten or deleted while it runs keep both indexes in step.
#[test]
fn the_backfill_fills_an_older_store_and_the_search_switches_over() {
    use mailypoppins::store::fts::{
        backfill_trigram_bounded, trigram_ready, META_TRIGRAM_COMPLETE, TRIGRAM_TABLE,
    };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("store.sqlite3");
    let blobs = BlobStore::new(tmp.path().join("blobs"));
    let ingest = |store: &Store, uid: i64, subject: &str, body: &str| {
        ingest_message(
            store,
            &blobs,
            &IngestInput {
                account: "acct",
                mailbox: "inbox",
                uid,
                email: &email(&format!("m{uid}"), "a@example.com", subject, body),
                raw: None,
            },
        )
        .unwrap()
        .row_id
    };
    let hits = |store: &Store, query: &str| -> Vec<i64> {
        let mut ids: Vec<i64> = search(store, "acct", query, None, 50, SearchOrder::Newest)
            .unwrap()
            .into_iter()
            .map(|h| h.row.id)
            .collect();
        ids.sort_unstable();
        ids
    };

    let mut rows = Vec::new();
    {
        let store = Store::open(&path).unwrap();
        for uid in 1..=7 {
            rows.push(ingest(
                &store,
                uid,
                &format!("Dachsanierung {uid}"),
                "Angebot",
            ));
        }
        // What an older build left: no trigram table, no completion stamp.
        store
            .conn()
            .execute_batch(&format!(
                "DROP TABLE {TRIGRAM_TABLE}; DELETE FROM meta WHERE key = '{META_TRIGRAM_COMPLETE}';"
            ))
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    assert!(!trigram_ready(store.conn()).unwrap());
    assert!(
        hits(&store, "sanierung").is_empty(),
        "whole words until complete"
    );
    assert_eq!(hits(&store, "dachsanierung"), rows);
    assert_eq!(hits(&store, "dachsanierung 3"), vec![rows[2]]);
    assert_eq!(
        index_drift(&store).unwrap(),
        (0, 0),
        "an unfilled index owes rows by design"
    );

    // Two rows per batch: four batches, the last one stamping completion.
    let first = backfill_trigram_bounded(&store, &blobs, 2, usize::MAX).unwrap();
    assert_eq!((first.indexed, first.complete), (2, false));
    // Writes during the backfill index both tables themselves.
    let late = ingest(&store, 8, "Dachsanierung spaet", "Angebot");
    ingest(
        &store,
        4,
        "Fassadenreinigung",
        "rewritten before the backfill reached it",
    );
    mailypoppins::store::write::delete_row(&store, &blobs, rows[5]).unwrap();
    let mut batches = 1;
    loop {
        let batch = backfill_trigram_bounded(&store, &blobs, 2, usize::MAX).unwrap();
        batches += 1;
        if batch.complete {
            break;
        }
        assert!(batches < 10, "the backfill never completed");
    }
    assert!(trigram_ready(store.conn()).unwrap());
    assert_eq!(index_drift(&store).unwrap(), (0, 0));

    let mut expected = vec![rows[0], rows[1], rows[2], rows[4], rows[6], late];
    expected.sort_unstable();
    assert_eq!(hits(&store, "sanierung"), expected);
    assert_eq!(hits(&store, "REINIGUNG"), vec![rows[3]]);
    // A completed store answers at once and indexes nothing more.
    let done = backfill_trigram_bounded(&store, &blobs, 2, usize::MAX).unwrap();
    assert_eq!((done.indexed, done.complete), (0, true));
}

/// A body the retention sweep evicted before the backfill reached its row
/// leaves the row in the trigram index by subject and sender only, and the
/// word index's entry, which the sweep keeps, still finds it by a whole body
/// word once the search answers from both (PERSO-276).
#[test]
fn an_evicted_body_is_still_found_by_a_whole_word_after_the_backfill() {
    use mailypoppins::config::RetentionPolicy;
    use mailypoppins::store::fts::{
        backfill_trigram_bounded, trigram_ready, META_TRIGRAM_COMPLETE, TRIGRAM_TABLE,
    };
    use mailypoppins::store::sweep::{sweep, SweepDecision, SweepOptions};
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("store.sqlite3");
    let blobs = BlobStore::new(tmp.path().join("blobs"));
    let row = {
        let store = Store::open(&path).unwrap();
        let row = ingest_message(
            &store,
            &blobs,
            &IngestInput {
                account: "acct",
                mailbox: "inbox",
                uid: 1,
                email: &email(
                    "a",
                    "a@example.com",
                    "Angebot",
                    "Die Dachsanierung beginnt im Mai.",
                ),
                raw: None,
            },
        )
        .unwrap()
        .row_id;
        // What an older build left: no trigram table, no completion stamp.
        store
            .conn()
            .execute_batch(&format!(
                "DROP TABLE {TRIGRAM_TABLE}; DELETE FROM meta WHERE key = '{META_TRIGRAM_COMPLETE}';"
            ))
            .unwrap();
        row
    };

    let store = Store::open(&path).unwrap();
    // The retention sweep over a one-byte cap: the first breach warns, the
    // second evicts the body blob and keeps the row and its word-index entry.
    let policy = RetentionPolicy {
        metadata_horizon_days: 0,
        body_horizon_days: 0,
        attachment_horizon_days: 0,
        max_disk_bytes: 1,
    };
    let force = SweepOptions {
        dry_run: false,
        force: true,
    };
    sweep(&store, &blobs, &policy, force).unwrap();
    let evicted = sweep(&store, &blobs, &policy, force).unwrap();
    assert_eq!(evicted.decision, SweepDecision::Evicted);
    assert!(!evicted.evicted.is_empty(), "the body blob was evicted");

    while !backfill_trigram_bounded(&store, &blobs, 10, usize::MAX)
        .unwrap()
        .complete
    {}
    assert!(trigram_ready(store.conn()).unwrap());
    assert_eq!(index_drift(&store).unwrap(), (0, 0));

    let hits = |query: &str| -> Vec<i64> {
        search(&store, "acct", query, None, 20, SearchOrder::Relevance)
            .unwrap()
            .into_iter()
            .map(|h| h.row.id)
            .collect()
    };
    assert_eq!(hits("dachsanierung"), vec![row], "a whole body word");
    assert_eq!(hits("body:Dachsanierung beginnt"), vec![row]);
    assert_eq!(hits("gebot"), vec![row], "the subject by substring");
    assert!(
        hits("sanierung").is_empty(),
        "an evicted body has no substrings to match"
    );
}

/// The byte bound ends a batch early but never before its first row, so a
/// store of large bodies still progresses one row at a time.
#[test]
fn the_backfill_bounds_a_batch_by_body_bytes() {
    use mailypoppins::store::fts::{backfill_trigram_bounded, META_TRIGRAM_COMPLETE};
    let f = Fixture::new();
    for uid in 1..=3 {
        f.ingest(
            "inbox",
            uid,
            &email(&format!("m{uid}"), "a@example.com", "Big", &"x".repeat(100)),
        );
    }
    f.store
        .conn()
        .execute("DELETE FROM meta WHERE key = ?1", [META_TRIGRAM_COMPLETE])
        .unwrap();
    let batch = backfill_trigram_bounded(&f.store, &f.blobs, 10, 1).unwrap();
    assert_eq!((batch.indexed, batch.complete), (1, false));
    let batch = backfill_trigram_bounded(&f.store, &f.blobs, 10, 150).unwrap();
    assert_eq!((batch.indexed, batch.complete), (2, true));
}

/// `limit` is honoured, and it takes the best-ranked hits rather than an
/// arbitrary slice.
#[test]
fn the_limit_caps_the_ranked_hits() {
    let f = Fixture::new();
    for uid in 1..=5 {
        f.ingest(
            "inbox",
            uid,
            &email(
                &format!("m{uid}"),
                "a@example.com",
                "Body hit",
                "common term here",
            ),
        );
    }
    let best = f.ingest(
        "inbox",
        6,
        &email("m6", "a@example.com", "Common", "subject hit"),
    );
    let hits = search(&f.store, "acct", "common", None, 2, SearchOrder::Relevance).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].row.id, best);
}
