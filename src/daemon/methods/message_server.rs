//! The two `message.*` operations that open a session on the account's mail
//! server: `message.search_server` and `message.fetch` (P5-U10c, #0126,
//! `LST-08` and `LST-09`).
//!
//! They are the TUI search overlay's server leg, moved off the client's own
//! background thread. `src/tui/helpers.rs`'s `lib_do_multi_search` opened one
//! IMAP session, searched each target mailbox with a slice of a total budget
//! and handed the whole batch back at the end; `src/tui/actions.rs`'s `f` key
//! asked the server for the uid of one Message-ID and ingested the bytes it
//! got back. Both are here now, unchanged in what they ask the server for.
//!
//! `message.list_server` stays in [`super::message`] and is a different
//! method: P4-U10 registered that name for `mp fetch`'s one-mailbox query over
//! `criteria`, which writes nothing and answers a listing. Redefining it would
//! break a command that has nothing to do with this one, which is why the
//! search leg is `message.search_server`.
//!
//! # Why operations rather than queries
//!
//! A server search is seconds of network per mailbox and a fetch is a round
//! trip plus an ingest, so both answer at once with `{operation_id}` and do
//! the work in the background. Both are `Durable`: a search a user started
//! must not be torn down because the window that started it went away, and a
//! fetch that has written a row has nothing to be cancelled back to.
//!
//! # What streams
//!
//! One `state.event` of kind `message.server_hit` per hit, payload
//! `{operation_id, hit}`, so the overlay fills in as the mailboxes answer
//! rather than painting one batch at the end. The operation id is on the
//! payload because a fast retype leaves two searches in flight, which is the
//! problem the overlay's own generation counter solves client-side.
//!
//! # What fails, and what does not
//!
//! A mailbox the server refuses does not fail the search: the in-process leg
//! logged a warning per mailbox and kept going, because four folders of
//! answers are worth having when the fifth is refused. `unreachable` is that
//! list and the operation still succeeds. What does fail it is a credential
//! that cannot be resolved, which happens once, before any mailbox is
//! selected.

use std::collections::HashSet;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::listing::{MessageFlags, ServerSearchHit};
use mp_protocol::{ErrorCode, RpcError};

use crate::config::{AccountConfig, AuthMethod};
use crate::daemon::state::events::Event;
use crate::daemon::state::{CanonicalState, Change};
use crate::parse::FetchedEmail;
use crate::selector::Selector;
use crate::store::{read, BlobStore, Store};
use crate::tui::app::{build_mailboxes, resolve_date};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::super::operations::{OperationHandle, OperationRegistry};
use super::super::state::ConnectionId;
use super::{internal, invalid_params, only_params, string_param};

/// The event kind one streamed hit travels as.
///
/// A lifecycle event: a hit is a fact about a moment in one search, two hits
/// never merge, and no snapshot brings one back, exactly as an
/// `operation.progress` report does not.
const KIND_SERVER_SEARCH_HIT: &str = "message.server_hit";

/// The total hit budget a search spends across its mailboxes when the caller
/// names none, which is what the in-process leg spent.
const DEFAULT_SEARCH_LIMIT: usize = 50;

/// The floor under a per-mailbox slice of that budget: five mailboxes sharing
/// fifty hits is ten each, and a client that asked for twelve across five
/// would otherwise get two apiece and see nothing.
const MIN_PER_MAILBOX: usize = 5;

/// The two methods, in method-name order.
pub const MESSAGE_SERVER_LEG_METHOD_SPECS: [MethodSpec; 2] = [
    MethodSpec::new("message.fetch", MethodKind::Operation, 1),
    MethodSpec::new("message.search_server", MethodKind::Operation, 1),
];

/// One of the two, selected by its own [`MethodSpec`].
pub struct MessageServerLeg {
    /// Which of [`MESSAGE_SERVER_LEG_METHOD_SPECS`] this instance serves.
    pub spec: MethodSpec,
    /// The live configuration, so a reload is visible to the next call.
    pub config: Arc<ConfigStore>,
    /// Where a fetch that wrote a row publishes the counts change it owes.
    pub canonical: Arc<CanonicalState>,
    /// The registry the operation is started in.
    pub operations: Arc<OperationRegistry>,
}

