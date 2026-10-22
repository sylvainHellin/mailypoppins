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
//!    row parses. A copy downloaded in the last two minutes is served again
//!    ([`ServerCopies`]), so the reads of one hit share one download.
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
use super::super::server_copies::{Fetcher, Request, ServerCopies};
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
    /// The server copies downloaded lately, shared with every read of a hit.
    pub copies: Arc<ServerCopies>,
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
            let server = ServerLeg {
                secrets: snapshot.config.secrets_backend,
                copies: &self.copies,
            };
            materialise_eml(&params, &snapshot.accounts, server, &self.handles)
                .await
                .map(Outcome::query)
                .map_err(DomainError::from)
        })
    }
}

/// Register the method on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    handles: Arc<HandleTable>,
    copies: Arc<ServerCopies>,
) {
    dispatcher.register(Arc::new(MessageEml {
        config,
        handles,
        copies,
    }));
}

/// What a read that may go to the server needs beside the account: the
/// backend its credentials are in, and the copies downloaded lately, which a
/// run of reads on one hit shares ([`ServerCopies`]).
#[derive(Clone, Copy)]
pub(super) struct ServerLeg<'a> {
    pub(super) secrets: crate::secrets::SecretsBackendKind,
    pub(super) copies: &'a ServerCopies,
}

impl ServerLeg<'_> {
    /// The server's copy of `message_id` in the mailbox keyed `mailbox`,
    /// downloaded once for the reads of the next couple of minutes.
    async fn download(
        &self,
        account: &AccountConfig,
        message_id: &str,
        mailbox: &str,
    ) -> Result<Vec<u8>, RpcError> {
        self.copies
            .get(Request {
                account: account.clone(),
                secrets: self.secrets,
                message_id: message_id.to_string(),
                mailbox: mailbox.to_string(),
            })
            .await
    }
}

/// The daemon's [`Fetcher`]: the credentials opened, then [`fetch_raw`].
pub fn server_fetcher() -> Fetcher {
    Arc::new(|request: Request| {
        Box::pin(async move {
            let name = request.account.name.as_str();
            super::open_secrets(name, request.secrets)?;
            fetch_raw(&request.account, &request.message_id, &request.mailbox)
                .await
                .map_err(|e| super::server_error(name, &e))
        })
    })
}

