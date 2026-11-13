//! The blocked senders (PERSO-261): one list the daemon keeps for every
//! account, and the moves to Junk it drives.
//!
//! # The list
//!
//! An entry is a full address (`spam@example.com`) or a domain
//! (`@example.com`), stored lowercase and matched case-insensitively against
//! the mailbox addresses of a message's `From`, never an address its display
//! name or a comment carries ([`crate::parse::mailbox_addresses`]); a domain
//! entry matches that exact domain, not its subdomains. What an entry may be,
//! and the refusal of one naming the user's own address or its domain, is
//! [`mp_core::blocklist`], which the clients read too. The list lives in
//! `<data_dir>/blocked-senders.json` ([`list_path`]): it is something the user
//! typed, so it may not live only in a store the daemon drops and rebuilds,
//! and `config.toml` is the user's file, which the daemon never writes.
//!
//! # The moves
//!
//! Blocking a sender moves that sender's mail in each account's Inbox to the
//! account's Junk folder ([`move_existing`]), and after every sync tick the
//! account's runner ([`crate::daemon::runtime::sender_runner`]) moves what
//! arrived from a blocked sender since ([`scan`]). Both go through
//! [`crate::mutations::queue_move`], the path `message.move` takes, so each
//! move commits the row change and the owed server move together and the
//! account's drain runs the server half (an IMAP `MOVE`, a Graph move). An
//! account whose catalogue resolves no Junk folder is left alone and says so.
//! No provider rule is written on the server: the moves happen while the
//! daemon runs.
//!
//! # Once per message
//!
//! The scan judges each row once, by the `messages.sender_judged` mark
//! (`NULL` until judged), which survives a local move and a UID rebind, so a
//! message the user moves back to the Inbox from mp stays there. A message
//! moved back from another client arrives as a new row; the Message-IDs
//! this module moved are remembered too ([`MOVED_MEMORY`] of them), and the
//! scan leaves such a message in the Inbox. The mark and the memory live in
//! the store, and a rebuilt store starts over: the scan arms once the
//! rebuilt Inbox has synced, marking everything already there as judged.
//!
//! # Owed moves
//!
//! A block that comes while the daemon is still opening an account's store
//! cannot move that account's Inbox mail, and the scan never will, since it
//! judges only mail that arrives later. The move is owed to the account
//! instead ([`owe`]), in `<data_dir>/blocked-senders-owed.json` beside the
//! list, written before the block answers, so a daemon that stops before the
//! account turned ready still owes it at its next start. Every start runs
//! what it owes ([`move_owed`]) and settles an entry only once its move ran;
//! an unblock settles the sender's entries in every account (PERSO-109).
//! The read a start makes ([`owed`]) drops the entries of every account the
//! configuration no longer names, which no start would ever settle.
//!
//! Every read-modify-write of the list or the owed moves and every scan or
//! move holds one lock ([`LOCK`]), so a block call and a runner's scan never
//! move one row twice, and an unblock never races an owed move.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};

use mp_core::blocklist::normalize;

use crate::catalogue::{Catalogue, FolderRole};
use crate::config::AccountConfig;
use crate::store::schema::{get_meta, set_meta};
use crate::store::write::MutatedRow;
use crate::store::Store;

/// The file name of the list under the data directory.
pub const LIST_FILE: &str = "blocked-senders.json";

/// The file name of the owed moves, beside the list.
pub const OWED_FILE: &str = "blocked-senders-owed.json";

/// How many moved Message-IDs a store remembers.
pub const MOVED_MEMORY: usize = 1024;

/// `meta` key: present once the scan armed on this store.
const META_ARMED: &str = "sender_scan_armed";

/// `meta` key: the Message-IDs moved to Junk, newest last, as a JSON array.
const META_MOVED: &str = "sender_block_moved";

/// Serialises every list edit, scan and move of the process.
static LOCK: Mutex<()> = Mutex::new(());

fn locked() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `<data_dir>/blocked-senders.json`.
pub fn list_path() -> PathBuf {
    crate::config::mailypoppins_data_dir().join(LIST_FILE)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ListFile {
    #[serde(default)]
    entries: Vec<String>,
}

/// The entries at `path`, sorted; an absent file is an empty list. A file that
/// does not parse is an error rather than an empty list, so a broken file
/// unblocks nobody behind the user's back.
pub fn load(path: &Path) -> Result<Vec<String>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let file: ListFile = serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not a blocked-senders file", path.display()))?;
            let mut entries: Vec<String> = file
                .entries
                .iter()
                .filter_map(|entry| normalize(entry).ok())
                .collect();
            entries.sort();
            entries.dedup();
            Ok(entries)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write the list atomically, private to the user.
fn save(path: &Path, entries: &[String]) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(&ListFile {
        entries: entries.to_vec(),
    })
    .context("serialising the blocked senders")?;
    write_private(path, &bytes)
}

/// Replace `path` with `bytes` atomically, private to the user.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let parent = path
        .parent()
        .context("the blocked-senders file has no parent directory")?;
    crate::config::create_private_dir_all(parent)
        .with_context(|| format!("creating {}", parent.display()))?;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("opening {}", tmp.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Whether `from`, a `From` header as the store holds it, names a sender on
/// `entries`: one of its mailbox addresses does
/// ([`crate::parse::mailbox_addresses`]), so a `From` naming several
/// mailboxes matches when any of them is blocked, each judged by the address
/// in its last `<...>` or, with none, its own. A quoted local part that
/// needs no quotes names the mailbox its unquoted form does
/// (`"john.doe"@x` is `john.doe@x`, [`crate::parse::comparable_address`]).
pub fn matches(entries: &[String], from: &str) -> bool {
    if entries.is_empty() {
        return false;
    }
    crate::parse::mailbox_addresses(from).iter().any(|address| {
        let address = crate::parse::comparable_address(address);
        let Some((_, domain)) = address.rsplit_once('@') else {
            return false;
        };
        entries.iter().any(|entry| match entry.strip_prefix('@') {
            Some(blocked) => domain == blocked,
            None => *entry == address,
        })
    })
}

/// What one list edit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// The entry as the list holds it.
    pub entry: String,
    /// Whether the list changed.
    pub changed: bool,
    /// The whole list after the edit.
    pub entries: Vec<String>,
}