impl Method for MessageServerLeg {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let snapshot = self.config.snapshot();
            let fetch = self.spec.name == "message.fetch";
            // Everything a caller can get wrong is answered here, before an id
            // is issued: an operation whose parameters are wrong is a refusal
            // a client has to unpick, not work that started.
            let request = if fetch {
                Request::Fetch(fetch_request(&params, &snapshot)?)
            } else {
                Request::Search(search_request(&params, &snapshot)?)
            };
            let (id, handle) = self.operations.start(
                ConnectionId(ctx.connection_id),
                self.spec.cancel_scope,
                self.spec.name,
            );
            let canonical = Arc::clone(&self.canonical);
            match request {
                Request::Fetch(request) => {
                    tokio::spawn(run_fetch(request, handle, canonical));
                }
                Request::Search(request) => {
                    tokio::spawn(run_search(request, handle, canonical));
                }
            }
            Ok(Outcome::query(json!({"operation_id": id.as_str()})))
        })
    }
}

/// Register the two operations on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    canonical: Arc<CanonicalState>,
    operations: Arc<OperationRegistry>,
) {
    for spec in MESSAGE_SERVER_LEG_METHOD_SPECS {
        dispatcher.register(Arc::new(MessageServerLeg {
            spec,
            config: Arc::clone(&config),
            canonical: Arc::clone(&canonical),
            operations: Arc::clone(&operations),
        }));
    }
}

/// One validated request, whichever method took it.
enum Request {
    Fetch(FetchRequest),
    Search(SearchRequest),
}

// ---------------------------------------------------------------------------
// message.fetch
// ---------------------------------------------------------------------------

/// One validated `message.fetch`.
struct FetchRequest {
    account: AccountConfig,
    /// The `messages.mailbox` key the row is recorded under.
    mailbox: String,
    /// The server mailbox the round trip selects, which falls back to the
    /// mailbox key for an account that maps it to no other name.
    server_name: String,
    message_id: String,
    secrets: crate::secrets::SecretsBackendKind,
}

/// Validate one fetch.
///
/// The address is the one a server-only hit has in hand: the account, the
/// mailbox the hit came from, and the `Message-ID` the server reported. Not a
/// uid, because the store has none for it; not a selector, because a selector
/// names a row and the whole point is that there is not one yet.
fn fetch_request(
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<FetchRequest, RpcError> {
    only_params(
        "message.fetch",
        params,
        &["account", "mailbox", "message_id"],
    )?;
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(&snapshot.accounts, &name)?;
    let wanted = string_param(params, "mailbox")?;
    let mailbox = super::message::resolve_mailbox(account, &wanted)?;
    let message_id = string_param(params, "message_id")?;
    if message_id.trim().is_empty() {
        return Err(invalid_params(
            "message_id is the Message-ID header of the message to fetch",
        ));
    }
    let server_name = crate::config::find_server_name_for_role(account, &mailbox);
    Ok(FetchRequest {
        account: account.clone(),
        mailbox,
        server_name,
        message_id,
        secrets: snapshot.config.secrets_backend,
    })
}

/// One fetch, from the idempotence check to the ingested row.
///
/// The short-circuit comes first and before the backend is resolved, which is
/// both a contract and the only reason any success path of this method is
/// reachable in a test with no server: a message the account's store already
/// holds is answered from that store with `already_present: true` and opens no
/// session at all. A client that raced a sync would otherwise be handed an
/// error for the state it wanted.
async fn run_fetch(request: FetchRequest, handle: OperationHandle, canonical: Arc<CanonicalState>) {
    handle.set_running();
    let name = request.account.name.clone();

    match held_row(&name, &request.message_id) {
        Err(e) => return handle.fail(DomainError::from(e)),
        Ok(Some(settled)) => return handle.succeed(settled),
        Ok(None) => {}
    }

    if let Err(e) = super::open_secrets(&name, request.secrets) {
        return handle.fail(DomainError::from(e));
    }

    let fetched = if request.account.auth_method == AuthMethod::Graph {
        graph_fetch(&request).await
    } else {
        imap_fetch(&request).await
    };
    let (uid, raw, email) = match fetched {
        Ok(fetched) => fetched,
        Err(e) => return handle.fail(DomainError::from(super::server_error(&name, &e))),
    };

    // Graph already carries the whole payload and ingests under the synthetic
    // uid derived from the Message-ID; plain IMAP needs the uid the mailbox
    // holds the message under, since a made-up one would be pruned or
    // duplicated by the next sync.
    let ingested = ingest(&name, &request.mailbox, uid, &email, raw.as_deref());
    let (row_id, selector, published) = match ingested {
        Ok(landed) => landed,
        Err(e) => return handle.fail(DomainError::from(internal(format!("{e:#}")))),
    };
    if let Some(change) = published {
        canonical.apply(change);
    }
    handle.succeed(json!({
        "account": name,
        "mailbox": request.mailbox,
        "uid": uid,
        "row_id": row_id,
        "selector": selector,
        "already_present": false,
    }));
}

/// The settled answer for a message the store already holds, or `None`.
///
/// The row is looked up by `Message-ID` across the account rather than within
/// the named mailbox: the question the overlay's guard asks is "can I already
/// see this message", and a copy filed in Archive answers it as well as one in
/// the Inbox. The answer names the mailbox the row is actually in, because
/// that is the row the client is being handed.
fn held_row(account: &str, message_id: &str) -> Result<Option<Value>, RpcError> {
    let store = Store::open(crate::config::store_path(account))
        .map_err(|e| internal(format!("opening the store of {account}: {e:#}")))?;
    let rows = read::find_by_message_id(&store, account, message_id)
        .map_err(|e| internal(format!("looking up {message_id} in {account}: {e:#}")))?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(json!({
        "account": account,
        "mailbox": row.mailbox,
        "uid": row.uid,
        "row_id": row.id,
        "selector": Selector::for_message(account, row).to_string(),
        "already_present": true,
    })))
}

