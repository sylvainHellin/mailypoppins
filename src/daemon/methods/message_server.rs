//! The two `message.*` operations that open a session on the account's mail
//! server: `message.search_server` and `message.fetch` (P5-U10c, #0126,
//! `LST-08` and `LST-09`).
//!
//! They were the TUI search overlay's server leg, moved off the client's own
//! background thread: `src/tui/helpers.rs`'s `lib_do_multi_search` and the
//! `f` key of `src/tui/actions.rs`, which asked the server for the uid of one
//! Message-ID and ingested the bytes it got back. The search is `mp search`'s
//! and the desktop's server leg too (`LST-06`).
//!
//! The search leg is `message.search_server` rather than `message.list_server`
//! because P4-U10 had given that name to `mp fetch`'s one-mailbox query; both
//! were retired on 2026-10-22 as a duplicate of `mp search --mailbox`, which
//! runs here.
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
//!
//! # What the limit means
//!
//! `limit` is the number of hits the whole search answers with (PERSO-175):
//! every mailbox is asked for its newest `limit` matches as stubs, the stubs
//! are merged newest first and cut to `limit`, and only the survivors are
//! downloaded whole, mailbox by mailbox ([`crate::server_search`]). The settle
//! says when the answer is cut, `truncated`, and when Microsoft Graph's
//! `$search` cap cut it, `capped`. `mp search`, the TUI overlay and the
//! desktop search bar all read the answer from this one call.
//!
//! # What the settle says about coverage
//!
//! A plain IMAP server has no search key for `has:attachment`, so the leg
//! keeps the hits the local store marks as carrying an attachment and the
//! settle names the term in `local_terms` with `coverage: "local"`
//! (PERSO-131), the two fields `mp search --json` carries (PERSO-122): an
//! un-synced message cannot be among those hits, and a client says so beside
//! its count.

use std::collections::HashSet;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::listing::{
    MessageFlags, SearchCoverage, ServerSearchHit, ServerSearchSettled, UnreachableMailbox,
};
use mp_protocol::RpcError;

use crate::config::{AccountConfig, AuthMethod};
use crate::daemon::state::events::Event;
use crate::daemon::state::{CanonicalState, Change};
use crate::parse::FetchedEmail;
use crate::selector::Selector;
use crate::server_search::{two_phase, Cut, GraphSearch, ImapSearch, Scope, SearchSink};
use crate::store::{read, BlobStore, Store};
use crate::tui::app::resolve_date;

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

/// The number of hits a search answers with when the caller names none.
const DEFAULT_SEARCH_LIMIT: usize = 50;

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
    let catalogue = crate::catalogue::Catalogue::load(account);
    let (mailbox, server_name) = fetch_storage(account, &catalogue, &wanted)?;
    let message_id = string_param(params, "message_id")?;
    if message_id.trim().is_empty() {
        return Err(invalid_params(
            "message_id is the Message-ID header of the message to fetch",
        ));
    }
    Ok(FetchRequest {
        account: account.clone(),
        mailbox,
        server_name,
        message_id,
        secrets: snapshot.config.secrets_backend,
    })
}

