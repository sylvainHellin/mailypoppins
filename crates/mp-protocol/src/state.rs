//! The `state.bootstrap` result, typed (P5-U2).
//!
//! A client turns one JSON answer into one value -
//! `serde_json::from_value::<Bootstrap>(result)` - and then reads fields
//! instead of indexing a map with string literals. The shape is the one
//! `docs/daemon-protocol.md` documents and
//! `crates/mp-protocol/fixtures/state.bootstrap.response.json` pins; the daemon
//! still *renders* it by hand in `src/daemon/state/snapshot.rs`, because that
//! module owns the state these are a projection of, and this module is what
//! every client decodes it with.
//!
//! Two rules run through the whole file:
//!
//! - **Every collection is `#[serde(default)]`.** The daemon always sends
//!   `mailboxes`, `drafts`, `outbox`, `holds`, `operations` and `diagnostics`,
//!   one key per listed account, so a client indexes them without a null check.
//!   Defaulting is for the other direction: a decoder that refuses a snapshot
//!   over an empty section it would have ignored anyway turns an additive
//!   protocol change into a client that will not start.
//! - **`holds`, `operations` and `diagnostics` stay [`Value`].** Nothing in
//!   this build fills the first and the third, and an `operations` entry is an
//!   `operation.status` result whose owner is the daemon's operation registry;
//!   typing them here before a client reads them would pin a shape from the
//!   wrong end.
//!
//! An account `state` and a `sync_health.state` are enums rather than strings:
//! they are closed sets the protocol version fixes, a client branches on them,
//! and a typo in a match arm should be a compile error rather than a frame that
//! silently renders as "not ready".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Whether an account's runtime has come up.
///
/// The runtime's state, not the store's: `account.list` answers "can I read
/// this account's store on disk", this answers "has this account's runtime
/// come up", and the two are deliberately different questions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    /// No runtime has reported yet. Counts are 0 and the draft list is empty,
    /// exactly as the TUI presents an account it has not opened.
    #[default]
    Opening,
    /// The runtime is serving.
    Ready,
    /// The runtime cannot serve; `reason` travels with the event, not here.
    Blocked,
}

impl AccountState {
    /// Whether this account is serving, which is the one question a client
    /// that only paints a loading marker has to ask.
    pub fn is_ready(self) -> bool {
        matches!(self, AccountState::Ready)
    }
}

/// How the last sync of an account went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncHealthState {
    /// Nothing has synced yet, which is what a fresh bootstrap reports.
    #[default]
    Unknown,
    /// The last tick succeeded.
    Ok,
    /// The last tick failed.
    Failed,
}

/// An account's sync health.
///
/// An object around one enum rather than a bare string, because the reason and
/// the timestamp join it without a version bump.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncHealth {
    /// `unknown`, `ok` or `failed`.
    #[serde(default)]
    pub state: SyncHealthState,
}

/// One account of the snapshot, in `config.toml`'s order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    /// The configured account name, which keys every other section.
    pub name: String,
    /// Whether its runtime has come up.
    #[serde(default)]
    pub state: AccountState,
    /// How its last sync went.
    #[serde(default)]
    pub sync_health: SyncHealth,
}

/// One mailbox row: the sidebar hierarchy plus the three counts.
///
/// `role` is the product role (`inbox`, `drafts`, `sent`, `archive`, `other`)
/// and is what a renderer branches on; `slug` is the store key and the
/// selector segment, which is what a client addresses; `label` is what the
/// sidebar shows. Three fields because they are three different jobs, and a
/// client that derived one from another would be right until an account maps
/// its Inbox to a server folder called something else.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxRow {
    /// `inbox`, `drafts`, `sent`, `archive` or `other`.
    pub role: String,
    /// The store key and selector segment.
    pub slug: String,
    /// What the sidebar shows.
    pub label: String,
    /// Messages in the mailbox.
    #[serde(default)]
    pub total: u64,
    /// Unread messages.
    #[serde(default)]
    pub unread: u64,
    /// What a badge shows, which is not always the unread count.
    #[serde(default)]
    pub badge: u64,
}

/// One draft row: the same fields the `draft.changed` event carries, so a
/// client's reducer is "replace the row with the payload".
///
/// Every field but `id` defaults: the P2 fixture predates `path`, `to` and
/// `ready`, and a snapshot section a client does not read yet must not be able
/// to stop it from starting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRow {
    /// The `id:` frontmatter field, or the file stem when the file has none.
    pub id: String,
    /// The absolute path, because a GUI opens a draft by path.
    #[serde(default)]
    pub path: String,
    /// The `to:` field, `null` for a draft with no recipient yet.
    #[serde(default)]
    pub to: Option<String>,
    /// The `subject:` field, empty when the file has none.
    #[serde(default)]
    pub subject: String,
    /// `draft`, `approved`, `sent`, or `invalid` for a file that would not
    /// parse (#0080).
    #[serde(default)]
    pub status: String,
    /// Whether the file parsed.
    #[serde(default)]
    pub valid: bool,
    /// Whether it would send, which is the other axis: a draft with no subject
    /// parses perfectly and is not sendable.
    #[serde(default)]
    pub ready: bool,
}