/// The uid, the raw bytes and the parsed message one IMAP server holds under
/// `message_id`.
async fn imap_fetch(
    request: &FetchRequest,
) -> anyhow::Result<(i64, Option<Vec<u8>>, FetchedEmail)> {
    let server_name = request.server_name.clone();
    let imap = crate::config::ImapConfig::load(&request.account)?;
    let mut session = crate::imap_client::open_imap_session(&imap).await?;
    let found = crate::imap_client::fetch_raw_by_message_id(
        &mut session,
        &server_name,
        &request.message_id,
    )
    .await;
    session.logout().await.ok();
    let (uid, raw, email) =
        found?.ok_or_else(|| anyhow::anyhow!("the server no longer has that message"))?;
    Ok((uid as i64, Some(raw), email))
}

/// The same, over Graph, which carries the whole payload already and keys the
/// row by the synthetic uid ingest derives from the Message-ID.
async fn graph_fetch(
    request: &FetchRequest,
) -> anyhow::Result<(i64, Option<Vec<u8>>, FetchedEmail)> {
    let server_name = request.server_name.clone();
    let config = crate::config::GraphConfig::load(&request.account)?;
    let client = crate::graph::GraphClient::new_async(&config).await?;
    let query = crate::search::Query {
        message_id: Some(request.message_id.clone()),
        ..Default::default()
    };
    let found = client
        .search_messages(&query, Some(&server_name), 1)
        .await?;
    let email = found
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("the server no longer has that message"))?;
    Ok((crate::ingest::graph_uid(&request.message_id), None, email))
}

/// Ingest one fetched message, answering the row it landed as, its selector,
/// and the counts change the mailbox owes.
///
/// A fetch that wrote a row publishes the mailbox's counts change, so a
/// listing update rides on the `state.invalidate` every other ingest travels
/// as; a fetch that found the message already there never reaches here and
/// publishes nothing, having changed nothing.
fn ingest(
    account: &str,
    mailbox: &str,
    uid: i64,
    email: &FetchedEmail,
    raw: Option<&[u8]>,
) -> anyhow::Result<(i64, String, Option<Change>)> {
    let store = Store::open(crate::config::store_path(account))?;
    let blobs = BlobStore::for_account(account);
    let outcome = crate::ingest::ingest_message(
        &store,
        &blobs,
        &crate::ingest::IngestInput {
            account,
            mailbox,
            uid,
            email,
            raw,
        },
    )?;
    let row = read::find_by_id(&store, outcome.row_id)?;
    let selector = row
        .as_ref()
        .map(|row| Selector::for_message(account, row).to_string())
        .unwrap_or_default();
    let change = read::mailbox_read_counts(&store, account)
        .ok()
        .and_then(|counts| {
            counts
                .into_iter()
                .find(|(name, _)| name == mailbox)
                .map(|(name, counts)| Change::MailboxCounts {
                    account: account.to_string(),
                    mailbox: name,
                    total: counts.total as u64,
                    unread: counts.unread as u64,
                    // What the sidebar prints beside the label, which is the
                    // total (`src/daemon/methods/mailbox.rs`).
                    badge: counts.total as u64,
                })
        });
    Ok((outcome.row_id, selector, change))
}

