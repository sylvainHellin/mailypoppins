//! IMAP sync, store-only.
//!
//! Replaces the `.md`-writing orchestrator: one IMAP session per sync, and
//! every message that comes back goes through [`crate::ingest`] into the
//! per-account `store.sqlite3` plus its blob store. Nothing is written to the
//! mailbox directories.
//!
//! Three things the old path did are gone with it, because the store makes
//! them unnecessary rather than merely cheaper:
//!
//! - the local Message-ID scan that decided what was new (identity is the UID,
//!   and the store answers "which UIDs do I hold" with one query);
//! - the dedup pass over the mailbox directory (the unique constraint on
//!   `(account, mailbox, uid)` makes a duplicate impossible);
//! - the EXISTS/UIDNEXT reconciliation heuristic and its `mailbox-states.json`
//!   cache (superseded by `sync_cursors`; a row the server no longer lists is
//!   pruned from the fetch's own enumeration of the mailbox, see
//!   [`crate::imap_client::vanished_uids`]).

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use log::{info, warn};

use super::fetch::{
    fetch_body_chunk_on_session, list_mailbox_on_session, stop_before_chunk, BodyPlan,
};
use super::pool::{self, PooledSession};
use crate::config::ImapConfig;
use crate::ingest::KnownUids;
use crate::store::{BlobStore, Store};
use crate::sync::engine::{run_sync_guarded, SyncRun};
use crate::sync::{
    BodyBatch, MailboxListing, ObservationSink, Stop, SyncBackend, SyncResult, SyncTarget,
};
use crate::timing::TimingSpan;

/// The IMAP transport behind [`crate::sync::SyncBackend`] (#0059).
///
/// It owns what a fetch needs and nothing the engine owns: no store handle, no
/// cursors, no ingest bookkeeping. That is the config, the body budget and
/// what the pass has spent of it, and each mailbox's body pass between the
/// engine's batches (PERSO-300); #0041
/// put the sessions themselves in [`super::pool`] rather than in this struct,
/// because they have to outlive the backend (a `mp sync` builds one per call,
/// and the queued-op drain has no backend at all), and the pool is what every
/// IMAP path in the process borrows from.
pub struct ImapBackend<'a> {
    config: &'a ImapConfig,
    /// How much network time the pass may spend downloading bodies, summed
    /// over its mailboxes, before the mailbox it is in and every one after it
    /// stop and leave the rest for the next pass (#0113, PERSO-300). `None` is
    /// unbounded.
    body_budget: Option<Duration>,
    /// The network time the pass has spent on bodies so far, every mailbox
    /// counted: what the budget is measured against ([`stop_before_chunk`]).
    body_spent: Duration,
    /// How many chunks of bodies the pass has asked for, every mailbox
    /// counted.
    chunks_asked: usize,
    /// The body pass of each target of the last listing, by index; `None` for
    /// a target whose listing failed.
    passes: Vec<Option<BodyPass>>,
}

/// One mailbox's body pass between two of the engine's batches (PERSO-300).
struct BodyPass {
    plan: BodyPlan,
    /// The session the pass `SELECT`ed its mailbox on, borrowed at its first
    /// chunk and returned to the pool when the pass ends, so a pass holds one
    /// connection only while the engine is working through it.
    session: Option<PooledSession>,
}

impl<'a> ImapBackend<'a> {
    pub fn new(config: &'a ImapConfig) -> Self {
        Self {
            config,
            body_budget: None,
            body_spent: Duration::ZERO,
            chunks_asked: 0,
            passes: Vec::new(),
        }
    }

    /// Bound the network time the pass spends downloading bodies (#0113).
    ///
    /// The budget is the pass's, every mailbox counted, not each mailbox's
    /// (PERSO-300): the mailboxes download one after another, and what the
    /// budget is for is that one account's slow download does not hold up the
    /// daemon's serial tick across accounts. A budget per mailbox let a pass
    /// catching up six mailboxes spend six. The listings, which run in
    /// parallel before any body, are not charged to it.
    pub fn with_body_budget(mut self, budget: Option<Duration>) -> Self {
        self.body_budget = budget;
        self
    }
}

