//! `message.materialise_eml`: a stored message as the RFC 822 file a user
//! attaches to another mail or files away (PERSO-174).
//!
//! The fourth materialiser, and a member of the handle family in every way:
//! the same result shape `{handle, path, name, bytes, expires_at}`, the same
//! handle directory, lifetime and `message.release_handle`. It is a family of
//! its own in the code for one reason: a Graph row is stored without a raw
//! message, so the bytes may have to come from the server, which makes this
//! the one materialiser with an await in it.
//!
//! Where the bytes come from, in order:
//!
//! 1. the row's own `raw` blob, which every IMAP ingest writes and which the
//!    retention sweep never evicts; the handle pins it (`ANO-6`);
//! 2. otherwise the server: Graph's `/messages/{id}/$value` export for a Graph
//!    account, a `BODY.PEEK[]` by `Message-ID` for an IMAP one. Nothing is
//!    written back to the store: the export is a copy for the user, and
//!    giving a Graph row a raw blob would change what every reader of that
//!    row parses.
//!
//! A server-search hit with no row is addressed as `message.fetch` addresses
//! it, `hit: {message_id, mailbox}`, and goes straight to the second source:
//! the same `$value` or `BODY.PEEK[]` by `Message-ID`, with the name read off
//! the bytes' own `Subject:` and `Date:`. Nothing is ingested, so the hit
//! stays a hit and no unread count moves.

use std::sync::Arc;

use chrono::Utc;
use futures::future::BoxFuture;
use serde_json::Value;

use mp_protocol::RpcError;

use crate::config::{AccountConfig, AuthMethod};
use crate::store::read::{self, MessageRow};
use crate::store::{BlobStore, Store};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::super::handles::{reap, HandleKind, HandleTable};
use super::{internal, invalid_params, string_param};

/// The method, in an array of its own as the Markdown rendition is
/// (`MESSAGE_MARKDOWN_METHOD_SPECS`): `ClientIntegration` and `Durable`,
/// because the daemon prepares the file and only the client can copy it where
/// the user wants it, and a client holding the file may not lose it because
/// the socket that asked went away.
pub const MESSAGE_EML_METHOD_SPECS: [MethodSpec; 1] = [MethodSpec::new(
    "message.materialise_eml",
    MethodKind::ClientIntegration,
    1,
)];

/// `message.materialise_eml` as the dispatcher serves it.
pub struct MessageEml {
    /// The live configuration, so a reload is visible to the next call.
    pub config: Arc<ConfigStore>,
    /// The daemon's one handle table, shared with the other materialisers.
    pub handles: Arc<HandleTable>,
}

impl Method for MessageEml {
    fn spec(&self) -> MethodSpec {
        MESSAGE_EML_METHOD_SPECS[0]
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            reap(&self.handles, Utc::now());
            let snapshot = self.config.snapshot();
            materialise_eml(
                &params,
                &snapshot.accounts,
                snapshot.config.secrets_backend,
                &self.handles,
            )
            .await
            .map(Outcome::query)
            .map_err(DomainError::from)
        })
    }
}

/// Register the method on `dispatcher`.
pub fn register(dispatcher: &mut Dispatcher, config: Arc<ConfigStore>, handles: Arc<HandleTable>) {
    dispatcher.register(Arc::new(MessageEml { config, handles }));
}

/// The `result` of `message.materialise_eml`, addressed as `message.get`
/// addresses a message, `{account, row_id|id|selector, mailbox?}`, or as
/// `message.fetch` addresses a server-only hit, `{account, hit: {message_id,
/// mailbox}}`.
async fn materialise_eml(
    params: &Value,
    accounts: &[AccountConfig],
    secrets: crate::secrets::SecretsBackendKind,
    handles: &HandleTable,
) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?.clone();
    let (filename, bytes, pinned) = match params.get("hit").filter(|v| !v.is_null()) {
        Some(hit) => {
            let addressed = |key: &str| !matches!(params.get(key), None | Some(Value::Null));
            if addressed("row_id") || addressed("id") || addressed("selector") {
                return Err(invalid_params(
                    "hit is an address of its own; send it without row_id, id or selector",
                ));
            }
            let hit = Hit::from_param(&account, hit)?;
            raw_of_hit(&account, &hit, secrets).await?
        }
        None => {
            let (row, bytes, pinned) = raw_message(&account, params, secrets).await?;
            (name_of_row(&row), bytes, pinned)
        }
    };
    super::message::write_handle(
        handles,
        HandleKind::Eml,
        &name,
        &filename,
        &bytes,
        &pinned,
        0o644,
    )
}

/// The addressed row, its RFC 822 bytes, and the blobs those were read from.
async fn raw_message(
    account: &AccountConfig,
    params: &Value,
    secrets: crate::secrets::SecretsBackendKind,
) -> Result<(MessageRow, Vec<u8>, Vec<String>), RpcError> {
    let name = account.name.as_str();
    let row = {
        let store = open_store(name)?;
        super::message::address(params, &store, name)?
    };
    let (bytes, pinned) = raw_of_row(account, &row, secrets).await?;
    Ok((row, bytes, pinned))
}

