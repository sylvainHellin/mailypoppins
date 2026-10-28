//! The entry into the durable mutation queue the message mutations share.
//!
//! A flag, a move, an archive or a delete is one local write and one server op.
//! [`queue_move`], [`queue_delete`], [`queue_read_flag`] and [`queue_flag`]
//! commit the local store change and the owed [`ServerOp`] in one transaction
//! through [`crate::pending_ops`] (#0039), then hand back what they queued so
//! the caller can drop those rows from the list it is showing and settle the
//! ops it owes. The server op is retired later by the background drain, which
//! the daemon runs about 1.5 s after the account's last mutation (#0133) and
//! at every sync tick, and a refusal is rolled back there, so a queueing
//! caller spawns no per-op server thread and keeps no rollback of its own: the
//! queue owns both.
//!
//! # Why it is not in the TUI any more
//!
//! It was `src/tui/mutations.rs` until P5-U6 (#0124): the TUI committed the row
//! change itself and the daemon's `message.archive` had a second copy of the
//! same pairing. Now the five message mutations are daemon methods
//! (`src/daemon/methods/message.rs`) and this is the one place that pairs a row
//! change with the [`ServerOp`] it owes, for a client that queues and for
//! `mp archive`, which queues and then drains.
//!
//! Rows are named by `messages.id`, the store's own key, rather than by the
//! TUI's `MessageRef` wrapper around it: the daemon addresses a row by that id
//! on the wire (`row_id`, P5-U4) and this module is below both clients.

use crate::ops::{ServerCopy, ServerOp};
use crate::pending_ops::{self, Runner};
use crate::store::write;
use crate::store::{BlobStore, Store};

/// One queued mutation: the row it changed and the server op it owes.
///
/// The op id is what a caller that wants the pre-daemon blocking UX, and so
/// queued for [`Runner::Caller`], hands to
/// [`crate::pending_ops::run_and_settle`]; a caller that queues for
/// [`Runner::Drain`] ignores it and lets the next drain retire it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Queued {
    /// The `messages.id` the mutation changed.
    pub row_id: i64,
    /// The `pending_ops` row the change owes the server.
    pub op_id: i64,
}

/// How a queued op names its message, read off the row before the op is built:
/// the Message-ID, and the server copy the row stands for (PERSO-238).
///
/// The durable `apply_*` functions read the row's coordinates themselves inside
/// their commit, but they take the [`ServerOp`] ready-made; this is the one
/// lightweight read that supplies its addressing. The copy is the row's UID
/// with its mailbox's UIDVALIDITY, so the server op acts on this row's copy
/// when the folder holds two under one Message-ID. `None` (with a log line)
/// when the row is already gone, which is a skip rather than an error.
fn addressing_of(
    store: &Store,
    account: &str,
    row_id: i64,
    what: &str,
) -> Option<(String, Option<ServerCopy>)> {
    let row = match write::row_coordinates(store, row_id) {
        Ok(Some(row)) => row,
        Ok(None) => {
            log::warn!("[store] message {row_id} has no row to {what}");
            return None;
        }
        Err(e) => {
            log::warn!("[store] reading message {row_id} to {what} failed: {e:#}");
            return None;
        }
    };
    // An unreadable cursor costs the op its UIDVALIDITY check, not the op: the
    // UID search still carries the Message-ID.
    let uidvalidity = crate::ingest::load_mailbox_cursor(store, account, &row.mailbox)
        .unwrap_or_else(|e| {
            log::warn!(
                "[store] reading the cursor of {account}/{}: {e:#}",
                row.mailbox
            );
            None
        })
        .and_then(|cursor| cursor.uidvalidity);
    let copy = ServerCopy::of_row(row.uid, uidvalidity);
    Some((row.message_id, copy))
}

/// Move rows into `dest_mailbox` (the store's mailbox key) and queue the server
/// moves that carry them to `dest_server`. Returns the rows actually moved, for
/// the list update.
///
/// Rows that are already gone are skipped rather than reported: a message the
/// store no longer holds cannot be moved, and a second mutation racing the
/// first is a user action rather than a bug.
pub fn queue_move(
    store: &Store,
    account: &str,
    rows: &[i64],
    dest_mailbox: &str,
    source_server: &str,
    dest_server: &str,
    runner: Runner<'_>,
) -> Vec<Queued> {
    let mut moved = Vec::with_capacity(rows.len());
    for row_id in rows.iter().copied() {
        let Some((message_id, copy)) = addressing_of(store, account, row_id, "move") else {
            continue;
        };
        let op = ServerOp::Move {
            message_id,
            source_mailbox: source_server.to_string(),
            dest_mailbox: dest_server.to_string(),
            copy,
        };
        match pending_ops::apply_move(store, account, row_id, dest_mailbox, op, runner) {
            Ok(Some((_previous, op_id))) => moved.push(Queued { row_id, op_id }),
            Ok(None) => log::warn!("[store] message {row_id} has no row to move"),
            Err(e) => log::warn!("[store] queuing a move for message {row_id} failed: {e:#}"),
        }
    }
    moved
}

