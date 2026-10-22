//! The `contact.*` family: the frecency index, read and rebuilt (P4-U14).
//!
//! ```text
//! contact.rebuild      {account}                -> {operation_id}
//!                      settles {account, contacts, kept, saved, cache_path}
//! contact.search       {account, query, limit}  -> {account, query, contacts, rebuild}
//! contact.stats        {account}                -> {account, total, sent_to, …}
//! contact.vcard_draft  {account, name, address, display_name}
//!                                               -> {draft, vcf}
//! ```
//!
//! Every write of an account's index is announced as `contact.changed`
//! `{account, contacts}` (the daemon leftovers, 2026-10-22): a rebuild the
//! cache guard let through, the build a search ran because there was no
//! index, and the bump a sync or a send gives the addresses it saw
//! ([`contact_changed`]). A search whose own build the guard refused answers
//! the kept index and says so in `rebuild`, `null` otherwise.
//!
//! `contact.vcard_draft` is the TUI's and the desktop's `v`: a draft addressed
//! to the contact, subject `Contact: <name>`, no signature, with the contact's
//! vCard written under the drafts directory's `_vcards/` and attached.
//!
//! A row is `{address, display_name, sent_to, sent_cc, received, first_seen,
//! last_seen, score}`: the field names of [`crate::contacts::Contact`] plus the
//! match score, because `mp contacts search --parsable` prints
//! `address\tdisplay_name` and a renamed pair would make the tab-delimited
//! line a translation rather than a projection (`ANO-8`). The two timestamps
//! are what the TUI's detail pane shows and the ranking's recency reads, so a
//! client that lists the whole index with an empty query can rank a typed
//! query over it as the daemon would.
//!
//! `contact.rebuild` is an [`MethodKind::Operation`] and the two reads are
//! queries. A rebuild walks every row of every mailbox of the account and
//! re-ranks the result; on a real mailbox that is seconds, it is what the
//! TUI's `r` key runs, and a GUI wants to watch it.
//!
//! The two reads take [`super::account::ready_account`], so an account with no
//! store is `-32006`; the rebuild takes
//! [`super::account::configured_account`], so it does *not*, because
//! `build_index_for_account` treats a storeless account as an empty index and
//! `mp contacts rebuild` with no `--account` has always walked every
//! configured account including those.
//!
//! ## A cancelled rebuild writes nothing
//!
//! The rebuild observes its cancel token twice: inside the walk, every 256 rows
//! (`STOP_EVERY_ROWS`) through [`build_index_for_account_until`], and once
//! more at the commit point, the
//! cache write, through [`commit_point`]. A rebuild cancelled anywhere before
//! that write drops the index it built and leaves the cache on disk exactly as
//! it was; the registry has already settled it `cancelled`.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::contact::{RefusedRebuild, VcardDraft};
use mp_protocol::draft::DraftCreated;
use mp_protocol::events::{ContactChanged, KIND_CONTACT_CHANGED};
use mp_protocol::RpcError;

use crate::config::{AccountConfig, EmailSettings};
use crate::contacts::{
    build_index_for_account, build_index_for_account_until, cache_path, contact_to_vcard,
    load_cache, save_rebuilt_cache, search, vcard_file_stem, CacheSave, Contact, ContactIndex,
    ContactSource, MatchResult,
};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};
use super::super::operations::{OperationHandle, OperationRegistry, Progress};
use super::super::state::events::Event;
use super::super::state::{commit_point, CanonicalState, ConnectionId};
use super::{internal, invalid_params, only_params, string_param};

/// The phase a rebuild reports under, which is the family's own word.
pub const REBUILD_PHASE: &str = "contacts";

/// How many rows `contact.stats` carries, which is what `mp contacts stats`
/// prints under `Top 10:`.
const TOP_ROWS: usize = 10;

/// The directory under an account's drafts directory a vCard is written to.
pub const VCARD_DIR: &str = "_vcards";

