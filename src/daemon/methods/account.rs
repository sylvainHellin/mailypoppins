//! `account.list`: the accounts `config.toml` declares, as the daemon sees them.
//!
//! Phase 2 starts no account runtime, so this reports the *configuration* plus
//! what is on disk, not a runtime table: `daemon.status`'s `accounts` stays
//! empty without `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES`, and a client asking
//! which accounts exist still deserves an answer.
//!
//! The four facts, and where each comes from:
//!
//! - `name`, `default`: `[[accounts]]` in file order, the first one being the
//!   account every `-A`-less command already means.
//! - `backend`: `auth_method` alone. `graph` is Graph, everything else reaches
//!   its server over IMAP.
//! - `state`: the store on disk. Present and openable is [`STATE_READY`];
//!   anything else is [`STATE_BLOCKED`], because an account with no store
//!   cannot serve a read and will not until `mp sync` writes one. It is never
//!   `opening`: nothing here is asynchronous, so no account is ever between
//!   states.

use serde_json::{json, Value};

use mp_protocol::{ErrorCode, RpcError};

use crate::config::{AccountConfig, AuthMethod};

use super::super::server::DaemonState;

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

/// The `result` of `account.list`.
pub fn list(state: &DaemonState) -> Value {
    json!({
        "accounts": entries(&state.configured)
            .iter()
            .map(to_json)
            .collect::<Vec<_>>(),
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
///
/// The existence check comes first on purpose: `Store::open` would *create* the
/// file, and probing an account must not give it a store it never had.
pub fn state_of(account: &str) -> &'static str {
    let path = crate::config::store_path(account);
    if !path.exists() {
        return STATE_BLOCKED;
    }
    match crate::store::Store::open(&path) {
        Ok(_) => STATE_READY,
        Err(_) => STATE_BLOCKED,
    }
}

/// The configured account behind `name`, once it can serve a read.
///
/// Unknown is `-32005` naming what was asked for; known but storeless is
/// `-32006` carrying the same state [`list`] reports for it, because two
/// answers about one account may not contradict each other.
pub fn ready_account<'a>(
    state: &'a DaemonState,
    name: &str,
) -> Result<&'a AccountConfig, RpcError> {
    let account = state
        .configured
        .iter()
        .find(|account| account.name == name)
        .ok_or_else(|| RpcError {
            code: ErrorCode::AccountUnknown.code(),
            message: format!("no account named {name} is configured"),
            data: Some(json!({"account": name})),
        })?;
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