impl SyncBackend for ImapBackend<'_> {
    type Known = KnownUids;

    /// The UIDs the store holds, with the UIDVALIDITY, the arrival mark and
    /// the CONDSTORE mark the last pass recorded.
    fn known(store: &Store, account: &str, target: &SyncTarget) -> Result<KnownUids> {
        crate::ingest::known_uids_with_cursor(store, account, target.role.as_str())
    }

    /// List every mailbox in parallel, one IMAP session each (#0005).
    ///
    /// IMAP allows one SELECTed mailbox per connection, so the old single
    /// session paid `N * latency` for N mailboxes; N connections overlap that
    /// latency. `buffered` caps how many run at once (servers throttle) and,
    /// crucially, yields results in target order however the listings finish,
    /// so the engine's ordered ingest is unaffected by completion order.
    /// Swapping in `buffer_unordered` would break the #0072 prune ordering; the
    /// test below pins that property.
    ///
    /// The bodies are not downloaded here (PERSO-300): each listing's session
    /// goes back to the pool with the plan of its body pass, which
    /// [`next_batch`](SyncBackend::next_batch) works through as the engine asks.
    async fn list_targets(
        &mut self,
        targets: &[SyncTarget],
        limit: usize,
        knowns: Vec<KnownUids>,
        // The listings run in parallel, so there is no point between two of
        // them to stop at.
        _stop: Option<&Stop>,
    ) -> Vec<Result<MailboxListing>> {
        use futures::stream::StreamExt;
        self.passes.clear();
        self.body_spent = Duration::ZERO;
        self.chunks_asked = 0;
        let concurrency = self.config.fetch_concurrency.clamp(1, 8);
        let config = self.config;
        // One checkout before the fan-out (PERSO-296), so the mailboxes do not
        // each log in at once with a password the server is about to refuse:
        // the first refusal is on the list before any other mailbox asks. On a
        // live pool it borrows an idle session and hands it straight back for
        // the first mailbox; on a cold one its login is the first mailbox's.
        // A failure here is every mailbox's, since each would have met it.
        if !targets.is_empty() {
            if let Err(e) = pool::checkout(config).await {
                self.passes = targets.iter().map(|_| None).collect();
                return targets.iter().map(|_| Err(copy_of(&e))).collect();
            }
        }
        let listed: Vec<Result<(MailboxListing, BodyPlan)>> = futures::stream::iter(
            targets
                .iter()
                .zip(knowns)
                .map(|(target, known)| async move {
                    // One borrowed session per mailbox, not one shared one: IMAP allows
                    // a single SELECTed mailbox per connection, so N mailboxes need N
                    // connections to overlap their latency. What #0041 changed is that
                    // they are borrowed and returned rather than opened and logged out.
                    let mut pooled = pool::checkout(config).await?;
                    // The capability gate travels with the connection that advertised
                    // it: `caps` is what *this* server said after *this* login, never a
                    // remembered or configured answer (#0041).
                    let caps = pooled.caps();
                    let out = list_mailbox_on_session(
                        pooled.session(),
                        &target.server_name,
                        Some(target.limit(limit)),
                        known,
                        caps,
                    )
                    .await;
                    pooled.check(out)
                }),
        )
        .buffered(concurrency)
        .collect()
        .await;
        listed
            .into_iter()
            .map(|listed| match listed {
                Ok((listing, plan)) => {
                    self.passes.push(Some(BodyPass {
                        plan,
                        session: None,
                    }));
                    Ok(listing)
                }
                Err(e) => {
                    self.passes.push(None);
                    Err(e)
                }
            })
            .collect()
    }

    /// The next chunk of the target's body pass, on a session borrowed at its
    /// first chunk and held until its last (PERSO-300).
    ///
    /// The session `SELECT`s the mailbox again, since whatever the pool hands
    /// out was left on some other one, and a mailbox renumbered since its
    /// listing fails the pass rather than download bodies under UIDs that now
    /// name other messages. The budget is asked before every chunk but the
    /// pass's first ([`stop_before_chunk`]), against the network time the
    /// pass has spent on bodies across its mailboxes, so once it is spent the
    /// mailbox in hand and every one after it end short.
    async fn next_batch(&mut self, index: usize) -> Result<BodyBatch> {
        let config = self.config;
        let budget = self.body_budget;
        let Some(pass) = self.passes.get_mut(index).and_then(Option::as_mut) else {
            return Err(anyhow!("no listing for the mailbox at {index}"));
        };
        if pass.plan.chunks.is_empty() {
            pass.session = None;
            return Ok(BodyBatch::Done { complete: true });
        }
        if stop_before_chunk(self.chunks_asked, budget, self.body_spent) {
            warn!(
                "[sync] '{}': body fetch stopped at the account's {}s budget after {}/{} new \
                 message(s); the rest resume on the next pass",
                pass.plan.mailbox,
                budget.map_or(0, |b| b.as_secs()),
                pass.plan.handed,
                pass.plan.new
            );
            pass.session = None;
            return Ok(BodyBatch::Done { complete: false });
        }
        let started = Instant::now();
        let fetched = fetch_next_chunk(config, pass).await;
        self.body_spent += started.elapsed();
        self.chunks_asked += 1;
        match fetched {
            Ok(bodies) => {
                pass.plan.handed += bodies.len();
                Ok(BodyBatch::Bodies(bodies))
            }
            Err(e) => {
                // `fetch_next_chunk` poisoned a session whose command failed
                // (#0041); the pass is over either way.
                pass.session = None;
                pass.plan.chunks.clear();
                Err(e)
            }
        }
    }
}

