//! The engine-free half of the contact extractor (#0126, P5-U10b).
//!
//! Everything here observes addresses that a caller already has in hand: the
//! `ObservedIn` hook kinds, the header parse, the self-address filter and the
//! per-observation merge. The full rebuild, which reads `messages` through
//! `store::read`, stays in the root crate beside the store and calls back into
//! these; the items it needs are `pub` and `#[doc(hidden)]` for that reason
//! and no other.

use crate::config::AccountConfig;
use crate::contacts::filter::is_usable_address;
use crate::contacts::rank::{update_from_observation, Observation, ObservationField};
use crate::contacts::types::{Contact, ContactIndex};
use crate::types::MailboxRole;
use anyhow::Result;
use chrono::Utc;
use mailparse::addrparse;
use std::collections::HashMap;

/// Public observation kind used by incremental-update hooks (send/sync).
#[derive(Debug, Clone, Copy)]
pub enum ObservedIn {
    /// Recipient was in the `to:` field of a sent message.
    SentTo,
    /// Recipient was in the `cc:` or `bcc:` field of a sent message.
    SentCc,
    /// Address was observed in an inbox/archive message.
    Inbox,
}

/// The `observed_at` a row with an absent or unparseable `Date:` header gets.
///
/// It has to be a constant rather than "now": `last_seen` is the frecency
/// tiebreaker inside a tier, so stamping undated mail with the wall clock made
/// it float to the top of its tier and gave it a different value on every
/// rebuild, i.e. a nondeterministic index (#0067). The epoch is the same rule
/// the store applies to the same rows — ingest marks an unparseable date with
/// `date_sort = 0` and the listings sort those last — so undated mail sinks in
/// both stacks instead of floating in one of them.
#[doc(hidden)]
pub const UNDATED_OBSERVED_AT: &str = "1970-01-01T00:00:00+00:00";

/// The account's own address, lowercased, for self-filtering.
///
/// `default_from` is a config string a user is free to write as
/// `Name <addr@host>`, and comparing that verbatim against a parsed header
/// address never matched, so the user's own address stayed in their own
/// contact corpus (#0067). Parse it the same way the headers are parsed and
/// fall back to the raw string when it does not parse as an address.
#[doc(hidden)]
pub fn self_address(account: &AccountConfig) -> String {
    let raw = account.default_from.trim();
    match addrparse(raw).ok().and_then(|addrs| {
        addrs.iter().find_map(|info| match info {
            mailparse::MailAddr::Single(s) => Some(s.addr.clone()),
            mailparse::MailAddr::Group(g) => g.addrs.first().map(|s| s.addr.clone()),
        })
    }) {
        Some(addr) => addr.to_ascii_lowercase(),
        None => raw.to_ascii_lowercase(),
    }
}

#[doc(hidden)]
pub fn empty_index(account: &AccountConfig) -> ContactIndex {
    ContactIndex {
        account: account.name.clone(),
        contacts: HashMap::new(),
        built_at: Utc::now().to_rfc3339(),
    }
}

#[doc(hidden)]
pub fn process_header(
    contacts: &mut HashMap<String, Contact>,
    raw: &str,
    field: ObservationField,
    role: &MailboxRole,
    observed_at: &str,
    self_addr: &str,
) {
    let Ok(parsed_addrs) = addrparse(raw) else {
        return;
    };
    for info in parsed_addrs.iter() {
        for (addr, name) in flatten_addr(info) {
            let addr_lc = addr.to_ascii_lowercase();
            if addr_lc == self_addr {
                continue;
            }
            if !is_usable_address(&addr_lc) {
                continue;
            }
            let obs = Observation {
                address: addr_lc,
                display_name: name,
                mailbox_role: role.clone(),
                field,
                observed_at: observed_at.to_string(),
            };
            update_from_observation(contacts, obs);
        }
    }
}

fn flatten_addr(info: &mailparse::MailAddr) -> Vec<(String, String)> {
    match info {
        mailparse::MailAddr::Single(s) => {
            vec![(s.addr.clone(), s.display_name.clone().unwrap_or_default())]
        }
        mailparse::MailAddr::Group(g) => g
            .addrs
            .iter()
            .map(|s| (s.addr.clone(), s.display_name.clone().unwrap_or_default()))
            .collect(),
    }
}

/// Convert common email date formats to RFC-3339 in UTC. Returns `None` if
/// parsing fails.
///
/// Normalised to UTC because `first_seen` and `last_seen` are compared as
/// strings, which orders instants only when they share an offset.
#[doc(hidden)]
pub fn parse_date_to_rfc3339(s: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(s)
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(s))
        .ok()?;
    Some(parsed.with_timezone(&Utc).to_rfc3339())
}

