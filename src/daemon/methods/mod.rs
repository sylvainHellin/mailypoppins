//! The read-only method family: `account.list` and `message.list` (P2-U11).
//!
//! These are the first domain methods the daemon serves, and they are reads
//! only. Both go through the same store path the CLI takes
//! ([`crate::store::read`]) and neither takes an [`crate::engine_lock::EngineLock`]:
//! the daemon does not become an account's engine before Phase 5, so a running
//! TUI or `mp sync` keeps the lock while the daemon answers listings beside it.
//!
//! The dispatcher is a function rather than a table because the gate in
//! [`super::server::dispatch_request`] runs first: by the time a method reaches
//! [`dispatch`], the connection has handshaken, and a method this build does
//! not serve falls back to `-32601`.

pub mod account;
pub mod message;

use serde_json::Value;

use mp_protocol::RpcError;

use super::server::DaemonState;

/// JSON-RPC's own "invalid params".
const INVALID_PARAMS: i32 = -32602;
/// JSON-RPC's own "internal error".
const INTERNAL_ERROR: i32 = -32603;

/// Answer one domain method, or `None` when this build does not serve it.
pub fn dispatch(
    method: &str,
    params: &Value,
    state: &DaemonState,
) -> Option<Result<Value, RpcError>> {
    match method {
        "account.list" => Some(Ok(account::list(state))),
        "message.list" => Some(message::list(params, state)),
        _ => None,
    }
}

/// A required string parameter, or `-32602` naming it.
fn string_param(params: &Value, name: &str) -> Result<String, RpcError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid_params(format!("{name} is a required string parameter")))
}

/// `-32602`, with a message the caller can act on.
fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError {
        code: INVALID_PARAMS,
        message: message.into(),
        data: None,
    }
}

/// `-32603`: the daemon failed at something that is nobody's fault but its own.
fn internal(message: impl Into<String>) -> RpcError {
    RpcError {
        code: INTERNAL_ERROR,
        message: message.into(),
        data: None,
    }
}
