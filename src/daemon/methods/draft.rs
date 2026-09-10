//! The `draft.*` family: the nine methods behind the draft slice (P4-U6).
//!
//! `mp new`, `mp list`, `mp validate`, `mp mark-approved`, `mp mark-draft`,
//! `mp path`, `mp edit`, `mp reply [--all]`, `mp forward` and the bare-selector
//! dry run all answer from here, byte-identically to the pre-daemon binary,
//! which `tests/daemon_draft_slice.rs` is the gate on.
//!
//! # One resolver, and mutators that take an id
//!
//! `draft.path` is the family's resolver: it takes what the user typed
//! (`<id>`, `drafts/<id>`, `mp://<account>/drafts/<id>`) and answers the
//! canonical selector, the canonical path and the current status. Which
//! *account* a selector names stays a client-side decision, exactly as in the
//! read slice, because `Selector::parse` needs no store.
//!
//! It answers from a **fresh directory scan**
//! ([`crate::store::drafts::index_dir`]), not from the watcher's settled
//! inventory. The pre-daemon binary rebuilt the drafts index at the start of
//! every command, so `mp new … && mp mark-approved …`, and an agent writing a
//! file that the next `mp list` shows, both work with no delay; a resolution
//! that waited for a poll plus a debounce would answer "no such draft" for up
//! to a second. The scan costs one `stat` plus one parse per file of one
//! directory, and takes neither an engine lock nor a store.
//!
//! The watcher stays the *fallback* of the two mutators, because a file that
//! will not parse has no `id:` and is announced under its stem: that is the id
//! `draft.approve` refuses with `-32010` `draft_invalid` (P3b-U10), and a scan
//! reports such a file as skipped rather than as a row.
//!
//! # Paths
//!
//! Every result is path-free except the fields whose whole point is a path, and
//! each of those is absolute and under `<data>/accounts/<account>/drafts/`. The
//! store, the blobs and the runtime directory stay invisible.
//!
//! # Refusals
//!
//! The sentence a user sees may not depend on which process did the looking, so
//! a refusal here is worded the way the command has always worded it:
//! [`crate::selector::draft_not_found`] for an id nothing resolves to, and the
//! `mark_as_approved` / `mark_as_draft` sentences for a draft that has already
//! been sent, carrying `{account, id, status}` so a client can tell the case
//! apart without reading English.

use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::draft::{
    DraftCollision, DraftCreated, DraftEntry, DraftListing, DraftLocation, DraftPreview,
    DraftReport, DraftSkip, DraftSource, DraftValidation,
};
use mp_protocol::events::{DraftInvalid, KIND_DRAFT_INVALID};
use mp_protocol::{ErrorCode, RpcError};

use crate::config::{AccountConfig, EmailSettings};
use crate::selector::{Namespace, Selector};
use crate::store::drafts::DraftRow;
use crate::store::read::MessageRow;
use crate::store::{BlobStore, Store};

use crate::daemon::config::ConfigStore;
use crate::daemon::watch::{diagnostics_of, DraftWatch};

use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};
use super::{internal, invalid_params, string_param};

/// The ten methods of the family, in method-name order.
///
/// Four queries read the directory and six commands write or remove one. All
/// ten are durable: a draft written half way because its caller hung up is
/// exactly what this family must never produce, and none of them runs long
/// enough to be worth cancelling.
pub const DRAFT_METHOD_SPECS: [MethodSpec; 10] = [
    MethodSpec::new("draft.approve", MethodKind::Command, 1),
    MethodSpec::new("draft.create", MethodKind::Command, 1),
    MethodSpec::new("draft.demote", MethodKind::Command, 1),
    MethodSpec::new("draft.discard", MethodKind::Command, 1),
    MethodSpec::new("draft.forward", MethodKind::Command, 1),
    MethodSpec::new("draft.list", MethodKind::Query, 1),
    MethodSpec::new("draft.path", MethodKind::Query, 1),
    MethodSpec::new("draft.preview", MethodKind::Query, 1),
    MethodSpec::new("draft.reply", MethodKind::Command, 1),
    MethodSpec::new("draft.validate", MethodKind::Query, 1),
];

