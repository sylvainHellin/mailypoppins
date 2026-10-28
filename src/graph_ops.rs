//! Per-message Graph ops: move, delete and the read toggle, each acting on the
//! Graph item the store row stood for (PERSO-245).
//!
//! The Graph twin of [`crate::imap_client`]'s per-message ops and of their
//! PERSO-238 rule. They used to look the message up by `Message-ID` across the
//! whole mailbox and act on the first item Graph listed, which could be a copy
//! in another folder: a delete from the inbox could take the copy filed in
//! Archive. Each op now looks in its own folder only, and acts on the item
//! whose id the row carried when the op was queued ([`GraphCopy`]).
//!
//! Every op touches nothing locally: the local half is the store row the caller
//! wrote optimistically (`crate::store::write`), and the result here is what
//! tells it whether to keep that write or put it back.
//!
//! Contract: a source folder with no copy left is a not-found with
//! `none_left`, which a caller's settle counts as a delete done and fails as a
//! move "already moved or deleted elsewhere" (`crate::pending_ops`, PERSO-245).

use anyhow::{anyhow, Result};
use log::info;

use crate::config::GraphConfig;
use crate::graph::GraphClient;
use crate::ops::GraphCopy;

/// The Graph requests the ops below are made of, behind a trait so their rules
/// are pinned against an in-memory mailbox: no live tenant reproduces two
/// copies of one message in a folder on demand.
pub(crate) trait GraphItems {
    /// The ids of every item `folder` holds under `message_id`.
    async fn folder_copies(&self, folder: &str, message_id: &str) -> Result<Vec<String>>;
    /// `POST /me/messages/{id}/move` into `dest_folder`.
    async fn move_item(&self, id: &str, dest_folder: &str) -> Result<()>;
    /// `DELETE /me/messages/{id}`.
    async fn delete_item(&self, id: &str) -> Result<()>;
    /// `PATCH /me/messages/{id}` with `isRead`.
    async fn set_read(&self, id: &str, read: bool) -> Result<()>;
}

impl GraphItems for GraphClient {
    async fn folder_copies(&self, folder: &str, message_id: &str) -> Result<Vec<String>> {
        GraphClient::folder_copies(self, folder, message_id).await
    }

    async fn move_item(&self, id: &str, dest_folder: &str) -> Result<()> {
        self.move_message(id, dest_folder).await
    }

    async fn delete_item(&self, id: &str) -> Result<()> {
        self.delete_message(id).await
    }

    async fn set_read(&self, id: &str, read: bool) -> Result<()> {
        self.update_read_status(id, read).await
    }
}

/// What a folder holds of the item an op names ([`resolve_item`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// The item to act on, by Graph id.
    Item(String),
    /// The folder holds no item under the `Message-ID`.
    Gone,
    /// The folder holds items under the `Message-ID`, none of which can be
    /// taken for the op's: its own id is not among them, or the op names none
    /// and there are several. `unbound` is whether any of them is bound to no
    /// other store row.
    Missed { unbound: bool },
}

