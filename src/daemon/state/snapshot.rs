//! What the canonical state holds, what a change does to it, and the JSON one
//! bootstrap hands over.
//!
//! A [`Snapshot`] is a whole-state capture rather than a patch, because a
//! client that has just connected has nothing to patch. Everything a Phase 3a
//! account can report is zero: no runtime has come up, so no count has been
//! read, exactly as `App::new` presents an account it has not opened yet.
//!
//! [`Change`] is the daemon's own vocabulary for "something moved", one variant
//! per thing a Phase 3a client tracks. It is deliberately not the wire-level
//! event enum, whose coalescing and bounds belong to P3a-U5; a change plus the
//! revision it committed at is the whole input the ordering rules need.

use std::collections::BTreeMap;

use mp_protocol::events::{SyncCompleted, KIND_SYNC_COMPLETED};
use serde_json::{json, Value};

use crate::config::AccountConfig;
use crate::sync_health::SyncHealth;

/// One mailbox of a seeded account, as the sidebar hierarchy names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxSeed {
    /// The product role: `inbox`, `drafts`, `sent`, `archive`, or `other`.
    pub role: String,
    /// The store key and selector segment, which is what a client addresses.
    pub slug: String,
    /// What the sidebar shows.
    pub label: String,
}

/// One account the daemon knows about, with the mailboxes it will have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountSeed {
    /// The configured account name.
    pub name: String,
    /// Its mailboxes, in the order the sidebar lists them.
    pub mailboxes: Vec<MailboxSeed>,
}

/// The seeds behind `config.toml`, in the file's order.
///
/// Derived from [`build_mailboxes`](crate::tui::app::build_mailboxes), the same
/// sidebar hierarchy the TUI and `message.list` resolve against, so the daemon
/// cannot disagree with them about which mailboxes an account has. It opens no
/// store: Phase 3a starts no runtimes, and asking which accounts exist may not
/// create a cache.
pub fn seeds_from_config(accounts: &[AccountConfig]) -> Vec<AccountSeed> {
    use crate::tui::app::MailboxKind;
    accounts
        .iter()
        .map(|account| AccountSeed {
            name: account.name.clone(),
            mailboxes: crate::tui::app::build_mailboxes(account)
                .into_iter()
                .map(|mailbox| MailboxSeed {
                    role: match mailbox.kind {
                        MailboxKind::Inbox => "inbox",
                        MailboxKind::Drafts => "drafts",
                        MailboxKind::Sent => "sent",
                        MailboxKind::Archive => "archive",
                        MailboxKind::Extra => "other",
                    }
                    .to_string(),
                    slug: mailbox.id,
                    label: mailbox.label,
                })
                .collect(),
        })
        .collect()
}

/// One thing that moved, as the daemon commits it.
///
/// Every variant names the `account` it is about, which is how the state routes
/// it, and carries the whole of what moved rather than a delta: the three
/// mailbox counts travel together because they are read together, and a draft
/// travels whole because replacing it is cheaper than patching it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// An account's runtime came up.
    AccountReady { account: String },
    /// An account's runtime cannot serve, with one line for the user.
    AccountBlocked { account: String, reason: String },
    /// A mailbox's total, unread and badge counts.
    MailboxCounts {
        account: String,
        mailbox: String,
        total: u64,
        unread: u64,
        badge: u64,
    },
    /// A draft appeared or changed; this replaces the client's copy.
    DraftUpsert {
        account: String,
        id: String,
        subject: String,
        status: String,
        valid: bool,
    },
    /// A draft was discarded or sent.
    DraftRemoved { account: String, id: String },
    /// An account's outbox counts.
    OutboxCounts {
        account: String,
        queued: u64,
        failed: u64,
    },
    /// One sync tick finished, with everything it did.
    ///
    /// The odd one out, and deliberately: it is the outcome of a command
    /// rather than a fact about a resource, so it reduces into no snapshot,
    /// travels as a non-coalescing [`Event::Lifecycle`](super::events::Event)
    /// and carries a payload the protocol crate owns.
    SyncCompleted(SyncCompleted),
}

impl Change {
    /// The account this change is about, which is how the state routes it.
    pub fn account(&self) -> &str {
        match self {
            Change::AccountReady { account }
            | Change::AccountBlocked { account, .. }
            | Change::MailboxCounts { account, .. }
            | Change::DraftUpsert { account, .. }
            | Change::DraftRemoved { account, .. }
            | Change::OutboxCounts { account, .. } => account,
            Change::SyncCompleted(outcome) => &outcome.account,
        }
    }

    /// The `kind` of the `state.event` this change travels as.
    pub fn kind(&self) -> &'static str {
        match self {
            Change::AccountReady { .. } | Change::AccountBlocked { .. } => "account.state_changed",
            Change::MailboxCounts { .. } => "mailbox.counts_changed",
            Change::DraftUpsert { .. } => "draft.changed",
            Change::DraftRemoved { .. } => "draft.removed",
            Change::OutboxCounts { .. } => "outbox.counts_changed",
            Change::SyncCompleted(_) => KIND_SYNC_COMPLETED,
        }
    }

    /// The event's payload, always an object so a kind can gain fields.
    pub fn payload(&self) -> Value {
        match self {
            Change::AccountReady { account } => json!({"account": account, "state": "ready"}),
            Change::AccountBlocked { account, reason } => {
                json!({"account": account, "state": "blocked", "reason": reason})
            }
            Change::MailboxCounts {
                account,
                mailbox,
                total,
                unread,
                badge,
            } => json!({
                "account": account, "mailbox": mailbox,
                "total": total, "unread": unread, "badge": badge,
            }),
            Change::DraftUpsert {
                account,
                id,
                subject,
                status,
                valid,
            } => json!({
                "account": account, "id": id,
                "subject": subject, "status": status, "valid": valid,
            }),
            Change::DraftRemoved { account, id } => json!({"account": account, "id": id}),
            Change::OutboxCounts {
                account,
                queued,
                failed,
            } => json!({"account": account, "queued": queued, "failed": failed}),
            // Infallible in practice: every field is a string, a `u64`, a list
            // of strings or an `Option<String>`, none of which can fail to
            // serialise. An empty object rather than a panic if that ever
            // changes, because a daemon must not die inside a fan-out.
            Change::SyncCompleted(outcome) => {
                serde_json::to_value(outcome).unwrap_or_else(|_| json!({}))
            }
        }
    }
}