/// The store key a fetched hit is filed under and the server folder it is
/// fetched from, for the mailbox name the hit was reported under (#0141).
///
/// A sidebar mailbox first, by key, label or server name, which is what a hit
/// from a configured or a synced folder carries. Then any folder the server
/// lists, whatever its policy: a hit from a `status-only` folder (a Proton
/// label, the trash) or an `ignore` one (Proton's All Mail, an Exchange
/// non-mail folder) is stored under that folder's own key, where its selector
/// and the local search find it; nothing syncs an `ignore` folder, so such a
/// row stays as fetched. Refused: a name the last listing did not have, and
/// one that would land the row in a role's or the local Drafts' mailbox.
fn fetch_storage(
    account: &AccountConfig,
    catalogue: &crate::catalogue::Catalogue,
    wanted: &str,
) -> Result<(String, String), RpcError> {
    if wanted.trim().is_empty() {
        return Err(invalid_params("mailbox names the folder the hit came from"));
    }
    // A folder the server spells exactly so first: a hit is reported under
    // its server name when the sidebar does not list it, and that name may
    // repeat a sidebar label (a plain `Sent` beside the `\Sent` folder).
    if catalogue
        .folders
        .iter()
        .any(|f| f.server == wanted && !f.listed())
    {
        return catalogue.storage_for(wanted).map_err(invalid_params);
    }
    if let Ok(key) = super::message::resolve_in(account, catalogue, wanted) {
        let server = catalogue.server_for_key(&key);
        return Ok((key, server));
    }
    catalogue.storage_for(wanted).map_err(invalid_params)
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
    let Fetched {
        uid,
        raw,
        email,
        threading,
    } = match fetched {
        Ok(fetched) => fetched,
        Err(e) => return handle.fail(DomainError::from(super::server_error(&name, &e))),
    };

    // Graph already carries the whole payload and ingests under the synthetic
    // uid derived from the Message-ID; plain IMAP needs the uid the mailbox
    // holds the message under, since a made-up one would be pruned or
    // duplicated by the next sync.
    let ingested = ingest(
        &name,
        &request.mailbox,
        uid,
        &email,
        raw.as_deref(),
        threading.as_ref(),
    );
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
///
/// [`Store::open`] is right here and wrong in [`Stream::land`]: this method's
/// gate is [`super::account::ready_account`], so the store existed and probed
/// clean before the operation was ever started, and the same holds for
/// [`ingest`] below. Nothing on this path can create a store for an account
/// that has never synced.
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

/// One message a server handed `message.fetch`, as ingest takes it.
struct Fetched {
    uid: i64,
    /// The RFC822 bytes, which IMAP has and Graph does not.
    raw: Option<Vec<u8>>,
    email: FetchedEmail,
    /// The threading signals of a backend without raw bytes (PERSO-147): the
    /// Graph hit's `In-Reply-To`, `References` and `conversationId`.
    threading: Option<crate::ingest::ThreadHints>,
}

/// The uid, the raw bytes and the parsed message one IMAP server holds under
/// `message_id`.
async fn imap_fetch(request: &FetchRequest) -> anyhow::Result<Fetched> {
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
    Ok(Fetched {
        uid: uid as i64,
        raw: Some(raw),
        email,
        threading: None,
    })
}

/// The same, over Graph, which carries the whole payload already and keys the
/// row by the synthetic uid ingest derives from the Message-ID.
///
/// The lookup asks for the threading signals the sync download asks for
/// (PERSO-147), so the row threads at ingest and reads as asked; without them
/// it would root its own thread and the next sync's re-thread would list its
/// whole folder to settle this one row.
async fn graph_fetch(request: &FetchRequest) -> anyhow::Result<Fetched> {
    let server_name = request.server_name.clone();
    let config = crate::config::GraphConfig::load(&request.account)?;
    let client = crate::graph::GraphClient::new_async(&config).await?;
    let query = crate::search::Query {
        message_id: Some(request.message_id.clone()),
        ..Default::default()
    };
    let found = client
        .search_messages_threaded(&query, Some(&server_name), 1)
        .await?;
    let hit = found
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("the server no longer has that message"))?;
    Ok(Fetched {
        uid: crate::ingest::graph_uid(&request.message_id),
        raw: None,
        email: hit.email,
        threading: Some(hit.threading),
    })
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
    threading: Option<&crate::ingest::ThreadHints>,
) -> anyhow::Result<(i64, String, Option<Change>)> {
    let store = Store::open(crate::config::store_path(account))?;
    let blobs = BlobStore::for_account(account);
    ingest_into(&store, &blobs, account, mailbox, uid, email, raw, threading)
}

/// [`ingest`] into a store already open, which is the part a test can drive.
#[allow(clippy::too_many_arguments)]
fn ingest_into(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    mailbox: &str,
    uid: i64,
    email: &FetchedEmail,
    raw: Option<&[u8]>,
    threading: Option<&crate::ingest::ThreadHints>,
) -> anyhow::Result<(i64, String, Option<Change>)> {
    let outcome = crate::ingest::ingest_message_threaded(
        store,
        blobs,
        &crate::ingest::IngestInput {
            account,
            mailbox,
            uid,
            email,
            raw,
        },
        &crate::ingest::RebindPolicy::Always,
        threading,
    )?;
    let row = read::find_by_id(store, outcome.row_id)?;
    let selector = row
        .as_ref()
        .map(|row| Selector::for_message(account, row).to_string())
        .unwrap_or_default();
    let change = read::mailbox_read_counts(store, account)
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
#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchTarget {
    label: String,
    server_name: String,
}