/// The item in `folder` an op on `message_id` acts on (PERSO-245).
///
/// - With a [`GraphCopy`] whose id the folder holds under the `Message-ID`,
///   that item, whatever else the folder holds.
/// - With a [`GraphCopy`] whose id the folder does not hold: [`Resolved::Gone`]
///   when it holds no copy, [`Resolved::Missed`] otherwise, and nothing is
///   acted on. Unlike a stale IMAP UID, no remaining copy is taken for the
///   row's: the store keeps one row per folder and `Message-ID` on a Graph
///   account ([`crate::ingest::graph_uid`]), so a twin has no row whose id
///   would mark it, and a replay of a move that landed would move it too.
/// - With none (a row a local move took here, a row synced before the id was
///   recorded, a payload queued before the field existed), the folder's one
///   copy; several are [`Resolved::Missed`], where the old lookup acted on
///   whichever the server listed first.
pub(crate) async fn resolve_item<G: GraphItems>(
    graph: &G,
    folder: &str,
    message_id: &str,
    copy: Option<&GraphCopy>,
) -> Result<Resolved> {
    let found = graph.folder_copies(folder, message_id).await?;
    if found.is_empty() {
        return Ok(Resolved::Gone);
    }
    match copy {
        Some(copy) if found.contains(&copy.id) => Ok(Resolved::Item(copy.id.clone())),
        Some(copy) => {
            let unbound = found.iter().any(|id| !copy.siblings.contains(id));
            info!(
                "Graph item {} of {message_id} is not in {folder} any more; its {} cop(ies) \
                 there are not taken for it",
                copy.id,
                found.len()
            );
            Ok(Resolved::Missed { unbound })
        }
        None => match <[String; 1]>::try_from(found) {
            Ok([only]) => Ok(Resolved::Item(only)),
            Err(found) => {
                info!(
                    "{message_id} is in {folder} {} times and the op names no item; acting on none",
                    found.len()
                );
                Ok(Resolved::Missed { unbound: true })
            }
        },
    }
}

/// The typed not-found of an op whose item is gone, so the durable queue's
/// drain converges a replay whose op already landed (#0039 review).
/// `none_left` is whether the folder holds no copy at all ([`Resolved::Gone`]).
fn not_found(message_id: &str, folder: Option<&str>, none_left: bool) -> anyhow::Error {
    crate::ops::NotFoundOnServer {
        message_id: message_id.to_string(),
        mailbox: folder.map(str::to_string),
        none_left,
    }
    .into()
}

/// A move or delete that acts on nothing although the folder still holds
/// copies, when no completed run of the op explains the miss: an ordinary
/// error, which the drain retries and then rolls back, so the store shows the
/// message where the server still holds it.
fn unsettled(message_id: &str, folder: &str, copy: Option<&GraphCopy>) -> anyhow::Error {
    let item = copy.map_or_else(
        || "no Graph item was recorded for the row".to_string(),
        |copy| format!("Graph item {} is not there any more", copy.id),
    );
    anyhow!(
        "{message_id} cannot be told apart in {folder}: {item}, and the copies {folder} holds \
         now may be other ones; nothing was changed on the server"
    )
}

/// The item a move of `message_id` from `source` to `dest` acts on, or why it
/// acts on none.
///
/// A miss whose remaining copies all stand for other rows, with the message
/// already in the destination, is what a replay of a move that landed leaves
/// (PERSO-236): it converges as not found, as a source with no copy does. Any
/// other miss is [`unsettled`].
pub(crate) async fn item_to_move<G: GraphItems>(
    graph: &G,
    message_id: &str,
    source: &str,
    copy: Option<&GraphCopy>,
    dest: &str,
) -> Result<String> {
    match resolve_item(graph, source, message_id, copy).await? {
        Resolved::Item(id) => Ok(id),
        Resolved::Gone => Err(not_found(message_id, Some(source), true)),
        Resolved::Missed { unbound: false }
            if !graph.folder_copies(dest, message_id).await?.is_empty() =>
        {
            info!(
                "{message_id} is already in {dest} and its item left {source}: a move that landed"
            );
            Err(not_found(message_id, Some(source), false))
        }
        Resolved::Missed { .. } => Err(unsettled(message_id, source, copy)),
    }
}

/// The item a delete of `message_id` from `source` acts on, or why it acts on
/// none: a miss whose remaining copies all stand for other rows converges as
/// not found, as a source with no copy does; any other is [`unsettled`].
pub(crate) async fn item_to_delete<G: GraphItems>(
    graph: &G,
    message_id: &str,
    source: &str,
    copy: Option<&GraphCopy>,
) -> Result<String> {
    match resolve_item(graph, source, message_id, copy).await? {
        Resolved::Item(id) => Ok(id),
        Resolved::Gone => Err(not_found(message_id, Some(source), true)),
        Resolved::Missed { unbound: false } => Err(not_found(message_id, Some(source), false)),
        Resolved::Missed { unbound: true } => Err(unsettled(message_id, source, copy)),
    }
}

