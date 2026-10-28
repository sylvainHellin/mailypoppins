//! Departures: the local moves and deletes a sync pass's listing may not have
//! seen land on the server (PERSO-240).
//!
//! A move or a delete is two halves: the local one, committed with its
//! `pending_ops` row, and the server one, which runs after it. Between the two
//! the source folder on the server still holds the message under the UID the
//! row has left. A sync pass whose listing of that folder is taken inside the
//! window sees the UID, finds no row for it (the row is already elsewhere, or
//! gone), and ingest would write a second row for a copy the server is about
//! to drop: in the case that found this, two back-to-back `mp move` legs, a
//! stale inbox row next to the moved one, which made the next move of the
//! message ambiguous until a later sync pruned it.
//!
//! The listing is taken in the transport phase, after the pass has read the
//! store and before it ingests, so what ingest has to know is whether a
//! departure's server half could still have been owed when the listing was
//! taken. Two kinds can:
//!
//! - one that was owed when the pass started: its `pending_ops` row was
//!   `queued` or `settling` then ([`Watch::open`] reads them), whether or not
//!   it has retired since;
//! - one committed after the pass started: [`record`] journals it for every
//!   open [`Watch`], because its row may well be retired (deleted) by the time
//!   ingest asks, and the store then holds no trace of where the message was.
//!
//! Anything else retired before the pass started, so the listing cannot show
//! its source copy.
//!
//! A departure names the copy it takes when its row knew its server UID
//! ([`ServerCopy`], PERSO-238), and then only that UID departed: another copy
//! under the same `Message-ID` stays on the server and is ingested as ever.
//! When the listing no longer holds that UID, because another client moved
//! the message out and back since the row was synced, the op's server half
//! takes the folder's copy no twin row stands for
//! (`imap_client::ops::resolve_copy`), and so the watch takes that copy for
//! the departing one too, from the same listing (PERSO-246). The question is
//! answered from what the pass listed rather than from what the server half
//! acted on, because the server half may run after the ingest that asks.
//! One whose row had no UID yet (a row still on the `-id` sentinel of an
//! earlier move, which is the back-to-back case) names none, and any copy of
//! its `Message-ID` in the folder is taken as the departing one, so a genuine
//! second copy listed in that window waits one pass. The journal lives in this process only, which is all it
//! has to: only the daemon commits departures and only the daemon syncs, and a
//! watch never outlives the pass that opened it. It records nothing while no
//! watch is open over the store and is dropped when the last one closes, so it
//! never grows past the departures of one pass.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use anyhow::Result;

use crate::ops::{ServerCopy, ServerOp};
use crate::store::Store;

/// What one move or delete takes out of a folder.
#[derive(Debug, Clone)]
struct Leaving {
    /// The server folder the message left.
    source: String,
    message_id: String,
    /// The copy the op acts on, when its row knew its UID.
    copy: Option<ServerCopy>,
}

impl Leaving {
    /// Whether this departure may be what took the copy `listed` names,
    /// carrying `message_id`.
    ///
    /// A named copy is the departing one under its own UID, and under any UID
    /// when the listing is under another UIDVALIDITY (a reset since the row was
    /// synced), where its UID means nothing and the `Message-ID` decides, as it
    /// does for an op that names no copy. See [`departs`] for a UID the folder
    /// no longer lists.
    fn covers(&self, message_id: &str, listed: &Listed<'_>) -> bool {
        self.message_id == message_id
            && same_folder(&self.source, listed.source)
            && self.copy.as_ref().is_none_or(|copy| departs(copy, listed))
    }
}

/// Whether an op naming `copy` takes the copy `listed` names out of its
/// folder.
///
/// Its own UID, and any UID under another UIDVALIDITY, as [`Leaving::covers`]
/// says. When the folder no longer lists the op's UID (PERSO-246), the
/// server half finds the message by `Message-ID` and acts on the folder's
/// copy when it is the only one (`imap_client::ops::resolve_copy`): the
/// copy listed here, unless a copy a twin row stands for
/// ([`ServerCopy::siblings`]) is listed beside it, which makes the folder hold
/// two and the server half act on neither. That twin's own copy is therefore
/// never taken for the departing one. Two copies no row stands for cannot be
/// told apart from a listing either, so both wait while the op is owed, as a
/// copy of an op that names none does; so does a new copy listed after the
/// op's own already left, which the next pass ingests.
///
/// A listing that cannot say what the folder holds (a short enumeration)
/// leaves the op's UID as the only one that departs.
fn departs(copy: &ServerCopy, listed: &Listed<'_>) -> bool {
    if copy.uid == listed.uid
        || matches!((copy.uidvalidity, listed.uidvalidity), (Some(a), Some(b)) if a != b)
    {
        return true;
    }
    listed.folder.is_some_and(|folder| {
        !folder.contains(&copy.uid) && !copy.siblings.iter().any(|uid| folder.contains(uid))
    })
}