/// The `result` of `message.materialise_eml`, addressed as `message.get`
/// addresses a message, `{account, row_id|id|selector, mailbox?}`, or as
/// `message.fetch` addresses a server-only hit, `{account, hit: {message_id,
/// mailbox}}`.
async fn materialise_eml(
    params: &Value,
    accounts: &[AccountConfig],
    server: ServerLeg<'_>,
    handles: &HandleTable,
) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?.clone();
    let (filename, bytes, pinned) = match hit_address(&account, params)? {
        Some(hit) => raw_of_hit(&account, &hit, server).await?,
        None => {
            let (row, bytes, pinned) = raw_message(&account, params, server).await?;
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
    server: ServerLeg<'_>,
) -> Result<(MessageRow, Vec<u8>, Vec<String>), RpcError> {
    let name = account.name.as_str();
    let row = {
        let store = open_store(name)?;
        super::message::address(params, &store, name)?
    };
    let (bytes, pinned) = raw_of_row(account, &row, server).await?;
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
    server: ServerLeg<'_>,
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
    let bytes = server
        .download(account, &row.message_id, &row.mailbox)
        .await?;
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
    /// as `message.fetch` resolves it: a sidebar mailbox, or since #0141 any
    /// folder the server lists, whatever its policy.
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
        let catalogue = crate::catalogue::Catalogue::load(account);
        let (mailbox, _server) =
            super::message_server::fetch_storage(account, &catalogue, &wanted)?;
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
    server: ServerLeg<'_>,
) -> Result<(String, Vec<u8>, Vec<String>), RpcError> {
    let name = account.name.as_str();
    if let Some(row) = held_row(name, hit)? {
        let (bytes, pinned) = raw_of_row(account, &row, server).await?;
        return Ok((name_of_row(&row), bytes, pinned));
    }
    let bytes = server
        .download(account, &hit.message_id, &hit.mailbox)
        .await?;
    Ok((name_of_raw(&bytes), bytes, Vec::new()))
}

/// The row the store has caught up with since the search found `hit`, by its
/// `Message-ID` across the account, as `message.fetch` looks one up.
fn held_row(name: &str, hit: &Hit) -> Result<Option<MessageRow>, RpcError> {
    let store = open_store(name)?;
    Ok(read::find_by_message_id(&store, name, &hit.message_id)
        .map_err(|e| internal(format!("looking up {} in {name}: {e:#}", hit.message_id)))?
        .into_iter()
        .next())
}

/// Whether `params` address a server-search hit, `hit: {message_id,
/// mailbox}`, rather than a row: the dispatch test of the reads and the
/// materialisers that take one.
pub(super) fn addresses_hit(params: &Value) -> bool {
    params.get("hit").is_some_and(|hit| !hit.is_null())
}

/// The hit `params` address, or `None` when they address a row; a hit beside
/// a row address is the caller disagreeing with themselves, `-32602`.
fn hit_address(account: &AccountConfig, params: &Value) -> Result<Option<Hit>, RpcError> {
    let Some(hit) = params.get("hit").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let addressed = |key: &str| !matches!(params.get(key), None | Some(Value::Null));
    if addressed("row_id") || addressed("id") || addressed("selector") {
        return Err(invalid_params(
            "hit is an address of its own; send it without row_id, id or selector",
        ));
    }
    Hit::from_param(account, hit).map(Some)
}

/// What a hit is read from: the row the store has caught up with since the
/// search, or the server's copy, parsed as a sync parses it, with the mailbox
/// key the hit was found in.
pub(super) enum HitMessage {
    Held(Box<MessageRow>),
    Server {
        mailbox: String,
        email: Box<crate::parse::FetchedEmail>,
    },
}

/// [`HitMessage`] of `hit`: the held row when there is one, which opens no
/// session, else the server's copy, downloaded as `message.materialise_eml`
/// downloads it and ingesting nothing, so the hit stays a hit.
pub(super) async fn hit_message(
    account: &AccountConfig,
    hit: &Hit,
    server: ServerLeg<'_>,
) -> Result<HitMessage, RpcError> {
    let name = account.name.as_str();
    if let Some(row) = held_row(name, hit)? {
        return Ok(HitMessage::Held(Box::new(row)));
    }
    let bytes = server
        .download(account, &hit.message_id, &hit.mailbox)
        .await?;
    let email = crate::parse::parse_rfc822_to_fetched_email(&bytes).ok_or_else(|| {
        internal(format!(
            "the server's copy of {} in {name} does not parse as a message",
            hit.message_id
        ))
    })?;
    Ok(HitMessage::Server {
        mailbox: hit.mailbox.clone(),
        email: Box::new(email),
    })
}

/// `params` with the hit replaced by the row the store holds for it, so a
/// held hit is answered by the row path verbatim.
fn as_row_params(params: &Value, row: &MessageRow) -> Value {
    let mut params = params.clone();
    if let Some(object) = params.as_object_mut() {
        object.remove("hit");
        object.insert("row_id".to_string(), Value::from(row.id));
    }
    params
}

/// `message.get` of a server-search hit, `{account, hit: {message_id,
/// mailbox}, body?}`: the held row's record, or the record of the server's
/// copy ([`crate::read_cmd::shown_fetched`]), whose attachment list is the
/// `part` order `message.materialise_attachment`'s `hit` address takes.
pub(super) async fn get_hit(
    params: &Value,
    accounts: &[AccountConfig],
    server: ServerLeg<'_>,
) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?.clone();
    let wants_body = match params.get("body") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(body)) => *body,
        Some(_) => return Err(invalid_params("body is a boolean")),
    };
    let hit = hit_address(&account, params)?.ok_or_else(|| invalid_params("hit is required"))?;
    let (mailbox, email) = match hit_message(&account, &hit, server).await? {
        HitMessage::Held(row) => {
            return super::message::get(&as_row_params(params, &row), accounts)
        }
        HitMessage::Server { mailbox, email } => (mailbox, email),
    };
    let shown = crate::read_cmd::shown_fetched(&name, &mailbox, &email);
    let mut result = serde_json::to_value(&shown)
        .map_err(|e| internal(format!("serialising {}: {e}", hit.message_id)))?;
    if !wants_body {
        if let Some(object) = result.as_object_mut() {
            object.remove("body");
        }
    }
    Ok(result)
}