/// One of the ten, selected by its own [`MethodSpec`].
///
/// One type for the family because they share every dependency and differ only
/// in which body below they run; the dispatcher registers ten instances, so
/// each still declares itself separately.
pub struct DraftMethod {
    /// Which of [`DRAFT_METHOD_SPECS`] this instance serves.
    pub spec: MethodSpec,
    /// The live configuration, so a reload decides which accounts exist.
    pub config: Arc<ConfigStore>,
    /// The settled inventory an unparseable draft resolves through.
    pub watch: Arc<DraftWatch>,
    /// The state whose revision a command reports.
    pub canonical: Arc<crate::daemon::state::CanonicalState>,
}

/// Register the family from the one array that declares it.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    watch: Arc<DraftWatch>,
    canonical: Arc<crate::daemon::state::CanonicalState>,
) {
    for spec in DRAFT_METHOD_SPECS {
        dispatcher.register(Arc::new(DraftMethod {
            spec,
            config: Arc::clone(&config),
            watch: Arc::clone(&watch),
            canonical: Arc::clone(&canonical),
        }));
    }
}

impl Method for DraftMethod {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let snapshot = self.config.snapshot();
            let accounts = snapshot.accounts.as_slice();
            let email = &snapshot.config.email;
            let result = match self.spec.name {
                "draft.approve" => self.set_status(&params, accounts, true),
                "draft.create" => create(&params, accounts, email),
                "draft.demote" => self.set_status(&params, accounts, false),
                "draft.discard" => discard(&params, accounts),
                "draft.forward" => from_source(&params, accounts, email, false),
                "draft.list" => list(&params, accounts),
                "draft.path" => path(&params, accounts),
                "draft.preview" => preview(&params, accounts, email),
                "draft.reply" => from_source(&params, accounts, email, true),
                _ => validate(&params, accounts),
            };
            let result = result.map_err(DomainError::from)?;
            if self.spec.kind == MethodKind::Query {
                return Ok(Outcome::query(result));
            }
            // Every command of this family answers about one draft and carries
            // its id, which is the resource a client's cached row is keyed by.
            // The one exception is the `draft.discard` sweep, which answers
            // about a whole account and names no draft: it invalidates the
            // account's drafts rather than one row.
            let account = result["account"].as_str().unwrap_or_default();
            let resource = match result["id"].as_str() {
                Some(id) => format!("draft:{account}/{id}"),
                None => format!("draft:{account}"),
            };
            Ok(Outcome::command(
                result,
                self.canonical.revision().get(),
                vec![ResourceId::new(resource)],
            ))
        })
    }
}

impl DraftMethod {
    /// `draft.approve` and `draft.demote`: the same surgical rewrite of one
    /// line, in the two directions.
    fn set_status(
        &self,
        params: &Value,
        accounts: &[AccountConfig],
        approve: bool,
    ) -> Result<Value, RpcError> {
        let account = configured(accounts, &string_param(params, "account")?)?;
        let name = account.name.clone();
        let id = string_param(params, "id")?;

        // The scan first, so a draft written a millisecond ago is addressable;
        // the watcher second, because a file that will not parse has no `id:`
        // and is announced under its stem, which is the id the refusal below is
        // about.
        let path = match row_by_id(&name, &id) {
            Some(row) => row.path,
            None => self
                .watch
                .resolve(&name, &id)
                .ok_or_else(|| not_found(&name, &id))?,
        };

        let draft = crate::draft::parse_email_draft(&path)
            .map_err(|_| refuse_unparseable(&name, &id, &path))?;
        let status = draft.frontmatter.status.to_string();
        if status == "sent" {
            return Err(RpcError {
                code: super::INVALID_PARAMS,
                message: if approve {
                    "Cannot approve an already sent email".to_string()
                } else {
                    "Cannot revert a sent email back to draft".to_string()
                },
                data: Some(json!({"account": name, "id": id, "status": status})),
            });
        }
        if approve {
            crate::draft::mark_as_approved(&path)
        } else {
            crate::draft::mark_as_draft(&path)
        }
        .map_err(|e| internal(format!("rewriting {}: {e:#}", path.display())))?;

        Ok(json!({
            "account": name,
            "id": id,
            "status": if approve { "approved" } else { "draft" },
            "path": path.display().to_string(),
        }))
    }
}

