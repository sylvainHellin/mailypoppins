//! Full-text search over the store's FTS5 index (#0043).
//!
//! Search is a `SELECT`, not a file stream. The word index (`messages_fts`)
//! has existed since #0038 and the trigram index (`messages_fts_tri`) since
//! PERSO-276 ([`super::fts`]); both are written inside the same transaction
//! as the `messages` row they describe ([`crate::ingest`]), and removed by
//! every delete path (`store::write::delete_row`, `pending_ops::apply_delete`,
//! and the prune through `delete_by_uid`), so they never need a reconcile
//! pass. What was missing was a way to ask them a question; this module is
//! that.
//!
//! A term of a single word of three characters or more matches any part of a
//! word, whatever the case, through the trigram index; a shorter one matches
//! the beginning of a word through the word index, and a phrase of several
//! words stays a word-index phrase ([`crate::search::to_fts_substring`]). A
//! store whose trigram index is still being backfilled
//! ([`super::fts::trigram_ready`]) answers from the word index alone, whole
//! words as before ([`crate::search::to_fts`]).
//!
//! Two constraints come from the indexes being *contentless*
//! (`content=''`, `contentless_delete=1`, see [`super::schema`]):
//!
//! - Only `rowid`-returning `MATCH` queries work. `snippet()` and
//!   `highlight()` fail, so a hit is rendered from the `messages` row it joins
//!   to (which is where the stored snippet lives) and never from the index.
//! - Neither index can be rebuilt from itself: the body text lives in a blob
//!   and the index keeps no copy. That is not a gap, because the store is a
//!   cache with a drop-and-rebuild contract: a file whose index is suspect is
//!   deleted and refilled by the next sync. [`index_drift`] is the check that
//!   says whether it is suspect, so the question can be asked without guessing.
//!   The trigram backfill reads the body blobs ([`super::fts::backfill_trigram`]),
//!   which is what lets an older store gain the second index without a resync.
//!
//! The user's query is *not* passed to FTS5 verbatim. A bare MATCH expression
//! makes half of ordinary typing a syntax error (`c++`, `foo:bar`, a stray
//! quote), so the query goes through the one shared parser in
//! [`crate::search`] (#0086a) and is lowered here by [`crate::search::to_fts`]
//! into a quoted `MATCH` expression plus the SQL predicates the contentless
//! index cannot carry (an attachment column test, an attachment-name match
//! against `message_blobs.filename`, and a date range). That single
//! parser is what closes the #0043 two-grammar debt: `--local` and the server
//! path now read the same input. [`fts_expression`] survives as a thin renderer
//! for the callers and tests that only want the `MATCH` string.

use anyhow::{Context, Result};

use super::fts::{TRIGRAM_TABLE, WORD_TABLE};
use super::read::{row_columns, row_from_sql, MessageRow, CALENDAR_SIDECAR_NAME};
use super::{fold_case, Store, FOLD_FUNCTION};

pub use crate::search::fts_expression;
pub use mp_protocol::listing::SearchOrder;

/// The bm25 column weights of either index: `subject`, `from_`, `body_text`.
const BM25_WEIGHTS: &str = "10.0, 5.0, 1.0";

/// One ranked hit: the message row and the bm25 score it matched with.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub row: MessageRow,
    /// bm25 rank. FTS5 returns a *negative* number and a smaller (more
    /// negative) one is a better match, which is why the ordering is `ASC`.
    /// A query answered by both indexes ranks by the sum of the two scores.
    /// The trigram index counts every three-character run as a token, so its
    /// scores sit on another scale than the word index's (a longer needle
    /// scores higher) and only compare among hits of one query.
    pub rank: f64,
}

/// A `YYYY-MM-DD` date as the unix timestamp of its UTC midnight, which is the
/// same clock `ingest::date_sort_for` stamps rows with. `after:D` keeps rows at
/// or past that instant, `before:D` keeps rows strictly before it.
fn date_to_timestamp(date: &str) -> Option<i64> {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp())
}

/// A `filename:` substring as a `LIKE` pattern over the [`fold_case`]d name:
/// the needle is folded the same way, and `%`, `_` and the escape character
/// itself match literally, so `filename:100%` is not a wildcard.
fn filename_like_pattern(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len() + 2);
    out.push('%');
    for c in fold_case(needle).chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// Search one account's messages, in `order` (PERSO-273).