/// Borrow and `SELECT` the pass's session if it has none yet, then download
/// its next chunk.
async fn fetch_next_chunk(
    config: &ImapConfig,
    pass: &mut BodyPass,
) -> Result<Vec<crate::sync::FetchedMessage>> {
    if pass.session.is_none() {
        let mut pooled = pool::checkout(config).await?;
        let selected = pooled
            .session()
            .select(&pass.plan.mailbox)
            .await
            .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", pass.plan.mailbox, e));
        let selected = pooled.check(selected)?;
        if selected.uid_validity != pass.plan.uid_validity {
            return Err(anyhow!(
                "'{}' changed its UIDVALIDITY from {:?} to {:?} since it was listed",
                pass.plan.mailbox,
                pass.plan.uid_validity,
                selected.uid_validity
            ));
        }
        pass.session = Some(pooled);
    }
    let chunk = pass
        .plan
        .chunks
        .pop_front()
        .expect("the caller checked a chunk is left");
    let session = pass.session.as_mut().expect("selected above");
    let fetched = fetch_body_chunk_on_session(session.session(), &chunk).await;
    session.check(fetched)
}

/// `error` again, for the next mailbox it fails: a refused login keeps its
/// type, which is what ends the pass, and any other failure its text.
fn copy_of(error: &anyhow::Error) -> anyhow::Error {
    match crate::login_gate::login_rejected(error) {
        Some(rejected) => rejected.clone().into(),
        None => anyhow!("{error:#}"),
    }
}

/// Fetch every target mailbox and ingest what comes back.
///
/// Since #0059 this is the wiring only: it opens the store, builds the IMAP
/// backend and hands both to [`run_sync_guarded`], which owns the
/// orchestration (ingest, arrival marks, flags, cursors, the deferred prune
/// pass) and is tested offline against a fake backend.
///
/// `dry_run` fetches and counts what *would* be ingested without touching the
/// store or the blob directory.
///
/// `body_budget` bounds the network time the pass spends on bodies, summed
/// over its mailboxes ([`ImapBackend::with_body_budget`]). Its one caller,
/// the daemon's guarded `mp sync` pass (`--mailbox`, `--dry-run`, or an
/// account with no runtime), passes `None`; the runtime's tick builds its own
/// backend with the configured budget.
///
/// Since #0122 the pass runs under the per-account engine lock, so `Ok(None)`
/// means another process is this account's engine and this call did nothing:
/// no session, no ingest, no error. Every caller reads that as a success and
/// says it skipped, the same way a refused outbox drain is read (#0116).
///
/// `stop` is asked at every mailbox boundary (PERSO-127) and between two
/// batches of a mailbox's bodies (PERSO-300); a stopped pass returns `Ok(Some(..))` with what it ingested
/// before the stop, and the caller that owns the `Stop` knows it stopped.
///
/// Every row the pass inserts is recorded in `observations` as it is
/// inserted, so the caller indexes its correspondents whatever the pass
/// returns (PERSO-234).
// Every argument is a separate fact of the pass, and the one caller names them
// all; a struct would only be `SyncRun` built a second time.
#[allow(clippy::too_many_arguments)]
pub async fn sync_mailboxes(
    imap_config: &ImapConfig,
    account_name: &str,
    targets: &[SyncTarget],
    limit: usize,
    dry_run: bool,
    body_budget: Option<Duration>,
    stop: Option<Stop>,
    observations: &ObservationSink,
) -> Result<Option<SyncResult>> {
    info!(
        "sync_mailboxes: account={account_name}, {} targets, limit={limit}, dry_run={dry_run}",
        targets.len(),
    );
    let span_label = if limit < usize::MAX {
        "sync_mailboxes:quick"
    } else {
        "sync_mailboxes:full"
    };
    let mut span = TimingSpan::with_context(span_label, format!("{} targets", targets.len()));

    let store = Store::open_for_pass(account_name, dry_run)?;
    let blobs = BlobStore::for_account(account_name);
    let mut backend = ImapBackend::new(imap_config).with_body_budget(body_budget);

    run_sync_guarded(
        &mut backend,
        &SyncRun {
            store: &store,
            blobs: &blobs,
            account: account_name,
            targets,
            limit,
            dry_run,
            stop: stop.as_ref(),
            observations,
        },
        &mut span,
    )
    .await
}