/// The three methods of the admin slice, in method-name order.
pub const CONTACT_METHOD_SPECS: [MethodSpec; 3] = [
    MethodSpec::new("contact.rebuild", MethodKind::Operation, 1),
    MethodSpec::new("contact.search", MethodKind::Query, 1),
    MethodSpec::new("contact.stats", MethodKind::Query, 1),
];

/// `contact.vcard_draft`, in an array of its own so the admin slice's three
/// stay three (the daemon leftovers, 2026-10-22).
pub const CONTACT_VCARD_METHOD_SPECS: [MethodSpec; 1] = [MethodSpec::new(
    "contact.vcard_draft",
    MethodKind::Command,
    1,
)];

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
    canonical: Arc<CanonicalState>,
) {
    for spec in CONTACT_METHOD_SPECS
        .into_iter()
        .chain(CONTACT_VCARD_METHOD_SPECS)
    {
        dispatcher.register(Arc::new(ContactMethod {
            spec,
            config: Arc::clone(&config),
            operations: Arc::clone(&operations),
            canonical: Arc::clone(&canonical),
        }));
    }
}

/// One of the four, selected by its own [`MethodSpec`].
pub struct ContactMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
    canonical: Arc<CanonicalState>,
}

/// The `contact.changed` an index write publishes.
pub fn contact_changed(account: &str, contacts: usize) -> Event {
    Event::Lifecycle {
        kind: KIND_CONTACT_CHANGED,
        payload: serde_json::to_value(ContactChanged {
            account: account.to_string(),
            contacts: contacts as u64,
        })
        .unwrap_or_else(|_| json!({"account": account})),
    }
}