/// One copy a sync pass listed, as [`Watch::departed`] asks about it.
#[derive(Debug, Clone, Copy)]
pub struct Listed<'a> {
    /// The server folder the copy was listed in.
    pub source: &'a str,
    /// The copy's UID there.
    pub uid: u32,
    /// The folder's UIDVALIDITY on this pass.
    pub uidvalidity: Option<u32>,
    /// Every UID the pass listed in the folder, when the enumeration covered
    /// the whole folder under the UIDVALIDITY the store knows; `None` when it
    /// cannot say what the folder holds.
    pub folder: Option<&'a HashSet<u32>>,
}

/// One local departure, as the journal keeps it.
#[derive(Debug, Clone)]
struct Departure {
    /// Position in the journal: a watch sees the entries at or after its own.
    seq: u64,
    account: String,
    leaving: Leaving,
}

/// The journal of one store, alive while at least one watch is open over it.
#[derive(Debug, Default)]
struct Journal {
    /// The first entry each open watch sees, one per watch.
    open: Vec<u64>,
    next: u64,
    entries: Vec<Departure>,
}

/// The journals, keyed by store path for the reason
/// [`crate::pending_ops::SettleScope`] keys its registry that way: two data
/// roots, or two tests, may each hold an account of the same name.
static JOURNALS: LazyLock<Mutex<HashMap<PathBuf, Journal>>> = LazyLock::new(Default::default);

fn journals() -> MutexGuard<'static, HashMap<PathBuf, Journal>> {
    JOURNALS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What an op takes out of a folder, when it is a move or a delete.
fn departure_of(op: &ServerOp) -> Option<Leaving> {
    match op {
        ServerOp::Move {
            message_id,
            source_mailbox,
            copy,
            ..
        }
        | ServerOp::Delete {
            message_id,
            source_mailbox,
            copy,
        } => Some(Leaving {
            source: source_mailbox.clone(),
            message_id: message_id.clone(),
            copy: copy.clone(),
        }),
        _ => None,
    }
}

/// Whether two server folder names name the same folder: `INBOX` is
/// case-insensitive in IMAP (RFC 3501 section 5.1), every other name is not.
fn same_folder(a: &str, b: &str) -> bool {
    a == b || (a.eq_ignore_ascii_case("INBOX") && b.eq_ignore_ascii_case("INBOX"))
}

/// Journal `op` for every watch open over `store`, if it takes a message out
/// of a folder.
///
/// Called by [`crate::pending_ops::apply_move`] and
/// [`crate::pending_ops::apply_delete`] inside the transaction that commits the
/// local half, before its commit: a pass whose ingest reads the store after
/// the row has left therefore always finds the entry. A transaction that then
/// fails to commit leaves an entry for a departure that never happened, which
/// costs one pass's deferral of a copy the store does not hold and nothing
/// else.
pub fn record(store: &Store, account: &str, op: &ServerOp) {
    let Some(leaving) = departure_of(op) else {
        return;
    };
    let mut journals = journals();
    let Some(journal) = journals.get_mut(store.path()) else {
        return;
    };
    let seq = journal.next;
    journal.next += 1;
    journal.entries.push(Departure {
        seq,
        account: account.to_string(),
        leaving,
    });
}

/// One sync pass's view of the departures its listing may not have seen.
///
/// Open it before the pass reads the store; ask it at ingest.
#[derive(Debug)]
pub struct Watch {
    store: PathBuf,
    account: String,
    /// The first journal entry this watch sees.
    from: u64,
    /// Every move or delete owed when the watch opened.
    owed: Vec<Leaving>,
}

impl Watch {
    /// Open a watch over `account` in `store`.
    ///
    /// The journal is joined before the owed ops are read, so a departure
    /// committed between the two is in the journal, the read, or both, and
    /// never in neither.
    pub fn open(store: &Store, account: &str) -> Result<Watch> {
        let path = store.path().to_path_buf();
        let from = {
            let mut journals = journals();
            let journal = journals.entry(path.clone()).or_default();
            journal.open.push(journal.next);
            journal.next
        };
        // Built before the read so a failed read still closes the watch.
        let mut watch = Watch {
            store: path,
            account: account.to_string(),
            from,
            owed: Vec::new(),
        };
        watch.owed = crate::pending_ops::unsettled_ops(store, account)?
            .into_iter()
            .filter_map(|row| departure_of(&row.op))
            .collect();
        Ok(watch)
    }

