//! Microsoft Graph as a [`SyncBackend`] (PERSO-213): the second transport the
//! one sync engine ([`crate::sync::engine::run_sync`]) drives.
//!
//! Until PERSO-213 `graph.rs` ran a loop of its own beside the engine,
//! mirroring its #0055/#0065/#0074 bookkeeping by hand and downloading every
//! new message of a folder before it ingested any (PERSO-301). What is
//! Graph's is here and in `graph.rs`: listing a folder by enumeration or by
//! delta walk, downloading by id a `/$batch` at a time, and the per-folder
//! maintenance that needs the folder listing. Ingest, flags, cursors, the
//! coverage gate and the deferred prune are the engine's.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{anyhow, Result};
use log::{info, warn};

use super::{
    backlog_owed, batch_fell_short, delta_verdict, folder_identity_hash, select_for_download,
    server_flags, vanished_graph_uids, BatchPacing, DeltaVerdict, FolderDelta, FolderEntry,
    FolderEnumeration, GraphClient, GraphFetched, BATCH_FAILURE_BUDGET, BATCH_FAILURE_WARN_LIMIT,
    BATCH_MAX_REQUESTS,
};
use crate::config::GraphConfig;
use crate::ingest::MailboxCursor;
use crate::store::{BlobStore, Store};
use crate::sync::engine::{run_sync, SyncRun};
use crate::sync::{
    stops_at, BodyBatch, FetchedMessage, Finish, FlagScope, MailboxCoverage, MailboxListing,
    NewMail, ObservationSink, Observed, ParsedMessage, ResumePoint, ResumeStale, Stop, StopPoint,
    SyncBackend, SyncResult, SyncTarget,
};
use crate::timing::TimingSpan;

/// What the store holds for one Graph folder: the Message-IDs of its rows,
/// which is what the listing dedups on since Graph has no UID, and the cursor
/// with the delta link and the folder identity it is bound to.
pub struct GraphKnown {
    pub(super) ids: HashSet<String>,
    pub(super) cursor: Option<MailboxCursor>,
}

/// The Graph transport behind [`SyncBackend`].
///
/// It owns the client and each folder's pass between the engine's calls, and
/// nothing the engine owns: no store handle and no cursor.
pub struct GraphBackend {
    client: GraphClient,
    /// A dry run reads no folder identity and mints no delta link, both of
    /// which exist only to be written.
    dry_run: bool,
    /// Each folder's pass of the last listing, by index; `None` for a folder
    /// whose listing failed, or once the engine has finished it.
    passes: Vec<Option<FolderPass>>,
    /// Whether this pass has queued the inline-only rows for the attachment
    /// repair, which it does once, before the first repair.
    swept: bool,
}

/// What one folder's listing walked.
pub(super) enum FolderView {
    /// The whole folder, and the delta link minted just before it was listed
    /// (`None` on a tenant that refuses one, and on a dry run).
    Enumeration {
        enumeration: FolderEnumeration,
        minted: Option<String>,
    },
    /// The change set since the stored link, which carries the next link.
    Delta(FolderDelta),
}

/// One folder's pass between the engine's calls.
pub(super) struct FolderPass {
    /// What the listing saw, a whole folder or a change set, keyed by trimmed
    /// `internetMessageId`: what the finish binds items and repairs from.
    pub(super) server: HashMap<String, FolderEntry>,
    /// The Graph ids still to download, newest first.
    pub(super) queue: VecDeque<String>,
    /// How many ids the listing asked for, for the failure warning.
    asked: usize,
    /// The throttle pacing and the failures counted against the budget.
    pacing: BatchPacing,
}

impl GraphBackend {
    pub fn new(client: GraphClient, dry_run: bool) -> Self {
        Self {
            client,
            dry_run,
            passes: Vec::new(),
            swept: false,
        }
    }