// ---------------------------------------------------------------------------
// list_mailboxes
// ---------------------------------------------------------------------------

/// The names `LIST "" "*"` returns, on a session of the caller's: what
/// `config.probe` lists on the session it dialled past the list of refused
/// logins.
pub(crate) async fn list_mailboxes_on(session: &mut super::ImapSession) -> Result<Vec<String>> {
    Ok(list_detailed_on(session)
        .await?
        .into_iter()
        .map(|mailbox| mailbox.name)
        .collect())
}

/// One mailbox as the server's `LIST` describes it (P4-U10).
///
/// `total` and `unread` are not here: `LIST` reports neither, and a `STATUS`
/// per mailbox would turn a listing into one round trip per row. The daemon's
/// `mailbox.list_server` carries both members as `null` on this transport and
/// fills them from Graph's folder list, which does report them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerMailbox {
    /// The name as the server spells it, which is what `SELECT` takes.
    pub name: String,
    /// The hierarchy separator, `None` for a flat name.
    pub delimiter: Option<String>,
    /// The `LIST` attributes, as the `\Name` tokens they travel as.
    pub attributes: Vec<String>,
}

/// Everything `LIST "" "*"` returns, with the attributes and the delimiter the
/// name-only [`list_mailboxes`] drops.
pub async fn list_mailboxes_detailed(imap_config: &ImapConfig) -> Result<Vec<ServerMailbox>> {
    let mut pooled = pool::checkout(imap_config).await?;
    let listed = list_detailed_on(pooled.session()).await;
    pooled.check(listed)
}

/// [`list_mailboxes_detailed`] on a session already open.
async fn list_detailed_on(session: &mut super::ImapSession) -> Result<Vec<ServerMailbox>> {
    use futures::TryStreamExt;

    Ok(session
        .list(None, Some("*"))
        .await
        .map_err(|e| anyhow!("Failed to list mailboxes: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect mailboxes: {}", e))?
        .iter()
        .map(|m| ServerMailbox {
            name: m.name().to_string(),
            delimiter: m.delimiter().map(str::to_string),
            attributes: m.attributes().iter().map(attribute_token).collect(),
        })
        .collect())
}

/// One `LIST` attribute as the `\Name` token RFC 3501 and RFC 6154 spell it.
///
/// The catch-all is a `Debug` rendering rather than a panic: a server may send
/// an attribute this build of the parser knows and this function does not, and
/// a listing is not the place to refuse over it.
fn attribute_token(attribute: &async_imap::types::NameAttribute<'_>) -> String {
    use async_imap::types::NameAttribute;
    match attribute {
        NameAttribute::NoInferiors => "\\Noinferiors".to_string(),
        NameAttribute::NoSelect => "\\Noselect".to_string(),
        NameAttribute::Marked => "\\Marked".to_string(),
        NameAttribute::Unmarked => "\\Unmarked".to_string(),
        NameAttribute::All => "\\All".to_string(),
        NameAttribute::Archive => "\\Archive".to_string(),
        NameAttribute::Drafts => "\\Drafts".to_string(),
        NameAttribute::Flagged => "\\Flagged".to_string(),
        NameAttribute::Junk => "\\Junk".to_string(),
        NameAttribute::Sent => "\\Sent".to_string(),
        NameAttribute::Trash => "\\Trash".to_string(),
        NameAttribute::Extension(name) => format!("\\{name}"),
        other => format!("{other:?}"),
    }
}