// ---------------------------------------------------------------------------
// The queries
// ---------------------------------------------------------------------------

/// The `result` of `draft.list`: the index projection of one directory, plus
/// what the scan could not make sense of.
///
/// A broken file is a warning about the directory rather than a failure of the
/// command, so it travels beside the rows instead of replacing them (#0080).
fn list(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let account = configured(accounts, &string_param(params, "account")?)?;
    let status = match params.get("status") {
        None | Some(Value::Null) => None,
        Some(Value::String(status)) if matches!(status.as_str(), "draft" | "approved" | "sent") => {
            Some(status.clone())
        }
        Some(other) => {
            return Err(invalid_params(format!(
                "status {other} is none of \"draft\", \"approved\", \"sent\""
            )))
        }
    };

    let (rows, collisions, skipped) =
        crate::store::drafts::index_dir(&crate::config::drafts_dir(&account.name));
    let listing = DraftListing {
        account: account.name.clone(),
        drafts: rows
            .iter()
            .filter(|row| status.as_deref().is_none_or(|status| row.status == status))
            .map(|row| entry(&account.name, row))
            .collect(),
        skipped: skipped
            .iter()
            .map(|skip| DraftSkip {
                path: skip.path.display().to_string(),
                error: skip.error.clone(),
            })
            .collect(),
        collisions: collisions
            .iter()
            .map(|collision| DraftCollision {
                id: collision.id.clone(),
                kept: collision.kept.display().to_string(),
                shadowed: collision.shadowed.display().to_string(),
            })
            .collect(),
    };
    to_value("draft.list", &listing)
}

/// One listed row.
///
/// `valid` is about the file and `ready` is about whether it would send, which
/// is the same split the watcher publishes: a draft with no subject parses
/// perfectly and would not go out.
///
/// `subject` is the frontmatter's own field rather than the index's, which
/// drops an empty one: an empty subject stays an empty subject here, and the
/// client decides that a blank one prints no line.
fn entry(account: &str, row: &DraftRow) -> DraftEntry {
    let draft = crate::draft::parse_email_draft(&row.path);
    DraftEntry {
        id: row.id.clone(),
        selector: Selector::for_draft(account, &row.id).to_string(),
        path: row.path.display().to_string(),
        status: row.status.clone(),
        to: row.to.clone(),
        subject: draft
            .as_ref()
            .ok()
            .map(|draft| draft.frontmatter.subject.clone()),
        valid: draft.is_ok(),
        ready: draft
            .as_ref()
            .is_ok_and(|draft| crate::draft::validate_draft(draft).is_ok()),
    }
}

/// The `result` of `draft.validate`: one report per draft, in listing order, or
/// one for the draft a selector names.
///
/// The daemon refuses nothing about the drafts themselves: an invalid draft is
/// an answer, and the exit code stays the client's decision.
fn validate(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let account = configured(accounts, &string_param(params, "account")?)?;
    let rows = match addressed(params, &account.name)? {
        Some(key) => vec![resolve(&account.name, &key)?],
        None => crate::store::drafts::index_dir(&crate::config::drafts_dir(&account.name)).0,
    };
    let reports: Vec<DraftReport> = rows.iter().map(|row| report(&account.name, row)).collect();
    to_value(
        "draft.validate",
        &DraftValidation {
            account: account.name.clone(),
            reports,
        },
    )
}

/// One draft's diagnostics, in the two fields `mp validate` prints: the single
/// line after the dash, and the warnings it joins with `", "`.
fn report(account: &str, row: &DraftRow) -> DraftReport {
    let outcome = crate::draft::parse_email_draft(&row.path)
        .and_then(|draft| crate::draft::validate_draft(&draft));
    DraftReport {
        id: row.id.clone(),
        selector: Selector::for_draft(account, &row.id).to_string(),
        valid: outcome.is_ok(),
        error: outcome.as_ref().err().map(|e| e.to_string()),
        warnings: outcome.unwrap_or_default(),
    }
}