    /// List one folder: its identity, then a delta walk from the stored link
    /// when [`delta_verdict`] allows one, and the full enumeration otherwise.
    ///
    /// The delta is strictly an accelerator (#0042): the verdict refuses it on
    /// anything unusual, any doubt during the walk marks the stored link stale
    /// (the engine drops it) and enumerates, a delta that reports removals
    /// hands the pass back to the enumeration
    /// ([`FolderDelta::forces_full_enumeration`]), and a full sync always
    /// relists.
    async fn list_folder(
        &self,
        target: &SyncTarget,
        limit: usize,
        known: GraphKnown,
    ) -> Result<(MailboxListing, FolderPass)> {
        let GraphKnown { ids, cursor } = known;
        let stored_link = cursor.as_ref().and_then(|c| c.deltalink.clone());
        let stored_identity = cursor.as_ref().and_then(|c| c.uidvalidity);

        // The folder id, read fresh: the rename and recreation check a delta
        // link is bound to. An id that could not be read is no evidence the
        // folder is the same one, so it counts as a mismatch and the pass
        // enumerates; the *stored* identity is kept, because a failed lookup
        // is no evidence of a change either.
        let observed_identity = if self.dry_run {
            None
        } else {
            match self.client.folder_identity(&target.server_name).await {
                Ok(id) => Some(folder_identity_hash(&id)),
                Err(e) => {
                    warn!("Could not read the folder id of {}: {e:#}", target.role);
                    None
                }
            }
        };
        let identity_matches = observed_identity.is_some() && observed_identity == stored_identity;
        let identity = observed_identity.or(stored_identity);

        // A dry run writes nothing, and every delta branch is a write: it
        // either drops a link or records one. So a dry run enumerates.
        let verdict = if self.dry_run {
            DeltaVerdict::FullSync
        } else {
            delta_verdict(limit, stored_link.as_deref(), identity_matches)
        };
        let mut stale = false;
        if let DeltaVerdict::Discard(reason) = verdict {
            info!("Graph delta token for {} dropped ({reason:?})", target.role);
            stale = true;
        }
        if verdict == DeltaVerdict::FolderChanged {
            info!(
                "Graph folder id for {} is not the one its delta token was minted against; \
                 dropping the token and enumerating",
                target.role
            );
            stale = true;
        }

        let mut view = None;
        if verdict == DeltaVerdict::Use {
            let link = stored_link.unwrap_or_default();
            match self.client.walk_delta(&link).await {
                Ok(delta) => {
                    info!(
                        "Graph delta for '{}': {} page(s), {} changed, {} removed",
                        target.server_name,
                        delta.pages,
                        delta.changed.len(),
                        delta.removed,
                    );
                    if delta.forces_full_enumeration() {
                        info!(
                            "Graph delta for {} reported removals; enumerating the folder so \
                             the prune can resolve them",
                            target.role
                        );
                    } else {
                        view = Some(FolderView::Delta(delta));
                    }
                }
                Err(reason) => {
                    info!(
                        "Graph delta for {} abandoned ({reason:?}); dropping the token and \
                         enumerating",
                        target.role
                    );
                    stale = true;
                }
            }
        }
        let view = match view {
            Some(view) => view,
            None => {
                // Minted before the enumeration, never after: a link taken
                // afterwards would silently swallow the window between the two.
                let minted = if self.dry_run {
                    None
                } else {
                    self.client.mint_delta_token(&target.server_name).await
                };
                let enumeration = self
                    .client
                    .enumerate_folder(&target.server_name)
                    .await
                    .map_err(|e| listing_failed(e, stale))?;
                FolderView::Enumeration {
                    enumeration,
                    minted,
                }
            }
        };
        Ok(folder_listing(target, limit, &ids, view, identity, stale))
    }
}

/// The error of a folder listing that failed, marked [`ResumeStale`] when the
/// listing had already found the stored delta link stale, so the engine drops
/// the link whatever became of the enumeration that was to replace it.
pub(super) fn listing_failed(error: anyhow::Error, stale: bool) -> anyhow::Error {
    if stale {
        ResumeStale(error).into()
    } else {
        error
    }
}

