//! The durable mutation queue: a flag, move or delete that survives a kill -9.
//!
//! Stage 3 of the data-access-layer redesign (#0039). Archive, delete, move and
//! mark-read / mark-unread change server-side state the server still holds, so
//! losing one loses a flag change or delays a move, never a message. Send is not
//! a kind here: it has the stricter [`crate::outbox`] state machine, because
//! SMTP is not retryable the way a flag toggle is.
//!
//! This is the mutation twin of the outbox, and it follows the same durability
//! patterns rather than inventing parallel ones:
//!
//! - the local write and the queue row commit in **one** transaction
//!   ([`apply_move`], [`apply_delete`], [`apply_set_read`], [`apply_set_flagged`]),
//!   so a crash can never leave the store optimistically changed with no durable
//!   record of the op it owes the server, nor an op row for a change that never
//!   landed locally;
//! - a background engine drains the queue ([`drain`]) with exponential backoff
//!   (the same [`crate::outbox::backoff_secs`] curve), on startup and on the
//!   sync tick, exactly where [`crate::send::resume_outbox`] already runs;
//! - replay is exactly-once for the local half by construction: the local write
//!   happened once, inside the commit, and the drain **never re-applies it**. It
//!   runs only the server op, and the drain **converges** that op on replay. A
//!   move, delete or flag whose server half already landed before the crash
//!   replays against a message the source folder no longer holds; both backends
//!   report that as a typed [`crate::ops::NotFoundOnServer`], which the drain
//!   treats as success and retires the row. So a crash between the server op and
//!   the row's retirement re-runs the op and converges, rather than parking a
//!   succeeded op as `failed` and rolling the local state back under it;
//! - two drains racing on these rows is the destructive case the engine
//!   advisory lock ([`crate::engine_lock`], #0061) exists to exclude, so a live
//!   drain runs under that lock ([`drain_account`]);
//! - a caller that runs its own op ([`run_and_settle`]) does not take that
//!   lock, because holding the engine turn across a server call would refuse
//!   the sync tick its own change wakes. Its op is committed `settling`
//!   instead of `queued`, in the transaction that commits the local write, so
//!   no drain ever sees it as owed (PERSO-236: a tick that started while a
//!   settled move was on the server ran the same move again and left two
//!   copies of the message);
//! - the ops on one message reach the server in the order they were
//!   committed, whoever runs them: the drain holds a `queued` op back behind
//!   an older one still owed on its message, `settling` or `queued` in
//!   backoff ([`drain`], PERSO-243), and a caller waits for the older ops
//!   still owed on its message before its own server call
//!   ([`run_and_settle`], PERSO-241), running none of them itself, so each
//!   still runs once. A delete orders nothing after it, since an op that
//!   follows it on its row is on a new mail that reused the row id
//!   ([`orders_later_ops`]). A move refused for good rolls back the later
//!   ops on its message with it, newest first, since they were committed
//!   against the folder its rollback takes the message out of, `queued` and
//!   `settling` alike, in the one transaction that settles it ([`refuse`],
//!   PERSO-249), so a command committing meanwhile lands before the refusal
//!   or after its rollbacks. Any other op refused for good changes no folder
//!   and releases the later ops to run, and a refused flag toggle puts back
//!   only its own bit ([`roll_flag_back`]).
//!
//! ## States
//!
//! Simpler than the outbox, because a mutation is single-phase (there is no
//! APPEND-to-Sent second leg):
//!
//! - `queued`: the local write is committed and the server op is owed. The
//!   drain attempts it under backoff.
//! - `settling`: the same, for an op the committing caller runs itself
//!   ([`run_and_settle`], [`Runner::Caller`]). The drain does not run it; it
//!   becomes `queued` only once its caller is gone
//!   ([`recover_orphaned_settles`]). The settle retires it or rolls it back
//!   and discards it. It reaches `failed` only when an older op on its
//!   message is refused for good and rolls it back with it; its caller then
//!   runs nothing and discards it, and one whose caller is gone stays
//!   `failed`, like a `queued` op rolled back the same way.
//! - a successful op **deletes** the row: a done mutation has nothing an
//!   operator can act on, unlike a `done` send that may carry a partial-delivery
//!   note, so there is nothing to retain.
//! - `failed`: the op was refused past its retry budget. The row is kept so the
//!   failure is visible, and the local state is rolled back to the server's:
//!   a moved row goes home, a flag toggle is undone. A delete has nothing to
//!   restore (the row is gone), so it converges when the next sync refetches the
//!   UID the server never dropped, which is the same answer
//!   [`crate::store::write`] already gives.
//!
//! ## Addressing
//!
//! `target_message_id` is the `messages` row id, for correlation with the store
//! (#0039 amendment); the full server addressing rides in the JSON payload as a
//! [`ServerOp`], which names the message by `Message-ID` and, for a move, a
//! delete and the two flag toggles, by the UID of the copy the row stood for
//! when the op was queued ([`crate::ops::ServerCopy`], PERSO-238): a folder can
//! hold two copies under one `Message-ID`, and the op acts on its own. A payload
//! queued before that field existed decodes without it and falls back to the
//! `Message-ID` search, so no queued row needs migrating.
//!
//! A move or delete whose copy is gone converges as a
//! [`crate::ops::NotFoundOnServer`] only when a completed run of the op
//! explains the miss; the IMAP arm reports any other miss, such as a copy it
//! cannot tell from a twin, as an ordinary error, which the drain retries and
//! then rolls back (`crate::imap_client::ops::copy_to_move`).

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use log::{info, warn};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::config::AccountConfig;
use crate::ops::{run_op, Backend, ServerOp};
use crate::outbox::{backoff_secs, unix_now};
use crate::store::write::MutatedRow;
use crate::store::{BlobStore, Store};
use crate::types::MessageFlags;

/// How many times the drain attempts a queued op before it gives up, rolls the
/// local state back and parks the row as `failed`.
///
/// The server ops (`imap_client` move / delete / flag, and their Graph twins)
/// return a plain error without a permanent-vs-transient verdict, so unlike the
/// outbox's SMTP classification the queue cannot tell a 5xx refusal from a
/// dropped connection. It therefore retries a bounded number of times under
/// backoff and then surfaces the failure, which is the honest reading of "we
/// asked and it did not take". A standing refusal (a delete the server rejects)
/// surfaces after the budget rather than on the first attempt.
pub const MAX_ATTEMPTS: i64 = 5;

/// `queued`, the one non-terminal state.
pub const STATE_QUEUED: &str = "queued";
/// `failed`, kept for visibility after the retry budget is spent.
pub const STATE_FAILED: &str = "failed";
/// `settling`: owed to the server, and run by the caller that committed it
/// ([`run_and_settle`]) rather than by the drain (PERSO-236). The drain never
/// runs a `settling` row; [`recover_orphaned_settles`] hands one back to it
/// as `queued` once no caller in this process can still be running it.
pub const STATE_SETTLING: &str = "settling";

/// Who runs the server half of an op being committed (PERSO-236).
#[derive(Debug, Clone, Copy)]
pub enum Runner<'a> {
    /// The background drain, under the engine turn: the row is `queued`.
    Drain,
    /// The caller, at once, through [`run_and_settle`] under this scope: the
    /// row is `settling`, which the drain leaves alone, so the op cannot run
    /// twice when a sync tick drains the account while the caller's server
    /// call is still in flight.
    Caller(&'a SettleScope),
}

/// The stores that have a [`SettleScope`] open in this process, with how many.
///
/// Keyed by the store's path rather than by account name, for the reason
/// [`crate::engine_lock`] keys its gates by path: two data roots, or two
/// tests, may each have an account of the same name.
static LIVE_SETTLES: LazyLock<Mutex<HashMap<PathBuf, usize>>> = LazyLock::new(Default::default);

fn live_settles() -> MutexGuard<'static, HashMap<PathBuf, usize>> {
    LIVE_SETTLES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A caller's hold on the ops it runs itself in one store, from before it
/// commits them `settling` until it has settled them (PERSO-236).
///
/// While one is open, [`recover_orphaned_settles`] leaves every `settling` row
/// of the store alone, because any of them may be on the server right now.
/// Once none is, a `settling` row can only be one whose caller died or failed
/// to retire it, and it is handed to the drain, which converges a replay of
/// an op that already landed (see the module docs). Only the daemon commits
/// `settling` rows, and only the daemon drains, so a scope in this process is
/// the whole answer to "is a caller still running this op".
#[derive(Debug)]
pub struct SettleScope {
    store: PathBuf,
}

impl SettleScope {
    /// Open a scope over `store`, before the ops it covers are committed.
    pub fn open(store: &Store) -> SettleScope {
        let path = store.path().to_path_buf();
        *live_settles().entry(path.clone()).or_insert(0) += 1;
        SettleScope { store: path }
    }

    /// Refuse a store this scope was not opened over.
    fn covers(&self, store: &Store) -> Result<()> {
        anyhow::ensure!(
            self.store == store.path(),
            "a settle scope opened over {} cannot settle an op in {}",
            self.store.display(),
            store.path().display()
        );
        Ok(())
    }
}

impl Drop for SettleScope {
    fn drop(&mut self) {
        let mut live = live_settles();
        if let Some(count) = live.get_mut(&self.store) {
            *count -= 1;
            if *count == 0 {
                live.remove(&self.store);
            }
        }
    }
}

/// The state a row committed for `runner` starts in, refusing a scope opened
/// over another store.
fn state_for(store: &Store, runner: Runner<'_>) -> Result<&'static str> {
    match runner {
        Runner::Drain => Ok(STATE_QUEUED),
        Runner::Caller(scope) => {
            scope.covers(store)?;
            Ok(STATE_SETTLING)
        }
    }
}

/// Hand every `settling` row of `account` back to the drain as `queued`, unless
/// a [`SettleScope`] over `store` is open in this process (PERSO-236).
///
/// What is left `settling` with no scope open is an op whose caller died
/// between its commit and its settle, or could not retire it: the drain
/// replays it, and a replay of an op that already landed converges. The
/// registry stays locked across the update, so a scope cannot open, commit a
/// row and start its server call between the check and the write.
pub fn recover_orphaned_settles(store: &Store, account: &str) -> Result<usize> {
    let live = live_settles();
    if live.contains_key(store.path()) {
        return Ok(0);
    }
    let recovered = store
        .conn()
        .execute(
            "UPDATE pending_ops SET state = ?2, updated = ?3
             WHERE account = ?1 AND state = ?4",
            rusqlite::params![account, STATE_QUEUED, unix_now(), STATE_SETTLING],
        )
        .context("handing orphaned settles back to the drain")?;
    drop(live);
    if recovered > 0 {
        warn!(
            "[pending_ops] {recovered} op(s) of {account} were left settling by a caller \
             that is gone; the drain runs them"
        );
    }
    Ok(recovered)
}

/// How to undo a mutation's local half when its server op is refused for good.
///
/// Stored in the row payload beside the [`ServerOp`], so a rollback after a
/// crash has everything it needs without a second query against a row the
/// mutation may have moved or removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rollback {
    /// Put a moved row back in its old mailbox and uid.
    Move(MutatedRow),
    /// Undo a read or star toggle: `flags` is the row's whole flag string
    /// before it, of which the rollback restores only the bit its op changed
    /// ([`roll_flag_back`], PERSO-243), so a bit written since, such as the
    /// `\Answered` of a reply sent meanwhile, is kept.
    Flags { id: i64, flags: String },
    /// Nothing to undo. Two cases:
    ///
    /// - a delete cannot be restored: the row is gone and the server still
    ///   holds the message, so the next sync refetches it;
    /// - the post-send answered/forwarded bit (#0076) must **not** be undone.
    ///   It records something that actually happened, the user's reply left the
    ///   building, and no server refusal makes that untrue. Rolling it back
    ///   would replace a true local statement the next sync can correct with a
    ///   false one, and this is the one op kind whose local half is written on
    ///   the tail of a delivered send.
    None,
}

/// The JSON payload of a `pending_ops` row: the op to run and how to undo it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Payload {
    op: ServerOp,
    rollback: Rollback,
}

/// One decoded `pending_ops` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOp {
    pub id: i64,
    pub account: String,
    pub kind: String,
    pub target_message_id: Option<i64>,
    pub op: ServerOp,
    pub rollback: Rollback,
    pub state: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created: i64,
    pub updated: i64,
}

// ---------------------------------------------------------------------------
// Enqueue, atomic with the local write
// ---------------------------------------------------------------------------

/// Insert a `queued` row inside an existing transaction.
///
/// This is the durability primitive: the caller opens one transaction, applies
/// the local store change and calls this in the same transaction, so the two
/// commit together. The `apply_*` functions below are the composed callers;
/// this is exposed so a future call site can compose its own local write with
/// the enqueue when it needs to.
pub fn enqueue(
    conn: &Connection,
    account: &str,
    target_message_id: Option<i64>,
    op: &ServerOp,
    rollback: &Rollback,
) -> Result<i64> {
    insert(conn, account, target_message_id, op, rollback, STATE_QUEUED)
}

/// [`enqueue`], in the state its runner starts it in.
fn insert(
    conn: &Connection,
    account: &str,
    target_message_id: Option<i64>,
    op: &ServerOp,
    rollback: &Rollback,
    state: &str,
) -> Result<i64> {
    let payload = serde_json::to_string(&Payload {
        op: op.clone(),
        rollback: rollback.clone(),
    })
    .context("encoding the pending op payload")?;
    let now = unix_now();
    conn.execute(
        "INSERT INTO pending_ops (
            account, kind, target_message_id, payload, state, attempts,
            last_error, created, updated
         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, NULL, ?6, ?6)",
        rusqlite::params![account, op.kind(), target_message_id, payload, state, now],
    )
    .context("inserting the pending op row")?;
    Ok(conn.last_insert_rowid())
}

/// Move a row into `dest_mailbox` and enqueue the server move, atomically.
///
/// Returns the row's previous coordinates (what a caller hands its UI) and the
/// queued op id. `Ok(None)` when the row is already gone, which is a no-op, not
/// an error.
pub fn apply_move(
    store: &Store,
    account: &str,
    id: i64,
    dest_mailbox: &str,
    op: ServerOp,
    runner: Runner<'_>,
) -> Result<Option<(MutatedRow, i64)>> {
    let state = state_for(store, runner)?;
    let tx = store
        .immediate_transaction()
        .context("opening the move-and-enqueue transaction")?;
    let Some(previous) = coordinates_in(&tx, id)? else {
        return Ok(None);
    };
    // The Graph id goes with the source folder's item (PERSO-245): Graph
    // gives the moved item a new one, which the next listing of the
    // destination records.
    tx.execute(
        "UPDATE messages SET mailbox = ?2, uid = ?3, graph_id = NULL WHERE id = ?1",
        rusqlite::params![id, dest_mailbox, -id],
    )
    .context("moving the message row")?;
    let op_id = insert(
        &tx,
        account,
        Some(id),
        &op,
        &Rollback::Move(previous.clone()),
        state,
    )?;
    crate::sync::departures::record(store, account, &op);
    tx.commit().context("committing the move and its op")?;
    info!(
        "[pending_ops] queued a {} for row {id} as op {op_id}",
        op.kind()
    );
    Ok(Some((previous, op_id)))
}

/// Delete a row and enqueue the server delete, atomically.
pub fn apply_delete(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    id: i64,
    op: ServerOp,
    runner: Runner<'_>,
) -> Result<Option<(MutatedRow, i64)>> {
    let state = state_for(store, runner)?;
    let tx = store
        .immediate_transaction()
        .context("opening the delete-and-enqueue transaction")?;
    let Some(previous) = coordinates_in(&tx, id)? else {
        return Ok(None);
    };
    let hashes = blob_refs_in(&tx, id)?;
    tx.execute("DELETE FROM messages_fts WHERE rowid = ?1", [id])
        .context("removing the FTS entry")?;
    tx.execute("DELETE FROM messages WHERE id = ?1", [id])
        .context("deleting the message row")?;
    for hash in &hashes {
        blobs.release(&tx, hash)?;
    }
    let op_id = insert(&tx, account, Some(id), &op, &Rollback::None, state)?;
    crate::sync::departures::record(store, account, &op);
    tx.commit().context("committing the delete and its op")?;
    info!("[pending_ops] queued a delete for row {id} as op {op_id}");
    Ok(Some((previous, op_id)))
}

/// Set (or clear) `\Seen` on a row and enqueue the server op, atomically.
pub fn apply_set_read(
    store: &Store,
    account: &str,
    id: i64,
    read: bool,
    op: ServerOp,
    runner: Runner<'_>,
) -> Result<Option<i64>> {
    apply_flag_change(store, account, id, op, runner, |flags| {
        flags.with_seen(read)
    })
}

/// Set (or clear) `\Flagged` on a row and enqueue the server op, atomically.
pub fn apply_set_flagged(
    store: &Store,
    account: &str,
    id: i64,
    flagged: bool,
    op: ServerOp,
    runner: Runner<'_>,
) -> Result<Option<i64>> {
    apply_flag_change(store, account, id, op, runner, |flags| {
        flags.with_flagged(flagged)
    })
}

