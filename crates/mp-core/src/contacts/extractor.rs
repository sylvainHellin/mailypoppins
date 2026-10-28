//! The engine-free half of the contact extractor (#0126, P5-U10b).
//!
//! Everything here observes addresses that a caller already has in hand: the
//! header parse, the self-address filter and the per-observation merge. The
//! full rebuild, which reads `messages` through `store::read`, stays in the
//! root crate beside the store, and so do the send and sync hooks.
//!
//! All three fold a message in through one function, [`observe_message`]
//! (PERSO-234): the hooks once had a rule of their own, which counted what
//! the rebuild did not and filtered the user's own address where the rebuild
//! did not, so an index kept up incrementally drifted from the one a rebuild
//! produced. One rule cannot drift, and [`EXTRACTOR_VERSION`] names it, so a
//! cache an older rule built is rebuilt rather than kept.

use crate::config::AccountConfig;
use crate::contacts::filter::is_usable_address;
use crate::contacts::rank::{update_from_observation, Observation, ObservationField};
use crate::contacts::types::{Contact, ContactIndex};
use crate::types::MailboxRole;
use chrono::Utc;
use mailparse::addrparse;
use std::collections::HashMap;

/// The version of the observation rule, stamped on every index a rebuild
/// writes ([`ContactIndex::extractor_version`]).
///
/// Bump it whenever [`observe_message`] or the rebuild's walk changes what an
/// index holds for the same mail: a cache whose stamp differs, a missing
/// stamp included, was built by another rule, and the first read of it
/// rebuilds it before it answers (PERSO-234). Without it a reinstall that
/// taught the extractor something kept the old, thinner cache for ever,
/// because a cache was only ever built when the file was missing.
///
/// 1: PERSO-234, one rule for the rebuild and both hooks, `Bcc:` observed.
pub const EXTRACTOR_VERSION: u32 = 1;

/// The address headers of one message, as a store row or a fetched email
/// carries them, verbatim.
#[derive(Debug, Clone, Copy, Default)]
pub struct MessageAddresses<'a> {
    pub from: Option<&'a str>,
    pub to: Option<&'a str>,
    pub cc: Option<&'a str>,
    pub bcc: Option<&'a str>,
}