/// Add `entry`, already [`normalize`]d, to the list at `path`.
pub fn block(path: &Path, entry: &str) -> Result<Edit> {
    let _turn = locked();
    let mut entries = load(path)?;
    let changed = !entries.iter().any(|known| known == entry);
    if changed {
        entries.push(entry.to_string());
        entries.sort();
        save(path, &entries)?;
    }
    Ok(Edit {
        entry: entry.to_string(),
        changed,
        entries,
    })
}

/// Remove `entry`, already [`normalize`]d, from the list at `path`, and the
/// moves owed for it from every account ([`owe`]). Nothing moves back.
///
/// The unblock stands once the list is saved: an owed-moves file that does
/// not parse or cannot be written is logged and left as it is, and an owed
/// move it still keeps for `entry` settles unmoved once it parses again,
/// since the list no longer holds the entry ([`move_owed`]).
pub fn unblock(path: &Path, entry: &str) -> Result<Edit> {
    let _turn = locked();
    let mut entries = load(path)?;
    let before = entries.len();
    entries.retain(|known| known != entry);
    let changed = entries.len() != before;
    if changed {
        save(path, &entries)?;
    }
    if let Err(e) = forget_owed(&owed_path(path), entry) {
        warn!("unblocked {entry} but left its owed moves as they are: {e:#}");
    }
    Ok(Edit {
        entry: entry.to_string(),
        changed,
        entries,
    })
}

/// Drop `entry` from every account's owed moves in the file at `path`.
fn forget_owed(path: &Path, entry: &str) -> Result<()> {
    let mut owed = load_owed(path)?;
    let mut settled = false;
    owed.accounts.retain(|_, owed| {
        let before = owed.len();
        owed.retain(|known| known != entry);
        settled |= owed.len() != before;
        !owed.is_empty()
    });
    if settled {
        save_owed(path, &owed)?;
    }
    Ok(())
}

/// The owed moves beside the list at `list`.
fn owed_path(list: &Path) -> PathBuf {
    list.with_file_name(OWED_FILE)
}

/// The owed moves: per account, the entries whose Inbox mail it is owed the
/// move of, in the order they were blocked.
#[derive(Debug, Default, Serialize, Deserialize)]
struct OwedFile {
    #[serde(default)]
    accounts: BTreeMap<String, Vec<String>>,
}

/// The owed moves at `path`; an absent file owes nothing, and one that does
/// not parse is an error, so a broken file settles nothing behind the user's
/// back.
fn load_owed(path: &Path) -> Result<OwedFile> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not an owed-moves file", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(OwedFile::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn save_owed(path: &Path, owed: &OwedFile) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(owed).context("serialising the owed moves")?;
    write_private(path, &bytes)
}

/// Owe `account` the move of `entry`'s mail in its Inbox to Junk, in the file
/// beside the list at `list`: the block of a sender while the daemon is still
/// opening the account's store (PERSO-109). Written when this returns.
pub fn owe(list: &Path, account: &str, entry: &str) -> Result<()> {
    let _turn = locked();
    let path = owed_path(list);
    let mut owed = load_owed(&path)?;
    let entries = owed.accounts.entry(account.to_string()).or_default();
    if entries.iter().any(|known| known == entry) {
        return Ok(());
    }
    entries.push(entry.to_string());
    save_owed(&path, &owed)
}

/// The entries whose Inbox move `account` is owed, beside the list at `list`.
///
/// The read every start makes, so it is also where the moves owed to an
/// account `configured` no longer names are dropped: that account never
/// starts again to run them, and one added back later under the same name is
/// a new account whose Inbox nobody blocked anything in.
pub fn owed(list: &Path, account: &str, configured: &[&str]) -> Result<Vec<String>> {
    let _turn = locked();
    let path = owed_path(list);
    let mut owed = load_owed(&path)?;
    let before = owed.accounts.len();
    owed.accounts
        .retain(|name, _| name == account || configured.contains(&name.as_str()));
    if owed.accounts.len() != before {
        info!(
            "[senders] dropped the moves owed to {} account(s) no longer configured",
            before - owed.accounts.len()
        );
        save_owed(&path, &owed)?;
    }
    Ok(owed.accounts.remove(account).unwrap_or_default())
}

/// Run the Inbox moves `account` is owed ([`owe`]) for the entries the list
/// at `list` still holds, and settle them once the move ran; an owed entry
/// the list no longer holds is settled unmoved. A move that fails settles
/// nothing, so the next start runs it again.
///
/// An account with no Junk folder settles too: the move ran and keeps the
/// mail in the Inbox, as a block of an open account does.
pub fn move_owed(store: &Store, account: &AccountConfig, list: &Path) -> Result<Moved> {
    let _turn = locked();
    let path = owed_path(list);
    let mut owed = load_owed(&path)?;
    let Some(entries) = owed.accounts.get(&account.name) else {
        return Ok(Moved::Rows {
            mailbox: String::new(),
            departed: Vec::new(),
        });
    };
    let blocked = load(list)?;
    let entries: Vec<String> = entries
        .iter()
        .filter(|entry| blocked.contains(entry))
        .cloned()
        .collect();
    let moved = move_matching(store, account, &entries)?;
    owed.accounts.remove(&account.name);
    save_owed(&path, &owed)?;
    Ok(moved)
}