/// `message.materialise_attachment` and `message.materialise_markdown` of a
/// server-search hit: the held row's file, or one cut from the server's copy,
/// an attachment by its `part` in [`get_hit`]'s list and the read-only view
/// by [`read::render_fetched_markdown`]. Nothing is pinned, since no blob of
/// the store was read.
pub(super) async fn materialise_hit(
    params: &Value,
    accounts: &[AccountConfig],
    server: ServerLeg<'_>,
    handles: &HandleTable,
    kind: HandleKind,
) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?.clone();
    let hit = hit_address(&account, params)?.ok_or_else(|| invalid_params("hit is required"))?;
    // The part is checked before any download, as the row path checks it
    // before reading a blob.
    let part = match kind {
        HandleKind::Attachment => Some(
            params
                .get("part")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid_params("part is a required non-negative integer"))?,
        ),
        _ => None,
    };
    let (mailbox, email) = match hit_message(&account, &hit, server).await? {
        HitMessage::Held(row) => {
            return super::message::materialise(
                &as_row_params(params, &row),
                accounts,
                handles,
                kind,
            )
        }
        HitMessage::Server { mailbox, email } => (mailbox, email),
    };
    let (filename, bytes, mode) = server_file(&email, &mailbox, part)?;
    super::message::write_handle(handles, kind, &name, &filename, &bytes, &[], mode)
}