///
/// `mailbox` scopes the search to one mailbox key (the `MailboxRole` spelling
/// ingest recorded); `None` searches every mailbox of the account, which is
/// the point of the command -- the file era could only ever grep one directory
/// at a time.
///
/// The order is the SQL's, so `limit` keeps the first rows of it: the newest
/// or the oldest matches, or the best ones. A row whose `Date:` did not parse
/// (`date_sort` 0) is last in both date orders, and a `Date:` before 1970
/// (a negative `date_sort`) is a date like any other. The bm25 weights `relevance`
/// ranks by are `subject`, `from_`, `body_text` in that order: a word in the
/// subject outranks the same word buried in a quoted reply chain. A query
/// whose only text terms are OR groups split across both indexes has no
/// score, and relevance is then newest first.
pub fn search(
    store: &Store,
    account: &str,
    query: &str,
    mailbox: Option<&str>,
    limit: usize,
    order: SearchOrder,
) -> Result<Vec<SearchHit>> {
    let parsed = crate::search::parse(query).map_err(|e| anyhow::anyhow!("{e}"))?;
    search_ast(store, account, &parsed, mailbox, limit, order)
}

/// The `ORDER BY` term true for a row whose `Date:` did not parse: ingest
/// stamps it 0 (`date_sort_for`), an older row may carry none. `date_sort` is
/// a Unix timestamp, so a negative one is a real date before 1970, not a
/// missing one.
macro_rules! undated_last {
    () => {
        "(messages.date_sort IS NULL OR messages.date_sort = 0)"
    };
}

/// The `ORDER BY` of one search `order`, over the `rank` column the query
/// selects: an undated row last in every order, as
/// [`SearchOrder::by_date`] and the server search put it.
fn order_by(order: SearchOrder) -> &'static str {
    match order {
        SearchOrder::Newest => concat!(
            undated_last!(),
            ", messages.date_sort DESC, messages.id DESC"
        ),
        SearchOrder::Oldest => {
            concat!(undated_last!(), ", messages.date_sort ASC, messages.id ASC")
        }
        SearchOrder::Relevance => concat!(
            "rank ASC, ",
            undated_last!(),
            ", messages.date_sort DESC, messages.id DESC"
        ),
    }
}