/// The label a Graph search across every folder is reported under when it
/// fails as a whole; its hits are listed under their own folders.
const EVERY_FOLDER_LABEL: &str = "every folder";

/// What one validated search runs against.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Targets {
    /// These mailboxes, one first-pass query each.
    Mailboxes(Vec<SearchTarget>),
    /// Every folder of a Graph account in one query, the default when the
    /// caller names no mailbox and the query no `in:`: one request rather
    /// than one per configured folder, and a folder the account does not
    /// configure (an Archive subfolder) is searched too.
    EveryFolder,
}

/// One validated `message.search_server`.
struct SearchRequest {
    account: AccountConfig,
    /// What the user typed, kept verbatim for the settle.
    query: String,
    parsed: crate::search::Query,
    targets: Targets,
    /// The account's mailboxes that have a server name, as `(label, server
    /// name)`: what a hit found across every folder is listed under when its
    /// folder is one of them.
    configured: Vec<(String, String)>,
    /// The number of hits the whole search answers with (PERSO-175).
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
/// `LST-06`'s whole point. The field filters `mp search` takes as flags travel
/// under `message.search`'s names and build the same query.
fn search_request(
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<SearchRequest, RpcError> {
    let mut known = vec![
        "account",
        "query",
        "mailboxes",
        "limit",
        "exclude_message_ids",
    ];
    known.extend(super::message::SEARCH_FLAG_PARAMS);
    only_params("message.search_server", params, &known)?;
    let name = string_param(params, "account")?;
    let account = super::sync::syncable_account(&snapshot.accounts, &name)?;
    let query = string_param(params, "query")?;
    let parsed = crate::search::from_cli(&query, &super::message::search_flags(params)?)
        .map_err(invalid_params)?;

    // The sidebar's folders (#0141). A search that names no mailbox runs over
    // the `sync` ones, the received mail; a `status-only` folder is searched
    // when it is named, since a Proton label repeats a real folder's messages
    // and the trash holds what the user threw away.
    let catalogue = crate::catalogue::Catalogue::load(account);
    let rows = crate::catalogue::sidebar(account, &catalogue);
    let searchable = |default_only: bool| -> Vec<SearchTarget> {
        rows.iter()
            .filter(|row| !default_only || row.policy == crate::config::FolderPolicy::Sync)
            .filter_map(|row| {
                row.server.clone().map(|server_name| SearchTarget {
                    label: row.label.clone(),
                    server_name,
                })
            })
            .collect()
    };
    let configured = searchable(false);
    let default_targets = searchable(true);
    let targets = search_targets(
        params.get("mailboxes"),
        parsed.in_mailbox.as_deref(),
        account.auth_method == AuthMethod::Graph,
        &configured,
        &default_targets,
    )?;

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
        configured: configured
            .into_iter()
            .map(|target| (target.label, target.server_name))
            .collect(),
        limit,
        excluded,
        secrets: snapshot.config.secrets_backend,
    })
}

/// What a search runs against, from its `mailboxes` param, the query's own
/// `in:` directive, and whether the account is on Graph.
///
/// `mailboxes` first, then `in:`, which is the precedence `mp search` gives
/// `--mailbox` over `in:`. Naming neither searches every `sync` folder
/// on IMAP, which has no query across mailboxes, and every folder at once on
/// Graph (PERSO-175).
fn search_targets(
    mailboxes: Option<&Value>,
    in_mailbox: Option<&str>,
    graph: bool,
    configured: &[SearchTarget],
    default_targets: &[SearchTarget],
) -> Result<Targets, RpcError> {
    match mailboxes {
        None | Some(Value::Null) => Ok(match in_mailbox {
            Some(wanted) => Targets::Mailboxes(vec![pick_target(configured, wanted)?]),
            None if graph => Targets::EveryFolder,
            None => Targets::Mailboxes(default_targets.to_vec()),
        }),
        Some(Value::Array(names)) => names
            .iter()
            .map(|name| {
                let wanted = name
                    .as_str()
                    .ok_or_else(|| invalid_params("mailboxes is an array of mailbox names"))?;
                pick_target(configured, wanted)
            })
            .collect::<Result<Vec<SearchTarget>, RpcError>>()
            .map(Targets::Mailboxes),
        Some(_) => Err(invalid_params(
            "mailboxes is an array of mailbox names, or null for every searchable one",
        )),
    }
}