    /// Whether the copy this pass listed (`listed`), carrying `message_id`,
    /// may be one a local move or delete is taking out of its folder: one
    /// whose server half may not have landed before the listing was taken.
    pub fn departed(&self, message_id: &str, listed: &Listed<'_>) -> bool {
        let covers = |leaving: &Leaving| leaving.covers(message_id, listed);
        self.owed.iter().any(covers)
            || journals().get(&self.store).is_some_and(|journal| {
                journal
                    .entries
                    .iter()
                    .any(|d| d.seq >= self.from && d.account == self.account && covers(&d.leaving))
            })
    }
}

impl Drop for Watch {
    /// Leave the journal, dropping what no open watch can see any more: the
    /// whole journal once the last watch has closed.
    fn drop(&mut self) {
        let mut journals = journals();
        let Some(journal) = journals.get_mut(&self.store) else {
            return;
        };
        if let Some(at) = journal.open.iter().position(|&from| from == self.from) {
            journal.open.swap_remove(at);
        }
        match journal.open.iter().min().copied() {
            Some(oldest) => journal.entries.retain(|d| d.seq >= oldest),
            None => {
                journals.remove(&self.store);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        (dir, store)
    }

    fn move_op(message_id: &str, from: &str) -> ServerOp {
        ServerOp::Move {
            message_id: message_id.to_string(),
            source_mailbox: from.to_string(),
            dest_mailbox: "Archive".to_string(),
            copy: None,
        }
    }

    impl Watch {
        /// [`Watch::departed`] for a copy listed at UID 1 under UIDVALIDITY 7,
        /// which an op naming no copy covers whatever its UID.
        fn left(&self, source: &str, message_id: &str) -> bool {
            self.departed(message_id, &at(source, 1, Some(7), None))
        }
    }

    /// A copy listed at `uid` in `source`, under `uidvalidity`, by a listing
    /// that holds `folder`.
    fn at<'a>(
        source: &'a str,
        uid: u32,
        uidvalidity: Option<u32>,
        folder: Option<&'a HashSet<u32>>,
    ) -> Listed<'a> {
        Listed {
            source,
            uid,
            uidvalidity,
            folder,
        }
    }

    /// A departure recorded while no pass watches is not kept: it is either
    /// still owed when the next pass opens its watch, which reads it from the
    /// queue, or it retired and no later listing can show its source copy.
    #[test]
    fn a_departure_with_no_watch_open_is_not_journalled() {
        let (_dir, store) = store();
        record(&store, "alice", &move_op("<a@x>", "INBOX"));
        let watch = Watch::open(&store, "alice").unwrap();
        assert!(!watch.left("INBOX", "<a@x>"));
    }

    /// A watch sees what was journalled after it opened, in its own account
    /// and folder only, and a delete counts as a departure as much as a move.
    #[test]
    fn a_watch_sees_the_departures_recorded_after_it_opened() {
        let (_dir, store) = store();
        let watch = Watch::open(&store, "alice").unwrap();
        record(&store, "alice", &move_op("<a@x>", "INBOX"));
        record(
            &store,
            "alice",
            &ServerOp::Delete {
                message_id: "<d@x>".to_string(),
                source_mailbox: "Archive".to_string(),
                copy: None,
            },
        );
        record(&store, "bob", &move_op("<b@x>", "INBOX"));
        assert!(watch.left("INBOX", "<a@x>"));
        assert!(watch.left("inbox", "<a@x>"), "INBOX is case-insensitive");
        assert!(watch.left("Archive", "<d@x>"));
        assert!(!watch.left("Archive", "<a@x>"), "another folder");
        assert!(!watch.left("INBOX", "<b@x>"), "another account");
    }

    /// A flag op takes nothing out of a folder.
    #[test]
    fn a_flag_op_is_no_departure() {
        let (_dir, store) = store();
        let watch = Watch::open(&store, "alice").unwrap();
        record(
            &store,
            "alice",
            &ServerOp::SetRead {
                message_id: "<a@x>".to_string(),
                mailbox: "INBOX".to_string(),
                read: true,
                copy: None,
            },
        );
        assert!(!watch.left("INBOX", "<a@x>"));
    }

    /// A watch opened later does not see what an earlier one does, and the
    /// journal is gone once the last watch closes.
    #[test]
    fn a_later_watch_starts_after_what_was_already_journalled() {
        let (_dir, store) = store();
        let first = Watch::open(&store, "alice").unwrap();
        record(&store, "alice", &move_op("<a@x>", "INBOX"));
        let second = Watch::open(&store, "alice").unwrap();
        assert!(first.left("INBOX", "<a@x>"));
        assert!(!second.left("INBOX", "<a@x>"));
        drop(first);
        drop(second);
        assert!(!journals().contains_key(store.path()));
    }