async fn move_on<G: GraphItems>(
    graph: &G,
    message_id: &str,
    source: &str,
    copy: Option<&GraphCopy>,
    dest: &str,
) -> Result<()> {
    let id = item_to_move(graph, message_id, source, copy, dest).await?;
    graph.move_item(&id, dest).await?;
    info!("Graph: moved {message_id} from {source} to {dest}");
    Ok(())
}

async fn delete_on<G: GraphItems>(
    graph: &G,
    message_id: &str,
    source: &str,
    copy: Option<&GraphCopy>,
) -> Result<()> {
    let id = item_to_delete(graph, message_id, source, copy).await?;
    graph.delete_item(&id).await?;
    info!("Graph: deleted {message_id} from {source}");
    Ok(())
}

/// The read toggle: a folder with no copy is the typed not-found the drain
/// converges a replay on, as it always was on Graph; a copy that cannot be
/// told from a twin is left alone, as the IMAP arm leaves it, since the next
/// sync restates what the server holds.
async fn mark_read_on<G: GraphItems>(
    graph: &G,
    message_id: &str,
    mailbox: &str,
    copy: Option<&GraphCopy>,
    read: bool,
) -> Result<()> {
    match resolve_item(graph, mailbox, message_id, copy).await? {
        Resolved::Item(id) => graph.set_read(&id, read).await,
        Resolved::Gone => Err(not_found(message_id, None, true)),
        Resolved::Missed { .. } => Ok(()),
    }
}

/// Move a message between Graph folders (#0018); archiving is this with the
/// archive folder as destination.
pub async fn move_message(
    config: &GraphConfig,
    message_id: &str,
    source: &str,
    copy: Option<&GraphCopy>,
    dest: &str,
) -> Result<()> {
    let client = GraphClient::new_async(config).await?;
    move_on(&client, message_id, source, copy, dest).await
}

/// Delete a message from a Graph folder.
pub async fn delete_message(
    config: &GraphConfig,
    message_id: &str,
    source: &str,
    copy: Option<&GraphCopy>,
) -> Result<()> {
    let client = GraphClient::new_async(config).await?;
    delete_on(&client, message_id, source, copy).await
}

