//! `mailbox.list`: one account's sidebar hierarchy, with the counts the TUI
//! sidebar shows (P3a-U8).
//!
//! The hierarchy is [`crate::catalogue::sidebar`]'s, the same one the
//! bootstrap snapshot's `mailboxes` map carries, so the daemon cannot disagree
//! with itself about which mailboxes an account has, in which order, under
//! which role, slug, label, server name and policy (#0141). The Drafts mailbox is listed here and excluded from
//! `message.list`, exactly as the sidebar lists it and the message listing
//! refuses it.
//!
//! The three counts, and where each comes from:
//!
//! - `total` is the grouped store query's, one open and one
//!   [`read::mailbox_read_counts`] for the whole account, with the Drafts row
//!   counted from the draft index instead: drafts are not `messages` rows, so a
//!   grouped query cannot see them. A `status-only` folder reports the counts
//!   the last tick's `STATUS` read from the server, since its store holds at
//!   most a tail of it.
//! - `unread` is the rows of that mailbox without `\Seen`, from the same
//!   grouped query. Drafts have no read state and report `0`.
//! - `badge` is what the sidebar prints beside the label, which is `total`
//!   today. It is a separate member because the snapshot's mailbox view already
//!   carries all three and the two shapes may not diverge.
//!
//! A [`MethodKind::Query`], refusing an unknown account with `-32005` and a
//! storeless one with `-32006`, which is what `message.list` does with the same
//! [`ready_account`](super::account::ready_account) gate.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::RpcError;

use crate::catalogue::{sidebar, Catalogue};
use crate::config::{AccountConfig, FolderPolicy};
use crate::draft::draft_count;
use crate::store::read;
use crate::store::Store;

use super::super::dispatch::{
    CancelToken, ClientCtx, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::{internal, open_secrets, server_error, string_param};

/// The family, in method-name order: the sidebar hierarchy this daemon derives
/// from the configuration and the store, and the list the *server* has.
///
/// Both are queries and both are durable, which is what a method that never
/// thought about cancellation means.
pub const MAILBOX_METHOD_SPECS: [MethodSpec; 2] = [
    MethodSpec::new("mailbox.list", MethodKind::Query, 1),
    MethodSpec::new("mailbox.list_server", MethodKind::Query, 1),
];

/// `mailbox.list` as the dispatcher serves it.
pub struct MailboxList {
    /// The live configuration, so a reload is visible to the next listing.
    pub config: Arc<crate::daemon::config::ConfigStore>,
}

impl Method for MailboxList {
    fn spec(&self) -> MethodSpec {
        MAILBOX_METHOD_SPECS[0]
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            list(&params, &self.config.accounts())
                .map(Outcome::query)
                .map_err(DomainError::from)
        })
    }
}

/// The `result` of `mailbox.list`.
pub fn list(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?;

    // The hierarchy the bootstrap seeds come from too (#0141): the four fixed
    // rows, then every listed folder of the account's catalogue. The counts
    // are looked up by slug, and a mailbox the store has no rows for keeps its
    // slot at zero.
    let catalogue = Catalogue::load(account);
    let rows = sidebar(account, &catalogue);
    let counts = counts_by_mailbox(&name)?;

    let mailboxes: Vec<Value> = rows
        .iter()
        .map(|row| {
            // Drafts are local files and not `messages` rows, so the grouped
            // query cannot see them: their total is the draft index's, the same
            // one the TUI sidebar prints, and they have no read state. A
            // `status-only` folder shows the server's counts.
            let (total, unread) = if row.slug == crate::selector::DRAFTS_MAILBOX {
                (draft_count(&name) as u64, 0)
            } else {
                row.counts(
                    counts
                        .get(&row.slug)
                        .map(|grouped| (grouped.total as u64, grouped.unread as u64)),
                )
            };
            json!({
                "role": row.role,
                "slug": row.slug,
                "label": row.label,
                "server": row.server,
                "policy": row.policy,
                "total": total,
                "unread": unread,
                "badge": total,
            })
        })
        .collect();
    Ok(json!({"account": name, "mailboxes": mailboxes}))
}

/// `mailbox.list_server`: what the account's server says it holds (P4-U10).
///
/// `mp list-mailboxes`' method, and the answer a `mp config init` run against a
/// live server needs. It reads no store and takes no engine lock: what it needs
/// is credentials, so an account with no local store is served and an account
/// with no credentials is refused in the secret store's own sentence.
pub struct MailboxListServer {
    /// The live configuration, so a reload is visible to the next listing.
    pub config: Arc<crate::daemon::config::ConfigStore>,
}

impl Method for MailboxListServer {
    fn spec(&self) -> MethodSpec {
        MAILBOX_METHOD_SPECS[1]
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let snapshot = self.config.snapshot();
            let name = string_param(&params, "account")?;
            let account = super::account::configured_account(&snapshot.accounts, &name)?;
            open_secrets(&name, snapshot.config.secrets_backend)?;
            list_server(account)
                .await
                .map(Outcome::query)
                .map_err(DomainError::from)
        })
    }
}