/// One account's outbox counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxCounts {
    /// Messages waiting to go out.
    #[serde(default)]
    pub queued: u64,
    /// Messages that failed and were left for the user.
    #[serde(default)]
    pub failed: u64,
}

/// The whole state a client mirrors, captured at one revision.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The accounts, in `config.toml`'s order, which is the order a sidebar
    /// lists them in.
    #[serde(default)]
    pub accounts: Vec<AccountSnapshot>,
    /// One entry per listed account, in the order the sidebar lists them.
    #[serde(default)]
    pub mailboxes: BTreeMap<String, Vec<MailboxRow>>,
    /// One entry per listed account.
    #[serde(default)]
    pub drafts: BTreeMap<String, Vec<DraftRow>>,
    /// One entry per listed account.
    #[serde(default)]
    pub outbox: BTreeMap<String, OutboxCounts>,
    /// Undo-send holds; nothing in this build fills it.
    #[serde(default)]
    pub holds: Vec<Value>,
    /// Every long-running operation the daemon has not settled, in start
    /// order, each entry an `operation.status` result.
    #[serde(default)]
    pub operations: Vec<Value>,
    /// Diagnostics; nothing in this build fills it.
    #[serde(default)]
    pub diagnostics: Vec<Value>,
}

impl Snapshot {
    /// This account's mailbox rows, or an empty slice for an account the
    /// snapshot does not list.
    ///
    /// A slice rather than an `Option`, because every caller renders a row per
    /// entry and none of them has anything different to do with "no such
    /// account" than with "no mailboxes".
    pub fn mailboxes_of(&self, account: &str) -> &[MailboxRow] {
        self.mailboxes.get(account).map_or(&[], Vec::as_slice)
    }

    /// This account's draft rows, likewise.
    pub fn drafts_of(&self, account: &str) -> &[DraftRow] {
        self.drafts.get(account).map_or(&[], Vec::as_slice)
    }

    /// This account's outbox counts, zeroed for an account it does not list.
    pub fn outbox_of(&self, account: &str) -> OutboxCounts {
        self.outbox.get(account).copied().unwrap_or_default()
    }
}

/// The whole `state.bootstrap` result.
///
/// `revision` is the capture point a client watermarks from and is never `0`,
/// which is what makes `0` usable as the client's own pre-bootstrap sentinel.
/// `capabilities` is what *this connection* agreed on at its handshake rather
/// than the daemon's whole list: a client acts on what it may use.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bootstrap {
    /// The daemon instance that answered. A client that sees an unfamiliar one
    /// has been reconnected to a new daemon and must bootstrap again.
    pub instance_id: String,
    /// The revision the snapshot was captured at, never `0`.
    #[serde(default)]
    pub revision: u64,
    /// What this connection agreed on at its handshake.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// The state itself.
    #[serde(default)]
    pub snapshot: Snapshot,
}

impl Bootstrap {
    /// Whether this connection agreed on `capability`, which is the question a
    /// client asks before issuing a method it may not be allowed to.
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|name| name == capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The committed fixture decodes whole, and every documented field lands
    /// where the daemon put it.
    ///
    /// The fixture is the response frame, so the decode starts at `result`,
    /// which is exactly what a client does with `Connection::call`'s return.
    #[test]
    fn the_committed_fixture_decodes_whole() {
        let raw = include_str!("../fixtures/state.bootstrap.response.json");
        let response: Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let bootstrap: Bootstrap =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");

        assert_eq!(bootstrap.instance_id, "01J8Z6Q9X4V3N2M1K0H7G5F4D3");
        assert_eq!(bootstrap.revision, 4217);
        assert!(bootstrap.has_capability("state.bootstrap"));
        assert!(!bootstrap.has_capability("draft.send"));

        let accounts = &bootstrap.snapshot.accounts;
        assert_eq!(accounts.len(), 2, "the order is config.toml's");
        assert_eq!(accounts[0].name, "work");
        assert_eq!(accounts[0].state, AccountState::Ready);
        assert_eq!(accounts[0].sync_health.state, SyncHealthState::Ok);
        assert_eq!(accounts[1].state, AccountState::Opening);
        assert_eq!(accounts[1].sync_health.state, SyncHealthState::Unknown);

        let work = bootstrap.snapshot.mailboxes_of("work");
        assert_eq!(work.len(), 4);
        assert_eq!(work[0].role, "inbox");
        assert_eq!(work[0].slug, "inbox");
        assert_eq!(work[0].label, "Inbox");
        assert_eq!(
            (work[0].total, work[0].unread, work[0].badge),
            (1841, 12, 12)
        );
        assert_eq!(bootstrap.snapshot.outbox_of("work").queued, 1);
        assert_eq!(
            bootstrap.snapshot.outbox_of("nobody"),
            OutboxCounts::default()
        );
        assert!(bootstrap.snapshot.mailboxes_of("nobody").is_empty());
    }

