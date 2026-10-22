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
use super::{internal, string_param};

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
/// addresses a message: `{account, row_id|id|selector, mailbox?}`.
async fn materialise_eml(
    params: &Value,
    accounts: &[AccountConfig],
    secrets: crate::secrets::SecretsBackendKind,
    handles: &HandleTable,
) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?.clone();
    let (row, bytes, pinned) = raw_message(&account, params, secrets).await?;
    let filename = crate::parse::eml_name(
        row.subject.as_deref().unwrap_or_default(),
        row.date_display.as_deref(),
    );
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
    let bytes = fetch_raw(account, row)
        .await
        .map_err(|e| super::server_error(name, &e))?;
    Ok((bytes, Vec::new()))
}

fn open_store(name: &str) -> Result<Store, RpcError> {
    Store::open(crate::config::store_path(name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))
}

/// The server's copy of a row with no stored raw message.
async fn fetch_raw(account: &AccountConfig, row: &MessageRow) -> anyhow::Result<Vec<u8>> {
    if account.auth_method == AuthMethod::Graph {
        let config = crate::config::GraphConfig::load(account)?;
        let client = crate::graph::GraphClient::new_async(&config).await?;
        let graph_id = client
            .find_message_by_internet_id(&row.message_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the server no longer has {}", row.message_id))?;
        return client.fetch_mime(&graph_id).await;
    }
    let server_name = crate::config::find_server_name_for_role(account, &row.mailbox);
    let imap = crate::config::ImapConfig::load(account)?;
    let mut session = crate::imap_client::open_imap_session(&imap).await?;
    let found =
        crate::imap_client::fetch_raw_by_message_id(&mut session, &server_name, &row.message_id)
            .await;
    session.logout().await.ok();
    let (_uid, raw, _email) = found?.ok_or_else(|| {
        anyhow::anyhow!(
            "the server no longer has {} in {server_name}",
            row.message_id
        )
    })?;
    Ok(raw)
}