/// The shared read-modify-write behind the two flag toggles: read the row's
/// flags, apply `f`, write the canonical string back and enqueue the op, all in
/// one transaction. The rollback carries the *old* string, from which a
/// refused op restores the one bit `f` changed ([`roll_flag_back`]).
fn apply_flag_change(
    store: &Store,
    account: &str,
    id: i64,
    op: ServerOp,
    runner: Runner<'_>,
    f: impl FnOnce(crate::types::MessageFlags) -> crate::types::MessageFlags,
) -> Result<Option<i64>> {
    let state = state_for(store, runner)?;
    let tx = store
        .immediate_transaction()
        .context("opening the flag-and-enqueue transaction")?;
    let current: Option<Option<String>> = tx
        .query_row("SELECT flags FROM messages WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .context("reading a message's flags")?;
    let Some(current) = current else {
        return Ok(None);
    };
    let old = current.unwrap_or_default();
    let new = f(crate::types::MessageFlags::parse(&old)).to_flag_string();
    tx.execute(
        "UPDATE messages SET flags = ?2 WHERE id = ?1",
        rusqlite::params![id, new],
    )
    .context("setting a message's flags")?;
    let op_id = insert(
        &tx,
        account,
        Some(id),
        &op,
        &Rollback::Flags { id, flags: old },
        state,
    )?;
    tx.commit()
        .context("committing the flag change and its op")?;
    info!(
        "[pending_ops] queued a {} for row {id} as op {op_id}",
        op.kind()
    );
    Ok(Some(op_id))
}

/// What [`apply_post_send_flag`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostSendFlag {
    /// Local rows whose answered/forwarded bit was written.
    pub rows: usize,
    /// The queued op, when there was a server half to owe. `None` when the
    /// account has no server to write to, or when nothing local matched.
    pub op_id: Option<i64>,
}

