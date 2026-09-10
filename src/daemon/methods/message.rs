//! `message.list`, and the three methods that turn a stored message into a file
//! a client can open: `message.materialise_attachment`,
//! `message.materialise_html` and `message.release_handle` (P3b-U12).
//!
//! `message.list`: one mailbox of one account, newest first.
//!
//! The rows are [`crate::store::read::list_mailbox`]'s, so the daemon answers
//! from the same query, in the same order (`date_sort DESC, id DESC`), as
//! `mp list-messages` and the TUI list. `date_sort` is
//! [`crate::tui::app::resolve_date`]'s sort key, so all three stacks derive a
//! date the same way rather than each parsing the header again, and
//! `date_display` is the `Date:` header as the store holds it, which is the
//! column a listing prints.
//!
//! `total` is how many messages the mailbox holds and ignores `limit`, which is
//! the "In the store: N" of `mp list-messages`. `limit: null` and an absent
//! `limit` both mean "all"; `0` means none, since `null` already spells "all"
//! and a number may not mean the opposite of itself.

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::RpcError;

use crate::config::AccountConfig;
use crate::selector::DRAFTS_MAILBOX;
use crate::store::read::{self, MessageRow};
use crate::store::{BlobStore, Store};
use crate::tui::app::{build_mailboxes, resolve_date};

use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::super::handles::{
    handle_dir, reap, remove_handle_dir, HandleId, HandleKind, HandleTable,
};
use super::{internal, invalid_params, string_param};

/// `message.list` as the dispatcher serves it.
pub struct MessageList {
    /// The live configuration, so a reload is visible to the next listing.
    pub config: Arc<super::super::config::ConfigStore>,
}

impl Method for MessageList {
    fn spec(&self) -> MethodSpec {
        MethodSpec::new("message.list", MethodKind::Query, 1)
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        // The store read is synchronous, as it was when the server called this
        // method directly. Moving it onto a blocking thread is a change to how
        // the daemon schedules work, not to how it dispatches, so it belongs
        // with the account runtimes of Phase 5.
        Box::pin(async move {
            list(&params, &self.config.accounts())
                .map(Outcome::query)
                .map_err(DomainError::from)
        })
    }
}

/// The `result` of `message.list`.
pub fn list(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let wanted = string_param(params, "mailbox")?;
    let limit = limit_param(params)?;

    let account = super::account::ready_account(accounts, &name)?;
    let mailbox = resolve_mailbox(account, &wanted)?;

    let path = crate::config::store_path(&name);
    let store =
        Store::open(&path).map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let rows = read::list_mailbox(&store, &name, &mailbox)
        .map_err(|e| internal(format!("listing {name}/{mailbox}: {e:#}")))?;

    let total = rows.len();
    let messages: Vec<Value> = rows
        .iter()
        .take(limit.unwrap_or(total))
        .map(to_json)
        .collect();
    Ok(json!({
        "account": name,
        "mailbox": mailbox,
        "total": total,
        "messages": messages,
    }))
}

/// One stored row on the wire.
///
/// The three nullable headers travel as `""` rather than `null`, because the
/// shape says `str`. `flags` carries the three axes the protocol names and not
/// the store's fourth (`\Flagged`): a client that needs the star waits for the
/// version that adds it.
///
/// Both dates are here because neither can be derived from the other:
/// `date_sort` is `resolve_date`'s UTC sort key, and `date_display` is the
/// `Date:` header as the store holds it, which is the column a listing prints.
/// A client renders from the wire alone rather than reading the store beside
/// the daemon.
pub fn to_json(row: &MessageRow) -> Value {
    let (_display, date_sort) = resolve_date(&row.date_display, &None, Path::new(""));
    let flags = row.flags();
    json!({
        "uid": row.uid,
        "message_id": row.message_id,
        "from": row.from.clone().unwrap_or_default(),
        "subject": row.subject.clone().unwrap_or_default(),
        "date_sort": date_sort,
        "date_display": row.date_display.clone().unwrap_or_default(),
        "flags": {
            "seen": flags.seen,
            "answered": flags.answered,
            "forwarded": flags.forwarded,
        },
        "has_attachments": row.has_attachments,
    })
}

// ---------------------------------------------------------------------------
// Materialised handles
// ---------------------------------------------------------------------------