/// The message and unread counts of each of `names`, from one `STATUS
/// (MESSAGES UNSEEN)` per folder on one borrowed session (#0141).
///
/// What a `status-only` folder's sidebar row shows, since its store holds at
/// most a tail of it. A folder the server refuses is left out and logged
/// rather than failing the rest: one renamed folder must not blank every
/// other count.
pub async fn status_counts(
    imap_config: &ImapConfig,
    names: &[String],
) -> Result<Vec<(String, u64, u64)>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut pooled = pool::checkout(imap_config).await?;
    let session = pooled.session();
    let counted = async {
        let mut counts = Vec::with_capacity(names.len());
        for name in names {
            match session.status(name, "(MESSAGES UNSEEN)").await {
                Ok(mailbox) => counts.push((
                    name.clone(),
                    u64::from(mailbox.exists),
                    u64::from(mailbox.unseen.unwrap_or(0)),
                )),
                Err(async_imap::error::Error::No(why)) => {
                    log::warn!("[catalogue] STATUS {name} was refused: {why}");
                }
                Err(e) => return Err(anyhow!("STATUS {name} failed: {e}")),
            }
        }
        Ok(counts)
    }
    .await;
    pooled.check(counted)
}

/// What the server says of one folder a listing gained, for telling a folder
/// it renamed from a new one (PERSO-185).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderIdentity {
    /// The name as the server lists it.
    pub name: String,
    pub uidvalidity: Option<u32>,
    pub uidnext: Option<u32>,
    /// The bare Message-ID of each sampled UID the folder holds and whose
    /// header carries one, by ascending UID.
    pub message_ids: Vec<(u32, String)>,
}

/// The `UID FETCH` items of a rename probe: the Message-ID header alone.
const MESSAGE_ID_QUERY: &str = "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])";

/// One `STATUS (UIDVALIDITY UIDNEXT)` per folder of `names`, on one borrowed
/// session, and for a folder whose UIDVALIDITY is one of `samples`', an
/// `EXAMINE` and a `UID FETCH` of that sample's UIDs for their Message-IDs
/// (PERSO-185).
///
/// A folder the server refuses a `STATUS` or an `EXAMINE` of is left out
/// and logged: no answer is no evidence of a rename, never an error that
/// fails the listing.
pub async fn folder_identities(
    imap_config: &ImapConfig,
    names: &[String],
    samples: &[(u32, Vec<u32>)],
) -> Result<Vec<FolderIdentity>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut pooled = pool::checkout(imap_config).await?;
    let probed = identities_on(pooled.session(), names, samples).await;
    pooled.check(probed)
}