/// The target `wanted` names.
///
/// A configured mailbox by its sidebar label **or its server name**, which is
/// one spelling more than [`super::message::resolve_mailbox`] accepts: the
/// overlay holds a target of `(label, server_name)` and the search grammar's
/// `in:` directive is matched against both, so the wire takes whichever the
/// caller has in hand rather than making it re-derive the other.
///
/// Any other name is a folder on the server, searched under its own name
/// (Sylvain, 2026-10-22): the rule mailbox discovery (#0141) brings, where
/// every folder the server lists is a mailbox. A name the server does not
/// list fails that one mailbox, as a refused SELECT, and lands in the settle's
/// `unreachable`; it does not fail the call.
fn pick_target(configured: &[SearchTarget], wanted: &str) -> Result<SearchTarget, RpcError> {
    if wanted.trim().is_empty() {
        return Err(invalid_params("a mailbox name is not empty"));
    }
    Ok(configured
        .iter()
        .find(|target| {
            wanted.eq_ignore_ascii_case(&target.label)
                || wanted.eq_ignore_ascii_case(&target.server_name)
        })
        .cloned()
        .unwrap_or_else(|| SearchTarget {
            label: wanted.to_string(),
            server_name: wanted.to_string(),
        }))
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
        Ok(settled) => match serde_json::to_value(&settled) {
            Ok(settled) => handle.succeed(settled),
            Err(e) => handle.fail(DomainError::from(internal(format!("{e:#}")))),
        },
        Err(e) => handle.fail(DomainError::from(super::server_error(&name, &e))),
    }
}

/// The scopes of the request's targets, in search order.
fn scopes(request: &SearchRequest) -> Vec<Scope> {
    match &request.targets {
        Targets::Mailboxes(targets) => targets
            .iter()
            .map(|target| Scope::Mailbox(target.server_name.clone()))
            .collect(),
        Targets::EveryFolder => vec![Scope::EveryFolder],
    }
}

/// The label the `index`-th scope of the request is reported under.
fn target_label(request: &SearchRequest, index: usize) -> &str {
    match &request.targets {
        Targets::Mailboxes(targets) => &targets[index].label,
        Targets::EveryFolder => EVERY_FOLDER_LABEL,
    }
}

/// The plain-IMAP leg: one session, both passes on it.
async fn search_imap(
    request: &SearchRequest,
    handle: &OperationHandle,
    canonical: &CanonicalState,
) -> anyhow::Result<ServerSearchSettled> {
    // One grammar (#0086a): the caller already parsed to the shared AST, so
    // this renders it for *this* server. Gmail runs has:attachment through
    // X-GM-RAW; a plain server has no attachment key, so the residue is
    // post-filtered against the store, and the settle names it in
    // `local_terms` (PERSO-131) as `mp search --json` does (PERSO-122).
    let imap = crate::config::ImapConfig::load(&request.account)?;
    let host = imap.host.to_ascii_lowercase();
    let gmail = host == "imap.gmail.com"
        || host.ends_with(".gmail.com")
        || host.ends_with("googlemail.com");
    let (imap_search, attachment_postfilter, local_terms) = imap_lowering(&request.parsed, gmail)?;
    let mut stream = Stream::new(
        request,
        handle,
        canonical,
        attachment_postfilter,
        local_terms,
    );

    let mut session = crate::imap_client::open_imap_session(&imap).await?;
    let mut backend = ImapSearch {
        session: &mut session,
        search: imap_search,
        message_id: request.parsed.message_id.clone(),
    };
    let cut = two_phase(&mut backend, &scopes(request), request.limit, &mut stream).await;
    session.logout().await.ok();
    Ok(stream.settle(cut))
}

/// The SEARCH command one IMAP server runs for `parsed`, whether the hits
/// must be post-filtered against the store's attachment index, and the terms
/// that post-filter answers, which is the settle's `local_terms`.
///
/// Gmail runs every term through `X-GM-RAW`, and a plain server has no
/// attachment key.
fn imap_lowering(
    parsed: &crate::search::Query,
    gmail: bool,
) -> anyhow::Result<(String, bool, Vec<String>)> {
    if gmail {
        let search =
            crate::search::to_gmail_search_command(parsed).map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok((search, false, Vec::new()));
    }
    let lowered = crate::search::to_imap(parsed).map_err(|e| anyhow::anyhow!("{e}"))?;
    let local_terms = lowered.local_terms();
    Ok((lowered.search, lowered.attachment_postfilter, local_terms))
}