/// The three handle methods, in method-name order.
///
/// The two materialisers are `ClientIntegration`, because the daemon prepares
/// the file and only the client's own process can open it; the release is a
/// `Query`, because it moves no revision and invalidates no resource, handles
/// being per-client scratch that appears in no snapshot. All three are
/// `Durable`: a viewer holding an open file may not lose it because the socket
/// that asked for it went away, and expiry is what ends a handle.
pub const MESSAGE_HANDLE_METHOD_SPECS: [MethodSpec; 3] = [
    MethodSpec::new(
        "message.materialise_attachment",
        MethodKind::ClientIntegration,
        1,
    ),
    MethodSpec::new("message.materialise_html", MethodKind::ClientIntegration, 1),
    MethodSpec::new("message.release_handle", MethodKind::Query, 1),
];

/// One of the three, selected by its own [`MethodSpec`].
///
/// One type for three methods because they share every dependency and differ
/// only in which of the three bodies below they run; the dispatcher registers
/// three instances, so each still declares itself separately.
pub struct MessageHandleMethod {
    /// Which of [`MESSAGE_HANDLE_METHOD_SPECS`] this instance serves.
    pub spec: MethodSpec,
    /// The live configuration, so a reload is visible to the next call.
    pub config: Arc<super::super::config::ConfigStore>,
    /// The daemon's one handle table, shared with the sweep that reads its pins.
    pub handles: Arc<HandleTable>,
}

impl Method for MessageHandleMethod {
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
            // The lazy reaper: every call collects what expired since the last
            // one, so an expired handle is unreleasable and its scratch is gone
            // without a periodic tick to be late.
            reap(&self.handles, Utc::now());
            let result = match self.spec.name {
                "message.materialise_attachment" => materialise(
                    &params,
                    &self.config.accounts(),
                    &self.handles,
                    HandleKind::Attachment,
                ),
                "message.materialise_html" => materialise(
                    &params,
                    &self.config.accounts(),
                    &self.handles,
                    HandleKind::Html,
                ),
                _ => release_handle(&params, &self.handles),
            };
            result.map(Outcome::query).map_err(DomainError::from)
        })
    }
}

/// Register the three handle methods on `dispatcher`.
pub fn register_handles(
    dispatcher: &mut Dispatcher,
    config: Arc<super::super::config::ConfigStore>,
    handles: Arc<HandleTable>,
) {
    for spec in MESSAGE_HANDLE_METHOD_SPECS {
        dispatcher.register(Arc::new(MessageHandleMethod {
            spec,
            config: Arc::clone(&config),
            handles: Arc::clone(&handles),
        }));
    }
}