/// The `result` of `draft.path`: the family's resolver.
fn path(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let account = configured(accounts, &string_param(params, "account")?)?;
    let row = resolve(&account.name, &addressed_one(params, &account.name)?)?;
    to_value(
        "draft.path",
        &DraftLocation {
            account: account.name.clone(),
            id: row.id.clone(),
            selector: Selector::for_draft(&account.name, &row.id).to_string(),
            path: row.path.display().to_string(),
            status: row.status.clone(),
        },
    )
}

/// The `result` of `draft.preview`: the record the dry run renders, field for
/// field, including the two cut-offs of [`crate::draft::preview_draft`] that a
/// client re-implementing them would get wrong.
fn preview(
    params: &Value,
    accounts: &[AccountConfig],
    email: &EmailSettings,
) -> Result<Value, RpcError> {
    let account = configured(accounts, &string_param(params, "account")?)?;
    let row = resolve(&account.name, &addressed_one(params, &account.name)?)?;
    let draft = crate::draft::parse_email_draft(&row.path)
        .map_err(|_| refuse_unparseable(&account.name, &row.id, &row.path))?;
    let outcome = crate::draft::validate_draft(&draft);

    to_value(
        "draft.preview",
        &DraftPreview {
            account: account.name.clone(),
            id: row.id.clone(),
            selector: Selector::for_draft(&account.name, &row.id).to_string(),
            path: row.path.display().to_string(),
            from: draft
                .frontmatter
                .from
                .clone()
                .unwrap_or_else(|| account.default_from.clone()),
            to: draft.frontmatter.to.clone(),
            cc: draft.frontmatter.cc.clone(),
            bcc: draft.frontmatter.bcc.clone(),
            subject: draft.frontmatter.subject.clone(),
            // 500 characters for the text and 500 bytes for the `...` line:
            // two rules, both the renderer's, decided here so that no client
            // has to re-derive either.
            body: draft.body_markdown.chars().take(500).collect(),
            body_truncated: draft.body_markdown.len() > 500,
            status: draft.frontmatter.status.to_string(),
            valid: outcome.is_ok(),
            error: outcome.as_ref().err().map(|e| e.to_string()),
            warnings: outcome.unwrap_or_default(),
            font_family: email.font_family.clone(),
            font_size: email.font_size.clone(),
            // Always absent for the CLI dry run: the body already carries the
            // signature (#0099).
            signature: None,
        },
    )
}

// ---------------------------------------------------------------------------
// The writers
// ---------------------------------------------------------------------------

/// The `result` of `draft.create`: a skeleton in the account's drafts
/// directory, with the id already in the file.
///
/// The `.md` suffixing rule is the daemon's, because the daemon owns the
/// directory the file lands in: `mp new note` writes `note.md` and
/// `mp new note.txt` writes `note.txt`.
fn create(
    params: &Value,
    accounts: &[AccountConfig],
    email: &EmailSettings,
) -> Result<Value, RpcError> {
    let account = configured(accounts, &string_param(params, "account")?)?;
    let name = string_param(params, "name")?;
    let file_name = match Path::new(&name).extension() {
        Some(_) => name.clone(),
        None => format!("{name}.md"),
    };

    let dir = crate::config::drafts_dir(&account.name);
    std::fs::create_dir_all(&dir)
        .map_err(|e| internal(format!("creating {}: {e}", dir.display())))?;
    let path = dir.join(&file_name);
    if path.exists() {
        return Err(RpcError {
            code: super::INVALID_PARAMS,
            message: format!("A draft already exists at {}", path.display()),
            data: Some(json!({
                "account": account.name,
                "name": name,
                "path": path.display().to_string(),
            })),
        });
    }

    // The id is minted here rather than by a later index pass, so the selector
    // handed out is the one in the file from its first byte.
    let id = crate::store::drafts::new_id();
    let skeleton = crate::draft::new_draft_skeleton_with_id(
        &account.default_from,
        &chrono::Utc::now().to_rfc2822(),
        &id,
        signature_of(account, params, email).as_deref(),
    );
    std::fs::write(&path, skeleton)
        .map_err(|e| internal(format!("writing {}: {e}", path.display())))?;

    created(&account.name, &id, &path, None)
}