impl Method for ContactMethod {
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
            match self.spec.name {
                "contact.search" => {
                    only_params("contact.search", &params, &["account", "limit", "query"])?;
                    let name = string_param(&params, "account")?;
                    let account = super::account::ready_account(&snapshot.accounts, &name)?.clone();
                    let (answer, written) = search_result(&account, &params)?;
                    if let Some(contacts) = written {
                        self.canonical
                            .publish(contact_changed(&account.name, contacts));
                    }
                    Ok(Outcome::query(answer))
                }
                "contact.vcard_draft" => {
                    only_params(
                        "contact.vcard_draft",
                        &params,
                        &["account", "address", "display_name", "name"],
                    )?;
                    let accounts = snapshot.accounts.as_slice();
                    let email = &snapshot.config.email;
                    let answer = vcard_draft(&params, accounts, email)?;
                    let resource = format!("draft:{}/{}", answer.draft.account, answer.draft.id);
                    let answer = serde_json::to_value(answer)
                        .map_err(|e| internal(format!("serialising contact.vcard_draft: {e}")))?;
                    Ok(Outcome::command(
                        answer,
                        self.canonical.revision().get(),
                        vec![ResourceId::new(resource)],
                    ))
                }
                "contact.stats" => {
                    only_params("contact.stats", &params, &["account"])?;
                    let name = string_param(&params, "account")?;
                    let account = super::account::ready_account(&snapshot.accounts, &name)?.clone();
                    Ok(Outcome::query(stats_result(&account)?))
                }
                _ => {
                    only_params("contact.rebuild", &params, &["account"])?;
                    let name = string_param(&params, "account")?;
                    let account =
                        super::account::configured_account(&snapshot.accounts, &name)?.clone();
                    let (id, handle) = self.operations.start(
                        ConnectionId(ctx.connection_id),
                        self.spec.cancel_scope,
                        self.spec.name,
                    );
                    tokio::spawn(rebuild(account, handle));
                    Ok(Outcome::query(json!({"operation_id": id.as_str()})))
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// The reads
// ---------------------------------------------------------------------------

/// `contact.search`: the ranked rows a query matches, capped at `limit`, and
/// how many contacts an index this search built and wrote holds, which the
/// caller announces.
///
/// A query nothing matches is an empty array rather than a refusal: "no
/// matches" is an answer. `rebuild` is the [`RefusedRebuild`] of a build the
/// cache guard refused, `null` otherwise.
fn search_result(
    account: &AccountConfig,
    params: &Value,
) -> Result<(Value, Option<usize>), RpcError> {
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(20);
    let loaded = load_index(account)?;
    let answer = json!({
        "account": account.name,
        "query": query,
        "contacts": rows(&search(&loaded.index, &query, limit)),
        "rebuild": loaded.refused,
    });
    Ok((answer, loaded.written))
}

/// `contact.stats`: the totals, the cache the answer was read from, when it
/// was built, and the top rows.
fn stats_result(account: &AccountConfig) -> Result<Value, RpcError> {
    let root = crate::config::account_dir(&account.name);
    let index = load_index(account)?.index;
    Ok(json!({
        "account": account.name,
        "total": index.contacts.len(),
        "sent_to": index.contacts.values().map(|c| u64::from(c.sent_to)).sum::<u64>(),
        "sent_cc": index.contacts.values().map(|c| u64::from(c.sent_cc)).sum::<u64>(),
        "received": index.contacts.values().map(|c| u64::from(c.received)).sum::<u64>(),
        "built_at": index.built_at,
        "cache_path": cache_path(&root).display().to_string(),
        "top": rows(&search(&index, "", TOP_ROWS)),
    }))
}

/// Every matched contact as the row the wire carries.
fn rows(matches: &[MatchResult<'_>]) -> Vec<Value> {
    matches
        .iter()
        .map(|m| {
            json!({
                "address": m.contact.address,
                "display_name": m.contact.display_name,
                "sent_to": m.contact.sent_to,
                "sent_cc": m.contact.sent_cc,
                "received": m.contact.received,
                "first_seen": m.contact.first_seen,
                "last_seen": m.contact.last_seen,
                "score": m.score,
            })
        })
        .collect()
}

/// The index a read answers from, and what loading it did.
struct Loaded {
    index: ContactIndex,
    /// How many contacts an index this read built and wrote holds.
    written: Option<usize>,
    /// The build the cache guard refused, when it did.
    refused: Option<RefusedRebuild>,
}

/// The cached index, built on demand when there is none.
///
/// [`crate::contacts_cmd`]'s `load_or_build`, moved: the guard that refuses to
/// replace a populated cache with a thin rebuild (#0067) keeps what is on
/// disk, so a refusal shows the disk rather than the rebuild that was just
/// refused, and says so in [`Loaded::refused`], which `contact.search`
/// answers as `rebuild` so a client can tell the user.
fn load_index(account: &AccountConfig) -> Result<Loaded, RpcError> {
    let root = crate::config::account_dir(&account.name);
    let opening = |e: anyhow::Error| internal(format!("{e:#}"));
    if let Some(index) = load_cache(&root).map_err(opening)? {
        return Ok(Loaded {
            index,
            written: None,
            refused: None,
        });
    }
    let index = build_index_for_account(account).map_err(opening)?;
    settle_built(&root, account, index).map_err(opening)
}

/// Offer an index a read built to the cache, and answer what the read then
/// serves: the built index when it was written, the kept one when the guard
/// refused it.
fn settle_built(
    root: &std::path::Path,
    account: &AccountConfig,
    index: ContactIndex,
) -> anyhow::Result<Loaded> {
    let found = index.contacts.len();
    let (saved, kept) = match save_rebuilt_cache(root, &index)? {
        CacheSave::Written => {
            return Ok(Loaded {
                written: Some(found),
                index,
                refused: None,
            })
        }
        CacheSave::RefusedEmpty { kept } => ("refused_empty", kept),
        CacheSave::RefusedShrunk { kept, .. } => ("refused_shrunk", kept),
    };
    log::warn!(
        "[daemon] contacts rebuild for {} refused ({saved}); serving the cache at {}",
        account.name,
        cache_path(root).display()
    );
    Ok(Loaded {
        index: load_cache(root)?.unwrap_or(index),
        written: None,
        refused: Some(RefusedRebuild {
            saved: saved.to_string(),
            contacts: found as u64,
            kept: kept as u64,
        }),
    })
}

// ---------------------------------------------------------------------------
// The vCard draft
// ---------------------------------------------------------------------------

/// The name a vCard draft's subject gives the contact: the display name, else
/// the address's local part.
pub fn vcard_name(display_name: &str, address: &str) -> String {
    let name = display_name.trim();
    if name.is_empty() {
        address.split('@').next().unwrap_or(address).to_string()
    } else {
        name.to_string()
    }
}

/// The first of `<stem>.vcf`, `<stem>-1.vcf`, `<stem>-2.vcf`, ... not taken in
/// `dir`.
fn vcard_path(dir: &std::path::Path, stem: &str) -> std::path::PathBuf {
    let mut path = dir.join(format!("{stem}.vcf"));
    let mut n = 1usize;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.vcf"));
        n += 1;
    }
    path
}

/// `contact.vcard_draft`: `draft.create` of `name` addressed to the contact,
/// no signature, then the vCard under `_vcards/` beside it, then `draft.attach`
/// of its absolute path.
fn vcard_draft(
    params: &Value,
    accounts: &[AccountConfig],
    email: &EmailSettings,
) -> Result<VcardDraft, RpcError> {
    let account = string_param(params, "account")?;
    let name = string_param(params, "name")?;
    let address = string_param(params, "address")?.trim().to_string();
    if address.is_empty() {
        return Err(invalid_params("A vCard needs the contact's address"));
    }
    let display_name = params
        .get("display_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let contact = Contact {
        address: address.clone(),
        display_name: display_name.clone(),
        sent_to: 0,
        sent_cc: 0,
        received: 0,
        first_seen: String::new(),
        last_seen: String::new(),
        source: ContactSource::default(),
    };
    let created = super::draft::create(
        &json!({
            "account": account,
            "name": name,
            "no_signature": true,
            "headers": {
                "to": mp_core::addresses::format_recipient(&display_name, &address),
                "cc": "",
                "bcc": "",
                "subject": format!("Contact: {}", vcard_name(&display_name, &address)),
            },
        }),
        accounts,
        email,
    )?;
    let draft: DraftCreated = serde_json::from_value(created)
        .map_err(|e| internal(format!("reading the created draft: {e}")))?;
    let drafts = std::path::Path::new(&draft.path)
        .parent()
        .ok_or_else(|| internal(format!("{} has no directory", draft.path)))?;
    let dir = drafts.join(VCARD_DIR);
    crate::config::create_private_dir_all(&dir)
        .map_err(|e| internal(format!("creating {}: {e}", dir.display())))?;
    let vcf = vcard_path(&dir, &vcard_file_stem(&contact));
    std::fs::write(&vcf, contact_to_vcard(&contact))
        .map_err(|e| internal(format!("writing {}: {e}", vcf.display())))?;
    let vcf = vcf.display().to_string();
    super::draft::attach(
        &json!({"account": account, "id": draft.id, "path": vcf}),
        accounts,
    )?;
    Ok(VcardDraft { draft, vcf })
}

// ---------------------------------------------------------------------------
// The rebuild
// ---------------------------------------------------------------------------

/// One rebuild, reported under [`REBUILD_PHASE`] so a batch of five accounts is
/// legible while it runs, and settled with the verdict `save_rebuilt_cache`
/// reached.
///
/// The walk and the write are two blocking tasks with the commit point between
/// them, on this task, so the hold of
/// [`FAKE_COMMIT_HOLD_ENV`](super::super::state::FAKE_COMMIT_HOLD_ENV) awaits
/// the token instead of parking a blocking thread. A shut token at either
/// check returns without a result: the cancel has already settled the
/// operation, and the index it built is dropped here.
async fn rebuild(account: AccountConfig, handle: OperationHandle) {
    handle.set_running();
    handle.report(Progress {
        phase: REBUILD_PHASE.to_string(),
        done: 0,
        total: None,
        message: Some(account.name.clone()),
    });
    let token = handle.token.clone();
    let walked = tokio::task::spawn_blocking(move || {
        build_index_for_account_until(&account, || token.is_cancelled())
            .map(|index| index.map(|index| (account, index)))
    })
    .await;
    let (account, index) = match walked {
        Ok(Ok(Some(built))) => built,
        Ok(Ok(None)) => return,
        Ok(Err(e)) => return handle.fail(DomainError::internal(format!("{e:#}"))),
        Err(e) => return handle.fail(DomainError::internal(format!("the rebuild task {e}"))),
    };
    if !commit_point("contact.rebuild", &handle.token).await {
        return;
    }
    let saved = tokio::task::spawn_blocking(move || save_blocking(&account, &index)).await;
    match saved {
        Ok(Ok(result)) => {
            if result["saved"] == "written" {
                if let (Some(account), Some(contacts)) =
                    (result["account"].as_str(), result["contacts"].as_u64())
                {
                    handle.announce(contact_changed(account, contacts as usize));
                }
            }
            handle.succeed(result)
        }
        Ok(Err(e)) => handle.fail(DomainError::internal(format!("{e:#}"))),
        Err(e) => handle.fail(DomainError::internal(format!("the rebuild task {e}"))),
    }
}

/// The blocking commit: offer the ranked index to the cache.
fn save_blocking(account: &AccountConfig, index: &ContactIndex) -> anyhow::Result<Value> {
    let root = crate::config::account_dir(&account.name);
    let contacts = index.contacts.len();
    let (saved, kept) = match save_rebuilt_cache(&root, index)? {
        CacheSave::Written => ("written", 0),
        CacheSave::RefusedEmpty { kept } => ("refused_empty", kept),
        CacheSave::RefusedShrunk { kept, .. } => ("refused_shrunk", kept),
    };
    Ok(json!({
        "account": account.name,
        "contacts": contacts,
        "kept": kept,
        "saved": saved,
        "cache_path": cache_path(&root).display().to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::super::super::dispatch::CancelScope;
    use super::*;

    /// A build a search ran because there was no index, refused by the cache
    /// guard because another writer's index appeared meanwhile, answers the
    /// kept index and says so; a written one says how big it is.
    #[test]
    fn a_refused_implicit_build_serves_the_kept_index_and_says_so() {
        use crate::contacts::{save_cache, ContactIndex};
        let dir = tempfile::tempdir().expect("tempdir");
        let account = AccountConfig {
            name: "work".to_string(),
            ..Default::default()
        };
        let index = |n: usize| {
            let mut index = ContactIndex {
                account: "work".to_string(),
                contacts: Default::default(),
                built_at: String::new(),
            };
            for i in 0..n {
                let address = format!("c{i}@example.com");
                index.contacts.insert(
                    address.clone(),
                    Contact {
                        address,
                        display_name: String::new(),
                        sent_to: 1,
                        sent_cc: 0,
                        received: 0,
                        first_seen: String::new(),
                        last_seen: String::new(),
                        source: ContactSource::default(),
                    },
                );
            }
            index
        };
        save_cache(dir.path(), &index(10)).expect("another writer's index");
        let loaded = settle_built(dir.path(), &account, index(1)).expect("settled");
        assert_eq!(loaded.index.contacts.len(), 10, "the kept index answers");
        assert_eq!(loaded.written, None);
        assert_eq!(
            loaded.refused,
            Some(RefusedRebuild {
                saved: "refused_shrunk".to_string(),
                contacts: 1,
                kept: 10,
            })
        );

        let fresh = tempfile::tempdir().expect("tempdir");
        let written = settle_built(fresh.path(), &account, index(3)).expect("settled");
        assert_eq!(written.written, Some(3));
        assert_eq!(written.refused, None);
    }

    /// The array is the family, in method-name order, and every one of it is
    /// durable: a cache nobody chose is what an abandoned rebuild leaves.
    #[test]
    fn the_family_declares_three_durable_methods_in_name_order() {
        let names: Vec<&str> = CONTACT_METHOD_SPECS.iter().map(|spec| spec.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for spec in CONTACT_METHOD_SPECS {
            assert_eq!(spec.cancel_scope, CancelScope::Durable, "{}", spec.name);
            assert_eq!(spec.since, 1, "{}", spec.name);
        }
    }
}