/// The listing the engine sees, and the body pass behind it, from what one
/// folder's listing walked.
///
/// Pure, so a test drives the real engine over it with no server.
///
/// What it maps (PERSO-213):
///
/// - the UIDs are [`crate::ingest::graph_uid`] of the trimmed Message-ID, the
///   identity every Graph row is stored under;
/// - `listed` is empty: a Graph UID is a content hash, so no listing of them
///   can tell a duplicate copy from a moved row, and the engine's rebind gate
///   degrades to the unconditional rebind the Graph pass always used;
/// - only an enumeration may compute what has vanished: a change set names
///   only what changed, so `known − changed` would be the whole mailbox
///   (#0042);
/// - an enumeration cut short, or a change set, observes no count
///   ([`Observed::Partial`]);
/// - the coverage is the download by id: the cap and a short return both
///   leave the folder short (#0065), and only the cap owes a follow-up
///   ([`backlog_owed`]);
/// - the resume point is the link the walk ended on, or the one minted before
///   the enumeration.
pub(super) fn folder_listing(
    target: &SyncTarget,
    limit: usize,
    known: &HashSet<String>,
    view: FolderView,
    identity: Option<i64>,
    resume_stale: bool,
) -> (MailboxListing, FolderPass) {
    let (server, complete, link, change_set) = match view {
        FolderView::Enumeration {
            enumeration,
            minted,
        } => (enumeration.entries, enumeration.complete, minted, false),
        // A delta pass covers the folder in the sense the prune gate asks
        // about: its link asserts the store held everything the folder listed
        // when it was minted, and this walk brought in every change since.
        FolderView::Delta(delta) => (delta.changed, true, Some(delta.delta_link), true),
    };
    let (new, found) = select_for_download(&server, known, limit, target.window);
    let queue: VecDeque<String> = new.iter().map(|entry| entry.graph_id.clone()).collect();
    let asked = queue.len();
    let capped = found > asked;
    let skipped = if change_set { 0 } else { server.len() - found };
    let vanished = if change_set {
        Vec::new()
    } else {
        vanished_graph_uids(known, &server)
    };
    let known_flags = server
        .iter()
        .map(|(mid, entry)| (crate::ingest::graph_uid(mid), server_flags(entry)))
        .collect();
    let observed = if change_set || !complete {
        Observed::Partial { identity }
    } else {
        Observed::Folder {
            identity,
            exists: server.len(),
        }
    };
    let window = target.window;
    let listing = MailboxListing {
        skipped,
        known_flags,
        observed,
        resume_stale,
        // Read status, and the reply or forward Outlook recorded
        // (PERSO-171), merged rather than replaced.
        flag_scope: FlagScope::ReadAndVerb,
        vanished,
        listed: Vec::new(),
        uidvalidity_reset: false,
        enumeration_complete: complete,
        coverage: Box::new(move |downloaded: &[i64], bodies_complete: bool| {
            let fell_short = batch_fell_short(asked, downloaded.len(), !bodies_complete);
            MailboxCoverage {
                download_incomplete: capped || fell_short,
                // The pull is by id, with no positional window that can leave
                // an arrival behind.
                pending_arrival_mark: None,
                resume: link.map(ResumePoint::DeltaLink),
                backlog: backlog_owed(window, found, asked, fell_short),
            }
        }),
    };
    let pass = FolderPass {
        server,
        queue,
        asked,
        pacing: BatchPacing::default(),
    };
    (listing, pass)
}

/// A downloaded Graph message as the engine ingests it: under the hash of its
/// Message-ID, with its threading signals and its item.
pub(super) fn into_message(fetched: GraphFetched) -> FetchedMessage {
    let uid = crate::ingest::graph_uid(&crate::ingest::resolve_message_id(&fetched.email, None));
    FetchedMessage::parsed(
        uid,
        ParsedMessage {
            email: fetched.email,
            threading: fetched.threading,
            item: fetched.graph_id,
        },
    )
}