/// The `result` of `draft.reply` and `draft.forward`: a draft built from a
/// stored message, naming the message it answers.
///
/// The source is addressed the way the read slice addresses one, and the
/// builder is [`crate::draft::create_draft_from_source`], so the file is the
/// one the CLI wrote before this: the same subject prefixing, the same recorded
/// `in_reply_to`, the same carried attachments.
fn from_source(
    params: &Value,
    accounts: &[AccountConfig],
    email: &EmailSettings,
    reply: bool,
) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?;
    let source = params.get("source").cloned().unwrap_or(Value::Null);

    let store = Store::open(crate::config::store_path(&name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let (row, selector) = address_received(&source, &store, &name)?;
    let blobs = BlobStore::new(crate::config::blobs_dir(&name));
    // A forward carries the original attachments, which is the property #0006
    // exists for; a reply quotes and carries none.
    let built = crate::draft::source_from_row(&store, &blobs, &row, !reply)
        .map_err(|e| internal(format!("reading {name}:{}/{}: {e:#}", row.mailbox, row.uid)))?;
    drop(store);

    let kind = if reply {
        crate::draft::DraftFromSource::Reply {
            all: flag(params, "all")?,
        }
    } else {
        crate::draft::DraftFromSource::Forward
    };
    let (path, written) = crate::draft::create_draft_from_source(
        &name,
        &account.default_from,
        &built,
        kind,
        None,
        signature_of(account, params, email).as_deref(),
    )
    .map_err(|e| internal(format!("writing the draft of {name}: {e:#}")))?;

    created(
        &name,
        &written.key,
        &path,
        Some(DraftSource {
            id: format!("{}/{}", row.mailbox, row.uid),
            selector: selector.to_string(),
        }),
    )
}

/// The answer of the three methods that write a new draft.
fn created(
    account: &str,
    id: &str,
    path: &Path,
    source: Option<DraftSource>,
) -> Result<Value, RpcError> {
    to_value(
        "a created draft",
        &DraftCreated {
            account: account.to_string(),
            id: id.to_string(),
            selector: Selector::for_draft(account, id).to_string(),
            path: path.display().to_string(),
            source,
        },
    )
}

/// The `result` of `draft.discard`: one draft removed, or the `--sent` sweep.
///
/// A draft is local, so there is no server op and no backend: just the file and
/// the index row. `force` is required for an `approved` draft and for nothing
/// else, because an approved draft is a queued send and deleting it drops that
/// send; a `sent` draft needs none, since retiring one is the whole point of
/// the sweep. Both refusals are
/// [`crate::draft::delete_indexed_draft`]'s own sentences, verbatim.
fn discard(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let account = configured(accounts, &string_param(params, "account")?)?;
    let name = account.name.clone();

    // The sweep is a parameter of this method rather than a method of its own:
    // one verb over a set is the same verb. It names no draft, so a caller who
    // named one disagrees with themselves.
    if flag(params, "sent")? {
        for key in ["id", "selector", "force"] {
            if !matches!(params.get(key), None | Some(Value::Null)) {
                return Err(invalid_params(format!(
                    "a sent sweep clears every sent draft of an account and names none; \
                     {key} and sent are two different calls"
                )));
            }
        }
        return sweep(&name);
    }

    let force = flag(params, "force")?;
    let row = resolve(&name, &addressed_one(params, &name)?)?;
    let store = drafts_store(&name)?;
    crate::draft::delete_indexed_draft(&store, &name, &row, force)
        .map_err(|e| invalid_params(format!("{e:#}")))?;
    reindex(&store, &name);
    Ok(json!({
        "account": name,
        "id": row.id,
        "selector": Selector::for_draft(&name, &row.id).to_string(),
        // The status it was in, which is what was discarded: a client prints
        // one line either way, and a caller that cared about the difference
        // (an approved draft is a queued send) learns it from the answer.
        "status": row.status,
    }))
}

/// Every `sent` draft of one account: what went, and what stayed.
///
/// The sweep keeps going past a file it cannot remove, because one unremovable
/// draft is not a reason to leave the other nine behind; the survivors travel
/// with their own error so the client can print one line each before its
/// summary.
fn sweep(account: &str) -> Result<Value, RpcError> {
    let store = drafts_store(account)?;
    // The scan first, so a draft written a millisecond ago is swept, and
    // through the index so the walk keeps the `mtime DESC, id ASC` order
    // `mp delete --sent` has always walked in.
    reindex(&store, account);
    let rows = crate::store::drafts::list(&store, account, Some("sent"))
        .map_err(|e| internal(format!("listing the sent drafts of {account}: {e:#}")))?;

    let mut cleared = 0u64;
    let mut kept = Vec::new();
    for row in &rows {
        match crate::draft::delete_indexed_draft(&store, account, row, false) {
            Ok(()) => cleared += 1,
            Err(e) => kept.push(json!({
                "id": row.id,
                "selector": Selector::for_draft(account, &row.id).to_string(),
                "error": format!("{e:#}"),
            })),
        }
    }
    reindex(&store, account);
    Ok(json!({"account": account, "cleared": cleared, "kept": kept}))
}

/// The account's store, for the two things a discard needs one for: the
/// mid-send check [`crate::draft::delete_indexed_draft`] makes against the
/// outbox, and the index the removal has to leave consistent.
fn drafts_store(account: &str) -> Result<Store, RpcError> {
    Store::open(crate::config::store_path(account))
        .map_err(|e| internal(format!("opening the store of {account}: {e:#}")))
}

/// Re-index the drafts directory, best-effort.
///
/// The removal already happened and the watcher would notice on its own; this
/// is what keeps the next reader that goes through the index - `mp send`, the
/// TUI - from resolving a row whose file is gone.
fn reindex(store: &Store, account: &str) {
    let dir = crate::config::drafts_dir(account);
    if let Err(e) = crate::store::drafts::refresh(store, account, &dir) {
        log::warn!("[draft] could not refresh the drafts index of {account}: {e:#}");
    }
}

// ---------------------------------------------------------------------------
// Addressing
// ---------------------------------------------------------------------------

/// The configured account behind `name`, or `-32005` naming what was asked for.
///
/// Not [`super::account::ready_account`]: a drafts directory is local truth and
/// an account that has never synced still has one, so `mp list` answers about
/// it rather than refusing.
fn configured<'a>(
    accounts: &'a [AccountConfig],
    name: &str,
) -> Result<&'a AccountConfig, RpcError> {
    accounts
        .iter()
        .find(|account| account.name == name)
        .ok_or_else(|| RpcError {
            code: ErrorCode::AccountUnknown.code(),
            message: format!("no account named {name} is configured"),
            data: Some(json!({ "account": name })),
        })
}