/// The same search over an already-parsed [`crate::search::Query`], so the
/// `mp search --local` flags (`--from`, `--has-attachment`, ...) reach the FTS
/// index through the same AST the positional grammar builds.
pub fn search_ast(
    store: &Store,
    account: &str,
    parsed: &crate::search::Query,
    mailbox: Option<&str>,
    limit: usize,
    order: SearchOrder,
) -> Result<Vec<SearchHit>> {
    // Substrings once the trigram index holds every row, whole words until
    // then (PERSO-276).
    let render = if super::fts::trigram_ready(store.conn())? {
        crate::search::to_fts_substring(parsed)
    } else {
        crate::search::to_fts(parsed)
    }
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    // A query with no clause at all (an empty string, or bare punctuation) has
    // nothing to answer, the same honest refusal the old parser gave.
    if render.match_expr.is_none()
        && render.trigram_expr.is_none()
        && render.either.is_empty()
        && !render.has_attachment
        && render.filenames.is_empty()
        && render.before.is_none()
        && render.after.is_none()
    {
        anyhow::bail!("nothing to search for: the query has no searchable words");
    }

    let columns = row_columns();

    // The predicates the contentless FTS index cannot carry ride as ordinary
    // SQL over the joined `messages` row: an attachment column test, an
    // attachment-name match, and a date range against the same `date_sort`
    // unix seconds ingest stamped.
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut predicates = String::new();

    // `?1` is always the account. An index with a `MATCH` of its own ranks by
    // its bm25, and both together by the sum, in one SQL statement, so the
    // order and the limit are the SQL's whatever mix of terms the query holds.
    // The trigram index is joined on the rowid; the word index beside it is a
    // materialised set joined on the id. Joined as a second FTS table instead,
    // it re-runs its `MATCH` for every trigram hit: `mail ab` took 1.4 s on
    // the TUM store that way and 17 ms this way.
    binds.push(Box::new(account.to_string()));
    let (trigram, word) = (TRIGRAM_TABLE, WORD_TABLE);
    let mut with_clause = String::new();
    let (from_clause, rank_expr, matches) = match (&render.trigram_expr, &render.match_expr) {
        (Some(trigrams), Some(words)) => {
            with_clause = format!(
                "WITH words AS MATERIALIZED (\
                     SELECT rowid AS id, bm25({word}, {BM25_WEIGHTS}) AS score \
                     FROM {word} WHERE {word} MATCH ?{}) ",
                binds.len() + 1
            );
            binds.push(Box::new(words.clone()));
            let matches = format!(" AND {trigram} MATCH ?{}", binds.len() + 1);
            binds.push(Box::new(trigrams.clone()));
            (
                format!(
                    "{trigram} JOIN messages ON messages.id = {trigram}.rowid \
                         JOIN words ON words.id = messages.id"
                ),
                format!("bm25({trigram}, {BM25_WEIGHTS}) + words.score"),
                matches,
            )
        }
        (Some(expr), None) | (None, Some(expr)) => {
            let table = if render.trigram_expr.is_some() {
                trigram
            } else {
                word
            };
            let matches = format!(" AND {table} MATCH ?{}", binds.len() + 1);
            binds.push(Box::new(expr.clone()));
            (
                format!("{table} JOIN messages ON messages.id = {table}.rowid"),
                format!("bm25({table}, {BM25_WEIGHTS})"),
                matches,
            )
        }
        (None, None) => ("messages".to_string(), "0.0".to_string(), String::new()),
    };
    let base_where = format!("messages.account = ?1{matches}");

    // An OR group split across the indexes matches a row either one matches.
    for group in &render.either {
        predicates.push_str(&format!(
            " AND (messages.id IN (SELECT rowid FROM {words} WHERE {words} MATCH ?{w}) \
             OR messages.id IN (SELECT rowid FROM {trigrams} WHERE {trigrams} MATCH ?{t}))",
            words = WORD_TABLE,
            trigrams = TRIGRAM_TABLE,
            w = binds.len() + 1,
            t = binds.len() + 2,
        ));
        binds.push(Box::new(group.words.clone()));
        binds.push(Box::new(group.trigrams.clone()));
    }

    if let Some(mb) = mailbox {
        predicates.push_str(&format!(" AND messages.mailbox = ?{}", binds.len() + 1));
        binds.push(Box::new(mb.to_string()));
    }
    if render.has_attachment {
        predicates.push_str(" AND messages.has_attachments = 1");
    }
    // `filename:` matches the names ingest recorded on `message_blobs`, which
    // stay after retention evicts the bytes. The iMIP sidecar is excluded, as
    // the read path excludes it from a message's attachments. `LIKE` folds
    // ASCII case only, so both sides are folded first: the stored name by the
    // store's `mp_fold` function, the pattern by the same Rust fold
    // (PERSO-134). `ß` and `SS` still differ, as a per-character fold leaves them.
    for group in &render.filenames {
        let mut alternatives: Vec<String> = Vec::new();
        for needle in group {
            alternatives.push(format!(
                "{FOLD_FUNCTION}(b.filename) LIKE ?{} ESCAPE '\\'",
                binds.len() + 1
            ));
            binds.push(Box::new(filename_like_pattern(needle)));
        }
        predicates.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM message_blobs b \
             WHERE b.message_row = messages.id AND b.kind = 'attachment' \
               AND b.filename <> '{CALENDAR_SIDECAR_NAME}' AND ({}))",
            alternatives.join(" OR ")
        ));
    }
    if let Some(ref after) = render.after {
        let ts = date_to_timestamp(after)
            .with_context(|| format!("after: is not a valid date: {after}"))?;
        predicates.push_str(&format!(" AND messages.date_sort >= ?{}", binds.len() + 1));
        binds.push(Box::new(ts));
    }
    if let Some(ref before) = render.before {
        let ts = date_to_timestamp(before)
            .with_context(|| format!("before: is not a valid date: {before}"))?;
        predicates.push_str(&format!(" AND messages.date_sort < ?{}", binds.len() + 1));
        binds.push(Box::new(ts));
    }
    let limit_placeholder = binds.len() + 1;
    binds.push(Box::new(limit as i64));

    let order_by = order_by(order);
    let sql = format!(
        "{with_clause}SELECT {columns}, {rank_expr} AS rank
         FROM {from_clause}
         WHERE {base_where}{predicates}
         ORDER BY {order_by}
         LIMIT ?{limit_placeholder}"
    );
    let mut stmt = store
        .conn()
        .prepare(&sql)
        .context("preparing the full-text search")?;
    let map = |row: &rusqlite::Row<'_>| {
        Ok(SearchHit {
            row: row_from_sql(row)?,
            rank: row.get("rank")?,
        })
    };
    // An FTS5 syntax error surfaces here rather than at prepare time, and the
    // expression is generated, so a failure is a bug in the translation and not
    // something to hand to the user as SQL.
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
            map,
        )
        .context("running the full-text search")?;
    let mut out = Vec::new();
    for hit in rows {
        out.push(hit.context("reading a search hit")?);
    }
    Ok(out)
}

