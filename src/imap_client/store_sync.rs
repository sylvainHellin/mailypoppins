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

use std::time::Duration;

use anyhow::{anyhow, Result};
use log::info;

use super::fetch::fetch_new_raw_on_session;
use super::pool;
use crate::config::ImapConfig;
use crate::ingest::KnownUids;
use crate::store::{BlobStore, Store};
use crate::sync::engine::{run_sync_guarded, SyncRun};
use crate::sync::{MailboxFetch, ObservationSink, Stop, SyncBackend, SyncResult, SyncTarget};
use crate::timing::TimingSpan;

/// The IMAP transport behind [`crate::sync::SyncBackend`] (#0059).
///
/// It owns what a fetch needs and nothing the engine owns: no store handle, no
/// cursors, no ingest bookkeeping. That is still only the config, because
/// #0041 put the sessions themselves in [`super::pool`] rather than in this
/// struct: they have to outlive the backend (a `mp sync` builds one per call,
/// and the queued-op drain has no backend at all), and the pool is what every
/// IMAP path in the process borrows from.
pub struct ImapBackend<'a> {
    config: &'a ImapConfig,
    /// How long one mailbox's fetch may spend on bodies before it stops and
    /// leaves the rest for the next pass (#0113). `None` is unbounded.
    body_budget: Option<Duration>,
    /// Asked before every body chunk of every mailbox (PERSO-127). `None`
    /// fetches to the end.
    stop: Option<Stop>,
}

impl<'a> ImapBackend<'a> {
    pub fn new(config: &'a ImapConfig) -> Self {
        Self {
            config,
            body_budget: None,
            stop: None,
        }
    }

    /// Stop each mailbox's body pass at its next chunk once `stop` says so
    /// (PERSO-127). The same `Stop` goes into [`SyncRun::stop`], which is
    /// where the engine drops what a stopped fetch brought back.
    pub fn with_stop(mut self, stop: Option<Stop>) -> Self {
        self.stop = stop;
        self
    }

    /// Bound each mailbox's body download (#0113).
    ///
    /// The budget is per target rather than per sync: the targets run
    /// concurrently, so a shared clock would spend one mailbox's budget on
    /// another's download and truncate whichever finished last for no reason of
    /// its own.
    pub fn with_body_budget(mut self, budget: Option<Duration>) -> Self {
        self.body_budget = budget;
        self
    }
}

impl SyncBackend for ImapBackend<'_> {
    /// Fetch every mailbox in parallel, one IMAP session each (#0005).
    ///
    /// IMAP allows one SELECTed mailbox per connection, so the old single
    /// session paid `N * latency` for N mailboxes; N connections overlap that
    /// latency. `buffered` caps how many run at once (servers throttle) and,
    /// crucially, yields results in target order however the fetches finish, so
    /// the engine's ordered ingest is unaffected by completion order. Swapping
    /// in `buffer_unordered` would break the #0072 prune ordering; the test
    /// below pins that property.
    async fn fetch_targets(
        &mut self,
        targets: &[SyncTarget],
        limit: usize,
        knowns: Vec<KnownUids>,
    ) -> Vec<Result<MailboxFetch>> {
        use futures::stream::StreamExt;
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
                return targets.iter().map(|_| Err(copy_of(&e))).collect();
            }
        }
        let body_budget = self.body_budget;
        let stop = self.stop.as_ref();
        futures::stream::iter(
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
                    let out = fetch_new_raw_on_session(
                        pooled.session(),
                        &target.server_name,
                        Some(target.limit(limit)),
                        known,
                        caps,
                        body_budget,
                        stop,
                    )
                    .await;
                    pooled.check(out)
                }),
        )
        .buffered(concurrency)
        .collect()
        .await
    }
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
/// `body_budget` bounds each mailbox's body download (#0113): a TUI tick passes
/// the account's configured deadline so one slow mailbox cannot hold the tick,
/// and `mp sync` passes `None`, because it is the explicit recovery path and a
/// full sync must not be cut short.
///
/// Since #0122 the pass runs under the per-account engine lock, so `Ok(None)`
/// means another process is this account's engine and this call did nothing:
/// no session, no ingest, no error. Every caller reads that as a success and
/// says it skipped, the same way a refused outbox drain is read (#0116).
///
/// `stop` is asked at every body chunk and every mailbox boundary
/// (PERSO-127); a stopped pass returns `Ok(Some(..))` with what it ingested
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
    let mut backend = ImapBackend::new(imap_config)
        .with_body_budget(body_budget)
        .with_stop(stop.clone());

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