/// What a Graph folder's store holds: its rows' Message-IDs and its cursor.
pub(super) fn graph_known(store: &Store, account: &str, target: &SyncTarget) -> Result<GraphKnown> {
    let mailbox = target.role.as_str();
    Ok(GraphKnown {
        ids: crate::ingest::known_message_ids(store, account, mailbox)?,
        cursor: crate::ingest::load_mailbox_cursor(store, account, mailbox)?,
    })
}

/// The item each row of the folder stands for, which a move, delete or read
/// toggle of it acts on (PERSO-245): the rows this pass wrote and the ones
/// held before it, whose id a move elsewhere may have changed. Local, so even
/// a pass stopped in the folder binds what it wrote.
pub(super) fn bind_items(finish: &Finish<'_>, server: &HashMap<String, FolderEntry>) {
    crate::ingest::bind_graph_ids(
        finish.store,
        finish.account,
        finish.target.role.as_str(),
        server
            .iter()
            .map(|(mid, entry)| (mid.as_str(), entry.graph_id.as_str())),
    );
}

impl SyncBackend for GraphBackend {
    type Known = GraphKnown;

    fn known(store: &Store, account: &str, target: &SyncTarget) -> Result<GraphKnown> {
        graph_known(store, account, target)
    }

    /// List the folders one after another, asking the stop before each but
    /// the first: a folder's identity, delta walk or enumeration is a few
    /// requests, and Graph throttles a tenant that asks many at once.
    async fn list_targets(
        &mut self,
        targets: &[SyncTarget],
        limit: usize,
        knowns: Vec<GraphKnown>,
        stop: Option<&Stop>,
    ) -> Vec<Result<MailboxListing>> {
        self.passes.clear();
        self.swept = false;
        let mut out = Vec::with_capacity(targets.len());
        for (index, (target, known)) in targets.iter().zip(knowns).enumerate() {
            if index > 0 && stops_at(stop, StopPoint::Listing(index)) {
                self.passes.push(None);
                out.push(Err(anyhow!(
                    "stopped before listing '{}'",
                    target.server_name
                )));
                continue;
            }
            match self.list_folder(target, limit, known).await {
                Ok((listing, pass)) => {
                    self.passes.push(Some(pass));
                    out.push(Ok(listing));
                }
                Err(e) => {
                    self.passes.push(None);
                    out.push(Err(e));
                }
            }
        }
        out
    }

    /// The next `/$batch` of the folder's new messages, newest first, or the
    /// end of its body pass: run to the end, or given up once
    /// [`BATCH_FAILURE_BUDGET`] sub-requests have failed.
    async fn next_batch(&mut self, index: usize) -> Result<BodyBatch> {
        let Self { client, passes, .. } = self;
        let Some(pass) = passes.get_mut(index).and_then(Option::as_mut) else {
            return Err(anyhow!("no listing for the folder at {index}"));
        };
        let gave_up = !pass.queue.is_empty() && pass.pacing.failures >= BATCH_FAILURE_BUDGET;
        if pass.queue.is_empty() || gave_up {
            if gave_up {
                warn!(
                    "Giving up on this pass's remaining message downloads after {} failed \
                     sub-requests; {} message(s) stay new",
                    pass.pacing.failures,
                    pass.queue.len(),
                );
                pass.queue.clear();
            }
            if pass.pacing.failures > BATCH_FAILURE_WARN_LIMIT {
                warn!(
                    "{} of {} batch sub-requests failed this pass (first \
                     {BATCH_FAILURE_WARN_LIMIT} logged above); those messages stay new",
                    pass.pacing.failures, pass.asked,
                );
            }
            return Ok(BodyBatch::Done { complete: !gave_up });
        }
        let take = pass.queue.len().min(BATCH_MAX_REQUESTS);
        let chunk: Vec<String> = pass.queue.drain(..take).collect();
        let ids: Vec<&str> = chunk.iter().map(String::as_str).collect();
        match client.fetch_batch_chunk(&ids, &mut pass.pacing).await {
            Ok(fetched) => Ok(BodyBatch::Bodies(
                fetched.into_iter().map(into_message).collect(),
            )),
            Err(e) => {
                pass.queue.clear();
                Err(e)
            }
        }
    }