/// The store key of `account`'s Junk folder, or `None` when its catalogue
/// resolves none: the server marks no folder `\Junk`, none carries a junk
/// name, and `[accounts.mailboxes] junk` names none.
pub fn junk_key(catalogue: &Catalogue) -> Option<String> {
    catalogue
        .role(FolderRole::Junk)
        .filter(|folder| !folder.missing)
        .map(|folder| folder.key.clone())
}

/// The store key of the Inbox.
fn inbox_key(catalogue: &Catalogue) -> String {
    catalogue
        .role(FolderRole::Inbox)
        .map(|folder| folder.key.clone())
        .unwrap_or_else(|| "inbox".to_string())
}

/// The sentence an account with no Junk folder is skipped with.
pub fn no_junk_reason(account: &str) -> String {
    format!("{account} has no Junk folder; its Inbox keeps the sender's mail")
}

/// The `skipped` reason of an account whose store the daemon is still
/// opening: its Inbox move is owed and runs once the store is open
/// (PERSO-109).
pub fn opening_reason(account: &str) -> String {
    format!(
        "{account} is still opening its store; its Inbox mail from the sender moves to Junk once it is ready"
    )
}

/// What a move over one account did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moved {
    /// Rows moved to the Junk folder under store key `mailbox`, each as it
    /// stood in the Inbox, which is how its removal is announced (PERSO-286).
    Rows {
        mailbox: String,
        departed: Vec<MutatedRow>,
    },
    /// Rows matched and the account has no Junk folder, so none moved.
    NoJunk { matched: usize },
}

/// Move the Inbox rows of `account` whose sender `entries` names to its Junk
/// folder, which is what `sender.block` does for mail already there.
///
/// Every matching row moves, a message the user once moved back included,
/// since the user just asked for it.
pub fn move_existing(store: &Store, account: &AccountConfig, entries: &[String]) -> Result<Moved> {
    let _turn = locked();
    move_matching(store, account, entries)
}

/// [`move_existing`] for a caller that holds [`LOCK`].
fn move_matching(store: &Store, account: &AccountConfig, entries: &[String]) -> Result<Moved> {
    let catalogue = Catalogue::from_store(store, account);
    let inbox = inbox_key(&catalogue);
    let rows: Vec<(i64, String)> = inbox_rows(store, &account.name, &inbox, false)?
        .into_iter()
        .filter(|row| matches(entries, &row.from))
        .map(|row| (row.id, row.message_id))
        .collect();
    move_rows(store, account, &catalogue, &inbox, &rows)
}

/// Whether the scan has armed on `store`: from then on every row that arrives
/// is judged.
pub fn is_armed(store: &Store) -> Result<bool> {
    Ok(get_meta(store.conn(), META_ARMED)?.is_some())
}

/// What one [`scan`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scanned {
    /// The scan armed on this store and judged nothing.
    pub armed: bool,
    /// Rows judged.
    pub judged: usize,
    /// Rows moved to Junk, each as it stood in the Inbox.
    pub moved: Vec<MutatedRow>,
    /// The store key of the Junk folder they moved to, empty when none did.
    pub junk: String,
    /// Rows from a blocked sender left in the Inbox, because the user moved
    /// them back after an earlier move.
    pub kept: usize,
    /// Rows from a blocked sender left in the Inbox, because the account has
    /// no Junk folder.
    pub no_junk: usize,
}

/// Judge every Inbox row of `account` not judged yet, and move the ones from a
/// sender on the list at `list` to Junk; then mark every row of the store
/// judged.
///
/// The list is read under [`LOCK`], so a block that lands while the scan
/// waits for it is judged against.
///
/// Nothing is judged before the Inbox has synced once: the first scan after
/// that arms, marking everything already in the store as judged, so mail that
/// was there before the scan first ran never moves on its own.
pub fn scan(store: &Store, account: &AccountConfig, list: &Path) -> Result<Scanned> {
    let _turn = locked();
    let entries = load(list)?;
    let name = account.name.as_str();
    let catalogue = Catalogue::from_store(store, account);
    let inbox = inbox_key(&catalogue);
    if get_meta(store.conn(), META_ARMED)?.is_none() {
        if crate::ingest::load_mailbox_cursor(store, name, &inbox)?.is_none() {
            return Ok(Scanned::default());
        }
        store
            .conn()
            .execute(
                "UPDATE messages SET sender_judged = 1 WHERE account = ?1 AND sender_judged IS NULL",
                [name],
            )
            .context("arming the blocked-senders scan")?;
        set_meta(store.conn(), META_ARMED, "1")?;
        info!("[senders] {name}: armed; mail already in the store is never judged");
        return Ok(Scanned {
            armed: true,
            ..Scanned::default()
        });
    }

    let conn = store.conn();
    if entries.is_empty() {
        // Nothing to judge against: every row is judged as it stands.
        conn.execute(
            "UPDATE messages SET sender_judged = 1 WHERE account = ?1 AND sender_judged IS NULL",
            [name],
        )
        .context("marking the rows judged")?;
        return Ok(Scanned::default());
    }
    let rows = inbox_rows(store, name, &inbox, true)?;
    let mut outcome = Scanned {
        judged: rows.len(),
        ..Scanned::default()
    };
    let remembered = moved_memory(store)?;
    let mut owed = Vec::new();
    for row in &rows {
        if !matches(&entries, &row.from) {
            continue;
        }
        if remembered.contains(&row.message_id) {
            info!(
                "[senders] {name}: {} is from a blocked sender and was moved back after an earlier move; left in the Inbox",
                row.message_id
            );
            outcome.kept += 1;
            continue;
        }
        owed.push((row.id, row.message_id.clone()));
    }
    match move_rows(store, account, &catalogue, &inbox, &owed)? {
        Moved::Rows { mailbox, departed } => {
            outcome.moved = departed;
            outcome.junk = mailbox;
        }
        Moved::NoJunk { matched } => outcome.no_junk = matched,
    }

    // Judged: the Inbox rows read above, and every row elsewhere, so a
    // message the user later moves into the Inbox from mp is not judged.
    conn.execute(
        "UPDATE messages SET sender_judged = 1
         WHERE account = ?1 AND sender_judged IS NULL AND mailbox <> ?2",
        [name, inbox.as_str()],
    )
    .context("marking the rows outside the Inbox judged")?;
    let mut mark = conn.prepare("UPDATE messages SET sender_judged = 1 WHERE id = ?1")?;
    for row in &rows {
        mark.execute([row.id])
            .context("marking an Inbox row judged")?;
    }
    Ok(outcome)
}