/// `local` when the store answered any term, `server` otherwise.
fn coverage_of(local_terms: &[String]) -> SearchCoverage {
    if local_terms.is_empty() {
        SearchCoverage::Server
    } else {
        SearchCoverage::Local
    }
}

/// The Graph leg, which searches the named folders one at a time, or every
/// folder in one query when none is named, over HTTP.
async fn search_graph(
    request: &SearchRequest,
    handle: &OperationHandle,
    canonical: &CanonicalState,
) -> anyhow::Result<ServerSearchSettled> {
    let config = crate::config::GraphConfig::load(&request.account)?;
    let client = crate::graph::GraphClient::new_async(&config).await?;
    let mut stream = Stream::new(request, handle, canonical, false, Vec::new());
    let mut backend = GraphSearch::new(&client, &request.parsed, &request.configured);
    let cut = two_phase(&mut backend, &scopes(request), request.limit, &mut stream).await;
    Ok(stream.settle(cut))
}

/// What a running search has dropped and published so far.
///
/// One type for both legs because the difference between them is the backend
/// that answers the two passes and nothing else: the dedup, the attachment
/// post-filter, the store resolution and the event are the same fact either
/// way.
struct Stream<'a> {
    request: &'a SearchRequest,
    handle: &'a OperationHandle,
    canonical: &'a CanonicalState,
    /// The normalised Message-IDs the store marks as carrying an attachment,
    /// when a plain-IMAP `has:attachment` residue is post-filtered against
    /// the store (#0086a option b). `None` when nothing is post-filtered.
    with_attachments: Option<HashSet<String>>,
    /// The terms the store answered rather than the server, which the settle
    /// reports so a client can say un-synced mail is not covered (PERSO-131).
    local_terms: Vec<String>,
    /// Hits published.
    published: usize,
    /// Hits the local pass was already showing.
    deduplicated: usize,
    /// One `{mailbox, error}` per mailbox that refused.
    unreachable: Vec<UnreachableMailbox>,
    /// Message-IDs this search has already admitted, so two mailboxes holding
    /// one message report it once.
    seen: HashSet<String>,
}

impl<'a> Stream<'a> {
    /// A stream for `request`, reading the store's attachment index once
    /// when the post-filter needs it.
    ///
    /// [`crate::store::open_store`] and never [`Store::open`]: this method's
    /// gate is [`super::sync::syncable_account`], which admits an account that
    /// has never synced, and `Store::open` *creates* the file it is handed. A
    /// search against such an account would therefore hand it an empty store,
    /// after which [`super::account::state_of_path`] reports it `ready` and
    /// every read method answers an empty listing where `-32006` is owed. An
    /// account with no store has no hit carrying an attachment, which is the
    /// truth: un-synced mail is what `local_terms` says is not covered.
    fn new(
        request: &'a SearchRequest,
        handle: &'a OperationHandle,
        canonical: &'a CanonicalState,
        attachment_postfilter: bool,
        local_terms: Vec<String>,
    ) -> Stream<'a> {
        let account = &request.account.name;
        let with_attachments = attachment_postfilter.then(|| {
            let Some(store) = crate::store::open_store(account) else {
                return HashSet::new();
            };
            read::message_ids_with_attachments(&store, account).unwrap_or_else(|e| {
                log::warn!("[search_server] reading the attachment index of {account}: {e:#}");
                HashSet::new()
            })
        });
        Stream {
            request,
            handle,
            canonical,
            with_attachments,
            local_terms,
            published: 0,
            deduplicated: 0,
            unreachable: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// What the operation settles with.
    fn settle(self, cut: Cut) -> ServerSearchSettled {
        let coverage = coverage_of(&self.local_terms);
        ServerSearchSettled {
            account: self.request.account.name.clone(),
            query: self.request.query.clone(),
            hits: self.published as u64,
            deduplicated: self.deduplicated as u64,
            unreachable: self.unreachable,
            coverage,
            local_terms: self.local_terms,
            truncated: cut.truncated,
            capped: cut.capped,
        }
    }
}

impl SearchSink for Stream<'_> {
    /// The attachment post-filter, the local pass's exclusions and the
    /// cross-mailbox dedup, all on the first pass, so a hit any of them drops
    /// neither takes a place under the limit nor is downloaded.
    fn admit(&mut self, message_id: Option<&str>) -> bool {
        match admission(
            message_id,
            self.with_attachments.as_ref(),
            &self.request.excluded,
            &mut self.seen,
        ) {
            Admission::Admitted => true,
            Admission::Excluded => {
                self.deduplicated += 1;
                false
            }
            Admission::NoAttachment | Admission::Duplicate => false,
        }
    }