impl<'a> MessageAddresses<'a> {
    /// The headers in the order the ranker sees them; `Bcc:` counts as `Cc:`,
    /// so a blind copy on sent mail is a `sent_cc` and anywhere else a
    /// `received`, which is what [`Contact::sent_cc`] says it counts.
    fn fields(&self) -> [(ObservationField, Option<&'a str>); 4] {
        [
            (ObservationField::From, self.from),
            (ObservationField::To, self.to),
            (ObservationField::Cc, self.cc),
            (ObservationField::Cc, self.bcc),
        ]
    }
}

/// Fold one message into `contacts`: the one observation rule the full
/// rebuild, the sync hook and the send hook all apply (PERSO-234).
///
/// `role` is the mailbox the message sits in, and only `sent` changes the
/// ranking. `self_addr` is [`AccountConfig::own_address`], already
/// lowercased, and is never a contact. `date` is the `Date:` header verbatim;
/// an absent or unparseable one is observed at [`UNDATED_OBSERVED_AT`].
pub fn observe_message(
    contacts: &mut HashMap<String, Contact>,
    self_addr: &str,
    role: &MailboxRole,
    addresses: &MessageAddresses<'_>,
    date: Option<&str>,
) {
    let observed_at = date
        .and_then(parse_date_to_rfc3339)
        .unwrap_or_else(|| UNDATED_OBSERVED_AT.to_string());
    for (field, raw) in addresses.fields() {
        let Some(raw) = raw else { continue };
        if raw.trim().is_empty() {
            continue;
        }
        process_header(contacts, raw, field, role, &observed_at, self_addr);
    }
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

/// The index a rebuild starts from: no contacts, built now, stamped with the
/// rule that is about to fill it.
#[doc(hidden)]
pub fn empty_index(account: &AccountConfig) -> ContactIndex {
    ContactIndex {
        account: account.name.clone(),
        contacts: HashMap::new(),
        built_at: Utc::now().to_rfc3339(),
        extractor_version: EXTRACTOR_VERSION,
    }
}

fn process_header(
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
fn parse_date_to_rfc3339(s: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(s)
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(s))
        .ok()?;
    Some(parsed.with_timezone(&Utc).to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "me@example.com";

    fn sent() -> MailboxRole {
        MailboxRole::Sent
    }

    fn to(header: &str) -> MessageAddresses<'_> {
        MessageAddresses {
            to: Some(header),
            ..Default::default()
        }
    }

    /// Dates from senders in different offsets order by instant: 20:00 at
    /// -08:00 is later than 01:00 at +00:00 the next day, so it is the last
    /// sighting and its display name wins.
    #[test]
    fn sightings_order_by_instant_across_offsets() {
        let mut contacts = HashMap::new();
        let pacific = "2026-01-01T20:00:00-08:00";
        let utc = "2026-01-02T01:00:00+00:00";
        for (name, at) in [("Pacific Alice", pacific), ("Utc Alice", utc)] {
            let from = format!("{name} <alice@example.com>");
            let addresses = MessageAddresses {
                from: Some(&from),
                ..Default::default()
            };
            observe_message(&mut contacts, ME, &MailboxRole::Inbox, &addresses, Some(at));
        }
        let alice = &contacts["alice@example.com"];
        assert_eq!(alice.last_seen, parse_date_to_rfc3339(pacific).unwrap());
        assert_eq!(alice.first_seen, parse_date_to_rfc3339(utc).unwrap());
        assert_eq!(alice.display_name, "Pacific Alice");
    }

    /// A sent message's `To:` is a `sent_to`, its `Cc:` and `Bcc:` are each a
    /// `sent_cc`; the same headers on received mail are all `received`.
    #[test]
    fn the_role_and_the_field_pick_the_counter() {
        let addresses = MessageAddresses {
            from: Some(ME),
            to: Some("Alice <alice@example.com>"),
            cc: Some("bob@example.com"),
            bcc: Some("carol@example.com"),
        };
        let mut contacts = HashMap::new();
        observe_message(&mut contacts, ME, &sent(), &addresses, None);
        let tuple = |c: &HashMap<String, Contact>, a: &str| c[a].tier_tuple();
        assert_eq!(tuple(&contacts, "alice@example.com"), (1, 0, 0));
        assert_eq!(contacts["alice@example.com"].display_name, "Alice");
        assert_eq!(tuple(&contacts, "bob@example.com"), (0, 1, 0));
        assert_eq!(tuple(&contacts, "carol@example.com"), (0, 1, 0));

        let mut received = HashMap::new();
        observe_message(&mut received, ME, &MailboxRole::Archive, &addresses, None);
        for address in ["alice@example.com", "bob@example.com", "carol@example.com"] {
            assert_eq!(tuple(&received, address), (0, 0, 1), "{address}");
        }
    }

    /// The user's own address is never a contact, in any field.
    #[test]
    fn the_self_address_is_skipped() {
        let mut contacts = HashMap::new();
        observe_message(
            &mut contacts,
            ME,
            &sent(),
            &MessageAddresses {
                from: Some("Me <ME@example.com>"),
                to: Some("me@example.com, bob@example.com"),
                ..Default::default()
            },
            None,
        );
        assert!(!contacts.contains_key(ME));
        assert!(contacts.contains_key("bob@example.com"));
    }

    #[test]
    fn noreply_is_skipped() {
        let mut contacts = HashMap::new();
        let addresses = MessageAddresses {
            from: Some("no-reply@example.com"),
            ..Default::default()
        };
        observe_message(&mut contacts, ME, &MailboxRole::Inbox, &addresses, None);
        assert!(contacts.is_empty());
    }

    #[test]
    fn counts_accumulate_across_messages() {
        let mut contacts = HashMap::new();
        for _ in 0..3 {
            observe_message(&mut contacts, ME, &sent(), &to("alice@example.com"), None);
        }
        assert_eq!(contacts["alice@example.com"].sent_to, 3);
    }

    #[test]
    fn a_newer_sighting_renames_the_contact() {
        let mut contacts = HashMap::new();
        for (name, at) in [
            ("Old Name", "2025-01-01T00:00:00Z"),
            ("New Name", "2026-04-08T00:00:00Z"),
        ] {
            let header = format!("{name} <alice@example.com>");
            observe_message(&mut contacts, ME, &sent(), &to(&header), Some(at));
        }
        let c = &contacts["alice@example.com"];
        assert_eq!(c.display_name, "New Name");
        assert_eq!(c.sent_to, 2);
    }

    #[test]
    fn a_multi_recipient_header_yields_every_recipient() {
        let mut contacts = HashMap::new();
        let header = "\"A User\" <a@x.com>, B User <b@x.com>, c@x.com";
        observe_message(&mut contacts, ME, &sent(), &to(header), None);
        assert_eq!(contacts.len(), 3);
        assert_eq!(contacts["a@x.com"].display_name, "A User");
        assert_eq!(contacts["b@x.com"].display_name, "B User");
    }

    /// #0067: an absent or unparseable date is the constant, never "now", so
    /// undated mail sinks in its tier and two folds of it agree.
    #[test]
    fn an_undated_message_is_observed_at_the_epoch() {
        for date in [None, Some(""), Some("not a date")] {
            let mut contacts = HashMap::new();
            observe_message(&mut contacts, ME, &sent(), &to("frank@example.com"), date);
            let frank = &contacts["frank@example.com"];
            assert_eq!(frank.last_seen, UNDATED_OBSERVED_AT, "{date:?}");
            assert_eq!(frank.first_seen, UNDATED_OBSERVED_AT, "{date:?}");
        }
    }

    /// A rebuild's empty index carries the current rule's stamp.
    #[test]
    fn an_empty_index_is_stamped_with_the_current_rule() {
        let account = AccountConfig {
            name: "alice".into(),
            ..Default::default()
        };
        let index = empty_index(&account);
        assert_eq!(index.extractor_version, EXTRACTOR_VERSION);
        assert!(index.is_current());
    }
}
