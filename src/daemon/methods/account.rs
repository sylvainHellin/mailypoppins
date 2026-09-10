//! `account.list`: the accounts `config.toml` declares, as the daemon sees them.
//!
//! This reports the *configuration* plus what is on disk, not the runtime
//! table: `daemon.status`'s `accounts` is what the runtimes say, and a client
//! asking which accounts exist deserves an answer whether or not their
//! runtimes have come up.
//!
//! The four facts, and where each comes from:
//!
//! - `name`, `default`: `[[accounts]]` in file order, the first one being the
//!   account every `-A`-less command already means.
//! - `backend`: `auth_method` alone. `graph` is Graph, everything else reaches
//!   its server over IMAP.
//! - `state`: the store on disk. Present and readable at the current schema
//!   version is [`STATE_READY`]; anything else is [`STATE_BLOCKED`], because an
//!   account with no store cannot serve a read and will not until `mp sync`
//!   writes one. It is never `opening`: nothing here is asynchronous, so no
//!   account is ever between states.
//!
//! The probe behind `state` is read-only on purpose, see [`state_of_path`]:
//! reporting what an account has may not change what it has.

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context};
use futures::future::BoxFuture;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

use mp_protocol::{ErrorCode, RpcError};

use crate::config::{AccountConfig, AuthMethod};
use crate::store::schema;

use super::super::dispatch::{
    CancelToken, ClientCtx, DomainError, Method, MethodKind, MethodSpec, Outcome,
};

/// The store is there and opens: reads are served.
pub const STATE_READY: &str = "ready";
/// The store is missing or does not open: reads are refused with `-32006`.
pub const STATE_BLOCKED: &str = "blocked";

/// One account as `account.list` reports it, and as `mp account list` prints
/// it. One type for both directions, so the routed CLI renders the daemon's
/// answer through the same code that renders the local one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountEntry {
    /// The configured account name.
    pub name: String,
    /// True for the first configured account and no other.
    pub default: bool,
    /// `imap` or `graph`.
    pub backend: String,
    /// One of `opening`, `ready`, `blocked`.
    pub state: String,
}

/// `account.list` as the dispatcher serves it.
pub struct AccountList {
    /// The live configuration, so a reload is visible to the next listing.
    pub config: Arc<super::super::config::ConfigStore>,
}

impl Method for AccountList {
    fn spec(&self) -> MethodSpec {
        MethodSpec::new("account.list", MethodKind::Query, 1)
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        _params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move { Ok(Outcome::query(list(&self.config.accounts()))) })
    }
}

/// The `result` of `account.list`.
pub fn list(accounts: &[AccountConfig]) -> Value {
    json!({
        "accounts": entries(accounts).iter().map(to_json).collect::<Vec<_>>(),
    })
}

/// Every configured account, in the file's order.
pub fn entries(accounts: &[AccountConfig]) -> Vec<AccountEntry> {
    accounts
        .iter()
        .enumerate()
        .map(|(index, account)| AccountEntry {
            name: account.name.clone(),
            default: index == 0,
            backend: backend_of(account).to_string(),
            state: state_of(&account.name).to_string(),
        })
        .collect()
}

/// The transport `auth_method` implies.
pub fn backend_of(account: &AccountConfig) -> &'static str {
    match account.auth_method {
        AuthMethod::Graph => "graph",
        _ => "imap",
    }
}

/// Whether an account can serve reads, decided on disk.
pub fn state_of(account: &str) -> &'static str {
    state_of_path(&crate::config::store_path(account))
}

/// The state of one store file, probed without writing a byte to it.
///
/// `Store::open` is the wrong tool here twice over: it *creates* the file when
/// it is missing, and it *deletes and rebuilds* it when it is corrupt or
/// stamped with another schema version. Both are right for a command the user
/// asked to work on that account and wrong for a question about it: answering
/// `account.list` may not give an account a store it never had, and may not
/// throw away a cache whose owner is a running TUI or `mp sync`.
///
/// So the probe opens read-only, which creates nothing, and runs the same two
/// structural checks [`crate::store::Store::open`] validates with, minus the
/// `integrity_check` a listing has no business paying for. A store that passes
/// here is one `message.list` may then open normally.
pub fn state_of_path(path: &Path) -> &'static str {
    if !path.exists() {
        return STATE_BLOCKED;
    }
    match probe(path) {
        Ok(()) => STATE_READY,
        Err(_) => STATE_BLOCKED,
    }
}

/// Read-only structural probe of a store file: it opens as a database, it is
/// stamped with the current schema version, and every required table is there.
fn probe(path: &Path) -> anyhow::Result<()> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} read-only", path.display()))?;
    match schema::stamped_version(&conn)? {
        Some(schema::SCHEMA_VERSION) => {}
        Some(other) => bail!(
            "schema version {other}, expected {}",
            schema::SCHEMA_VERSION
        ),
        None => bail!("no schema version stamp"),
    }
    if !schema::all_tables_present(&conn)? {
        bail!("schema v{} is incomplete", schema::SCHEMA_VERSION);
    }
    Ok(())
}

