//! Walks account mailboxes and builds or updates a `ContactIndex`.
//!
//! Reuses the existing walkdir + gray_matter pattern from `parse.rs`.

use crate::config::{resolve_root_dir, AccountConfig};
use crate::contacts::filter::is_usable_address;
use crate::contacts::rank::{update_from_observation, Observation, ObservationField};
use crate::contacts::types::ContactIndex;
use crate::types::InboxFrontmatter;
use anyhow::Result;
use chrono::Utc;
use gray_matter::engine::YAML;
use gray_matter::Matter;
use mailparse::addrparse;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

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

/// Walk the given account's mailboxes and build a full `ContactIndex`.
/// Returns even if some mailboxes are missing — best-effort across directories.
pub fn build_index_for_account(account: &AccountConfig) -> Result<ContactIndex> {
    let root = resolve_root_dir(account).ok_or_else(|| {
        anyhow::anyhow!(
            "account '{}' has no directories.root configured",
            account.name
        )
    })?;

    let mut index = ContactIndex {
        account: account.name.clone(),
        contacts: HashMap::new(),
        built_at: Utc::now().to_rfc3339(),
    };
    let self_addr = account.default_from.to_ascii_lowercase();

    // For each configured mailbox (inbox/archive/sent/extra), walk its files.
    for (role, dir) in account_mailboxes(account, &root) {
        if !dir.exists() {
            continue;
        }
        walk_mailbox_dir(&mut index.contacts, &dir, role, &self_addr);
    }

    Ok(index)
}

fn walk_mailbox_dir(
    contacts: &mut HashMap<String, crate::contacts::types::Contact>,
    dir: &Path,
    role: &'static str,
    self_addr: &str,
) {
    for entry in WalkDir::new(dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() || path.extension().map(|e| e != "md").unwrap_or(true) {
            continue;
        }
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let matter = Matter::<YAML>::new();
        let parsed = matter.parse(&content);
        let Some(data) = parsed.data else { continue };
        let Ok(fm) = data.deserialize::<InboxFrontmatter>() else {
            continue;
        };

        let observed_at = fm
            .date
            .clone()
            .and_then(|d| parse_date_to_rfc3339(&d))
            .unwrap_or_else(|| file_mtime_rfc3339(path));

        // Extract all addresses from from/to/cc fields.
        let fields: [(ObservationField, Option<String>); 3] = [
            (ObservationField::From, Some(fm.from.clone())),
            (ObservationField::To, Some(fm.to.clone())),
            (ObservationField::Cc, fm.cc.clone()),
        ];
        for (field, raw) in fields {
            let Some(raw) = raw else { continue };
            if raw.trim().is_empty() {
                continue;
            }
            process_header(contacts, &raw, field, role, &observed_at, self_addr);
        }
    }
}

fn process_header(
    contacts: &mut HashMap<String, crate::contacts::types::Contact>,
    raw: &str,
    field: ObservationField,
    role: &'static str,
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
                mailbox_role: role,
                field,
                observed_at: observed_at.to_string(),
            };
            update_from_observation(contacts, obs);
        }
    }
}

/// Returns `(role_label, absolute_path)` pairs for every configured mailbox.
fn account_mailboxes(account: &AccountConfig, root: &Path) -> Vec<(&'static str, PathBuf)> {
    let mut out = Vec::new();
    if let Some(m) = &account.mailboxes.inbox {
        out.push(("inbox", join_local(root, &m.local)));
    }
    if let Some(m) = &account.mailboxes.archive {
        out.push(("archive", join_local(root, &m.local)));
    }
    if let Some(m) = &account.mailboxes.sent {
        out.push(("sent", join_local(root, &m.local)));
    }
    if let Some(extras) = &account.mailboxes.extra {
        for m in extras {
            out.push(("extra", join_local(root, &m.local)));
        }
    }
    out
}

fn join_local(root: &Path, local: &str) -> PathBuf {
    let expanded = shellexpand::tilde(local).into_owned();
    let p = PathBuf::from(&expanded);
    if p.is_absolute() {
        p
    } else {
        root.join(&expanded)
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

fn file_mtime_rfc3339(path: &Path) -> String {
    use chrono::DateTime;
    if let Ok(meta) = fs::metadata(path) {
        if let Ok(mtime) = meta.modified() {
            let dt: DateTime<Utc> = mtime.into();
            return dt.to_rfc3339();
        }
    }
    Utc::now().to_rfc3339()
}

/// Convert common email date formats to RFC-3339. Returns `None` if parsing fails.
fn parse_date_to_rfc3339(s: &str) -> Option<String> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.to_rfc3339());
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) {
        return Some(dt.to_rfc3339());
    }
    None
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
        let (role, field): (&'static str, ObservationField) = match kind {
            ObservedIn::SentTo => ("sent", ObservationField::To),
            ObservedIn::SentCc => ("sent", ObservationField::Cc),
            ObservedIn::Inbox => ("inbox", ObservationField::From),
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
                    mailbox_role: role,
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
    use crate::contacts::types::Contact;

    fn empty_index() -> ContactIndex {
        ContactIndex {
            account: "test".into(),
            contacts: HashMap::new(),
            built_at: Utc::now().to_rfc3339(),
        }
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