/// Whether an account's runtime has come up.
///
/// Deliberately not `account.list`'s `state`, which probes the store on disk:
/// that answers "can I read this account", this answers "has this account's
/// runtime come up", and Phase 3a has no runtimes, so everything is `Opening`
/// until a change says otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AccountState {
    /// No runtime has reported yet.
    #[default]
    Opening,
    /// The runtime is serving.
    Ready,
    /// The runtime cannot serve.
    Blocked,
}

impl AccountState {
    /// The string this state travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            AccountState::Opening => "opening",
            AccountState::Ready => "ready",
            AccountState::Blocked => "blocked",
        }
    }
}

/// One account, one mailbox, one draft and one outbox as the snapshot carries
/// them. Their fields are the state's own to mutate, which is why they are
/// visible to the module above and to nothing else.
#[derive(Clone, Debug)]
pub struct AccountView {
    pub(super) name: String,
    pub(super) state: AccountState,
    pub(super) health: SyncHealth,
}

/// One mailbox row of a snapshot: its seed and the three counts.
#[derive(Clone, Debug)]
pub struct MailboxView {
    pub(super) seed: MailboxSeed,
    pub(super) total: u64,
    pub(super) unread: u64,
    pub(super) badge: u64,
}

/// One draft row of a snapshot.
#[derive(Clone, Debug)]
pub struct DraftView {
    pub(super) id: String,
    pub(super) subject: String,
    pub(super) status: String,
    pub(super) valid: bool,
}

/// One account's outbox counts.
#[derive(Clone, Copy, Debug, Default)]
pub struct OutboxView {
    pub(super) queued: u64,
    pub(super) failed: u64,
}

/// A whole-state capture, taken at one revision.
///
/// `mailboxes`, `drafts` and `outbox` carry one key per listed account,
/// always, so a client indexes them by account name without a null check.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub(super) accounts: Vec<AccountView>,
    pub(super) mailboxes: BTreeMap<String, Vec<MailboxView>>,
    pub(super) drafts: BTreeMap<String, Vec<DraftView>>,
    pub(super) outbox: BTreeMap<String, OutboxView>,
    /// The daemon's non-terminal operations, as
    /// [`OperationStatus::to_json`](crate::daemon::operations::OperationStatus::to_json)
    /// renders them. Filled by
    /// [`CanonicalState::bootstrap`](super::CanonicalState::bootstrap) from the
    /// registry, which is not part of the state a client mirrors.
    pub(super) operations: Vec<Value>,
}

impl Snapshot {
    /// The `snapshot` member of a `state.bootstrap` result.
    ///
    /// `holds` and `diagnostics` are empty arrays rather than absent keys:
    /// nothing in this build produces one, and a client that iterates them must
    /// not have to check first. `operations` lists whatever the registry has
    /// not settled, so a client that bootstraps mid-operation learns about it.
    pub fn to_json(&self) -> Value {
        // `sync_health` is an object rather than a bare string so the reason
        // and the timestamp can join it without a version bump.
        let accounts: Vec<Value> = self
            .accounts
            .iter()
            .map(|account| {
                json!({
                    "name": account.name,
                    "state": account.state.as_str(),
                    "sync_health": {"state": match account.health {
                        SyncHealth::Unknown => "unknown",
                        SyncHealth::Ok { .. } => "ok",
                        SyncHealth::Failed { .. } => "failed",
                    }},
                })
            })
            .collect();
        let mailboxes = per_account(&self.mailboxes, |list| {
            list.iter()
                .map(|mailbox| {
                    json!({
                        "role": mailbox.seed.role,
                        "slug": mailbox.seed.slug,
                        "label": mailbox.seed.label,
                        "total": mailbox.total,
                        "unread": mailbox.unread,
                        "badge": mailbox.badge,
                    })
                })
                .collect()
        });
        let drafts = per_account(&self.drafts, |list| {
            list.iter()
                .map(|draft| {
                    json!({
                        "id": draft.id,
                        "subject": draft.subject,
                        "status": draft.status,
                        "valid": draft.valid,
                    })
                })
                .collect()
        });
        let outbox = per_account(
            &self.outbox,
            |outbox| json!({"queued": outbox.queued, "failed": outbox.failed}),
        );
        json!({
            "accounts": accounts,
            "mailboxes": mailboxes,
            "drafts": drafts,
            "outbox": outbox,
            "holds": [],
            "operations": self.operations,
            "diagnostics": [],
        })
    }
}

/// One JSON object keyed by account name, rendering each account's value with
/// `render`.
fn per_account<T>(map: &BTreeMap<String, T>, render: impl Fn(&T) -> Value) -> Value {
    Value::Object(
        map.iter()
            .map(|(name, value)| (name.clone(), render(value)))
            .collect(),
    )
}