// ---------------------------------------------------------------------------
// message.search_server
// ---------------------------------------------------------------------------

/// One mailbox a search runs against: the sidebar label the hits are reported
/// under, and the server name the session selects.
#[derive(Clone, Debug)]
struct SearchTarget {
    label: String,
    server_name: String,
}

/// One validated `message.search_server`.
struct SearchRequest {
    account: AccountConfig,
    /// What the user typed, kept verbatim for the settle.
    query: String,
    parsed: crate::search::Query,
    targets: Vec<SearchTarget>,
    /// The total hit budget across every target.
    limit: usize,
    /// The normalised Message-IDs the local pass is already showing.
    excluded: HashSet<String>,
    secrets: crate::secrets::SecretsBackendKind,
}

/// Validate one server search.
///
/// `query` is the grammar a user types rather than an engine enum: the overlay
/// holds a parsed `search::Query` and renders it back with
/// `search::to_query_string`, so one parser serves every backend, which is
/// `LST-06`'s whole point.
fn search_request(
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<SearchRequest, RpcError> {
    only_params(
        "message.search_server",
        params,
        &[
            "account",
            "query",
            "mailboxes",
            "limit",
            "exclude_message_ids",
        ],
    )?;
    let name = string_param(params, "account")?;
    let account = super::sync::syncable_account(&snapshot.accounts, &name)?;
    let query = string_param(params, "query")?;
    let parsed = crate::search::from_cli(&query, &crate::search::Flags::default())
        .map_err(invalid_params)?;

    let configured: Vec<SearchTarget> = build_mailboxes(account)
        .into_iter()
        .filter_map(|mailbox| {
            mailbox.server_name.map(|server_name| SearchTarget {
                label: mailbox.label,
                server_name,
            })
        })
        .collect();
    let targets = match params.get("mailboxes") {
        None | Some(Value::Null) => configured,
        Some(Value::Array(names)) => names
            .iter()
            .map(|name| {
                let wanted = name
                    .as_str()
                    .ok_or_else(|| invalid_params("mailboxes is an array of mailbox names"))?;
                pick_target(&configured, wanted, &name_of(account))
            })
            .collect::<Result<Vec<SearchTarget>, RpcError>>()?,
        Some(_) => {
            return Err(invalid_params(
                "mailboxes is an array of mailbox names, or null for every searchable one",
            ))
        }
    };

    let limit = match params.get("limit") {
        None | Some(Value::Null) => DEFAULT_SEARCH_LIMIT,
        Some(value) => value
            .as_u64()
            .and_then(|limit| usize::try_from(limit).ok())
            .ok_or_else(|| invalid_params("limit is a non-negative integer"))?,
    };
    let excluded = match params.get("exclude_message_ids") {
        None | Some(Value::Null) => HashSet::new(),
        Some(Value::Array(ids)) => ids
            .iter()
            .map(|id| {
                id.as_str()
                    .map(read::normalize_message_id_key)
                    .ok_or_else(|| {
                        invalid_params("exclude_message_ids is an array of Message-ID strings")
                    })
            })
            .collect::<Result<HashSet<String>, RpcError>>()?,
        Some(_) => {
            return Err(invalid_params(
                "exclude_message_ids is an array of Message-ID strings",
            ))
        }
    };

    Ok(SearchRequest {
        account: account.clone(),
        query,
        parsed,
        targets,
        limit,
        excluded,
        secrets: snapshot.config.secrets_backend,
    })
}

/// The account's name, for the refusal sentence below.
fn name_of(account: &AccountConfig) -> String {
    account.name.clone()
}

/// The configured target `wanted` names.
///
/// Role, slug, sidebar label **or server name**, which is one spelling more
/// than [`super::message::resolve_mailbox`] accepts: the overlay holds a
/// target of `(label, server_name)` and the CLI's `in:` directive is matched
/// against both (`App::search_target_by_name`), so the wire takes whichever
/// the caller has in hand rather than making it re-derive the other.
fn pick_target(
    configured: &[SearchTarget],
    wanted: &str,
    account: &str,
) -> Result<SearchTarget, RpcError> {
    configured
        .iter()
        .find(|target| {
            wanted.eq_ignore_ascii_case(&target.label)
                || wanted.eq_ignore_ascii_case(&target.server_name)
        })
        .map(|target| SearchTarget {
            label: target.label.clone(),
            server_name: target.server_name.clone(),
        })
        .ok_or_else(|| {
            let known: Vec<&str> = configured
                .iter()
                .map(|target| target.server_name.as_str())
                .collect();
            invalid_params(format!(
                "'{wanted}' is not a searchable mailbox of {account} (known: {})",
                known.join(", ")
            ))
        })
}

/// One server search, from the credential resolution to the settle.
async fn run_search(
    request: SearchRequest,
    handle: OperationHandle,
    canonical: Arc<CanonicalState>,
) {
    handle.set_running();
    let name = request.account.name.clone();
    if let Err(e) = super::open_secrets(&name, request.secrets) {
        return handle.fail(DomainError::from(e));
    }
    let graph = request.account.auth_method == AuthMethod::Graph;
    let outcome = if graph {
        search_graph(&request, &handle, &canonical).await
    } else {
        search_imap(&request, &handle, &canonical).await
    };
    match outcome {
        Ok((hits, deduplicated, unreachable)) => handle.succeed(json!({
            "account": name,
            "query": request.query,
            "hits": hits,
            "deduplicated": deduplicated,
            "unreachable": unreachable,
        })),
        Err(e) => handle.fail(DomainError::from(super::server_error(&name, &e))),
    }
}

/// How many hits one mailbox of a `targets.len()`-mailbox search may take.
///
/// `limit` is a total budget split per mailbox, which is what the in-process
/// leg spent and what the protocol document says; a per-mailbox limit would
/// make a five-folder search cost five times what a one-folder search costs
/// for the same number.
fn per_mailbox(limit: usize, targets: usize) -> usize {
    (limit / targets.max(1)).max(MIN_PER_MAILBOX)
}

/// The plain-IMAP leg: one session, one `SEARCH` per mailbox.
async fn search_imap(
    request: &SearchRequest,
    handle: &OperationHandle,
    canonical: &CanonicalState,
) -> anyhow::Result<(usize, usize, Vec<Value>)> {
    // One grammar (#0086a): the caller already parsed to the shared AST, so
    // this renders it for *this* server. Gmail runs has:attachment through
    // X-GM-RAW; a plain server has no attachment key, so the residue is
    // post-filtered against the store.
    let imap = crate::config::ImapConfig::load(&request.account)?;
    let host = imap.host.to_ascii_lowercase();
    let gmail = host == "imap.gmail.com"
        || host.ends_with(".gmail.com")
        || host.ends_with("googlemail.com");
    let (imap_search, attachment_postfilter) = if gmail {
        (
            crate::search::to_gmail_search_command(&request.parsed),
            false,
        )
    } else {
        let lowered =
            crate::search::to_imap(&request.parsed).map_err(|e| anyhow::anyhow!("{e}"))?;
        (lowered.search, lowered.attachment_postfilter)
    };
    let message_id = request.parsed.message_id.clone();

    let mut session = crate::imap_client::open_imap_session(&imap).await?;
    let per_mailbox = per_mailbox(request.limit, request.targets.len());
    let mut state = Stream::new(request, handle, canonical, attachment_postfilter);

    for target in &request.targets {
        if state.spent >= request.limit {
            break;
        }
        let budget = per_mailbox.min(request.limit - state.spent);
        log::info!(
            "[search_server] querying mailbox '{}' (label={})",
            target.server_name,
            target.label,
        );
        match crate::imap_client::search_on_session(
            &mut session,
            &imap_search,
            message_id.as_deref(),
            &target.server_name,
            Some(budget),
        )
        .await
        {
            Ok(emails) => state.land(&target.label, emails),
            Err(e) => state.refused(&target.label, &e),
        }
    }
    session.logout().await.ok();
    Ok(state.settle())
}

/// The Graph leg, which searches a folder at a time over HTTP.
async fn search_graph(
    request: &SearchRequest,
    handle: &OperationHandle,
    canonical: &CanonicalState,
) -> anyhow::Result<(usize, usize, Vec<Value>)> {
    let config = crate::config::GraphConfig::load(&request.account)?;
    let client = crate::graph::GraphClient::new_async(&config).await?;
    let per_mailbox = per_mailbox(request.limit, request.targets.len());
    let mut state = Stream::new(request, handle, canonical, false);

    for target in &request.targets {
        if state.spent >= request.limit {
            break;
        }
        let budget = per_mailbox.min(request.limit - state.spent);
        match client
            .search_messages(&request.parsed, Some(&target.server_name), budget)
            .await
        {
            Ok(emails) => state.land(&target.label, emails),
            Err(e) => state.refused(&target.label, &e),
        }
    }
    Ok(state.settle())
}

/// What a running search has spent, dropped and published so far.
///
/// One type for both legs because the difference between them is the call that
/// produces a mailbox's emails and nothing else: the budget arithmetic, the
/// dedup, the store resolution and the event are the same fact either way.
struct Stream<'a> {
    request: &'a SearchRequest,
    handle: &'a OperationHandle,
    canonical: &'a CanonicalState,
    /// Whether a plain-IMAP `has:attachment` residue must be post-filtered
    /// against the store, which is #0086a option (b).
    attachment_postfilter: bool,
    /// Hits taken against the budget, dropped ones included: the budget is
    /// what the server was asked for, not what survived the dedup.
    spent: usize,
    /// Hits published.
    published: usize,
    /// Hits the local pass was already showing.
    deduplicated: usize,
    /// One `{mailbox, error}` per mailbox that refused.
    unreachable: Vec<Value>,
    /// Message-IDs this search has already published, so two mailboxes holding
    /// one message report it once.
    seen: HashSet<String>,
}