    /// The fixture's draft row predates `path`, `to` and `ready`, and decodes
    /// anyway: a section a client does not read yet may not stop it starting.
    #[test]
    fn a_draft_row_missing_the_later_fields_still_decodes() {
        let raw = include_str!("../fixtures/state.bootstrap.response.json");
        let response: Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let bootstrap: Bootstrap =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");

        let drafts = bootstrap.snapshot.drafts_of("work");
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].id, "2026-07-02-angebot");
        assert_eq!(drafts[0].subject, "Angebot");
        assert!(drafts[0].valid);
        assert!(
            !drafts[0].ready,
            "an absent `ready` is not a sendable draft"
        );
        assert_eq!(drafts[0].path, "");
        assert_eq!(drafts[0].to, None);
        assert!(bootstrap.snapshot.drafts_of("perso").is_empty());
    }

    /// Round trip: a value that went out re-decodes equal, which is what lets
    /// a test build one by hand and a client trust the wire.
    #[test]
    fn a_bootstrap_round_trips() {
        let bootstrap = Bootstrap {
            instance_id: "abcd".to_string(),
            revision: 7,
            capabilities: vec!["state.bootstrap".to_string()],
            snapshot: Snapshot {
                accounts: vec![AccountSnapshot {
                    name: "work".to_string(),
                    state: AccountState::Blocked,
                    sync_health: SyncHealth {
                        state: SyncHealthState::Failed,
                    },
                }],
                mailboxes: BTreeMap::from([(
                    "work".to_string(),
                    vec![MailboxRow {
                        role: "other".to_string(),
                        slug: "Projekte".to_string(),
                        label: "Projekte".to_string(),
                        total: 3,
                        unread: 1,
                        badge: 1,
                    }],
                )]),
                drafts: BTreeMap::from([(
                    "work".to_string(),
                    vec![DraftRow {
                        id: "d-one".to_string(),
                        path: "/data/accounts/work/drafts/d-one.md".to_string(),
                        to: Some("robin@example.com".to_string()),
                        subject: "Angebot".to_string(),
                        status: "draft".to_string(),
                        valid: true,
                        ready: true,
                    }],
                )]),
                outbox: BTreeMap::from([(
                    "work".to_string(),
                    OutboxCounts {
                        queued: 2,
                        failed: 1,
                    },
                )]),
                holds: Vec::new(),
                operations: vec![json!({"operation_id": "op-1"})],
                diagnostics: Vec::new(),
            },
        };

        let encoded = serde_json::to_value(&bootstrap).expect("it serialises");
        assert_eq!(
            encoded["snapshot"]["accounts"][0]["state"],
            json!("blocked")
        );
        assert_eq!(
            encoded["snapshot"]["accounts"][0]["sync_health"]["state"],
            json!("failed")
        );
        let decoded: Bootstrap = serde_json::from_value(encoded).expect("it decodes");
        assert_eq!(decoded, bootstrap);
    }

    /// The empty daemon: no configured account, three empty objects, and a
    /// decode that needs no null check anywhere.
    #[test]
    fn an_empty_daemon_decodes_to_empty_sections() {
        let bootstrap: Bootstrap = serde_json::from_value(json!({
            "instance_id": "abcd",
            "revision": 1,
            "capabilities": [],
            "snapshot": {
                "accounts": [], "mailboxes": {}, "drafts": {}, "outbox": {},
                "holds": [], "operations": [], "diagnostics": [],
            },
        }))
        .expect("it decodes");
        assert!(bootstrap.snapshot.accounts.is_empty());
        assert!(bootstrap.snapshot.mailboxes_of("work").is_empty());
    }

    /// An account state outside the closed set is a decode error rather than a
    /// silent `opening`: the set is fixed by the protocol version, so a fourth
    /// word means the client is talking to a daemon it does not understand.
    #[test]
    fn an_unknown_account_state_is_refused() {
        let decoded: Result<AccountSnapshot, _> =
            serde_json::from_value(json!({"name": "work", "state": "asleep"}));
        assert!(decoded.is_err(), "the state set is closed");
    }
}
