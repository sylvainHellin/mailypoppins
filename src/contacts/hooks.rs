//! Best-effort hooks that keep the contacts index fresh after send/sync.
//!
//! Both hooks are no-ops when the account has no `.contacts-cache.json` yet —
//! the user must run `email contacts rebuild` once to create the baseline.
//! All errors are logged at warning level but never propagated, so a broken
//! contacts index never fails a send or sync.

use crate::config::{resolve_root_dir, AccountConfig};
use crate::contacts::{load_cache, observe, save_cache, ObservedIn};
use crate::imap_client::FreshObservation;
use crate::types::EmailDraft;
use chrono::{DateTime, Utc};
use log::{debug, warn};

/// Merge the recipients of a just-sent draft into the account's contact index.
pub fn bump_after_send(account: &AccountConfig, draft: &EmailDraft) {
    let Some(root) = resolve_root_dir(account) else {
        return;
    };
    let mut index = match load_cache(&root) {
        Ok(Some(idx)) => idx,
        Ok(None) => {
            debug!(
                "contacts hook: no cache for account '{}', skipping send bump",
                account.name
            );
            return;
        }
        Err(e) => {
            warn!(
                "contacts hook: failed to load cache for '{}': {}",
                account.name, e
            );
            return;
        }
    };

    let now = Utc::now().to_rfc3339();
    let mut obs: Vec<(ObservedIn, &str)> = Vec::new();
    if let Some(ref to) = draft.frontmatter.to {
        if !to.trim().is_empty() {
            obs.push((ObservedIn::SentTo, to.as_str()));
        }
    }
    if let Some(ref cc) = draft.frontmatter.cc {
        if !cc.trim().is_empty() {
            obs.push((ObservedIn::SentCc, cc.as_str()));
        }
    }
    if let Some(ref bcc) = draft.frontmatter.bcc {
        if !bcc.trim().is_empty() {
            obs.push((ObservedIn::SentCc, bcc.as_str()));
        }
    }
    if obs.is_empty() {
        return;
    }

    if let Err(e) = observe(&mut index, &account.default_from, &obs, &now) {
        warn!(
            "contacts hook: observe failed for '{}': {}",
            account.name, e
        );
        return;
    }
    if let Err(e) = save_cache(&root, &index) {
        warn!(
            "contacts hook: failed to save cache for '{}': {}",
            account.name, e
        );
    }
}

/// Merge freshly-fetched email headers into the account's contact index.
/// Called after a successful `sync_mailboxes`.
pub fn bump_after_sync(account: &AccountConfig, observations: &[FreshObservation]) {
    if observations.is_empty() {
        return;
    }
    let Some(root) = resolve_root_dir(account) else {
        return;
    };
    let mut index = match load_cache(&root) {
        Ok(Some(idx)) => idx,
        Ok(None) => {
            debug!(
                "contacts hook: no cache for account '{}', skipping sync bump",
                account.name
            );
            return;
        }
        Err(e) => {
            warn!(
                "contacts hook: failed to load cache for '{}': {}",
                account.name, e
            );
            return;
        }
    };

    let mut merged = 0usize;
    for obs in observations {
        let observed_at = rfc2822_to_rfc3339(&obs.date).unwrap_or_else(|| Utc::now().to_rfc3339());
        let mut batch: Vec<(ObservedIn, &str)> = Vec::new();

        // Which ObservedIn kind to use depends on the mailbox role.
        // For "sent", the envelope `to:` is a true sent-to; cc is sent-cc.
        // For inbox/archive/extra, every address (from/to/cc) counts as received.
        let role_lc = obs.role.to_ascii_lowercase();
        if role_lc == "sent" {
            if !obs.to.trim().is_empty() {
                batch.push((ObservedIn::SentTo, obs.to.as_str()));
            }
            if let Some(ref cc) = obs.cc {
                if !cc.trim().is_empty() {
                    batch.push((ObservedIn::SentCc, cc.as_str()));
                }
            }
        } else {
            // inbox / archive / extra → all fields fold into "received".
            if !obs.from.trim().is_empty() {
                batch.push((ObservedIn::Inbox, obs.from.as_str()));
            }
            if !obs.to.trim().is_empty() {
                batch.push((ObservedIn::Inbox, obs.to.as_str()));
            }
            if let Some(ref cc) = obs.cc {
                if !cc.trim().is_empty() {
                    batch.push((ObservedIn::Inbox, cc.as_str()));
                }
            }
        }
        if batch.is_empty() {
            continue;
        }
        if let Err(e) = observe(&mut index, &account.default_from, &batch, &observed_at) {
            warn!(
                "contacts hook: observe failed for '{}': {}",
                account.name, e
            );
            continue;
        }
        merged += 1;
    }
    if merged == 0 {
        return;
    }
    if let Err(e) = save_cache(&root, &index) {
        warn!(
            "contacts hook: failed to save cache for '{}': {}",
            account.name, e
        );
    } else {
        debug!(
            "contacts hook: merged {} observations into '{}' index",
            merged, account.name
        );
    }
}