/// Delete rows and queue the server deletes. Returns the rows actually removed.
pub fn queue_delete(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    rows: &[i64],
    source_server: &str,
    runner: Runner<'_>,
) -> Vec<Queued> {
    let mut deleted = Vec::with_capacity(rows.len());
    for row_id in rows.iter().copied() {
        let Some((message_id, copy)) = addressing_of(store, account, row_id, "delete") else {
            continue;
        };
        let op = ServerOp::Delete {
            message_id,
            source_mailbox: source_server.to_string(),
            copy,
        };
        match pending_ops::apply_delete(store, blobs, account, row_id, op, runner) {
            Ok(Some((_previous, op_id))) => deleted.push(Queued { row_id, op_id }),
            Ok(None) => log::warn!("[store] message {row_id} has no row to delete"),
            Err(e) => log::warn!("[store] queuing a delete for message {row_id} failed: {e:#}"),
        }
    }
    deleted
}

/// Set the read flag on rows and queue the server ops that mirror it. Returns
/// the rows actually flagged.
pub fn queue_read_flag(
    store: &Store,
    account: &str,
    rows: &[i64],
    read: bool,
    server_mailbox: &str,
    runner: Runner<'_>,
) -> Vec<Queued> {
    let mut out = Vec::with_capacity(rows.len());
    for row_id in rows.iter().copied() {
        let Some((message_id, copy)) = addressing_of(store, account, row_id, "flag") else {
            continue;
        };
        let op = ServerOp::SetRead {
            message_id,
            mailbox: server_mailbox.to_string(),
            read,
            copy,
        };
        match pending_ops::apply_set_read(store, account, row_id, read, op, runner) {
            Ok(Some(op_id)) => out.push(Queued { row_id, op_id }),
            Ok(None) => log::warn!("[store] message {row_id} has no row to flag"),
            Err(e) => log::warn!("[store] queuing a read flag for message {row_id} failed: {e:#}"),
        }
    }
    out
}