/// The file [`materialise_hit`] writes for the server's copy of a hit: part
/// `part` of its attachments, or with no part its read-only view, with the
/// mode the handle gets.
fn server_file(
    email: &crate::parse::FetchedEmail,
    mailbox: &str,
    part: Option<u64>,
) -> Result<(String, Vec<u8>, u32), RpcError> {
    let Some(part) = part else {
        return Ok((
            super::message::markdown_name(&email.subject, "message.md"),
            read::render_fetched_markdown(email, mailbox).into_bytes(),
            // #0075's rule, as for a row's view: the buffer opens read-only.
            0o444,
        ));
    };
    let attachment = usize::try_from(part)
        .ok()
        .and_then(|part| email.attachments.get(part))
        .ok_or_else(|| {
            invalid_params(format!(
                "part {part} is not one of this message's {} attachments",
                email.attachments.len()
            ))
        })?;
    // Sanitised as a stored part's name is: this is the seam that turns a name
    // a sender chose into a path.
    Ok((
        super::message::safe_filename(&crate::parse::sanitize_attachment_filename(
            &attachment.filename,
        )),
        attachment.content.clone(),
        0o644,
    ))
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
    // The folder behind the key from the catalogue (#0141), which knows a
    // discovered folder and a role found on the server as well as a
    // configured one.
    let server_name = crate::catalogue::Catalogue::load(account).server_for_key(mailbox);
    let imap = crate::config::ImapConfig::load(account)?;
    let mut session = crate::imap_client::open_imap_session(&imap).await?;
    let found =
        crate::imap_client::fetch_raw_by_message_id(&mut session, &server_name, message_id).await;
    session.logout().await.ok();
    let (_uid, raw, _email) = found?
        .ok_or_else(|| anyhow::anyhow!("the server no longer has {message_id} in {server_name}"))?;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{listing, ListedFolder};

    /// A message with a PDF and an inline logo the HTML body shows by
    /// `cid:`, as a server hands it over.
    const RAW: &[u8] = b"From: Alice <alice@example.com>\r\n\
To: me@example.com\r\n\
Subject: LOC Day // Teilnehmer\r\n\
Date: Wed, 12 Nov 2025 09:00:00 +0100\r\n\
Message-ID: <loc-day@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
\r\n\
--outer\r\n\
Content-Type: multipart/related; boundary=\"inner\"\r\n\
\r\n\
--inner\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>The list</p><img src=\"cid:logo@x\">\r\n\
--inner\r\n\
Content-Type: image/png\r\n\
Content-ID: <logo@x>\r\n\
Content-Disposition: inline; filename=\"logo.png\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--inner--\r\n\
--outer\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"../teilnehmer.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--outer--\r\n";

    /// The server's copy of a hit is listed, cut into parts and rendered as
    /// a stored row of its mailbox would be: `message.get`'s part order is
    /// the order `part` addresses, a part's name is sanitised, and the view
    /// opens read-only (PERSO-155).
    #[test]
    fn a_hits_server_copy_lists_cuts_and_renders_like_a_row() {
        let email = crate::parse::parse_rfc822_to_fetched_email(RAW).expect("the raw parses");
        let shown = crate::read_cmd::shown_fetched("tum", "sent", &email);
        assert_eq!(shown.selector, "mp://tum/sent/loc-day@example.com");
        assert_eq!(shown.mailbox, "sent");
        let listed: Vec<(&str, bool)> = shown
            .attachments
            .iter()
            .map(|att| (att.name.as_str(), att.inline))
            .collect();
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert!(listed.contains(&("logo.png", true)), "{listed:?}");

        for (part, att) in shown.attachments.iter().enumerate() {
            let (name, bytes, mode) =
                server_file(&email, "sent", Some(part as u64)).expect("a listed part");
            assert_eq!(bytes.len() as u64, att.size, "part {part}");
            assert!(!name.contains('/'), "a part's name is sanitised: {name}");
            assert_eq!(mode, 0o644);
        }
        let refused = server_file(&email, "sent", Some(2)).expect_err("there is no part 2");
        assert_eq!(refused.code, -32602);

        let (name, view, mode) = server_file(&email, "sent", None).expect("the view");
        assert_eq!(name, "loc-day-teilnehmer.md");
        assert_eq!(mode, 0o444);
        let view = String::from_utf8(view).expect("UTF-8");
        assert!(view.starts_with("---\n"), "{view}");
        assert!(view.contains("mailbox: sent"), "{view}");
        assert!(view.contains("The list"), "{view}");
    }

    /// Listing a server-only hit, cutting each of its parts, opening its view
    /// and exporting it download the server's copy once: every read after
    /// the first is served from the daemon's recent copies (PERSO-155).
    #[tokio::test]
    async fn a_hits_reads_share_one_download() {
        use std::sync::atomic::Ordering;

        let data = tempfile::tempdir().expect("tempdir");
        // Thread-local, and the test runs on its own thread's runtime, so
        // every store and handle path below resolves into the temp tree.
        let _data = crate::config::test_env::DataDirOverride::set(data.path());
        let account = AccountConfig {
            name: "tum".to_string(),
            ..Default::default()
        };
        // A ready account with no row for the hit, which is a server-only hit.
        let store_file = crate::config::store_path(&account.name);
        std::fs::create_dir_all(store_file.parent().expect("a parent")).expect("account dir");
        drop(Store::open(&store_file).expect("the store"));
        let accounts = vec![account];

        let (fetcher, downloads) = crate::daemon::server_copies::tests::counting(RAW);
        let copies = ServerCopies::new(fetcher);
        let server = ServerLeg {
            secrets: Default::default(),
            copies: &copies,
        };
        let handles = HandleTable::new(std::time::Duration::from_secs(60));
        let hit = serde_json::json!({"message_id": "<loc-day@example.com>", "mailbox": "sent"});
        let params = |extra: Value| {
            let mut params = serde_json::json!({"account": "tum", "hit": hit});
            for (key, value) in extra.as_object().expect("an object") {
                params[key] = value.clone();
            }
            params
        };

        let shown = get_hit(&params(serde_json::json!({})), &accounts, server)
            .await
            .expect("the hit's record");
        let parts = shown["attachments"].as_array().expect("attachments").len();
        assert_eq!(parts, 2);
        for part in 0..parts {
            materialise_hit(
                &params(serde_json::json!({"part": part})),
                &accounts,
                server,
                &handles,
                HandleKind::Attachment,
            )
            .await
            .expect("a part");
        }
        materialise_hit(
            &params(serde_json::json!({})),
            &accounts,
            server,
            &handles,
            HandleKind::Markdown,
        )
        .await
        .expect("the view");
        materialise_eml(&params(serde_json::json!({})), &accounts, server, &handles)
            .await
            .expect("the export");

        assert_eq!(downloads.load(Ordering::SeqCst), 1);
    }

    /// A hit's `.eml` export and its forward as attachment address the hit's
    /// folder as `message.fetch` does (#0141): any folder the server lists,
    /// a counts-only or an ignored one included, where the configured
    /// mailboxes alone used to answer "not a mailbox of" the account.
    #[test]
    fn a_hit_from_a_folder_only_the_server_names_is_addressable() {
        let data = tempfile::tempdir().unwrap();
        let _data = crate::config::test_env::DataDirOverride::set(data.path());
        let account = AccountConfig {
            name: "tum".to_string(),
            ..Default::default()
        };
        crate::config::create_private_dir_all(crate::config::account_dir("tum")).unwrap();
        let store = Store::open_account("tum").unwrap();
        listing::replace(
            &store,
            "tum",
            &[
                ListedFolder::named("INBOX"),
                ListedFolder::named("Projects"),
                ListedFolder::named("Junk Email").with_attributes(&["\\Junk"]),
                ListedFolder::named("All Mail").with_attributes(&["\\All"]),
            ],
        )
        .unwrap();
        drop(store);

        let hit = |mailbox: &str| {
            Hit::from_param(
                &account,
                &serde_json::json!({"message_id": "<m@x>", "mailbox": mailbox}),
            )
        };
        assert_eq!(hit("Projects").unwrap().mailbox, "Projects");
        assert_eq!(hit("Junk").unwrap().mailbox, "Junk Email");
        assert_eq!(
            hit("All Mail").unwrap().mailbox,
            "All Mail",
            "an ignored folder"
        );
        assert_eq!(hit("Nowhere").err().map(|e| e.code), Some(-32602));
    }
}