/// [`folder_identities`] on a session already open.
async fn identities_on(
    session: &mut super::ImapSession,
    names: &[String],
    samples: &[(u32, Vec<u32>)],
) -> Result<Vec<FolderIdentity>> {
    use futures::TryStreamExt;

    let mut seen = Vec::with_capacity(names.len());
    for name in names {
        let status = match session.status(name, "(UIDVALIDITY UIDNEXT)").await {
            Ok(status) => status,
            Err(async_imap::error::Error::No(why)) => {
                log::warn!("[catalogue] STATUS {name} was refused: {why}");
                continue;
            }
            Err(e) => return Err(anyhow!("STATUS {name} failed: {e}")),
        };
        let mut identity = FolderIdentity {
            name: name.clone(),
            uidvalidity: status.uid_validity,
            uidnext: status.uid_next,
            message_ids: Vec::new(),
        };
        let sample = samples
            .iter()
            .find(|(uidvalidity, _)| Some(*uidvalidity) == status.uid_validity)
            .map(|(_, uids)| uids)
            .filter(|uids| !uids.is_empty());
        if let Some(uids) = sample {
            match session.examine(name).await {
                Ok(_) => {}
                Err(async_imap::error::Error::No(why)) => {
                    log::warn!("[catalogue] EXAMINE {name} was refused: {why}");
                    seen.push(identity);
                    continue;
                }
                Err(e) => return Err(anyhow!("EXAMINE {name} failed: {e}")),
            }
            let set = uids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetched: Vec<_> = session
                .uid_fetch(&set, MESSAGE_ID_QUERY)
                .await
                .map_err(|e| anyhow!("Failed to fetch the Message-IDs of {name}: {e}"))?
                .try_collect()
                .await
                .map_err(|e| anyhow!("Failed to collect the Message-IDs of {name}: {e}"))?;
            identity.message_ids = fetched
                .iter()
                .filter_map(|msg| {
                    let id =
                        crate::draft_mirror::folder::header_value(msg.header()?, "Message-ID")?;
                    let bare = crate::draft_mirror::folder::bare_message_id(&id).to_string();
                    Some((msg.uid?, bare)).filter(|(_, bare)| !bare.is_empty())
                })
                .collect();
            identity.message_ids.sort();
        }
        seen.push(identity);
    }
    Ok(seen)
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    // The orchestration tests this module used to hold moved to
    // `crate::sync::engine` with the loop itself (#0059), where they now run
    // through the real code path against a fake backend instead of re-walking
    // its calls by hand. What stays here is the one property that belongs to
    // *this* transport: the fetch's completion order.

    /// A future that returns `Poll::Pending` `n` times before resolving to its
    /// value, rescheduling itself each time. Used to make the parallel fetches
    /// finish in a deliberately scrambled order.
    struct ReadyAfter {
        remaining: usize,
        value: usize,
    }

    impl Future for ReadyAfter {
        type Output = usize;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<usize> {
            if self.remaining == 0 {
                Poll::Ready(self.value)
            } else {
                self.remaining -= 1;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    /// The load-bearing property of the #0005 parallel fetch, and the contract
    /// [`super::ImapBackend`] owes [`crate::sync::SyncBackend`]: `buffered`
    /// yields results in *input* (target) order regardless of which fetch
    /// finishes first, so the engine's serial ingest still processes inbox
    /// before archive before sent, and the #0072 prune ordering is untouched.
    /// Swapping in `buffer_unordered` would return `4,3,2,1,0` here and break
    /// that.
    #[test]
    fn buffered_fetch_yields_in_target_order_however_the_fetches_finish() {
        use futures::stream::StreamExt;
        let out: Vec<usize> = futures::executor::block_on(async {
            futures::stream::iter((0..5usize).map(|i| ReadyAfter {
                // input 0 stalls longest, input 4 finishes first
                remaining: (5 - i) * 2,
                value: i,
            }))
            .buffered(5)
            .collect()
            .await
        });
        assert_eq!(out, vec![0, 1, 2, 3, 4]);
    }

    /// PERSO-300: a full sync holds one chunk of bodies at a time, from the
    /// download through the ingest, whatever the size of the mailboxes: each
    /// chunk is ingested before the next `UID FETCH` goes out, and nothing
    /// keeps a body once it is written.
    ///
    /// Until PERSO-300 every mailbox was downloaded whole before the first was
    /// ingested, and a full sync of a 6,360-message folder peaked at about
    /// 8 GB. The count is taken through `FetchedMessage` itself
    /// ([`crate::sync::held_bodies`]), not from the process's memory.
    #[test]
    fn a_full_sync_holds_one_chunk_of_bodies_at_a_time() {
        use crate::config::ImapConfig;
        use crate::imap_client::fake_imap::{LoginPolicy, RejectingImapServer};
        use crate::imap_client::fetch::BODY_CHUNK_SIZE;
        use crate::store::{BlobStore, Store};
        use crate::sync::engine::{run_sync, SyncRun};
        use crate::sync::{held_bodies, ObservationSink, SyncTarget};
        use crate::types::MailboxRole;

        let message = |folder: &str, uid: u32| {
            format!(
                "From: a@example.com\r\nTo: b@example.com\r\nSubject: {folder} {uid}\r\n\
                 Message-ID: <{uid}@{folder}.example.com>\r\n\
                 Date: Thu, 7 Aug 2025 10:00:00 +0000\r\n\r\n{folder} body {uid}\r\n"
            )
            .into_bytes()
        };
        let server = RejectingImapServer::start_with(LoginPolicy::Accept);
        let inbox = 3 * BODY_CHUNK_SIZE as u32 + 5;
        let archive = 2 * BODY_CHUNK_SIZE as u32 + 3;
        server.set_mail(
            "INBOX",
            3,
            (1..=inbox)
                .map(|uid| (uid, message("inbox", uid)))
                .collect(),
        );
        server.set_mail(
            "Archive",
            4,
            (1..=archive)
                .map(|uid| (uid, message("archive", uid)))
                .collect(),
        );
        let config = ImapConfig {
            account: String::new(),
            host: "127.0.0.1".into(),
            port: server.port(),
            username: "bounded@example.com".into(),
            password: "right".into(),
            accept_invalid_certs: true,
            fetch_concurrency: 4,
            body_fetch_deadline_secs: 0,
        };
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(tmp.path().join("blobs"));
        let targets = vec![
            SyncTarget::new(MailboxRole::Inbox, "INBOX"),
            SyncTarget::new(MailboxRole::Archive, "Archive"),
        ];

        held_bodies::reset_peak();
        let result = futures::executor::block_on(run_sync(
            &mut super::ImapBackend::new(&config),
            &SyncRun {
                store: &store,
                blobs: &blobs,
                account: "acct",
                targets: &targets,
                limit: usize::MAX,
                dry_run: false,
                stop: None,
                observations: &ObservationSink::default(),
            },
            &mut crate::timing::TimingSpan::new("test"),
        ))
        .unwrap();

        assert_eq!(
            result.saved,
            (inbox + archive) as usize,
            "every message is ingested"
        );
        assert_eq!(result.bodies_truncated, 0);
        assert!(
            held_bodies::peak() <= BODY_CHUNK_SIZE,
            "the pass held {} bodies at once, more than one chunk of {BODY_CHUNK_SIZE}",
            held_bodies::peak()
        );
        assert_eq!(held_bodies::live(), 0, "no body outlives the pass");
    }

    /// PERSO-300: the body budget is the pass's, every mailbox counted. A
    /// pass whose mailboxes each owe a chunk of slow bodies spends one budget
    /// on them at most, plus the chunk that crossed it; the mailboxes
    /// it did not reach close short with their arrival mark held, the prune
    /// waits, and the next pass brings the rest in.
    ///
    /// With a budget per mailbox, each of the three would have downloaded its
    /// chunk, three budgets' worth of time for a tick every other account
    /// waits behind.
    #[test]
    fn a_pass_spends_one_body_budget_across_its_mailboxes() {
        use std::time::Duration;

        use crate::config::ImapConfig;
        use crate::imap_client::fake_imap::{LoginPolicy, RejectingImapServer};
        use crate::imap_client::fetch::BODY_CHUNK_SIZE;
        use crate::ingest;
        use crate::store::{BlobStore, Store};
        use crate::sync::engine::{run_sync, SyncRun};
        use crate::sync::{ObservationSink, SyncResult, SyncTarget};
        use crate::types::MailboxRole;

        let message = |folder: &str, uid: u32| {
            let raw = format!(
                "From: a@example.com\r\nTo: b@example.com\r\nSubject: {folder} {uid}\r\n\
                 Message-ID: <{uid}@{folder}.example.com>\r\n\
                 Date: Thu, 7 Aug 2025 10:00:00 +0000\r\n\r\n{folder} body {uid}\r\n"
            );
            (uid, raw.into_bytes())
        };
        let folders = ["INBOX", "Archive", "Sent"];
        let server = RejectingImapServer::start_with(LoginPolicy::Accept);
        let fill = |top: u32| {
            for folder in folders {
                server.set_mail(
                    folder,
                    3,
                    (1..=top).map(|uid| message(folder, uid)).collect(),
                );
            }
        };
        let config = ImapConfig {
            account: String::new(),
            host: "127.0.0.1".into(),
            port: server.port(),
            username: "budget@example.com".into(),
            password: "right".into(),
            accept_invalid_certs: true,
            fetch_concurrency: 4,
            body_fetch_deadline_secs: 0,
        };
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(tmp.path().join("blobs"));
        let targets = vec![
            SyncTarget::new(MailboxRole::Inbox, "INBOX"),
            SyncTarget::new(MailboxRole::Archive, "Archive"),
            SyncTarget::new(MailboxRole::Sent, "Sent"),
        ];
        let pass = |budget: Option<Duration>| -> SyncResult {
            futures::executor::block_on(run_sync(
                &mut super::ImapBackend::new(&config).with_body_budget(budget),
                &SyncRun {
                    store: &store,
                    blobs: &blobs,
                    account: "acct",
                    targets: &targets,
                    limit: usize::MAX,
                    dry_run: false,
                    stop: None,
                    observations: &ObservationSink::default(),
                },
                &mut crate::timing::TimingSpan::new("test"),
            ))
            .unwrap()
        };
        let held = |mailbox: &str| {
            let mut uids: Vec<i64> = ingest::known_uids(&store, "acct", mailbox)
                .unwrap()
                .into_iter()
                .collect();
            uids.sort_unstable();
            uids
        };
        let mark = |mailbox: &str| {
            ingest::known_uids_with_cursor(&store, "acct", mailbox)
                .unwrap()
                .arrival_mark
        };

        // History: five messages in each mailbox, so what follows arrives
        // above a recorded top.
        fill(5);
        assert_eq!(pass(None).saved, 15);

        // A chunk of new mail in each mailbox, every chunk slower than half
        // the budget, and a message gone from the inbox for the prune to want.
        let top = 5 + BODY_CHUNK_SIZE as u32;
        fill(top);
        server.set_mail(
            "INBOX",
            3,
            (2..=top).map(|uid| message("INBOX", uid)).collect(),
        );
        let delay = Duration::from_millis(100);
        let budget = Duration::from_millis(150);
        server.set_body_delay(delay);
        let before = server.body_fetches();
        let cut = pass(Some(budget));
        let fetched = server.body_fetches() - before;

        // Every fetch costs at least `delay`, and none is asked once `budget`
        // is spent: two at most, the one that crossed it included.
        let most = (budget.as_millis() / delay.as_millis()) as usize + 1;
        assert!(
            (1..=most).contains(&fetched),
            "the pass asked for {fetched} chunk(s) of bodies, where one budget buys {most} at most"
        );
        assert_eq!(
            held("sent"),
            (1..=5).collect::<Vec<_>>(),
            "the mailbox after the budget ran out downloads nothing"
        );
        assert_eq!(
            mark("sent"),
            Some(5),
            "its mark holds below the arrivals it left on the server"
        );
        assert!(cut.bodies_truncated >= 1);
        assert_eq!(cut.pruned, 0, "a pass the budget cut prunes nothing");
        assert_eq!(cut.prunes_deferred, 1);
        assert!(held("inbox").contains(&1));

        // The next pass takes what the budget left, and the prune runs.
        server.set_body_delay(Duration::ZERO);
        let caught_up = pass(None);
        assert_eq!(caught_up.pruned, 1);
        assert_eq!(held("sent"), (1..=i64::from(top)).collect::<Vec<_>>());
        assert_eq!(held("inbox"), (2..=i64::from(top)).collect::<Vec<_>>());
        for mailbox in ["inbox", "archive", "sent"] {
            assert_eq!(mark(mailbox), None, "{mailbox} caught up");
        }
    }

    /// PERSO-185: the rename probe reads each gained folder's UIDVALIDITY and
    /// UIDNEXT with a `STATUS`, and the Message-IDs of the sampled UIDs only
    /// from a folder carrying the UIDVALIDITY a lost folder had, bare, and
    /// only for the UIDs the folder still holds.
    #[test]
    fn the_rename_probe_reads_uidvalidity_uidnext_and_message_ids() {
        use crate::config::ImapConfig;
        use crate::imap_client::fake_imap::{FakeFolder, LoginPolicy, RejectingImapServer};

        let server = RejectingImapServer::start_with(LoginPolicy::Accept);
        server.set_folders(vec![
            FakeFolder::empty("INBOX"),
            FakeFolder {
                name: "Projekte".to_string(),
                uidvalidity: 7,
                uidnext: 4,
                messages: vec![
                    (1, "<one@example.com>".to_string()),
                    (3, "<three@example.com>".to_string()),
                ],
            },
            FakeFolder {
                name: "Neu".to_string(),
                uidvalidity: 9,
                uidnext: 2,
                messages: vec![(1, "<other@example.com>".to_string())],
            },
        ]);
        let config = ImapConfig {
            account: String::new(),
            host: "127.0.0.1".into(),
            port: server.port(),
            username: "probe@example.com".into(),
            password: "right".into(),
            accept_invalid_certs: true,
            fetch_concurrency: 4,
            body_fetch_deadline_secs: 30,
        };

        let seen = futures::executor::block_on(super::folder_identities(
            &config,
            &["Projekte".to_string(), "Neu".to_string()],
            &[(7, vec![1, 2, 3])],
        ))
        .unwrap();
        assert_eq!(
            seen,
            vec![
                super::FolderIdentity {
                    name: "Projekte".to_string(),
                    uidvalidity: Some(7),
                    uidnext: Some(4),
                    message_ids: vec![
                        (1, "one@example.com".to_string()),
                        (3, "three@example.com".to_string()),
                    ],
                },
                super::FolderIdentity {
                    name: "Neu".to_string(),
                    uidvalidity: Some(9),
                    uidnext: Some(2),
                    message_ids: Vec::new(),
                },
            ]
        );
    }
}