/// The draft key the caller addressed, or `None` when it addressed none.
///
/// `id` is the key verbatim, `selector` is the grammar a user types, and both
/// at once is a caller who may disagree with themselves.
pub(super) fn addressed(params: &Value, account: &str) -> Result<Option<String>, RpcError> {
    let given = |key: &str| !matches!(params.get(key), None | Some(Value::Null));
    match (given("id"), given("selector")) {
        (true, true) => Err(invalid_params(
            "id and selector are two addresses; send exactly one",
        )),
        (false, false) => Ok(None),
        (true, false) => string_param(params, "id").map(Some),
        (false, true) => {
            let selector = string_param(params, "selector")?;
            crate::selector::parse_in(&selector, Namespace::Drafts, account, None)
                .map(|query| Some(query.key))
                .map_err(|e| invalid_params(format!("{e:#}")))
        }
    }
}

/// [`addressed`], for the two methods that address exactly one draft.
pub(super) fn addressed_one(params: &Value, account: &str) -> Result<String, RpcError> {
    addressed(params, account)?
        .ok_or_else(|| invalid_params("a draft is addressed by id or by selector; send one"))
}

/// The received message a reply or a forward is built from, and its canonical
/// selector.
///
/// `{id}` is `"<mailbox>/<uid>"`, the store's own key; `{selector, mailbox?}`
/// is the grammar `mp reply` takes from a user, resolved here because resolving
/// one needs the store the client no longer has. Neither is a caller who
/// forgot, both is a caller who may disagree with themselves, and every way of
/// naming nothing is the caller's parameter being wrong rather than the store
/// failing.
fn address_received(
    source: &Value,
    store: &Store,
    account: &str,
) -> Result<(MessageRow, Selector), RpcError> {
    let given = |key: &str| !matches!(source.get(key), None | Some(Value::Null));
    match (given("id"), given("selector")) {
        (true, true) => Err(invalid_params(
            "id and selector are two addresses; send exactly one",
        )),
        (false, false) => Err(invalid_params(
            "a message is addressed by id or by selector; send exactly one",
        )),
        (true, false) => {
            let id = string_param(source, "id")?;
            let malformed =
                || invalid_params(format!("id {id:?} is not a \"<mailbox>/<uid>\" message id"));
            let (mailbox, uid) = id.rsplit_once('/').ok_or_else(malformed)?;
            let uid: i64 = uid.parse().map_err(|_| malformed())?;
            let missing = || invalid_params(format!("{account} holds no message {mailbox}/{uid}"));
            let row = crate::store::read::find_row_by_uid(store, account, mailbox, uid)
                .map_err(|e| internal(format!("looking up {account}:{mailbox}/{uid}: {e:#}")))?
                .ok_or_else(missing)?;
            let row = crate::store::read::find_by_id(store, row)
                .map_err(|e| internal(format!("reading message {row}: {e:#}")))?
                .ok_or_else(missing)?;
            let selector = Selector::for_message(account, &row);
            Ok((row, selector))
        }
        (false, true) => {
            let selector = string_param(source, "selector")?;
            let mailbox = source.get("mailbox").and_then(Value::as_str);
            let query = crate::selector::parse_in(&selector, Namespace::Received, account, mailbox)
                .map_err(|e| invalid_params(format!("{e:#}")))?;
            crate::selector::resolve_received(store, &query)
                .map_err(|e| invalid_params(format!("{e:#}")))
        }
    }
}