/// How far the indexes have drifted from the rows: `(rows not indexed, index
/// entries with no row)`, summed over the word and the trigram index.
///
/// Both are zero in a store every writer went through, because every write
/// path indexes inside the transaction that writes the row and every delete
/// path removes the entries in the transaction that removes it. It is here so
/// the invariant is *checkable* rather than merely claimed, which is what the
/// tests assert after ingest, re-ingest, delete and prune. A trigram index
/// still being backfilled ([`super::fts::trigram_ready`]) owes rows by
/// design, so only its orphans count until it is complete. A non-zero answer
/// is not repairable in place (a contentless index cannot be rebuilt from the
/// store), so the remedy is the drop-and-rebuild contract the store already
/// has: delete the file and sync.
pub fn index_drift(store: &Store) -> Result<(i64, i64)> {
    let conn = store.conn();
    let count = |sql: String, what: &str| -> Result<i64> {
        conn.query_row(&sql, [], |r| r.get(0))
            .with_context(|| format!("counting {what}"))
    };
    let unindexed_in = |table: &str| {
        count(
            format!("SELECT COUNT(*) FROM messages WHERE id NOT IN (SELECT rowid FROM {table})"),
            "unindexed messages",
        )
    };
    let orphaned_in = |table: &str| {
        count(
            format!("SELECT COUNT(*) FROM {table} WHERE rowid NOT IN (SELECT id FROM messages)"),
            "orphaned index entries",
        )
    };
    let mut unindexed = unindexed_in(super::fts::WORD_TABLE)?;
    if super::fts::trigram_ready(conn)? {
        unindexed += unindexed_in(super::fts::TRIGRAM_TABLE)?;
    }
    let orphaned = orphaned_in(super::fts::WORD_TABLE)? + orphaned_in(super::fts::TRIGRAM_TABLE)?;
    Ok((unindexed, orphaned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_query_becomes_and_ed_quoted_terms() {
        assert_eq!(
            fts_expression("hello world").unwrap(),
            "\"hello\" \"world\""
        );
    }

    #[test]
    fn punctuation_is_quoted_rather_than_parsed() {
        // Bare, every one of these is an FTS5 syntax error, so each term is
        // wrapped in quotes. Migration note (#0086a): parentheses are now
        // grammar, so `(draft)` is a one-term group rendered as `"draft"`,
        // not the literal `"(draft)"` the retired parser produced.
        assert_eq!(fts_expression("c++ (draft)").unwrap(), "\"c++\" \"draft\"");
        assert_eq!(fts_expression("say \"\"hi").unwrap(), "\"say\" \"hi\"");
    }

    #[test]
    fn a_quoted_phrase_stays_one_term() {
        assert_eq!(
            fts_expression("\"quarterly report\" urgent").unwrap(),
            "\"quarterly report\" \"urgent\""
        );
    }

    #[test]
    fn an_unterminated_quote_searches_what_was_typed() {
        assert_eq!(fts_expression("\"half typed").unwrap(), "\"half typed\"");
    }

    #[test]
    fn a_trailing_star_is_a_prefix_query() {
        assert_eq!(fts_expression("invoi*").unwrap(), "\"invoi\"*");
    }

    #[test]
    fn a_star_inside_a_phrase_is_literal() {
        assert_eq!(fts_expression("\"a*b\"").unwrap(), "\"a*b\"");
    }

    #[test]
    fn known_fields_become_column_filters() {
        assert_eq!(
            fts_expression("subject:invoice").unwrap(),
            "subject:\"invoice\""
        );
        assert_eq!(fts_expression("from:ada").unwrap(), "from_:\"ada\"");
        assert_eq!(
            fts_expression("body:ledger").unwrap(),
            "body_text:\"ledger\""
        );
    }

    #[test]
    fn an_unknown_field_is_searched_as_text() {
        assert_eq!(fts_expression("re:budget").unwrap(), "\"re:budget\"");
    }

    #[test]
    fn a_query_with_no_words_is_refused() {
        assert!(fts_expression("").is_err());
        assert!(fts_expression("   ").is_err());
        assert!(fts_expression("-- ??").is_err());
    }
}