/// The `result` of `mailbox.list_server`.
///
/// `source` says which server answered, because the two transports report
/// different things about a mailbox: Graph's folder list carries the item counts
/// `mp list-mailboxes` prints, and IMAP's `LIST` carries the attributes and the
/// hierarchy delimiter but no counts at all. Every member is present on both
/// paths, `null` where the server said nothing, so a client renders from the
/// shape rather than from a transport it has to know about.
async fn list_server(account: &AccountConfig) -> Result<Value, RpcError> {
    let name = account.name.clone();
    let graph = account.auth_method == crate::config::AuthMethod::Graph;
    let listed = crate::catalogue::discover::server_listing(account)
        .await
        .map_err(|e| server_error(&name, &e))?;
    // The role and the policy the daemon would keep each folder by (#0141),
    // resolved from this very listing against the live config, so the answer
    // describes the server as it is now rather than as the last tick saw it.
    let catalogue = Catalogue::resolve(account, &listed);
    let mailboxes: Vec<Value> = listed
        .iter()
        .map(|folder| {
            let resolved = catalogue.folders.iter().find(|f| f.server == folder.name);
            json!({
                "name": folder.name,
                "label": resolved.map(|f| f.label.clone()),
                "delimiter": folder.delimiter,
                "attributes": folder.attributes,
                // Graph's folder list carries both counts; `LIST` reports
                // neither, and a `STATUS` per row would turn one round trip
                // into one per mailbox.
                "total": folder.total,
                "unread": folder.unread,
                "role": resolved.and_then(|f| f.role).map(|role| role.as_str()),
                "policy": resolved.map(|f| f.policy),
                "key": resolved
                    .filter(|f| f.policy != FolderPolicy::Ignore)
                    .map(|f| f.key.clone()),
                "reason": resolved.and_then(|f| f.reason),
            })
        })
        .collect();
    let source = if graph { "graph" } else { "imap" };
    Ok(json!({"account": name, "source": source, "mailboxes": mailboxes}))
}

/// Both counts of every mailbox of one account, from one open and one grouped
/// query.
///
/// The listing itself is never materialised: counting the unread rows by
/// listing the whole account is the same answer at the cost of every envelope
/// in it, and `SUM` over the flag column is the same predicate
/// [`read::MessageRow::is_read`] applies row by row.
fn counts_by_mailbox(account: &str) -> Result<HashMap<String, read::MailboxReadCounts>, RpcError> {
    let path = crate::config::store_path(account);
    let store = Store::open(&path)
        .map_err(|e| internal(format!("opening the store of {account}: {e:#}")))?;
    read::mailbox_read_counts(&store, account)
        .map_err(|e| internal(format!("counting the mailboxes of {account}: {e:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{listing, ListedFolder};
    use crate::config::{MailboxMapping, MailboxesConfig};

    /// `mailbox.list` serves the catalogue's sidebar (#0141): the fixed rows,
    /// then every folder the server listed, each with its server name and
    /// policy; a synced folder's counts are the store's and a counts-only
    /// folder's the server's, and an ignored one is not a row at all.
    #[test]
    fn mailbox_list_serves_every_listed_folder_with_its_policy() {
        let data = tempfile::tempdir().unwrap();
        let _data = crate::config::test_env::DataDirOverride::set(data.path());
        let account = AccountConfig {
            name: "tum".to_string(),
            mailboxes: MailboxesConfig {
                inbox: Some(MailboxMapping {
                    server: "INBOX".to_string(),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        crate::config::create_private_dir_all(crate::config::account_dir("tum")).unwrap();
        let store = Store::open_account("tum").unwrap();
        let blobs = crate::store::BlobStore::for_account("tum");
        listing::replace(
            &store,
            "tum",
            &[
                ListedFolder::named("INBOX"),
                ListedFolder::named("Spesen"),
                ListedFolder::named("Junk Email").with_attributes(&["\\Junk"]),
                ListedFolder::named("Calendar"),
                ListedFolder::named("Contacts"),
                ListedFolder::named("Tasks"),
            ],
        )
        .unwrap();
        listing::record_counts(&store, "tum", &[("Junk Email".to_string(), 120, 7)]).unwrap();
        let email = crate::parse::FetchedEmail {
            from: "a@x".into(),
            to: "me@x".into(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: "receipt".into(),
            date: "Thu, 22 Oct 2026 10:00:00 +0000".into(),
            body_text: "body".into(),
            html_body: None,
            has_attachments: false,
            server_has_attachments: None,
            message_id: Some("<r@x>".into()),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        };
        crate::ingest::ingest_message(
            &store,
            &blobs,
            &crate::ingest::IngestInput {
                account: "tum",
                mailbox: "Spesen",
                uid: 3,
                email: &email,
                raw: None,
            },
        )
        .unwrap();
        drop(store);

        let answer = list(&json!({"account": "tum"}), std::slice::from_ref(&account)).unwrap();
        let rows: Vec<(String, String, Option<String>, String, u64, u64)> = answer["mailboxes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["slug"].as_str().unwrap().to_string(),
                    row["role"].as_str().unwrap().to_string(),
                    row["server"].as_str().map(str::to_string),
                    row["policy"].as_str().unwrap().to_string(),
                    row["total"].as_u64().unwrap(),
                    row["unread"].as_u64().unwrap(),
                )
            })
            .collect();
        let row = |slug: &str, role: &str, server: Option<&str>, policy: &str, t: u64, u: u64| {
            (
                slug.to_string(),
                role.to_string(),
                server.map(str::to_string),
                policy.to_string(),
                t,
                u,
            )
        };
        assert_eq!(
            rows,
            vec![
                row("inbox", "inbox", Some("INBOX"), "sync", 0, 0),
                row("drafts", "drafts", None, "sync", 0, 0),
                row("sent", "sent", None, "sync", 0, 0),
                row("archive", "archive", None, "sync", 0, 0),
                row(
                    "Junk Email",
                    "junk",
                    Some("Junk Email"),
                    "status-only",
                    120,
                    7
                ),
                row("Spesen", "other", Some("Spesen"), "sync", 1, 1),
            ]
        );
    }
}