/// The row `key` names, from a scan of the account's drafts directory.
pub(super) fn resolve(account: &str, key: &str) -> Result<DraftRow, RpcError> {
    row_by_id(account, key).ok_or_else(|| not_found(account, key))
}

/// One row of a fresh scan, by id.
fn row_by_id(account: &str, id: &str) -> Option<DraftRow> {
    crate::store::drafts::index_dir(&crate::config::drafts_dir(account))
        .0
        .into_iter()
        .find(|row| row.id == id)
}

/// `-32602` for an id nothing resolves to, worded as the command has always
/// worded it and carrying the two fields a client matches on.
fn not_found(account: &str, id: &str) -> RpcError {
    RpcError {
        code: super::INVALID_PARAMS,
        message: format!("{:#}", crate::selector::draft_not_found(account, id)),
        data: Some(json!({"account": account, "id": id})),
    }
}

/// `-32010`, carrying the diagnostics the watcher would have published: the
/// file is read again here rather than remembered, because the refusal is about
/// the file as it is now.
fn refuse_unparseable(account: &str, id: &str, path: &Path) -> RpcError {
    let payload = DraftInvalid {
        account: account.to_string(),
        id: id.to_string(),
        path: path.display().to_string(),
        diagnostics: diagnostics_of(path),
    };
    let message = payload
        .diagnostics
        .first()
        .map(|diagnostic| diagnostic.message.clone())
        .unwrap_or_else(|| format!("{} will not parse", path.display()));
    RpcError {
        code: ErrorCode::DraftInvalid.code(),
        message,
        data: Some(
            serde_json::to_value(&payload).unwrap_or_else(|_| json!({"kind": KIND_DRAFT_INVALID})),
        ),
    }
}

/// The signature a written draft carries, resolved from the account's
/// configuration exactly as the client resolved it before this slice.
pub(super) fn signature_of(
    account: &AccountConfig,
    params: &Value,
    email: &EmailSettings,
) -> Option<String> {
    crate::config::body_signature(
        account,
        matches!(params.get("no_signature"), Some(Value::Bool(true))),
        params.get("signature").and_then(Value::as_str),
        email,
    )
}

/// An optional boolean parameter, absent and `null` both meaning `false`.
fn flag(params: &Value, name: &str) -> Result<bool, RpcError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(invalid_params(format!("{name} is a boolean"))),
    }
}

/// One typed result on the wire.
fn to_value<T: serde::Serialize>(what: &str, value: &T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| internal(format!("serialising {what}: {e}")))
}