    /// The pull is by id, newest first, so a backlog drain and a first sync
    /// insert mail that is old to its owner (PERSO-172).
    fn new_mail(&self) -> NewMail {
        NewMail::NewerThanHeld
    }

    /// Bind each row to its item, then, unless the pass stops here, the
    /// maintenance that needs the folder listing: the attachments a first
    /// fetch could not bring, the threading signals and the inline marks an
    /// older build did not store.
    async fn finish_target(&mut self, index: usize, finish: &Finish<'_>) {
        let Some(pass) = self.passes.get_mut(index).and_then(Option::take) else {
            return;
        };
        bind_items(finish, &pass.server);
        if finish.stopped {
            return;
        }
        let Finish {
            store,
            blobs,
            account,
            target,
            ..
        } = *finish;
        let mailbox = target.role.as_str();
        // The inline-only rows an older build stored (PERSO-165), queued once
        // per store, before the first repair can take them.
        if !self.swept {
            self.swept = true;
            match crate::ingest::mark_inline_only_rows(store, blobs, account) {
                Ok(0) => {}
                Ok(n) => info!("Queued {n} inline-only message(s) for the attachment repair"),
                Err(e) => warn!("Could not queue the inline-only messages: {e:#}"),
            }
        }
        self.client
            .repair_missing_attachments(
                store,
                blobs,
                account,
                mailbox,
                &pass.server,
                crate::outbox::unix_now(),
            )
            .await;
        self.client.rethread_stored(store, account, target).await;
        self.client
            .classify_stored_inline(store, blobs, account, mailbox, &pass.server)
            .await;
    }
}

/// Sync every target of a Graph account, store-only, through the engine.
///
/// The wiring only: it opens the client and the store, builds the backend and
/// hands both to [`run_sync`]. Two documented differences from IMAP rows are
/// forced by the API and are the backend's: Graph never returns the original
/// RFC 822, so rows get no raw blob, and Graph has no UID, so a row's `uid`
/// is [`crate::ingest::graph_uid`] of its Message-ID.
///
/// Unguarded, as the Graph pass has always been: the engine lock of #0122
/// covers the IMAP ingest, and the daemon's runtime that calls this holds the
/// account's lock already.
///
/// `stop` is asked before the listing, between two folders' listings, before
/// each folder's ingest and between two of its batches (PERSO-127,
/// PERSO-300); a stopped pass returns what it wrote, and runs no prune.
///
/// Every row the pass inserts is recorded in `observations` as it is
/// inserted (PERSO-234): a quick tick cut off at its ceiling drops this
/// future, and the rows it committed before that must still reach the
/// contacts index.
pub async fn sync_mailboxes(
    config: &GraphConfig,
    account_name: &str,
    targets: &[SyncTarget],
    limit: usize,
    dry_run: bool,
    stop: Option<&Stop>,
    observations: &ObservationSink,
) -> Result<SyncResult> {
    info!(
        "graph::sync_mailboxes: account={account_name}, {} targets, limit={limit}, \
         dry_run={dry_run}",
        targets.len(),
    );
    let span_label = if limit < usize::MAX {
        "graph_sync:quick"
    } else {
        "graph_sync:full"
    };
    let mut span = TimingSpan::with_context(span_label, format!("{} targets", targets.len()));
    let client = GraphClient::new_async(config).await?;
    let store = Store::open_for_pass(account_name, dry_run)?;
    let blobs = BlobStore::for_account(account_name);
    span.mark("client_open");
    let mut backend = GraphBackend::new(client, dry_run);
    run_sync(
        &mut backend,
        &SyncRun {
            store: &store,
            blobs: &blobs,
            account: account_name,
            targets,
            limit,
            dry_run,
            stop,
            observations,
        },
        &mut span,
    )
    .await
}
