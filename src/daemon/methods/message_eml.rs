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
    let filename = eml_name(
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

/// The file name of an export: `<YYYY-MM-DD>_<subject>.eml`.
///
/// The date is the `Date:` header's own calendar day, as a reader of the
/// message sees it; a header that does not parse drops the prefix rather than
/// inventing a day. The subject keeps its spaces and its case, since the file
/// is usually attached to another mail and its name is what the recipient
/// reads; the characters a file system or a mail client refuses in a name are
/// dropped, and a separator becomes `-`. An empty subject is `message`, and a
/// long one is cut as an attachment's name is (`parse::cut_name`), by bytes
/// on a char boundary.
pub fn eml_name(subject: &str, date: Option<&str>) -> String {
    let cleaned: String = subject
        .chars()
        .filter_map(|c| match c {
            '/' | '\\' => Some('-'),
            ':' | '*' | '?' | '"' | '<' | '>' | '|' => None,
            c if c.is_control() => Some(' '),
            c => Some(c),
        })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let stem = if cleaned.is_empty() {
        "message"
    } else {
        crate::parse::cut_name(cleaned).trim_end()
    };
    let day = date
        .and_then(|d| chrono::DateTime::parse_from_rfc2822(d.trim()).ok())
        .map(|d| d.format("%Y-%m-%d").to_string());
    match day {
        Some(day) => format!("{day}_{stem}.eml"),
        None => format!("{stem}.eml"),
    }
}

#[cfg(test)]
mod tests {
    use super::eml_name;
    use crate::parse::MAX_NAME_BYTES;

    #[test]
    fn the_name_is_the_day_and_the_subject() {
        assert_eq!(
            eml_name(
                "Re: Elsevier DEAL open access",
                Some("Wed, 21 Oct 2026 09:12:00 +0200")
            ),
            "2026-10-21_Re Elsevier DEAL open access.eml"
        );
    }

    #[test]
    fn a_name_drops_what_a_file_system_refuses_and_keeps_the_rest() {
        assert_eq!(
            eml_name("a/b\\c: \"d\" <e>? *f* |g|\t\nh", None),
            "a-b-c d e f g h.eml"
        );
        assert_eq!(
            eml_name("Bericht über Anträge", None),
            "Bericht über Anträge.eml"
        );
    }

    #[test]
    fn an_empty_subject_or_an_odd_date_still_names_a_file() {
        assert_eq!(eml_name("", None), "message.eml");
        assert_eq!(eml_name(" ... ", Some("(unknown date)")), "message.eml");
        assert_eq!(eml_name("..", None), "message.eml");
        let long = "x".repeat(300);
        assert_eq!(eml_name(&long, None).len(), MAX_NAME_BYTES + ".eml".len());
    }

    /// A CJK or emoji subject is three or four bytes a character: the cut is
    /// by bytes, on a char boundary, so the name fits a file system's 255 and
    /// never splits a character.
    #[test]
    fn a_long_cjk_or_emoji_subject_is_cut_by_bytes_on_a_char_boundary() {
        let date = Some("Wed, 21 Oct 2026 09:12:00 +0200");
        for subject in ["会議の議事録".repeat(40), "📬🎉".repeat(80)] {
            let name = eml_name(&subject, date);
            let stem = name
                .strip_prefix("2026-10-21_")
                .and_then(|n| n.strip_suffix(".eml"))
                .expect("the day and the extension frame the stem");
            assert!(stem.len() <= MAX_NAME_BYTES, "{} bytes", stem.len());
            assert!(stem.len() > MAX_NAME_BYTES - 4, "{} bytes", stem.len());
            assert!(subject.starts_with(stem));
            assert!(name.len() <= 255);
        }
    }
}