fn rfc2822_to_rfc3339(s: &str) -> Option<String> {
    if s.trim().is_empty() {
        return None;
    }
    DateTime::parse_from_rfc2822(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DirectorySettings, MailboxMapping, MailboxesConfig};
    use crate::contacts::{build_index_for_account, cache_path};
    use crate::types::{EmailFrontmatter, EmailStatus};
    use std::path::PathBuf;

    fn mk_account(root: &std::path::Path) -> AccountConfig {
        AccountConfig {
            name: "testaccount".into(),
            default_from: "me@example.com".into(),
            directories: DirectorySettings {
                root: Some(root.to_string_lossy().into_owned()),
                drafts: None,
            },
            mailboxes: MailboxesConfig {
                inbox: Some(MailboxMapping {
                    server: "INBOX".into(),
                    local: "inbox".into(),
                }),
                sent: Some(MailboxMapping {
                    server: "Sent".into(),
                    local: "sent".into(),
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn mk_draft(to: &str, cc: Option<&str>) -> EmailDraft {
        let to_opt = if to.is_empty() { None } else { Some(to.to_string()) };
        EmailDraft {
            path: PathBuf::from("/tmp/fake.md"),
            frontmatter: EmailFrontmatter {
                to: to_opt,
                cc: cc.map(|s| s.into()),
                bcc: None,
                subject: "Test".into(),
                status: EmailStatus::Draft,
                from: Some("me@example.com".into()),
                reply_to: None,
                attachments: None,
                sent_at: None,
                sent_via: None,
                message_id: None,
            },
            body_markdown: String::new(),
        }
    }

    #[test]
    fn bump_after_send_is_noop_without_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let account = mk_account(tmp.path());
        let draft = mk_draft("alice@example.com", None);
        // No cache file exists yet.
        bump_after_send(&account, &draft);
        // No cache should have been created.
        assert!(!cache_path(tmp.path()).exists());
    }

    #[test]
    fn bump_after_send_bumps_sent_to_counter() {
        let tmp = tempfile::tempdir().unwrap();
        let account = mk_account(tmp.path());

        // Seed an empty cache via build_index_for_account (empty mailboxes).
        let index = build_index_for_account(&account).unwrap();
        save_cache(tmp.path(), &index).unwrap();
        assert!(cache_path(tmp.path()).exists());

        let draft = mk_draft("Alice <alice@example.com>", Some("bob@example.com"));
        bump_after_send(&account, &draft);

        let loaded = load_cache(tmp.path()).unwrap().unwrap();
        let alice = loaded.contacts.get("alice@example.com").expect("alice");
        assert_eq!(alice.sent_to, 1);
        assert_eq!(alice.display_name, "Alice");
        let bob = loaded.contacts.get("bob@example.com").expect("bob");
        assert_eq!(bob.sent_cc, 1);
    }

    #[test]
    fn bump_after_sync_merges_fresh_observations() {
        let tmp = tempfile::tempdir().unwrap();
        let account = mk_account(tmp.path());
        let index = build_index_for_account(&account).unwrap();
        save_cache(tmp.path(), &index).unwrap();

        let observations = vec![
            FreshObservation {
                role: "inbox".into(),
                from: "Carol <carol@example.com>".into(),
                to: "me@example.com".into(),
                cc: None,
                date: "Mon, 01 Jan 2026 12:00:00 +0000".into(),
            },
            FreshObservation {
                role: "sent".into(),
                from: "me@example.com".into(),
                to: "Dave <dave@example.com>".into(),
                cc: Some("Erin <erin@example.com>".into()),
                date: "Tue, 02 Jan 2026 12:00:00 +0000".into(),
            },
        ];
        bump_after_sync(&account, &observations);

        let loaded = load_cache(tmp.path()).unwrap().unwrap();
        assert_eq!(
            loaded.contacts.get("carol@example.com").unwrap().received,
            1
        );
        assert_eq!(loaded.contacts.get("dave@example.com").unwrap().sent_to, 1);
        assert_eq!(loaded.contacts.get("erin@example.com").unwrap().sent_cc, 1);
        // Self-address must be filtered.
        assert!(!loaded.contacts.contains_key("me@example.com"));
    }
}