/// The `result` of the two materialisers: `{handle, path, bytes, expires_at}`.
fn materialise(
    params: &Value,
    accounts: &[AccountConfig],
    handles: &HandleTable,
    kind: HandleKind,
) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    super::account::ready_account(accounts, &name)?;
    let (mailbox, uid) = message_param(params)?;

    // The store is opened by path, as `message.list` opens it, and no engine
    // lock is taken: materialising is a read plus a write into the daemon's own
    // runtime directory, neither of which makes the daemon an account's engine.
    let store = Store::open(crate::config::store_path(&name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let blobs = BlobStore::new(crate::config::blobs_dir(&name));
    let row = read::find_row_by_uid(&store, &name, &mailbox, uid)
        .map_err(|e| internal(format!("looking up {name}:{mailbox}/{uid}: {e:#}")))?
        .ok_or_else(|| invalid_params(format!("{name} holds no message {mailbox}/{uid}")))?;

    let (filename, bytes, pinned) = match kind {
        HandleKind::Attachment => attachment_file(params, &store, &blobs, row)?,
        HandleKind::Html => html_file(&store, &blobs, row)?,
    };
    write_handle(handles, kind, &name, &filename, &bytes, &pinned)
}

/// `id`, which is `"<mailbox>/<uid>"`.
///
/// The last two thirds of the `message:<account>/<mailbox>/<uid>` resource, and
/// the store's own `UNIQUE (account, mailbox, uid)` key: a client composes it
/// from the mailbox it listed and the `uid` of the row it is holding. The row id
/// was the other candidate and is a rebuild away from meaning another message;
/// the `Message-ID` header was the third and is shared by the Inbox and Sent
/// copies of one message.
fn message_param(params: &Value) -> Result<(String, i64), RpcError> {
    let id = string_param(params, "id")?;
    let malformed = || invalid_params(format!("id {id:?} is not a \"<mailbox>/<uid>\" message id"));
    let (mailbox, uid) = id.rsplit_once('/').ok_or_else(malformed)?;
    let uid: i64 = uid.parse().map_err(|_| malformed())?;
    if mailbox.is_empty() {
        return Err(malformed());
    }
    Ok((mailbox.to_string(), uid))
}

/// The name, the bytes and the pinned blobs of one attachment.
///
/// `part` is a dense zero-based index into the message's user-facing attachment
/// list, which is [`read::attachments_for`]'s order with the iMIP sidecar
/// excluded: it is the index of the row a client is looking at, and addressing
/// by the store's raw `ordinal` would leak the hidden sidecar's position into a
/// list it is deliberately absent from.
fn attachment_file(
    params: &Value,
    store: &Store,
    blobs: &BlobStore,
    row: i64,
) -> Result<(String, Vec<u8>, Vec<String>), RpcError> {
    let part = params
        .get("part")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_params("part is a required non-negative integer"))?;
    let attachments =
        read::attachments_for(store, row).map_err(|e| internal(format!("reading {row}: {e:#}")))?;
    let attachment = usize::try_from(part)
        .ok()
        .and_then(|part| attachments.get(part))
        .ok_or_else(|| {
            invalid_params(format!(
                "part {part} is not one of this message's {} attachments",
                attachments.len()
            ))
        })?;
    let bytes = read::read_blob(blobs, row, &attachment.hash).ok_or_else(|| {
        internal(format!(
            "the blob behind attachment {} is missing or unreadable",
            attachment.name
        ))
    })?;
    // The stored name is sanitised again here rather than trusted, the same rule
    // `read::materialise_attachments` applies: this is the seam that turns a
    // name a sender chose into a path.
    Ok((
        safe_filename(&crate::parse::sanitize_attachment_filename(
            &attachment.name,
        )),
        bytes,
        vec![attachment.hash.clone()],
    ))
}

/// The browser rendition of one message, and the blobs it read.
///
/// The rendition, not the raw markup: the charset and the CSP tag the TUI's `b`
/// binding injects before it hands a `file://` URL to a browser (#0037), and the
/// `cid:` inlining that makes the referenced parts visible without a message to
/// resolve them against. Serving unhardened markup through a new door would undo
/// that fix at the moment the GUI starts using it.
fn html_file(
    store: &Store,
    blobs: &BlobStore,
    row: i64,
) -> Result<(String, Vec<u8>, Vec<String>), RpcError> {
    let markup = read::load_html(store, blobs, row)
        .ok_or_else(|| invalid_params("this message carries no HTML to render"))?;
    let raw_hash = read::blob_hash_of_kind(store, row, "raw");
    // The `html` blob when there is one; otherwise the markup came out of the
    // raw message and it is the raw blob that must stay.
    let mut pinned: Vec<String> = read::blob_hash_of_kind(store, row, "html")
        .or_else(|| raw_hash.clone())
        .into_iter()
        .collect();
    let markup = if markup.to_ascii_lowercase().contains("cid:") {
        match read::load_raw(store, blobs, row) {
            Some(raw) => {
                // The inline-image scan parsed the raw message, so this handle
                // holds two blobs rather than one.
                if let Some(hash) = raw_hash.filter(|hash| !pinned.contains(hash)) {
                    pinned.push(hash);
                }
                let images = crate::parse::inline_images(&raw, &markup);
                crate::parse::embed_inline_images(&markup, &images)
            }
            None => markup,
        }
    } else {
        markup
    };
    let rendition = crate::parse::inject_csp_meta(&crate::parse::ensure_utf8_charset(&markup));
    Ok(("message.html".to_string(), rendition.into_bytes(), pinned))
}

/// A sanitised name that is still a name: `.` and `..` are directory entries
/// every path has, and joining either would leave the handle's own directory.
fn safe_filename(name: &str) -> String {
    match name {
        "." | ".." => "attachment.bin".to_string(),
        other => other.to_string(),
    }
}