/// The configured account behind `name`, once it can serve a read.
///
/// Unknown is `-32005` naming what was asked for; known but storeless is
/// `-32006` carrying the same state [`list`] reports for it, because two
/// answers about one account may not contradict each other.
pub fn ready_account<'a>(
    accounts: &'a [AccountConfig],
    name: &str,
) -> Result<&'a AccountConfig, RpcError> {
    let account = configured_account(accounts, name)?;
    let state = state_of(name);
    if state != STATE_READY {
        return Err(RpcError {
            code: ErrorCode::AccountNotReady.code(),
            message: format!("{name} has no local store to read yet; run `mp sync -A {name}`"),
            data: Some(json!({"account": name, "state": state})),
        });
    }
    Ok(account)
}

/// The configured account behind `name`, whatever is on disk for it.
///
/// The gate for a method whose whole job is to reach the *server*: a sync is
/// what gives an account its store, so requiring one first would refuse every
/// first sync, and a server-side listing needs credentials rather than rows.
/// Unknown is `-32005` naming what was asked for, exactly as in
/// [`ready_account`], which is this check plus the store probe.
pub fn configured_account<'a>(
    accounts: &'a [AccountConfig],
    name: &str,
) -> Result<&'a AccountConfig, RpcError> {
    accounts
        .iter()
        .find(|account| account.name == name)
        .ok_or_else(|| RpcError {
            code: ErrorCode::AccountUnknown.code(),
            message: format!("no account named {name} is configured"),
            data: Some(json!({"account": name})),
        })
}

/// One entry on the wire.
pub fn to_json(entry: &AccountEntry) -> Value {
    json!({
        "name": entry.name,
        "default": entry.default,
        "backend": entry.backend,
        "state": entry.state,
    })
}

/// One entry off the wire, for the client side of the route. A field of the
/// wrong type is dropped rather than guessed at.
pub fn from_json(value: &Value) -> Option<AccountEntry> {
    Some(AccountEntry {
        name: value.get("name")?.as_str()?.to_string(),
        default: value.get("default")?.as_bool()?,
        backend: value.get("backend")?.as_str()?.to_string(),
        state: value.get("state")?.as_str()?.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(name: &str, auth: AuthMethod) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            auth_method: auth,
            ..Default::default()
        }
    }

    /// Order, `default` and `backend` come from the configuration alone.
    #[test]
    fn the_first_configured_account_is_the_default_one() {
        let accounts = [
            account("alpha", AuthMethod::Password),
            account("gamma", AuthMethod::Graph),
        ];
        let entries = entries(&accounts);
        assert_eq!(entries[0].name, "alpha");
        assert!(entries[0].default);
        assert_eq!(entries[0].backend, "imap");
        assert!(!entries[1].default);
        assert_eq!(entries[1].backend, "graph", "auth_method alone decides");
    }

    /// A file at the store path that is not a store is `blocked`, and the probe
    /// leaves it exactly as it found it: `Store::open` would have deleted it and
    /// written a fresh schema in its place, which is a cache `account.list` has
    /// no mandate to destroy.
    #[test]
    fn a_garbage_store_file_is_blocked_and_survives_the_probe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.sqlite3");
        let garbage = b"this is not a SQLite database".to_vec();
        std::fs::write(&path, &garbage).expect("write the garbage file");

        assert_eq!(state_of_path(&path), STATE_BLOCKED);
        assert_eq!(
            std::fs::read(&path).expect("the file is still there"),
            garbage,
            "probing an account may not rewrite what is at its store path"
        );
        for suffix in ["-wal", "-shm"] {
            let sidecar = path.with_file_name(format!("store.sqlite3{suffix}"));
            assert!(
                !sidecar.exists(),
                "the probe left {} behind",
                sidecar.display()
            );
        }
    }

    /// A path with nothing at it is `blocked`, and stays a path with nothing at
    /// it: a question about an account may not create its store.
    #[test]
    fn a_missing_store_is_blocked_and_is_not_created() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("never-synced").join("store.sqlite3");
        assert_eq!(state_of_path(&path), STATE_BLOCKED);
        assert!(!path.exists(), "the probe created {}", path.display());
    }

    /// A real store, written by the real opener, is `ready`.
    #[test]
    fn a_store_of_the_current_schema_is_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.sqlite3");
        drop(crate::store::Store::open(&path).expect("create a store"));
        assert_eq!(state_of_path(&path), STATE_READY);
    }

    /// An entry survives the round trip through the wire shape.
    #[test]
    fn an_entry_round_trips_through_its_json() {
        let entry = AccountEntry {
            name: "alpha".to_string(),
            default: true,
            backend: "imap".to_string(),
            state: STATE_READY.to_string(),
        };
        assert_eq!(from_json(&to_json(&entry)), Some(entry));
    }
}