    /// Closing the oldest watch drops the entries only it could see, so a
    /// store that always has a pass open still keeps a bounded journal.
    #[test]
    fn closing_a_watch_drops_what_no_open_watch_can_see() {
        let (_dir, store) = store();
        let first = Watch::open(&store, "alice").unwrap();
        record(&store, "alice", &move_op("<a@x>", "INBOX"));
        let second = Watch::open(&store, "alice").unwrap();
        record(&store, "alice", &move_op("<b@x>", "INBOX"));
        drop(first);
        let kept: Vec<String> = journals()[store.path()]
            .entries
            .iter()
            .map(|d| d.leaving.message_id.clone())
            .collect();
        assert_eq!(kept, vec!["<b@x>".to_string()]);
        assert!(second.left("INBOX", "<b@x>"));
    }

    /// A move or delete still owed when the watch opens is a departure even
    /// once it has retired: the listing may have been taken before it landed.
    #[test]
    fn a_departure_owed_when_the_watch_opened_stays_one_after_it_retires() {
        let (_dir, store) = store();
        let op = move_op("<a@x>", "INBOX");
        let tx = store.conn().unchecked_transaction().unwrap();
        let op_id = crate::pending_ops::enqueue(
            &tx,
            "alice",
            None,
            &op,
            &crate::pending_ops::Rollback::None,
        )
        .unwrap();
        tx.commit().unwrap();
        let watch = Watch::open(&store, "alice").unwrap();
        store
            .conn()
            .execute("DELETE FROM pending_ops WHERE id = ?1", [op_id])
            .unwrap();
        assert!(watch.left("INBOX", "<a@x>"));
        assert!(!Watch::open(&store, "alice").unwrap().left("INBOX", "<a@x>"));
    }

    /// An op that names the copy it takes (PERSO-238) departs that UID only:
    /// another copy of the message listed beside it stays, and is ingested.
    /// Under another UIDVALIDITY the UID means nothing, and the Message-ID
    /// decides.
    #[test]
    fn a_named_copy_departs_under_its_own_uid_only() {
        let (_dir, store) = store();
        let watch = Watch::open(&store, "alice").unwrap();
        record(
            &store,
            "alice",
            &ServerOp::Move {
                message_id: "<a@x>".to_string(),
                source_mailbox: "INBOX".to_string(),
                dest_mailbox: "Archive".to_string(),
                copy: Some(ServerCopy {
                    uid: 10,
                    uidvalidity: Some(7),
                    siblings: Vec::new(),
                }),
            },
        );
        // The folder still lists UID 10, so UID 11 is a second copy.
        let listed: HashSet<u32> = [10, 11].into();
        let folder = Some(&listed);
        assert!(watch.departed("<a@x>", &at("INBOX", 10, Some(7), folder)));
        assert!(
            !watch.departed("<a@x>", &at("INBOX", 11, Some(7), folder)),
            "a second copy"
        );
        assert!(
            !watch.departed("<a@x>", &at("INBOX", 11, Some(7), None)),
            "a listing that cannot say UID 10 is gone"
        );
        assert!(
            watch.departed("<a@x>", &at("INBOX", 11, Some(8), folder)),
            "a renumbered folder"
        );
        assert!(
            watch.departed("<a@x>", &at("INBOX", 10, None, folder)),
            "no UIDVALIDITY to doubt"
        );
    }

    /// An op whose UID went stale (PERSO-246): the folder no longer lists it,
    /// so the server half takes the folder's one copy no twin row stands for,
    /// and the watch takes that copy for the departing one. A copy a twin row
    /// stands for is not taken, and neither is any copy listed beside one,
    /// since the server half then acts on none.
    #[test]
    fn a_stale_copy_departs_under_the_uid_the_folder_now_gives_it() {
        let (_dir, store) = store();
        let watch = Watch::open(&store, "alice").unwrap();
        record(
            &store,
            "alice",
            &ServerOp::Move {
                message_id: "<a@x>".to_string(),
                source_mailbox: "INBOX".to_string(),
                dest_mailbox: "Archive".to_string(),
                copy: Some(ServerCopy {
                    uid: 10,
                    uidvalidity: Some(7),
                    siblings: vec![30],
                }),
            },
        );
        let renumbered: HashSet<u32> = [25].into();
        assert!(watch.departed("<a@x>", &at("INBOX", 25, Some(7), Some(&renumbered))));
        assert!(
            !watch.departed("<b@x>", &at("INBOX", 25, Some(7), Some(&renumbered))),
            "another message"
        );

        let beside_a_twin: HashSet<u32> = [25, 30].into();
        assert!(
            !watch.departed("<a@x>", &at("INBOX", 30, Some(7), Some(&beside_a_twin))),
            "the twin's own copy"
        );
        assert!(
            !watch.departed("<a@x>", &at("INBOX", 25, Some(7), Some(&beside_a_twin))),
            "a copy the server half cannot tell from the twin's"
        );
    }
}