/// Write one file into its own handle directory and record the handle.
///
/// The id is minted before the write because it names the directory; a write
/// that fails leaves no entry in the table and no directory on disk.
fn write_handle(
    handles: &HandleTable,
    kind: HandleKind,
    account: &str,
    filename: &str,
    bytes: &[u8],
    pinned: &[String],
) -> Result<Value, RpcError> {
    let id = handles.mint_id();
    let dir = handle_dir(&id);
    let path = dir.join(filename);
    let written = (|| -> anyhow::Result<u64> {
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, Permissions::from_mode(0o700))?;
        fs::write(&path, bytes)?;
        Ok(fs::metadata(&path)?.len())
    })();
    let written = match written {
        Ok(written) => written,
        Err(e) => {
            remove_handle_dir(&id);
            return Err(internal(format!("materialising {}: {e:#}", path.display())));
        }
    };

    let handle =
        handles.materialise_with_id(id, kind, account, path.clone(), written, pinned, Utc::now());
    Ok(json!({
        "handle": handle.id.as_str(),
        "path": path.display().to_string(),
        // The length of the file, not the size of the backing blob: a rendition
        // grew by its CSP tag, and a client preallocating from this reads a file.
        "bytes": handle.bytes,
        "expires_at": handle.expires_at.to_rfc3339(),
    }))
}

/// The `result` of `message.release_handle`, which is `{}`.
///
/// An unknown, an already released and an expired handle are one answer,
/// `-32602`: all three mean "you are not holding that", and the daemon has no
/// reason to tell a client which of the three it did.
fn release_handle(params: &Value, handles: &HandleTable) -> Result<Value, RpcError> {
    let id = HandleId(string_param(params, "handle")?);
    if !handles.release(&id) {
        return Err(invalid_params(format!(
            "{} is not a live handle",
            id.as_str()
        )));
    }
    // Only ever a directory this daemon minted the name of, so the removal
    // cannot be steered by a caller's string.
    remove_handle_dir(&id);
    Ok(json!({}))
}

/// `limit`, which is optional and unsigned; anything else is `-32602`.
fn limit_param(params: &Value) -> Result<Option<usize>, RpcError> {
    match params.get("limit") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(|limit| Some(limit.min(usize::MAX as u64) as usize))
            .ok_or_else(|| {
                invalid_params("limit is a non-negative integer, or null for every message")
            }),
    }
}

/// The mailbox id behind what the caller asked for.
///
/// Role, slug or sidebar label, exactly as `mp list-messages --mailbox` accepts
/// them, and the answer echoes the resolved id rather than the spelling. An
/// unknown one is `-32602` naming the mailboxes this account has, because the
/// caller asked for something that does not exist rather than for something the
/// daemon refuses.
fn resolve_mailbox(account: &AccountConfig, wanted: &str) -> Result<String, RpcError> {
    let mailboxes: Vec<_> = build_mailboxes(account)
        .into_iter()
        .filter(|mailbox| mailbox.id != DRAFTS_MAILBOX)
        .collect();
    if let Some(hit) = mailboxes
        .iter()
        .find(|m| wanted.eq_ignore_ascii_case(&m.id) || wanted.eq_ignore_ascii_case(&m.label))
    {
        return Ok(hit.id.clone());
    }
    let known: Vec<&str> = mailboxes.iter().map(|m| m.id.as_str()).collect();
    Err(invalid_params(format!(
        "'{wanted}' is not a mailbox of {} (known: {})",
        account.name,
        known.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> AccountConfig {
        AccountConfig {
            name: "alpha".to_string(),
            ..Default::default()
        }
    }

    /// A role, a label and an unknown name, which is the error that names the
    /// alternatives.
    #[test]
    fn a_mailbox_resolves_by_role_or_by_label_and_never_to_drafts() {
        assert_eq!(resolve_mailbox(&account(), "inbox").unwrap(), "inbox");
        assert_eq!(resolve_mailbox(&account(), "Inbox").unwrap(), "inbox");
        let refused = resolve_mailbox(&account(), "drafts").expect_err("drafts is not listable");
        assert_eq!(refused.code, -32602);
        assert!(refused.message.contains("inbox"), "{}", refused.message);
    }

    /// `null`, absent, `0` and a number are four different answers.
    #[test]
    fn the_limit_parameter_tells_null_from_zero() {
        assert_eq!(limit_param(&json!({})).unwrap(), None);
        assert_eq!(limit_param(&json!({"limit": null})).unwrap(), None);
        assert_eq!(limit_param(&json!({"limit": 0})).unwrap(), Some(0));
        assert_eq!(limit_param(&json!({"limit": 5})).unwrap(), Some(5));
        assert!(limit_param(&json!({"limit": "5"})).is_err());
        assert!(limit_param(&json!({"limit": -1})).is_err());
    }
}