/// One Inbox row as the scan reads it.
struct InboxRow {
    id: i64,
    message_id: String,
    from: String,
}

/// The Inbox rows of `account`, every one or only those not judged yet.
fn inbox_rows(store: &Store, account: &str, inbox: &str, unjudged: bool) -> Result<Vec<InboxRow>> {
    let sql = if unjudged {
        "SELECT id, message_id, COALESCE(from_, '') FROM messages
         WHERE account = ?1 AND mailbox = ?2 AND sender_judged IS NULL ORDER BY id"
    } else {
        "SELECT id, message_id, COALESCE(from_, '') FROM messages
         WHERE account = ?1 AND mailbox = ?2 ORDER BY id"
    };
    let mut stmt = store.conn().prepare(sql)?;
    let rows = stmt
        .query_map([account, inbox], |row| {
            Ok(InboxRow {
                id: row.get(0)?,
                message_id: row.get(1)?,
                from: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("listing the Inbox rows")?;
    Ok(rows)
}

/// Queue the move of `rows` from the Inbox to Junk through the durable queue,
/// mark them judged, and remember their Message-IDs. The caller holds
/// [`LOCK`].
fn move_rows(
    store: &Store,
    account: &AccountConfig,
    catalogue: &Catalogue,
    inbox: &str,
    rows: &[(i64, String)],
) -> Result<Moved> {
    let name = account.name.as_str();
    if rows.is_empty() {
        return Ok(Moved::Rows {
            mailbox: junk_key(catalogue).unwrap_or_default(),
            departed: Vec::new(),
        });
    }
    let Some(junk) = junk_key(catalogue) else {
        warn!(
            "[senders] {name}: {} message(s) from a blocked sender stay in the Inbox: {}",
            rows.len(),
            no_junk_reason(name)
        );
        return Ok(Moved::NoJunk {
            matched: rows.len(),
        });
    };
    let ids: Vec<i64> = rows.iter().map(|(id, _)| *id).collect();
    let queued = crate::mutations::queue_move(
        store,
        name,
        &ids,
        &junk,
        &catalogue.server_for_key(inbox),
        &catalogue.server_for_key(&junk),
        crate::pending_ops::Runner::Drain,
    );
    let mut memory = moved_memory(store)?;
    let mut mark = store
        .conn()
        .prepare("UPDATE messages SET sender_judged = 1 WHERE id = ?1")?;
    for queued in &queued {
        mark.execute([queued.row_id])
            .context("marking a moved row judged")?;
        if let Some((_, message_id)) = rows.iter().find(|(id, _)| *id == queued.row_id) {
            memory.retain(|known| known != message_id);
            memory.push(message_id.clone());
        }
    }
    if memory.len() > MOVED_MEMORY {
        let excess = memory.len() - MOVED_MEMORY;
        memory.drain(..excess);
    }
    set_meta(
        store.conn(),
        META_MOVED,
        &serde_json::to_string(&memory).context("serialising the moved Message-IDs")?,
    )?;
    info!(
        "[senders] {name}: moved {} message(s) from blocked senders to {junk}",
        queued.len()
    );
    Ok(Moved::Rows {
        mailbox: junk,
        departed: queued.into_iter().filter_map(|q| q.departed).collect(),
    })
}

/// The Message-IDs this store moved to Junk, newest last.
fn moved_memory(store: &Store) -> Result<Vec<String>> {
    Ok(get_meta(store.conn(), META_MOVED)?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::ListedFolder;
    use crate::config::{MailboxMapping, MailboxesConfig};
    use crate::ingest::{ingest_message, record_mailbox_cursor, IngestInput, MailboxCursor};
    use crate::store::BlobStore;

    const ACCOUNT: &str = "alpha";

    fn account() -> AccountConfig {
        AccountConfig {
            name: ACCOUNT.to_string(),
            mailboxes: MailboxesConfig {
                inbox: Some(MailboxMapping {
                    server: "INBOX".to_string(),
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        store: Store,
        blobs: BlobStore,
    }

    impl Fixture {
        /// A store whose last folder listing has an Inbox and, when `junk`,
        /// a folder the server marks `\\Junk`.
        fn new(junk: bool) -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
            let blobs = BlobStore::new(dir.path().join("blobs"));
            let mut listed = vec![ListedFolder::named("INBOX").with_attributes(&["\\Inbox"])];
            if junk {
                listed.push(ListedFolder::named("Junk").with_attributes(&["\\Junk"]));
            }
            crate::catalogue::listing::replace(&store, ACCOUNT, &listed).unwrap();
            Fixture { dir, store, blobs }
        }

        fn ingest(&self, uid: i64, from: &str) -> String {
            let id = format!("m{uid}@x");
            let raw = format!(
                "From: {from}\r\nTo: alpha@example.com\r\nSubject: s{uid}\r\nMessage-ID: <{id}>\r\nDate: Wed, 30 Sep 2026 10:00:00 +0200\r\n\r\nbody\r\n"
            );
            let email = crate::parse::parse_rfc822_to_fetched_email(raw.as_bytes()).unwrap();
            ingest_message(
                &self.store,
                &self.blobs,
                &IngestInput {
                    account: ACCOUNT,
                    mailbox: "inbox",
                    uid,
                    email: &email,
                    raw: Some(raw.as_bytes()),
                },
            )
            .unwrap();
            format!("<{id}>")
        }

        fn synced(&self) {
            record_mailbox_cursor(
                &self.store,
                ACCOUNT,
                "inbox",
                &MailboxCursor {
                    uidvalidity: Some(7),
                    last_uid: Some(1),
                    ..MailboxCursor::default()
                },
            )
            .unwrap();
        }

        /// One [`scan`] against `entries`, written as the list file first.
        fn scan(&self, entries: &[String]) -> Result<Scanned> {
            let list = self.dir.path().join(LIST_FILE);
            save(&list, entries)?;
            scan(&self.store, &account(), &list)
        }

        fn mailbox_of(&self, message_id: &str) -> String {
            self.store
                .conn()
                .query_row(
                    "SELECT mailbox FROM messages WHERE message_id = ?1",
                    [message_id],
                    |row| row.get(0),
                )
                .unwrap()
        }

        fn move_back(&self, message_id: &str) {
            self.store
                .conn()
                .execute(
                    "UPDATE messages SET mailbox = 'inbox' WHERE message_id = ?1",
                    [message_id],
                )
                .unwrap();
        }
    }

    fn list(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|e| e.to_string()).collect()
    }

    /// The Message-IDs a move to Junk took out of the Inbox, each checked to
    /// be named as it stood there.
    fn moved_to_junk(moved: Moved) -> Vec<String> {
        let Moved::Rows { mailbox, departed } = moved else {
            panic!("expected rows moved to Junk, got {moved:?}");
        };
        assert_eq!(mailbox, "Junk");
        departed
            .into_iter()
            .map(|row| {
                assert_eq!(row.mailbox, "inbox", "{row:?}");
                assert!(row.uid > 0, "the Inbox uid, not a placeholder: {row:?}");
                row.message_id
            })
            .collect()
    }

    #[test]
    fn a_from_matches_by_address_or_by_exact_domain_in_any_case() {
        let entries = list(&["spam@x.com", "@bulk.example"]);
        assert!(matches(&entries, "Spammer <SPAM@x.com>"));
        assert!(matches(&entries, "news@Bulk.Example"));
        assert!(!matches(&entries, "ham@x.com"));
        assert!(!matches(&entries, "a@sub.bulk.example"));
        assert!(!matches(&entries, ""));
        assert!(!matches(&entries, "(unknown)"));
    }

    #[test]
    fn a_from_matches_on_its_mailbox_addresses_never_one_in_a_name_or_comment() {
        let entries = list(&["@blocked.example"]);
        for unrelated in [
            // A quoted display name holding a blocked address (IMAP rows).
            r#""Bob <bob@blocked.example>" <alice@ok.example>"#,
            // Graph's unquoted display name holding one.
            "Bob <bob@blocked.example> <alice@ok.example>",
            // A comment holding one.
            "alice@ok.example (Bob <bob@blocked.example>)",
            // An unquoted name before the address, as an older row holds an
            // encoded-word name decoded: no bare token beside a `<...>` is
            // tested.
            "bob@blocked.example <alice@ok.example>",
            "bob@blocked.example ( <alice@ok.example>",
            // A Graph name holding a comma.
            "Doe, Jane <jane@ok.example>",
        ] {
            assert!(!matches(&entries, unrelated), "{unrelated} is Alice's");
        }
        // A display name naming someone else hides no blocked address.
        assert!(matches(
            &entries,
            r#""PayPal <service@paypal.com>" <x@blocked.example>"#
        ));
        assert!(matches(
            &entries,
            "PayPal <service@paypal.com> <x@blocked.example>"
        ));
        // A From naming several bare mailboxes is a blocked sender's when any
        // is.
        assert!(matches(&entries, "alice@ok.example, x@blocked.example"));
        // So is one a comment between two addresses parses as one address
        // holding whitespace: every token holding an `@` is tested.
        assert!(matches(&entries, "x@blocked.example (c) alice@ok.example"));
        assert!(matches(&entries, "alice@ok.example (c) x@blocked.example"));
        assert!(matches(
            &entries,
            "Team: alice@ok.example, x@blocked.example;"
        ));
        assert!(!matches(&entries, "alice@ok.example, carol@ok.example"));
        // So is one with a `<`: each mailbox is judged by its own last
        // `<...>`, or its address when it has none, wherever it stands.
        for blocked in [
            "Alice <alice@ok.example>, Bob <bob@blocked.example>",
            "Bob <bob@blocked.example>, Alice <alice@ok.example>",
            "<x@blocked.example>, Alice <alice@ok.example>",
            "x@blocked.example, Alice <alice@ok.example>",
            "x@blocked.example; Alice <alice@ok.example>",
            r#""A, <a@ok.example>" <x@blocked.example>, alice@ok.example"#,
            // An older row's encoded-word name decoded with its comma: now
            // read as a mailbox of its own, which the backfill rewrites.
            "bob@blocked.example, <alice@ok.example>",
            "bob@blocked.example, Sales <alice@ok.example>",
        ] {
            assert!(
                matches(&entries, blocked),
                "{blocked} names a blocked mailbox"
            );
        }
        // A `<` never closed names no mailbox.
        assert!(!matches(&entries, "Bob <bob@blocked.example"));
        assert!(!matches(
            &entries,
            "Bob <bob@blocked.example, alice@ok.example"
        ));
    }

    /// A quoted local part that needs no quotes is the mailbox its unquoted
    /// form names; a domain entry matches any quoted local part.
    #[test]
    fn a_quoted_local_part_matches_the_address_it_quotes() {
        let exact = list(&["john.doe@example.com"]);
        for from in [
            r#""john.doe"@example.com"#,
            r#""John.Doe"@Example.com"#,
            r#"John <"john.doe"@example.com>"#,
            r#""john.doe"@example.com, c@y.com"#,
            r#"c@y.com, "john.doe"@example.com (John)"#,
            "john.doe@example.com",
        ] {
            assert!(matches(&exact, from), "{from}");
        }
        for from in [
            r#""john doe"@example.com"#,
            r#""john..doe"@example.com"#,
            r#""john.doe" <c@y.com>"#,
            r#""john.doe@example.com" <c@y.com>"#,
        ] {
            assert!(!matches(&exact, from), "{from}");
        }
        let domain = list(&["@example.com"]);
        assert!(matches(&domain, r#""john doe"@example.com"#));
        assert!(matches(&domain, r#""a@b"@example.com"#));
        assert!(!matches(&domain, r#""a@example.com"@y.com"#));
        let comparable = crate::parse::comparable_address;
        assert_eq!(comparable(r#""John.Doe"@X.com"#), "john.doe@x.com");
        assert_eq!(comparable(r#""john doe"@x.com"#), r#""john doe"@x.com"#);
    }

    #[test]
    fn an_encoded_word_name_decoding_to_a_blocked_address_moves_nothing() {
        let fx = Fixture::new(true);
        let entries = list(&["@blocked.example"]);
        let alice = fx.ingest(1, "=?utf-8?q?bob=40blocked.example?= <alice@ok.example>");
        // The decoded name ends on a comma: RFC 5322 reads two mailboxes.
        let comma = fx.ingest(2, "=?utf-8?q?bob=40blocked.example=2C?= <alice@ok.example>");
        let spam = fx.ingest(3, "=?utf-8?q?alice=40ok.example?= <x@blocked.example>");
        let from_of = |message_id: &str| -> String {
            fx.store
                .conn()
                .query_row(
                    "SELECT from_ FROM messages WHERE message_id = ?1",
                    [message_id],
                    |row| row.get(0),
                )
                .unwrap()
        };
        let stored = from_of(&comma);
        assert!(
            stored.contains("bob@blocked.example,"),
            "the store holds the name decoded: {stored}"
        );
        assert!(from_of(&alice).contains("bob@blocked.example"));
        assert_eq!(
            moved_to_junk(move_existing(&fx.store, &account(), &entries).unwrap()),
            std::slice::from_ref(&spam)
        );
        assert_eq!(fx.mailbox_of(&alice), "inbox");
        assert_eq!(fx.mailbox_of(&comma), "inbox");
        assert_eq!(fx.mailbox_of(&spam), "Junk");

        // A reply to it goes to Alice, the mailbox, never to the name.
        let source = crate::draft::SourceMessage {
            from: stored,
            subject: "s2".to_string(),
            ..Default::default()
        };
        let drafts = fx.dir.path().join("drafts");
        let reply = crate::draft::create_reply_draft_from(
            &source,
            false,
            "me@alpha.example",
            Some(&drafts),
            None,
        )
        .unwrap();
        let draft = crate::draft::parse_email_draft(&reply).unwrap();
        assert_eq!(draft.frontmatter.to.as_deref(), Some("alice@ok.example"));
    }

    /// An encoded-word decoding to a `(`, `)`, `"` or `<` that, read as syntax
    /// in the decoded text, would hide the blocked mailbox or name one a
    /// comment carries: ingest stores the name quoted, or the bare addresses
    /// the raw value names when the parser rejects it, so the block, the
    /// reader, Copy sender and a reply all name the blocked mailbox.
    #[test]
    fn an_encoded_word_decoding_to_syntax_hides_no_blocked_sender() {
        let fx = Fixture::new(true);
        let entries = list(&["@blocked.example"]);
        let cases = [
            (
                "=?utf-8?q?=28?= <x@blocked.example> (=?utf-8?q?=29?=)",
                r#""(" <x@blocked.example>"#,
            ),
            (
                "=?utf-8?q?=28?= <x@blocked.example> (=?utf-8?q?=29_=3Calice=40ok.example=3E_=28?=)",
                r#""(" <x@blocked.example>"#,
            ),
            (
                "=?utf-8?q?=22?= <x@blocked.example> (=?utf-8?q?=22=3Calice=40ok.example=3E?=)",
                r#""\"" <x@blocked.example>"#,
            ),
            (
                "Spam <x@blocked.example>, =?utf-8?q?=3C?=",
                "x@blocked.example",
            ),
            ("x@blocked.example, =?utf-8?q?=3C?=", "x@blocked.example"),
            (
                "x@blocked.example, =?utf-8?q?=3C?= alice@ok.example =?utf-8?q?=3E?=",
                "x@blocked.example, alice@ok.example",
            ),
            (
                "x@blocked.example (c), =?utf-8?q?=3C?= alice@ok.example =?utf-8?q?=3E?=",
                "x@blocked.example, alice@ok.example",
            ),
            ("=?utf-8?q?=28?=<x@blocked.example>, )", "x@blocked.example"),
            (
                "x@blocked.example, =?utf-8?q?=3C?= G:;",
                r#"x@blocked.example, "< G":;"#,
            ),
            (
                "<x@blocked.example>; =?utf-8?q?=3C?= G:;",
                r#"<x@blocked.example>, "< G":;"#,
            ),
            // A From naming several mailboxes: every one is tested, and the
            // first holding a `<...>` is the one a reply goes to.
            (
                "<x@blocked.example>, Alice <alice@ok.example>",
                "<x@blocked.example>, Alice <alice@ok.example>",
            ),
            // Two `<...>` and no comma: the raw value names both, and so does
            // the store, though the decoded text reads as one name.
            (
                "<x@blocked.example> (=?utf-8?q?=22?=) =?utf-8?q?=22?= <alice@ok.example>",
                "x@blocked.example, alice@ok.example",
            ),
        ];
        let drafts = fx.dir.path().join("drafts");
        for (at, (wire, stored)) in cases.iter().enumerate() {
            let message_id = fx.ingest(at as i64 + 1, wire);
            let from: String = fx
                .store
                .conn()
                .query_row(
                    "SELECT from_ FROM messages WHERE message_id = ?1",
                    [&message_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(from, *stored, "{wire}");
            assert!(matches(&entries, &from), "{wire}: {from} is blocked");
            // Copy sender, here and in the desktop's table.
            assert_eq!(
                crate::parse::extract_email_address(&from),
                "x@blocked.example",
                "{wire}"
            );
            let source = crate::draft::SourceMessage {
                from: from.clone(),
                subject: "s".to_string(),
                ..Default::default()
            };
            let reply = crate::draft::create_reply_draft_from(
                &source,
                false,
                "me@alpha.example",
                Some(&drafts),
                None,
            )
            .unwrap();
            let draft = crate::draft::parse_email_draft(&reply).unwrap();
            assert_eq!(
                draft.frontmatter.to.as_deref(),
                Some("x@blocked.example"),
                "{wire}"
            );
        }
        // A bare mailbox before a bracketed one is blocked all the same, and
        // a reply passes over it, as it would an older row's name decoded to
        // an address and a comma.
        let wire = "x@blocked.example, Alice <alice@ok.example>";
        let message_id = fx.ingest(cases.len() as i64 + 1, wire);
        let from: String = fx
            .store
            .conn()
            .query_row(
                "SELECT from_ FROM messages WHERE message_id = ?1",
                [&message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(from, wire);
        assert!(matches(&entries, &from));
        assert_eq!(
            crate::parse::extract_email_address(&from),
            "alice@ok.example"
        );
        assert_eq!(
            moved_to_junk(move_existing(&fx.store, &account(), &entries).unwrap()).len(),
            cases.len() + 1
        );
    }

    #[test]
    fn the_list_round_trips_and_an_edit_reports_whether_it_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LIST_FILE);
        assert!(load(&path).unwrap().is_empty());
        assert!(block(&path, "@x.com").unwrap().changed);
        let again = block(&path, "@x.com").unwrap();
        assert!(!again.changed);
        assert_eq!(again.entries, ["@x.com"]);
        assert!(block(&path, "a@b.com").unwrap().changed);
        assert_eq!(load(&path).unwrap(), ["@x.com", "a@b.com"]);
        assert!(unblock(&path, "@x.com").unwrap().changed);
        assert!(!unblock(&path, "@x.com").unwrap().changed);
        assert_eq!(load(&path).unwrap(), ["a@b.com"]);
    }

    #[test]
    fn nothing_is_judged_before_the_inbox_synced_and_arming_judges_nothing() {
        let fx = Fixture::new(true);
        let entries = list(&["@spam.example"]);
        let old = fx.ingest(1, "a@spam.example");
        assert_eq!(fx.scan(&entries).unwrap(), Scanned::default());
        fx.synced();
        assert!(fx.scan(&entries).unwrap().armed);
        assert_eq!(fx.mailbox_of(&old), "inbox", "mail already there stays");
        assert_eq!(fx.scan(&entries).unwrap().judged, 0);
    }

    #[test]
    fn a_new_row_from_a_blocked_sender_moves_once_and_a_moved_back_one_stays() {
        let fx = Fixture::new(true);
        let entries = list(&["@spam.example"]);
        fx.synced();
        fx.scan(&entries).unwrap();
        let spam = fx.ingest(2, "Spam <x@SPAM.example>");
        let ham = fx.ingest(3, "friend@ham.example");
        let outcome = fx.scan(&entries).unwrap();
        assert_eq!((outcome.judged, outcome.moved.len()), (2, 1));
        assert_eq!(outcome.junk, "Junk");
        assert_eq!(
            (outcome.moved[0].mailbox.as_str(), outcome.moved[0].uid),
            ("inbox", 2),
            "a moved row is named as it stood in the Inbox"
        );
        assert_eq!(fx.mailbox_of(&spam), "Junk");
        assert_eq!(fx.mailbox_of(&ham), "inbox");

        // Moved back in place (mp's own move keeps the row): judged already.
        fx.move_back(&spam);
        assert_eq!(fx.scan(&entries).unwrap().judged, 0);
        assert_eq!(fx.mailbox_of(&spam), "inbox");

        // Moved back from another client: a new row, kept by the memory.
        fx.store
            .conn()
            .execute(
                "UPDATE messages SET sender_judged = NULL, uid = 9 WHERE message_id = ?1",
                [&spam],
            )
            .unwrap();
        let outcome = fx.scan(&entries).unwrap();
        assert_eq!((outcome.moved.len(), outcome.kept), (0, 1));
        assert_eq!(fx.mailbox_of(&spam), "inbox");
    }

    #[test]
    fn an_account_without_junk_keeps_the_mail_and_says_so() {
        let fx = Fixture::new(false);
        let entries = list(&["x@spam.example"]);
        fx.synced();
        fx.scan(&entries).unwrap();
        let spam = fx.ingest(2, "x@spam.example");
        let outcome = fx.scan(&entries).unwrap();
        assert_eq!((outcome.moved.len(), outcome.no_junk), (0, 1));
        assert_eq!(fx.mailbox_of(&spam), "inbox");
        assert_eq!(
            move_existing(&fx.store, &account(), &entries).unwrap(),
            Moved::NoJunk { matched: 1 }
        );
    }

    /// The two accounts the owed-move tests configure.
    const BOTH: &[&str] = &[ACCOUNT, "beta"];

    /// A move owed to an account is kept in the file beside the list until
    /// it ran: a failed one stays for the next start, a successful one is
    /// settled, and an unblock settles the sender's.
    #[test]
    fn an_owed_move_is_kept_until_it_ran_and_an_unblock_cancels_it() {
        let fx = Fixture::new(true);
        let list = fx.dir.path().join(LIST_FILE);
        let spam = fx.ingest(1, "x@spam.example");
        for entry in ["x@spam.example", "@other.example"] {
            block(&list, entry).unwrap();
            owe(&list, ACCOUNT, entry).unwrap();
        }
        owe(&list, "beta", "x@spam.example").unwrap();
        assert_eq!(
            owed(&list, ACCOUNT, BOTH).unwrap(),
            ["x@spam.example", "@other.example"]
        );

        // An unblock settles the sender's debt in every account.
        unblock(&list, "@other.example").unwrap();
        assert_eq!(owed(&list, ACCOUNT, BOTH).unwrap(), ["x@spam.example"]);

        // A move that fails keeps its debt.
        fx.store
            .conn()
            .execute_batch("ALTER TABLE messages RENAME TO messages_away")
            .unwrap();
        assert!(move_owed(&fx.store, &account(), &list).is_err());
        assert_eq!(owed(&list, ACCOUNT, BOTH).unwrap(), ["x@spam.example"]);

        // The next one moves the mail and settles it, and only this account's.
        fx.store
            .conn()
            .execute_batch("ALTER TABLE messages_away RENAME TO messages")
            .unwrap();
        assert_eq!(
            moved_to_junk(move_owed(&fx.store, &account(), &list).unwrap()),
            std::slice::from_ref(&spam)
        );
        assert_eq!(fx.mailbox_of(&spam), "Junk");
        assert!(owed(&list, ACCOUNT, BOTH).unwrap().is_empty());
        assert_eq!(owed(&list, "beta", BOTH).unwrap(), ["x@spam.example"]);
    }

    /// Reading the owed moves drops those of an account no longer
    /// configured, keeps the reader's own and every configured account's, and
    /// leaves a file with nothing to drop untouched.
    #[test]
    fn reading_the_owed_moves_drops_an_unconfigured_accounts() {
        let fx = Fixture::new(true);
        let list = fx.dir.path().join(LIST_FILE);
        let owed_file = fx.dir.path().join(OWED_FILE);
        for account in [ACCOUNT, "beta", "gone"] {
            owe(&list, account, "x@spam.example").unwrap();
        }

        assert_eq!(owed(&list, ACCOUNT, BOTH).unwrap(), ["x@spam.example"]);
        let kept = load_owed(&owed_file).unwrap();
        assert_eq!(
            kept.accounts.keys().collect::<Vec<_>>(),
            [ACCOUNT, "beta"],
            "the account no longer configured is dropped from the file"
        );

        // The reader's own entries are never dropped, configured or not.
        assert_eq!(owed(&list, "beta", &[]).unwrap(), ["x@spam.example"]);
        assert_eq!(
            load_owed(&owed_file)
                .unwrap()
                .accounts
                .keys()
                .collect::<Vec<_>>(),
            ["beta"]
        );
    }

    #[test]
    fn blocking_moves_every_matching_inbox_row_judged_or_not() {
        let fx = Fixture::new(true);
        let a = fx.ingest(1, "a@spam.example");
        let b = fx.ingest(2, "B <b@spam.example>");
        let c = fx.ingest(3, "c@ham.example");
        let moved = move_existing(&fx.store, &account(), &list(&["@spam.example"])).unwrap();
        assert_eq!(moved_to_junk(moved), [a.clone(), b.clone()]);
        assert_eq!(
            [fx.mailbox_of(&a), fx.mailbox_of(&b), fx.mailbox_of(&c)],
            ["Junk", "Junk", "inbox"]
        );
        let (queued, _) = crate::pending_ops::counts(&fx.store, ACCOUNT).unwrap();
        assert_eq!(queued, 2, "the server half is owed through the queue");
    }
}
