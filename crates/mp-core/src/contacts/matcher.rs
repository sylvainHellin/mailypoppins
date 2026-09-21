//! Fuzzy matcher wrapper around `nucleo-matcher::Pattern`.
//!
//! Scores each contact's `"Display Name <address>"` haystack, sorts by
//! (nucleo score desc, tier tuple / frecency desc), and returns the top-N.

use crate::contacts::rank::compare;
use crate::contacts::types::{Contact, ContactIndex};
use chrono::Utc;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

#[derive(Debug, Clone)]
pub struct MatchResult<'a> {
    pub contact: &'a Contact,
    pub score: u32,
}

/// Fuzzy-match `query` against the index. If `query` is empty, return all
/// contacts sorted by rank. Otherwise return fuzzy matches sorted by
/// (nucleo score descending, rank descending).
pub fn search<'a>(index: &'a ContactIndex, query: &str, limit: usize) -> Vec<MatchResult<'a>> {
    let now = Utc::now();
    let mut all: Vec<&Contact> = index.contacts.values().collect();
    all.sort_by(|a, b| compare(a, b, now));

    if query.trim().is_empty() {
        return all
            .into_iter()
            .take(limit)
            .map(|c| MatchResult {
                contact: c,
                score: u32::MAX,
            })
            .collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    // Build haystacks: "Display Name <address>" (or just address if no name).
    // Score each contact individually so we retain the index→contact mapping.
    let mut buf = Vec::new();
    let mut results: Vec<MatchResult<'a>> = all
        .iter()
        .filter_map(|&c| {
            let haystack = if c.display_name.is_empty() {
                c.address.clone()
            } else {
                format!("{} <{}>", c.display_name, c.address)
            };
            let utf32 = Utf32Str::new(&haystack, &mut buf);
            pattern
                .score(utf32, &mut matcher)
                .map(|score| MatchResult { contact: c, score })
        })
        .collect();

    // Primary: nucleo score desc. Secondary: tier/frecency.
    results.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| compare(a.contact, b.contact, now))
    });
    results.truncate(limit);
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contacts::types::{Contact, ContactSource};
    use std::collections::HashMap;

    fn mk(addr: &str, name: &str, sent_to: u32) -> Contact {
        Contact {
            address: addr.into(),
            display_name: name.into(),
            sent_to,
            sent_cc: 0,
            received: 0,
            first_seen: "2026-01-01T00:00:00Z".into(),
            last_seen: "2026-04-08T00:00:00Z".into(),
            source: ContactSource::Local,
        }
    }

    fn idx(contacts: Vec<Contact>) -> ContactIndex {
        let mut map = HashMap::new();
        for c in contacts {
            map.insert(c.address.clone(), c);
        }
        ContactIndex {
            account: "test".into(),
            contacts: map,
            built_at: "2026-04-08T00:00:00Z".into(),
        }
    }

    #[test]
    fn empty_query_returns_top_by_rank() {
        let index = idx(vec![
            mk("c@x.com", "Carol", 3),
            mk("a@x.com", "Alice", 1),
            mk("b@x.com", "Bob", 2),
        ]);
        let results = search(&index, "", 10);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].contact.address, "c@x.com");
        assert_eq!(results[1].contact.address, "b@x.com");
        assert_eq!(results[2].contact.address, "a@x.com");
    }

    #[test]
    fn fuzzy_query_matches_name_or_address() {
        let index = idx(vec![
            mk("alice@foo.com", "Alice Smith", 1),
            mk("bob@bar.com", "Bob Jones", 1),
        ]);
        let results = search(&index, "alice", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].contact.address, "alice@foo.com");

        let results = search(&index, "jones", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].contact.address, "bob@bar.com");
    }

    #[test]
    fn no_match_returns_empty() {
        let index = idx(vec![mk("alice@foo.com", "Alice", 1)]);
        let results = search(&index, "xyzzy", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn respects_limit() {
        let index = idx(vec![
            mk("a@x.com", "Aaron", 1),
            mk("b@x.com", "Bart", 1),
            mk("c@x.com", "Carl", 1),
        ]);
        let results = search(&index, "", 2);
        assert_eq!(results.len(), 2);
    }
}