    /// One mailbox's downloaded hits: resolve against the store and publish.
    ///
    /// The store is opened and dropped inside this call and never held across
    /// an `.await`, which is what keeps the search future `Send`; it is
    /// optional for the reason [`Stream::new`] gives, and `to_hit` takes a
    /// `None` store as "the local store knows nothing about this hit".
    fn land(&mut self, mailbox: usize, folder: Option<&str>, emails: Vec<FetchedEmail>) {
        let label = folder.unwrap_or_else(|| target_label(self.request, mailbox));
        log::info!("[search_server] '{label}' landed {} hit(s)", emails.len());
        let account = &self.request.account.name;
        let store = crate::store::open_store(account);
        for email in emails {
            let hit = to_hit(store.as_ref(), account, label, &email);
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
    fn refused(&mut self, mailbox: usize, error: &anyhow::Error) {
        let label = target_label(self.request, mailbox);
        log::warn!("[search_server] search in {label} failed: {error}");
        self.unreachable.push(UnreachableMailbox {
            mailbox: label.to_string(),
            error: format!("{error}"),
        });
    }
}

/// What the first pass does with one hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    /// It competes for the limit.
    Admitted,
    /// The store marks no attachment on it, under a plain-IMAP
    /// `has:attachment`; a hit without a Message-ID has no mark to read.
    NoAttachment,
    /// The local pass is already showing it, which the settle counts.
    Excluded,
    /// Another mailbox already admitted it.
    Duplicate,
}

/// The [`Admission`] of a hit carrying `message_id`, recording it in `seen`
/// when admitted.
///
/// Plain-IMAP `has:attachment` (#0086a option b) keeps only hits the local
/// store marks as carrying an attachment, `with_attachments` being that set;
/// the exclusions and the dedup are by normalised Message-ID, so a hit
/// without one is never a duplicate.
fn admission(
    message_id: Option<&str>,
    with_attachments: Option<&HashSet<String>>,
    excluded: &HashSet<String>,
    seen: &mut HashSet<String>,
) -> Admission {
    let key = message_id.map(read::normalize_message_id_key);
    if let Some(with_attachments) = with_attachments {
        if !key
            .as_ref()
            .is_some_and(|key| with_attachments.contains(key))
        {
            return Admission::NoAttachment;
        }
    }
    let Some(key) = key else {
        return Admission::Admitted;
    };
    if excluded.contains(&key) {
        Admission::Excluded
    } else if seen.insert(key) {
        Admission::Admitted
    } else {
        Admission::Duplicate
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain IMAP server answers `has:attachment` from the store and the
    /// settle says so; Gmail answers it itself (PERSO-131).
    #[test]
    fn a_plain_imap_attachment_term_is_reported_as_local() {
        let parsed = crate::search::from_cli(
            "from:borrmann has:attachment",
            &crate::search::Flags::default(),
        )
        .expect("the query parses");

        let (_, postfilter, local_terms) = imap_lowering(&parsed, false).expect("plain IMAP");
        assert!(postfilter);
        assert_eq!(local_terms, vec!["has:attachment".to_string()]);
        assert_eq!(coverage_of(&local_terms), SearchCoverage::Local);

        let (_, postfilter, local_terms) = imap_lowering(&parsed, true).expect("Gmail");
        assert!(!postfilter);
        assert!(local_terms.is_empty());
        assert_eq!(coverage_of(&local_terms), SearchCoverage::Server);

        let plain = crate::search::from_cli("from:borrmann", &crate::search::Flags::default())
            .expect("the query parses");
        let (_, _, local_terms) = imap_lowering(&plain, false).expect("plain IMAP");
        assert_eq!(coverage_of(&local_terms), SearchCoverage::Server);
    }

    /// A target is named by its label or by its server name, and any other
    /// name is a folder on the server, searched under that name (PERSO-175):
    /// `mp search --mailbox Spesen` reaches a folder the account does not
    /// configure. Only an empty name is refused.
    #[test]
    fn a_target_is_a_configured_mailbox_or_any_server_folder() {
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
            pick_target(&configured, "inbox").unwrap().server_name,
            "INBOX"
        );
        assert_eq!(
            pick_target(&configured, "Team/Reports").unwrap().label,
            "Extra"
        );
        assert_eq!(
            pick_target(&configured, "Spesen").unwrap(),
            SearchTarget {
                label: "Spesen".to_string(),
                server_name: "Spesen".to_string(),
            }
        );
        let refused = pick_target(&configured, " ").expect_err("an empty name");
        assert_eq!(refused.code, -32602);
    }

    /// A search that names no mailbox and no `in:` runs across every folder
    /// in one query on Graph, which reaches a folder the account does not
    /// configure, and over the `sync` folders on IMAP, leaving a counts-only
    /// label or trash out (#0141); a named one is searched alone on either,
    /// counts-only included (PERSO-175).
    #[test]
    fn graph_searches_every_folder_at_once_when_no_mailbox_is_named() {
        let target = |name: &str| SearchTarget {
            label: name.to_string(),
            server_name: name.to_string(),
        };
        let configured = vec![target("Inbox"), target("Archive"), target("Labels/AI")];
        let default_targets = configured[..2].to_vec();
        assert_eq!(
            search_targets(None, None, true, &configured, &default_targets).unwrap(),
            Targets::EveryFolder
        );
        assert_eq!(
            search_targets(
                Some(&Value::Null),
                None,
                false,
                &configured,
                &default_targets
            )
            .unwrap(),
            Targets::Mailboxes(default_targets.clone())
        );
        assert_eq!(
            search_targets(None, Some("archive"), true, &configured, &default_targets).unwrap(),
            Targets::Mailboxes(vec![configured[1].clone()])
        );
        assert_eq!(
            search_targets(
                Some(&json!(["Inbox"])),
                None,
                true,
                &configured,
                &default_targets
            )
            .unwrap(),
            Targets::Mailboxes(vec![configured[0].clone()])
        );
        assert_eq!(
            search_targets(
                None,
                Some("labels/ai"),
                false,
                &configured,
                &default_targets
            )
            .unwrap(),
            Targets::Mailboxes(vec![configured[2].clone()])
        );
    }

    /// The first pass drops a hit the store marks no attachment on under a
    /// plain-IMAP `has:attachment`, one the local pass is showing, and the
    /// second copy of one another mailbox holds, before any of them takes a
    /// place under the limit (PERSO-175).
    #[test]
    fn the_first_pass_filters_before_the_limit_is_spent() {
        let key = |mid: &str| read::normalize_message_id_key(mid);
        let with_attachments: HashSet<String> =
            ["<pdf@x>", "<shown@x>"].into_iter().map(key).collect();
        let excluded: HashSet<String> = [key("<shown@x>")].into_iter().collect();
        let mut seen = HashSet::new();
        let mut admit = |mid: Option<&str>, attachments: bool| {
            admission(
                mid,
                attachments.then_some(&with_attachments),
                &excluded,
                &mut seen,
            )
        };

        assert_eq!(admit(Some("<pdf@x>"), true), Admission::Admitted);
        assert_eq!(admit(Some("<PDF@x>"), true), Admission::Duplicate);
        assert_eq!(admit(Some("<plain@x>"), true), Admission::NoAttachment);
        assert_eq!(admit(None, true), Admission::NoAttachment);
        assert_eq!(admit(Some("<shown@x>"), true), Admission::Excluded);
        assert_eq!(admit(Some("<plain@x>"), false), Admission::Admitted);
        assert_eq!(admit(None, false), Admission::Admitted);
    }

    fn email(mid: &str) -> FetchedEmail {
        FetchedEmail {
            from: "a@x".into(),
            to: "me@x".into(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: "Re: plan".into(),
            date: "Thu, 22 Oct 2026 10:00:00 +0000".into(),
            body_text: "body".into(),
            html_body: None,
            has_attachments: false,
            server_has_attachments: None,
            message_id: Some(mid.into()),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        }
    }

    /// A Graph hit `message.fetch` lands joins its stored parent at ingest and
    /// reads as asked, so the next sync's re-thread has no reason to list the
    /// folder for it (PERSO-147).
    #[test]
    fn a_fetched_graph_hit_threads_onto_its_stored_parent() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let ingest_one = |mid: &str, threading: Option<&crate::ingest::ThreadHints>| {
            ingest_into(
                &store,
                &blobs,
                "acct",
                "Inbox",
                crate::ingest::graph_uid(mid),
                &email(mid),
                None,
                threading,
            )
            .unwrap()
            .0
        };
        let field = |row: i64, column: &str| -> Option<String> {
            store
                .conn()
                .query_row(
                    &format!("SELECT {column} FROM messages WHERE id = ?1"),
                    [row],
                    |r| r.get(0),
                )
                .unwrap()
        };

        let parent = ingest_one("<p@x>", None);
        let hit = ingest_one(
            "<r@x>",
            Some(&crate::ingest::ThreadHints {
                in_reply_to: Some("<p@x>".into()),
                references: Some("<p@x>".into()),
                conversation_id: Some("AAQkConv=".into()),
            }),
        );
        assert_eq!(field(hit, "thread_id"), field(parent, "thread_id"));
        assert_eq!(field(hit, "conversation_id").as_deref(), Some("AAQkConv="));
        assert!(
            !crate::ingest::rethread_candidates(&store, "acct", "Inbox")
                .unwrap()
                .iter()
                .any(|c| c.row_id == hit),
            "the fetched hit is not a re-thread candidate"
        );
    }

    /// A server-search hit from any folder the server lists is fetchable
    /// through `message.fetch` (#0141): a folder the config never names, a
    /// counts-only label and an ignored union view are each stored under the
    /// folder's own key; a name the server does not list, and one that would
    /// land the row in a role's or the local Drafts' mailbox, are refused.
    /// Before the catalogue, every one of these but the Inbox was "not a
    /// mailbox of" the account.
    #[test]
    fn a_hit_from_any_server_folder_is_fetchable() {
        use crate::catalogue::{Catalogue, ListedFolder};
        let account = AccountConfig {
            name: "proton".to_string(),
            mailboxes: crate::config::MailboxesConfig {
                inbox: Some(crate::config::MailboxMapping {
                    server: "INBOX".to_string(),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let listed =
            |name: &str, attributes: &[&str]| ListedFolder::named(name).with_attributes(attributes);
        let catalogue = Catalogue::resolve(
            &account,
            &[
                listed("INBOX", &[]),
                listed("Folders", &["\\Noselect"]),
                listed("Labels", &["\\Noselect"]),
                listed("Projects", &[]),
                listed("Labels/AI", &[]),
                listed("All Mail", &["\\All"]),
                listed("Drafts", &["\\Drafts"]),
                listed("Sent", &["\\Sent"]),
                listed("sent", &[]),
            ],
        );
        let stored = |wanted: &str| fetch_storage(&account, &catalogue, wanted);
        let pair = |key: &str, server: &str| Ok((key.to_string(), server.to_string()));

        assert_eq!(stored("Inbox"), pair("inbox", "INBOX"));
        assert_eq!(
            stored("Projects"),
            pair("Projects", "Projects"),
            "never configured"
        );
        assert_eq!(
            stored("Labels/AI"),
            pair("Labels/AI", "Labels/AI"),
            "counts only"
        );
        assert_eq!(stored("All Mail"), pair("All Mail", "All Mail"), "ignored");
        assert_eq!(
            stored("Sent"),
            pair("sent", "Sent"),
            "the role's own folder"
        );
        for refused in ["Drafts", "sent", "Never Listed"] {
            let error = stored(refused).expect_err(refused);
            assert_eq!(error.code, -32602, "{refused}");
        }

        // The row lands under the folder's key, where its selector names it.
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let (key, _) = stored("Labels/AI").unwrap();
        let (row, selector, counts) = ingest_into(
            &store,
            &blobs,
            "proton",
            &key,
            7,
            &email("<l@x>"),
            None,
            None,
        )
        .unwrap();
        assert!(row > 0);
        assert_eq!(selector, "mp://proton/Labels%2FAI/l@x");
        assert!(matches!(
            counts,
            Some(Change::MailboxCounts { ref mailbox, total: 1, .. }) if mailbox == "Labels/AI"
        ));
    }
}