impl<'a> Stream<'a> {
    fn new(
        request: &'a SearchRequest,
        handle: &'a OperationHandle,
        canonical: &'a CanonicalState,
        attachment_postfilter: bool,
    ) -> Stream<'a> {
        Stream {
            request,
            handle,
            canonical,
            attachment_postfilter,
            spent: 0,
            published: 0,
            deduplicated: 0,
            unreachable: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// One mailbox's answer: resolve, dedup and publish.
    ///
    /// The store is opened and dropped inside this call and never held across
    /// an `.await`, which is what keeps the search future `Send`.
    fn land(&mut self, label: &str, emails: Vec<FetchedEmail>) {
        log::info!(
            "[search_server] '{label}' returned {} result(s)",
            emails.len()
        );
        self.spent += emails.len();
        let account = self.request.account.name.clone();
        let store = Store::open(crate::config::store_path(&account)).ok();
        let with_attachments = self
            .attachment_postfilter
            .then(|| {
                store
                    .as_ref()
                    .and_then(|store| read::message_ids_with_attachments(store, &account).ok())
            })
            .flatten();

        for email in emails {
            let key = email
                .message_id
                .as_deref()
                .map(read::normalize_message_id_key);
            // Plain-IMAP `has:attachment` (#0086a option b): keep only hits
            // the local store marks as carrying an attachment.
            if let (Some(with_attachments), Some(key)) = (with_attachments.as_ref(), key.as_ref()) {
                if !with_attachments.contains(key) {
                    continue;
                }
            }
            if let Some(key) = key.as_ref() {
                if self.request.excluded.contains(key) {
                    self.deduplicated += 1;
                    continue;
                }
                if !self.seen.insert(key.clone()) {
                    continue;
                }
            }
            let hit = to_hit(store.as_ref(), &account, label, &email);
            self.canonical.publish(Event::Lifecycle {
                kind: KIND_SERVER_SEARCH_HIT,
                payload: json!({
                    "operation_id": self.handle.id().as_str(),
                    "hit": hit,
                }),
            });
            self.published += 1;
        }
    }

    /// One mailbox the server refused, which does not fail the search.
    fn refused(&mut self, label: &str, error: &anyhow::Error) {
        log::warn!("[search_server] search in {label} failed: {error}");
        self.unreachable
            .push(json!({"mailbox": label, "error": format!("{error}")}));
    }

    /// `(hits, deduplicated, unreachable)`.
    fn settle(self) -> (usize, usize, Vec<Value>) {
        (self.published, self.deduplicated, self.unreachable)
    }
}

/// One fetched message as the hit the overlay renders.
///
/// `row_id` and `selector` are the daemon's own resolution of the hit against
/// the account's store, by `Message-ID`: a hit that resolves is a row the
/// overlay can act on, and one that does not says so with `null` rather than
/// with an empty string, because "not in the store" is the fact every
/// row-dependent key of the overlay branches on.
fn to_hit(
    store: Option<&Store>,
    account: &str,
    label: &str,
    email: &FetchedEmail,
) -> ServerSearchHit {
    let row =
        store.zip(email.message_id.as_deref()).and_then(
            |(store, id)| match read::find_by_message_id(store, account, id) {
                Ok(rows) => rows.into_iter().next(),
                Err(e) => {
                    log::warn!("[search_server] resolving {id} in {account}: {e:#}");
                    None
                }
            },
        );
    let (_display, date_sort) = resolve_date(
        &Some(email.date.clone()).filter(|date| !date.is_empty()),
        &None,
        std::path::Path::new(""),
    );
    ServerSearchHit {
        account: account.to_string(),
        mailbox: label.to_string(),
        message_id: email.message_id.clone(),
        row_id: row.as_ref().map(|row| row.id),
        selector: row
            .as_ref()
            .map(|row| Selector::for_message(account, row).to_string()),
        from: email.from.clone(),
        to: email.to.clone(),
        cc: email.cc.clone(),
        reply_to: email.reply_to.clone(),
        bcc: email.bcc.clone(),
        subject: email.subject.clone(),
        date_display: email.date.clone(),
        date_sort,
        flags: MessageFlags {
            seen: email.flags.seen,
            answered: email.flags.answered,
            forwarded: email.flags.forwarded,
            flagged: email.flags.flagged,
        },
        has_attachments: email.has_attachments,
        is_invite: email.event.is_some(),
        body_text: email.body_text.clone(),
        html_body: email.html_body.clone(),
    }
}

/// `-32006` for an account that configures no server at all, which is the same
/// refusal a sync of it makes.
#[allow(dead_code)]
fn local_only(name: &str) -> RpcError {
    RpcError {
        code: ErrorCode::AccountNotReady.code(),
        message: format!("{name} configures no server, so it cannot be searched"),
        data: Some(json!({"account": name, "state": "local_only"})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget is a total split across the mailboxes, with a floor so a
    /// narrow one still shows something.
    #[test]
    fn the_budget_is_split_across_the_mailboxes_with_a_floor() {
        assert_eq!(per_mailbox(50, 5), 10);
        assert_eq!(per_mailbox(50, 1), 50);
        assert_eq!(per_mailbox(12, 5), MIN_PER_MAILBOX);
        assert_eq!(
            per_mailbox(50, 0),
            50,
            "a search with no target spends none"
        );
    }

    /// A target is named by its label or by its server name, and anything else
    /// is `-32602` naming what the account has.
    #[test]
    fn a_target_is_named_by_its_label_or_its_server_name() {
        let configured = vec![
            SearchTarget {
                label: "Inbox".to_string(),
                server_name: "INBOX".to_string(),
            },
            SearchTarget {
                label: "Extra".to_string(),
                server_name: "Team/Reports".to_string(),
            },
        ];
        assert_eq!(
            pick_target(&configured, "inbox", "gamma")
                .unwrap()
                .server_name,
            "INBOX"
        );
        assert_eq!(
            pick_target(&configured, "Team/Reports", "gamma")
                .unwrap()
                .label,
            "Extra"
        );
        let refused = pick_target(&configured, "nosuch", "gamma").expect_err("no such mailbox");
        assert_eq!(refused.code, -32602);
        assert!(refused.message.contains("INBOX"), "{}", refused.message);
    }
}