/// Write the post-send `\Answered` / `$Forwarded` bit on every local copy of a
/// source message and enqueue the single multi-mailbox server op, atomically
/// (#TKT-0051, #0076).
///
/// The send path's op: it runs after a message has already been delivered, so
/// it is called for its effect and its error is only ever logged. What the
/// queue buys over the old inline per-mailbox `UID STORE` is that the server
/// half is durable and retried by the background drain instead of being lost
/// with the process, and that it costs the send path one `COMMIT` rather than N
/// IMAP logins.
///
/// `server_mailboxes` is empty for an account with no usable server config,
/// which writes the local half and queues nothing. A Graph account queues the
/// same op, which the Graph arm of [`run_op`] turns into Outlook's MAPI
/// history properties (PERSO-171); `reply_all` only matters there.
///
/// The op's rollback is [`Rollback::None`] on purpose; see that variant.
pub fn apply_post_send_flag(
    store: &Store,
    account: &str,
    message_id: &str,
    answered: bool,
    reply_all: bool,
    server_mailboxes: &[String],
) -> Result<PostSendFlag> {
    let tx = store
        .immediate_transaction()
        .context("opening the post-send flag transaction")?;
    let ids: Vec<i64> = {
        let mut stmt =
            tx.prepare("SELECT id FROM messages WHERE account = ?1 AND message_id = ?2")?;
        let rows = stmt.query_map(rusqlite::params![account, message_id], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<i64>>>()
            .context("listing the local copies of a sent message's source")?
    };
    if ids.is_empty() {
        return Ok(PostSendFlag::default());
    }
    for id in &ids {
        let current: Option<String> = tx
            .query_row("SELECT flags FROM messages WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .context("reading a message's flags")?
            .flatten();
        let old = crate::types::MessageFlags::parse(current.as_deref().unwrap_or_default());
        let new = if answered {
            crate::types::MessageFlags {
                answered: true,
                ..old
            }
        } else {
            crate::types::MessageFlags {
                forwarded: true,
                ..old
            }
        };
        tx.execute(
            "UPDATE messages SET flags = ?2 WHERE id = ?1",
            rusqlite::params![id, new.to_flag_string()],
        )
        .context("writing the post-send flag")?;
    }
    let op_id = if server_mailboxes.is_empty() {
        None
    } else {
        Some(enqueue(
            &tx,
            account,
            ids.first().copied(),
            &ServerOp::SetAnswered {
                message_id: message_id.to_string(),
                mailboxes: server_mailboxes.to_vec(),
                answered,
                reply_all: answered && reply_all,
            },
            &Rollback::None,
        )?)
    };
    tx.commit()
        .context("committing the post-send flag and its op")?;
    if let Some(op_id) = op_id {
        info!("[pending_ops] queued a set_answered for {message_id} as op {op_id}");
    }
    Ok(PostSendFlag {
        rows: ids.len(),
        op_id,
    })
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Every `queued` row for `account`, oldest first: what a drain works through.
pub fn queued_ops(store: &Store, account: &str) -> Result<Vec<PendingOp>> {
    rows_in_state(store, account, STATE_QUEUED)
}

/// Every `queued` or `settling` row for `account`, oldest first: the ops whose
/// server half is still owed, whoever runs it (PERSO-240).
pub fn unsettled_ops(store: &Store, account: &str) -> Result<Vec<PendingOp>> {
    let mut owed = rows_in_state(store, account, STATE_QUEUED)?;
    owed.extend(rows_in_state(store, account, STATE_SETTLING)?);
    owed.sort_by_key(|row| row.id);
    Ok(owed)
}

/// Every `failed` row for `account`, oldest first: what the UI surfaces so a
/// refused mutation is not silent (#0039 scope item 3).
pub fn failed_ops(store: &Store, account: &str) -> Result<Vec<PendingOp>> {
    rows_in_state(store, account, STATE_FAILED)
}

/// `(queued, failed)` counts in one query, for a badge.
pub fn counts(store: &Store, account: &str) -> Result<(usize, usize)> {
    let mut stmt = store
        .conn()
        .prepare("SELECT state, COUNT(*) FROM pending_ops WHERE account = ?1 GROUP BY state")?;
    let rows = stmt.query_map([account], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let (mut queued, mut failed) = (0usize, 0usize);
    for row in rows {
        let (state, n) = row?;
        match state.as_str() {
            STATE_QUEUED => queued = n as usize,
            STATE_FAILED => failed = n as usize,
            _ => {}
        }
    }
    Ok((queued, failed))
}

/// Queue one drafts-mirror op (#0143) in a transaction of its own.
///
/// A draft op has no local half to commit with it: the draft file is the
/// truth and is already written, so its rollback is [`Rollback::None`] and
/// `target_message_id` is `NULL`, since it addresses a file and no `messages`
/// row. A push for a draft that already has one queued, and a delete of a
/// `Message-ID` that already has one queued, are not queued twice: the drain
/// reads the file when it runs, so one queued push carries every save before
/// it; the queued push takes the newer op's `hash`, so a push parked
/// `failed` names the last bytes it was owed for. `Ok(None)` when nothing was
/// queued.
pub fn enqueue_draft_op(store: &Store, account: &str, op: &ServerOp) -> Result<Option<i64>> {
    for row in queued_ops(store, account)? {
        match (&row.op, op) {
            (
                ServerOp::PushDraft {
                    draft_id: a,
                    hash: queued,
                },
                ServerOp::PushDraft { draft_id: b, hash },
            ) if a == b => {
                if queued != hash {
                    rewrite_queued(store, &row, op)?;
                }
                return Ok(None);
            }
            (
                ServerOp::DeleteDraft { message_id: a, .. },
                ServerOp::DeleteDraft { message_id: b, .. },
            ) if a == b => return Ok(None),
            _ => {}
        }
    }
    enqueue(store.conn(), account, None, op, &Rollback::None).map(Some)
}

/// Record on the queued push of `draft_id` that it is running for the bytes
/// hashing to `hash` (#0143): a first push writes the draft's
/// `server_message_id`, which changes the file after the push was queued, and
/// a push parked `failed` must name the bytes it last tried.
pub fn note_push_hash(store: &Store, account: &str, draft_id: &str, hash: &str) -> Result<()> {
    for row in queued_ops(store, account)? {
        if let ServerOp::PushDraft {
            draft_id: id,
            hash: queued,
        } = &row.op
        {
            if id == draft_id && queued.as_deref() != Some(hash) {
                let op = ServerOp::PushDraft {
                    draft_id: id.clone(),
                    hash: Some(hash.to_string()),
                };
                rewrite_queued(store, &row, &op)?;
            }
        }
    }
    Ok(())
}

/// Replace the op of the queued row `row` with `op`.
fn rewrite_queued(store: &Store, row: &PendingOp, op: &ServerOp) -> Result<()> {
    let payload = serde_json::to_string(&Payload {
        op: op.clone(),
        rollback: row.rollback.clone(),
    })
    .context("encoding the pending op payload")?;
    store
        .conn()
        .execute(
            "UPDATE pending_ops SET payload = ?1 WHERE id = ?2 AND state = ?3",
            rusqlite::params![payload, row.id, STATE_QUEUED],
        )
        .context("updating a queued push")?;
    Ok(())
}

/// The drafts whose push was parked `failed` (#0143), which their rows show as
/// `failed` until a later push of the same draft succeeds.
pub fn failed_draft_pushes(store: &Store, account: &str) -> Result<Vec<String>> {
    Ok(failed_ops(store, account)?
        .into_iter()
        .filter_map(|row| match row.op {
            ServerOp::PushDraft { draft_id, .. } => Some(draft_id),
            _ => None,
        })
        .collect())
}

/// How long a push parked `failed` keeps the owed-push scan from queueing the
/// same bytes again (#0143): the queue's longest backoff.
pub const PUSH_RETRY_COOL_DOWN_SECS: i64 = crate::outbox::BACKOFF_MAX_SECS;

/// Whether a push of the draft `draft_id` for the bytes hashing to `hash` was
/// parked `failed` less than [`PUSH_RETRY_COOL_DOWN_SECS`] before `now`
/// (#0143): the owed-push scan does not queue it again until then, so a
/// server out of reach is not hammered at every drain. Past the cool-down the
/// scan queues one fresh push, and the next edit, a new hash, queues one at
/// once.
pub fn push_failed_for(
    store: &Store,
    account: &str,
    draft_id: &str,
    hash: &str,
    now: i64,
) -> Result<bool> {
    Ok(failed_ops(store, account)?.iter().any(|row| {
        row.updated + PUSH_RETRY_COOL_DOWN_SECS > now
            && matches!(&row.op, ServerOp::PushDraft { draft_id: id, hash: Some(h) }
                if id == draft_id && h == hash)
    }))
}

/// Settle the other failed pushes of the draft whose push `parked` was just
/// parked `failed` (#0143): each is retired, so a draft has one failed row at
/// a time. Returns whether one of them was for the same bytes, which makes
/// this park a retry after the cool-down whose failure was already
/// announced.
fn supersede_failed_pushes(store: &Store, account: &str, parked: i64) -> Result<bool> {
    let failed = failed_ops(store, account)?;
    let Some((draft_id, hash)) =
        failed
            .iter()
            .find(|row| row.id == parked)
            .and_then(|row| match &row.op {
                ServerOp::PushDraft { draft_id, hash } => Some((draft_id.clone(), hash.clone())),
                _ => None,
            })
    else {
        return Ok(false);
    };
    let mut repeat = false;
    for row in &failed {
        if row.id == parked {
            continue;
        }
        if let ServerOp::PushDraft {
            draft_id: id,
            hash: h,
        } = &row.op
        {
            if *id == draft_id {
                repeat |= *h == hash;
                retire(store, row.id)?;
            }
        }
    }
    Ok(repeat)
}

/// Forget the failed pushes of one draft, which a push that succeeded has
/// made moot.
pub fn forget_failed_pushes(store: &Store, account: &str, draft_id: &str) -> Result<()> {
    for row in failed_ops(store, account)? {
        if matches!(&row.op, ServerOp::PushDraft { draft_id: id, .. } if id == draft_id) {
            retire(store, row.id)?;
        }
    }
    Ok(())
}

fn rows_in_state(store: &Store, account: &str, state: &str) -> Result<Vec<PendingOp>> {
    let mut stmt = store.conn().prepare(
        "SELECT id, account, kind, target_message_id, payload, state, attempts,
                last_error, created, updated
         FROM pending_ops WHERE account = ?1 AND state = ?2 ORDER BY id",
    )?;
    let rows = stmt.query_map(rusqlite::params![account, state], row_from_sql)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingOp> {
    let payload: Option<String> = row.get(4)?;
    let decoded: Payload = payload
        .as_deref()
        .and_then(|p| serde_json::from_str(p).ok())
        .ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(
                4,
                "payload".to_string(),
                rusqlite::types::Type::Text,
            )
        })?;
    Ok(PendingOp {
        id: row.get(0)?,
        account: row.get(1)?,
        kind: row.get(2)?,
        target_message_id: row.get(3)?,
        op: decoded.op,
        rollback: decoded.rollback,
        state: row.get(5)?,
        attempts: row.get(6)?,
        last_error: row.get(7)?,
        created: row.get(8).unwrap_or(0),
        updated: row.get(9).unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// The drain
// ---------------------------------------------------------------------------

/// The server side of the drain, as a trait so the state machine is testable
/// against an in-memory fake. The live implementation is [`Backend`]; a test
/// drives every retry and rollback path offline.
///
/// An executor reports faithfully: a not-found on the server surfaces as an
/// [`Err`] carrying [`crate::ops::NotFoundOnServer`], and [`drain`] owns the
/// policy of treating that as a converged replay rather than a failure.
pub trait OpExecutor {
    fn execute(&mut self, op: &ServerOp) -> impl Future<Output = Result<()>> + Send;

    /// Run one drafts-mirror op (#0143), which reads the draft file and writes
    /// the store's `draft_sync` row and so needs the store the drain holds.
    /// An executor that runs no draft op refuses it, which the drain retries
    /// and parks like any refusal.
    fn execute_draft(&mut self, store: &Store, op: &ServerOp) -> impl Future<Output = Result<()>> {
        let _ = store;
        let kind = op.kind();
        async move {
            Err(anyhow::anyhow!(
                "this executor runs no drafts-mirror op ({kind})"
            ))
        }
    }
}

/// The executor an account's queue drains with: its backend for the message
/// ops, and the account's configuration for the drafts mirror, which renders
/// the draft and resolves the Drafts folder.
pub struct AccountExecutor {
    backend: Backend,
    account: AccountConfig,
}

impl AccountExecutor {
    /// The backend `account`'s owed ops run against ([`Backend::resolve`]).
    pub fn resolve(account: &AccountConfig) -> Result<AccountExecutor> {
        Ok(AccountExecutor {
            backend: Backend::resolve(account)?,
            account: account.clone(),
        })
    }
}

impl OpExecutor for AccountExecutor {
    async fn execute(&mut self, op: &ServerOp) -> Result<()> {
        run_op(&self.backend, op).await
    }

    async fn execute_draft(&mut self, store: &Store, op: &ServerOp) -> Result<()> {
        match &self.backend {
            Backend::Imap(config) => {
                let email = crate::config::load_global_config()
                    .map(|config| config.email)
                    .unwrap_or_default();
                let mirror = crate::draft_mirror::push::Mirror {
                    store,
                    account: &self.account,
                    email: &email,
                    now: unix_now(),
                };
                let mut folder =
                    crate::draft_mirror::folder::ImapDraftsFolder::new((**config).clone());
                crate::draft_mirror::push::run(&mirror, &mut folder, op).await
            }
            // Not built: a Graph account mirrors nothing, so nothing queues
            // one of these for it, and a row that predates the switch is
            // refused into `failed` rather than run against the wrong folder.
            Backend::Graph(_) => Err(anyhow::anyhow!(
                "the drafts mirror is not built for Graph accounts"
            )),
        }
    }
}

impl OpExecutor for Backend {
    async fn execute(&mut self, op: &ServerOp) -> Result<()> {
        run_op(self, op).await
    }
}

/// What one [`drain`] pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainResult {
    /// Rows whose op succeeded and were retired.
    pub completed: usize,
    /// Rows parked as `failed` this pass, with their local state rolled back.
    pub failed: usize,
    /// Rows still `queued` after this pass (backoff not elapsed, one more
    /// retryable failure short of the budget, or held behind an older op
    /// still owed on the same message).
    pub still_open: usize,
}

/// Drive every `queued` row for `account` towards done or failed.
///
/// One row at a time, oldest first. A row whose backoff has not elapsed is
/// counted and left. A successful op retires the row; a failure bumps its
/// attempt count and, once the budget is spent, rolls the local state back and
/// parks the row as `failed`.
///
/// The drain **does not re-apply the local write**: that happened once, in the
/// transaction that enqueued the row. This is the guard against a duplicate
/// apply on replay after a crash.
///
/// A server op that comes back [`crate::ops::NotFoundOnServer`] is a converged
/// replay, not a failure: the op's earlier attempt already moved, deleted or
/// flagged the message, so the row is retired like any success. Every other
/// error is a genuine failure and is retried, then rolled back once the budget
/// is spent.
///
/// A `settling` row is a caller's to run ([`run_and_settle`]) and is never
/// run here; one its caller left behind is first handed back as `queued`
/// ([`recover_orphaned_settles`]).
///
/// The ops on one message reach the server in the order they were committed
/// (PERSO-236, PERSO-243): a `queued` row waits while an older op on its
/// message is still owed, an older `settling` row ([`settling_targets`]) or an
/// older `queued` row this pass leaves owed (its backoff has not elapsed, it
/// was refused short of the budget, or it waits itself). A waiting row is
/// counted as still open, is not run and spends no attempt, so its own
/// backoff and budget start only once it is first in line. The settle that
/// retires an older `settling` row asks for the drain that runs the rest; an
/// older `queued` row that runs in this pass releases the rows behind it in
/// the same pass.
///
/// A row refused for good takes the later rows still owed on its message
/// with it, `queued` or `settling` ([`refuse`]): each was committed against
/// the local state the refused op left, which its rollback undoes, so they
/// are rolled back first, newest first, and parked `failed` unrun, with no
/// attempt spent, in the one transaction that settles the refused row
/// (PERSO-249). Every row is read again before it runs, since the list the
/// pass works through may predate a refusal that parked it.
///
/// `pub(crate)`, not `pub`: the only lock-guarded entry point is
/// [`drain_account`], and draining outside that lock is the race the engine
/// advisory lock (#0061) exists to exclude.
pub(crate) async fn drain<E: OpExecutor>(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    exec: &mut E,
    now: i64,
) -> Result<DrainResult> {
    let mut result = DrainResult::default();
    recover_orphaned_settles(store, account)?;
    // The queue before the settling rows: a row committed after this read
    // has a larger id than every row in `queued`, so it can defer none of
    // them, while one committed between the two reads would be missed the
    // other way round.
    let queued = queued_ops(store, account)?;
    // The oldest op still owed on each message: the settling ones now, and
    // every queued one this pass leaves owed as the pass reaches it. Rows run
    // oldest first, so a queued row only ever waits for a smaller id.
    let mut owed = settling_targets(store, account)?;
    for listed in queued {
        // Read again, since the list is a snapshot: a refusal on its message
        // since then, this pass's or a caller's settle, may have parked the
        // row or rewritten its rollback.
        let row = match row_by_id(store, listed.id) {
            Ok(Some(row)) if row.state == STATE_QUEUED => row,
            Ok(_) => continue,
            Err(e) => {
                warn!("[pending_ops] could not read op {} again: {e:#}", listed.id);
                result.still_open += 1;
                continue;
            }
        };
        let waits = row
            .target_message_id
            .and_then(|target| owed.get(&target))
            .is_some_and(|&oldest| oldest < row.id);
        if waits {
            result.still_open += 1;
            continue;
        }
        if row.updated + backoff_secs(row.attempts) > now {
            still_owed(&mut owed, &row);
            result.still_open += 1;
            continue;
        }
        // One row's bookkeeping error is logged and the drain moves on: a `?`
        // here would stop every later row from ever being drained.
        let outcome = if row.op.is_draft_op() {
            exec.execute_draft(store, &row.op).await
        } else {
            exec.execute(&row.op).await
        };
        match outcome {
            Ok(()) => {
                if let Err(e) = retire(store, row.id) {
                    warn!("[pending_ops] could not retire op {}: {e:#}", row.id);
                }
                result.completed += 1;
            }
            Err(e) if crate::ops::NotFoundOnServer::is_in(&e) => {
                // The op already landed before the crash: the message is gone
                // from the source, which is exactly where a successful op
                // leaves it. Retire the row, do not roll the local state back.
                info!(
                    "[pending_ops] op {} ({}) target already gone on server; converged",
                    row.id, row.kind
                );
                if let Err(e) = retire(store, row.id) {
                    warn!("[pending_ops] could not retire op {}: {e:#}", row.id);
                }
                result.completed += 1;
            }
            Err(e) => {
                let err = format!("{e:#}");
                if row.attempts + 1 >= MAX_ATTEMPTS {
                    match refuse(store, blobs, row.id, &err, Refusal::Park) {
                        Ok(later) => result.failed += later.len(),
                        Err(e) => {
                            warn!("[pending_ops] could not settle failed op {}: {e:#}", row.id)
                        }
                    }
                    // A drafts-mirror push retried after its cool-down that
                    // fails again for the same bytes replaces its old failed
                    // row and is not counted, so it is not announced twice.
                    let repeat = matches!(row.op, ServerOp::PushDraft { .. })
                        && supersede_failed_pushes(store, account, row.id).unwrap_or_else(|e| {
                            warn!(
                                "[pending_ops] could not settle the failed pushes beside op {}: {e:#}",
                                row.id
                            );
                            false
                        });
                    if !repeat {
                        result.failed += 1;
                    }
                } else {
                    if let Err(e) = bump_attempt(store, row.id, &err) {
                        warn!(
                            "[pending_ops] could not record a failed attempt on op {}: {e:#}",
                            row.id
                        );
                    }
                    still_owed(&mut owed, &row);
                    result.still_open += 1;
                }
            }
        }
    }
    Ok(result)
}

/// The oldest `settling` op id of every message of `account` a caller is
/// settling an op on, keyed by `target_message_id` (PERSO-236).
///
/// The `messages` row id is the key because it is what names one message
/// across its ops: a move keeps the row id and changes only its mailbox and
/// uid, so a move, a star and a second move of one message all carry the
/// same target, while the `Message-ID` in the [`ServerOp`] is shared by every
/// copy of a message in other folders. A drafts-mirror op has no target and
/// is never deferred, and a delete defers nothing ([`orders_later_ops`]).
fn settling_targets(store: &Store, account: &str) -> Result<HashMap<i64, i64>> {
    let mut stmt = store.conn().prepare(
        "SELECT target_message_id, MIN(id) FROM pending_ops
         WHERE account = ?1 AND state = ?2 AND target_message_id IS NOT NULL AND kind <> ?3
         GROUP BY target_message_id",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![account, STATE_SETTLING, KIND_DELETE],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    rows.collect::<rusqlite::Result<HashMap<i64, i64>>>()
        .context("listing the messages a caller is settling an op on")
}

/// The `kind` of a [`ServerOp::Delete`] row.
const KIND_DELETE: &str = "delete";

/// Whether the ops committed after `op` on its target row are ops on the same
/// message, which wait for it and are rolled back with it (PERSO-243).
///
/// Not after a delete: it removes the row, so nothing can follow it on its
/// own message, and `messages.id` is an `INTEGER PRIMARY KEY` without
/// `AUTOINCREMENT`, which SQLite hands to the next insert when the deleted
/// row had the largest id. An op committed later with the same target is on
/// a new mail that reused the id, and owes nothing to the delete: holding it
/// behind one in backoff, or rolling it back with one refused for good,
/// would stall or undo a change to another message.
fn orders_later_ops(op: &ServerOp) -> bool {
    !matches!(op, ServerOp::Delete { .. })
}

/// Record that `row` stays owed after this drain pass, so the later rows on
/// its message wait for it (PERSO-243).
fn still_owed(owed: &mut HashMap<i64, i64>, row: &PendingOp) {
    if !orders_later_ops(&row.op) {
        return;
    }
    if let Some(target) = row.target_message_id {
        owed.entry(target)
            .and_modify(|oldest| *oldest = (*oldest).min(row.id))
            .or_insert(row.id);
    }
}

/// What becomes of the row of an op refused for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The drain's: the row is parked `failed`, the refused attempt counted,
    /// and parked even when its rollback fails, since a rollback that fails
    /// the same way every tick would otherwise keep the op `queued` for good.
    Park,
    /// A caller's settle: the row is discarded and the caller reports the
    /// error, so a rollback that fails is the caller's error and leaves the
    /// row as it was.
    Discard,
}

/// Undo op `id`, refused for good, settle its row as `how` says, and, when it
/// is a move, first roll back and park `failed` the later ops still owed on
/// its message, newest first, returning their ids (PERSO-243).
///
/// The later ops were committed against the local state the refused move
/// left: a star or a second move names the folder it put the message in,
/// and its rollback restores the state just before it. A refused flag
/// toggle, a delete or a post-send flag changes no folder, so the later ops
/// stand as committed and run in order once it is parked; a flag toggle's
/// rollback restores only its own bit ([`roll_flag_back`]). Once the refused
/// op is undone that state never existed on the server, so none of them can
/// run as committed (a star would search a folder the message never reached
/// and be retired as a converged not-found), and only undoing them newest
/// first lands the message where it was before the refused op, which is
/// where the server still has it. None of them has run: the drain holds a
/// `queued` one behind the refused op and a caller waits with a `settling`
/// one, so no attempt is counted, and a waiting caller finds its row parked
/// and runs nothing ([`wait_for_older`]).
///
/// The listing, every rollback and the refused op's own settle are one
/// immediate transaction (PERSO-249). A daemon command on another
/// connection commits its op in an immediate transaction of its own, which
/// reads the row there, so it lands either before this one, where the
/// listing finds it, or after, against the state the rollbacks restored.
/// Separate transactions left it a window between the listing and the
/// rollbacks to commit an op against the moved state that nothing rolled
/// back, which then ran against a folder the server never got and was
/// retired as a converged not-found.
///
/// The row is read again here rather than trusted from the caller, whose
/// copy may predate a refusal that already settled it: a row that is no
/// longer owed is left alone, except that a caller's row an older refusal
/// parked is discarded, as its caller reports it.
fn refuse(store: &Store, blobs: &BlobStore, id: i64, err: &str, how: Refusal) -> Result<Vec<i64>> {
    let mut tx = store
        .immediate_transaction()
        .context("opening the refusal transaction")?;
    let Some(row) = row_in(&tx, id)? else {
        return Ok(Vec::new());
    };
    let owed = match how {
        Refusal::Park => row.state == STATE_QUEUED,
        Refusal::Discard => row.state == STATE_QUEUED || row.state == STATE_SETTLING,
    };
    if !owed {
        if how == Refusal::Discard && row.state == STATE_FAILED {
            tx.execute("DELETE FROM pending_ops WHERE id = ?1", [id])
                .context("discarding an op an earlier refusal rolled back")?;
            tx.commit().context("committing the discard")?;
        }
        return Ok(Vec::new());
    }
    // Only a move puts the later ops on a state its rollback undoes: they
    // name the folder it put the message in. After a refused flag toggle
    // they stand as committed and run once it is parked (PERSO-243).
    let later = match (how, &row.op) {
        (Refusal::Park, ServerOp::Move { .. }) => later_on_message(&tx, &row)?,
        _ => Vec::new(),
    };
    #[cfg(test)]
    tests::AFTER_LISTING.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
    let why = format!(
        "not run: an earlier change to this message (op {}, {}) failed and was rolled back \
         ({err})",
        row.id, row.kind
    );
    let mut parked = Vec::with_capacity(later.len());
    for op in later.iter().rev() {
        let note = noted(&why, undo(&mut tx, blobs, op), op);
        park(&tx, op, &note, false)?;
        parked.push(op.id);
    }
    match how {
        Refusal::Park => {
            let note = noted(err, undo(&mut tx, blobs, &row), &row);
            park(&tx, &row, &note, true)?;
        }
        Refusal::Discard => {
            undo(&mut tx, blobs, &row)?;
            tx.execute("DELETE FROM pending_ops WHERE id = ?1", [id])
                .context("discarding a refused op")?;
        }
    }
    tx.commit().context("committing the refusal")?;
    Ok(parked)
}

/// The ops still owed on `row`'s message that were committed after it,
/// oldest first: `queued` ones the drain holds behind it and `settling` ones
/// whose callers wait for it.
///
/// They end at the first delete among them, which is the message's last op:
/// what follows it on the row is on a new mail that reused the row id
/// ([`orders_later_ops`]).
fn later_on_message(conn: &Connection, row: &PendingOp) -> Result<Vec<PendingOp>> {
    let Some(target) = row.target_message_id.filter(|_| orders_later_ops(&row.op)) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT id, account, kind, target_message_id, payload, state, attempts,
                last_error, created, updated
         FROM pending_ops
         WHERE account = ?1 AND target_message_id = ?2 AND id > ?3 AND state IN (?4, ?5)
         ORDER BY id",
    )?;
    let mut later = stmt
        .query_map(
            rusqlite::params![row.account, target, row.id, STATE_QUEUED, STATE_SETTLING],
            row_from_sql,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("listing the ops owed after a refused one on its message")?;
    let end = later
        .iter()
        .position(|op| !orders_later_ops(&op.op))
        .map_or(later.len(), |delete| delete + 1);
    later.truncate(end);
    Ok(later)
}

/// Undo `row`'s local half under a savepoint of `tx`, so a rollback that
/// fails part way leaves nothing of itself behind.
fn undo(tx: &mut rusqlite::Transaction<'_>, blobs: &BlobStore, row: &PendingOp) -> Result<()> {
    let savepoint = tx.savepoint().context("opening a rollback savepoint")?;
    apply_rollback(&savepoint, blobs, row)?;
    savepoint.commit().context("releasing a rollback savepoint")
}

/// `err`, with the rollback's own failure appended when it had one.
fn noted(err: &str, undone: Result<()>, row: &PendingOp) -> String {
    match undone {
        Ok(()) => err.to_string(),
        Err(e) => {
            warn!(
                "[pending_ops] op {} ({}) could not be rolled back: {e:#}",
                row.id, row.kind
            );
            format!("{err} (rollback failed: {e:#})")
        }
    }
}

/// Park `row` as `failed` with `err`, counting the attempt that was refused
/// when `attempted`.
fn park(conn: &Connection, row: &PendingOp, err: &str, attempted: bool) -> Result<()> {
    conn.execute(
        "UPDATE pending_ops SET state = ?2, attempts = attempts + ?5, last_error = ?3,
         updated = ?4 WHERE id = ?1",
        rusqlite::params![row.id, STATE_FAILED, err, unix_now(), i64::from(attempted)],
    )
    .context("parking a pending op as failed")?;
    warn!(
        "[pending_ops] op {} ({}) failed and was rolled back: {err}",
        row.id, row.kind
    );
    Ok(())
}

/// Take the engine lock and drain, or do nothing when another process holds it.
///
/// The live entry point: gating the drain on [`crate::engine_lock`] is what
/// keeps two processes (an open TUI and a `mp sync`) from running the same op
/// twice or racing a row's transitions (#0061). `Ok(None)` means another
/// process is the engine and is draining this account; the work still happens,
/// there.
pub async fn drain_account<E: OpExecutor>(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    backend: &mut E,
) -> Result<Option<DrainResult>> {
    drain_account_or_busy(store, blobs, account, backend)
        .await
        .map(Resume::into_drained)
}

/// What one attempt at the mutation queue came to, for a caller that has to
/// tell a refused turn from an empty queue (#0133).
///
/// [`resume_account`] and [`drain_account`] fold the first two into one
/// `Ok(None)`, which is right for a caller that only reports: either way it
/// drained nothing. The daemon's debounced drainer is not such a caller. A turn
/// refused to it (the post-send outbox drain, a guarded `mp sync --mailbox`)
/// leaves an op queued that nothing else is about to drain, so it has to try
/// again, while an empty queue is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    /// No row was owed, or the account has no store: nothing to do.
    NothingQueued,
    /// Rows were owed and the engine turn was refused, or the lock could not
    /// be asked for at all: they are still queued.
    Busy,
    /// The drain ran under the engine turn.
    Drained(DrainResult),
}

impl Resume {
    /// The `Option` the older entry points return: a drain that ran, or none.
    pub fn into_drained(self) -> Option<DrainResult> {
        match self {
            Resume::Drained(result) => Some(result),
            Resume::NothingQueued | Resume::Busy => None,
        }
    }
}

/// [`drain_account`], telling a refused turn apart from a drain that ran.
pub async fn drain_account_or_busy<E: OpExecutor>(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    backend: &mut E,
) -> Result<Resume> {
    match crate::engine_lock::EngineLock::take_turn(account) {
        Ok(Some(_lock)) => {
            let result = drain(store, blobs, account, backend, unix_now()).await?;
            Ok(Resume::Drained(result))
        }
        Ok(None) => Ok(Resume::Busy),
        Err(e) => {
            warn!("[pending_ops] no engine lock for {account}, not draining: {e:#}");
            Ok(Resume::Busy)
        }
    }
}

/// Resume the mutation queue for one account: the startup and sync-tick entry
/// point, the mutation twin of [`crate::send::resume_outbox`].
///
/// It builds the backend and takes the engine lock **only when a row is owed**,
/// so a clean account costs one cheap `COUNT` and no server traffic: the ticket
/// asks for no sync traffic against live accounts beyond draining actually
/// queued ops (#0039). `Ok(None)` means there was nothing to drain, or another
/// process holds the engine lock and is draining this account instead.
pub async fn resume_account(account: &AccountConfig) -> Result<Option<DrainResult>> {
    resume_account_or_busy(account)
        .await
        .map(Resume::into_drained)
}

/// [`resume_account`], telling an empty queue apart from a refused turn
/// ([`Resume`]).
pub async fn resume_account_or_busy(account: &AccountConfig) -> Result<Resume> {
    let path = crate::config::store_path(&account.name);
    if !path.exists() {
        return Ok(Resume::NothingQueued);
    }
    let store = Store::open(&path)?;
    if owed_ops(&store, account)? == 0 {
        return Ok(Resume::NothingQueued);
    }
    let blobs = BlobStore::for_account(&account.name);
    let mut executor = AccountExecutor::resolve(account)?;
    drain_account_or_busy(&store, &blobs, &account.name, &mut executor).await
}

/// How many ops of `account` a drain would find `queued` in `store`, once
/// what is owed but not yet queued has joined them: the cheap gate that
/// keeps a clean account off the server.
fn owed_ops(store: &Store, account: &AccountConfig) -> Result<usize> {
    // The drafts mirror's owed pushes join the queue here, so every drain
    // point carries a save the watcher saw, or one nobody saw while no daemon
    // ran (#0143).
    if let Err(e) = crate::draft_mirror::push::enqueue_owed(store, account) {
        warn!(
            "[drafts] could not queue the owed pushes of {}: {e:#}",
            account.name
        );
    }
    // Before the count, so an op a caller left `settling` is owed here too.
    recover_orphaned_settles(store, &account.name)?;
    let (queued, _failed) = counts(store, &account.name)?;
    Ok(queued)
}

/// How long a caller settling an op waits for the older ops on its message to
/// reach the server, counted only while the account's engine turn is free,
/// before it gives up on its own (PERSO-241).
///
/// The drain that runs them is the debounced one
/// ([`crate::daemon::runtime::drainer`], 1.5 s of quiet) or a sync tick's, and
/// both need the account's turn. A tick holds that turn for its whole body,
/// which is unbounded (a full sync was seen at 160 s), and drains only at its
/// head and its tail: an op queued after the head drain waits for the tail or
/// for the debounced drain after the tick. That wait is the tick's, not the
/// queue's, so it spends none of this patience ([`OlderOps::paused_while`]);
/// what is left measures a drain that could run and has not, long enough for
/// the debounce and a few of its refused-turn retries.
pub const SETTLE_PATIENCE: Duration = Duration::from_secs(60);

/// How long a caller settling an op waits for the older ops on its message in
/// all, a held turn included (PERSO-241).
///
/// The bound a held turn needs once it stops spending [`SETTLE_PATIENCE`]: a
/// tick that never ends would otherwise keep the caller waiting forever. Short
/// against the five minutes the CLI gives a routed mutation, so the refusal
/// reaches the user as an answer and not as a client timeout.
pub const SETTLE_CEILING: Duration = Duration::from_secs(240);

/// How often a waiting caller looks at the queue again.
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// How a caller settling an op waits for the older ops on its message
/// (PERSO-241): how long, how it asks for the drain that runs them, and how it
/// tells that the drain cannot run yet.
pub struct OlderOps<'a> {
    patience: Duration,
    ceiling: Duration,
    nudge: &'a dyn Fn(),
    turn_held: Option<&'a dyn Fn() -> bool>,
}

impl<'a> OlderOps<'a> {
    /// Wait up to [`SETTLE_PATIENCE`], calling `nudge` once to ask the
    /// account's drainer to run them.
    pub fn wait(nudge: &'a dyn Fn()) -> OlderOps<'a> {
        OlderOps::wait_for(SETTLE_PATIENCE, nudge)
    }

    /// The same, with a patience of the caller's choosing.
    pub fn wait_for(patience: Duration, nudge: &'a dyn Fn()) -> OlderOps<'a> {
        OlderOps {
            patience,
            ceiling: SETTLE_CEILING,
            nudge,
            turn_held: None,
        }
    }

    /// Spend no patience while `turn_held` says the account's engine turn is
    /// taken, by a sync tick above all, since no drain can run the older ops
    /// until it is free; the wait as a whole still ends at the ceiling.
    pub fn paused_while(self, turn_held: &'a dyn Fn() -> bool) -> OlderOps<'a> {
        OlderOps {
            turn_held: Some(turn_held),
            ..self
        }
    }

    /// End the wait after `ceiling` in all, instead of [`SETTLE_CEILING`].
    pub fn within(self, ceiling: Duration) -> OlderOps<'a> {
        OlderOps { ceiling, ..self }
    }
}

/// Run one just-enqueued op synchronously and settle its row, for a caller that
/// wants the op's own outcome rather than the background drain's convergence
/// (the CLI).
///
/// The op must have been committed for this caller ([`Runner::Caller`] with
/// `scope`), which leaves it `settling` and out of every drain's reach, so a
/// sync tick draining the account meanwhile cannot run it a second time
/// (PERSO-236). An op committed for the drain is refused untouched: it is the
/// drain's to run, and running it here would race it.
///
/// Before its server call it waits for the ops committed earlier on the same
/// message that are still owed ([`older_owed`]), so the server sees the ops on
/// one message in the order they were committed, as the drain already keeps
/// for a `queued` op behind an older `settling` one (PERSO-241). It runs none
/// of them itself: a `queued` op is the drain's, and an older `settling` op
/// another caller's, so each still runs exactly once. It asks for the drain
/// through `older.nudge` and polls. An older op the server has already
/// refused once, or one still owed after `older.patience` of a free turn or
/// after its ceiling, makes the caller give up instead: its own op is rolled back and discarded unrun, like a
/// server refusal, and the error says why.
///
/// On success the row is retired; on any failure the local half is rolled back,
/// the row is discarded, and the error is returned verbatim, so a
/// [`crate::ops::NotFoundOnServer`] reaches the user byte-identical to the
/// pre-queue path. Unlike [`drain`] this does **not** converge a not-found: a
/// synchronous caller runs the op once, in the same process that enqueued it, so
/// it is never a crash replay, and a message the server does not hold is a
/// genuine error to report rather than a converged replay to swallow.
pub async fn run_and_settle<E: OpExecutor>(
    store: &Store,
    blobs: &BlobStore,
    scope: &SettleScope,
    op_id: i64,
    exec: &mut E,
    older: OlderOps<'_>,
) -> Result<()> {
    scope.covers(store)?;
    let row = row_by_id(store, op_id)?
        .ok_or_else(|| anyhow::anyhow!("pending op {op_id} vanished before it could run"))?;
    // A `failed` row was rolled back with an older op refused for good on
    // its message, which [`wait_for_older`] reports (PERSO-249).
    anyhow::ensure!(
        row.state == STATE_SETTLING || row.state == STATE_FAILED,
        "pending op {op_id} is {}, not settling: it is the drain's to run",
        row.state
    );
    if let Err(refusal) = wait_for_older(store, &row, &older).await {
        return settle(store, blobs, &row, Err(refusal));
    }
    let outcome = exec.execute(&row.op).await;
    settle(store, blobs, &row, outcome)
}

/// Wait until no op committed before `row` on its message is still owed, or
/// say why the caller should not wait any longer (PERSO-241).
///
/// An older op the server refused once is not waited for: its retry waits out
/// a backoff of thirty seconds or more, and if it is refused for good its
/// rollback restores the message to where it was before it, under the op this
/// caller would have run. Catching it at its first refusal also means an older
/// op is never seen parked `failed` here, since parking takes
/// [`MAX_ATTEMPTS`] refusals; an op parked `failed` before `row` was committed
/// is not owed and holds nothing back, because its rollback already ran.
///
/// The patience is spent only while the account's turn is free
/// ([`OlderOps::paused_while`]): each poll interval counts when the turn was
/// free at its end, so a tick holding the turn for minutes costs none of it.
///
/// An older op refused for good rolls `row` back with it and parks it
/// `failed`, in the transaction that takes the older op off the queue
/// ([`refuse`], PERSO-249), so `row` is read again after the older ops at
/// every poll: an older op is no longer owed only once that transaction has
/// committed, and a row it parked is not run against the state it undid.
async fn wait_for_older(store: &Store, row: &PendingOp, older: &OlderOps<'_>) -> Result<()> {
    let Some(target) = row.target_message_id else {
        return Ok(());
    };
    let started = tokio::time::Instant::now();
    let mut checked = started;
    let mut spent = Duration::ZERO;
    let mut nudged = false;
    loop {
        let ahead = older_owed(store, &row.account, target, row.id)?;
        match row_by_id(store, row.id)? {
            Some(own) if own.state == STATE_SETTLING => {}
            Some(own) => anyhow::bail!(
                "{}; nothing was changed",
                own.last_error
                    .as_deref()
                    .unwrap_or("an earlier change to this message failed and this one was undone")
            ),
            None => anyhow::bail!("pending op {} vanished before it could run", row.id),
        }
        let Some(first) = ahead.first() else {
            return Ok(());
        };
        if let Some(refused) = ahead.iter().find(|op| op.attempts > 0) {
            anyhow::bail!(
                "an earlier change to this message (op {}, {}) was refused by the server and \
                 is waiting to retry ({}); nothing was changed, try again once it has run",
                refused.id,
                refused.kind,
                refused.last_error.as_deref().unwrap_or("no error recorded")
            );
        }
        let now = tokio::time::Instant::now();
        if !older.turn_held.is_some_and(|held| held()) {
            spent += now - checked;
        }
        checked = now;
        if spent >= older.patience || now - started >= older.ceiling {
            anyhow::bail!(
                "an earlier change to this message (op {}, {}) has not reached the server yet; \
                 nothing was changed, try again in a moment",
                first.id,
                first.kind
            );
        }
        if !nudged {
            info!(
                "[pending_ops] op {} waits for {} older op(s) on its message",
                row.id,
                ahead.len()
            );
            (older.nudge)();
            nudged = true;
        }
        let left = older
            .patience
            .saturating_sub(spent)
            .min(older.ceiling.saturating_sub(now - started));
        tokio::time::sleep(SETTLE_POLL.min(left)).await;
    }
}

/// The ops of `account` committed before op `before` on message `target` that
/// are still owed to the server, oldest first: `queued` ones the drain runs and
/// `settling` ones another caller is running (PERSO-241). A delete is left
/// out: an op after it on its row is on another message
/// ([`orders_later_ops`]).
fn older_owed(store: &Store, account: &str, target: i64, before: i64) -> Result<Vec<PendingOp>> {
    let mut stmt = store.conn().prepare(
        "SELECT id, account, kind, target_message_id, payload, state, attempts,
                last_error, created, updated
         FROM pending_ops
         WHERE account = ?1 AND target_message_id = ?2 AND id < ?3 AND state IN (?4, ?5)
           AND kind <> ?6
         ORDER BY id",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![
            account,
            target,
            before,
            STATE_QUEUED,
            STATE_SETTLING,
            KIND_DELETE
        ],
        row_from_sql,
    )?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("listing the older ops owed on a message")
}

/// Retire a settled op on success, or roll its local half back and discard it
/// on failure, returning the error verbatim.
///
/// Split from [`run_and_settle`] so the settle policy is unit-testable without a
/// live backend: the execution is one `await`, this is the whole decision.
fn settle(store: &Store, blobs: &BlobStore, row: &PendingOp, outcome: Result<()>) -> Result<()> {
    match outcome {
        Ok(()) => {
            retire(store, row.id)?;
            Ok(())
        }
        Err(e) => {
            refuse(store, blobs, row.id, &format!("{e:#}"), Refusal::Discard)?;
            Err(e)
        }
    }
}

/// One decoded row by its `pending_ops.id`.
fn row_by_id(store: &Store, id: i64) -> Result<Option<PendingOp>> {
    row_in(store.conn(), id)
}

/// [`row_by_id`], on a connection inside a transaction.
fn row_in(conn: &Connection, id: i64) -> Result<Option<PendingOp>> {
    conn.query_row(
        "SELECT id, account, kind, target_message_id, payload, state, attempts,
                    last_error, created, updated
             FROM pending_ops WHERE id = ?1",
        [id],
        row_from_sql,
    )
    .optional()
    .context("reading a pending op by id")
}

/// Retire a row whose op succeeded. A done mutation has nothing to say, so the
/// row is deleted rather than parked.
fn retire(store: &Store, id: i64) -> Result<()> {
    store
        .conn()
        .execute("DELETE FROM pending_ops WHERE id = ?1", [id])
        .context("retiring a completed pending op")?;
    Ok(())
}

/// Record one more failed attempt on a still-retryable row.
fn bump_attempt(store: &Store, id: i64, err: &str) -> Result<()> {
    store
        .conn()
        .execute(
            "UPDATE pending_ops SET attempts = attempts + 1, last_error = ?2, updated = ?3
             WHERE id = ?1",
            rusqlite::params![id, err, unix_now()],
        )
        .context("recording a failed pending-op attempt")?;
    Ok(())
}

/// Undo a mutation's local half, inside the caller's transaction.
fn apply_rollback(conn: &Connection, blobs: &BlobStore, row: &PendingOp) -> Result<()> {
    match &row.rollback {
        Rollback::Move(previous) => roll_move_back(conn, blobs, previous)?,
        Rollback::Flags { id, flags } => roll_flag_back(conn, row, *id, flags)?,
        // A delete cannot be restored: the row is gone and the message is still
        // on the server, so convergence is the next sync refetching the UID.
        Rollback::None => {}
    }
    Ok(())
}

/// The one flag a read or star toggle changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlagBit {
    Seen,
    Flagged,
}

impl FlagBit {
    /// The flag `op` changes, `None` for an op that toggles none.
    fn of(op: &ServerOp) -> Option<FlagBit> {
        match op {
            ServerOp::SetRead { .. } => Some(FlagBit::Seen),
            ServerOp::SetFlagged { .. } => Some(FlagBit::Flagged),
            _ => None,
        }
    }

    fn get(self, flags: MessageFlags) -> bool {
        match self {
            FlagBit::Seen => flags.seen,
            FlagBit::Flagged => flags.flagged,
        }
    }

    fn set(self, flags: MessageFlags, value: bool) -> MessageFlags {
        match self {
            FlagBit::Seen => flags.with_seen(value),
            FlagBit::Flagged => flags.with_flagged(value),
        }
    }
}

/// Undo a read or star toggle: put back the one bit its op changed, as it
/// stood in `old`, the row's flag string before the op (PERSO-243).
///
/// The rest of the string is left as it is now: a refused star no longer
/// rolls back with the other ops on its message, so a reply sent meanwhile
/// may have written `\Answered` since, which the whole of `old` would wipe.
///
/// A later op still owed on the same flag of the message has since written
/// the local bit, and it is what the server gets once it runs, so the bit
/// is left to it; that op's own rollback is pointed at the value from
/// before the refused one instead, the value the server keeps if it is
/// refused in turn, since it was taken over a write the server never got.
fn roll_flag_back(conn: &Connection, row: &PendingOp, id: i64, old: &str) -> Result<()> {
    let Some(bit) = FlagBit::of(&row.op) else {
        conn.execute(
            "UPDATE messages SET flags = ?2 WHERE id = ?1",
            rusqlite::params![id, old],
        )
        .context("rolling a flag change back")?;
        return Ok(());
    };
    let before = bit.get(MessageFlags::parse(old));
    let next = later_on_message(conn, row)?
        .into_iter()
        .find(|later| FlagBit::of(&later.op) == Some(bit));
    if let Some(next) = next {
        if let Rollback::Flags { id, flags } = &next.rollback {
            let flags = bit.set(MessageFlags::parse(flags), before).to_flag_string();
            return rewrite_rollback(conn, &next, &Rollback::Flags { id: *id, flags });
        }
    }
    let current: Option<Option<String>> = conn
        .query_row("SELECT flags FROM messages WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .context("reading a message's flags to roll a change back")?;
    let Some(current) = current else {
        return Ok(());
    };
    let restored = bit
        .set(
            MessageFlags::parse(current.as_deref().unwrap_or_default()),
            before,
        )
        .to_flag_string();
    conn.execute(
        "UPDATE messages SET flags = ?2 WHERE id = ?1",
        rusqlite::params![id, restored],
    )
    .context("rolling a flag change back")?;
    Ok(())
}

/// Replace the rollback of the owed op `row` with `rollback`.
fn rewrite_rollback(conn: &Connection, row: &PendingOp, rollback: &Rollback) -> Result<()> {
    let payload = serde_json::to_string(&Payload {
        op: row.op.clone(),
        rollback: rollback.clone(),
    })
    .context("encoding the pending op payload")?;
    conn.execute(
        "UPDATE pending_ops SET payload = ?1 WHERE id = ?2",
        rusqlite::params![payload, row.id],
    )
    .context("pointing a later op's rollback past a refused one")?;
    Ok(())
}

/// Put a moved row back on its old `(mailbox, uid)`, or drop it when a sync has
/// meanwhile refetched the message onto that slot.
///
/// While a move is queued the source mailbox keeps syncing, and the sync no
/// longer sees the UID in its skip list, so it downloads the message again
/// into a row of its own. Moving the placeholder back would then collide on
/// `UNIQUE (account, mailbox, uid)`; the refetched row already stands for the
/// message, so the placeholder is the duplicate and goes.
///
/// Runs inside the caller's transaction ([`refuse`]), under a savepoint of
/// its own.
fn roll_move_back(tx: &Connection, blobs: &BlobStore, previous: &MutatedRow) -> Result<()> {
    let holder: Option<i64> = tx
        .query_row(
            "SELECT other.id FROM messages AS other
             JOIN messages AS moved ON moved.id = ?1
             WHERE other.account = moved.account AND other.mailbox = ?2 AND other.uid = ?3
               AND other.id != ?1",
            rusqlite::params![previous.id, previous.mailbox, previous.uid],
            |row| row.get(0),
        )
        .optional()
        .context("checking whether the old slot was refetched")?;
    match holder {
        None => {
            tx.execute(
                "UPDATE messages SET mailbox = ?2, uid = ?3 WHERE id = ?1",
                rusqlite::params![previous.id, previous.mailbox, previous.uid],
            )
            .context("rolling a moved row back")?;
        }
        Some(holder) => {
            let hashes = blob_refs_in(tx, previous.id)?;
            tx.execute("DELETE FROM messages_fts WHERE rowid = ?1", [previous.id])
                .context("removing the placeholder's FTS entry")?;
            tx.execute("DELETE FROM messages WHERE id = ?1", [previous.id])
                .context("deleting the moved placeholder row")?;
            for hash in &hashes {
                blobs.release(tx, hash)?;
            }
            info!(
                "[pending_ops] row {} was refetched as row {holder} while its move was queued; \
                 dropped the placeholder instead of moving it back",
                previous.id
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Transaction-scoped helpers (no auto-commit, so they compose with `enqueue`)
// ---------------------------------------------------------------------------

/// A row's coordinates, read inside a transaction.
fn coordinates_in(conn: &Connection, id: i64) -> Result<Option<MutatedRow>> {
    conn.query_row(
        "SELECT id, message_id, mailbox, uid FROM messages WHERE id = ?1",
        [id],
        |row| {
            Ok(MutatedRow {
                id: row.get(0)?,
                message_id: row.get(1)?,
                mailbox: row.get(2)?,
                uid: row.get(3)?,
            })
        },
    )
    .optional()
    .context("reading a message row's coordinates")
}

/// Every blob hash a row references, read inside a transaction.
fn blob_refs_in(conn: &Connection, id: i64) -> Result<Vec<crate::store::blobs::BlobHash>> {
    let mut stmt = conn.prepare("SELECT hash FROM message_blobs WHERE message_row = ?1")?;
    let rows = stmt.query_map([id], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for hash in rows {
        match crate::store::blobs::BlobHash::parse(&hash?) {
            Ok(h) => out.push(h),
            Err(e) => warn!("[pending_ops] ignoring unparseable blob reference on delete: {e:#}"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::tests::{fixture, invite_ics};
    use crate::store::read;
    use std::collections::VecDeque;

    thread_local! {
        /// Run by [`refuse`] on this thread once it has listed the later ops
        /// on the refused op's message and before it rolls any back, so a
        /// test can try to commit an op into that window (PERSO-249).
        pub(super) static AFTER_LISTING: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// A fake executor with a scripted verdict per call, recording every op it
    /// was asked to run so a test can assert exactly-once and idempotent replay.
    struct FakeExecutor {
        verdicts: VecDeque<Result<()>>,
        seen: Vec<ServerOp>,
    }

    impl FakeExecutor {
        fn always_ok() -> Self {
            FakeExecutor {
                verdicts: VecDeque::new(),
                seen: Vec::new(),
            }
        }
        fn scripted(verdicts: Vec<Result<()>>) -> Self {
            FakeExecutor {
                verdicts: verdicts.into(),
                seen: Vec::new(),
            }
        }
    }

    impl OpExecutor for FakeExecutor {
        async fn execute(&mut self, op: &ServerOp) -> Result<()> {
            self.seen.push(op.clone());
            self.verdicts.pop_front().unwrap_or(Ok(()))
        }
    }

    /// A nudge nobody answers: the settle under test has no older op to wait
    /// for, or the test drains by hand.
    fn nobody() {}

    fn move_op(message_id: &str) -> ServerOp {
        ServerOp::Move {
            message_id: message_id.to_string(),
            source_mailbox: "INBOX".to_string(),
            dest_mailbox: "Archive".to_string(),
            copy: None,
            graph_copy: None,
        }
    }

    /// The typed not-found a real backend returns when the op's server half
    /// already landed and the message is gone from the source folder. Scripting
    /// this into the fake is what exercises the drain's convergence contract
    /// rather than an assumed-idempotent backend.
    fn not_found(message_id: &str) -> Result<()> {
        Err(crate::ops::NotFoundOnServer {
            message_id: message_id.to_string(),
            mailbox: Some("INBOX".to_string()),
        }
        .into())
    }

    /// Applying a move writes the row *and* the queue row in one transaction:
    /// the store shows the move, and a queued op is durably recorded.
    #[test]
    fn apply_move_commits_the_row_and_the_op_together() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");

        let (previous, _op_id) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();

        assert_eq!(previous.mailbox, "inbox");
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "archive"
        );
        let queued = queued_ops(&fx.store, "alice").unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].kind, "move");
        assert_eq!(queued[0].target_message_id, Some(id));
        assert_eq!(queued[0].op, move_op("<inbox-1@example.com>"));
    }

    /// A successful drain retires the row: the store keeps the move and the
    /// queue is empty, with the server op run exactly once.
    #[tokio::test]
    async fn a_successful_drain_retires_the_row() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap();

        let mut exec = FakeExecutor::always_ok();
        let result = drain(&fx.store, &fx.blobs, "alice", &mut exec, unix_now() + 10)
            .await
            .unwrap();

        assert_eq!(result.completed, 1);
        assert_eq!(exec.seen.len(), 1, "the op ran exactly once");
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "archive"
        );
    }

    /// Crash-safety with the real not-found contract: the server move landed
    /// before the crash, so on restart the replay finds the message already
    /// gone from the source. The backend reports [`crate::ops::NotFoundOnServer`],
    /// and the drain must **converge** (retire the row, keep the optimistic
    /// move), not fail the succeeded op and roll it back. This is the #0039
    /// review blocker: a scripted `Ok` hides it, so the fake returns the true
    /// not-found here.
    #[tokio::test]
    async fn a_queued_op_replays_after_a_simulated_crash() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap();

        // The "crash": the process died after the server move but before the
        // drain retired the row. On restart the row is still queued and the
        // store still shows the optimistic move.
        assert_eq!(queued_ops(&fx.store, "alice").unwrap().len(), 1);
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "archive"
        );

        let mut exec = FakeExecutor::scripted(vec![not_found("<inbox-1@example.com>")]);
        let result = drain(&fx.store, &fx.blobs, "alice", &mut exec, unix_now() + 10)
            .await
            .unwrap();

        assert_eq!(exec.seen.len(), 1, "replay runs the op once");
        assert_eq!(
            result.completed, 1,
            "a not-found replay converges, it does not fail"
        );
        assert_eq!(result.failed, 0);
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "archive",
            "a converged replay keeps the move; it is not rolled back"
        );
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
        assert!(failed_ops(&fx.store, "alice").unwrap().is_empty());
    }

    /// The discrimination the blocker turns on: a single not-found converges on
    /// the first attempt (a succeeded op that crashed before retiring), while a
    /// genuine error is retried and rolled back once the budget is spent. One
    /// test pins both so a future change cannot make the drain swallow real
    /// failures as "converged".
    #[tokio::test]
    async fn not_found_converges_but_a_genuine_error_still_rolls_back() {
        // Not-found: converges at once, no rollback, no failed row.
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap();
        let mut exec = FakeExecutor::scripted(vec![not_found("<inbox-1@example.com>")]);
        let r = drain(&fx.store, &fx.blobs, "alice", &mut exec, unix_now() + 10)
            .await
            .unwrap();
        assert_eq!((r.completed, r.failed), (1, 0));
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "archive"
        );
        assert!(failed_ops(&fx.store, "alice").unwrap().is_empty());

        // Genuine error: retried to the budget, then rolled the row home and
        // parked as failed. A not-found must never be mistaken for this.
        let id2 = fx.ingest_plain("inbox", 2, "Other");
        apply_move(
            &fx.store,
            "alice",
            id2,
            "archive",
            move_op("<inbox-2@example.com>"),
            Runner::Drain,
        )
        .unwrap();
        let base = unix_now();
        for tick in 0..MAX_ATTEMPTS {
            let mut exec = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("NO server refused"))]);
            drain(
                &fx.store,
                &fx.blobs,
                "alice",
                &mut exec,
                base + (tick + 1) * 10_000_000,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            read::find_by_id(&fx.store, id2).unwrap().unwrap().mailbox,
            "inbox",
            "a genuine refusal must still roll the row home"
        );
        assert_eq!(failed_ops(&fx.store, "alice").unwrap().len(), 1);
    }

    /// A transient failure keeps the row queued and backs off: a second drain
    /// at the same instant does not re-attempt it, and one past the backoff
    /// window does.
    #[tokio::test]
    async fn a_transient_failure_backs_off_and_then_retries() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap();

        let base = unix_now();
        let mut exec = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("connection reset"))]);
        let r1 = drain(&fx.store, &fx.blobs, "alice", &mut exec, base + 5)
            .await
            .unwrap();
        assert_eq!(r1.still_open, 1);
        let queued = queued_ops(&fx.store, "alice").unwrap();
        assert_eq!(queued[0].attempts, 1);
        assert!(queued[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("connection reset"));

        // Same instant: backoff (30s after one failure) has not elapsed, so the
        // op is not attempted again.
        let mut exec2 = FakeExecutor::always_ok();
        let r2 = drain(&fx.store, &fx.blobs, "alice", &mut exec2, base + 5)
            .await
            .unwrap();
        assert_eq!(r2.still_open, 1);
        assert!(exec2.seen.is_empty(), "backoff must hold the retry back");

        // Well past the backoff window: it retries and succeeds.
        let mut exec3 = FakeExecutor::always_ok();
        let r3 = drain(&fx.store, &fx.blobs, "alice", &mut exec3, base + 10_000_000)
            .await
            .unwrap();
        assert_eq!(r3.completed, 1);
        assert_eq!(exec3.seen.len(), 1);
    }

    /// A move the server refuses past the retry budget parks the row as failed
    /// and rolls the local row back to where it was.
    #[tokio::test]
    async fn a_refused_move_fails_and_rolls_the_row_home() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap();
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "archive"
        );

        // Fail every attempt; drive the drain past the budget with elapsed
        // backoff each time.
        let base = unix_now();
        let mut failed = false;
        for tick in 0..MAX_ATTEMPTS {
            let mut exec = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("NO server refused"))]);
            let r = drain(
                &fx.store,
                &fx.blobs,
                "alice",
                &mut exec,
                base + (tick + 1) * 10_000_000,
            )
            .await
            .unwrap();
            if r.failed == 1 {
                failed = true;
            }
        }
        assert!(failed, "the op never reached the failed state");

        let row = read::find_by_id(&fx.store, id).unwrap().unwrap();
        assert_eq!(
            row.mailbox, "inbox",
            "a refused move must roll the row home"
        );
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
        let failed_rows = failed_ops(&fx.store, "alice").unwrap();
        assert_eq!(failed_rows.len(), 1);
        assert!(failed_rows[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("refused"));
    }

    /// A move that keeps failing while the source mailbox still syncs: the sync
    /// refetches the message onto its old `(mailbox, uid)`, so the rollback
    /// cannot move the placeholder back without colliding on the unique slot.
    /// It drops the placeholder instead, the op parks as failed, and the
    /// message keeps exactly one row.
    #[tokio::test]
    async fn a_refused_move_whose_slot_was_refetched_drops_the_placeholder() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap();
        // The sync re-downloads the UID the moved row no longer claims.
        let refetched = fx.ingest_plain("inbox", 1, "Receipt");
        assert_ne!(refetched, id, "the refetch is a row of its own");

        let base = unix_now();
        for tick in 0..MAX_ATTEMPTS {
            let mut exec = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("NO no such mailbox"))]);
            drain(
                &fx.store,
                &fx.blobs,
                "alice",
                &mut exec,
                base + (tick + 1) * 10_000_000,
            )
            .await
            .unwrap();
        }

        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
        assert_eq!(failed_ops(&fx.store, "alice").unwrap().len(), 1);
        let rows: i64 = fx
            .store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE message_id = '<inbox-1@example.com>'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1, "one message, one row");
        assert!(
            read::find_by_id(&fx.store, id).unwrap().is_none(),
            "the placeholder is gone"
        );
        assert_eq!(
            read::find_by_id(&fx.store, refetched)
                .unwrap()
                .unwrap()
                .mailbox,
            "inbox"
        );
    }

    /// One row whose settlement errors must not stall the queue: the drain
    /// logs it, parks the op as failed, and still drains the rows after it.
    #[tokio::test]
    async fn a_rollback_error_does_not_stall_the_rest_of_the_queue() {
        let fx = fixture();
        let stuck = fx.ingest_plain("inbox", 1, "Stuck");
        let next = fx.ingest_plain("inbox", 2, "Next");
        apply_move(
            &fx.store,
            "alice",
            stuck,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap();
        // Any rollback of `stuck` errors.
        fx.store
            .conn()
            .execute_batch(&format!(
                "CREATE TRIGGER refuse_rollback BEFORE UPDATE ON messages WHEN OLD.id = {stuck}
                 BEGIN SELECT RAISE(ABORT, 'rollback refused'); END;"
            ))
            .unwrap();

        let base = unix_now();
        for tick in 0..MAX_ATTEMPTS - 1 {
            let mut exec = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("NO refused"))]);
            drain(
                &fx.store,
                &fx.blobs,
                "alice",
                &mut exec,
                base + (tick + 1) * 10_000_000,
            )
            .await
            .unwrap();
        }
        // The second op is queued behind the one about to exhaust its budget.
        apply_move(
            &fx.store,
            "alice",
            next,
            "archive",
            move_op("<inbox-2@example.com>"),
            Runner::Drain,
        )
        .unwrap();
        let mut exec = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("NO refused")), Ok(())]);
        let r = drain(&fx.store, &fx.blobs, "alice", &mut exec, base + 100_000_000)
            .await
            .unwrap();

        assert_eq!((r.failed, r.completed), (1, 1));
        assert_eq!(exec.seen.len(), 2, "the later op was still drained");
        let failed = failed_ops(&fx.store, "alice").unwrap();
        assert_eq!(failed.len(), 1);
        assert!(failed[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("rollback failed"));
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
    }

    /// A read toggle stores the old flag string, so a refused op restores it
    /// without disturbing the orthogonal answered/forwarded bits.
    #[tokio::test]
    async fn a_refused_read_toggle_restores_the_flags() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 2, "Unread");
        crate::store::write::set_answered(&fx.store, id).unwrap();

        let op = ServerOp::SetRead {
            message_id: "<inbox-2@example.com>".to_string(),
            mailbox: "INBOX".to_string(),
            read: true,
            copy: None,
            graph_copy: None,
        };
        apply_set_read(&fx.store, "alice", id, true, op, Runner::Drain).unwrap();
        assert!(read::find_by_id(&fx.store, id).unwrap().unwrap().is_read());

        let base = unix_now();
        for tick in 0..MAX_ATTEMPTS {
            let mut exec = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("STORE rejected"))]);
            drain(
                &fx.store,
                &fx.blobs,
                "alice",
                &mut exec,
                base + (tick + 1) * 10_000_000,
            )
            .await
            .unwrap();
        }

        let row = read::find_by_id(&fx.store, id).unwrap().unwrap();
        assert!(!row.is_read(), "the read bit was not rolled back");
        assert!(row.is_answered(), "rollback disturbed an orthogonal bit");
    }

    /// A delete applies the row removal and the op together, and a refused
    /// delete parks as failed with nothing to restore (the row stays gone; the
    /// next sync refetches).
    #[tokio::test]
    async fn a_delete_queues_and_a_refusal_leaves_the_row_gone() {
        let fx = fixture();
        let id = fx.ingest_invite("inbox", 1, "Standup", &invite_ics("uid-a", 0, &["a@x.com"]));

        let op = ServerOp::Delete {
            message_id: "<inbox-1@example.com>".to_string(),
            source_mailbox: "INBOX".to_string(),
            copy: None,
            graph_copy: None,
        };
        apply_delete(&fx.store, &fx.blobs, "alice", id, op, Runner::Drain).unwrap();
        assert!(read::find_by_id(&fx.store, id).unwrap().is_none());
        assert_eq!(queued_ops(&fx.store, "alice").unwrap().len(), 1);

        let base = unix_now();
        for tick in 0..MAX_ATTEMPTS {
            let mut exec = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("delete refused"))]);
            drain(
                &fx.store,
                &fx.blobs,
                "alice",
                &mut exec,
                base + (tick + 1) * 10_000_000,
            )
            .await
            .unwrap();
        }

        assert!(
            read::find_by_id(&fx.store, id).unwrap().is_none(),
            "a delete has nothing to restore"
        );
        assert_eq!(failed_ops(&fx.store, "alice").unwrap().len(), 1);
        assert_eq!(counts(&fx.store, "alice").unwrap(), (0, 1));
    }

    /// PERSO-243: SQLite hands a deleted row's id to the next mail when it
    /// was the largest, so a delete in backoff and the ops on a new mail can
    /// share a target. The delete held the new mail's star back, a settle on
    /// it gave up on the delete's refusal, and the delete refused for good
    /// rolled the new mail's move back with it. Nothing follows a delete on
    /// its own message, so it orders nothing after it.
    #[tokio::test]
    async fn a_delete_orders_no_op_on_a_new_mail_that_reused_its_row_id() {
        let fx = fixture();
        let old = fx.ingest_plain("inbox", 1, "Old");
        let delete_op = ServerOp::Delete {
            message_id: "<inbox-1@example.com>".to_string(),
            source_mailbox: "INBOX".to_string(),
            copy: None,
            graph_copy: None,
        };
        let (_prev, delete) =
            apply_delete(&fx.store, &fx.blobs, "alice", old, delete_op, Runner::Drain)
                .unwrap()
                .unwrap();
        bump_attempt(&fx.store, delete, "connection reset").unwrap();
        let new = fx.ingest_plain("inbox", 2, "New");
        assert_eq!(new, old, "the new mail reused the deleted row's id");

        let star = ServerOp::SetFlagged {
            message_id: "<inbox-2@example.com>".to_string(),
            mailbox: "INBOX".to_string(),
            flagged: true,
            copy: None,
        };
        apply_set_flagged(&fx.store, "alice", new, true, star.clone(), Runner::Drain)
            .unwrap()
            .unwrap();
        let mut exec = FakeExecutor::always_ok();
        let result = drain(&fx.store, &fx.blobs, "alice", &mut exec, unix_now() + 10)
            .await
            .unwrap();
        assert_eq!(
            exec.seen,
            [star],
            "the delete is in backoff, the star is not"
        );
        assert_eq!((result.completed, result.still_open), (1, 1));

        let scope = SettleScope::open(&fx.store);
        let mark_read = apply_set_read(
            &fx.store,
            "alice",
            new,
            true,
            ServerOp::SetRead {
                message_id: "<inbox-2@example.com>".to_string(),
                mailbox: "INBOX".to_string(),
                read: true,
                copy: None,
                graph_copy: None,
            },
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        run_and_settle(
            &fx.store,
            &fx.blobs,
            &scope,
            mark_read,
            &mut CountingExecutor::new(&runs),
            OlderOps::wait_for(Duration::ZERO, &nobody),
        )
        .await
        .unwrap();
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);

        let (_prev, archive) = apply_move(
            &fx.store,
            "alice",
            new,
            "archive",
            move_op("<inbox-2@example.com>"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        let parked = refuse(&fx.store, &fx.blobs, delete, "NO refused", Refusal::Park).unwrap();
        assert!(parked.is_empty(), "parked {parked:?} with the delete");
        assert_eq!(queued_row(&fx.store, archive).attempts, 0);
        assert_eq!(
            read::find_by_id(&fx.store, new).unwrap().unwrap().mailbox,
            "archive"
        );
    }

    /// The CLI's synchronous settle: a succeeded op retires its row and the
    /// optimistic local change stands.
    #[test]
    fn settle_retires_a_succeeded_op_and_keeps_the_local_change() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let (_prev, op_id) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();

        let row = row_by_id(&fx.store, op_id).unwrap().unwrap();
        settle(&fx.store, &fx.blobs, &row, Ok(())).unwrap();

        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "archive"
        );
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
    }

    /// A refused op the CLI settles rolls its local half home, discards the row
    /// and returns the error, so the CLI reports it and exits.
    #[test]
    fn settle_rolls_a_refused_op_home_and_returns_the_error() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let (_prev, op_id) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();

        let row = row_by_id(&fx.store, op_id).unwrap().unwrap();
        let err = settle(
            &fx.store,
            &fx.blobs,
            &row,
            Err(anyhow::anyhow!("NO server refused")),
        )
        .unwrap_err();

        assert!(format!("{err:#}").contains("refused"));
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "inbox"
        );
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
    }

    /// CLI outcome parity: unlike the background [`drain`], the synchronous
    /// settle does **not** converge a not-found. A `mp delete` for a message the
    /// server no longer holds reports the not-found error byte-identical to the
    /// pre-queue path, because a synchronous caller is never a crash replay.
    #[test]
    fn settle_surfaces_a_not_found_verbatim_for_the_cli() {
        let fx = fixture();
        let id = fx.ingest_invite("inbox", 1, "Standup", &invite_ics("uid-a", 0, &["a@x.com"]));
        let op = ServerOp::Delete {
            message_id: "<inbox-1@example.com>".to_string(),
            source_mailbox: "INBOX".to_string(),
            copy: None,
            graph_copy: None,
        };
        let (_prev, op_id) = apply_delete(&fx.store, &fx.blobs, "alice", id, op, Runner::Drain)
            .unwrap()
            .unwrap();

        let not_found = crate::ops::NotFoundOnServer {
            message_id: "<inbox-1@example.com>".to_string(),
            mailbox: Some("INBOX".to_string()),
        };
        let expected = not_found.to_string();
        let row = row_by_id(&fx.store, op_id).unwrap().unwrap();
        let err = settle(&fx.store, &fx.blobs, &row, Err(not_found.into())).unwrap_err();

        assert_eq!(err.to_string(), expected);
        assert_eq!(
            expected,
            "Email with Message-ID <inbox-1@example.com> not found in INBOX on server"
        );
        assert!(
            read::find_by_id(&fx.store, id).unwrap().is_none(),
            "a delete has nothing to restore"
        );
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
    }

    /// An executor that counts every server op it runs into a counter it
    /// shares with another executor, so a test can count the server-side
    /// executions of one op across two runners.
    struct CountingExecutor {
        runs: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        /// Sent once the op is on the server, for a test that starts the
        /// other runner at that moment.
        started: Option<tokio::sync::oneshot::Sender<()>>,
        /// Awaited before the op returns, for a test that holds the op on
        /// the server until the other runner is done.
        hold: Option<tokio::sync::oneshot::Receiver<()>>,
    }

    impl CountingExecutor {
        fn new(runs: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
            CountingExecutor {
                runs: std::sync::Arc::clone(runs),
                started: None,
                hold: None,
            }
        }
    }

    impl OpExecutor for CountingExecutor {
        async fn execute(&mut self, _op: &ServerOp) -> Result<()> {
            self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            if let Some(hold) = self.hold.take() {
                let _ = hold.await;
            }
            Ok(())
        }
    }

    /// PERSO-236: a settled move and a sync tick's drain that starts while the
    /// move is on the server run the op once between them. The tick here is
    /// the IDLE watcher's, woken by the move's own first leg: it drains the
    /// account on a store connection of its own while the settle's server call
    /// is still in flight, and before the fix it found the same op queued and
    /// copied the message a second time.
    #[tokio::test]
    async fn a_settled_op_runs_once_when_a_drain_overlaps_it() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let scope = SettleScope::open(&fx.store);
        let (_prev, op_id) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();

        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (started, on_server) = tokio::sync::oneshot::channel();
        let (drained, release) = tokio::sync::oneshot::channel();
        let mut settling = CountingExecutor {
            started: Some(started),
            hold: Some(release),
            ..CountingExecutor::new(&runs)
        };
        let tick_store = Store::open(fx.store.path()).unwrap();
        let mut ticking = CountingExecutor::new(&runs);

        let settle = run_and_settle(
            &fx.store,
            &fx.blobs,
            &scope,
            op_id,
            &mut settling,
            OlderOps::wait(&nobody),
        );
        let tick = async {
            on_server.await.unwrap();
            let result = drain(
                &tick_store,
                &fx.blobs,
                "alice",
                &mut ticking,
                unix_now() + 10,
            )
            .await;
            drained.send(()).unwrap();
            result
        };
        let (settled, ticked) = tokio::join!(settle, tick);

        settled.unwrap();
        ticked.unwrap();
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the op ran on the server once, not once per runner"
        );
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "archive"
        );
    }

    /// Every kind a caller settles (move, delete, read, star) is committed out
    /// of the drain's reach while its caller's scope is open (PERSO-236).
    #[tokio::test]
    async fn a_drain_runs_no_kind_of_op_a_caller_is_settling() {
        let fx = fixture();
        let moved = fx.ingest_plain("inbox", 1, "Moved");
        let deleted = fx.ingest_plain("inbox", 2, "Deleted");
        let read_one = fx.ingest_plain("inbox", 3, "Read");
        let starred = fx.ingest_plain("inbox", 4, "Starred");
        let scope = SettleScope::open(&fx.store);
        let caller = Runner::Caller(&scope);
        let flag_op = |id: &str| ServerOp::SetFlagged {
            message_id: id.to_string(),
            mailbox: "INBOX".to_string(),
            flagged: true,
            copy: None,
        };
        apply_move(
            &fx.store,
            "alice",
            moved,
            "archive",
            move_op("<inbox-1@example.com>"),
            caller,
        )
        .unwrap()
        .unwrap();
        let delete = ServerOp::Delete {
            message_id: "<inbox-2@example.com>".to_string(),
            source_mailbox: "INBOX".to_string(),
            copy: None,
            graph_copy: None,
        };
        apply_delete(&fx.store, &fx.blobs, "alice", deleted, delete, caller)
            .unwrap()
            .unwrap();
        let read_op = ServerOp::SetRead {
            message_id: "<inbox-3@example.com>".to_string(),
            mailbox: "INBOX".to_string(),
            read: true,
            copy: None,
            graph_copy: None,
        };
        apply_set_read(&fx.store, "alice", read_one, true, read_op, caller)
            .unwrap()
            .unwrap();
        apply_set_flagged(
            &fx.store,
            "alice",
            starred,
            true,
            flag_op("<inbox-4@example.com>"),
            caller,
        )
        .unwrap()
        .unwrap();

        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut ticking = CountingExecutor::new(&runs);
        let result = drain(&fx.store, &fx.blobs, "alice", &mut ticking, unix_now() + 10)
            .await
            .unwrap();

        assert_eq!(result, DrainResult::default());
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
        assert_eq!(
            rows_in_state(&fx.store, "alice", STATE_SETTLING)
                .unwrap()
                .len(),
            4
        );
    }

    /// An op its caller left `settling`, because the process died between the
    /// commit and the settle, is the drain's once no scope over the store is
    /// open, and it runs once (PERSO-236).
    #[tokio::test]
    async fn an_op_left_settling_by_a_dead_caller_is_drained_once() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let scope = SettleScope::open(&fx.store);
        apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();
        // A second scope over the same store keeps the row out of reach on
        // its own: the first caller's is not the only one that counts.
        let other = SettleScope::open(&fx.store);
        drop(scope);
        assert_eq!(recover_orphaned_settles(&fx.store, "alice").unwrap(), 0);
        drop(other);

        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut ticking = CountingExecutor::new(&runs);
        let result = drain(&fx.store, &fx.blobs, "alice", &mut ticking, unix_now() + 10)
            .await
            .unwrap();

        assert_eq!(result.completed, 1);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(rows_in_state(&fx.store, "alice", STATE_SETTLING)
            .unwrap()
            .is_empty());
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "archive"
        );
    }

    /// An op committed for the drain is not a caller's to run: settling it
    /// would race the drain, so it is refused before the server is asked.
    #[tokio::test]
    async fn a_caller_cannot_settle_an_op_queued_for_the_drain() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let (_prev, op_id) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();

        let scope = SettleScope::open(&fx.store);
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut exec = CountingExecutor::new(&runs);
        let err = run_and_settle(
            &fx.store,
            &fx.blobs,
            &scope,
            op_id,
            &mut exec,
            OlderOps::wait(&nobody),
        )
        .await
        .unwrap_err();

        assert!(format!("{err:#}").contains("not settling"), "{err:#}");
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(queued_ops(&fx.store, "alice").unwrap().len(), 1);
    }

    /// A scope opened over one store cannot commit or settle an op in another.
    #[test]
    fn a_scope_over_another_store_is_refused() {
        let fx = fixture();
        let elsewhere = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let scope = SettleScope::open(&elsewhere.store);

        let err = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&scope),
        )
        .unwrap_err();

        assert!(format!("{err:#}").contains("cannot settle"), "{err:#}");
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "inbox",
            "a refused commit leaves the row where it was"
        );
    }

    /// The ops on one message run in the order they were committed when the
    /// first of them is a caller's (PERSO-236): a star and a second move
    /// queued while `mp archive` is on the server wait for it, rather than
    /// running against the folder the message has not reached yet, where the
    /// star finds nothing and is retired as done. Once the archive is
    /// settled they run, in order.
    #[tokio::test]
    async fn a_queued_op_waits_for_an_older_settling_op_on_its_message() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let scope = SettleScope::open(&fx.store);
        let (_prev, archive) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();
        let star = ServerOp::SetFlagged {
            message_id: "<inbox-1@example.com>".to_string(),
            mailbox: "Archive".to_string(),
            flagged: true,
            copy: None,
        };
        apply_set_flagged(&fx.store, "alice", id, true, star.clone(), Runner::Drain)
            .unwrap()
            .unwrap();
        let onwards = ServerOp::Move {
            message_id: "<inbox-1@example.com>".to_string(),
            source_mailbox: "Archive".to_string(),
            dest_mailbox: "Receipts".to_string(),
            copy: None,
            graph_copy: None,
        };
        apply_move(
            &fx.store,
            "alice",
            id,
            "receipts",
            onwards.clone(),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();

        let mut ticking = FakeExecutor::always_ok();
        let result = drain(&fx.store, &fx.blobs, "alice", &mut ticking, unix_now() + 10)
            .await
            .unwrap();

        assert!(ticking.seen.is_empty(), "ran {:?}", ticking.seen);
        assert_eq!(
            result,
            DrainResult {
                still_open: 2,
                ..DrainResult::default()
            }
        );
        let waiting = queued_ops(&fx.store, "alice").unwrap();
        assert_eq!(waiting.len(), 2);
        assert!(
            waiting.iter().all(|row| row.attempts == 0),
            "waiting is not a failed attempt"
        );

        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        run_and_settle(
            &fx.store,
            &fx.blobs,
            &scope,
            archive,
            &mut CountingExecutor::new(&runs),
            OlderOps::wait(&nobody),
        )
        .await
        .unwrap();
        let result = drain(&fx.store, &fx.blobs, "alice", &mut ticking, unix_now() + 10)
            .await
            .unwrap();

        assert_eq!(ticking.seen, [star, onwards], "in the order committed");
        assert_eq!(result.completed, 2);
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
    }

    /// A caller settling an op on one message holds back no op on another.
    #[tokio::test]
    async fn a_settling_op_defers_no_op_on_another_message() {
        let fx = fixture();
        let settled = fx.ingest_plain("inbox", 1, "Settled");
        let other = fx.ingest_plain("inbox", 2, "Other");
        let scope = SettleScope::open(&fx.store);
        apply_move(
            &fx.store,
            "alice",
            settled,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();
        apply_move(
            &fx.store,
            "alice",
            other,
            "archive",
            move_op("<inbox-2@example.com>"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();

        let mut ticking = FakeExecutor::always_ok();
        let result = drain(&fx.store, &fx.blobs, "alice", &mut ticking, unix_now() + 10)
            .await
            .unwrap();

        assert_eq!(ticking.seen, [move_op("<inbox-2@example.com>")]);
        assert_eq!(result.completed, 1);
        assert_eq!(result.still_open, 0);
        assert_eq!(
            rows_in_state(&fx.store, "alice", STATE_SETTLING)
                .unwrap()
                .len(),
            1,
            "the caller's op is still the caller's"
        );
    }

    /// A move to the archive and a star queued after it on the same message,
    /// both for the drain: what PERSO-243's tests start from. Returns the
    /// message's row id and the two op ids, oldest first.
    fn refused_move_then_star(fx: &crate::reconcile::tests::Fixture) -> (i64, [i64; 2]) {
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let (_prev, archive) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        let star = apply_set_flagged(
            &fx.store,
            "alice",
            id,
            true,
            star_op("Archive"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        (id, [archive, star])
    }

    fn queued_row(store: &Store, op_id: i64) -> PendingOp {
        let row = row_by_id(store, op_id).unwrap().unwrap();
        assert_eq!(row.state, STATE_QUEUED, "op {op_id}");
        row
    }

    /// PERSO-243: a move the server refused once waits out its backoff, and
    /// the star queued after it on the same message waits with it rather
    /// than running first against the folder the move has not reached. Once
    /// the backoff has elapsed both run, in the order committed.
    #[tokio::test]
    async fn a_queued_op_waits_for_an_older_op_in_backoff_on_its_message() {
        let fx = fixture();
        let (_id, [_archive, star]) = refused_move_then_star(&fx);
        let base = unix_now();

        let mut refusing = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("connection reset"))]);
        let result = drain(&fx.store, &fx.blobs, "alice", &mut refusing, base + 5)
            .await
            .unwrap();
        assert_eq!(refusing.seen, [move_op("<inbox-1@example.com>")]);
        assert_eq!(
            result,
            DrainResult {
                still_open: 2,
                ..DrainResult::default()
            }
        );

        // The move's backoff has not elapsed: neither op runs.
        let mut early = FakeExecutor::always_ok();
        drain(&fx.store, &fx.blobs, "alice", &mut early, base + 5)
            .await
            .unwrap();
        assert!(early.seen.is_empty(), "ran {:?}", early.seen);
        assert_eq!(queued_row(&fx.store, star).attempts, 0);

        let mut later = FakeExecutor::always_ok();
        let result = drain(&fx.store, &fx.blobs, "alice", &mut later, base + 10_000_000)
            .await
            .unwrap();
        assert_eq!(
            later.seen,
            [move_op("<inbox-1@example.com>"), star_op("Archive")],
            "in the order committed"
        );
        assert_eq!(result.completed, 2);
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
    }

    /// An op in backoff holds back only the later ops on its own message:
    /// a move of another message queued after them still runs.
    #[tokio::test]
    async fn an_op_in_backoff_holds_back_no_op_on_another_message() {
        let fx = fixture();
        let (_id, [_archive, star]) = refused_move_then_star(&fx);
        let other = fx.ingest_plain("inbox", 2, "Other");
        apply_move(
            &fx.store,
            "alice",
            other,
            "archive",
            move_op("<inbox-2@example.com>"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();

        let mut exec = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("connection reset"))]);
        let result = drain(&fx.store, &fx.blobs, "alice", &mut exec, unix_now() + 10)
            .await
            .unwrap();

        assert_eq!(
            exec.seen,
            [
                move_op("<inbox-1@example.com>"),
                move_op("<inbox-2@example.com>")
            ]
        );
        assert_eq!((result.completed, result.still_open), (1, 2));
        assert_eq!(queued_row(&fx.store, star).attempts, 0);
    }

    /// A held op spends no attempt and backs off on nothing of its own: across
    /// every refusal of the move ahead of it, the star stays as committed.
    #[tokio::test]
    async fn an_op_held_behind_an_older_one_spends_no_attempt() {
        let fx = fixture();
        let (_id, [archive, star]) = refused_move_then_star(&fx);
        let committed = queued_row(&fx.store, star);

        let base = unix_now();
        for tick in 0..MAX_ATTEMPTS - 1 {
            let mut exec = FakeExecutor::scripted(vec![
                Err(anyhow::anyhow!("connection reset")),
                Err(anyhow::anyhow!("connection reset")),
            ]);
            drain(
                &fx.store,
                &fx.blobs,
                "alice",
                &mut exec,
                base + (tick + 1) * 10_000_000,
            )
            .await
            .unwrap();
        }

        assert_eq!(queued_row(&fx.store, archive).attempts, MAX_ATTEMPTS - 1);
        let held = queued_row(&fx.store, star);
        assert_eq!(
            (held.attempts, held.last_error, held.updated),
            (0, None, committed.updated),
            "waiting is not a failed attempt"
        );
    }

    /// A move refused for good rolls back the later ops on its message with
    /// it, unrun: the star, the read toggle and the second move were
    /// committed against the folder the refused move put the message in.
    /// Undone newest first, the message lands back in the inbox, unstarred
    /// and unread, where the server still has it; undone oldest first, the
    /// second move's rollback would leave the message in the archive.
    #[tokio::test]
    async fn a_refused_op_rolls_back_the_later_ops_on_its_message() {
        let fx = fixture();
        let (id, [archive, star]) = refused_move_then_star(&fx);
        let read_op = ServerOp::SetRead {
            message_id: "<inbox-1@example.com>".to_string(),
            mailbox: "Archive".to_string(),
            read: true,
            copy: None,
            graph_copy: None,
        };
        let mark_read = apply_set_read(&fx.store, "alice", id, true, read_op, Runner::Drain)
            .unwrap()
            .unwrap();
        let (_prev, onwards) = apply_move(
            &fx.store,
            "alice",
            id,
            "receipts",
            ServerOp::Move {
                message_id: "<inbox-1@example.com>".to_string(),
                source_mailbox: "Archive".to_string(),
                dest_mailbox: "Receipts".to_string(),
                copy: None,
                graph_copy: None,
            },
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        let row = read::find_by_id(&fx.store, id).unwrap().unwrap();
        assert!(row.is_flagged() && row.is_read());

        let base = unix_now();
        let mut seen = Vec::new();
        let mut last = DrainResult::default();
        for tick in 0..MAX_ATTEMPTS {
            let mut exec = FakeExecutor::scripted(
                (0..4)
                    .map(|_| Err(anyhow::anyhow!("NO server refused")))
                    .collect(),
            );
            last = drain(
                &fx.store,
                &fx.blobs,
                "alice",
                &mut exec,
                base + (tick + 1) * 10_000_000,
            )
            .await
            .unwrap();
            seen.extend(exec.seen);
        }

        assert!(
            seen.iter()
                .all(|op| *op == move_op("<inbox-1@example.com>")),
            "only the refused move ran: {seen:?}"
        );
        assert_eq!(last.failed, 4);
        let row = read::find_by_id(&fx.store, id).unwrap().unwrap();
        assert_eq!(row.mailbox, "inbox");
        assert!(!row.is_flagged(), "the star was not rolled back");
        assert!(!row.is_read(), "the read toggle was not rolled back");
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
        let failed = failed_ops(&fx.store, "alice").unwrap();
        assert_eq!(
            failed.iter().map(|op| op.id).collect::<Vec<_>>(),
            [archive, star, mark_read, onwards]
        );
        for op in &failed[1..] {
            assert_eq!(op.attempts, 0, "op {} never ran", op.id);
            assert!(
                op.last_error
                    .as_deref()
                    .unwrap()
                    .contains(&format!("op {archive}, move")),
                "{:?}",
                op.last_error
            );
        }
    }

    /// Drain `MAX_ATTEMPTS` passes, each refusing the first op it runs, and
    /// return every op run and the last pass's result.
    async fn refuse_the_first_op_for_good(
        fx: &crate::reconcile::tests::Fixture,
    ) -> (Vec<ServerOp>, DrainResult) {
        let base = unix_now();
        let mut seen = Vec::new();
        let mut last = DrainResult::default();
        for tick in 0..MAX_ATTEMPTS {
            let mut exec = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("NO refused"))]);
            last = drain(
                &fx.store,
                &fx.blobs,
                "alice",
                &mut exec,
                base + (tick + 1) * 10_000_000,
            )
            .await
            .unwrap();
            seen.extend(exec.seen);
        }
        (seen, last)
    }

    /// PERSO-243: a star refused for good changed no folder, so the archive
    /// queued after it was committed against a state the server has, and
    /// runs once the star is parked rather than being rolled back with it.
    #[tokio::test]
    async fn a_refused_star_releases_a_later_move_to_run() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let star = apply_set_flagged(
            &fx.store,
            "alice",
            id,
            true,
            star_op("INBOX"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();

        let (seen, last) = refuse_the_first_op_for_good(&fx).await;

        let stars = vec![star_op("INBOX"); MAX_ATTEMPTS as usize];
        assert_eq!(seen[..stars.len()], stars[..]);
        assert_eq!(seen[stars.len()..], [move_op("<inbox-1@example.com>")]);
        assert_eq!((last.failed, last.completed), (1, 1));
        let row = read::find_by_id(&fx.store, id).unwrap().unwrap();
        assert_eq!(row.mailbox, "archive", "the move was rolled back");
        assert!(!row.is_flagged());
        assert_eq!(
            failed_ops(&fx.store, "alice")
                .unwrap()
                .iter()
                .map(|op| op.id)
                .collect::<Vec<_>>(),
            [star]
        );
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
    }

    /// PERSO-243: a reply sent while a star waits out its backoff writes
    /// `\Answered` after the star's rollback snapshot. The star refused for
    /// good puts back its own bit and keeps the reply's, which restoring the
    /// whole snapshot wiped, and the reply's server half still runs.
    #[tokio::test]
    async fn a_refused_star_keeps_an_answered_bit_written_after_it() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        apply_set_flagged(
            &fx.store,
            "alice",
            id,
            true,
            star_op("INBOX"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        let replied = apply_post_send_flag(
            &fx.store,
            "alice",
            "<inbox-1@example.com>",
            true,
            false,
            &["INBOX".to_string()],
        )
        .unwrap();
        assert_eq!(replied.rows, 1);

        let (seen, last) = refuse_the_first_op_for_good(&fx).await;

        assert_eq!(seen.last().map(ServerOp::kind), Some("set_answered"));
        assert_eq!((last.failed, last.completed), (1, 1));
        let row = read::find_by_id(&fx.store, id).unwrap().unwrap();
        assert!(!row.is_flagged(), "the star was not rolled back");
        assert!(
            row.is_answered(),
            "the star's rollback wiped the reply's bit"
        );
    }

    /// A star, an unstar and a star again on one message, the first two
    /// refused for good while a later one on the same flag is owed: the local
    /// bit is the last op's until it is refused too, and then the one from
    /// before them all, which is what the server kept. Restoring each op's
    /// own snapshot showed unstarred while the last star was still owed,
    /// and a refused unstar would have restored the star it was taken over.
    #[test]
    fn a_refused_toggle_leaves_its_flag_to_a_later_owed_toggle() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let toggles = [true, false, true].map(|flagged| {
            let op = ServerOp::SetFlagged {
                message_id: "<inbox-1@example.com>".to_string(),
                mailbox: "INBOX".to_string(),
                flagged,
                copy: None,
            };
            apply_set_flagged(&fx.store, "alice", id, flagged, op, Runner::Drain)
                .unwrap()
                .unwrap()
        });
        let flagged = || {
            read::find_by_id(&fx.store, id)
                .unwrap()
                .unwrap()
                .is_flagged()
        };

        refuse(&fx.store, &fx.blobs, toggles[0], "NO", Refusal::Park).unwrap();
        assert!(flagged(), "the last star is still owed");
        refuse(&fx.store, &fx.blobs, toggles[1], "NO", Refusal::Park).unwrap();
        assert!(flagged(), "the last star is still owed");
        refuse(&fx.store, &fx.blobs, toggles[2], "NO", Refusal::Park).unwrap();
        assert!(!flagged(), "the server never got a star");
    }

    /// An executor that appends every op it runs, and when it finished, to a
    /// log it shares with the other runners of a test, so the test can read
    /// the order the server saw the ops of one message in.
    struct LoggingExecutor {
        log: std::sync::Arc<Mutex<Vec<String>>>,
        name: &'static str,
        /// Awaited before the op returns, to hold it on the server.
        hold: Option<tokio::sync::oneshot::Receiver<()>>,
    }

    impl LoggingExecutor {
        fn new(log: &std::sync::Arc<Mutex<Vec<String>>>, name: &'static str) -> Self {
            LoggingExecutor {
                log: std::sync::Arc::clone(log),
                name,
                hold: None,
            }
        }
    }

    impl OpExecutor for LoggingExecutor {
        async fn execute(&mut self, op: &ServerOp) -> Result<()> {
            let kind = op.kind();
            self.log
                .lock()
                .unwrap()
                .push(format!("{} {kind}", self.name));
            if let Some(hold) = self.hold.take() {
                let _ = hold.await;
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("{} {kind} done", self.name));
            }
            Ok(())
        }
    }

    fn star_op(mailbox: &str) -> ServerOp {
        ServerOp::SetFlagged {
            message_id: "<inbox-1@example.com>".to_string(),
            mailbox: mailbox.to_string(),
            flagged: true,
            copy: None,
        }
    }

    /// The server folder of a store mailbox key, as a daemon command
    /// addresses its op.
    fn server_folder(mailbox: &str) -> &'static str {
        match mailbox {
            "inbox" => "INBOX",
            "archive" => "Archive",
            other => panic!("no server folder for {other}"),
        }
    }

    /// PERSO-249: a daemon command on another connection marks the message
    /// read while the drain is rolling back a refused move. It tries to
    /// commit after the refusal listed the later ops and before it rolled
    /// any back; that window let it commit against the archive the move had
    /// put the message in, unlisted and so never rolled back, and the read
    /// toggle then searched the archive and was retired as a converged
    /// not-found. Now it lands once the refusal has committed, addressed to
    /// the inbox the message is back in, or it is rolled back with the rest.
    #[test]
    fn an_op_committed_while_a_refusal_rolls_back_is_never_left_behind() {
        let fx = fixture();
        let (id, [archive, _star]) = refused_move_then_star(&fx);
        let path = fx.store.path().to_path_buf();
        let (sent, landed) = std::sync::mpsc::channel();
        let command = std::sync::Arc::new(Mutex::new(None));
        let spawned = std::sync::Arc::clone(&command);
        AFTER_LISTING.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let thread = std::thread::spawn(move || {
                    let store = Store::open(&path).unwrap();
                    // The command reads the row under the write lock, as
                    // `apply_*` does, and addresses its op from what it read.
                    let tx = store.immediate_transaction().unwrap();
                    let mailbox: String = tx
                        .query_row("SELECT mailbox FROM messages WHERE id = ?1", [id], |row| {
                            row.get(0)
                        })
                        .unwrap();
                    drop(tx);
                    let op = ServerOp::SetRead {
                        message_id: "<inbox-1@example.com>".to_string(),
                        mailbox: server_folder(&mailbox).to_string(),
                        read: true,
                        copy: None,
                        graph_copy: None,
                    };
                    let op_id = apply_set_read(&store, "alice", id, true, op, Runner::Drain)
                        .unwrap()
                        .unwrap();
                    let _ = sent.send(());
                    op_id
                });
                // Long enough for the command to commit when nothing holds
                // the write lock; a command still blocked lands afterwards.
                let _ = landed.recv_timeout(Duration::from_millis(500));
                *spawned.lock().unwrap() = Some(thread);
            }));
        });

        refuse(&fx.store, &fx.blobs, archive, "NO refused", Refusal::Park).unwrap();
        let thread = command.lock().unwrap().take().expect("the hook ran");
        let mark_read = thread.join().unwrap();

        let message = read::find_by_id(&fx.store, id).unwrap().unwrap();
        assert_eq!(message.mailbox, "inbox");
        let op = row_by_id(&fx.store, mark_read).unwrap().unwrap();
        match op.state.as_str() {
            STATE_FAILED => assert!(!message.is_read(), "rolled back with the move"),
            STATE_QUEUED => {
                let ServerOp::SetRead { mailbox, .. } = &op.op else {
                    panic!("{:?}", op.op)
                };
                assert_eq!(
                    mailbox, "INBOX",
                    "left queued against the folder the move never reached"
                );
                assert!(message.is_read());
            }
            other => panic!("op {mark_read} is {other}"),
        }
    }

    /// PERSO-249, the caller's side: `mp` marks the message read with a
    /// settle while the archive queued before it is on its last attempt.
    /// The archive is refused for good before the settle first looks at the
    /// queue, so it finds no older op owed; it ran the read toggle against
    /// the archive the move had put the message in, while the refusal had
    /// rolled the message back to the inbox. Now the refusal rolls the
    /// caller's op back with it and parks it, and the settle runs nothing.
    #[tokio::test]
    async fn a_settled_op_rolled_back_with_an_older_refused_move_does_not_run() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let (_prev, archive) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        let scope = SettleScope::open(&fx.store);
        let mark_read = apply_set_read(
            &fx.store,
            "alice",
            id,
            true,
            ServerOp::SetRead {
                message_id: "<inbox-1@example.com>".to_string(),
                mailbox: "Archive".to_string(),
                read: true,
                copy: None,
                graph_copy: None,
            },
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();
        refuse(&fx.store, &fx.blobs, archive, "NO refused", Refusal::Park).unwrap();

        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let err = run_and_settle(
            &fx.store,
            &fx.blobs,
            &scope,
            mark_read,
            &mut CountingExecutor::new(&runs),
            OlderOps::wait(&nobody),
        )
        .await
        .unwrap_err();

        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 0);
        let err = format!("{err:#}");
        assert!(err.contains(&format!("op {archive}, move")), "{err}");
        let message = read::find_by_id(&fx.store, id).unwrap().unwrap();
        assert_eq!(message.mailbox, "inbox");
        assert!(!message.is_read());
        assert!(
            row_by_id(&fx.store, mark_read).unwrap().is_none(),
            "the caller reported its op and discarded it"
        );
        assert_eq!(failed_ops(&fx.store, "alice").unwrap().len(), 1);
    }

    /// PERSO-241: a star the TUI queued, then `mp archive` on the same
    /// message inside the drainer's debounce. The archive waits for the
    /// drain it asks for and reaches the server after the star, so the star
    /// does not search a folder the message has already left. Each op runs
    /// once, the star by the drain and the archive by its caller.
    #[tokio::test]
    async fn a_settled_op_waits_for_an_older_queued_op_on_its_message() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        apply_set_flagged(
            &fx.store,
            "alice",
            id,
            true,
            star_op("INBOX"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        let scope = SettleScope::open(&fx.store);
        let (_prev, archive) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();

        let log = std::sync::Arc::new(Mutex::new(Vec::new()));
        let asked = tokio::sync::Notify::new();
        let nudge = || asked.notify_one();
        let mut caller = LoggingExecutor::new(&log, "caller");
        let settle = run_and_settle(
            &fx.store,
            &fx.blobs,
            &scope,
            archive,
            &mut caller,
            OlderOps::wait(&nudge),
        );
        // The account's drainer, on a store connection of its own. Bounded,
        // so a settle that never asks still lets the drain run and the test
        // fail on the order rather than hang.
        let drain_store = Store::open(fx.store.path()).unwrap();
        let mut drainer = LoggingExecutor::new(&log, "drain");
        let drained = async {
            let _ = tokio::time::timeout(Duration::from_secs(2), asked.notified()).await;
            drain(&drain_store, &fx.blobs, "alice", &mut drainer, unix_now()).await
        };
        let (settled, drained) = tokio::join!(settle, drained);

        settled.unwrap();
        assert_eq!(drained.unwrap().completed, 1);
        assert_eq!(
            *log.lock().unwrap(),
            ["drain set_flagged", "caller move"],
            "the server saw the star first, each op once"
        );
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
        let row = read::find_by_id(&fx.store, id).unwrap().unwrap();
        assert_eq!(row.mailbox, "archive");
        assert!(row.is_flagged());
    }

    /// Two callers on one message, as two agents' `mp` commands: the newer
    /// settle waits for the older one to come back from the server before it
    /// runs its own op (PERSO-241).
    #[tokio::test]
    async fn a_settled_op_waits_for_an_older_caller_on_its_message() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let first = SettleScope::open(&fx.store);
        let (_prev, archive) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&first),
        )
        .unwrap()
        .unwrap();
        let second = SettleScope::open(&fx.store);
        let star = apply_set_flagged(
            &fx.store,
            "alice",
            id,
            true,
            star_op("Archive"),
            Runner::Caller(&second),
        )
        .unwrap()
        .unwrap();

        let log = std::sync::Arc::new(Mutex::new(Vec::new()));
        let (release, hold) = tokio::sync::oneshot::channel();
        let mut archiving = LoggingExecutor {
            hold: Some(hold),
            ..LoggingExecutor::new(&log, "archive")
        };
        let mut starring = LoggingExecutor::new(&log, "star");
        let archived = run_and_settle(
            &fx.store,
            &fx.blobs,
            &first,
            archive,
            &mut archiving,
            OlderOps::wait(&nobody),
        );
        let starred = run_and_settle(
            &fx.store,
            &fx.blobs,
            &second,
            star,
            &mut starring,
            OlderOps::wait(&nobody),
        );
        let server = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            release.send(()).unwrap();
        };
        let (archived, starred, ()) = tokio::join!(archived, starred, server);

        archived.unwrap();
        starred.unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            ["archive move", "archive move done", "star set_flagged"]
        );
        assert!(rows_in_state(&fx.store, "alice", STATE_SETTLING)
            .unwrap()
            .is_empty());
    }

    /// An older op the server already refused once is waiting out a backoff
    /// of thirty seconds or more, and may yet be rolled back under the newer
    /// op: the settle gives up at once, rolls its own op back unrun and says
    /// why, and the older op stays the drain's (PERSO-241).
    #[tokio::test]
    async fn a_settled_op_gives_up_on_an_older_op_the_server_refused() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let star = apply_set_flagged(
            &fx.store,
            "alice",
            id,
            true,
            star_op("INBOX"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        bump_attempt(&fx.store, star, "connection reset").unwrap();
        let scope = SettleScope::open(&fx.store);
        let (_prev, archive) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();

        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let err = run_and_settle(
            &fx.store,
            &fx.blobs,
            &scope,
            archive,
            &mut CountingExecutor::new(&runs),
            OlderOps::wait(&nobody),
        )
        .await
        .unwrap_err();

        let err = format!("{err:#}");
        assert!(err.contains(&format!("op {star}, set_flagged")), "{err}");
        assert!(err.contains("connection reset"), "{err}");
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "inbox",
            "the archive was rolled back"
        );
        let queued = queued_ops(&fx.store, "alice").unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id, star);
        assert!(rows_in_state(&fx.store, "alice", STATE_SETTLING)
            .unwrap()
            .is_empty());
    }

    /// An older op no drain runs within the caller's patience makes the
    /// settle give up, after asking for the drain once (PERSO-241).
    #[tokio::test]
    async fn a_settled_op_gives_up_on_an_older_op_still_owed_after_its_patience() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let star = apply_set_flagged(
            &fx.store,
            "alice",
            id,
            true,
            star_op("INBOX"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        let scope = SettleScope::open(&fx.store);
        let (_prev, archive) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();

        let asked = std::cell::Cell::new(0);
        let nudge = || asked.set(asked.get() + 1);
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let err = run_and_settle(
            &fx.store,
            &fx.blobs,
            &scope,
            archive,
            &mut CountingExecutor::new(&runs),
            OlderOps::wait_for(Duration::from_millis(250), &nudge),
        )
        .await
        .unwrap_err();

        let err = format!("{err:#}");
        assert!(err.contains(&format!("op {star}, set_flagged")), "{err}");
        assert!(err.contains("has not reached the server"), "{err}");
        assert_eq!(asked.get(), 1, "the drain was asked for once");
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            read::find_by_id(&fx.store, id).unwrap().unwrap().mailbox,
            "inbox"
        );
        assert_eq!(queued_ops(&fx.store, "alice").unwrap().len(), 1);
    }

    /// A held turn spends none of the patience: a settle whose patience is a
    /// fraction of the wait still waits while the turn is held, and only the
    /// ceiling ends it (PERSO-241).
    #[tokio::test]
    async fn a_held_turn_spends_no_patience_and_the_ceiling_still_ends_the_wait() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let star = apply_set_flagged(
            &fx.store,
            "alice",
            id,
            true,
            star_op("INBOX"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        let scope = SettleScope::open(&fx.store);
        let (_prev, archive) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();

        let held = || true;
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let started = std::time::Instant::now();
        let err = run_and_settle(
            &fx.store,
            &fx.blobs,
            &scope,
            archive,
            &mut CountingExecutor::new(&runs),
            OlderOps::wait_for(Duration::from_millis(50), &nobody)
                .paused_while(&held)
                .within(Duration::from_millis(400)),
        )
        .await
        .unwrap_err();

        assert!(
            started.elapsed() >= Duration::from_millis(400),
            "gave up after {:?}, inside the ceiling",
            started.elapsed()
        );
        let err = format!("{err:#}");
        assert!(err.contains(&format!("op {star}, set_flagged")), "{err}");
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// A settle waits only for ops still owed on its own message: an older op
    /// parked `failed` was rolled back before this one was committed, and an
    /// op on another message is no concern of its ordering (PERSO-241).
    #[tokio::test]
    async fn a_settled_op_waits_for_no_failed_op_and_no_other_message() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let other = fx.ingest_plain("inbox", 2, "Other");
        let parked = apply_set_flagged(
            &fx.store,
            "alice",
            id,
            true,
            star_op("INBOX"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        refuse(&fx.store, &fx.blobs, parked, "refused", Refusal::Park).unwrap();
        apply_move(
            &fx.store,
            "alice",
            other,
            "archive",
            move_op("<inbox-2@example.com>"),
            Runner::Drain,
        )
        .unwrap()
        .unwrap();
        let scope = SettleScope::open(&fx.store);
        let (_prev, archive) = apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();

        let asked = std::cell::Cell::new(0);
        let nudge = || asked.set(asked.get() + 1);
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        run_and_settle(
            &fx.store,
            &fx.blobs,
            &scope,
            archive,
            &mut CountingExecutor::new(&runs),
            OlderOps::wait_for(Duration::ZERO, &nudge),
        )
        .await
        .unwrap();

        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(asked.get(), 0, "nothing to wait for, nothing asked");
    }

    /// The resume's gate owes an op a dead caller left `settling`: counted
    /// before it is handed back, the queue would read empty at every tick and
    /// the op would never reach the drain that recovers it (PERSO-236).
    #[test]
    fn a_resume_owes_an_op_left_settling_by_a_dead_caller() {
        let _data = crate::config::test_env::TestDataDir::new();
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Receipt");
        let scope = SettleScope::open(&fx.store);
        apply_move(
            &fx.store,
            "alice",
            id,
            "archive",
            move_op("<inbox-1@example.com>"),
            Runner::Caller(&scope),
        )
        .unwrap()
        .unwrap();
        drop(scope);
        let account = AccountConfig {
            name: "alice".to_string(),
            ..Default::default()
        };

        assert_eq!(owed_ops(&fx.store, &account).unwrap(), 1);
        assert_eq!(queued_ops(&fx.store, "alice").unwrap().len(), 1);
    }

    /// The post-send flag write (#0076) commits the local bit and one
    /// multi-mailbox server op together, and the op names every folder the
    /// source is filed in, so the drain spends one IMAP session where the old
    /// inline path spent one login per mailbox.
    #[test]
    fn a_post_send_flag_writes_every_copy_and_queues_one_op() {
        let fx = fixture();
        let inbox = fx.ingest_plain("inbox", 1, "Question");
        fx.store
            .conn()
            .execute(
                "INSERT INTO messages (account, mailbox, uid, message_id, subject)
                 SELECT account, 'archive', 99, message_id, subject FROM messages WHERE id = ?1",
                [inbox],
            )
            .unwrap();

        let outcome = apply_post_send_flag(
            &fx.store,
            "alice",
            "<inbox-1@example.com>",
            true,
            false,
            &["INBOX".to_string(), "Archive".to_string()],
        )
        .unwrap();

        assert_eq!(outcome.rows, 2);
        for row in read::find_by_message_id(&fx.store, "alice", "<inbox-1@example.com>").unwrap() {
            assert!(row.is_answered(), "{} was not flagged", row.mailbox);
        }
        let queued = queued_ops(&fx.store, "alice").unwrap();
        assert_eq!(queued.len(), 1, "N mailboxes are one op, not N");
        assert_eq!(queued[0].kind, "set_answered");
        assert_eq!(queued[0].rollback, Rollback::None);
    }

    /// The verb Graph records rides the payload (PERSO-171): a reply to all
    /// says so, and a forward never does, whatever the caller passed.
    #[test]
    fn the_post_send_op_carries_the_reply_all_verb_and_a_forward_drops_it() {
        let fx = fixture();
        fx.ingest_plain("inbox", 1, "Question");
        fx.ingest_plain("inbox", 2, "Passing on");
        for (message_id, answered) in [
            ("<inbox-1@example.com>", true),
            ("<inbox-2@example.com>", false),
        ] {
            apply_post_send_flag(
                &fx.store,
                "alice",
                message_id,
                answered,
                true,
                &["inbox".to_string()],
            )
            .unwrap();
        }
        let ops: Vec<_> = queued_ops(&fx.store, "alice")
            .unwrap()
            .into_iter()
            .map(|row| row.op)
            .collect();
        assert_eq!(
            ops,
            vec![
                ServerOp::SetAnswered {
                    message_id: "<inbox-1@example.com>".to_string(),
                    mailboxes: vec!["inbox".to_string()],
                    answered: true,
                    reply_all: true,
                },
                ServerOp::SetAnswered {
                    message_id: "<inbox-2@example.com>".to_string(),
                    mailboxes: vec!["inbox".to_string()],
                    answered: false,
                    reply_all: false,
                },
            ]
        );
    }

    /// The #0076 durability contract, offline: a send that succeeded and whose
    /// flag bookkeeping then fails for good must leave the message **sent**.
    ///
    /// The outbox row keeps its post-delivery state and its exactly-once
    /// submission marker, so the startup sweep finds nothing to resubmit; the
    /// answered bit stays true, because it records something that happened and
    /// [`Rollback::None`] is the right undo for it; and the refusal is visible
    /// as a `failed` queue row rather than a silent loss.
    #[tokio::test]
    async fn a_refused_post_send_flag_never_re_sends_the_message() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Question");
        let envelope = crate::outbox::Envelope {
            from: "me@example.com".to_string(),
            recipients: vec![(
                "bob@example.com".to_string(),
                crate::send::RecipientRole::To,
            )],
            ..Default::default()
        };
        let row_id = crate::outbox::enqueue(
            &fx.store,
            &fx.blobs,
            "alice",
            None,
            "<sent-1@example.com>",
            b"From: me\r\n\r\nbody",
            &envelope,
        )
        .unwrap();
        crate::outbox::mark_submission_started(&fx.store, row_id).unwrap();
        let state = crate::outbox::record_submission(
            &fx.store,
            &fx.blobs,
            row_id,
            &crate::outbox::SubmitOutcome::Accepted,
        )
        .unwrap();
        assert!(
            !state.is_open(),
            "the message is out and its row is settled"
        );

        // The bookkeeping the send owes, and a server that refuses it forever.
        apply_post_send_flag(
            &fx.store,
            "alice",
            "<inbox-1@example.com>",
            true,
            false,
            &["INBOX".to_string()],
        )
        .unwrap();
        let base = unix_now();
        for tick in 0..MAX_ATTEMPTS {
            let mut exec = FakeExecutor::scripted(vec![Err(anyhow::anyhow!("STORE rejected"))]);
            drain(
                &fx.store,
                &fx.blobs,
                "alice",
                &mut exec,
                base + (tick + 1) * 10_000_000,
            )
            .await
            .unwrap();
        }

        // The send is untouched: same row, same terminal state, nothing the
        // startup sweep would submit again.
        let sent = crate::outbox::load(&fx.store, row_id).unwrap().unwrap();
        assert_eq!(sent.state, state);
        let sweep = crate::outbox::sweep_pending_sends(&fx.store, "alice").unwrap();
        assert!(
            sweep.resubmittable.is_empty(),
            "a failed flag op must never re-send"
        );
        assert!(sweep.stranded.is_empty());
        assert_eq!(
            crate::outbox::unfinished_rows(&fx.store, "alice")
                .unwrap()
                .len(),
            0
        );

        // The bit stands, and the refusal is visible.
        assert!(read::find_by_id(&fx.store, id)
            .unwrap()
            .unwrap()
            .is_answered());
        assert_eq!(failed_ops(&fx.store, "alice").unwrap().len(), 1);
    }

    /// A post-send flag op that already landed before a crash replays and
    /// converges: the drain retires it and the answered bit stays.
    #[tokio::test]
    async fn a_post_send_flag_replay_converges() {
        let fx = fixture();
        let id = fx.ingest_plain("inbox", 1, "Question");
        apply_post_send_flag(
            &fx.store,
            "alice",
            "<inbox-1@example.com>",
            false,
            false,
            &["INBOX".to_string()],
        )
        .unwrap();

        let mut exec = FakeExecutor::scripted(vec![not_found("<inbox-1@example.com>")]);
        let r = drain(&fx.store, &fx.blobs, "alice", &mut exec, unix_now() + 10)
            .await
            .unwrap();

        assert_eq!((r.completed, r.failed), (1, 0));
        assert!(read::find_by_id(&fx.store, id)
            .unwrap()
            .unwrap()
            .is_forwarded());
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
    }

    /// Mutating a missing row is a no-op, not an error or a queued op.
    #[test]
    fn applying_to_a_missing_row_queues_nothing() {
        let fx = fixture();
        assert!(apply_move(
            &fx.store,
            "alice",
            404,
            "archive",
            move_op("<x@x>"),
            Runner::Drain
        )
        .unwrap()
        .is_none());
        assert!(apply_delete(
            &fx.store,
            &fx.blobs,
            "alice",
            404,
            ServerOp::Delete {
                message_id: "<x@x>".to_string(),
                source_mailbox: "INBOX".to_string(),
                copy: None,
                graph_copy: None,
            },
            Runner::Drain
        )
        .unwrap()
        .is_none());
        assert!(queued_ops(&fx.store, "alice").unwrap().is_empty());
    }
}
