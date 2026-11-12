//! Telling an IMAP folder the server renamed from a new one (PERSO-185).
//!
//! IMAP has no folder id, so a renamed folder lists as one folder gone and
//! another come. A listing that both loses and gains a folder is the only one
//! that asks the server more ([`plan`]): each gained folder's UIDVALIDITY and
//! UIDNEXT, and from one carrying a lost folder's UIDVALIDITY, the Message-IDs
//! of that folder's newest stored UIDs ([`crate::imap_client::folder_identities`]).
//! Under one UIDVALIDITY a UID names one message, so the same Message-ID at
//! the same UIDs is the same folder ([`confirm`]).
//!
//! Anything short of that is no rename, and the listing behaves as before:
//! the lost folder stays `missing` with its rows and the gained one syncs from
//! scratch. A server that issues a new UIDVALIDITY on a rename is never
//! followed, since without it the UIDs prove nothing (decision of 2026-10-22).

use std::collections::HashSet;

use anyhow::{Context, Result};

use crate::config::AccountConfig;
use crate::imap_client::FolderIdentity;
use crate::store::Store;

use super::listing::{self, ListedFolder, Rename};
use super::Catalogue;

/// How many of a lost folder's newest stored UIDs are compared.
pub const SAMPLE: usize = 20;

/// A folder the previous listing had and the fresh one lacks, with what the
/// store knows of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lost {
    /// The name the previous listing gave it.
    pub name: String,
    /// The UIDVALIDITY its sync cursor recorded.
    pub uidvalidity: u32,
    /// The highest UID its last fetch saw, when recorded.
    pub last_uid: Option<u32>,
    /// Its newest stored UIDs with their bare Message-IDs, newest first.
    pub sample: Vec<(u32, String)>,
}

/// The folders a listing lost and gained that could be one folder renamed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub lost: Vec<Lost>,
    /// The names of the gained folders, as the server lists them.
    pub gained: Vec<String>,
}

impl Plan {
    /// Whether nothing can pair, so the server need not be asked.
    pub fn is_empty(&self) -> bool {
        self.lost.is_empty() || self.gained.is_empty()
    }

    /// The UIDs to read per UIDVALIDITY, for
    /// [`crate::imap_client::folder_identities`].
    pub fn samples(&self) -> Vec<(u32, Vec<u32>)> {
        let mut samples: Vec<(u32, Vec<u32>)> = Vec::new();
        for lost in &self.lost {
            let uids = lost.sample.iter().map(|(uid, _)| *uid);
            match samples.iter_mut().find(|(v, _)| *v == lost.uidvalidity) {
                Some((_, wanted)) => wanted.extend(uids),
                None => samples.push((lost.uidvalidity, uids.collect())),
            }
        }
        for (_, uids) in &mut samples {
            uids.sort_unstable();
            uids.dedup();
        }
        samples
    }
}

/// What the server can be asked about `fresh` against the store's listing of
/// `account`.
///
/// A gained folder is one the store never listed, selectable, under a key no
/// folder held before. A lost folder is one the previous listing had, whose
/// key no folder of `fresh` takes over and whose cursor recorded a
/// UIDVALIDITY. A role folder keyed by its role (inbox, archive, sent) keeps
/// its key, and with it its rows, whatever the server calls it, so it is
/// never either.
pub fn plan(store: &Store, account: &AccountConfig, fresh: &[ListedFolder]) -> Result<Plan> {
    let previous = listing::load(store, &account.name)?;
    let before = Catalogue::resolve(account, &previous);
    let after = Catalogue::resolve(account, fresh);
    let keys = |catalogue: &Catalogue| -> HashSet<String> {
        catalogue
            .folders
            .iter()
            .filter(|f| f.listed_as.is_some() && !f.missing)
            .map(|f| f.key.clone())
            .collect()
    };
    let (keys_before, keys_after) = (keys(&before), keys(&after));
    let listed_as = |catalogue: &Catalogue, name: &str| -> Option<super::Folder> {
        catalogue
            .folders
            .iter()
            .find(|f| f.listed_as.as_deref() == Some(name))
            .cloned()
    };
    let gained: Vec<String> = fresh
        .iter()
        .filter(|f| !previous.iter().any(|p| p.name == f.name))
        .filter(|f| {
            listed_as(&after, &f.name)
                .is_some_and(|folder| folder.selectable() && !keys_before.contains(&folder.key))
        })
        .map(|f| f.name.clone())
        .collect();
    if gained.is_empty() {
        return Ok(Plan::default());
    }
    let mut lost = Vec::new();
    for gone in previous
        .iter()
        .filter(|p| !p.missing && !fresh.iter().any(|f| f.name == p.name))
    {
        let Some(folder) = listed_as(&before, &gone.name) else {
            continue;
        };
        if keys_after.contains(&folder.key) {
            continue;
        }
        let Some(cursor) = crate::ingest::load_mailbox_cursor(store, &account.name, &folder.key)?
        else {
            continue;
        };
        let Some(uidvalidity) = cursor.uidvalidity.and_then(|v| u32::try_from(v).ok()) else {
            continue;
        };
        lost.push(Lost {
            name: gone.name.clone(),
            uidvalidity,
            last_uid: cursor.last_uid.and_then(|uid| u32::try_from(uid).ok()),
            sample: sample(store, &account.name, &folder.key)?,
        });
    }
    Ok(Plan { lost, gained })
}

