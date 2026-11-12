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
pub fn plan(store: &Store, account: &AccountConfig, fresh: &[ListedFolder]) -> Result<Plan> {
    let previous = listing::load(store, &account.name)?;
    let gained: Vec<String> = fresh
        .iter()
        .filter(|f| !previous.iter().any(|p| p.name == f.name))
        .map(|f| f.name.clone())
        .collect();
    if gained.is_empty() {
        return Ok(Plan::default());
    }
    let before = Catalogue::resolve(account, &previous);
    let mut lost = Vec::new();
    for gone in previous
        .iter()
        .filter(|p| !p.missing && !fresh.iter().any(|f| f.name == p.name))
    {
        let Some(folder) = before
            .folders
            .iter()
            .find(|f| f.listed_as.as_deref() == Some(gone.name.as_str()))
        else {
            continue;
        };
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
/// UIDVALIDITY.
pub fn confirm(plan: &Plan, seen: &[FolderIdentity]) -> Vec<Rename> {
    let mut renames = Vec::new();
    let mut taken: HashSet<&str> = HashSet::new();
    for gained in seen {
        let Some(uidvalidity) = gained.uidvalidity else {
            continue;
        };
        let Some(lost) = plan
            .lost
            .iter()
            .find(|l| l.uidvalidity == uidvalidity && !taken.contains(l.name.as_str()))
        else {
            continue;
        };
        taken.insert(&lost.name);
        renames.push(Rename {
            from: lost.name.clone(),
            to: gained.name.clone(),
        });
    }
    renames
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