/// Set the `\Flagged` star on rows and queue the server ops that mirror it
/// (#0007). Returns the rows actually starred.
pub fn queue_flag(
    store: &Store,
    account: &str,
    rows: &[i64],
    flagged: bool,
    server_mailbox: &str,
    runner: Runner<'_>,
) -> Vec<Queued> {
    let mut out = Vec::with_capacity(rows.len());
    for row_id in rows.iter().copied() {
        let Some((message_id, copy)) = addressing_of(store, account, row_id, "flag") else {
            continue;
        };
        let op = ServerOp::SetFlagged {
            message_id,
            mailbox: server_mailbox.to_string(),
            flagged,
            copy,
        };
        match pending_ops::apply_set_flagged(store, account, row_id, flagged, op, runner) {
            Ok(Some(op_id)) => out.push(Queued { row_id, op_id }),
            Ok(None) => log::warn!("[store] message {row_id} has no row to flag"),
            Err(e) => log::warn!("[store] queuing a flag for message {row_id} failed: {e:#}"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agenda;
    use crate::reconcile::tests::{fixture, invite_ics, Fixture};
    use crate::store::read;

    fn refs(ids: &[i64]) -> Vec<i64> {
        ids.to_vec()
    }

    /// The rows a batch of [`Queued`] changed, which is what the assertions
    /// about "what went" are written against.
    fn rows(queued: &[Queued]) -> Vec<i64> {
        queued.iter().map(|q| q.row_id).collect()
    }

    fn mailbox_of(fx: &Fixture, id: i64) -> Option<String> {
        read::find_by_id(&fx.store, id).unwrap().map(|r| r.mailbox)
    }

    /// The one queued op of an account, when a test expects exactly one.
    fn only_queued_op(fx: &Fixture) -> ServerOp {
        let queued = pending_ops::queued_ops(&fx.store, "alice").unwrap();
        assert_eq!(queued.len(), 1, "expected exactly one queued op");
        queued[0].op.clone()
    }

    /// Archive is a move with a fixed destination: the row lands in the archive
    /// mailbox immediately, and the queued op names the message by Message-ID
    /// with the two *server* folders, not the store's mailbox keys.
    #[test]
    fn archiving_moves_the_row_and_queues_a_server_move() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");

        let moved = queue_move(
            &fx.store,
            "alice",
            &refs(&[id]),
            "archive",
            "INBOX",
            "Archive",
            Runner::Drain,
        );

        assert_eq!(mailbox_of(&fx, id).as_deref(), Some("archive"));
        assert_eq!(rows(&moved), vec![id]);
        assert_eq!(
            only_queued_op(&fx),
            ServerOp::Move {
                message_id: "<inbox-1@example.com>".to_string(),
                source_mailbox: "INBOX".to_string(),
                dest_mailbox: "Archive".to_string(),
                copy: ServerCopy::of_row(1, None),
            }
        );
    }

    /// Delete removes the row and queues a server delete against the folder the
    /// message was actually in, rather than the hardcoded INBOX the file build
    /// had to assume.
    #[test]
    fn deleting_removes_the_row_and_queues_a_server_delete() {
        let fx = fixture();
        let id = fx.ingest_plain("archive", 4, "Junk");

        let deleted = queue_delete(
            &fx.store,
            &fx.blobs,
            "alice",
            &refs(&[id]),
            "Archive",
            Runner::Drain,
        );

        assert_eq!(rows(&deleted), vec![id]);
        assert!(mailbox_of(&fx, id).is_none(), "the row survived the delete");
        assert_eq!(
            only_queued_op(&fx),
            ServerOp::Delete {
                message_id: "<archive-4@example.com>".to_string(),
                source_mailbox: "Archive".to_string(),
                copy: ServerCopy::of_row(4, None),
            }
        );
    }

    /// The flag lands on the row first, and the queued op carries the new state
    /// and the folder to apply it in.
    #[test]
    fn flagging_writes_seen_and_queues_the_matching_server_op() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 2, "Unread");

        queue_read_flag(
            &fx.store,
            "alice",
            &refs(&[id]),
            true,
            "INBOX",
            Runner::Drain,
        );

        assert!(read::find_by_id(&fx.store, id).unwrap().unwrap().is_read());
        assert_eq!(
            only_queued_op(&fx),
            ServerOp::SetRead {
                message_id: "<inbox-2@example.com>".to_string(),
                mailbox: "INBOX".to_string(),
                read: true,
                copy: ServerCopy::of_row(2, None),
            }
        );
    }

    /// The star lands on the row first and the queued op carries the new state
    /// and the folder to apply it in (#0007). Flagging leaves the read bit
    /// alone.
    #[test]
    fn flagging_writes_the_star_and_queues_the_matching_server_op() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 7, "Important");

        queue_flag(
            &fx.store,
            "alice",
            &refs(&[id]),
            true,
            "INBOX",
            Runner::Drain,
        );

        let row = read::find_by_id(&fx.store, id).unwrap().unwrap();
        assert!(row.is_flagged());
        assert!(!row.is_read(), "flagging must not touch the read bit");
        assert_eq!(
            only_queued_op(&fx),
            ServerOp::SetFlagged {
                message_id: "<inbox-7@example.com>".to_string(),
                mailbox: "INBOX".to_string(),
                flagged: true,
                copy: ServerCopy::of_row(7, None),
            }
        );
    }

    /// A batch moves every row and queues one op per message: the store shows
    /// the whole selection archived and the queue owes one op each.
    #[test]
    fn a_batch_moves_every_row_and_queues_one_op_each() {
        let fx = fixture();
        let ids: Vec<i64> = (1..=3)
            .map(|uid| fx.ingest_plain("inbox", uid, &format!("Mail {uid}")))
            .collect();

        let moved = queue_move(
            &fx.store,
            "alice",
            &refs(&ids),
            "archive",
            "INBOX",
            "Archive",
            Runner::Drain,
        );

        assert_eq!(moved.len(), 3);
        for id in &ids {
            assert_eq!(mailbox_of(&fx, *id).as_deref(), Some("archive"));
        }
        assert_eq!(
            read::list_mailbox(&fx.store, "alice", "inbox")
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            pending_ops::queued_ops(&fx.store, "alice").unwrap().len(),
            3
        );
    }

    /// Moving an invite is what the Calendar view has to hear about: the
    /// agenda is a snapshot of the invite rows, so the copy taken before the
    /// move still points at the old mailbox and only a rebuild agrees with the
    /// store. This is the refresh the mutation arms run (`bg.rs` runs the same
    /// one after an RSVP).
    #[test]
    fn moving_an_invite_changes_what_a_rebuilt_agenda_reads() {
        let fx = fixture();
        let id = fx.ingest_invite("inbox", 1, "Standup", &invite_ics("uid-a", 0, &["a@x.com"]));
        let stale = agenda::load_events_for_account(&fx.store, &fx.blobs, "alice", "");
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].row_id, id);

        queue_move(
            &fx.store,
            "alice",
            &refs(&[id]),
            "archive",
            "INBOX",
            "Archive",
            Runner::Drain,
        );
        let rebuilt = agenda::load_events_for_account(&fx.store, &fx.blobs, "alice", "");

        assert_eq!(rebuilt.len(), 1, "the invite is still on the agenda");
        assert_eq!(mailbox_of(&fx, id).as_deref(), Some("archive"));
    }

    /// Deleting an invite is the case a stale agenda gets visibly wrong: the
    /// row is gone, so a rebuilt agenda drops the event while the snapshot
    /// taken before the mutation still lists it.
    #[test]
    fn deleting_an_invite_drops_it_from_a_rebuilt_agenda() {
        let fx = fixture();
        let id = fx.ingest_invite("inbox", 1, "Standup", &invite_ics("uid-a", 0, &["a@x.com"]));
        let stale = agenda::load_events_for_account(&fx.store, &fx.blobs, "alice", "");
        assert_eq!(stale.len(), 1);

        queue_delete(
            &fx.store,
            &fx.blobs,
            "alice",
            &refs(&[id]),
            "INBOX",
            Runner::Drain,
        );
        let rebuilt = agenda::load_events_for_account(&fx.store, &fx.blobs, "alice", "");

        assert!(rebuilt.is_empty(), "a deleted invite stayed on the agenda");
        assert_eq!(stale.len(), 1, "the pre-mutation snapshot is the stale one");
    }

    /// A reference to a row that is gone is skipped rather than reported: it
    /// queues no op, so nothing is owed to the server for a message the store
    /// no longer holds.
    #[test]
    fn a_dead_reference_queues_nothing() {
        let fx = fixture();
        assert!(queue_move(
            &fx.store,
            "alice",
            &refs(&[404]),
            "archive",
            "INBOX",
            "Archive",
            Runner::Drain
        )
        .is_empty());
        assert!(queue_delete(
            &fx.store,
            &fx.blobs,
            "alice",
            &refs(&[404]),
            "INBOX",
            Runner::Drain
        )
        .is_empty());
        assert!(queue_read_flag(
            &fx.store,
            "alice",
            &refs(&[404]),
            true,
            "INBOX",
            Runner::Drain
        )
        .is_empty());
        assert!(queue_flag(
            &fx.store,
            "alice",
            &refs(&[404]),
            true,
            "INBOX",
            Runner::Drain
        )
        .is_empty());
        assert!(pending_ops::queued_ops(&fx.store, "alice")
            .unwrap()
            .is_empty());
    }

    /// Two rows sharing a Message-ID in one mailbox, the two server copies
    /// PERSO-236 left behind, each queue an op that names its own UID and the
    /// mailbox's UIDVALIDITY, so the server acts on the copy the user picked
    /// (PERSO-238). A row a local move parked on its `-id` sentinel has no
    /// server UID yet and names none.
    #[test]
    fn each_copy_of_a_shared_message_id_queues_an_op_naming_its_uid() {
        let fx = fixture();
        let first = fx.ingest_plain("archive", 285, "Copy");
        let second = fx.ingest_plain("archive", 288, "Copy");
        fx.store
            .conn()
            .execute(
                "UPDATE messages SET message_id = '<dup@example.com>' WHERE id IN (?1, ?2)",
                [first, second],
            )
            .unwrap();
        crate::ingest::record_mailbox_cursor(
            &fx.store,
            "alice",
            "archive",
            &crate::ingest::MailboxCursor {
                uidvalidity: Some(7),
                ..Default::default()
            },
        )
        .unwrap();

        queue_flag(
            &fx.store,
            "alice",
            &refs(&[second]),
            true,
            "Archive",
            Runner::Drain,
        );
        queue_move(
            &fx.store,
            "alice",
            &refs(&[first]),
            "inbox",
            "Archive",
            "INBOX",
            Runner::Drain,
        );
        let ops: Vec<ServerOp> = pending_ops::queued_ops(&fx.store, "alice")
            .unwrap()
            .into_iter()
            .map(|row| row.op)
            .collect();
        let named = |uid| {
            Some(ServerCopy {
                uid,
                uidvalidity: Some(7),
            })
        };
        assert_eq!(
            ops,
            vec![
                ServerOp::SetFlagged {
                    message_id: "<dup@example.com>".to_string(),
                    mailbox: "Archive".to_string(),
                    flagged: true,
                    copy: named(288),
                },
                ServerOp::Move {
                    message_id: "<dup@example.com>".to_string(),
                    source_mailbox: "Archive".to_string(),
                    dest_mailbox: "INBOX".to_string(),
                    copy: named(285),
                },
            ]
        );

        // The moved row now sits on its `-id` sentinel in inbox.
        queue_read_flag(
            &fx.store,
            "alice",
            &refs(&[first]),
            true,
            "INBOX",
            Runner::Drain,
        );
        let queued = pending_ops::queued_ops(&fx.store, "alice").unwrap();
        assert!(
            matches!(&queued[2].op, ServerOp::SetRead { copy: None, .. }),
            "{:?}",
            queued[2].op
        );
    }
}