/// The newest [`SAMPLE`] UIDs stored under `key`, with their bare
/// Message-IDs.
fn sample(store: &Store, account: &str, key: &str) -> Result<Vec<(u32, String)>> {
    let mut stmt = store.conn().prepare(
        "SELECT uid, message_id FROM messages
         WHERE account = ?1 AND mailbox = ?2 AND uid > 0
         ORDER BY uid DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(rusqlite::params![account, key, SAMPLE as i64], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut sample = Vec::new();
    for row in rows {
        let (uid, id) = row.context("reading a lost folder's messages")?;
        let bare = crate::draft_mirror::folder::bare_message_id(&id);
        if let (Ok(uid), false) = (u32::try_from(uid), bare.is_empty()) {
            sample.push((uid, bare.to_string()));
        }
    }
    Ok(sample)
}

/// The renames `seen` proves: a gained folder and a lost one sharing a
/// UIDVALIDITY no other lost or gained folder has, whose UIDNEXT is above the
/// highest UID the lost one's last fetch saw (a folder never hands out a UID
/// twice), and whose sampled UIDs carry the Message-IDs the store holds at
/// them ([`same_messages`]).
pub fn confirm(plan: &Plan, seen: &[FolderIdentity]) -> Vec<Rename> {
    let mut renames = Vec::new();
    for gained in seen {
        let Some(uidvalidity) = gained.uidvalidity else {
            continue;
        };
        let twins = seen
            .iter()
            .filter(|g| g.uidvalidity == Some(uidvalidity))
            .count();
        let mut lost = plan.lost.iter().filter(|l| l.uidvalidity == uidvalidity);
        let (Some(lost), None, 1) = (lost.next(), lost.next(), twins) else {
            continue;
        };
        if let (Some(top), Some(next)) = (lost.last_uid, gained.uidnext) {
            if next <= top {
                continue;
            }
        }
        if !same_messages(&lost.sample, &gained.message_ids) {
            continue;
        }
        renames.push(Rename {
            from: lost.name.clone(),
            to: gained.name.clone(),
        });
    }
    renames
}

/// Whether the server's Message-IDs at the sampled UIDs are the stored ones:
/// not one differs, and at least half of the sample is still there to
/// compare. A UID the server answers without a Message-ID neither counts for
/// nor against.
fn same_messages(sample: &[(u32, String)], served: &[(u32, String)]) -> bool {
    let mut compared = 0;
    for (uid, stored) in sample {
        if let Some((_, id)) = served.iter().find(|(served, _)| served == uid) {
            if id != stored {
                return false;
            }
            compared += 1;
        }
    }
    compared > 0 && compared * 2 >= sample.len()
}

/// [`confirm`] over what the probe answered; a probe that failed proves
/// nothing, and is logged.
pub fn confirm_probed(
    account: &str,
    plan: &Plan,
    probed: Result<Vec<FolderIdentity>>,
) -> Vec<Rename> {
    match probed {
        Ok(seen) => confirm(plan, &seen),
        Err(e) => {
            log::warn!(
                "[catalogue] {account}: asking the server whether a folder was renamed failed; \
                 the listing is recorded without renames: {e:#}"
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::MailboxCursor;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        (dir, store)
    }

    fn account() -> AccountConfig {
        AccountConfig {
            name: "tum".to_string(),
            ..Default::default()
        }
    }

    fn cursor(store: &Store, key: &str, uidvalidity: i64) {
        crate::ingest::record_mailbox_cursor(
            store,
            "tum",
            key,
            &MailboxCursor {
                uidvalidity: Some(uidvalidity),
                ..Default::default()
            },
        )
        .unwrap();
    }

    /// PERSO-185: only a listing that both loses a folder with a cursor and
    /// gains a folder whose key is new asks the server anything. A role
    /// folder renamed keeps its role key, and with it its rows, so it is no
    /// candidate either.
    #[test]
    fn a_listing_that_loses_or_gains_nothing_asks_the_server_nothing() {
        let (_dir, store) = store();
        let account = account();
        let named = ListedFolder::named;
        let sent = |name: &str| ListedFolder::named(name).with_attributes(&["\\Sent"]);
        listing::replace(
            &store,
            "tum",
            &[
                named("INBOX"),
                sent("Sent"),
                named("Projects"),
                named("Spesen"),
            ],
        )
        .unwrap();
        cursor(&store, "sent", 3);
        cursor(&store, "Projects", 7);

        let unchanged = [
            named("INBOX"),
            sent("Sent"),
            named("Projects"),
            named("Spesen"),
        ];
        assert!(plan(&store, &account, &unchanged).unwrap().is_empty());
        let only_gained = [
            named("INBOX"),
            sent("Sent"),
            named("Projects"),
            named("Spesen"),
            named("New"),
        ];
        assert!(plan(&store, &account, &only_gained).unwrap().is_empty());
        let only_lost = [named("INBOX"), sent("Sent"), named("Spesen")];
        assert!(plan(&store, &account, &only_lost).unwrap().is_empty());
        let role_renamed = [
            named("INBOX"),
            sent("Sent Messages"),
            named("Projects"),
            named("Spesen"),
        ];
        assert!(
            plan(&store, &account, &role_renamed).unwrap().is_empty(),
            "the sent folder keeps its key"
        );
        let never_synced = [
            named("INBOX"),
            sent("Sent"),
            named("Projects"),
            named("Reisen"),
        ];
        assert!(
            plan(&store, &account, &never_synced).unwrap().is_empty(),
            "Spesen has no cursor to compare"
        );

        let renamed = [
            named("INBOX"),
            sent("Sent"),
            named("Projekte"),
            named("Spesen"),
        ];
        let asked = plan(&store, &account, &renamed).unwrap();
        assert_eq!(asked.gained, vec!["Projekte"]);
        assert_eq!(
            asked
                .lost
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Projects"]
        );
    }

    /// PERSO-185: a probe that failed proves no rename, so the listing is
    /// recorded as it was before renames were detected, the lost folder kept
    /// as missing.
    #[test]
    fn a_failed_rename_probe_records_the_listing_as_before() {
        let (_dir, store) = store();
        let plan = Plan {
            lost: vec![Lost {
                name: "Projects".to_string(),
                uidvalidity: 7,
                last_uid: Some(1),
                sample: vec![(1, "one@example.com".to_string())],
            }],
            gained: vec!["Projekte".to_string()],
        };
        let proof = FolderIdentity {
            name: "Projekte".to_string(),
            uidvalidity: Some(7),
            uidnext: Some(2),
            message_ids: vec![(1, "one@example.com".to_string())],
        };
        assert_eq!(
            confirm_probed("tum", &plan, Ok(vec![proof])),
            vec![Rename {
                from: "Projects".to_string(),
                to: "Projekte".to_string()
            }]
        );
        let renamed = confirm_probed("tum", &plan, Err(anyhow::anyhow!("connection reset")));
        assert!(renamed.is_empty());

        listing::replace(&store, "tum", &[ListedFolder::named("Projects")]).unwrap();
        listing::replace_with(&store, "tum", &[ListedFolder::named("Projekte")], &renamed).unwrap();
        let listed = listing::load(&store, "tum").unwrap();
        assert!(listed.iter().any(|f| f.name == "Projects" && f.missing));
        assert!(listed.iter().any(|f| f.name == "Projekte" && !f.missing));
    }
}