/// Incremental update: merge a batch of address observations into an existing
/// index. Hook point for send-post-success and sync-post-new-message updates.
///
/// Caller is responsible for persisting the index via `cache::save_cache`
/// after calling this.
pub fn observe(
    index: &mut ContactIndex,
    self_addr: &str,
    observations: &[(ObservedIn, &str)],
    observed_at: &str,
) -> Result<()> {
    let self_lc = self_addr.to_ascii_lowercase();
    for (kind, raw_header) in observations {
        if raw_header.trim().is_empty() {
            continue;
        }
        let Ok(parsed) = addrparse(raw_header) else {
            continue;
        };
        let (role, field): (MailboxRole, ObservationField) = match kind {
            ObservedIn::SentTo => (MailboxRole::Sent, ObservationField::To),
            ObservedIn::SentCc => (MailboxRole::Sent, ObservationField::Cc),
            ObservedIn::Inbox => (MailboxRole::Inbox, ObservationField::From),
        };
        for info in parsed.iter() {
            for (addr, name) in flatten_addr(info) {
                let addr_lc = addr.to_ascii_lowercase();
                if addr_lc == self_lc || !is_usable_address(&addr_lc) {
                    continue;
                }
                let obs = Observation {
                    address: addr_lc,
                    display_name: name,
                    mailbox_role: role.clone(),
                    field,
                    observed_at: observed_at.to_string(),
                };
                update_from_observation(&mut index.contacts, obs);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_index() -> ContactIndex {
        ContactIndex {
            account: "test".into(),
            contacts: HashMap::new(),
            built_at: Utc::now().to_rfc3339(),
        }
    }

    /// Dates from senders in different offsets order by instant: 20:00 at
    /// -08:00 is later than 01:00 at +00:00 the next day, so it is the last
    /// sighting and its display name wins.
    #[test]
    fn sightings_order_by_instant_across_offsets() {
        let mut index = empty_index();
        let pacific = parse_date_to_rfc3339("2026-01-01T20:00:00-08:00").unwrap();
        let utc = parse_date_to_rfc3339("2026-01-02T01:00:00+00:00").unwrap();
        for (name, at) in [("Pacific Alice", &pacific), ("Utc Alice", &utc)] {
            observe(
                &mut index,
                "me@example.com",
                &[(ObservedIn::Inbox, &format!("{name} <alice@example.com>"))],
                at,
            )
            .unwrap();
        }
        let alice = &index.contacts["alice@example.com"];
        assert_eq!(alice.last_seen, pacific);
        assert_eq!(alice.first_seen, utc);
        assert_eq!(alice.display_name, "Pacific Alice");
    }

    #[test]
    fn observe_bumps_sent_to_counter() {
        let mut index = empty_index();
        observe(
            &mut index,
            "me@example.com",
            &[(ObservedIn::SentTo, "Alice <alice@example.com>")],
            "2026-04-08T00:00:00Z",
        )
        .unwrap();

        let c: &Contact = index
            .contacts
            .get("alice@example.com")
            .expect("alice added");
        assert_eq!(c.sent_to, 1);
        assert_eq!(c.sent_cc, 0);
        assert_eq!(c.received, 0);
        assert_eq!(c.display_name, "Alice");
    }

    #[test]
    fn observe_skips_self_address() {
        let mut index = empty_index();
        observe(
            &mut index,
            "me@example.com",
            &[(ObservedIn::SentTo, "me@example.com, bob@example.com")],
            "2026-04-08T00:00:00Z",
        )
        .unwrap();

        assert!(!index.contacts.contains_key("me@example.com"));
        assert!(index.contacts.contains_key("bob@example.com"));
    }

    #[test]
    fn observe_skips_noreply() {
        let mut index = empty_index();
        observe(
            &mut index,
            "me@example.com",
            &[(ObservedIn::Inbox, "no-reply@example.com")],
            "2026-04-08T00:00:00Z",
        )
        .unwrap();

        assert!(index.contacts.is_empty());
    }

    #[test]
    fn observe_accumulates_counts_across_calls() {
        let mut index = empty_index();
        for _ in 0..3 {
            observe(
                &mut index,
                "me@example.com",
                &[(ObservedIn::SentTo, "alice@example.com")],
                "2026-04-08T00:00:00Z",
            )
            .unwrap();
        }
        assert_eq!(index.contacts.get("alice@example.com").unwrap().sent_to, 3);
    }

    #[test]
    fn observe_updates_last_seen_with_newer_timestamp() {
        let mut index = empty_index();
        observe(
            &mut index,
            "me@example.com",
            &[(ObservedIn::SentTo, "Old Name <alice@example.com>")],
            "2025-01-01T00:00:00Z",
        )
        .unwrap();
        observe(
            &mut index,
            "me@example.com",
            &[(ObservedIn::SentTo, "New Name <alice@example.com>")],
            "2026-04-08T00:00:00Z",
        )
        .unwrap();

        let c = index.contacts.get("alice@example.com").unwrap();
        assert_eq!(c.display_name, "New Name");
        assert_eq!(c.sent_to, 2);
    }

    #[test]
    fn observe_handles_multi_recipient_header() {
        let mut index = empty_index();
        observe(
            &mut index,
            "me@example.com",
            &[(
                ObservedIn::SentTo,
                "\"A User\" <a@x.com>, B User <b@x.com>, c@x.com",
            )],
            "2026-04-08T00:00:00Z",
        )
        .unwrap();

        assert_eq!(index.contacts.len(), 3);
        assert_eq!(
            index.contacts.get("a@x.com").unwrap().display_name,
            "A User"
        );
        assert_eq!(
            index.contacts.get("b@x.com").unwrap().display_name,
            "B User"
        );
    }
}