/// Mark a message in a Graph folder read or unread.
pub async fn mark_read(
    config: &GraphConfig,
    message_id: &str,
    mailbox: &str,
    copy: Option<&GraphCopy>,
    read: bool,
) -> Result<()> {
    let client = GraphClient::new_async(config).await?;
    mark_read_on(&client, message_id, mailbox, copy, read).await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::ops::NotFoundOnServer;

    /// An in-memory Graph mailbox: folders of items, each a Graph id and a
    /// `Message-ID`, and the requests the ops sent.
    #[derive(Default)]
    struct Mailbox {
        folders: Mutex<BTreeMap<String, Vec<(String, String)>>>,
        next: Mutex<u32>,
        requests: Mutex<Vec<String>>,
    }

    impl Mailbox {
        fn with(items: &[(&str, &str, &str)]) -> Mailbox {
            let mailbox = Mailbox::default();
            for (folder, id, message_id) in items {
                mailbox
                    .folders
                    .lock()
                    .unwrap()
                    .entry(folder.to_string())
                    .or_default()
                    .push((id.to_string(), message_id.to_string()));
            }
            mailbox
        }

        fn ids_in(&self, folder: &str) -> Vec<String> {
            self.folders
                .lock()
                .unwrap()
                .get(folder)
                .map(|items| items.iter().map(|(id, _)| id.clone()).collect())
                .unwrap_or_default()
        }

        fn acted(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| !r.starts_with("GET"))
                .cloned()
                .collect()
        }

        fn take(&self, id: &str) -> Result<(String, String)> {
            let mut folders = self.folders.lock().unwrap();
            for items in folders.values_mut() {
                if let Some(at) = items.iter().position(|(i, _)| i == id) {
                    return Ok(items.remove(at));
                }
            }
            Err(anyhow!("HTTP 404 from the fake: no item {id}"))
        }
    }

    impl GraphItems for Mailbox {
        async fn folder_copies(&self, folder: &str, message_id: &str) -> Result<Vec<String>> {
            self.requests
                .lock()
                .unwrap()
                .push(format!("GET {folder} {message_id}"));
            Ok(self
                .folders
                .lock()
                .unwrap()
                .get(folder)
                .map(|items| {
                    items
                        .iter()
                        .filter(|(_, mid)| mid == message_id)
                        .map(|(id, _)| id.clone())
                        .collect()
                })
                .unwrap_or_default())
        }

        /// A move gives the item a new id, as Graph's default ids do.
        async fn move_item(&self, id: &str, dest_folder: &str) -> Result<()> {
            self.requests
                .lock()
                .unwrap()
                .push(format!("MOVE {id} {dest_folder}"));
            let (_, message_id) = self.take(id)?;
            let mut next = self.next.lock().unwrap();
            *next += 1;
            self.folders
                .lock()
                .unwrap()
                .entry(dest_folder.to_string())
                .or_default()
                .push((format!("moved-{next}"), message_id));
            Ok(())
        }

        async fn delete_item(&self, id: &str) -> Result<()> {
            self.requests.lock().unwrap().push(format!("DELETE {id}"));
            self.take(id).map(|_| ())
        }

        async fn set_read(&self, id: &str, read: bool) -> Result<()> {
            self.requests
                .lock()
                .unwrap()
                .push(format!("READ {id} {read}"));
            Ok(())
        }
    }

    const DUP: &str = "<dup@example.com>";

    fn named(id: &str) -> GraphCopy {
        GraphCopy::of_row(id.to_string(), [])
    }

    fn is_not_found(e: &anyhow::Error) -> bool {
        NotFoundOnServer::is_in(e)
    }

    /// Two items of one message in Inbox, and a third filed in Archive: each
    /// op acts on the item its row named, never on a twin and never on the
    /// copy in another folder, which the old mailbox-wide lookup could take.
    #[tokio::test]
    async fn an_op_acts_on_the_item_its_row_named() {
        let mailbox = Mailbox::with(&[
            ("Archive", "archived", DUP),
            ("Inbox", "first", DUP),
            ("Inbox", "second", DUP),
        ]);
        move_on(&mailbox, DUP, "Inbox", Some(&named("second")), "Receipts")
            .await
            .unwrap();
        assert_eq!(mailbox.acted(), ["MOVE second Receipts"]);
        assert_eq!(mailbox.ids_in("Inbox"), ["first"]);
        assert_eq!(mailbox.ids_in("Archive"), ["archived"]);

        delete_on(&mailbox, DUP, "Inbox", Some(&named("first")))
            .await
            .unwrap();
        mark_read_on(&mailbox, DUP, "Archive", Some(&named("archived")), true)
            .await
            .unwrap();
        assert_eq!(
            mailbox.acted(),
            ["MOVE second Receipts", "DELETE first", "READ archived true"]
        );
    }

    /// An op that names no item takes the folder's one copy, in its own folder
    /// only: the copy filed in Archive is not the inbox row's.
    #[tokio::test]
    async fn an_op_naming_no_item_takes_the_folders_one_copy() {
        let mailbox = Mailbox::with(&[("Archive", "archived", DUP), ("Inbox", "only", DUP)]);
        delete_on(&mailbox, DUP, "Inbox", None).await.unwrap();
        assert_eq!(mailbox.acted(), ["DELETE only"]);
        assert_eq!(mailbox.ids_in("Archive"), ["archived"]);
    }

    /// Two copies and no item named, or a named item that is no longer in the
    /// folder beside a copy that is: nothing is acted on, and a move or delete
    /// fails as an ordinary error, which the drain retries and rolls back,
    /// rather than as a not-found it would retire as converged.
    #[tokio::test]
    async fn an_op_that_cannot_tell_its_item_acts_on_none() {
        let twins = || Mailbox::with(&[("Inbox", "first", DUP), ("Inbox", "second", DUP)]);

        let mailbox = twins();
        let e = move_on(&mailbox, DUP, "Inbox", None, "Archive")
            .await
            .unwrap_err();
        assert!(!is_not_found(&e), "{e:#}");
        assert!(mailbox.acted().is_empty());

        let mailbox = twins();
        let e = delete_on(&mailbox, DUP, "Inbox", Some(&named("stale")))
            .await
            .unwrap_err();
        assert!(!is_not_found(&e), "{e:#}");
        assert!(mailbox.acted().is_empty());

        // The one copy left is not taken for a stale id either: on a Graph
        // account it may be a twin no row stands for.
        let mailbox = Mailbox::with(&[("Inbox", "other", DUP)]);
        let e = move_on(&mailbox, DUP, "Inbox", Some(&named("stale")), "Archive")
            .await
            .unwrap_err();
        assert!(!is_not_found(&e), "{e:#}");
        assert!(mailbox.acted().is_empty());

        // A read toggle leaves it alone.
        let mailbox = twins();
        mark_read_on(&mailbox, DUP, "Inbox", None, true)
            .await
            .unwrap();
        assert!(mailbox.acted().is_empty());
    }

    /// A replay of a move that landed finds its item gone from the source and
    /// converges as not found, without moving the twin left behind when that
    /// twin is another row's.
    #[tokio::test]
    async fn a_replayed_move_converges_without_moving_a_twin() {
        let mailbox = Mailbox::with(&[("Inbox", "mine", DUP), ("Inbox", "twin", DUP)]);
        let copy = GraphCopy::of_row("mine".to_string(), ["twin".to_string()]);
        move_on(&mailbox, DUP, "Inbox", Some(&copy), "Archive")
            .await
            .unwrap();
        let e = move_on(&mailbox, DUP, "Inbox", Some(&copy), "Archive")
            .await
            .unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
        assert_eq!(mailbox.acted(), ["MOVE mine Archive"]);
        assert_eq!(mailbox.ids_in("Inbox"), ["twin"]);

        // A delete replay converges too, and so does a source with no copy.
        // Only the second has none left, which a caller's settle counts as
        // a delete done: beside the twin, the message may not be gone
        // (PERSO-245).
        let e = delete_on(&mailbox, DUP, "Inbox", Some(&copy))
            .await
            .unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
        assert!(NotFoundOnServer::none_left_in(&e).is_none(), "{e:#}");
        let e = delete_on(&mailbox, DUP, "Drafts", None).await.unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
        assert!(NotFoundOnServer::none_left_in(&e).is_some(), "{e:#}");
        assert_eq!(mailbox.acted(), ["MOVE mine Archive"]);
    }

    /// The whole chain on a folder holding two items of one message: the
    /// listing binds the row to one of them, the move the user queues on the
    /// row carries that item, and the server moves that item and not its twin.
    #[tokio::test]
    async fn a_queued_move_acts_on_the_item_its_row_was_bound_to() {
        let fx = crate::reconcile::tests::fixture();
        let row = fx.ingest_plain("inbox", crate::ingest::graph_uid(DUP), "Dup");
        fx.store
            .conn()
            .execute("UPDATE messages SET message_id = ?1", [DUP])
            .unwrap();
        assert_eq!(
            crate::ingest::bind_graph_ids(&fx.store, "alice", "inbox", [(DUP, "second")]),
            1
        );
        crate::mutations::queue_move(
            &fx.store,
            "alice",
            &[row],
            "archive",
            "Inbox",
            "Archive",
            crate::pending_ops::Runner::Drain,
        );
        let queued = crate::pending_ops::queued_ops(&fx.store, "alice").unwrap();
        let crate::ops::ServerOp::Move {
            message_id,
            source_mailbox,
            dest_mailbox,
            graph_copy,
            ..
        } = &queued[0].op
        else {
            panic!("{:?}", queued[0].op);
        };

        let mailbox = Mailbox::with(&[("Inbox", "first", DUP), ("Inbox", "second", DUP)]);
        move_on(
            &mailbox,
            message_id,
            source_mailbox,
            graph_copy.as_ref(),
            dest_mailbox,
        )
        .await
        .unwrap();
        assert_eq!(mailbox.acted(), ["MOVE second Archive"]);
        assert_eq!(mailbox.ids_in("Inbox"), ["first"]);
    }

    /// A caller's settle (`mp delete`, `mp move`) over the in-memory mailbox:
    /// the op the queue builds off the store row, run by
    /// [`crate::pending_ops::run_and_settle`] against the real resolution, and
    /// what the settle does with its answer (PERSO-245).
    mod settle {
        use super::*;
        use crate::ops::ServerOp;
        use crate::pending_ops::{self, OlderOps, OpExecutor, Runner, SettleScope};
        use crate::reconcile::tests::{fixture, Fixture};
        use crate::store::read;

        /// The mailbox as the server a settle runs its op on.
        struct Server<'a>(&'a Mailbox);

        impl OpExecutor for Server<'_> {
            async fn execute(&mut self, op: &ServerOp) -> Result<()> {
                match op {
                    ServerOp::Move {
                        message_id,
                        source_mailbox,
                        dest_mailbox,
                        graph_copy,
                        ..
                    } => {
                        move_on(
                            self.0,
                            message_id,
                            source_mailbox,
                            graph_copy.as_ref(),
                            dest_mailbox,
                        )
                        .await
                    }
                    ServerOp::Delete {
                        message_id,
                        source_mailbox,
                        graph_copy,
                        ..
                    } => delete_on(self.0, message_id, source_mailbox, graph_copy.as_ref()).await,
                    other => Err(anyhow!("no {} here", other.kind())),
                }
            }
        }

        /// The inbox row of [`DUP`], bound to the Graph item `bound` when
        /// there is one.
        fn row(bound: Option<&str>) -> (Fixture, i64) {
            let fx = fixture();
            let row = fx.ingest_plain("inbox", crate::ingest::graph_uid(DUP), "Dup");
            fx.store
                .conn()
                .execute("UPDATE messages SET message_id = ?1", [DUP])
                .unwrap();
            if let Some(id) = bound {
                assert_eq!(
                    crate::ingest::bind_graph_ids(&fx.store, "alice", "inbox", [(DUP, id)]),
                    1
                );
            }
            (fx, row)
        }

        /// Commit `queue`'s op for the caller and settle it on `mailbox`.
        async fn settled(
            fx: &Fixture,
            mailbox: &Mailbox,
            queue: impl FnOnce(Runner<'_>) -> Vec<crate::mutations::Queued>,
        ) -> Result<()> {
            let scope = SettleScope::open(&fx.store);
            let queued = queue(Runner::Caller(&scope));
            let nudge = || {};
            pending_ops::run_and_settle(
                &fx.store,
                &fx.blobs,
                &scope,
                queued[0].op_id,
                &mut Server(mailbox),
                OlderOps::wait(&nudge),
            )
            .await
        }

        async fn deleted(fx: &Fixture, row: i64, mailbox: &Mailbox) -> Result<()> {
            settled(fx, mailbox, |runner| {
                crate::mutations::queue_delete(
                    &fx.store,
                    &fx.blobs,
                    "alice",
                    &[row],
                    "Inbox",
                    runner,
                )
            })
            .await
        }

        async fn archived(fx: &Fixture, row: i64, mailbox: &Mailbox) -> Result<()> {
            settled(fx, mailbox, |runner| {
                crate::mutations::queue_move(
                    &fx.store,
                    "alice",
                    &[row],
                    "archive",
                    "Inbox",
                    "Archive",
                    runner,
                )
            })
            .await
        }

        fn no_ops(fx: &Fixture) -> bool {
            pending_ops::unsettled_ops(&fx.store, "alice")
                .unwrap()
                .is_empty()
                && pending_ops::failed_ops(&fx.store, "alice")
                    .unwrap()
                    .is_empty()
        }

        /// The user deleted or moved the message on the phone before the next
        /// sync: Inbox holds no copy. The delete's goal is reached, so it
        /// succeeds and the local delete stands, where it used to fail with
        /// a not-found; the copy in Archive is not touched.
        #[tokio::test]
        async fn a_delete_of_a_message_gone_from_its_folder_counts_as_done() {
            let (fx, row) = row(Some("mine"));
            let mailbox = Mailbox::with(&[("Archive", "filed", DUP)]);

            deleted(&fx, row, &mailbox).await.unwrap();

            assert!(read::find_by_id(&fx.store, row).unwrap().is_none());
            assert!(no_ops(&fx));
            assert!(mailbox.acted().is_empty(), "{:?}", mailbox.acted());
            assert_eq!(mailbox.ids_in("Archive"), ["filed"]);
        }

        /// The same for a move: its destination is unknown, so it fails with
        /// the message saying so and the row goes back to Inbox, where the
        /// next sync finds where the message went.
        #[tokio::test]
        async fn a_move_of_a_message_gone_from_its_folder_fails_and_goes_home() {
            let (fx, row) = row(Some("mine"));
            let mailbox = Mailbox::with(&[("Receipts", "filed", DUP)]);

            let err = archived(&fx, row, &mailbox).await.unwrap_err();

            assert_eq!(
                format!("{err:#}"),
                format!(
                    "Email with Message-ID {DUP} is no longer in Inbox on server: already moved \
                     or deleted elsewhere; the next sync will show where"
                )
            );
            let back = read::find_by_id(&fx.store, row).unwrap().unwrap();
            assert_eq!(back.mailbox, "inbox");
            assert!(no_ops(&fx));
            assert!(mailbox.acted().is_empty(), "{:?}", mailbox.acted());
        }

        /// Inbox holds two copies and the row names neither, or names an
        /// item Inbox does not hold beside a copy it does: the message is
        /// not gone, and the delete still fails without touching either.
        #[tokio::test]
        async fn a_delete_that_cannot_tell_its_item_still_fails() {
            let (fx, row) = self::row(None);
            let mailbox = Mailbox::with(&[("Inbox", "first", DUP), ("Inbox", "second", DUP)]);
            let err = deleted(&fx, row, &mailbox).await.unwrap_err();
            assert!(err.to_string().contains("cannot be told apart"), "{err:#}");
            assert!(mailbox.acted().is_empty(), "{:?}", mailbox.acted());
            assert!(no_ops(&fx));

            let (fx, row) = self::row(Some("stale"));
            let mailbox = Mailbox::with(&[("Inbox", "other", DUP)]);
            let err = deleted(&fx, row, &mailbox).await.unwrap_err();
            assert!(err.to_string().contains("cannot be told apart"), "{err:#}");
            assert!(mailbox.acted().is_empty(), "{:?}", mailbox.acted());
            assert_eq!(mailbox.ids_in("Inbox"), ["other"]);
        }
    }

    /// The new lookup is scoped to the source folder.
    #[tokio::test]
    async fn the_lookup_reads_the_source_folder_only() {
        let mailbox = Mailbox::with(&[("Inbox", "only", DUP)]);
        move_on(&mailbox, DUP, "Inbox", Some(&named("only")), "Archive")
            .await
            .unwrap();
        assert_eq!(
            mailbox.requests.lock().unwrap().as_slice(),
            [format!("GET Inbox {DUP}"), "MOVE only Archive".to_string()]
        );
    }
}