/// The RFC 822 bytes of a resolved row, and the blobs they were read from:
/// the stored `raw` blob, or the server's copy when the row has none.
///
/// The store is opened, read and closed before any await: a server leg is
/// seconds of network, and nothing about the row can change what it asks for.
/// Shared with `draft.forward`'s `as_attachment`, which attaches these bytes.
pub(super) async fn raw_of_row(
    account: &AccountConfig,
    row: &MessageRow,
    secrets: crate::secrets::SecretsBackendKind,
) -> Result<(Vec<u8>, Vec<String>), RpcError> {
    let name = account.name.as_str();
    let stored = {
        let store = open_store(name)?;
        let blobs = BlobStore::new(crate::config::blobs_dir(name));
        read::blob_hash_of_kind(&store, row.id, "raw")
            .and_then(|hash| read::read_blob(&blobs, row.id, &hash).map(|bytes| (bytes, hash)))
    };
    if let Some((bytes, hash)) = stored {
        return Ok((bytes, vec![hash]));
    }
    super::open_secrets(name, secrets)?;
    let bytes = fetch_raw(account, &row.message_id, &row.mailbox)
        .await
        .map_err(|e| super::server_error(name, &e))?;
    Ok((bytes, Vec::new()))
}

/// The `.eml` name of a stored row.
fn name_of_row(row: &MessageRow) -> String {
    crate::parse::eml_name(
        row.subject.as_deref().unwrap_or_default(),
        row.date_display.as_deref(),
    )
}

/// The `.eml` name of bytes the store holds no row for, off their own
/// `Subject:` and `Date:`, which is where a row's subject and date come from.
pub(super) fn name_of_raw(bytes: &[u8]) -> String {
    let headers = mailparse::parse_headers(bytes)
        .map(|(headers, _)| headers)
        .unwrap_or_default();
    let header = |key: &str| {
        headers
            .iter()
            .find(|h| h.get_key().eq_ignore_ascii_case(key))
            .map(|h| h.get_value())
    };
    crate::parse::eml_name(
        header("Subject").as_deref().unwrap_or_default(),
        header("Date").as_deref(),
    )
}

/// A server-search hit with no row, addressed as `message.fetch` addresses
/// it: the mailbox it came from and the `Message-ID` the server reported.
pub(super) struct Hit {
    /// The `messages.mailbox` key the hit's mailbox resolves to.
    pub(super) mailbox: String,
    pub(super) message_id: String,
}

impl Hit {
    /// The `{message_id, mailbox}` object a client sends, `mailbox` resolved
    /// as `message.fetch` resolves it (a sidebar label or a mailbox key).
    pub(super) fn from_param(account: &AccountConfig, value: &Value) -> Result<Self, RpcError> {
        if !value.is_object() {
            return Err(invalid_params(
                "hit is {message_id, mailbox}, the address of a server-search hit",
            ));
        }
        super::only_params("hit", value, &["message_id", "mailbox"])?;
        let message_id = string_param(value, "message_id")?;
        if message_id.trim().is_empty() {
            return Err(invalid_params(
                "hit.message_id is the Message-ID header of the hit",
            ));
        }
        let wanted = string_param(value, "mailbox")?;
        let mailbox = super::message::resolve_mailbox(account, &wanted)?;
        Ok(Self {
            mailbox,
            message_id,
        })
    }
}

/// A hit's `.eml` name, its RFC 822 bytes, and the blobs those were read from.
///
/// A hit the store has caught up with since the search (a sync, an `f`) is
/// that row's export, pinned like any other; one it has not is the server's
/// copy, fetched as a row with no raw blob is.
pub(super) async fn raw_of_hit(
    account: &AccountConfig,
    hit: &Hit,
    secrets: crate::secrets::SecretsBackendKind,
) -> Result<(String, Vec<u8>, Vec<String>), RpcError> {
    let name = account.name.as_str();
    let held = {
        let store = open_store(name)?;
        read::find_by_message_id(&store, name, &hit.message_id)
            .map_err(|e| internal(format!("looking up {} in {name}: {e:#}", hit.message_id)))?
            .into_iter()
            .next()
    };
    if let Some(row) = held {
        let (bytes, pinned) = raw_of_row(account, &row, secrets).await?;
        return Ok((name_of_row(&row), bytes, pinned));
    }
    super::open_secrets(name, secrets)?;
    let bytes = fetch_raw(account, &hit.message_id, &hit.mailbox)
        .await
        .map_err(|e| super::server_error(name, &e))?;
    Ok((name_of_raw(&bytes), bytes, Vec::new()))
}

fn open_store(name: &str) -> Result<Store, RpcError> {
    Store::open(crate::config::store_path(name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))
}

/// The server's copy of the message `message_id` in the mailbox keyed
/// `mailbox`: a row with no stored raw message, or a hit with no row.
async fn fetch_raw(
    account: &AccountConfig,
    message_id: &str,
    mailbox: &str,
) -> anyhow::Result<Vec<u8>> {
    if account.auth_method == AuthMethod::Graph {
        let config = crate::config::GraphConfig::load(account)?;
        let client = crate::graph::GraphClient::new_async(&config).await?;
        let graph_id = client
            .find_message_by_internet_id(message_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the server no longer has {message_id}"))?;
        return client.fetch_mime(&graph_id).await;
    }
    let server_name = crate::config::find_server_name_for_role(account, mailbox);
    let imap = crate::config::ImapConfig::load(account)?;
    let mut session = crate::imap_client::open_imap_session(&imap).await?;
    let found =
        crate::imap_client::fetch_raw_by_message_id(&mut session, &server_name, message_id).await;
    session.logout().await.ok();
    let (_uid, raw, _email) = found?
        .ok_or_else(|| anyhow::anyhow!("the server no longer has {message_id} in {server_name}"))?;
    Ok(raw)
}
